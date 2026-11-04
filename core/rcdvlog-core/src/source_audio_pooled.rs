//! Core-owned pooled source-audio recognition, recipe `source-audio-pooled-mvp-v1`.
//!
//! This module owns the experimental recognition pipeline that previously lived
//! in the CLI: pooling complete per-second exact-offset cells into offset-mode
//! hypotheses, query-anchor-identity deduplication, candidate ranking, and the
//! versioned v3 acceptance decision. The CLI only validates requests, invokes
//! [`recognize`] (or [`pool_exact_offset_windows`]) and formats results.
//!
//! Frozen recognition semantics (recipe `source-audio-pooled-mvp-v1`): FFT512,
//! density100, fanout3 and exact 80 ms offset bins. Pool ALL full-reference
//! correspondences per requested window BEFORE diagnostic truncation; dedupe
//! query-anchor identities per offset hypothesis; the strongest NON-ADJACENT
//! competitor is determined from the full ranking, not the returned top-N rows.
//! Windows are independent observations: no evidence is combined across
//! overlapping windows, and neighboring offset bins remain alternatives.
//!
//! Acceptance is evaluated against the vendored policy
//! `tests/source_audio_eval/policies/pooled-recognition-policy-v3-anchor-aware.json`
//! (embedded byte-identical here). Verdicts are calibrated only for the frozen
//! recipe and the policy's calibrated floors (10-second windows); every other
//! duration/extractor abstains exactly like the frozen scorer's
//! `duration_or_density_not_calibrated` rule. Undefined margins are never
//! replaced by invented confidence: they fail their gate with the frozen
//! scorer's reason string.

use crate::tv_soundtrack_landmarks::{Index, OffsetEvidence, RATE, Recipe, Search, SearchPolicy};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

/// Opt-in recipe identity for pooled exact-bin recognition with anchor
/// identity. Recorded on every result; acceptance verdicts are only calibrated
/// under this identity (FFT512, density100, fanout3).
pub const RECIPE_ID: &str = "source-audio-pooled-mvp-v1";

/// Exact 80 ms offset bin width of the frozen recipe.
pub const OFFSET_BIN_SECONDS: f64 = 0.08;

const MAX_WINDOWS: usize = 256;
const MAX_MODE_LIMIT: usize = 64;
const MAX_WINDOW_CELL_SCANS: usize = 50_000_000;
const DEFAULT_MODE_LIMIT: usize = 8;

/// Frozen rejection/abstention vocabulary, verbatim from the demonstrated
/// scorer `$SOURCE_AUDIO_EVAL_ROOT/score_m1_anchor_identity_v2.py`
/// (`policy_pass`, `policy_pass_from_result_with_reasons`).
pub const REASON_INCOMPLETE_EVIDENCE: &str = "incomplete_evidence";
pub const REASON_NO_OFFSET_MODE: &str = "no_offset_mode";
pub const REASON_MINIMUM_ANCHORS: &str = "minimum_anchors";
pub const REASON_MINIMUM_OCCUPIED_TIME_BINS: &str = "minimum_occupied_time_bins";
pub const REASON_MINIMUM_DISTINCT_COMPETITOR_RATIO: &str = "minimum_distinct_competitor_ratio";
pub const REASON_MINIMUM_UNIQUE_ANCHOR_MARGIN: &str = "minimum_unique_anchor_margin";
pub const REASON_NOT_CALIBRATED: &str = "duration_or_density_not_calibrated";

const ANCHOR_IDENTITY_SCOPE: &str = "query landmark frame index within the current decoded recording; available to the in-memory pool and omitted from serialized raw diagnostic rows";
const AGGREGATION_SCOPE: &str = "exact offset bin per window; query anchor IDs deduplicate within a mode and quantify cross-mode overlap; no neighboring-bin merge and no cross-window accumulation";

