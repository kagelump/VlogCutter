use anyhow::{Context, bail};
use chrono::{SecondsFormat, Utc};
use rcdvlog_core::{
    Asset, AssetPatch, Catalog, ClockEvent, IngestReport, Project, ProxyReport, ThumbnailReport,
    ToolStatus,
};
use rcdvlog_sync::BaselineEnvelope;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use tauri::{Emitter, Manager, State};

mod chronology_commands;
mod keychain;
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
        Ok(Self {
            catalog,
            settings,
            settings_path,
            baseline_attempt: Arc::new(RwLock::new(None)),
            sync_manager,
            sync_status,
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
        Ok(Self {
            catalog: Arc::new(RwLock::new(catalog)),
            settings: Arc::new(RwLock::new(settings)),
            settings_path,
            baseline_attempt: Arc::new(RwLock::new(None)),
            sync_manager: sync_manager::SyncManager::spawn_test(),
            sync_status: Arc::new(RwLock::new(HashMap::new())),
        })
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
fn desktop_settings(state: State<'_, AppState>) -> std::result::Result<DesktopSettings, String> {
    state.desktop_settings().map_err(error_message)
}

#[tauri::command]
fn set_autoplay_mode(
    autoplay_mode: AutoplayMode,
    state: State<'_, AppState>,
) -> std::result::Result<DesktopSettings, String> {
    state
        .set_autoplay_mode(autoplay_mode)
        .map_err(error_message)
}

#[tauri::command]
fn record_desktop_context(
    project_id: String,
    asset_id: Option<String>,
    state: State<'_, AppState>,
) -> std::result::Result<DesktopSettings, String> {
    state
        .record_desktop_context(project_id, asset_id)
        .map_err(error_message)
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
    tauri::async_runtime::spawn_blocking(move || catalog.generate_thumbnail(&asset_id, quality))
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
    tauri::async_runtime::spawn_blocking(move || {
        catalog.generate_missing_thumbnails_with_progress(&project_id, quality, |progress| {
            let _ = app.emit("thumbnail-progress", progress);
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
            )?);
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
            ingest_folder,
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running RCD VlogCutter");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
        }
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
    }
}
