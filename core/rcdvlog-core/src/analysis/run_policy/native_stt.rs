//! Native speech-transcription policy helpers.
//!
//! This leaf has no provider I/O and never loads an ASR or VAD model. It binds
//! the registered native duration tariff to a conservative pre-VAD window count
//! so the accounted pilot leaf can reserve the same full-window units without
//! inventing a provider charge or constructing a fake audio request. B3 exposes
//! one explicit single-window accounted request. The bounded one-shot native
//! capability pilot now consumes this leaf; native bulk and targeted retry
//! remain separate future slices.

use crate::analysis::openrouter_stt::NativeSttModelSpec;
use crate::analysis::windows::WINDOW_MS;
use anyhow::{Result, ensure};

use super::{
    AnalysisPayload, AnalysisRunKind, CapabilityCheck, CapabilityStatus, RunAssetScope, RunPolicy,
    RunPolicyBlocker, RunPolicyParameters, RunPolicyRequest,
};
use crate::analysis::openrouter_stt::{
    NativeSttRequest, OPENROUTER_STT_PROVIDER, resolve_native_stt_model, validate_iso639_1,
};
use crate::analysis::store::native_stt_transcript_recipe;

/// An explicit native speech kind has one authoritative provider/model pair.
/// No fallback or extra allowlist is inferred, even before the request is
/// validated, so a blocked proposal cannot accidentally carry a chat model.
pub(super) fn canonical_native_models(request: &RunPolicyRequest) -> Vec<String> {
    // Even a blocked proposal only carries the one explicitly selected native
    // model. Extra request allowlist entries remain visible to validation and
    // readiness, never as a latent fallback path in the policy.
    vec![request.primary_model.clone()]
}

/// Native protocol capability checks deliberately distinguish the registered
/// provider contract from an observed high-level pilot/runtime capability. B3
/// makes the single-window accounted request leaf real; observed native pilot
/// and bulk workflow capability remain unavailable.
fn registered_protocol_checks(request: &RunPolicyRequest) -> Vec<CapabilityCheck> {
    let mut checks = vec![CapabilityCheck {
        requirement: "provider.native_stt".into(),
        status: if request.provider == OPENROUTER_STT_PROVIDER {
            CapabilityStatus::Passed
        } else {
            CapabilityStatus::Failed
        },
        detail: if request.provider == OPENROUTER_STT_PROVIDER {
            "explicit OpenRouter native audio-transcription provider selected".into()
        } else {
            "native speech requires the explicitly registered OpenRouter transcription provider"
                .into()
        },
    }];

    match resolve_native_stt_model(&request.provider, &request.primary_model) {
        Ok(spec) => {
            checks.push(CapabilityCheck {
                requirement: "native_stt.registered_protocol".into(),
                status: CapabilityStatus::Passed,
                detail: format!(
                    "registered provider-qualified native endpoint; response_format={} timestamp_granularities={:?} input_formats={:?}",
                    spec.response_format, spec.timestamp_granularities, spec.supported_input_formats
                ),
            });
            checks.push(CapabilityCheck {
                requirement: "native_stt.timestamps".into(),
                status: if spec.timestamp_granularities.contains(&"segment") {
                    CapabilityStatus::Passed
                } else {
                    CapabilityStatus::Failed
                },
                detail: "segment source timestamps are required and validated after the response".into(),
            });
        }
        Err(_) => checks.push(CapabilityCheck {
            requirement: "native_stt.registered_protocol".into(),
            status: CapabilityStatus::Failed,
            detail: "provider/model is not a verified native audio-transcription contract; a chat model identifier is not accepted".into(),
        }),
    }

    checks
}

