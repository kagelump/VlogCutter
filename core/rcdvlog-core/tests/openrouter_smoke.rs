//! One live request against OpenRouter, to validate the wire contract.
//!
//! Ignored by default because it spends real money (a fraction of a cent). Run
//! with a real proxy:
//!
//! ```text
//! RCDVLOG_PROXY=<path to a .v3.mp4> \
//!   cargo test --release -p rcdvlog-core --test openrouter_smoke -- --ignored --nocapture
//! ```

use std::path::PathBuf;

/// Deliberately carries the unusable escape hatch, so a degraded clip can refuse
/// rather than invent.
const SMOKE_PROMPT: &str = "Describe this video clip from the supplied frames. \
     Reply only with JSON matching the schema. Write the title and description in \
     English regardless of the language shown. Read only text that is actually \
     legible; never guess. If the frames are black, blank or unreadable, set \
     usable to false and give an unusableReason.";

use rcdvlog_core::{
    OpenRouterProvider, ProviderChain, VlmRequest, build_frame_pack, encode_frames, frame_data_uri,
    output_schema, resolve_api_key, validate_observation,
};

#[test]
#[ignore = "spends real money against the configured OpenRouter key"]
fn one_real_clip_round_trips_through_the_default_model() {
    let Ok(proxy) = std::env::var("RCDVLOG_PROXY") else {
        panic!("set RCDVLOG_PROXY to a generated proxy file");
    };
    let proxy = PathBuf::from(proxy);
    let directory = tempfile::tempdir().expect("temp dir");

    let manifest = build_frame_pack(&proxy, "smoke").expect("frame pack");
    println!("frames: {:?}", manifest.timestamps_ms);
    let paths = encode_frames(&proxy, &manifest, directory.path()).expect("encode");
    assert!(!paths.is_empty());

    let frames: Vec<String> = paths
        .iter()
        .map(|path| frame_data_uri(&std::fs::read(path).expect("read frame")))
        .collect();

    let key = resolve_api_key().expect("resolve API key");
    let provider = OpenRouterProvider::new(key).expect("client");
    let mut chain = ProviderChain::new();

    let clip_duration_ms = 12_000;
    let schema = output_schema(&manifest.timestamps_ms, clip_duration_ms);
    let request = VlmRequest {
        prompt: SMOKE_PROMPT.to_string(),
        frames,
        schema: schema.clone(),
    };

    let outcome = chain.analyze(&provider, &request).expect("provider chain");
    println!("served by:  {}", outcome.response.model);
    println!("fell back:  {}", outcome.fell_back);
    println!("tokens:     {} in", outcome.response.prompt_tokens);
    println!("cost:       ${:.6}", outcome.response.cost_usd);
    println!("latency:    {} ms", outcome.response.latency_ms);
    println!("raw body:   {}", outcome.response.body);

    let observation = validate_observation(
        &outcome.response.body,
        &manifest.timestamps_ms,
        clip_duration_ms,
    )
    .expect("domain validation");

    println!("title:       {}", observation.title);
    println!("description: {}", observation.description);
    println!("labels:      {:?}", observation.labels);
    println!("ocr:         {:?}", observation.ocr_text);
    println!("usable:      {}", observation.usable);
    println!("thumbnail:   {:?}", observation.thumbnail_timestamp_ms);

    assert!(!observation.title.is_empty(), "title must be populated");
    if let Some(thumbnail) = observation.thumbnail_timestamp_ms {
        assert!(
            manifest.timestamps_ms.contains(&thumbnail),
            "thumbnail must name a supplied frame"
        );
    }
}
