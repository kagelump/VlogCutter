//! The chronology write surface: capture-sequence management (D01-T009),
//! clock epochs (D01-T010), clip time exceptions (D01-T011), trip phases
//! (D01-T012), and event-order constraints with contradiction reporting
//! (D01-T013).
//!
//! Every method here commits an ordinary typed operation through the same
//! `commit_local` path as the rest of the catalog, so chronology state is never
//! a parallel model. Nothing here writes `assets.best_time` or
//! `assets.offset_seconds`; those columns belong exclusively to the solver's
//! projection helper (`CHRONOLOGY_MODEL.md`, "Evidence only").

use super::model::{
    CaptureSequence, ClipTimeException, ClockEpoch, ConstraintStrength, DisplayMode,
    EventOrderConstraint, PhaseKind, TripPhase,
};
use super::phases::{is_valid_timezone, parse_utc};
use super::solver::{Contradiction, ContradictionKind, solve};
use crate::Catalog;
use crate::operations::{
    CAPTURE_SEQUENCE_DECLARED, CAPTURE_SEQUENCE_MERGED, CAPTURE_SEQUENCE_SPLIT,
    CLIP_TIME_EXCEPTION_CLEARED, CLIP_TIME_EXCEPTION_SET, CLOCK_EPOCH_CLEARED, CLOCK_EPOCH_SET,
    CaptureSequenceDeclaredPayload, CaptureSequenceMergedPayload, CaptureSequenceSplitPayload,
    ClipTimeExceptionSetPayload, ClockEpochClearedPayload, ClockEpochSetPayload,
    EVENT_ORDER_CONSTRAINT_CLEARED, EVENT_ORDER_CONSTRAINT_SET, EventOrderConstraintClearedPayload,
    EventOrderConstraintSetPayload, NewOperation, TRIP_PHASE_REMOVED, TRIP_PHASE_SET,
    TripPhaseRemovedPayload, TripPhaseSetPayload, current_revision,
};
use anyhow::Result;
use thiserror::Error;
use uuid::Uuid;

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

fn strength_str(strength: ConstraintStrength) -> &'static str {
    match strength {
        ConstraintStrength::Hard => "hard",
        ConstraintStrength::Soft => "soft",
    }
}

