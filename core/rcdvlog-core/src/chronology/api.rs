//! The chronology write surface: capture-sequence management (D01-T009),
//! clock epochs (D01-T010), clip time exceptions (D01-T011), trip phases
//! (D01-T012), and event-order constraints with contradiction reporting
//! (D01-T013).
//!
//! Every method here commits an ordinary typed operation through the same
//! `commit_local` path as the rest of the catalog, so chronology state is never
//! a parallel model. Exact preview confirmations refresh the derived
//! `assets.best_time` and `assets.offset_seconds` projection in the same
//! transaction (`CHRONOLOGY_MODEL.md`, "Evidence only").

use super::model::{
    CaptureSequence, ClipTimeException, ClockEpoch, ConstraintStrength, DisplayMode,
    EventOrderConstraint, PhaseKind, PhaseSplitDirection, TripPhase,
};
use super::phases::{is_valid_timezone, parse_utc};
use super::solver::{
    ChronologySolution, Contradiction, ContradictionKind, resolve_anchored_phases, solve,
};
use crate::Catalog;
use crate::operations::{
    CAPTURE_SEQUENCE_DECLARED, CAPTURE_SEQUENCE_MERGED, CAPTURE_SEQUENCE_REASSIGNED,
    CAPTURE_SEQUENCE_SPLIT, CAPTURE_SEQUENCE_SPLIT_DECLINED, CAPTURE_SEQUENCE_SPLIT_EXACT,
    CAPTURE_SEQUENCE_SPLIT_RESTORED, CLIP_TIME_EXCEPTION_CLEARED, CLIP_TIME_EXCEPTION_SET,
    CLOCK_EPOCH_CLEARED, CLOCK_EPOCH_SET, CaptureSequenceDeclaredPayload,
    CaptureSequenceMergedPayload, CaptureSequenceReassignedPayload,
    CaptureSequenceSplitDeclinedPayload, CaptureSequenceSplitExactPayload,
    CaptureSequenceSplitPayload, CaptureSequenceSplitRestoredPayload, ClipTimeExceptionSetPayload,
    ClockEpochClearedPayload, ClockEpochSetPayload, EVENT_ORDER_CONSTRAINT_CLEARED,
    EVENT_ORDER_CONSTRAINT_SET, EventOrderConstraintClearedPayload, EventOrderConstraintSetPayload,
    NewOperation, TRIP_PHASE_REMOVED, TRIP_PHASE_SET, TripPhaseRemovedPayload, TripPhaseSetPayload,
    commit_local_in_transaction, current_revision,
};
use anyhow::Result;
use chrono::Duration;
use rusqlite::TransactionBehavior;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;
use uuid::Uuid;

/// One clip whose solved placement would change. Both positions come from the
/// core solver; adapters only format this evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChronologyImpactChange {
    pub asset_id: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub before_position: usize,
    pub after_position: usize,
}

/// A mutation preview. A hard contradiction blocks apply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChronologyImpact {
    pub changes: Vec<ChronologyImpactChange>,
    pub contradictions: Vec<Contradiction>,
    pub blocked: bool,
}

/// The concrete epoch a manual correction preview would write, plus its exact
/// solver diff. A blank caller-supplied id is allocated here so confirmation
/// applies the same identity the preview used for epoch tie-breaking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockEpochImpactPreview {
    pub epoch: ClockEpoch,
    pub facts_token: String,
    pub impact: ChronologyImpact,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockEpochClearPreview {
    pub epoch_id: String,
    pub facts_token: String,
    pub impact: ChronologyImpact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VisualPlacementSide {
    Before,
    After,
}

/// The existing fact a visual drag would write, plus the exact solver diff it
/// would cause. The desktop never has to reverse-engineer counters or offsets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisualPlacementPreview {
    pub epoch: ClockEpoch,
    pub facts_token: String,
    pub impact: ChronologyImpact,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSequenceReassignmentPreview {
    pub from_sequence_id: String,
    pub into_sequence_id: String,
    pub asset_ids: Vec<String>,
    pub facts_token: String,
    pub impact: ChronologyImpact,
}

/// An exact capture-sequence split candidate. `created_sequence.id` is
/// allocated during preview and confirmation writes that same identity.
/// `asset_ids` are explicit because a restarted card can reuse every counter
/// from the preceding card, making a counter-only mutation ambiguous.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSequenceSplitPreview {
    pub source_sequence_id: String,
    pub at_counter: u64,
    pub created_sequence: CaptureSequence,
    pub asset_ids: Vec<String>,
    pub facts_token: String,
    pub impact: ChronologyImpact,
}

/// An exact whole-sequence merge candidate. The facts token makes confirmation
/// refuse a preview whose solver inputs changed in the meantime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSequenceMergePreview {
    pub from_sequence_id: String,
    pub into_sequence_id: String,
    pub asset_ids: Vec<String>,
    pub epoch_ids: Vec<String>,
    pub facts_token: String,
    pub impact: ChronologyImpact,
}

/// One internally coherent read for the desktop calibration surface. Facts,
/// solution, and proposals are all derived from the same loaded catalog state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChronologySnapshot {
    pub facts: super::model::ChronologyFacts,
    pub solution: ChronologySolution,
    pub proposed_splits: Vec<SplitProposal>,
    /// Boundaries the user has already answered "same card" to. Carried so the
    /// desk can list and reverse those answers; they never affect the solve.
    pub declined_splits: Vec<DeclinedSplit>,
}

/// The conditions a chronology write refuses on, each with a stable code so the
/// desktop, the CLI, and diagnostics all name the same failure.
#[derive(Debug, Error)]
pub enum ChronologyError {
    #[error("chronology.hard_contradiction: {0}")]
    HardContradiction(String),
    #[error("chronology.invalid_timezone: {0} is not a resolvable IANA timezone")]
    InvalidTimezone(String),
    #[error("chronology.invalid_interval: {0}")]
    InvalidInterval(String),
    #[error("chronology.invalid_reference: {0}")]
    InvalidReference(String),
}

impl ChronologyError {
    /// The stable code, for diagnostics and for the CLI's exit surface.
    pub fn code(&self) -> &'static str {
        match self {
            Self::HardContradiction(_) => "chronology.hard_contradiction",
            Self::InvalidTimezone(_) => "chronology.invalid_timezone",
            Self::InvalidInterval(_) => "chronology.invalid_interval",
            Self::InvalidReference(_) => "chronology.invalid_reference",
        }
    }
}

fn phase_kind_str(kind: PhaseKind) -> &'static str {
    match kind {
        PhaseKind::Location => "location",
        PhaseKind::Transit => "transit",
    }
}

fn display_mode_str(mode: DisplayMode) -> &'static str {
    match mode {
        DisplayMode::Local => "local",
        DisplayMode::Dual => "dual",
        DisplayMode::Utc => "utc",
    }
}

fn trip_phase_set_operation(phase: &TripPhase, revision: u64) -> Result<NewOperation> {
    Ok(NewOperation {
        aggregate_type: "tripPhase",
        aggregate_id: phase.id.clone(),
        operation_type: TRIP_PHASE_SET,
        base_revision: Some(revision),
        payload: serde_json::to_value(TripPhaseSetPayload {
            phase_id: phase.id.clone(),
            label: phase.label.clone(),
            phase_kind: phase_kind_str(phase.kind).to_string(),
            start_anchor_asset_id: phase.start_anchor_asset_id.clone(),
            end_anchor_asset_id: phase.end_anchor_asset_id.clone(),
            start_trip_time: phase.start_trip_time.clone(),
            end_trip_time: phase.end_trip_time.clone(),
            timezone: phase.timezone.clone(),
            origin_timezone: phase.origin_timezone.clone(),
            destination_timezone: phase.destination_timezone.clone(),
            display_mode: display_mode_str(phase.display_mode).to_string(),
        })?,
    })
}

fn strength_str(strength: ConstraintStrength) -> &'static str {
    match strength {
        ConstraintStrength::Hard => "hard",
        ConstraintStrength::Soft => "soft",
    }
}

fn chronology_impact(before: &ChronologySolution, after: ChronologySolution) -> ChronologyImpact {
    let before_by_id: BTreeMap<&str, (usize, Option<&str>)> = before
        .assets
        .iter()
        .enumerate()
        .map(|(position, asset)| {
            (
                asset.asset_id.as_str(),
                (position, asset.trip_time.as_deref()),
            )
        })
        .collect();
    let mut changes = Vec::new();
    for (after_position, asset) in after.assets.iter().enumerate() {
        let Some((before_position, before_time)) = before_by_id.get(asset.asset_id.as_str()) else {
            continue;
        };
        if *before_position != after_position || *before_time != asset.trip_time.as_deref() {
            changes.push(ChronologyImpactChange {
                asset_id: asset.asset_id.clone(),
                before: before_time.map(str::to_string),
                after: asset.trip_time.clone(),
                before_position: *before_position,
                after_position,
            });
        }
    }
    let blocked = after
        .contradictions
        .iter()
        .any(|found| found.strength == ConstraintStrength::Hard);
    ChronologyImpact {
        changes,
        contradictions: after.contradictions,
        blocked,
    }
}

