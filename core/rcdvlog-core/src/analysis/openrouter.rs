//! The OpenRouter chat-completions transport for cloud vision analysis.
//!
//! This leaf performs only chat-completions network I/O. Native audio
//! transcription has its own typed leaf in `openrouter_stt`; a VLM request is
//! never used as a stand-in for speech input.
//!
//! The API key is never logged, never placed in a URL or argv, and never written
//! into the catalog or an error message.

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::analysis::provider::{
    AnalysisRequestCostBound, FALLBACK_MODEL, OPENROUTER_MAX_OUTPUT_TOKENS, ProviderFailure,
    VlmProvider, VlmReceipt, VlmRequest, VlmResponse, analysis_cost_bound_contract_id, model_spec,
};

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Where the key file lives when `OPENROUTER_API_KEY` is unset.
pub fn key_file_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".openrouter.key"))
}

/// Resolves the API key: environment first, then the key file.
///
/// An OAuth2 flow is planned and will take precedence over both once it exists.
/// Until then the file is the app's normal path and the environment variable is
/// the agent/CI override.
/// Whether credentials can be resolved without exposing the key or any path.
/// Used by preflight/pilot readiness checks so an absent key fails before a
/// provider client or request is constructed.
pub fn api_credential_present() -> bool {
    resolve_api_key().is_ok()
}

pub fn resolve_api_key() -> Result<String> {
    if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Ok(key);
        }
    }
    let path = key_file_path().ok_or_else(|| anyhow!("cannot resolve the home directory"))?;
    read_key_file(&path)
}

fn read_key_file(path: &Path) -> Result<String> {
    if !path.is_file() {
        bail!(
            "no OpenRouter API key. Set OPENROUTER_API_KEY, or write the key to {}",
            path.display()
        );
    }
    let key = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?
        .trim()
        .to_string();
    if key.is_empty() {
        bail!("{} is empty", path.display());
    }
    Ok(key)
}

