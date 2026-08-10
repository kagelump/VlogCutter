use crate::{
    Asset, AssetPatch, Catalog, ClockEvent, MediaVerificationState, normalize_datetime,
    normalize_labels, now,
};
use anyhow::{Result, anyhow, bail};
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::BTreeSet;
use uuid::Uuid;

pub const NOTE_REVISION_CREATED: &str = "noteRevisionCreated";
pub const LABEL_ADDED: &str = "labelAdded";
pub const LABEL_REMOVED: &str = "labelRemoved";
pub const REMOVED_SET: &str = "removedSet";
pub const ROTATION_SET: &str = "rotationSet";
pub const PROJECT_ASSET_ADDED: &str = "projectAssetAdded";
pub const PROJECT_ASSET_MEMBERSHIP_SET: &str = "projectAssetMembershipSet";
pub const PROJECT_ASSET_MEDIA_LINKED: &str = "projectAssetMediaLinked";
pub const CLOCK_EVENT_CREATED: &str = "clockEventCreated";
pub const CAPTURE_SEQUENCE_DECLARED: &str = "captureSequenceDeclared";
pub const CAPTURE_SEQUENCE_SPLIT: &str = "captureSequenceSplit";
pub const CAPTURE_SEQUENCE_MERGED: &str = "captureSequenceMerged";
pub const CLOCK_EPOCH_SET: &str = "clockEpochSet";
pub const CLOCK_EPOCH_CLEARED: &str = "clockEpochCleared";
pub const CLIP_TIME_EXCEPTION_SET: &str = "clipTimeExceptionSet";
pub const CLIP_TIME_EXCEPTION_CLEARED: &str = "clipTimeExceptionCleared";
pub const TRIP_PHASE_SET: &str = "tripPhaseSet";
pub const TRIP_PHASE_REMOVED: &str = "tripPhaseRemoved";
pub const EVENT_ORDER_CONSTRAINT_SET: &str = "eventOrderConstraintSet";
pub const EVENT_ORDER_CONSTRAINT_CLEARED: &str = "eventOrderConstraintCleared";
pub const EDIT_PLAN_REVISION_CREATED: &str = "editPlanRevisionCreated";
pub const EDIT_PLAN_REVISION_ACTIVATED: &str = "editPlanRevisionActivated";
pub const EDIT_PLAN_EXPORT_RECORDED: &str = "editPlanExportRecorded";
pub const MAX_REMOTE_HLC_FUTURE_SKEW_MILLIS: i64 = 5 * 60 * 1000;

/// The eleven chronology operation types D01-T007 added. The re-dispatch seam
/// on writable catalog open is scoped to exactly these: for the legacy
/// vocabulary a persisted row is never a signal that materialization is
/// missing, so replaying them would rewrite the legacy log.
pub(crate) const CHRONOLOGY_OPERATION_TYPES: &[&str] = &[
    CAPTURE_SEQUENCE_DECLARED,
    CAPTURE_SEQUENCE_SPLIT,
    CAPTURE_SEQUENCE_MERGED,
    CLOCK_EPOCH_SET,
    CLOCK_EPOCH_CLEARED,
    CLIP_TIME_EXCEPTION_SET,
    CLIP_TIME_EXCEPTION_CLEARED,
    TRIP_PHASE_SET,
    TRIP_PHASE_REMOVED,
    EVENT_ORDER_CONSTRAINT_SET,
    EVENT_ORDER_CONSTRAINT_CLEARED,
];

/// Every operation type this build recognizes. Anything in the operation log
/// outside this set is retained but quarantined: it is durable, reported
/// through `quarantined_operations`, never advances an aggregate revision, and
/// never fails the containing batch. A build that knows the type materializes
/// it normally from the persisted log.
pub(crate) const KNOWN_OPERATION_TYPES: &[&str] = &[
    NOTE_REVISION_CREATED,
    LABEL_ADDED,
    LABEL_REMOVED,
    REMOVED_SET,
    ROTATION_SET,
    PROJECT_ASSET_ADDED,
    PROJECT_ASSET_MEMBERSHIP_SET,
    PROJECT_ASSET_MEDIA_LINKED,
    CLOCK_EVENT_CREATED,
    CAPTURE_SEQUENCE_DECLARED,
    CAPTURE_SEQUENCE_SPLIT,
    CAPTURE_SEQUENCE_MERGED,
    CLOCK_EPOCH_SET,
    CLOCK_EPOCH_CLEARED,
    CLIP_TIME_EXCEPTION_SET,
    CLIP_TIME_EXCEPTION_CLEARED,
    TRIP_PHASE_SET,
    TRIP_PHASE_REMOVED,
    EVENT_ORDER_CONSTRAINT_SET,
    EVENT_ORDER_CONSTRAINT_CLEARED,
    EDIT_PLAN_REVISION_CREATED,
    EDIT_PLAN_REVISION_ACTIVATED,
    EDIT_PLAN_EXPORT_RECORDED,
];

