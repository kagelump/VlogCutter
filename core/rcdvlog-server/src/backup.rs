use crate::catalog::{
    CONTROL_CATALOG_MIGRATIONS, CONTROL_CATALOG_SCHEMA_VERSION, ControlCatalog,
    open_read_only_connection, open_writable_connection,
};
use crate::{
    APPLICATION_VERSION, ClaimedJob, FailureKind, JobClass, JobDefinition, JobEngine,
    JobEngineError, JobHandler, JobHandlerFailure, JobRequirements, JobResultCommit, JobSnapshot,
    PROTOCOL_MAJOR_VERSION, PROTOCOL_MINOR_VERSION, ReadinessReport, RetryPolicy, ServerConfig,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, SecondsFormat, Utc};
use rcdvlog_core::{Catalog, PROJECT_CATALOG_SCHEMA_VERSION, run_sqlite_migrations};
use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub const BACKUP_JOB_TYPE: &str = "backup.create.v1";
pub const BACKUP_RECIPE_KEY: &str = "backup.sqlite-online.v1";
pub const BACKUP_MANIFEST_FILENAME: &str = "manifest.json";
pub const BACKUP_FORMAT_VERSION: u32 = 1;
const BACKUP_DIRECTORY_PREFIX: &str = "rcdvlog-backup-";
const STAGING_DIRECTORY_PREFIX: &str = ".rcdvlog-backup-staging-";
const BACKUP_PAGE_STEP: i32 = 128;
const BACKUP_STEP_PAUSE: Duration = Duration::from_millis(10);
const BACKUP_ATTEMPT_BUDGET: Duration = Duration::from_secs(60);

