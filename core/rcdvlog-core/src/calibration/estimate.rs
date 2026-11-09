//! Timing uncertainty, ETA derivation, and override selection from calibration results.

use super::*;

pub(super) fn uncertainty_for(
    timing: &CalibrationTiming,
    samples: usize,
) -> CalibrationUncertainty {
    let spread = timing.relative_spread();
    let relative = if samples <= 1 { 0.25 } else { spread.max(0.02) };
    CalibrationUncertainty {
        confidence: if samples >= 3 {
            "measured".into()
        } else {
            "low_sample_count".into()
        },
        timing_relative_spread: Some(spread),
        eta_relative_range: Some(relative),
        note: if samples <= 1 {
            "one timing sample; the ETA range is intentionally widened rather than presented as a completion promise".into()
        } else {
            "ETA range reflects observed timing spread".into()
        },
    }
}

pub(super) fn eta_from_timing(
    timing: Option<&CalibrationTiming>,
    work_units: Option<u64>,
    uncertainty: &CalibrationUncertainty,
    source: &str,
) -> CalibrationEta {
    let Some(timing) = timing else {
        return CalibrationEta {
            status: "uncertain".into(),
            estimated_remaining_seconds: None,
            range_seconds: None,
            confidence: "none".into(),
            basis: source.into(),
            explanation: "no completed matching calibration timing is available".into(),
        };
    };
    let Some(work_units) = work_units.filter(|work_units| *work_units > 0) else {
        return CalibrationEta {
            status: "uncertain".into(),
            estimated_remaining_seconds: None,
            range_seconds: None,
            confidence: "none".into(),
            basis: source.into(),
            explanation: "calibration exists but the caller supplied no positive work weight"
                .into(),
        };
    };
    let Some(rate) = timing.work_units_per_second.filter(|rate| *rate > 0.0) else {
        return CalibrationEta {
            status: "uncertain".into(),
            estimated_remaining_seconds: None,
            range_seconds: None,
            confidence: "none".into(),
            basis: source.into(),
            explanation: "calibration has no usable throughput observation".into(),
        };
    };
    let estimate = work_units as f64 / rate;
    let relative = uncertainty.eta_relative_range.unwrap_or(0.25).max(0.02);
    CalibrationEta {
        status: "calibrated_estimate".into(),
        estimated_remaining_seconds: Some(estimate),
        range_seconds: Some([estimate * (1.0 - relative), estimate * (1.0 + relative)]),
        confidence: uncertainty.confidence.clone(),
        basis: source.into(),
        explanation: "range comes from this host/recipe/configuration calibration; it is not a completion promise".into(),
    }
}

pub(super) fn override_selection(
    representation_kind: &str,
    recipe_key: &str,
    overrides: &ResourceOverrides,
    history_key: String,
) -> Result<ResourceSelection> {
    validate_resource_overrides(representation_kind, overrides)?;
    Ok(ResourceSelection {
        representation_kind: representation_kind.into(),
        recipe_key: recipe_key.into(),
        source: "explicit_override".into(),
        state: "explicit".into(),
        encoder: overrides.encoder.clone(),
        worker_count: overrides.worker_count.unwrap_or(1),
        vad_threads: overrides.vad_threads.unwrap_or(DEFAULT_DECODE_THREADS),
        decode_threads: overrides.decode_threads.unwrap_or(DEFAULT_DECODE_THREADS),
        calibration_key: None,
        timing: None,
        uncertainty: CalibrationUncertainty {
            confidence: "explicit".into(),
            timing_relative_spread: None,
            eta_relative_range: None,
            note: "settings came from an explicit override; no calibrated ETA is claimed".into(),
        },
        eta: CalibrationEta {
            status: "explicit_override_no_eta".into(),
            estimated_remaining_seconds: None,
            range_seconds: None,
            confidence: "explicit".into(),
            basis: "explicit_override".into(),
            explanation: "explicit settings do not imply a measured throughput estimate".into(),
        },
        history_key,
        sample_compatibility: "unknown".into(),
    })
}

pub(super) fn per_worker_rate(timing: &CalibrationTiming, worker_count: usize) -> Option<f64> {
    let aggregate = timing.work_units_per_second.filter(|rate| *rate > 0.0)?;
    Some(aggregate / worker_count.max(1) as f64)
}

pub(super) fn widen_uncertainty_for_sample(
    mut uncertainty: CalibrationUncertainty,
    compatibility: &str,
) -> CalibrationUncertainty {
    match compatibility {
        "generated_contract_only" => {
            if uncertainty.confidence == "measured" {
                uncertainty.confidence = "low_sample_count".into();
            }
            uncertainty.eta_relative_range =
                Some(uncertainty.eta_relative_range.unwrap_or(0.25).max(0.5));
            uncertainty.note.push_str(
                "; the synthetic 320x180 sample validates the proxy contract but is not a representative 4K/HEVC corpus sample, so the ETA range is deliberately wide",
            );
        }
        "unknown" => {
            uncertainty.confidence = "low_sample_count".into();
            uncertainty.eta_relative_range =
                Some(uncertainty.eta_relative_range.unwrap_or(0.25).max(1.0));
            uncertainty.note.push_str(
                "; sample compatibility is unknown, so the ETA range is widened and must not be treated as coverage",
            );
        }
        _ => {}
    }
    uncertainty
}
