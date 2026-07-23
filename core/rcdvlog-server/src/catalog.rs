use anyhow::{Context, Result, anyhow};
use chrono::{SecondsFormat, Utc};
use fs2::FileExt;
use rcdvlog_core::{
    MigrationHistoryInspection, MigrationHistoryStatus, NodeId, SqliteMigration,
    inspect_sqlite_migrations, run_sqlite_migrations,
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;
use uuid::Uuid;

use crate::ConfigDiagnostics;

pub const CONTROL_CATALOG_SCHEMA_VERSION: u32 = 5;
pub const CONTROL_DATABASE_FILENAME: &str = "control.sqlite3";
pub const INSTANCE_LOCK_FILENAME: &str = "server.lock";

pub const CONTROL_CATALOG_MIGRATIONS: &[SqliteMigration] = &[
    SqliteMigration {
        id: 1,
        name: "control_catalog_foundation",
        source_identity: "server-control/0001-control-catalog-foundation/20260720",
        apply: create_control_catalog_foundation,
    },
    SqliteMigration {
        id: 2,
        name: "storage_global_library",
        source_identity: "server-control/0002-storage-global-library/20260720",
        apply: create_storage_global_library,
    },
    SqliteMigration {
        id: 3,
        name: "durable_server_job_engine",
        source_identity: "server-control/0003-durable-server-job-engine/20260720",
        apply: create_durable_server_job_engine,
    },
    SqliteMigration {
        id: 4,
        name: "nas_scan_verified_content_identity",
        source_identity: "server-control/0004-nas-scan-verified-content-identity/20260720",
        apply: create_nas_scan_verified_content_identity,
    },
    SqliteMigration {
        id: 5,
        name: "representation_store_media_jobs",
        source_identity: "server-control/0005-representation-store-media-jobs/20260723",
        apply: create_representation_store_media_jobs,
    },
];

#[derive(Debug, Error, Serialize)]
#[error("another RCD VlogCutter server process already holds the data-directory lock; {action}")]
#[serde(rename_all = "camelCase")]
pub struct InstanceLockError {
    pub code: &'static str,
    pub field: &'static str,
    pub action: &'static str,
}

impl InstanceLockError {
    fn already_running() -> Self {
        Self {
            code: "server_already_running",
            field: "dataDir",
            action: "stop the existing server or configure a different local data directory",
        }
    }
}

#[derive(Debug)]
pub struct ServerInstanceLock {
    file: File,
    path: PathBuf,
}

impl ServerInstanceLock {
    pub fn acquire(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join(INSTANCE_LOCK_FILENAME);
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("open server instance lock at {}", path.display()))?;
        match file.try_lock_exclusive() {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(InstanceLockError::already_running().into());
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("acquire server instance lock at {}", path.display())
                });
            }
        }
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        writeln!(file, "pid={}", std::process::id())?;
        file.sync_data()?;
        Ok(Self { file, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ServerInstanceLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[derive(Debug, Clone)]
pub struct ControlCatalog {
    database_path: PathBuf,
    node_id: NodeId,
}

impl ControlCatalog {
    pub fn open_or_create(data_dir: &Path, config: &ConfigDiagnostics) -> Result<Self> {
        let database_path = data_dir.join(CONTROL_DATABASE_FILENAME);
        let mut connection = open_writable_connection(&database_path)?;
        run_sqlite_migrations(&mut connection, CONTROL_CATALOG_MIGRATIONS)
            .context("migrate server control catalog")?;
        let node_id = load_or_create_node_id(&mut connection)?;
        record_configuration_revision(&mut connection, config)?;
        record_operational_timestamp(&connection, "last_started_at")?;
        Ok(Self {
            database_path,
            node_id,
        })
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    pub fn storage_library(&self) -> crate::StorageLibrary {
        crate::StorageLibrary::new(self.database_path.clone(), self.node_id.clone())
    }

    /// Durable server-only background-work repository. Jobs are scoped to this
    /// catalog's stable server node, not a distributed competitive worker pool.
    pub fn jobs(&self) -> crate::JobEngine {
        crate::JobEngine::new(self.database_path.clone(), self.node_id.clone())
    }

    pub fn representations(&self, derived_root: PathBuf) -> crate::RepresentationService {
        crate::RepresentationService::new(self.storage_library(), self.jobs(), derived_root)
    }

    pub fn inspect_migrations(&self) -> Result<MigrationHistoryInspection> {
        let connection = open_read_only_connection(&self.database_path)?;
        inspect_sqlite_migrations(&connection, CONTROL_CATALOG_MIGRATIONS)
    }

    pub fn check_usable(&self) -> Result<()> {
        let connection = open_read_only_connection(&self.database_path)?;
        let inspection = inspect_sqlite_migrations(&connection, CONTROL_CATALOG_MIGRATIONS)?;
        if inspection.status != MigrationHistoryStatus::Current {
            return Err(anyhow!(
                "control catalog migration status is {:?}",
                inspection.status
            ));
        }
        let stored_node_id = connection
            .query_row(
                "SELECT node_id FROM server_identity WHERE singleton_id = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .context("read control catalog server identity")?;
        if stored_node_id != self.node_id.as_str() {
            return Err(anyhow!(
                "control catalog server identity changed unexpectedly"
            ));
        }
        connection.query_row("SELECT 1", [], |_| Ok(()))?;
        Ok(())
    }

    pub fn checkpoint(&self) -> Result<()> {
        let connection = open_writable_connection(&self.database_path)?;
        connection
            .execute_batch("PRAGMA wal_checkpoint(PASSIVE);")
            .context("checkpoint server control catalog")?;
        Ok(())
    }
}

pub fn inspect_control_catalog_migrations(
    database_path: &Path,
) -> Result<MigrationHistoryInspection> {
    let connection = open_read_only_connection(database_path)?;
    inspect_sqlite_migrations(&connection, CONTROL_CATALOG_MIGRATIONS)
}

pub fn open_writable_connection(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)
        .with_context(|| format!("open server control catalog at {}", path.display()))?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        "#,
    )?;
    Ok(connection)
}

pub fn open_read_only_connection(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| {
        format!(
            "open server control catalog read-only at {}",
            path.display()
        )
    })?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA query_only = ON;")?;
    Ok(connection)
}

fn load_or_create_node_id(connection: &mut Connection) -> Result<NodeId> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing = transaction
        .query_row(
            "SELECT node_id FROM server_identity WHERE singleton_id = 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let node_id = match existing {
        Some(value) => NodeId::parse(value).context("validate stored server node ID")?,
        None => {
            let generated = NodeId::parse(format!("node_server_{}", Uuid::new_v4().simple()))?;
            transaction.execute(
                r#"
                INSERT INTO server_identity (singleton_id, node_id, created_at)
                VALUES (1, ?1, ?2)
                "#,
                params![
                    generated.as_str(),
                    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                ],
            )?;
            generated
        }
    };
    transaction.commit()?;
    Ok(node_id)
}

