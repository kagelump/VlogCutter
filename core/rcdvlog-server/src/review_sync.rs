//! Adapter from stored review-session evidence to the shared synchronizer (#223).
//!
//! `rcdvlog_core::tv_review_sync` owns every timing decision. This module only
//! assembles its `SyncRequest` from durable evidence and submits native
//! recording-frame queries; it never estimates movie time itself.
//!
//! - Capture clocks come only from `review-capture-calibration.v1` clock
//!   samples (the browser's bracketed worklet probes). Without one, the
//!   session's synchronization is *pending*: nothing is synthesized from
//!   request-start, packet arrival or server receive time.
//! - A manual cue is a keypress observed on the monotonic (performance) clock.
//!   Its capture position is that observation mapped through the calibration;
//!   its declared elapsed movie second becomes raw movie PTS on the target's
//!   exact origin and timebase.
//! - Capture discontinuities, buffer overflows, device loss and page
//!   interruptions become `captureInterrupted` boundaries.
//! - The shared default policy applies (no single-anchor extrapolation).
use crate::ReviewTargetEvidence;
use rcdvlog_core::tv_review_sync::{
    CaptureClock, CapturePosition, SYNC_VERSION, SyncAnchor, SyncAnchorSource, SyncBoundary,
    SyncBoundaryKind, SyncPolicy, SyncQuery, SyncReport, SyncRequest, SyncSlice,
};
use rusqlite::{Connection, params};
use serde::Serialize;
use std::collections::BTreeMap;

pub const CALIBRATION_SCHEMA: &str = "review-capture-calibration.v1";
/// A manual cue is a human reaction to a picture change, not a measurement.
pub const MANUAL_CUE_CONFIDENCE: f64 = 0.9;
pub const MANUAL_CUE_MOVIE_UNCERTAINTY_SECONDS: f64 = 0.25;
const MAX_QUERIES_PER_CALL: usize = 256;

/// How the session's synchronization was assembled, for display and export.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncDescription {
    /// `pending` (no calibrated clock), `noAnchors`, `mapped` or `failed`.
    pub status: String,
    pub source: String,
    pub note: String,
    pub calibrated_clocks: usize,
    pub anchors: usize,
    pub boundaries: usize,
    pub policy: SyncPolicy,
    pub diagnostics: Vec<String>,
}

/// The shared mapper's result for one utterance's native frame interval.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UtteranceSync {
    /// `anchored`, `estimated`, `conflicting`, `unknown`, `mixed` or `pending`.
    pub state: String,
    pub slices: Vec<SyncSlice>,
    /// Frames of the utterance outside every calibrated capture clock.
    pub uncovered_frames: u64,
}

pub struct SessionSync {
    request: Option<SyncRequest>,
    clocks: Vec<CaptureClock>,
    description: SyncDescription,
}

struct Event {
    event_id: String,
    kind: String,
    audio_frame: u64,
    performance_ms: Option<f64>,
    declared_movie_seconds: Option<f64>,
    detail: Option<serde_json::Value>,
}

struct Calibration {
    clock_id: String,
    epoch: u64,
    first_sample: u64,
    origin_ns: i64,
    tolerance_ns: i64,
    performance_epoch_ms: f64,
}

fn calibration(event: &Event, sample_rate: u32) -> Option<Calibration> {
    let d = event.detail.as_ref()?.as_object()?;
    if d.get("schema")?.as_str()? != CALIBRATION_SCHEMA
        || d.get("sampleRate")?.as_u64()? != u64::from(sample_rate)
    {
        return None;
    }
    let tolerance_ns = d.get("monotonicToleranceNs")?.as_i64()?;
    let calibration = Calibration {
        clock_id: d.get("clockId")?.as_str()?.to_string(),
        epoch: d.get("clockEpoch")?.as_u64()?,
        first_sample: d.get("firstSample")?.as_u64()?,
        origin_ns: d.get("monotonicOriginNs")?.as_i64()?,
        tolerance_ns,
        performance_epoch_ms: d.get("performanceEpochMs")?.as_f64()?,
    };
    (calibration.origin_ns >= 0
        && (0..=1_000_000_000).contains(&tolerance_ns)
        && calibration.performance_epoch_ms.is_finite())
    .then_some(calibration)
}

