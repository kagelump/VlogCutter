//! Turning a proxy into a frame-pack.
//!
//! Two FFmpeg calls: one emits every candidate as raw grayscale at the working
//! size, which `frames` scores; the second encodes only the selected timestamps
//! as 768 px JPEGs for the model.
//!
//! Frames always come from the proxy. Measured, the same recipe over the 4K
//! original produced a different pack on 3 of 4 clips and took 44x longer, and
//! the original may be offline on a NAS while review must still work.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::analysis::FRAMEPACK_RECIPE_KEY;
use crate::analysis::frames::{GrayFrame, score_frames, select_frames};
use crate::{media_tool_failure, redact_media_diagnostic, run_captured_media_command, tool_command};

const CANDIDATE_FPS: u32 = 2;
const WORK_WIDTH: usize = 320;
const WORK_HEIGHT: usize = 180;
/// The first moments of a clip are usually a hand on the camera.
const SKIP_MS: u32 = 400;
const MAX_CANDIDATES: usize = 40;
/// Both Google models bill a flat token count regardless of input width, and OCR
/// agreement drops by a third to a half below this. Frame width is a =<1.4x cost
/// lever against a 27x model-choice lever: do not "optimize" here.
const JPEG_WIDTH: u32 = 768;

/// The representation frame-packs are pinned to. Part of their stored identity,
/// because a different representation selects different frames.
pub const REPRESENTATION: &str = "proxy.h264-720p.orientation-v3";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FramePackManifest {
    pub recipe_key: String,
    pub representation: String,
    pub source_fingerprint: String,
    pub timestamps_ms: Vec<u32>,
}

pub fn candidate_command(proxy: &Path, fps: u32, width: usize, height: usize) -> Command {
    let mut command = tool_command("ffmpeg");
    command
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-ss")
        .arg(format!("{}.{:03}", SKIP_MS / 1_000, SKIP_MS % 1_000))
        .arg("-i")
        .arg(proxy)
        .arg("-vf")
        .arg(format!("fps={fps},scale={width}x{height}"))
        .arg("-pix_fmt")
        .arg("gray")
        .arg("-f")
        .arg("rawvideo")
        .arg("-");
    command
}

pub fn jpeg_command(proxy: &Path, timestamp_ms: u32, output: &Path) -> Command {
    let mut command = tool_command("ffmpeg");
    command
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-ss")
        .arg(format!(
            "{}.{:03}",
            timestamp_ms / 1_000,
            timestamp_ms % 1_000
        ))
        .arg("-i")
        .arg(proxy)
        .arg("-frames:v")
        .arg("1")
        .arg("-vf")
        .arg(format!("scale={JPEG_WIDTH}:-2"))
        .arg("-q:v")
        .arg("3")
        .arg(output);
    command
}

/// Splits the raw stream into frames, stamping each with its sample time.
///
/// The candidate budget is spread evenly across the whole stream rather than
/// taken from its head. FFmpeg decodes the clip in full either way, so keeping
/// only the first `MAX_CANDIDATES` bought nothing and made every clip longer
/// than `MAX_CANDIDATES / fps` seconds describe only its opening: on a
/// multi-minute clip the title, labels, best moment and representative
/// thumbnail would all come from the first twenty seconds.
///
/// A trailing partial frame is discarded rather than padded: a truncated frame
/// would score as though its missing rows were black.
pub fn parse_gray_frames(
    bytes: &[u8],
    width: usize,
    height: usize,
    fps: u32,
    skip_ms: u32,
) -> Vec<GrayFrame> {
    let frame_bytes = width * height;
    if frame_bytes == 0 || fps == 0 {
        return Vec::new();
    }
    let interval_ms = 1_000 / fps;
    let available = bytes.len() / frame_bytes;
    if available == 0 {
        return Vec::new();
    }
    // Integer striding, so the same stream always yields the same candidates.
    let count = available.min(MAX_CANDIDATES);
    (0..count)
        .map(|slot| {
            let index = slot * available / count;
            let start = index * frame_bytes;
            GrayFrame {
                timestamp_ms: skip_ms + index as u32 * interval_ms,
                width,
                height,
                pixels: bytes[start..start + frame_bytes].to_vec(),
            }
        })
        .collect()
}

fn retain_interior_frames(frames: &mut Vec<GrayFrame>, duration_ms: u32) {
    let last_allowed = duration_ms.saturating_sub(SKIP_MS);
    frames.retain(|frame| frame.timestamp_ms <= last_allowed);
}

