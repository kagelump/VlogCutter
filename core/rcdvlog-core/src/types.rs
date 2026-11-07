//! Public data types shared across the catalog API: projects, assets, reports and progress payloads.

use crate::*;
use batch_estimator::BatchEstimate;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub asset_count: u64,
    pub labeled_count: u64,
    pub removed_count: u64,
    pub origin_node_id: Option<String>,
    pub paired_server_node_id: Option<String>,
    pub paired_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyRepresentation {
    RawV3,
    MetadataTaggedRawV2,
    MetadataBakedV1,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaVerificationState {
    #[default]
    Unlinked,
    Provisional,
    Verified,
    Mismatch,
}

impl MediaVerificationState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unlinked => "unlinked",
            Self::Provisional => "provisional",
            Self::Verified => "verified",
            Self::Mismatch => "mismatch",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    pub id: String,
    pub project_id: String,
    pub path: String,
    pub filename: String,
    pub fingerprint: String,
    pub size_bytes: u64,
    pub duration_seconds: f64,
    pub frame_rate: f64,
    pub width: u32,
    pub height: u32,
    pub codec: String,
    pub camera: String,
    pub captured_at_raw: Option<String>,
    pub best_time: Option<String>,
    pub offset_seconds: i64,
    pub proxy_path: Option<String>,
    pub proxy_representation: Option<ProxyRepresentation>,
    #[serde(default)]
    pub camera_proxy_available: bool,
    /// Rotation applied to the encoded frame by camera metadata, in clockwise degrees.
    pub metadata_rotation_degrees: u16,
    /// Landscape-first orientation selected when the clip was ingested.
    pub default_rotation_degrees: u16,
    /// The reviewer-selected rotation applied to review media, in clockwise degrees.
    pub rotation_degrees: u16,
    /// True when camera rotation metadata has been explicitly probed for this asset.
    pub orientation_probed: bool,
    /// Explicit visual judgment, independent of inferred defaults and numeric overrides.
    #[serde(default)]
    pub orientation_decision: Option<OrientationDecision>,
    pub original_available: bool,
    pub proxy_available: bool,
    pub thumbnail_path: Option<String>,
    pub thumbnail_quality: u8,
    pub notes: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub labels: Vec<String>,
    pub removed: bool,
    pub media_object_id: Option<String>,
    pub media_verification_state: MediaVerificationState,
    pub available: bool,
    /// The operation revision this view was materialized from.
    ///
    /// A writer sends it back as `AssetPatch::expected_revision` so a patch
    /// built from a stale read is refused rather than applied over whatever
    /// happened in between.
    #[serde(default)]
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetPatch {
    pub notes: String,
    pub labels: Vec<String>,
    pub removed: bool,
    pub rotation_degrees: u16,
    /// The `Asset::revision` this patch was built from, when the caller has one.
    ///
    /// `None` keeps the pre-existing last-write-wins behaviour, which the
    /// scripted CLI paths and the ingest-time writers rely on. A caller that
    /// read the asset and let a human edit it should always send one.
    #[serde(default)]
    pub expected_revision: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestReport {
    pub scanned: usize,
    pub imported: usize,
    pub duplicates: usize,
    /// Clips already in this project whose original was offline and has now been
    /// found again at the scanned location.
    #[serde(default)]
    pub relinked: usize,
    pub failures: Vec<IngestFailure>,
}

/// The result of pointing the app at a folder and asking it to find originals
/// the project has lost track of.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelinkReport {
    pub scanned: usize,
    /// Assets whose original was offline and now resolves again.
    pub repaired: usize,
    /// Clips found here that the project could already reach elsewhere.
    pub already_online: usize,
    /// Media in this folder that belongs to no asset in this project. Counted,
    /// never imported.
    pub unrecognized: usize,
    /// Assets whose original is still not reachable anywhere after the search.
    pub still_missing: usize,
    pub failures: Vec<IngestFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestFailure {
    pub path: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyReport {
    pub generated: usize,
    pub skipped: usize,
    /// True when the caller stopped the batch early. Defaulted so reports
    /// serialized before cancellation existed still deserialize.
    #[serde(default)]
    pub cancelled: bool,
    /// Actual execution provenance. These fields are defaulted for older
    /// serialized reports and are never inferred from an unused calibration.
    #[serde(default)]
    pub resource_source: String,
    #[serde(default)]
    pub calibration_key: Option<String>,
    #[serde(default)]
    pub encoder: Option<String>,
    #[serde(default)]
    pub worker_count: usize,
    #[serde(default)]
    pub history_key: String,
    #[serde(default)]
    pub sample_compatibility: String,
    pub failures: Vec<IngestFailure>,
}

/// Durable evidence for one proxy generation transaction. Kept separately
/// from `Asset` so failed attempts remain inspectable without changing the
/// registered proxy path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyGenerationProvenance {
    pub id: i64,
    pub asset_id: String,
    pub recipe_key: String,
    pub policy: ProxyEncoderPolicy,
    pub selected_encoder: String,
    pub device: Option<String>,
    pub fallback_used: bool,
    pub status: String,
    pub elapsed_ms: u64,
    pub attempts: Vec<ProxyEncoderAttempt>,
    pub validation: Option<ProxyOutputValidation>,
    pub created_at: String,
}

/// Encoder-aware throughput observation. The scope note is deliberately not a
/// benchmark claim: a rate is useful only for this host/catalog and corpus.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyRateHistoryEntry {
    pub task_kind: String,
    pub encoder: String,
    pub worker_count: usize,
    pub work_units_per_worker_second: f64,
    pub samples: u64,
    pub updated_at: String,
    pub scope: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailReport {
    pub quality: u8,
    pub generated: usize,
    pub skipped: usize,
    pub failures: Vec<IngestFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetBatchProgress {
    pub asset: Option<Asset>,
    pub estimate: BatchEstimate,
}

/// The analysis facts a queue row needs, for one clip.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetAnalysisIndexEntry {
    pub asset_id: String,
    /// Absent when the clip has never been transcribed. `no_speech` is a value,
    /// not an absence.
    pub transcript_status: Option<String>,
    /// Every segment's text joined, for substring search.
    pub transcript_text: String,
    pub analyzed: bool,
    pub usable: bool,
    pub unusable_reason: String,
}

impl AssetAnalysisIndexEntry {
    pub(crate) fn empty(asset_id: &str) -> Self {
        Self {
            asset_id: asset_id.to_string(),
            transcript_status: None,
            transcript_text: String::new(),
            analyzed: false,
            // Defaults to usable so a clip with no observation is never hidden
            // by the unusable filter.
            usable: true,
            unusable_reason: String::new(),
        }
    }
}

/// One clip's observation as the review surface needs it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetAnalysisView {
    pub analysis: ClipAnalysis,
    /// Labels this analysis authored, as recorded in the ownership side table.
    /// Human-added labels are deliberately absent: they live on the asset.
    pub ai_labels: Vec<String>,
    /// False when the proxy has been regenerated since this observation, so the
    /// UI can mark it stale rather than presenting it as a description of the
    /// clip currently on screen.
    pub current: bool,
}

/// Cost per clip used before a project has analyzed anything of its own.
///
/// From the 2026-08-04 acceptance run: $0.02732851 across 252 clips on the
/// 273-clip Osaka/Expo project. It is a starting point that the first real run
/// replaces with this project's own observed average.
pub const REFERENCE_COST_PER_CLIP_USD: f64 = 0.02732851 / 252.0;

/// What a finished analysis run did, for the run record.
#[derive(Debug, Clone, Default)]
pub struct AnalysisRunOutcome {
    /// One of `completed`, `cancelled`, or `failed`.
    pub status: String,
    pub stage_reached: String,
    pub proxies_generated: usize,
    pub clips_transcribed: usize,
    pub clips_analyzed: usize,
    pub cost_usd: f64,
    pub budget_exhausted: bool,
    pub error: String,
}

/// Everything a caller needs to decide whether to start an analysis run, and
/// what it will cost. Computed without touching the network.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisReadiness {
    pub project_id: String,
    /// Validated usable clips currently eligible for analysis. Removed,
    /// unverified, quarantined, and unavailable clips are excluded.
    pub total_clips: usize,
    /// Nonremoved catalog rows whose sources can currently be statted.
    pub reachable_originals: usize,
    /// Nonremoved legacy rows awaiting explicit media validation.
    pub unverified_video_clips: usize,
    /// Nonremoved rows durably quarantined after failed video validation.
    pub quarantined_video_clips: usize,
    /// Previously usable media whose original and proxy are both unavailable.
    pub unavailable_video_clips: usize,
    pub missing_proxies: usize,
    pub missing_transcripts: usize,
    pub missing_analyses: usize,
    pub models: Vec<ModelStatus>,
    pub models_ready: bool,
    /// Whether a key can be resolved at all. The key itself never leaves the
    /// Keychain or environment, and is never returned here.
    pub api_key_present: bool,
    /// Projected spend for `missing_analyses`, in USD.
    pub estimated_cost_usd: f64,
    /// Where the projection came from, for display beside the number.
    pub cost_basis: AnalysisCostBasis,
    pub observed_cost_per_clip_usd: f64,
    pub last_run: Option<AnalysisRunRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetVideoValidationStatus {
    Unverified,
    Usable,
    Quarantined,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetVideoValidation {
    pub asset_id: String,
    pub status: AssetVideoValidationStatus,
    pub bytes_reachable: bool,
    pub diagnostic: Option<String>,
    pub validated_at: Option<String>,
}

/// Whether a cost projection is grounded in this project or in the reference
/// measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnalysisCostBasis {
    /// Averaged from clips already analyzed in this project.
    Observed,
    /// No clip has been analyzed here yet, so the published measurement is used.
    Reference,
}

/// One analysis run, for spend history and resumption context.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisRunRecord {
    pub id: String,
    pub project_id: String,
    pub started_at: String,
    pub finished_at: String,
    pub status: String,
    pub stage_reached: String,
    /// Exact run policy that admitted this work, or empty for legacy/local runs.
    pub policy_id: String,
    pub proxies_generated: usize,
    pub clips_transcribed: usize,
    pub clips_analyzed: usize,
    pub cost_usd: f64,
    pub budget_exhausted: bool,
    pub error: String,
    /// Canonical accounting-contract marker. Empty means a legacy or not-yet
    /// fully ledgered runtime row. The runtime integration must set the accounting
    /// version before any paid attempt.
    #[serde(default)]
    pub accounting_contract: String,
}

