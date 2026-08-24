use anyhow::{Context, bail};
use chrono::{SecondsFormat, Utc};
use rcdvlog_core::{
    AnalysisReadiness, AnalysisRunOptions, AnalysisRunOutcome, AnalysisRunRecord, AnalyzeReport,
    ArchiveRoot, Asset, AssetAnalysisIndexEntry, AssetAnalysisView, AssetPatch,
    BatchProgressEstimate, CacheCleanupReport, CacheKind, CameraMapping, CameraMergePreview,
    CameraMergeReport, CardCandidate, Catalog, ClipTranscript, ClockEvent, EndpointProbe,
    ImportCutoff, ImportHistoryEntry, ImportOutcome, ImportPreflight, ImportPreviewItem,
    ImportPreviewRequest, ImportProgress as CoreImportProgress, ImportRollbackPreview,
    ImportSessionDetail, ImportStage as CoreImportStage, IngestReport, ModelStatus, MountedVolumes,
    Project, ProjectRemovalAudit, ProjectStorageReport, ProxyReport, RelinkReport, ThumbnailReport,
    ToolStatus, TranscribeReport, detect_cards, inspect_card,
};
use rcdvlog_sync::BaselineEnvelope;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use tauri::{Emitter, Manager, State};

mod chronology_commands;
#[cfg(debug_assertions)]
mod eval_bridge;
mod keychain;
mod media_server;
mod server_client;
mod sync_manager;

