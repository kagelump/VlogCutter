//! Composing the transcription pipeline.
//!
//! VAD gate, window packing, decode, coverage re-split, timestamp translation
//! and the reject gate in the order the measurements require. Everything here
//! runs against the `SttEngine` trait, so the whole pipeline is testable with no
//! model, no FFmpeg and no network.

use anyhow::Result;
use serde::Serialize;
use std::collections::HashMap;

use crate::analysis::STT_RECIPE_KEY;
use crate::analysis::coverage::{needs_resplit, resplit};
use crate::analysis::engine::SttEngine;
use crate::analysis::quality::{
    DecodedSegment, mean_token_probability, reject_reason, repetition_ratio,
};
use crate::analysis::store::{StoredTranscript, TranscriptOutcomeReason, TranscriptStatus};
use crate::analysis::windows::{PackedWindow, SpeechRegion, pack_windows, translate_span};

/// Structured evidence carried by a processing failure. Keeping this as a
/// typed error lets callers persist the speech duration and partial provider
/// work they observed before a decode failed, instead of collapsing the whole
/// attempt to a diagnostic string.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct TranscriptionFailure {
    pub message: String,
    pub speech_seconds: f64,
    pub window_count: u32,
    pub provider_segment_count: u32,
}

fn processing_error(
    error: anyhow::Error,
    speech_seconds: f64,
    window_count: u32,
    provider_segment_count: u32,
) -> anyhow::Error {
    anyhow::Error::new(TranscriptionFailure {
        message: format!("{error:#}"),
        speech_seconds,
        window_count,
        provider_segment_count,
    })
}

/// Recover any partial evidence attached to a processing error.
pub(crate) fn failure_evidence(error: &anyhow::Error) -> (f64, u32, u32) {
    error
        .downcast_ref::<TranscriptionFailure>()
        .map(|failure| {
            (
                failure.speech_seconds,
                failure.window_count,
                failure.provider_segment_count,
            )
        })
        .unwrap_or((0.0, 0, 0))
}

/// Summary of one transcription, for progress reporting and CLI output.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscribeOutcome {
    pub asset_id: String,
    pub status: TranscriptStatus,
    pub outcome_reason: TranscriptOutcomeReason,
    /// Exact proxy fingerprint this outcome was derived from, including failed
    /// attempts so callers can correlate recovery with the bytes they saw.
    pub source_fingerprint: String,
    pub segment_count: usize,
    pub window_count: u32,
    pub speech_seconds: f64,
    pub provider_segment_count: u32,
    pub mean_token_probability: Option<f32>,
    pub repetition_ratio: Option<f32>,
}

