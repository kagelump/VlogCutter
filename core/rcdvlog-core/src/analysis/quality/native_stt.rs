//! Pure quality evaluation for provider-returned native speech transcripts.
//!
//! This module is deliberately a quality leaf: it takes an already decoded
//! [`NativeSttTranscript`] and the exact validated [`NativeSttAudioWindow`] it
//! came from, then maps candidate segments back to source time and evaluates the
//! evidence the provider actually reported. It performs no I/O, persistence,
//! accounting, VAD, retry or orchestration, and it does not select a backend.
//!
//! Native confidence and average log-probability are *different*, uncalibrated
//! provider heuristics. Neither is arithmetic mean token probability, so mapped
//! [`DecodedSegment::mean_token_probability`] is always `None`. This evaluator
//! makes no empirical-superiority claim for either heuristic.
//!
//! The evidence rules are intentionally conservative:
//! - repetition at or above the existing local threshold rejects;
//! - a reported (possibly incomplete) segment mean average log-probability below
//!   `ln(0.20)` rejects;
//! - a reported (possibly incomplete) segment mean provider confidence below
//!   `0.20` rejects;
//! - a reported full-transcript confidence below `0.20` rejects;
//! - invalid supplied quality evidence rejects before any candidate text is
//!   returned;
//! - at least one *complete* comparable metric must be reported to accept. If
//!   only missing or partial comparable evidence was supplied, the result is
//!   [`NativeSttQualityDecision::ReviewRequired`] and remains unaccepted.
//!
//! `ReviewRequired` retains candidate text solely so a human can review it.
//! Canonical persistence now has a distinct review-required status contract, but
//! this pure evaluator still does not write it or enable native execution; the
//! candidates remain unaccepted and must not be shown as accepted speech.

use serde::{Deserialize, Serialize};

use crate::analysis::openrouter_stt::{
    NativeSttAudioWindow, NativeSttSegment, NativeSttTranscript,
};
use crate::analysis::windows::translate_span;

use super::{DecodedSegment, REPETITION_REJECT_RATIO, repetition_ratio};

/// Identity of the native quality heuristic recipe.
///
/// This is separate from the local whisper recipe and native request contract.
/// A change to these heuristic rules must bump this version before any future
/// persistence slice uses it as a cache/status identity.
pub const NATIVE_STT_QUALITY_RECIPE_VERSION: &str = "native-stt.quality.v1";

/// Reject when the arithmetic mean of reported segment `avg_logprob` values is
/// below `ln(0.20)`.
///
/// This is the natural-log mean of *reported segment values*. It is neither a
/// token-weighted aggregate nor an estimate of arithmetic mean token
/// probability. `avg_logprob` is an uncalibrated provider heuristic.
pub const NATIVE_STT_AVG_LOGPROB_LN_0_20_REJECT_THRESHOLD: f64 = -1.609_437_912_434_100_3;

/// Reject when the arithmetic mean of reported segment confidence values, or a
/// reported full-transcript confidence, is below `0.20`.
///
/// Provider confidence is an uncalibrated heuristic and is not token
/// probability. The threshold is intentionally separate from the log-probability
/// threshold.
pub const NATIVE_STT_PROVIDER_CONFIDENCE_REJECT_THRESHOLD: f64 = 0.20;

/// Terminal quality decision for native provider output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeSttQualityDecision {
    /// At least one complete comparable quality metric passed and no known-bad
    /// evidence, invalid evidence or repetition was found.
    Accepted,
    /// The provider output was structurally unusable or failed a quality gate.
    /// Candidate text is discarded.
    Rejected,
    /// Candidate text was returned, but quality evidence is absent or partial.
    /// The candidates are unaccepted and require human review.
    ReviewRequired,
}

impl NativeSttQualityDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::ReviewRequired => "review_required",
        }
    }
}

