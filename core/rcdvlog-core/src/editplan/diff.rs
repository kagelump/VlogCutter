//! Revision comparison uses cut identity, never movie time or ordinal alone.
use super::{EditPlanError, EditPlanOperation};
use crate::Catalog;
use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditPlanChange {
    pub operation_id: String,
    pub changes: Vec<&'static str>,
    pub before: Option<EditPlanOperation>,
    pub after: Option<EditPlanOperation>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditPlanDiff {
    pub base_revision_id: String,
    pub revision_id: String,
    pub changes: Vec<EditPlanChange>,
}

impl Catalog {
    pub fn diff_edit_plans(&self, base: &str, revision: &str) -> Result<EditPlanDiff> {
        let before = self.edit_plan_revision(base)?;
        let after = self.edit_plan_revision(revision)?;
        if before.project_id != after.project_id {
            return Err(EditPlanError::InvalidPlan {
                message: "cannot compare revisions from different projects".into(),
            }
            .into());
        }
        let ids: BTreeSet<_> = before
            .operations
            .iter()
            .chain(&after.operations)
            .map(|cut| &cut.operation_id)
            .collect();
        let mut changes = Vec::new();
        for id in ids {
            let left = before.operations.iter().find(|cut| &cut.operation_id == id);
            let right = after.operations.iter().find(|cut| &cut.operation_id == id);
            let mut kinds = Vec::new();
            match (left, right) {
                (None, Some(_)) => kinds.push("added"),
                (Some(_), None) => kinds.push("omitted"),
                (Some(left), Some(right)) => {
                    if left.position != right.position {
                        kinds.push("order");
                    }
                    if left.asset_id != right.asset_id {
                        kinds.push("source");
                    }
                    if left.source_in_seconds != right.source_in_seconds
                        || left.source_out_seconds != right.source_out_seconds
                    {
                        kinds.push("range");
                    }
                    if left.rationale != right.rationale {
                        kinds.push("rationale");
                    }
                }
                _ => unreachable!(),
            }
            if !kinds.is_empty() {
                changes.push(EditPlanChange {
                    operation_id: id.clone(),
                    changes: kinds,
                    before: left.cloned(),
                    after: right.cloned(),
                });
            }
        }
        Ok(EditPlanDiff {
            base_revision_id: base.into(),
            revision_id: revision.into(),
            changes,
        })
    }
}
