mod catalog;
mod config;
mod foundation;
mod jobs;
mod library;
mod media_facts;
mod representations;
mod scan;
pub mod scan_handler;
mod storage_path;

pub use rcdvlog_core::{CatalogSchemaError, RepresentationId};

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
    JobSnapshotError, JobState, ProgressEstimateInput, RetryPolicy, ServerCapability, SystemClock,
    cancel_child_safely,
};
pub use library::{
    AssetObservationInput, LibraryCursor, LocalNodeRootBindingInput, MediaAvailability,
    MediaLibraryItem, MediaLibraryPage, MediaLibraryQuery, MediaObject, MediaObjectLocation,
    NewMediaObject, NewStorageObject, NewStorageRoot, NodeRootBindingAdmin, NodeRootCapability,
    NodeRootCapabilityInput, ObservationGeneration, ObservationScope,
    ProjectMediaMembershipProjection, StorageLibrary, StorageLibraryError,
    StorageLocationAvailability, StorageObject, StorageRoot, StorageRootUpdate,
};
pub use media_facts::{
    FullHashFact, MediaFactsError, MediaMismatchEvidence, ProbeFact, QuickFingerprintFact,
    ScanFileDiagnostic,
};
pub use rcdvlog_core::StorageAccessMode;
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