fn record_configuration_revision(
    connection: &mut Connection,
    config: &ConfigDiagnostics,
) -> Result<()> {
    let diagnostic_json = serde_json::to_string(config)?;
    let digest = blake3::hash(diagnostic_json.as_bytes())
        .to_hex()
        .to_string();
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let latest_digest = transaction
        .query_row(
            "SELECT config_digest FROM configuration_revisions ORDER BY revision DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if latest_digest.as_deref() != Some(digest.as_str()) {
        transaction.execute(
            r#"
            INSERT INTO configuration_revisions (
                revision, config_digest, redacted_diagnostics_json, recorded_at
            )
            SELECT COALESCE(MAX(revision), 0) + 1, ?1, ?2, ?3
            FROM configuration_revisions
            "#,
            params![
                digest,
                diagnostic_json,
                Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            ],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

fn record_operational_timestamp(connection: &Connection, key: &str) -> Result<()> {
    connection.execute(
        r#"
        INSERT INTO operational_metadata (key, value, updated_at)
        VALUES (?1, ?2, ?2)
        ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at
        "#,
        params![key, Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),],
    )?;
    Ok(())
}

fn create_control_catalog_foundation(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE server_identity (
            singleton_id INTEGER PRIMARY KEY CHECK(singleton_id = 1),
            node_id TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL
        );

        CREATE TABLE configuration_revisions (
            revision INTEGER PRIMARY KEY CHECK(revision > 0),
            config_digest TEXT NOT NULL,
            redacted_diagnostics_json TEXT NOT NULL,
            recorded_at TEXT NOT NULL
        );

        CREATE TABLE project_registry (
            project_id TEXT PRIMARY KEY,
            replica_relative_path TEXT NOT NULL UNIQUE,
            state TEXT NOT NULL CHECK(state IN ('importing', 'registered', 'quarantined')),
            registered_at TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE operational_metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        "#,
    )?;
    Ok(())
}

fn create_storage_global_library(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE storage_roots (
            storage_root_id TEXT PRIMARY KEY,
            display_name TEXT NOT NULL CHECK(length(display_name) > 0),
            immutable_original INTEGER NOT NULL CHECK(immutable_original IN (0, 1)),
            enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0, 1)),
            disabled_at TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE node_root_bindings (
            node_id TEXT NOT NULL,
            storage_root_id TEXT NOT NULL REFERENCES storage_roots(storage_root_id),
            local_mount_path TEXT CHECK(local_mount_path IS NULL OR length(local_mount_path) > 0),
            root_subpath TEXT,
            protocol TEXT NOT NULL CHECK(protocol IN ('nfs', 'smb', 'local', 'other')),
            access_mode TEXT NOT NULL CHECK(access_mode IN ('read_only', 'read_write')),
            verification_status TEXT NOT NULL
                CHECK(verification_status IN ('pending', 'confirmed', 'unavailable', 'mismatch', 'rejected')),
            enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0, 1)),
            association_evidence_json TEXT,
            confirmation_status TEXT NOT NULL DEFAULT 'pending'
                CHECK(confirmation_status IN ('pending', 'confirmed', 'rejected')),
            confirmed_by_node_id TEXT,
            confirmed_at TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY(node_id, storage_root_id),
            CHECK(
                (confirmation_status = 'confirmed' AND confirmed_by_node_id IS NOT NULL AND confirmed_at IS NOT NULL)
                OR (confirmation_status != 'confirmed' AND confirmed_by_node_id IS NULL AND confirmed_at IS NULL)
            )
        );

        CREATE TABLE media_objects (
            media_object_id TEXT PRIMARY KEY,
            size_bytes INTEGER CHECK(size_bytes >= 0),
            quick_fingerprint TEXT,
            full_content_hash TEXT,
            verification_state TEXT NOT NULL
                CHECK(verification_state IN ('unverified', 'provisional', 'verified', 'mismatch')),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            CHECK(verification_state != 'verified' OR (full_content_hash IS NOT NULL AND size_bytes IS NOT NULL))
        );

        CREATE UNIQUE INDEX media_objects_verified_hash_unique
            ON media_objects(full_content_hash)
            WHERE verification_state = 'verified';
        CREATE INDEX media_objects_library_order
            ON media_objects(created_at DESC, media_object_id ASC);
        CREATE INDEX media_objects_library_verification_order
            ON media_objects(verification_state, created_at DESC, media_object_id ASC);

        CREATE TABLE storage_objects (
            storage_object_id TEXT PRIMARY KEY,
            storage_root_id TEXT NOT NULL REFERENCES storage_roots(storage_root_id),
            relative_path TEXT NOT NULL,
            size_bytes INTEGER CHECK(size_bytes >= 0),
            modified_at TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(storage_root_id, relative_path),
            UNIQUE(storage_object_id, storage_root_id)
        );
        CREATE INDEX storage_objects_root_path
            ON storage_objects(storage_root_id, relative_path);

        CREATE TABLE media_object_locations (
            media_object_id TEXT NOT NULL REFERENCES media_objects(media_object_id),
            storage_object_id TEXT NOT NULL UNIQUE REFERENCES storage_objects(storage_object_id),
            verification_state TEXT NOT NULL
                CHECK(verification_state IN ('unverified', 'provisional', 'verified', 'mismatch')),
            linked_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY(media_object_id, storage_object_id)
        );
        CREATE INDEX media_object_locations_storage
            ON media_object_locations(storage_object_id, media_object_id);

        -- Rebuildable index of project-replica facts. Project annotations remain
        -- exclusively in project catalogs and are deliberately absent here.
        CREATE TABLE project_media_memberships (
            project_id TEXT NOT NULL REFERENCES project_registry(project_id),
            project_asset_id TEXT NOT NULL,
            media_object_id TEXT NOT NULL REFERENCES media_objects(media_object_id),
            source_operation_id TEXT NOT NULL,
            projected_at TEXT NOT NULL,
            PRIMARY KEY(project_id, project_asset_id),
            UNIQUE(project_id, source_operation_id)
        );
        CREATE INDEX project_media_memberships_media
            ON project_media_memberships(media_object_id, project_id, project_asset_id);

        CREATE TABLE node_observation_sequences (
            node_id TEXT PRIMARY KEY,
            last_sequence INTEGER NOT NULL CHECK(last_sequence >= 0)
        );

        CREATE TABLE observation_generations (
            generation_id TEXT PRIMARY KEY,
            node_id TEXT NOT NULL,
            generation_sequence INTEGER NOT NULL CHECK(generation_sequence > 0),
            storage_root_id TEXT NOT NULL REFERENCES storage_roots(storage_root_id),
            scope_relative_path TEXT,
            state TEXT NOT NULL CHECK(state IN ('in_progress', 'completed', 'abandoned')),
            started_at TEXT NOT NULL,
            completed_at TEXT,
            UNIQUE(node_id, generation_sequence),
            UNIQUE(generation_id, node_id, storage_root_id),
            CHECK(
                (state = 'completed' AND completed_at IS NOT NULL)
                OR (state != 'completed' AND completed_at IS NULL)
            )
        );
        CREATE INDEX observation_generations_scope_latest
            ON observation_generations(
                node_id, storage_root_id, state, generation_sequence DESC, scope_relative_path
            );

        CREATE TABLE node_asset_observations (
            generation_id TEXT NOT NULL,
            node_id TEXT NOT NULL,
            storage_root_id TEXT NOT NULL,
            storage_object_id TEXT NOT NULL,
            status TEXT NOT NULL
                CHECK(status IN ('accessible', 'temporarily_unavailable', 'fingerprint_mismatch')),
            observed_size_bytes INTEGER CHECK(observed_size_bytes >= 0),
            observed_quick_fingerprint TEXT,
            observed_at TEXT NOT NULL,
            PRIMARY KEY(generation_id, storage_object_id),
            FOREIGN KEY(generation_id, node_id, storage_root_id)
                REFERENCES observation_generations(generation_id, node_id, storage_root_id),
            FOREIGN KEY(storage_object_id, storage_root_id)
                REFERENCES storage_objects(storage_object_id, storage_root_id)
        );
        CREATE INDEX node_asset_observations_object
            ON node_asset_observations(node_id, storage_object_id, generation_id);
        "#,
    )?;
    Ok(())
}

