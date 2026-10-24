//! Native OpenRouter audio-transcription transport and strict wire protocol.
//!
//! This is a protocol/client leaf, not a production backend selection. It does
//! not read catalogs, claim media, authorize spending, retry, fall back, or
//! persist transcripts. A future accounting slice must stage every request
//! through the shared reservation/dispatch/receipt machinery before this client
//! is called.
//!
//! The wire contract here is deliberately not the chat-completions contract:
//! `POST /api/v1/audio/transcriptions` with raw base64 WAV in `input_audio`,
//! `response_format: verbose_json`, and segment timestamp granularity. Only the
//! data-policy provider fields `zdr` and `data_collection` are accepted. Chat
//! routing fields such as `order`, `only`, `ignore` and `max_price` are rejected
//! rather than copied and misrepresented as enforceable.

use anyhow::{Result, anyhow, bail, ensure};
use base64::Engine;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};
use std::time::Duration;

use crate::analysis::engine::{VAD_MODEL, WHISPER_MODEL};
use crate::analysis::openrouter::resolve_api_key;
use crate::analysis::provider::{AnalysisAudioCostBound, AnalysisRequestCostBound};
use crate::analysis::speech_audio::{
    MAX_SPEECH_WAV_BYTES, concatenate_regions_bounded, encode_pcm16_wav, validate_wav_16k_mono,
};
use crate::analysis::windows::{PackedWindow, SpeechRegion, WINDOW_MS, pack_windows};

pub const OPENROUTER_STT_PROVIDER: &str = "openrouter";
pub const OPENROUTER_WHISPER_MODEL: &str = "openai/whisper-1";
pub const OPENROUTER_STT_ENDPOINT: &str = "https://openrouter.ai/api/v1/audio/transcriptions";
pub const NATIVE_STT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
pub const MAX_NATIVE_STT_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// Domain-separated version of the native-audio reservation contract. The chat
/// VLM cost contract remains a separate identity.
pub const NATIVE_STT_COST_BOUND_CONTRACT_VERSION: &str =
    "openrouter.native-stt-duration-reservation.v1";
/// The native STT endpoint has no `max_price` routing control. This contract is
/// an explicit external-tariff reservation, never a hard provider invoice cap.
const NATIVE_STT_BILLING_SEMANTICS: &str =
    "full-registered-audio-window-admission-reservation;external-tariff-not-wire-enforced";

mod client;
mod decode;
#[cfg(test)]
mod tests;
pub use client::{NativeSttClientError, OpenRouterSttClient};
pub use decode::*;

/// Verified native transcription capability for one provider-qualified model.
///
/// The single registry entry is intentional. A model identifier is not evidence
/// that a chat model can serve audio transcription; callers must resolve an
/// exact provider/model pair here before any request body can be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeSttModelSpec {
    pub provider: &'static str,
    pub model: &'static str,
    pub endpoint: &'static str,
    pub response_format: &'static str,
    pub timestamp_granularities: &'static [&'static str],
    pub supported_input_formats: &'static [&'static str],
    pub supports_language: bool,
    pub max_window_ms: u32,
    /// Approved external-tariff ceiling in integer micro-USD per second. Integer
    /// units keep the registry `Eq` and make the contract identity deterministic.
    pub duration_price_micro_usd_per_second: u64,
}

/// Public OpenRouter model and endpoint snapshots checked 2026-10-25. Only
/// this exact provider-qualified entry is admitted; later models require a
/// deliberate contract update and tests rather than implicit slug acceptance.
pub const NATIVE_STT_MODELS: &[NativeSttModelSpec] = &[NativeSttModelSpec {
    provider: OPENROUTER_STT_PROVIDER,
    model: OPENROUTER_WHISPER_MODEL,
    endpoint: OPENROUTER_STT_ENDPOINT,
    response_format: "verbose_json",
    timestamp_granularities: &["segment"],
    supported_input_formats: &["wav"],
    supports_language: true,
    max_window_ms: WINDOW_MS,
    // Public OpenRouter endpoint snapshot checked 2026-10-25:
    // $0.0001 USD/second = 100 micro-USD/second.
    duration_price_micro_usd_per_second: 100,
}];

