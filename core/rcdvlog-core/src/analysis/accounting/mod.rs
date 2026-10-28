//! Durable analysis request admission and receipt reconciliation owned by the
//! project SQLite catalog.
//!
//! This slice provides canonical authority initialization, atomic full-bound
//! reservation, durable dispatch markers, pre-upload releases, uncertainty
//! retention, append-only provider receipt settlement, explicit bounded retry
//! authorization and canonical projections. Compatibility evidence preserves
//! spending written after a frozen legacy snapshot. Policy-bound pilot, bulk
//! observation, moment-discovery, chronology-suggestion and label-consolidation
//! runtime transmissions use the accounted provider wrapper. Operator-facing
//! inspection/reconciliation/retry commands remain follow-up work.

mod catalog;
mod legacy;
mod lifecycle;
mod model;
mod native_stt;
mod operation;
mod provider_boundary;
mod provider_health;
mod recovery;
mod settlement;
pub(crate) mod store;

pub use model::{
    ANALYSIS_ACCOUNTING_SCHEMA_VERSION, AnalysisAccountingAggregate, AnalysisAccountingContext,
    AnalysisAccountingReport, AnalysisBudgetAuthorityContext, AnalysisBudgetExhausted,
    AnalysisBudgetSnapshot, AnalysisOperationAccountingSummary, AnalysisOperationAttemptProjection,
    AnalysisProviderReceipt, AnalysisRequestEvent, AnalysisRequestLifecycle,
    AnalysisRequestReconciliation, AnalysisRequestRecord, AnalysisRetryAuthorization,
    AnalysisTargetedRetryAdmission, CHRONOLOGY_SUGGESTIONS_OPERATION,
    LABEL_CONSOLIDATION_OPERATION, LEGACY_UNKNOWN_USAGE_STOP_REASON,
    MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY, MAX_PROVIDER_RECEIPT_EVIDENCE_BYTES,
    MAX_REQUEST_ATTEMPTS, MAX_TARGETED_RETRY_PAYLOAD_BYTES, ProviderReceiptEvidence,
    UNTRACKED_POST_INITIALIZATION_USAGE_STOP_REASON, analysis_native_stt_payload_fingerprint,
    analysis_operation_budget_scope_id, analysis_policy_budget_scope_id,
    analysis_request_cost_bound_fingerprint, analysis_request_payload_fingerprint,
    bounded_failure_code,
};
pub(crate) use operation::{
    begin_accounted_operation, operation_intended_attempt_key, operation_policy_fingerprint,
    stable_operation_digest,
};

#[cfg(test)]
mod tests;

pub use native_stt::{AccountedNativeSttProvider, NativeSttAccountedResult};
pub use provider_boundary::AccountedVlmProvider;
pub use recovery::{
    ANALYSIS_RECOVERY_SCHEMA_VERSION, AnalysisAccountingInspection,
    AnalysisAccountingInspectionSelection, AnalysisAccountingRequestInspection,
    AnalysisRequestRecoveryReport, TargetedAnalysisRetryProviderResponse,
    TargetedAnalysisRetryReport, read_provider_receipt_evidence_file,
    read_targeted_retry_payload_file,
};

pub mod usage_pricing;
