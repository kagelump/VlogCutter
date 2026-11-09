//! Operation materialization dispatch plus the shared revision-guard and projection helpers.

use super::*;

/// Applies one already-persisted operation's materialization and revision
/// advance: the shared dispatch used by the insert path
/// (`insert_and_materialize`) and by the re-dispatch seam that materializes
/// quarantined chronology operations when a build that knows their type opens
/// the catalog. The caller decides whether the operation row was inserted in
/// this transaction; this function never touches `project_operations`.
pub(crate) fn materialize_operation(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let outcome = match operation.operation_type.as_str() {
        NOTE_REVISION_CREATED => materialize_note(transaction, operation)?,
        LABEL_ADDED => materialize_label_add(transaction, operation)?,
        LABEL_REMOVED => materialize_label_remove(transaction, operation)?,
        REMOVED_SET => materialize_register(transaction, operation, "removed")?,
        ROTATION_SET => materialize_register(transaction, operation, "rotationDegrees")?,
        ORIENTATION_DECISION_SET => {
            if !revision_guard_passes(transaction, operation)? {
                return Ok(ApplyOperationResult::Conflict);
            }
            materialize_register(transaction, operation, "orientationDecision")?
        }
        PROJECT_ASSET_MEMBERSHIP_SET => materialize_membership_set(transaction, operation)?,
        PROJECT_ASSET_ADDED => materialize_asset_add(transaction, operation)?,
        PROJECT_ASSET_MEDIA_LINKED => materialize_media_link(transaction, operation)?,
        CLOCK_EVENT_CREATED => return materialize_clock(transaction, operation),
        CAMERA_IDENTITY_MERGED => {
            return materialize_camera_identity_merged(transaction, operation);
        }
        CAMERA_ASSETS_REPAIRED => {
            return materialize_camera_assets_repaired(transaction, operation);
        }
        CAPTURE_SEQUENCE_DECLARED => {
            return materialize_capture_sequence_declared(transaction, operation);
        }
        CAPTURE_SEQUENCE_SPLIT => {
            return materialize_capture_sequence_split(transaction, operation);
        }
        CAPTURE_SEQUENCE_SPLIT_EXACT => {
            return materialize_capture_sequence_split_exact(transaction, operation);
        }
        CAPTURE_SEQUENCE_MERGED => {
            return materialize_capture_sequence_merged(transaction, operation);
        }
        CAPTURE_SEQUENCE_REASSIGNED => {
            return materialize_capture_sequence_reassigned(transaction, operation);
        }
        CAPTURE_SEQUENCE_SPLIT_DECLINED => {
            return materialize_capture_sequence_split_declined(transaction, operation);
        }
        CAPTURE_SEQUENCE_SPLIT_RESTORED => {
            return materialize_capture_sequence_split_restored(transaction, operation);
        }
        CLOCK_EPOCH_SET => return materialize_clock_epoch_set(transaction, operation),
        CLOCK_EPOCH_CLEARED => return materialize_clock_epoch_cleared(transaction, operation),
        CLOCK_QUESTION_DECISION_RECORDED => {
            return materialize_clock_question_decision(transaction, operation);
        }
        CLOCK_QUESTION_PROPOSED => {
            return materialize_clock_question_proposed(transaction, operation);
        }
        CLIP_TIME_EXCEPTION_SET => {
            return materialize_clip_time_exception_set(transaction, operation);
        }
        CLIP_TIME_EXCEPTION_CLEARED => {
            return materialize_clip_time_exception_cleared(transaction, operation);
        }
        TRIP_PHASE_SET => return materialize_trip_phase_set(transaction, operation),
        TRIP_PHASE_REMOVED => return materialize_trip_phase_removed(transaction, operation),
        EVENT_ORDER_CONSTRAINT_SET => {
            return materialize_event_order_constraint_set(transaction, operation);
        }
        EVENT_ORDER_CONSTRAINT_CLEARED => {
            return materialize_event_order_constraint_cleared(transaction, operation);
        }
        EDIT_PLAN_REVISION_CREATED => {
            return materialize_edit_plan_revision_created(transaction, operation);
        }
        EDIT_PLAN_REVISION_ACTIVATED => {
            return materialize_edit_plan_revision_activated(transaction, operation);
        }
        EDIT_PLAN_EXPORT_RECORDED => {
            return materialize_edit_plan_export_recorded(transaction, operation);
        }
        EDIT_REVIEW_NOTE | EDIT_REVIEW_PROPOSAL | EDIT_REVIEW_DECISION | EDIT_REVIEW_MAPPING => {
            return crate::editplan::feedback::materialize(transaction, operation);
        }
        SOURCE_RANGE_CONFIRMED | SOURCE_RANGE_REVOKED => {
            return crate::editplan::ranges::materialize_range_decision(transaction, operation);
        }
        _ => {
            // Unknown operation type: quarantine policy (D01-T007 decision 4).
            // The operation row is already persisted (by the caller's insert
            // path). It is retained and visible, never advances an aggregate
            // revision, and does not fail the containing batch.
            return Ok(ApplyOperationResult::Quarantined);
        }
    };
    if outcome {
        advance_revision(transaction, operation)?;
    }
    Ok(ApplyOperationResult::Applied)
}

