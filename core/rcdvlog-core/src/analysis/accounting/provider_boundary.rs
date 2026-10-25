//! Thin, reusable paid-provider accounting boundary.
//!
//! Exactly one call to the wrapped transport is admitted, reserved, dispatched
//! and receipted at a time. SQLite transactions never span provider I/O. This
//! type is intentionally passed into the existing `ProviderChain`, so transport
//! fallback and domain-validation fallback get one independent ledger attempt
//! each without a second scheduler or retry framework.
//!
//! The same boundary also dispatches one explicitly authorized targeted retry.
//! It uses the prior immutable `attempt_base_key` and expected next epoch; it
//! never fabricates an intended-attempt key or a replacement account context.

use anyhow::Result;

use super::lifecycle::AccountingBoundary;
use super::model::{
    AnalysisAccountingContext, AnalysisBudgetExhausted, AnalysisTargetedRetryAdmission,
    ProviderReceiptEvidence, analysis_request_cost_bound_fingerprint,
    analysis_request_payload_fingerprint,
};
use crate::Catalog;
use crate::analysis::provider::{
    AnalysisRequestCostBound, ProviderFailure, VlmProvider, VlmRequest, VlmResponse,
};

/// The VLM accounting wrapper may only admit chat/VLM token/image quote
/// contracts. A lower-level non-policy operation context still must never let a
/// provider smuggle a native duration quote into a VLM dispatch.
fn ensure_vlm_cost_bound(cost_bound: &AnalysisRequestCostBound) -> Result<()> {
    if cost_bound.audio.is_some() {
        return Err(ProviderFailure::local_admission().into());
    }
    Ok(())
}

enum Admission {
    Initial {
        context: AnalysisAccountingContext,
        explicit_retry: bool,
    },
    TargetedRetry(AnalysisTargetedRetryAdmission),
}

pub struct AccountedVlmProvider<'a> {
    catalog: &'a Catalog,
    provider: &'a dyn VlmProvider,
    admission: Admission,
    pre_dispatch_check: Option<Box<dyn Fn() -> Result<()> + Send + Sync + 'a>>,
}

impl<'a> AccountedVlmProvider<'a> {
    pub fn new(
        catalog: &'a Catalog,
        context: AnalysisAccountingContext,
        provider: &'a dyn VlmProvider,
    ) -> Self {
        Self {
            catalog,
            provider,
            admission: Admission::Initial {
                context,
                explicit_retry: false,
            },
            pre_dispatch_check: None,
        }
    }

    /// Build the same dispatch/prepare/cancel/receipt lifecycle for an explicit
    /// targeted retry. The durable authorization and expected epoch remain the
    /// only paid permit; this constructor grants no permission by itself.
    pub fn new_targeted_retry(
        catalog: &'a Catalog,
        admission: AnalysisTargetedRetryAdmission,
        provider: &'a dyn VlmProvider,
    ) -> Self {
        Self {
            catalog,
            provider,
            admission: Admission::TargetedRetry(admission),
            pre_dispatch_check: None,
        }
    }

    /// The caller flag only permits consideration of a retry. The canonical
    /// store still requires the durable authorization for this exact prior
    /// request and next epoch before any new paid transmission is admitted.
    pub fn with_explicit_retry(mut self, explicit_retry: bool) -> Self {
        if let Admission::Initial {
            explicit_retry: current,
            ..
        } = &mut self.admission
        {
            *current = explicit_retry;
        }
        self
    }

    /// Recheck operation-owned domain identity after local provider preparation
    /// and immediately before the durable dispatch marker. The check runs on
    /// the same thread and never holds a transaction across transport I/O.
    pub fn with_pre_dispatch_check(
        mut self,
        check: impl Fn() -> Result<()> + Send + Sync + 'a,
    ) -> Self {
        self.pre_dispatch_check = Some(Box::new(check));
        self
    }

    fn run_id(&self) -> &str {
        match &self.admission {
            Admission::Initial { context, .. } => &context.run_id,
            Admission::TargetedRetry(admission) => &admission.run_id,
        }
    }

