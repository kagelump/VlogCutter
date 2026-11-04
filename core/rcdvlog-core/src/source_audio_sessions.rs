//! Bounded source-audio recognition sessions (source-audio MVP step 3):
//! the 10-second/2-second window schedule with native sample origins,
//! bounded query batches with complete-reference lookup, cancellation,
//! progress and identity-keyed resumable derived results.
//!
//! Scheduling follows the frozen MVP design: within each continuous
//! capture-clock epoch, full [`SCHEDULE_WINDOW_SECONDS`] windows start every
//! [`SCHEDULE_STEP_SECONDS`] seconds on a regular grid, plus a final full
//! window ending at the epoch end when the grid does not reach it (an
//! identical final window is deduplicated). Epochs shorter than one window
//! schedule nothing and stay unknown; they surface explicitly as
//! [`ShortEpoch`] rows in the [`SessionSchedule`] and [`SessionResult`].
//! Every epoch carries its own recording-time origin on the capture clock.
//!
//! Scheduled windows and results carry native capture samples (half-open
//! `[start_sample, end_sample)` ranges at the ORIGINAL capture rate),
//! computed exactly from the epoch origins in integer sample arithmetic;
//! resampling to the extractor rate never shifts those origins. The offset
//! convention `movieElapsedSeconds = recordingSecondsWithinEpoch +
//! offsetSeconds` holds through query-chunk edges and nonzero native sample
//! origins: [`SessionWindowResult::offset_seconds`] is rebased from the
//! chunk-local pooled offset by the chunk's position within its epoch.
//!
//! Execution runs the scheduled windows through
//! [`source_audio_pooled::recognize`] in query batches that respect the
//! demonstrated caps ([`MAX_WINDOWS_PER_CALL`] windows and
//! [`MAX_CELL_SCANS_PER_CALL`] offset-cell scans per pooled call); every
//! window is searched against the ENTIRE reference. Progress
//! (completed-window counts) and cancellation state are published; an
//! interrupted run persists resumable per-window derived results and a later
//! run continues without redoing completed windows. Incomplete batches and
//! windows are returned explicitly and are NEVER accepted: a truncated
//! search can never produce a match. Overlapping window work is reported
//! separately from unique recording coverage.
//!
//! Derived results are persisted per window in the caller-provided
//! derived-data directory, keyed by ALL identities (recording digest, index
//! identity, recipe identity, policy identity, scheduling identity and
//! capture-clock identity). Reuse is refused when any identity changes.

use crate::source_audio_pooled::{
    self, PooledOffsetWindow, PooledRecognitionPolicy, PooledRecognitionRequest,
    PooledWindowAcceptance, PooledWindowResult, RECIPE_ID,
};
use crate::tv_soundtrack_landmarks::{Index, RATE, SearchPolicy};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Frozen MVP schedule: full 10-second recognition windows.
pub const SCHEDULE_WINDOW_SECONDS: u16 = 10;
/// Frozen MVP schedule: a new window starts every 2 seconds.
pub const SCHEDULE_STEP_SECONDS: u32 = 2;

/// Demonstrated pooler cap: at most 256 windows per pooled call.
pub const MAX_WINDOWS_PER_CALL: usize = 256;
/// Demonstrated pooler cap: at most 50,000,000 offset-cell scans
/// (`offset cells x windows`) per pooled call.
pub const MAX_CELL_SCANS_PER_CALL: usize = 50_000_000;

/// First pooled call of a query chunk: a completed search never carries more
/// than the landmark searcher's 2,000,000-cell vote cap (see
/// `tv_soundtrack_landmarks::MAX_VOTE_CELLS`), so 25 windows
/// (25 x 2,000,000 = 50,000,000) can never exceed the scan budget even before
/// the chunk's real offset-cell count is known. Later batches are sized from
/// the observed count and stay exact.
const SAFE_FIRST_BATCH: usize = 25;

/// Reason recorded on an epoch that schedules no windows because it is
/// shorter than one full window; such epochs stay unknown.
pub const SHORT_EPOCH_REASON: &str = "epoch_shorter_than_window";

const STORE_SCHEMA_VERSION: u32 = 1;
const STATUS_COMPLETE: &str = "complete";
const STATUS_CANCELLED: &str = "cancelled";
const STATUS_INCOMPLETE: &str = "incomplete";
const REASON_CANCELLED: &str = "cancelled";
const REASON_BUDGET_INFEASIBLE: &str = "budget_infeasible";
const REASON_TRUNCATED_SEARCH: &str = "truncated_search";
const REASON_NOT_ATTEMPTED: &str = "not_attempted";

