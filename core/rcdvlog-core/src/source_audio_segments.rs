//! Conservative segment assembly (source-audio MVP step 3): turns accepted
//! recognition windows into supported segments and explicit unknown ranges,
//! and adapts retained support positions to the existing tv_review_sync
//! query path without synthesized anchors.
//!
//! Frozen assembly semantics (recipe `source-audio-pooled-mvp-v1` evidence,
//! assembler identity [`ASSEMBLER_ID`]):
//!
//! 1. Each accepted window contributes its offset plus the one-second cells
//!    and absolute query-anchor feature frames that actually support it. A
//!    recognized window is never proof of uninterrupted playback throughout
//!    its whole range.
//! 2. Overlapping accepted windows are reconciled in recording coordinates.
//!    Compatible offsets differ by at most one 80 ms bin; a cell with
//!    incompatible accepted hypotheses is conflicting and stays unknown — it
//!    is never continuity-picked. Reconciled cells keep the median of the
//!    compatible hypotheses and all supporting anchor frames.
//! 3. Adjacent supported cells with compatible offsets in one epoch group by a
//!    median offset with EVERY member within 80 ms of that representative;
//!    transitive drift chains never merge. Anchor IDs deduplicate in support
//!    counts across overlapping windows.
//! 4. Uninterrupted support prefers a single segment. An internal unsupported
//!    gap of at most two seconds may stay inside one segment envelope only
//!    when both sides agree and there is no contradictory evidence (a
//!    conflicting cell or any weak/negative/abstained/incomplete window
//!    covering the gap) and no declared boundary; it is recorded explicitly as
//!    an unknown span with no support and no movie position. Longer gaps split
//!    segments.
//! 5. Known pause/seek/rewind/interruption boundaries, capture-epoch changes
//!    and incompatible evidence split segments; transition cells stay unknown.
//!
//! Mapping has fixed playback rate 1.0: there is no drift fitting and no
//! global-offset extrapolation. Offset precision (one 80 ms bin) is reported
//! separately from boundary uncertainty (one-second evidence cells).
//!
//! Offset convention (frozen):
//! `movieElapsedSeconds = recordingSecondsWithinEpoch + offsetSeconds`;
//! raw movie PTS come from the declared movie origin and rational timebase.
//! A hole, conflict or unknown NEVER receives a movie position.

use crate::source_audio_pooled::{
    OFFSET_BIN_SECONDS, PooledRecognitionPolicy, PooledWindowResult, REASON_INCOMPLETE_EVIDENCE,
    RECIPE_ID,
};
use crate::source_audio_sessions::{
    QUERY_BLOCK_SECONDS, SCHEDULE_STEP_SECONDS, SCHEDULE_WINDOW_SECONDS, SchedulingIdentity,
    SessionIdentity, SessionProgress, SessionResult, SessionRunRequest, SessionWindowResult,
};
use crate::tv_review_sync::{
    CaptureClock, CapturePosition, MovieEstimate, SYNC_VERSION, SyncAnchor, SyncAnchorSource,
    SyncBoundary, SyncMovieClock, SyncPolicy, SyncQuery, SyncRequest, SyncState,
};
use crate::tv_soundtrack_landmarks::{HOP, Index, RATE as FEATURE_RATE, decode_wav};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Versioned identity of this segment assembler. A semantics change requires a
/// new identity; the frozen v3 acceptance verdicts are never re-labelled.
pub const ASSEMBLER_ID: &str = "source-audio-segments-mvp-v1";

/// Report schema version of [`SegmentReport`].
pub const SEGMENT_REPORT_SCHEMA_VERSION: u32 = 1;

/// MVP mapping has fixed playback rate 1.0 (no drift fitting, no rate search).
pub const PLAYBACK_RATE: f64 = 1.0;

/// One-second evidence cells: the boundary-uncertainty granularity.
pub const TIME_BIN_SECONDS: u32 = 1;

/// An internal unsupported gap of at most two seconds may remain inside one
/// segment envelope; anything longer splits segments.
pub const MAX_INTERNAL_GAP_SECONDS: u32 = 2;

/// Unknown-range/unknown-span reason vocabulary (stable strings).
pub const REASON_NO_ACCEPTED_SUPPORT: &str = "no_accepted_support";
pub const REASON_CONFLICTING_OFFSETS: &str = "conflicting_offsets";
pub const REASON_DECLARED_BOUNDARY: &str = "declared_boundary_transition";
pub const REASON_INCOMPATIBLE_TRANSITION: &str = "incompatible_offset_transition";
pub const REASON_WEAK_OR_NEGATIVE_EVIDENCE: &str = "weak_or_negative_evidence";
pub const REASON_SHORT_EPOCH: &str = "epoch_shorter_than_recognition_window";
pub const REASON_OUTSIDE_EPOCH: &str = "outside_capture_epoch";
pub const REASON_UNSUPPORTED_GAP: &str = "unsupported_gap";

/// Alternative-hypothesis resolution: never continuity-picked.
pub const RESOLUTION_CONFLICTING_UNKNOWN: &str = "conflicting_unknown";

/// Compatibility tolerance: offsets compatible iff within one 80 ms bin
/// (inclusive; small epsilon for the binary lattice of `bin * 0.08`).
const OFFSET_TOLERANCE: f64 = OFFSET_BIN_SECONDS + 1e-9;

/// The mapper accepts at most 4096 anchors; retained support beyond that is
/// deterministically thinned (which can only reduce mapper coverage).
const MAPPER_ANCHOR_LIMIT: usize = 4096;

// ---------------------------------------------------------------------------
// Input DTOs.

/// Half-open native sample range on the capture clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SampleRange {
    pub start_sample: u64,
    pub end_sample: u64,
}

/// One continuous capture-clock epoch: a stretch of the recording with its own
/// recording-time origin (recording seconds within the epoch count from
/// `start_sample`). A new epoch has its own recording-time origin. Epochs are
/// supplied in recording order and never overlap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureEpoch {
    pub epoch_id: String,
    pub start_sample: u64,
    pub end_sample: u64,
}

/// One supporting one-second cell of a window: the occupied one-second time bin
/// with the absolute query-anchor feature-frame positions that support the
/// window's offset there, plus the cell's evidence ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SupportCell {
    /// One-second cell start within the epoch, in recording seconds
    /// (integral for epoch-chunk queries; fractional when the pooled query
    /// chunk starts at a fractional window).
    pub time_bin_seconds: f64,
    /// Absolute feature-frame positions within the decoded recording.
    pub query_anchor_frames: Vec<u32>,
    pub evidence_id: String,
}

/// One recognized window handed to the assembler: a `source_audio_pooled`
/// window result plus schedule metadata. Windows with `accepted == Some(true)`
/// and `complete == true` provide support; every other window is evidence
/// without support (it may contradict gap bridging and is always reported).
/// An incomplete search NEVER matches, even when `accepted == Some(true)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcceptedWindow {
    pub window_id: String,
    pub capture_clock_id: String,
    pub epoch_id: String,
    /// Native half-open range on the capture clock; validated against
    /// `epochStartSample + windowStartSeconds * sampleRate` and
    /// `startSample + durationSeconds * sampleRate`.
    pub start_sample: u64,
    pub end_sample: u64,
    /// Recording seconds within the capture epoch (`(startSample -
    /// epochStartSample) / sampleRate`; integral for normal windows,
    /// fractional for a final window ending at a fractional epoch end).
    pub window_start_seconds: f64,
    pub duration_seconds: u16,
    /// Best-mode offset under the frozen convention
    /// `movieElapsedSeconds = recordingSecondsWithinEpoch + offsetSeconds`;
    /// `None` exactly when no offset mode exists. Callers with a rebased final
    /// window set their epoch-convention value here.
    pub offset_seconds: Option<f64>,
    /// 80 ms hypothesis bin of `offset_seconds` (the nearest bin for a rebased
    /// final window whose epoch-convention offset is off the chunk lattice).
    /// Hypothesis identities key on this bin; hypothesis compatibility is
    /// decided in offset space (at most one 80 ms bin apart).
    pub offset_bin: Option<i32>,
    /// Complete reference search AND complete accumulation.
    pub complete: bool,
    /// Tri-state v3 verdict; `None` abstains.
    pub accepted: Option<bool>,
    pub failure_reasons: Vec<String>,
    /// The one-second cells/anchor positions that actually support the best
    /// mode. Weak/rejected windows may carry cells; they never provide support.
    pub supporting_cells: Vec<SupportCell>,
}

