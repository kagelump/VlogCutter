//! Chronology explanation and review-queue stability (D01-T014).
//!
//! Two problems, one module:
//!
//! - **Why is this clip here?** The solver derives a time from several facts;
//!   the user needs to see which ones, by identity, so they can navigate to the
//!   thing that caused it. The explanation is derived, never stored — storing it
//!   would let it drift from the solution it describes.
//! - **Where was I?** Applying a correction can reorder hundreds of clips. A
//!   queue position anchored to an *index* is meaningless afterwards, which is
//!   the entire problem. It is anchored to an asset id instead.

use super::model::ConstraintStrength;
use super::phases::DisplayTime;
use super::solver::{Contradiction, solve};
use crate::Catalog;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

/// One fact that moved a clip's time, named by its identity so a caller can
/// navigate to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum AppliedCorrection {
    CaptureSequence {
        sequence_id: String,
        label: String,
    },
    ClockEpoch {
        epoch_id: String,
        offset_seconds: i64,
        label: String,
    },
    ClipException {
        trip_time: String,
        reason: String,
    },
    TripPhase {
        phase_id: String,
        timezone: String,
        label: String,
    },
    EventOrder {
        constraint_id: String,
        strength: ConstraintStrength,
    },
}

/// Everything the solver concluded about one clip, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChronologyExplanation {
    pub asset_id: String,
    pub raw_time: Option<String>,
    pub trip_time: Option<String>,
    pub display_time: Option<DisplayTime>,
    pub confidence: f32,
    pub uncertainty_seconds: u32,
    pub applied_corrections: Vec<AppliedCorrection>,
    pub contradictions: Vec<Contradiction>,
}

/// A place in the review queue that survives a reorder.
///
/// `ordered_predecessors` is bounded to the ten assets preceding the anchor,
/// which is enough to find a surviving neighbour without carrying the whole
/// queue around.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuePosition {
    pub anchor_asset_id: String,
    pub ordered_predecessors: Vec<String>,
}

/// How many predecessors a captured position carries.
const PREDECESSOR_DEPTH: usize = 10;

impl Catalog {
    /// The review queue: the solved total order with removed clips dropped.
    ///
    /// "No longer in the queue" is what the user actually experiences, and a
    /// removed clip is not in it even though it is still in the catalog and
    /// still in the solution.
    fn review_queue(&self, project_id: &str) -> Result<Vec<String>> {
        let mut removed: Vec<String> = self
            .list_assets(project_id)?
            .into_iter()
            .filter(|asset| asset.removed)
            .map(|asset| asset.id)
            .collect();
        removed.sort();
        Ok(self
            .solve_chronology(project_id)?
            .assets
            .into_iter()
            .map(|entry| entry.asset_id)
            .filter(|asset_id| removed.binary_search(asset_id).is_err())
            .collect())
    }

