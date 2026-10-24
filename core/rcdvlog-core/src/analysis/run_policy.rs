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
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;

use crate::analysis::moments::{
    EditProfile, MOMENT_PROMPT_VERSION, MOMENT_RECIPE_VERSION, MOMENT_SCHEMA_VERSION,
    MomentPayload, MomentSpan, moment_output_schema, validate_reply,
};
use crate::analysis::openrouter::{
    OpenRouterProvider, api_credential_present, frame_data_uri, resolve_api_key,
};
use crate::analysis::provider::{
    DEFAULT_MODEL, FALLBACK_MODEL, ProviderChain, ProviderFailure, VlmProvider, VlmRequest,
    analysis_cost_bound_contract_id, model_spec,
};
use crate::analysis::schema::{self, output_schema as observation_output_schema};
use crate::analysis::vlm::{PromptContext, build_prompt};
use crate::analysis::{VLM_PROMPT_VERSION, framepack, store::TranscriptStatus};
use crate::{Asset, Catalog, REFERENCE_COST_PER_CLIP_USD};

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
}

impl AnalysisRunKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observation => "observation",
            Self::MomentDiscovery => "moment_discovery",
        }
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
#[serde(tag = "kind", rename_all = "snake_case")]
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
}

impl RunPolicyParameters {
    pub fn kind(&self) -> AnalysisRunKind {
        match self {
            Self::Observation { .. } => AnalysisRunKind::Observation,
            Self::MomentDiscovery { .. } => AnalysisRunKind::MomentDiscovery,
        }
    }

    pub fn force(&self) -> bool {
        match self {
            Self::Observation { force } | Self::MomentDiscovery { force, .. } => *force,
        }
    }
}

