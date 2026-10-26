//! Thin Catalog APIs for canonical accounting, including frozen legacy evidence.
//! Runtime transport and CLI integration remain separate changes. Runtime callers
//! must initialize the authority before creating their accounted run/pilot rows.

use anyhow::{Result, anyhow, ensure};
use rusqlite::params;
use serde_json::Value;

use super::model::{
    ANALYSIS_ACCOUNTING_SCHEMA_VERSION, AnalysisAccountingAggregate, AnalysisAccountingContext,
    AnalysisAccountingReport, AnalysisBudgetAuthorityContext, AnalysisBudgetSnapshot,
    AnalysisOperationAttemptProjection, AnalysisProviderReceipt, AnalysisRequestEvent,
    AnalysisRequestLifecycle, AnalysisRequestReconciliation, AnalysisRequestRecord,
    AnalysisRetryAuthorization, AnalysisTargetedRetryAdmission, ProviderReceiptEvidence,
    bounded_failure_code,
};
use super::store;
use crate::Catalog;
use crate::analysis::openrouter_stt::{
    NativeSttAudioWindow, NativeSttRequest, native_stt_request_cost_bound,
};
use crate::analysis::provider::AnalysisRequestCostBound;

impl Catalog {
    /// Freeze the canonical authority and its pre-ledger evidence. C must call
    /// this before creating the analysis run/pilot rows that represent the new
    /// runtime attempt, otherwise those fresh rows could be mistaken for legacy
    /// unknown usage. This does not dispatch or reserve a paid request.
    pub fn initialize_analysis_accounting_authority(
        &self,
        context: &AnalysisBudgetAuthorityContext,
    ) -> Result<AnalysisBudgetSnapshot> {
        store::initialize_authority(&mut self.connection()?, context)
    }

    /// Atomically reserve the complete conservative provider-owned bound before
    /// dispatch. Concurrent workers/processes serialize on SQLite `IMMEDIATE`;
    /// the bound is never clipped and a missing initialization fails closed.
    pub fn reserve_analysis_request(
        &self,
        context: &AnalysisAccountingContext,
        model: &str,
        payload_fingerprint: &str,
        cost_bound: &AnalysisRequestCostBound,
    ) -> Result<AnalysisRequestRecord> {
        context.validate_request(model, payload_fingerprint)?;
        cost_bound.validate()?;
        store::reserve(
            &mut self.connection()?,
            context,
            model,
            payload_fingerprint,
            cost_bound,
        )
    }

    /// Reserve one exact native speech window through the same canonical
    /// reservation transaction as VLM work. The owner-local quote and payload
    /// fingerprint helpers are called here; callers cannot substitute a chat
    /// token quote or transport-invented request identity.
    pub fn reserve_analysis_native_stt_request(
        &self,
        context: &AnalysisAccountingContext,
        request: &NativeSttRequest,
        audio: &NativeSttAudioWindow,
    ) -> Result<AnalysisRequestRecord> {
        let cost_bound = native_stt_request_cost_bound(request, audio)?;
        let payload_fingerprint = super::analysis_native_stt_payload_fingerprint(request, audio)?;
        context.validate_request(request.model(), &payload_fingerprint)?;
        store::reserve_native_stt(
            &mut self.connection()?,
            context,
            request,
            audio,
            &payload_fingerprint,
            &cost_bound,
        )
    }

    /// Read-only proof of a targeted retry's exact original payload/model and
    /// current provider quote against its durable prior authorization.
    pub(crate) fn validate_analysis_targeted_retry_claim(
        &self,
        admission: &AnalysisTargetedRetryAdmission,
        model: &str,
        payload_fingerprint: &str,
        cost_bound_fingerprint: &str,
        cost_bound: &AnalysisRequestCostBound,
    ) -> Result<()> {
        store::validate_targeted_retry_claim(
            &self.connection()?,
            &store::TargetedRetryClaim {
                project_id: &admission.project_id,
                prior_request_key: &admission.prior_request_key,
                expected_next_attempt_epoch: admission.expected_next_attempt_epoch,
                run_id: Some(&admission.run_id),
                model,
                payload_fingerprint,
                cost_bound_fingerprint,
                cost_bound,
            },
        )?;
        Ok(())
    }

    /// Atomically consume the particular durable retry authorization and append
    /// exactly the expected next epoch under the prior immutable attempt base.
    pub(crate) fn reserve_analysis_request_targeted_retry(
        &self,
        admission: &AnalysisTargetedRetryAdmission,
        model: &str,
        payload_fingerprint: &str,
        cost_bound: &AnalysisRequestCostBound,
    ) -> Result<AnalysisRequestRecord> {
        store::reserve_targeted_retry(
            &mut self.connection()?,
            admission,
            model,
            payload_fingerprint,
            cost_bound,
        )
    }

