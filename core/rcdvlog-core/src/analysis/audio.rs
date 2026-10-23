//! Extracting whisper-ready audio from a clip's review proxy.
//!
//! Audio always comes from the proxy, never the original. The original lives on
//! a NAS that may be offline, and the design requires transcription to work
//! whenever proxies are present. FFmpeg runs as a managed child process, in the
//! same style as proxy and thumbnail generation.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

use crate::{media_tool_failure, run_captured_media_command, tool_command};

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
        // Fail on damaged audio instead of accepting a partial/empty decode
        // that could be misclassified as literal silence by VAD.
        .arg("-xerror")
        .arg("-err_detect")
        .arg("explode")
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
        let mut command = audio_extract_command(source, destination);
        let (status, diagnostic) = run_captured_media_command(&mut command, source, destination)
            .context("spawn ffmpeg audio extraction process")?;
        if !status.success() {
            return Err(media_tool_failure(
                "ffmpeg audio extraction",
                status,
                diagnostic,
            ));
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
    fn advertised_audio_with_corrupt_packets_is_a_decode_error() {
        if !["ffmpeg", "ffprobe"].iter().all(|tool| {
            crate::tool_command(tool)
                .arg("-version")
                .output()
                .is_ok_and(|output| output.status.success())
        }) {
            return;
        }
        let directory = tempfile::TempDir::new().unwrap();
        let source = directory.path().join("damaged.aac");
        let status = crate::tool_command("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-c:a",
                "aac",
                "-f",
                "adts",
            ])
            .arg(&source)
            .status()
            .unwrap();
        assert!(status.success());
        // Preserve each ADTS header and destroy its AAC payload: FFprobe can
        // still identify an audio stream, while decoding its packets must fail.
        let mut bytes = std::fs::read(&source).unwrap();
        let mut offset = 0;
        while offset + 7 < bytes.len() {
            let size = ((usize::from(bytes[offset + 3]) & 3) << 11)
                | (usize::from(bytes[offset + 4]) << 3)
                | (usize::from(bytes[offset + 5]) >> 5);
            assert!(size >= 7 && offset + size <= bytes.len());
            bytes[offset + 7..offset + size].fill(0xff);
            offset += size;
        }
        std::fs::write(&source, bytes).unwrap();
        assert!(
            crate::probe_source_audio_present(
                &crate::resolve_tool_program(Path::new("ffprobe")),
                &source,
            )
            .unwrap()
        );
        let error = FfmpegAudioExtractor
            .extract_pcm16k(&source, &directory.path().join("decoded.wav"))
            .unwrap_err();
        assert!(format!("{error:#}").contains("ffmpeg audio extraction"));
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
