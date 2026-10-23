//! Encoder policy, bounded hardware probing, and validated proxy recipes.
//!
//! Proxy representations have one output contract (H.264, at most 1280 px
//! wide, zero display rotation, AAC source audio, v3 cache identity) but more
//! than one encoder implementation. This module is the domain owner for
//! choosing an implementation, proving hardware can really initialize, running
//! a full-clip recipe, validating its output, and describing the fallback that
//! happened. Catalog registration and SQLite provenance remain in the catalog
//! layer.

use crate::{
    drain_child_stderr, ffmpeg_progress_fraction, redact_media_diagnostic, resolve_tool,
    stall::output_with_timeout,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
#[cfg(any(target_os = "linux", test))]
use std::path::PathBuf;
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Canonical semantic recipe for a v3 proxy. Existing cache names, validation,
/// and camera-sidecar separation depend on this identity.
pub const PROXY_SOFTWARE_RECIPE_KEY: &str = "proxy.h264-720p.orientation-v3";
pub const PROXY_VAAPI_RECIPE_KEY: &str = "proxy.h264-720p.orientation-v3.linux-vaapi-qp23-v1";
#[cfg(target_os = "macos")]
pub const PROXY_VIDEOTOOLBOX_RECIPE_KEY: &str =
    "proxy.h264-720p.orientation-v3.macos-videotoolbox-q36-v1";

const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const VALIDATION_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_DIAGNOSTIC_CHARS: usize = 4096;

/// Explicit selection policy for shared proxy generation.
///
/// `Auto` is the safe default: on Linux it attempts the supported VAAPI
/// decode/scale/encode path only after a real bounded encoder probe succeeds,
/// then always retains the software recipe as its fallback. `Software` never
/// initializes or claims hardware and is deterministic for tests and operators.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyEncoderPolicy {
    #[default]
    Auto,
    Software,
}

impl ProxyEncoderPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Software => "software",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "auto" => Ok(Self::Auto),
            "software" => Ok(Self::Software),
            _ => bail!("unknown proxy encoder policy {value:?}; use auto or software"),
        }
    }
}

/// An encoder implementation that actually produced, or attempted, a proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyEncoderKind {
    Software,
    Vaapi,
    #[cfg(target_os = "macos")]
    VideoToolbox,
}

impl ProxyEncoderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Software => "software",
            Self::Vaapi => "vaapi",
            #[cfg(target_os = "macos")]
            Self::VideoToolbox => "videotoolbox",
        }
    }

    pub fn recipe_key(self) -> &'static str {
        match self {
            Self::Software => PROXY_SOFTWARE_RECIPE_KEY,
            Self::Vaapi => PROXY_VAAPI_RECIPE_KEY,
            #[cfg(target_os = "macos")]
            Self::VideoToolbox => PROXY_VIDEOTOOLBOX_RECIPE_KEY,
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "software" => Ok(Self::Software),
            "vaapi" => Ok(Self::Vaapi),
            #[cfg(target_os = "macos")]
            "videotoolbox" => Ok(Self::VideoToolbox),
            _ => bail!("unknown proxy encoder {value:?}"),
        }
    }
}

/// One encoder/device pair the runner may try. A fallback plan places software
/// last; it is never inferred at commit time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyEncoderTarget {
    pub encoder: ProxyEncoderKind,
    pub device: Option<String>,
    pub recipe_key: String,
}

impl ProxyEncoderTarget {
    fn software() -> Self {
        Self {
            encoder: ProxyEncoderKind::Software,
            device: None,
            recipe_key: PROXY_SOFTWARE_RECIPE_KEY.to_string(),
        }
    }

    #[cfg(any(target_os = "linux", test))]
    fn vaapi(device: String) -> Self {
        Self {
            encoder: ProxyEncoderKind::Vaapi,
            device: Some(device),
            recipe_key: PROXY_VAAPI_RECIPE_KEY.to_string(),
        }
    }

    #[cfg(target_os = "macos")]
    fn video_toolbox() -> Self {
        Self {
            encoder: ProxyEncoderKind::VideoToolbox,
            device: None,
            recipe_key: PROXY_VIDEOTOOLBOX_RECIPE_KEY.to_string(),
        }
    }
}

/// A capability observation. `usable` is intentionally not derived from
/// `device_present`; it is set only after the installed FFmpeg initialized the
/// device and encoded a bounded synthetic clip with the selected recipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncoderCapability {
    pub encoder: String,
    pub device: Option<String>,
    pub device_present: bool,
    pub device_accessible: Option<bool>,
    pub ffmpeg_encoder_present: bool,
    pub initialization_tested: bool,
    pub usable: bool,
    /// Stable machine-readable outcome: `usable`, `device_unavailable`,
    /// `device_permission_denied`, `ffmpeg_encoder_unavailable`,
    /// `initialization_failed`, `not_requested`, or `unsupported_platform`.
    pub status: String,
    pub detail: String,
}

impl EncoderCapability {
    fn unavailable(
        encoder: &str,
        device: Option<String>,
        device_present: bool,
        device_accessible: Option<bool>,
        ffmpeg_encoder_present: bool,
        status: &str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            encoder: encoder.to_string(),
            device,
            device_present,
            device_accessible,
            ffmpeg_encoder_present,
            initialization_tested: false,
            usable: false,
            status: status.to_string(),
            detail: detail.into(),
        }
    }
}

/// Machine-readable host capability report used by `proxy probe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyEncoderProbeReport {
    pub policy: ProxyEncoderPolicy,
    pub platform: String,
    pub software: EncoderCapability,
    pub hardware: Vec<EncoderCapability>,
    pub hardware_usable: bool,
    pub selected_encoder: String,
    pub checked_at: String,
    pub limitations: Vec<String>,
}

impl ProxyEncoderProbeReport {
    fn selected_hardware_target(&self) -> Option<ProxyEncoderTarget> {
        let candidate = self.hardware.iter().find(|candidate| candidate.usable)?;
        match candidate.encoder.as_str() {
            #[cfg(target_os = "linux")]
            "vaapi" | "h264_vaapi" => candidate.device.clone().map(ProxyEncoderTarget::vaapi),
            #[cfg(target_os = "macos")]
            "videotoolbox" => Some(ProxyEncoderTarget::video_toolbox()),
            _ => None,
        }
    }
}

/// Resolved attempt order. The runner consumes this exact plan so diagnostics
/// can distinguish "policy selected software" from "hardware probe failed and
/// auto fell back to software."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyEncoderPlan {
    pub policy: ProxyEncoderPolicy,
    pub preferred_encoder: ProxyEncoderKind,
    pub targets: Vec<ProxyEncoderTarget>,
    pub hardware_unavailable: Option<EncoderCapability>,
    pub software_usable: bool,
    pub limitations: Vec<String>,
}

impl ProxyEncoderPlan {
    pub fn preferred_recipe_key(&self) -> &str {
        self.targets
            .first()
            .map(|target| target.recipe_key.as_str())
            .unwrap_or(PROXY_SOFTWARE_RECIPE_KEY)
    }
}

/// Status of one encoder attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyAttemptStatus {
    Succeeded,
    ProcessFailed,
    ValidationFailed,
    Unavailable,
}

impl ProxyAttemptStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::ProcessFailed => "process_failed",
            Self::ValidationFailed => "validation_failed",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Bounded, path-redacted evidence of one attempted encoder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyEncoderAttempt {
    pub encoder: String,
    pub device: Option<String>,
    pub recipe_key: String,
    pub status: ProxyAttemptStatus,
    pub elapsed_ms: u64,
    pub diagnostic: Option<String>,
}

/// Metadata and decode checks performed before atomic registration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyOutputValidation {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub pixel_format: String,
    pub profile: Option<String>,
    pub duration_seconds: f64,
    pub expected_duration_seconds: f64,
    pub duration_tolerance_seconds: f64,
    pub source_audio_present: bool,
    pub audio_stream_count: usize,
    pub audio_codec: Option<String>,
    pub metadata_rotation_degrees: u16,
    pub decoded: bool,
    /// Metadata correspondence supplements full output decode; it does not
    /// certify visual equivalence of every source frame.
    #[serde(default)]
    pub time_correspondence: Option<ProxyTimeCorrespondence>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyTimeCorrespondence {
    pub source_video_frames: Option<u64>,
    pub output_video_frames: Option<u64>,
    pub source_audio_offset_seconds: Option<f64>,
    pub output_audio_offset_seconds: Option<f64>,
    pub audio_duration_difference_seconds: Option<f64>,
}