    fn reserve_attempt(
        &self,
        model: &str,
        request: &VlmRequest,
    ) -> Result<super::model::AnalysisRequestRecord> {
        let payload_fingerprint = analysis_request_payload_fingerprint(model, request)
            .map_err(|_| ProviderFailure::local_pre_upload())?;
        let cost_bound = self
            .provider
            .analysis_cost_bound(model, request)
            .map_err(|_| ProviderFailure::local_pre_upload())?;
        ensure_vlm_cost_bound(&cost_bound)?;
        match &self.admission {
            Admission::Initial {
                context,
                explicit_retry,
            } => {
                let mut context = context.clone();
                context.explicit_retry = *explicit_retry;
                match self.catalog.reserve_analysis_request(
                    &context,
                    model,
                    &payload_fingerprint,
                    &cost_bound,
                ) {
                    Ok(record) => Ok(record),
                    Err(error) if error.downcast_ref::<AnalysisBudgetExhausted>().is_some() => {
                        Err(ProviderFailure::local_budget_exhausted().into())
                    }
                    Err(_) => Err(ProviderFailure::local_admission().into()),
                }
            }
            Admission::TargetedRetry(admission) => {
                // The caller-supplied provider quote must still fingerprint to
                // the immutable prior cost contract. This local check occurs
                // before the admission transaction and before any transport.
                let cost_bound_fingerprint = analysis_request_cost_bound_fingerprint(&cost_bound)
                    .map_err(|_| ProviderFailure::local_pre_upload())?;
                match self.catalog.validate_analysis_targeted_retry_claim(
                    admission,
                    model,
                    &payload_fingerprint,
                    &cost_bound_fingerprint,
                    &cost_bound,
                ) {
                    Ok(_) => {}
                    Err(error) if error.downcast_ref::<AnalysisBudgetExhausted>().is_some() => {
                        return Err(ProviderFailure::local_budget_exhausted().into());
                    }
                    Err(_) => return Err(ProviderFailure::local_admission().into()),
                }
                match self.catalog.reserve_analysis_request_targeted_retry(
                    admission,
                    model,
                    &payload_fingerprint,
                    &cost_bound,
                ) {
                    Ok(record) => Ok(record),
                    Err(error) if error.downcast_ref::<AnalysisBudgetExhausted>().is_some() => {
                        Err(ProviderFailure::local_budget_exhausted().into())
                    }
                    Err(_) => Err(ProviderFailure::local_admission().into()),
                }
            }
        }
    }

    fn transmit(
        &self,
        model: &str,
        request: &VlmRequest,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<VlmResponse> {
        let pre_dispatch_check: Option<&dyn Fn() -> Result<()>> = self
            .pre_dispatch_check
            .as_deref()
            .map(|check| check as &dyn Fn() -> Result<()>);
        AccountingBoundary::new(self.catalog, self.run_id(), pre_dispatch_check)
            .execute(
                is_cancelled,
                || self.reserve_attempt(model, request),
                || self.provider.prepare(),
                || self.provider.analyze(model, request),
                |response| Ok(ProviderReceiptEvidence::from_response(response)),
            )
            .map(|transmission| transmission.value)
    }
}

impl VlmProvider for AccountedVlmProvider<'_> {
    fn analysis_cost_bound(
        &self,
        model: &str,
        request: &VlmRequest,
    ) -> Result<AnalysisRequestCostBound> {
        let cost_bound = self.provider.analysis_cost_bound(model, request)?;
        ensure_vlm_cost_bound(&cost_bound)?;
        Ok(cost_bound)
    }

    fn analyze(&self, model: &str, request: &VlmRequest) -> Result<VlmResponse> {
        self.transmit(model, request, &mut || false)
    }

    fn analyze_with_cancel(
        &self,
        model: &str,
        request: &VlmRequest,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<VlmResponse> {
        self.transmit(model, request, is_cancelled)
    }
}