    /// Durable dispatch marker committed immediately before transport I/O.
    /// After this point a local failure code alone is never no-upload proof.
    pub fn mark_analysis_request_dispatched(
        &self,
        request_key: &str,
    ) -> Result<AnalysisRequestRecord> {
        store::mark_dispatched(&mut self.connection()?, request_key)
    }

    /// Release only while the durable dispatch marker is absent.
    pub fn release_analysis_request_before_dispatch(
        &self,
        request_key: &str,
        failure_code: &str,
    ) -> Result<AnalysisRequestRecord> {
        store::release_before_dispatch(&mut self.connection()?, request_key, failure_code)
    }

    /// Typed local-failure convenience. The absent dispatch marker is the actual
    /// no-upload proof; the local failure code is retained only as evidence.
    pub fn release_analysis_request_local_failure(
        &self,
        request_key: &str,
        failure_code: &str,
    ) -> Result<AnalysisRequestRecord> {
        let safe = bounded_failure_code(failure_code);
        ensure!(
            safe.starts_with("local_")
                || safe == "provider_configuration"
                || safe == "provider_cooldown",
            "local release requires typed local failure evidence"
        );
        self.release_analysis_request_before_dispatch(request_key, &safe)
    }

    /// Retain honest uncertainty when a request may have reached the provider
    /// but no complete receipt is available in this slice.
    pub fn mark_analysis_request_uncertain(
        &self,
        request_key: &str,
        failure_code: &str,
    ) -> Result<AnalysisRequestRecord> {
        store::mark_uncertain(&mut self.connection()?, request_key, failure_code)
    }

    /// Record immutable provider evidence. Exact duplicate evidence is a no-op;
    /// a complete receipt may correct a prior incomplete reported amount for the
    /// same known identities, while an already confirmed final charge is frozen.
    pub fn record_analysis_request_receipt(
        &self,
        request_key: &str,
        evidence: &ProviderReceiptEvidence,
    ) -> Result<AnalysisRequestRecord> {
        store::record_receipt(&mut self.connection()?, request_key, evidence)
    }

    /// Idempotent reconciliation entry point for interrupted/uncertain requests.
    /// A local no-upload release is only accepted when the durable dispatch
    /// marker is absent. Unknown outcome never releases a dispatched request.
    pub fn reconcile_analysis_request(
        &self,
        request_key: &str,
        evidence: &AnalysisRequestReconciliation,
    ) -> Result<AnalysisRequestRecord> {
        match evidence {
            AnalysisRequestReconciliation::LateReceipt { receipt } => {
                self.record_analysis_request_receipt(request_key, receipt)
            }
            AnalysisRequestReconciliation::LocalNoUpload { failure_code } => {
                self.release_analysis_request_before_dispatch(request_key, failure_code)
            }
            AnalysisRequestReconciliation::UncertainOutcome { failure_code } => {
                self.mark_analysis_request_uncertain(request_key, failure_code)
            }
        }
    }

    /// Explicitly authorize one bounded retry for this particular prior request
    /// and next epoch. Repeat authorization for the same epoch is idempotent.
    /// Authorization never erases prior receipts, charges or uncertainty.
    pub fn authorize_analysis_request_retry(
        &self,
        request_key: &str,
    ) -> Result<AnalysisRetryAuthorization> {
        store::authorize_retry(&mut self.connection()?, request_key)
    }

    pub fn analysis_request(&self, request_key: &str) -> Result<Option<AnalysisRequestRecord>> {
        store::load_request(&self.connection()?, request_key)
    }

