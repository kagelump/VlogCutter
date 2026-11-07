//! Materializers for capture-sequence declare/split/merge/reassign operations.

use super::*;

pub(super) fn materialize_capture_sequence_declared(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: CaptureSequenceDeclaredPayload = payload(operation)?;
    if operation.aggregate_type != "captureSequence"
        || operation.aggregate_id != payload.sequence_id
    {
        bail!("captureSequenceDeclared aggregate must be the declared capture sequence");
    }
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    let camera_key = resolve_camera_alias(transaction, &operation.project_id, &payload.camera_key)?;
    transaction.execute(
        "INSERT INTO capture_sequences(id, project_id, camera_key, label, volume_hint, filename_prefix, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(id) DO UPDATE SET camera_key=excluded.camera_key, label=excluded.label, volume_hint=excluded.volume_hint, filename_prefix=excluded.filename_prefix",
        params![
            payload.sequence_id,
            operation.project_id,
            camera_key,
            payload.label,
            payload.volume_hint,
            payload.filename_prefix,
            now(),
        ],
    )?;
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

pub(super) fn materialize_capture_sequence_split(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: CaptureSequenceSplitPayload = payload(operation)?;
    if operation.aggregate_type != "captureSequence"
        || operation.aggregate_id == payload.new_sequence_id
    {
        bail!("captureSequenceSplit aggregate must be the source capture sequence");
    }
    // A destination id that already names a sequence in this project (for
    // example another camera's derived id) must never receive this split's
    // assets: `INSERT OR IGNORE` would silently no-op and the UPDATE below
    // would move `filename_counter >= at_counter` assets into an unrelated
    // sequence. Refuse before any write.
    let destination_exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM capture_sequences WHERE id=?1 AND project_id=?2)",
        params![payload.new_sequence_id, operation.project_id],
        |row| row.get(0),
    )?;
    if destination_exists {
        bail!("captureSequenceSplit destination sequence already exists");
    }
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    let source_exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM capture_sequences WHERE id=?1 AND project_id=?2)",
        params![operation.aggregate_id, operation.project_id],
        |row| row.get(0),
    )?;
    if !source_exists {
        bail!(
            "captureSequenceSplit source sequence not found: {}",
            operation.aggregate_id
        );
    }
    let camera_key = resolve_camera_alias(transaction, &operation.project_id, &payload.camera_key)?;
    transaction.execute(
        "INSERT OR IGNORE INTO capture_sequences(id, project_id, camera_key, label, volume_hint, filename_prefix, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            payload.new_sequence_id,
            operation.project_id,
            camera_key,
            payload.label,
            payload.volume_hint,
            payload.filename_prefix,
            now(),
        ],
    )?;
    transaction.execute(
        "UPDATE asset_capture_sequence SET capture_sequence_id=?1 WHERE project_id=?2 AND capture_sequence_id=?3 AND filename_counter >= ?4",
        params![
            payload.new_sequence_id,
            operation.project_id,
            operation.aggregate_id,
            payload.at_counter as i64,
        ],
    )?;
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