fn native_stt_cost_bound_contract_id_for(models: &[NativeSttModelSpec]) -> String {
    let mut hasher = blake3::Hasher::new();
    let add = |hasher: &mut blake3::Hasher, bytes: &[u8]| {
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    add(
        &mut hasher,
        NATIVE_STT_COST_BOUND_CONTRACT_VERSION.as_bytes(),
    );
    add(&mut hasher, NATIVE_STT_BILLING_SEMANTICS.as_bytes());
    for spec in models {
        add(&mut hasher, spec.provider.as_bytes());
        add(&mut hasher, spec.model.as_bytes());
        add(&mut hasher, spec.endpoint.as_bytes());
        hasher.update(&spec.max_window_ms.to_le_bytes());
        hasher.update(&spec.duration_price_micro_usd_per_second.to_le_bytes());
    }
    format!(
        "{NATIVE_STT_COST_BOUND_CONTRACT_VERSION}:{}",
        hasher.finalize().to_hex()
    )
}

/// Deterministic identity of the registered native endpoint, window limit and
/// approved duration-rate ceiling. Route/rate/window changes invalidate old
/// approvals instead of silently reusing them.
pub fn native_stt_cost_bound_contract_id() -> String {
    native_stt_cost_bound_contract_id_for(NATIVE_STT_MODELS)
}

/// Resolves an explicit provider/model pair to its verified native transcription
/// contract. No default model, hidden fallback or chat-model inference exists.
pub fn resolve_native_stt_model(
    provider: &str,
    model: &str,
) -> Result<&'static NativeSttModelSpec> {
    NATIVE_STT_MODELS
        .iter()
        .find(|spec| spec.provider == provider && spec.model == model)
        .ok_or_else(|| {
            anyhow!(
                "unsupported native STT provider/model pair; select an explicit provider-qualified model with a verified audio-transcription contract"
            )
        })
}

/// Explicit backend choice. Cloud work must never be selected implicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeechBackendSelection {
    /// Existing local CPU whisper.cpp path.
    LocalWhisper,
    /// Native OpenRouter transcription with an explicit provider-qualified model.
    NativeOpenRouter { provider: String, model: String },
}

/// The resolved local or cloud contract. No model is loaded here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedSpeechBackend {
    LocalWhisper {
        model: &'static str,
        vad_model: &'static str,
    },
    NativeOpenRouter {
        spec: &'static NativeSttModelSpec,
    },
}