/// Capture-clock identity of the recording a session was scheduled against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureClockIdentity {
    pub clock_id: String,
    /// Original capture rate (native samples per second), NOT the extractor
    /// rate. Native sample ranges on this clock never move under resampling.
    pub sample_rate: u32,
}

/// One continuous capture-clock epoch: a stretch of recording with a
/// continuous capture clock. `origin_sample` is this epoch's own
/// recording-time origin on the capture clock; recording seconds within the
/// epoch count from it. `end_sample` is the exclusive epoch end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EpochSegment {
    pub epoch_id: String,
    pub origin_sample: u64,
    pub end_sample: u64,
}

/// Scheduling identity: the schedule shape (window and step seconds) plus the
/// epoch segmentation. Any change here invalidates persisted derived results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchedulingIdentity {
    pub window_seconds: u16,
    pub step_seconds: u32,
    pub epochs: Vec<EpochSegment>,
}

/// Complete identity of a session's derived results. Every field participates
/// in the persistence key: reuse is refused when ANY identity changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionIdentity {
    pub recording_digest: String,
    /// Content-covering digest of the landmark index (`Index::index_digest`).
    pub index_identity: String,
    /// Opt-in pooled recipe identity (`source-audio-pooled-mvp-v1`).
    pub recipe_identity: String,
    /// Acceptance policy identity of the vendored v3 policy.
    pub policy_identity: String,
    pub scheduling_identity: SchedulingIdentity,
    pub capture_clock_identity: CaptureClockIdentity,
}

/// One scheduled recognition window. `start_sample`/`end_sample` are native
/// capture samples (half-open, ORIGINAL capture rate) derived exactly from the
/// epoch origins; `window_start_seconds` is the window's recording position
/// within its epoch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledWindow {
    pub capture_clock_id: String,
    pub epoch_id: String,
    pub window_evidence_id: String,
    pub window_start_seconds: f64,
    pub duration_seconds: u16,
    pub start_sample: u64,
    pub end_sample: u64,
}

/// An epoch that schedules no windows because it is shorter than one full
/// window. Such epochs stay unknown; the row surfaces that explicitly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortEpoch {
    pub epoch_id: String,
    pub start_sample: u64,
    pub end_sample: u64,
    pub duration_seconds: f64,
    pub reason: String,
}

/// The bounded window schedule of one session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSchedule {
    pub window_seconds: u16,
    pub step_seconds: u32,
    pub windows: Vec<ScheduledWindow>,
    pub short_epochs: Vec<ShortEpoch>,
}

/// Execution state of one scheduled window. `Incomplete` windows are returned
/// explicitly and can never become matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionWindowState {
    Completed,
    Incomplete,
}

/// Per-window session result: the schedule metadata of one window plus its
/// derived recognition outcome. `offset_seconds` follows the frozen convention
/// `movieElapsedSeconds = recordingSecondsWithinEpoch + offset_seconds`; the
/// window's movie span is `[window_start_seconds + offset_seconds, +duration)`.
/// `offset_bin` and `pooled` are the raw chunk-local pooled evidence (the
/// pooled window rows are chunk-local to the searched query; the chunk is the
/// window's epoch except for fractional-start final windows, whose chunk
/// starts at the window itself).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionWindowResult {
    pub capture_clock_id: String,
    pub epoch_id: String,
    pub window_evidence_id: String,
    pub start_sample: u64,
    pub end_sample: u64,
    pub window_start_seconds: f64,
    pub duration_seconds: u16,
    pub state: SessionWindowState,
    /// Documented code prefixes: `cancelled`, `budget_infeasible`,
    /// `truncated_search`, `not_attempted`, `search_failed: <message>`.
    pub incomplete_reason: Option<String>,
    /// Best-mode offset in the epoch convention; `None` when the window
    /// completed without any offset mode or when it is incomplete.
    pub offset_seconds: Option<f64>,
    /// Raw pooled offset bin of the best mode (chunk-relative evidence).
    pub offset_bin: Option<i32>,
    /// Epoch-relative start of the pooled query chunk (`Some` exactly when
    /// `pooled` is present). Pooled cell time-bins are chunk-local; adding this
    /// value yields epoch coordinates. It is 0.0 for the normal epoch-chunk
    /// case and approximately `window_start_seconds` for a fractional-start
    /// final window, whose chunk starts at the window itself.
    pub chunk_start_seconds_within_epoch: Option<f64>,
    pub acceptance: Option<PooledWindowAcceptance>,
    /// Full pooled window result (chunk-local coordinates), absent exactly for
    /// incomplete windows so that no incomplete window can become a match.
    pub pooled: Option<PooledWindowResult>,
}

