//! Media boundary for source inspection. No synthetic sampling timestamps.
use super::inspection_model::*;
use crate::{
    calibration_tool_identity_for_path, full_file_blake3, resolve_tool_path, tool_command,
};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

/// Hashing an original can otherwise park the inspection thread indefinitely
/// when a mounted volume stops answering. Bound the whole read, and let the
/// worker observe cancellation between chunks. A blocked kernel read cannot
/// be interrupted safely, so it is abandoned after the deadline, like the
/// import-path stall guard.
pub(crate) fn fingerprint(
    path: &Path,
    timeout_seconds: u32,
    cancel: &dyn Fn() -> bool,
) -> Result<String> {
    fingerprint_with_limit(path, cancel, Duration::from_secs(timeout_seconds.into()))
}

fn fingerprint_with_limit(
    path: &Path,
    cancel: &dyn Fn() -> bool,
    limit: Duration,
) -> Result<String> {
    let path = path.to_owned();
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = full_file_blake3(&path, &|| worker_stop.load(Ordering::Relaxed))
            .map(|hash| hash.hex_digest);
        let _ = tx.send(result);
    });
    let started = std::time::Instant::now();
    loop {
        if cancel() {
            stop.store(true, Ordering::Relaxed);
            anyhow::bail!("source inspection cancelled while hashing selected representation");
        }
        if let Ok(result) = rx.try_recv() {
            return result;
        }
        if started.elapsed() >= limit {
            stop.store(true, Ordering::Relaxed);
            return Err(SourceInspectionError::Timeout.into());
        }
        std::thread::sleep(crate::stall::POLL_INTERVAL);
    }
}

pub(crate) struct Stream {
    pub numerator: i64,
    pub denominator: i64,
    pub origin: i64,
    pub duration: f64,
    pub audio_start_seconds: Option<f64>,
}

pub(crate) fn tool_identity() -> Value {
    serde_json::json!([
        calibration_tool_identity_for_path(
            "source_inspection_decode",
            "ffmpeg",
            &resolve_tool_path("ffmpeg")
        ),
        calibration_tool_identity_for_path(
            "source_inspection_probe",
            "ffprobe",
            &resolve_tool_path("ffprobe")
        ),
    ])
}

fn output(
    command: &mut Command,
    timeout_seconds: u32,
    output_limit: usize,
    cancel: &dyn Fn() -> bool,
) -> Result<std::process::Output> {
    let output = match crate::stall::output_with_timeout_cancellation_and_limit(
        command,
        "source inspection media",
        Duration::from_secs(timeout_seconds.into()),
        &|| cancel(),
        output_limit,
    ) {
        Ok(output) => output,
        Err(error) if error.to_string().contains("stopped answering after") => {
            return Err(SourceInspectionError::Timeout.into());
        }
        Err(error) if error.to_string().contains("output limit") => {
            return Err(SourceInspectionError::Insufficient.into());
        }
        Err(error) => return Err(error),
    };
    // Keep raw provider/media output local; errors never expose source paths.
    if !output.status.success() {
        return Err(crate::media_tool_failure(
            "source inspection media",
            output.status,
            "selected representation decode/probe failed".into(),
        ));
    }
    Ok(output)
}