impl AcceptedWindow {
    /// Build from a pooled window result plus schedule metadata. The offset and
    /// supporting cells come from the window's best mode (the pooled request
    /// must set `includeModeSupport`); override the offset fields afterwards
    /// for rebased final windows.
    ///
    /// `chunk_start_seconds_within_epoch` is the epoch-relative start of the
    /// pooled query chunk: `Some(0.0)`/`None` for the normal epoch-chunk query
    /// (pooled cells already epoch-relative) and `Some(windowStartSeconds)` for
    /// a fractional final window searched in its own chunk (pooled cells are
    /// chunk-relative 0-based). Epoch-correct cell time is always `chunkStart +
    /// pooled timeBin` — never inferred from the cell values.
    pub fn from_pooled(
        window_id: impl Into<String>,
        capture_clock_id: impl Into<String>,
        epoch_id: impl Into<String>,
        start_sample: u64,
        end_sample: u64,
        chunk_start_seconds_within_epoch: Option<f64>,
        window: &PooledWindowResult,
    ) -> Self {
        let window_id = window_id.into();
        let chunk_start = chunk_start_seconds_within_epoch.unwrap_or(0.0);
        let best = window.modes.first();
        let supporting_cells = best
            .and_then(|mode| mode.supporting_cells.as_deref())
            .unwrap_or(&[])
            .iter()
            .map(|cell| SupportCell {
                time_bin_seconds: chunk_start + f64::from(cell.time_bin),
                query_anchor_frames: cell.query_anchor_frames.clone(),
                evidence_id: format!("{}b{}", window_id, cell.time_bin),
            })
            .collect();
        AcceptedWindow {
            window_id,
            capture_clock_id: capture_clock_id.into(),
            epoch_id: epoch_id.into(),
            start_sample,
            end_sample,
            window_start_seconds: chunk_start + f64::from(window.start_seconds),
            duration_seconds: window.duration_seconds,
            offset_seconds: best.map(|mode| mode.offset_seconds),
            offset_bin: best.map(|mode| mode.offset_bin),
            complete: window.complete,
            accepted: window.acceptance.accepted,
            failure_reasons: window
                .acceptance
                .failure_reasons
                .iter()
                .map(|reason| (*reason).to_string())
                .collect(),
            supporting_cells,
        }
    }

    /// Build the assembler input for one session window result. Completed
    /// windows carry their pooled best-mode evidence with the session's
    /// epoch-convention offset (the pooled rows are chunk-local and get
    /// rebased); incomplete windows (cancelled, truncated, budget-infeasible)
    /// arrive as explicit non-supporting evidence and can never match.
    pub fn from_session_window(window: &SessionWindowResult) -> Self {
        let window_id = opaque_window_id(&window.window_evidence_id);
        let Some(pooled) = window.pooled.as_ref() else {
            return AcceptedWindow {
                window_id,
                capture_clock_id: window.capture_clock_id.clone(),
                epoch_id: window.epoch_id.clone(),
                start_sample: window.start_sample,
                end_sample: window.end_sample,
                window_start_seconds: window.window_start_seconds,
                duration_seconds: window.duration_seconds,
                offset_seconds: None,
                offset_bin: None,
                complete: false,
                accepted: None,
                failure_reasons: Vec::new(),
                supporting_cells: Vec::new(),
            };
        };
        let mut accepted = Self::from_pooled(
            window_id,
            window.capture_clock_id.clone(),
            window.epoch_id.clone(),
            window.start_sample,
            window.end_sample,
            window.chunk_start_seconds_within_epoch,
            pooled,
        );
        // The session already rebased the best-mode offset into the epoch
        // convention and carries the schedule-exact window start. The
        // hypothesis bin is the nearest 80 ms bin of that epoch-convention
        // offset (the session's raw bin is chunk-local evidence).
        accepted.window_start_seconds = window.window_start_seconds;
        // A rebased final window's chunk origin is quantized to the extractor
        // rate, so it can sit up to one extractor sample before the native
        // window start (e.g. 59.39093 s for a 59.391 s window at 48 kHz). Its
        // one-second cells are the window's own cells: snap them back onto the
        // window grid when the difference is within that quantization, so a
        // sub-sample artefact never pushes a cell outside its window.
        let quantization = 1.0 / f64::from(FEATURE_RATE) + 1e-9;
        for cell in &mut accepted.supporting_cells {
            let relative = cell.time_bin_seconds - window.window_start_seconds;
            let snapped = relative.round();
            if (relative - snapped).abs() <= quantization {
                cell.time_bin_seconds = window.window_start_seconds + snapped;
            }
        }
        accepted.offset_seconds = window.offset_seconds;
        accepted.offset_bin = window
            .offset_seconds
            .map(|offset| (offset / OFFSET_BIN_SECONDS).round() as i32);
        accepted
    }

    /// A window provides support only with a complete search AND the accepted
    /// verdict; truncated/cancelled work NEVER matches.
    fn provides_support(&self) -> bool {
        self.complete && self.accepted == Some(true)
    }

    fn effective_rejection_reasons(&self) -> Vec<String> {
        let mut reasons = self.failure_reasons.clone();
        if !self.complete && !reasons.iter().any(|r| r == REASON_INCOMPLETE_EVIDENCE) {
            reasons.push(REASON_INCOMPLETE_EVIDENCE.to_string());
        }
        if reasons.is_empty() {
            reasons.push("not_accepted".to_string());
        }
        reasons
    }
}

/// Whole-input identities recorded on the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputIdentities {
    pub capture_clock_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_identity: Option<String>,
}

/// Full assembly request: the requested recording domain on one capture clock,
/// its epochs (in recording order), the recognized windows and declared
/// boundaries, and the movie clock used for raw PTS conversion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SegmentAssemblyRequest {
    pub capture_clock: CaptureClock,
    pub movie: SyncMovieClock,
    pub epochs: Vec<CaptureEpoch>,
    /// Requested recording domain (accounting covers exactly this range).
    pub domain: SampleRange,
    pub windows: Vec<AcceptedWindow>,
    /// Known pause/seek/rewind/interruption evidence; transition cells stay
    /// unknown and prevent interpolation.
    pub boundaries: Vec<SyncBoundary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_identity: Option<String>,
}

// ---------------------------------------------------------------------------
// Report DTOs (versioned camelCase).

/// Evidence-only accounting summary of the requested domain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportCompleteness {
    pub requested_start_sample: u64,
    pub requested_end_sample: u64,
    pub requested_seconds: f64,
    pub supported_seconds: f64,
    pub unknown_seconds: f64,
    /// Unknown seconds inside segment envelopes (recorded holes).
    pub envelope_unknown_seconds: f64,
    /// Supported spans plus unknown spans plus unknown ranges tile the
    /// requested domain exactly, without overlap. Always `true` on a returned
    /// report; asserted by construction and re-checkable by consumers.
    pub accounting_complete: bool,
    /// Every input window ran a complete search (no truncation/cancellation).
    pub search_complete: bool,
    pub accepted_window_count: usize,
    pub incomplete_window_count: usize,
}

/// One recognized window as reported: verdict, evidence and retention state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecognizedWindowReport {
    pub window_id: String,
    pub capture_clock_id: String,
    pub epoch_id: String,
    pub start_sample: u64,
    pub end_sample: u64,
    pub window_start_seconds: f64,
    pub duration_seconds: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset_bin: Option<i32>,
    pub complete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted: Option<bool>,
    pub failure_reasons: Vec<String>,
    pub supporting_cell_count: usize,
    /// `true` when the window actually contributed segment support.
    pub retained: bool,
}

/// One supported span of a segment: native half-open recording range that maps
/// to raw movie PTS through the segment offset. Evidence IDs are the retained
/// supporting-cell IDs (deduplicated).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SupportedSpan {
    pub start_sample: u64,
    pub end_sample: u64,
    pub evidence_ids: Vec<String>,
    /// Raw movie PTS bounds, rounded outwards to whole ticks from the declared
    /// origin and rational timebase (half-open span maps to half-open PTS).
    pub movie_start_pts: i64,
    pub movie_end_pts: i64,
}

/// An explicit unknown span inside a segment envelope (an internal unsupported
/// gap of at most two seconds, both sides agreeing). No support and NO movie
/// position: it is never automatically mapped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnknownSpan {
    pub start_sample: u64,
    pub end_sample: u64,
    pub reason: String,
}

/// One retained supporting cell (the accepted windows' offset plus the
/// one-second cells/anchor positions that actually support it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetainedSupportCell {
    /// One-second cell start within the epoch, in recording seconds.
    pub time_bin_seconds: f64,
    /// Native half-open cell range start on the capture clock.
    pub start_sample: u64,
    pub offset_seconds: f64,
    pub query_anchor_frames: Vec<u32>,
    pub evidence_ids: Vec<String>,
}

