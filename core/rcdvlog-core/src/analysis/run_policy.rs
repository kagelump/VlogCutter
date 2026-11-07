//! Concrete, bounded authorization for paid analysis runs.
//!
//! A policy joins the exact project-owned assets, provider/model allowlist,
//! payload, expected cost and spend ceiling before any provider work. Approval
//! records deliberately live separately from the runtime capability pilot: a
//! pilot observes one bounded request against one exact policy and is never a
//! promise that the provider will remain available or semantically correct.
//!
//! Nothing here writes prompts, response bodies, credentials, frame bytes or
//! source paths to SQLite or logs. Only identities, bounded conclusions and
//! receipts are retained.

use anyhow::{Context, Result, bail, ensure};
use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::analysis::accounting::{
    AnalysisAccountingContext, AnalysisBudgetAuthorityContext, AnalysisBudgetSnapshot,
    AnalysisRetryAuthorization,
};
use crate::analysis::moments::{
    EditProfile, MOMENT_PROMPT_VERSION, MOMENT_RECIPE_VERSION, MOMENT_SCHEMA_VERSION, MomentPayload,
};
use crate::analysis::openrouter::{OpenRouterProvider, api_credential_present, resolve_api_key};
use crate::analysis::openrouter_stt::{
    NativeSttProviderPolicy, native_stt_cost_bound_contract_id, resolve_native_stt_model,
};
use crate::analysis::pilot_runtime::{
    PilotAttemptBaseline, PilotSession, PilotSessionStart, execute_accounted_pilot_attempts,
    prepare_pilot_session, prepare_pilot_transmission,
};
use crate::analysis::provider::{
    DEFAULT_MODEL, FALLBACK_MODEL, ProviderFailure, VlmProvider, analysis_cost_bound_contract_id,
    model_spec,
};
use crate::analysis::{VLM_PROMPT_VERSION, framepack, store::TranscriptStatus};
use crate::{Asset, Catalog, REFERENCE_COST_PER_CLIP_USD};

mod native_bulk;
mod native_pilot;
mod pilot_projection;
use pilot_projection::{pilot_observed_capabilities, pilot_projection_status};
mod native_pilot_evidence;
pub(crate) use native_pilot_evidence::reusable_native_pilot_sample_on_transaction;
mod native_stt;
pub use native_bulk::{
    NATIVE_SPEECH_BATCH_SCHEMA_VERSION, NativeSpeechAssetOutcome, NativeSpeechBatchFailure,
    NativeSpeechBatchProgress, NativeSpeechBatchReport,
};
pub(crate) use native_pilot::native_serving_model_identity_requires_review;
pub use native_pilot_evidence::{
    MAX_NATIVE_PILOT_LOCAL_OUTCOMES, NATIVE_PILOT_EVIDENCE_SCHEMA_VERSION, NativePilotEvidence,
    NativePilotLocalOutcome, NativePilotLocalOutcomeFact, NativePilotPaidSample,
    NativePilotSampleCoverage, NativePilotSampleOutcome,
};
pub(crate) use native_stt::native_stt_request_from_policy;
use native_stt::{
    canonical_native_models, native_stt_blockers, native_stt_capability_checks,
    native_stt_recipe_version,
};

pub const RUN_POLICY_SCHEMA_VERSION: &str = "analysis.run-policy.v1";
pub const RUN_AUTHORIZATION_SCHEMA_VERSION: &str = "analysis.run-authorization.v1";
pub const RUN_PILOT_SCHEMA_VERSION: &str = "analysis.run-pilot.v1";
pub const RUN_POLICY_TOOL_VERSION: &str = "rcdvlog.analysis-preflight.v2-provider-cap-bound";
pub const MAX_SCOPE_ASSETS: usize = 10_000;

const PILOT_WARNING: &str = "A passed pilot is an observed capability for this exact policy only. It does not guarantee future provider availability, pricing, schema compliance or semantic correctness.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisRunKind {
    Observation,
    MomentDiscovery,
    NativeSpeechTranscription,
    NativeVideoReview,
}

impl AnalysisRunKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observation => "observation",
            Self::MomentDiscovery => "moment_discovery",
            Self::NativeSpeechTranscription => "native_speech_transcription",
            Self::NativeVideoReview => "native_video_review",
        }
    }

    /// Existing VLM pilot, provider-boundary and recovery entry points only
    /// operate on these two kinds. Native speech must be rejected explicitly by
    /// those owners rather than falling through to a vision request.
    pub fn is_vlm(self) -> bool {
        matches!(self, Self::Observation | Self::MomentDiscovery)
    }
}

/// The payload an authorized run may select. Frames and accepted transcript are
/// supported by the registered vision models. Audio and transcript-only modes
/// are represented explicitly so a preflight can fail closed before any request
/// instead of silently substituting images.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisPayload {
    Frames,
    FramesAndTranscript,
    Audio,
    AudioAndTranscript,
    Transcript,
    Video,
    VideoAndAudio,
    VideoAndTranscript,
    VideoAndAudioAndTranscript,
}

impl AnalysisPayload {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Frames => "frames",
            Self::FramesAndTranscript => "frames-and-transcript",
            Self::Audio => "audio",
            Self::AudioAndTranscript => "audio-and-transcript",
            Self::Transcript => "transcript",
            Self::Video => "video",
            Self::VideoAndAudio => "video-and-audio",
            Self::VideoAndTranscript => "video-and-transcript",
            Self::VideoAndAudioAndTranscript => "video-and-audio-and-transcript",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "frames" => Some(Self::Frames),
            "frames-and-transcript" | "frames_and_transcript" => Some(Self::FramesAndTranscript),
            "audio" => Some(Self::Audio),
            "audio-and-transcript" | "audio_and_transcript" => Some(Self::AudioAndTranscript),
            "transcript" => Some(Self::Transcript),
            "video" => Some(Self::Video),
            "video-and-audio" | "video_and_audio" => Some(Self::VideoAndAudio),
            "video-and-transcript" | "video_and_transcript" => Some(Self::VideoAndTranscript),
            "video-and-audio-and-transcript" | "video_and_audio_and_transcript" => {
                Some(Self::VideoAndAudioAndTranscript)
            }
            _ => None,
        }
    }

    pub fn uses_frames(self) -> bool {
        matches!(self, Self::Frames | Self::FramesAndTranscript)
    }

    pub fn uses_audio(self) -> bool {
        matches!(
            self,
            Self::Audio
                | Self::AudioAndTranscript
                | Self::VideoAndAudio
                | Self::VideoAndAudioAndTranscript
        )
    }

    pub fn uses_transcript(self) -> bool {
        matches!(
            self,
            Self::FramesAndTranscript
                | Self::AudioAndTranscript
                | Self::Transcript
                | Self::VideoAndTranscript
                | Self::VideoAndAudioAndTranscript
        )
    }

    pub fn uses_video(self) -> bool {
        matches!(
            self,
            Self::Video
                | Self::VideoAndAudio
                | Self::VideoAndTranscript
                | Self::VideoAndAudioAndTranscript
        )
    }

    fn moment_payload(self) -> Option<MomentPayload> {
        match self {
            Self::Frames => Some(MomentPayload::Frames),
            Self::FramesAndTranscript => Some(MomentPayload::FramesAndTranscript),
            _ => None,
        }
    }
}

/// Exact non-frame context the policy permits the runner to attach to a
/// provider request. Only identities and counts are persisted; vocabulary text
/// itself is never written into policy or logs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunPolicyContext {
    pub project_vocabulary: bool,
    pub vocabulary_fingerprint: Option<String>,
    pub vocabulary_item_count: usize,
    pub trip_context: bool,
    pub trip_context_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunPolicyParameters {
    Observation {
        force: bool,
    },
    MomentDiscovery {
        coarse_frames_per_asset: usize,
        refinement_frames_per_asset: usize,
        max_candidates_per_asset: usize,
        profile: EditProfile,
        force: bool,
        retry_uncertain: bool,
    },
    NativeSpeechTranscription {
        language: Option<String>,
        provider_policy: NativeSttProviderPolicy,
        force: bool,
    },
    NativeVideoReview {
        options: crate::NativeVideoOptions,
    },
}

impl RunPolicyParameters {
    pub fn kind(&self) -> AnalysisRunKind {
        match self {
            Self::Observation { .. } => AnalysisRunKind::Observation,
            Self::MomentDiscovery { .. } => AnalysisRunKind::MomentDiscovery,
            Self::NativeSpeechTranscription { .. } => AnalysisRunKind::NativeSpeechTranscription,
            Self::NativeVideoReview { .. } => AnalysisRunKind::NativeVideoReview,
        }
    }

    pub fn force(&self) -> bool {
        match self {
            Self::Observation { force }
            | Self::MomentDiscovery { force, .. }
            | Self::NativeSpeechTranscription { force, .. } => *force,
            Self::NativeVideoReview { .. } => false,
        }
    }
}

/// The caller supplies exactly what it wants to authorize. Describe re-reads
/// canonical ownership and source identity; the caller cannot supply a source
/// fingerprint or representation identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunPolicyRequest {
    pub project_id: String,
    pub asset_ids: Vec<String>,
    pub provider: String,
    pub primary_model: String,
    #[serde(default)]
    pub allowed_models: Vec<String>,
    pub allow_fallback: bool,
    pub payload: AnalysisPayload,
    /// Explicitly allow the exact current project label vocabulary to be
    /// attached to the provider prompt. Its identity/count is part of the
    /// policy; the text itself is never stored in provenance.
    #[serde(default)]
    pub include_project_vocabulary: bool,
    pub budget_usd: f64,
    pub parameters: RunPolicyParameters,
}