/// Stable, body-free reason for a native quality decision.
///
/// The values intentionally contain no provider text, transcript text, audio,
/// paths or credentials. [`Self::code`] is the stable machine-readable form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeSttQualityReason {
    /// At least one complete comparable metric passed.
    AcceptedCompleteEvidence,
    /// No comparable quality metric was reported.
    ReviewMissingQualityEvidence,
    /// Some comparable evidence was reported, but no metric covered all
    /// candidate segments and no full-transcript confidence was present.
    ReviewIncompleteQualityEvidence,
    /// The provider returned an empty usable result (for example no timed
    /// segment text). This is not local silence and must not become a
    /// `no_speech` or `no_audio` outcome.
    EmptyProviderOutput,
    /// The reported language could not be resolved through the existing
    /// whisper language registry.
    UnknownLanguage,
    /// The reported duration was zero or exceeded the validated audio window.
    InvalidDuration,
    /// The transcript or a segment had missing/blank text.
    InvalidText,
    /// Provider timestamps were reversed, overlapping, out of order or outside
    /// the reported duration/actual audio window.
    InvalidTimestamp,
    /// A supplied confidence or average log-probability value was non-finite or
    /// outside its valid range.
    InvalidQualityEvidence,
    /// Repetition met the existing local rejection threshold.
    Repetition,
    /// The mean of reported segment average log-probabilities was below
    /// [`NATIVE_STT_AVG_LOGPROB_LN_0_20_REJECT_THRESHOLD`].
    LowAverageLogProbability,
    /// A reported segment or full-transcript provider confidence mean was below
    /// [`NATIVE_STT_PROVIDER_CONFIDENCE_REJECT_THRESHOLD`].
    LowProviderConfidence,
}

impl NativeSttQualityReason {
    /// Stable body-free reason code suitable for diagnostics or future storage.
    pub fn code(self) -> &'static str {
        match self {
            Self::AcceptedCompleteEvidence => "accepted_complete_evidence",
            Self::ReviewMissingQualityEvidence => "review_missing_quality_evidence",
            Self::ReviewIncompleteQualityEvidence => "review_incomplete_quality_evidence",
            Self::EmptyProviderOutput => "empty_provider_output",
            Self::UnknownLanguage => "unknown_language",
            Self::InvalidDuration => "invalid_duration",
            Self::InvalidText => "invalid_text",
            Self::InvalidTimestamp => "invalid_timestamp",
            Self::InvalidQualityEvidence => "invalid_quality_evidence",
            Self::Repetition => "repetition",
            Self::LowAverageLogProbability => "low_average_log_probability",
            Self::LowProviderConfidence => "low_provider_confidence",
        }
    }

    /// Alias matching the existing local quality-reason convention.
    pub fn as_str(self) -> &'static str {
        self.code()
    }
}

/// Coverage and complete evidence for one segment-level native metric.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSttMetricSummary {
    /// Segments that supplied a valid value for this metric.
    pub known_segments: usize,
    /// Candidate segments considered.
    pub total_segments: usize,
    /// Arithmetic mean only when every candidate segment supplied a valid value
    /// and the overflow-safe aggregate is finite. Otherwise `None`; a partial
    /// estimate is never presented as a complete metric.
    pub mean: Option<f64>,
}

impl NativeSttMetricSummary {
    fn from_values(values: &[f64], total_segments: usize) -> Self {
        let mean = (total_segments > 0 && values.len() == total_segments)
            .then(|| finite_mean(values))
            .flatten();
        Self {
            known_segments: values.len(),
            total_segments,
            mean,
        }
    }
}

/// Word-confidence coverage.
///
/// Word confidence is validated when supplied but is deliberately not used as
/// segment confidence, full-transcript confidence or token probability, and no
/// word-weighted aggregate is computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSttWordConfidenceCoverage {
    pub known_words: usize,
    pub total_words: usize,
}