/// Encodes one JPEG as the `data:` URI the chat API expects.
pub fn frame_data_uri(bytes: &[u8]) -> String {
    format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// OpenRouter provider-routing preferences that make the cost bound
/// enforceable. `max_price.prompt`/`completion` are USD per million tokens;
/// `image` is a per-image ceiling and `request` is a separate per-request fee,
/// never a cap on total request price.
pub fn provider_routing_preferences(model: &str) -> Option<Value> {
    let spec = model_spec(model)?;
    Some(json!({
        "require_parameters": true,
        "max_price": {
            "prompt": spec.prompt_price_usd_per_million,
            "completion": spec.completion_price_usd_per_million,
            "image": spec.image_price_usd_each,
            "request": spec.request_price_usd_each
        }
    }))
}

/// Derive a conservative upper bound for one OpenRouter transmission.
///
/// The bound uses the registered model context window as the maximum prompt
/// tokens, the exact `max_tokens` wire value as the completion limit, the exact
/// frame count for separately-priced images, and one per-request fee. The same
/// numeric price ceilings are sent in `provider.max_price`, so a provider above
/// those ceilings is not selected. This is an admission reservation, not a
/// confirmed charge: the provider invoice remains the external fact, and a
/// returned cost above this bound is an external fact for later receipt
/// reconciliation rather than a reason to trust this reservation.
pub fn openrouter_request_cost_bound(
    model: &str,
    request: &VlmRequest,
) -> Result<AnalysisRequestCostBound> {
    let spec = model_spec(model).ok_or_else(|| {
        anyhow!(
            "no enforceable OpenRouter context/pricing contract is registered for model {model}; refusing paid dispatch"
        )
    })?;
    anyhow::ensure!(
        spec.context_window_tokens > 0,
        "registered OpenRouter model {model} has no bounded prompt context"
    );
    anyhow::ensure!(
        spec.max_completion_tokens >= OPENROUTER_MAX_OUTPUT_TOKENS,
        "registered OpenRouter model {model} cannot accept the exact max_tokens wire cap"
    );
    anyhow::ensure!(
        request.frames.is_empty() || spec.supports_image_input,
        "registered OpenRouter model {model} does not support the requested image payload"
    );
    let image_count = u32::try_from(request.frames.len())
        .context("request has more image frames than the cost contract supports")?;
    let mut bound = AnalysisRequestCostBound {
        maximum_cost_usd: 0.0,
        contract_version: analysis_cost_bound_contract_id(),
        prompt_tokens_max: spec.context_window_tokens,
        completion_tokens_max: OPENROUTER_MAX_OUTPUT_TOKENS,
        image_count,
        prompt_price_usd_per_million: spec.prompt_price_usd_per_million,
        completion_price_usd_per_million: spec.completion_price_usd_per_million,
        image_price_usd_each: spec.image_price_usd_each,
        request_price_usd_each: spec.request_price_usd_each,
        audio: None,
        assumptions: vec![
            "prompt tokens cannot exceed the registered model context window".into(),
            "completion tokens are capped by the exact request max_tokens and the registered model completion limit".into(),
            "OpenRouter provider.max_price at these prompt/completion/image/request ceilings is enforced during routing".into(),
            "image_count counts exactly the transmitted frames and image pricing is a separate per-image ceiling".into(),
            "max_price.request is one per-request fee, not a cap on total request cost".into(),
            "unmodelled provider fees or a returned charge above this bound are external provider facts; this bound is never a confirmed charge".into(),
        ],
        published_token_tariff: None,
    };
    bound.maximum_cost_usd = bound.computed_maximum_cost_usd()?;
    bound.validate()?;
    Ok(bound)
}

/// Builds the request body. Pure, so the wire contract is testable offline.
pub fn build_request_body(model: &str, prompt: &str, frames: &[String], schema: &Value) -> Value {
    let schema = if model == FALLBACK_MODEL {
        gemini_compatible_schema(schema.clone())
    } else {
        schema.clone()
    };
    let mut content = vec![json!({"type": "text", "text": prompt})];
    for frame in frames {
        content.push(json!({"type": "image_url", "image_url": {"url": frame}}));
    }
    json!({
        "model": model,
        // Determinism matters more than variety for a stored artifact a user
        // has reviewed: a re-analysis that churns labels is worse than a
        // slightly less imaginative one.
        "temperature": 0,
        "max_tokens": OPENROUTER_MAX_OUTPUT_TOKENS,
        "reasoning": {"enabled": false},
        "messages": [{"role": "user", "content": content}],
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "clip_observation",
                "strict": true,
                "schema": schema
            }
        },
        "provider": provider_routing_preferences(model)
    })
}

