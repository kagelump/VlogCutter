use crate::{MomentPage, MomentRunOptions, MomentRunReport};
use serde::{Deserialize, Serialize};

pub const PREPARATION_SCHEMA_VERSION: &str = "prepare.evidence.v1";
pub const PREPARATION_VALIDATION_RECIPE: &str = "prepare.proxy-full-decode.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparationScope {
    pub project_id: String,
    pub asset_ids: Vec<String>,
    pub all_assets: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparationScopeSummary {
    pub all_assets: bool,
    pub selected_asset_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparationState {
    Missing,
    Stale,
    Unavailable,
    Quarantined,
    Ready,
    Excluded,
    ReviewRequired,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparationEvidenceState {
    pub state: PreparationState,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparationRepresentation {
    pub kind: String,
    pub recipe: Option<String>,
    pub required_recipe: String,
    pub fingerprint: Option<String>,
    pub fingerprint_kind: String,
    pub byte_length: Option<u64>,
    pub validation: PreparationEvidenceState,
    pub receipt_id: Option<String>,
    pub validated_at: Option<String>,
    pub validator: Option<String>,
    pub usable_for_analysis: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparationTranscript {
    pub readiness: PreparationEvidenceState,
    pub status: Option<String>,
    /// Stable distinction between silence, absent audio, empty provider output,
    /// quality rejection and processing failure.
    pub outcome_reason: Option<String>,
    pub recipe: Option<String>,
    pub source_fingerprint: Option<String>,
    pub segment_count: usize,
    pub speech_seconds: Option<f64>,
    /// Provider decode windows and raw decoded segment count. Retained even
    /// when no transcript was accepted so targeted recovery stays explainable.
    pub window_count: Option<u32>,
    pub provider_segment_count: Option<u32>,
    pub mean_token_probability: Option<f32>,
    pub repetition_ratio: Option<f32>,
    pub binding_origin: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparationOrientation {
    pub metadata_probed: bool,
    pub metadata_rotation_degrees: u16,
    pub default_rotation_degrees: u16,
    pub effective_rotation_degrees: u16,
    pub confirmation: String,
    pub review_questions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparationAsset {
    pub asset_id: String,
    pub asset_revision: u64,
    pub original_available: bool,
    pub proxy_available: bool,
    pub state: PreparationState,
    pub review_questions: Vec<String>,
    pub review_questions_truncated: bool,
    pub representation: PreparationRepresentation,
    pub camera_sidecar_status: Option<String>,
    pub camera_sidecar_reason: Option<String>,
    pub transcript: PreparationTranscript,
    pub moments: Option<MomentPage>,
    pub moment_details_truncated: bool,
    pub orientation: PreparationOrientation,
    pub technical_range: Option<crate::moments::MomentSpan>,
    pub range_certification: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparationManifest {
    pub schema_version: String,
    pub schema_ready: bool,
    pub project_id: String,
    pub scope: PreparationScopeSummary,
    pub scope_fingerprint: String,
    pub total_assets: usize,
    pub offset: usize,
    pub limit: usize,
    pub next_offset: Option<usize>,
    /// This outcome covers only the returned page, never unseen project assets.
    pub page_outcome: String,
    pub items: Vec<PreparationAsset>,
    pub policy_references: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparationSpeechSegment {
    pub start_ms: u32,
    pub end_ms: u32,
    pub language: String,
    pub text: String,
    pub text_truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparationTranscriptPage {
    pub schema_version: String,
    pub asset_id: String,
    pub transcript: PreparationTranscript,
    pub segments: Vec<PreparationSpeechSegment>,
    pub offset: usize,
    pub limit: usize,
    pub next_offset: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct PreparationRunOptions {
    pub generate_proxies: bool,
    pub transcribe: bool,
    pub revalidate: bool,
    /// Presence authorizes only this selected policy over the explicit scope.
    /// The existing moment runner owns the single run budget and recovery.
    pub moments: Option<MomentRunOptions>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparationProgress {
    pub run_id: String,
    pub asset_id: String,
    pub stage: String,
    pub processed: usize,
    pub total_assets: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparationFailure {
    pub asset_id: String,
    pub stage: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparationRunReport {
    pub run_id: String,
    pub total_assets: usize,
    pub processed: usize,
    pub reused: usize,
    pub excluded: usize,
    pub skipped: usize,
    pub failed: usize,
    pub cancelled: bool,
    pub failures: Vec<PreparationFailure>,
    pub failures_truncated: bool,
    pub moments: Option<MomentRunReport>,
    pub manifest: PreparationManifest,
}