/// Provenance of one complete proxy generation transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyGenerationOutcome {
    pub policy: ProxyEncoderPolicy,
    pub status: String,
    pub selected_encoder: String,
    pub recipe_key: String,
    pub device: Option<String>,
    pub fallback_used: bool,
    pub elapsed_ms: u64,
    pub attempts: Vec<ProxyEncoderAttempt>,
    pub validation: Option<ProxyOutputValidation>,
}

impl ProxyGenerationOutcome {
    /// Excludes earlier failed hardware attempts from successful-encoder rate
    /// history. Total transaction time remains available in elapsed_ms.
    pub fn successful_elapsed(&self) -> Duration {
        Duration::from_millis(
            self.attempts
                .iter()
                .rev()
                .find(|attempt| attempt.status == ProxyAttemptStatus::Succeeded)
                .map(|attempt| attempt.elapsed_ms)
                .unwrap_or(1)
                .max(1),
        )
    }

    fn failed(
        policy: ProxyEncoderPolicy,
        attempts: Vec<ProxyEncoderAttempt>,
        elapsed_ms: u64,
        fallback_used: bool,
    ) -> Self {
        let last = attempts.last();
        Self {
            policy,
            status: "failed".to_string(),
            selected_encoder: last
                .map(|attempt| attempt.encoder.clone())
                .unwrap_or_else(|| "software".to_string()),
            recipe_key: last
                .map(|attempt| attempt.recipe_key.clone())
                .unwrap_or_else(|| PROXY_SOFTWARE_RECIPE_KEY.to_string()),
            device: last.and_then(|attempt| attempt.device.clone()),
            fallback_used,
            elapsed_ms,
            attempts,
            validation: None,
        }
    }
}

/// A typed failure that still carries durable provenance for SQLite.
#[derive(Debug, Clone)]
pub struct ProxyTranscodeFailure {
    pub message: String,
    pub outcome: ProxyGenerationOutcome,
}

impl std::fmt::Display for ProxyTranscodeFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProxyTranscodeFailure {}

/// Returns a host capability report. On Linux `Auto` performs a real bounded
/// FFmpeg VAAPI decode/scale/H.264-encode probe with generated synthetic media.
/// A device node, an `ffmpeg -encoders` entry, or a readable path alone never
/// makes a candidate usable.
pub fn probe_proxy_encoder_capabilities(policy: ProxyEncoderPolicy) -> ProxyEncoderProbeReport {
    probe_proxy_encoder_capabilities_with_tools(
        policy,
        &resolve_tool("ffmpeg"),
        &resolve_tool("ffprobe"),
    )
}

/// Probe using the exact configured media tools. Server adapters use this so a
/// custom `RCDVLOG_FFMPEG` is never silently probed through a different binary.
pub fn probe_proxy_encoder_capabilities_with_tools(
    policy: ProxyEncoderPolicy,
    ffmpeg: &Path,
    ffprobe: &Path,
) -> ProxyEncoderProbeReport {
    match policy {
        ProxyEncoderPolicy::Software => software_only_probe(ffmpeg),
        ProxyEncoderPolicy::Auto => {
            #[cfg(target_os = "linux")]
            {
                cached_linux_auto_probe(ffmpeg, ffprobe)
            }
            #[cfg(target_os = "macos")]
            {
                let _ = ffprobe;
                macos_auto_probe(ffmpeg)
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                let _ = ffprobe;
                unsupported_platform_probe(ffmpeg)
            }
        }
    }
}

fn software_only_probe(ffmpeg: &Path) -> ProxyEncoderProbeReport {
    let software = software_capability(ffmpeg);
    ProxyEncoderProbeReport {
        policy: ProxyEncoderPolicy::Software,
        platform: std::env::consts::OS.to_string(),
        selected_encoder: "software".to_string(),
        hardware_usable: false,
        software,
        hardware: vec![EncoderCapability::unavailable(
            "h264_vaapi",
            None,
            false,
            None,
            false,
            "not_requested",
            "policy=software does not initialize hardware; choose policy=auto to probe it",
        )],
        checked_at: timestamp(),
        limitations: probe_limitations(),
    }
}

#[cfg(target_os = "linux")]
type LinuxProbeCache = Option<(PathBuf, PathBuf, ProxyEncoderProbeReport)>;

#[cfg(target_os = "linux")]
fn shared_auto_probe_cache() -> &'static Mutex<LinuxProbeCache> {
    static CACHE: OnceLock<Mutex<LinuxProbeCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

#[cfg(target_os = "linux")]
fn cached_linux_auto_probe(ffmpeg: &Path, ffprobe: &Path) -> ProxyEncoderProbeReport {
    let ffmpeg_key = ffmpeg.to_path_buf();
    let ffprobe_key = ffprobe.to_path_buf();
    let mut guard = shared_auto_probe_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((cached_ffmpeg, cached_ffprobe, report)) = guard.as_ref()
        && cached_ffmpeg == &ffmpeg_key
        && cached_ffprobe == &ffprobe_key
    {
        return report.clone();
    }
    let report = linux_auto_probe(ffmpeg, ffprobe);
    *guard = Some((ffmpeg_key, ffprobe_key, report.clone()));
    report
}

#[cfg(test)]
pub(crate) fn clear_proxy_encoder_probe_cache() {
    #[cfg(target_os = "linux")]
    if let Ok(mut guard) = shared_auto_probe_cache().lock() {
        *guard = None;
    }
}

#[cfg(target_os = "linux")]
fn linux_auto_probe(ffmpeg: &Path, ffprobe: &Path) -> ProxyEncoderProbeReport {
    let software = software_capability(ffmpeg);
    let encoder_present = ffmpeg_advertises_encoder(ffmpeg, "h264_vaapi");
    let devices = linux_render_devices();
    let mut candidates = Vec::new();

    if devices.is_empty() {
        candidates.push(EncoderCapability::unavailable(
            "h264_vaapi",
            None,
            false,
            None,
            encoder_present,
            "device_unavailable",
            "no /dev/dri/renderD* device exists; verify the GPU driver and any container --device=/dev/dri mapping, or use policy=software",
        ));
    } else if !encoder_present {
        for device in devices {
            candidates.push(EncoderCapability::unavailable(
                "h264_vaapi",
                Some(device.display().to_string()),
                true,
                None,
                false,
                "ffmpeg_encoder_unavailable",
                "the installed FFmpeg does not advertise h264_vaapi; install a VAAPI-enabled FFmpeg or use policy=software",
            ));
        }
    } else {
        let probe_directory = probe_directory();
        let probe_source = probe_directory
            .as_ref()
            .map(|directory| directory.join("synthetic-source.mp4"));
        let source_ready = probe_source
            .as_deref()
            .map(|source| create_probe_source(ffmpeg, source).is_ok())
            .unwrap_or(false);
        for device in devices {
            let accessible = device_accessible(&device);
            let device_string = device.display().to_string();
            if !accessible {
                candidates.push(EncoderCapability::unavailable(
                    "h264_vaapi",
                    Some(device_string),
                    true,
                    Some(false),
                    true,
                    "device_permission_denied",
                    "the render device exists but this process cannot open it read/write; check render/video group membership or device permissions",
                ));
                continue;
            }
            if !source_ready {
                candidates.push(EncoderCapability::unavailable(
                    "h264_vaapi",
                    Some(device_string),
                    true,
                    Some(true),
                    true,
                    "initialization_failed",
                    "could not generate the bounded synthetic probe source with the configured FFmpeg",
                ));
                continue;
            }
            let started = Instant::now();
            match run_hardware_probe(ffmpeg, ffprobe, probe_source.as_deref().unwrap(), &device) {
                Ok(()) => candidates.push(EncoderCapability {
                    encoder: "h264_vaapi".to_string(),
                    device: Some(device_string),
                    device_present: true,
                    device_accessible: Some(true),
                    ffmpeg_encoder_present: true,
                    initialization_tested: true,
                    usable: true,
                    status: "usable".to_string(),
                    detail: format!(
                        "real bounded VAAPI decode/scale/H.264 encode succeeded in {} ms",
                        started.elapsed().as_millis()
                    ),
                }),
                Err(error) => candidates.push(EncoderCapability::unavailable(
                    "h264_vaapi",
                    Some(device_string),
                    true,
                    Some(true),
                    true,
                    "initialization_failed",
                    format!(
                        "bounded VAAPI initialization/decode/scale/encode probe failed: {}",
                        redact_probe_error(&error.to_string())
                    ),
                )),
            }
        }
        if let Some(directory) = probe_directory {
            let _ = fs::remove_dir_all(directory);
        }
    }

    let selected_hardware = candidates
        .iter()
        .find(|candidate| candidate.usable)
        .map(|candidate| match candidate.encoder.as_str() {
            "h264_vaapi" => "vaapi".to_string(),
            other => other.to_string(),
        });
    ProxyEncoderProbeReport {
        policy: ProxyEncoderPolicy::Auto,
        platform: "linux".to_string(),
        selected_encoder: selected_hardware.unwrap_or_else(|| "software".to_string()),
        hardware_usable: candidates.iter().any(|candidate| candidate.usable),
        software,
        hardware: candidates,
        checked_at: timestamp(),
        limitations: probe_limitations(),
    }
}

