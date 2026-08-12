use anyhow::{Context, bail};
use chrono::{SecondsFormat, Utc};
use rcdvlog_core::{
    AnalysisReadiness, AnalysisRunOptions, AnalysisRunOutcome, AnalysisRunRecord, AnalyzeReport,
    ArchiveRoot, Asset, AssetAnalysisIndexEntry, AssetAnalysisView, AssetPatch,
    BatchProgressEstimate, CardCandidate, Catalog, ClipTranscript, ClockEvent, ImportHistoryEntry,
    ImportOutcome, ImportPreflight, ImportProgress as CoreImportProgress, ImportRollbackPreview,
    ImportSessionDetail, ImportStage as CoreImportStage, IngestReport, ModelStatus, MountedVolumes,
    Project, ProxyReport, RelinkReport, ThumbnailReport, ToolStatus, TranscribeReport,
    detect_cards, inspect_card,
};
use rcdvlog_sync::BaselineEnvelope;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
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
    /// Set by `cancel_analysis` and observed by the running batch. Cleared when
    /// a run starts so a cancel from a previous run cannot stop the next one.
    analysis_cancel: Arc<AtomicBool>,
    /// Shared by preflight, copy/verification, and the optional proxy tail.
    import_cancel: Arc<AtomicBool>,
    /// Session-local ownership for thumbnail output paths. Import-triggered
    /// work and explicit repair batches share it so they cannot write the same
    /// temporary/output file concurrently.
    thumbnail_coordinator: Arc<ThumbnailCoordinator>,
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

#[derive(Default)]
struct ThumbnailCoordinator {
    in_flight: Mutex<HashSet<String>>,
    /// Serializes batches as well as per-asset ownership, preserving the
    /// measured one-worker policy across overlapping command invocations.
    execution: Mutex<()>,
}

impl ThumbnailCoordinator {
    fn claim(self: &Arc<Self>, asset_ids: impl IntoIterator<Item = String>) -> ThumbnailClaims {
        let mut in_flight = self
            .in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut claimed = Vec::new();
        for asset_id in asset_ids {
            if in_flight.insert(asset_id.clone()) {
                claimed.push(asset_id);
            }
        }
        drop(in_flight);
        ThumbnailClaims {
            coordinator: self.clone(),
            asset_ids: claimed,
        }
    }
}

struct ThumbnailClaims {
    coordinator: Arc<ThumbnailCoordinator>,
    asset_ids: Vec<String>,
}

