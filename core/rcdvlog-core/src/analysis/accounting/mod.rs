//! Durable analysis request admission owned by the project SQLite catalog.
//!
//! This slice provides canonical authority initialization, atomic full-bound
//! reservation, durable dispatch markers, pre-upload releases, uncertainty
//! retention and read projections. Receipt settlement, late reconciliation,
//! retries, provider wrappers, runtime run wiring and CLI adapters intentionally
//! remain follow-up work.

mod catalog;
mod model;
mod store;

pub use model::{
    ANALYSIS_ACCOUNTING_SCHEMA_VERSION, AnalysisAccountingContext, AnalysisAccountingReport,
    AnalysisBudgetAuthorityContext, AnalysisBudgetSnapshot, AnalysisRequestEvent,
    AnalysisRequestLifecycle, AnalysisRequestRecord, LEGACY_UNKNOWN_USAGE_STOP_REASON,
    MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY, analysis_operation_budget_scope_id,
    analysis_policy_budget_scope_id, analysis_request_cost_bound_fingerprint,
    analysis_request_payload_fingerprint, bounded_failure_code,
};

#[cfg(test)]
mod tests;
