use anyhow::{Context, Result, anyhow, bail};
use batch_estimator::{BatchEstimate, BatchEstimator, RateHistory};
use blake3::Hasher;
use chrono::{DateTime, Duration, NaiveDateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;
use uuid::Uuid;
use walkdir::WalkDir;

mod baseline;
mod identifiers;
mod migrations;
mod operations;
mod storage;

pub use operations::{
    ApplyOperationResult, AssetRevisionDiagnostics, CLOCK_EVENT_CREATED, ConflictProjection,
    HybridLogicalTime, LABEL_ADDED, LABEL_REMOVED, NOTE_REVISION_CREATED, NoteRevisionProjection,
    OperationBatchResult, OperationDiagnostics, PROJECT_ASSET_ADDED, PROJECT_ASSET_MEDIA_LINKED,
    ProjectAssetAddedPayload, ProjectAssetItem, ProjectAssetPage, ProjectOperation, REMOVED_SET,
    ROTATION_SET,
};

pub use baseline::{PortableAssetBaseline, PortableRootMapping, ProjectBaselineSnapshot};
pub use identifiers::{
    IdentifierError, IdentifierErrorReason, JobId, MediaObjectId, NodeId, OperationId,
    ProjectAssetId, ProjectId, RepresentationId, StorageObjectId, StorageRootId,
};
pub use migrations::{
    CatalogSchemaError, MigrationHistoryInspection, MigrationHistoryStatus,
    PROJECT_CATALOG_SCHEMA_VERSION, PendingMigration, SchemaMigrationRecord, SqliteMigration,
    inspect_project_catalog_migrations, inspect_sqlite_migrations, run_sqlite_migrations,
};
pub use storage::{
    BindingVerificationStatus, GlobalMediaVerificationState, LocationVerificationState,
    NodeAvailabilityState, ObservationGenerationState, ObservationStatus, RelativePathError,
    RelativePathErrorReason, RootAssociationConfirmation, RootAssociationConfirmationStatus,
    RootAssociationEvidence, RootRelativePath, StorageAccessMode, StorageProtocol,
};

const SAMPLE_BYTES: usize = 1024 * 1024;
const MEDIA_EXTENSIONS: &[&str] = &["mp4", "mov", "m4v", "mts", "m2ts", "avi"];
const PROXY_TASK_KIND: &str = "proxy.h264-720p.orientation-v3";
const THUMBNAIL_TASK_KIND: &str = "thumbnail.q1.orientation-v2";
const PROXY_CACHE_VERSION: &str = "v3";
const THUMBNAIL_CACHE_VERSION: &str = "v2";
const PROXY_WORKERS: usize = 1;
const THUMBNAIL_WORKERS: usize = 1;

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
    pub labels: Vec<String>,
    pub removed: bool,
    pub media_object_id: Option<String>,
    pub media_verification_state: MediaVerificationState,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetPatch {
    pub notes: String,
    pub labels: Vec<String>,
    pub removed: bool,
    pub rotation_degrees: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestReport {
    pub scanned: usize,
    pub imported: usize,
    pub duplicates: usize,
    pub failures: Vec<IngestFailure>,
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
    pub failures: Vec<IngestFailure>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolStatus {
    pub name: String,
    pub path: String,
    pub available: bool,
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
    let stream = probe["streams"].as_array().and_then(|items| items.first());
    let width = stream
        .and_then(|value| value["width"].as_u64())
        .unwrap_or(0) as u32;
    let height = stream
        .and_then(|value| value["height"].as_u64())
        .unwrap_or(0) as u32;
    let metadata_rotation_degrees = stream_rotation(stream);
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
        codec: stream
            .and_then(|value| value["codec_name"].as_str())
            .unwrap_or("unknown")
            .to_string(),
        metadata_rotation_degrees,
        display_width,
        display_height,
        display_orientation,
    })
}

pub fn prerequisite_status() -> Vec<ToolStatus> {
    ["ffmpeg", "ffprobe", "exiftool"]
        .into_iter()
        .map(|name| {
            let path = resolve_tool(name);
            ToolStatus {
                name: name.to_string(),
                path: path.to_string_lossy().into_owned(),
                available: path.is_file(),
            }
        })
        .collect()
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
            LEFT JOIN assets a ON a.project_id = p.id
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
            failures: Vec::new(),
        };

        let total = candidates.len();
        for (index, path) in candidates.into_iter().enumerate() {
            match self.ingest_file(project_id, &path) {
                Ok(true) => report.imported += 1,
                Ok(false) => report.duplicates += 1,
                Err(error) => report.failures.push(IngestFailure {
                    path: path.to_string_lossy().into_owned(),
                    error: format!("{error:#}"),
                }),
            }
            on_progress(index + 1, total, &path);
        }
        Ok(report)
    }

    fn ingest_file(&self, project_id: &str, path: &Path) -> Result<bool> {
        let metadata =
            fs::metadata(path).with_context(|| format!("read metadata for {}", path.display()))?;
        let fingerprint = sampled_fingerprint(path, metadata.len())?;
        let connection = self.connection()?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM assets WHERE project_id = ?1 AND fingerprint = ?2)",
            params![project_id, fingerprint],
            |row| row.get(0),
        )?;
        if exists {
            return Ok(false);
        }

        let probe = probe_media(path)?;
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
                id,
                fingerprint,
                filename: filename.to_string(),
                size_bytes: metadata.len(),
                duration_seconds: probe.duration_seconds,
                frame_rate: probe.frame_rate,
                width: probe.width,
                height: probe.height,
                codec: probe.codec,
                camera: probe.camera,
                captured_at_raw: probe.captured_at_raw,
                best_time: probe.best_time,
                metadata_rotation_degrees: probe.metadata_rotation_degrees,
                default_rotation_degrees: probe.default_rotation_degrees,
                created_at: now(),
            },
        )?;
        Ok(true)
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
        let sql = format!(
            r#"
            SELECT a.id, a.project_id, a.path, a.filename, a.fingerprint, a.size_bytes,
                   a.duration_seconds, a.frame_rate, a.width, a.height, a.codec, a.camera,
                   a.captured_at_raw, a.best_time, a.offset_seconds, a.proxy_path,
                   {metadata_rotation}, {default_rotation}, {rotation},
                   {orientation_probed}, {thumbnail_path}, {thumbnail_quality},
                   a.notes, {labels}, {removed}, {media_object_id},
                   {media_verification_state}
            FROM assets a
            {thumbnail_join}
            WHERE a.project_id = ?1
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
        let sql = format!(
            r#"
            SELECT a.id, a.project_id, a.path, a.filename, a.fingerprint, a.size_bytes,
                   a.duration_seconds, a.frame_rate, a.width, a.height, a.codec, a.camera,
                   a.captured_at_raw, a.best_time, a.offset_seconds, a.proxy_path,
                   {metadata_rotation}, {default_rotation}, {rotation},
                   {orientation_probed}, {thumbnail_path}, {thumbnail_quality},
                   a.notes, {labels}, {removed}, {media_object_id},
                   {media_verification_state}
            FROM assets a
            {thumbnail_join}
            WHERE a.id = ?1
            "#
        );
        connection
            .query_row(&sql, [asset_id], row_to_asset)
            .optional()?
            .ok_or_else(|| anyhow!("asset not found: {asset_id}"))
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

    fn generate_proxy_untracked(&self, asset_id: &str) -> Result<Asset> {
        let asset = self.refresh_asset_orientation(asset_id)?;
        if !Path::new(&asset.path).exists() {
            bail!("original is offline: {}", asset.path);
        }
        let parent = self
            .database_path
            .parent()
            .ok_or_else(|| anyhow!("catalog has no parent directory"))?;
        let proxy_dir = parent.join("cache").join("proxies").join(&asset.project_id);
        fs::create_dir_all(&proxy_dir)?;
        let output = proxy_dir.join(format!("{}.{}.mp4", asset.id, PROXY_CACHE_VERSION));
        let temporary = proxy_dir.join(format!("{}.{}.tmp.mp4", asset.id, PROXY_CACHE_VERSION));
        let status = tool_command("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-display_rotation:v:0",
                "0",
                "-noautorotate",
                "-i",
            ])
            .arg(&asset.path)
            .args([
                "-vf",
                "scale='min(1280,iw)':-2:force_original_aspect_ratio=decrease",
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-crf",
                "23",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-b:a",
                "128k",
                "-movflags",
                "+faststart",
                "-map_metadata",
                "-1",
            ])
            .arg(&temporary)
            .status()
            .context("start ffmpeg proxy process")?;
        if !status.success() {
            let _ = fs::remove_file(&temporary);
            bail!("ffmpeg proxy generation failed with {status}");
        }
        let proxy_probe = probe_media_stream(&temporary);
        match proxy_probe {
            Ok(probe) if probe.metadata_rotation_degrees == 0 => {}
            Ok(probe) => {
                let _ = fs::remove_file(&temporary);
                bail!(
                    "ffmpeg proxy validation failed: output retains a {}° display rotation",
                    probe.metadata_rotation_degrees
                );
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(error).context("validate generated proxy");
            }
        }
        fs::rename(&temporary, &output).context("commit generated proxy")?;
        self.connection()?.execute(
            "UPDATE assets SET proxy_path = ?2 WHERE id = ?1",
            params![asset_id, output.to_string_lossy()],
        )?;
        self.get_asset(asset_id)
    }

    /// Explicitly refreshes legacy orientation metadata from the immutable original.
    pub fn refresh_asset_orientation(&self, asset_id: &str) -> Result<Asset> {
        let asset = self.get_asset(asset_id)?;
        if asset.orientation_probed || !Path::new(&asset.path).is_file() {
            if !Path::new(&asset.path).is_file() && !asset.orientation_probed {
                bail!("original is offline: {}", asset.path);
            }
            return Ok(asset);
        }

        let probe = probe_media(Path::new(&asset.path))?;
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

    pub fn generate_proxy_with_progress<F>(
        &self,
        asset_id: &str,
        mut on_progress: F,
    ) -> Result<Asset>
    where
        F: FnMut(AssetBatchProgress),
    {
        let asset = self.get_asset(asset_id)?;
        let task_work = [asset.size_bytes];
        let mut estimator = BatchEstimator::new(
            &task_work,
            PROXY_WORKERS,
            self.load_batch_history(PROXY_TASK_KIND)?,
        );
        on_progress(AssetBatchProgress {
            asset: None,
            estimate: estimator.estimate(&task_work),
        });
        let started = Instant::now();
        let result = self.generate_proxy_untracked(asset_id);
        let updated = match &result {
            Ok(updated) => {
                estimator.record_completion(asset.size_bytes, started.elapsed());
                self.save_batch_history(PROXY_TASK_KIND, estimator.history())?;
                Some(updated.clone())
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
        result
    }

    pub fn generate_missing_proxies(&self, project_id: &str) -> Result<ProxyReport> {
        self.generate_missing_proxies_with_progress(project_id, |_| {})
    }

    pub fn generate_missing_proxies_with_progress<F>(
        &self,
        project_id: &str,
        mut on_progress: F,
    ) -> Result<ProxyReport>
    where
        F: FnMut(AssetBatchProgress),
    {
        let assets = self.list_assets(project_id)?;
        let (ready, pending): (Vec<_>, Vec<_>) = assets.into_iter().partition(|asset| {
            has_current_proxy(asset) || (!asset.original_available && asset.proxy_available)
        });
        let mut report = ProxyReport {
            generated: 0,
            skipped: ready.len(),
            failures: Vec::new(),
        };
        let task_work: Vec<u64> = pending.iter().map(|asset| asset.size_bytes).collect();
        let mut estimator = BatchEstimator::new(
            &task_work,
            PROXY_WORKERS,
            self.load_batch_history(PROXY_TASK_KIND)?,
        );
        on_progress(AssetBatchProgress {
            asset: None,
            estimate: estimator.estimate(&task_work),
        });
        for (index, asset) in pending.into_iter().enumerate() {
            let started = Instant::now();
            let updated = match self.generate_proxy_untracked(&asset.id) {
                Ok(updated) => {
                    report.generated += 1;
                    estimator.record_completion(asset.size_bytes, started.elapsed());
                    self.save_batch_history(PROXY_TASK_KIND, estimator.history())?;
                    Some(updated)
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

    pub fn generate_thumbnail(&self, asset_id: &str, quality: u8) -> Result<Asset> {
        if quality != 1 {
            bail!("invalid thumbnail quality {quality}; MVP supports quality 1");
        }
        let asset = self.get_asset(asset_id)?;
        let input = if Path::new(&asset.path).is_file() {
            PathBuf::from(&asset.path)
        } else if let Some(proxy) = asset
            .proxy_path
            .as_deref()
            .filter(|path| Path::new(path).is_file())
        {
            PathBuf::from(proxy)
        } else {
            bail!("media is offline and no proxy is available: {}", asset.path);
        };
        let parent = self
            .database_path
            .parent()
            .ok_or_else(|| anyhow!("catalog has no parent directory"))?;
        let thumbnail_dir = parent.join("cache").join("thumbnails").join(&asset.id);
        fs::create_dir_all(&thumbnail_dir)?;
        let output = thumbnail_dir.join(format!("q{quality}.{THUMBNAIL_CACHE_VERSION}.jpg"));
        let temporary = thumbnail_dir.join(format!("q{quality}.{THUMBNAIL_CACHE_VERSION}.tmp.jpg"));
        let seek = (asset.duration_seconds.max(0.0) * 0.2).to_string();
        let status = tool_command("ffmpeg")
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
            .arg(&temporary)
            .status()
            .context("start ffmpeg thumbnail process")?;
        if !status.success() {
            let _ = fs::remove_file(&temporary);
            bail!("ffmpeg thumbnail generation failed with {status}");
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
        mut on_progress: F,
    ) -> Result<ThumbnailReport>
    where
        F: FnMut(AssetBatchProgress),
    {
        if quality != 1 {
            bail!("invalid thumbnail quality {quality}; MVP supports quality 1");
        }
        let assets = self.list_assets(project_id)?;
        let (ready, pending): (Vec<_>, Vec<_>) = assets
            .into_iter()
            .partition(|asset| has_current_thumbnail(asset, quality));
        let mut report = ThumbnailReport {
            quality,
            generated: 0,
            skipped: ready.len(),
            failures: Vec::new(),
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
            let updated = match self.generate_thumbnail(&asset.id, quality) {
                Ok(updated) => {
                    report.generated += 1;
                    estimator.record_completion(asset.size_bytes, started.elapsed());
                    self.save_batch_history(THUMBNAIL_TASK_KIND, estimator.history())?;
                    Some(updated)
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

fn has_current_thumbnail(asset: &Asset, quality: u8) -> bool {
    asset.thumbnail_quality >= quality
        && asset.thumbnail_path.as_deref().is_some_and(|path| {
            Path::new(path).is_file()
                && path.ends_with(&format!("q{quality}.{THUMBNAIL_CACHE_VERSION}.jpg"))
        })
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

fn row_to_asset(row: &rusqlite::Row<'_>) -> rusqlite::Result<Asset> {
    let path: String = row.get(2)?;
    let proxy_path: Option<String> = row.get(15)?;
    let original_available = Path::new(&path).is_file();
    let proxy_available = proxy_path
        .as_deref()
        .is_some_and(|path| Path::new(path).is_file());
    let stored_thumbnail_path: Option<String> = row.get(20)?;
    let stored_thumbnail_quality = row.get::<_, i64>(21)? as u8;
    let thumbnail_available = stored_thumbnail_path
        .as_deref()
        .is_some_and(|path| Path::new(path).is_file());
    let available = original_available || proxy_available;
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
        labels: serde_json::from_str(&row.get::<_, String>(23)?).unwrap_or_default(),
        removed: row.get::<_, i64>(24)? != 0,
        media_object_id: row.get(25)?,
        media_verification_state,
        available,
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

fn tool_command(name: &str) -> Command {
    Command::new(resolve_tool(name))
}

/// Resolves a tool executable path from the environment. Returns the command
/// name itself when no concrete path is found on PATH or standard directories.
pub fn resolve_tool_path(name: &str) -> PathBuf {
    resolve_tool(name)
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
    let stream = document["streams"]
        .as_array()
        .and_then(|items| items.first());
    let raw_time = stream
        .and_then(|value| value.pointer("/tags/creation_time"))
        .or_else(|| document.pointer("/format/tags/creation_time"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let camera = exif_camera(path).unwrap_or_else(|| "Unknown camera".to_string());
    let duration_seconds = document["format"]["duration"]
        .as_str()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0.0);
    let rate = stream
        .and_then(|value| value["r_frame_rate"].as_str())
        .map(parse_rate)
        .unwrap_or(30.0);
    let metadata_rotation_degrees = stream_rotation(stream);
    let width = stream
        .and_then(|value| value["width"].as_u64())
        .unwrap_or(0) as u32;
    let height = stream
        .and_then(|value| value["height"].as_u64())
        .unwrap_or(0) as u32;
    Ok(MediaProbe {
        duration_seconds,
        frame_rate: rate,
        width,
        height,
        codec: stream
            .and_then(|value| value["codec_name"].as_str())
            .unwrap_or("unknown")
            .to_string(),
        camera,
        captured_at_raw: raw_time.clone(),
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
    pub best_time: Option<String>,
    pub metadata_rotation_degrees: u16,
    pub enrichment_status: String,
    pub enrichment_warning: Option<String>,
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
    let stream = document["streams"]
        .as_array()
        .and_then(|items| items.first());
    let raw_time = stream
        .and_then(|value| value.pointer("/tags/creation_time"))
        .or_else(|| document.pointer("/format/tags/creation_time"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let duration_seconds = document["format"]["duration"]
        .as_str()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0.0);
    let rate = stream
        .and_then(|value| value["r_frame_rate"].as_str())
        .map(parse_rate)
        .unwrap_or(30.0);
    let metadata_rotation_degrees = stream_rotation(stream);
    let width = stream
        .and_then(|value| value["width"].as_u64())
        .unwrap_or(0) as u32;
    let height = stream
        .and_then(|value| value["height"].as_u64())
        .unwrap_or(0) as u32;
    Ok(NormalizedMediaProbe {
        duration_seconds,
        frame_rate: rate,
        encoded_width: width,
        encoded_height: height,
        codec: stream
            .and_then(|value| value["codec_name"].as_str())
            .unwrap_or("unknown")
            .to_string(),
        camera_make,
        camera_model,
        camera_string,
        captured_at_raw: raw_time.clone(),
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
    let output = Command::new(ffprobe_program)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "format=duration:format_tags=creation_time:stream=width,height,codec_name,r_frame_rate:stream_tags=creation_time,rotate:stream_side_data=rotation",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .context("start ffprobe")?;
    if !output.status.success() {
        bail!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).context("parse ffprobe JSON")
}

/// Extracts camera make/model via a specific ExifTool executable.
fn exif_camera_with(path: &Path, exiftool_program: &Path) -> Option<(String, String)> {
    let output = Command::new(exiftool_program)
        .args(["-json", "-Make", "-Model"])
        .arg(path)
        .output()
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
    let output = tool_command("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "format=duration:format_tags=creation_time:stream=width,height,codec_name,r_frame_rate:stream_tags=creation_time,rotate:stream_side_data=rotation",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .context("start ffprobe")?;
    if !output.status.success() {
        bail!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
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
fn exif_camera(path: &Path) -> Option<String> {
    let output = tool_command("exiftool")
        .args(["-json", "-Make", "-Model"])
        .arg(path)
        .output()
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

fn parse_rate(value: &str) -> f64 {
    let mut parts = value.split('/');
    let numerator = parts
        .next()
        .and_then(|part| part.parse::<f64>().ok())
        .unwrap_or(30.0);
    let denominator = parts
        .next()
        .and_then(|part| part.parse::<f64>().ok())
        .unwrap_or(1.0);
    if denominator == 0.0 {
        30.0
    } else {
        numerator / denominator
    }
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

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{TempDir, tempdir};

    fn catalog_fixture() -> (TempDir, Catalog, Project) {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Test project").unwrap();
        (directory, catalog, project)
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
                },
            )
            .unwrap();
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
        drop(upgraded);

        let first_history = inspect_catalog(&path).unwrap().applied_migrations;
        assert_eq!(first_history.len(), PROJECT_CATALOG_SCHEMA_VERSION as usize);
        let reopened = Catalog::open_existing(&path).unwrap();
        assert_eq!(
            reopened.get_asset("asset_existing").unwrap().id,
            "asset_existing"
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
        assert_eq!(before.pending_migrations.len(), 2);
        let catalog = Catalog::open_existing(&path).unwrap();
        let asset = catalog.get_asset("asset_v4").unwrap();
        assert_eq!(asset.notes, "kept note");
        assert_eq!(asset.labels, ["kept label"]);
        assert!(asset.removed);
        assert_eq!(asset.rotation_degrees, 90);
        assert_eq!(catalog.list_operations("project_v4").unwrap().len(), 0);
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
        assert!((parse_rate("60000/1001") - 59.94005994).abs() < 0.0001);
        assert_eq!(parse_rate("30/0"), 30.0);
        assert_eq!(parse_rate("invalid"), 30.0);
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
            .generate_missing_proxies_with_progress(&project.id, |progress| {
                proxy_progress.push(progress);
            })
            .unwrap();
        assert_eq!(first_proxy_report.generated, 1);
        assert_eq!(proxy_progress.len(), 2);
        assert_eq!(proxy_progress[0].estimate.completed_items, 0);
        assert_eq!(proxy_progress[1].estimate.completed_items, 1);
        assert_eq!(proxy_progress[1].estimate.total_items, 1);
        assert_eq!(proxy_progress[1].estimate.worker_count, 1);
        assert_eq!(
            proxy_progress[1].estimate.estimated_remaining_seconds,
            Some(0.0)
        );
        assert_eq!(
            catalog.load_batch_history(PROXY_TASK_KIND).unwrap().samples,
            1
        );
        let proxied = proxy_progress[1].asset.as_ref().unwrap();
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
            .generate_proxy_with_progress(&asset.id, |progress| {
                single_proxy_progress.push(progress);
            })
            .unwrap();
        assert_eq!(single_proxy_progress.len(), 2);
        assert!(
            single_proxy_progress[0]
                .estimate
                .estimated_remaining_seconds
                .is_some()
        );
        assert_eq!(
            single_proxy_progress[1]
                .estimate
                .estimated_remaining_seconds,
            Some(0.0)
        );
        assert!(Path::new(regenerated_proxy.proxy_path.as_deref().unwrap()).is_file());
        assert_eq!(
            catalog.load_batch_history(PROXY_TASK_KIND).unwrap().samples,
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
    }
}
