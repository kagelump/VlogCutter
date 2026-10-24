//! Bounded runtime phases for the authorized analysis pilot.
//!
//! This module owns local request preparation and the one-at-a-time accounted
//! provider execution. Canonical admission and settlement remain in
//! `analysis::accounting`; run lifecycle and projections remain in
//! `analysis::run_policy`.

use anyhow::{Context, Result, bail, ensure};
use serde_json::json;

use crate::Catalog;
use crate::analysis::accounting::AccountedVlmProvider;
use crate::analysis::framepack;
use crate::analysis::moments::{EditProfile, MomentSpan, moment_output_schema, validate_reply};
use crate::analysis::openrouter::frame_data_uri;
use crate::analysis::provider::{ProviderChain, VlmProvider, VlmRequest};
use crate::analysis::run_policy::{
    AnalysisRunKind, PilotValidation, RunAssetScope, RunPilotRecord, RunPolicy,
    RunPolicyParameters, RunPolicyRequest, pilot_accounting_context, pilot_authority_context,
};
use crate::analysis::schema::{self, output_schema as observation_output_schema};
use crate::analysis::vlm::{PromptContext, build_prompt};

#[derive(Debug, Clone, Default)]
pub(crate) struct PilotAttemptBaseline {
    pub attempted_models: Vec<String>,
    pub attempts: u32,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

impl PilotAttemptBaseline {
    pub(crate) fn from_prior_record(record: &RunPilotRecord) -> Self {
        Self {
            attempted_models: record.attempted_models.clone(),
            attempts: record.attempts,
            prompt_tokens: record.prompt_tokens,
            completion_tokens: record.completion_tokens,
        }
    }
}

pub(crate) enum PilotSessionStart {
    AlreadyPassed(Box<RunPilotRecord>),
    Ready(Box<PilotSession>),
}

pub(crate) struct PilotSession {
    pub policy: RunPolicy,
    pub first_scope: RunAssetScope,
    pub previous_record: Option<RunPilotRecord>,
    pub had_previous_pilot: bool,
    pub prior_cost_usd: f64,
    pub prior_attempts: u32,
    pub baseline: PilotAttemptBaseline,
}

/// Validate authorization/history, freeze the authority and resolve the first
/// scoped asset before any local media work or owned pilot row is created.
pub(crate) fn prepare_pilot_session(
    catalog: &Catalog,
    request: &RunPolicyRequest,
    retry_uncertain: bool,
) -> Result<PilotSessionStart> {
    let policy = catalog.describe_run_policy(request)?;
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
    catalog.load_run_authorization(&policy.policy_id)?;

    let previous_record = catalog.load_run_pilot(&policy.policy_id)?;
    let mut prior_cost_usd = 0.0;
    let mut prior_attempts = 0u32;
    if let Some(existing) = previous_record.as_ref() {
        if existing.status == "passed" {
            return Ok(PilotSessionStart::AlreadyPassed(Box::new(existing.clone())));
        }
        ensure!(
            existing.status != "running",
            "pilot has interrupted or unknown usage; reconcile its receipt before any retry"
        );
        ensure!(
            existing.usage_complete || retry_uncertain,
            "pilot has interrupted or unknown usage; reconcile its receipt before any retry"
        );
        ensure!(
            retry_uncertain,
            "a prior pilot failed; inspect its receipt before an explicit retry"
        );
        prior_cost_usd = existing.cost_usd;
        prior_attempts = existing.attempts;
    }
    ensure!(
        prior_cost_usd.is_finite() && prior_cost_usd >= 0.0,
        "prior pilot cost is invalid"
    );
    ensure!(
        prior_cost_usd < policy.budget_usd,
        "prior pilot attempts consumed the authorized run budget"
    );

    // Freeze legacy evidence before creating the newly owned run/pilot row.
    let authority =
        catalog.initialize_analysis_accounting_authority(&pilot_authority_context(&policy))?;
    ensure!(
        !authority.dispatch_stopped,
        "analysis budget authority dispatch is stopped: {}",
        authority.stop_reason
    );

    let first_scope = first_scope(&policy)?;
    // A force-generated replacement policy must not bypass unresolved paid work.
    for unresolved in catalog.analysis_requests_needing_reconciliation_for_source(
        &policy.project_id,
        "pilot",
        &first_scope.source_identity,
    )? {
        ensure!(
            unresolved.policy_id == policy.policy_id,
            "a prior pilot request has unresolved usage; reconcile it and authorize the exact prior request before a replacement policy can retry"
        );
    }

    let baseline = match previous_record.as_ref() {
        Some(record) if !catalog.analysis_run_is_accounted(&record.run_id)? => {
            PilotAttemptBaseline::from_prior_record(record)
        }
        _ => PilotAttemptBaseline::default(),
    };
    let had_previous_pilot = previous_record.is_some();
    Ok(PilotSessionStart::Ready(Box::new(PilotSession {
        policy,
        first_scope,
        previous_record,
        had_previous_pilot,
        prior_cost_usd,
        prior_attempts,
        baseline,
    })))
}

fn first_scope(policy: &RunPolicy) -> Result<RunAssetScope> {
    policy
        .scope
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("run policy has no scoped asset for its pilot"))
}

