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
        /// Omitted anchor means automatic chronological placement. Explicit
        /// anchors are checked placements, not chronology exemptions.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        anchor: Option<PatchAnchor>,
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
    /// New immutable policy for the resulting revision. Omitted inherits the
    /// exact compatible base policy; `Some(empty)` explicitly clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub story_policy: Option<super::EditPlanStoryPolicy>,
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

#[derive(Debug)]
struct PlacementUnit {
    start: usize,
    end: usize,
    min_millis: i64,
    max_millis: i64,
    cuts: Vec<EditPlanOperation>,
}

fn placement_error(code: &'static str, message: String) -> anyhow::Error {
    match code {
        "edit_plan_chronology_ambiguous" => super::EditPlanError::ChronologyAmbiguous { message },
        "edit_plan_chronology_unresolved" => super::EditPlanError::ChronologyUnresolved { message },
        "edit_plan_story_policy_invalid" => super::EditPlanError::StoryPolicyInvalid { message },
        _ => super::EditPlanError::ChronologyConflict { message },
    }
    .into()
}

fn placement_units(
    cuts: &[EditPlanOperation],
    policy: Option<&super::EditPlanStoryPolicy>,
    chronology: &BTreeMap<String, super::story::ChronologyPlacement>,
) -> Result<Vec<PlacementUnit>> {
    let mut teaser_end = 0usize;
    if let Some(policy) = policy {
        for (index, teaser) in policy.opening_teasers.iter().enumerate() {
            let Some(operation) = cuts.get(index) else {
                return Err(placement_error(
                    "edit_plan_story_policy_invalid",
                    format!(
                        "opening teaser {} is not at the operation prefix",
                        teaser.operation_id
                    ),
                ));
            };
            if operation.operation_id != teaser.operation_id || !teaser.cut().matches(operation) {
                return Err(placement_error(
                    "edit_plan_story_policy_invalid",
                    format!(
                        "opening teaser {} no longer matches the exact operation prefix",
                        teaser.operation_id
                    ),
                ));
            }
            teaser_end = index + 1;
        }
    }

    let mut groups_by_start: BTreeMap<usize, Vec<EditPlanOperation>> = BTreeMap::new();
    if let Some(policy) = policy {
        for group in &policy.protected_groups {
            let mut positions = Vec::with_capacity(group.cuts.len());
            let mut operations = Vec::with_capacity(group.cuts.len());
            for cut in &group.cuts {
                let Some(position) = cuts
                    .iter()
                    .position(|operation| operation.operation_id == cut.operation_id)
                else {
                    return Err(placement_error(
                        "edit_plan_story_policy_invalid",
                        format!(
                            "protected group {} refers to missing cut {}",
                            group.group_id, cut.operation_id
                        ),
                    ));
                };
                if !cut.matches(&cuts[position]) {
                    return Err(placement_error(
                        "edit_plan_story_policy_invalid",
                        format!(
                            "protected group {} no longer matches cut {}'s exact source range",
                            group.group_id, cut.operation_id
                        ),
                    ));
                }
                positions.push(position);
                operations.push(cuts[position].clone());
            }
            if positions.windows(2).any(|pair| pair[1] != pair[0] + 1) {
                return Err(placement_error(
                    "edit_plan_story_policy_invalid",
                    format!(
                        "protected group {} is no longer contiguous; automatic insertion is refused",
                        group.group_id
                    ),
                ));
            }
            if let Some(start) = positions.first().copied() {
                if start < teaser_end {
                    return Err(placement_error(
                        "edit_plan_story_policy_invalid",
                        format!(
                            "protected group {} overlaps the opening teaser prefix",
                            group.group_id
                        ),
                    ));
                }
                groups_by_start.insert(start, operations);
            }
        }
    }

    let mut units = Vec::new();
    let mut index = teaser_end;
    while index < cuts.len() {
        let operations = if let Some(group) = groups_by_start.get(&index) {
            group.clone()
        } else {
            vec![cuts[index].clone()]
        };
        let end = index + operations.len();
        let mut min_millis = i64::MAX;
        let mut max_millis = i64::MIN;
        for operation in &operations {
            let Some(placement) = chronology.get(&operation.asset_id) else {
                return Err(placement_error(
                    "edit_plan_chronology_unresolved",
                    format!(
                        "cut {} has no solved source chronology",
                        operation.operation_id
                    ),
                ));
            };
            let Some(millis) = placement.trip_time_millis else {
                return Err(placement_error(
                    "edit_plan_chronology_unresolved",
                    format!(
                        "cut {} has no solved trip time; automatic insertion cannot guess story order",
                        operation.operation_id
                    ),
                ));
            };
            min_millis = min_millis.min(millis);
            max_millis = max_millis.max(millis);
        }
        units.push(PlacementUnit {
            start: index,
            end,
            min_millis,
            max_millis,
            cuts: operations,
        });
        index = end;
    }
    Ok(units)
}

