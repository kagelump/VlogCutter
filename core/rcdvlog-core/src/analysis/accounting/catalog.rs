//! Thin existing-owner APIs for canonical analysis request admission.
//!
//! This slice stops at transactional persistence. It does not wrap a provider,
//! dispatch a request, reconcile receipts or expose a CLI command. Runtime
//! integration is the next separately reviewable change and must call
//! `initialize_analysis_accounting_authority` before creating its run/pilot
//! rows.

use anyhow::{Result, anyhow, ensure};
use rusqlite::params;
use serde_json::Value;

use super::model::{
    ANALYSIS_ACCOUNTING_SCHEMA_VERSION, AnalysisAccountingContext, AnalysisAccountingReport,
    AnalysisBudgetAuthorityContext, AnalysisBudgetSnapshot, AnalysisRequestEvent,
    AnalysisRequestLifecycle, AnalysisRequestRecord, bounded_failure_code,
};
use super::store;
use crate::Catalog;
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
            safe.starts_with("local_") || safe == "provider_configuration",
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

    pub fn analysis_request_events(&self, request_key: &str) -> Result<Vec<AnalysisRequestEvent>> {
        store::events(&self.connection()?, request_key)
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
