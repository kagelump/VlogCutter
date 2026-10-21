//! The chronology and diagnostics CLI surface (D01-T017).
//!
//! Thin chronology adapters over the owning core APIs. Reads and previews are
//! read-only; confirmed changes use typed core operations and tokened previews.
//!
//! Every command returns the *same* solved values the desktop shows, because
//! both call the same pure solver. `chronology status` deliberately includes the
//! ordered asset id list, which makes a desktop/CLI order disagreement a
//! one-line diff instead of an argument.

use anyhow::Result;
use clap::Subcommand;
use rcdvlog_core::{
    CaptureSequence, Catalog, ChronologyMutation, ChronologyMutationPreview, ClipTimeException,
    DiagnosticFilter, DiagnosticSeverity, EventOrderConstraint,
};
use serde_json::{Value, json};
use std::io::Read;

#[derive(Debug, Subcommand)]
pub enum ChronologyCommand {
    /// The project's capture sequences, and the splits the evidence suggests.
    Sequences {
        #[arg(long)]
        project: String,
    },
    /// Clock epochs, optionally narrowed to one capture sequence.
    Epochs {
        #[arg(long)]
        project: String,
        #[arg(long)]
        sequence: Option<String>,
    },
    /// Trip phases, in start-time order.
    Phases {
        #[arg(long)]
        project: String,
    },
    /// Per-clip time overrides.
    Exceptions {
        #[arg(long)]
        project: String,
    },
    /// Event-order constraints.
    Constraints {
        #[arg(long)]
        project: String,
    },
    /// Facts that cannot all be true at once.
    Contradictions {
        #[arg(long)]
        project: String,
    },
    /// Why one clip landed where it did.
    Explain {
        #[arg(long)]
        asset: String,
    },
    /// Fact counts, unplaced clips, contradictions, and the full ordered
    /// asset id list.
    Status {
        #[arg(long)]
        project: String,
    },
    /// Export a bounded, revision-pinned page of full chronology evidence.
    Evidence {
        #[arg(long)]
        project: String,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long)]
        facts_revision: Option<String>,
    },
    /// Review persisted clock-block alternatives and anchor evidence.
    Questions {
        #[arg(long)]
        project: String,
    },
    /// Preview a typed chronology change from JSON stdin or a request file.
    Preview {
        #[arg(long)]
        project: String,
        /// JSON file, or - to read stdin. The request has a `kind` field.
        #[arg(long)]
        input: String,
    },
    /// Apply an unchanged core preview or a direct typed chronology operation.
    Apply {
        #[arg(long)]
        project: String,
        /// JSON file, or - to read stdin. Preview requests include the returned preview.
        #[arg(long)]
        input: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum DiagnosticsCommand {
    /// Conditions the user needs to know about.
    List {
        #[arg(long)]
        project: String,
        #[arg(long, value_parser = ["error", "warning", "info"])]
        severity: Option<String>,
        #[arg(long)]
        kind_prefix: Option<String>,
        #[arg(long)]
        include_acknowledged: bool,
    },
    /// Mark one diagnostic as seen. The row is never deleted, and the
    /// condition happening again resurfaces it.
    Acknowledge {
        #[arg(long)]
        project: String,
        #[arg(long)]
        diagnostic: String,
    },
}

pub fn chronology_command_name(command: &ChronologyCommand) -> &'static str {
    match command {
        ChronologyCommand::Sequences { .. } => "chronology.sequences",
        ChronologyCommand::Epochs { .. } => "chronology.epochs",
        ChronologyCommand::Phases { .. } => "chronology.phases",
        ChronologyCommand::Exceptions { .. } => "chronology.exceptions",
        ChronologyCommand::Constraints { .. } => "chronology.constraints",
        ChronologyCommand::Contradictions { .. } => "chronology.contradictions",
        ChronologyCommand::Explain { .. } => "chronology.explain",
        ChronologyCommand::Status { .. } => "chronology.status",
        ChronologyCommand::Evidence { .. } => "chronology.evidence",
        ChronologyCommand::Questions { .. } => "chronology.questions",
        ChronologyCommand::Preview { .. } => "chronology.preview",
        ChronologyCommand::Apply { .. } => "chronology.apply",
    }
}

pub fn diagnostics_command_name(command: &DiagnosticsCommand) -> &'static str {
    match command {
        DiagnosticsCommand::List { .. } => "diagnostics.list",
        DiagnosticsCommand::Acknowledge { .. } => "diagnostics.acknowledge",
    }
}

/// Read-only commands use a read-only catalog connection; only explicit apply
/// opens the catalog for typed chronology writes.
pub fn chronology_is_read_only(command: &ChronologyCommand) -> bool {
    !matches!(command, ChronologyCommand::Apply { .. })
}

