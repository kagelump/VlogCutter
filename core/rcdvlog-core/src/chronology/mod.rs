//! Persisted chronology evidence: fact types, the deterministic loader, and
//! the shared helpers used by migration 9 and the operation materializers.
//!
//! Everything in this module is *evidence only*. Derived values (trip time,
//! confidence, total order) belong to the solver, not here. All collections
//! leave the loader sorted by primary key so no downstream consumer can
//! inherit nondeterminism.

pub mod model;

pub use model::{
    AssetTimeHypothesis, CaptureSequence, ChronologyFacts, ClipTimeException, ClockEpoch,
    ConstraintStrength, DisplayMode, EventOrderConstraint, PhaseKind, TripPhase,
};

use crate::operations::{
    ApplyOperationResult, CHRONOLOGY_OPERATION_TYPES, KNOWN_OPERATION_TYPES, materialize_operation,
    operation_from_row,
};
use anyhow::Result;
use rusqlite::{Connection, Transaction, TransactionBehavior, params, params_from_iter};
use serde::{Deserialize, Serialize};

/// An operation this build retained but could not materialize, because its
/// operation type is unknown to this build. It is durable in
/// `project_operations` and applies normally once a build that knows the type
/// opens the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantinedOperation {
    pub operation_id: String,
    pub operation_type: String,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub payload_version: u32,
    pub received_at: String,
}

/// Deterministic capture-sequence id for one project camera. Both migration 9
/// and the operation materializers derive sequence ids this way, so a legacy
/// `clockEventCreated` replay produces exactly the epoch rows the migration
/// produces for an equivalent legacy row. The digest keeps camera strings of
/// any shape out of the id, which must stay whitespace-free for operation
/// aggregate ids.
pub(crate) fn capture_sequence_id_for(project_id: &str, camera: &str) -> String {
    format!(
        "sequence_{}_{}",
        project_id,
        blake3::hash(camera.as_bytes()).to_hex()
    )
}

/// Parses the trailing digit run of a filename's stem into a counter, the
/// migration-9 rule. A filename with no digit run, or a digit run that exceeds
/// `u64`, yields `None`.
///
/// - `C0101.MP4` → 101
/// - `DJI_0042.MP4` → 42
/// - `clip.mp4` → None
/// - `GX010234.MP4` → 10234
/// - `IMG_1234 (1).MOV` → 1
pub(crate) fn parse_filename_counter(filename: &str) -> Option<u64> {
    let stem = match filename.rfind('.') {
        Some(extension) => &filename[..extension],
        None => filename,
    };
    let trimmed = stem.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
    let run_start = trimmed
        .rfind(|c: char| !c.is_ascii_digit())
        .map_or(0, |index| index + 1);
    let run = &trimmed[run_start..];
    if run.is_empty() {
        return None;
    }
    run.parse::<u64>().ok()
}

/// Ensures a `capture_sequences` row exists for one project camera, creating
/// it with the migration-9 shape (camera key and label are the camera string)
/// when it does not. Deterministic and idempotent, so replay across replicas
/// converges.
pub(crate) fn ensure_capture_sequence_for_camera(
    transaction: &Transaction<'_>,
    project_id: &str,
    camera: &str,
    created_at: &str,
) -> Result<String> {
    let sequence_id = capture_sequence_id_for(project_id, camera);
    transaction.execute(
        "INSERT OR IGNORE INTO capture_sequences(id, project_id, camera_key, label, volume_hint, filename_prefix, created_at) VALUES (?1, ?2, ?3, ?3, '', '', ?4)",
        params![sequence_id, project_id, camera, created_at],
    )?;
    Ok(sequence_id)
}

/// The legacy `clockEventCreated` path into `clock_epochs` — the same path
/// migration 9 uses for existing `clock_events` rows. The epoch keeps the
/// legacy event id, carries the offset and label over, and records the
/// migration evidence. `start_counter` is parsed from `start_filename`, with
/// 0 when the filename has no parseable counter (today's behavior of applying
/// the offset to the whole camera).
pub(crate) struct LegacyClockEpoch<'a> {
    pub event_id: &'a str,
    pub camera: &'a str,
    pub start_filename: &'a str,
    pub offset_seconds: i64,
    pub label: &'a str,
    pub created_at: &'a str,
}