/// Resolves an explicit backend selection while leaving production wiring to a
/// later slice.
pub fn resolve_speech_backend(selection: &SpeechBackendSelection) -> Result<ResolvedSpeechBackend> {
    match selection {
        SpeechBackendSelection::LocalWhisper => Ok(ResolvedSpeechBackend::LocalWhisper {
            model: WHISPER_MODEL.file_name,
            vad_model: VAD_MODEL.file_name,
        }),
        SpeechBackendSelection::NativeOpenRouter { provider, model } => {
            Ok(ResolvedSpeechBackend::NativeOpenRouter {
                spec: resolve_native_stt_model(provider, model)?,
            })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SttDataCollection {
    Allow,
    Deny,
}

impl SttDataCollection {
    fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }

    fn parse(value: &Value) -> Result<Self> {
        match value.as_str() {
            Some("allow") => Ok(Self::Allow),
            Some("deny") => Ok(Self::Deny),
            _ => bail!("native STT data_collection must be 'allow' or 'deny'"),
        }
    }
}

/// The only provider routing fields supported by the native STT endpoint.
///
/// A caller can explicitly select zero-data-retention and/or a data-collection
/// policy. Other provider routing keys fail before any body is built.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NativeSttProviderPolicy {
    pub zero_data_retention: Option<bool>,
    pub data_collection: Option<SttDataCollection>,
}

impl NativeSttProviderPolicy {
    pub fn new(zero_data_retention: bool, data_collection: SttDataCollection) -> Self {
        Self {
            zero_data_retention: Some(zero_data_retention),
            data_collection: Some(data_collection),
        }
    }

    pub fn zdr_only(zero_data_retention: bool) -> Self {
        Self {
            zero_data_retention: Some(zero_data_retention),
            data_collection: None,
        }
    }

    pub fn data_collection_only(data_collection: SttDataCollection) -> Self {
        Self {
            zero_data_retention: None,
            data_collection: Some(data_collection),
        }
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.zero_data_retention.is_some() || self.data_collection.is_some(),
            "native STT requires an explicit supported provider data policy"
        );
        Ok(())
    }

    /// Serializes only the two supported data-policy fields.
    pub fn to_wire_value(&self) -> Result<Value> {
        self.validate()?;
        let mut provider = Map::new();
        if let Some(zero_data_retention) = self.zero_data_retention {
            provider.insert("zdr".to_string(), Value::Bool(zero_data_retention));
        }
        if let Some(data_collection) = self.data_collection {
            provider.insert(
                "data_collection".to_string(),
                Value::String(data_collection.as_str().to_string()),
            );
        }
        Ok(Value::Object(provider))
    }

    /// Parses a provider-routing object and rejects every key that the STT
    /// endpoint does not apply. This is the explicit pre-upload route gate.
    pub fn from_wire_value(value: &Value) -> Result<Self> {
        let object = value
            .as_object()
            .ok_or_else(|| anyhow!("native STT provider routing must be an object"))?;
        let mut policy = Self::default();
        for (key, value) in object {
            match key.as_str() {
                "zdr" => {
                    policy.zero_data_retention = Some(
                        value
                            .as_bool()
                            .ok_or_else(|| anyhow!("native STT zdr must be a boolean"))?,
                    );
                }
                "data_collection" => {
                    policy.data_collection = Some(SttDataCollection::parse(value)?);
                }
                _ => bail!(
                    "unsupported native STT provider routing restriction; only zdr and data_collection are applied"
                ),
            }
        }
        policy.validate()?;
        Ok(policy)
    }
}

impl Serialize for NativeSttProviderPolicy {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.to_wire_value()
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for NativeSttProviderPolicy {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        Self::from_wire_value(&value).map_err(serde::de::Error::custom)
    }
}

/// One explicitly selected native transcription request.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeSttRequest {
    provider: String,
    model: String,
    language: Option<String>,
    temperature: f64,
    provider_policy: NativeSttProviderPolicy,
}

impl NativeSttRequest {
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        language: Option<String>,
        temperature: f64,
        provider_policy: NativeSttProviderPolicy,
    ) -> Result<Self> {
        let request = Self {
            provider: provider.into(),
            model: model.into(),
            language,
            temperature,
            provider_policy,
        };
        request.validate()?;
        Ok(request)
    }

    fn validate(&self) -> Result<&'static NativeSttModelSpec> {
        let spec = resolve_native_stt_model(&self.provider, &self.model)?;
        ensure!(
            spec.supported_input_formats.contains(&"wav"),
            "selected native STT model does not support WAV input"
        );
        if let Some(language) = &self.language {
            validate_iso639_1(language)?;
        }
        ensure!(
            self.temperature.is_finite() && (0.0..=1.0).contains(&self.temperature),
            "native STT temperature must be finite and between 0 and 1"
        );
        self.provider_policy.validate()?;
        Ok(spec)
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    pub fn temperature(&self) -> f64 {
        self.temperature
    }

    pub fn provider_policy(&self) -> NativeSttProviderPolicy {
        self.provider_policy
    }
}

