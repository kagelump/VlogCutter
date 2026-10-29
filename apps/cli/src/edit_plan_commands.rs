//! Thin supported adapter for immutable edit plans. Core owns all plan writes.
use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand};
use rcdvlog_core::{
    AcceptedReviewReference, ApplyReviewFeedbackRequest, AssembledContextRequest,
    AudioIntegrityRequest, Catalog, ComparisonPreferenceRequest, ConfirmSourceRangeRequest,
    DecideReviewFeedbackRequest, EditPlanOperation, MapReviewTargetRequest, PreviewExcerptRequest,
    PreviewRequest, ProposeReviewChangeRequest, RecordReviewNoteRequest, SequenceSelectionInput,
    SequenceSelectionRequest, ShortComparisonRequest, SourceInspectionRequest, SourceTrimRequest,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::Read;
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
    /// Render a bounded whole-cut excerpt for context review; never edits a plan.
    Excerpt {
        /// PreviewExcerptRequest JSON, capped at 64 KiB.
        #[arg(long)]
        input: PathBuf,
    },
    /// Assemble verified excerpt evidence and separately playable source handles.
    Context {
        #[command(subcommand)]
        command: AssembledContextCommand,
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
    /// Create and operate a bounded local reviewer-requested inspection session.
    ReviewSession {
        #[command(subcommand)]
        command: InspectionReviewCommand,
    },
    /// Record and apply explicit, revision-bound human review feedback.
    Feedback {
        #[command(subcommand)]
        command: ReviewFeedbackCommand,
    },
    /// Preview, create, or read versioned sequence selections.
    Selection {
        #[command(subcommand)]
        command: SelectionCommand,
    },
    /// Render and compare bounded local variants; preference is an explicit human action.
    Compare {
        #[command(subcommand)]
        command: ShortComparisonCommand,
    },
    /// Audit bounded source and rendered audio presence/energy without making editorial decisions.
    AudioAudit {
        #[command(subcommand)]
        command: AudioAuditCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum AudioAuditCommand {
    /// Create or reuse bounded selected-audio evidence for a revision scope.
    Create {
        /// AudioIntegrityRequest JSON, capped at 64 KiB.
        #[arg(long)]
        input: PathBuf,
    },
    /// Read historical audio evidence without rechecking current media.
    Get {
        #[arg(long)]
        evidence: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ShortComparisonCommand {
    /// Render 2-4 scoped variants and create a local sequential player.
    Create {
        #[arg(long)]
        input: PathBuf,
    },
    /// Read comparison state; --verify checks current media and player bytes.
    Status {
        #[arg(long)]
        comparison: String,
        #[arg(long)]
        verify: bool,
    },
    /// Cooperatively stop an active comparison render.
    Cancel {
        #[arg(long)]
        comparison: String,
    },
    /// Record an explicit human preference and optional retained cuts.
    Prefer {
        #[arg(long)]
        input: PathBuf,
    },
    /// Read a durable preference and draft receipt.
    Choice {
        #[arg(long)]
        choice: String,
    },
    /// Create/retrieve the inactive draft linked to a preference choice.
    Apply {
        #[arg(long)]
        choice: String,
        #[arg(long)]
        expected_active: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum InspectionReviewCommand {
    /// Bind a bounded session to one current source-inspection receipt.
    Create {
        #[arg(long)]
        input: PathBuf,
    },
    /// Run one typed inspection, trim suggestion, or advisory observation step.
    Step {
        #[arg(long)]
        input: PathBuf,
    },
    /// Read current session state without repairing or verifying media.
    Status {
        #[arg(long)]
        session: String,
    },
    /// Read a bounded page of durable session steps.
    Steps {
        #[arg(long)]
        session: String,
        #[arg(long)]
        after: Option<u32>,
        #[arg(long, default_value_t = 32)]
        limit: u32,
    },
    /// Stop a still-open session.
    Cancel {
        #[arg(long)]
        session: String,
    },
    /// Finish a still-open session after freshness checks.
    Finish {
        #[arg(long)]
        session: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum AssembledContextCommand {
    /// Create revision-bound review evidence from a verified excerpt.
    Create {
        #[arg(long)]
        input: PathBuf,
    },
    /// Read historical assembled evidence without certifying current media.
    Get {
        #[arg(long)]
        evidence: String,
    },
    /// Record a human note scoped to the viewed assembled context.
    Note {
        #[arg(long)]
        evidence: String,
        #[arg(long)]
        input: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum SelectionCommand {
    /// Propose sequence selections without changing the catalog.
    Preview(SelectionScopeArgs),
    /// Create an inactive edit-plan draft from sequence selections.
    Create(SelectionScopeArgs),
    /// Read selection metadata and report for one revision.
    Get {
        #[arg(long)]
        project: String,
        #[arg(long)]
        revision: String,
    },
}

#[derive(Debug, Args)]
pub struct SelectionScopeArgs {
    #[arg(long)]
    project: String,
    /// JSON file containing SequenceSelectionInput.
    #[arg(long)]
    input: PathBuf,
    /// Immutable base revision id, or literal 'none'.
    #[arg(long)]
    expected_base: String,
    /// Expected active revision id, or literal 'none'.
    #[arg(long)]
    expected_active: String,
}

#[derive(Debug, Subcommand)]
pub enum ReviewFeedbackCommand {
    Record {
        #[arg(long)]
        input: PathBuf,
    },
    Propose {
        #[arg(long)]
        input: PathBuf,
    },
    Decide {
        #[arg(long)]
        input: PathBuf,
    },
    Map {
        #[arg(long)]
        input: PathBuf,
    },
    Get {
        #[arg(long)]
        note: String,
        #[arg(long)]
        revision: String,
    },
    Report {
        #[arg(long)]
        project: String,
        #[arg(long)]
        revision: String,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    Apply {
        #[arg(long)]
        project: String,
        #[arg(long)]
        expected_base: String,
        #[arg(long)]
        expected_active: String,
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ApplyReviewFeedbackInput {
    accepted: Vec<AcceptedReviewReference>,
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
        EditPlanCommand::Excerpt { .. } => "edit-plan.excerpt",
        EditPlanCommand::Context { command } => match command {
            AssembledContextCommand::Create { .. } => "edit-plan.context.create",
            AssembledContextCommand::Get { .. } => "edit-plan.context.get",
            AssembledContextCommand::Note { .. } => "edit-plan.context.note",
        },
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
        EditPlanCommand::ReviewSession { command } => match command {
            InspectionReviewCommand::Create { .. } => "edit-plan.review-session.create",
            InspectionReviewCommand::Step { .. } => "edit-plan.review-session.step",
            InspectionReviewCommand::Status { .. } => "edit-plan.review-session.status",
            InspectionReviewCommand::Steps { .. } => "edit-plan.review-session.steps",
            InspectionReviewCommand::Cancel { .. } => "edit-plan.review-session.cancel",
            InspectionReviewCommand::Finish { .. } => "edit-plan.review-session.finish",
        },
        EditPlanCommand::Feedback { command } => match command {
            ReviewFeedbackCommand::Record { .. } => "edit-plan.feedback.record",
            ReviewFeedbackCommand::Propose { .. } => "edit-plan.feedback.propose",
            ReviewFeedbackCommand::Decide { .. } => "edit-plan.feedback.decide",
            ReviewFeedbackCommand::Map { .. } => "edit-plan.feedback.map",
            ReviewFeedbackCommand::Get { .. } => "edit-plan.feedback.get",
            ReviewFeedbackCommand::Report { .. } => "edit-plan.feedback.report",
            ReviewFeedbackCommand::Apply { .. } => "edit-plan.feedback.apply",
        },
        EditPlanCommand::Selection { command } => match command {
            SelectionCommand::Preview(_) => "edit-plan.selection.preview",
            SelectionCommand::Create(_) => "edit-plan.selection.create",
            SelectionCommand::Get { .. } => "edit-plan.selection.get",
        },
        EditPlanCommand::Compare { command } => match command {
            ShortComparisonCommand::Create { .. } => "edit-plan.compare.create",
            ShortComparisonCommand::Status { .. } => "edit-plan.compare.status",
            ShortComparisonCommand::Cancel { .. } => "edit-plan.compare.cancel",
            ShortComparisonCommand::Prefer { .. } => "edit-plan.compare.prefer",
            ShortComparisonCommand::Choice { .. } => "edit-plan.compare.choice",
            ShortComparisonCommand::Apply { .. } => "edit-plan.compare.apply",
        },
        EditPlanCommand::AudioAudit { command } => match command {
            AudioAuditCommand::Create { .. } => "edit-plan.audio-audit.create",
            AudioAuditCommand::Get { .. } => "edit-plan.audio-audit.get",
        },
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
            | EditPlanCommand::Feedback {
                command: ReviewFeedbackCommand::Get { .. } | ReviewFeedbackCommand::Report { .. },
            }
            | EditPlanCommand::Selection {
                command: SelectionCommand::Preview(_) | SelectionCommand::Get { .. },
            }
            | EditPlanCommand::ReviewSession {
                command: InspectionReviewCommand::Status { .. }
                    | InspectionReviewCommand::Steps { .. },
            }
            | EditPlanCommand::Context {
                command: AssembledContextCommand::Get { .. }
            }
            | EditPlanCommand::Compare {
                command: ShortComparisonCommand::Status { verify: false, .. }
                    | ShortComparisonCommand::Choice { .. }
            }
            | EditPlanCommand::AudioAudit {
                command: AudioAuditCommand::Get { .. }
            }
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
        EditPlanCommand::Excerpt { input } => {
            let request: PreviewExcerptRequest = read_review_input(input)?;
            Ok(serde_json::to_value(catalog.render_preview_excerpt(
                &request,
                &mut |event| eprintln!("{}", json!({ "event": "preview-progress", "data": event })),
                &|| false,
            )?)?)
        }
        EditPlanCommand::Context { command } => match command {
            AssembledContextCommand::Create { input } => {
                let request: AssembledContextRequest = read_review_input(input)?;
                Ok(serde_json::to_value(
                    catalog.assembled_review_context(&request, &|| false)?,
                )?)
            }
            AssembledContextCommand::Get { evidence } => Ok(serde_json::to_value(
                catalog.assembled_review_evidence(evidence)?,
            )?),
            AssembledContextCommand::Note { evidence, input } => {
                let request: RecordReviewNoteRequest = read_review_input(input)?;
                Ok(serde_json::to_value(
                    catalog.record_assembled_review_note(evidence, &request)?,
                )?)
            }
        },
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
            let request: SourceInspectionRequest = read_review_input(input)?;
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
        EditPlanCommand::ReviewSession { command } => match command {
            InspectionReviewCommand::Create { input } => {
                let request = read_review_input(input)?;
                Ok(serde_json::to_value(
                    catalog.create_inspection_review(&request)?,
                )?)
            }
            InspectionReviewCommand::Step { input } => {
                let request = read_review_input(input)?;
                Ok(serde_json::to_value(
                    catalog.inspection_review_step(&request, &|| false)?,
                )?)
            }
            InspectionReviewCommand::Status { session } => Ok(serde_json::to_value(
                catalog.inspection_review_status(session)?,
            )?),
            InspectionReviewCommand::Steps {
                session,
                after,
                limit,
            } => Ok(serde_json::to_value(
                catalog.inspection_review_steps(session, *after, *limit)?,
            )?),
            InspectionReviewCommand::Cancel { session } => Ok(serde_json::to_value(
                catalog.cancel_inspection_review(session)?,
            )?),
            InspectionReviewCommand::Finish { session } => Ok(serde_json::to_value(
                catalog.finish_inspection_review(session)?,
            )?),
        },
        EditPlanCommand::Feedback { command } => match command {
            ReviewFeedbackCommand::Record { input } => {
                let request: RecordReviewNoteRequest = read_review_input(input)?;
                Ok(serde_json::to_value(catalog.record_review_note(&request)?)?)
            }
            ReviewFeedbackCommand::Propose { input } => {
                let request: ProposeReviewChangeRequest = read_review_input(input)?;
                Ok(serde_json::to_value(
                    catalog.propose_review_change(&request)?,
                )?)
            }
            ReviewFeedbackCommand::Decide { input } => {
                let request: DecideReviewFeedbackRequest = read_review_input(input)?;
                Ok(serde_json::to_value(
                    catalog.decide_review_feedback(&request)?,
                )?)
            }
            ReviewFeedbackCommand::Map { input } => {
                let request: MapReviewTargetRequest = read_review_input(input)?;
                Ok(serde_json::to_value(catalog.map_review_target(&request)?)?)
            }
            ReviewFeedbackCommand::Get { note, revision } => Ok(serde_json::to_value(
                catalog.review_feedback(note, revision)?,
            )?),
            ReviewFeedbackCommand::Report {
                project,
                revision,
                offset,
                limit,
            } => Ok(serde_json::to_value(
                catalog.review_feedback_report(project, revision, *offset, *limit)?,
            )?),
            ReviewFeedbackCommand::Apply {
                project,
                expected_base,
                expected_active,
                input,
            } => {
                ensure!(
                    !expected_base.trim().is_empty() && expected_base != "none",
                    "invalid input: feedback application requires an existing --expected-base revision"
                );
                let parsed: ApplyReviewFeedbackInput = read_review_input(input)?;
                Ok(serde_json::to_value(catalog.apply_review_feedback(
                    &ApplyReviewFeedbackRequest {
                        project_id: project.clone(),
                        base_revision_id: expected_base.clone(),
                        expected_active_revision_id: expected(expected_active).map(str::to_owned),
                        accepted: parsed.accepted,
                    },
                )?)?)
            }
        },
        EditPlanCommand::Selection { command } => match command {
            SelectionCommand::Preview(args) => {
                let request = selection_request(args, read_selection_input(&args.input)?);
                Ok(serde_json::to_value(
                    catalog.preview_sequence_selection(&request)?,
                )?)
            }
            SelectionCommand::Create(args) => {
                let request = selection_request(args, read_selection_input(&args.input)?);
                Ok(serde_json::to_value(
                    catalog.create_sequence_selection(&request)?,
                )?)
            }
            SelectionCommand::Get { project, revision } => {
                let revision_view = catalog.edit_plan_revision(revision)?;
                ensure!(
                    revision_view.project_id == *project,
                    "edit plan revision belongs to another project"
                );
                Ok(serde_json::to_value(
                    catalog.edit_plan_selection(revision)?,
                )?)
            }
        },
        EditPlanCommand::Compare { command } => {
            match command {
                ShortComparisonCommand::Create { input } => {
                    let request: ShortComparisonRequest = read_review_input(input)?;
                    Ok(serde_json::to_value(catalog.create_short_comparison(
                    &request,
                    &|| false,
                    &mut |variant, event| eprintln!("{}", json!({
                        "event": "comparison-progress", "variant": variant, "data": event
                    })),
                )?)?)
                }
                ShortComparisonCommand::Status { comparison, verify } => Ok(serde_json::to_value(
                    catalog.short_comparison_status(comparison, *verify)?,
                )?),
                ShortComparisonCommand::Cancel { comparison } => Ok(serde_json::to_value(
                    catalog.cancel_short_comparison(comparison)?,
                )?),
                ShortComparisonCommand::Prefer { input } => {
                    let request: ComparisonPreferenceRequest = read_review_input(input)?;
                    Ok(serde_json::to_value(
                        catalog.prefer_short_comparison(&request)?,
                    )?)
                }
                ShortComparisonCommand::Choice { choice } => {
                    Ok(serde_json::to_value(catalog.comparison_choice(choice)?)?)
                }
                ShortComparisonCommand::Apply {
                    choice,
                    expected_active,
                } => Ok(serde_json::to_value(
                    catalog.apply_comparison_choice(choice, expected_active)?,
                )?),
            }
        }
        EditPlanCommand::AudioAudit { command } => match command {
            AudioAuditCommand::Create { input } => {
                let request: AudioIntegrityRequest = read_review_input(input)?;
                Ok(serde_json::to_value(
                    catalog.audit_selected_audio_with_progress(
                        &request,
                        &|| false,
                        &mut |event| {
                            eprintln!(
                                "{}",
                                json!({ "event": "audio-audit-progress", "data": event })
                            )
                        },
                    )?,
                )?)
            }
            AudioAuditCommand::Get { evidence } => Ok(serde_json::to_value(
                catalog.audio_integrity_evidence(evidence)?,
            )?),
        },
    }
}

fn selection_request(
    args: &SelectionScopeArgs,
    input: SequenceSelectionInput,
) -> SequenceSelectionRequest {
    SequenceSelectionRequest {
        project_id: args.project.clone(),
        expected_base_revision_id: expected(&args.expected_base).map(str::to_owned),
        expected_active_revision_id: expected(&args.expected_active).map(str::to_owned),
        input,
    }
}

fn read_selection_input(input: &PathBuf) -> Result<SequenceSelectionInput> {
    let mut bytes = Vec::new();
    std::fs::File::open(input)
        .context("invalid input: open sequence selection JSON")?
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .context("invalid input: read sequence selection JSON")?;
    ensure!(
        bytes.len() <= 8 * 1024 * 1024,
        "invalid input: sequence selection JSON exceeds 8 MiB"
    );
    serde_json::from_slice(&bytes).context("invalid input: parse sequence selection JSON")
}

fn read_review_input<T: serde::de::DeserializeOwned>(input: &PathBuf) -> Result<T> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(input)
        .context("invalid input: open review JSON")?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .context("invalid input: read review JSON")?;
    ensure!(
        bytes.len() <= 64 * 1024,
        "invalid input: review JSON exceeds 64 KiB"
    );
    serde_json::from_slice(&bytes).context("invalid input: parse review JSON")
}
