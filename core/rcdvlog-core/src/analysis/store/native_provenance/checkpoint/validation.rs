use super::*;

pub(super) fn ensure_checkpoint_quality_is_typed(
    decision: NativeSttQualityDecision,
    reason: NativeSttQualityReason,
) -> Result<()> {
    if decision == NativeSttQualityDecision::Rejected
        && matches!(
            reason,
            NativeSttQualityReason::UnknownLanguage
                | NativeSttQualityReason::InvalidDuration
                | NativeSttQualityReason::InvalidText
                | NativeSttQualityReason::InvalidTimestamp
                | NativeSttQualityReason::InvalidQualityEvidence
        )
    {
        bail!(
            "native checkpoint refuses a structurally unusable provider result; paid-result recovery is required"
        );
    }
    Ok(())
}

pub(super) fn validate_request_matches_policy(
    policy: &RunPolicy,
    request: &NativeSttRequest,
    recipe: &str,
) -> Result<()> {
    let RunPolicyParameters::NativeSpeechTranscription {
        language,
        provider_policy,
        ..
    } = &policy.parameters
    else {
        bail!("native checkpoint policy is missing native speech parameters");
    };
    ensure!(
        policy.provider == request.provider()
            && policy.primary_model == request.model()
            && language.as_deref() == request.language()
            && *provider_policy == request.provider_policy()
            && request.temperature().to_bits() == 0.0_f64.to_bits(),
        "native checkpoint request language/data-policy/provider/model does not match the frozen policy"
    );
    ensure!(
        native_stt_transcript_recipe(request)? == recipe,
        "native checkpoint request does not match the frozen native transcript recipe"
    );
    Ok(())
}

pub(super) fn validate_current_checkpoint_window(
    connection: &Connection,
    policy: &RunPolicy,
    scope: &RunAssetScope,
    request: &NativeSttRequest,
    audio: &NativeSttAudioWindow,
    window: &NativeTranscriptCheckpointWindow,
) -> Result<()> {
    window.observation.validate()?;
    ensure_checkpoint_quality_is_typed(
        window.observation.quality.decision,
        window.observation.quality.reason,
    )?;
    crate::analysis::quality::native_stt::validate_native_mapped_candidates(
        window.observation.quality.decision,
        window.observation.quality.reason,
        &window.observation.reported_language,
        &window.candidates,
        audio.window(),
    )?;
    let recorded = accounting_store::validate_native_settled_result_request(
        connection,
        &window.observation.request_key,
        &NativeSettledResultProof {
            project_id: &policy.project_id,
            policy_id: &policy.policy_id,
            policy_fingerprint: &policy.policy_id,
            expected_provider_cost_bound_contract: &policy.provider_cost_bound_contract,
            source_fingerprint: &scope.source_identity,
            requested_provider: &policy.provider,
            requested_model: &policy.primary_model,
            run_id: None,
            allowed_operations: &["pilot", AnalysisRunKind::NativeSpeechTranscription.as_str()],
        },
    )?;
    ensure!(
        recorded.payload_fingerprint == analysis_native_stt_payload_fingerprint(request, audio)?,
        "native checkpoint completed window no longer matches the exact current prepared payload"
    );
    Ok(())
}

/// Re-prove the current authorization on the caller's transaction. Persistence
/// never trusts a preparation's cached policy identities on their own.
pub(super) fn validated_checkpoint_policy(
    transaction: &Transaction<'_>,
    evidence: &NativeSpeechPreparationEvidence,
) -> Result<RunPolicy> {
    let (authorization, accounting_contract) =
        load_run_authorization_on_connection(transaction, &evidence.policy_id)?;
    ensure!(
        accounting_contract == ANALYSIS_ACCOUNTING_SCHEMA_VERSION,
        "native checkpoint policy was authorized under a different accounting contract"
    );
    let policy = authorization.policy;
    ensure!(
        policy.policy_id == evidence.policy_id
            && policy.project_id == evidence.project_id
            && policy.provider == evidence.provider
            && policy.primary_model == evidence.model
            && policy.recipe_version == evidence.recipe,
        "native checkpoint preparation identities no longer match the frozen policy"
    );
    validate_native_policy_recipe(&policy, &evidence.recipe)?;
    ensure!(
        policy.scope.iter().any(|scope| scope == &evidence.scope),
        "native checkpoint preparation source is outside the frozen policy scope"
    );
    Ok(policy)
}
