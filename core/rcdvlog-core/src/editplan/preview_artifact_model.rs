//! Node-local render receipts are derived evidence, not portable edit decisions.
use super::{PreviewRequest, PreviewResult};
use crate::CalibrationToolIdentity;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PreviewArtifactState {
    Running,
    Complete,
    Cancelled,
    Failed,
    Interrupted,
}

impl PreviewArtifactState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Complete => "complete",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PreviewArtifactError {
    #[error("preview render cancelled: {artifact_id}")]
    Cancelled { artifact_id: String },
    #[error("preview wait timed out for {artifact_id}; the original attempt continues running")]
    ObservationTimedOut { artifact_id: String },
    #[error("preview rendering is already in progress for revision {revision_id}")]
    Busy { revision_id: String },
    #[error("preview artifact not found: {artifact_id}")]
    NotFound { artifact_id: String },
    #[error(
        "preview artifact is already {state}; request a new preview rather than cancelling or resuming this terminal attempt"
    )]
    AlreadyTerminal { state: String },
    #[error("preview receipt is inconsistent; no current artifact can be asserted")]
    InvalidReceipt,
}
impl PreviewArtifactError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Cancelled { .. } => "cancelled",
            Self::ObservationTimedOut { .. } => "preview_wait_timeout",
            Self::Busy { .. } => "representation_busy",
            Self::NotFound { .. } => "not_found",
            Self::AlreadyTerminal { .. } => "preview_artifact_terminal",
            Self::InvalidReceipt => "preview_artifact_invalid",
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSegmentCounts {
    pub encoded: u32,
    pub reused: u32,
    /// Matching receipts rejected for damaged/missing output or invalid metadata.
    pub invalidated: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSourceIdentity {
    pub asset_id: String,
    pub source_fingerprint: String,
    pub representation: String,
    pub representation_fingerprint: String,
    pub rotation_degrees: u16,
    /// Actual selected-media stream clocks, not inferred frame correspondences.
    pub clocks: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewInputIdentity {
    pub recipe: String,
    pub revision_id: String,
    pub plan_digest: String,
    pub max_height: u32,
    pub constraints_key: String,
    pub sources: Vec<PreviewSourceIdentity>,
    pub tools: Vec<CalibrationToolIdentity>,
    #[serde(default)]
    pub tool_content_fingerprints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewCutMapping {
    pub operation_id: String,
    pub asset_id: String,
    pub source_in_seconds: f64,
    pub source_out_seconds: f64,
    /// Exact output-frame boundaries on the shared movie clock. Source sampling remains
    /// subject to the input cadence and renderer conversion recipe.
    pub movie_in_pts: i64,
    pub movie_out_pts: i64,
    pub output_frame_count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewMediaValidation {
    pub output_fingerprint: String,
    pub byte_length: u64,
    pub duration_seconds: f64,
    pub video_frame_count: u64,
    pub video_time_base_numerator: i64,
    pub video_time_base_denominator: i64,
    pub video_origin_pts: i64,
    pub movie_time_base_numerator: i64,
    pub movie_time_base_denominator: i64,
    pub movie_origin_pts: i64,
    pub width: u32,
    pub height: u32,
    pub audio_sample_rate: u32,
    pub audio_channels: u32,
    pub fully_decoded: bool,
    pub cuts: Vec<PreviewCutMapping>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewArtifact {
    pub artifact_id: String,
    pub project_id: String,
    pub revision_id: String,
    pub generation: u64,
    pub request: PreviewRequest,
    /// fullPreview now; future excerpts must carry a distinct declared scope.
    pub kind: String,
    pub state: PreviewArtifactState,
    pub sequence: u64,
    pub stage: String,
    pub cancel_requested: bool,
    pub identity: Option<PreviewInputIdentity>,
    pub result: Option<PreviewResult>,
    pub validation: Option<PreviewMediaValidation>,
    pub output_relative_path: Option<String>,
    pub failure: Option<String>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub cut_outcomes: Vec<PreviewCutOutcome>,
    pub outcome_total: usize,
    pub outcomes_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewCutOutcome {
    pub operation_id: String,
    pub position: u32,
    pub asset_id: String,
    pub state: String,
    pub reused: bool,
    pub cache_key: Option<String>,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewArtifactVerification {
    pub verified_at: String,
    pub output_matches: bool,
    pub inputs_match: bool,
    pub constraints_match: bool,
    pub plan_renderable: bool,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewArtifactStatus {
    pub artifact: PreviewArtifact,
    /// Distinguishes a lost writer from a currently held render claim.
    pub observed_state: PreviewArtifactState,
    pub latest_attempt: bool,
    pub active_revision_id: Option<String>,
    pub file_present: bool,
    /// None means no fresh media verification was requested. Stored completed
    /// receipts and path existence alone never assert current inputs/output.
    pub current_for_request: Option<bool>,
    pub verification: Option<PreviewArtifactVerification>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewArtifactSummary {
    pub artifact_id: String,
    pub revision_id: String,
    pub kind: String,
    pub generation: u64,
    pub sequence: u64,
    pub state: PreviewArtifactState,
    pub observed_state: PreviewArtifactState,
    pub stage: String,
    pub cancel_requested: bool,
    pub max_height: u32,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub output_path: Option<String>,
    pub duration_seconds: Option<f64>,
    pub clip_count: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewArtifactPage {
    pub schema_ready: bool,
    pub revision_id: String,
    pub total: usize,
    pub next_offset: Option<usize>,
    pub items: Vec<PreviewArtifactSummary>,
}
