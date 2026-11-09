//! Media tooling: ffprobe/ExifTool probing, timestamp evidence, fingerprints and captured child-process helpers.

use crate::*;
use anyhow::{Context, Result, anyhow, bail};
use blake3::Hasher;
use chrono::{DateTime, Duration, NaiveDateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

pub(crate) const SAMPLE_BYTES: usize = 1024 * 1024;
pub(crate) const MEDIA_EXTENSIONS: &[&str] = &["mp4", "mov", "m4v", "mts", "m2ts", "avi"];
pub(crate) fn full_media_fingerprint(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .with_context(|| format!("open selected representation {}", path.display()))?;
    let mut hasher = Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read selected representation {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Probes the encoded video stream and its display rotation without changing catalog state.
pub fn probe_media_stream(path: impl AsRef<Path>) -> Result<MediaStreamInfo> {
    let path = path.as_ref();
    if !path.is_file() {
        bail!("media file not found: {}", path.display());
    }
    let probe = probe_media_document(path)?;
    let stream = require_video_stream(&probe)?;
    let width = stream["width"]
        .as_u64()
        .ok_or_else(|| anyhow!("video stream has no usable width"))? as u32;
    let height = stream["height"]
        .as_u64()
        .ok_or_else(|| anyhow!("video stream has no usable height"))? as u32;
    let metadata_rotation_degrees = stream_rotation(Some(stream));
    let sideways = matches!(metadata_rotation_degrees, 90 | 270);
    let (display_width, display_height) = if sideways {
        (height, width)
    } else {
        (width, height)
    };
    let display_orientation = orientation_name(display_width, display_height).to_string();
    Ok(MediaStreamInfo {
        path: path.to_string_lossy().into_owned(),
        width,
        height,
        codec: stream["codec_name"]
            .as_str()
            .ok_or_else(|| anyhow!("video stream has no codec name"))?
            .to_string(),
        metadata_rotation_degrees,
        display_width,
        display_height,
        display_orientation,
    })
}

#[derive(Debug, Default)]
pub(crate) struct MediaProbe {
    pub(crate) duration_seconds: f64,
    pub(crate) frame_rate: f64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) codec: String,
    pub(crate) camera: String,
    pub(crate) captured_at_raw: Option<String>,
    pub(crate) timestamp_candidates: Vec<chronology::TimestampCandidate>,
    pub(crate) best_time: Option<String>,
    pub(crate) metadata_rotation_degrees: u16,
    pub(crate) default_rotation_degrees: u16,
}

pub(crate) fn camera_sidecar_fingerprint(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Hasher::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub(crate) fn camera_sidecar_has_aac(path: &Path) -> Result<bool> {
    let ffprobe = resolve_tool("ffprobe");
    let mut command = Command::new(ffprobe);
    command
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(path);
    let output = crate::stall::output_with_timeout(
        &mut command,
        &format!("ffprobe audio in {}", path.display()),
        crate::stall::io_stall_timeout(),
    )?;
    Ok(output.status.success()
        && String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.trim() == "aac"))
}

pub(crate) fn verify_camera_sidecar_decode(path: &Path) -> Result<()> {
    let ffmpeg = resolve_tool("ffmpeg");
    let mut command = Command::new(ffmpeg);
    command
        .args(["-v", "error", "-xerror", "-i"])
        .arg(path)
        .args(["-f", "null", "-"]);
    let output = crate::stall::output_with_timeout(
        &mut command,
        &format!("decode camera proxy {}", path.display()),
        crate::stall::io_stall_timeout(),
    )?;
    if !output.status.success() {
        bail!("truncated_or_unreadable: full proxy decode failed")
    }
    Ok(())
}

pub(crate) fn normalize_rotation(degrees: f64) -> u16 {
    let rounded = degrees.round() as i32;
    let normalized = rounded.rem_euclid(360) as u16;
    match normalized {
        0..=44 | 316..=359 => 0,
        45..=134 => 90,
        135..=224 => 180,
        _ => 270,
    }
}

pub fn landscape_default_rotation(width: u32, height: u32, metadata_rotation_degrees: u16) -> u16 {
    let metadata_turns_sideways = matches!(metadata_rotation_degrees, 90 | 270);
    let displayed_width = if metadata_turns_sideways {
        height
    } else {
        width
    };
    let displayed_height = if metadata_turns_sideways {
        width
    } else {
        height
    };
    if displayed_height > displayed_width {
        (metadata_rotation_degrees + 90) % 360
    } else {
        metadata_rotation_degrees
    }
}

/// Returns true when the path has a supported media extension (case-insensitive).
/// The extension set is the same one used for desktop ingest and server scanning.
pub fn is_supported_media_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| MEDIA_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

pub(crate) fn is_media(path: &Path) -> bool {
    is_supported_media_path(path)
}

pub(crate) fn tool_command(name: &str) -> Command {
    Command::new(resolve_tool(name))
}

pub(crate) fn drain_child_stderr(
    mut stderr: impl Read + Send + 'static,
) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let count = match stderr.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if kept.len() < 4096 {
                kept.extend_from_slice(&buffer[..count.min(4096 - kept.len())]);
            }
        }
        kept
    })
}

