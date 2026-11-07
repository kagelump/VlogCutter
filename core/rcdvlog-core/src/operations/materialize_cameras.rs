//! Materializers for camera identity merges and camera asset repairs.

use super::*;

pub(crate) fn resolve_camera_alias(
    transaction: &Transaction<'_>,
    project_id: &str,
    camera: &str,
) -> Result<String> {
    let mut current = camera.to_string();
    let mut seen = BTreeSet::new();
    while seen.insert(current.clone()) {
        let next = transaction
            .query_row(
                "SELECT destination_camera FROM camera_identity_aliases WHERE project_id=?1 AND source_camera=?2",
                params![project_id, current],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match next {
            Some(next) => current = next,
            None => return Ok(current),
        }
    }
    bail!("camera identity aliases contain a cycle for {camera}")
}

pub(super) fn materialize_camera_identity_merged(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: CameraIdentityMergedPayload = payload(operation)?;
    if operation.aggregate_type != "cameraIdentity"
        || operation.aggregate_id != payload.source_camera
    {
        bail!("cameraIdentityMerged aggregate must be the source camera");
    }
    if payload.source_camera.trim().is_empty()
        || payload.destination_camera.trim().is_empty()
        || payload.source_camera == payload.destination_camera
    {
        bail!("cameraIdentityMerged requires two distinct non-empty camera identities");
    }
    let asset_ids = payload.asset_ids.iter().collect::<BTreeSet<_>>();
    let capture_sequence_ids = payload.capture_sequence_ids.iter().collect::<BTreeSet<_>>();
    if asset_ids.len() != payload.asset_ids.len()
        || capture_sequence_ids.len() != payload.capture_sequence_ids.len()
    {
        bail!("cameraIdentityMerged requires unique asset and capture-sequence identities");
    }
    if payload
        .assignments
        .iter()
        .map(|item| &item.asset_id)
        .collect::<BTreeSet<_>>()
        .len()
        != payload.assignments.len()
        || payload.assignments.iter().any(|item| {
            !asset_ids.contains(&item.asset_id)
                || !capture_sequence_ids.contains(&item.capture_sequence_id)
        })
    {
        bail!(
            "cameraIdentityMerged assignments must uniquely reference confirmed assets and sequences"
        );
    }
    transaction.execute(
        "INSERT OR IGNORE INTO camera_identity_merge_facts(operation_id, project_id, source_camera, destination_camera, hlc_wall_millis, hlc_counter, origin_node_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![operation.operation_id, operation.project_id, payload.source_camera, payload.destination_camera,
            operation.logical_time.wall_millis, operation.logical_time.counter as i64, operation.origin_node_id],
    )?;
    recompute_camera_identity_component(
        transaction,
        &operation.project_id,
        &payload.source_camera,
        &payload.destination_camera,
    )?;
    for assignment in &payload.assignments {
        let sequence_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM capture_sequences WHERE id=?1 AND project_id=?2)",
            params![assignment.capture_sequence_id, operation.project_id],
            |row| row.get(0),
        )?;
        if !sequence_exists {
            bail!(
                "cameraIdentityMerged capture sequence not found during replay: {}",
                assignment.capture_sequence_id
            );
        }
        transaction.execute(
            "UPDATE asset_capture_sequence SET capture_sequence_id=?1 WHERE asset_id=?2 AND project_id=?3",
            params![assignment.capture_sequence_id, assignment.asset_id, operation.project_id],
        )?;
    }
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

