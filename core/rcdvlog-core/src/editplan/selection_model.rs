//! Versioned selection inputs attached to immutable plans, not another timeline.
use super::{EditPlanOperation, EditPlanRevision, RangeDecisionOwner};
use crate::moments::{MomentCandidate, MomentSpan};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum SequenceSelectionError {
    #[error(
        "selection evidence is unavailable for {asset_id}; inspect or explicitly declare source locators"
    )]
    EvidenceUnavailable { asset_id: String },
    #[error(
        "selection evidence changed for {asset_id}; inspect the current candidate record before reselecting"
    )]
    StaleEvidence { asset_id: String },
}
impl SequenceSelectionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::EvidenceUnavailable { .. } => "selection_evidence_unavailable",
            Self::StaleEvidence { .. } => "selection_stale_evidence",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SequencePacingPreset {
    Natural,
    RapidMovement,
    Custom,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SequencePacingProfile {
    pub preset: SequencePacingPreset,
    pub preferred_min_seconds: f64,
    pub preferred_max_seconds: f64,
    pub long_review_seconds: f64,
    pub preferred_upper_seconds: f64,
    pub preferred_long_holds: u32,
    /// A fragment must be deliberately marked; dialogue/actions are not blindly cropped.
    pub allow_montage_fragments: bool,
}
impl Default for SequencePacingProfile {
    fn default() -> Self {
        Self {
            preset: SequencePacingPreset::Natural,
            preferred_min_seconds: 3.0,
            preferred_max_seconds: 8.0,
            long_review_seconds: 8.0,
            preferred_upper_seconds: 30.0,
            preferred_long_holds: 2,
            allow_montage_fragments: false,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SelectionActivity {
    Active,
    Passive,
    Movement,
    Dialogue,
    Uncertain,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectionCandidateKey {
    pub sequence_id: String,
    pub asset_id: String,
    pub candidate_id: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SelectionMomentSource {
    Current {
        asset_id: String,
        expected_moment_identity: String,
        #[serde(default)]
        candidate_ids: Vec<String>,
    },
    /// Explicit authored locators also support public/generated fixture workflows.
    /// These are never represented as paid provider responses or inspected cuts.
    Declared {
        asset_id: String,
        expected_source_fingerprint: String,
        rationale: String,
        candidates: Vec<MomentCandidate>,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectionRoleClaim {
    pub key: SelectionCandidateKey,
    pub role: String,
    pub activity: SelectionActivity,
    pub rationale: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetainedChoiceAssignment {
    pub note_id: String,
    pub role: String,
    pub activity: SelectionActivity,
    pub rationale: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlaceContextKind {
    Visual,
    Spoken,
    HumanSupplied,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SequencePlaceContext {
    pub kind: PlaceContextKind,
    pub description: String,
    /// The actual establishing shot/utterance is selected before the new sequence proceeds.
    pub establishing_candidate: SelectionCandidateKey,
    pub decision_owner: RangeDecisionOwner,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SequenceIntent {
    pub sequence_id: String,
    pub goal: String,
    pub pacing: SequencePacingProfile,
    pub sources: Vec<SelectionMomentSource>,
    #[serde(default)]
    pub role_claims: Vec<SelectionRoleClaim>,
    #[serde(default)]
    pub retained_choices: Vec<RetainedChoiceAssignment>,
    #[serde(default)]
    pub hero_candidates: Vec<SelectionCandidateKey>,
    pub budget_seconds: Option<f64>,
    #[serde(default)]
    pub hard_budget: bool,
    #[serde(default)]
    pub major_place_change: bool,
    pub place_context: Option<SequencePlaceContext>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectionCountConstraint {
    pub sequence_id: Option<String>,
    pub role: String,
    pub minimum: u32,
    pub maximum: u32,
    pub hard: bool,
    pub rationale: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectionRelationship {
    pub before: SelectionCandidateKey,
    pub after: SelectionCandidateKey,
    pub keep_together: bool,
    pub hard: bool,
    pub rationale: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SequenceSelectionInput {
    pub version: u32,
    pub decision_owner: RangeDecisionOwner,
    pub goal: String,
    pub sequences: Vec<SequenceIntent>,
    #[serde(default)]
    pub include: Vec<SelectionCandidateKey>,
    #[serde(default)]
    pub exclude: Vec<SelectionCandidateKey>,
    #[serde(default)]
    pub counts: Vec<SelectionCountConstraint>,
    #[serde(default)]
    pub relationships: Vec<SelectionRelationship>,
    pub budget_seconds: Option<f64>,
    #[serde(default)]
    pub hard_budget: bool,
    /// No minimum runtime. Weak material is not filler for an unmet preference.
    pub minimum_score: f64,
    pub repetition_penalty: f64,
    pub coverage_bonus: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SequenceSelectionRequest {
    pub project_id: String,
    pub expected_base_revision_id: Option<String>,
    pub expected_active_revision_id: Option<String>,
    pub input: SequenceSelectionInput,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionEvidenceIdentity {
    pub source_fingerprint: String,
    pub moment_identity: String,
    pub candidate_fingerprint: String,
    pub origin: String,
    pub provider: Option<String>,
    pub requested_model: Option<String>,
    pub actual_model: Option<String>,
    pub prompt_version: Option<String>,
    pub schema_version: Option<String>,
    pub recipe_version: String,
    pub policy_fingerprint: String,
    pub representation_fingerprint: Option<String>,
    pub sampling_limits: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedMomentAssignment {
    pub operation_id: String,
    pub sequence_id: String,
    pub candidate: Option<SelectionCandidateKey>,
    pub retained_note_id: Option<String>,
    pub asset_id: String,
    pub role: String,
    pub activity: SelectionActivity,
    pub event: String,
    pub coverage_bucket: u8,
    pub original_core: MomentSpan,
    pub complete_span: MomentSpan,
    pub chosen_source_in_seconds: f64,
    pub chosen_source_out_seconds: f64,
    pub montage_fragment: bool,
    pub rationale: String,
    pub evidence: SelectionEvidenceIdentity,
    pub uncertainty: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionMetadata {
    pub version: u32,
    pub recipe: String,
    pub input_identity: String,
    pub input: SequenceSelectionInput,
    pub assignments: Vec<SelectedMomentAssignment>,
    pub constraints_key: String,
    #[serde(default)]
    pub planning_conflicts: Vec<SelectionConflict>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionConflict {
    pub candidate: Option<SelectionCandidateKey>,
    pub asset_id: Option<String>,
    pub code: String,
    pub blocking: bool,
    pub sequence_id: Option<String>,
    pub operation_id: Option<String>,
    pub message: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionRoleDistribution {
    pub role: String,
    pub count: u32,
    pub duration_seconds: f64,
    pub shortest_seconds: f64,
    pub longest_seconds: f64,
    pub median_seconds: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionSequenceSummary {
    pub sequence_id: String,
    pub goal: String,
    pub earned_seconds: f64,
    pub preferred_budget_seconds: Option<f64>,
    pub unfilled_seconds: Option<f64>,
    pub active_seconds: f64,
    pub passive_seconds: f64,
    pub uncertain_seconds: f64,
    pub early_count: u32,
    pub middle_count: u32,
    pub late_count: u32,
    pub roles: Vec<SelectionRoleDistribution>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionReviewItem {
    pub operation_id: String,
    pub sequence_id: String,
    pub duration_seconds: f64,
    pub reason: String,
    pub alternatives: Vec<SelectionCandidateKey>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionReport {
    pub duration_basis: String,
    pub recipe: String,
    pub input_identity: String,
    pub reused: bool,
    pub earned_seconds: f64,
    pub preferred_budget_seconds: Option<f64>,
    pub unfilled_seconds: Option<f64>,
    pub sequences: Vec<SelectionSequenceSummary>,
    pub conflicts: Vec<SelectionConflict>,
    pub long_take_review: Vec<SelectionReviewItem>,
    pub deferred_candidates: Vec<SelectionCandidateKey>,
    pub limitations: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionProposal {
    pub operations: Vec<EditPlanOperation>,
    pub metadata: SelectionMetadata,
    pub report: SelectionReport,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionDraftResult {
    pub revision: EditPlanRevision,
    pub report: SelectionReport,
    pub validation: super::ValidationReport,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionRevisionView {
    pub schema_ready: bool,
    pub revision_id: String,
    pub metadata: Option<SelectionMetadata>,
    pub report: Option<SelectionReport>,
}
