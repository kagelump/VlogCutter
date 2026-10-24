//! Typed analysis request admission, receipt and projection values.
//!
//! These values keep a conservative admission reservation distinct from a
//! provider-returned cost, a confirmed final charge and unresolved usage. They
//! never perform provider I/O or treat an estimate/reservation as an invoice.

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::analysis::provider::{AnalysisRequestCostBound, VlmRequest, VlmResponse};

pub const ANALYSIS_ACCOUNTING_SCHEMA_VERSION: &str = "analysis.accounting.v1";
/// Hard admission cap for one durable budget authority. A full conservative
/// request bound is still required to fit the remaining ceiling; this cap only
/// bounds simultaneous in-flight exposure.
pub const MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY: usize = 4;
/// Explicit attempts per exact request identity, including the first attempt.
pub const MAX_REQUEST_ATTEMPTS: u32 = 4;
pub const LEGACY_UNKNOWN_USAGE_STOP_REASON: &str =
    "legacy_usage_unknown_requires_fresh_authorization";
/// A runtime in the pre-C window wrote run/pilot spending after the canonical
/// authority snapshot was frozen. The authority fails closed and preserves the
/// newly discovered floor as append-only evidence.
pub const UNTRACKED_POST_INITIALIZATION_USAGE_STOP_REASON: &str =
    "untracked_post_initialization_usage_requires_reconciliation";

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

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Uncertain | Self::Released)
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
    /// This only permits consideration of a retry. A durable authorization for
    /// this particular prior request and next epoch is also mandatory.
    #[serde(default)]
    pub explicit_retry: bool,
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
    /// Requested provider/route supplied by the caller.
    pub provider: String,
    /// Provider that actually served the request, exclusively from receipt
    /// evidence. Missing evidence stays `None`; it is never inferred from the
    /// requested route.
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
    /// Last provider-returned observed cost, retained separately from the
    /// reservation even when usage is incomplete.
    pub reported_cost_usd: Option<f64>,
    /// Confirmed only by complete provider receipt evidence.
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisProviderReceipt {
    pub receipt_key: String,
    pub request_key: String,
    /// Actual serving provider from receipt metadata; empty remains unknown.
    pub provider: String,
    pub provider_generation_id: String,
    pub actual_model: String,
    pub reported_cost_usd: Option<f64>,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub usage_complete: bool,
    pub receipt_fingerprint: String,
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
    /// Append-only evidence discovered after the authority snapshot was frozen.
    /// Kept separate from the immutable legacy fields.
    #[serde(default)]
    pub post_initialization_untracked_usage_reserve_usd: f64,
    #[serde(default)]
    pub post_initialization_untracked_evidence_count: usize,
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

/// Canonical projection over accounting attempts for one exact policy and,
/// optionally, one operation. A pilot caller must request the `pilot` operation
/// so fallback/primary attempts are included while bulk charges are excluded.
/// The whole-policy projection includes every operation plus frozen legacy
/// evidence exactly once. Pilot filtering retains provable frozen pilot evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisAccountingAggregate {
    pub project_id: String,
    pub policy_id: String,
    /// `None` means all operations; `Some("pilot")` is the pilot projection.
    pub operation: Option<String>,
    pub request_count: usize,
    pub completed_request_count: usize,
    pub active_request_count: usize,
    pub uncertain_request_count: usize,
    pub released_request_count: usize,
    pub confirmed_cost_usd: f64,
    pub active_reserved_cost_usd: f64,
    pub uncertain_usage_reserve_usd: f64,
    /// Frozen evidence is included once for the whole policy, or filtered to
    /// provable legacy pilot entries for the pilot operation. It is never
    /// re-derived from post-ledger rows.
    pub legacy_included: bool,
    pub legacy_confirmed_cost_usd: f64,
    pub legacy_uncertain_usage_reserve_usd: f64,
    pub legacy_usage_complete: bool,
    /// Append-only post-snapshot unknown usage, separate from frozen legacy
    /// evidence and from active/uncertain request reservations.
    #[serde(default)]
    pub post_initialization_untracked_usage_reserve_usd: f64,
    #[serde(default)]
    pub post_initialization_untracked_evidence_count: usize,
    pub committed_or_reserved_usd: f64,
    /// True only when relevant evidence exists (or this is a known zero-work
    /// operation) and no relevant attempt/evidence remains unresolved.
    pub usage_complete: bool,
    pub current_run_actual_provider: Option<String>,
    pub current_run_actual_model: Option<String>,
    pub current_run_generation_id: Option<String>,
    pub current_run_settled_at: Option<String>,
}

