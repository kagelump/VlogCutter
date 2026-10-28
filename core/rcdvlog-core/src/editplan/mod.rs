//! The edit-plan model and revision semantics (D01-T019).
//!
//! One picture track, hard picture cuts, and immutable ordered operations.
//! Optional versioned source audio extends legacy linked playback with explicit
//! placement, gain, and short fades; see `audio` for the bounded contract.
//!
//! Two invariants are enforced by the database rather than by application
//! discipline, because discipline is what fails at 2am:
//!
//! - `UNIQUE(revision_id, position)` makes two clips at one position
//!   unrepresentable.
//! - A partial unique index on `active = 1` makes two active revisions for one
//!   project unrepresentable.
//!
//! An edit-plan revision can never silently replace a newer one: activation
//! takes the revision the caller *believed* was active and refuses if that is
//! no longer true.

pub mod audio;
mod audio_render;
pub mod diff;
pub mod feedback;
mod feedback_model;
pub mod inspection;
mod inspection_media;
mod inspection_model;
pub mod model;
pub mod otio;
pub mod preview;
mod preview_artifact_model;
pub mod preview_artifacts;
mod preview_media;
mod preview_segments;
mod range_model;
pub mod ranges;
pub mod selection;
mod selection_logic;
mod selection_model;
mod selection_report;
mod selection_source;
pub mod validate;

pub use audio::{EditPlanAudio, SourceAudio};
pub use inspection_model::*;
pub use model::{
    EditPlanError, EditPlanExport, EditPlanOperation, EditPlanRevision, EditPlanStaleness,
};
pub use otio::{OtioExportError, OtioExportRequest, OtioExportResult, otio_document};
pub use preview::{PreviewFailure, PreviewProgress, PreviewRequest, PreviewResult};
pub use preview_artifact_model::*;
pub use range_model::*;
pub use validate::{Finding, FindingKind, Severity, ValidationReport};

use crate::Catalog;
use crate::now;
use crate::operations::{
    EDIT_PLAN_EXPORT_RECORDED, EDIT_PLAN_REVISION_ACTIVATED, EDIT_PLAN_REVISION_CREATED,
    NewOperation, commit_local_in_transaction, current_revision,
};
use anyhow::{Result, anyhow};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

impl Catalog {
    /// Creates a new revision from an ordered list of operations.
    ///
    /// Positions are assigned `0..n-1` from the given order, so the caller's
    /// order *is* the plan's order. A revision with zero operations is valid:
    /// an empty plan is a legitimate state, not an error.
    ///
    /// `expected_active_revision_id` is `Option` only to express "no plan
    /// exists yet". Once a plan exists, passing `None` is refused rather than
    /// treated as a force.
    pub fn create_edit_plan_revision(
        &self,
        project_id: &str,
        operations: &[EditPlanOperation],
        script_revision_id: Option<&str>,
        expected_active_revision_id: Option<&str>,
    ) -> Result<EditPlanRevision> {
        self.create_edit_plan_from_base(
            project_id,
            operations,
            script_revision_id,
            expected_active_revision_id,
            expected_active_revision_id,
        )
    }

