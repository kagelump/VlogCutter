//! Shared, local-only speech-audio framing helpers.
//!
//! The local whisper path and the native cloud-transcription leaf must feed
//! exactly the same bytes: the speech regions selected by the local VAD,
//! concatenated in window order, at 16 kHz mono signed 16-bit PCM. Keeping the
//! packing and WAV encoding here avoids a second "close enough" implementation
//! in the cloud transport.

use anyhow::{Context, Result, ensure};
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use std::io::Cursor;

use crate::analysis::windows::{PackedWindow, WINDOW_MS};

/// The only sample rate the local engine and native WAV wire contract accept.
pub const SPEECH_SAMPLE_RATE_HZ: u32 = 16_000;
/// Mono signed 16-bit PCM.
pub const SPEECH_CHANNELS: u16 = 1;
pub const SPEECH_BITS_PER_SAMPLE: u16 = 16;
pub const PCM_SAMPLES_PER_MS: usize = (SPEECH_SAMPLE_RATE_HZ / 1_000) as usize;
pub const MAX_SPEECH_WINDOW_SAMPLES: usize = WINDOW_MS as usize * PCM_SAMPLES_PER_MS;
/// A canonical 44-byte PCM WAV header plus the largest permitted speech window.
pub const MAX_SPEECH_WAV_BYTES: usize = 44 + MAX_SPEECH_WINDOW_SAMPLES * 2;

/// Concatenates a packed window's speech regions into the local engine's input.
///
/// This is intentionally lenient about a region past the decoded audio: VAD
/// region ends and the actual sample count can disagree by a frame, and the
/// local decode path has always clamped rather than failing the whole clip.
/// Cloud request construction uses [`concatenate_regions_bounded`], which adds
/// strict range and length checks before bytes leave the machine.
pub fn concatenate_regions(pcm: &[f32], window: &PackedWindow) -> Vec<f32> {
    let mut packed = Vec::new();
    for region in &window.regions {
        let start = (region.source_start_ms as usize) * PCM_SAMPLES_PER_MS;
        if start >= pcm.len() {
            continue;
        }
        let end = (start + (region.duration_ms as usize) * PCM_SAMPLES_PER_MS).min(pcm.len());
        packed.extend_from_slice(&pcm[start..end]);
    }
    packed
}

/// Strictly validates a packed speech window and concatenates its samples.
///
/// Cloud requests must describe genuine, bounded audio. Unlike the local
/// lenient clamp, this rejects a truncated region, inconsistent window offsets,
/// arithmetic overflow, an empty window, more than thirty seconds of audio, or
/// source regions that overlap or run backwards in clip time.
pub fn concatenate_regions_bounded(pcm: &[f32], window: &PackedWindow) -> Result<Vec<f32>> {
    ensure!(!window.regions.is_empty(), "speech window has no regions");

    let mut packed = Vec::new();
    let mut expected_offset_ms = 0_u32;
    let mut total_ms = 0_u32;
    let mut previous_source_end_ms = None;

    for region in &window.regions {
        ensure!(region.duration_ms > 0, "speech region has no duration");
        ensure!(
            region.window_offset_ms == expected_offset_ms,
            "speech window offsets are not contiguous"
        );
        let source_end_ms = region
            .source_start_ms
            .checked_add(region.duration_ms)
            .ok_or_else(|| anyhow::anyhow!("speech region source range overflowed"))?;
        if let Some(previous_end_ms) = previous_source_end_ms {
            ensure!(
                region.source_start_ms >= previous_end_ms,
                "speech window source regions overlap or are out of order"
            );
        }
        previous_source_end_ms = Some(source_end_ms);
        let start_samples = (region.source_start_ms as usize)
            .checked_mul(PCM_SAMPLES_PER_MS)
            .ok_or_else(|| anyhow::anyhow!("speech region start overflowed"))?;
        let duration_samples = (region.duration_ms as usize)
            .checked_mul(PCM_SAMPLES_PER_MS)
            .ok_or_else(|| anyhow::anyhow!("speech region duration overflowed"))?;
        let end_samples = start_samples
            .checked_add(duration_samples)
            .ok_or_else(|| anyhow::anyhow!("speech region end overflowed"))?;
        ensure!(
            end_samples <= pcm.len(),
            "speech region {}..{} exceeds the supplied audio",
            region.source_start_ms,
            source_end_ms
        );
        let next_sample_count = packed
            .len()
            .checked_add(duration_samples)
            .ok_or_else(|| anyhow::anyhow!("speech window sample count overflowed"))?;
        ensure!(
            next_sample_count <= MAX_SPEECH_WINDOW_SAMPLES,
            "speech window exceeds the 30-second cloud limit"
        );

        packed.extend_from_slice(&pcm[start_samples..end_samples]);
        expected_offset_ms = expected_offset_ms
            .checked_add(region.duration_ms)
            .ok_or_else(|| anyhow::anyhow!("speech window duration overflowed"))?;
        total_ms = total_ms
            .checked_add(region.duration_ms)
            .ok_or_else(|| anyhow::anyhow!("speech window duration overflowed"))?;
    }

    ensure!(total_ms > 0, "speech window is empty");
    ensure!(total_ms <= WINDOW_MS, "speech window exceeds 30 seconds");
    ensure!(
        packed.len() == total_ms as usize * PCM_SAMPLES_PER_MS,
        "speech window sample count does not match its declared duration"
    );
    Ok(packed)
}

