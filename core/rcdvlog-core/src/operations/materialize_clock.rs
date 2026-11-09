//! Materializers for clock events, epochs, clock questions, clip time exceptions, trip phases, and event order.

use super::*;

pub(super) fn materialize_clock(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: ClockPayload = payload(operation)?;
    if operation.aggregate_type != "clock" || operation.aggregate_id != payload.camera {
        bail!("clock operation aggregate does not match its camera");
    }
    let actual = current_revision(
        transaction,
        &operation.project_id,
        "clock",
        &operation.aggregate_id,
    )?;
    if operation.base_revision != Some(actual) {
        let active = transaction.query_row(
            "SELECT last_operation_id FROM materialized_revisions WHERE project_id=?1 AND aggregate_type='clock' AND aggregate_id=?2",
            params![operation.project_id, operation.aggregate_id], |row| row.get::<_, Option<String>>(0)).optional()?.flatten();
        transaction.execute("INSERT INTO operation_conflicts(conflict_id, project_id, aggregate_type, aggregate_id, incoming_operation_id, active_operation_id, expected_base_revision, actual_revision, conflict_type, created_at) VALUES (?1, ?2, 'clock', ?3, ?4, ?5, ?6, ?7, 'stale_clock_base_revision', ?8)",
            params![format!("conflict_{}", Uuid::new_v4().simple()), operation.project_id, operation.aggregate_id, operation.operation_id,
                active, operation.base_revision.map(|v| v as i64), actual as i64, now()])?;
        return Ok(ApplyOperationResult::Conflict);
    }
    // The immutable operation and its revision remain on the raw camera
    // aggregate named by the writer. Only materialized camera projections are
    // canonicalized. This makes revision checks deterministic while a late
    // pre-merge operation still follows the durable identity equivalence.
    let camera = resolve_camera_alias(transaction, &operation.project_id, &payload.camera)?;
    let created_at = now();
    transaction.execute("INSERT INTO clock_events(id, project_id, camera, start_filename, offset_seconds, label, created_at, operation_id, hlc_wall_millis, hlc_counter, origin_node_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![payload.event_id, operation.project_id, camera, payload.start_filename, payload.offset_seconds, payload.label, created_at, operation.operation_id,
            operation.logical_time.wall_millis, operation.logical_time.counter as i64, operation.origin_node_id])?;
    crate::chronology::materialize_legacy_clock_epoch(
        transaction,
        &operation.project_id,
        &crate::chronology::LegacyClockEpoch {
            event_id: &payload.event_id,
            camera: &camera,
            start_filename: &payload.start_filename,
            offset_seconds: payload.offset_seconds,
            label: &payload.label,
            created_at: &created_at,
        },
    )?;
    recalculate_clock(transaction, &operation.project_id, &camera)?;
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

pub(super) fn materialize_clock_epoch_set(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: ClockEpochSetPayload = payload(operation)?;
    if operation.aggregate_type != "clockEpoch" || operation.aggregate_id != payload.epoch_id {
        bail!("clockEpochSet aggregate must be the clock epoch");
    }
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    if payload
        .end_counter
        .is_some_and(|end| end < payload.start_counter)
    {
        bail!("clockEpochSet end counter is before its start counter");
    }
    let sequence_exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM capture_sequences WHERE id=?1 AND project_id=?2)",
        params![payload.capture_sequence_id, operation.project_id],
        |row| row.get(0),
    )?;
    if !sequence_exists {
        bail!(
            "clockEpochSet capture sequence not found: {}",
            payload.capture_sequence_id
        );
    }
    let confidence = payload.confidence.unwrap_or(1.0);
    let evidence = payload.evidence.unwrap_or_default();
    transaction.execute(
        "INSERT INTO clock_epochs(id, project_id, capture_sequence_id, start_counter, end_counter, offset_seconds, confidence, evidence_json, label, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) ON CONFLICT(id) DO UPDATE SET capture_sequence_id=excluded.capture_sequence_id, start_counter=excluded.start_counter, end_counter=excluded.end_counter, offset_seconds=excluded.offset_seconds, confidence=excluded.confidence, evidence_json=excluded.evidence_json, label=excluded.label",
        params![
            payload.epoch_id,
            operation.project_id,
            payload.capture_sequence_id,
            payload.start_counter as i64,
            payload.end_counter.map(|value| value as i64),
            payload.offset_seconds,
            confidence,
            serde_json::to_string(&evidence)?,
            payload.label,
            now(),
        ],
    )?;
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

