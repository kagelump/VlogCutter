//! Native video uses the existing reserve/prepare/guard/dispatch/receipt owner.
use super::{AnalysisAccountingContext, lifecycle::AccountingBoundary, store};
use crate::analysis::native_video::{
    protocol::{CallResult, PreparedRequest},
    transport::Transport,
};
use crate::{Catalog, NativeVideoMedia, NativeVideoOptions, RunPolicy, RunPolicyParameters};
use anyhow::{Result, ensure};

pub(crate) struct AccountedNativeVideoProvider<'a> {
    pub catalog: &'a Catalog,
    pub policy: &'a RunPolicy,
    pub context: AnalysisAccountingContext,
    pub media: &'a NativeVideoMedia,
    pub evidence_id: &'a str,
    pub request: &'a PreparedRequest,
    pub transport: &'a dyn Transport,
}
impl AccountedNativeVideoProvider<'_> {
    fn options(&self) -> Result<&NativeVideoOptions> {
        let RunPolicyParameters::NativeVideoReview { options } = &self.policy.parameters else {
            anyhow::bail!("native video policy required");
        };
        Ok(options)
    }
    fn validate_preconditions(&self, cancelled: &dyn Fn() -> bool) -> Result<()> {
        let _heavy = self.catalog.acquire_local_heavy_cancellable(cancelled)?;
        ensure!(
            self.context.policy_id == self.policy.policy_id
                && self.context.policy_fingerprint == self.policy.policy_id
                && self.context.project_id == self.policy.project_id
                && self.context.provider == self.policy.provider
                && self.context.budget_usd.to_bits() == self.policy.budget_usd.to_bits()
                && matches!(
                    self.context.operation.as_str(),
                    "pilot" | "native_video_review"
                ),
            "native policy/accounting context mismatch"
        );
        self.media.validate_policy_binding(
            self.policy,
            &self.context.source_fingerprint,
            &self.context.intended_attempt_key,
        )?;
        self.request.validate_binding(self.options()?, self.media)?;
        let authorized = self
            .catalog
            .load_run_authorization(&self.policy.policy_id)?;
        ensure!(
            authorized.policy.policy_id == self.policy.policy_id,
            "native canonical authorization changed"
        );
        ensure!(
            authorized.policy.readiness.ready_for_authorization,
            "native policy is not ready"
        );
        crate::analysis::run_policy::validate_native_video_asset(
            self.catalog,
            &authorized.policy,
            &self.media.view().asset_id,
            cancelled,
        )?;
        if self.context.operation == "native_video_review" {
            self.catalog
                .require_native_video_pilot(&authorized.policy)?;
        }
        self.media
            .require_current(self.catalog, self.options()?, cancelled)
    }
    pub fn transmit(&self, cancelled: &dyn Fn() -> bool) -> Result<(String, CallResult)> {
        let options = self.options()?;
        let bound = crate::analysis::native_video::cost_bound(options)?;
        let guard = || self.validate_preconditions(cancelled);
        let result = AccountingBoundary::new(self.catalog, &self.context.run_id, Some(&guard))
            .execute(
                &mut || cancelled(),
                || {
                    guard()?;
                    store::reserve_native_video(
                        &mut self.catalog.connection()?,
                        &self.context,
                        self.media,
                        self.evidence_id,
                        &self.request.fingerprint,
                        &bound,
                    )
                },
                || {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        self.transport.prepare()
                    }))
                    .unwrap_or_else(|_| {
                        Err(anyhow::anyhow!(
                            "native transport preparation failed locally"
                        ))
                    })
                },
                || {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        self.transport.transmit(self.request, options, cancelled)
                    }))
                    .unwrap_or_else(|_| {
                        Err(anyhow::anyhow!(
                            "native transport failed after dispatch marker"
                        ))
                    })
                },
                |call| {
                    // A raw-evidence write failure must not prevent an attempt
                    // to retain the financial facts. The normal result write
                    // retries raw persistence after settlement succeeds.
                    let _ = self.catalog.stage_native_video_response(
                        self.evidence_id,
                        &self.context.run_id,
                        call,
                    );
                    Ok(call.receipt.clone())
                },
            )?;
        Ok((result.request_key, result.value))
    }
}