pub(crate) fn materialize_legacy_clock_epoch(
    transaction: &Transaction<'_>,
    project_id: &str,
    epoch: &LegacyClockEpoch<'_>,
) -> Result<()> {
    let sequence_id = ensure_capture_sequence_for_camera(
        transaction,
        project_id,
        epoch.camera,
        epoch.created_at,
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO clock_epochs(id, project_id, capture_sequence_id, start_counter, end_counter, offset_seconds, confidence, evidence_json, label, created_at) VALUES (?1, ?2, ?3, ?4, NULL, ?5, 1.0, ?6, ?7, ?8)",
        params![
            epoch.event_id,
            project_id,
            sequence_id,
            parse_filename_counter(epoch.start_filename).unwrap_or(0) as i64,
            epoch.offset_seconds,
            r#"["migrated_clock_event"]"#,
            epoch.label,
            epoch.created_at,
        ],
    )?;
    Ok(())
}

/// Loads the complete persisted chronology evidence for one project. Every
/// collection is sorted by primary key:
///
/// - assets by `assets.id`
/// - sequences by `capture_sequences.id`
/// - epochs by `clock_epochs.id`
/// - exceptions by `clip_time_exceptions.asset_id`
/// - phases by `trip_phases.id`
/// - constraints by `event_order_constraints.id`
pub fn load_chronology_facts(connection: &Connection, project_id: &str) -> Result<ChronologyFacts> {
    if !table_exists(connection, "capture_sequences")? {
        return Ok(ChronologyFacts {
            project_id: project_id.to_string(),
            assets: Vec::new(),
            sequences: Vec::new(),
            epochs: Vec::new(),
            exceptions: Vec::new(),
            phases: Vec::new(),
            constraints: Vec::new(),
        });
    }
    let mut assets_statement = connection.prepare(
        r#"
        SELECT a.id, a.filename, a.captured_at_raw, a.duration_seconds,
               acs.capture_sequence_id, acs.filename_counter
        FROM assets a
        LEFT JOIN asset_capture_sequence acs ON acs.asset_id = a.id
        WHERE a.project_id = ?1
        ORDER BY a.id
        "#,
    )?;
    let assets = assets_statement
        .query_map([project_id], |row| {
            Ok(AssetTimeHypothesis {
                asset_id: row.get(0)?,
                filename: row.get(1)?,
                captured_at_raw: row.get(2)?,
                duration_seconds: row.get(3)?,
                capture_sequence_id: row.get(4)?,
                filename_counter: row.get::<_, Option<i64>>(5)?.map(|value| value as u64),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut sequences_statement = connection.prepare(
        r#"
        SELECT id, camera_key, label, volume_hint, filename_prefix
        FROM capture_sequences
        WHERE project_id = ?1
        ORDER BY id
        "#,
    )?;
    let sequences = sequences_statement
        .query_map([project_id], |row| {
            Ok(CaptureSequence {
                id: row.get(0)?,
                camera_key: row.get(1)?,
                label: row.get(2)?,
                volume_hint: row.get(3)?,
                filename_prefix: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut epochs_statement = connection.prepare(
        r#"
        SELECT id, capture_sequence_id, start_counter, end_counter,
               offset_seconds, confidence, evidence_json, label
        FROM clock_epochs
        WHERE project_id = ?1
        ORDER BY id
        "#,
    )?;
    let epochs = epochs_statement
        .query_map([project_id], |row| {
            let evidence_json: String = row.get(6)?;
            Ok(ClockEpoch {
                id: row.get(0)?,
                capture_sequence_id: row.get(1)?,
                start_counter: row.get::<_, i64>(2)? as u64,
                end_counter: row.get::<_, Option<i64>>(3)?.map(|value| value as u64),
                offset_seconds: row.get(4)?,
                confidence: row.get(5)?,
                evidence: serde_json::from_str(&evidence_json).unwrap_or_default(),
                label: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut exceptions_statement = connection.prepare(
        r#"
        SELECT asset_id, trip_time, reason
        FROM clip_time_exceptions
        WHERE project_id = ?1
        ORDER BY asset_id
        "#,
    )?;
    let exceptions = exceptions_statement
        .query_map([project_id], |row| {
            Ok(ClipTimeException {
                asset_id: row.get(0)?,
                trip_time: row.get(1)?,
                reason: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut phases_statement = connection.prepare(
        r#"
        SELECT id, label, phase_kind, start_trip_time, end_trip_time,
               timezone, origin_timezone, destination_timezone, display_mode
        FROM trip_phases
        WHERE project_id = ?1
        ORDER BY id
        "#,
    )?;
    let phases = phases_statement
        .query_map([project_id], |row| {
            let phase_kind = match row.get::<_, String>(2)?.as_str() {
                "location" => PhaseKind::Location,
                "transit" => PhaseKind::Transit,
                value => {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        format!("invalid phase kind: {value}").into(),
                    ));
                }
            };
            let display_mode = match row.get::<_, String>(8)?.as_str() {
                "local" => DisplayMode::Local,
                "dual" => DisplayMode::Dual,
                "utc" => DisplayMode::Utc,
                value => {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        8,
                        rusqlite::types::Type::Text,
                        format!("invalid display mode: {value}").into(),
                    ));
                }
            };
            Ok(TripPhase {
                id: row.get(0)?,
                label: row.get(1)?,
                kind: phase_kind,
                start_trip_time: row.get(3)?,
                end_trip_time: row.get(4)?,
                timezone: row.get(5)?,
                origin_timezone: row.get(6)?,
                destination_timezone: row.get(7)?,
                display_mode,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut constraints_statement = connection.prepare(
        r#"
        SELECT id, earlier_asset_id, later_asset_id, strength, source
        FROM event_order_constraints
        WHERE project_id = ?1
        ORDER BY id
        "#,
    )?;
    let constraints = constraints_statement
        .query_map([project_id], |row| {
            let strength = match row.get::<_, String>(3)?.as_str() {
                "hard" => ConstraintStrength::Hard,
                "soft" => ConstraintStrength::Soft,
                value => {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        format!("invalid constraint strength: {value}").into(),
                    ));
                }
            };
            Ok(EventOrderConstraint {
                id: row.get(0)?,
                earlier_asset_id: row.get(1)?,
                later_asset_id: row.get(2)?,
                strength,
                source: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(ChronologyFacts {
        project_id: project_id.to_string(),
        assets,
        sequences,
        epochs,
        exceptions,
        phases,
        constraints,
    })
}

/// Loads the operations this build retained but could not materialize: rows in
/// `project_operations` whose operation type this build does not recognize.
/// They are durable in the operation log and apply normally once a build that
/// knows the type opens the catalog.
pub fn load_quarantined_operations(
    connection: &Connection,
    project_id: &str,
) -> Result<Vec<QuarantinedOperation>> {
    if !table_exists(connection, "project_operations")? {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat_n("?", KNOWN_OPERATION_TYPES.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        r#"
        SELECT operation_id, operation_type, aggregate_type, aggregate_id,
               payload_version, created_at
        FROM project_operations
        WHERE project_id = ?1
          AND operation_type NOT IN ({placeholders})
        ORDER BY operation_id
        "#
    );
    let mut statement = connection.prepare(&sql)?;
    let mut rows = statement.query(params_from_iter(
        std::iter::once(project_id).chain(KNOWN_OPERATION_TYPES.iter().copied()),
    ))?;
    let mut operations = Vec::new();
    while let Some(row) = rows.next()? {
        operations.push(QuarantinedOperation {
            operation_id: row.get(0)?,
            operation_type: row.get(1)?,
            aggregate_type: row.get(2)?,
            aggregate_id: row.get(3)?,
            payload_version: row.get::<_, i64>(4)? as u32,
            received_at: row.get(5)?,
        });
    }
    Ok(operations)
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )?)
}

/// Re-dispatches persisted chronology operations that an older build
/// quarantined (D01-T007 decision 4, "applies after upgrade"). Runs on
/// writable catalog open, after migration, and never on a read-only open.
///
/// A row qualifies only when all of these hold:
///
/// - its operation type is one of the eleven chronology types this task added
///   (legacy types are never re-dispatched: a persisted legacy row is not a
///   signal that materialization is missing, so replaying them would rewrite
///   the legacy log);
/// - `payload_version == 1` (known types keep rejecting other versions);
/// - no `materialized_revisions` row exists for the operation's aggregate —
///   for the new vocabulary, insert and materialize are atomic, so
///   row-persisted-without-a-revision-row is exactly never-applied;
/// - no `operation_conflicts` row names the operation — a previously rejected
///   operation must not re-apply.
///
/// Rows are processed in `operation_id` order inside one transaction. Each row
/// is best-effort: a row that errors (bail or Conflict) is skipped and left in
/// the log — never aborting the open, never deleted, and a Conflict outcome
/// records its own durable conflict row, which is the correct outcome for a
/// stale-base row. The seam is idempotent: every applied row leaves a
/// `materialized_revisions` row (or a conflict row) behind, so a second open
/// applies nothing new.
pub(crate) fn redispatch_chronology_operations(connection: &mut Connection) -> Result<()> {
    if !table_exists(connection, "project_operations")? {
        return Ok(());
    }
    let placeholders = std::iter::repeat_n("?", CHRONOLOGY_OPERATION_TYPES.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        r#"
        SELECT operation_id, project_id, origin_node_id, origin_sequence,
               hlc_wall_millis, hlc_counter, aggregate_type, aggregate_id,
               operation_type, payload_version, base_revision, payload_json
        FROM project_operations
        WHERE operation_type IN ({placeholders})
          AND payload_version = 1
          AND NOT EXISTS (
              SELECT 1 FROM materialized_revisions mr
              WHERE mr.project_id = project_operations.project_id
                AND mr.aggregate_type = project_operations.aggregate_type
                AND mr.aggregate_id = project_operations.aggregate_id
          )
          AND NOT EXISTS (
              SELECT 1 FROM operation_conflicts oc
              WHERE oc.incoming_operation_id = project_operations.operation_id
          )
        ORDER BY operation_id
        "#
    );
    let mut statement = connection.prepare(&sql)?;
    let candidates = statement
        .query_map(
            params_from_iter(CHRONOLOGY_OPERATION_TYPES.iter().copied()),
            operation_from_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    if candidates.is_empty() {
        return Ok(());
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for operation in candidates {
        match materialize_operation(&transaction, &operation) {
            Ok(ApplyOperationResult::Applied) => {}
            Ok(_) => {}
            Err(_) => {
                // Best effort: skip the row and leave it in the log. The open
                // must never fail because one quarantined row cannot apply.
            }
        }
    }
    transaction.commit()?;
    Ok(())
}

/// Runs the migration-9 forward migration for one project: one capture
/// sequence per distinct camera (from assets and from clock events), an
/// assignment row for every asset, and one epoch per legacy clock event.
/// `assets.best_time` and `assets.offset_seconds` are never touched.
pub fn forward_migrate_project_chronology(
    transaction: &Transaction<'_>,
    project_id: &str,
) -> Result<()> {
    let mut cameras = std::collections::BTreeSet::new();
    let mut statement = transaction
        .prepare("SELECT DISTINCT camera FROM assets WHERE project_id = ?1 ORDER BY camera")?;
    let asset_cameras = statement
        .query_map([project_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for camera in asset_cameras {
        cameras.insert(camera);
    }
    let mut statement = transaction.prepare(
        "SELECT DISTINCT camera FROM clock_events WHERE project_id = ?1 ORDER BY camera",
    )?;
    let event_cameras = statement
        .query_map([project_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for camera in event_cameras {
        cameras.insert(camera);
    }

    for camera in &cameras {
        let sequence_id = capture_sequence_id_for(project_id, camera);
        transaction.execute(
            "INSERT OR IGNORE INTO capture_sequences(id, project_id, camera_key, label, volume_hint, filename_prefix, created_at) VALUES (?1, ?2, ?3, ?3, '', '', ?4)",
            params![sequence_id, project_id, camera, now()],
        )?;
    }

    let mut assets = transaction
        .prepare("SELECT id, camera, filename FROM assets WHERE project_id = ?1 ORDER BY id")?;
    let asset_rows = assets
        .query_map([project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(assets);
    for (asset_id, camera, filename) in asset_rows {
        let sequence_id = capture_sequence_id_for(project_id, &camera);
        transaction.execute(
            "INSERT OR IGNORE INTO asset_capture_sequence(asset_id, project_id, capture_sequence_id, filename_counter) VALUES (?1, ?2, ?3, ?4)",
            params![
                asset_id,
                project_id,
                sequence_id,
                parse_filename_counter(&filename).map(|value| value as i64),
            ],
        )?;
    }

    let mut events = transaction.prepare(
        "SELECT id, camera, start_filename, offset_seconds, label, created_at FROM clock_events WHERE project_id = ?1 ORDER BY id",
    )?;
    let event_rows = events
        .query_map([project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(events);
    for (event_id, camera, start_filename, offset_seconds, label, created_at) in event_rows {
        let sequence_id = capture_sequence_id_for(project_id, &camera);
        transaction.execute(
            "INSERT OR IGNORE INTO clock_epochs(id, project_id, capture_sequence_id, start_counter, end_counter, offset_seconds, confidence, evidence_json, label, created_at) VALUES (?1, ?2, ?3, ?4, NULL, ?5, 1.0, ?6, ?7, ?8)",
            params![
                event_id,
                project_id,
                sequence_id,
                parse_filename_counter(&start_filename).unwrap_or(0) as i64,
                offset_seconds,
                r#"["migrated_clock_event"]"#,
                label,
                created_at,
            ],
        )?;
    }
    Ok(())
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
