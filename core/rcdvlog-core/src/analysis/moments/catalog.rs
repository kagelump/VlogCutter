//! Catalog-owned, resumable moment discovery. Adapters never write derived rows.

use super::{
    logic::*,
    model::*,
    store::{self, MomentStep},
};
use crate::analysis::{
    framepack,
    openrouter::{OpenRouterProvider, frame_data_uri, resolve_api_key},
    provider::{ProviderChain, VlmProvider, VlmRequest, VlmResponse},
    store::{TranscriptStatus, load_transcript},
    vlm_store::{analysis_is_current, load_analysis},
};
use crate::{
    AnalysisRunOutcome, Asset, Catalog, RepresentationBusy, RepresentationKind, RunPolicyRequest,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use batch_estimator::{BatchEstimate, BatchEstimator};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    ops::ControlFlow,
    path::PathBuf,
    sync::OnceLock,
    time::Instant,
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentProgress {
    pub run_id: String,
    pub asset_id: String,
    pub phase: String,
    pub cost_usd: f64,
    pub estimate: BatchEstimate,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentRunReport {
    pub run_id: String,
    pub total_assets: usize,
    pub processed: usize,
    pub ready: usize,
    pub no_candidates: usize,
    pub needs_review: usize,
    pub skipped: usize,
    pub failed: usize,
    pub cost_usd: f64,
    pub usage_complete: bool,
    pub cancelled: bool,
    pub budget_exhausted: bool,
    pub failures: Vec<MomentFailure>,
    pub failures_truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentFailure {
    pub asset_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentPage {
    pub schema_version: String,
    pub current: bool,
    pub artifact: MomentArtifact,
    pub total_candidates: usize,
    pub total_coverage_segments: usize,
    pub total_receipts: usize,
    pub total_review_questions: usize,
    pub offset: usize,
    pub next_offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentInventoryItem {
    pub asset_id: String,
    pub status: Option<MomentStatus>,
    pub current: bool,
    pub candidate_count: usize,
    pub questions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentInventoryPage {
    pub schema_version: String,
    pub schema_ready: bool,
    pub project_id: String,
    pub items: Vec<MomentInventoryItem>,
    pub offset: usize,
    pub next_offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentPlan {
    pub schema_version: String,
    pub asset_ids: Vec<String>,
    pub provider: String,
    pub requested_model: String,
    pub payload: MomentPayload,
    pub budget_usd: f64,
    pub coarse_frames_per_asset: usize,
    pub refinement_frames_per_asset: usize,
    pub max_candidates_per_asset: usize,
    pub profile: EditProfile,
    pub policy: String,
    /// Fully materialized policy: exact scoped source identities, explicit
    /// model allowlist/fallbacks, payload, expected cost, budget and readiness.
    pub run_policy: crate::RunPolicy,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct ProviderUnavailable(String);

/// Resolves credentials only when a missing step actually needs a paid call.
/// Fully cached runs remain usable offline without a key.
struct DeferredProvider(OnceLock<std::result::Result<OpenRouterProvider, String>>);
impl DeferredProvider {
    fn client(&self) -> Result<&OpenRouterProvider> {
        match self.0.get_or_init(|| {
            resolve_api_key()
                .and_then(OpenRouterProvider::new)
                .map_err(|_| {
                    "OpenRouter credentials are not available; no moment request was made"
                        .to_string()
                })
        }) {
            Ok(provider) => Ok(provider),
            Err(_) => Err(ProviderUnavailable(
                "OpenRouter credentials are not available; no moment request was made".into(),
            )
            .into()),
        }
    }
}
impl VlmProvider for DeferredProvider {
    fn prepare(&self) -> Result<()> {
        self.client().map(|_| ())
    }
    fn analyze(&self, model: &str, request: &VlmRequest) -> Result<VlmResponse> {
        self.client()?.analyze(model, request)
    }
}

#[derive(Default)]
struct RunState {
    run_id: String,
    executed_steps: usize,
    reused_steps: usize,
    historical_unknown: bool,
    spent: f64,
    unknown_usage: bool,
    cancelled: bool,
    budget_stopped: bool,
    approved_policy: Option<crate::RunPolicy>,
}

struct SourceContext {
    proxy: PathBuf,
    duration_ms: u32,
    last_safe_sample_ms: u32,
    fingerprint: String,
    transcript_fingerprint: Option<String>,
    observation_fingerprint: Option<String>,
    dialogue: Vec<(MomentSpan, String)>,
    coarse_observation: Option<MomentSpan>,
}

impl SourceContext {
    fn bound_sample_times(&self, mut times: Vec<u32>) -> Vec<u32> {
        // A seek after the final frame's presentation time can succeed without
        // producing an image. Keep the tail sample at least one proxy frame
        // period before EOF, while coverage retains the entire source span and
        // records the resulting unsampled tail in max_gap_ms.
        for time in &mut times {
            *time = (*time).min(self.last_safe_sample_ms);
        }
        times.dedup();
        times
    }
}

fn digest(value: &impl Serialize) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}

impl Catalog {
    pub fn plan_moments(
        &self,
        project_id: &str,
        asset_ids: &[String],
        options: &MomentRunOptions,
    ) -> Result<MomentPlan> {
        options.validate()?;
        let assets = self.moment_scope(project_id, asset_ids)?;
        let policy = self.describe_run_policy(&crate::RunPolicyRequest::moment_discovery(
            project_id.to_string(),
            assets.iter().map(|asset| asset.id.clone()).collect(),
            options.model.clone(),
            options.allowed_models.clone(),
            options.allow_fallback,
            match options.payload {
                MomentPayload::Frames => crate::AnalysisPayload::Frames,
                MomentPayload::FramesAndTranscript => crate::AnalysisPayload::FramesAndTranscript,
            },
            options.budget_usd,
            options.coarse_frames,
            options.refinement_frames,
            options.max_candidates,
            options.profile,
            options.force,
            options.retry_uncertain,
        ))?;
        Ok(MomentPlan {
            schema_version: SCHEMA_VERSION.into(), asset_ids: assets.iter().map(|a| a.id.clone()).collect(),
            provider: options.provider.clone(), requested_model: options.model.clone(), payload: options.payload,
            budget_usd: options.budget_usd, coarse_frames_per_asset: options.coarse_frames,
            refinement_frames_per_asset: options.refinement_frames, max_candidates_per_asset: options.max_candidates,
            profile: options.profile,
            policy: "Explicit selected assets only; proxy JPEG frames and optionally accepted timed speech; explicit model allowlist and permitted fallbacks; stop before the next request at the spend ceiling or after unknown usage. No selection or human-metadata mutations.".into(),
            run_policy: policy,
        })
    }

    fn moment_scope(&self, project_id: &str, asset_ids: &[String]) -> Result<Vec<Asset>> {
        self.require_project(project_id)?;
        ensure!(
            !asset_ids.is_empty() && asset_ids.len() <= 10000,
            "moment scope must explicitly select 1..=10000 asset ids"
        );
        let mut seen = HashSet::new();
        let mut assets = Vec::new();
        for id in asset_ids {
            if !seen.insert(id) {
                continue;
            }
            let asset = self.get_asset(id)?;
            ensure!(
                asset.project_id == project_id,
                "asset {id} is outside the authorized project scope"
            );
            assets.push(asset);
        }
        Ok(assets)
    }

    pub fn discover_moments_for_assets<F>(
        &self,
        project_id: &str,
        asset_ids: &[String],
        options: &MomentRunOptions,
        on_progress: F,
    ) -> Result<MomentRunReport>
    where
        F: FnMut(MomentProgress) -> ControlFlow<()>,
    {
        self.discover_moments_for_assets_with_provider(
            project_id,
            asset_ids,
            options,
            &DeferredProvider(OnceLock::new()),
            on_progress,
        )
    }

    /// Public paid boundary for an explicit scope. If the caller has not
    /// supplied a stored policy id, the explicit invocation materializes one,
    /// persists approval separately, and runs a bounded pilot before discovery.
    /// Caller flags can never broaden the stored policy.
    pub(crate) fn discover_moments_for_assets_with_provider<F>(
        &self,
        project_id: &str,
        asset_ids: &[String],
        options: &MomentRunOptions,
        provider: &dyn VlmProvider,
        mut on_progress: F,
    ) -> Result<MomentRunReport>
    where
        F: FnMut(MomentProgress) -> ControlFlow<()>,
    {
        if options.policy_id.is_some() {
            return self.discover_moments_with_provider(
                project_id,
                asset_ids,
                options,
                provider,
                on_progress,
            );
        }
        options.validate()?;
        let request = RunPolicyRequest::moment_discovery(
            project_id.to_string(),
            asset_ids.to_vec(),
            options.model.clone(),
            options.allowed_models.clone(),
            options.allow_fallback,
            match options.payload {
                MomentPayload::Frames => crate::AnalysisPayload::Frames,
                MomentPayload::FramesAndTranscript => crate::AnalysisPayload::FramesAndTranscript,
            },
            options.budget_usd,
            options.coarse_frames,
            options.refinement_frames,
            options.max_candidates,
            options.profile,
            options.force,
            options.retry_uncertain,
        );
        self.authorize_run_policy(&request)?;
        let mut cancelled = false;
        let pilot_result = self.run_run_policy_pilot_with_provider_and_cancel(
            &request,
            provider,
            options.retry_uncertain,
            || {
                let mut estimate = BatchEstimator::new(&[], 1, Default::default()).estimate(&[]);
                estimate.confidence = "capability_pilot".into();
                estimate.estimated_remaining_seconds = None;
                cancelled |= on_progress(MomentProgress {
                    run_id: String::new(),
                    asset_id: asset_ids.first().cloned().unwrap_or_default(),
                    phase: "pilot".into(),
                    cost_usd: 0.0,
                    estimate,
                })
                .is_break();
                cancelled
            },
        );
        if cancelled {
            return Ok(MomentRunReport {
                run_id: String::new(),
                total_assets: asset_ids.len(),
                processed: 0,
                ready: 0,
                no_candidates: 0,
                needs_review: 0,
                skipped: 0,
                failed: 0,
                cost_usd: 0.0,
                usage_complete: true,
                cancelled: true,
                budget_exhausted: false,
                failures: Vec::new(),
                failures_truncated: false,
            });
        }
        let pilot = pilot_result?;
        ensure!(
            pilot.status == "passed",
            "capability pilot did not pass for this exact moment policy: {} ({}); no moment requests were made",
            pilot.failure_code,
            pilot.validation.detail
        );
        let state = self.authorized_policy_for_bulk(
            &pilot.policy_id,
            crate::AnalysisRunKind::MomentDiscovery,
        )?;
        let bound_options = state.authorization.policy.moment_options()?;
        self.discover_moments_with_provider(
            project_id,
            asset_ids,
            &bound_options,
            provider,
            on_progress,
        )
    }

    /// Execute only the exact scope and settings authorized by `policy_id`.
    /// This is the provider-injectable bulk boundary used by adapters and
    /// synthetic tests; the stored policy is authoritative.
    pub fn discover_authorized_moments_with_provider<F>(
        &self,
        policy_id: &str,
        provider: &dyn VlmProvider,
        on_progress: F,
    ) -> Result<MomentRunReport>
    where
        F: FnMut(MomentProgress) -> ControlFlow<()>,
    {
        let state = self.authorized_policy_for_bulk(
            policy_id,
            crate::analysis::run_policy::AnalysisRunKind::MomentDiscovery,
        )?;
        let options = state.authorization.policy.moment_options()?;
        let project_id = state.authorization.policy.project_id.clone();
        let asset_ids = state
            .authorization
            .policy
            .scope
            .iter()
            .map(|asset| asset.asset_id.clone())
            .collect::<Vec<_>>();
        self.discover_moments_with_provider(
            &project_id,
            &asset_ids,
            &options,
            provider,
            on_progress,
        )
    }

    pub(crate) fn discover_moments_with_provider<F>(
        &self,
        project_id: &str,
        asset_ids: &[String],
        options: &MomentRunOptions,
        provider: &dyn VlmProvider,
        mut on_progress: F,
    ) -> Result<MomentRunReport>
    where
        F: FnMut(MomentProgress) -> ControlFlow<()>,
    {
        options.validate()?;
        let assets = self.moment_scope(project_id, asset_ids)?;
        let mut effective_options = options.clone();
        if effective_options.allowed_models.is_empty() {
            effective_options.allowed_models = vec![effective_options.model.clone()];
        }
        let mut approved_policy = None;
        if let Some(policy_id) = &options.policy_id {
            let state = self.authorized_policy_for_bulk(
                policy_id,
                crate::analysis::run_policy::AnalysisRunKind::MomentDiscovery,
            )?;
            let policy = &state.authorization.policy;
            ensure!(
                policy.project_id == project_id,
                "authorized policy belongs to a different project"
            );
            let scoped = assets
                .iter()
                .map(|asset| asset.id.as_str())
                .collect::<Vec<_>>();
            let authorized = policy
                .scope
                .iter()
                .map(|asset| asset.asset_id.as_str())
                .collect::<Vec<_>>();
            ensure!(
                scoped == authorized,
                "authorized run scope does not exactly match the requested asset ids"
            );
            ensure!(
                policy.primary_model == options.model
                    && policy.payload
                        == match options.payload {
                            MomentPayload::Frames => crate::AnalysisPayload::Frames,
                            MomentPayload::FramesAndTranscript => {
                                crate::AnalysisPayload::FramesAndTranscript
                            }
                        }
                    && (policy.budget_usd - options.budget_usd).abs() < f64::EPSILON,
                "run model, payload or requested budget does not match the authorized policy"
            );
            ensure!(
                effective_options.policy_id.is_some()
                    && policy_id == &effective_options.policy_id.clone().unwrap(),
                "internal policy binding mismatch"
            );
            effective_options.budget_usd = state.remaining_budget_usd;
            effective_options.allowed_models = policy.allowed_models.clone();
            effective_options.allow_fallback = policy.allow_fallback;
            approved_policy = Some(policy.clone());
        }
        provider.prepare()?;
        let options = &effective_options;
        let run_id = self.begin_analysis_run(project_id)?;
        // Reuse the shared run account, identifying the stage before any call.
        self.connection()?.execute(
            "UPDATE analysis_runs SET stage_reached='moments',policy_id=?2 WHERE id=?1",
            params![run_id, options.policy_id.clone().unwrap_or_default()],
        )?;
        let task_key = format!(
            "{RECIPE_VERSION}.{}.w1",
            digest(&(
                &options.provider,
                &options.model,
                &options.allowed_models,
                options.allow_fallback,
                options.payload,
                options.coarse_frames,
                options.refinement_frames,
                options.max_candidates,
                options.profile,
                options.policy_id.clone(),
            ))?
        );
        let weights = vec![1; assets.len()];
        let mut estimator = BatchEstimator::new(&weights, 1, self.load_batch_history(&task_key)?);
        let mut state = RunState {
            run_id: run_id.clone(),
            approved_policy,
            ..Default::default()
        };
        let mut report = MomentRunReport {
            run_id: run_id.clone(),
            total_assets: assets.len(),
            processed: 0,
            ready: 0,
            no_candidates: 0,
            needs_review: 0,
            skipped: 0,
            failed: 0,
            cost_usd: 0.0,
            usage_complete: true,
            cancelled: false,
            budget_exhausted: false,
            failures: vec![],
            failures_truncated: false,
        };
        let chain = ProviderChain::with_policy(
            &options.model,
            &options.allowed_models,
            options.allow_fallback,
        )?;
        for (index, asset) in assets.iter().enumerate() {
            let estimate = estimator.estimate(&weights[index..]);
            let mut notify = |phase: &str, cost: f64| {
                on_progress(MomentProgress {
                    run_id: run_id.clone(),
                    asset_id: asset.id.clone(),
                    phase: phase.into(),
                    cost_usd: cost,
                    estimate: estimate.clone(),
                })
            };
            if notify("preflight", state.spent).is_break() {
                state.cancelled = true;
                break;
            }
            let started = Instant::now();
            let executed_before = state.executed_steps;
            let reused_before = state.reused_steps;
            let outcome = self.discover_asset_moments(
                asset,
                options,
                provider,
                &chain,
                &mut state,
                &mut notify,
            );
            match outcome {
                Ok(artifact) => {
                    report.processed += 1;
                    match artifact.status {
                        MomentStatus::Ready => report.ready += 1,
                        MomentStatus::NoCandidates => report.no_candidates += 1,
                        MomentStatus::NeedsReview | MomentStatus::Unavailable => {
                            report.needs_review += 1
                        }
                    }
                    if state.executed_steps > executed_before
                        && state.reused_steps == reused_before
                        && matches!(
                            artifact.status,
                            MomentStatus::Ready | MomentStatus::NoCandidates
                        )
                    {
                        estimator.record_completion(1, started.elapsed());
                    } else {
                        estimator.record_failure(1);
                    }
                }
                Err(error) if error.is::<RepresentationBusy>() => {
                    report.skipped += 1;
                    estimator.record_failure(1);
                }
                Err(error) => {
                    report.failed += 1;
                    estimator.record_failure(1);
                    if report.failures.len() < 100 {
                        report.failures.push(MomentFailure {
                            asset_id: asset.id.clone(),
                            reason: format!("{error:#}"),
                        });
                    } else {
                        report.failures_truncated = true;
                    }
                }
            }
            // Existing shared accounting preserves spend as each source commits.
            self.connection()?.execute(
                "UPDATE analysis_runs SET cost_usd=?2,clips_analyzed=?3 WHERE id=?1",
                params![run_id, state.spent, report.processed as i64],
            )?;
            let flow = on_progress(MomentProgress {
                run_id: run_id.clone(),
                asset_id: asset.id.clone(),
                phase: "asset_complete".into(),
                cost_usd: state.spent,
                estimate: estimator.estimate(&weights[index + 1..]),
            });
            if flow.is_break() {
                state.cancelled = true;
            }
            if state.cancelled || state.budget_stopped || state.unknown_usage {
                break;
            }
        }
        report.cost_usd = state.spent;
        report.usage_complete = !state.unknown_usage && !state.historical_unknown;
        report.cancelled = state.cancelled;
        report.budget_exhausted = state.budget_stopped;
        self.save_batch_history(&task_key, estimator.history())?;
        self.finish_analysis_run(
            &run_id,
            &AnalysisRunOutcome {
                status: if state.cancelled {
                    "cancelled"
                } else if report.failed > 0 || state.unknown_usage {
                    "failed"
                } else {
                    "completed"
                }
                .into(),
                stage_reached: "moments".into(),
                clips_analyzed: report.processed,
                cost_usd: state.spent,
                budget_exhausted: state.budget_stopped,
                error: if state.unknown_usage {
                    "provider usage unknown; explicit recovery required".into()
                } else {
                    String::new()
                },
                ..Default::default()
            },
        )?;
        Ok(report)
    }

    fn moment_source(&self, asset: &Asset, include_transcript: bool) -> Result<SourceContext> {
        let source = self.analysis_proxy_source(asset)?;
        let proxy = source.proxy;
        let duration_ms = source.duration_ms;
        let last_safe_sample_ms = source.last_safe_sample_ms;
        let fingerprint = source.fingerprint;
        let sampled = source.sampled_fingerprint;
        let connection = self.connection()?;
        let transcript = if include_transcript {
            load_transcript(&connection, &asset.id)?.filter(|t| {
                t.source_fingerprint == sampled && t.recipe_key == crate::analysis::STT_RECIPE_KEY
            })
        } else {
            None
        };
        let transcript_fingerprint = transcript.as_ref().map(digest).transpose()?;
        let dialogue = transcript
            .filter(|t| t.status == TranscriptStatus::Speech)
            .map(|t| {
                t.segments
                    .into_iter()
                    .filter_map(|s| {
                        let span = MomentSpan {
                            start_ms: s.start_ms,
                            end_ms: s.end_ms.min(duration_ms),
                        };
                        (span.start_ms < span.end_ms).then_some((span, s.text))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let observation = if analysis_is_current(&connection, &asset.id, &sampled)? {
            load_analysis(&connection, &asset.id)?
        } else {
            None
        };
        let observation_fingerprint = observation.as_ref().map(digest).transpose()?;
        let coarse_observation = observation
            .and_then(|o| {
                Some(MomentSpan {
                    start_ms: o.best_moment_start_ms?,
                    end_ms: o.best_moment_end_ms?,
                })
            })
            .filter(|s| s.start_ms < s.end_ms && s.end_ms <= duration_ms);
        Ok(SourceContext {
            proxy,
            duration_ms,
            last_safe_sample_ms,
            fingerprint,
            transcript_fingerprint,
            observation_fingerprint,
            dialogue,
            coarse_observation,
        })
    }

    fn discover_asset_moments(
        &self,
        asset: &Asset,
        options: &MomentRunOptions,
        provider: &dyn VlmProvider,
        chain: &ProviderChain,
        state: &mut RunState,
        notify: &mut dyn FnMut(&str, f64) -> ControlFlow<()>,
    ) -> Result<MomentArtifact> {
        let _claim = self.claim_representation(&asset.id, RepresentationKind::Moments)?;
        // Re-encoding the same proxy during paid inspection is an integrity
        // conflict. Reuse representation ownership rather than a global app lock.
        let _proxy_claim = self.claim_representation(&asset.id, RepresentationKind::Proxy)?;
        // Scope snapshots can outlive earlier clips in the batch. Honor any
        // intervening exclusion, metadata correction or proxy registration.
        let refreshed = self.get_asset(&asset.id)?;
        ensure!(
            refreshed.project_id == asset.project_id,
            "asset changed project scope"
        );
        let asset = &refreshed;
        let source = match self
            .moment_source(asset, options.payload == MomentPayload::FramesAndTranscript)
        {
            Ok(source) => source,
            Err(error) => {
                return self.unavailable_moment_artifact(asset, options, format!("{error:#}"));
            }
        };
        let configuration_hash = digest(&(
            RECIPE_VERSION,
            PROMPT_VERSION,
            SCHEMA_VERSION,
            TIME_MAPPING,
            &options.model,
            &options.provider,
            options.payload,
            options.coarse_frames,
            options.refinement_frames,
            options.max_candidates,
            options.profile,
        ))?;
        let identity = digest(&(
            &asset.id,
            &source.fingerprint,
            source.duration_ms,
            &source.transcript_fingerprint,
            &source.observation_fingerprint,
            &configuration_hash,
        ))?;
        let unresolved_receipts = self.unresolved_moment_receipts(&asset.id, &identity)?;
        if !unresolved_receipts.is_empty() {
            state.historical_unknown = true;
            if !options.retry_uncertain {
                state.unknown_usage = true;
            }
        }
        let mut recovery_options;
        let options = if !options.retry_uncertain && !unresolved_receipts.is_empty() {
            // Discover unresolved transmissions before force can repeat an
            // earlier completed step. Reuse known evidence while rebuilding
            // the review artifact, including the unresolved receipt history.
            recovery_options = options.clone();
            recovery_options.force = false;
            &recovery_options
        } else {
            options
        };
        if !options.force
            && unresolved_receipts.is_empty()
            && let Some(artifact) = store::load_artifact(&self.connection()?, &asset.id, &identity)?
            && matches!(
                artifact.status,
                MomentStatus::Ready | MomentStatus::NoCandidates
            )
        {
            state.reused_steps += 1;
            store::save_artifact(&self.connection()?, &asset.id, &identity, &artifact)?;
            return Ok(artifact);
        }
        let mut runner = StepRunner {
            catalog: self,
            asset,
            source: &source,
            options,
            provider,
            chain,
            identity: &identity,
            state,
            notify,
            receipts: unresolved_receipts,
            frame_cache: HashMap::new(),
        };
        let mut candidates = Vec::new();
        let mut coverage = Vec::new();
        let mut questions = Vec::new();
        let mut uncertain_windows = Vec::new();
        for (span, times) in coarse_windows(source.duration_ms, options.coarse_frames) {
            let times = source.bound_sample_times(times);
            match runner.step("coarse", span, &times, None, 4) {
                Ok((reply, evidence)) => {
                    coverage.push(CoverageSegment {
                        span,
                        level: CoverageLevel::Sampled,
                        max_gap_ms: sample_gap(span, &times),
                        sample_times_ms: times,
                    });
                    if reply.uncertain {
                        uncertain_windows.push(span);
                    }
                    for proposal in reply.moments {
                        candidates.push(candidate(
                            &asset.id,
                            &source.fingerprint,
                            &proposal,
                            span,
                            false,
                            &evidence,
                            &source.dialogue.iter().map(|x| x.0).collect::<Vec<_>>(),
                        ));
                    }
                }
                Err(error) => {
                    questions.push(format!(
                        "coarse interval {}..{}: {error:#}",
                        span.start_ms, span.end_ms
                    ));
                    coverage.push(CoverageSegment {
                        span,
                        level: CoverageLevel::Uninspected,
                        max_gap_ms: span.duration_ms(),
                        sample_times_ms: vec![],
                    });
                }
            }
        }
        if coverage
            .iter()
            .any(|c| c.level == CoverageLevel::Sampled && c.max_gap_ms > 5000)
        {
            questions.push("coarse sample gaps exceed five seconds; brief events between samples may be unobserved".into());
        }
        let count_before_limit = rank_and_deduplicate(candidates.clone(), usize::MAX).len();
        candidates = rank_and_deduplicate(candidates, options.max_candidates);
        if count_before_limit > candidates.len() {
            questions.push(format!(
                "{} additional candidate detections exceed the selected candidate budget",
                count_before_limit - candidates.len()
            ));
        }
        let mut remaining_frames = options.refinement_frames;
        let mut refined = Vec::new();
        let tasks = candidates
            .into_iter()
            .map(|c| {
                (
                    refinement_span(&c, source.duration_ms, options.profile),
                    Some(c),
                )
            })
            .chain(uncertain_windows.into_iter().map(|span| (span, None)))
            .collect::<Vec<_>>();
        for (span, seed) in tasks {
            let coarse_gap = coverage
                .iter()
                .filter(|c| c.span.overlaps(span))
                .map(|c| c.max_gap_ms)
                .max()
                .unwrap_or(1000);
            let target_gap = (coarse_gap / 2).clamp(1, 500);
            let needed = (span.duration_ms() as usize / target_gap as usize) + 2;
            let count = needed.min(96).min(remaining_frames);
            if count < 2 {
                if let Some(mut candidate) = seed {
                    candidate
                        .uncertainty
                        .push("refinement frame budget exhausted".into());
                    refined.push(candidate);
                }
                questions.push("refinement frame budget exhausted".into());
                continue;
            }
            remaining_frames -= count;
            let times = source.bound_sample_times(spread_samples(span, count));
            let gap = sample_gap(span, &times);
            let temporal_refinement = gap <= 500 && gap < coarse_gap;
            match runner.step(
                "refine",
                span,
                &times,
                seed.as_ref(),
                if seed.is_some() { 1 } else { 4 },
            ) {
                Ok((reply, evidence)) => {
                    coverage.push(CoverageSegment {
                        span,
                        level: if temporal_refinement {
                            CoverageLevel::Refined
                        } else {
                            CoverageLevel::Sampled
                        },
                        sample_times_ms: times,
                        max_gap_ms: gap,
                    });
                    if reply.uncertain {
                        questions.push(reply.reason.clone());
                    }
                    if !temporal_refinement {
                        questions.push("selected refinement budget cannot establish denser sub-second boundaries".into());
                    }
                    for proposal in reply.moments {
                        let mut result = candidate(
                            &asset.id,
                            &source.fingerprint,
                            &proposal,
                            span,
                            temporal_refinement,
                            &evidence,
                            &source.dialogue.iter().map(|x| x.0).collect::<Vec<_>>(),
                        );
                        if let Some(seed) = &seed {
                            result.id = seed.id.clone();
                        }
                        if reply.uncertain {
                            result.uncertainty.push(reply.reason.clone());
                        }
                        refined.push(result);
                    }
                }
                Err(error) => {
                    questions.push(format!(
                        "refinement {}..{}: {error:#}",
                        span.start_ms, span.end_ms
                    ));
                    if let Some(mut candidate) = seed {
                        candidate
                            .uncertainty
                            .push("temporal refinement unavailable".into());
                        refined.push(candidate);
                    }
                }
            }
        }
        let refined_total = rank_and_deduplicate(refined.clone(), usize::MAX).len();
        let candidates = rank_and_deduplicate(refined, options.max_candidates);
        if refined_total > candidates.len() {
            questions.push(format!(
                "{} additional refined moments exceed the candidate budget",
                refined_total - candidates.len()
            ));
        }
        let mut receipt_ids = HashSet::new();
        let receipts = runner
            .receipts
            .into_iter()
            .filter(|receipt| {
                receipt_ids.insert((
                    receipt.run_id.clone(),
                    receipt.step_key.clone(),
                    receipt.generation_id.clone(),
                ))
            })
            .collect::<Vec<_>>();
        if receipts
            .iter()
            .any(|r| !r.usage_complete || r.actual_model.is_none() || r.provider.is_none())
        {
            questions.push(
                "one or more requests lack complete provider-reported provenance or usage".into(),
            );
        }
        if receipts.iter().any(|r| {
            r.actual_model
                .as_deref()
                .is_some_and(|m| m != options.model)
        }) {
            questions.push(
                "reported model differs from the selected model; review the provider receipt"
                    .into(),
            );
        }
        if candidates
            .iter()
            .any(|c| !c.refined || !c.uncertainty.is_empty())
        {
            questions.push("one or more moment boundaries or interpretations need review".into());
        }
        if crate::full_file_blake3(&source.proxy, &|| false)?.hex_digest != source.fingerprint {
            bail!("representation changed during moment inspection; stored steps must be reviewed");
        }
        questions.retain(|q| !q.trim().is_empty());
        questions.sort();
        questions.dedup();
        let models = receipts
            .iter()
            .filter_map(|r| r.actual_model.clone())
            .collect::<BTreeSet<_>>();
        let providers = receipts
            .iter()
            .filter_map(|r| r.provider.clone())
            .collect::<BTreeSet<_>>();
        let status = if !questions.is_empty()
            || coverage
                .iter()
                .any(|c| c.level == CoverageLevel::Uninspected)
        {
            MomentStatus::NeedsReview
        } else if candidates.is_empty() {
            MomentStatus::NoCandidates
        } else {
            MomentStatus::Ready
        };
        // Public evidence identity includes the models/providers actually
        // represented; step/artifact storage lookup uses the base input identity.
        let artifact = MomentArtifact {
            asset_id: asset.id.clone(),
            identity: digest(&(&identity, &models, &providers))?,
            status,
            candidates,
            coverage,
            provenance: MomentProvenance {
                requested_provider: options.provider.clone(),
                payload: options.payload,
                provider: providers.into_iter().collect::<Vec<_>>().join(", "),
                requested_model: options.model.clone(),
                actual_model: models.into_iter().collect::<Vec<_>>().join(", "),
                prompt_version: PROMPT_VERSION.into(),
                schema_version: SCHEMA_VERSION.into(),
                recipe_version: RECIPE_VERSION.into(),
                representation: framepack::REPRESENTATION.into(),
                representation_fingerprint: source.fingerprint,
                time_mapping: TIME_MAPPING.into(),
                source_duration_ms: source.duration_ms,
                transcript_fingerprint: source.transcript_fingerprint,
                observation_fingerprint: source.observation_fingerprint,
                profile: options.profile,
                configuration_hash,
                cost_usd: receipts.iter().filter_map(|r| r.cost_usd).sum(),
                usage_complete: receipts.iter().all(|r| r.usage_complete),
                receipts,
            },
            review_questions: questions,
        };
        store::save_artifact(&self.connection()?, &asset.id, &identity, &artifact)?;
        Ok(artifact)
    }

    fn unavailable_moment_artifact(
        &self,
        asset: &Asset,
        options: &MomentRunOptions,
        reason: String,
    ) -> Result<MomentArtifact> {
        let identity = digest(&(
            "unavailable",
            &asset.id,
            &asset.fingerprint,
            &options.model,
            options.payload,
            options.profile,
        ))?;
        let duration_ms = (asset.duration_seconds.max(0.0) * 1000.0).min(u32::MAX as f64) as u32;
        let artifact = MomentArtifact {
            asset_id: asset.id.clone(),
            identity: identity.clone(),
            status: MomentStatus::Unavailable,
            candidates: vec![],
            coverage: vec![CoverageSegment {
                span: MomentSpan {
                    start_ms: 0,
                    end_ms: duration_ms,
                },
                level: CoverageLevel::Uninspected,
                sample_times_ms: vec![],
                max_gap_ms: duration_ms,
            }],
            provenance: MomentProvenance {
                requested_provider: options.provider.clone(),
                payload: options.payload,
                provider: String::new(),
                requested_model: options.model.clone(),
                actual_model: String::new(),
                prompt_version: PROMPT_VERSION.into(),
                schema_version: SCHEMA_VERSION.into(),
                recipe_version: RECIPE_VERSION.into(),
                representation: framepack::REPRESENTATION.into(),
                representation_fingerprint: String::new(),
                time_mapping: TIME_MAPPING.into(),
                source_duration_ms: duration_ms,
                transcript_fingerprint: None,
                observation_fingerprint: None,
                profile: options.profile,
                configuration_hash: digest(options)?,
                cost_usd: 0.0,
                usage_complete: true,
                receipts: vec![],
            },
            review_questions: vec![reason],
        };
        store::save_artifact(&self.connection()?, &asset.id, &identity, &artifact)?;
        Ok(artifact)
    }

    fn latest_moment_artifact(&self, asset_id: &str) -> Result<Option<MomentArtifact>> {
        if !self.moment_schema_ready()? {
            return Ok(None);
        }
        let body: Option<String> = self.connection()?.query_row("SELECT body_json FROM moment_artifacts WHERE asset_id=?1 ORDER BY created_at DESC,rowid DESC LIMIT 1", [asset_id], |row| row.get(0)).optional()?;
        body.map(|s| serde_json::from_str(&s).context("decode stored moment evidence"))
            .transpose()
    }

    fn unresolved_moment_receipts(
        &self,
        asset_id: &str,
        identity: &str,
    ) -> Result<Vec<MomentReceipt>> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT body_json FROM moment_steps WHERE asset_id=?1 AND identity=?2")?;
        let mut rows = statement.query(params![asset_id, identity])?;
        let mut receipts = Vec::new();
        while let Some(row) = rows.next()? {
            let step: MomentStep = serde_json::from_str(&row.get::<_, String>(0)?)?;
            let cached: CachedStep = serde_json::from_value(step.body)?;
            for mut receipt in cached.prior_receipts.into_iter().chain([cached.receipt]) {
                if !receipt.usage_complete {
                    receipt.reused = true;
                    receipts.push(receipt);
                }
            }
        }
        Ok(receipts)
    }

    fn moment_schema_ready(&self) -> Result<bool> {
        Ok(self.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='moment_artifacts')", [], |row| row.get(0))?)
    }

    fn moment_artifact_current(&self, asset: &Asset, artifact: &MomentArtifact) -> bool {
        if artifact.provenance.recipe_version != RECIPE_VERSION
            || artifact.provenance.prompt_version != PROMPT_VERSION
            || artifact.provenance.schema_version != SCHEMA_VERSION
            || artifact.provenance.time_mapping != TIME_MAPPING
        {
            return false;
        }
        self.moment_source(
            asset,
            artifact.provenance.payload == MomentPayload::FramesAndTranscript,
        )
        .is_ok_and(|source| {
            source.fingerprint == artifact.provenance.representation_fingerprint
                && source.duration_ms == artifact.provenance.source_duration_ms
                && source.transcript_fingerprint == artifact.provenance.transcript_fingerprint
                && source.observation_fingerprint == artifact.provenance.observation_fingerprint
        })
    }

    pub fn moment_evidence_page(
        &self,
        asset_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Option<MomentPage>> {
        require_page(offset, limit)?;
        let asset = self.get_asset(asset_id)?;
        let Some(mut artifact) = self.latest_moment_artifact(asset_id)? else {
            return Ok(None);
        };
        let current = self.moment_artifact_current(&asset, &artifact);
        let total = artifact.candidates.len();
        let total_coverage_segments = artifact.coverage.len();
        let total_receipts = artifact.provenance.receipts.len();
        let total_review_questions = artifact.review_questions.len();
        let page_length = total
            .max(total_coverage_segments)
            .max(total_receipts)
            .max(total_review_questions);
        // The offset applies independently to each evidence collection. Every
        // array is bounded, and next_offset covers even a source with no moments.
        artifact.coverage = artifact
            .coverage
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect();
        artifact.provenance.receipts = artifact
            .provenance
            .receipts
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect();
        artifact.review_questions = artifact
            .review_questions
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect();
        artifact.candidates = artifact
            .candidates
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect();
        Ok(Some(MomentPage {
            schema_version: SCHEMA_VERSION.into(),
            current,
            artifact,
            total_candidates: total,
            total_coverage_segments,
            total_receipts,
            total_review_questions,
            offset,
            next_offset: (offset.saturating_add(limit) < page_length).then_some(offset + limit),
        }))
    }

    pub fn moment_inventory_page(
        &self,
        project_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<MomentInventoryPage> {
        require_page(offset, limit)?;
        self.require_project(project_id)?;
        let schema_ready = self.moment_schema_ready()?;
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT id FROM assets WHERE project_id=?1 ORDER BY id LIMIT ?2 OFFSET ?3")?;
        let ids = statement
            .query_map(
                params![project_id, (limit + 1) as i64, offset as i64],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let has_more = ids.len() > limit;
        let mut items = Vec::new();
        for id in ids.into_iter().take(limit) {
            let asset = self.get_asset(&id)?;
            let artifact = self.latest_moment_artifact(&id)?;
            items.push(MomentInventoryItem {
                asset_id: id,
                current: artifact
                    .as_ref()
                    .is_some_and(|a| self.moment_artifact_current(&asset, a)),
                status: artifact.as_ref().map(|a| a.status),
                candidate_count: artifact.as_ref().map_or(0, |a| a.candidates.len()),
                questions: artifact.map_or_else(
                    || vec![if schema_ready { "moment evidence is missing" } else { "moment evidence schema is unavailable; explicitly open the catalog writable to upgrade before preparation" }.into()],
                    |a| a.review_questions.into_iter().take(8).collect(),
                ),
            });
        }
        Ok(MomentInventoryPage {
            schema_version: SCHEMA_VERSION.into(),
            schema_ready,
            project_id: project_id.into(),
            items,
            offset,
            next_offset: has_more.then_some(offset + limit),
        })
    }
}

struct StepRunner<'a> {
    catalog: &'a Catalog,
    asset: &'a Asset,
    source: &'a SourceContext,
    options: &'a MomentRunOptions,
    provider: &'a dyn VlmProvider,
    chain: &'a ProviderChain,
    identity: &'a str,
    state: &'a mut RunState,
    notify: &'a mut dyn FnMut(&str, f64) -> ControlFlow<()>,
    receipts: Vec<MomentReceipt>,
    frame_cache: HashMap<u32, String>,
}

#[derive(Serialize, Deserialize)]
struct CachedStep {
    #[serde(default)]
    prior_receipts: Vec<MomentReceipt>,
    state: String,
    reply: Option<StepReply>,
    receipt: MomentReceipt,
    error: String,
}

impl StepRunner<'_> {
    fn step(
        &mut self,
        phase: &str,
        span: MomentSpan,
        times: &[u32],
        seed: Option<&MomentCandidate>,
        limit: usize,
    ) -> Result<(StepReply, Vec<MomentEvidenceRef>)> {
        ensure!(
            !times.is_empty()
                && times
                    .iter()
                    .all(|at| *at >= span.start_ms && *at < span.end_ms),
            "no decodable sample times within the inspected interval"
        );
        let step_key = digest(&(phase, span, times, seed.map(|s| &s.id)))?;
        let mut evidence = times
            .iter()
            .map(|at| MomentEvidenceRef {
                kind: "proxy_frame".into(),
                key: format!("frame:{at}"),
                span: Some(MomentSpan {
                    start_ms: *at,
                    end_ms: at.saturating_add(1),
                }),
            })
            .collect::<Vec<_>>();
        let mut speech = Vec::new();
        if self.options.payload == MomentPayload::FramesAndTranscript {
            for (index, (utterance, text)) in self
                .source
                .dialogue
                .iter()
                .enumerate()
                .filter(|(_, (s, _))| s.overlaps(span))
                .take(64)
            {
                let key = format!("utterance:{index}");
                speech.push(json!({"key":key,"span":utterance,"text":text.chars().take(500).collect::<String>()}));
                evidence.push(MomentEvidenceRef {
                    kind: "accepted_transcript".into(),
                    key,
                    span: Some(*utterance),
                });
            }
        }
        if let Some(observation) = self.source.coarse_observation.filter(|s| s.overlaps(span)) {
            evidence.push(MomentEvidenceRef {
                kind: "prior_coarse_observation".into(),
                key: "prior_coarse".into(),
                span: Some(observation),
            });
        }
        let keys = evidence.iter().map(|e| e.key.clone()).collect::<Vec<_>>();
        let mut prior_receipts = Vec::new();
        if let Some(cached) = store::load_step(
            &self.catalog.connection()?,
            &self.asset.id,
            self.identity,
            &step_key,
        )? {
            self.state.reused_steps += 1;
            let mut parsed: CachedStep = serde_json::from_value(cached.body)?;
            prior_receipts = parsed.prior_receipts.clone();
            if prior_receipts.iter().any(|r| !r.usage_complete) {
                self.state.historical_unknown = true;
                if !self.options.retry_uncertain {
                    self.state.unknown_usage = true;
                }
            }
            for mut receipt in prior_receipts.iter().cloned() {
                receipt.reused = true;
                self.receipts.push(receipt);
            }
            if parsed.state == "complete"
                && !parsed.receipt.usage_complete
                && self.options.force
                && !self.options.retry_uncertain
            {
                self.state.unknown_usage = true;
                let mut receipt = parsed.receipt.clone();
                receipt.reused = true;
                self.receipts.push(receipt);
                bail!(
                    "force cannot repeat a request with unknown usage without explicit retry-uncertain authorization"
                );
            }
            if matches!(parsed.state.as_str(), "pending" | "uncertain")
                && !self.options.retry_uncertain
            {
                self.state.unknown_usage = true;
                parsed.receipt.reused = true;
                self.receipts.push(parsed.receipt);
                bail!(
                    "request outcome is uncertain; explicit retry-uncertain authorization required"
                );
            }
            if (matches!(parsed.state.as_str(), "pending" | "uncertain")
                || (parsed.state == "complete"
                    && !parsed.receipt.usage_complete
                    && self.options.force))
                && self.options.retry_uncertain
            {
                ensure!(
                    prior_receipts.len() < 16,
                    "request recovery history is full; reconciliation required"
                );
                self.state.historical_unknown = true;
                prior_receipts.push(parsed.receipt.clone());
                let mut previous = parsed.receipt.clone();
                previous.reused = true;
                self.receipts.push(previous);
            }
            if parsed.state == "complete" && !self.options.force {
                let reply = parsed
                    .reply
                    .ok_or_else(|| anyhow!("cached moment step has no reply"))?;
                let reply = validate_reply(
                    &serde_json::to_string(&reply)?,
                    span,
                    self.options.profile,
                    &keys,
                    limit,
                )?;
                validate_dialogue_evidence(&reply, &evidence)?;
                if !parsed.receipt.usage_complete {
                    self.state.historical_unknown = true;
                    if !self.options.retry_uncertain {
                        self.state.unknown_usage = true;
                    }
                }
                parsed.receipt.reused = true;
                self.receipts.push(parsed.receipt);
                return Ok((reply, evidence));
            }
            if parsed.state == "rejected" && !self.options.force {
                parsed.receipt.reused = true;
                self.receipts.push(parsed.receipt);
                bail!(
                    "cached provider output was rejected; inspect evidence before an explicit force run"
                );
            }
        }
        if (self.notify)(phase, self.state.spent).is_break() {
            self.state.cancelled = true;
        }
        ensure!(
            !self.state.cancelled,
            "moment run cancelled before the next request"
        );
        ensure!(
            !self.state.unknown_usage,
            "provider usage unknown; no further paid requests allowed in this run"
        );
        let current_asset = self.catalog.get_asset(&self.asset.id)?;
        ensure!(
            !current_asset.removed
                && current_asset.project_id == self.asset.project_id
                && current_asset.duration_seconds == self.asset.duration_seconds,
            "source scope or time mapping changed; re-plan moment discovery"
        );
        if self.state.spent >= self.options.budget_usd {
            self.state.budget_stopped = true;
            bail!("authorized moment budget exhausted");
        }
        let parent = self
            .catalog
            .database_path()
            .parent()
            .ok_or_else(|| anyhow!("catalog has no parent"))?;
        let directory = parent
            .join("cache/framepacks")
            .join(&self.asset.project_id)
            .join(&self.asset.id)
            .join(&self.source.fingerprint)
            .join(RECIPE_VERSION);
        let mut frames = Vec::new();
        let mut payload_bytes = 0;
        for &at in times {
            if !self.frame_cache.contains_key(&at) {
                let manifest = framepack::FramePackManifest {
                    recipe_key: RECIPE_VERSION.into(),
                    representation: framepack::REPRESENTATION.into(),
                    source_fingerprint: self.source.fingerprint.clone(),
                    timestamps_ms: vec![at],
                };
                let paths = framepack::encode_frames(&self.source.proxy, &manifest, &directory)?;
                let bytes = fs::read(&paths[0])
                    .with_context(|| format!("read encoded proxy sample at {at}ms"))?;
                ensure!(
                    bytes.starts_with(&[0xff, 0xd8]),
                    "proxy sample at {at}ms is not a JPEG"
                );
                ensure!(
                    bytes.len() <= 512 * 1024,
                    "moment frame exceeds payload limit"
                );
                // Retain only a small working set across requests, even at the
                // maximum authorized per-asset frame budget.
                if self.frame_cache.len() >= 16 {
                    self.frame_cache.clear();
                }
                self.frame_cache.insert(at, frame_data_uri(&bytes));
            }
            payload_bytes += self.frame_cache[&at].len();
            ensure!(
                payload_bytes <= 10 * 1024 * 1024,
                "moment request exceeds authorized payload bound"
            );
            frames.push(self.frame_cache[&at].clone());
        }
        ensure!(
            frames.iter().map(String::len).sum::<usize>() <= 10 * 1024 * 1024,
            "moment request exceeds authorized payload bound"
        );
        ensure!(
            crate::full_file_blake3(&self.source.proxy, &|| false)?.hex_digest
                == self.source.fingerprint,
            "proxy changed before moment request"
        );
        let prompt = format!(
            "Discover reusable within-clip moments from ONLY this explicitly scoped evidence. Return bounded JSON; zero moments is valid. Do not force source inclusion or invent events. Inspect every supplied time, including late action. Nearby views of the same payoff are one event; genuinely separate payoffs remain separate. Identify the actual peak and necessary setup, not the first N seconds. Prefer concise cores using the edit profile, with optional context and occasional justified long holds. Sampled frames are not continuous video; set uncertain=true when temporal boundaries or visual content cannot be established. Preserve each cited utterance entirely in the core or explicit context. Use only supplied evidence keys. Role must be action, dialogue, atmosphere, transition or hold. Core/payoff must fit inside inspectedSpan; payoff must fit inside core. Optional setup precedes payoff; optional context contains core. Keep explanations short. Refine-phase evidence is denser than coarse evidence; check setup/payoff, dialogue boundaries and whether the original candidate actually exists. Preserve useful long event context while keeping the selected core concise.\nEVIDENCE={}\n",
            json!({"phase":phase,"inspectedSpan":span,"sourceDurationMs":self.source.duration_ms,"frameTimesMs":times,"evidence":evidence,"acceptedTimedSpeech":speech,"priorCoarseRange":self.source.coarse_observation,"candidateToRefine":seed,"editProfile":self.options.profile})
        );
        let request = VlmRequest {
            prompt,
            frames,
            schema: output_schema(limit, self.source.duration_ms, &keys),
        };
        if let Some(policy) = &self.state.approved_policy {
            self.catalog
                .validate_run_policy_asset(policy, &self.asset.id)?;
        }
        self.provider.prepare()?;
        let pending_receipt = MomentReceipt {
            run_id: self.state.run_id.clone(),
            step_key: step_key.clone(),
            actual_model: None,
            provider: None,
            generation_id: None,
            cost_usd: None,
            usage_complete: false,
            reused: false,
        };
        self.save(
            &step_key,
            CachedStep {
                prior_receipts: prior_receipts.clone(),
                state: "pending".into(),
                reply: None,
                receipt: pending_receipt.clone(),
                error: String::new(),
            },
        )?;
        self.state.executed_steps += 1;
        let outcome = match self.chain.analyze(self.provider, &request) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.state.unknown_usage = true;
                self.save(
                    &step_key,
                    CachedStep {
                        prior_receipts: prior_receipts.clone(),
                        state: "uncertain".into(),
                        reply: None,
                        receipt: pending_receipt.clone(),
                        error: format!("{error:#}"),
                    },
                )?;
                self.receipts.push(pending_receipt);
                return Err(error);
            }
        };
        let response = outcome.response;
        let complete = response.receipt.usage_complete
            && response.cost_usd.is_finite()
            && response.cost_usd >= 0.0;
        let receipt = MomentReceipt {
            run_id: self.state.run_id.clone(),
            step_key: step_key.clone(),
            actual_model: response.receipt.actual_model,
            provider: response.receipt.provider,
            generation_id: response.receipt.generation_id,
            cost_usd: complete.then_some(response.cost_usd),
            usage_complete: complete,
            reused: false,
        };
        if complete {
            self.state.spent += response.cost_usd;
        } else {
            self.state.unknown_usage = true;
        }
        self.receipts.push(receipt.clone());
        let validated = validate_reply(&response.body, span, self.options.profile, &keys, limit)
            .and_then(|reply| {
                validate_dialogue_evidence(&reply, &evidence)?;
                Ok(reply)
            });
        match validated {
            Ok(reply) => {
                self.save(
                    &step_key,
                    CachedStep {
                        prior_receipts: prior_receipts.clone(),
                        state: "complete".into(),
                        reply: Some(reply.clone()),
                        receipt,
                        error: String::new(),
                    },
                )?;
                Ok((reply, evidence))
            }
            Err(error) => {
                self.save(
                    &step_key,
                    CachedStep {
                        prior_receipts: prior_receipts.clone(),
                        state: "rejected".into(),
                        reply: None,
                        receipt,
                        error: format!("{error:#}"),
                    },
                )?;
                Err(error)
            }
        }
    }

    fn save(&self, step_key: &str, cached: CachedStep) -> Result<()> {
        let mut connection = self.catalog.connection()?;
        let transaction = connection.transaction()?;
        store::save_step(
            &transaction,
            &MomentStep {
                asset_id: self.asset.id.clone(),
                identity: self.identity.into(),
                step_key: step_key.into(),
                actual_model: cached.receipt.actual_model.clone().unwrap_or_default(),
                cost_usd: cached.receipt.cost_usd.unwrap_or(0.0),
                usage_complete: cached.receipt.usage_complete,
                body: serde_json::to_value(cached)?,
            },
        )?;
        transaction.execute(
            "UPDATE analysis_runs SET cost_usd=?2,error=?3 WHERE id=?1",
            params![
                self.state.run_id,
                self.state.spent,
                if self.state.unknown_usage {
                    "moment provider usage is unknown"
                } else {
                    ""
                }
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::provider::{DEFAULT_MODEL, FALLBACK_MODEL, VlmReceipt};
    use crate::analysis::quality::DecodedSegment;
    use crate::analysis::store::{StoredTranscript, save_transcript};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

    struct EmptyMomentProvider {
        calls: AtomicUsize,
    }

    impl VlmProvider for EmptyMomentProvider {
        fn analyze(&self, model: &str, request: &VlmRequest) -> Result<VlmResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let evidence: serde_json::Value = request
                .prompt
                .split_once("EVIDENCE=")
                .and_then(|(_, json)| serde_json::from_str(json).ok())
                .context("fake provider did not receive structured evidence")?;
            ensure!(
                evidence["frameTimesMs"]
                    .as_array()
                    .is_some_and(|times| !times.is_empty()),
                "fake provider expected offered frame times"
            );
            ensure!(
                request.frames.len() == evidence["frameTimesMs"].as_array().unwrap().len()
                    && request
                        .frames
                        .iter()
                        .all(|frame| frame.starts_with("data:image/jpeg;base64,")),
                "fake provider expected a JPEG payload for each offered time"
            );
            Ok(VlmResponse {
                model: model.to_owned(),
                body: r#"{"moments":[],"uncertain":false,"reason":"synthetic quiet clip"}"#.into(),
                prompt_tokens: 4,
                completion_tokens: 4,
                cost_usd: 0.0,
                latency_ms: 1,
                receipt: VlmReceipt {
                    actual_model: Some(model.to_owned()),
                    provider: Some("openrouter".into()),
                    generation_id: Some(format!("fake-{}", self.calls.load(Ordering::SeqCst))),
                    usage_complete: true,
                },
            })
        }
    }

    struct EventProvider {
        calls: AtomicUsize,
        centers_ms: Vec<u32>,
    }

    impl VlmProvider for EventProvider {
        fn analyze(&self, model: &str, request: &VlmRequest) -> Result<VlmResponse> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            let input: serde_json::Value = request
                .prompt
                .split_once("EVIDENCE=")
                .and_then(|(_, json)| serde_json::from_str(json).ok())
                .context("fake event provider did not receive structured evidence")?;
            let times = input["frameTimesMs"]
                .as_array()
                .context("frame times missing")?;
            ensure!(
                request.frames.len() == times.len()
                    && request
                        .frames
                        .iter()
                        .all(|frame| frame.starts_with("data:image/jpeg;base64,")),
                "each supplied sample must have its proxy JPEG payload"
            );
            let start = input["inspectedSpan"]["startMs"].as_u64().unwrap() as u32;
            let end = input["inspectedSpan"]["endMs"].as_u64().unwrap() as u32;
            let phase = input["phase"].as_str().unwrap();
            let centers = if phase == "refine" {
                let seed_event = input["candidateToRefine"]["event"].as_str();
                self.centers_ms
                    .iter()
                    .copied()
                    .filter(|center| {
                        seed_event.is_some_and(|event| event == format!("event at {center}ms"))
                    })
                    .collect::<Vec<_>>()
            } else {
                self.centers_ms
                    .iter()
                    .copied()
                    .filter(|center| start <= *center && *center < end)
                    .collect()
            };
            let mut moments = Vec::new();
            for center in centers {
                let core_start = center.saturating_sub(2_000).max(start);
                let core_end = center.saturating_add(2_000).min(end);
                ensure!(
                    core_end.saturating_sub(core_start) >= 3_000,
                    "fixture event span too short"
                );
                let payoff_start = center.saturating_sub(500).max(core_start);
                let payoff_end = center.saturating_add(500).min(core_end);
                let nearest = times
                    .iter()
                    .filter_map(|time| time.as_u64())
                    .min_by_key(|at| at.abs_diff(u64::from(center)))
                    .context("event request has no frame samples")?;
                moments.push(json!({
                    "event": format!("event at {center}ms"), "role":"action", "reason":"synthetic event at a sampled time",
                    "score":0.9, "coreStartMs":core_start, "coreEndMs":core_end,
                    "payoffStartMs":payoff_start, "payoffEndMs":payoff_end,
                    "setup":null, "context":null, "longHold":false,
                    "evidenceKeys":[format!("frame:{nearest}")], "uncertainty":[]
                }));
            }
            let body =
                json!({"moments":moments,"uncertain":false,"reason":"synthetic event fixture"})
                    .to_string();
            Ok(VlmResponse {
                model: model.to_owned(),
                body,
                prompt_tokens: 4,
                completion_tokens: 4,
                cost_usd: 0.0,
                latency_ms: 1,
                receipt: VlmReceipt {
                    actual_model: Some(model.to_owned()),
                    provider: Some("openrouter".into()),
                    generation_id: Some(format!("event-{call}")),
                    usage_complete: true,
                },
            })
        }
    }

    struct LongHoldProvider;

    impl VlmProvider for LongHoldProvider {
        fn analyze(&self, model: &str, request: &VlmRequest) -> Result<VlmResponse> {
            let input: serde_json::Value = request
                .prompt
                .split_once("EVIDENCE=")
                .and_then(|(_, json)| serde_json::from_str(json).ok())
                .context("fake hold provider did not receive structured evidence")?;
            let times = input["frameTimesMs"]
                .as_array()
                .context("frame times missing")?;
            ensure!(
                request.frames.len() == times.len(),
                "frame payload count mismatch"
            );
            let start = input["inspectedSpan"]["startMs"].as_u64().unwrap() as u32;
            let end = input["inspectedSpan"]["endMs"].as_u64().unwrap() as u32;
            let center = (u64::from(start) + u64::from(end)) / 2;
            let nearest = times
                .iter()
                .filter_map(|time| time.as_u64())
                .min_by_key(|at| at.abs_diff(center))
                .context("hold request has no samples")?;
            let body = json!({"moments":[{
                "event":"continuous scenic hold", "role":"hold", "reason":"the full sustained scene is useful",
                "score":0.8, "coreStartMs":start, "coreEndMs":end,
                "payoffStartMs":center.saturating_sub(500), "payoffEndMs":center.saturating_add(500),
                "setup":null, "context":null, "longHold":true,
                "evidenceKeys":[format!("frame:{nearest}")], "uncertainty":[]
            }],"uncertain":false,"reason":"synthetic hold fixture"}).to_string();
            Ok(VlmResponse {
                model: model.into(),
                body,
                prompt_tokens: 2,
                completion_tokens: 2,
                cost_usd: 0.0,
                latency_ms: 1,
                receipt: VlmReceipt {
                    actual_model: Some(model.into()),
                    provider: Some("openrouter".into()),
                    generation_id: Some(format!("hold-{center}")),
                    usage_complete: true,
                },
            })
        }
    }

    struct UncertainProvider {
        calls: AtomicUsize,
    }

    struct FailAtProvider {
        calls: AtomicUsize,
        fail_at: usize,
        events: EventProvider,
    }

    impl VlmProvider for FailAtProvider {
        fn analyze(&self, model: &str, request: &VlmRequest) -> Result<VlmResponse> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if call == self.fail_at {
                anyhow::bail!("synthetic transport interruption after request submission");
            }
            self.events.analyze(model, request)
        }
    }

    struct CostProvider {
        calls: AtomicUsize,
        cost_usd: f64,
    }

    impl VlmProvider for CostProvider {
        fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(VlmResponse {
                model: model.into(),
                body: r#"{"moments":[],"uncertain":false,"reason":"quiet synthetic sample"}"#
                    .into(),
                prompt_tokens: 10,
                completion_tokens: 5,
                cost_usd: self.cost_usd,
                latency_ms: 10,
                receipt: VlmReceipt {
                    actual_model: Some(model.into()),
                    provider: Some("openrouter".into()),
                    generation_id: Some(format!("cost-{call}")),
                    usage_complete: true,
                },
            })
        }
    }

    struct ReportedIdentityProvider {
        calls: AtomicUsize,
        actual_model: Option<String>,
        provider: Option<String>,
    }

    impl VlmProvider for ReportedIdentityProvider {
        fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(VlmResponse {
                model: model.into(),
                body: r#"{"moments":[],"uncertain":false,"reason":"quiet synthetic sample"}"#
                    .into(),
                prompt_tokens: 1,
                completion_tokens: 1,
                cost_usd: 0.0,
                latency_ms: 1,
                receipt: VlmReceipt {
                    actual_model: self.actual_model.clone(),
                    provider: self.provider.clone(),
                    generation_id: Some("reported-identity".into()),
                    usage_complete: true,
                },
            })
        }
    }

    struct DialogueProvider;

    impl VlmProvider for DialogueProvider {
        fn analyze(&self, model: &str, request: &VlmRequest) -> Result<VlmResponse> {
            let input: serde_json::Value = request
                .prompt
                .split_once("EVIDENCE=")
                .and_then(|(_, json)| serde_json::from_str(json).ok())
                .context("fake dialogue provider did not receive structured evidence")?;
            let times = input["frameTimesMs"]
                .as_array()
                .context("frame times missing")?;
            ensure!(
                request.frames.len() == times.len(),
                "frame payload count mismatch"
            );
            let start = input["inspectedSpan"]["startMs"].as_u64().unwrap() as u32;
            let end = input["inspectedSpan"]["endMs"].as_u64().unwrap() as u32;
            let speech = input["acceptedTimedSpeech"]
                .as_array()
                .context("timed speech missing")?;
            let mut moments = Vec::new();
            for utterance in speech {
                let key = utterance["key"]
                    .as_str()
                    .context("utterance evidence key missing")?;
                let utterance_start = utterance["span"]["startMs"].as_u64().unwrap() as u32;
                let utterance_end = utterance["span"]["endMs"].as_u64().unwrap() as u32;
                let core_start = utterance_start.saturating_sub(1_200).max(start);
                let core_end = utterance_end.saturating_add(1_200).min(end);
                if core_end.saturating_sub(core_start) < 3_000 {
                    continue;
                }
                let midpoint = (u64::from(utterance_start) + u64::from(utterance_end)) / 2;
                let nearest = times
                    .iter()
                    .filter_map(|time| time.as_u64())
                    .min_by_key(|at| at.abs_diff(midpoint))
                    .context("dialogue request has no frames")?;
                moments.push(json!({
                    "event":format!("timed utterance {key}"), "role":"dialogue", "reason":"accepted speech aligns with the sampled scene",
                    "score":0.8, "coreStartMs":core_start, "coreEndMs":core_end,
                    "payoffStartMs":utterance_start, "payoffEndMs":utterance_end,
                    "setup":null, "context":null, "longHold":false,
                    "evidenceKeys":[format!("frame:{nearest}"),key], "uncertainty":[]
                }));
            }
            Ok(VlmResponse {
                model: model.into(),
                body:
                    json!({"moments":moments,"uncertain":false,"reason":"synthetic timed dialogue"})
                        .to_string(),
                prompt_tokens: 2,
                completion_tokens: 2,
                cost_usd: 0.0,
                latency_ms: 1,
                receipt: VlmReceipt {
                    actual_model: Some(model.into()),
                    provider: Some("openrouter".into()),
                    generation_id: Some("dialogue-fixture".into()),
                    usage_complete: true,
                },
            })
        }
    }

    impl VlmProvider for UncertainProvider {
        fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(VlmResponse {
                model: model.into(),
                body: r#"{"moments":[],"uncertain":true,"reason":"repetitive synthetic views do not establish an event"}"#.into(),
                prompt_tokens: 2,
                completion_tokens: 2,
                cost_usd: 0.0,
                latency_ms: 1,
                receipt: VlmReceipt {
                    actual_model: Some(model.into()),
                    provider: Some("openrouter".into()),
                    generation_id: Some(format!("uncertain-{call}")),
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
    ) -> PathBuf {
        let source_dir = root.join("source");
        std::fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join(format!("{id}.MP4"));
        let mut command = crate::tool_command("ffmpeg");
        command
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
            ])
            .arg(if id == "repetitive" {
                format!("testsrc2=s=640x360:r=24:d={seconds}")
            } else {
                format!(
                    "color=c=0x{}:s=640x360:r=24:d={seconds}",
                    &blake3::hash(id.as_bytes()).to_hex()[..6]
                )
            });
        if id == "dialogue" {
            command
                .args(["-f", "lavfi", "-i"])
                .arg(format!(
                    "sine=frequency=440:sample_rate=48000:duration={seconds},volume=0:enable='not(between(t,3.4,4.4)+between(t,16.4,17.4)+between(t,21.9,22.9))'"
                ))
                .args(["-map", "0:v:0", "-map", "1:a:0", "-c:a", "aac"]);
        } else {
            command.arg("-an");
        }
        command.args(["-c:v", "libx264", "-pix_fmt", "yuv420p"]);
        if id == "events" {
            command.args([
                "-vf",
                "drawbox=x='100+50*sin(100*t)':y=90:w=120:h=120:color=red:t=fill:enable='between(t,4.8,5.8)',drawbox=x='350+40*sin(80*t)':y=150:w=140:h=90:color=blue:t=fill:enable='between(t,21.6,22.6)'",
            ]);
        } else {
            command.args(["-vf", "format=yuv420p"]);
        }
        let generated = command.arg(&source).status().unwrap();
        assert!(
            generated.success(),
            "synthetic H.264 source creation failed"
        );
        crate::tests::insert_asset_at(catalog, project_id, id, &source);
        catalog
            .connection()
            .unwrap()
            .execute(
                "UPDATE assets SET duration_seconds=?2,source_out=?2 WHERE id=?1",
                params![id, seconds],
            )
            .unwrap();
        catalog.validate_asset_video(id).unwrap();
        catalog.generate_proxy(id).unwrap();
        source
    }

    fn event_options() -> MomentRunOptions {
        MomentRunOptions {
            model: DEFAULT_MODEL.into(),
            provider: "openrouter".into(),
            payload: MomentPayload::Frames,
            budget_usd: 2.0,
            coarse_frames: 96,
            refinement_frames: 384,
            max_candidates: 8,
            profile: EditProfile::default(),
            force: false,
            retry_uncertain: false,
            allowed_models: vec![DEFAULT_MODEL.to_string()],
            allow_fallback: false,
            policy_id: None,
        }
    }

    fn human_asset_snapshot(
        catalog: &Catalog,
        asset_id: &str,
    ) -> (
        String,
        String,
        String,
        String,
        i64,
        i64,
        Option<String>,
        i64,
    ) {
        catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT notes,title,description,labels_json,removed,rotation_degrees,best_time,offset_seconds FROM assets WHERE id=?1",
                [asset_id],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?)),
            )
            .unwrap()
    }

    #[test]
    fn quiet_opening_late_action_and_disjoint_payoffs_are_refined_from_full_span_samples() {
        if !tool_available("ffmpeg") || !tool_available("ffprobe") {
            eprintln!("skipping generated moment event fixture: ffmpeg/ffprobe unavailable");
            return;
        }
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog
            .create_project("Synthetic late and separate events")
            .unwrap();
        let source = generated_asset(&catalog, &project.id, directory.path(), "events", 24);
        let original_bytes = std::fs::read(&source).unwrap();
        {
            let connection = catalog.connection().unwrap();
            connection.execute(
                "UPDATE assets SET notes='human note',title='human title',description='human description',labels_json='[\"human label\"]',removed=0,rotation_degrees=270,best_time='2025-12-07T08:09:10Z',offset_seconds=37 WHERE id='events'",
                [],
            ).unwrap();
            connection.execute(
                "INSERT INTO analysis_labels(asset_id,normalized_label,display_label,proposed_label,created_at) VALUES ('events','synthetic','Synthetic','Synthetic','now')",
                [],
            ).unwrap();
            connection.execute(
                "INSERT INTO capture_sequences(id,project_id,camera_key,label,created_at) VALUES ('sequence',?1,'camera','trip','now')",
                params![project.id],
            ).unwrap();
            connection.execute(
                "INSERT INTO asset_capture_sequence(asset_id,project_id,capture_sequence_id,filename_counter) VALUES ('events',?1,'sequence',1)",
                params![project.id],
            ).unwrap();
        }
        let human_fields_before = human_asset_snapshot(&catalog, "events");
        let provider = EventProvider {
            calls: AtomicUsize::new(0),
            centers_ms: vec![5_000, 22_000],
        };
        let options = event_options();
        let report = catalog
            .discover_moments_with_provider(
                &project.id,
                &["events".into()],
                &options,
                &provider,
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        let page = catalog
            .moment_evidence_page("events", 0, 50)
            .unwrap()
            .unwrap();
        assert_eq!(
            report.processed,
            1,
            "discovery report: {}",
            serde_json::to_string(&report).unwrap()
        );
        assert_eq!(
            page.artifact.status,
            MomentStatus::Ready,
            "review questions: {:?}",
            page.artifact.review_questions
        );
        assert_eq!(
            page.artifact.candidates.len(),
            2,
            "independent payoffs must survive deduplication"
        );
        assert!(
            page.artifact
                .candidates
                .iter()
                .any(|candidate| candidate.payoff.start_ms > 19_000),
            "late action was omitted"
        );
        assert!(
            page.artifact
                .candidates
                .iter()
                .all(|candidate| candidate.refined),
            "candidates must use truthful dense refinement"
        );
        assert!(
            page.artifact
                .coverage
                .iter()
                .any(|segment| segment.span.start_ms > 15_000)
        );
        let limited = catalog
            .moment_evidence_page("events", 0, 1)
            .unwrap()
            .unwrap();
        assert!(limited.artifact.candidates.len() <= 1);
        assert!(limited.artifact.coverage.len() <= 1);
        assert!(limited.artifact.provenance.receipts.len() <= 1);
        assert!(limited.artifact.review_questions.len() <= 1);
        assert!(limited.total_coverage_segments > limited.artifact.coverage.len());
        assert_eq!(limited.total_candidates, 2);
        assert_eq!(
            limited.total_receipts,
            page.artifact.provenance.receipts.len()
        );
        let second_page = catalog
            .moment_evidence_page("events", 1, 1)
            .unwrap()
            .unwrap();
        assert_eq!(second_page.offset, 1);
        assert_eq!(second_page.artifact.candidates.len(), 1);
        assert_eq!(second_page.artifact.coverage.len(), 1);
        assert_eq!(second_page.artifact.provenance.receipts.len(), 1);
        assert_eq!(second_page.next_offset, Some(2));
        for (offset, limit) in [(0, 0), (0, 51), (1_000_001, 1)] {
            assert!(
                catalog
                    .moment_evidence_page("events", offset, limit)
                    .is_err()
            );
            assert!(
                catalog
                    .moment_inventory_page(&project.id, offset, limit)
                    .is_err()
            );
        }
        assert_eq!(limited.next_offset, Some(1));
        let connection = catalog.connection().unwrap();
        let label: String = connection
            .query_row(
                "SELECT display_label FROM analysis_labels WHERE asset_id='events'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let sequence: String = connection
            .query_row(
                "SELECT capture_sequence_id FROM asset_capture_sequence WHERE asset_id='events'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            label, "Synthetic",
            "moment discovery must not mutate human/analysis labels"
        );
        assert_eq!(
            sequence, "sequence",
            "moment discovery must not mutate chronology facts"
        );
        assert_eq!(
            human_asset_snapshot(&catalog, "events"),
            human_fields_before,
            "discovery must preserve human annotations and timing/rotation overrides"
        );
        assert_eq!(
            std::fs::read(&source).unwrap(),
            original_bytes,
            "discovery must not alter immutable source bytes"
        );
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            6,
            "four coarse windows and two event refinements expected"
        );
        let ids = page
            .artifact
            .candidates
            .iter()
            .map(|candidate| candidate.id.clone())
            .collect::<Vec<_>>();
        let samples_before_reuse: i64 = catalog.connection().unwrap().query_row(
            "SELECT COALESCE(SUM(samples),0) FROM batch_performance WHERE task_kind LIKE 'moments.%'", [], |row| row.get(0),
        ).unwrap();
        let cached = catalog
            .discover_moments_with_provider(
                &project.id,
                &["events".into()],
                &options,
                &provider,
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        let cached_page = catalog
            .moment_evidence_page("events", 0, 50)
            .unwrap()
            .unwrap();
        assert_eq!(cached.no_candidates, 0);
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            6,
            "cached event evidence made another provider call"
        );
        assert_eq!(
            cached_page
                .artifact
                .candidates
                .iter()
                .map(|candidate| candidate.id.clone())
                .collect::<Vec<_>>(),
            ids
        );
        let samples_after_reuse: i64 = catalog.connection().unwrap().query_row(
            "SELECT COALESCE(SUM(samples),0) FROM batch_performance WHERE task_kind LIKE 'moments.%'", [], |row| row.get(0),
        ).unwrap();
        assert_eq!(
            samples_after_reuse, samples_before_reuse,
            "cache-only work must not train throughput"
        );
        assert_eq!(
            human_asset_snapshot(&catalog, "events"),
            human_fields_before,
            "cache reuse must preserve human annotations and overrides"
        );
        assert_eq!(
            std::fs::read(&source).unwrap(),
            original_bytes,
            "cache reuse must leave source bytes unchanged"
        );
    }

    #[test]
    fn justified_long_hold_retains_a_twenty_four_second_core_through_refinement() {
        if !tool_available("ffmpeg") || !tool_available("ffprobe") {
            eprintln!("skipping generated long-hold fixture: ffmpeg/ffprobe unavailable");
            return;
        }
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Synthetic long hold").unwrap();
        generated_asset(&catalog, &project.id, directory.path(), "hold", 24);
        let provider = LongHoldProvider;
        let mut options = event_options();
        options.coarse_frames = 8;
        let report = catalog
            .discover_moments_with_provider(
                &project.id,
                &["hold".into()],
                &options,
                &provider,
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        let page = catalog
            .moment_evidence_page("hold", 0, 50)
            .unwrap()
            .unwrap();
        assert_eq!(report.processed, 1);
        assert_eq!(
            page.artifact.status,
            MomentStatus::Ready,
            "review questions: {:?}",
            page.artifact.review_questions
        );
        let candidate = &page.artifact.candidates[0];
        assert!(candidate.long_hold);
        assert_eq!(
            candidate.core.duration_ms(),
            24_000,
            "the justified hold must not be shortened to a select"
        );
        assert!(candidate.refined);
        assert!(page.artifact.coverage.iter().any(|segment| segment.level == CoverageLevel::Refined && segment.max_gap_ms <= 500));
    }

    #[test]
    fn short_quiet_clip_is_a_valid_zero_candidate_result() {
        if !tool_available("ffmpeg") || !tool_available("ffprobe") {
            eprintln!("skipping generated short-source fixture: ffmpeg/ffprobe unavailable");
            return;
        }
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Synthetic short source").unwrap();
        generated_asset(&catalog, &project.id, directory.path(), "short", 2);
        let provider = EmptyMomentProvider {
            calls: AtomicUsize::new(0),
        };
        let mut options = event_options();
        options.coarse_frames = 8;
        options.refinement_frames = 8;
        let report = catalog
            .discover_moments_with_provider(
                &project.id,
                &["short".into()],
                &options,
                &provider,
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        let page = catalog
            .moment_evidence_page("short", 0, 50)
            .unwrap()
            .unwrap();
        assert_eq!(
            report.no_candidates, 1,
            "quiet concise source should be a valid zero result"
        );
        assert_eq!(page.artifact.status, MomentStatus::NoCandidates);
        assert!(page.artifact.candidates.is_empty());
        assert!(page.artifact.provenance.source_duration_ms <= 2_000);
    }

    #[test]
    fn repetitive_uncertain_views_remain_needs_review_with_reason() {
        if !tool_available("ffmpeg") || !tool_available("ffprobe") {
            eprintln!("skipping generated uncertain-view fixture: ffmpeg/ffprobe unavailable");
            return;
        }
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog
            .create_project("Synthetic repetitive views")
            .unwrap();
        generated_asset(&catalog, &project.id, directory.path(), "repetitive", 10);
        let provider = UncertainProvider {
            calls: AtomicUsize::new(0),
        };
        let mut options = event_options();
        options.coarse_frames = 8;
        let report = catalog
            .discover_moments_with_provider(
                &project.id,
                &["repetitive".into()],
                &options,
                &provider,
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        let page = catalog
            .moment_evidence_page("repetitive", 0, 50)
            .unwrap()
            .unwrap();
        assert_eq!(report.needs_review, 1);
        assert_eq!(page.artifact.status, MomentStatus::NeedsReview);
        assert!(page.artifact.candidates.is_empty());
        assert!(
            page.artifact
                .review_questions
                .iter()
                .any(|question| question.contains("repetitive synthetic views"))
        );
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            2,
            "uncertainty should trigger one bounded refinement request"
        );
    }

    #[test]
    fn budget_ceiling_stops_before_the_next_provider_request() {
        if !tool_available("ffmpeg") || !tool_available("ffprobe") {
            return;
        }
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Synthetic budget ceiling").unwrap();
        generated_asset(&catalog, &project.id, directory.path(), "budget", 24);
        let provider = CostProvider {
            calls: AtomicUsize::new(0),
            cost_usd: 0.02,
        };
        let mut options = event_options();
        options.budget_usd = 0.005;
        let report = catalog
            .discover_moments_with_provider(
                &project.id,
                &["budget".into()],
                &options,
                &provider,
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        assert!(report.budget_exhausted);
        assert_eq!(
            report.cost_usd, 0.02,
            "complete reported spend stays visible even above the ceiling"
        );
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "the next request must not start after the ceiling is reached"
        );

        let samples_before_resume: i64 = catalog.connection().unwrap().query_row(
            "SELECT COALESCE(SUM(samples),0) FROM batch_performance WHERE task_kind LIKE 'moments.%'", [], |row| row.get(0),
        ).unwrap();
        options.budget_usd = 0.1;
        let resumed = catalog
            .discover_moments_with_provider(
                &project.id,
                &["budget".into()],
                &options,
                &provider,
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            4,
            "resume should reuse the first completed window and request only three missing windows"
        );
        assert_eq!(resumed.cost_usd, 0.06, "report only newly incurred spend");
        let page = catalog
            .moment_evidence_page("budget", 0, 50)
            .unwrap()
            .unwrap();
        assert_eq!(
            page.artifact.provenance.cost_usd, 0.08,
            "provenance retains the first run's 0.02 plus the resumed 0.06"
        );
        let samples_after_resume: i64 = catalog.connection().unwrap().query_row(
            "SELECT COALESCE(SUM(samples),0) FROM batch_performance WHERE task_kind LIKE 'moments.%'", [], |row| row.get(0),
        ).unwrap();
        assert_eq!(
            samples_after_resume, samples_before_resume,
            "partial-cache reuse must not train throughput"
        );
    }

    #[test]
    fn cancellation_before_first_request_makes_no_provider_call() {
        if !tool_available("ffmpeg") || !tool_available("ffprobe") {
            return;
        }
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Synthetic cancellation").unwrap();
        generated_asset(&catalog, &project.id, directory.path(), "cancel", 2);
        let provider = EmptyMomentProvider {
            calls: AtomicUsize::new(0),
        };
        let options = event_options();
        let report = catalog
            .discover_moments_with_provider(
                &project.id,
                &["cancel".into()],
                &options,
                &provider,
                |_| ControlFlow::Break(()),
            )
            .unwrap();
        assert!(report.cancelled);
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            0,
            "cancellation must be observed before the paid call"
        );
    }

    #[test]
    fn busy_proxy_claim_skips_and_preflight_removal_is_refreshed_before_paid_work() {
        if !tool_available("ffmpeg") || !tool_available("ffprobe") {
            return;
        }
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog
            .create_project("Synthetic busy representation")
            .unwrap();
        generated_asset(&catalog, &project.id, directory.path(), "busy", 2);
        let provider = EmptyMomentProvider {
            calls: AtomicUsize::new(0),
        };
        let options = event_options();
        let proxy_claim = catalog
            .claim_representation("busy", RepresentationKind::Proxy)
            .unwrap();
        let busy = catalog
            .discover_moments_with_provider(
                &project.id,
                &["busy".into()],
                &options,
                &provider,
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        assert_eq!(busy.skipped, 1);
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            0,
            "a busy proxy representation must skip before any paid request"
        );
        drop(proxy_claim);

        let mut changed = false;
        let removed = catalog
            .discover_moments_with_provider(
                &project.id,
                &["busy".into()],
                &options,
                &provider,
                |_| {
                    if !changed {
                        catalog
                            .connection()
                            .unwrap()
                            .execute("UPDATE assets SET removed=1 WHERE id='busy'", [])
                            .unwrap();
                        changed = true;
                    }
                    ControlFlow::Continue(())
                },
            )
            .unwrap();
        assert!(changed);
        assert_eq!(
            removed.needs_review, 1,
            "the asset changed during preflight and must be reread after representation claims"
        );
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            0,
            "a newly removed asset must never reach the provider"
        );
        let page = catalog
            .moment_evidence_page("busy", 0, 50)
            .unwrap()
            .unwrap();
        assert_eq!(page.artifact.status, MomentStatus::Unavailable);
    }

    #[test]
    fn cancellation_after_first_request_resumes_only_missing_windows() {
        if !tool_available("ffmpeg") || !tool_available("ffprobe") {
            return;
        }
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog
            .create_project("Synthetic mid-run cancellation")
            .unwrap();
        generated_asset(
            &catalog,
            &project.id,
            directory.path(),
            "cancel_after_first",
            24,
        );
        let provider = EmptyMomentProvider {
            calls: AtomicUsize::new(0),
        };
        let options = event_options();
        let mut phase_callbacks = 0;
        let cancelled = catalog
            .discover_moments_with_provider(
                &project.id,
                &["cancel_after_first".into()],
                &options,
                &provider,
                |_| {
                    phase_callbacks += 1;
                    if phase_callbacks == 3 {
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                },
            )
            .unwrap();
        assert!(cancelled.cancelled);
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "cancellation at the second request boundary follows one completed request"
        );
        let resumed = catalog
            .discover_moments_with_provider(
                &project.id,
                &["cancel_after_first".into()],
                &options,
                &provider,
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            4,
            "resume reuses the first coarse window and requests only the three missing windows"
        );
        assert_eq!(resumed.no_candidates, 1);
    }

    #[test]
    fn orphaned_pending_receipt_blocks_force_and_explicit_retry() {
        if !tool_available("ffmpeg") || !tool_available("ffprobe") {
            return;
        }
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog
            .create_project("Synthetic orphaned receipt")
            .unwrap();
        generated_asset(&catalog, &project.id, directory.path(), "orphaned", 2);
        let provider = EmptyMomentProvider {
            calls: AtomicUsize::new(0),
        };
        let options = event_options();
        catalog
            .discover_moments_with_provider(
                &project.id,
                &["orphaned".into()],
                &options,
                &provider,
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        // Resume rows use the internal base identity from moment_steps. The
        // public artifact identity also includes reported provider/model data.
        let identity: String = catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT identity FROM moment_steps WHERE asset_id='orphaned' LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let pending = MomentReceipt {
            run_id: "crashed-run".into(),
            step_key: "orphaned-pending-step".into(),
            actual_model: None,
            provider: None,
            generation_id: None,
            cost_usd: None,
            usage_complete: false,
            reused: false,
        };
        store::save_step(
            &catalog.connection().unwrap(),
            &MomentStep {
                asset_id: "orphaned".into(),
                identity: identity.clone(),
                step_key: "orphaned-pending-step".into(),
                actual_model: options.model.clone(),
                body: serde_json::to_value(CachedStep {
                    prior_receipts: vec![],
                    state: "pending".into(),
                    reply: None,
                    receipt: pending,
                    error: String::new(),
                })
                .unwrap(),
                cost_usd: 0.0,
                usage_complete: false,
            },
        )
        .unwrap();
        assert_eq!(
            catalog
                .unresolved_moment_receipts("orphaned", &identity)
                .unwrap()
                .len(),
            1,
            "the injected pending row is readable under the saved artifact identity"
        );

        let mut forced = options.clone();
        forced.force = true;
        for retry_uncertain in [false, true] {
            forced.retry_uncertain = retry_uncertain;
            let report = catalog
                .discover_moments_with_provider(
                    &project.id,
                    &["orphaned".into()],
                    &forced,
                    &provider,
                    |_| ControlFlow::Continue(()),
                )
                .unwrap();
            assert!(
                !report.usage_complete,
                "retry_uncertain={retry_uncertain}, calls={}, report={report:?}: retry must keep an orphaned charge unknown until reconciled",
                provider.calls.load(Ordering::SeqCst)
            );
            assert_eq!(
                provider.calls.load(Ordering::SeqCst),
                if retry_uncertain { 2 } else { 1 },
                "force alone is blocked; explicit retry may make one fresh request"
            );
        }
        let page = catalog
            .moment_evidence_page("orphaned", 0, 50)
            .unwrap()
            .unwrap();
        assert_eq!(page.artifact.status, MomentStatus::NeedsReview);
        assert!(
            page.artifact
                .provenance
                .receipts
                .iter()
                .any(|receipt| receipt.reused
                    && !receipt.usage_complete
                    && receipt.run_id == "crashed-run")
        );
    }

    #[test]
    fn uncertain_transport_blocks_force_and_explicit_retry_keeps_unknown_history() {
        if !tool_available("ffmpeg") || !tool_available("ffprobe") {
            return;
        }
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog
            .create_project("Synthetic interrupted request")
            .unwrap();
        generated_asset(&catalog, &project.id, directory.path(), "interrupted", 24);
        let provider = FailAtProvider {
            calls: AtomicUsize::new(0),
            fail_at: 2,
            events: EventProvider {
                calls: AtomicUsize::new(0),
                centers_ms: vec![5_000],
            },
        };
        let mut options = event_options();
        options.coarse_frames = 8;
        let first = catalog
            .discover_moments_with_provider(
                &project.id,
                &["interrupted".into()],
                &options,
                &provider,
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        assert!(!first.usage_complete);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);

        let mut forced = options.clone();
        forced.force = true;
        let blocked = catalog
            .discover_moments_with_provider(
                &project.id,
                &["interrupted".into()],
                &forced,
                &provider,
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        assert!(!blocked.usage_complete);
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            2,
            "force alone must not duplicate an uncertain provider request"
        );

        let mut retry = options;
        retry.retry_uncertain = true;
        let recovered = catalog
            .discover_moments_with_provider(
                &project.id,
                &["interrupted".into()],
                &retry,
                &provider,
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        assert!(
            !recovered.usage_complete,
            "unknown prior spend remains reflected after explicit retry"
        );
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            3,
            "only the uncertain step should be retried; completed coarse evidence is reused"
        );
        let page = catalog
            .moment_evidence_page("interrupted", 0, 50)
            .unwrap()
            .unwrap();
        assert!(
            page.artifact
                .provenance
                .receipts
                .iter()
                .any(|receipt| receipt.reused && !receipt.usage_complete),
            "uncertain attempt history must remain visible"
        );
    }

    #[test]
    fn accepted_timed_dialogue_is_grounded_across_pauses_on_video_audio_source() {
        if !tool_available("ffmpeg") || !tool_available("ffprobe") {
            eprintln!("skipping generated dialogue fixture: ffmpeg/ffprobe unavailable");
            return;
        }
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Synthetic timed dialogue").unwrap();
        generated_asset(&catalog, &project.id, directory.path(), "dialogue", 24);
        let source_fingerprint = catalog.proxy_fingerprint("dialogue").unwrap();
        save_transcript(
            &catalog.connection().unwrap(),
            &StoredTranscript {
                asset_id: "dialogue".into(),
                recipe_key: crate::analysis::STT_RECIPE_KEY.into(),
                source_fingerprint,
                status: TranscriptStatus::Speech,
                outcome_reason: crate::analysis::store::TranscriptOutcomeReason::Accepted,
                reject_reason: String::new(),
                primary_language: "en".into(),
                speech_seconds: 2.4,
                window_count: 1,
                provider_segment_count: 2,
                mean_token_probability: None,
                repetition_ratio: None,
                segments: vec![
                    DecodedSegment {
                        start_ms: 3_500,
                        end_ms: 4_300,
                        language: "en".into(),
                        text: "first synthetic line".into(),
                        mean_token_probability: 0.9,
                    },
                    DecodedSegment {
                        start_ms: 16_500,
                        end_ms: 17_300,
                        language: "en".into(),
                        text: "second synthetic line".into(),
                        mean_token_probability: 0.9,
                    },
                    DecodedSegment {
                        start_ms: 22_000,
                        end_ms: 22_800,
                        language: "en".into(),
                        text: "late synthetic line".into(),
                        mean_token_probability: 0.9,
                    },
                ],
            },
        )
        .unwrap();
        let provider = DialogueProvider;
        let mut options = event_options();
        options.payload = MomentPayload::FramesAndTranscript;
        let report = catalog
            .discover_moments_with_provider(
                &project.id,
                &["dialogue".into()],
                &options,
                &provider,
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        let page = catalog
            .moment_evidence_page("dialogue", 0, 50)
            .unwrap()
            .unwrap();
        assert_eq!(
            report.processed,
            1,
            "dialogue discovery report: {}",
            serde_json::to_string(&report).unwrap()
        );
        assert_eq!(
            page.artifact.status,
            MomentStatus::Ready,
            "review questions: {:?}",
            page.artifact.review_questions
        );
        assert_eq!(
            page.artifact.provenance.payload,
            MomentPayload::FramesAndTranscript
        );
        assert_eq!(
            page.artifact.candidates.len(),
            3,
            "only timed utterances should be returned across the pauses"
        );
        assert!(
            page.artifact
                .candidates
                .iter()
                .all(|candidate| candidate.role == "dialogue"
                    && !candidate.dialogue_boundaries.is_empty())
        );
        assert!(
            page.artifact
                .candidates
                .iter()
                .all(
                    |candidate| candidate.evidence.iter().any(|reference| reference.kind
                        == "accepted_transcript"
                        && reference
                            .span
                            .is_some_and(|span| span.overlaps(candidate.payoff)))
                )
        );
        assert!(
            !page
                .artifact
                .provenance
                .representation_fingerprint
                .is_empty()
        );
        assert!(page.artifact.provenance.source_duration_ms >= 24_000);
    }

    #[test]
    fn actual_provider_identity_is_not_inferred_when_receipt_fields_are_missing_or_mismatched() {
        if !tool_available("ffmpeg") || !tool_available("ffprobe") {
            return;
        }
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog
            .create_project("Synthetic provider receipts")
            .unwrap();
        generated_asset(&catalog, &project.id, directory.path(), "receipt", 10);
        let mut options = event_options();
        options.coarse_frames = 8;
        let missing = ReportedIdentityProvider {
            calls: AtomicUsize::new(0),
            actual_model: None,
            provider: None,
        };
        let first = catalog
            .discover_moments_with_provider(
                &project.id,
                &["receipt".into()],
                &options,
                &missing,
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        assert_eq!(first.needs_review, 1);
        let missing_page = catalog
            .moment_evidence_page("receipt", 0, 50)
            .unwrap()
            .unwrap();
        assert_eq!(
            missing_page.artifact.provenance.requested_provider,
            "openrouter"
        );
        assert!(missing_page.artifact.provenance.provider.is_empty());
        assert!(missing_page.artifact.provenance.actual_model.is_empty());
        assert!(missing_page.artifact.provenance.usage_complete);

        options.model = FALLBACK_MODEL.into();
        options.allowed_models = vec![FALLBACK_MODEL.into()];
        let mismatch = ReportedIdentityProvider {
            calls: AtomicUsize::new(0),
            actual_model: Some("upstream/other-model".into()),
            provider: Some("upstream-endpoint".into()),
        };
        let second = catalog
            .discover_moments_with_provider(
                &project.id,
                &["receipt".into()],
                &options,
                &mismatch,
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        assert_eq!(second.needs_review, 1);
        let mismatch_page = catalog
            .moment_evidence_page("receipt", 0, 50)
            .unwrap()
            .unwrap();
        assert_eq!(
            mismatch_page.artifact.provenance.requested_model,
            FALLBACK_MODEL
        );
        assert_eq!(
            mismatch_page.artifact.provenance.actual_model,
            "upstream/other-model"
        );
        assert_eq!(
            mismatch_page.artifact.provenance.provider,
            "upstream-endpoint"
        );
        assert!(
            mismatch_page
                .artifact
                .review_questions
                .iter()
                .any(|question| question.contains("differs from the selected model"))
        );
    }

    #[test]
    fn valid_proxy_works_without_original_but_removed_and_quarantined_assets_are_unavailable() {
        if !tool_available("ffmpeg") || !tool_available("ffprobe") {
            return;
        }
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog
            .create_project("Synthetic proxy availability")
            .unwrap();
        let offline_source =
            generated_asset(&catalog, &project.id, directory.path(), "offline", 10);
        generated_asset(&catalog, &project.id, directory.path(), "quarantined", 10);
        generated_asset(&catalog, &project.id, directory.path(), "removed", 10);
        fs::remove_file(&offline_source).unwrap();
        catalog.connection().unwrap().execute(
            "UPDATE asset_video_validation SET status='quarantined',diagnostic='fixture quarantine' WHERE asset_id='quarantined'",
            [],
        ).unwrap();
        catalog
            .connection()
            .unwrap()
            .execute("UPDATE assets SET removed=1 WHERE id='removed'", [])
            .unwrap();
        let provider = EmptyMomentProvider {
            calls: AtomicUsize::new(0),
        };
        let options = event_options();
        let offline = catalog
            .discover_moments_with_provider(
                &project.id,
                &["offline".into()],
                &options,
                &provider,
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        assert_eq!(
            offline.no_candidates, 1,
            "validated current proxy remains usable when original media is offline"
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        for id in ["quarantined", "removed"] {
            let report = catalog
                .discover_moments_with_provider(
                    &project.id,
                    &[id.into()],
                    &options,
                    &provider,
                    |_| ControlFlow::Continue(()),
                )
                .unwrap();
            assert_eq!(
                report.needs_review, 1,
                "{id} should yield an unavailable review artifact"
            );
            let page = catalog.moment_evidence_page(id, 0, 50).unwrap().unwrap();
            assert_eq!(page.artifact.status, MomentStatus::Unavailable);
            assert!(
                page.artifact
                    .coverage
                    .iter()
                    .all(|segment| segment.level == CoverageLevel::Uninspected)
            );
        }
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            2,
            "removed/quarantined clips must not reach the provider"
        );
    }

    fn tool_available(name: &str) -> bool {
        match crate::tool_command(name).arg("-version").output() {
            Ok(output) => {
                assert!(output.status.success(), "{name} -version failed");
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => panic!("could not run {name}: {error}"),
        }
    }

    #[test]
    fn cached_moment_discovery_reuses_provider_steps_without_training_rate_history() {
        if !tool_available("ffmpeg") || !tool_available("ffprobe") {
            eprintln!("skipping generated moment cache fixture: ffmpeg/ffprobe unavailable");
            return;
        }
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Synthetic moments cache").unwrap();
        let source = generated_asset(&catalog, &project.id, directory.path(), "moment_asset", 10);

        let provider = EmptyMomentProvider {
            calls: AtomicUsize::new(0),
        };
        let options = MomentRunOptions {
            model: DEFAULT_MODEL.into(),
            provider: "openrouter".into(),
            payload: MomentPayload::Frames,
            budget_usd: 0.5,
            coarse_frames: 8,
            refinement_frames: 8,
            max_candidates: 1,
            profile: EditProfile::default(),
            force: false,
            retry_uncertain: false,
            allowed_models: vec![DEFAULT_MODEL.to_string()],
            allow_fallback: false,
            policy_id: None,
        };
        let assets = vec!["moment_asset".to_owned()];
        let run = || {
            catalog
                .discover_moments_with_provider(&project.id, &assets, &options, &provider, |_| {
                    ControlFlow::Continue(())
                })
                .unwrap()
        };

        let first = run();
        assert_eq!(
            first.no_candidates,
            1,
            "unexpected first discovery report: {}; evidence: {}",
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&catalog.moment_evidence_page("moment_asset", 0, 50).unwrap())
                .unwrap()
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        let first_page = catalog
            .moment_evidence_page("moment_asset", 0, 50)
            .unwrap()
            .unwrap();
        let original_identity = first_page.artifact.identity.clone();
        let original_candidate_ids = first_page
            .artifact
            .candidates
            .iter()
            .map(|c| c.id.clone())
            .collect::<Vec<_>>();
        let samples_after_first: i64 = catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT COALESCE(SUM(samples),0) FROM batch_performance WHERE task_kind LIKE 'moments.%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(samples_after_first > 0);

        let second = run();
        assert_eq!(second.no_candidates, 1);
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "cached discovery must make no provider calls"
        );
        let samples_after_reuse: i64 = catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT COALESCE(SUM(samples),0) FROM batch_performance WHERE task_kind LIKE 'moments.%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            samples_after_reuse, samples_after_first,
            "reused evidence must not be measured as new processing throughput"
        );
        let cached_page = catalog
            .moment_evidence_page("moment_asset", 0, 50)
            .unwrap()
            .unwrap();
        assert_eq!(cached_page.artifact.identity, original_identity);
        assert_eq!(
            cached_page
                .artifact
                .candidates
                .iter()
                .map(|c| c.id.clone())
                .collect::<Vec<_>>(),
            original_candidate_ids
        );

        let mut changed_model = options.clone();
        changed_model.model = FALLBACK_MODEL.into();
        changed_model.allowed_models = vec![FALLBACK_MODEL.into()];
        catalog
            .discover_moments_with_provider(&project.id, &assets, &changed_model, &provider, |_| {
                ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            2,
            "model change must invalidate reusable steps"
        );
        assert_ne!(
            catalog
                .moment_evidence_page("moment_asset", 0, 50)
                .unwrap()
                .unwrap()
                .artifact
                .identity,
            original_identity
        );

        let mut changed_profile = options.clone();
        changed_profile.profile.select_max_ms -= 1_000;
        catalog
            .discover_moments_with_provider(
                &project.id,
                &assets,
                &changed_profile,
                &provider,
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            3,
            "edit profile change must invalidate reusable steps"
        );

        let original_bytes = fs::read(&source).unwrap();
        let write = catalog.begin_proxy_write("moment_asset").unwrap().unwrap();
        let replacement = crate::tool_command("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=blue:s=640x360:r=24:d=10",
                "-an",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(write.temporary_path())
            .status()
            .unwrap();
        assert!(
            replacement.success(),
            "replacement H.264 proxy must be generated"
        );
        catalog.commit_proxy_write(write, None).unwrap();
        assert_eq!(fs::read(&source).unwrap(), original_bytes);
        catalog
            .discover_moments_with_provider(&project.id, &assets, &options, &provider, |_| {
                ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            4,
            "proxy replacement must invalidate reusable steps"
        );
        let replaced = catalog
            .moment_evidence_page("moment_asset", 0, 50)
            .unwrap()
            .unwrap();
        assert_ne!(
            replaced.artifact.provenance.representation_fingerprint,
            first_page.artifact.provenance.representation_fingerprint
        );
        catalog
            .connection()
            .unwrap()
            .execute(
                "UPDATE assets SET duration_seconds=9.9 WHERE id='moment_asset'",
                [],
            )
            .unwrap();
        assert!(
            !catalog
                .moment_evidence_page("moment_asset", 0, 50)
                .unwrap()
                .unwrap()
                .current
        );
        let corrected = catalog
            .discover_moments_with_provider(&project.id, &assets, &options, &provider, |_| {
                ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(corrected.no_candidates, 1);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 5);
        let corrected = catalog
            .moment_evidence_page("moment_asset", 0, 50)
            .unwrap()
            .unwrap();
        assert_eq!(corrected.artifact.provenance.source_duration_ms, 9900);
        assert_ne!(corrected.artifact.identity, replaced.artifact.identity);
    }
}
