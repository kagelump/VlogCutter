//! One native HTTP transmission, without authorization, retries or persistence.
use super::decode::bounded_generation_id;
use super::*;
use std::io::Read;
use std::time::Instant;

/// Stable client error categories safe to persist or print.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeSttClientError {
    Configuration,
    Transport,
    CancelledBeforeUpload,
}

impl std::fmt::Display for NativeSttClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration => formatter.write_str("native_stt_client:configuration"),
            Self::Transport => formatter.write_str("native_stt_client:transport"),
            Self::CancelledBeforeUpload => {
                formatter.write_str("native_stt_client:cancelled_before_upload")
            }
        }
    }
}

impl std::error::Error for NativeSttClientError {}

/// Blocking native-transcription HTTP client.
///
/// Credentials are reused from the existing OpenRouter environment/key-file
/// convention. The key is never stored in request metadata, errors or logs.
pub struct OpenRouterSttClient {
    endpoint: String,
    api_key: String,
    client: reqwest::blocking::Client,
}

impl std::fmt::Debug for OpenRouterSttClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenRouterSttClient")
            .field("endpoint", &self.endpoint)
            .field("api_key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl OpenRouterSttClient {
    /// Builds a client from an already resolved key. Missing credentials fail
    /// locally before any request body can be built or sent.
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            bail!(NativeSttClientError::Configuration);
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(NATIVE_STT_REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| NativeSttClientError::Configuration)?;
        Ok(Self {
            endpoint: OPENROUTER_STT_ENDPOINT.to_string(),
            api_key,
            client,
        })
    }

    /// Reuses the existing secure OpenRouter credential convention.
    pub fn from_environment() -> Result<Self> {
        Self::new(resolve_api_key()?)
    }

    /// Overrides the endpoint for local fake-provider tests only.
    #[cfg(test)]
    pub(super) fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    fn validate_configuration(&self) -> Result<()> {
        if self.api_key.trim().is_empty() || self.endpoint.trim().is_empty() {
            bail!(NativeSttClientError::Configuration);
        }
        Ok(())
    }

    /// Performs one request with no automatic retry or model fallback.
    pub fn transcribe(
        &self,
        request: &NativeSttRequest,
        audio: &NativeSttAudioWindow,
    ) -> Result<NativeSttCallResult> {
        self.transcribe_with_cancel(request, audio, &mut || false)
    }

    /// Checks cancellation only before the HTTP send. Once a request has been
    /// uploaded this client cannot and does not claim that no charge occurred.
    pub fn transcribe_with_cancel(
        &self,
        request: &NativeSttRequest,
        audio: &NativeSttAudioWindow,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<NativeSttCallResult> {
        self.validate_configuration()?;
        let body = build_native_stt_request_body(request, audio)?;
        if is_cancelled() {
            bail!(NativeSttClientError::CancelledBeforeUpload);
        }

        let started = Instant::now();
        let mut response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .map_err(|_| NativeSttClientError::Transport)?;
        let status = response.status();
        let generation_id = response
            .headers()
            .get("x-generation-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);

        let mut body_bytes = Vec::new();
        let read_result = (&mut response)
            .take(MAX_NATIVE_STT_RESPONSE_BYTES as u64 + 1)
            .read_to_end(&mut body_bytes);
        if read_result.is_err() {
            return Ok(NativeSttCallResult {
                receipt: NativeSttReceipt {
                    generation_id: generation_id.as_deref().and_then(bounded_generation_id),
                    ..NativeSttReceipt::default()
                },
                provider_error: None,
                latency_ms: elapsed_ms(started),
                outcome: NativeSttOutcome::Unusable(NativeSttProtocolError::BodyReadFailed),
            });
        }
        if body_bytes.len() > MAX_NATIVE_STT_RESPONSE_BYTES {
            return Ok(NativeSttCallResult {
                receipt: NativeSttReceipt {
                    generation_id: generation_id.as_deref().and_then(bounded_generation_id),
                    ..NativeSttReceipt::default()
                },
                provider_error: None,
                latency_ms: elapsed_ms(started),
                outcome: NativeSttOutcome::Unusable(NativeSttProtocolError::ResponseTooLarge),
            });
        }
        let mut result = decode_native_stt_body(
            &body_bytes,
            audio.duration_ms(),
            generation_id,
            elapsed_ms(started),
        );
        if !status.is_success() {
            result.provider_error =
                parse_provider_error(&body_bytes, status.as_u16(), &self.api_key);
            result.outcome =
                NativeSttOutcome::Unusable(NativeSttProtocolError::HttpStatus(status.as_u16()));
        } else if let NativeSttOutcome::Unusable(reason) = result.outcome {
            let value: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
            let provider_duration = value
                .get("duration")
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite())
                .map(|v| format!("{v:.3}s"))
                .unwrap_or_else(|| "missing".into());
            let latest_end = value
                .get("segments")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|segment| segment.get("end").and_then(Value::as_f64))
                .filter(|v| v.is_finite())
                .reduce(f64::max)
                .map(|v| format!("{v:.3}s"))
                .unwrap_or_else(|| "missing".into());
            result.provider_error = Some(NativeSttProviderError {
                http_status: status.as_u16(),
                code: Some(reason.code().into()),
                message: format!(
                    "Response validation failed ({}). Audio sent: {:.3}s; provider duration: {}; latest segment end: {}.",
                    reason.code(),
                    audio.duration_ms() as f64 / 1000.0,
                    provider_duration,
                    latest_end
                ),
            });
        }
        Ok(result)
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

impl NativeSttTransport for OpenRouterSttClient {
    fn prepare(&self) -> Result<()> {
        OpenRouterSttClient::validate_configuration(self)
    }

    /// Uses the existing no-cancel leaf. The shared accounting boundary has
    /// already committed the dispatch marker; a second cancellation check here
    /// could be misreported as proof that no upload occurred.
    fn transcribe(
        &self,
        request: &NativeSttRequest,
        audio: &NativeSttAudioWindow,
    ) -> Result<NativeSttCallResult> {
        OpenRouterSttClient::transcribe(self, request, audio)
    }
}

fn parse_provider_error(body: &[u8], status: u16, api_key: &str) -> Option<NativeSttProviderError> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let error = value.get("error").unwrap_or(&value);
    let mut message = error.get("message").and_then(Value::as_str)?.to_owned();
    // OpenRouter sometimes wraps the upstream message in metadata.raw JSON.
    if let Some(raw) = error.pointer("/metadata/raw").and_then(Value::as_str)
        && let Ok(raw) = serde_json::from_str::<Value>(raw)
        && let Some(detail) = raw
            .pointer("/error/message")
            .or_else(|| raw.get("message"))
            .and_then(Value::as_str)
    {
        message.push_str(": ");
        message.push_str(detail);
    }
    if !api_key.is_empty() {
        message = message.replace(api_key, "[redacted]");
    }
    let message = redact_error_tokens(&message);
    let code = error.get("code").and_then(|code| match code {
        Value::String(code) => Some(redact_error_tokens(code).chars().take(128).collect()),
        Value::Number(code) => Some(code.to_string()),
        _ => None,
    });
    Some(NativeSttProviderError {
        http_status: status,
        code,
        message,
    })
}

fn redact_error_tokens(value: &str) -> String {
    value
        .split_whitespace()
        .map(|word| {
            if word.to_ascii_lowercase().starts_with("sk-") || word.len() > 256 {
                "[redacted]"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(2048)
        .collect()
}