pub(crate) fn run_captured_media_command(
    command: &mut Command,
    input: &Path,
    output: &Path,
) -> Result<(std::process::ExitStatus, String)> {
    run_captured_media_command_with_cancellation(command, input, output, &mut || false)
}

/// Shared captured-child execution with cancellation. The child is killed and
/// reaped before this returns, and its stderr drain thread is joined so a
/// canceled FFmpeg cannot leave an orphan or a blocked pipe behind.
pub(crate) fn run_captured_media_command_with_cancellation(
    command: &mut Command,
    input: &Path,
    output: &Path,
    is_cancelled: &mut dyn FnMut() -> bool,
) -> Result<(std::process::ExitStatus, String)> {
    if is_cancelled() {
        bail!("media tool process canceled before spawn");
    }
    command.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = command.spawn().context("spawn media tool process")?;
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            bail!("media tool stderr pipe was not available");
        }
    };
    let stderr_reader = drain_child_stderr(stderr);
    let status = loop {
        if is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stderr_reader.join();
            bail!("media tool process canceled");
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(crate::stall::POLL_INTERVAL),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stderr_reader.join();
                return Err(error).context("wait for media tool process");
            }
        }
    };
    let stderr = stderr_reader.join().unwrap_or_default();
    let stderr = String::from_utf8_lossy(&stderr).into_owned();
    let diagnostic = redact_media_diagnostic(&stderr, input, output);
    Ok((status, diagnostic))
}

pub(crate) fn redact_media_diagnostic(stderr: &str, source: &Path, output: &Path) -> String {
    let exact_redacted = stderr
        .replace(&source.to_string_lossy().to_string(), "<source>")
        .replace(&output.to_string_lossy().to_string(), "<output>");
    let absolute_paths = regex::Regex::new(
        r#"'(?:[A-Za-z]:\\|/)[^']*'|"(?:[A-Za-z]:\\|/)[^"]*"|(?:[A-Za-z]:\\|/)[^\s:'"<>]+"#,
    )
    .expect("constant media path pattern");
    let redacted = absolute_paths.replace_all(&exact_redacted, "<path>");
    let trimmed = redacted.trim();
    let mut end = trimmed.len().min(4094);
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    if end == 0 {
        String::new()
    } else {
        format!(": {}", &trimmed[..end])
    }
}

pub(crate) fn ffmpeg_progress_fraction(line: &str, duration_seconds: f64) -> Option<f64> {
    if !duration_seconds.is_finite() || duration_seconds <= 0.0 {
        return None;
    }
    let output_microseconds = line.strip_prefix("out_time_us=")?.parse::<u64>().ok()?;
    let fraction = output_microseconds as f64 / 1_000_000.0 / duration_seconds;
    fraction.is_finite().then_some(fraction.clamp(0.0, 1.0))
}

/// Resolves a tool executable path from the environment. Returns the command
/// name itself when no concrete path is found on PATH or standard directories.
pub fn resolve_tool_path(name: &str) -> PathBuf {
    resolve_tool(name)
}

/// Resolves a configured program without replacing an explicit relative or
/// absolute path. Bare command names are looked up on `PATH`; explicit paths
/// are preserved so callers can identify the exact executable they execute.
pub fn resolve_tool_program(program: &Path) -> PathBuf {
    if program.is_absolute() || program.components().count() > 1 {
        return program.to_path_buf();
    }
    resolve_tool(&program.to_string_lossy())
}

