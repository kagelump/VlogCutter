//! Pure validation and semantic identity for a retained native window plan.
//!
//! The plan is a versioned, ordered `Vec<PackedWindow>` stored on the existing
//! transcript header. It contains no audio, provider body, path or financial
//! value. `window_index` on the existing native child rows selects one entry.

use anyhow::{Context, Result, ensure};
use serde::Serialize;

use crate::analysis::quality::DecodedSegment;
use crate::analysis::run_policy::{RunAssetScope, RunPolicy};
use crate::analysis::windows::PackedWindow;

use super::types::{
    MAX_NATIVE_TRANSCRIPT_PLANNED_WINDOWS, NATIVE_TRANSCRIPT_WINDOW_PLAN_SCHEMA_VERSION,
    NativeTranscriptWindowPlan,
};

#[derive(Serialize)]
struct NativeTranscriptWindowPlanSemantics<'a> {
    schema_version: &'a str,
    policy_id: &'a str,
    recipe_version: &'a str,
    asset_id: &'a str,
    source_identity: &'a str,
    source_fingerprint: &'a str,
    source_duration_ms: u32,
    representation: &'a str,
    representation_fingerprint: Option<&'a str>,
    sampled_representation_fingerprint: Option<&'a str>,
    time_mapping: &'a str,
    windows: &'a [PackedWindow],
}

/// Validate the structural source map. Windows are source ordered and
/// non-overlapping, each internally packed from offset zero, and every region
/// stays inside the approved source duration.
pub(super) fn validate_plan(
    plan: &NativeTranscriptWindowPlan,
    source_duration_ms: u32,
) -> Result<()> {
    ensure!(
        plan.schema_version == NATIVE_TRANSCRIPT_WINDOW_PLAN_SCHEMA_VERSION,
        "native transcript window plan schema version is unsupported"
    );
    ensure!(
        !plan.windows.is_empty() && plan.windows.len() <= MAX_NATIVE_TRANSCRIPT_PLANNED_WINDOWS,
        "native transcript window plan must contain a bounded positive number of windows"
    );
    let mut previous_source_end = 0_u32;
    for (index, window) in plan.windows.iter().enumerate() {
        crate::analysis::quality::native_stt::validate_native_window_mapping(
            window,
            source_duration_ms,
        )
        .with_context(|| format!("validate native transcript window plan entry {index}"))?;
        let first = window.regions.first().ok_or_else(|| {
            anyhow::anyhow!("native transcript window plan entry {index} is empty")
        })?;
        ensure!(
            first.source_start_ms >= previous_source_end,
            "native transcript window plan entries overlap or are out of source order"
        );
        let end = window
            .regions
            .last()
            .and_then(|region| region.source_start_ms.checked_add(region.duration_ms))
            .ok_or_else(|| {
                anyhow::anyhow!("native transcript window plan source time overflowed")
            })?;
        previous_source_end = end;
    }
    Ok(())
}

/// Domain-separated semantic fingerprint. It binds the versioned map to the
/// frozen policy/recipe and exact approved source identity. It is a cache and
/// representation proof only; it is never a request key or accounting identity.
pub(super) fn semantic_fingerprint(
    plan: &NativeTranscriptWindowPlan,
    policy: &RunPolicy,
    scope: &RunAssetScope,
) -> Result<String> {
    validate_plan(plan, scope.source_duration_ms)?;
    ensure!(
        policy.scope.iter().any(|entry| entry == scope),
        "native transcript window plan scope is absent from the frozen policy"
    );
    let semantics = NativeTranscriptWindowPlanSemantics {
        schema_version: &plan.schema_version,
        policy_id: &policy.policy_id,
        recipe_version: &policy.recipe_version,
        asset_id: &scope.asset_id,
        source_identity: &scope.source_identity,
        source_fingerprint: &scope.source_fingerprint,
        source_duration_ms: scope.source_duration_ms,
        representation: &scope.representation,
        representation_fingerprint: scope.representation_fingerprint.as_deref(),
        sampled_representation_fingerprint: scope.sampled_representation_fingerprint.as_deref(),
        time_mapping: &scope.time_mapping,
        windows: &plan.windows,
    };
    let bytes = serde_json::to_vec(&semantics)
        .context("serialize native transcript window plan semantics")?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rcdvlog-native-transcript-window-plan-v1\0");
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(&bytes);
    Ok(hasher.finalize().to_hex().to_string())
}

/// Parse a persisted plan and prove it still matches its stored fingerprint and
/// the frozen policy/source scope. Historical inspection uses this structural
/// proof only; it does not require today's tariff or readiness.
pub(super) fn parse_validated_plan(
    plan_json: &str,
    stored_fingerprint: &str,
    policy: &RunPolicy,
    scope: &RunAssetScope,
) -> Result<NativeTranscriptWindowPlan> {
    let plan: NativeTranscriptWindowPlan =
        serde_json::from_str(plan_json).context("parse persisted native transcript window plan")?;
    validate_plan(&plan, scope.source_duration_ms)?;
    ensure!(
        stored_fingerprint.len() == 64
            && stored_fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "persisted native transcript window plan fingerprint is malformed"
    );
    let expected = semantic_fingerprint(&plan, policy, scope)?;
    ensure!(
        expected == stored_fingerprint,
        "persisted native transcript window plan fingerprint does not match its frozen policy/source identity"
    );
    Ok(plan)
}

pub(super) fn plan_matches_windows(
    plan: &NativeTranscriptWindowPlan,
    windows: &[PackedWindow],
) -> bool {
    plan.windows == windows
}

/// Return the unique plan entry that owns this source span. A candidate must map
/// at both endpoints inside exactly one packed window; no overlap inference or
/// nearest-window guessing is permitted.
pub(super) fn candidate_window_index(
    plan: &NativeTranscriptWindowPlan,
    candidate: &DecodedSegment,
) -> Result<usize> {
    let mut matches = Vec::new();
    for (index, window) in plan.windows.iter().enumerate() {
        let start_mapped = crate::analysis::quality::native_stt::source_start_maps_to_window(
            window,
            candidate.start_ms,
        );
        let end_mapped = crate::analysis::quality::native_stt::source_end_maps_to_window(
            window,
            candidate.end_ms,
        );
        if start_mapped && end_mapped {
            matches.push(index);
        }
    }
    ensure!(
        matches.len() == 1,
        "native checkpoint candidate does not map to exactly one plan window"
    );
    Ok(matches[0])
}

/// Reconstruct candidates per plan window from the sole canonical
/// `transcript_segments` rows. This is a read projection; it writes nothing.
pub(super) fn reconstruct_candidates_by_window(
    plan: &NativeTranscriptWindowPlan,
    segments: &[DecodedSegment],
) -> Result<Vec<Vec<DecodedSegment>>> {
    let mut by_window = vec![Vec::new(); plan.windows.len()];
    let mut previous_end_ms = 0_u32;
    for (index, candidate) in segments.iter().enumerate() {
        ensure!(
            candidate.start_ms < candidate.end_ms && candidate.start_ms >= previous_end_ms,
            "native checkpoint candidate {index} is invalid, overlapping or out of source order"
        );
        let window_index = candidate_window_index(plan, candidate)
            .with_context(|| format!("map native checkpoint candidate {index}"))?;
        by_window[window_index].push(candidate.clone());
        previous_end_ms = candidate.end_ms;
    }
    Ok(by_window)
}