/// Comparable metric summaries and coverage for one native provider result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSttQualityMetrics {
    pub avg_logprob: NativeSttMetricSummary,
    pub segment_confidence: NativeSttMetricSummary,
    /// Valid full-transcript provider confidence, retained separately from
    /// segment confidence. Never synthesized from segment or word values.
    pub transcript_confidence: Option<f64>,
    pub word_confidence: NativeSttWordConfidenceCoverage,
    /// Repetition is computed only after timestamps are validated and segments
    /// are mapped. `None` means the mapped candidate set was unavailable because
    /// the provider output was rejected as malformed.
    pub repetition_ratio: Option<f32>,
}

impl NativeSttQualityMetrics {
    /// At least one comparable metric covered every candidate segment.
    pub(crate) fn has_complete_comparable_evidence(&self) -> bool {
        self.avg_logprob.mean.is_some()
            || self.segment_confidence.mean.is_some()
            || self.transcript_confidence.is_some()
    }

    /// At least one complete comparable metric exists and passes the existing
    /// native quality thresholds. This is the same acceptance rule the pure C2
    /// evaluator applies; persistence reuses it instead of introducing a second
    /// calibration.
    pub(crate) fn has_complete_passing_evidence(&self) -> bool {
        self.avg_logprob
            .mean
            .is_some_and(|mean| mean >= NATIVE_STT_AVG_LOGPROB_LN_0_20_REJECT_THRESHOLD)
            || self
                .segment_confidence
                .mean
                .is_some_and(|mean| mean >= NATIVE_STT_PROVIDER_CONFIDENCE_REJECT_THRESHOLD)
            || self.transcript_confidence.is_some_and(|confidence| {
                confidence >= NATIVE_STT_PROVIDER_CONFIDENCE_REJECT_THRESHOLD
            })
    }

    /// A complete comparable metric contains a known value below its existing
    /// rejection threshold. An accepted artifact cannot hide this behind a
    /// passing alternate metric.
    pub(crate) fn has_known_low_complete_evidence(&self) -> bool {
        self.avg_logprob
            .mean
            .is_some_and(|mean| mean < NATIVE_STT_AVG_LOGPROB_LN_0_20_REJECT_THRESHOLD)
            || self
                .segment_confidence
                .mean
                .is_some_and(|mean| mean < NATIVE_STT_PROVIDER_CONFIDENCE_REJECT_THRESHOLD)
            || self.transcript_confidence.is_some_and(|confidence| {
                confidence < NATIVE_STT_PROVIDER_CONFIDENCE_REJECT_THRESHOLD
            })
    }

    /// Completely absent or partial comparable evidence: the exact C2 state
    /// that requires human review rather than accepted speech.
    pub(crate) fn has_any_comparable_evidence(&self) -> bool {
        self.avg_logprob.known_segments > 0
            || self.segment_confidence.known_segments > 0
            || self.transcript_confidence.is_some()
    }

    fn unavailable(total_segments: usize, total_words: usize) -> Self {
        Self {
            avg_logprob: NativeSttMetricSummary::from_values(&[], total_segments),
            segment_confidence: NativeSttMetricSummary::from_values(&[], total_segments),
            transcript_confidence: None,
            word_confidence: NativeSttWordConfidenceCoverage {
                known_words: 0,
                total_words,
            },
            repetition_ratio: None,
        }
    }
}

/// Pure native speech-quality result.
///
/// Rejected instances always have an empty [`Self::segments`] vector so
/// candidate/provider text cannot leak downstream. Accepted and review-required
/// instances retain mapped candidates with
/// [`DecodedSegment::mean_token_probability`] set to `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeSttQualityEvaluation {
    pub decision: NativeSttQualityDecision,
    pub reason: NativeSttQualityReason,
    /// Explicit native quality heuristic recipe identity.
    pub recipe_version: &'static str,
    /// Normalized ISO 639-1 language code when the reported language was
    /// recognized.
    pub language: Option<String>,
    /// Recognized original provider language spelling, retained only after it
    /// has been resolved through the whisper registry. Unknown raw provider
    /// language text is never copied into a rejection result.
    pub reported_language: Option<String>,
    pub metrics: NativeSttQualityMetrics,
    /// Mapped source-time candidates. Empty for rejected output.
    pub segments: Vec<DecodedSegment>,
}

