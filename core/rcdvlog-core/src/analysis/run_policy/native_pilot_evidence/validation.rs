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
    ensure!(
        !window.regions.is_empty(),
        "native pilot sample packed window has no speech regions"
    );
    let mut next_window_offset = 0_u32;
    let mut previous_source_end = 0_u32;
    let mut speech_ms = 0_u32;
    for (index, region) in window.regions.iter().enumerate() {
        ensure!(
            region.duration_ms > 0
                && region.window_offset_ms == next_window_offset
                && region.window_offset_ms.saturating_add(region.duration_ms) <= WINDOW_MS,
            "native pilot sample packed window region {index} has an invalid packed offset"
        );
        let source_end = region
            .source_start_ms
            .checked_add(region.duration_ms)
            .ok_or_else(|| anyhow::anyhow!("native pilot sample source time overflowed"))?;
        ensure!(
            region.source_start_ms >= previous_source_end && source_end <= source_duration_ms,
            "native pilot sample packed region {index} exceeds source bounds or overlaps"
        );
        next_window_offset = next_window_offset.saturating_add(region.duration_ms);
        previous_source_end = source_end;
        speech_ms = speech_ms.saturating_add(region.duration_ms);
    }
    ensure!(
        speech_ms > 0 && speech_ms <= WINDOW_MS,
        "native pilot sample packed speech duration is invalid"
    );
    Ok(())
}

pub(super) fn validate_candidates(
    reported_language: &Option<String>,
    quality: &NativeSttWindowQualityObservation,
    candidates: &[DecodedSegment],
    window: &PackedWindow,
) -> Result<()> {
    match quality.decision {
        NativeSttQualityDecision::Rejected => {
            ensure!(
                candidates.is_empty(),
                "rejected native pilot sample must not retain candidate text"
            );
            ensure!(
                quality.reason != NativeSttQualityReason::AcceptedCompleteEvidence,
                "rejected native pilot sample quality reason is inconsistent"
            );
            return Ok(());
        }
        NativeSttQualityDecision::Accepted | NativeSttQualityDecision::ReviewRequired => {
            ensure!(
                !candidates.is_empty(),
                "accepted/review native pilot sample must retain mapped candidates"
            );
            ensure!(
                reported_language.is_some(),
                "accepted/review native pilot sample must retain its recognized reported language"
            );
        }
    }
    ensure!(
        candidates.len() <= MAX_NATIVE_PILOT_CANDIDATES,
        "native pilot sample candidate count exceeds the bounded provider segment limit"
    );
    let total_text_bytes = candidates.iter().try_fold(0_usize, |total, candidate| {
        total.checked_add(candidate.text.len())
    });
    ensure!(
        total_text_bytes.is_some_and(
            |total| total <= crate::analysis::openrouter_stt::MAX_NATIVE_STT_RESPONSE_BYTES
        ),
        "native pilot sample candidate text exceeds the bounded response size"
    );
    let expected_language = reported_language
        .as_deref()
        .and_then(normalized_native_stt_language)
        .ok_or_else(|| {
            anyhow::anyhow!("native pilot sample reported language is not recognized")
        })?;
    ensure!(
        crate::analysis::quality::native_stt::native_stt_repetition_is_safe(
            crate::analysis::quality::repetition_ratio(candidates)
        ),
        "native pilot sample candidate text fails the repetition gate"
    );
    let mut previous_end_ms = 0_u32;
    let mut candidate_language: Option<&str> = None;
    for (index, candidate) in candidates.iter().enumerate() {
        ensure!(
            candidate.start_ms < candidate.end_ms,
            "native pilot sample candidate {index} has an invalid source span"
        );
        ensure!(
            !candidate.text.trim().is_empty()
                && candidate.text.len() <= MAX_NATIVE_PILOT_CANDIDATE_TEXT_BYTES,
            "native pilot sample candidate {index} text is empty or oversized"
        );
        ensure!(
            candidate.mean_token_probability.is_none(),
            "native pilot sample candidate {index} must not claim arithmetic token probability"
        );
        validate_iso639_1(&candidate.language)?;
        ensure!(
            candidate.language == expected_language,
            "native pilot sample candidate language disagrees with its reported language"
        );
        if let Some(language) = candidate_language {
            ensure!(
                language == candidate.language,
                "native pilot sample candidates use inconsistent languages"
            );
        } else {
            candidate_language = Some(&candidate.language);
        }
        ensure!(
            previous_end_ms <= candidate.start_ms,
            "native pilot sample candidate {index} overlaps or is out of source order"
        );
        ensure!(
            source_start_maps_to_window(window, candidate.start_ms)
                && source_end_maps_to_window(window, candidate.end_ms),
            "native pilot sample candidate {index} is not mapped by the packed source window"
        );
        previous_end_ms = candidate.end_ms;
    }
    Ok(())
}

pub(super) fn source_start_maps_to_window(window: &PackedWindow, source_ms: u32) -> bool {
    window.regions.iter().any(|region| {
        source_ms >= region.source_start_ms
            && source_ms < region.source_start_ms + region.duration_ms
    })
}

pub(super) fn source_end_maps_to_window(window: &PackedWindow, source_ms: u32) -> bool {
    window.regions.iter().any(|region| {
        source_ms > region.source_start_ms
            && source_ms <= region.source_start_ms + region.duration_ms
    })
}