/// Deduplicated segment support counts: repeated votes from overlapping
/// windows never inflate them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SupportCounts {
    /// Unique absolute anchor frame identities across all member cells.
    pub independent_anchor_ids: u64,
    pub supporting_cells: u64,
    pub contributing_windows: u64,
}

/// One assembled segment: a single epoch-local envelope at one offset
/// (playback rate fixed 1.0), with supported spans and recorded unknown spans.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Segment {
    pub segment_id: String,
    pub capture_clock_id: String,
    pub epoch_id: String,
    /// Native half-open envelope on the capture clock.
    pub start_sample: u64,
    pub end_sample: u64,
    /// Median of all member cells; every member is within one 80 ms bin of it.
    pub offset_seconds: f64,
    pub playback_rate: f64,
    pub supported_spans: Vec<SupportedSpan>,
    /// Recorded holes inside the envelope; never mapped.
    pub unknown_spans: Vec<UnknownSpan>,
    pub retained_support: Vec<RetainedSupportCell>,
    /// Offset precision: worst member deviation from the segment offset.
    pub offset_uncertainty_seconds: f64,
    /// Boundary uncertainty (one-second evidence cells), reported separately
    /// from offset precision: a 40 ms offset error is never a 40 ms boundary
    /// estimate.
    pub boundary_uncertainty_seconds: f64,
    pub deduplicated_support: SupportCounts,
    pub acceptance_policy_id: String,
    /// Frozen-vocabulary reasons from non-accepted windows overlapping the
    /// envelope.
    pub rejection_reasons: Vec<String>,
}

/// An unknown range outside every segment envelope. Recording coordinates
/// only: an unknown NEVER receives a movie position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnknownRange {
    pub capture_clock_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub epoch_id: Option<String>,
    pub start_sample: u64,
    pub end_sample: u64,
    pub reason: String,
}

/// One competing offset hypothesis of a conflicting cell (retained as an
/// alternative; never chosen by continuity).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OffsetHypothesis {
    pub offset_seconds: f64,
    pub offset_bin: i32,
    pub window_ids: Vec<String>,
}

/// Conflicting-cell alternatives kept visible in the report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Alternative {
    pub capture_clock_id: String,
    pub epoch_id: String,
    /// One-second cell start within the epoch, in recording seconds.
    pub time_bin_seconds: f64,
    pub hypotheses: Vec<OffsetHypothesis>,
    pub resolution: String,
}

/// Versioned assembly report: status, identities, completeness, recognized
/// windows, segments, unknownRanges, alternatives and diagnostics. Supported
/// segments plus unknown ranges form a non-overlapping complete accounting of
/// the requested recording domain even when envelopes contain holes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SegmentReport {
    pub schema_version: u32,
    /// `"complete"` when every window search completed; `"incomplete"` when any
    /// window was truncated/cancelled (incomplete work is returned explicitly
    /// and never matched).
    pub status: String,
    pub assembler_id: String,
    pub recipe_identity: String,
    pub acceptance_policy_id: String,
    pub identities: InputIdentities,
    pub movie_clock: SyncMovieClock,
    pub capture_clock: CaptureClock,
    pub epochs: Vec<CaptureEpoch>,
    pub declared_boundaries: Vec<SyncBoundary>,
    pub completeness: ReportCompleteness,
    pub recognized_windows: Vec<RecognizedWindowReport>,
    pub segments: Vec<Segment>,
    pub unknown_ranges: Vec<UnknownRange>,
    pub alternatives: Vec<Alternative>,
    pub diagnostics: Vec<String>,
}

// ---------------------------------------------------------------------------
// Assembly helpers.

fn opaque(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// Map a generated session window evidence id (which embeds `:` separators)
/// onto the opaque identifier vocabulary the assembler enforces. Session
/// evidence ids are machine-generated `saw:<clock>:<epoch>:<start>-<end>`
/// strings, so the mapping is deterministic and collision-free for one clock.
fn opaque_window_id(evidence_id: &str) -> String {
    evidence_id
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-' {
                byte as char
            } else {
                '-'
            }
        })
        .collect()
}

/// Frozen offset convention: `movieElapsedSeconds =
/// recordingSecondsWithinEpoch + offsetSeconds`.
pub fn movie_elapsed_seconds(recording_seconds_within_epoch: f64, offset_seconds: f64) -> f64 {
    recording_seconds_within_epoch + offset_seconds
}

/// Raw movie PTS bounds of a recording span at fixed playback rate 1.0,
/// rounded outwards to whole movie ticks from the declared origin and rational
/// timebase.
fn movie_pts_bounds(
    movie: &SyncMovieClock,
    recording_seconds_within_epoch_start: f64,
    recording_seconds_within_epoch_end: f64,
    offset_seconds: f64,
) -> (i64, i64) {
    let ticks = movie.ticks_per_second();
    let start = movie.origin_pts as f64
        + movie_elapsed_seconds(recording_seconds_within_epoch_start, offset_seconds) * ticks;
    let end = movie.origin_pts as f64
        + movie_elapsed_seconds(recording_seconds_within_epoch_end, offset_seconds) * ticks;
    (start.floor() as i64, end.ceil() as i64)
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|left, right| left.total_cmp(right));
    let length = sorted.len();
    if length % 2 == 1 {
        sorted[length / 2]
    } else {
        (sorted[length / 2 - 1] + sorted[length / 2]) / 2.0
    }
}

/// Group acceptance: the candidate is compatible with every member (pairwise
/// within one 80 ms bin) AND every member of the extended set stays within
/// 80 ms of its median (no transitive drift chains).
fn group_accepts(members: &[f64], candidate: f64) -> bool {
    if !members
        .iter()
        .all(|member| (member - candidate).abs() <= OFFSET_TOLERANCE)
    {
        return false;
    }
    let mut extended = members.to_vec();
    extended.push(candidate);
    let representative = median(&extended);
    extended
        .iter()
        .all(|member| (member - representative).abs() <= OFFSET_TOLERANCE)
}

#[derive(Clone)]
struct CellRecord {
    /// Epoch-relative sample offset of the one-second cell start.
    cell_start: u64,
    /// Recording seconds within the epoch (report/evidence value).
    time_bin_seconds: f64,
    offset_seconds: f64,
    frames: BTreeSet<u32>,
    evidence_ids: BTreeSet<String>,
    window_ids: BTreeSet<String>,
}

struct Group {
    cells: Vec<CellRecord>,
    representative: f64,
}

struct Chain {
    groups: Vec<Group>,
    /// Unsupported gaps bridged inside the envelope as absolute half-open
    /// sample ranges on the capture clock.
    gaps: Vec<(u64, u64)>,
}

impl Chain {
    fn all_offsets(&self) -> Vec<f64> {
        self.groups
            .iter()
            .flat_map(|group| group.cells.iter().map(|cell| cell.offset_seconds))
            .collect()
    }
    fn representative(&self) -> f64 {
        median(&self.all_offsets())
    }
}

/// Group the consecutive-run cells of one epoch greedily in recording order.
fn group_run(cells: Vec<CellRecord>) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut members: Vec<f64> = Vec::new();
    let mut current: Vec<CellRecord> = Vec::new();
    for cell in cells {
        if current.is_empty() || group_accepts(&members, cell.offset_seconds) {
            members.push(cell.offset_seconds);
            current.push(cell);
        } else {
            let representative = median(&members);
            groups.push(Group {
                cells: std::mem::take(&mut current),
                representative,
            });
            members = vec![cell.offset_seconds];
            current.push(cell);
        }
    }
    if !current.is_empty() {
        let representative = median(&members);
        groups.push(Group {
            cells: current,
            representative,
        });
    }
    groups
}

fn cell_sample_range(epoch: &CaptureEpoch, sample_rate: u32, cell_start: u64) -> (u64, u64) {
    let start = epoch.start_sample + cell_start;
    (start, start + u64::from(sample_rate))
}