pub fn diagnostics_is_read_only(command: &DiagnosticsCommand) -> bool {
    matches!(command, DiagnosticsCommand::List { .. })
}

pub fn execute_chronology(catalog: &Catalog, command: &ChronologyCommand) -> Result<Value> {
    match command {
        ChronologyCommand::Sequences { project } => {
            let facts = catalog.chronology_facts(project)?;
            let proposals = catalog.propose_capture_sequence_splits(project)?;
            Ok(json!({
                "sequences": facts.sequences,
                "proposedSplits": proposals
                    .into_iter()
                    .map(|proposal| json!({
                        "captureSequenceId": proposal.capture_sequence_id,
                        "atCounter": proposal.at_counter,
                        "reason": proposal.reason,
                        "affectedAssetIds": proposal.affected_asset_ids,
                    }))
                    .collect::<Vec<_>>(),
            }))
        }
        ChronologyCommand::Epochs { project, sequence } => {
            let epochs = catalog.chronology_facts(project)?.epochs;
            let filtered: Vec<_> = epochs
                .into_iter()
                .filter(|epoch| {
                    sequence
                        .as_deref()
                        .is_none_or(|wanted| epoch.capture_sequence_id == wanted)
                })
                .collect();
            Ok(serde_json::to_value(filtered)?)
        }
        ChronologyCommand::Phases { project } => {
            Ok(serde_json::to_value(catalog.trip_phases(project)?)?)
        }
        ChronologyCommand::Exceptions { project } => Ok(serde_json::to_value(
            catalog.chronology_facts(project)?.exceptions,
        )?),
        ChronologyCommand::Constraints { project } => Ok(serde_json::to_value(
            catalog.chronology_facts(project)?.constraints,
        )?),
        ChronologyCommand::Contradictions { project } => {
            Ok(serde_json::to_value(catalog.contradictions(project)?)?)
        }
        ChronologyCommand::Explain { asset } => {
            Ok(serde_json::to_value(catalog.explain_chronology(asset)?)?)
        }
        ChronologyCommand::Status { project } => {
            let facts = catalog.chronology_facts(project)?;
            let solution = catalog.solve_chronology(project)?;
            let unplaced = solution
                .assets
                .iter()
                .filter(|entry| entry.trip_time.is_none())
                .count();
            let hard = solution
                .contradictions
                .iter()
                .filter(|found| found.strength == rcdvlog_core::ConstraintStrength::Hard)
                .count();
            Ok(json!({
                "counts": {
                    "assets": facts.assets.len(),
                    "sequences": facts.sequences.len(),
                    "epochs": facts.epochs.len(),
                    "exceptions": facts.exceptions.len(),
                    "phases": facts.phases.len(),
                    "constraints": facts.constraints.len(),
                },
                "unplacedAssets": unplaced,
                "contradictions": { "hard": hard, "soft": solution.contradictions.len() - hard },
                // The ordered list is the point: comparing desktop and CLI order
                // should be a diff, not a discussion.
                "orderedAssetIds": solution
                    .assets
                    .iter()
                    .map(|entry| entry.asset_id.as_str())
                    .collect::<Vec<_>>(),
            }))
        }
        ChronologyCommand::Evidence {
            project,
            offset,
            limit,
            facts_revision,
        } => Ok(serde_json::to_value(catalog.chronology_evidence_page(
            project,
            facts_revision.as_deref(),
            *offset,
            *limit,
        )?)?),
        ChronologyCommand::Questions { project } => {
            let snapshot = catalog.chronology_snapshot(project)?;
            Ok(json!({
                "openQuestions": snapshot.clock_questions,
                "humanDecisions": snapshot.clock_question_decisions,
            }))
        }
        ChronologyCommand::Preview { project, input } => {
            let request = read_json(input)?;
            let kind = request["kind"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("request.kind is required"))?;
            let data = match kind {
                "split" => {
                    let asset_ids: Vec<String> =
                        serde_json::from_value(request["assetIds"].clone())?;
                    serde_json::to_value(
                        catalog.preview_capture_sequence_split(
                            project,
                            request["sourceSequenceId"]
                                .as_str()
                                .ok_or_else(|| anyhow::anyhow!("sourceSequenceId is required"))?,
                            request["atCounter"]
                                .as_u64()
                                .ok_or_else(|| anyhow::anyhow!("atCounter is required"))?,
                            request["label"].as_str().unwrap_or(""),
                            &asset_ids,
                        )?,
                    )?
                }
                "reassign" => {
                    let asset_ids: Vec<String> =
                        serde_json::from_value(request["assetIds"].clone())?;
                    serde_json::to_value(
                        catalog.preview_capture_sequence_assets(
                            project,
                            request["fromSequenceId"]
                                .as_str()
                                .ok_or_else(|| anyhow::anyhow!("fromSequenceId is required"))?,
                            request["intoSequenceId"]
                                .as_str()
                                .ok_or_else(|| anyhow::anyhow!("intoSequenceId is required"))?,
                            &asset_ids,
                        )?,
                    )?
                }
                "merge" => serde_json::to_value(
                    catalog.preview_capture_sequence_merge(
                        project,
                        request["fromSequenceId"]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("fromSequenceId is required"))?,
                        request["intoSequenceId"]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("intoSequenceId is required"))?,
                    )?,
                )?,
                "epochSet" => serde_json::to_value(catalog.preview_clock_epoch_impact(
                    project,
                    &serde_json::from_value(request["epoch"].clone())?,
                )?)?,
                "epochClear" => serde_json::to_value(
                    catalog.preview_clear_clock_epoch_impact(
                        project,
                        request["epochId"]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("epochId is required"))?,
                    )?,
                )?,
                "questionDecision" => {
                    serde_json::to_value(catalog.preview_clock_question_decision(
                        project,
                        &serde_json::from_value(request["decision"].clone())?,
                    )?)?
                }
                "questionPropose" => serde_json::to_value(catalog.preview_clock_block_question(
                    project,
                    &serde_json::from_value(request["question"].clone())?,
                )?)?,
                "sequenceDeclare" | "sameCard" | "sameCardRestore" | "exceptionSet"
                | "exceptionClear" | "phaseSet" | "phaseRemove" | "phaseSplit"
                | "constraintSet" | "constraintClear" => {
                    let mutation = parse_direct_mutation(kind, &request)?;
                    serde_json::to_value(catalog.preview_chronology_mutation(project, &mutation)?)?
                }
                _ => anyhow::bail!("unsupported preview kind: {kind}"),
            };
            Ok(json!({"kind": kind, "data": data}))
        }
        ChronologyCommand::Apply { project, input } => {
            let request = read_json(input)?;
            let kind = request["kind"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("request.kind is required"))?;
            let data = match kind {
                "split" => serde_json::to_value(catalog.apply_capture_sequence_split(
                    project,
                    &serde_json::from_value(request["preview"].clone())?,
                )?)?,
                "reassign" => serde_json::to_value(catalog.apply_capture_sequence_reassignment(
                    project,
                    &serde_json::from_value(request["preview"].clone())?,
                )?)?,
                "merge" => {
                    catalog.apply_capture_sequence_merge(
                        project,
                        &serde_json::from_value(request["preview"].clone())?,
                    )?;
                    json!({"applied": true})
                }
                "epochSet" => serde_json::to_value(catalog.apply_clock_epoch_preview(
                    project,
                    &serde_json::from_value(request["preview"].clone())?,
                )?)?,
                "epochClear" => {
                    catalog.apply_clear_clock_epoch_preview(
                        project,
                        &serde_json::from_value(request["preview"].clone())?,
                    )?;
                    json!({"applied": true})
                }
                "questionPropose" => {
                    let revision = catalog.apply_clock_block_question_preview(
                        project,
                        &serde_json::from_value(request["preview"].clone())?,
                    )?;
                    json!({"revision": revision})
                }
                "questionDecision" => {
                    serde_json::to_value(catalog.apply_clock_question_decision(
                        project,
                        &serde_json::from_value(request["preview"].clone())?,
                    )?)?
                }
                "sequenceDeclare" | "sameCard" | "sameCardRestore" | "exceptionSet"
                | "exceptionClear" | "phaseSet" | "phaseRemove" | "phaseSplit"
                | "constraintSet" | "constraintClear" => {
                    let preview: ChronologyMutationPreview =
                        serde_json::from_value(request["preview"].clone())?;
                    let preview_kind = direct_mutation_name(&preview.mutation);
                    anyhow::ensure!(
                        kind == preview_kind,
                        "preview kind does not match request kind"
                    );
                    serde_json::to_value(
                        catalog.apply_chronology_mutation_preview(project, &preview)?,
                    )?
                }
                _ => anyhow::bail!("unsupported apply kind: {kind}"),
            };
            Ok(json!({"kind": kind, "data": data}))
        }
    }
}

