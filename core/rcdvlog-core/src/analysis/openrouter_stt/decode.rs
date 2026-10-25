//! Native response validation and receipt extraction, without provider I/O.
use super::*;

const MAX_PROVENANCE_BYTES: usize = 256;
pub(super) const MAX_GENERATION_ID_BYTES: usize = 512;
const MAX_LANGUAGE_BYTES: usize = 35;
const MAX_SEGMENT_TEXT_BYTES: usize = 64 * 1024;
const MAX_SEGMENTS: usize = 10_000;
const MAX_WORDS_PER_SEGMENT: usize = 10_000;

/// Stable, body-free protocol failure categories. Provider response text is
/// never interpolated into these values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeSttProtocolError {
    InvalidJson,
    NotAnObject,
    MissingText,
    InvalidText,
    MissingLanguage,
    InvalidLanguage,
    MissingDuration,
    InvalidDuration,
    MissingSegments,
    InvalidSegments,
    EmptySegments,
    TooManySegments,
    InvalidSegment,
    MissingTimestamp,
    InvalidTimestamp,
    OutOfOrderTimestamp,
    InvalidQualityEvidence,
    InvalidWindow,
    ResponseTooLarge,
    BodyReadFailed,
    HttpStatus(u16),
}

impl NativeSttProtocolError {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidJson => "invalid_json",
            Self::NotAnObject => "not_an_object",
            Self::MissingText => "missing_text",
            Self::InvalidText => "invalid_text",
            Self::MissingLanguage => "missing_language",
            Self::InvalidLanguage => "invalid_language",
            Self::MissingDuration => "missing_duration",
            Self::InvalidDuration => "invalid_duration",
            Self::MissingSegments => "missing_segments",
            Self::InvalidSegments => "invalid_segments",
            Self::EmptySegments => "empty_segments",
            Self::TooManySegments => "too_many_segments",
            Self::InvalidSegment => "invalid_segment",
            Self::MissingTimestamp => "missing_timestamp",
            Self::InvalidTimestamp => "invalid_timestamp",
            Self::OutOfOrderTimestamp => "out_of_order_timestamp",
            Self::InvalidQualityEvidence => "invalid_quality_evidence",
            Self::InvalidWindow => "invalid_window",
            Self::ResponseTooLarge => "response_too_large",
            Self::BodyReadFailed => "body_read_failed",
            Self::HttpStatus(_) => "http_status",
        }
    }
}

impl std::fmt::Display for NativeSttProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HttpStatus(status) => {
                write!(formatter, "native_stt_protocol:http_status:{status}")
            }
            _ => write!(formatter, "native_stt_protocol:{}", self.code()),
        }
    }
}

impl std::error::Error for NativeSttProtocolError {}

/// Measured latency and receipts are kept even when the transcript is unusable.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeSttCallResult {
    pub receipt: NativeSttReceipt,
    pub latency_ms: u64,
    pub outcome: NativeSttOutcome,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NativeSttOutcome {
    Usable(NativeSttTranscript),
    Unusable(NativeSttProtocolError),
}