/// Google AI Studio's OpenRouter adapter currently turns numeric JSON-Schema
/// enums into string enums. Remove only those enums for Gemini; the integer type
/// remains structural and the domain validator still requires membership in the
/// exact supplied frame pack. String enums and array bounds remain untouched.
fn gemini_compatible_schema(mut value: Value) -> Value {
    if let Some(thumbnail) = value
        .get_mut("properties")
        .and_then(|properties| properties.get_mut("thumbnailTimestampMs"))
    {
        let mut allowed = thumbnail["anyOf"][0]["enum"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .map(|timestamp| Value::String(timestamp.to_string()))
            .collect::<Vec<_>>();
        allowed.push(Value::String("none".to_string()));
        *thumbnail = json!({
            "type": "string",
            "enum": allowed,
            "description": "Choose exactly one supplied frame timestamp as a decimal string, or 'none'."
        });
    }
    match &mut value {
        Value::Object(object) => {
            let remove_numeric_enum = object
                .get("enum")
                .and_then(Value::as_array)
                .is_some_and(|values| values.iter().any(Value::is_number));
            if remove_numeric_enum {
                object.remove("enum");
            }
            for child in object.values_mut() {
                *child = gemini_compatible_schema(child.take());
            }
        }
        Value::Array(values) => {
            for child in values {
                *child = gemini_compatible_schema(child.take());
            }
        }
        _ => {}
    }
    value
}

fn normalize_gemini_content(content: String) -> Result<String> {
    let mut value: Value = serde_json::from_str(&content).context("decode Gemini JSON content")?;
    if let Some(thumbnail) = value.get_mut("thumbnailTimestampMs")
        && let Some(text) = thumbnail.as_str()
    {
        *thumbnail = if matches!(text, "none" | "null") {
            Value::Null
        } else {
            Value::Number(
                text.parse::<u64>()
                    .context("parse Gemini thumbnail timestamp")?
                    .into(),
            )
        };
    }
    serde_json::to_string(&value).context("encode normalized Gemini content")
}

pub struct OpenRouterProvider {
    base_url: String,
    api_key: String,
    client: reqwest::blocking::Client,
}

fn retry_after_seconds(header: &str) -> Option<i64> {
    let header = header.trim();
    let seconds = header.parse::<i64>().ok().or_else(|| {
        chrono::DateTime::parse_from_rfc2822(header)
            .ok()
            .map(|date| {
                date.timestamp()
                    .saturating_sub(chrono::Utc::now().timestamp())
            })
    })?;
    Some(seconds.clamp(0, 900))
}

impl OpenRouterProvider {
    pub fn new(api_key: String) -> Result<Self> {
        Ok(Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            api_key,
            client: reqwest::blocking::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .context("build HTTP client")?,
        })
    }

    /// Points the client at a different endpoint. Tests only.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

/// Pulls the model's text out of a chat-completions envelope.
pub fn extract_content(payload: &Value) -> Result<String> {
    payload["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("response carried no message content"))
}

/// Reads token counts and charged cost from the `usage` block.
///
/// These are recorded per request precisely so the measured image-token ratio in
/// the model registry can be re-derived from real traffic rather than estimated.
pub fn extract_usage(payload: &Value) -> (u32, u32, f64) {
    let usage = &payload["usage"];
    (
        usage["prompt_tokens"].as_u64().unwrap_or(0) as u32,
        usage["completion_tokens"].as_u64().unwrap_or(0) as u32,
        usage["cost"].as_f64().unwrap_or(0.0),
    )
}

/// Captures only provenance fields explicitly reported by OpenRouter.
pub fn extract_receipt(payload: &Value) -> VlmReceipt {
    // Oversized or blank metadata is unknown provenance, never a truncated
    // identity that could be confused with a different provider execution.
    let bounded = |value: &str, limit| !value.trim().is_empty() && value.len() <= limit;
    let model = payload["model"]
        .as_str()
        .filter(|value| bounded(value, 256));
    let generation_id = payload["id"].as_str().filter(|value| bounded(value, 512));
    let provider = payload["openrouter_metadata"]["endpoints"]["available"]
        .as_array()
        .and_then(|endpoints| {
            endpoints.iter().find_map(|endpoint| {
                (endpoint["selected"].as_bool() == Some(true))
                    .then(|| endpoint["provider"].as_str())
                    .flatten()
                    .filter(|value| bounded(value, 256))
            })
        })
        .or_else(|| {
            payload["provider"]
                .as_str()
                .filter(|value| bounded(value, 256))
        });

    let usage = &payload["usage"];
    let cost = usage["cost"].as_f64();
    let prompt_tokens = usage["prompt_tokens"].as_u64();
    let completion_tokens = usage["completion_tokens"].as_u64();
    let usage_complete = cost.is_some_and(|value| value.is_finite() && value >= 0.0)
        && prompt_tokens.is_some_and(|value| u32::try_from(value).is_ok())
        && completion_tokens.is_some_and(|value| u32::try_from(value).is_ok());

    VlmReceipt {
        actual_model: model.map(str::to_owned),
        provider: provider.map(str::to_owned),
        generation_id: generation_id.map(str::to_owned),
        usage_complete,
    }
}

impl VlmProvider for OpenRouterProvider {
    fn prepare(&self) -> Result<()> {
        // Local-only: prove the client/credential/endpoint prerequisites are
        // present before the accounting wrapper commits a dispatch marker.
        if self.api_key.trim().is_empty() {
            return Err(ProviderFailure::configuration().into());
        }
        if self.base_url.trim().is_empty() {
            return Err(ProviderFailure::configuration().into());
        }
        Ok(())
    }

    fn analysis_cost_bound(
        &self,
        model: &str,
        request: &VlmRequest,
    ) -> Result<AnalysisRequestCostBound> {
        openrouter_request_cost_bound(model, request)
    }

    fn analyze(&self, model: &str, request: &VlmRequest) -> Result<VlmResponse> {
        // Fail closed before any network I/O when no enforceable context/price
        // contract is registered, even if a caller bypassed the accounting
        // wrapper.
        openrouter_request_cost_bound(model, request)?;
        let body = build_request_body(model, &request.prompt, &request.frames, &request.schema);
        let started = Instant::now();
        let response = self
            .client
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .header("Content-Type", "application/json")
            .header("X-OpenRouter-Metadata", "enabled")
            .json(&body)
            .send()
            .map_err(|_| ProviderFailure::transport())?;

        let status = response.status();
        if !status.is_success() {
            // Response bodies can echo prompts, frame payloads or private
            // provider diagnostics. Only the bounded status is retained.
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(retry_after_seconds);
            return Err(ProviderFailure::http_status_with_retry_after(
                status.as_u16(),
                retry_after,
            )
            .into());
        }

        let payload: Value = response
            .json()
            .map_err(|_| ProviderFailure::invalid_response())?;
        Ok(decode_paid_response(
            model,
            &payload,
            started.elapsed().as_millis() as u64,
        ))
    }
}

/// A parsed paid envelope is usage evidence even when its content is unusable.
/// Keep that evidence on the existing response so the accounting boundary can
/// persist it before the domain validator rejects the empty/invalid body.
fn decode_paid_response(model: &str, payload: &Value, latency_ms: u64) -> VlmResponse {
    let (prompt_tokens, completion_tokens, cost_usd) = extract_usage(payload);
    let content = extract_content(payload)
        .and_then(|content| {
            if model == FALLBACK_MODEL {
                normalize_gemini_content(content)
            } else {
                Ok(content)
            }
        })
        .unwrap_or_default();
    VlmResponse {
        model: model.to_string(),
        body: content,
        prompt_tokens,
        completion_tokens,
        cost_usd,
        latency_ms,
        receipt: extract_receipt(payload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_accepts_delta_and_http_date_with_a_fifteen_minute_cap() {
        assert_eq!(retry_after_seconds("45"), Some(45));
        assert_eq!(retry_after_seconds("9999"), Some(900));
        assert_eq!(retry_after_seconds("invalid"), None);
        let future = (chrono::Utc::now() + chrono::Duration::seconds(45)).to_rfc2822();
        let parsed = retry_after_seconds(&future).unwrap();
        assert!(
            (0..=45).contains(&parsed),
            "parsed HTTP date should become a bounded delta"
        );
    }
    use crate::analysis::provider::DEFAULT_MODEL;
    use std::io::Write;

    #[test]
    fn the_request_body_carries_the_frames_and_bounded_schema() {
        let body = build_request_body(
            "qwen/qwen3.7-flash",
            "prompt text",
            &["data:image/jpeg;base64,AAAA".to_string()],
            &json!({"type": "object", "properties": {}}),
        );
        assert_eq!(body["model"], "qwen/qwen3.7-flash");
        assert_eq!(body["temperature"], 0);
        let content = body["messages"][0]["content"].as_array().unwrap().clone();
        assert!(content.iter().any(|part| part["type"] == "image_url"));
        assert!(content.iter().any(|part| part["type"] == "text"));
        assert!(body["response_format"]["json_schema"]["schema"].is_object());
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        assert_eq!(body["max_tokens"], OPENROUTER_MAX_OUTPUT_TOKENS);
        assert_eq!(body["provider"]["require_parameters"], true);
        assert!(body["provider"]["max_price"]["prompt"].is_number());
        assert!(body["provider"]["max_price"]["completion"].is_number());
        assert!(body["provider"]["max_price"]["image"].is_number());
        assert!(body["provider"]["max_price"]["request"].is_number());
    }

    #[test]
    fn cost_bound_and_wire_caps_share_the_same_registered_contract_offline() {
        let request = VlmRequest {
            prompt: "prompt".into(),
            frames: vec!["data:image/jpeg;base64,AAAA".into(); 3],
            schema: json!({"type": "object"}),
        };
        for model in [DEFAULT_MODEL, FALLBACK_MODEL] {
            let spec = model_spec(model).unwrap();
            let bound = openrouter_request_cost_bound(model, &request).unwrap();
            let body = build_request_body(model, &request.prompt, &request.frames, &request.schema);
            assert_eq!(bound.prompt_tokens_max, spec.context_window_tokens);
            assert_eq!(bound.completion_tokens_max, OPENROUTER_MAX_OUTPUT_TOKENS);
            assert_eq!(
                body["provider"]["max_price"]["prompt"].as_f64(),
                Some(bound.prompt_price_usd_per_million)
            );
            assert_eq!(
                body["provider"]["max_price"]["completion"].as_f64(),
                Some(bound.completion_price_usd_per_million)
            );
            assert_eq!(
                body["provider"]["max_price"]["image"].as_f64(),
                Some(bound.image_price_usd_each)
            );
            assert_eq!(
                body["provider"]["max_price"]["request"].as_f64(),
                Some(bound.request_price_usd_each)
            );
            assert_eq!(body["max_tokens"], bound.completion_tokens_max);
            assert_eq!(bound.image_count, 3);
            let expected = bound.prompt_tokens_max as f64 * bound.prompt_price_usd_per_million
                / 1_000_000.0
                + bound.completion_tokens_max as f64 * bound.completion_price_usd_per_million
                    / 1_000_000.0
                + bound.image_count as f64 * bound.image_price_usd_each
                + bound.request_price_usd_each;
            assert!((bound.maximum_cost_usd - expected).abs() < 1e-12);
            assert!(bound.maximum_cost_usd > bound.request_price_usd_each);
            assert!(!bound.assumptions.is_empty());
        }
        assert!(openrouter_request_cost_bound("unregistered/model", &request).is_err());
        assert!(provider_routing_preferences("unregistered/model").is_none());
    }

    #[test]
    fn unsupported_model_is_rejected_before_network_io() {
        let provider = OpenRouterProvider::new("sk-or-v1-test".into())
            .unwrap()
            .with_base_url("http://127.0.0.1:9/unreachable");
        let error = provider
            .analyze("unregistered/model", &VlmRequest::default())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("no enforceable OpenRouter context/pricing contract"),
            "{error}"
        );
    }

    #[test]
    fn receipt_parses_actual_model_selected_provider_and_complete_usage() {
        let requested_model = "router/model-alias";
        let receipt = extract_receipt(&json!({
            "id": "gen-42",
            "model": "provider/actual-model",
            "openrouter_metadata": {"endpoints": {"available": [
                {"provider": "Other", "selected": false},
                {"provider": "Provider A", "selected": true}
            ]}},
            "usage": {"cost": 0.004, "prompt_tokens": 20, "completion_tokens": 7}
        }));
        assert_eq!(
            receipt.actual_model.as_deref(),
            Some("provider/actual-model")
        );
        assert_ne!(receipt.actual_model.as_deref(), Some(requested_model));
        assert_eq!(receipt.provider.as_deref(), Some("Provider A"));
        assert_eq!(receipt.generation_id.as_deref(), Some("gen-42"));
        assert!(receipt.usage_complete);
    }

    #[test]
    fn receipt_does_not_infer_missing_provenance_or_usage() {
        let receipt = extract_receipt(&json!({
            "usage": {"prompt_tokens": 1, "completion_tokens": 2}
        }));
        assert_eq!(receipt.actual_model, None);
        assert_eq!(receipt.provider, None);
        assert_eq!(receipt.generation_id, None);
        assert!(!receipt.usage_complete);

        let zero_cost = extract_receipt(&json!({
            "provider": "legacy-provider",
            "usage": {"cost": 0, "prompt_tokens": 0, "completion_tokens": 0}
        }));
        assert_eq!(zero_cost.provider.as_deref(), Some("legacy-provider"));
        assert!(zero_cost.usage_complete);
    }

    #[test]
    fn receipt_bounds_reported_metadata_without_discarding_valid_usage() {
        let oversized_model = "m".repeat(257);
        let oversized_provider = "p".repeat(257);
        let oversized_id = "i".repeat(513);
        let receipt = extract_receipt(&json!({
            "id": oversized_id,
            "model": oversized_model,
            "openrouter_metadata": {"endpoints": {"available": [
                {"provider": oversized_provider, "selected": true}
            ]}},
            "usage": {"cost": 0.01, "prompt_tokens": 10, "completion_tokens": 5}
        }));
        assert_eq!(receipt.actual_model, None);
        assert_eq!(receipt.provider, None);
        assert_eq!(receipt.generation_id, None);
        assert!(
            receipt.usage_complete,
            "valid cost and token counts remain usable"
        );

        let blank = extract_receipt(&json!({
            "id": "  ", "model": "\t",
            "provider": " ",
            "usage": {"cost": 0, "prompt_tokens": 0, "completion_tokens": 0}
        }));
        assert_eq!(blank.actual_model, None);
        assert_eq!(blank.provider, None);
        assert_eq!(blank.generation_id, None);
        assert!(blank.usage_complete);
    }

    #[test]
    fn reasoning_is_disabled_so_tokens_are_not_burned_before_the_answer() {
        let body = build_request_body("m", "p", &[], &json!({}));
        assert_eq!(body["reasoning"]["enabled"], false);
    }

    #[test]
    fn every_frame_becomes_its_own_image_part() {
        let frames = vec!["data:image/jpeg;base64,A".to_string(); 5];
        let body = build_request_body("m", "p", &frames, &json!({}));
        let images = body["messages"][0]["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|part| part["type"] == "image_url")
            .count();
        assert_eq!(images, 5);
    }

    #[test]
    fn gemini_keeps_string_enums_and_array_bounds_but_drops_numeric_enums() {
        let schema = json!({
            "type": "object",
            "properties": {
                "stamp": {"type": "integer", "enum": [400, 900]},
                "movement": {"type": "string", "enum": ["pan", "other"]},
                "labels": {"type": "array", "maxItems": 5, "items": {"type": "string"}}
            }
        });
        let body = build_request_body(FALLBACK_MODEL, "p", &[], &schema);
        let translated = &body["response_format"]["json_schema"]["schema"];
        assert!(translated["properties"]["stamp"]["enum"].is_null());
        assert_eq!(
            translated["properties"]["movement"]["enum"],
            json!(["pan", "other"])
        );
        assert_eq!(translated["properties"]["labels"]["maxItems"], 5);
    }

    #[test]
    fn gemini_thumbnail_strings_normalize_to_the_canonical_domain_shape() {
        let normalized = normalize_gemini_content(
            r#"{"thumbnailTimestampMs":"2500","usable":true}"#.to_string(),
        )
        .unwrap();
        let value: Value = serde_json::from_str(&normalized).unwrap();
        assert_eq!(value["thumbnailTimestampMs"], 2_500);
        let none = normalize_gemini_content(
            r#"{"thumbnailTimestampMs":"none","usable":false}"#.to_string(),
        )
        .unwrap();
        assert!(serde_json::from_str::<Value>(&none).unwrap()["thumbnailTimestampMs"].is_null());
    }

    #[test]
    fn the_api_key_never_appears_in_the_request_body() {
        let body = build_request_body("m", "p", &[], &json!({}));
        let rendered = body.to_string();
        assert!(!rendered.contains("sk-"));
        assert!(!rendered.to_lowercase().contains("authorization"));
    }

    #[test]
    fn a_jpeg_encodes_to_a_data_uri() {
        let uri = frame_data_uri(&[0xFF, 0xD8, 0xFF]);
        assert!(uri.starts_with("data:image/jpeg;base64,"));
        assert!(uri.ends_with("/9j/"));
    }

    #[test]
    fn a_missing_key_file_produces_an_actionable_error() {
        let directory = tempfile::tempdir().unwrap();
        let error = read_key_file(&directory.path().join(".openrouter.key"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("OPENROUTER_API_KEY"), "{error}");
        assert!(error.contains(".openrouter.key"), "{error}");
    }

    #[test]
    fn a_key_file_is_read_and_trimmed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(".openrouter.key");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "  sk-or-v1-example  ").unwrap();
        drop(file);
        assert_eq!(read_key_file(&path).unwrap(), "sk-or-v1-example");
    }

    #[test]
    fn an_empty_key_file_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(".openrouter.key");
        std::fs::write(&path, "\n\n").unwrap();
        assert!(
            read_key_file(&path)
                .unwrap_err()
                .to_string()
                .contains("empty")
        );
    }

    #[test]
    fn the_default_key_file_is_the_documented_path() {
        let path = key_file_path().unwrap();
        assert!(path.ends_with(".openrouter.key"));
    }

    #[test]
    fn content_and_usage_are_read_out_of_a_chat_envelope() {
        let payload = json!({
            "choices": [{"message": {"content": "{\"title\":\"x\"}"}}],
            "usage": {"prompt_tokens": 2175, "completion_tokens": 120, "cost": 0.00013}
        });
        assert_eq!(extract_content(&payload).unwrap(), "{\"title\":\"x\"}");
        let (prompt, completion, cost) = extract_usage(&payload);
        assert_eq!(prompt, 2175);
        assert_eq!(completion, 120);
        assert!((cost - 0.00013).abs() < 1e-9);
    }

    #[test]
    fn a_response_without_content_is_an_error_rather_than_an_empty_string() {
        // nex-n2-mini returned exactly this shape after burning its budget on
        // reasoning tokens.
        let payload = json!({"choices": [{"message": {}}], "usage": {}});
        assert!(extract_content(&payload).is_err());
    }
    #[test]
    fn unusable_paid_content_preserves_actual_usage_and_identity_for_accounting() {
        for (model, choices) in [
            (DEFAULT_MODEL, json!([{"message": {}}])),
            (
                FALLBACK_MODEL,
                json!([{"message": {"content": "malformed-provider-content"}}]),
            ),
        ] {
            let response = decode_paid_response(
                model,
                &json!({
                    "choices": choices, "id": "paid-generation", "model": "actual-model",
                    "provider": "actual-upstream", "usage": {"cost": 0.03, "prompt_tokens": 20, "completion_tokens": 4}
                }),
                7,
            );
            assert!(response.body.is_empty());
            assert_eq!(response.cost_usd, 0.03);
            assert_eq!(response.prompt_tokens, 20);
            assert_eq!(response.completion_tokens, 4);
            assert!(response.receipt.usage_complete);
            assert_eq!(
                response.receipt.generation_id.as_deref(),
                Some("paid-generation")
            );
            assert_eq!(
                response.receipt.actual_model.as_deref(),
                Some("actual-model")
            );
            assert!(crate::ProviderReceiptEvidence::from_response(&response).is_complete_charge());
        }
    }
}
