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
use std::collections::HashMap;
use std::sync::Mutex;

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
    health_revisions: Mutex<HashMap<String, i64>>,
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
            health_revisions: Mutex::new(HashMap::new()),
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
            health_revisions: Mutex::new(HashMap::new()),
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

    fn provider_health_scope(&self) -> Result<(String, String, String)> {
        match &self.admission {
            Admission::Initial { context, .. } => Ok((
                context.project_id.clone(),
                context.resolved_budget_scope_id()?,
                context.provider.clone(),
            )),
            Admission::TargetedRetry(admission) => {
                let prior = self
                    .catalog
                    .analysis_request(&admission.prior_request_key)?
                    .ok_or_else(ProviderFailure::local_admission)?;
                Ok((prior.project_id, prior.budget_scope_id, prior.provider))
            }
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
        let health_context: std::sync::Mutex<Option<(String, String, String, i64)>> =
            std::sync::Mutex::new(None);
        let health_check = || -> Result<()> {
            let (project_id, policy_id, provider) = self
                .provider_health_scope()
                .map_err(|_| ProviderFailure::local_admission())?;
            let revision = self
                .catalog
                .check_provider_health(&project_id, &policy_id, &provider, model)
                .map_err(|error| {
                    if error
                        .downcast_ref::<super::provider_health::ProviderCooldown>()
                        .is_some()
                    {
                        anyhow::Error::from(ProviderFailure::provider_cooldown())
                    } else {
                        anyhow::Error::from(ProviderFailure::local_admission())
                    }
                })?;
            *health_context
                .lock()
                .map_err(|_| ProviderFailure::local_admission())? =
                Some((project_id, policy_id, provider, revision));
            Ok(())
        };
        let combined_check = || {
            health_check()?;
            if let Some(check) = pre_dispatch_check {
                check()?;
            }
            Ok(())
        };
        let result = AccountingBoundary::new(self.catalog, self.run_id(), Some(&combined_check))
            .execute(
                is_cancelled,
                || self.reserve_attempt(model, request),
                || self.provider.prepare(),
                || self.provider.analyze(model, request),
                |response| Ok(ProviderReceiptEvidence::from_response(response)),
            );
        match result {
            Ok(transmission) => {
                let (_, _, _, revision) = health_context
                    .lock()
                    .map_err(|_| ProviderFailure::local_post_dispatch())?
                    .take()
                    .ok_or_else(ProviderFailure::local_post_dispatch)?;
                self.health_revisions
                    .lock()
                    .map_err(|_| ProviderFailure::local_post_dispatch())?
                    .insert(model.to_string(), revision);
                Ok(transmission.value)
            }
            Err(error) => {
                let failure = ProviderFailure::from_anyhow(&error);
                if matches!(
                    failure.category,
                    crate::analysis::provider::ProviderFailureCategory::Transport
                        | crate::analysis::provider::ProviderFailureCategory::HttpStatus
                        | crate::analysis::provider::ProviderFailureCategory::InvalidResponse
                        | crate::analysis::provider::ProviderFailureCategory::Unclassified
                ) {
                    let health_context = health_context
                        .lock()
                        .map_err(|_| ProviderFailure::local_post_dispatch())?
                        .take();
                    if let Some((project_id, policy_id, provider, _)) = health_context {
                        let category = match failure.category {
                            crate::analysis::provider::ProviderFailureCategory::Transport => {
                                "transport"
                            }
                            crate::analysis::provider::ProviderFailureCategory::HttpStatus
                                if failure.http_status == Some(429) =>
                            {
                                "rate_limit"
                            }
                            crate::analysis::provider::ProviderFailureCategory::InvalidResponse => {
                                "schema"
                            }
                            _ => "provider",
                        };
                        self.catalog
                            .record_provider_health_failure(
                                &project_id,
                                &policy_id,
                                &provider,
                                model,
                                category,
                                failure.retry_after_seconds,
                            )
                            .map_err(|_| ProviderFailure::local_post_dispatch())?;
                    }
                }
                Err(error)
            }
        }
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

    fn record_response_rejection(
        &self,
        model: &str,
        kind: crate::analysis::provider::ProviderResponseRejection,
    ) -> Result<()> {
        let (project_id, scope_id, provider) = self.provider_health_scope()?;
        let result = self.catalog.record_provider_health_failure(
            &project_id,
            &scope_id,
            &provider,
            model,
            match kind {
                crate::analysis::provider::ProviderResponseRejection::Schema => "schema",
                crate::analysis::provider::ProviderResponseRejection::ContentQuality => {
                    "content_quality"
                }
            },
            None,
        );
        self.health_revisions
            .lock()
            .map_err(|_| ProviderFailure::local_post_dispatch())?
            .remove(model);
        result.map_err(|_| ProviderFailure::local_post_dispatch().into())
    }

    fn record_response_success(&self, model: &str) -> Result<()> {
        let (project_id, scope_id, provider) = self.provider_health_scope()?;
        let revision = self
            .health_revisions
            .lock()
            .map_err(|_| ProviderFailure::local_post_dispatch())?
            .remove(model)
            .unwrap_or(0);
        self.catalog
            .record_provider_health_success(&project_id, &scope_id, &provider, model, revision)
            .map_err(|_| ProviderFailure::local_post_dispatch().into())
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
