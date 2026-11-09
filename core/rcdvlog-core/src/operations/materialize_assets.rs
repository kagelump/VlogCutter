//! Materializers for notes, labels, review flags, asset registration, membership, and media links.

use super::*;

pub(super) fn materialize_note(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<bool> {
    let payload: NotePayload = payload(operation)?;
    require_asset(transaction, operation)?;
    transaction.execute(
        "INSERT INTO note_revisions(revision_id, project_id, asset_id, operation_id, text, hlc_wall_millis, hlc_counter, origin_node_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![payload.revision_id, operation.project_id, operation.aggregate_id, operation.operation_id, payload.text,
            operation.logical_time.wall_millis, operation.logical_time.counter as i64, operation.origin_node_id, now()],
    )?;
    let active: String = transaction.query_row(
        "SELECT text FROM note_revisions WHERE asset_id = ?1 ORDER BY hlc_wall_millis DESC, hlc_counter DESC, origin_node_id DESC, revision_id DESC LIMIT 1",
        [&operation.aggregate_id], |row| row.get(0))?;
    transaction.execute(
        "UPDATE assets SET notes = ?2 WHERE id = ?1",
        params![operation.aggregate_id, active],
    )?;
    Ok(true)
}

pub(super) fn materialize_label_add(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<bool> {
    let payload: LabelAddPayload = payload(operation)?;
    require_asset(transaction, operation)?;
    let labels = normalize_labels(std::slice::from_ref(&payload.label))?;
    let label = labels
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("label cannot be empty"))?;
    transaction.execute(
        "INSERT INTO label_tags(add_tag, project_id, asset_id, normalized_label, display_label, operation_id, hlc_wall_millis, hlc_counter, origin_node_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![payload.add_tag, operation.project_id, operation.aggregate_id, label_key(&label), label, operation.operation_id,
            operation.logical_time.wall_millis, operation.logical_time.counter as i64, operation.origin_node_id, now()],
    )?;
    refresh_labels(transaction, &operation.aggregate_id)?;
    Ok(true)
}

