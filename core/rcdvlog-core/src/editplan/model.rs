//! Edit-plan record types, frozen by D01-T019
//! (`docs/tasks/desktop/EDIT_PLAN_MODEL.md`).
//!
//! The editing scope is exhaustively one video track, linked source audio, hard
//! cuts, source in/out ranges, and ordered operations. There is no track field,
//! no transition, and no effect — not even reserved. Anything a later feature
//! would need is a later migration with a later contract.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One clip in the plan: a source range, at a position, with the reason it is
/// there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditPlanOperation {
    pub operation_id: String,
    pub position: u32,
    pub asset_id: String,
    pub source_in_seconds: f64,
    pub source_out_seconds: f64,
    pub rationale: String,
}

/// One immutable version of the plan. Revisions are never edited in place and
/// never deleted, so a refused activation leaves its work recoverable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditPlanRevision {
    pub revision_id: String,
    pub project_id: String,
    pub script_revision_id: Option<String>,
    pub parent_revision_id: Option<String>,
    pub active: bool,
    pub operations: Vec<EditPlanOperation>,
    pub created_at: String,
}

impl EditPlanRevision {
    /// An empty plan is a legitimate state, so this reports emptiness rather
    /// than any caller treating it as an error.
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    /// Total runtime of the plan in seconds.
    pub fn duration_seconds(&self) -> f64 {
        self.operations
            .iter()
            .map(|operation| (operation.source_out_seconds - operation.source_in_seconds).max(0.0))
            .sum()
    }
}

/// Whether a revision was derived from a script revision that has since moved
/// on. Always derived, never stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditPlanStaleness {
    pub stale: bool,
    pub recorded_script_revision_id: Option<String>,
    pub active_script_revision_id: Option<String>,
}

/// One recorded export. History is append-only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditPlanExport {
    pub export_id: String,
    pub revision_id: String,
    pub output_path: String,
    pub output_digest: String,
    pub exported_at: String,
}

/// Why an edit-plan write was refused, with a stable code for every surface.
#[derive(Debug, Error)]
pub enum EditPlanError {
    #[error("edit plan revision {expected} is no longer active; {actual} superseded it")]
    StaleRevision { expected: String, actual: String },
    #[error("invalid edit plan: {message}")]
    InvalidPlan { message: String },
    #[error("edit plan has blocking validation findings: {codes:?}")]
    ValidationFailed { codes: Vec<String> },
    #[error("edit plan revision {revision_id} not found")]
    UnknownRevision { revision_id: String },
}

impl EditPlanError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::StaleRevision { .. } => "edit_plan_stale_revision",
            Self::UnknownRevision { .. } => "edit_plan_unknown_revision",
            Self::InvalidPlan { .. } => "edit_plan_invalid_input",
            Self::ValidationFailed { .. } => "edit_plan_validation_failed",
        }
    }
}
