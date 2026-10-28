//! Thin supported adapter for immutable edit plans. Core owns all plan writes.
use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand};
use rcdvlog_core::{
    Catalog, ConfirmSourceRangeRequest, EditPlanOperation, PreviewRequest, SourceInspectionRequest,
    SourceTrimRequest,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum EditPlanCommand {
    /// List immutable revisions and the active revision.
    List {
        #[arg(long)]
        project: String,
    },
    /// Read one immutable revision.
    Get(RevisionArgs),
    /// Create a complete ordered draft; never activates it.
    Create {
        #[arg(long)]
        project: String,
        /// JSON object containing operations and optional scriptRevisionId.
        #[arg(long)]
        input: PathBuf,
        /// Immutable parent revision ID, or the literal 'none'.
        #[arg(long)]
        expected_base: String,
        /// Expected active revision ID, or the literal 'none'.
        #[arg(long)]
        expected_active: String,
    },
    /// Compare cut identity, source ranges, ordering, and rationale.
    Diff {
        #[arg(long)]
        base: String,
        #[arg(long)]
        revision: String,
    },
    /// Return blocking errors and advisory warnings without changing state.
    Validate(RevisionArgs),
    /// Activate a valid draft only if the expected active revision still matches.
    Activate {
        #[arg(long)]
        revision: String,
        #[arg(long)]
        expected_active: String,
    },
    /// Render a local preview without activating the revision.
    Preview {
        #[arg(long)]
        revision: String,
        #[arg(long, default_value_t = 360, value_parser = clap::value_parser!(u32).range(144..=1080))]
        max_height: u32,
    },
    /// Read one immutable preview attempt; --verify explicitly checks current media bytes.
    PreviewStatus {
        #[arg(long)]
        artifact: String,
        #[arg(long)]
        verify: bool,
    },
    /// List bounded preview attempt summaries for an immutable revision.
    PreviewList {
        #[arg(long)]
        revision: String,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Cooperatively stop one owning preview attempt and reap its media children.
    PreviewCancel {
        #[arg(long)]
        artifact: String,
    },
    /// Quiet observation; timing out never cancels or replaces the original render.
    PreviewWait {
        #[arg(long)]
        artifact: String,
        #[arg(long, default_value_t = 600)]
        timeout_seconds: u64,
        #[arg(long)]
        verify: bool,
    },
    /// Inspect oriented actual frames and optional audio in a bounded source window.
    Inspect {
        /// SourceInspectionRequest JSON; never chooses a fallback representation.
        #[arg(long)]
        input: PathBuf,
    },
    /// Read or explicitly confirm/revoke human-owned source constraints.
    Ranges {
        #[command(subcommand)]
        command: SourceRangeCommand,
    },
    /// Suggest a clean/utterance trim using current inspection and human constraints.
    SuggestTrim {
        #[arg(long)]
        input: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum SourceRangeCommand {
    List {
        #[arg(long)]
        asset: String,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    Confirm {
        #[arg(long)]
        input: PathBuf,
    },
    Revoke {
        #[arg(long)]
        asset: String,
        #[arg(long)]
        constraint: String,
        #[arg(long)]
        expected_revision: u64,
        #[arg(long)]
        rationale: String,
    },
}

#[derive(Debug, Args)]
pub struct RevisionArgs {
    #[arg(long)]
    revision: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PlanInput {
    operations: Vec<EditPlanOperation>,
    #[serde(default)]
    script_revision_id: Option<String>,
}

pub fn command_name(command: &EditPlanCommand) -> &'static str {
    match command {
        EditPlanCommand::List { .. } => "edit-plan.list",
        EditPlanCommand::Get(_) => "edit-plan.get",
        EditPlanCommand::Create { .. } => "edit-plan.create",
        EditPlanCommand::Diff { .. } => "edit-plan.diff",
        EditPlanCommand::Validate(_) => "edit-plan.validate",
        EditPlanCommand::Activate { .. } => "edit-plan.activate",
        EditPlanCommand::Preview { .. } => "edit-plan.preview",
        EditPlanCommand::PreviewStatus { .. } => "edit-plan.preview-status",
        EditPlanCommand::PreviewList { .. } => "edit-plan.preview-list",
        EditPlanCommand::PreviewCancel { .. } => "edit-plan.preview-cancel",
        EditPlanCommand::PreviewWait { .. } => "edit-plan.preview-wait",
        EditPlanCommand::Inspect { .. } => "edit-plan.inspect",
        EditPlanCommand::Ranges { command } => match command {
            SourceRangeCommand::List { .. } => "edit-plan.ranges.list",
            SourceRangeCommand::Confirm { .. } => "edit-plan.ranges.confirm",
            SourceRangeCommand::Revoke { .. } => "edit-plan.ranges.revoke",
        },
        EditPlanCommand::SuggestTrim { .. } => "edit-plan.suggest-trim",
    }
}

pub fn is_read_only(command: &EditPlanCommand) -> bool {
    matches!(
        command,
        EditPlanCommand::List { .. }
            | EditPlanCommand::PreviewStatus { verify: false, .. }
            | EditPlanCommand::PreviewList { .. }
            | EditPlanCommand::PreviewWait { verify: false, .. }
            | EditPlanCommand::Ranges {
                command: SourceRangeCommand::List { .. }
            }
            | EditPlanCommand::Get(_)
            | EditPlanCommand::Diff { .. }
            | EditPlanCommand::Validate(_)
    )
}

fn expected(value: &str) -> Option<&str> {
    (value != "none").then_some(value)
}

pub fn execute(catalog: &Catalog, command: &EditPlanCommand) -> Result<Value> {
    match command {
        EditPlanCommand::List { project } => Ok(json!({
            "activeRevisionId": catalog.active_edit_plan(project)?.map(|plan| plan.revision_id),
            "revisions": catalog.edit_plan_revisions(project)?,
        })),
        EditPlanCommand::Get(args) => Ok(serde_json::to_value(
            catalog.edit_plan_revision(&args.revision)?,
        )?),
        EditPlanCommand::Create {
            project,
            input,
            expected_base,
            expected_active,
        } => {
            let bytes = std::fs::read(input).context("invalid input: read edit-plan JSON")?;
            ensure!(
                bytes.len() <= 4 * 1024 * 1024,
                "invalid input: plan JSON exceeds 4 MiB"
            );
            let plan: PlanInput =
                serde_json::from_slice(&bytes).context("invalid input: parse edit-plan JSON")?;
            ensure!(
                plan.operations.len() <= 10_000,
                "invalid input: maximum 10000 cuts"
            );
            ensure!(
                plan.operations
                    .iter()
                    .enumerate()
                    .all(|(i, cut)| cut.position as usize == i),
                "invalid input: positions must equal ordered zero-based array indices"
            );
            Ok(serde_json::to_value(catalog.create_edit_plan_from_base(
                project,
                &plan.operations,
                plan.script_revision_id.as_deref(),
                expected(expected_base),
                expected(expected_active),
            )?)?)
        }
        EditPlanCommand::Diff { base, revision } => Ok(serde_json::to_value(
            catalog.diff_edit_plans(base, revision)?,
        )?),
        EditPlanCommand::Validate(args) => {
            let report = catalog.validate_edit_plan(&args.revision)?;
            Ok(
                json!({ "revisionId": args.revision, "renderable": report.renderable(), "findings": report.findings }),
            )
        }
        EditPlanCommand::Activate {
            revision,
            expected_active,
        } => Ok(serde_json::to_value(catalog.activate_edit_plan_revision(
            revision,
            expected(expected_active),
        )?)?),
        EditPlanCommand::Preview {
            revision,
            max_height,
        } => {
            let result = catalog.render_preview(
                &PreviewRequest {
                    revision_id: revision.clone(),
                    max_height: *max_height,
                },
                &mut |event| eprintln!("{}", json!({ "event": "preview-progress", "data": event })),
                &|| false,
            )?;
            Ok(json!({ "revisionId": revision, "preview": result }))
        }
        EditPlanCommand::PreviewStatus { artifact, verify } => Ok(serde_json::to_value(
            catalog.preview_artifact_status(artifact, *verify)?,
        )?),
        EditPlanCommand::PreviewList {
            revision,
            offset,
            limit,
        } => Ok(serde_json::to_value(
            catalog.preview_artifacts(revision, *offset, *limit)?,
        )?),
        EditPlanCommand::PreviewCancel { artifact } => Ok(serde_json::to_value(
            catalog.cancel_preview_artifact(artifact)?,
        )?),
        EditPlanCommand::PreviewWait {
            artifact,
            timeout_seconds,
            verify,
        } => {
            let observed = catalog.wait_preview_artifact(
                artifact,
                std::time::Duration::from_secs(*timeout_seconds),
            )?;
            Ok(serde_json::to_value(if *verify {
                catalog.preview_artifact_status(artifact, true)?
            } else {
                observed
            })?)
        }
        EditPlanCommand::Inspect { input } => {
            let bytes = std::fs::read(input).context("invalid input: read inspection JSON")?;
            ensure!(
                bytes.len() <= 64 * 1024,
                "invalid input: inspection JSON exceeds 64 KiB"
            );
            let request: SourceInspectionRequest = serde_json::from_slice(&bytes)
                .context("invalid input: parse source inspection JSON")?;
            Ok(serde_json::to_value(
                catalog.inspect_source_window(&request, &|| false)?,
            )?)
        }
        EditPlanCommand::Ranges { command } => match command {
            SourceRangeCommand::List {
                asset,
                offset,
                limit,
            } => Ok(serde_json::to_value(
                catalog.source_range_constraints(asset, *offset, *limit)?,
            )?),
            SourceRangeCommand::Confirm { input } => {
                let request: ConfirmSourceRangeRequest = read_review_input(input)?;
                Ok(serde_json::to_value(
                    catalog.confirm_source_range(&request)?,
                )?)
            }
            SourceRangeCommand::Revoke {
                asset,
                constraint,
                expected_revision,
                rationale,
            } => Ok(serde_json::to_value(catalog.revoke_source_range(
                asset,
                constraint,
                *expected_revision,
                rationale,
            )?)?),
        },
        EditPlanCommand::SuggestTrim { input } => {
            let request: SourceTrimRequest = read_review_input(input)?;
            Ok(serde_json::to_value(
                catalog.suggest_source_trim(&request)?,
            )?)
        }
    }
}

fn read_review_input<T: serde::de::DeserializeOwned>(input: &PathBuf) -> Result<T> {
    let bytes = std::fs::read(input).context("invalid input: read review JSON")?;
    ensure!(
        bytes.len() <= 64 * 1024,
        "invalid input: review JSON exceeds 64 KiB"
    );
    serde_json::from_slice(&bytes).context("invalid input: parse review JSON")
}