fn validate_request(request: &SegmentAssemblyRequest) -> Result<()> {
    let clock = &request.capture_clock;
    ensure!(
        opaque(&clock.clock_id) && (8000..=192000).contains(&clock.sample_rate),
        "invalid input: segment assembly capture clock"
    );
    ensure!(
        request.movie.time_base_numerator > 0
            && request.movie.time_base_denominator > 0
            && request.movie.duration_pts > 0,
        "invalid input: segment assembly movie clock"
    );
    let domain = request.domain;
    ensure!(
        domain.start_sample < domain.end_sample
            && domain.start_sample >= clock.first_sample
            && domain.end_sample <= clock.last_sample.max(clock.first_sample),
        "invalid input: segment assembly domain outside capture clock"
    );
    let mut previous_end: Option<u64> = None;
    for epoch in &request.epochs {
        ensure!(
            opaque(&epoch.epoch_id)
                && epoch.start_sample < epoch.end_sample
                && epoch.start_sample >= clock.first_sample
                && epoch.end_sample <= clock.last_sample.max(clock.first_sample),
            "invalid input: capture epoch outside capture clock"
        );
        if let Some(end) = previous_end {
            ensure!(
                epoch.start_sample >= end,
                "invalid input: capture epochs must not overlap"
            );
        }
        previous_end = Some(epoch.end_sample);
    }
    let mut window_ids = BTreeSet::new();
    for window in &request.windows {
        ensure!(
            opaque(&window.window_id) && window_ids.insert(window.window_id.clone()),
            "invalid input: window evidence identities must be unique"
        );
        ensure!(
            window.capture_clock_id == clock.clock_id,
            "invalid input: window capture clock mismatch"
        );
        let epoch = request
            .epochs
            .iter()
            .find(|epoch| epoch.epoch_id == window.epoch_id)
            .ok_or_else(|| anyhow::anyhow!("invalid input: unknown capture epoch"))?;
        let rate = u64::from(clock.sample_rate);
        let within_epoch = window.start_sample.checked_sub(epoch.start_sample);
        ensure!(
            window.window_start_seconds.is_finite()
                && window.window_start_seconds >= 0.0
                && within_epoch == Some((window.window_start_seconds * rate as f64).round() as u64)
                && window.end_sample
                    == window.start_sample + u64::from(window.duration_seconds) * rate
                && window.end_sample <= epoch.end_sample
                && window.start_sample >= domain.start_sample
                && window.end_sample <= domain.end_sample,
            "invalid input: window range must match its epoch schedule inside the domain"
        );
        match (window.offset_seconds, window.offset_bin) {
            (Some(offset), Some(bin)) => ensure!(
                offset.is_finite()
                    && (offset - f64::from(bin) * OFFSET_BIN_SECONDS).abs()
                        <= 0.5 * OFFSET_BIN_SECONDS + 1e-6,
                "invalid input: window offset must match its offset bin"
            ),
            (None, None) => {}
            _ => anyhow::bail!("invalid input: window offset and offset bin must both be set"),
        }
        for cell in &window.supporting_cells {
            ensure!(
                opaque(&cell.evidence_id),
                "invalid input: cell evidence identities must be opaque"
            );
            ensure!(
                cell.time_bin_seconds.is_finite() && cell.time_bin_seconds >= 0.0,
                "invalid input: supporting cell time must be finite and epoch-relative"
            );
            let cell_start =
                epoch.start_sample + (cell.time_bin_seconds * rate as f64).round() as u64;
            ensure!(
                cell_start >= window.start_sample && cell_start + rate <= window.end_sample,
                "invalid input: supporting cell outside its window"
            );
        }
    }
    for boundary in &request.boundaries {
        ensure!(
            opaque(&boundary.evidence_id) && boundary.clock_id == clock.clock_id,
            "invalid input: discontinuity evidence"
        );
    }
    Ok(())
}

struct Hypothesis {
    offset_bin: i32,
    offset_seconds: f64,
}

#[derive(Default)]
struct CellBuilder {
    /// Recording seconds within the epoch (first contributing cell value).
    time_bin_seconds: f64,
    hypotheses: Vec<Hypothesis>,
    frames: BTreeSet<u32>,
    evidence_ids: BTreeSet<String>,
    window_ids: BTreeSet<String>,
}

