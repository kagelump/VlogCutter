//! whisper.cpp behind the `SttEngine` seam.
//!
//! This is the only file that names `whisper-rs` types. CPU-only by
//! construction: the `metal` feature compiles a reference to
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
use crate::analysis::windows::{PackedRegion, PackedWindow, SpeechRegion};

const SAMPLE_RATE: u32 = 16_000;
const SAMPLES_PER_MS: usize = (SAMPLE_RATE / 1_000) as usize;
const DECODE_THREADS: i32 = 4;

/// whisper reports timestamps in centiseconds.
const CENTISECONDS_TO_MS: i64 = 10;

/// Converts 16-bit PCM to the normalized floats whisper expects.
fn i16_to_f32(samples: &[i16]) -> Vec<f32> {
    samples
        .iter()
        .map(|sample| f32::from(*sample) / f32::from(i16::MAX))
        .collect()
}

/// Builds one window's audio by concatenating its speech regions.
///
/// Regions past the end of the decoded audio are clamped rather than panicking:
/// VAD region ends and the actual sample count can disagree by a frame.
fn concatenate_regions(pcm: &[f32], window: &PackedWindow) -> Vec<f32> {
    let mut packed = Vec::new();
    for PackedRegion {
        source_start_ms,
        duration_ms,
        ..
    } in &window.regions
    {
        let start = (*source_start_ms as usize) * SAMPLES_PER_MS;
        if start >= pcm.len() {
            continue;
        }
        let end = (start + (*duration_ms as usize) * SAMPLES_PER_MS).min(pcm.len());
        packed.extend_from_slice(&pcm[start..end]);
    }
    packed
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
}

impl WhisperEngine {
    pub fn new(whisper_model: &Path, vad_model: &Path) -> Result<Self> {
        let context = whisper_rs::WhisperContext::new_with_params(
            whisper_model,
            whisper_rs::WhisperContextParameters::default(),
        )
        .context("load whisper model")?;
        Ok(Self {
            context,
            vad_model_path: vad_model.to_path_buf(),
        })
    }
}

impl SttEngine for WhisperEngine {
    fn detect_speech(&self, pcm: &[f32]) -> Result<Vec<SpeechRegion>> {
        let mut vad = whisper_rs::WhisperVadContext::new(
            &self.vad_model_path.to_string_lossy(),
            whisper_rs::WhisperVadContextParams::default(),
        )
        .context("load VAD model")?;
        let segments = vad
            .segments_from_samples(whisper_rs::WhisperVadParams::default(), pcm)
            .context("run VAD")?;
        Ok(segments
            .map(|segment| SpeechRegion {
                start_ms: (segment.start * CENTISECONDS_TO_MS as f32).max(0.0) as u32,
                end_ms: (segment.end * CENTISECONDS_TO_MS as f32).max(0.0) as u32,
            })
            .filter(|region| region.end_ms > region.start_ms)
            .collect())
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
        params.set_n_threads(DECODE_THREADS);
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
            let mean_token_probability = if counted > 0 {
                probability_total / counted as f32
            } else {
                0.0
            };

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
    fn regions_are_concatenated_in_window_order_at_16khz() {
        // 16 kHz mono: 1 ms is 16 samples.
        let pcm: Vec<f32> = (0..16_000).map(|index| index as f32).collect();
        let window = PackedWindow {
            regions: vec![
                PackedRegion {
                    window_offset_ms: 0,
                    source_start_ms: 100,
                    duration_ms: 10,
                },
                PackedRegion {
                    window_offset_ms: 10,
                    source_start_ms: 500,
                    duration_ms: 10,
                },
            ],
        };
        let packed = concatenate_regions(&pcm, &window);
        assert_eq!(packed.len(), 320);
        assert_eq!(packed[0], 1_600.0);
        assert_eq!(packed[160], 8_000.0);
    }

    #[test]
    fn a_region_past_the_end_of_the_audio_is_clamped_rather_than_panicking() {
        let pcm: Vec<f32> = vec![0.0; 160];
        let window = PackedWindow {
            regions: vec![PackedRegion {
                window_offset_ms: 0,
                source_start_ms: 5,
                duration_ms: 100,
            }],
        };
        let packed = concatenate_regions(&pcm, &window);
        assert_eq!(packed.len(), 80);
    }

    #[test]
    fn a_region_starting_past_the_audio_contributes_nothing() {
        let pcm: Vec<f32> = vec![0.0; 160];
        let window = PackedWindow {
            regions: vec![PackedRegion {
                window_offset_ms: 0,
                source_start_ms: 5_000,
                duration_ms: 100,
            }],
        };
        assert!(concatenate_regions(&pcm, &window).is_empty());
    }

    #[test]
    fn an_empty_window_concatenates_to_nothing() {
        let pcm: Vec<f32> = vec![0.0; 160];
        assert!(concatenate_regions(&pcm, &PackedWindow::default()).is_empty());
    }

    #[test]
    fn wav_samples_convert_to_normalized_f32() {
        let samples: Vec<i16> = vec![0, i16::MAX, i16::MIN];
        let converted = i16_to_f32(&samples);
        assert!((converted[0] - 0.0).abs() < 1e-6);
        assert!((converted[1] - 1.0).abs() < 1e-4);
        assert!((converted[2] + 1.0).abs() < 1e-4);
    }
}