impl RunPolicyRequest {
    pub fn observation(
        project_id: impl Into<String>,
        asset_ids: Vec<String>,
        primary_model: impl Into<String>,
        allowed_models: Vec<String>,
        allow_fallback: bool,
        budget_usd: f64,
        force: bool,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            asset_ids,
            provider: "openrouter".into(),
            primary_model: primary_model.into(),
            allowed_models,
            allow_fallback,
            payload: AnalysisPayload::FramesAndTranscript,
            include_project_vocabulary: false,
            budget_usd,
            parameters: RunPolicyParameters::Observation { force },
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn moment_discovery(
        project_id: impl Into<String>,
        asset_ids: Vec<String>,
        primary_model: impl Into<String>,
        allowed_models: Vec<String>,
        allow_fallback: bool,
        payload: AnalysisPayload,
        budget_usd: f64,
        coarse_frames: usize,
        refinement_frames: usize,
        max_candidates: usize,
        profile: EditProfile,
        force: bool,
        retry_uncertain: bool,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            asset_ids,
            provider: "openrouter".into(),
            primary_model: primary_model.into(),
            allowed_models,
            allow_fallback,
            payload,
            include_project_vocabulary: false,
            budget_usd,
            parameters: RunPolicyParameters::MomentDiscovery {
                coarse_frames_per_asset: coarse_frames,
                refinement_frames_per_asset: refinement_frames,
                max_candidates_per_asset: max_candidates,
                profile,
                force,
                retry_uncertain,
            },
        }
    }

