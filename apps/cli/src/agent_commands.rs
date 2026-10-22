use anyhow::{Result, anyhow};
use clap::{Args, CommandFactory, Subcommand, ValueEnum};
use rcdvlog_core::{CliBuild, CliExecutable, CliSelection, ReadinessRequirements};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Args)]
pub struct ReadinessArgs {
    /// Require a compatible catalog, all media tools and installed speech models.
    #[arg(long)]
    check: bool,
    /// Require an exact command name or versioned schema capability (repeatable).
    #[arg(long = "require-capability")]
    capabilities: Vec<String>,
    /// Require a supported media tool (ffmpeg, ffprobe, exiftool; repeatable).
    #[arg(long = "require-tool")]
    tools: Vec<String>,
    /// Require locally installed speech models, without downloading them.
    #[arg(long)]
    require_models: bool,
    /// Require this full source revision, captured at compile time.
    #[arg(long)]
    expect_revision: Option<String>,
}

impl ReadinessArgs {
    fn requirements(&self) -> ReadinessRequirements {
        let mut tools = self.tools.clone();
        if self.check {
            tools.extend(["ffmpeg".into(), "ffprobe".into(), "exiftool".into()]);
        }
        tools.sort();
        tools.dedup();
        ReadinessRequirements {
            catalog: self.check,
            tools,
            models: self.check || self.require_models,
            capabilities: self.capabilities.clone(),
            revision: self.expect_revision.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Selection {
    Installed,
    Development,
}

#[derive(Debug, Subcommand)]
pub enum WorkspaceCommand {
    /// Explicitly bind/repair this project's selected CLI; preserve project instructions.
    Refresh {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        executable: PathBuf,
        #[arg(long, value_enum)]
        selection: Selection,
        #[command(flatten)]
        readiness: ReadinessArgs,
    },
    /// Verify the existing exact binding without writes or fallback.
    Check {
        #[arg(long)]
        root: PathBuf,
        #[command(flatten)]
        readiness: ReadinessArgs,
    },
}

pub fn command_name(command: &WorkspaceCommand) -> &'static str {
    match command {
        WorkspaceCommand::Refresh { .. } => "workspace.refresh",
        WorkspaceCommand::Check { .. } => "workspace.check",
    }
}

fn command_paths(command: &clap::Command, prefix: &str, paths: &mut Vec<String>) {
    for child in command
        .get_subcommands()
        .filter(|child| child.get_name() != "help")
    {
        let name = if prefix.is_empty() {
            child.get_name().to_owned()
        } else {
            format!("{prefix}.{}", child.get_name())
        };
        if child.has_subcommands() {
            command_paths(child, &name, paths);
        } else {
            paths.push(name);
        }
    }
}

fn executable() -> Result<CliExecutable> {
    let mut commands = Vec::new();
    command_paths(&super::Cli::command(), "", &mut commands);
    commands.sort();
    let revision = env!("RCDVLOG_BUILD_REVISION");
    Ok(CliExecutable {
        path: std::fs::canonicalize(std::env::current_exe()?)?,
        build: CliBuild {
            version: env!("CARGO_PKG_VERSION").into(),
            revision: (revision != "unknown").then(|| revision.to_owned()),
            dirty: env!("RCDVLOG_BUILD_DIRTY") == "true",
            profile: env!("RCDVLOG_BUILD_PROFILE").into(),
            target: env!("RCDVLOG_BUILD_TARGET").into(),
            rustc: env!("RCDVLOG_BUILD_RUSTC").into(),
        },
        capabilities: BTreeMap::from([
            ("cli.envelope.v1".into(), 1),
            ("agent.readiness.v1".into(), 1),
            ("agent.binding.v1".into(), 1),
            (
                "catalog.schema".into(),
                rcdvlog_core::PROJECT_CATALOG_SCHEMA_VERSION,
            ),
            ("speech.local.v1".into(), 1),
            ("video.validation.v1".into(), 1),
            ("moments.discovery.v1".into(), 1),
        ]),
        commands,
    })
}

pub fn doctor(catalog: Option<&Path>, args: &ReadinessArgs) -> Result<Value> {
    let catalog = catalog
        .map(|path| {
            if path.is_absolute() {
                Ok(path.to_path_buf())
            } else {
                std::env::current_dir().map(|root| root.join(path))
            }
        })
        .transpose()?;
    let report = rcdvlog_core::doctor_report(executable()?, catalog.as_deref())?;
    rcdvlog_core::ensure_agent_readiness(&report, &args.requirements())?;
    Ok(serde_json::to_value(report)?)
}

pub fn workspace(catalog: Option<&Path>, command: &WorkspaceCommand) -> Result<Value> {
    let result = match command {
        WorkspaceCommand::Refresh {
            root,
            executable,
            selection,
            readiness,
        } => {
            let catalog = catalog.ok_or_else(|| anyhow!(rcdvlog_core::AgentReadinessError {
                code: "catalog_required", message: "workspace refresh requires explicit --catalog (or RCDVLOG_CATALOG); desktop fallback is disabled.".into(), details: None,
            }))?;
            rcdvlog_core::refresh_cli_binding(
                root,
                catalog,
                executable,
                match selection {
                    Selection::Installed => CliSelection::Installed,
                    Selection::Development => CliSelection::Development,
                },
                &readiness.requirements(),
            )?
        }
        WorkspaceCommand::Check { root, readiness } => {
            rcdvlog_core::check_cli_binding(root, catalog, &readiness.requirements())?
        }
    };
    Ok(serde_json::to_value(result)?)
}