fn read_json(input: &str) -> Result<Value> {
    let mut bytes = Vec::new();
    if input == "-" {
        std::io::stdin().read_to_end(&mut bytes)?;
    } else {
        bytes = std::fs::read(input)?;
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn required_string<'a>(request: &'a Value, field: &str) -> Result<&'a str> {
    request[field]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("{field} is required"))
}

fn required_u64(request: &Value, field: &str) -> Result<u64> {
    request[field]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("{field} is required"))
}

fn parse_direct_mutation(kind: &str, request: &Value) -> Result<ChronologyMutation> {
    Ok(match kind {
        "sequenceDeclare" => ChronologyMutation::SequenceDeclare {
            sequence: CaptureSequence {
                id: String::new(),
                camera_key: request["cameraKey"].as_str().unwrap_or("").into(),
                label: request["label"].as_str().unwrap_or("").into(),
                volume_hint: request["volumeHint"].as_str().unwrap_or("").into(),
                filename_prefix: request["filenamePrefix"].as_str().unwrap_or("").into(),
            },
        },
        "sameCard" => ChronologyMutation::SameCard {
            sequence_id: required_string(request, "sequenceId")?.into(),
            at_counter: required_u64(request, "atCounter")?,
            reason: request["reason"].as_str().unwrap_or("").into(),
        },
        "sameCardRestore" => ChronologyMutation::SameCardRestore {
            sequence_id: required_string(request, "sequenceId")?.into(),
            at_counter: required_u64(request, "atCounter")?,
        },
        "exceptionSet" => ChronologyMutation::ExceptionSet {
            exception: ClipTimeException {
                asset_id: required_string(request, "assetId")?.into(),
                trip_time: required_string(request, "tripTime")?.into(),
                reason: request["reason"].as_str().unwrap_or("").into(),
            },
        },
        "exceptionClear" => ChronologyMutation::ExceptionClear {
            asset_id: required_string(request, "assetId")?.into(),
        },
        "phaseSet" => ChronologyMutation::PhaseSet {
            phase: serde_json::from_value(request["phase"].clone())?,
        },
        "phaseRemove" => ChronologyMutation::PhaseRemove {
            phase_id: required_string(request, "phaseId")?.into(),
        },
        "phaseSplit" => ChronologyMutation::PhaseSplit {
            existing_phase_id: required_string(request, "existingPhaseId")?.into(),
            boundary_asset_id: required_string(request, "boundaryAssetId")?.into(),
            direction: serde_json::from_value(request["direction"].clone())?,
            phase: serde_json::from_value(request["phase"].clone())?,
        },
        "constraintSet" => ChronologyMutation::ConstraintSet {
            constraint: EventOrderConstraint {
                id: String::new(),
                earlier_asset_id: required_string(request, "earlierAssetId")?.into(),
                later_asset_id: required_string(request, "laterAssetId")?.into(),
                strength: serde_json::from_value(request["strength"].clone())?,
                source: request["source"].as_str().unwrap_or("").into(),
            },
        },
        "constraintClear" => ChronologyMutation::ConstraintClear {
            constraint_id: required_string(request, "constraintId")?.into(),
        },
        _ => anyhow::bail!("unsupported direct chronology kind: {kind}"),
    })
}

