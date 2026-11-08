mod audit;
pub mod auth;
mod backup;
mod catalog;
mod config;
mod error_codes;
mod foundation;
mod job_callbacks;
mod jobs;
mod library;
pub mod library_query;
mod media_facts;
mod replicas;
mod representations;
mod review_discussions;
mod review_sessions;
mod review_sync;
mod review_targets;
mod review_transcripts;
mod scan;
pub mod scan_handler;
mod search_index;
mod storage_path;

pub use rcdvlog_core::review_speech;
pub use rcdvlog_core::{
    BindingVerificationStatus, Catalog, CatalogSchemaError, GlobalMediaVerificationState,
    HistoricalRenderedArtifactReview, HistoricalReviewPurpose, HistoricalReviewUnresolved, JobId,
    MediaObjectId, NodeAvailabilityState, NodeId, PROJECT_CATALOG_SCHEMA_VERSION,
    ProjectAssetAddedPayload, ProjectAssetItem, ProjectAssetPage, ProjectId, RepresentationId,
    ReviewTargetHandoff, ReviewTargetSnapshot, RootAssociationConfirmation,
    RootAssociationConfirmationStatus, RootAssociationEvidence, RootRelativePath, StorageObjectId,
    StorageProtocol, StorageRootId, default_local_heavy_lock_path,
};
pub use rcdvlog_sync::{
    BASELINE_VERSION, BaselineEnvelope, BaselinePayload, BaselinePreview, NodeTargetedJob,
    ObservationFact, ProtocolRange, ReconcileRequest, ReconcileResponse,
    RepresentationManifestFact, SyncError, apply_reconcile_operations,
};

