//! The `Catalog` handle: opening and migrating the SQLite catalog, projects, and shared bookkeeping.

mod analysis_runs;
mod assets;
mod chronology_ops;
mod labels;
mod proxies;
mod rows;
mod speech;
mod thumbnails;
mod vlm_batch;

pub(crate) use analysis_runs::*;
pub(crate) use labels::*;
pub use proxies::*;
pub(crate) use rows::*;
pub(crate) use thumbnails::*;

use crate::*;
use anyhow::{Context, Result, bail};
use batch_estimator::RateHistory;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct Catalog {
    pub(crate) database_path: PathBuf,
    pub(crate) read_only: bool,
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
        "orientation_decision_json",
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
        editplan::selection::redispatch(&mut catalog.connection()?)?;
        editplan::ranges::redispatch(&mut catalog.connection()?)?;
        editplan::feedback::redispatch(&mut catalog.connection()?)?;
        Ok(catalog)
    }

    pub fn open_existing(database_path: impl AsRef<Path>) -> Result<Self> {
        let database_path = database_path.as_ref();
        Self::validate_existing(database_path)?;
        Self::open(database_path)
    }

    /// Opens a same-machine catalog for shared writes, without migrations,
    /// recovery or redispatch. Both processes must use the exact current schema.
    pub fn open_shared_existing(database_path: impl AsRef<Path>) -> Result<Self> {
        let database_path = database_path.as_ref();
        Self::validate_existing(database_path)?;
        let connection =
            Connection::open_with_flags(database_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let inspection = migrations::inspect_project_catalog_migrations(&connection)?;
        if inspection.status != migrations::MigrationHistoryStatus::Current {
            bail!("shared catalog requires the exact current schema");
        }
        Ok(Self {
            database_path: database_path.to_path_buf(),
            read_only: false,
        })
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

    pub(crate) fn validate_existing(database_path: &Path) -> Result<()> {
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

    pub(crate) fn connection(&self) -> Result<Connection> {
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

    pub(crate) fn migrate(&self) -> Result<()> {
        let mut connection = self.connection()?;
        migrations::migrate_project_catalog(&mut connection)
    }

    pub(crate) fn redispatch_chronology_operations(&self) -> Result<()> {
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

    /// This catalog's node identity, creating it on first use.
    pub(crate) fn node_id(&self) -> Result<String> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let node_id = operations::ensure_node(&transaction)?;
        transaction.commit()?;
        Ok(node_id)
    }

    pub(crate) fn require_project(&self, project_id: &str) -> Result<()> {
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

    pub(crate) fn load_batch_history(&self, task_kind: &str) -> Result<RateHistory> {
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

    pub(crate) fn save_batch_history(&self, task_kind: &str, history: RateHistory) -> Result<()> {
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