/// Whether one speech model is present, intact, and ready to load.
///
/// `installed` is retained as the readiness-compatible field used by existing
/// desktop consumers. `present`, `integrity`, and `ready` separate file
/// existence from verified readiness so a corrupt file is not presented as an
/// installed model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    /// Stable artifact filename used as the model identity.
    pub name: String,
    /// `speech_to_text` or `voice_activity_detection`.
    pub role: String,
    pub installed: bool,
    pub ready: bool,
    pub present: bool,
    /// `verified`, `present_unverified`, `invalid`, or `missing`.
    pub integrity: String,
    pub detail: Option<String>,
    /// Published artifact size in bytes.
    pub bytes: u64,
    pub url: String,
    /// The directory this catalog reads the model from.
    pub directory: String,
    /// The exact path expected for this model in `directory`.
    pub path: String,
    pub expected_blake3: Option<String>,
}

/// Progress for an explicit model download.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInstallProgress {
    pub name: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}

/// Result of an explicit model installation attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInstallReport {
    /// True when the progress callback stopped the transfer. The partial file is
    /// removed and is not counted as installed. Earlier completed downloads stay
    /// installed and remain included in `installed`.
    pub cancelled: bool,
    /// Number of model files downloaded by this attempt, excluding already-ready
    /// models.
    pub installed: usize,
    pub models: Vec<ModelStatus>,
}

