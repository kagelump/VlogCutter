//! Explicit bounded native-video inputs. These types do not authorize a send.
use crate::analysis::accounting::usage_pricing::{PublishedServiceTier, UsagePricedReceipt};
use crate::{InspectionRepresentation, MomentCandidate};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NativeVideoPurpose {
    Moments,
    EndpointReview,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NativeVideoNavigation {
    Static,
    Agentic,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NativeVideoThinking {
    Minimal,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeVideoSourceScope {
    pub asset_id: String,
    pub source_in_seconds: f64,
    pub source_out_seconds: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeVideoOptions {
    pub version: u32,
    pub purpose: NativeVideoPurpose,
    pub tier: PublishedServiceTier,
    pub representation: InspectionRepresentation,
    pub navigation: NativeVideoNavigation,
    pub thinking: NativeVideoThinking,
    pub source_scopes: Vec<NativeVideoSourceScope>,
    pub fps: f64,
    pub max_width: u32,
    pub max_window_seconds: u32,
    pub max_media_seconds: u32,
    pub max_requests: u32,
    pub max_output_tokens: u32,
    pub media_workers: u32,
    pub api_workers: u32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeVideoWindowRequest {
    pub policy_id: String,
    pub scope_index: u32,
    pub source_in_seconds: f64,
    pub source_out_seconds: f64,
    pub fps: f64,
    pub max_width: u32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeVideoClock {
    pub source_offset_seconds: f64,
    pub seconds_scale: f64,
    pub local_valid_end_seconds: f64,
    pub first_representation_pts: i64,
    pub representation_time_base_numerator: i64,
    pub representation_time_base_denominator: i64,
    pub precision: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeVideoProvenance {
    pub policy_id: String,
    pub asset_id: String,
    pub source_fingerprint: String,
    pub source_identity: String,
    pub representation_fingerprint: String,
    pub media_fingerprint: String,
    pub media_recipe: String,
    pub prompt_version: String,
    pub schema_version: String,
    pub clock: NativeVideoClock,
    pub requested_model: String,
    pub actual_model: Option<String>,
    pub requested_tier: PublishedServiceTier,
    pub reported_tier: Option<String>,
    pub http_status: Option<u16>,
    pub request_key: Option<String>,
    pub usage: Option<Box<UsagePricedReceipt>>,
    pub limitations: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeVideoFlag {
    pub kind: String,
    pub source_in_seconds: f64,
    pub source_out_seconds: f64,
    pub confidence: f64,
    pub observation: String,
    pub uncertainty: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeVideoWindowResult {
    pub evidence_id: String,
    pub state: String,
    pub request: NativeVideoWindowRequest,
    pub provenance: NativeVideoProvenance,
    pub candidates: Vec<MomentCandidate>,
    pub flags: Vec<NativeVideoFlag>,
    pub rejection_reason: Option<String>,
    pub reused: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeVideoProgress {
    pub run_id: String,
    pub sequence: u64,
    pub stage: String,
    pub completed: u32,
    pub total: u32,
    pub media_workers: u32,
    pub api_workers: u32,
    pub estimated_cost_usd: f64,
    pub confirmed_cost_usd: f64,
    pub uncertain_usage_reserve_usd: f64,
    pub active_reserved_cost_usd: f64,
    pub committed_or_reserved_usd: f64,
    pub budget_usd: f64,
    pub media_estimate: batch_estimator::BatchEstimate,
    pub api_estimate: batch_estimator::BatchEstimate,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeVideoRunReport {
    pub run_id: String,
    pub policy_id: String,
    pub completed: u32,
    pub reused: u32,
    pub rejected: u32,
    pub interrupted: u32,
    pub cancelled: bool,
    pub budget_stopped: bool,
    pub evidence_ids: Vec<String>,
    pub evidence_count: u32,
    pub failures: Vec<NativeVideoWindowFailure>,
    pub moment_artifacts: Vec<NativeVideoMomentProjection>,
    pub moment_artifact_count: u32,
    pub accounting: crate::AnalysisAccountingAggregate,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeVideoMomentProjection {
    pub asset_id: String,
    pub identity: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeVideoWindowFailure {
    pub window_index: u32,
    pub code: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeVideoRunStatus {
    pub run_id: String,
    pub policy_id: String,
    pub state: String,
    pub cancel_requested: bool,
    pub progress: NativeVideoProgress,
    pub report: Option<NativeVideoRunReport>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeVideoWindowStatus {
    pub evidence_id: String,
    pub run_id: String,
    pub policy_id: String,
    pub stored_state: String,
    pub observed_state: String,
    pub request: NativeVideoWindowRequest,
    pub provenance: NativeVideoProvenance,
    pub raw_response_available: bool,
    pub result_available: bool,
    pub failure_code: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeVideoRunWindowStatus {
    pub window_index: u32,
    pub request: NativeVideoWindowRequest,
    pub state: String,
    pub reused: bool,
    pub evidence_id: Option<String>,
    pub failure_code: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeVideoRawResponse {
    pub evidence_id: String,
    pub http_status: Option<u16>,
    pub body: String,
}