/// Assemble accepted recognition windows into conservative segments and
/// explicit unknown ranges. Deterministic pure computation: identical input
/// yields an identical report. Incomplete windows never match.
pub fn assemble(request: &SegmentAssemblyRequest) -> Result<SegmentReport> {
    validate_request(request)?;
    let clock = &request.capture_clock;
    let rate = clock.sample_rate;
    let policy_id = PooledRecognitionPolicy::frozen().policy_id.clone();
    let mut diagnostics = vec![
        "playback_rate_fixed_1.0".to_string(),
        "no_drift_fitting".to_string(),
        "no_global_offset_extrapolation".to_string(),
    ];

    // Retained support comes only from complete accepted windows; every other
    // window is evidence without support.
    let effective: Vec<&AcceptedWindow> = request
        .windows
        .iter()
        .filter(|window| window.provides_support())
        .collect();
    let non_effective_ranges: Vec<(u64, u64)> = request
        .windows
        .iter()
        .filter(|window| !window.provides_support())
        .map(|window| (window.start_sample, window.end_sample))
        .collect();

    // Per-epoch cell reconciliation. Cells are keyed by their epoch-relative
    // sample offset (exact native coordinates; one-second evidence cells).
    let mut epoch_cells: BTreeMap<String, BTreeMap<u64, CellBuilder>> = BTreeMap::new();
    for window in &effective {
        let Some((offset_seconds, offset_bin)) = window.offset_seconds.zip(window.offset_bin)
        else {
            continue;
        };
        let cells = epoch_cells.entry(window.epoch_id.clone()).or_default();
        for cell in &window.supporting_cells {
            let cell_start = (cell.time_bin_seconds * f64::from(rate)).round() as u64;
            let builder = cells.entry(cell_start).or_default();
            builder.time_bin_seconds = cell.time_bin_seconds;
            if !builder.hypotheses.iter().any(|hypothesis| {
                hypothesis.offset_bin == offset_bin && hypothesis.offset_seconds == offset_seconds
            }) {
                builder.hypotheses.push(Hypothesis {
                    offset_bin,
                    offset_seconds,
                });
            }
            builder
                .frames
                .extend(cell.query_anchor_frames.iter().copied());
            builder.evidence_ids.insert(cell.evidence_id.clone());
            builder.window_ids.insert(window.window_id.clone());
        }
    }

    // Split supported vs conflicting cells per epoch; incompatible accepted
    // hypotheses make the cell conflicting/unknown and are kept as
    // alternatives, never continuity-picked.
    let mut alternatives: Vec<Alternative> = Vec::new();
    let mut supported_by_epoch: BTreeMap<String, BTreeMap<u64, CellRecord>> = BTreeMap::new();
    let mut conflicting_by_epoch: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for (epoch_id, cells) in epoch_cells {
        let epoch = request
            .epochs
            .iter()
            .find(|epoch| epoch.epoch_id == epoch_id)
            .expect("validated epoch");
        let mut supported = BTreeMap::new();
        let mut conflicting = Vec::new();
        for (cell_start, builder) in cells {
            // A cell containing a declared pause/seek/rewind/interruption
            // boundary is a transition cell: it stays unknown even when its
            // hypotheses agree.
            let (cell_start_sample, cell_end_sample) = cell_sample_range(epoch, rate, cell_start);
            let boundary_in_cell = request.boundaries.iter().any(|boundary| {
                boundary.sample_index >= cell_start_sample
                    && boundary.sample_index < cell_end_sample
            });
            if boundary_in_cell {
                continue;
            }
            // Frozen compatibility rule in offset space: hypotheses are
            // compatible iff every pair differs by at most one 80 ms bin
            // (exact on the bin lattice; correct for a rebased final window
            // whose offset is off the chunk lattice).
            let mut hypothesis_offsets: Vec<f64> = builder
                .hypotheses
                .iter()
                .map(|hypothesis| hypothesis.offset_seconds)
                .collect();
            hypothesis_offsets.sort_by(|left, right| left.total_cmp(right));
            let spread = hypothesis_offsets
                .last()
                .expect("cell hypotheses are non-empty")
                - hypothesis_offsets
                    .first()
                    .expect("cell hypotheses are non-empty");
            if spread > OFFSET_TOLERANCE {
                let mut merged: Vec<OffsetHypothesis> = Vec::new();
                for window in &effective {
                    if window.epoch_id != epoch_id {
                        continue;
                    }
                    let (Some(offset_seconds), Some(offset_bin)) =
                        (window.offset_seconds, window.offset_bin)
                    else {
                        continue;
                    };
                    for cell in &window.supporting_cells {
                        if (cell.time_bin_seconds * f64::from(rate)).round() as u64 != cell_start {
                            continue;
                        }
                        match merged.iter_mut().find(|hypothesis| {
                            hypothesis.offset_bin == offset_bin
                                && hypothesis.offset_seconds == offset_seconds
                        }) {
                            Some(hypothesis) => {
                                hypothesis.window_ids.push(window.window_id.clone())
                            }
                            None => merged.push(OffsetHypothesis {
                                offset_seconds,
                                offset_bin,
                                window_ids: vec![window.window_id.clone()],
                            }),
                        }
                    }
                }
                let mut hypotheses: Vec<OffsetHypothesis> = merged;
                for hypothesis in &mut hypotheses {
                    hypothesis.window_ids.sort();
                    hypothesis.window_ids.dedup();
                }
                hypotheses.sort_by(|left, right| {
                    left.offset_bin
                        .cmp(&right.offset_bin)
                        .then(left.offset_seconds.total_cmp(&right.offset_seconds))
                });
                alternatives.push(Alternative {
                    capture_clock_id: clock.clock_id.clone(),
                    epoch_id: epoch_id.clone(),
                    time_bin_seconds: builder.time_bin_seconds,
                    hypotheses,
                    resolution: RESOLUTION_CONFLICTING_UNKNOWN.to_string(),
                });
                conflicting.push(cell_start);
                continue;
            }
            let mut offsets: Vec<f64> = builder
                .hypotheses
                .iter()
                .map(|hypothesis| hypothesis.offset_seconds)
                .collect();
            offsets.sort_by(|left, right| left.total_cmp(right));
            supported.insert(
                cell_start,
                CellRecord {
                    cell_start,
                    time_bin_seconds: builder.time_bin_seconds,
                    offset_seconds: median(&offsets),
                    frames: builder.frames,
                    evidence_ids: builder.evidence_ids,
                    window_ids: builder.window_ids,
                },
            );
        }
        conflicting_by_epoch.insert(epoch_id.clone(), conflicting);
        supported_by_epoch.insert(epoch_id, supported);
    }

    // Group and bridge per epoch. Bridging an unsupported gap of at most two
    // seconds is allowed only between agreeing groups without contradictory
    // evidence or a declared boundary in the gap.
    let mut chains: Vec<(String, Chain)> = Vec::new();
    for (epoch_id, supported) in &supported_by_epoch {
        let epoch = request
            .epochs
            .iter()
            .find(|epoch| &epoch.epoch_id == epoch_id)
            .expect("validated epoch");
        let boundary_samples: Vec<u64> = request
            .boundaries
            .iter()
            .filter(|boundary| {
                boundary.sample_index >= epoch.start_sample
                    && boundary.sample_index < epoch.end_sample
            })
            .map(|boundary| boundary.sample_index)
            .collect();
        let conflicting_cells = conflicting_by_epoch.get(epoch_id);
        let cell_width = u64::from(rate);
        let mut runs: Vec<Vec<CellRecord>> = Vec::new();
        let mut run: Vec<CellRecord> = Vec::new();
        for cell_start in supported.keys() {
            let cell = supported.get(cell_start).expect("supported cell").clone();
            match run.last() {
                Some(previous) if previous.cell_start + cell_width == cell.cell_start => {
                    run.push(cell)
                }
                Some(_) => {
                    runs.push(std::mem::take(&mut run));
                    run.push(cell);
                }
                None => run.push(cell),
            }
        }
        if !run.is_empty() {
            runs.push(run);
        }
        let mut flat_groups: Vec<Group> = runs.into_iter().flat_map(group_run).collect();
        flat_groups.sort_by_key(|group| group.cells[0].cell_start);
        while !flat_groups.is_empty() {
            let first = flat_groups.remove(0);
            let mut chain = Chain {
                groups: vec![first],
                gaps: Vec::new(),
            };
            while !flat_groups.is_empty() {
                let last_end = chain
                    .groups
                    .last()
                    .expect("chain group")
                    .cells
                    .last()
                    .expect("group cell")
                    .cell_start
                    + cell_width;
                let next_start = flat_groups[0].cells[0].cell_start;
                let gap_samples = next_start.saturating_sub(last_end);
                if gap_samples == 0
                    || gap_samples > u64::from(MAX_INTERNAL_GAP_SECONDS) * cell_width
                {
                    break;
                }
                let gap_start = epoch.start_sample + last_end;
                let gap_end = epoch.start_sample + next_start;
                let conflicting_in_gap = conflicting_cells.is_some_and(|cells| {
                    cells
                        .iter()
                        .any(|cell_start| *cell_start >= last_end && *cell_start < next_start)
                });
                let boundary_in_gap = boundary_samples
                    .iter()
                    .any(|sample| *sample >= gap_start && *sample < gap_end);
                let weak_in_gap = non_effective_ranges
                    .iter()
                    .any(|(start, end)| *start < gap_end && gap_start < *end);
                let chain_representative = chain.representative();
                let next_representative = flat_groups[0].representative;
                let agree = (chain_representative - next_representative).abs() <= OFFSET_TOLERANCE;
                let mut combined = chain.all_offsets();
                combined.extend(flat_groups[0].cells.iter().map(|cell| cell.offset_seconds));
                let combined_representative = median(&combined);
                let no_drift_chain = combined
                    .iter()
                    .all(|offset| (offset - combined_representative).abs() <= OFFSET_TOLERANCE);
                if agree
                    && no_drift_chain
                    && !conflicting_in_gap
                    && !boundary_in_gap
                    && !weak_in_gap
                {
                    chain.gaps.push((gap_start, gap_end));
                    chain.groups.push(flat_groups.remove(0));
                } else {
                    break;
                }
            }
            chains.push((epoch_id.clone(), chain));
        }
    }
    chains.sort_by_key(|(epoch_id, chain)| {
        let order = request
            .epochs
            .iter()
            .position(|epoch| &epoch.epoch_id == epoch_id)
            .expect("validated epoch");
        (order, chain.groups[0].cells[0].cell_start)
    });

    // Materialize segments.
    let mut segments: Vec<Segment> = Vec::new();
    for (ordinal, (epoch_id, chain)) in chains.iter().enumerate() {
        let epoch = request
            .epochs
            .iter()
            .find(|epoch| &epoch.epoch_id == epoch_id)
            .expect("validated epoch");
        let cell_width = u64::from(rate);
        let offsets = chain.all_offsets();
        let segment_offset = median(&offsets);
        let offset_uncertainty = offsets
            .iter()
            .map(|offset| (offset - segment_offset).abs())
            .fold(0.0f64, f64::max);
        let mut supported_spans: Vec<SupportedSpan> = Vec::new();
        let mut retained_support: Vec<RetainedSupportCell> = Vec::new();
        let mut frames: BTreeSet<u32> = BTreeSet::new();
        let mut window_ids: BTreeSet<String> = BTreeSet::new();
        for group in &chain.groups {
            let first = &group.cells[0];
            let last = group.cells.last().expect("group cell");
            let span_start = epoch.start_sample + first.cell_start;
            let span_end = epoch.start_sample + last.cell_start + cell_width;
            let mut evidence_ids: BTreeSet<String> = BTreeSet::new();
            for cell in &group.cells {
                evidence_ids.extend(cell.evidence_ids.iter().cloned());
                frames.extend(cell.frames.iter().copied());
                window_ids.extend(cell.window_ids.iter().cloned());
                retained_support.push(RetainedSupportCell {
                    time_bin_seconds: cell.time_bin_seconds,
                    start_sample: epoch.start_sample + cell.cell_start,
                    offset_seconds: cell.offset_seconds,
                    query_anchor_frames: cell.frames.iter().copied().collect(),
                    evidence_ids: cell.evidence_ids.iter().cloned().collect(),
                });
            }
            let (movie_start_pts, movie_end_pts) = movie_pts_bounds(
                &request.movie,
                (span_start - epoch.start_sample) as f64 / f64::from(rate),
                (span_end - epoch.start_sample) as f64 / f64::from(rate),
                segment_offset,
            );
            supported_spans.push(SupportedSpan {
                start_sample: span_start,
                end_sample: span_end,
                evidence_ids: evidence_ids.into_iter().collect(),
                movie_start_pts,
                movie_end_pts,
            });
        }
        let unknown_spans: Vec<UnknownSpan> = chain
            .gaps
            .iter()
            .map(|(gap_start, gap_end)| UnknownSpan {
                start_sample: *gap_start,
                end_sample: *gap_end,
                reason: REASON_UNSUPPORTED_GAP.to_string(),
            })
            .collect();
        let envelope_start = supported_spans
            .first()
            .map(|span| span.start_sample)
            .expect("segment has supported spans");
        let envelope_end = supported_spans
            .last()
            .map(|span| span.end_sample)
            .expect("segment has supported spans");
        let mut rejection_reasons: BTreeSet<String> = BTreeSet::new();
        for window in &request.windows {
            let overlaps = window.start_sample < envelope_end && envelope_start < window.end_sample;
            if overlaps && !window.provides_support() {
                rejection_reasons.extend(window.effective_rejection_reasons());
            }
        }
        diagnostics.push(format!(
            "segment:{}:{}s-{}s",
            ordinal,
            (envelope_start - epoch.start_sample) as f64 / f64::from(rate),
            (envelope_end - epoch.start_sample) as f64 / f64::from(rate),
        ));
        for (gap_start, gap_end) in &chain.gaps {
            diagnostics.push(format!(
                "recorded_unknown_span:{}:{}-{}",
                ordinal, gap_start, gap_end
            ));
        }
        segments.push(Segment {
            segment_id: format!("seg{}", ordinal),
            capture_clock_id: clock.clock_id.clone(),
            epoch_id: epoch_id.clone(),
            start_sample: envelope_start,
            end_sample: envelope_end,
            offset_seconds: segment_offset,
            playback_rate: PLAYBACK_RATE,
            supported_spans,
            unknown_spans,
            retained_support,
            offset_uncertainty_seconds: offset_uncertainty,
            boundary_uncertainty_seconds: f64::from(TIME_BIN_SECONDS),
            deduplicated_support: SupportCounts {
                independent_anchor_ids: frames.len() as u64,
                supporting_cells: chain
                    .groups
                    .iter()
                    .map(|group| group.cells.len() as u64)
                    .sum(),
                contributing_windows: window_ids.len() as u64,
            },
            acceptance_policy_id: policy_id.clone(),
            rejection_reasons: rejection_reasons.into_iter().collect(),
        });
    }

    // Unknown ranges: everything in the domain outside every envelope, split at
    // epoch boundaries, with a deterministic reason priority.
    let mut unknown_ranges: Vec<UnknownRange> = Vec::new();
    let mut push_unknown = |start: u64, end: u64, segments: &[Segment]| {
        if start >= end {
            return;
        }
        for (piece_start, piece_end, epoch_id) in split_at_epoch_boundaries(request, start, end) {
            let reason = unknown_reason(
                request,
                &conflicting_by_epoch,
                &non_effective_ranges,
                segments,
                &epoch_id,
                piece_start,
                piece_end,
            );
            unknown_ranges.push(UnknownRange {
                capture_clock_id: clock.clock_id.clone(),
                epoch_id,
                start_sample: piece_start,
                end_sample: piece_end,
                reason,
            });
        }
    };
    let mut cursor = domain_start(request);
    for segment in &segments {
        push_unknown(cursor, segment.start_sample, &segments);
        cursor = cursor.max(segment.end_sample);
    }
    push_unknown(cursor, domain_end(request), &segments);

    // Accounting invariant: supported spans + envelope holes + unknown ranges
    // tile the requested domain exactly, without overlap.
    let mut pieces: Vec<(u64, u64)> = Vec::new();
    for segment in &segments {
        for span in &segment.supported_spans {
            pieces.push((span.start_sample, span.end_sample));
        }
        for span in &segment.unknown_spans {
            pieces.push((span.start_sample, span.end_sample));
        }
    }
    for range in &unknown_ranges {
        pieces.push((range.start_sample, range.end_sample));
    }
    pieces.sort();
    let mut expected = domain_start(request);
    for (start, end) in &pieces {
        ensure!(
            *start == expected && *end > *start,
            "internal: segment accounting must be non-overlapping and complete"
        );
        expected = *end;
    }
    ensure!(
        expected == domain_end(request),
        "internal: segment accounting must cover the requested domain"
    );

    // Fold from +0.0: an empty float `sum()` is -0.0, which would serialize
    // as `-0.0` seconds in reports without any support or holes.
    let supported_seconds: f64 = segments
        .iter()
        .flat_map(|segment| segment.supported_spans.iter())
        .map(|span| (span.end_sample - span.start_sample) as f64 / f64::from(rate))
        .fold(0.0, |total, seconds| total + seconds);
    let envelope_unknown_seconds: f64 = segments
        .iter()
        .flat_map(|segment| segment.unknown_spans.iter())
        .map(|span| (span.end_sample - span.start_sample) as f64 / f64::from(rate))
        .fold(0.0, |total, seconds| total + seconds);
    let unknown_seconds: f64 = envelope_unknown_seconds
        + unknown_ranges
            .iter()
            .map(|range| (range.end_sample - range.start_sample) as f64 / f64::from(rate))
            .fold(0.0, |total, seconds| total + seconds);
    let search_complete = request.windows.iter().all(|window| window.complete);
    let incomplete_window_count = request
        .windows
        .iter()
        .filter(|window| !window.complete)
        .count();

    let recognized_windows: Vec<RecognizedWindowReport> = request
        .windows
        .iter()
        .map(|window| {
            let mut failure_reasons = window.failure_reasons.clone();
            if !window.complete
                && !failure_reasons
                    .iter()
                    .any(|r| r == REASON_INCOMPLETE_EVIDENCE)
            {
                failure_reasons.push(REASON_INCOMPLETE_EVIDENCE.to_string());
            }
            RecognizedWindowReport {
                window_id: window.window_id.clone(),
                capture_clock_id: window.capture_clock_id.clone(),
                epoch_id: window.epoch_id.clone(),
                start_sample: window.start_sample,
                end_sample: window.end_sample,
                window_start_seconds: window.window_start_seconds,
                duration_seconds: window.duration_seconds,
                offset_seconds: window.offset_seconds,
                offset_bin: window.offset_bin,
                complete: window.complete,
                accepted: window.accepted,
                failure_reasons,
                supporting_cell_count: window.supporting_cells.len(),
                retained: effective
                    .iter()
                    .any(|other| other.window_id == window.window_id),
            }
        })
        .collect();

    Ok(SegmentReport {
        schema_version: SEGMENT_REPORT_SCHEMA_VERSION,
        status: if search_complete {
            "complete".to_string()
        } else {
            "incomplete".to_string()
        },
        assembler_id: ASSEMBLER_ID.to_string(),
        recipe_identity: RECIPE_ID.to_string(),
        acceptance_policy_id: policy_id,
        identities: InputIdentities {
            capture_clock_id: clock.clock_id.clone(),
            recording_identity: request.recording_identity.clone(),
            index_identity: request.index_identity.clone(),
        },
        movie_clock: request.movie.clone(),
        capture_clock: request.capture_clock.clone(),
        epochs: request.epochs.clone(),
        declared_boundaries: request.boundaries.clone(),
        completeness: ReportCompleteness {
            requested_start_sample: domain_start(request),
            requested_end_sample: domain_end(request),
            requested_seconds: (domain_end(request) - domain_start(request)) as f64
                / f64::from(rate),
            supported_seconds,
            unknown_seconds,
            envelope_unknown_seconds,
            accounting_complete: true,
            search_complete,
            accepted_window_count: effective.len(),
            incomplete_window_count,
        },
        recognized_windows,
        segments,
        unknown_ranges,
        alternatives,
        diagnostics,
    })
}

