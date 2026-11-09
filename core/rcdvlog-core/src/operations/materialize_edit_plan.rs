//! Edit-plan revision, activation, and export materializers.

use super::*;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct EditPlanRevisionCreatedPayload {
    pub(crate) revision_id: String,
    pub(crate) script_revision_id: Option<String>,
    pub(crate) parent_revision_id: Option<String>,
    pub(crate) operations: Vec<crate::editplan::EditPlanOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) selection: Option<crate::SelectionMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) patch_lineage: Option<Vec<crate::PatchLineage>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) variant_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) story_policy: Option<crate::editplan::EditPlanStoryPolicy>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct EditPlanRevisionActivatedPayload {
    pub(crate) revision_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct EditPlanExportRecordedPayload {
    pub(crate) export_id: String,
    pub(crate) revision_id: String,
    pub(crate) output_path: String,
    pub(crate) output_digest: String,
    pub(crate) exported_at: String,
}

/// The edit plan rides the `editPlan` aggregate with the project id as the
/// aggregate id, because activation is a project-level fact and the active
/// revision is what actually conflicts.
pub(super) fn require_edit_plan_aggregate(operation: &ProjectOperation) -> Result<()> {
    if operation.aggregate_type != "editPlan" || operation.aggregate_id != operation.project_id {
        bail!("edit plan operations must ride the editPlan aggregate keyed by project id");
    }
    Ok(())
}

pub(crate) fn materialize_edit_plan_revision_created(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: EditPlanRevisionCreatedPayload = payload(operation)?;
    if operation.payload_version == 1 && payload.operations.iter().any(|cut| cut.audio.is_some()) {
        bail!("independent source audio requires editPlanRevisionCreated payload version 2");
    }
    if payload.selection.is_some() && !matches!(operation.payload_version, 3..=5) {
        bail!(
            "sequence selection metadata requires editPlanRevisionCreated payload version 3 or newer"
        );
    }
    if operation.payload_version == 3 && payload.selection.is_none() {
        bail!("sequence selection metadata requires editPlanRevisionCreated payload version 3");
    }
    if payload.patch_lineage.is_some() && !matches!(operation.payload_version, 4 | 5) {
        bail!("patch lineage requires editPlanRevisionCreated payload version 4 or newer");
    }
    if operation.payload_version == 4 && payload.patch_lineage.is_none() {
        bail!("patch payload version 4 requires lineage and variant name");
    }
    if payload.story_policy.is_some() && operation.payload_version != 5 {
        bail!("story policy requires editPlanRevisionCreated payload version 5");
    }
    if operation.payload_version == 5 && payload.story_policy.is_none() {
        bail!("story policy payload version 5 requires storyPolicy");
    }
    if operation.payload_version == 4
        && !crate::migrations::column_exists(transaction, "edit_plan_revisions", "patch_json")?
    {
        return Ok(ApplyOperationResult::Quarantined);
    }
    if operation.payload_version == 5
        && !crate::migrations::column_exists(
            transaction,
            "edit_plan_revisions",
            "story_policy_json",
        )?
    {
        return Ok(ApplyOperationResult::Quarantined);
    }
    require_edit_plan_aggregate(operation)?;
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    let patch_metadata = match (&payload.patch_lineage, &payload.variant_name) {
        (None, None) => {
            ensure!(
                operation.payload_version < 4
                    || (operation.payload_version == 5 && payload.story_policy.is_some()),
                "patch payload versions 4 and 5 require lineage and variant name"
            );
            None
        }
        (Some(lineage), Some(name)) => {
            ensure!(
                matches!(operation.payload_version, 4 | 5)
                    && !name.trim().is_empty()
                    && name.len() <= 255
                    && lineage.len() <= 2000,
                "invalid sparse patch metadata/version"
            );
            let parent = crate::editplan::read_revision(
                transaction,
                payload
                    .parent_revision_id
                    .as_deref()
                    .context("patch requires an immutable base")?,
            )?;
            ensure!(
                parent.project_id == operation.project_id,
                "patch parent belongs to another project"
            );
            let mut ids = BTreeSet::new();
            for item in lineage {
                ensure!(
                    ids.insert(&item.operation_id)
                        && payload
                            .operations
                            .iter()
                            .any(|cut| cut.operation_id == item.operation_id)
                        && match item.kind.as_str() {
                            "insert" =>
                                item.parent_operation_id.is_none()
                                    && !parent
                                        .operations
                                        .iter()
                                        .any(|cut| cut.operation_id == item.operation_id),
                            "split" | "replace" =>
                                item.parent_operation_id.as_ref().is_some_and(|id| parent
                                    .operations
                                    .iter()
                                    .any(|cut| &cut.operation_id == id)),
                            _ => false,
                        },
                    "invalid sparse patch lineage identity"
                );
            }
            Some(serde_json::json!({"name":name,"lineage":lineage}))
        }
        _ => bail!("patch lineage and variant name must be supplied together"),
    };
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM edit_plan_revisions WHERE revision_id=?1)",
        [&payload.revision_id],
        |r| r.get(0),
    )?;
    if exists {
        let existing = crate::editplan::read_revision(transaction, &payload.revision_id)?;
        ensure!(
            existing.project_id == operation.project_id
                && existing.parent_revision_id == payload.parent_revision_id
                && existing.script_revision_id == payload.script_revision_id
                && existing.operations == payload.operations
                && crate::editplan::selection::load_metadata(transaction, &payload.revision_id)?
                    == payload.selection
                && crate::editplan::patch::load_patch_metadata(transaction, &payload.revision_id)?
                    == patch_metadata
                && crate::editplan::load_story_policy(transaction, &payload.revision_id)?
                    == payload.story_policy,
            "immutable edit revision identity was reused with different content"
        );
    }
    // `INSERT OR IGNORE` throughout, so applying the same operation id twice
    // materializes exactly once.
    crate::editplan::insert_revision(
        transaction,
        crate::editplan::RevisionInsert {
            project_id: &operation.project_id,
            revision_id: &payload.revision_id,
            script_revision_id: payload.script_revision_id.as_deref(),
            parent_revision_id: payload.parent_revision_id.as_deref(),
            created_at: &now(),
            operations: &payload.operations,
            story_policy: payload.story_policy.as_ref(),
        },
    )?;
    if let Some(metadata) = patch_metadata {
        transaction.execute(
            "UPDATE edit_plan_revisions SET patch_json=?2 WHERE revision_id=?1",
            params![payload.revision_id, serde_json::to_string(&metadata)?],
        )?;
    }
    if let Some(metadata) = &payload.selection {
        crate::editplan::selection::materialize_metadata(
            transaction,
            &operation.project_id,
            &payload.revision_id,
            metadata,
        )?;
    }
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

pub(super) fn materialize_edit_plan_revision_activated(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: EditPlanRevisionActivatedPayload = payload(operation)?;
    require_edit_plan_aggregate(operation)?;
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    crate::editplan::activate_revision(transaction, &payload.revision_id)?;
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

pub(super) fn materialize_edit_plan_export_recorded(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: EditPlanExportRecordedPayload = payload(operation)?;
    require_edit_plan_aggregate(operation)?;
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    // Append-only: a new export never removes a prior one.
    transaction.execute(
        "INSERT OR IGNORE INTO edit_plan_exports(export_id, project_id, revision_id, output_path, output_digest, exported_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            payload.export_id,
            operation.project_id,
            payload.revision_id,
            payload.output_path,
            payload.output_digest,
            payload.exported_at,
        ],
    )?;
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}
