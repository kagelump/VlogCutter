use anyhow::{Context, Result, anyhow, bail};
use batch_estimator::{BatchEstimate, BatchEstimator, RateHistory};
use blake3::Hasher;
use chrono::{DateTime, Duration, NaiveDateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::env;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;
use url::Url;
use uuid::Uuid;
use walkdir::WalkDir;

const SAMPLE_BYTES: usize = 1024 * 1024;
const MEDIA_EXTENSIONS: &[&str] = &["mp4", "mov", "m4v", "mts", "m2ts", "avi"];
const PROXY_TASK_KIND: &str = "proxy.h264-720p.v1";
const THUMBNAIL_TASK_KIND: &str = "thumbnail.q1.v1";
const PROXY_WORKERS: usize = 1;
const THUMBNAIL_WORKERS: usize = 1;

#[derive(Debug, Clone)]
pub struct Catalog {
    database_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub asset_count: u64,
    pub reviewed_count: u64,
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
    pub original_available: bool,
    pub proxy_available: bool,
    pub thumbnail_path: Option<String>,
    pub thumbnail_quality: u8,
    pub notes: String,
    pub status: ReviewStatus,
    pub included: bool,
    pub source_in: f64,
    pub source_out: f64,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Unreviewed,
    Reviewed,
    Skipped,
    NeedsAttention,
}

impl ReviewStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Unreviewed => "unreviewed",
            Self::Reviewed => "reviewed",
            Self::Skipped => "skipped",
            Self::NeedsAttention => "needs_attention",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "reviewed" => Self::Reviewed,
            "skipped" => Self::Skipped,
            "needs_attention" => Self::NeedsAttention,
            _ => Self::Unreviewed,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetPatch {
    pub notes: String,
    pub status: ReviewStatus,
    pub included: bool,
    pub source_in: f64,
    pub source_out: f64,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

impl Catalog {
    pub fn open(database_path: impl AsRef<Path>) -> Result<Self> {
        let database_path = database_path.as_ref().to_path_buf();
        if let Some(parent) = database_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create catalog directory {}", parent.display()))?;
        }
        let catalog = Self { database_path };
        catalog.migrate()?;
        Ok(catalog)
    }

    pub fn open_existing(database_path: impl AsRef<Path>) -> Result<Self> {
        let database_path = database_path.as_ref();
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
        drop(connection);
        Self::open(database_path)
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    fn connection(&self) -> Result<Connection> {
        let connection = Connection::open(&self.database_path)
            .with_context(|| format!("open catalog {}", self.database_path.display()))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(connection)
    }

    fn migrate(&self) -> Result<()> {
        let connection = self.connection()?;
        connection.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS projects (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS assets (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                fingerprint TEXT NOT NULL,
                path TEXT NOT NULL,
                filename TEXT NOT NULL,
                size_bytes INTEGER NOT NULL,
                duration_seconds REAL NOT NULL,
                frame_rate REAL NOT NULL DEFAULT 30,
                width INTEGER NOT NULL DEFAULT 0,
                height INTEGER NOT NULL DEFAULT 0,
                codec TEXT NOT NULL DEFAULT '',
                camera TEXT NOT NULL DEFAULT 'Unknown camera',
                captured_at_raw TEXT,
                best_time TEXT,
                offset_seconds INTEGER NOT NULL DEFAULT 0,
                proxy_path TEXT,
                notes TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'unreviewed',
                included INTEGER NOT NULL DEFAULT 0,
                source_in REAL NOT NULL DEFAULT 0,
                source_out REAL NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                UNIQUE(project_id, fingerprint)
            );

            CREATE INDEX IF NOT EXISTS assets_project_order
            ON assets(project_id, best_time, filename, id);

            CREATE TABLE IF NOT EXISTS clock_events (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                camera TEXT NOT NULL,
                start_filename TEXT NOT NULL,
                offset_seconds INTEGER NOT NULL,
                label TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS asset_thumbnails (
                asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
                quality INTEGER NOT NULL CHECK(quality BETWEEN 1 AND 10),
                path TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY(asset_id, quality)
            );

            CREATE TABLE IF NOT EXISTS batch_performance (
                task_kind TEXT PRIMARY KEY,
                work_units_per_worker_second REAL NOT NULL,
                samples INTEGER NOT NULL,
                updated_at TEXT NOT NULL
            );
            "#,
        )?;
        Ok(())
    }

    pub fn create_project(&self, name: &str) -> Result<Project> {
        let name = name.trim();
        if name.is_empty() {
            bail!("project name cannot be empty");
        }
        let id = format!("project_{}", Uuid::new_v4().simple());
        let created_at = now();
        self.connection()?.execute(
            "INSERT INTO projects (id, name, created_at) VALUES (?1, ?2, ?3)",
            params![id, name, created_at],
        )?;
        Ok(Project {
            id,
            name: name.to_string(),
            created_at,
            asset_count: 0,
            reviewed_count: 0,
        })
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT p.id, p.name, p.created_at,
                   COUNT(a.id),
                   COALESCE(SUM(CASE WHEN a.status = 'reviewed' THEN 1 ELSE 0 END), 0)
            FROM projects p
            LEFT JOIN assets a ON a.project_id = p.id
            GROUP BY p.id
            ORDER BY p.created_at DESC
            "#,
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Project {
                id: row.get(0)?,
                name: row.get(1)?,
                created_at: row.get(2)?,
                asset_count: row.get::<_, i64>(3)? as u64,
                reviewed_count: row.get::<_, i64>(4)? as u64,
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
        connection.execute(
            r#"
            INSERT INTO assets (
                id, project_id, fingerprint, path, filename, size_bytes,
                duration_seconds, frame_rate, width, height, codec, camera,
                captured_at_raw, best_time, source_out, created_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16
            )
            "#,
            params![
                id,
                project_id,
                fingerprint,
                path.to_string_lossy(),
                filename,
                metadata.len() as i64,
                probe.duration_seconds,
                probe.frame_rate,
                probe.width,
                probe.height,
                probe.codec,
                probe.camera,
                probe.captured_at_raw,
                probe.best_time,
                probe.duration_seconds,
                now(),
            ],
        )?;
        Ok(true)
    }

    pub fn list_assets(&self, project_id: &str) -> Result<Vec<Asset>> {
        self.require_project(project_id)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT a.id, a.project_id, a.path, a.filename, a.fingerprint, a.size_bytes,
                   a.duration_seconds, a.frame_rate, a.width, a.height, a.codec, a.camera,
                   a.captured_at_raw, a.best_time, a.offset_seconds, a.proxy_path,
                   t.path, COALESCE(t.quality, 0),
                   a.notes, a.status, a.included, a.source_in, a.source_out
            FROM assets a
            LEFT JOIN asset_thumbnails t
              ON t.asset_id = a.id
             AND t.quality = (SELECT MAX(t2.quality) FROM asset_thumbnails t2 WHERE t2.asset_id = a.id)
            WHERE a.project_id = ?1
            ORDER BY a.best_time IS NULL, a.best_time, a.filename, a.id
            "#,
        )?;
        let rows = statement.query_map([project_id], row_to_asset)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get_asset(&self, asset_id: &str) -> Result<Asset> {
        let connection = self.connection()?;
        connection
            .query_row(
                r#"
                SELECT a.id, a.project_id, a.path, a.filename, a.fingerprint, a.size_bytes,
                       a.duration_seconds, a.frame_rate, a.width, a.height, a.codec, a.camera,
                       a.captured_at_raw, a.best_time, a.offset_seconds, a.proxy_path,
                       t.path, COALESCE(t.quality, 0),
                       a.notes, a.status, a.included, a.source_in, a.source_out
                FROM assets a
                LEFT JOIN asset_thumbnails t
                  ON t.asset_id = a.id
                 AND t.quality = (SELECT MAX(t2.quality) FROM asset_thumbnails t2 WHERE t2.asset_id = a.id)
                WHERE a.id = ?1
                "#,
                [asset_id],
                row_to_asset,
            )
            .optional()?
            .ok_or_else(|| anyhow!("asset not found: {asset_id}"))
    }

    pub fn update_asset(&self, asset_id: &str, patch: &AssetPatch) -> Result<Asset> {
        let current = self.get_asset(asset_id)?;
        if !patch.source_in.is_finite()
            || !patch.source_out.is_finite()
            || patch.source_in < 0.0
            || patch.source_out < patch.source_in
            || patch.source_out > current.duration_seconds
        {
            bail!(
                "invalid source range: {}–{} for {:.3}-second asset",
                patch.source_in,
                patch.source_out,
                current.duration_seconds
            );
        }
        let connection = self.connection()?;
        let changed = connection.execute(
            r#"
            UPDATE assets
            SET notes = ?2, status = ?3, included = ?4, source_in = ?5, source_out = ?6
            WHERE id = ?1
            "#,
            params![
                asset_id,
                patch.notes,
                patch.status.as_str(),
                patch.included,
                patch.source_in,
                patch.source_out,
            ],
        )?;
        if changed == 0 {
            bail!("asset not found: {asset_id}");
        }
        self.get_asset(asset_id)
    }

    pub fn apply_clock_event(
        &self,
        project_id: &str,
        camera: &str,
        start_filename: &str,
        offset_seconds: i64,
        label: &str,
    ) -> Result<ClockEvent> {
        self.require_project(project_id)?;
        let event = ClockEvent {
            id: format!("clock_{}", Uuid::new_v4().simple()),
            project_id: project_id.to_string(),
            camera: camera.to_string(),
            start_filename: start_filename.to_string(),
            offset_seconds,
            label: label.trim().to_string(),
            created_at: now(),
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            r#"
            INSERT INTO clock_events
                (id, project_id, camera, start_filename, offset_seconds, label, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![
                event.id,
                event.project_id,
                event.camera,
                event.start_filename,
                event.offset_seconds,
                event.label,
                event.created_at,
            ],
        )?;

        let mut event_statement = transaction.prepare(
            r#"
            SELECT start_filename, offset_seconds FROM clock_events
            WHERE project_id = ?1 AND camera = ?2
            ORDER BY start_filename, created_at, rowid
            "#,
        )?;
        let events = event_statement
            .query_map(params![project_id, camera], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(event_statement);

        let mut asset_statement = transaction.prepare(
            r#"
            SELECT id, filename, captured_at_raw FROM assets
            WHERE project_id = ?1 AND camera = ?2
            "#,
        )?;
        let affected = asset_statement
            .query_map(params![project_id, camera], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(asset_statement);

        for (asset_id, filename, raw) in affected {
            let effective_offset = events
                .iter()
                .filter(|(boundary, _)| boundary <= &filename)
                .map(|(_, offset)| *offset)
                .next_back()
                .unwrap_or(0);
            let best_time = raw
                .as_deref()
                .and_then(|value| normalize_datetime(value, effective_offset));
            transaction.execute(
                "UPDATE assets SET offset_seconds = ?2, best_time = ?3 WHERE id = ?1",
                params![asset_id, effective_offset, best_time],
            )?;
        }
        transaction.commit()?;
        Ok(event)
    }

    fn generate_proxy_untracked(&self, asset_id: &str) -> Result<Asset> {
        let asset = self.get_asset(asset_id)?;
        if !Path::new(&asset.path).exists() {
            bail!("original is offline: {}", asset.path);
        }
        let parent = self
            .database_path
            .parent()
            .ok_or_else(|| anyhow!("catalog has no parent directory"))?;
        let proxy_dir = parent.join("cache").join("proxies").join(&asset.project_id);
        fs::create_dir_all(&proxy_dir)?;
        let output = proxy_dir.join(format!("{}.mp4", asset.id));
        let status = tool_command("ffmpeg")
            .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
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
            ])
            .arg(&output)
            .status()
            .context("start ffmpeg proxy process")?;
        if !status.success() {
            bail!("ffmpeg proxy generation failed with {status}");
        }
        self.connection()?.execute(
            "UPDATE assets SET proxy_path = ?2 WHERE id = ?1",
            params![asset_id, output.to_string_lossy()],
        )?;
        self.get_asset(asset_id)
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
            asset
                .proxy_path
                .as_deref()
                .is_some_and(|path| Path::new(path).exists())
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
        let output = thumbnail_dir.join(format!("q{quality}.jpg"));
        let temporary = thumbnail_dir.join(format!("q{quality}.tmp.jpg"));
        let seek = (asset.duration_seconds.max(0.0) * 0.2).to_string();
        let status = tool_command("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-ss",
                &seek,
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
        let (ready, pending): (Vec<_>, Vec<_>) = assets.into_iter().partition(|asset| {
            asset.thumbnail_quality >= quality
                && asset
                    .thumbnail_path
                    .as_deref()
                    .is_some_and(|path| Path::new(path).is_file())
        });
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

    pub fn export_otio(&self, project_id: &str) -> Result<PathBuf> {
        let project = self
            .list_projects()?
            .into_iter()
            .find(|project| project.id == project_id)
            .ok_or_else(|| anyhow!("project not found: {project_id}"))?;
        let assets: Vec<Asset> = self
            .list_assets(project_id)?
            .into_iter()
            .filter(|asset| asset.included)
            .collect();
        if assets.is_empty() {
            bail!("rough cut is empty; include at least one clip");
        }
        let children: Vec<Value> = assets.iter().map(asset_to_otio_clip).collect();
        let timeline = json!({
            "OTIO_SCHEMA": "Timeline.1",
            "name": format!("{} Rough Cut", project.name),
            "global_start_time": Value::Null,
            "metadata": { "rcdvlog": { "project_id": project.id, "exported_at": now() } },
            "tracks": {
                "OTIO_SCHEMA": "Stack.1",
                "name": "tracks",
                "effects": [],
                "markers": [],
                "metadata": {},
                "source_range": Value::Null,
                "enabled": true,
                "color": Value::Null,
                "children": [{
                    "OTIO_SCHEMA": "Track.1",
                    "name": "V1",
                    "kind": "Video",
                    "effects": [],
                    "markers": [],
                    "metadata": {},
                    "source_range": Value::Null,
                    "enabled": true,
                    "color": Value::Null,
                    "children": children
                }]
            }
        });
        let parent = self
            .database_path
            .parent()
            .unwrap_or_else(|| Path::new("."));
        let export_dir = parent.join("exports").join(project_id);
        fs::create_dir_all(&export_dir)?;
        let path = export_dir.join(format!(
            "rough-cut-{}-{}.otio",
            Utc::now().format("%Y%m%d-%H%M%S"),
            &Uuid::new_v4().simple().to_string()[..8]
        ));
        let mut file = File::create(&path)?;
        serde_json::to_writer_pretty(&mut file, &timeline)?;
        file.write_all(b"\n")?;
        Ok(path)
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

fn row_to_asset(row: &rusqlite::Row<'_>) -> rusqlite::Result<Asset> {
    let path: String = row.get(2)?;
    let proxy_path: Option<String> = row.get(15)?;
    let original_available = Path::new(&path).is_file();
    let proxy_available = proxy_path
        .as_deref()
        .is_some_and(|path| Path::new(path).is_file());
    let stored_thumbnail_path: Option<String> = row.get(16)?;
    let stored_thumbnail_quality = row.get::<_, i64>(17)? as u8;
    let thumbnail_available = stored_thumbnail_path
        .as_deref()
        .is_some_and(|path| Path::new(path).is_file());
    let available = original_available || proxy_available;
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
        proxy_path,
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
        notes: row.get(18)?,
        status: ReviewStatus::parse(&row.get::<_, String>(19)?),
        included: row.get::<_, i64>(20)? != 0,
        source_in: row.get(21)?,
        source_out: row.get(22)?,
        available,
    })
}

fn is_media(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| MEDIA_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn tool_command(name: &str) -> Command {
    Command::new(resolve_tool(name))
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

fn probe_media(path: &Path) -> Result<MediaProbe> {
    let output = tool_command("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "format=duration:format_tags=creation_time:stream=width,height,codec_name,r_frame_rate:stream_tags=creation_time",
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
    let document: Value = serde_json::from_slice(&output.stdout).context("parse ffprobe JSON")?;
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
    Ok(MediaProbe {
        duration_seconds,
        frame_rate: rate,
        width: stream
            .and_then(|value| value["width"].as_u64())
            .unwrap_or(0) as u32,
        height: stream
            .and_then(|value| value["height"].as_u64())
            .unwrap_or(0) as u32,
        codec: stream
            .and_then(|value| value["codec_name"].as_str())
            .unwrap_or("unknown")
            .to_string(),
        camera,
        captured_at_raw: raw_time.clone(),
        best_time: raw_time
            .as_deref()
            .and_then(|value| normalize_datetime(value, 0)),
    })
}

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

fn asset_to_otio_clip(asset: &Asset) -> Value {
    let rate = if asset.frame_rate > 0.0 {
        asset.frame_rate
    } else {
        30.0
    };
    let source_in = asset.source_in.max(0.0);
    let source_out = asset.source_out.min(asset.duration_seconds).max(source_in);
    let duration = source_out - source_in;
    let target_url = Url::from_file_path(&asset.path)
        .map(String::from)
        .unwrap_or_else(|_| asset.path.clone());
    json!({
        "OTIO_SCHEMA": "Clip.2",
        "name": asset.filename,
        "effects": [],
        "markers": [],
        "enabled": true,
        "color": Value::Null,
        "metadata": { "rcdvlog": { "asset_id": asset.id, "notes": asset.notes } },
        "source_range": time_range(source_in, duration, rate),
        "media_references": {
            "DEFAULT_MEDIA": {
                "OTIO_SCHEMA": "ExternalReference.1",
                "name": "",
                "metadata": {},
                "target_url": target_url,
                "available_range": time_range(0.0, asset.duration_seconds, rate),
                "available_image_bounds": Value::Null
            }
        },
        "active_media_reference_key": "DEFAULT_MEDIA"
    })
}

fn time_range(start_seconds: f64, duration_seconds: f64, rate: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "TimeRange.1",
        "start_time": {
            "OTIO_SCHEMA": "RationalTime.1",
            "value": start_seconds * rate,
            "rate": rate
        },
        "duration": {
            "OTIO_SCHEMA": "RationalTime.1",
            "value": duration_seconds * rate,
            "rate": rate
        }
    })
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
    fn validates_projects_asset_ranges_and_missing_ids() {
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
        for (source_in, source_out) in [
            (-1.0, 2.0),
            (4.0, 3.0),
            (0.0, 11.0),
            (f64::NAN, 2.0),
            (0.0, f64::INFINITY),
        ] {
            let result = catalog.update_asset(
                "asset_a",
                &AssetPatch {
                    notes: current.notes.clone(),
                    status: current.status.clone(),
                    included: false,
                    source_in,
                    source_out,
                },
            );
            assert!(result.is_err());
        }
        assert!(catalog.get_asset("asset_missing").is_err());
        assert!(catalog.list_assets("project_missing").is_err());
    }

    #[test]
    fn orders_by_best_time_then_filename_and_counts_reviewed_assets() {
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
                    notes: "reviewed".to_string(),
                    status: ReviewStatus::Reviewed,
                    included: false,
                    source_in: 0.0,
                    source_out: tie.duration_seconds,
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
        assert_eq!(summary.reviewed_count, 1);
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
    fn otio_contains_only_included_assets_in_chronological_order() {
        let (directory, catalog, project) = catalog_fixture();
        let source_dir = directory.path().join("source with spaces");
        fs::create_dir_all(&source_dir).unwrap();
        for (id, filename, raw_time) in [
            ("late", "LATE.MP4", "2026-07-18T03:00:00Z"),
            ("early", "EARLY.MP4", "2026-07-18T01:00:00Z"),
            ("excluded", "EXCLUDED.MP4", "2026-07-18T02:00:00Z"),
        ] {
            let path = source_dir.join(filename);
            fs::write(&path, b"source sentinel").unwrap();
            insert_asset(
                &catalog,
                &project.id,
                id,
                filename,
                "Camera A",
                raw_time,
                &path,
            );
        }
        for (id, source_in, source_out, notes) in
            [("early", 1.0, 7.0, "first"), ("late", 2.0, 8.0, "second")]
        {
            let asset = catalog.get_asset(id).unwrap();
            catalog
                .update_asset(
                    id,
                    &AssetPatch {
                        notes: notes.to_string(),
                        status: asset.status,
                        included: true,
                        source_in,
                        source_out,
                    },
                )
                .unwrap();
        }

        let otio_path = catalog.export_otio(&project.id).unwrap();
        let otio: Value = serde_json::from_slice(&fs::read(&otio_path).unwrap()).unwrap();
        let clips = otio["tracks"]["children"][0]["children"]
            .as_array()
            .unwrap();
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0]["name"], "EARLY.MP4");
        assert_eq!(clips[1]["name"], "LATE.MP4");
        assert_eq!(clips[0]["source_range"]["start_time"]["value"], 30.0);
        assert_eq!(clips[0]["source_range"]["duration"]["value"], 180.0);
        assert_eq!(clips[0]["metadata"]["rcdvlog"]["notes"], "first");
        assert!(
            clips[0]["media_references"]["DEFAULT_MEDIA"]["target_url"]
                .as_str()
                .unwrap()
                .contains("source%20with%20spaces")
        );

        let second_path = catalog.export_otio(&project.id).unwrap();
        assert_ne!(second_path, otio_path);
        assert!(second_path.exists());
    }

    #[test]
    fn empty_rough_cut_is_rejected() {
        let (_directory, catalog, project) = catalog_fixture();
        assert!(catalog.export_otio(&project.id).is_err());
    }

    #[test]
    fn recognizes_media_extensions_and_parses_rates() {
        assert!(is_media(Path::new("clip.MP4")));
        assert!(is_media(Path::new("clip.m2ts")));
        assert!(!is_media(Path::new("notes.txt")));
        assert!((parse_rate("60000/1001") - 59.94005994).abs() < 0.0001);
        assert_eq!(parse_rate("30/0"), 30.0);
        assert_eq!(parse_rate("invalid"), 30.0);
    }

    #[test]
    fn media_round_trip_ingest_proxy_notes_clock_and_otio() {
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
                    status: ReviewStatus::Reviewed,
                    included: true,
                    source_in: 0.0,
                    source_out: asset.duration_seconds,
                },
            )
            .unwrap();
        assert_eq!(updated.notes, "Opening station view");

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
        assert!(thumbnail_path.ends_with(format!("{}/q1.jpg", asset.id)));
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

        let otio_path = catalog.export_otio(&project.id).unwrap();
        let otio: Value = serde_json::from_slice(&fs::read(&otio_path).unwrap()).unwrap();
        assert_eq!(otio["OTIO_SCHEMA"], "Timeline.1");
        assert_eq!(
            otio["tracks"]["children"][0]["children"][0]["name"],
            "C0001.MP4"
        );
        let otio_available = tool_command("python3")
            .args(["-c", "import opentimelineio"])
            .status()
            .is_ok_and(|status| status.success());
        if otio_available {
            let status = tool_command("python3")
                .args([
                    "-c",
                    "import opentimelineio as otio, sys; otio.adapters.read_from_file(sys.argv[1])",
                ])
                .arg(&otio_path)
                .status()
                .unwrap();
            assert!(status.success(), "OpenTimelineIO rejected the Rust export");
        }
    }
}
