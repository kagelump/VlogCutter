//! Extracting whisper-ready audio from a clip's review proxy.
//!
//! Audio always comes from the proxy, never the original. The original lives on
//! a NAS that may be offline, and the design requires transcription to work
//! whenever proxies are present. FFmpeg runs as a managed child process, in the
//! same style as proxy and thumbnail generation.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

use crate::tool_command;

/// Extracts 16 kHz mono PCM, the only input format whisper.cpp accepts.
pub trait AudioExtractor {
    fn extract_pcm16k(&self, source: &Path, destination: &Path) -> Result<()>;
}

/// Builds the extraction command. Separated from execution so the argument
/// contract can be asserted without invoking FFmpeg.
pub fn audio_extract_command(source: &Path, destination: &Path) -> Command {
    let mut command = tool_command("ffmpeg");
    command
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-i")
        .arg(source)
        .arg("-vn")
        .arg("-ar")
        .arg("16000")
        .arg("-ac")
        .arg("1")
        .arg("-acodec")
        .arg("pcm_s16le")
        .arg(destination);
    command
}

pub struct FfmpegAudioExtractor;

impl AudioExtractor for FfmpegAudioExtractor {
    fn extract_pcm16k(&self, source: &Path, destination: &Path) -> Result<()> {
        if !source.is_file() {
            bail!("proxy is offline: {}", source.display());
        }
        let status = audio_extract_command(source, destination)
            .status()
            .context("spawn ffmpeg audio extraction process")?;
        if !status.success() {
            bail!("ffmpeg audio extraction failed with {status}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered_args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn extraction_requests_16khz_mono_signed_16bit_pcm() {
        let command = audio_extract_command(Path::new("/cache/p.mp4"), Path::new("/tmp/o.wav"));
        let args = rendered_args(&command);
        assert!(args.windows(2).any(|pair| pair == ["-ar", "16000"]));
        assert!(args.windows(2).any(|pair| pair == ["-ac", "1"]));
        assert!(args.windows(2).any(|pair| pair == ["-acodec", "pcm_s16le"]));
    }

    #[test]
    fn extraction_drops_video_and_overwrites() {
        let command = audio_extract_command(Path::new("/cache/p.mp4"), Path::new("/tmp/o.wav"));
        let args = rendered_args(&command);
        assert!(args.iter().any(|arg| arg == "-vn"));
        assert!(args.iter().any(|arg| arg == "-y"));
    }

    #[test]
    fn extraction_reads_the_given_source_and_writes_the_given_destination() {
        let command = audio_extract_command(Path::new("/cache/p.mp4"), Path::new("/tmp/o.wav"));
        let args = rendered_args(&command);
        let input_index = args.iter().position(|arg| arg == "-i").unwrap();
        assert_eq!(args[input_index + 1], "/cache/p.mp4");
        assert_eq!(args.last().unwrap(), "/tmp/o.wav");
    }

    #[test]
    fn an_offline_proxy_fails_before_spawning_ffmpeg() {
        let error = FfmpegAudioExtractor
            .extract_pcm16k(
                Path::new("/nonexistent/proxy.mp4"),
                Path::new("/tmp/never.wav"),
            )
            .unwrap_err();
        assert!(error.to_string().contains("proxy is offline"), "{error}");
    }
}
