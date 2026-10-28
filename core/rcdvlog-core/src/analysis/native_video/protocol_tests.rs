//! Pure wire-contract checks over generated local media and canned provider bytes.
use super::*;
use crate::{AnalysisPayload, NativeVideoOptions, RunPolicyParameters};
use serde_json::{Value, json};

fn prepared(
    fps: f64,
) -> (
    tempfile::TempDir,
    NativeVideoOptions,
    super::media::NativeVideoMedia,
    crate::RunPolicy,
) {
    let (dir, catalog, policy, _) = super::media::media_tests::setup(AnalysisPayload::Video);
    let options = match &policy.parameters {
        RunPolicyParameters::NativeVideoReview { options } => options.clone(),
        _ => unreachable!(),
    };
    let media = catalog
        .prepare_native_video_window(
            &policy,
            &super::media::media_tests::window(&policy, fps),
            &|| false,
        )
        .unwrap();
    (dir, options, media, policy)
}

fn response(tier: Option<&str>, model: Option<&str>, finish: &str, parts: Value) -> Vec<u8> {
    let mut doc = json!({"responseId":"canned-1","usageMetadata":{},"candidates":[{"finishReason":finish,"content":{"parts":parts}}]});
    if let Some(tier) = tier {
        doc["usageMetadata"]["serviceTier"] = json!(tier);
    }
    if let Some(model) = model {
        doc["modelVersion"] = json!(model);
    }
    serde_json::to_vec(&doc).unwrap()
}

fn response_usage(tier: &str, usage: Value) -> Vec<u8> {
    let mut doc: Value = serde_json::from_slice(&response(
        Some(tier),
        Some(NATIVE_VIDEO_MODEL),
        "STOP",
        json!([{"text":"{}"}]),
    ))
    .unwrap();
    doc["usageMetadata"] = usage;
    serde_json::to_vec(&doc).unwrap()
}

#[test]
fn prepare_emits_explicit_video_metadata_without_hidden_audio_text_or_credentials() {
    let (_d, options, media, _) = prepared(1.75);
    let request = prepare(&options, &media).unwrap();
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    let parts = body["contents"][0]["parts"].as_array().unwrap();
    let video = parts
        .iter()
        .find(|part| part.get("inlineData").is_some())
        .unwrap();
    let metadata = &video["videoMetadata"];
    assert_eq!(metadata["fps"], 1.75);
    assert_eq!(metadata["startOffset"], "0s");
    let end_nanos = (media.view().clock.local_valid_end_seconds * 1e9).floor() as u64;
    assert_eq!(
        metadata["endOffset"],
        format!(
            "{}.{:09}s",
            end_nanos / 1_000_000_000,
            end_nanos % 1_000_000_000
        )
    );
    assert!(!body.to_string().to_ascii_lowercase().contains("api_key"));
    assert!(!body.to_string().to_ascii_lowercase().contains("credential"));
    assert!(
        !parts
            .iter()
            .any(|part| part.get("audio").is_some() || part.get("transcript").is_some())
    );
    let prompt = parts[0]["text"].as_str().unwrap();
    assert!(prompt.contains("audioIncluded=false"));
    assert!(prompt.contains("acceptedTranscriptSegments=[]"));
}

#[test]
fn request_fingerprint_tracks_sampling_rate_and_thinking_options() {
    let (_d1, options1, media1, _) = prepared(1.0);
    let first = prepare(&options1, &media1).unwrap();
    let (_d2, options2, media2, _) = prepared(2.0);
    let second = prepare(&options2, &media2).unwrap();
    assert_ne!(first.fingerprint, second.fingerprint);
    let mut high = options1.clone();
    high.thinking = NativeVideoThinking::High;
    assert_ne!(
        first.fingerprint,
        prepare(&high, &media1).unwrap().fingerprint
    );
}

#[test]
fn decode_preserves_raw_response_and_missing_token_counts_as_missing() {
    let (_d, options, _, _) = prepared(1.0);
    let raw = response(
        Some("standard"),
        Some(NATIVE_VIDEO_MODEL),
        "STOP",
        json!([{"text":"{}"}]),
    );
    let result = decode_response(200, raw.clone(), None, 123, &options).unwrap();
    assert_eq!(result.raw_response, String::from_utf8(raw).unwrap());
    assert_eq!(result.output.as_ref().unwrap(), "{}");
    let usage = result.receipt.usage_pricing.unwrap();
    assert_eq!(usage.prompt_tokens, None);
    assert_eq!(usage.candidate_tokens, None);
    assert_eq!(usage.total_tokens, None);
}

