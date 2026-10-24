//! Durable analysis request admission and receipt reconciliation owned by the
//! project SQLite catalog.
//!
//! This slice provides canonical authority initialization, atomic full-bound
//! reservation, durable dispatch markers, pre-upload releases, uncertainty
//! retention, append-only provider receipt settlement, explicit bounded retry
//! authorization and canonical projections. Compatibility evidence preserves
//! spending written after a frozen legacy snapshot. Policy-bound pilot, bulk
//! observation, and moment-discovery runtime transmissions use the accounted
//! provider wrapper; chronology, ad-hoc, and CLI recovery adapters remain
//! follow-up work.

mod catalog;
mod legacy;
mod model;
mod provider_boundary;
mod settlement;
mod store;

pub use model::{
    ANALYSIS_ACCOUNTING_SCHEMA_VERSION, AnalysisAccountingAggregate, AnalysisAccountingContext,
    AnalysisAccountingReport, AnalysisBudgetAuthorityContext, AnalysisBudgetExhausted,
    AnalysisBudgetSnapshot, AnalysisOperationAttemptProjection, AnalysisProviderReceipt,
    AnalysisRequestEvent, AnalysisRequestLifecycle, AnalysisRequestReconciliation,
    AnalysisRequestRecord, AnalysisRetryAuthorization, LEGACY_UNKNOWN_USAGE_STOP_REASON,
    MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY, MAX_REQUEST_ATTEMPTS, ProviderReceiptEvidence,
    UNTRACKED_POST_INITIALIZATION_USAGE_STOP_REASON, analysis_operation_budget_scope_id,
    analysis_policy_budget_scope_id, analysis_request_cost_bound_fingerprint,
    analysis_request_payload_fingerprint, bounded_failure_code,
};

#[cfg(test)]
mod tests;

pub use provider_boundary::AccountedVlmProvider;