/// A capture-sequence split the evidence suggests but the user must confirm.
/// No chronology change is applied without explicit user confirmation
/// (architecture contract), so this is a proposal and nothing more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitProposal {
    pub capture_sequence_id: String,
    pub at_counter: u64,
    pub reason: String,
    pub affected_asset_ids: Vec<String>,
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
        self.require_project(project_id)?;
        if from_sequence_id == into_sequence_id {
            return Err(ChronologyError::InvalidReference(
                "a capture sequence cannot be merged into itself".to_string(),
            )
            .into());
        }
        // The merge rides the *destination* aggregate: it is the sequence that
        // survives and gains rows, so it is the one whose revision must guard
        // the change.
        let revision = current_revision(
            &self.connection()?,
            project_id,
            "captureSequence",
            into_sequence_id,
        )?;
        self.commit_local(
            project_id,
            vec![NewOperation {
                aggregate_type: "captureSequence",
                aggregate_id: into_sequence_id.to_string(),
                operation_type: CAPTURE_SEQUENCE_MERGED,
                base_revision: Some(revision),
                payload: serde_json::to_value(CaptureSequenceMergedPayload {
                    from_sequence_id: from_sequence_id.to_string(),
                    into_sequence_id: into_sequence_id.to_string(),
                })?,
            }],
        )?;
        Ok(())
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
    /// drops is the second card starting over. The proposal names the counter
    /// the new card resumes at, which is exactly what `split_capture_sequence`
    /// takes.
    pub fn propose_capture_sequence_splits(&self, project_id: &str) -> Result<Vec<SplitProposal>> {
        let facts = self.chronology_facts(project_id)?;
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

            let mut boundary: Option<u64> = None;
            for pair in timeline.windows(2) {
                if pair[1].1 < pair[0].1 {
                    boundary = Some(pair[1].1);
                    break;
                }
            }
            let Some(at_counter) = boundary else {
                continue;
            };
            let mut affected: Vec<String> = timeline
                .iter()
                .filter(|(_, counter, _)| *counter >= at_counter)
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
        Ok(proposals)
    }

    // --- D01-T010: clock epochs --------------------------------------------

    /// Sets a clock epoch over a counter range in one capture sequence.
    /// The range is `[start_counter, end_counter]` inclusive; a `None` end is
    /// open-ended. An epoch never reaches across capture sequences.
    pub fn set_clock_epoch(&self, project_id: &str, epoch: &ClockEpoch) -> Result<ClockEpoch> {
        self.require_project(project_id)?;
        if let Some(end) = epoch.end_counter
            && end < epoch.start_counter
        {
            return Err(ChronologyError::InvalidInterval(format!(
                "clock epoch end counter {end} precedes its start counter {}",
                epoch.start_counter
            ))
            .into());
        }
        let mut stored = epoch.clone();
        if stored.id.is_empty() {
            stored.id = format!("epoch_{}", Uuid::new_v4().simple());
        }
        let revision = current_revision(&self.connection()?, project_id, "clockEpoch", &stored.id)?;
        self.commit_local(
            project_id,
            vec![NewOperation {
                aggregate_type: "clockEpoch",
                aggregate_id: stored.id.clone(),
                operation_type: CLOCK_EPOCH_SET,
                base_revision: Some(revision),
                payload: serde_json::to_value(ClockEpochSetPayload {
                    epoch_id: stored.id.clone(),
                    capture_sequence_id: stored.capture_sequence_id.clone(),
                    start_counter: stored.start_counter,
                    end_counter: stored.end_counter,
                    offset_seconds: stored.offset_seconds,
                    label: stored.label.clone(),
                    confidence: Some(stored.confidence),
                    evidence: Some(stored.evidence.clone()),
                })?,
            }],
        )?;
        Ok(stored)
    }

    /// Clears a clock epoch. What remains is whatever the other epochs imply —
    /// clearing does not restore a snapshot.
    pub fn clear_clock_epoch(&self, project_id: &str, epoch_id: &str) -> Result<()> {
        self.require_project(project_id)?;
        let revision = current_revision(&self.connection()?, project_id, "clockEpoch", epoch_id)?;
        self.commit_local(
            project_id,
            vec![NewOperation {
                aggregate_type: "clockEpoch",
                aggregate_id: epoch_id.to_string(),
                operation_type: CLOCK_EPOCH_CLEARED,
                base_revision: Some(revision),
                payload: serde_json::to_value(ClockEpochClearedPayload {
                    epoch_id: epoch_id.to_string(),
                })?,
            }],
        )?;
        Ok(())
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

    /// Adds or replaces a trip phase. Timezones are validated as resolvable
    /// IANA identifiers *before* anything is written, so an invalid identifier
    /// can never reach the database.
    pub fn set_trip_phase(&self, project_id: &str, phase: &TripPhase) -> Result<TripPhase> {
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
        let (Some(start), Some(end)) = (
            parse_utc(&phase.start_trip_time),
            parse_utc(&phase.end_trip_time),
        ) else {
            return Err(ChronologyError::InvalidInterval(
                "trip phase bounds must be RFC 3339 timestamps".to_string(),
            )
            .into());
        };
        if end < start {
            return Err(ChronologyError::InvalidInterval(format!(
                "trip phase {} ends before it starts",
                phase.label
            ))
            .into());
        }

        let mut stored = phase.clone();
        if stored.id.is_empty() {
            stored.id = format!("phase_{}", Uuid::new_v4().simple());
        }
        let revision = current_revision(&self.connection()?, project_id, "tripPhase", &stored.id)?;
        self.commit_local(
            project_id,
            vec![NewOperation {
                aggregate_type: "tripPhase",
                aggregate_id: stored.id.clone(),
                operation_type: TRIP_PHASE_SET,
                base_revision: Some(revision),
                payload: serde_json::to_value(TripPhaseSetPayload {
                    phase_id: stored.id.clone(),
                    label: stored.label.clone(),
                    phase_kind: phase_kind_str(stored.kind).to_string(),
                    start_trip_time: stored.start_trip_time.clone(),
                    end_trip_time: stored.end_trip_time.clone(),
                    timezone: stored.timezone.clone(),
                    origin_timezone: stored.origin_timezone.clone(),
                    destination_timezone: stored.destination_timezone.clone(),
                    display_mode: display_mode_str(stored.display_mode).to_string(),
                })?,
            }],
        )?;
        Ok(stored)
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
        let mut phases = self.chronology_facts(project_id)?.phases;
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
        let mut facts = self.chronology_facts(project_id)?;
        facts.epochs.retain(|existing| existing.id != candidate.id);
        facts.epochs.push(candidate.clone());
        Ok(solve(&facts).contradictions)
    }
}
