use super::*;

pub(super) fn evaluate_checkpoint_input<'a>(
    input: NativeTranscriptCheckpointWindowInput<'a>,
    evidence: &NativeSpeechPreparationEvidence,
) -> Result<(
    &'a NativeSttRequest,
    &'a NativeSttAudioWindow,
    EvaluatedNativeCheckpointWindow,
)> {
    let (request, audio, evaluated) = match input {
        NativeTranscriptCheckpointWindowInput::Evaluated {
            request,
            audio,
            observation,
            candidates,
        } => (
            request,
            audio,
            EvaluatedNativeCheckpointWindow {
                observation,
                candidates,
            },
        ),
        NativeTranscriptCheckpointWindowInput::AccountedResult {
            request,
            audio,
            result,
        } => (
            request,
            audio,
            evaluate_native_checkpoint_result(result, audio)?,
        ),
        NativeTranscriptCheckpointWindowInput::PilotSample {
            request,
            audio,
            sample,
        } => (
            request,
            audio,
            evaluate_native_pilot_sample(
                sample,
                request,
                audio,
                &evidence.scope,
                &evidence.recipe,
            )?,
        ),
    };
    Ok((request, audio, evaluated))
}

/// Evaluate exactly one typed provider result against the exact supplied audio
/// window. Protocol-unusable replies have no quality observation and must be
/// handled by the owning run/recovery boundary, not fabricated into a
/// checkpoint.
pub(crate) fn evaluate_native_checkpoint_result(
    result: &NativeSttAccountedResult,
    audio: &NativeSttAudioWindow,
) -> Result<EvaluatedNativeCheckpointWindow> {
    let NativeSttOutcome::Usable(transcript) = result.outcome() else {
        bail!(
            "native checkpoint cannot fabricate a quality result for a protocol-unusable paid reply; run recovery is required"
        );
    };
    let evaluation = evaluate_native_stt_quality(transcript, audio);
    let evaluated = evaluated_window_from_evaluation(
        result.request_key.clone(),
        result.latency_ms(),
        evaluation,
    )?;
    Ok(evaluated)
}

pub(super) fn evaluated_window_from_evaluation(
    request_key: String,
    latency_ms: u64,
    evaluation: NativeSttQualityEvaluation,
) -> Result<EvaluatedNativeCheckpointWindow> {
    ensure_checkpoint_quality_is_typed(evaluation.decision, evaluation.reason)?;
    let quality = NativeSttWindowQualityObservation::from_evaluation(&evaluation)?;
    let observation = NativeSttWindowObservation {
        request_key,
        latency_ms,
        reported_language: evaluation.reported_language.clone(),
        quality,
    };
    observation.validate()?;
    Ok(EvaluatedNativeCheckpointWindow {
        observation,
        candidates: evaluation.segments,
    })
}

pub(super) fn evaluate_native_pilot_sample(
    sample: &crate::analysis::run_policy::NativePilotPaidSample,
    request: &NativeSttRequest,
    audio: &NativeSttAudioWindow,
    scope: &RunAssetScope,
    recipe: &str,
) -> Result<EvaluatedNativeCheckpointWindow> {
    ensure!(
        &sample.scope == scope,
        "native pilot sample scope does not match the exact current preparation"
    );
    ensure!(
        sample.recipe_version == recipe,
        "native pilot sample recipe does not match the exact current preparation"
    );
    let current_payload = analysis_native_stt_payload_fingerprint(request, audio)?;
    ensure!(
        sample.payload_fingerprint == current_payload,
        "native pilot sample payload does not match the exact current checkpoint audio/request"
    );
    ensure!(
        sample.window == *audio.window(),
        "native pilot sample window does not match the exact current checkpoint audio map"
    );
    ensure!(
        sample.requested_provider == request.provider()
            && sample.requested_model == request.model()
            && sample.quality_recipe_version == NATIVE_STT_QUALITY_RECIPE_VERSION,
        "native pilot sample provider/model/quality recipe does not match the exact request"
    );
    let crate::analysis::run_policy::NativePilotSampleOutcome::Evaluated {
        reported_language,
        quality,
        candidates,
    } = &sample.outcome
    else {
        bail!(
            "protocol-unusable native pilot sample cannot seed a quality checkpoint; run recovery is required"
        );
    };
    ensure_checkpoint_quality_is_typed(quality.decision, quality.reason)?;
    let observation = NativeSttWindowObservation {
        request_key: sample.request_key.clone(),
        latency_ms: sample.latency_ms,
        reported_language: reported_language.clone(),
        quality: quality.clone(),
    };
    observation.validate()?;
    Ok(EvaluatedNativeCheckpointWindow {
        observation,
        candidates: candidates.clone(),
    })
}
