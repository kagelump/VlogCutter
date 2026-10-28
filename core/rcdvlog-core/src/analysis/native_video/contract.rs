//! Versioned direct endpoint, sampling and cost contracts; no network or media I/O.
use super::*;
use crate::analysis::accounting::usage_pricing::{PublishedServiceTier, PublishedTokenTariff};
use crate::analysis::provider::AnalysisRequestCostBound;
use anyhow::{Result, ensure};

pub(crate) fn validate_options(options: &NativeVideoOptions) -> Result<()> {
    ensure!(
        options.version == 1
            && !options.source_scopes.is_empty()
            && options.source_scopes.len() <= 10_000,
        "invalid input: native video needs version 1 and 1-10000 explicit source scopes"
    );
    ensure!(
        options.fps.is_finite()
            && (0.25..=24.0).contains(&options.fps)
            && (160..=1280).contains(&options.max_width),
        "invalid input: native video sampling must be 0.25-24 fps and width 160-1280"
    );
    ensure!(
        (1..=60).contains(&options.max_window_seconds)
            && (1..=86_400).contains(&options.max_media_seconds)
            && (1..=10_000).contains(&options.max_requests)
            && (256..=8192).contains(&options.max_output_tokens)
            && (1..=4).contains(&options.media_workers)
            && (1..=8).contains(&options.api_workers),
        "invalid input: native video media/request/token/worker bounds exceeded"
    );
    let mut ordered = options.source_scopes.iter().collect::<Vec<_>>();
    ordered.sort_by(|a, b| {
        a.asset_id
            .cmp(&b.asset_id)
            .then_with(|| a.source_in_seconds.total_cmp(&b.source_in_seconds))
    });
    ensure!(
        ordered
            .windows(2)
            .all(|w| w[0].asset_id != w[1].asset_id
                || w[0].source_out_seconds <= w[1].source_in_seconds),
        "invalid input: declared native source scopes must not duplicate or overlap; request subwindows inside one broader scope"
    );
    for scope in &options.source_scopes {
        ensure!(
            !scope.asset_id.is_empty()
                && scope.source_in_seconds.is_finite()
                && scope.source_out_seconds.is_finite()
                && scope.source_in_seconds >= 0.0
                && scope.source_out_seconds > scope.source_in_seconds,
            "invalid input: source scope must have finite ordered positive source seconds"
        );
    }
    Ok(())
}
pub(crate) fn cost_bound(options: &NativeVideoOptions) -> Result<AnalysisRequestCostBound> {
    validate_options(options)?;
    let tariff = PublishedTokenTariff::gemini_35_flash_lite(options.tier);
    // Reserve the registered Standard ceiling even for explicitly requested
    // Flex. Missing or mismatched serving-tier evidence never earns a discount.
    let ceiling = PublishedTokenTariff::gemini_35_flash_lite(PublishedServiceTier::Standard);
    let contract = blake3::hash(&serde_json::to_vec(&(
        NATIVE_VIDEO_PROVIDER,
        NATIVE_VIDEO_MODEL,
        options.max_output_tokens,
        &tariff,
        &ceiling,
    ))?)
    .to_hex()
    .to_string();
    let mut bound=AnalysisRequestCostBound{maximum_cost_usd:0.0,contract_version:format!("gemini-direct.native-video.token-bound.v1:{contract}"),prompt_tokens_max:1_048_576,completion_tokens_max:options.max_output_tokens,image_count:0,prompt_price_usd_per_million:ceiling.input_usd_per_million,completion_price_usd_per_million:ceiling.output_including_thinking_usd_per_million,image_price_usd_each:0.0,request_price_usd_each:0.0,audio:None,published_token_tariff:Some(tariff),assumptions:vec![
        "Conservative full registered input-context reservation; source sampling predicts usage but is not a hard input-token cap.".into(),
        "The exact wire maxOutputTokens caps combined thinking plus output tokens; candidateCount is fixed at one.".into(),
        "Registered published Standard rates bound reservation estimates for Standard/Flex; this is an external tariff assumption, not an invoice cap.".into(),
        "Actual service tier, cached-token credit and thinking usage require returned evidence. No implicit tier/model fallback or automatic retransmission.".into()]};
    bound.maximum_cost_usd = bound.computed_maximum_cost_usd()?;
    bound.validate()?;
    Ok(bound)
}
pub(crate) fn credential_present() -> bool {
    std::env::var("GEMINI_API_KEY")
        .ok()
        .is_some_and(|key| !key.trim().is_empty())
}

pub(crate) fn tool_fingerprint(cancel: &dyn Fn() -> bool) -> Result<String> {
    let mut identities = Vec::new();
    for name in ["ffmpeg", "ffprobe"] {
        let path = crate::resolve_tool_path(name);
        let identity = crate::calibration_tool_identity_for_path("native_video", name, &path);
        let content = crate::full_file_blake3(&path, cancel)?.hex_digest;
        identities.push(serde_json::json!({"name":identity.name,"version":identity.version,"contentFingerprint":content}));
    }
    Ok(blake3::hash(&serde_json::to_vec(&identities)?)
        .to_hex()
        .to_string())
}