#[derive(Clone, Copy)]
enum PilotSchemaKind {
    Observation,
    Moment,
}

pub(crate) struct PreparedPilotTransmission {
    pub request: VlmRequest,
    schema_kind: PilotSchemaKind,
    frame_timestamps: Vec<u32>,
    source_duration_ms: u32,
}

impl PreparedPilotTransmission {
    pub(crate) fn validate(&self, policy: &RunPolicy, body: &str) -> PilotValidation {
        validate_pilot_response(
            body,
            &self.request.schema,
            policy,
            self.source_duration_ms,
            &self.frame_timestamps,
            &self.schema_kind,
        )
    }
}

/// Prepare one exactly-approved local payload. No provider reservation or
/// network call exists in this phase.
pub(crate) fn prepare_pilot_transmission<C: FnMut() -> bool>(
    catalog: &Catalog,
    request: &RunPolicyRequest,
    policy: &RunPolicy,
    first_scope: &RunAssetScope,
    is_cancelled: &mut C,
) -> Result<PreparedPilotTransmission> {
    let (manifest, frames) = prepare_frames(catalog, first_scope, is_cancelled)?;
    let transcript = if policy.payload.uses_transcript() {
        catalog.prompt_transcript(&first_scope.asset_id)?
    } else {
        None
    };
    let vocabulary = catalog.run_policy_vocabulary(policy)?;
    let (vlm_request, schema_kind) = build_pilot_request(
        policy,
        first_scope,
        &manifest.timestamps_ms,
        frames,
        transcript,
        vocabulary,
    );
    if is_cancelled() {
        bail!("pilot cancelled before provider transmission");
    }
    // Source and approved context may change while waiting for the local-heavy
    // lease or preparing frames. Never transmit under stale approval.
    ensure!(
        catalog.describe_run_policy(request)?.policy_id == policy.policy_id,
        "run policy or source identity changed before pilot transmission"
    );
    Ok(PreparedPilotTransmission {
        request: vlm_request,
        schema_kind,
        frame_timestamps: manifest.timestamps_ms,
        source_duration_ms: first_scope.source_duration_ms,
    })
}

fn prepare_frames<C: FnMut() -> bool>(
    catalog: &Catalog,
    first_scope: &RunAssetScope,
    is_cancelled: &mut C,
) -> Result<(framepack::FramePackManifest, Vec<String>)> {
    let asset = catalog.get_asset(&first_scope.asset_id)?;
    let heavy = catalog.acquire_local_heavy_cancellable(&mut *is_cancelled)?;
    let manifest = catalog.analysis_frame_pack(&asset)?;
    if is_cancelled() {
        drop(heavy);
        bail!("pilot cancelled before provider transmission");
    }
    let proxy = asset
        .proxy_path
        .as_deref()
        .filter(|path| std::path::Path::new(path).is_file())
        .ok_or_else(|| anyhow::anyhow!("source proxy is no longer available"))?;
    let parent = catalog
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
    Ok((manifest, frames))
}

