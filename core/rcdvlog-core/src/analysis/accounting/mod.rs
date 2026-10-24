//! Durable analysis request admission and receipt reconciliation owned by the
//! project SQLite catalog.
//!
//! This slice provides canonical authority initialization, atomic full-bound
//! reservation, durable dispatch markers, pre-upload releases, uncertainty
//! retention, append-only provider receipt settlement, explicit bounded retry
//! authorization and canonical accounting projections. Provider wrappers,
//! runtime run wiring, observation/moment/fallback dispatch and CLI adapters
//! intentionally remain follow-up work.

mod catalog;
mod model;
mod settlement;
mod store;

pub use model::{
    ANALYSIS_ACCOUNTING_SCHEMA_VERSION, AnalysisAccountingAggregate, AnalysisAccountingContext,
    AnalysisAccountingReport, AnalysisBudgetAuthorityContext, AnalysisBudgetSnapshot,
    AnalysisProviderReceipt, AnalysisRequestEvent, AnalysisRequestLifecycle,
    AnalysisRequestReconciliation, AnalysisRequestRecord, AnalysisRetryAuthorization,
    LEGACY_UNKNOWN_USAGE_STOP_REASON, MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY,
    MAX_REQUEST_ATTEMPTS, ProviderReceiptEvidence, analysis_operation_budget_scope_id,
    analysis_policy_budget_scope_id, analysis_request_cost_bound_fingerprint,
    analysis_request_payload_fingerprint, bounded_failure_code,
};

#[cfg(test)]
mod tests;