/// Append-only migration; Task 1/2 migrations and their identities remain
/// immutable. JobEngine validates the bounded JSON contracts before writing.
fn create_durable_server_job_engine(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(r#"
        CREATE TABLE job_enqueue_sequences (singleton_id INTEGER PRIMARY KEY CHECK(singleton_id = 1), last_sequence INTEGER NOT NULL CHECK(last_sequence >= 0));
        INSERT INTO job_enqueue_sequences(singleton_id, last_sequence) VALUES (1, 0);
        CREATE TABLE jobs (
            job_id TEXT PRIMARY KEY, job_type TEXT NOT NULL, payload_version INTEGER NOT NULL CHECK(payload_version > 0),
            payload_json TEXT NOT NULL CHECK(length(payload_json) <= 65536), immutable_identity_json TEXT NOT NULL CHECK(length(immutable_identity_json) <= 16384),
            requirements_json TEXT NOT NULL CHECK(length(requirements_json) <= 16384),
            target_node_id TEXT NOT NULL, recipe_key TEXT NOT NULL, job_class TEXT NOT NULL CHECK(job_class IN ('proxy','full_hash','lightweight')),
            project_id TEXT, media_object_id TEXT, storage_root_id TEXT, representation_id TEXT,
            idempotency_key TEXT NOT NULL CHECK(length(idempotency_key) BETWEEN 1 AND 255), priority INTEGER NOT NULL DEFAULT 0,
            idempotency_identity_hash TEXT NOT NULL CHECK(length(idempotency_identity_hash) = 64),
            enqueue_sequence INTEGER NOT NULL UNIQUE CHECK(enqueue_sequence > 0), state TEXT NOT NULL CHECK(state IN ('queued','running','retry_wait','succeeded','failed','canceled')),
            attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0), max_attempts INTEGER NOT NULL CHECK(max_attempts BETWEEN 1 AND 100), retry_initial_seconds INTEGER NOT NULL CHECK(retry_initial_seconds BETWEEN 1 AND 86400), retry_max_seconds INTEGER NOT NULL CHECK(retry_max_seconds BETWEEN 1 AND 86400 AND retry_initial_seconds <= retry_max_seconds),
            available_at TEXT NOT NULL, cancel_requested_at TEXT, cancel_reason TEXT CHECK(cancel_reason IS NULL OR length(cancel_reason) <= 1024),
            lease_owner TEXT, lease_token TEXT, lease_expires_at TEXT, heartbeat_at TEXT,
            completed_work_units INTEGER NOT NULL DEFAULT 0 CHECK(completed_work_units >= 0), total_work_units INTEGER NOT NULL CHECK(total_work_units >= 0),
            total_items INTEGER NOT NULL CHECK(total_items >= 0), completed_items INTEGER NOT NULL DEFAULT 0 CHECK(completed_items >= 0), failed_items INTEGER NOT NULL DEFAULT 0 CHECK(failed_items >= 0),
            error_code TEXT CHECK(error_code IS NULL OR length(error_code) <= 128), error_details_json TEXT CHECK(error_details_json IS NULL OR length(error_details_json) <= 8192), output_identity TEXT CHECK(output_identity IS NULL OR length(output_identity) <= 1024),
            created_at TEXT NOT NULL, started_at TEXT, updated_at TEXT NOT NULL, completed_at TEXT,
            CHECK(completed_work_units <= total_work_units), CHECK(failed_items <= completed_items AND completed_items <= total_items),
            CHECK((state = 'running') = (lease_owner IS NOT NULL AND lease_token IS NOT NULL AND lease_expires_at IS NOT NULL)),
            CHECK((state IN ('succeeded','failed','canceled')) = (completed_at IS NOT NULL)),
            UNIQUE(target_node_id, job_type, idempotency_key)
        );
        CREATE INDEX jobs_claimable_by_node ON jobs(target_node_id, state, available_at, priority DESC, enqueue_sequence ASC);
        CREATE INDEX jobs_claimable_by_node_class ON jobs(target_node_id, job_class, state, available_at, priority DESC, enqueue_sequence ASC);
        CREATE INDEX jobs_recipe_state ON jobs(target_node_id, recipe_key, state);
        CREATE TABLE job_attempts (
            job_id TEXT NOT NULL REFERENCES jobs(job_id) ON DELETE CASCADE, attempt_number INTEGER NOT NULL CHECK(attempt_number > 0),
            lease_owner TEXT NOT NULL, lease_token TEXT NOT NULL UNIQUE, state TEXT NOT NULL CHECK(state IN ('running','succeeded','failed','canceled','expired')),
            started_at TEXT NOT NULL, finished_at TEXT, error_code TEXT CHECK(error_code IS NULL OR length(error_code) <= 128), error_details_json TEXT CHECK(error_details_json IS NULL OR length(error_details_json) <= 8192),
            PRIMARY KEY(job_id, attempt_number),
            CHECK((state = 'running') = (finished_at IS NULL))
        );
        CREATE TABLE job_dependencies (job_id TEXT NOT NULL REFERENCES jobs(job_id) ON DELETE CASCADE, depends_on_job_id TEXT NOT NULL REFERENCES jobs(job_id), PRIMARY KEY(job_id, depends_on_job_id), CHECK(job_id <> depends_on_job_id));
        CREATE INDEX job_dependencies_dependency ON job_dependencies(depends_on_job_id, job_id);
        CREATE TABLE job_rate_history (node_id TEXT NOT NULL, recipe_key TEXT NOT NULL, work_units_per_worker_second REAL NOT NULL CHECK(work_units_per_worker_second >= 0), samples INTEGER NOT NULL CHECK(samples >= 0), updated_at TEXT NOT NULL, PRIMARY KEY(node_id, recipe_key));
        CREATE TABLE job_events (event_sequence INTEGER PRIMARY KEY AUTOINCREMENT, job_id TEXT NOT NULL REFERENCES jobs(job_id) ON DELETE CASCADE, attempt_number INTEGER, event_type TEXT NOT NULL CHECK(length(event_type) BETWEEN 1 AND 128), state TEXT NOT NULL, details_json TEXT NOT NULL CHECK(length(details_json) <= 8192), created_at TEXT NOT NULL);
        CREATE INDEX job_events_replay ON job_events(event_sequence, job_id);
        CREATE INDEX job_events_job ON job_events(job_id, event_sequence);
    "#)?;
    Ok(())
}