pub(super) fn native_stt_capability_checks(
    request: &RunPolicyRequest,
    allowed: &[String],
) -> Vec<CapabilityCheck> {
    let mut checks = registered_protocol_checks(request);
    let data_policy_ok = match &request.parameters {
        RunPolicyParameters::NativeSpeechTranscription {
            provider_policy, ..
        } => provider_policy.validate().is_ok(),
        _ => false,
    };
    checks.push(CapabilityCheck {
        requirement: "native_stt.data_policy".into(),
        status: if data_policy_ok {
            CapabilityStatus::Passed
        } else {
            CapabilityStatus::Failed
        },
        detail: "only explicit supported zdr and data_collection provider fields are admitted"
            .into(),
    });

    checks.push(CapabilityCheck {
        requirement: "native_stt.audio_only_payload".into(),
        status: if request.payload == AnalysisPayload::Audio {
            CapabilityStatus::Passed
        } else {
            CapabilityStatus::Failed
        },
        detail: "native transcription accepts bounded derived audio only; frames, transcripts and vocabulary are not sent".into(),
    });

    if let RunPolicyParameters::NativeSpeechTranscription { language, .. } = &request.parameters {
        checks.push(CapabilityCheck {
            requirement: "native_stt.language".into(),
            status: match language {
                None => CapabilityStatus::NotRequired,
                Some(language) if validate_iso639_1(language).is_ok() => CapabilityStatus::Passed,
                Some(_) => CapabilityStatus::Failed,
            },
            detail: "language, when selected, must be an explicit lowercase ISO 639-1 code".into(),
        });
    } else {
        checks.push(CapabilityCheck {
            requirement: "native_stt.language".into(),
            status: CapabilityStatus::Failed,
            detail: "native request is missing its explicit native parameters".into(),
        });
    }

    let requested_allowlist_is_primary_only = request
        .allowed_models
        .iter()
        .all(|model| model == &request.primary_model);
    let no_fallback = !request.allow_fallback
        && requested_allowlist_is_primary_only
        && allowed.len() == 1
        && allowed
            .first()
            .is_some_and(|model| model == &request.primary_model);
    checks.push(CapabilityCheck {
        requirement: "native_stt.no_fallback".into(),
        status: if no_fallback {
            CapabilityStatus::Passed
        } else {
            CapabilityStatus::Failed
        },
        detail: "native transcription admits exactly the selected provider-qualified model and has no fallback".into(),
    });

    checks.push(CapabilityCheck {
        requirement: "native_stt.temperature".into(),
        status: CapabilityStatus::NotRequired,
        detail: format!(
            "native transcription uses fixed temperature {} and exposes no sampling configuration",
            NATIVE_STT_TEMPERATURE
        ),
    });
    checks.push(CapabilityCheck {
        requirement: "native_stt.local_vad_recipe".into(),
        status: CapabilityStatus::NotRequired,
        detail: "the proposal binds the shared Silero VAD and 30-second packing recipe; proposal-time file presence is not inference evidence".into(),
    });
    checks.push(CapabilityCheck {
        requirement: "native_stt.accounted_runtime".into(),
        status: CapabilityStatus::Passed,
        detail: "one supplied bounded native window can cross the shared accounted request leaf with canonical reservation, dispatch marker and receipt settlement".into(),
    });
    checks.push(CapabilityCheck {
        requirement: "native_stt.high_level_pilot".into(),
        status: CapabilityStatus::Passed,
        detail: "the bounded core native capability pilot is implemented through the same session, authorization, local VAD preparation, accounting and projection owners; it sends at most one supplied speech window and observed endpoint capability remains policy-specific".into(),
    });
    checks
}

pub(super) fn native_stt_blockers(request: &RunPolicyRequest) -> Vec<RunPolicyBlocker> {
    let mut blockers = Vec::new();
    if resolve_native_stt_model(&request.provider, &request.primary_model).is_err() {
        blockers.push(RunPolicyBlocker {
            code: "unsupported_native_stt_model".into(),
            message:
                "selected provider/model is not a verified native audio-transcription contract"
                    .into(),
            asset_id: None,
        });
    }
    if request.payload != AnalysisPayload::Audio {
        blockers.push(RunPolicyBlocker {
            code: "unsupported_payload".into(),
            message: "native speech transcription requires the audio-only payload".into(),
            asset_id: None,
        });
    }
    if request.include_project_vocabulary {
        blockers.push(RunPolicyBlocker {
            code: "unsupported_payload_context".into(),
            message: "native speech transcription cannot attach project vocabulary".into(),
            asset_id: None,
        });
    }
    let has_extra_allowlist = request
        .allowed_models
        .iter()
        .any(|model| model != &request.primary_model);
    if request.allow_fallback || has_extra_allowlist {
        blockers.push(RunPolicyBlocker {
            code: "unsupported_model_allowlist".into(),
            message: "native speech transcription admits exactly one selected model and no fallback or extra allowlist".into(),
            asset_id: None,
        });
    }
    if let RunPolicyParameters::NativeSpeechTranscription {
        language,
        provider_policy,
        ..
    } = &request.parameters
    {
        if language
            .as_deref()
            .is_some_and(|language| validate_iso639_1(language).is_err())
        {
            blockers.push(RunPolicyBlocker {
                code: "unsupported_language".into(),
                message: "native STT language must be an explicit lowercase ISO 639-1 code".into(),
                asset_id: None,
            });
        }
        if provider_policy.validate().is_err() {
            blockers.push(RunPolicyBlocker {
                code: "unsupported_data_policy".into(),
                message: "native STT requires an explicit supported zdr and/or data_collection provider policy".into(),
                asset_id: None,
            });
        }
    } else {
        blockers.push(RunPolicyBlocker {
            code: "unsupported_native_parameters".into(),
            message: "native speech request is missing its explicit native parameters".into(),
            asset_id: None,
        });
    }
    blockers
}

