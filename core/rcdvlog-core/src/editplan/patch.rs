//! Sparse edits use the ordinary immutable revision and validation owners.
use super::{EditPlanAudio, EditPlanOperation, EditPlanRevision, ValidationReport};
use crate::Catalog;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PatchAnchor {
    Before { operation_id: String },
    After { operation_id: String },
    End,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EditPlanPatch {
    Trim {
        operation_id: String,
        source_in_seconds: f64,
        source_out_seconds: f64,
        rationale: String,
    },
    Replace {
        operation_id: String,
        asset_id: String,
        source_in_seconds: f64,
        source_out_seconds: f64,
        audio: Option<EditPlanAudio>,
        rationale: String,
    },
    Insert {
        anchor: PatchAnchor,
        operation: EditPlanOperation,
    },
    Reorder {
        operation_id: String,
        anchor: PatchAnchor,
    },
    Omit {
        operation_id: String,
    },
    Audio {
        operation_id: String,
        audio: Option<EditPlanAudio>,
        rationale: String,
    },
    Split {
        operation_id: String,
        source_split_seconds: f64,
        new_operation_id: String,
        rationale: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditPlanPatchInput {
    pub version: u32,
    pub name: String,
    pub edits: Vec<EditPlanPatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchLineage {
    pub operation_id: String,
    pub parent_operation_id: Option<String>,
    pub kind: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditPlanPatchResult {
    pub name: String,
    pub revision: EditPlanRevision,
    pub diff: super::diff::EditPlanDiff,
    pub validation: ValidationReport,
    pub lineage: Vec<PatchLineage>,
}

fn target(cuts: &[EditPlanOperation], id: &str) -> Result<usize> {
    cuts.iter()
        .position(|o| o.operation_id == id)
        .context("invalid input: patch target/anchor is missing")
}
fn anchor(cuts: &[EditPlanOperation], anchor: &PatchAnchor) -> Result<usize> {
    Ok(match anchor {
        PatchAnchor::Before { operation_id } => target(cuts, operation_id)?,
        PatchAnchor::After { operation_id } => target(cuts, operation_id)? + 1,
        PatchAnchor::End => cuts.len(),
    })
}
fn starts(cuts: &[EditPlanOperation]) -> BTreeMap<String, f64> {
    let mut cursor = 0.0;
    cuts.iter()
        .map(|cut| {
            let start = cursor;
            cursor += cut.source_out_seconds - cut.source_in_seconds;
            (cut.operation_id.clone(), start)
        })
        .collect()
}

impl Catalog {
    pub fn patch_edit_plan(
        &self,
        project: &str,
        base_id: &str,
        expected_active: Option<&str>,
        input: &EditPlanPatchInput,
    ) -> Result<EditPlanPatchResult> {
        ensure!(
            input.version == 1
                && !input.name.trim().is_empty()
                && input.name.len() <= 255
                && !input.edits.is_empty()
                && input.edits.len() <= 2000,
            "invalid input: patch version, name or edit bounds"
        );
        let base = self.edit_plan_revision(base_id)?;
        ensure!(
            base.project_id == project,
            "invalid input: patch base belongs to another project"
        );
        let mut candidate = base.clone();
        candidate.active = false;
        let cuts = &mut candidate.operations;
        let mut touched = BTreeSet::new();
        let mut explicit_audio = BTreeSet::new();
        let mut lineage = Vec::new();
        let base_ids: BTreeSet<_> = cuts.iter().map(|c| c.operation_id.clone()).collect();
        let mut new_ids = base_ids.clone();
        for edit in &input.edits {
            let id = match edit {
                EditPlanPatch::Insert { operation, .. } => &operation.operation_id,
                EditPlanPatch::Trim { operation_id, .. }
                | EditPlanPatch::Replace { operation_id, .. }
                | EditPlanPatch::Reorder { operation_id, .. }
                | EditPlanPatch::Omit { operation_id }
                | EditPlanPatch::Audio { operation_id, .. }
                | EditPlanPatch::Split { operation_id, .. } => operation_id,
            };
            ensure!(
                !id.is_empty() && id.len() <= 255 && touched.insert(id.clone()),
                "invalid input: each patch target must be explicit and unique in a batch"
            );
            match edit {
                EditPlanPatch::Insert {
                    anchor: a,
                    operation,
                } => {
                    ensure!(
                        new_ids.insert(id.clone()),
                        "invalid input: inserted cut ID already exists"
                    );
                    let at = anchor(cuts, a)?;
                    cuts.insert(at, operation.clone());
                    explicit_audio.insert(id.clone());
                    lineage.push(PatchLineage {
                        operation_id: id.clone(),
                        parent_operation_id: None,
                        kind: "insert".into(),
                    });
                }
                EditPlanPatch::Reorder { anchor: a, .. } => {
                    let at = target(cuts, id)?;
                    let cut = cuts.remove(at);
                    let new_at = anchor(cuts, a)?;
                    cuts.insert(new_at, cut);
                }
                EditPlanPatch::Omit { .. } => {
                    let at = target(cuts, id)?;
                    cuts.remove(at);
                }
                EditPlanPatch::Trim {
                    source_in_seconds,
                    source_out_seconds,
                    rationale,
                    ..
                } => {
                    let at = target(cuts, id)?;
                    cuts[at].source_in_seconds = *source_in_seconds;
                    cuts[at].source_out_seconds = *source_out_seconds;
                    cuts[at].rationale = rationale.clone();
                }
                EditPlanPatch::Replace {
                    asset_id,
                    source_in_seconds,
                    source_out_seconds,
                    audio,
                    rationale,
                    ..
                } => {
                    let at = target(cuts, id)?;
                    cuts[at].asset_id = asset_id.clone();
                    cuts[at].source_in_seconds = *source_in_seconds;
                    cuts[at].source_out_seconds = *source_out_seconds;
                    cuts[at].audio = audio.clone();
                    cuts[at].rationale = rationale.clone();
                    explicit_audio.insert(id.clone());
                    lineage.push(PatchLineage {
                        operation_id: id.clone(),
                        parent_operation_id: Some(id.clone()),
                        kind: "replace".into(),
                    });
                }
                EditPlanPatch::Audio {
                    audio, rationale, ..
                } => {
                    let at = target(cuts, id)?;
                    cuts[at].audio = audio.clone();
                    cuts[at].rationale = rationale.clone();
                    explicit_audio.insert(id.clone());
                }
                EditPlanPatch::Split {
                    source_split_seconds,
                    new_operation_id,
                    rationale,
                    ..
                } => {
                    let at = target(cuts, id)?;
                    ensure!(
                        source_split_seconds.is_finite()
                            && *source_split_seconds > cuts[at].source_in_seconds
                            && *source_split_seconds < cuts[at].source_out_seconds
                            && !new_operation_id.is_empty()
                            && new_operation_id.len() <= 255
                            && new_ids.insert(new_operation_id.clone()),
                        "invalid input: split boundary or new ID"
                    );
                    let mut next = cuts[at].clone();
                    next.operation_id = new_operation_id.clone();
                    next.source_in_seconds = *source_split_seconds;
                    next.rationale = rationale.clone();
                    if next.audio.is_some() {
                        next.audio = Some(EditPlanAudio::Silent);
                    }
                    cuts[at].source_out_seconds = *source_split_seconds;
                    cuts[at].rationale = rationale.clone();
                    cuts.insert(at + 1, next);
                    lineage.push(PatchLineage {
                        operation_id: new_operation_id.clone(),
                        parent_operation_id: Some(id.clone()),
                        kind: "split".into(),
                    });
                }
            }
        }
        let old_starts = starts(&base.operations);
        let new_starts = starts(cuts);
        for (i, cut) in cuts.iter_mut().enumerate() {
            cut.position = i as u32;
            if !explicit_audio.contains(&cut.operation_id)
                && let Some(EditPlanAudio::Source(audio)) = &mut cut.audio
                && let Some(old) = base
                    .operations
                    .iter()
                    .find(|o| o.operation_id == cut.operation_id)
            {
                let source_shift = if old.asset_id == audio.asset_id && old.asset_id == cut.asset_id
                {
                    cut.source_in_seconds - old.source_in_seconds
                } else {
                    0.0
                };
                audio.timeline_start_seconds +=
                    new_starts[&cut.operation_id] - old_starts[&cut.operation_id] - source_shift;
            }
        }
        self.check_revision_inputs(project, cuts, Some(base_id))?;
        let key = super::ranges::constraints_key(&self.connection()?, &candidate)?;
        let validation = self.validate_edit_plan_snapshot(&candidate, &|| false, None)?;
        ensure!(
            validation.renderable(),
            super::EditPlanError::ValidationFailed {
                codes: validation.errors().map(|f| f.code.to_owned()).collect()
            }
        );
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        super::require_active(&tx, project, expected_active)?;
        ensure!(
            super::ranges::constraints_key(&tx, &candidate)? == key,
            "stale evidence: constraints changed while validating patch"
        );
        let (id, mut operation) = super::revision_operation(
            &tx,
            project,
            &candidate.operations,
            base.script_revision_id.as_deref(),
            Some(base_id),
        )?;
        operation.payload["patchLineage"] = serde_json::to_value(&lineage)?;
        operation.payload["variantName"] = serde_json::json!(input.name);
        crate::operations::commit_local_in_transaction(&tx, project, vec![operation])?;
        tx.commit()?;
        Ok(EditPlanPatchResult {
            name: input.name.clone(),
            revision: self.edit_plan_revision(&id)?,
            diff: self.diff_edit_plans(base_id, &id)?,
            validation,
            lineage,
        })
    }
}

/// Canonical patch lineage is projected from immutable creation operations.
pub(crate) fn load_patch_metadata(
    connection: &rusqlite::Connection,
    revision: &str,
) -> Result<Option<serde_json::Value>> {
    use rusqlite::OptionalExtension;
    if !crate::migrations::column_exists(connection, "edit_plan_revisions", "patch_json")? {
        return Ok(None);
    }
    let body: Option<String> = connection
        .query_row(
            "SELECT patch_json FROM edit_plan_revisions WHERE revision_id=?1",
            [revision],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    body.map(|s| serde_json::from_str(&s).context("invalid patch metadata projection"))
        .transpose()
}
impl Catalog {
    pub fn edit_plan_patch_metadata(&self, revision: &str) -> Result<Option<serde_json::Value>> {
        self.edit_plan_revision(revision)?;
        load_patch_metadata(&self.connection()?, revision)
    }
}
