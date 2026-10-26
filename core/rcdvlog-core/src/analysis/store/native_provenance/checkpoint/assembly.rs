//! Pure complete-clip assembly for an already paid native window checkpoint.
//!
//! This module has no database, filesystem, model, credential or provider
//! access. It consumes the exact versioned plan plus the complete typed window
//! facts retained by the checkpoint owner and produces the existing canonical
//! transcript value. Persistence and current accounting proof remain in the
//! finalizer.

use std::collections::HashSet;

use anyhow::{Result, ensure};

use crate::analysis::quality::DecodedSegment;
use crate::analysis::quality::native_stt::{
    NativeSttQualityDecision, NativeSttQualityReason, native_stt_repetition_is_safe,
    validate_native_mapped_candidates,
};
use crate::analysis::run_policy::RunAssetScope;
use crate::analysis::store::{StoredTranscript, TranscriptOutcomeReason, TranscriptStatus};
use crate::analysis::stt::dominant_language;
use crate::transcription::NativeSpeechPreparationEvidence;

use super::NativeTranscriptCheckpointWindow;
use crate::analysis::store::native_provenance::plan::{candidate_window_index, validate_plan};
use crate::analysis::store::native_provenance::types::NativeTranscriptWindowPlan;

/// Assemble the one truthful clip outcome from every planned native window.
///
/// All planned indexes must be present exactly once and in order. Per-window
/// observations and candidates are validated through the existing native
/// quality/source-map owners; their recorded decisions and metrics are never
/// rewritten. Whole-clip repetition is the only new gate and is measured over
/// the combined source-mapped candidates.
/// Narrow truthful identity shared by the live finalizer and read-only
/// currentness proof. This deliberately excludes VAD/model/resource facts that
/// are irrelevant to assembling already evaluated window evidence.
#[derive(Debug, Clone, Copy)]
pub(crate) struct NativeTranscriptAssemblyIdentity<'a> {
    pub asset_id: &'a str,
    pub recipe: &'a str,
    pub scope: &'a RunAssetScope,
}

/// The live finalizer supplies actual preparation evidence. Read-only
/// currentness supplies the narrower frozen identity below; both paths share
/// the same pure assembly without fabricating preparation facts.
pub(crate) fn assemble_native_transcript_completion(
    evidence: &NativeSpeechPreparationEvidence,
    plan: &NativeTranscriptWindowPlan,
    windows: &[NativeTranscriptCheckpointWindow],
) -> Result<StoredTranscript> {
    assemble_native_transcript_completion_for_source(
        NativeTranscriptAssemblyIdentity {
            asset_id: &evidence.asset_id,
            recipe: &evidence.recipe,
            scope: &evidence.scope,
        },
        plan,
        windows,
    )
}

/// Pure complete-clip assembly from existing transcript identity plus the exact
/// frozen source scope. No preparation, model, credential, database or provider
/// effect is required.
pub(crate) fn assemble_native_transcript_completion_for_source(
    identity: NativeTranscriptAssemblyIdentity<'_>,
    plan: &NativeTranscriptWindowPlan,
    windows: &[NativeTranscriptCheckpointWindow],
) -> Result<StoredTranscript> {
    validate_plan(plan, identity.scope.source_duration_ms)?;
    ensure!(
        windows.len() == plan.windows.len(),
        "native complete-clip assembly requires exactly one result per planned window"
    );

    let (mapped_candidates, provider_segment_count) = validate_complete_windows(plan, windows)?;
    let speech_ms = plan
        .windows
        .iter()
        .try_fold(0_u64, |total, window| {
            total.checked_add(u64::from(window.speech_ms()))
        })
        .ok_or_else(|| anyhow::anyhow!("native complete-clip speech duration overflowed"))?;

    let repetition_ratio = (!mapped_candidates.is_empty())
        .then(|| crate::analysis::quality::repetition_ratio(&mapped_candidates));
    let (status, outcome_reason, reject_reason) = clip_outcome(windows, repetition_ratio);
    let segments = if matches!(
        status,
        TranscriptStatus::Speech | TranscriptStatus::ReviewRequired
    ) {
        mapped_candidates
    } else {
        Vec::new()
    };
    let source_fingerprint = identity
        .scope
        .sampled_representation_fingerprint
        .clone()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("native complete-clip source fingerprint is missing"))?;
    Ok(StoredTranscript {
        asset_id: identity.asset_id.to_string(),
        recipe_key: identity.recipe.to_string(),
        source_fingerprint,
        status,
        outcome_reason,
        reject_reason: reject_reason.to_string(),
        primary_language: dominant_language(&segments),
        speech_seconds: speech_ms as f64 / 1_000.0,
        window_count: u32::try_from(windows.len())?,
        provider_segment_count,
        mean_token_probability: None,
        repetition_ratio,
        segments,
    })
}

