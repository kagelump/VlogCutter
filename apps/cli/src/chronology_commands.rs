//! The chronology and diagnostics CLI surface (D01-T017).
//!
//! Read-only over project facts, with one deliberate exception:
//! `diagnostics acknowledge`. Acknowledging an observation changes no project
//! fact, emits no operation, and cannot alter derived order — it is the
//! exception, not a precedent for chronology mutation from the CLI.
//!
//! Every command returns the *same* solved values the desktop shows, because
//! both call the same pure solver. `chronology status` deliberately includes the
//! ordered asset id list, which makes a desktop/CLI order disagreement a
//! one-line diff instead of an argument.

use anyhow::Result;
use clap::Subcommand;
use rcdvlog_core::{Catalog, DiagnosticFilter, DiagnosticSeverity};
use serde_json::{Value, json};

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
    }
}

pub fn diagnostics_command_name(command: &DiagnosticsCommand) -> &'static str {
    match command {
        DiagnosticsCommand::List { .. } => "diagnostics.list",
        DiagnosticsCommand::Acknowledge { .. } => "diagnostics.acknowledge",
    }
}

/// Only `diagnostics acknowledge` writes, so everything else can be served from
/// a read-only catalog open — which is also the strongest possible statement
/// that these commands cannot corrupt a project.
pub fn chronology_is_read_only(_command: &ChronologyCommand) -> bool {
    true
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