/// Request language must be an explicit ISO 639-1 code.
pub fn validate_iso639_1(language: &str) -> Result<()> {
    ensure!(
        language.len() == 2
            && language.bytes().all(|byte| byte.is_ascii_lowercase())
            && (0..=whisper_rs::get_lang_max_id())
                .any(|id| whisper_rs::get_lang_str(id) == Some(language)),
        "native STT language must be an explicit lowercase ISO 639-1 code"
    );
    Ok(())
}

/// A strictly bounded chunk of speech from one packed speech window.
///
/// Construction is the only way to attach audio to a request. The fields are
/// private so callers cannot forge a duration, sample count or WAV byte string.
#[derive(Clone, PartialEq, Eq)]
pub struct NativeSttAudioWindow {
    window: PackedWindow,
    wav_bytes: Vec<u8>,
    duration_ms: u32,
    sample_count: usize,
}

impl std::fmt::Debug for NativeSttAudioWindow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeSttAudioWindow")
            .field("duration_ms", &self.duration_ms)
            .field("sample_count", &self.sample_count)
            .finish_non_exhaustive()
    }
}

impl NativeSttAudioWindow {
    /// Concatenates only the window's selected speech regions from supplied PCM,
    /// then encodes a genuine bounded 16 kHz mono PCM WAV.
    pub fn from_pcm(pcm: &[f32], window: &PackedWindow) -> Result<Self> {
        let samples = concatenate_regions_bounded(pcm, window)?;
        let duration_ms = window.speech_ms();
        ensure!(duration_ms > 0, "speech window is empty");
        ensure!(duration_ms <= WINDOW_MS, "speech window exceeds 30 seconds");
        let wav_bytes = encode_pcm16_wav(&samples)?;
        let sample_count = validate_wav_16k_mono(&wav_bytes)?;
        ensure!(
            sample_count == samples.len(),
            "encoded WAV sample count changed unexpectedly"
        );
        Ok(Self {
            window: window.clone(),
            wav_bytes,
            duration_ms,
            sample_count,
        })
    }

    pub fn window(&self) -> &PackedWindow {
        &self.window
    }

    pub fn wav_bytes(&self) -> &[u8] {
        &self.wav_bytes
    }

    pub fn duration_ms(&self) -> u32 {
        self.duration_ms
    }

    pub fn sample_count(&self) -> usize {
        self.sample_count
    }

    fn validate_for(&self, spec: &NativeSttModelSpec) -> Result<()> {
        ensure!(self.duration_ms > 0, "speech window is empty");
        ensure!(
            self.duration_ms <= spec.max_window_ms,
            "speech window exceeds the selected model's duration limit"
        );
        ensure!(
            self.wav_bytes.len() <= MAX_SPEECH_WAV_BYTES,
            "speech WAV exceeds the bounded wire size"
        );
        let sample_count = validate_wav_16k_mono(&self.wav_bytes)?;
        ensure!(
            sample_count == self.sample_count,
            "speech WAV sample count does not match the bounded window"
        );
        ensure!(
            self.window.speech_ms() == self.duration_ms,
            "speech WAV duration does not match its packed window"
        );
        Ok(())
    }
}

