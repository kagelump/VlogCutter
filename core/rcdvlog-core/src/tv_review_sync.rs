//! Versioned evidence-only microphone sample clock -> exact movie clock mapper.
//! Stateless/retryable; never owns a recording, timeline, transcript or decision.
use crate::{ReviewTargetHandoff, tv_soundtrack_consensus::ConsensusSegment};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

pub const SYNC_VERSION: u32 = 1;
pub const SYNC_MAX_BYTES: usize = 4 * 1024 * 1024;
const SAFE: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncMovieClock {
    pub movie_digest: String,
    pub time_base_numerator: u32,
    pub time_base_denominator: u32,
    pub origin_pts: i64,
    pub duration_pts: u64,
}
impl SyncMovieClock {
    pub fn ticks_per_second(&self) -> f64 {
        self.time_base_denominator as f64 / self.time_base_numerator as f64
    }
    fn end(&self) -> f64 {
        self.origin_pts as f64 + self.duration_pts as f64
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureClock {
    pub clock_id: String,
    pub sample_rate: u32,
    pub first_sample: u64,
    pub last_sample: u64,
    /// Monotonic clock origin, never wall-clock time. Serialized integer
    /// nanoseconds and samples are limited to JavaScript-safe integers in v1.
    pub monotonic_origin_ns: u64,
    /// Declared calibration error, not movie-time precision.
    pub monotonic_tolerance_ns: u64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapturePosition {
    pub clock_id: String,
    pub sample_index: u64,
    pub monotonic_ns: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncAnchorSource {
    ManualCue,
    AcousticTimecode,
    PassiveSoundtrack,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncAnchor {
    pub evidence_id: String,
    pub capture: CapturePosition,
    pub movie_pts: i64,
    pub source: SyncAnchorSource,
    pub confidence: f64,
    pub movie_uncertainty_ticks: u64,
    pub capture_uncertainty_samples: u32,
    /// Multiple positions for the same detection remain alternatives, never
    /// votes that can overwhelm another observation.
    pub candidate_group_id: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncBoundaryKind {
    Pause,
    Buffering,
    Seek,
    Rewind,
    Resume,
    Resync,
    CaptureInterrupted,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncBoundary {
    pub evidence_id: String,
    pub clock_id: String,
    pub sample_index: u64,
    pub kind: SyncBoundaryKind,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncPolicy {
    pub minimum_confidence: f64,
    pub max_anchor_gap_seconds: f64,
    /// Single-anchor estimation is approximate 1x, explicitly opt-in (0 disables).
    pub max_extrapolation_seconds: f64,
    pub max_drift_ppm: f64,
    pub residual_tolerance_seconds: f64,
    /// Unobserved playback variation grows the uncertainty between observations.
    pub unobserved_uncertainty_ppm: f64,
}
impl Default for SyncPolicy {
    fn default() -> Self {
        Self {
            minimum_confidence: 0.8,
            max_anchor_gap_seconds: 15.0,
            max_extrapolation_seconds: 0.0,
            max_drift_ppm: 2000.0,
            residual_tolerance_seconds: 0.1,
            unobserved_uncertainty_ppm: 5000.0,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncRequest {
    pub schema_version: u32,
    pub session_id: String,
    pub target_digest: String,
    pub movie: SyncMovieClock,
    pub capture_clocks: Vec<CaptureClock>,
    pub anchors: Vec<SyncAnchor>,
    pub boundaries: Vec<SyncBoundary>,
    pub policy: SyncPolicy,
    /// Copied from consensus output; the mapper never infers them (§9).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consensus_segments: Vec<ConsensusSegment>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncState {
    Anchored,
    ConsensusInterpolated,
    Estimated,
    Conflicting,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncQuery {
    pub clock_id: String,
    pub start_sample: u64,
    pub end_sample: u64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MovieEstimate {
    /// Floats are estimates on the exact rational clock, never integer precision
    /// claims. Bounds are rounded outwards to whole movie ticks.
    pub start_pts: f64,
    pub end_pts: f64,
    pub lower_pts: i64,
    pub upper_pts: i64,
    pub uncertainty_ticks: u64,
    pub confidence: f64,
    pub evidence_ids: Vec<String>,
    pub measured_drift_ppm: Option<f64>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncSlice {
    pub recording: SyncQuery,
    pub state: SyncState,
    pub candidates: Vec<MovieEstimate>,
    pub diagnostics: Vec<String>,
    pub candidate_operation_ids: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncReport {
    pub schema_version: u32,
    pub session_id: String,
    pub target_digest: String,
    pub slices: Vec<SyncSlice>,
    pub rejected_evidence_ids: Vec<String>,
    /// Unmodified observations, including weak and ambiguous candidates.
    pub anchors: Vec<SyncAnchor>,
}

fn opaque(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
impl SyncRequest {
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= SYNC_MAX_BYTES,
            "invalid input: synchronization request exceeds 4 MiB"
        );
        let request: Self = serde_json::from_slice(bytes)?;
        request.validate()?;
        Ok(request)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == SYNC_VERSION,
            "invalid input: unsupported synchronization version"
        );
        ensure!(
            opaque(&self.session_id)
                && digest(&self.target_digest)
                && digest(&self.movie.movie_digest),
            "invalid input: sync identities"
        );
        let m = &self.movie;
        ensure!(
            m.time_base_numerator > 0
                && m.time_base_denominator > 0
                && m.origin_pts.unsigned_abs() <= SAFE
                && m.duration_pts > 0
                && m.duration_pts <= SAFE
                && m.end().abs() <= SAFE as f64
                && m.duration_pts as f64 / m.ticks_per_second() <= 21600.0,
            "invalid input: movie clock"
        );
        ensure!(
            !self.capture_clocks.is_empty()
                && self.capture_clocks.len() <= 64
                && self.anchors.len() <= 4096
                && self.boundaries.len() <= 4096,
            "invalid input: synchronization evidence bounds"
        );
        let p = &self.policy;
        ensure!(
            [
                p.minimum_confidence,
                p.max_anchor_gap_seconds,
                p.max_extrapolation_seconds,
                p.max_drift_ppm,
                p.residual_tolerance_seconds,
                p.unobserved_uncertainty_ppm
            ]
            .iter()
            .all(|x| x.is_finite())
                && (0.0..=1.0).contains(&p.minimum_confidence)
                && (0.1..=120.0).contains(&p.max_anchor_gap_seconds)
                && (0.0..=30.0).contains(&p.max_extrapolation_seconds)
                && (0.0..=500_000.0).contains(&p.max_drift_ppm)
                && (0.0..=2.0).contains(&p.residual_tolerance_seconds)
                && (0.0..=100_000.0).contains(&p.unobserved_uncertainty_ppm),
            "invalid input: sync policy"
        );
        let mut clocks = HashSet::new();
        for c in &self.capture_clocks {
            ensure!(
                opaque(&c.clock_id)
                    && clocks.insert(&c.clock_id)
                    && (8000..=192000).contains(&c.sample_rate)
                    && c.first_sample <= c.last_sample
                    && c.last_sample <= SAFE
                    && c.monotonic_origin_ns <= SAFE
                    && c.monotonic_tolerance_ns <= 1_000_000_000
                    && c.last_sample as f64 / c.sample_rate as f64 * 1e9
                        + c.monotonic_origin_ns as f64
                        <= SAFE as f64,
                "invalid input: capture clock"
            );
        }
        let mut ids = HashSet::new();
        for a in &self.anchors {
            let c = self.clock(&a.capture.clock_id)?;
            let expected = c.monotonic_origin_ns as f64
                + a.capture.sample_index as f64 / c.sample_rate as f64 * 1e9;
            ensure!(
                opaque(&a.evidence_id)
                    && ids.insert(&a.evidence_id)
                    && a.candidate_group_id.as_ref().is_none_or(|id| opaque(id))
                    && a.capture.sample_index >= c.first_sample
                    && a.capture.sample_index <= c.last_sample
                    && a.capture.monotonic_ns <= SAFE
                    && (a.capture.monotonic_ns as f64 - expected).abs()
                        <= c.monotonic_tolerance_ns as f64
                    && a.movie_pts >= m.origin_pts
                    && a.movie_pts as f64 <= m.end()
                    && a.confidence.is_finite()
                    && (0.0..=1.0).contains(&a.confidence)
                    && a.movie_uncertainty_ticks <= m.duration_pts
                    && a.capture_uncertainty_samples <= c.sample_rate * 10,
                "invalid input: anchor clock/evidence; sample and monotonic clocks exceed their declared calibration tolerance"
            );
        }
        for b in &self.boundaries {
            let c = self.clock(&b.clock_id)?;
            ensure!(
                opaque(&b.evidence_id)
                    && ids.insert(&b.evidence_id)
                    && b.sample_index >= c.first_sample
                    && b.sample_index <= c.last_sample,
                "invalid input: discontinuity evidence"
            );
        }
        ensure!(
            self.consensus_segments.len() <= 4096,
            "invalid input: consensus segment bounds"
        );
        for s in &self.consensus_segments {
            let c = self.clock(&s.clock_id)?;
            ensure!(
                opaque(&s.segment_id)
                    && s.mic_start_sample <= s.mic_end_sample
                    && s.mic_start_sample >= c.first_sample
                    && s.mic_end_sample <= c.last_sample
                    && s.offset_seconds.is_finite()
                    && s.residual_rms_seconds.is_finite()
                    && s.residual_rms_seconds >= 0.0,
                "invalid input: consensus segment"
            );
        }
        Ok(())
    }
    fn clock(&self, id: &str) -> Result<&CaptureClock> {
        self.capture_clocks
            .iter()
            .find(|c| c.clock_id == id)
            .ok_or_else(|| anyhow::anyhow!("invalid input: unknown capture clock"))
    }
    fn uncertainty(&self, a: &SyncAnchor, c: &CaptureClock) -> f64 {
        a.movie_uncertainty_ticks as f64
            + a.capture_uncertainty_samples as f64 / c.sample_rate as f64
                * self.movie.ticks_per_second()
    }
    fn crosses(&self, clock: &str, a: u64, b: u64) -> bool {
        self.boundaries
            .iter()
            .any(|e| e.clock_id == clock && a < e.sample_index && e.sample_index <= b)
    }
    /// The first supplied segment whose supporting span holds both anchors and
    /// the whole query (ARCHITECTURE.md §9). Segments are never inferred here.
    fn consensus_span(
        &self,
        c: &CaptureClock,
        left: u64,
        right: u64,
        q: &SyncQuery,
    ) -> Option<&ConsensusSegment> {
        self.consensus_segments.iter().find(|s| {
            s.clock_id == c.clock_id
                && s.mic_start_sample <= left
                && right <= s.mic_end_sample
                && s.mic_start_sample <= q.start_sample
                && q.end_sample <= s.mic_end_sample
        })
    }

    /// Deterministic bounded pure computation. Cancellation is checked per query
    /// slice; a cancelled call has no side effects and can be retried verbatim.
    pub fn map(&self, queries: &[SyncQuery], cancel: &dyn Fn() -> bool) -> Result<SyncReport> {
        self.validate()?;
        ensure!(
            queries.len() <= 256,
            "invalid input: at most 256 sync queries"
        );
        let rejected: Vec<_> = self
            .anchors
            .iter()
            .filter(|a| a.confidence < self.policy.minimum_confidence)
            .map(|a| a.evidence_id.clone())
            .collect();
        let mut slices = Vec::new();
        let mut estimate_count = 0usize;
        for q in queries {
            let c = self.clock(&q.clock_id)?;
            ensure!(
                q.start_sample <= q.end_sample
                    && q.start_sample >= c.first_sample
                    && q.end_sample <= c.last_sample,
                "invalid input: query outside capture"
            );
            let mut points = vec![q.start_sample, q.end_sample];
            for a in &self.anchors {
                if a.capture.clock_id == q.clock_id
                    && a.capture.sample_index > q.start_sample
                    && a.capture.sample_index < q.end_sample
                {
                    points.push(a.capture.sample_index);
                }
            }
            for b in &self.boundaries {
                if b.clock_id == q.clock_id
                    && b.sample_index > q.start_sample
                    && b.sample_index < q.end_sample
                {
                    points.push(b.sample_index);
                }
            }
            points.sort_unstable();
            points.dedup();
            let intervals: Vec<_> = if points.len() == 1 {
                vec![(points[0], points[0])]
            } else {
                points.windows(2).map(|w| (w[0], w[1])).collect()
            };
            ensure!(
                slices.len() + intervals.len() <= 8192,
                "invalid input: sync result exceeds slice bound"
            );
            for (start, end) in intervals {
                ensure!(!cancel(), "synchronization cancelled");
                let slice = self.map_slice(
                    c,
                    SyncQuery {
                        clock_id: q.clock_id.clone(),
                        start_sample: start,
                        end_sample: end,
                    },
                );
                estimate_count += slice.candidates.len();
                ensure!(
                    estimate_count <= 8192,
                    "invalid input: sync result exceeds candidate budget; narrow the query/evidence scope"
                );
                slices.push(slice);
            }
        }
        Ok(SyncReport {
            schema_version: SYNC_VERSION,
            session_id: self.session_id.clone(),
            target_digest: self.target_digest.clone(),
            slices,
            rejected_evidence_ids: rejected,
            anchors: self.anchors.clone(),
        })
    }
    fn map_slice(&self, c: &CaptureClock, q: SyncQuery) -> SyncSlice {
        let mut slice = SyncSlice {
            recording: q.clone(),
            state: SyncState::Unknown,
            candidates: vec![],
            diagnostics: vec![],
            candidate_operation_ids: vec![],
        };
        let mut groups: BTreeMap<u64, Vec<&SyncAnchor>> = BTreeMap::new();
        for a in self.anchors.iter().filter(|a| {
            a.capture.clock_id == c.clock_id && a.confidence >= self.policy.minimum_confidence
        }) {
            groups.entry(a.capture.sample_index).or_default().push(a);
        }
        let left = groups.range(..=q.start_sample).next_back();
        let right = groups.range(q.end_sample..).next();
        let tps = self.movie.ticks_per_second();
        let tolerance = self.policy.residual_tolerance_seconds * tps;
        if let (Some((&ls, la)), Some((&rs, ra))) = (left, right) {
            if ls == rs && q.start_sample == q.end_sample {
                for a in la {
                    slice.candidates.push(self.estimate(
                        a.movie_pts as f64,
                        a.movie_pts as f64,
                        self.uncertainty(a, c),
                        a.confidence,
                        vec![a.evidence_id.clone()],
                        None,
                    ));
                }
                slice.state = SyncState::Anchored;
            } else if rs > ls
                && !self.crosses(&c.clock_id, ls, rs)
                && ((rs - ls) as f64 / c.sample_rate as f64 <= self.policy.max_anchor_gap_seconds
                    || self.consensus_span(c, ls, rs, &q).is_some())
            {
                // Interpolation beyond max_anchor_gap_seconds is reached only through
                // a supplied consensus segment that contains both anchors and the query.
                let consensus = if (rs - ls) as f64 / c.sample_rate as f64
                    > self.policy.max_anchor_gap_seconds
                {
                    self.consensus_span(c, ls, rs, &q)
                } else {
                    None
                };
                // All alternatives are retained. Too many combinations remain
                // unknown rather than allowing quadratic work from hostile input.
                if la.len() * ra.len() > 64 {
                    slice
                        .diagnostics
                        .push("too_many_anchor_alternatives".into());
                    return slice;
                }
                // Offset implied by an anchor: movie seconds (from origin) minus mic seconds.
                let anchor_offset = |a: &SyncAnchor| {
                    (a.movie_pts - self.movie.origin_pts) as f64 / tps
                        - a.capture.sample_index as f64 / c.sample_rate as f64
                };
                for a in la {
                    for b in ra {
                        let capture_seconds = (rs - ls) as f64 / c.sample_rate as f64;
                        let progress = (b.movie_pts - a.movie_pts) as f64;
                        let rate = progress / capture_seconds;
                        let drift = (rate / tps - 1.0) * 1e6;
                        let allowed = self.policy.max_drift_ppm / 1e6 * tps * capture_seconds
                            + self.uncertainty(a, c)
                            + self.uncertainty(b, c)
                            + tolerance;
                        if progress <= 0.0 || (progress - capture_seconds * tps).abs() > allowed {
                            slice
                                .diagnostics
                                .push("inconsistent_progress_requires_resync".into());
                            continue;
                        }
                        // ARCHITECTURE.md §9: consensus must not override anchors whose
                        // offset disagrees with the segment beyond residual tolerance.
                        if let Some(seg) = consensus
                            && [a, b].iter().any(|x| {
                                (anchor_offset(x) - seg.offset_seconds).abs()
                                    > self.policy.residual_tolerance_seconds
                            })
                        {
                            slice
                                .diagnostics
                                .push("inconsistent_progress_requires_resync".into());
                            continue;
                        }
                        let map = |s: u64| {
                            a.movie_pts as f64 + (s - ls) as f64 / (rs - ls) as f64 * progress
                        };
                        let unobserved =
                            capture_seconds * tps * self.policy.unobserved_uncertainty_ppm / 1e6;
                        let uncertainty = match consensus {
                            // ARCHITECTURE.md §9: anchor uncertainty + residual_rms + unobserved.
                            Some(seg) => {
                                self.uncertainty(a, c).max(self.uncertainty(b, c))
                                    + seg.residual_rms_seconds * tps
                                    + unobserved
                            }
                            None => {
                                self.uncertainty(a, c).max(self.uncertainty(b, c))
                                    + tolerance
                                    + unobserved
                            }
                        };
                        slice.candidates.push(self.estimate(
                            map(q.start_sample),
                            map(q.end_sample),
                            uncertainty,
                            a.confidence.min(b.confidence),
                            vec![a.evidence_id.clone(), b.evidence_id.clone()],
                            Some(drift),
                        ));
                    }
                }
                slice.state = match (slice.diagnostics.is_empty(), consensus) {
                    (false, _) => SyncState::Conflicting,
                    (true, Some(_)) => SyncState::ConsensusInterpolated,
                    (true, None) => SyncState::Estimated,
                };
                if let (SyncState::ConsensusInterpolated, Some(seg)) = (slice.state, consensus) {
                    slice
                        .diagnostics
                        .push(format!("consensus_segment:{}", seg.segment_id));
                }
            } else {
                slice.diagnostics.push("gap_or_discontinuity".into());
            }
        } else if self.policy.max_extrapolation_seconds > 0.0 {
            let nearest = left.or(right);
            if let Some((&s, aa)) = nearest {
                let distance = q.start_sample.abs_diff(s).max(q.end_sample.abs_diff(s)) as f64
                    / c.sample_rate as f64;
                if distance <= self.policy.max_extrapolation_seconds
                    && !self.crosses(&c.clock_id, s.min(q.start_sample), s.max(q.end_sample))
                {
                    for a in aa {
                        let map = |x: u64| {
                            a.movie_pts as f64 + (x as f64 - s as f64) / c.sample_rate as f64 * tps
                        };
                        let uncertainty = self.uncertainty(a, c)
                            + tolerance
                            + distance
                                * tps
                                * (self.policy.max_drift_ppm
                                    + self.policy.unobserved_uncertainty_ppm)
                                / 1e6;
                        slice.candidates.push(self.estimate(
                            map(q.start_sample),
                            map(q.end_sample),
                            uncertainty,
                            a.confidence,
                            vec![a.evidence_id.clone()],
                            None,
                        ));
                    }
                    slice.state = SyncState::Estimated;
                    slice.diagnostics.push("assumed_uninterrupted_1x".into());
                }
            }
        }
        if slice
            .candidates
            .iter()
            .any(|a| a.start_pts < self.movie.origin_pts as f64 || a.end_pts > self.movie.end())
        {
            slice.candidates.clear();
            slice.state = SyncState::Unknown;
            slice.diagnostics.push("outside_movie".into());
        }
        if slice.candidates.len() > 1 {
            let lo = slice
                .candidates
                .iter()
                .map(|a| a.start_pts)
                .fold(f64::INFINITY, f64::min);
            let hi = slice
                .candidates
                .iter()
                .map(|a| a.start_pts)
                .fold(f64::NEG_INFINITY, f64::max);
            // Evidence intervals must have a common intersection, not merely
            // a high score; never choose manual vs automatic by source priority.
            let low = slice.candidates.iter().map(|a| a.lower_pts).max().unwrap();
            let high = slice.candidates.iter().map(|a| a.upper_pts).min().unwrap();
            if low > high || hi - lo > tolerance {
                slice.state = SyncState::Conflicting;
                slice
                    .diagnostics
                    .push("contradictory_or_ambiguous_anchors".into());
            }
        }
        if slice
            .candidates
            .iter()
            .flat_map(|e| &e.evidence_ids)
            .any(|id| {
                self.anchors
                    .iter()
                    .find(|a| &a.evidence_id == id)
                    .and_then(|a| a.candidate_group_id.as_ref())
                    .is_some_and(|group| {
                        self.anchors
                            .iter()
                            .filter(|a| a.candidate_group_id.as_ref() == Some(group))
                            .count()
                            > 1
                    })
            })
        {
            slice.state = SyncState::Conflicting;
            slice.diagnostics.push("unresolved_candidate_group".into());
        }
        slice.diagnostics.sort();
        slice.diagnostics.dedup();
        slice
    }
    fn estimate(
        &self,
        start: f64,
        end: f64,
        u: f64,
        confidence: f64,
        evidence_ids: Vec<String>,
        drift: Option<f64>,
    ) -> MovieEstimate {
        MovieEstimate {
            start_pts: start,
            end_pts: end,
            lower_pts: (start.min(end) - u)
                .floor()
                .max(self.movie.origin_pts as f64) as i64,
            upper_pts: (start.max(end) + u).ceil().min(self.movie.end()) as i64,
            uncertainty_ticks: u.ceil() as u64,
            confidence,
            evidence_ids,
            measured_drift_ppm: drift,
        }
    }
    /// Exact target binding and conservative candidate operation lookup. Unknown
    /// and conflicting slices cannot make a cut assertion. Bounds include clock
    /// uncertainty; these are picture candidates, never source-frame trims.
    pub fn map_to_cuts(
        &self,
        queries: &[SyncQuery],
        target: &ReviewTargetHandoff,
        cancel: &dyn Fn() -> bool,
    ) -> Result<SyncReport> {
        target.validate()?;
        self.map_to_snapshot(queries, &target.target, &target.target_digest, cancel)
    }
    /// Explicit historical-review boundary. This provides picture locators for
    /// the already rendered movie; it cannot establish current renderability.
    pub fn map_to_historical_cuts(
        &self,
        queries: &[SyncQuery],
        target: &crate::HistoricalRenderedArtifactReview,
        cancel: &dyn Fn() -> bool,
    ) -> Result<SyncReport> {
        target.validate()?;
        self.map_to_snapshot(queries, &target.target, &target.target_digest, cancel)
    }
    fn map_to_snapshot(
        &self,
        queries: &[SyncQuery],
        target: &crate::ReviewTargetSnapshot,
        target_digest: &str,
        cancel: &dyn Fn() -> bool,
    ) -> Result<SyncReport> {
        let m = &target.movie;
        ensure!(
            self.target_digest == target_digest
                && self.movie.movie_digest == m.blake3
                && i64::from(self.movie.time_base_numerator) == m.time_base_numerator
                && i64::from(self.movie.time_base_denominator) == m.time_base_denominator
                && self.movie.origin_pts == m.origin_pts
                && (self.movie.duration_pts as f64 / self.movie.ticks_per_second()
                    - m.duration_seconds)
                    .abs()
                    <= 1.0 / self.movie.ticks_per_second(),
            "invalid input: synchronization target/movie clock mismatch"
        );
        let mut report = self.map(queries, cancel)?;
        let mut operation_count = 0usize;
        for s in &mut report.slices {
            if matches!(s.state, SyncState::Anchored | SyncState::Estimated) {
                for e in &s.candidates {
                    ensure!(!cancel(), "synchronization cancelled");
                    let seconds =
                        |p: i64| (p - self.movie.origin_pts) as f64 / self.movie.ticks_per_second();
                    s.candidate_operation_ids
                        .extend(target.picture_candidates_validated(
                            seconds(e.lower_pts),
                            seconds(e.upper_pts).min(m.duration_seconds),
                        )?);
                    s.candidate_operation_ids.sort();
                    s.candidate_operation_ids.dedup();
                    ensure!(
                        operation_count + s.candidate_operation_ids.len() <= 8192,
                        "invalid input: sync result exceeds operation budget; narrow the query scope"
                    );
                }
                operation_count += s.candidate_operation_ids.len();
                ensure!(
                    operation_count <= 8192,
                    "invalid input: sync result exceeds operation budget; narrow the query scope"
                );
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
#[path = "tv_review_sync_tests.rs"]
mod tests;
