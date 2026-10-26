//! Pure validation for a native transcript artifact and its window evidence.

use anyhow::{Result, bail, ensure};

use crate::analysis::quality::native_stt::{NativeSttQualityDecision, NativeSttQualityReason};
use crate::analysis::run_policy::{RunAssetScope, RunPolicy, RunPolicyParameters};

use super::super::{StoredTranscript, TranscriptOutcomeReason, TranscriptStatus};
use super::types::NativeSttWindowObservation;

/// Validate the status/reason/request-count relationship for a native artifact.
///
/// Provider output that retained candidates must reference at least one exact
/// paid request. Verified local silence/audio absence may use an approved native
/// policy with no request and must not invent speech duration, segment counts or
/// serving provider/model. A generic failure is not promoted into native
/// provenance in this slice.
pub(super) fn validate_transcript_shape(
    transcript: &StoredTranscript,
    observations: &[NativeSttWindowObservation],
) -> Result<()> {
    ensure!(
        transcript.speech_seconds.is_finite() && transcript.speech_seconds >= 0.0,
        "native transcript speech duration must be finite and non-negative"
    );
    ensure!(
        transcript.mean_token_probability.is_none(),
        "native transcript header must not claim an unmeasured arithmetic token probability"
    );
    for (index, segment) in transcript.segments.iter().enumerate() {
        ensure!(
            segment.mean_token_probability.is_none(),
            "native transcript segment {index} must not claim an unmeasured arithmetic token probability"
        );
        ensure!(
            segment.start_ms < segment.end_ms,
            "native transcript segment {index} must have a positive source-time span"
        );
        ensure!(
            !segment.text.trim().is_empty(),
            "native transcript segment {index} must not contain empty text"
        );
    }
    if let Some(ratio) = transcript.repetition_ratio {
        ensure!(
            ratio.is_finite() && (0.0..=1.0).contains(&ratio),
            "native transcript repetition ratio must be finite and in [0, 1]"
        );
    }

    let request_count = observations.len();
    match transcript.status {
        TranscriptStatus::NoAudio | TranscriptStatus::NoSpeech => {
            let expected_reason = if transcript.status == TranscriptStatus::NoAudio {
                TranscriptOutcomeReason::NoAudioStream
            } else {
                TranscriptOutcomeReason::NoDetectedSpeech
            };
            ensure!(
                transcript.outcome_reason == expected_reason,
                "verified native silence/audio evidence must use its exact measured outcome reason, not {}",
                transcript.outcome_reason.as_str()
            );
            ensure!(
                request_count == 0,
                "silent/no-audio native evidence cannot acquire provider window requests"
            );
            ensure!(
                transcript.window_count == 0
                    && transcript.provider_segment_count == 0
                    && transcript.speech_seconds == 0.0
                    && transcript.segments.is_empty(),
                "silent/no-audio native evidence must have zero provider windows, segments and speech duration"
            );
        }
        TranscriptStatus::Speech
        | TranscriptStatus::NoAcceptedSpeech
        | TranscriptStatus::Rejected
        | TranscriptStatus::ReviewRequired => {
            ensure!(
                request_count > 0,
                "provider-backed native transcript status {} requires at least one exact request",
                transcript.status.as_str()
            );
            ensure!(
                transcript.window_count as usize == request_count,
                "native transcript window count must equal its exact accounting request observations"
            );

            match transcript.status {
                TranscriptStatus::Speech => {
                    ensure!(
                        transcript.outcome_reason == TranscriptOutcomeReason::Accepted,
                        "accepted native speech must use the accepted outcome reason"
                    );
                    ensure!(
                        !transcript.segments.is_empty(),
                        "accepted native speech must retain at least one accepted segment"
                    );
                    ensure!(
                        observations
                            .iter()
                            .all(|observation| observation.quality.decision
                                == NativeSttQualityDecision::Accepted),
                        "accepted native speech requires every provider window to have accepted quality evidence"
                    );
                }
                TranscriptStatus::ReviewRequired => {
                    ensure!(
                        transcript.outcome_reason
                            == TranscriptOutcomeReason::QualityEvidenceIncomplete,
                        "review_required native transcript must use quality_evidence_incomplete"
                    );
                    ensure!(
                        !transcript.segments.is_empty(),
                        "review_required native transcript must retain candidate segments"
                    );
                    ensure!(
                        observations
                            .iter()
                            .any(|observation| observation.quality.decision
                                == NativeSttQualityDecision::ReviewRequired),
                        "review_required native transcript requires review-required quality evidence"
                    );
                    ensure!(
                        observations
                            .iter()
                            .all(|observation| observation.quality.decision
                                != NativeSttQualityDecision::Rejected),
                        "review_required native transcript cannot hide a known-bad provider window"
                    );
                }
                TranscriptStatus::Rejected => {
                    ensure!(
                        transcript.outcome_reason == TranscriptOutcomeReason::QualityRejected,
                        "rejected native transcript must use the quality_rejected outcome reason"
                    );
                    ensure!(
                        transcript.segments.is_empty(),
                        "rejected native output must not retain candidate text"
                    );
                    ensure!(
                        observations
                            .iter()
                            .any(|observation| observation.quality.decision
                                == NativeSttQualityDecision::Rejected),
                        "rejected native transcript requires rejected quality evidence"
                    );
                }
                TranscriptStatus::NoAcceptedSpeech => {
                    ensure!(
                        transcript.outcome_reason == TranscriptOutcomeReason::EmptyProviderOutput,
                        "no_accepted_speech must remain distinct from verified silence and use empty_provider_output"
                    );
                    ensure!(
                        transcript.segments.is_empty(),
                        "no-accepted-speech native output must not retain candidate text"
                    );
                    ensure!(
                        observations
                            .iter()
                            .all(|observation| observation.quality.decision
                                == NativeSttQualityDecision::Rejected
                                && observation.quality.reason
                                    == NativeSttQualityReason::EmptyProviderOutput),
                        "no_accepted_speech requires empty provider output in every window; quality rejection and review evidence remain distinct"
                    );
                }
                TranscriptStatus::NoAudio
                | TranscriptStatus::NoSpeech
                | TranscriptStatus::Failed => unreachable!("handled above"),
            }
            if matches!(
                transcript.status,
                TranscriptStatus::Speech | TranscriptStatus::ReviewRequired
            ) {
                ensure!(
                    transcript.speech_seconds > 0.0,
                    "provider-backed native speech must record positive detected speech duration"
                );
            }
        }
        TranscriptStatus::Failed => bail!(
            "failed/pre-dispatch native evidence must remain ordinary local/failed output and cannot claim paid provenance"
        ),
    }
    Ok(())
}

