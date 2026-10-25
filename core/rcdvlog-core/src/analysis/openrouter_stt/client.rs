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
            result.outcome =
                NativeSttOutcome::Unusable(NativeSttProtocolError::HttpStatus(status.as_u16()));
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