fn load_events(connection: &Connection, session_id: &str) -> rusqlite::Result<Vec<Event>> {
    let mut statement = connection.prepare(
        "SELECT event_id, kind, audio_frame, performance_ms, declared_movie_seconds, detail_json
         FROM review_session_events WHERE session_id=?1 ORDER BY sequence",
    )?;
    statement
        .query_map(params![session_id], |r| {
            Ok(Event {
                event_id: r.get(0)?,
                kind: r.get(1)?,
                audio_frame: r.get::<_, i64>(2)?.max(0) as u64,
                performance_ms: r.get(3)?,
                declared_movie_seconds: r.get(4)?,
                detail: r
                    .get::<_, Option<String>>(5)?
                    .and_then(|t| serde_json::from_str(&t).ok()),
            })
        })?
        .collect()
}

impl SessionSync {
    /// Assemble the shared synchronizer's request from stored session evidence.
    pub fn from_session(
        connection: &Connection,
        session_id: &str,
        handoff: &ReviewTargetEvidence,
    ) -> anyhow::Result<Self> {
        let (sample_rate, final_frames): (u32, Option<i64>) = connection.query_row(
            "SELECT sample_rate, final_frame_count FROM review_sessions WHERE session_id=?1",
            [session_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let received_end: i64 = connection.query_row(
            "SELECT COALESCE(MAX(start_frame + frame_count), 0) FROM review_session_chunks WHERE session_id=?1",
            [session_id],
            |r| r.get(0),
        )?;
        let events = load_events(connection, session_id)?;
        let recording_end = final_frames.unwrap_or(0).max(received_end).max(
            events
                .iter()
                .map(|e| e.audio_frame as i64)
                .max()
                .unwrap_or(0),
        ) as u64;
        let mut diagnostics = Vec::new();
        let policy = SyncPolicy::default();

        // One capture clock per calibrated clock identity. Several probes of
        // one clock must share a performance epoch; their origin spread is
        // added to the declared tolerance instead of being averaged away.
        let mut groups: BTreeMap<String, Vec<Calibration>> = BTreeMap::new();
        for event in events.iter().filter(|e| e.kind == "clockSample") {
            if let Some(c) = calibration(event, sample_rate) {
                groups.entry(c.clock_id.clone()).or_default().push(c);
            }
        }
        struct Clock {
            clock: CaptureClock,
            epoch: u64,
            performance_epoch_ms: f64,
        }
        let mut clocks: Vec<Clock> = Vec::new();
        for (clock_id, group) in groups {
            let best = group
                .iter()
                .min_by_key(|c| c.tolerance_ns)
                .expect("non-empty");
            if group.iter().any(|c| {
                c.performance_epoch_ms != best.performance_epoch_ms || c.epoch != best.epoch
            }) {
                diagnostics.push(format!(
                    "clock {clock_id}: calibrations disagree on epoch; clock left uncalibrated"
                ));
                continue;
            }
            let tolerance = group
                .iter()
                .map(|c| (c.origin_ns - best.origin_ns).abs() + c.tolerance_ns)
                .max()
                .unwrap_or(best.tolerance_ns);
            if tolerance > 1_000_000_000 {
                diagnostics.push(format!(
                    "clock {clock_id}: calibration spread exceeds 1 s; clock left uncalibrated"
                ));
                continue;
            }
            clocks.push(Clock {
                clock: CaptureClock {
                    clock_id,
                    sample_rate,
                    first_sample: group.iter().map(|c| c.first_sample).min().unwrap_or(0),
                    last_sample: 0,
                    monotonic_origin_ns: best.origin_ns as u64,
                    monotonic_tolerance_ns: tolerance as u64,
                },
                epoch: best.epoch,
                performance_epoch_ms: best.performance_epoch_ms,
            });
        }
        // Each clock runs until the next one starts (sharing that endpoint,
        // so a query's coverage is exact) or the recording ends.
        clocks.sort_by_key(|c| c.clock.first_sample);
        let starts: Vec<u64> = clocks.iter().map(|c| c.clock.first_sample).collect();
        for (index, c) in clocks.iter_mut().enumerate() {
            c.clock.last_sample = starts
                .get(index + 1)
                .copied()
                .unwrap_or(recording_end)
                .max(c.clock.first_sample);
        }

        let movie = &handoff.target().movie;
        let tps = movie.time_base_denominator as f64 / movie.time_base_numerator as f64;
        let movie_clock = rcdvlog_core::tv_review_sync::SyncMovieClock {
            movie_digest: movie.blake3.clone(),
            time_base_numerator: movie.time_base_numerator as u32,
            time_base_denominator: movie.time_base_denominator as u32,
            origin_pts: movie.origin_pts,
            duration_pts: (movie.duration_seconds * tps).round().max(1.0) as u64,
        };
        let mut anchors = Vec::new();
        let mut boundaries = Vec::new();
        for event in &events {
            let epoch = event
                .detail
                .as_ref()
                .and_then(|d| d.get("clockEpoch"))
                .and_then(|v| v.as_u64());
            match event.kind.as_str() {
                "manualSyncCue" => {
                    let Some(declared) = event.declared_movie_seconds else {
                        continue;
                    };
                    let Some(clock) = clocks.iter().find(|c| Some(c.epoch) == epoch) else {
                        diagnostics.push(format!(
                            "cue {}: no calibrated clock for its epoch; not used",
                            event.event_id
                        ));
                        continue;
                    };
                    let Some(performance_ms) = event.performance_ms else {
                        diagnostics.push(format!(
                            "cue {}: no monotonic observation; not used",
                            event.event_id
                        ));
                        continue;
                    };
                    let monotonic_ns =
                        ((performance_ms - clock.performance_epoch_ms) * 1e6).round();
                    let c = &clock.clock;
                    let sample = (monotonic_ns - c.monotonic_origin_ns as f64) / 1e9
                        * f64::from(c.sample_rate);
                    if monotonic_ns < 0.0
                        || !sample.is_finite()
                        || sample.round() < c.first_sample as f64
                        || sample.round() > c.last_sample as f64
                    {
                        diagnostics.push(format!(
                            "cue {}: outside its calibrated clock's capture range; not used",
                            event.event_id
                        ));
                        continue;
                    }
                    let movie_pts = movie.origin_pts + (declared * tps).round() as i64;
                    anchors.push(SyncAnchor {
                        evidence_id: event.event_id.clone(),
                        capture: CapturePosition {
                            clock_id: c.clock_id.clone(),
                            sample_index: sample.round() as u64,
                            monotonic_ns: monotonic_ns as u64,
                        },
                        movie_pts,
                        source: SyncAnchorSource::ManualCue,
                        confidence: MANUAL_CUE_CONFIDENCE,
                        movie_uncertainty_ticks: (MANUAL_CUE_MOVIE_UNCERTAINTY_SECONDS * tps).ceil()
                            as u64,
                        capture_uncertainty_samples: (c.monotonic_tolerance_ns as f64 / 1e9
                            * f64::from(c.sample_rate))
                        .ceil() as u32,
                        candidate_group_id: None,
                    });
                }
                "captureDiscontinuity"
                | "localBufferOverflow"
                | "deviceLost"
                | "pageInterrupted" => {
                    if let Some(c) = clocks.iter().find(|c| {
                        (c.clock.first_sample..=c.clock.last_sample).contains(&event.audio_frame)
                    }) {
                        boundaries.push(SyncBoundary {
                            evidence_id: event.event_id.clone(),
                            clock_id: c.clock.clock_id.clone(),
                            sample_index: event.audio_frame,
                            kind: SyncBoundaryKind::CaptureInterrupted,
                        });
                    }
                }
                _ => {}
            }
        }

        let capture_clocks: Vec<CaptureClock> = clocks.into_iter().map(|c| c.clock).collect();
        let (status, note) = if capture_clocks.is_empty() {
            (
                "pending",
                "No calibrated capture clock (review-capture-calibration.v1) is recorded for this session, so synchronization is pending: remarks stay on the recording clock and are not assigned to cuts by time.",
            )
        } else if anchors.is_empty() {
            (
                "noAnchors",
                "The capture clock is calibrated but no usable cue or detection anchors it to the movie: every interval is unknown.",
            )
        } else {
            (
                "mapped",
                "Movie time comes from the shared synchronizer over your cues. Intervals far from an anchor, across a capture interruption or between inconsistent anchors stay unknown or conflicting and get no cut candidates.",
            )
        };
        let request = (!capture_clocks.is_empty()).then(|| SyncRequest {
            schema_version: SYNC_VERSION,
            session_id: session_id.to_string(),
            target_digest: handoff.target_digest().to_string(),
            movie: movie_clock,
            capture_clocks: capture_clocks.clone(),
            anchors: anchors.clone(),
            boundaries: boundaries.clone(),
            policy: policy.clone(),
        });
        let mut description = SyncDescription {
            status: status.into(),
            source: "sharedSync".into(),
            note: note.into(),
            calibrated_clocks: capture_clocks.len(),
            anchors: anchors.len(),
            boundaries: boundaries.len(),
            policy,
            diagnostics,
        };
        if let Some(request) = &request
            && let Err(error) = request.validate()
        {
            description.status = "failed".into();
            description.diagnostics.push(format!("{error:#}"));
            return Ok(Self {
                request: None,
                clocks: capture_clocks,
                description,
            });
        }
        Ok(Self {
            request,
            clocks: capture_clocks,
            description,
        })
    }

    pub fn description(&self) -> &SyncDescription {
        &self.description
    }

    /// Map native `[start_frame, end_frame]` intervals through the shared
    /// mapper with conservative cut candidates. One result per interval.
    pub fn map_intervals(
        &mut self,
        intervals: &[(u64, u64)],
        handoff: &ReviewTargetEvidence,
    ) -> Vec<UtteranceSync> {
        let pending = || UtteranceSync {
            state: "pending".into(),
            slices: vec![],
            uncovered_frames: 0,
        };
        let Some(request) = &self.request else {
            return intervals.iter().map(|_| pending()).collect();
        };
        // Split each interval at clock ranges; remember which interval owns
        // each query so slices can be returned per utterance.
        let mut queries: Vec<(usize, SyncQuery)> = Vec::new();
        let mut uncovered = vec![0u64; intervals.len()];
        for (index, &(start, end)) in intervals.iter().enumerate() {
            let mut covered = 0u64;
            for c in &self.clocks {
                let s = start.max(c.first_sample);
                let e = end.min(c.last_sample);
                if s <= e {
                    covered += e - s;
                    queries.push((
                        index,
                        SyncQuery {
                            clock_id: c.clock_id.clone(),
                            start_sample: s,
                            end_sample: e,
                        },
                    ));
                }
            }
            uncovered[index] = end.saturating_sub(start).saturating_sub(covered);
        }
        let mut slices: Vec<Vec<SyncSlice>> = vec![Vec::new(); intervals.len()];
        for batch in queries.chunks(MAX_QUERIES_PER_CALL) {
            let only: Vec<SyncQuery> = batch.iter().map(|(_, q)| q.clone()).collect();
            let report: SyncReport = match handoff.map_to_cuts(request, &only, &|| false) {
                Ok(report) => report,
                Err(error) => {
                    self.description.status = "failed".into();
                    self.description.diagnostics.push(format!("{error:#}"));
                    return intervals.iter().map(|_| pending()).collect();
                }
            };
            // Slices are emitted in query order; each query's last slice ends
            // at the query's end sample.
            let mut produced = report.slices.into_iter();
            for (index, query) in batch {
                for slice in produced.by_ref() {
                    let last = slice.recording.end_sample == query.end_sample;
                    slices[*index].push(slice);
                    if last {
                        break;
                    }
                }
            }
        }
        slices
            .into_iter()
            .zip(uncovered)
            .map(|(slices, uncovered_frames)| {
                let mut states: Vec<String> = slices
                    .iter()
                    .map(|s| {
                        serde_json::to_value(s.state)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_string))
                            .unwrap_or_default()
                    })
                    .collect();
                if uncovered_frames > 0 {
                    states.push("unknown".into());
                }
                states.sort();
                states.dedup();
                let state = match states.as_slice() {
                    [] => "unknown".to_string(),
                    [one] => one.clone(),
                    _ => "mixed".to_string(),
                };
                UtteranceSync {
                    state,
                    slices,
                    uncovered_frames,
                }
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "review_sync_tests.rs"]
mod tests;
