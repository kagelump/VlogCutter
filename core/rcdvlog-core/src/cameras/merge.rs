//! Project-local camera identity correction.
//!
//! A camera is a lane; capture sequences are the cards inside it. Renaming or
//! merging a lane therefore changes the camera projection on assets and
//! sequences together, without deleting or combining a capture sequence.

use crate::operations::{
    CAMERA_ASSETS_REPAIRED, CAMERA_IDENTITY_MERGED, CameraIdentityMergedPayload,
    CameraSequenceAssignmentPayload, NewOperation, commit_local_in_transaction, current_revision,
};
use crate::{Catalog, chronology::CaptureSequence, table_exists};
use anyhow::{Result, anyhow, bail};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraMergePreview {
    pub project_id: String,
    pub source_camera: String,
    pub destination_camera: String,
    pub affected_clip_count: usize,
    pub affected_capture_sequence_count: usize,
    pub affected_mapping_count: usize,
    pub destination_capture_sequence_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraMergeReport {
    pub project_id: String,
    pub source_camera: String,
    pub destination_camera: String,
    pub moved_clip_count: usize,
    pub moved_capture_sequence_count: usize,
    pub moved_mapping_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraRepairPreview {
    pub project_id: String,
    pub camera: String,
    pub asset_ids: Vec<String>,
    pub capture_sequence_ids: Vec<String>,
    pub counters_to_repair: usize,
    pub assignments: Vec<CameraRepairAssignment>,
    pub expected_asset_revisions: Vec<(String, u64)>,
    pub expected_asset_cameras: Vec<(String, String)>,
    pub expected_sequence_revisions: Vec<(String, u64)>,
    pub expected_sequence_cameras: Vec<(String, String)>,
    pub expected_repair_revision: u64,
    pub new_sequences: Vec<CaptureSequence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraRepairAssignment {
    pub asset_id: String,
    pub source_capture_sequence_id: String,
    pub capture_sequence_id: String,
    pub filename_counter: Option<u64>,
}

fn normalized_pair<'a>(source: &'a str, destination: &'a str) -> Result<(&'a str, &'a str)> {
    let source = source.trim();
    let destination = destination.trim();
    if source.is_empty() || destination.is_empty() {
        bail!("source and destination camera names cannot be empty");
    }
    if source == destination {
        bail!("source and destination cameras must be different");
    }
    Ok((source, destination))
}

fn camera_exists(transaction: &Transaction<'_>, project_id: &str, camera: &str) -> Result<bool> {
    let assets: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM assets WHERE project_id=?1 AND camera=?2)",
        params![project_id, camera],
        |row| row.get(0),
    )?;
    if assets {
        return Ok(true);
    }
    let sequences: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM capture_sequences WHERE project_id=?1 AND camera_key=?2)",
        params![project_id, camera],
        |row| row.get(0),
    )?;
    if sequences {
        return Ok(true);
    }
    if table_exists(transaction, "camera_identity_mappings")? {
        return transaction
            .query_row(
                "SELECT 1 FROM camera_identity_mappings WHERE project_id=?1 AND camera=?2 LIMIT 1",
                params![project_id, camera],
                |_| Ok(true),
            )
            .optional()
            .map(|found| found.unwrap_or(false))
            .map_err(Into::into);
    }
    Ok(false)
}

fn count(
    transaction: &Transaction<'_>,
    sql: &str,
    project_id: &str,
    camera: &str,
) -> Result<usize> {
    let value =
        transaction.query_row(sql, params![project_id, camera], |row| row.get::<_, i64>(0))?;
    usize::try_from(value).map_err(|_| anyhow!("camera count exceeds this platform's limit"))
}

