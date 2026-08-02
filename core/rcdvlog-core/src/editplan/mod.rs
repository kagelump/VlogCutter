//! The edit-plan model and revision semantics (D01-T019).
//!
//! The editing scope is exhaustively: one video track, linked source audio,
//! hard cuts, source in/out ranges, ordered operations. There is no `track`
//! column, no `transition` table, and no `effect` field — not even as reserved
//! schema. That absence is a deliverable, not an omission.
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

pub mod model;
pub mod otio;
pub mod preview;
pub mod validate;

pub use model::{
    EditPlanError, EditPlanExport, EditPlanOperation, EditPlanRevision, EditPlanStaleness,
};
pub use otio::{OtioExportError, OtioExportRequest, OtioExportResult, otio_document};
pub use preview::{PreviewFailure, PreviewProgress, PreviewRequest, PreviewResult};
pub use validate::{Finding, FindingKind, Severity, ValidationReport};

use crate::Catalog;
use crate::now;
use crate::operations::{
    EDIT_PLAN_EXPORT_RECORDED, EDIT_PLAN_REVISION_ACTIVATED, EDIT_PLAN_REVISION_CREATED,
    NewOperation, current_revision,
};
use anyhow::{Result, anyhow};
use rusqlite::{OptionalExtension, params};
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
        self.require_project(project_id)?;
        let active = self.active_edit_plan_revision_id(project_id)?;
        match (&active, expected_active_revision_id) {
            (Some(current), Some(expected)) if current != expected => {
                return Err(EditPlanError::StaleRevision {
                    expected: expected.to_string(),
                    actual: current.clone(),
                }
                .into());
            }
            (Some(current), None) => {
                return Err(EditPlanError::StaleRevision {
                    expected: "none".to_string(),
                    actual: current.clone(),
                }
                .into());
            }
            _ => {}
        }

        let revision_id = format!("editrev_{}", Uuid::new_v4().simple());
        let payload = serde_json::json!({
            "revisionId": revision_id,
            "scriptRevisionId": script_revision_id,
            "parentRevisionId": active,
            "operations": operations
                .iter()
                .enumerate()
                .map(|(position, operation)| serde_json::json!({
                    "operationId": if operation.operation_id.is_empty() {
                        format!("editop_{}", Uuid::new_v4().simple())
                    } else {
                        operation.operation_id.clone()
                    },
                    "position": position as u32,
                    "assetId": operation.asset_id,
                    "sourceInSeconds": operation.source_in_seconds,
                    "sourceOutSeconds": operation.source_out_seconds,
                    "rationale": operation.rationale,
                }))
                .collect::<Vec<_>>(),
        });
        let guard = current_revision(&self.connection()?, project_id, "editPlan", project_id)?;
        self.commit_local(
            project_id,
            vec![NewOperation {
                aggregate_type: "editPlan",
                aggregate_id: project_id.to_string(),
                operation_type: EDIT_PLAN_REVISION_CREATED,
                base_revision: Some(guard),
                payload,
            }],
        )?;
        self.edit_plan_revision(&revision_id)
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
        let active = self.active_edit_plan_revision_id(&revision.project_id)?;
        match (&active, expected_active_revision_id) {
            (Some(current), Some(expected)) if current != expected => {
                return Err(EditPlanError::StaleRevision {
                    expected: expected.to_string(),
                    actual: current.clone(),
                }
                .into());
            }
            (Some(current), None) => {
                return Err(EditPlanError::StaleRevision {
                    expected: "none".to_string(),
                    actual: current.clone(),
                }
                .into());
            }
            _ => {}
        }
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
                operation_type: EDIT_PLAN_REVISION_ACTIVATED,
                base_revision: Some(guard),
                payload: serde_json::json!({ "revisionId": revision_id }),
            }],
        )?;
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

        let mut statement = connection.prepare(
            "SELECT operation_id, position, asset_id, source_in_seconds, source_out_seconds, rationale
             FROM edit_plan_operations WHERE revision_id=?1 ORDER BY position",
        )?;
        let operations = statement
            .query_map([revision_id], |row| {
                Ok(EditPlanOperation {
                    operation_id: row.get(0)?,
                    position: row.get::<_, i64>(1)? as u32,
                    asset_id: row.get(2)?,
                    source_in_seconds: row.get(3)?,
                    source_out_seconds: row.get(4)?,
                    rationale: row.get(5)?,
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
            "INSERT OR IGNORE INTO edit_plan_operations(operation_id, revision_id, position, asset_id, source_in_seconds, source_out_seconds, rationale)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                operation.operation_id,
                revision_id,
                operation.position as i64,
                operation.asset_id,
                operation.source_in_seconds,
                operation.source_out_seconds,
                operation.rationale,
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