impl NativeSttQualityEvaluation {
    pub fn is_accepted(&self) -> bool {
        self.decision == NativeSttQualityDecision::Accepted
    }

    pub fn is_rejected(&self) -> bool {
        self.decision == NativeSttQualityDecision::Rejected
    }

    pub fn is_review_required(&self) -> bool {
        self.decision == NativeSttQualityDecision::ReviewRequired
    }

    pub fn reason_code(&self) -> &'static str {
        self.reason.code()
    }
}

/// Repetition is safe only when measured and below the existing local
/// repetition gate. Native persistence reuses this exact threshold.
pub(crate) fn native_stt_repetition_is_safe(ratio: f32) -> bool {
    ratio.is_finite() && (0.0..REPETITION_REJECT_RATIO).contains(&ratio)
}

/// Evaluates a decoded native transcript against the exact validated audio
/// window that produced it.
///
/// Structural DTO validation is repeated here rather than trusting a production
/// decoder, because this is a public pure boundary. Provider timestamps are
/// checked before existing packed-window mapping is applied; they are never
/// clamped into range. Empty output is an explicit provider-result rejection,
/// never local silence.
pub fn evaluate_native_stt_quality(
    transcript: &NativeSttTranscript,
    audio: &NativeSttAudioWindow,
) -> NativeSttQualityEvaluation {
    let total_segments = transcript.segments.len();
    let total_words = transcript
        .segments
        .iter()
        .map(|segment| segment.words.len())
        .sum();

    if let Err(reason) = validate_structure(transcript, audio) {
        return rejected(
            reason,
            NativeSttQualityMetrics::unavailable(total_segments, total_words),
        );
    }

    let evidence = match collect_quality_evidence(transcript) {
        Ok(evidence) => evidence,
        Err(()) => {
            return rejected(
                NativeSttQualityReason::InvalidQualityEvidence,
                NativeSttQualityMetrics::unavailable(total_segments, total_words),
            );
        }
    };

    let Some(language) = normalize_language(&transcript.language) else {
        return rejected(
            NativeSttQualityReason::UnknownLanguage,
            NativeSttQualityMetrics::unavailable(total_segments, total_words),
        );
    };

    let Some(segments) = map_candidates(transcript, audio, &language.iso_code) else {
        return rejected(
            NativeSttQualityReason::InvalidTimestamp,
            NativeSttQualityMetrics::unavailable(total_segments, total_words),
        );
    };

    let repetition = repetition_ratio(&segments);
    let metrics = NativeSttQualityMetrics {
        avg_logprob: NativeSttMetricSummary::from_values(&evidence.avg_logprobs, total_segments),
        segment_confidence: NativeSttMetricSummary::from_values(
            &evidence.segment_confidences,
            total_segments,
        ),
        transcript_confidence: evidence.transcript_confidence,
        word_confidence: NativeSttWordConfidenceCoverage {
            known_words: evidence.known_word_confidences,
            total_words,
        },
        repetition_ratio: Some(repetition),
    };

    if repetition >= REPETITION_REJECT_RATIO {
        return rejected(NativeSttQualityReason::Repetition, metrics);
    }

    // A known low subset must reject even when coverage is incomplete. Do not
    // dilute a bad value behind missing values or a passing alternate metric.
    if finite_mean(&evidence.avg_logprobs)
        .is_some_and(|mean| mean < NATIVE_STT_AVG_LOGPROB_LN_0_20_REJECT_THRESHOLD)
    {
        return rejected(NativeSttQualityReason::LowAverageLogProbability, metrics);
    }
    if finite_mean(&evidence.segment_confidences)
        .is_some_and(|mean| mean < NATIVE_STT_PROVIDER_CONFIDENCE_REJECT_THRESHOLD)
        || evidence
            .transcript_confidence
            .is_some_and(|confidence| confidence < NATIVE_STT_PROVIDER_CONFIDENCE_REJECT_THRESHOLD)
    {
        return rejected(NativeSttQualityReason::LowProviderConfidence, metrics);
    }

    let has_complete_metric = metrics.avg_logprob.mean.is_some()
        || metrics.segment_confidence.mean.is_some()
        || metrics.transcript_confidence.is_some();
    let (decision, reason) = if has_complete_metric {
        (
            NativeSttQualityDecision::Accepted,
            NativeSttQualityReason::AcceptedCompleteEvidence,
        )
    } else {
        let has_any_comparable_evidence = metrics.avg_logprob.known_segments > 0
            || metrics.segment_confidence.known_segments > 0
            || metrics.transcript_confidence.is_some();
        (
            NativeSttQualityDecision::ReviewRequired,
            if has_any_comparable_evidence {
                NativeSttQualityReason::ReviewIncompleteQualityEvidence
            } else {
                NativeSttQualityReason::ReviewMissingQualityEvidence
            },
        )
    };

    NativeSttQualityEvaluation {
        decision,
        reason,
        recipe_version: NATIVE_STT_QUALITY_RECIPE_VERSION,
        language: Some(language.iso_code),
        reported_language: Some(language.reported),
        metrics,
        segments,
    }
}