impl SessionWindowResult {
    /// Whether this window produced a match-quality outcome. Incomplete
    /// windows, truncated searches and non-accepting verdicts are never
    /// accepted here.
    pub fn is_accepted(&self) -> bool {
        self.state == SessionWindowState::Completed
            && self.offset_seconds.is_some()
            && self
                .acceptance
                .as_ref()
                .is_some_and(|acceptance| acceptance.accepted == Some(true))
            && self.pooled.as_ref().is_some_and(|pooled| pooled.complete)
    }
}

/// Published progress: completed-window counts over the scheduled windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionProgress {
    pub completed_windows: usize,
    pub total_windows: usize,
}

/// Overlapping window work and unique recording coverage reported separately:
/// overlapping windows count their full duration as work but never multiply
/// the unique recording coverage.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowCoverageAccounting {
    pub window_count: usize,
    /// Sum of window durations; overlapping windows count once per window.
    pub overlapping_window_work_seconds: f64,
    /// Length of the union of the window spans on the capture clock.
    pub unique_recording_coverage_seconds: f64,
    /// `overlapping_window_work_seconds - unique_recording_coverage_seconds`.
    pub overlap_seconds: f64,
}

/// Complete session run result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResult {
    pub schema_version: u32,
    /// `complete`, `cancelled` or `incomplete`.
    pub status: String,
    pub identity: SessionIdentity,
    pub schedule: SessionSchedule,
    /// One row per scheduled window, in schedule order. Incomplete rows are
    /// explicit and never accepted.
    pub windows: Vec<SessionWindowResult>,
    pub cancelled: bool,
    pub progress: SessionProgress,
    pub window_accounting: WindowCoverageAccounting,
    pub accepted_accounting: WindowCoverageAccounting,
    pub reused_window_count: usize,
    pub computed_window_count: usize,
}

/// Run request: the session identity (which carries the schedule), the
/// decoded extractor-rate query PCM and its capture-clock origin, and the
/// caller-provided derived-data directory for resumable per-window results.
pub struct SessionRunRequest<'a> {
    pub identity: SessionIdentity,
    pub pcm: &'a [i16],
    /// Native capture-clock sample where `pcm[0]` sits (original rate).
    pub pcm_origin_sample: u64,
    pub derived_data_dir: PathBuf,
    pub search_policy: Option<SearchPolicy>,
    pub mode_limit: Option<u8>,
}

/// Build the bounded window schedule for one session identity: the regular
/// grid inside every epoch, the deduplicated final window at each epoch end
/// and explicit [`ShortEpoch`] rows for epochs shorter than one window.
pub fn build_schedule(identity: &SessionIdentity) -> Result<SessionSchedule> {
    let scheduling = &identity.scheduling_identity;
    let clock = &identity.capture_clock_identity;
    validate_capture_clock(clock)?;
    ensure!(
        matches!(scheduling.window_seconds, 10 | 20 | 30),
        "invalid input: source-audio session window seconds must be 10, 20 or 30"
    );
    ensure!(
        scheduling.step_seconds >= 1,
        "invalid input: source-audio session step seconds must be positive"
    );
    let rate = u64::from(clock.sample_rate);
    let mut seen_epochs = BTreeSet::new();
    let mut previous_end: Option<u64> = None;
    for epoch in &scheduling.epochs {
        ensure!(
            seen_epochs.insert(epoch.epoch_id.as_str()),
            "invalid input: duplicate source-audio epoch id"
        );
        ensure!(
            epoch.end_sample > epoch.origin_sample,
            "invalid input: source-audio epoch must span at least one sample"
        );
        ensure!(
            epoch.end_sample - epoch.origin_sample <= rate * 3600,
            "invalid input: source-audio epoch exceeds the one-hour limit"
        );
        if let Some(previous) = previous_end {
            ensure!(
                epoch.origin_sample >= previous,
                "invalid input: source-audio epochs must not overlap"
            );
        }
        previous_end = Some(epoch.end_sample);
    }

    let window_samples = u64::from(scheduling.window_seconds) * rate;
    let step_samples = u64::from(scheduling.step_seconds) * rate;
    let mut windows = Vec::new();
    let mut short_epochs = Vec::new();
    for epoch in &scheduling.epochs {
        let duration_samples = epoch.end_sample - epoch.origin_sample;
        if duration_samples < window_samples {
            short_epochs.push(ShortEpoch {
                epoch_id: epoch.epoch_id.clone(),
                start_sample: epoch.origin_sample,
                end_sample: epoch.end_sample,
                duration_seconds: duration_samples as f64 / rate as f64,
                reason: SHORT_EPOCH_REASON.to_string(),
            });
            continue;
        }
        let mut start_in_epoch = 0u64;
        while start_in_epoch + window_samples <= duration_samples {
            windows.push(scheduled_window(
                identity,
                epoch,
                epoch.origin_sample + start_in_epoch,
                epoch.origin_sample + start_in_epoch + window_samples,
            ));
            start_in_epoch += step_samples;
        }
        // Final full window ending at the epoch end; deduplicated when it is
        // identical to a grid window (exact sample arithmetic, no rounding).
        let final_start_in_epoch = duration_samples - window_samples;
        if !final_start_in_epoch.is_multiple_of(step_samples) {
            windows.push(scheduled_window(
                identity,
                epoch,
                epoch.origin_sample + final_start_in_epoch,
                epoch.end_sample,
            ));
        }
    }
    Ok(SessionSchedule {
        window_seconds: scheduling.window_seconds,
        step_seconds: scheduling.step_seconds,
        windows,
        short_epochs,
    })
}

