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

mod candidates;
mod estimate;
mod identity;
mod proxy_run;
mod run_owner;
mod samples;
mod selection;
mod speech_run;
mod store;
mod validation;

pub use candidates::*;
use estimate::*;
pub use identity::*;
use run_owner::*;
use samples::*;
pub(crate) use validation::*;

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
#[path = "calibration_tests.rs"]
mod tests;
