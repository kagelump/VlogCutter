//! Operator-facing recovery slice over the canonical analysis accounting owner.
//!
//! This module adds no database, scheduler or retry framework. It provides
//! exact-scope inspection, typed receipt/no-upload/uncertain reconciliation,
//! conservative pilot-projection refresh and one explicitly authorized targeted
//! retry through the existing `AccountedVlmProvider` lifecycle. Prompt/frame
//! content is never persisted; an operator-supplied original payload file is
//! only fingerprinted in canonical state and returned provider body text stays
//! in the command result.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, anyhow, ensure};
use serde::{Deserialize, Serialize};

use super::AccountedVlmProvider;
use super::model::{
    AnalysisAccountingAggregate, AnalysisBudgetAuthorityContext, AnalysisProviderReceipt,
    AnalysisRequestEvent, AnalysisRequestReconciliation, AnalysisRequestRecord,
    AnalysisRetryAuthorization, AnalysisTargetedRetryAdmission,
    MAX_PROVIDER_RECEIPT_EVIDENCE_BYTES, MAX_TARGETED_RETRY_PAYLOAD_BYTES,
    OPERATION_REQUESTED_PROVIDER, ProviderReceiptEvidence, analysis_operation_budget_scope_id,
    analysis_request_cost_bound_fingerprint, analysis_request_payload_fingerprint,
};
use super::store;
use crate::Catalog;
use crate::analysis::provider::{
    ProviderFailure, VlmProvider, VlmRequest, VlmResponse, analysis_cost_bound_contract_id,
};
use crate::analysis::run_policy::{PilotValidation, RunPilotRecord, pilot_authority_context};

