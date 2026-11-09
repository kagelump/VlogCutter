//! Proxy candidate plans, proxy quality equivalence, and speech-region comparison.

use super::*;

pub(super) fn proxy_candidates(
    policy: ProxyEncoderPolicy,
    worker_counts: &[usize],
) -> Vec<CalibrationCandidateSettings> {
    // Candidate discovery is declarative. Resolving a usable hardware target
    // runs a synthetic probe and must happen only inside the heavy lease.
    let policies: &[ProxyEncoderPolicy] = match policy {
        ProxyEncoderPolicy::Software => &[ProxyEncoderPolicy::Software],
        ProxyEncoderPolicy::Auto => &[ProxyEncoderPolicy::Auto, ProxyEncoderPolicy::Software],
    };
    let mut candidates = Vec::new();
    for policy in policies {
        let encoder = match policy {
            ProxyEncoderPolicy::Auto => "auto",
            ProxyEncoderPolicy::Software => "software",
        };
        for worker_count in worker_counts {
            candidates.push(CalibrationCandidateSettings {
                label: format!(
                    "policy={};encoder={encoder};workers={worker_count}",
                    policy.as_str()
                ),
                encoder: Some(encoder.into()),
                encoder_policy: Some(policy.as_str().into()),
                worker_count: *worker_count,
                vad_threads: 0,
                decode_threads: 0,
            });
        }
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

pub(super) fn plan_for_candidate(
    policy: ProxyEncoderPolicy,
    candidate: &CalibrationCandidateSettings,
) -> ProxyEncoderPlan {
    match candidate.encoder_policy.as_deref() {
        Some("software") => resolve_proxy_encoder_plan(ProxyEncoderPolicy::Software),
        _ => resolve_proxy_encoder_plan(policy),
    }
}

pub(super) fn proxy_quality_equivalent(
    baseline: &ProxyGenerationOutcome,
    candidate: &ProxyGenerationOutcome,
    source_duration_seconds: f64,
) -> Result<()> {
    let baseline_validation = baseline
        .validation
        .as_ref()
        .ok_or_else(|| anyhow!("baseline output has no validation"))?;
    let candidate_validation = candidate
        .validation
        .as_ref()
        .ok_or_else(|| anyhow!("candidate output has no validation"))?;
    ensure!(
        baseline_validation.decoded && candidate_validation.decoded,
        "calibration output did not fully decode"
    );
    ensure!(
        candidate_validation.expected_duration_seconds > 0.0
            && (candidate_validation.duration_seconds - source_duration_seconds).abs()
                <= candidate_validation.duration_tolerance_seconds.max(0.25),
        "candidate output duration does not correspond to the source"
    );
    ensure!(
        baseline_validation.width == candidate_validation.width
            && baseline_validation.height == candidate_validation.height
            && baseline_validation.codec == candidate_validation.codec
            && baseline_validation.pixel_format == candidate_validation.pixel_format,
        "candidate output render contract differs from the baseline"
    );
    ensure!(
        baseline_validation.source_audio_present == candidate_validation.source_audio_present
            && baseline_validation.audio_stream_count == candidate_validation.audio_stream_count,
        "candidate output audio contract differs from the baseline"
    );
    Ok(())
}

pub(super) fn speech_regions_valid(regions: &[SpeechRegion], duration_ms: u32) -> (bool, String) {
    let mut previous_end = 0_u32;
    for (index, region) in regions.iter().enumerate() {
        if region.start_ms >= region.end_ms {
            return (false, format!("region {index} has a non-positive duration"));
        }
        if region.start_ms >= duration_ms || region.end_ms > duration_ms {
            return (
                false,
                format!("region {index} is outside the decoded audio duration"),
            );
        }
        if index > 0 && region.start_ms < previous_end {
            return (
                false,
                format!("region {index} overlaps or is unordered relative to its predecessor"),
            );
        }
        previous_end = region.end_ms;
    }
    (true, "valid".to_string())
}

pub(super) fn region_digest(regions: &[SpeechRegion]) -> String {
    let intervals: Vec<[u32; 2]> = regions
        .iter()
        .map(|region| [region.start_ms, region.end_ms])
        .collect();
    blake3::hash(&serde_json::to_vec(&intervals).unwrap_or_default())
        .to_hex()
        .to_string()
}

/// Compare detected regions against the baseline before adopting a faster VAD
/// thread count. Exact intervals are required: a faster run that drops,
/// adds, reorders, overlaps, leaves range, or shifts a boundary is rejected.
pub fn compare_speech_regions(
    reference: &[SpeechRegion],
    candidate: &[SpeechRegion],
    duration_ms: u32,
) -> SpeechRegionComparison {
    let (reference_valid, reference_reason) = speech_regions_valid(reference, duration_ms);
    let (candidate_valid, candidate_reason) = speech_regions_valid(candidate, duration_ms);
    let reference_interval: Vec<[u32; 2]> = reference
        .iter()
        .map(|region| [region.start_ms, region.end_ms])
        .collect();
    let candidate_interval: Vec<[u32; 2]> = candidate
        .iter()
        .map(|region| [region.start_ms, region.end_ms])
        .collect();
    let mismatch_count = reference_interval
        .iter()
        .zip(candidate_interval.iter())
        .filter(|(left, right)| left != right)
        .count()
        + reference_interval.len().abs_diff(candidate_interval.len());
    let differences = reference_interval
        .iter()
        .zip(candidate_interval.iter())
        .enumerate()
        .filter_map(|(index, (left, right))| {
            (left != right).then_some(SpeechRegionDifference {
                index,
                reference: Some(*left),
                candidate: Some(*right),
            })
        })
        .chain(
            reference_interval
                .iter()
                .skip(candidate_interval.len())
                .enumerate()
                .map(|(offset, left)| SpeechRegionDifference {
                    index: candidate_interval.len() + offset,
                    reference: Some(*left),
                    candidate: None,
                }),
        )
        .chain(
            candidate_interval
                .iter()
                .skip(reference_interval.len())
                .enumerate()
                .map(|(offset, right)| SpeechRegionDifference {
                    index: reference_interval.len() + offset,
                    reference: None,
                    candidate: Some(*right),
                }),
        )
        .take(16)
        .collect::<Vec<_>>();
    let reason = if !reference_valid {
        format!("baseline regions invalid: {reference_reason}")
    } else if !candidate_valid {
        format!("candidate regions invalid: {candidate_reason}")
    } else if mismatch_count == 0 {
        "candidate detected the same validated speech regions as the baseline".to_string()
    } else {
        format!("candidate detected different speech regions ({mismatch_count} mismatches)")
    };
    SpeechRegionComparison {
        reference_valid,
        candidate_valid,
        equivalent: reference_valid && candidate_valid && mismatch_count == 0,
        duration_ms,
        reference_region_count: reference.len(),
        candidate_region_count: candidate.len(),
        mismatch_count,
        reference_digest: region_digest(reference),
        candidate_digest: region_digest(candidate),
        reason,
        differences,
    }
}
