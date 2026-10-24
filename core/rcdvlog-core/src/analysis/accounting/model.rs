//! Typed analysis request admission values and domain checks.
//!
//! These values keep a conservative admission reservation distinct from the
//! reported provider cost and from any confirmed charge. Nothing in this module
//! performs provider I/O or treats an estimate as an invoice.

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::analysis::provider::{AnalysisRequestCostBound, VlmRequest};

pub const ANALYSIS_ACCOUNTING_SCHEMA_VERSION: &str = "analysis.accounting.v1";
/// Hard admission cap for one durable budget authority. A full conservative
/// request bound is still required to fit the remaining ceiling; this cap only
/// bounds simultaneous in-flight exposure.
pub const MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY: usize = 4;
pub const LEGACY_UNKNOWN_USAGE_STOP_REASON: &str =
    "legacy_usage_unknown_requires_fresh_authorization";

/// The canonical authority for one explicitly authorized run policy.
pub fn analysis_policy_budget_scope_id(project_id: &str, policy_id: &str) -> String {
    format!("policy:{project_id}:{policy_id}")
}

/// The canonical authority for one non-policy operation. The intended request
/// key deliberately does not participate, otherwise concurrent callers could
/// create independent ceilings for the same operation.
pub fn analysis_operation_budget_scope_id(project_id: &str, operation: &str) -> String {
    format!("operation:{project_id}:{operation}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisRequestLifecycle {
    Reserved,
    Dispatched,
    Completed,
    Uncertain,
    Released,
}

impl AnalysisRequestLifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Dispatched => "dispatched",
            Self::Completed => "completed",
            Self::Uncertain => "uncertain",
            Self::Released => "released",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "reserved" => Ok(Self::Reserved),
            "dispatched" => Ok(Self::Dispatched),
            "completed" => Ok(Self::Completed),
            "uncertain" => Ok(Self::Uncertain),
            "released" => Ok(Self::Released),
            other => bail!("unknown analysis request lifecycle: {other}"),
        }
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Reserved | Self::Dispatched)
    }

    pub fn needs_reconciliation(self) -> bool {
        matches!(self, Self::Dispatched | Self::Uncertain)
    }
}

/// Inputs that establish or reopen the single canonical budget authority.
/// This deliberately excludes request-specific run, source, model and payload
/// identities: authority initialization may run before the runtime creates its
/// own analysis run/pilot rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisBudgetAuthorityContext {
    pub project_id: String,
    pub policy_id: String,
    pub policy_fingerprint: String,
    pub operation: String,
    pub provider: String,
    /// Empty is normal. A caller may only echo the canonical authority derived
    /// from project/policy or project/operation.
    #[serde(default)]
    pub budget_scope_id: String,
    pub budget_usd: f64,
}

impl AnalysisBudgetAuthorityContext {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.project_id.trim().is_empty(),
            "analysis budget authority project is required"
        );
        ensure!(
            !self.policy_fingerprint.trim().is_empty(),
            "analysis budget authority policy fingerprint is required"
        );
        ensure!(
            !self.operation.trim().is_empty(),
            "analysis budget authority operation is required"
        );
        ensure!(
            !self.provider.trim().is_empty(),
            "analysis budget authority provider is required"
        );
        ensure!(
            self.budget_usd.is_finite() && self.budget_usd > 0.0,
            "analysis budget authority must be finite and greater than zero"
        );
        self.resolved_budget_scope_id()?;
        Ok(())
    }

    pub fn canonical_budget_scope_id(&self) -> String {
        if self.policy_id.trim().is_empty() {
            analysis_operation_budget_scope_id(&self.project_id, &self.operation)
        } else {
            analysis_policy_budget_scope_id(&self.project_id, &self.policy_id)
        }
    }

    pub fn resolved_budget_scope_id(&self) -> Result<String> {
        let canonical = self.canonical_budget_scope_id();
        ensure!(
            self.budget_scope_id.trim().is_empty() || self.budget_scope_id == canonical,
            "analysis budget scope must equal the canonical authority {canonical}"
        );
        Ok(canonical)
    }
}

/// Caller-supplied identity for one intended paid request. Request-specific
/// validation is separate from authority initialization so C can freeze legacy
/// evidence before creating an owned runtime run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisAccountingContext {
    pub project_id: String,
    pub policy_id: String,
    pub policy_fingerprint: String,
    pub run_id: String,
    pub operation: String,
    pub intended_attempt_key: String,
    pub source_fingerprint: String,
    pub provider: String,
    #[serde(default)]
    pub budget_scope_id: String,
    pub budget_usd: f64,
}

impl AnalysisAccountingContext {
    pub fn validate_request(&self, model: &str, payload_fingerprint: &str) -> Result<()> {
        ensure!(
            !self.project_id.trim().is_empty(),
            "accounting project is required"
        );
        ensure!(
            !self.policy_fingerprint.trim().is_empty(),
            "accounting policy fingerprint is required"
        );
        ensure!(
            !self.operation.trim().is_empty(),
            "accounting operation is required"
        );
        ensure!(
            !self.intended_attempt_key.trim().is_empty(),
            "accounting intended attempt key is required"
        );
        ensure!(
            !self.source_fingerprint.trim().is_empty(),
            "accounting source fingerprint is required"
        );
        ensure!(
            !self.provider.trim().is_empty(),
            "accounting provider is required"
        );
        ensure!(
            !model.trim().is_empty(),
            "accounting requested model is required"
        );
        ensure!(
            !payload_fingerprint.trim().is_empty(),
            "accounting payload fingerprint is required"
        );
        ensure!(
            self.budget_usd.is_finite() && self.budget_usd > 0.0,
            "analysis budget authority must be finite and greater than zero"
        );
        self.resolved_budget_scope_id()?;
        Ok(())
    }