/// Samples, scores, and selects a clip's frames.
pub fn build_frame_pack(proxy: &Path, fingerprint: &str) -> Result<FramePackManifest> {
    if !proxy.is_file() {
        bail!(
            "proxy is required to build a frame pack: {}",
            proxy.display()
        );
    }
    let output = candidate_command(proxy, CANDIDATE_FPS, WORK_WIDTH, WORK_HEIGHT)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("spawn ffmpeg frame sampling")?;
    if !output.status.success() {
        let diagnostic = redact_media_diagnostic(
            &String::from_utf8_lossy(&output.stderr),
            proxy,
            proxy,
        );
        return Err(media_tool_failure("ffmpeg frame sampling", output.status, diagnostic));
    }
    let duration_output = tool_command("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(proxy)
        .output()
        .context("spawn ffprobe frame-pack duration")?;
    if !duration_output.status.success() {
        let diagnostic = redact_media_diagnostic(
            &String::from_utf8_lossy(&duration_output.stderr),
            proxy,
            proxy,
        );
        return Err(media_tool_failure(
            "ffprobe frame-pack duration",
            duration_output.status,
            diagnostic,
        ));
    }
    let duration_ms = String::from_utf8_lossy(&duration_output.stdout)
        .trim()
        .parse::<f64>()
        .context("parse frame-pack duration")?
        .mul_add(1_000.0, 0.0)
        .round() as u32;
    let mut frames = parse_gray_frames(
        &output.stdout,
        WORK_WIDTH,
        WORK_HEIGHT,
        CANDIDATE_FPS,
        SKIP_MS,
    );
    retain_interior_frames(&mut frames, duration_ms);
    if frames.is_empty() {
        bail!("no frames could be sampled from {}", proxy.display());
    }
    Ok(FramePackManifest {
        recipe_key: FRAMEPACK_RECIPE_KEY.to_string(),
        representation: REPRESENTATION.to_string(),
        source_fingerprint: fingerprint.to_string(),
        timestamps_ms: select_frames(&score_frames(&frames), &frames),
    })
}

/// Encodes the selected timestamps as 768 px JPEGs.
pub fn encode_frames(
    proxy: &Path,
    manifest: &FramePackManifest,
    directory: &Path,
) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(directory)
        .with_context(|| format!("create frame directory {}", directory.display()))?;
    let mut written = Vec::new();
    for timestamp in &manifest.timestamps_ms {
        let path = directory.join(format!("f{timestamp:08}.jpg"));
        let mut command = jpeg_command(proxy, *timestamp, &path);
        let (status, diagnostic) = run_captured_media_command(&mut command, proxy, &path)
            .context("spawn ffmpeg frame encode")?;
        if !status.success() {
            return Err(media_tool_failure("ffmpeg frame encode", status, diagnostic));
        }
        written.push(path);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn candidates_are_raw_gray_at_the_working_size_and_rate() {
        let rendered = args(&candidate_command(Path::new("/c/p.mp4"), 2, 320, 180));
        assert!(rendered.iter().any(|arg| arg.contains("fps=2")));
        assert!(rendered.iter().any(|arg| arg.contains("320x180")));
        assert!(rendered.windows(2).any(|pair| pair == ["-pix_fmt", "gray"]));
        assert!(rendered.windows(2).any(|pair| pair == ["-f", "rawvideo"]));
        let seek = rendered.iter().position(|arg| arg == "-ss").unwrap();
        let input = rendered.iter().position(|arg| arg == "-i").unwrap();
        assert!(seek < input, "the first 400 ms must be skipped at input");
    }

    #[test]
    fn jpeg_encoding_requests_768px_and_a_single_frame() {
        let rendered = args(&jpeg_command(
            Path::new("/c/p.mp4"),
            2_500,
            Path::new("/t/f.jpg"),
        ));
        assert!(rendered.iter().any(|arg| arg == "2.500"));
        assert!(rendered.iter().any(|arg| arg.contains("768")));
        assert!(rendered.windows(2).any(|pair| pair == ["-frames:v", "1"]));
    }

    #[test]
    fn seeking_precedes_the_input_so_ffmpeg_seeks_rather_than_decodes_forward() {
        let rendered = args(&jpeg_command(
            Path::new("/c/p.mp4"),
            2_500,
            Path::new("/t/f.jpg"),
        ));
        let seek = rendered.iter().position(|arg| arg == "-ss").unwrap();
        let input = rendered.iter().position(|arg| arg == "-i").unwrap();
        assert!(seek < input, "-ss must come before -i");
    }

    #[test]
    fn raw_bytes_split_into_frames_at_the_sampling_interval() {
        let frame_bytes = 4 * 2;
        let bytes = vec![7_u8; frame_bytes * 3];
        let frames = parse_gray_frames(&bytes, 4, 2, 2, 400);
        assert_eq!(frames.len(), 3);
        // 2 fps, starting after the 400 ms skip.
        assert_eq!(frames[0].timestamp_ms, 400);
        assert_eq!(frames[1].timestamp_ms, 900);
        assert_eq!(frames[2].timestamp_ms, 1_400);
        assert_eq!(frames[0].pixels.len(), frame_bytes);
    }

    #[test]
    fn a_trailing_partial_frame_is_discarded() {
        let bytes = vec![0_u8; 8 * 2 + 3];
        assert_eq!(parse_gray_frames(&bytes, 4, 2, 2, 400).len(), 2);
    }

    #[test]
    fn candidates_are_capped_at_forty() {
        let bytes = vec![0_u8; 8 * 100];
        assert_eq!(parse_gray_frames(&bytes, 4, 2, 2, 400).len(), 40);
    }

    #[test]
    fn candidates_span_the_whole_clip_rather_than_its_opening() {
        // 600 sampled frames is a five-minute clip at 2 fps. Truncating to the
        // first 40 would describe only its first twenty seconds.
        let bytes = vec![0_u8; 8 * 600];
        let frames = parse_gray_frames(&bytes, 4, 2, 2, 400);
        assert_eq!(frames.len(), MAX_CANDIDATES);
        assert_eq!(frames[0].timestamp_ms, 400);
        // The last candidate sits near the end of the clip, not at 20 s.
        assert!(
            frames.last().unwrap().timestamp_ms > 290_000,
            "last candidate was {}",
            frames.last().unwrap().timestamp_ms
        );
        assert!(
            frames
                .windows(2)
                .all(|pair| pair[0].timestamp_ms < pair[1].timestamp_ms),
            "candidates must stay ascending and distinct"
        );
    }

    #[test]
    fn striding_is_deterministic_for_the_same_stream() {
        let bytes: Vec<u8> = (0..8 * 137).map(|index| (index % 251) as u8).collect();
        let first = parse_gray_frames(&bytes, 4, 2, 2, 400);
        let second = parse_gray_frames(&bytes, 4, 2, 2, 400);
        assert_eq!(first, second);
    }

    #[test]
    fn a_short_clip_keeps_every_sampled_frame() {
        let bytes = vec![0_u8; 8 * 12];
        let frames = parse_gray_frames(&bytes, 4, 2, 2, 400);
        assert_eq!(frames.len(), 12);
        assert_eq!(frames[11].timestamp_ms, 400 + 11 * 500);
    }

    #[test]
    fn empty_input_parses_to_no_frames() {
        assert!(parse_gray_frames(&[], 4, 2, 2, 400).is_empty());
    }

    #[test]
    fn the_last_four_hundred_milliseconds_are_dropped() {
        let mut frames = parse_gray_frames(&[0; 8 * 4], 4, 2, 2, 400);
        retain_interior_frames(&mut frames, 1_500);
        assert_eq!(
            frames
                .iter()
                .map(|frame| frame.timestamp_ms)
                .collect::<Vec<_>>(),
            vec![400, 900]
        );
    }

    #[test]
    fn an_absent_proxy_fails_before_spawning_ffmpeg() {
        let error = build_frame_pack(Path::new("/nonexistent/p.mp4"), "fp").unwrap_err();
        assert!(error.to_string().contains("proxy is required"), "{error}");
    }

    #[test]
    fn the_manifest_carries_representation_identity_not_just_the_recipe() {
        // A recipe key alone is insufficient provenance: a proxy regeneration
        // must invalidate stored packs.
        let manifest = FramePackManifest {
            recipe_key: FRAMEPACK_RECIPE_KEY.to_string(),
            representation: REPRESENTATION.to_string(),
            source_fingerprint: "fp-1".to_string(),
            timestamps_ms: vec![400],
        };
        let encoded = serde_json::to_string(&manifest).unwrap();
        assert!(encoded.contains("representation"));
        assert!(encoded.contains("sourceFingerprint"));
    }
}