pub(crate) fn is_known_operation_type(value: &str) -> bool {
    KNOWN_OPERATION_TYPES.contains(&value)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HybridLogicalTime {
    pub wall_millis: i64,
    pub counter: u64,
    pub node_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectOperation {
    pub operation_id: String,
    pub project_id: String,
    pub origin_node_id: String,
    pub origin_sequence: u64,
    pub logical_time: HybridLogicalTime,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub operation_type: String,
    pub payload_version: u32,
    pub base_revision: Option<u64>,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyOperationResult {
    Applied,
    Duplicate,
    Conflict,
    /// The operation was persisted but not materialized, because its operation
    /// type is unknown to this build. Durable, visible, and no aggregate
    /// revision advanced (quarantine policy, D01-T007 decision 4).
    Quarantined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictProjection {
    pub conflict_id: String,
    pub project_id: String,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub incoming_operation_id: String,
    pub active_operation_id: Option<String>,
    pub expected_base_revision: Option<u64>,
    pub actual_revision: u64,
    pub conflict_type: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationDiagnostics {
    pub project_id: String,
    pub operation_count: u64,
    pub latest_local_sequence: u64,
    pub unresolved_conflicts: Vec<ConflictProjection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteRevisionProjection {
    pub revision_id: String,
    pub operation_id: Option<String>,
    pub text: String,
    pub logical_time: HybridLogicalTime,
    pub active: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetRevisionDiagnostics {
    pub asset_id: String,
    pub materialized_revision: u64,
    pub note_revisions: Vec<NoteRevisionProjection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationBatchResult {
    pub origin_node_id: String,
    pub committed_sequence: u64,
    pub applied: usize,
    pub duplicates: usize,
    pub conflicts: usize,
    /// Operations persisted but not materialized because this build does not
    /// know their type. They remain durable in `project_operations` and are
    /// reported through `quarantined_operations`.
    pub quarantined: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SequenceRecoveryState {
    Awaiting,
    Divergent,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SequenceRecoveryProjection {
    pub peer_origin_node_id: String,
    pub server_origin_node_id: String,
    pub manifest_watermark: u64,
    pub contained_watermark: u64,
    pub reported_watermark: Option<u64>,
    pub state: SequenceRecoveryState,
    pub divergence_start: Option<u64>,
    pub divergence_end: Option<u64>,
    pub decision: Option<String>,
    pub updated_at: String,
}

/// Path-free projection of one project asset. `assets.path` and
/// `assets.proxy_path` are node-local absolutes and are deliberately absent.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAssetItem {
    pub project_asset_id: String,
    pub filename: String,
    pub camera: String,
    pub media_object_id: Option<String>,
    pub media_verification_state: String,
    pub removed: bool,
    pub rotation_degrees: i32,
    pub labels: Vec<String>,
    pub active_note: Option<String>,
    pub duration_seconds: f64,
    pub best_time: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAssetPage {
    pub items: Vec<ProjectAssetItem>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NotePayload {
    revision_id: String,
    text: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LabelAddPayload {
    add_tag: String,
    label: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LabelRemovePayload {
    normalized_label: String,
    observed_add_tags: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemovedPayload {
    removed: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RotationPayload {
    rotation_degrees: u16,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MembershipPayload {
    present: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClockPayload {
    event_id: String,
    camera: String,
    start_filename: String,
    offset_seconds: i64,
    label: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectAssetAddedPayload {
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
    pub metadata_rotation_degrees: u16,
    pub default_rotation_degrees: u16,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MediaLinkedPayload {
    media_object_id: String,
    verification_state: MediaVerificationState,
}

/// Chronology operation payloads, frozen by D01-T007
/// (`docs/tasks/desktop/CHRONOLOGY_MODEL.md`). All are payload version 1 with
/// `deny_unknown_fields`, matching the existing operation payload discipline.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CaptureSequenceDeclaredPayload {
    pub(crate) sequence_id: String,
    pub(crate) camera_key: String,
    pub(crate) label: String,
    pub(crate) volume_hint: String,
    pub(crate) filename_prefix: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CaptureSequenceSplitPayload {
    pub(crate) new_sequence_id: String,
    pub(crate) camera_key: String,
    pub(crate) label: String,
    pub(crate) volume_hint: String,
    pub(crate) filename_prefix: String,
    pub(crate) at_counter: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CaptureSequenceMergedPayload {
    pub(crate) from_sequence_id: String,
    pub(crate) into_sequence_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ClockEpochSetPayload {
    pub(crate) epoch_id: String,
    pub(crate) capture_sequence_id: String,
    pub(crate) start_counter: u64,
    pub(crate) end_counter: Option<u64>,
    pub(crate) offset_seconds: i64,
    pub(crate) label: String,
    pub(crate) confidence: Option<f32>,
    pub(crate) evidence: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ClockEpochClearedPayload {
    pub(crate) epoch_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ClipTimeExceptionSetPayload {
    pub(crate) trip_time: String,
    pub(crate) reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ClipTimeExceptionClearedPayload {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TripPhaseSetPayload {
    pub(crate) phase_id: String,
    pub(crate) label: String,
    pub(crate) phase_kind: String,
    #[serde(default)]
    pub(crate) start_anchor_asset_id: Option<String>,
    #[serde(default)]
    pub(crate) end_anchor_asset_id: Option<String>,
    pub(crate) start_trip_time: String,
    pub(crate) end_trip_time: String,
    pub(crate) timezone: Option<String>,
    pub(crate) origin_timezone: Option<String>,
    pub(crate) destination_timezone: Option<String>,
    pub(crate) display_mode: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TripPhaseRemovedPayload {
    pub(crate) phase_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct EventOrderConstraintSetPayload {
    pub(crate) constraint_id: String,
    pub(crate) earlier_asset_id: String,
    pub(crate) later_asset_id: String,
    pub(crate) strength: String,
    pub(crate) source: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct EventOrderConstraintClearedPayload {
    pub(crate) constraint_id: String,
}

pub(crate) struct NewOperation {
    pub(crate) aggregate_type: &'static str,
    pub(crate) aggregate_id: String,
    pub(crate) operation_type: &'static str,
    pub(crate) base_revision: Option<u64>,
    pub(crate) payload: Value,
}

impl Catalog {
    /// Tombstones project membership without touching the asset identity,
    /// original locations, or operation history. This is deliberately a
    /// synced operation instead of a SQL delete: peers must not replay an old
    /// add operation and silently bring a rolled-back import back into view.
    pub fn remove_project_asset_membership(
        &self,
        project_id: &str,
        asset_ids: &[String],
    ) -> Result<()> {
        let operations = asset_ids
            .iter()
            .map(|asset_id| {
                Ok(NewOperation {
                    aggregate_type: "projectAsset",
                    aggregate_id: asset_id.clone(),
                    operation_type: PROJECT_ASSET_MEMBERSHIP_SET,
                    base_revision: None,
                    payload: serde_json::to_value(MembershipPayload { present: false })?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        self.commit_local(project_id, operations)
    }

    pub(crate) fn restore_project_asset_membership(
        &self,
        project_id: &str,
        asset_id: &str,
    ) -> Result<()> {
        self.commit_local(
            project_id,
            vec![NewOperation {
                aggregate_type: "projectAsset",
                aggregate_id: asset_id.to_string(),
                operation_type: PROJECT_ASSET_MEMBERSHIP_SET,
                base_revision: None,
                payload: serde_json::to_value(MembershipPayload { present: true })?,
            }],
        )
    }
    /// Seeds the stable local node identity supplied by an installation adapter.
    /// A catalog that has already acknowledged operations cannot be reassigned.
    pub fn set_local_node_id(&self, node_id: &str) -> Result<()> {
        validate_id("node ID", node_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT node_id FROM catalog_identity WHERE singleton_id = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match existing {
            Some(existing) if existing != node_id => {
                let operations: i64 = transaction.query_row(
                    "SELECT COUNT(*) FROM project_operations",
                    [],
                    |row| row.get(0),
                )?;
                if operations != 0 {
                    bail!("catalog node identity is already in use: {existing}");
                }
                transaction.execute(
                    "UPDATE catalog_identity SET node_id = ?1 WHERE singleton_id = 1",
                    [node_id],
                )?;
                transaction.execute(
                    "UPDATE projects SET origin_node_id=?1 WHERE origin_node_id=?2",
                    params![node_id, existing],
                )?;
                transaction.execute(
                    "DELETE FROM project_asset_local_locations WHERE node_id=?1",
                    [existing],
                )?;
            }
            Some(_) => {}
            None => {
                transaction.execute(
                    "INSERT INTO catalog_identity(singleton_id, node_id, created_at) VALUES (1, ?1, ?2)",
                    params![node_id, now()],
                )?;
            }
        }
        backfill_local_locations(&transaction, node_id)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn local_node_id(&self) -> Result<Option<String>> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT node_id FROM catalog_identity WHERE singleton_id = 1",
                [],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn create_note_revision(&self, asset_id: &str, text: &str) -> Result<Asset> {
        let current = self.get_asset(asset_id)?;
        if current.notes == text {
            return Ok(current);
        }
        let operation = NewOperation {
            aggregate_type: "projectAsset",
            aggregate_id: asset_id.to_string(),
            operation_type: NOTE_REVISION_CREATED,
            base_revision: None,
            payload: serde_json::to_value(NotePayload {
                revision_id: format!("note_{}", Uuid::new_v4().simple()),
                text: text.to_string(),
            })?,
        };
        self.commit_local(&current.project_id, vec![operation])?;
        self.get_asset(asset_id)
    }

    pub fn add_label(&self, asset_id: &str, label: &str) -> Result<Asset> {
        let current = self.get_asset(asset_id)?;
        let normalized = normalize_labels(&[label.to_string()])?;
        let Some(label) = normalized.into_iter().next() else {
            return Ok(current);
        };
        if current
            .labels
            .iter()
            .any(|value| value.eq_ignore_ascii_case(&label))
        {
            return Ok(current);
        }
        if current.labels.len() >= 24 {
            bail!("invalid labels: an asset can have at most 24 labels");
        }
        let operation = label_add_operation(asset_id, label)?;
        self.commit_local(&current.project_id, vec![operation])?;
        self.get_asset(asset_id)
    }

    pub fn remove_label(&self, asset_id: &str, label: &str) -> Result<Asset> {
        let current = self.get_asset(asset_id)?;
        let Some(display) = current
            .labels
            .iter()
            .find(|value| value.eq_ignore_ascii_case(label.trim()))
        else {
            return Ok(current);
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        reconcile_asset_baseline(&transaction, &current.project_id, asset_id)?;
        let tags = active_label_tags(&transaction, asset_id, &label_key(display))?;
        if tags.is_empty() {
            transaction.commit()?;
            return self.get_asset(asset_id);
        }
        let operation = NewOperation {
            aggregate_type: "projectAsset",
            aggregate_id: asset_id.to_string(),
            operation_type: LABEL_REMOVED,
            base_revision: None,
            payload: serde_json::to_value(LabelRemovePayload {
                normalized_label: label_key(display),
                observed_add_tags: tags,
            })?,
        };
        commit_local_in_transaction(&transaction, &current.project_id, vec![operation])?;
        transaction.commit()?;
        self.get_asset(asset_id)
    }

    pub fn set_removed(&self, asset_id: &str, removed: bool) -> Result<Asset> {
        let current = self.get_asset(asset_id)?;
        if current.removed == removed {
            return Ok(current);
        }
        self.commit_local(
            &current.project_id,
            vec![NewOperation {
                aggregate_type: "projectAsset",
                aggregate_id: asset_id.to_string(),
                operation_type: REMOVED_SET,
                base_revision: None,
                payload: serde_json::to_value(RemovedPayload { removed })?,
            }],
        )?;
        self.get_asset(asset_id)
    }

    pub fn set_rotation(&self, asset_id: &str, rotation_degrees: u16) -> Result<Asset> {
        if !matches!(rotation_degrees, 0 | 90 | 180 | 270) {
            bail!("invalid rotation: {rotation_degrees}; expected 0, 90, 180, or 270 degrees");
        }
        let current = self.get_asset(asset_id)?;
        if current.rotation_degrees == rotation_degrees {
            return Ok(current);
        }
        self.commit_local(
            &current.project_id,
            vec![NewOperation {
                aggregate_type: "projectAsset",
                aggregate_id: asset_id.to_string(),
                operation_type: ROTATION_SET,
                base_revision: None,
                payload: serde_json::to_value(RotationPayload { rotation_degrees })?,
            }],
        )?;
        self.get_asset(asset_id)
    }

    /// Compatibility adapter for the desktop's full replacement patch.
    pub(crate) fn update_asset_with_operations(
        &self,
        asset_id: &str,
        patch: &AssetPatch,
    ) -> Result<Asset> {
        if !matches!(patch.rotation_degrees, 0 | 90 | 180 | 270) {
            bail!(
                "invalid rotation: {}; expected 0, 90, 180, or 270 degrees",
                patch.rotation_degrees
            );
        }
        let labels = normalize_labels(&patch.labels)?;
        let current = self.get_asset(asset_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        reconcile_asset_baseline(&transaction, &current.project_id, asset_id)?;
        let mut operations = Vec::new();
        if current.notes != patch.notes {
            operations.push(NewOperation {
                aggregate_type: "projectAsset",
                aggregate_id: asset_id.to_string(),
                operation_type: NOTE_REVISION_CREATED,
                base_revision: None,
                payload: serde_json::to_value(NotePayload {
                    revision_id: format!("note_{}", Uuid::new_v4().simple()),
                    text: patch.notes.clone(),
                })?,
            });
        }
        for label in &current.labels {
            if !labels
                .iter()
                .any(|wanted| wanted.eq_ignore_ascii_case(label))
            {
                let normalized_label = label_key(label);
                operations.push(NewOperation {
                    aggregate_type: "projectAsset",
                    aggregate_id: asset_id.to_string(),
                    operation_type: LABEL_REMOVED,
                    base_revision: None,
                    payload: serde_json::to_value(LabelRemovePayload {
                        observed_add_tags: active_label_tags(
                            &transaction,
                            asset_id,
                            &normalized_label,
                        )?,
                        normalized_label,
                    })?,
                });
            }
        }
        for label in labels {
            if !current
                .labels
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&label))
            {
                operations.push(label_add_operation(asset_id, label)?);
            }
        }
        if current.removed != patch.removed {
            operations.push(NewOperation {
                aggregate_type: "projectAsset",
                aggregate_id: asset_id.to_string(),
                operation_type: REMOVED_SET,
                base_revision: None,
                payload: serde_json::to_value(RemovedPayload {
                    removed: patch.removed,
                })?,
            });
        }
        if current.rotation_degrees != patch.rotation_degrees {
            operations.push(NewOperation {
                aggregate_type: "projectAsset",
                aggregate_id: asset_id.to_string(),
                operation_type: ROTATION_SET,
                base_revision: None,
                payload: serde_json::to_value(RotationPayload {
                    rotation_degrees: patch.rotation_degrees,
                })?,
            });
        }
        if !operations.is_empty() {
            commit_local_in_transaction(&transaction, &current.project_id, operations)?;
        }
        transaction.commit()?;
        self.get_asset(asset_id)
    }

    pub(crate) fn add_project_asset(
        &self,
        project_id: &str,
        local_path: &str,
        payload: ProjectAssetAddedPayload,
    ) -> Result<()> {
        let asset_id = payload.id.clone();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        commit_local_in_transaction(
            &transaction,
            project_id,
            vec![NewOperation {
                aggregate_type: "projectAsset",
                aggregate_id: asset_id.clone(),
                operation_type: PROJECT_ASSET_ADDED,
                base_revision: Some(0),
                payload: serde_json::to_value(payload)?,
            }],
        )?;
        let node_id = ensure_node(&transaction)?;
        transaction.execute(
            "INSERT INTO project_asset_local_locations(asset_id, node_id, local_path, created_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(asset_id, node_id, local_path) DO NOTHING",
            params![asset_id, node_id, local_path, now()],
        )?;
        transaction.execute(
            "UPDATE assets SET path=?2 WHERE id=?1",
            params![asset_id, local_path],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn add_portable_project_asset(
        &self,
        project_id: &str,
        payload: ProjectAssetAddedPayload,
    ) -> Result<Asset> {
        let asset_id = payload.id.clone();
        self.commit_local(
            project_id,
            vec![NewOperation {
                aggregate_type: "projectAsset",
                aggregate_id: asset_id.clone(),
                operation_type: PROJECT_ASSET_ADDED,
                base_revision: Some(0),
                payload: serde_json::to_value(payload)?,
            }],
        )?;
        self.get_asset(&asset_id)
    }

    pub fn link_project_asset_media(
        &self,
        asset_id: &str,
        media_object_id: &str,
        verification_state: MediaVerificationState,
    ) -> Result<Asset> {
        let current = self.get_asset(asset_id)?;
        if current.media_object_id.as_deref() == Some(media_object_id)
            && current.media_verification_state == verification_state
        {
            return Ok(current);
        }
        self.commit_local(
            &current.project_id,
            vec![NewOperation {
                aggregate_type: "projectAsset",
                aggregate_id: asset_id.to_string(),
                operation_type: PROJECT_ASSET_MEDIA_LINKED,
                base_revision: None,
                payload: serde_json::to_value(MediaLinkedPayload {
                    media_object_id: media_object_id.to_string(),
                    verification_state,
                })?,
            }],
        )?;
        self.get_asset(asset_id)
    }

    pub(crate) fn create_clock_event_operation(
        &self,
        project_id: &str,
        camera: &str,
        start_filename: &str,
        offset_seconds: i64,
        label: &str,
    ) -> Result<ClockEvent> {
        self.require_project(project_id)?;
        if camera.trim().is_empty() || start_filename.trim().is_empty() {
            bail!("camera and start filename cannot be empty");
        }
        let event = ClockEvent {
            id: format!("clock_{}", Uuid::new_v4().simple()),
            project_id: project_id.to_string(),
            camera: camera.to_string(),
            start_filename: start_filename.to_string(),
            offset_seconds,
            label: label.trim().to_string(),
            created_at: now(),
        };
        let revision = current_revision(&self.connection()?, project_id, "clock", camera)?;
        self.commit_local(
            project_id,
            vec![NewOperation {
                aggregate_type: "clock",
                aggregate_id: camera.to_string(),
                operation_type: CLOCK_EVENT_CREATED,
                base_revision: Some(revision),
                payload: serde_json::to_value(ClockPayload {
                    event_id: event.id.clone(),
                    camera: event.camera.clone(),
                    start_filename: event.start_filename.clone(),
                    offset_seconds,
                    label: event.label.clone(),
                })?,
            }],
        )?;
        Ok(event)
    }

    pub fn apply_operation(&self, operation: &ProjectOperation) -> Result<ApplyOperationResult> {
        validate_operation(operation)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if operation_exists(&transaction, &operation.operation_id)? {
            return Ok(ApplyOperationResult::Duplicate);
        }
        let result = insert_and_materialize(&transaction, operation)?;
        receive_hlc(&transaction, operation)?;
        transaction.commit()?;
        // A quarantined operation is retained and reported by
        // `quarantined_operations()`, but nothing would tell the user that
        // surface is non-empty. Record it after the commit so a diagnostic
        // failure can never roll back a successfully applied operation
        // (D01-T027).
        if result == ApplyOperationResult::Quarantined {
            let _ = self.record_quarantined_operation(
                &operation.project_id,
                &operation.operation_id,
                &operation.operation_type,
            );
        }
        Ok(result)
    }

    /// Applies one origin's contiguous operation page and advances its durable
    /// acknowledgement cursor in the same SQLite transaction.
    pub fn apply_operation_batch(
        &self,
        origin_node_id: &str,
        operations: &[ProjectOperation],
    ) -> Result<OperationBatchResult> {
        validate_id("origin node ID", origin_node_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut cursor = transaction
            .query_row(
                "SELECT committed_sequence FROM sync_origin_cursors WHERE origin_node_id=?1",
                [origin_node_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0) as u64;
        let mut quarantined: Vec<(String, String, String)> = Vec::new();
        let mut result = OperationBatchResult {
            origin_node_id: origin_node_id.to_string(),
            committed_sequence: cursor,
            applied: 0,
            duplicates: 0,
            conflicts: 0,
            quarantined: 0,
        };
        for operation in operations {
            validate_operation(operation)?;
            if operation.origin_node_id != origin_node_id {
                bail!("operation origin does not match batch origin");
            }
            if operation.origin_sequence <= cursor {
                if !operation_matches_acknowledged_sequence(&transaction, operation)? {
                    bail!(
                        "operation sequence {} is already acknowledged with a different operation",
                        operation.origin_sequence
                    );
                }
                result.duplicates += 1;
                continue;
            }
            let expected = cursor + 1;
            if operation.origin_sequence != expected {
                bail!(
                    "operation sequence gap for {origin_node_id}: expected {expected}, received {}",
                    operation.origin_sequence
                );
            }
            match insert_and_materialize(&transaction, operation)? {
                ApplyOperationResult::Applied => result.applied += 1,
                ApplyOperationResult::Duplicate => result.duplicates += 1,
                ApplyOperationResult::Conflict => result.conflicts += 1,
                ApplyOperationResult::Quarantined => {
                    result.quarantined += 1;
                    quarantined.push((
                        operation.project_id.clone(),
                        operation.operation_id.clone(),
                        operation.operation_type.clone(),
                    ));
                }
            }
            receive_hlc(&transaction, operation)?;
            cursor = operation.origin_sequence;
        }
        transaction.execute(
            "INSERT INTO sync_origin_cursors(origin_node_id, committed_sequence, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(origin_node_id) DO UPDATE SET committed_sequence=excluded.committed_sequence, updated_at=excluded.updated_at",
            params![origin_node_id, cursor as i64, now()],
        )?;
        result.committed_sequence = cursor;
        transaction.commit()?;
        // Recorded after the commit, so a diagnostic failure can never roll
        // back a successfully applied batch (D01-T027).
        for (project_id, operation_id, operation_type) in quarantined {
            let _ = self.record_quarantined_operation(&project_id, &operation_id, &operation_type);
        }
        Ok(result)
    }

    pub fn operation_cursors(&self) -> Result<std::collections::BTreeMap<String, u64>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT origin_node_id, MAX(sequence) FROM (
                SELECT origin_node_id, committed_sequence AS sequence FROM sync_origin_cursors
                UNION ALL
                SELECT origin_node_id, origin_sequence AS sequence FROM project_operations
            ) GROUP BY origin_node_id ORDER BY origin_node_id
            "#,
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn peer_operation_acknowledgements(
        &self,
        peer_node_id: &str,
    ) -> Result<std::collections::BTreeMap<String, u64>> {
        validate_id("peer node ID", peer_node_id)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT origin_node_id, committed_sequence FROM sync_peer_operation_acknowledgements WHERE peer_node_id=?1 ORDER BY origin_node_id",
        )?;
        Ok(statement
            .query_map([peer_node_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn record_peer_operation_acknowledgements(
        &self,
        peer_node_id: &str,
        cursors: &std::collections::BTreeMap<String, u64>,
    ) -> Result<()> {
        validate_id("peer node ID", peer_node_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let timestamp = now();
        for (origin_node_id, sequence) in cursors {
            validate_id("origin node ID", origin_node_id)?;
            let sequence = i64::try_from(*sequence)?;
            transaction.execute(
                "INSERT INTO sync_peer_operation_acknowledgements(peer_node_id,origin_node_id,committed_sequence,updated_at) VALUES(?1,?2,?3,?4) ON CONFLICT(peer_node_id,origin_node_id) DO UPDATE SET committed_sequence=MAX(committed_sequence,excluded.committed_sequence),updated_at=excluded.updated_at",
                params![peer_node_id, origin_node_id, sequence, timestamp],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_sequence_recovery(
        &self,
        peer_origin_node_id: &str,
        server_origin_node_id: &str,
        manifest_watermark: u64,
    ) -> Result<SequenceRecoveryProjection> {
        validate_id("peer origin node ID", peer_origin_node_id)?;
        validate_id("server origin node ID", server_origin_node_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let contained = transaction.query_row(
            "SELECT COALESCE(MAX(origin_sequence), 0) FROM project_operations WHERE origin_node_id=?1",
            [server_origin_node_id],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let timestamp = now();
        transaction.execute(
            r#"
            INSERT INTO operation_sequence_recovery(
                peer_origin_node_id, server_origin_node_id, manifest_watermark,
                contained_watermark, state, created_at, updated_at
            ) VALUES(?1, ?2, ?3, ?4, 'awaiting', ?5, ?5)
            ON CONFLICT(peer_origin_node_id) DO UPDATE SET
                server_origin_node_id=excluded.server_origin_node_id,
                manifest_watermark=excluded.manifest_watermark,
                contained_watermark=excluded.contained_watermark,
                reported_watermark=NULL, state='awaiting', divergence_start=NULL,
                divergence_end=NULL, decision=NULL, updated_at=excluded.updated_at
            "#,
            params![
                peer_origin_node_id,
                server_origin_node_id,
                i64::try_from(manifest_watermark)?,
                i64::try_from(contained)?,
                timestamp,
            ],
        )?;
        let projection = load_sequence_recovery(&transaction, peer_origin_node_id)?;
        transaction.commit()?;
        Ok(projection)
    }

    pub fn reconcile_sequence_recovery(
        &self,
        peer_origin_node_id: &str,
        reported_server_sequence: u64,
    ) -> Result<Option<SequenceRecoveryProjection>> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(current) = load_sequence_recovery_optional(&transaction, peer_origin_node_id)?
        else {
            transaction.commit()?;
            return Ok(None);
        };
        if current.state == SequenceRecoveryState::Completed {
            transaction.commit()?;
            return Ok(Some(current));
        }
        let contained_watermark = transaction.query_row(
            "SELECT COALESCE(MAX(origin_sequence), 0) FROM project_operations WHERE origin_node_id=?1",
            [&current.server_origin_node_id],
            |row| row.get::<_, i64>(0),
        )? as u64;
        if contained_watermark != current.contained_watermark {
            transaction.execute(
                "UPDATE operation_sequence_recovery SET contained_watermark=?2 WHERE peer_origin_node_id=?1",
                params![peer_origin_node_id, i64::try_from(contained_watermark)?],
            )?;
        }
        let timestamp = now();
        if reported_server_sequence > contained_watermark {
            transaction.execute(
                "UPDATE operation_sequence_recovery SET reported_watermark=?2,state='divergent',divergence_start=?3,divergence_end=?2,updated_at=?4 WHERE peer_origin_node_id=?1",
                params![
                    peer_origin_node_id,
                    i64::try_from(reported_server_sequence)?,
                    i64::try_from(contained_watermark.saturating_add(1))?,
                    timestamp,
                ],
            )?;
        } else {
            let safe = current
                .manifest_watermark
                .max(contained_watermark)
                .max(reported_server_sequence);
            transaction.execute(
                "INSERT INTO operation_origin_sequences(origin_node_id,last_sequence) VALUES(?1,?2) ON CONFLICT(origin_node_id) DO UPDATE SET last_sequence=MAX(last_sequence,excluded.last_sequence)",
                params![current.server_origin_node_id, i64::try_from(safe)?],
            )?;
            transaction.execute(
                "UPDATE operation_sequence_recovery SET reported_watermark=?2,state='completed',divergence_start=NULL,divergence_end=NULL,updated_at=?3 WHERE peer_origin_node_id=?1",
                params![peer_origin_node_id, i64::try_from(reported_server_sequence)?, timestamp],
            )?;
        }
        let projection = load_sequence_recovery(&transaction, peer_origin_node_id)?;
        transaction.commit()?;
        Ok(Some(projection))
    }

    pub fn accept_sequence_divergence(
        &self,
        peer_origin_node_id: &str,
        decision: &str,
    ) -> Result<SequenceRecoveryProjection> {
        if decision != "accept_allocator_advance" {
            bail!("sequence recovery decision is not recognized");
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = load_sequence_recovery(&transaction, peer_origin_node_id)?;
        if current.state != SequenceRecoveryState::Divergent {
            bail!("sequence recovery does not have a divergence to accept");
        }
        let reported = current
            .reported_watermark
            .ok_or_else(|| anyhow!("divergence is missing its reported watermark"))?;
        transaction.execute(
            "INSERT INTO operation_origin_sequences(origin_node_id,last_sequence) VALUES(?1,?2) ON CONFLICT(origin_node_id) DO UPDATE SET last_sequence=MAX(last_sequence,excluded.last_sequence)",
            params![current.server_origin_node_id, i64::try_from(reported)?],
        )?;
        transaction.execute(
            "UPDATE operation_sequence_recovery SET state='completed',decision=?2,divergence_start=NULL,divergence_end=NULL,updated_at=?3 WHERE peer_origin_node_id=?1",
            params![peer_origin_node_id, decision, now()],
        )?;
        let projection = load_sequence_recovery(&transaction, peer_origin_node_id)?;
        transaction.commit()?;
        Ok(projection)
    }

    pub fn sequence_recovery_state(&self) -> Result<Vec<SequenceRecoveryProjection>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT peer_origin_node_id FROM operation_sequence_recovery ORDER BY peer_origin_node_id",
        )?;
        let peers = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        peers
            .into_iter()
            .map(|peer| load_sequence_recovery(&connection, &peer))
            .collect()
    }

    pub fn list_operations(&self, project_id: &str) -> Result<Vec<ProjectOperation>> {
        self.require_project(project_id)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT operation_id, project_id, origin_node_id, origin_sequence, hlc_wall_millis, hlc_counter, aggregate_type, aggregate_id, operation_type, payload_version, base_revision, payload_json FROM project_operations WHERE project_id = ?1 ORDER BY hlc_wall_millis, hlc_counter, origin_node_id, operation_id",
        )?;
        Ok(statement
            .query_map([project_id], operation_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn operation_diagnostics(&self, project_id: &str) -> Result<OperationDiagnostics> {
        self.require_project(project_id)?;
        let connection = self.connection()?;
        if !crate::table_exists(&connection, "project_operations")? {
            return Ok(OperationDiagnostics {
                project_id: project_id.to_string(),
                operation_count: 0,
                latest_local_sequence: 0,
                unresolved_conflicts: Vec::new(),
            });
        }
        let operation_count = connection.query_row(
            "SELECT COUNT(*) FROM project_operations WHERE project_id = ?1",
            [project_id],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let node = connection
            .query_row(
                "SELECT node_id FROM catalog_identity WHERE singleton_id = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let latest_local_sequence = match node {
            Some(node) => connection.query_row(
                "SELECT COALESCE(last_sequence, 0) FROM operation_origin_sequences WHERE origin_node_id = ?1",
                [node],
                |row| row.get::<_, i64>(0),
            ).optional()?.unwrap_or(0) as u64,
            None => 0,
        };
        let mut statement = connection.prepare(
            "SELECT conflict_id, project_id, aggregate_type, aggregate_id, incoming_operation_id, active_operation_id, expected_base_revision, actual_revision, conflict_type, created_at FROM operation_conflicts WHERE project_id = ?1 AND resolved_at IS NULL ORDER BY created_at, conflict_id",
        )?;
        let unresolved_conflicts = statement
            .query_map([project_id], |row| {
                Ok(ConflictProjection {
                    conflict_id: row.get(0)?,
                    project_id: row.get(1)?,
                    aggregate_type: row.get(2)?,
                    aggregate_id: row.get(3)?,
                    incoming_operation_id: row.get(4)?,
                    active_operation_id: row.get(5)?,
                    expected_base_revision: row.get::<_, Option<i64>>(6)?.map(|value| value as u64),
                    actual_revision: row.get::<_, i64>(7)? as u64,
                    conflict_type: row.get(8)?,
                    created_at: row.get(9)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(OperationDiagnostics {
            project_id: project_id.to_string(),
            operation_count,
            latest_local_sequence,
            unresolved_conflicts,
        })
    }

    pub fn asset_revision_diagnostics(&self, asset_id: &str) -> Result<AssetRevisionDiagnostics> {
        let asset = self.get_asset(asset_id)?;
        let connection = self.connection()?;
        if !crate::table_exists(&connection, "note_revisions")? {
            return Ok(AssetRevisionDiagnostics {
                asset_id: asset_id.to_string(),
                materialized_revision: 0,
                note_revisions: vec![NoteRevisionProjection {
                    revision_id: format!("baseline_note_{asset_id}"),
                    operation_id: None,
                    text: asset.notes,
                    logical_time: HybridLogicalTime {
                        wall_millis: 0,
                        counter: 0,
                        node_id: "baseline".to_string(),
                    },
                    active: true,
                    created_at: String::new(),
                }],
            });
        }
        let materialized_revision =
            current_revision(&connection, &asset.project_id, "projectAsset", asset_id)?;
        let active_id = connection.query_row(
            "SELECT revision_id FROM note_revisions WHERE asset_id=?1 ORDER BY hlc_wall_millis DESC, hlc_counter DESC, origin_node_id DESC, revision_id DESC LIMIT 1",
            [asset_id], |row| row.get::<_, String>(0),
        ).optional()?;
        let mut statement = connection.prepare(
            "SELECT revision_id, operation_id, text, hlc_wall_millis, hlc_counter, origin_node_id, created_at FROM note_revisions WHERE asset_id=?1 ORDER BY hlc_wall_millis DESC, hlc_counter DESC, origin_node_id DESC, revision_id DESC",
        )?;
        let note_revisions = statement
            .query_map([asset_id], |row| {
                let revision_id: String = row.get(0)?;
                let node_id: String = row.get(5)?;
                Ok(NoteRevisionProjection {
                    active: active_id.as_deref() == Some(&revision_id),
                    revision_id,
                    operation_id: row.get(1)?,
                    text: row.get(2)?,
                    logical_time: HybridLogicalTime {
                        wall_millis: row.get(3)?,
                        counter: row.get::<_, i64>(4)? as u64,
                        node_id,
                    },
                    created_at: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(AssetRevisionDiagnostics {
            asset_id: asset_id.to_string(),
            materialized_revision,
            note_revisions,
        })
    }

    pub fn count_project_assets(&self, project_id: &str) -> Result<u64> {
        let connection = self.connection()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM assets WHERE project_id = ?1 AND COALESCE(membership_removed, 0) = 0",
            [project_id],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Resolves project membership from the replica itself. Callers must use
    /// this instead of a control-catalog projection when deduplicating an
    /// add-to-project command because that projection can legitimately lag.
    pub fn project_asset_for_media_object(
        &self,
        project_id: &str,
        media_object_id: &str,
    ) -> Result<Option<Asset>> {
        let connection = self.connection()?;
        let asset_id = connection
            .query_row(
                "SELECT id FROM assets WHERE project_id = ?1 AND media_object_id = ?2 AND COALESCE(membership_removed, 0) = 0 ORDER BY id LIMIT 1",
                params![project_id, media_object_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        drop(connection);
        asset_id.map(|id| self.get_asset(&id)).transpose()
    }

    /// Pages one project's assets by the stable `project_asset_id` order.
    /// Search is a case-folded substring match over this project's own
    /// filename, camera, active note, and labels. A replica is per-project and
    /// bounded, so a substring match is acceptable here in a way it is not for
    /// the global library.
    pub fn list_project_asset_page(
        &self,
        project_id: &str,
        limit: u32,
        after: Option<&str>,
        search: Option<&str>,
    ) -> Result<ProjectAssetPage> {
        let connection = self.connection()?;
        let folded = search.map(|value| format!("%{}%", value.to_lowercase()));
        let mut statement = connection.prepare(
            r#"
            SELECT id, filename, camera, media_object_id, media_verification_state,
                   removed, rotation_degrees, labels_json, notes, duration_seconds, best_time
            FROM assets
            WHERE project_id = ?1
              AND (?2 IS NULL OR id > ?2)
              AND (?3 IS NULL
                   OR LOWER(filename) LIKE ?3
                   OR LOWER(camera) LIKE ?3
                   OR LOWER(notes) LIKE ?3
                   OR LOWER(labels_json) LIKE ?3)
            ORDER BY id ASC
            LIMIT ?4
            "#,
        )?;
        let mut items = statement
            .query_map(
                rusqlite::params![project_id, after, folded, i64::from(limit + 1)],
                |row| {
                    let labels_json: String = row.get(7)?;
                    let notes: String = row.get(8)?;
                    Ok(ProjectAssetItem {
                        project_asset_id: row.get(0)?,
                        filename: row.get(1)?,
                        camera: row.get(2)?,
                        media_object_id: row.get(3)?,
                        media_verification_state: row.get(4)?,
                        removed: row.get::<_, i64>(5)? != 0,
                        rotation_degrees: row.get(6)?,
                        labels: serde_json::from_str(&labels_json).unwrap_or_default(),
                        active_note: (!notes.is_empty()).then_some(notes),
                        duration_seconds: row.get(9)?,
                        best_time: row.get(10)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let has_more = items.len() > limit as usize;
        items.truncate(limit as usize);
        let next_cursor = has_more.then(|| {
            items
                .last()
                .expect("a page with more rows has a last item")
                .project_asset_id
                .clone()
        });
        Ok(ProjectAssetPage { items, next_cursor })
    }

    pub(crate) fn commit_local(
        &self,
        project_id: &str,
        operations: Vec<NewOperation>,
    ) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        commit_local_in_transaction(&transaction, project_id, operations)?;
        transaction.commit()?;
        Ok(())
    }
}

fn label_add_operation(asset_id: &str, label: String) -> Result<NewOperation> {
    Ok(NewOperation {
        aggregate_type: "projectAsset",
        aggregate_id: asset_id.to_string(),
        operation_type: LABEL_ADDED,
        base_revision: None,
        payload: serde_json::to_value(LabelAddPayload {
            add_tag: format!("label_{}", Uuid::new_v4().simple()),
            label,
        })?,
    })
}

fn commit_local_in_transaction(
    transaction: &Transaction<'_>,
    project_id: &str,
    operations: Vec<NewOperation>,
) -> Result<()> {
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
        [project_id],
        |row| row.get(0),
    )?;
    if !exists {
        bail!("project not found: {project_id}");
    }
    let node_id = ensure_node(transaction)?;
    transaction.execute(
        "UPDATE projects SET origin_node_id = COALESCE(origin_node_id, ?2) WHERE id = ?1",
        params![project_id, node_id],
    )?;
    for new in operations {
        if new.aggregate_type == "projectAsset" && new.operation_type != PROJECT_ASSET_ADDED {
            reconcile_asset_baseline(transaction, project_id, &new.aggregate_id)?;
        }
        let origin_sequence = next_sequence(transaction, &node_id)?;
        let logical_time = next_hlc(transaction, &node_id)?;
        let operation = ProjectOperation {
            operation_id: format!(
                "op_{:013}_{}",
                logical_time.wall_millis,
                Uuid::new_v4().simple()
            ),
            project_id: project_id.to_string(),
            origin_node_id: node_id.clone(),
            origin_sequence,
            logical_time,
            aggregate_type: new.aggregate_type.to_string(),
            aggregate_id: new.aggregate_id,
            operation_type: new.operation_type.to_string(),
            payload_version: 1,
            base_revision: new.base_revision,
            payload: new.payload,
        };
        let result = insert_and_materialize(transaction, &operation)?;
        if result == ApplyOperationResult::Conflict {
            bail!("locally constructed operation unexpectedly conflicted");
        }
    }
    Ok(())
}

pub(crate) fn reconcile_asset_baseline(
    transaction: &Transaction<'_>,
    project_id: &str,
    asset_id: &str,
) -> Result<()> {
    let current = transaction
        .query_row(
            "SELECT notes, labels_json, removed, rotation_degrees, created_at FROM assets WHERE id=?1 AND project_id=?2",
            params![asset_id, project_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, bool>(2)?, row.get::<_, i64>(3)?, row.get::<_, String>(4)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow!("asset not found: {asset_id}"))?;
    transaction.execute(
        "INSERT INTO materialized_revisions(project_id, aggregate_type, aggregate_id, revision, last_operation_id) VALUES (?1, 'projectAsset', ?2, 0, NULL) ON CONFLICT DO NOTHING",
        params![project_id, asset_id],
    )?;
    transaction.execute(
        "INSERT INTO note_revisions(revision_id, project_id, asset_id, operation_id, text, hlc_wall_millis, hlc_counter, origin_node_id, created_at) VALUES (?1, ?2, ?3, NULL, ?4, 0, 0, 'baseline', ?5) ON CONFLICT DO NOTHING",
        params![format!("baseline_note_{asset_id}"), project_id, asset_id, current.0, current.4],
    )?;
    for (field, value) in [
        (
            "removed",
            if current.2 {
                "true".to_string()
            } else {
                "false".to_string()
            },
        ),
        ("rotationDegrees", current.3.to_string()),
        ("membershipPresent", "true".to_string()),
    ] {
        transaction.execute(
            "INSERT INTO lww_registers(project_id, aggregate_type, aggregate_id, field_name, value_json, operation_id, hlc_wall_millis, hlc_counter, origin_node_id) VALUES (?1, 'projectAsset', ?2, ?3, ?4, NULL, 0, 0, 'baseline') ON CONFLICT DO NOTHING",
            params![project_id, asset_id, field, value],
        )?;
    }
    let tag_count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM label_tags WHERE asset_id=?1",
        [asset_id],
        |row| row.get(0),
    )?;
    if tag_count == 0 {
        for (ordinal, label) in serde_json::from_str::<Vec<String>>(&current.1)
            .unwrap_or_default()
            .into_iter()
            .enumerate()
        {
            transaction.execute(
                "INSERT INTO label_tags(add_tag, project_id, asset_id, normalized_label, display_label, operation_id, hlc_wall_millis, hlc_counter, origin_node_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, NULL, 0, ?6, 'baseline', ?7) ON CONFLICT DO NOTHING",
                params![format!("baseline_label_{asset_id}_{ordinal}"), project_id, asset_id, label_key(&label), label, ordinal as i64, current.4],
            )?;
        }
    }
    Ok(())
}

pub(crate) fn ensure_node(transaction: &Transaction<'_>) -> Result<String> {
    if let Some(node) = transaction
        .query_row(
            "SELECT node_id FROM catalog_identity WHERE singleton_id = 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok(node);
    }
    let node = format!("node_desktop_{}", Uuid::new_v4().simple());
    transaction.execute(
        "INSERT INTO catalog_identity(singleton_id, node_id, created_at) VALUES (1, ?1, ?2)",
        params![node, now()],
    )?;
    backfill_local_locations(transaction, &node)?;
    Ok(node)
}

fn backfill_local_locations(transaction: &Transaction<'_>, node_id: &str) -> Result<()> {
    transaction.execute(
        "INSERT OR IGNORE INTO project_asset_local_locations(asset_id, node_id, local_path, created_at) SELECT id, ?1, path, created_at FROM assets WHERE path != ''",
        [node_id],
    )?;
    Ok(())
}

fn load_sequence_recovery_optional(
    connection: &rusqlite::Connection,
    peer_origin_node_id: &str,
) -> Result<Option<SequenceRecoveryProjection>> {
    connection
        .query_row(
            r#"
            SELECT peer_origin_node_id,server_origin_node_id,manifest_watermark,
                   contained_watermark,reported_watermark,state,divergence_start,
                   divergence_end,decision,updated_at
            FROM operation_sequence_recovery WHERE peer_origin_node_id=?1
            "#,
            [peer_origin_node_id],
            |row| {
                let state = match row.get::<_, String>(5)?.as_str() {
                    "awaiting" => SequenceRecoveryState::Awaiting,
                    "divergent" => SequenceRecoveryState::Divergent,
                    "completed" => SequenceRecoveryState::Completed,
                    _ => return Err(rusqlite::Error::InvalidQuery),
                };
                Ok(SequenceRecoveryProjection {
                    peer_origin_node_id: row.get(0)?,
                    server_origin_node_id: row.get(1)?,
                    manifest_watermark: row.get::<_, i64>(2)? as u64,
                    contained_watermark: row.get::<_, i64>(3)? as u64,
                    reported_watermark: row.get::<_, Option<i64>>(4)?.map(|value| value as u64),
                    state,
                    divergence_start: row.get::<_, Option<i64>>(6)?.map(|value| value as u64),
                    divergence_end: row.get::<_, Option<i64>>(7)?.map(|value| value as u64),
                    decision: row.get(8)?,
                    updated_at: row.get(9)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn load_sequence_recovery(
    connection: &rusqlite::Connection,
    peer_origin_node_id: &str,
) -> Result<SequenceRecoveryProjection> {
    load_sequence_recovery_optional(connection, peer_origin_node_id)?
        .ok_or_else(|| anyhow!("sequence recovery state was not found"))
}

fn next_sequence(transaction: &Transaction<'_>, node_id: &str) -> Result<u64> {
    let blocked: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM operation_sequence_recovery WHERE server_origin_node_id=?1 AND state IN ('awaiting','divergent'))",
        [node_id],
        |row| row.get(0),
    )?;
    if blocked {
        bail!("server-origin operation allocation is blocked by sequence recovery");
    }
    transaction.execute(
        "INSERT INTO operation_origin_sequences(origin_node_id, last_sequence) VALUES (?1, 0) ON CONFLICT(origin_node_id) DO NOTHING",
        [node_id],
    )?;
    transaction.execute(
        "UPDATE operation_origin_sequences SET last_sequence = last_sequence + 1 WHERE origin_node_id = ?1",
        [node_id],
    )?;
    Ok(transaction.query_row(
        "SELECT last_sequence FROM operation_origin_sequences WHERE origin_node_id = ?1",
        [node_id],
        |row| row.get::<_, i64>(0),
    )? as u64)
}

fn next_hlc(transaction: &Transaction<'_>, node_id: &str) -> Result<HybridLogicalTime> {
    let wall_now = Utc::now().timestamp_millis().max(0);
    let previous = transaction
        .query_row(
            "SELECT wall_millis, counter FROM operation_hlc_state WHERE node_id = ?1",
            [node_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    let (wall_millis, counter) = match previous {
        Some((wall, counter)) if wall >= wall_now => (wall, counter as u64 + 1),
        _ => (wall_now, 0),
    };
    transaction.execute(
        "INSERT INTO operation_hlc_state(node_id, wall_millis, counter) VALUES (?1, ?2, ?3) ON CONFLICT(node_id) DO UPDATE SET wall_millis=excluded.wall_millis, counter=excluded.counter",
        params![node_id, wall_millis, counter as i64],
    )?;
    Ok(HybridLogicalTime {
        wall_millis,
        counter,
        node_id: node_id.to_string(),
    })
}

fn receive_hlc(transaction: &Transaction<'_>, operation: &ProjectOperation) -> Result<()> {
    let local = ensure_node(transaction)?;
    let wall_now = Utc::now().timestamp_millis().max(0);
    if operation.logical_time.wall_millis
        > wall_now.saturating_add(MAX_REMOTE_HLC_FUTURE_SKEW_MILLIS)
    {
        bail!("incoming operation wall clock exceeds the allowed future skew");
    }
    let (last_wall, last_counter) = transaction
        .query_row(
            "SELECT wall_millis, counter FROM operation_hlc_state WHERE node_id = ?1",
            [&local],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?
        .unwrap_or((0, 0));
    let wall = wall_now
        .max(last_wall)
        .max(operation.logical_time.wall_millis);
    let counter = if wall == last_wall && wall == operation.logical_time.wall_millis {
        last_counter.max(operation.logical_time.counter as i64) + 1
    } else if wall == last_wall {
        last_counter + 1
    } else if wall == operation.logical_time.wall_millis {
        operation.logical_time.counter as i64 + 1
    } else {
        0
    };
    transaction.execute(
        "INSERT INTO operation_hlc_state(node_id, wall_millis, counter) VALUES (?1, ?2, ?3) ON CONFLICT(node_id) DO UPDATE SET wall_millis=excluded.wall_millis, counter=excluded.counter",
        params![local, wall, counter],
    )?;
    Ok(())
}

fn insert_and_materialize(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    validate_operation(operation)?;
    if operation_exists(transaction, &operation.operation_id)? {
        return Ok(ApplyOperationResult::Duplicate);
    }
    if is_known_operation_type(&operation.operation_type)
        && operation.aggregate_type == "projectAsset"
        && operation.operation_type != PROJECT_ASSET_ADDED
    {
        reconcile_asset_baseline(transaction, &operation.project_id, &operation.aggregate_id)?;
    }
    transaction.execute(
        "INSERT INTO project_operations(operation_id, project_id, origin_node_id, origin_sequence, hlc_wall_millis, hlc_counter, aggregate_type, aggregate_id, operation_type, payload_version, base_revision, payload_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![operation.operation_id, operation.project_id, operation.origin_node_id, operation.origin_sequence as i64,
            operation.logical_time.wall_millis, operation.logical_time.counter as i64, operation.aggregate_type,
            operation.aggregate_id, operation.operation_type, operation.payload_version, operation.base_revision.map(|v| v as i64),
            serde_json::to_string(&operation.payload)?, now()],
    )?;

    materialize_operation(transaction, operation)
}

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
        PROJECT_ASSET_MEMBERSHIP_SET => materialize_membership_set(transaction, operation)?,
        PROJECT_ASSET_ADDED => materialize_asset_add(transaction, operation)?,
        PROJECT_ASSET_MEDIA_LINKED => materialize_media_link(transaction, operation)?,
        CLOCK_EVENT_CREATED => return materialize_clock(transaction, operation),
        CAPTURE_SEQUENCE_DECLARED => {
            return materialize_capture_sequence_declared(transaction, operation);
        }
        CAPTURE_SEQUENCE_SPLIT => {
            return materialize_capture_sequence_split(transaction, operation);
        }
        CAPTURE_SEQUENCE_MERGED => {
            return materialize_capture_sequence_merged(transaction, operation);
        }
        CLOCK_EPOCH_SET => return materialize_clock_epoch_set(transaction, operation),
        CLOCK_EPOCH_CLEARED => return materialize_clock_epoch_cleared(transaction, operation),
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

fn materialize_note(transaction: &Transaction<'_>, operation: &ProjectOperation) -> Result<bool> {
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

fn materialize_label_add(
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

fn materialize_label_remove(
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

fn materialize_register(
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

fn materialize_asset_add(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<bool> {
    let payload: ProjectAssetAddedPayload = payload(operation)?;
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
    transaction.execute(
        "INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, metadata_rotation_degrees, default_rotation_degrees, rotation_degrees, orientation_probed, created_at) VALUES (?1, ?2, ?3, '', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15, 1, ?16)",
        params![payload.id, operation.project_id, payload.fingerprint, payload.filename, payload.size_bytes as i64,
            payload.duration_seconds, payload.frame_rate, payload.width, payload.height, payload.codec, payload.camera,
            payload.captured_at_raw, payload.best_time, payload.metadata_rotation_degrees, payload.default_rotation_degrees, payload.created_at],
    )?;
    // Every asset belongs to exactly one capture sequence. A new asset joins
    // its camera's sequence (created on demand), using the same rule migration
    // 9 applies to existing assets. This keeps the chronology fact model
    // closed from migration 9 onward.
    crate::chronology::ensure_capture_sequence_for_camera(
        transaction,
        &operation.project_id,
        &payload.camera,
        &payload.created_at,
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO asset_capture_sequence(asset_id, project_id, capture_sequence_id, filename_counter) VALUES (?1, ?2, ?3, ?4)",
        params![
            operation.aggregate_id,
            operation.project_id,
            crate::chronology::capture_sequence_id_for(&operation.project_id, &payload.camera),
            crate::chronology::parse_filename_counter(&payload.filename).map(|value| value as i64),
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
    Ok(true)
}

fn materialize_membership_set(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<bool> {
    if operation.aggregate_type != "projectAsset" {
        bail!("projectAssetMembershipSet requires a projectAsset aggregate");
    }
    materialize_register(transaction, operation, "membershipPresent")
}

fn materialize_media_link(
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

fn materialize_clock(
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
    let created_at = now();
    transaction.execute("INSERT INTO clock_events(id, project_id, camera, start_filename, offset_seconds, label, created_at, operation_id, hlc_wall_millis, hlc_counter, origin_node_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![payload.event_id, operation.project_id, payload.camera, payload.start_filename, payload.offset_seconds, payload.label, created_at, operation.operation_id,
            operation.logical_time.wall_millis, operation.logical_time.counter as i64, operation.origin_node_id])?;
    crate::chronology::materialize_legacy_clock_epoch(
        transaction,
        &operation.project_id,
        &crate::chronology::LegacyClockEpoch {
            event_id: &payload.event_id,
            camera: &payload.camera,
            start_filename: &payload.start_filename,
            offset_seconds: payload.offset_seconds,
            label: &payload.label,
            created_at: &created_at,
        },
    )?;
    recalculate_clock(transaction, &operation.project_id, &operation.aggregate_id)?;
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

/// The revision-guard comparison for one operation: `None` when the operation
/// declares no base revision (the guard passes unconditionally), otherwise
/// `Some((expected, actual))` from the aggregate's current revision.
fn guard_comparison(
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
fn revision_guard_passes(
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

fn materialize_capture_sequence_declared(
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
    transaction.execute(
        "INSERT INTO capture_sequences(id, project_id, camera_key, label, volume_hint, filename_prefix, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(id) DO UPDATE SET camera_key=excluded.camera_key, label=excluded.label, volume_hint=excluded.volume_hint, filename_prefix=excluded.filename_prefix",
        params![
            payload.sequence_id,
            operation.project_id,
            payload.camera_key,
            payload.label,
            payload.volume_hint,
            payload.filename_prefix,
            now(),
        ],
    )?;
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

fn materialize_capture_sequence_split(
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
    transaction.execute(
        "INSERT OR IGNORE INTO capture_sequences(id, project_id, camera_key, label, volume_hint, filename_prefix, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            payload.new_sequence_id,
            operation.project_id,
            payload.camera_key,
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

fn materialize_capture_sequence_merged(
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
    transaction.execute(
        "UPDATE asset_capture_sequence SET capture_sequence_id=?1 WHERE project_id=?2 AND capture_sequence_id=?3",
        params![payload.into_sequence_id, operation.project_id, payload.from_sequence_id],
    )?;
    // Epochs ride their capture sequence; merging unions the asset sets, so
    // the consumed sequence's epochs follow its assets into the destination.
    transaction.execute(
        "UPDATE clock_epochs SET capture_sequence_id=?1 WHERE project_id=?2 AND capture_sequence_id=?3",
        params![payload.into_sequence_id, operation.project_id, payload.from_sequence_id],
    )?;
    transaction.execute(
        "DELETE FROM capture_sequences WHERE id=?1 AND project_id=?2",
        params![payload.from_sequence_id, operation.project_id],
    )?;
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

fn materialize_clock_epoch_set(
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

fn materialize_clock_epoch_cleared(
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

fn materialize_clip_time_exception_set(
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

fn materialize_clip_time_exception_cleared(
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

fn materialize_trip_phase_set(
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

fn materialize_trip_phase_removed(
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

fn materialize_event_order_constraint_set(
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct EditPlanRevisionCreatedPayload {
    pub(crate) revision_id: String,
    pub(crate) script_revision_id: Option<String>,
    pub(crate) parent_revision_id: Option<String>,
    pub(crate) operations: Vec<crate::editplan::EditPlanOperation>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct EditPlanRevisionActivatedPayload {
    pub(crate) revision_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct EditPlanExportRecordedPayload {
    pub(crate) export_id: String,
    pub(crate) revision_id: String,
    pub(crate) output_path: String,
    pub(crate) output_digest: String,
    pub(crate) exported_at: String,
}

/// The edit plan rides the `editPlan` aggregate with the project id as the
/// aggregate id, because activation is a project-level fact and the active
/// revision is what actually conflicts.
fn require_edit_plan_aggregate(operation: &ProjectOperation) -> Result<()> {
    if operation.aggregate_type != "editPlan" || operation.aggregate_id != operation.project_id {
        bail!("edit plan operations must ride the editPlan aggregate keyed by project id");
    }
    Ok(())
}

fn materialize_edit_plan_revision_created(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: EditPlanRevisionCreatedPayload = payload(operation)?;
    require_edit_plan_aggregate(operation)?;
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    // `INSERT OR IGNORE` throughout, so applying the same operation id twice
    // materializes exactly once.
    crate::editplan::insert_revision(
        transaction,
        &operation.project_id,
        &payload.revision_id,
        payload.script_revision_id.as_deref(),
        payload.parent_revision_id.as_deref(),
        &now(),
        &payload.operations,
    )?;
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

fn materialize_edit_plan_revision_activated(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: EditPlanRevisionActivatedPayload = payload(operation)?;
    require_edit_plan_aggregate(operation)?;
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    crate::editplan::activate_revision(transaction, &payload.revision_id)?;
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

fn materialize_edit_plan_export_recorded(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    let payload: EditPlanExportRecordedPayload = payload(operation)?;
    require_edit_plan_aggregate(operation)?;
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    // Append-only: a new export never removes a prior one.
    transaction.execute(
        "INSERT OR IGNORE INTO edit_plan_exports(export_id, project_id, revision_id, output_path, output_digest, exported_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            payload.export_id,
            operation.project_id,
            payload.revision_id,
            payload.output_path,
            payload.output_digest,
            payload.exported_at,
        ],
    )?;
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

fn materialize_event_order_constraint_cleared(
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

fn recalculate_clock(transaction: &Transaction<'_>, project_id: &str, camera: &str) -> Result<()> {
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

fn refresh_labels(transaction: &Transaction<'_>, asset_id: &str) -> Result<()> {
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

fn active_label_tags(
    transaction: &Transaction<'_>,
    asset_id: &str,
    normalized: &str,
) -> Result<Vec<String>> {
    let mut statement = transaction.prepare("SELECT add_tag FROM label_tags t WHERE asset_id=?1 AND normalized_label=?2 AND NOT EXISTS(SELECT 1 FROM label_tag_removals r WHERE r.add_tag=t.add_tag AND r.normalized_label=t.normalized_label) ORDER BY add_tag")?;
    Ok(statement
        .query_map(params![asset_id, normalized], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn advance_revision(transaction: &Transaction<'_>, operation: &ProjectOperation) -> Result<()> {
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

fn require_asset(transaction: &Transaction<'_>, operation: &ProjectOperation) -> Result<()> {
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

fn payload<T: DeserializeOwned>(operation: &ProjectOperation) -> Result<T> {
    if operation.payload_version != 1 {
        bail!(
            "unsupported payload version {} for {}",
            operation.payload_version,
            operation.operation_type
        );
    }
    Ok(serde_json::from_value(operation.payload.clone())?)
}

fn validate_operation(operation: &ProjectOperation) -> Result<()> {
    validate_id("operation ID", &operation.operation_id)?;
    validate_id("project ID", &operation.project_id)?;
    validate_id("origin node ID", &operation.origin_node_id)?;
    if operation.origin_sequence == 0 {
        bail!("origin sequence must be greater than zero");
    }
    if operation.origin_sequence > i64::MAX as u64
        || operation.logical_time.counter > i64::MAX as u64
        || operation
            .base_revision
            .is_some_and(|value| value > i64::MAX as u64)
    {
        bail!("operation sequence, counter, or revision exceeds SQLite integer range");
    }
    if operation.logical_time.wall_millis < 0 {
        bail!("logical wall time cannot be negative");
    }
    if operation.logical_time.node_id != operation.origin_node_id {
        bail!("logical time node ID must match origin node ID");
    }
    if operation.payload_version != 1 && is_known_operation_type(&operation.operation_type) {
        bail!(
            "unsupported payload version {} for {}",
            operation.payload_version,
            operation.operation_type
        );
    }
    Ok(())
}

fn validate_id(kind: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 255
        || value.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        bail!("invalid {kind}");
    }
    Ok(())
}

fn operation_exists(transaction: &Transaction<'_>, operation_id: &str) -> Result<bool> {
    Ok(transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM project_operations WHERE operation_id=?1)",
        [operation_id],
        |row| row.get(0),
    )?)
}

fn operation_matches_acknowledged_sequence(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<bool> {
    Ok(transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM project_operations WHERE operation_id=?1 AND origin_node_id=?2 AND origin_sequence=?3)",
        params![
            operation.operation_id,
            operation.origin_node_id,
            operation.origin_sequence as i64
        ],
        |row| row.get(0),
    )?)
}

fn operation_key(operation: &ProjectOperation) -> (i64, i64, String, String) {
    (
        operation.logical_time.wall_millis,
        operation.logical_time.counter as i64,
        operation.origin_node_id.clone(),
        operation.operation_id.clone(),
    )
}

pub(crate) fn operation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectOperation> {
    let origin_node_id: String = row.get(2)?;
    Ok(ProjectOperation {
        operation_id: row.get(0)?,
        project_id: row.get(1)?,
        origin_node_id: origin_node_id.clone(),
        origin_sequence: row.get::<_, i64>(3)? as u64,
        logical_time: HybridLogicalTime {
            wall_millis: row.get(4)?,
            counter: row.get::<_, i64>(5)? as u64,
            node_id: origin_node_id,
        },
        aggregate_type: row.get(6)?,
        aggregate_id: row.get(7)?,
        operation_type: row.get(8)?,
        payload_version: row.get(9)?,
        base_revision: row.get::<_, Option<i64>>(10)?.map(|value| value as u64),
        payload: serde_json::from_str(&row.get::<_, String>(11)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                11,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
    })
}

fn label_key(label: &str) -> String {
    label.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;
    use tempfile::{TempDir, tempdir};

    fn fixture() -> (TempDir, Catalog) {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let connection = catalog.connection().unwrap();
        connection.execute("INSERT INTO projects(id, name, created_at) VALUES ('project_test', 'Test', '2026-01-01T00:00:00Z')", []).unwrap();
        connection.execute(
            "INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, notes, labels_json, removed, rotation_degrees, created_at) VALUES ('asset_test', 'project_test', 'fingerprint', '/offline/clip.mp4', 'clip.mp4', 100, 10, 30, 1920, 1080, 'h264', 'DJI', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00.000Z', 'baseline', '[\"arrival\"]', 0, 0, '2026-01-01T00:00:00Z')",
            [],
        ).unwrap();
        (directory, catalog)
    }

    fn operation(
        id: &str,
        node: &str,
        sequence: u64,
        wall: i64,
        operation_type: &str,
        payload: Value,
    ) -> ProjectOperation {
        ProjectOperation {
            operation_id: id.to_string(),
            project_id: "project_test".to_string(),
            origin_node_id: node.to_string(),
            origin_sequence: sequence,
            logical_time: HybridLogicalTime {
                wall_millis: wall,
                counter: 0,
                node_id: node.to_string(),
            },
            aggregate_type: "projectAsset".to_string(),
            aggregate_id: "asset_test".to_string(),
            operation_type: operation_type.to_string(),
            payload_version: 1,
            base_revision: None,
            payload,
        }
    }

    #[test]
    fn unchanged_patches_allocate_no_identity_sequence_or_operation() {
        let (_directory, catalog) = fixture();
        let asset = catalog.get_asset("asset_test").unwrap();
        catalog
            .update_asset(
                "asset_test",
                &AssetPatch {
                    notes: asset.notes,
                    labels: asset.labels,
                    removed: asset.removed,
                    rotation_degrees: asset.rotation_degrees,
                },
            )
            .unwrap();
        assert_eq!(catalog.local_node_id().unwrap(), None);
        assert_eq!(
            catalog
                .operation_diagnostics("project_test")
                .unwrap()
                .operation_count,
            0
        );
    }

    #[test]
    fn compatibility_patch_is_atomic_and_emits_only_explicit_changes() {
        let (_directory, catalog) = fixture();
        let updated = catalog
            .update_asset(
                "asset_test",
                &AssetPatch {
                    notes: "new note".to_string(),
                    labels: vec!["market".to_string()],
                    removed: true,
                    rotation_degrees: 90,
                },
            )
            .unwrap();
        assert_eq!(updated.notes, "new note");
        assert_eq!(updated.labels, ["market"]);
        assert!(updated.removed);
        assert_eq!(updated.rotation_degrees, 90);
        assert_eq!(
            catalog
                .operation_diagnostics("project_test")
                .unwrap()
                .operation_count,
            5
        );

        let error = catalog
            .update_asset(
                "asset_test",
                &AssetPatch {
                    notes: "must roll back".to_string(),
                    labels: vec!["x".repeat(65)],
                    removed: false,
                    rotation_degrees: 180,
                },
            )
            .unwrap_err();
        assert!(format!("{error:#}").contains("64 characters"));
        let unchanged = catalog.get_asset("asset_test").unwrap();
        assert_eq!(unchanged.notes, "new note");
        assert_eq!(
            catalog
                .operation_diagnostics("project_test")
                .unwrap()
                .operation_count,
            5
        );
    }

    #[test]
    fn note_and_lww_materialization_converge_under_reversed_delivery() {
        let (_left_dir, left) = fixture();
        let (_right_dir, right) = fixture();
        let early_note = operation(
            "op_note_early",
            "node_a",
            1,
            10,
            NOTE_REVISION_CREATED,
            json!({"revisionId":"note_early","text":"early"}),
        );
        let late_note = operation(
            "op_note_late",
            "node_b",
            1,
            20,
            NOTE_REVISION_CREATED,
            json!({"revisionId":"note_late","text":"late"}),
        );
        let early_removed = operation(
            "op_removed_early",
            "node_a",
            2,
            11,
            REMOVED_SET,
            json!({"removed":true}),
        );
        let late_removed = operation(
            "op_removed_late",
            "node_b",
            2,
            21,
            REMOVED_SET,
            json!({"removed":false}),
        );
        for value in [&early_note, &late_note, &early_removed, &late_removed] {
            left.apply_operation(value).unwrap();
        }
        for value in [&late_removed, &early_removed, &late_note, &early_note] {
            right.apply_operation(value).unwrap();
        }
        for catalog in [&left, &right] {
            let asset = catalog.get_asset("asset_test").unwrap();
            assert_eq!(asset.notes, "late");
            assert!(!asset.removed);
        }
        assert_eq!(
            left.apply_operation(&late_note).unwrap(),
            ApplyOperationResult::Duplicate
        );
        assert_eq!(left.list_operations("project_test").unwrap().len(), 4);
    }

    #[test]
    fn labels_are_add_wins_observed_remove_even_when_delivered_out_of_order() {
        let (_directory, catalog) = fixture();
        let remove = operation(
            "op_remove",
            "node_a",
            2,
            20,
            LABEL_REMOVED,
            json!({"normalizedLabel":"market","observedAddTags":["tag_observed"]}),
        );
        let observed_add = operation(
            "op_add_observed",
            "node_a",
            1,
            10,
            LABEL_ADDED,
            json!({"addTag":"tag_observed","label":"Market"}),
        );
        let concurrent_add = operation(
            "op_add_concurrent",
            "node_b",
            1,
            15,
            LABEL_ADDED,
            json!({"addTag":"tag_concurrent","label":"MARKET"}),
        );
        catalog.apply_operation(&remove).unwrap();
        catalog.apply_operation(&observed_add).unwrap();
        catalog.apply_operation(&concurrent_add).unwrap();
        assert_eq!(
            catalog.get_asset("asset_test").unwrap().labels,
            ["arrival", "MARKET"]
        );
    }

    #[test]
    fn stale_clock_base_creates_a_conflict_without_replacing_chronology() {
        let (_directory, catalog) = fixture();
        let mut first = operation(
            "op_clock_first",
            "node_a",
            1,
            10,
            CLOCK_EVENT_CREATED,
            json!({"eventId":"clock_first","camera":"DJI","startFilename":"clip.mp4","offsetSeconds":60,"label":"first"}),
        );
        first.aggregate_type = "clock".to_string();
        first.aggregate_id = "DJI".to_string();
        first.base_revision = Some(0);
        let mut stale = operation(
            "op_clock_stale",
            "node_b",
            1,
            20,
            CLOCK_EVENT_CREATED,
            json!({"eventId":"clock_stale","camera":"DJI","startFilename":"clip.mp4","offsetSeconds":120,"label":"stale"}),
        );
        stale.aggregate_type = "clock".to_string();
        stale.aggregate_id = "DJI".to_string();
        stale.base_revision = Some(0);
        assert_eq!(
            catalog.apply_operation(&first).unwrap(),
            ApplyOperationResult::Applied
        );
        assert_eq!(
            catalog.apply_operation(&stale).unwrap(),
            ApplyOperationResult::Conflict
        );
        assert_eq!(catalog.get_asset("asset_test").unwrap().offset_seconds, 60);
        assert_eq!(
            catalog
                .operation_diagnostics("project_test")
                .unwrap()
                .unresolved_conflicts
                .len(),
            1
        );
    }

    #[test]
    fn restore_sequence_recovery_blocks_reuse_and_records_divergence_decision() {
        let (_directory, catalog) = fixture();
        catalog.set_local_node_id("node_server").unwrap();
        let marked = catalog
            .mark_sequence_recovery("node_mac", "node_server", 0)
            .unwrap();
        assert_eq!(marked.state, SequenceRecoveryState::Awaiting);
        assert!(
            catalog
                .create_note_revision("asset_test", "blocked")
                .is_err()
        );

        let divergent = catalog
            .reconcile_sequence_recovery("node_mac", 7)
            .unwrap()
            .unwrap();
        assert_eq!(divergent.state, SequenceRecoveryState::Divergent);
        assert_eq!(divergent.divergence_start, Some(1));
        assert_eq!(divergent.divergence_end, Some(7));
        assert!(
            catalog
                .create_note_revision("asset_test", "still blocked")
                .is_err()
        );

        catalog
            .accept_sequence_divergence("node_mac", "accept_allocator_advance")
            .unwrap();
        catalog
            .create_note_revision("asset_test", "after decision")
            .unwrap();
        let operations = catalog.list_operations("project_test").unwrap();
        assert_eq!(operations.last().unwrap().origin_sequence, 8);
    }

    #[test]
    fn restore_sequence_recovery_completes_after_exact_operations_are_replayed() {
        let (_directory, catalog) = fixture();
        catalog.set_local_node_id("node_server").unwrap();
        catalog
            .mark_sequence_recovery("node_mac", "node_server", 0)
            .unwrap();
        let divergent = catalog
            .reconcile_sequence_recovery("node_mac", 2)
            .unwrap()
            .unwrap();
        assert_eq!(divergent.state, SequenceRecoveryState::Divergent);

        let first = operation(
            "op_recovered_1",
            "node_server",
            1,
            10,
            LABEL_ADDED,
            json!({"addTag":"tag_recovered_1","label":"recovered-one"}),
        );
        let second = operation(
            "op_recovered_2",
            "node_server",
            2,
            20,
            LABEL_ADDED,
            json!({"addTag":"tag_recovered_2","label":"recovered-two"}),
        );
        catalog
            .apply_operation_batch("node_server", &[first, second])
            .unwrap();
        let completed = catalog
            .reconcile_sequence_recovery("node_mac", 2)
            .unwrap()
            .unwrap();
        assert_eq!(completed.state, SequenceRecoveryState::Completed);
        assert_eq!(completed.contained_watermark, 2);

        catalog
            .create_note_revision("asset_test", "after exact recovery")
            .unwrap();
        let operations = catalog.list_operations("project_test").unwrap();
        assert_eq!(operations.last().unwrap().origin_sequence, 3);
    }

    #[test]
    fn operation_cursors_include_local_history_and_peer_acknowledgements_are_monotonic() {
        let (_directory, catalog) = fixture();
        catalog.set_local_node_id("node_mac").unwrap();
        catalog
            .create_note_revision("asset_test", "local history")
            .unwrap();
        assert_eq!(catalog.operation_cursors().unwrap()["node_mac"], 1);

        catalog
            .record_peer_operation_acknowledgements(
                "node_server",
                &std::collections::BTreeMap::from([("node_mac".to_string(), 1)]),
            )
            .unwrap();
        catalog
            .record_peer_operation_acknowledgements(
                "node_server",
                &std::collections::BTreeMap::from([("node_mac".to_string(), 0)]),
            )
            .unwrap();
        assert_eq!(
            catalog
                .peer_operation_acknowledgements("node_server")
                .unwrap()["node_mac"],
            1
        );
    }

    #[test]
    fn remote_hlc_beyond_future_skew_is_rejected_without_advancing_state() {
        let (_directory, catalog) = fixture();
        let future = Utc::now().timestamp_millis() + MAX_REMOTE_HLC_FUTURE_SKEW_MILLIS + 10_000;
        let incoming = operation(
            "op_future",
            "node_mac",
            1,
            future,
            REMOVED_SET,
            json!({"removed":true}),
        );
        assert!(
            catalog
                .apply_operation_batch("node_mac", &[incoming])
                .is_err()
        );
        assert!(catalog.operation_cursors().unwrap().is_empty());
        assert!(!catalog.get_asset("asset_test").unwrap().removed);
    }

    proptest! {
        #[test]
        fn lww_rotation_converges_for_shuffled_delivery(order_keys in any::<[u8; 3]>()) {
            let (_directory, catalog) = fixture();
            let operations = [
                operation("op_rotation_1", "node_a", 1, 10, ROTATION_SET, json!({"rotationDegrees":90})),
                operation("op_rotation_2", "node_b", 1, 30, ROTATION_SET, json!({"rotationDegrees":270})),
                operation("op_rotation_3", "node_a", 2, 20, ROTATION_SET, json!({"rotationDegrees":180})),
            ];
            let mut indices = [0_usize, 1, 2];
            indices.sort_by_key(|index| (order_keys[*index], *index));
            for index in indices {
                catalog.apply_operation(&operations[index]).unwrap();
            }
            prop_assert_eq!(catalog.get_asset("asset_test").unwrap().rotation_degrees, 270);
        }
    }
}