/// Encodes normalized samples as a genuine 16 kHz mono signed 16-bit WAV.
///
/// Every value must be finite. Out-of-range amplitudes are clamped to the
/// representable PCM range; NaN and infinity are rejected rather than silently
/// becoming digital silence.
pub fn encode_pcm16_wav(samples: &[f32]) -> Result<Vec<u8>> {
    ensure!(!samples.is_empty(), "cannot encode an empty speech window");
    ensure!(
        samples.len() <= MAX_SPEECH_WINDOW_SAMPLES,
        "speech window exceeds 30 seconds"
    );
    ensure!(
        samples.iter().all(|sample| sample.is_finite()),
        "speech window contains a non-finite sample"
    );

    let spec = WavSpec {
        channels: SPEECH_CHANNELS,
        sample_rate: SPEECH_SAMPLE_RATE_HZ,
        bits_per_sample: SPEECH_BITS_PER_SAMPLE,
        sample_format: SampleFormat::Int,
    };
    let mut buffer = Cursor::new(Vec::new());
    let mut writer = WavWriter::new(&mut buffer, spec).context("create in-memory PCM WAV")?;
    for sample in samples {
        let pcm = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        writer.write_sample(pcm).context("write PCM WAV sample")?;
    }
    writer.finalize().context("finalize PCM WAV")?;

    let bytes = buffer.into_inner();
    ensure!(
        bytes.len() <= MAX_SPEECH_WAV_BYTES,
        "encoded speech WAV exceeds the bounded wire size"
    );
    Ok(bytes)
}