pub(crate) fn resolve_tool(name: &str) -> PathBuf {
    if let Some(path) = env::var_os("PATH").and_then(|path| {
        env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    }) {
        return path;
    }
    for directory in [
        "/opt/homebrew/bin",
        "/opt/homebrew/opt/ffmpeg@7/bin",
        "/opt/homebrew/opt/ffmpeg/bin",
        "/usr/local/bin",
        "/usr/bin",
    ] {
        let candidate = Path::new(directory).join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(name)
}

/// Computes a sampled BLAKE3 fingerprint of a file: file size plus up to 1 MiB
/// from the beginning and end. Returns a 64-char lowercase hex digest.
pub fn sampled_media_fingerprint(path: &Path, size: u64) -> Result<String> {
    sampled_fingerprint(path, size)
}

pub(crate) fn sampled_fingerprint(path: &Path, size: u64) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Hasher::new();
    hasher.update(&size.to_le_bytes());
    let mut buffer = vec![0; SAMPLE_BYTES.min(size as usize)];
    let read = file.read(&mut buffer)?;
    hasher.update(&buffer[..read]);
    if size > SAMPLE_BYTES as u64 {
        file.seek(SeekFrom::End(-(SAMPLE_BYTES.min(size as usize) as i64)))?;
        let read = file.read(&mut buffer)?;
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Probes a media file with ffprobe and optional ExifTool enrichment.
pub(crate) fn probe_media(path: &Path) -> Result<MediaProbe> {
    let document = probe_media_document(path)?;
    let stream = require_video_stream(&document)?;
    let timestamp_candidates = collect_timestamp_candidates(path, &document);
    let raw_time = stream
        .pointer("/tags/creation_time")
        .or_else(|| document.pointer("/format/tags/creation_time"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    // EXIF first, then the filename pattern table, then the literal
    // (D01-T028). A pattern is inference and never overrides EXIF.
    let camera = cameras::identify_camera(
        exif_camera(path).as_deref(),
        &path.file_name().unwrap_or_default().to_string_lossy(),
    )
    .camera;
    let duration_seconds = video_duration(&document, stream)
        .ok_or_else(|| anyhow!("video stream has no valid duration"))?;
    let rate = stream["r_frame_rate"]
        .as_str()
        .and_then(parse_rate)
        .filter(|rate| *rate > 0.0)
        .ok_or_else(|| anyhow!("video stream has no valid frame rate"))?;
    let metadata_rotation_degrees = stream_rotation(Some(stream));
    let width = stream["width"]
        .as_u64()
        .ok_or_else(|| anyhow!("video stream has no usable width"))? as u32;
    let height = stream["height"]
        .as_u64()
        .ok_or_else(|| anyhow!("video stream has no usable height"))? as u32;
    let codec = stream["codec_name"]
        .as_str()
        .filter(|codec| !codec.is_empty())
        .ok_or_else(|| anyhow!("video stream has no codec name"))?
        .to_string();
    Ok(MediaProbe {
        duration_seconds,
        frame_rate: rate,
        width,
        height,
        codec,
        camera,
        captured_at_raw: raw_time.clone(),
        timestamp_candidates,
        best_time: raw_time
            .as_deref()
            .and_then(|value| normalize_datetime(value, 0)),
        metadata_rotation_degrees,
        default_rotation_degrees: landscape_default_rotation(
            width,
            height,
            metadata_rotation_degrees,
        ),
    })
}

pub(crate) fn collect_timestamp_candidates(
    path: &Path,
    document: &Value,
) -> Vec<chronology::TimestampCandidate> {
    let mut candidates = Vec::new();
    let mut add = |origin: String, original_text: String, semantics: Option<String>| {
        let parsed = chrono::DateTime::parse_from_rfc3339(&original_text).ok();
        let timezone_claim = timestamp_timezone_claim(&original_text);
        let parse_status = if parsed.is_some() {
            "parsed"
        } else {
            "unparseable"
        };
        candidates.push(chronology::TimestampCandidate {
            origin,
            original_text,
            parsed_value: parsed.map(|value| value.to_rfc3339()),
            timezone_claim,
            parse_status: parse_status.into(),
            semantics,
        });
    };
    if let Some(tags) = document.pointer("/format/tags").and_then(Value::as_object) {
        for key in [
            "creation_time",
            "recording_start_time",
            "recording_end_time",
        ] {
            if let Some(value) = tags.get(key).and_then(Value::as_str) {
                add(
                    format!("ffprobe.format.tags.{key}"),
                    value.into(),
                    timestamp_tag_semantics(key, "container_creation_time"),
                );
            }
        }
    }
    if let Some(streams) = document.get("streams").and_then(Value::as_array) {
        for (index, stream) in streams.iter().enumerate() {
            if stream.get("codec_type").and_then(Value::as_str) == Some("video")
                && let Some(tags) = stream.pointer("/tags").and_then(Value::as_object)
            {
                for key in [
                    "creation_time",
                    "recording_start_time",
                    "recording_end_time",
                ] {
                    if let Some(value) = tags.get(key).and_then(Value::as_str) {
                        add(
                            format!("ffprobe.streams[{index}].tags.{key}"),
                            value.into(),
                            timestamp_tag_semantics(key, "stream_creation_time"),
                        );
                    }
                }
            }
        }
    }
    if let Some(filename) = path.file_name().and_then(|value| value.to_str())
        && let Some((start, len)) = find_compact_filename_timestamp(filename)
    {
        let value = &filename[start..start + len];
        let parsed = chrono::NaiveDateTime::parse_from_str(value, "%Y%m%d%H%M%S")
            .ok()
            .map(|date| date.format("%Y-%m-%dT%H:%M:%S").to_string());
        let parse_status = if parsed.is_some() {
            "parsed_without_timezone"
        } else {
            "unparseable"
        };
        candidates.push(chronology::TimestampCandidate {
            origin: "filename.compact_timestamp".into(),
            original_text: value.into(),
            parsed_value: parsed,
            timezone_claim: None,
            parse_status: parse_status.into(),
            semantics: None,
        });
    }
    candidates
}

pub(crate) fn timestamp_tag_semantics(tag: &str, creation_semantics: &str) -> Option<String> {
    match tag {
        // These are explicit semantic tag names. Generic creation_time tags do
        // not establish whether a recording started or ended at that instant.
        "recording_start_time" => Some("recording_start".into()),
        "recording_end_time" => Some("recording_end".into()),
        "creation_time" => Some(creation_semantics.into()),
        _ => None,
    }
}

pub(crate) fn timestamp_timezone_claim(raw: &str) -> Option<String> {
    if raw.ends_with('Z') || raw.ends_with('z') {
        return Some(raw[raw.len() - 1..].to_string());
    }
    let time_start = raw.find('T').or_else(|| raw.find(' '))? + 1;
    let suffix = &raw[time_start..];
    let sign_at = suffix.rfind(['+', '-'])?;
    let claim = &suffix[sign_at..];
    let digits = &claim[1..];
    let compact = digits.replace(':', "");
    let valid = (digits.len() == 5 && digits.as_bytes()[2] == b':' || digits.len() == 4)
        && compact.len() == 4
        && compact.bytes().all(|byte| byte.is_ascii_digit());
    valid.then(|| claim.to_string())
}

#[cfg(test)]
mod timestamp_candidate_tests {
    use super::*;

    #[cfg(unix)]
    fn probe_with_explicit_start_end_tags() -> (tempfile::TempDir, NormalizedMediaProbe) {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("20300102120415_synthetic.mp4");
        std::fs::write(&media, b"synthetic media placeholder").unwrap();
        let probe_json = dir.path().join("probe.json");
        std::fs::write(
            &probe_json,
            serde_json::to_vec(&serde_json::json!({
                "format": {"duration":"10", "tags": {
                    "creation_time": "2030-01-02T03:04:05Z",
                    "recording_start_time": "2030-01-02T03:04:05Z",
                    "recording_end_time": "2030-01-02T03:04:15Z"
                }},
                "streams": [{"codec_type":"video", "width":16, "height":16,
                    "codec_name":"synthetic", "r_frame_rate":"30/1", "duration":"10",
                    "tags":{"creation_time":"2030-01-02T03:04:05Z"}}]
            }))
            .unwrap(),
        )
        .unwrap();
        let ffprobe = dir.path().join("synthetic-ffprobe");
        std::fs::write(
            &ffprobe,
            format!(
                "#!/bin/sh\ncase \"$*\" in *recording_start_time*recording_end_time*) ;; *) exit 9 ;; esac\ncat '{}'\n",
                probe_json.display()
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&ffprobe).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&ffprobe, permissions).unwrap();
        let probe = probe_media_normalized(&media, &ffprobe, None).unwrap();
        (dir, probe)
    }

    #[test]
    fn probe_candidates_preserve_origin_timezone_and_unparseable_text() {
        let document = serde_json::json!({
            "format": {"tags": {"creation_time": "2030-01-02T03:04:05Z"}},
            "streams": [{"codec_type":"video", "tags":{"creation_time":"broken-camera-time"}}]
        });
        let found = collect_timestamp_candidates(Path::new("20300102030415_clip.mp4"), &document);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].origin, "ffprobe.format.tags.creation_time");
        assert_eq!(found[0].timezone_claim.as_deref(), Some("Z"));
        assert_eq!(found[0].parse_status, "parsed");
        assert_eq!(found[1].origin, "ffprobe.streams[0].tags.creation_time");
        assert_eq!(found[1].original_text, "broken-camera-time");
        assert_eq!(found[1].parse_status, "unparseable");
        assert_eq!(found[2].parse_status, "parsed_without_timezone");
        assert_eq!(found[2].timezone_claim, None);
        assert_eq!(found[2].semantics, None);
    }

    #[test]
    fn invalid_calendar_text_keeps_its_explicit_timezone_claim() {
        let found = collect_timestamp_candidates(
            Path::new("clip.mp4"),
            &serde_json::json!({"format":{"tags":{"creation_time":"2030-13-40T31:72:99-08:00"}},"streams":[]}),
        );
        assert_eq!(found[0].timezone_claim.as_deref(), Some("-08:00"));
        assert_eq!(found[0].parse_status, "unparseable");
        assert_eq!(found[0].parsed_value, None);
    }

    #[test]
    fn absent_timestamp_candidates_are_an_empty_new_probe_result() {
        let found = collect_timestamp_candidates(
            Path::new("generic_clip.mp4"),
            &serde_json::json!({"streams":[]}),
        );
        assert!(found.is_empty());
    }

    #[test]
    fn filename_local_convention_changes_do_not_rewrite_embedded_utc() {
        let document = serde_json::json!({
            "format": {"tags": {"creation_time": "2030-01-02T03:04:05Z"}},
            "streams": []
        });
        let before = collect_timestamp_candidates(Path::new("20300102030405_clip.mp4"), &document);
        let after = collect_timestamp_candidates(Path::new("20300102120405_clip.mp4"), &document);
        assert_eq!(before[0].original_text, after[0].original_text);
        assert_ne!(before[1].original_text, after[1].original_text);
        assert_eq!(after[1].timezone_claim, None);
    }

    #[cfg(unix)]
    #[test]
    fn explicit_start_end_probe_evidence_survives_operation_baseline_and_export() {
        let (_media_dir, probe) = probe_with_explicit_start_end_tags();
        let candidates = probe.timestamp_candidates;
        let start = candidates
            .iter()
            .find(|c| c.semantics.as_deref() == Some("recording_start"))
            .unwrap();
        let end = candidates
            .iter()
            .find(|c| c.semantics.as_deref() == Some("recording_end"))
            .unwrap();
        assert!(start.origin.ends_with("recording_start_time"));
        assert!(end.origin.ends_with("recording_end_time"));
        assert_eq!(probe.duration_seconds, 10.0);

        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("source.sqlite3")).unwrap();
        let project = catalog
            .create_project("Synthetic chronology fixture")
            .unwrap();
        catalog
            .add_portable_project_asset(
                &project.id,
                operations::ProjectAssetAddedPayload {
                    id: "asset_synthetic_evidence".into(),
                    fingerprint: "fingerprint_synthetic_evidence".into(),
                    filename: "20300102120415_synthetic.mp4".into(),
                    size_bytes: 1,
                    duration_seconds: 10.0,
                    frame_rate: 30.0,
                    width: 16,
                    height: 16,
                    codec: "synthetic".into(),
                    camera: "Synthetic".into(),
                    captured_at_raw: probe.captured_at_raw.clone(),
                    timestamp_candidates: Some(candidates.clone()),
                    best_time: probe.best_time.clone(),
                    metadata_rotation_degrees: 0,
                    default_rotation_degrees: 0,
                    created_at: "2030-01-02T00:00:00Z".into(),
                },
            )
            .unwrap();
        let first = catalog
            .chronology_evidence_page(&project.id, None, 0, 10)
            .unwrap();
        let comparison = first.assets[0]
            .candidate_comparisons
            .iter()
            .find(|row| {
                row.left_origin.ends_with("recording_start_time")
                    && row.right_origin.ends_with("recording_end_time")
            })
            .unwrap();
        assert_eq!(comparison.duration_delta_seconds, Some(0));
        assert_eq!(comparison.duration_explains_difference, Some(true));

        let snapshot = catalog.export_project_baseline(&project.id).unwrap();
        let restore_dir = tempfile::tempdir().unwrap();
        let restored = Catalog::import_project_baseline(
            restore_dir.path().join("restored.sqlite3"),
            &snapshot,
            "synthetic-node",
            1,
            "synthetic-digest",
        )
        .unwrap();
        let restored_page = restored
            .chronology_evidence_page(&project.id, None, 0, 10)
            .unwrap();
        assert_eq!(
            restored_page.assets[0].timestamp_candidates.as_deref(),
            Some(candidates.as_slice())
        );
        assert_eq!(
            restored_page.assets[0].candidate_comparisons,
            first.assets[0].candidate_comparisons
        );
    }
}

pub(crate) fn find_compact_filename_timestamp(filename: &str) -> Option<(usize, usize)> {
    let bytes = filename.as_bytes();
    (0..bytes.len().saturating_sub(13)).find_map(|start| {
        bytes[start..start + 14]
            .iter()
            .all(u8::is_ascii_digit)
            .then_some((start, 14))
    })
}

/// Normalized technical probe DTO shared between desktop and server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedMediaProbe {
    pub duration_seconds: f64,
    pub frame_rate: f64,
    pub encoded_width: u32,
    pub encoded_height: u32,
    pub codec: String,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub camera_string: Option<String>,
    pub captured_at_raw: Option<String>,
    #[serde(default)]
    pub timestamp_candidates: Vec<chronology::TimestampCandidate>,
    pub best_time: Option<String>,
    pub metadata_rotation_degrees: u16,
    pub enrichment_status: String,
    pub enrichment_warning: Option<String>,
}