/// Produces the conservative admission quote for one already validated native
/// audio request and one bounded speech window.
///
/// The quote reserves the full registered 30-second window at the approved
/// external-tariff rate even when the actual speech window is shorter. The
/// native endpoint does not enforce `provider.max_price`; this reservation is
/// not a provider invoice cap. Any invoice above the bound remains an external
/// fact for the existing receipt settlement owner to record.
pub fn native_stt_request_cost_bound(
    request: &NativeSttRequest,
    audio: &NativeSttAudioWindow,
) -> Result<AnalysisRequestCostBound> {
    let spec = request.validate()?;
    audio.validate_for(spec)?;
    let billable_seconds_max = spec.max_window_ms.div_ceil(1_000);
    let duration_price_usd_per_second =
        spec.duration_price_micro_usd_per_second as f64 / 1_000_000.0;
    let mut bound = AnalysisRequestCostBound {
        maximum_cost_usd: 0.0,
        contract_version: native_stt_cost_bound_contract_id(),
        prompt_tokens_max: 0,
        completion_tokens_max: 0,
        image_count: 0,
        prompt_price_usd_per_million: 0.0,
        completion_price_usd_per_million: 0.0,
        image_price_usd_each: 0.0,
        request_price_usd_each: 0.0,
        audio: Some(AnalysisAudioCostBound {
            input_duration_ms: audio.duration_ms(),
            billable_seconds_max,
            duration_price_usd_per_second,
        }),
        assumptions: vec![
            "the request uses the exact registered native audio-transcription provider/model endpoint and bounded WAV wire contract".into(),
            "the reservation bills the full registered audio window rather than the shorter observed speech duration".into(),
            "the reservation uses the registered approved external-tariff duration-rate ceiling and has no prompt, completion or image components".into(),
            "the native STT endpoint does not enforce provider.max_price, order, only or ignore; this reservation is not a provider invoice cap".into(),
            "an external invoice above this reservation and unknown usage remain explicit external facts for existing receipt settlement and authority stopping".into(),
        ],
    };
    bound.maximum_cost_usd = bound.computed_maximum_cost_usd()?;
    bound.validate()?;
    Ok(bound)
}

/// Runs the supplied local detector and builds one bounded WAV per packed speech
/// window. Silence returns an empty vector, so callers have no request to send.
/// This function performs no provider I/O.
pub fn plan_native_stt_audio_windows<F>(
    pcm: &[f32],
    mut detect_speech: F,
) -> Result<Vec<NativeSttAudioWindow>>
where
    F: FnMut(&[f32]) -> Result<Vec<SpeechRegion>>,
{
    let regions = detect_speech(pcm)?;
    let mut previous_end_ms = 0;
    for region in &regions {
        ensure!(
            region.end_ms > region.start_ms && region.start_ms >= previous_end_ms,
            "invalid or overlapping VAD speech ranges"
        );
        ensure!(
            (region.end_ms as u64) * 16 <= pcm.len() as u64,
            "VAD speech range exceeds supplied audio"
        );
        previous_end_ms = region.end_ms;
    }
    let speech_ms = regions.iter().try_fold(0_u32, |total, region| {
        total
            .checked_add(region.duration_ms())
            .ok_or_else(|| anyhow!("speech region duration overflowed"))
    })?;
    if speech_ms == 0 {
        return Ok(Vec::new());
    }
    pack_windows(&regions)
        .into_iter()
        .map(|window| NativeSttAudioWindow::from_pcm(pcm, &window))
        .collect()
}

/// Builds the exact native transcription request body. Pure and offline.
pub fn build_native_stt_request_body(
    request: &NativeSttRequest,
    audio: &NativeSttAudioWindow,
) -> Result<Value> {
    let spec = request.validate()?;
    audio.validate_for(spec)?;

    let data = base64::engine::general_purpose::STANDARD.encode(&audio.wav_bytes);
    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(request.model.clone()));
    body.insert(
        "input_audio".to_string(),
        json!({
            "data": data,
            "format": "wav",
        }),
    );
    if let Some(language) = &request.language {
        body.insert("language".to_string(), Value::String(language.clone()));
    }
    body.insert("temperature".to_string(), json!(request.temperature));
    body.insert(
        "response_format".to_string(),
        Value::String(spec.response_format.to_string()),
    );
    body.insert(
        "timestamp_granularities".to_string(),
        json!(spec.timestamp_granularities),
    );
    body.insert(
        "provider".to_string(),
        request.provider_policy.to_wire_value()?,
    );
    Ok(Value::Object(body))
}
