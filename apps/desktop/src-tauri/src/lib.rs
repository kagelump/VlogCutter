use anyhow::{Context, bail};
use chrono::{SecondsFormat, Utc};
use rcdvlog_core::{
    Asset, AssetPatch, Catalog, ClockEvent, IngestReport, Project, ProxyReport, ThumbnailReport,
    ToolStatus,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use tauri::{Emitter, Manager, State};

struct AppState {
    catalog: RwLock<Catalog>,
    settings: RwLock<DesktopSettings>,
    settings_path: PathBuf,
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
}

impl AppState {
    fn new(
        catalog: Catalog,
        settings: DesktopSettings,
        settings_path: PathBuf,
    ) -> anyhow::Result<Self> {
        fs::create_dir_all(&settings.projects_root).with_context(|| {
            format!("create project folder {}", settings.projects_root.display())
        })?;
        Ok(Self {
            catalog: RwLock::new(catalog),
            settings: RwLock::new(settings),
            settings_path,
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
            autoplay_mode: default_autoplay_mode(),
            last_catalog_path: None,
            last_project_id: None,
            last_asset_id: None,
            last_context_updated_at: None,
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

fn error_message(error: impl std::fmt::Display) -> String {
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
    tauri::async_runtime::spawn_blocking(move || catalog.ingest_folder(&project_id, path))
        .await
        .map_err(error_message)?
        .map_err(error_message)
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
fn apply_clock_event(
    project_id: String,
    camera: String,
    start_filename: String,
    offset_seconds: i64,
    label: String,
    state: State<'_, AppState>,
) -> Result<ClockEvent, String> {
    state
        .catalog()?
        .apply_clock_event(
            &project_id,
            &camera,
            &start_filename,
            offset_seconds,
            &label,
        )
        .map_err(error_message)
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
            let catalog = match std::env::var_os("RCDVLOG_CATALOG") {
                Some(path) => Catalog::open_existing(PathBuf::from(path))?,
                None => Catalog::open(app_data.join("catalog.sqlite3"))?,
            };
            app.manage(AppState::new(catalog, settings, settings_path)?);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
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
            apply_clock_event,
            generate_proxy,
            generate_missing_proxies,
            generate_thumbnail,
            generate_missing_thumbnails,
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
            autoplay_mode: default_autoplay_mode(),
            last_catalog_path: None,
            last_project_id: None,
            last_asset_id: None,
            last_context_updated_at: None,
        }
    }

    #[test]
    fn switches_to_an_existing_catalog_atomically() {
        let directory = tempdir().unwrap();
        let original_path = directory.path().join("original.sqlite3");
        let original = Catalog::open(&original_path).unwrap();
        let original_project = original.create_project("Original").unwrap();
        let state = AppState::new(
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
        let state = AppState::new(
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