/// Parses and validates a WAV as canonical 16 kHz mono signed 16-bit PCM.
///
/// Returns the sample count. Used as a defense-in-depth check before a request
/// body is built, so a future caller cannot attach arbitrary bytes to the
/// `input_audio` field and call the resulting payload "speech".
pub fn validate_wav_16k_mono(bytes: &[u8]) -> Result<usize> {
    let mut reader = WavReader::new(Cursor::new(bytes)).context("parse PCM WAV header")?;
    let spec = reader.spec();
    ensure!(spec.channels == SPEECH_CHANNELS, "speech WAV must be mono");
    ensure!(
        spec.sample_rate == SPEECH_SAMPLE_RATE_HZ,
        "speech WAV must be 16 kHz"
    );
    ensure!(
        spec.bits_per_sample == SPEECH_BITS_PER_SAMPLE,
        "speech WAV must be signed 16-bit PCM"
    );
    ensure!(
        spec.sample_format == SampleFormat::Int,
        "speech WAV must be integer PCM"
    );
    let mut count = 0_usize;
    for sample in reader.samples::<i16>() {
        sample.context("read PCM WAV sample")?;
        count = count
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("speech WAV sample count overflowed"))?;
        ensure!(
            count <= MAX_SPEECH_WINDOW_SAMPLES,
            "speech WAV exceeds 30 seconds"
        );
    }
    ensure!(count > 0, "speech WAV contains no samples");
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::windows::PackedRegion;

    fn region(window_offset_ms: u32, source_start_ms: u32, duration_ms: u32) -> PackedRegion {
        PackedRegion {
            window_offset_ms,
            source_start_ms,
            duration_ms,
        }
    }

    #[test]
    fn regions_are_concatenated_in_window_order_at_16khz() {
        // 16 kHz mono: 1 ms is 16 samples.
        let pcm: Vec<f32> = (0..16_000).map(|index| index as f32).collect();
        let window = PackedWindow {
            regions: vec![region(0, 100, 10), region(10, 500, 10)],
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
            regions: vec![region(0, 5, 100)],
        };
        let packed = concatenate_regions(&pcm, &window);
        assert_eq!(packed.len(), 80);
    }

    #[test]
    fn a_region_starting_past_the_audio_contributes_nothing() {
        let pcm: Vec<f32> = vec![0.0; 160];
        let window = PackedWindow {
            regions: vec![region(0, 5_000, 100)],
        };
        assert!(concatenate_regions(&pcm, &window).is_empty());
    }

    #[test]
    fn an_empty_window_concatenates_to_nothing() {
        let pcm: Vec<f32> = vec![0.0; 160];
        assert!(concatenate_regions(&pcm, &PackedWindow::default()).is_empty());
    }

    #[test]
    fn bounded_packing_rejects_a_truncated_source_window() {
        let pcm = vec![0.0; 160];
        let window = PackedWindow {
            regions: vec![region(0, 5, 100)],
        };
        assert!(concatenate_regions_bounded(&pcm, &window).is_err());
    }

    #[test]
    fn bounded_packing_rejects_non_contiguous_offsets_and_overflow() {
        let pcm = vec![0.0; 16_000];
        let non_contiguous = PackedWindow {
            regions: vec![region(0, 0, 10), region(20, 10, 10)],
        };
        assert!(concatenate_regions_bounded(&pcm, &non_contiguous).is_err());

        let overflow = PackedWindow {
            regions: vec![region(0, u32::MAX, 2)],
        };
        assert!(concatenate_regions_bounded(&pcm, &overflow).is_err());
    }

    #[test]
    fn bounded_packing_returns_exact_window_samples() {
        let pcm: Vec<f32> = (0..16_000).map(|index| index as f32).collect();
        let window = PackedWindow {
            regions: vec![region(0, 100, 20), region(20, 500, 30)],
        };
        let packed = concatenate_regions_bounded(&pcm, &window).unwrap();
        assert_eq!(packed.len(), 50 * PCM_SAMPLES_PER_MS);
        assert_eq!(packed[0], 1_600.0);
        assert_eq!(packed[20 * PCM_SAMPLES_PER_MS], 8_000.0);
    }

    #[test]
    fn encoded_wav_is_a_real_16khz_mono_pcm_file() {
        let samples: Vec<f32> = (0..160).map(|index| index as f32 / 160.0).collect();
        let bytes = encode_pcm16_wav(&samples).unwrap();
        assert_eq!(validate_wav_16k_mono(&bytes).unwrap(), samples.len());

        let reader = WavReader::new(Cursor::new(bytes)).unwrap();
        let spec = reader.spec();
        assert_eq!(spec.sample_rate, 16_000);
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.bits_per_sample, 16);
        assert_eq!(spec.sample_format, SampleFormat::Int);
    }

    #[test]
    fn encoding_rejects_non_finite_and_oversized_audio() {
        assert!(encode_pcm16_wav(&[0.0, f32::NAN]).is_err());
        assert!(encode_pcm16_wav(&[f32::INFINITY]).is_err());
        assert!(encode_pcm16_wav(&[]).is_err());
        assert!(encode_pcm16_wav(&vec![0.0; MAX_SPEECH_WINDOW_SAMPLES + 1]).is_err());
    }

    #[test]
    fn the_largest_permitted_window_encodes_within_the_bounded_wire_size() {
        let samples = vec![0.0_f32; MAX_SPEECH_WINDOW_SAMPLES];
        let bytes = encode_pcm16_wav(&samples).unwrap();
        assert!(bytes.len() <= MAX_SPEECH_WAV_BYTES);
        assert_eq!(validate_wav_16k_mono(&bytes).unwrap(), samples.len());
    }
}
