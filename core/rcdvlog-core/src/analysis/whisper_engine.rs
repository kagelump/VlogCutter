//! whisper.cpp behind the `SttEngine` seam.
//!
//! This file names whisper-rs ASR types; standalone Silero VAD types live in
//! `silero_vad`. CPU-only by construction: the `metal` feature compiles a reference to
//! `MTLResidencySetDescriptor`, which is macOS 15+, against a deployment target
//! that is lower, producing a binary that will not launch. The crate is
//! declared with `default-features = false` to keep it that way.
//!
//! Language detection uses whisper's internal auto-detect rather than a separate
//! `lang_detect()` pass. Measured head to head on 33 chunks, the separate pass
//! cost 34% more time and 20% more peak memory for identical language decisions
//! on every chunk.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::analysis::engine::SttEngine;
use crate::analysis::quality::DecodedSegment;
use crate::analysis::silero_vad::detect_silero_speech;
use crate::analysis::speech_audio::concatenate_regions;
use crate::analysis::windows::{PackedWindow, SpeechRegion};

const DECODE_THREADS: i32 = 4;

/// whisper ASR segment timestamps are reported in centiseconds.
const CENTISECONDS_TO_MS: i64 = 10;

/// Converts 16-bit PCM to the normalized floats whisper expects.
fn i16_to_f32(samples: &[i16]) -> Vec<f32> {
    samples
        .iter()
        .map(|sample| f32::from(*sample) / f32::from(i16::MAX))
        .collect()
}

/// Reads a 16 kHz mono WAV into whisper's normalized float samples.
///
/// Called once per clip by the pipeline, never per window.
pub fn read_pcm(path: &Path) -> Result<Vec<f32>> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("open {}", path.display()))?;
    let samples = reader
        .samples::<i16>()
        .collect::<std::result::Result<Vec<i16>, _>>()
        .context("read PCM samples")?;
    Ok(i16_to_f32(&samples))
}

pub struct WhisperEngine {
    context: whisper_rs::WhisperContext,
    vad_model_path: PathBuf,
    vad_threads: i32,
    decode_threads: i32,
}

impl WhisperEngine {
    /// Default engine. Thread counts remain explicit calibration candidates;
    /// this constructor does not claim they are universally optimal.
    pub fn new(whisper_model: &Path, vad_model: &Path) -> Result<Self> {
        Self::with_threads(
            whisper_model,
            vad_model,
            DECODE_THREADS as usize,
            DECODE_THREADS as usize,
        )
    }

    /// Open the same models with explicit, bounded VAD and decode thread
    /// settings. Callers must validate the range and keep the settings in their
    /// calibration/history identity.
    pub fn with_threads(
        whisper_model: &Path,
        vad_model: &Path,
        vad_threads: usize,
        decode_threads: usize,
    ) -> Result<Self> {
        let vad_threads = i32::try_from(vad_threads)
            .ok()
            .filter(|threads| (1..=64).contains(threads))
            .ok_or_else(|| anyhow::anyhow!("vadThreads must be 1..=64"))?;
        let decode_threads = i32::try_from(decode_threads)
            .ok()
            .filter(|threads| (1..=64).contains(threads))
            .ok_or_else(|| anyhow::anyhow!("decodeThreads must be 1..=64"))?;
        let context = whisper_rs::WhisperContext::new_with_params(
            whisper_model,
            whisper_rs::WhisperContextParameters::default(),
        )
        .context("load whisper model")?;
        Ok(Self {
            context,
            vad_model_path: vad_model.to_path_buf(),
            vad_threads,
            decode_threads,
        })
    }
}

impl SttEngine for WhisperEngine {
    fn detect_speech(&self, pcm: &[f32]) -> Result<Vec<SpeechRegion>> {
        detect_silero_speech(&self.vad_model_path, self.vad_threads as usize, pcm)
    }

    fn decode_window(&self, pcm: &[f32], window: &PackedWindow) -> Result<Vec<DecodedSegment>> {
        let packed = concatenate_regions(pcm, window);
        if packed.is_empty() {
            return Ok(Vec::new());
        }

        let mut state = self
            .context
            .create_state()
            .context("create whisper state")?;
        let mut params =
            whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.decode_threads);
        // `None` selects whisper's internal auto-detect. A separate LID pass
        // costs 34% more for identical decisions.
        params.set_language(None);
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state.full(params, &packed).context("whisper decode")?;

        let language = whisper_rs::get_lang_str(state.full_lang_id_from_state())
            .unwrap_or_default()
            .to_string();

        let count = state.full_n_segments();
        let mut segments = Vec::with_capacity(count.max(0) as usize);
        for index in 0..count {
            let Some(segment) = state.get_segment(index) else {
                continue;
            };
            let text = segment
                .to_str_lossy()
                .map(|text| text.trim().to_string())
                .unwrap_or_default();
            if text.is_empty() {
                continue;
            }

            let token_count = segment.n_tokens();
            let mut probability_total = 0.0_f32;
            let mut counted = 0_u32;
            for token_index in 0..token_count {
                if let Some(token) = segment.get_token(token_index) {
                    probability_total += token.token_probability();
                    counted += 1;
                }
            }
            let mean_token_probability = (counted > 0).then(|| probability_total / counted as f32);

            segments.push(DecodedSegment {
                start_ms: (segment.start_timestamp() * CENTISECONDS_TO_MS).max(0) as u32,
                end_ms: (segment.end_timestamp() * CENTISECONDS_TO_MS).max(0) as u32,
                language: language.clone(),
                text,
                mean_token_probability,
            });
        }
        Ok(segments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_samples_convert_to_normalized_f32() {
        let samples: Vec<i16> = vec![0, i16::MAX, i16::MIN];
        let converted = i16_to_f32(&samples);
        assert!((converted[0] - 0.0).abs() < 1e-6);
        assert!((converted[1] - 1.0).abs() < 1e-4);
        assert!((converted[2] + 1.0).abs() < 1e-4);
    }
}