fn refuse_hard_impact(impact: &ChronologyImpact) -> Result<()> {
    if let Some(blocking) = impact
        .contradictions
        .iter()
        .find(|found| found.strength == ConstraintStrength::Hard)
    {
        return Err(ChronologyError::HardContradiction(blocking.message.clone()).into());
    }
    Ok(())
}

fn chronology_facts_token(facts: &super::model::ChronologyFacts) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(facts)?)
        .to_hex()
        .to_string())
}

/// A capture-sequence split the evidence suggests but the user must confirm.
/// No chronology change is applied without explicit user confirmation
/// (architecture contract), so this is a proposal and nothing more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitProposal {
    pub capture_sequence_id: String,
    pub at_counter: u64,
    pub reason: String,
    pub affected_asset_ids: Vec<String>,
}

/// A counter restart the user has answered "no" to: same card, keep it whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclinedSplit {
    pub capture_sequence_id: String,
    pub at_counter: u64,
    pub reason: String,
    pub declined_at: String,
}

impl Catalog {
    // --- D01-T009: capture-sequence management -----------------------------

    /// Declares a new capture sequence for a camera, for example when a second
    /// card is imported and its clips must not share the first card's counter
    /// space.
    pub fn declare_capture_sequence(
        &self,
        project_id: &str,
        camera_key: &str,
        label: &str,
        volume_hint: &str,
        filename_prefix: &str,
    ) -> Result<CaptureSequence> {
        self.require_project(project_id)?;
        let sequence = CaptureSequence {
            id: format!("sequence_{}", Uuid::new_v4().simple()),
            camera_key: camera_key.to_string(),
            label: label.trim().to_string(),
            volume_hint: volume_hint.trim().to_string(),
            filename_prefix: filename_prefix.trim().to_string(),
        };
        self.commit_local(
            project_id,
            vec![NewOperation {
                aggregate_type: "captureSequence",
                aggregate_id: sequence.id.clone(),
                operation_type: CAPTURE_SEQUENCE_DECLARED,
                base_revision: Some(0),
                payload: serde_json::to_value(CaptureSequenceDeclaredPayload {
                    sequence_id: sequence.id.clone(),
                    camera_key: sequence.camera_key.clone(),
                    label: sequence.label.clone(),
                    volume_hint: sequence.volume_hint.clone(),
                    filename_prefix: sequence.filename_prefix.clone(),
                })?,
            }],
        )?;
        Ok(sequence)
    }

    /// Records the card a capture sequence's footage actually came from (#74).
    ///
    /// Import creates a sequence per camera with empty `volume_hint` and
    /// `filename_prefix`, and an empty hint means "matches any run" — so until
    /// something writes real evidence, every later import from that camera
    /// extends the same sequence and the reformatted-card detectors can never
    /// fire. This is the write that closes that loop, and it is deliberately
    /// narrow:
    ///
    /// - **Observed, not inferred.** It records where bytes came from, which is
    ///   a fact this machine witnessed with the card in the slot. It changes no
    ///   ordering, no time, and no membership, so it is not a chronology
    ///   correction and needs no confirmation gate — the only thing it can
    ///   cause is the app asking *more* questions of a later import, never
    ///   fewer.
    /// - **First write wins.** A sequence that already names a card keeps it.
    ///   Overwriting would erase precisely the evidence that lets the next
    ///   reformatted card be recognised as a different one.
    /// - **Card sources only.** A folder import has no durable identity to
    ///   record; a mount path is not a name.
    ///
    /// It goes through `captureSequenceDeclared` rather than a direct UPDATE so
    /// it is an operation like every other project mutation: logged, guarded by
    /// the aggregate's revision, and carried to peers.
    ///
    /// Returns the sequence when it recorded something, and `None` when there
    /// was nothing to annotate or the sequence already named a card.
    pub fn record_capture_sequence_card(
        &self,
        project_id: &str,
        camera_key: &str,
        volume_hint: &str,
        filename_prefix: &str,
    ) -> Result<Option<CaptureSequence>> {
        self.require_project(project_id)?;
        let volume_hint = volume_hint.trim();
        if volume_hint.is_empty() {
            return Ok(None);
        }
        let camera_key = self.canonical_camera_identity(project_id, camera_key)?;
        let sequence_id = super::capture_sequence_id_for(project_id, &camera_key);
        let Some(existing) = self
            .capture_sequences(project_id)?
            .into_iter()
            .find(|sequence| sequence.id == sequence_id)
        else {
            // Nothing to annotate. The sequence is created by ingest, and this
            // runs after it, so absence means this camera imported nothing.
            return Ok(None);
        };
        if !existing.volume_hint.is_empty() {
            return Ok(None);
        }

        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let revision = current_revision(&transaction, project_id, "captureSequence", &sequence_id)?;
        let recorded = CaptureSequence {
            volume_hint: volume_hint.to_string(),
            filename_prefix: filename_prefix.trim().to_string(),
            ..existing
        };
        commit_local_in_transaction(
            &transaction,
            project_id,
            vec![NewOperation {
                aggregate_type: "captureSequence",
                aggregate_id: sequence_id.clone(),
                operation_type: CAPTURE_SEQUENCE_DECLARED,
                base_revision: Some(revision),
                payload: serde_json::to_value(CaptureSequenceDeclaredPayload {
                    sequence_id,
                    camera_key: recorded.camera_key.clone(),
                    label: recorded.label.clone(),
                    volume_hint: recorded.volume_hint.clone(),
                    filename_prefix: recorded.filename_prefix.clone(),
                })?,
            }],
        )?;
        transaction.commit()?;
        Ok(Some(recorded))
    }

    fn capture_sequence_split_preview_from_facts(
        facts: super::model::ChronologyFacts,
        source_sequence_id: &str,
        at_counter: u64,
        created_sequence: CaptureSequence,
        asset_ids: &[String],
    ) -> Result<CaptureSequenceSplitPreview> {
        let source = facts
            .sequences
            .iter()
            .find(|sequence| sequence.id == source_sequence_id)
            .ok_or_else(|| {
                ChronologyError::InvalidReference(format!(
                    "capture sequence {source_sequence_id} is not in this project"
                ))
            })?;
        if created_sequence.id.trim().is_empty()
            || created_sequence.id == source_sequence_id
            || facts
                .sequences
                .iter()
                .any(|sequence| sequence.id == created_sequence.id)
        {
            return Err(ChronologyError::InvalidReference(
                "the split destination identity is unavailable".to_string(),
            )
            .into());
        }
        if created_sequence.camera_key != source.camera_key
            || created_sequence.volume_hint != source.volume_hint
            || created_sequence.filename_prefix != source.filename_prefix
            || created_sequence.label != created_sequence.label.trim()
        {
            return Err(ChronologyError::InvalidReference(
                "the split destination details no longer match the source sequence".to_string(),
            )
            .into());
        }
        if asset_ids.is_empty() {
            return Err(ChronologyError::InvalidReference(
                "select at least one clip for the new capture sequence".to_string(),
            )
            .into());
        }
        let mut exact_asset_ids = asset_ids.to_vec();
        exact_asset_ids.sort();
        if exact_asset_ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(ChronologyError::InvalidReference(
                "a split cannot contain the same clip more than once".to_string(),
            )
            .into());
        }
        let mut includes_boundary = false;
        for asset_id in &exact_asset_ids {
            let asset = facts
                .assets
                .iter()
                .find(|asset| asset.asset_id == *asset_id)
                .ok_or_else(|| {
                    ChronologyError::InvalidReference(format!(
                        "asset {asset_id} is not in this project"
                    ))
                })?;
            if asset.capture_sequence_id.as_deref() != Some(source_sequence_id) {
                return Err(ChronologyError::InvalidReference(format!(
                    "the split preview is stale: asset {asset_id} is no longer in source sequence {source_sequence_id}"
                ))
                .into());
            }
            includes_boundary |= asset.filename_counter == Some(at_counter);
        }
        if !includes_boundary {
            return Err(ChronologyError::InvalidReference(format!(
                "the selected clips do not include counter {at_counter}"
            ))
            .into());
        }

