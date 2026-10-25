//! Pure status and observed-capability projection for the existing pilot owner.

use super::*;

pub(super) fn pilot_projection_status(
    disposition: PilotProjectionDisposition,
    validation: &PilotValidation,
    usage_complete: bool,
    failure_hint: Option<&str>,
) -> (&'static str, String, String) {
    let unresolved = !usage_complete;
    match disposition {
            PilotProjectionDisposition::NativeNoProviderRequired => (
                "no_provider_required",
                String::new(),
                "Native capability pilot observed only local NoAudio/NoSpeech facts for every selected source. No provider request was made, endpoint capability is unobserved, no speech or accuracy claim is made, and this outcome does not authorize native bulk.".to_string(),
            ),
            PilotProjectionDisposition::Standard => {
                let status = if validation.capability_observed && usage_complete {
                    "passed"
                } else if unresolved {
                    "uncertain"
                } else {
                    "failed"
                };
                let failure_code = if status == "passed" {
                    String::new()
                } else if unresolved {
                    bounded(failure_hint.unwrap_or("usage_unknown"), 80)
                } else if !validation.structural_schema_valid {
                    "schema_validation_failed".to_string()
                } else if !validation.source_time_semantics_valid {
                    "source_time_semantics_failed".to_string()
                } else {
                    bounded(failure_hint.unwrap_or("analysis_error"), 80)
                };
                let warning = if usage_complete {
                    PILOT_WARNING.to_string()
                } else {
                    format!(
                        "{PILOT_WARNING} Canonical ledger usage is incomplete; the total below includes a reserve, not a confirmed charge."
                    )
                };
                (status, failure_code, warning)
            }
        }
}

pub(super) fn pilot_observed_capabilities(
    validation: &PilotValidation,
    native_evidence: Option<&NativePilotEvidence>,
) -> Vec<String> {
    let mut observed_capabilities = Vec::new();
    if validation.structural_schema_valid {
        observed_capabilities.push(if native_evidence.is_some() {
            "native response shape accepted this reply".into()
        } else {
            "configured JSON schema accepted this response".into()
        });
    }
    if validation.source_time_semantics_valid {
        observed_capabilities.push(if native_evidence.is_some() {
            "explicit native source-time semantics accepted this reply".into()
        } else {
            "explicit source-time semantics accepted this response".into()
        });
    }
    if validation.structural_schema_valid
        && validation.source_time_semantics_valid
        && let Some(evidence) = native_evidence
        && let Some(sample) = evidence.sample.as_ref()
    {
        match &sample.outcome {
            NativePilotSampleOutcome::Evaluated { quality, .. } => {
                observed_capabilities.push(
                        "native endpoint protocol and bounded source/window mapping were observed for this exact policy/window".into(),
                    );
                if quality.decision == crate::NativeSttQualityDecision::ReviewRequired {
                    observed_capabilities.push(
                            "native quality evidence is absent or partial; candidate text remains explicitly unaccepted and requires human review".into(),
                        );
                } else if quality.decision == crate::NativeSttQualityDecision::Accepted {
                    observed_capabilities.push(
                            "native quality recipe accepted this bounded window; this is not an accuracy claim for the provider or other assets".into(),
                        );
                }
            }
            NativePilotSampleOutcome::ProtocolUnusable { .. } => {}
        }
    }
    observed_capabilities
}
