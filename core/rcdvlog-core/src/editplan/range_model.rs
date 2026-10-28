//! Human-owned source constraints. Model inspection/suggestions never write these.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RangeDecision {
    Allow,
    Exclude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RangeDecisionOwner {
    Human,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RangeMedia {
    Picture,
    Audio,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TrimMedia {
    Picture,
    Audio,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceTimeRange {
    pub source_in_seconds: f64,
    pub source_out_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfirmSourceRangeRequest {
    pub asset_id: String,
    pub expected_revision: u64,
    pub expected_source_fingerprint: String,
    #[serde(default)]
    pub constraint_id: Option<String>,
    pub decision: RangeDecision,
    pub media: RangeMedia,
    /// Explicit human authority is required; no modelSuggested alternative.
    pub decision_owner: RangeDecisionOwner,
    pub source_in_seconds: f64,
    pub source_out_seconds: f64,
    pub rationale: String,
    #[serde(default)]
    pub inspected_evidence_id: Option<String>,
    #[serde(default)]
    pub overrides_evidence_ids: Vec<String>,
}

/// Path-free inspection provenance travels with the human operation. Local
/// derived cache files need not exist on a peer for it to honor the constraint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfirmedRangeInspection {
    pub evidence_id: String,
    pub representation_fingerprint: String,
    pub recipe: String,
    pub purpose: super::InspectionPurpose,
    pub included_audio: bool,
    pub included_transcript: bool,
    pub rotation_degrees: u16,
    pub mapping: super::InspectionTimeMapping,
    pub window_in_seconds: f64,
    pub window_out_seconds: f64,
    pub sample_count: u32,
    pub max_unsampled_gap_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfirmedSourceRange {
    pub constraint_id: String,
    pub project_id: String,
    pub asset_id: String,
    pub source_fingerprint: String,
    pub decision: RangeDecision,
    pub media: RangeMedia,
    pub decision_owner: RangeDecisionOwner,
    pub source_in_seconds: f64,
    pub source_out_seconds: f64,
    pub rationale: String,
    pub inspection: Option<ConfirmedRangeInspection>,
    pub overrides_evidence_ids: Vec<String>,
    pub active: bool,
    pub last_operation_id: String,
    pub updated_at_millis: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRangeConstraints {
    pub schema_ready: bool,
    pub asset_id: String,
    pub source_fingerprint: String,
    pub revision: u64,
    pub items: Vec<ConfirmedSourceRange>,
    pub total_items: usize,
    pub next_offset: Option<usize>,
    pub feasible_picture_ranges: Vec<SourceTimeRange>,
    pub feasible_audio_ranges: Vec<SourceTimeRange>,
    pub picture_conflicts: Vec<String>,
    pub audio_conflicts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceTrimRequest {
    pub asset_id: String,
    pub inspected_evidence_id: String,
    pub media: TrimMedia,
    pub source_in_seconds: f64,
    pub source_out_seconds: f64,
    #[serde(default)]
    pub prefer_utterances: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceTrimSuggestion {
    pub inspected_evidence_id: String,
    pub media: TrimMedia,
    pub constraints_revision: u64,
    pub clean_ranges: Vec<SourceTimeRange>,
    pub proposed_ranges: Vec<SourceTimeRange>,
    pub conflicts: Vec<String>,
    pub limitations: Vec<String>,
    pub listening_required: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SourceRangeError {
    #[error("source range constraints changed: expected revision {expected}, found {actual}")]
    StaleRevision { expected: u64, actual: u64 },
    #[error("source range constraint source identity changed")]
    StaleSource,
}
impl SourceRangeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::StaleRevision { .. } => "source_ranges_stale_revision",
            Self::StaleSource => "stale_evidence",
        }
    }
}
