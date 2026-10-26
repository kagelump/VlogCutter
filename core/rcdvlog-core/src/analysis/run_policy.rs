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

mod native_pilot;
mod pilot_projection;
use pilot_projection::{pilot_observed_capabilities, pilot_projection_status};
mod native_pilot_evidence;
pub(crate) use native_pilot_evidence::reusable_native_pilot_sample_on_transaction;
mod native_stt;
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
}

impl AnalysisRunKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observation => "observation",
            Self::MomentDiscovery => "moment_discovery",
            Self::NativeSpeechTranscription => "native_speech_transcription",
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
}

impl AnalysisPayload {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Frames => "frames",
            Self::FramesAndTranscript => "frames-and-transcript",
            Self::Audio => "audio",
            Self::AudioAndTranscript => "audio-and-transcript",
            Self::Transcript => "transcript",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "frames" => Some(Self::Frames),
            "frames-and-transcript" | "frames_and_transcript" => Some(Self::FramesAndTranscript),
            "audio" => Some(Self::Audio),
            "audio-and-transcript" | "audio_and_transcript" => Some(Self::AudioAndTranscript),
            "transcript" => Some(Self::Transcript),
            _ => None,
        }
    }

    pub fn uses_frames(self) -> bool {
        matches!(self, Self::Frames | Self::FramesAndTranscript)
    }

    pub fn uses_audio(self) -> bool {
        matches!(self, Self::Audio | Self::AudioAndTranscript)
    }

    pub fn uses_transcript(self) -> bool {
        matches!(
            self,
            Self::FramesAndTranscript | Self::AudioAndTranscript | Self::Transcript
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

impl RunPolicyParameters {
    pub fn kind(&self) -> AnalysisRunKind {
        match self {
            Self::Observation { .. } => AnalysisRunKind::Observation,
            Self::MomentDiscovery { .. } => AnalysisRunKind::MomentDiscovery,
            Self::NativeSpeechTranscription { .. } => AnalysisRunKind::NativeSpeechTranscription,
        }
    }

    pub fn force(&self) -> bool {
        match self {
            Self::Observation { force }
            | Self::MomentDiscovery { force, .. }
            | Self::NativeSpeechTranscription { force, .. } => *force,
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

fn run_policy_identity(policy: &RunPolicy) -> Result<String> {
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
                !asset.removed,
                "asset {asset_id} is excluded from analysis scope"
            );
            assets.push(asset);
        }

        let kind = request.parameters.kind();
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
        ensure_vlm_run_kind(authorization.policy.kind)?;
        ensure_vlm_run_kind(expected_kind)?;
        ensure!(
            authorization.policy.kind == expected_kind,
            "authorized policy kind does not match the requested bulk operation"
        );
        let current = self.describe_run_policy(&authorization.policy.request())?;
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
        ensure!(
            pilot.cost_usd.is_finite() && pilot.cost_usd >= 0.0,
            "capability pilot cost is invalid"
        );
        // Remaining budget is canonical whole-policy exposure, not a separate
        // add-up of pilot summary fields. Active reservations are legitimate
        // concurrent attempts and are accounted by atomic admission; only
        // unknown/interrupted usage blocks a new bulk run before it starts.
        let aggregate = self.analysis_accounting_aggregate(policy_id, Some(&pilot.run_id))?;
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
mod tests {
    mod native_boundary_tests;
    mod native_tests;
    use super::*;
    use crate::ProviderReceiptEvidence;
    use crate::analysis::moments::{MomentPayload, MomentRunOptions};
    use crate::analysis::provider::{
        AnalysisRequestCostBound, ProviderFailure, ProviderFailureCategory, VlmReceipt, VlmRequest,
        VlmResponse,
    };
    use crate::analysis::quality::DecodedSegment;
    use crate::analysis::store::{StoredTranscript, TranscriptOutcomeReason, save_transcript};
    use rusqlite::params;
    use serde_json::{Value, json};
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Barrier, Mutex};
    use tempfile::tempdir;

    struct FakeProvider {
        calls: AtomicUsize,
        body: String,
        fail_transport: bool,
        fail_prepare: bool,
    }

    impl FakeProvider {
        fn observation() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                body: serde_json::json!({
                    "title": "Synthetic pilot",
                    "description": "A bounded synthetic observation.",
                    "labels": [],
                    "usable": true,
                    "unusableReason": null,
                    "bestMomentStartMs": null,
                    "bestMomentEndMs": null,
                    "thumbnailTimestampMs": null,
                    "cameraMovement": "static",
                    "ocrText": []
                })
                .to_string(),
                fail_transport: false,
                fail_prepare: false,
            }
        }

        fn moments(body: serde_json::Value) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                body: body.to_string(),
                fail_transport: false,
                fail_prepare: false,
            }
        }
    }

    impl VlmProvider for FakeProvider {
        fn prepare(&self) -> Result<()> {
            if self.fail_prepare {
                bail!("synthetic credentials unavailable");
            }
            Ok(())
        }

        fn analysis_cost_bound(
            &self,
            _model: &str,
            _request: &VlmRequest,
        ) -> Result<AnalysisRequestCostBound> {
            Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
                0.02,
            ))
        }

        fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
            let _call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_transport {
                bail!("synthetic transport interruption");
            }
            Ok(VlmResponse {
                model: model.into(),
                body: self.body.clone(),
                prompt_tokens: 10,
                completion_tokens: 5,
                cost_usd: 0.01,
                latency_ms: 1,
                receipt: VlmReceipt {
                    actual_model: Some(model.into()),
                    provider: Some("synthetic".into()),
                    generation_id: Some(format!("generation-{}", uuid::Uuid::new_v4())),
                    usage_complete: true,
                },
            })
        }
    }

    fn generated_asset(
        catalog: &Catalog,
        project_id: &str,
        root: &std::path::Path,
        id: &str,
        seconds: u32,
    ) -> String {
        let source_dir = root.join("source");
        fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join(format!("{id}.mp4"));
        let status = crate::tool_command("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
            ])
            .arg(format!(
                "color=c=0x{}:s=320x180:r=24:d={seconds}",
                &blake3::hash(id.as_bytes()).to_hex()[..6]
            ))
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&source)
            .status();
        let Ok(status) = status else {
            panic!("synthetic run-policy fixture requires ffmpeg")
        };
        assert!(status.success(), "synthetic fixture generation failed");
        crate::tests::insert_asset_at(catalog, project_id, id, &source);
        let asset_id = id.to_string();
        catalog
            .connection()
            .unwrap()
            .execute(
                "UPDATE assets SET duration_seconds=?2,source_out=?2 WHERE id=?1",
                params![asset_id, seconds],
            )
            .unwrap();
        catalog.validate_asset_video(&asset_id).unwrap();
        catalog.generate_proxy(&asset_id).unwrap();
        asset_id
    }

    pub(super) fn fixture() -> (tempfile::TempDir, Catalog, String, String) {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Synthetic run policy").unwrap();
        let asset = generated_asset(&catalog, &project.id, directory.path(), "clip", 2);
        (directory, catalog, project.id, asset)
    }

    fn observation_request(project: &str, asset: &str, budget: f64) -> RunPolicyRequest {
        RunPolicyRequest::observation(
            project,
            vec![asset.to_string()],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.to_string()],
            false,
            budget,
            false,
        )
    }

    fn authorized_observation(
        catalog: &Catalog,
        project: &str,
        asset: &str,
    ) -> (RunPolicyRequest, RunPolicy, RunPilotRecord) {
        let request = observation_request(project, asset, 1.0);
        let policy = catalog.describe_run_policy(&request).unwrap();
        assert!(
            policy.readiness.ready_for_authorization,
            "{:#?}",
            policy.readiness
        );
        catalog.authorize_run_policy(&request).unwrap();
        let provider = FakeProvider::observation();
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &provider, false)
            .unwrap();
        assert_eq!(pilot.status, "passed", "{pilot:?}");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        (request, policy, pilot)
    }

    #[test]
    fn denied_and_foreign_scope_fail_before_authorization() {
        let (_directory, catalog, project, asset) = fixture();
        let other = catalog.create_project("Other project").unwrap();
        let mut request = observation_request(&project, &asset, 1.0);
        request.asset_ids = vec!["missing".into()];
        assert!(catalog.describe_run_policy(&request).is_err());
        request.asset_ids = vec![asset.clone()];
        request.project_id = other.id;
        assert!(catalog.describe_run_policy(&request).is_err());
    }

    #[test]
    fn unsupported_audio_and_fallback_choices_fail_closed() {
        let (_directory, catalog, project, asset) = fixture();
        let mut request = observation_request(&project, &asset, 1.0);
        request.payload = AnalysisPayload::AudioAndTranscript;
        let policy = catalog.describe_run_policy(&request).unwrap();
        assert!(!policy.readiness.capabilities_ready);
        assert!(!policy.readiness.ready_for_authorization);
        assert!(catalog.authorize_run_policy(&request).is_err());
        assert!(policy.capability_checks.iter().any(|check| {
            check.requirement == "payload.audio_backend" && check.status == CapabilityStatus::Failed
        }));

        let mut invalid_fallback = observation_request(&project, &asset, 1.0);
        invalid_fallback.allow_fallback = true;
        invalid_fallback.allowed_models = vec![DEFAULT_MODEL.into(), "unknown/model".into()];
        let policy = catalog.describe_run_policy(&invalid_fallback).unwrap();
        assert!(!policy.readiness.capabilities_ready);
        assert!(policy.capability_checks.iter().any(|check| {
            check.requirement.contains("unknown/model") && check.status == CapabilityStatus::Failed
        }));
    }

    #[test]
    fn policy_budget_does_not_authorize_a_different_model_or_payload() {
        let (_directory, catalog, project, asset) = fixture();
        let request = observation_request(&project, &asset, 1.0);
        let authorized = catalog.authorize_run_policy(&request).unwrap();
        assert_eq!(
            authorized.policy.provider_cost_bound_contract,
            analysis_cost_bound_contract_id()
        );
        let mut changed_contract = authorized.policy.clone();
        changed_contract
            .provider_cost_bound_contract
            .push_str("-changed");
        assert_ne!(
            run_policy_identity(&changed_contract).unwrap(),
            authorized.policy.policy_id
        );

        let mut changed_model = request.clone();
        changed_model.primary_model = FALLBACK_MODEL.into();
        changed_model.allowed_models = vec![FALLBACK_MODEL.into()];
        let changed_policy = catalog.describe_run_policy(&changed_model).unwrap();
        assert_ne!(changed_policy.policy_id, authorized.policy.policy_id);
        assert!(
            catalog
                .load_run_authorization(&changed_policy.policy_id)
                .is_err()
        );

        let mut changed_payload = request.clone();
        changed_payload.payload = AnalysisPayload::Frames;
        let changed_policy = catalog.describe_run_policy(&changed_payload).unwrap();
        assert_ne!(changed_policy.policy_id, authorized.policy.policy_id);
        assert!(
            catalog
                .load_run_authorization(&changed_policy.policy_id)
                .is_err()
        );
    }

    #[test]
    fn source_change_invalidates_authorization_and_pilot() {
        let (_directory, catalog, project, asset) = fixture();
        let (request, policy, _pilot) = authorized_observation(&catalog, &project, &asset);
        assert!(
            catalog
                .authorized_policy_for_bulk(&policy.policy_id, AnalysisRunKind::Observation)
                .is_ok()
        );
        catalog
            .connection()
            .unwrap()
            .execute(
                "UPDATE assets SET duration_seconds=3.0,source_out=3.0 WHERE id=?1",
                [&asset],
            )
            .unwrap();
        let error = catalog
            .authorized_policy_for_bulk(&policy.policy_id, AnalysisRunKind::Observation)
            .unwrap_err();
        assert!(format!("{error:#}").contains("changed"));
        assert_eq!(
            catalog.describe_run_policy(&request).unwrap().policy_id,
            catalog.describe_run_policy(&request).unwrap().policy_id
        );
    }

    #[test]
    fn a_schema_valid_moment_response_with_invalid_source_times_fails_the_pilot() {
        let (_directory, catalog, project, asset) = fixture();
        let mut request = RunPolicyRequest::moment_discovery(
            project.clone(),
            vec![asset],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.into()],
            false,
            AnalysisPayload::Frames,
            1.0,
            8,
            8,
            1,
            EditProfile::default(),
            false,
            false,
        );
        catalog.authorize_run_policy(&request).unwrap();
        let provider = FakeProvider::moments(serde_json::json!({
            "moments": [{
                "event": "inverted synthetic moment",
                "role": "action",
                "reason": "schema-valid but semantically invalid source times",
                "score": 0.5,
                "coreStartMs": 2000,
                "coreEndMs": 1000,
                "payoffStartMs": 500,
                "payoffEndMs": 1500,
                "setup": null,
                "context": null,
                "longHold": false,
                "evidenceKeys": ["frame:400"],
                "uncertainty": []
            }],
            "uncertain": false,
            "reason": "synthetic"
        }));
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &provider, false)
            .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(pilot.status, "failed", "{pilot:?}");
        assert!(pilot.validation.structural_schema_valid);
        assert!(!pilot.validation.source_time_semantics_valid);
        assert!(
            catalog
                .authorized_policy_for_bulk(&pilot.policy_id, AnalysisRunKind::MomentDiscovery)
                .is_err()
        );

        // A budget-only or sample approval cannot broaden to another asset.
        request.asset_ids.push("not-in-scope".into());
        assert!(catalog.describe_run_policy(&request).is_err());
    }

    #[test]
    fn interrupted_pilot_is_recorded_and_not_retried_implicitly() {
        let (_directory, catalog, project, asset) = fixture();
        let request = observation_request(&project, &asset, 1.0);
        catalog.authorize_run_policy(&request).unwrap();
        let provider = FakeProvider {
            calls: AtomicUsize::new(0),
            body: String::new(),
            fail_transport: true,
            fail_prepare: false,
        };
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &provider, false)
            .unwrap();
        assert_eq!(pilot.status, "uncertain");
        assert!(!pilot.usage_complete);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        let error = catalog
            .run_run_policy_pilot_with_provider(&request, &provider, false)
            .unwrap_err();
        assert!(format!("{error:#}").contains("unknown usage"));
        // prepare failure is before any provider call and does not consume budget.
        let prepare_failure = FakeProvider {
            calls: AtomicUsize::new(0),
            body: String::new(),
            fail_transport: false,
            fail_prepare: true,
        };
        let mut second = observation_request(&project, &asset, 1.0);
        second.parameters = RunPolicyParameters::Observation { force: true };
        catalog.authorize_run_policy(&second).unwrap();
        assert!(
            catalog
                .run_run_policy_pilot_with_provider(&second, &prepare_failure, false)
                .is_err()
        );
        assert_eq!(prepare_failure.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn authorized_analysis_executes_only_policy_scope_and_counts_pilot_cost() {
        let (directory, catalog, project, first_asset) = fixture();
        let second_asset = generated_asset(&catalog, &project, directory.path(), "clip-two", 2);
        let request = observation_request(&project, &first_asset, 1.0);
        catalog.authorize_run_policy(&request).unwrap();
        let pilot_provider = FakeProvider::observation();
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &pilot_provider, false)
            .unwrap();
        assert_eq!(pilot.status, "passed");
        let state = catalog
            .authorized_policy_for_bulk(&pilot.policy_id, AnalysisRunKind::Observation)
            .unwrap();
        assert!(
            (state.remaining_budget_usd - (1.0 - pilot.cost_usd)).abs() < f64::EPSILON,
            "pilot spend must count against the same policy ceiling"
        );

        let bulk_provider = FakeProvider::observation();
        let report = catalog
            .analyze_authorized_with_provider(&pilot.policy_id, &bulk_provider, |_| {
                std::ops::ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(bulk_provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(report.analyzed, 1);
        assert!(
            catalog
                .list_assets(&project)
                .unwrap()
                .iter()
                .find(|asset| asset.id == second_asset)
                .is_some()
        );
        assert!(
            crate::analysis::vlm_store::load_analysis(
                &catalog.connection().unwrap(),
                &second_asset
            )
            .unwrap()
            .is_none(),
            "the exact policy scope must not drift to another project asset"
        );
    }

    #[test]
    fn moment_plan_reports_policy_readiness_and_exact_scope_identity() {
        let (_directory, catalog, project, asset) = fixture();
        let options = MomentRunOptions {
            model: DEFAULT_MODEL.into(),
            provider: "openrouter".into(),
            payload: MomentPayload::Frames,
            budget_usd: 1.0,
            coarse_frames: 8,
            refinement_frames: 8,
            max_candidates: 1,
            profile: EditProfile::default(),
            force: false,
            retry_uncertain: false,
            allowed_models: vec![DEFAULT_MODEL.into()],
            allow_fallback: false,
            policy_id: None,
        };
        let plan = catalog
            .plan_moments(&project, std::slice::from_ref(&asset), &options)
            .unwrap();
        assert_eq!(plan.run_policy.scope[0].asset_id, asset);
        assert!(
            plan.run_policy.scope[0]
                .representation_fingerprint
                .is_some()
        );
        assert!(plan.run_policy.scope[0].source_identity.len() >= 32);
        assert_eq!(plan.run_policy.budget_usd, 1.0);
        assert!(plan.run_policy.expected_cost_usd > 0.0);
        assert!(plan.run_policy.readiness.ready_for_authorization);
    }
    #[test]
    fn authorized_moment_execution_uses_exact_scope_and_rejects_broadening() {
        let (directory, catalog, project, first_asset) = fixture();
        let _second_asset = generated_asset(&catalog, &project, directory.path(), "clip-two", 2);
        let request = RunPolicyRequest::moment_discovery(
            project.clone(),
            vec![first_asset.clone()],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.into()],
            false,
            AnalysisPayload::Frames,
            1.0,
            8,
            8,
            1,
            EditProfile::default(),
            false,
            false,
        );
        let policy = catalog.authorize_run_policy(&request).unwrap().policy;
        let pilot_provider = FakeProvider::moments(serde_json::json!({
            "moments": [],
            "uncertain": true,
            "reason": "synthetic frames only"
        }));
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &pilot_provider, false)
            .unwrap();
        assert_eq!(pilot.status, "passed", "{pilot:?}");

        let mut broadened_options = policy.moment_options().unwrap();
        broadened_options.policy_id = Some(policy.policy_id.clone());
        let broadening_provider = FakeProvider::moments(serde_json::json!({
            "moments": [],
            "uncertain": true,
            "reason": "must not be called"
        }));
        let error = catalog
            .discover_moments_with_provider(
                &project,
                &[first_asset.clone(), "clip-two".into()],
                &broadened_options,
                &broadening_provider,
                |_| std::ops::ControlFlow::Continue(()),
            )
            .unwrap_err();
        assert!(format!("{error:#}").contains("exactly match"));
        assert_eq!(broadening_provider.calls.load(Ordering::SeqCst), 0);

        let run_provider = FakeProvider::moments(serde_json::json!({
            "moments": [],
            "uncertain": true,
            "reason": "synthetic exact scope"
        }));
        let report = catalog
            .discover_authorized_moments_with_provider(&policy.policy_id, &run_provider, |_| {
                std::ops::ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(report.total_assets, 1);
        assert!(run_provider.calls.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn unknown_model_and_schema_capability_are_rejected_before_authorization() {
        let (_directory, catalog, project, asset) = fixture();
        let mut request = observation_request(&project, &asset, 1.0);
        request.primary_model = "unknown/unsupported-model".into();
        request.allowed_models = vec!["unknown/unsupported-model".into()];
        let policy = catalog.describe_run_policy(&request).unwrap();
        assert!(!policy.readiness.capabilities_ready);
        assert!(!policy.readiness.ready_for_authorization);
        assert!(policy.capability_checks.iter().any(|check| {
            check.status == CapabilityStatus::Failed
                && (check.requirement.contains("unknown/unsupported-model")
                    || check.requirement.contains("schema"))
        }));
        assert!(catalog.authorize_run_policy(&request).is_err());
    }

    #[test]
    fn real_pilot_entrypoint_refuses_missing_credentials_before_provider_creation() {
        if crate::api_credential_present() {
            eprintln!("credential variable present; skipping missing-credential assertion");
            return;
        }
        let (_directory, catalog, project, asset) = fixture();
        let request = observation_request(&project, &asset, 1.0);
        catalog.authorize_run_policy(&request).unwrap();
        let error = catalog.run_run_policy_pilot(&request, false).unwrap_err();
        assert!(format!("{error:#}").contains("no pilot request was made"));
        assert!(
            catalog
                .load_run_pilot(&catalog.describe_run_policy(&request).unwrap().policy_id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn cancellation_before_first_bulk_request_stops_without_provider_calls() {
        let (_directory, catalog, project, asset) = fixture();
        let request = observation_request(&project, &asset, 1.0);
        catalog.authorize_run_policy(&request).unwrap();
        let pilot_provider = FakeProvider::observation();
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &pilot_provider, false)
            .unwrap();
        assert_eq!(pilot.status, "passed");
        let provider = FakeProvider::observation();
        let report = catalog
            .analyze_authorized_with_provider(&pilot.policy_id, &provider, |_| {
                std::ops::ControlFlow::Break(())
            })
            .unwrap();
        assert!(report.cancelled);
        assert_eq!(report.analyzed, 0);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    }
    struct CapturingProvider {
        calls: AtomicUsize,
        requests: Mutex<Vec<VlmRequest>>,
        body: String,
        fail_message: Option<String>,
        cost_usd: f64,
        usage_complete: bool,
    }

    impl CapturingProvider {
        fn observation() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                requests: Mutex::new(Vec::new()),
                body: serde_json::json!({
                    "title": "Synthetic captured observation",
                    "description": "A bounded synthetic observation.",
                    "labels": [],
                    "usable": true,
                    "unusableReason": null,
                    "bestMomentStartMs": null,
                    "bestMomentEndMs": null,
                    "thumbnailTimestampMs": null,
                    "cameraMovement": "static",
                    "ocrText": []
                })
                .to_string(),
                fail_message: None,
                cost_usd: 0.01,
                usage_complete: true,
            }
        }

        fn failing(message: &str) -> Self {
            let mut provider = Self::observation();
            provider.fail_message = Some(message.to_string());
            provider
        }

        fn with_body(body: &str) -> Self {
            let mut provider = Self::observation();
            provider.body = body.to_string();
            provider
        }

        fn with_incomplete_usage() -> Self {
            let mut provider = Self::observation();
            provider.usage_complete = false;
            provider
        }
    }

    impl VlmProvider for CapturingProvider {
        fn analysis_cost_bound(
            &self,
            _model: &str,
            _request: &VlmRequest,
        ) -> Result<AnalysisRequestCostBound> {
            Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
                0.02,
            ))
        }

        fn analyze(&self, model: &str, request: &VlmRequest) -> Result<VlmResponse> {
            let _call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            self.requests.lock().unwrap().push(request.clone());
            if let Some(message) = &self.fail_message {
                bail!("{message}");
            }
            Ok(VlmResponse {
                model: model.into(),
                body: self.body.clone(),
                prompt_tokens: 10,
                completion_tokens: 5,
                cost_usd: self.cost_usd,
                latency_ms: 1,
                receipt: VlmReceipt {
                    actual_model: Some(model.into()),
                    provider: Some("synthetic".into()),
                    generation_id: Some(format!("generation-capture-{}", uuid::Uuid::new_v4())),
                    usage_complete: self.usage_complete,
                },
            })
        }
    }

    struct PrepareBreaksProxy {
        calls: AtomicUsize,
        proxy_path: std::path::PathBuf,
    }

    impl VlmProvider for PrepareBreaksProxy {
        fn prepare(&self) -> Result<()> {
            // Simulate local representation loss after run admission but before
            // any paid upload. Repeated prepare calls are idempotent.
            let _ = fs::remove_file(&self.proxy_path);
            Ok(())
        }

        fn analysis_cost_bound(
            &self,
            _model: &str,
            _request: &VlmRequest,
        ) -> Result<AnalysisRequestCostBound> {
            Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
                0.02,
            ))
        }

        fn analyze(&self, _model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            bail!("local pre-upload test provider must never be called")
        }
    }

    fn save_private_transcript(catalog: &Catalog, asset_id: &str) {
        save_transcript(
            &catalog.connection().unwrap(),
            &StoredTranscript {
                asset_id: asset_id.to_string(),
                recipe_key: crate::analysis::STT_RECIPE_KEY.to_string(),
                source_fingerprint: catalog.proxy_fingerprint(asset_id).unwrap(),
                status: TranscriptStatus::Speech,
                outcome_reason: TranscriptOutcomeReason::Accepted,
                reject_reason: String::new(),
                primary_language: "en".into(),
                speech_seconds: 1.0,
                window_count: 1,
                provider_segment_count: 1,
                mean_token_probability: Some(0.9),
                repetition_ratio: None,
                segments: vec![DecodedSegment {
                    start_ms: 400,
                    end_ms: 1_600,
                    language: "en".into(),
                    text: "PRIVATE_TRANSCRIPT_CANARY".into(),
                    mean_token_probability: Some(0.9),
                }],
            },
        )
        .unwrap();
    }

    fn save_review_private_transcript(catalog: &Catalog, asset_id: &str) {
        save_transcript(
            &catalog.connection().unwrap(),
            &StoredTranscript {
                asset_id: asset_id.to_string(),
                recipe_key: crate::analysis::STT_RECIPE_KEY.to_string(),
                source_fingerprint: catalog.proxy_fingerprint(asset_id).unwrap(),
                status: TranscriptStatus::ReviewRequired,
                outcome_reason: TranscriptOutcomeReason::QualityEvidenceIncomplete,
                reject_reason: "review_missing_quality_evidence".into(),
                primary_language: "en".into(),
                speech_seconds: 1.0,
                window_count: 1,
                provider_segment_count: 1,
                mean_token_probability: None,
                repetition_ratio: None,
                segments: vec![DecodedSegment {
                    start_ms: 400,
                    end_ms: 1_600,
                    language: "en".into(),
                    text: "REVIEW_CANDIDATE_PRIVATE_CANARY".into(),
                    mean_token_probability: None,
                }],
            },
        )
        .unwrap();
    }

    #[test]
    fn review_required_candidates_are_not_transcript_scope_or_prompt_evidence() {
        let (_directory, catalog, project, asset) = fixture();
        save_review_private_transcript(&catalog, &asset);

        let mut request = observation_request(&project, &asset, 1.0);
        request.parameters = RunPolicyParameters::Observation { force: true };
        let described = catalog.describe_run_policy(&request).unwrap();
        assert!(
            described.scope[0].transcript_fingerprint.is_none(),
            "review candidates must not be admitted as transcript scope, even with force"
        );
        assert!(
            described
                .readiness
                .warnings
                .iter()
                .any(|warning| warning.contains("no accepted current transcript")),
            "{:?}",
            described.readiness.warnings
        );
        catalog.authorize_run_policy(&request).unwrap();

        let provider = CapturingProvider::observation();
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &provider, false)
            .unwrap();
        assert_eq!(pilot.status, "passed", "{pilot:?}");
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            !requests[0]
                .prompt
                .contains("REVIEW_CANDIDATE_PRIVATE_CANARY"),
            "review candidate text escaped into a provider prompt"
        );
    }

    #[test]
    fn frames_only_pilot_and_bulk_never_transmit_transcript_or_vocabulary() {
        let (_directory, catalog, project, asset) = fixture();
        catalog.add_label(&asset, "PRIVATE_VOCAB_CANARY").unwrap();
        save_private_transcript(&catalog, &asset);

        let mut request = observation_request(&project, &asset, 1.0);
        request.payload = AnalysisPayload::Frames;
        request.include_project_vocabulary = false;
        let described = catalog.describe_run_policy(&request).unwrap();
        assert!(described.scope[0].transcript_fingerprint.is_none());
        assert!(!described.context.project_vocabulary);
        catalog.authorize_run_policy(&request).unwrap();

        let pilot_provider = CapturingProvider::observation();
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &pilot_provider, false)
            .unwrap();
        assert_eq!(pilot.status, "passed", "{pilot:?}");
        assert!(!pilot.requested_model.is_empty());
        let pilot_requests = pilot_provider.requests.lock().unwrap();
        assert_eq!(pilot_requests.len(), 1);
        let pilot_prompt = &pilot_requests[0].prompt;
        assert!(!pilot_prompt.contains("PRIVATE_TRANSCRIPT_CANARY"));
        assert!(!pilot_prompt.contains("PRIVATE_VOCAB_CANARY"));
        assert!(!pilot_requests[0].frames.is_empty());
        drop(pilot_requests);

        let bulk_provider = CapturingProvider::observation();
        let report = catalog
            .analyze_authorized_with_provider(&pilot.policy_id, &bulk_provider, |_| {
                std::ops::ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(report.analyzed, 1);
        let bulk_requests = bulk_provider.requests.lock().unwrap();
        assert_eq!(bulk_requests.len(), 1);
        assert!(
            !bulk_requests[0]
                .prompt
                .contains("PRIVATE_TRANSCRIPT_CANARY")
        );
        assert!(!bulk_requests[0].prompt.contains("PRIVATE_VOCAB_CANARY"));
    }

    #[test]
    fn pilot_diagnostics_never_persist_provider_error_or_response_canaries() {
        let (_directory, catalog, project, asset) = fixture();
        let request = observation_request(&project, &asset, 1.0);
        catalog.authorize_run_policy(&request).unwrap();

        let provider = CapturingProvider::failing(
            "HTTP 500 sk-or-v1-PRIVATE-ERROR-CANARY /private/footage/canary.mp4 prompt-body-canary",
        );
        let record = catalog
            .run_run_policy_pilot_with_provider(&request, &provider, false)
            .unwrap();
        assert_eq!(record.status, "uncertain");
        let persisted = serde_json::to_string(&record).unwrap();
        for canary in [
            "sk-or-v1-PRIVATE-ERROR-CANARY",
            "/private/footage/canary.mp4",
            "prompt-body-canary",
        ] {
            assert!(!persisted.contains(canary), "leaked {canary}: {persisted}");
        }
        assert!(record.failure_code.contains("provider_error"));

        let run = catalog.analysis_runs(&project, 10).unwrap().remove(0);
        for canary in [
            "sk-or-v1-PRIVATE-ERROR-CANARY",
            "/private/footage/canary.mp4",
            "prompt-body-canary",
        ] {
            assert!(!run.error.contains(canary), "run error leaked {canary}");
        }

        // Reconcile the interrupted attempt before an explicitly different
        // force-policy can proceed; force cannot bypass unknown paid work.
        let interrupted = catalog
            .list_analysis_requests(&project, Some(&record.policy_id), 10)
            .unwrap()
            .remove(0);
        catalog
            .record_analysis_request_receipt(
                &interrupted.request_key,
                &ProviderReceiptEvidence {
                    provider: "synthetic".into(),
                    provider_generation_id: "diagnostic-late-generation".into(),
                    actual_model: DEFAULT_MODEL.into(),
                    reported_cost_usd: Some(0.01),
                    prompt_tokens: Some(10),
                    completion_tokens: Some(5),
                    usage_complete: true,
                    billed_audio_seconds: None,
                },
            )
            .unwrap();
        let mut different_request = request.clone();
        different_request.parameters = RunPolicyParameters::Observation { force: true };
        catalog.authorize_run_policy(&different_request).unwrap();
        let body_provider = CapturingProvider::with_body(
            r#"{"title":"PRIVATE_RESPONSE_BODY_CANARY","privatePath":"/private/body/path"}"#,
        );
        let body_record = catalog
            .run_run_policy_pilot_with_provider(&different_request, &body_provider, false)
            .unwrap();
        assert_eq!(body_record.status, "failed");
        let persisted = serde_json::to_string(&body_record).unwrap();
        assert!(!persisted.contains("PRIVATE_RESPONSE_BODY_CANARY"));
        assert!(!persisted.contains("/private/body/path"));
    }

    #[test]
    fn unknown_usage_retry_retains_prior_cost_and_stays_blocked() {
        let (_directory, catalog, project, asset) = fixture();
        let request = observation_request(&project, &asset, 1.0);
        catalog.authorize_run_policy(&request).unwrap();

        let first = CapturingProvider::with_incomplete_usage();
        let first_pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &first, false)
            .unwrap();
        assert_eq!(first_pilot.status, "uncertain");
        assert!(!first_pilot.usage_complete);
        assert_eq!(first_pilot.confirmed_cost_usd, 0.0);
        assert!(first_pilot.uncertain_usage_reserve_usd >= 0.01);
        assert!((first_pilot.cost_usd - first_pilot.uncertain_usage_reserve_usd).abs() < 1e-9);

        let second = CapturingProvider::observation();
        let error = catalog
            .run_run_policy_pilot_with_provider(&request, &second, true)
            .unwrap_err();
        assert!(format!("{error:#}").contains("reconcile"));
        assert_eq!(second.calls.load(Ordering::SeqCst), 0);
        let retained = catalog
            .load_run_pilot(&first_pilot.policy_id)
            .unwrap()
            .unwrap();
        assert_eq!(retained, first_pilot);
        assert!(
            catalog
                .authorized_policy_for_bulk(&retained.policy_id, AnalysisRunKind::Observation)
                .is_err()
        );
    }

    #[test]
    fn cancellation_before_pilot_transmission_makes_zero_provider_calls() {
        let (_directory, catalog, project, asset) = fixture();
        let request = observation_request(&project, &asset, 1.0);
        catalog.authorize_run_policy(&request).unwrap();
        let provider = CapturingProvider::observation();
        let error = catalog
            .run_run_policy_pilot_with_provider_and_cancel(&request, &provider, false, || true)
            .unwrap_err();
        assert!(format!("{error:#}").contains("cancelled"));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert!(
            catalog
                .load_run_pilot(&catalog.describe_run_policy(&request).unwrap().policy_id)
                .unwrap()
                .is_none(),
            "a cancelled pre-transmission pilot must not persist a paid record"
        );
    }

    #[test]
    fn persisted_source_change_invalidates_authorization_and_pilot_after_restart() {
        let (directory, catalog, project, asset) = fixture();
        let request = observation_request(&project, &asset, 1.0);
        catalog.authorize_run_policy(&request).unwrap();
        let pilot_provider = CapturingProvider::observation();
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &pilot_provider, false)
            .unwrap();
        assert_eq!(pilot.status, "passed");
        let catalog_path = directory.path().join("catalog.sqlite3");
        drop(catalog);

        let reopened = Catalog::open(&catalog_path).unwrap();
        reopened
            .connection()
            .unwrap()
            .execute(
                "UPDATE assets SET fingerprint='changed-after-pilot' WHERE id=?1",
                params![asset],
            )
            .unwrap();
        let error = reopened
            .authorized_policy_for_bulk(&pilot.policy_id, AnalysisRunKind::Observation)
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("changed"),
            "unexpected invalidation error: {error:#}"
        );
        assert!(
            reopened.load_run_pilot(&pilot.policy_id).unwrap().is_some(),
            "the old observation must remain auditable even though it is invalid"
        );
    }

    #[test]
    fn explicit_batch_invocation_without_policy_id_materializes_approval_and_pilot() {
        let (_directory, catalog, project, asset) = fixture();
        save_private_transcript(&catalog, &asset);
        catalog.add_label(&asset, "PRIVATE_VOCAB_CANARY").unwrap();
        let options = crate::AnalysisRunOptions {
            payload: Some(AnalysisPayload::Frames),
            force: true,
            budget_usd: Some(1.0),
            primary_model: Some(DEFAULT_MODEL.to_string()),
            allowed_models: vec![DEFAULT_MODEL.to_string()],
            allow_fallback: Some(false),
            include_project_vocabulary: false,
            ..crate::AnalysisRunOptions::default()
        };
        let provider = CapturingProvider::observation();
        let report = catalog
            .analyze_missing_with_options_and_provider(&project, &options, Some(&provider), |_| {
                std::ops::ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(report.analyzed, 1);
        assert!(
            provider.calls.load(Ordering::SeqCst) >= 2,
            "bulk must not run without the pilot call"
        );
        let policy_rows: i64 = catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM analysis_run_authorizations WHERE project_id=?1",
                params![project],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(policy_rows, 1, "explicit invocation must persist approval");
        let recorded_policy_runs: i64 = catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM analysis_runs WHERE project_id=?1 AND policy_id<>''",
                params![project],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            recorded_policy_runs >= 2,
            "pilot and bulk runs must both record policy provenance (found {recorded_policy_runs})"
        );
        assert!(provider.requests.lock().unwrap().iter().all(|request| {
            !request.prompt.contains("PRIVATE_TRANSCRIPT_CANARY")
                && !request.prompt.contains("PRIVATE_VOCAB_CANARY")
        }));
        let _ = asset;
    }

    #[test]
    fn frames_only_moment_pilot_and_bulk_never_transmit_transcript() {
        let (_directory, catalog, project, asset) = fixture();
        save_private_transcript(&catalog, &asset);
        let request = RunPolicyRequest::moment_discovery(
            project.clone(),
            vec![asset.clone()],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.into()],
            false,
            AnalysisPayload::Frames,
            1.0,
            8,
            8,
            1,
            EditProfile::default(),
            false,
            false,
        );
        let described = catalog.describe_run_policy(&request).unwrap();
        assert!(described.scope[0].transcript_fingerprint.is_none());
        catalog.authorize_run_policy(&request).unwrap();
        let moment_body = serde_json::json!({
            "moments": [],
            "uncertain": true,
            "reason": "synthetic frames only"
        })
        .to_string();

        let pilot_provider = CapturingProvider::with_body(&moment_body);
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &pilot_provider, false)
            .unwrap();
        assert_eq!(pilot.status, "passed", "{pilot:?}");
        {
            let requests = pilot_provider.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert!(!requests[0].prompt.contains("PRIVATE_TRANSCRIPT_CANARY"));
        }

        let bulk_provider = CapturingProvider::with_body(&moment_body);
        let report = catalog
            .discover_authorized_moments_with_provider(&pilot.policy_id, &bulk_provider, |_| {
                std::ops::ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(report.total_assets, 1);
        let requests = bulk_provider.requests.lock().unwrap();
        assert!(!requests.is_empty());
        assert!(
            requests
                .iter()
                .all(|request| !request.prompt.contains("PRIVATE_TRANSCRIPT_CANARY"))
        );
    }

    #[test]
    fn explicit_moment_invocation_without_policy_id_materializes_approval_and_pilot() {
        let (_directory, catalog, project, asset) = fixture();
        let options = MomentRunOptions {
            model: DEFAULT_MODEL.into(),
            provider: "openrouter".into(),
            payload: MomentPayload::Frames,
            budget_usd: 1.0,
            coarse_frames: 8,
            refinement_frames: 8,
            max_candidates: 1,
            profile: EditProfile::default(),
            force: false,
            retry_uncertain: false,
            allowed_models: vec![DEFAULT_MODEL.into()],
            allow_fallback: false,
            policy_id: None,
        };
        let provider = CapturingProvider::with_body(
            &serde_json::json!({
                "moments": [],
                "uncertain": true,
                "reason": "synthetic frames only"
            })
            .to_string(),
        );
        let report = catalog
            .discover_moments_for_assets_with_provider(
                &project,
                &[asset],
                &options,
                &provider,
                |_| std::ops::ControlFlow::Continue(()),
            )
            .unwrap();
        assert_eq!(report.total_assets, 1);
        assert!(
            provider.calls.load(Ordering::SeqCst) >= 2,
            "moment bulk must not run without the pilot call"
        );
        let policy_rows: i64 = catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM analysis_run_authorizations WHERE project_id=?1",
                params![project],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(policy_rows, 1);
        let recorded_policy_runs: i64 = catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM analysis_runs WHERE project_id=?1 AND policy_id<>''",
                params![project],
                |row| row.get(0),
            )
            .unwrap();
        assert!(recorded_policy_runs >= 2, "found {recorded_policy_runs}");
    }
    #[test]
    fn default_observation_and_moment_cancel_before_any_pilot_call() {
        let (_directory, catalog, project, asset) = fixture();
        let provider = CapturingProvider::observation();
        let options = crate::AnalysisRunOptions {
            budget_usd: Some(1.0),
            force: true,
            payload: Some(AnalysisPayload::Frames),
            ..Default::default()
        };
        let report = catalog
            .analyze_missing_with_options_and_provider(&project, &options, Some(&provider), |_| {
                std::ops::ControlFlow::Break(())
            })
            .unwrap();
        assert!(report.cancelled);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        let options = MomentRunOptions {
            model: DEFAULT_MODEL.into(),
            provider: "openrouter".into(),
            payload: MomentPayload::Frames,
            budget_usd: 1.0,
            coarse_frames: 8,
            refinement_frames: 8,
            max_candidates: 1,
            profile: EditProfile::default(),
            force: false,
            retry_uncertain: false,
            allowed_models: vec![DEFAULT_MODEL.into()],
            allow_fallback: false,
            policy_id: None,
        };
        let report = catalog
            .discover_moments_for_assets_with_provider(
                &project,
                &[asset],
                &options,
                &provider,
                |_| std::ops::ControlFlow::Break(()),
            )
            .unwrap();
        assert!(report.cancelled);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        let count: i64 = catalog
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM analysis_run_pilots", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn changed_transcript_timing_after_admission_blocks_transmission() {
        let (_directory, catalog, project, asset) = fixture();
        save_private_transcript(&catalog, &asset);
        let request = observation_request(&project, &asset, 1.0);
        catalog.authorize_run_policy(&request).unwrap();
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &CapturingProvider::observation(), false)
            .unwrap();
        let provider = CapturingProvider::observation();
        let mut changed = false;
        let report = catalog
            .analyze_authorized_with_provider(&pilot.policy_id, &provider, |_| {
                if !changed {
                    catalog
                        .connection()
                        .unwrap()
                        .execute(
                            "UPDATE transcript_segments SET end_ms=end_ms+10 WHERE asset_id=?1",
                            [&asset],
                        )
                        .unwrap();
                    changed = true;
                }
                std::ops::ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.analyzed, 0);
    }

    #[test]
    fn admitted_vocabulary_is_frozen_while_analysis_adds_labels() {
        let (directory, catalog, project, first) = fixture();
        let second = generated_asset(&catalog, &project, directory.path(), "clip-two", 2);
        catalog.add_label(&first, "APPROVED_VOCABULARY").unwrap();
        let mut request = observation_request(&project, &first, 1.0);
        request.asset_ids.push(second);
        request.include_project_vocabulary = true;
        catalog.authorize_run_policy(&request).unwrap();
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &CapturingProvider::observation(), false)
            .unwrap();
        let mut provider = CapturingProvider::observation();
        let mut body: Value = serde_json::from_str(&provider.body).unwrap();
        body["labels"] = json!(["new_generated_label"]);
        provider.body = body.to_string();
        let report = catalog
            .analyze_authorized_with_provider_and_workers(
                &pilot.policy_id,
                &provider,
                Some(1),
                |_| std::ops::ControlFlow::Continue(()),
            )
            .unwrap();
        assert_eq!(report.analyzed, 2, "{:?}", report.failures);
        assert!(
            provider
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request.prompt.contains("APPROVED_VOCABULARY")
                    && !request.prompt.contains("new_generated_label"))
        );
    }

    #[test]
    fn stored_schema_version_change_invalidates_authorization() {
        let (_directory, catalog, project, asset) = fixture();
        let request = observation_request(&project, &asset, 1.0);
        let authorization = catalog.authorize_run_policy(&request).unwrap();
        let policy_id = authorization.policy.policy_id;
        let policy_json: String = catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT policy_json FROM analysis_run_authorizations WHERE policy_id=?1",
                params![policy_id],
                |row| row.get(0),
            )
            .unwrap();
        let original = format!(
            "\"outputSchemaVersion\":\"{}\"",
            crate::analysis::schema::CLIP_OBSERVATION_SCHEMA_VERSION
        );
        let tampered = format!(
            "\"outputSchemaVersion\":\"{}.changed\"",
            crate::analysis::schema::CLIP_OBSERVATION_SCHEMA_VERSION
        );
        assert!(policy_json.contains(&original));
        catalog
            .connection()
            .unwrap()
            .execute(
                "UPDATE analysis_run_authorizations SET policy_json=?2 WHERE policy_id=?1",
                params![policy_id, policy_json.replace(&original, &tampered)],
            )
            .unwrap();
        let error = catalog.load_run_authorization(&policy_id).unwrap_err();
        assert!(
            format!("{error:#}").contains("contents no longer match"),
            "unexpected schema invalidation error: {error:#}"
        );
    }

    type ProviderScript = Box<dyn Fn(&str, usize) -> Result<VlmResponse> + Send + Sync>;

    struct ModelScriptProvider {
        calls: AtomicUsize,
        bound_usd: f64,
        script: ProviderScript,
    }

    impl ModelScriptProvider {
        fn new(
            bound_usd: f64,
            script: impl Fn(&str, usize) -> Result<VlmResponse> + Send + Sync + 'static,
        ) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                bound_usd,
                script: Box::new(script),
            }
        }
    }

    impl VlmProvider for ModelScriptProvider {
        fn analysis_cost_bound(
            &self,
            _model: &str,
            _request: &VlmRequest,
        ) -> Result<AnalysisRequestCostBound> {
            Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
                self.bound_usd,
            ))
        }

        fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            (self.script)(model, call)
        }
    }

    fn scripted_response(model: &str, call: usize, body: String, cost: f64) -> VlmResponse {
        VlmResponse {
            model: model.into(),
            body,
            prompt_tokens: 10,
            completion_tokens: 5,
            cost_usd: cost,
            latency_ms: 1,
            receipt: VlmReceipt {
                actual_model: Some(model.into()),
                provider: Some("synthetic-script".into()),
                generation_id: Some(format!("script-generation-{call}-{model}")),
                usage_complete: true,
            },
        }
    }

    fn valid_observation_body() -> String {
        serde_json::json!({
            "title": "Synthetic fallback pilot",
            "description": "A bounded synthetic observation.",
            "labels": [],
            "usable": true,
            "unusableReason": null,
            "bestMomentStartMs": null,
            "bestMomentEndMs": null,
            "thumbnailTimestampMs": null,
            "cameraMovement": "static",
            "ocrText": []
        })
        .to_string()
    }

    #[test]
    fn passed_pilot_is_idempotent_without_another_paid_call_or_run() {
        let (_directory, catalog, project, asset) = fixture();
        let request = observation_request(&project, &asset, 1.0);
        catalog.authorize_run_policy(&request).unwrap();
        let first_provider = CapturingProvider::observation();
        let first = catalog
            .run_run_policy_pilot_with_provider(&request, &first_provider, false)
            .unwrap();
        assert_eq!(first.status, "passed");
        assert_eq!(first_provider.calls.load(Ordering::SeqCst), 1);

        let second_provider = CapturingProvider::observation();
        let second = catalog
            .run_run_policy_pilot_with_provider(&request, &second_provider, false)
            .unwrap();
        assert_eq!(second, first);
        assert_eq!(second_provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            catalog.analysis_runs(&project, 10).unwrap().len(),
            1,
            "a passed pilot must not open another paid run"
        );
        assert_eq!(
            catalog
                .list_analysis_requests(&project, Some(&first.policy_id), 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn domain_invalid_response_falls_back_through_a_second_accounted_request() {
        let (_directory, catalog, project, asset) = fixture();
        let request = RunPolicyRequest::observation(
            project.clone(),
            vec![asset],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.into(), FALLBACK_MODEL.into()],
            true,
            1.0,
            false,
        );
        catalog.authorize_run_policy(&request).unwrap();
        let provider = ModelScriptProvider::new(0.02, |model, call| {
            let body = if model == DEFAULT_MODEL {
                r#"{"not":"a valid observation"}"#.to_string()
            } else {
                valid_observation_body()
            };
            Ok(scripted_response(model, call, body, 0.01))
        });
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &provider, false)
            .unwrap();
        assert_eq!(pilot.status, "passed", "{pilot:?}");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        assert_eq!(pilot.actual_model.as_deref(), Some(FALLBACK_MODEL));
        assert_eq!(pilot.confirmed_cost_usd, 0.02);
        assert_eq!(pilot.uncertain_usage_reserve_usd, 0.0);
        let requests = catalog
            .list_analysis_requests(&project, Some(&pilot.policy_id), 10)
            .unwrap();
        assert_eq!(
            requests.len(),
            2,
            "primary and domain fallback are separate"
        );
        assert!(requests.iter().all(|request| {
            catalog
                .analysis_provider_receipts(&request.request_key)
                .unwrap()
                .len()
                == 1
        }));
    }

    #[test]
    fn transport_fallback_incomplete_attempt_is_retained_until_late_receipt() {
        let (_directory, catalog, project, asset) = fixture();
        let request = RunPolicyRequest::observation(
            project.clone(),
            vec![asset],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.into(), FALLBACK_MODEL.into()],
            true,
            1.0,
            false,
        );
        catalog.authorize_run_policy(&request).unwrap();
        let provider = ModelScriptProvider::new(0.02, |model, call| {
            if model == DEFAULT_MODEL {
                Err(ProviderFailure::transport().into())
            } else {
                Ok(scripted_response(
                    model,
                    call,
                    valid_observation_body(),
                    0.01,
                ))
            }
        });
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &provider, false)
            .unwrap();
        assert_eq!(pilot.status, "uncertain", "{pilot:?}");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        assert_eq!(pilot.confirmed_cost_usd, 0.01);
        assert!(pilot.uncertain_usage_reserve_usd >= 0.02);
        let requests = catalog
            .list_analysis_requests(&project, Some(&pilot.policy_id), 10)
            .unwrap();
        assert_eq!(requests.len(), 2);
        let incomplete = requests
            .iter()
            .find(|request| request.lifecycle == crate::AnalysisRequestLifecycle::Uncertain)
            .unwrap();
        let complete = requests
            .iter()
            .find(|request| request.lifecycle == crate::AnalysisRequestLifecycle::Completed)
            .unwrap();

        catalog
            .record_analysis_request_receipt(
                &incomplete.request_key,
                &ProviderReceiptEvidence {
                    provider: "synthetic-script".into(),
                    provider_generation_id: "late-generation-primary".into(),
                    actual_model: DEFAULT_MODEL.into(),
                    reported_cost_usd: Some(0.01),
                    prompt_tokens: Some(10),
                    completion_tokens: Some(5),
                    usage_complete: true,
                    billed_audio_seconds: None,
                },
            )
            .unwrap();
        let aggregate = catalog
            .analysis_accounting_aggregate_for_operation(&pilot.policy_id, "pilot", None)
            .unwrap();
        assert_eq!(aggregate.uncertain_request_count, 0);
        assert!((aggregate.confirmed_cost_usd - 0.02).abs() < 1e-12);
        assert!((aggregate.uncertain_usage_reserve_usd).abs() < 1e-12);
        assert!(aggregate.usage_complete);
        assert_eq!(
            catalog
                .analysis_provider_receipts(&complete.request_key)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            catalog
                .analysis_provider_receipts(&incomplete.request_key)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn targeted_retry_authorization_retains_prior_unknown_cost_and_executes_once() {
        let (_directory, catalog, project, asset) = fixture();
        let request = observation_request(&project, &asset, 1.0);
        catalog.authorize_run_policy(&request).unwrap();
        let first = CapturingProvider::with_incomplete_usage();
        let first_pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &first, false)
            .unwrap();
        assert_eq!(first_pilot.status, "uncertain");
        let prior = catalog
            .list_analysis_requests(&project, Some(&first_pilot.policy_id), 10)
            .unwrap()
            .remove(0);
        assert_eq!(prior.lifecycle, crate::AnalysisRequestLifecycle::Uncertain);

        // Generic `retry_uncertain` alone cannot mint a new paid attempt.
        let unauthorized = CapturingProvider::observation();
        let error = catalog
            .run_run_policy_pilot_with_provider(&request, &unauthorized, true)
            .unwrap_err();
        let failure = ProviderFailure::from_anyhow(&error);
        assert_eq!(failure.category, ProviderFailureCategory::LocalAdmission);
        assert!(failure.safe_detail().contains("explicit retry"));
        assert_eq!(unauthorized.calls.load(Ordering::SeqCst), 0);

        catalog
            .authorize_run_policy_pilot_retry(&first_pilot.policy_id, &prior.request_key)
            .unwrap();
        let second = CapturingProvider::observation();
        let second_pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &second, true)
            .unwrap();
        assert_eq!(second.calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_pilot.status, "uncertain");
        assert!(!second_pilot.usage_complete);
        assert_eq!(second_pilot.confirmed_cost_usd, 0.01);
        assert!(second_pilot.uncertain_usage_reserve_usd >= 0.02);
        let requests = catalog
            .list_analysis_requests(&project, Some(&first_pilot.policy_id), 10)
            .unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().any(|request| {
            request.lifecycle == crate::AnalysisRequestLifecycle::Uncertain
                && request.attempt_epoch == 0
        }));
        assert!(requests.iter().any(|request| {
            request.lifecycle == crate::AnalysisRequestLifecycle::Completed
                && request.attempt_epoch == 1
        }));
    }

    #[test]
    fn retry_history_counts_actual_ledger_attempts_and_budget_denial_adds_none() {
        let (_directory, catalog, project, asset) = fixture();
        let request = RunPolicyRequest::observation(
            project.clone(),
            vec![asset],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.into()],
            false,
            0.05,
            false,
        );
        catalog.authorize_run_policy(&request).unwrap();

        let invalid = ModelScriptProvider::new(0.03, |model, call| {
            Ok(scripted_response(
                model,
                call,
                r#"{"not":"a valid observation"}"#.into(),
                0.01,
            ))
        });
        let first = catalog
            .run_run_policy_pilot_with_provider(&request, &invalid, false)
            .unwrap();
        assert_eq!(first.status, "failed");
        assert_eq!(first.attempts, 1);
        assert_eq!(first.prompt_tokens, 10);
        let first_request = catalog
            .list_analysis_requests(&project, Some(&first.policy_id), 10)
            .unwrap()
            .remove(0);
        catalog
            .authorize_run_policy_pilot_retry(&first.policy_id, &first_request.request_key)
            .unwrap();

        let interrupted = ModelScriptProvider::new(0.03, |_model, _call| {
            Err(ProviderFailure::transport().into())
        });
        let second = catalog
            .run_run_policy_pilot_with_provider(&request, &interrupted, true)
            .unwrap();
        assert_eq!(interrupted.calls.load(Ordering::SeqCst), 1);
        assert_eq!(second.status, "uncertain");
        assert_eq!(second.attempts, 2, "prior actual receipt must not be lost");
        assert_eq!(second.attempted_models.len(), 2);
        assert_eq!(second.prompt_tokens, 10, "all actual receipts stay visible");
        assert_eq!(second.completion_tokens, 5);
        let requests = catalog
            .list_analysis_requests(&project, Some(&second.policy_id), 10)
            .unwrap();
        assert_eq!(requests.len(), 2);
        let second_request = requests
            .iter()
            .find(|request| request.run_id == second.run_id)
            .unwrap();
        catalog
            .authorize_run_policy_pilot_retry(&second.policy_id, &second_request.request_key)
            .unwrap();

        let denied = CapturingProvider::observation();
        let error = catalog
            .run_run_policy_pilot_with_provider(&request, &denied, true)
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("budget_exhausted"),
            "{error:#}"
        );
        assert_eq!(denied.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            catalog
                .list_analysis_requests(&project, Some(&second.policy_id), 10)
                .unwrap()
                .len(),
            2,
            "budget denial must not create a paid request row"
        );
        let preserved = catalog.load_run_pilot(&second.policy_id).unwrap().unwrap();
        assert_eq!(preserved.attempts, 2);
        assert_eq!(preserved.attempted_models.len(), 2);
        assert_eq!(preserved.prompt_tokens, 10);
        assert_eq!(preserved.completion_tokens, 5);
    }

    #[test]
    fn prepare_failure_before_dispatch_records_zero_unknown_pilot() {
        let (_directory, catalog, project, asset) = fixture();
        let request = observation_request(&project, &asset, 1.0);
        let policy = catalog.describe_run_policy(&request).unwrap();
        catalog.authorize_run_policy(&request).unwrap();
        let provider = FakeProvider {
            calls: AtomicUsize::new(0),
            body: String::new(),
            fail_transport: false,
            fail_prepare: true,
        };
        let error = catalog
            .run_run_policy_pilot_with_provider(&request, &provider, false)
            .unwrap_err();
        assert!(format!("{error:#}").contains("pre_upload"));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        let requests = catalog
            .list_analysis_requests(&project, Some(&policy.policy_id), 10)
            .unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].lifecycle,
            crate::AnalysisRequestLifecycle::Released
        );
        assert!(requests[0].dispatch_at.is_none());
        let aggregate = catalog
            .analysis_accounting_aggregate_for_operation(&policy.policy_id, "pilot", None)
            .unwrap();
        assert_eq!(aggregate.confirmed_cost_usd, 0.0);
        assert_eq!(aggregate.uncertain_usage_reserve_usd, 0.0);
        assert!(aggregate.usage_complete);
        let pilot = catalog.load_run_pilot(&policy.policy_id).unwrap().unwrap();
        assert_eq!(pilot.status, "failed");
        assert!(pilot.usage_complete);
        assert_eq!(pilot.cost_usd, 0.0);
        assert_eq!(pilot.confirmed_cost_usd, 0.0);
        assert_eq!(pilot.uncertain_usage_reserve_usd, 0.0);
    }

    #[test]
    fn concurrent_bulk_workers_cannot_oversubscribe_the_whole_policy_budget() {
        let (directory, catalog, project, first_asset) = fixture();
        let second_asset =
            generated_asset(&catalog, &project, directory.path(), "clip-race-second", 2);
        // Two assets cost $0.02 at the reference estimate. The pilot's returned
        // $0.01 plus one $0.02 full request bound exactly exhausts this ceiling,
        // so a second concurrent admission must fail atomically.
        let request = RunPolicyRequest::observation(
            project.clone(),
            vec![first_asset, second_asset],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.into()],
            false,
            0.03,
            false,
        );
        catalog.authorize_run_policy(&request).unwrap();
        let pilot_provider = FakeProvider::observation();
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &pilot_provider, false)
            .unwrap();
        assert_eq!(pilot.status, "passed", "{pilot:?}");
        assert_eq!(pilot_provider.calls.load(Ordering::SeqCst), 1);

        let provider = FakeProvider::observation();
        let barrier = Barrier::new(2);
        let left = catalog.clone();
        let right = catalog.clone();
        let policy_id = pilot.policy_id.clone();
        let (left_report, right_report) = std::thread::scope(|scope| {
            let left = scope.spawn(|| {
                barrier.wait();
                left.analyze_authorized_with_provider_and_workers(
                    &policy_id,
                    &provider,
                    Some(2),
                    |_| std::ops::ControlFlow::Continue(()),
                )
            });
            let right = scope.spawn(|| {
                barrier.wait();
                right.analyze_authorized_with_provider_and_workers(
                    &policy_id,
                    &provider,
                    Some(2),
                    |_| std::ops::ControlFlow::Continue(()),
                )
            });
            (left.join().unwrap(), right.join().unwrap())
        });
        let reports = [left_report, right_report];
        assert!(
            reports.iter().any(|report| report.is_ok()),
            "at least one concurrent bulk attempt should be admitted: {reports:?}"
        );
        assert!(
            reports
                .iter()
                .flatten()
                .any(|report| report.budget_exhausted),
            "the remaining whole-policy budget cannot fit another full request bound: {reports:?}"
        );
        // Exactly one bulk call can fit after the pilot; local admission denial
        // must not consume a fallback or create an unpaid uncertainty row.
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "the canonical full-bound admission must prevent any oversubscription"
        );
        let aggregate = catalog
            .analysis_accounting_aggregate(&policy_id, None)
            .unwrap();
        assert!(aggregate.confirmed_cost_usd <= 0.03 + 1e-9);
        assert_eq!(aggregate.confirmed_cost_usd, 0.02);
        assert_eq!(aggregate.active_request_count, 0);
        assert_eq!(aggregate.uncertain_request_count, 0);
        assert_eq!(
            aggregate.committed_or_reserved_usd, aggregate.confirmed_cost_usd,
            "reservations must settle to confirmed receipts without a second balance"
        );
    }

    #[test]
    fn cancelled_partial_bulk_work_keeps_receipts_and_restart_reuses_results() {
        let (directory, catalog, project, first_asset) = fixture();
        let second_asset = generated_asset(
            &catalog,
            &project,
            directory.path(),
            "clip-restart-second",
            2,
        );
        let request = RunPolicyRequest::observation(
            project.clone(),
            vec![first_asset, second_asset],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.into()],
            false,
            1.0,
            false,
        );
        catalog.authorize_run_policy(&request).unwrap();
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &FakeProvider::observation(), false)
            .unwrap();
        assert_eq!(pilot.status, "passed", "{pilot:?}");

        let first_provider = FakeProvider::observation();
        let mut stopped = false;
        let first_report = catalog
            .analyze_authorized_with_provider_and_workers(
                &pilot.policy_id,
                &first_provider,
                Some(1),
                |progress| {
                    if progress.outcome.is_some() && !stopped {
                        stopped = true;
                        return std::ops::ControlFlow::Break(());
                    }
                    std::ops::ControlFlow::Continue(())
                },
            )
            .unwrap();
        assert!(first_report.cancelled);
        assert_eq!(first_report.analyzed, 1);
        assert_eq!(first_provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(first_report.cost_usd, 0.01);

        let observation_requests = catalog
            .list_analysis_requests(&project, Some(&pilot.policy_id), 1000)
            .unwrap()
            .into_iter()
            .filter(|request| request.operation == "observation")
            .collect::<Vec<_>>();
        assert_eq!(observation_requests.len(), 1);
        assert_eq!(
            observation_requests[0].lifecycle,
            crate::AnalysisRequestLifecycle::Completed
        );
        assert_eq!(observation_requests[0].confirmed_cost_usd, 0.01);
        let first_run_aggregate = catalog
            .analysis_accounting_aggregate_for_run(
                &pilot.policy_id,
                "observation",
                &observation_requests[0].run_id,
            )
            .unwrap();
        assert_eq!(first_run_aggregate.confirmed_cost_usd, 0.01);
        assert!(!first_run_aggregate.has_unresolved_usage());

        let second_provider = FakeProvider::observation();
        let second_report = catalog
            .analyze_authorized_with_provider_and_workers(
                &pilot.policy_id,
                &second_provider,
                Some(1),
                |_| std::ops::ControlFlow::Continue(()),
            )
            .unwrap();
        assert_eq!(second_provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_report.analyzed, 1, "{:?}", second_report.failures);
        assert!(!second_report.cancelled);
        let observation_aggregate = catalog
            .analysis_accounting_aggregate_for_operation(&pilot.policy_id, "observation", None)
            .unwrap();
        assert_eq!(observation_aggregate.request_count, 2);
        assert_eq!(observation_aggregate.confirmed_cost_usd, 0.02);
        assert_eq!(observation_aggregate.uncertain_request_count, 0);
    }

    #[test]
    fn unknown_bulk_usage_blocks_implicit_resend_and_is_not_treated_as_zero() {
        let (_directory, catalog, project, asset) = fixture();
        let request = observation_request(&project, &asset, 1.0);
        catalog.authorize_run_policy(&request).unwrap();
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &FakeProvider::observation(), false)
            .unwrap();
        assert_eq!(pilot.status, "passed", "{pilot:?}");

        let interrupted = FakeProvider {
            calls: AtomicUsize::new(0),
            body: String::new(),
            fail_transport: true,
            fail_prepare: false,
        };
        let report = catalog
            .analyze_authorized_with_provider_and_workers(
                &pilot.policy_id,
                &interrupted,
                Some(1),
                |_| std::ops::ControlFlow::Continue(()),
            )
            .unwrap();
        assert_eq!(interrupted.calls.load(Ordering::SeqCst), 1);
        assert!(!report.usage_complete);
        let observation_requests = catalog
            .list_analysis_requests(&project, Some(&pilot.policy_id), 1000)
            .unwrap()
            .into_iter()
            .filter(|request| request.operation == "observation")
            .collect::<Vec<_>>();
        assert_eq!(observation_requests.len(), 1);
        assert_eq!(
            observation_requests[0].lifecycle,
            crate::AnalysisRequestLifecycle::Uncertain
        );
        assert!(observation_requests[0].uncertain_usage_reserve_usd > 0.0);

        let replacement = FakeProvider::observation();
        let error = catalog
            .analyze_authorized_with_provider_and_workers(
                &pilot.policy_id,
                &replacement,
                Some(1),
                |_| std::ops::ControlFlow::Continue(()),
            )
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("unknown or interrupted"),
            "{error:#}"
        );
        assert_eq!(
            replacement.calls.load(Ordering::SeqCst),
            0,
            "a potentially completed paid request must never be blindly resent"
        );
    }

    #[test]
    fn authorized_moment_bulk_accounts_distinct_per_asset_and_span_requests() {
        let (directory, catalog, project, first_asset) = fixture();
        let second_asset = generated_asset(
            &catalog,
            &project,
            directory.path(),
            "clip-moment-ledger-second",
            2,
        );
        let request = RunPolicyRequest::moment_discovery(
            project.clone(),
            vec![first_asset.clone(), second_asset.clone()],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.into()],
            false,
            AnalysisPayload::Frames,
            1.0,
            8,
            8,
            1,
            EditProfile::default(),
            false,
            false,
        );
        let policy = catalog.authorize_run_policy(&request).unwrap().policy;
        let pilot_provider = FakeProvider::moments(serde_json::json!({
            "moments": [],
            "uncertain": true,
            "reason": "synthetic pilot"
        }));
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &pilot_provider, false)
            .unwrap();
        assert_eq!(pilot.status, "passed", "{pilot:?}");

        let bulk_provider = FakeProvider::moments(serde_json::json!({
            "moments": [],
            "uncertain": true,
            "reason": "synthetic distinct span accounting"
        }));
        let report = catalog
            .discover_authorized_moments_with_provider(&policy.policy_id, &bulk_provider, |_| {
                std::ops::ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(report.processed, 2, "{:?}", report.failures);
        assert!(bulk_provider.calls.load(Ordering::SeqCst) >= 2);

        let requests = catalog
            .list_analysis_requests(&project, Some(&policy.policy_id), 1000)
            .unwrap()
            .into_iter()
            .filter(|request| {
                request.operation == "moment_discovery" && request.run_id == report.run_id
            })
            .collect::<Vec<_>>();
        assert!(
            requests.len() >= 2,
            "expected per-span requests: {requests:?}"
        );
        let mut keys = requests
            .iter()
            .map(|request| request.attempt_base_key.clone())
            .collect::<Vec<_>>();
        keys.sort();
        keys.dedup();
        assert_eq!(
            keys.len(),
            requests.len(),
            "each asset/span attempt must have its own identity"
        );
        let authorized_sources = policy
            .scope
            .iter()
            .map(|scope| scope.source_identity.clone())
            .collect::<Vec<_>>();
        let mut observed_sources = requests
            .iter()
            .map(|request| request.source_fingerprint.clone())
            .collect::<Vec<_>>();
        observed_sources.sort();
        observed_sources.dedup();
        assert_eq!(
            observed_sources.len(),
            2,
            "both authorized asset source identities must be represented"
        );
        assert!(
            observed_sources
                .iter()
                .all(|source| authorized_sources.contains(source))
        );
        assert!(requests.iter().all(|request| {
            request.lifecycle == crate::AnalysisRequestLifecycle::Completed
                && request.dispatch_at.is_some()
                && request.confirmed_cost_usd == 0.01
        }));
        let aggregate = catalog
            .analysis_accounting_aggregate_for_run(
                &policy.policy_id,
                "moment_discovery",
                &report.run_id,
            )
            .unwrap();
        assert_eq!(aggregate.request_count, requests.len());
        assert_eq!(aggregate.confirmed_cost_usd, requests.len() as f64 * 0.01);
        assert_eq!(report.cost_usd, aggregate.confirmed_cost_usd);
        assert!(!aggregate.has_unresolved_usage());
    }

    #[test]
    fn bulk_domain_fallback_records_two_complete_ledger_receipts() {
        let (_directory, catalog, project, asset) = fixture();
        let request = RunPolicyRequest::observation(
            project.clone(),
            vec![asset],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.into(), FALLBACK_MODEL.into()],
            true,
            1.0,
            false,
        );
        catalog.authorize_run_policy(&request).unwrap();
        let provider = ModelScriptProvider::new(0.02, |model, call| {
            let body = if model == DEFAULT_MODEL {
                r#"{"not":"a valid bulk observation"}"#.to_string()
            } else {
                valid_observation_body()
            };
            Ok(scripted_response(model, call, body, 0.01))
        });
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &provider, false)
            .unwrap();
        assert_eq!(pilot.status, "passed", "{pilot:?}");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);

        let report = catalog
            .analyze_authorized_with_provider(&pilot.policy_id, &provider, |_| {
                std::ops::ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
        assert_eq!(report.fallback_served, 1);
        assert_eq!(report.cost_usd, 0.02);
        let requests = catalog
            .list_analysis_requests(&project, Some(&pilot.policy_id), 1000)
            .unwrap()
            .into_iter()
            .filter(|request| request.operation == "observation")
            .collect::<Vec<_>>();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| {
            request.lifecycle == crate::AnalysisRequestLifecycle::Completed
                && request.confirmed_cost_usd == 0.01
                && catalog
                    .analysis_provider_receipts(&request.request_key)
                    .unwrap()
                    .len()
                    == 1
        }));
        let aggregate = catalog
            .analysis_accounting_aggregate_for_run(
                &pilot.policy_id,
                "observation",
                &requests[0].run_id,
            )
            .unwrap();
        assert_eq!(aggregate.confirmed_cost_usd, 0.02);
        assert!(!aggregate.has_unresolved_usage());
    }

    #[test]
    fn bulk_transport_fallback_retains_primary_uncertainty_and_fallback_receipt() {
        let (_directory, catalog, project, asset) = fixture();
        let request = RunPolicyRequest::observation(
            project.clone(),
            vec![asset],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.into(), FALLBACK_MODEL.into()],
            true,
            1.0,
            false,
        );
        catalog.authorize_run_policy(&request).unwrap();
        let provider = ModelScriptProvider::new(0.02, |model, call| {
            if call == 1 || model == FALLBACK_MODEL {
                Ok(scripted_response(
                    model,
                    call,
                    valid_observation_body(),
                    0.01,
                ))
            } else {
                Err(ProviderFailure::transport().into())
            }
        });
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &provider, false)
            .unwrap();
        assert_eq!(pilot.status, "passed", "{pilot:?}");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

        let report = catalog
            .analyze_authorized_with_provider(&pilot.policy_id, &provider, |_| {
                std::ops::ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
        assert!(!report.usage_complete);
        assert_eq!(report.cost_usd, 0.01);
        let requests = catalog
            .list_analysis_requests(&project, Some(&pilot.policy_id), 1000)
            .unwrap()
            .into_iter()
            .filter(|request| request.operation == "observation")
            .collect::<Vec<_>>();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.lifecycle == crate::AnalysisRequestLifecycle::Uncertain)
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.lifecycle == crate::AnalysisRequestLifecycle::Completed)
                .count(),
            1
        );
        assert!(requests.iter().any(|request| {
            request.lifecycle == crate::AnalysisRequestLifecycle::Uncertain
                && request.uncertain_usage_reserve_usd >= 0.02
        }));
        let aggregate = catalog
            .analysis_accounting_aggregate_for_run(
                &pilot.policy_id,
                "observation",
                &requests[0].run_id,
            )
            .unwrap();
        assert_eq!(aggregate.confirmed_cost_usd, 0.01);
        assert_eq!(aggregate.uncertain_request_count, 1);
        assert!(aggregate.has_unresolved_usage());
    }

    #[test]
    fn local_proxy_loss_after_admission_makes_zero_paid_calls_and_zero_reservations() {
        let (_directory, catalog, project, asset) = fixture();
        let request = observation_request(&project, &asset, 1.0);
        catalog.authorize_run_policy(&request).unwrap();
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &FakeProvider::observation(), false)
            .unwrap();
        assert_eq!(pilot.status, "passed", "{pilot:?}");
        let proxy_path = catalog
            .get_asset(&asset)
            .unwrap()
            .proxy_path
            .expect("synthetic asset has a proxy");
        let provider = PrepareBreaksProxy {
            calls: AtomicUsize::new(0),
            proxy_path: proxy_path.into(),
        };
        let report = catalog
            .analyze_authorized_with_provider(&pilot.policy_id, &provider, |_| {
                std::ops::ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(report.analyzed, 0);
        assert_eq!(report.cost_usd, 0.0);
        assert_eq!(report.failures.len(), 1);
        assert!(
            catalog
                .list_analysis_requests(&project, Some(&pilot.policy_id), 1000)
                .unwrap()
                .iter()
                .all(|request| request.operation != "observation"),
            "a local pre-upload failure must not create a paid request row"
        );
        let analysis_run: (String, String) = catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT id,accounting_contract FROM analysis_runs
                 WHERE policy_id=?1 AND stage_reached='analysis'
                 ORDER BY started_at DESC,id DESC LIMIT 1",
                params![pilot.policy_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(analysis_run.1, crate::ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
        let aggregate = catalog
            .analysis_accounting_aggregate_for_run(&pilot.policy_id, "observation", &analysis_run.0)
            .unwrap();
        assert_eq!(aggregate.request_count, 0);
        assert_eq!(aggregate.confirmed_cost_usd, 0.0);
        assert_eq!(aggregate.active_reserved_cost_usd, 0.0);
        assert!(!aggregate.has_unresolved_usage());
    }

    #[test]
    fn force_policy_cannot_bypass_unresolved_prior_pilot_request() {
        let (_directory, catalog, project, asset) = fixture();
        let request = observation_request(&project, &asset, 1.0);
        catalog.authorize_run_policy(&request).unwrap();
        let first = CapturingProvider::with_incomplete_usage();
        let first_pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &first, false)
            .unwrap();
        assert_eq!(first_pilot.status, "uncertain");

        let mut forced = request.clone();
        forced.parameters = RunPolicyParameters::Observation { force: true };
        catalog.authorize_run_policy(&forced).unwrap();
        let replacement = CapturingProvider::observation();
        let error = catalog
            .run_run_policy_pilot_with_provider(&forced, &replacement, true)
            .unwrap_err();
        assert!(format!("{error:#}").contains("replacement policy"));
        assert_eq!(replacement.calls.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn bulk_domain_fallback_local_denial_remains_typed_and_retains_primary_charge() {
        let (_directory, catalog, project, asset) = fixture();
        let request = RunPolicyRequest::observation(
            project.clone(),
            vec![asset],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.into(), FALLBACK_MODEL.into()],
            true,
            0.05,
            false,
        );
        catalog.authorize_run_policy(&request).unwrap();
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &FakeProvider::observation(), false)
            .unwrap();
        assert_eq!(pilot.status, "passed");
        let provider = ModelScriptProvider::new(0.04, |model, call| {
            Ok(scripted_response(model, call, "{}".into(), 0.01))
        });
        let report = catalog
            .analyze_authorized_with_provider(&pilot.policy_id, &provider, |_| {
                std::ops::ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert!(report.budget_exhausted);
        assert_eq!(report.cost_usd, 0.01);
        assert!(report.usage_complete);
        let aggregate = catalog
            .analysis_accounting_aggregate(&pilot.policy_id, None)
            .unwrap();
        assert!((aggregate.confirmed_cost_usd - 0.02).abs() < 1e-12);
        assert_eq!(aggregate.uncertain_request_count, 0);
    }

    #[test]
    fn moment_fallback_local_denial_or_cancel_cannot_erase_paid_primary_uncertainty() {
        for cancel in [false, true] {
            let (_directory, catalog, project, asset) = fixture();
            let request = RunPolicyRequest::moment_discovery(
                project.clone(),
                vec![asset.clone()],
                DEFAULT_MODEL,
                vec![DEFAULT_MODEL.into(), FALLBACK_MODEL.into()],
                true,
                AnalysisPayload::Frames,
                0.07,
                8,
                8,
                1,
                EditProfile::default(),
                false,
                false,
            );
            catalog.authorize_run_policy(&request).unwrap();
            let body = json!({"moments": [], "uncertain": true, "reason": "synthetic"}).to_string();
            let pilot = catalog
                .run_run_policy_pilot_with_provider(
                    &request,
                    &CapturingProvider::with_body(&body),
                    false,
                )
                .unwrap();
            assert_eq!(pilot.status, "passed", "{pilot:?}");
            let provider =
                ModelScriptProvider::new(0.05, |_, _| Err(ProviderFailure::transport().into()));
            let report = catalog
                .discover_authorized_moments_with_provider(&pilot.policy_id, &provider, |_| {
                    if cancel && provider.calls.load(Ordering::SeqCst) > 0 {
                        std::ops::ControlFlow::Break(())
                    } else {
                        std::ops::ControlFlow::Continue(())
                    }
                })
                .unwrap();
            assert_eq!(
                provider.calls.load(Ordering::SeqCst),
                1,
                "local fallback denial must not transmit"
            );
            assert_eq!(report.cancelled, cancel);
            assert!(!report.usage_complete);
            let aggregate = catalog
                .analysis_accounting_aggregate(&pilot.policy_id, None)
                .unwrap();
            assert_eq!(aggregate.uncertain_request_count, 1);
            assert!((aggregate.uncertain_usage_reserve_usd - 0.05).abs() < 1e-12);
            let states: Vec<String> = catalog.connection().unwrap().prepare(
                "SELECT json_extract(body_json,'$.body.state') FROM moment_steps WHERE asset_id=?1"
            ).unwrap().query_map([&asset], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
            assert!(
                states.iter().any(|state| state == "uncertain"),
                "{states:?}"
            );
            assert!(
                !states.iter().any(|state| state == "local_failure"),
                "{states:?}"
            );
        }
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use crate::ProviderReceiptEvidence;
    use crate::analysis::provider::{
        AnalysisRequestCostBound, ProviderFailure, ProviderFailureCategory, VlmProvider,
        VlmReceipt, VlmRequest, VlmResponse,
    };
    use crate::{AnalysisRequestLifecycle, AnalysisRequestReconciliation};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    struct InterruptingProvider {
        calls: AtomicUsize,
    }

    impl VlmProvider for InterruptingProvider {
        fn analysis_cost_bound(
            &self,
            _model: &str,
            _request: &VlmRequest,
        ) -> Result<AnalysisRequestCostBound> {
            Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
                0.03,
            ))
        }

        fn analyze(&self, _model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            panic!("synthetic process interruption after durable dispatch marker")
        }
    }

    #[test]
    fn recovered_crashed_running_pilot_keeps_unknown_validation_and_known_usage() {
        let (_directory, catalog, project, asset) = super::tests::fixture();
        let request = RunPolicyRequest::observation(
            project.clone(),
            vec![asset.clone()],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.into()],
            false,
            1.0,
            false,
        );
        let authorization = catalog.authorize_run_policy(&request).unwrap();
        let provider = InterruptingProvider {
            calls: AtomicUsize::new(0),
        };
        let interrupted = catch_unwind(AssertUnwindSafe(|| {
            catalog.run_run_policy_pilot_with_provider(&request, &provider, false)
        }));
        assert!(interrupted.is_err());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        let policy_id = authorization.policy.policy_id;
        let running = catalog.load_run_pilot(&policy_id).unwrap().unwrap();
        assert_eq!(running.status, "running");
        let request_record = catalog
            .list_analysis_requests(&project, Some(&policy_id), 10)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(
            request_record.lifecycle,
            AnalysisRequestLifecycle::Dispatched
        );

        let report = catalog
            .reconcile_analysis_request_for_project(
                &project,
                &request_record.request_key,
                &AnalysisRequestReconciliation::LateReceipt {
                    receipt: ProviderReceiptEvidence {
                        provider: "synthetic-script".into(),
                        provider_generation_id: "recovered-generation".into(),
                        actual_model: DEFAULT_MODEL.into(),
                        reported_cost_usd: Some(0.03),
                        prompt_tokens: Some(10),
                        completion_tokens: Some(5),
                        usage_complete: true,
                        billed_audio_seconds: None,
                    },
                },
            )
            .unwrap();
        assert_eq!(
            report.request.lifecycle,
            AnalysisRequestLifecycle::Completed
        );
        let pilot = report.pilot.expect("existing pilot projection refreshed");
        assert_eq!(pilot.status, "failed");
        assert!(pilot.usage_complete);
        assert!(!pilot.validation.capability_observed);
        assert!(!pilot.validation.structural_schema_valid);
        assert!(!pilot.validation.source_time_semantics_valid);
        assert!(pilot.observed_capabilities.is_empty());
        assert!((pilot.confirmed_cost_usd - 0.03).abs() < 1e-12);
        assert_eq!(pilot.failure_code, "reconciled_receipt_requires_review");
        let run = catalog
            .analysis_runs(&project, 10)
            .unwrap()
            .into_iter()
            .find(|run| run.id == pilot.run_id)
            .unwrap();
        assert_eq!(run.status, "failed");
        assert!((run.cost_usd - 0.03).abs() < 1e-12);
        // Recovery read the prior pilot, but a later runtime run now owns its
        // projection. A stale write cannot replace that newer run/metadata.
        let mut newer = pilot.clone();
        newer.run_id = catalog
            .begin_accounted_analysis_run(&project, &policy_id, "pilot")
            .unwrap();
        newer.status = "running".into();
        newer.finished_at.clear();
        newer.provider = None;
        catalog.save_pilot(&newer).unwrap();
        assert!(!catalog.save_pilot_if_unchanged(&pilot, &pilot).unwrap());
        assert_eq!(catalog.load_run_pilot(&policy_id).unwrap(), Some(newer));
    }
    struct CaptureFailProvider {
        calls: AtomicUsize,
        payload: Mutex<Option<VlmRequest>>,
    }

    impl VlmProvider for CaptureFailProvider {
        fn analysis_cost_bound(
            &self,
            _model: &str,
            _request: &VlmRequest,
        ) -> Result<AnalysisRequestCostBound> {
            Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
                0.03,
            ))
        }

        fn analyze(&self, _model: &str, request: &VlmRequest) -> Result<VlmResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.payload.lock().unwrap() = Some(request.clone());
            Err(ProviderFailure::transport().into())
        }
    }

    struct PolicyRetryProvider {
        calls: AtomicUsize,
        body: String,
    }

    impl VlmProvider for PolicyRetryProvider {
        fn analysis_cost_bound(
            &self,
            _model: &str,
            _request: &VlmRequest,
        ) -> Result<AnalysisRequestCostBound> {
            Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
                0.03,
            ))
        }

        fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(VlmResponse {
                model: model.into(),
                body: self.body.clone(),
                prompt_tokens: 11,
                completion_tokens: 7,
                cost_usd: 0.03,
                latency_ms: 2,
                receipt: VlmReceipt {
                    actual_model: Some(model.into()),
                    provider: Some("synthetic-policy-retry".into()),
                    generation_id: Some("policy-retry-generation".into()),
                    usage_complete: true,
                },
            })
        }
    }

    #[test]
    fn policy_owned_targeted_retry_rederives_source_policy_and_payload_then_dispatches_once() {
        let (_directory, catalog, project, asset) = super::tests::fixture();
        let request = RunPolicyRequest::observation(
            project.clone(),
            vec![asset.clone()],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.into()],
            false,
            1.0,
            false,
        );
        let _authorization = catalog.authorize_run_policy(&request).unwrap();
        let failed = CaptureFailProvider {
            calls: AtomicUsize::new(0),
            payload: Mutex::new(None),
        };
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &failed, false)
            .unwrap();
        assert_eq!(pilot.status, "uncertain");
        let prior = catalog
            .list_analysis_requests(&project, Some(&pilot.policy_id), 10)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let original_payload = failed.payload.lock().unwrap().clone().unwrap();
        catalog
            .authorize_analysis_request_retry_for_project(&project, &prior.request_key)
            .unwrap();

        let retry = PolicyRetryProvider {
            calls: AtomicUsize::new(0),
            body: "REVIEW_ONLY_POLICY_RETRY_BODY".into(),
        };
        let report = catalog
            .dispatch_targeted_analysis_retry_with_provider(
                &project,
                &prior.request_key,
                1,
                &original_payload,
                &retry,
            )
            .unwrap();
        assert_eq!(retry.calls.load(Ordering::SeqCst), 1);
        assert_eq!(report.request.attempt_epoch, 1);
        assert_eq!(
            report.request.lifecycle,
            AnalysisRequestLifecycle::Completed
        );
        assert_eq!(
            report.provider_response.as_ref().unwrap().body,
            "REVIEW_ONLY_POLICY_RETRY_BODY"
        );
        assert!((report.budget.confirmed_cost_usd - 0.03).abs() < 1e-12);
        assert!((report.budget.uncertain_usage_reserve_usd - 0.03).abs() < 1e-12);
    }
    #[test]
    fn policy_source_or_project_mismatch_denies_targeted_retry_before_any_call() {
        let (_directory, catalog, project, asset) = super::tests::fixture();
        let request = RunPolicyRequest::observation(
            project.clone(),
            vec![asset.clone()],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.into()],
            false,
            1.0,
            false,
        );
        catalog.authorize_run_policy(&request).unwrap();
        let failed = CaptureFailProvider {
            calls: AtomicUsize::new(0),
            payload: Mutex::new(None),
        };
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &failed, false)
            .unwrap();
        let prior = catalog
            .list_analysis_requests(&project, Some(&pilot.policy_id), 10)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let original_payload = failed.payload.lock().unwrap().clone().unwrap();
        catalog
            .authorize_analysis_request_retry_for_project(&project, &prior.request_key)
            .unwrap();

        let source = catalog.get_asset(&asset).unwrap();
        let proxy = source.proxy_path.expect("synthetic fixture has a proxy");
        std::fs::write(proxy, b"changed proxy bytes after authorization").unwrap();
        let provider = PolicyRetryProvider {
            calls: AtomicUsize::new(0),
            body: "must not be sent".into(),
        };
        let source_error = catalog
            .dispatch_targeted_analysis_retry_with_provider(
                &project,
                &prior.request_key,
                1,
                &original_payload,
                &provider,
            )
            .unwrap_err();
        assert!(
            format!("{source_error:#}")
                .to_lowercase()
                .contains("policy")
                || format!("{source_error:#}")
                    .to_lowercase()
                    .contains("source")
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);

        let wrong_project_error = catalog
            .dispatch_targeted_analysis_retry_with_provider(
                "not-the-owning-project",
                &prior.request_key,
                1,
                &original_payload,
                &provider,
            )
            .unwrap_err();
        assert!(format!("{wrong_project_error:#}").contains("does not belong"));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);

        // A revoked canonical authorization is denied by the same shared
        // owner check even when the caller reaches the provider-injected direct
        // entry point. No provider method has been called.
        catalog
            .connection()
            .unwrap()
            .execute(
                "DELETE FROM analysis_run_authorizations WHERE policy_id=?1",
                [&prior.policy_id],
            )
            .unwrap();
        let revoked_provider = PolicyRetryProvider {
            calls: AtomicUsize::new(0),
            body: "must not be sent".into(),
        };
        let revoked_error = catalog
            .dispatch_targeted_analysis_retry_with_provider(
                &project,
                &prior.request_key,
                1,
                &original_payload,
                &revoked_provider,
            )
            .unwrap_err();
        assert!(
            format!("{revoked_error:#}").contains("not authorized"),
            "{revoked_error:#}"
        );
        assert_eq!(revoked_provider.calls.load(Ordering::SeqCst), 0);
    }
    struct PrepareMutatesProxyProvider {
        calls: AtomicUsize,
        proxy_path: std::path::PathBuf,
    }

    impl VlmProvider for PrepareMutatesProxyProvider {
        fn prepare(&self) -> Result<()> {
            std::fs::write(&self.proxy_path, b"changed after policy-owning prepare")
                .map_err(|_| anyhow::anyhow!("synthetic prepare mutation failed"))?;
            Ok(())
        }

        fn analysis_cost_bound(
            &self,
            _model: &str,
            _request: &VlmRequest,
        ) -> Result<AnalysisRequestCostBound> {
            Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
                0.03,
            ))
        }

        fn analyze(&self, _model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            panic!("mutated policy must be denied before transport")
        }
    }

    struct PrepareRevokesAuthorizationProvider {
        calls: AtomicUsize,
        catalog: Catalog,
        policy_id: String,
    }

    impl VlmProvider for PrepareRevokesAuthorizationProvider {
        fn prepare(&self) -> Result<()> {
            self.catalog
                .connection()
                .map_err(|_| anyhow::anyhow!("synthetic authorization mutation failed"))?
                .execute(
                    "DELETE FROM analysis_run_authorizations WHERE policy_id=?1",
                    [&self.policy_id],
                )
                .map_err(|_| anyhow::anyhow!("synthetic authorization mutation failed"))?;
            Ok(())
        }

        fn analysis_cost_bound(
            &self,
            _model: &str,
            _request: &VlmRequest,
        ) -> Result<AnalysisRequestCostBound> {
            Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
                0.03,
            ))
        }

        fn analyze(&self, _model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            panic!("revoked authorization must be denied before transport")
        }
    }

    struct PrepareCancelsProvider {
        calls: AtomicUsize,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    }

    impl VlmProvider for PrepareCancelsProvider {
        fn prepare(&self) -> Result<()> {
            self.cancelled.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn analysis_cost_bound(
            &self,
            _model: &str,
            _request: &VlmRequest,
        ) -> Result<AnalysisRequestCostBound> {
            Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
                0.03,
            ))
        }

        fn analyze(&self, _model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            panic!("pre-marker cancellation must not reach transport")
        }
    }

    #[test]
    fn policy_retry_prepare_mutation_is_rechecked_before_marker_and_released() {
        let (_directory, catalog, project, asset) = super::tests::fixture();
        let request = RunPolicyRequest::observation(
            project.clone(),
            vec![asset.clone()],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.into()],
            false,
            1.0,
            false,
        );
        catalog.authorize_run_policy(&request).unwrap();
        let failed = CaptureFailProvider {
            calls: AtomicUsize::new(0),
            payload: Mutex::new(None),
        };
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &failed, false)
            .unwrap();
        assert_eq!(pilot.status, "uncertain");
        let prior = catalog
            .list_analysis_requests(&project, Some(&pilot.policy_id), 10)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let original_payload = failed.payload.lock().unwrap().clone().unwrap();
        catalog
            .authorize_analysis_request_retry_for_project(&project, &prior.request_key)
            .unwrap();
        let proxy_path = catalog
            .get_asset(&asset)
            .unwrap()
            .proxy_path
            .expect("synthetic fixture has a proxy");
        let provider = PrepareMutatesProxyProvider {
            calls: AtomicUsize::new(0),
            proxy_path: proxy_path.into(),
        };
        let report = catalog
            .dispatch_targeted_analysis_retry_with_provider(
                &project,
                &prior.request_key,
                1,
                &original_payload,
                &provider,
            )
            .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(report.request.lifecycle, AnalysisRequestLifecycle::Released);
        assert!(report.request.dispatch_at.is_none());
        let run = catalog
            .analysis_runs(&project, 100)
            .unwrap()
            .into_iter()
            .find(|run| run.id == report.request.run_id)
            .unwrap();
        assert_eq!(run.status, "failed");
        assert_eq!(run.cost_usd, 0.0);
        assert_eq!(report.attempt_history.len(), 2);
        assert_eq!(
            report.attempt_history[0].lifecycle,
            AnalysisRequestLifecycle::Uncertain
        );
        assert_eq!(
            report.attempt_history[1].lifecycle,
            AnalysisRequestLifecycle::Released
        );
        assert!((report.budget.uncertain_usage_reserve_usd - 0.03).abs() < 1e-12);
        assert_eq!(report.budget.active_reserved_cost_usd, 0.0);
        assert!(!report.budget.dispatch_stopped);
        assert!(
            report
                .provider_error_code
                .as_deref()
                .unwrap_or_default()
                .contains("pre_upload")
                || report
                    .provider_error_code
                    .as_deref()
                    .unwrap_or_default()
                    .contains("admission")
        );
    }

    #[test]
    fn policy_retry_prepare_authorization_revocation_is_rechecked_before_marker() {
        let (_directory, catalog, project, asset) = super::tests::fixture();
        let request = RunPolicyRequest::observation(
            project.clone(),
            vec![asset],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.into()],
            false,
            1.0,
            false,
        );
        catalog.authorize_run_policy(&request).unwrap();
        let failed = CaptureFailProvider {
            calls: AtomicUsize::new(0),
            payload: Mutex::new(None),
        };
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &failed, false)
            .unwrap();
        let prior = catalog
            .list_analysis_requests(&project, Some(&pilot.policy_id), 10)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let original_payload = failed.payload.lock().unwrap().clone().unwrap();
        catalog
            .authorize_analysis_request_retry_for_project(&project, &prior.request_key)
            .unwrap();
        let provider = PrepareRevokesAuthorizationProvider {
            calls: AtomicUsize::new(0),
            catalog: catalog.clone(),
            policy_id: pilot.policy_id.clone(),
        };
        let report = catalog
            .dispatch_targeted_analysis_retry_with_provider(
                &project,
                &prior.request_key,
                1,
                &original_payload,
                &provider,
            )
            .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(report.request.lifecycle, AnalysisRequestLifecycle::Released);
        assert!(report.request.dispatch_at.is_none());
        assert_eq!(
            report.attempt_history.last().unwrap().lifecycle,
            AnalysisRequestLifecycle::Released
        );
        assert!((report.budget.uncertain_usage_reserve_usd - 0.03).abs() < 1e-12);
        assert_eq!(report.budget.active_reserved_cost_usd, 0.0);
        assert!(!report.budget.dispatch_stopped);
    }

    #[test]
    fn targeted_retry_cancellation_before_prepare_and_after_prepare_make_zero_calls() {
        // Cancellation before local preparation creates no paid run or request.
        let (_directory, catalog, project, asset) = super::tests::fixture();
        let request = RunPolicyRequest::observation(
            project.clone(),
            vec![asset.clone()],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.into()],
            false,
            1.0,
            false,
        );
        catalog.authorize_run_policy(&request).unwrap();
        let failed = CaptureFailProvider {
            calls: AtomicUsize::new(0),
            payload: Mutex::new(None),
        };
        let pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &failed, false)
            .unwrap();
        let prior = catalog
            .list_analysis_requests(&project, Some(&pilot.policy_id), 10)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let original_payload = failed.payload.lock().unwrap().clone().unwrap();
        catalog
            .authorize_analysis_request_retry_for_project(&project, &prior.request_key)
            .unwrap();
        let runs_before = catalog.analysis_runs(&project, 100).unwrap().len();
        let never_called = PolicyRetryProvider {
            calls: AtomicUsize::new(0),
            body: "unused".into(),
        };
        let error = catalog
            .dispatch_targeted_analysis_retry_with_provider_and_cancel(
                &project,
                &prior.request_key,
                1,
                &original_payload,
                &never_called,
                || true,
            )
            .unwrap_err();
        assert_eq!(
            ProviderFailure::from_anyhow(&error).category,
            ProviderFailureCategory::Cancelled
        );
        assert_eq!(never_called.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            catalog.analysis_runs(&project, 100).unwrap().len(),
            runs_before
        );
        assert_eq!(
            catalog
                .analysis_attempt_history(&prior.request_key)
                .unwrap()
                .len(),
            1
        );

        // Cancellation raised by provider.prepare happens after the reservation
        // but before the durable marker; the reservation is releasable and the
        // prior unknown remains intact.
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let provider = PrepareCancelsProvider {
            calls: AtomicUsize::new(0),
            cancelled: Arc::clone(&cancelled),
        };
        let report = catalog
            .dispatch_targeted_analysis_retry_with_provider_and_cancel(
                &project,
                &prior.request_key,
                1,
                &original_payload,
                &provider,
                move || cancelled.load(Ordering::SeqCst),
            )
            .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(report.request.lifecycle, AnalysisRequestLifecycle::Released);
        assert!(report.request.dispatch_at.is_none());
        let run = catalog
            .analysis_runs(&project, 100)
            .unwrap()
            .into_iter()
            .find(|run| run.id == report.request.run_id)
            .unwrap();
        assert_eq!(run.status, "cancelled");
        assert_eq!(run.cost_usd, 0.0);
        assert_eq!(report.attempt_history.len(), 2);
        assert_eq!(
            report.attempt_history.last().unwrap().lifecycle,
            AnalysisRequestLifecycle::Released
        );
        assert!((report.budget.uncertain_usage_reserve_usd - 0.03).abs() < 1e-12);
        assert_eq!(report.budget.active_reserved_cost_usd, 0.0);
        assert!(!report.budget.dispatch_stopped);
        assert!(
            report
                .provider_error_code
                .as_deref()
                .unwrap_or_default()
                .contains("cancelled")
        );
    }

    #[test]
    fn late_old_pilot_receipt_refreshes_whole_pilot_but_keeps_current_run_metadata() {
        let (_directory, catalog, project, asset) = super::tests::fixture();
        let request = RunPolicyRequest::observation(
            project.clone(),
            vec![asset.clone()],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.into()],
            false,
            1.0,
            false,
        );
        catalog.authorize_run_policy(&request).unwrap();
        let failed = CaptureFailProvider {
            calls: AtomicUsize::new(0),
            payload: Mutex::new(None),
        };
        let first_pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &failed, false)
            .unwrap();
        assert_eq!(first_pilot.status, "uncertain");
        let old_request = catalog
            .list_analysis_requests(&project, Some(&first_pilot.policy_id), 10)
            .unwrap()
            .into_iter()
            .find(|record| record.run_id == first_pilot.run_id)
            .unwrap();
        catalog
            .authorize_run_policy_pilot_retry(&first_pilot.policy_id, &old_request.request_key)
            .unwrap();
        let second_provider = PolicyRetryProvider {
            calls: AtomicUsize::new(0),
            body: r#"{"title":"Synthetic late receipt pilot","description":"valid synthetic observation","labels":[],"usable":true,"unusableReason":null,"bestMomentStartMs":null,"bestMomentEndMs":null,"thumbnailTimestampMs":null,"cameraMovement":"static","ocrText":[]}"#.into(),
        };
        let second_pilot = catalog
            .run_run_policy_pilot_with_provider(&request, &second_provider, true)
            .unwrap();
        assert_eq!(second_provider.calls.load(Ordering::SeqCst), 1);
        assert!(!second_pilot.usage_complete);
        let current_metadata = (
            second_pilot.provider.clone(),
            second_pilot.actual_model.clone(),
            second_pilot.generation_id.clone(),
        );
        assert!(current_metadata.0.is_some());
        assert!(current_metadata.1.is_some());
        assert!(current_metadata.2.is_some());
        let first_receipt = ProviderReceiptEvidence {
            provider: "synthetic-old-late".into(),
            provider_generation_id: "old-late-generation".into(),
            actual_model: DEFAULT_MODEL.into(),
            reported_cost_usd: Some(0.03),
            prompt_tokens: Some(19),
            completion_tokens: Some(7),
            usage_complete: true,
            billed_audio_seconds: None,
        };
        let evidence = AnalysisRequestReconciliation::LateReceipt {
            receipt: first_receipt,
        };
        let first_report = catalog
            .reconcile_analysis_request_for_project(&project, &old_request.request_key, &evidence)
            .unwrap();
        let first_projection = first_report.pilot.expect("pilot projection refreshed");
        assert!((first_projection.confirmed_cost_usd - 0.06).abs() < 1e-12);
        assert_eq!(first_projection.uncertain_usage_reserve_usd, 0.0);
        assert_eq!(first_projection.status, "passed");
        assert_eq!(
            (
                first_projection.provider.clone(),
                first_projection.actual_model.clone(),
                first_projection.generation_id.clone(),
            ),
            current_metadata,
            "a late old-run receipt must not replace current-run provenance"
        );
        let current_run = first_projection.run_id.clone();
        let (run_cost, run_status): (f64, String) = catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT cost_usd,status FROM analysis_runs WHERE id=?1",
                [&current_run],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(
            (run_cost - 0.03).abs() < 1e-12,
            "historical run cost is its own confirmed projection, not whole-pilot cost"
        );
        assert_eq!(
            run_status, "completed",
            "a persisted pilot validation plus fully known usage closes the owned run as completed"
        );
        let finished_at = first_projection.finished_at.clone();
        let history_after_first = catalog
            .analysis_request_events(&old_request.request_key)
            .unwrap();

        let second_report = catalog
            .reconcile_analysis_request_for_project(&project, &old_request.request_key, &evidence)
            .unwrap();
        let second_projection = second_report.pilot.expect("pilot projection retained");
        assert_eq!(first_projection, second_projection);
        assert_eq!(second_projection.finished_at, finished_at);
        assert_eq!(
            catalog
                .analysis_request_events(&old_request.request_key)
                .unwrap(),
            history_after_first,
            "identical reconciliation must not append metadata churn"
        );
        assert_eq!(
            catalog
                .analysis_provider_receipts(&old_request.request_key)
                .unwrap()
                .len(),
            1
        );
    }
}