pub(super) fn materialize_label_remove(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<bool> {
    let payload: LabelRemovePayload = payload(operation)?;
    require_asset(transaction, operation)?;
    if payload.normalized_label.is_empty()
        || label_key(&payload.normalized_label) != payload.normalized_label
    {
        bail!("labelRemoved normalized label is invalid");
    }
    for tag in payload.observed_add_tags {
        transaction.execute(
            "INSERT INTO label_tag_removals(remove_operation_id, add_tag, normalized_label) VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING",
            params![operation.operation_id, tag, payload.normalized_label],
        )?;
    }
    refresh_labels(transaction, &operation.aggregate_id)?;
    Ok(true)
}

pub(super) fn materialize_register(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
    field: &str,
) -> Result<bool> {
    require_asset(transaction, operation)?;
    let value = match field {
        "removed" => serde_json::to_value(payload::<RemovedPayload>(operation)?.removed)?,
        "rotationDegrees" => {
            let rotation = payload::<RotationPayload>(operation)?.rotation_degrees;
            if !matches!(rotation, 0 | 90 | 180 | 270) {
                bail!("invalid rotation: {rotation}; expected 0, 90, 180, or 270 degrees");
            }
            serde_json::to_value(rotation)?
        }
        "orientationDecision" => {
            let decision = payload::<OrientationDecisionPayload>(operation)?;
            validate_orientation_decision(&decision.status, decision.rotation_degrees)?;
            serde_json::to_value(OrientationDecision {
                status: decision.status,
                rotation_degrees: decision.rotation_degrees,
            })?
        }
        "membershipRemoved" => serde_json::Value::Bool(true),
        "membershipPresent" => {
            serde_json::to_value(payload::<MembershipPayload>(operation)?.present)?
        }
        _ => unreachable!(),
    };
    let wins = transaction.query_row(
        "SELECT hlc_wall_millis, hlc_counter, origin_node_id, COALESCE(operation_id, '') FROM lww_registers WHERE project_id=?1 AND aggregate_type=?2 AND aggregate_id=?3 AND field_name=?4",
        params![operation.project_id, operation.aggregate_type, operation.aggregate_id, field],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)),
    ).optional()?.is_none_or(|current| operation_key(operation) > current);
    if wins {
        transaction.execute(
            "INSERT INTO lww_registers(project_id, aggregate_type, aggregate_id, field_name, value_json, operation_id, hlc_wall_millis, hlc_counter, origin_node_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) ON CONFLICT(project_id, aggregate_type, aggregate_id, field_name) DO UPDATE SET value_json=excluded.value_json, operation_id=excluded.operation_id, hlc_wall_millis=excluded.hlc_wall_millis, hlc_counter=excluded.hlc_counter, origin_node_id=excluded.origin_node_id",
            params![operation.project_id, operation.aggregate_type, operation.aggregate_id, field, serde_json::to_string(&value)?, operation.operation_id,
                operation.logical_time.wall_millis, operation.logical_time.counter as i64, operation.origin_node_id],
        )?;
        match field {
            "removed" => {
                transaction.execute(
                    "UPDATE assets SET removed=?2 WHERE id=?1",
                    params![operation.aggregate_id, value.as_bool().unwrap()],
                )?;
            }
            "rotationDegrees" => {
                transaction.execute(
                    "UPDATE assets SET rotation_degrees=?2 WHERE id=?1",
                    params![operation.aggregate_id, value.as_u64().unwrap() as i64],
                )?;
            }
            "orientationDecision" => {
                let degrees = value
                    .get("rotationDegrees")
                    .and_then(serde_json::Value::as_u64)
                    .map(|degrees| degrees as i64);
                transaction.execute(
                    "UPDATE assets SET orientation_decision_json=?2 WHERE id=?1",
                    params![operation.aggregate_id, value.to_string()],
                )?;
                if let Some(degrees) = degrees {
                    let current = transaction.query_row(
                        "SELECT hlc_wall_millis,hlc_counter,origin_node_id,COALESCE(operation_id,'') FROM lww_registers WHERE project_id=?1 AND aggregate_type='projectAsset' AND aggregate_id=?2 AND field_name='rotationDegrees'",
                        params![operation.project_id, operation.aggregate_id],
                        |row| Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?)),
                    ).optional()?;
                    if current.is_none_or(|current| operation_key(operation) > current) {
                        transaction.execute(
                            "INSERT INTO lww_registers(project_id,aggregate_type,aggregate_id,field_name,value_json,operation_id,hlc_wall_millis,hlc_counter,origin_node_id) VALUES (?1,'projectAsset',?2,'rotationDegrees',?3,?4,?5,?6,?7) ON CONFLICT(project_id,aggregate_type,aggregate_id,field_name) DO UPDATE SET value_json=excluded.value_json,operation_id=excluded.operation_id,hlc_wall_millis=excluded.hlc_wall_millis,hlc_counter=excluded.hlc_counter,origin_node_id=excluded.origin_node_id",
                            params![operation.project_id, operation.aggregate_id, degrees.to_string(), operation.operation_id, operation.logical_time.wall_millis, operation.logical_time.counter as i64, operation.origin_node_id],
                        )?;
                        transaction.execute(
                            "UPDATE assets SET rotation_degrees=?2 WHERE id=?1",
                            params![operation.aggregate_id, degrees],
                        )?;
                    }
                }
            }
            "membershipRemoved" => {
                transaction.execute(
                    "UPDATE assets SET membership_removed=1 WHERE id=?1",
                    params![operation.aggregate_id],
                )?;
            }
            "membershipPresent" => {
                transaction.execute(
                    "UPDATE assets SET membership_removed=?2 WHERE id=?1",
                    params![operation.aggregate_id, !value.as_bool().unwrap()],
                )?;
            }
            _ => unreachable!(),
        }
    }
    Ok(true)
}

