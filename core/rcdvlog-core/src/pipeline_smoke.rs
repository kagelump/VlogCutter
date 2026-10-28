//! Explicit, generated-media proof of the selected FFmpeg preview boundary.
//!
//! Status is a read-only lookup. Running is explicit and bounded; originals
//! are generated in a managed temporary workspace and removed after rendering.

use crate::{
    CalibrationToolIdentity, Catalog, CliExecutable, EditPlanOperation, PreviewRequest,
    calibration_tool_identity_for_path, resolve_tool_path,
};
use anyhow::{Context, Result, anyhow, ensure};
use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::process::Command;
use uuid::Uuid;

pub const PIPELINE_SMOKE_RECIPE_VERSION: &str = "preview-generated-media-smoke.v1";
const MAX_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;
const FIXTURE_SECONDS: f64 = 0.8;
const REQUIRED_CHECKS: [&str; 5] = [
    "generated-fixture-diversity",
    "decoded-duration",
    "decoded-av-alignment",
    "tone-versus-no-audio-and-silence",
    "effective-rotation",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PipelineSmokeIdentity {
    pub recipe_version: String,
    pub executable_build: crate::CliBuild,
    pub executable_digest: String,
    pub ffmpeg: CalibrationToolIdentity,
    pub ffprobe: CalibrationToolIdentity,
}

impl PipelineSmokeIdentity {
    pub fn from_executable(executable: &CliExecutable) -> Result<Self> {
        let metadata = fs::metadata(&executable.path).with_context(|| {
            format!("inspect selected executable {}", executable.path.display())
        })?;
        ensure!(
            metadata.is_file(),
            "selected executable is not a regular file"
        );
        ensure!(
            metadata.len() <= MAX_EXECUTABLE_BYTES,
            "selected executable exceeds the 512 MiB identity bound"
        );
        let mut file =
            File::open(&executable.path).context("open selected executable for identity")?;
        let mut hasher = blake3::Hasher::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).context("hash selected executable")?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let ffmpeg_path = resolve_tool_path("ffmpeg");
        let ffprobe_path = resolve_tool_path("ffprobe");
        Ok(Self {
            recipe_version: PIPELINE_SMOKE_RECIPE_VERSION.into(),
            executable_build: executable.build.clone(),
            executable_digest: hasher.finalize().to_hex().to_string(),
            ffmpeg: calibration_tool_identity_for_path(
                "pipeline_transcode",
                "ffmpeg",
                &ffmpeg_path,
            ),
            ffprobe: calibration_tool_identity_for_path("pipeline_probe", "ffprobe", &ffprobe_path),
        })
    }

    fn cache_key(&self) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(self)?)
            .to_hex()
            .to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PipelineSmokeCheck {
    pub name: String,
    pub passed: bool,
    pub measured: String,
    pub expectation: String,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PipelineSmokeReport {
    pub schema_version: String,
    pub state: String,
    pub cache_key: Option<String>,
    pub identity: PipelineSmokeIdentity,
    pub checks: Vec<PipelineSmokeCheck>,
    pub limitations: Vec<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub reused: bool,
    pub remediation: Option<String>,
}

impl PipelineSmokeReport {
    pub fn unverified(identity: PipelineSmokeIdentity) -> Self {
        Self::unverified_with_reason(
            identity,
            "No completed receipt matches the current executable, tool, and recipe identity.",
        )
    }

    fn unverified_with_reason(identity: PipelineSmokeIdentity, reason: &str) -> Self {
        Self {
            schema_version: "pipeline-smoke-receipt.v1".into(),
            state: "unverified".into(),
            cache_key: None,
            identity,
            checks: Vec::new(),
            limitations: limitations(),
            started_at: None,
            completed_at: None,
            reused: false,
            remediation: Some(format!(
                "{reason} Run `rcdvlog calibrate run --recipe pipeline-smoke` explicitly to prove this executable and media-toolchain identity."
            )),
        }
    }
}

fn limitations() -> Vec<String> {
    vec![
        "Generated local preview fixtures do not establish native editor, Resolve, or NLE export compatibility.".into(),
        "The check covers the configured ffmpeg/ffprobe executables on this host and does not inspect project footage.".into(),
    ]
}

impl Catalog {
    /// Current identity-qualified receipt, without media work or writes.
    pub fn pipeline_smoke_status(
        &self,
        identity: PipelineSmokeIdentity,
    ) -> Result<PipelineSmokeReport> {
        let key = identity.cache_key()?;
        let connection = self.connection()?;
        let table_exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='pipeline_smoke_receipts')",
            [], |row| row.get(0),
        )?;
        if !table_exists {
            return Ok(PipelineSmokeReport::unverified(identity));
        }
        let row: Option<(String, String, String)> = connection
            .query_row(
                "SELECT identity_json,report_json,state FROM pipeline_smoke_receipts WHERE cache_key=?1",
                [&key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((identity_json, report_json, stored_state)) = row {
            let stored_identity = serde_json::from_str::<PipelineSmokeIdentity>(&identity_json);
            let decoded_report = serde_json::from_str::<PipelineSmokeReport>(&report_json);
            if let (Ok(stored_identity), Ok(mut report)) = (stored_identity, decoded_report) {
                let mut names: Vec<&str> = report
                    .checks
                    .iter()
                    .map(|check| check.name.as_str())
                    .collect();
                names.sort_unstable();
                let duplicate_checks = names.windows(2).any(|window| window[0] == window[1]);
                let checks_consistent = match report.state.as_str() {
                    "passed" => {
                        REQUIRED_CHECKS.iter().all(|required| {
                            report
                                .checks
                                .iter()
                                .any(|check| check.name == *required && check.passed)
                        }) && report.checks.iter().all(|check| check.passed)
                    }
                    "failed" => report.checks.iter().any(|check| !check.passed),
                    _ => false,
                } && report.checks.iter().all(|check| {
                    !check.name.trim().is_empty()
                        && !check.measured.trim().is_empty()
                        && !check.expectation.trim().is_empty()
                        && (check.passed == check.failure.is_none())
                });
                let valid = stored_identity == identity
                    && report.schema_version == "pipeline-smoke-receipt.v1"
                    && report.cache_key.as_deref() == Some(key.as_str())
                    && report.identity == identity
                    && report.state == stored_state
                    && !duplicate_checks
                    && checks_consistent;
                if valid {
                    report.reused = true;
                    return Ok(report);
                }
            }
            return Ok(PipelineSmokeReport::unverified_with_reason(
                identity,
                "The cached receipt identity or contents are inconsistent, so readiness is not verified.",
            ));
        }
        Ok(PipelineSmokeReport::unverified(identity))
    }

    /// Explicit smoke run. Exact completed results are reused unless forced.
    pub fn run_pipeline_smoke(
        &self,
        identity: PipelineSmokeIdentity,
        force: bool,
    ) -> Result<PipelineSmokeReport> {
        self.run_pipeline_smoke_using(identity, force, run_generated_preview)
    }

    fn run_pipeline_smoke_using<F>(
        &self,
        identity: PipelineSmokeIdentity,
        force: bool,
        run: F,
    ) -> Result<PipelineSmokeReport>
    where
        F: FnOnce(&Path, &crate::LocalHeavyLease) -> Result<Vec<PipelineSmokeCheck>>,
    {
        ensure!(
            !self.read_only,
            "pipeline smoke requires an explicitly writable catalog"
        );
        let key = identity.cache_key()?;
        if !force {
            let current = self.pipeline_smoke_status(identity.clone())?;
            if current.state == "passed" || current.state == "failed" {
                return Ok(current);
            }
        }
        let heavy = self.acquire_local_heavy()?;
        let started_at = Utc::now().to_rfc3339();
        let root = self
            .database_path()
            .parent()
            .ok_or_else(|| anyhow!("catalog has no parent directory"))?
            .join("cache")
            .join("pipeline-smoke")
            .join(Uuid::new_v4().simple().to_string());
        fs::create_dir_all(&root).context("create managed pipeline-smoke workspace")?;
        let report = match run(&root, &heavy) {
            Ok(checks) => {
                let passed = checks.iter().all(|check| check.passed);
                PipelineSmokeReport {
                    schema_version: "pipeline-smoke-receipt.v1".into(),
                    state: if passed { "passed" } else { "failed" }.into(),
                    cache_key: Some(key.clone()),
                    identity,
                    checks,
                    limitations: limitations(),
                    started_at: Some(started_at),
                    completed_at: Some(Utc::now().to_rfc3339()),
                    reused: false,
                    remediation: (!passed).then(|| "Inspect the failed decoded checks; fix the selected media toolchain or preview renderer, then rerun with --force.".into()),
                }
            }
            Err(error) => PipelineSmokeReport {
                schema_version: "pipeline-smoke-receipt.v1".into(),
                state: "failed".into(),
                cache_key: Some(key.clone()),
                identity,
                checks: vec![PipelineSmokeCheck {
                    name: "generated-preview-pipeline".into(),
                    passed: false,
                    measured: "pipeline could not complete".into(),
                    expectation: "renderer produces decodable mixed-media output".into(),
                    failure: Some(crate::redact_media_diagnostic(
                        &format!("{error:#}"),
                        &root,
                        &root,
                    )),
                }],
                limitations: limitations(),
                started_at: Some(started_at),
                completed_at: Some(Utc::now().to_rfc3339()),
                reused: false,
                remediation: Some(
                    "Check the reported FFmpeg/FFprobe failure and rerun the explicit smoke check."
                        .into(),
                ),
            },
        };
        let _ = fs::remove_dir_all(&root);
        let body = serde_json::to_string(&report)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO pipeline_smoke_receipts(cache_key,identity_json,report_json,state,created_at,completed_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(cache_key) DO UPDATE SET identity_json=excluded.identity_json,report_json=excluded.report_json,state=excluded.state,completed_at=excluded.completed_at",
            params![key, serde_json::to_string(&report.identity)?, body, report.state, report.started_at, report.completed_at],
        )?;
        transaction.commit()?;
        Ok(report)
    }
}

fn run_generated_preview(
    root: &Path,
    heavy: &crate::LocalHeavyLease,
) -> Result<Vec<PipelineSmokeCheck>> {
    run_generated_preview_with_fault(root, None, heavy)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SmokeFault {
    ShortDuration,
    ShiftedAudio,
    WrongTone,
    AudibleSilence,
    WrongRotation,
}

fn run_generated_preview_with_fault(
    root: &Path,
    fault: Option<SmokeFault>,
    heavy: &crate::LocalHeavyLease,
) -> Result<Vec<PipelineSmokeCheck>> {
    let media = root.join("fixtures");
    fs::create_dir_all(&media)?;
    generate_fixture(
        &media.join("01-tone.mp4"),
        Fixture::Tone {
            frequency: if fault == Some(SmokeFault::WrongTone) {
                880
            } else {
                440
            },
            delay_ms: if fault == Some(SmokeFault::ShiftedAudio) {
                400
            } else {
                200
            },
        },
    )?;
    generate_fixture(&media.join("02-no-audio.mp4"), Fixture::NoAudio)?;
    generate_fixture(
        &media.join("03-silent.mp4"),
        Fixture::Silent {
            audible: fault == Some(SmokeFault::AudibleSilence),
        },
    )?;
    generate_fixture(&media.join("04-rotated.mp4"), Fixture::Rotated)?;
    let temp_catalog = Catalog::open(root.join("synthetic.sqlite"))?;
    let project = temp_catalog.create_project("Generated pipeline smoke")?;
    temp_catalog.ingest_folder(&project.id, &media)?;
    let mut assets = temp_catalog.list_assets(&project.id)?;
    assets.sort_by(|left, right| left.filename.cmp(&right.filename));
    ensure!(
        assets.len() == 4,
        "expected four generated fixtures, found {}",
        assets.len()
    );
    let rotated = assets
        .iter()
        .find(|asset| asset.filename == "04-rotated.mp4")
        .context("generated asymmetric fixture is missing")?;
    temp_catalog
        .set_rotation(
            &rotated.id,
            if fault == Some(SmokeFault::WrongRotation) {
                270
            } else {
                90
            },
        )
        .context("set effective rotation through catalog orientation owner")?;
    assets = temp_catalog.list_assets(&project.id)?;
    assets.sort_by(|left, right| left.filename.cmp(&right.filename));
    let operations: Vec<_> = assets
        .iter()
        .enumerate()
        .map(|(index, asset)| EditPlanOperation {
            operation_id: String::new(),
            position: index as u32,
            asset_id: asset.id.clone(),
            source_in_seconds: 0.0,
            source_out_seconds: if fault == Some(SmokeFault::ShortDuration) && index == 3 {
                FIXTURE_SECONDS / 2.0
            } else {
                FIXTURE_SECONDS
            },
            rationale: "generated pipeline smoke".into(),
            audio: None,
        })
        .collect();
    let plan = temp_catalog.create_edit_plan_revision(&project.id, &operations, None, None)?;
    let plan = temp_catalog.activate_edit_plan_revision(&plan.revision_id, None)?;
    let mut progress = |_| {};
    let preview = temp_catalog.render_preview_with_heavy_lease(
        &PreviewRequest {
            revision_id: plan.revision_id,
            max_height: 180,
        },
        &mut progress,
        &|| false,
        heavy,
    )?;
    ensure!(
        preview.failures.is_empty(),
        "preview dropped {} fixture(s)",
        preview.failures.len()
    );
    let output = Path::new(&preview.output_path);
    ensure!(
        fs::metadata(output)?.len() <= 64 * 1024 * 1024,
        "generated preview exceeded 64 MiB bound"
    );
    let mut checks = measure_preview(output)?;
    checks.push(verify_fixture_diversity(&media, &assets)?);
    Ok(checks)
}

#[derive(Clone, Copy)]
enum Fixture {
    Tone { frequency: u32, delay_ms: u32 },
    NoAudio,
    Silent { audible: bool },
    Rotated,
}

fn generate_fixture(path: &Path, fixture: Fixture) -> Result<()> {
    let ffmpeg = resolve_tool_path("ffmpeg");
    let mut command = Command::new(ffmpeg);
    command.args(["-y", "-hide_banner", "-loglevel", "error"]);
    match fixture {
        Fixture::Tone {
            frequency,
            delay_ms,
        } => {
            command
                .args([
                    "-f",
                    "lavfi",
                    "-i",
                    "color=black:size=160x90:rate=24",
                    "-f",
                    "lavfi",
                    "-i",
                ])
                .arg(format!(
                    "sine=frequency={frequency}:sample_rate=44100:duration=0.8"
                ))
                .args([
                    "-vf",
                    "drawbox=x=0:y=0:w=80:h=45:color=white:t=fill:enable='between(t,0.2,0.8)'",
                ])
                .args(["-af"])
                .arg(format!("adelay={delay_ms}|{delay_ms}"))
                .args([
                    "-t", "0.8", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-ac",
                    "2", "-ar", "44100",
                ]);
        }
        Fixture::NoAudio => {
            command.args([
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=128x96:rate=15",
                "-t",
                "0.8",
                "-an",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ]);
        }
        Fixture::Silent { audible } => {
            command
                .args([
                    "-f",
                    "lavfi",
                    "-i",
                    "color=black:size=160x90:rate=30",
                    "-f",
                    "lavfi",
                    "-i",
                ])
                .arg(if audible {
                    "sine=frequency=440:sample_rate=32000:duration=0.8"
                } else {
                    "anullsrc=channel_layout=mono:sample_rate=32000"
                })
                .args([
                    "-t", "0.8", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-ac",
                    "1", "-ar", "32000",
                ]);
        }
        Fixture::Rotated => {
            command.args(["-f", "lavfi", "-i", "color=red:size=160x90:rate=20", "-vf", "drawbox=x=80:y=0:w=80:h=45:color=blue:t=fill,drawbox=x=0:y=45:w=80:h=45:color=green:t=fill,drawbox=x=80:y=45:w=80:h=45:color=yellow:t=fill", "-t", "0.8", "-metadata:s:v:0", "rotate=90", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-an"]);
        }
    }
    command.arg(path);
    let output = crate::stall::output_with_timeout(
        &mut command,
        "generated pipeline-smoke fixture",
        std::time::Duration::from_secs(20),
    )
    .context("run bounded generated FFmpeg fixture command")?;
    ensure!(
        output.status.success(),
        "fixture generation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(
        fs::metadata(path)?.len() <= 8 * 1024 * 1024,
        "generated fixture exceeded 8 MiB bound"
    );
    Ok(())
}

fn measure_preview(path: &Path) -> Result<Vec<PipelineSmokeCheck>> {
    let ffprobe = resolve_tool_path("ffprobe");
    let duration = command_text(
        &ffprobe,
        &[
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nw=1:nk=1",
            path.to_str()
                .ok_or_else(|| anyhow!("non-UTF8 generated preview path"))?,
        ],
    )?
    .trim()
    .parse::<f64>()?;
    let mut frame_command = Command::new(resolve_tool_path("ffmpeg"));
    frame_command
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-vsync", "0", "-f", "framemd5", "-"]);
    let frame_output = crate::stall::output_with_timeout(
        &mut frame_command,
        "decode generated preview frames",
        std::time::Duration::from_secs(20),
    )?;
    ensure!(
        frame_output.status.success(),
        "could not decode preview video frames"
    );
    let frame_count = String::from_utf8_lossy(&frame_output.stdout)
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .count();
    let decoded_video_seconds = frame_count as f64 * 1001.0 / 30_000.0;
    let mut pcm_command = Command::new(resolve_tool_path("ffmpeg"));
    pcm_command.args(["-v", "error", "-i"]).arg(path).args([
        "-map", "0:a:0", "-f", "f32le", "-ac", "1", "-ar", "8000", "-",
    ]);
    let pcm = crate::stall::output_with_timeout(
        &mut pcm_command,
        "decode generated preview PCM",
        std::time::Duration::from_secs(20),
    )?;
    ensure!(pcm.status.success(), "could not decode preview audio PCM");
    ensure!(
        pcm.stdout.len() % 4 == 0,
        "decoded PCM byte count is not sample aligned"
    );
    let samples: Vec<f32> = pcm
        .stdout
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect();
    let decoded_audio_seconds = samples.len() as f64 / 8000.0;
    let rms = |from: f64, to: f64| -> f64 {
        let start = (from * 8000.0) as usize;
        let end = (to * 8000.0) as usize;
        if start >= samples.len() || start >= end {
            return 0.0;
        }
        (samples[start..end.min(samples.len())]
            .iter()
            .map(|sample| f64::from(*sample) * f64::from(*sample))
            .sum::<f64>()
            / (end.min(samples.len()) - start) as f64)
            .sqrt()
    };
    let tone_rms = rms(0.12, 0.62);
    let no_audio_rms = rms(0.92, 1.38);
    let silent_rms = rms(1.72, 2.28);
    let tone_samples = samples
        .get((0.3 * 8000.0) as usize..(0.7 * 8000.0) as usize)
        .unwrap_or_default();
    let (mut in_phase, mut quadrature) = (0.0_f64, 0.0_f64);
    for (offset, sample) in tone_samples.iter().enumerate() {
        let phase = std::f64::consts::TAU * 440.0 * offset as f64 / 8000.0;
        in_phase += f64::from(*sample) * phase.cos();
        quadrature += f64::from(*sample) * phase.sin();
    }
    let tone_440_amplitude = 2.0 * in_phase.hypot(quadrature) / tone_samples.len() as f64;
    let audio_onset_seconds = samples
        .chunks_exact(160)
        .position(|window| {
            let window_rms = (window
                .iter()
                .map(|sample| f64::from(*sample).powi(2))
                .sum::<f64>()
                / window.len() as f64)
                .sqrt();
            window_rms > 0.015
        })
        .map(|window| window as f64 * 0.02);
    let mut marker_command = Command::new(resolve_tool_path("ffmpeg"));
    marker_command.args(["-v", "error", "-i"]).arg(path).args([
        "-vf",
        "crop=16:16:32:32,scale=1:1,format=gray",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "gray",
        "-",
    ]);
    let marker = crate::stall::output_with_timeout(
        &mut marker_command,
        "decode generated alignment marker frames",
        std::time::Duration::from_secs(20),
    )?;
    ensure!(
        marker.status.success(),
        "could not decode alignment marker frames"
    );
    let video_onset_seconds = marker
        .stdout
        .iter()
        .position(|level| *level > 180)
        .map(|frame| frame as f64 * 1001.0 / 30_000.0);
    let mut orientation_command = Command::new(resolve_tool_path("ffmpeg"));
    orientation_command
        .args(["-v", "error", "-ss", "2.6", "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            "format=rgb24",
            "-f",
            "rawvideo",
            "-",
        ]);
    let frame = crate::stall::output_with_timeout(
        &mut orientation_command,
        "decode generated orientation frame",
        std::time::Duration::from_secs(20),
    )?;
    ensure!(
        frame.status.success() && frame.stdout.len() >= 320 * 180 * 3,
        "could not decode rotated preview frame"
    );
    let pixel = |x: usize, y: usize| -> [u8; 3] {
        let offset = (y * 320 + x) * 3;
        [
            frame.stdout[offset],
            frame.stdout[offset + 1],
            frame.stdout[offset + 2],
        ]
    };
    let top_left = pixel(135, 45);
    let top_right = pixel(185, 45);
    let bottom_left = pixel(135, 135);
    let bottom_right = pixel(185, 135);
    let is_green = |p: [u8; 3]| p[1] > 100 && p[0] < 130 && p[2] < 130;
    let is_red = |p: [u8; 3]| p[0] > 120 && p[1] < 130 && p[2] < 130;
    let is_yellow = |p: [u8; 3]| p[0] > 120 && p[1] > 120 && p[2] < 130;
    let is_blue = |p: [u8; 3]| p[2] > 100 && p[0] < 130 && p[1] < 130;
    let rotation_visible =
        is_green(top_left) && is_red(top_right) && is_yellow(bottom_left) && is_blue(bottom_right);
    Ok(evaluate_measurements(SmokeMeasurements {
        container_seconds: duration,
        video_seconds: decoded_video_seconds,
        audio_seconds: decoded_audio_seconds,
        tone_rms,
        tone_440_amplitude,
        no_audio_rms,
        silent_rms,
        audio_onset_seconds,
        video_onset_seconds,
        rotation_visible,
        frame_detail: format!("{} decoded frames", frame_count),
        rotation_detail: format!(
            "TL={top_left:?}, TR={top_right:?}, BL={bottom_left:?}, BR={bottom_right:?}"
        ),
    }))
}

fn verify_fixture_diversity(media: &Path, assets: &[crate::Asset]) -> Result<PipelineSmokeCheck> {
    let mut video_profiles = Vec::new();
    let mut audio_profiles = Vec::new();
    for filename in [
        "01-tone.mp4",
        "02-no-audio.mp4",
        "03-silent.mp4",
        "04-rotated.mp4",
    ] {
        let path = media.join(filename);
        let document: serde_json::Value = serde_json::from_str(&command_text(
            &resolve_tool_path("ffprobe"),
            &[
                "-v",
                "error",
                "-show_entries",
                "stream=codec_type,width,height,r_frame_rate,sample_rate,channels",
                "-of",
                "json",
                path.to_str()
                    .ok_or_else(|| anyhow!("non-UTF8 fixture path"))?,
            ],
        )?)?;
        let streams = document["streams"]
            .as_array()
            .ok_or_else(|| anyhow!("ffprobe returned no stream list for generated fixture"))?;
        let video = streams
            .iter()
            .find(|stream| stream["codec_type"] == "video")
            .ok_or_else(|| anyhow!("generated fixture has no video stream"))?;
        video_profiles.push((
            video["width"].as_u64().unwrap_or_default(),
            video["height"].as_u64().unwrap_or_default(),
            video["r_frame_rate"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        ));
        let audio = streams
            .iter()
            .find(|stream| stream["codec_type"] == "audio");
        audio_profiles.push(audio.map(|stream| {
            (
                stream["sample_rate"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                stream["channels"].as_u64().unwrap_or_default(),
            )
        }));
    }
    let valid = video_profiles[0] == (160, 90, "24/1".into())
        && video_profiles[1] == (128, 96, "15/1".into())
        && video_profiles[2] == (160, 90, "30/1".into())
        && video_profiles[3] == (160, 90, "20/1".into())
        && audio_profiles[0] == Some(("44100".into(), 2))
        && audio_profiles[1].is_none()
        && audio_profiles[2] == Some(("32000".into(), 1))
        && assets.len() == 4
        && assets[3].rotation_degrees == 90;
    Ok(check(
        "generated-fixture-diversity",
        valid,
        format!(
            "videoProfiles={video_profiles:?}, audioProfiles={audio_profiles:?}, effectiveRotation={:?}",
            assets.get(3).map(|asset| asset.rotation_degrees)
        ),
        "fixtures decode at mixed dimensions/rates, stereo 44.1 kHz and mono 32 kHz plus no audio, with explicit 90-degree effective rotation",
    ))
}

#[derive(Clone)]
struct SmokeMeasurements {
    container_seconds: f64,
    video_seconds: f64,
    audio_seconds: f64,
    tone_rms: f64,
    tone_440_amplitude: f64,
    no_audio_rms: f64,
    silent_rms: f64,
    audio_onset_seconds: Option<f64>,
    video_onset_seconds: Option<f64>,
    rotation_visible: bool,
    frame_detail: String,
    rotation_detail: String,
}

fn evaluate_measurements(measurements: SmokeMeasurements) -> Vec<PipelineSmokeCheck> {
    let expected_seconds = FIXTURE_SECONDS * 4.0;
    let alignment = measurements
        .audio_onset_seconds
        .zip(measurements.video_onset_seconds);
    let alignment_ok = (measurements.video_seconds - measurements.audio_seconds).abs() <= 0.15
        && alignment.is_some_and(|(audio, video)| (audio - video).abs() <= 0.10);
    vec![
        check(
            "decoded-duration",
            (measurements.container_seconds - expected_seconds).abs() <= 0.18
                && (measurements.video_seconds - expected_seconds).abs() <= 0.18,
            format!(
                "container={:.3}s, decodedVideo={:.3}s, {}",
                measurements.container_seconds,
                measurements.video_seconds,
                measurements.frame_detail
            ),
            "container and decoded video within 180 ms of four generated 0.8 s clips",
        ),
        check(
            "decoded-av-alignment",
            alignment_ok,
            format!(
                "video={:.3}s, pcm={:.3}s, toneOnset={:?}, videoMarkerOnset={:?}",
                measurements.video_seconds,
                measurements.audio_seconds,
                measurements.audio_onset_seconds,
                measurements.video_onset_seconds
            ),
            "decoded video and PCM durations differ by at most 150 ms and tone/video marker onsets by at most 100 ms",
        ),
        check(
            "tone-versus-no-audio-and-silence",
            measurements.tone_rms > 0.015
                && measurements.tone_440_amplitude > 0.02
                && measurements.tone_440_amplitude / measurements.tone_rms > 1.2
                && measurements.no_audio_rms < 0.004
                && measurements.silent_rms < 0.004,
            format!(
                "toneRms={:.5}, expected440HzAmplitude={:.5}, noAudioRms={:.5}, silentRms={:.5}",
                measurements.tone_rms,
                measurements.tone_440_amplitude,
                measurements.no_audio_rms,
                measurements.silent_rms
            ),
            "decoded tone is dominated by 440 Hz; no-audio and silent sections decode near silence",
        ),
        check(
            "effective-rotation",
            measurements.rotation_visible,
            measurements.rotation_detail,
            "asymmetric quadrant colors appear in the expected clockwise 90-degree positions",
        ),
    ]
}

fn check(name: &str, passed: bool, measured: String, expectation: &str) -> PipelineSmokeCheck {
    PipelineSmokeCheck {
        name: name.into(),
        passed,
        measured,
        expectation: expectation.into(),
        failure: (!passed).then(|| format!("{name} did not meet its measured acceptance bound")),
    }
}

fn command_text(program: &Path, arguments: &[&str]) -> Result<String> {
    let mut command = Command::new(program);
    command.args(arguments);
    let output = crate::stall::output_with_timeout(
        &mut command,
        "pipeline-smoke media probe",
        std::time::Duration::from_secs(20),
    )
    .with_context(|| format!("run {}", program.display()))?;
    ensure!(
        output.status.success(),
        "media probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(
        output.stdout.len() <= 1024 * 1024,
        "media probe output exceeded 1 MiB"
    );
    String::from_utf8(output.stdout).context("media probe returned non-UTF8 output")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn smoke_identity() -> PipelineSmokeIdentity {
        let executable_path = std::env::current_exe().unwrap();
        PipelineSmokeIdentity {
            recipe_version: PIPELINE_SMOKE_RECIPE_VERSION.into(),
            executable_build: crate::CliBuild {
                version: "test".into(),
                revision: Some("test-revision".into()),
                dirty: false,
                profile: "test".into(),
                target: "test".into(),
                rustc: "test".into(),
            },
            executable_digest: blake3::hash(&fs::read(executable_path).unwrap())
                .to_hex()
                .to_string(),
            ffmpeg: calibration_tool_identity_for_path(
                "pipeline_transcode",
                "ffmpeg",
                &resolve_tool_path("ffmpeg"),
            ),
            ffprobe: calibration_tool_identity_for_path(
                "pipeline_probe",
                "ffprobe",
                &resolve_tool_path("ffprobe"),
            ),
        }
    }

    fn good_measurements() -> SmokeMeasurements {
        SmokeMeasurements {
            container_seconds: 3.2,
            video_seconds: 3.203,
            audio_seconds: 3.199,
            tone_rms: 0.08,
            tone_440_amplitude: 0.12,
            no_audio_rms: 0.0001,
            silent_rms: 0.0002,
            audio_onset_seconds: Some(0.2),
            video_onset_seconds: Some(0.22),
            rotation_visible: true,
            frame_detail: "96 frames".into(),
            rotation_detail: "quadrant positions verified".into(),
        }
    }

    #[test]
    fn each_decoded_acceptance_rejects_its_own_broken_measurement() {
        assert!(
            evaluate_measurements(good_measurements())
                .iter()
                .all(|check| check.passed)
        );
        let mut broken = Vec::new();
        let mut short = good_measurements();
        short.video_seconds = 2.1;
        short.container_seconds = 2.1;
        short.audio_seconds = 2.1;
        broken.push((short, "decoded-duration"));
        let mut shifted = good_measurements();
        shifted.audio_onset_seconds = Some(0.5);
        broken.push((shifted, "decoded-av-alignment"));
        let mut wrong_frequency = good_measurements();
        wrong_frequency.tone_440_amplitude = 0.001;
        broken.push((wrong_frequency, "tone-versus-no-audio-and-silence"));
        let mut audible_no_audio = good_measurements();
        audible_no_audio.no_audio_rms = 0.05;
        broken.push((audible_no_audio, "tone-versus-no-audio-and-silence"));
        let mut audible_silence = good_measurements();
        audible_silence.silent_rms = 0.05;
        broken.push((audible_silence, "tone-versus-no-audio-and-silence"));
        let mut wrong_rotation = good_measurements();
        wrong_rotation.rotation_visible = false;
        broken.push((wrong_rotation, "effective-rotation"));
        for (measurements, expected_name) in broken {
            let checks = evaluate_measurements(measurements);
            assert!(
                checks
                    .iter()
                    .any(|check| check.name == expected_name && !check.passed),
                "{expected_name} accepted a broken measurement"
            );
            assert_eq!(
                checks.iter().filter(|check| !check.passed).count(),
                1,
                "the broken condition should fail independently"
            );
        }
    }

    #[test]
    fn real_broken_fixtures_fail_the_production_renderer_decoder_checks() {
        for (fault, expected_check) in [
            (SmokeFault::ShortDuration, "decoded-duration"),
            (SmokeFault::ShiftedAudio, "decoded-av-alignment"),
            (SmokeFault::WrongTone, "tone-versus-no-audio-and-silence"),
            (
                SmokeFault::AudibleSilence,
                "tone-versus-no-audio-and-silence",
            ),
            (SmokeFault::WrongRotation, "effective-rotation"),
        ] {
            let directory = tempdir().unwrap();
            let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
            let identity = smoke_identity();
            let report = catalog
                .run_pipeline_smoke_using(identity.clone(), true, |root, heavy| {
                    run_generated_preview_with_fault(root, Some(fault), heavy)
                })
                .unwrap();
            let checks = report.checks;
            assert_eq!(report.state, "failed", "{fault:?} unexpectedly passed");
            assert!(
                checks
                    .iter()
                    .any(|check| check.name == expected_check && !check.passed),
                "{fault:?} did not fail {expected_check}: {checks:#?}"
            );
            assert_eq!(
                catalog.pipeline_smoke_status(identity).unwrap().state,
                "failed"
            );
        }
    }

    #[test]
    fn generated_pipeline_receipt_is_cached_and_identity_changes_are_unverified() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("project.sqlite3")).unwrap();
        let identity = smoke_identity();
        let report = catalog.run_pipeline_smoke(identity.clone(), false).unwrap();
        assert_eq!(report.state, "passed", "{:#?}", report.checks);
        assert!(report.checks.iter().all(|check| check.passed));
        let status = catalog.pipeline_smoke_status(identity.clone()).unwrap();
        assert!(status.reused);
        assert_eq!(status.cache_key, report.cache_key);
        let key = report.cache_key.as_deref().unwrap();
        let connection = catalog.connection().unwrap();
        connection
            .execute(
                "UPDATE pipeline_smoke_receipts SET identity_json='{}' WHERE cache_key=?1",
                [key],
            )
            .unwrap();
        let tampered = catalog.pipeline_smoke_status(identity.clone()).unwrap();
        assert_eq!(tampered.state, "unverified");
        assert!(tampered.remediation.unwrap().contains("inconsistent"));
        let repaired = catalog
            .run_pipeline_smoke_using(identity.clone(), true, |_, _| {
                let mut checks = evaluate_measurements(good_measurements());
                checks.push(check(
                    "generated-fixture-diversity",
                    true,
                    "five media fixtures decoded at required distinct rates and formats".into(),
                    "generated fixtures cover mixed dimensions, frame rates, audio layouts and no-audio input",
                ));
                Ok(checks)
            })
            .unwrap();
        assert_eq!(repaired.state, "passed");
        let stored_debug: (String, String, String) = connection
            .query_row(
                "SELECT identity_json, report_json, state FROM pipeline_smoke_receipts WHERE cache_key=?1",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(stored_debug.2, "passed");
        assert_eq!(
            serde_json::from_str::<PipelineSmokeIdentity>(&stored_debug.0).unwrap(),
            identity
        );
        let stored_report: PipelineSmokeReport = serde_json::from_str(&stored_debug.1).unwrap();
        assert_eq!(stored_report.cache_key.as_deref(), Some(key));
        assert_eq!(stored_report.identity, identity);
        assert!(stored_report.checks.iter().all(|check| check.passed));
        let repaired_status = catalog.pipeline_smoke_status(identity.clone()).unwrap();
        assert_eq!(repaired_status.state, "passed", "{repaired_status:#?}");
        let repaired_identity: String = connection
            .query_row(
                "SELECT identity_json FROM pipeline_smoke_receipts WHERE cache_key=?1",
                [key],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<PipelineSmokeIdentity>(&repaired_identity).unwrap(),
            identity
        );
        connection
            .execute(
                "UPDATE pipeline_smoke_receipts SET identity_json=?2 WHERE cache_key=?1",
                params![key, serde_json::to_string(&identity).unwrap()],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE pipeline_smoke_receipts SET state='failed' WHERE cache_key=?1",
                [key],
            )
            .unwrap();
        assert_eq!(
            catalog
                .pipeline_smoke_status(identity.clone())
                .unwrap()
                .state,
            "unverified"
        );
        connection
            .execute(
                "UPDATE pipeline_smoke_receipts SET state='passed' WHERE cache_key=?1",
                [key],
            )
            .unwrap();
        let mut wrong_report = report.clone();
        wrong_report.schema_version = "wrong-schema".into();
        connection
            .execute(
                "UPDATE pipeline_smoke_receipts SET report_json=?2 WHERE cache_key=?1",
                params![key, serde_json::to_string(&wrong_report).unwrap()],
            )
            .unwrap();
        assert_eq!(
            catalog
                .pipeline_smoke_status(identity.clone())
                .unwrap()
                .state,
            "unverified"
        );
        connection
            .execute(
                "UPDATE pipeline_smoke_receipts SET report_json=?2 WHERE cache_key=?1",
                params![key, serde_json::to_string(&report).unwrap()],
            )
            .unwrap();
        let mut changed_executable = identity.clone();
        changed_executable.executable_digest = "changed-executable-digest".into();
        assert_eq!(
            catalog
                .pipeline_smoke_status(changed_executable)
                .unwrap()
                .state,
            "unverified"
        );
        let mut changed_tool = identity.clone();
        changed_tool.ffmpeg.executable_key = "changed-ffmpeg-identity".into();
        assert_eq!(
            catalog.pipeline_smoke_status(changed_tool).unwrap().state,
            "unverified"
        );
        let mut changed_recipe = identity;
        changed_recipe.recipe_version.push_str("-changed");
        assert_eq!(
            catalog.pipeline_smoke_status(changed_recipe).unwrap().state,
            "unverified"
        );
    }
}