/// Byte-identical vendored v3 acceptance policy. Changing these bytes changes
/// acceptance semantics and requires a NEW recipe/policy identity.
const V3_POLICY_JSON: &str = include_str!(
    "../tests/source_audio_eval/policies/pooled-recognition-policy-v3-anchor-aware.json"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PooledOffsetWindow {
    pub start_seconds: u32,
    pub duration_seconds: u16,
}

/// Typed request for core-owned recognition: the existing search inputs plus
/// the requested pooled windows and diagnostic mode limit. Optional fields are
/// omitted when unset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PooledRecognitionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_policy: Option<SearchPolicy>,
    /// Omitted when unset: without windows no pooling runs and the evidence
    /// result stays absent. An empty list is invalid (see the pooler limits).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<Vec<PooledOffsetWindow>>,
    /// Diagnostic rows returned per window after pooling; defaults to 8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_limit: Option<u8>,
    /// Opt-in per-mode supporting evidence: the occupied one-second cells with
    /// their absolute query-anchor feature frames. When unset (the default),
    /// serialized diagnostics stay byte-identical to the demonstrated output;
    /// when `Some(true)` each returned mode row carries `supportingCells`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_mode_support: Option<bool>,
}

/// One occupied one-second cell of an offset-mode hypothesis with the absolute
/// query-anchor feature-frame positions that support the mode in that cell.
/// Returned only when [`PooledRecognitionRequest::include_mode_support`] is
/// `Some(true)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PooledModeSupportCell {
    /// One-second query time bin (recording seconds within the decoded query).
    pub time_bin: u32,
    /// Absolute feature-frame positions within the decoded recording
    /// (a local chunk frame number is never used as an identity).
    pub query_anchor_frames: Vec<u32>,
}

/// A pooled offset-mode hypothesis row (diagnostic top-N output). Scores and
/// decisions never depend on how many rows are returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PooledOffsetModeRow {
    pub offset_bin: i32,
    pub offset_seconds: f64,
    pub independent_anchors: u64,
    pub summed_cell_anchor_count: u64,
    pub duplicate_anchor_votes_removed: u64,
    pub occupied_time_bins: usize,
    pub support_span_seconds: u32,
    pub rarity_weight: f64,
    pub frequency_diversity_median_per_time_bin: f64,
    pub saturated_anchors: u64,
    pub shared_query_anchors_with_best: usize,
    /// Occupied one-second cells with their absolute query-anchor feature
    /// frames; `None` (and omitted from serialization) unless the request set
    /// `includeModeSupport`, keeping serialized diagnostics byte-compatible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supporting_cells: Option<Vec<PooledModeSupportCell>>,
}

/// The strongest non-adjacent competing offset bin (|delta bin| >= 2) taken
/// from the full ranking, not from the returned top-N rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PooledDistinctCompetitor {
    pub offset_bin: i32,
    pub offset_seconds: f64,
    pub independent_anchors: u64,
    pub occupied_time_bins: usize,
    pub shared_query_anchors_with_best: usize,
    pub unique_query_anchors_outside_best: usize,
}

/// v3 acceptance verdict for one window. `accepted` is `None` exactly when the
/// verdict abstains (uncalibrated floors or a non-frozen extractor recipe),
/// mirroring the frozen scorer's tri-state `policyV3AnchorAwareAccepted`.
/// Round-trips through the frozen reason vocabulary (`serde(try_from)`), so a
/// deserialized verdict always carries the canonical statics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", try_from = "PooledWindowAcceptanceWire")]
pub struct PooledWindowAcceptance {
    /// Per-duration floors from the vendored policy are calibrated
    /// (10-second windows only; 20/30-second floors are diagnostic-only).
    pub floors_calibrated: bool,
    /// The search ran under the frozen recipe (FFT512, density100, fanout3).
    pub recipe_calibrated: bool,
    pub accepted: Option<bool>,
    /// Frozen scorer vocabulary; `["duration_or_density_not_calibrated"]`
    /// when abstaining.
    pub failure_reasons: Vec<&'static str>,
    /// Per-duration gate evaluation against the vendored floors, always
    /// computed (diagnostic-only for uncalibrated durations).
    pub gate_failure_reasons: Vec<&'static str>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PooledWindowAcceptanceWire {
    floors_calibrated: bool,
    recipe_calibrated: bool,
    accepted: Option<bool>,
    #[serde(default)]
    failure_reasons: Vec<String>,
    #[serde(default)]
    gate_failure_reasons: Vec<String>,
}

impl TryFrom<PooledWindowAcceptanceWire> for PooledWindowAcceptance {
    type Error = String;
    fn try_from(wire: PooledWindowAcceptanceWire) -> Result<Self, Self::Error> {
        Ok(PooledWindowAcceptance {
            floors_calibrated: wire.floors_calibrated,
            recipe_calibrated: wire.recipe_calibrated,
            accepted: wire.accepted,
            failure_reasons: wire
                .failure_reasons
                .iter()
                .map(|reason| frozen_reason(reason))
                .collect::<Result<_, _>>()?,
            gate_failure_reasons: wire
                .gate_failure_reasons
                .iter()
                .map(|reason| frozen_reason(reason))
                .collect::<Result<_, _>>()?,
        })
    }
}