#[test]
fn decode_rejects_missing_wrong_or_conflicting_tier_and_missing_or_wrong_model() {
    let (_d, options, _, _) = prepared(1.0);
    for (tier, header, model, expected) in [
        (
            None,
            None,
            Some(NATIVE_VIDEO_MODEL),
            "provider_tier_missing_or_mismatched",
        ),
        (
            Some("flex"),
            None,
            Some(NATIVE_VIDEO_MODEL),
            "provider_tier_missing_or_mismatched",
        ),
        (
            Some("standard"),
            Some("flex"),
            Some(NATIVE_VIDEO_MODEL),
            "provider_tier_conflict",
        ),
        (
            Some("standard"),
            None,
            None,
            "provider_model_missing_or_mismatched",
        ),
        (
            Some("standard"),
            None,
            Some("other-model"),
            "provider_model_missing_or_mismatched",
        ),
    ] {
        let result = decode_response(
            200,
            response(tier, model, "STOP", json!([{"text":"{}"}])),
            header,
            1,
            &options,
        )
        .unwrap();
        assert_eq!(result.output.unwrap_err(), expected);
    }
}

#[test]
fn decode_rejects_incomplete_finish_and_unsolicited_tool_parts() {
    let (_d, options, _, _) = prepared(1.0);
    let incomplete = decode_response(
        200,
        response(
            Some("standard"),
            Some(NATIVE_VIDEO_MODEL),
            "MAX_TOKENS",
            json!([{"text":"{}"}]),
        ),
        None,
        1,
        &options,
    )
    .unwrap();
    assert_eq!(
        incomplete.output.unwrap_err(),
        "provider_incomplete_candidate"
    );
    let tool = decode_response(
        200,
        response(
            Some("standard"),
            Some(NATIVE_VIDEO_MODEL),
            "STOP",
            json!([{"functionCall":{"name":"browse"}}]),
        ),
        None,
        1,
        &options,
    )
    .unwrap();
    assert_eq!(tool.output.unwrap_err(), "provider_unexpected_part");
    let hidden_tool = decode_response(
        200,
        response(
            Some("standard"),
            Some(NATIVE_VIDEO_MODEL),
            "STOP",
            json!([{"thought":true,"toolCall":{"id":"unexpected"}},{"text":"{}"}]),
        ),
        None,
        1,
        &options,
    )
    .unwrap();
    assert_eq!(hidden_tool.output.unwrap_err(), "provider_unexpected_part");
}

#[test]
fn decode_derives_only_proven_zero_auxiliary_usage_and_never_cache_credit() {
    let (_d, options, _, _) = prepared(1.0);
    let raw_usage = json!({"serviceTier":"standard","promptTokenCount":100,"candidatesTokenCount":20,"totalTokenCount":120});
    let result = decode_response(
        200,
        response_usage("standard", raw_usage),
        None,
        1,
        &options,
    )
    .unwrap();
    let usage = result.receipt.usage_pricing.unwrap();
    assert_eq!(usage.prompt_tokens, Some(100));
    assert_eq!(usage.candidate_tokens, Some(20));
    assert_eq!(usage.total_tokens, Some(120));
    assert_eq!(usage.cached_prompt_tokens, None);
    assert_eq!(usage.thinking_tokens, None);
    assert_eq!(usage.tool_prompt_tokens, None);
    assert_eq!(usage.conservative_derivation, Some(crate::analysis::accounting::usage_pricing::UsageConservativeDerivation::GeminiGenerateContentV1BetaConservedAuxiliaryCountersV1));
    let bound = super::cost_bound(&options).unwrap();
    let estimate = usage
        .estimate(
            &bound,
            NATIVE_VIDEO_PROVIDER,
            NATIVE_VIDEO_MODEL,
            NATIVE_VIDEO_MODEL,
        )
        .unwrap();
    assert!(estimate.is_some());

    for bad in [
        json!({"serviceTier":"standard","promptTokenCount":100,"candidatesTokenCount":20,"totalTokenCount":121}),
        json!({"serviceTier":"standard","candidatesTokenCount":20,"totalTokenCount":20}),
        json!({"serviceTier":"standard","promptTokenCount":100,"cachedContentTokenCount":-1,"candidatesTokenCount":20,"totalTokenCount":120}),
    ] {
        let result =
            decode_response(200, response_usage("standard", bad), None, 1, &options).unwrap();
        let usage = result.receipt.usage_pricing.unwrap();
        assert!(
            usage
                .estimate(
                    &bound,
                    NATIVE_VIDEO_PROVIDER,
                    NATIVE_VIDEO_MODEL,
                    NATIVE_VIDEO_MODEL
                )
                .unwrap()
                .is_none()
        );
    }
    let wrong_tier = decode_response(200, response_usage("flex", json!({"serviceTier":"flex","promptTokenCount":100,"candidatesTokenCount":20,"totalTokenCount":120})), None, 1, &options).unwrap();
    let usage = wrong_tier.receipt.usage_pricing.unwrap();
    assert!(
        usage
            .estimate(
                &bound,
                NATIVE_VIDEO_PROVIDER,
                NATIVE_VIDEO_MODEL,
                NATIVE_VIDEO_MODEL
            )
            .unwrap()
            .is_none()
    );
}
