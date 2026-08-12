use crate::{Catalog, ClockEvent, ConflictProjection, MediaVerificationState, ProjectOperation};
use anyhow::{Result, anyhow, bail};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableAssetBaseline {
    pub id: String,
    pub fingerprint: String,
    pub filename: String,
    pub size_bytes: u64,
    pub duration_seconds: f64,
    pub frame_rate: f64,
    pub width: u32,
    pub height: u32,
    pub codec: String,
    pub camera: String,
    pub captured_at_raw: Option<String>,
    pub best_time: Option<String>,
    pub offset_seconds: i64,
    pub metadata_rotation_degrees: u16,
    pub default_rotation_degrees: u16,
    pub orientation_probed: bool,
    pub baseline_notes: String,
    pub baseline_labels: Vec<String>,
    pub baseline_removed: bool,
    pub baseline_rotation_degrees: u16,
    pub media_object_id: Option<String>,
    pub media_verification_state: MediaVerificationState,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableRootMapping {
    pub storage_root_id: String,
    pub relative_prefix: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectBaselineSnapshot {
    pub project_id: String,
    pub name: String,
    pub created_at: String,
    pub origin_node_id: Option<String>,
    pub paired_server_node_id: Option<String>,
    pub paired_at: Option<String>,
    pub assets: Vec<PortableAssetBaseline>,
    pub root_mappings: Vec<PortableRootMapping>,
    pub baseline_clock_events: Vec<ClockEvent>,
    pub operations: Vec<ProjectOperation>,
    pub operation_cursors: BTreeMap<String, u64>,
    pub materialized_revision_count: u64,
    pub conflicts: Vec<ConflictProjection>,
}

impl Catalog {
    pub fn pair_project_with_server(&self, project_id: &str, server_node_id: &str) -> Result<()> {
        if server_node_id.trim().is_empty() {
            bail!("server node ID cannot be empty");
        }
        self.require_project(project_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let existing = transaction.query_row(
            "SELECT paired_server_node_id FROM projects WHERE id=?1",
            [project_id],
            |row| row.get::<_, Option<String>>(0),
        )?;
        if existing
            .as_deref()
            .is_some_and(|existing| existing != server_node_id)
        {
            bail!("project is already paired with another server");
        }
        transaction.execute(
            "UPDATE projects SET paired_server_node_id=?2, paired_at=COALESCE(paired_at, ?3) WHERE id=?1",
            params![project_id, server_node_id, crate::now()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Produces a path-free, selected-project-only baseline. Revision-zero state
    /// is separated from operations so importing and replaying preserves CRDT semantics.
    pub fn export_project_baseline(&self, project_id: &str) -> Result<ProjectBaselineSnapshot> {
        self.require_project(project_id)?;
        let connection = self.connection()?;
        let (name, created_at, origin_node_id, paired_server_node_id, paired_at) = connection.query_row(
            "SELECT name, created_at, origin_node_id, paired_server_node_id, paired_at FROM projects WHERE id=?1",
            [project_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )?;
        let mut statement = connection.prepare(
            "SELECT id, fingerprint, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, offset_seconds, metadata_rotation_degrees, default_rotation_degrees, orientation_probed, notes, labels_json, removed, rotation_degrees, media_object_id, media_verification_state, created_at FROM assets WHERE project_id=?1 AND COALESCE(membership_removed, 0) = 0 ORDER BY id",
        )?;
        let rows = statement
            .query_map([project_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, f64>(4)?,
                    row.get::<_, f64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, bool>(15)?,
                    row.get::<_, String>(16)?,
                    row.get::<_, String>(17)?,
                    row.get::<_, bool>(18)?,
                    row.get::<_, i64>(19)?,
                    row.get::<_, Option<String>>(20)?,
                    row.get::<_, String>(21)?,
                    row.get::<_, String>(22)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let mut assets = Vec::with_capacity(rows.len());
        for row in rows {
            let baseline_notes = connection.query_row("SELECT text FROM note_revisions WHERE asset_id=?1 AND operation_id IS NULL ORDER BY hlc_counter DESC, revision_id DESC LIMIT 1", [&row.0], |value| value.get(0)).optional()?.unwrap_or(row.16);
            let mut labels_statement = connection.prepare("SELECT display_label FROM label_tags WHERE asset_id=?1 AND operation_id IS NULL ORDER BY hlc_counter, add_tag")?;
            let baseline_labels = labels_statement
                .query_map([&row.0], |value| value.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            let baseline_removed = baseline_register(&connection, project_id, &row.0, "removed")?
                .and_then(|value| serde_json::from_str(&value).ok())
                .unwrap_or(row.18);
            let baseline_rotation_degrees =
                baseline_register(&connection, project_id, &row.0, "rotationDegrees")?
                    .and_then(|value| serde_json::from_str::<u16>(&value).ok())
                    .unwrap_or(row.19 as u16);
            let media_verification_state =
                serde_json::from_value(serde_json::Value::String(row.21))
                    .map_err(|_| anyhow!("invalid stored media verification state"))?;
            assets.push(PortableAssetBaseline {
                id: row.0,
                fingerprint: row.1,
                filename: row.2,
                size_bytes: row.3 as u64,
                duration_seconds: row.4,
                frame_rate: row.5,
                width: row.6 as u32,
                height: row.7 as u32,
                codec: row.8,
                camera: row.9,
                captured_at_raw: row.10,
                best_time: row.11,
                offset_seconds: row.12,
                metadata_rotation_degrees: row.13 as u16,
                default_rotation_degrees: row.14 as u16,
                orientation_probed: row.15,
                baseline_notes,
                baseline_labels,
                baseline_removed,
                baseline_rotation_degrees,
                media_object_id: row.20,
                media_verification_state,
                created_at: row.22,
            });
        }
        let mut clock_statement = connection.prepare("SELECT id, project_id, camera, start_filename, offset_seconds, label, created_at FROM clock_events WHERE project_id=?1 AND operation_id IS NULL ORDER BY camera, start_filename, created_at, id")?;
        let baseline_clock_events = clock_statement
            .query_map([project_id], |row| {
                Ok(ClockEvent {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    camera: row.get(2)?,
                    start_filename: row.get(3)?,
                    offset_seconds: row.get(4)?,
                    label: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut root_statement = connection.prepare("SELECT storage_root_id, relative_prefix, created_at FROM project_storage_root_mappings WHERE project_id=?1 ORDER BY storage_root_id, relative_prefix")?;
        let root_mappings = root_statement
            .query_map([project_id], |row| {
                Ok(PortableRootMapping {
                    storage_root_id: row.get(0)?,
                    relative_prefix: row.get(1)?,
                    created_at: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let operations = self.list_operations(project_id)?;
        let mut operation_cursors = BTreeMap::new();
        for operation in &operations {
            operation_cursors
                .entry(operation.origin_node_id.clone())
                .and_modify(|sequence: &mut u64| {
                    *sequence = (*sequence).max(operation.origin_sequence)
                })
                .or_insert(operation.origin_sequence);
        }
        let materialized_revision_count = connection.query_row(
            "SELECT COUNT(*) FROM materialized_revisions WHERE project_id=?1",
            [project_id],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let conflicts = self.operation_diagnostics(project_id)?.unresolved_conflicts;
        Ok(ProjectBaselineSnapshot {
            project_id: project_id.to_string(),
            name,
            created_at,
            origin_node_id,
            paired_server_node_id,
            paired_at,
            assets,
            root_mappings,
            baseline_clock_events,
            operations,
            operation_cursors,
            materialized_revision_count,
            conflicts,
        })
    }

    pub fn import_project_baseline(
        database_path: impl AsRef<Path>,
        snapshot: &ProjectBaselineSnapshot,
        local_node_id: &str,
        baseline_version: u32,
        baseline_digest: &str,
    ) -> Result<Catalog> {
        if snapshot.project_id.is_empty() || snapshot.assets.iter().any(|asset| asset.id.is_empty())
        {
            bail!("baseline contains an empty project or asset ID");
        }
        let catalog = Catalog::open(database_path)?;
        catalog.set_local_node_id(local_node_id)?;
        let mut connection = catalog.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute("INSERT INTO projects(id, name, created_at, origin_node_id, paired_server_node_id, paired_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![snapshot.project_id, snapshot.name, snapshot.created_at, snapshot.origin_node_id, snapshot.paired_server_node_id, snapshot.paired_at])?;
        for asset in &snapshot.assets {
            transaction.execute("INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, offset_seconds, metadata_rotation_degrees, default_rotation_degrees, rotation_degrees, orientation_probed, notes, labels_json, removed, media_object_id, media_verification_state, created_at) VALUES (?1, ?2, ?3, '', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)", params![asset.id, snapshot.project_id, asset.fingerprint, asset.filename, asset.size_bytes as i64, asset.duration_seconds, asset.frame_rate, asset.width, asset.height, asset.codec, asset.camera, asset.captured_at_raw, asset.best_time, asset.offset_seconds, asset.metadata_rotation_degrees, asset.default_rotation_degrees, asset.baseline_rotation_degrees, asset.orientation_probed, asset.baseline_notes, serde_json::to_string(&asset.baseline_labels)?, asset.baseline_removed, asset.media_object_id, asset.media_verification_state.as_str(), asset.created_at])?;
            let capture_sequence_id = crate::chronology::ensure_capture_sequence_for_camera(
                &transaction,
                &snapshot.project_id,
                &asset.camera,
                &asset.created_at,
            )?;
            transaction.execute(
                "INSERT INTO asset_capture_sequence(asset_id, project_id, capture_sequence_id, filename_counter) VALUES (?1, ?2, ?3, ?4)",
                params![
                    asset.id,
                    snapshot.project_id,
                    capture_sequence_id,
                    crate::chronology::parse_filename_counter(&asset.filename)
                        .map(|counter| counter as i64),
                ],
            )?;
            crate::operations::reconcile_asset_baseline(
                &transaction,
                &snapshot.project_id,
                &asset.id,
            )?;
        }
        for event in &snapshot.baseline_clock_events {
            if event.project_id != snapshot.project_id {
                bail!("baseline clock event belongs to another project");
            }
            transaction.execute("INSERT INTO clock_events(id, project_id, camera, start_filename, offset_seconds, label, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", params![event.id, event.project_id, event.camera, event.start_filename, event.offset_seconds, event.label, event.created_at])?;
        }
        for mapping in &snapshot.root_mappings {
            transaction.execute("INSERT INTO project_storage_root_mappings(project_id, storage_root_id, relative_prefix, created_at) VALUES (?1, ?2, ?3, ?4)", params![snapshot.project_id, mapping.storage_root_id, mapping.relative_prefix, mapping.created_at])?;
        }
        transaction.execute("INSERT INTO project_baseline_metadata(singleton_id, project_id, baseline_version, baseline_digest, imported_at) VALUES (1, ?1, ?2, ?3, ?4)", params![snapshot.project_id, baseline_version, baseline_digest, crate::now()])?;
        transaction.commit()?;

        for operation in &snapshot.operations {
            if operation.project_id != snapshot.project_id {
                bail!("baseline operation belongs to another project");
            }
            catalog.apply_operation_batch(
                &operation.origin_node_id,
                std::slice::from_ref(operation),
            )?;
        }
        let imported = catalog.export_project_baseline(&snapshot.project_id)?;
        if imported.operation_cursors != snapshot.operation_cursors
            || imported.materialized_revision_count != snapshot.materialized_revision_count
            || imported.conflicts.len() != snapshot.conflicts.len()
        {
            bail!("baseline operation/materialization watermark mismatch");
        }
        Ok(catalog)
    }

    pub fn baseline_digest(&self) -> Result<Option<String>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT baseline_digest FROM project_baseline_metadata WHERE singleton_id=1",
                [],
                |row| row.get(0),
            )
            .optional()?)
    }
}

fn baseline_register(
    connection: &rusqlite::Connection,
    project_id: &str,
    asset_id: &str,
    field: &str,
) -> Result<Option<String>> {
    Ok(connection.query_row("SELECT value_json FROM lww_registers WHERE project_id=?1 AND aggregate_type='projectAsset' AND aggregate_id=?2 AND field_name=?3 AND operation_id IS NULL", params![project_id, asset_id, field], |row| row.get(0)).optional()?)
}
