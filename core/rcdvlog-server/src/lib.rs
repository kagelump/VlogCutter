pub mod auth;
mod catalog;
mod config;
mod foundation;
mod jobs;
mod library;
pub mod library_query;
mod media_facts;
mod replicas;
mod representations;
mod scan;
pub mod scan_handler;
mod search_index;
mod storage_path;

pub use rcdvlog_core::{
    Catalog, CatalogSchemaError, GlobalMediaVerificationState, JobId, MediaObjectId,
    NodeAvailabilityState, NodeId, PROJECT_CATALOG_SCHEMA_VERSION, ProjectAssetAddedPayload,
    ProjectAssetItem, ProjectAssetPage, ProjectId, RepresentationId, StorageRootId,
};
pub use rcdvlog_sync::{
    BASELINE_VERSION, BaselineEnvelope, BaselinePayload, BaselinePreview, NodeTargetedJob,
    ObservationFact, ProtocolRange, ReconcileRequest, ReconcileResponse,
    RepresentationManifestFact, SyncError,
};

pub use auth::{
    AuthConfig, AuthDiagnostics, AuthError, AuthService, BootstrapCode, DeviceCredentialCreated,
    DeviceCredentialInfo, PairingCode, SessionCreated, SessionInfo,
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
pub use foundation::{
    CheckStatus, ReadinessCheck, ReadinessReport, ReadinessStatus, RuntimeFoundation,
};
pub use jobs::{
    CancellableChild, ClaimedJob, Clock, FailureKind, ItemCompletion, JobClass, JobDefinition,
    JobEngine, JobEngineError, JobEvent, JobEventPage, JobHandler, JobHandlerFailure, JobProgress,
    JobRequirements, JobResultCommit, JobScheduler, JobSchedulerConfig, JobSnapshot,
    JobSnapshotError, JobState, NodeJobCompletion, ProgressEstimateInput, RetryPolicy,
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
pub use rcdvlog_core::StorageAccessMode;
pub use replicas::{
    AddToProjectDisposition, AddToProjectItem, AddToProjectPreview, AddToProjectRequest,
    AddToProjectResult, MAX_ADD_TO_PROJECT_ITEMS, ProjectPage, ProjectReplica, ProjectReplicaError,
    ProjectReplicaManager, ProjectSummary,
};
pub use representations::{
    PROXY_RECIPE_KEY, Representation, RepresentationError, RepresentationJobHandler,
    RepresentationKind, RepresentationNeed, RepresentationRequest, RepresentationRequestResult,
    RepresentationService, RepresentationState, ServerJobHandler, THUMBNAIL_RECIPE_KEY,
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
