//! Thin JSON-file adapter for bounded native-video review.
use super::agent_commands::CommandCancellation;
use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand};
use rcdvlog_core::{Catalog, NativeVideoWindowRequest, RunPolicyParameters, RunPolicyRequest};
use serde_json::{Value, json};
use std::{fs::File, io::Read, path::Path, time::Duration};

const POLICY_LIMIT: u64 = 8 * 1024 * 1024;
const WINDOWS_LIMIT: u64 = 64 * 1024;

#[derive(Debug, Args)]
pub struct PolicyFile {
    #[arg(long)]
    policy_file: std::path::PathBuf,
}
#[derive(Debug, Subcommand)]
pub enum NativeVideoCommand {
    Preflight(PolicyFile),
    Authorize(PolicyFile),
    Pilot(PolicyFile),
    Prepare {
        #[arg(long)]
        policy_file: std::path::PathBuf,
        #[arg(long)]
        window_file: std::path::PathBuf,
    },
    Windows(PolicyFile),
    Run {
        #[arg(long)]
        policy_id: String,
        #[arg(long)]
        windows_file: Option<std::path::PathBuf>,
    },
    Status {
        #[arg(long)]
        project: String,
        #[arg(long)]
        run: String,
    },
    Wait {
        #[arg(long)]
        project: String,
        #[arg(long)]
        run: String,
        #[arg(long, default_value_t = 60)]
        timeout_seconds: u64,
    },
    Cancel {
        #[arg(long)]
        project: String,
        #[arg(long)]
        run: String,
    },
    Result {
        #[arg(long)]
        project: String,
        #[arg(long)]
        evidence: String,
    },
    Response {
        #[arg(long)]
        project: String,
        #[arg(long)]
        evidence: String,
    },
    Recover {
        #[arg(long)]
        project: String,
        #[arg(long)]
        evidence: String,
    },
    EvidenceList {
        #[arg(long)]
        project: String,
        #[arg(long)]
        policy_id: String,
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    RunWindows {
        #[arg(long)]
        project: String,
        #[arg(long)]
        run: String,
        #[arg(long)]
        after_index: Option<u32>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
}
fn read_bounded<T: serde::de::DeserializeOwned>(path: &Path, limit: u64) -> Result<T> {
    let file = File::open(path)
        .with_context(|| format!("could not open JSON input {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "invalid input: JSON file exceeds the {} byte limit",
        limit
    );
    serde_json::from_slice(&bytes).context("invalid input: malformed JSON file")
}
fn request(path: &Path) -> Result<RunPolicyRequest> {
    let req: RunPolicyRequest = read_bounded(path, POLICY_LIMIT)?;
    ensure!(
        matches!(
            &req.parameters,
            RunPolicyParameters::NativeVideoReview { .. }
        ),
        "invalid input: policy file must contain native_video_review parameters"
    );
    ensure!(
        req.provider == "gemini-direct" && req.primary_model == "gemini-3.5-flash-lite",
        "invalid input: native video requires provider gemini-direct and model gemini-3.5-flash-lite"
    );
    ensure!(
        req.allowed_models
            .iter()
            .all(|model| model == &req.primary_model)
            && !req.allow_fallback,
        "invalid input: native video does not support model alternatives or fallback"
    );
    Ok(req)
}
pub fn command_name(c: &NativeVideoCommand) -> &'static str {
    match c {
        NativeVideoCommand::Preflight(_) => "native-video.preflight",
        NativeVideoCommand::Authorize(_) => "native-video.authorize",
        NativeVideoCommand::Pilot(_) => "native-video.pilot",
        NativeVideoCommand::Prepare { .. } => "native-video.prepare",
        NativeVideoCommand::Windows(_) => "native-video.windows",
        NativeVideoCommand::Run { .. } => "native-video.run",
        NativeVideoCommand::Status { .. } => "native-video.status",
        NativeVideoCommand::Wait { .. } => "native-video.wait",
        NativeVideoCommand::Cancel { .. } => "native-video.cancel",
        NativeVideoCommand::Result { .. } => "native-video.result",
        NativeVideoCommand::Response { .. } => "native-video.response",
        NativeVideoCommand::Recover { .. } => "native-video.recover",
        NativeVideoCommand::EvidenceList { .. } => "native-video.evidence-list",
        NativeVideoCommand::RunWindows { .. } => "native-video.run-windows",
    }
}
pub fn is_read_only(c: &NativeVideoCommand) -> bool {
    matches!(
        c,
        NativeVideoCommand::Preflight(_)
            | NativeVideoCommand::Windows(_)
            | NativeVideoCommand::Status { .. }
            | NativeVideoCommand::Wait { .. }
            | NativeVideoCommand::Result { .. }
            | NativeVideoCommand::Response { .. }
            | NativeVideoCommand::EvidenceList { .. }
            | NativeVideoCommand::RunWindows { .. }
    )
}
pub fn execute(catalog: &Catalog, c: &NativeVideoCommand) -> Result<Value> {
    Ok(match c {
        NativeVideoCommand::Preflight(a) => {
            serde_json::to_value(catalog.describe_run_policy(&request(&a.policy_file)?)?)?
        }
        NativeVideoCommand::Authorize(a) => {
            serde_json::to_value(catalog.authorize_run_policy(&request(&a.policy_file)?)?)?
        }
        NativeVideoCommand::Pilot(a) => {
            let cancel = CommandCancellation::arm()?;
            serde_json::to_value(
                catalog.run_native_video_pilot(&request(&a.policy_file)?, &|| {
                    cancel.flow().is_break()
                })?,
            )?
        }
        NativeVideoCommand::Prepare {
            policy_file,
            window_file,
        } => {
            let req = request(policy_file)?;
            let policy = catalog.describe_run_policy(&req)?;
            let window: NativeVideoWindowRequest = read_bounded(window_file, WINDOWS_LIMIT)?;
            let cancel = CommandCancellation::arm()?;
            let media = catalog
                .prepare_native_video_window(&policy, &window, &|| cancel.flow().is_break())?;
            serde_json::to_value(media.view())?
        }
        NativeVideoCommand::Windows(a) => {
            let policy = catalog.describe_run_policy(&request(&a.policy_file)?)?;
            json!(rcdvlog_core::plan_native_video_windows(&policy)?)
        }
        NativeVideoCommand::Run {
            policy_id,
            windows_file,
        } => {
            let windows: Option<Vec<NativeVideoWindowRequest>> = windows_file
                .as_deref()
                .map(|p| read_bounded(p, WINDOWS_LIMIT))
                .transpose()?;
            let cancel = CommandCancellation::arm()?;
            let report = catalog.run_native_video_review(
                policy_id,
                windows.as_deref(),
                &|| cancel.flow().is_break(),
                &mut |progress| {
                    eprintln!(
                        "{}",
                        json!({"event":"native-video.progress","progress":progress})
                    )
                },
            )?;
            serde_json::to_value(report)?
        }
        NativeVideoCommand::Status { project, run } => {
            serde_json::to_value(catalog.native_video_run_status(project, run)?)?
        }
        NativeVideoCommand::Wait {
            project,
            run,
            timeout_seconds,
        } => {
            ensure!(*timeout_seconds <= 3600, "timeout must be at most one hour");
            serde_json::to_value(catalog.wait_native_video_run(
                project,
                run,
                Duration::from_secs(*timeout_seconds),
            )?)?
        }
        NativeVideoCommand::Cancel { project, run } => {
            json!({"cancelRequested":catalog.cancel_native_video_run(project,run)?})
        }
        NativeVideoCommand::Result { project, evidence } => {
            serde_json::to_value(catalog.native_video_window_result(project, evidence)?)?
        }
        NativeVideoCommand::Response { project, evidence } => {
            serde_json::to_value(catalog.native_video_raw_response(project, evidence)?)?
        }
        NativeVideoCommand::Recover { project, evidence } => {
            serde_json::to_value(catalog.recover_native_video_window(project, evidence)?)?
        }
        NativeVideoCommand::EvidenceList {
            project,
            policy_id,
            after,
            limit,
        } => {
            ensure!((1..=100).contains(limit), "limit must be 1-100");
            serde_json::to_value(catalog.list_native_video_windows(
                project,
                policy_id,
                after.as_deref(),
                *limit,
            )?)?
        }
        NativeVideoCommand::RunWindows {
            project,
            run,
            after_index,
            limit,
        } => {
            ensure!((1..=100).contains(limit), "limit must be 1-100");
            serde_json::to_value(catalog.list_native_video_run_windows(
                project,
                run,
                *after_index,
                *limit,
            )?)?
        }
    })
}