fn preview_in_transaction(
    transaction: &Transaction<'_>,
    project_id: &str,
    source: &str,
    destination: &str,
    require_destination: bool,
) -> Result<CameraMergePreview> {
    let project_exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
        [project_id],
        |row| row.get(0),
    )?;
    if !project_exists {
        bail!("project not found: {project_id}");
    }
    if !camera_exists(transaction, project_id, source)? {
        bail!("camera not found in project: {source}");
    }
    if require_destination && !camera_exists(transaction, project_id, destination)? {
        bail!("destination camera not found in project: {destination}");
    }
    let affected_mapping_count = if table_exists(transaction, "camera_identity_mappings")? {
        count(
            transaction,
            "SELECT COUNT(*) FROM camera_identity_mappings WHERE project_id=?1 AND camera=?2",
            project_id,
            source,
        )?
    } else {
        0
    };
    Ok(CameraMergePreview {
        project_id: project_id.to_string(),
        source_camera: source.to_string(),
        destination_camera: destination.to_string(),
        affected_clip_count: count(
            transaction,
            "SELECT COUNT(*) FROM assets WHERE project_id=?1 AND camera=?2",
            project_id,
            source,
        )?,
        affected_capture_sequence_count: count(
            transaction,
            "SELECT COUNT(*) FROM capture_sequences WHERE project_id=?1 AND camera_key=?2",
            project_id,
            source,
        )?,
        affected_mapping_count,
        destination_capture_sequence_count: count(
            transaction,
            "SELECT COUNT(*) FROM capture_sequences WHERE project_id=?1 AND camera_key=?2",
            project_id,
            destination,
        )?,
    })
}