/// The frozen rejection/abstention vocabulary is closed: a round-tripped
/// reason maps back onto its canonical static.
fn frozen_reason(reason: &str) -> Result<&'static str, String> {
    for candidate in [
        REASON_INCOMPLETE_EVIDENCE,
        REASON_NO_OFFSET_MODE,
        REASON_MINIMUM_ANCHORS,
        REASON_MINIMUM_OCCUPIED_TIME_BINS,
        REASON_MINIMUM_DISTINCT_COMPETITOR_RATIO,
        REASON_MINIMUM_UNIQUE_ANCHOR_MARGIN,
        REASON_NOT_CALIBRATED,
    ] {
        if reason == candidate {
            return Ok(candidate);
        }
    }
    Err(format!(
        "invalid input: unknown pooled acceptance reason {reason:?}"
    ))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PooledWindowResult {
    pub start_seconds: u32,
    pub duration_seconds: u16,
    pub end_seconds: u32,
    pub complete: bool,
    pub internal_offset_cell_count: usize,
    pub offset_cells_scanned: usize,
    pub offset_cells_pooled_for_window: usize,
    pub offset_cells_dropped: usize,
    pub ranked_offset_bin_count: usize,
    pub returned_offset_bin_count: usize,
    pub best_distinct_competitor: Option<PooledDistinctCompetitor>,
    pub best_anchor_margin: Option<f64>,
    pub best_unique_anchor_margin: Option<f64>,
    pub modes: Vec<PooledOffsetModeRow>,
    pub acceptance: PooledWindowAcceptance,
}

/// Pooled recognition result. Carries the demonstrated payload key names and
/// shape unchanged; `recipeIdentity` and `acceptancePolicyId` record the
/// versioned recipe/policy identity and per-window `acceptance` rows carry the
/// v3 verdict. Round-trips through its wire shape (`serde(try_from)`), so a
/// deserialized result always carries the canonical identity statics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", try_from = "PooledRecognitionResultWire")]
pub struct PooledRecognitionResult {
    pub schema_version: u32,
    pub status: &'static str,
    pub complete: bool,
    pub search_complete: bool,
    pub internal_offset_cell_count: usize,
    pub offset_cell_scan_complete: bool,
    pub diagnostic_truncation_applied_after_pooling: bool,
    pub query_anchor_identity_available: bool,
    pub anchor_identity_serialized: bool,
    pub anchor_identity_scope: &'static str,
    pub aggregation_scope: &'static str,
    pub recipe_identity: &'static str,
    pub acceptance_policy_id: &'static str,
    pub mode_limit: usize,
    pub windows: Vec<PooledWindowResult>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PooledRecognitionResultWire {
    schema_version: u32,
    status: String,
    complete: bool,
    search_complete: bool,
    internal_offset_cell_count: usize,
    offset_cell_scan_complete: bool,
    diagnostic_truncation_applied_after_pooling: bool,
    query_anchor_identity_available: bool,
    anchor_identity_serialized: bool,
    anchor_identity_scope: String,
    aggregation_scope: String,
    recipe_identity: String,
    acceptance_policy_id: String,
    mode_limit: usize,
    #[serde(default)]
    windows: Vec<PooledWindowResult>,
}

impl TryFrom<PooledRecognitionResultWire> for PooledRecognitionResult {
    type Error = String;
    fn try_from(wire: PooledRecognitionResultWire) -> Result<Self, Self::Error> {
        let status = match wire.status.as_str() {
            "complete" => "complete",
            "incomplete" => "incomplete",
            other => {
                return Err(format!("invalid input: unknown pooled status {other:?}"));
            }
        };
        let identity_static = |value: &str, expected: &'static str, what: &str| {
            if value == expected {
                Ok(expected)
            } else {
                Err(format!("invalid input: unexpected pooled {what} {value:?}"))
            }
        };
        Ok(PooledRecognitionResult {
            schema_version: wire.schema_version,
            status,
            complete: wire.complete,
            search_complete: wire.search_complete,
            internal_offset_cell_count: wire.internal_offset_cell_count,
            offset_cell_scan_complete: wire.offset_cell_scan_complete,
            diagnostic_truncation_applied_after_pooling: wire
                .diagnostic_truncation_applied_after_pooling,
            query_anchor_identity_available: wire.query_anchor_identity_available,
            anchor_identity_serialized: wire.anchor_identity_serialized,
            anchor_identity_scope: identity_static(
                &wire.anchor_identity_scope,
                ANCHOR_IDENTITY_SCOPE,
                "anchor identity scope",
            )?,
            aggregation_scope: identity_static(
                &wire.aggregation_scope,
                AGGREGATION_SCOPE,
                "aggregation scope",
            )?,
            recipe_identity: identity_static(&wire.recipe_identity, RECIPE_ID, "recipe identity")?,
            acceptance_policy_id: identity_static(
                &wire.acceptance_policy_id,
                PooledRecognitionPolicy::frozen().policy_id.as_str(),
                "acceptance policy id",
            )?,
            mode_limit: wire.mode_limit,
            windows: wire.windows,
        })
    }
}