#[cfg(target_os = "macos")]
fn macos_auto_probe(ffmpeg: &Path) -> ProxyEncoderProbeReport {
    let software = software_capability(ffmpeg);
    let encoder_present = ffmpeg_advertises_encoder(ffmpeg, "h264_videotoolbox");
    let hardware = EncoderCapability {
        encoder: "videotoolbox".to_string(),
        device: None,
        device_present: true,
        device_accessible: Some(true),
        ffmpeg_encoder_present: encoder_present,
        initialization_tested: false,
        usable: encoder_present,
        status: if encoder_present {
            "advertised_pending_per_input".to_string()
        } else {
            "ffmpeg_encoder_unavailable".to_string()
        },
        detail: "VideoToolbox has no device-file presence check; the existing per-clip attempt remains authoritative and falls back to software on failure".to_string(),
    };
    ProxyEncoderProbeReport {
        policy: ProxyEncoderPolicy::Auto,
        platform: "macos".to_string(),
        selected_encoder: if hardware.usable {
            "videotoolbox"
        } else {
            "software"
        }
        .to_string(),
        hardware_usable: hardware.usable,
        software,
        hardware: vec![hardware],
        checked_at: timestamp(),
        limitations: probe_limitations(),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn unsupported_platform_probe(ffmpeg: &Path) -> ProxyEncoderProbeReport {
    ProxyEncoderProbeReport {
        policy: ProxyEncoderPolicy::Auto,
        platform: std::env::consts::OS.to_string(),
        selected_encoder: "software".to_string(),
        hardware_usable: false,
        software: software_capability(ffmpeg),
        hardware: vec![EncoderCapability::unavailable(
            "hardware-h264",
            None,
            false,
            None,
            false,
            "unsupported_platform",
            "hardware probe and fallback policy are implemented for Linux VAAPI; this host uses the software recipe",
        )],
        checked_at: timestamp(),
        limitations: probe_limitations(),
    }
}

/// Resolve claim order without running a clip. Auto on Linux uses the cached
/// bounded probe, so a batch does not re-probe hardware once per asset.
pub fn resolve_proxy_encoder_plan(policy: ProxyEncoderPolicy) -> ProxyEncoderPlan {
    resolve_proxy_encoder_plan_with_tools(policy, &resolve_tool("ffmpeg"), &resolve_tool("ffprobe"))
}

pub fn resolve_proxy_encoder_plan_with_tools(
    policy: ProxyEncoderPolicy,
    ffmpeg: &Path,
    ffprobe: &Path,
) -> ProxyEncoderPlan {
    let report = probe_proxy_encoder_capabilities_with_tools(policy, ffmpeg, ffprobe);
    let software_usable = report.software.usable;
    match policy {
        ProxyEncoderPolicy::Software => ProxyEncoderPlan {
            policy,
            preferred_encoder: ProxyEncoderKind::Software,
            targets: vec![ProxyEncoderTarget::software()],
            hardware_unavailable: None,
            software_usable,
            limitations: report.limitations,
        },
        ProxyEncoderPolicy::Auto => {
            let software = ProxyEncoderTarget::software();
            if let Some(hardware) = report.selected_hardware_target() {
                let preferred_encoder = hardware.encoder;
                ProxyEncoderPlan {
                    policy,
                    preferred_encoder,
                    targets: vec![hardware, software],
                    hardware_unavailable: None,
                    software_usable,
                    limitations: report.limitations,
                }
            } else {
                let hardware_unavailable = report
                    .hardware
                    .iter()
                    .find(|candidate| !candidate.usable)
                    .cloned();
                ProxyEncoderPlan {
                    policy,
                    preferred_encoder: ProxyEncoderKind::Software,
                    targets: vec![software],
                    hardware_unavailable,
                    software_usable,
                    limitations: report.limitations,
                }
            }
        }
    }
}

/// Run a complete planned proxy generation on a synthetic-or-real source.
///
/// The staging output is removed between attempts and on total failure. The
/// caller must hold the existing RepresentationWrite claim and must atomically
/// register the successfully validated output afterward.
#[allow(clippy::too_many_arguments, clippy::result_large_err)]
pub fn run_proxy_transcode_with_plan<F>(
    ffmpeg: &Path,
    ffprobe: &Path,
    source: &Path,
    output: &Path,
    duration_seconds: f64,
    source_audio_present: bool,
    plan: &ProxyEncoderPlan,
    on_progress: F,
) -> std::result::Result<ProxyGenerationOutcome, ProxyTranscodeFailure>
where
    F: FnMut(f64),
{
    run_proxy_transcode_with_plan_and_cancellation(
        ffmpeg,
        ffprobe,
        source,
        output,
        duration_seconds,
        source_audio_present,
        plan,
        on_progress,
        || false,
    )
}

/// Shared transcode with prompt in-flight cancellation. Cancellation stops the
/// current child and suppresses fallback; the caller still owns registration.
#[allow(clippy::too_many_arguments, clippy::result_large_err)]
pub fn run_proxy_transcode_with_plan_and_cancellation<F, C>(
    ffmpeg: &Path,
    ffprobe: &Path,
    source: &Path,
    output: &Path,
    duration_seconds: f64,
    source_audio_present: bool,
    plan: &ProxyEncoderPlan,
    mut on_progress: F,
    is_cancelled: C,
) -> std::result::Result<ProxyGenerationOutcome, ProxyTranscodeFailure>
where
    F: FnMut(f64),
    C: Fn() -> bool,
{
    let started = Instant::now();
    let mut attempts = Vec::new();
    if let Some(unavailable) = &plan.hardware_unavailable {
        attempts.push(ProxyEncoderAttempt {
            encoder: unavailable.encoder.clone(),
            device: unavailable.device.clone(),
            recipe_key: match unavailable.encoder.as_str() {
                "vaapi" | "h264_vaapi" => ProxyEncoderKind::Vaapi.recipe_key().to_string(),
                #[cfg(target_os = "macos")]
                "videotoolbox" => ProxyEncoderKind::VideoToolbox.recipe_key().to_string(),
                _ => PROXY_SOFTWARE_RECIPE_KEY.to_string(),
            },
            status: ProxyAttemptStatus::Unavailable,
            elapsed_ms: 0,
            diagnostic: Some(bound_diagnostic(&unavailable.detail)),
        });
    }

    let mut last_failure = None;
    for target in &plan.targets {
        if is_cancelled() {
            last_failure = Some("proxy generation canceled".to_string());
            break;
        }
        let attempt_started = Instant::now();
        let _ = fs::remove_file(output);
        on_progress(0.0);
        let process = run_proxy_ffmpeg_encoder(
            ffmpeg,
            source,
            output,
            target,
            duration_seconds,
            &mut on_progress,
            &is_cancelled,
        );
        let process = match process {
            Ok(process) => process,
            Err(error) => {
                let diagnostic =
                    bound_diagnostic(&redact_media_diagnostic(&error.to_string(), source, output));
                attempts.push(ProxyEncoderAttempt {
                    encoder: target.encoder.as_str().to_string(),
                    device: target.device.clone(),
                    recipe_key: target.recipe_key.clone(),
                    status: ProxyAttemptStatus::ProcessFailed,
                    elapsed_ms: elapsed_ms(attempt_started),
                    diagnostic: Some(diagnostic.clone()),
                });
                last_failure = Some(format!(
                    "{} attempt could not start: {diagnostic}",
                    target.encoder.as_str()
                ));
                continue;
            }
        };

        if !process.status.success() {
            let diagnostic = bound_diagnostic(&process.diagnostic);
            attempts.push(ProxyEncoderAttempt {
                encoder: target.encoder.as_str().to_string(),
                device: target.device.clone(),
                recipe_key: target.recipe_key.clone(),
                status: ProxyAttemptStatus::ProcessFailed,
                elapsed_ms: elapsed_ms(attempt_started),
                diagnostic: Some(diagnostic.clone()),
            });
            last_failure = Some(format!(
                "{} attempt exited {}: {}",
                target.encoder.as_str(),
                process.status,
                diagnostic
            ));
            let _ = fs::remove_file(output);
            continue;
        }

        match validate_proxy_output_with_cancellation(
            ffmpeg,
            ffprobe,
            output,
            duration_seconds,
            source_audio_present,
            &is_cancelled,
        )
        .and_then(|mut validation| {
            validation.time_correspondence =
                Some(validate_source_correspondence(ffprobe, source, output)?);
            Ok(validation)
        }) {
            Ok(validation) => {
                let elapsed = elapsed_ms(attempt_started);
                attempts.push(ProxyEncoderAttempt {
                    encoder: target.encoder.as_str().to_string(),
                    device: target.device.clone(),
                    recipe_key: target.recipe_key.clone(),
                    status: ProxyAttemptStatus::Succeeded,
                    elapsed_ms: elapsed,
                    diagnostic: None,
                });
                return Ok(ProxyGenerationOutcome {
                    policy: plan.policy,
                    status: "succeeded".to_string(),
                    selected_encoder: target.encoder.as_str().to_string(),
                    recipe_key: target.recipe_key.clone(),
                    device: target.device.clone(),
                    fallback_used: plan.policy == ProxyEncoderPolicy::Auto
                        && (target.encoder != plan.preferred_encoder
                            || plan.hardware_unavailable.is_some()),
                    elapsed_ms: elapsed_ms(started),
                    attempts,
                    validation: Some(validation),
                });
            }
            Err(error) => {
                let diagnostic =
                    bound_diagnostic(&redact_media_diagnostic(&error.to_string(), source, output));
                attempts.push(ProxyEncoderAttempt {
                    encoder: target.encoder.as_str().to_string(),
                    device: target.device.clone(),
                    recipe_key: target.recipe_key.clone(),
                    status: ProxyAttemptStatus::ValidationFailed,
                    elapsed_ms: elapsed_ms(attempt_started),
                    diagnostic: Some(diagnostic.clone()),
                });
                last_failure = Some(format!(
                    "{} output failed validation: {diagnostic}",
                    target.encoder.as_str()
                ));
                let _ = fs::remove_file(output);
            }
        }
    }

    let _ = fs::remove_file(output);
    let fallback_used = plan.policy == ProxyEncoderPolicy::Auto
        && (plan.preferred_encoder != ProxyEncoderKind::Software
            || plan.hardware_unavailable.is_some());
    Err(ProxyTranscodeFailure {
        message: last_failure.unwrap_or_else(|| "no proxy encoder was available".to_string()),
        outcome: ProxyGenerationOutcome::failed(
            plan.policy,
            attempts,
            elapsed_ms(started),
            fallback_used,
        ),
    })
}

#[allow(clippy::too_many_arguments)]
fn run_proxy_ffmpeg_encoder<F>(
    ffmpeg: &Path,
    source: &Path,
    output: &Path,
    target: &ProxyEncoderTarget,
    duration_seconds: f64,
    on_progress: &mut F,
    is_cancelled: &impl Fn() -> bool,
) -> Result<ProxyProcessResult>
where
    F: FnMut(f64),
{
    let mut command = proxy_ffmpeg_command_with_program(ffmpeg, source, output, target);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| format!("spawn {} ffmpeg proxy process", target.encoder.as_str()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("ffmpeg progress pipe was not available"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("ffmpeg stderr pipe was not available"))?;
    let stderr_reader = drain_child_stderr(stderr);
    let (sender, receiver) = std::sync::mpsc::channel();
    let progress_reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout)
            .lines()
            .map_while(std::result::Result::ok)
        {
            if let Some(fraction) = ffmpeg_progress_fraction(&line, duration_seconds) {
                let _ = sender.send(fraction);
            }
        }
    });
    let status = loop {
        if is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = progress_reader.join();
            let _ = stderr_reader.join();
            bail!("proxy generation canceled");
        }
        for fraction in receiver.try_iter() {
            on_progress(fraction);
        }
        if let Some(status) = child.try_wait().context("wait for ffmpeg proxy process")? {
            break status;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = progress_reader.join();
    for fraction in receiver.try_iter() {
        on_progress(fraction);
    }
    let stderr = stderr_reader.join().unwrap_or_default();
    let stderr = String::from_utf8_lossy(&stderr).into_owned();
    let diagnostic = redact_media_diagnostic(&stderr, source, output);
    Ok(ProxyProcessResult { status, diagnostic })
}

struct ProxyProcessResult {
    status: std::process::ExitStatus,
    diagnostic: String,
}

/// Build the full-clip command for a target. Linux VAAPI uses the same device
/// for hard decode, the filter pool, and encoding; software and VideoToolbox
/// retain their existing recipes.
pub fn proxy_ffmpeg_command_with_program(
    ffmpeg: &Path,
    source: &Path,
    output: &Path,
    target: &ProxyEncoderTarget,
) -> Command {
    let mut command = Command::new(ffmpeg);
    command.args([
        "-y",
        "-hide_banner",
        "-loglevel",
        "error",
        "-stats_period",
        "1",
        "-progress",
        "pipe:1",
        "-nostats",
    ]);

    if target.encoder == ProxyEncoderKind::Vaapi
        && let Some(device) = target.device.as_deref()
    {
        command
            .arg("-init_hw_device")
            .arg(format!("vaapi=va:{device}"))
            .args(["-filter_hw_device", "va"])
            .args(["-hwaccel", "vaapi"])
            .args(["-hwaccel_device", device])
            .args(["-hwaccel_output_format", "vaapi"]);
    }
    #[cfg(target_os = "macos")]
    if target.encoder == ProxyEncoderKind::VideoToolbox {
        command.args(["-hwaccel", "videotoolbox"]);
    }

    command
        .args(["-display_rotation:v:0", "0", "-noautorotate", "-i"])
        .arg(source)
        .args(["-map", "0:v:0", "-map", "0:a:0?", "-vf"]);

    match target.encoder {
        ProxyEncoderKind::Software => {
            command.arg("scale='min(1280,iw)':-2:force_original_aspect_ratio=decrease");
        }
        ProxyEncoderKind::Vaapi => {
            command.arg("scale_vaapi=w='min(1280,iw)':h=-2:format=nv12");
        }
        #[cfg(target_os = "macos")]
        ProxyEncoderKind::VideoToolbox => {
            command.arg("scale='min(1280,iw)':-2:force_original_aspect_ratio=decrease");
        }
    }

    match target.encoder {
        ProxyEncoderKind::Software => {
            command.args([
                "-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p",
            ]);
        }
        ProxyEncoderKind::Vaapi => {
            command.args(["-c:v", "h264_vaapi", "-profile:v", "high", "-qp", "23"]);
        }
        #[cfg(target_os = "macos")]
        ProxyEncoderKind::VideoToolbox => {
            command.args([
                "-c:v",
                "h264_videotoolbox",
                "-profile:v",
                "high",
                "-q:v",
                "36",
                "-power_efficient",
                "1",
            ]);
        }
    }

    command.args([
        "-c:a",
        "aac",
        "-b:a",
        "128k",
        "-movflags",
        "+faststart",
        "-fps_mode",
        "passthrough",
        "-map_metadata",
        "-1",
    ]);
    command.arg(output);
    command
}

/// Validate a staged proxy before registration. This checks the output
/// contract, source-time duration, orientation, source-audio correspondence,
/// and then decodes every mapped stream through FFmpeg rather than trusting
/// headers alone.
pub fn validate_proxy_output(
    ffmpeg: &Path,
    ffprobe: &Path,
    path: &Path,
    expected_duration_seconds: f64,
    source_audio_present: bool,
) -> Result<ProxyOutputValidation> {
    validate_proxy_output_with_cancellation(
        ffmpeg,
        ffprobe,
        path,
        expected_duration_seconds,
        source_audio_present,
        &|| false,
    )
}

fn validate_proxy_output_with_cancellation(
    ffmpeg: &Path,
    ffprobe: &Path,
    path: &Path,
    expected_duration_seconds: f64,
    source_audio_present: bool,
    is_cancelled: &impl Fn() -> bool,
) -> Result<ProxyOutputValidation> {
    ensure!(!is_cancelled(), "proxy generation canceled");
    ensure!(
        expected_duration_seconds.is_finite() && expected_duration_seconds > 0.0,
        "source has no finite positive duration"
    );
    let document = ffprobe_document(ffprobe, path)?;
    let video = crate::require_video_stream(&document)?;
    let codec = video["codec_name"]
        .as_str()
        .ok_or_else(|| anyhow!("proxy video stream has no codec name"))?;
    ensure!(
        codec == "h264",
        "proxy video codec must be h264, got {codec}"
    );
    let width = video["width"]
        .as_u64()
        .ok_or_else(|| anyhow!("proxy video stream has no width"))?;
    let height = video["height"]
        .as_u64()
        .ok_or_else(|| anyhow!("proxy video stream has no height"))?;
    ensure!(width > 0 && height > 0, "proxy dimensions are empty");
    ensure!(width <= 1280, "proxy width {width} exceeds 1280");
    let pixel_format = video["pix_fmt"]
        .as_str()
        .ok_or_else(|| anyhow!("proxy video stream has no pixel format"))?;
    ensure!(
        pixel_format == "yuv420p",
        "proxy pixel format must be yuv420p, got {pixel_format}"
    );
    let rotation = crate::stream_rotation(Some(video));
    ensure!(
        rotation == 0,
        "ffmpeg proxy validation failed: output retains a {rotation}° display rotation"
    );
    let duration = crate::video_duration(&document, video)
        .ok_or_else(|| anyhow!("proxy has no finite positive duration"))?;
    // A long clip must not gain seconds of allowed truncation merely because
    // it is long. AAC priming/frame rounding is covered by a fixed bound.
    let tolerance = 0.25;
    ensure!(
        (duration - expected_duration_seconds).abs() <= tolerance,
        "proxy/source duration mismatch: output {duration:.3}s, source {expected_duration_seconds:.3}s (tolerance {tolerance:.3}s)"
    );

    let audio_streams: Vec<&Value> = document["streams"]
        .as_array()
        .map(|streams| {
            streams
                .iter()
                .filter(|stream| stream["codec_type"] == "audio")
                .collect()
        })
        .unwrap_or_default();
    ensure!(
        source_audio_present == !audio_streams.is_empty(),
        "source/proxy audio correspondence failed: sourceAudio={source_audio_present}, proxyAudioStreams={}",
        audio_streams.len()
    );
    let audio_codec = audio_streams
        .first()
        .and_then(|stream| stream["codec_name"].as_str())
        .map(str::to_owned);
    if source_audio_present {
        ensure!(
            audio_codec.as_deref() == Some("aac"),
            "proxy source audio must remain AAC, got {}",
            audio_codec.as_deref().unwrap_or("none")
        );
    }

    decode_proxy(ffmpeg, path, expected_duration_seconds, is_cancelled)?;
    Ok(ProxyOutputValidation {
        codec: codec.to_string(),
        width: width as u32,
        height: height as u32,
        pixel_format: pixel_format.to_string(),
        profile: video["profile"].as_str().map(str::to_owned),
        duration_seconds: duration,
        expected_duration_seconds,
        duration_tolerance_seconds: tolerance,
        source_audio_present,
        audio_stream_count: audio_streams.len(),
        audio_codec,
        metadata_rotation_degrees: rotation,
        decoded: true,
        time_correspondence: None,
    })
}

/// Probe whether a source has at least one audio stream. Used to make the
/// generated proxy's audio correspondence explicit before encoding starts.
pub fn probe_source_audio_present(ffprobe: &Path, path: &Path) -> Result<bool> {
    let document = ffprobe_document(ffprobe, path)?;
    Ok(document["streams"]
        .as_array()
        .map(|streams| streams.iter().any(|stream| stream["codec_type"] == "audio"))
        .unwrap_or(false))
}

fn numeric_field(stream: &Value, field: &str) -> Option<f64> {
    stream[field]
        .as_str()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
}

fn validate_source_correspondence(
    ffprobe: &Path,
    source: &Path,
    output: &Path,
) -> Result<ProxyTimeCorrespondence> {
    let source = ffprobe_document(ffprobe, source)?;
    let output = ffprobe_document(ffprobe, output)?;
    let source_video = crate::require_video_stream(&source)?;
    let output_video = crate::require_video_stream(&output)?;
    let frames = |stream: &Value| {
        stream["nb_frames"]
            .as_str()
            .and_then(|value| value.parse::<u64>().ok())
    };
    let source_frames = frames(source_video);
    let output_frames = frames(output_video);
    if let (Some(expected), Some(actual)) = (source_frames, output_frames) {
        ensure!(
            expected == actual,
            "proxy/source frame count mismatch: source {expected}, output {actual}"
        );
    }
    if let (Some(expected), Some(actual)) = (
        numeric_field(source_video, "duration"),
        numeric_field(output_video, "duration"),
    ) {
        ensure!(
            (expected - actual).abs() <= 0.1,
            "proxy/source video duration mismatch"
        );
    }
    let first_audio = |document: &Value| {
        document["streams"]
            .as_array()
            .and_then(|streams| {
                streams
                    .iter()
                    .find(|stream| stream["codec_type"] == "audio")
            })
            .cloned()
    };
    let source_audio = first_audio(&source);
    let output_audio = first_audio(&output);
    let mut evidence = ProxyTimeCorrespondence {
        source_video_frames: source_frames,
        output_video_frames: output_frames,
        source_audio_offset_seconds: None,
        output_audio_offset_seconds: None,
        audio_duration_difference_seconds: None,
    };
    if let (Some(source_audio), Some(output_audio)) = (source_audio, output_audio) {
        evidence.source_audio_offset_seconds = numeric_field(&source_audio, "start_time")
            .zip(numeric_field(source_video, "start_time"))
            .map(|(audio, video)| audio - video);
        evidence.output_audio_offset_seconds = numeric_field(&output_audio, "start_time")
            .zip(numeric_field(output_video, "start_time"))
            .map(|(audio, video)| audio - video);
        if let (Some(expected), Some(actual)) = (
            evidence.source_audio_offset_seconds,
            evidence.output_audio_offset_seconds,
        ) {
            ensure!(
                (expected - actual).abs() <= 0.1,
                "proxy/source audio timing offset mismatch"
            );
        }
        evidence.audio_duration_difference_seconds = numeric_field(&source_audio, "duration")
            .zip(numeric_field(&output_audio, "duration"))
            .map(|(source, output)| output - source);
        if let Some(difference) = evidence.audio_duration_difference_seconds {
            ensure!(
                difference.abs() <= 0.1,
                "proxy/source audio duration mismatch"
            );
        }
    }
    Ok(evidence)
}

fn ffprobe_document(ffprobe: &Path, path: &Path) -> Result<Value> {
    let mut command = Command::new(ffprobe);
    command.args([
        "-v",
        "error",
        "-show_entries",
        "format=duration:stream=codec_type,codec_name,profile,width,height,pix_fmt,duration,r_frame_rate,avg_frame_rate,start_time,nb_frames:stream_tags=rotate:stream_side_data=rotation",
        "-of",
        "json",
    ]);
    command.arg(path);
    let output = output_with_timeout(
        &mut command,
        &format!("ffprobe on proxy {}", path.display()),
        VALIDATION_TIMEOUT,
    )?;
    if !output.status.success() {
        bail!(
            "ffprobe rejected generated proxy: {}",
            bound_diagnostic(&String::from_utf8_lossy(&output.stderr))
        );
    }
    serde_json::from_slice(&output.stdout).context("parse generated proxy ffprobe JSON")
}

fn decode_proxy(
    ffmpeg: &Path,
    path: &Path,
    duration_seconds: f64,
    is_cancelled: &impl Fn() -> bool,
) -> Result<()> {
    let mut command = Command::new(ffmpeg);
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-xerror",
        "-err_detect",
        "explode",
        "-nostdin",
        "-i",
    ]);
    command
        .arg(path)
        .args(["-map", "0:v:0", "-map", "0:a:0?", "-f", "null", "-"]);
    // Full-clip decoding can legitimately outlive a small metadata probe.
    let limit = Duration::from_secs_f64(duration_seconds.clamp(120.0, 86400.0));
    let output = crate::stall::output_with_timeout_and_cancellation(
        &mut command,
        "decode generated proxy",
        limit,
        is_cancelled,
    )?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure!(
        output.status.success(),
        "generated proxy failed decode validation: {}",
        bound_diagnostic(&stderr)
    );
    ensure!(
        stderr.trim().is_empty(),
        "generated proxy failed decode validation despite exit 0: {}",
        bound_diagnostic(&stderr)
    );
    Ok(())
}

