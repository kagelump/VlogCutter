//! Explicit, bounded host calibration for local-heavy resource settings.
//!
//! Calibration is opt-in. It runs generated synthetic media by default, or an
//! explicitly selected representative file, and never downloads models or
//! probes/benchmarks during readiness. Results are persisted in canonical
//! SQLite keyed by every identity component that can change the measurement.
//! A missing, interrupted, failed, stale, or differently configured result is
//! uncertainty: callers must not turn it into a completion estimate.

use crate::analysis::engine::{SttEngine, VAD_MODEL, WHISPER_MODEL};
use crate::analysis::whisper_engine::{WhisperEngine, read_pcm};
use crate::analysis::windows::SpeechRegion;
use crate::proxy_encoding::{
    PROXY_SOFTWARE_RECIPE_KEY, ProxyEncoderPlan, ProxyEncoderPolicy, ProxyGenerationOutcome,
    probe_source_audio_present, resolve_proxy_encoder_plan,
    run_proxy_transcode_with_plan_and_cancellation,
};
use crate::{
    BatchProgressEstimate, Catalog, ModelStatus, STT_RECIPE_KEY, STT_TASK_KEY, resolve_tool_path,
};
use anyhow::{Context, Result, anyhow, ensure};
use batch_estimator::{BatchEstimator, RateHistory};
use chrono::{SecondsFormat, Utc};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const RESOURCE_CALIBRATION_SCHEMA_VERSION: &str = "resource-calibration.v1";
pub const RESOURCE_CALIBRATION_RECIPE_VERSION: &str = "host-resource-calibration.v1";
pub const DEFAULT_GENERATED_CALIBRATION_SECONDS: u32 = 2;
pub const MAX_GENERATED_CALIBRATION_SECONDS: u32 = 30;
pub const MAX_CALIBRATION_TRIALS: u32 = 5;
pub const MAX_CALIBRATION_THREADS: usize = 64;
pub const MAX_CALIBRATION_WORKERS: usize = 4;
/// Cross-product guard so a CLI cannot turn an explicit calibration into an
/// unbounded benchmark by supplying large candidate lists.
pub const MAX_CALIBRATION_CANDIDATES: usize = 8;
pub const MAX_CALIBRATION_TOTAL_TRIALS: usize = 20;
/// Representative calibration media is a bounded pilot, not a full corpus
/// transcode. The byte bound is checked before hashing or PCM allocation.
pub const MAX_REPRESENTATIVE_MEDIA_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_REPRESENTATIVE_MEDIA_DURATION_MS: u32 = 30_000;
/// Speech VAD calibration deliberately fixes decode threads. Decode thread
/// tuning is only available as an explicit, uncalibrated override because a
/// VAD-only benchmark does not measure decode throughput or transcript quality.
pub const DEFAULT_DECODE_THREADS: usize = 4;

pub const PROXY_REPRESENTATION_KIND: &str = "generated_proxy";
pub const SPEECH_REPRESENTATION_KIND: &str = "speech_vad";

/// Immutable environment facts that qualify a throughput observation. Only
/// lightweight host/tool/device identity is collected here; constructing one
/// never probes an encoder or runs media.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MediaEstimatorEnvironment {
    pub schema_version: String,
    pub host: CalibrationHostIdentity,
    pub tools: Vec<CalibrationToolIdentity>,
    pub devices: Vec<CalibrationDeviceIdentity>,
}

impl MediaEstimatorEnvironment {
    /// Build the environment from the exact configured FFmpeg/FFprobe
    /// programs. Tool version discovery is bounded and cached; no media is
    /// transcoded or probed.
    pub fn from_tools(ffmpeg_program: &Path, ffprobe_program: &Path) -> Self {
        let ffmpeg = crate::resolve_tool_program(ffmpeg_program);
        let ffprobe = crate::resolve_tool_program(ffprobe_program);
        Self {
            schema_version: "media-estimator-environment.v1".into(),
            host: host_identity(),
            tools: vec![
                tool_identity_for_path("media_transcode", "ffmpeg", &ffmpeg),
                tool_identity_for_path("media_probe", "ffprobe", &ffprobe),
            ],
            devices: hardware_device_identities(),
        }
    }
}

/// Estimator-only identity for actual media work. It deliberately does not
/// participate in `RepresentationId` or the semantic representation recipe;
/// changing a host, tool, encoder, or worker setting invalidates only timing
/// reuse.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MediaEstimatorIdentity {
    pub schema_version: String,
    /// Semantic output kind, e.g. `generated_proxy`.
    pub representation_kind: String,
    /// Public representation recipe; remains stable when the encoder changes.
    pub semantic_recipe_key: String,
    /// Actual execution recipe selected for this attempt.
    pub execution_recipe_key: String,
    pub encoder: Option<String>,
    pub worker_count: usize,
    pub vad_threads: usize,
    pub decode_threads: usize,
    pub environment: MediaEstimatorEnvironment,
}

impl MediaEstimatorIdentity {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        environment: MediaEstimatorEnvironment,
        representation_kind: impl Into<String>,
        semantic_recipe_key: impl Into<String>,
        execution_recipe_key: impl Into<String>,
        encoder: Option<impl Into<String>>,
        worker_count: usize,
        vad_threads: usize,
        decode_threads: usize,
    ) -> Self {
        Self {
            schema_version: "media-estimator-identity.v1".into(),
            representation_kind: representation_kind.into(),
            semantic_recipe_key: semantic_recipe_key.into(),
            execution_recipe_key: execution_recipe_key.into(),
            encoder: encoder.map(Into::into),
            worker_count: worker_count.max(1),
            vad_threads,
            decode_threads,
            environment,
        }
    }

    fn configuration_key(&self) -> String {
        let configuration = serde_json::json!({
            "schemaVersion": self.schema_version,
            "representationKind": self.representation_kind,
            "semanticRecipeKey": self.semantic_recipe_key,
            "executionRecipeKey": self.execution_recipe_key,
            "encoder": self.encoder,
            "workerCount": self.worker_count,
            "vadThreads": self.vad_threads,
            "decodeThreads": self.decode_threads,
        });
        blake3::hash(&serde_json::to_vec(&configuration).expect("estimator config is serializable"))
            .to_hex()
            .to_string()
    }

    fn environment_key(environment: &MediaEstimatorEnvironment) -> String {
        blake3::hash(
            &serde_json::to_vec(environment).expect("estimator environment is serializable"),
        )
        .to_hex()
        .to_string()
    }

    /// Appends environment/configuration digests to the existing versioned
    /// execution key. The base keeps encoder/worker semantics readable.
    pub fn qualified_history_key(&self, base_key: &str) -> String {
        let environment = Self::environment_key(&self.environment);
        let configuration = self.configuration_key();
        format!(
            "{base_key}.h{}.c{}",
            &environment[..16],
            &configuration[..16]
        )
    }

    /// A persisted observation is reusable only when it was produced by the
    /// same immutable semantic output and the current exact environment.
    pub fn is_compatible_with(
        &self,
        environment: &MediaEstimatorEnvironment,
        semantic_recipe_key: &str,
    ) -> bool {
        self.schema_version == "media-estimator-identity.v1"
            && self.semantic_recipe_key == semantic_recipe_key
            && &self.environment == environment
    }
}

/// Opaque process-session identity used only to fence a live job attempt's
/// estimator context. New processes get a new key, so a lease left over from a
/// restarted server cannot keep advertising an ETA from the dead process.
pub fn media_estimator_process_key() -> &'static str {
    static KEY: OnceLock<String> = OnceLock::new();
    KEY.get_or_init(|| {
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let executable = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("unresolved"));
        let mut hasher = blake3::Hasher::new();
        hasher.update(std::process::id().to_le_bytes().as_slice());
        hasher.update(started.to_le_bytes().as_slice());
        hasher.update(executable.to_string_lossy().as_bytes());
        hasher.finalize().to_hex().to_string()[..32].to_string()
    })
}

/// Public wrapper for the host/build identity used by estimator keys.
pub fn calibration_host_identity() -> CalibrationHostIdentity {
    host_identity()
}

/// Public wrapper for the lightweight device identity bundle used by estimator
/// keys. It does not run encoder or media probes.
pub fn calibration_hardware_device_identities() -> Vec<CalibrationDeviceIdentity> {
    hardware_device_identities()
}