/// Transcribes one clip's extracted PCM.
///
/// The VAD gate comes first and is a correctness requirement, not an
/// optimization: given pure silence both local and cloud engines invent fluent
/// speech at high confidence, so a clip with no detected speech must never be
/// handed to a model at all.
pub fn transcribe_pcm(
    engine: &dyn SttEngine,
    asset_id: &str,
    source_fingerprint: &str,
    pcm: &[f32],
) -> Result<StoredTranscript> {
    let regions = engine
        .detect_speech(pcm)
        .map_err(|error| processing_error(error, 0.0, 0, 0))?;
    let speech_ms: u32 = regions.iter().map(SpeechRegion::duration_ms).sum();
    let speech_seconds = f64::from(speech_ms) / 1_000.0;

    if regions.is_empty() || speech_ms == 0 {
        return Ok(empty_transcript(
            asset_id,
            source_fingerprint,
            TranscriptStatus::NoSpeech,
            TranscriptOutcomeReason::NoDetectedSpeech,
            String::new(),
            0,
            0.0,
            0,
            None,
            None,
        ));
    }

    let windows = pack_windows(&regions);
    let mut window_count = 0_u32;
    let mut provider_segment_count = 0_u32;
    let mut segments: Vec<DecodedSegment> = Vec::new();

    for window in &windows {
        window_count += 1;
        let decoded = engine.decode_window(pcm, window).map_err(|error| {
            processing_error(error, speech_seconds, window_count, provider_segment_count)
        })?;
        provider_segment_count += decoded.len() as u32;

        if needs_resplit(window, &decoded) {
            // A whole language's speech went missing. Re-decode each speech
            // region on its own; the silence boundaries between them are the
            // likely switch points.
            for narrow in resplit(window) {
                window_count += 1;
                let narrow_decoded = engine.decode_window(pcm, &narrow).map_err(|error| {
                    processing_error(error, speech_seconds, window_count, provider_segment_count)
                })?;
                provider_segment_count += narrow_decoded.len() as u32;
                segments.extend(to_clip_time(&narrow, &narrow_decoded));
            }
        } else {
            segments.extend(to_clip_time(window, &decoded));
        }
    }

    segments.retain(|segment| !segment.text.trim().is_empty());
    segments.sort_by_key(|segment| (segment.start_ms, segment.end_ms));

    let measured_mean_probability = mean_token_probability(&segments);
    let measured_repetition = (!segments.is_empty()).then(|| repetition_ratio(&segments));

    if let Some(reason) = reject_reason(&segments) {
        // A rejected transcript keeps its reason and discards its content:
        // storing hallucinated text as usable would feed it straight into the
        // vision prompt downstream.
        return Ok(empty_transcript(
            asset_id,
            source_fingerprint,
            TranscriptStatus::Rejected,
            TranscriptOutcomeReason::QualityRejected,
            reason.as_str().to_string(),
            window_count,
            speech_seconds,
            provider_segment_count,
            measured_mean_probability,
            measured_repetition,
        ));
    }

    if segments.is_empty() {
        // Speech was detected and the provider was invoked, but it returned no
        // accepted text. This is not literal silence and must not be stored as
        // `no_speech`; recovery with a different provider/settings may help.
        return Ok(empty_transcript(
            asset_id,
            source_fingerprint,
            TranscriptStatus::NoAcceptedSpeech,
            TranscriptOutcomeReason::EmptyProviderOutput,
            String::new(),
            window_count,
            speech_seconds,
            provider_segment_count,
            measured_mean_probability,
            measured_repetition,
        ));
    }

    let primary_language = dominant_language(&segments);
    Ok(StoredTranscript {
        asset_id: asset_id.to_string(),
        recipe_key: STT_RECIPE_KEY.to_string(),
        source_fingerprint: source_fingerprint.to_string(),
        status: TranscriptStatus::Speech,
        outcome_reason: TranscriptOutcomeReason::Accepted,
        reject_reason: String::new(),
        primary_language,
        speech_seconds,
        window_count,
        provider_segment_count,
        mean_token_probability: measured_mean_probability,
        repetition_ratio: measured_repetition,
        segments,
    })
}

/// A successful terminal outcome for a valid video whose proxy has no audio
/// stream. No VAD or provider work is implied or attempted.
pub(crate) fn no_audio_transcript(asset_id: &str, source_fingerprint: &str) -> StoredTranscript {
    empty_transcript(
        asset_id,
        source_fingerprint,
        TranscriptStatus::NoAudio,
        TranscriptOutcomeReason::NoAudioStream,
        String::new(),
        0,
        0.0,
        0,
        None,
        None,
    )
}

/// A first-class processing failure. The source fingerprint is retained so the
/// failed attempt remains correlated with the exact proxy; `is_current` still
/// treats it as stale and retryable.
pub(crate) fn failed_transcript(
    asset_id: &str,
    source_fingerprint: &str,
    message: String,
) -> StoredTranscript {
    failed_transcript_with_evidence(asset_id, source_fingerprint, message, 0.0, 0, 0)
}