fn direct_mutation_name(mutation: &ChronologyMutation) -> &'static str {
    match mutation {
        ChronologyMutation::SequenceDeclare { .. } => "sequenceDeclare",
        ChronologyMutation::SameCard { .. } => "sameCard",
        ChronologyMutation::SameCardRestore { .. } => "sameCardRestore",
        ChronologyMutation::ExceptionSet { .. } => "exceptionSet",
        ChronologyMutation::ExceptionClear { .. } => "exceptionClear",
        ChronologyMutation::PhaseSet { .. } => "phaseSet",
        ChronologyMutation::PhaseRemove { .. } => "phaseRemove",
        ChronologyMutation::PhaseSplit { .. } => "phaseSplit",
        ChronologyMutation::ConstraintSet { .. } => "constraintSet",
        ChronologyMutation::ConstraintClear { .. } => "constraintClear",
    }
}

pub fn execute_diagnostics(catalog: &Catalog, command: &DiagnosticsCommand) -> Result<Value> {
    match command {
        DiagnosticsCommand::List {
            project,
            severity,
            kind_prefix,
            include_acknowledged,
        } => {
            let filter = DiagnosticFilter {
                severity_at_least: severity.as_deref().and_then(DiagnosticSeverity::parse),
                kind_prefix: kind_prefix.clone(),
                include_acknowledged: *include_acknowledged,
            };
            let found = catalog.diagnostics(project, &filter)?;
            let counts = catalog.diagnostic_counts(project)?;
            Ok(json!({ "diagnostics": found, "unacknowledgedCounts": counts }))
        }
        DiagnosticsCommand::Acknowledge {
            project,
            diagnostic,
        } => {
            catalog.acknowledge_diagnostic(project, diagnostic)?;
            Ok(json!({ "acknowledged": diagnostic }))
        }
    }
}
