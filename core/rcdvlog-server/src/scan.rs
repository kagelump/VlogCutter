use chrono::{NaiveTime, SecondsFormat, TimeDelta, Utc};
use chrono_tz::Tz;
use rcdvlog_core::{
    LocationVerificationState, NodeId, ObservationStatus, RootRelativePath, StorageObjectId,
    StorageRootId,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::str::FromStr;
use thiserror::Error;
use uuid::Uuid;

use crate::catalog::{open_read_only_connection, open_writable_connection};
use crate::library::{StorageLibrary, StorageLibraryError};

#[derive(Debug, Error)]
pub enum ScanServiceError {
    #[error("invalid {field}: {reason}")]
    InvalidInput { field: &'static str, reason: String },
    #[error("{entity} {id} was not found")]
    NotFound { entity: &'static str, id: String },
    #[error("scan scope is already active")]
    ScanAlreadyActive,
    #[error(transparent)]
    Library(#[from] StorageLibraryError),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Infrastructure(#[from] anyhow::Error),
}

impl From<ScanServiceError> for crate::jobs::JobEngineError {
    fn from(error: ScanServiceError) -> Self {
        crate::jobs::JobEngineError::Infrastructure(anyhow::anyhow!("{error}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Canceled,
}

impl ScanState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    fn parse_stored(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "canceled" => Some(Self::Canceled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanSource {
    Manual,
    Schedule,
}

impl ScanSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Schedule => "schedule",
        }
    }

    fn parse_stored(value: &str) -> Option<Self> {
        match value {
            "manual" => Some(Self::Manual),
            "schedule" => Some(Self::Schedule),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanRun {
    pub scan_id: String,
    pub node_id: NodeId,
    pub storage_root_id: StorageRootId,
    pub scope_relative_path: Option<RootRelativePath>,
    pub generation_id: String,
    pub root_job_id: String,
    pub scheduled_occurrence_at: Option<String>,
    pub source: ScanSource,
    pub schedule_id: Option<String>,
    pub state: ScanState,
    pub visited_entries: u64,
    pub supported_files: u64,
    pub unchanged_files: u64,
    pub queued_files: u64,
    pub completed_files: u64,
    pub failed_files: u64,
    pub error_code: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSchedule {
    pub schedule_id: String,
    pub storage_root_id: StorageRootId,
    pub scope_relative_path: Option<RootRelativePath>,
    pub enabled: bool,
    pub local_time: String,
    pub timezone: String,
    pub next_run_at: String,
    pub last_enqueued_at: Option<String>,
    pub last_scan_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewScanSchedule {
    pub storage_root_id: StorageRootId,
    pub scope_relative_path: Option<RootRelativePath>,
    pub enabled: bool,
    pub local_time: String,
    pub timezone: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanScheduleUpdate {
    pub enabled: Option<bool>,
    pub local_time: Option<String>,
    pub timezone: Option<String>,
}

/// Configuration for scan-related tools passed from the server process.
#[derive(Debug, Clone)]
pub struct ScanToolConfig {
    pub ffprobe_program: PathBuf,
    pub exiftool_program: Option<PathBuf>,
}

/// Result of startup scan recovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryReport {
    pub total_pending: u64,
    pub recovered: u64,
    pub skipped: u64,
    pub errors: Vec<String>,
}

/// Domain service for manual/scheduled scans and verification reconciliation.
pub struct ScanService {
    library: StorageLibrary,
    tool_config: ScanToolConfig,
}

impl ScanService {
    pub fn new(library: StorageLibrary, tool_config: ScanToolConfig) -> Self {
        Self {
            library,
            tool_config,
        }
    }

    pub fn library(&self) -> &StorageLibrary {
        &self.library
    }

    pub fn tool_config(&self) -> &ScanToolConfig {
        &self.tool_config
    }

    pub fn local_node_id(&self) -> &NodeId {
        self.library.local_node_id()
    }

    // ── Scan runs ──────────────────────────────────────────────────────

    /// Request a scan. Generates stable scan/generation/root-job IDs,
    /// persists the scan row, and enqueues the root job. Returns the existing
    /// active scan if one for the same (node, root, scope) is already queued
    /// or running (after ensuring its root job is also durable).
    pub fn request_scan(
        &self,
        source: ScanSource,
        schedule_id: Option<&str>,
        root_id: &StorageRootId,
        scope: Option<&RootRelativePath>,
    ) -> Result<ScanRun, ScanServiceError> {
        self.library.get_storage_root(root_id)?;
        let mut connection = open_writable_connection(self.library.database_path())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let scope_str = scope.map(RootRelativePath::as_str);
        let existing = transaction
            .query_row(
                r#"
                SELECT scan_id, state, root_job_id FROM scan_runs
                WHERE node_id = ?1 AND storage_root_id = ?2
                  AND scope_relative_path IS NOT DISTINCT FROM ?3
                  AND state IN ('queued', 'running')
                "#,
                params![self.local_node_id().as_str(), root_id.as_str(), scope_str,],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((existing_id, state, root_job_id)) = existing
            && (state == "queued" || state == "running")
        {
            let scan = load_scan(&transaction, &existing_id)?;
            transaction.commit()?;
            // Ensure root job is durable (crash recovery)
            self.ensure_root_job_enqueued(&scan, &root_job_id)?;
            return Ok(scan);
        }

        let scan_id = format!("scan_{}", Uuid::new_v4().simple());
        let generation_id = format!("generation_{}", Uuid::new_v4().simple());
        let root_job_id = format!("job_root_{}", Uuid::new_v4().simple());
        let scheduled_occurrence = if source == ScanSource::Schedule {
            schedule_id.map(|sid| {
                let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
                format!("{sid}|{now}")
            })
        } else {
            None
        };
        let now = now_str();
        transaction.execute(
            r#"
            INSERT INTO scan_runs (
                scan_id, node_id, storage_root_id, scope_relative_path,
                generation_id, root_job_id, scheduled_occurrence_at,
                source, schedule_id, state, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'queued', ?10)
            "#,
            params![
                scan_id,
                self.local_node_id().as_str(),
                root_id.as_str(),
                scope_str,
                generation_id,
                root_job_id,
                scheduled_occurrence.as_deref(),
                source.as_str(),
                schedule_id,
                now,
            ],
        )?;
        let scan = load_scan(&transaction, &scan_id)?;
        transaction.commit()?;

        // Enqueue the root job AFTER the scan row is committed.
        // If enqueue fails, the scan remains queued and will be recovered.
        let engine = crate::JobEngine::new(
            self.library.database_path().clone(),
            self.library.local_node_id().clone(),
        );
        let scope_cloned: Option<RootRelativePath> = scope.cloned();
        let root_job_def = crate::scan_handler::build_root_scan_job(
            &scan_id,
            &generation_id,
            root_id,
            &scope_cloned,
            self.library.local_node_id(),
        )
        .map_err(|e| {
            ScanServiceError::Infrastructure(anyhow::anyhow!("failed to build root scan job: {e}"))
        })?;
        engine
            .enqueue_with_id(&root_job_id, root_job_def)
            .map_err(|e| {
                // Leave the scan row recoverable; the root job can be retried at startup
                tracing::warn!(
                    scan_id = %scan_id,
                    root_job_id = %root_job_id,
                    error = %e,
                    "root job enqueue after scan creation failed; scan will need recovery"
                );
                ScanServiceError::Infrastructure(anyhow::anyhow!(
                    "root job enqueue failed; scan is queued but needs manual recovery: {e}"
                ))
            })?;

        Ok(scan)
    }

    /// Ensure the root job for an existing scan exists. Used for idempotent
    /// retry and startup recovery.
    pub fn ensure_root_job_enqueued(
        &self,
        scan: &ScanRun,
        root_job_id: &str,
    ) -> Result<(), ScanServiceError> {
        let engine = crate::JobEngine::new(
            self.library.database_path().clone(),
            self.library.local_node_id().clone(),
        );
        // Check if the root job already exists
        if let Ok(parsed_id) = rcdvlog_core::JobId::parse(root_job_id.to_string())
            && engine.get_job(&parsed_id).is_ok()
        {
            return Ok(());
        }
        // Re-enqueue the root job
        let root_job_def = crate::scan_handler::build_root_scan_job(
            &scan.scan_id,
            &scan.generation_id,
            &scan.storage_root_id,
            &scan.scope_relative_path,
            self.library.local_node_id(),
        )
        .map_err(|e| {
            ScanServiceError::Infrastructure(anyhow::anyhow!("failed to build root scan job: {e}"))
        })?;
        engine
            .enqueue_with_id(root_job_id, root_job_def)
            .map_err(|e| {
                ScanServiceError::Infrastructure(anyhow::anyhow!(
                    "failed to re-enqueue root job: {e}"
                ))
            })?;
        Ok(())
    }

    /// Startup recovery: lists all queued/running scan rows and idempotently
    /// calls ensure_root_job_enqueued. Safe if the root job already exists,
    /// already succeeded, or the scan already transitioned terminally.
    /// Returns counts of recovered scans and errors (without exposing paths).
    pub fn recover_active_scan_jobs(&self) -> Result<RecoveryReport, ScanServiceError> {
        let connection = open_read_only_connection(self.library.database_path())?;
        let mut statement = connection.prepare(
            r#"
            SELECT scan_id, node_id, storage_root_id, scope_relative_path,
                   generation_id, root_job_id, scheduled_occurrence_at,
                   source, schedule_id, state,
                   visited_entries, supported_files, unchanged_files,
                   queued_files, completed_files, failed_files,
                   error_code, created_at, started_at, completed_at
            FROM scan_runs
            WHERE node_id = ?1 AND state IN ('queued', 'running')
            ORDER BY created_at
            "#,
        )?;
        let pending: Vec<ScanRun> = statement
            .query_map(params![self.local_node_id().as_str()], scan_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);

        let total = pending.len();
        let mut recovered = 0u64;
        let mut skipped = 0u64;
        let mut errors = Vec::new();

        for scan in &pending {
            let root_job_id = scan.root_job_id.clone();
            match self.ensure_root_job_enqueued(scan, &root_job_id) {
                Ok(()) => {
                    recovered += 1;
                    tracing::info!(
                        scan_id = %scan.scan_id,
                        root_job_id = %root_job_id,
                        "recovered scan root job"
                    );
                }
                Err(e) => {
                    skipped += 1;
                    let err_msg = format!("{e}");
                    tracing::warn!(
                        scan_id = %scan.scan_id,
                        error = %err_msg,
                        "scan recovery skipped"
                    );
                    errors.push(err_msg);
                }
            }
        }

        tracing::info!(
            total_pending = total,
            recovered = recovered,
            skipped = skipped,
            "startup scan recovery complete"
        );

        Ok(RecoveryReport {
            total_pending: total as u64,
            recovered,
            skipped,
            errors,
        })
    }

    /// Mark a scan as running. Used by the root handler.
    pub fn start_scan(&self, scan_id: &str) -> Result<ScanRun, ScanServiceError> {
        let mut connection = open_writable_connection(self.library.database_path())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            r#"
            UPDATE scan_runs SET state = 'running', started_at = ?2
            WHERE scan_id = ?1 AND state = 'queued'
            "#,
            params![scan_id, now_str()],
        )?;
        let scan = load_scan(&transaction, scan_id)?;
        transaction.commit()?;
        Ok(scan)
    }

    /// Update discovery counter incrementally (used by root handler for
    /// progressive counter tracking during discovery).
    pub fn increment_scan_counters(
        &self,
        scan_id: &str,
        visited: u64,
        supported: u64,
        unchanged: u64,
        queued: u64,
    ) -> Result<(), ScanServiceError> {
        let mut connection = open_writable_connection(self.library.database_path())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            r#"
            UPDATE scan_runs SET
                visited_entries = visited_entries + ?2,
                supported_files = supported_files + ?3,
                unchanged_files = unchanged_files + ?4,
                queued_files = queued_files + ?5
            WHERE scan_id = ?1
            "#,
            params![
                scan_id,
                visited as i64,
                supported as i64,
                unchanged as i64,
                queued as i64,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Set discovery counters to absolute values (idempotent on root retry).
    /// Used when the root job is retried and re-walks: counters should reflect
    /// the current walk, not accumulate across retries.
    pub fn set_scan_counters_absolute(
        &self,
        scan_id: &str,
        visited: u64,
        supported: u64,
        unchanged: u64,
        queued: u64,
    ) -> Result<(), ScanServiceError> {
        let mut connection = open_writable_connection(self.library.database_path())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            r#"
            UPDATE scan_runs SET
                visited_entries = ?2,
                supported_files = ?3,
                unchanged_files = ?4,
                queued_files = ?5
            WHERE scan_id = ?1
            "#,
            params![
                scan_id,
                visited as i64,
                supported as i64,
                unchanged as i64,
                queued as i64,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Mark a scan as completed (succeeded or failed).
    pub fn complete_scan(
        &self,
        scan_id: &str,
        state: ScanState,
        error_code: Option<&str>,
    ) -> Result<ScanRun, ScanServiceError> {
        let mut connection = open_writable_connection(self.library.database_path())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now_str();
        transaction.execute(
            r#"
            UPDATE scan_runs SET state = ?2, completed_at = ?3,
                error_code = ?4
            WHERE scan_id = ?1
            "#,
            params![scan_id, state.as_str(), now, error_code],
        )?;
        let scan = load_scan(&transaction, scan_id)?;
        transaction.commit()?;
        Ok(scan)
    }

    /// Increment completed/failed file counters.
    pub fn increment_file_counters(
        &self,
        scan_id: &str,
        completed: u64,
        failed: u64,
    ) -> Result<(), ScanServiceError> {
        let mut connection = open_writable_connection(self.library.database_path())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            r#"
            UPDATE scan_runs SET
                completed_files = completed_files + ?2,
                failed_files = failed_files + ?3
            WHERE scan_id = ?1
            "#,
            params![scan_id, completed as i64, failed as i64],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Get a single scan run.
    pub fn get_scan(&self, scan_id: &str) -> Result<ScanRun, ScanServiceError> {
        let connection = open_read_only_connection(self.library.database_path())?;
        load_scan(&connection, scan_id)
    }

    /// List scan runs with cursor pagination.
    pub fn list_scans(
        &self,
        limit: u32,
        after: Option<&str>,
    ) -> Result<Vec<ScanRun>, ScanServiceError> {
        if limit == 0 || limit > 200 {
            return Err(ScanServiceError::InvalidInput {
                field: "limit",
                reason: "must be 1–200".into(),
            });
        }
        let connection = open_read_only_connection(self.library.database_path())?;
        let mut statement = connection.prepare(
            r#"
            SELECT scan_id, node_id, storage_root_id, scope_relative_path,
                   generation_id, root_job_id, scheduled_occurrence_at,
                   source, schedule_id, state,
                   visited_entries, supported_files, unchanged_files,
                   queued_files, completed_files, failed_files,
                   error_code, created_at, started_at, completed_at
            FROM scan_runs
            WHERE (?1 IS NULL OR created_at < ?1)
            ORDER BY created_at DESC
            LIMIT ?2
            "#,
        )?;
        statement
            .query_map(params![after, limit], scan_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    // ── Schedules ──────────────────────────────────────────────────────

    /// Create a new scan schedule.
    pub fn create_schedule(
        &self,
        input: NewScanSchedule,
    ) -> Result<ScanSchedule, ScanServiceError> {
        validate_local_time(&input.local_time)?;
        validate_timezone(&input.timezone)?;
        self.library.get_storage_root(&input.storage_root_id)?;
        let schedule_id = format!("schedule_{}", Uuid::new_v4().simple());
        let next_run_at = compute_next_run_utc(&input.local_time, &input.timezone)?;
        let now = now_str();
        let mut connection = open_writable_connection(self.library.database_path())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            r#"
            INSERT INTO scan_schedules (
                schedule_id, storage_root_id, scope_relative_path,
                enabled, local_time, timezone, next_run_at, created_at, updated_at
            ) VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6, ?7, ?7)
            "#,
            params![
                schedule_id,
                input.storage_root_id.as_str(),
                input
                    .scope_relative_path
                    .as_ref()
                    .map(RootRelativePath::as_str),
                input.local_time,
                input.timezone,
                next_run_at,
                now,
            ],
        )?;
        // Set enabled separately to avoid ID extraction complexity
        if input.enabled {
            transaction.execute(
                "UPDATE scan_schedules SET enabled = 1 WHERE schedule_id = ?1",
                [&schedule_id],
            )?;
        }
        let schedule = load_schedule(&transaction, &schedule_id)?;
        transaction.commit()?;
        Ok(schedule)
    }

    /// Update a schedule's properties.
    pub fn update_schedule(
        &self,
        schedule_id: &str,
        update: ScanScheduleUpdate,
    ) -> Result<ScanSchedule, ScanServiceError> {
        let mut connection = open_writable_connection(self.library.database_path())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut schedule = load_schedule(&transaction, schedule_id)?;

        if let Some(enabled) = update.enabled {
            schedule.enabled = enabled;
        }
        if let Some(ref local_time) = update.local_time {
            validate_local_time(local_time)?;
            schedule.local_time = local_time.clone();
        }
        if let Some(ref timezone) = update.timezone {
            validate_timezone(timezone)?;
            schedule.timezone = timezone.clone();
        }
        // Recalculate next_run_at if time or timezone changed
        if update.local_time.is_some() || update.timezone.is_some() {
            schedule.next_run_at = compute_next_run_utc(&schedule.local_time, &schedule.timezone)?;
        }
        let now = now_str();
        transaction.execute(
            r#"
            UPDATE scan_schedules SET
                enabled = ?2, local_time = ?3, timezone = ?4,
                next_run_at = ?5, updated_at = ?6
            WHERE schedule_id = ?1
            "#,
            params![
                schedule_id,
                schedule.enabled,
                schedule.local_time,
                schedule.timezone,
                schedule.next_run_at,
                now,
            ],
        )?;
        let schedule = load_schedule(&transaction, schedule_id)?;
        transaction.commit()?;
        Ok(schedule)
    }

    /// Get a single schedule.
    pub fn get_schedule(&self, schedule_id: &str) -> Result<ScanSchedule, ScanServiceError> {
        let connection = open_read_only_connection(self.library.database_path())?;
        load_schedule(&connection, schedule_id)
    }

    /// List all schedules.
    pub fn list_schedules(&self) -> Result<Vec<ScanSchedule>, ScanServiceError> {
        let connection = open_read_only_connection(self.library.database_path())?;
        let mut statement = connection.prepare(
            r#"
            SELECT schedule_id, storage_root_id, scope_relative_path,
                   enabled, local_time, timezone, next_run_at,
                   last_enqueued_at, last_scan_id, created_at, updated_at
            FROM scan_schedules
            ORDER BY created_at
            "#,
        )?;
        statement
            .query_map([], schedule_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// List due enabled schedules (where next_run_at <= now).
    pub fn list_due_schedules(&self) -> Result<Vec<ScanSchedule>, ScanServiceError> {
        let now = now_str();
        let connection = open_read_only_connection(self.library.database_path())?;
        let mut statement = connection.prepare(
            r#"
            SELECT schedule_id, storage_root_id, scope_relative_path,
                   enabled, local_time, timezone, next_run_at,
                   last_enqueued_at, last_scan_id, created_at, updated_at
            FROM scan_schedules
            WHERE enabled = 1 AND next_run_at <= ?1
            ORDER BY next_run_at
            "#,
        )?;
        statement
            .query_map([now], schedule_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Record that a schedule triggered a scan, advancing next_run_at.
    pub fn record_schedule_trigger(
        &self,
        schedule_id: &str,
        scan_id: &str,
        next_run_at: &str,
    ) -> Result<(), ScanServiceError> {
        let now = now_str();
        let mut connection = open_writable_connection(self.library.database_path())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            r#"
            UPDATE scan_schedules SET
                next_run_at = ?2,
                last_enqueued_at = ?3,
                last_scan_id = ?4,
                updated_at = ?3
            WHERE schedule_id = ?1
            "#,
            params![schedule_id, next_run_at, now, scan_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    // ── Diagnostics ────────────────────────────────────────────────────

    /// List file diagnostics for a scan.
    pub fn list_scan_diagnostics(
        &self,
        scan_id: &str,
        limit: u32,
        after: Option<&str>,
    ) -> Result<Vec<crate::media_facts::ScanFileDiagnostic>, ScanServiceError> {
        if limit == 0 || limit > 200 {
            return Err(ScanServiceError::InvalidInput {
                field: "limit",
                reason: "must be 1–200".into(),
            });
        }
        let connection = open_read_only_connection(self.library.database_path())?;
        let mut statement = connection.prepare(
            r#"
            SELECT diagnostic_id, scan_id, storage_object_id,
                   stage, code, safe_path_identity, retryable, recorded_at
            FROM scan_file_diagnostics
            WHERE scan_id = ?1 AND (?2 IS NULL OR recorded_at < ?2)
            ORDER BY recorded_at DESC
            LIMIT ?3
            "#,
        )?;
        statement
            .query_map(params![scan_id, after, limit], |row| {
                Ok(crate::media_facts::ScanFileDiagnostic {
                    diagnostic_id: row.get(0)?,
                    scan_id: row.get(1)?,
                    storage_object_id: row
                        .get::<_, Option<String>>(2)?
                        .map(StorageObjectId::parse)
                        .transpose()
                        .map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?,
                    stage: row.get(3)?,
                    code: row.get(4)?,
                    safe_path_identity: row.get(5)?,
                    retryable: row.get(6)?,
                    recorded_at: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}

// ── Transaction helpers for job handler commit_result ──────────────────

/// Insert a scan-run job link. Used by handlers to track child jobs.
pub(crate) fn insert_scan_run_job(
    transaction: &Transaction<'_>,
    scan_id: &str,
    job_id: &str,
    storage_object_id: Option<&StorageObjectId>,
    stage: &str,
) -> Result<(), ScanServiceError> {
    transaction.execute(
        r#"
        INSERT OR IGNORE INTO scan_run_jobs (
            scan_id, job_id, storage_object_id, stage
        ) VALUES (?1, ?2, ?3, ?4)
        "#,
        params![
            scan_id,
            job_id,
            storage_object_id.map(StorageObjectId::as_str),
            stage,
        ],
    )?;
    Ok(())
}

/// Check whether any child jobs for a scan are still non-terminal.
/// Returns true when all linked child jobs have settled.
#[allow(dead_code)]
pub(crate) fn scan_children_settled(
    transaction: &Transaction<'_>,
    scan_id: &str,
) -> Result<bool, ScanServiceError> {
    let pending: i64 = transaction.query_row(
        r#"
        SELECT COUNT(*) FROM scan_run_jobs srj
        JOIN jobs j ON j.job_id = srj.job_id
        WHERE srj.scan_id = ?1
          AND srj.stage IN ('quick_fingerprint', 'probe', 'full_hash')
          AND j.state IN ('queued', 'running', 'retry_wait')
        "#,
        params![scan_id],
        |row| row.get(0),
    )?;
    Ok(pending == 0)
}

/// Read-only variant of scan_children_settled for validation before commit.
pub(crate) fn scan_children_settled_read_only(
    connection: &rusqlite::Connection,
    scan_id: &str,
) -> Result<bool, ScanServiceError> {
    let pending: i64 = connection.query_row(
        r#"
        SELECT COUNT(*) FROM scan_run_jobs srj
        JOIN jobs j ON j.job_id = srj.job_id
        WHERE srj.scan_id = ?1
          AND srj.stage IN ('quick_fingerprint', 'probe', 'full_hash')
          AND j.state IN ('queued', 'running', 'retry_wait')
        "#,
        params![scan_id],
        |row| row.get(0),
    )?;
    Ok(pending == 0)
}

/// Count settled/failed files by distinct storage object, not job count.
/// completed_files = distinct objects with NO non-terminal child jobs.
/// failed_files = subset of completed_files with at least one terminal failure.
/// This satisfies the constraint `failed_files <= completed_files` even when
/// every child job for a file fails.
#[allow(dead_code)]
pub(crate) fn scan_child_outcomes(
    transaction: &Transaction<'_>,
    scan_id: &str,
) -> Result<(u64, u64), ScanServiceError> {
    let completed: i64 = transaction.query_row(
        r#"
        SELECT COUNT(DISTINCT srj.storage_object_id)
        FROM scan_run_jobs srj
        WHERE srj.scan_id = ?1
          AND srj.stage IN ('quick_fingerprint', 'probe', 'full_hash')
          AND srj.storage_object_id IS NOT NULL
          AND srj.storage_object_id NOT IN (
              SELECT srj2.storage_object_id
              FROM scan_run_jobs srj2
              JOIN jobs j2 ON j2.job_id = srj2.job_id
              WHERE srj2.scan_id = ?1
                AND srj2.stage IN ('quick_fingerprint', 'probe', 'full_hash')
                AND srj2.storage_object_id IS NOT NULL
                AND j2.state IN ('queued', 'running', 'retry_wait')
          )
        "#,
        params![scan_id],
        |row| row.get(0),
    )?;

    let failed: i64 = transaction.query_row(
        r#"
        SELECT COUNT(DISTINCT srj.storage_object_id)
        FROM scan_run_jobs srj
        JOIN jobs j ON j.job_id = srj.job_id
        WHERE srj.scan_id = ?1
          AND srj.stage IN ('quick_fingerprint', 'probe', 'full_hash')
          AND srj.storage_object_id IS NOT NULL
          AND j.state IN ('failed', 'canceled')
          AND srj.storage_object_id NOT IN (
              SELECT srj2.storage_object_id
              FROM scan_run_jobs srj2
              JOIN jobs j2 ON j2.job_id = srj2.job_id
              WHERE srj2.scan_id = ?1
                AND srj2.stage IN ('quick_fingerprint', 'probe', 'full_hash')
                AND srj2.storage_object_id IS NOT NULL
                AND j2.state IN ('queued', 'running', 'retry_wait')
          )
        "#,
        params![scan_id],
        |row| row.get(0),
    )?;

    Ok((completed as u64, failed as u64))
}

/// Upsert a storage object by logical path (or find existing).
#[allow(dead_code)]
pub(crate) fn upsert_storage_object(
    transaction: &Transaction<'_>,
    object_id: &StorageObjectId,
    root_id: &StorageRootId,
    relative_path: &RootRelativePath,
    size_bytes: Option<u64>,
    modified_at: Option<&str>,
) -> Result<(), ScanServiceError> {
    let size = to_i64(size_bytes)?;
    transaction.execute(
        r#"
        INSERT INTO storage_objects (
            storage_object_id, storage_root_id, relative_path,
            size_bytes, modified_at, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
        ON CONFLICT(storage_object_id, storage_root_id) DO UPDATE SET
            size_bytes = COALESCE(excluded.size_bytes, storage_objects.size_bytes),
            modified_at = COALESCE(excluded.modified_at, storage_objects.modified_at),
            updated_at = excluded.updated_at
        "#,
        params![
            object_id.as_str(),
            root_id.as_str(),
            relative_path.as_str(),
            size,
            modified_at,
            now_str(),
        ],
    )?;
    Ok(())
}

/// Find a storage object by root and relative path.
#[allow(dead_code)]
pub(crate) fn find_storage_object(
    transaction: &Transaction<'_>,
    root_id: &StorageRootId,
    relative_path: &RootRelativePath,
) -> Result<Option<StorageObjectId>, ScanServiceError> {
    let id_str: Option<String> = transaction
        .query_row(
            r#"
            SELECT storage_object_id FROM storage_objects
            WHERE storage_root_id = ?1 AND relative_path = ?2
            "#,
            params![root_id.as_str(), relative_path.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    id_str
        .map(|s| {
            StorageObjectId::parse(s).map_err(|e| {
                ScanServiceError::Database(rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                ))
            })
        })
        .transpose()
}

/// Get current location/media link for a storage object.
#[allow(dead_code)]
pub(crate) fn get_storage_location_media(
    transaction: &Transaction<'_>,
    storage_object_id: &StorageObjectId,
) -> Result<Option<(rcdvlog_core::MediaObjectId, LocationVerificationState)>, ScanServiceError> {
    let row: Option<(String, String)> = transaction
        .query_row(
            r#"
            SELECT media_object_id, verification_state
            FROM media_object_locations
            WHERE storage_object_id = ?1
            "#,
            params![storage_object_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    row.map(|(media_id_str, state_str)| {
        let media_id = rcdvlog_core::MediaObjectId::parse(media_id_str).map_err(|e| {
            ScanServiceError::Database(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(e),
            ))
        })?;
        let state = LocationVerificationState::parse_stored(&state_str).ok_or_else(|| {
            ScanServiceError::Database(rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                "invalid location verification state".into(),
            ))
        })?;
        Ok((media_id, state))
    })
    .transpose()
}

/// Record an asset observation in the scan generation.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_scan_observation(
    transaction: &Transaction<'_>,
    generation_id: &str,
    node_id: &NodeId,
    root_id: &StorageRootId,
    storage_object_id: &StorageObjectId,
    status: ObservationStatus,
    observed_size_bytes: Option<u64>,
    observed_quick_fingerprint: Option<&str>,
) -> Result<(), ScanServiceError> {
    let size = to_i64(observed_size_bytes)?;
    transaction.execute(
        r#"
        INSERT INTO node_asset_observations (
            generation_id, node_id, storage_root_id, storage_object_id,
            status, observed_size_bytes, observed_quick_fingerprint, observed_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        ON CONFLICT(generation_id, storage_object_id) DO NOTHING
        "#,
        params![
            generation_id,
            node_id.as_str(),
            root_id.as_str(),
            storage_object_id.as_str(),
            status.as_str(),
            size,
            observed_quick_fingerprint,
            now_str(),
        ],
    )?;
    Ok(())
}

// ── Public helpers ────────────────────────────────────────────────────

/// Validate IANA timezone name. Returns the Tz if valid.
pub fn validate_timezone(tz_name: &str) -> Result<Tz, ScanServiceError> {
    Tz::from_str(tz_name).map_err(|_| ScanServiceError::InvalidInput {
        field: "timezone",
        reason: format!("unknown or unsupported IANA timezone: {tz_name}"),
    })
}

/// Validate HH:MM local time format.
pub fn validate_local_time(value: &str) -> Result<(), ScanServiceError> {
    if value.len() != 5 || value.as_bytes().get(2) != Some(&b':') {
        return Err(ScanServiceError::InvalidInput {
            field: "localTime",
            reason: "must be HH:MM format".into(),
        });
    }
    NaiveTime::parse_from_str(value, "%H:%M").map_err(|_| ScanServiceError::InvalidInput {
        field: "localTime",
        reason: "must be a valid wall-clock time".into(),
    })?;
    Ok(())
}

/// Compute the next UTC instant a daily HH:MM schedule in a given timezone should fire.
pub fn compute_next_run_utc(local_time: &str, tz_name: &str) -> Result<String, ScanServiceError> {
    let tz = validate_timezone(tz_name)?;
    let time = NaiveTime::parse_from_str(local_time, "%H:%M").map_err(|_| {
        ScanServiceError::InvalidInput {
            field: "localTime",
            reason: "must be a valid wall-clock time".into(),
        }
    })?;
    let now_utc = Utc::now();
    // Try today in target TZ, then fall forward day by day until we find a future instant.
    let now_in_tz = now_utc.with_timezone(&tz);
    let today_local = now_in_tz.date_naive().and_time(time).and_local_timezone(tz);
    let candidate = match today_local {
        chrono::LocalResult::Single(dt) => dt,
        chrono::LocalResult::Ambiguous(earliest, _) => earliest,
        chrono::LocalResult::None => {
            // DST gap: use the next valid instant
            let next_day = now_in_tz
                .date_naive()
                .succ_opt()
                .unwrap_or_else(|| now_in_tz.date_naive());
            next_day
                .and_time(time)
                .and_local_timezone(tz)
                .earliest()
                .unwrap_or_else(|| (now_utc + TimeDelta::hours(1)).with_timezone(&tz))
        }
    };
    let candidate_utc = candidate.with_timezone(&Utc);
    // If candidate is in the past, advance to tomorrow
    let next = if candidate_utc <= now_utc {
        let next_day = candidate
            .date_naive()
            .succ_opt()
            .unwrap_or_else(|| candidate.date_naive());
        next_day
            .and_time(time)
            .and_local_timezone(tz)
            .single()
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|| candidate_utc + TimeDelta::hours(24))
    } else {
        candidate_utc
    };
    Ok(next.to_rfc3339_opts(SecondsFormat::Millis, true))
}

/// Generate a safe path identity from a root-relative path (a BLAKE3 hash).
pub fn safe_path_identity(relative_path: &RootRelativePath) -> String {
    blake3::hash(relative_path.as_str().as_bytes())
        .to_hex()
        .to_string()[..32]
        .to_string()
}

// ── Row loaders ───────────────────────────────────────────────────────

fn scan_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScanRun> {
    Ok(ScanRun {
        scan_id: row.get(0)?,
        node_id: NodeId::parse(row.get::<_, String>(1)?).map_err(parse_error(1))?,
        storage_root_id: StorageRootId::parse(row.get::<_, String>(2)?).map_err(parse_error(2))?,
        scope_relative_path: row
            .get::<_, Option<String>>(3)?
            .map(|s| RootRelativePath::parse(s).map_err(parse_error(3)))
            .transpose()?,
        generation_id: row.get(4)?,
        root_job_id: row.get(5)?,
        scheduled_occurrence_at: row.get(6)?,
        source: ScanSource::parse_stored(&row.get::<_, String>(7)?)
            .ok_or_else(|| stored_value_error(7, "scan source"))?,
        schedule_id: row.get(8)?,
        state: ScanState::parse_stored(&row.get::<_, String>(9)?)
            .ok_or_else(|| stored_value_error(9, "scan state"))?,
        visited_entries: from_i64_row(row.get(10)?, 10)?,
        supported_files: from_i64_row(row.get(11)?, 11)?,
        unchanged_files: from_i64_row(row.get(12)?, 12)?,
        queued_files: from_i64_row(row.get(13)?, 13)?,
        completed_files: from_i64_row(row.get(14)?, 14)?,
        failed_files: from_i64_row(row.get(15)?, 15)?,
        error_code: row.get(16)?,
        created_at: row.get(17)?,
        started_at: row.get(18)?,
        completed_at: row.get(19)?,
    })
}

fn schedule_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScanSchedule> {
    Ok(ScanSchedule {
        schedule_id: row.get(0)?,
        storage_root_id: StorageRootId::parse(row.get::<_, String>(1)?).map_err(parse_error(1))?,
        scope_relative_path: row
            .get::<_, Option<String>>(2)?
            .map(|s| RootRelativePath::parse(s).map_err(parse_error(2)))
            .transpose()?,
        enabled: row.get(3)?,
        local_time: row.get(4)?,
        timezone: row.get(5)?,
        next_run_at: row.get(6)?,
        last_enqueued_at: row.get(7)?,
        last_scan_id: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn load_scan(
    connection: &rusqlite::Connection,
    scan_id: &str,
) -> Result<ScanRun, ScanServiceError> {
    connection
        .query_row(
            r#"
            SELECT scan_id, node_id, storage_root_id, scope_relative_path,
                   generation_id, root_job_id, scheduled_occurrence_at,
                   source, schedule_id, state,
                   visited_entries, supported_files, unchanged_files,
                   queued_files, completed_files, failed_files,
                   error_code, created_at, started_at, completed_at
            FROM scan_runs WHERE scan_id = ?1
            "#,
            [scan_id],
            scan_from_row,
        )
        .optional()?
        .ok_or_else(|| ScanServiceError::NotFound {
            entity: "scan run",
            id: scan_id.to_string(),
        })
}

fn load_schedule(
    connection: &rusqlite::Connection,
    schedule_id: &str,
) -> Result<ScanSchedule, ScanServiceError> {
    connection
        .query_row(
            r#"
            SELECT schedule_id, storage_root_id, scope_relative_path,
                   enabled, local_time, timezone, next_run_at,
                   last_enqueued_at, last_scan_id, created_at, updated_at
            FROM scan_schedules WHERE schedule_id = ?1
            "#,
            [schedule_id],
            schedule_from_row,
        )
        .optional()?
        .ok_or_else(|| ScanServiceError::NotFound {
            entity: "scan schedule",
            id: schedule_id.to_string(),
        })
}

fn to_i64(value: Option<u64>) -> Result<Option<i64>, ScanServiceError> {
    value
        .map(|v| {
            i64::try_from(v).map_err(|_| ScanServiceError::InvalidInput {
                field: "size_bytes",
                reason: "exceeds SQLite integer range".into(),
            })
        })
        .transpose()
}

fn from_i64_row(value: i64, column: usize) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            "counter must be non-negative".into(),
        )
    })
}

fn parse_error<E: std::error::Error + Send + Sync + 'static>(
    column: usize,
) -> impl FnOnce(E) -> rusqlite::Error {
    move |error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    }
}

fn stored_value_error(column: usize, kind: &'static str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        rusqlite::types::Type::Text,
        format!("invalid stored {kind} value").into(),
    )
}

fn now_str() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{LocalNodeRootBindingInput, NewStorageRoot};
    use crate::{ControlCatalog, ServerConfig, StorageLibrary};
    use rcdvlog_core::{
        BindingVerificationStatus, RootAssociationConfirmation, RootAssociationConfirmationStatus,
        RootAssociationEvidence, StorageAccessMode, StorageProtocol, StorageRootId,
    };
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, ControlCatalog, StorageLibrary) {
        let root = TempDir::new().unwrap();
        let data = root.path().join("state");
        let derived = root.path().join("derived");
        let backups = root.path().join("backups");
        let original = root.path().join("original");
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(&derived).unwrap();
        fs::create_dir_all(&backups).unwrap();
        fs::create_dir_all(&original).unwrap();
        let config = ServerConfig::from_values(BTreeMap::from([
            ("RCDVLOG_DATA_DIR".to_string(), data.display().to_string()),
            (
                "RCDVLOG_DERIVED_DIR".to_string(),
                derived.display().to_string(),
            ),
            (
                "RCDVLOG_BACKUP_DIR".to_string(),
                backups.display().to_string(),
            ),
            (
                "RCDVLOG_ALLOWED_ORIGINAL_MOUNTS".to_string(),
                original.display().to_string(),
            ),
            ("RCDVLOG_FFPROBE".to_string(), "ffprobe".to_string()),
            ("RCDVLOG_EXIFTOOL".to_string(), "exiftool".to_string()),
        ]))
        .unwrap();
        let catalog = ControlCatalog::open_or_create(&data, &config.diagnostics()).unwrap();
        let library = catalog.storage_library();
        (root, catalog, library)
    }

    fn create_root(library: &StorageLibrary, id: &str) -> StorageRootId {
        let id = StorageRootId::parse(id).unwrap();
        library
            .create_storage_root(NewStorageRoot {
                id: id.clone(),
                display_name: format!("Root {id}"),
                immutable_original: true,
            })
            .unwrap();
        id
    }

    fn confirm_local_binding(
        library: &StorageLibrary,
        root_id: &StorageRootId,
        mount: &std::path::Path,
    ) {
        let node = library.local_node_id().clone();
        library
            .upsert_local_node_root_binding(LocalNodeRootBindingInput {
                storage_root_id: root_id.clone(),
                local_mount_path: mount.to_path_buf(),
                root_subpath: None,
                protocol: StorageProtocol::Local,
                access_mode: StorageAccessMode::ReadOnly,
                verification_status: BindingVerificationStatus::Confirmed,
                enabled: true,
                association_evidence: Some(RootAssociationEvidence {
                    matching_relative_paths: 1,
                    matching_quick_fingerprints: 0,
                    matching_full_hashes: 0,
                    mismatches: 0,
                    sampled_relative_paths: vec![],
                    captured_at: "2026-07-20T00:00:00Z".to_string(),
                }),
                confirmation: RootAssociationConfirmation {
                    status: RootAssociationConfirmationStatus::Confirmed,
                    confirmed_by_node_id: Some(node),
                    confirmed_at: Some("2026-07-20T00:01:00Z".to_string()),
                },
            })
            .unwrap();
    }

    fn make_scan_service(library: &StorageLibrary) -> ScanService {
        let tool_config = ScanToolConfig {
            ffprobe_program: std::path::PathBuf::from("ffprobe"),
            exiftool_program: Some(std::path::PathBuf::from("exiftool")),
        };
        ScanService::new(library.clone(), tool_config)
    }

    #[test]
    fn schedule_time_and_timezone_validation() {
        let (_root, _catalog, library) = fixture();
        let root_id = create_root(&library, "root_sched_val");
        let scan_service = make_scan_service(&library);

        // Valid schedule
        let sched = scan_service
            .create_schedule(NewScanSchedule {
                storage_root_id: root_id.clone(),
                scope_relative_path: None,
                enabled: false,
                local_time: "03:00".into(),
                timezone: "UTC".into(),
            })
            .unwrap();
        assert!(!sched.enabled);
        assert_eq!(sched.local_time, "03:00");

        // Invalid timezone
        assert!(
            scan_service
                .create_schedule(NewScanSchedule {
                    storage_root_id: root_id.clone(),
                    scope_relative_path: None,
                    enabled: false,
                    local_time: "03:00".into(),
                    timezone: "Not/A_Real_Zone".into(),
                })
                .is_err()
        );

        // Invalid local time
        assert!(
            scan_service
                .create_schedule(NewScanSchedule {
                    storage_root_id: root_id.clone(),
                    scope_relative_path: None,
                    enabled: false,
                    local_time: "25:00".into(),
                    timezone: "UTC".into(),
                })
                .is_err()
        );

        assert!(
            scan_service
                .create_schedule(NewScanSchedule {
                    storage_root_id: root_id.clone(),
                    scope_relative_path: None,
                    enabled: false,
                    local_time: "bad".into(),
                    timezone: "UTC".into(),
                })
                .is_err()
        );
    }

    #[test]
    fn manual_scan_is_idempotent_while_active() {
        let (_root, _catalog, library) = fixture();
        let root_id = create_root(&library, "root_scan_idem");
        let mount = _root.path().join("mount");
        fs::create_dir_all(&mount).unwrap();
        confirm_local_binding(&library, &root_id, &mount);
        let scan_service = make_scan_service(&library);

        let first = scan_service
            .request_scan(ScanSource::Manual, None, &root_id, None)
            .unwrap();
        assert_eq!(first.state, ScanState::Queued);

        // Second request for same scope returns the existing queued scan
        let second = scan_service
            .request_scan(ScanSource::Manual, None, &root_id, None)
            .unwrap();
        assert_eq!(second.scan_id, first.scan_id);
        assert_eq!(second.state, ScanState::Queued);
    }

    #[test]
    fn schedules_persist_and_reload_after_reopen() {
        let (_root, catalog, library) = fixture();
        let root_id = create_root(&library, "root_sched_persist");
        let scan_service = make_scan_service(&library);

        let sched = scan_service
            .create_schedule(NewScanSchedule {
                storage_root_id: root_id.clone(),
                scope_relative_path: None,
                enabled: true,
                local_time: "04:30".into(),
                timezone: "UTC".into(),
            })
            .unwrap();
        assert!(sched.enabled);
        assert_eq!(sched.local_time, "04:30");

        // Reload via a new library connection (same database)
        let library2 = catalog.storage_library();
        let scan_service2 = make_scan_service(&library2);
        let reloaded = scan_service2.list_schedules().unwrap();
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].schedule_id, sched.schedule_id);
        assert_eq!(reloaded[0].local_time, "04:30");

        // Disable and verify
        scan_service2
            .update_schedule(
                &sched.schedule_id,
                ScanScheduleUpdate {
                    enabled: Some(false),
                    local_time: None,
                    timezone: None,
                },
            )
            .unwrap();
        let updated = scan_service2.get_schedule(&sched.schedule_id).unwrap();
        assert!(!updated.enabled);

        // Due schedules should not include disabled ones
        let due = scan_service2.list_due_schedules().unwrap();
        assert!(due.is_empty());
    }

    #[test]
    fn compute_next_run_returns_future_utc() {
        let result = compute_next_run_utc("12:00", "UTC").unwrap();
        assert!(!result.is_empty());
        // The returned time should be parseable
        let dt: chrono::DateTime<chrono::Utc> = chrono::DateTime::parse_from_rfc3339(&result)
            .unwrap()
            .into();
        // It should be in the future (or very recently past)
        let now = chrono::Utc::now();
        assert!(dt > now - chrono::TimeDelta::minutes(2));
    }

    #[test]
    fn safe_path_identity_is_stable() {
        let path = RootRelativePath::parse("Trips/2026/clip.mp4").unwrap();
        let id1 = safe_path_identity(&path);
        let id2 = safe_path_identity(&path);
        assert_eq!(id1, id2);
        assert_eq!(id1.len(), 32);

        let different = RootRelativePath::parse("Other/file.mp4").unwrap();
        assert_ne!(safe_path_identity(&different), id1);
    }

    #[test]
    fn scan_list_is_paginated_and_ordered() {
        let (_root, _catalog, library) = fixture();
        let root_id = create_root(&library, "root_scan_list");
        let mount = _root.path().join("mount");
        fs::create_dir_all(&mount).unwrap();
        confirm_local_binding(&library, &root_id, &mount);
        let scan_service = make_scan_service(&library);

        let s1 = scan_service
            .request_scan(ScanSource::Manual, None, &root_id, None)
            .unwrap();
        let s2 = scan_service
            .request_scan(
                ScanSource::Manual,
                None,
                &root_id,
                Some(&RootRelativePath::parse("Trips").unwrap()),
            )
            .unwrap();

        let scans = scan_service.list_scans(10, None).unwrap();
        assert!(scans.len() >= 2);
        // Most recent first
        let ids: Vec<_> = scans.iter().map(|s| s.scan_id.as_str()).collect();
        let pos1 = ids.iter().position(|&id| id == s1.scan_id).unwrap();
        let pos2 = ids.iter().position(|&id| id == s2.scan_id).unwrap();
        // s2 was created after s1, should appear before
        assert!(pos2 < pos1);
    }
}