pub(crate) fn require_video_stream(document: &Value) -> Result<&Value> {
    let stream = document["streams"]
        .as_array()
        .and_then(|streams| streams.iter().find(|stream| stream["codec_type"] == "video"))
        .ok_or_else(|| {
            anyhow!("ffprobe found no video stream; file bytes are reachable but contain no usable video")
        })?;
    if stream["width"].as_u64().is_none_or(|value| value == 0) {
        bail!("video stream has no usable width");
    }
    if stream["height"].as_u64().is_none_or(|value| value == 0) {
        bail!("video stream has no usable height");
    }
    if stream["codec_name"].as_str().is_none_or(str::is_empty) {
        bail!("video stream has no codec name");
    }
    if stream["r_frame_rate"]
        .as_str()
        .and_then(parse_rate)
        .is_none_or(|rate| !rate.is_finite() || rate <= 0.0)
    {
        bail!("video stream has no valid frame rate");
    }
    if video_duration(document, stream).is_none() {
        bail!("video stream has no valid duration");
    }
    Ok(stream)
}

pub(crate) fn video_duration(document: &Value, stream: &Value) -> Option<f64> {
    document["format"]["duration"]
        .as_str()
        .or_else(|| stream["duration"].as_str())
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|duration| duration.is_finite() && *duration > 0.0)
}