/// Complete recognition outcome: the full search evidence plus the pooled
/// result (absent when no pooled windows were requested).
#[derive(Debug, Clone)]
pub struct PooledRecognition {
    pub search: Search,
    pub evidence: Option<PooledRecognitionResult>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DurationFloors {
    pub minimum_anchors: u64,
    pub minimum_occupied_time_bins: u32,
    pub minimum_distinct_competitor_ratio: f64,
    /// Unique-anchor margin floor (10-second calibrated floors only).
    #[serde(default)]
    pub minimum_unique_anchor_margin: Option<f64>,
    pub calibrated: bool,
    #[serde(default)]
    pub calibrated_from_negative_capture_count: Option<u32>,
    #[serde(default)]
    pub maximum_calibration_negative_best_mode_anchors: Option<u64>,
    #[serde(default)]
    pub maximum_calibration_negative_unique_anchor_margin: Option<f64>,
    #[serde(default)]
    pub inherited_from: Option<String>,
    #[serde(default)]
    pub negative_window_count: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcceptanceFloorsTable {
    #[serde(rename = "10")]
    pub ten: DurationFloors,
    #[serde(rename = "20")]
    pub twenty: DurationFloors,
    #[serde(rename = "30")]
    pub thirty: DurationFloors,
}

impl AcceptanceFloorsTable {
    pub fn for_duration(&self, seconds: u16) -> Option<&DurationFloors> {
        match seconds {
            10 => Some(&self.ten),
            20 => Some(&self.twenty),
            30 => Some(&self.thirty),
            _ => None,
        }
    }
}

/// Typed view of the vendored v3 acceptance policy.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PooledRecognitionPolicy {
    pub schema_version: u32,
    pub policy_id: String,
    pub status: String,
    pub extractor_identity: String,
    pub score_definition: String,
    pub baseline_policy_id: String,
    pub baseline_policy_sha256: String,
    pub calibration_method: String,
    pub requires_baseline_distinct_competitor_ratio: bool,
    pub acceptance: AcceptanceFloorsTable,
    pub completeness_required: bool,
    pub anchor_identity_available: bool,
    pub window_aggregation: String,
    pub calibration_capture_ids: Vec<String>,
}

impl PooledRecognitionPolicy {
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }

    /// The byte-identical vendored policy; parsed once.
    pub fn frozen() -> &'static PooledRecognitionPolicy {
        static POLICY: LazyLock<PooledRecognitionPolicy> = LazyLock::new(|| {
            PooledRecognitionPolicy::from_json(V3_POLICY_JSON.as_bytes())
                .expect("vendored pooled recognition v3 policy must parse")
        });
        &POLICY
    }
}

/// Run the complete-reference search and, when windows are requested, pool and
/// score them. This is the core-owned versioned recognition entry point.
pub fn recognize(
    index: &Index,
    pcm: &[i16],
    request: &PooledRecognitionRequest,
    cancel: &dyn Fn() -> bool,
) -> Result<PooledRecognition> {
    let search =
        index.search_pcm_with_policy(pcm, request.search_policy.unwrap_or_default(), cancel)?;
    let evidence = if request.windows.is_some() {
        Some(pool_exact_offset_windows(&search, request)?)
    } else {
        None
    };
    Ok(PooledRecognition { search, evidence })
}

#[cfg(test)]
#[path = "source_audio_pooled_tests.rs"]
mod tests;