fn scheduled_window(
    identity: &SessionIdentity,
    epoch: &EpochSegment,
    start_sample: u64,
    end_sample: u64,
) -> ScheduledWindow {
    let rate = u64::from(identity.capture_clock_identity.sample_rate);
    ScheduledWindow {
        capture_clock_id: identity.capture_clock_identity.clock_id.clone(),
        epoch_id: epoch.epoch_id.clone(),
        window_evidence_id: format!(
            "saw:{}:{}:{}-{}",
            identity.capture_clock_identity.clock_id, epoch.epoch_id, start_sample, end_sample
        ),
        window_start_seconds: (start_sample - epoch.origin_sample) as f64 / rate as f64,
        duration_seconds: identity.scheduling_identity.window_seconds,
        start_sample,
        end_sample,
    }
}

/// Run the scheduled windows through [`source_audio_pooled::recognize`] in
/// bounded query batches, publishing progress and cancellation state. An
/// interrupted run persists the completed per-window derived results and a
/// later run with the same identity continues without redoing them; reuse is
/// refused whenever any identity component changes.
pub fn run_session(
    index: &Index,
    request: &SessionRunRequest<'_>,
    on_progress: &mut dyn FnMut(SessionProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<SessionResult> {
    let identity = &request.identity;
    ensure!(
        identity.recipe_identity == RECIPE_ID,
        "invalid input: source-audio session recipe identity must be {RECIPE_ID}"
    );
    ensure!(
        identity.policy_identity == PooledRecognitionPolicy::frozen().policy_id,
        "invalid input: source-audio session policy identity does not match the vendored policy"
    );
    ensure!(
        identity.index_identity == index.index_digest,
        "invalid input: source-audio session index identity does not match the searched index"
    );
    let schedule = build_schedule(identity)?;
    validate_pcm_coverage(identity, &schedule, request.pcm, request.pcm_origin_sample)?;
    let store_path = store_path(&request.derived_data_dir, identity)?;
    let stored = load_store(&store_path, identity, &schedule)?;

    let mut executor = SessionExecutor {
        index,
        identity,
        pcm: request.pcm,
        pcm_origin_sample: request.pcm_origin_sample,
        rate: identity.capture_clock_identity.sample_rate,
        search_policy: request.search_policy,
        mode_limit: request.mode_limit,
        store_path,
        schedule: &schedule,
        results: Vec::with_capacity(schedule.windows.len()),
        reused: 0,
        computed: 0,
    };
    for window in &schedule.windows {
        match stored.get(&window_key(window)) {
            Some(done) => {
                executor.results.push(Some(done.clone()));
                executor.reused += 1;
            }
            None => executor.results.push(None),
        }
    }
    on_progress(SessionProgress {
        completed_windows: executor.reused,
        total_windows: schedule.windows.len(),
    });

    let cancelled = executor.run(on_progress, cancel);
    Ok(executor.finish(cancelled))
}

/// Next pooled-call batch size for a query chunk: never more than
/// [`MAX_WINDOWS_PER_CALL`] windows and never more than
/// [`MAX_CELL_SCANS_PER_CALL`] cell scans (`offset_cells x windows`).
/// `observed_offset_cells` is the chunk's observed offset-cell count (`None`
/// before the first call). A returned size of 0 means no batch can respect the
/// scan budget and the remaining windows must be reported incomplete.
fn planned_batch_size(remaining_windows: usize, observed_offset_cells: Option<usize>) -> usize {
    match observed_offset_cells {
        None => remaining_windows.min(SAFE_FIRST_BATCH),
        Some(cells) => remaining_windows.min(max_windows_per_call(cells)),
    }
}

fn max_windows_per_call(offset_cells: usize) -> usize {
    MAX_CELL_SCANS_PER_CALL
        .checked_div(offset_cells)
        .unwrap_or(MAX_WINDOWS_PER_CALL)
        .min(MAX_WINDOWS_PER_CALL)
}

/// Derive one per-window result from the pooled window evidence. `truncated`
/// covers every completeness flag: an incomplete search or scan is returned
/// explicitly as an incomplete window and can never produce a match, even if
/// the pooled acceptance claims otherwise.
fn session_window_result(
    window: &ScheduledWindow,
    chunk_start_within_epoch: f64,
    pooled: PooledWindowResult,
    evidence_complete: bool,
) -> SessionWindowResult {
    let truncated = !(evidence_complete && pooled.complete);
    let mut result = SessionWindowResult {
        capture_clock_id: window.capture_clock_id.clone(),
        epoch_id: window.epoch_id.clone(),
        window_evidence_id: window.window_evidence_id.clone(),
        start_sample: window.start_sample,
        end_sample: window.end_sample,
        window_start_seconds: window.window_start_seconds,
        duration_seconds: window.duration_seconds,
        state: SessionWindowState::Completed,
        incomplete_reason: None,
        offset_seconds: None,
        offset_bin: None,
        chunk_start_seconds_within_epoch: None,
        acceptance: None,
        pooled: None,
    };
    if truncated {
        result.state = SessionWindowState::Incomplete;
        result.incomplete_reason = Some(REASON_TRUNCATED_SEARCH.to_string());
        return result;
    }
    if let Some(best) = pooled.modes.first() {
        result.offset_bin = Some(best.offset_bin);
        result.offset_seconds = Some(best.offset_seconds - chunk_start_within_epoch);
    }
    result.chunk_start_seconds_within_epoch = Some(chunk_start_within_epoch);
    result.acceptance = Some(pooled.acceptance.clone());
    result.pooled = Some(pooled);
    result
}

/// Overlapping window work (sum of durations) versus unique recording
/// coverage (union of the half-open native sample ranges).
fn coverage_accounting(rate: u32, ranges: &[(u64, u64)]) -> WindowCoverageAccounting {
    let mut sorted: Vec<(u64, u64)> = ranges.to_vec();
    sorted.sort_unstable();
    let mut union_samples = 0u64;
    let mut merged: Option<(u64, u64)> = None;
    for (start, end) in sorted {
        match merged {
            None => merged = Some((start, end)),
            Some((m_start, m_end)) if start <= m_end => {
                merged = Some((m_start, m_end.max(end)));
            }
            Some((m_start, m_end)) => {
                union_samples += m_end - m_start;
                merged = Some((start, end));
            }
        }
    }
    if let Some((m_start, m_end)) = merged {
        union_samples += m_end - m_start;
    }
    let work_samples: u64 = ranges.iter().map(|(start, end)| end - start).sum();
    let work = work_samples as f64 / rate as f64;
    let unique = union_samples as f64 / rate as f64;
    WindowCoverageAccounting {
        window_count: ranges.len(),
        overlapping_window_work_seconds: work,
        unique_recording_coverage_seconds: unique,
        overlap_seconds: work - unique,
    }
}

fn validate_capture_clock(clock: &CaptureClockIdentity) -> Result<()> {
    ensure!(
        !clock.clock_id.is_empty(),
        "invalid input: source-audio capture clock id must not be empty"
    );
    ensure!(
        (8000..=192000).contains(&clock.sample_rate),
        "invalid input: source-audio capture clock sample rate"
    );
    Ok(())
}

/// Every epoch must lie inside the decoded extractor-rate PCM coverage; the
/// extractor end is rounded outward (ceil) so a chunk never claims samples it
/// does not have.
fn validate_pcm_coverage(
    identity: &SessionIdentity,
    schedule: &SessionSchedule,
    pcm: &[i16],
    pcm_origin_sample: u64,
) -> Result<()> {
    ensure!(
        !pcm.is_empty() && pcm.len() <= RATE as usize * 60 * 60,
        "invalid input: bounded query PCM required"
    );
    let rate = identity.capture_clock_identity.sample_rate;
    for epoch in &identity.scheduling_identity.epochs {
        extractor_end(epoch.origin_sample, pcm_origin_sample, rate, pcm.len())?;
        extractor_end(epoch.end_sample, pcm_origin_sample, rate, pcm.len())?;
    }
    for window in &schedule.windows {
        extractor_end(window.start_sample, pcm_origin_sample, rate, pcm.len())?;
        extractor_end(window.end_sample, pcm_origin_sample, rate, pcm.len())?;
    }
    Ok(())
}

fn extractor_end(sample: u64, pcm_origin_sample: u64, rate: u32, pcm_len: usize) -> Result<usize> {
    ensure!(
        sample >= pcm_origin_sample,
        "invalid input: source-audio epoch or window begins before the decoded recording"
    );
    let distance = u128::from(sample - pcm_origin_sample);
    let end = (distance * u128::from(RATE)).div_ceil(u128::from(rate));
    ensure!(
        end <= pcm_len as u128,
        "invalid input: source-audio epoch or window extends past the decoded recording"
    );
    Ok(end as usize)
}

fn window_key(window: &ScheduledWindow) -> (String, u64) {
    (window.epoch_id.clone(), window.start_sample)
}

fn store_key(identity: &SessionIdentity) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(identity)?)
        .to_hex()
        .to_string())
}