    /// Explicit native cloud speech proposal. Provider and model remain in the
    /// common authoritative request fields; the native parameters carry only
    /// the exact language, supported data-policy fields and force flag.
    #[allow(clippy::too_many_arguments)]
    pub fn native_speech_transcription(
        project_id: impl Into<String>,
        asset_ids: Vec<String>,
        provider: impl Into<String>,
        primary_model: impl Into<String>,
        budget_usd: f64,
        language: Option<String>,
        provider_policy: NativeSttProviderPolicy,
        force: bool,
    ) -> Self {
        let primary_model = primary_model.into();
        Self {
            project_id: project_id.into(),
            asset_ids,
            provider: provider.into(),
            primary_model: primary_model.clone(),
            allowed_models: vec![primary_model],
            allow_fallback: false,
            payload: AnalysisPayload::Audio,
            include_project_vocabulary: false,
            budget_usd,
            parameters: RunPolicyParameters::NativeSpeechTranscription {
                language,
                provider_policy,
                force,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAssetScope {
    pub asset_id: String,
    pub source_fingerprint: String,
    pub source_duration_ms: u32,
    pub representation: String,
    pub representation_fingerprint: Option<String>,
    pub sampled_representation_fingerprint: Option<String>,
    pub transcript_fingerprint: Option<String>,
    pub time_mapping: String,
    pub source_identity: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    Passed,
    Failed,
    NotRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityCheck {
    pub requirement: String,
    pub status: CapabilityStatus,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunPolicyBlocker {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunPolicyReadiness {
    pub capabilities_ready: bool,
    pub scope_ready: bool,
    pub budget_ready: bool,
    pub credentials_present: bool,
    pub ready_for_authorization: bool,
    pub ready_for_pilot: bool,
    pub blockers: Vec<RunPolicyBlocker>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunPolicy {
    pub schema_version: String,
    pub policy_id: String,
    pub project_id: String,
    pub kind: AnalysisRunKind,
    pub provider: String,
    pub primary_model: String,
    pub allowed_models: Vec<String>,
    pub allow_fallback: bool,
    pub permitted_fallbacks: Vec<String>,
    pub payload: AnalysisPayload,
    pub context: RunPolicyContext,
    pub scope: Vec<RunAssetScope>,
    pub expected_cost_usd: f64,
    pub cost_basis: String,
    pub budget_usd: f64,
    pub parameters: RunPolicyParameters,
    pub prompt_version: String,
    pub output_schema_version: String,
    pub recipe_version: String,
    pub tool_version: String,
    /// Versioned provider-routing/cost-bound contract used by the wire bodies
    /// and admission reservations for this policy.
    #[serde(default)]
    pub provider_cost_bound_contract: String,
    pub capability_checks: Vec<CapabilityCheck>,
    pub readiness: RunPolicyReadiness,
}

impl RunPolicy {
    pub fn request(&self) -> RunPolicyRequest {
        RunPolicyRequest {
            project_id: self.project_id.clone(),
            asset_ids: self
                .scope
                .iter()
                .map(|asset| asset.asset_id.clone())
                .collect(),
            provider: self.provider.clone(),
            primary_model: self.primary_model.clone(),
            allowed_models: self.allowed_models.clone(),
            allow_fallback: self.allow_fallback,
            payload: self.payload,
            include_project_vocabulary: self.context.project_vocabulary,
            budget_usd: self.budget_usd,
            parameters: self.parameters.clone(),
        }
    }

    pub fn moment_options(&self) -> Result<crate::analysis::moments::MomentRunOptions> {
        let RunPolicyParameters::MomentDiscovery {
            coarse_frames_per_asset,
            refinement_frames_per_asset,
            max_candidates_per_asset,
            profile,
            force,
            retry_uncertain,
        } = &self.parameters
        else {
            bail!("run policy is not a moment-discovery policy");
        };
        let payload = self.payload.moment_payload().ok_or_else(|| {
            anyhow::anyhow!("run policy payload is not supported by moment discovery")
        })?;
        Ok(crate::analysis::moments::MomentRunOptions {
            model: self.primary_model.clone(),
            provider: self.provider.clone(),
            payload,
            budget_usd: self.budget_usd,
            coarse_frames: *coarse_frames_per_asset,
            refinement_frames: *refinement_frames_per_asset,
            max_candidates: *max_candidates_per_asset,
            profile: *profile,
            force: *force,
            retry_uncertain: *retry_uncertain,
            allowed_models: self.allowed_models.clone(),
            allow_fallback: self.allow_fallback,
            policy_id: Some(self.policy_id.clone()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAuthorization {
    pub schema_version: String,
    pub authorized_at: String,
    pub policy: RunPolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PilotValidation {
    pub structural_schema_valid: bool,
    pub source_time_semantics_valid: bool,
    pub capability_observed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunPilotRecord {
    pub schema_version: String,
    pub policy_id: String,
    pub project_id: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: String,
    pub run_id: String,
    pub asset_id: String,
    pub requested_model: String,
    pub actual_model: Option<String>,
    pub provider: Option<String>,
    pub generation_id: Option<String>,
    pub attempted_models: Vec<String>,
    pub attempts: u32,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    /// Backward-compatible total recognized by this projection. It is the sum
    /// of confirmed charges, unresolved uncertainty and active reservations,
    /// never a claim that a reservation/estimate is a confirmed charge.
    pub cost_usd: f64,
    #[serde(default)]
    pub confirmed_cost_usd: f64,
    #[serde(default, skip_serializing_if = "pilot_estimate_is_zero")]
    pub estimated_cost_usd: f64,

    #[serde(default)]
    pub uncertain_usage_reserve_usd: f64,
    #[serde(default)]
    pub active_reserved_cost_usd: f64,
    pub usage_complete: bool,
    /// Stable machine category for why this record is not a clean pass.
    pub failure_code: String,
    /// Cost already incurred by earlier attempts for the same exact policy.
    /// Retries aggregate rather than overwrite this amount.
    pub prior_cost_usd: f64,
    /// Attempts already made by earlier records for the same exact policy.
    pub prior_attempts: u32,
    pub validation: PilotValidation,
    pub observed_capabilities: Vec<String>,
    pub warning: String,
    /// Bounded native capability-pilot facts. Omitted for legacy VLM records so
    /// their JSON wire shape remains unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_evidence: Option<NativePilotEvidence>,
    /// Exact locally persisted native-video capability response, possibly
    /// reused from a prior identical payload; never a new paid observation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_video_evidence_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizedPolicyState {
    pub authorization: RunAuthorization,
    pub pilot: RunPilotRecord,
    pub remaining_budget_usd: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PilotProjectionDisposition {
    Standard,
    NativeNoProviderRequired,
}

fn bounded(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn digest<T: Serialize>(value: &T) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}

fn validate_budget(budget_usd: f64) -> Result<()> {
    ensure!(
        budget_usd.is_finite() && budget_usd > 0.0,
        "run budget must be finite and greater than zero"
    );
    Ok(())
}

/// Load and re-validate one frozen run authorization on an existing SQLite
/// connection. Native transcript provenance uses this inside its owning
/// transaction so it consumes the same canonical policy identity check as the
/// public catalog API instead of re-parsing policy JSON independently.
///
/// The returned accounting contract is the immutable contract recorded when the
/// policy was authorized. Historical provenance reads inspect it but do not
/// require it to equal a newer current contract.
pub(crate) fn load_run_authorization_on_connection(
    connection: &Connection,
    policy_id: &str,
) -> Result<(RunAuthorization, String)> {
    let row: Option<(String, String, String, String)> = connection
        .query_row(
            "SELECT project_id,policy_json,authorized_at,accounting_contract
             FROM analysis_run_authorizations WHERE policy_id=?1",
            [policy_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let (project_id, policy_json, authorized_at, accounting_contract) =
        row.ok_or_else(|| anyhow::anyhow!("run policy is not authorized: {policy_id}"))?;
    let policy: RunPolicy = serde_json::from_str(&policy_json)
        .context("stored run authorization contains an unreadable policy")?;
    ensure!(
        policy.policy_id == policy_id,
        "stored authorization policy id mismatch"
    );
    ensure!(
        run_policy_identity(&policy)? == policy_id,
        "stored authorization contents no longer match the policy identity"
    );
    ensure!(
        policy.project_id == project_id,
        "stored authorization project mismatch"
    );
    Ok((
        RunAuthorization {
            schema_version: RUN_AUTHORIZATION_SCHEMA_VERSION.into(),
            authorized_at,
            policy,
        },
        accounting_contract,
    ))
}

pub(crate) fn pilot_authority_context(policy: &RunPolicy) -> AnalysisBudgetAuthorityContext {
    AnalysisBudgetAuthorityContext {
        project_id: policy.project_id.clone(),
        policy_id: policy.policy_id.clone(),
        policy_fingerprint: policy.policy_id.clone(),
        operation: "pilot".into(),
        provider: policy.provider.clone(),
        budget_scope_id: String::new(),
        budget_usd: policy.budget_usd,
    }
}

/// Verify (and, for a legacy-created policy, freeze) the one canonical budget
/// authority before opening a new bulk run row. Runtime callers must never
/// substitute a live remaining-balance value for this immutable whole-policy
/// ceiling.
pub(crate) fn verify_bulk_authority(
    catalog: &Catalog,
    policy: &RunPolicy,
) -> Result<AnalysisBudgetSnapshot> {
    for scope in &policy.scope {
        for unresolved in catalog.analysis_requests_needing_reconciliation_for_source(
            &policy.project_id,
            policy.kind.as_str(),
            &scope.source_identity,
        )? {
            ensure!(
                unresolved.policy_id == policy.policy_id,
                "a prior bulk analysis request has unresolved usage; reconcile its exact receipt before a replacement policy can run"
            );
        }
    }
    let authority =
        catalog.initialize_analysis_accounting_authority(&pilot_authority_context(policy))?;
    ensure!(
        !authority.dispatch_stopped,
        "analysis budget authority dispatch is stopped: {}",
        authority.stop_reason
    );
    Ok(authority)
}

/// Build the exact request identity for one bulk transmission against an
/// already-authorized whole-policy authority. `intended_attempt_key` must be
/// stable across restarts and independent of the run id so a completed or
/// uncertain prior attempt cannot be replayed by creating a fresh run.
pub(crate) fn bulk_request_accounting_context(
    policy: &RunPolicy,
    run_id: &str,
    asset_id: &str,
    intended_attempt_key: String,
    explicit_retry: bool,
) -> Result<AnalysisAccountingContext> {
    ensure!(
        !run_id.trim().is_empty(),
        "bulk accounting run id is required"
    );
    ensure!(
        !intended_attempt_key.trim().is_empty(),
        "bulk accounting intended attempt key is required"
    );
    let scope = policy
        .scope
        .iter()
        .find(|scope| scope.asset_id == asset_id)
        .ok_or_else(|| anyhow::anyhow!("asset is outside the authorized bulk scope"))?;
    ensure!(
        !scope.source_identity.trim().is_empty(),
        "authorized bulk source identity is missing"
    );
    Ok(AnalysisAccountingContext {
        project_id: policy.project_id.clone(),
        policy_id: policy.policy_id.clone(),
        policy_fingerprint: policy.policy_id.clone(),
        run_id: run_id.to_string(),
        operation: policy.kind.as_str().to_string(),
        intended_attempt_key,
        source_fingerprint: scope.source_identity.clone(),
        provider: policy.provider.clone(),
        budget_scope_id: String::new(),
        budget_usd: policy.budget_usd,
        explicit_retry,
    })
}

/// Build the exact pilot-accounting context for an explicitly selected frozen
/// scope entry. The VLM wrapper below retains its historical first-scope choice;
/// native orchestration uses the actual first speech asset after local scanning.
pub(crate) fn pilot_accounting_context_for_scope(
    policy: &RunPolicy,
    run_id: &str,
    scope: &RunAssetScope,
    explicit_retry: bool,
) -> Result<AnalysisAccountingContext> {
    ensure!(
        !run_id.trim().is_empty(),
        "pilot accounting run id is required"
    );
    ensure!(
        policy
            .scope
            .iter()
            .filter(|candidate| *candidate == scope)
            .count()
            == 1,
        "pilot accounting scope is not one exact frozen policy entry"
    );
    Ok(AnalysisAccountingContext {
        project_id: policy.project_id.clone(),
        policy_id: policy.policy_id.clone(),
        policy_fingerprint: policy.policy_id.clone(),
        run_id: run_id.to_string(),
        operation: "pilot".into(),
        // Deliberately excludes run_id and remains stable across explicit
        // retries, so a fresh run cannot bypass the duplicate/retry fence.
        intended_attempt_key: format!("{}:{}", policy.kind.as_str(), scope.asset_id),
        source_fingerprint: scope.source_identity.clone(),
        provider: policy.provider.clone(),
        budget_scope_id: String::new(),
        budget_usd: policy.budget_usd,
        explicit_retry,
    })
}

pub(crate) fn pilot_accounting_context(
    policy: &RunPolicy,
    run_id: &str,
    explicit_retry: bool,
) -> Result<AnalysisAccountingContext> {
    let first_scope = policy
        .scope
        .first()
        .ok_or_else(|| anyhow::anyhow!("run policy has no scoped asset for its pilot"))?;
    pilot_accounting_context_for_scope(policy, run_id, first_scope, explicit_retry)
}

fn canonical_models(request: &RunPolicyRequest) -> Vec<String> {
    let mut allowed = Vec::new();
    for model in &request.allowed_models {
        if !allowed.contains(model) {
            allowed.push(model.clone());
        }
    }
    if allowed.is_empty() {
        allowed.push(request.primary_model.clone());
        if request.allow_fallback {
            let alternate = if request.primary_model == DEFAULT_MODEL {
                FALLBACK_MODEL
            } else {
                DEFAULT_MODEL
            };
            allowed.push(alternate.to_string());
        }
    }
    allowed
}

fn permitted_fallbacks(primary: &str, allowed: &[String], allow_fallback: bool) -> Vec<String> {
    if !allow_fallback {
        return Vec::new();
    }
    let candidate = if primary == DEFAULT_MODEL {
        FALLBACK_MODEL
    } else {
        DEFAULT_MODEL
    };
    if allowed.iter().any(|model| model == candidate) {
        vec![candidate.to_string()]
    } else {
        Vec::new()
    }
}

fn capability_checks(
    request: &RunPolicyRequest,
    allowed: &[String],
    fallbacks: &[String],
) -> Vec<CapabilityCheck> {
    let mut checks = Vec::new();
    checks.push(CapabilityCheck {
        requirement: "provider.openrouter".into(),
        status: if request.provider == "openrouter" {
            CapabilityStatus::Passed
        } else {
            CapabilityStatus::Failed
        },
        detail: if request.provider == "openrouter" {
            "explicit OpenRouter provider selected".into()
        } else {
            "only the explicitly selected OpenRouter provider is supported".into()
        },
    });

    for model in std::iter::once(&request.primary_model)
        .chain(allowed.iter())
        .chain(fallbacks.iter())
    {
        let Some(spec) = model_spec(model) else {
            checks.push(CapabilityCheck {
                requirement: format!("model.{model}"),
                status: CapabilityStatus::Failed,
                detail: "model is not in the local approved registry".into(),
            });
            continue;
        };
        checks.push(CapabilityCheck {
            requirement: format!("model.{model}.image"),
            status: if spec.supports_image_input {
                CapabilityStatus::Passed
            } else {
                CapabilityStatus::Failed
            },
            detail: if spec.supports_image_input {
                "image input is registered for this model".into()
            } else {
                "model does not accept image input".into()
            },
        });
        checks.push(CapabilityCheck {
            requirement: format!("model.{model}.json_schema"),
            status: if spec.supports_json_schema {
                CapabilityStatus::Passed
            } else {
                CapabilityStatus::Failed
            },
            detail: if spec.supports_json_schema {
                "structured JSON response format is registered".into()
            } else {
                "model does not accept a JSON Schema response format".into()
            },
        });
        checks.push(CapabilityCheck {
            requirement: format!("model.{model}.transcript"),
            status: if request.payload.uses_transcript() {
                if spec.supports_transcript_input {
                    CapabilityStatus::Passed
                } else {
                    CapabilityStatus::Failed
                }
            } else {
                CapabilityStatus::NotRequired
            },
            detail: if request.payload.uses_transcript() {
                "accepted transcript text is used as optional context".into()
            } else {
                "payload does not include transcript context".into()
            },
        });
        checks.push(CapabilityCheck {
            requirement: format!("model.{model}.audio"),
            status: if request.payload.uses_audio() {
                if spec.supports_audio_input {
                    CapabilityStatus::Passed
                } else {
                    CapabilityStatus::Failed
                }
            } else {
                CapabilityStatus::NotRequired
            },
            detail: if request.payload.uses_audio() {
                "no registered model currently accepts audio input".into()
            } else {
                "payload does not include audio input".into()
            },
        });
    }

    if request.payload.uses_audio() {
        checks.push(CapabilityCheck {
            requirement: "payload.audio_backend".into(),
            status: CapabilityStatus::Failed,
            detail:
                "the audio analysis backend is not implemented; readiness fails before bulk work"
                    .into(),
        });
    }
    if request.payload == AnalysisPayload::Transcript {
        checks.push(CapabilityCheck {
            requirement: "payload.transcript_only".into(),
            status: CapabilityStatus::Failed,
            detail: "registered providers require image evidence; transcript-only analysis is unsupported".into(),
        });
    }
    checks
}

/// Existing vision pilot/provider/recovery entry points call this before any
/// local preparation, credential resolution or provider request. A future
/// native runtime must replace it with its own accounted boundary rather than
/// routing native audio through a VLM request.
pub(crate) fn ensure_vlm_run_kind(kind: AnalysisRunKind) -> Result<()> {
    if kind.is_vlm() {
        Ok(())
    } else {
        bail!(
            "native speech transcription is not a VLM operation; its accounted runtime is unavailable"
        )
    }
}

pub(crate) fn source_scope(
    catalog: &Catalog,
    asset: &Asset,
    payload: AnalysisPayload,
    add_legacy_audio_backend_blocker: bool,
) -> (RunAssetScope, Vec<RunPolicyBlocker>, Option<String>) {
    let source_duration_ms = (asset.duration_seconds.max(0.0) * 1_000.0).round() as u32;
    let proxy_identity = catalog.analysis_proxy_source(asset);
    // A frames-only or audio-only policy must not even bind or load transcript
    // content that it is not authorized to transmit. The identity is present
    // only when the payload selects current accepted transcript context, and it
    // reuses the same single analysis_proxy_source result as source identity.
    let transcript_fingerprint = if payload.uses_transcript() {
        match &proxy_identity {
            Ok(source) => catalog
                .current_transcript_state_with_proxy(asset, source)
                .ok()
                .and_then(|state| {
                    let transcript = state.current_transcript()?;
                    (transcript.status == TranscriptStatus::Speech
                        && !transcript.segments.is_empty())
                    .then(|| digest(transcript).ok())
                    .flatten()
                }),
            Err(_) => None,
        }
    } else {
        None
    };
    let (representation_fingerprint, sampled_representation_fingerprint, blockers) =
        match proxy_identity {
            Ok(source) => (
                Some(source.fingerprint),
                Some(source.sampled_fingerprint),
                Vec::new(),
            ),
            Err(error) => {
                let mut blockers = vec![RunPolicyBlocker {
                    code: "source_not_ready".into(),
                    message: format!(
                        "source representation is not ready: {}",
                        source_failure_code(&error)
                    ),
                    asset_id: Some(asset.id.clone()),
                }];
                if payload.uses_audio() && add_legacy_audio_backend_blocker {
                    blockers.push(RunPolicyBlocker {
                        code: "audio_backend_unavailable".into(),
                        message: "audio payload requires an implemented audio backend".into(),
                        asset_id: Some(asset.id.clone()),
                    });
                }
                (None, None, blockers)
            }
        };
    let source_identity = digest(&(
        &asset.id,
        &asset.fingerprint,
        source_duration_ms,
        &representation_fingerprint,
        &sampled_representation_fingerprint,
        &transcript_fingerprint,
    ))
    .unwrap_or_else(|_| String::new());
    (
        RunAssetScope {
            asset_id: asset.id.clone(),
            source_fingerprint: asset.fingerprint.clone(),
            source_duration_ms,
            representation: framepack::REPRESENTATION.into(),
            representation_fingerprint,
            sampled_representation_fingerprint,
            transcript_fingerprint,
            time_mapping: "proxy-v3.source-ms.identity.v1".into(),
            source_identity,
        },
        blockers,
        None,
    )
}

/// Canonical approval identity. Runtime readiness (credential presence and
/// transient blockers) is deliberately excluded; version, payload, context,
/// scope/source identities, models/fallbacks, parameters and budget are bound.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PolicyIdentity<'a> {
    schema_version: &'a str,
    project_id: &'a str,
    kind: AnalysisRunKind,
    provider: &'a str,
    primary_model: &'a str,
    allowed_models: &'a [String],
    allow_fallback: bool,
    permitted_fallbacks: &'a [String],
    payload: AnalysisPayload,
    context: &'a RunPolicyContext,
    scope: &'a [RunAssetScope],
    expected_cost_usd: f64,
    budget_usd: f64,
    parameters: &'a RunPolicyParameters,
    prompt_version: &'a str,
    output_schema_version: &'a str,
    recipe_version: &'a str,
    tool_version: &'a str,
    provider_cost_bound_contract: &'a str,
    capability_checks: &'a [CapabilityCheck],
}

pub(crate) fn run_policy_identity(policy: &RunPolicy) -> Result<String> {
    digest(&PolicyIdentity {
        schema_version: &policy.schema_version,
        project_id: &policy.project_id,
        kind: policy.kind,
        provider: &policy.provider,
        primary_model: &policy.primary_model,
        allowed_models: &policy.allowed_models,
        allow_fallback: policy.allow_fallback,
        permitted_fallbacks: &policy.permitted_fallbacks,
        payload: policy.payload,
        context: &policy.context,
        scope: &policy.scope,
        expected_cost_usd: policy.expected_cost_usd,
        budget_usd: policy.budget_usd,
        parameters: &policy.parameters,
        prompt_version: &policy.prompt_version,
        output_schema_version: &policy.output_schema_version,
        recipe_version: &policy.recipe_version,
        tool_version: &policy.tool_version,
        provider_cost_bound_contract: &policy.provider_cost_bound_contract,
        capability_checks: &policy.capability_checks,
    })
}

fn source_failure_code(error: &anyhow::Error) -> &'static str {
    if error.downcast_ref::<crate::RepresentationBusy>().is_some() {
        "representation_busy"
    } else {
        "source_not_ready"
    }
}

fn policy_context(
    catalog: &Catalog,
    project_id: &str,
    include_project_vocabulary: bool,
) -> Result<RunPolicyContext> {
    if !include_project_vocabulary {
        return Ok(RunPolicyContext::default());
    }
    let vocabulary = catalog.prompt_vocabulary(project_id)?;
    Ok(RunPolicyContext {
        project_vocabulary: true,
        vocabulary_fingerprint: Some(digest(&vocabulary)?),
        vocabulary_item_count: vocabulary.len(),
        // No trip-context source is wired into this build. The explicit false
        // records that absence rather than implying it was overlooked.
        trip_context: false,
        trip_context_fingerprint: None,
    })
}

fn load_run_pilot_on_connection(
    connection: &rusqlite::Connection,
    policy_id: &str,
) -> Result<Option<RunPilotRecord>> {
    let record: Option<String> = connection
        .query_row(
            "SELECT record_json FROM analysis_run_pilots WHERE policy_id=?1",
            [policy_id],
            |row| row.get(0),
        )
        .optional()?;
    record
        .map(|record| {
            let pilot: RunPilotRecord =
                serde_json::from_str(&record).context("stored pilot record is unreadable")?;
            ensure!(
                pilot.policy_id == policy_id,
                "stored pilot policy id mismatch"
            );
            Ok(pilot)
        })
        .transpose()
}

fn save_pilot_on_connection(
    connection: &rusqlite::Connection,
    record: &RunPilotRecord,
) -> Result<()> {
    let record_json = serde_json::to_string(record)?;
    connection.execute(
            "INSERT INTO analysis_run_pilots(policy_id,project_id,status,record_json,started_at,finished_at)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(policy_id) DO UPDATE SET status=excluded.status,record_json=excluded.record_json,
                started_at=excluded.started_at,finished_at=excluded.finished_at",
            params![
                record.policy_id,
                record.project_id,
                record.status,
                record_json,
                record.started_at,
                record.finished_at
            ],
        )?;
    Ok(())
}

impl Catalog {
    /// Recheck only the asset about to be transmitted, avoiding a full scope
    /// scan for every request in a large batch. The caller holds a policy that
    /// already passed the durable authorization/pilot checks at run admission.
    pub(crate) fn validate_run_policy_asset(
        &self,
        policy: &RunPolicy,
        asset_id: &str,
    ) -> Result<()> {
        if policy.kind == AnalysisRunKind::NativeVideoReview {
            return native_video::validate_asset(self, policy, asset_id, &|| false);
        }
        let asset = self.get_asset(asset_id)?;
        ensure!(
            asset.project_id == policy.project_id && !asset.removed,
            "authorized asset scope changed before transmission"
        );
        let approved = policy
            .scope
            .iter()
            .find(|scope| scope.asset_id == asset_id)
            .ok_or_else(|| anyhow::anyhow!("asset is outside authorized scope"))?;
        let (current, blockers, _) =
            source_scope(self, &asset, policy.payload, policy.kind.is_vlm());
        ensure!(
            blockers.is_empty() && &current == approved,
            "authorized source or transcript changed before transmission"
        );

        Ok(())
    }

    /// Freeze the approved vocabulary for this run. Analysis may itself add
    /// labels, but requests must keep using the original approved snapshot.
    pub(crate) fn run_policy_vocabulary(&self, policy: &RunPolicy) -> Result<Vec<String>> {
        ensure_vlm_run_kind(policy.kind)?;
        if !policy.context.project_vocabulary {
            return Ok(Vec::new());
        }
        let vocabulary = self.prompt_vocabulary(&policy.project_id)?;
        ensure!(
            Some(digest(&vocabulary)?) == policy.context.vocabulary_fingerprint,
            "authorized vocabulary changed before run admission"
        );
        Ok(vocabulary)
    }

    /// Materialize the proposed run without credentials, network access or
    /// catalog writes. Unknown or non-owned asset IDs fail immediately; source
    /// availability and capability problems become readiness blockers.
    pub fn describe_run_policy(&self, request: &RunPolicyRequest) -> Result<RunPolicy> {
        self.describe_run_policy_with_credentials(request, api_credential_present())
    }

    /// Canonical local preparation needs source/policy identity, not key access.
    /// Transient credential readiness is deliberately left unprobed here; the
    /// public descriptor still reports it and the paid transport resolves it.
    pub(crate) fn describe_run_policy_for_local_preparation(
        &self,
        request: &RunPolicyRequest,
    ) -> Result<RunPolicy> {
        self.describe_run_policy_with_credentials(request, false)
    }

    fn describe_run_policy_with_credentials(
        &self,
        request: &RunPolicyRequest,
        credentials_present: bool,
    ) -> Result<RunPolicy> {
        validate_budget(request.budget_usd)?;
        ensure!(
            !request.project_id.trim().is_empty(),
            "run project is required"
        );
        self.require_project(&request.project_id)?;
        ensure!(
            !request.asset_ids.is_empty() && request.asset_ids.len() <= MAX_SCOPE_ASSETS,
            "run scope must explicitly select 1..={MAX_SCOPE_ASSETS} asset ids"
        );
        let kind = request.parameters.kind();
        let mut seen = HashSet::new();
        let mut assets = Vec::new();
        for asset_id in &request.asset_ids {
            ensure!(
                seen.insert(asset_id.clone()),
                "duplicate asset id in run scope: {asset_id}"
            );
            let asset = self.get_asset(asset_id)?;
            ensure!(
                asset.project_id == request.project_id,
                "asset {asset_id} is outside the authorized project scope"
            );
            ensure!(
                !asset.removed
                    || matches!(&request.parameters,RunPolicyParameters::NativeVideoReview { options } if options.purpose==crate::NativeVideoPurpose::EndpointReview),
                "asset {asset_id} is excluded from analysis scope"
            );
            assets.push(asset);
        }

        if kind == AnalysisRunKind::NativeVideoReview {
            return native_video::describe(self, request, &assets);
        }
        let native_stt = kind == AnalysisRunKind::NativeSpeechTranscription;
        // Native speech is always represented as audio-only even when a
        // malformed caller selected another payload. The request is still
        // rejected by native_stt_blockers, but no transcript/vocabulary
        // identity is ever bound into the blocked policy.
        let effective_payload = if native_stt {
            AnalysisPayload::Audio
        } else {
            request.payload
        };
        let allowed = if native_stt {
            canonical_native_models(request)
        } else {
            canonical_models(request)
        };
        let fallbacks = if native_stt {
            Vec::new()
        } else {
            permitted_fallbacks(&request.primary_model, &allowed, request.allow_fallback)
        };
        let checks = if native_stt {
            native_stt_capability_checks(request, &allowed)
        } else {
            capability_checks(request, &allowed, &fallbacks)
        };
        let mut blockers = Vec::new();
        let mut warnings = Vec::new();
        let mut scope = Vec::new();
        for asset in &assets {
            let (entry, mut source_blockers, _) =
                source_scope(self, asset, effective_payload, !native_stt);
            scope.push(entry);
            blockers.append(&mut source_blockers);
        }

        if !request
            .asset_ids
            .iter()
            .any(|id| scope.iter().any(|scope| &scope.asset_id == id))
        {
            blockers.push(RunPolicyBlocker {
                code: "scope_empty".into(),
                message: "no exact project-owned assets were resolved".into(),
                asset_id: None,
            });
        }
        let native_spec = if native_stt {
            blockers.extend(native_stt_blockers(request));
            warnings.push(
                "native STT expected cost uses the registered full-window duration tariff and a conservative pre-VAD packed-window upper bound from source duration; the actual window count is unknown until runtime".into(),
            );
            warnings.push(
                "native STT max_price, order, only and ignore routing restrictions are not enforced by the transcription endpoint; the duration tariff is an external reservation assumption, not a hard invoice cap, and receipt settlement retains above-bound or unknown charges".into(),
            );
            warnings.push(native_stt::NATIVE_STT_BULK_UNAVAILABLE_WARNING.into());
            warnings.push(
                "an all-NoAudio/NoSpeech scope needs no provider credentials; detecting silence in audio-bearing sources requires VAD, while NoAudio needs no model. Paid speech-pilot readiness requires VAD and credentials".into(),
            );
            resolve_native_stt_model(&request.provider, &request.primary_model).ok()
        } else {
            None
        };
        if !native_stt {
            if request.provider != "openrouter" {
                blockers.push(RunPolicyBlocker {
                    code: "unsupported_provider".into(),
                    message: "only the explicit OpenRouter provider is supported".into(),
                    asset_id: None,
                });
            }
            if !model_spec(&request.primary_model).is_some_and(|spec| spec.supports_json_schema) {
                blockers.push(RunPolicyBlocker {
                    code: "unsupported_schema".into(),
                    message: "selected primary model does not accept the configured JSON schema"
                        .into(),
                    asset_id: None,
                });
            }
            if !allowed.iter().any(|model| model == &request.primary_model) {
                blockers.push(RunPolicyBlocker {
                    code: "primary_model_not_allowed".into(),
                    message: "selected primary model is not present in the permitted allowlist"
                        .into(),
                    asset_id: None,
                });
            }
            if request.allow_fallback && fallbacks.is_empty() {
                blockers.push(RunPolicyBlocker {
                    code: "unsupported_fallback".into(),
                    message:
                        "fallback was requested but no permitted fallback model is in the allowlist"
                            .into(),
                    asset_id: None,
                });
            }
            for fallback in &fallbacks {
                if !model_spec(fallback)
                    .is_some_and(|spec| spec.supports_image_input && spec.supports_json_schema)
                {
                    blockers.push(RunPolicyBlocker {
                        code: "unsupported_fallback".into(),
                        message: format!(
                            "permitted fallback {fallback} lacks required capabilities"
                        ),
                        asset_id: None,
                    });
                }
            }
            if request.payload.uses_audio() {
                blockers.push(RunPolicyBlocker {
                    code: "audio_backend_unavailable".into(),
                    message:
                        "audio analysis is represented in policy but no backend exists; readiness fails"
                            .into(),
                    asset_id: None,
                });
            }
            if request.payload == AnalysisPayload::Transcript {
                blockers.push(RunPolicyBlocker {
                    code: "unsupported_payload".into(),
                    message: "transcript-only payload has no supported provider path".into(),
                    asset_id: None,
                });
            }
            if !request.payload.uses_frames() {
                blockers.push(RunPolicyBlocker {
                    code: "unsupported_payload".into(),
                    message: "registered vision models require an image payload".into(),
                    asset_id: None,
                });
            }
            if request.payload.uses_transcript()
                && scope
                    .iter()
                    .all(|item| item.transcript_fingerprint.is_none())
            {
                warnings.push(
                    "no accepted current transcript was found; the frames payload can still proceed but transcript context will be absent"
                        .into(),
                );
            }
        }

        let (expected_cost_usd, cost_basis) = match native_spec {
            Some(spec) => native_stt::expected_cost_usd(&scope, spec)?,
            None if native_stt => (
                0.0,
                "native STT expected cost unavailable: no verified provider/model duration tariff was resolved"
                    .into(),
            ),
            None => (
                assets.len() as f64 * REFERENCE_COST_PER_CLIP_USD,
                "reference measurement; pilot spend is counted against the same ceiling".into(),
            ),
        };
        let capabilities_ready = checks
            .iter()
            .all(|check| check.status != CapabilityStatus::Failed);
        let budget_ready = expected_cost_usd <= request.budget_usd;
        if !budget_ready {
            blockers.push(RunPolicyBlocker {
                code: "expected_cost_exceeds_budget".into(),
                message: format!(
                    "expected cost ${:.6} exceeds the requested budget ${:.6}",
                    expected_cost_usd, request.budget_usd
                ),
                asset_id: None,
            });
        }
        // Transient VAD readiness is deliberately not a capability check and
        // therefore does not enter the canonical policy/recipe identity. Native
        // paid pilots need only the shared Silero artifact; the local ASR model is
        // never inspected or required. This check is skipped for VLM policies.
        let native_vad_ready = if native_stt {
            let selection = crate::SpeechBackendSelection::NativeOpenRouter {
                provider: request.provider.clone(),
                model: request.primary_model.clone(),
            };
            match self.model_status_for(&selection) {
                Ok(models) => models
                    .iter()
                    .any(|model| model.role == "voice_activity_detection" && model.ready),
                Err(_) => false,
            }
        } else {
            true
        };
        if native_stt && !native_vad_ready {
            warnings.push(format!(
                "native STT paid pilot requires the shared Silero VAD artifact; install it explicitly with `rcdvlog models install --backend native-openrouter --provider {} --model {}`. The all-local no-provider outcome can still run with no model when every selected source is truthfully NoAudio",
                request.provider, request.primary_model
            ));
        }
        if native_stt && !credentials_present {
            warnings.push(
                "native STT paid pilot provider credentials are not currently present; the all-local no-provider outcome may still run, while any speech window will fail locally before a provider request".into(),
            );
        }
        let native_pilot_prerequisites_ready =
            credentials_present && (!native_stt || native_vad_ready);
        let (prompt_version, schema_version, recipe_version) = match kind {
            AnalysisRunKind::Observation => (
                VLM_PROMPT_VERSION.to_string(),
                crate::analysis::schema::CLIP_OBSERVATION_SCHEMA_VERSION.to_string(),
                crate::analysis::FRAMEPACK_RECIPE_KEY.to_string(),
            ),
            AnalysisRunKind::MomentDiscovery => (
                MOMENT_PROMPT_VERSION.to_string(),
                MOMENT_SCHEMA_VERSION.to_string(),
                MOMENT_RECIPE_VERSION.to_string(),
            ),
            AnalysisRunKind::NativeSpeechTranscription => (
                native_stt::NATIVE_STT_PROMPT_VERSION.to_string(),
                native_stt::NATIVE_STT_OUTPUT_SCHEMA_VERSION.to_string(),
                native_stt_recipe_version(request)?,
            ),
            AnalysisRunKind::NativeVideoReview => {
                unreachable!("native video has its explicit descriptor")
            }
        };
        let context = if native_stt {
            RunPolicyContext::default()
        } else {
            policy_context(
                self,
                &request.project_id,
                request.include_project_vocabulary,
            )?
        };
        if kind == AnalysisRunKind::MomentDiscovery && context.project_vocabulary {
            blockers.push(RunPolicyBlocker {
                code: "unsupported_payload_context".into(),
                message:
                    "moment discovery does not transmit project vocabulary; remove that context approval"
                        .into(),
                asset_id: None,
            });
        }
        let readiness = RunPolicyReadiness {
            capabilities_ready,
            scope_ready: blockers.is_empty(),
            budget_ready,
            credentials_present,
            ready_for_authorization: capabilities_ready && blockers.is_empty() && budget_ready,
            ready_for_pilot: capabilities_ready
                && blockers.is_empty()
                && budget_ready
                && native_pilot_prerequisites_ready,
            blockers,
            warnings,
        };
        let mut policy = RunPolicy {
            schema_version: RUN_POLICY_SCHEMA_VERSION.into(),
            policy_id: String::new(),
            project_id: request.project_id.clone(),
            kind,
            provider: request.provider.clone(),
            primary_model: request.primary_model.clone(),
            allowed_models: allowed,
            allow_fallback: if native_stt {
                false
            } else {
                request.allow_fallback
            },
            permitted_fallbacks: fallbacks,
            payload: effective_payload,
            context,
            scope,
            expected_cost_usd,
            cost_basis,
            budget_usd: request.budget_usd,
            parameters: request.parameters.clone(),
            prompt_version,
            output_schema_version: schema_version,
            recipe_version,
            tool_version: RUN_POLICY_TOOL_VERSION.into(),
            provider_cost_bound_contract: if native_stt {
                native_stt_cost_bound_contract_id()
            } else {
                analysis_cost_bound_contract_id()
            },
            capability_checks: checks,
            readiness,
        };
        policy.policy_id = run_policy_identity(&policy)?;
        Ok(policy)
    }

    /// Persist an explicit approval of the exact, freshly materialized policy.
    /// Authorization is not a runtime capability observation and performs no
    /// network access. Unsupported choices fail before a row is written.
    pub fn authorize_run_policy(&self, request: &RunPolicyRequest) -> Result<RunAuthorization> {
        let policy = self.describe_run_policy(request)?;
        ensure!(
            policy.readiness.ready_for_authorization,
            "run policy is not ready for authorization: {}",
            policy
                .readiness
                .blockers
                .iter()
                .map(|blocker| blocker.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
        let policy_json = serde_json::to_string(&policy)?;
        let authorized_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        self.connection()?.execute(
            "INSERT INTO analysis_run_authorizations(
                policy_id,project_id,policy_json,authorized_at,accounting_contract)
             VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(policy_id) DO UPDATE SET
                policy_json=excluded.policy_json,
                authorized_at=excluded.authorized_at,
                accounting_contract=excluded.accounting_contract",
            params![
                policy.policy_id,
                policy.project_id,
                policy_json,
                authorized_at,
                crate::analysis::accounting::ANALYSIS_ACCOUNTING_SCHEMA_VERSION
            ],
        )?;
        self.load_run_authorization(&policy.policy_id)
    }

    pub fn load_run_authorization(&self, policy_id: &str) -> Result<RunAuthorization> {
        let connection = self.connection()?;
        Ok(load_run_authorization_on_connection(&connection, policy_id)?.0)
    }

    pub fn load_run_pilot(&self, policy_id: &str) -> Result<Option<RunPilotRecord>> {
        load_run_pilot_on_connection(&self.connection()?, policy_id)
    }

    /// Execute exactly one bounded capability/schema/editorial-semantic pilot.
    /// The first scoped asset is sampled with the same frame pack or editorial
    /// source-time contract used by the real batch. A successful result is
    /// persisted as observed capability only.
    pub fn run_run_policy_pilot_with_provider(
        &self,
        request: &RunPolicyRequest,
        provider: &dyn VlmProvider,
        retry_uncertain: bool,
    ) -> Result<RunPilotRecord> {
        self.run_run_policy_pilot_with_provider_and_cancel(
            request,
            provider,
            retry_uncertain,
            || false,
        )
    }

    /// Cancellation-aware pilot boundary. Local frame work uses the shared
    /// local-heavy lease, and cancellation is checked again immediately before
    /// the paid transmission boundary. Cancelling early therefore performs zero
    /// provider calls and records no paid pilot.
    pub fn run_run_policy_pilot_with_provider_and_cancel<C>(
        &self,
        request: &RunPolicyRequest,
        provider: &dyn VlmProvider,
        retry_uncertain: bool,
        mut is_cancelled: C,
    ) -> Result<RunPilotRecord>
    where
        C: FnMut() -> bool,
    {
        ensure_vlm_run_kind(request.parameters.kind())?;
        if is_cancelled() {
            bail!("pilot cancelled before provider transmission");
        }
        let session = match prepare_pilot_session(self, request, retry_uncertain)? {
            PilotSessionStart::AlreadyPassed(record) | PilotSessionStart::Cached(record) => {
                return Ok(*record);
            }
            PilotSessionStart::Ready(session) => *session,
        };
        let prepared = prepare_pilot_transmission(
            self,
            request,
            &session.policy,
            &session.first_scope,
            &mut is_cancelled,
        )?;

        let started_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let run_id = self.begin_accounted_analysis_run(
            &session.policy.project_id,
            &session.policy.policy_id,
            "pilot",
        )?;
        let running = self.start_owned_pilot_run(&session, &run_id, &started_at)?;

        if is_cancelled() {
            self.finalize_pilot_projection(
                &running,
                &session.baseline,
                PilotValidation {
                    structural_schema_valid: false,
                    source_time_semantics_valid: false,
                    capability_observed: false,
                    detail: "pilot_cancelled_before_transmission".into(),
                },
                Some("pilot_cancelled_before_transmission".into()),
            )?;
            bail!("pilot cancelled before provider transmission");
        }

        match execute_accounted_pilot_attempts(
            self,
            &session.policy,
            provider,
            &prepared,
            &run_id,
            retry_uncertain,
            &mut is_cancelled,
        ) {
            Ok(execution) if execution.cancelled_before_domain_fallback => {
                self.finalize_pilot_projection(
                    &running,
                    &session.baseline,
                    execution.validation,
                    Some("pilot_cancelled_before_domain_fallback".into()),
                )?;
                bail!("pilot cancelled before provider transmission");
            }
            Ok(execution) => self.finalize_pilot_projection(
                &running,
                &session.baseline,
                execution.validation,
                None,
            ),
            Err(error) => {
                let failure = ProviderFailure::from_anyhow(&error);
                let has_new_request = self
                    .list_analysis_requests(
                        &session.policy.project_id,
                        Some(&session.policy.policy_id),
                        1000,
                    )?
                    .iter()
                    .any(|request| request.run_id == running.run_id);
                if failure.category.is_local() && session.had_previous_pilot && !has_new_request {
                    // A local denial never transmitted. Preserve the prior
                    // pilot's unknown/failed evidence and close this audit run
                    // at zero recognized cost.
                    if let Some(prior) = session.previous_record.as_ref() {
                        self.save_pilot(prior)?;
                    }
                    let safe = bounded(&failure.safe_detail(), 240);
                    self.finish_analysis_run(
                        &running.run_id,
                        &crate::AnalysisRunOutcome {
                            status: "failed".into(),
                            stage_reached: "pilot".into(),
                            cost_usd: 0.0,
                            error: safe,
                            ..Default::default()
                        },
                    )?;
                    return Err(error);
                }
                let record = self.finalize_pilot_projection(
                    &running,
                    &session.baseline,
                    PilotValidation {
                        structural_schema_valid: false,
                        source_time_semantics_valid: false,
                        capability_observed: false,
                        detail: failure.safe_detail(),
                    },
                    Some(failure.safe_detail()),
                )?;
                if failure.category.is_local() {
                    return Err(error);
                }
                Ok(record)
            }
        }
    }

    /// Explicit, durable authorization for one exact prior pilot attempt. This
    /// is the only way a later pilot run may consider a new epoch for that same
    /// intended request. A generic `force` flag alone is not sufficient.
    pub fn authorize_run_policy_pilot_retry(
        &self,
        policy_id: &str,
        request_key: &str,
    ) -> Result<AnalysisRetryAuthorization> {
        let authorization = self.load_run_authorization(policy_id)?;
        ensure_vlm_run_kind(authorization.policy.kind)?;
        let request = self
            .analysis_request(request_key)?
            .ok_or_else(|| anyhow::anyhow!("analysis request not found"))?;
        ensure!(
            request.policy_id == policy_id,
            "prior analysis request does not belong to this exact run policy"
        );
        ensure!(
            request.operation == "pilot",
            "prior analysis request is not a pilot attempt"
        );
        self.authorize_analysis_request_retry(request_key)
    }

    /// Open the owned pilot row after authority initialization. Attempt/model
    /// and token fields come from the canonical ledger plus any pre-ledger
    /// historical baseline, never from optimistic runtime counters.
    fn start_owned_pilot_run(
        &self,
        session: &PilotSession,
        run_id: &str,
        started_at: &str,
    ) -> Result<RunPilotRecord> {
        let initial = self.analysis_accounting_aggregate_for_operation(
            &session.policy.policy_id,
            "pilot",
            Some(run_id),
        )?;
        let ledger =
            self.analysis_operation_attempt_projection(&session.policy.policy_id, "pilot")?;
        let mut attempted_models = session.baseline.attempted_models.clone();
        attempted_models.extend(ledger.attempted_models);
        let running = RunPilotRecord {
            schema_version: RUN_PILOT_SCHEMA_VERSION.into(),
            policy_id: session.policy.policy_id.clone(),
            project_id: session.policy.project_id.clone(),
            status: "running".into(),
            started_at: started_at.to_string(),
            finished_at: String::new(),
            run_id: run_id.to_string(),
            asset_id: session.first_scope.asset_id.clone(),
            requested_model: session.policy.primary_model.clone(),
            actual_model: None,
            provider: None,
            generation_id: None,
            attempted_models,
            attempts: session
                .baseline
                .attempts
                .saturating_add(ledger.actual_attempt_count),
            prompt_tokens: session
                .baseline
                .prompt_tokens
                .saturating_add(ledger.prompt_tokens),
            completion_tokens: session
                .baseline
                .completion_tokens
                .saturating_add(ledger.completion_tokens),
            cost_usd: initial.committed_or_reserved_usd,
            confirmed_cost_usd: initial.confirmed_cost_usd,
            estimated_cost_usd: initial.estimated_cost_usd,
            uncertain_usage_reserve_usd: initial.uncertain_usage_reserve_usd,
            active_reserved_cost_usd: initial.active_reserved_cost_usd,
            usage_complete: !initial.has_unresolved_usage(),
            failure_code: String::new(),
            prior_cost_usd: session.prior_cost_usd,
            prior_attempts: session.prior_attempts,
            validation: PilotValidation {
                structural_schema_valid: false,
                source_time_semantics_valid: false,
                capability_observed: false,
                detail: "pilot_running".into(),
            },
            observed_capabilities: Vec::new(),
            warning: PILOT_WARNING.into(),
            native_evidence: None,
            native_video_evidence_id: None,
        };
        self.save_pilot(&running)?;
        Ok(running)
    }

    fn finalize_pilot_projection(
        &self,
        running: &RunPilotRecord,
        baseline: &PilotAttemptBaseline,
        validation: PilotValidation,
        failure_hint: Option<String>,
    ) -> Result<RunPilotRecord> {
        self.finalize_pilot_projection_with_disposition(
            running,
            baseline,
            validation,
            failure_hint,
            PilotProjectionDisposition::Standard,
        )
    }

    /// Finalize the truthful all-local native outcome. This uses the exact same
    /// immediate transaction, ledger projection and record writer as the paid
    /// pilot; only the explicit status/warning differ. The same transaction
    /// re-proves the native policy, complete distinct local facts, zero actual
    /// attempts and no unresolved accounting evidence.
    fn finalize_native_no_provider_projection(
        &self,
        running: &RunPilotRecord,
        baseline: &PilotAttemptBaseline,
    ) -> Result<RunPilotRecord> {
        self.finalize_pilot_projection_with_disposition(
            running,
            baseline,
            PilotValidation {
                structural_schema_valid: false,
                source_time_semantics_valid: false,
                capability_observed: false,
                detail: "native_local_scan_no_provider_request_required".into(),
            },
            Some("no_provider_required".into()),
            PilotProjectionDisposition::NativeNoProviderRequired,
        )
    }

    fn finalize_pilot_projection_with_disposition(
        &self,
        running: &RunPilotRecord,
        baseline: &PilotAttemptBaseline,
        validation: PilotValidation,
        failure_hint: Option<String>,
        disposition: PilotProjectionDisposition,
    ) -> Result<RunPilotRecord> {
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current = load_run_pilot_on_connection(&transaction, &running.policy_id)?;
        let native_evidence = match current.as_ref() {
            Some(current) if current.run_id == running.run_id => current.native_evidence.clone(),
            _ => running.native_evidence.clone(),
        };
        let native_video_evidence_id = current
            .as_ref()
            .filter(|c| c.run_id == running.run_id)
            .and_then(|c| c.native_video_evidence_id.clone())
            .or_else(|| running.native_video_evidence_id.clone());
        let aggregate = crate::analysis::accounting::store::accounting_aggregate(
            &transaction,
            &running.policy_id,
            Some("pilot"),
            Some(&running.run_id),
        )?;
        let ledger = crate::analysis::accounting::store::operation_attempt_projection(
            &transaction,
            &running.policy_id,
            "pilot",
        )?;
        if disposition == PilotProjectionDisposition::NativeNoProviderRequired {
            let current = current
                .as_ref()
                .filter(|current| current.run_id == running.run_id && current.status == "running")
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "native no-provider finalization lost ownership of the running pilot row"
                    )
                })?;
            native_pilot_evidence::validate_native_no_provider_finalization(&transaction, current)?;
            ensure!(
                aggregate.request_count == 0
                    && ledger.actual_attempt_count == 0
                    && !aggregate.has_unresolved_usage()
                    && aggregate.committed_or_reserved_usd == 0.0,
                "native no-provider finalization requires zero actual requests, zero unresolved usage and zero ledger cost"
            );
        }
        let mut attempted_models = baseline.attempted_models.clone();
        attempted_models.extend(ledger.attempted_models);
        let attempts = baseline
            .attempts
            .saturating_add(ledger.actual_attempt_count);
        let prompt_tokens = baseline.prompt_tokens.saturating_add(ledger.prompt_tokens);
        let completion_tokens = baseline
            .completion_tokens
            .saturating_add(ledger.completion_tokens);
        let unresolved = aggregate.has_unresolved_usage();
        let usage_complete = !unresolved;
        let (status, failure_code, warning) = pilot_projection_status(
            disposition,
            &validation,
            usage_complete,
            failure_hint.as_deref(),
        );
        let observed_capabilities =
            pilot_observed_capabilities(&validation, native_evidence.as_ref());
        let record = RunPilotRecord {
            schema_version: RUN_PILOT_SCHEMA_VERSION.into(),
            policy_id: running.policy_id.clone(),
            project_id: running.project_id.clone(),
            status: status.into(),
            started_at: running.started_at.clone(),
            finished_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            run_id: running.run_id.clone(),
            asset_id: running.asset_id.clone(),
            requested_model: running.requested_model.clone(),
            actual_model: aggregate.current_run_actual_model.clone(),
            provider: aggregate.current_run_actual_provider.clone(),
            generation_id: aggregate.current_run_generation_id.clone(),
            attempted_models,
            attempts,
            prompt_tokens,
            completion_tokens,
            cost_usd: aggregate.committed_or_reserved_usd,
            confirmed_cost_usd: aggregate.confirmed_cost_usd,
            estimated_cost_usd: aggregate.estimated_cost_usd,
            uncertain_usage_reserve_usd: aggregate.uncertain_usage_reserve_usd,
            active_reserved_cost_usd: aggregate.active_reserved_cost_usd,
            usage_complete,
            failure_code: failure_code.clone(),
            prior_cost_usd: running.prior_cost_usd,
            prior_attempts: running.prior_attempts,
            validation: validation.clone(),
            observed_capabilities,
            warning,
            native_evidence,
            native_video_evidence_id,
        };
        // A late result may finish its audit run, but cannot replace a newer
        // pilot projection. The same transaction preserves any attached sample.
        if current
            .as_ref()
            .is_some_and(|current| current.run_id == running.run_id)
        {
            save_pilot_on_connection(&transaction, &record)?;
        }
        transaction.commit()?;
        self.finish_analysis_run(
            &running.run_id,
            &crate::AnalysisRunOutcome {
                status: if status == "passed" || status == "no_provider_required" {
                    "completed".into()
                } else {
                    "failed".into()
                },
                stage_reached: "pilot".into(),
                cost_usd: record.cost_usd,
                error: if status == "passed" || status == "no_provider_required" {
                    String::new()
                } else {
                    bounded(failure_hint.as_deref().unwrap_or(&validation.detail), 240)
                },
                ..Default::default()
            },
        )?;
        Ok(record)
    }

    /// Cancellation-aware native capability pilot with an injected transport.
    /// The local VAD/FFmpeg work, accounting boundary and projection are the real
    /// production owners; this seam exists so focused tests can avoid live HTTP.
    pub fn run_native_speech_pilot_with_transport_and_cancel<C>(
        &self,
        request: &RunPolicyRequest,
        transport: &dyn crate::NativeSttTransport,
        retry_uncertain: bool,
        mut is_cancelled: C,
    ) -> Result<RunPilotRecord>
    where
        C: FnMut() -> bool,
    {
        let mut preparer = crate::transcription::NativeSpeechPreparer::new();
        native_pilot::run_native_pilot_with_transport(
            self,
            request,
            transport,
            retry_uncertain,
            &mut preparer,
            &mut is_cancelled,
        )
    }

    /// Convenience injected native pilot entry point without a cancellation hook.
    pub fn run_native_speech_pilot_with_transport(
        &self,
        request: &RunPolicyRequest,
        transport: &dyn crate::NativeSttTransport,
        retry_uncertain: bool,
    ) -> Result<RunPilotRecord> {
        self.run_native_speech_pilot_with_transport_and_cancel(
            request,
            transport,
            retry_uncertain,
            || false,
        )
    }

    /// Focused test seam for the same native orchestration with a locally
    /// injected VAD loader/probe/extract hook. It is never production-visible.
    #[cfg(test)]
    pub(crate) fn run_native_speech_pilot_with_transport_and_preparer<C>(
        &self,
        request: &RunPolicyRequest,
        transport: &dyn crate::NativeSttTransport,
        retry_uncertain: bool,
        preparer: &mut crate::transcription::NativeSpeechPreparer,
        is_cancelled: &mut C,
    ) -> Result<RunPilotRecord>
    where
        C: FnMut() -> bool,
    {
        native_pilot::run_native_pilot_with_transport(
            self,
            request,
            transport,
            retry_uncertain,
            preparer,
            is_cancelled,
        )
    }

    /// Real native capability-pilot entry point. Credential resolution is
    /// deferred inside the transport until a speech window actually reaches the
    /// accounted pre-dispatch preparation step; all-local silence never reads a
    /// key or opens a client.
    pub fn run_native_speech_pilot(
        &self,
        request: &RunPolicyRequest,
        retry_uncertain: bool,
    ) -> Result<RunPilotRecord> {
        let transport = native_pilot::DeferredEnvironmentNativeSttTransport::new();
        self.run_native_speech_pilot_with_transport(request, &transport, retry_uncertain)
    }

    /// CLI-facing pilot entry point. VLM credentials are checked before a
    /// provider client is built; the explicitly selected native kind routes to
    /// its own deferred native transport and never constructs a chat request.
    pub fn run_run_policy_pilot(
        &self,
        request: &RunPolicyRequest,
        retry_uncertain: bool,
    ) -> Result<RunPilotRecord> {
        if request.parameters.kind() == AnalysisRunKind::NativeVideoReview {
            ensure!(
                !retry_uncertain,
                "native video does not support implicit pilot retry; inspect/reconcile the exact prior request"
            );
            return self.run_native_video_pilot(request, &|| false);
        }
        if request.parameters.kind() == AnalysisRunKind::NativeSpeechTranscription {
            return self.run_native_speech_pilot(request, retry_uncertain);
        }
        ensure_vlm_run_kind(request.parameters.kind())?;
        ensure!(
            api_credential_present(),
            "OpenRouter credentials are not available; no pilot request was made"
        );
        let key = resolve_api_key().context("resolve OpenRouter credentials for pilot")?;
        let provider = OpenRouterProvider::new(key)?;
        self.run_run_policy_pilot_with_provider(request, &provider, retry_uncertain)
    }

    pub fn authorized_policy_for_bulk(
        &self,
        policy_id: &str,
        expected_kind: AnalysisRunKind,
    ) -> Result<AuthorizedPolicyState> {
        let authorization = self.load_run_authorization(policy_id)?;
        let kind = authorization.policy.kind;
        ensure!(
            kind.is_vlm()
                || matches!(
                    kind,
                    AnalysisRunKind::NativeSpeechTranscription | AnalysisRunKind::NativeVideoReview
                ),
            "unsupported authorized bulk policy kind"
        );
        ensure!(
            expected_kind.is_vlm()
                || matches!(
                    expected_kind,
                    AnalysisRunKind::NativeSpeechTranscription | AnalysisRunKind::NativeVideoReview
                ),
            "unsupported requested bulk kind"
        );
        ensure!(
            kind == expected_kind,
            "authorized policy kind does not match the requested bulk operation"
        );
        let current = if matches!(
            kind,
            AnalysisRunKind::NativeSpeechTranscription | AnalysisRunKind::NativeVideoReview
        ) {
            self.describe_run_policy_for_local_preparation(&authorization.policy.request())?
        } else {
            self.describe_run_policy(&authorization.policy.request())?
        };
        ensure!(
            current.policy_id == policy_id,
            "run policy or source identity changed; the prior authorization is invalid"
        );
        ensure!(
            current.readiness.ready_for_authorization,
            "authorized policy is no longer ready"
        );
        let pilot = self
            .load_run_pilot(policy_id)?
            .ok_or_else(|| anyhow::anyhow!("no capability pilot is recorded for this policy"))?;
        ensure!(
            pilot.policy_id == policy_id && pilot.project_id == current.project_id,
            "capability pilot does not belong to this exact policy"
        );
        ensure!(
            pilot.status == "passed" && pilot.failure_code.is_empty(),
            "capability pilot has not passed for this policy"
        );
        ensure!(pilot.usage_complete, "capability pilot usage is incomplete");
        if kind == AnalysisRunKind::NativeVideoReview {
            self.require_native_video_pilot(&current)?;
            verify_bulk_authority(self, &current)?;
        }
        ensure!(
            pilot.cost_usd.is_finite() && pilot.cost_usd >= 0.0,
            "capability pilot cost is invalid"
        );
        if kind == AnalysisRunKind::NativeSpeechTranscription {
            ensure!(
                pilot.validation.structural_schema_valid
                    && pilot.validation.source_time_semantics_valid
                    && pilot.validation.capability_observed,
                "capability pilot did not observe the native protocol/source contract"
            );
            let sample = pilot
                .native_evidence
                .as_ref()
                .and_then(|evidence| evidence.sample.as_ref())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "native paid capability evidence is missing; local/no-provider pilot results cannot authorize an upload"
                    )
                })?;
            ensure!(
                matches!(sample.outcome, NativePilotSampleOutcome::Evaluated { .. }),
                "native paid capability evidence is protocol-unusable; targeted recovery is required"
            );
            ensure!(
                sample.requested_provider == current.provider
                    && sample.requested_model == current.primary_model
                    && sample.scope.asset_id == pilot.asset_id,
                "native paid capability evidence does not match the frozen provider/model"
            );
            let sample_request = self
                .analysis_request(&sample.request_key)?
                .ok_or_else(|| anyhow::anyhow!("native paid capability request is missing"))?;
            ensure!(
                sample_request.operation == "pilot"
                    && sample_request.run_id == pilot.run_id
                    && sample_request.policy_id == policy_id
                    && sample_request.project_id == current.project_id
                    && sample_request.provider == current.provider
                    && sample_request.requested_model == current.primary_model
                    && sample_request.lifecycle.is_terminal()
                    && sample_request
                        .cost_bound
                        .as_ref()
                        .is_some_and(|bound| bound.audio.is_some()),
                "native paid capability request is not the exact settled pilot audio window"
            );
            // This is the same whole-policy authority the pilot froze. A
            // replacement policy cannot create a fresh remaining-balance
            // ceiling, and a stopped/overrun authority is never bypassed.
            let authority = verify_bulk_authority(self, &current)?;
            ensure!(
                !authority.dispatch_stopped,
                "native speech budget authority dispatch is stopped: {}",
                authority.stop_reason
            );
        }
        // Remaining budget is canonical whole-policy exposure, not a separate
        // add-up of pilot summary fields. Active reservations are legitimate
        // concurrent attempts and are accounted by atomic admission; only
        // unknown/interrupted usage blocks a new bulk run before it starts.
        let aggregate = self.analysis_accounting_aggregate(policy_id, Some(&pilot.run_id))?;
        if kind == AnalysisRunKind::NativeVideoReview {
            ensure!(
                aggregate.active_request_count == 0,
                "a prior native video operation still has active or interrupted requests; inspect/reconcile them before a new run"
            );
        }
        ensure!(
            !aggregate.has_unknown_usage(),
            "canonical analysis usage is unknown or interrupted; reconcile request evidence before bulk work"
        );
        let remaining_budget_usd = current.budget_usd - aggregate.committed_or_reserved_usd;
        ensure!(
            remaining_budget_usd > 0.0,
            "capability pilot consumed the authorized run budget"
        );
        Ok(AuthorizedPolicyState {
            authorization,
            pilot,
            remaining_budget_usd,
        })
    }

    /// Refresh a derived pilot projection only while the exact record read by
    /// recovery is still current. SQLite serializes this compare-and-write;
    /// a new runtime run or newer reconciliation cannot be overwritten.
    pub(crate) fn save_pilot_if_unchanged(
        &self,
        expected: &RunPilotRecord,
        replacement: &RunPilotRecord,
    ) -> Result<bool> {
        ensure!(
            expected.policy_id == replacement.policy_id
                && expected.project_id == replacement.project_id,
            "pilot recovery owner changed"
        );
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let raw: Option<String> = transaction
            .query_row(
                "SELECT record_json FROM analysis_run_pilots WHERE policy_id=?1",
                [&expected.policy_id],
                |row| row.get(0),
            )
            .optional()?;
        let current = raw
            .map(|value| serde_json::from_str::<RunPilotRecord>(&value))
            .transpose()?;
        if current.as_ref() != Some(expected) {
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute(
            "UPDATE analysis_run_pilots SET status=?2,record_json=?3,started_at=?4,finished_at=?5
             WHERE policy_id=?1 AND project_id=?6",
            params![
                replacement.policy_id,
                replacement.status,
                serde_json::to_string(replacement)?,
                replacement.started_at,
                replacement.finished_at,
                replacement.project_id
            ],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub(crate) fn save_pilot(&self, record: &RunPilotRecord) -> Result<()> {
        save_pilot_on_connection(&self.connection()?, record)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod recovery_tests;

fn pilot_estimate_is_zero(value: &f64) -> bool {
    *value == 0.0
}

mod native_video;

pub(crate) use native_video::accepted_transcript as accepted_native_video_transcript;
pub(crate) use native_video::validate_asset as validate_native_video_asset;
pub(crate) use native_video::validate_asset_mode as validate_native_video_asset_mode;