fn build_pilot_request(
    policy: &RunPolicy,
    first_scope: &RunAssetScope,
    frame_timestamps_ms: &[u32],
    frames: Vec<String>,
    transcript: Option<crate::analysis::vlm::PromptTranscript>,
    vocabulary: Vec<String>,
) -> (VlmRequest, PilotSchemaKind) {
    match policy.kind {
        AnalysisRunKind::Observation => {
            let context = PromptContext {
                frame_timestamps_ms: frame_timestamps_ms.to_vec(),
                clip_duration_ms: first_scope.source_duration_ms,
                transcript,
                vocabulary,
                trip_context: None,
            };
            (
                VlmRequest {
                    prompt: build_prompt(&context),
                    frames,
                    schema: observation_output_schema(
                        frame_timestamps_ms,
                        first_scope.source_duration_ms,
                    ),
                },
                PilotSchemaKind::Observation,
            )
        }
        AnalysisRunKind::MomentDiscovery => {
            let keys = frame_timestamps_ms
                .iter()
                .map(|at| format!("frame:{at}"))
                .collect::<Vec<_>>();
            let schema = moment_output_schema(1, first_scope.source_duration_ms, &keys);
            let evidence = frame_timestamps_ms
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
                    "frameTimesMs": frame_timestamps_ms,
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
            (
                VlmRequest {
                    prompt,
                    frames,
                    schema,
                },
                PilotSchemaKind::Moment,
            )
        }
    }
}

pub(crate) struct PilotExecution {
    pub validation: PilotValidation,
    pub cancelled_before_domain_fallback: bool,
}

/// Execute the chain and the one explicitly permitted domain-validation
/// fallback through the accounted boundary. Each actual call reserves and
/// receipts independently.
pub(crate) fn execute_accounted_pilot_attempts<C: FnMut() -> bool>(
    catalog: &Catalog,
    policy: &RunPolicy,
    provider: &dyn VlmProvider,
    prepared: &PreparedPilotTransmission,
    run_id: &str,
    retry_uncertain: bool,
    is_cancelled: &mut C,
) -> Result<PilotExecution> {
    let chain = ProviderChain::with_policy(
        &policy.primary_model,
        &policy.allowed_models,
        policy.allow_fallback,
    )?;
    let accounted = AccountedVlmProvider::new(
        catalog,
        pilot_accounting_context(policy, run_id, retry_uncertain)?,
        provider,
    )
    .with_explicit_retry(retry_uncertain);

    let first = chain.analyze_with_cancel(&accounted, &prepared.request, is_cancelled)?;
    let mut response = first.response;
    let mut validation = prepared.validate(policy, &response.body);
    if validation.capability_observed {
        return Ok(PilotExecution {
            validation,
            cancelled_before_domain_fallback: false,
        });
    }
    let Some(fallback_model) = chain.fallback_model() else {
        return Ok(PilotExecution {
            validation,
            cancelled_before_domain_fallback: false,
        });
    };
    if fallback_model == response.model.as_str() {
        return Ok(PilotExecution {
            validation,
            cancelled_before_domain_fallback: false,
        });
    }
    if is_cancelled() {
        return Ok(PilotExecution {
            validation,
            cancelled_before_domain_fallback: true,
        });
    }
    chain.record_default_failure();
    response = accounted.analyze_with_cancel(fallback_model, &prepared.request, is_cancelled)?;
    validation = prepared.validate(policy, &response.body);
    Ok(PilotExecution {
        validation,
        cancelled_before_domain_fallback: false,
    })
}

#[allow(clippy::too_many_arguments)]
fn validate_pilot_response(
    body: &str,
    schema: &serde_json::Value,
    policy: &RunPolicy,
    source_duration_ms: u32,
    frame_timestamps: &[u32],
    kind: &PilotSchemaKind,
) -> PilotValidation {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
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
        PilotSchemaKind::Observation => {
            schema::validate(body, frame_timestamps, source_duration_ms)
                .map(|_| ())
                .map_err(|error| error.safe_code())
        }
        PilotSchemaKind::Moment => {
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