fn identities(
    transaction: &Transaction<'_>,
    preview: &CameraMergePreview,
) -> Result<(
    Vec<String>,
    Vec<String>,
    Vec<CameraSequenceAssignmentPayload>,
)> {
    let mut asset_statement = transaction
        .prepare("SELECT id FROM assets WHERE project_id=?1 AND camera=?2 ORDER BY id")?;
    let asset_ids = asset_statement
        .query_map(params![preview.project_id, preview.source_camera], |row| {
            row.get(0)
        })?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    let mut sequence_statement = transaction.prepare(
        "SELECT id FROM capture_sequences WHERE project_id=?1 AND camera_key=?2 ORDER BY id",
    )?;
    let sequence_ids = sequence_statement
        .query_map(params![preview.project_id, preview.source_camera], |row| {
            row.get(0)
        })?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    let mut assignment_statement = transaction.prepare(
        "SELECT m.asset_id, m.capture_sequence_id FROM asset_capture_sequence m JOIN capture_sequences s ON s.id=m.capture_sequence_id WHERE m.project_id=?1 AND s.camera_key=?2 ORDER BY m.asset_id",
    )?;
    let assignments = assignment_statement
        .query_map(params![preview.project_id, preview.source_camera], |row| {
            Ok(CameraSequenceAssignmentPayload {
                asset_id: row.get(0)?,
                capture_sequence_id: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((asset_ids, sequence_ids, assignments))
}

fn commit_identity_change(
    transaction: &Transaction<'_>,
    preview: &CameraMergePreview,
) -> Result<CameraMergeReport> {
    let (asset_ids, capture_sequence_ids, assignments) = identities(transaction, preview)?;
    commit_local_in_transaction(
        transaction,
        &preview.project_id,
        vec![NewOperation {
            aggregate_type: "cameraIdentity",
            aggregate_id: preview.source_camera.clone(),
            operation_type: CAMERA_IDENTITY_MERGED,
            base_revision: None,
            payload: serde_json::to_value(CameraIdentityMergedPayload {
                source_camera: preview.source_camera.clone(),
                destination_camera: preview.destination_camera.clone(),
                asset_ids,
                capture_sequence_ids,
                assignments,
            })?,
        }],
    )?;
    Ok(CameraMergeReport {
        project_id: preview.project_id.clone(),
        source_camera: preview.source_camera.clone(),
        destination_camera: preview.destination_camera.clone(),
        moved_clip_count: preview.affected_clip_count,
        moved_capture_sequence_count: preview.affected_capture_sequence_count,
        moved_mapping_count: preview.affected_mapping_count,
    })
}

impl Catalog {
    /// Builds an exact identity/counter repair plan. Partial card selections
    /// receive a new sequence, so neither card identities nor counter spaces
    /// are combined.
    pub fn preview_camera_repair(
        &self,
        project_id: &str,
        camera: &str,
        asset_ids: &[String],
    ) -> Result<CameraRepairPreview> {
        let camera = camera.trim();
        if camera.is_empty() || asset_ids.is_empty() {
            bail!("camera and exact asset selection are required");
        }
        let mut ids = asset_ids.to_vec();
        ids.sort();
        ids.dedup();
        if ids.len() != asset_ids.len() {
            bail!("asset selection contains duplicate IDs");
        }
        let connection = self.connection()?;
        let mut assets = Vec::<(String, String, u64, Option<u64>, Option<u64>, String)>::new();
        for id in &ids {
            let (seq, revision, old_counter, old_camera): (String,u64,Option<i64>,String) = connection.query_row(
                "SELECT acs.capture_sequence_id, COALESCE(mr.revision,0), acs.filename_counter, a.camera FROM assets a JOIN asset_capture_sequence acs ON acs.asset_id=a.id LEFT JOIN materialized_revisions mr ON mr.project_id=a.project_id AND mr.aggregate_type='projectAsset' AND mr.aggregate_id=a.id WHERE a.project_id=?1 AND a.id=?2",
                params![project_id,id], |r| Ok((r.get(0)?,r.get::<_,i64>(1)? as u64,r.get(2)?,r.get(3)?)))?;
            let parsed =
                connection.query_row("SELECT filename FROM assets WHERE id=?1", [id], |r| {
                    r.get::<_, String>(0)
                })?;
            let counter = crate::chronology::parse_filename_counter(&parsed);
            assets.push((
                id.clone(),
                seq,
                revision,
                counter,
                old_counter.map(|v| v as u64),
                old_camera,
            ));
        }
        let selected_sequences = assets
            .iter()
            .map(|a| a.1.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let mut new_sequences = Vec::new();
        let mut revisions = Vec::new();
        let mut sequence_cameras = Vec::new();
        let mut sequence_ids = Vec::new();
        let mut assignments = Vec::<CameraRepairAssignment>::new();
        for seq_id in selected_sequences {
            let members: Vec<String> = {
                let mut stmt=connection.prepare("SELECT asset_id FROM asset_capture_sequence WHERE project_id=?1 AND capture_sequence_id=?2 ORDER BY asset_id")?;
                let rows = stmt.query_map(params![project_id, seq_id], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            let selected = assets
                .iter()
                .filter(|a| a.1 == seq_id)
                .map(|a| a.0.clone())
                .collect::<Vec<_>>();
            let old_seq: CaptureSequence = connection.query_row("SELECT id,camera_key,label,volume_hint,filename_prefix FROM capture_sequences WHERE project_id=?1 AND id=?2",params![project_id,seq_id],|r|Ok(CaptureSequence{id:r.get(0)?,camera_key:r.get(1)?,label:r.get(2)?,volume_hint:r.get(3)?,filename_prefix:r.get(4)?}))?;
            sequence_cameras.push((seq_id.clone(), old_seq.camera_key.clone()));
            let target_seq = if selected.len() == members.len() {
                seq_id.clone()
            } else {
                let key = format!("{project_id}:{seq_id}:{}:{}", camera, selected.join(","));
                let id = format!("sequence_repair_{}", blake3::hash(key.as_bytes()).to_hex());
                let collision: bool = connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM capture_sequences WHERE id=?1)",
                    [&id],
                    |r| r.get(0),
                )?;
                if collision {
                    bail!(
                        "camera repair destination sequence already exists; preview a fresh selection"
                    );
                }
                new_sequences.push(CaptureSequence {
                    id: id.clone(),
                    camera_key: camera.into(),
                    label: format!("{} (repaired selection)", old_seq.label),
                    volume_hint: old_seq.volume_hint,
                    filename_prefix: old_seq.filename_prefix,
                });
                id
            };
            let rev: i64=connection.query_row("SELECT COALESCE(revision,0) FROM materialized_revisions WHERE project_id=?1 AND aggregate_type='captureSequence' AND aggregate_id=?2",params![project_id,seq_id],|r|r.get(0)).optional()?.unwrap_or(0);
            revisions.push((seq_id.clone(), rev as u64));
            sequence_ids.push(target_seq.clone());
            for a in assets.iter().filter(|a| a.1 == seq_id) {
                assignments.push(CameraRepairAssignment {
                    asset_id: a.0.clone(),
                    source_capture_sequence_id: seq_id.clone(),
                    capture_sequence_id: target_seq.clone(),
                    filename_counter: a.3.or(a.4),
                });
            }
        }
        revisions.sort();
        sequence_cameras.sort();
        sequence_ids.sort();
        sequence_ids.dedup();
        let expected_asset_revisions = assets.iter().map(|a| (a.0.clone(), a.2)).collect();
        let expected_asset_cameras = assets.iter().map(|a| (a.0.clone(), a.5.clone())).collect();
        let counters_to_repair = assets
            .iter()
            .filter(|a| a.4.is_none() && a.3.is_some())
            .count();
        let repair_revision = current_revision(&connection, project_id, "cameraRepair", "assets")?;
        Ok(CameraRepairPreview {
            project_id: project_id.into(),
            camera: camera.into(),
            asset_ids: ids,
            capture_sequence_ids: sequence_ids,
            counters_to_repair,
            assignments,
            expected_asset_revisions,
            expected_asset_cameras,
            expected_sequence_revisions: revisions,
            expected_sequence_cameras: sequence_cameras,
            expected_repair_revision: repair_revision,
            new_sequences,
        })
    }

    /// Applies an unchanged exact repair preview as one synchronized operation.
    pub fn apply_camera_repair(&self, preview: &CameraRepairPreview) -> Result<usize> {
        let current =
            self.preview_camera_repair(&preview.project_id, &preview.camera, &preview.asset_ids)?;
        if current != *preview {
            bail!("camera repair preview is stale; preview again before applying");
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let revision =
            current_revision(&transaction, &preview.project_id, "cameraRepair", "assets")?;
        if revision != preview.expected_repair_revision {
            bail!("camera repair preview is stale; preview again before applying");
        }
        for (id, expected) in &preview.expected_asset_revisions {
            if current_revision(&transaction, &preview.project_id, "projectAsset", id)? != *expected
            {
                bail!("camera repair preview is stale; preview again before applying");
            }
        }
        for (id, expected) in &preview.expected_asset_cameras {
            let actual: String = transaction.query_row(
                "SELECT camera FROM assets WHERE project_id=?1 AND id=?2",
                params![preview.project_id, id],
                |r| r.get(0),
            )?;
            if actual != *expected {
                bail!("camera repair preview is stale; preview again before applying");
            }
        }
        for (id, expected) in &preview.expected_sequence_revisions {
            if current_revision(&transaction, &preview.project_id, "captureSequence", id)?
                != *expected
            {
                bail!("camera repair preview is stale; preview again before applying");
            }
        }
        for (id, expected) in &preview.expected_sequence_cameras {
            let actual: String = transaction.query_row(
                "SELECT camera_key FROM capture_sequences WHERE project_id=?1 AND id=?2",
                params![preview.project_id, id],
                |r| r.get(0),
            )?;
            if actual != *expected {
                bail!("camera repair preview is stale; preview again before applying");
            }
        }
        let payload = serde_json::json!({"camera":preview.camera,"assignments":preview.assignments,"newSequences":preview.new_sequences});
        commit_local_in_transaction(
            &transaction,
            &preview.project_id,
            vec![NewOperation {
                aggregate_type: "cameraRepair",
                aggregate_id: "assets".into(),
                operation_type: CAMERA_ASSETS_REPAIRED,
                base_revision: Some(revision),
                payload,
            }],
        )?;
        transaction.commit()?;
        Ok(preview.asset_ids.len())
    }

    /// Resolves a camera spelling through the durable merge equivalence.
    /// Import preflight uses this before showing an identity, so the preview
    /// cannot disagree with operation materialization at commit time.
    pub(crate) fn canonical_camera_identity(
        &self,
        project_id: &str,
        camera: &str,
    ) -> Result<String> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        crate::operations::resolve_camera_alias(&transaction, project_id, camera)
    }

    /// Describes a camera identity merge without mutating the catalog.
    pub fn preview_camera_merge(
        &self,
        project_id: &str,
        source: &str,
        destination: &str,
    ) -> Result<CameraMergePreview> {
        let (source, destination) = normalized_pair(source, destination)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        preview_in_transaction(&transaction, project_id, source, destination, true)
    }

    /// Merges one accidental camera identity into another existing project
    /// camera. Capture-sequence and clock-epoch identities are never changed.
    pub fn merge_camera(
        &self,
        project_id: &str,
        source: &str,
        destination: &str,
    ) -> Result<CameraMergeReport> {
        let (source, destination) = normalized_pair(source, destination)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let preview = preview_in_transaction(&transaction, project_id, source, destination, true)?;
        let report = commit_identity_change(&transaction, &preview)?;
        transaction.commit()?;
        Ok(report)
    }

    /// Renames a camera identity, including the card and ingest projections.
    /// Unlike merge, the destination need not already exist.
    pub(crate) fn rename_camera_identity(
        &self,
        project_id: &str,
        source: &str,
        destination: &str,
    ) -> Result<CameraMergeReport> {
        let (source, destination) = normalized_pair(source, destination)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let preview = preview_in_transaction(&transaction, project_id, source, destination, false)?;
        let report = commit_identity_change(&transaction, &preview)?;
        transaction.commit()?;
        Ok(report)
    }
}

#[cfg(test)]
#[path = "merge_tests.rs"]
mod tests;