    pub fn list_analysis_requests(
        &self,
        project_id: &str,
        policy_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AnalysisRequestRecord>> {
        ensure!(limit > 0 && limit <= 1000, "request limit must be 1..=1000");
        store::list_requests(&self.connection()?, project_id, policy_id, limit)
    }

    /// Unresolved attempts for one exact source/operation. Runtime callers use
    /// this to prevent a newly generated force-policy identity from bypassing a
    /// prior unknown paid attempt that still needs reconciliation/authorization.
    pub fn analysis_requests_needing_reconciliation_for_source(
        &self,
        project_id: &str,
        operation: &str,
        source_fingerprint: &str,
    ) -> Result<Vec<AnalysisRequestRecord>> {
        ensure!(
            !project_id.trim().is_empty()
                && !operation.trim().is_empty()
                && !source_fingerprint.trim().is_empty(),
            "project, operation and source identity are required"
        );
        store::unresolved_requests_for_source(
            &self.connection()?,
            project_id,
            operation,
            source_fingerprint,
        )
    }

    /// Prior unresolved native speech work for one exact source. This query
    /// spans pilot and native bulk operations and every policy identity, so a
    /// replacement/force policy cannot bypass unknown paid work.
    pub fn analysis_native_requests_needing_reconciliation_for_source(
        &self,
        project_id: &str,
        source_fingerprint: &str,
    ) -> Result<Vec<AnalysisRequestRecord>> {
        ensure!(
            !project_id.trim().is_empty() && !source_fingerprint.trim().is_empty(),
            "project and source identity are required"
        );
        store::native_unresolved_requests_for_source(
            &self.connection()?,
            project_id,
            source_fingerprint,
        )
    }

    pub(crate) fn analysis_request_keys_for_run(
        &self,
        project_id: &str,
        operation: &str,
        run_id: &str,
    ) -> Result<Vec<String>> {
        store::request_keys_for_run(&self.connection()?, project_id, operation, run_id)
    }

    /// Every immutable attempt under a request's `attempt_base_key`, oldest
    /// first. This is the prior-retry-epoch history used by inspection.
    pub fn analysis_attempt_history(
        &self,
        request_key: &str,
    ) -> Result<Vec<AnalysisRequestRecord>> {
        store::attempt_history(&self.connection()?, request_key)
    }

    pub fn analysis_request_events(&self, request_key: &str) -> Result<Vec<AnalysisRequestEvent>> {
        store::events(&self.connection()?, request_key)
    }

    /// Immutable provider receipt evidence for one request, oldest first.
    pub fn analysis_provider_receipts(
        &self,
        request_key: &str,
    ) -> Result<Vec<AnalysisProviderReceipt>> {
        store::receipts(&self.connection()?, request_key)
    }

    pub fn analysis_budget_snapshot(
        &self,
        budget_scope_id: &str,
    ) -> Result<Option<AnalysisBudgetSnapshot>> {
        store::budget_snapshot(&self.connection()?, budget_scope_id)
    }

    pub fn analysis_accounting_report(
        &self,
        project_id: &str,
        policy_id: Option<&str>,
        limit: usize,
    ) -> Result<AnalysisAccountingReport> {
        ensure!(limit > 0 && limit <= 1000, "request limit must be 1..=1000");
        let requests = self.list_analysis_requests(project_id, policy_id, limit)?;
        let connection = self.connection()?;
        let mut scopes = requests
            .iter()
            .map(|request| request.budget_scope_id.clone())
            .collect::<Vec<_>>();
        let mut statement = connection.prepare(
            "SELECT budget_scope_id FROM analysis_budget_authorities
             WHERE project_id=?1 AND (?2='' OR policy_id=?2)",
        )?;
        scopes.extend(
            statement
                .query_map(params![project_id, policy_id.unwrap_or("")], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
        scopes.sort();
        scopes.dedup();
        let budgets = scopes
            .iter()
            .map(|scope| {
                store::budget_snapshot(&connection, scope)?.ok_or_else(|| {
                    anyhow!("analysis budget authority {scope} disappeared during report")
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(AnalysisAccountingReport {
            schema_version: ANALYSIS_ACCOUNTING_SCHEMA_VERSION.to_string(),
            project_id: project_id.to_string(),
            policy_id: policy_id.map(str::to_string),
            budgets,
            requests,
        })
    }

    /// Whole-policy accounting projection. This includes every operation plus
    /// the frozen legacy evidence exactly once. Runtime callers that need a
    /// pilot-only projection must use `analysis_accounting_aggregate_for_operation`
    /// so bulk charges are not mixed into pilot cost.
    pub fn analysis_accounting_aggregate(
        &self,
        policy_id: &str,
        current_run_id: Option<&str>,
    ) -> Result<AnalysisAccountingAggregate> {
        self.analysis_accounting_aggregate_scoped(policy_id, None, current_run_id)
    }

    /// One-operation accounting projection (for example `Some("pilot")`).
    pub fn analysis_accounting_aggregate_for_operation(
        &self,
        policy_id: &str,
        operation: &str,
        current_run_id: Option<&str>,
    ) -> Result<AnalysisAccountingAggregate> {
        let operation = operation.trim();
        ensure!(
            !operation.is_empty(),
            "accounting aggregate operation is required"
        );
        self.analysis_accounting_aggregate_scoped(policy_id, Some(operation), current_run_id)
    }

    /// Canonical confirmed/active/uncertain projection for exactly one runtime
    /// run. Unlike the whole-policy projection this excludes frozen legacy and
    /// post-snapshot evidence; those remain visible on the authority aggregate
    /// and must not be presented as charges from the current run.
    pub fn analysis_accounting_aggregate_for_run(
        &self,
        policy_id: &str,
        operation: &str,
        run_id: &str,
    ) -> Result<AnalysisAccountingAggregate> {
        ensure!(
            !policy_id.trim().is_empty(),
            "accounting aggregate policy is required"
        );
        store::accounting_aggregate_for_run(&self.connection()?, policy_id, operation, run_id)
    }

    /// Whole-operation projection for the canonical non-policy authority. This
    /// includes frozen operation evidence only when `run_id_filter` is absent;
    /// a current-run summary passes the run id so legacy spending is never
    /// presented as a charge from this invocation.
    pub fn analysis_accounting_aggregate_for_operation_scope(
        &self,
        project_id: &str,
        operation: &str,
        current_run_id: Option<&str>,
        run_id_filter: Option<&str>,
    ) -> Result<AnalysisAccountingAggregate> {
        ensure!(
            !project_id.trim().is_empty(),
            "accounting aggregate project is required"
        );
        ensure!(
            !operation.trim().is_empty(),
            "accounting aggregate operation is required"
        );
        store::accounting_aggregate_for_operation_scope(
            &self.connection()?,
            project_id,
            operation,
            current_run_id,
            run_id_filter,
        )
    }

    /// Confirmed/reserved/uncertain projection for one operation-owned runtime
    /// run. Legacy and post-snapshot evidence remain visible on the operation
    /// authority rather than being attributed to this run.
    pub fn analysis_accounting_aggregate_for_operation_run(
        &self,
        project_id: &str,
        operation: &str,
        run_id: &str,
    ) -> Result<AnalysisAccountingAggregate> {
        ensure!(
            !project_id.trim().is_empty(),
            "accounting aggregate project is required"
        );
        ensure!(
            !operation.trim().is_empty(),
            "accounting aggregate operation is required"
        );
        store::accounting_aggregate_for_operation_run(
            &self.connection()?,
            project_id,
            operation,
            run_id,
        )
    }

    /// Associate a result-cache identity with canonical paid request evidence.
    /// This is append-only metadata, not a second paid permit.
    pub fn associate_analysis_request_cache_result(
        &self,
        request_key: &str,
        cache_key: &str,
    ) -> Result<()> {
        store::associate_cache_result(&self.connection()?, request_key, cache_key)
    }

    /// Explicit optional-operation projection used by adapters that already
    /// carry an optional filter. `None` is the whole-policy view.
    pub fn analysis_accounting_aggregate_scoped(
        &self,
        policy_id: &str,
        operation: Option<&str>,
        current_run_id: Option<&str>,
    ) -> Result<AnalysisAccountingAggregate> {
        ensure!(
            !policy_id.trim().is_empty(),
            "accounting aggregate policy is required"
        );
        store::accounting_aggregate(&self.connection()?, policy_id, operation, current_run_id)
    }

    /// Whether an analysis run was created by the fully ledgered runtime.
    pub(crate) fn analysis_run_is_accounted(&self, run_id: &str) -> Result<bool> {
        super::legacy::run_is_accounted(&self.connection()?, run_id)
    }

    /// Canonical factual attempts for one policy operation, used to project
    /// pilot provenance and token totals without parallel cost math.
    pub(crate) fn analysis_operation_attempt_projection(
        &self,
        policy_id: &str,
        operation: &str,
    ) -> Result<AnalysisOperationAttemptProjection> {
        ensure!(
            !policy_id.trim().is_empty(),
            "analysis attempt projection policy is required"
        );
        ensure!(
            !operation.trim().is_empty(),
            "analysis attempt projection operation is required"
        );
        store::operation_attempt_projection(&self.connection()?, policy_id, operation)
    }

    /// Read the frozen pre-ledger evidence without editing history.
    pub fn analysis_budget_authority_evidence(
        &self,
        budget_scope_id: &str,
    ) -> Result<Option<Value>> {
        store::authority_evidence(&self.connection()?, budget_scope_id)
    }

    /// Surface interrupted/uncertain work explicitly so callers inspect and
    /// reconcile instead of blindly resending a potentially completed request.
    pub fn analysis_request_needs_reconciliation(
        &self,
        request_key: &str,
    ) -> Result<Option<AnalysisRequestLifecycle>> {
        Ok(self
            .analysis_request(request_key)?
            .map(|request| request.lifecycle)
            .filter(|lifecycle| lifecycle.needs_reconciliation()))
    }
}
