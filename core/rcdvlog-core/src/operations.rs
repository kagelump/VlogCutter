use crate::{
    Asset, AssetPatch, Catalog, ClockEvent, MediaVerificationState, normalize_datetime,
    normalize_labels, now,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::BTreeSet;
use uuid::Uuid;

use crate::editplan::feedback::{
    DECISION as EDIT_REVIEW_DECISION, MAPPING as EDIT_REVIEW_MAPPING, NOTE as EDIT_REVIEW_NOTE,
    PROPOSAL as EDIT_REVIEW_PROPOSAL,
};

mod catalog;
mod materialize;
mod materialize_assets;
mod materialize_cameras;
mod materialize_clock;
mod materialize_edit_plan;
mod materialize_sequences;

pub(crate) use materialize::*;
use materialize_assets::*;
pub(crate) use materialize_cameras::*;
use materialize_clock::*;
pub(crate) use materialize_edit_plan::*;
use materialize_sequences::*;

pub const NOTE_REVISION_CREATED: &str = "noteRevisionCreated";
pub const LABEL_ADDED: &str = "labelAdded";
pub const LABEL_REMOVED: &str = "labelRemoved";
pub const REMOVED_SET: &str = "removedSet";
pub const ROTATION_SET: &str = "rotationSet";
pub const ORIENTATION_DECISION_SET: &str = "orientationDecisionSet";
pub const PROJECT_ASSET_ADDED: &str = "projectAssetAdded";
pub const PROJECT_ASSET_MEMBERSHIP_SET: &str = "projectAssetMembershipSet";
pub const PROJECT_ASSET_MEDIA_LINKED: &str = "projectAssetMediaLinked";
pub const CLOCK_EVENT_CREATED: &str = "clockEventCreated";
pub const CAMERA_IDENTITY_MERGED: &str = "cameraIdentityMerged";
pub const CAMERA_ASSETS_REPAIRED: &str = "cameraAssetsRepaired";
pub const CAPTURE_SEQUENCE_DECLARED: &str = "captureSequenceDeclared";
pub const CAPTURE_SEQUENCE_SPLIT: &str = "captureSequenceSplit";
pub const CAPTURE_SEQUENCE_SPLIT_EXACT: &str = "captureSequenceSplitExact";
pub const CAPTURE_SEQUENCE_MERGED: &str = "captureSequenceMerged";
pub const CAPTURE_SEQUENCE_REASSIGNED: &str = "captureSequenceReassigned";
pub const CAPTURE_SEQUENCE_SPLIT_DECLINED: &str = "captureSequenceSplitDeclined";
pub const CAPTURE_SEQUENCE_SPLIT_RESTORED: &str = "captureSequenceSplitRestored";
pub const CLOCK_EPOCH_SET: &str = "clockEpochSet";
pub const CLOCK_EPOCH_CLEARED: &str = "clockEpochCleared";
pub const CLOCK_QUESTION_DECISION_RECORDED: &str = "clockQuestionDecisionRecorded";
pub const CLOCK_QUESTION_PROPOSED: &str = "clockQuestionProposed";
pub const CLIP_TIME_EXCEPTION_SET: &str = "clipTimeExceptionSet";
pub const CLIP_TIME_EXCEPTION_CLEARED: &str = "clipTimeExceptionCleared";
pub const TRIP_PHASE_SET: &str = "tripPhaseSet";
pub const TRIP_PHASE_REMOVED: &str = "tripPhaseRemoved";
pub const EVENT_ORDER_CONSTRAINT_SET: &str = "eventOrderConstraintSet";
pub const EVENT_ORDER_CONSTRAINT_CLEARED: &str = "eventOrderConstraintCleared";
pub const EDIT_PLAN_REVISION_CREATED: &str = "editPlanRevisionCreated";
pub const EDIT_PLAN_REVISION_ACTIVATED: &str = "editPlanRevisionActivated";
pub const EDIT_PLAN_EXPORT_RECORDED: &str = "editPlanExportRecorded";
pub const SOURCE_RANGE_CONFIRMED: &str = "sourceRangeConfirmed";
pub const SOURCE_RANGE_REVOKED: &str = "sourceRangeRevoked";
pub const MAX_REMOTE_HLC_FUTURE_SKEW_MILLIS: i64 = 5 * 60 * 1000;

/// The chronology operation types known to the re-dispatch seam. The seam
/// on writable catalog open is scoped to exactly these: for the legacy
/// vocabulary a persisted row is never a signal that materialization is
/// missing, so replaying them would rewrite the legacy log.
/// The stable prefix of the error a stale whole-patch write is refused with.
///
/// Every adapter matches on this rather than on prose, because a client's only
/// correct response — re-read, re-decide, retry — depends on telling this
/// refusal apart from a genuine failure.
pub const STALE_ASSET_REVISION: &str = "stale asset revision";

pub(crate) const CHRONOLOGY_OPERATION_TYPES: &[&str] = &[
    CAMERA_IDENTITY_MERGED,
    CAMERA_ASSETS_REPAIRED,
    CAPTURE_SEQUENCE_DECLARED,
    CAPTURE_SEQUENCE_SPLIT,
    CAPTURE_SEQUENCE_SPLIT_EXACT,
    CAPTURE_SEQUENCE_MERGED,
    CAPTURE_SEQUENCE_REASSIGNED,
    CAPTURE_SEQUENCE_SPLIT_DECLINED,
    CAPTURE_SEQUENCE_SPLIT_RESTORED,
    CLOCK_EPOCH_SET,
    CLOCK_EPOCH_CLEARED,
    CLOCK_QUESTION_DECISION_RECORDED,
    CLOCK_QUESTION_PROPOSED,
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
    ORIENTATION_DECISION_SET,
    PROJECT_ASSET_ADDED,
    PROJECT_ASSET_MEMBERSHIP_SET,
    PROJECT_ASSET_MEDIA_LINKED,
    CLOCK_EVENT_CREATED,
    CAMERA_IDENTITY_MERGED,
    CAMERA_ASSETS_REPAIRED,
    CAPTURE_SEQUENCE_DECLARED,
    CAPTURE_SEQUENCE_SPLIT,
    CAPTURE_SEQUENCE_SPLIT_EXACT,
    CAPTURE_SEQUENCE_MERGED,
    CAPTURE_SEQUENCE_REASSIGNED,
    CAPTURE_SEQUENCE_SPLIT_DECLINED,
    CAPTURE_SEQUENCE_SPLIT_RESTORED,
    CLOCK_EPOCH_SET,
    CLOCK_EPOCH_CLEARED,
    CLOCK_QUESTION_DECISION_RECORDED,
    CLOCK_QUESTION_PROPOSED,
    CLIP_TIME_EXCEPTION_SET,
    CLIP_TIME_EXCEPTION_CLEARED,
    TRIP_PHASE_SET,
    TRIP_PHASE_REMOVED,
    EVENT_ORDER_CONSTRAINT_SET,
    EVENT_ORDER_CONSTRAINT_CLEARED,
    EDIT_PLAN_REVISION_CREATED,
    EDIT_PLAN_REVISION_ACTIVATED,
    EDIT_PLAN_EXPORT_RECORDED,
    SOURCE_RANGE_CONFIRMED,
    SOURCE_RANGE_REVOKED,
    EDIT_REVIEW_NOTE,
    EDIT_REVIEW_PROPOSAL,
    EDIT_REVIEW_DECISION,
    EDIT_REVIEW_MAPPING,
];

pub(crate) fn is_known_operation_type(value: &str) -> bool {
    KNOWN_OPERATION_TYPES.contains(&value)
}

/// Whether materializing this operation can change the reproducible
/// `assets.best_time`/`offset_seconds` projection. Synchronization calls this
/// after a successful remote page so queue reads immediately reflect the same
/// chronology the solver does.
pub fn operation_affects_chronology_projection(value: &str) -> bool {
    value != CLOCK_QUESTION_DECISION_RECORDED
        && value != CLOCK_QUESTION_PROPOSED
        && (value == PROJECT_ASSET_ADDED
            || value == CLOCK_EVENT_CREATED
            || CHRONOLOGY_OPERATION_TYPES.contains(&value))
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OrientationDecision {
    /// `confirmed` records an explicit visual judgment; `unresolved` records
    /// an explicit decision to defer judgment.
    pub status: String,
    pub rotation_degrees: Option<u16>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrientationDecisionPayload {
    status: String,
    rotation_degrees: Option<u16>,
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
pub(crate) struct CameraIdentityMergedPayload {
    pub(crate) source_camera: String,
    pub(crate) destination_camera: String,
    pub(crate) asset_ids: Vec<String>,
    pub(crate) capture_sequence_ids: Vec<String>,
    pub(crate) assignments: Vec<CameraSequenceAssignmentPayload>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CameraSequenceAssignmentPayload {
    pub(crate) asset_id: String,
    pub(crate) capture_sequence_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CameraAssetsRepairedPayload {
    camera: String,
    assignments: Vec<CameraAssetRepairAssignment>,
    new_sequences: Vec<crate::chronology::CaptureSequence>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CameraAssetRepairAssignment {
    asset_id: String,
    source_capture_sequence_id: String,
    capture_sequence_id: String,
    filename_counter: Option<u64>,
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
    #[serde(default)]
    pub timestamp_candidates: Option<Vec<crate::chronology::TimestampCandidate>>,
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
/// (`4c5eb2f:docs/tasks/desktop/CHRONOLOGY_MODEL.md`). All are payload version 1 with
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
pub(crate) struct CaptureSequenceSplitExactPayload {
    pub(crate) new_sequence_id: String,
    pub(crate) camera_key: String,
    pub(crate) label: String,
    pub(crate) volume_hint: String,
    pub(crate) filename_prefix: String,
    pub(crate) at_counter: u64,
    pub(crate) asset_ids: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CaptureSequenceMergedPayload {
    pub(crate) from_sequence_id: String,
    pub(crate) into_sequence_id: String,
    /// New merge operations carry the exact facts the user confirmed. `None`
    /// remains the backwards-compatible meaning for legacy payloads authored
    /// before exact previews existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) asset_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) epoch_ids: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CaptureSequenceReassignedPayload {
    pub(crate) from_sequence_id: String,
    pub(crate) into_sequence_id: String,
    /// Explicit identities make replay deterministic even when later imports
    /// introduce clips whose counters fall inside the originally selected run.
    pub(crate) asset_ids: Vec<String>,
}

/// "That counter restart is not a card change." Carries the boundary rather
/// than a proposal id, because proposals are derived and have no identity that
/// survives a resolve.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CaptureSequenceSplitDeclinedPayload {
    pub(crate) at_counter: u64,
    #[serde(default)]
    pub(crate) reason: String,
}

/// Undoes the above, putting the boundary back in front of the user.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CaptureSequenceSplitRestoredPayload {
    pub(crate) at_counter: u64,
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ClockQuestionDecisionPayload {
    pub(crate) question_id: String,
    pub(crate) accepted: bool,
    pub(crate) alternative_id: Option<String>,
    pub(crate) note: String,
}

pub(crate) struct NewOperation {
    pub(crate) aggregate_type: &'static str,
    pub(crate) aggregate_id: String,
    pub(crate) operation_type: &'static str,
    pub(crate) base_revision: Option<u64>,
    pub(crate) payload: Value,
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

pub(crate) fn commit_local_in_transaction(
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
            payload_version: if new.operation_type == EDIT_PLAN_REVISION_CREATED
                && new.payload.get("storyPolicy").is_some_and(|v| !v.is_null())
            {
                5
            } else if new.operation_type == EDIT_PLAN_REVISION_CREATED
                && new
                    .payload
                    .get("patchLineage")
                    .is_some_and(|v| !v.is_null())
            {
                4
            } else if new.operation_type == EDIT_PLAN_REVISION_CREATED
                && new.payload.get("selection").is_some_and(|v| !v.is_null())
            {
                3
            } else if (new.operation_type == PROJECT_ASSET_ADDED
                && new
                    .payload
                    .get("timestampCandidates")
                    .is_some_and(|value| value.is_array()))
                || (new.operation_type == EDIT_PLAN_REVISION_CREATED
                    && new.payload["operations"].as_array().is_some_and(|cuts| {
                        cuts.iter()
                            .any(|cut| cut.get("audio").is_some_and(|audio| !audio.is_null()))
                    }))
            {
                2
            } else {
                1
            },
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

pub(crate) fn validate_orientation_decision(status: &str, degrees: Option<u16>) -> Result<()> {
    match (status, degrees) {
        ("confirmed", Some(0 | 90 | 180 | 270)) => Ok(()),
        ("unresolved", None) => Ok(()),
        ("confirmed", Some(degrees)) => bail!("invalid confirmed rotation: {degrees}"),
        _ => bail!(
            "orientation decision must be confirmed with rotation degrees or unresolved without them"
        ),
    }
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
    if operation.operation_type == ORIENTATION_DECISION_SET && operation.base_revision.is_none() {
        bail!("orientation decision requires a base revision");
    }
    if operation.logical_time.node_id != operation.origin_node_id {
        bail!("logical time node ID must match origin node ID");
    }
    if operation.payload_version != 1
        && !(matches!(
            operation.operation_type.as_str(),
            PROJECT_ASSET_ADDED | EDIT_PLAN_REVISION_CREATED
        ) && operation.payload_version == 2)
        && !(operation.operation_type == EDIT_PLAN_REVISION_CREATED
            && matches!(operation.payload_version, 3..=5))
        && is_known_operation_type(&operation.operation_type)
    {
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
#[path = "operations_tests.rs"]
mod tests;