impl Drop for ThumbnailClaims {
    fn drop(&mut self) {
        if let Ok(mut in_flight) = self.coordinator.in_flight.lock() {
            for asset_id in &self.asset_ids {
                in_flight.remove(asset_id);
            }
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
        let sync_manager = sync_manager::SyncManager::spawn(
            app_handle,
            catalog.clone(),
            settings.clone(),
            sync_status.clone(),
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
            analysis_cancel: Arc::new(AtomicBool::new(false)),
            import_cancel: Arc::new(AtomicBool::new(false)),
            thumbnail_coordinator: Arc::new(ThumbnailCoordinator::default()),
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
            analysis_cancel: Arc::new(AtomicBool::new(false)),
            import_cancel: Arc::new(AtomicBool::new(false)),
            thumbnail_coordinator: Arc::new(ThumbnailCoordinator::default()),
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
    stage: DesktopImportStage,
    completed: usize,
    total: usize,
    current_file: String,
    completed_bytes: u64,
    total_bytes: u64,
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
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<ImportPreflight, String> {
    let catalog = state.catalog()?;
    let cancel = state.import_cancel.clone();
    cancel.store(false, Ordering::SeqCst);
    tauri::async_runtime::spawn_blocking(move || {
        // A caller may offer the remembered archive root before it knows what
        // kind of source was picked. Non-card folders always remain in place.
        let is_card = inspect_card(Path::new(&source_path))?.is_some();
        let archive_root_id = is_card.then_some(archive_root_id).flatten();
        catalog.preflight_import(
            &project_id,
            &source_path,
            archive_root_id.as_deref(),
            |completed, total, path| {
                let _ = app.emit(
                    "import-progress",
                    DesktopImportProgress {
                        stage: DesktopImportStage::Scanning,
                        completed,
                        total,
                        current_file: path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        completed_bytes: 0,
                        total_bytes: 0,
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
    let cancel = state.import_cancel.clone();
    cancel.store(false, Ordering::SeqCst);
    let pid = project_id.clone();
    let import_app = app.clone();
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
                            stage,
                            completed: progress.completed_items,
                            total: progress.total_items,
                            current_file: progress.current_file,
                            completed_bytes: progress.completed_bytes,
                            total_bytes: progress.total_bytes,
                        },
                    );
                    !cancel.load(Ordering::SeqCst)
                },
            )?;

            let proxies = if make_proxies && !outcome.cancelled {
                Some(
                    catalog.generate_missing_proxies_with_progress(&pid, |progress| {
                        let _ = import_app.emit(
                            "import-progress",
                            DesktopImportProgress {
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
                            },
                        );
                        if cancel.load(Ordering::SeqCst) {
                            ControlFlow::Break(())
                        } else {
                            ControlFlow::Continue(())
                        }
                    })?,
                )
            } else {
                None
            };
            let _ = import_app.emit(
                "import-progress",
                DesktopImportProgress {
                    stage: DesktopImportStage::Complete,
                    completed: plan.scanned,
                    total: plan.scanned,
                    current_file: String::new(),
                    completed_bytes: outcome.bytes_copied,
                    total_bytes: outcome.bytes_copied,
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
        state.thumbnail_coordinator.clone(),
        touched_asset_ids,
        app,
    );
    Ok(report)
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
    coordinator: Arc<ThumbnailCoordinator>,
    asset_ids: Vec<String>,
    app: tauri::AppHandle,
) {
    schedule_background_thumbnails_with(
        catalog,
        coordinator,
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
    coordinator: Arc<ThumbnailCoordinator>,
    asset_ids: Vec<String>,
    mut on_progress: F,
    start: S,
) where
    F: FnMut(rcdvlog_core::AssetBatchProgress) + Send + 'static,
    S: FnOnce(Box<dyn FnOnce() -> anyhow::Result<ThumbnailReport> + Send>),
{
    let claims = coordinator.claim(asset_ids);
    if claims.asset_ids.is_empty() {
        return;
    }
    start(Box::new(move || {
        let _claims = claims;
        let _execution = _claims
            .coordinator
            .execution
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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

#[tauri::command]
fn cancel_import(state: State<'_, AppState>) {
    state.import_cancel.store(true, Ordering::SeqCst);
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
    tauri::async_runtime::spawn_blocking(move || {
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
    tauri::async_runtime::spawn_blocking(move || {
        catalog.generate_missing_proxies_with_progress(&project_id, |progress| {
            let _ = app.emit("proxy-progress", progress);
            ControlFlow::Continue(())
        })
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
    let claims = state
        .thumbnail_coordinator
        .claim(std::iter::once(asset_id.clone()));
    if claims.asset_ids.is_empty() {
        return catalog.get_asset(&asset_id).map_err(error_message);
    }
    tauri::async_runtime::spawn_blocking(move || {
        let _claims = claims;
        let _execution = _claims
            .coordinator
            .execution
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
    let asset_ids = catalog
        .list_assets(&project_id)
        .map_err(error_message)?
        .into_iter()
        .map(|asset| asset.id)
        .collect::<Vec<_>>();
    let total = asset_ids.len();
    let claims = state.thumbnail_coordinator.claim(asset_ids);
    let already_in_flight = total.saturating_sub(claims.asset_ids.len());
    tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<ThumbnailReport> {
        let _claims = claims;
        let _execution = _claims
            .coordinator
            .execution
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
    let cancel = Arc::clone(&state.analysis_cancel);
    cancel.store(false, Ordering::Relaxed);
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

/// Stops the running analysis chain or model download at the next safe point.
#[tauri::command]
fn cancel_analysis(state: State<'_, AppState>) {
    state.analysis_cancel.store(true, Ordering::Relaxed);
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
    let cancel = Arc::clone(&state.analysis_cancel);
    // Cleared here rather than in `cancel_analysis` so a cancel arriving
    // between runs cannot stop the next one before it starts.
    cancel.store(false, Ordering::Relaxed);
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

        let proxies = catalog.generate_missing_proxies_with_progress(&project_id, |progress| {
            emit(
                AnalysisStage::Proxies,
                0,
                progress.asset,
                progress.estimate,
                0.0,
            );
            flow(stop_requested())
        })?;

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
pub fn run() {
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
            chronology_commands::set_clock_epoch,
            chronology_commands::clear_clock_epoch,
            chronology_commands::split_capture_sequence,
            chronology_commands::merge_capture_sequences,
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
            create_project,
            inspect_import_source,
            detect_mounted_cards,
            list_archive_roots,
            create_archive_root,
            bind_archive_root,
            set_import_preferences,
            preflight_import,
            run_smart_import,
            cancel_import,
            list_import_sessions,
            import_session,
            import_rollback_preview,
            rollback_import_membership,
            ingest_folder,
            relink_folder,
            list_assets,
            refresh_asset_orientation,
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
    fn thumbnail_coordinator_deduplicates_overlapping_batches_until_release() {
        let coordinator = Arc::new(ThumbnailCoordinator::default());
        let first = coordinator.claim(["asset_a".to_string(), "asset_b".to_string()]);
        assert_eq!(first.asset_ids, ["asset_a", "asset_b"]);

        let overlap = coordinator.claim(["asset_b".to_string(), "asset_c".to_string()]);
        assert_eq!(overlap.asset_ids, ["asset_c"]);
        drop(first);

        let resumed = coordinator.claim(["asset_a".to_string(), "asset_b".to_string()]);
        assert_eq!(resumed.asset_ids, ["asset_a", "asset_b"]);
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
            .preflight_import(&project.id, &source_dir, None, |_, _, _| true)
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
        schedule_background_thumbnails_with(
            catalog.clone(),
            Arc::new(ThumbnailCoordinator::default()),
            imported_ids.clone(),
            |_| {},
            |task| automatic_report = Some(task().unwrap()),
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

        assert!(!state.analysis_cancel.load(Ordering::Relaxed));
        state.analysis_cancel.store(true, Ordering::Relaxed);

        // `analyze_project` clears the flag as it starts rather than when a
        // cancel is handled; this models that entry point.
        state.analysis_cancel.store(false, Ordering::Relaxed);
        assert!(!state.analysis_cancel.load(Ordering::Relaxed));
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