/// The revision-guard comparison for one operation: `None` when the operation
/// declares no base revision (the guard passes unconditionally), otherwise
/// `Some((expected, actual))` from the aggregate's current revision.
pub(super) fn guard_comparison(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<Option<(u64, u64)>> {
    let Some(expected) = operation.base_revision else {
        return Ok(None);
    };
    let actual = current_revision(
        transaction,
        &operation.project_id,
        &operation.aggregate_type,
        &operation.aggregate_id,
    )?;
    Ok(Some((expected, actual)))
}

/// The same guard comparison without conflict recording: the re-dispatch
/// seam pre-checks candidates with it so a guard-failing row is skipped
/// silently and left in the log instead of writing a phantom
/// `operation_conflicts` row for a legitimately applied (but superseded)
/// operation — a phantom row would break baseline export/import watermark
/// comparison. Genuine stale-base resends on the normal apply path still
/// record their conflict rows through `revision_guard_passes`.
pub(crate) fn revision_guard_passes_without_recording(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<bool> {
    let Some((expected, actual)) = guard_comparison(transaction, operation)? else {
        return Ok(true);
    };
    Ok(expected == actual)
}

/// The `materialized_revisions` base-revision guard for the chronology
/// operations: when the operation declares a base revision it must equal the
/// aggregate's current revision, otherwise a durable conflict row is recorded
/// and the operation is refused without overwriting. An operation without a
/// declared base revision passes, matching the existing non-chronology
/// operations.
pub(crate) fn revision_guard_passes(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<bool> {
    let Some((expected, actual)) = guard_comparison(transaction, operation)? else {
        return Ok(true);
    };
    if expected == actual {
        return Ok(true);
    }
    let active = transaction
        .query_row(
            "SELECT last_operation_id FROM materialized_revisions WHERE project_id=?1 AND aggregate_type=?2 AND aggregate_id=?3",
            params![operation.project_id, operation.aggregate_type, operation.aggregate_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    transaction.execute(
        "INSERT INTO operation_conflicts(conflict_id, project_id, aggregate_type, aggregate_id, incoming_operation_id, active_operation_id, expected_base_revision, actual_revision, conflict_type, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'stale_base_revision', ?9)",
        params![
            format!("conflict_{}", Uuid::new_v4().simple()),
            operation.project_id,
            operation.aggregate_type,
            operation.aggregate_id,
            operation.operation_id,
            active,
            expected as i64,
            actual as i64,
            now(),
        ],
    )?;
    Ok(false)
}

pub(super) fn recalculate_clock(
    transaction: &Transaction<'_>,
    project_id: &str,
    camera: &str,
) -> Result<()> {
    let mut events_statement = transaction.prepare("SELECT start_filename, offset_seconds FROM clock_events WHERE project_id=?1 AND camera=?2 ORDER BY start_filename, COALESCE(hlc_wall_millis, 0), COALESCE(hlc_counter, 0), COALESCE(origin_node_id, ''), COALESCE(operation_id, ''), created_at, rowid")?;
    let events = events_statement
        .query_map(params![project_id, camera], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(events_statement);
    let mut assets_statement = transaction.prepare(
        "SELECT id, filename, captured_at_raw FROM assets WHERE project_id=?1 AND camera=?2",
    )?;
    let assets = assets_statement
        .query_map(params![project_id, camera], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(assets_statement);
    for (asset_id, filename, raw) in assets {
        let offset = events
            .iter()
            .filter(|(boundary, _)| boundary <= &filename)
            .map(|(_, offset)| *offset)
            .next_back()
            .unwrap_or(0);
        let best = raw
            .as_deref()
            .and_then(|value| normalize_datetime(value, offset));
        transaction.execute(
            "UPDATE assets SET offset_seconds=?2, best_time=?3 WHERE id=?1",
            params![asset_id, offset, best],
        )?;
    }
    Ok(())
}

/// Routes one newly materialized asset through the solver-owned bounded
/// projection. It loads only that card's epochs and this asset's exception,
/// rather than replaying every project asset after each imported file.
pub(super) fn project_chronology_for_asset(
    transaction: &Transaction<'_>,
    project_id: &str,
    asset_id: &str,
    capture_sequence_id: &str,
    filename_counter: Option<u64>,
    filename: &str,
    captured_at_raw: Option<&str>,
) -> Result<()> {
    Catalog::project_new_asset_chronology_on_connection(
        transaction,
        project_id,
        asset_id,
        capture_sequence_id,
        filename_counter,
        filename,
        captured_at_raw,
    )
}

pub(super) fn refresh_labels(transaction: &Transaction<'_>, asset_id: &str) -> Result<()> {
    let mut statement = transaction.prepare(
        "SELECT normalized_label, display_label FROM label_tags t WHERE asset_id=?1 AND NOT EXISTS(SELECT 1 FROM label_tag_removals r WHERE r.add_tag=t.add_tag AND r.normalized_label=t.normalized_label) ORDER BY hlc_wall_millis, hlc_counter, origin_node_id, add_tag",
    )?;
    let rows = statement
        .query_map([asset_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    let mut seen = BTreeSet::new();
    let labels = rows
        .into_iter()
        .filter_map(|(key, display)| seen.insert(key).then_some(display))
        .collect::<Vec<_>>();
    if labels.len() > 24 {
        bail!("invalid labels: an asset can have at most 24 labels");
    }
    transaction.execute(
        "UPDATE assets SET labels_json=?2 WHERE id=?1",
        params![asset_id, serde_json::to_string(&labels)?],
    )?;
    Ok(())
}

pub(super) fn active_label_tags(
    transaction: &Transaction<'_>,
    asset_id: &str,
    normalized: &str,
) -> Result<Vec<String>> {
    let mut statement = transaction.prepare("SELECT add_tag FROM label_tags t WHERE asset_id=?1 AND normalized_label=?2 AND NOT EXISTS(SELECT 1 FROM label_tag_removals r WHERE r.add_tag=t.add_tag AND r.normalized_label=t.normalized_label) ORDER BY add_tag")?;
    Ok(statement
        .query_map(params![asset_id, normalized], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(crate) fn advance_revision(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO materialized_revisions(project_id, aggregate_type, aggregate_id, revision, last_operation_id) VALUES (?1, ?2, ?3, 1, ?4) ON CONFLICT(project_id, aggregate_type, aggregate_id) DO UPDATE SET revision=revision+1, last_operation_id=excluded.last_operation_id",
        params![operation.project_id, operation.aggregate_type, operation.aggregate_id, operation.operation_id],
    )?;
    Ok(())
}

pub(crate) fn current_revision(
    connection: &rusqlite::Connection,
    project_id: &str,
    aggregate_type: &str,
    aggregate_id: &str,
) -> Result<u64> {
    Ok(connection.query_row("SELECT revision FROM materialized_revisions WHERE project_id=?1 AND aggregate_type=?2 AND aggregate_id=?3",
        params![project_id, aggregate_type, aggregate_id], |row| row.get::<_, i64>(0)).optional()?.unwrap_or(0) as u64)
}

pub(super) fn require_asset(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<()> {
    if operation.aggregate_type != "projectAsset" {
        bail!(
            "{} requires a projectAsset aggregate",
            operation.operation_type
        );
    }
    let belongs: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM assets WHERE id=?1 AND project_id=?2)",
        params![operation.aggregate_id, operation.project_id],
        |row| row.get(0),
    )?;
    if !belongs {
        bail!("asset not found: {}", operation.aggregate_id);
    }
    Ok(())
}

pub(super) fn payload<T: DeserializeOwned>(operation: &ProjectOperation) -> Result<T> {
    if operation.payload_version != 1
        && !(operation.operation_type == EDIT_PLAN_REVISION_CREATED
            && matches!(operation.payload_version, 2..=5))
    {
        bail!(
            "unsupported payload version {} for {}",
            operation.payload_version,
            operation.operation_type
        );
    }
    Ok(serde_json::from_value(operation.payload.clone())?)
}