impl AnalysisAccountingAggregate {
    /// Conservative unresolved-usage check used by runtime projections. A
    /// known zero-request operation is not unresolved merely because it has no
    /// ledger rows.
    pub fn has_unresolved_usage(&self) -> bool {
        self.active_request_count > 0
            || self.uncertain_request_count > 0
            || (self.legacy_included && !self.legacy_usage_complete)
            || self.post_initialization_untracked_usage_reserve_usd > 0.0
            || self.post_initialization_untracked_evidence_count > 0
    }
}

/// Provider-returned evidence for one request. Missing facts stay missing; no
/// field is populated from the requested route or requested model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderReceiptEvidence {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub provider_generation_id: String,
    #[serde(default)]
    pub actual_model: String,
    #[serde(default)]
    pub reported_cost_usd: Option<f64>,
    #[serde(default)]
    pub prompt_tokens: Option<u32>,
    #[serde(default)]
    pub completion_tokens: Option<u32>,
    #[serde(default)]
    pub usage_complete: bool,
}

impl ProviderReceiptEvidence {
    pub fn from_response(response: &VlmResponse) -> Self {
        Self {
            provider: response.receipt.provider.clone().unwrap_or_default(),
            provider_generation_id: response.receipt.generation_id.clone().unwrap_or_default(),
            actual_model: response.receipt.actual_model.clone().unwrap_or_default(),
            reported_cost_usd: (response.cost_usd.is_finite() && response.cost_usd >= 0.0)
                .then_some(response.cost_usd),
            prompt_tokens: Some(response.prompt_tokens),
            completion_tokens: Some(response.completion_tokens),
            usage_complete: response.receipt.usage_complete,
        }
    }

    /// Complete usage plus a trustworthy returned cost is required to confirm a
    /// final charge. Complete usage without a cost remains unknown.
    pub fn is_complete_charge(&self) -> bool {
        self.usage_complete
            && self
                .reported_cost_usd
                .is_some_and(|cost| cost.is_finite() && cost >= 0.0)
    }

    pub fn validate(&self) -> Result<()> {
        for identity in [
            &self.provider,
            &self.provider_generation_id,
            &self.actual_model,
        ] {
            ensure!(
                identity == identity.trim(),
                "provider receipt identity must not contain surrounding whitespace"
            );
        }
        if let Some(cost) = self.reported_cost_usd {
            ensure!(
                cost.is_finite() && cost >= 0.0,
                "provider receipt cost must be finite and non-negative"
            );
        }
        ensure!(
            self.provider.len() <= 256,
            "provider receipt name is oversized"
        );
        ensure!(
            self.actual_model.len() <= 256,
            "provider receipt model identity is oversized"
        );
        ensure!(
            self.provider_generation_id.len() <= 512,
            "provider receipt generation identity is oversized"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnalysisRequestReconciliation {
    /// Provider evidence arriving after the dispatching process exited.
    LateReceipt { receipt: ProviderReceiptEvidence },
    /// Reliable local evidence that transmission never began. The store also
    /// verifies the durable dispatch marker is absent; a failure string alone
    /// is never accepted as no-upload proof after dispatch.
    LocalNoUpload { failure_code: String },
    /// Conservatively retain an unknown outcome for a potentially uploaded
    /// request. This never marks the request as a no-upload release.
    UncertainOutcome { failure_code: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisRetryAuthorization {
    pub original_request_key: String,
    pub attempt_base_key: String,
    pub next_attempt_epoch: u32,
    pub retained_uncertain_cost_usd: f64,
    pub prior_reported_cost_usd: Option<f64>,
    pub attempts_already_recorded: u32,
    pub max_attempts: u32,
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
