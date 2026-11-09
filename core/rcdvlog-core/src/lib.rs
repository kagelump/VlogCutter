use anyhow::{Context, Result, anyhow};
use chrono::{SecondsFormat, Utc};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub mod acoustic_markers;
mod agent;
mod analysis;
mod archive;
mod baseline;
mod calibration;
mod camera_proxy;
mod cameras;
mod cards;
mod catalog;
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
mod media;
mod migrations;
mod operations;
mod pipeline_smoke;
mod preflight;
mod preparation;
mod prerequisites;
mod proxy_encoding;
mod representation;
mod resources;
mod stall;
mod storage;
mod transcription;
mod types;
mod volumes;
pub mod youtube;

pub use analysis::accounting::{
    ANALYSIS_ACCOUNTING_SCHEMA_VERSION, ANALYSIS_RECOVERY_SCHEMA_VERSION,
    AccountedNativeSttProvider, AccountedVlmProvider, AnalysisAccountingAggregate,
    AnalysisAccountingContext, AnalysisAccountingInspection, AnalysisAccountingInspectionSelection,
    AnalysisAccountingReport, AnalysisAccountingRequestInspection, AnalysisBudgetAuthorityContext,
    AnalysisBudgetExhausted, AnalysisBudgetSnapshot, AnalysisOperationAccountingSummary,
    AnalysisOperationAttemptProjection, AnalysisProviderReceipt, AnalysisRequestEvent,
    AnalysisRequestLifecycle, AnalysisRequestReconciliation, AnalysisRequestRecord,
    AnalysisRequestRecoveryReport, AnalysisRetryAuthorization, AnalysisTargetedRetryAdmission,
    CHRONOLOGY_SUGGESTIONS_OPERATION, LABEL_CONSOLIDATION_OPERATION,
    LEGACY_UNKNOWN_USAGE_STOP_REASON, MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY,
    MAX_PROVIDER_RECEIPT_EVIDENCE_BYTES, MAX_REQUEST_ATTEMPTS, MAX_TARGETED_RETRY_PAYLOAD_BYTES,
    NativeSttAccountedResult, ProviderReceiptEvidence, TargetedAnalysisRetryProviderResponse,
    TargetedAnalysisRetryReport, UNTRACKED_POST_INITIALIZATION_USAGE_STOP_REASON,
    analysis_native_stt_payload_fingerprint, analysis_operation_budget_scope_id,
    analysis_policy_budget_scope_id, analysis_request_cost_bound_fingerprint,
    analysis_request_payload_fingerprint, bounded_failure_code,
    read_provider_receipt_evidence_file, read_targeted_retry_payload_file,
};
pub use analysis::chronology_suggestions::{
    ChronologyEvidenceRequest, ChronologySuggestion, EvidenceFrame, PairMatch,
    SuggestionProvenance, SuggestionResult, SuggestionReview, review_suggestion,
    review_suggestion_against_facts,
};
pub use analysis::consolidate::{ConsolidationReport, LabelMapping, LabelProposal};
pub use analysis::framepack::{FramePackManifest, build_frame_pack, encode_frames};
pub use analysis::moments;
/// Narrow facade over the shared speech pipeline for consumers outside this
/// crate (the companion server's review-session transcription, #224). It
/// exposes the engine seam, VAD, window packing/re-split, quality gates and the
/// strict native transport; it does not expose the source-asset transcript store.
pub mod review_speech {
    pub use crate::analysis::coverage::{needs_resplit, resplit};
    pub use crate::analysis::engine::{
        ModelAsset, SttEngine, VAD_MODEL, WHISPER_MODEL, install_model, shared_model_dir,
    };
    pub use crate::analysis::openrouter::resolve_api_key;
    pub use crate::analysis::openrouter_stt::{
        NATIVE_STT_MODELS, NativeSttAudioWindow, NativeSttCallResult, NativeSttCost,
        NativeSttOutcome, NativeSttProviderPolicy, NativeSttReceipt, NativeSttRequest,
        NativeSttSegment, NativeSttTranscript, NativeSttTransport, NativeSttUsage,
        OPENROUTER_STT_PROVIDER, OPENROUTER_WHISPER_MODEL, OpenRouterSttClient, SttDataCollection,
        resolve_native_stt_model, validate_iso639_1,
    };
    pub use crate::analysis::quality::{DecodedSegment, RejectReason, reject_reason};
    pub use crate::analysis::silero_vad::detect_silero_speech;
    pub use crate::analysis::whisper_engine::WhisperEngine;
    pub use crate::analysis::windows::{
        PackedRegion, PackedWindow, SpeechRegion, pack_windows, translate_span,
    };
}
pub use analysis::openrouter::{
    OpenRouterProvider, api_credential_present, frame_data_uri, key_file_path,
    openrouter_request_cost_bound, resolve_api_key,
};
pub use analysis::openrouter_stt::{
    MAX_NATIVE_STT_RESPONSE_BYTES, NATIVE_STT_COST_BOUND_CONTRACT_VERSION, NATIVE_STT_MODELS,
    NATIVE_STT_REQUEST_TIMEOUT, NativeSttAudioWindow, NativeSttCallResult, NativeSttClientError,
    NativeSttCost, NativeSttModelSpec, NativeSttOutcome, NativeSttProtocolError,
    NativeSttProviderPolicy, NativeSttReceipt, NativeSttRequest, NativeSttSegment,
    NativeSttTranscript, NativeSttTransport, NativeSttUsage, NativeSttWord,
    OPENROUTER_STT_ENDPOINT, OPENROUTER_STT_PROVIDER, OPENROUTER_WHISPER_MODEL,
    OpenRouterSttClient, ResolvedSpeechBackend, SpeechBackendSelection, SttDataCollection,
    build_native_stt_request_body, decode_native_stt_body, decode_native_stt_value,
    native_stt_cost_bound_contract_id, native_stt_request_cost_bound, parse_native_stt_receipt,
    plan_native_stt_audio_windows, resolve_native_stt_model, resolve_speech_backend,
    seconds_to_window_ms, validate_iso639_1,
};
pub use analysis::provider::{
    ANALYSIS_COST_BOUND_CONTRACT_VERSION, AnalysisAudioCostBound, AnalysisRequestCostBound,
    ChainOutcome, DEFAULT_MODEL, FALLBACK_MODEL, MODELS, ModelSpec, OPENROUTER_MAX_OUTPUT_TOKENS,
    ProviderChain, ProviderFailure, ProviderFailureCategory, ProviderResponseRejection,
    VlmProvider, VlmReceipt, VlmRequest, VlmResponse, analysis_cost_bound_contract_id, model_spec,
};
pub use analysis::run_policy::{
    AnalysisPayload, AnalysisRunKind, AuthorizedPolicyState, CapabilityCheck, CapabilityStatus,
    MAX_NATIVE_PILOT_LOCAL_OUTCOMES, NATIVE_PILOT_EVIDENCE_SCHEMA_VERSION,
    NATIVE_SPEECH_BATCH_SCHEMA_VERSION, NativePilotEvidence, NativePilotLocalOutcome,
    NativePilotLocalOutcomeFact, NativePilotPaidSample, NativePilotSampleCoverage,
    NativePilotSampleOutcome, NativeSpeechAssetOutcome, NativeSpeechBatchFailure,
    NativeSpeechBatchProgress, NativeSpeechBatchReport, PilotValidation,
    RUN_AUTHORIZATION_SCHEMA_VERSION, RUN_PILOT_SCHEMA_VERSION, RUN_POLICY_SCHEMA_VERSION,
    RUN_POLICY_TOOL_VERSION, RunAssetScope, RunAuthorization, RunPilotRecord, RunPolicy,
    RunPolicyBlocker, RunPolicyContext, RunPolicyParameters, RunPolicyReadiness, RunPolicyRequest,
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
pub use pipeline_smoke::{
    PIPELINE_SMOKE_RECIPE_VERSION, PipelineSmokeCheck, PipelineSmokeIdentity, PipelineSmokeReport,
};

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
    install_speech_models_with_progress_for, refresh_cli_binding, speech_model_status,
    speech_model_status_for,
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
pub use analysis::quality::native_stt::{
    NATIVE_STT_AVG_LOGPROB_LN_0_20_REJECT_THRESHOLD,
    NATIVE_STT_PROVIDER_CONFIDENCE_REJECT_THRESHOLD, NATIVE_STT_QUALITY_RECIPE_VERSION,
    NativeSttMetricSummary, NativeSttQualityDecision, NativeSttQualityEvaluation,
    NativeSttQualityMetrics, NativeSttQualityReason, NativeSttWordConfidenceCoverage,
    evaluate_native_stt_quality,
};
pub use analysis::schema::{
    CameraMovement, ClipObservation, DomainError, UnusableReason, output_schema,
    validate as validate_observation,
};
pub use analysis::store::{
    NATIVE_TRANSCRIPT_PROVENANCE_SCHEMA_VERSION, NATIVE_TRANSCRIPT_WINDOW_PLAN_SCHEMA_VERSION,
    NativeSttWindowObservation, NativeSttWindowQualityObservation, NativeTranscriptCoverage,
    NativeTranscriptProvenance, NativeTranscriptWindowPlan, NativeTranscriptWindowProvenance,
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
pub use editplan::audio_integrity::*;
pub use editplan::audio_sample_plan::*;
pub use editplan::comparison::*;
pub use editplan::context_review::*;
pub use editplan::coverage::*;
pub use editplan::delivery::*;
pub use editplan::feedback::*;
pub use editplan::historical_review::*;
pub use editplan::inspection_review::*;
pub use editplan::patch::*;
pub use editplan::review_coverage::*;
pub use editplan::review_target::*;
pub use editplan::selection::*;
pub use editplan::{
    ConfirmSourceRangeRequest, ConfirmedRangeInspection, ConfirmedSourceRange, EditPlanAudio,
    EditPlanError, EditPlanExport, EditPlanOperation, EditPlanRevision, EditPlanStaleness,
    EditPlanStoryPolicy, Finding, FindingKind, InspectSourceWithPolicyRequest,
    InspectSourceWithPolicyResult, InspectedAudio, InspectedFrame, InspectedUtterance,
    InspectionAttempt, InspectionFile, InspectionPurpose, InspectionRepresentation,
    InspectionTimeMapping, InspectionWaveformBin, OtioExportError, OtioExportRequest,
    OtioExportResult, PreviewArtifact, PreviewArtifactError, PreviewArtifactPage,
    PreviewArtifactState, PreviewArtifactStatus, PreviewArtifactSummary,
    PreviewArtifactVerification, PreviewCutMapping, PreviewCutOutcome, PreviewExcerptRequest,
    PreviewExcerptScope, PreviewFailure, PreviewInputIdentity, PreviewMediaValidation,
    PreviewProgress, PreviewRequest, PreviewResult, PreviewSegmentCounts, PreviewSourceIdentity,
    ProtectedStoryGroup, RangeDecision, RangeDecisionOwner, RangeMedia, Severity, SourceAudio,
    SourceInspection, SourceInspectionError, SourceInspectionRequest, SourceInspectionResult,
    SourceRangeConstraints, SourceRangeError, SourceTimeRange, SourceTrimRequest,
    SourceTrimSuggestion, StoryChronologyException, StoryCutIdentity, StoryDecision,
    StoryDecisionActor, StoryExceptionScope, StoryOpeningTeaser, StoryOrderEdge, TrimMedia,
    ValidationReport, otio_document,
};
pub use operations::{
    ApplyOperationResult, AssetRevisionDiagnostics, CAPTURE_SEQUENCE_DECLARED,
    CAPTURE_SEQUENCE_MERGED, CAPTURE_SEQUENCE_REASSIGNED, CAPTURE_SEQUENCE_SPLIT,
    CAPTURE_SEQUENCE_SPLIT_EXACT, CLIP_TIME_EXCEPTION_CLEARED, CLIP_TIME_EXCEPTION_SET,
    CLOCK_EPOCH_CLEARED, CLOCK_EPOCH_SET, CLOCK_EVENT_CREATED, ConflictProjection,
    EDIT_PLAN_EXPORT_RECORDED, EDIT_PLAN_REVISION_ACTIVATED, EDIT_PLAN_REVISION_CREATED,
    EVENT_ORDER_CONSTRAINT_CLEARED, EVENT_ORDER_CONSTRAINT_SET, HybridLogicalTime, LABEL_ADDED,
    LABEL_REMOVED, MAX_REMOTE_HLC_FUTURE_SKEW_MILLIS, NOTE_REVISION_CREATED,
    NoteRevisionProjection, ORIENTATION_DECISION_SET, OperationBatchResult, OperationDiagnostics,
    OrientationDecision, PROJECT_ASSET_ADDED, PROJECT_ASSET_MEDIA_LINKED, ProjectAssetAddedPayload,
    ProjectAssetItem, ProjectAssetPage, ProjectOperation, REMOVED_SET, ROTATION_SET,
    STALE_ASSET_REVISION, SequenceRecoveryProjection, SequenceRecoveryState, TRIP_PHASE_REMOVED,
    TRIP_PHASE_SET, operation_affects_chronology_projection,
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

pub use catalog::*;
pub use media::*;
pub use types::*;

pub(crate) fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
pub(crate) mod tests;

pub use analysis::native_video::*;

pub mod source_audio_pooled;
pub mod source_audio_segments;
pub mod source_audio_sessions;
pub mod tv_review_sync;

pub mod tv_soundtrack;
pub mod tv_soundtrack_alignment;
pub mod tv_soundtrack_consensus;
pub mod tv_soundtrack_landmarks;
pub mod tv_soundtrack_session;