pub(crate) fn failed_transcript_with_evidence(
    asset_id: &str,
    source_fingerprint: &str,
    message: String,
    speech_seconds: f64,
    window_count: u32,
    provider_segment_count: u32,
) -> StoredTranscript {
    empty_transcript(
        asset_id,
        source_fingerprint,
        TranscriptStatus::Failed,
        TranscriptOutcomeReason::ProcessingFailed,
        message,
        window_count,
        speech_seconds,
        provider_segment_count,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn empty_transcript(
    asset_id: &str,
    source_fingerprint: &str,
    status: TranscriptStatus,
    outcome_reason: TranscriptOutcomeReason,
    reject_reason: String,
    window_count: u32,
    speech_seconds: f64,
    provider_segment_count: u32,
    mean_token_probability: Option<f32>,
    repetition_ratio: Option<f32>,
) -> StoredTranscript {
    StoredTranscript {
        asset_id: asset_id.to_string(),
        recipe_key: STT_RECIPE_KEY.to_string(),
        source_fingerprint: source_fingerprint.to_string(),
        status,
        outcome_reason,
        reject_reason,
        primary_language: String::new(),
        speech_seconds,
        window_count,
        provider_segment_count,
        mean_token_probability,
        repetition_ratio,
        segments: Vec::new(),
    }
}

/// Rewrites window-relative timestamps into clip time.
fn to_clip_time(window: &PackedWindow, decoded: &[DecodedSegment]) -> Vec<DecodedSegment> {
    decoded
        .iter()
        .filter_map(|segment| {
            let (start_ms, end_ms) = translate_span(window, segment.start_ms, segment.end_ms)?;
            Some(DecodedSegment {
                start_ms,
                end_ms,
                language: segment.language.clone(),
                text: segment.text.clone(),
                mean_token_probability: segment.mean_token_probability,
            })
        })
        .collect()
}

/// The language covering the most transcribed time, used as the clip's summary
/// language. Per-segment languages are retained; this is only a headline.
fn dominant_language(segments: &[DecodedSegment]) -> String {
    let mut totals: HashMap<&str, u32> = HashMap::new();
    for segment in segments {
        *totals.entry(segment.language.as_str()).or_default() +=
            segment.end_ms.saturating_sub(segment.start_ms);
    }
    totals
        .into_iter()
        .max_by_key(|(language, total)| (*total, *language))
        .map(|(language, _)| language.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Returns scripted results so every gate can be exercised without whisper.
    struct FakeEngine {
        regions: Vec<SpeechRegion>,
        decodes: RefCell<Vec<Vec<DecodedSegment>>>,
        decode_calls: RefCell<usize>,
    }

    impl FakeEngine {
        fn new(regions: Vec<SpeechRegion>, decodes: Vec<Vec<DecodedSegment>>) -> Self {
            Self {
                regions,
                decodes: RefCell::new(decodes),
                decode_calls: RefCell::new(0),
            }
        }
    }

    impl SttEngine for FakeEngine {
        fn detect_speech(&self, _pcm: &[f32]) -> Result<Vec<SpeechRegion>> {
            Ok(self.regions.clone())
        }

        fn decode_window(
            &self,
            _pcm: &[f32],
            _window: &PackedWindow,
        ) -> Result<Vec<DecodedSegment>> {
            *self.decode_calls.borrow_mut() += 1;
            let mut decodes = self.decodes.borrow_mut();
            if decodes.is_empty() {
                return Ok(Vec::new());
            }
            Ok(decodes.remove(0))
        }
    }

    fn segment(start_ms: u32, end_ms: u32, text: &str, probability: f32) -> DecodedSegment {
        DecodedSegment {
            start_ms,
            end_ms,
            language: "ja".to_string(),
            text: text.to_string(),
            mean_token_probability: Some(probability),
        }
    }

    #[test]
    fn silence_is_stored_as_no_speech_and_never_reaches_the_model() {
        let engine = FakeEngine::new(Vec::new(), Vec::new());
        let stored = transcribe_pcm(&engine, "a1", "fp", &[]).unwrap();
        assert_eq!(stored.status, TranscriptStatus::NoSpeech);
        assert_eq!(
            stored.outcome_reason,
            TranscriptOutcomeReason::NoDetectedSpeech
        );
        assert!(stored.segments.is_empty());
        assert_eq!(stored.window_count, 0);
        assert_eq!(
            *engine.decode_calls.borrow(),
            0,
            "model must not be invoked"
        );
    }

    #[test]
    fn a_decode_failure_retains_detected_duration_and_partial_provider_evidence() {
        struct FailsOnDecode;
        impl SttEngine for FailsOnDecode {
            fn detect_speech(&self, _pcm: &[f32]) -> Result<Vec<SpeechRegion>> {
                Ok(vec![SpeechRegion {
                    start_ms: 0,
                    end_ms: 4_000,
                }])
            }

            fn decode_window(
                &self,
                _pcm: &[f32],
                _window: &PackedWindow,
            ) -> Result<Vec<DecodedSegment>> {
                anyhow::bail!("synthetic provider failure")
            }
        }

        let error = transcribe_pcm(&FailsOnDecode, "a1", "fp", &[]).unwrap_err();
        let (speech_seconds, window_count, provider_segment_count) = failure_evidence(&error);
        assert!((speech_seconds - 4.0).abs() < 1e-6);
        assert_eq!(window_count, 1);
        assert_eq!(provider_segment_count, 0);
    }

    #[test]
    fn a_healthy_decode_is_stored_with_clip_time_segments() {
        // 3s of speech starting at 5s. Window time 0 is clip time 5000.
        let engine = FakeEngine::new(
            vec![SpeechRegion {
                start_ms: 5_000,
                end_ms: 8_000,
            }],
            vec![vec![segment(0, 3_000, "次は新大阪です", 0.73)]],
        );
        let stored = transcribe_pcm(&engine, "a1", "fp", &[]).unwrap();
        assert_eq!(stored.status, TranscriptStatus::Speech);
        assert_eq!(stored.segments.len(), 1);
        assert_eq!(stored.segments[0].start_ms, 5_000);
        assert_eq!(stored.segments[0].end_ms, 8_000);
        assert_eq!(stored.window_count, 1);
        assert!((stored.speech_seconds - 3.0).abs() < 1e-6);
        assert_eq!(stored.provider_segment_count, 1);
        assert!(stored.mean_token_probability.is_some());
        assert_eq!(stored.repetition_ratio, Some(0.0));
        assert_eq!(stored.primary_language, "ja");
    }

    #[test]
    fn a_repeated_hallucination_is_rejected_with_a_reason() {
        let repeats: Vec<DecodedSegment> = (0..6)
            .map(|index| segment(index * 500, index * 500 + 400, "ご飯を食べてみます。", 0.7))
            .collect();
        let engine = FakeEngine::new(
            vec![SpeechRegion {
                start_ms: 0,
                end_ms: 4_000,
            }],
            vec![repeats],
        );
        let stored = transcribe_pcm(&engine, "a1", "fp", &[]).unwrap();
        assert_eq!(stored.status, TranscriptStatus::Rejected);
        assert_eq!(
            stored.outcome_reason,
            TranscriptOutcomeReason::QualityRejected
        );
        assert_eq!(stored.reject_reason, "repetition");
        assert!(stored.provider_segment_count > 0);
        assert!(stored.repetition_ratio.is_some_and(|ratio| ratio >= 0.3));
        assert!(
            stored.segments.is_empty(),
            "rejected content must not be stored as usable"
        );
    }

    #[test]
    fn a_dropped_language_half_triggers_one_resplit_decode_per_region() {
        // Two 6s regions packed into one window; the first decode returns only
        // the Japanese half, so the window is re-split and re-decoded.
        let engine = FakeEngine::new(
            vec![
                SpeechRegion {
                    start_ms: 0,
                    end_ms: 6_000,
                },
                SpeechRegion {
                    start_ms: 10_000,
                    end_ms: 16_000,
                },
            ],
            vec![
                vec![segment(0, 6_000, "日本語です", 0.7)],
                vec![segment(0, 6_000, "日本語です", 0.7)],
                vec![segment(0, 6_000, "this is english", 0.7)],
            ],
        );
        let stored = transcribe_pcm(&engine, "a1", "fp", &[]).unwrap();
        assert_eq!(*engine.decode_calls.borrow(), 3, "1 packed + 2 re-split");
        assert_eq!(stored.segments.len(), 2);
        assert_eq!(stored.segments[0].start_ms, 0);
        // The English half is recovered and lands at its true clip time.
        assert_eq!(stored.segments[1].start_ms, 10_000);
        assert_eq!(stored.segments[1].text, "this is english");
    }

    #[test]
    fn segments_are_stored_in_clip_time_order() {
        let engine = FakeEngine::new(
            vec![
                SpeechRegion {
                    start_ms: 0,
                    end_ms: 2_000,
                },
                SpeechRegion {
                    start_ms: 20_000,
                    end_ms: 22_000,
                },
            ],
            vec![vec![
                segment(2_000, 4_000, "second", 0.7),
                segment(0, 2_000, "first", 0.7),
            ]],
        );
        let stored = transcribe_pcm(&engine, "a1", "fp", &[]).unwrap();
        assert_eq!(stored.segments[0].text, "first");
        assert_eq!(stored.segments[1].text, "second");
    }

    #[test]
    fn empty_text_segments_are_discarded() {
        let engine = FakeEngine::new(
            vec![SpeechRegion {
                start_ms: 0,
                end_ms: 4_000,
            }],
            vec![vec![
                segment(0, 2_000, "  ", 0.7),
                segment(2_000, 4_000, "real", 0.7),
            ]],
        );
        let stored = transcribe_pcm(&engine, "a1", "fp", &[]).unwrap();
        assert_eq!(stored.segments.len(), 1);
        assert_eq!(stored.segments[0].text, "real");
    }

    #[test]
    fn a_decode_returning_nothing_after_detected_speech_is_not_no_speech() {
        let engine = FakeEngine::new(
            vec![SpeechRegion {
                start_ms: 0,
                end_ms: 4_000,
            }],
            vec![vec![]],
        );
        let stored = transcribe_pcm(&engine, "a1", "fp", &[]).unwrap();
        assert_eq!(stored.status, TranscriptStatus::NoAcceptedSpeech);
        assert_eq!(
            stored.outcome_reason,
            TranscriptOutcomeReason::EmptyProviderOutput
        );
        assert!(stored.reject_reason.is_empty());
        assert_eq!(stored.window_count, 1);
        assert!((stored.speech_seconds - 4.0).abs() < 1e-6);
        assert_eq!(stored.provider_segment_count, 0);
    }

    #[test]
    fn whitespace_only_provider_output_keeps_provider_evidence_as_no_accepted_speech() {
        let engine = FakeEngine::new(
            vec![SpeechRegion {
                start_ms: 0,
                end_ms: 4_000,
            }],
            vec![vec![segment(0, 2_000, "  ", 0.7)]],
        );
        let stored = transcribe_pcm(&engine, "a1", "fp", &[]).unwrap();
        assert_eq!(stored.status, TranscriptStatus::NoAcceptedSpeech);
        assert_eq!(
            stored.outcome_reason,
            TranscriptOutcomeReason::EmptyProviderOutput
        );
        assert_eq!(stored.provider_segment_count, 1);
        assert!((stored.speech_seconds - 4.0).abs() < 1e-6);
    }

    #[test]
    fn a_low_confidence_response_preserves_quality_evidence() {
        let engine = FakeEngine::new(
            vec![SpeechRegion {
                start_ms: 0,
                end_ms: 4_000,
            }],
            vec![vec![segment(0, 4_000, "uncertain words", 0.1)]],
        );
        let stored = transcribe_pcm(&engine, "a1", "fp", &[]).unwrap();
        assert_eq!(stored.status, TranscriptStatus::Rejected);
        assert_eq!(
            stored.outcome_reason,
            TranscriptOutcomeReason::QualityRejected
        );
        assert_eq!(stored.reject_reason, "low_token_probability");
        assert_eq!(stored.provider_segment_count, 1);
        assert!(
            stored
                .mean_token_probability
                .is_some_and(|value| value < 0.2)
        );
    }

    #[test]
    fn unknown_probability_evidence_is_rejected_without_storing_hallucinated_text() {
        let engine = FakeEngine::new(
            vec![SpeechRegion {
                start_ms: 0,
                end_ms: 4_000,
            }],
            vec![vec![DecodedSegment {
                start_ms: 0,
                end_ms: 4_000,
                language: "ja".to_string(),
                text: "unmeasured words".to_string(),
                mean_token_probability: None,
            }]],
        );
        let stored = transcribe_pcm(&engine, "a1", "fp", &[]).unwrap();
        assert_eq!(stored.status, TranscriptStatus::Rejected);
        assert_eq!(
            stored.outcome_reason,
            TranscriptOutcomeReason::QualityRejected
        );
        assert_eq!(stored.reject_reason, "missing_probability_evidence");
        assert_eq!(stored.window_count, 1);
        assert_eq!(stored.provider_segment_count, 1);
        assert!((stored.speech_seconds - 4.0).abs() < 1e-6);
        assert_eq!(stored.mean_token_probability, None);
        assert!(stored.segments.is_empty());
    }

    #[test]
    fn the_stored_recipe_key_is_the_current_one() {
        let engine = FakeEngine::new(Vec::new(), Vec::new());
        let stored = transcribe_pcm(&engine, "a1", "fp", &[]).unwrap();
        assert_eq!(stored.recipe_key, STT_RECIPE_KEY);
        assert_eq!(stored.source_fingerprint, "fp");
    }
}