fn domain_start(request: &SegmentAssemblyRequest) -> u64 {
    request.domain.start_sample
}

fn domain_end(request: &SegmentAssemblyRequest) -> u64 {
    request.domain.end_sample
}

/// Split an unknown interval at capture-epoch boundaries; pieces carry their
/// epoch (or `None` outside every epoch).
fn split_at_epoch_boundaries(
    request: &SegmentAssemblyRequest,
    start: u64,
    end: u64,
) -> Vec<(u64, u64, Option<String>)> {
    let mut pieces = Vec::new();
    let mut cursor = start;
    for epoch in &request.epochs {
        if epoch.end_sample <= cursor || epoch.start_sample >= end {
            continue;
        }
        if cursor < epoch.start_sample {
            pieces.push((cursor, epoch.start_sample.min(end), None));
        }
        let piece_end = epoch.end_sample.min(end);
        let piece_start = cursor.max(epoch.start_sample);
        if piece_start < piece_end {
            pieces.push((piece_start, piece_end, Some(epoch.epoch_id.clone())));
        }
        cursor = cursor.max(piece_end);
    }
    if cursor < end {
        pieces.push((cursor, end, None));
    }
    pieces
}

/// Deterministic unknown-range reason priority: conflicting cells, declared
/// boundaries, incompatible transitions, weak/negative evidence, short epochs,
/// outside epochs, plain absence of accepted support.
fn unknown_reason(
    request: &SegmentAssemblyRequest,
    conflicting_by_epoch: &BTreeMap<String, Vec<u64>>,
    non_effective_ranges: &[(u64, u64)],
    segments: &[Segment],
    epoch_id: &Option<String>,
    start: u64,
    end: u64,
) -> String {
    let Some(epoch_id) = epoch_id else {
        return REASON_OUTSIDE_EPOCH.to_string();
    };
    let epoch = request
        .epochs
        .iter()
        .find(|epoch| &epoch.epoch_id == epoch_id)
        .expect("validated epoch");
    let sample_rate = request.capture_clock.sample_rate;
    let conflicting = conflicting_by_epoch.get(epoch_id).is_some_and(|cells| {
        cells.iter().any(|cell_start| {
            let (cell_start_sample, cell_end_sample) =
                cell_sample_range(epoch, sample_rate, *cell_start);
            cell_start_sample < end && start < cell_end_sample
        })
    });
    if conflicting {
        return REASON_CONFLICTING_OFFSETS.to_string();
    }
    if request
        .boundaries
        .iter()
        .any(|boundary| boundary.sample_index >= start && boundary.sample_index < end)
    {
        return REASON_DECLARED_BOUNDARY.to_string();
    }
    let left = segments
        .iter()
        .rev()
        .find(|segment| segment.end_sample <= start);
    let right = segments.iter().find(|segment| segment.start_sample >= end);
    if let (Some(left), Some(right)) = (left, right)
        && (left.offset_seconds - right.offset_seconds).abs() > OFFSET_TOLERANCE
    {
        return REASON_INCOMPATIBLE_TRANSITION.to_string();
    }
    if non_effective_ranges
        .iter()
        .any(|(window_start, window_end)| *window_start < end && start < *window_end)
    {
        return REASON_WEAK_OR_NEGATIVE_EVIDENCE.to_string();
    }
    if epoch.start_sample <= start
        && end <= epoch.end_sample
        && epoch.end_sample - epoch.start_sample
            < u64::from(TIME_BIN_SECONDS) * u64::from(sample_rate) * 10
    {
        return REASON_SHORT_EPOCH.to_string();
    }
    REASON_NO_ACCEPTED_SUPPORT.to_string()
}