/// Pool complete per-second exact-offset cells before the bounded raw
/// diagnostics are selected, then rank offset-mode hypotheses and evaluate the
/// v3 acceptance verdict per window. Windows are independent observations: no
/// evidence is combined across overlapping windows, and neighboring offset
/// bins remain alternatives. A request without windows pools nothing and
/// reports the demonstrated empty-window-count error.
pub fn pool_exact_offset_windows(
    search: &Search,
    request: &PooledRecognitionRequest,
) -> Result<PooledRecognitionResult> {
    let windows: &[PooledOffsetWindow] = request.windows.as_deref().unwrap_or(&[]);
    let mode_limit = usize::from(request.mode_limit.unwrap_or(DEFAULT_MODE_LIMIT as u8));
    ensure!(
        !windows.is_empty() && windows.len() <= MAX_WINDOWS,
        "invalid input: pooled offset window count must be 1..256"
    );
    ensure!(
        (1..=MAX_MODE_LIMIT).contains(&mode_limit),
        "invalid input: pooled offset mode limit must be 1..64"
    );
    ensure!(
        search
            .offsets
            .len()
            .checked_mul(windows.len())
            .is_some_and(|scans| scans <= MAX_WINDOW_CELL_SCANS),
        "invalid input: pooled offset cell-scan budget exceeded"
    );
    let query_seconds = search.query_sample_count as f64 / f64::from(RATE);
    let mut output_windows = Vec::with_capacity(windows.len());

    for window in windows {
        ensure!(
            matches!(window.duration_seconds, 10 | 20 | 30),
            "invalid input: pooled offset durations must be 10, 20 or 30 seconds"
        );
        let end_seconds = window
            .start_seconds
            .checked_add(u32::from(window.duration_seconds))
            .ok_or_else(|| anyhow::anyhow!("invalid input: pooled offset window overflow"))?;
        ensure!(
            f64::from(end_seconds) <= query_seconds + 1e-6,
            "invalid input: pooled offset window exceeds decoded query duration"
        );

        let mut by_offset: BTreeMap<i32, PooledOffsetMode> = BTreeMap::new();
        let mut pooled_offset_cell_count = 0usize;
        for evidence in &search.offsets {
            if evidence.time_bin >= window.start_seconds && evidence.time_bin < end_seconds {
                by_offset
                    .entry(evidence.offset_bin)
                    .or_default()
                    .add(evidence);
                pooled_offset_cell_count += 1;
            }
        }
        let mut ranked: Vec<(i32, PooledOffsetMode)> = by_offset.into_iter().collect();
        ranked.sort_by(|(left_bin, left), (right_bin, right)| {
            right
                .independent_anchor_count()
                .cmp(&left.independent_anchor_count())
                .then(left_bin.cmp(right_bin))
        });

        let best_competitor = ranked.first().and_then(|(best_bin, best)| {
            ranked
                .iter()
                .skip(1)
                .find(|(bin, _)| (i64::from(*bin) - i64::from(*best_bin)).abs() >= 2)
                .map(|(bin, mode)| {
                    let shared = best.shared_query_anchors(mode);
                    PooledDistinctCompetitor {
                        offset_bin: *bin,
                        offset_seconds: f64::from(*bin) * OFFSET_BIN_SECONDS,
                        independent_anchors: mode.independent_anchor_count(),
                        occupied_time_bins: mode.occupied_time_bins.len(),
                        shared_query_anchors_with_best: shared,
                        unique_query_anchors_outside_best: mode.independent_anchor_count() as usize
                            - shared,
                    }
                })
        });
        let best_anchor_margin = ranked.first().and_then(|(_, best)| {
            ranked
                .iter()
                .skip(1)
                .find(|(bin, _)| (i64::from(*bin) - i64::from(ranked[0].0)).abs() >= 2)
                .and_then(|(_, competitor)| {
                    (competitor.independent_anchor_count() > 0).then(|| {
                        ((best.independent_anchor_count() as f64
                            / competitor.independent_anchor_count() as f64)
                            * 10_000.0)
                            .round()
                            / 10_000.0
                    })
                })
        });
        let best_unique_anchor_margin = ranked.first().and_then(|(best_bin, best)| {
            ranked
                .iter()
                .skip(1)
                .find(|(bin, _)| (i64::from(*bin) - i64::from(*best_bin)).abs() >= 2)
                .and_then(|(_, competitor)| {
                    let shared = best.shared_query_anchors(competitor);
                    let best_unique = best.independent_anchor_count() as usize - shared;
                    let competitor_unique = competitor.independent_anchor_count() as usize - shared;
                    (competitor_unique > 0).then(|| {
                        ((best_unique as f64 / competitor_unique as f64) * 10_000.0).round()
                            / 10_000.0
                    })
                })
        });
        let include_support = request.include_mode_support.unwrap_or(false);
        let modes: Vec<PooledOffsetModeRow> = ranked
            .iter()
            .take(mode_limit)
            .map(|(offset_bin, mode)| {
                let shared_with_best = ranked
                    .first()
                    .map(|(_, best)| best.shared_query_anchors(mode))
                    .unwrap_or(0);
                PooledOffsetModeRow {
                    offset_bin: *offset_bin,
                    offset_seconds: f64::from(*offset_bin) * OFFSET_BIN_SECONDS,
                    independent_anchors: mode.independent_anchor_count(),
                    summed_cell_anchor_count: mode.summed_cell_anchor_count,
                    duplicate_anchor_votes_removed: mode.summed_cell_anchor_count
                        - mode.independent_anchor_count(),
                    occupied_time_bins: mode.occupied_time_bins.len(),
                    support_span_seconds: mode.support_span_seconds(),
                    rarity_weight: (mode.rarity_weight * 1_000_000.0).round() / 1_000_000.0,
                    frequency_diversity_median_per_time_bin: mode.median_frequency_diversity(),
                    saturated_anchors: mode.saturated_anchors,
                    shared_query_anchors_with_best: shared_with_best,
                    supporting_cells: if include_support {
                        Some(mode_support_cells(
                            &search.offsets,
                            window.start_seconds,
                            end_seconds,
                            *offset_bin,
                        ))
                    } else {
                        None
                    },
                }
            })
            .collect();
        let acceptance = evaluate_acceptance(
            search,
            window.duration_seconds,
            search.complete,
            &modes,
            best_anchor_margin,
            best_unique_anchor_margin,
        );
        output_windows.push(PooledWindowResult {
            start_seconds: window.start_seconds,
            duration_seconds: window.duration_seconds,
            end_seconds,
            complete: search.complete,
            internal_offset_cell_count: search.offsets.len(),
            offset_cells_scanned: search.offsets.len(),
            offset_cells_pooled_for_window: pooled_offset_cell_count,
            offset_cells_dropped: 0,
            ranked_offset_bin_count: ranked.len(),
            returned_offset_bin_count: modes.len(),
            best_distinct_competitor: best_competitor,
            best_anchor_margin,
            best_unique_anchor_margin,
            modes,
            acceptance,
        });
    }

    Ok(PooledRecognitionResult {
        schema_version: 1,
        status: if search.complete {
            "complete"
        } else {
            "incomplete"
        },
        complete: search.complete,
        search_complete: search.complete,
        internal_offset_cell_count: search.offsets.len(),
        offset_cell_scan_complete: true,
        diagnostic_truncation_applied_after_pooling: true,
        query_anchor_identity_available: true,
        anchor_identity_serialized: false,
        anchor_identity_scope: ANCHOR_IDENTITY_SCOPE,
        aggregation_scope: AGGREGATION_SCOPE,
        recipe_identity: RECIPE_ID,
        acceptance_policy_id: &PooledRecognitionPolicy::frozen().policy_id,
        mode_limit,
        windows: output_windows,
    })
}