fn rejected(
    reason: NativeSttQualityReason,
    metrics: NativeSttQualityMetrics,
) -> NativeSttQualityEvaluation {
    NativeSttQualityEvaluation {
        decision: NativeSttQualityDecision::Rejected,
        reason,
        recipe_version: NATIVE_STT_QUALITY_RECIPE_VERSION,
        language: None,
        reported_language: None,
        metrics,
        segments: Vec::new(),
    }
}

fn validate_structure(
    transcript: &NativeSttTranscript,
    audio: &NativeSttAudioWindow,
) -> Result<(), NativeSttQualityReason> {
    let audio_duration_ms = audio.duration_ms();
    if transcript.duration_ms == 0 || transcript.duration_ms > audio_duration_ms {
        return Err(NativeSttQualityReason::InvalidDuration);
    }
    if transcript.segments.is_empty() {
        return Err(NativeSttQualityReason::EmptyProviderOutput);
    }
    if transcript.text.trim().is_empty() {
        return Err(NativeSttQualityReason::InvalidText);
    }

    let mut previous_end_ms = 0_u32;
    for segment in &transcript.segments {
        if segment.text.trim().is_empty() {
            return Err(NativeSttQualityReason::InvalidText);
        }
        if segment.start_ms >= segment.end_ms
            || segment.end_ms > transcript.duration_ms
            || segment.end_ms > audio_duration_ms
        {
            return Err(NativeSttQualityReason::InvalidTimestamp);
        }
        if segment.start_ms < previous_end_ms {
            return Err(NativeSttQualityReason::InvalidTimestamp);
        }
        validate_word_timestamps(segment, transcript.duration_ms, audio_duration_ms)?;
        previous_end_ms = segment.end_ms;
    }
    Ok(())
}

fn validate_word_timestamps(
    segment: &NativeSttSegment,
    transcript_duration_ms: u32,
    audio_duration_ms: u32,
) -> Result<(), NativeSttQualityReason> {
    for word in &segment.words {
        match (word.start_ms, word.end_ms) {
            (None, None) => {}
            (Some(start_ms), Some(end_ms))
                if start_ms < end_ms
                    && end_ms <= transcript_duration_ms
                    && end_ms <= audio_duration_ms => {}
            _ => return Err(NativeSttQualityReason::InvalidTimestamp),
        }
    }
    Ok(())
}

struct CollectedQualityEvidence {
    avg_logprobs: Vec<f64>,
    segment_confidences: Vec<f64>,
    transcript_confidence: Option<f64>,
    known_word_confidences: usize,
}