fn software_capability(ffmpeg: &Path) -> EncoderCapability {
    let encoder_present = ffmpeg_advertises_encoder(ffmpeg, "libx264");
    EncoderCapability {
        encoder: "libx264".to_string(),
        device: None,
        device_present: true,
        device_accessible: Some(true),
        ffmpeg_encoder_present: encoder_present,
        initialization_tested: false,
        usable: encoder_present,
        status: if encoder_present {
            "usable".to_string()
        } else {
            "ffmpeg_encoder_unavailable".to_string()
        },
        detail: if encoder_present {
            "software fallback encoder is advertised by the configured FFmpeg".to_string()
        } else {
            "the configured FFmpeg does not advertise libx264; install an FFmpeg build with libx264 before generating proxies".to_string()
        },
    }
}

fn ffmpeg_advertises_encoder(ffmpeg: &Path, encoder: &str) -> bool {
    let mut command = Command::new(ffmpeg);
    command.args(["-hide_banner", "-encoders"]);
    match output_with_timeout(&mut command, "list ffmpeg encoders", PROBE_TIMEOUT) {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.split_whitespace().any(|field| field == encoder)),
        _ => false,
    }
}

#[cfg(target_os = "linux")]
fn linux_render_devices() -> Vec<PathBuf> {
    use std::os::unix::fs::FileTypeExt;
    let Ok(entries) = fs::read_dir("/dev/dri") else {
        return Vec::new();
    };
    let mut devices = entries
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("renderD"))
        .filter(|entry| {
            entry
                .file_type()
                .map(|kind| kind.is_char_device())
                .unwrap_or(false)
        })
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    devices.sort();
    devices
}

