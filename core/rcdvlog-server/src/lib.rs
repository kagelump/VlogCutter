mod catalog;
mod config;
mod foundation;
mod jobs;
mod library;
mod storage_path;

pub use rcdvlog_core::CatalogSchemaError;

pub use catalog::{
    CONTROL_CATALOG_MIGRATIONS, CONTROL_CATALOG_SCHEMA_VERSION, CONTROL_DATABASE_FILENAME,
    ControlCatalog, INSTANCE_LOCK_FILENAME, InstanceLockError, ServerInstanceLock,
    inspect_control_catalog_migrations,
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
pub use storage_path::{
    PathAccessIntent, PathResolutionError, PathResolutionErrorReason,
    resolve_existing_binding_path, validate_local_mount_path,
};

pub const APPLICATION_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PROTOCOL_MAJOR_VERSION: u32 = 1;
pub const PROTOCOL_MINOR_VERSION: u32 = 0;
