//! Shared, private one-transmission accounting lifecycle.
//!
//! The VLM and native-audio adapters have different request and response types,
//! so they cannot share `VlmRequest`. They do share the exact durable ordering
//! around one paid call: reserve, local prepare, caller-owned domain guard,
//! cancellation, durable dispatch marker, one transport call, and immutable
//! receipt settlement before domain validation. This module owns only that
//! ordering; protocol-specific quote and payload identity stay with each
//! adapter.

use anyhow::Result;

use super::model::{AnalysisRequestRecord, ProviderReceiptEvidence};
use crate::Catalog;
use crate::analysis::provider::{ProviderFailure, ProviderFailureCategory};

/// A completed local lifecycle step with the canonical request identity.
pub(super) struct AccountedTransmission<T> {
    pub request_key: String,
    pub value: T,
}

/// The fixed accounting context for one paid transmission. Protocol-specific
/// adapters supply only their narrow quote/prepare/send/receipt closures.
pub(super) struct AccountingBoundary<'a> {
    catalog: &'a Catalog,
    run_id: &'a str,
    pre_dispatch_check: Option<&'a dyn Fn() -> Result<()>>,
}

impl<'a> AccountingBoundary<'a> {
    pub(super) fn new(
        catalog: &'a Catalog,
        run_id: &'a str,
        pre_dispatch_check: Option<&'a dyn Fn() -> Result<()>>,
    ) -> Self {
        Self {
            catalog,
            run_id,
            pre_dispatch_check,
        }
    }

    /// Runs exactly one already-admitted transmission through the canonical
    /// accounting lifecycle. The closures are deliberately narrow: they cannot
    /// schedule retries, change the account context, or bypass a durable phase.
    pub(super) fn execute<T>(
        &self,
        is_cancelled: &mut dyn FnMut() -> bool,
        reserve: impl FnOnce() -> Result<AnalysisRequestRecord>,
        prepare: impl FnOnce() -> Result<()>,
        send: impl FnOnce() -> Result<T>,
        receipt: impl FnOnce(&T) -> Result<ProviderReceiptEvidence>,
    ) -> Result<AccountedTransmission<T>> {
        if self.run_id.trim().is_empty() {
            return Err(ProviderFailure::local_admission().into());
        }
        if is_cancelled() {
            return Err(ProviderFailure::cancelled().into());
        }

        let reservation = reserve()?;
        if is_cancelled() {
            release_no_upload(
                self.catalog,
                &reservation.request_key,
                "local_cancelled_before_dispatch",
            );
            return Err(ProviderFailure::cancelled().into());
        }

        prepare_before_dispatch(self.catalog, &reservation.request_key, prepare)?;
        if let Some(check) = self.pre_dispatch_check
            && let Err(error) = check()
        {
            let failure = local_or_pre_upload(error);
            let release_code = if failure.category == ProviderFailureCategory::ProviderCooldown {
                "provider_cooldown"
            } else {
                "local_domain_guard"
            };
            release_no_upload(self.catalog, &reservation.request_key, release_code);
            return Err(failure.into());
        }
        if is_cancelled() {
            release_no_upload(
                self.catalog,
                &reservation.request_key,
                "local_cancelled_before_dispatch",
            );
            return Err(ProviderFailure::cancelled().into());
        }

        let value = dispatch_and_receipt(self.catalog, &reservation.request_key, send, receipt)?;
        Ok(AccountedTransmission {
            request_key: reservation.request_key,
            value,
        })
    }
}

fn release_no_upload(catalog: &Catalog, request_key: &str, failure_code: &str) {
    let _ = catalog.release_analysis_request_local_failure(request_key, failure_code);
}

fn prepare_before_dispatch(
    catalog: &Catalog,
    request_key: &str,
    prepare: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if let Err(error) = prepare() {
        let failure = local_or_pre_upload(error);
        let release_code = if failure.category == ProviderFailureCategory::Configuration {
            "provider_configuration"
        } else {
            "local_provider_prepare"
        };
        release_no_upload(catalog, request_key, release_code);
        return Err(failure.into());
    }
    Ok(())
}

fn local_or_pre_upload(error: anyhow::Error) -> ProviderFailure {
    let observed = ProviderFailure::from_anyhow(&error);
    if observed.category.is_local()
        || observed.category == ProviderFailureCategory::ProviderCooldown
    {
        observed
    } else {
        ProviderFailure::local_pre_upload()
    }
}

fn dispatch_and_receipt<T>(
    catalog: &Catalog,
    request_key: &str,
    send: impl FnOnce() -> Result<T>,
    receipt: impl FnOnce(&T) -> Result<ProviderReceiptEvidence>,
) -> Result<T> {
    // The durable marker is committed immediately before the one actual
    // transport call. Any later failure can no longer prove no upload.
    if catalog
        .mark_analysis_request_dispatched(request_key)
        .is_err()
    {
        release_no_upload(catalog, request_key, "local_dispatch_marker_failed");
        return Err(ProviderFailure::local_pre_upload().into());
    }

    match send() {
        Ok(result) => {
            record_receipt_or_uncertain(catalog, request_key, &result, receipt)?;
            Ok(result)
        }
        Err(error) => {
            let failure = ProviderFailure::from_anyhow(&error);
            let _ = catalog.mark_analysis_request_uncertain(request_key, &failure.safe_detail());
            Err(error)
        }
    }
}

fn record_receipt_or_uncertain<T>(
    catalog: &Catalog,
    request_key: &str,
    result: &T,
    receipt: impl FnOnce(&T) -> Result<ProviderReceiptEvidence>,
) -> Result<()> {
    let evidence = match receipt(result) {
        Ok(evidence) => evidence,
        Err(_) => {
            let _ = catalog
                .mark_analysis_request_uncertain(request_key, "local_receipt_evidence_failed");
            return Err(ProviderFailure::local_post_dispatch().into());
        }
    };
    if catalog
        .record_analysis_request_receipt(request_key, &evidence)
        .is_err()
    {
        // The paid response is already an external fact. Retain uncertainty and
        // refuse any implicit fallback/resend.
        let _ =
            catalog.mark_analysis_request_uncertain(request_key, "local_receipt_persist_failed");
        return Err(ProviderFailure::local_post_dispatch().into());
    }
    Ok(())
}
