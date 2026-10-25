//! Standalone Silero speech detection through whisper-rs.
//!
//! The local ASR engine and the future native cloud transcription path need the
//! same local VAD. Silero is loaded here without constructing a whisper context,
//! so a cloud backend can gate speech without loading the 574 MB ASR model.
//! No model download, network access or automatic provisioning happens here.

use anyhow::{Context, Result, anyhow};
use std::path::Path;

use crate::analysis::windows::SpeechRegion;

const CENTISECONDS_TO_MS: f32 = 10.0;
const MAX_VAD_THREADS: usize = 64;

/// Validates the explicit Silero thread bound shared with the local engine.
pub fn validate_vad_threads(threads: usize) -> Result<i32> {
    i32::try_from(threads)
        .ok()
        .filter(|threads| (1..=MAX_VAD_THREADS as i32).contains(threads))
        .ok_or_else(|| anyhow!("vadThreads must be 1..=64"))
}

/// Converts whisper's centisecond VAD spans into bounded, ordered milliseconds.
///
/// The actual model-produced `WhisperVadSegment` values are passed through this
/// helper by the detector. Keeping the conversion separate makes the
/// non-finite/filtering behavior testable without a model download.
fn regions_from_centiseconds<I>(segments: I) -> Vec<SpeechRegion>
where
    I: IntoIterator<Item = (f32, f32)>,
{
    segments
        .into_iter()
        .filter_map(|(start, end)| {
            if !start.is_finite() || !end.is_finite() {
                return None;
            }
            let start_ms = (start * CENTISECONDS_TO_MS).max(0.0) as u32;
            let end_ms = (end * CENTISECONDS_TO_MS).max(0.0) as u32;
            (end_ms > start_ms).then_some(SpeechRegion { start_ms, end_ms })
        })
        .collect()
}

/// A loaded standalone Silero VAD context.
///
/// `detect_speech` takes `&mut self` because whisper-rs's VAD context is
/// stateful. Callers may keep one instance for a batch or load one per clip,
/// matching the local engine's existing resource behavior.
pub struct SileroVad {
    context: whisper_rs::WhisperVadContext,
}

/// Narrow VAD-only seam. Native speech preparation can inject a fake detector
/// in tests without constructing an ASR engine or a second generic pipeline.
pub(crate) trait SpeechDetector {
    fn detect_speech(&mut self, pcm: &[f32]) -> Result<Vec<SpeechRegion>>;
}

impl SpeechDetector for SileroVad {
    fn detect_speech(&mut self, pcm: &[f32]) -> Result<Vec<SpeechRegion>> {
        SileroVad::detect_speech(self, pcm)
    }
}

impl SileroVad {
    /// Loads only the Silero VAD model. The ASR model is never touched.
    pub fn load(model_path: &Path, threads: usize) -> Result<Self> {
        let threads = validate_vad_threads(threads)?;
        let mut params = whisper_rs::WhisperVadContextParams::default();
        params.set_n_threads(threads);
        let context = whisper_rs::WhisperVadContext::new(&model_path.to_string_lossy(), params)
            .context("load VAD model")?;
        Ok(Self { context })
    }

    /// Detects speech spans in clip time. An empty result is a successful
    /// silence outcome, not a provider request.
    pub fn detect_speech(&mut self, pcm: &[f32]) -> Result<Vec<SpeechRegion>> {
        let segments = self
            .context
            .segments_from_samples(whisper_rs::WhisperVadParams::default(), pcm)
            .context("run VAD")?;
        Ok(regions_from_centiseconds(
            segments.map(|segment| (segment.start, segment.end)),
        ))
    }
}

/// Loads and runs the standalone Silero VAD for one caller, preserving the
/// local engine's existing model-load and thread semantics.
pub fn detect_silero_speech(
    model_path: &Path,
    threads: usize,
    pcm: &[f32],
) -> Result<Vec<SpeechRegion>> {
    SileroVad::load(model_path, threads)?.detect_speech(pcm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vad_thread_bounds_are_explicit_and_match_the_local_engine() {
        assert_eq!(validate_vad_threads(1).unwrap(), 1);
        assert_eq!(validate_vad_threads(64).unwrap(), 64);
        assert!(validate_vad_threads(0).is_err());
        assert!(validate_vad_threads(65).is_err());
    }

    #[test]
    fn centisecond_regions_are_converted_and_invalid_spans_are_dropped() {
        let regions = regions_from_centiseconds([
            (0.0, 125.0),
            (125.0, 125.0),
            (f32::NAN, 150.0),
            (200.0, f32::INFINITY),
            (-10.0, 20.0),
        ]);
        assert_eq!(
            regions,
            vec![
                SpeechRegion {
                    start_ms: 0,
                    end_ms: 1_250
                },
                SpeechRegion {
                    start_ms: 0,
                    end_ms: 200
                }
            ]
        );
    }

    #[test]
    fn missing_vad_model_fails_without_downloading_anything() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing-vad.bin");
        let Err(error) = SileroVad::load(&missing, 1) else {
            panic!("missing VAD model unexpectedly loaded");
        };
        let message = format!("{error:#}");
        assert!(message.contains("load VAD model"), "{message}");
        assert!(!message.contains("http"), "{message}");
    }
}