/// Progress for a transcription batch, mirroring `AssetBatchProgress`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscribeBatchProgress {
    pub outcome: Option<TranscribeOutcome>,
    pub estimate: BatchEstimate,
}

/// Summary of one `transcribe missing` run.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscribeReport {
    /// Terminal transcript outcomes persisted by this run, including
    /// `no_audio`, `no_speech`, `no_accepted_speech`, and `rejected`.
    pub transcribed: usize,
    /// Subset of `transcribed` that completed without an audio stream.
    pub no_audio: usize,
    /// Processing failures. These remain retryable and are not in `transcribed`.
    pub failed: usize,
    pub skipped: usize,
    /// True when the caller stopped the batch early. Distinct from a failure:
    /// the clips already transcribed are committed and a later run resumes.
    pub cancelled: bool,
    /// Actual selected resource identity for this run.
    pub resource_source: String,
    pub calibration_key: Option<String>,
    pub vad_threads: usize,
    pub decode_threads: usize,
    pub worker_count: usize,
    pub history_key: String,
    /// Calibration sample compatibility for any ETA/rate provenance.
    pub sample_compatibility: String,
    pub outcomes: Vec<TranscribeOutcome>,
    pub failures: Vec<IngestFailure>,
}

/// Transcript coverage for a project, counted by terminal status.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptCoverage {
    pub recipe_key: String,
    pub total: usize,
    /// Clips with usable speech.
    pub speech: usize,
    /// Clips whose VAD gate ran and found no detected speech.
    pub no_speech: usize,
    /// Subset of `no_speech` migrated from older catalogs where the schema
    /// could not prove whether the clip was silent or audioless.
    pub legacy_no_speech_unverified: usize,
    /// Valid videos with no audio stream; no speech work was attempted.
    pub no_audio: usize,
    /// Clips where speech was detected but the provider returned no accepted
    /// text. Distinct from literal silence.
    pub no_accepted_speech: usize,
    /// Clips whose decode tripped the hallucination gate.
    pub rejected: usize,
    /// Clips with retained candidate text whose quality evidence is
    /// incomplete. These candidates are unaccepted and are never counted as
    /// `speech`.
    pub review_required: usize,
    pub failed: usize,
    /// Clips never transcribed, or whose proxy has since been regenerated.
    pub missing: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisCoverage {
    pub recipe_key: String,
    pub prompt_version: String,
    pub total: usize,
    pub ok: usize,
    pub rejected: usize,
    pub failed: usize,
    pub missing: usize,
    pub cost_usd: f64,
    pub fallback_served: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeReport {
    pub analyzed: usize,
    pub skipped: usize,
    pub rejected: usize,
    pub fallback_served: usize,
    pub force_overwrite_warnings: usize,
    pub cost_usd: f64,
    /// False when any attempted provider call returned no usage data.
    pub usage_complete: bool,
    /// True when the caller stopped the batch early.
    pub cancelled: bool,
    /// True when the batch stopped because the run's cost ceiling was reached.
    /// Kept separate from `cancelled` so the two stop reasons can be reported
    /// differently; a budget stop is a spend guard, not a user action.
    pub budget_exhausted: bool,
    pub outcomes: Vec<AnalysisOutcome>,
    pub failures: Vec<IngestFailure>,
}

impl AnalyzeReport {
    pub(crate) fn cancelled_before_work() -> Self {
        Self {
            analyzed: 0,
            skipped: 0,
            rejected: 0,
            fallback_served: 0,
            force_overwrite_warnings: 0,
            cost_usd: 0.0,
            usage_complete: true,
            cancelled: true,
            budget_exhausted: false,
            outcomes: Vec::new(),
            failures: Vec::new(),
        }
    }
}

/// Explicit inputs for one cloud analysis run.
///
/// The desktop supplies these from its Keychain entry and confirm dialog, while
/// the CLI leaves them unset and keeps resolving from the environment. Both
/// adapters therefore reach the same batch through the same method.
#[derive(Debug, Clone, Default)]
pub struct AnalysisRunOptions {
    /// Re-analyze clips whose observation is already current.
    pub force: bool,
    /// Overrides `OPENROUTER_API_KEY` / the key file when set.
    pub api_key: Option<String>,
    /// Overrides `RCDVLOG_VLM_MAX_COST_USD` when set. Checked against returned
    /// provider usage before each request.
    pub budget_usd: Option<f64>,
    /// Overrides `RCDVLOG_VLM_WORKERS` when set.
    pub worker_count: Option<usize>,
    /// Model explicitly selected for this run. Defaults to the measured primary.
    pub primary_model: Option<String>,
    /// Complete set of models the run may call. Empty means the defaults.
    pub allowed_models: Vec<String>,
    /// Whether one retry using the configured alternate model is permitted.
    pub allow_fallback: Option<bool>,
    /// Optional exact asset scope. The policy-bound bulk path sets this from
    /// the authorized policy; `None` retains the legacy "missing" workflow.
    pub selected_assets: Option<Vec<String>>,
    /// Optional payload binding. Analysis always uses frames and may include
    /// accepted transcript context.
    pub payload: Option<crate::AnalysisPayload>,
    /// Explicit approval to attach the exact current project label vocabulary.
    /// Its digest/count is bound into the policy; text is never logged.
    pub include_project_vocabulary: bool,
    /// When set, the stored policy is authoritative for scope, model, payload
    /// and remaining budget, and a matching passed pilot is mandatory.
    pub policy_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeBatchProgress {
    /// The clip this event is about. `AnalysisOutcome` describes the model's
    /// response and does not identify the clip, so a UI updating a row needs
    /// this separately.
    pub asset_id: Option<String>,
    pub outcome: Option<AnalysisOutcome>,
    pub estimate: BatchEstimate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogInspection {
    pub path: String,
    pub read_only: bool,
    pub journal_mode: String,
    pub project_count: u64,
    pub asset_count: u64,
    pub missing_required_tables: Vec<String>,
    pub pending_project_columns: Vec<String>,
    pub pending_asset_columns: Vec<String>,
    pub schema_history_present: bool,
    pub current_schema_version: u32,
    pub latest_supported_schema_version: u32,
    pub schema_status: MigrationHistoryStatus,
    pub applied_migrations: Vec<SchemaMigrationRecord>,
    pub pending_migrations: Vec<PendingMigration>,
    pub unresolved_conflict_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaStreamInfo {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub codec: String,
    pub metadata_rotation_degrees: u16,
    pub display_width: u32,
    pub display_height: u32,
    pub display_orientation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrientationEvidence {
    pub asset_id: String,
    pub asset_revision: u64,
    pub source_fingerprint: String,
    pub representation: String,
    pub representation_fingerprint: Option<String>,
    pub original_available: bool,
    pub raw_frame_path: Option<String>,
    pub effective_frame_path: Option<String>,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrientationReviewItem {
    pub asset: Asset,
    pub review_reasons: Vec<String>,
    pub evidence_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrientationReviewPage {
    pub items: Vec<OrientationReviewItem>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockEvent {
    pub id: String,
    pub project_id: String,
    pub camera: String,
    pub start_filename: String,
    pub offset_seconds: i64,
    pub label: String,
    pub created_at: String,
}