/// The operation's wall clock as RFC 3339, for columns that record *when a
/// decision was made* rather than when a replay happened.
pub(super) fn hlc_timestamp(logical_time: &HybridLogicalTime) -> String {
    chrono::DateTime::from_timestamp_millis(logical_time.wall_millis)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Records the user's "this restart is not a card change" for one boundary.
///
/// Idempotent by primary key: replaying the log, or two devices declining the
/// same boundary, converges on one row. The decision is deliberately keyed by
/// counter rather than by the affected clip identities — a later import can add
/// clips on either side of the boundary, and the user's answer was about the
/// boundary, not about the clips that happened to be there when they gave it.
pub(super) fn materialize_capture_sequence_split_declined(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: CaptureSequenceSplitDeclinedPayload = payload(operation)?;
    if operation.aggregate_type != "captureSequence" {
        bail!("captureSequenceSplitDeclined aggregate must be a capture sequence");
    }
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    transaction.execute(
        "INSERT INTO declined_capture_sequence_splits(project_id, capture_sequence_id, at_counter, reason, declined_at) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(project_id, capture_sequence_id, at_counter) DO UPDATE SET reason=excluded.reason, declined_at=excluded.declined_at",
        rusqlite::params![
            &operation.project_id,
            &operation.aggregate_id,
            payload.at_counter as i64,
            payload.reason.trim(),
            // The operation's own clock, not the replaying machine's: two
            // devices materializing the same log must land on the same row.
            hlc_timestamp(&operation.logical_time),
        ],
    )?;
    Ok(ApplyOperationResult::Applied)
}

/// Puts a declined boundary back in front of the user.
pub(super) fn materialize_capture_sequence_split_restored(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: CaptureSequenceSplitRestoredPayload = payload(operation)?;
    if operation.aggregate_type != "captureSequence" {
        bail!("captureSequenceSplitRestored aggregate must be a capture sequence");
    }
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    transaction.execute(
        "DELETE FROM declined_capture_sequence_splits WHERE project_id=?1 AND capture_sequence_id=?2 AND at_counter=?3",
        rusqlite::params![
            &operation.project_id,
            &operation.aggregate_id,
            payload.at_counter as i64,
        ],
    )?;
    Ok(ApplyOperationResult::Applied)
}

pub(super) fn materialize_capture_sequence_split_exact(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: CaptureSequenceSplitExactPayload = payload(operation)?;
    if operation.aggregate_type != "captureSequence"
        || operation.aggregate_id == payload.new_sequence_id
    {
        bail!("captureSequenceSplitExact aggregate must be the source capture sequence");
    }
    validate_id("capture sequence ID", &payload.new_sequence_id)?;
    if payload.asset_ids.is_empty() {
        bail!("captureSequenceSplitExact requires at least one asset");
    }
    if payload.asset_ids.iter().collect::<BTreeSet<_>>().len() != payload.asset_ids.len() {
        bail!("captureSequenceSplitExact requires unique asset identities");
    }
    let destination_exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM capture_sequences WHERE id=?1)",
        [&payload.new_sequence_id],
        |row| row.get(0),
    )?;
    if destination_exists {
        bail!("captureSequenceSplitExact destination sequence already exists");
    }
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    let source_details = transaction
        .query_row(
            "SELECT camera_key, volume_hint, filename_prefix FROM capture_sequences WHERE id=?1 AND project_id=?2",
            params![operation.aggregate_id, operation.project_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            anyhow!(
                "captureSequenceSplitExact source sequence not found: {}",
                operation.aggregate_id
            )
        })?;
    let camera_key = resolve_camera_alias(transaction, &operation.project_id, &payload.camera_key)?;
    if source_details
        != (
            camera_key.clone(),
            payload.volume_hint.clone(),
            payload.filename_prefix.clone(),
        )
    {
        bail!("captureSequenceSplitExact destination details do not match the source sequence");
    }
    let boundary_counter = i64::try_from(payload.at_counter)
        .map_err(|_| anyhow!("captureSequenceSplitExact counter exceeds SQLite integer range"))?;
    let mut includes_boundary = false;
    for asset_id in &payload.asset_ids {
        let counter = transaction
            .query_row(
                "SELECT filename_counter FROM asset_capture_sequence WHERE asset_id=?1 AND project_id=?2 AND capture_sequence_id=?3",
                params![asset_id, operation.project_id, operation.aggregate_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .ok_or_else(|| {
                anyhow!(
                    "captureSequenceSplitExact asset {asset_id} is not in source sequence {}",
                    operation.aggregate_id
                )
            })?;
        includes_boundary |= counter == Some(boundary_counter);
    }
    if !includes_boundary {
        bail!(
            "captureSequenceSplitExact selected assets do not include counter {}",
            payload.at_counter
        );
    }
    transaction.execute(
        "INSERT INTO capture_sequences(id, project_id, camera_key, label, volume_hint, filename_prefix, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            payload.new_sequence_id,
            operation.project_id,
            camera_key,
            payload.label,
            payload.volume_hint,
            payload.filename_prefix,
            now(),
        ],
    )?;
    for asset_id in &payload.asset_ids {
        transaction.execute(
            "UPDATE asset_capture_sequence SET capture_sequence_id=?1 WHERE asset_id=?2 AND project_id=?3 AND capture_sequence_id=?4",
            params![
                payload.new_sequence_id,
                asset_id,
                operation.project_id,
                operation.aggregate_id,
            ],
        )?;
    }
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

pub(super) fn materialize_capture_sequence_merged(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: CaptureSequenceMergedPayload = payload(operation)?;
    if operation.aggregate_type != "captureSequence"
        || operation.aggregate_id != payload.into_sequence_id
    {
        bail!("captureSequenceMerged aggregate must be the destination capture sequence");
    }
    // A self-merge must be refused before any write: with `from == into` both
    // UPDATEs are no-ops and the DELETE below cascade-destroys every
    // `asset_capture_sequence` and `clock_epochs` row of the sequence
    // (cascades declared in migrations.rs).
    if payload.from_sequence_id == payload.into_sequence_id {
        bail!("captureSequenceMerged requires two distinct capture sequences");
    }
    let exact_identities = match (&payload.asset_ids, &payload.epoch_ids) {
        (Some(asset_ids), Some(epoch_ids)) => Some((asset_ids, epoch_ids)),
        (None, None) => None,
        _ => bail!("captureSequenceMerged exact payload requires both assetIds and epochIds"),
    };
    if let Some((asset_ids, epoch_ids)) = exact_identities {
        if asset_ids.iter().collect::<BTreeSet<_>>().len() != asset_ids.len() {
            bail!("captureSequenceMerged requires unique asset identities");
        }
        if epoch_ids.iter().collect::<BTreeSet<_>>().len() != epoch_ids.len() {
            bail!("captureSequenceMerged requires unique epoch identities");
        }
    }
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    let into_exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM capture_sequences WHERE id=?1 AND project_id=?2)",
        params![payload.into_sequence_id, operation.project_id],
        |row| row.get(0),
    )?;
    if !into_exists {
        bail!(
            "captureSequenceMerged destination sequence not found: {}",
            payload.into_sequence_id
        );
    }
    let from_exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM capture_sequences WHERE id=?1 AND project_id=?2)",
        params![payload.from_sequence_id, operation.project_id],
        |row| row.get(0),
    )?;
    if !from_exists {
        bail!(
            "captureSequenceMerged source sequence not found: {}",
            payload.from_sequence_id
        );
    }
    if let Some((asset_ids, epoch_ids)) = exact_identities {
        // Validate every confirmed identity before the first write so one
        // stale/malformed identity cannot produce a partial merge.
        for asset_id in asset_ids {
            let belongs_to_source: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM asset_capture_sequence WHERE asset_id=?1 AND project_id=?2 AND capture_sequence_id=?3)",
                params![asset_id, operation.project_id, payload.from_sequence_id],
                |row| row.get(0),
            )?;
            if !belongs_to_source {
                bail!(
                    "captureSequenceMerged asset {asset_id} is not in source sequence {}",
                    payload.from_sequence_id
                );
            }
        }
        for epoch_id in epoch_ids {
            let belongs_to_source: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM clock_epochs WHERE id=?1 AND project_id=?2 AND capture_sequence_id=?3)",
                params![epoch_id, operation.project_id, payload.from_sequence_id],
                |row| row.get(0),
            )?;
            if !belongs_to_source {
                bail!(
                    "captureSequenceMerged epoch {epoch_id} is not in source sequence {}",
                    payload.from_sequence_id
                );
            }
        }
        for asset_id in asset_ids {
            transaction.execute(
                "UPDATE asset_capture_sequence SET capture_sequence_id=?1 WHERE asset_id=?2 AND project_id=?3 AND capture_sequence_id=?4",
                params![
                    payload.into_sequence_id,
                    asset_id,
                    operation.project_id,
                    payload.from_sequence_id,
                ],
            )?;
        }
        for epoch_id in epoch_ids {
            transaction.execute(
                "UPDATE clock_epochs SET capture_sequence_id=?1 WHERE id=?2 AND project_id=?3 AND capture_sequence_id=?4",
                params![
                    payload.into_sequence_id,
                    epoch_id,
                    operation.project_id,
                    payload.from_sequence_id,
                ],
            )?;
        }
        // Concurrent/later facts that were not in the preview stay in their
        // source sequence. Delete it only when the exact move emptied it.
        let source_has_facts: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM asset_capture_sequence WHERE project_id=?1 AND capture_sequence_id=?2 UNION ALL SELECT 1 FROM clock_epochs WHERE project_id=?1 AND capture_sequence_id=?2)",
            params![operation.project_id, payload.from_sequence_id],
            |row| row.get(0),
        )?;
        if !source_has_facts {
            transaction.execute(
                "DELETE FROM capture_sequences WHERE id=?1 AND project_id=?2",
                params![payload.from_sequence_id, operation.project_id],
            )?;
        }
    } else {
        // Legacy counter-era merge operations intentionally retain their
        // original whole-sequence semantics for deterministic old-log replay.
        transaction.execute(
            "UPDATE asset_capture_sequence SET capture_sequence_id=?1 WHERE project_id=?2 AND capture_sequence_id=?3",
            params![payload.into_sequence_id, operation.project_id, payload.from_sequence_id],
        )?;
        transaction.execute(
            "UPDATE clock_epochs SET capture_sequence_id=?1 WHERE project_id=?2 AND capture_sequence_id=?3",
            params![payload.into_sequence_id, operation.project_id, payload.from_sequence_id],
        )?;
        transaction.execute(
            "DELETE FROM capture_sequences WHERE id=?1 AND project_id=?2",
            params![payload.from_sequence_id, operation.project_id],
        )?;
    }
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

