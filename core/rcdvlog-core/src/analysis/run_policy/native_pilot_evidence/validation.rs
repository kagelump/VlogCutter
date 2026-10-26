//! Pure validation of bounded native pilot facts and mapped candidates.

use super::*;

pub(super) fn ensure_provider_request_matches_policy(
    policy: &RunPolicy,
    request: &NativeSttRequest,
) -> Result<()> {
    let RunPolicyParameters::NativeSpeechTranscription {
        language,
        provider_policy,
        ..
    } = &policy.parameters
    else {
        anyhow::bail!("native pilot evidence policy is missing native speech parameters");
    };
    ensure!(
        policy.provider == request.provider()
            && policy.primary_model == request.model()
            && language.as_deref() == request.language()
            && *provider_policy == request.provider_policy()
            && request.temperature().to_bits() == 0.0_f64.to_bits(),
        "native pilot request language/data-policy/provider/model does not match the frozen policy"
    );
    let expected_recipe = native_stt_transcript_recipe(request)?;
    ensure!(
        policy.recipe_version == expected_recipe,
        "native pilot request does not match the frozen deterministic speech recipe"
    );
    Ok(())
}

pub(super) fn validate_scope_structure(scope: &RunAssetScope) -> Result<()> {
    ensure!(
        !scope.asset_id.trim().is_empty()
            && !scope.source_fingerprint.trim().is_empty()
            && !scope.source_identity.trim().is_empty()
            && scope.source_duration_ms > 0,
        "native pilot evidence source scope identity is incomplete"
    );
    ensure!(
        scope.representation == crate::analysis::framepack::REPRESENTATION,
        "native pilot evidence source representation is unsupported"
    );
    ensure!(
        scope
            .representation_fingerprint
            .as_deref()
            .is_some_and(|fingerprint| !fingerprint.trim().is_empty())
            && scope
                .sampled_representation_fingerprint
                .as_deref()
                .is_some_and(|fingerprint| !fingerprint.trim().is_empty()),
        "native pilot evidence source representation fingerprints are incomplete"
    );
    ensure!(
        scope.transcript_fingerprint.is_none(),
        "audio-only native pilot evidence must not bind transcript context"
    );
    Ok(())
}

pub(super) fn validate_request_key(request_key: &str) -> Result<()> {
    ensure!(
        !request_key.trim().is_empty()
            && request_key.trim() == request_key
            && request_key.len() <= MAX_NATIVE_PILOT_REQUEST_KEY_BYTES,
        "native pilot sample request key is missing, malformed or oversized"
    );
    Ok(())
}

pub(super) fn validate_payload_fingerprint(fingerprint: &str) -> Result<()> {
    ensure!(
        fingerprint.len() == 64
            && fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "native pilot sample payload fingerprint must be a lowercase BLAKE3 digest"
    );
    Ok(())
}

pub(super) fn validate_window_mapping(
    window: &PackedWindow,
    source_duration_ms: u32,
) -> Result<()> {
    crate::analysis::quality::native_stt::validate_native_window_mapping(window, source_duration_ms)
}

pub(super) fn validate_candidates(
    reported_language: &Option<String>,
    quality: &NativeSttWindowQualityObservation,
    candidates: &[DecodedSegment],
    window: &PackedWindow,
) -> Result<()> {
    crate::analysis::quality::native_stt::validate_native_mapped_candidates(
        quality.decision,
        quality.reason,
        reported_language,
        candidates,
        window,
    )
}