fn store_path(derived_data_dir: &Path, identity: &SessionIdentity) -> Result<PathBuf> {
    Ok(derived_data_dir.join(format!(
        "source-audio-session-{}.json",
        store_key(identity)?
    )))
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionStoreFile {
    schema_version: u32,
    identity: SessionIdentity,
    windows: Vec<SessionWindowResult>,
}

fn load_store(
    path: &Path,
    identity: &SessionIdentity,
    schedule: &SessionSchedule,
) -> Result<BTreeMap<(String, u64), SessionWindowResult>> {
    let mut out = BTreeMap::new();
    if !path.exists() {
        return Ok(out);
    }
    let bytes = std::fs::read(path)?;
    let store: SessionStoreFile = serde_json::from_slice(&bytes).map_err(|err| {
        anyhow::anyhow!("invalid input: unreadable source-audio derived session: {err}")
    })?;
    ensure!(
        store.schema_version == STORE_SCHEMA_VERSION,
        "invalid input: source-audio derived session schema version"
    );
    ensure!(
        store.identity == *identity,
        "invalid input: source-audio derived session identity mismatch; refusing to reuse"
    );
    let scheduled: BTreeMap<(String, u64), &ScheduledWindow> = schedule
        .windows
        .iter()
        .map(|window| (window_key(window), window))
        .collect();
    for window in store.windows {
        ensure!(
            window.state == SessionWindowState::Completed,
            "invalid input: source-audio derived session holds an incomplete window; refusing to reuse"
        );
        let key = (window.epoch_id.clone(), window.start_sample);
        let expected = scheduled.get(&key).ok_or_else(|| {
            anyhow::anyhow!("invalid input: source-audio derived session holds an unscheduled window; refusing to reuse")
        })?;
        ensure!(
            expected.capture_clock_id == window.capture_clock_id
                && expected.window_evidence_id == window.window_evidence_id
                && expected.end_sample == window.end_sample
                && expected.duration_seconds == window.duration_seconds
                && expected.window_start_seconds == window.window_start_seconds,
            "invalid input: source-audio derived session window does not match the schedule; refusing to reuse"
        );
        ensure!(
            out.insert(key, window).is_none(),
            "invalid input: duplicate window in source-audio derived session; refusing to reuse"
        );
    }
    Ok(out)
}

fn persist_completed(
    path: &Path,
    identity: &SessionIdentity,
    results: &[Option<SessionWindowResult>],
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = SessionStoreFile {
        schema_version: STORE_SCHEMA_VERSION,
        identity: identity.clone(),
        windows: results
            .iter()
            .flatten()
            .filter(|result| result.state == SessionWindowState::Completed)
            .cloned()
            .collect(),
    };
    let bytes = serde_json::to_vec(&store)?;
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    std::fs::write(PathBuf::from(temporary.as_os_str()), &bytes)?;
    std::fs::rename(PathBuf::from(temporary.as_os_str()), path)?;
    Ok(())
}

struct SessionExecutor<'a> {
    index: &'a Index,
    identity: &'a SessionIdentity,
    pcm: &'a [i16],
    pcm_origin_sample: u64,
    rate: u32,
    search_policy: Option<SearchPolicy>,
    mode_limit: Option<u8>,
    store_path: PathBuf,
    schedule: &'a SessionSchedule,
    results: Vec<Option<SessionWindowResult>>,
    reused: usize,
    computed: usize,
}