#[derive(Debug, Clone, PartialEq)]
pub struct NativeSttTranscript {
    pub text: String,
    pub language: String,
    /// Optional full-transcript confidence reported by the provider.
    pub confidence: Option<f64>,
    /// Provider-reported duration in window milliseconds.
    pub duration_ms: u32,
    pub segments: Vec<NativeSttSegment>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NativeSttSegment {
    pub start_ms: u32,
    pub end_ms: u32,
    pub text: String,
    /// Optional provider evidence. Missing stays missing, never 0 or 1.
    pub confidence: Option<f64>,
    /// Optional provider log-probability evidence. Missing stays missing.
    pub avg_logprob: Option<f64>,
    pub words: Vec<NativeSttWord>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NativeSttWord {
    pub text: String,
    pub start_ms: Option<u32>,
    pub end_ms: Option<u32>,
    /// Optional word confidence. Missing stays missing.
    pub confidence: Option<f64>,
}

/// Provider-reported cost. Absent or malformed usage remains explicitly unknown.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum NativeSttCost {
    Known(f64),
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NativeSttUsage {
    pub cost: NativeSttCost,
    pub seconds: Option<f64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

/// Provenance explicitly returned by the provider or trusted response header.
/// The requested provider/model is never copied into these fields.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NativeSttReceipt {
    pub generation_id: Option<String>,
    pub actual_model: Option<String>,
    pub actual_provider: Option<String>,
    pub usage: NativeSttUsage,
}

/// Parses the receipt portion independently of transcript usability.
pub fn parse_native_stt_receipt(
    payload: &Value,
    generation_id: Option<String>,
) -> NativeSttReceipt {
    let usage_value = payload.get("usage");
    let cost = usage_value
        .and_then(|usage| usage.get("cost"))
        .and_then(Value::as_f64)
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
        .map(NativeSttCost::Known)
        .unwrap_or_default();
    let seconds = usage_value
        .and_then(|usage| usage.get("seconds"))
        .and_then(Value::as_f64)
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0);
    let input_tokens = usage_value
        .and_then(|usage| usage.get("input_tokens"))
        .and_then(Value::as_u64);
    let output_tokens = usage_value
        .and_then(|usage| usage.get("output_tokens"))
        .and_then(Value::as_u64);

    NativeSttReceipt {
        generation_id: generation_id.and_then(|value| bounded_generation_id(&value)),
        actual_model: payload
            .get("model")
            .and_then(Value::as_str)
            .and_then(|value| bounded_provenance(value, MAX_PROVENANCE_BYTES)),
        actual_provider: payload
            .get("provider")
            .and_then(Value::as_str)
            .and_then(|value| bounded_provenance(value, MAX_PROVENANCE_BYTES)),
        usage: NativeSttUsage {
            cost,
            seconds,
            input_tokens,
            output_tokens,
        },
    }
}

/// Parses and validates a successful HTTP body. A malformed body still yields a
/// call result with the trusted header receipt so future accounting can
/// reconcile the paid request instead of losing it.
pub fn decode_native_stt_body(
    body: &[u8],
    window_duration_ms: u32,
    generation_id: Option<String>,
    latency_ms: u64,
) -> NativeSttCallResult {
    if body.len() > MAX_NATIVE_STT_RESPONSE_BYTES {
        return NativeSttCallResult {
            receipt: NativeSttReceipt {
                generation_id: generation_id.and_then(|value| bounded_generation_id(&value)),
                ..NativeSttReceipt::default()
            },
            latency_ms,
            outcome: NativeSttOutcome::Unusable(NativeSttProtocolError::ResponseTooLarge),
        };
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(payload) => {
            let receipt = parse_native_stt_receipt(&payload, generation_id);
            let outcome = decode_native_stt_value(&payload, window_duration_ms)
                .map(NativeSttOutcome::Usable)
                .unwrap_or_else(NativeSttOutcome::Unusable);
            NativeSttCallResult {
                receipt,
                latency_ms,
                outcome,
            }
        }
        Err(_) => NativeSttCallResult {
            receipt: NativeSttReceipt {
                generation_id: generation_id.and_then(|value| bounded_generation_id(&value)),
                ..NativeSttReceipt::default()
            },
            latency_ms,
            outcome: NativeSttOutcome::Unusable(NativeSttProtocolError::InvalidJson),
        },
    }
}

/// Strictly validates a verbose_json transcript in window time.
pub fn decode_native_stt_value(
    payload: &Value,
    window_duration_ms: u32,
) -> std::result::Result<NativeSttTranscript, NativeSttProtocolError> {
    if window_duration_ms == 0 || window_duration_ms > WINDOW_MS {
        return Err(NativeSttProtocolError::InvalidWindow);
    }
    let object = payload
        .as_object()
        .ok_or(NativeSttProtocolError::NotAnObject)?;
    let text = required_text(
        object.get("text"),
        NativeSttProtocolError::MissingText,
        NativeSttProtocolError::InvalidText,
    )?;
    let language = required_language(object.get("language"))?;
    let confidence = optional_confidence(object.get("confidence"))?;
    let duration_seconds = object
        .get("duration")
        .ok_or(NativeSttProtocolError::MissingDuration)?
        .as_f64()
        .ok_or(NativeSttProtocolError::InvalidDuration)?;
    let duration_ms = seconds_to_window_ms(duration_seconds, window_duration_ms)?;
    if duration_ms == 0 {
        return Err(NativeSttProtocolError::InvalidDuration);
    }

    let segments_value = object
        .get("segments")
        .ok_or(NativeSttProtocolError::MissingSegments)?;
    let segments_array = segments_value
        .as_array()
        .ok_or(NativeSttProtocolError::InvalidSegments)?;
    if segments_array.is_empty() {
        return Err(NativeSttProtocolError::EmptySegments);
    }
    if segments_array.len() > MAX_SEGMENTS {
        return Err(NativeSttProtocolError::TooManySegments);
    }

    let mut segments = Vec::with_capacity(segments_array.len());
    let mut previous_end_ms = 0_u32;
    for value in segments_array {
        let segment = value
            .as_object()
            .ok_or(NativeSttProtocolError::InvalidSegment)?;
        let text = required_text(
            segment.get("text"),
            NativeSttProtocolError::InvalidSegment,
            NativeSttProtocolError::InvalidText,
        )?;
        let start_ms = required_timestamp(segment.get("start"), window_duration_ms)?;
        let end_ms = required_timestamp(segment.get("end"), window_duration_ms)?;
        if start_ms >= end_ms || end_ms > duration_ms {
            return Err(NativeSttProtocolError::InvalidTimestamp);
        }
        if start_ms < previous_end_ms {
            return Err(NativeSttProtocolError::OutOfOrderTimestamp);
        }
        let confidence = optional_confidence(segment.get("confidence"))?;
        let avg_logprob = optional_logprob(segment.get("avg_logprob"))?;
        let words = parse_words(segment.get("words"), window_duration_ms)?;
        previous_end_ms = end_ms;
        segments.push(NativeSttSegment {
            start_ms,
            end_ms,
            text,
            confidence,
            avg_logprob,
            words,
        });
    }

    Ok(NativeSttTranscript {
        text,
        language,
        confidence,
        duration_ms,
        segments,
    })
}

/// Converts provider seconds into rounding-stable window milliseconds with
/// finite, nonnegative and upper-bound checks. Public so protocol tests can
/// exercise NaN/infinity without a JSON transport.
pub fn seconds_to_window_ms(
    seconds: f64,
    window_duration_ms: u32,
) -> std::result::Result<u32, NativeSttProtocolError> {
    if window_duration_ms == 0
        || window_duration_ms > WINDOW_MS
        || !seconds.is_finite()
        || seconds < 0.0
        || seconds * 1_000.0 > f64::from(window_duration_ms)
    {
        return Err(NativeSttProtocolError::InvalidTimestamp);
    }
    let milliseconds = (seconds * 1_000.0).round();
    if !milliseconds.is_finite()
        || milliseconds < 0.0
        || milliseconds > f64::from(window_duration_ms)
    {
        return Err(NativeSttProtocolError::InvalidTimestamp);
    }
    Ok(milliseconds as u32)
}

fn required_text(
    value: Option<&Value>,
    missing: NativeSttProtocolError,
    invalid: NativeSttProtocolError,
) -> std::result::Result<String, NativeSttProtocolError> {
    let value = value.ok_or(missing)?;
    let text = value.as_str().ok_or(invalid)?;
    let trimmed = text.trim();
    if trimmed.is_empty() || text.len() > MAX_SEGMENT_TEXT_BYTES {
        return Err(invalid);
    }
    Ok(trimmed.to_string())
}

fn required_language(value: Option<&Value>) -> std::result::Result<String, NativeSttProtocolError> {
    let value = value.ok_or(NativeSttProtocolError::MissingLanguage)?;
    let language = value
        .as_str()
        .ok_or(NativeSttProtocolError::InvalidLanguage)?;
    if !valid_language_tag(language) {
        return Err(NativeSttProtocolError::InvalidLanguage);
    }
    Ok(language.to_string())
}

fn valid_language_tag(value: &str) -> bool {
    let bytes = value.as_bytes();
    (2..=MAX_LANGUAGE_BYTES).contains(&bytes.len())
        && bytes[0].is_ascii_alphabetic()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        && (0..=whisper_rs::get_lang_max_id()).any(|id| {
            whisper_rs::get_lang_str(id) == Some(value)
                || whisper_rs::get_lang_str_full(id) == Some(value)
        })
}

fn required_timestamp(
    value: Option<&Value>,
    window_duration_ms: u32,
) -> std::result::Result<u32, NativeSttProtocolError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Err(NativeSttProtocolError::MissingTimestamp);
    };
    let seconds = value
        .as_f64()
        .ok_or(NativeSttProtocolError::InvalidTimestamp)?;
    seconds_to_window_ms(seconds, window_duration_ms)
}