/// Identity for a configured media tool path. Bare names are resolved once so
/// a later `PATH` change cannot silently swap the measured executable.
pub fn calibration_tool_identity_for_path(
    role: &str,
    name: &str,
    program: &Path,
) -> CalibrationToolIdentity {
    let resolved = crate::resolve_tool_program(program);
    tool_identity_for_path(role, name, &resolved)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationHostIdentity {
    /// Stable opaque host/user identity. The raw hostname is never persisted.
    pub host_key: String,
    pub os: String,
    pub arch: String,
    pub logical_cpus: usize,
    pub application_build: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationToolIdentity {
    pub role: String,
    pub name: String,
    /// Bounded first version line; never a full path.
    pub version: String,
    /// Opaque digest of the resolved executable path.
    pub executable_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationDeviceIdentity {
    pub kind: String,
    pub name: String,
    pub identity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationModelIdentity {
    pub role: String,
    pub name: String,
    pub integrity: String,
    pub bytes: u64,
    /// Computed only by explicit calibration, never by status/selection.
    #[serde(default)]
    pub content_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationMediaIdentity {
    /// `generated` or `authorized_representative`.
    pub kind: String,
    /// Deterministic synthetic descriptor or full-content digest. Never a path.
    pub sample_identity: String,
    pub duration_ms: u32,
    pub width: u32,
    pub height: u32,
    pub has_audio: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationConditions {
    pub trials: u32,
    pub local_heavy_slots: usize,
    pub generated_duration_seconds: Option<u32>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationCandidateSettings {
    pub label: String,
    pub encoder: Option<String>,
    pub encoder_policy: Option<String>,
    pub worker_count: usize,
    pub vad_threads: usize,
    pub decode_threads: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationIdentity {
    pub schema_version: String,
    pub recipe_key: String,
    /// Version of the calibration recipe implementation. Changing measurement
    /// semantics must invalidate prior fits even when serialized schema is
    /// unchanged.
    #[serde(default)]
    pub recipe_version: String,
    pub representation_kind: String,
    pub host: CalibrationHostIdentity,
    pub tools: Vec<CalibrationToolIdentity>,
    pub devices: Vec<CalibrationDeviceIdentity>,
    pub models: Vec<CalibrationModelIdentity>,
    pub media: CalibrationMediaIdentity,
    pub conditions: CalibrationConditions,
    pub candidates: Vec<CalibrationCandidateSettings>,
    /// Stable digest of all non-environment configuration facts that affect
    /// interpretation of the candidates (policy, media contract, worker and
    /// thread sets). Kept explicit so callers can prove configuration match.
    #[serde(default)]
    pub configuration_key: String,
}

impl CalibrationIdentity {
    pub fn cache_key(&self) -> Result<String> {
        // Content evidence is recorded at explicit calibration time; the
        // robust metadata identity detects changes for lightweight reuse.
        let mut key_identity = self.clone();
        for model in &mut key_identity.models {
            model.content_digest = None;
        }
        Ok(blake3::hash(&serde_json::to_vec(&key_identity)?)
            .to_hex()
            .to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationTiming {
    pub samples_ms: Vec<u64>,
    pub minimum_ms: u64,
    pub maximum_ms: u64,
    pub median_ms: u64,
    pub mean_ms: f64,
    pub work_units_per_second: Option<f64>,
}

impl CalibrationTiming {
    fn from_samples(mut samples_ms: Vec<u64>, work_units: Option<u64>) -> Option<Self> {
        samples_ms.retain(|duration| *duration > 0);
        if samples_ms.is_empty() {
            return None;
        }
        samples_ms.sort_unstable();
        let minimum_ms = *samples_ms.first()?;
        let maximum_ms = *samples_ms.last()?;
        let median_ms = if samples_ms.len().is_multiple_of(2) {
            let high = samples_ms[samples_ms.len() / 2];
            let low = samples_ms[samples_ms.len() / 2 - 1];
            low.saturating_add(high) / 2
        } else {
            samples_ms[samples_ms.len() / 2]
        };
        let total = samples_ms.iter().copied().sum::<u64>() as f64;
        let mean_ms = total / samples_ms.len() as f64;
        let work_units_per_second = work_units.and_then(|work_units| {
            (work_units > 0 && mean_ms.is_finite() && mean_ms > 0.0)
                .then_some(work_units as f64 / (mean_ms / 1000.0))
        });
        Some(Self {
            samples_ms,
            minimum_ms,
            maximum_ms,
            median_ms,
            mean_ms,
            work_units_per_second,
        })
    }

    fn relative_spread(&self) -> f64 {
        if self.mean_ms <= 0.0 {
            return 0.0;
        }
        (self.maximum_ms.saturating_sub(self.minimum_ms)) as f64 / self.mean_ms
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpeechRegionDifference {
    pub index: usize,
    pub reference: Option<[u32; 2]>,
    pub candidate: Option<[u32; 2]>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpeechRegionComparison {
    pub reference_valid: bool,
    pub candidate_valid: bool,
    pub equivalent: bool,
    pub duration_ms: u32,
    pub reference_region_count: usize,
    pub candidate_region_count: usize,
    pub mismatch_count: usize,
    pub reference_digest: String,
    pub candidate_digest: String,
    pub reason: String,
    pub differences: Vec<SpeechRegionDifference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationCandidateOutcome {
    pub settings: CalibrationCandidateSettings,
    pub succeeded: bool,
    pub timing: Option<CalibrationTiming>,
    pub failures: Vec<String>,
    /// Output/quality identity. Generated media paths are never recorded.
    pub output_identity: Option<String>,
    /// Present for speech-region calibration.
    pub quality: Option<SpeechRegionComparison>,
    /// Present for generated-proxy calibration after existing output validation
    /// and source/render/audio contract comparison.
    pub render_contract_equivalent: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationQualityDecision {
    pub baseline_candidate: String,
    pub adopted_candidate: Option<String>,
    /// Equal detected regions (or equivalent validated render properties) are
    /// required before a faster candidate may replace the baseline.
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationUncertainty {
    /// `measured`, `low_sample_count`, or `none`.
    pub confidence: String,
    pub timing_relative_spread: Option<f64>,
    /// A low/high range is always reported for an adopted timing. A single
    /// sample uses a deliberately broad 25% band instead of a false point ETA.
    pub eta_relative_range: Option<f64>,
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalibrationState {
    Running,
    Complete,
    Failed,
    NotReady,
    Interrupted,
    Invalidated,
}

impl CalibrationState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::NotReady => "not_ready",
            Self::Interrupted => "interrupted",
            Self::Invalidated => "invalidated",
        }
    }
    fn parse(value: &str) -> Result<Self> {
        match value {
            "running" => Ok(Self::Running),
            "complete" => Ok(Self::Complete),
            "failed" => Ok(Self::Failed),
            "not_ready" => Ok(Self::NotReady),
            "interrupted" => Ok(Self::Interrupted),
            "invalidated" => Ok(Self::Invalidated),
            _ => Err(anyhow!("unknown calibration state {value}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceCalibrationReport {
    pub schema_version: String,
    pub cache_key: String,
    pub state: CalibrationState,
    pub identity: CalibrationIdentity,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub candidates: Vec<CalibrationCandidateOutcome>,
    pub selected_settings: Option<CalibrationCandidateSettings>,
    pub selection_reason: String,
    pub quality: Option<CalibrationQualityDecision>,
    pub uncertainty: CalibrationUncertainty,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationSummary {
    pub cache_key: String,
    pub recipe_key: String,
    pub representation_kind: String,
    pub media_kind: String,
    pub sample_identity: String,
    pub state: CalibrationState,
    pub selected_settings: Option<CalibrationCandidateSettings>,
    pub uncertainty: Option<CalibrationUncertainty>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
    pub invalidation_reason: Option<String>,
    /// Whether host/build/tools still match. Media and candidate identity are
    /// intentionally shown separately because a changed sample is a different
    /// cache entry rather than a silently reusable result.
    pub host_tools_match_current: bool,
    pub reusable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationStatusReport {
    pub schema_version: String,
    pub recipe_key: Option<String>,
    pub representation_kind: Option<String>,
    pub entries: Vec<CalibrationSummary>,
    pub note: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceOverrides {
    /// Explicit encoder override (`software`, `vaapi`, `videotoolbox`, ...).
    pub encoder: Option<String>,
    /// Internal worker count for a batch operation.
    pub worker_count: Option<usize>,
    /// VAD thread count. This does not imply one thread is universally best.
    pub vad_threads: Option<usize>,
    /// Whisper decode thread count.
    pub decode_threads: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationEta {
    pub status: String,
    pub estimated_remaining_seconds: Option<f64>,
    pub range_seconds: Option<[f64; 2]>,
    pub confidence: String,
    pub basis: String,
    pub explanation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSelection {
    pub representation_kind: String,
    pub recipe_key: String,
    pub source: String,
    pub state: String,
    pub encoder: Option<String>,
    pub worker_count: usize,
    pub vad_threads: usize,
    pub decode_threads: usize,
    pub calibration_key: Option<String>,
    pub timing: Option<CalibrationTiming>,
    pub uncertainty: CalibrationUncertainty,
    pub eta: CalibrationEta,
    /// Settings- and environment-qualified rate-history key. This never reuses
    /// a rate measured on another host/tool/model/device/configuration.
    pub history_key: String,
    /// `exact_representative`, `generated_contract_only`, or `unknown` for the
    /// calibration sample relative to the caller-supplied corpus identity.
    pub sample_compatibility: String,
}

#[derive(Debug, Clone)]
pub enum CalibrationMedia {
    /// Deterministic synthetic media made in managed scratch space and removed.
    Generated,
    /// Explicitly selected representative media. The paths are read but never
    /// persisted; only the full-content digest and technical properties are.
    Representative(PathBuf),
}

impl CalibrationMedia {
    fn kind(&self) -> &'static str {
        match self {
            Self::Generated => "generated",
            Self::Representative(_) => "authorized_representative",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProxyCalibrationOptions {
    pub media: CalibrationMedia,
    pub policy: ProxyEncoderPolicy,
    pub worker_counts: Vec<usize>,
    pub trials: u32,
    pub generated_duration_seconds: u32,
    /// Explicitly repeat a benchmark even when an exact completed cache entry
    /// exists. Without this flag completed exact matches are reused.
    pub force: bool,
}

impl Default for ProxyCalibrationOptions {
    fn default() -> Self {
        Self {
            media: CalibrationMedia::Generated,
            policy: ProxyEncoderPolicy::Auto,
            worker_counts: vec![1],
            trials: 1,
            generated_duration_seconds: DEFAULT_GENERATED_CALIBRATION_SECONDS,
            force: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SpeechCalibrationOptions {
    pub media: CalibrationMedia,
    pub vad_threads: Vec<usize>,
    /// Must contain only `DEFAULT_DECODE_THREADS`; decode throughput is not
    /// measured by VAD calibration.
    pub decode_threads: Vec<usize>,
    pub trials: u32,
    pub generated_duration_seconds: u32,
    pub force: bool,
}

impl Default for SpeechCalibrationOptions {
    fn default() -> Self {
        Self {
            media: CalibrationMedia::Generated,
            // Deliberately compare more than one setting. Neither one nor four
            // threads is assumed universally optimal.
            vad_threads: vec![1, 2, 4],
            decode_threads: vec![DEFAULT_DECODE_THREADS],
            trials: 1,
            generated_duration_seconds: DEFAULT_GENERATED_CALIBRATION_SECONDS,
            force: false,
        }
    }
}

fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn bounded_text(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn system_time_nanos(value: SystemTime) -> u128 {
    value
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

/// Opaque, stable-within-a-stat fingerprint. It is deliberately never exposed
/// as a path; callers retain only a digest. A replacement at the same path
/// changes length/mtime/inode and therefore invalidates cached identities.
fn file_stat_fingerprint(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok().map(system_time_nanos).unwrap_or(0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(format!(
            "dev={};ino={};len={};mtime={};mode={};ctime={}.{}",
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            modified,
            metadata.mode(),
            metadata.ctime(),
            metadata.ctime_nsec()
        ))
    }
    #[cfg(not(unix))]
    {
        Some(format!(
            "len={};mtime={};readonly={}",
            metadata.len(),
            modified,
            metadata.permissions().readonly()
        ))
    }
}

fn file_identity(path: &Path) -> String {
    let fingerprint = file_stat_fingerprint(path).unwrap_or_else(|| "unresolved".into());
    let mut hasher = blake3::Hasher::new();
    hasher.update(path.as_os_str().to_string_lossy().as_bytes());
    hasher.update(fingerprint.as_bytes());
    hasher.finalize().to_hex().to_string()[..32].to_string()
}

#[derive(Debug, Clone)]
struct CachedHostIdentity {
    fingerprint: String,
    identity: CalibrationHostIdentity,
}

fn actual_host_name() -> String {
    if let Ok(value) = std::env::var("RCDVLOG_HOST_ID") {
        return value;
    }
    #[cfg(unix)]
    {
        let mut name = [0_u8; 256];
        if unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) } == 0 {
            name[255] = 0;
            return String::from_utf8_lossy(
                &name[..name.iter().position(|byte| *byte == 0).unwrap_or(255)],
            )
            .into_owned();
        }
    }
    std::env::var("HOSTNAME")
        .unwrap_or_else(|_| format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH))
}

fn kernel_identity() -> String {
    #[cfg(unix)]
    {
        let mut value = std::mem::MaybeUninit::<libc::utsname>::uninit();
        if unsafe { libc::uname(value.as_mut_ptr()) } == 0 {
            let value = unsafe { value.assume_init() };
            return unsafe { std::ffi::CStr::from_ptr(value.release.as_ptr()) }
                .to_string_lossy()
                .into_owned();
        }
    }
    std::env::consts::OS.to_string()
}

fn platform_cpu_model() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        return crate::prerequisites::run_bounded_command(
            Path::new("/usr/sbin/sysctl"),
            &[OsString::from("-n"), OsString::from("hw.model")],
            Duration::from_secs(1),
            1024,
        )
        .ok()
        .filter(|result| result.status.success())
        .map(|result| String::from_utf8_lossy(&result.stdout).trim().to_string());
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

fn host_identity() -> CalibrationHostIdentity {
    static IDENTITY: OnceLock<Mutex<Option<CachedHostIdentity>>> = OnceLock::new();
    let executable = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("unresolved"));
    let raw_host = actual_host_name();
    let fingerprint = format!(
        "exe={};host={:?};build={:?};hostname={:?};epoch={:?};kernel={};cpus={}",
        file_stat_fingerprint(&executable).unwrap_or_else(|| "missing".into()),
        std::env::var("RCDVLOG_HOST_ID").ok(),
        std::env::var("RCDVLOG_BUILD_ID").ok(),
        raw_host,
        std::env::var("RCDVLOG_IDENTITY_EPOCH").ok(),
        kernel_identity(),
        std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1),
    );
    let cache = IDENTITY.get_or_init(|| Mutex::new(None));
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(cached) = guard
        .as_ref()
        .filter(|cached| cached.fingerprint == fingerprint)
    {
        return cached.identity.clone();
    }
    let host_key = blake3::hash(raw_host.trim().as_bytes())
        .to_hex()
        .to_string()[..32]
        .to_string();
    let build_marker = std::env::var("RCDVLOG_BUILD_ID")
        .ok()
        .or_else(|| option_env!("RCDVLOG_BUILD_ID").map(str::to_string))
        .unwrap_or_else(|| "local".into());
    let executable_key = file_identity(&executable);
    let identity = CalibrationHostIdentity {
        host_key,
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        logical_cpus: std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1),
        // Include the actual running executable's file identity. `local`
        // alone would let two different local builds reuse each other's fit.
        application_build: format!(
            "{}+{}+exe-{}",
            env!("CARGO_PKG_VERSION"),
            build_marker,
            &executable_key[..16]
        ),
    };
    *guard = Some(CachedHostIdentity {
        fingerprint,
        identity: identity.clone(),
    });
    identity
}

#[derive(Debug, Clone)]
struct CachedToolIdentity {
    fingerprint: String,
    identity: CalibrationToolIdentity,
}

fn tool_identity(role: &str, name: &str) -> CalibrationToolIdentity {
    let path = resolve_tool_path(name);
    tool_identity_for_path(role, name, &path)
}

fn tool_identity_for_path(role: &str, name: &str, path: &Path) -> CalibrationToolIdentity {
    static IDENTITIES: OnceLock<Mutex<HashMap<String, CachedToolIdentity>>> = OnceLock::new();
    let fingerprint = format!(
        "stat={};epoch={:?}",
        file_stat_fingerprint(path).unwrap_or_else(|| "missing".into()),
        std::env::var("RCDVLOG_IDENTITY_EPOCH").ok(),
    );
    let cache = IDENTITIES.get_or_init(|| Mutex::new(HashMap::new()));
    let key = format!("{role}:{name}:{}", path.to_string_lossy());
    {
        let guard = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cached) = guard
            .get(&key)
            .filter(|cached| cached.fingerprint == fingerprint)
        {
            return cached.identity.clone();
        }
    }
    let version = if path.is_file() {
        crate::prerequisites::run_bounded_command(
            path,
            &[OsString::from("-version")],
            Duration::from_secs(3),
            16 * 1024,
        )
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            let text = String::from_utf8_lossy(&output.stdout);
            bounded_text(
                text.lines().next().unwrap_or("version unavailable").trim(),
                256,
            )
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "version unavailable".to_string())
    } else {
        "unresolved".to_string()
    };
    let identity = CalibrationToolIdentity {
        role: role.to_string(),
        name: name.to_string(),
        version,
        executable_key: file_identity(path),
    };
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.len() >= 16 {
        guard.clear();
    }
    guard.insert(
        key,
        CachedToolIdentity {
            fingerprint,
            identity: identity.clone(),
        },
    );
    identity
}

fn tool_identities() -> Vec<CalibrationToolIdentity> {
    vec![
        tool_identity("media_transcode", "ffmpeg"),
        tool_identity("media_probe", "ffprobe"),
    ]
}

#[derive(Debug, Clone)]
struct CachedFileDigest {
    fingerprint: String,
    digest: Option<String>,
    bytes: u64,
}

fn model_content_digest(path: &Path) -> (Option<String>, u64) {
    static DIGESTS: OnceLock<Mutex<HashMap<PathBuf, CachedFileDigest>>> = OnceLock::new();
    let Some(fingerprint) = file_stat_fingerprint(path) else {
        return (None, 0);
    };
    let bytes = fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let cache = DIGESTS.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let guard = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cached) = guard
            .get(path)
            .filter(|cached| cached.fingerprint == fingerprint)
        {
            return (cached.digest.clone(), cached.bytes);
        }
    }
    let digest = crate::full_file_blake3(path, &|| false)
        .ok()
        .map(|hash| format!("blake3:{}", hash.hex_digest));
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.len() >= 64 {
        guard.clear();
    }
    guard.insert(
        path.to_path_buf(),
        CachedFileDigest {
            fingerprint,
            digest: digest.clone(),
            bytes,
        },
    );
    (digest, bytes)
}

fn model_identity(models: &[ModelStatus]) -> Vec<CalibrationModelIdentity> {
    models
        .iter()
        .map(|model| {
            let path = Path::new(&model.path);
            let bytes = fs::metadata(path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            CalibrationModelIdentity {
                role: model.role.clone(),
                name: model.name.clone(),
                integrity: format!("{};stat:{}", model.integrity, file_identity(path)),
                bytes,
                content_digest: None,
            }
        })
        .collect()
}

fn cpu_device_identity() -> CalibrationDeviceIdentity {
    let model = std::env::var("RCDVLOG_CPU_IDENTITY")
        .ok()
        .or_else(|| {
            fs::read_to_string("/proc/cpuinfo").ok().and_then(|text| {
                text.lines()
                    .find(|line| line.starts_with("model name"))
                    .and_then(|line| line.split_once(':'))
                    .map(|(_, value)| value.trim().to_string())
            })
        })
        .or_else(platform_cpu_model)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS));
    let identity = blake3::hash(
        format!(
            "{model}|{}|{}|{}|{}",
            kernel_identity(),
            std::env::consts::ARCH,
            std::env::consts::OS,
            std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1)
        )
        .as_bytes(),
    )
    .to_hex()
    .to_string()[..32]
        .to_string();
    CalibrationDeviceIdentity {
        kind: "cpu".into(),
        name: model,
        identity,
    }
}

fn gpu_device_identities() -> Vec<CalibrationDeviceIdentity> {
    if let Ok(raw) = std::env::var("RCDVLOG_GPU_IDENTITY") {
        let identity = blake3::hash(raw.as_bytes()).to_hex().to_string()[..32].to_string();
        return vec![CalibrationDeviceIdentity {
            kind: "gpu".into(),
            name: bounded_text(&raw, 128),
            identity,
        }];
    }
    let mut entries = Vec::new();
    if let Ok(paths) = fs::read_dir("/sys/class/drm") {
        for path in paths.flatten() {
            let name = path.file_name().to_string_lossy().to_string();
            if !name.starts_with("card") || name.contains('-') {
                continue;
            }
            let uevent = path.path().join("device/uevent");
            let Ok(text) = fs::read_to_string(uevent) else {
                continue;
            };
            let facts = text
                .lines()
                .filter(|line| {
                    line.starts_with("DRIVER=")
                        || line.starts_with("PCI_ID=")
                        || line.starts_with("PCI_SUBSYS_ID=")
                })
                .collect::<Vec<_>>()
                .join(";");
            if facts.is_empty() {
                continue;
            }
            let identity = blake3::hash(facts.as_bytes()).to_hex().to_string()[..32].to_string();
            entries.push(CalibrationDeviceIdentity {
                kind: "gpu".into(),
                name,
                identity,
            });
        }
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    entries
}

/// Lightweight hardware facts only. This function never probes an encoder or
/// runs media, so it is safe from read-only status and selection paths.
fn hardware_device_identities() -> Vec<CalibrationDeviceIdentity> {
    let mut devices = vec![cpu_device_identity()];
    let gpus = gpu_device_identities();
    #[cfg(target_os = "linux")]
    {
        let mut paths = vec![
            PathBuf::from("/usr/lib/x86_64-linux-gnu/dri"),
            PathBuf::from("/usr/lib/aarch64-linux-gnu/dri"),
            PathBuf::from("/usr/lib64/dri"),
            PathBuf::from("/usr/lib/dri"),
        ];
        if let Some(path) = std::env::var_os("LIBVA_DRIVERS_PATH") {
            paths.extend(std::env::split_paths(&path));
        }
        let mut files = Vec::new();
        for directory in paths {
            if let Ok(entries) = fs::read_dir(directory) {
                for entry in entries.flatten() {
                    if entry
                        .file_name()
                        .to_string_lossy()
                        .ends_with("_drv_video.so")
                    {
                        files.push(entry.path());
                    }
                }
            }
        }
        files.sort();
        files.dedup();
        let mut hasher = blake3::Hasher::new();
        for file in files {
            hasher.update(file_identity(&file).as_bytes());
        }
        devices.push(CalibrationDeviceIdentity {
            kind: "driver".into(),
            name: "vaapi-userspace".into(),
            identity: hasher.finalize().to_hex().to_string(),
        });
    }

    if gpus.is_empty() {
        devices.push(CalibrationDeviceIdentity {
            kind: "gpu".into(),
            name: "none".into(),
            identity: "none".into(),
        });
    } else {
        devices.extend(gpus);
    }
    devices
}

fn validate_trials(trials: u32) -> Result<()> {
    ensure!(
        (1..=MAX_CALIBRATION_TRIALS).contains(&trials),
        "invalid input: calibration trials must be 1..={MAX_CALIBRATION_TRIALS}"
    );
    Ok(())
}

fn validate_generated_duration(seconds: u32) -> Result<()> {
    ensure!(
        (1..=MAX_GENERATED_CALIBRATION_SECONDS).contains(&seconds),
        "invalid input: generated media duration must be 1..={MAX_GENERATED_CALIBRATION_SECONDS} seconds"
    );
    Ok(())
}

fn validate_threads(values: &[usize], label: &str) -> Result<()> {
    ensure!(
        !values.is_empty(),
        "invalid input: at least one {label} candidate is required"
    );
    ensure!(
        values
            .iter()
            .all(|value| (1..=MAX_CALIBRATION_THREADS).contains(value)),
        "invalid input: {label} must be 1..={MAX_CALIBRATION_THREADS}"
    );
    Ok(())
}

fn validate_worker_counts(values: &[usize]) -> Result<()> {
    ensure!(
        !values.is_empty(),
        "invalid input: at least one worker-count candidate is required"
    );
    ensure!(
        values
            .iter()
            .all(|value| (1..=MAX_CALIBRATION_WORKERS).contains(value)),
        "invalid input: worker counts must be 1..={MAX_CALIBRATION_WORKERS}"
    );
    Ok(())
}

fn validate_candidate_budget(candidate_count: usize, trials: u32) -> Result<()> {
    ensure!(
        candidate_count > 0 && candidate_count <= MAX_CALIBRATION_CANDIDATES,
        "invalid input: calibration candidate count must be 1..={MAX_CALIBRATION_CANDIDATES}, got {candidate_count}"
    );
    let total_trials = candidate_count.saturating_mul(trials as usize);
    ensure!(
        total_trials <= MAX_CALIBRATION_TOTAL_TRIALS,
        "invalid input: calibration would run {total_trials} trials; the bounded maximum is {MAX_CALIBRATION_TOTAL_TRIALS}"
    );
    Ok(())
}

fn validate_speech_decode_threads(values: &[usize]) -> Result<()> {
    ensure!(
        values == [DEFAULT_DECODE_THREADS],
        "invalid input: VAD calibration measures detection only; decodeThreads must be exactly [{DEFAULT_DECODE_THREADS}] (decode tuning requires an explicit uncalibrated override)"
    );
    Ok(())
}

fn validate_representative_media_metadata(path: &Path) -> Result<u64> {
    ensure!(
        path.is_file(),
        "authorized representative media does not exist or is unreadable"
    );
    let bytes = fs::metadata(path)
        .with_context(|| "read representative calibration media metadata")?
        .len();
    ensure!(
        bytes > 0 && bytes <= MAX_REPRESENTATIVE_MEDIA_BYTES,
        "authorized representative media is {bytes} bytes; calibration is bounded to {MAX_REPRESENTATIVE_MEDIA_BYTES} bytes"
    );
    Ok(bytes)
}

fn validate_representative_duration(duration_ms: u32) -> Result<()> {
    ensure!(
        duration_ms > 0 && duration_ms <= MAX_REPRESENTATIVE_MEDIA_DURATION_MS,
        "authorized representative media duration is {duration_ms}ms; calibration is bounded to {MAX_REPRESENTATIVE_MEDIA_DURATION_MS}ms"
    );
    Ok(())
}

fn calibration_configuration_key(
    media_kind: &str,
    candidates: &[CalibrationCandidateSettings],
    policy: Option<ProxyEncoderPolicy>,
) -> Result<String> {
    let value = serde_json::json!({
        "recipeVersion": RESOURCE_CALIBRATION_RECIPE_VERSION,
        "mediaKind": media_kind,
        "policy": policy.map(ProxyEncoderPolicy::as_str),
        "candidates": candidates,
    });
    Ok(blake3::hash(&serde_json::to_vec(&value)?)
        .to_hex()
        .to_string())
}

struct ScratchDirectory(PathBuf);
impl Drop for ScratchDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn create_scratch_root(catalog: &Catalog) -> Result<ScratchDirectory> {
    let parent = catalog
        .database_path()
        .parent()
        .ok_or_else(|| anyhow!("catalog has no parent directory"))?;
    let root = parent
        .join("cache")
        .join("calibration-staging")
        .join(uuid::Uuid::new_v4().simple().to_string());
    fs::create_dir_all(&root)
        .with_context(|| format!("create calibration scratch root {}", root.display()))?;
    Ok(ScratchDirectory(root))
}

fn run_ffmpeg_fixture(
    root: &Path,
    seconds: u32,
    with_audio: bool,
    audio_only: bool,
) -> Result<PathBuf> {
    let ffmpeg = resolve_tool_path("ffmpeg");
    let output = root.join(if audio_only {
        "calibration.wav"
    } else {
        "calibration.mp4"
    });
    let mut args = vec![
        OsString::from("-hide_banner"),
        OsString::from("-loglevel"),
        OsString::from("error"),
        OsString::from("-y"),
        OsString::from("-f"),
        OsString::from("lavfi"),
        OsString::from("-i"),
        OsString::from(if audio_only {
            "sine=frequency=440:sample_rate=16000".to_string()
        } else {
            "testsrc2=size=320x180:rate=30".to_string()
        }),
    ];
    if with_audio && !audio_only {
        args.extend([
            OsString::from("-f"),
            OsString::from("lavfi"),
            OsString::from("-i"),
            OsString::from("sine=frequency=440:sample_rate=48000"),
        ]);
    }
    args.extend([
        OsString::from("-t"),
        OsString::from(seconds.to_string()),
        OsString::from("-c:v"),
        OsString::from("mpeg4"),
        OsString::from("-q:v"),
        OsString::from("5"),
    ]);
    if with_audio && !audio_only {
        args.extend([
            OsString::from("-c:a"),
            OsString::from("aac"),
            OsString::from("-shortest"),
        ]);
    } else if audio_only {
        args.extend([OsString::from("-c:a"), OsString::from("pcm_s16le")]);
    }
    args.push(output.as_os_str().to_owned());
    let result = crate::prerequisites::run_bounded_command(
        &ffmpeg,
        &args,
        Duration::from_secs(30),
        64 * 1024,
    )
    .map_err(|error| anyhow!("generate calibration media failed: {error}"))?;
    ensure!(
        result.status.success(),
        "generate calibration media failed: {}",
        bounded_text(&String::from_utf8_lossy(&result.stderr), 1024)
    );
    Ok(output)
}

struct SourceFacts {
    path: PathBuf,
    duration_ms: u32,
    duration_seconds: f64,
    width: u32,
    height: u32,
    has_audio: bool,
}

fn probe_source_facts(path: &Path) -> Result<SourceFacts> {
    let document = crate::probe_media_document(path)?;
    let stream = crate::require_video_stream(&document)?;
    let duration_seconds = crate::video_duration(&document, stream)
        .ok_or_else(|| anyhow!("calibration source has no finite duration"))?;
    ensure!(
        duration_seconds > 0.0 && duration_seconds * 1000.0 < u32::MAX as f64,
        "calibration source duration is outside the supported range"
    );
    let ffprobe = resolve_tool_path("ffprobe");
    let has_audio = probe_source_audio_present(&ffprobe, path)?;
    Ok(SourceFacts {
        path: path.to_path_buf(),
        duration_ms: (duration_seconds * 1000.0).round() as u32,
        duration_seconds,
        width: stream
            .get("width")
            .and_then(|value| value.as_u64())
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
        height: stream
            .get("height")
            .and_then(|value| value.as_u64())
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
        has_audio,
    })
}

fn generated_sample_identity(seconds: u32, kind: &str) -> String {
    let descriptor = serde_json::json!({
        "kind": "generated",
        "surface": kind,
        "seconds": seconds,
        "video": "testsrc2-320x180-30fps-mpeg4",
        "audio": "sine-440hz-48k-aac",
        "recipe": RESOURCE_CALIBRATION_RECIPE_VERSION,
    });
    let digest = blake3::hash(&serde_json::to_vec(&descriptor).unwrap_or_default())
        .to_hex()
        .to_string();
    format!("generated:{digest}")
}

fn representative_sample_identity(path: &Path) -> Result<(String, u64)> {
    let hash = crate::full_file_blake3(path, &|| false)
        .with_context(|| "fingerprint representative calibration media")?;
    Ok((format!("blake3:{}", hash.hex_digest), hash.byte_count))
}

fn decode_calibration_pcm(source_path: &Path) -> Result<Vec<f32>> {
    let pcm_scratch = std::env::temp_dir().join(format!(
        "rcdvlog-calibration-pcm-{}.wav",
        uuid::Uuid::new_v4().simple()
    ));
    let pcm = read_pcm(source_path).or_else(|_| {
        let wav_path = pcm_scratch.clone();
        let ffmpeg = resolve_tool_path("ffmpeg");
        let args = [
            OsString::from("-hide_banner"),
            OsString::from("-loglevel"),
            OsString::from("error"),
            OsString::from("-y"),
            OsString::from("-i"),
            source_path.as_os_str().to_owned(),
            OsString::from("-ar"),
            OsString::from("16000"),
            OsString::from("-ac"),
            OsString::from("1"),
            OsString::from("-c:a"),
            OsString::from("pcm_s16le"),
            wav_path.as_os_str().to_owned(),
        ];
        let result = crate::prerequisites::run_bounded_command(
            &ffmpeg,
            &args,
            Duration::from_secs(30),
            64 * 1024,
        )
        .map_err(|error| anyhow!("extract calibration PCM: {error}"))?;
        ensure!(
            result.status.success(),
            "extract calibration PCM with ffmpeg failed"
        );
        read_pcm(&wav_path)
    });
    let _ = fs::remove_file(&pcm_scratch);
    pcm
}

fn not_ready_calibration_report(
    cache_key: String,
    identity: CalibrationIdentity,
    started_at: String,
    candidates: Vec<CalibrationCandidateOutcome>,
    note: &str,
    error: String,
) -> ResourceCalibrationReport {
    ResourceCalibrationReport {
        schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
        cache_key,
        state: CalibrationState::NotReady,
        identity,
        started_at,
        completed_at: Some(timestamp()),
        candidates,
        selected_settings: None,
        selection_reason: "not ready".into(),
        quality: None,
        uncertainty: CalibrationUncertainty {
            confidence: "none".into(),
            timing_relative_spread: None,
            eta_relative_range: None,
            note: note.into(),
        },
        error: Some(bounded_text(&error, 2048)),
    }
}

fn proxy_candidates(
    policy: ProxyEncoderPolicy,
    worker_counts: &[usize],
) -> Vec<CalibrationCandidateSettings> {
    // Candidate discovery is declarative. Resolving a usable hardware target
    // runs a synthetic probe and must happen only inside the heavy lease.
    let policies: &[ProxyEncoderPolicy] = match policy {
        ProxyEncoderPolicy::Software => &[ProxyEncoderPolicy::Software],
        ProxyEncoderPolicy::Auto => &[ProxyEncoderPolicy::Auto, ProxyEncoderPolicy::Software],
    };
    let mut candidates = Vec::new();
    for policy in policies {
        let encoder = match policy {
            ProxyEncoderPolicy::Auto => "auto",
            ProxyEncoderPolicy::Software => "software",
        };
        for worker_count in worker_counts {
            candidates.push(CalibrationCandidateSettings {
                label: format!(
                    "policy={};encoder={encoder};workers={worker_count}",
                    policy.as_str()
                ),
                encoder: Some(encoder.into()),
                encoder_policy: Some(policy.as_str().into()),
                worker_count: *worker_count,
                vad_threads: 0,
                decode_threads: 0,
            });
        }
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

fn plan_for_candidate(
    policy: ProxyEncoderPolicy,
    candidate: &CalibrationCandidateSettings,
) -> ProxyEncoderPlan {
    match candidate.encoder_policy.as_deref() {
        Some("software") => resolve_proxy_encoder_plan(ProxyEncoderPolicy::Software),
        _ => resolve_proxy_encoder_plan(policy),
    }
}

fn proxy_quality_equivalent(
    baseline: &ProxyGenerationOutcome,
    candidate: &ProxyGenerationOutcome,
    source_duration_seconds: f64,
) -> Result<()> {
    let baseline_validation = baseline
        .validation
        .as_ref()
        .ok_or_else(|| anyhow!("baseline output has no validation"))?;
    let candidate_validation = candidate
        .validation
        .as_ref()
        .ok_or_else(|| anyhow!("candidate output has no validation"))?;
    ensure!(
        baseline_validation.decoded && candidate_validation.decoded,
        "calibration output did not fully decode"
    );
    ensure!(
        candidate_validation.expected_duration_seconds > 0.0
            && (candidate_validation.duration_seconds - source_duration_seconds).abs()
                <= candidate_validation.duration_tolerance_seconds.max(0.25),
        "candidate output duration does not correspond to the source"
    );
    ensure!(
        baseline_validation.width == candidate_validation.width
            && baseline_validation.height == candidate_validation.height
            && baseline_validation.codec == candidate_validation.codec
            && baseline_validation.pixel_format == candidate_validation.pixel_format,
        "candidate output render contract differs from the baseline"
    );
    ensure!(
        baseline_validation.source_audio_present == candidate_validation.source_audio_present
            && baseline_validation.audio_stream_count == candidate_validation.audio_stream_count,
        "candidate output audio contract differs from the baseline"
    );
    Ok(())
}

fn speech_regions_valid(regions: &[SpeechRegion], duration_ms: u32) -> (bool, String) {
    let mut previous_end = 0_u32;
    for (index, region) in regions.iter().enumerate() {
        if region.start_ms >= region.end_ms {
            return (false, format!("region {index} has a non-positive duration"));
        }
        if region.start_ms >= duration_ms || region.end_ms > duration_ms {
            return (
                false,
                format!("region {index} is outside the decoded audio duration"),
            );
        }
        if index > 0 && region.start_ms < previous_end {
            return (
                false,
                format!("region {index} overlaps or is unordered relative to its predecessor"),
            );
        }
        previous_end = region.end_ms;
    }
    (true, "valid".to_string())
}

fn region_digest(regions: &[SpeechRegion]) -> String {
    let intervals: Vec<[u32; 2]> = regions
        .iter()
        .map(|region| [region.start_ms, region.end_ms])
        .collect();
    blake3::hash(&serde_json::to_vec(&intervals).unwrap_or_default())
        .to_hex()
        .to_string()
}

/// Compare detected regions against the baseline before adopting a faster VAD
/// thread count. Exact intervals are required: a faster run that drops,
/// adds, reorders, overlaps, leaves range, or shifts a boundary is rejected.
pub fn compare_speech_regions(
    reference: &[SpeechRegion],
    candidate: &[SpeechRegion],
    duration_ms: u32,
) -> SpeechRegionComparison {
    let (reference_valid, reference_reason) = speech_regions_valid(reference, duration_ms);
    let (candidate_valid, candidate_reason) = speech_regions_valid(candidate, duration_ms);
    let reference_interval: Vec<[u32; 2]> = reference
        .iter()
        .map(|region| [region.start_ms, region.end_ms])
        .collect();
    let candidate_interval: Vec<[u32; 2]> = candidate
        .iter()
        .map(|region| [region.start_ms, region.end_ms])
        .collect();
    let mismatch_count = reference_interval
        .iter()
        .zip(candidate_interval.iter())
        .filter(|(left, right)| left != right)
        .count()
        + reference_interval.len().abs_diff(candidate_interval.len());
    let differences = reference_interval
        .iter()
        .zip(candidate_interval.iter())
        .enumerate()
        .filter_map(|(index, (left, right))| {
            (left != right).then_some(SpeechRegionDifference {
                index,
                reference: Some(*left),
                candidate: Some(*right),
            })
        })
        .chain(
            reference_interval
                .iter()
                .skip(candidate_interval.len())
                .enumerate()
                .map(|(offset, left)| SpeechRegionDifference {
                    index: candidate_interval.len() + offset,
                    reference: Some(*left),
                    candidate: None,
                }),
        )
        .chain(
            candidate_interval
                .iter()
                .skip(reference_interval.len())
                .enumerate()
                .map(|(offset, right)| SpeechRegionDifference {
                    index: reference_interval.len() + offset,
                    reference: None,
                    candidate: Some(*right),
                }),
        )
        .take(16)
        .collect::<Vec<_>>();
    let reason = if !reference_valid {
        format!("baseline regions invalid: {reference_reason}")
    } else if !candidate_valid {
        format!("candidate regions invalid: {candidate_reason}")
    } else if mismatch_count == 0 {
        "candidate detected the same validated speech regions as the baseline".to_string()
    } else {
        format!("candidate detected different speech regions ({mismatch_count} mismatches)")
    };
    SpeechRegionComparison {
        reference_valid,
        candidate_valid,
        equivalent: reference_valid && candidate_valid && mismatch_count == 0,
        duration_ms,
        reference_region_count: reference.len(),
        candidate_region_count: candidate.len(),
        mismatch_count,
        reference_digest: region_digest(reference),
        candidate_digest: region_digest(candidate),
        reason,
        differences,
    }
}

fn uncertainty_for(timing: &CalibrationTiming, samples: usize) -> CalibrationUncertainty {
    let spread = timing.relative_spread();
    let relative = if samples <= 1 { 0.25 } else { spread.max(0.02) };
    CalibrationUncertainty {
        confidence: if samples >= 3 {
            "measured".into()
        } else {
            "low_sample_count".into()
        },
        timing_relative_spread: Some(spread),
        eta_relative_range: Some(relative),
        note: if samples <= 1 {
            "one timing sample; the ETA range is intentionally widened rather than presented as a completion promise".into()
        } else {
            "ETA range reflects observed timing spread".into()
        },
    }
}

fn eta_from_timing(
    timing: Option<&CalibrationTiming>,
    work_units: Option<u64>,
    uncertainty: &CalibrationUncertainty,
    source: &str,
) -> CalibrationEta {
    let Some(timing) = timing else {
        return CalibrationEta {
            status: "uncertain".into(),
            estimated_remaining_seconds: None,
            range_seconds: None,
            confidence: "none".into(),
            basis: source.into(),
            explanation: "no completed matching calibration timing is available".into(),
        };
    };
    let Some(work_units) = work_units.filter(|work_units| *work_units > 0) else {
        return CalibrationEta {
            status: "uncertain".into(),
            estimated_remaining_seconds: None,
            range_seconds: None,
            confidence: "none".into(),
            basis: source.into(),
            explanation: "calibration exists but the caller supplied no positive work weight"
                .into(),
        };
    };
    let Some(rate) = timing.work_units_per_second.filter(|rate| *rate > 0.0) else {
        return CalibrationEta {
            status: "uncertain".into(),
            estimated_remaining_seconds: None,
            range_seconds: None,
            confidence: "none".into(),
            basis: source.into(),
            explanation: "calibration has no usable throughput observation".into(),
        };
    };
    let estimate = work_units as f64 / rate;
    let relative = uncertainty.eta_relative_range.unwrap_or(0.25).max(0.02);
    CalibrationEta {
        status: "calibrated_estimate".into(),
        estimated_remaining_seconds: Some(estimate),
        range_seconds: Some([estimate * (1.0 - relative), estimate * (1.0 + relative)]),
        confidence: uncertainty.confidence.clone(),
        basis: source.into(),
        explanation: "range comes from this host/recipe/configuration calibration; it is not a completion promise".into(),
    }
}

/// Validate explicit resource settings without model inspection, catalog writes
/// or media work, including for no-work/no-audio fast paths.
pub(crate) fn validate_resource_overrides(
    representation_kind: &str,
    overrides: &ResourceOverrides,
) -> Result<()> {
    if let Some(value) = overrides.worker_count {
        ensure!(
            (1..=MAX_CALIBRATION_WORKERS).contains(&value),
            "invalid input: workerCount must be 1..={MAX_CALIBRATION_WORKERS}"
        );
        if representation_kind == SPEECH_REPRESENTATION_KIND {
            ensure!(
                value == 1,
                "invalid input: speech transcription is serial; workerCount > 1 is not supported and cannot be silently ignored"
            );
        }
    }
    if let Some(value) = overrides.vad_threads {
        ensure!(
            representation_kind == SPEECH_REPRESENTATION_KIND,
            "invalid input: vadThreads is only valid for speech calibration/transcription"
        );
        ensure!(
            (1..=MAX_CALIBRATION_THREADS).contains(&value),
            "invalid input: vadThreads must be 1..={MAX_CALIBRATION_THREADS}"
        );
    }
    if let Some(value) = overrides.decode_threads {
        ensure!(
            representation_kind == SPEECH_REPRESENTATION_KIND,
            "invalid input: decodeThreads is only valid for speech calibration/transcription"
        );
        ensure!(
            (1..=MAX_CALIBRATION_THREADS).contains(&value),
            "invalid input: decodeThreads must be 1..={MAX_CALIBRATION_THREADS}"
        );
    }
    if let Some(encoder) = overrides.encoder.as_deref() {
        ensure!(
            representation_kind == PROXY_REPRESENTATION_KIND,
            "invalid input: encoder is only valid for proxy resource selection"
        );
        // Only an encoder the domain can actually force is accepted. The
        // current proxy domain deliberately has no force-hardware policy:
        // accepting `vaapi`/`videotoolbox` here would let it fall back while
        // still claiming the override was honored.
        ensure!(
            encoder == "software",
            "invalid input: explicit encoder override {encoder:?} is unsupported; only software can be forced (use policy auto for bounded hardware probing)"
        );
    }
    Ok(())
}

fn override_selection(
    representation_kind: &str,
    recipe_key: &str,
    overrides: &ResourceOverrides,
    history_key: String,
) -> Result<ResourceSelection> {
    validate_resource_overrides(representation_kind, overrides)?;
    Ok(ResourceSelection {
        representation_kind: representation_kind.into(),
        recipe_key: recipe_key.into(),
        source: "explicit_override".into(),
        state: "explicit".into(),
        encoder: overrides.encoder.clone(),
        worker_count: overrides.worker_count.unwrap_or(1),
        vad_threads: overrides.vad_threads.unwrap_or(DEFAULT_DECODE_THREADS),
        decode_threads: overrides.decode_threads.unwrap_or(DEFAULT_DECODE_THREADS),
        calibration_key: None,
        timing: None,
        uncertainty: CalibrationUncertainty {
            confidence: "explicit".into(),
            timing_relative_spread: None,
            eta_relative_range: None,
            note: "settings came from an explicit override; no calibrated ETA is claimed".into(),
        },
        eta: CalibrationEta {
            status: "explicit_override_no_eta".into(),
            estimated_remaining_seconds: None,
            range_seconds: None,
            confidence: "explicit".into(),
            basis: "explicit_override".into(),
            explanation: "explicit settings do not imply a measured throughput estimate".into(),
        },
        history_key,
        sample_compatibility: "unknown".into(),
    })
}

fn per_worker_rate(timing: &CalibrationTiming, worker_count: usize) -> Option<f64> {
    let aggregate = timing.work_units_per_second.filter(|rate| *rate > 0.0)?;
    Some(aggregate / worker_count.max(1) as f64)
}

fn widen_uncertainty_for_sample(
    mut uncertainty: CalibrationUncertainty,
    compatibility: &str,
) -> CalibrationUncertainty {
    match compatibility {
        "generated_contract_only" => {
            if uncertainty.confidence == "measured" {
                uncertainty.confidence = "low_sample_count".into();
            }
            uncertainty.eta_relative_range =
                Some(uncertainty.eta_relative_range.unwrap_or(0.25).max(0.5));
            uncertainty.note.push_str(
                "; the synthetic 320x180 sample validates the proxy contract but is not a representative 4K/HEVC corpus sample, so the ETA range is deliberately wide",
            );
        }
        "unknown" => {
            uncertainty.confidence = "low_sample_count".into();
            uncertainty.eta_relative_range =
                Some(uncertainty.eta_relative_range.unwrap_or(0.25).max(1.0));
            uncertainty.note.push_str(
                "; sample compatibility is unknown, so the ETA range is widened and must not be treated as coverage",
            );
        }
        _ => {}
    }
    uncertainty
}

#[derive(Debug, Clone)]
struct CalibrationRunOwner {
    owner_id: String,
    pid: u32,
    process_start: String,
}

fn process_start_marker(pid: u32) -> Option<String> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, after_comm) = text.rsplit_once(") ")?;
    after_comm.split_whitespace().nth(19).map(str::to_string)
}

fn current_run_owner() -> CalibrationRunOwner {
    let pid = std::process::id();
    CalibrationRunOwner {
        owner_id: uuid::Uuid::new_v4().simple().to_string(),
        pid,
        process_start: process_start_marker(pid).unwrap_or_default(),
    }
}

fn active_calibration_runs() -> &'static Mutex<HashMap<String, CalibrationRunOwner>> {
    static ACTIVE: OnceLock<Mutex<HashMap<String, CalibrationRunOwner>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn active_run_key(database_path: &Path, cache_key: &str) -> String {
    format!("{}::{cache_key}", database_path.to_string_lossy())
}

struct ActiveCalibrationCleanup {
    database_path: PathBuf,
    keys: Vec<String>,
}

impl Drop for ActiveCalibrationCleanup {
    fn drop(&mut self) {
        let mut active = active_calibration_runs()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for key in &self.keys {
            active.remove(&active_run_key(&self.database_path, key));
        }
    }
}

fn owner_is_live(
    owner_id: Option<&str>,
    pid: Option<u32>,
    process_start: Option<&str>,
    heartbeat_at: Option<&str>,
) -> bool {
    let Some(pid) = pid.filter(|pid| *pid > 0) else {
        return false;
    };
    let active = active_calibration_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(owner_id) = owner_id
        && active
            .values()
            .any(|owner| owner.owner_id == owner_id && owner.pid == pid)
    {
        return true;
    }
    if pid == std::process::id() {
        return false;
    }
    if let Some(expected_start) = process_start.filter(|value| !value.is_empty())
        && let Some(actual_start) = process_start_marker(pid)
    {
        return actual_start == expected_start;
    }
    #[cfg(unix)]
    if i32::try_from(pid).is_err() {
        return false;
    }
    #[cfg(unix)]
    if let Ok(pid) = i32::try_from(pid) {
        let result = unsafe { libc::kill(pid, 0) };
        if result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) {
            return true;
        }
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return false;
        }
    }
    // An unobservable process is conservatively live while its last heartbeat
    // is fresh. It can never be treated as a completed calibration.
    heartbeat_at.is_some_and(recent_heartbeat)
}

fn recent_heartbeat(value: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|time| {
            Utc::now()
                .signed_duration_since(time.with_timezone(&Utc))
                .to_std()
                .ok()
        })
        .is_some_and(|age| age <= Duration::from_secs(300))
}

impl Catalog {
    /// Recover only runs whose owner is proven gone. Opening a second catalog
    /// while another live process owns a running calibration must be a no-op.
    pub(crate) fn recover_interrupted_resource_calibrations(&self) -> Result<()> {
        if self.read_only {
            return Ok(());
        }
        let mut connection = self.connection()?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='resource_calibrations')",
            [],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(());
        }
        let rows = {
            let mut statement = connection.prepare(
                "SELECT cache_key,owner_id,owner_pid,owner_process_start,heartbeat_at FROM resource_calibrations WHERE state='running'",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let abandoned = rows
            .into_iter()
            .filter(|(_, owner_id, pid, process_start, heartbeat_at)| {
                !owner_is_live(
                    owner_id.as_deref(),
                    pid.and_then(|value| u32::try_from(value).ok()),
                    process_start.as_deref(),
                    heartbeat_at.as_deref(),
                )
            })
            .collect::<Vec<_>>();
        if abandoned.is_empty() {
            return Ok(());
        }
        // Recovery is maintenance, never a reason to block a catalog reader.
        // Effective status already treats a dead owner's row as interrupted;
        // a later writer may persist that fact when SQLite is available.
        connection.busy_timeout(Duration::ZERO)?;
        let transaction = match connection.transaction_with_behavior(TransactionBehavior::Immediate)
        {
            Ok(transaction) => transaction,
            Err(error)
                if matches!(
                    error.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let now = timestamp();
        for (cache_key, owner_id, pid, process_start, heartbeat_at) in abandoned {
            let pid = pid.and_then(|value| u32::try_from(value).ok());
            if owner_is_live(
                owner_id.as_deref(),
                pid,
                process_start.as_deref(),
                heartbeat_at.as_deref(),
            ) {
                continue;
            }
            transaction.execute(
                "UPDATE resource_calibrations SET state='interrupted',updated_at=?1,completed_at=?1,invalidated_at=?1,invalidation_reason='owner process is no longer live' WHERE cache_key=?2 AND state='running' AND owner_id IS ?3",
                params![now, cache_key, owner_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn begin_resource_calibration(
        &self,
        identity: &CalibrationIdentity,
        cache_key: &str,
    ) -> Result<()> {
        let now = timestamp();
        let identity_json = serde_json::to_string(identity)?;
        let owner = current_run_owner();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT state,owner_id,owner_pid,owner_process_start,heartbeat_at FROM resource_calibrations WHERE cache_key=?1",
                [cache_key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?;
        if let Some((state, owner_id, pid, process_start, heartbeat_at)) = existing {
            let pid = pid.and_then(|value| u32::try_from(value).ok());
            if state == "running"
                && owner_is_live(
                    owner_id.as_deref(),
                    pid,
                    process_start.as_deref(),
                    heartbeat_at.as_deref(),
                )
            {
                return Err(anyhow!(
                    "calibration for this exact identity is already running in a live process"
                ));
            }
        }
        transaction.execute(
            "INSERT INTO resource_calibrations(cache_key,schema_version,recipe_key,representation_kind,media_kind,sample_identity,state,identity_json,created_at,updated_at,owner_id,owner_pid,owner_process_start,heartbeat_at) VALUES(?1,?2,?3,?4,?5,?6,'running',?7,?8,?8,?9,?10,?11,?8) ON CONFLICT(cache_key) DO UPDATE SET state='running',report_json=NULL,selected_settings_json=NULL,created_at=excluded.created_at,updated_at=excluded.updated_at,completed_at=NULL,invalidated_at=NULL,invalidation_reason=NULL,identity_json=excluded.identity_json,owner_id=excluded.owner_id,owner_pid=excluded.owner_pid,owner_process_start=excluded.owner_process_start,heartbeat_at=excluded.heartbeat_at",
            params![
                cache_key,
                RESOURCE_CALIBRATION_SCHEMA_VERSION,
                identity.recipe_key,
                identity.representation_kind,
                identity.media.kind,
                identity.media.sample_identity,
                identity_json,
                now,
                owner.owner_id,
                owner.pid as i64,
                owner.process_start,
            ],
        )?;
        transaction.execute(
            "DELETE FROM resource_calibration_observations WHERE cache_key=?1",
            [cache_key],
        )?;
        let run_key = active_run_key(self.database_path(), cache_key);
        // Publish in-process ownership before the SQLite row becomes visible.
        active_calibration_runs()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(run_key.clone(), owner);
        if let Err(error) = transaction.commit() {
            active_calibration_runs()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&run_key);
            return Err(error.into());
        }
        Ok(())
    }

    fn touch_resource_calibration(&self, cache_key: &str) -> Result<()> {
        let run_key = active_run_key(self.database_path(), cache_key);
        let owner_id = active_calibration_runs()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&run_key)
            .map(|owner| owner.owner_id.clone());
        let Some(owner_id) = owner_id else {
            return Ok(());
        };
        let connection = self.connection()?;
        connection.execute(
            "UPDATE resource_calibrations SET heartbeat_at=?1,updated_at=?1 WHERE cache_key=?2 AND state='running' AND owner_id=?3",
            params![timestamp(), cache_key, owner_id],
        )?;
        Ok(())
    }

    /// Persist the report, its selected settings and all observations in one
    /// SQLite transaction. A failure leaves the prior running row and no
    /// partial terminal report/observation set.
    fn persist_resource_calibration(&self, report: &mut ResourceCalibrationReport) -> Result<()> {
        let now = timestamp();
        let final_key = report.identity.cache_key()?;
        let original_key = report.cache_key.clone();
        let _cleanup = ActiveCalibrationCleanup {
            database_path: self.database_path().to_path_buf(),
            keys: vec![original_key.clone(), final_key.clone()],
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if final_key != original_key {
            transaction.execute(
                "INSERT INTO resource_calibrations(cache_key,schema_version,recipe_key,representation_kind,media_kind,sample_identity,state,identity_json,report_json,selected_settings_json,created_at,updated_at,completed_at,invalidated_at,invalidation_reason,owner_id,owner_pid,owner_process_start,heartbeat_at) SELECT ?1,schema_version,recipe_key,representation_kind,media_kind,sample_identity,state,identity_json,report_json,selected_settings_json,created_at,updated_at,completed_at,invalidated_at,invalidation_reason,owner_id,owner_pid,owner_process_start,heartbeat_at FROM resource_calibrations WHERE cache_key=?2",
                params![final_key, original_key],
            )?;
            transaction.execute(
                "DELETE FROM resource_calibrations WHERE cache_key=?1",
                [&original_key],
            )?;
            report.cache_key = final_key.clone();
        }
        let selected = report
            .selected_settings
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        transaction.execute(
            "UPDATE resource_calibrations SET schema_version=?1,recipe_key=?2,representation_kind=?3,media_kind=?4,sample_identity=?5,identity_json=?6,state=?7,report_json=?8,selected_settings_json=?9,updated_at=?10,completed_at=?11,heartbeat_at=?10 WHERE cache_key=?12",
            params![
                RESOURCE_CALIBRATION_SCHEMA_VERSION,
                report.identity.recipe_key,
                report.identity.representation_kind,
                report.identity.media.kind,
                report.identity.media.sample_identity,
                serde_json::to_string(&report.identity)?,
                report.state.as_str(),
                serde_json::to_string(report)?,
                selected,
                now,
                report.completed_at,
                report.cache_key,
            ],
        )?;
        transaction.execute(
            "DELETE FROM resource_calibration_observations WHERE cache_key=?1",
            [&report.cache_key],
        )?;
        for candidate in &report.candidates {
            if let Some(timing) = &candidate.timing {
                for duration_ms in &timing.samples_ms {
                    transaction.execute(
                        "INSERT INTO resource_calibration_observations(cache_key,candidate_label,succeeded,duration_ms,failure,created_at) VALUES(?1,?2,1,?3,NULL,?4)",
                        params![report.cache_key, candidate.settings.label, *duration_ms as i64, now],
                    )?;
                }
            }
            for failure in &candidate.failures {
                transaction.execute(
                    "INSERT INTO resource_calibration_observations(cache_key,candidate_label,succeeded,duration_ms,failure,created_at) VALUES(?1,?2,0,NULL,?3,?4)",
                    params![report.cache_key, candidate.settings.label, bounded_text(failure, 1024), now],
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    fn exact_complete_calibration(
        &self,
        cache_key: &str,
    ) -> Result<Option<ResourceCalibrationReport>> {
        let connection = self.connection()?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='resource_calibrations')",
            [],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(None);
        }
        let body: Option<String> = connection
            .query_row(
                "SELECT report_json FROM resource_calibrations WHERE cache_key=?1 AND state='complete' AND report_json IS NOT NULL",
                [cache_key],
                |row| row.get(0),
            )
            .optional()?;
        let Some(body) = body else {
            return Ok(None);
        };
        let report: ResourceCalibrationReport = serde_json::from_str(&body)?;
        if report.cache_key != cache_key
            || report.identity.cache_key()? != cache_key
            || !self.report_environment_matches_current(&report)?
        {
            return Ok(None);
        }
        Ok(Some(report))
    }

    fn report_environment_matches_current(
        &self,
        report: &ResourceCalibrationReport,
    ) -> Result<bool> {
        if report.identity.schema_version != RESOURCE_CALIBRATION_SCHEMA_VERSION
            || report.identity.recipe_version != RESOURCE_CALIBRATION_RECIPE_VERSION
            || report.identity.host != host_identity()
            || report.identity.tools != tool_identities()
            || report.identity.devices != hardware_device_identities()
        {
            return Ok(false);
        }
        let policy = report
            .identity
            .candidates
            .iter()
            .find_map(|candidate| candidate.encoder_policy.as_deref())
            .and_then(|value| match value {
                "auto" => Some(ProxyEncoderPolicy::Auto),
                "software" => Some(ProxyEncoderPolicy::Software),
                _ => None,
            });
        let expected_configuration = calibration_configuration_key(
            &report.identity.media.kind,
            &report.identity.candidates,
            policy,
        )?;
        if report.identity.configuration_key != expected_configuration {
            return Ok(false);
        }
        let current_models = if report.identity.representation_kind == SPEECH_REPRESENTATION_KIND {
            model_identity(&self.model_status()?)
        } else {
            Vec::new()
        };
        let mut measured_models = report.identity.models.clone();
        for model in &mut measured_models {
            model.content_digest = None;
        }
        Ok(measured_models == current_models)
    }

    fn running_owner_is_live(&self, cache_key: &str) -> Result<bool> {
        let connection = self.connection()?;
        let row = connection
            .query_row(
                "SELECT owner_id,owner_pid,owner_process_start,heartbeat_at FROM resource_calibrations WHERE cache_key=?1 AND state='running'",
                [cache_key],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((owner_id, pid, process_start, heartbeat_at)) = row else {
            return Ok(false);
        };
        Ok(owner_is_live(
            owner_id.as_deref(),
            pid.and_then(|value| u32::try_from(value).ok()),
            process_start.as_deref(),
            heartbeat_at.as_deref(),
        ))
    }

    fn calibration_entries(
        &self,
        recipe_key: Option<&str>,
        representation_kind: Option<&str>,
    ) -> Result<Vec<CalibrationSummary>> {
        let connection = self.connection()?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='resource_calibrations')",
            [],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(Vec::new());
        }
        let mut statement = connection.prepare(
            "SELECT cache_key,recipe_key,representation_kind,media_kind,sample_identity,state,selected_settings_json,report_json,created_at,updated_at,completed_at,invalidation_reason FROM resource_calibrations WHERE (?1 IS NULL OR recipe_key=?1) AND (?2 IS NULL OR representation_kind=?2) ORDER BY updated_at DESC, cache_key ASC LIMIT 100",
        )?;
        let rows = statement.query_map(params![recipe_key, representation_kind], |row| {
            let state_text: String = row.get(5)?;
            let selected_text: Option<String> = row.get(6)?;
            let report_text: Option<String> = row.get(7)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                state_text,
                selected_text,
                report_text,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<String>>(11)?,
            ))
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let (
                cache_key,
                recipe_key,
                representation_kind,
                media_kind,
                sample_identity,
                state_text,
                selected_text,
                report_text,
                created_at,
                updated_at,
                completed_at,
                invalidation_reason,
            ) = row?;
            let state = CalibrationState::parse(&state_text)?;
            let selected_settings = selected_text
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?;
            let report: Option<ResourceCalibrationReport> = report_text
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?;
            let host_tools_match_current = report
                .as_ref()
                .map(|report| self.report_environment_matches_current(report))
                .transpose()?
                .unwrap_or(false);
            entries.push(CalibrationSummary {
                reusable: state == CalibrationState::Complete && host_tools_match_current,
                cache_key,
                recipe_key,
                representation_kind,
                media_kind,
                sample_identity,
                state,
                selected_settings,
                uncertainty: report.map(|report| report.uncertainty),
                created_at,
                updated_at,
                completed_at,
                invalidation_reason,
                host_tools_match_current,
            });
        }
        Ok(entries)
    }

    /// Read-only calibration inventory. A running row is shown as interrupted
    /// only when its owner is proven gone; a live owner in another process is
    /// never invalidated by a read-only status call.
    pub fn resource_calibration_status(
        &self,
        recipe_key: Option<&str>,
        representation_kind: Option<&str>,
    ) -> Result<CalibrationStatusReport> {
        let mut entries = self.calibration_entries(recipe_key, representation_kind)?;
        for entry in &mut entries {
            if entry.state == CalibrationState::Running {
                entry.reusable = false;
                if !self.running_owner_is_live(&entry.cache_key)? {
                    entry.state = CalibrationState::Interrupted;
                }
            }
        }
        Ok(CalibrationStatusReport {
            schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
            recipe_key: recipe_key.map(str::to_string),
            representation_kind: representation_kind.map(str::to_string),
            entries,
            note: "Only complete calibrations matching host/build/tool/device/model/recipe/configuration facts are reusable. Missing, interrupted, failed, not-ready, and differently configured results remain uncertainty and are never converted into a completion promise.".into(),
        })
    }

    /// Find the newest reusable calibration for one kind/recipe. Generated
    /// media calibrations are preferred because their sample identity is
    /// reproducible; representative-media results are only reused when the
    /// caller provides the exact same media sample identity.
    fn reusable_calibration_report(
        &self,
        representation_kind: &str,
        recipe_key: &str,
        sample_identity: Option<&str>,
    ) -> Result<Option<ResourceCalibrationReport>> {
        let connection = self.connection()?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='resource_calibrations')",
            [],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(None);
        }
        let mut statement = connection.prepare(
            "SELECT report_json FROM resource_calibrations WHERE state='complete' AND representation_kind=?1 AND recipe_key=?2 AND report_json IS NOT NULL ORDER BY updated_at DESC LIMIT 100",
        )?;
        let reports = statement
            .query_map(params![representation_kind, recipe_key], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for body in reports {
            let report: ResourceCalibrationReport = serde_json::from_str(&body)?;
            if report.identity.representation_kind != representation_kind
                || report.identity.recipe_key != recipe_key
                || !self.report_environment_matches_current(&report)?
            {
                continue;
            }
            if let Some(sample_identity) = sample_identity
                && report.identity.media.sample_identity != sample_identity
            {
                continue;
            }
            if report.identity.media.kind == "authorized_representative"
                && sample_identity.is_none()
            {
                continue;
            }
            return Ok(Some(report));
        }
        Ok(None)
    }

    fn resource_environment_fingerprint(
        &self,
        representation_kind: &str,
        recipe_key: &str,
    ) -> Result<String> {
        let models = if representation_kind == SPEECH_REPRESENTATION_KIND {
            model_identity(&self.model_status()?)
        } else {
            Vec::new()
        };
        let scope = serde_json::json!({
            "schemaVersion": RESOURCE_CALIBRATION_SCHEMA_VERSION,
            "recipeKey": recipe_key,
            "recipeVersion": RESOURCE_CALIBRATION_RECIPE_VERSION,
            "host": host_identity(),
            "tools": tool_identities(),
            "devices": hardware_device_identities(),
            "models": models,
        });
        Ok(blake3::hash(&serde_json::to_vec(&scope)?)
            .to_hex()
            .to_string())
    }

    /// A history key is only reusable where every environment/configuration
    /// fact that contributed to the timing is still identical. This prevents a
    /// one-worker or software rate from being read on another host/build/GPU or
    /// under a different thread/worker setting.
    #[allow(clippy::too_many_arguments)]
    pub fn qualified_history_key(
        &self,
        base_key: &str,
        representation_kind: &str,
        recipe_key: &str,
        encoder: Option<&str>,
        worker_count: usize,
        vad_threads: usize,
        decode_threads: usize,
    ) -> Result<String> {
        let environment = self.resource_environment_fingerprint(representation_kind, recipe_key)?;
        let configuration = serde_json::json!({
            "encoder": encoder,
            "workerCount": worker_count,
            "vadThreads": vad_threads,
            "decodeThreads": decode_threads,
        });
        let configuration = blake3::hash(&serde_json::to_vec(&configuration)?)
            .to_hex()
            .to_string();
        Ok(format!(
            "{base_key}.h{}.c{}",
            &environment[..16],
            &configuration[..16]
        ))
    }

    /// Resolve the actual settings and calibrated ETA for one representation
    /// kind. Explicit overrides are validated and reported as their own
    /// provenance; they never masquerade as calibrated measurements.
    pub fn resolve_resource_selection(
        &self,
        representation_kind: &str,
        recipe_key: &str,
        work_units: Option<u64>,
        media_sample_identity: Option<&str>,
        overrides: &ResourceOverrides,
    ) -> Result<ResourceSelection> {
        let any_override = overrides.encoder.is_some()
            || overrides.worker_count.is_some()
            || overrides.vad_threads.is_some()
            || overrides.decode_threads.is_some();
        let history_base = if representation_kind == SPEECH_REPRESENTATION_KIND {
            STT_TASK_KEY
        } else {
            representation_kind
        };
        let default_history_key = self.qualified_history_key(
            history_base,
            representation_kind,
            recipe_key,
            None,
            1,
            DEFAULT_DECODE_THREADS,
            DEFAULT_DECODE_THREADS,
        )?;
        if any_override {
            let mut selection = override_selection(
                representation_kind,
                recipe_key,
                overrides,
                default_history_key,
            )?;
            selection.history_key = self.qualified_history_key(
                history_base,
                representation_kind,
                recipe_key,
                selection.encoder.as_deref(),
                selection.worker_count,
                selection.vad_threads,
                selection.decode_threads,
            )?;
            return Ok(selection);
        }
        if let Some(report) = self.reusable_calibration_report(
            representation_kind,
            recipe_key,
            media_sample_identity,
        )? {
            let selected = report
                .selected_settings
                .clone()
                .ok_or_else(|| anyhow!("complete calibration has no selected settings"))?;
            let measured_timing = report
                .candidates
                .iter()
                .find(|candidate| candidate.settings == selected)
                .and_then(|candidate| candidate.timing.clone());
            let sample_compatibility = match report.identity.media.kind.as_str() {
                "generated"
                    if media_sample_identity
                        == Some(report.identity.media.sample_identity.as_str()) =>
                {
                    "exact_generated"
                }
                "generated" => "generated_contract_only",
                "authorized_representative"
                    if media_sample_identity
                        == Some(report.identity.media.sample_identity.as_str()) =>
                {
                    "exact_representative"
                }
                _ => "unknown",
            }
            .to_string();
            // A synthetic contract check is not a corpus throughput fit.
            // Preserve measured timings in the report; only an explicitly
            // matching sample may seed an ETA through this selection API.
            let timing = if sample_compatibility.starts_with("exact_") {
                measured_timing
            } else {
                None
            };
            let uncertainty =
                widen_uncertainty_for_sample(report.uncertainty, &sample_compatibility);
            let eta = eta_from_timing(
                timing.as_ref(),
                work_units,
                &uncertainty,
                "matching_host_calibration",
            );
            let worker_count = selected.worker_count.max(1);
            let vad_threads = selected.vad_threads.max(1);
            let decode_threads = selected.decode_threads.max(1);
            let history_key = self.qualified_history_key(
                history_base,
                representation_kind,
                recipe_key,
                selected.encoder.as_deref(),
                worker_count,
                vad_threads,
                decode_threads,
            )?;
            return Ok(ResourceSelection {
                representation_kind: representation_kind.into(),
                recipe_key: recipe_key.into(),
                source: "calibration".into(),
                state: "complete".into(),
                encoder: selected.encoder,
                worker_count,
                vad_threads,
                decode_threads,
                calibration_key: Some(report.cache_key),
                timing,
                uncertainty,
                eta,
                history_key,
                sample_compatibility,
            });
        }
        // Deliberately do not call a hardware probe here. Readiness, speech,
        // read-only inspection and ordinary batch planning must not benchmark
        // an encoder merely because no calibration exists.
        let encoder =
            (representation_kind == PROXY_REPRESENTATION_KIND).then(|| "software".to_string());
        Ok(ResourceSelection {
            representation_kind: representation_kind.into(),
            recipe_key: recipe_key.into(),
            source: "default_uncalibrated".into(),
            state: "missing".into(),
            encoder,
            worker_count: 1,
            vad_threads: DEFAULT_DECODE_THREADS,
            decode_threads: DEFAULT_DECODE_THREADS,
            calibration_key: None,
            timing: None,
            uncertainty: CalibrationUncertainty {
                confidence: "none".into(),
                timing_relative_spread: None,
                eta_relative_range: None,
                note: "no matching complete host/tool/recipe/configuration calibration exists"
                    .into(),
            },
            eta: CalibrationEta {
                status: "uncertain_missing_calibration".into(),
                estimated_remaining_seconds: None,
                range_seconds: None,
                confidence: "none".into(),
                basis: "default_uncalibrated".into(),
                explanation:
                    "run explicit `rcdvlog calibrate run` before relying on a calibrated ETA".into(),
            },
            history_key: default_history_key,
            sample_compatibility: "unknown".into(),
        })
    }

    /// Seed a BatchEstimator only from a matching completed calibration. This
    /// does not create a synthetic sample count and does not turn explicit
    /// overrides into measured throughput.
    pub fn calibration_rate_history(
        &self,
        representation_kind: &str,
        recipe_key: &str,
        encoder: Option<&str>,
        worker_count: usize,
        media_sample_identity: Option<&str>,
    ) -> Result<RateHistory> {
        let Some(sample_identity) = media_sample_identity else {
            return Ok(RateHistory::default());
        };
        let Some(report) = self.reusable_calibration_report(
            representation_kind,
            recipe_key,
            Some(sample_identity),
        )?
        else {
            return Ok(RateHistory::default());
        };
        let Some(selected) = report.selected_settings.as_ref() else {
            return Ok(RateHistory::default());
        };
        if selected.worker_count != worker_count {
            return Ok(RateHistory::default());
        }
        if let Some(encoder) = encoder
            && selected.encoder.as_deref() != Some(encoder)
        {
            return Ok(RateHistory::default());
        }
        let Some(timing) = report
            .candidates
            .iter()
            .find(|candidate| candidate.settings == *selected)
            .and_then(|candidate| candidate.timing.as_ref())
        else {
            return Ok(RateHistory::default());
        };
        let Some(rate) = per_worker_rate(timing, selected.worker_count.max(1)) else {
            return Ok(RateHistory::default());
        };
        Ok(RateHistory {
            work_units_per_worker_second: rate,
            samples: timing.samples_ms.len() as u64,
        })
    }

    /// Explicit bounded proxy calibration. With `CalibrationMedia::Generated`
    /// this creates and removes synthetic media under managed cache scratch.
    #[allow(clippy::result_large_err)]
    pub fn calibrate_proxy(
        &self,
        options: &ProxyCalibrationOptions,
    ) -> Result<ResourceCalibrationReport> {
        ensure!(
            !self.read_only,
            "calibration requires an explicitly writable catalog"
        );
        validate_trials(options.trials)?;
        validate_worker_counts(&options.worker_counts)?;
        validate_generated_duration(options.generated_duration_seconds)?;
        if let CalibrationMedia::Representative(path) = &options.media {
            validate_representative_media_metadata(path)?;
        }
        let sample_lease = if matches!(&options.media, CalibrationMedia::Representative(_)) {
            Some(self.acquire_local_heavy()?)
        } else {
            None
        };
        let candidates = proxy_candidates(options.policy, &options.worker_counts);
        validate_candidate_budget(candidates.len(), options.trials)?;
        let (media_identity, representative_path) = match &options.media {
            CalibrationMedia::Generated => (
                CalibrationMediaIdentity {
                    kind: "generated".into(),
                    sample_identity: generated_sample_identity(
                        options.generated_duration_seconds,
                        "proxy",
                    ),
                    duration_ms: options.generated_duration_seconds.saturating_mul(1000),
                    width: 320,
                    height: 180,
                    has_audio: true,
                },
                None,
            ),
            CalibrationMedia::Representative(path) => {
                let facts = probe_source_facts(path)?;
                validate_representative_duration(facts.duration_ms)?;
                let (sample_identity, _) = representative_sample_identity(path)?;
                (
                    CalibrationMediaIdentity {
                        kind: "authorized_representative".into(),
                        sample_identity,
                        duration_ms: facts.duration_ms,
                        width: facts.width,
                        height: facts.height,
                        has_audio: facts.has_audio,
                    },
                    Some(path.clone()),
                )
            }
        };
        let recipe_key = PROXY_SOFTWARE_RECIPE_KEY;
        let identity = CalibrationIdentity {
            schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
            recipe_key: recipe_key.into(),
            recipe_version: RESOURCE_CALIBRATION_RECIPE_VERSION.into(),
            representation_kind: PROXY_REPRESENTATION_KIND.into(),
            host: host_identity(),
            tools: tool_identities(),
            devices: hardware_device_identities(),
            models: vec![],
            media: media_identity,
            conditions: CalibrationConditions {
                trials: options.trials,
                local_heavy_slots: 1,
                generated_duration_seconds: matches!(options.media, CalibrationMedia::Generated)
                    .then_some(options.generated_duration_seconds),
                note: "single shared local-heavy slot; concurrent worker candidates are measured inside that one slot".into(),
            },
            candidates: candidates.clone(),
            configuration_key: calibration_configuration_key(
                options.media.kind(),
                &candidates,
                Some(options.policy),
            )?,
        };
        let cache_key = identity.cache_key()?;
        if !options.force
            && let Some(report) = self.exact_complete_calibration(&cache_key)?
        {
            return Ok(report);
        }
        let _heavy = match sample_lease {
            Some(lease) => lease,
            None => self.acquire_local_heavy()?,
        };
        self.begin_resource_calibration(&identity, &cache_key)?;
        let started_at = timestamp();
        let mut measurements: Vec<CalibrationCandidateOutcome> = Vec::new();
        let mut accepted_reference: Option<(String, ProxyGenerationOutcome)> = None;

        let run = (|| -> Result<()> {
            let scratch = (representative_path.is_none())
                .then(|| create_scratch_root(self))
                .transpose()?;
            let generated_path = scratch
                .as_ref()
                .map(|scratch| {
                    run_ffmpeg_fixture(&scratch.0, options.generated_duration_seconds, true, false)
                })
                .transpose()?;
            let source_path = match &options.media {
                CalibrationMedia::Generated => generated_path
                    .as_ref()
                    .ok_or_else(|| anyhow!("generated calibration media was not created"))?,
                CalibrationMedia::Representative(_) => representative_path
                    .as_ref()
                    .ok_or_else(|| anyhow!("authorized representative media path missing"))?,
            };
            let facts = if representative_path.is_some() {
                probe_source_facts(source_path)?
            } else {
                SourceFacts {
                    path: source_path.clone(),
                    duration_ms: options.generated_duration_seconds.saturating_mul(1000),
                    duration_seconds: f64::from(options.generated_duration_seconds),
                    width: 320,
                    height: 180,
                    has_audio: true,
                }
            };
            let ffmpeg = resolve_tool_path("ffmpeg");
            let ffprobe = resolve_tool_path("ffprobe");
            let scratch_work = scratch.as_ref().map(|scratch| &scratch.0);
            for candidate in &candidates {
                self.touch_resource_calibration(&cache_key)?;
                let mut durations = Vec::new();
                let mut failures = Vec::new();
                let mut output_identity = None;
                let mut render_contract_equivalent = None;
                let mut observed_encoder = None;
                let plan = plan_for_candidate(options.policy, candidate);
                for trial in 0..options.trials {
                    let trial_dir = scratch_work
                        .map(|root| {
                            root.join(format!(
                                "candidate-{}-trial-{trial}",
                                blake3::hash(candidate.label.as_bytes()).to_hex()
                            ))
                        })
                        .unwrap_or_else(|| {
                            std::env::temp_dir().join(format!(
                                "rcdvlog-calibration-{}",
                                uuid::Uuid::new_v4().simple()
                            ))
                        });
                    if let Err(error) = fs::create_dir_all(&trial_dir) {
                        failures.push(format!("create trial output directory: {error}"));
                        break;
                    }
                    let worker_count = candidate.worker_count.max(1);
                    let started = Instant::now();
                    let outputs = std::thread::scope(|scope| {
                        let mut handles = Vec::new();
                        for worker in 0..worker_count {
                            let plan = plan.clone();
                            let source_path = &facts.path;
                            let ffmpeg = &ffmpeg;
                            let ffprobe = &ffprobe;
                            let duration_seconds = facts.duration_seconds;
                            let has_audio = facts.has_audio;
                            let output = trial_dir.join(format!("worker-{worker}.mp4"));
                            handles.push(scope.spawn(move || {
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    run_proxy_transcode_with_plan_and_cancellation(
                                        ffmpeg,
                                        ffprobe,
                                        source_path,
                                        &output,
                                        duration_seconds,
                                        has_audio,
                                        &plan,
                                        |_| {},
                                        || started.elapsed() >= Duration::from_secs(120),
                                    )
                                }))
                                .unwrap_or_else(|_| {
                                    Err(crate::ProxyTranscodeFailure {
                                        message: "calibration transcode panicked".into(),
                                        outcome: ProxyGenerationOutcome {
                                            policy: ProxyEncoderPolicy::Software,
                                            status: "failed".into(),
                                            selected_encoder: "software".into(),
                                            recipe_key: PROXY_SOFTWARE_RECIPE_KEY.into(),
                                            device: None,
                                            fallback_used: false,
                                            elapsed_ms: 0,
                                            attempts: vec![],
                                            validation: None,
                                        },
                                    })
                                })
                            }));
                        }
                        handles
                            .into_iter()
                            .map(|handle| {
                                handle.join().unwrap_or_else(|_| {
                                    Err(crate::ProxyTranscodeFailure {
                                        message: "calibration worker panicked".into(),
                                        outcome: ProxyGenerationOutcome {
                                            policy: ProxyEncoderPolicy::Software,
                                            status: "failed".into(),
                                            selected_encoder: "software".into(),
                                            recipe_key: PROXY_SOFTWARE_RECIPE_KEY.into(),
                                            device: None,
                                            fallback_used: false,
                                            elapsed_ms: 0,
                                            attempts: vec![],
                                            validation: None,
                                        },
                                    })
                                })
                            })
                            .collect::<Vec<_>>()
                    });
                    let elapsed_ms = started
                        .elapsed()
                        .as_millis()
                        .max(1)
                        .min(u128::from(u64::MAX)) as u64;
                    let mut trial_successes = Vec::new();
                    for result in outputs {
                        match result {
                            Ok(outcome)
                                if outcome.status == "succeeded"
                                    && outcome
                                        .validation
                                        .as_ref()
                                        .is_some_and(|validation| validation.decoded) =>
                            {
                                trial_successes.push(outcome);
                            }
                            Ok(outcome) => failures
                                .push(format!("candidate returned status {}", outcome.status)),
                            Err(failure) => failures.push(bounded_text(&failure.message, 512)),
                        }
                    }
                    if trial_successes.len() == worker_count
                        && failures.is_empty()
                        && let Some(first) = trial_successes.first()
                    {
                        observed_encoder = Some(first.selected_encoder.clone());
                        match &accepted_reference {
                            Some((_, baseline)) => {
                                match proxy_quality_equivalent(
                                    baseline,
                                    first,
                                    facts.duration_seconds,
                                ) {
                                    Ok(()) => render_contract_equivalent = Some(true),
                                    Err(error) => {
                                        render_contract_equivalent = Some(false);
                                        failures.push(format!(
                                            "quality comparison rejected candidate: {error:#}"
                                        ));
                                    }
                                }
                            }
                            None => {
                                accepted_reference = Some((candidate.label.clone(), first.clone()));
                                render_contract_equivalent = Some(true);
                            }
                        }
                        output_identity = Some({
                            let mut hasher = blake3::Hasher::new();
                            for outcome in &trial_successes {
                                hasher.update(outcome.selected_encoder.as_bytes());
                                hasher.update(&outcome.elapsed_ms.to_le_bytes());
                                if let Some(validation) = &outcome.validation {
                                    hasher.update(validation.codec.as_bytes());
                                    hasher.update(&validation.width.to_le_bytes());
                                    hasher.update(&validation.height.to_le_bytes());
                                }
                            }
                            hasher.finalize().to_hex().to_string()
                        });
                        durations.push(elapsed_ms);
                    }
                    let _ = fs::remove_dir_all(&trial_dir);
                }
                let work_units = facts
                    .path
                    .metadata()
                    .map(|metadata| metadata.len().saturating_mul(candidate.worker_count as u64))
                    .ok();
                let timing = CalibrationTiming::from_samples(durations, work_units);
                let success = timing.is_some() && failures.is_empty();
                let mut measured_settings = candidate.clone();
                if let Some(encoder) = observed_encoder {
                    measured_settings.encoder = Some(encoder);
                }
                measurements.push(CalibrationCandidateOutcome {
                    settings: measured_settings,
                    succeeded: success,
                    timing,
                    failures,
                    output_identity,
                    quality: None,
                    render_contract_equivalent,
                });
            }
            Ok(())
        })();

        let (baseline_label, baseline_outcome) = match (accepted_reference, &run) {
            (Some((label, _)), Ok(())) => {
                let baseline = measurements.iter().find(|measurement| {
                    measurement.settings.label == label && measurement.succeeded
                });
                match baseline {
                    Some(baseline) => (label, baseline),
                    None => {
                        let mut report = failed_calibration_report(
                            cache_key.clone(),
                            identity.clone(),
                            started_at,
                            measurements.clone(),
                            "baseline proxy candidate did not complete all trials".into(),
                        );
                        self.persist_resource_calibration(&mut report)?;
                        return Ok(report);
                    }
                }
            }
            (_, Err(error)) => {
                let mut report = failed_calibration_report(
                    cache_key.clone(),
                    identity.clone(),
                    started_at,
                    measurements.clone(),
                    format!("{error:#}"),
                );
                self.persist_resource_calibration(&mut report)?;
                return Ok(report);
            }
            _ => {
                let mut report = failed_calibration_report(
                    cache_key.clone(),
                    identity.clone(),
                    started_at,
                    measurements.clone(),
                    "no proxy candidate produced a valid baseline output".into(),
                );
                self.persist_resource_calibration(&mut report)?;
                return Ok(report);
            }
        };
        let baseline_settings = baseline_outcome.settings.clone();
        let mut adopted = None;
        let mut selection_reason = "no candidate improved on the validated baseline".to_string();
        for measurement in &measurements {
            if !measurement.succeeded || measurement.settings.label == baseline_label {
                continue;
            }
            if measurement.render_contract_equivalent != Some(true) {
                continue;
            }
            if timing_better(measurement, baseline_outcome)
                && adopted
                    .as_ref()
                    .is_none_or(|current: &&CalibrationCandidateOutcome| {
                        timing_better(measurement, current)
                    })
            {
                adopted = Some(measurement);
            }
        }
        if let Some(adopted) = adopted {
            selection_reason = format!(
                "selected the fastest quality-equivalent candidate {}",
                adopted.settings.label
            );
        }
        let adopted_label = adopted.map(|adopted| adopted.settings.label.clone());
        let selected_settings = adopted
            .map(|adopted| adopted.settings.clone())
            .unwrap_or(baseline_settings);
        let selected_timing = measurements
            .iter()
            .find(|measurement| measurement.settings == selected_settings)
            .and_then(|measurement| measurement.timing.as_ref());
        let uncertainty = selected_timing
            .map(|timing| uncertainty_for(timing, timing.samples_ms.len()))
            .unwrap_or_else(|| CalibrationUncertainty {
                confidence: "none".into(),
                timing_relative_spread: None,
                eta_relative_range: None,
                note: "no valid timing sample".into(),
            });
        let mut report = ResourceCalibrationReport {
            schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
            cache_key,
            state: CalibrationState::Complete,
            identity,
            started_at,
            completed_at: Some(timestamp()),
            candidates: measurements,
            selected_settings: Some(selected_settings),
            selection_reason,
            quality: Some(CalibrationQualityDecision {
                baseline_candidate: baseline_label,
                adopted_candidate: adopted_label,
                reason: "proxy candidates were accepted only when existing output validation and render/audio contracts matched the baseline".into(),
            }),
            uncertainty,
            error: None,
        };
        self.persist_resource_calibration(&mut report)?;
        Ok(report)
    }

    /// Explicit bounded VAD calibration. It requires already-installed models;
    /// it never downloads. A generated sample without detected speech is
    /// reported as not-ready rather than fabricating a winning setting.
    #[allow(clippy::result_large_err)]
    pub fn calibrate_speech(
        &self,
        options: &SpeechCalibrationOptions,
    ) -> Result<ResourceCalibrationReport> {
        ensure!(
            !self.read_only,
            "calibration requires an explicitly writable catalog"
        );
        validate_trials(options.trials)?;
        validate_threads(&options.vad_threads, "VAD thread count")?;
        validate_speech_decode_threads(&options.decode_threads)?;
        validate_generated_duration(options.generated_duration_seconds)?;
        if let CalibrationMedia::Representative(path) = &options.media {
            validate_representative_media_metadata(path)?;
        }
        let sample_lease = if matches!(&options.media, CalibrationMedia::Representative(_)) {
            Some(self.acquire_local_heavy()?)
        } else {
            None
        };
        let mut candidates = Vec::new();
        for vad_threads in &options.vad_threads {
            candidates.push(CalibrationCandidateSettings {
                label: format!("vad={vad_threads};decode={DEFAULT_DECODE_THREADS};workers=1"),
                encoder: None,
                encoder_policy: None,
                worker_count: 1,
                vad_threads: *vad_threads,
                decode_threads: DEFAULT_DECODE_THREADS,
            });
        }
        candidates.sort();
        candidates.dedup();
        validate_candidate_budget(candidates.len(), options.trials)?;
        let models = self.model_status()?;
        let models_identity = model_identity(&models);
        let (media_identity, representative_path, representative_duration_ms) = match &options.media
        {
            CalibrationMedia::Generated => (
                CalibrationMediaIdentity {
                    kind: "generated".into(),
                    sample_identity: generated_sample_identity(
                        options.generated_duration_seconds,
                        "speech",
                    ),
                    duration_ms: options.generated_duration_seconds.saturating_mul(1000),
                    width: 0,
                    height: 0,
                    has_audio: true,
                },
                None,
                None,
            ),
            CalibrationMedia::Representative(path) => {
                let document = crate::probe_media_document(path)?;
                let duration_seconds = document
                    .get("format")
                    .and_then(|format| format.get("duration"))
                    .and_then(|duration| duration.as_str())
                    .and_then(|duration| duration.parse::<f64>().ok())
                    .filter(|duration| duration.is_finite() && *duration > 0.0)
                    .ok_or_else(|| anyhow!("calibration audio has no finite duration"))?;
                let duration_ms = (duration_seconds * 1000.0).round().min(u32::MAX as f64) as u32;
                validate_representative_duration(duration_ms)?;
                // Perform the full-content digest only after duration and
                // byte bounds pass. An arbitrary hours-long original is
                // never hashed/PCM-expanded as a bounded pilot.
                let (sample_identity, _) = representative_sample_identity(path)?;
                (
                    CalibrationMediaIdentity {
                        kind: "authorized_representative".into(),
                        sample_identity,
                        duration_ms,
                        width: 0,
                        height: 0,
                        has_audio: true,
                    },
                    Some(path.clone()),
                    Some(duration_ms),
                )
            }
        };
        let mut identity = CalibrationIdentity {
            schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
            recipe_key: STT_RECIPE_KEY.into(),
            recipe_version: RESOURCE_CALIBRATION_RECIPE_VERSION.into(),
            representation_kind: SPEECH_REPRESENTATION_KIND.into(),
            host: host_identity(),
            tools: tool_identities(),
            devices: hardware_device_identities(),
            models: models_identity,
            media: media_identity,
            conditions: CalibrationConditions {
                trials: options.trials,
                local_heavy_slots: 1,
                generated_duration_seconds: matches!(options.media, CalibrationMedia::Generated)
                    .then_some(options.generated_duration_seconds),
                note: "VAD candidates use fixed decode threads and are compared only against the same baseline regions; missing models are not downloaded".into(),
            },
            candidates: candidates.clone(),
            configuration_key: calibration_configuration_key(options.media.kind(), &candidates, None)?,
        };
        let cache_key = identity.cache_key()?;
        if !options.force
            && let Some(report) = self.exact_complete_calibration(&cache_key)?
        {
            return Ok(report);
        }
        let not_ready: Vec<String> = models
            .iter()
            .filter(|model| !model.ready)
            .map(|model| format!("{} ({})", model.name, model.integrity))
            .collect();
        if !not_ready.is_empty() {
            self.begin_resource_calibration(&identity, &cache_key)?;
            let started_at = timestamp();
            let mut report = not_ready_calibration_report(
                cache_key,
                identity,
                started_at,
                vec![],
                "speech models are not installed/ready",
                format!(
                    "speech models are not ready: {}; install them explicitly and rerun calibration",
                    not_ready.join(", ")
                ),
            );
            self.persist_resource_calibration(&mut report)?;
            return Ok(report);
        }

        let _heavy = match sample_lease {
            Some(lease) => lease,
            None => self.acquire_local_heavy()?,
        };
        self.begin_resource_calibration(&identity, &cache_key)?;
        let started_at = timestamp();
        for (identity_model, model) in identity.models.iter_mut().zip(models.iter()) {
            identity_model.content_digest = model_content_digest(Path::new(&model.path)).0;
        }
        let scratch = (representative_path.is_none())
            .then(|| create_scratch_root(self))
            .transpose()?;
        let generated_path = scratch
            .as_ref()
            .map(|scratch| {
                run_ffmpeg_fixture(&scratch.0, options.generated_duration_seconds, false, true)
            })
            .transpose()?;
        let source_path = match &options.media {
            CalibrationMedia::Generated => generated_path
                .as_ref()
                .ok_or_else(|| anyhow!("generated calibration audio was not created"))?,
            CalibrationMedia::Representative(_) => representative_path
                .as_ref()
                .ok_or_else(|| anyhow!("authorized representative media path missing"))?,
        };
        let duration_ms = match &options.media {
            CalibrationMedia::Generated => options.generated_duration_seconds.saturating_mul(1000),
            CalibrationMedia::Representative(_) => representative_duration_ms
                .ok_or_else(|| anyhow!("representative media duration missing"))?,
        };
        let pcm = match decode_calibration_pcm(source_path) {
            Ok(pcm) => pcm,
            Err(error) => {
                let mut report = failed_calibration_report(
                    cache_key,
                    identity,
                    started_at,
                    vec![],
                    format!("decode calibration audio: {error:#}"),
                );
                self.persist_resource_calibration(&mut report)?;
                return Ok(report);
            }
        };
        let model_dir = crate::analysis::engine::model_dir(self.database_path())?;
        let whisper = model_dir.join(WHISPER_MODEL.file_name);
        let vad = model_dir.join(VAD_MODEL.file_name);
        let run = self.run_speech_calibration_with_factory(
            options,
            &identity,
            &cache_key,
            &started_at,
            &pcm,
            duration_ms,
            |vad_threads, decode_threads| {
                Ok(Box::new(WhisperEngine::with_threads(
                    &whisper,
                    &vad,
                    vad_threads,
                    decode_threads,
                )?) as Box<dyn SttEngine>)
            },
        );
        match run {
            Ok(report) => Ok(report),
            Err(error) => {
                let mut report = failed_calibration_report(
                    cache_key,
                    identity,
                    started_at,
                    vec![],
                    format!("{error:#}"),
                );
                self.persist_resource_calibration(&mut report)?;
                Ok(report)
            }
        }
    }

    /// Shared production/fake-engine implementation. The closure is the only
    /// engine seam; candidate validation, timing, region quality comparisons,
    /// selection and persistence are identical for real and fake engines.
    #[allow(clippy::too_many_arguments)]
    fn run_speech_calibration_with_factory<F>(
        &self,
        options: &SpeechCalibrationOptions,
        identity: &CalibrationIdentity,
        cache_key: &str,
        started_at: &str,
        pcm: &[f32],
        duration_ms: u32,
        mut open_engine: F,
    ) -> Result<ResourceCalibrationReport>
    where
        F: FnMut(usize, usize) -> Result<Box<dyn SttEngine>>,
    {
        let mut measurements = Vec::new();
        let mut regions_by_label: HashMap<String, Vec<SpeechRegion>> = HashMap::new();
        for candidate in &identity.candidates {
            self.touch_resource_calibration(cache_key).ok();
            let mut durations = Vec::new();
            let mut failures = Vec::new();
            let mut candidate_regions: Option<Vec<SpeechRegion>> = None;
            for _trial in 0..options.trials {
                let engine = match open_engine(candidate.vad_threads, candidate.decode_threads) {
                    Ok(engine) => engine,
                    Err(error) => {
                        failures.push(bounded_text(
                            &format!("load speech engine failed: {error:#}"),
                            512,
                        ));
                        continue;
                    }
                };
                let started = Instant::now();
                match engine.detect_speech(pcm) {
                    Ok(regions) => {
                        let (valid, reason) = speech_regions_valid(&regions, duration_ms);
                        if !valid {
                            failures.push(reason);
                        } else {
                            if let Some(previous) = &candidate_regions {
                                let comparison =
                                    compare_speech_regions(previous, &regions, duration_ms);
                                if !comparison.equivalent {
                                    failures.push(
                                        "VAD regions changed between trials for the same candidate"
                                            .into(),
                                    );
                                }
                            }
                            candidate_regions = Some(regions);
                            durations.push(
                                started
                                    .elapsed()
                                    .as_millis()
                                    .max(1)
                                    .min(u128::from(u64::MAX))
                                    as u64,
                            );
                        }
                    }
                    Err(error) => {
                        failures.push(bounded_text(&format!("VAD failed: {error:#}"), 512))
                    }
                }
            }
            let timing = CalibrationTiming::from_samples(durations, Some(1));
            let output_identity = candidate_regions
                .as_ref()
                .map(|regions| region_digest(regions));
            if let Some(regions) = candidate_regions.take() {
                regions_by_label.insert(candidate.label.clone(), regions);
            }
            measurements.push(CalibrationCandidateOutcome {
                settings: candidate.clone(),
                succeeded: timing.is_some() && failures.is_empty(),
                timing,
                failures,
                output_identity,
                quality: None,
                render_contract_equivalent: None,
            });
        }
        let baseline_index = measurements.iter().position(|measurement| {
            measurement.succeeded
                && regions_by_label
                    .get(&measurement.settings.label)
                    .is_some_and(|regions| !regions.is_empty())
        });
        let Some(baseline_index) = baseline_index else {
            let empty_valid_measurement = measurements.iter().any(|measurement| {
                measurement.succeeded
                    && regions_by_label
                        .get(&measurement.settings.label)
                        .is_some_and(Vec::is_empty)
            });
            let note = if empty_valid_measurement {
                "the calibration sample contained no detected speech"
            } else {
                "no VAD candidate produced a successful valid baseline region set"
            };
            let state = if empty_valid_measurement {
                CalibrationState::NotReady
            } else {
                CalibrationState::Failed
            };
            let mut report = ResourceCalibrationReport {
                schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
                cache_key: cache_key.to_string(),
                state,
                identity: identity.clone(),
                started_at: started_at.to_string(),
                completed_at: Some(timestamp()),
                candidates: measurements,
                selected_settings: None,
                selection_reason: "no VAD setting was adopted".into(),
                quality: None,
                uncertainty: CalibrationUncertainty {
                    confidence: "none".into(),
                    timing_relative_spread: None,
                    eta_relative_range: None,
                    note: note.into(),
                },
                error: Some(format!("{note}; no VAD thread setting was adopted")),
            };
            self.persist_resource_calibration(&mut report)?;
            return Ok(report);
        };
        let baseline_label = measurements[baseline_index].settings.label.clone();
        let baseline_regions = regions_by_label
            .get(&baseline_label)
            .cloned()
            .ok_or_else(|| anyhow!("baseline regions disappeared"))?;
        for measurement in &mut measurements {
            if let Some(regions) = regions_by_label.get(&measurement.settings.label) {
                let comparison = compare_speech_regions(&baseline_regions, regions, duration_ms);
                if measurement.settings.label == baseline_label {
                    measurement.quality = Some(SpeechRegionComparison {
                        equivalent: comparison.reference_valid,
                        ..comparison
                    });
                } else if measurement.succeeded && comparison.equivalent {
                    measurement.quality = Some(comparison);
                } else if measurement.succeeded {
                    measurement.failures.push(comparison.reason.clone());
                    measurement.succeeded = false;
                    measurement.quality = Some(comparison);
                }
            }
        }
        let baseline = &measurements[baseline_index];
        let mut adopted = None;
        for measurement in &measurements {
            if !measurement.succeeded || measurement.settings.label == baseline_label {
                continue;
            }
            if measurement
                .quality
                .as_ref()
                .is_some_and(|quality| quality.equivalent)
                && timing_better(measurement, baseline)
                && adopted
                    .as_ref()
                    .is_none_or(|current: &&CalibrationCandidateOutcome| {
                        timing_better(measurement, current)
                    })
            {
                adopted = Some(measurement);
            }
        }
        let adopted_label = adopted.map(|measurement| measurement.settings.label.clone());
        let selected_settings = adopted
            .map(|measurement| measurement.settings.clone())
            .unwrap_or_else(|| baseline.settings.clone());
        let selection_reason = match &adopted_label {
            Some(label) => {
                format!("selected the fastest valid quality-equivalent candidate {label}")
            }
            None => "baseline was the fastest valid quality-equivalent candidate".into(),
        };
        let selected_timing = measurements
            .iter()
            .find(|measurement| measurement.settings == selected_settings)
            .and_then(|measurement| measurement.timing.as_ref());
        let uncertainty = selected_timing
            .map(|timing| uncertainty_for(timing, timing.samples_ms.len()))
            .unwrap_or_else(|| CalibrationUncertainty {
                confidence: "none".into(),
                timing_relative_spread: None,
                eta_relative_range: None,
                note: "no valid timing sample".into(),
            });
        let mut report = ResourceCalibrationReport {
            schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
            cache_key: cache_key.to_string(),
            state: CalibrationState::Complete,
            identity: identity.clone(),
            started_at: started_at.to_string(),
            completed_at: Some(timestamp()),
            candidates: measurements,
            selected_settings: Some(selected_settings),
            selection_reason,
            quality: Some(CalibrationQualityDecision {
                baseline_candidate: baseline_label,
                adopted_candidate: adopted_label,
                reason: "a faster VAD thread setting is adopted only when it has a successful, valid, measured region set exactly matching the baseline".into(),
            }),
            uncertainty,
            error: None,
        };
        self.persist_resource_calibration(&mut report)?;
        Ok(report)
    }

    /// Seed an ETA estimate for a prepared work list. The returned estimate is
    /// deliberately uncertain (`None` rate) when calibration is missing,
    /// explicitly overridden, or measured for a different encoder/worker
    /// configuration. A range is attached whenever a calibrated rate is used.
    #[allow(clippy::too_many_arguments)]
    pub fn calibrated_batch_estimate(
        &self,
        representation_kind: &str,
        recipe_key: &str,
        task_work_units: &[u64],
        worker_count: usize,
        encoder: Option<&str>,
        media_sample_identity: Option<&str>,
        overrides: &ResourceOverrides,
    ) -> Result<BatchProgressEstimate> {
        let total_work = task_work_units.iter().copied().sum::<u64>();
        let selection = self.resolve_resource_selection(
            representation_kind,
            recipe_key,
            Some(total_work),
            media_sample_identity,
            overrides,
        )?;
        let compatible = selection.source == "calibration"
            && worker_count > 0
            && selection.worker_count == worker_count
            && match encoder {
                Some(encoder) => selection.encoder.as_deref() == Some(encoder),
                None => true,
            };
        let estimator = BatchEstimator::new(
            task_work_units,
            worker_count,
            if compatible {
                selection
                    .timing
                    .as_ref()
                    .and_then(|timing| per_worker_rate(timing, worker_count.max(1)))
                    .map(|rate| RateHistory {
                        work_units_per_worker_second: rate,
                        samples: selection
                            .timing
                            .as_ref()
                            .map(|timing| timing.samples_ms.len() as u64)
                            .unwrap_or(0),
                    })
                    .unwrap_or_default()
            } else {
                RateHistory::default()
            },
        );
        let mut estimate = estimator.estimate(task_work_units);
        if compatible {
            if let Some(seconds) = estimate.estimated_remaining_seconds {
                let relative = selection
                    .uncertainty
                    .eta_relative_range
                    .unwrap_or(0.25)
                    .max(0.02);
                estimate.confidence = selection.uncertainty.confidence.clone();
                estimate.estimated_remaining_seconds_range = Some([
                    (seconds * (1.0 - relative)).max(0.0),
                    seconds * (1.0 + relative),
                ]);
            }
        } else {
            estimate.confidence = "mismatched_configuration".into();
            estimate.estimated_remaining_seconds = None;
            estimate.estimated_remaining_seconds_range = None;
        }
        Ok(estimate)
    }
}

fn failed_calibration_report(
    cache_key: String,
    identity: CalibrationIdentity,
    started_at: String,
    candidates: Vec<CalibrationCandidateOutcome>,
    error: String,
) -> ResourceCalibrationReport {
    ResourceCalibrationReport {
        schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
        cache_key,
        state: CalibrationState::Failed,
        identity,
        started_at,
        completed_at: Some(timestamp()),
        candidates,
        selected_settings: None,
        selection_reason: "failed calibration; no setting was adopted".into(),
        quality: None,
        uncertainty: CalibrationUncertainty {
            confidence: "none".into(),
            timing_relative_spread: None,
            eta_relative_range: None,
            note: "no calibrated setting or ETA is available from a failed run".into(),
        },
        error: Some(bounded_text(&error, 2048)),
    }
}

fn timing_better(
    measurement: &CalibrationCandidateOutcome,
    baseline: &CalibrationCandidateOutcome,
) -> bool {
    measurement
        .timing
        .as_ref()
        .zip(baseline.timing.as_ref())
        .is_some_and(|(left, right)| left.mean_ms < right.mean_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(start_ms: u32, end_ms: u32) -> SpeechRegion {
        SpeechRegion { start_ms, end_ms }
    }

    #[test]
    fn explicit_speech_thread_overrides_have_distinct_actual_history_keys() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let selection = |vad_threads, decode_threads| {
            catalog
                .resolve_resource_selection(
                    SPEECH_REPRESENTATION_KIND,
                    STT_RECIPE_KEY,
                    None,
                    None,
                    &ResourceOverrides {
                        vad_threads: Some(vad_threads),
                        decode_threads: Some(decode_threads),
                        ..ResourceOverrides::default()
                    },
                )
                .unwrap()
        };
        assert_ne!(selection(1, 4).history_key, selection(2, 4).history_key);
        assert_ne!(selection(2, 4).history_key, selection(2, 8).history_key);
        assert_eq!(selection(2, 8).source, "explicit_override");
    }

    #[cfg(unix)]
    #[test]
    fn another_live_process_is_not_interrupted_without_proc_metadata() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let live = owner_is_live(Some("external-owner"), Some(child.id()), None, None);
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(live, "portable liveness must not require Linux /proc");
    }

    #[test]
    fn full_model_content_evidence_does_not_change_the_lightweight_cache_key() {
        let mut identity = fake_proxy_identity();
        identity.models.push(CalibrationModelIdentity {
            role: "voice_activity_detection".into(),
            name: "synthetic-model".into(),
            integrity: "present_unverified;stat:synthetic".into(),
            bytes: 10,
            content_digest: None,
        });
        let key = identity.cache_key().unwrap();
        identity.models[0].content_digest = Some("blake3:synthetic-evidence".into());
        assert_eq!(key, identity.cache_key().unwrap());
        identity.models[0].integrity = "present_unverified;stat:changed".into();
        assert_ne!(key, identity.cache_key().unwrap());
    }

    #[test]
    fn varying_regions_across_trials_cannot_be_adopted_from_the_last_trial_only() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let options = SpeechCalibrationOptions {
            vad_threads: vec![1, 2],
            trials: 2,
            ..SpeechCalibrationOptions::default()
        };
        let calls = std::cell::Cell::new(0);
        let report = run_fake_speech(
            &catalog,
            &options,
            |threads| {
                let count = calls.get();
                calls.set(count + 1);
                Ok(vec![region(
                    100,
                    if threads == 2 && count == 2 { 450 } else { 400 },
                )])
            },
            |threads| {
                if threads == 1 {
                    Duration::from_millis(10)
                } else {
                    Duration::ZERO
                }
            },
        );
        assert_eq!(report.selected_settings.unwrap().vad_threads, 1);
        let rejected = report
            .candidates
            .iter()
            .find(|candidate| candidate.settings.vad_threads == 2)
            .unwrap();
        assert!(!rejected.succeeded);
        assert!(
            rejected
                .failures
                .iter()
                .any(|failure| failure.contains("between trials"))
        );
    }

    #[test]
    fn speech_region_comparison_requires_exact_equal_intervals() {
        let baseline = vec![region(100, 500), region(700, 900)];
        let same = baseline.clone();
        assert!(compare_speech_regions(&baseline, &same, 1000).equivalent);

        let shifted = vec![region(100, 501), region(700, 900)];
        let comparison = compare_speech_regions(&baseline, &shifted, 1000);
        assert!(!comparison.equivalent);
        assert_eq!(comparison.mismatch_count, 1);
    }

    #[test]
    fn speech_region_comparison_rejects_invalid_or_overlapping_regions() {
        let baseline = vec![region(100, 500)];
        for candidate in [
            vec![region(500, 400)],
            vec![region(100, 1001)],
            vec![region(100, 500), region(450, 600)],
        ] {
            let comparison = compare_speech_regions(&baseline, &candidate, 1000);
            assert!(!comparison.equivalent);
            assert!(!comparison.candidate_valid);
        }
    }

    #[test]
    fn region_digest_is_stable() {
        let regions = vec![region(100, 500), region(700, 900)];
        assert_eq!(region_digest(&regions), region_digest(&regions));
        assert_ne!(region_digest(&regions), region_digest(&[region(100, 501)]));
    }

    #[test]
    fn explicit_overrides_validate_ranges_and_are_not_calibrated_eta() {
        let mut overrides = ResourceOverrides {
            vad_threads: Some(0),
            ..ResourceOverrides::default()
        };
        assert!(
            override_selection(
                SPEECH_REPRESENTATION_KIND,
                STT_RECIPE_KEY,
                &overrides,
                String::new()
            )
            .is_err()
        );
        overrides.vad_threads = Some(3);
        let selection = override_selection(
            SPEECH_REPRESENTATION_KIND,
            STT_RECIPE_KEY,
            &overrides,
            String::new(),
        )
        .unwrap();
        assert_eq!(selection.source, "explicit_override");
        assert_eq!(selection.vad_threads, 3);
        assert!(selection.eta.estimated_remaining_seconds.is_none());

        let unsupported = ResourceOverrides {
            encoder: Some("vaapi".into()),
            ..ResourceOverrides::default()
        };
        assert!(
            override_selection(
                PROXY_REPRESENTATION_KIND,
                PROXY_SOFTWARE_RECIPE_KEY,
                &unsupported,
                String::new(),
            )
            .is_err()
        );
        let parallel_speech = ResourceOverrides {
            worker_count: Some(2),
            ..ResourceOverrides::default()
        };
        assert!(
            override_selection(
                SPEECH_REPRESENTATION_KIND,
                STT_RECIPE_KEY,
                &parallel_speech,
                String::new(),
            )
            .is_err()
        );
    }

    #[test]
    fn unbiased_resource_selection_never_runs_a_hardware_probe() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let before = crate::proxy_encoding::encoder_plan_resolution_count();
        let selection = catalog
            .resolve_resource_selection(
                PROXY_REPRESENTATION_KIND,
                PROXY_SOFTWARE_RECIPE_KEY,
                Some(1024),
                None,
                &ResourceOverrides::default(),
            )
            .unwrap();
        let after = crate::proxy_encoding::encoder_plan_resolution_count();
        assert_eq!(selection.source, "default_uncalibrated");
        assert_eq!(selection.encoder.as_deref(), Some("software"));
        assert_eq!(
            before, after,
            "ordinary selection/readiness must not initialize or benchmark an encoder"
        );
    }

    #[test]
    fn resource_selection_entrypoint_rejects_overrides_it_cannot_apply() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        assert!(
            catalog
                .resolve_resource_selection(
                    SPEECH_REPRESENTATION_KIND,
                    STT_RECIPE_KEY,
                    None,
                    None,
                    &ResourceOverrides {
                        worker_count: Some(2),
                        ..ResourceOverrides::default()
                    },
                )
                .is_err()
        );
        assert!(
            catalog
                .resolve_resource_selection(
                    PROXY_REPRESENTATION_KIND,
                    PROXY_SOFTWARE_RECIPE_KEY,
                    None,
                    None,
                    &ResourceOverrides {
                        encoder: Some("videotoolbox".into()),
                        ..ResourceOverrides::default()
                    },
                )
                .is_err()
        );
        let forced_software = catalog
            .resolve_resource_selection(
                PROXY_REPRESENTATION_KIND,
                PROXY_SOFTWARE_RECIPE_KEY,
                None,
                None,
                &ResourceOverrides {
                    encoder: Some("software".into()),
                    ..ResourceOverrides::default()
                },
            )
            .unwrap();
        assert_eq!(forced_software.source, "explicit_override");
        assert_eq!(forced_software.encoder.as_deref(), Some("software"));
    }

    #[test]
    fn one_sample_timing_reports_a_wide_range_not_false_precision() {
        let timing = CalibrationTiming::from_samples(vec![100], Some(100)).unwrap();
        let uncertainty = uncertainty_for(&timing, 1);
        let eta = eta_from_timing(Some(&timing), Some(300), &uncertainty, "test");
        assert_eq!(eta.estimated_remaining_seconds, Some(0.3));
        let range = eta.range_seconds.unwrap();
        assert!(range[0] < 0.3 && range[1] > 0.3);
        assert_eq!(uncertainty.confidence, "low_sample_count");
    }

    fn fake_proxy_report(
        catalog: &Catalog,
        identity: CalibrationIdentity,
    ) -> ResourceCalibrationReport {
        let cache_key = identity.cache_key().unwrap();
        catalog
            .begin_resource_calibration(&identity, &cache_key)
            .unwrap();
        let settings = identity.candidates[0].clone();
        let timing = CalibrationTiming::from_samples(vec![100], Some(100)).unwrap();
        let uncertainty = uncertainty_for(&timing, 1);
        let mut report = ResourceCalibrationReport {
            schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
            cache_key,
            state: CalibrationState::Complete,
            identity,
            started_at: timestamp(),
            completed_at: Some(timestamp()),
            candidates: vec![CalibrationCandidateOutcome {
                settings: settings.clone(),
                succeeded: true,
                timing: Some(timing),
                failures: vec![],
                output_identity: Some("synthetic-output".into()),
                quality: None,
                render_contract_equivalent: Some(true),
            }],
            selected_settings: Some(settings),
            selection_reason: "synthetic complete calibration".into(),
            quality: None,
            uncertainty,
            error: None,
        };
        report.identity.candidates = report
            .candidates
            .iter()
            .map(|candidate| candidate.settings.clone())
            .collect();
        catalog.persist_resource_calibration(&mut report).unwrap();
        report
    }

    fn fake_proxy_identity() -> CalibrationIdentity {
        let candidate = CalibrationCandidateSettings {
            label: "policy=software;encoder=software;workers=1".into(),
            encoder: Some("software".into()),
            encoder_policy: Some("software".into()),
            worker_count: 1,
            vad_threads: 0,
            decode_threads: 0,
        };
        CalibrationIdentity {
            schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
            recipe_key: PROXY_SOFTWARE_RECIPE_KEY.into(),
            recipe_version: RESOURCE_CALIBRATION_RECIPE_VERSION.into(),
            representation_kind: PROXY_REPRESENTATION_KIND.into(),
            host: host_identity(),
            tools: tool_identities(),
            devices: hardware_device_identities(),
            models: vec![],
            media: CalibrationMediaIdentity {
                kind: "generated".into(),
                sample_identity: "generated:test".into(),
                duration_ms: 1_000,
                width: 320,
                height: 180,
                has_audio: true,
            },
            conditions: CalibrationConditions {
                trials: 1,
                local_heavy_slots: 1,
                generated_duration_seconds: Some(1),
                note: "test".into(),
            },
            candidates: vec![candidate.clone()],
            configuration_key: calibration_configuration_key(
                "generated",
                &[candidate],
                Some(ProxyEncoderPolicy::Software),
            )
            .unwrap(),
        }
    }

    #[test]
    fn a_generated_contract_calibration_cannot_predict_unrelated_footage() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        fake_proxy_report(&catalog, fake_proxy_identity());
        let selection = catalog
            .resolve_resource_selection(
                PROXY_REPRESENTATION_KIND,
                PROXY_SOFTWARE_RECIPE_KEY,
                Some(1_000_000),
                None,
                &ResourceOverrides::default(),
            )
            .unwrap();
        assert_eq!(selection.source, "calibration");
        assert_eq!(selection.sample_compatibility, "generated_contract_only");
        assert!(selection.timing.is_none());
        assert!(selection.eta.estimated_remaining_seconds.is_none());
        assert!(
            catalog
                .calibration_rate_history(
                    PROXY_REPRESENTATION_KIND,
                    PROXY_SOFTWARE_RECIPE_KEY,
                    Some("software"),
                    1,
                    None
                )
                .unwrap()
                .samples
                == 0
        );
    }

    #[test]
    fn complete_matching_calibration_is_reused_and_seeds_eta() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let report = fake_proxy_report(&catalog, fake_proxy_identity());
        let selection = catalog
            .resolve_resource_selection(
                PROXY_REPRESENTATION_KIND,
                PROXY_SOFTWARE_RECIPE_KEY,
                Some(300),
                Some(&report.identity.media.sample_identity),
                &ResourceOverrides::default(),
            )
            .unwrap();
        assert_eq!(selection.source, "calibration");
        assert_eq!(selection.state, "complete");
        assert_eq!(selection.worker_count, 1);
        assert!(selection.eta.estimated_remaining_seconds.is_some());
        let range = selection.eta.range_seconds.unwrap();
        assert!(range[0] < range[1]);
        assert_eq!(selection.sample_compatibility, "exact_generated");
        assert_eq!(
            selection.uncertainty.eta_relative_range,
            Some(0.25),
            "one observation of the exact fixture still retains timing uncertainty"
        );
        let history = catalog
            .calibration_rate_history(
                PROXY_REPRESENTATION_KIND,
                PROXY_SOFTWARE_RECIPE_KEY,
                Some("software"),
                1,
                Some(&report.identity.media.sample_identity),
            )
            .unwrap();
        assert_eq!(history.samples, 1);
    }

    #[test]
    fn calibrated_eta_rejects_mismatched_encoder_or_workers_and_is_not_n_fold_inflated() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let mut identity = fake_proxy_identity();
        identity.candidates[0].worker_count = 2;
        identity.candidates[0].label = "policy=software;encoder=software;workers=2".into();
        identity.configuration_key = calibration_configuration_key(
            "generated",
            &identity.candidates,
            Some(ProxyEncoderPolicy::Software),
        )
        .unwrap();
        let cache_key = identity.cache_key().unwrap();
        catalog
            .begin_resource_calibration(&identity, &cache_key)
            .unwrap();
        let timing = CalibrationTiming::from_samples(vec![100], Some(200)).unwrap();
        let settings = identity.candidates[0].clone();
        let uncertainty = uncertainty_for(&timing, 1);
        let mut report = ResourceCalibrationReport {
            schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
            cache_key,
            state: CalibrationState::Complete,
            identity,
            started_at: timestamp(),
            completed_at: Some(timestamp()),
            candidates: vec![CalibrationCandidateOutcome {
                settings: settings.clone(),
                succeeded: true,
                timing: Some(timing),
                failures: vec![],
                output_identity: None,
                quality: None,
                render_contract_equivalent: Some(true),
            }],
            selected_settings: Some(settings),
            selection_reason: "synthetic".into(),
            quality: None,
            uncertainty,
            error: None,
        };
        catalog.persist_resource_calibration(&mut report).unwrap();

        let estimate = catalog
            .calibrated_batch_estimate(
                PROXY_REPRESENTATION_KIND,
                PROXY_SOFTWARE_RECIPE_KEY,
                &[100, 100],
                2,
                Some("software"),
                Some(&report.identity.media.sample_identity),
                &ResourceOverrides::default(),
            )
            .unwrap();
        let seconds = estimate.estimated_remaining_seconds.unwrap();
        assert!(
            (seconds - 0.1).abs() < 0.001,
            "aggregate 2000 units/s for two workers must seed 1000 units/worker/s, got {seconds}"
        );
        assert!(estimate.estimated_remaining_seconds_range.is_some());

        let mismatch_workers = catalog
            .calibrated_batch_estimate(
                PROXY_REPRESENTATION_KIND,
                PROXY_SOFTWARE_RECIPE_KEY,
                &[100, 100],
                1,
                Some("software"),
                Some(&report.identity.media.sample_identity),
                &ResourceOverrides::default(),
            )
            .unwrap();
        assert!(mismatch_workers.estimated_remaining_seconds.is_none());
        assert_eq!(mismatch_workers.confidence, "mismatched_configuration");

        let mismatch_encoder = catalog
            .calibrated_batch_estimate(
                PROXY_REPRESENTATION_KIND,
                PROXY_SOFTWARE_RECIPE_KEY,
                &[100, 100],
                2,
                Some("vaapi"),
                Some(&report.identity.media.sample_identity),
                &ResourceOverrides::default(),
            )
            .unwrap();
        assert!(mismatch_encoder.estimated_remaining_seconds.is_none());
    }

    #[test]
    fn changed_host_identity_is_stale_and_never_supplies_an_eta() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let mut identity = fake_proxy_identity();
        identity.host.arch = "different-host".into();
        fake_proxy_report(&catalog, identity);
        let selection = catalog
            .resolve_resource_selection(
                PROXY_REPRESENTATION_KIND,
                PROXY_SOFTWARE_RECIPE_KEY,
                Some(300),
                None,
                &ResourceOverrides::default(),
            )
            .unwrap();
        assert_eq!(selection.source, "default_uncalibrated");
        assert_eq!(selection.state, "missing");
        assert!(selection.eta.estimated_remaining_seconds.is_none());
        let status = catalog
            .resource_calibration_status(
                Some(PROXY_SOFTWARE_RECIPE_KEY),
                Some(PROXY_REPRESENTATION_KIND),
            )
            .unwrap();
        assert!(!status.entries[0].host_tools_match_current);
        assert!(!status.entries[0].reusable);
    }

    #[test]
    fn abandoned_calibration_recovery_does_not_block_catalog_readers() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite3");
        let catalog = Catalog::open(&path).unwrap();
        let mut report = fake_proxy_report(&catalog, fake_proxy_identity());
        report.state = CalibrationState::Running;
        report.completed_at = None;
        catalog.persist_resource_calibration(&mut report).unwrap();
        let writer = catalog.connection().unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        let opened =
            Catalog::open(&path).expect("maintenance cannot force a write lock on readers");
        let status = opened.resource_calibration_status(None, None).unwrap();
        assert_eq!(status.entries[0].state, CalibrationState::Interrupted);
        assert!(!status.entries[0].reusable);
        writer.execute_batch("ROLLBACK").unwrap();
        opened.recover_interrupted_resource_calibrations().unwrap();
        let state: String = opened
            .connection()
            .unwrap()
            .query_row(
                "SELECT state FROM resource_calibrations WHERE cache_key=?1",
                [&report.cache_key],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "interrupted");
    }

    #[test]
    fn interrupted_run_is_never_reused_after_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite3");
        let catalog = Catalog::open(&path).unwrap();
        let mut report = fake_proxy_report(&catalog, fake_proxy_identity());
        report.state = CalibrationState::Running;
        report.completed_at = None;
        catalog.persist_resource_calibration(&mut report).unwrap();
        drop(catalog);

        let reopened = Catalog::open(&path).unwrap();
        let status = reopened
            .resource_calibration_status(
                Some(PROXY_SOFTWARE_RECIPE_KEY),
                Some(PROXY_REPRESENTATION_KIND),
            )
            .unwrap();
        assert_eq!(status.entries[0].state, CalibrationState::Interrupted);
        assert!(!status.entries[0].reusable);
        let selection = reopened
            .resolve_resource_selection(
                PROXY_REPRESENTATION_KIND,
                PROXY_SOFTWARE_RECIPE_KEY,
                Some(300),
                None,
                &ResourceOverrides::default(),
            )
            .unwrap();
        assert_eq!(selection.source, "default_uncalibrated");
    }
    #[test]
    fn history_keys_are_qualified_by_environment_and_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let one_worker = catalog
            .qualified_history_key(
                STT_TASK_KEY,
                SPEECH_REPRESENTATION_KIND,
                STT_RECIPE_KEY,
                None,
                1,
                1,
                DEFAULT_DECODE_THREADS,
            )
            .unwrap();
        let two_workers = catalog
            .qualified_history_key(
                STT_TASK_KEY,
                SPEECH_REPRESENTATION_KIND,
                STT_RECIPE_KEY,
                None,
                2,
                1,
                DEFAULT_DECODE_THREADS,
            )
            .unwrap();
        let two_vad_threads = catalog
            .qualified_history_key(
                STT_TASK_KEY,
                SPEECH_REPRESENTATION_KIND,
                STT_RECIPE_KEY,
                None,
                1,
                2,
                DEFAULT_DECODE_THREADS,
            )
            .unwrap();
        assert_ne!(one_worker, two_workers);
        assert_ne!(one_worker, two_vad_threads);
        assert!(one_worker.starts_with(STT_TASK_KEY));
        assert!(one_worker.contains(".h"));
        assert!(one_worker.contains(".c"));
    }

    #[test]
    fn file_and_model_identity_changes_invalidate_same_path_content() {
        let directory = tempfile::tempdir().unwrap();
        let tool_path = directory.path().join("ffmpeg");
        fs::write(&tool_path, b"tool-v1").unwrap();
        let first_tool = file_identity(&tool_path);
        fs::write(&tool_path, b"tool-v2-longer").unwrap();
        let second_tool = file_identity(&tool_path);
        assert_ne!(first_tool, second_tool);

        let model_path = directory.path().join("model.bin");
        fs::write(&model_path, b"model-v1").unwrap();
        let first_digest = model_content_digest(&model_path);
        fs::write(&model_path, b"model-v2").unwrap();
        let second_digest = model_content_digest(&model_path);
        assert_ne!(first_digest.0, second_digest.0);
    }

    #[test]
    fn changed_device_or_model_identity_is_not_reusable() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let mut device_identity = fake_proxy_identity();
        device_identity.devices[0].identity = "different-device".into();
        fake_proxy_report(&catalog, device_identity);
        let selection = catalog
            .resolve_resource_selection(
                PROXY_REPRESENTATION_KIND,
                PROXY_SOFTWARE_RECIPE_KEY,
                Some(300),
                None,
                &ResourceOverrides::default(),
            )
            .unwrap();
        assert_eq!(selection.source, "default_uncalibrated");

        let mut tool_identity = fake_proxy_identity();
        tool_identity.tools[0].version = "different-tool-build".into();
        fake_proxy_report(&catalog, tool_identity);
        let status = catalog
            .resource_calibration_status(
                Some(PROXY_SOFTWARE_RECIPE_KEY),
                Some(PROXY_REPRESENTATION_KIND),
            )
            .unwrap();
        assert!(status.entries.iter().all(|entry| !entry.reusable));
    }

    #[test]
    fn opening_a_second_catalog_does_not_interrupt_a_live_run() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite3");
        let catalog = Catalog::open(&path).unwrap();
        let identity = fake_proxy_identity();
        let cache_key = identity.cache_key().unwrap();
        catalog
            .begin_resource_calibration(&identity, &cache_key)
            .unwrap();
        let second = Catalog::open(&path).unwrap();
        let status = second
            .resource_calibration_status(
                Some(PROXY_SOFTWARE_RECIPE_KEY),
                Some(PROXY_REPRESENTATION_KIND),
            )
            .unwrap();
        assert_eq!(status.entries[0].state, CalibrationState::Running);
        assert!(!status.entries[0].reusable);

        active_calibration_runs()
            .lock()
            .unwrap()
            .remove(&active_run_key(&path, &cache_key));
        let connection = catalog.connection().unwrap();
        connection
            .execute(
                "UPDATE resource_calibrations SET owner_pid=?1,owner_process_start='abandoned' WHERE cache_key=?2",
                params![i64::from(u32::MAX - 1), cache_key],
            )
            .unwrap();
        let reopened = Catalog::open(&path).unwrap();
        let status = reopened
            .resource_calibration_status(
                Some(PROXY_SOFTWARE_RECIPE_KEY),
                Some(PROXY_REPRESENTATION_KIND),
            )
            .unwrap();
        assert_eq!(status.entries[0].state, CalibrationState::Interrupted);
    }

    #[test]
    fn persistence_failure_rolls_back_report_and_observations_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let identity = fake_proxy_identity();
        let cache_key = identity.cache_key().unwrap();
        catalog
            .begin_resource_calibration(&identity, &cache_key)
            .unwrap();
        let connection = catalog.connection().unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER fail_calibration_observation BEFORE INSERT ON resource_calibration_observations BEGIN SELECT RAISE(ABORT, 'injected observation failure'); END;",
            )
            .unwrap();
        let settings = identity.candidates[0].clone();
        let timing = CalibrationTiming::from_samples(vec![100], Some(100)).unwrap();
        let mut report = ResourceCalibrationReport {
            schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
            cache_key: cache_key.clone(),
            state: CalibrationState::Complete,
            identity,
            started_at: timestamp(),
            completed_at: Some(timestamp()),
            candidates: vec![CalibrationCandidateOutcome {
                settings: settings.clone(),
                succeeded: true,
                timing: Some(timing),
                failures: vec![],
                output_identity: None,
                quality: None,
                render_contract_equivalent: Some(true),
            }],
            selected_settings: Some(settings),
            selection_reason: "synthetic".into(),
            quality: None,
            uncertainty: CalibrationUncertainty {
                confidence: "measured".into(),
                timing_relative_spread: Some(0.0),
                eta_relative_range: Some(0.25),
                note: "synthetic".into(),
            },
            error: None,
        };
        assert!(catalog.persist_resource_calibration(&mut report).is_err());
        let (state, report_json): (String, Option<String>) = connection
            .query_row(
                "SELECT state,report_json FROM resource_calibrations WHERE cache_key=?1",
                [&cache_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "running");
        assert!(report_json.is_none());
        let observations: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM resource_calibration_observations WHERE cache_key=?1",
                [&cache_key],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(observations, 0);
        let run_key = active_run_key(catalog.database_path(), &cache_key);
        active_calibration_runs().lock().unwrap().remove(&run_key);
    }

    struct FakeSpeechEngine {
        regions: Vec<SpeechRegion>,
        delay: Duration,
    }
    impl SttEngine for FakeSpeechEngine {
        fn detect_speech(&self, _pcm: &[f32]) -> Result<Vec<SpeechRegion>> {
            std::thread::sleep(self.delay);
            Ok(self.regions.clone())
        }
        fn decode_window(
            &self,
            _pcm: &[f32],
            _window: &crate::analysis::windows::PackedWindow,
        ) -> Result<Vec<crate::analysis::quality::DecodedSegment>> {
            Ok(Vec::new())
        }
    }

    fn speech_identity(options: &SpeechCalibrationOptions) -> CalibrationIdentity {
        let mut candidates = Vec::new();
        for vad_threads in &options.vad_threads {
            candidates.push(CalibrationCandidateSettings {
                label: format!("vad={vad_threads};decode={DEFAULT_DECODE_THREADS};workers=1"),
                encoder: None,
                encoder_policy: None,
                worker_count: 1,
                vad_threads: *vad_threads,
                decode_threads: DEFAULT_DECODE_THREADS,
            });
        }
        candidates.sort();
        CalibrationIdentity {
            schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
            recipe_key: STT_RECIPE_KEY.into(),
            recipe_version: RESOURCE_CALIBRATION_RECIPE_VERSION.into(),
            representation_kind: SPEECH_REPRESENTATION_KIND.into(),
            host: host_identity(),
            tools: tool_identities(),
            devices: hardware_device_identities(),
            models: vec![],
            media: CalibrationMediaIdentity {
                kind: "generated".into(),
                sample_identity: generated_sample_identity(1, "speech"),
                duration_ms: 1000,
                width: 0,
                height: 0,
                has_audio: true,
            },
            conditions: CalibrationConditions {
                trials: options.trials,
                local_heavy_slots: 1,
                generated_duration_seconds: Some(1),
                note: "test".into(),
            },
            candidates: candidates.clone(),
            configuration_key: calibration_configuration_key("generated", &candidates, None)
                .unwrap(),
        }
    }

    fn run_fake_speech(
        catalog: &Catalog,
        options: &SpeechCalibrationOptions,
        regions_for: impl Fn(usize) -> Result<Vec<SpeechRegion>> + Copy,
        delay_for: impl Fn(usize) -> Duration + Copy,
    ) -> ResourceCalibrationReport {
        let identity = speech_identity(options);
        let cache_key = identity.cache_key().unwrap();
        catalog
            .begin_resource_calibration(&identity, &cache_key)
            .unwrap();
        catalog
            .run_speech_calibration_with_factory(
                options,
                &identity,
                &cache_key,
                &timestamp(),
                &vec![0.0; 16000],
                1000,
                |vad_threads, _decode_threads| {
                    Ok(Box::new(FakeSpeechEngine {
                        regions: regions_for(vad_threads)?,
                        delay: delay_for(vad_threads),
                    }) as Box<dyn SttEngine>)
                },
            )
            .unwrap()
    }

    #[test]
    fn fake_speech_engine_selects_fastest_valid_equivalent_candidate() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let options = SpeechCalibrationOptions {
            vad_threads: vec![1, 2],
            trials: 1,
            ..SpeechCalibrationOptions::default()
        };
        let report = run_fake_speech(
            &catalog,
            &options,
            |_| Ok(vec![region(100, 400), region(600, 900)]),
            |vad| {
                if vad == 1 {
                    Duration::from_millis(20)
                } else {
                    Duration::from_millis(2)
                }
            },
        );
        assert_eq!(report.state, CalibrationState::Complete);
        assert_eq!(report.selected_settings.as_ref().unwrap().vad_threads, 2);
        assert_eq!(
            report.quality.unwrap().adopted_candidate.as_deref(),
            Some("vad=2;decode=4;workers=1")
        );
    }

    #[test]
    fn fake_speech_engine_rejects_faster_but_different_regions() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let options = SpeechCalibrationOptions {
            vad_threads: vec![1, 2],
            trials: 1,
            ..SpeechCalibrationOptions::default()
        };
        let report = run_fake_speech(
            &catalog,
            &options,
            |vad| {
                if vad == 1 {
                    Ok(vec![region(100, 400)])
                } else {
                    Ok(vec![region(100, 401)])
                }
            },
            |vad| {
                if vad == 1 {
                    Duration::from_millis(20)
                } else {
                    Duration::from_millis(1)
                }
            },
        );
        assert_eq!(report.state, CalibrationState::Complete);
        assert_eq!(report.selected_settings.as_ref().unwrap().vad_threads, 1);
        assert!(report.quality.unwrap().adopted_candidate.is_none());
        let rejected = report
            .candidates
            .iter()
            .find(|candidate| candidate.settings.vad_threads == 2)
            .unwrap();
        assert!(!rejected.succeeded);
    }

    #[test]
    fn fake_speech_engine_failures_produce_failed_report_without_settings() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let options = SpeechCalibrationOptions {
            vad_threads: vec![1, 2],
            trials: 1,
            ..SpeechCalibrationOptions::default()
        };
        let report = run_fake_speech(
            &catalog,
            &options,
            |_| Err(anyhow!("engine exploded")),
            |_| Duration::ZERO,
        );
        assert_eq!(report.state, CalibrationState::Failed);
        assert!(report.selected_settings.is_none());
        assert!(
            report
                .candidates
                .iter()
                .all(|candidate| !candidate.succeeded)
        );
    }

    #[test]
    fn exact_complete_calibration_is_reused_without_rerunning_benchmark() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let options = ProxyCalibrationOptions {
            media: CalibrationMedia::Generated,
            policy: ProxyEncoderPolicy::Software,
            worker_counts: vec![1],
            trials: 1,
            generated_duration_seconds: 1,
            force: false,
        };
        let candidates = proxy_candidates(options.policy, &options.worker_counts);
        let identity = CalibrationIdentity {
            schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
            recipe_key: PROXY_SOFTWARE_RECIPE_KEY.into(),
            recipe_version: RESOURCE_CALIBRATION_RECIPE_VERSION.into(),
            representation_kind: PROXY_REPRESENTATION_KIND.into(),
            host: host_identity(),
            tools: tool_identities(),
            devices: hardware_device_identities(),
            models: vec![],
            media: CalibrationMediaIdentity {
                kind: "generated".into(),
                sample_identity: generated_sample_identity(1, "proxy"),
                duration_ms: 1000,
                width: 320,
                height: 180,
                has_audio: true,
            },
            conditions: CalibrationConditions {
                trials: 1,
                local_heavy_slots: 1,
                generated_duration_seconds: Some(1),
                note: "single shared local-heavy slot; concurrent worker candidates are measured inside that one slot".into(),
            },
            candidates,
            configuration_key: calibration_configuration_key(
                options.media.kind(),
                &proxy_candidates(options.policy, &options.worker_counts),
                Some(options.policy),
            )
            .unwrap(),
        };
        let mut cached = fake_proxy_report(&catalog, identity);
        cached.selection_reason = "returned from exact cache".into();
        let cache_key = cached.cache_key.clone();
        catalog.persist_resource_calibration(&mut cached).unwrap();
        let report = catalog.calibrate_proxy(&options).unwrap();
        assert_eq!(report.cache_key, cache_key);
        assert_eq!(report.selection_reason, "returned from exact cache");
    }

    fn synthetic_estimator_environment(tool_version: &str) -> MediaEstimatorEnvironment {
        MediaEstimatorEnvironment {
            schema_version: "media-estimator-environment.v1".into(),
            host: CalibrationHostIdentity {
                host_key: "host".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
                logical_cpus: 4,
                application_build: "build-a".into(),
            },
            tools: vec![CalibrationToolIdentity {
                role: "media_transcode".into(),
                name: "ffmpeg".into(),
                version: tool_version.into(),
                executable_key: "exact-program-a".into(),
            }],
            devices: vec![CalibrationDeviceIdentity {
                kind: "cpu".into(),
                name: "synthetic".into(),
                identity: "cpu-a".into(),
            }],
        }
    }

    #[test]
    fn media_estimator_keys_separate_semantic_identity_from_execution_and_environment() {
        let identity = MediaEstimatorIdentity::new(
            synthetic_estimator_environment("ffmpeg-a"),
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            "proxy.h264-720p.orientation-v3.linux-software-w1-v1",
            Some("software"),
            1,
            0,
            0,
        );
        let base = "proxy.h264-720p.orientation-v3.linux-software-w1-v1";
        let qualified = identity.qualified_history_key(base);
        assert!(qualified.starts_with(base));
        assert!(qualified.contains(".h"));
        assert!(qualified.contains(".c"));
        assert_eq!(
            identity.semantic_recipe_key, PROXY_SOFTWARE_RECIPE_KEY,
            "estimator qualification must not rewrite the semantic recipe"
        );
        assert!(identity.is_compatible_with(
            &synthetic_estimator_environment("ffmpeg-a"),
            PROXY_SOFTWARE_RECIPE_KEY
        ));

        let changed_tool = MediaEstimatorIdentity::new(
            synthetic_estimator_environment("ffmpeg-b"),
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            "proxy.h264-720p.orientation-v3.linux-software-w1-v1",
            Some("software"),
            1,
            0,
            0,
        );
        let changed_encoder = MediaEstimatorIdentity::new(
            synthetic_estimator_environment("ffmpeg-a"),
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            "proxy.h264-720p.orientation-v3.linux-vaapi-qp23-w1-v1",
            Some("vaapi"),
            1,
            0,
            0,
        );
        assert_ne!(qualified, changed_tool.qualified_history_key(base));
        assert_ne!(qualified, changed_encoder.qualified_history_key(base));
        assert!(!changed_tool.is_compatible_with(
            &synthetic_estimator_environment("ffmpeg-a"),
            PROXY_SOFTWARE_RECIPE_KEY
        ));
        assert!(!identity.is_compatible_with(
            &synthetic_estimator_environment("ffmpeg-a"),
            "thumbnail.q1.orientation-v2"
        ));
    }

    #[test]
    fn configured_tool_identity_uses_the_exact_program_not_the_global_name() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("ffmpeg-a");
        let second = directory.path().join("ffmpeg-b");
        fs::write(&first, "#!/bin/sh\necho 'ffmpeg version synthetic-a'\n").unwrap();
        fs::write(&second, "#!/bin/sh\necho 'ffmpeg version synthetic-b'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [&first, &second] {
                fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let first_identity =
            calibration_tool_identity_for_path("media_transcode", "ffmpeg", &first);
        let second_identity =
            calibration_tool_identity_for_path("media_transcode", "ffmpeg", &second);
        assert!(first_identity.version.contains("synthetic-a"));
        assert!(second_identity.version.contains("synthetic-b"));
        assert_ne!(
            first_identity.executable_key,
            second_identity.executable_key
        );
    }

    #[test]
    fn representative_media_is_rejected_before_hashing_when_over_duration_bound() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("too-long.mp4");
        fs::write(&path, b"not really a video").unwrap();
        // The byte bound is independent of media parsing and is checked first.
        let oversized = directory.path().join("oversized.mp4");
        let file = fs::File::create(&oversized).unwrap();
        file.set_len(MAX_REPRESENTATIVE_MEDIA_BYTES + 1).unwrap();
        let error = validate_representative_media_metadata(&oversized).unwrap_err();
        assert!(error.to_string().contains("bounded"));
        assert!(
            validate_representative_duration(MAX_REPRESENTATIVE_MEDIA_DURATION_MS + 1).is_err()
        );
        assert!(validate_representative_duration(MAX_REPRESENTATIVE_MEDIA_DURATION_MS).is_ok());
    }
}