pub(super) fn materialize_camera_assets_repaired(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: CameraAssetsRepairedPayload = payload(operation)?;
    if operation.aggregate_type != "cameraRepair" || operation.aggregate_id != "assets" {
        bail!("cameraAssetsRepaired requires the cameraRepair/assets aggregate");
    }
    if payload.camera.trim().is_empty() || payload.assignments.is_empty() {
        bail!("cameraAssetsRepaired requires a camera and at least one assignment");
    }
    if payload.assignments.iter().any(|assignment| {
        assignment
            .filename_counter
            .is_some_and(|counter| i64::try_from(counter).is_err())
    }) {
        bail!("cameraAssetsRepaired filename counter exceeds SQLite integer range");
    }
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    let mut ids = BTreeSet::new();
    let mut sequences = BTreeSet::new();
    for assignment in &payload.assignments {
        if !ids.insert(&assignment.asset_id) || assignment.capture_sequence_id.trim().is_empty() {
            bail!("cameraAssetsRepaired assignments must have unique assets and valid sequences");
        }
        sequences.insert(assignment.capture_sequence_id.as_str());
    }
    for sequence in &payload.new_sequences {
        if sequence.id.trim().is_empty() || sequence.camera_key != payload.camera {
            bail!("cameraAssetsRepaired new sequence has invalid identity");
        }
        let existing: Option<(String,String,String,String,String)> = transaction.query_row(
            "SELECT project_id,camera_key,label,volume_hint,filename_prefix FROM capture_sequences WHERE id=?1",
            [&sequence.id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
        ).optional()?;
        if let Some(existing) = existing {
            if existing
                != (
                    operation.project_id.clone(),
                    sequence.camera_key.clone(),
                    sequence.label.clone(),
                    sequence.volume_hint.clone(),
                    sequence.filename_prefix.clone(),
                )
            {
                bail!("cameraAssetsRepaired destination sequence identity collision");
            }
        } else {
            transaction.execute(
                "INSERT INTO capture_sequences(id,project_id,camera_key,label,volume_hint,filename_prefix,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![sequence.id, operation.project_id, sequence.camera_key, sequence.label, sequence.volume_hint, sequence.filename_prefix, now()],
            )?;
        }
    }
    let new_sequence_ids = payload
        .new_sequences
        .iter()
        .map(|s| s.id.as_str())
        .collect::<BTreeSet<_>>();
    for sequence_id in sequences {
        if new_sequence_ids.contains(sequence_id) {
            continue;
        }
        let total: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM asset_capture_sequence WHERE project_id=?1 AND capture_sequence_id=?2",
            params![operation.project_id, sequence_id], |row| row.get(0))?;
        let selected = payload
            .assignments
            .iter()
            .filter(|a| a.capture_sequence_id == sequence_id)
            .count() as i64;
        if total != selected {
            bail!("cameraAssetsRepaired must select every asset in each affected capture sequence");
        }
        transaction.execute(
            "UPDATE capture_sequences SET camera_key=?1 WHERE project_id=?2 AND id=?3",
            params![payload.camera, operation.project_id, sequence_id],
        )?;
    }
    for assignment in &payload.assignments {
        let affected = transaction.execute(
            "UPDATE assets SET camera=?1 WHERE id=?2 AND project_id=?3",
            params![payload.camera, assignment.asset_id, operation.project_id],
        )?;
        if affected != 1 {
            bail!(
                "cameraAssetsRepaired asset is missing during replay: {}",
                assignment.asset_id
            );
        }
        let affected = transaction.execute(
            "UPDATE asset_capture_sequence SET capture_sequence_id=?1, filename_counter=?2 WHERE asset_id=?3 AND project_id=?4 AND capture_sequence_id=?5",
            params![assignment.capture_sequence_id, assignment.filename_counter.map(|value| value as i64), assignment.asset_id, operation.project_id, assignment.source_capture_sequence_id],
        )?;
        if affected != 1 {
            bail!(
                "cameraAssetsRepaired sequence membership is missing: {}",
                assignment.asset_id
            );
        }
        let mut affected_aggregates = vec![
            ("projectAsset", assignment.asset_id.as_str()),
            (
                "captureSequence",
                assignment.source_capture_sequence_id.as_str(),
            ),
        ];
        if assignment.source_capture_sequence_id != assignment.capture_sequence_id {
            affected_aggregates.push(("captureSequence", assignment.capture_sequence_id.as_str()));
        }
        for (aggregate_type, aggregate_id) in affected_aggregates {
            transaction.execute(
                "INSERT INTO materialized_revisions(project_id,aggregate_type,aggregate_id,revision,last_operation_id) VALUES (?1,?2,?3,1,?4) ON CONFLICT(project_id,aggregate_type,aggregate_id) DO UPDATE SET revision=revision+1,last_operation_id=excluded.last_operation_id",
                params![operation.project_id, aggregate_type, aggregate_id, operation.operation_id],
            )?;
        }
    }
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

/// Rebuilds the entire undirected merge component from durable facts. The
/// destination of the fact with the greatest operation key is canonical, so
/// chained and even opposing concurrent merges converge regardless of delivery
/// order. Aliases are flattened and every camera-bearing projection follows.
pub(super) fn recompute_camera_identity_component(
    transaction: &Transaction<'_>,
    project_id: &str,
    source: &str,
    destination: &str,
) -> Result<String> {
    type MergeFact = (String, String, (i64, i64, String, String));
    let mut statement = transaction.prepare(
        "SELECT source_camera, destination_camera, hlc_wall_millis, hlc_counter, origin_node_id, operation_id FROM camera_identity_merge_facts WHERE project_id=?1",
    )?;
    let facts = statement
        .query_map([project_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                (row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?),
            ))
        })?
        .collect::<rusqlite::Result<Vec<MergeFact>>>()?;
    drop(statement);

    let mut component = BTreeSet::from([source.to_string(), destination.to_string()]);
    loop {
        let before = component.len();
        for (left, right, _) in &facts {
            if component.contains(left) || component.contains(right) {
                component.insert(left.clone());
                component.insert(right.clone());
            }
        }
        if component.len() == before {
            break;
        }
    }
    let (_, canonical, winning_key) = facts
        .iter()
        .filter(|(left, right, _)| component.contains(left) && component.contains(right))
        .max_by_key(|(_, _, key)| key)
        .cloned()
        .ok_or_else(|| anyhow!("camera identity merge fact was not persisted"))?;

    for member in &component {
        transaction.execute(
            "DELETE FROM camera_identity_aliases WHERE project_id=?1 AND source_camera=?2",
            params![project_id, member],
        )?;
    }
    for member in component.iter().filter(|member| *member != &canonical) {
        transaction.execute(
            "INSERT INTO camera_identity_aliases(project_id, source_camera, destination_camera, operation_id, hlc_wall_millis, hlc_counter, origin_node_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![project_id, member, canonical, winning_key.3, winning_key.0, winning_key.1, winning_key.2],
        )?;
    }
    for member in &component {
        transaction.execute(
            "UPDATE assets SET camera=?1 WHERE project_id=?2 AND camera=?3",
            params![canonical, project_id, member],
        )?;
        transaction.execute(
            "UPDATE capture_sequences SET camera_key=?1 WHERE project_id=?2 AND camera_key=?3",
            params![canonical, project_id, member],
        )?;
        transaction.execute(
            "UPDATE clock_events SET camera=?1 WHERE project_id=?2 AND camera=?3",
            params![canonical, project_id, member],
        )?;
        if crate::table_exists(transaction, "camera_identity_mappings")? {
            transaction.execute(
                "UPDATE camera_identity_mappings SET camera=?1, updated_at=?2 WHERE project_id=?3 AND camera=?4",
                params![canonical, now(), project_id, member],
            )?;
        }
    }
    recalculate_clock(transaction, project_id, &canonical)?;
    Ok(canonical)
}