// ---------------------------------------------------------------------------
// Full-recording report entrypoint: one core-owned pipeline from the recording
// file to the bounded session run and the conservative segment report.

/// Full-recording report request: the explicit pooled matcher selection plus
/// the recording's capture clock, its continuous capture epochs and the
/// requested recording domain the report accounting must cover.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportRequest {
    /// Explicit opt-in matcher selection; must be `source-audio-pooled-mvp-v1`.
    pub recipe_identity: String,
    pub capture_clock: CaptureClock,
    pub epochs: Vec<CaptureEpoch>,
    pub domain: SampleRange,
    /// Known pause/seek/rewind/interruption evidence; transition cells stay
    /// unknown and prevent interpolation.
    #[serde(default)]
    pub boundaries: Vec<SyncBoundary>,
    /// Optional pin: must match the index's movie identity when present.
    #[serde(default)]
    pub movie_digest: Option<String>,
    /// Optional pin: must match the recording file's BLAKE3 when present.
    #[serde(default)]
    pub recording_digest: Option<String>,
    /// Diagnostic returned mode rows per window (1..=64; decisions never
    /// depend on it).
    #[serde(default)]
    pub pooled_mode_limit: Option<u8>,
}

/// The full-recording report outcome: the bounded session run (window work,
/// resume/reuse accounting and progress totals) plus the conservative
/// [`SegmentReport`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportOutcome {
    pub session: SessionResult,
    pub report: SegmentReport,
}

/// Run the frozen pooled session schedule over a full recording and assemble
/// the conservative segment report: the single core-owned source-audio report
/// pipeline. Validates the request (matcher selection is enforced by the
/// session run; movie/recording digest pins, clock/epoch/domain shapes),
/// verifies the recording identity around decoding (a changed recording can
/// never be reported), executes the bounded resumable session through
/// [`crate::source_audio_sessions::run_session`] and assembles the
/// [`SegmentReport`] from the session windows. Callers only read files, invoke
/// this function and format the JSON; incomplete work is returned explicitly
/// and no hole, conflict or unknown ever receives a movie position.
pub fn report(
    index: &Index,
    recording: &Path,
    derived_data_dir: &Path,
    request: &ReportRequest,
    on_progress: &mut dyn FnMut(SessionProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<ReportOutcome> {
    if let Some(expected) = request.movie_digest.as_deref() {
        ensure!(
            expected == index.movie.movie_digest,
            "invalid input: source-audio index movie identity mismatch"
        );
    }
    // Structural request validation before any media work.
    ensure!(
        !request.epochs.is_empty()
            && request.domain.start_sample < request.domain.end_sample
            && request.domain.start_sample >= request.capture_clock.first_sample
            && request.domain.end_sample <= request.capture_clock.last_sample,
        "invalid input: source-audio report domain must lie inside the capture clock"
    );
    let mut previous_end: Option<u64> = None;
    for epoch in &request.epochs {
        ensure!(
            epoch.start_sample < epoch.end_sample
                && epoch.start_sample >= request.domain.start_sample
                && epoch.end_sample <= request.domain.end_sample
                && previous_end.is_none_or(|end| epoch.start_sample >= end),
            "invalid input: source-audio report epochs must be ordered and inside the requested domain"
        );
        previous_end = Some(epoch.end_sample);
    }
    let before = crate::full_file_blake3(recording, cancel)?;
    if let Some(expected) = request.recording_digest.as_deref() {
        ensure!(
            expected == before.hex_digest,
            "invalid input: recording digest mismatch"
        );
    }
    // Native sample coordinates are interpreted on the declared capture
    // clock; a recording whose first audio stream runs at another rate would
    // silently rescale every position, so it is refused before decoding.
    let native_rate = probe_recording_sample_rate(recording, cancel)?;
    ensure!(
        native_rate == request.capture_clock.sample_rate,
        "invalid input: recording sample rate {native_rate} does not match the declared capture clock rate {}",
        request.capture_clock.sample_rate
    );
    let pcm = decode_wav(recording, cancel)?;
    let after = crate::full_file_blake3(recording, cancel)?;
    ensure!(
        before.hex_digest == after.hex_digest && before.byte_count == after.byte_count,
        "input_changed"
    );
    let session = crate::source_audio_sessions::run_session(
        index,
        &SessionRunRequest {
            identity: SessionIdentity {
                recording_digest: before.hex_digest.clone(),
                index_identity: index.index_digest.clone(),
                recipe_identity: request.recipe_identity.clone(),
                policy_identity: PooledRecognitionPolicy::frozen().policy_id.clone(),
                scheduling_identity: SchedulingIdentity {
                    window_seconds: SCHEDULE_WINDOW_SECONDS,
                    step_seconds: SCHEDULE_STEP_SECONDS,
                    query_block_seconds: QUERY_BLOCK_SECONDS,
                    epochs: request.epochs.clone(),
                },
                capture_clock: request.capture_clock.clone(),
            },
            pcm: &pcm,
            pcm_origin_sample: request.capture_clock.first_sample,
            derived_data_dir: derived_data_dir.to_path_buf(),
            search_policy: None,
            mode_limit: request.pooled_mode_limit,
        },
        on_progress,
        cancel,
    )?;
    let report = assemble(&SegmentAssemblyRequest {
        capture_clock: request.capture_clock.clone(),
        movie: index.movie.clone(),
        epochs: request.epochs.clone(),
        domain: request.domain,
        windows: session
            .windows
            .iter()
            .map(AcceptedWindow::from_session_window)
            .collect(),
        boundaries: request.boundaries.clone(),
        recording_identity: Some(before.hex_digest),
        index_identity: Some(index.index_digest.clone()),
    })?;
    Ok(ReportOutcome { session, report })
}

/// Native sample rate of the recording's first audio stream (local file
/// protocol only, bounded and cancellable like the capture decode).
fn probe_recording_sample_rate(recording: &Path, cancel: &dyn Fn() -> bool) -> Result<u32> {
    let mut command = crate::tool_command("ffprobe");
    command
        .args([
            "-v",
            "error",
            "-protocol_whitelist",
            "file,pipe",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=sample_rate",
            "-of",
            "csv=p=0",
        ])
        .arg(recording);
    let output = crate::stall::output_with_timeout_cancellation_and_limit(
        &mut command,
        "source-audio capture probe",
        std::time::Duration::from_secs(60),
        &|| cancel(),
        4096,
    )?;
    ensure!(
        output.status.success(),
        "media tool failed: source-audio capture probe"
    );
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .map_err(|_| anyhow::anyhow!("media tool failed: source-audio capture has no audio stream"))
}

// ---------------------------------------------------------------------------
// tv_review_sync adapter (read-only).

/// Query-level mapping outcome. Unknown, conflicting and gap-crossing queries
/// keep their recording timestamps and NO automatic movie location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MappedQueryState {
    /// Every sub-range of the query mapped through the existing mapper.
    Mapped,
    /// The mapper returned less coverage than the segment envelope; the
    /// unmapped sub-ranges are preserved, never backfilled from the offset.
    PartiallyMapped,
    /// No movie location: the query is unsupported.
    Unknown,
    /// No movie location: the query sits on conflicting evidence.
    Conflicting,
    /// No movie location: the query crosses an unknown span, an unknown range
    /// or a segment/epoch boundary.
    GapCrossing,
}

/// Read-only adapter request: one recording-time query range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryMappingRequest {
    pub session_id: String,
    pub query: SyncQuery,
    pub policy: SyncPolicy,
}