pub(crate) fn probe(
    path: &Path,
    include_audio: bool,
    timeout_seconds: u32,
    cancel: &dyn Fn() -> bool,
) -> Result<Stream> {
    let mut command = tool_command("ffprobe");
    command
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,time_base,start_pts,duration:format=duration,start_time",
            "-of",
            "json",
        ])
        .arg(path);
    let document: Value = serde_json::from_slice(
        &output(&mut command, timeout_seconds, 1024 * 1024, cancel)?.stdout,
    )?;
    let streams = document["streams"]
        .as_array()
        .context("selected representation has no streams")?;
    let video = streams
        .iter()
        .find(|s| s["codec_type"] == "video")
        .context("selected representation has no video")?;
    let (n, d) = video["time_base"]
        .as_str()
        .and_then(|s| s.split_once('/'))
        .context("video has no rational time base")?;
    let numerator: i64 = n.parse()?;
    let denominator: i64 = d.parse()?;
    ensure!(numerator > 0 && denominator > 0, "invalid video time base");
    let start = document["format"]["start_time"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    let origin = start * denominator as f64 / numerator as f64;
    ensure!(
        origin.is_finite() && (origin - origin.round()).abs() <= 0.01 && origin.abs() < 9e15,
        "unsupported source clock: format origin is not representable in video time base"
    );
    ensure!(
        video["start_pts"].as_i64() == Some(origin.round() as i64),
        "unsupported source clock: video and container origins differ; explicit timeline mapping required"
    );
    let audio_start_seconds = streams
        .iter()
        .find(|s| include_audio && s["codec_type"] == "audio")
        .map(|audio| -> Result<f64> {
            let pts = audio["start_pts"]
                .as_i64()
                .context("unsupported audio clock: no stream origin")?;
            let (n, d) = audio["time_base"]
                .as_str()
                .and_then(|s| s.split_once('/'))
                .context("unsupported audio time base")?;
            let n: f64 = n.parse()?;
            let d: f64 = d.parse()?;
            ensure!(
                n > 0.0 && d > 0.0 && n.is_finite() && d.is_finite(),
                "invalid audio time base"
            );
            let offset = pts as f64 * n / d - start;
            ensure!(offset.is_finite(), "invalid audio stream origin");
            Ok(offset)
        })
        .transpose()?;
    let duration = video["duration"]
        .as_str()
        .or_else(|| document["format"]["duration"].as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .context("no video duration")?;
    ensure!(
        duration.is_finite() && duration > 0.0 && duration <= 86400.0,
        "unsupported source duration"
    );
    Ok(Stream {
        numerator,
        denominator,
        origin: origin.round() as i64,
        duration,
        audio_start_seconds,
    })
}

pub(crate) fn frame_pts(
    path: &Path,
    mapping: &InspectionTimeMapping,
    start: f64,
    end: f64,
    timeout_seconds: u32,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<i64>> {
    let origin = mapping.representation_origin_pts as f64 * mapping.time_base_numerator as f64
        / mapping.time_base_denominator as f64;
    let mut command = tool_command("ffprobe");
    command
        .args(["-v", "error", "-select_streams", "v:0", "-read_intervals"])
        .arg(format!("{}%{}", (start + origin).max(0.0), end + origin))
        .args([
            "-show_frames",
            "-show_entries",
            "frame=best_effort_timestamp",
            "-of",
            "json",
        ])
        .arg(path);
    let bytes = output(&mut command, timeout_seconds, 16 * 1024 * 1024, cancel)?.stdout;
    ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "source frame metadata exceeds inspection bound"
    );
    let document: Value = serde_json::from_slice(&bytes)?;
    let frames = document["frames"]
        .as_array()
        .context("no frame timestamp evidence")?;
    ensure!(
        frames.len() <= 20000,
        "representation frame density exceeds inspection bound"
    );
    let mut pts: Vec<_> = frames
        .iter()
        .filter_map(|f| f["best_effort_timestamp"].as_i64())
        .filter(|p| p.unsigned_abs() < 9_000_000_000_000_000)
        .filter(|p| {
            let t = (*p - mapping.representation_origin_pts) as f64
                * mapping.time_base_numerator as f64
                / mapping.time_base_denominator as f64;
            t + 1e-9 >= start && t < end - 1e-9
        })
        .collect();
    pts.sort_unstable();
    pts.dedup();
    ensure!(
        !pts.is_empty(),
        "no timestamped frames available inside requested window"
    );
    Ok(pts)
}

pub(super) struct FrameDecodeConfig<'a> {
    pub rotation: u16,
    pub width: u32,
    pub start_seconds: f64,
    pub pts: &'a [i64],
    pub timeout_seconds: u32,
}

