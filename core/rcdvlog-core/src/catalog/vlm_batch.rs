//! Per-clip visual analysis and analysis batches.

use crate::analysis::store::TranscriptCurrentState;
use crate::analysis::vlm::{
    PromptContext, PromptTranscript, analyze_clip_with_cancel as run_vlm_with_cancel,
};
use crate::analysis::vlm_store::{
    AnalysisStatus, StoredAnalysis, ai_labels_for_asset, analysis_is_current, clear_ai_labels,
    commit_thumbnail, load_analysis, load_frame_pack, record_ai_labels, save_analysis,
    save_frame_pack, set_title_and_description,
};
use crate::*;
use anyhow::{Context, Result, anyhow, ensure};
use batch_estimator::BatchEstimator;
use std::collections::VecDeque;
use std::ops::ControlFlow;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Instant;

impl Catalog {
    /// Runs cloud vision for one clip. This is an explicit replacement action;
    /// project-wide idempotence is provided by `analyze_missing`. The explicit
    /// invocation materializes and records an exact one-asset policy, then runs
    /// the same bounded capability pilot before the paid replacement call.
    pub fn analyze_asset(&self, asset_id: &str) -> Result<AnalysisOutcome> {
        let asset = self.get_asset(asset_id)?;
        let budget = resolve_analysis_budget(&AnalysisRunOptions::default())?;
        anyhow::ensure!(
            budget.is_finite(),
            "an explicit finite analysis budget is required for paid observation"
        );
        anyhow::ensure!(
            api_credential_present(),
            "OpenRouter credentials are not available; no analysis request was made"
        );
        let provider = OpenRouterProvider::new(resolve_api_key()?)?;
        let chain = ProviderChain::new();
        let mut request = crate::RunPolicyRequest::observation(
            asset.project_id.clone(),
            vec![asset_id.to_string()],
            chain.primary_model().to_string(),
            chain
                .allowed_models()
                .iter()
                .map(|model| (*model).to_string())
                .collect(),
            chain.allows_fallback(),
            budget,
            true,
        );
        request.include_project_vocabulary = true;
        self.authorize_run_policy(&request)?;
        let pilot = self.run_run_policy_pilot_with_provider(&request, &provider, false)?;
        anyhow::ensure!(
            pilot.status == "passed",
            "capability pilot failed for this exact policy: {} ({})",
            pilot.failure_code,
            pilot.validation.detail
        );
        let mut report =
            self.analyze_authorized_with_provider(&pilot.policy_id, &provider, |_| {
                ControlFlow::Continue(())
            })?;
        report
            .outcomes
            .pop()
            .ok_or_else(|| anyhow!("authorized single-asset analysis produced no result"))
    }

    pub(crate) fn analysis_frame_pack(&self, asset: &Asset) -> Result<FramePackManifest> {
        let proxy = Self::require_proxy(asset)?;
        let fingerprint = self.proxy_fingerprint(&asset.id)?;
        let connection = self.connection()?;
        if let Some(manifest) = load_frame_pack(&connection, &asset.id)?
            && manifest.recipe_key == FRAMEPACK_RECIPE_KEY
            && manifest.source_fingerprint == fingerprint
        {
            return Ok(manifest);
        }
        let manifest = build_frame_pack(&proxy, &fingerprint)?;
        save_frame_pack(&connection, &asset.id, &manifest)?;
        Ok(manifest)
    }