/// A media executable exited unsuccessfully. Kept typed so CLI adapters can
/// preserve the stable `media_tool_failed` code regardless of child wording.
#[derive(Debug, thiserror::Error)]
#[error("{tool} failed with {status}{diagnostic}")]
pub struct MediaToolFailure {
    pub(crate) tool: &'static str,
    pub(crate) status: String,
    pub(crate) diagnostic: String,
}

pub(crate) fn media_tool_failure(
    tool: &'static str,
    status: impl std::fmt::Display,
    diagnostic: String,
) -> anyhow::Error {
    MediaToolFailure {
        tool,
        status: status.to_string(),
        diagnostic,
    }
    .into()
}

/// Probes a media file with configured ffprobe and optional ExifTool,
/// returning a normalized DTO suitable for cross-node persistence.
pub fn probe_media_normalized(
    path: &Path,
    ffprobe_program: &Path,
    exiftool_program: Option<&Path>,
) -> Result<NormalizedMediaProbe> {
    let document = probe_media_document_with(path, ffprobe_program)?;
    let (camera_make, camera_model, enrichment_status, enrichment_warning) =
        if let Some(exif_tool) = exiftool_program {
            // First check if exiftool can run at all
            let can_run = std::process::Command::new(exif_tool)
                .arg("-ver")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !can_run {
                (
                    None,
                    None,
                    "unavailable".to_string(),
                    Some("ExifTool could not be executed".to_string()),
                )
            } else {
                match exif_camera_with(path, exif_tool) {
                    Some((make, model)) => (Some(make), Some(model), "ok".to_string(), None),
                    None => (
                        None,
                        None,
                        "ok".to_string(),
                        Some(
                            "ExifTool ran successfully but found no camera make/model metadata"
                                .to_string(),
                        ),
                    ),
                }
            }
        } else {
            (None, None, "unavailable".to_string(), None)
        };
    let camera_string = match (&camera_make, &camera_model) {
        (Some(make), Some(model)) => Some(format!("{make} {model}").trim().to_string()),
        (Some(make), None) => Some(make.clone()),
        _ => None,
    };
    let stream = require_video_stream(&document)?;
    let timestamp_candidates = collect_timestamp_candidates(path, &document);
    let raw_time = stream
        .pointer("/tags/creation_time")
        .or_else(|| document.pointer("/format/tags/creation_time"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let duration_seconds = video_duration(&document, stream)
        .ok_or_else(|| anyhow!("video stream has no valid duration"))?;
    let rate = stream["r_frame_rate"]
        .as_str()
        .and_then(parse_rate)
        .filter(|rate| *rate > 0.0)
        .ok_or_else(|| anyhow!("video stream has no valid frame rate"))?;
    let metadata_rotation_degrees = stream_rotation(Some(stream));
    let width = stream["width"]
        .as_u64()
        .ok_or_else(|| anyhow!("video stream has no usable width"))? as u32;
    let height = stream["height"]
        .as_u64()
        .ok_or_else(|| anyhow!("video stream has no usable height"))? as u32;
    let codec = stream["codec_name"]
        .as_str()
        .filter(|codec| !codec.is_empty())
        .ok_or_else(|| anyhow!("video stream has no codec name"))?
        .to_string();
    Ok(NormalizedMediaProbe {
        duration_seconds,
        frame_rate: rate,
        encoded_width: width,
        encoded_height: height,
        codec,
        camera_make,
        camera_model,
        camera_string,
        captured_at_raw: raw_time.clone(),
        timestamp_candidates,
        best_time: raw_time
            .as_deref()
            .and_then(|value| normalize_datetime(value, 0)),
        metadata_rotation_degrees,
        enrichment_status,
        enrichment_warning,
    })
}

/// Returns the raw ffprobe JSON document using a specific ffprobe executable.
pub(crate) fn probe_media_document_with(path: &Path, ffprobe_program: &Path) -> Result<Value> {
    let mut command = Command::new(ffprobe_program);
    command
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "format=duration:format_tags=creation_time,recording_start_time,recording_end_time:stream=codec_type,width,height,codec_name,r_frame_rate,duration:stream_tags=creation_time,recording_start_time,recording_end_time,rotate:stream_side_data=rotation",
            "-of",
            "json",
        ])
        .arg(path);
    let output = crate::stall::output_with_timeout(
        &mut command,
        &format!("ffprobe on {}", path.display()),
        crate::stall::io_stall_timeout(),
    )?;
    if !output.status.success() {
        let diagnostic =
            redact_media_diagnostic(&String::from_utf8_lossy(&output.stderr), path, path);
        return Err(media_tool_failure("ffprobe", output.status, diagnostic));
    }
    serde_json::from_slice(&output.stdout).context("parse ffprobe JSON")
}