fn validate_complete_windows(
    plan: &NativeTranscriptWindowPlan,
    windows: &[NativeTranscriptCheckpointWindow],
) -> Result<(Vec<DecodedSegment>, u32)> {
    let mut seen_requests = HashSet::new();
    let mut mapped_candidates: Vec<DecodedSegment> = Vec::new();
    let mut previous_end_ms = 0_u32;
    let mut provider_segment_count = 0_u32;

    for (expected_index, window) in windows.iter().enumerate() {
        ensure!(
            window.window_index as usize == expected_index,
            "native complete-clip assembly windows must be contiguous, ordered and appear exactly once"
        );
        window.observation.validate()?;
        super::validation::ensure_checkpoint_quality_is_typed(
            window.observation.quality.decision,
            window.observation.quality.reason,
        )?;
        ensure!(
            seen_requests.insert(window.observation.request_key.as_str()),
            "native complete-clip assembly duplicates an accounting request"
        );
        let planned_window = plan.windows.get(expected_index).ok_or_else(|| {
            anyhow::anyhow!("native complete-clip assembly window is outside its plan")
        })?;
        validate_native_mapped_candidates(
            window.observation.quality.decision,
            window.observation.quality.reason,
            &window.observation.reported_language,
            &window.candidates,
            planned_window,
        )?;

        provider_segment_count = provider_segment_count
            .checked_add(
                u32::try_from(
                    window
                        .observation
                        .quality
                        .metrics
                        .avg_logprob
                        .total_segments,
                )
                .map_err(|_| {
                    anyhow::anyhow!("native complete-clip provider segment count overflowed")
                })?,
            )
            .ok_or_else(|| {
                anyhow::anyhow!("native complete-clip provider segment count overflowed")
            })?;

        if matches!(
            window.observation.quality.decision,
            NativeSttQualityDecision::Accepted | NativeSttQualityDecision::ReviewRequired
        ) {
            for candidate in &window.candidates {
                ensure!(
                    candidate_window_index(plan, candidate)? == expected_index,
                    "native complete-clip candidate does not map to its planned source window"
                );
                ensure!(
                    previous_end_ms <= candidate.start_ms,
                    "native complete-clip candidates overlap or are out of source order"
                );
                previous_end_ms = candidate.end_ms;
                mapped_candidates.push(candidate.clone());
            }
        }
    }

    Ok((mapped_candidates, provider_segment_count))
}

/// Choose the parent outcome without rewriting any paid per-window fact.
fn clip_outcome(
    windows: &[NativeTranscriptCheckpointWindow],
    repetition_ratio: Option<f32>,
) -> (TranscriptStatus, TranscriptOutcomeReason, &'static str) {
    let mut rejected = windows
        .iter()
        .filter(|window| window.observation.quality.decision == NativeSttQualityDecision::Rejected)
        .map(|window| window.observation.quality.reason)
        .peekable();
    if windows.iter().all(|window| {
        window.observation.quality.reason == NativeSttQualityReason::EmptyProviderOutput
    }) {
        return (
            TranscriptStatus::NoAcceptedSpeech,
            TranscriptOutcomeReason::EmptyProviderOutput,
            NativeSttQualityReason::EmptyProviderOutput.code(),
        );
    }
    if rejected.peek().is_some() {
        let reason = rejected
            .find(|reason| *reason != NativeSttQualityReason::EmptyProviderOutput)
            .map(|reason| reason.code())
            .unwrap_or("mixed_empty_provider_output");
        return (
            TranscriptStatus::Rejected,
            TranscriptOutcomeReason::QualityRejected,
            reason,
        );
    }
    if !repetition_ratio.is_some_and(native_stt_repetition_is_safe) {
        return (
            TranscriptStatus::Rejected,
            TranscriptOutcomeReason::QualityRejected,
            NativeSttQualityReason::Repetition.code(),
        );
    }
    if windows.iter().any(|window| {
        window.observation.quality.decision == NativeSttQualityDecision::ReviewRequired
    }) {
        return (
            TranscriptStatus::ReviewRequired,
            TranscriptOutcomeReason::QualityEvidenceIncomplete,
            "",
        );
    }
    (
        TranscriptStatus::Speech,
        TranscriptOutcomeReason::Accepted,
        "",
    )
}

#[cfg(test)]
#[path = "assembly_tests.rs"]
mod tests;
