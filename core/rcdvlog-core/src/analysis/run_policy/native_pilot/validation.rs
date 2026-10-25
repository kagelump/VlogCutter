//! Pure native pilot protocol, quality and identity interpretation.

use super::*;

pub(super) fn validate_native_accounted_result(
    request: &NativeSttRequest,
    result: &crate::analysis::accounting::NativeSttAccountedResult,
    sample: &NativePilotPaidSample,
) -> (PilotValidation, Option<String>) {
    let NativePilotSampleOutcome::Evaluated { quality, .. } = &sample.outcome else {
        let NativePilotSampleOutcome::ProtocolUnusable { protocol_error } = &sample.outcome else {
            unreachable!("all native sample outcomes are exhaustive")
        };
        return (
            PilotValidation {
                structural_schema_valid: false,
                source_time_semantics_valid: false,
                capability_observed: false,
                detail: format!("native_stt_protocol_unusable:{}", protocol_error.code()),
            },
            Some(format!(
                "native_stt_protocol_unusable:{}",
                protocol_error.code()
            )),
        );
    };

    let (structural, source_time) = match quality.reason {
        NativeSttQualityReason::AcceptedCompleteEvidence
        | NativeSttQualityReason::ReviewMissingQualityEvidence
        | NativeSttQualityReason::ReviewIncompleteQualityEvidence => (true, true),
        NativeSttQualityReason::Repetition
        | NativeSttQualityReason::LowAverageLogProbability
        | NativeSttQualityReason::LowProviderConfidence => (true, true),
        NativeSttQualityReason::InvalidTimestamp => (true, false),
        NativeSttQualityReason::EmptyProviderOutput
        | NativeSttQualityReason::UnknownLanguage
        | NativeSttQualityReason::InvalidDuration
        | NativeSttQualityReason::InvalidText
        | NativeSttQualityReason::InvalidQualityEvidence => (false, false),
    };
    let mut capability_observed = structural
        && source_time
        && matches!(
            quality.decision,
            NativeSttQualityDecision::Accepted | NativeSttQualityDecision::ReviewRequired
        );
    let mut detail = match quality.decision {
        NativeSttQualityDecision::Accepted => {
            "native_stt_protocol_and_source_mapping_passed_quality_accepted".to_string()
        }
        NativeSttQualityDecision::ReviewRequired => {
            "native_stt_protocol_and_source_mapping_passed_quality_review_required_unaccepted"
                .to_string()
        }
        NativeSttQualityDecision::Rejected => {
            format!("native_stt_quality_rejected:{}", quality.reason.code())
        }
    };
    // A different returned identifier is unverified in this deliberately narrow
    // registry. Require review without claiming an alias is a different model;
    // never compare requested broker and upstream provider namespaces.
    if result
        .call
        .receipt
        .actual_model
        .as_deref()
        .is_some_and(|actual| actual != request.model())
    {
        capability_observed = false;
        detail = "native_stt_serving_model_identity_requires_review".into();
    }
    let mut failure_hint = match quality.decision {
        NativeSttQualityDecision::Accepted | NativeSttQualityDecision::ReviewRequired => None,
        NativeSttQualityDecision::Rejected => Some(format!(
            "native_stt_quality_rejected:{}",
            quality.reason.code()
        )),
    };
    if matches!(result.call.receipt.usage.cost, NativeSttCost::Unknown) {
        failure_hint.get_or_insert_with(|| "native_stt_usage_unknown".into());
    }
    if detail == "native_stt_serving_model_identity_requires_review" {
        failure_hint.get_or_insert_with(|| detail.clone());
    }
    (
        PilotValidation {
            structural_schema_valid: structural,
            source_time_semantics_valid: source_time,
            capability_observed,
            detail,
        },
        failure_hint,
    )
}