#[cfg(target_os = "linux")]
fn device_accessible(device: &Path) -> bool {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(device)
        .is_ok()
}

#[cfg(target_os = "linux")]
fn probe_directory() -> Option<PathBuf> {
    let directory = std::env::temp_dir().join(format!(
        "rcdvlog-vaapi-probe-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&directory).ok()?;
    Some(directory)
}

#[cfg(target_os = "linux")]
fn create_probe_source(ffmpeg: &Path, path: &Path) -> Result<()> {
    let mut command = Command::new(ffmpeg);
    command.args([
        "-y",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=320x180:rate=10:duration=0.2",
        "-c:v",
        "libx264",
        "-pix_fmt",
        "yuv420p",
        "-an",
    ]);
    command.arg(path);
    let output = output_with_timeout(
        &mut command,
        "create bounded VAAPI probe source",
        PROBE_TIMEOUT,
    )?;
    ensure!(
        output.status.success(),
        "synthetic probe source generation failed: {}",
        bound_diagnostic(&String::from_utf8_lossy(&output.stderr))
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_hardware_probe(ffmpeg: &Path, ffprobe: &Path, source: &Path, device: &Path) -> Result<()> {
    let output_path = source.with_file_name("synthetic-vaapi-output.mp4");
    let device = device.display().to_string();
    let mut command = Command::new(ffmpeg);
    command.args([
        "-y",
        "-hide_banner",
        "-loglevel",
        "error",
        "-init_hw_device",
    ]);
    command
        .arg(format!("vaapi=va:{device}"))
        .args(["-filter_hw_device", "va"])
        .args(["-hwaccel", "vaapi"])
        .args(["-hwaccel_device", &device])
        .args(["-hwaccel_output_format", "vaapi"])
        .args(["-display_rotation:v:0", "0", "-noautorotate", "-i"])
        .arg(source)
        .args([
            "-map",
            "0:v:0",
            "-vf",
            "scale_vaapi=w='min(1280,iw)':h=-2:format=nv12",
            "-c:v",
            "h264_vaapi",
            "-profile:v",
            "high",
            "-qp",
            "23",
            "-an",
        ])
        .arg(&output_path);
    let output = output_with_timeout(&mut command, "bounded VAAPI encoder probe", PROBE_TIMEOUT)?;
    ensure!(
        output.status.success(),
        "ffmpeg exited {}: {}",
        output.status,
        bound_diagnostic(&String::from_utf8_lossy(&output.stderr))
    );
    let document = crate::probe_media_document_with(&output_path, ffprobe)?;
    let video = crate::require_video_stream(&document)?;
    ensure!(
        video["codec_name"].as_str() == Some("h264"),
        "VAAPI probe output was not H.264"
    );
    decode_proxy(ffmpeg, &output_path, 0.2, &|| false)?;
    let _ = fs::remove_file(&output_path);
    Ok(())
}

fn probe_limitations() -> Vec<String> {
    vec![
        "Hardware probes use one bounded generated clip; they do not certify every source codec, resolution, color format, or concurrent workload.".to_string(),
        "Throughput observations describe only successful work on this host and catalog; they do not generalize to another corpus, driver, or worker count.".to_string(),
        "Hardware acceptance is explicitly unavailable when no render device initializes; software remains the measured fallback.".to_string(),
    ]
}

/// Versioned ETA-history key for the encoder that actually succeeded and the
/// actual worker count. New macOS keys avoid reusing the old observations
/// that mixed successful hardware and software fallback work.
pub fn proxy_rate_history_key(encoder: ProxyEncoderKind, worker_count: usize) -> String {
    #[cfg(target_os = "macos")]
    {
        format!(
            "{}.macos-{}-w{}-v2",
            PROXY_SOFTWARE_RECIPE_KEY,
            encoder.as_str(),
            worker_count.max(1)
        )
    }
    #[cfg(target_os = "linux")]
    {
        match encoder {
            ProxyEncoderKind::Software => format!(
                "proxy.h264-720p.orientation-v3.linux-software-w{}-v1",
                worker_count.max(1)
            ),
            ProxyEncoderKind::Vaapi => format!(
                "proxy.h264-720p.orientation-v3.linux-vaapi-qp23-w{}-v1",
                worker_count.max(1)
            ),
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (encoder, worker_count);
        "proxy.h264-720p.orientation-v3".to_string()
    }
}

fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn redact_probe_error(value: &str) -> String {
    let redacted = crate::redact_media_diagnostic(
        value,
        Path::new("/nonexistent-probe-source"),
        Path::new("/nonexistent-probe-output"),
    );
    bound_diagnostic(redacted.trim_start_matches(": "))
}

fn bound_diagnostic(value: &str) -> String {
    let trimmed = value.trim();
    let mut end = trimmed.len().min(MAX_DIAGNOSTIC_CHARS);
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    trimmed[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn tools_available() -> bool {
        crate::tool_command("ffmpeg")
            .arg("-version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
            && crate::tool_command("ffprobe")
                .arg("-version")
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false)
    }

    fn make_source_with_audio(directory: &Path) -> PathBuf {
        let unrotated = directory.join("unrotated.mp4");
        let rotated = directory.join("source.mp4");
        let status = crate::tool_command("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x360:rate=24:duration=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(&unrotated)
            .status()
            .unwrap();
        assert!(status.success(), "synthetic source generation failed");
        let status = crate::tool_command("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-display_rotation:v:0",
                "90",
                "-i",
            ])
            .arg(&unrotated)
            .args(["-c", "copy"])
            .arg(&rotated)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "synthetic rotated source generation failed"
        );
        rotated
    }

    fn software_plan() -> ProxyEncoderPlan {
        ProxyEncoderPlan {
            policy: ProxyEncoderPolicy::Software,
            preferred_encoder: ProxyEncoderKind::Software,
            targets: vec![ProxyEncoderTarget::software()],
            hardware_unavailable: None,
            software_usable: true,
            limitations: vec![],
        }
    }

    #[cfg(unix)]
    fn fake_ffmpeg(directory: &Path, name: &str, behavior: &str) -> PathBuf {
        let real = crate::resolve_tool_path("ffmpeg");
        let path = directory.join(name);
        let script = format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = h264_vaapi ]; then\n{behavior}\n  fi\ndone\nexec \"{}\" \"$@\"\n",
            real.display()
        );
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(script.as_bytes()).unwrap();
        let mut permissions = file.metadata().unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_kills_a_silent_encoder_and_does_not_start_fallback() {
        let directory = TempDir::new().unwrap();
        let script = directory.path().join("silent-encoder");
        fs::write(&script, "#!/bin/sh\nexec sleep 30\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let output = directory.path().join("canceled.mp4");
        let mut plan = software_plan();
        // A cancellation must suppress even an otherwise valid fallback.
        plan.targets.push(ProxyEncoderTarget::software());
        let started = Instant::now();
        let failure = run_proxy_transcode_with_plan_and_cancellation(
            &script,
            Path::new("unused-ffprobe"),
            Path::new("unused-source"),
            &output,
            1.0,
            false,
            &plan,
            |_| {},
            || started.elapsed() >= Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(failure.message.contains("canceled"));
        assert_eq!(failure.outcome.attempts.len(), 1);
        assert!(!output.exists());
    }

    #[test]
    fn damaged_h264_packets_fail_full_decode_despite_valid_headers() {
        if !tools_available() {
            return;
        }
        let directory = TempDir::new().unwrap();
        make_source_with_audio(directory.path());
        let source = directory.path().join("unrotated.mp4");
        let ffmpeg = crate::resolve_tool_path("ffmpeg");
        let ffprobe = crate::resolve_tool_path("ffprobe");
        let packets = Command::new(&ffprobe)
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_packets",
                "-show_entries",
                "packet=pos,size",
                "-of",
                "json",
            ])
            .arg(&source)
            .output()
            .unwrap();
        let packets: Value = serde_json::from_slice(&packets.stdout).unwrap();
        let packet = &packets["packets"][1];
        let position: usize = packet["pos"].as_str().unwrap().parse().unwrap();
        let size: usize = packet["size"].as_str().unwrap().parse().unwrap();
        let mut bytes = fs::read(&source).unwrap();
        // Corrupt a complete noninitial AVCC sample, including its NAL length
        // fields. The moov headers remain readable, but the video bitstream
        // cannot be decoded.
        bytes[position..position + size].fill(0xff);
        let damaged = directory.path().join("damaged.mp4");
        fs::write(&damaged, bytes).unwrap();
        assert_eq!(
            crate::require_video_stream(&ffprobe_document(&ffprobe, &damaged).unwrap()).unwrap()["codec_name"],
            "h264"
        );
        let error = validate_proxy_output(&ffmpeg, &ffprobe, &damaged, 1.0, true).unwrap_err();
        assert!(format!("{error:#}").contains("decode"), "{error:#}");
    }

    #[test]
    fn truncated_audio_is_rejected_even_when_video_and_audio_headers_are_valid() {
        if !tools_available() {
            return;
        }
        let directory = TempDir::new().unwrap();
        let source = make_source_with_audio(directory.path());
        let output = directory.path().join("short-audio.mp4");
        let ffprobe = crate::resolve_tool_path("ffprobe");
        let status = crate::tool_command("ffmpeg")
            .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
            .arg(&source)
            .args([
                "-display_rotation:v:0",
                "0",
                "-f",
                "lavfi",
                "-i",
                "sine=duration=0.2",
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-c:v",
                "libx264",
                "-c:a",
                "aac",
                "-map_metadata",
                "-1",
            ])
            .arg(&output)
            .status()
            .unwrap();
        assert!(status.success());
        let error = validate_source_correspondence(&ffprobe, &source, &output).unwrap_err();
        assert!(format!("{error:#}").contains("audio duration"), "{error:#}");
    }

    #[test]
    fn policy_parsing_is_explicit_and_rejects_unknown_values() {
        assert_eq!(
            ProxyEncoderPolicy::parse("auto").unwrap(),
            ProxyEncoderPolicy::Auto
        );
        assert_eq!(
            ProxyEncoderPolicy::parse("software").unwrap(),
            ProxyEncoderPolicy::Software
        );
        assert!(ProxyEncoderPolicy::parse("vaapi").is_err());
        assert_eq!(ProxyEncoderPolicy::default(), ProxyEncoderPolicy::Auto);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rate_history_keys_separate_actual_encoders_and_worker_counts() {
        assert_eq!(
            proxy_rate_history_key(ProxyEncoderKind::Software, 2),
            "proxy.h264-720p.orientation-v3.linux-software-w2-v1"
        );
        assert_eq!(
            proxy_rate_history_key(ProxyEncoderKind::Vaapi, 1),
            "proxy.h264-720p.orientation-v3.linux-vaapi-qp23-w1-v1"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn vaapi_recipe_uses_one_device_for_hardware_decode_scale_and_encode() {
        let target = ProxyEncoderTarget::vaapi("/dev/dri/renderD128".to_string());
        let command = proxy_ffmpeg_command_with_program(
            Path::new("ffmpeg"),
            Path::new("source.mp4"),
            Path::new("proxy.mp4"),
            &target,
        );
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-init_hw_device", "vaapi=va:/dev/dri/renderD128"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-filter_hw_device", "va"])
        );
        assert!(args.windows(2).any(|pair| pair == ["-hwaccel", "vaapi"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-hwaccel_output_format", "vaapi"])
        );
        assert!(args.windows(2).any(|pair| pair == ["-c:v", "h264_vaapi"]));
        assert!(args.iter().any(|arg| arg.starts_with("scale_vaapi=")));
        assert!(args.windows(2).any(|pair| pair == ["-map", "0:a:0?"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-display_rotation:v:0", "0"])
        );
        assert!(args.iter().any(|arg| arg == "-noautorotate"));
        assert!(args.windows(2).any(|pair| pair == ["-map_metadata", "-1"]));
    }

    #[test]
    fn software_smoke_validates_source_time_audio_and_raw_orientation() {
        if !tools_available() {
            eprintln!("ffmpeg or ffprobe unavailable; skipping software proxy smoke test");
            return;
        }
        let directory = TempDir::new().unwrap();
        let source = make_source_with_audio(directory.path());
        let output = directory.path().join("proxy.mp4");
        let ffmpeg = crate::resolve_tool_path("ffmpeg");
        let ffprobe = crate::resolve_tool_path("ffprobe");
        let source_audio = probe_source_audio_present(&ffprobe, &source).unwrap();
        assert!(source_audio);
        let outcome = run_proxy_transcode_with_plan(
            &ffmpeg,
            &ffprobe,
            &source,
            &output,
            1.0,
            source_audio,
            &software_plan(),
            |_| {},
        )
        .unwrap();
        assert_eq!(outcome.selected_encoder, "software");
        assert_eq!(outcome.recipe_key, PROXY_SOFTWARE_RECIPE_KEY);
        let validation = outcome.validation.as_ref().unwrap();
        assert!(validation.decoded);
        assert_eq!(validation.metadata_rotation_degrees, 0);
        assert_eq!(validation.audio_stream_count, 1);
        assert_eq!(validation.audio_codec.as_deref(), Some("aac"));
        assert!((validation.duration_seconds - 1.0).abs() <= validation.duration_tolerance_seconds);
        assert!(output.is_file());
        assert!(probe_source_audio_present(&ffprobe, &output).unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn usable_linux_hardware_runs_the_full_validated_recipe_on_generated_media() {
        if !tools_available() {
            return;
        }
        clear_proxy_encoder_probe_cache();
        let probe = probe_proxy_encoder_capabilities(ProxyEncoderPolicy::Auto);
        let Some(hardware) = probe.selected_hardware_target() else {
            eprintln!("VAAPI hardware unavailable; hardware smoke coverage explicitly skipped");
            return;
        };
        let directory = TempDir::new().unwrap();
        let source = make_source_with_audio(directory.path());
        let output = directory.path().join("hardware-proxy.mp4");
        let plan = ProxyEncoderPlan {
            policy: ProxyEncoderPolicy::Auto,
            preferred_encoder: ProxyEncoderKind::Vaapi,
            targets: vec![hardware, ProxyEncoderTarget::software()],
            hardware_unavailable: None,
            software_usable: probe.software.usable,
            limitations: probe.limitations,
        };
        let outcome = run_proxy_transcode_with_plan(
            &crate::resolve_tool_path("ffmpeg"),
            &crate::resolve_tool_path("ffprobe"),
            &source,
            &output,
            1.0,
            true,
            &plan,
            |_| {},
        )
        .unwrap();
        assert_eq!(outcome.selected_encoder, "vaapi");
        assert_eq!(outcome.recipe_key, PROXY_VAAPI_RECIPE_KEY);
        assert!(!outcome.fallback_used);
        let validation = outcome.validation.as_ref().unwrap();
        assert!(validation.decoded);
        assert_eq!(validation.audio_stream_count, 1);
        assert_eq!(validation.metadata_rotation_degrees, 0);
        assert!(output.is_file());
    }

    #[test]
    fn hardware_probe_never_claims_usable_without_initialization() {
        #[cfg(target_os = "linux")]
        {
            clear_proxy_encoder_probe_cache();
            let report = probe_proxy_encoder_capabilities(ProxyEncoderPolicy::Auto);
            assert_eq!(report.platform, "linux");
            assert!(!report.hardware.is_empty());
            for candidate in &report.hardware {
                if candidate.usable {
                    assert!(candidate.initialization_tested);
                    assert_eq!(candidate.device_accessible, Some(true));
                    assert_eq!(candidate.status, "usable");
                } else {
                    assert!(!candidate.status.is_empty());
                    assert!(!candidate.detail.is_empty());
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let report = probe_proxy_encoder_capabilities(ProxyEncoderPolicy::Software);
            assert_eq!(report.selected_encoder, "software");
            assert_eq!(report.hardware[0].status, "not_requested");
        }
    }

    #[test]
    fn non_h264_hardware_output_is_rejected_by_strict_validation() {
        if !tools_available() {
            return;
        }
        let directory = TempDir::new().unwrap();
        let source = directory.path().join("source.mp4");
        let status = crate::tool_command("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=navy:s=320x180:r=10:d=1",
                "-c:v",
                "mpeg4",
            ])
            .arg(&source)
            .status()
            .unwrap();
        assert!(status.success());
        let error = validate_proxy_output(
            &crate::resolve_tool_path("ffmpeg"),
            &crate::resolve_tool_path("ffprobe"),
            &source,
            1.0,
            false,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("h264"));
    }

    #[cfg(unix)]
    #[test]
    fn failed_hardware_process_removes_staging_and_falls_back_to_software() {
        if !tools_available() {
            return;
        }
        let directory = TempDir::new().unwrap();
        let source = make_source_with_audio(directory.path());
        let output = directory.path().join("proxy.mp4");
        let ffmpeg = fake_ffmpeg(
            directory.path(),
            "fake-ffmpeg-process-failure",
            "echo 'synthetic VAAPI device initialization failed' >&2\n    exit 1",
        );
        let ffprobe = crate::resolve_tool_path("ffprobe");
        let plan = ProxyEncoderPlan {
            policy: ProxyEncoderPolicy::Auto,
            preferred_encoder: ProxyEncoderKind::Vaapi,
            targets: vec![
                ProxyEncoderTarget::vaapi("/dev/dri/renderD128".to_string()),
                ProxyEncoderTarget::software(),
            ],
            hardware_unavailable: None,
            software_usable: true,
            limitations: vec![],
        };
        let outcome = run_proxy_transcode_with_plan(
            &ffmpeg,
            &ffprobe,
            &source,
            &output,
            1.0,
            true,
            &plan,
            |_| {},
        )
        .unwrap();
        assert_eq!(outcome.selected_encoder, "software");
        assert_eq!(outcome.recipe_key, PROXY_SOFTWARE_RECIPE_KEY);
        assert!(outcome.fallback_used);
        assert!(outcome.attempts.iter().any(|attempt| {
            attempt.encoder == "vaapi"
                && attempt.status == ProxyAttemptStatus::ProcessFailed
                && attempt
                    .diagnostic
                    .as_deref()
                    .is_some_and(|value| value.contains("synthetic VAAPI"))
        }));
        assert!(output.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn invalid_hardware_output_is_removed_and_software_retry_is_validated() {
        if !tools_available() {
            return;
        }
        let directory = TempDir::new().unwrap();
        let source = make_source_with_audio(directory.path());
        let output = directory.path().join("proxy.mp4");
        let ffmpeg = fake_ffmpeg(
            directory.path(),
            "fake-ffmpeg-invalid-output",
            "printf 'not media' > \"$last\"\n    exit 0",
        );
        // The generic behavior above does not know the output argument. Replace
        // it with a script that finds the final argument and writes invalid bytes.
        let real = crate::resolve_tool_path("ffmpeg");
        let script = format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = h264_vaapi ]; then\n    for last; do :; done\n    printf 'not media' > \"$last\"\n    exit 0\n  fi\ndone\nexec \"{}\" \"$@\"\n",
            real.display()
        );
        fs::write(&ffmpeg, script).unwrap();
        let mut permissions = fs::metadata(&ffmpeg).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&ffmpeg, permissions).unwrap();
        let ffprobe = crate::resolve_tool_path("ffprobe");
        let plan = ProxyEncoderPlan {
            policy: ProxyEncoderPolicy::Auto,
            preferred_encoder: ProxyEncoderKind::Vaapi,
            targets: vec![
                ProxyEncoderTarget::vaapi("/dev/dri/renderD128".to_string()),
                ProxyEncoderTarget::software(),
            ],
            hardware_unavailable: None,
            software_usable: true,
            limitations: vec![],
        };
        let outcome = run_proxy_transcode_with_plan(
            &ffmpeg,
            &ffprobe,
            &source,
            &output,
            1.0,
            true,
            &plan,
            |_| {},
        )
        .unwrap();
        assert_eq!(outcome.selected_encoder, "software");
        assert!(outcome.fallback_used);
        assert!(outcome.attempts.iter().any(|attempt| {
            attempt.encoder == "vaapi" && attempt.status == ProxyAttemptStatus::ValidationFailed
        }));
        assert!(output.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn software_only_failure_records_no_fallback_claim() {
        if !tools_available() {
            return;
        }
        let directory = TempDir::new().unwrap();
        let source = make_source_with_audio(directory.path());
        let output = directory.path().join("proxy.mp4");
        let ffmpeg = directory.path().join("fake-software-fail");
        fs::write(&ffmpeg, "#!/bin/sh\nexit 1\n").unwrap();
        let mut permissions = fs::metadata(&ffmpeg).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&ffmpeg, permissions).unwrap();
        let failure = run_proxy_transcode_with_plan(
            &ffmpeg,
            &crate::resolve_tool_path("ffprobe"),
            &source,
            &output,
            1.0,
            true,
            &software_plan(),
            |_| {},
        )
        .unwrap_err();
        assert_eq!(failure.outcome.status, "failed");
        assert!(!failure.outcome.fallback_used);
        assert_eq!(failure.outcome.attempts.len(), 1);
        assert!(!output.exists());
    }

    #[cfg(unix)]
    #[test]
    fn both_encoder_failures_leave_no_staging_output() {
        if !tools_available() {
            return;
        }
        let directory = TempDir::new().unwrap();
        let source = make_source_with_audio(directory.path());
        let output = directory.path().join("proxy.mp4");
        let real = crate::resolve_tool_path("ffmpeg");
        let ffmpeg = directory.path().join("fake-ffmpeg-all-fail");
        fs::write(
            &ffmpeg,
            format!(
                "#!/bin/sh\necho 'hardware and software failed' >&2\nexit 1\n# {}\n",
                real.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&ffmpeg).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&ffmpeg, permissions).unwrap();
        let ffprobe = crate::resolve_tool_path("ffprobe");
        let plan = ProxyEncoderPlan {
            policy: ProxyEncoderPolicy::Auto,
            preferred_encoder: ProxyEncoderKind::Vaapi,
            targets: vec![
                ProxyEncoderTarget::vaapi("/dev/dri/renderD128".to_string()),
                ProxyEncoderTarget::software(),
            ],
            hardware_unavailable: None,
            software_usable: true,
            limitations: vec![],
        };
        let failure = run_proxy_transcode_with_plan(
            &ffmpeg,
            &ffprobe,
            &source,
            &output,
            1.0,
            true,
            &plan,
            |_| {},
        )
        .unwrap_err();
        assert_eq!(failure.outcome.status, "failed");
        assert_eq!(failure.outcome.recipe_key, PROXY_SOFTWARE_RECIPE_KEY);
        assert!(failure.outcome.fallback_used);
        assert_eq!(failure.outcome.attempts.len(), 2);
        assert!(!output.exists());
    }
}