/// Per-cell supporting evidence of one offset mode within a window: occupied
/// one-second bins with the deduplicated absolute query-anchor feature frames
/// that voted for the mode in that cell. Only computed when the request opts in
/// to `includeModeSupport`.
fn mode_support_cells(
    offsets: &[OffsetEvidence],
    window_start_seconds: u32,
    end_seconds: u32,
    offset_bin: i32,
) -> Vec<PooledModeSupportCell> {
    let mut cells: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    for evidence in offsets {
        if evidence.time_bin >= window_start_seconds
            && evidence.time_bin < end_seconds
            && evidence.offset_bin == offset_bin
        {
            cells
                .entry(evidence.time_bin)
                .or_default()
                .extend(evidence.query_anchor_frames.iter().copied());
        }
    }
    cells
        .into_iter()
        .map(|(time_bin, frames)| PooledModeSupportCell {
            time_bin,
            query_anchor_frames: frames.into_iter().collect(),
        })
        .collect()
}

#[derive(Default)]
struct PooledOffsetMode {
    summed_cell_anchor_count: u64,
    rarity_weight: f64,
    saturated_anchors: u64,
    query_anchor_frames: BTreeSet<u32>,
    occupied_time_bins: Vec<u32>,
    frequency_diversity_by_time_bin: Vec<u32>,
}

