//! Shared operation-level accounting helpers.
//!
//! The two remaining direct paid paths are not RunPolicy-owned. They therefore
//! use the existing project/operation budget authority, but still create a real
//! accounted `analysis_runs` row before any reservation or paid transmission.
//! This module deliberately performs no provider I/O and owns no balance.

use anyhow::{Result, ensure};
use serde::Serialize;

use super::model::{
    ANALYSIS_ACCOUNTING_SCHEMA_VERSION, AnalysisAccountingContext, AnalysisBudgetAuthorityContext,
    OPERATION_REQUESTED_PROVIDER,
};
use crate::Catalog;
use crate::analysis::provider::{ProviderChain, analysis_cost_bound_contract_id};

#[derive(Debug, Clone)]
pub(crate) struct AccountedOperationSession {
    pub(crate) project_id: String,
    pub(crate) operation: String,
    pub(crate) run_id: String,
    pub(crate) policy_fingerprint: String,
    pub(crate) provider: String,
    pub(crate) budget_usd: f64,
}

/// Stable digest for operation identity and source identity. It never persists
/// the input: only the resulting fixed-size digest crosses the ledger boundary.
pub(crate) fn stable_operation_digest(value: &impl Serialize) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}

/// The operation authority binds the provider cost contract, requested route,
/// permitted model set/fallback policy and operation recipe. It deliberately
/// excludes the current run id and request payload; those belong to request
/// identity and must not manufacture a fresh ceiling.
pub(crate) fn operation_policy_fingerprint(
    project_id: &str,
    operation: &str,
    chain: &ProviderChain,
    recipe_version: &str,
    budget_usd: f64,
) -> Result<String> {
    ensure!(
        !project_id.trim().is_empty(),
        "operation project is required"
    );
    ensure!(!operation.trim().is_empty(), "operation name is required");
    ensure!(
        !recipe_version.trim().is_empty(),
        "operation recipe is required"
    );
    ensure!(
        budget_usd.is_finite() && budget_usd > 0.0,
        "operation budget must be finite and greater than zero"
    );
    stable_operation_digest(&serde_json::json!({
        "schemaVersion": ANALYSIS_ACCOUNTING_SCHEMA_VERSION,
        "projectId": project_id,
        "operation": operation,
        "provider": OPERATION_REQUESTED_PROVIDER,
        "primaryModel": chain.primary_model(),
        "allowedModels": chain.allowed_models(),
        "allowFallback": chain.allows_fallback(),
        "fallbackModel": chain.fallback_model(),
        "costBoundContract": analysis_cost_bound_contract_id(),
        "recipeVersion": recipe_version,
        "budgetUsd": budget_usd,
    }))
}

/// The result of an operation request must remain stable across a restart. The
/// run id is intentionally absent; a fresh process/run cannot move the paid
/// request fence to a new identity.
pub(crate) fn operation_intended_attempt_key(
    project_id: &str,
    operation: &str,
    source_fingerprint: &str,
    request_identity: &str,
) -> Result<String> {
    ensure!(
        !source_fingerprint.trim().is_empty() && !request_identity.trim().is_empty(),
        "operation request identity is required"
    );
    stable_operation_digest(&serde_json::json!({
        "projectId": project_id,
        "operation": operation,
        "sourceFingerprint": source_fingerprint,
        "requestIdentity": request_identity,
    }))
}

/// Freeze/verify the one operation authority before creating the accounted run
/// row, then return the immutable run identity used by every transmission in
/// this operation invocation.
pub(crate) fn begin_accounted_operation(
    catalog: &Catalog,
    project_id: &str,
    operation: &str,
    policy_fingerprint: &str,
    budget_usd: f64,
    stage_reached: &str,
) -> Result<AccountedOperationSession> {
    ensure!(
        !project_id.trim().is_empty(),
        "operation project is required"
    );
    ensure!(!operation.trim().is_empty(), "operation name is required");
    ensure!(
        !policy_fingerprint.trim().is_empty(),
        "operation policy fingerprint is required"
    );
    ensure!(
        budget_usd.is_finite() && budget_usd > 0.0,
        "operation budget must be finite and greater than zero"
    );
    let authority =
        catalog.initialize_analysis_accounting_authority(&AnalysisBudgetAuthorityContext {
            project_id: project_id.to_string(),
            policy_id: String::new(),
            policy_fingerprint: policy_fingerprint.to_string(),
            operation: operation.to_string(),
            provider: OPERATION_REQUESTED_PROVIDER.to_string(),
            budget_scope_id: String::new(),
            budget_usd,
        })?;
    ensure!(
        !authority.dispatch_stopped,
        "analysis budget authority dispatch is stopped: {}",
        authority.stop_reason
    );
    let run_id = catalog.begin_accounted_analysis_run(project_id, "", stage_reached)?;
    Ok(AccountedOperationSession {
        project_id: project_id.to_string(),
        operation: operation.to_string(),
        run_id,
        policy_fingerprint: policy_fingerprint.to_string(),
        provider: OPERATION_REQUESTED_PROVIDER.to_string(),
        budget_usd,
    })
}

impl AccountedOperationSession {
    pub(crate) fn request_context(
        &self,
        intended_attempt_key: String,
        source_fingerprint: String,
    ) -> AnalysisAccountingContext {
        AnalysisAccountingContext {
            project_id: self.project_id.clone(),
            policy_id: String::new(),
            policy_fingerprint: self.policy_fingerprint.clone(),
            run_id: self.run_id.clone(),
            operation: self.operation.clone(),
            intended_attempt_key,
            source_fingerprint,
            provider: self.provider.clone(),
            budget_scope_id: String::new(),
            budget_usd: self.budget_usd,
            explicit_retry: false,
        }
    }
}