fn create_nas_scan_verified_content_identity(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE scan_runs (
            scan_id TEXT PRIMARY KEY,
            node_id TEXT NOT NULL,
            storage_root_id TEXT NOT NULL REFERENCES storage_roots(storage_root_id),
            scope_relative_path TEXT,
            generation_id TEXT NOT NULL,
            root_job_id TEXT NOT NULL,
            scheduled_occurrence_at TEXT,
            source TEXT NOT NULL CHECK(source IN ('manual', 'schedule')),
            schedule_id TEXT,
            state TEXT NOT NULL CHECK(state IN ('queued', 'running', 'succeeded', 'failed', 'canceled')),
            visited_entries INTEGER NOT NULL DEFAULT 0 CHECK(visited_entries >= 0),
            supported_files INTEGER NOT NULL DEFAULT 0 CHECK(supported_files >= 0),
            unchanged_files INTEGER NOT NULL DEFAULT 0 CHECK(unchanged_files >= 0),
            queued_files INTEGER NOT NULL DEFAULT 0 CHECK(queued_files >= 0),
            completed_files INTEGER NOT NULL DEFAULT 0 CHECK(completed_files >= 0),
            failed_files INTEGER NOT NULL DEFAULT 0 CHECK(failed_files >= 0),
            error_code TEXT CHECK(error_code IS NULL OR length(error_code) <= 128),
            error_details_json TEXT CHECK(error_details_json IS NULL OR length(error_details_json) <= 8192),
            created_at TEXT NOT NULL,
            started_at TEXT,
            completed_at TEXT,
            CHECK(
                (state IN ('succeeded', 'failed', 'canceled') AND completed_at IS NOT NULL)
                OR (state IN ('queued', 'running') AND completed_at IS NULL)
            ),
            CHECK(failed_files <= completed_files AND completed_files <= queued_files),
            CHECK(supported_files <= visited_entries)
        );
        CREATE UNIQUE INDEX scan_runs_active_scope
            ON scan_runs(node_id, storage_root_id, scope_relative_path)
            WHERE state IN ('queued', 'running');
        CREATE INDEX scan_runs_state ON scan_runs(node_id, state, created_at DESC);

        -- Durable child-job tracking so the finalizer can check settlement
        -- without storing thousands of dependency IDs on one job definition.
        CREATE TABLE scan_run_jobs (
            scan_id TEXT NOT NULL REFERENCES scan_runs(scan_id),
            job_id TEXT NOT NULL REFERENCES jobs(job_id),
            storage_object_id TEXT REFERENCES storage_objects(storage_object_id),
            stage TEXT NOT NULL CHECK(stage IN ('root','quick_fingerprint','probe','full_hash','finalizer')),
            PRIMARY KEY(scan_id, job_id)
        );
        CREATE INDEX scan_run_jobs_storage
            ON scan_run_jobs(scan_id, stage, storage_object_id);
        CREATE INDEX scan_run_jobs_job ON scan_run_jobs(job_id);

        CREATE TABLE scan_schedules (
            schedule_id TEXT PRIMARY KEY,
            storage_root_id TEXT NOT NULL REFERENCES storage_roots(storage_root_id),
            scope_relative_path TEXT,
            enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0, 1)),
            local_time TEXT NOT NULL,
            timezone TEXT NOT NULL CHECK(length(timezone) BETWEEN 1 AND 64),
            next_run_at TEXT NOT NULL,
            last_enqueued_at TEXT,
            last_scan_id TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX scan_schedules_enabled_scope
            ON scan_schedules(storage_root_id, scope_relative_path, local_time, timezone)
            WHERE enabled = 1;
        CREATE INDEX scan_schedules_due ON scan_schedules(enabled, next_run_at);

        CREATE TABLE media_quick_fingerprint_facts (
            storage_object_id TEXT NOT NULL REFERENCES storage_objects(storage_object_id),
            recipe_key TEXT NOT NULL CHECK(length(recipe_key) BETWEEN 1 AND 255),
            recipe_version TEXT NOT NULL CHECK(length(recipe_version) BETWEEN 1 AND 64),
            size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0),
            quick_fingerprint TEXT NOT NULL,
            captured_at TEXT NOT NULL,
            PRIMARY KEY(storage_object_id, recipe_key, recipe_version)
        );
        CREATE INDEX media_quick_fingerprint_lookup
            ON media_quick_fingerprint_facts(quick_fingerprint, size_bytes);

        CREATE TABLE media_full_hash_facts (
            storage_object_id TEXT NOT NULL REFERENCES storage_objects(storage_object_id),
            recipe_key TEXT NOT NULL CHECK(length(recipe_key) BETWEEN 1 AND 255),
            recipe_version TEXT NOT NULL CHECK(length(recipe_version) BETWEEN 1 AND 64),
            full_content_hash TEXT NOT NULL,
            byte_count INTEGER NOT NULL CHECK(byte_count >= 0),
            pre_read_size_bytes INTEGER NOT NULL CHECK(pre_read_size_bytes >= 0),
            pre_read_modified_ns INTEGER,
            post_read_size_bytes INTEGER NOT NULL CHECK(post_read_size_bytes >= 0),
            post_read_modified_ns INTEGER,
            captured_at TEXT NOT NULL,
            PRIMARY KEY(storage_object_id, recipe_key, recipe_version)
        );
        CREATE INDEX media_full_hash_verified_lookup
            ON media_full_hash_facts(full_content_hash, byte_count);

        CREATE TABLE media_probe_facts (
            storage_object_id TEXT NOT NULL REFERENCES storage_objects(storage_object_id),
            recipe_key TEXT NOT NULL CHECK(length(recipe_key) BETWEEN 1 AND 255),
            recipe_version TEXT NOT NULL CHECK(length(recipe_version) BETWEEN 1 AND 64),
            probe_json TEXT NOT NULL CHECK(length(probe_json) <= 32768),
            camera_make TEXT,
            camera_model TEXT,
            enrichment_status TEXT CHECK(enrichment_status IN ('ok', 'warning', 'unavailable')),
            enrichment_warning TEXT CHECK(length(enrichment_warning) <= 1024),
            captured_at TEXT NOT NULL,
            PRIMARY KEY(storage_object_id, recipe_key, recipe_version)
        );

        CREATE TABLE scan_file_diagnostics (
            diagnostic_id TEXT PRIMARY KEY,
            scan_id TEXT NOT NULL REFERENCES scan_runs(scan_id),
            storage_object_id TEXT REFERENCES storage_objects(storage_object_id),
            stage TEXT NOT NULL CHECK(length(stage) BETWEEN 1 AND 64),
            code TEXT NOT NULL CHECK(length(code) BETWEEN 1 AND 128),
            safe_path_identity TEXT NOT NULL CHECK(length(safe_path_identity) BETWEEN 1 AND 256),
            retryable INTEGER NOT NULL CHECK(retryable IN (0, 1)),
            recorded_at TEXT NOT NULL
        );
        CREATE INDEX scan_file_diagnostics_scan
            ON scan_file_diagnostics(scan_id, recorded_at);
        CREATE INDEX scan_file_diagnostics_object
            ON scan_file_diagnostics(storage_object_id, recorded_at);

        CREATE TABLE media_mismatch_evidence (
            mismatch_id TEXT PRIMARY KEY,
            media_object_id TEXT REFERENCES media_objects(media_object_id),
            storage_object_id TEXT REFERENCES storage_objects(storage_object_id),
            mismatch_type TEXT NOT NULL CHECK(mismatch_type IN ('quick_fingerprint_collision')),
            related_media_object_id TEXT REFERENCES media_objects(media_object_id),
            related_storage_object_id TEXT REFERENCES storage_objects(storage_object_id),
            recorded_at TEXT NOT NULL
        );
        CREATE INDEX media_mismatch_evidence_media
            ON media_mismatch_evidence(media_object_id, recorded_at);
        "#,
    )?;
    Ok(())
}

