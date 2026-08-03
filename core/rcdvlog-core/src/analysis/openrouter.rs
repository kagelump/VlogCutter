//! The OpenRouter transport.
//!
//! This is the only file in the crate that performs network I/O. Requests go to
//! the configured base URL and nowhere else; nothing here follows a URL that
//! came from model output or project data.
//!
//! The API key is never logged, never placed in a URL or argv, and never written
//! into the catalog or an error message.

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::analysis::provider::{VlmProvider, VlmRequest, VlmResponse};

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Generous: a truncated response is a rejected result, so the limit should not
/// be the thing that truncates it.
const MAX_TOKENS: u32 = 1_500;

/// Where the key file lives when `OPENROUTER_API_KEY` is unset.
pub fn key_file_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".openrouter.key"))
}

/// Resolves the API key: environment first, then the key file.
///
/// An OAuth2 flow is planned and will take precedence over both once it exists.
/// Until then the file is the app's normal path and the environment variable is
/// the agent/CI override.
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

/// Builds the request body. Pure, so the wire contract is testable offline.
pub fn build_request_body(model: &str, prompt: &str, frames: &[String], schema: &Value) -> Value {
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
        "max_tokens": MAX_TOKENS,
        "reasoning": {"enabled": false},
        "messages": [{"role": "user", "content": content}],
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "clip_observation",
                "strict": true,
                "schema": schema
            }
        }
    })
}

pub struct OpenRouterProvider {
    base_url: String,
    api_key: String,
    client: reqwest::blocking::Client,
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

impl VlmProvider for OpenRouterProvider {
    fn analyze(&self, model: &str, request: &VlmRequest) -> Result<VlmResponse> {
        let body = build_request_body(model, &request.prompt, &request.frames, &request.schema);
        let started = Instant::now();
        let response = self
            .client
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .context("send OpenRouter request")?;

        let status = response.status();
        if !status.is_success() {
            // The body can echo request content but never the key, which travels
            // only in the Authorization header.
            let detail = response.text().unwrap_or_default();
            let detail: String = detail.chars().take(300).collect();
            bail!("OpenRouter returned {status}: {detail}");
        }

        let payload: Value = response.json().context("decode OpenRouter response")?;
        let (prompt_tokens, completion_tokens, cost_usd) = extract_usage(&payload);
        Ok(VlmResponse {
            model: model.to_string(),
            body: extract_content(&payload)?,
            prompt_tokens,
            completion_tokens,
            cost_usd,
            latency_ms: started.elapsed().as_millis() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