fn optional_confidence(
    value: Option<&Value>,
) -> std::result::Result<Option<f64>, NativeSttProtocolError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let confidence = value
        .as_f64()
        .ok_or(NativeSttProtocolError::InvalidQualityEvidence)?;
    if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
        return Err(NativeSttProtocolError::InvalidQualityEvidence);
    }
    Ok(Some(confidence))
}

fn optional_logprob(
    value: Option<&Value>,
) -> std::result::Result<Option<f64>, NativeSttProtocolError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let logprob = value
        .as_f64()
        .ok_or(NativeSttProtocolError::InvalidQualityEvidence)?;
    if !logprob.is_finite() || logprob > 0.0 {
        return Err(NativeSttProtocolError::InvalidQualityEvidence);
    }
    Ok(Some(logprob))
}

fn parse_words(
    value: Option<&Value>,
    window_duration_ms: u32,
) -> std::result::Result<Vec<NativeSttWord>, NativeSttProtocolError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(Vec::new());
    };
    let words = value
        .as_array()
        .ok_or(NativeSttProtocolError::InvalidQualityEvidence)?;
    if words.len() > MAX_WORDS_PER_SEGMENT {
        return Err(NativeSttProtocolError::InvalidQualityEvidence);
    }
    let mut parsed = Vec::with_capacity(words.len());
    for word in words {
        let word = word
            .as_object()
            .ok_or(NativeSttProtocolError::InvalidQualityEvidence)?;
        let text = word
            .get("word")
            .or_else(|| word.get("text"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty() && text.len() <= MAX_SEGMENT_TEXT_BYTES)
            .ok_or(NativeSttProtocolError::InvalidQualityEvidence)?;
        let start_ms = optional_word_timestamp(word.get("start"), window_duration_ms)?;
        let end_ms = optional_word_timestamp(word.get("end"), window_duration_ms)?;
        match (start_ms, end_ms) {
            (Some(start_ms), Some(end_ms)) if start_ms < end_ms => {}
            (None, None) => {}
            _ => return Err(NativeSttProtocolError::InvalidTimestamp),
        }
        let confidence = optional_confidence(word.get("confidence"))?;
        parsed.push(NativeSttWord {
            text: text.to_string(),
            start_ms,
            end_ms,
            confidence,
        });
    }
    Ok(parsed)
}

fn optional_word_timestamp(
    value: Option<&Value>,
    window_duration_ms: u32,
) -> std::result::Result<Option<u32>, NativeSttProtocolError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let seconds = value
        .as_f64()
        .ok_or(NativeSttProtocolError::InvalidTimestamp)?;
    Ok(Some(seconds_to_window_ms(seconds, window_duration_ms)?))
}

pub(super) fn bounded_generation_id(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= MAX_GENERATION_ID_BYTES
        && value.bytes().all(|byte| byte.is_ascii_graphic()))
    .then(|| value.to_string())
}

fn bounded_provenance(value: &str, limit: usize) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control))
        .then(|| value.to_string())
}