/// The native protocol is fixed at deterministic transcription; no caller can
/// tune sampling or turn it into a chat request.
pub(super) const NATIVE_STT_TEMPERATURE: f64 = 0.0;

/// Honest version labels for the native endpoint. These are protocol/transport
/// identities, not a fabricated chat prompt or JSON-schema request.
pub(super) const NATIVE_STT_PROMPT_VERSION: &str = "native-stt.no-chat-prompt.v1";
pub(super) const NATIVE_STT_OUTPUT_SCHEMA_VERSION: &str =
    "openrouter.audio-transcriptions.verbose-json.segment.v1";
/// Stable prefix retained only for blocked/unregistered proposal identities.
/// A valid native proposal uses the transcript owner's semantic digest instead.
pub(super) const NATIVE_STT_RECIPE_VERSION: &str =
    "native-stt.silero-v5.pack-30s.full-window-reservation.v1";

/// Recipe identity for a native proposal, built only from the validated request
/// semantics. Invalid/unregistered proposals keep a stable blocked placeholder
/// and their existing structured blockers; they never acquire a valid recipe or
/// runtime capability.
pub(super) fn native_stt_recipe_version(request: &RunPolicyRequest) -> Result<String> {
    let placeholder = || format!("{NATIVE_STT_RECIPE_VERSION}:unregistered");
    let RunPolicyParameters::NativeSpeechTranscription {
        language,
        provider_policy,
        ..
    } = &request.parameters
    else {
        return Ok(placeholder());
    };
    let native_request = NativeSttRequest::new(
        request.provider.clone(),
        request.primary_model.clone(),
        language.clone(),
        NATIVE_STT_TEMPERATURE,
        *provider_policy,
    );
    match native_request {
        Ok(native_request) => native_stt_transcript_recipe(&native_request),
        Err(_) => Ok(placeholder()),
    }
}

/// Build the single explicit native request from the already frozen policy.
///
/// This is the only production constructor used by the pilot and local
/// preparation owners. Provider, model, language, fixed temperature and provider
/// data policy all come from the frozen policy; no chat model, fallback, sampling
/// option or default is invented here.
pub(crate) fn native_stt_request_from_policy(policy: &RunPolicy) -> Result<NativeSttRequest> {
    ensure!(
        policy.kind == AnalysisRunKind::NativeSpeechTranscription,
        "authorized policy is not native speech transcription"
    );
    ensure!(
        policy.payload == AnalysisPayload::Audio,
        "authorized native speech policy payload is not audio"
    );
    ensure!(
        !policy.allow_fallback
            && policy.permitted_fallbacks.is_empty()
            && policy.allowed_models == vec![policy.primary_model.clone()],
        "authorized native speech policy must select exactly one model without fallback"
    );
    ensure!(
        policy.readiness.ready_for_authorization && policy.readiness.budget_ready,
        "authorized native speech policy is not ready"
    );
    ensure!(
        policy.budget_usd.is_finite() && policy.budget_usd > 0.0,
        "authorized native speech budget is invalid"
    );
    ensure!(
        policy.expected_cost_usd.is_finite()
            && policy.expected_cost_usd >= 0.0
            && policy.expected_cost_usd <= policy.budget_usd,
        "authorized native speech cost exceeds its budget"
    );
    ensure!(
        policy.provider_cost_bound_contract == crate::native_stt_cost_bound_contract_id(),
        "authorized native speech provider cost contract changed"
    );
    let RunPolicyParameters::NativeSpeechTranscription {
        language,
        provider_policy,
        ..
    } = &policy.parameters
    else {
        anyhow::bail!("authorized policy is missing native speech parameters");
    };
    NativeSttRequest::new(
        policy.provider.clone(),
        policy.primary_model.clone(),
        language.clone(),
        NATIVE_STT_TEMPERATURE,
        *provider_policy,
    )
}