    /// Explains one clip's derived time in terms of the facts that produced it.
    ///
    /// Corrections are listed in *application* order — the order the solver's
    /// pipeline consults them (`PRD.md` §9.5): capture sequence, clock epoch,
    /// clip exception, trip phase, then event-order constraints.
    pub fn explain_chronology(&self, asset_id: &str) -> Result<ChronologyExplanation> {
        let asset = self.get_asset(asset_id)?;
        let facts = self.chronology_facts(&asset.project_id)?;
        let solution = solve(&facts);
        let solved = solution
            .assets
            .iter()
            .find(|entry| entry.asset_id == asset_id)
            .ok_or_else(|| anyhow!("asset {asset_id} is not in the chronology solution"))?;

        let hypothesis = facts
            .assets
            .iter()
            .find(|entry| entry.asset_id == asset_id)
            .ok_or_else(|| anyhow!("asset {asset_id} has no chronology hypothesis"))?;

        let mut applied: Vec<AppliedCorrection> = Vec::new();

        if let Some(sequence_id) = hypothesis.capture_sequence_id.as_deref()
            && let Some(sequence) = facts
                .sequences
                .iter()
                .find(|candidate| candidate.id == sequence_id)
        {
            applied.push(AppliedCorrection::CaptureSequence {
                sequence_id: sequence.id.clone(),
                label: sequence.label.clone(),
            });
        }

        // The epoch actually governing this clip, by the same rule the solver
        // used — an explanation that named a different epoch would be worse
        // than no explanation.
        if let Some(sequence_id) = hypothesis.capture_sequence_id.as_deref()
            && let Some(epoch) = super::solver::governing_epoch_for(
                &facts.epochs,
                sequence_id,
                hypothesis.filename_counter,
            )
        {
            applied.push(AppliedCorrection::ClockEpoch {
                epoch_id: epoch.id.clone(),
                offset_seconds: epoch.offset_seconds,
                label: epoch.label.clone(),
            });
        }

        if let Some(exception) = facts
            .exceptions
            .iter()
            .find(|candidate| candidate.asset_id == asset_id)
        {
            applied.push(AppliedCorrection::ClipException {
                trip_time: exception.trip_time.clone(),
                reason: exception.reason.clone(),
            });
        }

        if let Some(display) = solved.display_time.as_ref()
            && let Some(phase_id) = display.phase_id.as_deref()
            && let Some(phase) = facts
                .phases
                .iter()
                .find(|candidate| candidate.id == phase_id)
        {
            applied.push(AppliedCorrection::TripPhase {
                phase_id: phase.id.clone(),
                timezone: display.timezone.clone(),
                label: phase.label.clone(),
            });
        }

        // Constraints in a deterministic order, since several can touch one
        // clip and the solution's own ordering must be reproducible.
        let mut touching: Vec<_> = facts
            .constraints
            .iter()
            .filter(|constraint| {
                constraint.earlier_asset_id == asset_id || constraint.later_asset_id == asset_id
            })
            .collect();
        touching.sort_by(|left, right| left.id.cmp(&right.id));
        for constraint in touching {
            applied.push(AppliedCorrection::EventOrder {
                constraint_id: constraint.id.clone(),
                strength: constraint.strength,
            });
        }

        let contradictions = solution
            .contradictions
            .into_iter()
            .filter(|found| found.asset_ids.iter().any(|id| id == asset_id))
            .collect();

        Ok(ChronologyExplanation {
            asset_id: asset_id.to_string(),
            raw_time: solved.raw_time.clone(),
            trip_time: solved.trip_time.clone(),
            display_time: solved.display_time.clone(),
            confidence: solved.confidence,
            uncertainty_seconds: solved.uncertainty_seconds,
            applied_corrections: applied,
            contradictions,
        })
    }

    /// Captures where the user is in the review queue, *before* applying a
    /// change. Capturing afterwards captures the damage.
    pub fn capture_queue_position(
        &self,
        project_id: &str,
        current_asset_id: &str,
    ) -> Result<QueuePosition> {
        let ordered = self.review_queue(project_id)?;
        let anchor = ordered
            .iter()
            .position(|candidate| candidate == current_asset_id);
        let predecessors = match anchor {
            Some(index) => ordered[index.saturating_sub(PREDECESSOR_DEPTH)..index]
                .iter()
                .rev()
                .cloned()
                .collect(),
            None => Vec::new(),
        };
        Ok(QueuePosition {
            anchor_asset_id: current_asset_id.to_string(),
            ordered_predecessors: predecessors,
        })
    }

    /// Resumes the review queue after a change, returning the asset to land on.
    ///
    /// The anchor wins when it survived. Otherwise the nearest surviving
    /// predecessor by the *pre-change* order wins, which matches the
    /// `nextSelection` rule frozen in D01-T003. When nothing survived, the
    /// first asset in the new queue is the honest answer — the same
    /// first-surviving-asset fallback the desktop already uses.
    pub fn restore_queue_position(
        &self,
        project_id: &str,
        position: &QueuePosition,
    ) -> Result<String> {
        let ordered = self.review_queue(project_id)?;
        if ordered.contains(&position.anchor_asset_id) {
            return Ok(position.anchor_asset_id.clone());
        }
        for candidate in &position.ordered_predecessors {
            if ordered.contains(candidate) {
                return Ok(candidate.clone());
            }
        }
        ordered
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("project {project_id} has no assets to resume on"))
    }
}
