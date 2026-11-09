//! Catalog-owned, resumable moment discovery. Adapters never write derived rows.

use super::{
    logic::*,
    model::*,
    store::{self, MomentStep},
};
use crate::analysis::{
    accounting::{AccountedVlmProvider, analysis_policy_budget_scope_id},
    framepack,
    openrouter::{OpenRouterProvider, frame_data_uri, resolve_api_key},
    provider::{ProviderChain, ProviderFailure, VlmProvider, VlmRequest, VlmResponse},
    run_policy::{AnalysisRunKind, bulk_request_accounting_context, verify_bulk_authority},
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
                _ => bail!("native video requires its explicitly authorized native runtime"),
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
                _ => bail!("native video requires its explicitly authorized native runtime"),
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
        on_progress: F,
    ) -> Result<MomentRunReport>
    where
        F: FnMut(MomentProgress) -> ControlFlow<()>,
    {
        options.validate()?;
        let policy_id = options.policy_id.as_deref().ok_or_else(|| {
            anyhow!("paid moment discovery must be bound to an authorized, piloted run policy")
        })?;
        let state = self.authorized_policy_for_bulk(policy_id, AnalysisRunKind::MomentDiscovery)?;
        let policy = state.authorization.policy;
        ensure!(
            policy.project_id == project_id,
            "authorized policy belongs to a different project"
        );
        let authorized_scope = policy
            .scope
            .iter()
            .map(|scope| scope.asset_id.as_str())
            .collect::<Vec<_>>();
        let requested_scope = asset_ids.iter().map(String::as_str).collect::<Vec<_>>();
        ensure!(
            requested_scope == authorized_scope,
            "authorized run scope does not exactly match the requested asset ids"
        );
        let bound_options = policy.moment_options()?;
        ensure!(
            options.policy_id.as_deref() == Some(policy.policy_id.as_str())
                && options.model == bound_options.model
                && options.provider == bound_options.provider
                && options.payload == bound_options.payload
                && (options.budget_usd - bound_options.budget_usd).abs() <= 1e-9
                && options.coarse_frames == bound_options.coarse_frames
                && options.refinement_frames == bound_options.refinement_frames
                && options.max_candidates == bound_options.max_candidates
                && options.profile == bound_options.profile
                && options.force == bound_options.force
                && options.retry_uncertain == bound_options.retry_uncertain
                && options.allowed_models == bound_options.allowed_models
                && options.allow_fallback == bound_options.allow_fallback,
            "requested moment policy, payload or whole budget does not match the authorized policy"
        );
        self.discover_moments_pipeline(
            project_id,
            asset_ids,
            &bound_options,
            provider,
            on_progress,
            Some(policy),
        )
    }

    /// The unaccounted core is retained only for focused domain tests. The
    /// production entry points above always pass an authorized policy so each
    /// actual step transmission crosses `AccountedVlmProvider`.
    #[cfg(test)]
    fn discover_moments_with_provider_for_domain_test<F>(
        &self,
        project_id: &str,
        asset_ids: &[String],
        options: &MomentRunOptions,
        provider: &dyn VlmProvider,
        on_progress: F,
    ) -> Result<MomentRunReport>
    where
        F: FnMut(MomentProgress) -> ControlFlow<()>,
    {
        self.discover_moments_pipeline(project_id, asset_ids, options, provider, on_progress, None)
    }

    fn discover_moments_pipeline<F>(
        &self,
        project_id: &str,
        asset_ids: &[String],
        options: &MomentRunOptions,
        provider: &dyn VlmProvider,
        mut on_progress: F,
        accounted_policy: Option<crate::RunPolicy>,
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
        if let Some(policy) = accounted_policy.as_ref() {
            ensure!(
                policy.kind == AnalysisRunKind::MomentDiscovery,
                "authorized policy kind does not match moment discovery"
            );
            ensure!(
                policy.project_id == project_id,
                "authorized policy belongs to a different project"
            );
            // The stored policy owns the immutable whole-policy ceiling. A live
            // remaining balance is never substituted for this authority value.
            effective_options.budget_usd = policy.budget_usd;
            effective_options.allowed_models = policy.allowed_models.clone();
            effective_options.allow_fallback = policy.allow_fallback;
            effective_options.policy_id = Some(policy.policy_id.clone());
        }
        provider.prepare()?;
        let options = &effective_options;
        let run_id = if let Some(policy) = accounted_policy.as_ref() {
            // Freeze/verify before the accounted run row exists.
            verify_bulk_authority(self, policy)?;
            self.begin_accounted_analysis_run(&policy.project_id, &policy.policy_id, "moments")?
        } else {
            let run_id = self.begin_analysis_run(project_id)?;
            // Test-only unaccounted pipeline still identifies its stage.
            self.connection()?.execute(
                "UPDATE analysis_runs SET stage_reached='moments',policy_id=?2 WHERE id=?1",
                params![run_id, options.policy_id.clone().unwrap_or_default()],
            )?;
            run_id
        };
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
            approved_policy: accounted_policy.clone(),
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
                    let failure = ProviderFailure::from_anyhow(&error);
                    if failure.category == crate::ProviderFailureCategory::Cancelled {
                        state.cancelled = true;
                    } else if failure.category
                        == crate::ProviderFailureCategory::LocalBudgetExhausted
                    {
                        state.budget_stopped = true;
                    } else if failure.category == crate::ProviderFailureCategory::LocalAdmission
                        && let Some(policy) = accounted_policy.as_ref()
                        && self
                            .analysis_budget_snapshot(&analysis_policy_budget_scope_id(
                                &policy.project_id,
                                &policy.policy_id,
                            ))?
                            .is_some_and(|snapshot| {
                                snapshot.dispatch_stopped || snapshot.available_usd <= 1e-12
                            })
                    {
                        state.budget_stopped = true;
                    }
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
        let canonical_projection = if let Some(policy) = accounted_policy.as_ref() {
            Some(self.analysis_accounting_aggregate_for_run(
                &policy.policy_id,
                policy.kind.as_str(),
                &run_id,
            )?)
        } else {
            None
        };
        if let Some(projection) = canonical_projection.as_ref() {
            report.cost_usd = projection.confirmed_cost_usd;
            report.usage_complete = !projection.has_unresolved_usage();
        } else {
            report.cost_usd = state.spent;
            report.usage_complete = !state.unknown_usage && !state.historical_unknown;
        }
        report.cancelled = state.cancelled;
        report.budget_exhausted = state.budget_stopped;
        self.save_batch_history(&task_key, estimator.history())?;
        let confirmed_cost = canonical_projection
            .as_ref()
            .map_or(state.spent, |projection| projection.confirmed_cost_usd);
        self.finish_analysis_run(
            &run_id,
            &AnalysisRunOutcome {
                status: if state.cancelled {
                    "cancelled"
                } else if report.failed > 0 || !report.usage_complete {
                    "failed"
                } else {
                    "completed"
                }
                .into(),
                stage_reached: "moments".into(),
                clips_analyzed: report.processed,
                cost_usd: confirmed_cost,
                budget_exhausted: state.budget_stopped,
                error: if !report.usage_complete {
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
        self.moment_source_mode(asset, include_transcript, false)
    }

    fn moment_source_mode(
        &self,
        asset: &Asset,
        include_transcript: bool,
        explicit_review: bool,
    ) -> Result<SourceContext> {
        let source = if explicit_review {
            self.validated_review_proxy_source(asset, &|| false)?
        } else {
            self.analysis_proxy_source(asset)?
        };
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
                estimated_cost_usd: None,
                cost_basis: None,
                native_source: None,
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
                estimated_cost_usd: None,
                cost_basis: None,
                native_source: None,
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
            if cached.state == "local_failure" {
                continue;
            }
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
        self.moment_artifact_current_mode(asset, artifact, false)
    }

    fn moment_artifact_current_mode(
        &self,
        asset: &Asset,
        artifact: &MomentArtifact,
        explicit_review: bool,
    ) -> bool {
        if artifact.provenance.native_source.is_some() {
            return self.native_video_moment_current(asset, artifact, explicit_review);
        }
        if artifact.provenance.recipe_version != RECIPE_VERSION
            || artifact.provenance.prompt_version != PROMPT_VERSION
            || artifact.provenance.schema_version != SCHEMA_VERSION
            || artifact.provenance.time_mapping != TIME_MAPPING
        {
            return false;
        }
        self.moment_source_mode(
            asset,
            artifact.provenance.payload == MomentPayload::FramesAndTranscript,
            explicit_review,
        )
        .is_ok_and(|source| {
            source.fingerprint == artifact.provenance.representation_fingerprint
                && source.duration_ms == artifact.provenance.source_duration_ms
                && source.transcript_fingerprint == artifact.provenance.transcript_fingerprint
                && source.observation_fingerprint == artifact.provenance.observation_fingerprint
        })
    }

    /// One immutable row observation for explicit local selection. A full-row
    /// digest catches force-replacement under the same configuration identity;
    /// paging could otherwise splice two generations. No paid preparation runs.
    pub(crate) fn selection_moment_artifact(
        &self,
        asset_id: &str,
        identity: &str,
    ) -> Result<Option<(MomentArtifact, bool, String)>> {
        let asset = self.get_asset(asset_id)?;
        let connection = self.connection()?;
        if !crate::table_exists(&connection, "moment_artifacts")? {
            return Ok(None);
        }
        let body: Option<String> = connection
            .query_row(
                "SELECT body_json FROM moment_artifacts WHERE asset_id=?1 AND identity=?2 AND length(body_json)<=8388608",
                params![asset_id, identity],
                |r| r.get(0),
            )
            .optional()?;
        let Some(body) = body else {
            return Ok(None);
        };
        ensure!(
            body.len() <= 8 * 1024 * 1024,
            "moment evidence exceeds selection input bound"
        );
        let artifact: MomentArtifact = serde_json::from_str(&body)?;
        ensure!(
            artifact.asset_id == asset_id && artifact.identity == identity,
            "moment evidence identity does not match its canonical row"
        );
        let current = self.moment_artifact_current_mode(&asset, &artifact, true);
        Ok(Some((
            artifact,
            current,
            blake3::hash(body.as_bytes()).to_hex().to_string(),
        )))
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
    #[serde(default)]
    raw_output: Option<String>,
}

impl StepRunner<'_> {
    fn step_settled_attempts(&self) -> Result<usize> {
        let Some(policy) = self.state.approved_policy.as_ref() else {
            return Ok(0);
        };
        let projection = self.catalog.analysis_accounting_aggregate_for_run(
            &policy.policy_id,
            policy.kind.as_str(),
            &self.state.run_id,
        )?;
        Ok(projection.completed_request_count + projection.uncertain_request_count)
    }

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
        let speech_contract = match self.options.payload {
            MomentPayload::Frames => {
                "This request contains frames only: do not claim dialogue, words, or spoken-thought boundaries."
            }
            MomentPayload::FramesAndTranscript => {
                "Speech claims require a cited accepted timed utterance; preserve each cited utterance entirely in the core or explicit context. A transcript alone cannot establish a visible payoff."
            }
            _ => bail!("native video cannot enter the frame-payload provider path"),
        };
        let refinement_seed = seed.map(|candidate| {
            json!({
                "event": candidate.event,
                "core": candidate.core,
                "payoff": candidate.payoff,
                "reason": candidate.reason,
            })
        });
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
        ensure!(
            !self.state.budget_stopped,
            "authorized moment budget exhausted; no further requests allowed in this run"
        );
        let current_asset = self.catalog.get_asset(&self.asset.id)?;
        ensure!(
            !current_asset.removed
                && current_asset.project_id == self.asset.project_id
                && current_asset.duration_seconds == self.asset.duration_seconds,
            "source scope or time mapping changed; re-plan moment discovery"
        );
        if let Some(policy) = self.state.approved_policy.as_ref() {
            let snapshot = self
                .catalog
                .analysis_budget_snapshot(&analysis_policy_budget_scope_id(
                    &policy.project_id,
                    &policy.policy_id,
                ))?
                .ok_or_else(|| anyhow!("analysis budget authority is not initialized"))?;
            if snapshot.dispatch_stopped || snapshot.available_usd <= 1e-12 {
                self.state.budget_stopped = true;
                bail!(
                    "authorized moment budget exhausted; available ${:.6} of ${:.6}",
                    snapshot.available_usd,
                    snapshot.budget_usd
                );
            }
        } else if self.state.spent >= self.options.budget_usd {
            // Test-only unaccounted pipeline retains its local ceiling.
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
            "Discover reusable within-clip moments from ONLY this explicitly scoped evidence. All times are integer milliseconds on the original source clock; inspectedSpan is the exact allowed interval [startMs,endMs), and every proposed core, payoff, setup, and context must stay within it. Return bounded JSON; zero moments is valid. Do not force source inclusion or invent events. Inspect every supplied time, including late action. Nearby views of the same payoff are one event; genuinely separate payoffs remain separate. Identify the actual peak and necessary setup, not the first N seconds. Prefer concise cores using the edit profile, with optional context and occasional justified long holds. Sampled frames are not continuous video; set uncertain=true when temporal boundaries or visual content cannot be established. {} Use only evidence keys supplied in this request; visual grounding requires a supplied frame near the payoff. Role must be action, dialogue, atmosphere, transition or hold. Payoff must fit inside core. Optional setup precedes payoff; optional context contains core. Keep explanations short. Refine-phase evidence is denser than coarse evidence; check setup/payoff, dialogue boundaries and whether the original candidate actually exists. The refinement seed is only an event/core/payoff/reason hint; its prior evidence is not available for citation. Preserve useful long event context while keeping the selected core concise.\nEVIDENCE={}\n",
            speech_contract,
            json!({"phase":phase,"inspectedSpan":span,"sourceDurationMs":self.source.duration_ms,"frameTimesMs":times,"evidence":evidence,"acceptedTimedSpeech":speech,"priorCoarseRange":self.source.coarse_observation,"candidateToRefine":refinement_seed,"editProfile":self.options.profile})
        );
        let request = VlmRequest {
            prompt,
            frames,
            schema: output_schema(limit, span, &keys),
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
            estimated_cost_usd: None,
            cost_basis: None,
            native_usage: None,
        };
        self.save(
            &step_key,
            CachedStep {
                prior_receipts: prior_receipts.clone(),
                state: "pending".into(),
                reply: None,
                receipt: pending_receipt.clone(),
                error: String::new(),
                raw_output: None,
            },
        )?;
        self.state.executed_steps += 1;
        ensure!(
            !self.state.cancelled,
            "moment run cancelled before the next paid transmission"
        );
        let settled_before = self.step_settled_attempts()?;
        let (outcome, accounted) = match self.state.approved_policy.clone() {
            Some(policy) => {
                let context = bulk_request_accounting_context(
                    &policy,
                    &self.state.run_id,
                    &self.asset.id,
                    format!("{}:{}:{}", policy.kind.as_str(), self.asset.id, step_key),
                    self.options.retry_uncertain,
                )?;
                let accounted = AccountedVlmProvider::new(self.catalog, context, self.provider)
                    .with_explicit_retry(self.options.retry_uncertain);
                let cancelled = &mut self.state.cancelled;
                let spent = self.state.spent;
                let notify = &mut self.notify;
                let outcome = self
                    .chain
                    .analyze_with_cancel(&accounted, &request, &mut || {
                        if !*cancelled && notify(phase, spent).is_break() {
                            *cancelled = true;
                        }
                        *cancelled
                    });
                (outcome, Some(accounted))
            }
            None => (self.chain.analyze(self.provider, &request), None),
        };
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                let failure = ProviderFailure::from_anyhow(&error);
                if failure.category == crate::ProviderFailureCategory::LocalBudgetExhausted {
                    self.state.budget_stopped = true;
                }
                if failure.category == crate::ProviderFailureCategory::Cancelled {
                    self.state.cancelled = true;
                }
                if failure.category.is_pre_upload_local()
                    && self.step_settled_attempts()? == settled_before
                {
                    // A fallback's local denial does not prove the earlier
                    // primary was never uploaded. Only classify the whole step
                    // as local when this chain added no completed/uncertain
                    // paid attempt to its canonical run projection.
                    self.save(
                        &step_key,
                        CachedStep {
                            prior_receipts: prior_receipts.clone(),
                            state: "local_failure".into(),
                            reply: None,
                            receipt: pending_receipt,
                            error: failure.safe_detail(),
                            raw_output: None,
                        },
                    )?;
                    return Err(error);
                }
                self.state.unknown_usage = true;
                self.save(
                    &step_key,
                    CachedStep {
                        prior_receipts: prior_receipts.clone(),
                        state: "uncertain".into(),
                        reply: None,
                        receipt: pending_receipt.clone(),
                        error: format!("{error:#}"),
                        raw_output: None,
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
            estimated_cost_usd: None,
            cost_basis: None,
            native_usage: None,
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
                if let Some(accounted) = &accounted {
                    accounted.record_response_success(&response.model)?;
                }
                self.save(
                    &step_key,
                    CachedStep {
                        prior_receipts: prior_receipts.clone(),
                        state: "complete".into(),
                        reply: Some(reply.clone()),
                        receipt,
                        error: String::new(),
                        raw_output: None,
                    },
                )?;
                Ok((reply, evidence))
            }
            Err(error) => {
                if let Some(accounted) = &accounted {
                    let category = if error.to_string().to_ascii_lowercase().contains("schema")
                        || error.to_string().to_ascii_lowercase().contains("json")
                    {
                        crate::analysis::provider::ProviderResponseRejection::Schema
                    } else {
                        crate::analysis::provider::ProviderResponseRejection::ContentQuality
                    };
                    accounted.record_response_rejection(&response.model, category)?;
                }
                self.save(
                    &step_key,
                    CachedStep {
                        prior_receipts: prior_receipts.clone(),
                        state: "rejected".into(),
                        reply: None,
                        receipt,
                        error: format!("{error:#}"),
                        raw_output: Some(response.body.clone()),
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
#[path = "catalog_tests.rs"]
mod tests;