pub(super) fn encode_frames(
    path: &Path,
    workspace: &Path,
    config: FrameDecodeConfig<'_>,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<InspectionFile>> {
    let FrameDecodeConfig {
        rotation,
        width,
        start_seconds,
        pts,
        timeout_seconds,
    } = config;
    let turn = match rotation {
        0 => "",
        90 => "transpose=clock,",
        180 => "hflip,vflip,",
        270 => "transpose=cclock,",
        _ => anyhow::bail!("unsupported effective rotation"),
    };
    let select = pts
        .iter()
        .map(|p| format!("eq(pts\\,{p})"))
        .collect::<Vec<_>>()
        .join("+");
    let filter = format!(
        "select={select},{turn}scale='min({width},iw)':'min({width},ih)':force_original_aspect_ratio=decrease"
    );
    let seek = (start_seconds - 1.0).max(0.0).floor();
    let mut command = tool_command("ffmpeg");
    command
        .args(["-y", "-v", "error", "-copyts", "-ss"])
        .arg(seek.to_string())
        .args(["-noautorotate", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-an", "-sn", "-dn", "-vf"])
        .arg(filter)
        .args(["-frames:v"])
        .arg(pts.len().to_string())
        .args([
            "-fps_mode",
            "passthrough",
            "-q:v",
            "3",
            "-start_number",
            "0",
        ])
        .arg(workspace.join("frame-%04d.jpg"));
    output(&mut command, timeout_seconds, 1024 * 1024, cancel)?;
    let mut files = Vec::new();
    for i in 0..pts.len() {
        files.push(file(&workspace.join(format!("frame-{i:04}.jpg")), cancel)?);
    }
    ensure!(
        files.iter().map(|f| f.byte_length).sum::<u64>() <= 128 * 1024 * 1024,
        "inspection images exceed size bound"
    );
    Ok(files)
}

pub(crate) fn audio_context(
    path: &Path,
    workspace: &Path,
    audio_start: Option<f64>,
    start: f64,
    end: f64,
    timeout_seconds: u32,
    cancel: &dyn Fn() -> bool,
) -> Result<InspectedAudio> {
    let first = (start * 48000.0).ceil() as u64;
    let last = (end * 48000.0).floor() as u64;
    let mut audio = InspectedAudio {
        state: if let Some(audio_start) = audio_start {
            if audio_start > start {
                "completeWithLeadingGap"
            } else {
                "complete"
            }
        } else {
            "noAudioStream"
        }
        .into(),
        playable: None,
        sample_rate: 48000,
        channels: 2,
        source_in_samples: first,
        source_out_samples: last,
        stream_source_in_seconds: audio_start,
        leading_gap_seconds: audio_start.map_or(0.0, |s| (s - start).max(0.0).min(end - start)),
        waveform: Vec::new(),
    };
    if audio_start.is_none() {
        return Ok(audio);
    }
    ensure!(
        last > first,
        "inspection audio range has no complete samples"
    );
    let seek = (first / 48000).saturating_sub(1);
    let trim = first - seek * 48000;
    let raw = workspace.join("audio.s16le");
    let mut command = tool_command("ffmpeg");
    command.args(["-y","-v","error","-ss"]).arg(seek.to_string()).arg("-i").arg(path)
        .args(["-map","0:a:0","-vn","-sn","-dn","-af"])
        .arg(format!("aresample=48000:async=1:first_pts=0,atrim=start_sample={trim}:end_sample={},asetpts=PTS-STARTPTS",trim+last-first))
        .args(["-ar","48000","-ac","2","-c:a","pcm_s16le","-f","s16le"]).arg(&raw);
    output(&mut command, timeout_seconds, 1024 * 1024, cancel)?;
    ensure!(
        fs::metadata(&raw)?.len() == (last - first) * 4,
        "selected audio did not cover all requested samples; narrow the window or inspect original explicitly"
    );
    let bytes = fs::read(&raw)?;
    // 20 ms bins, source clock retained through the inward sample rounding.
    for (index, bin) in bytes.chunks(960 * 4).enumerate() {
        ensure!(!cancel(), "source inspection cancelled during waveform");
        let mut peak: f64 = 0.0;
        let mut sum = 0.0;
        for sample in bin.chunks_exact(2) {
            let v = f64::from(i16::from_le_bytes([sample[0], sample[1]])) / 32768.0;
            peak = peak.max(v.abs());
            sum += v * v;
        }
        let rms = (sum / (bin.len() / 2) as f64).sqrt();
        audio.waveform.push(InspectionWaveformBin {
            excerpt_start_seconds: first as f64 / 48000.0 - start + index as f64 * 0.02,
            excerpt_end_seconds: first as f64 / 48000.0 - start
                + (index * 960 + bin.len() / 4) as f64 / 48000.0,
            peak,
            rms,
            low_energy: rms < 0.0031622776601683794,
        });
    }
    let wav = workspace.join("audio.wav");
    let mut command = tool_command("ffmpeg");
    command
        .args([
            "-y", "-v", "error", "-f", "s16le", "-ar", "48000", "-ac", "2", "-i",
        ])
        .arg(raw)
        .args(["-c:a", "copy"])
        .arg(&wav);
    output(&mut command, timeout_seconds, 1024 * 1024, cancel)?;
    audio.playable = Some(file(&wav, cancel)?);
    Ok(audio)
}

fn file(path: &Path, cancel: &dyn Fn() -> bool) -> Result<InspectionFile> {
    let hash = full_file_blake3(path, cancel)?;
    ensure!(hash.byte_count > 0, "empty inspection artifact");
    Ok(InspectionFile {
        path: path.to_string_lossy().into_owned(),
        blake3: hash.hex_digest,
        byte_length: hash.byte_count,
    })
}

pub(super) fn files_current(
    report: &SourceInspection,
    root: &Path,
    cancel: &dyn Fn() -> bool,
) -> Result<bool> {
    let mut files: Vec<_> = report.frames.iter().map(|f| &f.image).collect();
    if let Some(audio) = &report.audio
        && let Some(playable) = &audio.playable
    {
        files.push(playable);
    }
    let root = fs::canonicalize(root)?;
    for expected in files {
        ensure!(
            !cancel(),
            "source inspection cancelled during evidence verification"
        );
        let path = PathBuf::from(&expected.path);
        let Ok(path) = fs::canonicalize(path) else {
            return Ok(false);
        };
        if !path.starts_with(&root) {
            return Ok(false);
        }
        let Ok(hash) = full_file_blake3(&path, cancel) else {
            ensure!(!cancel(), "source inspection cancelled");
            return Ok(false);
        };
        if hash.hex_digest != expected.blake3 || hash.byte_count != expected.byte_length {
            return Ok(false);
        }
    }
    Ok(!report.frames.is_empty())
}

#[cfg(test)]
mod bounded_media_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[cfg(unix)]
    #[test]
    fn blocked_original_reader_times_out_and_media_process_is_cancellable() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("blocked-original");
        assert!(
            Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let started = std::time::Instant::now();
        let error = fingerprint_with_limit(&fifo, &|| false, Duration::from_millis(150))
            .unwrap_err()
            .to_string();
        assert!(error.contains("source inspection timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));
        // Let the abandonable open/read finish after proving the caller has
        // returned, so the fixture itself does not strand a worker thread.
        drop(std::fs::OpenOptions::new().write(true).open(&fifo).unwrap());

        let cancelled = AtomicBool::new(false);
        let timer = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(150));
                cancelled.store(true, Ordering::Relaxed);
            });
            let mut child = Command::new("sleep");
            child.arg("30");
            output(&mut child, 30, 1024, &|| cancelled.load(Ordering::Relaxed))
                .unwrap_err()
                .to_string()
        });
        assert!(timer.contains("canceled"), "{timer}");
    }
}