pub(super) fn materialize_capture_sequence_reassigned(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: CaptureSequenceReassignedPayload = payload(operation)?;
    if operation.aggregate_type != "captureSequence"
        || operation.aggregate_id != payload.from_sequence_id
    {
        bail!("captureSequenceReassigned aggregate must be the source capture sequence");
    }
    if payload.from_sequence_id == payload.into_sequence_id {
        bail!("captureSequenceReassigned requires two distinct capture sequences");
    }
    if payload.asset_ids.is_empty() {
        bail!("captureSequenceReassigned requires at least one asset");
    }
    if payload.asset_ids.iter().collect::<BTreeSet<_>>().len() != payload.asset_ids.len() {
        bail!("captureSequenceReassigned requires unique asset identities");
    }
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    let destination_exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM capture_sequences WHERE id=?1 AND project_id=?2)",
        params![payload.into_sequence_id, operation.project_id],
        |row| row.get(0),
    )?;
    if !destination_exists {
        bail!(
            "captureSequenceReassigned destination sequence not found: {}",
            payload.into_sequence_id
        );
    }
    for asset_id in &payload.asset_ids {
        let belongs_to_source: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM asset_capture_sequence WHERE asset_id=?1 AND project_id=?2 AND capture_sequence_id=?3)",
            params![asset_id, operation.project_id, payload.from_sequence_id],
            |row| row.get(0),
        )?;
        if !belongs_to_source {
            bail!(
                "captureSequenceReassigned asset {asset_id} is not in source sequence {}",
                payload.from_sequence_id
            );
        }
    }
    for asset_id in &payload.asset_ids {
        transaction.execute(
            "UPDATE asset_capture_sequence SET capture_sequence_id=?1 WHERE asset_id=?2 AND project_id=?3 AND capture_sequence_id=?4",
            params![
                payload.into_sequence_id,
                asset_id,
                operation.project_id,
                payload.from_sequence_id,
            ],
        )?;
    }
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}