/// Extracts camera make/model via a specific ExifTool executable.
pub(crate) fn exif_camera_with(path: &Path, exiftool_program: &Path) -> Option<(String, String)> {
    let mut command = Command::new(exiftool_program);
    command.args(["-json", "-Make", "-Model"]).arg(path);
    let output = crate::stall::output_with_timeout(
        &mut command,
        &format!("exiftool on {}", path.display()),
        crate::stall::io_stall_timeout(),
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let document: Value = serde_json::from_slice(&output.stdout).ok()?;
    let item = document.as_array()?.first()?;
    let make = item["Make"].as_str().unwrap_or("").trim();
    let model = item["Model"].as_str().unwrap_or("").trim();
    match (make.is_empty(), model.is_empty()) {
        (true, true) => None,
        _ => Some((make.to_string(), model.to_string())),
    }
}

/// Full-file BLAKE3 result with pre- and post-read file evidence.
#[derive(Debug, Clone)]
pub struct FullFileHashResult {
    pub hex_digest: String,
    pub byte_count: u64,
    pub pre_read_size: u64,
    pub pre_read_modified_ns: Option<i128>,
    pub post_read_size: u64,
    pub post_read_modified_ns: Option<i128>,
}

/// Streams a file through BLAKE3 using bounded memory (64 KiB buffer).
/// Returns `input_changed` when size or modification metadata differs
/// between the pre-read and post-read stat calls.
///
/// `check_cancellation` is called between read chunks to enable cooperative
/// cancellation without storing a concrete cancellation type in the core.
pub fn full_file_blake3(
    path: &Path,
    check_cancellation: &dyn Fn() -> bool,
) -> Result<FullFileHashResult> {
    let pre_meta = fs::metadata(path).context("stat file before hash")?;
    let pre_size = pre_meta.len();
    let pre_modified_ns = modified_nanos(&pre_meta);

    let mut file = File::open(path)?;
    let mut hasher = Hasher::new();
    let mut buffer = [0u8; 65536];
    let mut byte_count: u64 = 0;
    loop {
        if check_cancellation() {
            anyhow::bail!("full-file BLAKE3 cancelled");
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        byte_count += read as u64;
    }

    let post_meta = fs::metadata(path).context("stat file after hash")?;
    let post_size = post_meta.len();
    let post_modified_ns = modified_nanos(&post_meta);

    if pre_size != post_size || pre_modified_ns != post_modified_ns || byte_count != pre_size {
        anyhow::bail!("input_changed");
    }

    Ok(FullFileHashResult {
        hex_digest: hasher.finalize().to_hex().to_string(),
        byte_count,
        pre_read_size: pre_size,
        pre_read_modified_ns: pre_modified_ns,
        post_read_size: post_size,
        post_read_modified_ns: post_modified_ns,
    })
}

#[cfg(unix)]
pub(crate) fn modified_nanos(metadata: &std::fs::Metadata) -> Option<i128> {
    use std::os::unix::fs::MetadataExt;
    let secs = metadata.mtime() as i128;
    let nsecs = metadata.mtime_nsec() as i128;
    Some(secs * 1_000_000_000 + nsecs)
}

#[cfg(not(unix))]
pub(crate) fn modified_nanos(_metadata: &std::fs::Metadata) -> Option<i128> {
    None
}

/// Returns the raw ffprobe JSON document for a media file.
pub(crate) fn probe_media_document(path: &Path) -> Result<Value> {
    let mut command = tool_command("ffprobe");
    command
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "format=duration:format_tags=creation_time,recording_start_time,recording_end_time:stream=codec_type,width,height,codec_name,r_frame_rate,duration:stream_tags=creation_time,recording_start_time,recording_end_time,rotate:stream_side_data=rotation",
            "-of",
            "json",
        ])
        .arg(path);
    let output = crate::stall::output_with_timeout(
        &mut command,
        &format!("ffprobe on {}", path.display()),
        crate::stall::io_stall_timeout(),
    )?;
    if !output.status.success() {
        let diagnostic =
            redact_media_diagnostic(&String::from_utf8_lossy(&output.stderr), path, path);
        return Err(media_tool_failure("ffprobe", output.status, diagnostic));
    }
    serde_json::from_slice(&output.stdout).context("parse ffprobe JSON")
}