pub(super) fn materialize_asset_add(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<bool> {
    if !matches!(operation.payload_version, 1 | 2) {
        bail!(
            "unsupported payload version {} for {}",
            operation.payload_version,
            operation.operation_type
        );
    }
    let payload: ProjectAssetAddedPayload = serde_json::from_value(operation.payload.clone())?;
    if operation.payload_version == 2 && payload.timestamp_candidates.is_none() {
        bail!("projectAssetAdded payload version 2 requires timestamp candidate provenance");
    }
    if payload.id != operation.aggregate_id {
        bail!("projectAssetAdded payload ID does not match aggregate ID");
    }
    if operation.aggregate_type != "projectAsset" {
        bail!("projectAssetAdded requires a projectAsset aggregate");
    }
    let existing: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM assets WHERE id=?1)",
        [&payload.id],
        |row| row.get(0),
    )?;
    if existing {
        return Ok(true);
    }
    let camera = resolve_camera_alias(transaction, &operation.project_id, &payload.camera)?;
    transaction.execute(
        "INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, metadata_rotation_degrees, default_rotation_degrees, rotation_degrees, orientation_probed, created_at, timestamp_candidates_json) VALUES (?1, ?2, ?3, '', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15, 1, ?16, ?17)",
        params![payload.id, operation.project_id, payload.fingerprint, payload.filename, payload.size_bytes as i64,
            payload.duration_seconds, payload.frame_rate, payload.width, payload.height, payload.codec, camera,
            payload.captured_at_raw, payload.best_time, payload.metadata_rotation_degrees, payload.default_rotation_degrees, payload.created_at,
            payload.timestamp_candidates.as_ref().map(serde_json::to_string).transpose()?],
    )?;
    // Every asset belongs to exactly one capture sequence. A new asset joins
    // its camera's sequence (created on demand), using the same rule migration
    // 9 applies to existing assets. This keeps the chronology fact model
    // closed from migration 9 onward.
    crate::chronology::ensure_capture_sequence_for_camera(
        transaction,
        &operation.project_id,
        &camera,
        &payload.created_at,
    )?;
    let capture_sequence_id =
        crate::chronology::capture_sequence_id_for(&operation.project_id, &camera);
    let filename_counter = crate::chronology::parse_filename_counter(&payload.filename);
    transaction.execute(
        "INSERT OR IGNORE INTO asset_capture_sequence(asset_id, project_id, capture_sequence_id, filename_counter) VALUES (?1, ?2, ?3, ?4)",
        params![
            operation.aggregate_id,
            operation.project_id,
            capture_sequence_id,
            filename_counter.map(|value| value as i64),
        ],
    )?;
    transaction.execute(
        "INSERT INTO note_revisions(revision_id, project_id, asset_id, operation_id, text, hlc_wall_millis, hlc_counter, origin_node_id, created_at) VALUES (?1, ?2, ?3, NULL, '', 0, 0, 'baseline', ?4)",
        params![format!("baseline_note_{}", operation.aggregate_id), operation.project_id, operation.aggregate_id, payload.created_at],
    )?;
    for (field, value) in [
        ("removed", "false".to_string()),
        (
            "rotationDegrees",
            payload.default_rotation_degrees.to_string(),
        ),
        ("membershipPresent", "true".to_string()),
    ] {
        transaction.execute("INSERT INTO lww_registers(project_id, aggregate_type, aggregate_id, field_name, value_json, operation_id, hlc_wall_millis, hlc_counter, origin_node_id) VALUES (?1, 'projectAsset', ?2, ?3, ?4, NULL, 0, 0, 'baseline')",
            params![operation.project_id, operation.aggregate_id, field, value])?;
    }
    // A clock epoch or exception may have been committed before this clip was
    // ingested. Re-running a whole project on every item would make a large
    // card quadratic, so project the solver's governing fact onto this row
    // only. A clock-event write still recalculates its camera because it
    // intentionally changes every existing row.
    project_chronology_for_asset(
        transaction,
        &operation.project_id,
        &operation.aggregate_id,
        &capture_sequence_id,
        filename_counter,
        &payload.filename,
        payload.captured_at_raw.as_deref(),
    )?;
    Ok(true)
}

pub(super) fn materialize_membership_set(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<bool> {
    if operation.aggregate_type != "projectAsset" {
        bail!("projectAssetMembershipSet requires a projectAsset aggregate");
    }
    materialize_register(transaction, operation, "membershipPresent")
}

pub(super) fn materialize_media_link(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<bool> {
    let payload: MediaLinkedPayload = payload(operation)?;
    require_asset(transaction, operation)?;
    transaction.execute(
        "UPDATE assets SET media_object_id=?2, media_verification_state=?3 WHERE id=?1",
        params![
            operation.aggregate_id,
            payload.media_object_id,
            payload.verification_state.as_str()
        ],
    )?;
    Ok(true)
}
