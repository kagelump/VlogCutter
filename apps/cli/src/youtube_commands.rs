//! Thin CLI adapter; OAuth, credentials and upload recovery live in Rust core.
use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use rcdvlog_core::{
    Catalog,
    youtube::{self, CredentialStore, LoginOptions, UploadOptions},
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Debug, Args)]
pub struct YoutubeArgs {
    /// Absolute credential file; defaults to macOS Keychain or private local app data.
    #[arg(long, global = true, env = "RCDVLOG_YOUTUBE_CREDENTIAL_FILE")]
    credential_file: Option<PathBuf>,
    #[command(subcommand)]
    command: YoutubeCommand,
}

#[derive(Debug, Subcommand)]
enum YoutubeCommand {
    /// One-time browser authorization and persistent credential management.
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Upload a completed render, resume an interrupted upload, or reuse its existing URL.
    Upload {
        #[arg(long)]
        project: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        title: String,
        /// UTF-8 text file containing the description; defaults to empty.
        #[arg(long)]
        description_file: Option<PathBuf>,
        #[arg(long, default_value = "private", value_parser = ["private", "unlisted", "public"])]
        privacy: String,
        /// Optional identity of the rendered cut revision.
        #[arg(long)]
        cut_revision: Option<String>,
        /// Allow replacement of an expired/uncertain session; may create a duplicate.
        #[arg(long)]
        restart_upload: bool,
    },
    /// Read local upload history without contacting Google or loading credentials.
    History {
        #[arg(long)]
        project: String,
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=200))]
        limit: u32,
    },
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    Login {
        /// Google Desktop or Web application client JSON (never a service account).
        #[arg(long)]
        client_file: PathBuf,
        /// Registered HTTPS callback for a Web client, behind a trusted loopback proxy.
        #[arg(long)]
        redirect_uri: Option<String>,
        /// Explicit loopback port for SSH forwarding; zero selects an ephemeral port.
        #[arg(long, default_value_t = 0)]
        port: u16,
        #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout_seconds: u64,
        /// Print the authorization URL as a JSON event without launching a browser.
        #[arg(long)]
        no_browser: bool,
    },
    /// Offline inspection; never prints credential material or creates files.
    Status,
    /// Revoke Google's grant, then delete the local credential on confirmed success.
    Revoke,
}

pub fn command_name(arguments: &YoutubeArgs) -> &'static str {
    match &arguments.command {
        YoutubeCommand::Auth {
            command: AuthCommand::Login { .. },
        } => "youtube.auth.login",
        YoutubeCommand::Auth {
            command: AuthCommand::Status,
        } => "youtube.auth.status",
        YoutubeCommand::Auth {
            command: AuthCommand::Revoke,
        } => "youtube.auth.revoke",
        YoutubeCommand::Upload { .. } => "youtube.upload",
        YoutubeCommand::History { .. } => "youtube.history",
    }
}

pub fn execute(catalog_path: Option<&Path>, arguments: &YoutubeArgs) -> Result<Value> {
    let store = CredentialStore::new(arguments.credential_file.as_deref())?;
    let progress = |event: Value| eprintln!("{event}");
    match &arguments.command {
        YoutubeCommand::Auth { command } => match command {
            AuthCommand::Login {
                client_file,
                redirect_uri,
                port,
                timeout_seconds,
                no_browser,
            } => Ok(serde_json::to_value(youtube::login(
                &store,
                &LoginOptions {
                    client_file: client_file.clone(),
                    port: *port,
                    timeout_seconds: *timeout_seconds,
                    open_browser: !*no_browser,
                    redirect_uri: redirect_uri.clone(),
                },
                progress,
            )?)?),
            AuthCommand::Status => Ok(serde_json::to_value(store.status()?)?),
            AuthCommand::Revoke => youtube::revoke(&store),
        },
        YoutubeCommand::Upload {
            project,
            file,
            title,
            description_file,
            privacy,
            cut_revision,
            restart_upload,
        } => {
            let path = catalog_path.context(
                "invalid input: youtube upload requires an explicit --catalog or RCDVLOG_CATALOG",
            )?;
            let description = if let Some(path) = description_file {
                use std::io::Read;
                let mut description = String::new();
                std::fs::File::open(path)
                    .context("open upload description file")?
                    .take(5001)
                    .read_to_string(&mut description)
                    .context("read UTF-8 upload description")?;
                anyhow::ensure!(
                    description.len() <= 5000,
                    "invalid input: description exceeds 5000 bytes"
                );
                description
            } else {
                String::new()
            };
            let catalog = Catalog::open_existing(path)?;
            Ok(serde_json::to_value(catalog.youtube_upload(
                &store,
                &UploadOptions {
                    project_id: project.clone(),
                    file: file.clone(),
                    title: title.clone(),
                    description,
                    privacy: privacy.clone(),
                    cut_revision: cut_revision.clone(),
                    restart_upload: *restart_upload,
                },
                progress,
            )?)?)
        }
        YoutubeCommand::History { project, limit } => {
            let path = catalog_path.context(
                "invalid input: youtube history requires an explicit --catalog or RCDVLOG_CATALOG",
            )?;
            let catalog = Catalog::open_read_only_existing(path)?;
            Ok(json!({"uploads":catalog.youtube_upload_history(project, *limit)?}))
        }
    }
}