fn collect_quality_evidence(
    transcript: &NativeSttTranscript,
) -> Result<CollectedQualityEvidence, ()> {
    let transcript_confidence = match transcript.confidence {
        Some(confidence) => Some(validate_confidence(confidence)?),
        None => None,
    };
    let mut avg_logprobs = Vec::with_capacity(transcript.segments.len());
    let mut segment_confidences = Vec::with_capacity(transcript.segments.len());
    let mut known_word_confidences = 0_usize;

    for segment in &transcript.segments {
        if let Some(confidence) = segment.confidence {
            segment_confidences.push(validate_confidence(confidence)?);
        }
        if let Some(logprob) = segment.avg_logprob {
            avg_logprobs.push(validate_logprob(logprob)?);
        }
        for word in &segment.words {
            if let Some(confidence) = word.confidence {
                validate_confidence(confidence)?;
                known_word_confidences += 1;
            }
        }
    }

    Ok(CollectedQualityEvidence {
        avg_logprobs,
        segment_confidences,
        transcript_confidence,
        known_word_confidences,
    })
}

fn validate_confidence(value: f64) -> Result<f64, ()> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(())
    }
}

fn validate_logprob(value: f64) -> Result<f64, ()> {
    if value.is_finite() && value <= 0.0 {
        Ok(value)
    } else {
        Err(())
    }
}

struct RecognizedLanguage {
    iso_code: String,
    reported: String,
}

/// Resolves a provider-reported code or full language name through the existing
/// whisper language registry.
///
/// The unknown input is never passed to `get_lang_id`, which can log raw input.
/// Resolve a provider-reported language using the same whisper registry and
/// normalization rules as native quality evaluation. Unknown input is never
/// passed to `get_lang_id`, so callers cannot make an unknown canary appear as
/// recognized language metadata. The recognized provider spelling is returned.
pub(crate) fn recognized_native_stt_reported_language(reported: &str) -> Option<String> {
    normalize_language(reported).map(|language| language.reported)
}

fn normalize_language(reported: &str) -> Option<RecognizedLanguage> {
    let reported = reported.trim();
    if reported.is_empty() {
        return None;
    }
    let normalized = reported.to_ascii_lowercase();
    for id in 0..=whisper_rs::get_lang_max_id() {
        let Some(code) = whisper_rs::get_lang_str(id) else {
            continue;
        };
        let full_name = whisper_rs::get_lang_str_full(id);
        if code == normalized || full_name == Some(normalized.as_str()) {
            return Some(RecognizedLanguage {
                iso_code: code.to_string(),
                reported: reported.to_string(),
            });
        }
    }
    None
}

fn map_candidates(
    transcript: &NativeSttTranscript,
    audio: &NativeSttAudioWindow,
    language: &str,
) -> Option<Vec<DecodedSegment>> {
    transcript
        .segments
        .iter()
        .map(|segment| {
            let (start_ms, end_ms) =
                translate_span(audio.window(), segment.start_ms, segment.end_ms)?;
            Some(DecodedSegment {
                start_ms,
                end_ms,
                language: language.to_string(),
                text: segment.text.clone(),
                // Provider confidence and avg_logprob are not arithmetic mean
                // token probability and must never be copied or exponentiated
                // into this local-only field.
                mean_token_probability: None,
            })
        })
        .collect()
}

/// Overflow-safe finite arithmetic mean.
///
/// Quality evidence is validated into one sign before this helper is called:
/// log-probabilities are nonpositive and confidence is nonnegative. Updating a
/// running mean keeps both the difference and result within the finite input
/// range. Summing terms divided by the count can still overflow at rounding
/// boundaries, hiding a known-low log metric behind a passing confidence.
fn finite_mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() || !values.iter().all(|value| value.is_finite()) {
        return None;
    }
    let mut mean = 0.0_f64;
    for (index, value) in values.iter().enumerate() {
        mean += (*value - mean) / (index + 1) as f64;
        if !mean.is_finite() {
            return None;
        }
    }
    Some(mean)
}

#[cfg(test)]
mod tests;
