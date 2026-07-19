use anyhow::{Context, Result, anyhow};
#[cfg(test)]
use clap::CommandFactory;
use clap::{ArgAction, Parser, Subcommand, ValueEnum, error::ErrorKind};
use rcdvlog_core::{AssetPatch, Catalog, ReviewStatus};
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
    /// Export a single-track rough cut.
    Export {
        #[command(subcommand)]
        command: ExportCommand,
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
    },
    Get {
        #[arg(long)]
        asset: String,
    },
    Update {
        #[arg(long)]
        asset: String,
        #[arg(long)]
        notes: Option<String>,
        #[arg(long)]
        status: Option<CliReviewStatus>,
        #[arg(long, action = ArgAction::Set)]
        included: Option<bool>,
        #[arg(long)]
        source_in: Option<f64>,
        #[arg(long)]
        source_out: Option<f64>,
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

#[derive(Debug, Subcommand)]
enum ExportCommand {
    Otio {
        #[arg(long)]
        project: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliReviewStatus {
    Unreviewed,
    Reviewed,
    Skipped,
    NeedsAttention,
}

impl From<CliReviewStatus> for ReviewStatus {
    fn from(value: CliReviewStatus) -> Self {
        match value {
            CliReviewStatus::Unreviewed => Self::Unreviewed,
            CliReviewStatus::Reviewed => Self::Reviewed,
            CliReviewStatus::Skipped => Self::Skipped,
            CliReviewStatus::NeedsAttention => Self::NeedsAttention,
        }
    }
}

fn default_catalog_path() -> Result<PathBuf> {
    let data_dir =
        dirs::data_dir().ok_or_else(|| anyhow!("cannot resolve application data directory"))?;
    Ok(data_dir.join("com.rcd.vlogcutter").join("catalog.sqlite3"))
}

fn selected_catalog(cli: &Cli) -> Result<PathBuf> {
    cli.catalog
        .clone()
        .map(Ok)
        .unwrap_or_else(default_catalog_path)
}

fn command_name(command: &Commands) -> &'static str {
    match command {
        Commands::Doctor => "doctor",
        Commands::Project { command } => match command {
            ProjectCommand::Create { .. } => "project.create",
            ProjectCommand::List => "project.list",
            ProjectCommand::Status { .. } => "project.status",
        },
        Commands::Ingest { .. } => "ingest",
        Commands::Clips { command } => match command {
            ClipsCommand::List { .. } => "clips.list",
            ClipsCommand::Get { .. } => "clips.get",
            ClipsCommand::Update { .. } => "clips.update",
        },
        Commands::Clock { .. } => "clock.apply",
        Commands::Proxy { command } => match command {
            ProxyCommand::Generate { .. } => "proxy.generate",
            ProxyCommand::Missing { .. } => "proxy.missing",
        },
        Commands::Thumbnails { command } => match command {
            ThumbnailCommand::Generate { .. } => "thumbnails.generate",
            ThumbnailCommand::Missing { .. } => "thumbnails.missing",
        },
        Commands::Export { .. } => "export.otio",
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

    let catalog = Catalog::open(&catalog_path)?;
    match &cli.command {
        Commands::Doctor => unreachable!(),
        Commands::Project { command } => match command {
            ProjectCommand::Create { name } => {
                Ok(serde_json::to_value(catalog.create_project(name)?)?)
            }
            ProjectCommand::List => Ok(serde_json::to_value(catalog.list_projects()?)?),
            ProjectCommand::Status { project } => project_status(&catalog, project),
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
            } => {
                let all = catalog.list_assets(project)?;
                let filtered: Vec<_> = all
                    .into_iter()
                    .filter(|asset| {
                        query.as_ref().is_none_or(|query| {
                            let needle = query.to_lowercase();
                            asset.filename.to_lowercase().contains(&needle)
                                || asset.camera.to_lowercase().contains(&needle)
                                || asset.notes.to_lowercase().contains(&needle)
                        })
                    })
                    .collect();
                let total = filtered.len();
                let items: Vec<_> = filtered.into_iter().skip(*offset).take(*limit).collect();
                Ok(json!({ "total": total, "offset": offset, "limit": limit, "items": items }))
            }
            ClipsCommand::Get { asset } => Ok(serde_json::to_value(catalog.get_asset(asset)?)?),
            ClipsCommand::Update {
                asset,
                notes,
                status,
                included,
                source_in,
                source_out,
            } => {
                let current = catalog.get_asset(asset)?;
                let patch = AssetPatch {
                    notes: notes.clone().unwrap_or(current.notes),
                    status: status.map(Into::into).unwrap_or(current.status),
                    included: included.unwrap_or(current.included),
                    source_in: source_in.unwrap_or(current.source_in),
                    source_out: source_out.unwrap_or(current.source_out),
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
        },
        Commands::Thumbnails { command } => match command {
            ThumbnailCommand::Generate { asset, quality } => Ok(serde_json::to_value(
                catalog.generate_thumbnail(asset, *quality)?,
            )?),
            ThumbnailCommand::Missing { project, quality } => Ok(serde_json::to_value(
                catalog.generate_missing_thumbnails(project, *quality)?,
            )?),
        },
        Commands::Export { command } => match command {
            ExportCommand::Otio { project } => {
                let path = catalog.export_otio(project)?;
                Ok(json!({ "path": path }))
            }
        },
    }
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
            "proxiesReady": assets.iter().filter(|asset| asset.proxy_path.as_deref().is_some_and(|path| Path::new(path).exists())).count(),
            "reviewed": assets.iter().filter(|asset| asset.status == ReviewStatus::Reviewed).count(),
            "included": assets.iter().filter(|asset| asset.included).count(),
            "thumbnailsReady": assets.iter().filter(|asset| asset.thumbnail_quality > 0 && asset.thumbnail_path.as_deref().is_some_and(|path| Path::new(path).exists())).count(),
        }
    }))
}