impl SessionExecutor<'_> {
    /// Execute every pending window in bounded query batches. Returns whether
    /// the run was cancelled; on cancellation the remaining windows stay
    /// pending and are marked cancelled afterwards.
    fn run(
        &mut self,
        on_progress: &mut dyn FnMut(SessionProgress),
        cancel: &dyn Fn() -> bool,
    ) -> bool {
        let rate = u64::from(self.rate);
        let identity = self.identity;
        let schedule = self.schedule;
        for epoch in &identity.scheduling_identity.epochs {
            let pending: Vec<usize> = schedule
                .windows
                .iter()
                .enumerate()
                .filter(|(index, window)| {
                    window.epoch_id == epoch.epoch_id && self.results[*index].is_none()
                })
                .map(|(index, _)| index)
                .collect();
            if pending.is_empty() {
                continue;
            }
            // The epoch chunk is the query for every integral-start window;
            // a fractional-start final window runs on its own chunk starting
            // at the window itself (chunk-local start 0).
            let (fractional, integral): (Vec<usize>, Vec<usize>) =
                pending.into_iter().partition(|index| {
                    !(schedule.windows[*index].start_sample - epoch.origin_sample)
                        .is_multiple_of(rate)
                });
            if !integral.is_empty() {
                let epoch_origin = epoch.origin_sample;
                let epoch_end = epoch.end_sample;
                let e0 = ((epoch_origin - self.pcm_origin_sample) as u128 * u128::from(RATE)
                    / u128::from(self.rate)) as usize;
                let e1 =
                    extractor_end(epoch_end, self.pcm_origin_sample, self.rate, self.pcm.len())
                        .expect("epoch coverage validated");
                let chunk = &self.pcm[e0..e1];
                let chunk0_samples =
                    self.pcm_origin_sample as f64 + e0 as f64 * self.rate as f64 / RATE as f64;
                let chunk_start_within_epoch =
                    (chunk0_samples - epoch_origin as f64) / self.rate as f64;
                let group: Vec<(usize, u32)> = integral
                    .into_iter()
                    .map(|index| {
                        let window = &self.schedule.windows[index];
                        let relative =
                            (window.start_sample as f64 - chunk0_samples) / self.rate as f64;
                        let start = relative.round();
                        assert!(
                            (relative - start).abs() < 1e-3,
                            "integral window start must fall on a whole second within the chunk"
                        );
                        (index, start as u32)
                    })
                    .collect();
                if self.execute_group(chunk, chunk_start_within_epoch, &group, on_progress, cancel)
                {
                    return true;
                }
            }
            for index in fractional {
                if cancel() {
                    return true;
                }
                let window = &self.schedule.windows[index];
                let e0 = ((window.start_sample - self.pcm_origin_sample) as u128 * u128::from(RATE)
                    / u128::from(self.rate)) as usize;
                let chunk = &self.pcm[e0..e0 + 10 * RATE as usize];
                let chunk0_samples =
                    self.pcm_origin_sample as f64 + e0 as f64 * self.rate as f64 / RATE as f64;
                let chunk_start_within_epoch =
                    (chunk0_samples - epoch.origin_sample as f64) / self.rate as f64;
                if self.execute_group(
                    chunk,
                    chunk_start_within_epoch,
                    &[(index, 0)],
                    on_progress,
                    cancel,
                ) {
                    return true;
                }
            }
        }
        false
    }

    /// Execute one query chunk's windows in bounded pooled batches. Returns
    /// true when the run is cancelled and remaining windows must stop.
    fn execute_group(
        &mut self,
        chunk: &[i16],
        chunk_start_within_epoch: f64,
        group: &[(usize, u32)],
        on_progress: &mut dyn FnMut(SessionProgress),
        cancel: &dyn Fn() -> bool,
    ) -> bool {
        let mut cursor = 0usize;
        let mut observed_offset_cells: Option<usize> = None;
        while cursor < group.len() {
            if cancel() {
                return true;
            }
            let batch_size = planned_batch_size(group.len() - cursor, observed_offset_cells);
            if batch_size == 0 {
                for &(index, _) in &group[cursor..] {
                    self.mark_incomplete(index, REASON_BUDGET_INFEASIBLE.to_string());
                }
                break;
            }
            let batch = &group[cursor..cursor + batch_size];
            let windows: Vec<PooledOffsetWindow> = batch
                .iter()
                .map(|&(index, start)| PooledOffsetWindow {
                    start_seconds: start,
                    duration_seconds: self.schedule.windows[index].duration_seconds,
                })
                .collect();
            let pooled_request = PooledRecognitionRequest {
                search_policy: self.search_policy,
                windows: Some(windows),
                mode_limit: self.mode_limit,
                // Retain the supporting one-second cells (query anchor frames)
                // the segment assembler needs; persisted derived results carry
                // them through resume.
                include_mode_support: Some(true),
            };
            match source_audio_pooled::recognize(self.index, chunk, &pooled_request, cancel) {
                Ok(recognition) => {
                    let Some(evidence) = recognition.evidence else {
                        for &(index, _) in &group[cursor..] {
                            self.mark_incomplete(
                                index,
                                "search_failed: pooled evidence missing".to_string(),
                            );
                        }
                        break;
                    };
                    observed_offset_cells = Some(evidence.internal_offset_cell_count);
                    let evidence_complete = evidence.complete
                        && evidence.search_complete
                        && evidence.offset_cell_scan_complete;
                    for (offset, &(index, _)) in batch.iter().enumerate() {
                        let pooled = evidence
                            .windows
                            .get(offset)
                            .cloned()
                            .expect("pooler returns one row per requested window");
                        self.results[index] = Some(session_window_result(
                            &self.schedule.windows[index],
                            chunk_start_within_epoch,
                            pooled,
                            evidence_complete,
                        ));
                    }
                    if let Err(err) =
                        persist_completed(&self.store_path, self.identity, &self.results)
                    {
                        for &(index, _) in &group[cursor..] {
                            self.mark_incomplete(
                                index,
                                format!("search_failed: derived result persistence failed: {err}"),
                            );
                        }
                        break;
                    }
                    for &(index, _) in batch {
                        if self.results[index]
                            .as_ref()
                            .is_some_and(|result| result.state == SessionWindowState::Completed)
                        {
                            self.computed += 1;
                        }
                    }
                    cursor += batch_size;
                    on_progress(SessionProgress {
                        completed_windows: self.reused + self.computed,
                        total_windows: self.schedule.windows.len(),
                    });
                }
                Err(err) => {
                    let reason = if cancel() {
                        REASON_CANCELLED.to_string()
                    } else {
                        format!("search_failed: {err}")
                    };
                    for &(index, _) in &group[cursor..] {
                        self.mark_incomplete(index, reason.clone());
                    }
                    return cancel();
                }
            }
        }
        false
    }

    fn mark_incomplete(&mut self, index: usize, reason: String) {
        let window = &self.schedule.windows[index];
        self.results[index] = Some(SessionWindowResult {
            capture_clock_id: window.capture_clock_id.clone(),
            epoch_id: window.epoch_id.clone(),
            window_evidence_id: window.window_evidence_id.clone(),
            start_sample: window.start_sample,
            end_sample: window.end_sample,
            window_start_seconds: window.window_start_seconds,
            duration_seconds: window.duration_seconds,
            state: SessionWindowState::Incomplete,
            incomplete_reason: Some(reason),
            offset_seconds: None,
            offset_bin: None,
            chunk_start_seconds_within_epoch: None,
            acceptance: None,
            pooled: None,
        });
    }

    fn finish(mut self, cancelled: bool) -> SessionResult {
        for index in 0..self.results.len() {
            if self.results[index].is_none() {
                let reason = if cancelled {
                    REASON_CANCELLED
                } else {
                    REASON_NOT_ATTEMPTED
                };
                self.mark_incomplete(index, reason.to_string());
            }
        }
        let windows: Vec<SessionWindowResult> = self.results.iter().flatten().cloned().collect();
        let all_completed = windows
            .iter()
            .all(|window| window.state == SessionWindowState::Completed);
        let status = if cancelled {
            STATUS_CANCELLED
        } else if all_completed {
            STATUS_COMPLETE
        } else {
            STATUS_INCOMPLETE
        };
        let rate = self.rate;
        let window_accounting = coverage_accounting(
            rate,
            &self
                .schedule
                .windows
                .iter()
                .map(|window| (window.start_sample, window.end_sample))
                .collect::<Vec<_>>(),
        );
        let accepted_accounting = coverage_accounting(
            rate,
            &windows
                .iter()
                .filter(|window| window.is_accepted())
                .map(|window| (window.start_sample, window.end_sample))
                .collect::<Vec<_>>(),
        );
        SessionResult {
            schema_version: 1,
            status: status.to_string(),
            identity: self.identity.clone(),
            schedule: self.schedule.clone(),
            windows,
            cancelled,
            progress: SessionProgress {
                completed_windows: self.reused + self.computed,
                total_windows: self.schedule.windows.len(),
            },
            window_accounting,
            accepted_accounting,
            reused_window_count: self.reused,
            computed_window_count: self.computed,
        }
    }
}

#[cfg(test)]
#[path = "source_audio_sessions_tests.rs"]
mod tests;