/// Validate that retained header segments stay inside the approved source
/// duration and preserve ordered, non-overlapping source-time semantics.
pub(super) fn validate_segment_source_bounds(
    transcript: &StoredTranscript,
    source_duration_ms: u32,
) -> Result<()> {
    let mut previous_end_ms = 0_u32;
    for (index, segment) in transcript.segments.iter().enumerate() {
        ensure!(
            segment.start_ms >= previous_end_ms && segment.end_ms <= source_duration_ms,
            "native transcript segment {index} falls outside approved source bounds or overlaps"
        );
        previous_end_ms = segment.end_ms;
    }
    Ok(())
}

/// Validate the frozen policy is an authorized audio-only native contract and
/// matches the artifact's deterministic recipe.
pub(super) fn validate_native_policy(
    policy: &RunPolicy,
    transcript: &StoredTranscript,
) -> Result<()> {
    validate_native_policy_recipe(policy, &transcript.recipe_key)
}

pub(super) fn validate_native_policy_recipe(policy: &RunPolicy, recipe_key: &str) -> Result<()> {
    ensure!(
        policy.kind == crate::analysis::run_policy::AnalysisRunKind::NativeSpeechTranscription
            && policy.payload == crate::analysis::run_policy::AnalysisPayload::Audio
            && policy.readiness.ready_for_authorization,
        "native transcript save requires an authorized audio-only native speech policy"
    );
    ensure!(
        policy.allowed_models == vec![policy.primary_model.clone()]
            && !policy.allow_fallback
            && policy.permitted_fallbacks.is_empty(),
        "native transcript policy must select exactly one provider-qualified model with no fallback"
    );
    ensure!(
        policy.provider_cost_bound_contract
            == crate::analysis::openrouter_stt::native_stt_cost_bound_contract_id(),
        "native transcript policy is not bound to the registered native cost contract"
    );
    let RunPolicyParameters::NativeSpeechTranscription {
        language,
        provider_policy,
        ..
    } = &policy.parameters
    else {
        bail!("native transcript policy is missing native speech parameters");
    };
    let expected_policy_request = crate::analysis::openrouter_stt::NativeSttRequest::new(
        policy.provider.clone(),
        policy.primary_model.clone(),
        language.clone(),
        0.0,
        *provider_policy,
    )
    .map_err(|error| {
        anyhow::anyhow!("reconstruct frozen native policy recipe semantics: {error}")
    })?;
    let expected_recipe =
        crate::analysis::store::native_stt_transcript_recipe(&expected_policy_request)?;
    ensure!(
        policy.recipe_version == expected_recipe,
        "frozen native policy recipe is not the deterministic native transcript recipe"
    );
    ensure!(
        recipe_key == expected_recipe,
        "native transcript header recipe does not match the frozen native policy recipe"
    );
    Ok(())
}

/// Validate the approved source scope against the artifact's exact fingerprint
/// and duration. Filesystem/media freshness remains the runtime caller's
/// responsibility; this SQL owner only compares canonical approved values.
pub(super) fn validate_scope(
    scope: &RunAssetScope,
    asset_fingerprint: &str,
    asset_duration_ms: u32,
    transcript: &StoredTranscript,
) -> Result<()> {
    ensure!(
        !asset_fingerprint.trim().is_empty(),
        "native transcript asset source fingerprint is missing"
    );
    ensure!(
        scope.source_fingerprint == asset_fingerprint
            && scope.source_duration_ms == asset_duration_ms
            && scope.representation == crate::analysis::framepack::REPRESENTATION
            && !scope.source_identity.trim().is_empty()
            && scope.sampled_representation_fingerprint.as_deref()
                == Some(transcript.source_fingerprint.as_str()),
        "native transcript source or derived-representation fingerprint is stale"
    );
    validate_segment_source_bounds(transcript, scope.source_duration_ms)
}