/// Content-addressed derived-media manifests. Absolute paths remain node-local;
/// the durable protocol fact is the representation identity plus a relative
/// location beneath a configured derived root.
fn create_representation_store_media_jobs(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE representations (
            representation_id TEXT PRIMARY KEY,
            media_object_id TEXT NOT NULL REFERENCES media_objects(media_object_id),
            source_content_hash TEXT NOT NULL CHECK(length(source_content_hash) = 64),
            source_size_bytes INTEGER NOT NULL CHECK(source_size_bytes >= 0),
            kind TEXT NOT NULL CHECK(kind IN ('proxy', 'thumbnail')),
            recipe_key TEXT NOT NULL CHECK(length(recipe_key) BETWEEN 1 AND 255),
            settings_identity TEXT NOT NULL CHECK(length(settings_identity) = 64),
            state TEXT NOT NULL CHECK(state IN ('pending', 'available')),
            output_content_hash TEXT CHECK(output_content_hash IS NULL OR length(output_content_hash) = 64),
            byte_length INTEGER CHECK(byte_length IS NULL OR byte_length >= 0),
            content_type TEXT NOT NULL CHECK(length(content_type) BETWEEN 1 AND 128),
            created_at TEXT NOT NULL,
            completed_at TEXT,
            updated_at TEXT NOT NULL,
            UNIQUE(source_content_hash, source_size_bytes, kind, recipe_key, settings_identity),
            CHECK(
                (state = 'available' AND output_content_hash IS NOT NULL AND byte_length IS NOT NULL AND completed_at IS NOT NULL)
                OR (state = 'pending' AND output_content_hash IS NULL AND byte_length IS NULL AND completed_at IS NULL)
            )
        );
        CREATE INDEX representations_media_recipe
            ON representations(media_object_id, kind, recipe_key, state);

        CREATE TABLE representation_locations (
            representation_id TEXT NOT NULL REFERENCES representations(representation_id),
            node_id TEXT NOT NULL,
            location_kind TEXT NOT NULL CHECK(location_kind IN ('shared_derived', 'server_local', 'project_local')),
            location_key TEXT NOT NULL CHECK(length(location_key) BETWEEN 1 AND 2048),
            relative_path TEXT CHECK(relative_path IS NULL OR length(relative_path) BETWEEN 1 AND 2048),
            local_path TEXT CHECK(local_path IS NULL OR length(local_path) BETWEEN 1 AND 8192),
            state TEXT NOT NULL CHECK(state IN ('available', 'missing', 'invalid')),
            verified_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY(representation_id, node_id, location_kind, location_key),
            CHECK(
                (location_kind = 'shared_derived' AND relative_path IS NOT NULL AND local_path IS NULL)
                OR (location_kind != 'shared_derived' AND relative_path IS NULL AND local_path IS NOT NULL)
            )
        );
        CREATE INDEX representation_locations_node_state
            ON representation_locations(node_id, state, representation_id);
        CREATE UNIQUE INDEX jobs_one_active_representation
            ON jobs(representation_id)
            WHERE representation_id IS NOT NULL AND state IN ('queued', 'running', 'retry_wait');
        "#,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ServerConfig;
    use rusqlite::Connection;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn config(root: &TempDir) -> ServerConfig {
        let data = root.path().join("state");
        let derived = root.path().join("derived");
        let backups = root.path().join("backups");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&derived).unwrap();
        std::fs::create_dir_all(&backups).unwrap();
        ServerConfig::from_values(BTreeMap::from([
            ("RCDVLOG_DATA_DIR".to_string(), data.display().to_string()),
            (
                "RCDVLOG_DERIVED_DIR".to_string(),
                derived.display().to_string(),
            ),
            (
                "RCDVLOG_BACKUP_DIR".to_string(),
                backups.display().to_string(),
            ),
        ]))
        .unwrap()
    }

    #[test]
    fn first_start_migrates_and_restart_preserves_node_identity() {
        let root = TempDir::new().unwrap();
        let config = config(&root);
        let first =
            ControlCatalog::open_or_create(&config.data_dir, &config.diagnostics()).unwrap();
        let first_node_id = first.node_id().clone();
        drop(first);

        let second =
            ControlCatalog::open_or_create(&config.data_dir, &config.diagnostics()).unwrap();
        assert_eq!(second.node_id(), &first_node_id);
        assert_eq!(
            second.inspect_migrations().unwrap().status,
            MigrationHistoryStatus::Current
        );

        let connection = Connection::open(second.database_path()).unwrap();
        let migration_count: u32 = connection
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        let revision_count: u32 = connection
            .query_row("SELECT COUNT(*) FROM configuration_revisions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(migration_count, CONTROL_CATALOG_SCHEMA_VERSION);
        assert_eq!(revision_count, 1);
    }

    #[test]
    fn only_one_instance_lock_can_be_held_for_a_data_directory() {
        let root = TempDir::new().unwrap();
        let first = ServerInstanceLock::acquire(root.path()).unwrap();
        let error = ServerInstanceLock::acquire(root.path()).unwrap_err();
        assert_eq!(
            error.downcast_ref::<InstanceLockError>().unwrap().code,
            "server_already_running"
        );
        drop(first);
        ServerInstanceLock::acquire(root.path()).unwrap();
    }

    #[test]
    fn control_catalog_rejects_newer_migration_history_without_changing_it() {
        let root = TempDir::new().unwrap();
        let config = config(&root);
        let catalog =
            ControlCatalog::open_or_create(&config.data_dir, &config.diagnostics()).unwrap();
        let connection = Connection::open(catalog.database_path()).unwrap();
        connection
            .execute(
                r#"
                INSERT INTO schema_migrations (
                    migration_id, name, source_identity, applied_at
                ) VALUES (?1, 'future', 'server-control/future', '2026-07-20T00:00:00Z')
                "#,
                [CONTROL_CATALOG_SCHEMA_VERSION + 1],
            )
            .unwrap();
        drop(connection);

        let inspection = inspect_control_catalog_migrations(catalog.database_path()).unwrap();
        assert_eq!(inspection.status, MigrationHistoryStatus::NewerUnsupported);
        let error =
            ControlCatalog::open_or_create(&config.data_dir, &config.diagnostics()).unwrap_err();
        assert!(format!("{error:#}").contains("newer than supported"));
    }

    #[test]
    fn scan_migration_upgrade_preserves_existing_rows() {
        let root = TempDir::new().unwrap();
        let database_path = root.path().join(CONTROL_DATABASE_FILENAME);
        let mut connection = open_writable_connection(&database_path).unwrap();
        // Run migrations 1-3
        run_sqlite_migrations(&mut connection, &CONTROL_CATALOG_MIGRATIONS[..3]).unwrap();
        // Insert existing data
        connection
            .execute(
                "INSERT INTO server_identity(singleton_id, node_id, created_at) VALUES (1, ?1, ?2)",
                params!["node_server_preserved", "2026-07-20T00:00:00Z"],
            )
            .unwrap();
        connection.execute(
            "INSERT INTO project_registry(project_id, replica_relative_path, state, registered_at, created_at, updated_at) VALUES (?1, ?2, 'registered', ?3, ?3, ?3)",
            params!["project_preserved", "projects/project_preserved/catalog.sqlite3", "2026-07-20T00:00:00Z"],
        ).unwrap();
        connection.execute(
            "INSERT INTO storage_roots(storage_root_id, display_name, immutable_original, created_at, updated_at) VALUES ('root_1', 'Root 1', 1, '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z')",
            [],
        ).unwrap();
        // Enqueue a job using params
        let reqs = r#"{"schema_version":1,"requires_original":false,"requires_derived_write":false,"capabilities":[]}"#;
        connection.execute(
            "INSERT INTO jobs(job_id, job_type, payload_version, payload_json, immutable_identity_json, requirements_json, target_node_id, recipe_key, job_class, idempotency_key, idempotency_identity_hash, enqueue_sequence, state, max_attempts, retry_initial_seconds, retry_max_seconds, available_at, total_work_units, total_items, created_at, updated_at) VALUES (?1, 'test.v1', 1, '{}', '{}', ?2, 'node_server_preserved', 'test.v1', 'lightweight', 'key_1', ?3, 1, 'queued', 3, 5, 300, '2026-07-20T00:00:00Z', 10, 1, '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z')",
            params!["job_keep", reqs, "a".repeat(64)],
        ).unwrap();

        // Run migration 4
        run_sqlite_migrations(&mut connection, CONTROL_CATALOG_MIGRATIONS).unwrap();

        // Verify existing rows survive
        let node: String = connection
            .query_row(
                "SELECT node_id FROM server_identity WHERE singleton_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(node, "node_server_preserved");
        let project: String = connection
            .query_row("SELECT project_id FROM project_registry", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(project, "project_preserved");
        let root: String = connection
            .query_row("SELECT storage_root_id FROM storage_roots", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(root, "root_1");
        let job_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE job_id='job_keep'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(job_count, 1);

        // New tables exist
        connection
            .query_row("SELECT COUNT(*) FROM scan_runs", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        connection
            .query_row("SELECT COUNT(*) FROM scan_schedules", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        connection
            .query_row(
                "SELECT COUNT(*) FROM media_quick_fingerprint_facts",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        connection
            .query_row("SELECT COUNT(*) FROM media_full_hash_facts", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        connection
            .query_row("SELECT COUNT(*) FROM media_probe_facts", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        connection
            .query_row("SELECT COUNT(*) FROM scan_file_diagnostics", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        connection
            .query_row("SELECT COUNT(*) FROM media_mismatch_evidence", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();

        assert_eq!(
            inspect_sqlite_migrations(&connection, CONTROL_CATALOG_MIGRATIONS)
                .unwrap()
                .status,
            MigrationHistoryStatus::Current
        );
    }

    #[test]
    fn storage_library_upgrade_preserves_server_identity_and_project_registry() {
        let root = TempDir::new().unwrap();
        let database_path = root.path().join(CONTROL_DATABASE_FILENAME);
        let mut connection = open_writable_connection(&database_path).unwrap();
        run_sqlite_migrations(&mut connection, &CONTROL_CATALOG_MIGRATIONS[..1]).unwrap();
        connection
            .execute(
                "INSERT INTO server_identity(singleton_id, node_id, created_at) VALUES (1, ?1, ?2)",
                params!["node_server_preserved", "2026-07-20T00:00:00Z"],
            )
            .unwrap();
        connection
            .execute(
                r#"
                INSERT INTO project_registry(
                    project_id, replica_relative_path, state, registered_at, created_at, updated_at
                ) VALUES (?1, ?2, 'registered', ?3, ?3, ?3)
                "#,
                params![
                    "project_preserved",
                    "projects/project_preserved/catalog.sqlite3",
                    "2026-07-20T00:00:00Z"
                ],
            )
            .unwrap();

        run_sqlite_migrations(&mut connection, CONTROL_CATALOG_MIGRATIONS).unwrap();

        let node: String = connection
            .query_row(
                "SELECT node_id FROM server_identity WHERE singleton_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let project: (String, String, String) = connection
            .query_row(
                "SELECT project_id, replica_relative_path, state FROM project_registry",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(node, "node_server_preserved");
        assert_eq!(
            project,
            (
                "project_preserved".to_string(),
                "projects/project_preserved/catalog.sqlite3".to_string(),
                "registered".to_string()
            )
        );
        assert_eq!(
            inspect_sqlite_migrations(&connection, CONTROL_CATALOG_MIGRATIONS)
                .unwrap()
                .status,
            MigrationHistoryStatus::Current
        );
    }

    #[test]
    fn representation_migration_upgrade_preserves_verified_media_and_jobs() {
        let root = TempDir::new().unwrap();
        let database_path = root.path().join(CONTROL_DATABASE_FILENAME);
        let mut connection = open_writable_connection(&database_path).unwrap();
        run_sqlite_migrations(&mut connection, &CONTROL_CATALOG_MIGRATIONS[..4]).unwrap();
        connection.execute(
            "INSERT INTO media_objects(media_object_id,size_bytes,full_content_hash,verification_state,created_at,updated_at) VALUES('media_preserved',10,?1,'verified',?2,?2)",
            params!["a".repeat(64), "2026-07-23T00:00:00Z"],
        ).unwrap();
        connection
            .execute(
                r#"
            INSERT INTO jobs(
                job_id,job_type,payload_version,payload_json,immutable_identity_json,
                requirements_json,target_node_id,recipe_key,job_class,idempotency_key,
                priority,idempotency_identity_hash,enqueue_sequence,state,attempt_count,
                max_attempts,retry_initial_seconds,retry_max_seconds,available_at,
                total_work_units,total_items,created_at,updated_at
            ) VALUES(
                'job_preserved','fixture.v1',1,'{}','{}',?1,'node_preserved',
                'fixture.v1','lightweight','preserved',0,?2,1,'queued',0,3,1,5,
                ?3,1,1,?3,?3
            )
            "#,
                params![
                    serde_json::json!({
                        "schemaVersion": 1,
                        "requiresOriginal": false,
                        "requiresDerivedWrite": false,
                        "capabilities": []
                    })
                    .to_string(),
                    "b".repeat(64),
                    "2026-07-23T00:00:00Z"
                ],
            )
            .unwrap();

        run_sqlite_migrations(&mut connection, CONTROL_CATALOG_MIGRATIONS).unwrap();

        assert_eq!(connection.query_row(
            "SELECT verification_state FROM media_objects WHERE media_object_id='media_preserved'",
            [], |row| row.get::<_, String>(0),
        ).unwrap(), "verified");
        assert_eq!(
            connection
                .query_row(
                    "SELECT state FROM jobs WHERE job_id='job_preserved'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "queued"
        );
        assert_eq!(
            inspect_sqlite_migrations(&connection, CONTROL_CATALOG_MIGRATIONS)
                .unwrap()
                .status,
            MigrationHistoryStatus::Current
        );
    }
}