/// One mapper slice passthrough: recording range, mapper state, retained
/// candidates (raw movie PTS estimates) and mapper diagnostics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappedSlice {
    pub recording: SyncQuery,
    pub state: SyncState,
    pub candidates: Vec<MovieEstimate>,
    pub diagnostics: Vec<String>,
}

/// Adapter result. Abstained queries carry recording coordinates only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappedQueryResult {
    pub query: SyncQuery,
    pub state: MappedQueryState,
    /// The actual retained support positions handed to the mapper (derived IDs
    /// of retained cell/anchor pairs; never synthesized anchors).
    pub support_anchor_ids: Vec<String>,
    pub slices: Vec<MappedSlice>,
    pub diagnostics: Vec<String>,
}

/// Absolute feature-frame position -> native capture-clock sample. Exact for
/// the 11025-family feature rates; other rates round to the nearest sample
/// (absorbed by the mapper uncertainty).
fn anchor_sample(epoch_start_sample: u64, frame: u32, sample_rate: u32) -> u64 {
    epoch_start_sample
        + (u64::from(frame) * HOP as u64 * u64::from(sample_rate) + u64::from(FEATURE_RATE) / 2)
            / u64::from(FEATURE_RATE)
}

/// Map a recording-time query range through the EXISTING `tv_review_sync`
/// query path using the ACTUAL retained support positions. Anchors are only
/// derived from retained cell/anchor pairs — two independent anchors are never
/// synthesized from one pooled mode to bypass mapper gates. Queries that are
/// unknown, conflicting or gap-crossing abstain with recording timestamps and
/// no movie location; the mapper may return less coverage than the segment
/// envelope and that result is preserved without backfilling.
pub fn map_recording_query(
    report: &SegmentReport,
    request: &QueryMappingRequest,
) -> Result<MappedQueryResult> {
    ensure!(
        report.schema_version == SEGMENT_REPORT_SCHEMA_VERSION,
        "invalid input: unsupported source-audio segment report schema version {}; expected {SEGMENT_REPORT_SCHEMA_VERSION}",
        report.schema_version
    );
    ensure!(
        request.query.clock_id == report.capture_clock.clock_id,
        "invalid input: query capture clock mismatch"
    );
    ensure!(
        request.query.start_sample <= request.query.end_sample,
        "invalid input: query range"
    );
    let query = &request.query;
    let containing = report.segments.iter().find_map(|segment| {
        segment
            .supported_spans
            .iter()
            .find(|span| {
                span.start_sample <= query.start_sample && query.end_sample <= span.end_sample
            })
            .map(|span| (segment, span))
    });
    let Some((segment, _span)) = containing else {
        let contains = |start_sample: u64, end_sample: u64| {
            start_sample <= query.start_sample && query.end_sample <= end_sample
        };
        if report.unknown_ranges.iter().any(|range| {
            range.reason == REASON_CONFLICTING_OFFSETS
                && contains(range.start_sample, range.end_sample)
        }) {
            return Ok(abstain(
                request,
                MappedQueryState::Conflicting,
                "query_on_conflicting_evidence",
            ));
        }
        let inside_unknown = report
            .unknown_ranges
            .iter()
            .any(|range| contains(range.start_sample, range.end_sample))
            || report
                .segments
                .iter()
                .flat_map(|segment| segment.unknown_spans.iter())
                .any(|span| contains(span.start_sample, span.end_sample));
        if inside_unknown {
            return Ok(abstain(
                request,
                MappedQueryState::Unknown,
                "query_in_unknown_range",
            ));
        }
        return Ok(abstain(
            request,
            MappedQueryState::GapCrossing,
            "query_crosses_unknown_span_range_or_boundary",
        ));
    };

    // Anchors come from the actual retained support positions of the segment
    // (absolute feature frames -> native samples); the per-cell reconciled
    // offsets place them on the movie clock. No anchor is ever synthesized.
    let epoch_start = report
        .epochs
        .iter()
        .find(|epoch| epoch.epoch_id == segment.epoch_id)
        .map(|epoch| epoch.start_sample)
        .unwrap_or(segment.start_sample);
    let ticks = report.movie_clock.ticks_per_second();
    let mut anchors = Vec::new();
    let mut anchor_ids = Vec::new();
    for cell in &segment.retained_support {
        for frame in &cell.query_anchor_frames {
            let sample_index = anchor_sample(epoch_start, *frame, report.capture_clock.sample_rate);
            let recording_seconds =
                (sample_index - epoch_start) as f64 / f64::from(report.capture_clock.sample_rate);
            let elapsed = movie_elapsed_seconds(recording_seconds, cell.offset_seconds);
            let pts = (report.movie_clock.origin_pts as f64 + elapsed * ticks).round() as i64;
            let pts = pts.clamp(
                report.movie_clock.origin_pts,
                report.movie_clock.origin_pts + report.movie_clock.duration_pts as i64,
            );
            let expected_monotonic = report.capture_clock.monotonic_origin_ns as f64
                + sample_index as f64 / f64::from(report.capture_clock.sample_rate) * 1e9;
            let evidence_id = format!("{}c{}f{}", segment.segment_id, cell.start_sample, frame);
            anchors.push((
                sample_index,
                SyncAnchor {
                    evidence_id: evidence_id.clone(),
                    capture: CapturePosition {
                        clock_id: report.capture_clock.clock_id.clone(),
                        sample_index,
                        monotonic_ns: expected_monotonic.round() as u64,
                    },
                    movie_pts: pts,
                    source: SyncAnchorSource::PassiveSoundtrack,
                    confidence: 1.0,
                    movie_uncertainty_ticks: (segment.offset_uncertainty_seconds * ticks).ceil()
                        as u64,
                    capture_uncertainty_samples: HOP as u32,
                    candidate_group_id: None,
                },
            ));
            anchor_ids.push(evidence_id);
        }
    }
    anchors.sort_by_key(|(sample_index, _)| *sample_index);
    let thinned = anchors.len() > MAPPER_ANCHOR_LIMIT;
    anchors.truncate(MAPPER_ANCHOR_LIMIT);
    anchor_ids = anchors
        .iter()
        .map(|(_, anchor)| anchor.evidence_id.clone())
        .collect();
    anchor_ids.sort();
    anchor_ids.dedup();
    let sync_request = SyncRequest {
        schema_version: SYNC_VERSION,
        session_id: request.session_id.clone(),
        target_digest: report.movie_clock.movie_digest.clone(),
        movie: report.movie_clock.clone(),
        capture_clocks: vec![report.capture_clock.clone()],
        anchors: anchors.into_iter().map(|(_, anchor)| anchor).collect(),
        boundaries: report.declared_boundaries.clone(),
        policy: request.policy.clone(),
        consensus_segments: Vec::new(),
    };
    let sync_report = sync_request.map(std::slice::from_ref(&request.query), &|| false)?;
    let slices: Vec<MappedSlice> = sync_report
        .slices
        .iter()
        .map(|slice| MappedSlice {
            recording: slice.recording.clone(),
            state: slice.state,
            candidates: slice.candidates.clone(),
            diagnostics: slice.diagnostics.clone(),
        })
        .collect();
    let mapped = slices
        .iter()
        .filter(|slice| !slice.candidates.is_empty())
        .count();
    let state = if !slices.is_empty() && mapped == slices.len() {
        MappedQueryState::Mapped
    } else if mapped > 0 {
        MappedQueryState::PartiallyMapped
    } else {
        MappedQueryState::Unknown
    };
    let mut diagnostics = vec!["mapped_through_tv_review_sync_query_path".to_string()];
    if thinned {
        diagnostics.push("support_thinned_to_mapper_anchor_limit".to_string());
    }
    Ok(MappedQueryResult {
        query: query.clone(),
        state,
        support_anchor_ids: anchor_ids,
        slices,
        diagnostics,
    })
}

fn abstain(
    request: &QueryMappingRequest,
    state: MappedQueryState,
    reason: &str,
) -> MappedQueryResult {
    MappedQueryResult {
        query: request.query.clone(),
        state,
        support_anchor_ids: Vec::new(),
        slices: Vec::new(),
        diagnostics: vec![reason.to_string()],
    }
}

#[cfg(test)]
#[path = "source_audio_segments_tests.rs"]
mod tests;