impl PooledOffsetMode {
    fn add(&mut self, evidence: &OffsetEvidence) {
        self.summed_cell_anchor_count += u64::from(evidence.independent_anchors);
        self.query_anchor_frames
            .extend(evidence.query_anchor_frames.iter().copied());
        self.rarity_weight += evidence.rarity_weight;
        self.saturated_anchors += u64::from(evidence.saturated_anchors);
        self.occupied_time_bins.push(evidence.time_bin);
        self.frequency_diversity_by_time_bin
            .push(evidence.frequency_diversity);
    }

    fn independent_anchor_count(&self) -> u64 {
        self.query_anchor_frames.len() as u64
    }

    fn shared_query_anchors(&self, other: &Self) -> usize {
        self.query_anchor_frames
            .intersection(&other.query_anchor_frames)
            .count()
    }

    fn support_span_seconds(&self) -> u32 {
        match (
            self.occupied_time_bins.first(),
            self.occupied_time_bins.last(),
        ) {
            (Some(first), Some(last)) => last - first + 1,
            _ => 0,
        }
    }

    fn median_frequency_diversity(&self) -> f64 {
        let mut values = self.frequency_diversity_by_time_bin.clone();
        values.sort_unstable();
        match values.len() {
            0 => 0.0,
            length if length % 2 == 1 => f64::from(values[length / 2]),
            length => f64::from(values[length / 2 - 1] + values[length / 2]) / 2.0,
        }
    }
}

/// Evaluate the vendored v3 acceptance gates for one window exactly like the
/// frozen scorer (`score_m1_anchor_identity_v2.py::policy_pass`): gates are
/// `>=` floors, a missing margin fails its gate, and an absent mode abstains.
/// Verdicts are calibrated only for the frozen recipe and calibrated floors;
/// everything else abstains with `duration_or_density_not_calibrated`.
fn evaluate_acceptance(
    search: &Search,
    duration_seconds: u16,
    complete: bool,
    modes: &[PooledOffsetModeRow],
    best_anchor_margin: Option<f64>,
    best_unique_anchor_margin: Option<f64>,
) -> PooledWindowAcceptance {
    let policy = PooledRecognitionPolicy::frozen();
    let floors = policy
        .acceptance
        .for_duration(duration_seconds)
        .expect("validated windows are 10, 20 or 30 seconds");
    let recipe_calibrated = search.recipe == Recipe::default();
    let mut gate_failure_reasons = Vec::new();
    if !complete {
        gate_failure_reasons.push(REASON_INCOMPLETE_EVIDENCE);
    }
    match modes.first() {
        None => gate_failure_reasons.push(REASON_NO_OFFSET_MODE),
        Some(best) => {
            if best.independent_anchors < floors.minimum_anchors {
                gate_failure_reasons.push(REASON_MINIMUM_ANCHORS);
            }
            if (best.occupied_time_bins as u64) < u64::from(floors.minimum_occupied_time_bins) {
                gate_failure_reasons.push(REASON_MINIMUM_OCCUPIED_TIME_BINS);
            }
            if !best_anchor_margin
                .is_some_and(|ratio| ratio >= floors.minimum_distinct_competitor_ratio)
            {
                gate_failure_reasons.push(REASON_MINIMUM_DISTINCT_COMPETITOR_RATIO);
            }
            if let Some(minimum_unique) = floors.minimum_unique_anchor_margin
                && !best_unique_anchor_margin.is_some_and(|ratio| ratio >= minimum_unique)
            {
                gate_failure_reasons.push(REASON_MINIMUM_UNIQUE_ANCHOR_MARGIN);
            }
        }
    }
    let verdict_calibrated = floors.calibrated && recipe_calibrated;
    let (accepted, failure_reasons) = if verdict_calibrated {
        (
            Some(gate_failure_reasons.is_empty()),
            std::mem::take(&mut gate_failure_reasons),
        )
    } else {
        (None, vec![REASON_NOT_CALIBRATED])
    };
    PooledWindowAcceptance {
        floors_calibrated: floors.calibrated,
        recipe_calibrated,
        accepted,
        failure_reasons,
        gate_failure_reasons,
    }
}
