use super::logic::MomentPayload;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MomentSpan {
    pub start_ms: u32,
    pub end_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentReceipt {
    #[serde(default)]
    pub run_id: String,
    pub step_key: String,
    pub actual_model: Option<String>,
    pub provider: Option<String>,
    pub generation_id: Option<String>,
    pub cost_usd: Option<f64>,
    pub usage_complete: bool,
    pub reused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditProfile {
    pub select_min_ms: u32,
    pub select_max_ms: u32,
    pub hold_min_ms: u32,
    pub hold_max_ms: u32,
}

impl Default for EditProfile {
    fn default() -> Self {
        Self {
            select_min_ms: 3_000,
            select_max_ms: 8_000,
            hold_min_ms: 20_000,
            hold_max_ms: 30_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentEvidenceRef {
    pub kind: String,
    pub key: String,
    pub span: Option<MomentSpan>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentCandidate {
    pub id: String,
    pub rank: u32,
    pub score: f64,
    pub core: MomentSpan,
    pub payoff: MomentSpan,
    pub setup: Option<MomentSpan>,
    pub context: Option<MomentSpan>,
    pub allowed_range: MomentSpan,
    pub reason: String,
    pub event: String,
    pub role: String,
    pub evidence: Vec<MomentEvidenceRef>,
    pub uncertainty: Vec<String>,
    pub dialogue_boundaries: Vec<MomentSpan>,
    pub refined: bool,
    #[serde(default)]
    pub long_hold: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MomentStatus {
    Ready,
    NoCandidates,
    NeedsReview,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageLevel {
    Sampled,
    Refined,
    Uninspected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageSegment {
    pub span: MomentSpan,
    pub level: CoverageLevel,
    pub sample_times_ms: Vec<u32>,
    pub max_gap_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentProvenance {
    #[serde(default)]
    pub requested_provider: String,
    #[serde(default)]
    pub payload: MomentPayload,
    pub provider: String,
    pub requested_model: String,
    pub actual_model: String,
    pub prompt_version: String,
    pub schema_version: String,
    pub recipe_version: String,
    pub representation: String,
    pub representation_fingerprint: String,
    pub time_mapping: String,
    #[serde(default)]
    pub source_duration_ms: u32,
    pub transcript_fingerprint: Option<String>,
    pub observation_fingerprint: Option<String>,
    pub profile: EditProfile,
    pub configuration_hash: String,
    pub cost_usd: f64,
    pub usage_complete: bool,
    #[serde(default)]
    pub receipts: Vec<MomentReceipt>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentArtifact {
    pub asset_id: String,
    pub identity: String,
    pub status: MomentStatus,
    pub candidates: Vec<MomentCandidate>,
    pub coverage: Vec<CoverageSegment>,
    pub provenance: MomentProvenance,
    pub review_questions: Vec<String>,
}
