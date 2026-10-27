//! Bounded-memory 48 kHz source mixing; no automatic ducking or normalization.
use super::audio::{AUDIO_SAMPLE_RATE, AudioEvent};
use crate::{
    media_tool_failure, run_captured_media_command_with_cancellation as run_media, tool_command,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

pub(super) fn render_mix(
    workspace: &Path,
    events: &[AudioEvent],
    movie_seconds: f64,
    cancel: &dyn Fn() -> bool,
) -> Result<PathBuf> {
    let mix_path = workspace.join("source-audio.f32le");
    let mut mix = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&mix_path)?;
    let total_samples = (movie_seconds * f64::from(AUDIO_SAMPLE_RATE)).ceil() as u64;
    mix.set_len(
        total_samples
            .checked_mul(8)
            .context("audio mix length overflow")?,
    )?;
    for (index, event) in events.iter().enumerate() {
        ensure!(!cancel(), "preview render cancelled during source audio");
        let decoded_path = workspace.join(format!("source-audio-{index:05}.f32le"));
        // Seek on a whole-second native sample boundary, retaining at least a
        // second of decoder/resampler history. Final trimming stays on the
        // source-derived 48 kHz grid instead of rounding a seek to microseconds.
        let seek_seconds =
            (event.source_in_samples / u64::from(AUDIO_SAMPLE_RATE)).saturating_sub(1);
        let trim_in = event.source_in_samples - seek_seconds * u64::from(AUDIO_SAMPLE_RATE);
        let mut command = tool_command("ffmpeg");
        command
            .args(["-y", "-hide_banner", "-loglevel", "error", "-ss"])
            .arg(seek_seconds.to_string())
            .arg("-i")
            .arg(&event.source)
            .args(["-map", "0:a:0", "-vn", "-sn", "-dn", "-af"])
            .arg(format!(
                "aresample=48000:async=1:first_pts=0,atrim=start_sample={trim_in}:end_sample={},asetpts=PTS-STARTPTS",
                trim_in + event.sample_count()
            ))
            .args([
                "-ar",
                "48000",
                "-ac",
                "2",
                "-c:a",
                "pcm_f32le",
                "-f",
                "f32le",
            ])
            .arg(&decoded_path);
        let (status, diagnostic) = run_media(
            &mut command,
            Path::new(&event.source),
            &decoded_path,
            &mut || cancel(),
        )?;
        if !status.success() {
            return Err(media_tool_failure(
                "ffmpeg independent source audio",
                status,
                diagnostic,
            ));
        }
        let expected_bytes = event.sample_count() * 8;
        ensure!(
            decoded_path.metadata()?.len() == expected_bytes,
            "source audio did not provide the complete requested sample range; refusing a truncated preview"
        );
        let mut decoded = File::open(&decoded_path)?;
        let mut source_bytes = vec![0_u8; 4096 * 8];
        let mut mixed_bytes = vec![0_u8; 4096 * 8];
        let gain = 10.0_f64.powf(event.gain_db / 20.0) as f32;
        let mut offset_samples = 0;
        while offset_samples < event.sample_count() {
            ensure!(!cancel(), "preview render cancelled during audio mixing");
            let count = (event.sample_count() - offset_samples).min(4096) as usize;
            let bytes = count * 8;
            decoded.read_exact(&mut source_bytes[..bytes])?;
            let destination = (event.timeline_start_samples + offset_samples) * 8;
            ensure!(
                destination + bytes as u64 <= total_samples * 8,
                "audio placement exceeds movie sample bounds"
            );
            mix.seek(SeekFrom::Start(destination))?;
            mix.read_exact(&mut mixed_bytes[..bytes])?;
            for sample in 0..count {
                let at = offset_samples + sample as u64;
                let attack = if event.fade_in_samples > 0 {
                    (at as f32 / event.fade_in_samples as f32).min(1.0)
                } else {
                    1.0
                };
                let release = if event.fade_out_samples > 0 {
                    ((event.sample_count() - 1 - at) as f32 / event.fade_out_samples as f32)
                        .min(1.0)
                } else {
                    1.0
                };
                for channel in 0..2 {
                    let byte = sample * 8 + channel * 4;
                    let source =
                        f32::from_le_bytes(source_bytes[byte..byte + 4].try_into().unwrap());
                    let prior = f32::from_le_bytes(mixed_bytes[byte..byte + 4].try_into().unwrap());
                    ensure!(
                        source.is_finite() && prior.is_finite(),
                        "nonfinite decoded source audio"
                    );
                    mixed_bytes[byte..byte + 4]
                        .copy_from_slice(&(prior + source * gain * attack * release).to_le_bytes());
                }
            }
            mix.seek(SeekFrom::Start(destination))?;
            mix.write_all(&mixed_bytes[..bytes])?;
            offset_samples += count as u64;
        }
        drop(decoded);
        std::fs::remove_file(decoded_path)?;
    }
    mix.sync_all()?;
    Ok(mix_path)
}

pub(super) fn mux_mix(
    video: &Path,
    audio: &Path,
    output: &Path,
    cancel: &dyn Fn() -> bool,
) -> Result<f64> {
    let mut command = tool_command("ffmpeg");
    command
        .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(video)
        .args(["-f", "f32le", "-ar", "48000", "-ac", "2", "-i"])
        .arg(audio)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-map_metadata",
            "-1",
            "-movflags",
            "+faststart",
        ])
        .arg(output);
    let (status, diagnostic) = run_media(&mut command, video, output, &mut || cancel())?;
    if !status.success() {
        return Err(media_tool_failure(
            "ffmpeg split audio mux",
            status,
            diagnostic,
        ));
    }
    let mut command = tool_command("ffmpeg");
    command
        .args(["-v", "error", "-xerror", "-i"])
        .arg(output)
        .args(["-map", "0:v:0", "-map", "0:a:0", "-f", "null", "-"]);
    let (status, diagnostic) = run_media(&mut command, output, output, &mut || cancel())?;
    if !status.success() {
        return Err(media_tool_failure(
            "decode split audio preview",
            status,
            diagnostic,
        ));
    }
    let mut command = tool_command("ffprobe");
    command
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "json",
        ])
        .arg(output);
    let result = crate::stall::output_with_timeout_and_cancellation(
        &mut command,
        "inspect split audio duration",
        crate::stall::io_stall_timeout(),
        &cancel,
    )?;
    if !result.status.success() {
        bail!("cannot inspect split audio preview duration");
    }
    let json: serde_json::Value = serde_json::from_slice(&result.stdout)?;
    json["format"]["duration"]
        .as_str()
        .context("preview duration missing")?
        .parse()
        .context("invalid preview duration")
}
