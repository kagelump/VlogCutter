use anyhow::{Context, Result, anyhow};
#[cfg(test)]
use clap::CommandFactory;
use clap::{Parser, Subcommand, error::ErrorKind};
use rcdvlog_core::{
    Asset, AssetPatch, Catalog, MediaStreamInfo, ProxyRepresentation, inspect_catalog,
    landscape_default_rotation, probe_media_stream,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(
    name = "rcdvlog",
    version,
    about = "Machine-readable control surface for RCD VlogCutter"
)]
struct Cli {
    /// Catalog database. Defaults to the desktop app's catalog.
    #[arg(long, global = true, env = "RCDVLOG_CATALOG")]
    catalog: Option<PathBuf>,

    /// Pretty-print JSON output.
    #[arg(long, global = true)]
    pretty: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Report required external tools and the selected catalog.
    Doctor,
    /// Inspect catalog structure without migrations or writes.
    Catalog {
        #[command(subcommand)]
        command: CatalogCommand,
    },
    /// Discover the catalog, project, and clip last shown by the desktop app.
    Desktop {
        #[command(subcommand)]
        command: DesktopCommand,
    },
    /// Create, list, or summarize projects.
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Reference all supported media in a folder.
    Ingest {
        #[arg(long)]
        project: String,
        #[arg(long)]
        folder: PathBuf,
    },
    /// Read or mutate clips.
    Clips {
        #[command(subcommand)]
        command: ClipsCommand,
    },
    /// Apply a fixed camera-clock boundary.
    Clock {
        #[command(subcommand)]
        command: ClockCommand,
    },
    /// Generate laptop review proxies.
    Proxy {
        #[command(subcommand)]
        command: ProxyCommand,
    },
    /// Generate project-local clip thumbnails.
    Thumbnails {
        #[command(subcommand)]
        command: ThumbnailCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ProjectCommand {
    Create {
        #[arg(long)]
        name: String,
    },
    List,
    Status {
        #[arg(long)]
        project: String,
    },
    /// Summarize diagnostic risks without probing every source file.
    Diagnose {
        #[arg(long)]
        project: String,
    },
}

#[derive(Debug, Subcommand)]
enum CatalogCommand {
    Inspect,
}

#[derive(Debug, Subcommand)]
enum DesktopCommand {
    LastOpened,
}

#[derive(Debug, Subcommand)]
enum ClipsCommand {
    List {
        #[arg(long)]
        project: String,
        #[arg(long)]
        query: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Include clips removed from the labeling queue.
        #[arg(long)]
        include_removed: bool,
    },
    Get {
        #[arg(long)]
        asset: String,
    },
    /// Probe source/cache media and explain the exact render plan without writes.
    Diagnose {
        #[arg(long)]
        asset: String,
    },
    /// Persist current orientation metadata from the immutable original.
    RefreshOrientation {
        #[arg(long)]
        asset: String,
    },
    Update {
        #[arg(long)]
        asset: String,
        #[arg(long)]
        notes: Option<String>,
        /// Add a free-form label. Repeat to add more than one.
        #[arg(long = "add-label")]
        add_labels: Vec<String>,
        /// Remove a label by case-insensitive match. Repeat as needed.
        #[arg(long = "remove-label")]
        remove_labels: Vec<String>,
        /// Hide or restore the clip in the labeling queue.
        #[arg(long)]
        removed: Option<bool>,
        /// Clockwise display rotation: 0, 90, 180, or 270 degrees.
        #[arg(long)]
        rotation_degrees: Option<u16>,
    },
}

#[derive(Debug, Subcommand)]
enum ClockCommand {
    Apply {
        #[arg(long)]
        project: String,
        #[arg(long)]
        camera: String,
        #[arg(long)]
        start_filename: String,
        #[arg(long, allow_hyphen_values = true)]
        offset_minutes: i64,
        #[arg(long, default_value = "Agent clock correction")]
        label: String,
    },
}

#[derive(Debug, Subcommand)]
enum ProxyCommand {
    Generate {
        #[arg(long)]
        asset: String,
    },
    Missing {
        #[arg(long)]
        project: String,
    },
    /// Report missing and obsolete proxy representations without generating them.
    Status {
        #[arg(long, required_unless_present = "project", conflicts_with = "project")]
        asset: Option<String>,
        #[arg(long, required_unless_present = "asset", conflicts_with = "asset")]
        project: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
}

#[derive(Debug, Subcommand)]
enum ThumbnailCommand {
    Generate {
        #[arg(long)]
        asset: String,
        #[arg(long, default_value_t = 1)]
        quality: u8,
    },
    Missing {
        #[arg(long)]
        project: String,
        #[arg(long, default_value_t = 1)]
        quality: u8,
    },
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopContext {
    last_catalog_path: Option<PathBuf>,
    last_project_id: Option<String>,
    last_asset_id: Option<String>,
    last_context_updated_at: Option<String>,
}

fn desktop_settings_path() -> Result<PathBuf> {
    let data_dir =
        dirs::data_dir().ok_or_else(|| anyhow!("cannot resolve application data directory"))?;
    Ok(data_dir.join("com.rcd.vlogcutter").join("settings.json"))
}

fn desktop_context() -> Result<DesktopContext> {
    let path = desktop_settings_path()?;
    if !path.is_file() {
        return Ok(DesktopContext::default());
    }
    let value: Value = serde_json::from_slice(
        &std::fs::read(&path)
            .with_context(|| format!("read desktop settings {}", path.display()))?,
    )
    .with_context(|| format!("parse desktop settings {}", path.display()))?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

fn default_catalog_path() -> Result<PathBuf> {
    let data_dir =
        dirs::data_dir().ok_or_else(|| anyhow!("cannot resolve application data directory"))?;
    Ok(data_dir.join("com.rcd.vlogcutter").join("catalog.sqlite3"))
}

fn selected_catalog(cli: &Cli) -> Result<PathBuf> {
    if let Some(path) = cli.catalog.clone() {
        return Ok(path);
    }
    if let Some(path) = desktop_context()?.last_catalog_path {
        return Ok(path);
    }
    default_catalog_path()
}

fn command_name(command: &Commands) -> &'static str {
    match command {
        Commands::Doctor => "doctor",
        Commands::Catalog { .. } => "catalog.inspect",
        Commands::Desktop { .. } => "desktop.last-opened",
        Commands::Project { command } => match command {
            ProjectCommand::Create { .. } => "project.create",
            ProjectCommand::List => "project.list",
            ProjectCommand::Status { .. } => "project.status",
            ProjectCommand::Diagnose { .. } => "project.diagnose",
        },
        Commands::Ingest { .. } => "ingest",
        Commands::Clips { command } => match command {
            ClipsCommand::List { .. } => "clips.list",
            ClipsCommand::Get { .. } => "clips.get",
            ClipsCommand::Diagnose { .. } => "clips.diagnose",
            ClipsCommand::RefreshOrientation { .. } => "clips.refresh-orientation",
            ClipsCommand::Update { .. } => "clips.update",
        },
        Commands::Clock { .. } => "clock.apply",
        Commands::Proxy { command } => match command {
            ProxyCommand::Generate { .. } => "proxy.generate",
            ProxyCommand::Missing { .. } => "proxy.missing",
            ProxyCommand::Status { .. } => "proxy.status",
        },
        Commands::Thumbnails { command } => match command {
            ThumbnailCommand::Generate { .. } => "thumbnails.generate",
            ThumbnailCommand::Missing { .. } => "thumbnails.missing",
        },
    }
}

fn execute(cli: &Cli) -> Result<Value> {
    let catalog_path = selected_catalog(cli)?;
    if matches!(cli.command, Commands::Doctor) {
        return Ok(json!({
            "catalog": catalog_path,
            "tools": rcdvlog_core::prerequisite_status(),
        }));
    }

    if matches!(cli.command, Commands::Desktop { .. }) {
        let context = desktop_context()?;
        return Ok(json!({
            "settingsPath": desktop_settings_path()?,
            "catalogPath": context.last_catalog_path,
            "projectId": context.last_project_id,
            "assetId": context.last_asset_id,
            "updatedAt": context.last_context_updated_at,
        }));
    }
    if matches!(cli.command, Commands::Catalog { .. }) {
        return Ok(serde_json::to_value(inspect_catalog(&catalog_path)?)?);
    }

    let read_only = matches!(
        cli.command,
        Commands::Project {
            command: ProjectCommand::Diagnose { .. }
        } | Commands::Clips {
            command: ClipsCommand::Diagnose { .. }
        } | Commands::Proxy {
            command: ProxyCommand::Status { .. }
        }
    );
    let catalog = if read_only {
        Catalog::open_read_only_existing(&catalog_path)?
    } else {
        Catalog::open(&catalog_path)?
    };
    match &cli.command {
        Commands::Doctor => unreachable!(),
        Commands::Catalog { .. } | Commands::Desktop { .. } => unreachable!(),
        Commands::Project { command } => match command {
            ProjectCommand::Create { name } => {
                Ok(serde_json::to_value(catalog.create_project(name)?)?)
            }
            ProjectCommand::List => Ok(serde_json::to_value(catalog.list_projects()?)?),
            ProjectCommand::Status { project } => project_status(&catalog, project),
            ProjectCommand::Diagnose { project } => project_diagnosis(&catalog, project),
        },
        Commands::Ingest { project, folder } => Ok(serde_json::to_value(
            catalog.ingest_folder_with_progress(project, folder, |completed, total, path| {
                if completed == total || completed % 25 == 0 {
                    eprintln!(
                        "{}",
                        json!({
                            "event": "ingest.progress",
                            "completed": completed,
                            "total": total,
                            "path": path,
                        })
                    );
                }
            })?,
        )?),
        Commands::Clips { command } => match command {
            ClipsCommand::List {
                project,
                query,
                limit,
                offset,
                include_removed,
            } => {
                let all = catalog.list_assets(project)?;
                let filtered: Vec<_> = all
                    .into_iter()
                    .filter(|asset| {
                        (*include_removed || !asset.removed)
                            && query.as_ref().is_none_or(|query| {
                                let needle = query.to_lowercase();
                                asset.filename.to_lowercase().contains(&needle)
                                    || asset.camera.to_lowercase().contains(&needle)
                                    || asset.notes.to_lowercase().contains(&needle)
                                    || asset
                                        .labels
                                        .iter()
                                        .any(|label| label.to_lowercase().contains(&needle))
                            })
                    })
                    .collect();
                let total = filtered.len();
                let items: Vec<_> = filtered.into_iter().skip(*offset).take(*limit).collect();
                Ok(json!({ "total": total, "offset": offset, "limit": limit, "items": items }))
            }
            ClipsCommand::Get { asset } => Ok(serde_json::to_value(catalog.get_asset(asset)?)?),
            ClipsCommand::Diagnose { asset } => clip_diagnosis(&catalog, asset),
            ClipsCommand::RefreshOrientation { asset } => Ok(serde_json::to_value(
                catalog.refresh_asset_orientation(asset)?,
            )?),
            ClipsCommand::Update {
                asset,
                notes,
                add_labels,
                remove_labels,
                removed,
                rotation_degrees,
            } => {
                let current = catalog.get_asset(asset)?;
                let mut labels = current.labels;
                labels.retain(|label| {
                    !remove_labels
                        .iter()
                        .any(|removed| removed.trim().eq_ignore_ascii_case(label))
                });
                labels.extend(add_labels.iter().cloned());
                let patch = AssetPatch {
                    notes: notes.clone().unwrap_or(current.notes),
                    labels,
                    removed: removed.unwrap_or(current.removed),
                    rotation_degrees: rotation_degrees.unwrap_or(current.rotation_degrees),
                };
                Ok(serde_json::to_value(catalog.update_asset(asset, &patch)?)?)
            }
        },
        Commands::Clock { command } => match command {
            ClockCommand::Apply {
                project,
                camera,
                start_filename,
                offset_minutes,
                label,
            } => Ok(serde_json::to_value(catalog.apply_clock_event(
                project,
                camera,
                start_filename,
                offset_minutes * 60,
                label,
            )?)?),
        },
        Commands::Proxy { command } => match command {
            ProxyCommand::Generate { asset } => {
                Ok(serde_json::to_value(catalog.generate_proxy(asset)?)?)
            }
            ProxyCommand::Missing { project } => Ok(serde_json::to_value(
                catalog.generate_missing_proxies(project)?,
            )?),
            ProxyCommand::Status {
                asset,
                project,
                limit,
            } => proxy_status(&catalog, asset.as_deref(), project.as_deref(), *limit),
        },
        Commands::Thumbnails { command } => match command {
            ThumbnailCommand::Generate { asset, quality } => Ok(serde_json::to_value(
                catalog.generate_thumbnail(asset, *quality)?,
            )?),
            ThumbnailCommand::Missing { project, quality } => Ok(serde_json::to_value(
                catalog.generate_missing_thumbnails(project, *quality)?,
            )?),
        },
    }
}

fn proxy_version(asset: &Asset) -> &'static str {
    if !asset.proxy_available {
        return "missing";
    }
    match asset.proxy_representation {
        Some(ProxyRepresentation::RawV3) => "current_raw_v3",
        Some(ProxyRepresentation::MetadataTaggedRawV2) => "metadata_tagged_raw_v2",
        Some(ProxyRepresentation::MetadataBakedV1) => "legacy_metadata_baked_v1",
        None => "missing",
    }
}

fn thumbnail_version(path: Option<&str>) -> &'static str {
    match path {
        None => "missing",
        Some(path) if !Path::new(path).is_file() => "missing",
        Some(path) if path.ends_with(".v2.jpg") => "current_raw_v2",
        Some(_) => "legacy_metadata_baked_v1",
    }
}

fn probe_value(path: &str) -> (Option<MediaStreamInfo>, Value) {
    match probe_media_stream(path) {
        Ok(probe) => {
            let value = serde_json::to_value(&probe).unwrap_or(Value::Null);
            (Some(probe), value)
        }
        Err(error) => (None, json!({ "path": path, "error": format!("{error:#}") })),
    }
}

fn rotation_after(rotation: u16, addition: i32) -> u16 {
    ((rotation as i32 + addition).rem_euclid(360)) as u16
}

fn clip_diagnosis(catalog: &Catalog, asset_id: &str) -> Result<Value> {
    let asset = catalog.get_asset(asset_id)?;
    let (original_probe, original) = probe_value(&asset.path);
    let proxy_version = proxy_version(&asset);
    let proxy_probe = asset
        .proxy_path
        .as_deref()
        .filter(|path| Path::new(path).is_file())
        .map(probe_value);
    let thumbnail_version = thumbnail_version(asset.thumbnail_path.as_deref());

    let live_metadata_rotation = original_probe
        .as_ref()
        .map(|probe| probe.metadata_rotation_degrees);
    let live_default_rotation = original_probe.as_ref().map(|probe| {
        landscape_default_rotation(probe.width, probe.height, probe.metadata_rotation_degrees)
    });
    let has_override = asset.rotation_degrees != asset.default_rotation_degrees;
    let effective_rotation = if asset.orientation_probed || has_override {
        asset.rotation_degrees
    } else {
        live_default_rotation.unwrap_or(asset.rotation_degrees)
    };
    let effective_metadata = if asset.orientation_probed {
        asset.metadata_rotation_degrees
    } else {
        live_metadata_rotation.unwrap_or(asset.metadata_rotation_degrees)
    };

    let (selected_kind, selected_version, selected_probe) = if let Some((probe, _)) = &proxy_probe {
        ("proxy", proxy_version, probe.as_ref())
    } else {
        ("original", "source_with_metadata", original_probe.as_ref())
    };
    let proxy_live_rotation = proxy_probe
        .as_ref()
        .and_then(|(probe, _)| probe.as_ref())
        .map(|probe| probe.metadata_rotation_degrees);
    let current_proxy_contract_valid =
        proxy_version == "current_raw_v3" && proxy_live_rotation == Some(0);
    let representation_is_raw =
        selected_version == "current_raw_v3" && current_proxy_contract_valid;
    let baked_rotation = if representation_is_raw {
        0
    } else if selected_kind == "proxy"
        && matches!(
            selected_version,
            "current_raw_v3" | "metadata_tagged_raw_v2"
        )
    {
        proxy_live_rotation.unwrap_or(effective_metadata)
    } else {
        effective_metadata
    };
    let applied_rotation = rotation_after(effective_rotation, -(baked_rotation as i32));
    let (final_width, final_height) = selected_probe
        .map(|probe| {
            let (width, height) = (probe.display_width, probe.display_height);
            if matches!(applied_rotation, 90 | 270) {
                (height, width)
            } else {
                (width, height)
            }
        })
        .unwrap_or((0, 0));
    let final_orientation = if final_width > final_height {
        "landscape"
    } else if final_width < final_height {
        "portrait"
    } else {
        "unknown"
    };

    let mut warnings = Vec::new();
    if !asset.orientation_probed {
        warnings.push(
            "catalog orientation is unprobed; stored 0° means unknown, not confirmed".to_string(),
        );
    }
    if let Some(rotation) = live_metadata_rotation
        && rotation != asset.metadata_rotation_degrees
    {
        warnings.push(format!(
            "stored metadata rotation is {}° but the original currently reports {}°",
            asset.metadata_rotation_degrees, rotation
        ));
    }
    if proxy_version == "metadata_tagged_raw_v2" {
        warnings.push(
            "selected proxy is obsolete v2 media whose display rotation can be applied twice"
                .to_string(),
        );
    } else if proxy_version == "legacy_metadata_baked_v1" {
        warnings.push(
            "selected proxy is legacy v1 media with camera rotation baked into its pixels"
                .to_string(),
        );
    }
    if proxy_version == "current_raw_v3" && !current_proxy_contract_valid {
        warnings.push(
            "current v3 proxy failed its raw-orientation contract; regenerate it from the original"
                .to_string(),
        );
    }
    if final_orientation != "landscape" {
        warnings.push(format!(
            "computed rendered result is {final_orientation}, not landscape"
        ));
    }

    let mut recommended_actions = Vec::new();
    if !asset.orientation_probed {
        recommended_actions.push(format!(
            "rcdvlog --catalog {} clips refresh-orientation --asset {}",
            catalog.database_path().display(),
            asset.id
        ));
    }
    if proxy_version != "missing"
        && (proxy_version != "current_raw_v3" || !current_proxy_contract_valid)
        && asset.original_available
    {
        recommended_actions.push(format!(
            "rcdvlog --catalog {} proxy generate --asset {}",
            catalog.database_path().display(),
            asset.id
        ));
    }

    Ok(json!({
        "catalogPath": catalog.database_path(),
        "asset": asset,
        "storedOrientation": {
            "probed": asset.orientation_probed,
            "metadataRotationDegrees": asset.metadata_rotation_degrees,
            "defaultRotationDegrees": asset.default_rotation_degrees,
            "rotationDegrees": asset.rotation_degrees,
            "hasUserOverride": has_override,
        },
        "liveOrientation": {
            "metadataRotationDegrees": live_metadata_rotation,
            "landscapeDefaultRotationDegrees": live_default_rotation,
            "effectiveRotationDegrees": effective_rotation,
        },
        "representations": {
            "original": original,
            "proxy": {
                "path": asset.proxy_path,
                "version": proxy_version,
                "rawOrientationContractValid": current_proxy_contract_valid,
                "probe": proxy_probe.as_ref().map(|(_, value)| value),
            },
            "thumbnail": {
                "path": asset.thumbnail_path,
                "version": thumbnail_version,
            },
        },
        "renderPlan": {
            "selectedRepresentation": selected_kind,
            "selectedVersion": selected_version,
            "representationRotationAlreadyAppliedDegrees": baked_rotation,
            "cssRotationDegrees": applied_rotation,
            "finalWidth": final_width,
            "finalHeight": final_height,
            "finalOrientation": final_orientation,
        },
        "warnings": warnings,
        "recommendedActions": recommended_actions,
    }))
}

fn proxy_status(
    catalog: &Catalog,
    asset_id: Option<&str>,
    project_id: Option<&str>,
    limit: usize,
) -> Result<Value> {
    let assets = if let Some(asset_id) = asset_id {
        vec![catalog.get_asset(asset_id)?]
    } else {
        catalog.list_assets(project_id.expect("clap requires asset or project"))?
    };
    let total = assets.len();
    let items = assets
        .iter()
        .take(limit)
        .map(|asset| {
            let version = proxy_version(asset);
            let validation = if version == "current_raw_v3" {
                asset
                    .proxy_path
                    .as_deref()
                    .map(|path| match probe_media_stream(path) {
                        Ok(probe) => json!({
                            "valid": probe.metadata_rotation_degrees == 0,
                            "metadataRotationDegrees": probe.metadata_rotation_degrees,
                        }),
                        Err(error) => json!({ "valid": false, "error": format!("{error:#}") }),
                    })
            } else {
                None
            };
            let ready = version == "current_raw_v3"
                && validation
                    .as_ref()
                    .and_then(|value| value["valid"].as_bool())
                    == Some(true);
            json!({
                "assetId": asset.id,
                "filename": asset.filename,
                "path": asset.proxy_path,
                "version": version,
                "validation": validation,
                "ready": ready,
                "regenerationRequired": !ready,
                "originalAvailable": asset.original_available,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "total": total,
        "limit": limit,
        "summary": {
            "current": assets.iter().filter(|asset| proxy_version(asset) == "current_raw_v3").count(),
            "obsoleteV2": assets.iter().filter(|asset| proxy_version(asset) == "metadata_tagged_raw_v2").count(),
            "legacy": assets.iter().filter(|asset| proxy_version(asset) == "legacy_metadata_baked_v1").count(),
            "missing": assets.iter().filter(|asset| proxy_version(asset) == "missing").count(),
        },
        "items": items,
    }))
}

fn project_diagnosis(catalog: &Catalog, project_id: &str) -> Result<Value> {
    let status = project_status(catalog, project_id)?;
    let assets = catalog.list_assets(project_id)?;
    let context = desktop_context().unwrap_or_default();
    Ok(json!({
        "catalogPath": catalog.database_path(),
        "status": status,
        "orientation": {
            "unprobed": assets.iter().filter(|asset| !asset.orientation_probed).count(),
            "overridden": assets.iter().filter(|asset| asset.rotation_degrees != asset.default_rotation_degrees).count(),
        },
        "proxies": {
            "current": assets.iter().filter(|asset| proxy_version(asset) == "current_raw_v3").count(),
            "obsoleteV2": assets.iter().filter(|asset| proxy_version(asset) == "metadata_tagged_raw_v2").count(),
            "legacy": assets.iter().filter(|asset| proxy_version(asset) == "legacy_metadata_baked_v1").count(),
            "missing": assets.iter().filter(|asset| proxy_version(asset) == "missing").count(),
        },
        "desktopContext": {
            "matchesProject": context.last_project_id.as_deref() == Some(project_id),
            "selectedAssetId": context.last_asset_id,
            "updatedAt": context.last_context_updated_at,
        },
        "next": "Run clips diagnose --asset <id> for live ffprobe evidence and the exact render plan.",
    }))
}

fn project_status(catalog: &Catalog, project_id: &str) -> Result<Value> {
    let project = catalog
        .list_projects()?
        .into_iter()
        .find(|project| project.id == project_id)
        .ok_or_else(|| anyhow!("project not found: {project_id}"))?;
    let assets = catalog.list_assets(project_id)?;
    let duration_seconds: f64 = assets.iter().map(|asset| asset.duration_seconds).sum();
    Ok(json!({
        "project": project,
        "media": {
            "assets": assets.len(),
            "durationSeconds": duration_seconds,
            "originalsOffline": assets.iter().filter(|asset| !Path::new(&asset.path).exists()).count(),
            "proxiesReady": assets.iter().filter(|asset| proxy_version(asset) == "current_raw_v3").count(),
            "proxiesObsoleteV2": assets.iter().filter(|asset| proxy_version(asset) == "metadata_tagged_raw_v2").count(),
            "proxiesLegacy": assets.iter().filter(|asset| proxy_version(asset) == "legacy_metadata_baked_v1").count(),
            "labeled": assets.iter().filter(|asset| !asset.labels.is_empty()).count(),
            "removed": assets.iter().filter(|asset| asset.removed).count(),
            "thumbnailsReady": assets.iter().filter(|asset| asset.thumbnail_quality > 0 && asset.thumbnail_path.as_deref().is_some_and(|path| Path::new(path).exists())).count(),
        }
    }))
}

fn error_code(message: &str) -> &'static str {
    let message = message.to_lowercase();
    if message.contains("catalog schema") || message.contains("migration history") {
        "schema_incompatible"
    } else if message.contains("not found") {
        "not_found"
    } else if message.contains("offline") {
        "media_offline"
    } else if message.contains("empty") || message.contains("invalid") {
        "invalid_input"
    } else if message.contains("ffmpeg")
        || message.contains("ffprobe")
        || message.contains("exiftool")
    {
        "media_tool_failed"
    } else {
        "internal_error"
    }
}

fn write_json(value: &Value, pretty: bool, stderr: bool) {
    let output = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
    .expect("serialize CLI output");
    if stderr {
        eprintln!("{output}");
    } else {
        println!("{output}");
    }
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            error.print().ok();
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            write_json(
                &json!({
                    "ok": false,
                    "command": "parse",
                    "error": { "code": "invalid_arguments", "message": error.to_string() }
                }),
                false,
                true,
            );
            return ExitCode::from(2);
        }
    };
    let name = command_name(&cli.command);
    match execute(&cli).with_context(|| format!("{name} failed")) {
        Ok(data) => {
            write_json(
                &json!({ "ok": true, "command": name, "data": data }),
                cli.pretty,
                false,
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            let message = format!("{error:#}");
            write_json(
                &json!({
                    "ok": false,
                    "command": name,
                    "error": { "code": error_code(&message), "message": message }
                }),
                cli.pretty,
                true,
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{Connection, params};
    use tempfile::tempdir;

    fn parse(arguments: &[&str]) -> Cli {
        Cli::try_parse_from(arguments).unwrap()
    }

    fn insert_cli_asset(
        catalog_path: &Path,
        project_id: &str,
        id: &str,
        filename: &str,
        notes: &str,
        labels_json: &str,
        removed: bool,
    ) {
        Connection::open(catalog_path)
            .unwrap()
            .execute(
                r#"
                INSERT INTO assets (
                    id, project_id, fingerprint, path, filename, size_bytes,
                    duration_seconds, frame_rate, width, height, codec, camera,
                    captured_at_raw, best_time, notes, labels_json, removed,
                    created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, 100, 10, 30, 1920, 1080,
                          'h264', 'DJI', '2026-07-18T00:00:00Z',
                          '2026-07-18T00:00:00.000Z', ?6, ?7, ?8,
                          '2026-07-18T00:00:00.000Z')
                "#,
                params![
                    id,
                    project_id,
                    format!("fingerprint-{id}"),
                    format!("/missing/{filename}"),
                    filename,
                    notes,
                    labels_json,
                    removed,
                ],
            )
            .unwrap();
    }

    #[test]
    fn creates_lists_and_summarizes_a_project() {
        let directory = tempdir().unwrap();
        let catalog = directory.path().join("catalog.sqlite3");
        let catalog_arg = catalog.to_string_lossy();
        let create = parse(&[
            "rcdvlog",
            "--catalog",
            &catalog_arg,
            "project",
            "create",
            "--name",
            "CLI test",
        ]);
        let created = execute(&create).unwrap();
        let project_id = created["id"].as_str().unwrap();

        let list = parse(&["rcdvlog", "--catalog", &catalog_arg, "project", "list"]);
        assert_eq!(execute(&list).unwrap().as_array().unwrap().len(), 1);

        let status = parse(&[
            "rcdvlog",
            "--catalog",
            &catalog_arg,
            "project",
            "status",
            "--project",
            project_id,
        ]);
        assert_eq!(execute(&status).unwrap()["media"]["assets"], 0);
    }

    #[test]
    fn clap_contract_has_all_agent_commands() {
        let help = Cli::command().render_long_help().to_string();
        for command in [
            "doctor",
            "project",
            "ingest",
            "clips",
            "clock",
            "proxy",
            "thumbnails",
        ] {
            assert!(help.contains(command));
        }
    }

    #[test]
    fn clip_update_is_a_patch_for_labels_and_removed_state() {
        let directory = tempdir().unwrap();
        let catalog_path = directory.path().join("catalog.sqlite3");
        let catalog = Catalog::open(&catalog_path).unwrap();
        let project = catalog.create_project("Patch test").unwrap();
        insert_cli_asset(
            &catalog_path,
            &project.id,
            "asset_a",
            "DJI_A.MP4",
            "original note",
            "[\"arrival\"]",
            false,
        );
        let catalog_arg = catalog_path.to_string_lossy();

        let notes_only = parse(&[
            "rcdvlog",
            "--catalog",
            &catalog_arg,
            "clips",
            "update",
            "--asset",
            "asset_a",
            "--notes",
            "changed note",
        ]);
        let updated = execute(&notes_only).unwrap();
        assert_eq!(updated["notes"], "changed note");
        assert_eq!(updated["labels"], json!(["arrival"]));
        assert_eq!(updated["removed"], false);

        let label_update = parse(&[
            "rcdvlog",
            "--catalog",
            &catalog_arg,
            "clips",
            "update",
            "--asset",
            "asset_a",
            "--remove-label",
            "ARRIVAL",
            "--add-label",
            "train",
            "--add-label",
            "Train",
            "--removed",
            "true",
        ]);
        let updated = execute(&label_update).unwrap();
        assert_eq!(updated["labels"], json!(["train"]));
        assert_eq!(updated["removed"], true);
        assert_eq!(updated["notes"], "changed note");
    }

    #[test]
    fn clip_list_filters_before_paginating() {
        let directory = tempdir().unwrap();
        let catalog_path = directory.path().join("catalog.sqlite3");
        let catalog = Catalog::open(&catalog_path).unwrap();
        let project = catalog.create_project("Filter test").unwrap();
        for (id, filename, notes) in [
            ("asset_a", "DJI_A.MP4", "arrival exterior"),
            ("asset_b", "DJI_B.MP4", "arrival interior"),
            ("asset_c", "DJI_C.MP4", "departure"),
        ] {
            insert_cli_asset(&catalog_path, &project.id, id, filename, notes, "[]", false);
        }
        let catalog_arg = catalog_path.to_string_lossy();
        let command = parse(&[
            "rcdvlog",
            "--catalog",
            &catalog_arg,
            "clips",
            "list",
            "--project",
            &project.id,
            "--query",
            "ARRIVAL",
            "--limit",
            "1",
            "--offset",
            "1",
        ]);
        let result = execute(&command).unwrap();
        assert_eq!(result["total"], 2);
        assert_eq!(result["items"].as_array().unwrap().len(), 1);
        assert_eq!(result["items"][0]["filename"], "DJI_B.MP4");
    }

    #[test]
    fn doctor_does_not_create_the_selected_catalog() {
        let directory = tempdir().unwrap();
        let catalog_path = directory.path().join("not-created.sqlite3");
        let catalog_arg = catalog_path.to_string_lossy();
        let doctor = parse(&["rcdvlog", "--catalog", &catalog_arg, "doctor"]);
        let result = execute(&doctor).unwrap();
        assert_eq!(result["catalog"], catalog_arg.as_ref());
        assert!(!catalog_path.exists());
    }

    #[test]
    fn catalog_inspection_reports_pending_schema_without_migrating() {
        let directory = tempdir().unwrap();
        let catalog_path = directory.path().join("legacy.sqlite3");
        let connection = Connection::open(&catalog_path).unwrap();
        connection
            .execute_batch("CREATE TABLE projects (id TEXT); CREATE TABLE assets (id TEXT);")
            .unwrap();
        drop(connection);
        let catalog_arg = catalog_path.to_string_lossy();
        let inspect = parse(&["rcdvlog", "--catalog", &catalog_arg, "catalog", "inspect"]);
        let result = execute(&inspect).unwrap();
        assert_eq!(result["readOnly"], true);
        assert_eq!(result["schemaHistoryPresent"], false);
        assert_eq!(result["currentSchemaVersion"], 0);
        assert_eq!(
            result["latestSupportedSchemaVersion"],
            rcdvlog_core::PROJECT_CATALOG_SCHEMA_VERSION
        );
        assert_eq!(result["schemaStatus"], "legacy_unversioned");
        assert_eq!(result["pendingMigrations"].as_array().unwrap().len(), 4);
        assert!(
            result["pendingAssetColumns"]
                .as_array()
                .unwrap()
                .iter()
                .any(|column| column == "orientation_probed")
        );
        let columns = Connection::open(&catalog_path)
            .unwrap()
            .prepare("PRAGMA table_info(assets)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(!columns.iter().any(|column| column == "orientation_probed"));
        assert_eq!(
            Connection::open(&catalog_path)
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn reports_stable_error_codes_and_requires_explicit_project_arguments() {
        assert_eq!(error_code("asset not found"), "not_found");
        assert_eq!(error_code("original is offline"), "media_offline");
        assert_eq!(error_code("invalid source range"), "invalid_input");
        assert_eq!(error_code("ffmpeg failed"), "media_tool_failed");
        assert_eq!(
            error_code("catalog schema version 5 is newer than supported version 4"),
            "schema_incompatible"
        );
        assert_eq!(error_code("database locked"), "internal_error");
        assert!(Cli::try_parse_from(["rcdvlog", "clips", "list"]).is_err());
        assert!(Cli::try_parse_from(["rcdvlog", "project", "status"]).is_err());
    }
}