/// Native bulk remains a separate future owner. This warning is shown alongside
/// the now-implemented one-shot pilot so callers cannot infer that bulk or
/// adapters are available.
pub(super) const NATIVE_STT_BULK_UNAVAILABLE_WARNING: &str = "native STT bounded capability pilot is implemented, but native bulk execution, targeted retry and CLI/desktop native selection remain unavailable";

/// Conservative upper bound on packed speech windows for a positive source
/// duration. Greedy packing cannot produce more than
/// `floor(2 * duration_ms / WINDOW_MS) + 1` windows. Before local VAD runs the
/// actual speech count is unknown, so the estimate deliberately uses this
/// source-duration bound rather than pretending a transcript or audio probe
/// already observed speech.
pub(super) fn max_packed_windows_upper_bound(duration_ms: u32) -> u64 {
    if duration_ms == 0 {
        return 0;
    }
    (2 * u64::from(duration_ms)) / u64::from(WINDOW_MS) + 1
}

/// Expected full-window native STT cost for the exact proposed asset scope.
///
/// The native registry bills a full registered window at an external duration
/// tariff; the quote is therefore windows x full-window seconds x rate. It is a
/// conservative admission estimate shared by pilot and bulk, not a hard provider
/// invoice cap.
pub(super) fn expected_cost_usd(
    scope: &[RunAssetScope],
    spec: &NativeSttModelSpec,
) -> Result<(f64, String)> {
    let bulk_windows = scope.iter().try_fold(0_u64, |total, asset| {
        total
            .checked_add(max_packed_windows_upper_bound(asset.source_duration_ms))
            .ok_or_else(|| anyhow::anyhow!("native STT estimated window count overflowed"))
    })?;
    let pilot_windows = u64::from(!scope.is_empty());
    let windows = bulk_windows
        .checked_add(pilot_windows)
        .ok_or_else(|| anyhow::anyhow!("native STT estimated window count overflowed"))?;
    let billable_seconds_max = spec.max_window_ms.div_ceil(1_000);
    let duration_price_usd_per_second =
        spec.duration_price_micro_usd_per_second as f64 / 1_000_000.0;
    let expected_cost_usd =
        windows as f64 * billable_seconds_max as f64 * duration_price_usd_per_second;
    ensure!(
        expected_cost_usd.is_finite() && expected_cost_usd >= 0.0,
        "native STT expected cost is invalid"
    );
    let cost_basis = format!(
        "conservative native STT duration reservation: {windows} total windows ({bulk_windows} bulk packed windows, pre-VAD upper bound from source duration, plus {pilot_windows} separate capability-pilot window), each reserving the full {}ms registered window at ${duration_price_usd_per_second:.7}/second; pilot and bulk share the same authorized ceiling; the external duration tariff is not a wire-enforced hard invoice cap",
        spec.max_window_ms
    );
    Ok((expected_cost_usd, cost_basis))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::windows::{SpeechRegion, pack_windows};

    #[test]
    fn source_duration_window_bound_is_conservative() {
        assert_eq!(max_packed_windows_upper_bound(0), 0);
        assert_eq!(max_packed_windows_upper_bound(1), 1);
        assert_eq!(max_packed_windows_upper_bound(WINDOW_MS), 3);
        assert_eq!(max_packed_windows_upper_bound(WINDOW_MS + 1), 3);
        assert_eq!(max_packed_windows_upper_bound(u32::MAX), 286_332);
    }

    #[test]
    fn greedy_packed_windows_never_exceed_source_duration_bound() {
        let mut state = 0x9e37_79b9_u32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state
        };
        for source_duration_ms in [
            1_u32, 29_999, 30_000, 30_001, 60_000, 60_001, 120_000, 900_000,
        ] {
            for _ in 0..128 {
                let mut cursor = 0_u32;
                let mut regions = Vec::new();
                while cursor < source_duration_ms && regions.len() < 64 {
                    let gap = next() % 997;
                    let start = cursor.saturating_add(gap).min(source_duration_ms);
                    if start >= source_duration_ms {
                        break;
                    }
                    let length = 1 + next() % 45_000;
                    let end = start.saturating_add(length).min(source_duration_ms);
                    if end > start {
                        regions.push(SpeechRegion {
                            start_ms: start,
                            end_ms: end,
                        });
                    }
                    cursor = end;
                }
                let packed = pack_windows(&regions);
                assert!(
                    packed.len() as u64 <= max_packed_windows_upper_bound(source_duration_ms),
                    "packed {} windows for duration {}; regions={regions:?}",
                    packed.len(),
                    source_duration_ms
                );
            }
        }
    }
}