    pub fn canonical_budget_scope_id(&self) -> String {
        if self.policy_id.trim().is_empty() {
            analysis_operation_budget_scope_id(&self.project_id, &self.operation)
        } else {
            analysis_policy_budget_scope_id(&self.project_id, &self.policy_id)
        }
    }

    pub fn resolved_budget_scope_id(&self) -> Result<String> {
        let canonical = self.canonical_budget_scope_id();
        ensure!(
            self.budget_scope_id.trim().is_empty() || self.budget_scope_id == canonical,
            "analysis budget scope must equal the canonical authority {canonical}"
        );
        Ok(canonical)
    }
}

/// Persisted admission/lifecycle state for one exact intended request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisRequestRecord {
    pub request_key: String,
    pub budget_scope_id: String,
    pub project_id: String,
    pub policy_id: String,
    pub run_id: String,
    pub operation: String,
    pub attempt_base_key: String,
    pub attempt_epoch: u32,
    pub provider: String,
    pub actual_provider: Option<String>,
    pub requested_model: String,
    pub actual_model: Option<String>,
    pub provider_generation_id: Option<String>,
    pub policy_fingerprint: String,
    pub payload_fingerprint: String,
    pub source_fingerprint: String,
    pub request_fingerprint: String,
    pub cost_bound_fingerprint: String,
    /// Full conservative admission bound. Never a provider charge.
    pub cost_bound: Option<AnalysisRequestCostBound>,
    pub lifecycle: AnalysisRequestLifecycle,
    /// Durable admission reservation. Not a charge.
    pub reserved_cost_usd: f64,
    /// Last provider-returned cost, retained separately from the reservation.
    pub reported_cost_usd: Option<f64>,
    /// Confirmed only by complete provider receipt evidence in the receipt
    /// reconciliation slice.
    pub confirmed_cost_usd: f64,
    /// Unresolved usage reserve, kept separate from confirmed spend.
    pub uncertain_usage_reserve_usd: f64,
    pub failure_code: String,
    pub dispatch_at: Option<String>,
    pub settled_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisRequestEvent {
    pub event_id: i64,
    pub request_key: String,
    pub event_kind: String,
    pub detail: Value,
    pub created_at: String,
}

/// One canonical budget calculation. Confirmed historical/ledger charges,
/// active reservations and unresolved-usage reserves are exposed separately
/// and then summed once for admission.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisBudgetSnapshot {
    pub budget_scope_id: String,
    pub project_id: String,
    pub policy_id: String,
    pub requested_provider: String,
    pub budget_usd: f64,
    pub legacy_confirmed_cost_usd: f64,
    pub legacy_uncertain_usage_reserve_usd: f64,
    pub confirmed_cost_usd: f64,
    pub active_reserved_cost_usd: f64,
    pub uncertain_usage_reserve_usd: f64,
    pub committed_or_reserved_usd: f64,
    pub available_usd: f64,
    pub active_requests: usize,
    pub max_in_flight_requests: usize,
    pub dispatch_stopped: bool,
    pub stop_reason: String,
    pub legacy_usage_complete: bool,
    pub legacy_evidence: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisAccountingReport {
    pub schema_version: String,
    pub project_id: String,
    pub policy_id: Option<String>,
    pub budgets: Vec<AnalysisBudgetSnapshot>,
    pub requests: Vec<AnalysisRequestRecord>,
}

pub fn bounded_failure_code(value: &str) -> String {
    let safe: String = value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':'))
        .take(120)
        .collect();
    if safe.is_empty() {
        "unspecified".to_string()
    } else {
        safe
    }
}

pub(crate) fn digest_bytes(parts: &[&[u8]]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rcdvlog-analysis-request-admission-v1\0");
    for part in parts {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hasher.finalize().to_hex().to_string()
}

pub(crate) fn digest_json<T: Serialize>(value: &T) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}

/// Digest of the exact request payload. Only this digest is persisted.
pub fn analysis_request_payload_fingerprint(model: &str, request: &VlmRequest) -> Result<String> {
    let schema = serde_json::to_vec(&request.schema)?;
    let mut parts: Vec<&[u8]> = vec![
        model.as_bytes(),
        request.prompt.as_bytes(),
        schema.as_slice(),
    ];
    for frame in &request.frames {
        parts.push(frame.as_bytes());
    }
    Ok(digest_bytes(&parts))
}

/// Digest of the complete provider-owned cost-bound contract. It participates
/// in request identity so changed routing caps cannot look like one idempotent
/// paid request.
pub fn analysis_request_cost_bound_fingerprint(bound: &AnalysisRequestCostBound) -> Result<String> {
    bound.validate()?;
    digest_json(bound)
}

pub(crate) fn attempt_base_key(
    context: &AnalysisAccountingContext,
    model: &str,
    payload_fingerprint: &str,
    cost_bound_fingerprint: &str,
) -> Result<String> {
    digest_json(&serde_json::json!({
        "projectId": context.project_id,
        "policyId": context.policy_id,
        "policyFingerprint": context.policy_fingerprint,
        "operation": context.operation,
        "intendedAttemptKey": context.intended_attempt_key,
        "sourceFingerprint": context.source_fingerprint,
        "requestedProvider": context.provider,
        "model": model,
        "payloadFingerprint": payload_fingerprint,
        "costBoundFingerprint": cost_bound_fingerprint,
    }))
}

pub(crate) fn request_key(base: &str, epoch: u32) -> String {
    digest_bytes(&[base.as_bytes(), &epoch.to_le_bytes()])
}