fn error_code(message: &str) -> &'static str {
    let message = message.to_lowercase();
    if message.contains("not found") {
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
        status: &str,
        included: bool,
    ) {
        Connection::open(catalog_path)
            .unwrap()
            .execute(
                r#"
                INSERT INTO assets (
                    id, project_id, fingerprint, path, filename, size_bytes,
                    duration_seconds, frame_rate, width, height, codec, camera,
                    captured_at_raw, best_time, notes, status, included,
                    source_in, source_out, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, 100, 10, 30, 1920, 1080,
                          'h264', 'DJI', '2026-07-18T00:00:00Z',
                          '2026-07-18T00:00:00.000Z', ?6, ?7, ?8, 1, 5,
                          '2026-07-18T00:00:00.000Z')
                "#,
                params![
                    id,
                    project_id,
                    format!("fingerprint-{id}"),
                    format!("/missing/{filename}"),
                    filename,
                    notes,
                    status,
                    included,
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
            "export",
        ] {
            assert!(help.contains(command));
        }
    }

    #[test]
    fn clip_update_is_a_patch_and_accepts_agent_status_spelling() {
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
            "reviewed",
            true,
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
        assert_eq!(updated["status"], "reviewed");
        assert_eq!(updated["included"], true);
        assert_eq!(updated["sourceIn"], 1.0);
        assert_eq!(updated["sourceOut"], 5.0);

        let status_update = parse(&[
            "rcdvlog",
            "--catalog",
            &catalog_arg,
            "clips",
            "update",
            "--asset",
            "asset_a",
            "--status",
            "needs-attention",
            "--included",
            "false",
        ]);
        let updated = execute(&status_update).unwrap();
        assert_eq!(updated["status"], "needs_attention");
        assert_eq!(updated["included"], false);
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
            insert_cli_asset(
                &catalog_path,
                &project.id,
                id,
                filename,
                notes,
                "unreviewed",
                false,
            );
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
    fn reports_stable_error_codes_and_requires_explicit_project_arguments() {
        assert_eq!(error_code("asset not found"), "not_found");
        assert_eq!(error_code("original is offline"), "media_offline");
        assert_eq!(error_code("invalid source range"), "invalid_input");
        assert_eq!(error_code("ffmpeg failed"), "media_tool_failed");
        assert_eq!(error_code("database locked"), "internal_error");
        assert!(Cli::try_parse_from(["rcdvlog", "clips", "list"]).is_err());
        assert!(Cli::try_parse_from(["rcdvlog", "project", "status"]).is_err());
    }
}