// ── Server connection types ────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerEndpoint {
    url: String,
    kind: String,
    label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PathPrefix {
    prefix: String,
    asset_count: usize,
    sample_assets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PathAnalysis {
    common_prefixes: Vec<PathPrefix>,
    total_assets: usize,
    absolute_path_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RootMappingEntry {
    local_prefix: String,
    storage_root_id: String,
    kind: String,
    label: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BaselinePreviewResult {
    digest: String,
    preview: rcdvlog_sync::BaselinePreview,
    endpoint_used: String,
    server_node_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BaselineImportResult {
    project_id: String,
    baseline_digest: String,
    reused_existing: bool,
    state: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectPairingStatus {
    paired: bool,
    server_node_id: Option<String>,
    paired_at: Option<String>,
}

// ── Baseline attempt (in-memory, survives within one app session) ─────

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct BaselineAttempt {
    project_id: String,
    digest: String,
    envelope_json: String,
}

pub(crate) struct AppState {
    catalog: Arc<RwLock<Catalog>>,
    settings: Arc<RwLock<DesktopSettings>>,
    settings_path: PathBuf,
    baseline_attempt: Arc<RwLock<Option<BaselineAttempt>>>,
    sync_manager: sync_manager::SyncManager,
    sync_status: Arc<RwLock<HashMap<String, sync_manager::SyncStatus>>>,
    /// Live cancel tokens for in-flight imports, analyses, and model
    /// downloads. Every run mints its own token, so a cancel reaches exactly
    /// the runs that were in flight when it was requested — never a later run,
    /// and never a run of another kind.
    operation_cancels: Arc<OperationCancels>,
    /// Session-local ownership for derived-media output paths — proxies and
    /// thumbnails both. Import-triggered work, explicit repair batches, and
    /// the sync manager's downloads and server-directed jobs share it so no
    /// two writers ever address the same temporary/output file concurrently.
    representations: Arc<RepresentationCoordinator>,
    /// The background work currently touching each project's files, so
    /// `delete_project` can refuse to remove a folder a writer is still
    /// working inside.
    project_work: Arc<ProjectWorkRegistry>,
    /// Resolves asset ids to on-disk media for the loopback origin. Held here so
    /// a restarted origin reads the same live catalog and settings.
    media_catalog: Arc<CatalogMedia>,
    /// `None` until the origin binds, and again if it could not be restarted.
    /// Media falls back to nothing rather than to a weaker transport.
    media_server: Arc<RwLock<Option<media_server::MediaServer>>>,
    /// The origin is restarted at most once per session, so a listener that
    /// cannot bind does not turn into a retry loop behind every media error.
    media_restarted: Arc<AtomicBool>,
}

/// Which derived-media slot of an asset a writer owns while it runs. Proxies
/// and thumbnails each live at one fixed per-asset cache path (temporary and
/// final), so each slot admits exactly one writer at a time — but the two
/// slots of the same asset are different files and never conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RepresentationKind {
    Proxy,
    Thumbnail,
}

/// Session-local ownership of representation output paths, keyed by
/// (kind, asset id).
///
/// A slot already claimed is skipped, never queued: the holder is producing
/// the very file the second writer wanted, and a sync job that skips is
/// simply re-offered by the server on its next cycle.
#[derive(Default)]
struct RepresentationCoordinator {
    in_flight: Mutex<HashSet<(RepresentationKind, String)>>,
    /// Serializes thumbnail batches as well as per-asset ownership, preserving
    /// the measured one-worker policy across overlapping command invocations.
    /// Proxy batches stay outside it: they bring their own bounded worker
    /// pool, and per-asset claims already keep their output paths disjoint.
    thumbnail_execution: Mutex<()>,
}

impl RepresentationCoordinator {
    fn claim(
        self: &Arc<Self>,
        kind: RepresentationKind,
        asset_ids: impl IntoIterator<Item = String>,
    ) -> RepresentationClaims {
        let mut in_flight = self
            .in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut claimed = Vec::new();
        for asset_id in asset_ids {
            if in_flight.insert((kind, asset_id.clone())) {
                claimed.push(asset_id);
            }
        }
        drop(in_flight);
        RepresentationClaims {
            coordinator: self.clone(),
            kind,
            asset_ids: claimed,
        }
    }

    /// The one-worker execution gate every thumbnail writer takes.
    fn lock_thumbnail_execution(&self) -> std::sync::MutexGuard<'_, ()> {
        self.thumbnail_execution
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

struct RepresentationClaims {
    coordinator: Arc<RepresentationCoordinator>,
    kind: RepresentationKind,
    asset_ids: Vec<String>,
}

impl Drop for RepresentationClaims {
    fn drop(&mut self) {
        if let Ok(mut in_flight) = self.coordinator.in_flight.lock() {
            for asset_id in &self.asset_ids {
                in_flight.remove(&(self.kind, asset_id.clone()));
            }
        }
    }
}

/// The background work currently touching one project's files, one labelled
/// entry per running writer.
///
/// `delete_project` cannot see in-flight work by looking at the folder — the
/// audit inspects contents, not writers — while the writers hold `Catalog`
/// clones whose `create_dir_all` calls resurrect cache directories in the
/// middle of a `remove_dir_all`. Keyed by project id because deletion may
/// target a catalog that is not the open one; the id is the one name every
/// writer already knows.
#[derive(Default)]
struct ProjectWorkRegistry {
    active: Mutex<HashMap<String, Vec<(u64, &'static str)>>>,
    next_entry: AtomicU64,
}

impl ProjectWorkRegistry {
    /// Marks `label` work as running for `project_id` until the guard drops,
    /// so an early return or error in the writer releases it too.
    fn register(self: &Arc<Self>, project_id: &str, label: &'static str) -> ProjectWorkGuard {
        let entry = self.next_entry.fetch_add(1, Ordering::Relaxed);
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        active
            .entry(project_id.to_string())
            .or_default()
            .push((entry, label));
        drop(active);
        ProjectWorkGuard {
            registry: self.clone(),
            project_id: project_id.to_string(),
            entry,
        }
    }

    /// The labels of work still running for `project_id`, deduplicated in
    /// registration order — exactly what a refusal message shows.
    fn active_labels(&self, project_id: &str) -> Vec<&'static str> {
        let active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut labels = Vec::new();
        for (_, label) in active.get(project_id).into_iter().flatten() {
            if !labels.contains(label) {
                labels.push(*label);
            }
        }
        labels
    }
}

/// Keeps one writer's work entry registered for exactly the writer's lifetime.
struct ProjectWorkGuard {
    registry: Arc<ProjectWorkRegistry>,
    project_id: String,
    entry: u64,
}

impl Drop for ProjectWorkGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = self.registry.active.lock()
            && let Some(entries) = active.get_mut(&self.project_id)
        {
            entries.retain(|(entry, _)| *entry != self.entry);
            if entries.is_empty() {
                active.remove(&self.project_id);
            }
        }
    }
}

/// The integrity-conflict refusal (#53 rule 6) for deleting a project whose
/// background work is still running: name what is running so the user can let
/// it finish or cancel it, instead of racing `remove_dir_all` against a live
/// writer.
fn active_work_refusal(labels: &[&str]) -> Option<String> {
    if labels.is_empty() {
        return None;
    }
    Some(format!(
        "project has work in progress: {} — let it finish or cancel it first",
        labels.join(", ")
    ))
}

/// The family of long-running work a cancel token belongs to. A cancel is
/// aimed at one kind and must never observe or disturb another kind's runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum OperationKind {
    /// Preflight, copy/verification, and the optional proxy tail.
    Import,
    /// The proxies → transcripts → cloud-analysis chain.
    Analysis,
    /// The speech-model download.
    ModelInstall,
}

/// Cancel tokens for the operations currently in flight, keyed by kind.
///
/// Every run registers a token of its own instead of sharing an app-global
/// flag, which gives two invariants for free: a cancel arriving between runs
/// cannot stop the next one before it starts (the next run's token did not
/// exist when the cancel fired), and starting a run cannot erase a cancel
/// aimed at one already in flight (no run ever writes to another's token).
#[derive(Default)]
struct OperationCancels {
    registered: Mutex<HashMap<OperationKind, Vec<Weak<AtomicBool>>>>,
}

impl OperationCancels {
    /// Mints a fresh token for one run. It stays registered until the returned
    /// guard drops, so an early return or error unregisters it too.
    fn register(self: &Arc<Self>, kind: OperationKind) -> OperationCancelGuard {
        let token = Arc::new(AtomicBool::new(false));
        let mut registered = self
            .registered
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        registered
            .entry(kind)
            .or_default()
            .push(Arc::downgrade(&token));
        drop(registered);
        OperationCancelGuard {
            registry: self.clone(),
            kind,
            token,
        }
    }

    /// Sets every token currently registered under `kind`. A token registered
    /// after this call belongs to a run the cancel could not have been aimed
    /// at, so it is left untouched.
    fn cancel(&self, kind: OperationKind) {
        let mut registered = self
            .registered
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(tokens) = registered.get_mut(&kind) {
            // The weak entries double as liveness: a guard that already
            // dropped leaves nothing to upgrade, and its slot is reclaimed
            // here rather than accumulating for the session.
            tokens.retain(|token| match token.upgrade() {
                Some(token) => {
                    token.store(true, Ordering::SeqCst);
                    true
                }
                None => false,
            });
        }
    }
}

/// Keeps one run's token registered for exactly the run's lifetime.
struct OperationCancelGuard {
    registry: Arc<OperationCancels>,
    kind: OperationKind,
    token: Arc<AtomicBool>,
}

impl OperationCancelGuard {
    /// The token a run's progress callbacks observe. Set only by a cancel
    /// aimed at this kind while the run is in flight.
    fn token(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.token)
    }
}

impl Drop for OperationCancelGuard {
    fn drop(&mut self) {
        if let Ok(mut registered) = self.registry.registered.lock()
            && let Some(tokens) = registered.get_mut(&self.kind)
        {
            let ptr = Arc::as_ptr(&self.token);
            tokens.retain(|token| token.as_ptr() != ptr);
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LoadedCatalog {
    path: String,
    projects: Vec<Project>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectLocation {
    catalog_path: String,
    folder_path: String,
    project: Project,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectLibrary {
    root_path: String,
    projects: Vec<ProjectLocation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum AutoplayMode {
    Never,
    ProxyAvailable,
    Always,
}

fn default_autoplay_mode() -> AutoplayMode {
    AutoplayMode::ProxyAvailable
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopSettings {
    projects_root: PathBuf,
    #[serde(default = "default_node_id")]
    node_id: String,
    #[serde(default = "default_autoplay_mode")]
    autoplay_mode: AutoplayMode,
    #[serde(default)]
    last_catalog_path: Option<PathBuf>,
    #[serde(default)]
    last_project_id: Option<String>,
    #[serde(default)]
    last_asset_id: Option<String>,
    #[serde(default)]
    last_context_updated_at: Option<String>,
    #[serde(default)]
    paired_server_node_id: Option<String>,
    #[serde(default)]
    paired_server_endpoint: Option<String>,
    #[serde(default)]
    paired_server_label: Option<String>,
    #[serde(default)]
    paired_at: Option<String>,
    #[serde(default)]
    paired_server_endpoints: Option<Vec<ServerEndpoint>>,
    #[serde(default = "default_true")]
    make_proxies_on_import: bool,
    #[serde(default)]
    last_import_archive_root_id: Option<String>,
    /// Stable card-session fingerprint to the logical archive chosen for it.
    #[serde(default)]
    import_archive_root_ids_by_card_session: HashMap<String, String>,
}

const fn default_true() -> bool {
    true
}

/// `DesktopSettings` plus the things the frontend needs but must never be
/// written to disk. The settings themselves are flattened so the TypeScript
/// shape stays one object.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopSettingsView {
    #[serde(flatten)]
    settings: DesktopSettings,
    /// `None` when the loopback origin could not bind. The stage then shows the
    /// offline slate instead of an element that would never paint.
    media_origin: Option<media_server::MediaOrigin>,
}

/// Resolves asset ids to media paths for the loopback origin, reading the
/// catalog and settings the app currently has open rather than a snapshot, so
/// opening another project does not strand the origin on the previous one.
struct CatalogMedia {
    catalog: Arc<RwLock<Catalog>>,
    settings: Arc<RwLock<DesktopSettings>>,
    app_data_dir: PathBuf,
}

impl media_server::MediaCatalog for CatalogMedia {
    fn locate(&self, asset_id: &str) -> Option<media_server::AssetMedia> {
        let catalog = self.catalog.read().ok()?.clone();
        let asset = catalog.get_asset(asset_id).ok()?;
        Some(media_server::AssetMedia {
            original: Some(PathBuf::from(asset.path)),
            proxy: asset.proxy_path.map(PathBuf::from),
        })
    }

    /// The roots review media is allowed to live under. This mirrors the asset
    /// protocol scope in `tauri.conf.json` — cache, home, mounted volumes, and
    /// the temporary fixture root — plus wherever the open catalog and the
    /// projects folder actually are, since either can be moved by settings or
    /// by `RCDVLOG_CATALOG`.
    fn storage_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![self.app_data_dir.clone(), std::env::temp_dir()];
        if let Some(home) = dirs::home_dir() {
            roots.push(home);
        }
        if cfg!(target_os = "macos") {
            roots.push(PathBuf::from("/Volumes"));
        }
        if let Ok(settings) = self.settings.read() {
            roots.push(settings.projects_root.clone());
        }
        if let Ok(catalog) = self.catalog.read()
            && let Some(parent) = catalog.database_path().parent()
        {
            roots.push(parent.to_path_buf());
        }
        roots
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerPairingResult {
    node_id: String,
    endpoint: String,
    label: String,
    paired_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerConnectionStatus {
    paired: bool,
    node_id: Option<String>,
    endpoint: Option<String>,
    label: Option<String>,
    paired_at: Option<String>,
    endpoints: Option<Vec<ServerEndpoint>>,
}

fn default_node_id() -> String {
    format!("node_desktop_{}", uuid::Uuid::new_v4().simple())
}

impl AppState {
    fn new(
        app_handle: tauri::AppHandle,
        catalog: Catalog,
        settings: DesktopSettings,
        settings_path: PathBuf,
        app_data_dir: PathBuf,
    ) -> anyhow::Result<Self> {
        fs::create_dir_all(&settings.projects_root).with_context(|| {
            format!("create project folder {}", settings.projects_root.display())
        })?;
        catalog.set_local_node_id(&settings.node_id)?;
        let catalog = Arc::new(RwLock::new(catalog));
        let settings = Arc::new(RwLock::new(settings));
        let sync_status = Arc::new(RwLock::new(HashMap::new()));
        let representations = Arc::new(RepresentationCoordinator::default());
        let project_work = Arc::new(ProjectWorkRegistry::default());
        let sync_manager = sync_manager::SyncManager::spawn(
            app_handle,
            catalog.clone(),
            settings.clone(),
            sync_status.clone(),
            representations.clone(),
            project_work.clone(),
        );
        let media_catalog = Arc::new(CatalogMedia {
            catalog: catalog.clone(),
            settings: settings.clone(),
            app_data_dir,
        });
        Ok(Self {
            catalog,
            settings,
            settings_path,
            baseline_attempt: Arc::new(RwLock::new(None)),
            sync_manager,
            sync_status,
            operation_cancels: Arc::new(OperationCancels::default()),
            representations,
            project_work,
            media_catalog,
            media_server: Arc::new(RwLock::new(None)),
            media_restarted: Arc::new(AtomicBool::new(false)),
        })
    }

    #[cfg(test)]
    fn new_for_test(
        catalog: Catalog,
        settings: DesktopSettings,
        settings_path: PathBuf,
    ) -> anyhow::Result<Self> {
        fs::create_dir_all(&settings.projects_root).with_context(|| {
            format!("create project folder {}", settings.projects_root.display())
        })?;
        catalog.set_local_node_id(&settings.node_id)?;
        let catalog = Arc::new(RwLock::new(catalog));
        let settings = Arc::new(RwLock::new(settings));
        let app_data_dir = settings_path
            .parent()
            .map_or_else(std::env::temp_dir, Path::to_path_buf);
        Ok(Self {
            catalog: catalog.clone(),
            settings: settings.clone(),
            settings_path,
            baseline_attempt: Arc::new(RwLock::new(None)),
            sync_manager: sync_manager::SyncManager::spawn_test(),
            sync_status: Arc::new(RwLock::new(HashMap::new())),
            operation_cancels: Arc::new(OperationCancels::default()),
            representations: Arc::new(RepresentationCoordinator::default()),
            project_work: Arc::new(ProjectWorkRegistry::default()),
            media_catalog: Arc::new(CatalogMedia {
                catalog,
                settings,
                app_data_dir,
            }),
            media_server: Arc::new(RwLock::new(None)),
            media_restarted: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Bind the loopback media origin. Called once at startup; a failure here is
    /// surfaced immediately rather than at the first playback attempt.
    fn start_media_origin(&self) -> anyhow::Result<media_server::MediaOrigin> {
        let server = media_server::MediaServer::start(self.media_catalog.clone())?;
        let origin = server.origin();
        *self
            .media_server
            .write()
            .map_err(|_| anyhow::anyhow!("media origin state is unavailable"))? = Some(server);
        Ok(origin)
    }

    fn media_origin(&self) -> Option<media_server::MediaOrigin> {
        self.media_server
            .read()
            .ok()?
            .as_ref()
            .map(media_server::MediaServer::origin)
    }

    /// Restart the origin, at most once per session. Returns the new origin, or
    /// the error that stopped it, so the frontend can report either.
    fn restart_media_origin(&self) -> anyhow::Result<media_server::MediaOrigin> {
        if self.media_restarted.swap(true, Ordering::SeqCst) {
            bail!("the media origin has already been restarted once this session");
        }
        // Dropping the old server releases its port and its token.
        *self
            .media_server
            .write()
            .map_err(|_| anyhow::anyhow!("media origin state is unavailable"))? = None;
        self.start_media_origin()
    }

    fn stop_media_origin(&self) {
        if let Ok(mut server) = self.media_server.write() {
            *server = None;
        }
    }

    fn settings_view(&self, settings: DesktopSettings) -> DesktopSettingsView {
        DesktopSettingsView {
            settings,
            media_origin: self.media_origin(),
        }
    }

    fn catalog(&self) -> Result<Catalog, String> {
        self.catalog
            .read()
            .map(|catalog| (*catalog).clone())
            .map_err(|_| "catalog state is unavailable".to_string())
    }

    fn load_catalog(&self, path: PathBuf) -> std::result::Result<LoadedCatalog, String> {
        if !path.is_file() {
            return Err(format!("catalog file not found: {}", path.display()));
        }
        let catalog = Catalog::open_existing(&path).map_err(error_message)?;
        let node_id = self
            .settings
            .read()
            .map_err(|_| "desktop settings are unavailable".to_string())?
            .node_id
            .clone();
        catalog.set_local_node_id(&node_id).map_err(error_message)?;
        let projects = catalog.list_projects().map_err(error_message)?;
        let loaded = LoadedCatalog {
            path: catalog.database_path().to_string_lossy().into_owned(),
            projects,
        };
        *self
            .catalog
            .write()
            .map_err(|_| "catalog state is unavailable".to_string())? = catalog;
        self.record_context_path(path, None, None)
            .map_err(error_message)?;
        Ok(loaded)
    }

    fn projects_root(&self) -> anyhow::Result<PathBuf> {
        self.settings
            .read()
            .map(|settings| settings.projects_root.clone())
            .map_err(|_| anyhow::anyhow!("desktop settings are unavailable"))
    }

    fn desktop_settings(&self) -> anyhow::Result<DesktopSettings> {
        self.settings
            .read()
            .map(|settings| settings.clone())
            .map_err(|_| anyhow::anyhow!("desktop settings are unavailable"))
    }

    fn project_library(&self) -> anyhow::Result<ProjectLibrary> {
        let root = self.projects_root()?;
        let mut projects = Vec::new();
        for entry in fs::read_dir(&root)
            .with_context(|| format!("scan project folder {}", root.display()))?
        {
            let entry = match entry {
                Ok(entry) if entry.file_type().is_ok_and(|kind| kind.is_dir()) => entry,
                _ => continue,
            };
            let folder = entry.path();
            let catalog_path = folder.join("catalog.sqlite3");
            if !catalog_path.is_file() {
                continue;
            }
            let catalog = match Catalog::open_existing(&catalog_path) {
                Ok(catalog) => catalog,
                Err(_) => continue,
            };
            for project in catalog.list_projects().unwrap_or_default() {
                projects.push(ProjectLocation {
                    catalog_path: catalog_path.to_string_lossy().into_owned(),
                    folder_path: folder.to_string_lossy().into_owned(),
                    project,
                });
            }
        }
        projects.sort_by(|left, right| {
            right
                .project
                .created_at
                .cmp(&left.project.created_at)
                .then_with(|| left.project.name.cmp(&right.project.name))
        });
        Ok(ProjectLibrary {
            root_path: root.to_string_lossy().into_owned(),
            projects,
        })
    }

    fn set_projects_root(&self, path: PathBuf) -> anyhow::Result<ProjectLibrary> {
        if path.as_os_str().is_empty() {
            bail!("project folder cannot be empty");
        }
        fs::create_dir_all(&path)
            .with_context(|| format!("create project folder {}", path.display()))?;
        let path = path
            .canonicalize()
            .with_context(|| format!("resolve project folder {}", path.display()))?;
        let mut settings = self
            .settings
            .write()
            .map_err(|_| anyhow::anyhow!("desktop settings are unavailable"))?;
        let updated = DesktopSettings {
            projects_root: path,
            ..settings.clone()
        };
        write_settings(&self.settings_path, &updated)?;
        *settings = updated;
        drop(settings);
        self.project_library()
    }

    fn set_autoplay_mode(&self, autoplay_mode: AutoplayMode) -> anyhow::Result<DesktopSettings> {
        let mut settings = self
            .settings
            .write()
            .map_err(|_| anyhow::anyhow!("desktop settings are unavailable"))?;
        let updated = DesktopSettings {
            autoplay_mode,
            ..settings.clone()
        };
        write_settings(&self.settings_path, &updated)?;
        *settings = updated.clone();
        Ok(updated)
    }

    fn record_context_path(
        &self,
        catalog_path: PathBuf,
        project_id: Option<String>,
        asset_id: Option<String>,
    ) -> anyhow::Result<DesktopSettings> {
        let mut settings = self
            .settings
            .write()
            .map_err(|_| anyhow::anyhow!("desktop settings are unavailable"))?;
        let updated = DesktopSettings {
            last_catalog_path: Some(catalog_path),
            last_project_id: project_id,
            last_asset_id: asset_id,
            last_context_updated_at: Some(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)),
            ..settings.clone()
        };
        write_settings(&self.settings_path, &updated)?;
        *settings = updated.clone();
        Ok(updated)
    }

    fn record_desktop_context(
        &self,
        project_id: String,
        asset_id: Option<String>,
    ) -> anyhow::Result<DesktopSettings> {
        let catalog = self.catalog().map_err(anyhow::Error::msg)?;
        if !catalog
            .list_projects()?
            .iter()
            .any(|project| project.id == project_id)
        {
            bail!("project not found: {project_id}");
        }
        if let Some(asset_id) = asset_id.as_deref() {
            let asset = catalog.get_asset(asset_id)?;
            if asset.project_id != project_id {
                bail!("asset {asset_id} does not belong to project {project_id}");
            }
        }
        self.record_context_path(
            catalog.database_path().to_path_buf(),
            Some(project_id),
            asset_id,
        )
    }

    fn create_project(&self, name: &str) -> anyhow::Result<Project> {
        let name = name.trim();
        if name.is_empty() {
            bail!("project name cannot be empty");
        }
        let root = self.projects_root()?;
        let base_name = safe_folder_name(name);
        let mut suffix = 1_u32;
        let folder = loop {
            let folder_name = if suffix == 1 {
                base_name.clone()
            } else {
                format!("{base_name} {suffix}")
            };
            let candidate = root.join(folder_name);
            match fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == ErrorKind::AlreadyExists => suffix += 1,
                Err(error) => return Err(error).context("create project directory"),
            }
        };
        let catalog = Catalog::open(folder.join("catalog.sqlite3"))?;
        catalog.set_local_node_id(&self.desktop_settings()?.node_id)?;
        let project = catalog.create_project(name)?;
        *self
            .catalog
            .write()
            .map_err(|_| anyhow::anyhow!("catalog state is unavailable"))? = catalog;
        self.record_desktop_context(project.id.clone(), None)?;
        Ok(project)
    }
}

fn safe_folder_name(name: &str) -> String {
    let name: String = name
        .trim()
        .chars()
        .map(|character| {
            if character.is_control() || matches!(character, '/' | '\\' | ':') {
                '-'
            } else {
                character
            }
        })
        .collect();
    let name = name.trim_matches([' ', '.']);
    if name.is_empty() {
        "Untitled project".to_string()
    } else {
        name.to_string()
    }
}

fn load_settings(settings_path: &Path, default_root: PathBuf) -> DesktopSettings {
    fs::read(settings_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<DesktopSettings>(&bytes).ok())
        .unwrap_or(DesktopSettings {
            projects_root: default_root,
            node_id: default_node_id(),
            autoplay_mode: default_autoplay_mode(),
            last_catalog_path: None,
            last_project_id: None,
            last_asset_id: None,
            last_context_updated_at: None,
            paired_server_node_id: None,
            paired_server_endpoint: None,
            paired_server_label: None,
            paired_at: None,
            paired_server_endpoints: None,
            make_proxies_on_import: true,
            last_import_archive_root_id: None,
            import_archive_root_ids_by_card_session: HashMap::new(),
        })
}

fn write_settings(settings_path: &Path, settings: &DesktopSettings) -> anyhow::Result<()> {
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(settings)?;
    let temporary_path = settings_path.with_extension("json.tmp");
    fs::write(&temporary_path, bytes)
        .with_context(|| format!("save temporary settings {}", temporary_path.display()))?;
    fs::rename(&temporary_path, settings_path)
        .with_context(|| format!("commit settings {}", settings_path.display()))
}

pub(crate) fn error_message(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[tauri::command]
fn prerequisite_status() -> Vec<ToolStatus> {
    rcdvlog_core::prerequisite_status()
}

#[tauri::command]
fn load_catalog(path: String, state: State<'_, AppState>) -> Result<LoadedCatalog, String> {
    state.load_catalog(PathBuf::from(path))
}

#[tauri::command]
fn project_library(state: State<'_, AppState>) -> std::result::Result<ProjectLibrary, String> {
    state.project_library().map_err(error_message)
}

#[tauri::command]
fn set_projects_root(
    path: String,
    state: State<'_, AppState>,
) -> std::result::Result<ProjectLibrary, String> {
    state
        .set_projects_root(PathBuf::from(path))
        .map_err(error_message)
}

#[tauri::command]
fn desktop_settings(
    state: State<'_, AppState>,
) -> std::result::Result<DesktopSettingsView, String> {
    state
        .desktop_settings()
        .map(|settings| state.settings_view(settings))
        .map_err(error_message)
}

#[tauri::command]
fn set_autoplay_mode(
    autoplay_mode: AutoplayMode,
    state: State<'_, AppState>,
) -> std::result::Result<DesktopSettingsView, String> {
    state
        .set_autoplay_mode(autoplay_mode)
        .map(|settings| state.settings_view(settings))
        .map_err(error_message)
}

#[tauri::command]
fn record_desktop_context(
    project_id: String,
    asset_id: Option<String>,
    state: State<'_, AppState>,
) -> std::result::Result<DesktopSettingsView, String> {
    state
        .record_desktop_context(project_id, asset_id)
        .map(|settings| state.settings_view(settings))
        .map_err(error_message)
}

/// Health-check the loopback media origin and restart it once if it is gone.
///
/// The frontend calls this when a media element reports an error it cannot
/// explain, which is the only cheap signal that the origin died mid-session.
/// A healthy origin is returned unchanged, so a genuine decode failure does not
/// invalidate every URL on screen.
#[tauri::command]
async fn recover_media_origin(
    state: State<'_, AppState>,
) -> std::result::Result<Option<media_server::MediaOrigin>, String> {
    let Some(origin) = state.media_origin() else {
        return state
            .start_media_origin()
            .map(Some)
            .map_err(|error| format!("media origin is unavailable: {error}"));
    };
    if media_origin_healthy(&origin).await {
        return Ok(Some(origin));
    }
    state
        .restart_media_origin()
        .map(Some)
        .map_err(|error| format!("media origin could not be restarted: {error}"))
}

async fn media_origin_healthy(origin: &media_server::MediaOrigin) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    client
        .get(format!("{}/healthz", origin.base_url))
        .query(&[("t", origin.token.as_str())])
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

/// What one project costs on this machine, category by category (#16).
///
/// Answered against whichever catalog holds it, not only the open one, so the
/// project library can show usage for a project the user has not opened.
#[tauri::command]
fn project_storage_report(
    catalog_path: String,
    project_id: String,
) -> Result<ProjectStorageReport, String> {
    Catalog::open_existing(Path::new(&catalog_path))
        .map_err(error_message)?
        .project_storage_report(&project_id)
        .map_err(error_message)
}

/// Deletes regenerable local data. Never opens the catalog for writing and
/// never touches media — see `rcdvlog_core::lifecycle`.
#[tauri::command]
async fn clear_project_cache(
    catalog_path: String,
    project_id: String,
    kinds: Vec<CacheKind>,
) -> Result<CacheCleanupReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        Catalog::open_existing(Path::new(&catalog_path))?.clear_project_cache(
            &project_id,
            &kinds,
            |_, _| true,
        )
    })
    .await
    .map_err(error_message)?
    .map_err(error_message)
}

/// Everything the user needs to see before a project is deleted, including
/// every reason its folder must survive.
#[tauri::command]
fn audit_project_removal(
    catalog_path: String,
    folder_path: String,
    project_id: String,
) -> Result<ProjectRemovalAudit, String> {
    Catalog::open_existing(Path::new(&catalog_path))
        .map_err(error_message)?
        .audit_project_removal(&project_id, Path::new(&folder_path))
        .map_err(error_message)
}

/// The destructive half: removes the project's own state and caches, and then
/// the folder itself only when the audit — re-run here, against the state as
/// it is now rather than as it was when the dialog opened — says it may go.
///
/// Originals and archive copies are never removed. The audit refuses a folder
/// holding either, and this command refuses to act on an audit it did not
/// just perform itself.
#[tauri::command]
async fn delete_project(
    catalog_path: String,
    folder_path: String,
    project_id: String,
    state: State<'_, AppState>,
) -> Result<ProjectRemovalAudit, String> {
    // Checked before anything is removed. The audit inspects the folder's
    // contents, not running work, and a live writer's `create_dir_all` would
    // resurrect cache directories in the middle of `remove_dir_all` — so
    // running work is refused here, out of the registry every writer reports
    // to, with the labels the user needs to know what to wait for or cancel.
    if let Some(refusal) = active_work_refusal(&state.project_work.active_labels(&project_id)) {
        return Err(refusal);
    }
    tauri::async_runtime::spawn_blocking(move || {
        perform_project_delete(
            Path::new(&catalog_path),
            Path::new(&folder_path),
            &project_id,
        )
    })
    .await
    .map_err(error_message)?
    .map_err(error_message)
}

/// The blocking half of `delete_project`: re-audit, remove the project's
/// catalog state, and remove the folder only when the fresh audit allows it.
fn perform_project_delete(
    catalog_path: &Path,
    folder_path: &Path,
    project_id: &str,
) -> anyhow::Result<ProjectRemovalAudit> {
    let catalog = Catalog::open_existing(catalog_path)?;
    let audit = catalog.audit_project_removal(project_id, folder_path)?;
    catalog.delete_project(project_id)?;
    // The catalog handle is dropped before the folder goes, so the file is
    // closed first. A failure below leaves an empty project folder, which is
    // untidy and harmless — the project itself is already gone.
    drop(catalog);
    if audit.folder_removable {
        fs::remove_dir_all(folder_path)?;
    }
    Ok(audit)
}

#[tauri::command]
fn list_projects(state: State<'_, AppState>) -> Result<Vec<Project>, String> {
    state.catalog()?.list_projects().map_err(error_message)
}

#[tauri::command]
fn create_project(
    name: String,
    state: State<'_, AppState>,
) -> std::result::Result<Project, String> {
    state.create_project(&name).map_err(error_message)
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum DesktopImportStage {
    Scanning,
    Copying,
    Reading,
    Proxies,
    Complete,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopImportProgress {
    project_id: String,
    stage: DesktopImportStage,
    completed: usize,
    total: usize,
    current_file: String,
    completed_bytes: u64,
    total_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    asset: Option<Asset>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportRunReport {
    outcome: ImportOutcome,
    proxies: Option<ProxyReport>,
    files: Vec<ImportFileMetadata>,
    session: ImportSessionDetail,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportFileMetadata {
    path: String,
    timestamp: Option<String>,
    duration_seconds: Option<f64>,
}

#[tauri::command]
async fn inspect_import_source(path: String) -> Result<Option<CardCandidate>, String> {
    tauri::async_runtime::spawn_blocking(move || inspect_card(Path::new(&path)))
        .await
        .map_err(error_message)?
        .map_err(error_message)
}

#[tauri::command]
async fn detect_mounted_cards() -> Result<Vec<CardCandidate>, String> {
    tauri::async_runtime::spawn_blocking(move || detect_cards(&MountedVolumes))
        .await
        .map_err(error_message)
}

#[tauri::command]
fn list_archive_roots(state: State<'_, AppState>) -> Result<Vec<ArchiveRoot>, String> {
    state.catalog()?.list_archive_roots().map_err(error_message)
}

/// What a look at a paused import's destination finds right now (#79).
///
/// Deliberately cheap and deliberately repeatable: a paused import asks this on
/// a timer while it waits, and the answer must arrive whether the volume is
/// back, gone, or mounted and refusing to answer.
#[tauri::command]
async fn probe_archive_root(
    archive_root_id: String,
    state: State<'_, AppState>,
) -> Result<EndpointProbe, String> {
    let catalog = state.catalog()?;
    tauri::async_runtime::spawn_blocking(move || catalog.probe_archive_root(&archive_root_id))
        .await
        .map_err(error_message)?
        .map_err(error_message)
}

#[tauri::command]
fn create_archive_root(
    display_name: String,
    path: String,
    state: State<'_, AppState>,
) -> Result<ArchiveRoot, String> {
    state
        .catalog()?
        .create_archive_root(&display_name, Path::new(&path))
        .map_err(error_message)
}

#[tauri::command]
fn bind_archive_root(
    archive_root_id: String,
    path: String,
    state: State<'_, AppState>,
) -> Result<ArchiveRoot, String> {
    state
        .catalog()?
        .bind_archive_root(&archive_root_id, Path::new(&path))
        .map_err(error_message)
}

/// Every camera identity this project already has footage under — the pick
/// list for "choose an existing project camera" in the import dialog's
/// camera control.
#[tauri::command]
fn known_project_cameras(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    state
        .catalog()?
        .known_project_cameras(&project_id)
        .map_err(error_message)
}

/// The durable per-project camera-identity mappings (#71), for the inspect
/// half of "inspect/change/clear stored camera mappings".
#[tauri::command]
fn list_camera_mappings(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<CameraMapping>, String> {
    state
        .catalog()?
        .list_camera_mappings(&project_id)
        .map_err(error_message)
}

/// The "change" half: repoints a stored mapping at a different camera.
#[tauri::command]
fn rename_camera_mapping(
    project_id: String,
    mapping_id: String,
    camera: String,
    state: State<'_, AppState>,
) -> Result<CameraMapping, String> {
    state
        .catalog()?
        .rename_camera_mapping(&project_id, &mapping_id, &camera)
        .map_err(error_message)
}

/// The "clear" half: forgets a stored mapping. A future matching card goes
/// back to plain detection until a new choice is made.
#[tauri::command]
fn clear_camera_mapping(
    project_id: String,
    mapping_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .catalog()?
        .clear_camera_mapping(&project_id, &mapping_id)
        .map_err(error_message)
}

/// The durable "start here" boundary for one card session (#66). Setting and
/// moving are the same call: there is one boundary per card, and pointing at
/// another clip replaces it.
#[tauri::command]
fn set_import_cutoff(
    project_id: String,
    card_session_id: String,
    card_label: String,
    relative_path: String,
    filename: String,
    captured_at: Option<String>,
    state: State<'_, AppState>,
) -> Result<ImportCutoff, String> {
    state
        .catalog()?
        .set_import_cutoff(
            &project_id,
            &card_session_id,
            &card_label,
            &relative_path,
            &filename,
            captured_at.as_deref(),
        )
        .map_err(error_message)
}

/// Forgets a boundary, so the whole card is offered again.
#[tauri::command]
fn clear_import_cutoff(
    project_id: String,
    card_session_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .catalog()?
        .clear_import_cutoff(&project_id, &card_session_id)
        .map_err(error_message)
}

/// Every boundary this project holds, so a stored rule stays inspectable even
/// when its card is not in the slot.
#[tauri::command]
fn list_import_cutoffs(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<ImportCutoff>, String> {
    state
        .catalog()?
        .list_import_cutoffs(&project_id)
        .map_err(error_message)
}

#[tauri::command]
fn set_import_preferences(
    make_proxies: bool,
    archive_root_id: Option<String>,
    card_session_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<DesktopSettingsView, String> {
    let mut settings = state
        .settings
        .write()
        .map_err(|_| "desktop settings are unavailable".to_string())?;
    let mut import_archive_root_ids_by_card_session =
        settings.import_archive_root_ids_by_card_session.clone();
    if let (Some(card_session_id), Some(archive_root_id)) =
        (card_session_id, archive_root_id.as_ref())
    {
        import_archive_root_ids_by_card_session.insert(card_session_id, archive_root_id.clone());
    }
    let updated = DesktopSettings {
        make_proxies_on_import: make_proxies,
        last_import_archive_root_id: archive_root_id,
        import_archive_root_ids_by_card_session,
        ..settings.clone()
    };
    write_settings(&state.settings_path, &updated).map_err(error_message)?;
    *settings = updated.clone();
    Ok(state.settings_view(updated))
}

#[tauri::command]
async fn preflight_import(
    project_id: String,
    source_path: String,
    archive_root_id: Option<String>,
    camera_override: Option<String>,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<ImportPreflight, String> {
    let catalog = state.catalog()?;
    // A token of this run's own, not a shared flag: a cancel arriving between
    // runs cannot stop this one, and this run starting cannot erase a cancel
    // aimed at an import already in flight.
    let cancel_guard = state.operation_cancels.register(OperationKind::Import);
    let cancel = cancel_guard.token();
    tracing::info!(
        target: "rcdvlog_desktop_lib::import",
        project_id = %project_id,
        source_path = %source_path,
        "preflight_import: begin"
    );
    tauri::async_runtime::spawn_blocking(move || {
        // A caller may offer the remembered archive root before it knows what
        // kind of source was picked. Non-card folders always remain in place.
        let is_card = inspect_card(Path::new(&source_path))?.is_some();
        let archive_root_id = is_card.then_some(archive_root_id).flatten();
        catalog.preflight_import(
            &project_id,
            &source_path,
            archive_root_id.as_deref(),
            camera_override.as_deref(),
            |completed, total, path| {
                let _ = app.emit(
                    "import-progress",
                    DesktopImportProgress {
                        project_id: project_id.clone(),
                        stage: DesktopImportStage::Scanning,
                        completed,
                        total,
                        current_file: path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        completed_bytes: 0,
                        total_bytes: 0,
                        asset: None,
                    },
                );
                !cancel.load(Ordering::SeqCst)
            },
        )
    })
    .await
    .map_err(error_message)?
    .map_err(error_message)
}

#[tauri::command]
async fn run_smart_import(
    project_id: String,
    plan: ImportPreflight,
    verify_copies: bool,
    make_proxies: bool,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<ImportRunReport, String> {
    let catalog = state.catalog()?;
    // One token covers copy/verification and the optional proxy tail; it is
    // this run's alone, so a cancel between runs cannot stop the next one.
    let cancel_guard = state.operation_cancels.register(OperationKind::Import);
    let cancel = cancel_guard.token();
    let _work = state.project_work.register(&project_id, "import");
    tracing::info!(
        target: "rcdvlog_desktop_lib::import",
        project_id = %project_id,
        items = plan.items.len(),
        verify_copies,
        make_proxies,
        "run_smart_import: begin"
    );
    let pid = project_id.clone();
    let import_app = app.clone();
    let representations = state.representations.clone();
    let report =
        tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<ImportRunReport> {
            let outcome = catalog.run_import(
                &pid,
                &plan,
                verify_copies,
                |progress: CoreImportProgress| {
                    let stage = match progress.stage {
                        CoreImportStage::Copying => DesktopImportStage::Copying,
                        CoreImportStage::Reading => DesktopImportStage::Reading,
                    };
                    let _ = import_app.emit(
                        "import-progress",
                        DesktopImportProgress {
                            project_id: pid.clone(),
                            stage,
                            completed: progress.completed_items,
                            total: progress.total_items,
                            current_file: progress.current_file,
                            completed_bytes: progress.completed_bytes,
                            total_bytes: progress.total_bytes,
                            asset: progress.asset,
                        },
                    );
                    !cancel.load(Ordering::SeqCst)
                },
            )?;

            // A pulled card stops the run. Proxying the handful of clips
            // that did land would bury the reconnect prompt behind minutes of
            // unrelated work, and any referenced-in-place original is on the
            // volume that just went away. Resume makes them on the next pass.
            let proxies = if make_proxies
                && !outcome.cancelled
                && !outcome.source_disconnected
                && !outcome.destination_disconnected
            {
                // The tail claims the concrete assets it will write, so a
                // sync writer already producing one of these proxies keeps
                // sole ownership of its paths and the tail counts that clip
                // as skipped instead of racing it.
                let proxy_ids = catalog
                    .list_assets(&pid)?
                    .into_iter()
                    .map(|asset| asset.id)
                    .collect::<Vec<_>>();
                let total = proxy_ids.len();
                let claims = representations.claim(RepresentationKind::Proxy, proxy_ids);
                let already_in_flight = total.saturating_sub(claims.asset_ids.len());
                let mut tail = catalog.generate_proxies_for_assets_with_progress(
                    &claims.asset_ids,
                    |progress| {
                        let _ = import_app.emit(
                            "import-progress",
                            DesktopImportProgress {
                                project_id: pid.clone(),
                                stage: DesktopImportStage::Proxies,
                                completed: progress.estimate.completed_items,
                                total: progress.estimate.total_items,
                                current_file: progress
                                    .asset
                                    .as_ref()
                                    .map(|asset| asset.filename.clone())
                                    .unwrap_or_default(),
                                completed_bytes: progress.estimate.completed_work_units,
                                total_bytes: progress.estimate.total_work_units,
                                // The proxy generator commits an updated
                                // materialized asset one file at a time. Keep
                                // it on the same import event stream so a row
                                // shown during ingest is enriched immediately.
                                asset: progress.asset,
                            },
                        );
                        if cancel.load(Ordering::SeqCst) {
                            ControlFlow::Break(())
                        } else {
                            ControlFlow::Continue(())
                        }
                    },
                )?;
                tail.skipped += already_in_flight;
                Some(tail)
            } else {
                None
            };
            let _ = import_app.emit(
                "import-progress",
                DesktopImportProgress {
                    project_id: pid.clone(),
                    stage: DesktopImportStage::Complete,
                    completed: plan.scanned,
                    total: plan.scanned,
                    current_file: String::new(),
                    completed_bytes: outcome.bytes_copied,
                    total_bytes: outcome.bytes_copied,
                    asset: None,
                },
            );
            let session = catalog.record_import_session(&pid, &plan, &outcome)?;
            // Import has now run ffprobe for every successfully-added clip. Build
            // the durable ledger from catalog facts instead of making preflight
            // probe hundreds of card files merely because its disclosure exists.
            let assets = catalog.list_assets(&pid)?;
            let assets_by_id: HashMap<&str, &Asset> = assets
                .iter()
                .map(|asset| (asset.id.as_str(), asset))
                .collect();
            let assets_by_path: HashMap<&str, &Asset> = assets
                .iter()
                .map(|asset| (asset.path.as_str(), asset))
                .collect();
            let files = plan
                .items
                .iter()
                .map(|item| {
                    let expected_path = if plan.destination.copies {
                        Path::new(&plan.destination.resolved_path)
                            .join(&item.relative_path)
                            .to_string_lossy()
                            .into_owned()
                    } else {
                        item.path.clone()
                    };
                    let asset = item
                        .asset_id
                        .as_deref()
                        .and_then(|id| assets_by_id.get(id).copied())
                        .or_else(|| assets_by_path.get(expected_path.as_str()).copied());
                    ImportFileMetadata {
                        path: item.path.clone(),
                        timestamp: asset.and_then(|asset| {
                            asset
                                .best_time
                                .clone()
                                .or_else(|| asset.captured_at_raw.clone())
                        }),
                        duration_seconds: asset.map(|asset| asset.duration_seconds),
                    }
                })
                .collect();
            Ok(ImportRunReport {
                outcome,
                proxies,
                files,
                session,
            })
        })
        .await
        .map_err(error_message)?
        .map_err(error_message)?;
    state.sync_manager.wake_project(&project_id);
    let touched_asset_ids = report
        .outcome
        .imported_asset_ids
        .iter()
        .chain(&report.outcome.relinked_asset_ids)
        .cloned()
        .collect();
    schedule_background_thumbnails(
        state.catalog()?,
        state.representations.clone(),
        state
            .project_work
            .register(&project_id, "thumbnail generation"),
        touched_asset_ids,
        app,
    );
    tracing::info!(
        target: "rcdvlog_desktop_lib::import",
        imported = report.outcome.imported,
        relinked = report.outcome.relinked,
        failures = report.outcome.failures.len(),
        cancelled = report.outcome.cancelled,
        "run_smart_import: end"
    );
    Ok(report)
}

/// Posters/duration/probed capture time for a bounded batch of preflight
/// rows — the dialog asks only for the rows it is currently showing, never
/// for a whole card at once. See `rcdvlog_core::import_preview` for why this
/// is a separate operation from `preflight_import` rather than folded into
/// it: preflight itself must never run ffprobe/ffmpeg.
#[tauri::command]
async fn generate_import_preview(
    requests: Vec<ImportPreviewRequest>,
    state: State<'_, AppState>,
) -> Result<Vec<ImportPreviewItem>, String> {
    let catalog = state.catalog()?;
    tauri::async_runtime::spawn_blocking(move || catalog.generate_import_preview(&requests))
        .await
        .map_err(error_message)?
        .map_err(error_message)
}

#[tauri::command]
fn list_import_sessions(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<ImportHistoryEntry>, String> {
    state
        .catalog()?
        .list_import_sessions(&project_id)
        .map_err(error_message)
}

#[tauri::command]
fn import_session(
    project_id: String,
    session_id: String,
    state: State<'_, AppState>,
) -> Result<ImportSessionDetail, String> {
    state
        .catalog()?
        .import_session(&project_id, &session_id)
        .map_err(error_message)
}

#[tauri::command]
fn import_rollback_preview(
    project_id: String,
    session_id: String,
    state: State<'_, AppState>,
) -> Result<ImportRollbackPreview, String> {
    state
        .catalog()?
        .import_rollback_preview(&project_id, &session_id)
        .map_err(error_message)
}

#[tauri::command]
fn rollback_import_membership(
    project_id: String,
    session_id: String,
    state: State<'_, AppState>,
) -> Result<ImportRollbackPreview, String> {
    let result = state
        .catalog()?
        .rollback_import_membership(&project_id, &session_id)
        .map_err(error_message)?;
    state.sync_manager.wake_project(&project_id);
    Ok(result)
}

fn schedule_background_thumbnails(
    catalog: Catalog,
    coordinator: Arc<RepresentationCoordinator>,
    work: ProjectWorkGuard,
    asset_ids: Vec<String>,
    app: tauri::AppHandle,
) {
    schedule_background_thumbnails_with(
        catalog,
        coordinator,
        work,
        asset_ids,
        move |progress| {
            let _ = app.emit("thumbnail-progress", progress);
        },
        |task| {
            tauri::async_runtime::spawn_blocking(move || {
                let _ = task();
            });
        },
    );
}

/// Schedules the detached post-import Q1 task. The launch boundary stays
/// injectable so this production path can be tested without a native app.
fn schedule_background_thumbnails_with<F, S>(
    catalog: Catalog,
    coordinator: Arc<RepresentationCoordinator>,
    work: ProjectWorkGuard,
    asset_ids: Vec<String>,
    mut on_progress: F,
    start: S,
) where
    F: FnMut(rcdvlog_core::AssetBatchProgress) + Send + 'static,
    S: FnOnce(Box<dyn FnOnce() -> anyhow::Result<ThumbnailReport> + Send>),
{
    let claims = coordinator.claim(RepresentationKind::Thumbnail, asset_ids);
    if claims.asset_ids.is_empty() {
        return;
    }
    start(Box::new(move || {
        // The work registration travels with the claims: the project reads as
        // busy for exactly as long as this detached task can touch its files.
        let _work = work;
        let _claims = claims;
        let _execution = _claims.coordinator.lock_thumbnail_execution();
        // Import success is already final. Individual failures stay in this
        // quiet report and the explicit backfill action remains the recovery path.
        generate_detached_q1_thumbnails(&catalog, &_claims.asset_ids, &mut on_progress)
    }));
}

/// The post-import Q1 task deliberately has no bearing on the completed import
/// report. A failed clip stays eligible for the explicit thumbnail backfill.
fn generate_detached_q1_thumbnails<F>(
    catalog: &Catalog,
    asset_ids: &[String],
    on_progress: F,
) -> anyhow::Result<ThumbnailReport>
where
    F: FnMut(rcdvlog_core::AssetBatchProgress),
{
    catalog.generate_thumbnails_for_assets_with_progress(asset_ids, 1, on_progress)
}

/// Stops every import-family run currently in flight — preflight as well as
/// copy/proxy work — at its next progress callback. Runs of other kinds and
/// runs that start after this call are out of reach by construction.
#[tauri::command]
fn cancel_import(state: State<'_, AppState>) {
    state.operation_cancels.cancel(OperationKind::Import);
}

#[tauri::command]
async fn ingest_folder(
    project_id: String,
    path: String,
    state: State<'_, AppState>,
) -> Result<IngestReport, String> {
    let catalog = state.catalog()?;
    let pid = project_id.clone();
    let report = tauri::async_runtime::spawn_blocking(move || catalog.ingest_folder(&pid, path))
        .await
        .map_err(error_message)?
        .map_err(error_message)?;
    state.sync_manager.wake_project(&project_id);
    Ok(report)
}

/// Points the app at a folder and asks it to reconnect originals the project has
/// lost track of. Never imports: content this project does not know is counted
/// and left alone.
#[tauri::command]
async fn relink_folder(
    project_id: String,
    path: String,
    state: State<'_, AppState>,
) -> Result<RelinkReport, String> {
    let catalog = state.catalog()?;
    let pid = project_id.clone();
    let report = tauri::async_runtime::spawn_blocking(move || {
        catalog.relink_folder(&pid, path, |_, _, _| {})
    })
    .await
    .map_err(error_message)?
    .map_err(error_message)?;
    state.sync_manager.wake_project(&project_id);
    Ok(report)
}

#[tauri::command]
fn list_assets(project_id: String, state: State<'_, AppState>) -> Result<Vec<Asset>, String> {
    state
        .catalog()?
        .list_assets(&project_id)
        .map_err(error_message)
}

#[tauri::command]
async fn refresh_asset_orientation(
    asset_id: String,
    state: State<'_, AppState>,
) -> Result<Asset, String> {
    let catalog = state.catalog()?;
    tauri::async_runtime::spawn_blocking(move || catalog.refresh_asset_orientation(&asset_id))
        .await
        .map_err(error_message)?
        .map_err(error_message)
}

/// One clip, read again.
///
/// The recovery path for a write the catalog refused as stale (issue #49):
/// the client needs the current value and the current revision before it can
/// decide what its edit still meant.
#[tauri::command]
fn get_asset(asset_id: String, state: State<'_, AppState>) -> Result<Asset, String> {
    state.catalog()?.get_asset(&asset_id).map_err(error_message)
}

#[tauri::command]
fn update_asset(
    asset_id: String,
    patch: AssetPatch,
    state: State<'_, AppState>,
) -> Result<Asset, String> {
    state
        .catalog()?
        .update_asset(&asset_id, &patch)
        .map_err(error_message)
}

#[tauri::command]
fn create_note_revision(
    asset_id: String,
    text: String,
    state: State<'_, AppState>,
) -> Result<Asset, String> {
    let asset = state
        .catalog()?
        .create_note_revision(&asset_id, &text)
        .map_err(error_message)?;
    state.sync_manager.wake_project(&asset.project_id);
    Ok(asset)
}

#[tauri::command]
fn add_label(asset_id: String, label: String, state: State<'_, AppState>) -> Result<Asset, String> {
    let asset = state
        .catalog()?
        .add_label(&asset_id, &label)
        .map_err(error_message)?;
    state.sync_manager.wake_project(&asset.project_id);
    Ok(asset)
}

#[tauri::command]
fn remove_label(
    asset_id: String,
    label: String,
    state: State<'_, AppState>,
) -> Result<Asset, String> {
    let asset = state
        .catalog()?
        .remove_label(&asset_id, &label)
        .map_err(error_message)?;
    state.sync_manager.wake_project(&asset.project_id);
    Ok(asset)
}

#[tauri::command]
fn set_removed(
    asset_id: String,
    removed: bool,
    state: State<'_, AppState>,
) -> Result<Asset, String> {
    let asset = state
        .catalog()?
        .set_removed(&asset_id, removed)
        .map_err(error_message)?;
    state.sync_manager.wake_project(&asset.project_id);
    Ok(asset)
}

#[tauri::command]
fn set_rotation(
    asset_id: String,
    rotation_degrees: u16,
    state: State<'_, AppState>,
) -> Result<Asset, String> {
    let asset = state
        .catalog()?
        .set_rotation(&asset_id, rotation_degrees)
        .map_err(error_message)?;
    state.sync_manager.wake_project(&asset.project_id);
    Ok(asset)
}

#[tauri::command]
fn apply_clock_event(
    project_id: String,
    camera: String,
    start_filename: String,
    offset_seconds: i64,
    label: String,
    state: State<'_, AppState>,
) -> Result<ClockEvent, String> {
    let event = state
        .catalog()?
        .apply_clock_event(
            &project_id,
            &camera,
            &start_filename,
            offset_seconds,
            &label,
        )
        .map_err(error_message)?;
    state.sync_manager.wake_project(&project_id);
    Ok(event)
}

#[tauri::command]
async fn generate_proxy(
    asset_id: String,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<Asset, String> {
    let catalog = state.catalog()?;
    let asset = catalog.get_asset(&asset_id).map_err(error_message)?;
    let _work = state
        .project_work
        .register(&asset.project_id, "proxy generation");
    let claims = state
        .representations
        .claim(RepresentationKind::Proxy, std::iter::once(asset_id.clone()));
    if claims.asset_ids.is_empty() {
        // Another writer owns this proxy slot right now; hand back the
        // current row rather than queueing a duplicate transcode behind it.
        return Ok(asset);
    }
    tauri::async_runtime::spawn_blocking(move || {
        let _claims = claims;
        catalog.generate_proxy_with_progress(&asset_id, |progress| {
            let _ = app.emit("proxy-progress", progress);
        })
    })
    .await
    .map_err(error_message)?
    .map_err(error_message)
}

#[tauri::command]
async fn generate_missing_proxies(
    project_id: String,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<ProxyReport, String> {
    let catalog = state.catalog()?;
    let _work = state.project_work.register(&project_id, "proxy generation");
    let asset_ids = catalog
        .list_assets(&project_id)
        .map_err(error_message)?
        .into_iter()
        .map(|asset| asset.id)
        .collect::<Vec<_>>();
    let total = asset_ids.len();
    let claims = state
        .representations
        .claim(RepresentationKind::Proxy, asset_ids);
    let already_in_flight = total.saturating_sub(claims.asset_ids.len());
    tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<ProxyReport> {
        let _claims = claims;
        let mut report =
            catalog.generate_proxies_for_assets_with_progress(&_claims.asset_ids, |progress| {
                let _ = app.emit("proxy-progress", progress);
                ControlFlow::Continue(())
            })?;
        report.skipped += already_in_flight;
        Ok(report)
    })
    .await
    .map_err(error_message)?
    .map_err(error_message)
}

#[tauri::command]
async fn generate_thumbnail(
    asset_id: String,
    quality: u8,
    state: State<'_, AppState>,
) -> Result<Asset, String> {
    let catalog = state.catalog()?;
    let asset = catalog.get_asset(&asset_id).map_err(error_message)?;
    let _work = state
        .project_work
        .register(&asset.project_id, "thumbnail generation");
    let claims = state.representations.claim(
        RepresentationKind::Thumbnail,
        std::iter::once(asset_id.clone()),
    );
    if claims.asset_ids.is_empty() {
        return Ok(asset);
    }
    tauri::async_runtime::spawn_blocking(move || {
        let _claims = claims;
        let _execution = _claims.coordinator.lock_thumbnail_execution();
        catalog.generate_thumbnail(&asset_id, quality)
    })
    .await
    .map_err(error_message)?
    .map_err(error_message)
}

#[tauri::command]
async fn generate_missing_thumbnails(
    project_id: String,
    quality: u8,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<ThumbnailReport, String> {
    let catalog = state.catalog()?;
    let _work = state
        .project_work
        .register(&project_id, "thumbnail generation");
    let asset_ids = catalog
        .list_assets(&project_id)
        .map_err(error_message)?
        .into_iter()
        .map(|asset| asset.id)
        .collect::<Vec<_>>();
    let total = asset_ids.len();
    let claims = state
        .representations
        .claim(RepresentationKind::Thumbnail, asset_ids);
    let already_in_flight = total.saturating_sub(claims.asset_ids.len());
    tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<ThumbnailReport> {
        let _claims = claims;
        let _execution = _claims.coordinator.lock_thumbnail_execution();
        let mut report = catalog.generate_thumbnails_for_assets_with_progress(
            &_claims.asset_ids,
            quality,
            |progress| {
                let _ = app.emit("thumbnail-progress", progress);
            },
        )?;
        report.skipped += already_in_flight;
        Ok(report)
    })
    .await
    .map_err(error_message)?
    .map_err(error_message)
}

// ── Clip analysis ────────────────────────────────────────────────────

/// Which stage of the chain a progress event belongs to.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum AnalysisStage {
    Proxies,
    Transcripts,
    Analysis,
}

impl AnalysisStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Proxies => "proxies",
            Self::Transcripts => "transcripts",
            Self::Analysis => "analysis",
        }
    }
}

/// One `analysis-progress` event.
///
/// Carries the completed asset so the React cache can replace it immediately,
/// exactly as the proxy and thumbnail flows already do; a generated title
/// therefore appears while the run continues rather than after it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AnalysisProgress {
    stage: AnalysisStage,
    stage_index: usize,
    stage_count: usize,
    asset: Option<Asset>,
    estimate: BatchProgressEstimate,
    cost_usd: f64,
}

/// What one chained run did, per stage.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AnalysisRunReport {
    run_id: String,
    status: String,
    stage_reached: String,
    proxies: ProxyReport,
    transcripts: TranscribeReport,
    analysis: AnalyzeReport,
    cost_usd: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AnalysisRunRequest {
    /// Re-analyze clips whose observation is already current.
    #[serde(default)]
    force: bool,
    /// Ceiling for this run's cloud spend, in USD.
    #[serde(default)]
    budget_usd: Option<f64>,
}

#[tauri::command]
fn analysis_readiness(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<AnalysisReadiness, String> {
    let catalog = state.catalog()?;
    let mut readiness = catalog
        .analysis_readiness(&project_id)
        .map_err(error_message)?;
    // The core resolves a key from the environment or key file. The desktop
    // also accepts one from the Keychain, so readiness must reflect that.
    readiness.api_key_present =
        readiness.api_key_present || matches!(keychain::get_openrouter_key(), Ok(Some(_)));
    Ok(readiness)
}

#[tauri::command]
fn asset_transcript(
    asset_id: String,
    state: State<'_, AppState>,
) -> Result<Option<ClipTranscript>, String> {
    state
        .catalog()?
        .asset_transcript(&asset_id)
        .map_err(error_message)
}

#[tauri::command]
fn asset_analysis(
    asset_id: String,
    state: State<'_, AppState>,
) -> Result<Option<AssetAnalysisView>, String> {
    state
        .catalog()?
        .asset_analysis(&asset_id)
        .map_err(error_message)
}

#[tauri::command]
fn set_clip_summary(
    asset_id: String,
    title: String,
    description: String,
    state: State<'_, AppState>,
) -> Result<Asset, String> {
    state
        .catalog()?
        .set_clip_summary(&asset_id, &title, &description)
        .map_err(error_message)
}

#[tauri::command]
fn rename_camera(
    project_id: String,
    from: String,
    to: String,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    state
        .catalog()?
        .rename_camera(&project_id, &from, &to)
        .map_err(error_message)
}

#[tauri::command]
fn preview_camera_merge(
    project_id: String,
    source_camera: String,
    destination_camera: String,
    state: State<'_, AppState>,
) -> Result<CameraMergePreview, String> {
    state
        .catalog()?
        .preview_camera_merge(&project_id, &source_camera, &destination_camera)
        .map_err(error_message)
}

#[tauri::command]
fn merge_camera(
    project_id: String,
    source_camera: String,
    destination_camera: String,
    state: State<'_, AppState>,
) -> Result<CameraMergeReport, String> {
    state
        .catalog()?
        .merge_camera(&project_id, &source_camera, &destination_camera)
        .map_err(error_message)
}

#[tauri::command]
fn project_analysis_index(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<AssetAnalysisIndexEntry>, String> {
    state
        .catalog()?
        .project_analysis_index(&project_id)
        .map_err(error_message)
}

#[tauri::command]
fn analysis_runs(
    project_id: String,
    limit: usize,
    state: State<'_, AppState>,
) -> Result<Vec<AnalysisRunRecord>, String> {
    state
        .catalog()?
        .analysis_runs(&project_id, limit)
        .map_err(error_message)
}

#[tauri::command]
fn set_openrouter_key(key: String, state: State<'_, AppState>) -> Result<bool, String> {
    let _ = state;
    let trimmed = key.trim();
    if trimmed.is_empty() {
        keychain::delete_openrouter_key().map_err(error_message)?;
        return Ok(false);
    }
    keychain::store_openrouter_key(trimmed).map_err(error_message)?;
    Ok(true)
}

/// Whether a key is stored. The key itself is never returned to the frontend.
#[tauri::command]
fn openrouter_key_present() -> Result<bool, String> {
    Ok(matches!(keychain::get_openrouter_key(), Ok(Some(_))))
}

#[tauri::command]
fn model_status(state: State<'_, AppState>) -> Result<Vec<ModelStatus>, String> {
    state.catalog()?.model_status().map_err(error_message)
}

/// Downloads any missing speech model. The only path in the app that fetches
/// one: batches fail with an actionable error instead of downloading silently.
#[tauri::command]
async fn install_analysis_models(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<Vec<ModelStatus>, String> {
    let catalog = state.catalog()?;
    // Registered under its own kind so cancelling an analysis run cannot
    // abort a download, nor a download's start un-cancel a pending analysis
    // cancel. Nothing sets this token today — the settings screen offers no
    // way to stop a download — but the observation point stays, so a stop
    // affordance needs no plumbing here.
    let cancel_guard = state
        .operation_cancels
        .register(OperationKind::ModelInstall);
    let cancel = cancel_guard.token();
    tauri::async_runtime::spawn_blocking(move || {
        catalog.install_models_with_progress(|progress| {
            let _ = app.emit("model-install-progress", progress);
            if cancel.load(Ordering::Relaxed) {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
    })
    .await
    .map_err(error_message)?
    .map_err(error_message)
}

/// Stops the running analysis chain at the next safe point. Only the chain:
/// the model download is its own operation, and the one place the frontend
/// calls this — the busy bar's Cancel during an analysis run — means the
/// chain.
#[tauri::command]
fn cancel_analysis(state: State<'_, AppState>) {
    state.operation_cancels.cancel(OperationKind::Analysis);
}

/// Runs proxies, then transcription, then cloud analysis, as one cancellable
/// chain.
///
/// Proxies come first because both later stages read the proxy and fail per
/// clip without one; on a freshly ingested project a chain that skipped them
/// would fail every clip. Each stage's own skip rule keeps the chain
/// idempotent, so cancelling and re-running resumes rather than repeating.
#[tauri::command]
async fn analyze_project(
    project_id: String,
    request: AnalysisRunRequest,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<AnalysisRunReport, String> {
    let catalog = state.catalog()?;
    // A fresh token per run, so a cancel arriving between runs cannot stop
    // the next one before it starts.
    let cancel_guard = state.operation_cancels.register(OperationKind::Analysis);
    let cancel = cancel_guard.token();
    let _work = state.project_work.register(&project_id, "clip analysis");
    let representations = state.representations.clone();
    let api_key = keychain::get_openrouter_key().map_err(error_message)?;

    tauri::async_runtime::spawn_blocking(move || {
        let run_id = catalog.begin_analysis_run(&project_id)?;
        let stage_count = 3;
        let emit = |stage: AnalysisStage, stage_index: usize, asset, estimate, cost_usd| {
            let _ = app.emit(
                "analysis-progress",
                AnalysisProgress {
                    stage,
                    stage_index,
                    stage_count,
                    asset,
                    estimate,
                    cost_usd,
                },
            );
        };
        let stop_requested = || cancel.load(Ordering::Relaxed);
        let flow = |cancelled: bool| {
            if cancelled {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };

        // The stage claims the concrete assets it will write; a clip whose
        // proxy is being produced elsewhere is counted as skipped here, and
        // the later stages fail it per clip if that proxy has not landed yet.
        let proxies = {
            let proxy_ids = catalog
                .list_assets(&project_id)?
                .into_iter()
                .map(|asset| asset.id)
                .collect::<Vec<_>>();
            let total = proxy_ids.len();
            let claims = representations.claim(RepresentationKind::Proxy, proxy_ids);
            let already_in_flight = total.saturating_sub(claims.asset_ids.len());
            let mut report = catalog.generate_proxies_for_assets_with_progress(
                &claims.asset_ids,
                |progress| {
                    emit(
                        AnalysisStage::Proxies,
                        0,
                        progress.asset,
                        progress.estimate,
                        0.0,
                    );
                    flow(stop_requested())
                },
            )?;
            report.skipped += already_in_flight;
            report
        };

        let mut stage_reached = AnalysisStage::Proxies;
        let mut transcripts = TranscribeReport {
            transcribed: 0,
            skipped: 0,
            cancelled: proxies.cancelled,
            outcomes: Vec::new(),
            failures: Vec::new(),
        };
        if !proxies.cancelled {
            stage_reached = AnalysisStage::Transcripts;
            transcripts = catalog.transcribe_missing_with_progress(&project_id, |progress| {
                let asset = progress
                    .outcome
                    .as_ref()
                    .and_then(|outcome| catalog.get_asset(&outcome.asset_id).ok());
                emit(AnalysisStage::Transcripts, 1, asset, progress.estimate, 0.0);
                flow(stop_requested())
            })?;
        }

        let mut analysis = AnalyzeReport {
            analyzed: 0,
            skipped: 0,
            rejected: 0,
            fallback_served: 0,
            force_overwrite_warnings: 0,
            cost_usd: 0.0,
            cancelled: transcripts.cancelled,
            budget_exhausted: false,
            outcomes: Vec::new(),
            failures: Vec::new(),
        };
        if !transcripts.cancelled {
            stage_reached = AnalysisStage::Analysis;
            let options = AnalysisRunOptions {
                force: request.force,
                api_key: api_key.clone(),
                budget_usd: request.budget_usd,
                worker_count: None,
            };
            let mut spent = 0.0;
            analysis = catalog.analyze_missing_with_options(&project_id, &options, |progress| {
                if let Some(outcome) = progress.outcome.as_ref() {
                    spent += outcome.cost_usd;
                }
                let asset = progress
                    .asset_id
                    .as_deref()
                    .and_then(|asset_id| catalog.get_asset(asset_id).ok());
                emit(AnalysisStage::Analysis, 2, asset, progress.estimate, spent);
                flow(stop_requested())
            })?;
        }

        let cancelled = proxies.cancelled || transcripts.cancelled || analysis.cancelled;
        let status = if cancelled { "cancelled" } else { "completed" };
        catalog.finish_analysis_run(
            &run_id,
            &AnalysisRunOutcome {
                status: status.to_string(),
                stage_reached: stage_reached.as_str().to_string(),
                proxies_generated: proxies.generated,
                clips_transcribed: transcripts.transcribed,
                clips_analyzed: analysis.analyzed,
                cost_usd: analysis.cost_usd,
                budget_exhausted: analysis.budget_exhausted,
                error: String::new(),
            },
        )?;

        anyhow::Ok(AnalysisRunReport {
            run_id,
            status: status.to_string(),
            stage_reached: stage_reached.as_str().to_string(),
            cost_usd: analysis.cost_usd,
            proxies,
            transcripts,
            analysis,
        })
    })
    .await
    .map_err(error_message)?
    .map_err(error_message)
}

#[tauri::command]
async fn pair_with_server(
    endpoint: String,
    pairing_code: String,
    label: String,
    state: State<'_, AppState>,
) -> Result<ServerPairingResult, String> {
    // 1. Verify server identity before exchanging code
    let version = server_client::ServerClient::fetch_version(&endpoint)
        .await
        .map_err(|e| format!("server identity check failed: {e}"))?;

    let endpoint = endpoint.trim_end_matches('/').to_string();

    // 2. Exchange pairing code
    let url = format!("{}/api/v1/auth/devices/exchange", endpoint);
    let body = serde_json::json!({
        "code": pairing_code,
        "label": label,
    });

    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("failed to reach server: {e}"))?;

    if !response.status().is_success() {
        let error_body = response.text().await.unwrap_or_default();
        return Err(format!("pairing failed: {error_body}"));
    }

    let result: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("invalid server response: {e}"))?;

    let token = result["token"]
        .as_str()
        .ok_or("missing token in server response")?;
    let _credential_id = result["credential_id"]
        .as_str()
        .ok_or("missing credential_id in server response")?;

    // 3. Verify the server node ID from /version matches the credential
    // The credential_id is a server-side identifier for this device, not the server's node ID.
    // We store using the server's stable node_id from /version as the account.
    let server_node_id = &version.node_id;

    // Store token in Keychain keyed by the server's stable node_id
    keychain::store_server_credential(&endpoint, server_node_id, token)
        .map_err(|e| format!("keychain error: {e}"))?;

    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let paired_at_clone = now.clone();

    // Build endpoints list (add or update the current endpoint)
    let mut endpoints: Vec<ServerEndpoint> = state
        .settings
        .read()
        .map_err(|_| "desktop settings are unavailable".to_string())?
        .paired_server_endpoints
        .clone()
        .unwrap_or_default();

    // Remove any existing entry for this URL, then add the new one
    endpoints.retain(|ep| ep.url != endpoint);
    endpoints.push(ServerEndpoint {
        url: endpoint.clone(),
        kind: "lan".to_string(), // caller can update to "tailnet" later
        label: label.clone(),
    });

    // Persist pairing metadata in settings (no secrets)
    let mut settings = state
        .settings
        .write()
        .map_err(|_| "desktop settings are unavailable".to_string())?;
    let updated = DesktopSettings {
        paired_server_node_id: Some(server_node_id.to_string()),
        paired_server_endpoint: Some(endpoint.clone()),
        paired_server_label: Some(label.clone()),
        paired_at: Some(now),
        paired_server_endpoints: Some(endpoints),
        ..settings.clone()
    };
    write_settings(&state.settings_path, &updated)
        .map_err(|e| format!("failed to save settings: {e}"))?;
    *settings = updated;

    Ok(ServerPairingResult {
        node_id: server_node_id.to_string(),
        endpoint,
        label,
        paired_at: paired_at_clone,
    })
}

#[tauri::command]
fn server_connection_status(state: State<'_, AppState>) -> Result<ServerConnectionStatus, String> {
    let settings = state
        .settings
        .read()
        .map_err(|_| "desktop settings are unavailable".to_string())?;
    Ok(ServerConnectionStatus {
        paired: settings.paired_server_node_id.is_some(),
        node_id: settings.paired_server_node_id.clone(),
        endpoint: settings.paired_server_endpoint.clone(),
        label: settings.paired_server_label.clone(),
        paired_at: settings.paired_at.clone(),
        endpoints: settings.paired_server_endpoints.clone(),
    })
}

#[tauri::command]
fn unpair_server(state: State<'_, AppState>) -> Result<(), String> {
    let settings = state
        .settings
        .read()
        .map_err(|_| "desktop settings are unavailable".to_string())?;

    if let (Some(endpoint), Some(node_id)) = (
        &settings.paired_server_endpoint,
        &settings.paired_server_node_id,
    ) {
        keychain::delete_server_credential(endpoint, node_id).ok();
    }

    let mut settings = state
        .settings
        .write()
        .map_err(|_| "desktop settings are unavailable".to_string())?;
    let updated = DesktopSettings {
        paired_server_node_id: None,
        paired_server_endpoint: None,
        paired_server_label: None,
        paired_at: None,
        paired_server_endpoints: None,
        ..settings.clone()
    };
    write_settings(&state.settings_path, &updated)
        .map_err(|e| format!("failed to save settings: {e}"))?;
    *settings = updated;
    Ok(())
}

// ── Server info ──────────────────────────────────────────────────────

#[tauri::command]
async fn fetch_server_info(endpoint: String) -> Result<server_client::ServerVersionInfo, String> {
    server_client::ServerClient::fetch_version(&endpoint)
        .await
        .map_err(|e| e.to_string())
}

// ── Path analysis / root mapping ─────────────────────────────────────

#[tauri::command]
fn analyze_project_paths(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<PathAnalysis, String> {
    let catalog = state.catalog()?;
    let assets = catalog.list_assets(&project_id).map_err(error_message)?;
    let total = assets.len();

    // Group assets by directory prefix
    let mut prefix_groups: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut absolute_count = 0_usize;

    for asset in &assets {
        let path = std::path::Path::new(&asset.path);
        if path.is_absolute() {
            absolute_count += 1;
            if let Some(parent) = path.parent() {
                let key = parent.to_string_lossy().into_owned();
                prefix_groups
                    .entry(key)
                    .or_default()
                    .push(asset.filename.clone());
            }
        }
    }

    let mut common_prefixes: Vec<PathPrefix> = prefix_groups
        .into_iter()
        .filter(|(_, filenames)| !filenames.is_empty())
        .map(|(prefix, filenames)| {
            let count = filenames.len();
            let samples: Vec<String> = filenames.into_iter().take(3).collect();
            PathPrefix {
                prefix,
                asset_count: count,
                sample_assets: samples,
            }
        })
        .collect();

    // Sort by asset count descending
    common_prefixes.sort_by_key(|b| std::cmp::Reverse(b.asset_count));

    Ok(PathAnalysis {
        common_prefixes,
        total_assets: total,
        absolute_path_count: absolute_count,
    })
}

#[tauri::command]
fn propose_root_mappings(
    path_analysis: PathAnalysis,
    _state: State<'_, AppState>,
) -> Result<Vec<RootMappingEntry>, String> {
    let mut mappings = Vec::new();

    for prefix_info in &path_analysis.common_prefixes {
        // Generate a stable storage_root_id from the path prefix
        let slug = prefix_info
            .prefix
            .chars()
            .map(|c| {
                if c.is_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>();
        let slug = slug.trim_matches('-');
        let storage_root_id = format!("root_{}", slug.chars().take(48).collect::<String>());

        mappings.push(RootMappingEntry {
            local_prefix: prefix_info.prefix.clone(),
            storage_root_id,
            kind: "new_logical_root".to_string(),
            label: format!(
                "{} ({} assets)",
                prefix_info.prefix, prefix_info.asset_count
            ),
        });
    }

    Ok(mappings)
}

// ── Baseline preview / import ────────────────────────────────────────

fn read_server_credentials(settings: &DesktopSettings) -> Result<(String, String), String> {
    let raw_endpoint = settings
        .paired_server_endpoint
        .as_deref()
        .ok_or("no server endpoint configured — pair with a server first")?;
    let endpoint = raw_endpoint.trim_end_matches('/');
    let node_id = settings
        .paired_server_node_id
        .as_deref()
        .ok_or("no server node ID configured — pair with a server first")?;
    let token = keychain::get_server_credential(endpoint, node_id)
        .map_err(|e| format!("keychain read failed: {e}"))?
        .ok_or("server credential not found in keychain — pair again")?;
    Ok((node_id.to_string(), token))
}

#[tauri::command]
async fn preview_project_baseline(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<BaselinePreviewResult, String> {
    let settings = state.desktop_settings().map_err(error_message)?;
    let (server_node_id, _token) = read_server_credentials(&settings)?;
    let endpoint = settings
        .paired_server_endpoint
        .clone()
        .ok_or("no server endpoint configured")?;

    // Export baseline from local catalog
    let catalog = state.catalog()?;
    let envelope = BaselineEnvelope::export_catalog(&catalog, &project_id)
        .map_err(|e| format!("baseline export failed: {e}"))?;
    envelope
        .validate()
        .map_err(|e| format!("baseline validation failed: {e}"))?;

    let digest = envelope.digest.clone();

    // Store the envelope for potential retry (as JSON)
    let envelope_json = serde_json::to_string(&envelope)
        .map_err(|e| format!("failed to serialize baseline: {e}"))?;
    *state
        .baseline_attempt
        .write()
        .map_err(|_| "baseline attempt state unavailable".to_string())? = Some(BaselineAttempt {
        project_id: project_id.clone(),
        digest: digest.clone(),
        envelope_json,
    });

    // Send to server for server-side preview
    let client = server_client::ServerClient::new(endpoint.clone(), server_node_id.clone())
        .map_err(|e| e.to_string())?;
    let server_preview = client
        .preview_baseline(&envelope)
        .await
        .map_err(|e| format!("server preview failed: {e}"))?;

    Ok(BaselinePreviewResult {
        digest,
        preview: server_preview,
        endpoint_used: endpoint,
        server_node_id,
    })
}

#[tauri::command]
async fn confirm_baseline_import(
    project_id: String,
    expected_digest: String,
    state: State<'_, AppState>,
) -> Result<BaselineImportResult, String> {
    let settings = state.desktop_settings().map_err(error_message)?;
    let (server_node_id, _token) = read_server_credentials(&settings)?;
    let endpoint = settings
        .paired_server_endpoint
        .clone()
        .ok_or("no server endpoint configured")?;

    // Re-export baseline to ensure it matches the previewed state
    let catalog = state.catalog()?;
    let envelope = BaselineEnvelope::export_catalog(&catalog, &project_id)
        .map_err(|e| format!("baseline export failed: {e}"))?;

    if envelope.digest != expected_digest {
        // Clear the stale attempt
        *state
            .baseline_attempt
            .write()
            .map_err(|_| "baseline attempt state unavailable".to_string())? = None;
        return Err(
            "project state changed since preview — re-run preview before confirming".to_string(),
        );
    }

    let client = server_client::ServerClient::new(endpoint.clone(), server_node_id.clone())
        .map_err(|e| e.to_string())?;
    let replica = client
        .import_baseline(&envelope)
        .await
        .map_err(|e| format!("baseline import failed: {e}"))?;

    // Persist pairing in the local project catalog
    catalog
        .pair_project_with_server(&project_id, &server_node_id)
        .map_err(|e| format!("failed to record pairing in project: {e}"))?;
    let local_node_id = catalog
        .local_node_id()
        .map_err(|e| format!("failed to read local node identity: {e}"))?
        .ok_or("local node identity is missing")?;
    let local_sequence = catalog
        .operation_cursors()
        .map_err(|e| format!("failed to read baseline cursors: {e}"))?
        .get(&local_node_id)
        .copied()
        .unwrap_or(0);
    catalog
        .record_peer_operation_acknowledgements(
            &server_node_id,
            &std::collections::BTreeMap::from([(local_node_id, local_sequence)]),
        )
        .map_err(|e| format!("failed to record baseline acknowledgement: {e}"))?;

    // Clear the baseline attempt
    *state
        .baseline_attempt
        .write()
        .map_err(|_| "baseline attempt state unavailable".to_string())? = None;

    Ok(BaselineImportResult {
        project_id: replica.project_id,
        baseline_digest: replica.baseline_digest,
        reused_existing: replica.reused_existing,
        state: replica.state,
    })
}

#[tauri::command]
fn abandon_baseline_attempt(state: State<'_, AppState>) -> Result<(), String> {
    *state
        .baseline_attempt
        .write()
        .map_err(|_| "baseline attempt state unavailable".to_string())? = None;
    Ok(())
}

// ── Project pairing status ───────────────────────────────────────────

#[tauri::command]
fn project_pairing_status(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<ProjectPairingStatus, String> {
    let catalog = state.catalog()?;
    let projects = catalog.list_projects().map_err(error_message)?;
    let project = projects
        .into_iter()
        .find(|p| p.id == project_id)
        .ok_or("project not found")?;

    Ok(ProjectPairingStatus {
        paired: project.paired_server_node_id.is_some(),
        server_node_id: project.paired_server_node_id,
        paired_at: project.paired_at,
    })
}

// ── Sync status commands ─────────────────────────────────────────────

#[tauri::command]
fn sync_status(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<sync_manager::SyncStatus, String> {
    let map = state
        .sync_status
        .read()
        .map_err(|_| "sync status unavailable".to_string())?;
    if let Some(status) = map.get(&project_id) {
        Ok(status.clone())
    } else {
        Ok(sync_manager::SyncStatus::unpaired(project_id))
    }
}

#[tauri::command]
fn sync_now(project_id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.sync_manager.sync_now(&project_id);
    Ok(())
}

#[tauri::command]
fn sync_all_status(state: State<'_, AppState>) -> Result<Vec<sync_manager::SyncStatus>, String> {
    let map = state
        .sync_status
        .read()
        .map_err(|_| "sync status unavailable".to_string())?;
    Ok(map.values().cloned().collect())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Install a stderr tracing subscriber for local development. The dev-with-logs
/// wrapper tees stderr to `./logs`, so these events land in the session file
/// alongside cargo/vite output. Never active in release unless `RCDVLOG_LOG`
/// is set; `try_init` keeps a second call (tests, re-entry) from panicking.
fn init_dev_logging() {
    let enabled = cfg!(debug_assertions) || std::env::var_os("RCDVLOG_LOG").is_some();
    if !enabled {
        return;
    }
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};
    let filter = EnvFilter::try_from_env("RCDVLOG_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,rcdvlog_core=debug,rcdvlog_desktop_lib=debug"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(true).with_writer(std::io::stderr))
        .try_init();
}

pub fn run() {
    init_dev_logging();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // A dev convenience never gets to stop the app from launching:
            // report the failure the way the media origin does and carry on.
            #[cfg(debug_assertions)]
            if let Err(error) = eval_bridge::start_if_enabled(app) {
                eprintln!("[eval-bridge] could not start: {error:#}");
            }

            let app_data = app.path().app_data_dir()?;
            let settings_path = app_data.join("settings.json");
            let default_root = dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("Movies")
                .join("VideoProjects");
            let settings = load_settings(&settings_path, default_root);
            write_settings(&settings_path, &settings)?;
            let catalog = match std::env::var_os("RCDVLOG_CATALOG") {
                Some(path) => Catalog::open_existing(PathBuf::from(path))?,
                None => Catalog::open(app_data.join("catalog.sqlite3"))?,
            };
            app.manage(AppState::new(
                app.handle().clone(),
                catalog,
                settings,
                settings_path,
                app_data.clone(),
            )?);

            // Review media is served over loopback HTTP rather than the asset
            // protocol; see `media_server`. Binding failure is reported here
            // because the alternative is media that silently never paints.
            let state = app.state::<AppState>();
            if let Err(error) = state.start_media_origin() {
                eprintln!("[media-origin] could not start: {error:#}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // D01-T015/T016: calibration surface, read and edit
            chronology_commands::chronology_overview,
            chronology_commands::chronology_solution,
            chronology_commands::explain_chronology,
            chronology_commands::preview_clock_epoch,
            chronology_commands::preview_clock_epoch_impact,
            chronology_commands::preview_clear_clock_epoch_impact,
            chronology_commands::preview_visual_placement,
            chronology_commands::set_clock_epoch,
            chronology_commands::clear_clock_epoch,
            chronology_commands::preview_capture_sequence_split,
            chronology_commands::apply_capture_sequence_split,
            chronology_commands::decline_capture_sequence_split,
            chronology_commands::restore_capture_sequence_split,
            chronology_commands::split_capture_sequence,
            chronology_commands::preview_capture_sequence_merge,
            chronology_commands::apply_capture_sequence_merge,
            chronology_commands::merge_capture_sequences,
            chronology_commands::preview_capture_sequence_reassignment,
            chronology_commands::preview_capture_sequence_assets,
            chronology_commands::reassign_capture_sequence_assets,
            chronology_commands::set_clip_time_exception,
            chronology_commands::clear_clip_time_exception,
            chronology_commands::set_trip_phase,
            chronology_commands::split_trip_phase,
            chronology_commands::remove_trip_phase,
            chronology_commands::set_event_order_constraint,
            chronology_commands::clear_event_order_constraint,
            chronology_commands::capture_queue_position,
            chronology_commands::restore_queue_position,
            // D01-T027 consumer: diagnostics
            chronology_commands::list_diagnostics,
            chronology_commands::acknowledge_diagnostic,
            chronology_commands::detect_offline_volumes,
            // D01-T023: edit plan and export
            chronology_commands::active_edit_plan,
            chronology_commands::edit_plan_revisions,
            chronology_commands::create_edit_plan_revision,
            chronology_commands::activate_edit_plan_revision,
            chronology_commands::validate_edit_plan,
            chronology_commands::render_edit_plan_preview,
            chronology_commands::export_edit_plan_otio,
            prerequisite_status,
            load_catalog,
            project_library,
            set_projects_root,
            desktop_settings,
            set_autoplay_mode,
            record_desktop_context,
            list_projects,
            project_storage_report,
            clear_project_cache,
            audit_project_removal,
            delete_project,
            create_project,
            inspect_import_source,
            probe_archive_root,
            detect_mounted_cards,
            list_archive_roots,
            create_archive_root,
            bind_archive_root,
            known_project_cameras,
            list_camera_mappings,
            rename_camera_mapping,
            clear_camera_mapping,
            set_import_cutoff,
            clear_import_cutoff,
            list_import_cutoffs,
            set_import_preferences,
            preflight_import,
            run_smart_import,
            generate_import_preview,
            cancel_import,
            list_import_sessions,
            import_session,
            import_rollback_preview,
            rollback_import_membership,
            ingest_folder,
            relink_folder,
            list_assets,
            refresh_asset_orientation,
            get_asset,
            update_asset,
            create_note_revision,
            add_label,
            remove_label,
            set_removed,
            set_rotation,
            apply_clock_event,
            generate_proxy,
            generate_missing_proxies,
            generate_thumbnail,
            generate_missing_thumbnails,
            // Clip analysis: run control, prerequisites, and per-clip reads
            analyze_project,
            cancel_analysis,
            analysis_readiness,
            analysis_runs,
            project_analysis_index,
            set_clip_summary,
            rename_camera,
            preview_camera_merge,
            merge_camera,
            asset_transcript,
            asset_analysis,
            model_status,
            install_analysis_models,
            set_openrouter_key,
            openrouter_key_present,
            pair_with_server,
            server_connection_status,
            unpair_server,
            fetch_server_info,
            analyze_project_paths,
            propose_root_mappings,
            preview_project_baseline,
            confirm_baseline_import,
            abandon_baseline_attempt,
            project_pairing_status,
            sync_status,
            sync_now,
            sync_all_status,
            recover_media_origin,
        ])
        .build(tauri::generate_context!())
        .expect("error while running RCD VlogCutter")
        .run(|app, event| {
            // Requirement 8: the origin stops with the app, and the session
            // token goes with it.
            if matches!(event, tauri::RunEvent::Exit) {
                app.state::<AppState>().stop_media_origin();
                // Retract the advertised bridge address with it, so the next
                // client reads "not running" rather than a reassigned port.
                #[cfg(debug_assertions)]
                eval_bridge::stop();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    #[test]
    fn representation_coordinator_deduplicates_overlapping_batches_until_release() {
        let coordinator = Arc::new(RepresentationCoordinator::default());
        let first = coordinator.claim(
            RepresentationKind::Thumbnail,
            ["asset_a".to_string(), "asset_b".to_string()],
        );
        assert_eq!(first.asset_ids, ["asset_a", "asset_b"]);

        let overlap = coordinator.claim(
            RepresentationKind::Thumbnail,
            ["asset_b".to_string(), "asset_c".to_string()],
        );
        assert_eq!(overlap.asset_ids, ["asset_c"]);

        // The same asset's proxy slot is a different file: owning the
        // thumbnail must not block a proxy writer, and vice versa.
        let other_kind = coordinator.claim(
            RepresentationKind::Proxy,
            ["asset_a".to_string(), "asset_b".to_string()],
        );
        assert_eq!(other_kind.asset_ids, ["asset_a", "asset_b"]);
        drop(first);

        let resumed = coordinator.claim(
            RepresentationKind::Thumbnail,
            ["asset_a".to_string(), "asset_b".to_string()],
        );
        assert_eq!(resumed.asset_ids, ["asset_a", "asset_b"]);
        drop(other_kind);
        let proxy_resumed = coordinator.claim(RepresentationKind::Proxy, ["asset_a".to_string()]);
        assert_eq!(proxy_resumed.asset_ids, ["asset_a"]);
    }

    #[test]
    fn project_work_shows_active_labels_until_every_guard_releases() {
        let registry = Arc::new(ProjectWorkRegistry::default());
        assert!(registry.active_labels("p1").is_empty());

        let import = registry.register("p1", "import");
        let thumbs = registry.register("p1", "thumbnail generation");
        let duplicate = registry.register("p1", "import");
        let elsewhere = registry.register("p2", "proxy generation");
        assert_eq!(
            registry.active_labels("p1"),
            ["import", "thumbnail generation"],
            "labels deduplicate in registration order"
        );
        assert_eq!(registry.active_labels("p2"), ["proxy generation"]);

        drop(import);
        assert_eq!(
            registry.active_labels("p1"),
            ["thumbnail generation", "import"],
            "a second registration keeps its label alive"
        );
        drop(duplicate);
        assert_eq!(registry.active_labels("p1"), ["thumbnail generation"]);
        drop(thumbs);
        assert!(registry.active_labels("p1").is_empty());
        drop(elsewhere);
        assert!(registry.active_labels("p2").is_empty());
    }

    #[test]
    fn a_delete_refusal_names_the_running_work() {
        assert_eq!(active_work_refusal(&[]), None);
        assert_eq!(
            active_work_refusal(&["proxy generation", "sync media transfer"]).as_deref(),
            Some(
                "project has work in progress: proxy generation, sync media transfer — let it finish or cancel it first"
            )
        );
    }

    #[test]
    fn delete_refuses_while_work_is_registered_and_succeeds_after_release() {
        let directory = tempdir().unwrap();
        let folder = directory.path().join("Trip");
        fs::create_dir_all(&folder).unwrap();
        let catalog_path = folder.join("catalog.sqlite3");
        let catalog = Catalog::open(&catalog_path).unwrap();
        let project = catalog.create_project("Trip").unwrap();
        drop(catalog);

        let registry = Arc::new(ProjectWorkRegistry::default());
        let work = registry.register(&project.id, "proxy generation");
        let refusal = active_work_refusal(&registry.active_labels(&project.id))
            .expect("registered work must block deletion");
        assert!(refusal.contains("proxy generation"), "{refusal}");
        assert!(folder.exists(), "a refusal removes nothing");

        drop(work);
        assert_eq!(
            active_work_refusal(&registry.active_labels(&project.id)),
            None
        );
        let audit = perform_project_delete(&catalog_path, &folder, &project.id).unwrap();
        assert!(audit.folder_removable);
        assert!(!folder.exists());
    }

    #[test]
    fn detached_q1_failure_does_not_revoke_a_completed_smart_import_and_can_be_repaired() {
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            eprintln!("ffmpeg is unavailable; skipping media integration test");
            return;
        }

        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Thumbnail recovery").unwrap();
        let source_dir = directory.path().join("source");
        fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("C0001.MP4");
        let status = Command::new("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=navy:s=64x64:r=5:d=1",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                source.to_str().unwrap(),
            ])
            .status()
            .unwrap();
        assert!(status.success());
        let backup = directory.path().join("C0001.backup.mp4");
        fs::copy(&source, &backup).unwrap();

        let plan = catalog
            .preflight_import(&project.id, &source_dir, None, None, |_, _, _| true)
            .unwrap();
        let outcome = catalog
            .run_import(&project.id, &plan, true, |_| true)
            .unwrap();
        assert_eq!(outcome.imported, 1);
        assert!(!outcome.cancelled);
        let imported_ids = outcome.imported_asset_ids.clone();
        let session = catalog
            .record_import_session(&project.id, &plan, &outcome)
            .unwrap();

        // The import has already reported success when its detached Q1 task
        // sees the original disappear and records a recoverable failure.
        fs::remove_file(&source).unwrap();
        let mut automatic_report = None;
        let registry = Arc::new(ProjectWorkRegistry::default());
        schedule_background_thumbnails_with(
            catalog.clone(),
            Arc::new(RepresentationCoordinator::default()),
            registry.register(&project.id, "thumbnail generation"),
            imported_ids.clone(),
            |_| {},
            |task| automatic_report = Some(task().unwrap()),
        );
        assert!(
            registry.active_labels(&project.id).is_empty(),
            "the finished task releases its project-work entry"
        );
        let failed = automatic_report.unwrap();
        assert_eq!(failed.generated, 0);
        assert_eq!(failed.failures.len(), 1);
        assert_eq!(
            outcome.imported, 1,
            "thumbnail work cannot revise the import outcome"
        );
        assert_eq!(
            catalog
                .import_session(&project.id, &session.entry.id)
                .unwrap()
                .entry
                .imported,
            1,
            "the completed import ledger remains available after detached failure"
        );
        assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 1);

        fs::copy(&backup, &source).unwrap();
        let repaired = catalog.generate_missing_thumbnails(&project.id, 1).unwrap();
        assert_eq!(repaired.generated, 1);
        assert!(repaired.failures.is_empty());
        assert_eq!(
            catalog
                .get_asset(&imported_ids[0])
                .unwrap()
                .thumbnail_quality,
            1
        );
    }

    fn test_settings(projects_root: PathBuf) -> DesktopSettings {
        DesktopSettings {
            projects_root,
            node_id: "node_desktop_test".to_string(),
            autoplay_mode: default_autoplay_mode(),
            last_catalog_path: None,
            last_project_id: None,
            last_asset_id: None,
            last_context_updated_at: None,
            paired_server_node_id: None,
            paired_server_endpoint: None,
            paired_server_label: None,
            paired_at: None,
            paired_server_endpoints: None,
            make_proxies_on_import: true,
            last_import_archive_root_id: None,
            import_archive_root_ids_by_card_session: HashMap::new(),
        }
    }

    #[test]
    fn analysis_progress_serializes_with_the_keys_the_frontend_mirrors() {
        let progress = AnalysisProgress {
            stage: AnalysisStage::Transcripts,
            stage_index: 1,
            stage_count: 3,
            asset: None,
            estimate: BatchProgressEstimate {
                completed_items: 2,
                total_items: 8,
                completed_work_units: 2,
                total_work_units: 8,
                worker_count: 1,
                work_units_per_worker_second: None,
                elapsed_seconds: 1.0,
                estimated_remaining_seconds: None,
            },
            cost_usd: 0.5,
        };
        let value = serde_json::to_value(&progress).unwrap();
        assert_eq!(value["stage"], "transcripts");
        assert_eq!(value["stageIndex"], 1);
        assert_eq!(value["stageCount"], 3);
        assert_eq!(value["costUsd"], 0.5);
        assert!(value.get("estimate").is_some());
        assert!(value.get("asset").is_some(), "null asset stays present");
    }

    #[test]
    fn each_stage_name_matches_the_run_record_value() {
        // The stage a run reached is persisted as this string and read back for
        // display, so the two spellings must not drift.
        assert_eq!(AnalysisStage::Proxies.as_str(), "proxies");
        assert_eq!(AnalysisStage::Transcripts.as_str(), "transcripts");
        assert_eq!(AnalysisStage::Analysis.as_str(), "analysis");
    }

    #[test]
    fn a_cancel_from_a_previous_run_does_not_stop_the_next_one() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let state = AppState::new_for_test(
            catalog,
            test_settings(directory.path().join("projects")),
            directory.path().join("settings.json"),
        )
        .unwrap();

        // A cancel with nothing in flight has no token to reach; the run that
        // starts afterwards mints its own token, which the earlier cancel
        // never saw.
        state.operation_cancels.cancel(OperationKind::Analysis);
        let run = state.operation_cancels.register(OperationKind::Analysis);
        assert!(!run.token().load(Ordering::SeqCst));
    }

    #[test]
    fn a_cancel_still_reaches_its_run_when_a_newer_run_starts() {
        // The lost-cancel race under the old shared flag: run B's start
        // cleared a cancel that run A had not yet observed. With registered
        // tokens, the cancel lands on A immediately and B stays untouched.
        let registry = Arc::new(OperationCancels::default());
        let run_a = registry.register(OperationKind::Import);
        registry.cancel(OperationKind::Import);
        let run_b = registry.register(OperationKind::Import);
        assert!(run_a.token().load(Ordering::SeqCst));
        assert!(!run_b.token().load(Ordering::SeqCst));
    }

    #[test]
    fn cancelling_one_kind_leaves_the_other_kinds_running() {
        let registry = Arc::new(OperationCancels::default());
        let import = registry.register(OperationKind::Import);
        let analysis = registry.register(OperationKind::Analysis);
        let install = registry.register(OperationKind::ModelInstall);
        registry.cancel(OperationKind::Import);
        assert!(import.token().load(Ordering::SeqCst));
        assert!(!analysis.token().load(Ordering::SeqCst));
        assert!(!install.token().load(Ordering::SeqCst));
    }

    #[test]
    fn a_finished_run_is_out_of_reach_of_a_later_cancel() {
        // The guard is what unregisters, so every exit path — success, error,
        // early return — takes the token out of the registry with it.
        let registry = Arc::new(OperationCancels::default());
        let run = registry.register(OperationKind::Analysis);
        let token = run.token();
        drop(run);
        registry.cancel(OperationKind::Analysis);
        assert!(!token.load(Ordering::SeqCst));
    }

    #[test]
    fn switches_to_an_existing_catalog_atomically() {
        let directory = tempdir().unwrap();
        let original_path = directory.path().join("original.sqlite3");
        let original = Catalog::open(&original_path).unwrap();
        let original_project = original.create_project("Original").unwrap();
        let state = AppState::new_for_test(
            original,
            test_settings(directory.path().join("projects")),
            directory.path().join("settings.json"),
        )
        .unwrap();

        let loaded_path = directory.path().join("loaded.sqlite3");
        let loaded_catalog = Catalog::open(&loaded_path).unwrap();
        let loaded_project = loaded_catalog.create_project("Loaded project").unwrap();
        let loaded = state.load_catalog(loaded_path.clone()).unwrap();
        assert_eq!(loaded.path, loaded_path.to_string_lossy());
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0].id, loaded_project.id);
        assert_eq!(state.catalog().unwrap().database_path(), loaded_path);
        let context = state.desktop_settings().unwrap();
        assert_eq!(
            context.last_catalog_path.as_deref(),
            Some(loaded_path.as_path())
        );
        assert_eq!(context.last_project_id, None);

        let missing = directory.path().join("missing.sqlite3");
        assert!(state.load_catalog(missing).is_err());
        let active_projects = state.catalog().unwrap().list_projects().unwrap();
        assert_eq!(active_projects.len(), 1);
        assert_eq!(active_projects[0].id, loaded_project.id);
        assert_ne!(active_projects[0].id, original_project.id);
    }

    #[test]
    fn creates_discovers_and_configures_one_folder_per_project() {
        let directory = tempdir().unwrap();
        let scratch = Catalog::open(directory.path().join("scratch.sqlite3")).unwrap();
        let projects_root = directory.path().join("VideoProjects");
        let settings_path = directory.path().join("settings.json");
        let state = AppState::new_for_test(
            scratch,
            test_settings(projects_root.clone()),
            settings_path.clone(),
        )
        .unwrap();

        let first = state.create_project("Hokkaido: Winter/2026").unwrap();
        let first_folder = projects_root.join("Hokkaido- Winter-2026");
        assert!(first_folder.join("catalog.sqlite3").is_file());
        assert_eq!(
            state.desktop_settings().unwrap().last_project_id,
            Some(first.id.clone())
        );
        let second = state.create_project("Hokkaido: Winter/2026").unwrap();
        assert!(
            projects_root
                .join("Hokkaido- Winter-2026 2")
                .join("catalog.sqlite3")
                .is_file()
        );

        fs::create_dir(projects_root.join("Not a project")).unwrap();
        let library = state.project_library().unwrap();
        assert_eq!(library.root_path, projects_root.to_string_lossy());
        assert_eq!(library.projects.len(), 2);
        assert!(
            library
                .projects
                .iter()
                .any(|item| item.project.id == first.id)
        );
        assert!(
            library
                .projects
                .iter()
                .any(|item| item.project.id == second.id)
        );

        let alternate_root = directory.path().join("AlternateProjects");
        let alternate = state.set_projects_root(alternate_root.clone()).unwrap();
        let alternate_root = alternate_root.canonicalize().unwrap();
        assert_eq!(alternate.root_path, alternate_root.to_string_lossy());
        assert!(alternate.projects.is_empty());
        let persisted = load_settings(&settings_path, PathBuf::from("fallback"));
        assert_eq!(persisted.projects_root, alternate_root);
        assert_eq!(persisted.autoplay_mode, AutoplayMode::ProxyAvailable);

        let updated = state.set_autoplay_mode(AutoplayMode::Always).unwrap();
        assert_eq!(updated.autoplay_mode, AutoplayMode::Always);
        assert_eq!(updated.projects_root, alternate_root);
        let reloaded = load_settings(&settings_path, PathBuf::from("fallback"));
        assert_eq!(reloaded, updated);
        let alternate_project = state.create_project("Expo").unwrap();
        assert!(alternate_root.join("Expo/catalog.sqlite3").is_file());
        assert_eq!(
            state.catalog().unwrap().list_projects().unwrap()[0].id,
            alternate_project.id
        );
    }

    #[test]
    fn old_settings_default_autoplay_to_proxy_media() {
        let directory = tempdir().unwrap();
        let settings_path = directory.path().join("settings.json");
        let projects_root = directory.path().join("projects");
        fs::write(
            &settings_path,
            serde_json::to_vec(&serde_json::json!({
                "projectsRoot": projects_root,
            }))
            .unwrap(),
        )
        .unwrap();

        let settings = load_settings(&settings_path, PathBuf::from("fallback"));
        assert_eq!(settings.projects_root, projects_root);
        assert_eq!(settings.autoplay_mode, AutoplayMode::ProxyAvailable);
        assert!(settings.import_archive_root_ids_by_card_session.is_empty());
    }

    #[test]
    fn the_media_origin_reaches_the_frontend_but_never_the_settings_file() {
        let directory = tempdir().unwrap();
        let settings_path = directory.path().join("settings.json");
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let state = AppState::new_for_test(
            catalog,
            test_settings(directory.path().join("projects")),
            settings_path.clone(),
        )
        .unwrap();

        // Before the origin binds, the frontend is told there is none rather
        // than being handed a URL that would never answer.
        let view = state.settings_view(state.desktop_settings().unwrap());
        assert_eq!(view.media_origin, None);

        let origin = state.start_media_origin().unwrap();
        assert!(origin.base_url.starts_with("http://127.0.0.1:"));
        assert!(!origin.base_url.ends_with(":0"));
        assert!(origin.token.len() >= 22);

        let view =
            serde_json::to_value(state.settings_view(state.desktop_settings().unwrap())).unwrap();
        assert_eq!(view["mediaOrigin"]["baseUrl"], origin.base_url);
        assert_eq!(view["mediaOrigin"]["token"], origin.token);
        // The flattened settings still carry every field the frontend reads.
        assert_eq!(view["nodeId"], "node_desktop_test");
        assert!(view.get("projectsRoot").is_some());

        // Requirement: the token never touches disk.
        state.set_autoplay_mode(AutoplayMode::Always).unwrap();
        let persisted = fs::read_to_string(&settings_path).unwrap();
        assert!(
            !persisted.contains(&origin.token),
            "the session token was written to settings.json"
        );
        assert!(!persisted.contains("mediaOrigin"));
    }

    #[test]
    fn the_media_origin_restarts_at_most_once_a_session() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let state = AppState::new_for_test(
            catalog,
            test_settings(directory.path().join("projects")),
            directory.path().join("settings.json"),
        )
        .unwrap();

        let first = state.start_media_origin().unwrap();
        let second = state.restart_media_origin().unwrap();
        // A new port and, more importantly, a new token: the old one is gone.
        assert_ne!(first.token, second.token);
        assert_ne!(first.base_url, second.base_url);
        assert_eq!(state.media_origin(), Some(second));

        assert!(
            state.restart_media_origin().is_err(),
            "a second restart would turn a failing listener into a retry loop"
        );

        state.stop_media_origin();
        assert_eq!(state.media_origin(), None);
    }

    #[test]
    fn storage_roots_cover_project_media_and_exclude_the_rest() {
        use media_server::MediaCatalog as _;

        let directory = tempdir().unwrap();
        let projects_root = directory.path().join("VideoProjects");
        let catalog_path = directory.path().join("elsewhere").join("catalog.sqlite3");
        fs::create_dir_all(catalog_path.parent().unwrap()).unwrap();
        let catalog = Catalog::open(&catalog_path).unwrap();
        let state = AppState::new_for_test(
            catalog,
            test_settings(projects_root.clone()),
            directory.path().join("settings.json"),
        )
        .unwrap();

        let roots = state.media_catalog.storage_roots();
        assert!(
            roots.contains(&projects_root),
            "the projects folder is a root"
        );
        assert!(
            roots.contains(&catalog_path.parent().unwrap().to_path_buf()),
            "a catalog opened outside the projects folder is still reachable"
        );
        assert!(
            !roots.iter().any(|root| root == Path::new("/")),
            "the whole filesystem is never a root"
        );
        assert!(!roots.iter().any(|root| root == Path::new("/etc")));
    }

    #[test]
    fn media_paths_come_from_the_catalog_not_the_caller() {
        use media_server::MediaCatalog as _;

        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Media").unwrap();
        let state = AppState::new_for_test(
            catalog,
            test_settings(directory.path().join("projects")),
            directory.path().join("settings.json"),
        )
        .unwrap();

        // An id that is not in the open catalog resolves to nothing, which the
        // origin reports as 404 without ever touching the filesystem.
        assert_eq!(state.media_catalog.locate("asset_not_here"), None);
        assert!(
            state
                .catalog()
                .unwrap()
                .list_assets(&project.id)
                .unwrap()
                .is_empty()
        );
    }
}
