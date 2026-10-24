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

use super::model::{
    AnalysisAccountingContext, AnalysisBudgetExhausted, AnalysisTargetedRetryAdmission,
    ProviderReceiptEvidence, analysis_request_cost_bound_fingerprint,
    analysis_request_payload_fingerprint,
};
use crate::Catalog;
use crate::analysis::provider::{
    AnalysisRequestCostBound, ProviderFailure, VlmProvider, VlmRequest, VlmResponse,
};

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

    fn release_no_upload(&self, request_key: &str, failure_code: &str) {
        let _ = self
            .catalog
            .release_analysis_request_local_failure(request_key, failure_code);
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

    fn prepare_before_dispatch(&self, request_key: &str) -> Result<()> {
        if let Err(error) = self.provider.prepare() {
            let observed = ProviderFailure::from_anyhow(&error);
            let failure = if observed.category.is_local() {
                observed
            } else {
                ProviderFailure::local_pre_upload()
            };
            let release_code = if failure.category
                == crate::analysis::provider::ProviderFailureCategory::Configuration
            {
                "provider_configuration"
            } else {
                "local_provider_prepare"
            };
            self.release_no_upload(request_key, release_code);
            return Err(failure.into());
        }
        Ok(())
    }

    fn record_receipt_or_uncertain(&self, request_key: &str, response: &VlmResponse) -> Result<()> {
        let evidence = ProviderReceiptEvidence::from_response(response);
        if self
            .catalog
            .record_analysis_request_receipt(request_key, &evidence)
            .is_err()
        {
            // The paid response is already an external fact. Retain uncertainty
            // and refuse any implicit fallback/resend.
            let _ = self
                .catalog
                .mark_analysis_request_uncertain(request_key, "local_receipt_persist_failed");
            return Err(ProviderFailure::local_post_dispatch().into());
        }
        Ok(())
    }

    fn dispatch_and_receipt(
        &self,
        request_key: &str,
        model: &str,
        request: &VlmRequest,
    ) -> Result<VlmResponse> {
        // The durable marker is committed immediately before the one actual
        // transport call. Any later failure can no longer prove no upload.
        if self
            .catalog
            .mark_analysis_request_dispatched(request_key)
            .is_err()
        {
            self.release_no_upload(request_key, "local_dispatch_marker_failed");
            return Err(ProviderFailure::local_pre_upload().into());
        }
        match self.provider.analyze(model, request) {
            Ok(response) => {
                self.record_receipt_or_uncertain(request_key, &response)?;
                Ok(response)
            }
            Err(error) => {
                let failure = ProviderFailure::from_anyhow(&error);
                let _ = self
                    .catalog
                    .mark_analysis_request_uncertain(request_key, &failure.safe_detail());
                Err(error)
            }
        }
    }

    fn transmit(
        &self,
        model: &str,
        request: &VlmRequest,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<VlmResponse> {
        if self.run_id().trim().is_empty() {
            return Err(ProviderFailure::local_admission().into());
        }
        if is_cancelled() {
            return Err(ProviderFailure::cancelled().into());
        }
        let reservation = self.reserve_attempt(model, request)?;
        if is_cancelled() {
            self.release_no_upload(&reservation.request_key, "local_cancelled_before_dispatch");
            return Err(ProviderFailure::cancelled().into());
        }
        self.prepare_before_dispatch(&reservation.request_key)?;
        if let Some(check) = &self.pre_dispatch_check
            && let Err(error) = check()
        {
            self.release_no_upload(&reservation.request_key, "local_domain_guard");
            let observed = ProviderFailure::from_anyhow(&error);
            let failure = if observed.category.is_local() {
                observed
            } else {
                ProviderFailure::local_pre_upload()
            };
            return Err(failure.into());
        }
        if is_cancelled() {
            self.release_no_upload(&reservation.request_key, "local_cancelled_before_dispatch");
            return Err(ProviderFailure::cancelled().into());
        }
        self.dispatch_and_receipt(&reservation.request_key, model, request)
    }
}

impl VlmProvider for AccountedVlmProvider<'_> {
    fn analysis_cost_bound(
        &self,
        model: &str,
        request: &VlmRequest,
    ) -> Result<AnalysisRequestCostBound> {
        self.provider.analysis_cost_bound(model, request)
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