        let facts_token = chronology_facts_token(&facts)?;
        let before = solve(&facts);
        let moved: BTreeSet<&str> = exact_asset_ids.iter().map(String::as_str).collect();
        let mut candidate = facts;
        candidate.sequences.push(created_sequence.clone());
        candidate
            .sequences
            .sort_by(|left, right| left.id.cmp(&right.id));
        for asset in &mut candidate.assets {
            if moved.contains(asset.asset_id.as_str()) {
                asset.capture_sequence_id = Some(created_sequence.id.clone());
            }
        }
        Ok(CaptureSequenceSplitPreview {
            source_sequence_id: source_sequence_id.to_string(),
            at_counter,
            created_sequence,
            asset_ids: exact_asset_ids,
            facts_token,
            impact: chronology_impact(&before, solve(&candidate)),
        })
    }

    /// Previews an exact split without writing. The caller supplies the clip
    /// identities because counter restarts make a counter-only selection
    /// ambiguous; confirmation receives the returned candidate unchanged.
    pub fn preview_capture_sequence_split(
        &self,
        project_id: &str,
        source_sequence_id: &str,
        at_counter: u64,
        label: &str,
        asset_ids: &[String],
    ) -> Result<CaptureSequenceSplitPreview> {
        let facts = self.chronology_facts(project_id)?;
        let source = facts
            .sequences
            .iter()
            .find(|sequence| sequence.id == source_sequence_id)
            .ok_or_else(|| {
                ChronologyError::InvalidReference(format!(
                    "capture sequence {source_sequence_id} is not in this project"
                ))
            })?;
        let created_sequence = CaptureSequence {
            id: format!("sequence_{}", Uuid::new_v4().simple()),
            camera_key: source.camera_key.clone(),
            label: label.trim().to_string(),
            volume_hint: source.volume_hint.clone(),
            filename_prefix: source.filename_prefix.clone(),
        };
        Self::capture_sequence_split_preview_from_facts(
            facts,
            source_sequence_id,
            at_counter,
            created_sequence,
            asset_ids,
        )
    }

    /// Confirms exactly the candidate returned by
    /// `preview_capture_sequence_split`. One typed operation creates the
    /// sequence and moves the explicit identities atomically, including on
    /// replay, so neither a partial split nor an expanded selection appears.
    pub fn apply_capture_sequence_split(
        &self,
        project_id: &str,
        preview: &CaptureSequenceSplitPreview,
    ) -> Result<CaptureSequence> {
        self.require_project(project_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let facts = super::load_chronology_facts(&transaction, project_id)?;
        let expected = Self::capture_sequence_split_preview_from_facts(
            facts,
            &preview.source_sequence_id,
            preview.at_counter,
            preview.created_sequence.clone(),
            &preview.asset_ids,
        )?;
        if expected != *preview {
            return Err(ChronologyError::InvalidReference(
                "the split preview is stale; preview it again before applying".to_string(),
            )
            .into());
        }
        refuse_hard_impact(&expected.impact)?;
        if current_revision(
            &transaction,
            project_id,
            "captureSequence",
            &preview.created_sequence.id,
        )? != 0
        {
            return Err(ChronologyError::InvalidReference(
                "the split destination identity is no longer available".to_string(),
            )
            .into());
        }
        let source_revision = current_revision(
            &transaction,
            project_id,
            "captureSequence",
            &preview.source_sequence_id,
        )?;
        commit_local_in_transaction(
            &transaction,
            project_id,
            vec![NewOperation {
                aggregate_type: "captureSequence",
                aggregate_id: preview.source_sequence_id.clone(),
                operation_type: CAPTURE_SEQUENCE_SPLIT_EXACT,
                base_revision: Some(source_revision),
                payload: serde_json::to_value(CaptureSequenceSplitExactPayload {
                    new_sequence_id: preview.created_sequence.id.clone(),
                    camera_key: preview.created_sequence.camera_key.clone(),
                    label: preview.created_sequence.label.clone(),
                    volume_hint: preview.created_sequence.volume_hint.clone(),
                    filename_prefix: preview.created_sequence.filename_prefix.clone(),
                    at_counter: preview.at_counter,
                    asset_ids: preview.asset_ids.clone(),
                })?,
            }],
        )?;
        Self::project_current_chronology_in_transaction(&transaction, project_id)?;
        transaction.commit()?;
        Ok(preview.created_sequence.clone())
    }

    /// Splits a capture sequence at a counter: assets with
    /// `filename_counter >= at_counter` move to a new sequence. Assets with a
    /// NULL counter stay in the source. Nothing is ever dropped.
    pub fn split_capture_sequence(
        &self,
        project_id: &str,
        source_sequence_id: &str,
        at_counter: u64,
        label: &str,
    ) -> Result<CaptureSequence> {
        self.require_project(project_id)?;
        let source = self
            .capture_sequences(project_id)?
            .into_iter()
            .find(|candidate| candidate.id == source_sequence_id)
            .ok_or_else(|| {
                ChronologyError::InvalidReference(format!(
                    "capture sequence {source_sequence_id} is not in this project"
                ))
            })?;
        let created = CaptureSequence {
            id: format!("sequence_{}", Uuid::new_v4().simple()),
            camera_key: source.camera_key.clone(),
            label: label.trim().to_string(),
            volume_hint: source.volume_hint.clone(),
            filename_prefix: source.filename_prefix.clone(),
        };
        let revision = current_revision(
            &self.connection()?,
            project_id,
            "captureSequence",
            source_sequence_id,
        )?;
        self.commit_local(
            project_id,
            vec![NewOperation {
                aggregate_type: "captureSequence",
                aggregate_id: source_sequence_id.to_string(),
                operation_type: CAPTURE_SEQUENCE_SPLIT,
                base_revision: Some(revision),
                payload: serde_json::to_value(CaptureSequenceSplitPayload {
                    new_sequence_id: created.id.clone(),
                    camera_key: created.camera_key.clone(),
                    label: created.label.clone(),
                    volume_hint: created.volume_hint.clone(),
                    filename_prefix: created.filename_prefix.clone(),
                    at_counter,
                })?,
            }],
        )?;
        Ok(created)
    }

    /// Merges one capture sequence into another: the asset sets union, the
    /// source's epochs re-point at the destination, and the source is deleted.
    /// Merging unions the facts rather than discarding the source's.
    pub fn merge_capture_sequences(
        &self,
        project_id: &str,
        from_sequence_id: &str,
        into_sequence_id: &str,
    ) -> Result<()> {
        let preview =
            self.preview_capture_sequence_merge(project_id, from_sequence_id, into_sequence_id)?;
        self.apply_capture_sequence_merge(project_id, &preview)
    }

    fn capture_sequence_merge_preview_from_facts(
        facts: super::model::ChronologyFacts,
        from_sequence_id: &str,
        into_sequence_id: &str,
    ) -> Result<CaptureSequenceMergePreview> {
        if from_sequence_id == into_sequence_id {
            return Err(ChronologyError::InvalidReference(
                "a capture sequence cannot be merged into itself".to_string(),
            )
            .into());
        }
        for sequence_id in [from_sequence_id, into_sequence_id] {
            if !facts
                .sequences
                .iter()
                .any(|sequence| sequence.id == sequence_id)
            {
                return Err(ChronologyError::InvalidReference(format!(
                    "capture sequence {sequence_id} is not in this project"
                ))
                .into());
            }
        }
        let mut asset_ids: Vec<String> = facts
            .assets
            .iter()
            .filter(|asset| asset.capture_sequence_id.as_deref() == Some(from_sequence_id))
            .map(|asset| asset.asset_id.clone())
            .collect();
        asset_ids.sort();
        let mut epoch_ids: Vec<String> = facts
            .epochs
            .iter()
            .filter(|epoch| epoch.capture_sequence_id == from_sequence_id)
            .map(|epoch| epoch.id.clone())
            .collect();
        epoch_ids.sort();
        let facts_token = chronology_facts_token(&facts)?;
        let before = solve(&facts);
        let mut candidate = facts;
        for asset in &mut candidate.assets {
            if asset.capture_sequence_id.as_deref() == Some(from_sequence_id) {
                asset.capture_sequence_id = Some(into_sequence_id.to_string());
            }
        }
        for epoch in &mut candidate.epochs {
            if epoch.capture_sequence_id == from_sequence_id {
                epoch.capture_sequence_id = into_sequence_id.to_string();
            }
        }
        candidate
            .sequences
            .retain(|sequence| sequence.id != from_sequence_id);
        Ok(CaptureSequenceMergePreview {
            from_sequence_id: from_sequence_id.to_string(),
            into_sequence_id: into_sequence_id.to_string(),
            asset_ids,
            epoch_ids,
            facts_token,
            impact: chronology_impact(&before, solve(&candidate)),
        })
    }

    /// Previews a whole-sequence merge without writing.
    pub fn preview_capture_sequence_merge(
        &self,
        project_id: &str,
        from_sequence_id: &str,
        into_sequence_id: &str,
    ) -> Result<CaptureSequenceMergePreview> {
        Self::capture_sequence_merge_preview_from_facts(
            self.chronology_facts(project_id)?,
            from_sequence_id,
            into_sequence_id,
        )
    }

    /// Applies an unchanged merge preview. Any chronology fact written after
    /// preview invalidates the token so the user sees a fresh impact before
    /// confirming a different result.
    pub fn apply_capture_sequence_merge(
        &self,
        project_id: &str,
        preview: &CaptureSequenceMergePreview,
    ) -> Result<()> {
        self.require_project(project_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let facts = super::load_chronology_facts(&transaction, project_id)?;
        let expected = Self::capture_sequence_merge_preview_from_facts(
            facts,
            &preview.from_sequence_id,
            &preview.into_sequence_id,
        )?;
        if expected != *preview {
            return Err(ChronologyError::InvalidReference(
                "the merge preview is stale; preview it again before applying".to_string(),
            )
            .into());
        }
        refuse_hard_impact(&expected.impact)?;
        // The merge rides the destination aggregate: it is the sequence that
        // survives and gains rows, so its revision guards the change.
        let revision = current_revision(
            &transaction,
            project_id,
            "captureSequence",
            &preview.into_sequence_id,
        )?;
        commit_local_in_transaction(
            &transaction,
            project_id,
            vec![NewOperation {
                aggregate_type: "captureSequence",
                aggregate_id: preview.into_sequence_id.clone(),
                operation_type: CAPTURE_SEQUENCE_MERGED,
                base_revision: Some(revision),
                payload: serde_json::to_value(CaptureSequenceMergedPayload {
                    from_sequence_id: preview.from_sequence_id.clone(),
                    into_sequence_id: preview.into_sequence_id.clone(),
                    asset_ids: Some(preview.asset_ids.clone()),
                    epoch_ids: Some(preview.epoch_ids.clone()),
                })?,
            }],
        )?;
        Self::project_current_chronology_in_transaction(&transaction, project_id)?;
        transaction.commit()?;
        Ok(())
    }

    fn capture_sequence_reassignment_candidate(
        &self,
        project_id: &str,
        anchor_asset_id: &str,
        into_sequence_id: &str,
    ) -> Result<(String, Vec<String>, super::model::ChronologyFacts)> {
        let facts = self.chronology_facts(project_id)?;
        if !facts
            .sequences
            .iter()
            .any(|sequence| sequence.id == into_sequence_id)
        {
            return Err(ChronologyError::InvalidReference(format!(
                "capture sequence {into_sequence_id} is not in this project"
            ))
            .into());
        }
        let anchor = facts
            .assets
            .iter()
            .find(|asset| asset.asset_id == anchor_asset_id)
            .ok_or_else(|| {
                ChronologyError::InvalidReference(format!(
                    "asset {anchor_asset_id} is not in this project"
                ))
            })?;
        let from_sequence_id = anchor.capture_sequence_id.clone().ok_or_else(|| {
            ChronologyError::InvalidReference(format!(
                "asset {anchor_asset_id} has no capture sequence"
            ))
        })?;
        if from_sequence_id == into_sequence_id {
            return Err(ChronologyError::InvalidReference(
                "the clip is already in that capture sequence".to_string(),
            )
            .into());
        }

        // "This clip and following" means the actual visible chronology
        // suffix. Counter predicates are not identity-safe: a second card can
        // restart at C0001, and `counter >= 1` would reach backwards into the
        // first card. The solver provides the stable raw/corrected order for
        // both placed and unplaced clips.
        let solved = solve(&facts);
        let sequence_order: Vec<&str> = solved
            .assets
            .iter()
            .filter(|asset| asset.order_key.capture_sequence_id == from_sequence_id)
            .map(|asset| asset.asset_id.as_str())
            .collect();
        let start = sequence_order
            .iter()
            .position(|asset_id| *asset_id == anchor_asset_id)
            .ok_or_else(|| {
                ChronologyError::InvalidReference(format!(
                    "asset {anchor_asset_id} is not in its capture sequence"
                ))
            })?;
        let mut asset_ids: Vec<String> = sequence_order[start..]
            .iter()
            .map(|asset_id| (*asset_id).to_string())
            .collect();
        asset_ids.sort();
        asset_ids.dedup();
        if asset_ids.is_empty() {
            return Err(ChronologyError::InvalidReference(
                "no clips are available to move".to_string(),
            )
            .into());
        }
        Ok((from_sequence_id, asset_ids, facts))
    }

    fn validate_capture_sequence_asset_ids(
        facts: &super::model::ChronologyFacts,
        from_sequence_id: &str,
        into_sequence_id: &str,
        asset_ids: &[String],
    ) -> Result<Vec<String>> {
        if from_sequence_id == into_sequence_id {
            return Err(ChronologyError::InvalidReference(
                "a reassignment requires two distinct capture sequences".to_string(),
            )
            .into());
        }
        for sequence_id in [from_sequence_id, into_sequence_id] {
            if !facts
                .sequences
                .iter()
                .any(|sequence| sequence.id == sequence_id)
            {
                return Err(ChronologyError::InvalidReference(format!(
                    "capture sequence {sequence_id} is not in this project"
                ))
                .into());
            }
        }
        if asset_ids.is_empty() {
            return Err(ChronologyError::InvalidReference(
                "select at least one clip to reassign".to_string(),
            )
            .into());
        }
        let mut exact_asset_ids = asset_ids.to_vec();
        exact_asset_ids.sort();
        if exact_asset_ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(ChronologyError::InvalidReference(
                "a reassignment cannot contain the same clip more than once".to_string(),
            )
            .into());
        }
        for asset_id in &exact_asset_ids {
            let current_sequence = facts
                .assets
                .iter()
                .find(|asset| asset.asset_id == *asset_id)
                .and_then(|asset| asset.capture_sequence_id.as_deref());
            if current_sequence != Some(from_sequence_id) {
                return Err(ChronologyError::InvalidReference(format!(
                    "the preview is stale: asset {asset_id} is no longer in source sequence {from_sequence_id}"
                ))
                .into());
            }
        }
        Ok(exact_asset_ids)
    }

    fn capture_sequence_assets_preview(
        facts: super::model::ChronologyFacts,
        from_sequence_id: &str,
        into_sequence_id: &str,
        asset_ids: &[String],
    ) -> Result<CaptureSequenceReassignmentPreview> {
        let exact_asset_ids = Self::validate_capture_sequence_asset_ids(
            &facts,
            from_sequence_id,
            into_sequence_id,
            asset_ids,
        )?;
        let facts_token = chronology_facts_token(&facts)?;
        let before = solve(&facts);
        let moved: BTreeSet<&str> = exact_asset_ids.iter().map(String::as_str).collect();
        let mut candidate = facts;
        for asset in &mut candidate.assets {
            if moved.contains(asset.asset_id.as_str()) {
                asset.capture_sequence_id = Some(into_sequence_id.to_string());
            }
        }
        let impact = chronology_impact(&before, solve(&candidate));
        Ok(CaptureSequenceReassignmentPreview {
            from_sequence_id: from_sequence_id.to_string(),
            into_sequence_id: into_sequence_id.to_string(),
            asset_ids: exact_asset_ids,
            facts_token,
            impact,
        })
    }

    /// Previews moving one clip and the rest of its card run into another
    /// capture sequence. No operation is emitted.
    pub fn preview_capture_sequence_reassignment(
        &self,
        project_id: &str,
        anchor_asset_id: &str,
        into_sequence_id: &str,
    ) -> Result<CaptureSequenceReassignmentPreview> {
        let (from_sequence_id, asset_ids, facts) = self.capture_sequence_reassignment_candidate(
            project_id,
            anchor_asset_id,
            into_sequence_id,
        )?;
        Self::capture_sequence_assets_preview(
            facts,
            &from_sequence_id,
            into_sequence_id,
            &asset_ids,
        )
    }

    /// Previews moving exactly the named clips from one capture sequence to
    /// another. This is the single-clip/custom-selection companion to
    /// `preview_capture_sequence_reassignment`; both return identities that
    /// `reassign_capture_sequence_assets` can confirm without re-derivation.
    pub fn preview_capture_sequence_assets(
        &self,
        project_id: &str,
        from_sequence_id: &str,
        into_sequence_id: &str,
        asset_ids: &[String],
    ) -> Result<CaptureSequenceReassignmentPreview> {
        Self::capture_sequence_assets_preview(
            self.chronology_facts(project_id)?,
            from_sequence_id,
            into_sequence_id,
            asset_ids,
        )
    }

    fn commit_capture_sequence_reassignment(
        transaction: &rusqlite::Transaction<'_>,
        project_id: &str,
        preview: &CaptureSequenceReassignmentPreview,
    ) -> Result<Vec<String>> {
        refuse_hard_impact(&preview.impact)?;
        let revision = current_revision(
            transaction,
            project_id,
            "captureSequence",
            &preview.from_sequence_id,
        )?;
        commit_local_in_transaction(
            transaction,
            project_id,
            vec![NewOperation {
                aggregate_type: "captureSequence",
                aggregate_id: preview.from_sequence_id.clone(),
                operation_type: CAPTURE_SEQUENCE_REASSIGNED,
                base_revision: Some(revision),
                payload: serde_json::to_value(CaptureSequenceReassignedPayload {
                    from_sequence_id: preview.from_sequence_id.clone(),
                    into_sequence_id: preview.into_sequence_id.clone(),
                    asset_ids: preview.asset_ids.clone(),
                })?,
            }],
        )?;
        Ok(preview.asset_ids.clone())
    }

    /// Applies the unchanged preview returned by either reassignment preview
    /// seam. Any chronology fact change invalidates confirmation, even when
    /// the selected assets themselves still belong to the source sequence.
    pub fn apply_capture_sequence_reassignment(
        &self,
        project_id: &str,
        preview: &CaptureSequenceReassignmentPreview,
    ) -> Result<Vec<String>> {
        self.require_project(project_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let facts = super::load_chronology_facts(&transaction, project_id)?;
        let expected = Self::capture_sequence_assets_preview(
            facts,
            &preview.from_sequence_id,
            &preview.into_sequence_id,
            &preview.asset_ids,
        )?;
        if expected != *preview {
            return Err(ChronologyError::InvalidReference(
                "the reassignment preview is stale; preview it again before applying".to_string(),
            )
            .into());
        }
        let moved =
            Self::commit_capture_sequence_reassignment(&transaction, project_id, &expected)?;
        Self::project_current_chronology_in_transaction(&transaction, project_id)?;
        transaction.commit()?;
        Ok(moved)
    }

    /// Applies the exact source and asset identities returned by the preview.
    /// Reassignment is a typed operation so replicas converge and a clip
    /// imported between preview and confirmation is never silently added to
    /// the move. If any previewed identity has since left the source sequence,
    /// confirmation is stale and the whole write is refused.
    pub fn reassign_capture_sequence_assets(
        &self,
        project_id: &str,
        from_sequence_id: &str,
        into_sequence_id: &str,
        asset_ids: &[String],
    ) -> Result<Vec<String>> {
        self.require_project(project_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let facts = super::load_chronology_facts(&transaction, project_id)?;
        let preview = Self::capture_sequence_assets_preview(
            facts,
            from_sequence_id,
            into_sequence_id,
            asset_ids,
        )?;
        let moved = Self::commit_capture_sequence_reassignment(&transaction, project_id, &preview)?;
        Self::project_current_chronology_in_transaction(&transaction, project_id)?;
        transaction.commit()?;
        Ok(moved)
    }

    /// The capture sequences of one project, sorted by id.
    pub fn capture_sequences(&self, project_id: &str) -> Result<Vec<CaptureSequence>> {
        Ok(self.chronology_facts(project_id)?.sequences)
    }

    /// Proposes capture-sequence splits the evidence supports, without applying
    /// any of them.
    ///
    /// The card-boundary signal is a counter that runs *backwards* against the
    /// raw timeline: two cards in one camera legitimately both contain
    /// `C0001.MP4`, so within a sequence sorted by capture time a counter that
    /// drops is the second card starting over. The proposal names both the
    /// counter and the exact timeline suffix, because the preceding card can
    /// contain the same counters.
    fn proposed_capture_sequence_splits(
        facts: &super::model::ChronologyFacts,
        declined: &BTreeSet<(String, u64)>,
    ) -> Vec<SplitProposal> {
        let mut proposals: Vec<SplitProposal> = Vec::new();
        for sequence in &facts.sequences {
            // Only assets that carry both a counter and a raw time can witness
            // a backwards counter; the rest cannot support a proposal.
            let mut timeline: Vec<(i64, u64, &str)> = facts
                .assets
                .iter()
                .filter(|asset| asset.capture_sequence_id.as_deref() == Some(sequence.id.as_str()))
                .filter_map(|asset| {
                    let counter = asset.filename_counter?;
                    let instant = parse_utc(asset.captured_at_raw.as_deref()?)?;
                    Some((instant.timestamp_millis(), counter, asset.asset_id.as_str()))
                })
                .collect();
            timeline.sort_unstable();

            // The first restart the user has *not* already answered. Scanning
            // past declined ones is what lets a three-card trip surface its
            // second boundary once the first is settled; stopping at the first
            // restart outright would silence the sequence for good.
            let Some(boundary_index) = timeline
                .windows(2)
                .position(|pair| {
                    pair[1].1 < pair[0].1 && !declined.contains(&(sequence.id.clone(), pair[1].1))
                })
                .map(|index| index + 1)
            else {
                continue;
            };
            let at_counter = timeline[boundary_index].1;
            // The exact suffix matters. Both cards can contain C0001, C0002,
            // and so on; filtering solely by counter would select clips from
            // both sides of the restart and make the proposal destructive.
            let mut affected: Vec<String> = timeline[boundary_index..]
                .iter()
                .map(|(_, _, asset_id)| (*asset_id).to_string())
                .collect();
            affected.sort();
            affected.dedup();
            proposals.push(SplitProposal {
                capture_sequence_id: sequence.id.clone(),
                at_counter,
                reason: format!(
                    "the filename counter restarts at {at_counter} partway through this sequence, which is what a second card looks like"
                ),
                affected_asset_ids: affected,
            });
        }
        proposals.sort_by(|left, right| {
            left.capture_sequence_id
                .cmp(&right.capture_sequence_id)
                .then_with(|| left.at_counter.cmp(&right.at_counter))
        });
        proposals
    }

    pub fn propose_capture_sequence_splits(&self, project_id: &str) -> Result<Vec<SplitProposal>> {
        let facts = self.chronology_facts(project_id)?;
        let declined = self.declined_split_boundaries(project_id)?;
        Ok(Self::proposed_capture_sequence_splits(&facts, &declined))
    }

    /// The boundaries the user has answered "same card" to, as a lookup set.
    fn declined_split_boundaries(&self, project_id: &str) -> Result<BTreeSet<(String, u64)>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT capture_sequence_id, at_counter FROM declined_capture_sequence_splits WHERE project_id=?1",
        )?;
        let rows = statement.query_map([project_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
        })?;
        Ok(rows.collect::<rusqlite::Result<BTreeSet<_>>>()?)
    }

    /// Every "same card" answer in the project, newest boundary last.
    pub fn declined_capture_sequence_splits(&self, project_id: &str) -> Result<Vec<DeclinedSplit>> {
        self.require_project(project_id)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT capture_sequence_id, at_counter, reason, declined_at \
             FROM declined_capture_sequence_splits WHERE project_id=?1 \
             ORDER BY capture_sequence_id, at_counter",
        )?;
        let rows = statement.query_map([project_id], |row| {
            Ok(DeclinedSplit {
                capture_sequence_id: row.get(0)?,
                at_counter: row.get::<_, i64>(1)? as u64,
                reason: row.get(2)?,
                declined_at: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Answers "no, that counter restart is not a card change" for one boundary.
    ///
    /// Nothing about the solve changes — the clips were already in one sequence
    /// and stay there. What changes is that the desk stops asking.
    pub fn decline_capture_sequence_split(
        &self,
        project_id: &str,
        capture_sequence_id: &str,
        at_counter: u64,
        reason: &str,
    ) -> Result<()> {
        self.require_project(project_id)?;
        self.commit_local(
            project_id,
            vec![NewOperation {
                aggregate_type: "captureSequence",
                aggregate_id: capture_sequence_id.to_string(),
                operation_type: CAPTURE_SEQUENCE_SPLIT_DECLINED,
                base_revision: None,
                payload: serde_json::to_value(CaptureSequenceSplitDeclinedPayload {
                    at_counter,
                    reason: reason.trim().to_string(),
                })?,
            }],
        )?;
        Ok(())
    }

    /// Reverses [`Catalog::decline_capture_sequence_split`], so the boundary is
    /// proposed again.
    pub fn restore_capture_sequence_split(
        &self,
        project_id: &str,
        capture_sequence_id: &str,
        at_counter: u64,
    ) -> Result<()> {
        self.require_project(project_id)?;
        self.commit_local(
            project_id,
            vec![NewOperation {
                aggregate_type: "captureSequence",
                aggregate_id: capture_sequence_id.to_string(),
                operation_type: CAPTURE_SEQUENCE_SPLIT_RESTORED,
                base_revision: None,
                payload: serde_json::to_value(CaptureSequenceSplitRestoredPayload { at_counter })?,
            }],
        )?;
        Ok(())
    }

    /// Loads chronology evidence once, then derives every surface read from
    /// that immutable value. A concurrent write lands either before or after
    /// this snapshot, never between its facts, solution, and proposals.
    pub fn chronology_snapshot(&self, project_id: &str) -> Result<ChronologySnapshot> {
        self.require_project(project_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let facts = super::load_chronology_facts(&transaction, project_id)?;
        let solution = solve(&facts);
        let mut declined_statement = transaction.prepare(
            "SELECT capture_sequence_id, at_counter, reason, declined_at \
             FROM declined_capture_sequence_splits WHERE project_id=?1 \
             ORDER BY capture_sequence_id, at_counter",
        )?;
        let declined_splits = declined_statement
            .query_map([project_id], |row| {
                Ok(DeclinedSplit {
                    capture_sequence_id: row.get(0)?,
                    at_counter: row.get::<_, i64>(1)? as u64,
                    reason: row.get(2)?,
                    declined_at: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(declined_statement);
        let declined_lookup: BTreeSet<(String, u64)> = declined_splits
            .iter()
            .map(|entry| (entry.capture_sequence_id.clone(), entry.at_counter))
            .collect();
        let proposed_splits = Self::proposed_capture_sequence_splits(&facts, &declined_lookup);
        transaction.commit()?;
        Ok(ChronologySnapshot {
            facts,
            solution,
            proposed_splits,
            declined_splits,
        })
    }

    // --- D01-T010: clock epochs --------------------------------------------

    fn prepare_clock_epoch(
        facts: &super::model::ChronologyFacts,
        epoch: &ClockEpoch,
    ) -> Result<ClockEpoch> {
        if let Some(end) = epoch.end_counter
            && end < epoch.start_counter
        {
            return Err(ChronologyError::InvalidInterval(format!(
                "clock epoch end counter {end} precedes its start counter {}",
                epoch.start_counter
            ))
            .into());
        }
        if !facts
            .sequences
            .iter()
            .any(|sequence| sequence.id == epoch.capture_sequence_id)
        {
            return Err(ChronologyError::InvalidReference(format!(
                "capture sequence {} is not in this project",
                epoch.capture_sequence_id
            ))
            .into());
        }
        let mut stored = epoch.clone();
        if stored.id.is_empty() {
            stored.id = format!("epoch_{}", Uuid::new_v4().simple());
        }
        Ok(stored)
    }

    /// Sets a clock epoch over a counter range in one capture sequence.
    /// The range is `[start_counter, end_counter]` inclusive; a `None` end is
    /// open-ended. An epoch never reaches across capture sequences.
    pub fn set_clock_epoch(&self, project_id: &str, epoch: &ClockEpoch) -> Result<ClockEpoch> {
        let preview = self.preview_clock_epoch_impact(project_id, epoch)?;
        self.apply_clock_epoch_preview(project_id, &preview)
    }

    /// Clears a clock epoch. What remains is whatever the other epochs imply —
    /// clearing does not restore a snapshot.
    pub fn clear_clock_epoch(&self, project_id: &str, epoch_id: &str) -> Result<()> {
        let preview = self.preview_clear_clock_epoch_impact(project_id, epoch_id)?;
        self.apply_clear_clock_epoch_preview(project_id, &preview)
    }

    // --- D01-T011: clip time exceptions ------------------------------------

    /// Overrides one clip's trip time, breaking it out of its capture
    /// sequence. Rides the `projectAsset` aggregate, so it inherits that
    /// aggregate's revision guard.
    pub fn set_clip_time_exception(
        &self,
        asset_id: &str,
        trip_time: &str,
        reason: &str,
    ) -> Result<ClipTimeException> {
        let asset = self.get_asset(asset_id)?;
        if parse_utc(trip_time).is_none() {
            return Err(ChronologyError::InvalidInterval(format!(
                "{trip_time} is not an RFC 3339 timestamp"
            ))
            .into());
        }
        self.commit_local(
            &asset.project_id,
            vec![NewOperation {
                aggregate_type: "projectAsset",
                aggregate_id: asset_id.to_string(),
                operation_type: CLIP_TIME_EXCEPTION_SET,
                base_revision: None,
                payload: serde_json::to_value(ClipTimeExceptionSetPayload {
                    trip_time: trip_time.to_string(),
                    reason: reason.trim().to_string(),
                })?,
            }],
        )?;
        Ok(ClipTimeException {
            asset_id: asset_id.to_string(),
            trip_time: trip_time.to_string(),
            reason: reason.trim().to_string(),
        })
    }

    /// Removes a clip's time exception, returning it to whatever its capture
    /// sequence and epochs imply.
    pub fn clear_clip_time_exception(&self, asset_id: &str) -> Result<()> {
        let asset = self.get_asset(asset_id)?;
        self.commit_local(
            &asset.project_id,
            vec![NewOperation {
                aggregate_type: "projectAsset",
                aggregate_id: asset_id.to_string(),
                operation_type: CLIP_TIME_EXCEPTION_CLEARED,
                base_revision: None,
                payload: serde_json::json!({}),
            }],
        )?;
        Ok(())
    }

    // --- D01-T012: trip phases ---------------------------------------------

    fn prepare_trip_phase(&self, project_id: &str, phase: &TripPhase) -> Result<TripPhase> {
        self.require_project(project_id)?;
        for candidate in [
            phase.timezone.as_deref(),
            phase.origin_timezone.as_deref(),
            phase.destination_timezone.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if !is_valid_timezone(candidate) {
                return Err(ChronologyError::InvalidTimezone(candidate.to_string()).into());
            }
        }
        let mut stored = phase.clone();
        if stored.start_anchor_asset_id.is_some() || stored.end_anchor_asset_id.is_some() {
            let solution = self.solve_chronology(project_id)?;
            let resolve = |label: &str, asset_id: &str| -> Result<String> {
                solution
                    .assets
                    .iter()
                    .find(|asset| asset.asset_id == asset_id)
                    .and_then(|asset| asset.trip_time.clone())
                    .ok_or_else(|| {
                        ChronologyError::InvalidReference(format!(
                            "{label} phase anchor {asset_id} is not a placed clip in project {project_id}"
                        ))
                        .into()
                    })
            };
            if let Some(asset_id) = stored.start_anchor_asset_id.as_deref() {
                stored.start_trip_time = resolve("start", asset_id)?;
            }
            if let Some(asset_id) = stored.end_anchor_asset_id.as_deref() {
                stored.end_trip_time = resolve("end", asset_id)?;
            }
        }
        let (Some(start), Some(end)) = (
            parse_utc(&stored.start_trip_time),
            parse_utc(&stored.end_trip_time),
        ) else {
            return Err(ChronologyError::InvalidInterval(
                "trip phase bounds must be RFC 3339 timestamps".to_string(),
            )
            .into());
        };
        if end < start {
            return Err(ChronologyError::InvalidInterval(format!(
                "trip phase {} ends before it starts",
                stored.label
            ))
            .into());
        }

        if stored.id.is_empty() {
            stored.id = format!("phase_{}", Uuid::new_v4().simple());
        }
        Ok(stored)
    }

    /// Adds or replaces a trip phase. Timezones are validated as resolvable
    /// IANA identifiers *before* anything is written, so an invalid identifier
    /// can never reach the database.
    pub fn set_trip_phase(&self, project_id: &str, phase: &TripPhase) -> Result<TripPhase> {
        let stored = self.prepare_trip_phase(project_id, phase)?;
        let revision = current_revision(&self.connection()?, project_id, "tripPhase", &stored.id)?;
        self.commit_local(
            project_id,
            vec![trip_phase_set_operation(&stored, revision)?],
        )?;
        Ok(stored)
    }

    /// Atomically cuts one visible phase at a clip. The newly authored phase
    /// takes the requested side, while the old phase keeps the other side. If
    /// the new side covers the whole phase, the old aggregate is removed.
    /// Nothing is written until both resulting intervals validate.
    pub fn split_trip_phase(
        &self,
        project_id: &str,
        existing_phase_id: &str,
        boundary_asset_id: &str,
        direction: PhaseSplitDirection,
        new_phase: &TripPhase,
    ) -> Result<TripPhase> {
        self.require_project(project_id)?;
        let solution = self.solve_chronology(project_id)?;
        let covered_indices: Vec<usize> = solution
            .assets
            .iter()
            .enumerate()
            .filter_map(|(index, asset)| {
                (asset
                    .display_time
                    .as_ref()
                    .and_then(|display| display.phase_id.as_deref())
                    == Some(existing_phase_id))
                .then_some(index)
            })
            .collect();
        let Some(&first_index) = covered_indices.first() else {
            return Err(ChronologyError::InvalidReference(format!(
                "trip phase {existing_phase_id} has no visible clips to split"
            ))
            .into());
        };
        let last_index = *covered_indices
            .last()
            .expect("a non-empty phase has a last clip");
        if !covered_indices
            .windows(2)
            .all(|pair| pair[1] == pair[0] + 1)
        {
            return Err(ChronologyError::InvalidInterval(format!(
                "trip phase {existing_phase_id} is interrupted by overlapping context"
            ))
            .into());
        }
        let boundary_index = solution
            .assets
            .iter()
            .position(|asset| asset.asset_id == boundary_asset_id)
            .filter(|index| *index >= first_index && *index <= last_index)
            .ok_or_else(|| {
                ChronologyError::InvalidReference(format!(
                    "clip {boundary_asset_id} is not inside trip phase {existing_phase_id}"
                ))
            })?;

        let mut existing = self
            .trip_phases(project_id)?
            .into_iter()
            .find(|phase| phase.id == existing_phase_id)
            .ok_or_else(|| {
                ChronologyError::InvalidReference(format!(
                    "trip phase not found: {existing_phase_id}"
                ))
            })?;
        let mut authored = new_phase.clone();
        authored.id.clear();

        let asset_id_at = |index: usize| solution.assets[index].asset_id.clone();
        let keeps_existing = match direction {
            PhaseSplitDirection::Through => {
                authored.start_anchor_asset_id = Some(asset_id_at(first_index));
                authored.end_anchor_asset_id = Some(asset_id_at(boundary_index));
                if boundary_index < last_index {
                    existing.start_anchor_asset_id = Some(asset_id_at(boundary_index + 1));
                    existing.end_anchor_asset_id = Some(asset_id_at(last_index));
                    true
                } else {
                    false
                }
            }
            PhaseSplitDirection::From => {
                authored.start_anchor_asset_id = Some(asset_id_at(boundary_index));
                authored.end_anchor_asset_id = Some(asset_id_at(last_index));
                if boundary_index > first_index {
                    existing.start_anchor_asset_id = Some(asset_id_at(first_index));
                    existing.end_anchor_asset_id = Some(asset_id_at(boundary_index - 1));
                    true
                } else {
                    false
                }
            }
        };

        let authored = self.prepare_trip_phase(project_id, &authored)?;
        let connection = self.connection()?;
        let existing_revision =
            current_revision(&connection, project_id, "tripPhase", existing_phase_id)?;
        let mut operations = Vec::with_capacity(2);
        if keeps_existing {
            let existing = self.prepare_trip_phase(project_id, &existing)?;
            operations.push(trip_phase_set_operation(&existing, existing_revision)?);
        } else {
            operations.push(NewOperation {
                aggregate_type: "tripPhase",
                aggregate_id: existing_phase_id.to_string(),
                operation_type: TRIP_PHASE_REMOVED,
                base_revision: Some(existing_revision),
                payload: serde_json::to_value(TripPhaseRemovedPayload {
                    phase_id: existing_phase_id.to_string(),
                })?,
            });
        }
        let authored_revision =
            current_revision(&connection, project_id, "tripPhase", &authored.id)?;
        operations.push(trip_phase_set_operation(&authored, authored_revision)?);
        self.commit_local(project_id, operations)?;
        Ok(authored)
    }

    /// Removes a trip phase. No asset is retimed and none is deleted: the
    /// assets it covered simply become uncovered and display as UTC.
    pub fn remove_trip_phase(&self, project_id: &str, phase_id: &str) -> Result<()> {
        self.require_project(project_id)?;
        let revision = current_revision(&self.connection()?, project_id, "tripPhase", phase_id)?;
        self.commit_local(
            project_id,
            vec![NewOperation {
                aggregate_type: "tripPhase",
                aggregate_id: phase_id.to_string(),
                operation_type: TRIP_PHASE_REMOVED,
                base_revision: Some(revision),
                payload: serde_json::to_value(TripPhaseRemovedPayload {
                    phase_id: phase_id.to_string(),
                })?,
            }],
        )?;
        Ok(())
    }

    /// The trip phases of one project, in a deterministic order.
    pub fn trip_phases(&self, project_id: &str) -> Result<Vec<TripPhase>> {
        let facts = self.chronology_facts(project_id)?;
        let solution = solve(&facts);
        let (mut phases, _) = resolve_anchored_phases(&facts.phases, &solution.assets);
        phases.sort_by(|left, right| {
            left.start_trip_time
                .cmp(&right.start_trip_time)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(phases)
    }

    // --- D01-T013: constraints and contradictions --------------------------

    /// Records that one clip must come before another.
    ///
    /// Refuses with `ChronologyError::HardContradiction` when the constraint
    /// would create a hard contradiction. Nothing is written in that case: the
    /// check runs against a candidate fact set built in memory, so a refused
    /// constraint leaves the catalog byte-identical.
    pub fn set_event_order_constraint(
        &self,
        project_id: &str,
        earlier_asset_id: &str,
        later_asset_id: &str,
        strength: ConstraintStrength,
        source: &str,
    ) -> Result<EventOrderConstraint> {
        self.require_project(project_id)?;
        if earlier_asset_id == later_asset_id {
            return Err(ChronologyError::InvalidReference(
                "a clip cannot be constrained to precede itself".to_string(),
            )
            .into());
        }
        let facts = self.chronology_facts(project_id)?;
        for asset_id in [earlier_asset_id, later_asset_id] {
            if !facts.assets.iter().any(|asset| asset.asset_id == asset_id) {
                return Err(ChronologyError::InvalidReference(format!(
                    "asset {asset_id} is not in this project"
                ))
                .into());
            }
        }

        let constraint = EventOrderConstraint {
            id: format!("constraint_{}", Uuid::new_v4().simple()),
            earlier_asset_id: earlier_asset_id.to_string(),
            later_asset_id: later_asset_id.to_string(),
            strength,
            source: source.trim().to_string(),
        };

        // Hard contradictions block apply and name the conflicting evidence.
        // A soft constraint is a hint and can never block.
        if strength == ConstraintStrength::Hard {
            let mut candidate = facts.clone();
            candidate.constraints.retain(|existing| {
                !(existing.earlier_asset_id == earlier_asset_id
                    && existing.later_asset_id == later_asset_id)
            });
            candidate.constraints.push(constraint.clone());
            let introduced: Vec<Contradiction> = solve(&candidate)
                .contradictions
                .into_iter()
                .filter(|found| found.kind == ContradictionKind::Cycle)
                .collect();
            if let Some(blocking) = introduced.first() {
                return Err(ChronologyError::HardContradiction(blocking.message.clone()).into());
            }
        }

        let revision = current_revision(
            &self.connection()?,
            project_id,
            "eventOrderConstraint",
            &constraint.id,
        )?;
        self.commit_local(
            project_id,
            vec![NewOperation {
                aggregate_type: "eventOrderConstraint",
                aggregate_id: constraint.id.clone(),
                operation_type: EVENT_ORDER_CONSTRAINT_SET,
                base_revision: Some(revision),
                payload: serde_json::to_value(EventOrderConstraintSetPayload {
                    constraint_id: constraint.id.clone(),
                    earlier_asset_id: constraint.earlier_asset_id.clone(),
                    later_asset_id: constraint.later_asset_id.clone(),
                    strength: strength_str(strength).to_string(),
                    source: constraint.source.clone(),
                })?,
            }],
        )?;
        Ok(constraint)
    }

    /// Removes an event-order constraint. A no-op when it does not exist.
    pub fn clear_event_order_constraint(
        &self,
        project_id: &str,
        constraint_id: &str,
    ) -> Result<()> {
        self.require_project(project_id)?;
        let revision = current_revision(
            &self.connection()?,
            project_id,
            "eventOrderConstraint",
            constraint_id,
        )?;
        self.commit_local(
            project_id,
            vec![NewOperation {
                aggregate_type: "eventOrderConstraint",
                aggregate_id: constraint_id.to_string(),
                operation_type: EVENT_ORDER_CONSTRAINT_CLEARED,
                base_revision: Some(revision),
                payload: serde_json::to_value(EventOrderConstraintClearedPayload {
                    constraint_id: constraint_id.to_string(),
                })?,
            }],
        )?;
        Ok(())
    }

    /// The contradictions currently present in a project's chronology.
    pub fn contradictions(&self, project_id: &str) -> Result<Vec<Contradiction>> {
        Ok(self.solve_chronology(project_id)?.contradictions)
    }

    /// The contradictions a candidate epoch *would* create, computed without
    /// writing anything. This is the calibration impact preview: the user sees
    /// the consequence before confirming the change.
    pub fn preview_clock_epoch(
        &self,
        project_id: &str,
        candidate: &ClockEpoch,
    ) -> Result<Vec<Contradiction>> {
        Ok(self
            .preview_clock_epoch_impact(project_id, candidate)?
            .impact
            .contradictions)
    }

    /// Exact before/after placement for a candidate epoch, still without a
    /// write. A blank candidate identity is allocated in the returned epoch;
    /// confirmation must apply that exact returned value.
    pub fn preview_clock_epoch_impact(
        &self,
        project_id: &str,
        candidate: &ClockEpoch,
    ) -> Result<ClockEpochImpactPreview> {
        Self::clock_epoch_impact_from_facts(self.chronology_facts(project_id)?, candidate)
    }

    fn clock_epoch_impact_from_facts(
        mut facts: super::model::ChronologyFacts,
        candidate: &ClockEpoch,
    ) -> Result<ClockEpochImpactPreview> {
        let facts_token = chronology_facts_token(&facts)?;
        let before = solve(&facts);
        let epoch = Self::prepare_clock_epoch(&facts, candidate)?;
        facts.epochs.retain(|existing| existing.id != epoch.id);
        facts.epochs.push(epoch.clone());
        Ok(ClockEpochImpactPreview {
            epoch,
            facts_token,
            impact: chronology_impact(&before, solve(&facts)),
        })
    }

    fn commit_clock_epoch(
        transaction: &rusqlite::Transaction<'_>,
        project_id: &str,
        preview: &ClockEpochImpactPreview,
    ) -> Result<ClockEpoch> {
        refuse_hard_impact(&preview.impact)?;
        let epoch = &preview.epoch;
        let revision = current_revision(transaction, project_id, "clockEpoch", &epoch.id)?;
        commit_local_in_transaction(
            transaction,
            project_id,
            vec![NewOperation {
                aggregate_type: "clockEpoch",
                aggregate_id: epoch.id.clone(),
                operation_type: CLOCK_EPOCH_SET,
                base_revision: Some(revision),
                payload: serde_json::to_value(ClockEpochSetPayload {
                    epoch_id: epoch.id.clone(),
                    capture_sequence_id: epoch.capture_sequence_id.clone(),
                    start_counter: epoch.start_counter,
                    end_counter: epoch.end_counter,
                    offset_seconds: epoch.offset_seconds,
                    label: epoch.label.clone(),
                    confidence: Some(epoch.confidence),
                    evidence: Some(epoch.evidence.clone()),
                })?,
            }],
        )?;
        Ok(epoch.clone())
    }

    /// Applies exactly the epoch candidate and solver impact returned by the
    /// preview. Any intervening chronology fact invalidates confirmation.
    pub fn apply_clock_epoch_preview(
        &self,
        project_id: &str,
        preview: &ClockEpochImpactPreview,
    ) -> Result<ClockEpoch> {
        self.require_project(project_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let facts = super::load_chronology_facts(&transaction, project_id)?;
        let expected = Self::clock_epoch_impact_from_facts(facts, &preview.epoch)?;
        if expected != *preview {
            return Err(ChronologyError::InvalidReference(
                "the clock correction preview is stale; preview it again before applying"
                    .to_string(),
            )
            .into());
        }
        let stored = Self::commit_clock_epoch(&transaction, project_id, &expected)?;
        Self::project_current_chronology_in_transaction(&transaction, project_id)?;
        transaction.commit()?;
        Ok(stored)
    }

    /// Exact before/after placement for removing one existing epoch, computed
    /// without writing. This deliberately removes the fact in memory rather
    /// than simulating removal with a zero offset, because an overlapping
    /// broader epoch may become visible underneath it.
    pub fn preview_clear_clock_epoch_impact(
        &self,
        project_id: &str,
        epoch_id: &str,
    ) -> Result<ClockEpochClearPreview> {
        Self::clear_clock_epoch_impact_from_facts(self.chronology_facts(project_id)?, epoch_id)
    }

    fn clear_clock_epoch_impact_from_facts(
        mut facts: super::model::ChronologyFacts,
        epoch_id: &str,
    ) -> Result<ClockEpochClearPreview> {
        let facts_token = chronology_facts_token(&facts)?;
        let before = solve(&facts);
        let previous_len = facts.epochs.len();
        facts.epochs.retain(|epoch| epoch.id != epoch_id);
        if facts.epochs.len() == previous_len {
            return Err(ChronologyError::InvalidReference(format!(
                "clock epoch {epoch_id} is not in this project"
            ))
            .into());
        }
        Ok(ClockEpochClearPreview {
            epoch_id: epoch_id.to_string(),
            facts_token,
            impact: chronology_impact(&before, solve(&facts)),
        })
    }

    pub fn apply_clear_clock_epoch_preview(
        &self,
        project_id: &str,
        preview: &ClockEpochClearPreview,
    ) -> Result<()> {
        self.require_project(project_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let facts = super::load_chronology_facts(&transaction, project_id)?;
        let expected = Self::clear_clock_epoch_impact_from_facts(facts, &preview.epoch_id)?;
        if expected != *preview {
            return Err(ChronologyError::InvalidReference(
                "the removal preview is stale; preview it again before applying".to_string(),
            )
            .into());
        }
        refuse_hard_impact(&expected.impact)?;
        let revision = current_revision(&transaction, project_id, "clockEpoch", &preview.epoch_id)?;
        commit_local_in_transaction(
            &transaction,
            project_id,
            vec![NewOperation {
                aggregate_type: "clockEpoch",
                aggregate_id: preview.epoch_id.clone(),
                operation_type: CLOCK_EPOCH_CLEARED,
                base_revision: Some(revision),
                payload: serde_json::to_value(ClockEpochClearedPayload {
                    epoch_id: preview.epoch_id.clone(),
                })?,
            }],
        )?;
        Self::project_current_chronology_in_transaction(&transaction, project_id)?;
        transaction.commit()?;
        Ok(())
    }

    /// Translates a spatial "put this contiguous run here" gesture into one
    /// ordinary clock epoch. The desktop supplies identities and intent; the
    /// owning domain derives the counter range, offset, and impact.
    pub fn preview_visual_placement(
        &self,
        project_id: &str,
        asset_ids: &[String],
        target_asset_id: &str,
        side: VisualPlacementSide,
        label: &str,
    ) -> Result<VisualPlacementPreview> {
        if asset_ids.is_empty() {
            return Err(ChronologyError::InvalidReference(
                "select at least one clip to place".to_string(),
            )
            .into());
        }
        let facts = self.chronology_facts(project_id)?;
        let facts_token = chronology_facts_token(&facts)?;
        let before = solve(&facts);
        let selected: BTreeSet<&str> = asset_ids.iter().map(String::as_str).collect();
        if selected.len() != asset_ids.len() {
            return Err(ChronologyError::InvalidReference(
                "the selected run cannot contain the same clip more than once".to_string(),
            )
            .into());
        }
        if selected.contains(target_asset_id) {
            return Err(ChronologyError::InvalidReference(
                "the destination clip cannot be inside the selected run".to_string(),
            )
            .into());
        }
        let selected_positions: Vec<usize> = before
            .assets
            .iter()
            .enumerate()
            .filter_map(|(position, asset)| {
                selected
                    .contains(asset.asset_id.as_str())
                    .then_some(position)
            })
            .collect();
        if selected_positions.len() != selected.len() {
            return Err(ChronologyError::InvalidReference(
                "one or more selected clips are not in this project".to_string(),
            )
            .into());
        }
        let first_position = *selected_positions.first().unwrap_or(&0);
        let last_position = *selected_positions.last().unwrap_or(&first_position);
        if last_position - first_position + 1 != selected_positions.len() {
            return Err(ChronologyError::InvalidReference(
                "the selected clips must be one contiguous run".to_string(),
            )
            .into());
        }
        let target = before
            .assets
            .iter()
            .find(|asset| asset.asset_id == target_asset_id)
            .ok_or_else(|| {
                ChronologyError::InvalidReference(format!(
                    "destination clip {target_asset_id} is not in this project"
                ))
            })?;
        let selected_assets: Vec<_> = before.assets[first_position..=last_position]
            .iter()
            .collect();
        let sequence_id = selected_assets[0].order_key.capture_sequence_id.clone();
        if sequence_id.is_empty()
            || selected_assets
                .iter()
                .any(|asset| asset.order_key.capture_sequence_id != sequence_id)
        {
            return Err(ChronologyError::InvalidReference(
                "a visual placement must stay within one capture sequence".to_string(),
            )
            .into());
        }
        let counters: Vec<u64> = selected_assets
            .iter()
            .map(|asset| asset.order_key.filename_counter)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                ChronologyError::InvalidReference(
                    "the selected run has clips without filename counters".to_string(),
                )
            })?;
        let start_counter = *counters.iter().min().unwrap_or(&0);
        let end_counter = counters.iter().max().copied().unwrap_or(start_counter);
        let ranged_assets: BTreeSet<&str> = facts
            .assets
            .iter()
            .filter(|asset| asset.capture_sequence_id.as_deref() == Some(sequence_id.as_str()))
            .filter(|asset| {
                asset
                    .filename_counter
                    .is_some_and(|counter| counter >= start_counter && counter <= end_counter)
            })
            .map(|asset| asset.asset_id.as_str())
            .collect();
        if ranged_assets != selected {
            return Err(ChronologyError::InvalidReference(
                "the selected clips do not form one isolated filename-counter range; split or reassign the capture sequence first"
                    .to_string(),
            )
            .into());
        }

        let source_anchor = match side {
            VisualPlacementSide::Before => selected_assets.last().copied(),
            VisualPlacementSide::After => selected_assets.first().copied(),
        }
        .ok_or_else(|| {
            ChronologyError::InvalidReference("the selected run is empty".to_string())
        })?;
        let source_raw_time = parse_utc(source_anchor.raw_time.as_deref().ok_or_else(|| {
            ChronologyError::InvalidReference(
                "the selected run has no usable raw camera time".to_string(),
            )
        })?)
        .ok_or_else(|| {
            ChronologyError::InvalidInterval("selected clip raw time is invalid".to_string())
        })?;
        let target_time = parse_utc(target.trip_time.as_deref().ok_or_else(|| {
            ChronologyError::InvalidReference("the destination clip has no placed time".to_string())
        })?)
        .ok_or_else(|| {
            ChronologyError::InvalidInterval("destination clip time is invalid".to_string())
        })?;
        let duration_for = |asset_id: &str| {
            facts
                .assets
                .iter()
                .find(|asset| asset.asset_id == asset_id)
                .map(|asset| asset.duration_seconds.max(0.0).ceil() as i64)
                .unwrap_or(0)
        };
        let desired = match side {
            VisualPlacementSide::Before => {
                target_time - Duration::seconds(duration_for(&source_anchor.asset_id) + 1)
            }
            VisualPlacementSide::After => {
                target_time + Duration::seconds(duration_for(&target.asset_id) + 1)
            }
        };
        // A ClockEpoch stores an absolute correction from the immutable raw
        // camera timestamp. It is not a delta from whatever earlier epoch is
        // currently visible, so deriving against `trip_time` would double- or
        // under-correct a run that has already been calibrated.
        let offset_seconds = desired.signed_duration_since(source_raw_time).num_seconds();
        let epoch = ClockEpoch {
            // Preview and confirmation must apply the same fact identity.
            // Besides making the operation auditable, this keeps the solver's
            // epoch-id tie-break identical on both sides of confirmation.
            id: format!("epoch_{}", Uuid::new_v4().simple()),
            capture_sequence_id: sequence_id,
            start_counter,
            end_counter: Some(end_counter),
            offset_seconds,
            confidence: 1.0,
            evidence: vec!["user_visual_placement".to_string()],
            label: label.trim().to_string(),
        };
        let mut candidate = facts.clone();
        candidate.epochs.push(epoch.clone());
        let after = solve(&candidate);
        // One epoch can represent the gesture only when it becomes the
        // governing correction for every selected clip. Clip exceptions and
        // narrower overlapping epochs outrank it; accepting such a preview
        // would claim the run was placed when confirmation could not do so.
        for selected_asset in facts
            .assets
            .iter()
            .filter(|asset| selected.contains(asset.asset_id.as_str()))
        {
            let raw = parse_utc(selected_asset.captured_at_raw.as_deref().ok_or_else(|| {
                ChronologyError::InvalidReference(format!(
                    "selected clip {} has no usable raw camera time",
                    selected_asset.asset_id
                ))
            })?)
            .ok_or_else(|| {
                ChronologyError::InvalidInterval(format!(
                    "selected clip {} has an invalid raw camera time",
                    selected_asset.asset_id
                ))
            })?;
            let actual = after
                .assets
                .iter()
                .find(|asset| asset.asset_id == selected_asset.asset_id)
                .and_then(|asset| asset.trip_time.as_deref())
                .and_then(parse_utc);
            if actual != Some(raw + Duration::seconds(offset_seconds)) {
                return Err(ChronologyError::InvalidReference(
                    "the selected run has a clip exception or overlapping correction; resolve it before visual placement"
                        .to_string(),
                )
                .into());
            }
        }
        let impact = chronology_impact(&before, after);
        Ok(VisualPlacementPreview {
            epoch,
            facts_token,
            impact,
        })
    }
}
