//! Extracting whisper-ready audio from a clip's review proxy.
//!
//! Audio always comes from the proxy, never the original. The original lives on
//! a NAS that may be offline, and the design requires transcription to work
//! whenever proxies are present. FFmpeg runs as a managed child process, in the
//! same style as proxy and thumbnail generation.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

use crate::{media_tool_failure, run_captured_media_command_with_cancellation, tool_command};

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

impl FfmpegAudioExtractor {
    /// Cancellation-aware extraction for the native speech preparation path.
    /// The existing trait method remains the behavior-compatible local entry
    /// point and delegates here with an uncancelled closure.
    pub(crate) fn extract_pcm16k_cancellable(
        &self,
        source: &Path,
        destination: &Path,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<()> {
        if is_cancelled() {
            bail!("ffmpeg audio extraction canceled before spawn");
        }
        if !source.is_file() {
            bail!("proxy is offline: {}", source.display());
        }
        let mut command = audio_extract_command(source, destination);
        let (status, diagnostic) = run_captured_media_command_with_cancellation(
            &mut command,
            source,
            destination,
            is_cancelled,
        )
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

impl AudioExtractor for FfmpegAudioExtractor {
    fn extract_pcm16k(&self, source: &Path, destination: &Path) -> Result<()> {
        self.extract_pcm16k_cancellable(source, destination, &mut || false)
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
    fn early_extraction_cancel_spawns_nothing() {
        let directory = tempfile::TempDir::new().unwrap();
        let marker = directory.path().join("spawned");
        let mut command = Command::new("sh");
        command.arg("-c").arg("touch \"$1\"").arg("sh").arg(&marker);
        let error = crate::run_captured_media_command_with_cancellation(
            &mut command,
            Path::new("/unused/source.mp4"),
            Path::new("/unused/output.wav"),
            &mut || true,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("canceled before spawn"),
            "{error}"
        );
        assert!(!marker.exists(), "a canceled extraction must not spawn");
    }

    #[cfg(unix)]
    #[test]
    fn extraction_cancel_kills_reaps_and_drains_stderr() {
        use std::time::{Duration, Instant};

        let directory = tempfile::TempDir::new().unwrap();
        let pid_file = directory.path().join("child.pid");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(
                "echo $$ > \"$1\"; i=0; while [ \"$i\" -lt 16384 ]; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx' >&2; i=$((i+1)); done; exec sleep 60",
            )
            .arg("sh")
            .arg(&pid_file);
        let started = Instant::now();
        let error = crate::run_captured_media_command_with_cancellation(
            &mut command,
            Path::new("/unused/source.mp4"),
            Path::new("/unused/output.wav"),
            &mut || pid_file.exists() && started.elapsed() > Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(error.to_string().contains("canceled"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "canceled child took too long to kill and reap"
        );
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let alive = unsafe { libc::kill(pid, 0) };
        assert_eq!(alive, -1, "canceled child {pid} was not reaped");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
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
