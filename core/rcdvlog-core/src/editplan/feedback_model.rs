//! Portable review facts keep observations, proposals, and human decisions distinct.
use super::diff::EditPlanDiff;
use super::{
    EditPlanAudio, EditPlanRevision, PreviewCutMapping, RangeDecisionOwner, ValidationReport,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ReviewFeedbackError {
    #[error("review feedback not found: {id}")]
    NotFound { id: String },
    #[error("review feedback changed: expected revision {expected}, actual {actual}")]
    StaleRevision { expected: u64, actual: u64 },
    #[error(
        "review target changed; explicitly map this note/proposal or record a new proposal before applying it"
    )]
    ChangedTarget,
    #[error("review proposal requires an explicit accepted human decision")]
    NotAccepted,
    #[error("multiple proposals change the same cut; combine them into one explicit proposal")]
    ConflictingChanges,
}
impl ReviewFeedbackError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "not_found",
            Self::StaleRevision { .. } => "review_feedback_stale_revision",
            Self::ChangedTarget => "review_feedback_changed_target",
            Self::NotAccepted => "review_feedback_not_accepted",
            Self::ConflictingChanges => "review_feedback_conflicting_changes",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewAuthor {
    Human,
    Reviewer,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewCategory {
    Endpoint,
    Order,
    Duration,
    FeaturedShot,
    SoundContinuity,
    Intent,
    Preference,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewCutSnapshot {
    pub operation_id: String,
    pub position: u32,
    pub asset_id: String,
    pub source_in_seconds: f64,
    pub source_out_seconds: f64,
    pub audio: Option<EditPlanAudio>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewMovieEvidence {
    pub artifact_id: String,
    pub output_fingerprint: String,
    pub movie_time_base_numerator: i64,
    pub movie_time_base_denominator: i64,
    pub movie_duration_seconds: f64,
    pub requested_movie_seconds: Option<f64>,
    /// Declared playback locator; never converted into a source boundary.
    pub timestamp_mapping: String,
    pub picture_mapping: PreviewCutMapping,
    /// Retains the selected representation identity without any local paths.
    pub source_fingerprints: Vec<ReviewSourceReference>,
    pub limitations: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewSourceReference {
    pub asset_id: String,
    pub source_fingerprint: String,
    pub representation_fingerprint: String,
    pub rotation_degrees: u16,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewTarget {
    pub revision_id: String,
    pub cut: ReviewCutSnapshot,
    pub previous_cut: Option<ReviewCutSnapshot>,
    pub next_cut: Option<ReviewCutSnapshot>,
    pub source_occurrences: u32,
    /// Declared 48 kHz sound events intersecting the picture-neighbor window.
    pub context_audio_digest: String,
    pub context_audio_precision: String,
    pub context_audio_event_count: u32,
    pub viewed_movie: Option<ReviewMovieEvidence>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordReviewNoteRequest {
    pub project_id: String,
    pub revision_id: String,
    pub operation_id: String,
    pub author: ReviewAuthor,
    pub category: ReviewCategory,
    pub observation: String,
    pub viewed_artifact_id: Option<String>,
    pub movie_time_seconds: Option<f64>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewNote {
    pub note_id: String,
    pub project_id: String,
    pub author: ReviewAuthor,
    pub category: ReviewCategory,
    pub observation: String,
    pub target: ReviewTarget,
    pub evidence_refs: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ReviewChange {
    Trim {
        source_in_seconds: f64,
        source_out_seconds: f64,
    },
    /// Also supports an explicit combined picture/audio update to the same asset.
    Replace {
        asset_id: String,
        source_in_seconds: f64,
        source_out_seconds: f64,
        audio: Option<EditPlanAudio>,
    },
    /// None appends the cut. Stable operation IDs define the ordering target.
    Reorder {
        before_operation_id: Option<String>,
    },
    Omit,
    /// None explicitly restores legacy linked-source audio.
    Audio {
        audio: Option<EditPlanAudio>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProposeReviewChangeRequest {
    pub note_id: String,
    pub expected_revision: u64,
    pub author: ReviewAuthor,
    pub change: ReviewChange,
    pub rationale: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewProposal {
    pub proposal_id: String,
    pub note_id: String,
    pub author: ReviewAuthor,
    pub target: ReviewTarget,
    pub change: ReviewChange,
    pub rationale: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewDecisionAction {
    Accepted,
    Deferred,
    Superseded,
    Addressed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewDecisionState {
    Accepted,
    Deferred,
    Superseded,
    Addressed,
    Applied,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewDecisionOrigin {
    Human,
    Agent,
    Host,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewDecisionActor {
    Human,
    Agent,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewDecisionAuthority {
    ExplicitHumanChoice,
    DelegatedEditing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewDecisionConfidence {
    Confirmed,
    Provisional,
}

/// Missing on historical facts: authorship cannot be reconstructed from a rationale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewDecisionProvenance {
    pub actor: ReviewDecisionActor,
    pub authority: ReviewDecisionAuthority,
    pub confidence: ReviewDecisionConfidence,
}

impl ReviewDecisionProvenance {
    pub fn human() -> Self {
        Self {
            actor: ReviewDecisionActor::Human,
            authority: ReviewDecisionAuthority::ExplicitHumanChoice,
            confidence: ReviewDecisionConfidence::Confirmed,
        }
    }
    pub fn agent() -> Self {
        Self {
            actor: ReviewDecisionActor::Agent,
            authority: ReviewDecisionAuthority::DelegatedEditing,
            confidence: ReviewDecisionConfidence::Provisional,
        }
    }
    pub(super) fn origin(&self) -> anyhow::Result<ReviewDecisionOrigin> {
        match (self.actor, self.authority, self.confidence) {
            (
                ReviewDecisionActor::Human,
                ReviewDecisionAuthority::ExplicitHumanChoice,
                ReviewDecisionConfidence::Confirmed,
            ) => Ok(ReviewDecisionOrigin::Human),
            (
                ReviewDecisionActor::Agent,
                ReviewDecisionAuthority::DelegatedEditing,
                ReviewDecisionConfidence::Provisional,
            ) => Ok(ReviewDecisionOrigin::Agent),
            _ => anyhow::bail!("invalid input: decision actor, authority and confidence disagree"),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewHardChoice {
    KeepPicture,
    KeepPictureAndAudio,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecideReviewFeedbackRequest {
    /// Omitted retains the legacy explicit decisionOwner=human assertion.
    #[serde(default)]
    pub provenance: Option<ReviewDecisionProvenance>,
    pub note_id: String,
    pub expected_revision: u64,
    pub proposal_id: Option<String>,
    #[serde(default)]
    pub decision_owner: Option<RangeDecisionOwner>,
    pub action: ReviewDecisionAction,
    pub rationale: String,
    /// Explicit retention of an existing cut; proposals are not permanent locks.
    pub hard_choice: Option<ReviewHardChoice>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewDecision {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ReviewDecisionProvenance>,
    pub decision_origin: ReviewDecisionOrigin,
    pub decision_id: String,
    pub note_id: String,
    pub proposal_id: Option<String>,
    pub state: ReviewDecisionState,
    pub rationale: String,
    pub hard_choice: Option<ReviewHardChoice>,
    pub retained_target: Option<ReviewTarget>,
    pub applied_revision_id: Option<String>,
    /// Applied is a host receipt of a prior human acceptance, not a new human vote.
    pub accepted_decision_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MapReviewTargetRequest {
    pub note_id: String,
    pub expected_revision: u64,
    pub proposal_id: Option<String>,
    pub revision_id: String,
    pub operation_id: String,
    pub decision_owner: RangeDecisionOwner,
    pub rationale: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewTargetMapping {
    pub decision_owner: RangeDecisionOwner,
    pub mapping_id: String,
    pub note_id: String,
    pub proposal_id: Option<String>,
    pub target: ReviewTarget,
    pub rationale: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum ReviewFeedbackFact {
    Note(ReviewNote),
    Proposal(ReviewProposal),
    Decision(ReviewDecision),
    Mapping(ReviewTargetMapping),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFeedbackHistoryItem {
    pub operation_id: String,
    pub note_revision: u64,
    pub recorded_at_millis: i64,
    pub fact: ReviewFeedbackFact,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTargetStatus {
    pub flags: Vec<String>,
    pub candidate_operation_ids: Vec<String>,
    pub candidates_truncated: bool,
    pub requires_mapping: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFeedbackView {
    pub current_revision_id: String,
    pub note: ReviewNote,
    pub revision: u64,
    pub effective_target: ReviewTarget,
    pub target_status: ReviewTargetStatus,
    pub latest_proposal: Option<ReviewProposal>,
    pub latest_decision: Option<ReviewDecision>,
    pub decided_proposal: Option<ReviewProposal>,
    pub proposal_target_status: Option<ReviewTargetStatus>,
    /// addressed/deferred/superseded/accepted/unresolved, kept distinct from target flags.
    pub resolution: String,
    pub history: Vec<ReviewFeedbackHistoryItem>,
    pub history_total: usize,
    pub history_truncated: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFeedbackPage {
    pub schema_ready: bool,
    pub project_id: String,
    pub revision_id: String,
    pub total: usize,
    pub next_offset: Option<usize>,
    pub items: Vec<ReviewFeedbackView>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcceptedReviewReference {
    pub note_id: String,
    pub expected_revision: u64,
    pub proposal_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplyReviewFeedbackRequest {
    pub project_id: String,
    pub base_revision_id: String,
    pub expected_active_revision_id: Option<String>,
    pub accepted: Vec<AcceptedReviewReference>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyReviewFeedbackResult {
    pub revision: EditPlanRevision,
    pub diff: EditPlanDiff,
    pub validation: ValidationReport,
    pub addressed_note_ids: Vec<String>,
}