fn automatic_insert_slot(
    cuts: &[EditPlanOperation],
    policy: Option<&super::EditPlanStoryPolicy>,
    chronology: &BTreeMap<String, super::story::ChronologyPlacement>,
    operation: &EditPlanOperation,
) -> Result<usize> {
    let Some(placement) = chronology.get(&operation.asset_id) else {
        return Err(placement_error(
            "edit_plan_chronology_unresolved",
            format!(
                "cut {} has no solved source chronology",
                operation.operation_id
            ),
        ));
    };
    let Some(new_millis) = placement.trip_time_millis else {
        return Err(placement_error(
            "edit_plan_chronology_unresolved",
            format!(
                "cut {} has no solved trip time; automatic insertion cannot infer filenames or retrieval rank as story order",
                operation.operation_id
            ),
        ));
    };
    let units = placement_units(cuts, policy, chronology)?;
    let before_edges = super::story::backward_edge_count(cuts, chronology, policy);
    let mut slot = cuts.len();
    for unit in &units {
        if new_millis < unit.min_millis {
            slot = unit.start;
            break;
        }
        if new_millis > unit.max_millis {
            continue;
        }
        // The new source time is inside this unit's chronology envelope. A
        // protected group is atomic, so ordinary insertion must not split it.
        if unit.cuts.len() > 1 {
            return Err(placement_error(
                "edit_plan_chronology_conflict",
                format!(
                    "Corrected source time places cut {} inside protected group unit at position {}. Choose an explicit reviewed placement with a scoped exception, or move the group.",
                    operation.operation_id, unit.start
                ),
            ));
        }
        let existing = &unit.cuts[0];
        if existing.asset_id == operation.asset_id
            && operation.source_out_seconds <= existing.source_in_seconds + 1e-9
        {
            slot = unit.start;
            break;
        }
        if existing.asset_id == operation.asset_id
            && operation.source_in_seconds >= existing.source_out_seconds - 1e-9
        {
            slot = unit.end;
            continue;
        }
        return Err(placement_error(
            "edit_plan_chronology_ambiguous",
            format!(
                "Cut {} and retained cut {} have the same corrected source time without an exact source-range order. Supply a narrowly scoped story exception.",
                operation.operation_id, existing.operation_id
            ),
        ));
    }
    if slot < units.first().map_or(0, |unit| unit.start) {
        // Never move an ordinary addition ahead of the exact teaser prefix.
        slot = units.first().map_or(0, |unit| unit.start);
    }

    let mut candidate = cuts.to_vec();
    let at = slot.min(candidate.len());
    candidate.insert(at, operation.clone());
    let after_edges = super::story::backward_edge_count(&candidate, chronology, policy);
    if after_edges > before_edges {
        let earlier = at.checked_sub(1).and_then(|left| candidate.get(left));
        let later = candidate.get(at + 1);
        return Err(placement_error(
            "edit_plan_chronology_conflict",
            format!(
                "Automatic placement of cut {} between {} and {} would create an uncovered backward-time join. Supply an exact scoped exception or choose a different story range.",
                operation.operation_id,
                earlier.map_or("the opening teaser", |cut| cut.operation_id.as_str()),
                later.map_or("the end", |cut| cut.operation_id.as_str())
            ),
        ));
    }
    Ok(at)
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
        candidate.story_policy = input
            .story_policy
            .clone()
            .or_else(|| base.story_policy.clone());
        let cuts = &mut candidate.operations;
        let mut touched = BTreeSet::new();
        let mut explicit_audio = BTreeSet::new();
        let mut lineage = Vec::new();
        let mut automatic: Vec<EditPlanOperation> = Vec::new();
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
                    explicit_audio.insert(id.clone());
                    match a {
                        Some(a) => {
                            let at = anchor(cuts, a)?;
                            cuts.insert(at, operation.clone());
                            lineage.push(PatchLineage {
                                operation_id: id.clone(),
                                parent_operation_id: None,
                                kind: "insert".into(),
                            });
                        }
                        None => automatic.push(operation.clone()),
                    }
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

        if !automatic.is_empty() {
            let facts = self.chronology_facts(project)?;
            let solution = crate::chronology::solve(&facts);
            let chronology = super::story::placement_index(&solution);
            // Batch additions are one deterministic group. Incoming
            // enumeration order is not story order.
            automatic.sort_by(|left, right| {
                let left_time = chronology
                    .get(&left.asset_id)
                    .and_then(|placement| placement.trip_time_millis)
                    .unwrap_or(i64::MAX);
                let right_time = chronology
                    .get(&right.asset_id)
                    .and_then(|placement| placement.trip_time_millis)
                    .unwrap_or(i64::MAX);
                left_time
                    .cmp(&right_time)
                    .then_with(|| left.asset_id.cmp(&right.asset_id))
                    .then_with(|| left.source_in_seconds.total_cmp(&right.source_in_seconds))
                    .then_with(|| left.source_out_seconds.total_cmp(&right.source_out_seconds))
                    .then_with(|| left.operation_id.cmp(&right.operation_id))
            });
            for pair in automatic.windows(2) {
                let left = chronology
                    .get(&pair[0].asset_id)
                    .and_then(|placement| placement.trip_time_millis);
                let right = chronology
                    .get(&pair[1].asset_id)
                    .and_then(|placement| placement.trip_time_millis);
                if let (Some(left), Some(right)) = (left, right)
                    && left == right
                    && pair[0].asset_id != pair[1].asset_id
                {
                    return Err(placement_error(
                        "edit_plan_chronology_ambiguous",
                        format!(
                            "Incoming cuts {} and {} have the same corrected source time. A batch cannot guess their story order from enumeration order.",
                            pair[0].operation_id, pair[1].operation_id
                        ),
                    ));
                }
            }
            for operation in automatic {
                let at = automatic_insert_slot(
                    cuts,
                    candidate.story_policy.as_ref(),
                    &chronology,
                    &operation,
                )?;
                cuts.insert(at, operation.clone());
                lineage.push(PatchLineage {
                    operation_id: operation.operation_id,
                    parent_operation_id: None,
                    kind: "insert".into(),
                });
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
        // Canonical lineage order follows final immutable sequence position.
        let positions: BTreeMap<_, _> = cuts
            .iter()
            .enumerate()
            .map(|(position, cut)| (cut.operation_id.clone(), position))
            .collect();
        lineage.sort_by_key(|item| {
            positions
                .get(&item.operation_id)
                .copied()
                .unwrap_or(usize::MAX)
        });

        self.check_revision_inputs(project, cuts, Some(base_id))?;
        if let Some(policy) = &mut candidate.story_policy {
            let facts = self.chronology_facts(project)?;
            if let Err(error) = super::story::stamp_policy_dependencies(policy, &facts) {
                return Err(super::EditPlanError::StaleStoryScope {
                    message: error.to_string(),
                }
                .into());
            }
        }
        let validation = self.validate_edit_plan_candidate(&candidate, &|| false)?;
        if !validation.renderable() {
            return Err(super::validate::validation_error(&validation).into());
        }
        let key = super::ranges::constraints_key(&self.connection()?, &candidate)?;
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        super::require_active(&tx, project, expected_active)?;
        ensure!(
            super::ranges::constraints_key(&tx, &candidate)? == key,
            "stale evidence: constraints changed while validating patch"
        );
        // Recheck chronology and exact scope under the write reservation.
        let facts = crate::chronology::load_chronology_facts(&tx, project)?;
        let solution = crate::chronology::solve(&facts);
        if let Some(policy) = candidate.story_policy.as_mut()
            && let Err(error) = super::story::stamp_policy_dependencies(policy, &facts)
        {
            return Err(super::EditPlanError::StaleStoryScope {
                message: error.to_string(),
            }
            .into());
        }
        if candidate.story_policy.is_some() {
            let issues = super::story::validate_revision_chronology(&candidate, &facts, &solution);
            if issues.iter().any(|issue| issue.kind.is_error()) {
                return Err(super::EditPlanError::StoryPolicyInvalid {
                    message: "story policy or chronology changed while patch was validating".into(),
                }
                .into());
            }
            if candidate
                .story_policy
                .as_ref()
                .is_some_and(|policy| policy.is_empty())
                && !solution.contradictions.is_empty()
            {
                return Err(super::EditPlanError::ChronologyConflict {
                    message: "project chronology changed while patch was validating".into(),
                }
                .into());
            }
        }
        let (id, mut operation) = super::revision_operation_with_story_policy(
            &tx,
            project,
            &candidate.operations,
            base.script_revision_id.as_deref(),
            Some(base_id),
            candidate.story_policy.as_ref(),
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