pub const ANALYSIS_RECOVERY_SCHEMA_VERSION: &str = "analysis.recovery.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisAccountingInspectionSelection {
    pub project_id: String,
    pub policy_id: Option<String>,
    pub operation: Option<String>,
    pub run_id: Option<String>,
    pub request_key: Option<String>,
    pub limit: usize,
    pub offset: usize,
    pub matching_request_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisAccountingRequestInspection {
    pub request: AnalysisRequestRecord,
    pub events: Vec<AnalysisRequestEvent>,
    pub receipts: Vec<AnalysisProviderReceipt>,
    /// Every epoch for the same immutable attempt base, oldest first. Retry
    /// authorization events remain visible in `events`.
    pub attempt_history: Vec<AnalysisRequestRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisAccountingInspection {
    pub schema_version: String,
    pub selection: AnalysisAccountingInspectionSelection,
    /// Canonical budget rows for the selected scope. Confirmed, active and
    /// unknown amounts remain distinct fields in every snapshot.
    pub budgets: Vec<super::model::AnalysisBudgetSnapshot>,
    pub requests: Vec<AnalysisAccountingRequestInspection>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisRequestRecoveryReport {
    pub schema_version: String,
    pub project_id: String,
    pub request: AnalysisRequestRecord,
    pub events: Vec<AnalysisRequestEvent>,
    pub receipts: Vec<AnalysisProviderReceipt>,
    /// A refreshed existing pilot projection when this reconciliation belonged
    /// to the currently recorded pilot run. Never a newly invented capability.
    pub pilot: Option<RunPilotRecord>,
    pub warning: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetedAnalysisRetryProviderResponse {
    /// Returned only in this operator-facing result. It is never persisted in
    /// the accounting ledger, event history, run summary or diagnostics.
    pub body: String,
    pub requested_model: String,
    pub actual_model: Option<String>,
    pub actual_provider: Option<String>,
    pub provider_generation_id: Option<String>,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub reported_cost_usd: f64,
    pub latency_ms: u64,
    pub usage_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetedAnalysisRetryReport {
    pub schema_version: String,
    pub project_id: String,
    pub prior_request_key: String,
    pub expected_next_attempt_epoch: u32,
    pub request: AnalysisRequestRecord,
    /// Exact current-run projection (confirmed/active/unknown separately).
    pub accounting: AnalysisAccountingAggregate,
    /// Whole original authority projection, including every retained prior
    /// attempt and the current retry. This is where old uncertainty remains
    /// visible after a newer attempt completes.
    pub budget: super::model::AnalysisBudgetSnapshot,
    pub receipts: Vec<AnalysisProviderReceipt>,
    pub events: Vec<AnalysisRequestEvent>,
    pub attempt_history: Vec<AnalysisRequestRecord>,
    pub provider_response: Option<TargetedAnalysisRetryProviderResponse>,
    /// Safe bounded provider failure category, if the one transmission ended
    /// without a returned response.
    pub provider_error_code: Option<String>,
    /// A targeted retry is accounting evidence only; domain validation,
    /// model-body application and human-visible review remain separate actions.
    pub review_required: bool,
}

struct PreparedTargetedAnalysisRetry {
    prior: AnalysisRequestRecord,
    policy_id: String,
    operation: String,
    model: String,
    payload_fingerprint: String,
    authority_context: AnalysisBudgetAuthorityContext,
}

fn read_bounded_file(path: &Path, limit: u64, description: &str) -> Result<Vec<u8>> {
    let file = File::open(path).map_err(|_| anyhow!("{description} is not readable"))?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow!("{description} could not be read"))?;
    ensure!(
        bytes.len() as u64 <= limit,
        "{description} exceeds the supported byte bound"
    );
    Ok(bytes)
}

/// Read an operator-supplied original `VlmRequest` JSON file under an explicit
/// bound. A missing original cannot be reconstructed from a stored digest.
pub fn read_targeted_retry_payload_file(path: &Path) -> Result<VlmRequest> {
    let bytes = read_bounded_file(
        path,
        MAX_TARGETED_RETRY_PAYLOAD_BYTES,
        "targeted retry payload file",
    )?;
    serde_json::from_slice(&bytes)
        .map_err(|_| anyhow!("targeted retry payload file is not a typed VlmRequest JSON document"))
}

/// Read trusted, user-provided provider receipt evidence as typed JSON. This
/// helper never contacts a provider and never re-sends a request.
pub fn read_provider_receipt_evidence_file(path: &Path) -> Result<ProviderReceiptEvidence> {
    let bytes = read_bounded_file(
        path,
        MAX_PROVIDER_RECEIPT_EVIDENCE_BYTES,
        "provider receipt evidence file",
    )?;
    let evidence: ProviderReceiptEvidence = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow!("receipt evidence file is not typed provider receipt JSON"))?;
    evidence.validate()?;
    Ok(evidence)
}

fn pilot_validation_after_reconciliation(running: &RunPilotRecord) -> PilotValidation {
    let mut validation = running.validation.clone();
    if running.status == "running" {
        // A process that crashed while the pilot was running has no persisted
        // domain-validation conclusion. Receipt reconciliation cannot invent
        // schema/semantic capability success or preserve a provisional pass.
        validation.structural_schema_valid = false;
        validation.source_time_semantics_valid = false;
        validation.capability_observed = false;
        validation.detail = "interrupted_pilot_reconciled_requires_review".into();
    }
    validation
}

impl Catalog {
    /// Inspect canonical accounting state using exact SQL scope filters.
    #[allow(clippy::too_many_arguments)]
    pub fn inspect_analysis_accounting(
        &self,
        project_id: &str,
        policy_id: Option<&str>,
        operation: Option<&str>,
        run_id: Option<&str>,
        request_key: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<AnalysisAccountingInspection> {
        ensure!(
            !project_id.trim().is_empty(),
            "analysis inspection project is required"
        );
        ensure!(
            (1..=1000).contains(&limit),
            "analysis inspection limit must be 1..=1000"
        );
        ensure!(
            offset <= 1_000_000,
            "analysis inspection offset is oversized"
        );
        for (value, label) in [
            (policy_id, "policy"),
            (operation, "operation"),
            (run_id, "run"),
            (request_key, "request"),
        ] {
            if let Some(value) = value {
                ensure!(
                    !value.trim().is_empty(),
                    "analysis inspection {label} filter must not be empty"
                );
            }
        }
        let mut database = self.connection()?;
        // A single SQLite read snapshot keeps requests, receipts and budget
        // totals consistent even while another process settles usage.
        let connection =
            database.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
        let requests = store::scoped_requests(
            &connection,
            project_id,
            policy_id,
            operation,
            run_id,
            request_key,
            limit,
            offset,
        )?;
        if request_key.is_some() {
            ensure!(
                !requests.is_empty(),
                "analysis request not found for this project and selection"
            );
        }
        let matching_request_count = store::scoped_request_count(
            &connection,
            project_id,
            policy_id,
            operation,
            run_id,
            request_key,
        )?;
        let mut scopes = if request_key.is_some() || run_id.is_some() {
            store::budget_scope_ids_for_request_selection(
                &connection,
                project_id,
                policy_id,
                operation,
                run_id,
                request_key,
            )?
        } else {
            store::budget_scope_ids_for_selection(&connection, project_id, policy_id, operation)?
        };
        scopes.extend(
            requests
                .iter()
                .map(|request| request.budget_scope_id.clone()),
        );
        scopes.sort();
        scopes.dedup();
        let budgets = scopes
            .iter()
            .map(|scope| {
                store::budget_snapshot(&connection, scope)?.ok_or_else(|| {
                    anyhow!("analysis budget authority {scope} disappeared during inspection")
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let mut details = Vec::with_capacity(requests.len());
        for request in requests {
            let events = store::events(&connection, &request.request_key)?;
            let receipts = store::receipts(&connection, &request.request_key)?;
            let attempt_history = store::attempt_history(&connection, &request.request_key)?;
            details.push(AnalysisAccountingRequestInspection {
                request,
                events,
                receipts,
                attempt_history,
            });
        }
        let inspection = AnalysisAccountingInspection {
            schema_version: ANALYSIS_RECOVERY_SCHEMA_VERSION.into(),
            selection: AnalysisAccountingInspectionSelection {
                project_id: project_id.to_string(),
                policy_id: policy_id.map(str::to_string),
                operation: operation.map(str::to_string),
                run_id: run_id.map(str::to_string),
                request_key: request_key.map(str::to_string),
                limit,
                offset,
                matching_request_count,
            },
            budgets,
            requests: details,
        };
        connection.commit()?;
        Ok(inspection)
    }

    /// Reconcile exactly one interrupted request after validating project
    /// ownership. Late provider evidence is imported as immutable receipt JSON;
    /// a no-upload release is accepted only while the dispatch marker is absent;
    /// an unknown outcome remains an honest reserve.
    pub fn reconcile_analysis_request_for_project(
        &self,
        project_id: &str,
        request_key: &str,
        evidence: &AnalysisRequestReconciliation,
    ) -> Result<AnalysisRequestRecoveryReport> {
        ensure!(
            !project_id.trim().is_empty() && !request_key.trim().is_empty(),
            "reconciliation project and request are required"
        );
        let current = self
            .analysis_request(request_key)?
            .ok_or_else(|| anyhow!("analysis request not found"))?;
        ensure!(
            current.project_id == project_id,
            "analysis request does not belong to this project"
        );
        let request = match evidence {
            AnalysisRequestReconciliation::LateReceipt { receipt } => {
                self.record_analysis_request_receipt(request_key, receipt)?
            }
            AnalysisRequestReconciliation::LocalNoUpload { failure_code } => {
                let safe = super::model::bounded_failure_code(failure_code);
                ensure!(
                    safe.starts_with("local_") || safe == "provider_configuration",
                    "local no-upload reconciliation requires typed local failure evidence"
                );
                self.release_analysis_request_local_failure(request_key, &safe)?
            }
            AnalysisRequestReconciliation::UncertainOutcome { failure_code } => {
                self.mark_analysis_request_uncertain(request_key, failure_code)?
            }
        };
        let pilot = self.refresh_pilot_projection_after_reconciliation(&request)?;
        let events = self.analysis_request_events(request_key)?;
        let receipts = self.analysis_provider_receipts(request_key)?;
        Ok(AnalysisRequestRecoveryReport {
            schema_version: ANALYSIS_RECOVERY_SCHEMA_VERSION.into(),
            project_id: project_id.to_string(),
            request,
            events,
            receipts,
            pilot,
            warning: "Reconciliation updates accounting evidence only; it does not validate the provider body, apply model output, rerun a pilot or claim a capability pass.".into(),
        })
    }

    /// Validate project ownership before durably authorizing one particular
    /// prior request's next bounded attempt epoch.
    pub fn authorize_analysis_request_retry_for_project(
        &self,
        project_id: &str,
        request_key: &str,
    ) -> Result<AnalysisRetryAuthorization> {
        ensure!(
            !project_id.trim().is_empty() && !request_key.trim().is_empty(),
            "retry authorization project and request are required"
        );
        let request = self
            .analysis_request(request_key)?
            .ok_or_else(|| anyhow!("analysis request not found"))?;
        ensure!(
            request.project_id == project_id,
            "analysis request does not belong to this project"
        );
        self.authorize_analysis_request_retry(request_key)
    }

    fn refresh_pilot_projection_after_reconciliation(
        &self,
        reconciled: &AnalysisRequestRecord,
    ) -> Result<Option<RunPilotRecord>> {
        if reconciled.operation != "pilot" || reconciled.policy_id.is_empty() {
            return Ok(None);
        }
        let Some(existing) = self.load_run_pilot(&reconciled.policy_id)? else {
            return Ok(None);
        };
        // The pilot DTO is one whole-operation projection: every current and
        // historical pilot attempt plus frozen legacy pilot evidence. The
        // currently recorded run id supplies provenance only; a late receipt for
        // an older run must not erase the current run's metadata.
        let current_run_id =
            (!existing.run_id.trim().is_empty()).then_some(existing.run_id.as_str());
        let aggregate = self.analysis_accounting_aggregate_for_operation(
            &reconciled.policy_id,
            "pilot",
            current_run_id,
        )?;
        let current_run = current_run_id
            .map(|run_id| {
                self.analysis_accounting_aggregate_for_run(&reconciled.policy_id, "pilot", run_id)
            })
            .transpose()?;
        let ledger = self.analysis_operation_attempt_projection(&reconciled.policy_id, "pilot")?;
        let unresolved = aggregate.has_unresolved_usage();
        let validation = pilot_validation_after_reconciliation(&existing);
        let status = if unresolved {
            "uncertain"
        } else if validation.capability_observed
            && validation.structural_schema_valid
            && validation.source_time_semantics_valid
        {
            "passed"
        } else {
            "failed"
        };
        let failure_code = if status == "passed" {
            String::new()
        } else if unresolved {
            "reconciled_usage_unknown".into()
        } else {
            "reconciled_receipt_requires_review".into()
        };
        let warning = "Canonical accounting was reconciled. Domain validation and capability state remain unchanged unless an interrupted running pilot required review; no paid pilot rerun occurred. Legacy attempt/token counters retain known minima when a per-request historical breakdown is unavailable.";
        let (actual_model, provider, generation_id) = if reconciled.run_id == existing.run_id {
            (
                aggregate.current_run_actual_model.clone(),
                aggregate.current_run_actual_provider.clone(),
                aggregate.current_run_generation_id.clone(),
            )
        } else {
            (
                existing.actual_model.clone(),
                existing.provider.clone(),
                existing.generation_id.clone(),
            )
        };
        let mut attempted_models = existing.attempted_models.clone();
        for model in ledger.attempted_models {
            if !attempted_models.iter().any(|existing| existing == &model) {
                attempted_models.push(model);
            }
        }
        let record = RunPilotRecord {
            status: status.into(),
            finished_at: if existing.finished_at.is_empty() {
                crate::now()
            } else {
                existing.finished_at.clone()
            },
            actual_model,
            provider,
            generation_id,
            attempted_models,
            attempts: existing.attempts.max(ledger.actual_attempt_count),
            prompt_tokens: existing.prompt_tokens.max(ledger.prompt_tokens),
            completion_tokens: existing.completion_tokens.max(ledger.completion_tokens),
            cost_usd: aggregate.committed_or_reserved_usd,
            confirmed_cost_usd: aggregate.confirmed_cost_usd,
            uncertain_usage_reserve_usd: aggregate.uncertain_usage_reserve_usd,
            active_reserved_cost_usd: aggregate.active_reserved_cost_usd,
            usage_complete: !unresolved,
            failure_code,
            validation,
            warning: warning.into(),
            ..existing.clone()
        };
        let record_changed = record != existing;
        let connection = self.connection()?;
        let run: Option<(String, Option<String>, f64)> = if current_run_id.is_some() {
            Some(connection.query_row(
                "SELECT status,finished_at,cost_usd FROM analysis_runs WHERE id=?1",
                [&record.run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?)
        } else {
            None
        };
        let desired_run_status = if record.status == "passed" {
            "completed"
        } else {
            "failed"
        };
        let run_needs_refresh = match (run.as_ref(), current_run.as_ref()) {
            (Some(run), Some(current)) => {
                run.0 != desired_run_status
                    || run.1.is_none()
                    || (run.2 - current.confirmed_cost_usd).abs() > 1e-12
            }
            _ => false,
        };
        if !record_changed && !run_needs_refresh {
            return Ok(Some(record));
        }
        if record_changed && !self.save_pilot_if_unchanged(&existing, &record)? {
            // A concurrent pilot start/finish owns the newer projection.
            // Receipt settlement has already committed; never replace that
            // newer run's provenance with this stale projection.
            return self.load_run_pilot(&reconciled.policy_id);
        }
        if let Some(run_id) = current_run_id
            && (record_changed || run_needs_refresh)
        {
            self.finish_analysis_run(
                run_id,
                &crate::AnalysisRunOutcome {
                    status: desired_run_status.into(),
                    stage_reached: "pilot".into(),
                    // Historical run cost is exactly this run's confirmed
                    // projection; the whole-pilot total lives on the pilot DTO.
                    cost_usd: current_run
                        .as_ref()
                        .map_or(0.0, |run| run.confirmed_cost_usd),
                    error: if record.status == "passed" {
                        String::new()
                    } else if record.status == "uncertain" {
                        "reconciled pilot usage remains uncertain".into()
                    } else {
                        "reconciled pilot requires manual domain review".into()
                    },
                    ..Default::default()
                },
            )?;
        }
        Ok(Some(record))
    }

    fn current_targeted_retry_owner(
        &self,
        prior: &AnalysisRequestRecord,
        project_id: &str,
    ) -> Result<AnalysisBudgetAuthorityContext> {
        ensure!(
            prior.project_id == project_id,
            "targeted retry request is outside the authorized project"
        );
        if prior.policy_id.is_empty() {
            ensure!(
                matches!(
                    prior.operation.as_str(),
                    super::model::CHRONOLOGY_SUGGESTIONS_OPERATION
                        | super::model::LABEL_CONSOLIDATION_OPERATION
                ),
                "targeted retry for an operation-owned request requires a supported canonical operation"
            );
            let snapshot = self
                .analysis_budget_snapshot(&prior.budget_scope_id)?
                .ok_or_else(|| anyhow!("analysis budget authority not found for prior request"))?;
            ensure!(
                !snapshot.dispatch_stopped,
                "analysis budget authority dispatch is stopped: {}",
                snapshot.stop_reason
            );
            ensure!(
                snapshot.requested_provider == prior.provider,
                "targeted retry provider route differs from the immutable prior request"
            );
            ensure!(
                prior.budget_scope_id
                    == analysis_operation_budget_scope_id(&prior.project_id, &prior.operation),
                "targeted retry operation budget scope is not canonical"
            );
            return Ok(AnalysisBudgetAuthorityContext {
                project_id: prior.project_id.clone(),
                policy_id: String::new(),
                policy_fingerprint: prior.policy_fingerprint.clone(),
                operation: prior.operation.clone(),
                provider: prior.provider.clone(),
                budget_scope_id: prior.budget_scope_id.clone(),
                budget_usd: snapshot.budget_usd,
            });
        }

        let authorization = self.load_run_authorization(&prior.policy_id)?;
        let stored = authorization.policy;
        crate::analysis::run_policy::ensure_vlm_run_kind(stored.kind)?;
        ensure!(
            stored.project_id == prior.project_id,
            "targeted retry request is outside the authorized project"
        );
        ensure!(
            stored.provider == prior.provider,
            "targeted retry requested provider differs from the authorized policy"
        );
        ensure!(
            stored.allowed_models.contains(&prior.requested_model)
                || stored.primary_model == prior.requested_model,
            "targeted retry requested model is outside the authorized model allowlist"
        );
        ensure!(
            prior.operation == "pilot" || prior.operation == stored.kind.as_str(),
            "targeted retry operation differs from the authorized policy operation"
        );
        let current = self.describe_run_policy(&stored.request())?;
        ensure!(
            current.policy_id == prior.policy_id,
            "active policy, source, vocabulary, payload or provider contract changed; targeted retry is denied"
        );
        ensure!(
            current.readiness.ready_for_authorization,
            "authorized policy is no longer locally ready for a targeted retry"
        );
        ensure!(
            current.provider_cost_bound_contract == analysis_cost_bound_contract_id(),
            "provider cost-bound contract changed; targeted retry is denied"
        );
        let matching_scope = current
            .scope
            .iter()
            .filter(|scope| scope.source_identity == prior.source_fingerprint)
            .collect::<Vec<_>>();
        ensure!(
            matching_scope.len() == 1,
            "targeted retry source identity is no longer uniquely present in the authorized policy scope"
        );
        self.validate_run_policy_asset(&current, &matching_scope[0].asset_id)?;
        self.run_policy_vocabulary(&current)?;
        Ok(pilot_authority_context(&current))
    }

    fn prepare_targeted_analysis_retry(
        &self,
        project_id: &str,
        prior_request_key: &str,
        expected_next_attempt_epoch: u32,
        payload: &VlmRequest,
    ) -> Result<PreparedTargetedAnalysisRetry> {
        let connection = self.connection()?;
        let prior = store::validate_retry_authorization(
            &connection,
            project_id,
            prior_request_key,
            expected_next_attempt_epoch,
        )?;
        ensure!(
            prior.lifecycle.is_terminal(),
            "prior analysis request is not terminal; retry is not allowed"
        );
        let model = prior.requested_model.clone();
        let payload_fingerprint = analysis_request_payload_fingerprint(&model, payload)?;
        ensure!(
            payload_fingerprint == prior.payload_fingerprint,
            "targeted retry payload does not match the original request payload fingerprint"
        );
        let authority_context = self.current_targeted_retry_owner(&prior, project_id)?;

        Ok(PreparedTargetedAnalysisRetry {
            policy_id: prior.policy_id.clone(),
            operation: prior.operation.clone(),
            model,
            payload_fingerprint,
            authority_context,
            prior,
        })
    }

    fn targeted_retry_run_aggregate(
        &self,
        prepared: &PreparedTargetedAnalysisRetry,
        run_id: &str,
    ) -> Result<AnalysisAccountingAggregate> {
        if prepared.policy_id.is_empty() {
            self.analysis_accounting_aggregate_for_operation_run(
                &prepared.prior.project_id,
                &prepared.operation,
                run_id,
            )
        } else {
            self.analysis_accounting_aggregate_for_run(
                &prepared.policy_id,
                &prepared.operation,
                run_id,
            )
        }
    }

    fn dispatch_prepared_targeted_analysis_retry(
        &self,
        prepared: PreparedTargetedAnalysisRetry,
        payload: &VlmRequest,
        provider: &dyn VlmProvider,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<TargetedAnalysisRetryReport> {
        if is_cancelled() {
            return Err(ProviderFailure::cancelled().into());
        }
        // Provider-owned quote is local; prove it is byte-for-byte the same cost
        // contract as the immutable prior request before creating a new run.
        let cost_bound = provider
            .analysis_cost_bound(&prepared.model, payload)
            .context("load current provider cost bound for targeted retry")?;
        let cost_bound_fingerprint = analysis_request_cost_bound_fingerprint(&cost_bound)?;
        store::validate_targeted_retry_claim(
            &self.connection()?,
            &store::TargetedRetryClaim {
                project_id: &prepared.prior.project_id,
                prior_request_key: &prepared.prior.request_key,
                expected_next_attempt_epoch: prepared.prior.attempt_epoch + 1,
                run_id: None,
                model: &prepared.model,
                payload_fingerprint: &prepared.payload_fingerprint,
                cost_bound_fingerprint: &cost_bound_fingerprint,
            },
        )?;
        if is_cancelled() {
            return Err(ProviderFailure::cancelled().into());
        }

        // Authority initialization/verification must precede the new owned run
        // row. The whole original budget is reused; nothing resets or narrows to
        // the currently remaining amount.
        let authority =
            self.initialize_analysis_accounting_authority(&prepared.authority_context)?;
        ensure!(
            !authority.dispatch_stopped,
            "analysis budget authority dispatch is stopped: {}",
            authority.stop_reason
        );
        if is_cancelled() {
            return Err(ProviderFailure::cancelled().into());
        }
        let run_id = self.begin_accounted_analysis_run(
            &prepared.prior.project_id,
            &prepared.policy_id,
            "targeted_retry",
        )?;
        let admission = AnalysisTargetedRetryAdmission {
            project_id: prepared.prior.project_id.clone(),
            prior_request_key: prepared.prior.request_key.clone(),
            expected_next_attempt_epoch: prepared.prior.attempt_epoch + 1,
            run_id: run_id.clone(),
        };
        let proof_prior = prepared.prior.clone();
        let accounted = AccountedVlmProvider::new_targeted_retry(self, admission, provider)
            .with_pre_dispatch_check(move || {
                self.current_targeted_retry_owner(&proof_prior, &proof_prior.project_id)
                    .map(|_| ())
            });
        let dispatch = accounted.analyze_with_cancel(&prepared.model, payload, is_cancelled);
        let request_keys = store::request_keys_for_run(
            &self.connection()?,
            &prepared.prior.project_id,
            &prepared.operation,
            &run_id,
        )?;
        let cancelled = dispatch.as_ref().err().is_some_and(|error| {
            ProviderFailure::from_anyhow(error).category
                == crate::ProviderFailureCategory::Cancelled
        });
        let Some(request_key) = request_keys.first() else {
            // No reservation proves no paid request was admitted. Close the
            // empty audit run at confirmed zero, never with a reservation.
            self.finish_analysis_run(
                &run_id,
                &crate::AnalysisRunOutcome {
                    status: if cancelled { "cancelled" } else { "failed" }.into(),
                    stage_reached: "targeted_retry".into(),
                    cost_usd: 0.0,
                    error: "targeted retry was denied before admission".into(),
                    ..Default::default()
                },
            )?;
            return dispatch
                .map(|_| unreachable!("targeted retry without a canonical request row"));
        };
        let request = self
            .analysis_request(request_key)?
            .ok_or_else(|| anyhow!("targeted retry analysis request disappeared"))?;
        let (provider_response, provider_error_code) = match dispatch {
            Ok(response) => (Some(targeted_retry_provider_response(&response)), None),
            Err(error) => (
                None,
                Some(ProviderFailure::from_anyhow(&error).safe_detail()),
            ),
        };
        let aggregate = self.targeted_retry_run_aggregate(&prepared, &run_id)?;
        let budget = self
            .analysis_budget_snapshot(&request.budget_scope_id)?
            .ok_or_else(|| anyhow!("analysis budget authority disappeared after targeted retry"))?;
        let receipts = self.analysis_provider_receipts(request_key)?;
        let events = self.analysis_request_events(request_key)?;
        let attempt_history = self.analysis_attempt_history(request_key)?;
        self.finish_analysis_run(
            &run_id,
            &crate::AnalysisRunOutcome {
                status: if cancelled { "cancelled" } else { "failed" }.into(),
                stage_reached: "targeted_retry".into(),
                cost_usd: aggregate.confirmed_cost_usd,
                error: if provider_error_code.is_some() {
                    provider_error_code
                        .clone()
                        .unwrap_or_else(|| "targeted_retry_provider_error".into())
                } else {
                    "targeted_retry_response_requires_operator_review".into()
                },
                ..Default::default()
            },
        )?;
        Ok(TargetedAnalysisRetryReport {
            schema_version: ANALYSIS_RECOVERY_SCHEMA_VERSION.into(),
            project_id: prepared.prior.project_id.clone(),
            prior_request_key: prepared.prior.request_key.clone(),
            expected_next_attempt_epoch: prepared.prior.attempt_epoch + 1,
            request,
            accounting: aggregate,
            budget,
            receipts,
            events,
            attempt_history,
            provider_response,
            provider_error_code,
            review_required: true,
        })
    }

    /// Core provider-injected targeted-retry entry point. It performs exactly
    /// one transmission through the existing accounted boundary, after proving
    /// the expected epoch, durable authorization, original payload/model and
    /// current provider cost contract.
    pub fn dispatch_targeted_analysis_retry_with_provider(
        &self,
        project_id: &str,
        prior_request_key: &str,
        expected_next_attempt_epoch: u32,
        payload: &VlmRequest,
        provider: &dyn VlmProvider,
    ) -> Result<TargetedAnalysisRetryReport> {
        self.dispatch_targeted_analysis_retry_with_provider_and_cancel(
            project_id,
            prior_request_key,
            expected_next_attempt_epoch,
            payload,
            provider,
            || false,
        )
    }

    /// Cancellation-aware core entry point. Cancellation before local
    /// preparation creates no run; cancellation after reservation is released
    /// only while the durable dispatch marker is still absent. The one permitted
    /// transmission never has a fallback attempt.
    pub fn dispatch_targeted_analysis_retry_with_provider_and_cancel<F>(
        &self,
        project_id: &str,
        prior_request_key: &str,
        expected_next_attempt_epoch: u32,
        payload: &VlmRequest,
        provider: &dyn VlmProvider,
        mut is_cancelled: F,
    ) -> Result<TargetedAnalysisRetryReport>
    where
        F: FnMut() -> bool,
    {
        if is_cancelled() {
            return Err(ProviderFailure::cancelled().into());
        }
        let prepared = self.prepare_targeted_analysis_retry(
            project_id,
            prior_request_key,
            expected_next_attempt_epoch,
            payload,
        )?;
        if is_cancelled() {
            return Err(ProviderFailure::cancelled().into());
        }
        self.dispatch_prepared_targeted_analysis_retry(
            prepared,
            payload,
            provider,
            &mut is_cancelled,
        )
    }

    /// Production CLI wrapper. Cheap local ownership, authorization, policy,
    /// source, payload, epoch and route checks all run before credentials are
    /// resolved or a provider client is constructed.
    pub fn dispatch_targeted_analysis_retry(
        &self,
        project_id: &str,
        prior_request_key: &str,
        expected_next_attempt_epoch: u32,
        payload: &VlmRequest,
    ) -> Result<TargetedAnalysisRetryReport> {
        self.dispatch_targeted_analysis_retry_with_cancel(
            project_id,
            prior_request_key,
            expected_next_attempt_epoch,
            payload,
            || false,
        )
    }

    /// Cancellation-aware production entry point. The stored immutable request
    /// must target the actual OpenRouter route before any credentials or client
    /// are resolved.
    pub fn dispatch_targeted_analysis_retry_with_cancel<F>(
        &self,
        project_id: &str,
        prior_request_key: &str,
        expected_next_attempt_epoch: u32,
        payload: &VlmRequest,
        mut is_cancelled: F,
    ) -> Result<TargetedAnalysisRetryReport>
    where
        F: FnMut() -> bool,
    {
        if is_cancelled() {
            return Err(ProviderFailure::cancelled().into());
        }
        let prepared = self.prepare_targeted_analysis_retry(
            project_id,
            prior_request_key,
            expected_next_attempt_epoch,
            payload,
        )?;
        ensure!(
            prepared.prior.provider == OPERATION_REQUESTED_PROVIDER,
            "targeted retry requested provider route is not the configured OpenRouter route; no client was constructed"
        );
        if is_cancelled() {
            return Err(ProviderFailure::cancelled().into());
        }
        ensure!(
            crate::api_credential_present(),
            "OpenRouter credentials are not available; no targeted retry request was made"
        );
        let key = crate::resolve_api_key()
            .context("resolve OpenRouter credentials for targeted retry")?;
        let provider = crate::OpenRouterProvider::new(key)?;
        self.dispatch_prepared_targeted_analysis_retry(
            prepared,
            payload,
            &provider,
            &mut is_cancelled,
        )
    }
}

fn targeted_retry_provider_response(
    response: &VlmResponse,
) -> TargetedAnalysisRetryProviderResponse {
    TargetedAnalysisRetryProviderResponse {
        body: response.body.clone(),
        requested_model: response.model.clone(),
        actual_model: response.receipt.actual_model.clone(),
        actual_provider: response.receipt.provider.clone(),
        provider_generation_id: response.receipt.generation_id.clone(),
        prompt_tokens: response.prompt_tokens,
        completion_tokens: response.completion_tokens,
        reported_cost_usd: response.cost_usd,
        latency_ms: response.latency_ms,
        usage_complete: response.receipt.usage_complete,
    }
}