pub(crate) fn stream_rotation(stream: Option<&Value>) -> u16 {
    stream
        .and_then(|value| {
            value["side_data_list"]
                .as_array()
                .and_then(|items| items.iter().find_map(|item| item["rotation"].as_f64()))
                .or_else(|| {
                    value
                        .pointer("/tags/rotate")
                        .and_then(Value::as_str)?
                        .parse()
                        .ok()
                })
        })
        .map(normalize_rotation)
        .unwrap_or(0)
}

pub(crate) fn orientation_name(width: u32, height: u32) -> &'static str {
    match width.cmp(&height) {
        std::cmp::Ordering::Greater => "landscape",
        std::cmp::Ordering::Less => "portrait",
        std::cmp::Ordering::Equal => "square",
    }
}

/// Extracts camera make/model via ExifTool. Returns None on failure.
///
/// `pub(crate)` so import preflight can make one bounded representative read
/// and carry the raw evidence unchanged through sticky-camera lookup and
/// persistence — see `cameras::mapping::CameraEvidence::exif_hint`.
pub(crate) fn exif_camera(path: &Path) -> Option<String> {
    let mut command = tool_command("exiftool");
    command.args(["-json", "-Make", "-Model"]).arg(path);
    let output = crate::stall::output_with_timeout(
        &mut command,
        &format!("exiftool on {}", path.display()),
        crate::stall::io_stall_timeout(),
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let document: Value = serde_json::from_slice(&output.stdout).ok()?;
    let item = document.as_array()?.first()?;
    let make = item["Make"].as_str().unwrap_or("").trim();
    let model = item["Model"].as_str().unwrap_or("").trim();
    let combined = format!("{make} {model}").trim().to_string();
    (!combined.is_empty()).then_some(combined)
}

pub(crate) fn parse_rate(value: &str) -> Option<f64> {
    let (numerator, denominator) = value.split_once('/').unwrap_or((value, "1"));
    let numerator = numerator.parse::<f64>().ok()?;
    let denominator = denominator.parse::<f64>().ok()?;
    if denominator == 0.0 {
        return None;
    }
    let rate = numerator / denominator;
    (rate.is_finite() && rate > 0.0).then_some(rate)
}

pub(crate) fn normalize_datetime(value: &str, offset_seconds: i64) -> Option<String> {
    if let Ok(datetime) = DateTime::parse_from_rfc3339(value) {
        return Some(
            (datetime.with_timezone(&Utc) + Duration::seconds(offset_seconds))
                .to_rfc3339_opts(SecondsFormat::Millis, true),
        );
    }
    for format in ["%Y-%m-%d %H:%M:%S", "%Y:%m:%d %H:%M:%S"] {
        if let Ok(datetime) = NaiveDateTime::parse_from_str(value, format) {
            return Some(
                (datetime.and_utc() + Duration::seconds(offset_seconds))
                    .to_rfc3339_opts(SecondsFormat::Millis, true),
            );
        }
    }
    None
}
