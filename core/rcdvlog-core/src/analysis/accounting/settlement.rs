//! Pure receipt classification and settlement projection rules.
//!
//! This module deliberately has no SQLite or provider I/O. `store` applies the
//! returned settlement in one `BEGIN IMMEDIATE` transaction, appends the
//! immutable receipt/event rows, and recomputes the authority projection.

use anyhow::{Result, bail, ensure};

use super::model::{AnalysisRequestLifecycle, AnalysisRequestRecord, ProviderReceiptEvidence};

const AMOUNT_EPSILON: f64 = 1e-12;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ExistingReceipt {
    pub fingerprint: String,
    pub provider: String,
    pub generation: String,
    pub actual_model: String,
    pub reported_cost_usd: Option<f64>,
    pub usage_complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReceiptDisposition {
    /// The exact same evidence fingerprint is already stored. No write or event
    /// is needed, so repeated reconciliation cannot grow history without bound.
    Idempotent,
    /// Append immutable evidence and apply one settlement projection.
    Append,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReceiptSettlement {
    pub confirmed_cost_usd: f64,
    pub estimated_cost_usd: Option<f64>,
    pub uncertain_usage_reserve_usd: f64,
    pub reported_cost_usd: Option<f64>,
    pub actual_provider: Option<String>,
    pub actual_model: Option<String>,
    pub provider_generation_id: Option<String>,
    pub lifecycle: AnalysisRequestLifecycle,
    pub failure_code: String,
    pub metadata_enriched: bool,
    pub observed_cost_exceeded_bound: bool,
}

fn merge_identity(known: Option<&str>, incoming: &str) -> Result<Option<String>> {
    let known = known.filter(|value| !value.trim().is_empty());
    let incoming = incoming.trim();
    match (known, incoming.is_empty()) {
        (Some(known), true) => Ok(Some(known.to_string())),
        (Some(known), false) if known == incoming => Ok(Some(known.to_string())),
        (Some(_), false) => {
            bail!("late receipt identity conflicts with the known receipt identity")
        }
        (None, true) => Ok(None),
        (None, false) => Ok(Some(incoming.to_string())),
    }
}

fn merge_observed_cost(previous: Option<f64>, incoming: Option<f64>) -> Option<f64> {
    match (previous, incoming) {
        (Some(previous), Some(incoming)) => Some(previous.max(incoming)),
        (Some(previous), None) => Some(previous),
        (None, Some(incoming)) => Some(incoming),
        (None, None) => None,
    }
}

fn enforce_identity(
    current: &AnalysisRequestRecord,
    existing: &[ExistingReceipt],
    evidence: &ProviderReceiptEvidence,
) -> Result<()> {
    for receipt in existing {
        merge_identity(Some(&receipt.provider), &evidence.provider)?;
        merge_identity(Some(&receipt.generation), &evidence.provider_generation_id)?;
        merge_identity(Some(&receipt.actual_model), &evidence.actual_model)?;
    }
    merge_identity(current.actual_provider.as_deref(), &evidence.provider)?;
    merge_identity(current.actual_model.as_deref(), &evidence.actual_model)?;
    merge_identity(
        current.provider_generation_id.as_deref(),
        &evidence.provider_generation_id,
    )?;
    Ok(())
}

struct MergedIdentity {
    actual_provider: Option<String>,
    actual_model: Option<String>,
    provider_generation_id: Option<String>,
    enriched: bool,
}

fn merged_identity(
    current: &AnalysisRequestRecord,
    evidence: &ProviderReceiptEvidence,
) -> Result<MergedIdentity> {
    let actual_provider = merge_identity(current.actual_provider.as_deref(), &evidence.provider)?;
    let actual_model = merge_identity(current.actual_model.as_deref(), &evidence.actual_model)?;
    let provider_generation_id = merge_identity(
        current.provider_generation_id.as_deref(),
        &evidence.provider_generation_id,
    )?;
    let enriched = actual_provider != current.actual_provider
        || actual_model != current.actual_model
        || provider_generation_id != current.provider_generation_id;
    Ok(MergedIdentity {
        actual_provider,
        actual_model,
        provider_generation_id,
        enriched,
    })
}

fn observed_exceeds_bound(
    current: &AnalysisRequestRecord,
    evidence: &ProviderReceiptEvidence,
) -> bool {
    evidence
        .reported_cost_usd
        .is_some_and(|cost| cost > current.reserved_cost_usd + AMOUNT_EPSILON)
}

/// Validate the request/lifecycle boundary and receipt identity without writing.
pub(crate) fn classify_receipt(
    current: &AnalysisRequestRecord,
    existing: &[ExistingReceipt],
    evidence: &ProviderReceiptEvidence,
    fingerprint: &str,
) -> Result<ReceiptDisposition> {
    if existing
        .iter()
        .any(|receipt| receipt.fingerprint == fingerprint)
    {
        return Ok(ReceiptDisposition::Idempotent);
    }
    ensure!(
        !matches!(
            current.lifecycle,
            AnalysisRequestLifecycle::Reserved | AnalysisRequestLifecycle::Released
        ),
        "a provider receipt cannot turn a never-dispatched or locally-released request into a charged completion"
    );
    ensure!(
        current.dispatch_at.is_some(),
        "a provider receipt cannot settle a request without a durable dispatch marker"
    );
    enforce_identity(current, existing, evidence)?;
    if current.lifecycle == AnalysisRequestLifecycle::Completed
        && current.estimated_cost_usd.is_none()
    {
        if let Some(cost) = evidence.reported_cost_usd {
            ensure!(
                (cost - current.confirmed_cost_usd).abs() <= AMOUNT_EPSILON,
                "analysis request already has a completed provider receipt; conflicting receipt identity or cost was rejected"
            );
        }
    } else {
        let existing_complete_charge = existing
            .iter()
            .any(|receipt| receipt.usage_complete && receipt.reported_cost_usd.is_some());
        ensure!(
            !existing_complete_charge || !evidence.is_complete_charge(),
            "analysis request already has a complete provider receipt; a later receipt cannot overwrite the confirmed charge"
        );
    }
    Ok(ReceiptDisposition::Append)
}

/// Compute the one settlement projection for an accepted receipt. Prior rows
/// are never edited; this only tells the store what current projection the
/// validated evidence implies.
pub(crate) fn settlement_for_receipt(
    current: &AnalysisRequestRecord,
    evidence: &ProviderReceiptEvidence,
) -> Result<ReceiptSettlement> {
    let merged = merged_identity(current, evidence)?;
    let observed_cost_exceeded_bound = observed_exceeds_bound(current, evidence);

    let estimate = match (&evidence.usage_pricing, &current.cost_bound) {
        (Some(usage), Some(bound))
            if evidence.usage_complete
                && evidence.provider == current.provider
                && evidence
                    .prompt_tokens
                    .is_none_or(|count| usage.prompt_tokens == Some(u64::from(count)))
                && evidence.completion_tokens.is_none_or(|count| {
                    usage
                        .candidate_tokens
                        .zip(usage.thinking_tokens)
                        .and_then(|(a, b)| a.checked_add(b))
                        == Some(u64::from(count))
                }) =>
        {
            usage.estimate(
                bound,
                &evidence.provider,
                &current.requested_model,
                &evidence.actual_model,
            )?
        }
        _ => None,
    }
    .filter(|estimate| {
        current
            .reported_cost_usd
            .is_none_or(|floor| floor <= *estimate + AMOUNT_EPSILON)
    })
    .filter(|estimate| {
        current
            .estimated_cost_usd
            .is_none_or(|previous| (previous - *estimate).abs() <= AMOUNT_EPSILON)
    });
    if current.lifecycle == AnalysisRequestLifecycle::Completed
        && (current.estimated_cost_usd.is_none()
            || (!evidence.is_complete_charge()
                && evidence.usage_pricing.is_none()
                && evidence.reported_cost_usd.is_none_or(|cost| {
                    cost <= current.estimated_cost_usd.unwrap_or(0.0) + AMOUNT_EPSILON
                })))
    {
        return Ok(ReceiptSettlement {
            confirmed_cost_usd: current.confirmed_cost_usd,
            estimated_cost_usd: current.estimated_cost_usd,
            uncertain_usage_reserve_usd: 0.0,
            reported_cost_usd: current.reported_cost_usd.or(evidence.reported_cost_usd),
            actual_provider: merged.actual_provider,
            actual_model: merged.actual_model,
            provider_generation_id: merged.provider_generation_id,
            lifecycle: AnalysisRequestLifecycle::Completed,
            failure_code: current.failure_code.clone(),
            metadata_enriched: merged.enriched,
            observed_cost_exceeded_bound,
        });
    }

    if evidence.is_complete_charge() {
        let confirmed = evidence.reported_cost_usd.unwrap_or(0.0);
        return Ok(ReceiptSettlement {
            confirmed_cost_usd: confirmed,
            estimated_cost_usd: None,
            uncertain_usage_reserve_usd: 0.0,
            reported_cost_usd: Some(confirmed),
            actual_provider: merged.actual_provider,
            actual_model: merged.actual_model,
            provider_generation_id: merged.provider_generation_id,
            lifecycle: AnalysisRequestLifecycle::Completed,
            failure_code: String::new(),
            metadata_enriched: merged.enriched,
            observed_cost_exceeded_bound,
        });
    }

    if let Some(estimated) = estimate {
        return Ok(ReceiptSettlement {
            confirmed_cost_usd: 0.0,
            estimated_cost_usd: Some(estimated),
            uncertain_usage_reserve_usd: 0.0,
            reported_cost_usd: current.reported_cost_usd,
            actual_provider: merged.actual_provider,
            actual_model: merged.actual_model,
            provider_generation_id: merged.provider_generation_id,
            lifecycle: AnalysisRequestLifecycle::Completed,
            failure_code: String::new(),
            metadata_enriched: merged.enriched,
            observed_cost_exceeded_bound: estimated > current.reserved_cost_usd + AMOUNT_EPSILON,
        });
    }

    // An incomplete or missing-cost receipt must never shrink prior uncertainty
    // or erase the original admission bound. The returned amount is a floor, not
    // a final charge.
    let uncertain = current
        .uncertain_usage_reserve_usd
        .max(current.reserved_cost_usd)
        .max(evidence.reported_cost_usd.unwrap_or(0.0));
    let failure_code = if evidence.usage_complete {
        "usage_cost_missing"
    } else {
        "usage_incomplete"
    };
    Ok(ReceiptSettlement {
        confirmed_cost_usd: 0.0,
        estimated_cost_usd: None,
        uncertain_usage_reserve_usd: uncertain,
        reported_cost_usd: merge_observed_cost(
            current.reported_cost_usd,
            evidence.reported_cost_usd,
        ),
        actual_provider: merged.actual_provider,
        actual_model: merged.actual_model,
        provider_generation_id: merged.provider_generation_id,
        lifecycle: AnalysisRequestLifecycle::Uncertain,
        failure_code: failure_code.to_string(),
        metadata_enriched: merged.enriched,
        observed_cost_exceeded_bound,
    })
}