#[derive(Debug, Error)]
pub enum BackupError {
    #[error("invalid backup input: {0}")]
    InvalidInput(String),
    #[error("backup operation was canceled")]
    Canceled,
    #[error("backup source remained busy beyond its bounded attempt budget")]
    BusyTimeout,
    #[error("backup restore point was not found")]
    NotFound,
    #[error("backup restore point is incompatible: {0}")]
    Incompatible(String),
    #[error("backup restore point failed verification: {0}")]
    Verification(String),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
    #[error(transparent)]
    Infrastructure(#[from] anyhow::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupDatabaseManifest {
    pub role: String,
    pub project_id: Option<String>,
    pub relative_path: String,
    pub original_replica_relative_path: Option<String>,
    pub schema_version: u32,
    pub size_bytes: u64,
    pub sha256: String,
    pub integrity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupProjectManifest {
    pub project_id: String,
    pub replica_relative_path: String,
    pub database_relative_path: String,
    pub origin_node_id: Option<String>,
    pub server_node_id: String,
    pub operation_cursors: BTreeMap<String, u64>,
    pub origin_sequence_watermarks: BTreeMap<String, u64>,
    pub acknowledged_server_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupStorageRootIdentity {
    pub storage_root_id: String,
    pub display_name: String,
    pub immutable_original: bool,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RedactedBackupConfiguration {
    pub storage_roots: Vec<BackupStorageRootIdentity>,
    pub allowed_original_mount_count: usize,
    pub derived_path_identity: String,
    pub backup_path_identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupManifest {
    pub format_version: u32,
    pub backup_id: String,
    pub server_node_id: String,
    pub application_version: String,
    pub protocol_major_version: u32,
    pub protocol_minor_version: u32,
    pub control_schema_version: u32,
    pub project_schema_version: u32,
    pub created_at: String,
    pub completed_at: String,
    pub databases: Vec<BackupDatabaseManifest>,
    pub projects: Vec<BackupProjectManifest>,
    pub total_bytes: u64,
    pub configuration: RedactedBackupConfiguration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupCreationResult {
    pub manifest: BackupManifest,
    pub published_relative_path: String,
    pub manifest_sha256: String,
    pub reused: bool,
    pub pruned_backup_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreInspection {
    pub valid: bool,
    pub compatible: bool,
    pub errors: Vec<String>,
    pub manifest: Option<BackupManifest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreResult {
    pub backup_id: String,
    pub restored_data_path: PathBuf,
    pub project_count: usize,
    pub sequence_recovery_projects: Vec<String>,
    pub invalidated_representation_locations: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryDiagnostic {
    pub project_id: String,
    pub status: String,
    pub reseedable_from_origin: bool,
    pub replica_path_identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticBundle {
    pub bundle_format_version: u32,
    pub created_at: String,
    pub application_version: String,
    pub protocol_major_version: u32,
    pub protocol_minor_version: u32,
    pub control_schema_version: u32,
    pub server_node_id: String,
    pub readiness: ReadinessReport,
    pub configuration: RedactedBackupConfiguration,
    pub control_migration_status: String,
    pub job_counts: BTreeMap<String, u64>,
    pub restore_points_valid: usize,
    pub restore_points_invalid: usize,
    pub recovery: Vec<RecoveryDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticBundleResult {
    pub bundle_id: String,
    pub output_path: PathBuf,
    pub sha256: String,
}

#[derive(Clone)]
pub struct BackupManager {
    control: ControlCatalog,
    config: ServerConfig,
}

#[derive(Clone)]
pub struct BackupService {
    manager: BackupManager,
    jobs: JobEngine,
}

impl BackupService {
    pub fn new(manager: BackupManager, jobs: JobEngine) -> Self {
        Self { manager, jobs }
    }

    pub fn request_backup(&self, idempotency_key: &str) -> Result<JobSnapshot, BackupError> {
        if idempotency_key.is_empty()
            || idempotency_key.len() > 255
            || !idempotency_key.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.')
            })
        {
            return Err(BackupError::InvalidInput(
                "idempotency key must be a safe 1–255 character token".into(),
            ));
        }
        let identity =
            blake3::hash(format!("{}\0{idempotency_key}", self.jobs.node_id()).as_bytes()).to_hex();
        let suffix = &identity[..32];
        let backup_id = format!("set_{suffix}");
        let job_id = rcdvlog_core::JobId::parse(format!("job_backup_{suffix}"))
            .map_err(|error| BackupError::InvalidInput(error.to_string()))?;
        let work_units = estimated_backup_bytes(&self.manager.config)
            .unwrap_or(1)
            .max(1);
        let definition = JobDefinition {
            job_id: job_id.clone(),
            job_type: BACKUP_JOB_TYPE.into(),
            payload_version: 1,
            payload: json!({"backupId": backup_id}),
            immutable_identity: json!({"backupId": backup_id,"formatVersion":BACKUP_FORMAT_VERSION}),
            requirements: JobRequirements {
                schema_version: 1,
                requires_original: false,
                requires_derived_write: false,
                capabilities: vec![],
            },
            target_node_id: self.jobs.node_id().clone(),
            project_id: None,
            media_object_id: None,
            storage_root_id: None,
            representation_id: None,
            recipe_key: BACKUP_RECIPE_KEY.into(),
            job_class: JobClass::Lightweight,
            idempotency_key: idempotency_key.to_string(),
            priority: -10,
            total_work_units: work_units,
            total_items: 1,
            retry_policy: RetryPolicy {
                max_attempts: 3,
                initial_backoff_seconds: 30,
                max_backoff_seconds: 600,
            },
            dependencies: vec![],
        };
        self.jobs
            .enqueue_with_id(job_id.as_str(), definition)
            .map_err(|error| match error {
                JobEngineError::Database(error) => BackupError::Database(error),
                other => BackupError::Infrastructure(anyhow::anyhow!(other)),
            })?;
        self.jobs.get_job(&job_id).map_err(|error| match error {
            JobEngineError::Database(error) => BackupError::Database(error),
            other => BackupError::Infrastructure(anyhow::anyhow!(other)),
        })
    }
}

#[derive(Clone)]
pub struct BackupJobHandler {
    manager: BackupManager,
}

impl BackupJobHandler {
    pub fn new(manager: BackupManager) -> Self {
        Self { manager }
    }
}

impl JobHandler for BackupJobHandler {
    fn run(
        &self,
        job: ClaimedJob,
        cancellation: CancellationToken,
    ) -> Result<JobResultCommit, JobHandlerFailure> {
        if job.job.job_type != BACKUP_JOB_TYPE || job.job.payload_version != 1 {
            return Err(backup_handler_failure(
                FailureKind::Terminal,
                "unsupported_backup_job",
            ));
        }
        let backup_id = job
            .job
            .payload
            .get("backupId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                backup_handler_failure(FailureKind::Terminal, "invalid_backup_payload")
            })?;
        if job
            .job
            .immutable_identity
            .get("backupId")
            .and_then(Value::as_str)
            != Some(backup_id)
        {
            return Err(backup_handler_failure(
                FailureKind::Terminal,
                "backup_identity_mismatch",
            ));
        }
        let result = self
            .manager
            .create_restore_point(backup_id, || cancellation.is_cancelled())
            .map_err(|error| match error {
                BackupError::InvalidInput(_)
                | BackupError::Incompatible(_)
                | BackupError::Verification(_) => {
                    backup_handler_failure(FailureKind::Terminal, "backup_verification_failed")
                }
                BackupError::Canceled => {
                    backup_handler_failure(FailureKind::Retryable, "backup_canceled")
                }
                BackupError::BusyTimeout => {
                    backup_handler_failure(FailureKind::Retryable, "backup_source_busy")
                }
                _ => backup_handler_failure(FailureKind::Retryable, "backup_io_failed"),
            })?;
        Ok(JobResultCommit {
            output_identity: Some(result.manifest.backup_id.clone()),
            details: json!({
                "backupId": result.manifest.backup_id,
                "publishedRelativePath": result.published_relative_path,
                "manifestSha256": result.manifest_sha256,
                "databaseCount": result.manifest.databases.len(),
                "projectCount": result.manifest.projects.len(),
                "totalBytes": result.manifest.total_bytes,
                "completedAt": result.manifest.completed_at,
                "reused": result.reused,
                "prunedCount": result.pruned_backup_ids.len(),
            }),
        })
    }

    fn commit_result(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        job: &ClaimedJob,
        result: &JobResultCommit,
    ) -> Result<(), JobEngineError> {
        let string = |key: &str| {
            result
                .details
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| JobEngineError::CorruptState(format!("backup result lacks {key}")))
        };
        let count = |key: &str| {
            result
                .details
                .get(key)
                .and_then(Value::as_u64)
                .ok_or_else(|| JobEngineError::CorruptState(format!("backup result lacks {key}")))
        };
        transaction.execute(
            r#"
            INSERT INTO backup_sets(
                backup_id,job_id,state,published_relative_path,manifest_sha256,
                database_count,project_count,total_bytes,created_at,completed_at
            ) VALUES(?1,?2,'completed',?3,?4,?5,?6,?7,?8,?8)
            ON CONFLICT(backup_id) DO UPDATE SET
                job_id=excluded.job_id,state='completed',published_relative_path=excluded.published_relative_path,
                manifest_sha256=excluded.manifest_sha256,database_count=excluded.database_count,
                project_count=excluded.project_count,total_bytes=excluded.total_bytes,
                completed_at=excluded.completed_at,pruned_at=NULL
            "#,
            params![
                string("backupId")?,
                job.job.job_id.as_str(),
                string("publishedRelativePath")?,
                string("manifestSha256")?,
                i64::try_from(count("databaseCount")?)
                    .map_err(|error| JobEngineError::CorruptState(error.to_string()))?,
                i64::try_from(count("projectCount")?)
                    .map_err(|error| JobEngineError::CorruptState(error.to_string()))?,
                i64::try_from(count("totalBytes")?)
                    .map_err(|error| JobEngineError::CorruptState(error.to_string()))?,
                string("completedAt")?,
            ],
        )?;
        Ok(())
    }
}

fn backup_handler_failure(kind: FailureKind, code: &str) -> JobHandlerFailure {
    JobHandlerFailure {
        kind,
        code: code.into(),
        details: json!({"code":code}),
    }
}

impl BackupManager {
    pub fn new(control: ControlCatalog, config: ServerConfig) -> Self {
        Self { control, config }
    }

    pub fn backup_root(&self) -> &Path {
        &self.config.backup_dir
    }

    pub fn inspect_restore_point_in_root(
        backup_root: &Path,
        path: &Path,
    ) -> Result<RestoreInspection, BackupError> {
        let root = canonical_backup_root(backup_root)?;
        inspect_restore_point_at(&root, path)
    }

    pub fn create_restore_point<F>(
        &self,
        backup_id: &str,
        canceled: F,
    ) -> Result<BackupCreationResult, BackupError>
    where
        F: Fn() -> bool,
    {
        validate_backup_id(backup_id)?;
        let root = canonical_backup_root(&self.config.backup_dir)?;
        let published_name = format!("{BACKUP_DIRECTORY_PREFIX}{backup_id}");
        let published = root.join(&published_name);
        if published.join(BACKUP_MANIFEST_FILENAME).is_file() {
            let inspection = inspect_restore_point_at(&root, &published)?;
            if inspection.valid && inspection.compatible {
                let manifest = inspection
                    .manifest
                    .ok_or_else(|| BackupError::Verification("manifest disappeared".into()))?;
                return Ok(BackupCreationResult {
                    manifest_sha256: sha256_file(&published.join(BACKUP_MANIFEST_FILENAME))?,
                    manifest,
                    published_relative_path: published_name,
                    reused: true,
                    pruned_backup_ids: vec![],
                });
            }
        }
        if published.exists() {
            remove_validated_backup_work_directory(&root, &published, &published_name)?;
        }
        let staging_name = format!("{STAGING_DIRECTORY_PREFIX}{backup_id}");
        let staging = root.join(&staging_name);
        if staging.exists() {
            remove_validated_backup_work_directory(&root, &staging, &staging_name)?;
        }
        create_private_directory(&staging)?;

        let result = (|| -> Result<BackupCreationResult, BackupError> {
            if canceled() {
                return Err(BackupError::Canceled);
            }
            let created_at = timestamp();
            let control_target = staging.join("control.sqlite3");
            backup_database_bounded(self.control.database_path(), &control_target, &canceled)?;
            let control_database = database_manifest(
                "control",
                None,
                "control.sqlite3",
                None,
                CONTROL_CATALOG_SCHEMA_VERSION,
                &control_target,
            )?;

            // The registry and peer watermarks must come from the same control-catalog
            // snapshot that is published. Reading the live catalog here could pair a
            // newly registered project with an older control snapshot.
            let registry = registered_projects(&control_target)?;
            let projects_directory = staging.join("projects");
            create_private_directory(&projects_directory)?;
            let mut databases = vec![control_database];
            let mut projects = Vec::with_capacity(registry.len());
            for (index, registered) in registry.into_iter().enumerate() {
                if canceled() {
                    return Err(BackupError::Canceled);
                }
                let source = resolve_data_relative(
                    &self.config.data_dir,
                    &registered.replica_relative_path,
                )?;
                if !source.is_file() {
                    return Err(BackupError::Verification(format!(
                        "registered project {} has no replica",
                        registered.project_id
                    )));
                }
                let digest = blake3::hash(registered.project_id.as_bytes()).to_hex();
                let file_name = format!("{index:04}-{}.sqlite3", &digest[..16]);
                let relative = format!("projects/{file_name}");
                let target = projects_directory.join(file_name);
                backup_database_bounded(&source, &target, &canceled)?;
                let database = database_manifest(
                    "project_replica",
                    Some(registered.project_id.clone()),
                    &relative,
                    Some(registered.replica_relative_path.clone()),
                    PROJECT_CATALOG_SCHEMA_VERSION,
                    &target,
                )?;
                let (operation_cursors, origin_sequence_watermarks) = project_sequences(&target)?;
                let acknowledged_server_sequence = registered
                    .origin_node_id
                    .as_deref()
                    .map(|peer| {
                        acknowledged_server_sequence(&control_target, &registered.project_id, peer)
                    })
                    .transpose()?
                    .unwrap_or(0);
                projects.push(BackupProjectManifest {
                    project_id: registered.project_id,
                    replica_relative_path: registered.replica_relative_path,
                    database_relative_path: relative,
                    origin_node_id: registered.origin_node_id,
                    server_node_id: registered.server_node_id,
                    operation_cursors,
                    origin_sequence_watermarks,
                    acknowledged_server_sequence,
                });
                databases.push(database);
            }
            let total_bytes = databases.iter().map(|database| database.size_bytes).sum();
            let completed_at = timestamp();
            let manifest = BackupManifest {
                format_version: BACKUP_FORMAT_VERSION,
                backup_id: backup_id.to_string(),
                server_node_id: self.control.node_id().to_string(),
                application_version: APPLICATION_VERSION.to_string(),
                protocol_major_version: PROTOCOL_MAJOR_VERSION,
                protocol_minor_version: PROTOCOL_MINOR_VERSION,
                control_schema_version: CONTROL_CATALOG_SCHEMA_VERSION,
                project_schema_version: PROJECT_CATALOG_SCHEMA_VERSION,
                created_at,
                completed_at,
                databases,
                projects,
                total_bytes,
                configuration: self.redacted_configuration_from(&control_target)?,
            };
            fs::rename(&staging, &published).context("publish completed backup directory")?;
            let manifest_temp = published.join("manifest.tmp.json");
            write_private_file(&manifest_temp, &serde_json::to_vec_pretty(&manifest)?)?;
            let manifest_path = published.join(BACKUP_MANIFEST_FILENAME);
            fs::rename(&manifest_temp, &manifest_path).context("publish backup manifest")?;
            sync_directory(&published)?;
            let manifest_sha256 = sha256_file(&manifest_path)?;
            let pruned_backup_ids = self.prune_retention(Some(backup_id))?;
            Ok(BackupCreationResult {
                manifest,
                published_relative_path: published_name,
                manifest_sha256,
                reused: false,
                pruned_backup_ids,
            })
        })();
        if result.is_err() && staging.exists() {
            let _ = remove_validated_backup_work_directory(&root, &staging, &staging_name);
        }
        result
    }

    pub fn inspect_restore_point(&self, path: &Path) -> Result<RestoreInspection, BackupError> {
        Self::inspect_restore_point_in_root(&self.config.backup_dir, path)
    }

    pub fn list_restore_points(&self) -> Result<Vec<RestoreInspection>, BackupError> {
        let root = canonical_backup_root(&self.config.backup_dir)?;
        let mut points = Vec::new();
        for entry in fs::read_dir(&root).context("read backup root")? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(BACKUP_DIRECTORY_PREFIX) && entry.file_type()?.is_dir() {
                points.push(inspect_restore_point_at(&root, &entry.path())?);
            }
        }
        points.sort_by(|left, right| {
            right
                .manifest
                .as_ref()
                .map(|manifest| &manifest.completed_at)
                .cmp(
                    &left
                        .manifest
                        .as_ref()
                        .map(|manifest| &manifest.completed_at),
                )
        });
        Ok(points)
    }

    pub fn restore_to_new_data_root(
        &self,
        restore_point: &Path,
        target_data_root: &Path,
    ) -> Result<RestoreResult, BackupError> {
        validate_restore_target(&self.config, target_data_root)?;
        let inspection = self.inspect_restore_point(restore_point)?;
        if !inspection.valid {
            return Err(BackupError::Verification(inspection.errors.join(", ")));
        }
        if !inspection.compatible {
            return Err(BackupError::Incompatible(inspection.errors.join(", ")));
        }
        let manifest = inspection
            .manifest
            .ok_or_else(|| BackupError::Verification("manifest is unavailable".into()))?;
        let root = canonical_backup_root(&self.config.backup_dir)?;
        let restore_point = canonical_restore_point(&root, restore_point)?;
        let parent = target_data_root.parent().ok_or_else(|| {
            BackupError::InvalidInput("restore target has no parent directory".into())
        })?;
        fs::create_dir_all(parent).context("create restore target parent")?;
        let staging = parent.join(format!(".rcdvlog-restore-{}", Uuid::new_v4().simple()));
        create_private_directory(&staging)?;
        let result = (|| -> Result<RestoreResult, BackupError> {
            for database in &manifest.databases {
                let source = safe_manifest_path(&restore_point, &database.relative_path)?;
                let destination = match database.role.as_str() {
                    "control" => staging.join("control.sqlite3"),
                    "project_replica" => {
                        let relative = database
                            .original_replica_relative_path
                            .as_deref()
                            .ok_or_else(|| {
                                BackupError::Verification("project replica path is missing".into())
                            })?;
                        safe_manifest_path(&staging, relative)?
                    }
                    _ => return Err(BackupError::Verification("unknown database role".into())),
                };
                if let Some(parent) = destination.parent() {
                    create_private_directory(parent)?;
                }
                fs::copy(source, &destination).context("copy verified database snapshot")?;
            }
            let control_path = staging.join("control.sqlite3");
            let mut connection = open_writable_connection(&control_path)?;
            run_sqlite_migrations(&mut connection, CONTROL_CATALOG_MIGRATIONS)?;
            drop(connection);

            let mut sequence_recovery_projects = Vec::new();
            for project in &manifest.projects {
                let replica_path = safe_manifest_path(&staging, &project.replica_relative_path)?;
                let catalog = Catalog::open_existing(&replica_path)?;
                if let Some(peer) = project.origin_node_id.as_deref() {
                    let watermark = project
                        .origin_sequence_watermarks
                        .get(&project.server_node_id)
                        .copied()
                        .unwrap_or(0)
                        .max(project.acknowledged_server_sequence);
                    catalog.mark_sequence_recovery(peer, &project.server_node_id, watermark)?;
                    sequence_recovery_projects.push(project.project_id.clone());
                }
            }
            let invalidated = revalidate_restored_representations(
                &control_path,
                &self.config.derived_dir,
                &manifest.server_node_id,
            )?;
            fs::rename(&staging, target_data_root).context("publish restored data directory")?;
            sync_directory(parent)?;
            Ok(RestoreResult {
                backup_id: manifest.backup_id,
                restored_data_path: target_data_root.to_path_buf(),
                project_count: manifest.projects.len(),
                sequence_recovery_projects,
                invalidated_representation_locations: invalidated,
            })
        })();
        if result.is_err() && staging.exists() {
            let _ = fs::remove_dir_all(&staging);
        }
        result
    }

    pub fn generate_diagnostic_bundle(
        &self,
        output_directory: &Path,
        readiness: ReadinessReport,
    ) -> Result<DiagnosticBundleResult, BackupError> {
        if !output_directory.is_absolute() {
            return Err(BackupError::InvalidInput(
                "diagnostic output directory must be absolute".into(),
            ));
        }
        create_private_directory(output_directory)?;
        let connection = open_read_only_connection(self.control.database_path())?;
        let mut job_counts = BTreeMap::new();
        {
            let mut statement = connection
                .prepare("SELECT state,COUNT(*) FROM jobs GROUP BY state ORDER BY state")?;
            for row in statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })? {
                let (state, count) = row?;
                job_counts.insert(
                    state,
                    u64::try_from(count)
                        .map_err(|error| BackupError::Verification(error.to_string()))?,
                );
            }
        }
        let mut recovery = Vec::new();
        for registered in registered_projects(self.control.database_path())? {
            let replica =
                resolve_data_relative(&self.config.data_dir, &registered.replica_relative_path)?;
            let status = if !replica.is_file() {
                "missing"
            } else if verify_sqlite_integrity(&replica).is_err() {
                "corrupt"
            } else {
                "healthy"
            };
            recovery.push(RecoveryDiagnostic {
                project_id: registered.project_id,
                status: status.into(),
                reseedable_from_origin: status != "healthy" && registered.origin_node_id.is_some(),
                replica_path_identity: path_identity(&replica),
            });
        }
        drop(connection);
        let inspections = self.list_restore_points()?;
        let bundle = DiagnosticBundle {
            bundle_format_version: 1,
            created_at: timestamp(),
            application_version: APPLICATION_VERSION.into(),
            protocol_major_version: PROTOCOL_MAJOR_VERSION,
            protocol_minor_version: PROTOCOL_MINOR_VERSION,
            control_schema_version: CONTROL_CATALOG_SCHEMA_VERSION,
            server_node_id: self.control.node_id().to_string(),
            readiness,
            configuration: self.redacted_configuration_from(self.control.database_path())?,
            control_migration_status: format!("{:?}", self.control.inspect_migrations()?.status)
                .to_lowercase(),
            job_counts,
            restore_points_valid: inspections.iter().filter(|point| point.valid).count(),
            restore_points_invalid: inspections.iter().filter(|point| !point.valid).count(),
            recovery,
        };
        let bundle_id = format!("bundle_{}", Uuid::new_v4().simple());
        let output_path = output_directory.join(format!("rcdvlog-diagnostics-{bundle_id}.json"));
        write_private_file(&output_path, &serde_json::to_vec_pretty(&bundle)?)?;
        Ok(DiagnosticBundleResult {
            bundle_id,
            sha256: sha256_file(&output_path)?,
            output_path,
        })
    }

    fn redacted_configuration_from(
        &self,
        control_path: &Path,
    ) -> Result<RedactedBackupConfiguration, BackupError> {
        let connection = open_read_only_connection(control_path)?;
        let mut statement = connection.prepare(
            "SELECT storage_root_id,display_name,immutable_original,enabled FROM storage_roots ORDER BY storage_root_id",
        )?;
        let storage_roots = statement
            .query_map([], |row| {
                Ok(BackupStorageRootIdentity {
                    storage_root_id: row.get(0)?,
                    display_name: row.get(1)?,
                    immutable_original: row.get(2)?,
                    enabled: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(RedactedBackupConfiguration {
            storage_roots,
            allowed_original_mount_count: self.config.allowed_original_mounts.len(),
            derived_path_identity: path_identity(&self.config.derived_dir),
            backup_path_identity: path_identity(&self.config.backup_dir),
        })
    }

    fn prune_retention(
        &self,
        protected_backup_id: Option<&str>,
    ) -> Result<Vec<String>, BackupError> {
        let root = canonical_backup_root(&self.config.backup_dir)?;
        let mut manifests = Vec::new();
        for inspection in self.list_restore_points()? {
            if inspection.valid
                && inspection.compatible
                && let Some(manifest) = inspection.manifest
            {
                manifests.push(manifest);
            }
        }
        manifests.sort_by(|left, right| right.completed_at.cmp(&left.completed_at));
        let mut daily = BTreeSet::new();
        let mut weekly = BTreeSet::new();
        let mut keep = BTreeSet::new();
        for manifest in &manifests {
            let completed = DateTime::parse_from_rfc3339(&manifest.completed_at)
                .map_err(|_| BackupError::Verification("invalid completion timestamp".into()))?
                .with_timezone(&Utc);
            let day = completed.date_naive();
            if daily.len() < self.config.backup_daily_retention && daily.insert(day) {
                keep.insert(manifest.backup_id.clone());
            }
            let week = (completed.iso_week().year(), completed.iso_week().week());
            if weekly.len() < self.config.backup_weekly_retention && weekly.insert(week) {
                keep.insert(manifest.backup_id.clone());
            }
        }
        if let Some(protected) = protected_backup_id {
            keep.insert(protected.to_string());
        }
        let mut pruned = Vec::new();
        for manifest in manifests {
            if keep.contains(&manifest.backup_id) {
                continue;
            }
            let name = format!("{BACKUP_DIRECTORY_PREFIX}{}", manifest.backup_id);
            let path = root.join(&name);
            remove_validated_completed_backup(&root, &path, &manifest.backup_id)?;
            let connection = open_writable_connection(self.control.database_path())?;
            connection.execute(
                "UPDATE backup_sets SET state='pruned',pruned_at=?2 WHERE backup_id=?1",
                params![manifest.backup_id, timestamp()],
            )?;
            pruned.push(manifest.backup_id);
        }
        Ok(pruned)
    }
}

#[derive(Debug)]
struct RegisteredProject {
    project_id: String,
    replica_relative_path: String,
    origin_node_id: Option<String>,
    server_node_id: String,
}

fn estimated_backup_bytes(config: &ServerConfig) -> Result<u64, BackupError> {
    let control = config.data_dir.join("control.sqlite3");
    let mut total = fs::metadata(control).map(|value| value.len()).unwrap_or(1);
    if let Ok(connection) = open_read_only_connection(&config.data_dir.join("control.sqlite3")) {
        let mut statement = connection.prepare(
            "SELECT replica_relative_path FROM project_registry WHERE state='registered'",
        )?;
        let paths = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for relative in paths {
            if let Ok(path) = safe_manifest_path(&config.data_dir, &relative) {
                total =
                    total.saturating_add(fs::metadata(path).map(|value| value.len()).unwrap_or(0));
            }
        }
    }
    Ok(total.max(1))
}

fn registered_projects(control_path: &Path) -> Result<Vec<RegisteredProject>, BackupError> {
    let connection = open_read_only_connection(control_path)?;
    let mut statement = connection.prepare(
        "SELECT project_id,replica_relative_path,origin_node_id,server_node_id FROM project_registry WHERE state='registered' ORDER BY project_id",
    )?;
    Ok(statement
        .query_map([], |row| {
            Ok(RegisteredProject {
                project_id: row.get(0)?,
                replica_relative_path: row.get(1)?,
                origin_node_id: row.get(2)?,
                server_node_id: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn acknowledged_server_sequence(
    control_path: &Path,
    project_id: &str,
    peer_node_id: &str,
) -> Result<u64, BackupError> {
    let connection = open_read_only_connection(control_path)?;
    let value = connection
        .query_row(
            "SELECT acknowledged_server_sequence FROM project_peer_sync_state WHERE project_id=?1 AND peer_node_id=?2",
            params![project_id, peer_node_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0);
    u64::try_from(value).map_err(|error| BackupError::Verification(error.to_string()))
}

type SequenceMaps = (BTreeMap<String, u64>, BTreeMap<String, u64>);

fn project_sequences(database_path: &Path) -> Result<SequenceMaps, BackupError> {
    let connection = open_read_only_connection(database_path)?;
    let mut cursors = BTreeMap::new();
    {
        let mut statement = connection.prepare(
            "SELECT origin_node_id,committed_sequence FROM sync_origin_cursors ORDER BY origin_node_id",
        )?;
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })? {
            let (node, sequence) = row?;
            cursors.insert(
                node,
                u64::try_from(sequence)
                    .map_err(|error| BackupError::Verification(error.to_string()))?,
            );
        }
    }
    let mut watermarks = BTreeMap::new();
    {
        let mut statement = connection.prepare(
            "SELECT origin_node_id,last_sequence FROM operation_origin_sequences ORDER BY origin_node_id",
        )?;
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })? {
            let (node, sequence) = row?;
            watermarks.insert(
                node,
                u64::try_from(sequence)
                    .map_err(|error| BackupError::Verification(error.to_string()))?,
            );
        }
    }
    Ok((cursors, watermarks))
}

fn backup_database_bounded<F>(source: &Path, target: &Path, canceled: &F) -> Result<(), BackupError>
where
    F: Fn() -> bool,
{
    if let Some(parent) = target.parent() {
        create_private_directory(parent)?;
    }
    let source = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let mut target_connection = Connection::open(target)?;
    let backup = Backup::new(&source, &mut target_connection)?;
    let deadline = Instant::now() + BACKUP_ATTEMPT_BUDGET;
    loop {
        if canceled() {
            return Err(BackupError::Canceled);
        }
        if Instant::now() > deadline {
            return Err(BackupError::BusyTimeout);
        }
        match backup.step(BACKUP_PAGE_STEP)? {
            StepResult::Done => break,
            StepResult::More => thread::sleep(BACKUP_STEP_PAUSE),
            StepResult::Busy | StepResult::Locked => thread::sleep(Duration::from_millis(25)),
            _ => {
                return Err(BackupError::Verification(
                    "unknown SQLite backup state".into(),
                ));
            }
        }
    }
    drop(backup);
    target_connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    drop(target_connection);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(target, fs::Permissions::from_mode(0o600))?;
    }
    verify_sqlite_integrity(target)?;
    Ok(())
}

fn database_manifest(
    role: &str,
    project_id: Option<String>,
    relative_path: &str,
    original_replica_relative_path: Option<String>,
    schema_version: u32,
    path: &Path,
) -> Result<BackupDatabaseManifest, BackupError> {
    verify_sqlite_integrity(path)?;
    Ok(BackupDatabaseManifest {
        role: role.to_string(),
        project_id,
        relative_path: relative_path.to_string(),
        original_replica_relative_path,
        schema_version,
        size_bytes: fs::metadata(path)?.len(),
        sha256: sha256_file(path)?,
        integrity: "ok".to_string(),
    })
}

fn inspect_restore_point_at(root: &Path, path: &Path) -> Result<RestoreInspection, BackupError> {
    let point = match canonical_restore_point(root, path) {
        Ok(point) => point,
        Err(error) => {
            return Ok(RestoreInspection {
                valid: false,
                compatible: false,
                errors: vec![error.to_string()],
                manifest: None,
            });
        }
    };
    let manifest_path = point.join(BACKUP_MANIFEST_FILENAME);
    let bytes = match fs::read(&manifest_path) {
        Ok(bytes) => bytes,
        Err(_) => {
            return Ok(RestoreInspection {
                valid: false,
                compatible: false,
                errors: vec!["published manifest is missing".into()],
                manifest: None,
            });
        }
    };
    let manifest: BackupManifest = match serde_json::from_slice(&bytes) {
        Ok(manifest) => manifest,
        Err(_) => {
            return Ok(RestoreInspection {
                valid: false,
                compatible: false,
                errors: vec!["published manifest is invalid".into()],
                manifest: None,
            });
        }
    };
    let mut errors = Vec::new();
    if manifest.format_version != BACKUP_FORMAT_VERSION {
        errors.push("backup format is unsupported".into());
    }
    let compatible = manifest.control_schema_version <= CONTROL_CATALOG_SCHEMA_VERSION
        && manifest.project_schema_version <= PROJECT_CATALOG_SCHEMA_VERSION
        && manifest.protocol_major_version == PROTOCOL_MAJOR_VERSION;
    if !compatible {
        errors.push("backup schema or protocol is newer than this server".into());
    }
    for database in &manifest.databases {
        match safe_manifest_path(&point, &database.relative_path) {
            Ok(database_path) => {
                if !database_path.is_file() {
                    errors.push(format!("database {} is missing", database.role));
                    continue;
                }
                if fs::metadata(&database_path).map(|value| value.len()).ok()
                    != Some(database.size_bytes)
                {
                    errors.push(format!("database {} size mismatch", database.role));
                }
                if sha256_file(&database_path).ok().as_deref() != Some(database.sha256.as_str()) {
                    errors.push(format!("database {} checksum mismatch", database.role));
                }
                if verify_sqlite_integrity(&database_path).is_err() {
                    errors.push(format!("database {} integrity failed", database.role));
                }
            }
            Err(_) => errors.push(format!("database {} path is unsafe", database.role)),
        }
    }
    Ok(RestoreInspection {
        valid: errors.is_empty(),
        compatible,
        errors,
        manifest: Some(manifest),
    })
}

fn verify_sqlite_integrity(path: &Path) -> Result<(), BackupError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let result: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if result != "ok" {
        return Err(BackupError::Verification(
            "SQLite integrity check failed".into(),
        ));
    }
    Ok(())
}

fn revalidate_restored_representations(
    control_path: &Path,
    derived_root: &Path,
    server_node_id: &str,
) -> Result<u64, BackupError> {
    let mut connection = open_writable_connection(control_path)?;
    let transaction = connection.transaction()?;
    let mut statement = transaction.prepare(
        r#"
        SELECT l.representation_id,l.location_kind,l.location_key,l.relative_path,r.byte_length
        FROM representation_locations l
        JOIN representations r ON r.representation_id=l.representation_id
        WHERE l.node_id=?1 AND l.state='available'
        "#,
    )?;
    let locations = statement
        .query_map([server_node_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    let mut invalidated = 0_u64;
    for (representation_id, kind, key, relative, expected_size) in locations {
        let valid = if kind == "shared_derived" {
            relative
                .as_deref()
                .and_then(|relative| safe_manifest_path(derived_root, relative).ok())
                .and_then(|path| fs::metadata(path).ok())
                .is_some_and(|metadata| {
                    expected_size.is_none_or(|size| size >= 0 && metadata.len() == size as u64)
                })
        } else {
            false
        };
        if !valid {
            transaction.execute(
                "UPDATE representation_locations SET state='missing',updated_at=?4 WHERE representation_id=?1 AND node_id=?2 AND location_kind=?3 AND location_key=?5",
                params![representation_id, server_node_id, kind, timestamp(), key],
            )?;
            invalidated += 1;
        }
    }
    transaction.commit()?;
    Ok(invalidated)
}

fn validate_backup_id(value: &str) -> Result<(), BackupError> {
    if value.is_empty()
        || value.len() > 96
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(BackupError::InvalidInput(
            "backup ID must be 1–96 safe token characters".into(),
        ));
    }
    Ok(())
}

fn canonical_backup_root(path: &Path) -> Result<PathBuf, BackupError> {
    let canonical = path
        .canonicalize()
        .context("resolve configured backup root")?;
    if canonical == Path::new("/") {
        return Err(BackupError::InvalidInput(
            "backup root cannot be the filesystem root".into(),
        ));
    }
    Ok(canonical)
}

fn canonical_restore_point(root: &Path, path: &Path) -> Result<PathBuf, BackupError> {
    let canonical = path.canonicalize().map_err(|_| BackupError::NotFound)?;
    let name = canonical
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| BackupError::InvalidInput("restore point name is invalid".into()))?;
    if !canonical.starts_with(root)
        || canonical.parent() != Some(root)
        || !name.starts_with(BACKUP_DIRECTORY_PREFIX)
    {
        return Err(BackupError::InvalidInput(
            "restore point is outside the configured backup root".into(),
        ));
    }
    Ok(canonical)
}

fn validate_restore_target(config: &ServerConfig, target: &Path) -> Result<(), BackupError> {
    if !target.is_absolute()
        || target
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        || target == Path::new("/")
        || target.exists()
    {
        return Err(BackupError::InvalidInput(
            "restore target must be a new absolute directory without traversal".into(),
        ));
    }
    let mut protected = vec![
        config.data_dir.as_path(),
        config.temp_dir.as_path(),
        config.derived_dir.as_path(),
        config.backup_dir.as_path(),
    ];
    protected.extend(config.allowed_original_mounts.iter().map(PathBuf::as_path));
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if target == home {
            return Err(BackupError::InvalidInput(
                "restore target cannot be the home directory".into(),
            ));
        }
    }
    if std::env::current_dir().is_ok_and(|workspace| target == workspace) {
        return Err(BackupError::InvalidInput(
            "restore target cannot be the workspace root".into(),
        ));
    }
    if protected
        .into_iter()
        .any(|path| target.starts_with(path) || path.starts_with(target))
    {
        return Err(BackupError::InvalidInput(
            "restore target overlaps configured live or media storage".into(),
        ));
    }
    Ok(())
}

fn resolve_data_relative(data_root: &Path, relative: &str) -> Result<PathBuf, BackupError> {
    safe_manifest_path(data_root, relative)
}

fn safe_manifest_path(root: &Path, relative: &str) -> Result<PathBuf, BackupError> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::CurDir
                    | Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return Err(BackupError::Verification("manifest path is unsafe".into()));
    }
    Ok(root.join(relative))
}

fn remove_validated_backup_work_directory(
    root: &Path,
    path: &Path,
    expected_name: &str,
) -> Result<(), BackupError> {
    if path.parent() != Some(root)
        || path.file_name().and_then(|value| value.to_str()) != Some(expected_name)
        || !(expected_name.starts_with(BACKUP_DIRECTORY_PREFIX)
            || expected_name.starts_with(STAGING_DIRECTORY_PREFIX))
    {
        return Err(BackupError::InvalidInput(
            "backup work directory is unsafe".into(),
        ));
    }
    fs::remove_dir_all(path).context("remove validated incomplete backup")?;
    Ok(())
}

fn remove_validated_completed_backup(
    root: &Path,
    path: &Path,
    expected_id: &str,
) -> Result<(), BackupError> {
    let point = canonical_restore_point(root, path)?;
    let manifest: BackupManifest =
        serde_json::from_slice(&fs::read(point.join(BACKUP_MANIFEST_FILENAME))?)?;
    if manifest.backup_id != expected_id {
        return Err(BackupError::Verification(
            "retention manifest identity mismatch".into(),
        ));
    }
    fs::remove_dir_all(point).context("prune validated backup")?;
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<(), BackupError> {
    fs::create_dir_all(path).context("create private backup directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), BackupError> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), BackupError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, BackupError> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn path_identity(path: &Path) -> String {
    blake3::hash(path.to_string_lossy().as_bytes()).to_hex()[..16].to_string()
}

fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    struct Fixture {
        root: TempDir,
        config: ServerConfig,
        control: ControlCatalog,
        project_id: String,
    }

    impl Fixture {
        fn new() -> Self {
            let root = TempDir::new().unwrap();
            let data = root.path().join("state");
            let derived = root.path().join("derived");
            let backups = root.path().join("backups");
            fs::create_dir_all(&data).unwrap();
            fs::create_dir_all(&derived).unwrap();
            fs::create_dir_all(&backups).unwrap();
            let config = ServerConfig::from_values(BTreeMap::from([
                ("RCDVLOG_DATA_DIR".into(), data.display().to_string()),
                ("RCDVLOG_DERIVED_DIR".into(), derived.display().to_string()),
                ("RCDVLOG_BACKUP_DIR".into(), backups.display().to_string()),
                (
                    "RCDVLOG_MINIMUM_FREE_BYTES".into(),
                    (64 * 1024 * 1024).to_string(),
                ),
            ]))
            .unwrap();
            let control =
                ControlCatalog::open_or_create(&config.data_dir, &config.diagnostics()).unwrap();
            let relative = "projects/fixture/catalog.sqlite3";
            let project_path = config.data_dir.join(relative);
            fs::create_dir_all(project_path.parent().unwrap()).unwrap();
            let catalog = Catalog::open(&project_path).unwrap();
            catalog
                .set_local_node_id(control.node_id().as_str())
                .unwrap();
            let project = catalog.create_project("Backup fixture").unwrap();
            let connection = open_writable_connection(control.database_path()).unwrap();
            let now = timestamp();
            connection
                .execute(
                    "INSERT INTO project_registry(project_id,replica_relative_path,state,registered_at,created_at,updated_at,baseline_version,baseline_digest,origin_node_id,server_node_id) VALUES(?1,?2,'registered',?3,?3,?3,1,'fixture','node_mac',?4)",
                    params![project.id, relative, now, control.node_id().as_str()],
                )
                .unwrap();
            Self {
                root,
                config,
                control,
                project_id: project.id,
            }
        }

        fn manager(&self) -> BackupManager {
            self.control.backups(self.config.clone())
        }
    }

    #[test]
    fn verified_backup_captures_control_and_every_replica_without_media() {
        let fixture = Fixture::new();
        let result = fixture
            .manager()
            .create_restore_point("manual_one", || false)
            .unwrap();
        assert_eq!(result.manifest.databases.len(), 2);
        assert_eq!(result.manifest.projects[0].project_id, fixture.project_id);
        assert_eq!(
            result.manifest.configuration.allowed_original_mount_count,
            0
        );
        let published = fixture
            .config
            .backup_dir
            .join(&result.published_relative_path);
        let inspection = fixture.manager().inspect_restore_point(&published).unwrap();
        assert!(inspection.valid);
        let manifest_text = fs::read_to_string(published.join(BACKUP_MANIFEST_FILENAME)).unwrap();
        assert!(!manifest_text.contains(fixture.config.derived_dir.to_str().unwrap()));
        assert!(!manifest_text.contains(fixture.config.data_dir.to_str().unwrap()));
        assert!(!published.join("derived").exists());
        assert!(!published.join("originals").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&published).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(published.join("control.sqlite3"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(published.join(BACKUP_MANIFEST_FILENAME))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn cancellation_never_publishes_a_restore_point_and_tampering_is_detected() {
        let fixture = Fixture::new();
        let error = fixture
            .manager()
            .create_restore_point("canceled", || true)
            .unwrap_err();
        assert!(matches!(error, BackupError::Canceled));
        assert!(
            !fixture
                .config
                .backup_dir
                .join("rcdvlog-backup-canceled")
                .exists()
        );
        let result = fixture
            .manager()
            .create_restore_point("tamper", || false)
            .unwrap();
        let published = fixture
            .config
            .backup_dir
            .join(result.published_relative_path);
        OpenOptions::new()
            .append(true)
            .open(published.join("control.sqlite3"))
            .unwrap()
            .write_all(b"tampered")
            .unwrap();
        let inspection = fixture.manager().inspect_restore_point(&published).unwrap();
        assert!(!inspection.valid);
        assert!(
            inspection
                .errors
                .iter()
                .any(|error| error.contains("mismatch"))
        );
    }

    #[test]
    fn restore_to_new_root_preserves_project_state_and_enters_sequence_recovery() {
        let fixture = Fixture::new();
        let connection = open_writable_connection(fixture.control.database_path()).unwrap();
        let now = timestamp();
        let hash = "a".repeat(64);
        let output_hash = "b".repeat(64);
        let settings = "c".repeat(64);
        connection.execute(
            "INSERT INTO media_objects(media_object_id,size_bytes,full_content_hash,verification_state,created_at,updated_at) VALUES('media_restore',10,?1,'verified',?2,?2)",
            params![hash, now],
        ).unwrap();
        connection.execute(
            "INSERT INTO representations(representation_id,media_object_id,source_content_hash,source_size_bytes,kind,recipe_key,settings_identity,state,output_content_hash,byte_length,content_type,created_at,completed_at,updated_at) VALUES('representation_restore','media_restore',?1,10,'proxy','proxy.fixture',?2,'available',?3,99,'video/mp4',?4,?4,?4)",
            params![hash, settings, output_hash, now],
        ).unwrap();
        connection.execute(
            "INSERT INTO representation_locations(representation_id,node_id,location_kind,location_key,relative_path,state,verified_at,updated_at) VALUES('representation_restore',?1,'shared_derived','missing.mp4','missing.mp4','available',?2,?2)",
            params![fixture.control.node_id().as_str(), now],
        ).unwrap();
        drop(connection);
        let result = fixture
            .manager()
            .create_restore_point("restore", || false)
            .unwrap();
        let published = fixture
            .config
            .backup_dir
            .join(result.published_relative_path);
        let target = fixture.root.path().join("restored-state");
        let restored = fixture
            .manager()
            .restore_to_new_data_root(&published, &target)
            .unwrap();
        assert_eq!(restored.project_count, 1);
        assert_eq!(restored.invalidated_representation_locations, 1);
        assert_eq!(
            restored.sequence_recovery_projects,
            vec![fixture.project_id.clone()]
        );
        let restored_project = target.join("projects/fixture/catalog.sqlite3");
        let catalog = Catalog::open_existing(restored_project).unwrap();
        assert_eq!(catalog.list_projects().unwrap()[0].name, "Backup fixture");
        let recovery = catalog.sequence_recovery_state().unwrap();
        assert_eq!(recovery.len(), 1);
        assert_eq!(recovery[0].peer_origin_node_id, "node_mac");
        let restored_control = open_read_only_connection(&target.join("control.sqlite3")).unwrap();
        let location_state: String = restored_control
            .query_row(
                "SELECT state FROM representation_locations WHERE representation_id='representation_restore'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(location_state, "missing");
        assert!(matches!(
            fixture
                .manager()
                .restore_to_new_data_root(&published, &target),
            Err(BackupError::InvalidInput(_))
        ));
    }

    #[test]
    fn backup_requests_are_durable_and_diagnostic_bundle_is_redacted() {
        let fixture = Fixture::new();
        let service = BackupService::new(fixture.manager(), fixture.control.jobs());
        let first = service.request_backup("manual:fixture").unwrap();
        let second = service.request_backup("manual:fixture").unwrap();
        assert_eq!(first.definition.job_id, second.definition.job_id);
        assert_eq!(first.state, crate::JobState::Queued);

        let output = fixture.root.path().join("diagnostics");
        let bundle = fixture
            .manager()
            .generate_diagnostic_bundle(
                &output,
                ReadinessReport::from_checks(vec![crate::ReadinessCheck::ok("fixture")]),
            )
            .unwrap();
        let text = fs::read_to_string(bundle.output_path).unwrap();
        assert!(text.contains("replicaPathIdentity"));
        assert!(!text.contains(fixture.config.data_dir.to_str().unwrap()));
        assert!(!text.to_lowercase().contains("token"));
        assert!(!text.to_lowercase().contains("password"));
    }

    #[test]
    fn retention_keeps_daily_and_weekly_buckets_and_ignores_invalid_directories() {
        let fixture = Fixture::new();
        let template = fixture
            .manager()
            .create_restore_point("retention_template", || false)
            .unwrap();
        let template_path = fixture
            .config
            .backup_dir
            .join(template.published_relative_path);
        let base = Utc::now();
        for day in 1..40 {
            let backup_id = format!("history_{day:02}");
            let point = fixture
                .config
                .backup_dir
                .join(format!("{BACKUP_DIRECTORY_PREFIX}{backup_id}"));
            fs::create_dir_all(point.join("projects")).unwrap();
            fs::copy(
                template_path.join("control.sqlite3"),
                point.join("control.sqlite3"),
            )
            .unwrap();
            let project_name = template.manifest.databases[1]
                .relative_path
                .strip_prefix("projects/")
                .unwrap();
            fs::copy(
                template_path.join(&template.manifest.databases[1].relative_path),
                point.join("projects").join(project_name),
            )
            .unwrap();
            let mut manifest = template.manifest.clone();
            manifest.backup_id = backup_id;
            let completed = base - chrono::Duration::days(day);
            manifest.created_at = completed.to_rfc3339_opts(SecondsFormat::Millis, true);
            manifest.completed_at = manifest.created_at.clone();
            fs::write(
                point.join(BACKUP_MANIFEST_FILENAME),
                serde_json::to_vec_pretty(&manifest).unwrap(),
            )
            .unwrap();
        }
        let invalid = fixture.config.backup_dir.join("rcdvlog-backup-unvalidated");
        fs::create_dir_all(&invalid).unwrap();
        fs::write(invalid.join(BACKUP_MANIFEST_FILENAME), b"not-json").unwrap();

        let pruned = fixture.manager().prune_retention(None).unwrap();
        assert!(!pruned.is_empty());
        let valid_remaining = fixture
            .manager()
            .list_restore_points()
            .unwrap()
            .into_iter()
            .filter(|inspection| inspection.valid)
            .count();
        assert!(valid_remaining >= fixture.config.backup_daily_retention);
        assert!(
            valid_remaining
                <= fixture.config.backup_daily_retention + fixture.config.backup_weekly_retention
        );
        assert!(invalid.exists());
    }

    #[test]
    fn online_backup_allows_concurrent_replica_commits() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let fixture = Fixture::new();
        let replica = fixture
            .config
            .data_dir
            .join("projects/fixture/catalog.sqlite3");
        let connection = Connection::open(&replica).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE backup_writer_fixture(value INTEGER NOT NULL); INSERT INTO backup_writer_fixture VALUES(0);",
            )
            .unwrap();
        drop(connection);
        let stop = Arc::new(AtomicBool::new(false));
        let committed = Arc::new(AtomicUsize::new(0));
        let writer_stop = stop.clone();
        let writer_committed = committed.clone();
        let writer = std::thread::spawn(move || {
            let connection = Connection::open(replica).unwrap();
            connection.busy_timeout(Duration::from_secs(2)).unwrap();
            while !writer_stop.load(Ordering::Relaxed) {
                if connection
                    .execute("UPDATE backup_writer_fixture SET value=value+1", [])
                    .is_ok()
                {
                    writer_committed.fetch_add(1, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        let writer_deadline = Instant::now() + Duration::from_secs(1);
        while committed.load(Ordering::Relaxed) == 0 && Instant::now() < writer_deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let started = Instant::now();
        let result = fixture
            .manager()
            .create_restore_point("concurrent", || false);
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
        assert!(result.is_ok());
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(committed.load(Ordering::Relaxed) > 0);
    }
}