    /// Fork any immutable draft while independently guarding the active plan.
    /// Stable operation IDs identify the same cut across revisions. New/repeated
    /// cuts must use distinct IDs; empty IDs receive fresh generated identities.
    pub fn create_edit_plan_from_base(
        &self,
        project_id: &str,
        operations: &[EditPlanOperation],
        script_revision_id: Option<&str>,
        base_revision_id: Option<&str>,
        expected_active_revision_id: Option<&str>,
    ) -> Result<EditPlanRevision> {
        self.check_revision_inputs(project_id, operations, base_revision_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active(&transaction, project_id, expected_active_revision_id)?;
        let (revision_id, operation) = revision_operation(
            &transaction,
            project_id,
            operations,
            script_revision_id,
            base_revision_id,
        )?;
        commit_local_in_transaction(&transaction, project_id, vec![operation])?;
        transaction.commit()?;
        self.edit_plan_revision(&revision_id)
    }

    fn check_revision_inputs(
        &self,
        project_id: &str,
        operations: &[EditPlanOperation],
        base_revision_id: Option<&str>,
    ) -> Result<()> {
        anyhow::ensure!(
            operations.len() <= 10_000,
            "invalid input: maximum 10000 cuts"
        );
        self.require_project(project_id)?;
        if let Some(base) = base_revision_id {
            let base = self.edit_plan_revision(base)?;
            if base.project_id != project_id {
                return Err(EditPlanError::InvalidPlan {
                    message: "base revision belongs to another project".into(),
                }
                .into());
            }
        }
        let mut ids = std::collections::HashSet::new();
        for operation in operations {
            if !operation.source_in_seconds.is_finite()
                || !operation.source_out_seconds.is_finite()
                || (!operation.operation_id.is_empty() && !ids.insert(&operation.operation_id))
            {
                return Err(EditPlanError::InvalidPlan {
                    message: "ranges must be finite and cut IDs unique within a revision".into(),
                }
                .into());
            }
            if let Some(EditPlanAudio::Source(audio)) = &operation.audio
                && ![
                    audio.source_in_seconds,
                    audio.source_out_seconds,
                    audio.timeline_start_seconds,
                    audio.sync_offset_seconds,
                    audio.gain_db,
                    audio.fade_in_seconds,
                    audio.fade_out_seconds,
                ]
                .iter()
                .all(|value| value.is_finite())
            {
                return Err(EditPlanError::InvalidPlan {
                    message: "audio timing and gains must be finite".into(),
                }
                .into());
            }
            if let Some(EditPlanAudio::Source(audio)) = &operation.audio
                && self.get_asset(&audio.asset_id)?.project_id != project_id
            {
                return Err(EditPlanError::InvalidPlan {
                    message: "audio asset belongs to another project".into(),
                }
                .into());
            }
            if self.get_asset(&operation.asset_id)?.project_id != project_id {
                return Err(EditPlanError::InvalidPlan {
                    message: "asset belongs to another project".into(),
                }
                .into());
            }
        }
        Ok(())
    }

    /// Makes a revision the active plan.
    ///
    /// Refused with `EditPlanError::StaleRevision` when the revision the caller
    /// believed was active is not — so a plan derived from a superseded parent
    /// can never silently replace the newer one. The refused revision is *not*
    /// lost: it stays in the table and can be re-derived from the winner.
    pub fn activate_edit_plan_revision(
        &self,
        revision_id: &str,
        expected_active_revision_id: Option<&str>,
    ) -> Result<EditPlanRevision> {
        let revision = self.edit_plan_revision(revision_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active(
            &transaction,
            &revision.project_id,
            expected_active_revision_id,
        )?;
        let report = self.validate_edit_plan(revision_id)?;
        if !report.renderable() {
            return Err(EditPlanError::ValidationFailed {
                codes: report
                    .errors()
                    .map(|finding| finding.code.to_string())
                    .collect(),
            }
            .into());
        }
        let guard = current_revision(
            &transaction,
            &revision.project_id,
            "editPlan",
            &revision.project_id,
        )?;
        commit_local_in_transaction(
            &transaction,
            &revision.project_id,
            vec![NewOperation {
                aggregate_type: "editPlan",
                aggregate_id: revision.project_id.clone(),
                operation_type: EDIT_PLAN_REVISION_ACTIVATED,
                base_revision: Some(guard),
                payload: serde_json::json!({ "revisionId": revision_id }),
            }],
        )?;
        transaction.commit()?;
        self.edit_plan_revision(revision_id)
    }

    pub(crate) fn active_edit_plan_revision_id(&self, project_id: &str) -> Result<Option<String>> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT revision_id FROM edit_plan_revisions WHERE project_id=?1 AND active=1",
                [project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?)
    }

    /// The project's active plan, or `None` when it has none yet.
    pub fn active_edit_plan(&self, project_id: &str) -> Result<Option<EditPlanRevision>> {
        self.require_project(project_id)?;
        match self.active_edit_plan_revision_id(project_id)? {
            Some(revision_id) => Ok(Some(self.edit_plan_revision(&revision_id)?)),
            None => Ok(None),
        }
    }

    /// One revision with its operations, in position order.
    pub fn edit_plan_revision(&self, revision_id: &str) -> Result<EditPlanRevision> {
        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction()?;
        let revision = read_revision(&transaction, revision_id)?;
        transaction.commit()?;
        Ok(revision)
    }

    /// Whether a revision was derived from a script revision that is no longer
    /// the active one.
    ///
    /// Staleness is *derived*, never stored: storing it would let it drift from
    /// the thing it describes. Until a script surface exists, the active script
    /// revision is the one recorded on the project's active plan.
    pub fn edit_plan_staleness(&self, revision_id: &str) -> Result<EditPlanStaleness> {
        let revision = self.edit_plan_revision(revision_id)?;
        let active_script = self
            .active_edit_plan(&revision.project_id)?
            .and_then(|plan| plan.script_revision_id);
        Ok(EditPlanStaleness {
            stale: match (&revision.script_revision_id, &active_script) {
                (Some(recorded), Some(active)) => recorded != active,
                _ => false,
            },
            recorded_script_revision_id: revision.script_revision_id,
            active_script_revision_id: active_script,
        })
    }

    /// Records that a revision was exported. Export history is append-only:
    /// recording a new export never removes a prior one.
    pub fn record_edit_plan_export(&self, export: &EditPlanExport) -> Result<()> {
        let revision = self.edit_plan_revision(&export.revision_id)?;
        let guard = current_revision(
            &self.connection()?,
            &revision.project_id,
            "editPlan",
            &revision.project_id,
        )?;
        self.commit_local(
            &revision.project_id,
            vec![NewOperation {
                aggregate_type: "editPlan",
                aggregate_id: revision.project_id.clone(),
                operation_type: EDIT_PLAN_EXPORT_RECORDED,
                base_revision: Some(guard),
                payload: serde_json::json!({
                    "exportId": if export.export_id.is_empty() {
                        format!("editexp_{}", Uuid::new_v4().simple())
                    } else {
                        export.export_id.clone()
                    },
                    "revisionId": export.revision_id,
                    "outputPath": export.output_path,
                    "outputDigest": export.output_digest,
                    "exportedAt": if export.exported_at.is_empty() {
                        now()
                    } else {
                        export.exported_at.clone()
                    },
                }),
            }],
        )?;
        Ok(())
    }

    /// Every export recorded for a project, oldest first.
    pub fn edit_plan_exports(&self, project_id: &str) -> Result<Vec<EditPlanExport>> {
        self.require_project(project_id)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT export_id, revision_id, output_path, output_digest, exported_at
             FROM edit_plan_exports WHERE project_id=?1 ORDER BY exported_at, export_id",
        )?;
        Ok(statement
            .query_map([project_id], |row| {
                Ok(EditPlanExport {
                    export_id: row.get(0)?,
                    revision_id: row.get(1)?,
                    output_path: row.get(2)?,
                    output_digest: row.get(3)?,
                    exported_at: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every revision of a project, newest first. Superseded revisions are
    /// never deleted — a refused activation must leave its revision recoverable.
    pub fn edit_plan_revisions(&self, project_id: &str) -> Result<Vec<EditPlanRevision>> {
        self.require_project(project_id)?;
        let ids: Vec<String> = {
            let connection = self.connection()?;
            let mut statement = connection.prepare(
                "SELECT revision_id FROM edit_plan_revisions WHERE project_id=?1
                 ORDER BY created_at DESC, revision_id DESC",
            )?;
            statement
                .query_map([project_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        ids.iter()
            .map(|revision_id| self.edit_plan_revision(revision_id))
            .collect()
    }
}

/// Shared by the materializers: writes a revision and its operations.
pub(crate) fn insert_revision(
    transaction: &rusqlite::Transaction<'_>,
    project_id: &str,
    revision_id: &str,
    script_revision_id: Option<&str>,
    parent_revision_id: Option<&str>,
    created_at: &str,
    operations: &[EditPlanOperation],
) -> Result<()> {
    transaction.execute(
        "INSERT OR IGNORE INTO edit_plan_revisions(revision_id, project_id, script_revision_id, parent_revision_id, active, created_at)
         VALUES (?1, ?2, ?3, ?4, 0, ?5)",
        params![revision_id, project_id, script_revision_id, parent_revision_id, created_at],
    )?;
    for operation in operations {
        transaction.execute(
            "INSERT OR IGNORE INTO edit_plan_operations(operation_id, revision_id, position, asset_id, source_in_seconds, source_out_seconds, rationale, audio_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                operation.operation_id,
                revision_id,
                operation.position as i64,
                operation.asset_id,
                operation.source_in_seconds,
                operation.source_out_seconds,
                operation.rationale,
                operation.audio.as_ref().map(serde_json::to_string).transpose()?,
            ],
        )?;
    }
    Ok(())
}

/// Shared by the materializers: makes one revision active, deactivating the
/// previous one in the same transaction so the partial unique index is never
/// momentarily violated.
pub(crate) fn activate_revision(
    transaction: &rusqlite::Transaction<'_>,
    revision_id: &str,
) -> Result<()> {
    let project_id: String = transaction
        .query_row(
            "SELECT project_id FROM edit_plan_revisions WHERE revision_id=?1",
            [revision_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("edit plan revision {revision_id} not found"))?;
    transaction.execute(
        "UPDATE edit_plan_revisions SET active=0 WHERE project_id=?1 AND active=1",
        [&project_id],
    )?;
    transaction.execute(
        "UPDATE edit_plan_revisions SET active=1 WHERE revision_id=?1",
        [revision_id],
    )?;
    Ok(())
}

/// The read and the operation commit share one immediate write transaction.
fn require_active(
    connection: &rusqlite::Connection,
    project_id: &str,
    expected: Option<&str>,
) -> Result<()> {
    let actual: Option<String> = connection
        .query_row(
            "SELECT revision_id FROM edit_plan_revisions WHERE project_id=?1 AND active=1",
            [project_id],
            |row| row.get(0),
        )
        .optional()?;
    if actual.as_deref() != expected {
        return Err(EditPlanError::StaleRevision {
            expected: expected.unwrap_or("none").into(),
            actual: actual.unwrap_or_else(|| "none".into()),
        }
        .into());
    }
    Ok(())
}

pub(crate) fn read_revision(
    connection: &rusqlite::Connection,
    revision_id: &str,
) -> Result<EditPlanRevision> {
    let (project_id, script_revision_id, parent_revision_id, active, created_at) = connection
        .query_row(
            "SELECT project_id, script_revision_id, parent_revision_id, active, created_at
                 FROM edit_plan_revisions WHERE revision_id=?1",
            [revision_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)? != 0,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| EditPlanError::UnknownRevision {
            revision_id: revision_id.to_string(),
        })?;

    // Read-only callers must not have to migrate a legacy linked plan.
    let audio_column =
        if crate::migrations::column_exists(connection, "edit_plan_operations", "audio_json")? {
            "audio_json"
        } else {
            "NULL"
        };
    let mut statement = connection.prepare(&format!(
            "SELECT operation_id, position, asset_id, source_in_seconds, source_out_seconds, rationale, {audio_column}
             FROM edit_plan_operations WHERE revision_id=?1 ORDER BY position",
        ))?;
    let operations = statement
        .query_map([revision_id], |row| {
            Ok(EditPlanOperation {
                operation_id: row.get(0)?,
                position: row.get::<_, i64>(1)? as u32,
                asset_id: row.get(2)?,
                source_in_seconds: row.get(3)?,
                source_out_seconds: row.get(4)?,
                rationale: row.get(5)?,
                audio: row
                    .get::<_, Option<String>>(6)?
                    .map(|json| {
                        serde_json::from_str(&json).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                6,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })
                    })
                    .transpose()?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(EditPlanRevision {
        revision_id: revision_id.to_string(),
        project_id,
        script_revision_id,
        parent_revision_id,
        active,
        operations,
        created_at,
    })
}

/// Shared by ordinary drafts and atomic accepted-feedback revisions.
fn revision_operation(
    connection: &rusqlite::Connection,
    project_id: &str,
    operations: &[EditPlanOperation],
    script_revision_id: Option<&str>,
    base_revision_id: Option<&str>,
) -> Result<(String, NewOperation)> {
    let inherited = base_revision_id
        .map(|base| selection::load_metadata(connection, base))
        .transpose()?
        .flatten();
    revision_operation_with_selection(
        connection,
        project_id,
        operations,
        script_revision_id,
        base_revision_id,
        inherited.as_ref(),
    )
}

fn revision_operation_with_selection(
    connection: &rusqlite::Connection,
    project_id: &str,
    operations: &[EditPlanOperation],
    script_revision_id: Option<&str>,
    base_revision_id: Option<&str>,
    selection: Option<&selection_model::SelectionMetadata>,
) -> Result<(String, NewOperation)> {
    let revision_id = format!("editrev_{}", Uuid::new_v4().simple());
    let mut payload = serde_json::json!({
        "revisionId": revision_id,
        "scriptRevisionId": script_revision_id,
        "parentRevisionId": base_revision_id,
        "operations": operations
            .iter()
            .enumerate()
            .map(|(position, operation)| {
                let mut operation = operation.clone();
                operation.position = position as u32;
                if operation.operation_id.is_empty() {
                    operation.operation_id = format!("editop_{}", Uuid::new_v4().simple());
                }
                serde_json::to_value(operation).expect("finite edit operation serializes")
            })
            .collect::<Vec<_>>(),
    });
    if let Some(metadata) = selection {
        payload["selection"] = serde_json::to_value(metadata)?;
    }
    let guard = current_revision(connection, project_id, "editPlan", project_id)?;
    Ok((
        revision_id,
        NewOperation {
            aggregate_type: "editPlan",
            aggregate_id: project_id.into(),
            operation_type: EDIT_PLAN_REVISION_CREATED,
            base_revision: Some(guard),
            payload,
        },
    ))
}