pub(super) fn materialize_clock_epoch_cleared(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: ClockEpochClearedPayload = payload(operation)?;
    if operation.aggregate_type != "clockEpoch" || operation.aggregate_id != payload.epoch_id {
        bail!("clockEpochCleared aggregate must be the clock epoch");
    }
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    transaction.execute(
        "DELETE FROM clock_epochs WHERE id=?1 AND project_id=?2",
        params![payload.epoch_id, operation.project_id],
    )?;
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

pub(super) fn materialize_clock_question_proposed(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let question: crate::chronology::ClockBlockQuestion = payload(operation)?;
    if operation.aggregate_type != "clockQuestion" || operation.aggregate_id != question.id {
        bail!("clock question proposal aggregate must be the question id");
    }
    question
        .validate()
        .map_err(|error| anyhow!("invalid clock question: {error}"))?;
    let facts = crate::chronology::load_chronology_facts(transaction, &operation.project_id)?;
    crate::chronology::api::validate_question_scope(&question, &facts)
        .map_err(|error| anyhow!("invalid clock question scope: {error}"))?;
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

pub(super) fn materialize_clock_question_decision(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let decision: ClockQuestionDecisionPayload = payload(operation)?;
    if operation.aggregate_type != "clockQuestion"
        || operation.aggregate_id != decision.question_id
        || decision.question_id.trim().is_empty()
        || decision.accepted != decision.alternative_id.is_some()
        || decision.note.chars().count() > 2_000
    {
        bail!("clock question decision has invalid aggregate or fields");
    }
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    let current: Option<(String, String)> = transaction
        .query_row(
            "SELECT o.operation_type, o.payload_json FROM materialized_revisions r \
             JOIN project_operations o ON o.operation_id=r.last_operation_id \
             WHERE r.project_id=?1 AND r.aggregate_type='clockQuestion' AND r.aggregate_id=?2",
            params![operation.project_id, decision.question_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((kind, proposal_json)) = current else {
        bail!("clock question decision requires a current proposal");
    };
    if kind != CLOCK_QUESTION_PROPOSED {
        bail!("clock question is already answered; revise it before answering again");
    }
    let question: crate::chronology::ClockBlockQuestion = serde_json::from_str(&proposal_json)?;
    if decision.accepted {
        let alternative = question
            .alternatives
            .iter()
            .find(|alternative| Some(alternative.id.as_str()) == decision.alternative_id.as_deref())
            .ok_or_else(|| anyhow!("accepted clock question alternative is not in the proposal"))?;
        let epoch: Option<(i64, Option<i64>, i64)> = transaction
            .query_row(
                "SELECT start_counter, end_counter, offset_seconds FROM clock_epochs \
                 WHERE project_id=?1 AND id=?2 AND capture_sequence_id=?3",
                params![
                    operation.project_id,
                    alternative.epoch.id,
                    alternative.epoch.capture_sequence_id
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if epoch
            != Some((
                alternative.epoch.start_counter as i64,
                alternative.epoch.end_counter.map(|value| value as i64),
                alternative.epoch.offset_seconds,
            ))
        {
            bail!("accepted clock question requires its selected clock epoch operation first");
        }
    } else {
        if decision.alternative_id.is_some() {
            bail!("rejected clock question cannot select an alternative");
        }
    }
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

pub(super) fn materialize_clip_time_exception_set(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: ClipTimeExceptionSetPayload = payload(operation)?;
    require_asset(transaction, operation)?;
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    transaction.execute(
        "INSERT INTO clip_time_exceptions(asset_id, project_id, trip_time, reason, created_at) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(asset_id) DO UPDATE SET trip_time=excluded.trip_time, reason=excluded.reason",
        params![
            operation.aggregate_id,
            operation.project_id,
            payload.trip_time,
            payload.reason,
            now(),
        ],
    )?;
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

pub(super) fn materialize_clip_time_exception_cleared(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let _payload: ClipTimeExceptionClearedPayload = payload(operation)?;
    require_asset(transaction, operation)?;
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    transaction.execute(
        "DELETE FROM clip_time_exceptions WHERE asset_id=?1 AND project_id=?2",
        params![operation.aggregate_id, operation.project_id],
    )?;
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

pub(super) fn materialize_trip_phase_set(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: TripPhaseSetPayload = payload(operation)?;
    if operation.aggregate_type != "tripPhase" || operation.aggregate_id != payload.phase_id {
        bail!("tripPhaseSet aggregate must be the trip phase");
    }
    if !matches!(payload.phase_kind.as_str(), "location" | "transit") {
        bail!("invalid trip phase kind: {}", payload.phase_kind);
    }
    if !matches!(payload.display_mode.as_str(), "local" | "dual" | "utc") {
        bail!("invalid display mode: {}", payload.display_mode);
    }
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    transaction.execute(
        "INSERT INTO trip_phases(id, project_id, label, phase_kind, start_anchor_asset_id, end_anchor_asset_id, start_trip_time, end_trip_time, timezone, origin_timezone, destination_timezone, display_mode, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) ON CONFLICT(id) DO UPDATE SET label=excluded.label, phase_kind=excluded.phase_kind, start_anchor_asset_id=excluded.start_anchor_asset_id, end_anchor_asset_id=excluded.end_anchor_asset_id, start_trip_time=excluded.start_trip_time, end_trip_time=excluded.end_trip_time, timezone=excluded.timezone, origin_timezone=excluded.origin_timezone, destination_timezone=excluded.destination_timezone, display_mode=excluded.display_mode",
        params![
            payload.phase_id,
            operation.project_id,
            payload.label,
            payload.phase_kind,
            payload.start_anchor_asset_id,
            payload.end_anchor_asset_id,
            payload.start_trip_time,
            payload.end_trip_time,
            payload.timezone,
            payload.origin_timezone,
            payload.destination_timezone,
            payload.display_mode,
            now(),
        ],
    )?;
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

pub(super) fn materialize_trip_phase_removed(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: TripPhaseRemovedPayload = payload(operation)?;
    if operation.aggregate_type != "tripPhase" || operation.aggregate_id != payload.phase_id {
        bail!("tripPhaseRemoved aggregate must be the trip phase");
    }
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    transaction.execute(
        "DELETE FROM trip_phases WHERE id=?1 AND project_id=?2",
        params![payload.phase_id, operation.project_id],
    )?;
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

pub(super) fn materialize_event_order_constraint_set(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: EventOrderConstraintSetPayload = payload(operation)?;
    if operation.aggregate_type != "eventOrderConstraint"
        || operation.aggregate_id != payload.constraint_id
    {
        bail!("eventOrderConstraintSet aggregate must be the event-order constraint");
    }
    if !matches!(payload.strength.as_str(), "hard" | "soft") {
        bail!("invalid constraint strength: {}", payload.strength);
    }
    if payload.earlier_asset_id == payload.later_asset_id {
        bail!("eventOrderConstraintSet requires two distinct assets");
    }
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    for asset_id in [&payload.earlier_asset_id, &payload.later_asset_id] {
        let belongs: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM assets WHERE id=?1 AND project_id=?2)",
            params![asset_id, operation.project_id],
            |row| row.get(0),
        )?;
        if !belongs {
            bail!("eventOrderConstraintSet asset not found: {asset_id}");
        }
    }
    // One constraint per ordered pair, regardless of constraint id.
    transaction.execute(
        "DELETE FROM event_order_constraints WHERE project_id=?1 AND earlier_asset_id=?2 AND later_asset_id=?3 AND id != ?4",
        params![
            operation.project_id,
            payload.earlier_asset_id,
            payload.later_asset_id,
            payload.constraint_id,
        ],
    )?;
    transaction.execute(
        "INSERT INTO event_order_constraints(id, project_id, earlier_asset_id, later_asset_id, strength, source, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(id) DO UPDATE SET earlier_asset_id=excluded.earlier_asset_id, later_asset_id=excluded.later_asset_id, strength=excluded.strength, source=excluded.source",
        params![
            payload.constraint_id,
            operation.project_id,
            payload.earlier_asset_id,
            payload.later_asset_id,
            payload.strength,
            payload.source,
            now(),
        ],
    )?;
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

pub(super) fn materialize_event_order_constraint_cleared(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: EventOrderConstraintClearedPayload = payload(operation)?;
    if operation.aggregate_type != "eventOrderConstraint"
        || operation.aggregate_id != payload.constraint_id
    {
        bail!("eventOrderConstraintCleared aggregate must be the event-order constraint");
    }
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    transaction.execute(
        "DELETE FROM event_order_constraints WHERE id=?1 AND project_id=?2",
        params![payload.constraint_id, operation.project_id],
    )?;
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}