pub use auth::{
    AuthConfig, AuthDiagnostics, AuthError, AuthService, BootstrapCode, DeviceCredentialCreated,
    DeviceCredentialInfo, PairingCode, SessionCreated, SessionInfo,
};
pub use backup::{
    BACKUP_FORMAT_VERSION, BACKUP_JOB_TYPE, BACKUP_MANIFEST_FILENAME, BACKUP_RECIPE_KEY,
    BackupCreationResult, BackupDatabaseManifest, BackupError, BackupJobHandler, BackupManager,
    BackupManifest, BackupProjectManifest, BackupService, BackupStorageRootIdentity,
    DiagnosticBundle, DiagnosticBundleResult, RecoveryDiagnostic, RedactedBackupConfiguration,
    RestoreInspection, RestoreResult,
};
pub use catalog::{
    CONTROL_CATALOG_MIGRATIONS, CONTROL_CATALOG_SCHEMA_VERSION, CONTROL_DATABASE_FILENAME,
    ControlCatalog, INSTANCE_LOCK_FILENAME, InstanceLockError, ServerInstanceLock,
    inspect_control_catalog_migrations, open_read_only_connection, open_writable_connection,
};
pub use config::{
    ConfigDiagnostics, ConfigError, ConfigErrorReason, DEFAULT_DATA_DIR, DEFAULT_LISTEN_ADDR,
    DEFAULT_SHUTDOWN_TIMEOUT_SECONDS, SecretString, ServerConfig,
};
pub use error_codes::{STABLE_ERROR_CODES, is_stable_error_code};
pub use foundation::{
    CheckStatus, ReadinessCheck, ReadinessReport, ReadinessStatus, RuntimeFoundation,
};
pub use job_callbacks::{
    CallbackClaim, CallbackDelivery, CallbackPage, CallbackSubscription, JobCallbacks,
};
pub use jobs::{
    CancellableChild, ClaimedJob, Clock, FailureKind, ItemCompletion, JobClass, JobDefinition,
    JobEngine, JobEngineError, JobEvent, JobEventPage, JobHandler, JobHandlerFailure, JobListItem,
    JobPage, JobProgress, JobRequirements, JobResultCommit, JobScheduler, JobSchedulerConfig,
    JobSnapshot, JobSnapshotError, JobState, NodeJobCompletion, ProgressEstimateInput, RetryPolicy,
    ServerCapability, SystemClock, cancel_child_safely,
};
pub use library::{
    AssetObservationInput, LibraryCursor, LibrarySearchItem, LibrarySearchPage,
    LocalNodeRootBindingInput, MediaAvailability, MediaLibraryItem, MediaLibraryPage,
    MediaLibraryQuery, MediaObject, MediaObjectLocation, MediaSearchFacts, NewMediaObject,
    NewStorageObject, NewStorageRoot, NodeRootBindingAdmin, NodeRootCapability,
    NodeRootCapabilityInput, ObservationGeneration, ObservationScope,
    ProjectMediaMembershipProjection, StorageLibrary, StorageLibraryError,
    StorageLocationAvailability, StorageObject, StorageRoot, StorageRootUpdate,
};
pub use media_facts::{
    FullHashFact, MediaFactsError, MediaMismatchEvidence, ProbeFact, QuickFingerprintFact,
    ScanFileDiagnostic,
};
pub use rcdvlog_core::{MediaEstimatorEnvironment, MediaEstimatorIdentity, StorageAccessMode};
pub use replicas::{
    AddToProjectDisposition, AddToProjectItem, AddToProjectPreview, AddToProjectRequest,
    AddToProjectResult, MAX_ADD_TO_PROJECT_ITEMS, ProjectPage, ProjectReplica, ProjectReplicaError,
    ProjectReplicaManager, ProjectReviewAsset, ProjectSummary, ProjectSyncOverview,
    ReviewMutationResult, ReviewRepresentation, inside_any_root,
};
pub use representations::{
    PROXY_RECIPE_KEY, Representation, RepresentationError, RepresentationJobHandler,
    RepresentationKind, RepresentationNeed, RepresentationRequest, RepresentationRequestResult,
    RepresentationService, RepresentationState, ServerJobHandler, THUMBNAIL_RECIPE_KEY,
};
pub use review_discussions::{
    AuthorKind, DecisionAction, Discussion, DiscussionItem, ItemKind, MovieTimeEstimate,
    NewDiscussionItem, REVIEW_REPORT_SCHEMA, ReviewActor, ReviewDiscussionError, ReviewDiscussions,
    ReviewWorkspace, SyncDescription, UtteranceSync, WorkspaceUtterance,
};
pub use review_sessions::{
    ChunkReceipt, ChunkUpload, CreateReviewSessionRequest, FinalizeReviewSessionRequest,
    FrameRange, REVIEW_SESSION_LIST_MAX_LIMIT, REVIEW_SESSION_MAX_ACTIVE, REVIEW_SESSION_MAX_BYTES,
    REVIEW_SESSION_MAX_CHUNK_BYTES, RecordingAudioFormat, RecordingCaptureSettings,
    RecordingConsent, RecordingEndReason, ReviewSessionAudio, ReviewSessionCoverage,
    ReviewSessionDetail, ReviewSessionError, ReviewSessionEvent, ReviewSessionEventKind,
    ReviewSessionEventRequest, ReviewSessionPage, ReviewSessionState, ReviewSessionSummary,
    ReviewSessions, wav_header,
};
pub use review_sync::{
    CALIBRATION_SCHEMA, MANUAL_CUE_CONFIDENCE, MANUAL_CUE_MOVIE_UNCERTAINTY_SECONDS, SessionSync,
};
pub use review_targets::{
    ArtifactAvailabilityAssertion, ArtifactAvailabilityState, HISTORICAL_REVIEW_ACKNOWLEDGEMENT,
    HISTORICAL_REVIEW_TARGET_LIMITATIONS, HistoricalIntegrityReceipt,
    HistoricalReviewTargetImportRequest, ImportOutcome, PreflightOutcome,
    REVIEW_TARGET_LIMITATIONS, REVIEW_TARGET_LIST_MAX_LIMIT,
    REVIEW_TARGET_SERVER_MAX_VERIFICATION_AGE_SECONDS, ReviewTargetActor, ReviewTargetCandidates,
    ReviewTargetCheck, ReviewTargetCheckStatus, ReviewTargetContract, ReviewTargetCutView,
    ReviewTargetDetail, ReviewTargetEvidence, ReviewTargetImportRequest, ReviewTargetImportResult,
    ReviewTargetObservation, ReviewTargetPage, ReviewTargetPreflight, ReviewTargetPreview,
    ReviewTargetReadiness, ReviewTargetReceipt, ReviewTargetServiceError, ReviewTargetSummary,
    ReviewTargets, VerificationFreshness,
};
pub use review_transcripts::{
    ModelDirSpeechRuntime, REVIEW_TRANSCRIBE_JOB_TYPE, REVIEW_TRANSCRIPT_SCHEMA,
    ReviewSpeechRuntime, ReviewTranscriptError, ReviewTranscriptionBackend,
    ReviewTranscriptionJobHandler, ReviewTranscriptionPreflight, ReviewTranscriptionRequest,
    ReviewTranscriptionRun, ReviewTranscriptionState, ReviewTranscripts, ReviewUtterance,
    resample_to, script_and_display,
};
pub use scan::{
    NewScanSchedule, RecoveryReport, ScanRun, ScanSchedule, ScanScheduleUpdate, ScanService,
    ScanServiceError, ScanSource, ScanState, ScanToolConfig, compute_next_run_utc,
    safe_path_identity, validate_local_time, validate_timezone,
};
pub use scan_handler::ScanJobHandler;
pub use storage_path::{
    PathAccessIntent, PathResolutionError, PathResolutionErrorReason,
    resolve_existing_binding_path, validate_local_mount_path,
};

pub const APPLICATION_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PROTOCOL_MAJOR_VERSION: u32 = 1;
pub const PROTOCOL_MINOR_VERSION: u32 = 0;
pub use audit::{AdminAuditError, AdminAuditEvent, AdminAuditLog, AuditActorType};
