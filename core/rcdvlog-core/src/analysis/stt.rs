//! Composing the transcription pipeline.
//!
//! VAD gate, window packing, decode, coverage re-split, timestamp translation
//! and the reject gate in the order the measurements require. Everything here
//! runs against the `SttEngine` trait, so the whole pipeline is testable with no
//! model, no FFmpeg and no network.

use anyhow::Result;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

use crate::analysis::STT_RECIPE_KEY;
use crate::analysis::coverage::{needs_resplit, resplit};
use crate::analysis::engine::SttEngine;
use crate::analysis::quality::{DecodedSegment, reject_reason};
use crate::analysis::store::{StoredTranscript, TranscriptStatus};
use crate::analysis::windows::{PackedWindow, SpeechRegion, pack_windows, translate_span};

/// Summary of one transcription, for progress reporting and CLI output.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscribeOutcome {
    pub asset_id: String,
    pub status: TranscriptStatus,
    pub segment_count: usize,
    pub window_count: u32,
    pub speech_seconds: f64,
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
    pcm_path: &Path,
) -> Result<StoredTranscript> {
    let regions = engine.detect_speech(pcm_path)?;
    let speech_ms: u32 = regions.iter().map(SpeechRegion::duration_ms).sum();

    if regions.is_empty() || speech_ms == 0 {
        return Ok(empty_transcript(
            asset_id,
            source_fingerprint,
            TranscriptStatus::NoSpeech,
            String::new(),
            0,
            0.0,
        ));
    }

    let windows = pack_windows(&regions);
    let mut window_count = 0_u32;
    let mut segments: Vec<DecodedSegment> = Vec::new();

    for window in &windows {
        window_count += 1;
        let decoded = engine.decode_window(pcm_path, window)?;

        if needs_resplit(window, &decoded) {
            // A whole language's speech went missing. Re-decode each speech
            // region on its own; the silence boundaries between them are the
            // likely switch points.
            for narrow in resplit(window) {
                window_count += 1;
                let narrow_decoded = engine.decode_window(pcm_path, &narrow)?;
                segments.extend(to_clip_time(&narrow, &narrow_decoded));
            }
        } else {
            segments.extend(to_clip_time(window, &decoded));
        }
    }

    segments.retain(|segment| !segment.text.trim().is_empty());
    segments.sort_by_key(|segment| (segment.start_ms, segment.end_ms));

    let speech_seconds = f64::from(speech_ms) / 1_000.0;

    if let Some(reason) = reject_reason(&segments) {
        // A rejected transcript keeps its reason and discards its content:
        // storing hallucinated text as usable would feed it straight into the
        // vision prompt downstream.
        return Ok(empty_transcript(
            asset_id,
            source_fingerprint,
            TranscriptStatus::Rejected,
            reason.as_str().to_string(),
            window_count,
            speech_seconds,
        ));
    }

    if segments.is_empty() {
        return Ok(empty_transcript(
            asset_id,
            source_fingerprint,
            TranscriptStatus::NoSpeech,
            String::new(),
            window_count,
            speech_seconds,
        ));
    }

    let primary_language = dominant_language(&segments);
    Ok(StoredTranscript {
        asset_id: asset_id.to_string(),
        recipe_key: STT_RECIPE_KEY.to_string(),
        source_fingerprint: source_fingerprint.to_string(),
        status: TranscriptStatus::Speech,
        reject_reason: String::new(),
        primary_language,
        speech_seconds,
        window_count,
        segments,
    })
}

fn empty_transcript(
    asset_id: &str,
    source_fingerprint: &str,
    status: TranscriptStatus,
    reject_reason: String,
    window_count: u32,
    speech_seconds: f64,
) -> StoredTranscript {
    StoredTranscript {
        asset_id: asset_id.to_string(),
        recipe_key: STT_RECIPE_KEY.to_string(),
        source_fingerprint: source_fingerprint.to_string(),
        status,
        reject_reason,
        primary_language: String::new(),
        speech_seconds,
        window_count,
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
        fn detect_speech(&self, _pcm_path: &Path) -> Result<Vec<SpeechRegion>> {
            Ok(self.regions.clone())
        }

        fn decode_window(
            &self,
            _pcm_path: &Path,
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
            mean_token_probability: probability,
        }
    }

    #[test]
    fn silence_is_stored_as_no_speech_and_never_reaches_the_model() {
        let engine = FakeEngine::new(Vec::new(), Vec::new());
        let stored = transcribe_pcm(&engine, "a1", "fp", Path::new("/tmp/x.wav")).unwrap();
        assert_eq!(stored.status, TranscriptStatus::NoSpeech);
        assert!(stored.segments.is_empty());
        assert_eq!(stored.window_count, 0);
        assert_eq!(
            *engine.decode_calls.borrow(),
            0,
            "model must not be invoked"
        );
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
        let stored = transcribe_pcm(&engine, "a1", "fp", Path::new("/tmp/x.wav")).unwrap();
        assert_eq!(stored.status, TranscriptStatus::Speech);
        assert_eq!(stored.segments.len(), 1);
        assert_eq!(stored.segments[0].start_ms, 5_000);
        assert_eq!(stored.segments[0].end_ms, 8_000);
        assert_eq!(stored.window_count, 1);
        assert!((stored.speech_seconds - 3.0).abs() < 1e-6);
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
        let stored = transcribe_pcm(&engine, "a1", "fp", Path::new("/tmp/x.wav")).unwrap();
        assert_eq!(stored.status, TranscriptStatus::Rejected);
        assert_eq!(stored.reject_reason, "repetition");
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
        let stored = transcribe_pcm(&engine, "a1", "fp", Path::new("/tmp/x.wav")).unwrap();
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
        let stored = transcribe_pcm(&engine, "a1", "fp", Path::new("/tmp/x.wav")).unwrap();
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
        let stored = transcribe_pcm(&engine, "a1", "fp", Path::new("/tmp/x.wav")).unwrap();
        assert_eq!(stored.segments.len(), 1);
        assert_eq!(stored.segments[0].text, "real");
    }

    #[test]
    fn a_decode_returning_nothing_is_no_speech_not_rejected() {
        let engine = FakeEngine::new(
            vec![SpeechRegion {
                start_ms: 0,
                end_ms: 4_000,
            }],
            vec![vec![]],
        );
        let stored = transcribe_pcm(&engine, "a1", "fp", Path::new("/tmp/x.wav")).unwrap();
        assert_eq!(stored.status, TranscriptStatus::NoSpeech);
        assert!(stored.reject_reason.is_empty());
    }

    #[test]
    fn the_stored_recipe_key_is_the_current_one() {
        let engine = FakeEngine::new(Vec::new(), Vec::new());
        let stored = transcribe_pcm(&engine, "a1", "fp", Path::new("/tmp/x.wav")).unwrap();
        assert_eq!(stored.recipe_key, STT_RECIPE_KEY);
        assert_eq!(stored.source_fingerprint, "fp");
    }
}