    pub(crate) fn prompt_vocabulary(&self, project_id: &str) -> Result<Vec<String>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT display_label FROM label_vocabulary WHERE project_id=?1 ORDER BY usage_count DESC, normalized_label",
        )?;
        let vocabulary = statement
            .query_map([project_id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        if !vocabulary.is_empty() {
            return Ok(vocabulary);
        }
        drop(statement);
        let mut labels = self
            .list_assets(project_id)?
            .into_iter()
            .flat_map(|asset| asset.labels)
            .collect::<Vec<_>>();
        labels.sort_by_key(|label| label.to_lowercase());
        labels.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        labels.truncate(80);
        Ok(labels)
    }

    pub(crate) fn prompt_transcript(&self, asset_id: &str) -> Result<Option<PromptTranscript>> {
        // Prompt context and RunPolicy::source_scope share this exact currentness
        // owner. Historical rows remain readable through asset_transcript, but
        // only a currently proved local/native artifact may contribute text.
        let state = self.current_transcript_state(asset_id)?;
        let (transcript, native) = match &state {
            TranscriptCurrentState::CurrentLocal(transcript) => (transcript, false),
            TranscriptCurrentState::CurrentNative(transcript) => (transcript, true),
            TranscriptCurrentState::Missing
            | TranscriptCurrentState::RetryableLocalFailed(_)
            | TranscriptCurrentState::ProtectedNative { .. } => return Ok(None),
        };
        // Review/rejected/no-accepted native outcomes are retained for a human,
        // not accepted speech. Only current accepted native Speech may become
        // AI context. Local status semantics remain unchanged.
        if (native && transcript.status != TranscriptStatus::Speech)
            || matches!(
                transcript.status,
                TranscriptStatus::ReviewRequired | TranscriptStatus::Failed
            )
        {
            return Ok(None);
        }
        let text = transcript
            .segments
            .iter()
            .map(|segment| segment.text.trim())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        Ok(Some(PromptTranscript {
            status: transcript.status,
            text,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn analyze_asset_with_provider(
        &self,
        asset_id: &str,
        provider: &dyn VlmProvider,
        chain: &ProviderChain,
        include_transcript: bool,
        approved_vocabulary: &[String],
        policy: &crate::RunPolicy,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<AnalysisOutcome> {
        let asset = self.get_asset(asset_id)?;
        let proxy = Self::require_proxy(&asset)?;
        let manifest = self.analysis_frame_pack(&asset)?;
        let parent = self
            .database_path
            .parent()
            .ok_or_else(|| anyhow!("catalog has no parent directory"))?;
        let frame_directory = parent
            .join("cache")
            .join("framepacks")
            .join(&asset.project_id)
            .join(&asset.id)
            .join(&manifest.source_fingerprint);
        let frame_paths = encode_frames(&proxy, &manifest, &frame_directory)?;
        let frames = frame_paths
            .iter()
            .map(|path| {
                std::fs::read(path)
                    .map(|bytes| frame_data_uri(&bytes))
                    .with_context(|| format!("read frame {}", path.display()))
            })
            .collect::<Result<Vec<_>>>()?;
        let transcript = if include_transcript {
            self.prompt_transcript(asset_id)?
        } else {
            None
        };
        let transcript_used = transcript.as_ref().is_some_and(|value| {
            value.status == TranscriptStatus::Speech && !value.text.is_empty()
        });
        let context = PromptContext {
            frame_timestamps_ms: manifest.timestamps_ms.clone(),
            clip_duration_ms: (asset.duration_seconds.max(0.0) * 1_000.0).round() as u32,
            transcript,
            vocabulary: approved_vocabulary.to_vec(),
            trip_context: None,
        };
        self.validate_run_policy_asset(policy, asset_id)?;
        let outcome = run_vlm_with_cancel(provider, chain, &context, frames, is_cancelled)?;
        let mut stored = if let Some(observation) = &outcome.observation {
            StoredAnalysis::from_observation(asset_id, &outcome.model, observation)
        } else {
            StoredAnalysis {
                asset_id: asset_id.to_string(),
                recipe_key: FRAMEPACK_RECIPE_KEY.to_string(),
                prompt_version: VLM_PROMPT_VERSION.to_string(),
                provider: "openrouter".to_string(),
                model: outcome.model.clone(),
                status: if outcome.model.is_empty() {
                    AnalysisStatus::Failed
                } else {
                    AnalysisStatus::Rejected
                },
                reject_reason: outcome.reject_reason.clone(),
                usable: false,
                unusable_reason: None,
                best_moment_start_ms: None,
                best_moment_end_ms: None,
                thumbnail_timestamp_ms: None,
                camera_movement: CameraMovement::Other,
                ocr_text: Vec::new(),
                transcript_used,
                prompt_tokens: 0,
                completion_tokens: 0,
                cost_usd: 0.0,
                latency_ms: 0,
                attempts: outcome.attempts,
                attempted_models: outcome.attempted_models.clone(),
                usage_complete: outcome.usage_complete,
                created_at: now(),
            }
        };
        stored.transcript_used = transcript_used;
        stored.prompt_tokens = outcome.prompt_tokens;
        stored.completion_tokens = outcome.completion_tokens;
        stored.cost_usd = outcome.cost_usd;
        stored.latency_ms = outcome.latency_ms;
        stored.attempts = outcome.attempts;
        stored.attempted_models = outcome.attempted_models.clone();
        stored.usage_complete = outcome.usage_complete;

        if let Some(observation) = &outcome.observation {
            // Remove only labels whose local authorship side-table says this
            // stage created. Human labels are never part of this replacement.
            for label in ai_labels_for_asset(&self.connection()?, asset_id)? {
                self.remove_label(asset_id, &label)?;
            }
            clear_ai_labels(&self.connection()?, asset_id)?;
            set_title_and_description(
                &self.connection()?,
                asset_id,
                &observation.title,
                &observation.description,
            )?;
            let mut authored = Vec::new();
            for proposed in &observation.labels {
                self.add_label(asset_id, proposed)?;
                authored.push((proposed.clone(), proposed.clone()));
            }
            record_ai_labels(&self.connection()?, asset_id, &authored)?;
            if let Some(timestamp) = observation.thumbnail_timestamp_ms
                && let Some(source) = frame_paths.iter().find(|path| {
                    path.file_stem()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name == format!("f{timestamp:08}"))
                })
            {
                let destination = parent
                    .join("cache")
                    .join("thumbnails")
                    .join(asset_id)
                    .join("q10.v1.jpg");
                if let Some(_claim) =
                    self.try_claim_representation(asset_id, RepresentationKind::Thumbnail)?
                {
                    commit_thumbnail(&self.connection()?, asset_id, source, &destination)?;
                }
            }
        }
        save_analysis(&self.connection()?, &stored)?;
        Ok(outcome)
    }

    pub fn analysis_coverage(&self, project_id: &str) -> Result<AnalysisCoverage> {
        let connection = self.connection()?;
        let mut coverage = AnalysisCoverage {
            recipe_key: FRAMEPACK_RECIPE_KEY.to_string(),
            prompt_version: VLM_PROMPT_VERSION.to_string(),
            ..AnalysisCoverage::default()
        };
        for asset in self.list_assets(project_id)? {
            if asset.removed {
                continue;
            }
            coverage.total += 1;
            let current = match self.proxy_fingerprint(&asset.id) {
                Ok(fingerprint) => analysis_is_current(&connection, &asset.id, &fingerprint)?,
                Err(_) => false,
            };
            if !current {
                coverage.missing += 1;
                continue;
            }
            match analysis::vlm_store::load_analysis(&connection, &asset.id)? {
                Some(stored) => {
                    coverage.cost_usd += stored.cost_usd;
                    if stored.model == FALLBACK_MODEL {
                        coverage.fallback_served += 1;
                    }
                    match stored.status {
                        AnalysisStatus::Ok => coverage.ok += 1,
                        AnalysisStatus::Rejected => coverage.rejected += 1,
                        AnalysisStatus::Failed => coverage.failed += 1,
                    }
                }
                None => coverage.missing += 1,
            }
        }
        Ok(coverage)
    }

    pub fn analyze_missing(&self, project_id: &str, force: bool) -> Result<AnalyzeReport> {
        self.analyze_missing_with_progress(project_id, force, |_| ControlFlow::Continue(()))
    }

    pub fn analyze_missing_with_progress<F>(
        &self,
        project_id: &str,
        force: bool,
        on_progress: F,
    ) -> Result<AnalyzeReport>
    where
        F: FnMut(AnalyzeBatchProgress) -> ControlFlow<()>,
    {
        self.analyze_missing_with_options(
            project_id,
            &AnalysisRunOptions {
                force,
                ..AnalysisRunOptions::default()
            },
            on_progress,
        )
    }

    /// Runs the cloud analysis batch with explicit key, budget, and concurrency.
    ///
    /// An explicit adapter invocation is approval intent, but it is not a
    /// bypass: the core first materializes the exact concrete policy, records
    /// that approval, runs one bounded pilot, and only then admits the bulk
    /// batch. A caller may instead pass `policy_id` to reuse an already-approved
    /// and piloted policy.
    ///
    /// `on_progress` returning `ControlFlow::Break` stops the bulk batch;
    /// workers observe the stop between clips, so cancelling never abandons a
    /// request that has already been paid for.
    pub fn analyze_missing_with_options<F>(
        &self,
        project_id: &str,
        options: &AnalysisRunOptions,
        on_progress: F,
    ) -> Result<AnalyzeReport>
    where
        F: FnMut(AnalyzeBatchProgress) -> ControlFlow<()>,
    {
        self.analyze_missing_with_options_and_provider(project_id, options, None, on_progress)
    }

    /// Provider-injectable form of the default batch path. Tests and future
    /// adapters can prove that an explicit invocation without a policy id still
    /// materializes approval and runs a passed pilot before bulk transmission.
    pub(crate) fn analyze_missing_with_options_and_provider<F>(
        &self,
        project_id: &str,
        options: &AnalysisRunOptions,
        injected_provider: Option<&dyn VlmProvider>,
        mut on_progress: F,
    ) -> Result<AnalyzeReport>
    where
        F: FnMut(AnalyzeBatchProgress) -> ControlFlow<()>,
    {
        let mut prepared_options = None;
        let mut explicit_request = None;
        if let Some(policy_id) = &options.policy_id {
            prepared_options =
                Some(self.analysis_options_bound_to_policy(project_id, options, policy_id)?);
        } else {
            let requested_budget = resolve_analysis_budget(options)?;
            anyhow::ensure!(
                requested_budget.is_finite(),
                "an explicit finite analysis budget is required for paid analysis"
            );
            self.require_project(project_id)?;
            let (pending, skipped, force_overwrite_warnings) =
                self.analysis_pending_assets(project_id, options, false)?;
            if pending.is_empty() {
                return Ok(AnalyzeReport {
                    analyzed: 0,
                    skipped,
                    rejected: 0,
                    fallback_served: 0,
                    force_overwrite_warnings,
                    cost_usd: 0.0,
                    usage_complete: true,
                    cancelled: false,
                    budget_exhausted: false,
                    outcomes: Vec::new(),
                    failures: Vec::new(),
                });
            }

            let primary = options
                .primary_model
                .clone()
                .unwrap_or_else(|| DEFAULT_MODEL.to_string());
            let allowed_models = if options.allowed_models.is_empty() {
                vec![DEFAULT_MODEL.to_string(), FALLBACK_MODEL.to_string()]
            } else {
                options.allowed_models.clone()
            };
            let mut request = crate::RunPolicyRequest::observation(
                project_id.to_string(),
                pending.iter().map(|asset| asset.id.clone()).collect(),
                primary,
                allowed_models,
                options.allow_fallback.unwrap_or(true),
                requested_budget,
                options.force,
            );
            request.payload = options
                .payload
                .unwrap_or(crate::AnalysisPayload::FramesAndTranscript);
            request.include_project_vocabulary = options.include_project_vocabulary;
            explicit_request = Some(request);
        }

        let provider_owned;
        let provider: &dyn VlmProvider = match injected_provider {
            Some(provider) => provider,
            None => {
                let api_key = match options.api_key.clone() {
                    Some(key) => key,
                    None => {
                        anyhow::ensure!(
                            api_credential_present(),
                            "OpenRouter credentials are not available; no analysis request was made"
                        );
                        resolve_api_key()?
                    }
                };
                provider_owned = OpenRouterProvider::new(api_key)?;
                &provider_owned
            }
        };

        let policy_id = if let Some(request) = explicit_request {
            self.authorize_run_policy(&request)?;
            let mut cancelled = false;
            let pilot_result = self.run_run_policy_pilot_with_provider_and_cancel(
                &request,
                provider,
                false,
                || {
                    let mut estimate =
                        BatchEstimator::new(&[], 1, Default::default()).estimate(&[]);
                    estimate.confidence = "capability_pilot".into();
                    estimate.estimated_remaining_seconds = None;
                    cancelled |= on_progress(AnalyzeBatchProgress {
                        asset_id: None,
                        outcome: None,
                        estimate,
                    })
                    .is_break();
                    cancelled
                },
            );
            if cancelled {
                return Ok(AnalyzeReport::cancelled_before_work());
            }
            let pilot = pilot_result?;
            anyhow::ensure!(
                pilot.status == "passed",
                "capability pilot did not pass for this exact policy: {} ({}); no bulk requests were made",
                pilot.failure_code,
                pilot.validation.detail
            );
            self.analysis_options_bound_to_policy(project_id, options, &pilot.policy_id)?
                .policy_id
                .expect("bound policy id")
        } else {
            prepared_options
                .expect("policy-bound branch prepared options")
                .policy_id
                .expect("bound policy id")
        };

        // Route through the shared policy-bound run-accounting wrapper so both
        // explicit and pre-authorized invocations record a bulk run with the
        // same policy provenance.
        self.analyze_authorized_with_provider_and_workers(
            &policy_id,
            provider,
            options.worker_count,
            &mut on_progress,
        )
    }

    /// Validates caller-supplied flags against the stored policy and replaces
    /// them with the policy's authoritative scope, model, payload/context and
    /// immutable whole-policy budget. Model, payload, scope or budget approval
    /// for one run can never be broadened by a caller argument.
    pub(crate) fn analysis_options_bound_to_policy(
        &self,
        project_id: &str,
        options: &AnalysisRunOptions,
        policy_id: &str,
    ) -> Result<AnalysisRunOptions> {
        let state =
            self.authorized_policy_for_bulk(policy_id, crate::AnalysisRunKind::Observation)?;
        let policy = &state.authorization.policy;
        anyhow::ensure!(
            policy.project_id == project_id,
            "authorized analysis policy belongs to a different project"
        );
        anyhow::ensure!(
            policy.payload.uses_frames(),
            "authorized analysis policy does not select frames"
        );
        if let Some(requested) = options.primary_model.as_deref() {
            anyhow::ensure!(
                requested == policy.primary_model,
                "requested model does not match the authorized policy"
            );
        }
        if !options.allowed_models.is_empty() {
            anyhow::ensure!(
                options.allowed_models == policy.allowed_models,
                "requested model allowlist does not match the authorized policy"
            );
        }
        if let Some(requested) = options.budget_usd {
            anyhow::ensure!(
                (requested - policy.budget_usd).abs() < f64::EPSILON,
                "requested budget does not match the authorized policy"
            );
        }
        if let Some(requested) = options.allow_fallback {
            anyhow::ensure!(
                requested == policy.allow_fallback,
                "requested fallback policy does not match the authorized policy"
            );
        }
        if let Some(requested) = &options.selected_assets {
            let authorized = policy
                .scope
                .iter()
                .map(|asset| asset.asset_id.clone())
                .collect::<Vec<_>>();
            anyhow::ensure!(
                requested == &authorized,
                "requested asset scope does not exactly match the authorized policy"
            );
        }
        if let Some(payload) = options.payload {
            anyhow::ensure!(
                payload == policy.payload,
                "requested payload does not match the authorized policy"
            );
        }
        let mut effective = options.clone();
        effective.budget_usd = Some(policy.budget_usd);
        effective.primary_model = Some(policy.primary_model.clone());
        effective.allowed_models = policy.allowed_models.clone();
        effective.allow_fallback = Some(policy.allow_fallback);
        effective.selected_assets = Some(
            policy
                .scope
                .iter()
                .map(|asset| asset.asset_id.clone())
                .collect(),
        );
        effective.payload = Some(policy.payload);
        effective.include_project_vocabulary = policy.context.project_vocabulary;
        effective.policy_id = Some(policy_id.to_string());
        Ok(effective)
    }

    /// Pending asset selection shared by policy materialization and execution,
    /// so the approved IDs are the IDs the batch can actually consider.
    pub(crate) fn analysis_pending_assets(
        &self,
        project_id: &str,
        options: &AnalysisRunOptions,
        allow_rejected_recovery: bool,
    ) -> Result<(Vec<Asset>, usize, usize)> {
        let force = options.force;
        let selected = options.selected_assets.as_ref().map(|ids| {
            ids.iter()
                .cloned()
                .collect::<std::collections::HashSet<_>>()
        });
        let connection = self.connection()?;
        let recovery_source_by_asset = if allow_rejected_recovery {
            options
                .policy_id
                .as_deref()
                .map(|policy_id| self.load_run_authorization(policy_id))
                .transpose()?
                .map(|authorization| {
                    authorization
                        .policy
                        .scope
                        .into_iter()
                        .map(|scope| (scope.asset_id, scope.source_identity))
                        .collect::<std::collections::HashMap<_, _>>()
                })
                .unwrap_or_default()
        } else {
            std::collections::HashMap::new()
        };
        let mut skipped = 0;
        let mut force_overwrite_warnings = 0;
        let mut pending = Vec::new();
        let mut selected_seen = std::collections::HashSet::new();
        for asset in self.list_assets(project_id)? {
            if let Some(selected) = &selected {
                if !selected.contains(&asset.id) {
                    continue;
                }
                selected_seen.insert(asset.id.clone());
            }
            if asset.removed {
                continue;
            }
            if self.asset_video_validation(&asset.id)?.status != AssetVideoValidationStatus::Usable
            {
                skipped += 1;
                continue;
            }
            let current = self
                .proxy_fingerprint(&asset.id)
                .ok()
                .is_some_and(|fingerprint| {
                    analysis_is_current(&connection, &asset.id, &fingerprint).unwrap_or(false)
                });
            // Ordinary Analyze Missing treats every paid result as present,
            // including rejected responses. Explicit recovery is eligible only
            // under a changed primary model and a policy/source pair with no
            // prior dispatched ledger attempt. This also protects fallback
            // rejections whose stored serving model differs from the primary.
            let rejected_under_changed_model = allow_rejected_recovery
                && current
                && options
                    .primary_model
                    .as_deref()
                    .is_some_and(|requested_model| {
                        load_analysis(&connection, &asset.id)
                            .ok()
                            .flatten()
                            .is_some_and(|stored| {
                                stored.status == AnalysisStatus::Rejected
                                    && stored.model != requested_model
                                    && recovery_source_by_asset
                                        .get(&asset.id)
                                        .is_some_and(|source_identity| {
                                            !connection
                                                .query_row(
                                                    "SELECT EXISTS(SELECT 1 FROM analysis_request_reservations WHERE project_id=?1 AND policy_id=?2 AND operation IN ('observation','moment_discovery') AND source_fingerprint=?3 AND lifecycle IN ('dispatched','completed','uncertain'))",
                                                    rusqlite::params![project_id, options.policy_id.as_deref().unwrap_or_default(), source_identity],
                                                    |row| row.get::<_, bool>(0),
                                                )
                                                .unwrap_or(true)
                                        })
                            })
                    });
            if current && !force && !rejected_under_changed_model {
                skipped += 1;
                continue;
            }
            if force && current && (!asset.title.is_empty() || !asset.description.is_empty()) {
                force_overwrite_warnings += 1;
            }
            pending.push(asset);
        }
        if let Some(selected) = &selected {
            anyhow::ensure!(
                selected_seen.len() == selected.len(),
                "authorized analysis scope contains assets outside this project"
            );
        }
        Ok((pending, skipped, force_overwrite_warnings))
    }

    /// Provider-injectable policy-bound execution boundary used by adapters and
    /// synthetic tests. The stored policy, not caller-supplied flags, determines
    /// the exact asset scope, allowlist, payload and remaining budget.
    pub fn analyze_authorized_with_provider<F>(
        &self,
        policy_id: &str,
        provider: &dyn crate::VlmProvider,
        on_progress: F,
    ) -> Result<AnalyzeReport>
    where
        F: FnMut(AnalyzeBatchProgress) -> ControlFlow<()>,
    {
        self.analyze_authorized_with_provider_and_workers(policy_id, provider, None, on_progress)
    }

    pub(crate) fn analyze_authorized_with_provider_and_workers<F>(
        &self,
        policy_id: &str,
        provider: &dyn crate::VlmProvider,
        worker_count: Option<usize>,
        mut on_progress: F,
    ) -> Result<AnalyzeReport>
    where
        F: FnMut(AnalyzeBatchProgress) -> ControlFlow<()>,
    {
        let state =
            self.authorized_policy_for_bulk(policy_id, crate::AnalysisRunKind::Observation)?;
        let policy = &state.authorization.policy;
        // Verify the same frozen whole-policy authority before opening an
        // accounted run row. The live remaining balance is an exposure
        // projection and is never passed downstream as a new ceiling.
        crate::analysis::run_policy::verify_bulk_authority(self, policy)?;
        let force = matches!(
            policy.parameters,
            crate::RunPolicyParameters::Observation { force: true }
        );
        let options = AnalysisRunOptions {
            force,
            worker_count,
            budget_usd: Some(policy.budget_usd),
            primary_model: Some(policy.primary_model.clone()),
            allowed_models: policy.allowed_models.clone(),
            allow_fallback: Some(policy.allow_fallback),
            selected_assets: Some(
                policy
                    .scope
                    .iter()
                    .map(|asset| asset.asset_id.clone())
                    .collect(),
            ),
            payload: Some(policy.payload),
            include_project_vocabulary: policy.context.project_vocabulary,
            policy_id: Some(policy_id.to_string()),
            ..AnalysisRunOptions::default()
        };
        let run_id =
            self.begin_accounted_analysis_run(&policy.project_id, policy_id, "analysis")?;
        let result = self.analyze_missing_with_provider(
            policy.project_id.as_str(),
            &options,
            provider,
            &run_id,
            &mut on_progress,
        );
        match result {
            Ok(mut report) => {
                let projection = self.analysis_accounting_aggregate_for_run(
                    policy_id,
                    policy.kind.as_str(),
                    &run_id,
                )?;
                report.cost_usd = projection.confirmed_cost_usd;
                report.usage_complete = !projection.has_unresolved_usage();
                let status = if report.cancelled {
                    "cancelled"
                } else if report.failures.is_empty() && report.usage_complete {
                    "completed"
                } else {
                    "failed"
                };
                self.finish_analysis_run(
                    &run_id,
                    &AnalysisRunOutcome {
                        status: status.into(),
                        stage_reached: "analysis".into(),
                        clips_analyzed: report.analyzed,
                        cost_usd: projection.confirmed_cost_usd,
                        budget_exhausted: report.budget_exhausted,
                        error: if report.failures.is_empty() {
                            String::new()
                        } else {
                            format!(
                                "analysis batch completed with {} failures",
                                report.failures.len()
                            )
                        },
                        ..Default::default()
                    },
                )?;
                Ok(report)
            }
            Err(error) => {
                let cost = self
                    .analysis_accounting_aggregate_for_run(policy_id, policy.kind.as_str(), &run_id)
                    .map(|projection| projection.confirmed_cost_usd)
                    .unwrap_or(0.0);
                let _ = self.finish_analysis_run(
                    &run_id,
                    &AnalysisRunOutcome {
                        status: "failed".into(),
                        stage_reached: "analysis".into(),
                        cost_usd: cost,
                        error: "analysis batch failed before completion".into(),
                        ..Default::default()
                    },
                );
                Err(error)
            }
        }
    }

    pub(crate) fn analyze_missing_with_provider<F>(
        &self,
        project_id: &str,
        options: &AnalysisRunOptions,
        provider: &dyn VlmProvider,
        run_id: &str,
        on_progress: &mut F,
    ) -> Result<AnalyzeReport>
    where
        F: FnMut(AnalyzeBatchProgress) -> ControlFlow<()>,
    {
        let policy_id = options.policy_id.as_deref().ok_or_else(|| {
            anyhow!("paid analysis must be bound to an authorized, piloted run policy")
        })?;
        let state =
            self.authorized_policy_for_bulk(policy_id, crate::AnalysisRunKind::Observation)?;
        let policy = &state.authorization.policy;
        ensure!(
            policy.project_id == project_id,
            "authorized analysis policy belongs to a different project"
        );
        let authorized_scope = policy
            .scope
            .iter()
            .map(|asset| asset.asset_id.clone())
            .collect::<Vec<_>>();
        ensure!(
            options.selected_assets.as_ref() == Some(&authorized_scope),
            "authorized analysis scope does not exactly match the requested asset ids"
        );
        ensure!(
            options.primary_model.as_deref() == Some(policy.primary_model.as_str())
                && options.allowed_models == policy.allowed_models
                && options.allow_fallback == Some(policy.allow_fallback),
            "authorized analysis model allowlist does not match the requested run"
        );
        ensure!(
            options.payload == Some(policy.payload)
                && options.include_project_vocabulary == policy.context.project_vocabulary,
            "authorized analysis payload/context does not match the requested run"
        );
        ensure!(
            options.budget_usd == Some(policy.budget_usd),
            "authorized analysis budget does not match the frozen whole policy ceiling"
        );
        ensure!(
            options.force == policy.parameters.force(),
            "authorized analysis force setting does not match the policy"
        );
        // Local-only credential/client preparation, before any run row or paid
        // transmission. A missing credential therefore causes zero provider calls.
        provider.prepare()?;
        let (pending, skipped, force_overwrite_warnings) =
            self.analysis_pending_assets(project_id, options, true)?;

        // Materialize frame packs first so the estimator's remaining weights
        // are exact frame counts, not clip-count or byte-size guesses.
        let mut prepared = Vec::new();
        let mut failures = Vec::new();
        for asset in pending {
            match self.analysis_frame_pack(&asset) {
                Ok(manifest) => prepared.push((asset, manifest.timestamps_ms.len() as u64)),
                Err(error) => failures.push(IngestFailure {
                    path: asset.path,
                    error: format!("{error:#}"),
                }),
            }
        }
        let task_work = prepared
            .iter()
            .map(|(_, frames)| *frames)
            .collect::<Vec<_>>();
        let worker_count = options
            .worker_count
            .or_else(|| {
                std::env::var("RCDVLOG_VLM_WORKERS")
                    .ok()
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .unwrap_or(MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY)
            .clamp(1, prepared.len().max(1))
            .min(MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY);
        let task_key = format!(
            "vlm.observe-v2.{}-w{worker_count}-v1",
            options
                .primary_model
                .as_deref()
                .unwrap_or(DEFAULT_MODEL)
                .replace('/', "-")
        );
        let mut estimator = BatchEstimator::new(
            &task_work,
            worker_count,
            self.load_batch_history(&task_key)?,
        );
        let start = on_progress(AnalyzeBatchProgress {
            asset_id: None,
            outcome: None,
            estimate: estimator.estimate(&task_work),
        });
        let mut report = AnalyzeReport {
            analyzed: 0,
            skipped,
            rejected: 0,
            fallback_served: 0,
            force_overwrite_warnings,
            cost_usd: 0.0,
            usage_complete: true,
            cancelled: start.is_break(),
            budget_exhausted: false,
            outcomes: Vec::new(),
            failures,
        };
        if report.cancelled {
            return Ok(report);
        }
        let budget_usd = policy.budget_usd;
        let policy_scope_id = analysis_policy_budget_scope_id(project_id, policy_id);
        let queue = Arc::new(Mutex::new(
            prepared
                .into_iter()
                .enumerate()
                .map(|(index, (asset, frames))| (index, asset, frames))
                .collect::<VecDeque<_>>(),
        ));
        // Admission/budget exhaustion stops queue claims. Only an explicit
        // caller cancellation may abandon work that already reserved its cost.
        let stop_claiming = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let default_allowed = vec![DEFAULT_MODEL.to_string(), FALLBACK_MODEL.to_string()];
        let allowed_models = if options.allowed_models.is_empty() {
            &default_allowed
        } else {
            &options.allowed_models
        };
        let chain = ProviderChain::with_policy(
            options.primary_model.as_deref().unwrap_or(DEFAULT_MODEL),
            allowed_models,
            options.allow_fallback.unwrap_or(true),
        )?;
        let approved_vocabulary = self.run_policy_vocabulary(policy)?;
        let (sender, receiver) = mpsc::channel();
        let mut completed = vec![false; task_work.len()];
        thread::scope(|scope| -> Result<()> {
            for _ in 0..worker_count {
                let catalog = self.clone();
                let queue = Arc::clone(&queue);
                let stop_claiming = Arc::clone(&stop_claiming);
                let cancelled = Arc::clone(&cancelled);
                let sender = sender.clone();
                let worker_chain = chain.clone();
                let approved_vocabulary = &approved_vocabulary;
                scope.spawn(move || {
                    loop {
                        if stop_claiming.load(std::sync::atomic::Ordering::Relaxed) {
                            break;
                        }
                        let Some((index, asset, frames)) = queue
                            .lock()
                            .expect("VLM work queue lock poisoned")
                            .pop_front()
                        else {
                            break;
                        };
                        let started = Instant::now();
                        let include_transcript = options
                            .payload
                            .map(crate::AnalysisPayload::uses_transcript)
                            .unwrap_or(false);
                        let result = (|| -> Result<crate::AnalysisOutcome> {
                            let context =
                                crate::analysis::run_policy::bulk_request_accounting_context(
                                    policy,
                                    run_id,
                                    &asset.id,
                                    format!("{}:{}", policy.kind.as_str(), asset.id),
                                    false,
                                )?;
                            let accounted = AccountedVlmProvider::new(&catalog, context, provider);
                            catalog.analyze_asset_with_provider(
                                &asset.id,
                                &accounted,
                                &worker_chain,
                                include_transcript,
                                approved_vocabulary,
                                policy,
                                &mut || cancelled.load(std::sync::atomic::Ordering::Relaxed),
                            )
                        })();
                        if sender
                            .send((index, asset, frames, started.elapsed(), result))
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            }
            drop(sender);
            for (index, asset, frames, elapsed, result) in receiver {
                completed[index] = true;
                let remaining = task_work
                    .iter()
                    .enumerate()
                    .filter_map(|(index, weight)| (!completed[index]).then_some(*weight))
                    .collect::<Vec<_>>();
                // Captured before the failure arm moves the asset's path.
                let asset_id = Some(asset.id.clone());
                let flow = match result {
                    Ok(outcome) => {
                        estimator.record_completion(frames, elapsed);
                        report.analyzed += 1;
                        report.cost_usd += outcome.cost_usd;
                        report.usage_complete &= outcome.usage_complete;
                        if outcome.observation.is_none() {
                            report.rejected += 1;
                        }
                        if outcome.fell_back {
                            report.fallback_served += 1;
                        }
                        report.outcomes.push(outcome.clone());
                        on_progress(AnalyzeBatchProgress {
                            asset_id,
                            outcome: Some(outcome),
                            estimate: estimator.estimate(&remaining),
                        })
                    }
                    Err(error) => {
                        estimator.record_failure(frames);
                        let failure = ProviderFailure::from_anyhow(&error);
                        if failure.category == crate::ProviderFailureCategory::Cancelled {
                            report.cancelled = true;
                            cancelled.store(true, Ordering::Relaxed);
                            stop_claiming.store(true, Ordering::Relaxed);
                        } else if failure.category
                            == crate::ProviderFailureCategory::LocalBudgetExhausted
                        {
                            report.budget_exhausted = true;
                            stop_claiming.store(true, Ordering::Relaxed);
                        } else if failure.category == crate::ProviderFailureCategory::LocalAdmission
                        {
                            // Canonical admission denied this transmission. Stop
                            // claiming new work; the final canonical snapshot
                            // decides whether the denial was budget exhaustion.
                            stop_claiming.store(true, Ordering::Relaxed);
                        }
                        report.failures.push(IngestFailure {
                            path: asset.path,
                            error: format!("{error:#}"),
                        });
                        on_progress(AnalyzeBatchProgress {
                            asset_id,
                            outcome: None,
                            estimate: estimator.estimate(&remaining),
                        })
                    }
                };
                if flow.is_break() {
                    report.cancelled = true;
                    cancelled.store(true, Ordering::Relaxed);
                    stop_claiming.store(true, Ordering::Relaxed);
                }
                let budget_stop = self
                    .analysis_budget_snapshot(&policy_scope_id)?
                    .is_some_and(|snapshot| {
                        snapshot.dispatch_stopped || snapshot.available_usd <= 1e-12
                    });
                if budget_stop {
                    report.budget_exhausted = true;
                    stop_claiming.store(true, Ordering::Relaxed);
                }
            }
            Ok(())
        })?;
        // Use the canonical current-run projection for the report. Reported
        // provider cost remains in outcomes; this summary is confirmed ledger
        // cost and never invents a charge from a reservation or estimate.
        let projection =
            self.analysis_accounting_aggregate_for_run(policy_id, policy.kind.as_str(), run_id)?;
        report.cost_usd = projection.confirmed_cost_usd;
        report.usage_complete = !projection.has_unresolved_usage();
        if report.budget_exhausted {
            let aggregate = self.analysis_accounting_aggregate(policy_id, Some(run_id))?;
            report.failures.push(IngestFailure {
                path: project_id.to_string(),
                error: format!(
                    "analysis budget exhausted; confirmed ${:.6}, reserved ${:.6}, uncertain ${:.6}, available ${:.6} (whole-policy limit ${budget_usd:.2})",
                    aggregate.confirmed_cost_usd,
                    aggregate.active_reserved_cost_usd,
                    aggregate.uncertain_usage_reserve_usd,
                    (budget_usd - aggregate.committed_or_reserved_usd).max(0.0)
                ),
            });
        }
        self.save_batch_history(&task_key, estimator.history())?;
        Ok(report)
    }
}