/// The caller supplies exactly what it wants to authorize. Describe re-reads
/// canonical ownership and source identity; the caller cannot supply a source
/// fingerprint or representation identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
    pub cost_usd: f64,
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizedPolicyState {
    pub authorization: RunAuthorization,
    pub pilot: RunPilotRecord,
    pub remaining_budget_usd: f64,
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

fn source_scope(
    catalog: &Catalog,
    asset: &Asset,
    payload: AnalysisPayload,
) -> (RunAssetScope, Vec<RunPolicyBlocker>, Option<String>) {
    let source_duration_ms = (asset.duration_seconds.max(0.0) * 1_000.0).round() as u32;
    // A frames-only or audio-only policy must not even bind transcript content
    // that it is not authorized to transmit. The identity is present only when
    // the payload actually selects accepted transcript context.
    let transcript_fingerprint = if payload.uses_transcript() {
        catalog
            .asset_transcript(&asset.id)
            .ok()
            .flatten()
            .and_then(|transcript| {
                (transcript.status == TranscriptStatus::Speech && !transcript.segments.is_empty())
                    .then(|| digest(&transcript).ok())
                    .flatten()
            })
    } else {
        None
    };
    let proxy_identity = catalog.analysis_proxy_source(asset);
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
                if payload.uses_audio() {
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
        let (current, blockers, _) = source_scope(self, &asset, policy.payload);
        ensure!(
            blockers.is_empty() && &current == approved,
            "authorized source or transcript changed before transmission"
        );

        Ok(())
    }

    /// Freeze the approved vocabulary for this run. Analysis may itself add
    /// labels, but requests must keep using the original approved snapshot.
    pub(crate) fn run_policy_vocabulary(&self, policy: &RunPolicy) -> Result<Vec<String>> {
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

        let allowed = canonical_models(request);
        let fallbacks =
            permitted_fallbacks(&request.primary_model, &allowed, request.allow_fallback);
        let checks = capability_checks(request, &allowed, &fallbacks);
        let mut blockers = Vec::new();
        let mut warnings = Vec::new();
        let mut scope = Vec::new();
        for asset in &assets {
            let (entry, mut source_blockers, _) = source_scope(self, asset, request.payload);
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
                message: "selected primary model does not accept the configured JSON schema".into(),
                asset_id: None,
            });
        }
        if !allowed.iter().any(|model| model == &request.primary_model) {
            blockers.push(RunPolicyBlocker {
                code: "primary_model_not_allowed".into(),
                message: "selected primary model is not present in the permitted allowlist".into(),
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
                    message: format!("permitted fallback {fallback} lacks required capabilities"),
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

        let expected_cost_usd = assets.len() as f64 * REFERENCE_COST_PER_CLIP_USD;
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
        let credentials_present = api_credential_present();
        let kind = request.parameters.kind();
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
        };
        let context = policy_context(
            self,
            &request.project_id,
            request.include_project_vocabulary,
        )?;
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
                && credentials_present,
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
            allow_fallback: request.allow_fallback,
            permitted_fallbacks: fallbacks,
            payload: request.payload,
            context,
            scope,
            expected_cost_usd,
            cost_basis: "reference measurement; pilot spend is counted against the same ceiling"
                .into(),
            budget_usd: request.budget_usd,
            parameters: request.parameters.clone(),
            prompt_version,
            output_schema_version: schema_version,
            recipe_version,
            tool_version: RUN_POLICY_TOOL_VERSION.into(),
            provider_cost_bound_contract: analysis_cost_bound_contract_id(),
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
            "INSERT INTO analysis_run_authorizations(policy_id,project_id,policy_json,authorized_at)
             VALUES (?1,?2,?3,?4)
             ON CONFLICT(policy_id) DO NOTHING",
            params![policy.policy_id, policy.project_id, policy_json, authorized_at],
        )?;
        self.load_run_authorization(&policy.policy_id)
    }

    pub fn load_run_authorization(&self, policy_id: &str) -> Result<RunAuthorization> {
        let row: Option<(String, String, String)> = self
            .connection()?
            .query_row(
                "SELECT project_id,policy_json,authorized_at FROM analysis_run_authorizations WHERE policy_id=?1",
                [policy_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (project_id, policy_json, authorized_at) =
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
        Ok(RunAuthorization {
            schema_version: RUN_AUTHORIZATION_SCHEMA_VERSION.into(),
            authorized_at,
            policy,
        })
    }

    pub fn load_run_pilot(&self, policy_id: &str) -> Result<Option<RunPilotRecord>> {
        let record: Option<String> = self
            .connection()?
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
        let policy = self.describe_run_policy(request)?;
        ensure!(
            policy.readiness.ready_for_authorization,
            "run policy is not ready for a pilot: {}",
            policy
                .readiness
                .blockers
                .iter()
                .map(|blocker| blocker.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
        self.load_run_authorization(&policy.policy_id)?;

        let previous = self.load_run_pilot(&policy.policy_id)?;
        let mut prior_cost_usd = 0.0;
        let mut prior_attempts = 0u32;
        let mut prior_usage_complete = true;
        let mut prior_models = Vec::new();
        if let Some(existing) = previous {
            if existing.status == "passed" {
                return Ok(existing);
            }
            ensure!(
                existing.usage_complete && existing.status != "running",
                "pilot has interrupted or unknown usage; reconcile its receipt before any retry"
            );
            ensure!(
                retry_uncertain,
                "a prior pilot failed; inspect its receipt before an explicit retry"
            );
            prior_cost_usd = existing.cost_usd;
            prior_attempts = existing.attempts;
            prior_usage_complete = existing.usage_complete;
            prior_models = existing.attempted_models;
        }
        ensure!(
            prior_cost_usd.is_finite() && prior_cost_usd >= 0.0,
            "prior pilot cost is invalid"
        );
        ensure!(
            prior_cost_usd < policy.budget_usd,
            "prior pilot attempts consumed the authorized run budget"
        );

        if is_cancelled() {
            bail!("pilot cancelled before provider transmission");
        }
        provider.prepare()?;
        if is_cancelled() {
            bail!("pilot cancelled before provider transmission");
        }

        let first_scope = &policy.scope[0];
        let asset = self.get_asset(&first_scope.asset_id)?;
        // Local frame extraction is heavy work shared with every other local
        // media operation. Hold one cancellable lease only around extraction.
        let heavy = self.acquire_local_heavy_cancellable(&mut is_cancelled)?;
        let manifest = self.analysis_frame_pack(&asset)?;
        if is_cancelled() {
            drop(heavy);
            bail!("pilot cancelled before provider transmission");
        }
        let proxy = asset
            .proxy_path
            .as_deref()
            .filter(|path| std::path::Path::new(path).is_file())
            .ok_or_else(|| anyhow::anyhow!("source proxy is no longer available"))?;
        let parent = self
            .database_path()
            .parent()
            .ok_or_else(|| anyhow::anyhow!("catalog has no parent directory"))?;
        let frame_directory = parent
            .join("cache")
            .join("framepacks")
            .join(&asset.project_id)
            .join(&asset.id)
            .join(&manifest.source_fingerprint);
        let frame_paths =
            framepack::encode_frames(std::path::Path::new(proxy), &manifest, &frame_directory)?;
        drop(heavy);
        let frames = frame_paths
            .iter()
            .map(|path| {
                std::fs::read(path)
                    .map(|bytes| frame_data_uri(&bytes))
                    .with_context(|| "read pilot frame from managed cache")
            })
            .collect::<Result<Vec<_>>>()?;

        // Payload promises are enforced at the point the request is built. A
        // frames-only approval never reads a transcript; vocabulary is read only
        // when the policy explicitly approved its exact identity.
        let transcript = if policy.payload.uses_transcript() {
            self.prompt_transcript(&asset.id)?
        } else {
            None
        };
        let vocabulary = if policy.context.project_vocabulary {
            let current = self.prompt_vocabulary(&policy.project_id)?;
            ensure!(
                digest(&current)?
                    == policy
                        .context
                        .vocabulary_fingerprint
                        .clone()
                        .unwrap_or_default(),
                "project vocabulary changed after policy approval"
            );
            current
        } else {
            Vec::new()
        };

        let (request_body, schema, schema_validator) = match policy.kind {
            AnalysisRunKind::Observation => {
                let context = PromptContext {
                    frame_timestamps_ms: manifest.timestamps_ms.clone(),
                    clip_duration_ms: first_scope.source_duration_ms,
                    transcript: transcript.clone(),
                    vocabulary,
                    trip_context: None,
                };
                (
                    build_prompt(&context),
                    observation_output_schema(
                        &manifest.timestamps_ms,
                        first_scope.source_duration_ms,
                    ),
                    ObservationSchemaKind::Observation,
                )
            }
            AnalysisRunKind::MomentDiscovery => {
                let keys = manifest
                    .timestamps_ms
                    .iter()
                    .map(|at| format!("frame:{at}"))
                    .collect::<Vec<_>>();
                let schema = moment_output_schema(1, first_scope.source_duration_ms, &keys);
                let evidence = manifest
                    .timestamps_ms
                    .iter()
                    .map(|at| json!({"kind":"frame","key":format!("frame:{at}"),"timeMs":at}))
                    .collect::<Vec<_>>();
                let prompt = format!(
                    "Editorial source-time capability pilot. Return ONLY JSON matching the configured schema. \
                     Every source time is an integer number of milliseconds on the same source clock. \
                     The enclosed inspectedSpan.startMs and inspectedSpan.endMs use those units. For every moment: \
                     coreStartMs, coreEndMs, payoffStartMs and payoffEndMs are integer source milliseconds; \
                     coreStartMs < coreEndMs and payoffStartMs < payoffEndMs; the inspected span must contain the core; \
                     the core must contain the payoff; optional setup must precede the payoff and end at or before payoffStartMs; \
                     optional context must contain the core; evidenceKeys may only contain supplied frame:<sourceMs> keys. \
                     A schema-valid object with inverted, out-of-source or cross-field-inconsistent times is still invalid. \
                     Sampled frames are sparse evidence, not continuous video. Use zero moments when nothing is supported. \
                     EVIDENCE={}",
                    json!({
                        "inspectedSpan": {"startMs": 0, "endMs": first_scope.source_duration_ms},
                        "sourceDurationMs": first_scope.source_duration_ms,
                        "frameTimesMs": manifest.timestamps_ms,
                        "evidence": evidence,
                        "acceptedTimedSpeech": policy.payload.uses_transcript().then(|| {
                            transcript.as_ref().map(|value| value.text.clone())
                        }).flatten(),
                        "editProfile": match &policy.parameters {
                            RunPolicyParameters::MomentDiscovery { profile, .. } => Some(*profile),
                            RunPolicyParameters::Observation { .. } => None,
                        }
                    })
                );
                (prompt, schema, ObservationSchemaKind::Moment)
            }
        };
        let vlm_request = VlmRequest {
            prompt: request_body,
            frames,
            schema,
        };

        // Cancellation is authoritative right up to the transmission boundary.
        if is_cancelled() {
            bail!("pilot cancelled before provider transmission");
        }
        // Source and approved context may have changed while waiting for the
        // heavy lease or preparing frames. Never transmit under stale approval.
        ensure!(
            self.describe_run_policy(request)?.policy_id == policy.policy_id,
            "run policy or source identity changed before pilot transmission"
        );
        let started_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let run_id = self.begin_analysis_run(&policy.project_id)?;
        self.connection()?.execute(
            "UPDATE analysis_runs SET stage_reached='pilot',policy_id=?2 WHERE id=?1",
            params![run_id, policy.policy_id],
        )?;
        let running = RunPilotRecord {
            schema_version: RUN_PILOT_SCHEMA_VERSION.into(),
            policy_id: policy.policy_id.clone(),
            project_id: policy.project_id.clone(),
            status: "running".into(),
            started_at: started_at.clone(),
            finished_at: String::new(),
            run_id: run_id.clone(),
            asset_id: first_scope.asset_id.clone(),
            requested_model: policy.primary_model.clone(),
            actual_model: None,
            provider: None,
            generation_id: None,
            attempted_models: prior_models.clone(),
            attempts: prior_attempts,
            prompt_tokens: 0,
            completion_tokens: 0,
            cost_usd: prior_cost_usd,
            usage_complete: false,
            failure_code: String::new(),
            prior_cost_usd,
            prior_attempts,
            validation: PilotValidation {
                structural_schema_valid: false,
                source_time_semantics_valid: false,
                capability_observed: false,
                detail: "pilot_running".into(),
            },
            observed_capabilities: Vec::new(),
            warning: PILOT_WARNING.into(),
        };
        self.save_pilot(&running)?;
        let chain = ProviderChain::with_policy(
            &policy.primary_model,
            &policy.allowed_models,
            policy.allow_fallback,
        )?;
        // A cancellation observed after local preparation or run-record
        // persistence but before this instruction must still make zero paid
        // calls. Keep the audit row but fail closed.
        if is_cancelled() {
            let _ = self.finish_pilot_uncertain(
                &running,
                "pilot_cancelled_before_transmission".to_string(),
                "cancelled",
                prior_models,
                prior_cost_usd,
            );
            bail!("pilot cancelled before provider transmission");
        }

        let first_outcome = match chain.analyze(provider, &vlm_request) {
            Ok(outcome) => outcome,
            Err(error) => {
                let failure = ProviderFailure::from_anyhow(&error);
                return Ok(self.finish_pilot_uncertain(
                    &running,
                    failure.safe_detail(),
                    "provider_error",
                    prior_models,
                    prior_cost_usd,
                ));
            }
        };

        let mut validated_response = first_outcome.response.clone();
        let mut prompt_tokens = validated_response.prompt_tokens;
        let mut completion_tokens = validated_response.completion_tokens;
        let mut cost_usd = validated_response.cost_usd;
        let mut usage_complete = first_outcome.usage_complete
            && validated_response.receipt.usage_complete
            && cost_usd.is_finite()
            && cost_usd >= 0.0;
        let mut attempted_models = prior_models.clone();
        attempted_models.extend(first_outcome.attempted_models.clone());
        let mut attempts = prior_attempts + first_outcome.attempts;
        let mut validation = validate_pilot_response(
            &validated_response.body,
            &vlm_request.schema,
            &policy,
            first_scope.source_duration_ms,
            &manifest.timestamps_ms,
            &schema_validator,
        );

        // The provider chain retries transport failures automatically. A
        // domain-invalid response gets one explicit permitted fallback attempt.
        if !validation.capability_observed
            && let Some(fallback_model) = chain.fallback_model()
            && fallback_model != validated_response.model.as_str()
        {
            chain.record_default_failure();
            match provider.analyze(fallback_model, &vlm_request) {
                Ok(fallback) => {
                    prompt_tokens = prompt_tokens.saturating_add(fallback.prompt_tokens);
                    completion_tokens =
                        completion_tokens.saturating_add(fallback.completion_tokens);
                    let fallback_cost_valid =
                        fallback.cost_usd.is_finite() && fallback.cost_usd >= 0.0;
                    if fallback_cost_valid {
                        cost_usd += fallback.cost_usd;
                    } else {
                        usage_complete = false;
                    }
                    usage_complete =
                        usage_complete && fallback.receipt.usage_complete && fallback_cost_valid;
                    attempted_models.push(fallback_model.to_string());
                    attempts += 1;
                    validated_response = fallback;
                    validation = validate_pilot_response(
                        &validated_response.body,
                        &vlm_request.schema,
                        &policy,
                        first_scope.source_duration_ms,
                        &manifest.timestamps_ms,
                        &schema_validator,
                    );
                }
                Err(error) => {
                    let failure = ProviderFailure::from_anyhow(&error);
                    let safe_cost = if cost_usd.is_finite() && cost_usd >= 0.0 {
                        prior_cost_usd + cost_usd
                    } else {
                        prior_cost_usd
                    };
                    return Ok(self.finish_pilot_uncertain(
                        &running,
                        failure.safe_detail(),
                        "provider_error",
                        attempted_models,
                        safe_cost,
                    ));
                }
            }
        }

        let aggregated_cost = prior_cost_usd + cost_usd;
        let cost_valid = aggregated_cost.is_finite() && aggregated_cost >= 0.0;
        let total_usage_complete = prior_usage_complete && usage_complete && cost_valid;
        let status = if validation.capability_observed && total_usage_complete {
            "passed"
        } else if total_usage_complete {
            "failed"
        } else {
            "uncertain"
        };
        let failure_code = if status == "passed" {
            String::new()
        } else if !total_usage_complete {
            "usage_unknown".to_string()
        } else if !validation.structural_schema_valid {
            "schema_validation_failed".to_string()
        } else {
            "source_time_semantics_failed".to_string()
        };
        let total_cost = if cost_valid {
            aggregated_cost
        } else {
            prior_cost_usd
        };
        let mut observed_capabilities = Vec::new();
        if validation.structural_schema_valid {
            observed_capabilities.push("configured JSON schema accepted this response".into());
        }
        if validation.source_time_semantics_valid {
            observed_capabilities
                .push("explicit source-time semantics accepted this response".into());
        }
        let warning = if prior_usage_complete {
            PILOT_WARNING.to_string()
        } else {
            format!("{PILOT_WARNING} Prior attempt usage was unknown; this retry cannot pass.")
        };
        let record = RunPilotRecord {
            schema_version: RUN_PILOT_SCHEMA_VERSION.into(),
            policy_id: policy.policy_id.clone(),
            project_id: policy.project_id.clone(),
            status: status.into(),
            started_at: started_at.clone(),
            finished_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            run_id: run_id.clone(),
            asset_id: first_scope.asset_id.clone(),
            requested_model: policy.primary_model.clone(),
            actual_model: validated_response.receipt.actual_model.clone(),
            provider: validated_response.receipt.provider.clone(),
            generation_id: validated_response.receipt.generation_id.clone(),
            attempted_models,
            attempts,
            prompt_tokens,
            completion_tokens,
            cost_usd: total_cost,
            usage_complete: total_usage_complete,
            failure_code: failure_code.clone(),
            prior_cost_usd,
            prior_attempts,
            validation: validation.clone(),
            observed_capabilities,
            warning,
        };
        self.save_pilot(&record)?;
        self.finish_analysis_run(
            &run_id,
            &crate::AnalysisRunOutcome {
                status: if status == "passed" {
                    "completed".into()
                } else {
                    "failed".into()
                },
                stage_reached: "pilot".into(),
                clips_analyzed: 0,
                cost_usd: record.cost_usd,
                error: if status == "passed" {
                    String::new()
                } else {
                    record.validation.detail.clone()
                },
                ..Default::default()
            },
        )?;
        Ok(record)
    }

    /// CLI-facing pilot entry point. Credentials are checked before a provider
    /// client is built; a missing key therefore performs zero provider calls.
    pub fn run_run_policy_pilot(
        &self,
        request: &RunPolicyRequest,
        retry_uncertain: bool,
    ) -> Result<RunPilotRecord> {
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
        let remaining_budget_usd = current.budget_usd - pilot.cost_usd;
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

    fn save_pilot(&self, record: &RunPilotRecord) -> Result<()> {
        let record_json = serde_json::to_string(record)?;
        self.connection()?.execute(
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

    fn finish_pilot_uncertain(
        &self,
        running: &RunPilotRecord,
        detail: String,
        failure_code: &str,
        attempted_models: Vec<String>,
        cost_usd: f64,
    ) -> RunPilotRecord {
        let safe_detail = bounded(&detail, 240);
        let record = RunPilotRecord {
            status: "uncertain".into(),
            finished_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            attempted_models,
            cost_usd: if cost_usd.is_finite() && cost_usd >= 0.0 {
                cost_usd
            } else {
                running.prior_cost_usd
            },
            usage_complete: false,
            failure_code: bounded(failure_code, 80),
            validation: PilotValidation {
                structural_schema_valid: false,
                source_time_semantics_valid: false,
                capability_observed: false,
                detail: safe_detail.clone(),
            },
            ..running.clone()
        };
        let _ = self.save_pilot(&record);
        let _ = self.finish_analysis_run(
            &running.run_id,
            &crate::AnalysisRunOutcome {
                status: "failed".into(),
                stage_reached: "pilot".into(),
                cost_usd: record.cost_usd,
                error: safe_detail,
                ..Default::default()
            },
        );
        record
    }
}

#[derive(Clone, Copy)]
enum ObservationSchemaKind {
    Observation,
    Moment,
}

fn validate_pilot_response(
    body: &str,
    schema: &Value,
    policy: &RunPolicy,
    source_duration_ms: u32,
    frame_timestamps: &[u32],
    kind: &ObservationSchemaKind,
) -> PilotValidation {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return PilotValidation {
            structural_schema_valid: false,
            source_time_semantics_valid: false,
            capability_observed: false,
            detail: "provider response was not JSON".into(),
        };
    };
    let structural = schema::validate_json_structure(&value, schema);
    let structural_schema_valid = structural.is_ok();
    let semantic: Result<(), &'static str> = match kind {
        ObservationSchemaKind::Observation => {
            schema::validate(body, frame_timestamps, source_duration_ms)
                .map(|_| ())
                .map_err(|error| error.safe_code())
        }
        ObservationSchemaKind::Moment => {
            let span = MomentSpan {
                start_ms: 0,
                end_ms: source_duration_ms,
            };
            let keys = frame_timestamps
                .iter()
                .map(|at| format!("frame:{at}"))
                .collect::<Vec<_>>();
            let profile = match &policy.parameters {
                RunPolicyParameters::MomentDiscovery { profile, .. } => *profile,
                RunPolicyParameters::Observation { .. } => EditProfile::default(),
            };
            validate_reply(body, span, profile, &keys, 1)
                .map(|_| ())
                .map_err(|_| "moment_source_time_semantics_invalid")
        }
    };
    let detail = match (&structural, &semantic) {
        (Ok(()), Ok(())) => "bounded_schema_and_source_time_semantics_passed".to_string(),
        (Err(error), _) => format!("schema_validation:{}", error.safe_code()),
        (_, Err(code)) => format!("source_time_semantics:{code}"),
    };
    PilotValidation {
        structural_schema_valid,
        source_time_semantics_valid: semantic.is_ok(),
        capability_observed: structural_schema_valid && semantic.is_ok(),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::moments::{MomentPayload, MomentRunOptions};
    use crate::analysis::provider::{VlmReceipt, VlmRequest, VlmResponse};
    use crate::analysis::quality::DecodedSegment;
    use crate::analysis::store::{StoredTranscript, TranscriptOutcomeReason, save_transcript};
    use rusqlite::params;
    use std::fs;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
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

        fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
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
                    generation_id: Some("generation-1".into()),
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
            eprintln!("ffmpeg unavailable; skipping synthetic run-policy test");
            panic!("synthetic ftest requires ffmpeg")
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

    fn fixture() -> (tempfile::TempDir, Catalog, String, String) {
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
        fn analyze(&self, model: &str, request: &VlmRequest) -> Result<VlmResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
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
                    generation_id: Some("generation-capture".into()),
                    usage_complete: self.usage_complete,
                },
            })
        }
    }

    fn save_private_transcript(catalog: &Catalog, asset_id: &str) {
        save_transcript(
            &catalog.connection().unwrap(),
            &StoredTranscript {
                asset_id: asset_id.to_string(),
                recipe_key: crate::analysis::STT_RECIPE_KEY.to_string(),
                source_fingerprint: "synthetic-source".into(),
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
                    mean_token_probability: 0.9,
                }],
            },
        )
        .unwrap();
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
        assert!((first_pilot.cost_usd - 0.01).abs() < 1e-9);

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
}
