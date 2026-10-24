use anyhow::{Context, Result, anyhow, bail, ensure};
use batch_estimator::{BatchEstimate, BatchEstimator, RateHistory};
use blake3::Hasher;
use chrono::{DateTime, Duration, NaiveDateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Instant;
use uuid::Uuid;
use walkdir::WalkDir;

mod agent;
mod analysis;
mod archive;
mod baseline;
mod calibration;
mod camera_proxy;
mod cameras;
mod cards;
mod chronology;
mod copy;
mod diagnostics;
mod editplan;
mod identifiers;
mod import_cutoff;
mod import_history;
mod import_preview;
mod lifecycle;
mod locations;
mod migrations;
mod operations;
mod preflight;
mod preparation;
mod prerequisites;
mod proxy_encoding;
mod representation;
mod resources;
mod stall;
mod storage;
mod transcription;
mod volumes;

use analysis::audio::{AudioExtractor, FfmpegAudioExtractor};
use analysis::chronology_suggestions::request_suggestion;
use analysis::consolidate::{
    apply_mapping as persist_label_mapping, assets_for_label, consolidation_prompt, mapping_schema,
    parse_mapping, proposals as label_proposals,
};
use analysis::engine::{SttEngine, VAD_MODEL, WHISPER_MODEL, ensure_model, model_dir};
use analysis::store::{StoredTranscript, is_current, load_transcript, save_transcript};
use analysis::stt::transcribe_pcm;
use analysis::stt::{
    failed_transcript, failed_transcript_with_evidence, failure_evidence, no_audio_transcript,
};
use analysis::vlm::{
    PromptContext, PromptTranscript, analyze_clip_with_cancel as run_vlm_with_cancel,
};
use analysis::vlm_store::{
    AnalysisStatus, StoredAnalysis, ai_labels_for_asset, analysis_is_current, clear_ai_labels,
    commit_thumbnail, load_analysis, load_frame_pack, record_ai_labels, save_analysis,
    save_frame_pack, set_title_and_description,
};
use analysis::whisper_engine::{WhisperEngine, read_pcm};

pub use analysis::accounting::{
    ANALYSIS_ACCOUNTING_SCHEMA_VERSION, AccountedVlmProvider, AnalysisAccountingAggregate,
    AnalysisAccountingContext, AnalysisAccountingReport, AnalysisBudgetAuthorityContext,
    AnalysisBudgetExhausted, AnalysisBudgetSnapshot, AnalysisOperationAttemptProjection,
    AnalysisProviderReceipt, AnalysisRequestEvent, AnalysisRequestLifecycle,
    AnalysisRequestReconciliation, AnalysisRequestRecord, AnalysisRetryAuthorization,
    LEGACY_UNKNOWN_USAGE_STOP_REASON, MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY,
    MAX_REQUEST_ATTEMPTS, ProviderReceiptEvidence, UNTRACKED_POST_INITIALIZATION_USAGE_STOP_REASON,
    analysis_operation_budget_scope_id, analysis_policy_budget_scope_id,
    analysis_request_cost_bound_fingerprint, analysis_request_payload_fingerprint,
    bounded_failure_code,
};
pub use analysis::chronology_suggestions::{
    ChronologyEvidenceRequest, ChronologySuggestion, EvidenceFrame, PairMatch,
    SuggestionProvenance, SuggestionResult, SuggestionReview, review_suggestion,
    review_suggestion_against_facts,
};
pub use analysis::consolidate::{ConsolidationReport, LabelMapping, LabelProposal};
pub use analysis::framepack::{FramePackManifest, build_frame_pack, encode_frames};
pub use analysis::moments;
pub use analysis::openrouter::{
    OpenRouterProvider, api_credential_present, frame_data_uri, key_file_path,
    openrouter_request_cost_bound, resolve_api_key,
};
pub use analysis::provider::{
    ANALYSIS_COST_BOUND_CONTRACT_VERSION, AnalysisRequestCostBound, ChainOutcome, DEFAULT_MODEL,
    FALLBACK_MODEL, MODELS, ModelSpec, OPENROUTER_MAX_OUTPUT_TOKENS, ProviderChain,
    ProviderFailure, ProviderFailureCategory, VlmProvider, VlmReceipt, VlmRequest, VlmResponse,
    analysis_cost_bound_contract_id, model_spec,
};
pub use analysis::run_policy::{
    AnalysisPayload, AnalysisRunKind, AuthorizedPolicyState, CapabilityCheck, CapabilityStatus,
    PilotValidation, RUN_AUTHORIZATION_SCHEMA_VERSION, RUN_PILOT_SCHEMA_VERSION,
    RUN_POLICY_SCHEMA_VERSION, RUN_POLICY_TOOL_VERSION, RunAssetScope, RunAuthorization,
    RunPilotRecord, RunPolicy, RunPolicyBlocker, RunPolicyContext, RunPolicyParameters,
    RunPolicyReadiness, RunPolicyRequest,
};
pub use archive::{ArchiveRoot, EndpointProbe};
pub use batch_estimator::BatchEstimate as BatchProgressEstimate;
pub use calibration::{
    CalibrationCandidateSettings, CalibrationConditions, CalibrationDeviceIdentity, CalibrationEta,
    CalibrationHostIdentity, CalibrationIdentity, CalibrationMedia, CalibrationMediaIdentity,
    CalibrationModelIdentity, CalibrationQualityDecision, CalibrationState,
    CalibrationStatusReport, CalibrationSummary, CalibrationTiming, CalibrationToolIdentity,
    CalibrationUncertainty, DEFAULT_GENERATED_CALIBRATION_SECONDS, MAX_CALIBRATION_CANDIDATES,
    MAX_CALIBRATION_THREADS, MAX_CALIBRATION_TOTAL_TRIALS, MAX_CALIBRATION_TRIALS,
    MAX_CALIBRATION_WORKERS, MAX_GENERATED_CALIBRATION_SECONDS, MAX_REPRESENTATIVE_MEDIA_BYTES,
    MAX_REPRESENTATIVE_MEDIA_DURATION_MS, MediaEstimatorEnvironment, MediaEstimatorIdentity,
    PROXY_REPRESENTATION_KIND, ProxyCalibrationOptions, RESOURCE_CALIBRATION_RECIPE_VERSION,
    RESOURCE_CALIBRATION_SCHEMA_VERSION, ResourceCalibrationReport, ResourceOverrides,
    ResourceSelection, SPEECH_REPRESENTATION_KIND, SpeechCalibrationOptions,
    SpeechRegionComparison, SpeechRegionDifference, calibration_hardware_device_identities,
    calibration_host_identity, calibration_tool_identity_for_path, compare_speech_regions,
    media_estimator_process_key,
};
pub use camera_proxy::{CameraProxySidecar, CameraSidecarProvenance, CameraTimeMapping};
pub use cards::{
    CardCandidate, MountedVolumes, VolumeSource, card_session_slug, detect_cards, inspect_card,
};
pub use import_cutoff::ImportCutoff;

pub use import_history::{
    ImportHistoryEntry, ImportRollbackModifiedClip, ImportRollbackPreview, ImportSessionDetail,
    ImportSessionFile,
};
pub use import_preview::{ImportPreviewItem, ImportPreviewRequest};
pub use lifecycle::{
    CacheCategory, CacheCleanupReport, CacheKind, CacheScope, ProjectRemovalAudit,
    ProjectStorageReport,
};

pub use agent::{
    AgentReadinessError, BindingCheck, CatalogReadiness, CliBinding, CliBuild, CliExecutable,
    CliSelection, DoctorReport, ReadinessRequirements, catalog_readiness, check_cli_binding,
    doctor_report, ensure_agent_readiness, install_speech_models_with_progress,
    refresh_cli_binding, speech_model_status,
};
pub use preflight::{
    Classification, ImportDestination, ImportOutcome, ImportPreflight, ImportProgress,
    ImportSequenceBoundary, ImportSequenceBoundaryEvidence, ImportSequencePreview,
    ImportSequenceRunDecision, ImportSequenceRunPreview, ImportStage,
    LARGE_CAPTURE_SEQUENCE_GAP_SECONDS, PreflightItem, human_bytes, import_source_present,
};
pub use preparation::*;
pub use prerequisites::{
    MediaPrerequisiteError, ToolPrerequisiteStatus, ToolStatus, ensure_media_prerequisites,
    prerequisite_status,
};
#[cfg(target_os = "macos")]
pub use proxy_encoding::PROXY_VIDEOTOOLBOX_RECIPE_KEY;
pub use proxy_encoding::{
    EncoderCapability, PROXY_SOFTWARE_RECIPE_KEY, PROXY_VAAPI_RECIPE_KEY, ProxyAttemptStatus,
    ProxyEncoderAttempt, ProxyEncoderKind, ProxyEncoderPlan, ProxyEncoderPolicy,
    ProxyEncoderProbeReport, ProxyEncoderTarget, ProxyGenerationOutcome, ProxyOutputValidation,
    ProxyTimeCorrespondence, ProxyTranscodeFailure, probe_proxy_encoder_capabilities,
    probe_proxy_encoder_capabilities_with_tools, probe_source_audio_present,
    proxy_rate_history_key, resolve_proxy_encoder_plan, resolve_proxy_encoder_plan_with_tools,
    run_proxy_transcode_with_plan, run_proxy_transcode_with_plan_and_cancellation,
    validate_proxy_output,
};
pub use representation::{ProxyWrite, RepresentationBusy, RepresentationClaim, RepresentationKind};
pub use resources::{
    LocalHeavyLease, ResourceWaitCancelled, acquire_local_heavy_at,
    acquire_local_heavy_at_cancellable, default_local_heavy_lock_path, try_acquire_local_heavy_at,
};
pub use transcription::{
    TranscriptionAssetOutcome, TranscriptionAssetState, TranscriptionBatchOptions,
    TranscriptionBatchProgress, TranscriptionBatchReport, TranscriptionReadinessError,
    TranscriptionSelectionError,
};

pub use analysis::moments::{
    EditProfile, MomentArtifact, MomentCandidate, MomentInventoryPage, MomentPage, MomentPayload,
    MomentPlan, MomentProgress, MomentRunOptions, MomentRunReport, MomentStatus,
};
pub use analysis::quality::DecodedSegment;
pub use analysis::schema::{
    CameraMovement, ClipObservation, DomainError, UnusableReason, output_schema,
    validate as validate_observation,
};
pub use analysis::store::{
    StoredTranscript as ClipTranscript, TranscriptOutcomeReason, TranscriptStatus,
};
pub use analysis::stt::TranscribeOutcome;
pub use analysis::vlm::AnalysisOutcome;
pub use analysis::vlm_store::StoredAnalysis as ClipAnalysis;
pub use analysis::{FRAMEPACK_RECIPE_KEY, STT_RECIPE_KEY, STT_TASK_KEY, VLM_PROMPT_VERSION};
pub use cameras::{
    CAMERA_PATTERNS, CameraEvidence, CameraIdentity, CameraIdentitySource, CameraMapping,
    CameraMergePreview, CameraMergeReport, CameraPattern, CameraRepairAssignment,
    CameraRepairPreview, PatternConfidence, UNKNOWN_CAMERA, identify_camera, pattern_counter,
};
pub use chronology::{
    AnchorKind, AppliedCorrection, AssetChronology, AssetTimeHypothesis, CaptureSequence,
    CaptureSequenceMergePreview, CaptureSequenceReassignmentPreview, CaptureSequenceSplitPreview,
    ChronologyError, ChronologyEvidenceAsset, ChronologyEvidenceError, ChronologyEvidencePage,
    ChronologyExplanation, ChronologyFacts, ChronologyImpact, ChronologyImpactChange,
    ChronologyMutation, ChronologyMutationPreview, ChronologySnapshot, ChronologySolution,
    ClipTimeException, ClockAlternative, ClockBlockQuestion, ClockEpoch, ClockEpochClearPreview,
    ClockEpochImpactPreview, ClockQuestionDecision, ClockQuestionDecisionKind,
    ClockQuestionDecisionPreview, ClockQuestionOutcome, ClockQuestionProposalPreview,
    ConstraintStrength, Contradiction, ContradictionKind, DisplayMode, DisplayTime,
    EventOrderConstraint, Evidence, HumanAnchor, OrderKey, PhaseKind, PhaseSplitDirection,
    ProxyExcerpt, QuarantinedOperation, QueuePosition, SplitProposal, TimestampCandidate,
    TripPhase, VisualPlacementPreview, VisualPlacementSide, covering_phase, display_time,
    is_valid_timezone, resolve_anchored_phases, solve,
};
pub use diagnostics::{
    Diagnostic, DiagnosticCounts, DiagnosticFilter, DiagnosticRecord, DiagnosticSeverity,
    DiagnosticSubject,
};
pub use editplan::{
    EditPlanError, EditPlanExport, EditPlanOperation, EditPlanRevision, EditPlanStaleness, Finding,
    FindingKind, OtioExportError, OtioExportRequest, OtioExportResult, PreviewFailure,
    PreviewProgress, PreviewRequest, PreviewResult, Severity, ValidationReport, otio_document,
};
pub use operations::{
    ApplyOperationResult, AssetRevisionDiagnostics, CAPTURE_SEQUENCE_DECLARED,
    CAPTURE_SEQUENCE_MERGED, CAPTURE_SEQUENCE_REASSIGNED, CAPTURE_SEQUENCE_SPLIT,
    CAPTURE_SEQUENCE_SPLIT_EXACT, CLIP_TIME_EXCEPTION_CLEARED, CLIP_TIME_EXCEPTION_SET,
    CLOCK_EPOCH_CLEARED, CLOCK_EPOCH_SET, CLOCK_EVENT_CREATED, ConflictProjection,
    EDIT_PLAN_EXPORT_RECORDED, EDIT_PLAN_REVISION_ACTIVATED, EDIT_PLAN_REVISION_CREATED,
    EVENT_ORDER_CONSTRAINT_CLEARED, EVENT_ORDER_CONSTRAINT_SET, HybridLogicalTime, LABEL_ADDED,
    LABEL_REMOVED, MAX_REMOTE_HLC_FUTURE_SKEW_MILLIS, NOTE_REVISION_CREATED,
    NoteRevisionProjection, OperationBatchResult, OperationDiagnostics, PROJECT_ASSET_ADDED,
    PROJECT_ASSET_MEDIA_LINKED, ProjectAssetAddedPayload, ProjectAssetItem, ProjectAssetPage,
    ProjectOperation, REMOVED_SET, ROTATION_SET, STALE_ASSET_REVISION, SequenceRecoveryProjection,
    SequenceRecoveryState, TRIP_PHASE_REMOVED, TRIP_PHASE_SET,
    operation_affects_chronology_projection,
};

pub use baseline::{PortableAssetBaseline, PortableRootMapping, ProjectBaselineSnapshot};
pub use identifiers::{
    IdentifierError, IdentifierErrorReason, JobId, MediaObjectId, NodeId, OperationId,
    ProjectAssetId, ProjectId, RepresentationId, StorageObjectId, StorageRootId,
};
pub use migrations::{
    CatalogSchemaError, MigrationHistoryInspection, MigrationHistoryStatus,
    PROJECT_CATALOG_MIGRATIONS, PROJECT_CATALOG_SCHEMA_VERSION, PendingMigration,
    SchemaMigrationRecord, SqliteMigration, inspect_project_catalog_migrations,
    inspect_sqlite_migrations, run_sqlite_migrations,
};
pub use storage::{
    BindingVerificationStatus, GlobalMediaVerificationState, LocationVerificationState,
    NodeAvailabilityState, ObservationGenerationState, ObservationStatus, RelativePathError,
    RelativePathErrorReason, RootAssociationConfirmation, RootAssociationConfirmationStatus,
    RootAssociationEvidence, RootRelativePath, StorageAccessMode, StorageProtocol,
};

const SAMPLE_BYTES: usize = 1024 * 1024;
const MEDIA_EXTENSIONS: &[&str] = &["mp4", "mov", "m4v", "mts", "m2ts", "avi"];
const THUMBNAIL_TASK_KIND: &str = "thumbnail.q1.orientation-v2";
const PROXY_CACHE_VERSION: &str = "v3";
const THUMBNAIL_CACHE_VERSION: &str = "v2";
#[cfg(target_os = "macos")]
const PROXY_WORKERS: usize = 4;
#[cfg(not(target_os = "macos"))]
const PROXY_WORKERS: usize = 1;
const THUMBNAIL_WORKERS: usize = 1;

#[cfg(all(test, target_os = "macos"))]
type ProxyEncoder = ProxyEncoderKind;

enum ProxyWorkerMessage {
    Progress {
        index: usize,
        completed_work_units: u64,
    },
    Finished(Box<ProxyWorkerFinished>),
}

#[derive(Debug)]
struct ProxyGenerated {
    asset: Asset,
    outcome: ProxyGenerationOutcome,
}

struct ProxyWorkerFinished {
    index: usize,
    asset: Asset,
    elapsed: std::time::Duration,
    result: Result<ProxyGenerated>,
}

fn proxy_task_kind_for_encoder(encoder: ProxyEncoderKind, worker_count: usize) -> String {
    proxy_rate_history_key(encoder, worker_count)
}

#[cfg(test)]
fn proxy_task_kind(worker_count: usize) -> String {
    let plan = resolve_proxy_encoder_plan(ProxyEncoderPolicy::Auto);
    proxy_task_kind_for_encoder(plan.preferred_encoder, worker_count)
}

#[derive(Debug, Clone)]
pub struct Catalog {
    database_path: PathBuf,
    read_only: bool,
}

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

/// What ingest did with one candidate file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IngestOutcome {
    /// New content; an asset was created.
    Imported,
    /// Already known, and already resolvable somewhere. The discovered location
    /// is recorded, but the asset was left alone.
    Duplicate,
    /// Already known, but its original was offline. The asset now points here.
    Relinked,
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
    fn empty(asset_id: &str) -> Self {
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

fn transcribe_outcome(asset_id: &str, stored: &StoredTranscript) -> TranscribeOutcome {
    TranscribeOutcome {
        asset_id: asset_id.to_string(),
        status: stored.status,
        outcome_reason: stored.outcome_reason,
        source_fingerprint: stored.source_fingerprint.clone(),
        segment_count: stored.segments.len(),
        window_count: stored.window_count,
        speech_seconds: stored.speech_seconds,
        provider_segment_count: stored.provider_segment_count,
        mean_token_probability: stored.mean_token_probability,
        repetition_ratio: stored.repetition_ratio,
    }
}

enum ProjectPlannedAsset {
    NoAudio {
        asset: Asset,
        source_fingerprint: String,
    },
    ProbeFailed {
        asset: Asset,
        source_fingerprint: String,
        reason: String,
    },
    Ready {
        asset: Asset,
        proxy_path: PathBuf,
        source_fingerprint: String,
    },
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
    fn cancelled_before_work() -> Self {
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

/// Resolves and validates the spend ceiling locally. Zero and negative ceilings
/// are rejected; they do not mean "disable spend" or "allow free requests".
fn resolve_analysis_budget(options: &AnalysisRunOptions) -> Result<f64> {
    let budget = if let Some(value) = options.budget_usd {
        value
    } else {
        match std::env::var("RCDVLOG_VLM_MAX_COST_USD") {
            Ok(value) => value
                .parse::<f64>()
                .with_context(|| "RCDVLOG_VLM_MAX_COST_USD must be a finite positive number")?,
            Err(std::env::VarError::NotPresent) => return Ok(f64::INFINITY),
            Err(std::env::VarError::NotUnicode(_)) => {
                anyhow::bail!("RCDVLOG_VLM_MAX_COST_USD is not valid Unicode")
            }
        }
    };
    anyhow::ensure!(budget.is_finite(), "analysis budget must be finite");
    anyhow::ensure!(budget > 0.0, "analysis budget must be greater than zero");
    Ok(budget)
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

/// Inspects catalog shape and counts without running migrations or opening it writable.
pub fn inspect_catalog(path: impl AsRef<Path>) -> Result<CatalogInspection> {
    let path = path.as_ref();
    if !path.is_file() {
        bail!("catalog file not found: {}", path.display());
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("inspect catalog {}", path.display()))?;
    connection.pragma_update(None, "query_only", "ON")?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    let tables = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let missing_required_tables = ["projects", "assets"]
        .into_iter()
        .filter(|name| !tables.iter().any(|table| table == name))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let required_project_columns = ["origin_node_id", "paired_server_node_id", "paired_at"];
    let project_columns = if tables.iter().any(|table| table == "projects") {
        connection
            .prepare("PRAGMA table_info(projects)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        Vec::new()
    };
    let pending_project_columns = required_project_columns
        .into_iter()
        .filter(|name| !project_columns.iter().any(|column| column == name))
        .map(str::to_string)
        .collect();
    let required_columns = [
        "metadata_rotation_degrees",
        "default_rotation_degrees",
        "rotation_degrees",
        "orientation_probed",
        "labels_json",
        "removed",
        "media_object_id",
        "media_verification_state",
    ];
    let asset_columns = if tables.iter().any(|table| table == "assets") {
        connection
            .prepare("PRAGMA table_info(assets)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        Vec::new()
    };
    let pending_asset_columns = required_columns
        .into_iter()
        .filter(|name| !asset_columns.iter().any(|column| column == name))
        .map(str::to_string)
        .collect();
    let project_count = if missing_required_tables.is_empty() {
        connection.query_row("SELECT COUNT(*) FROM projects", [], |row| {
            row.get::<_, i64>(0)
        })? as u64
    } else {
        0
    };
    let asset_count = if missing_required_tables.is_empty() {
        connection.query_row("SELECT COUNT(*) FROM assets", [], |row| {
            row.get::<_, i64>(0)
        })? as u64
    } else {
        0
    };
    let journal_mode = connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    let migration_history = inspect_project_catalog_migrations(&connection)?;
    let unresolved_conflict_count = if table_exists(&connection, "operation_conflicts")? {
        connection.query_row(
            "SELECT COUNT(*) FROM operation_conflicts WHERE resolved_at IS NULL",
            [],
            |row| row.get::<_, i64>(0),
        )? as u64
    } else {
        0
    };
    Ok(CatalogInspection {
        path: path.to_string_lossy().into_owned(),
        read_only: true,
        journal_mode,
        project_count,
        asset_count,
        missing_required_tables,
        pending_project_columns,
        pending_asset_columns,
        schema_history_present: migration_history.history_present,
        current_schema_version: migration_history.current_version,
        latest_supported_schema_version: migration_history.latest_supported_version,
        schema_status: migration_history.status,
        applied_migrations: migration_history.applied_migrations,
        pending_migrations: migration_history.pending_migrations,
        unresolved_conflict_count,
    })
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

#[derive(Debug, Default)]
struct MediaProbe {
    duration_seconds: f64,
    frame_rate: f64,
    width: u32,
    height: u32,
    codec: String,
    camera: String,
    captured_at_raw: Option<String>,
    timestamp_candidates: Vec<chronology::TimestampCandidate>,
    best_time: Option<String>,
    metadata_rotation_degrees: u16,
    default_rotation_degrees: u16,
}

impl Catalog {
    pub fn open(database_path: impl AsRef<Path>) -> Result<Self> {
        let database_path = database_path.as_ref().to_path_buf();
        if database_path.is_file() {
            let connection =
                Connection::open_with_flags(&database_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .with_context(|| format!("inspect catalog {}", database_path.display()))?;
            migrations::validate_project_catalog_migrations(&connection)?;
        }
        if let Some(parent) = database_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create catalog directory {}", parent.display()))?;
        }
        let catalog = Self {
            database_path,
            read_only: false,
        };
        catalog.migrate()?;
        catalog.recover_interrupted_resource_calibrations()?;
        // D01-T007 decision 4 "applies after upgrade": operations an older
        // build quarantined (persisted but not materialized) are re-dispatched
        // from the log by a build that knows their type. Writable open only —
        // `open_read_only_existing` must stay side-effect free.
        catalog.redispatch_chronology_operations()?;
        Ok(catalog)
    }

    pub fn open_existing(database_path: impl AsRef<Path>) -> Result<Self> {
        let database_path = database_path.as_ref();
        Self::validate_existing(database_path)?;
        Self::open(database_path)
    }

    /// Opens an existing catalog without running migrations or permitting writes.
    pub fn open_read_only_existing(database_path: impl AsRef<Path>) -> Result<Self> {
        let database_path = database_path.as_ref();
        Self::validate_existing(database_path)?;
        Ok(Self {
            database_path: database_path.to_path_buf(),
            read_only: true,
        })
    }

    fn validate_existing(database_path: &Path) -> Result<()> {
        if !database_path.is_file() {
            bail!("catalog file not found: {}", database_path.display());
        }
        let connection =
            Connection::open_with_flags(database_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .with_context(|| format!("inspect catalog {}", database_path.display()))?;
        for table in ["projects", "assets"] {
            let exists: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                [table],
                |row| row.get(0),
            )?;
            if !exists {
                bail!(
                    "not an RCD VlogCutter catalog: missing {table} table in {}",
                    database_path.display()
                );
            }
        }
        Ok(())
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    fn connection(&self) -> Result<Connection> {
        let connection = if self.read_only {
            Connection::open_with_flags(&self.database_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        } else {
            Connection::open(&self.database_path)
        }
        .with_context(|| format!("open catalog {}", self.database_path.display()))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        if self.read_only {
            connection.pragma_update(None, "query_only", "ON")?;
        } else {
            connection.pragma_update(None, "journal_mode", "WAL")?;
        }
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(connection)
    }

    fn migrate(&self) -> Result<()> {
        let mut connection = self.connection()?;
        migrations::migrate_project_catalog(&mut connection)
    }

    fn redispatch_chronology_operations(&self) -> Result<()> {
        let mut connection = self.connection()?;
        chronology::redispatch_chronology_operations(&mut connection)
    }

    pub fn create_project(&self, name: &str) -> Result<Project> {
        let name = name.trim();
        if name.is_empty() {
            bail!("project name cannot be empty");
        }
        let id = format!("project_{}", Uuid::new_v4().simple());
        let created_at = now();
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let origin_node_id = operations::ensure_node(&transaction)?;
        transaction.execute(
            "INSERT INTO projects (id, name, created_at, origin_node_id) VALUES (?1, ?2, ?3, ?4)",
            params![id, name, created_at, origin_node_id],
        )?;
        transaction.commit()?;
        Ok(Project {
            id,
            name: name.to_string(),
            created_at,
            asset_count: 0,
            labeled_count: 0,
            removed_count: 0,
            origin_node_id: Some(origin_node_id),
            paired_server_node_id: None,
            paired_at: None,
        })
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let connection = self.connection()?;
        let labels = asset_column_expression(&connection, "labels_json", "a.labels_json", "'[]'")?;
        let removed = asset_column_expression(&connection, "removed", "a.removed", "0")?;
        let origin_node_id = table_column_expression(
            &connection,
            "projects",
            "origin_node_id",
            "p.origin_node_id",
            "NULL",
        )?;
        let paired_server_node_id = table_column_expression(
            &connection,
            "projects",
            "paired_server_node_id",
            "p.paired_server_node_id",
            "NULL",
        )?;
        let paired_at =
            table_column_expression(&connection, "projects", "paired_at", "p.paired_at", "NULL")?;
        let sql = format!(
            r#"
            SELECT p.id, p.name, p.created_at,
                   COUNT(a.id),
                   COALESCE(SUM(CASE WHEN {labels} != '[]' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN {removed} = 1 THEN 1 ELSE 0 END), 0),
                   {origin_node_id}, {paired_server_node_id}, {paired_at}
            FROM projects p
            LEFT JOIN assets a ON a.project_id = p.id AND COALESCE(a.membership_removed, 0) = 0
            GROUP BY p.id
            ORDER BY p.created_at DESC
            "#
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map([], |row| {
            Ok(Project {
                id: row.get(0)?,
                name: row.get(1)?,
                created_at: row.get(2)?,
                asset_count: row.get::<_, i64>(3)? as u64,
                labeled_count: row.get::<_, i64>(4)? as u64,
                removed_count: row.get::<_, i64>(5)? as u64,
                origin_node_id: row.get(6)?,
                paired_server_node_id: row.get(7)?,
                paired_at: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn ingest_folder(&self, project_id: &str, root: impl AsRef<Path>) -> Result<IngestReport> {
        self.ingest_folder_with_progress(project_id, root, |_, _, _| {})
    }

    pub fn ingest_folder_with_progress<F>(
        &self,
        project_id: &str,
        root: impl AsRef<Path>,
        on_progress: F,
    ) -> Result<IngestReport>
    where
        F: FnMut(usize, usize, &Path),
    {
        self.ingest_folder_with_camera_and_progress(project_id, root, None, on_progress)
    }

    /// Imports a folder with an explicit camera identity chosen by the user.
    /// The override is applied consistently to every newly ingested asset.
    pub fn ingest_folder_with_camera<F>(
        &self,
        project_id: &str,
        root: impl AsRef<Path>,
        camera: &str,
        on_progress: F,
    ) -> Result<IngestReport>
    where
        F: FnMut(usize, usize, &Path),
    {
        let camera = camera.trim();
        if camera.is_empty() {
            bail!("camera override must not be empty");
        }
        self.ingest_folder_with_camera_and_progress(project_id, root, Some(camera), on_progress)
    }

    fn ingest_folder_with_camera_and_progress<F>(
        &self,
        project_id: &str,
        root: impl AsRef<Path>,
        camera_override: Option<&str>,
        mut on_progress: F,
    ) -> Result<IngestReport>
    where
        F: FnMut(usize, usize, &Path),
    {
        self.require_project(project_id)?;
        let root = root.as_ref();
        if !root.is_dir() {
            bail!("ingest path is not a directory: {}", root.display());
        }

        let candidates: Vec<PathBuf> = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.into_path())
            .filter(|path| is_media(path))
            .collect();

        let mut report = IngestReport {
            scanned: candidates.len(),
            imported: 0,
            duplicates: 0,
            relinked: 0,
            failures: Vec::new(),
        };

        let total = candidates.len();
        for (index, path) in candidates.into_iter().enumerate() {
            match match camera_override {
                Some(camera) => self.ingest_file_with_camera(project_id, &path, Some(camera)),
                None => self.ingest_file(project_id, &path),
            } {
                Ok(IngestOutcome::Imported) => report.imported += 1,
                Ok(IngestOutcome::Duplicate) => report.duplicates += 1,
                Ok(IngestOutcome::Relinked) => report.relinked += 1,
                Err(error) => {
                    // The transient report is not enough: a rejected file must
                    // still be discoverable after the run, and after relaunch
                    // (D01-T027). Sidecars remain folder-deduped, but each
                    // actual media candidate retains its own failure details.
                    let folder = path
                        .parent()
                        .map(|parent| parent.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let filename = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    // Recording a diagnostic must never itself fail an ingest.
                    let reason = format!("{error:#}");
                    let _ = if reason.contains("no video stream") {
                        self.record_no_video_stream(project_id, &folder, &filename, &reason)
                    } else if reason.contains("video stream") {
                        self.record_invalid_video_stream(project_id, &folder, &filename, &reason)
                    } else if filename.starts_with("._") {
                        self.record_unreadable_media(project_id, &folder, &filename, &reason)
                    } else {
                        self.record_unreadable_media_file(project_id, &folder, &filename, &reason)
                    };
                    report.failures.push(IngestFailure {
                        path: path.to_string_lossy().into_owned(),
                        error: format!("{error:#}"),
                    });
                }
            }
            on_progress(index + 1, total, &path);
        }
        Ok(report)
    }

    /// Finds originals this project has lost track of, somewhere the user points
    /// us at.
    ///
    /// Deliberately not `ingest_folder`: locating moved media must never quietly
    /// enlarge the project. Content this project does not already know is
    /// counted and left alone, so pointing at the wrong folder is a no-op the
    /// user can read rather than an import they have to undo.
    ///
    /// Matching is by content. Filenames and folder structure are not evidence —
    /// two cards legitimately both contain `C0001.MP4` (`PRD.md` §8.2).
    pub fn relink_folder<F>(
        &self,
        project_id: &str,
        root: impl AsRef<Path>,
        mut on_progress: F,
    ) -> Result<RelinkReport>
    where
        F: FnMut(usize, usize, &Path),
    {
        self.require_project(project_id)?;
        let root = root.as_ref();
        if !root.is_dir() {
            bail!("search path is not a directory: {}", root.display());
        }

        let candidates: Vec<PathBuf> = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.into_path())
            .filter(|path| is_media(path))
            .collect();

        let missing_before = self.offline_asset_count(project_id)?;
        let total = candidates.len();
        let mut report = RelinkReport {
            scanned: total,
            repaired: 0,
            already_online: 0,
            unrecognized: 0,
            still_missing: missing_before,
            failures: Vec::new(),
        };

        for (index, path) in candidates.into_iter().enumerate() {
            match self.relink_file(project_id, &path) {
                Ok(IngestOutcome::Relinked) => report.repaired += 1,
                Ok(IngestOutcome::Duplicate) => report.already_online += 1,
                Ok(IngestOutcome::Imported) => report.unrecognized += 1,
                Err(error) => report.failures.push(IngestFailure {
                    path: path.to_string_lossy().into_owned(),
                    error: format!("{error:#}"),
                }),
            }
            on_progress(index + 1, total, &path);
        }

        report.still_missing = self.offline_asset_count(project_id)?;
        // Whatever is reachable again should stop being reported as offline, and
        // whatever is still missing should be restated with its new count.
        self.resolve_offline_volume_diagnostics(project_id)?;
        Ok(report)
    }

    /// Matches one file against this project's assets without ever creating one.
    /// `Imported` here means "not recognized", which is the caller's cue to
    /// count it rather than act on it.
    pub(crate) fn relink_file(&self, project_id: &str, path: &Path) -> Result<IngestOutcome> {
        let metadata =
            fs::metadata(path).with_context(|| format!("read metadata for {}", path.display()))?;
        let fingerprint = sampled_fingerprint(path, metadata.len())?;
        let known: Option<(String, String, bool)> = self
            .connection()?
            .query_row(
                "SELECT id, path, COALESCE(membership_removed, 0) FROM assets WHERE project_id = ?1 AND fingerprint = ?2",
                params![project_id, fingerprint],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        match known {
            Some((asset_id, recorded_path, membership_removed)) => {
                if membership_removed {
                    return Ok(IngestOutcome::Imported);
                }
                self.relink_known_asset(&asset_id, &recorded_path, path, &fingerprint)
            }
            None => Ok(IngestOutcome::Imported),
        }
    }

    fn offline_asset_count(&self, project_id: &str) -> Result<usize> {
        Ok(self
            .list_assets(project_id)?
            .iter()
            .filter(|asset| !asset.original_available)
            .count())
    }

    /// Re-states which volumes are offline after a repair pass.
    ///
    /// Diagnostics are acknowledged rather than deleted, per this module's
    /// "acknowledge, don't delete" rule: a volume that comes back and later goes
    /// away again resurfaces with its history intact.
    fn resolve_offline_volume_diagnostics(&self, project_id: &str) -> Result<()> {
        let still_offline = self.detect_offline_volumes(project_id)?;
        let unresolved: Vec<&str> = still_offline
            .iter()
            .map(|diagnostic| diagnostic.dedupe_key.as_str())
            .collect();
        let existing = self.diagnostics(project_id, &DiagnosticFilter::default())?;
        for diagnostic in existing {
            if diagnostic.kind == "media.volume_offline"
                && !unresolved.contains(&diagnostic.dedupe_key.as_str())
            {
                self.acknowledge_diagnostic(project_id, &diagnostic.id)?;
            }
        }
        Ok(())
    }

    pub(crate) fn ingest_file(&self, project_id: &str, path: &Path) -> Result<IngestOutcome> {
        self.ingest_file_with_camera(project_id, path, None)
    }

    /// Same as `ingest_file`, except a genuinely new asset's camera is
    /// `camera_override` when given, instead of `probe_media`'s own
    /// EXIF-then-filename-pattern guess. This is the other half of #71's
    /// "the selected identity is used by both asset ingest and
    /// capture-sequence preflight" requirement: `preflight_import` resolves
    /// one identity into `ImportPreflight.camera_identity`, and `run_import`
    /// passes that same string here so the asset this creates and the
    /// capture sequence `materialize_asset_add` files it under (keyed on
    /// `payload.camera`, see `operations.rs`) can never disagree with what
    /// the dialog showed. Has no effect on already-known content: a relink
    /// never re-probes or re-assigns a camera.
    pub(crate) fn ingest_file_with_camera(
        &self,
        project_id: &str,
        path: &Path,
        camera_override: Option<&str>,
    ) -> Result<IngestOutcome> {
        // Both of these touch the volume the file lives on, and a removable
        // volume that has gone away parks a read rather than failing it. Bound
        // them: the import's disconnect handling only runs on an `Err`, so a
        // call that never returns disables every recovery path we have.
        let stat_target = path.to_path_buf();
        let metadata = crate::stall::with_stall_timeout(
            &format!("reading metadata for {}", path.display()),
            crate::stall::io_stall_timeout(),
            move || fs::metadata(stat_target),
        )?
        .with_context(|| format!("read metadata for {}", path.display()))?;
        let fingerprint_target = path.to_path_buf();
        let size = metadata.len();
        let fingerprint = crate::stall::with_stall_timeout(
            &format!("fingerprinting {}", path.display()),
            crate::stall::io_stall_timeout(),
            move || sampled_fingerprint(&fingerprint_target, size),
        )??;
        let connection = self.connection()?;
        let known: Option<(String, String, bool)> = connection
            .query_row(
                "SELECT id, path, COALESCE(membership_removed, 0) FROM assets WHERE project_id = ?1 AND fingerprint = ?2",
                params![project_id, fingerprint],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((asset_id, recorded_path, membership_removed)) = known {
            if membership_removed {
                drop(connection);
                // Undo only brings the asset back into the materialized project
                // view. The file we just fingerprinted may be a remounted card
                // or a freshly committed archive copy, so record it first.
                self.relink_known_asset(&asset_id, &recorded_path, path, &fingerprint)?;
                self.restore_project_asset_membership(project_id, &asset_id)?;
                return Ok(IngestOutcome::Imported);
            }
            // Seeing content we already know is evidence about *where that
            // content lives*, and throwing it away is how an asset ends up
            // stranded on a path that no longer exists. Register the discovered
            // location, and adopt it if the asset has nowhere else to resolve.
            return self.relink_known_asset(&asset_id, &recorded_path, path, &fingerprint);
        }

        let mut probe = probe_media(path)?;
        if let Some(camera) = camera_override
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            probe.camera = camera.to_string();
        }
        self.record_camera_identity_diagnostics(project_id, path, &probe.camera);
        let id = format!("asset_{}", Uuid::new_v4().simple());
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("media filename is not valid UTF-8"))?;
        drop(connection);
        self.add_project_asset(
            project_id,
            &path.to_string_lossy(),
            operations::ProjectAssetAddedPayload {
                id: id.clone(),
                fingerprint: fingerprint.clone(),
                filename: filename.to_string(),
                size_bytes: metadata.len(),
                duration_seconds: probe.duration_seconds,
                frame_rate: probe.frame_rate,
                width: probe.width,
                height: probe.height,
                codec: probe.codec,
                camera: probe.camera,
                captured_at_raw: probe.captured_at_raw,
                timestamp_candidates: Some(probe.timestamp_candidates),
                best_time: probe.best_time,
                metadata_rotation_degrees: probe.metadata_rotation_degrees,
                default_rotation_degrees: probe.default_rotation_degrees,
                created_at: now(),
            },
        )?;
        self.store_video_validation(&id, &fingerprint, "usable", None)?;
        Ok(IngestOutcome::Imported)
    }

    /// Records a rediscovered location for content this project already holds,
    /// repairing the asset when its recorded path has gone offline.
    ///
    /// The fingerprint has already been computed by the caller and matched an
    /// existing asset, so the discovered path is verified by construction — no
    /// second hash.
    fn relink_known_asset(
        &self,
        asset_id: &str,
        recorded_path: &str,
        discovered: &Path,
        fingerprint: &str,
    ) -> Result<IngestOutcome> {
        let discovered_text = discovered.to_string_lossy().into_owned();
        let node_id = self.node_id()?;
        let connection = self.connection()?;
        locations::record_verification(
            &connection,
            asset_id,
            &node_id,
            &discovered_text,
            locations::Verification {
                state: LocationVerificationState::Verified,
                fingerprint: Some(fingerprint),
                guard: locations::LocationGuard::read(discovered),
                at: &now(),
            },
        )?;

        if discovered_text == recorded_path {
            return Ok(IngestOutcome::Duplicate);
        }
        // The asset already resolves somewhere; a second copy is worth knowing
        // about but is no reason to move the asset off a working location.
        let recorded_state = locations::recorded_location(&connection, asset_id, recorded_path)?;
        if locations::location_is_available(
            recorded_path,
            recorded_state.map(|recorded| recorded.state),
            recorded_state.and_then(|recorded| recorded.guard),
        ) || locations::any_available_location(&connection, asset_id, &discovered_text)?
            .is_some()
        {
            return Ok(IngestOutcome::Duplicate);
        }

        locations::set_preferred_path(&connection, asset_id, &discovered_text)?;
        Ok(IngestOutcome::Relinked)
    }

    pub fn list_assets(&self, project_id: &str) -> Result<Vec<Asset>> {
        self.require_project(project_id)?;
        let connection = self.connection()?;
        let metadata_rotation = asset_column_expression(
            &connection,
            "metadata_rotation_degrees",
            "a.metadata_rotation_degrees",
            "0",
        )?;
        let default_rotation = asset_column_expression(
            &connection,
            "default_rotation_degrees",
            "a.default_rotation_degrees",
            "0",
        )?;
        let rotation =
            asset_column_expression(&connection, "rotation_degrees", "a.rotation_degrees", "0")?;
        let orientation_probed = asset_column_expression(
            &connection,
            "orientation_probed",
            "a.orientation_probed",
            "0",
        )?;
        let labels = asset_column_expression(&connection, "labels_json", "a.labels_json", "'[]'")?;
        let removed = asset_column_expression(&connection, "removed", "a.removed", "0")?;
        let media_object_id =
            asset_column_expression(&connection, "media_object_id", "a.media_object_id", "NULL")?;
        let media_verification_state = asset_column_expression(
            &connection,
            "media_verification_state",
            "a.media_verification_state",
            "'unlinked'",
        )?;
        let title = asset_column_expression(&connection, "title", "a.title", "''")?;
        let description =
            asset_column_expression(&connection, "description", "a.description", "''")?;
        let (thumbnail_path, thumbnail_quality, thumbnail_join) = if table_exists(
            &connection,
            "asset_thumbnails",
        )? {
            (
                "t.path",
                "COALESCE(t.quality, 0)",
                "LEFT JOIN asset_thumbnails t ON t.asset_id = a.id AND t.quality = (SELECT MAX(t2.quality) FROM asset_thumbnails t2 WHERE t2.asset_id = a.id)",
            )
        } else {
            ("NULL", "0", "")
        };
        let (location_state, location_size, location_mtime, location_join) =
            location_columns(&connection)?;
        let (revision, revision_join) = revision_columns(&connection)?;
        let camera_proxy_columns = camera_proxy_asset_columns(&connection)?;
        let sql = format!(
            r#"
            SELECT a.id, a.project_id, a.path, a.filename, a.fingerprint, a.size_bytes,
                   a.duration_seconds, a.frame_rate, a.width, a.height, a.codec, a.camera,
                   a.captured_at_raw, a.best_time, a.offset_seconds, a.proxy_path,
                   {metadata_rotation}, {default_rotation}, {rotation},
                   {orientation_probed}, {thumbnail_path}, {thumbnail_quality},
                   a.notes, {labels}, {removed}, {media_object_id},
                   {media_verification_state}, {title}, {description},
                   {location_state}, {location_size}, {location_mtime},
                   {revision}, {camera_proxy_columns}
            FROM assets a
            {thumbnail_join}
            {location_join}
            {revision_join}
            WHERE a.project_id = ?1 AND COALESCE(a.membership_removed, 0) = 0
            ORDER BY a.best_time IS NULL, a.best_time, a.filename, a.id
            "#
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map([project_id], row_to_asset)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Lists assets after explicitly probing legacy rows whose orientation is unknown.
    pub fn list_assets_with_refreshed_orientation(&self, project_id: &str) -> Result<Vec<Asset>> {
        self.list_assets(project_id)?
            .into_iter()
            .map(|asset| {
                if !asset.orientation_probed && asset.original_available {
                    self.refresh_asset_orientation(&asset.id)
                } else {
                    Ok(asset)
                }
            })
            .collect()
    }

    pub fn get_asset(&self, asset_id: &str) -> Result<Asset> {
        let connection = self.connection()?;
        let metadata_rotation = asset_column_expression(
            &connection,
            "metadata_rotation_degrees",
            "a.metadata_rotation_degrees",
            "0",
        )?;
        let default_rotation = asset_column_expression(
            &connection,
            "default_rotation_degrees",
            "a.default_rotation_degrees",
            "0",
        )?;
        let rotation =
            asset_column_expression(&connection, "rotation_degrees", "a.rotation_degrees", "0")?;
        let orientation_probed = asset_column_expression(
            &connection,
            "orientation_probed",
            "a.orientation_probed",
            "0",
        )?;
        let labels = asset_column_expression(&connection, "labels_json", "a.labels_json", "'[]'")?;
        let removed = asset_column_expression(&connection, "removed", "a.removed", "0")?;
        let media_object_id =
            asset_column_expression(&connection, "media_object_id", "a.media_object_id", "NULL")?;
        let media_verification_state = asset_column_expression(
            &connection,
            "media_verification_state",
            "a.media_verification_state",
            "'unlinked'",
        )?;
        let title = asset_column_expression(&connection, "title", "a.title", "''")?;
        let description =
            asset_column_expression(&connection, "description", "a.description", "''")?;
        let (thumbnail_path, thumbnail_quality, thumbnail_join) = if table_exists(
            &connection,
            "asset_thumbnails",
        )? {
            (
                "t.path",
                "COALESCE(t.quality, 0)",
                "LEFT JOIN asset_thumbnails t ON t.asset_id = a.id AND t.quality = (SELECT MAX(t2.quality) FROM asset_thumbnails t2 WHERE t2.asset_id = a.id)",
            )
        } else {
            ("NULL", "0", "")
        };
        let (location_state, location_size, location_mtime, location_join) =
            location_columns(&connection)?;
        let (revision, revision_join) = revision_columns(&connection)?;
        let camera_proxy_columns = camera_proxy_asset_columns(&connection)?;
        let sql = format!(
            r#"
            SELECT a.id, a.project_id, a.path, a.filename, a.fingerprint, a.size_bytes,
                   a.duration_seconds, a.frame_rate, a.width, a.height, a.codec, a.camera,
                   a.captured_at_raw, a.best_time, a.offset_seconds, a.proxy_path,
                   {metadata_rotation}, {default_rotation}, {rotation},
                   {orientation_probed}, {thumbnail_path}, {thumbnail_quality},
                   a.notes, {labels}, {removed}, {media_object_id},
                   {media_verification_state}, {title}, {description},
                   {location_state}, {location_size}, {location_mtime},
                   {revision}, {camera_proxy_columns}
            FROM assets a
            {thumbnail_join}
            {location_join}
            {revision_join}
            WHERE a.id = ?1
            "#
        );
        connection
            .query_row(&sql, [asset_id], row_to_asset)
            .optional()?
            .ok_or_else(|| anyhow!("asset not found: {asset_id}"))
    }

    /// Returns the path to this asset's original only after proving the bytes
    /// there are still this asset's bytes.
    ///
    /// Every operation that reads original media goes through here. Path
    /// existence is not enough: a reformatted card can put a different clip at
    /// the same `.../C0012.MP4`, and transcoding or exporting that would produce
    /// silently wrong output. See `docs/tasks/import/ARCHITECTURE.md` §3.2.
    ///
    /// The fingerprint is computed at most once per file version. A location
    /// already verified, whose size and modification time are unchanged, is
    /// returned without touching its contents, so repeated proxy or export runs
    /// over a large project pay the hash exactly once.
    pub fn resolve_original(&self, asset_id: &str) -> Result<PathBuf> {
        let asset = self.get_asset(asset_id)?;
        locations::require_path(asset_id, &asset.path)?;
        let path = PathBuf::from(&asset.path);
        let Some(observed) = locations::LocationGuard::read(&path) else {
            bail!("original is offline: {}", asset.path);
        };

        let connection = self.connection()?;
        let recorded = locations::recorded_location(&connection, asset_id, &asset.path)?;
        if let Some(recorded) = recorded {
            match recorded.state {
                LocationVerificationState::Mismatch => {
                    return Err(locations::mismatch_error(&asset.path));
                }
                LocationVerificationState::Verified if observed.matches(recorded.guard) => {
                    return Ok(path);
                }
                _ => {}
            }
        }

        let fingerprint = sampled_fingerprint(&path, observed.size_bytes as u64)?;
        let verified = fingerprint == asset.fingerprint;
        let node_id = self.node_id()?;
        locations::record_verification(
            &connection,
            asset_id,
            &node_id,
            &asset.path,
            locations::Verification {
                state: if verified {
                    LocationVerificationState::Verified
                } else {
                    LocationVerificationState::Mismatch
                },
                fingerprint: Some(&fingerprint),
                guard: Some(observed),
                at: &now(),
            },
        )?;
        if verified {
            Ok(path)
        } else {
            Err(locations::mismatch_error(&asset.path))
        }
    }

    /// This catalog's node identity, creating it on first use.
    fn node_id(&self) -> Result<String> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let node_id = operations::ensure_node(&transaction)?;
        transaction.commit()?;
        Ok(node_id)
    }

    pub fn update_asset(&self, asset_id: &str, patch: &AssetPatch) -> Result<Asset> {
        self.update_asset_with_operations(asset_id, patch)
    }

    pub fn apply_clock_event(
        &self,
        project_id: &str,
        camera: &str,
        start_filename: &str,
        offset_seconds: i64,
        label: &str,
    ) -> Result<ClockEvent> {
        self.create_clock_event_operation(project_id, camera, start_filename, offset_seconds, label)
    }

    /// Testable and adapter-reusable execution boundary. The caller supplies
    /// explicit media tools and may supply a plan so deterministic fallback can
    /// be exercised without pretending a device exists.
    #[allow(clippy::too_many_arguments)]
    fn generate_proxy_untracked_with_execution<F>(
        &self,
        asset_id: &str,
        refresh_orientation: bool,
        policy: ProxyEncoderPolicy,
        mut on_progress: F,
        ffmpeg: &Path,
        ffprobe: &Path,
        plan_override: Option<ProxyEncoderPlan>,
    ) -> Result<ProxyGenerated>
    where
        F: FnMut(f64),
    {
        let write = self
            .begin_proxy_write(asset_id)?
            .ok_or_else(|| RepresentationBusy {
                asset_id: asset_id.to_owned(),
                kind: RepresentationKind::Proxy,
            })?;
        let asset = if refresh_orientation {
            self.refresh_asset_orientation(asset_id)?
        } else {
            self.get_asset(asset_id)?
        };
        // Prove this is still the original clip before transcoding.
        let original = self.resolve_original(asset_id)?;
        let source_audio_present = probe_source_audio_present(ffprobe, &original)
            .context("probe source audio before proxy generation")?;
        let plan = plan_override.unwrap_or_else(|| resolve_proxy_encoder_plan(policy));
        let temporary = write.temporary_path();
        let outcome = match run_proxy_transcode_with_plan(
            ffmpeg,
            ffprobe,
            &original,
            temporary,
            asset.duration_seconds,
            source_audio_present,
            &plan,
            &mut on_progress,
        ) {
            Ok(outcome) => outcome,
            Err(failure) => {
                let _ = fs::remove_file(temporary);
                if let Err(error) =
                    self.persist_proxy_generation_provenance(asset_id, &failure.outcome)
                {
                    tracing::warn!(
                        target: "rcdvlog_core::proxy",
                        asset = %asset_id,
                        error = %error,
                        "failed to persist proxy generation failure provenance"
                    );
                }
                return Err(anyhow::Error::new(failure).context("ffmpeg proxy generation failed"));
            }
        };
        let outcome_for_result = outcome.clone();
        let asset = self.commit_generated_proxy_write(write, &outcome)?;
        Ok(ProxyGenerated {
            asset,
            outcome: outcome_for_result,
        })
    }

    /// Explicitly refreshes legacy orientation metadata from the immutable original.
    pub fn refresh_asset_orientation(&self, asset_id: &str) -> Result<Asset> {
        let asset = self.get_asset(asset_id)?;
        if asset.orientation_probed {
            return Ok(asset);
        }
        // Reading orientation off whatever happens to sit at the path would
        // rotate this clip by another clip's metadata.
        let original = self.resolve_original(asset_id)?;

        let probe = probe_media(&original)?;
        let rotation_degrees = if asset.rotation_degrees != asset.default_rotation_degrees {
            asset.rotation_degrees
        } else {
            probe.default_rotation_degrees
        };
        self.connection()?.execute(
            r#"
                UPDATE assets
                SET metadata_rotation_degrees = ?2, default_rotation_degrees = ?3,
                    rotation_degrees = ?4, orientation_probed = 1
                WHERE id = ?1
                "#,
            params![
                &asset.id,
                probe.metadata_rotation_degrees,
                probe.default_rotation_degrees,
                rotation_degrees,
            ],
        )?;
        self.get_asset(&asset.id)
    }

    pub fn generate_proxy(&self, asset_id: &str) -> Result<Asset> {
        self.generate_proxy_with_progress(asset_id, |_| {})
    }

    pub fn generate_proxy_with_progress<F>(&self, asset_id: &str, on_progress: F) -> Result<Asset>
    where
        F: FnMut(AssetBatchProgress),
    {
        self.generate_proxy_with_policy_and_progress(
            asset_id,
            ProxyEncoderPolicy::Auto,
            on_progress,
        )
    }

    /// Generate one proxy with an explicit encoder policy. `Auto` uses the
    /// shared capability probe and retains the software fallback; `Software`
    /// is deterministic and never initializes hardware.
    pub fn generate_proxy_with_policy(
        &self,
        asset_id: &str,
        policy: ProxyEncoderPolicy,
    ) -> Result<Asset> {
        self.generate_proxy_with_policy_and_progress(asset_id, policy, |_| {})
    }

    pub fn generate_proxy_with_policy_and_progress<F>(
        &self,
        asset_id: &str,
        policy: ProxyEncoderPolicy,
        on_progress: F,
    ) -> Result<Asset>
    where
        F: FnMut(AssetBatchProgress),
    {
        let _heavy = self.acquire_local_heavy()?;
        self.generate_proxy_with_orientation_policy_and_policy(asset_id, true, policy, on_progress)
    }

    fn generate_proxy_with_orientation_policy<F>(
        &self,
        asset_id: &str,
        refresh_orientation: bool,
        on_progress: F,
    ) -> Result<Asset>
    where
        F: FnMut(AssetBatchProgress),
    {
        self.generate_proxy_with_orientation_policy_and_policy(
            asset_id,
            refresh_orientation,
            ProxyEncoderPolicy::Auto,
            on_progress,
        )
    }

    fn generate_proxy_with_orientation_policy_and_policy<F>(
        &self,
        asset_id: &str,
        refresh_orientation: bool,
        policy: ProxyEncoderPolicy,
        mut on_progress: F,
    ) -> Result<Asset>
    where
        F: FnMut(AssetBatchProgress),
    {
        // Internal path: callers already own the one shared local-heavy slot.
        let asset = self.get_asset(asset_id)?;
        let task_work = [asset.size_bytes];
        let selection = self.resolve_resource_selection(
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            Some(asset.size_bytes),
            None,
            &ResourceOverrides::default(),
        )?;
        let effective_policy = if selection.source == "calibration"
            && selection.encoder.as_deref() == Some("software")
        {
            ProxyEncoderPolicy::Software
        } else {
            policy
        };
        let plan = resolve_proxy_encoder_plan(effective_policy);
        let planned_encoder = plan.preferred_encoder.as_str();
        let task_kind = proxy_task_kind_for_encoder(plan.preferred_encoder, 1);
        let history_key = self.qualified_history_key(
            &task_kind,
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            Some(planned_encoder),
            1,
            0,
            0,
        )?;
        let mut history = self.load_batch_history(&history_key)?;
        if history.samples == 0
            && selection.source == "calibration"
            && selection.worker_count == 1
            && selection.encoder.as_deref() == Some(planned_encoder)
            && let Some(rate) = selection
                .timing
                .as_ref()
                .and_then(|timing| timing.work_units_per_second)
        {
            history = RateHistory {
                work_units_per_worker_second: rate,
                samples: selection
                    .timing
                    .as_ref()
                    .map(|timing| timing.samples_ms.len() as u64)
                    .unwrap_or(0),
            };
        }
        let mut estimator = BatchEstimator::new(&task_work, 1, history);
        on_progress(AssetBatchProgress {
            asset: None,
            estimate: estimator.estimate(&task_work),
        });
        let started = Instant::now();
        let ffmpeg = resolve_tool("ffmpeg");
        let ffprobe = resolve_tool("ffprobe");
        let result = self.generate_proxy_untracked_with_execution(
            asset_id,
            refresh_orientation,
            policy,
            |fraction| {
                let partial_work = work_units_from_fraction(asset.size_bytes, fraction);
                let remaining_work = asset.size_bytes.saturating_sub(partial_work);
                on_progress(AssetBatchProgress {
                    asset: None,
                    estimate: estimator
                        .estimate_with_partial_progress(&[remaining_work], partial_work),
                });
            },
            &ffmpeg,
            &ffprobe,
            Some(plan),
        );
        let updated = match &result {
            Ok(generated) => {
                estimator.record_completion(asset.size_bytes, started.elapsed());
                let actual_encoder = ProxyEncoderKind::parse(&generated.outcome.selected_encoder)
                    .unwrap_or(ProxyEncoderKind::Software);
                let actual_task_kind = proxy_task_kind_for_encoder(actual_encoder, 1);
                let actual_history_key = self.qualified_history_key(
                    &actual_task_kind,
                    PROXY_REPRESENTATION_KIND,
                    PROXY_SOFTWARE_RECIPE_KEY,
                    Some(actual_encoder.as_str()),
                    1,
                    0,
                    0,
                )?;
                let mut history = self.load_batch_history(&actual_history_key)?;
                history.observe(asset.size_bytes, generated.outcome.successful_elapsed());
                self.save_batch_history(&actual_history_key, history)?;
                Some(generated.asset.clone())
            }
            Err(_) => {
                estimator.record_failure(asset.size_bytes);
                None
            }
        };
        on_progress(AssetBatchProgress {
            asset: updated,
            estimate: estimator.estimate(&[]),
        });
        result.map(|generated| generated.asset)
    }

    /// Fingerprint of the proxy a transcript would be derived from.
    ///
    /// Transcripts are pinned to the proxy file, so regenerating a proxy must
    /// make its transcript stale. This reuses the same sampled BLAKE3 scheme as
    /// asset identity rather than introducing a second one.
    pub fn proxy_fingerprint(&self, asset_id: &str) -> Result<String> {
        let asset = self.get_asset(asset_id)?;
        let proxy_path = Self::require_proxy(&asset)?;
        let size = fs::metadata(&proxy_path)
            .with_context(|| format!("stat proxy {}", proxy_path.display()))?
            .len();
        sampled_fingerprint(&proxy_path, size)
    }

    pub(crate) fn require_proxy(asset: &Asset) -> Result<PathBuf> {
        let path = asset
            .proxy_path
            .as_deref()
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .ok_or_else(|| {
                anyhow!(
                    "proxy is required to transcribe {}; generate it first",
                    asset.id
                )
            })?;
        Ok(path)
    }

    /// Replaces a clip's title and description with a human edit.
    ///
    /// These are AI-written but human-editable, which is why they live on the
    /// asset beside `notes` rather than inside the analysis row: an edit has to
    /// survive the observation being deleted or re-run. Deliberately a separate
    /// operation rather than part of `AssetPatch`, whose four fields are a
    /// full-replacement contract shared with the CLI and the browser.
    ///
    /// Written directly rather than as a typed operation, matching how analysis
    /// writes the same columns; neither is synchronized between nodes today.
    pub fn set_clip_summary(
        &self,
        asset_id: &str,
        title: &str,
        description: &str,
    ) -> Result<Asset> {
        let connection = self.connection()?;
        set_title_and_description(&connection, asset_id, title.trim(), description.trim())?;
        drop(connection);
        self.get_asset(asset_id)
    }

    /// Renames a camera identity across every clip and capture sequence in a project.
    ///
    /// The camera on an asset is a guess: `identify_camera` reads a filename
    /// pattern and whatever EXIF survived the card, and `UNKNOWN_CAMERA` is a
    /// routine outcome. The ribbon's lane gutter is where that guess becomes
    /// visible, so it is also where it gets corrected.
    ///
    /// Written as the same typed identity operation as an explicit merge so
    /// replay and synchronization keep clips, card lanes, and future ingest
    /// aliases coherent. Sequence and epoch identities remain intact. Returns
    /// the number of clips moved to the new lane.
    pub fn rename_camera(&self, project_id: &str, from: &str, to: &str) -> Result<usize> {
        Ok(self
            .rename_camera_identity(project_id, from, to)?
            .moved_clip_count)
    }

    /// Per-clip analysis facts the queue needs for every clip at once.
    ///
    /// The review queue filters on transcript text and flags unusable clips, so
    /// it needs these for the whole project. Fetching them per clip would be one
    /// round trip per row; this is two queries for the project. Only clips with
    /// something recorded appear.
    pub fn project_analysis_index(&self, project_id: &str) -> Result<Vec<AssetAnalysisIndexEntry>> {
        let connection = self.connection()?;
        let mut entries: HashMap<String, AssetAnalysisIndexEntry> = HashMap::new();

        let mut transcripts = connection.prepare(
            "SELECT t.asset_id, t.status, COALESCE(GROUP_CONCAT(s.text, ' '), '')
             FROM asset_transcripts t
             JOIN assets a ON a.id = t.asset_id
             LEFT JOIN transcript_segments s ON s.asset_id = t.asset_id
             WHERE a.project_id = ?1 AND COALESCE(a.membership_removed, 0) = 0
             GROUP BY t.asset_id, t.status",
        )?;
        let rows = transcripts.query_map([project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (asset_id, status, text) = row?;
            let entry = entries
                .entry(asset_id.clone())
                .or_insert_with(|| AssetAnalysisIndexEntry::empty(&asset_id));
            entry.transcript_status = Some(status);
            entry.transcript_text = text;
        }

        let mut analyses = connection.prepare(
            "SELECT n.asset_id, n.usable, n.unusable_reason
             FROM asset_analyses n
             JOIN assets a ON a.id = n.asset_id
             WHERE a.project_id = ?1 AND COALESCE(a.membership_removed, 0) = 0",
        )?;
        let rows = analyses.query_map([project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? != 0,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (asset_id, usable, unusable_reason) = row?;
            let entry = entries
                .entry(asset_id.clone())
                .or_insert_with(|| AssetAnalysisIndexEntry::empty(&asset_id));
            entry.analyzed = true;
            entry.usable = usable;
            entry.unusable_reason = unusable_reason;
        }

        let mut index = entries.into_values().collect::<Vec<_>>();
        index.sort_by(|left, right| left.asset_id.cmp(&right.asset_id));
        Ok(index)
    }

    /// The stored transcript for one clip, if it has one.
    ///
    /// `no_speech` is returned as a normal transcript with no segments, not as
    /// `None`: the difference between "the VAD gate found silence" and "this
    /// clip was never transcribed" is exactly what a reviewer needs to see.
    pub fn asset_transcript(&self, asset_id: &str) -> Result<Option<ClipTranscript>> {
        load_transcript(&self.connection()?, asset_id)
    }

    /// The stored observation for one clip, with the labels analysis authored.
    pub fn asset_analysis(&self, asset_id: &str) -> Result<Option<AssetAnalysisView>> {
        let connection = self.connection()?;
        let Some(analysis) = load_analysis(&connection, asset_id)? else {
            return Ok(None);
        };
        // Whether the observation still describes the current proxy. A stale
        // observation is shown rather than hidden, but must be marked: the clip
        // it describes is not the clip on screen.
        let current = self
            .proxy_fingerprint(asset_id)
            .ok()
            .is_some_and(|fingerprint| {
                analysis_is_current(&connection, asset_id, &fingerprint).unwrap_or(false)
            });
        Ok(Some(AssetAnalysisView {
            analysis,
            ai_labels: ai_labels_for_asset(&connection, asset_id)?,
            current,
        }))
    }

    /// Reports what an analysis run would do, cost, and need, without running it.
    ///
    /// Offline and read-only. Every count matches the skip rule of the stage it
    /// describes, so the dialog cannot promise work the batch will then skip.
    pub fn analysis_readiness(&self, project_id: &str) -> Result<AnalysisReadiness> {
        let connection = self.connection()?;
        let mut readiness = AnalysisReadiness {
            project_id: project_id.to_string(),
            total_clips: 0,
            reachable_originals: 0,
            unverified_video_clips: 0,
            quarantined_video_clips: 0,
            unavailable_video_clips: 0,
            missing_proxies: 0,
            missing_transcripts: 0,
            missing_analyses: 0,
            models: self.model_status()?,
            models_ready: false,
            api_key_present: resolve_api_key().is_ok(),
            estimated_cost_usd: 0.0,
            cost_basis: AnalysisCostBasis::Reference,
            observed_cost_per_clip_usd: 0.0,
            last_run: self.analysis_runs(project_id, 1)?.into_iter().next(),
        };
        readiness.models_ready = readiness.models.iter().all(|model| model.installed);

        for asset in self.list_assets(project_id)? {
            if asset.removed {
                continue;
            }
            let validation = self.asset_video_validation_for(&asset)?;
            if validation.bytes_reachable {
                readiness.reachable_originals += 1;
            }
            match validation.status {
                AssetVideoValidationStatus::Usable => {
                    if !validation.bytes_reachable && !asset.proxy_available {
                        readiness.unavailable_video_clips += 1;
                        continue;
                    }
                }
                AssetVideoValidationStatus::Unverified => {
                    readiness.unverified_video_clips += 1;
                    continue;
                }
                AssetVideoValidationStatus::Quarantined => {
                    readiness.quarantined_video_clips += 1;
                    continue;
                }
            }
            readiness.total_clips += 1;
            // A clip whose original is offline but whose proxy is present is not
            // "missing a proxy": there is no input to make a better one from.
            let proxy_is_the_only_representation =
                !asset.original_available && asset.proxy_available;
            if !has_current_proxy(&asset) && !proxy_is_the_only_representation {
                readiness.missing_proxies += 1;
            }
            // A clip without a current proxy has no current fingerprint, so it
            // counts as missing for both derived stages: stage zero will make
            // the proxy and the later stages will then have work to do.
            match self.proxy_fingerprint(&asset.id) {
                Ok(fingerprint) => {
                    if !is_current(&connection, &asset.id, &fingerprint)? {
                        readiness.missing_transcripts += 1;
                    }
                    if !analysis_is_current(&connection, &asset.id, &fingerprint)? {
                        readiness.missing_analyses += 1;
                    }
                }
                Err(_) => {
                    readiness.missing_transcripts += 1;
                    readiness.missing_analyses += 1;
                }
            }
        }

        let observed: Option<f64> = connection
            .query_row(
                "SELECT AVG(a.cost_usd) FROM asset_analyses a
                 JOIN assets s ON s.id = a.asset_id
                 WHERE s.project_id = ?1 AND a.cost_usd > 0",
                [project_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let per_clip = match observed {
            Some(cost) if cost > 0.0 => {
                readiness.cost_basis = AnalysisCostBasis::Observed;
                cost
            }
            _ => REFERENCE_COST_PER_CLIP_USD,
        };
        readiness.observed_cost_per_clip_usd = per_clip;
        readiness.estimated_cost_usd = per_clip * readiness.missing_analyses as f64;
        Ok(readiness)
    }

    /// Returns durable video eligibility separately from current byte reachability.
    /// Catalog rows created before explicit validation are unverified until checked.
    pub fn asset_video_validation(&self, asset_id: &str) -> Result<AssetVideoValidation> {
        let asset = self.get_asset(asset_id)?;
        self.asset_video_validation_for(&asset)
    }

    fn asset_video_validation_for(&self, asset: &Asset) -> Result<AssetVideoValidation> {
        let connection = self.connection()?;
        let has_validation_table = table_exists(&connection, "asset_video_validation")?;
        let stored: Option<(String, String, Option<String>, String)> = if has_validation_table {
            connection.query_row(
                "SELECT fingerprint, status, diagnostic, validated_at FROM asset_video_validation WHERE asset_id = ?1",
                [&asset.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            ).optional()?
        } else {
            None
        };
        let (status, diagnostic, validated_at) = match stored {
            Some((fingerprint, status, diagnostic, validated_at))
                if fingerprint == asset.fingerprint =>
            {
                let state = match status.as_str() {
                    "usable" => AssetVideoValidationStatus::Usable,
                    "quarantined" => AssetVideoValidationStatus::Quarantined,
                    _ => AssetVideoValidationStatus::Unverified,
                };
                (state, diagnostic, Some(validated_at))
            }
            _ => (AssetVideoValidationStatus::Unverified, None, None),
        };
        Ok(AssetVideoValidation {
            asset_id: asset.id.clone(),
            status,
            bytes_reachable: Path::new(&asset.path).is_file(),
            diagnostic,
            validated_at,
        })
    }

    /// Explicitly validates a legacy or new asset and durably records its video eligibility.
    /// Offline originals may validate against an existing proxy; neither representation is changed.
    pub fn validate_asset_video(&self, asset_id: &str) -> Result<AssetVideoValidation> {
        let asset = self.get_asset(asset_id)?;
        let validation_path = if Path::new(&asset.path).is_file() {
            PathBuf::from(&asset.path)
        } else if let Some(proxy_path) = asset
            .proxy_path
            .as_deref()
            .filter(|proxy_path| Path::new(proxy_path).is_file())
        {
            PathBuf::from(proxy_path)
        } else {
            bail!("original bytes and proxy are not reachable; video validation was not recorded");
        };
        let (status, diagnostic) = match probe_media_stream(&validation_path) {
            Ok(_) => ("usable", None),
            Err(error) => ("quarantined", Some(format!("{error:#}"))),
        };
        self.store_video_validation(asset_id, &asset.fingerprint, status, diagnostic.as_deref())?;
        self.asset_video_validation(asset_id)
    }

    fn store_video_validation(
        &self,
        asset_id: &str,
        fingerprint: &str,
        status: &str,
        diagnostic: Option<&str>,
    ) -> Result<()> {
        self.connection()?.execute(
            "INSERT INTO asset_video_validation(asset_id, fingerprint, status, diagnostic, validated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(asset_id) DO UPDATE SET fingerprint=excluded.fingerprint,
                status=excluded.status, diagnostic=excluded.diagnostic, validated_at=excluded.validated_at",
            params![asset_id, fingerprint, status, diagnostic, now()],
        )?;
        Ok(())
    }

    /// Opens a run record so spend is attributable even if the app exits.
    pub fn begin_analysis_run(&self, project_id: &str) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        self.connection()?.execute(
            "INSERT INTO analysis_runs (id, project_id, started_at, status, stage_reached)
             VALUES (?1, ?2, ?3, 'running', 'proxies')",
            params![
                id,
                project_id,
                Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
            ],
        )?;
        Ok(id)
    }

    /// Opens a run owned by a fully ledgered runtime. The marker is written in
    /// the same row as the run identity so a legitimate zero-request cancelled
    /// pilot is distinguishable from untracked legacy spending.
    pub fn begin_accounted_analysis_run(
        &self,
        project_id: &str,
        policy_id: &str,
        stage_reached: &str,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        self.connection()?.execute(
            "INSERT INTO analysis_runs
                (id, project_id, started_at, status, stage_reached, policy_id, accounting_contract)
             VALUES (?1, ?2, ?3, 'running', ?4, ?5, ?6)",
            params![
                id,
                project_id,
                Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                stage_reached,
                policy_id,
                crate::analysis::accounting::ANALYSIS_ACCOUNTING_SCHEMA_VERSION,
            ],
        )?;
        Ok(id)
    }

    /// Closes a run record with what it actually did.
    pub fn finish_analysis_run(&self, run_id: &str, outcome: &AnalysisRunOutcome) -> Result<()> {
        self.connection()?.execute(
            "UPDATE analysis_runs
             SET finished_at=?2, status=?3, stage_reached=?4, proxies_generated=?5,
                 clips_transcribed=?6, clips_analyzed=?7, cost_usd=?8, budget_exhausted=?9,
                 error=?10
             WHERE id=?1",
            params![
                run_id,
                Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                outcome.status,
                outcome.stage_reached,
                outcome.proxies_generated as i64,
                outcome.clips_transcribed as i64,
                outcome.clips_analyzed as i64,
                outcome.cost_usd,
                outcome.budget_exhausted as i64,
                outcome.error,
            ],
        )?;
        Ok(())
    }

    /// Most recent runs for a project, newest first.
    pub fn analysis_runs(&self, project_id: &str, limit: usize) -> Result<Vec<AnalysisRunRecord>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, project_id, started_at, finished_at, status, stage_reached,
                    policy_id, proxies_generated, clips_transcribed, clips_analyzed,
                    cost_usd, budget_exhausted, error, accounting_contract
             FROM analysis_runs WHERE project_id=?1
             ORDER BY started_at DESC, id DESC LIMIT ?2",
        )?;
        let runs = statement
            .query_map(params![project_id, limit as i64], |row| {
                Ok(AnalysisRunRecord {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    started_at: row.get(2)?,
                    finished_at: row.get(3)?,
                    status: row.get(4)?,
                    stage_reached: row.get(5)?,
                    policy_id: row.get(6)?,
                    proxies_generated: row.get::<_, i64>(7)? as usize,
                    clips_transcribed: row.get::<_, i64>(8)? as usize,
                    clips_analyzed: row.get::<_, i64>(9)? as usize,
                    cost_usd: row.get(10)?,
                    budget_exhausted: row.get::<_, i64>(11)? != 0,
                    error: row.get(12)?,
                    accounting_contract: row.get(13)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(runs)
    }

    /// Reports whether each speech model is installed for this catalog.
    ///
    /// Read-only and offline: it never downloads and never creates directories.
    pub fn model_status(&self) -> Result<Vec<ModelStatus>> {
        speech_model_status(&self.database_path)
    }

    /// Downloads whichever speech models are missing.
    ///
    /// This is the only path that fetches a model. It exists so installation is
    /// an explicit user action; no batch calls it. The legacy return type keeps
    /// desktop compatibility; use `install_speech_models_with_progress` when the
    /// caller needs an explicit cancellation outcome.
    pub fn install_models_with_progress<F>(&self, on_progress: F) -> Result<Vec<ModelStatus>>
    where
        F: FnMut(ModelInstallProgress) -> ControlFlow<()>,
    {
        let report = install_speech_models_with_progress(&self.database_path, on_progress)?;
        if report.cancelled {
            bail!("model download cancelled");
        }
        Ok(report.models)
    }

    /// Transcribes one clip, replacing any previous transcript.
    pub fn transcribe_asset(&self, asset_id: &str) -> Result<TranscribeOutcome> {
        self.transcribe_asset_with_resource_overrides(asset_id, &ResourceOverrides::default())
    }

    /// Transcribe one clip with explicit, validated resource overrides. An
    /// override is provenance-bearing and never presented as calibrated.
    ///
    /// Audio presence is probed before the heavy slot or speech engine is
    /// acquired. A valid clip with no audio therefore completes as `no_audio`
    /// without initializing models, and a probe error remains a processing
    /// failure rather than being reclassified as silence.
    pub fn transcribe_asset_with_resource_overrides(
        &self,
        asset_id: &str,
        overrides: &ResourceOverrides,
    ) -> Result<TranscribeOutcome> {
        // Validate even when a no-audio fast path will not open an engine.
        let selection = self.resolve_resource_selection(
            SPEECH_REPRESENTATION_KIND,
            STT_RECIPE_KEY,
            None,
            None,
            overrides,
        )?;
        let asset = self.get_asset(asset_id)?;
        let proxy_path = Self::require_proxy(&asset)?;
        let source_fingerprint = self.proxy_fingerprint(asset_id)?;
        let audio_present = match self.transcription_audio_present(&proxy_path) {
            Ok(present) => present,
            Err(error) => {
                let message = format!("{error:#}");
                save_transcript(
                    &self.connection()?,
                    &failed_transcript(asset_id, &source_fingerprint, message.clone()),
                )?;
                return Err(error);
            }
        };
        if !audio_present {
            let stored = no_audio_transcript(asset_id, &source_fingerprint);
            save_transcript(&self.connection()?, &stored)?;
            return Ok(transcribe_outcome(asset_id, &stored));
        }

        let _heavy = self.acquire_local_heavy()?;
        let engine = match self
            .open_stt_engine_with_threads(selection.vad_threads, selection.decode_threads)
        {
            Ok(engine) => engine,
            Err(error) => {
                let message = format!("{error:#}");
                save_transcript(
                    &self.connection()?,
                    &failed_transcript(asset_id, &source_fingerprint, message),
                )?;
                return Err(error);
            }
        };
        match self.transcribe_source_with_engine(
            asset_id,
            &proxy_path,
            &source_fingerprint,
            engine.as_ref(),
        ) {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                let message = format!("{error:#}");
                let (speech_seconds, window_count, provider_segment_count) =
                    failure_evidence(&error);
                save_transcript(
                    &self.connection()?,
                    &failed_transcript_with_evidence(
                        asset_id,
                        &source_fingerprint,
                        message,
                        speech_seconds,
                        window_count,
                        provider_segment_count,
                    ),
                )?;
                Err(error)
            }
        }
    }

    pub(crate) fn open_stt_engine_with_threads(
        &self,
        vad_threads: usize,
        decode_threads: usize,
    ) -> Result<Box<dyn SttEngine>> {
        let directory = model_dir(&self.database_path)?;
        let whisper = ensure_model(&directory, &WHISPER_MODEL)?;
        let vad = ensure_model(&directory, &VAD_MODEL)?;
        if vad_threads == 4 && decode_threads == 4 {
            Ok(Box::new(WhisperEngine::new(&whisper, &vad)?))
        } else {
            Ok(Box::new(WhisperEngine::with_threads(
                &whisper,
                &vad,
                vad_threads,
                decode_threads,
            )?))
        }
    }

    /// Probes the proxy that transcription will actually read. Reuses the
    /// reviewed FFprobe helper from proxy generation so corrupt media and tool
    /// failures stay errors; only a successful probe with no audio stream is a
    /// no-audio outcome.
    pub(crate) fn transcription_audio_present(&self, proxy_path: &Path) -> Result<bool> {
        let ffprobe = resolve_tool_program(Path::new("ffprobe"));
        probe_source_audio_present(&ffprobe, proxy_path)
            .with_context(|| format!("probe proxy audio {}", proxy_path.display()))
    }

    /// Extracts PCM from an already-probed audio-bearing proxy and runs the
    /// speech pipeline. Callers must probe first so the engine is never opened
    /// solely to discover that a valid video has no audio stream.
    pub(crate) fn transcribe_source_with_engine(
        &self,
        asset_id: &str,
        proxy_path: &Path,
        source_fingerprint: &str,
        engine: &dyn SttEngine,
    ) -> Result<TranscribeOutcome> {
        let asset = self.get_asset(asset_id)?;

        let parent = self
            .database_path
            .parent()
            .ok_or_else(|| anyhow!("catalog has no parent directory"))?;
        let audio_dir = parent.join("cache").join("audio").join(&asset.project_id);
        fs::create_dir_all(&audio_dir)?;
        let pcm_path = audio_dir.join(format!("{asset_id}.{}.16k.wav", Uuid::new_v4()));

        let result = (|| -> Result<StoredTranscript> {
            FfmpegAudioExtractor.extract_pcm16k(proxy_path, &pcm_path)?;
            // Read once per clip. The engine receives samples, never a path, so
            // a multi-window clip does not re-read and re-convert the whole file
            // for every window.
            let pcm = read_pcm(&pcm_path)?;
            anyhow::ensure!(!pcm.is_empty(), "audio stream decoded to no PCM samples");
            transcribe_pcm(engine, asset_id, source_fingerprint, &pcm)
        })();
        // The extracted PCM is scratch, not a retained representation, so it is
        // removed whether or not the decode succeeded.
        let _ = fs::remove_file(&pcm_path);
        let stored = result?;

        save_transcript(&self.connection()?, &stored)?;
        Ok(transcribe_outcome(asset_id, &stored))
    }

    pub fn transcribe_missing(&self, project_id: &str) -> Result<TranscribeReport> {
        self.transcribe_missing_with_progress(project_id, |_| ControlFlow::Continue(()))
    }

    /// Transcript coverage for a project, without transcribing anything.
    ///
    /// A stored transcript whose proxy fingerprint no longer matches counts as
    /// `missing`, because that is what a `transcribe missing` run would redo.
    pub fn transcript_coverage(&self, project_id: &str) -> Result<TranscriptCoverage> {
        let connection = self.connection()?;
        let mut coverage = TranscriptCoverage {
            recipe_key: STT_RECIPE_KEY.to_string(),
            ..TranscriptCoverage::default()
        };
        for asset in self.list_assets(project_id)? {
            if asset.removed {
                continue;
            }
            coverage.total += 1;
            let stored = load_transcript(&connection, &asset.id)?;
            let matches_source =
                self.proxy_fingerprint(&asset.id)
                    .ok()
                    .is_some_and(|fingerprint| {
                        stored.as_ref().is_some_and(|row| {
                            row.recipe_key == STT_RECIPE_KEY
                                && row.source_fingerprint == fingerprint
                        })
                    });
            if !matches_source {
                coverage.missing += 1;
                continue;
            }
            // A matching failed attempt is visible as failed coverage, while
            // is_current still excludes it from automatic reuse.
            match stored {
                Some(stored) => match stored.status {
                    TranscriptStatus::Speech => coverage.speech += 1,
                    TranscriptStatus::NoSpeech => {
                        coverage.no_speech += 1;
                        if stored.outcome_reason
                            == crate::analysis::store::TranscriptOutcomeReason::LegacyNoSpeechUnverified
                        {
                            coverage.legacy_no_speech_unverified += 1;
                        }
                    }
                    TranscriptStatus::NoAudio => coverage.no_audio += 1,
                    TranscriptStatus::NoAcceptedSpeech => coverage.no_accepted_speech += 1,
                    TranscriptStatus::Rejected => coverage.rejected += 1,
                    TranscriptStatus::Failed => coverage.failed += 1,
                },
                None => coverage.missing += 1,
            }
        }
        Ok(coverage)
    }

    /// Transcribes every clip in the project without a current transcript.
    ///
    /// Weighting is by clip, not source bytes, because decode cost tracks
    /// 30-second window count rather than file size. One worker: peak RSS is
    /// 1.15-1.38 GB and concurrent transcription alongside the four-worker
    /// proxy pipeline has never been measured.
    ///
    /// `on_progress` returning `ControlFlow::Break` stops the batch after the
    /// current clip. The callback is the cancellation point rather than a
    /// separate flag parameter, so a new progress site cannot be added that
    /// forgets to check for cancellation.
    pub fn transcribe_missing_with_progress<F>(
        &self,
        project_id: &str,
        on_progress: F,
    ) -> Result<TranscribeReport>
    where
        F: FnMut(TranscribeBatchProgress) -> ControlFlow<()>,
    {
        self.transcribe_missing_with_resource_overrides_and_progress(
            project_id,
            &ResourceOverrides::default(),
            on_progress,
        )
    }

    /// Explicit resource-override form of `transcribe_missing`.
    pub fn transcribe_missing_with_resource_overrides_and_progress<F>(
        &self,
        project_id: &str,
        overrides: &ResourceOverrides,
        mut on_progress: F,
    ) -> Result<TranscribeReport>
    where
        F: FnMut(TranscribeBatchProgress) -> ControlFlow<()>,
    {
        // Validate overrides and resolve the selected engine settings before
        // acquiring the heavy slot or opening models.
        let selection = self.resolve_resource_selection(
            SPEECH_REPRESENTATION_KIND,
            STT_RECIPE_KEY,
            None,
            None,
            overrides,
        )?;
        let assets = self.list_assets(project_id)?;
        let history_key = selection.history_key.clone();
        let mut report = TranscribeReport {
            transcribed: 0,
            no_audio: 0,
            failed: 0,
            skipped: 0,
            cancelled: false,
            resource_source: selection.source.clone(),
            calibration_key: selection.calibration_key.clone(),
            vad_threads: selection.vad_threads,
            decode_threads: selection.decode_threads,
            worker_count: selection.worker_count,
            history_key: history_key.clone(),
            sample_compatibility: selection.sample_compatibility.clone(),
            outcomes: Vec::new(),
            failures: Vec::new(),
        };
        let probe_estimator = BatchEstimator::new(
            &vec![1; assets.len()],
            selection.worker_count.max(1),
            Default::default(),
        );
        let mut pending: Vec<ProjectPlannedAsset> = Vec::new();
        {
            let connection = self.connection()?;
            for asset in assets {
                let mut estimate = probe_estimator.estimate(&[]);
                estimate.estimated_remaining_seconds = None;
                estimate.estimated_remaining_seconds_range = None;
                estimate.confidence = "inspecting_audio".into();
                if on_progress(TranscribeBatchProgress {
                    outcome: None,
                    estimate,
                })
                .is_break()
                {
                    report.cancelled = true;
                    return Ok(report);
                }
                if asset.removed {
                    continue;
                }
                if self.asset_video_validation(&asset.id)?.status
                    != AssetVideoValidationStatus::Usable
                {
                    report.skipped += 1;
                    continue;
                }
                let source_fingerprint = match self.proxy_fingerprint(&asset.id) {
                    Ok(fingerprint) => fingerprint,
                    // A clip with no usable proxy cannot be transcribed. It is
                    // reported as a per-clip processing failure rather than
                    // aborting the whole batch.
                    Err(error) => {
                        pending.push(ProjectPlannedAsset::ProbeFailed {
                            asset,
                            source_fingerprint: String::new(),
                            reason: format!("{error:#}"),
                        });
                        continue;
                    }
                };
                if is_current(&connection, &asset.id, &source_fingerprint)? {
                    report.skipped += 1;
                    continue;
                }
                let proxy_path = match Self::require_proxy(&asset) {
                    Ok(path) => path,
                    Err(error) => {
                        pending.push(ProjectPlannedAsset::ProbeFailed {
                            asset,
                            source_fingerprint,
                            reason: format!("{error:#}"),
                        });
                        continue;
                    }
                };
                match self.transcription_audio_present(&proxy_path) {
                    Ok(false) => pending.push(ProjectPlannedAsset::NoAudio {
                        asset,
                        source_fingerprint,
                    }),
                    Ok(true) => pending.push(ProjectPlannedAsset::Ready {
                        asset,
                        proxy_path,
                        source_fingerprint,
                    }),
                    Err(error) => pending.push(ProjectPlannedAsset::ProbeFailed {
                        asset,
                        source_fingerprint,
                        reason: format!("probe audio before transcription: {error:#}"),
                    }),
                }
            }
        }

        let audio_pending = pending
            .iter()
            .filter(|item| matches!(item, ProjectPlannedAsset::Ready { .. }))
            .count();

        let task_work: Vec<u64> = vec![1; audio_pending];
        let mut estimator = BatchEstimator::new(
            &task_work,
            selection.worker_count.max(1),
            self.load_batch_history(&history_key)?,
        );
        let start = on_progress(TranscribeBatchProgress {
            outcome: None,
            estimate: estimator.estimate(&task_work),
        });
        if pending.is_empty() || start.is_break() {
            report.cancelled = start.is_break();
            return Ok(report);
        }

        let _heavy = if audio_pending > 0 {
            match self.wait_for_local_heavy(|| {
                let mut estimate = estimator.estimate(&task_work);
                estimate.confidence = "waiting_for_resource".into();
                estimate.estimated_remaining_seconds = None;
                on_progress(TranscribeBatchProgress {
                    outcome: None,
                    estimate,
                })
                .is_break()
            })? {
                Some(lease) => Some(lease),
                None => {
                    report.cancelled = true;
                    return Ok(report);
                }
            }
        } else {
            None
        };
        // Loading the model costs seconds and over a gigabyte of RSS, so it is
        // opened only for a clip that actually has an audio stream and only once
        // the batch is known to be non-empty and uncancelled.
        let mut engine: Option<Box<dyn SttEngine>> = None;
        let mut audio_processed = 0_usize;
        for item in pending.iter() {
            let remaining = &task_work[audio_processed.min(task_work.len())..];
            let flow;
            match item {
                ProjectPlannedAsset::NoAudio {
                    asset,
                    source_fingerprint,
                } => {
                    let stored = no_audio_transcript(&asset.id, source_fingerprint);
                    save_transcript(&self.connection()?, &stored)?;
                    let outcome = transcribe_outcome(&asset.id, &stored);
                    report.transcribed += 1;
                    report.no_audio += 1;
                    flow = on_progress(TranscribeBatchProgress {
                        outcome: Some(outcome.clone()),
                        estimate: estimator.estimate(remaining),
                    });
                    report.outcomes.push(outcome);
                }
                ProjectPlannedAsset::ProbeFailed {
                    asset,
                    source_fingerprint,
                    reason,
                } => {
                    let stored = failed_transcript(&asset.id, source_fingerprint, reason.clone());
                    save_transcript(&self.connection()?, &stored)?;
                    report.failed += 1;
                    report.failures.push(IngestFailure {
                        path: asset.path.clone(),
                        error: reason.clone(),
                    });
                    flow = on_progress(TranscribeBatchProgress {
                        outcome: None,
                        estimate: estimator.estimate(remaining),
                    });
                }
                ProjectPlannedAsset::Ready {
                    asset,
                    proxy_path,
                    source_fingerprint,
                } => {
                    if engine.is_none() {
                        engine = Some(self.open_stt_engine_with_threads(
                            selection.vad_threads,
                            selection.decode_threads,
                        )?);
                    }
                    let started = Instant::now();
                    let result = self.transcribe_source_with_engine(
                        &asset.id,
                        proxy_path,
                        source_fingerprint,
                        engine
                            .as_deref()
                            .expect("engine opened for audio-bearing asset"),
                    );
                    audio_processed += 1;
                    match result {
                        Ok(outcome) => {
                            estimator.record_completion(1, started.elapsed());
                            report.transcribed += 1;
                            flow = on_progress(TranscribeBatchProgress {
                                outcome: Some(outcome.clone()),
                                estimate: estimator
                                    .estimate(&task_work[audio_processed.min(task_work.len())..]),
                            });
                            report.outcomes.push(outcome);
                        }
                        Err(error) => {
                            estimator.record_failure(1);
                            let message = format!("{error:#}");
                            // Keep the exact source fingerprint, detected-speech
                            // duration, and partial provider evidence while
                            // leaving the failed row retryable.
                            let (speech_seconds, window_count, provider_segment_count) =
                                failure_evidence(&error);
                            let stored = failed_transcript_with_evidence(
                                &asset.id,
                                source_fingerprint,
                                message.clone(),
                                speech_seconds,
                                window_count,
                                provider_segment_count,
                            );
                            save_transcript(&self.connection()?, &stored)?;
                            report.failed += 1;
                            report.failures.push(IngestFailure {
                                path: asset.path.clone(),
                                error: message,
                            });
                            flow = on_progress(TranscribeBatchProgress {
                                outcome: None,
                                estimate: estimator
                                    .estimate(&task_work[audio_processed.min(task_work.len())..]),
                            });
                        }
                    }
                }
            }
            if flow.is_break() {
                // Completed clips are already committed, so a later run resumes
                // from here rather than repeating them.
                report.cancelled = true;
                break;
            }
        }

        if audio_pending > 0 {
            self.save_batch_history(&history_key, estimator.history())?;
        }
        Ok(report)
    }

    /// Runs cloud vision for one clip. This is an explicit replacement action;
    /// project-wide idempotence is provided by `analyze_missing`. The explicit
    /// invocation materializes and records an exact one-asset policy, then runs
    /// the same bounded capability pilot before the paid replacement call.
    pub fn analyze_asset(&self, asset_id: &str) -> Result<AnalysisOutcome> {
        let asset = self.get_asset(asset_id)?;
        let budget = resolve_analysis_budget(&AnalysisRunOptions::default())?;
        anyhow::ensure!(
            budget.is_finite(),
            "an explicit finite analysis budget is required for paid observation"
        );
        anyhow::ensure!(
            api_credential_present(),
            "OpenRouter credentials are not available; no analysis request was made"
        );
        let provider = OpenRouterProvider::new(resolve_api_key()?)?;
        let chain = ProviderChain::new();
        let mut request = crate::RunPolicyRequest::observation(
            asset.project_id.clone(),
            vec![asset_id.to_string()],
            chain.primary_model().to_string(),
            chain
                .allowed_models()
                .iter()
                .map(|model| (*model).to_string())
                .collect(),
            chain.allows_fallback(),
            budget,
            true,
        );
        request.include_project_vocabulary = true;
        self.authorize_run_policy(&request)?;
        let pilot = self.run_run_policy_pilot_with_provider(&request, &provider, false)?;
        anyhow::ensure!(
            pilot.status == "passed",
            "capability pilot failed for this exact policy: {} ({})",
            pilot.failure_code,
            pilot.validation.detail
        );
        let mut report =
            self.analyze_authorized_with_provider(&pilot.policy_id, &provider, |_| {
                ControlFlow::Continue(())
            })?;
        report
            .outcomes
            .pop()
            .ok_or_else(|| anyhow!("authorized single-asset analysis produced no result"))
    }

    pub(crate) fn analysis_frame_pack(&self, asset: &Asset) -> Result<FramePackManifest> {
        let proxy = Self::require_proxy(asset)?;
        let fingerprint = self.proxy_fingerprint(&asset.id)?;
        let connection = self.connection()?;
        if let Some(manifest) = load_frame_pack(&connection, &asset.id)?
            && manifest.recipe_key == FRAMEPACK_RECIPE_KEY
            && manifest.source_fingerprint == fingerprint
        {
            return Ok(manifest);
        }
        let manifest = build_frame_pack(&proxy, &fingerprint)?;
        save_frame_pack(&connection, &asset.id, &manifest)?;
        Ok(manifest)
    }

    pub(crate) fn prompt_vocabulary(&self, project_id: &str) -> Result<Vec<String>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT display_label FROM label_vocabulary WHERE project_id=?1 ORDER BY usage_count DESC, normalized_label",
        )?;
        let vocabulary = statement
            .query_map([project_id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        if !vocabulary.is_empty() {
            return Ok(vocabulary);
        }
        drop(statement);
        let mut labels = self
            .list_assets(project_id)?
            .into_iter()
            .flat_map(|asset| asset.labels)
            .collect::<Vec<_>>();
        labels.sort_by_key(|label| label.to_lowercase());
        labels.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        labels.truncate(80);
        Ok(labels)
    }

    pub(crate) fn prompt_transcript(&self, asset_id: &str) -> Result<Option<PromptTranscript>> {
        let Some(transcript) = load_transcript(&self.connection()?, asset_id)? else {
            return Ok(None);
        };
        let text = transcript
            .segments
            .iter()
            .map(|segment| segment.text.trim())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        Ok(Some(PromptTranscript {
            status: transcript.status,
            text,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn analyze_asset_with_provider(
        &self,
        asset_id: &str,
        provider: &dyn VlmProvider,
        chain: &ProviderChain,
        include_transcript: bool,
        approved_vocabulary: &[String],
        policy: &crate::RunPolicy,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<AnalysisOutcome> {
        let asset = self.get_asset(asset_id)?;
        let proxy = Self::require_proxy(&asset)?;
        let manifest = self.analysis_frame_pack(&asset)?;
        let parent = self
            .database_path
            .parent()
            .ok_or_else(|| anyhow!("catalog has no parent directory"))?;
        let frame_directory = parent
            .join("cache")
            .join("framepacks")
            .join(&asset.project_id)
            .join(&asset.id)
            .join(&manifest.source_fingerprint);
        let frame_paths = encode_frames(&proxy, &manifest, &frame_directory)?;
        let frames = frame_paths
            .iter()
            .map(|path| {
                std::fs::read(path)
                    .map(|bytes| frame_data_uri(&bytes))
                    .with_context(|| format!("read frame {}", path.display()))
            })
            .collect::<Result<Vec<_>>>()?;
        let transcript = if include_transcript {
            self.prompt_transcript(asset_id)?
        } else {
            None
        };
        let transcript_used = transcript.as_ref().is_some_and(|value| {
            value.status == TranscriptStatus::Speech && !value.text.is_empty()
        });
        let context = PromptContext {
            frame_timestamps_ms: manifest.timestamps_ms.clone(),
            clip_duration_ms: (asset.duration_seconds.max(0.0) * 1_000.0).round() as u32,
            transcript,
            vocabulary: approved_vocabulary.to_vec(),
            trip_context: None,
        };
        self.validate_run_policy_asset(policy, asset_id)?;
        let outcome = run_vlm_with_cancel(provider, chain, &context, frames, is_cancelled)?;
        let mut stored = if let Some(observation) = &outcome.observation {
            StoredAnalysis::from_observation(asset_id, &outcome.model, observation)
        } else {
            StoredAnalysis {
                asset_id: asset_id.to_string(),
                recipe_key: FRAMEPACK_RECIPE_KEY.to_string(),
                prompt_version: VLM_PROMPT_VERSION.to_string(),
                provider: "openrouter".to_string(),
                model: outcome.model.clone(),
                status: if outcome.model.is_empty() {
                    AnalysisStatus::Failed
                } else {
                    AnalysisStatus::Rejected
                },
                reject_reason: outcome.reject_reason.clone(),
                usable: false,
                unusable_reason: None,
                best_moment_start_ms: None,
                best_moment_end_ms: None,
                thumbnail_timestamp_ms: None,
                camera_movement: CameraMovement::Other,
                ocr_text: Vec::new(),
                transcript_used,
                prompt_tokens: 0,
                completion_tokens: 0,
                cost_usd: 0.0,
                latency_ms: 0,
                attempts: outcome.attempts,
                attempted_models: outcome.attempted_models.clone(),
                usage_complete: outcome.usage_complete,
                created_at: now(),
            }
        };
        stored.transcript_used = transcript_used;
        stored.prompt_tokens = outcome.prompt_tokens;
        stored.completion_tokens = outcome.completion_tokens;
        stored.cost_usd = outcome.cost_usd;
        stored.latency_ms = outcome.latency_ms;
        stored.attempts = outcome.attempts;
        stored.attempted_models = outcome.attempted_models.clone();
        stored.usage_complete = outcome.usage_complete;

        if let Some(observation) = &outcome.observation {
            // Remove only labels whose local authorship side-table says this
            // stage created. Human labels are never part of this replacement.
            for label in ai_labels_for_asset(&self.connection()?, asset_id)? {
                self.remove_label(asset_id, &label)?;
            }
            clear_ai_labels(&self.connection()?, asset_id)?;
            set_title_and_description(
                &self.connection()?,
                asset_id,
                &observation.title,
                &observation.description,
            )?;
            let mut authored = Vec::new();
            for proposed in &observation.labels {
                self.add_label(asset_id, proposed)?;
                authored.push((proposed.clone(), proposed.clone()));
            }
            record_ai_labels(&self.connection()?, asset_id, &authored)?;
            if let Some(timestamp) = observation.thumbnail_timestamp_ms
                && let Some(source) = frame_paths.iter().find(|path| {
                    path.file_stem()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name == format!("f{timestamp:08}"))
                })
            {
                let destination = parent
                    .join("cache")
                    .join("thumbnails")
                    .join(asset_id)
                    .join("q10.v1.jpg");
                if let Some(_claim) =
                    self.try_claim_representation(asset_id, RepresentationKind::Thumbnail)?
                {
                    commit_thumbnail(&self.connection()?, asset_id, source, &destination)?;
                }
            }
        }
        save_analysis(&self.connection()?, &stored)?;
        Ok(outcome)
    }

    pub fn analysis_coverage(&self, project_id: &str) -> Result<AnalysisCoverage> {
        let connection = self.connection()?;
        let mut coverage = AnalysisCoverage {
            recipe_key: FRAMEPACK_RECIPE_KEY.to_string(),
            prompt_version: VLM_PROMPT_VERSION.to_string(),
            ..AnalysisCoverage::default()
        };
        for asset in self.list_assets(project_id)? {
            if asset.removed {
                continue;
            }
            coverage.total += 1;
            let current = match self.proxy_fingerprint(&asset.id) {
                Ok(fingerprint) => analysis_is_current(&connection, &asset.id, &fingerprint)?,
                Err(_) => false,
            };
            if !current {
                coverage.missing += 1;
                continue;
            }
            match analysis::vlm_store::load_analysis(&connection, &asset.id)? {
                Some(stored) => {
                    coverage.cost_usd += stored.cost_usd;
                    if stored.model == FALLBACK_MODEL {
                        coverage.fallback_served += 1;
                    }
                    match stored.status {
                        AnalysisStatus::Ok => coverage.ok += 1,
                        AnalysisStatus::Rejected => coverage.rejected += 1,
                        AnalysisStatus::Failed => coverage.failed += 1,
                    }
                }
                None => coverage.missing += 1,
            }
        }
        Ok(coverage)
    }

    pub fn analyze_missing(&self, project_id: &str, force: bool) -> Result<AnalyzeReport> {
        self.analyze_missing_with_progress(project_id, force, |_| ControlFlow::Continue(()))
    }

    pub fn analyze_missing_with_progress<F>(
        &self,
        project_id: &str,
        force: bool,
        on_progress: F,
    ) -> Result<AnalyzeReport>
    where
        F: FnMut(AnalyzeBatchProgress) -> ControlFlow<()>,
    {
        self.analyze_missing_with_options(
            project_id,
            &AnalysisRunOptions {
                force,
                ..AnalysisRunOptions::default()
            },
            on_progress,
        )
    }

    /// Runs the cloud analysis batch with explicit key, budget, and concurrency.
    ///
    /// An explicit adapter invocation is approval intent, but it is not a
    /// bypass: the core first materializes the exact concrete policy, records
    /// that approval, runs one bounded pilot, and only then admits the bulk
    /// batch. A caller may instead pass `policy_id` to reuse an already-approved
    /// and piloted policy.
    ///
    /// `on_progress` returning `ControlFlow::Break` stops the bulk batch;
    /// workers observe the stop between clips, so cancelling never abandons a
    /// request that has already been paid for.
    pub fn analyze_missing_with_options<F>(
        &self,
        project_id: &str,
        options: &AnalysisRunOptions,
        on_progress: F,
    ) -> Result<AnalyzeReport>
    where
        F: FnMut(AnalyzeBatchProgress) -> ControlFlow<()>,
    {
        self.analyze_missing_with_options_and_provider(project_id, options, None, on_progress)
    }

    /// Provider-injectable form of the default batch path. Tests and future
    /// adapters can prove that an explicit invocation without a policy id still
    /// materializes approval and runs a passed pilot before bulk transmission.
    pub(crate) fn analyze_missing_with_options_and_provider<F>(
        &self,
        project_id: &str,
        options: &AnalysisRunOptions,
        injected_provider: Option<&dyn VlmProvider>,
        mut on_progress: F,
    ) -> Result<AnalyzeReport>
    where
        F: FnMut(AnalyzeBatchProgress) -> ControlFlow<()>,
    {
        let mut prepared_options = None;
        let mut explicit_request = None;
        if let Some(policy_id) = &options.policy_id {
            prepared_options =
                Some(self.analysis_options_bound_to_policy(project_id, options, policy_id)?);
        } else {
            let requested_budget = resolve_analysis_budget(options)?;
            anyhow::ensure!(
                requested_budget.is_finite(),
                "an explicit finite analysis budget is required for paid analysis"
            );
            self.require_project(project_id)?;
            let (pending, skipped, force_overwrite_warnings) =
                self.analysis_pending_assets(project_id, options)?;
            if pending.is_empty() {
                return Ok(AnalyzeReport {
                    analyzed: 0,
                    skipped,
                    rejected: 0,
                    fallback_served: 0,
                    force_overwrite_warnings,
                    cost_usd: 0.0,
                    usage_complete: true,
                    cancelled: false,
                    budget_exhausted: false,
                    outcomes: Vec::new(),
                    failures: Vec::new(),
                });
            }

            let primary = options
                .primary_model
                .clone()
                .unwrap_or_else(|| DEFAULT_MODEL.to_string());
            let allowed_models = if options.allowed_models.is_empty() {
                vec![DEFAULT_MODEL.to_string(), FALLBACK_MODEL.to_string()]
            } else {
                options.allowed_models.clone()
            };
            let mut request = crate::RunPolicyRequest::observation(
                project_id.to_string(),
                pending.iter().map(|asset| asset.id.clone()).collect(),
                primary,
                allowed_models,
                options.allow_fallback.unwrap_or(true),
                requested_budget,
                options.force,
            );
            request.payload = options
                .payload
                .unwrap_or(crate::AnalysisPayload::FramesAndTranscript);
            request.include_project_vocabulary = options.include_project_vocabulary;
            explicit_request = Some(request);
        }

        let provider_owned;
        let provider: &dyn VlmProvider = match injected_provider {
            Some(provider) => provider,
            None => {
                let api_key = match options.api_key.clone() {
                    Some(key) => key,
                    None => {
                        anyhow::ensure!(
                            api_credential_present(),
                            "OpenRouter credentials are not available; no analysis request was made"
                        );
                        resolve_api_key()?
                    }
                };
                provider_owned = OpenRouterProvider::new(api_key)?;
                &provider_owned
            }
        };

        let policy_id = if let Some(request) = explicit_request {
            self.authorize_run_policy(&request)?;
            let mut cancelled = false;
            let pilot_result = self.run_run_policy_pilot_with_provider_and_cancel(
                &request,
                provider,
                false,
                || {
                    let mut estimate =
                        BatchEstimator::new(&[], 1, Default::default()).estimate(&[]);
                    estimate.confidence = "capability_pilot".into();
                    estimate.estimated_remaining_seconds = None;
                    cancelled |= on_progress(AnalyzeBatchProgress {
                        asset_id: None,
                        outcome: None,
                        estimate,
                    })
                    .is_break();
                    cancelled
                },
            );
            if cancelled {
                return Ok(AnalyzeReport::cancelled_before_work());
            }
            let pilot = pilot_result?;
            anyhow::ensure!(
                pilot.status == "passed",
                "capability pilot did not pass for this exact policy: {} ({}); no bulk requests were made",
                pilot.failure_code,
                pilot.validation.detail
            );
            self.analysis_options_bound_to_policy(project_id, options, &pilot.policy_id)?
                .policy_id
                .expect("bound policy id")
        } else {
            prepared_options
                .expect("policy-bound branch prepared options")
                .policy_id
                .expect("bound policy id")
        };

        // Route through the shared policy-bound run-accounting wrapper so both
        // explicit and pre-authorized invocations record a bulk run with the
        // same policy provenance.
        self.analyze_authorized_with_provider_and_workers(
            &policy_id,
            provider,
            options.worker_count,
            &mut on_progress,
        )
    }

    /// Validates caller-supplied flags against the stored policy and replaces
    /// them with the policy's authoritative scope, model, payload/context and
    /// immutable whole-policy budget. Model, payload, scope or budget approval
    /// for one run can never be broadened by a caller argument.
    fn analysis_options_bound_to_policy(
        &self,
        project_id: &str,
        options: &AnalysisRunOptions,
        policy_id: &str,
    ) -> Result<AnalysisRunOptions> {
        let state =
            self.authorized_policy_for_bulk(policy_id, crate::AnalysisRunKind::Observation)?;
        let policy = &state.authorization.policy;
        anyhow::ensure!(
            policy.project_id == project_id,
            "authorized analysis policy belongs to a different project"
        );
        anyhow::ensure!(
            policy.payload.uses_frames(),
            "authorized analysis policy does not select frames"
        );
        if let Some(requested) = options.primary_model.as_deref() {
            anyhow::ensure!(
                requested == policy.primary_model,
                "requested model does not match the authorized policy"
            );
        }
        if !options.allowed_models.is_empty() {
            anyhow::ensure!(
                options.allowed_models == policy.allowed_models,
                "requested model allowlist does not match the authorized policy"
            );
        }
        if let Some(requested) = options.budget_usd {
            anyhow::ensure!(
                (requested - policy.budget_usd).abs() < f64::EPSILON,
                "requested budget does not match the authorized policy"
            );
        }
        if let Some(requested) = options.allow_fallback {
            anyhow::ensure!(
                requested == policy.allow_fallback,
                "requested fallback policy does not match the authorized policy"
            );
        }
        if let Some(requested) = &options.selected_assets {
            let authorized = policy
                .scope
                .iter()
                .map(|asset| asset.asset_id.clone())
                .collect::<Vec<_>>();
            anyhow::ensure!(
                requested == &authorized,
                "requested asset scope does not exactly match the authorized policy"
            );
        }
        if let Some(payload) = options.payload {
            anyhow::ensure!(
                payload == policy.payload,
                "requested payload does not match the authorized policy"
            );
        }
        let mut effective = options.clone();
        effective.budget_usd = Some(policy.budget_usd);
        effective.primary_model = Some(policy.primary_model.clone());
        effective.allowed_models = policy.allowed_models.clone();
        effective.allow_fallback = Some(policy.allow_fallback);
        effective.selected_assets = Some(
            policy
                .scope
                .iter()
                .map(|asset| asset.asset_id.clone())
                .collect(),
        );
        effective.payload = Some(policy.payload);
        effective.include_project_vocabulary = policy.context.project_vocabulary;
        effective.policy_id = Some(policy_id.to_string());
        Ok(effective)
    }

    /// Pending asset selection shared by policy materialization and execution,
    /// so the approved IDs are the IDs the batch can actually consider.
    fn analysis_pending_assets(
        &self,
        project_id: &str,
        options: &AnalysisRunOptions,
    ) -> Result<(Vec<Asset>, usize, usize)> {
        let force = options.force;
        let selected = options.selected_assets.as_ref().map(|ids| {
            ids.iter()
                .cloned()
                .collect::<std::collections::HashSet<_>>()
        });
        let connection = self.connection()?;
        let mut skipped = 0;
        let mut force_overwrite_warnings = 0;
        let mut pending = Vec::new();
        let mut selected_seen = std::collections::HashSet::new();
        for asset in self.list_assets(project_id)? {
            if let Some(selected) = &selected {
                if !selected.contains(&asset.id) {
                    continue;
                }
                selected_seen.insert(asset.id.clone());
            }
            if asset.removed {
                continue;
            }
            if self.asset_video_validation(&asset.id)?.status != AssetVideoValidationStatus::Usable
            {
                skipped += 1;
                continue;
            }
            let current = self
                .proxy_fingerprint(&asset.id)
                .ok()
                .is_some_and(|fingerprint| {
                    analysis_is_current(&connection, &asset.id, &fingerprint).unwrap_or(false)
                });
            if current && !force {
                skipped += 1;
                continue;
            }
            if force && current && (!asset.title.is_empty() || !asset.description.is_empty()) {
                force_overwrite_warnings += 1;
            }
            pending.push(asset);
        }
        if let Some(selected) = &selected {
            anyhow::ensure!(
                selected_seen.len() == selected.len(),
                "authorized analysis scope contains assets outside this project"
            );
        }
        Ok((pending, skipped, force_overwrite_warnings))
    }

    /// Provider-injectable policy-bound execution boundary used by adapters and
    /// synthetic tests. The stored policy, not caller-supplied flags, determines
    /// the exact asset scope, allowlist, payload and remaining budget.
    pub fn analyze_authorized_with_provider<F>(
        &self,
        policy_id: &str,
        provider: &dyn crate::VlmProvider,
        on_progress: F,
    ) -> Result<AnalyzeReport>
    where
        F: FnMut(AnalyzeBatchProgress) -> ControlFlow<()>,
    {
        self.analyze_authorized_with_provider_and_workers(policy_id, provider, None, on_progress)
    }

    fn analyze_authorized_with_provider_and_workers<F>(
        &self,
        policy_id: &str,
        provider: &dyn crate::VlmProvider,
        worker_count: Option<usize>,
        mut on_progress: F,
    ) -> Result<AnalyzeReport>
    where
        F: FnMut(AnalyzeBatchProgress) -> ControlFlow<()>,
    {
        let state =
            self.authorized_policy_for_bulk(policy_id, crate::AnalysisRunKind::Observation)?;
        let policy = &state.authorization.policy;
        // Verify the same frozen whole-policy authority before opening an
        // accounted run row. The live remaining balance is an exposure
        // projection and is never passed downstream as a new ceiling.
        crate::analysis::run_policy::verify_bulk_authority(self, policy)?;
        let force = matches!(
            policy.parameters,
            crate::RunPolicyParameters::Observation { force: true }
        );
        let options = AnalysisRunOptions {
            force,
            worker_count,
            budget_usd: Some(policy.budget_usd),
            primary_model: Some(policy.primary_model.clone()),
            allowed_models: policy.allowed_models.clone(),
            allow_fallback: Some(policy.allow_fallback),
            selected_assets: Some(
                policy
                    .scope
                    .iter()
                    .map(|asset| asset.asset_id.clone())
                    .collect(),
            ),
            payload: Some(policy.payload),
            include_project_vocabulary: policy.context.project_vocabulary,
            policy_id: Some(policy_id.to_string()),
            ..AnalysisRunOptions::default()
        };
        let run_id =
            self.begin_accounted_analysis_run(&policy.project_id, policy_id, "analysis")?;
        let result = self.analyze_missing_with_provider(
            policy.project_id.as_str(),
            &options,
            provider,
            &run_id,
            &mut on_progress,
        );
        match result {
            Ok(mut report) => {
                let projection = self.analysis_accounting_aggregate_for_run(
                    policy_id,
                    policy.kind.as_str(),
                    &run_id,
                )?;
                report.cost_usd = projection.confirmed_cost_usd;
                report.usage_complete = !projection.has_unresolved_usage();
                let status = if report.cancelled {
                    "cancelled"
                } else if report.failures.is_empty() && report.usage_complete {
                    "completed"
                } else {
                    "failed"
                };
                self.finish_analysis_run(
                    &run_id,
                    &AnalysisRunOutcome {
                        status: status.into(),
                        stage_reached: "analysis".into(),
                        clips_analyzed: report.analyzed,
                        cost_usd: projection.confirmed_cost_usd,
                        budget_exhausted: report.budget_exhausted,
                        error: if report.failures.is_empty() {
                            String::new()
                        } else {
                            format!(
                                "analysis batch completed with {} failures",
                                report.failures.len()
                            )
                        },
                        ..Default::default()
                    },
                )?;
                Ok(report)
            }
            Err(error) => {
                let cost = self
                    .analysis_accounting_aggregate_for_run(policy_id, policy.kind.as_str(), &run_id)
                    .map(|projection| projection.confirmed_cost_usd)
                    .unwrap_or(0.0);
                let _ = self.finish_analysis_run(
                    &run_id,
                    &AnalysisRunOutcome {
                        status: "failed".into(),
                        stage_reached: "analysis".into(),
                        cost_usd: cost,
                        error: "analysis batch failed before completion".into(),
                        ..Default::default()
                    },
                );
                Err(error)
            }
        }
    }

    fn analyze_missing_with_provider<F>(
        &self,
        project_id: &str,
        options: &AnalysisRunOptions,
        provider: &dyn VlmProvider,
        run_id: &str,
        on_progress: &mut F,
    ) -> Result<AnalyzeReport>
    where
        F: FnMut(AnalyzeBatchProgress) -> ControlFlow<()>,
    {
        let policy_id = options.policy_id.as_deref().ok_or_else(|| {
            anyhow!("paid analysis must be bound to an authorized, piloted run policy")
        })?;
        let state =
            self.authorized_policy_for_bulk(policy_id, crate::AnalysisRunKind::Observation)?;
        let policy = &state.authorization.policy;
        ensure!(
            policy.project_id == project_id,
            "authorized analysis policy belongs to a different project"
        );
        let authorized_scope = policy
            .scope
            .iter()
            .map(|asset| asset.asset_id.clone())
            .collect::<Vec<_>>();
        ensure!(
            options.selected_assets.as_ref() == Some(&authorized_scope),
            "authorized analysis scope does not exactly match the requested asset ids"
        );
        ensure!(
            options.primary_model.as_deref() == Some(policy.primary_model.as_str())
                && options.allowed_models == policy.allowed_models
                && options.allow_fallback == Some(policy.allow_fallback),
            "authorized analysis model allowlist does not match the requested run"
        );
        ensure!(
            options.payload == Some(policy.payload)
                && options.include_project_vocabulary == policy.context.project_vocabulary,
            "authorized analysis payload/context does not match the requested run"
        );
        ensure!(
            options.budget_usd == Some(policy.budget_usd),
            "authorized analysis budget does not match the frozen whole policy ceiling"
        );
        ensure!(
            options.force == policy.parameters.force(),
            "authorized analysis force setting does not match the policy"
        );
        // Local-only credential/client preparation, before any run row or paid
        // transmission. A missing credential therefore causes zero provider calls.
        provider.prepare()?;
        let (pending, skipped, force_overwrite_warnings) =
            self.analysis_pending_assets(project_id, options)?;

        // Materialize frame packs first so the estimator's remaining weights
        // are exact frame counts, not clip-count or byte-size guesses.
        let mut prepared = Vec::new();
        let mut failures = Vec::new();
        for asset in pending {
            match self.analysis_frame_pack(&asset) {
                Ok(manifest) => prepared.push((asset, manifest.timestamps_ms.len() as u64)),
                Err(error) => failures.push(IngestFailure {
                    path: asset.path,
                    error: format!("{error:#}"),
                }),
            }
        }
        let task_work = prepared
            .iter()
            .map(|(_, frames)| *frames)
            .collect::<Vec<_>>();
        let worker_count = options
            .worker_count
            .or_else(|| {
                std::env::var("RCDVLOG_VLM_WORKERS")
                    .ok()
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .unwrap_or(MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY)
            .clamp(1, prepared.len().max(1))
            .min(MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY);
        let task_key = format!(
            "vlm.observe-v2.{}-w{worker_count}-v1",
            options
                .primary_model
                .as_deref()
                .unwrap_or(DEFAULT_MODEL)
                .replace('/', "-")
        );
        let mut estimator = BatchEstimator::new(
            &task_work,
            worker_count,
            self.load_batch_history(&task_key)?,
        );
        let start = on_progress(AnalyzeBatchProgress {
            asset_id: None,
            outcome: None,
            estimate: estimator.estimate(&task_work),
        });
        let mut report = AnalyzeReport {
            analyzed: 0,
            skipped,
            rejected: 0,
            fallback_served: 0,
            force_overwrite_warnings,
            cost_usd: 0.0,
            usage_complete: true,
            cancelled: start.is_break(),
            budget_exhausted: false,
            outcomes: Vec::new(),
            failures,
        };
        if report.cancelled {
            return Ok(report);
        }
        let budget_usd = policy.budget_usd;
        let policy_scope_id = analysis_policy_budget_scope_id(project_id, policy_id);
        let queue = Arc::new(Mutex::new(
            prepared
                .into_iter()
                .enumerate()
                .map(|(index, (asset, frames))| (index, asset, frames))
                .collect::<VecDeque<_>>(),
        ));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let default_allowed = vec![DEFAULT_MODEL.to_string(), FALLBACK_MODEL.to_string()];
        let allowed_models = if options.allowed_models.is_empty() {
            &default_allowed
        } else {
            &options.allowed_models
        };
        let chain = ProviderChain::with_policy(
            options.primary_model.as_deref().unwrap_or(DEFAULT_MODEL),
            allowed_models,
            options.allow_fallback.unwrap_or(true),
        )?;
        let approved_vocabulary = self.run_policy_vocabulary(policy)?;
        let (sender, receiver) = mpsc::channel();
        let mut completed = vec![false; task_work.len()];
        thread::scope(|scope| -> Result<()> {
            for _ in 0..worker_count {
                let catalog = self.clone();
                let queue = Arc::clone(&queue);
                let stop = Arc::clone(&stop);
                let sender = sender.clone();
                let worker_chain = chain.clone();
                let approved_vocabulary = &approved_vocabulary;
                scope.spawn(move || {
                    loop {
                        if stop.load(std::sync::atomic::Ordering::Relaxed) {
                            break;
                        }
                        let Some((index, asset, frames)) = queue
                            .lock()
                            .expect("VLM work queue lock poisoned")
                            .pop_front()
                        else {
                            break;
                        };
                        let started = Instant::now();
                        let include_transcript = options
                            .payload
                            .map(crate::AnalysisPayload::uses_transcript)
                            .unwrap_or(false);
                        let result = (|| -> Result<crate::AnalysisOutcome> {
                            let context =
                                crate::analysis::run_policy::bulk_request_accounting_context(
                                    policy,
                                    run_id,
                                    &asset.id,
                                    format!("{}:{}", policy.kind.as_str(), asset.id),
                                    false,
                                )?;
                            let accounted = AccountedVlmProvider::new(&catalog, context, provider);
                            catalog.analyze_asset_with_provider(
                                &asset.id,
                                &accounted,
                                &worker_chain,
                                include_transcript,
                                approved_vocabulary,
                                policy,
                                &mut || stop.load(std::sync::atomic::Ordering::Relaxed),
                            )
                        })();
                        if sender
                            .send((index, asset, frames, started.elapsed(), result))
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            }
            drop(sender);
            for (index, asset, frames, elapsed, result) in receiver {
                completed[index] = true;
                let remaining = task_work
                    .iter()
                    .enumerate()
                    .filter_map(|(index, weight)| (!completed[index]).then_some(*weight))
                    .collect::<Vec<_>>();
                // Captured before the failure arm moves the asset's path.
                let asset_id = Some(asset.id.clone());
                let flow = match result {
                    Ok(outcome) => {
                        estimator.record_completion(frames, elapsed);
                        report.analyzed += 1;
                        report.cost_usd += outcome.cost_usd;
                        report.usage_complete &= outcome.usage_complete;
                        if outcome.observation.is_none() {
                            report.rejected += 1;
                        }
                        if outcome.fell_back {
                            report.fallback_served += 1;
                        }
                        report.outcomes.push(outcome.clone());
                        on_progress(AnalyzeBatchProgress {
                            asset_id,
                            outcome: Some(outcome),
                            estimate: estimator.estimate(&remaining),
                        })
                    }
                    Err(error) => {
                        estimator.record_failure(frames);
                        let failure = ProviderFailure::from_anyhow(&error);
                        if failure.category == crate::ProviderFailureCategory::Cancelled {
                            report.cancelled = true;
                            stop.store(true, Ordering::Relaxed);
                        } else if failure.category
                            == crate::ProviderFailureCategory::LocalBudgetExhausted
                        {
                            report.budget_exhausted = true;
                            stop.store(true, Ordering::Relaxed);
                        } else if failure.category == crate::ProviderFailureCategory::LocalAdmission
                        {
                            // Canonical admission denied this transmission. Stop
                            // claiming new work; the final canonical snapshot
                            // decides whether the denial was budget exhaustion.
                            stop.store(true, Ordering::Relaxed);
                        }
                        report.failures.push(IngestFailure {
                            path: asset.path,
                            error: format!("{error:#}"),
                        });
                        on_progress(AnalyzeBatchProgress {
                            asset_id,
                            outcome: None,
                            estimate: estimator.estimate(&remaining),
                        })
                    }
                };
                if flow.is_break() {
                    report.cancelled = true;
                    stop.store(true, Ordering::Relaxed);
                }
                let budget_stop = self
                    .analysis_budget_snapshot(&policy_scope_id)?
                    .is_some_and(|snapshot| {
                        snapshot.dispatch_stopped || snapshot.available_usd <= 1e-12
                    });
                if budget_stop {
                    report.budget_exhausted = true;
                    stop.store(true, Ordering::Relaxed);
                }
            }
            Ok(())
        })?;
        // Use the canonical current-run projection for the report. Reported
        // provider cost remains in outcomes; this summary is confirmed ledger
        // cost and never invents a charge from a reservation or estimate.
        let projection =
            self.analysis_accounting_aggregate_for_run(policy_id, policy.kind.as_str(), run_id)?;
        report.cost_usd = projection.confirmed_cost_usd;
        report.usage_complete = !projection.has_unresolved_usage();
        if report.budget_exhausted {
            let aggregate = self.analysis_accounting_aggregate(policy_id, Some(run_id))?;
            report.failures.push(IngestFailure {
                path: project_id.to_string(),
                error: format!(
                    "analysis budget exhausted; confirmed ${:.6}, reserved ${:.6}, uncertain ${:.6}, available ${:.6} (whole-policy limit ${budget_usd:.2})",
                    aggregate.confirmed_cost_usd,
                    aggregate.active_reserved_cost_usd,
                    aggregate.uncertain_usage_reserve_usd,
                    (budget_usd - aggregate.committed_or_reserved_usd).max(0.0)
                ),
            });
        }
        self.save_batch_history(&task_key, estimator.history())?;
        Ok(report)
    }

    pub fn label_vocabulary(&self, project_id: &str) -> Result<Vec<LabelProposal>> {
        analysis::consolidate::vocabulary(&self.connection()?, project_id)
    }

    /// Applies an already validated label remap through typed label operations.
    /// Exposed separately so deterministic tests and future review UI can show
    /// the proposed mapping before accepting it.
    pub fn apply_label_mapping(
        &self,
        project_id: &str,
        mappings: &[LabelMapping],
    ) -> Result<ConsolidationReport> {
        let connection = self.connection()?;
        let proposals = label_proposals(&connection, project_id)?;
        let envelope = serde_json::json!({"mappings": mappings});
        parse_mapping(&envelope.to_string(), &proposals)?;
        let actions = mappings
            .iter()
            .map(|mapping| {
                Ok((
                    mapping.clone(),
                    assets_for_label(&connection, project_id, &mapping.from)?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        drop(connection);
        for (mapping, assets) in &actions {
            if mapping.from.eq_ignore_ascii_case(&mapping.to) {
                continue;
            }
            for asset_id in assets {
                self.remove_label(asset_id, &mapping.from)?;
                self.add_label(asset_id, &mapping.to)?;
            }
        }
        let (remapped_assets, vocabulary_size) =
            persist_label_mapping(&self.connection()?, project_id, mappings)?;
        Ok(ConsolidationReport {
            remapped_assets,
            vocabulary_size,
            mappings: mappings.to_vec(),
            model: String::new(),
            cost_usd: 0.0,
        })
    }

    /// Performs one text-only vocabulary consolidation request, then remaps
    /// only labels recorded as AI-authored.
    pub fn consolidate_labels(&self, project_id: &str) -> Result<ConsolidationReport> {
        let proposals = label_proposals(&self.connection()?, project_id)?;
        if proposals.is_empty() {
            return Ok(ConsolidationReport {
                remapped_assets: 0,
                vocabulary_size: 0,
                mappings: Vec::new(),
                model: String::new(),
                cost_usd: 0.0,
            });
        }
        let provider = OpenRouterProvider::new(resolve_api_key()?)?;
        let request = VlmRequest {
            prompt: consolidation_prompt(&proposals),
            frames: Vec::new(),
            schema: mapping_schema(proposals.len()),
        };
        let chain = ProviderChain::new();
        let first = chain.analyze(&provider, &request)?;
        let (mapping, response, total_cost) = match parse_mapping(&first.response.body, &proposals)
        {
            Ok(mapping) => (mapping, first.response.clone(), first.response.cost_usd),
            Err(default_error) if first.response.model == DEFAULT_MODEL => {
                chain.record_default_failure();
                let fallback = provider.analyze(FALLBACK_MODEL, &request)?;
                let mapping = parse_mapping(&fallback.body, &proposals).with_context(|| {
                    format!("default mapping was invalid ({default_error:#}); fallback was invalid")
                })?;
                let total_cost = first.response.cost_usd + fallback.cost_usd;
                (mapping, fallback, total_cost)
            }
            Err(error) => return Err(error).context("validate consolidation mapping"),
        };
        let mut report = self.apply_label_mapping(project_id, &mapping)?;
        report.model = response.model;
        report.cost_usd = total_cost;
        Ok(report)
    }

    pub fn generate_missing_proxies(&self, project_id: &str) -> Result<ProxyReport> {
        self.generate_missing_proxies_with_progress(project_id, |_| ControlFlow::Continue(()))
    }

    /// Generate missing proxies with an explicit encoder policy.
    pub fn generate_missing_proxies_with_policy(
        &self,
        project_id: &str,
        policy: ProxyEncoderPolicy,
    ) -> Result<ProxyReport> {
        self.generate_missing_proxies_with_policy_and_progress(project_id, policy, |_| {
            ControlFlow::Continue(())
        })
    }

    /// Generates every missing or stale proxy using the safe default policy.
    ///
    /// `on_progress` returning `ControlFlow::Break` stops the batch; workers
    /// finish the clip in flight and then stop claiming new work, so a
    /// cancelled run still commits whatever it completed.
    pub fn generate_missing_proxies_with_progress<F>(
        &self,
        project_id: &str,
        on_progress: F,
    ) -> Result<ProxyReport>
    where
        F: FnMut(AssetBatchProgress) -> ControlFlow<()>,
    {
        self.generate_missing_proxies_with_policy_and_progress(
            project_id,
            ProxyEncoderPolicy::Auto,
            on_progress,
        )
    }

    pub fn generate_missing_proxies_with_policy_and_progress<F>(
        &self,
        project_id: &str,
        policy: ProxyEncoderPolicy,
        on_progress: F,
    ) -> Result<ProxyReport>
    where
        F: FnMut(AssetBatchProgress) -> ControlFlow<()>,
    {
        self.generate_missing_proxies_with_resource_overrides_and_progress(
            project_id,
            policy,
            &ResourceOverrides::default(),
            on_progress,
        )
    }

    /// Proxy batch entrypoint used by CLI adapters that can pass explicit
    /// encoder/worker overrides. Calibration-selected worker counts are used
    /// only from a complete host/tool/recipe/configuration match.
    pub fn generate_missing_proxies_with_resource_overrides_and_progress<F>(
        &self,
        project_id: &str,
        policy: ProxyEncoderPolicy,
        overrides: &ResourceOverrides,
        mut on_progress: F,
    ) -> Result<ProxyReport>
    where
        F: FnMut(AssetBatchProgress) -> ControlFlow<()>,
    {
        let assets = self.list_assets(project_id)?;
        let mut ready = Vec::new();
        let mut pending = Vec::new();
        for asset in assets {
            if asset.removed
                || self.asset_video_validation(&asset.id)?.status
                    != AssetVideoValidationStatus::Usable
                || has_current_proxy(&asset)
                || (!asset.original_available && asset.proxy_available)
            {
                ready.push(asset);
            } else {
                pending.push(asset);
            }
        }
        let mut report = ProxyReport {
            generated: 0,
            skipped: ready.len(),
            cancelled: false,
            resource_source: String::new(),
            calibration_key: None,
            encoder: None,
            worker_count: 0,
            history_key: String::new(),
            sample_compatibility: "unknown".into(),
            failures: Vec::new(),
        };
        if let Some(requested) = overrides.worker_count
            && !pending.is_empty()
            && requested > pending.len()
        {
            bail!(
                "invalid input: workerCount {requested} exceeds the {} eligible proxy assets; the override cannot be honored without idling workers",
                pending.len()
            );
        }
        let task_work: Vec<u64> = pending.iter().map(|asset| asset.size_bytes).collect();
        let total_work = task_work.iter().copied().sum::<u64>();
        let selection = self.resolve_resource_selection(
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            Some(total_work),
            None,
            overrides,
        )?;
        let requested_workers = overrides
            .worker_count
            .unwrap_or_else(|| {
                if selection.source == "calibration" {
                    selection.worker_count
                } else {
                    PROXY_WORKERS
                }
            })
            .max(1);
        let worker_count = pending
            .len()
            .clamp(1, MAX_CALIBRATION_WORKERS.min(requested_workers));
        tracing::info!(
            target: "rcdvlog_core::proxy",
            worker_count,
            pending = task_work.len(),
            calibration_source = %selection.source,
            "generate_missing_proxies: begin"
        );
        let waiting_estimator =
            BatchEstimator::new(&task_work, worker_count, RateHistory::default());
        let _heavy = match self.wait_for_local_heavy(|| {
            let mut estimate = waiting_estimator.estimate(&task_work);
            estimate.confidence = "waiting_for_resource".into();
            estimate.estimated_remaining_seconds = None;
            on_progress(AssetBatchProgress {
                asset: None,
                estimate,
            })
            .is_break()
        })? {
            Some(lease) => lease,
            None => {
                report.cancelled = true;
                return Ok(report);
            }
        };
        let effective_policy = if overrides.encoder.as_deref() == Some("software")
            || (overrides.encoder.is_none()
                && selection.source == "calibration"
                && selection.encoder.as_deref() == Some("software"))
        {
            ProxyEncoderPolicy::Software
        } else {
            policy
        };
        let plan = resolve_proxy_encoder_plan(effective_policy);
        let planned_encoder = plan.preferred_encoder.as_str();
        let task_kind = proxy_task_kind_for_encoder(plan.preferred_encoder, worker_count);
        let history_key = self.qualified_history_key(
            &task_kind,
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            Some(planned_encoder),
            worker_count,
            0,
            0,
        )?;
        let mut history = self.load_batch_history(&history_key)?;
        if history.samples == 0
            && selection.source == "calibration"
            && selection.worker_count == worker_count
            && selection.encoder.as_deref() == Some(planned_encoder)
            && let Some(rate) = selection
                .timing
                .as_ref()
                .and_then(|timing| timing.work_units_per_second)
        {
            history = RateHistory {
                work_units_per_worker_second: rate / worker_count.max(1) as f64,
                samples: selection
                    .timing
                    .as_ref()
                    .map(|timing| timing.samples_ms.len() as u64)
                    .unwrap_or(0),
            };
        }
        report.resource_source = selection.source.clone();
        report.calibration_key = selection.calibration_key.clone();
        report.encoder = Some(planned_encoder.to_string());
        report.worker_count = worker_count;
        report.history_key = history_key.clone();
        report.sample_compatibility = selection.sample_compatibility.clone();
        let mut estimator = BatchEstimator::new(&task_work, worker_count, history);
        let ffmpeg = resolve_tool("ffmpeg");
        let ffprobe = resolve_tool("ffprobe");
        let start = on_progress(AssetBatchProgress {
            asset: None,
            estimate: estimator.estimate(&task_work),
        });
        if pending.is_empty() || start.is_break() {
            report.cancelled = start.is_break();
            return Ok(report);
        }

        let queue = Arc::new(Mutex::new(
            pending.into_iter().enumerate().collect::<VecDeque<_>>(),
        ));
        let stop = Arc::new(AtomicBool::new(false));
        let (outcome_sender, outcome_receiver) = mpsc::channel();
        let mut completed = vec![false; task_work.len()];
        let mut actual_encoders = HashSet::new();

        thread::scope(|scope| -> Result<()> {
            for _ in 0..worker_count {
                let catalog = self.clone();
                let queue = Arc::clone(&queue);
                let stop = Arc::clone(&stop);
                let outcome_sender = outcome_sender.clone();
                let plan = plan.clone();
                let ffmpeg = ffmpeg.clone();
                let ffprobe = ffprobe.clone();
                scope.spawn(move || {
                    loop {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let Some((index, asset)) = queue
                            .lock()
                            .expect("proxy work queue lock poisoned")
                            .pop_front()
                        else {
                            break;
                        };
                        let asset_id = asset.id.clone();
                        tracing::debug!(
                            target: "rcdvlog_core::proxy",
                            index,
                            asset = %asset_id,
                            "proxy worker: spawn ffmpeg"
                        );
                        let started = Instant::now();
                        let result = catalog.generate_proxy_untracked_with_execution(
                            &asset.id,
                            true,
                            policy,
                            |fraction| {
                                let _ = outcome_sender.send(ProxyWorkerMessage::Progress {
                                    index,
                                    completed_work_units: work_units_from_fraction(
                                        asset.size_bytes,
                                        fraction,
                                    ),
                                });
                            },
                            &ffmpeg,
                            &ffprobe,
                            Some(plan.clone()),
                        );
                        match &result {
                            Ok(_) => tracing::debug!(
                                target: "rcdvlog_core::proxy",
                                index,
                                asset = %asset_id,
                                elapsed_ms = started.elapsed().as_millis() as u64,
                                "proxy worker: ffmpeg done"
                            ),
                            Err(error) => tracing::warn!(
                                target: "rcdvlog_core::proxy",
                                index,
                                asset = %asset_id,
                                error = %format!("{error:#}"),
                                "proxy worker: ffmpeg failed"
                            ),
                        }
                        if outcome_sender
                            .send(ProxyWorkerMessage::Finished(Box::new(
                                ProxyWorkerFinished {
                                    index,
                                    asset,
                                    elapsed: started.elapsed(),
                                    result,
                                },
                            )))
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            }
            drop(outcome_sender);

            let mut partial_work = vec![0_u64; task_work.len()];
            for message in outcome_receiver {
                let updated = match message {
                    ProxyWorkerMessage::Progress {
                        index,
                        completed_work_units,
                    } => {
                        partial_work[index] = completed_work_units.min(task_work[index]);
                        None
                    }
                    ProxyWorkerMessage::Finished(finished) => {
                        let ProxyWorkerFinished {
                            index,
                            asset,
                            elapsed,
                            result,
                        } = *finished;
                        completed[index] = true;
                        partial_work[index] = 0;
                        match result {
                            Ok(generated) => {
                                report.generated += 1;
                                estimator.record_completion(asset.size_bytes, elapsed);
                                let actual_encoder =
                                    ProxyEncoderKind::parse(&generated.outcome.selected_encoder)
                                        .unwrap_or(ProxyEncoderKind::Software);
                                actual_encoders.insert(actual_encoder.as_str().to_string());
                                let actual_task_kind =
                                    proxy_task_kind_for_encoder(actual_encoder, worker_count);
                                let actual_history_key = self.qualified_history_key(
                                    &actual_task_kind,
                                    PROXY_REPRESENTATION_KIND,
                                    PROXY_SOFTWARE_RECIPE_KEY,
                                    Some(actual_encoder.as_str()),
                                    worker_count,
                                    0,
                                    0,
                                )?;
                                let mut history = self.load_batch_history(&actual_history_key)?;
                                history.observe(
                                    asset.size_bytes,
                                    generated.outcome.successful_elapsed(),
                                );
                                self.save_batch_history(&actual_history_key, history)?;
                                Some(generated.asset)
                            }
                            Err(error) if error.is::<RepresentationBusy>() => {
                                // Advance completion without training throughput.
                                estimator.record_failure(asset.size_bytes);
                                report.skipped += 1;
                                None
                            }
                            Err(error) => {
                                estimator.record_failure(asset.size_bytes);
                                report.failures.push(IngestFailure {
                                    path: asset.path,
                                    error: format!("{error:#}"),
                                });
                                None
                            }
                        }
                    }
                };
                let remaining_work = residual_work_units(&task_work, &completed, &partial_work);
                let partial_completed_work = partial_work.iter().copied().sum();
                let flow = on_progress(AssetBatchProgress {
                    asset: updated,
                    estimate: estimator
                        .estimate_with_partial_progress(&remaining_work, partial_completed_work),
                });
                if flow.is_break() {
                    report.cancelled = true;
                    stop.store(true, Ordering::Relaxed);
                }
            }
            Ok(())
        })?;

        report.encoder = match actual_encoders.len() {
            0 => Some(planned_encoder.to_string()),
            1 => {
                let actual_encoder = actual_encoders.iter().next().cloned();
                if let Some(actual_encoder) = actual_encoder.as_deref() {
                    let actual_kind = ProxyEncoderKind::parse(actual_encoder)
                        .unwrap_or(ProxyEncoderKind::Software);
                    let actual_base = proxy_task_kind_for_encoder(actual_kind, worker_count);
                    report.history_key = self.qualified_history_key(
                        &actual_base,
                        PROXY_REPRESENTATION_KIND,
                        PROXY_SOFTWARE_RECIPE_KEY,
                        Some(actual_encoder),
                        worker_count,
                        0,
                        0,
                    )?;
                }
                actual_encoder
            }
            _ => Some("mixed".into()),
        };
        report
            .failures
            .sort_by(|left, right| left.path.cmp(&right.path));
        Ok(report)
    }

    pub fn generate_thumbnail(&self, asset_id: &str, quality: u8) -> Result<Asset> {
        if quality != 1 {
            bail!("invalid thumbnail quality {quality}; MVP supports quality 1");
        }
        let claim = self.claim_representation(asset_id, RepresentationKind::Thumbnail)?;
        self.generate_thumbnail_with_claim(&claim, quality)
    }

    /// Execute an already-reserved thumbnail without reacquiring its claim.
    pub fn generate_thumbnail_with_claim(
        &self,
        claim: &RepresentationClaim,
        quality: u8,
    ) -> Result<Asset> {
        if quality != 1 {
            bail!("invalid thumbnail quality {quality}; MVP supports quality 1");
        }
        let asset_id = claim.asset_id();
        self.validate_representation_claim(claim, asset_id, RepresentationKind::Thumbnail)?;
        let asset = self.get_asset(asset_id)?;
        let input = if let Some(proxy) = asset
            .proxy_path
            .as_deref()
            .filter(|path| has_current_proxy(&asset) && Path::new(path).is_file())
        {
            PathBuf::from(proxy)
        } else {
            self.resolve_original(asset_id)?
        };
        let parent = self
            .database_path
            .parent()
            .ok_or_else(|| anyhow!("catalog has no parent directory"))?;
        let thumbnail_dir = parent.join("cache").join("thumbnails").join(&asset.id);
        fs::create_dir_all(&thumbnail_dir)?;
        let output = thumbnail_dir.join(format!("q{quality}.{THUMBNAIL_CACHE_VERSION}.jpg"));
        let temporary = thumbnail_dir.join(format!(
            "q{quality}.{THUMBNAIL_CACHE_VERSION}.{}.tmp.jpg",
            Uuid::new_v4().simple()
        ));
        let seek = (asset.duration_seconds.max(0.0) * 0.2).to_string();
        let mut command = tool_command("ffmpeg");
        command
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-ss",
                &seek,
                "-noautorotate",
                "-i",
            ])
            .arg(&input)
            .args([
                "-frames:v",
                "1",
                "-vf",
                "scale='min(480,iw)':-2:force_original_aspect_ratio=decrease",
                "-q:v",
                "3",
            ])
            .arg(&temporary);
        let (status, diagnostic) = run_captured_media_command(&mut command, &input, &temporary)
            .context("start ffmpeg thumbnail process")?;
        if !status.success() {
            let _ = fs::remove_file(&temporary);
            return Err(media_tool_failure(
                "ffmpeg thumbnail generation",
                status,
                diagnostic,
            ));
        }
        fs::rename(&temporary, &output)?;
        self.connection()?.execute(
            r#"
            INSERT INTO asset_thumbnails (asset_id, quality, path, created_at)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(asset_id, quality)
            DO UPDATE SET path = excluded.path, created_at = excluded.created_at
            "#,
            params![asset_id, quality, output.to_string_lossy(), now()],
        )?;
        self.get_asset(asset_id)
    }

    pub fn generate_missing_thumbnails(
        &self,
        project_id: &str,
        quality: u8,
    ) -> Result<ThumbnailReport> {
        self.generate_missing_thumbnails_with_progress(project_id, quality, |_| {})
    }

    pub fn generate_missing_thumbnails_with_progress<F>(
        &self,
        project_id: &str,
        quality: u8,
        on_progress: F,
    ) -> Result<ThumbnailReport>
    where
        F: FnMut(AssetBatchProgress),
    {
        let asset_ids = self
            .list_assets(project_id)?
            .into_iter()
            .map(|asset| asset.id)
            .collect::<Vec<_>>();
        self.generate_thumbnails_for_assets_with_progress(&asset_ids, quality, on_progress)
    }

    pub fn generate_thumbnails_for_assets(
        &self,
        asset_ids: &[String],
        quality: u8,
    ) -> Result<ThumbnailReport> {
        self.generate_thumbnails_for_assets_with_progress(asset_ids, quality, |_| {})
    }

    /// Generates a thumbnail batch for exactly the supplied assets. Duplicate
    /// ids are coalesced and an existing current-version Q1 is left untouched.
    pub fn generate_thumbnails_for_assets_with_progress<F>(
        &self,
        asset_ids: &[String],
        quality: u8,
        on_progress: F,
    ) -> Result<ThumbnailReport>
    where
        F: FnMut(AssetBatchProgress),
    {
        self.generate_thumbnail_batch(asset_ids, quality, None, on_progress)
    }

    /// Batch reserved before entering a desktop execution queue. Claims remain
    /// held for the whole batch, including validation and catalog commits.
    pub fn generate_thumbnails_with_claims_with_progress<F>(
        &self,
        claims: &[RepresentationClaim],
        quality: u8,
        on_progress: F,
    ) -> Result<ThumbnailReport>
    where
        F: FnMut(AssetBatchProgress),
    {
        let ids = claims
            .iter()
            .map(|claim| claim.asset_id().to_owned())
            .collect::<Vec<_>>();
        self.generate_thumbnail_batch(&ids, quality, Some(claims), on_progress)
    }

    fn generate_thumbnail_batch<F>(
        &self,
        asset_ids: &[String],
        quality: u8,
        claims: Option<&[RepresentationClaim]>,
        mut on_progress: F,
    ) -> Result<ThumbnailReport>
    where
        F: FnMut(AssetBatchProgress),
    {
        if quality != 1 {
            bail!("invalid thumbnail quality {quality}; MVP supports quality 1");
        }
        if let Some(claims) = claims {
            for claim in claims {
                self.validate_representation_claim(
                    claim,
                    claim.asset_id(),
                    RepresentationKind::Thumbnail,
                )?;
            }
        }
        let mut seen = HashSet::new();
        let mut pending = Vec::new();
        let mut skipped = 0;
        let mut failures = Vec::new();
        for asset_id in asset_ids {
            if !seen.insert(asset_id.clone()) {
                continue;
            }
            match self.get_asset(asset_id) {
                Ok(asset) if self.has_current_thumbnail(&asset.id, quality)? => skipped += 1,
                Ok(asset) => pending.push(asset),
                Err(error) => failures.push(IngestFailure {
                    path: asset_id.clone(),
                    error: format!("{error:#}"),
                }),
            }
        }
        let mut report = ThumbnailReport {
            quality,
            generated: 0,
            skipped,
            failures,
        };
        let task_work: Vec<u64> = pending.iter().map(|asset| asset.size_bytes).collect();
        let mut estimator = BatchEstimator::new(
            &task_work,
            THUMBNAIL_WORKERS,
            self.load_batch_history(THUMBNAIL_TASK_KIND)?,
        );
        on_progress(AssetBatchProgress {
            asset: None,
            estimate: estimator.estimate(&task_work),
        });
        for (index, asset) in pending.into_iter().enumerate() {
            let started = Instant::now();
            let result = if let Some(claims) = claims {
                let claim = claims
                    .iter()
                    .find(|claim| claim.asset_id() == asset.id)
                    .ok_or_else(|| anyhow!("missing thumbnail claim"))?;
                self.generate_thumbnail_with_claim(claim, quality)
            } else {
                self.generate_thumbnail(&asset.id, quality)
            };
            let updated = match result {
                Ok(updated) => {
                    report.generated += 1;
                    estimator.record_completion(asset.size_bytes, started.elapsed());
                    self.save_batch_history(THUMBNAIL_TASK_KIND, estimator.history())?;
                    Some(updated)
                }
                Err(error) if error.is::<RepresentationBusy>() => {
                    estimator.record_failure(asset.size_bytes);
                    report.skipped += 1;
                    None
                }
                Err(error) => {
                    estimator.record_failure(asset.size_bytes);
                    report.failures.push(IngestFailure {
                        path: asset.path,
                        error: format!("{error:#}"),
                    });
                    None
                }
            };
            on_progress(AssetBatchProgress {
                asset: updated,
                estimate: estimator.estimate(&task_work[index + 1..]),
            });
        }
        Ok(report)
    }

    fn has_current_thumbnail(&self, asset_id: &str, quality: u8) -> Result<bool> {
        let path = self
            .connection()?
            .query_row(
                "SELECT path FROM asset_thumbnails WHERE asset_id = ?1 AND quality = ?2",
                params![asset_id, quality],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(path.is_some_and(|path| {
            Path::new(&path).is_file()
                && path.ends_with(&format!("q{quality}.{THUMBNAIL_CACHE_VERSION}.jpg"))
        }))
    }

    fn require_project(&self, project_id: &str) -> Result<()> {
        let exists: bool = self.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
            [project_id],
            |row| row.get(0),
        )?;
        if !exists {
            bail!("project not found: {project_id}");
        }
        Ok(())
    }

    /// Loads the complete persisted chronology evidence for one project
    /// (D01-T007). Every collection is returned in deterministic order, sorted
    /// by primary key, so the solver cannot inherit nondeterminism from the
    /// loader. The facts are evidence only: derived values belong to the
    /// solver (D01-T008).
    pub fn chronology_facts(&self, project_id: &str) -> Result<ChronologyFacts> {
        self.require_project(project_id)?;
        let connection = self.connection()?;
        chronology::load_chronology_facts(&connection, project_id)
    }

    /// Solves this project's chronology (D01-T008). Loading is I/O; solving is
    /// pure, so identical persisted facts always produce an identical solution.
    pub fn solve_chronology(&self, project_id: &str) -> Result<ChronologySolution> {
        Ok(solve(&self.chronology_facts(project_id)?))
    }

    /// Read-only, revision-pinned page of chronology facts and solved evidence.
    pub fn chronology_evidence_page(
        &self,
        project_id: &str,
        expected_revision: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<ChronologyEvidencePage> {
        chronology::evidence_page(
            self.chronology_facts(project_id)?,
            expected_revision,
            offset,
            limit,
        )
    }

    /// Makes one read-only, proxy-only cross-clip evidence request. Results are
    /// suggestions and provenance; this method has no path to chronology writes.
    pub fn suggest_chronology_with_provider(
        &self,
        project_id: &str,
        asset_ids: &[String],
        provider: &dyn VlmProvider,
        chain: &ProviderChain,
    ) -> Result<SuggestionResult> {
        anyhow::ensure!((2..=8).contains(&asset_ids.len()), "select 2..=8 clips");
        anyhow::ensure!(
            asset_ids.iter().collect::<HashSet<_>>().len() == asset_ids.len(),
            "each clip may be selected only once"
        );
        let facts = self.chronology_facts(project_id)?;
        let facts_revision = blake3::hash(&serde_json::to_vec(&facts)?)
            .to_hex()
            .to_string();
        let mut frames = Vec::new();
        for asset_id in asset_ids {
            let asset = self.get_asset(asset_id)?;
            anyhow::ensure!(
                asset.project_id == project_id && !asset.removed,
                "clip is not an active member of this project"
            );
            let proxy = Self::require_proxy(&asset)?;
            let manifest = self.analysis_frame_pack(&asset)?;
            let parent = self
                .database_path
                .parent()
                .ok_or_else(|| anyhow!("catalog has no parent directory"))?;
            let directory = parent
                .join("cache")
                .join("framepacks")
                .join(project_id)
                .join(asset_id)
                .join(&manifest.source_fingerprint);
            let paths = encode_frames(&proxy, &manifest, &directory)?;
            anyhow::ensure!(
                !paths.is_empty() && paths.len() == manifest.timestamps_ms.len(),
                "proxy frame pack is empty or incomplete for {asset_id}"
            );
            // Two deterministic samples per selected clip keep cross-clip requests small.
            let indexes = if paths.len() == 1 {
                vec![0]
            } else {
                vec![0, paths.len() - 1]
            };
            for index in indexes {
                let timestamp_ms = manifest.timestamps_ms[index];
                let bytes = fs::read(&paths[index])
                    .with_context(|| format!("read proxy frame {}", paths[index].display()))?;
                frames.push(EvidenceFrame {
                    id: format!("{asset_id}@{timestamp_ms}"),
                    asset_id: asset_id.clone(),
                    timestamp_ms,
                    proxy_data_uri: frame_data_uri(&bytes),
                });
            }
        }
        let required_frame_ids = frames.iter().map(|frame| frame.id.clone()).collect();
        let request = ChronologyEvidenceRequest {
            facts_revision,
            frames,
            required_frame_ids,
        };
        let request_key = blake3::hash(&serde_json::to_vec(&(
            &request,
            chain.current_default(),
            chain.fallback_model(),
        ))?)
        .to_hex()
        .to_string();
        let connection = self.connection()?;
        if let Some(result) =
            analysis::chronology_suggestions::load_cached(&connection, &request_key)?
        {
            return Ok(result);
        }
        anyhow::ensure!(
            analysis::chronology_suggestions::reserve_request(
                &connection,
                &request_key,
                project_id,
                &request.facts_revision
            )?,
            "an identical chronology suggestion request is already running or was interrupted; refusing a duplicate provider charge"
        );
        drop(connection);
        let mut result = request_suggestion(provider, chain, &request)?;
        result.review = review_suggestion_against_facts(&request, &result.suggestion, &facts);
        analysis::chronology_suggestions::store_result(&self.connection()?, &request_key, &result)?;
        Ok(result)
    }

    /// Writes the solution's derived times back into the `assets` projection
    /// columns.
    ///
    /// This is the whole-project route through the solver-owned writer of
    /// `assets.best_time` and `assets.offset_seconds` (`CHRONOLOGY_MODEL.md`,
    /// "Evidence only"). Asset-add materialization uses that same writer's
    /// bounded single-asset route; no adapter or parallel chronology rule may
    /// write them. They exist for query performance and are reproducible by
    /// re-solving.
    pub fn project_chronology_solution(
        &self,
        project_id: &str,
        solution: &ChronologySolution,
    ) -> Result<usize> {
        self.require_project(project_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let written =
            Self::project_chronology_solution_on_connection(&transaction, project_id, solution)?;
        transaction.commit()?;
        Ok(written)
    }

    fn project_chronology_solution_on_connection(
        connection: &Connection,
        project_id: &str,
        solution: &ChronologySolution,
    ) -> Result<usize> {
        let mut written = 0usize;
        for asset in &solution.assets {
            written += Self::project_solved_asset_on_connection(connection, project_id, asset)?;
        }
        Ok(written)
    }

    /// The one writer for the derived `assets.best_time`/`offset_seconds`
    /// projection. Both a whole chronology solve and a just-committed import
    /// route through this helper so epoch and exception semantics cannot fork.
    fn project_solved_asset_on_connection(
        connection: &Connection,
        project_id: &str,
        asset: &chronology::AssetChronology,
    ) -> Result<usize> {
        // The offset is the derived shift from the camera's own claim, so an
        // exception correctly replaces an epoch instead of merely displaying
        // over it.
        let offset_seconds = match (asset.raw_time.as_deref(), asset.trip_time.as_deref()) {
            (Some(raw), Some(trip)) => {
                match (chronology::parse_utc(raw), chronology::parse_utc(trip)) {
                    (Some(from), Some(to)) => (to - from).num_seconds(),
                    _ => 0,
                }
            }
            _ => 0,
        };
        Ok(connection.execute(
            "UPDATE assets SET best_time = ?1, offset_seconds = ?2 WHERE id = ?3 AND project_id = ?4",
            params![asset.trip_time, offset_seconds, asset.asset_id, project_id],
        )?)
    }

    /// Projects the solver's exact local result for one newly materialized
    /// asset. This is bounded to its sequence epochs and own exception rather
    /// than re-solving every row of a large import.
    pub(crate) fn project_new_asset_chronology_on_connection(
        connection: &Connection,
        project_id: &str,
        asset_id: &str,
        capture_sequence_id: &str,
        filename_counter: Option<u64>,
        filename: &str,
        captured_at_raw: Option<&str>,
    ) -> Result<()> {
        let solved = chronology::projected_asset_solution(
            connection,
            project_id,
            asset_id,
            capture_sequence_id,
            filename_counter,
            filename,
            captured_at_raw,
        )?;
        Self::project_solved_asset_on_connection(connection, project_id, &solved)?;
        Ok(())
    }

    pub(crate) fn project_current_chronology_in_transaction(
        transaction: &rusqlite::Transaction<'_>,
        project_id: &str,
    ) -> Result<ChronologySolution> {
        let facts = chronology::load_chronology_facts(transaction, project_id)?;
        let solution = solve(&facts);
        Self::project_chronology_solution_on_connection(transaction, project_id, &solution)?;
        Ok(solution)
    }

    /// Solves and projects under one immediate transaction, the ordinary path
    /// after any chronology change. Acquiring the write reservation before
    /// loading facts prevents an older solve from projecting after a newer
    /// chronology operation commits on another connection.
    pub fn resolve_chronology(&self, project_id: &str) -> Result<ChronologySolution> {
        self.resolve_chronology_with_hook(project_id, || {})
    }

    fn resolve_chronology_with_hook<F>(
        &self,
        project_id: &str,
        after_solve: F,
    ) -> Result<ChronologySolution>
    where
        F: FnOnce(),
    {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
            [project_id],
            |row| row.get(0),
        )?;
        if !exists {
            bail!("project not found: {project_id}");
        }
        let facts = chronology::load_chronology_facts(&transaction, project_id)?;
        let solution = solve(&facts);
        after_solve();
        Self::project_chronology_solution_on_connection(&transaction, project_id, &solution)?;
        transaction.commit()?;
        Ok(solution)
    }

    /// Reports the operations this build retained but could not materialize,
    /// because their operation type is unknown to this build (D01-T007
    /// quarantine policy). The operations are durable in `project_operations`,
    /// never advanced an aggregate revision, and apply normally once a build
    /// that knows the type opens the catalog.
    pub fn quarantined_operations(&self, project_id: &str) -> Result<Vec<QuarantinedOperation>> {
        self.require_project(project_id)?;
        let connection = self.connection()?;
        chronology::load_quarantined_operations(&connection, project_id)
    }

    /// Persist both successful and failed proxy-generation evidence. A failed
    /// row never mutates `assets.proxy_path`; the previously registered output
    /// remains canonical.
    fn persist_proxy_generation_provenance(
        &self,
        asset_id: &str,
        outcome: &ProxyGenerationOutcome,
    ) -> Result<()> {
        insert_proxy_generation_provenance(&self.connection()?, asset_id, outcome)
    }

    /// Latest-first proxy generation provenance for one asset. Attempts and
    /// validation are decoded from the canonical SQLite JSON columns.
    pub fn proxy_generation_provenance(
        &self,
        asset_id: &str,
        limit: usize,
    ) -> Result<Vec<ProxyGenerationProvenance>> {
        self.load_proxy_generation_provenance(asset_id, limit.clamp(1, 1_000) as i64, None)
    }

    fn load_proxy_generation_provenance(
        &self,
        asset_id: &str,
        limit: i64,
        id: Option<i64>,
    ) -> Result<Vec<ProxyGenerationProvenance>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT id, asset_id, recipe_key, policy, selected_encoder, device,
                   fallback_used, status, elapsed_ms, attempts_json,
                   validation_json, created_at
            FROM proxy_generation_provenance
            WHERE asset_id = ?1 AND (?3 IS NULL OR id = ?3)
            ORDER BY id DESC
            LIMIT ?2
            "#,
        )?;
        let rows = statement.query_map(params![asset_id, limit, id], |row| {
            let policy_value: String = row.get(3)?;
            let attempts_json: String = row.get(9)?;
            let validation_json: Option<String> = row.get(10)?;
            Ok(ProxyGenerationProvenance {
                id: row.get(0)?,
                asset_id: row.get(1)?,
                recipe_key: row.get(2)?,
                policy: ProxyEncoderPolicy::parse(&policy_value).map_err(|_| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        format!("invalid proxy encoder policy {policy_value}").into(),
                    )
                })?,
                selected_encoder: row.get(4)?,
                device: row.get(5)?,
                fallback_used: row.get::<_, i64>(6)? != 0,
                status: row.get(7)?,
                elapsed_ms: row.get::<_, i64>(8)? as u64,
                attempts: serde_json::from_str(&attempts_json).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        9,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                validation: validation_json
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()
                    .map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            10,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                created_at: row.get(11)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Provenance attached to the currently registered generated proxy. A later
    /// failed attempt never replaces this association; downloads clear it.
    pub fn registered_proxy_generation_provenance(
        &self,
        asset_id: &str,
    ) -> Result<Option<ProxyGenerationProvenance>> {
        let id: Option<i64> = self.connection()?.query_row(
            "SELECT proxy_generation_id FROM assets WHERE id = ?1",
            [asset_id],
            |row| row.get(0),
        )?;
        let Some(id) = id else {
            return Ok(None);
        };
        Ok(self
            .load_proxy_generation_provenance(asset_id, 1, Some(id))?
            .pop())
    }

    /// Encoder- and worker-count-aware proxy rate histories in this catalog.
    pub fn proxy_rate_histories(&self) -> Result<Vec<ProxyRateHistoryEntry>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT task_kind, work_units_per_worker_second, samples, updated_at
            FROM batch_performance
            WHERE task_kind LIKE 'proxy.h264-720p.orientation-v3%'
            ORDER BY task_kind
            "#,
        )?;
        let rows = statement.query_map([], |row| {
            let task_kind: String = row.get(0)?;
            let encoder = if task_kind.contains("videotoolbox") {
                "videotoolbox"
            } else if task_kind.contains("vaapi") {
                "vaapi"
            } else {
                "software"
            };
            let worker_count = task_kind
                .rsplit_once("-w")
                .and_then(|(_, suffix)| suffix.split('-').next())
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(1);
            Ok(ProxyRateHistoryEntry {
                task_kind,
                encoder: encoder.to_string(),
                worker_count,
                work_units_per_worker_second: row.get(1)?,
                samples: row.get::<_, i64>(2)? as u64,
                updated_at: row.get(3)?,
                scope: "Observed only on this catalog's host with its successful hardware/software mix and worker count; not a cross-corpus benchmark.".to_string(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn load_batch_history(&self, task_kind: &str) -> Result<RateHistory> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT work_units_per_worker_second, samples FROM batch_performance WHERE task_kind = ?1",
                [task_kind],
                |row| {
                    Ok(RateHistory {
                        work_units_per_worker_second: row.get(0)?,
                        samples: row.get::<_, i64>(1)? as u64,
                    })
                },
            )
            .optional()?
            .unwrap_or_default())
    }

    fn save_batch_history(&self, task_kind: &str, history: RateHistory) -> Result<()> {
        self.connection()?.execute(
            r#"
            INSERT INTO batch_performance (
                task_kind, work_units_per_worker_second, samples, updated_at
            ) VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(task_kind) DO UPDATE SET
                work_units_per_worker_second = excluded.work_units_per_worker_second,
                samples = excluded.samples,
                updated_at = excluded.updated_at
            "#,
            params![
                task_kind,
                history.work_units_per_worker_second,
                history.samples as i64,
                now()
            ],
        )?;
        Ok(())
    }
}

fn camera_sidecar_fingerprint(path: &Path) -> Result<String> {
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

fn camera_sidecar_has_aac(path: &Path) -> Result<bool> {
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

fn verify_camera_sidecar_decode(path: &Path) -> Result<()> {
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

fn asset_column_exists(connection: &Connection, column: &str) -> Result<bool> {
    table_column_exists(connection, "assets", column)
}

fn table_column_exists(connection: &Connection, table: &str, column: &str) -> Result<bool> {
    Ok(connection
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == column))
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )?)
}

fn camera_proxy_asset_columns(connection: &Connection) -> Result<String> {
    if table_exists(connection, "camera_proxy_sidecars")? {
        Ok(
            "(SELECT path FROM camera_proxy_sidecars cps WHERE cps.asset_id = a.id), \
            (SELECT byte_length FROM camera_proxy_sidecars cps WHERE cps.asset_id = a.id)"
                .into(),
        )
    } else {
        Ok("NULL, NULL".into())
    }
}

/// Select expressions and the join that carry the verification state of the
/// location `assets.path` currently points at.
///
/// A catalog old enough to predate verification returns literals and no join, so
/// every asset query keeps working against a legacy schema exactly as the
/// `asset_column_expression` guards already do elsewhere in this file.
fn location_columns(
    connection: &Connection,
) -> Result<(&'static str, &'static str, &'static str, &'static str)> {
    if table_column_exists(
        connection,
        "project_asset_local_locations",
        "verification_state",
    )? {
        Ok((
            "l.verification_state",
            "l.verified_size_bytes",
            "l.verified_mtime_millis",
            "LEFT JOIN project_asset_local_locations l \
             ON l.asset_id = a.id AND l.local_path = a.path \
             AND l.node_id = (SELECT node_id FROM catalog_identity WHERE singleton_id = 1)",
        ))
    } else {
        Ok(("NULL", "NULL", "NULL", ""))
    }
}

fn asset_column_expression(
    connection: &Connection,
    column: &str,
    present_expression: &'static str,
    missing_expression: &'static str,
) -> Result<&'static str> {
    Ok(if asset_column_exists(connection, column)? {
        present_expression
    } else {
        missing_expression
    })
}

fn table_column_expression(
    connection: &Connection,
    table: &str,
    column: &str,
    present_expression: &'static str,
    missing_expression: &'static str,
) -> Result<&'static str> {
    Ok(if table_column_exists(connection, table, column)? {
        present_expression
    } else {
        missing_expression
    })
}

fn has_current_proxy(asset: &Asset) -> bool {
    asset.proxy_available && asset.proxy_representation == Some(ProxyRepresentation::RawV3)
}

pub fn proxy_representation(path: impl AsRef<Path>) -> ProxyRepresentation {
    let path = path.as_ref().to_string_lossy();
    if path.ends_with(".v3.mp4") {
        ProxyRepresentation::RawV3
    } else if path.ends_with(".v2.mp4") {
        ProxyRepresentation::MetadataTaggedRawV2
    } else {
        ProxyRepresentation::MetadataBakedV1
    }
}

fn normalize_rotation(degrees: f64) -> u16 {
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

pub(crate) fn insert_proxy_generation_provenance(
    connection: &Connection,
    asset_id: &str,
    outcome: &ProxyGenerationOutcome,
) -> Result<()> {
    let attempts_json =
        serde_json::to_string(&outcome.attempts).context("serialize proxy encoder attempts")?;
    let validation_json = outcome
        .validation
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .context("serialize proxy output validation")?;
    connection.execute(
        r#"
        INSERT INTO proxy_generation_provenance (
            asset_id, recipe_key, policy, selected_encoder, device,
            fallback_used, status, elapsed_ms, attempts_json,
            validation_json, created_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
        "#,
        params![
            asset_id,
            outcome.recipe_key,
            outcome.policy.as_str(),
            outcome.selected_encoder,
            outcome.device,
            i64::from(outcome.fallback_used),
            outcome.status,
            outcome.elapsed_ms.min(i64::MAX as u64) as i64,
            attempts_json,
            validation_json,
            now(),
        ],
    )?;
    Ok(())
}

fn row_to_asset(row: &rusqlite::Row<'_>) -> rusqlite::Result<Asset> {
    let path: String = row.get(2)?;
    let proxy_path: Option<String> = row.get(15)?;
    // Advisory availability: cheap enough to run for every row in a list, which
    // rules out hashing. A location proven to hold the wrong bytes is excluded
    // outright, and one proven correct is trusted only while its size and mtime
    // still match. Anything that reads original bytes goes through
    // `Catalog::resolve_original` instead, which hashes.
    let location_state = row
        .get::<_, Option<String>>(29)?
        .as_deref()
        .and_then(LocationVerificationState::parse_stored);
    let location_guard =
        row.get::<_, Option<i64>>(30)?
            .map(|size_bytes| locations::LocationGuard {
                size_bytes,
                mtime_millis: row.get::<_, Option<i64>>(31).unwrap_or(None),
            });
    let original_available =
        locations::location_is_available(&path, location_state, location_guard);
    let proxy_available = proxy_path
        .as_deref()
        .is_some_and(|path| Path::new(path).is_file());
    let camera_proxy_path: Option<String> = row.get(33)?;
    let camera_proxy_size: Option<i64> = row.get(34)?;
    let camera_proxy_available = camera_proxy_path
        .as_deref()
        .zip(camera_proxy_size)
        .is_some_and(|(path, size)| {
            fs::metadata(path)
                .map(|metadata| metadata.is_file() && metadata.len() == size as u64)
                .unwrap_or(false)
        });
    let stored_thumbnail_path: Option<String> = row.get(20)?;
    let stored_thumbnail_quality = row.get::<_, i64>(21)? as u8;
    let thumbnail_available = stored_thumbnail_path
        .as_deref()
        .is_some_and(|path| Path::new(path).is_file());
    let available = original_available || proxy_available || camera_proxy_available;
    let media_verification_state = match row.get::<_, String>(26)?.as_str() {
        "unlinked" => MediaVerificationState::Unlinked,
        "provisional" => MediaVerificationState::Provisional,
        "verified" => MediaVerificationState::Verified,
        "mismatch" => MediaVerificationState::Mismatch,
        value => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                26,
                rusqlite::types::Type::Text,
                format!("invalid media verification state: {value}").into(),
            ));
        }
    };
    Ok(Asset {
        id: row.get(0)?,
        project_id: row.get(1)?,
        path,
        filename: row.get(3)?,
        fingerprint: row.get(4)?,
        size_bytes: row.get::<_, i64>(5)? as u64,
        duration_seconds: row.get(6)?,
        frame_rate: row.get(7)?,
        width: row.get::<_, i64>(8)? as u32,
        height: row.get::<_, i64>(9)? as u32,
        codec: row.get(10)?,
        camera: row.get(11)?,
        captured_at_raw: row.get(12)?,
        best_time: row.get(13)?,
        offset_seconds: row.get(14)?,
        proxy_representation: proxy_path.as_deref().map(proxy_representation),
        proxy_path,
        camera_proxy_available,
        metadata_rotation_degrees: row.get::<_, i64>(16)? as u16,
        default_rotation_degrees: row.get::<_, i64>(17)? as u16,
        rotation_degrees: row.get::<_, i64>(18)? as u16,
        orientation_probed: row.get::<_, i64>(19)? != 0,
        original_available,
        proxy_available,
        thumbnail_path: thumbnail_available
            .then_some(stored_thumbnail_path)
            .flatten(),
        thumbnail_quality: if thumbnail_available {
            stored_thumbnail_quality
        } else {
            0
        },
        notes: row.get(22)?,
        title: row.get(27)?,
        description: row.get(28)?,
        labels: serde_json::from_str(&row.get::<_, String>(23)?).unwrap_or_default(),
        removed: row.get::<_, i64>(24)? != 0,
        media_object_id: row.get(25)?,
        media_verification_state,
        available,
        revision: row.get::<_, i64>(32)? as u64,
    })
}

/// The asset's operation revision, and the join that supplies it.
///
/// One `LEFT JOIN` on a primary key rather than a query per asset: `list_assets`
/// runs this for every clip in a project, and the target is thousands.
/// A catalog predating the operations log has no such table and reports 0,
/// which is the same revision `reconcile_asset_baseline` gives an asset that
/// has never been written through an operation.
fn revision_columns(connection: &rusqlite::Connection) -> Result<(&'static str, &'static str)> {
    Ok(if table_exists(connection, "materialized_revisions")? {
        (
            "COALESCE(r.revision, 0)",
            "LEFT JOIN materialized_revisions r ON r.project_id = a.project_id AND r.aggregate_type = 'projectAsset' AND r.aggregate_id = a.id",
        )
    } else {
        ("0", "")
    })
}

/// Returns true when the path has a supported media extension (case-insensitive).
/// The extension set is the same one used for desktop ingest and server scanning.
pub fn is_supported_media_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| MEDIA_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn is_media(path: &Path) -> bool {
    is_supported_media_path(path)
}

pub(crate) fn tool_command(name: &str) -> Command {
    Command::new(resolve_tool(name))
}

#[cfg(all(test, target_os = "macos"))]
fn proxy_ffmpeg_command(source: &Path, output: &Path, encoder: ProxyEncoder) -> Command {
    let target = ProxyEncoderTarget {
        encoder,
        device: None,
        recipe_key: encoder.recipe_key().to_string(),
    };
    proxy_encoding::proxy_ffmpeg_command_with_program(
        &resolve_tool("ffmpeg"),
        source,
        output,
        &target,
    )
}

fn drain_child_stderr(mut stderr: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
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
    command.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = command.spawn().context("spawn media tool process")?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("media tool stderr pipe was not available"))?;
    let stderr_reader = drain_child_stderr(stderr);
    let status = child.wait().context("wait for media tool process")?;
    let stderr = stderr_reader.join().unwrap_or_default();
    let stderr = String::from_utf8_lossy(&stderr).into_owned();
    let diagnostic = redact_media_diagnostic(&stderr, input, output);
    Ok((status, diagnostic))
}

fn redact_media_diagnostic(stderr: &str, source: &Path, output: &Path) -> String {
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

fn ffmpeg_progress_fraction(line: &str, duration_seconds: f64) -> Option<f64> {
    if !duration_seconds.is_finite() || duration_seconds <= 0.0 {
        return None;
    }
    let output_microseconds = line.strip_prefix("out_time_us=")?.parse::<u64>().ok()?;
    let fraction = output_microseconds as f64 / 1_000_000.0 / duration_seconds;
    fraction.is_finite().then_some(fraction.clamp(0.0, 1.0))
}

fn work_units_from_fraction(work_units: u64, fraction: f64) -> u64 {
    if !fraction.is_finite() || fraction <= 0.0 {
        return 0;
    }
    (work_units as f64 * fraction.clamp(0.0, 1.0))
        .round()
        .min(work_units as f64) as u64
}

fn residual_work_units(task_work: &[u64], completed: &[bool], partial_work: &[u64]) -> Vec<u64> {
    task_work
        .iter()
        .enumerate()
        .filter_map(|(index, weight)| {
            (!completed[index]).then_some(weight.saturating_sub(partial_work[index]))
        })
        .collect()
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

fn resolve_tool(name: &str) -> PathBuf {
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

fn sampled_fingerprint(path: &Path, size: u64) -> Result<String> {
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
fn probe_media(path: &Path) -> Result<MediaProbe> {
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

fn collect_timestamp_candidates(
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

fn timestamp_tag_semantics(tag: &str, creation_semantics: &str) -> Option<String> {
    match tag {
        // These are explicit semantic tag names. Generic creation_time tags do
        // not establish whether a recording started or ended at that instant.
        "recording_start_time" => Some("recording_start".into()),
        "recording_end_time" => Some("recording_end".into()),
        "creation_time" => Some(creation_semantics.into()),
        _ => None,
    }
}

fn timestamp_timezone_claim(raw: &str) -> Option<String> {
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

fn find_compact_filename_timestamp(filename: &str) -> Option<(usize, usize)> {
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

fn require_video_stream(document: &Value) -> Result<&Value> {
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

fn video_duration(document: &Value, stream: &Value) -> Option<f64> {
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
    tool: &'static str,
    status: String,
    diagnostic: String,
}

fn media_tool_failure(
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
fn probe_media_document_with(path: &Path, ffprobe_program: &Path) -> Result<Value> {
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
fn exif_camera_with(path: &Path, exiftool_program: &Path) -> Option<(String, String)> {
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
fn modified_nanos(metadata: &std::fs::Metadata) -> Option<i128> {
    use std::os::unix::fs::MetadataExt;
    let secs = metadata.mtime() as i128;
    let nsecs = metadata.mtime_nsec() as i128;
    Some(secs * 1_000_000_000 + nsecs)
}

#[cfg(not(unix))]
fn modified_nanos(_metadata: &std::fs::Metadata) -> Option<i128> {
    None
}

/// Returns the raw ffprobe JSON document for a media file.
fn probe_media_document(path: &Path) -> Result<Value> {
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

fn stream_rotation(stream: Option<&Value>) -> u16 {
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

fn orientation_name(width: u32, height: u32) -> &'static str {
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

fn parse_rate(value: &str) -> Option<f64> {
    let (numerator, denominator) = value.split_once('/').unwrap_or((value, "1"));
    let numerator = numerator.parse::<f64>().ok()?;
    let denominator = denominator.parse::<f64>().ok()?;
    if denominator == 0.0 {
        return None;
    }
    let rate = numerator / denominator;
    (rate.is_finite() && rate > 0.0).then_some(rate)
}

fn normalize_datetime(value: &str, offset_seconds: i64) -> Option<String> {
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

fn normalize_labels(labels: &[String]) -> Result<Vec<String>> {
    let mut normalized = Vec::new();
    for label in labels {
        let label = label.trim();
        if label.is_empty() {
            continue;
        }
        if label.chars().count() > 64 {
            bail!("invalid label: labels must be 64 characters or fewer");
        }
        if !normalized
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(label))
        {
            normalized.push(label.to_string());
        }
    }
    if normalized.len() > 24 {
        bail!("invalid labels: an asset can have at most 24 labels");
    }
    Ok(normalized)
}

pub(crate) fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tempfile::{TempDir, tempdir};

    #[test]
    fn analysis_budget_requires_finite_positive_value() {
        for value in [0.0, -0.01, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let options = AnalysisRunOptions {
                budget_usd: Some(value),
                ..Default::default()
            };
            assert!(
                resolve_analysis_budget(&options).is_err(),
                "accepted {value:?}"
            );
        }
        let options = AnalysisRunOptions {
            budget_usd: Some(0.01),
            ..Default::default()
        };
        assert_eq!(resolve_analysis_budget(&options).unwrap(), 0.01);
    }

    #[test]
    fn invalid_budget_is_rejected_before_key_resolution_or_network_setup() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("budget.sqlite3")).unwrap();
        let error = catalog
            .analyze_missing_with_options(
                "missing-project",
                &AnalysisRunOptions {
                    budget_usd: Some(f64::NAN),
                    ..Default::default()
                },
                |_| ControlFlow::Continue(()),
            )
            .unwrap_err();
        assert!(format!("{error:#}").contains("finite"));
    }

    /// Inserts an asset whose stored fingerprint is the real sampled fingerprint
    /// of the file on disk, so location verification has something honest to
    /// compare against. Deliberately avoids `ingest_folder`, which would probe
    /// the file and require ffprobe.
    pub(crate) fn insert_asset_at(catalog: &Catalog, project_id: &str, id: &str, path: &Path) {
        let size = fs::metadata(path).unwrap().len();
        let fingerprint = sampled_fingerprint(path, size).unwrap();
        catalog
            .connection()
            .unwrap()
            .execute(
                r#"
                INSERT INTO assets (
                    id, project_id, fingerprint, path, filename, size_bytes,
                    duration_seconds, frame_rate, width, height, codec, camera,
                    captured_at_raw, best_time, source_out, created_at,
                    orientation_probed
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 10, 30, 1920, 1080,
                          'h264', 'Sony (C-series)', NULL, NULL, 10, ?7, 1)
                "#,
                params![
                    id,
                    project_id,
                    fingerprint,
                    path.to_string_lossy(),
                    path.file_name().unwrap().to_string_lossy(),
                    size as i64,
                    now(),
                ],
            )
            .unwrap();
    }

    fn location_state(catalog: &Catalog, asset_id: &str, path: &Path) -> Option<String> {
        catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT verification_state FROM project_asset_local_locations
                 WHERE asset_id = ?1 AND local_path = ?2",
                params![asset_id, path.to_string_lossy()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .unwrap()
    }

    /// The defect this epic exists to prevent: a card is reformatted and reused,
    /// a new clip lands at the pathname an old asset remembers, and the old
    /// asset quietly starts resolving to footage that is not its own.
    #[test]
    fn a_reused_pathname_never_binds_an_asset_to_different_content() {
        let (directory, catalog, project) = catalog_fixture();
        let card = directory.path().join("CARD/DCIM/100MSDCF");
        fs::create_dir_all(&card).unwrap();
        let clip = card.join("C0012.MP4");
        fs::write(&clip, b"the original clip, shot in Spain").unwrap();
        insert_asset_at(&catalog, &project.id, "asset_spain", &clip);

        // Verified once, this resolves without complaint.
        assert!(catalog.resolve_original("asset_spain").is_ok());
        assert!(catalog.get_asset("asset_spain").unwrap().original_available);

        // The card is reformatted and the camera writes a different clip to the
        // same DCIM pathname.
        fs::write(&clip, b"a completely different clip, shot in Iceland later").unwrap();

        assert!(
            !catalog.get_asset("asset_spain").unwrap().original_available,
            "an asset must not report an original it can no longer vouch for"
        );
        let error = catalog.resolve_original("asset_spain").unwrap_err();
        assert!(
            format!("{error:#}").contains("not this clip any more"),
            "unexpected error: {error:#}"
        );
        assert_eq!(
            location_state(&catalog, "asset_spain", &clip).as_deref(),
            Some("mismatch")
        );
    }

    /// A mismatch is a conclusion, not a transient failure. Restoring the right
    /// bytes must not silently re-arm the location; only an explicit rescan does.
    #[test]
    fn a_mismatched_location_is_repaired_only_by_an_explicit_rescan() {
        let (directory, catalog, project) = catalog_fixture();
        let card = directory.path().join("CARD");
        fs::create_dir_all(&card).unwrap();
        let clip = card.join("C0012.MP4");
        fs::write(&clip, b"the original clip").unwrap();
        insert_asset_at(&catalog, &project.id, "asset_one", &clip);
        catalog.resolve_original("asset_one").unwrap();

        fs::write(&clip, b"different content entirely").unwrap();
        assert!(catalog.resolve_original("asset_one").is_err());

        // The right file comes back, but nothing has re-checked it yet.
        fs::write(&clip, b"the original clip").unwrap();
        assert!(
            !catalog.get_asset("asset_one").unwrap().original_available,
            "a location proven wrong must stay untrusted until it is rechecked"
        );

        // An ingest over the folder is that recheck.
        let report = catalog.ingest_folder(&project.id, &card).unwrap();
        assert_eq!(report.imported, 0);
        assert_eq!(
            location_state(&catalog, "asset_one", &clip).as_deref(),
            Some("verified")
        );
        assert!(catalog.get_asset("asset_one").unwrap().original_available);
    }

    /// Content already known to the project, discovered where the asset is not
    /// currently looking, is the missing original — not a duplicate to discard.
    #[test]
    fn a_known_clip_found_at_a_new_path_repairs_the_asset() {
        let (directory, catalog, project) = catalog_fixture();
        let first_mount = directory.path().join("Volumes/CARD");
        let second_mount = directory.path().join("Volumes/CARD 1");
        fs::create_dir_all(&first_mount).unwrap();
        fs::create_dir_all(&second_mount).unwrap();
        let original = first_mount.join("C0001.MP4");
        fs::write(&original, b"clip bytes that do not change").unwrap();
        insert_asset_at(&catalog, &project.id, "asset_one", &original);

        // The volume remounts somewhere else, exactly as macOS does when a card
        // is inserted while the previous mount lingers.
        let moved = second_mount.join("C0001.MP4");
        fs::rename(&original, &moved).unwrap();
        assert!(!catalog.get_asset("asset_one").unwrap().original_available);

        let report = catalog.ingest_folder(&project.id, &second_mount).unwrap();
        assert_eq!(report.scanned, 1);
        assert_eq!(
            report.imported, 0,
            "repairing must not create a second asset"
        );
        assert_eq!(report.relinked, 1);
        assert_eq!(report.duplicates, 0);

        let repaired = catalog.get_asset("asset_one").unwrap();
        assert_eq!(repaired.path, moved.to_string_lossy());
        assert!(repaired.original_available);
        assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 1);
    }

    /// A second copy is worth remembering, but it is no reason to move an asset
    /// off a location that already works.
    #[test]
    fn a_second_copy_is_recorded_without_disturbing_a_working_asset() {
        let (directory, catalog, project) = catalog_fixture();
        let nas = directory.path().join("nas");
        let card = directory.path().join("card");
        fs::create_dir_all(&nas).unwrap();
        fs::create_dir_all(&card).unwrap();
        let original = nas.join("C0001.MP4");
        fs::write(&original, b"clip bytes").unwrap();
        insert_asset_at(&catalog, &project.id, "asset_one", &original);
        catalog.resolve_original("asset_one").unwrap();

        let copy = card.join("C0001.MP4");
        fs::copy(&original, &copy).unwrap();
        let report = catalog.ingest_folder(&project.id, &card).unwrap();
        assert_eq!(report.duplicates, 1);
        assert_eq!(report.relinked, 0);

        let asset = catalog.get_asset("asset_one").unwrap();
        assert_eq!(
            asset.path,
            original.to_string_lossy(),
            "a working location must not be swapped out from under the asset"
        );
        // Both locations are nonetheless known, which is what lets a later
        // archive step reason about where this content already exists.
        assert_eq!(
            location_state(&catalog, "asset_one", &copy).as_deref(),
            Some("verified")
        );
    }

    /// The whole point of the repair flow: the user moved their footage, points
    /// at where it went, and the project reconnects without gaining anything it
    /// did not already have.
    #[test]
    fn locating_moved_originals_repairs_them_without_importing_anything_new() {
        let (directory, catalog, project) = catalog_fixture();
        let old_home = directory.path().join("local/footage");
        let new_home = directory.path().join("Volumes/Shuttle/footage");
        fs::create_dir_all(&old_home).unwrap();
        fs::create_dir_all(&new_home).unwrap();

        for (id, name, bytes) in [
            ("asset_one", "C0001.MP4", b"first clip ".as_slice()),
            ("asset_two", "C0002.MP4", b"second clip".as_slice()),
        ] {
            let path = old_home.join(name);
            fs::write(&path, bytes).unwrap();
            insert_asset_at(&catalog, &project.id, id, &path);
        }

        // The user drags the folder to an external drive behind the app's back,
        // and something unrelated to this project is sitting there too.
        for name in ["C0001.MP4", "C0002.MP4"] {
            fs::rename(old_home.join(name), new_home.join(name)).unwrap();
        }
        fs::write(new_home.join("SOMEONE_ELSES.MP4"), b"not this project").unwrap();

        catalog.detect_offline_volumes(&project.id).unwrap();
        assert_eq!(catalog.offline_asset_count(&project.id).unwrap(), 2);

        let report = catalog
            .relink_folder(&project.id, &new_home, |_, _, _| {})
            .unwrap();

        assert_eq!(report.scanned, 3);
        assert_eq!(report.repaired, 2);
        assert_eq!(
            report.unrecognized, 1,
            "media belonging to no asset here must be counted, never imported"
        );
        assert_eq!(report.still_missing, 0);
        assert_eq!(
            catalog.list_assets(&project.id).unwrap().len(),
            2,
            "locating must not enlarge the project"
        );
        for asset in catalog.list_assets(&project.id).unwrap() {
            assert!(asset.original_available);
            assert!(
                asset
                    .path
                    .starts_with(&new_home.to_string_lossy().to_string())
            );
        }

        // The offline-volume diagnostic that sent the user here is resolved.
        let unacknowledged = catalog
            .diagnostics(&project.id, &DiagnosticFilter::default())
            .unwrap();
        assert!(
            !unacknowledged
                .iter()
                .any(|diagnostic| diagnostic.kind == "media.volume_offline"),
            "a volume that came back must stop being reported as offline"
        );
    }

    /// Pointing at the wrong folder has to be a no-op the user can read, not an
    /// import they have to undo.
    #[test]
    fn locating_against_an_unrelated_folder_changes_nothing() {
        let (directory, catalog, project) = catalog_fixture();
        let home = directory.path().join("footage");
        let elsewhere = directory.path().join("holiday-snaps");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        let clip = home.join("C0001.MP4");
        fs::write(&clip, b"the missing clip").unwrap();
        insert_asset_at(&catalog, &project.id, "asset_one", &clip);
        fs::remove_file(&clip).unwrap();
        fs::write(elsewhere.join("VACATION.MP4"), b"unrelated footage").unwrap();

        let report = catalog
            .relink_folder(&project.id, &elsewhere, |_, _, _| {})
            .unwrap();

        assert_eq!(report.repaired, 0);
        assert_eq!(report.unrecognized, 1);
        assert_eq!(report.still_missing, 1);
        assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 1);
        assert_eq!(
            catalog.get_asset("asset_one").unwrap().path,
            clip.to_string_lossy(),
            "a failed search must leave the asset pointing where it was"
        );
    }

    /// Verification is cached: a second resolve of an unchanged file must not
    /// re-hash it, or every proxy batch would re-read every original.
    #[test]
    fn an_unchanged_verified_location_is_not_rehashed() {
        let (directory, catalog, project) = catalog_fixture();
        let clip = directory.path().join("C0001.MP4");
        fs::write(&clip, b"clip bytes").unwrap();
        insert_asset_at(&catalog, &project.id, "asset_one", &clip);

        catalog.resolve_original("asset_one").unwrap();
        let first_verified_at: String = catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT verified_at FROM project_asset_local_locations WHERE asset_id = 'asset_one'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        catalog.resolve_original("asset_one").unwrap();
        let second_verified_at: String = catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT verified_at FROM project_asset_local_locations WHERE asset_id = 'asset_one'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            first_verified_at, second_verified_at,
            "an unchanged location must be trusted from its cached verification"
        );
    }

    /// An asset carried forward from an older catalog has never been verified.
    /// It must still be usable, or upgrading would report a whole project
    /// offline.
    #[test]
    fn an_unverified_legacy_location_stays_usable() {
        let (directory, catalog, project) = catalog_fixture();
        let clip = directory.path().join("C0001.MP4");
        fs::write(&clip, b"clip bytes").unwrap();
        insert_asset_at(&catalog, &project.id, "asset_one", &clip);
        assert!(
            location_state(&catalog, "asset_one", &clip).is_none(),
            "this fixture inserts no location row, standing in for a legacy catalog"
        );
        assert!(catalog.get_asset("asset_one").unwrap().original_available);
    }

    fn catalog_fixture() -> (TempDir, Catalog, Project) {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Test project").unwrap();
        (directory, catalog, project)
    }

    /// #76: a volume that stops answering leaves a read parked forever rather
    /// than failing. The ingest has to give up on its own, because the
    /// import's disconnect handling only runs on an `Err` — a call that never
    /// returns bypasses every recovery path the import has.
    #[cfg(unix)]
    #[test]
    fn an_ingest_whose_read_never_returns_gives_up_instead_of_hanging() {
        let (directory, catalog, project) = catalog_fixture();
        let unresponsive = directory.path().join("unresponsive.MP4");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&unresponsive)
                .status()
                .unwrap()
                .success(),
            "could not create the FIFO this test needs"
        );

        // Run the ingest on its own thread so a regression fails this test in
        // seconds instead of hanging the suite the way it hung the app.
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome =
                crate::stall::with_io_stall_timeout(std::time::Duration::from_millis(300), || {
                    catalog.ingest_file_with_camera(&project.id, &unresponsive, None)
                });
            let _ = sender.send(outcome.err().map(|error| format!("{error:#}")));
        });

        match receiver.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(Some(error)) => assert!(
                error.contains("stopped answering"),
                "the failure must name the stall, got: {error}"
            ),
            Ok(None) => panic!("ingesting an unresponsive path unexpectedly succeeded"),
            Err(_) => panic!("the ingest never returned — this is the hang #76 is about"),
        }
    }

    #[test]
    fn chronology_resolution_reserves_the_writer_before_loading_facts() {
        let (_directory, catalog, project) = catalog_fixture();
        catalog
            .add_portable_project_asset(
                &project.id,
                ProjectAssetAddedPayload {
                    id: "asset_chronology_lock".to_string(),
                    fingerprint: "fingerprint_chronology_lock".to_string(),
                    filename: "C0001.MP4".to_string(),
                    size_bytes: 100,
                    duration_seconds: 10.0,
                    frame_rate: 30.0,
                    width: 1920,
                    height: 1080,
                    codec: "h264".to_string(),
                    camera: "Sony".to_string(),
                    captured_at_raw: Some("2026-01-01T09:00:00Z".to_string()),
                    timestamp_candidates: None,
                    best_time: Some("2026-01-01T09:00:00Z".to_string()),
                    metadata_rotation_degrees: 0,
                    default_rotation_degrees: 0,
                    created_at: "2026-01-01T00:00:00Z".to_string(),
                },
            )
            .unwrap();
        let catalog_path = catalog.database_path().to_path_buf();
        let mut observed_reservation = false;

        let solution = catalog
            .resolve_chronology_with_hook(&project.id, || {
                let competing = Connection::open(&catalog_path).unwrap();
                competing.busy_timeout(std::time::Duration::ZERO).unwrap();
                let error = competing
                    .execute_batch("BEGIN IMMEDIATE")
                    .expect_err("a competing writer must wait until projection commits");
                assert!(
                    error.to_string().contains("locked"),
                    "unexpected competing-writer error: {error}"
                );
                observed_reservation = true;
            })
            .unwrap();

        assert!(observed_reservation);
        assert_eq!(solution.assets.len(), 1);
        assert_eq!(
            catalog
                .get_asset("asset_chronology_lock")
                .unwrap()
                .best_time
                .as_deref(),
            solution.assets[0].trip_time.as_deref()
        );
    }

    /// Writes a byte-identical stand-in for a generated proxy and points the
    /// asset at it. Enough for fingerprinting and staleness tests, which never
    /// decode the file.
    fn attach_stub_proxy(
        catalog: &Catalog,
        asset_id: &str,
        directory: &Path,
        bytes: &[u8],
    ) -> PathBuf {
        let proxy_path = directory.join(format!("{asset_id}.proxy.mp4"));
        fs::write(&proxy_path, bytes).unwrap();
        catalog
            .connection()
            .unwrap()
            .execute(
                "UPDATE assets SET proxy_path = ?2 WHERE id = ?1",
                params![asset_id, proxy_path.to_string_lossy()],
            )
            .unwrap();
        proxy_path
    }

    fn mark_asset_video_usable(catalog: &Catalog, asset_id: &str) {
        let asset = catalog.get_asset(asset_id).unwrap();
        catalog
            .store_video_validation(asset_id, &asset.fingerprint, "usable", None)
            .unwrap();
    }

    #[test]
    fn proxy_fingerprint_changes_when_the_proxy_is_regenerated() {
        let (directory, catalog, project) = catalog_fixture();
        insert_asset(
            &catalog,
            &project.id,
            "a1",
            "DJI_0001.MP4",
            "DJI Pocket 3",
            "2026-04-01T09:00:00Z",
            Path::new("/volumes/nas/DJI_0001.MP4"),
        );
        attach_stub_proxy(&catalog, "a1", directory.path(), b"proxy version three");
        let first = catalog.proxy_fingerprint("a1").unwrap();
        attach_stub_proxy(&catalog, "a1", directory.path(), b"proxy regenerated anew");
        let second = catalog.proxy_fingerprint("a1").unwrap();
        assert_ne!(
            first, second,
            "a regenerated proxy must invalidate its transcript"
        );
    }

    #[test]
    fn proxy_fingerprint_fails_clearly_without_a_proxy() {
        let (_directory, catalog, project) = catalog_fixture();
        insert_asset(
            &catalog,
            &project.id,
            "a1",
            "DJI_0001.MP4",
            "DJI Pocket 3",
            "2026-04-01T09:00:00Z",
            Path::new("/volumes/nas/DJI_0001.MP4"),
        );
        let error = catalog.proxy_fingerprint("a1").unwrap_err();
        assert!(error.to_string().contains("proxy is required"), "{error}");
    }

    #[test]
    fn transcribe_missing_skips_clips_with_a_current_transcript() {
        let (directory, catalog, project) = catalog_fixture();
        insert_asset(
            &catalog,
            &project.id,
            "a1",
            "DJI_0001.MP4",
            "DJI Pocket 3",
            "2026-04-01T09:00:00Z",
            Path::new("/volumes/nas/DJI_0001.MP4"),
        );
        attach_stub_proxy(&catalog, "a1", directory.path(), b"proxy bytes");
        let fingerprint = catalog.proxy_fingerprint("a1").unwrap();
        save_transcript(
            &catalog.connection().unwrap(),
            &StoredTranscript {
                asset_id: "a1".to_string(),
                recipe_key: STT_RECIPE_KEY.to_string(),
                source_fingerprint: fingerprint,
                status: TranscriptStatus::NoSpeech,
                outcome_reason: crate::analysis::store::TranscriptOutcomeReason::NoDetectedSpeech,
                reject_reason: String::new(),
                primary_language: String::new(),
                speech_seconds: 0.0,
                window_count: 0,
                provider_segment_count: 0,
                mean_token_probability: None,
                repetition_ratio: None,
                segments: Vec::new(),
            },
        )
        .unwrap();

        // No models are installed, so reaching the engine would fail. Returning
        // a clean report proves the clip was skipped before that point.
        let report = catalog.transcribe_missing(&project.id).unwrap();
        assert_eq!(report.skipped, 1);
        assert_eq!(report.transcribed, 0);
        assert!(report.outcomes.is_empty());
    }

    #[test]
    fn transcribe_missing_reports_a_corrupt_proxy_as_a_processing_failure() {
        let (directory, catalog, project) = catalog_fixture();
        insert_asset(
            &catalog,
            &project.id,
            "a1",
            "DJI_0001.MP4",
            "DJI Pocket 3",
            "2026-04-01T09:00:00Z",
            Path::new("/volumes/nas/DJI_0001.MP4"),
        );
        attach_stub_proxy(&catalog, "a1", directory.path(), b"proxy bytes");
        mark_asset_video_usable(&catalog, "a1");
        let report = catalog.transcribe_missing(&project.id).unwrap();
        assert_eq!(report.failed, 1);
        assert_eq!(report.transcribed, 0);
        assert_eq!(report.failures.len(), 1);
        assert!(
            report.failures[0].error.contains("probe audio"),
            "expected a probe failure, got: {}",
            report.failures[0].error
        );
        let stored = catalog.asset_transcript("a1").unwrap().unwrap();
        assert_eq!(stored.status, TranscriptStatus::Failed);
        assert_eq!(
            stored.outcome_reason,
            crate::analysis::store::TranscriptOutcomeReason::ProcessingFailed
        );
        assert!(!stored.source_fingerprint.is_empty());
        let coverage = catalog.transcript_coverage(&project.id).unwrap();
        assert_eq!(coverage.failed, 1);
        assert_eq!(coverage.missing, 0);
    }

    fn insert_asset(
        catalog: &Catalog,
        project_id: &str,
        id: &str,
        filename: &str,
        camera: &str,
        raw_time: &str,
        path: &Path,
    ) {
        catalog
            .connection()
            .unwrap()
            .execute(
                r#"
                INSERT INTO assets (
                    id, project_id, fingerprint, path, filename, size_bytes,
                    duration_seconds, frame_rate, width, height, codec, camera,
                    captured_at_raw, best_time, source_out, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, 100, 10, 30, 1920, 1080,
                          'h264', ?6, ?7, ?8, 10, ?9)
                "#,
                params![
                    id,
                    project_id,
                    format!("fingerprint-{id}"),
                    path.to_string_lossy(),
                    filename,
                    camera,
                    raw_time,
                    normalize_datetime(raw_time, 0),
                    now(),
                ],
            )
            .unwrap();
    }

    #[test]
    fn rename_camera_moves_only_the_named_lane_in_the_named_project() {
        let (_directory, catalog, project) = catalog_fixture();
        let other = catalog.create_project("Another trip").unwrap();
        for (id, camera) in [
            ("a1", UNKNOWN_CAMERA),
            ("a2", UNKNOWN_CAMERA),
            ("a3", "GoPro"),
        ] {
            insert_asset(
                &catalog,
                &project.id,
                id,
                &format!("{id}.MP4"),
                camera,
                "2026-04-01T09:00:00Z",
                Path::new("/volumes/nas/clip.MP4"),
            );
        }
        insert_asset(
            &catalog,
            &other.id,
            "b1",
            "b1.MP4",
            UNKNOWN_CAMERA,
            "2026-04-01T09:00:00Z",
            Path::new("/volumes/nas/other.MP4"),
        );

        let changed = catalog
            .rename_camera(&project.id, UNKNOWN_CAMERA, "  DJI Pocket 3  ")
            .unwrap();

        assert_eq!(changed, 2);
        let cameras = |id: &str| catalog.get_asset(id).unwrap().camera;
        assert_eq!(cameras("a1"), "DJI Pocket 3", "trims the typed name");
        assert_eq!(cameras("a2"), "DJI Pocket 3");
        assert_eq!(cameras("a3"), "GoPro", "another lane is untouched");
        assert_eq!(
            cameras("b1"),
            UNKNOWN_CAMERA,
            "the same lane in another project is untouched"
        );
    }

    #[test]
    fn rename_camera_refuses_an_empty_name() {
        let (_directory, catalog, project) = catalog_fixture();
        insert_asset(
            &catalog,
            &project.id,
            "a1",
            "a1.MP4",
            "GoPro",
            "2026-04-01T09:00:00Z",
            Path::new("/volumes/nas/clip.MP4"),
        );
        assert!(catalog.rename_camera(&project.id, "GoPro", "   ").is_err());
        assert_eq!(catalog.get_asset("a1").unwrap().camera, "GoPro");
    }

    #[test]
    fn creates_and_lists_projects() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let created = catalog.create_project("Canada 2026").unwrap();
        let projects = catalog.list_projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, created.id);
        assert_eq!(projects[0].name, "Canada 2026");
    }

    #[test]
    fn opens_existing_catalogs_without_mutating_unrelated_databases() {
        let directory = tempdir().unwrap();
        let catalog_path = directory.path().join("catalog.sqlite3");
        let catalog = Catalog::open(&catalog_path).unwrap();
        let project = catalog.create_project("Existing").unwrap();
        drop(catalog);
        let reopened = Catalog::open_existing(&catalog_path).unwrap();
        assert_eq!(reopened.list_projects().unwrap()[0].id, project.id);

        let unrelated_path = directory.path().join("unrelated.sqlite3");
        Connection::open(&unrelated_path)
            .unwrap()
            .execute("CREATE TABLE unrelated (id INTEGER PRIMARY KEY)", [])
            .unwrap();
        assert!(Catalog::open_existing(&unrelated_path).is_err());
        let unrelated = Connection::open(&unrelated_path).unwrap();
        let vlog_tables: i64 = unrelated
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('projects', 'assets')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(vlog_tables, 0);
        assert!(Catalog::open_existing(directory.path().join("missing.sqlite3")).is_err());
    }

    #[test]
    fn migrates_legacy_catalogs_to_labels_removal_and_landscape_orientation() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(include_str!(
                "../tests/fixtures/catalog_pre_orientation_labels.sql"
            ))
            .unwrap();
        drop(connection);

        let bytes_before_read_only = fs::read(&path).unwrap();
        let read_only = Catalog::open_read_only_existing(&path).unwrap();
        let legacy_asset = read_only.get_asset("asset_portrait").unwrap();
        assert!(!legacy_asset.orientation_probed);
        assert_eq!(legacy_asset.metadata_rotation_degrees, 0);
        assert_eq!(legacy_asset.notes, "keep this note");
        assert!(legacy_asset.labels.is_empty());
        assert!(!legacy_asset.removed);
        assert_eq!(legacy_asset.media_object_id, None);
        assert_eq!(
            legacy_asset.media_verification_state,
            MediaVerificationState::Unlinked
        );
        drop(read_only);
        let inspection = inspect_catalog(&path).unwrap();
        assert_eq!(inspection.current_schema_version, 0);
        assert_eq!(
            inspection.schema_status,
            MigrationHistoryStatus::LegacyUnversioned
        );
        assert_eq!(
            inspection.pending_migrations.len(),
            PROJECT_CATALOG_SCHEMA_VERSION as usize
        );
        assert_eq!(fs::read(&path).unwrap(), bytes_before_read_only);
        let still_legacy = Connection::open(&path).unwrap();
        assert!(!asset_column_exists(&still_legacy, "orientation_probed").unwrap());
        assert!(!table_exists(&still_legacy, "schema_migrations").unwrap());
        drop(still_legacy);

        let catalog = Catalog::open_existing(&path).unwrap();
        let asset = catalog.get_asset("asset_portrait").unwrap();
        assert_eq!(asset.id, "asset_portrait");
        assert_eq!(asset.project_id, "project_old");
        assert_eq!(asset.metadata_rotation_degrees, 0);
        assert_eq!(asset.default_rotation_degrees, 90);
        assert_eq!(asset.rotation_degrees, 90);
        assert_eq!(asset.notes, "keep this note");
        assert!(asset.labels.is_empty());
        assert!(!asset.removed);
        assert_eq!(
            asset.proxy_path.as_deref(),
            Some("/legacy/cache/asset_portrait.v2.mp4")
        );
        assert_eq!(asset.media_object_id, None);
        assert_eq!(
            asset.media_verification_state,
            MediaVerificationState::Unlinked
        );
        let asset_json = serde_json::to_value(&asset).unwrap();
        assert_eq!(asset_json["mediaObjectId"], serde_json::Value::Null);
        assert_eq!(asset_json["mediaVerificationState"], "unlinked");
        let project_json = serde_json::to_value(&catalog.list_projects().unwrap()[0]).unwrap();
        assert_eq!(project_json["originNodeId"], serde_json::Value::Null);
        assert_eq!(project_json["pairedServerNodeId"], serde_json::Value::Null);
        assert_eq!(
            catalog
                .connection()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM clock_events WHERE project_id = 'project_old'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        let migrated = Connection::open(&path).unwrap();
        assert!(asset_column_exists(&migrated, "labels_json").unwrap());
        assert!(asset_column_exists(&migrated, "removed").unwrap());
        assert!(asset_column_exists(&migrated, "media_object_id").unwrap());
        assert_eq!(
            migrated
                .query_row(
                    "SELECT path FROM asset_thumbnails WHERE asset_id = 'asset_portrait' AND quality = 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "/legacy/cache/asset_portrait/q1.v2.jpg"
        );
        let migration_count = migrated
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        assert_eq!(migration_count, PROJECT_CATALOG_SCHEMA_VERSION as i64);
    }

    #[test]
    fn unversioned_current_catalog_upgrades_once_without_changing_ids_or_annotations() {
        type ArtifactRow = (String, String, String, String);
        type StepRow = (String, String, String, String, String);

        fn moment_rows(catalog: &Catalog) -> (ArtifactRow, StepRow, i64, i64) {
            let connection = catalog.connection().unwrap();
            let artifact = connection
                .query_row(
                    "SELECT asset_id,identity,body_json,created_at FROM moment_artifacts",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
            let step = connection
                .query_row(
                    "SELECT asset_id,identity,step_key,body_json,created_at FROM moment_steps",
                    [],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .unwrap();
            let artifact_count = connection
                .query_row("SELECT COUNT(*) FROM moment_artifacts", [], |row| {
                    row.get(0)
                })
                .unwrap();
            let step_count = connection
                .query_row("SELECT COUNT(*) FROM moment_steps", [], |row| row.get(0))
                .unwrap();
            (artifact, step, artifact_count, step_count)
        }

        let directory = tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite3");
        let catalog = Catalog::open(&path).unwrap();
        let project = catalog.create_project("Current catalog").unwrap();
        insert_asset(
            &catalog,
            &project.id,
            "asset_existing",
            "A.MP4",
            "Camera A",
            "2026-07-18T00:00:00Z",
            Path::new("/missing/A.MP4"),
        );
        let current = catalog.get_asset("asset_existing").unwrap();
        catalog
            .update_asset(
                "asset_existing",
                &AssetPatch {
                    notes: "preserved note".to_string(),
                    labels: vec!["arrival".to_string()],
                    removed: true,
                    rotation_degrees: current.rotation_degrees,
                    expected_revision: None,
                },
            )
            .unwrap();
        catalog.connection().unwrap().execute(
            "INSERT INTO moment_artifacts(asset_id,identity,body_json,created_at) VALUES ('asset_existing','base-identity','{\"preserved\":\"artifact\"}','artifact-created')",
            [],
        ).unwrap();
        catalog.connection().unwrap().execute(
            "INSERT INTO moment_steps(asset_id,identity,step_key,body_json,created_at) VALUES ('asset_existing','base-identity','coarse:0','{\"preserved\":\"step\"}','step-created')",
            [],
        ).unwrap();
        let expected_moment_rows = moment_rows(&catalog);
        assert_eq!(expected_moment_rows.2, 1);
        assert_eq!(expected_moment_rows.3, 1);
        drop(catalog);

        Connection::open(&path)
            .unwrap()
            .execute_batch("DROP TABLE schema_migrations")
            .unwrap();
        let before = inspect_catalog(&path).unwrap();
        assert_eq!(before.current_schema_version, 0);
        assert_eq!(
            before.pending_migrations.len(),
            PROJECT_CATALOG_SCHEMA_VERSION as usize
        );

        let upgraded = Catalog::open_existing(&path).unwrap();
        let upgraded_project = upgraded.list_projects().unwrap().remove(0);
        let upgraded_asset = upgraded.get_asset("asset_existing").unwrap();
        assert_eq!(upgraded_project.id, project.id);
        assert_eq!(upgraded_asset.id, "asset_existing");
        assert_eq!(upgraded_asset.notes, "preserved note");
        assert_eq!(upgraded_asset.labels, vec!["arrival"]);
        assert!(upgraded_asset.removed);
        assert_eq!(
            moment_rows(&upgraded),
            expected_moment_rows,
            "migration replay must preserve cached moment artifacts and resumable steps"
        );
        drop(upgraded);

        let first_history = inspect_catalog(&path).unwrap().applied_migrations;
        assert_eq!(first_history.len(), PROJECT_CATALOG_SCHEMA_VERSION as usize);
        let reopened = Catalog::open_existing(&path).unwrap();
        assert_eq!(
            reopened.get_asset("asset_existing").unwrap().id,
            "asset_existing"
        );
        assert_eq!(
            moment_rows(&reopened),
            expected_moment_rows,
            "reopening the upgraded catalog must retain the exact moment rows"
        );
        drop(reopened);
        let reopened_inspection = inspect_catalog(&path).unwrap();
        assert_eq!(
            reopened_inspection.current_schema_version,
            PROJECT_CATALOG_SCHEMA_VERSION
        );
        assert_eq!(
            reopened_inspection.schema_status,
            MigrationHistoryStatus::Current
        );
        assert!(reopened_inspection.pending_migrations.is_empty());
        assert_eq!(reopened_inspection.applied_migrations, first_history);
    }

    #[test]
    fn migration_four_upgrades_to_operation_baselines_without_inventing_history() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("catalog-v4.sqlite3");
        let mut connection = Connection::open(&path).unwrap();
        migrations::run_sqlite_migrations(
            &mut connection,
            &migrations::PROJECT_CATALOG_MIGRATIONS[..4],
        )
        .unwrap();
        connection.execute("INSERT INTO projects(id, name, created_at) VALUES ('project_v4', 'Version four', '2026-01-01T00:00:00Z')", []).unwrap();
        connection.execute("INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, notes, labels_json, removed, rotation_degrees, created_at) VALUES ('asset_v4', 'project_v4', 'fingerprint-v4', '/offline/v4.mp4', 'v4.mp4', 100, 10, 30, 1920, 1080, 'h264', 'DJI', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00.000Z', 'kept note', '[\"kept label\"]', 1, 90, '2026-01-01T00:00:00Z')", []).unwrap();
        connection.execute("INSERT INTO clock_events(id, project_id, camera, start_filename, offset_seconds, label, created_at) VALUES ('clock_v4', 'project_v4', 'DJI', 'v4.mp4', 60, 'kept clock', '2026-01-01T00:00:00Z')", []).unwrap();
        drop(connection);

        let before = inspect_catalog(&path).unwrap();
        assert_eq!(before.current_schema_version, 4);
        assert_eq!(
            before.pending_migrations.len(),
            PROJECT_CATALOG_SCHEMA_VERSION as usize - 4
        );
        let catalog = Catalog::open_existing(&path).unwrap();
        let asset = catalog.get_asset("asset_v4").unwrap();
        assert_eq!(asset.notes, "kept note");
        assert_eq!(asset.labels, ["kept label"]);
        assert!(asset.removed);
        assert_eq!(asset.rotation_degrees, 90);
        assert_eq!(catalog.list_operations("project_v4").unwrap().len(), 0);
        let video_validation = catalog.asset_video_validation("asset_v4").unwrap();
        assert_eq!(
            video_validation.status,
            AssetVideoValidationStatus::Unverified
        );
        assert!(!video_validation.bytes_reachable);
        let diagnostics = catalog.asset_revision_diagnostics("asset_v4").unwrap();
        assert_eq!(diagnostics.materialized_revision, 0);
        assert_eq!(diagnostics.note_revisions.len(), 1);
        assert!(diagnostics.note_revisions[0].active);
        let connection = catalog.connection().unwrap();
        for table in [
            "project_baseline_metadata",
            "project_storage_root_mappings",
            "sync_origin_cursors",
            "sync_observation_facts",
            "sync_representation_manifests",
        ] {
            assert!(table_exists(&connection, table).unwrap(), "{table}");
        }
        assert_eq!(
            connection.query_row("SELECT revision FROM materialized_revisions WHERE project_id='project_v4' AND aggregate_type='clock' AND aggregate_id='DJI'", [], |row| row.get::<_, i64>(0)).unwrap(),
            1
        );
    }

    #[test]
    fn writable_open_rejects_a_newer_catalog_with_an_actionable_error() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite3");
        drop(Catalog::open(&path).unwrap());
        Connection::open(&path)
            .unwrap()
            .execute(
                "INSERT INTO schema_migrations (migration_id, name, source_identity, applied_at) VALUES (?1, 'future', ?2, '2026-07-20T00:00:00.000Z')",
                params![
                    PROJECT_CATALOG_SCHEMA_VERSION + 1,
                    format!("future/{}", PROJECT_CATALOG_SCHEMA_VERSION + 1)
                ],
            )
            .unwrap();

        let inspection = inspect_catalog(&path).unwrap();
        assert_eq!(
            inspection.current_schema_version,
            PROJECT_CATALOG_SCHEMA_VERSION + 1
        );
        assert_eq!(
            inspection.schema_status,
            MigrationHistoryStatus::NewerUnsupported
        );
        let error = Catalog::open_existing(&path).unwrap_err();
        assert_eq!(
            error.downcast_ref::<CatalogSchemaError>().unwrap().code(),
            "schema_incompatible"
        );
        let message = format!("{error:#}");
        assert!(message.contains(&format!(
            "newer than supported version {PROJECT_CATALOG_SCHEMA_VERSION}"
        )));
        assert!(message.contains("upgrade RCD VlogCutter"));
    }

    #[test]
    fn changed_migration_name_or_source_identity_is_inspectable_but_not_writable() {
        for (stored_name, stored_source_identity) in [
            (
                "changed_catalog_baseline",
                "project-catalog/0001-catalog-baseline/20260720",
            ),
            ("catalog_baseline", "changed/project-catalog/0001"),
        ] {
            let directory = tempdir().unwrap();
            let path = directory.path().join("catalog.sqlite3");
            drop(Catalog::open(&path).unwrap());
            Connection::open(&path)
                .unwrap()
                .execute(
                    "UPDATE schema_migrations SET name = ?1, source_identity = ?2 WHERE migration_id = 1",
                    params![stored_name, stored_source_identity],
                )
                .unwrap();

            let before = inspect_catalog(&path).unwrap();
            assert_eq!(
                before.schema_status,
                MigrationHistoryStatus::IncompatibleHistory
            );
            assert!(before.pending_migrations.is_empty());
            let error = Catalog::open_existing(&path).unwrap_err();
            assert!(matches!(
                error.downcast_ref::<CatalogSchemaError>(),
                Some(CatalogSchemaError::IncompatibleHistory {
                    migration_id: 1,
                    ..
                })
            ));
            let after = inspect_catalog(&path).unwrap();
            assert_eq!(after.applied_migrations, before.applied_migrations);
            assert_eq!(after.pending_migrations, before.pending_migrations);
            assert_eq!(
                after.schema_status,
                MigrationHistoryStatus::IncompatibleHistory
            );
        }
    }

    #[test]
    fn migration_history_gap_is_rejected_without_applying_the_pending_migration() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite3");
        drop(Catalog::open(&path).unwrap());
        Connection::open(&path)
            .unwrap()
            .execute("DELETE FROM schema_migrations WHERE migration_id = 2", [])
            .unwrap();

        let before = inspect_catalog(&path).unwrap();
        assert_eq!(
            before.current_schema_version,
            PROJECT_CATALOG_SCHEMA_VERSION
        );
        assert_eq!(
            before.schema_status,
            MigrationHistoryStatus::IncompatibleHistory
        );
        assert_eq!(
            before
                .pending_migrations
                .iter()
                .map(|migration| migration.id)
                .collect::<Vec<_>>(),
            vec![2]
        );
        let error = Catalog::open_existing(&path).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<CatalogSchemaError>(),
            Some(CatalogSchemaError::IncompatibleHistory {
                migration_id: 2,
                ..
            })
        ));
        assert!(format!("{error:#}").contains("migration history contains a gap"));

        let after = inspect_catalog(&path).unwrap();
        assert_eq!(after.applied_migrations, before.applied_migrations);
        assert_eq!(after.pending_migrations, before.pending_migrations);
        assert_eq!(
            after.schema_status,
            MigrationHistoryStatus::IncompatibleHistory
        );
    }

    #[test]
    fn normalizes_offset_datetimes() {
        assert_eq!(
            normalize_datetime("2026-07-18T09:00:00+09:00", -3600).as_deref(),
            Some("2026-07-17T23:00:00.000Z")
        );
        assert_eq!(
            normalize_datetime("2026:07:18 09:00:00", 60).as_deref(),
            Some("2026-07-18T09:01:00.000Z")
        );
        assert!(normalize_datetime("not a datetime", 0).is_none());
    }

    #[test]
    fn validates_projects_labels_rotation_and_missing_ids() {
        let (_directory, catalog, project) = catalog_fixture();
        assert!(catalog.create_project("  ").is_err());
        insert_asset(
            &catalog,
            &project.id,
            "asset_a",
            "A.MP4",
            "Camera A",
            "2026-07-18T00:00:00Z",
            Path::new("/missing/A.MP4"),
        );
        let current = catalog.get_asset("asset_a").unwrap();
        let result = catalog.update_asset(
            "asset_a",
            &AssetPatch {
                notes: current.notes.clone(),
                labels: vec!["x".repeat(65)],
                removed: false,
                rotation_degrees: current.rotation_degrees,
                expected_revision: None,
            },
        );
        assert!(result.is_err());
        assert!(catalog.get_asset("asset_missing").is_err());
        assert!(catalog.list_assets("project_missing").is_err());
    }

    #[test]
    fn persists_only_right_angle_orientation_overrides() {
        let (_directory, catalog, project) = catalog_fixture();
        insert_asset(
            &catalog,
            &project.id,
            "asset_a",
            "A.MP4",
            "Camera A",
            "2026-07-18T00:00:00Z",
            Path::new("/missing/A.MP4"),
        );
        let asset = catalog.get_asset("asset_a").unwrap();
        let updated = catalog
            .update_asset(
                &asset.id,
                &AssetPatch {
                    notes: asset.notes.clone(),
                    labels: asset.labels.clone(),
                    removed: asset.removed,
                    rotation_degrees: 180,
                    expected_revision: None,
                },
            )
            .unwrap();
        assert_eq!(updated.rotation_degrees, 180);
        assert_eq!(landscape_default_rotation(1080, 1920, 0), 90);
        assert_eq!(landscape_default_rotation(1920, 1080, 90), 180);
        assert!(
            catalog
                .update_asset(
                    &asset.id,
                    &AssetPatch {
                        notes: asset.notes,
                        labels: asset.labels,
                        removed: asset.removed,
                        rotation_degrees: 45,
                        expected_revision: None,
                    },
                )
                .is_err()
        );
    }

    #[test]
    fn orders_by_best_time_then_filename_and_counts_labeled_assets() {
        let (_directory, catalog, project) = catalog_fixture();
        for (id, filename, raw_time) in [
            ("asset_late", "B.MP4", "2026-07-18T03:00:00Z"),
            ("asset_tie_b", "D.MP4", "2026-07-18T02:00:00Z"),
            ("asset_tie_a", "C.MP4", "2026-07-18T02:00:00Z"),
        ] {
            insert_asset(
                &catalog,
                &project.id,
                id,
                filename,
                "Camera A",
                raw_time,
                Path::new("/missing/source.mp4"),
            );
        }
        let tie = catalog.get_asset("asset_tie_a").unwrap();
        catalog
            .update_asset(
                &tie.id,
                &AssetPatch {
                    notes: "labeled".to_string(),
                    labels: vec!["arrival".to_string()],
                    removed: false,
                    rotation_degrees: tie.rotation_degrees,
                    expected_revision: None,
                },
            )
            .unwrap();

        let filenames: Vec<_> = catalog
            .list_assets(&project.id)
            .unwrap()
            .into_iter()
            .map(|asset| asset.filename)
            .collect();
        assert_eq!(filenames, ["C.MP4", "D.MP4", "B.MP4"]);
        let summary = catalog.list_projects().unwrap().remove(0);
        assert_eq!(summary.asset_count, 3);
        assert_eq!(summary.labeled_count, 1);
        assert_eq!(summary.removed_count, 0);
    }

    #[test]
    fn clock_event_only_changes_matching_camera_at_and_after_boundary() {
        let (_directory, catalog, project) = catalog_fixture();
        for (id, filename, camera) in [
            ("a1", "C0001.MP4", "Camera A"),
            ("a2", "C0002.MP4", "Camera A"),
            ("a3", "C0003.MP4", "Camera A"),
            ("b2", "C0002.MP4", "Camera B"),
        ] {
            insert_asset(
                &catalog,
                &project.id,
                id,
                filename,
                camera,
                "2026-07-18T10:00:00Z",
                Path::new("/missing/source.mp4"),
            );
        }
        catalog
            .apply_clock_event(
                &project.id,
                "Camera A",
                "C0002.MP4",
                -3600,
                "timezone change",
            )
            .unwrap();

        assert_eq!(catalog.get_asset("a1").unwrap().offset_seconds, 0);
        for id in ["a2", "a3"] {
            let asset = catalog.get_asset(id).unwrap();
            assert_eq!(asset.offset_seconds, -3600);
            assert_eq!(asset.best_time.as_deref(), Some("2026-07-18T09:00:00.000Z"));
        }
        assert_eq!(catalog.get_asset("b2").unwrap().offset_seconds, 0);
    }

    #[test]
    fn clock_epochs_preserve_later_boundaries_when_earlier_events_are_added() {
        let (_directory, catalog, project) = catalog_fixture();
        for (id, filename) in [
            ("a1", "C0001.MP4"),
            ("a2", "C0002.MP4"),
            ("a3", "C0003.MP4"),
        ] {
            insert_asset(
                &catalog,
                &project.id,
                id,
                filename,
                "Camera A",
                "2026-07-18T10:00:00Z",
                Path::new("/missing/source.mp4"),
            );
        }
        catalog
            .apply_clock_event(
                &project.id,
                "Camera A",
                "C0003.MP4",
                -7200,
                "later boundary",
            )
            .unwrap();
        catalog
            .apply_clock_event(
                &project.id,
                "Camera A",
                "C0002.MP4",
                -3600,
                "earlier boundary added later",
            )
            .unwrap();
        assert_eq!(catalog.get_asset("a1").unwrap().offset_seconds, 0);
        assert_eq!(catalog.get_asset("a2").unwrap().offset_seconds, -3600);
        assert_eq!(catalog.get_asset("a3").unwrap().offset_seconds, -7200);

        catalog
            .apply_clock_event(
                &project.id,
                "Camera A",
                "C0002.MP4",
                -1800,
                "replacement at same boundary",
            )
            .unwrap();
        assert_eq!(catalog.get_asset("a2").unwrap().offset_seconds, -1800);
        assert_eq!(catalog.get_asset("a3").unwrap().offset_seconds, -7200);
    }

    #[test]
    fn proxy_only_asset_is_available_and_missing_proxy_is_reported() {
        let (directory, catalog, project) = catalog_fixture();
        insert_asset(
            &catalog,
            &project.id,
            "asset_offline",
            "OFFLINE.MP4",
            "Camera A",
            "2026-07-18T00:00:00Z",
            directory.path().join("missing.mp4").as_path(),
        );
        mark_asset_video_usable(&catalog, "asset_offline");
        assert!(!catalog.get_asset("asset_offline").unwrap().available);
        let report = catalog.generate_missing_proxies(&project.id).unwrap();
        assert_eq!(report.generated, 0);
        assert_eq!(report.failures.len(), 1);

        let proxy = directory.path().join("offline-proxy.mp4");
        fs::write(&proxy, b"proxy sentinel").unwrap();
        catalog
            .connection()
            .unwrap()
            .execute(
                "UPDATE assets SET proxy_path = ?2 WHERE id = ?1",
                params!["asset_offline", proxy.to_string_lossy()],
            )
            .unwrap();
        assert!(catalog.get_asset("asset_offline").unwrap().available);
        let report = catalog.generate_missing_proxies(&project.id).unwrap();
        assert_eq!(report.skipped, 1);
        assert!(report.failures.is_empty());
    }

    #[test]
    fn recognizes_media_extensions_and_parses_rates() {
        assert!(is_media(Path::new("clip.MP4")));
        assert!(is_media(Path::new("clip.m2ts")));
        assert!(!is_media(Path::new("notes.txt")));
        assert_eq!(
            proxy_representation("asset.v3.mp4"),
            ProxyRepresentation::RawV3
        );
        assert_eq!(
            proxy_representation("asset.v2.mp4"),
            ProxyRepresentation::MetadataTaggedRawV2
        );
        assert_eq!(
            proxy_representation("asset.mp4"),
            ProxyRepresentation::MetadataBakedV1
        );
        assert!((parse_rate("60000/1001").unwrap() - 59.94005994).abs() < 0.0001);
        assert_eq!(parse_rate("30/0"), None);
        assert_eq!(parse_rate("invalid"), None);
    }

    #[test]
    fn parses_ffmpeg_timestamp_progress_into_residual_byte_work() {
        assert_eq!(
            ffmpeg_progress_fraction("out_time_us=5000000", 10.0),
            Some(0.5)
        );
        assert_eq!(
            ffmpeg_progress_fraction("out_time_us=12000000", 10.0),
            Some(1.0)
        );
        assert_eq!(ffmpeg_progress_fraction("progress=continue", 10.0), None);
        assert_eq!(ffmpeg_progress_fraction("out_time_us=1", 0.0), None);
        assert_eq!(work_units_from_fraction(1_000, 0.25), 250);
        assert_eq!(
            residual_work_units(&[1_000, 500], &[false, true], &[250, 0]),
            vec![750]
        );
    }

    #[test]
    fn media_diagnostics_redact_paths_and_bound_the_redacted_text() {
        let stderr = "failure ".to_owned() + &"/a ".repeat(2_000);
        let diagnostic = redact_media_diagnostic(&stderr, Path::new("/a"), Path::new("/output"));
        assert!(diagnostic.len() <= 4096);
        assert!(!diagnostic.contains("/a"));
        assert!(diagnostic.contains("<source>"));

        let quoted = redact_media_diagnostic(
            "could not read '/private area/clip.mp4': invalid data",
            Path::new("/source"),
            Path::new("/output"),
        );
        assert!(!quoted.contains("/private area/clip.mp4"));
        assert!(quoted.contains("invalid data"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_proxy_command_uses_videotoolbox_with_quality_matched_settings() {
        let command = proxy_ffmpeg_command(
            Path::new("source.mp4"),
            Path::new("proxy.mp4"),
            ProxyEncoder::VideoToolbox,
        );
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(
            args.windows(2)
                .any(|pair| pair == ["-hwaccel", "videotoolbox"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-c:v", "h264_videotoolbox"])
        );
        assert!(args.windows(2).any(|pair| pair == ["-q:v", "36"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-power_efficient", "1"])
        );
        assert!(args.windows(2).any(|pair| pair == ["-map", "0:v:0"]));
        assert!(args.windows(2).any(|pair| pair == ["-map", "0:a:0?"]));
        assert!(args.iter().any(|arg| arg.starts_with("scale=")));
        assert!(!args.iter().any(|arg| arg.starts_with("scale_vt=")));
        assert!(args.windows(2).any(|pair| pair == ["-progress", "pipe:1"]));
        assert!(args.windows(2).any(|pair| pair == ["-stats_period", "1"]));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_missing_proxy_batch_uses_four_workers_and_commits_every_asset() {
        if tool_command("ffmpeg").arg("-version").output().is_err()
            || tool_command("ffprobe").arg("-version").output().is_err()
        {
            eprintln!("ffmpeg or ffprobe is unavailable; skipping media integration test");
            return;
        }
        let directory = tempdir().unwrap();
        let source_dir = directory.path().join("source");
        fs::create_dir_all(&source_dir).unwrap();
        for (index, color) in ["red", "green", "blue", "yellow"].into_iter().enumerate() {
            let source = source_dir.join(format!("C{index:04}.MP4"));
            let input = format!("color=c={color}:s=160x90:r=10:d=0.2");
            let status = tool_command("ffmpeg")
                .args([
                    "-y",
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &input,
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                ])
                .arg(source)
                .status()
                .unwrap();
            assert!(status.success());
        }

        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Parallel proxies").unwrap();
        let ingest = catalog.ingest_folder(&project.id, &source_dir).unwrap();
        assert_eq!(ingest.imported, 4);

        let mut progress = Vec::new();
        let report = catalog
            .generate_missing_proxies_with_progress(&project.id, |event| {
                progress.push(event);
                ControlFlow::Continue(())
            })
            .unwrap();

        assert_eq!(report.generated, 4);
        assert!(report.failures.is_empty());
        assert!(progress.len() >= 9);
        assert_eq!(progress[0].estimate.worker_count, 4);
        assert!(progress.iter().any(|event| {
            event.estimate.completed_items == 0 && event.estimate.completed_work_units > 0
        }));
        assert_eq!(
            catalog
                .proxy_rate_histories()
                .unwrap()
                .iter()
                .filter(|entry| entry.worker_count == 4)
                .map(|entry| entry.samples)
                .sum::<u64>(),
            4
        );
        assert_eq!(
            progress.last().unwrap().estimate.completed_items,
            progress.last().unwrap().estimate.total_items
        );
        assert!(
            catalog
                .list_assets(&project.id)
                .unwrap()
                .iter()
                .all(|asset| {
                    asset
                        .proxy_path
                        .as_deref()
                        .is_some_and(|path| Path::new(path).is_file())
                })
        );
    }

    #[test]
    fn a_cancelled_proxy_batch_keeps_what_it_finished_and_resumes() {
        if tool_command("ffmpeg").arg("-version").output().is_err()
            || tool_command("ffprobe").arg("-version").output().is_err()
        {
            eprintln!("ffmpeg or ffprobe is unavailable; skipping media integration test");
            return;
        }
        let directory = tempdir().unwrap();
        let source_dir = directory.path().join("source");
        fs::create_dir_all(&source_dir).unwrap();
        // More clips than workers, so stopping is observable: at the moment the
        // first clip completes only the in-flight claims can still finish.
        let clip_count = PROXY_WORKERS * 2;
        for index in 0..clip_count {
            let source = source_dir.join(format!("C{index:04}.MP4"));
            // Each clip needs distinct content: identical bytes share a
            // fingerprint and ingest would deduplicate them to one asset.
            let shade = 0x20 + index * 0x10;
            let input = format!("color=c=0x{shade:02x}{shade:02x}{shade:02x}:s=160x90:r=10:d=0.2");
            let status = tool_command("ffmpeg")
                .args([
                    "-y",
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &input,
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                ])
                .arg(source)
                .status()
                .unwrap();
            assert!(status.success());
        }

        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Cancelled proxies").unwrap();
        assert_eq!(
            catalog
                .ingest_folder(&project.id, &source_dir)
                .unwrap()
                .imported,
            clip_count
        );

        let report = catalog
            .generate_missing_proxies_with_progress(&project.id, |event| {
                // Stop once one clip has actually been committed, not on the
                // initial zero-progress event.
                if event.asset.is_some() {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            })
            .unwrap();

        assert!(report.cancelled);
        assert!(report.generated >= 1, "the committed clip is kept");
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        // How many clips land before the stop takes effect is deliberately not
        // asserted: workers claim independently of the collector thread that
        // observes the cancellation, so the exact cut-off is a race. What must
        // hold is that finished work is committed and the rest is still pending.

        // Re-running resumes rather than repeating.
        let resumed = catalog.generate_missing_proxies(&project.id).unwrap();
        assert_eq!(resumed.skipped, report.generated);
        assert_eq!(resumed.generated, clip_count - report.generated);
        assert!(!resumed.cancelled);
        assert!(
            catalog
                .list_assets(&project.id)
                .unwrap()
                .iter()
                .all(|asset| asset
                    .proxy_path
                    .as_deref()
                    .is_some_and(|path| Path::new(path).is_file()))
        );
    }

    #[test]
    fn a_silent_clip_reads_back_as_a_transcript_rather_than_as_nothing() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Transcript reads").unwrap();
        let connection = catalog.connection().unwrap();
        connection
            .execute(
                "INSERT INTO assets(id,project_id,fingerprint,path,filename,size_bytes,
                                    duration_seconds,created_at)
                 VALUES ('a1',?1,'f1','/tmp/a1.mp4','a1.mp4',1,1,'2026-08-04T00:00:00Z')",
                [&project.id],
            )
            .unwrap();

        assert!(catalog.asset_transcript("a1").unwrap().is_none());

        save_transcript(
            &connection,
            &StoredTranscript {
                asset_id: "a1".to_string(),
                recipe_key: STT_RECIPE_KEY.to_string(),
                source_fingerprint: "proxy-1".to_string(),
                status: TranscriptStatus::NoSpeech,
                outcome_reason: crate::analysis::store::TranscriptOutcomeReason::NoDetectedSpeech,
                reject_reason: String::new(),
                primary_language: String::new(),
                speech_seconds: 0.0,
                window_count: 0,
                provider_segment_count: 0,
                mean_token_probability: None,
                repetition_ratio: None,
                segments: Vec::new(),
            },
        )
        .unwrap();

        let transcript = catalog.asset_transcript("a1").unwrap().unwrap();
        assert_eq!(transcript.status, TranscriptStatus::NoSpeech);
        assert!(transcript.segments.is_empty());
        // Silence is a result. Returning None here would make a silent clip
        // indistinguishable from one that was never transcribed.
        assert!(catalog.asset_analysis("a1").unwrap().is_none());
    }

    #[test]
    fn readiness_projects_from_the_reference_measurement_until_a_project_has_history() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Readiness").unwrap();

        let readiness = catalog.analysis_readiness(&project.id).unwrap();
        assert_eq!(readiness.total_clips, 0);
        assert_eq!(readiness.missing_proxies, 0);
        assert_eq!(readiness.missing_transcripts, 0);
        assert_eq!(readiness.missing_analyses, 0);
        assert_eq!(readiness.cost_basis, AnalysisCostBasis::Reference);
        assert_eq!(
            readiness.observed_cost_per_clip_usd,
            REFERENCE_COST_PER_CLIP_USD
        );
        // Nothing to analyze means nothing to spend, whatever the per-clip rate.
        assert_eq!(readiness.estimated_cost_usd, 0.0);
        assert!(readiness.last_run.is_none());
        assert_eq!(readiness.models.len(), 2);
    }

    #[test]
    fn a_run_record_carries_its_spend_and_stop_reason() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Run accounting").unwrap();

        let run_id = catalog.begin_analysis_run(&project.id).unwrap();
        let running = catalog.analysis_runs(&project.id, 10).unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].status, "running");
        assert_eq!(running[0].finished_at, "");

        catalog
            .finish_analysis_run(
                &run_id,
                &AnalysisRunOutcome {
                    status: "cancelled".to_string(),
                    stage_reached: "analysis".to_string(),
                    proxies_generated: 3,
                    clips_transcribed: 2,
                    clips_analyzed: 1,
                    cost_usd: 0.25,
                    budget_exhausted: true,
                    error: String::new(),
                },
            )
            .unwrap();

        let finished = catalog.analysis_runs(&project.id, 10).unwrap();
        assert_eq!(finished.len(), 1, "finishing updates rather than appends");
        assert_eq!(finished[0].status, "cancelled");
        assert_eq!(finished[0].stage_reached, "analysis");
        assert_eq!(finished[0].proxies_generated, 3);
        assert_eq!(finished[0].clips_transcribed, 2);
        assert_eq!(finished[0].clips_analyzed, 1);
        assert_eq!(finished[0].cost_usd, 0.25);
        assert!(finished[0].budget_exhausted);
        assert!(!finished[0].finished_at.is_empty());

        let readiness = catalog.analysis_readiness(&project.id).unwrap();
        assert_eq!(readiness.last_run.unwrap().status, "cancelled");
    }

    #[test]
    fn runs_are_scoped_to_their_project() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let first = catalog.create_project("First").unwrap();
        let second = catalog.create_project("Second").unwrap();
        catalog.begin_analysis_run(&first.id).unwrap();
        assert_eq!(catalog.analysis_runs(&first.id, 10).unwrap().len(), 1);
        assert!(catalog.analysis_runs(&second.id, 10).unwrap().is_empty());
    }

    #[test]
    fn a_batch_cancelled_before_any_work_reports_no_generation() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Immediate cancel").unwrap();
        let report = catalog
            .generate_missing_proxies_with_progress(&project.id, |_| ControlFlow::Break(()))
            .unwrap();
        assert!(report.cancelled);
        assert_eq!(report.generated, 0);
    }

    fn synthetic_proxy_source(directory: &Path) -> PathBuf {
        let source_dir = directory.join("proxy-source");
        fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("CLIP.MP4");
        let status = tool_command("ffmpeg")
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
            .arg(&source)
            .status()
            .unwrap();
        assert!(status.success(), "could not create synthetic proxy source");
        source
    }

    #[test]
    fn auto_proxy_generation_records_actual_encoder_recipe_timing_and_validation() {
        if tool_command("ffmpeg").arg("-version").output().is_err()
            || tool_command("ffprobe").arg("-version").output().is_err()
        {
            eprintln!("ffmpeg or ffprobe unavailable; skipping proxy provenance test");
            return;
        }
        let (directory, catalog, project) = catalog_fixture();
        let source = synthetic_proxy_source(directory.path());
        let source_dir = source.parent().unwrap();
        catalog.ingest_folder(&project.id, source_dir).unwrap();
        let asset = catalog.list_assets(&project.id).unwrap().remove(0);
        let proxied = catalog
            .generate_proxy_with_policy(&asset.id, ProxyEncoderPolicy::Auto)
            .unwrap();
        assert_eq!(
            proxied.proxy_representation,
            Some(ProxyRepresentation::RawV3)
        );
        assert!(
            proxied
                .proxy_path
                .as_deref()
                .is_some_and(|path| Path::new(path).is_file())
        );

        let provenance = catalog.proxy_generation_provenance(&asset.id, 10).unwrap();
        assert_eq!(provenance.len(), 1);
        let latest = &provenance[0];
        assert_eq!(latest.status, "succeeded");
        assert!(
            latest.fallback_used
                || matches!(latest.selected_encoder.as_str(), "vaapi" | "videotoolbox")
        );
        let validation = latest.validation.as_ref().unwrap();
        assert!(validation.decoded);
        assert_eq!(validation.metadata_rotation_degrees, 0);
        assert_eq!(validation.audio_stream_count, 1);
        assert_eq!(validation.audio_codec.as_deref(), Some("aac"));
        assert!(
            latest
                .attempts
                .iter()
                .any(|attempt| attempt.status == ProxyAttemptStatus::Succeeded)
        );

        let histories = catalog.proxy_rate_histories().unwrap();
        let matching = histories
            .iter()
            .find(|history| {
                history.encoder == latest.selected_encoder
                    && history.task_kind.contains("proxy.h264-720p.orientation-v3")
            })
            .expect("actual encoder should have an encoder-aware rate history");
        assert_eq!(matching.samples, 1);
        assert!(matching.scope.contains("not a cross-corpus benchmark"));
    }

    #[cfg(unix)]
    #[test]
    fn both_encoder_failures_preserve_registered_proxy_and_record_failure() {
        use std::os::unix::fs::PermissionsExt;

        if tool_command("ffmpeg").arg("-version").output().is_err()
            || tool_command("ffprobe").arg("-version").output().is_err()
        {
            eprintln!("ffmpeg or ffprobe unavailable; skipping proxy failure provenance test");
            return;
        }
        let (directory, catalog, project) = catalog_fixture();
        let source = synthetic_proxy_source(directory.path());
        catalog
            .ingest_folder(&project.id, source.parent().unwrap())
            .unwrap();
        let asset = catalog.list_assets(&project.id).unwrap().remove(0);
        let first = catalog
            .generate_proxy_with_policy(&asset.id, ProxyEncoderPolicy::Software)
            .unwrap();
        let registered_path = PathBuf::from(first.proxy_path.as_deref().unwrap());
        let registered_bytes = fs::read(&registered_path).unwrap();

        let fake = directory.path().join("always-fail-ffmpeg");
        fs::write(
            &fake,
            "#!/bin/sh\necho 'synthetic hardware and software failure' >&2\nexit 1\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&fake).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake, permissions).unwrap();
        let plan = ProxyEncoderPlan {
            policy: ProxyEncoderPolicy::Auto,
            preferred_encoder: ProxyEncoderKind::Vaapi,
            targets: vec![
                ProxyEncoderTarget {
                    encoder: ProxyEncoderKind::Vaapi,
                    device: Some("/dev/dri/renderD128".to_string()),
                    recipe_key: PROXY_VAAPI_RECIPE_KEY.to_string(),
                },
                ProxyEncoderTarget {
                    encoder: ProxyEncoderKind::Software,
                    device: None,
                    recipe_key: PROXY_SOFTWARE_RECIPE_KEY.to_string(),
                },
            ],
            hardware_unavailable: None,
            software_usable: true,
            limitations: vec![],
        };
        let error = catalog
            .generate_proxy_untracked_with_execution(
                &asset.id,
                true,
                ProxyEncoderPolicy::Auto,
                |_| {},
                &fake,
                &resolve_tool_path("ffprobe"),
                Some(plan),
            )
            .unwrap_err();
        assert!(format!("{error:#}").contains("ffmpeg proxy generation failed"));

        let unchanged = catalog.get_asset(&asset.id).unwrap();
        assert_eq!(
            unchanged.proxy_path.as_deref(),
            Some(registered_path.to_string_lossy().as_ref())
        );
        assert_eq!(fs::read(&registered_path).unwrap(), registered_bytes);
        let provenance = catalog.proxy_generation_provenance(&asset.id, 10).unwrap();
        assert_eq!(provenance.len(), 2);
        assert_eq!(provenance[0].status, "failed");
        let registered = catalog
            .registered_proxy_generation_provenance(&asset.id)
            .unwrap()
            .unwrap();
        assert_eq!(registered.id, provenance[1].id);
        assert_eq!(registered.status, "succeeded");
        // A downloaded replacement has no locally generated encoder claim.
        let replacement = catalog.begin_proxy_write(&asset.id).unwrap().unwrap();
        fs::write(replacement.temporary_path(), &registered_bytes).unwrap();
        catalog.commit_proxy_write(replacement, None).unwrap();
        assert!(
            catalog
                .registered_proxy_generation_provenance(&asset.id)
                .unwrap()
                .is_none()
        );

        assert_eq!(provenance[0].attempts.len(), 2);
        assert!(
            provenance[0]
                .attempts
                .iter()
                .all(|attempt| { attempt.status == ProxyAttemptStatus::ProcessFailed })
        );
        assert!(provenance[0].validation.is_none());
        let proxy_directory = registered_path.parent().unwrap();
        assert!(
            fs::read_dir(proxy_directory)
                .unwrap()
                .filter_map(std::result::Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp.mp4"))
        );
    }

    #[test]
    fn queued_proxy_batch_cancels_before_encoder_probe_or_generation() {
        if tool_command("ffmpeg").arg("-version").output().is_err()
            || tool_command("ffprobe").arg("-version").output().is_err()
        {
            return;
        }
        let (directory, catalog, project) = catalog_fixture();
        let source = synthetic_proxy_source(directory.path());
        catalog
            .ingest_folder(&project.id, source.parent().unwrap())
            .unwrap();
        let asset = catalog.list_assets(&project.id).unwrap().remove(0);
        let _held = catalog.acquire_local_heavy().unwrap();
        let worker_catalog = catalog.clone();
        let project_id = project.id.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            sender
                .send(worker_catalog.generate_missing_proxies_with_progress(
                    &project_id,
                    |progress| {
                        assert_eq!(progress.estimate.confidence, "waiting_for_resource");
                        ControlFlow::Break(())
                    },
                ))
                .unwrap();
        });
        let report = receiver
            .recv_timeout(std::time::Duration::from_secs(3))
            .expect("queued cancellation must remain responsive")
            .unwrap();
        worker.join().unwrap();
        assert!(report.cancelled);
        assert_eq!(report.generated, 0);
        assert!(catalog.get_asset(&asset.id).unwrap().proxy_path.is_none());
        assert!(
            catalog
                .proxy_generation_provenance(&asset.id, 10)
                .unwrap()
                .is_empty()
        );
        assert!(catalog.proxy_rate_histories().unwrap().is_empty());
    }

    #[test]
    fn generated_proxy_commit_failure_restores_file_and_registered_provenance() {
        use std::io::Write;
        if tool_command("ffmpeg").arg("-version").output().is_err()
            || tool_command("ffprobe").arg("-version").output().is_err()
        {
            return;
        }
        let (directory, catalog, project) = catalog_fixture();
        let source = synthetic_proxy_source(directory.path());
        catalog
            .ingest_folder(&project.id, source.parent().unwrap())
            .unwrap();
        let asset = catalog.list_assets(&project.id).unwrap().remove(0);
        let generated = catalog
            .generate_proxy_with_policy(&asset.id, ProxyEncoderPolicy::Software)
            .unwrap();
        let path = PathBuf::from(generated.proxy_path.unwrap());
        // Distinguish the prior valid output from a deterministic regeneration.
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"previous_registered_proxy_marker")
            .unwrap();
        let old_bytes = fs::read(&path).unwrap();
        let old_provenance = catalog
            .registered_proxy_generation_provenance(&asset.id)
            .unwrap()
            .unwrap();
        // Fail at COMMIT, after the staging file has been atomically published.
        catalog
            .connection()
            .unwrap()
            .execute_batch(
                "CREATE TABLE proxy_commit_failure (
                asset_id TEXT REFERENCES assets(id) DEFERRABLE INITIALLY DEFERRED);
             CREATE TRIGGER fail_proxy_commit AFTER INSERT ON proxy_generation_provenance
             BEGIN INSERT INTO proxy_commit_failure VALUES ('missing-asset'); END;",
            )
            .unwrap();
        let error = catalog
            .generate_proxy_with_policy(&asset.id, ProxyEncoderPolicy::Software)
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("register generated proxy"),
            "{error:#}"
        );
        assert_eq!(fs::read(&path).unwrap(), old_bytes);
        assert_eq!(
            catalog
                .registered_proxy_generation_provenance(&asset.id)
                .unwrap()
                .unwrap()
                .id,
            old_provenance.id
        );
        assert_eq!(
            catalog
                .proxy_generation_provenance(&asset.id, 20)
                .unwrap()
                .len(),
            1
        );
        assert!(fs::read_dir(path.parent().unwrap()).unwrap().all(|entry| {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            !name.contains("commit-backup") && !name.ends_with(".tmp.mp4")
        }));
    }

    #[test]
    fn proxy_v3_clears_the_source_display_rotation() {
        if tool_command("ffmpeg").arg("-version").output().is_err()
            || tool_command("ffprobe").arg("-version").output().is_err()
        {
            eprintln!("ffmpeg or ffprobe is unavailable; skipping media integration test");
            return;
        }
        let directory = tempdir().unwrap();
        let source_dir = directory.path().join("source");
        fs::create_dir_all(&source_dir).unwrap();
        let unrotated = directory.path().join("unrotated.mp4");
        let source = source_dir.join("ROTATED.MP4");
        let create_status = tool_command("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=navy:s=640x360:r=30:d=1",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&unrotated)
            .status()
            .unwrap();
        assert!(create_status.success());
        let rotation_status = tool_command("ffmpeg")
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
            .arg(&source)
            .status()
            .unwrap();
        assert!(rotation_status.success());
        assert_eq!(
            probe_media_stream(&source)
                .unwrap()
                .metadata_rotation_degrees,
            90
        );

        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Rotated source").unwrap();
        catalog.ingest_folder(&project.id, &source_dir).unwrap();
        let asset = catalog.list_assets(&project.id).unwrap().remove(0);
        let proxied = catalog.generate_proxy(&asset.id).unwrap();
        let proxy_path = PathBuf::from(proxied.proxy_path.as_deref().unwrap());

        assert!(proxy_path.ends_with(format!("{}.v3.mp4", asset.id)));
        assert_eq!(
            proxied.proxy_representation,
            Some(ProxyRepresentation::RawV3)
        );
        let proxy_probe = probe_media_stream(proxy_path).unwrap();
        assert_eq!(proxy_probe.metadata_rotation_degrees, 0);
        assert_eq!((proxy_probe.width, proxy_probe.height), (640, 360));
    }

    #[test]
    fn camera_proxy_discovery_is_card_scoped_and_registration_is_separate() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Synthetic sidecar fixture").unwrap();
        let mut asset_ids = Vec::new();
        let mut sidecars = Vec::new();
        for (card, color) in [("card-a", "red"), ("card-b", "blue")] {
            let root = directory.path().join(card);
            let dcim = root.join("DCIM");
            fs::create_dir_all(&dcim).unwrap();
            let original = dcim.join("CLIP0001.MP4");
            let sidecar = dcim.join("CLIP0001.LRF");
            for (target, size, audio) in [(&original, "640x360", true), (&sidecar, "320x180", true)]
            {
                let mut cmd = tool_command("ffmpeg");
                cmd.args([
                    "-y",
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("color=c={color}:s={size}:r=24:d=2"),
                ]);
                if audio {
                    cmd.args(["-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo"]);
                }
                cmd.args([
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                    "-c:a",
                    "aac",
                    "-shortest",
                    "-f",
                    "mp4",
                ])
                .arg(target);
                assert!(cmd.status().unwrap().success());
            }
            catalog.ingest_folder(&project.id, &root).unwrap();
            let asset = catalog
                .list_assets(&project.id)
                .unwrap()
                .into_iter()
                .find(|a| a.path == original.to_string_lossy())
                .unwrap();
            asset_ids.push(asset.id);
            sidecars.push((root, sidecar, original));
        }
        assert_ne!(asset_ids[0], asset_ids[1]);
        for i in 0..2 {
            let before = fs::read(&sidecars[i].1).unwrap();
            let original_before = fs::read(&sidecars[i].2).unwrap();
            let found = catalog
                .discover_camera_proxy_sidecars(&project.id, &sidecars[i].0)
                .unwrap();
            assert_eq!(
                found
                    .iter()
                    .filter(|x| x.asset_id == asset_ids[i] && x.status == "candidate")
                    .count(),
                1,
                "{found:?}"
            );
            assert!(found.iter().all(|x| x.asset_id == asset_ids[i]));
            let registered = catalog
                .register_camera_proxy_sidecar(&asset_ids[i], &sidecars[i].0, &sidecars[i].1)
                .unwrap();
            assert_eq!(registered.status, "registered");
            assert_eq!(
                catalog
                    .get_asset(&asset_ids[i])
                    .unwrap()
                    .proxy_representation,
                None
            );
            assert_eq!(
                catalog
                    .camera_proxy_sidecar(&asset_ids[i])
                    .unwrap()
                    .unwrap()
                    .status,
                "registered"
            );
            fs::remove_file(&sidecars[i].2).unwrap();
            let offline_asset = catalog.get_asset(&asset_ids[i]).unwrap();
            assert!(offline_asset.camera_proxy_available);
            assert!(offline_asset.available);
            let offline = catalog
                .resolve_camera_proxy_for_review(&asset_ids[i])
                .unwrap()
                .unwrap();
            assert_eq!(offline.path, sidecars[i].1.to_string_lossy());
            fs::write(&sidecars[i].2, &original_before).unwrap();
            let mut changed = before.clone();
            changed.push(0);
            fs::write(&sidecars[i].1, &changed).unwrap();
            assert_eq!(
                catalog
                    .camera_proxy_sidecar(&asset_ids[i])
                    .unwrap()
                    .unwrap()
                    .status,
                "stale"
            );
            fs::write(&sidecars[i].1, &before[..before.len() / 2]).unwrap();
            assert!(
                catalog
                    .register_camera_proxy_sidecar(&asset_ids[i], &sidecars[i].0, &sidecars[i].1)
                    .is_err()
            );
            fs::write(&sidecars[i].1, &before).unwrap();
            assert_eq!(fs::read(&sidecars[i].1).unwrap(), before);
            assert_eq!(fs::read(&sidecars[i].2).unwrap(), original_before);
        }
        let wrong_card = catalog
            .discover_camera_proxy_sidecars(&project.id, &sidecars[0].0)
            .unwrap();
        assert!(wrong_card.iter().all(|x| x.asset_id != asset_ids[1]));
    }

    #[test]
    fn media_round_trip_ingest_labels_proxy_notes_and_clock() {
        if tool_command("ffmpeg").arg("-version").output().is_err() {
            eprintln!("ffmpeg is unavailable; skipping media integration test");
            return;
        }
        let directory = tempdir().unwrap();
        let source_dir = directory.path().join("source");
        fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("C0001.MP4");
        let status = tool_command("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=navy:s=640x360:r=30:d=1",
                "-metadata",
                "creation_time=2026-07-18T09:00:00+09:00",
                "-c:v",
                "mpeg4",
            ])
            .arg(&source)
            .status()
            .unwrap();
        assert!(status.success());

        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Round trip").unwrap();
        let report = catalog.ingest_folder(&project.id, &source_dir).unwrap();
        assert_eq!(report.imported, 1);
        let ingest_operations = catalog.list_operations(&project.id).unwrap();
        assert_eq!(ingest_operations.len(), 1);
        assert_eq!(
            ingest_operations[0].operation_type,
            operations::PROJECT_ASSET_ADDED
        );
        assert!(ingest_operations[0].payload.get("path").is_none());

        fs::copy(&source, source_dir.join("COPY.MP4")).unwrap();
        fs::write(source_dir.join("README.txt"), b"ignored").unwrap();
        let mut progress = Vec::new();
        let duplicate_report = catalog
            .ingest_folder_with_progress(&project.id, &source_dir, |completed, total, path| {
                progress.push((completed, total, path.to_path_buf()));
            })
            .unwrap();
        assert_eq!(duplicate_report.scanned, 2);
        assert_eq!(duplicate_report.imported, 0);
        assert_eq!(duplicate_report.duplicates, 2);
        assert_eq!(progress.len(), 2);
        assert_eq!(progress.last().map(|item| (item.0, item.1)), Some((2, 2)));

        let asset = catalog.list_assets(&project.id).unwrap().remove(0);
        let updated = catalog
            .update_asset(
                &asset.id,
                &AssetPatch {
                    notes: "Opening station view".to_string(),
                    labels: vec!["station".to_string(), "opening".to_string()],
                    removed: false,
                    rotation_degrees: asset.rotation_degrees,
                    expected_revision: None,
                },
            )
            .unwrap();
        assert_eq!(updated.notes, "Opening station view");
        assert_eq!(updated.labels, ["station", "opening"]);

        catalog
            .apply_clock_event(
                &project.id,
                &asset.camera,
                &asset.filename,
                -3600,
                "camera was one hour fast",
            )
            .unwrap();
        let corrected = catalog.get_asset(&asset.id).unwrap();
        assert_eq!(corrected.offset_seconds, -3600);

        let mut proxy_progress = Vec::new();
        let first_proxy_report = catalog
            .generate_missing_proxies_with_policy_and_progress(
                &project.id,
                ProxyEncoderPolicy::Software,
                |progress| {
                    proxy_progress.push(progress);
                    ControlFlow::Continue(())
                },
            )
            .unwrap();
        assert_eq!(first_proxy_report.generated, 1);
        assert!(proxy_progress.len() >= 3);
        assert_eq!(proxy_progress[0].estimate.completed_items, 0);
        assert!(proxy_progress.iter().any(|progress| {
            progress.estimate.completed_items == 0 && progress.estimate.completed_work_units > 0
        }));
        let completed_proxy_progress = proxy_progress.last().unwrap();
        assert_eq!(completed_proxy_progress.estimate.completed_items, 1);
        assert_eq!(completed_proxy_progress.estimate.total_items, 1);
        assert_eq!(completed_proxy_progress.estimate.worker_count, 1);
        assert_eq!(
            completed_proxy_progress
                .estimate
                .estimated_remaining_seconds,
            Some(0.0)
        );
        let first_encoder = ProxyEncoderKind::parse(
            &catalog
                .proxy_generation_provenance(&asset.id, 1)
                .unwrap()
                .remove(0)
                .selected_encoder,
        )
        .unwrap();
        let first_history_key = catalog
            .qualified_history_key(
                &proxy_task_kind_for_encoder(first_encoder, 1),
                PROXY_REPRESENTATION_KIND,
                PROXY_SOFTWARE_RECIPE_KEY,
                Some(first_encoder.as_str()),
                1,
                0,
                0,
            )
            .unwrap();
        assert_eq!(
            catalog
                .load_batch_history(&first_history_key)
                .unwrap()
                .samples,
            1
        );
        let proxied = completed_proxy_progress.asset.as_ref().unwrap();
        assert!(Path::new(proxied.proxy_path.as_deref().unwrap()).exists());
        assert!(proxied.proxy_path.as_deref().unwrap().ends_with(".v3.mp4"));
        assert_eq!(
            proxied.proxy_representation,
            Some(ProxyRepresentation::RawV3)
        );
        assert_eq!(
            probe_media_stream(proxied.proxy_path.as_deref().unwrap())
                .unwrap()
                .metadata_rotation_degrees,
            0
        );
        let proxy_report = catalog.generate_missing_proxies(&project.id).unwrap();
        assert_eq!(proxy_report.generated, 0);
        assert_eq!(proxy_report.skipped, 1);
        fs::remove_file(proxied.proxy_path.as_deref().unwrap()).unwrap();
        let mut single_proxy_progress = Vec::new();
        let regenerated_proxy = catalog
            .generate_proxy_with_policy_and_progress(
                &asset.id,
                ProxyEncoderPolicy::Software,
                |progress| {
                    single_proxy_progress.push(progress);
                },
            )
            .unwrap();
        assert!(single_proxy_progress.len() >= 3);
        assert!(
            single_proxy_progress[0]
                .estimate
                .estimated_remaining_seconds
                .is_some()
        );
        assert_eq!(
            single_proxy_progress
                .last()
                .unwrap()
                .estimate
                .estimated_remaining_seconds,
            Some(0.0)
        );
        assert!(Path::new(regenerated_proxy.proxy_path.as_deref().unwrap()).is_file());
        let second_encoder = ProxyEncoderKind::parse(
            &catalog
                .proxy_generation_provenance(&asset.id, 1)
                .unwrap()
                .remove(0)
                .selected_encoder,
        )
        .unwrap();
        let second_history_key = catalog
            .qualified_history_key(
                &proxy_task_kind_for_encoder(second_encoder, 1),
                PROXY_REPRESENTATION_KIND,
                PROXY_SOFTWARE_RECIPE_KEY,
                Some(second_encoder.as_str()),
                1,
                0,
                0,
            )
            .unwrap();
        assert_eq!(
            catalog
                .load_batch_history(&second_history_key)
                .unwrap()
                .samples,
            2
        );

        assert!(catalog.generate_thumbnail(&asset.id, 10).is_err());
        let thumbnail = catalog.generate_thumbnail(&asset.id, 1).unwrap();
        let thumbnail_path = PathBuf::from(thumbnail.thumbnail_path.as_deref().unwrap());
        assert!(thumbnail_path.is_file());
        assert_eq!(thumbnail.thumbnail_quality, 1);
        assert!(thumbnail_path.ends_with(format!("{}/q1.v2.jpg", asset.id)));
        fs::remove_file(&thumbnail_path).unwrap();
        fs::rename(&source, source.with_extension("offline")).unwrap();
        let mut thumbnail_progress = Vec::new();
        let regenerated = catalog
            .generate_missing_thumbnails_with_progress(&project.id, 1, |progress| {
                thumbnail_progress.push(progress);
            })
            .unwrap();
        assert_eq!(regenerated.generated, 1);
        assert_eq!(thumbnail_progress.len(), 2);
        assert_eq!(thumbnail_progress[0].estimate.completed_items, 0);
        assert_eq!(thumbnail_progress[1].estimate.completed_items, 1);
        assert_eq!(
            thumbnail_progress[1].estimate.completed_work_units,
            asset.size_bytes
        );
        assert_eq!(
            catalog
                .load_batch_history(THUMBNAIL_TASK_KIND)
                .unwrap()
                .samples,
            1
        );
        let from_proxy = thumbnail_progress[1].asset.as_ref().unwrap();
        assert!(Path::new(from_proxy.thumbnail_path.as_deref().unwrap()).is_file());
        let thumbnail_report = catalog.generate_missing_thumbnails(&project.id, 1).unwrap();
        assert_eq!(thumbnail_report.generated, 0);
        assert_eq!(thumbnail_report.skipped, 1);

        // A targeted import tail must not sweep unrelated old Q0 assets.
        insert_asset(
            &catalog,
            &project.id,
            "asset_old_offline",
            "OLD.MP4",
            "Sony",
            "2026-07-18T10:00:00Z",
            &directory.path().join("offline/OLD.MP4"),
        );
        fs::remove_file(&thumbnail_path).unwrap();
        let targeted = catalog
            .generate_thumbnails_for_assets(&[asset.id.clone(), asset.id.clone()], 1)
            .unwrap();
        assert_eq!(targeted.generated, 1, "duplicate target ids are coalesced");
        assert!(targeted.failures.is_empty());
        assert_eq!(
            catalog
                .get_asset("asset_old_offline")
                .unwrap()
                .thumbnail_quality,
            0,
            "an unrelated offline asset is never swept"
        );

        // Without a current proxy, fallback must verify original identity rather
        // than trusting that the old pathname happens to exist again.
        fs::remove_file(&thumbnail_path).unwrap();
        fs::remove_file(regenerated_proxy.proxy_path.as_deref().unwrap()).unwrap();
        fs::write(&source, b"different footage at a reused card path").unwrap();
        let mismatch = catalog
            .generate_thumbnails_for_assets(std::slice::from_ref(&asset.id), 1)
            .unwrap();
        assert_eq!(mismatch.generated, 0);
        assert_eq!(mismatch.failures.len(), 1);
        assert!(
            mismatch.failures[0]
                .error
                .contains("not this clip any more")
        );
    }
}
