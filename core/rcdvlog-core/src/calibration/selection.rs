//! `Catalog` methods that resolve resource selections, rate histories, and calibrated estimates.

use super::*;

impl Catalog {
    /// Resolve the actual settings and calibrated ETA for one representation
    /// kind. Explicit overrides are validated and reported as their own
    /// provenance; they never masquerade as calibrated measurements.
    pub fn resolve_resource_selection(
        &self,
        representation_kind: &str,
        recipe_key: &str,
        work_units: Option<u64>,
        media_sample_identity: Option<&str>,
        overrides: &ResourceOverrides,
    ) -> Result<ResourceSelection> {
        let any_override = overrides.encoder.is_some()
            || overrides.worker_count.is_some()
            || overrides.vad_threads.is_some()
            || overrides.decode_threads.is_some();
        let history_base = if representation_kind == SPEECH_REPRESENTATION_KIND {
            STT_TASK_KEY
        } else {
            representation_kind
        };
        let default_history_key = self.qualified_history_key(
            history_base,
            representation_kind,
            recipe_key,
            None,
            1,
            DEFAULT_DECODE_THREADS,
            DEFAULT_DECODE_THREADS,
        )?;
        if any_override {
            let mut selection = override_selection(
                representation_kind,
                recipe_key,
                overrides,
                default_history_key,
            )?;
            selection.history_key = self.qualified_history_key(
                history_base,
                representation_kind,
                recipe_key,
                selection.encoder.as_deref(),
                selection.worker_count,
                selection.vad_threads,
                selection.decode_threads,
            )?;
            return Ok(selection);
        }
        if let Some(report) = self.reusable_calibration_report(
            representation_kind,
            recipe_key,
            media_sample_identity,
        )? {
            let selected = report
                .selected_settings
                .clone()
                .ok_or_else(|| anyhow!("complete calibration has no selected settings"))?;
            let measured_timing = report
                .candidates
                .iter()
                .find(|candidate| candidate.settings == selected)
                .and_then(|candidate| candidate.timing.clone());
            let sample_compatibility = match report.identity.media.kind.as_str() {
                "generated"
                    if media_sample_identity
                        == Some(report.identity.media.sample_identity.as_str()) =>
                {
                    "exact_generated"
                }
                "generated" => "generated_contract_only",
                "authorized_representative"
                    if media_sample_identity
                        == Some(report.identity.media.sample_identity.as_str()) =>
                {
                    "exact_representative"
                }
                _ => "unknown",
            }
            .to_string();
            // A synthetic contract check is not a corpus throughput fit.
            // Preserve measured timings in the report; only an explicitly
            // matching sample may seed an ETA through this selection API.
            let timing = if sample_compatibility.starts_with("exact_") {
                measured_timing
            } else {
                None
            };
            let uncertainty =
                widen_uncertainty_for_sample(report.uncertainty, &sample_compatibility);
            let eta = eta_from_timing(
                timing.as_ref(),
                work_units,
                &uncertainty,
                "matching_host_calibration",
            );
            let worker_count = selected.worker_count.max(1);
            let vad_threads = selected.vad_threads.max(1);
            let decode_threads = selected.decode_threads.max(1);
            let history_key = self.qualified_history_key(
                history_base,
                representation_kind,
                recipe_key,
                selected.encoder.as_deref(),
                worker_count,
                vad_threads,
                decode_threads,
            )?;
            return Ok(ResourceSelection {
                representation_kind: representation_kind.into(),
                recipe_key: recipe_key.into(),
                source: "calibration".into(),
                state: "complete".into(),
                encoder: selected.encoder,
                worker_count,
                vad_threads,
                decode_threads,
                calibration_key: Some(report.cache_key),
                timing,
                uncertainty,
                eta,
                history_key,
                sample_compatibility,
            });
        }
        // Deliberately do not call a hardware probe here. Readiness, speech,
        // read-only inspection and ordinary batch planning must not benchmark
        // an encoder merely because no calibration exists.
        let encoder =
            (representation_kind == PROXY_REPRESENTATION_KIND).then(|| "software".to_string());
        Ok(ResourceSelection {
            representation_kind: representation_kind.into(),
            recipe_key: recipe_key.into(),
            source: "default_uncalibrated".into(),
            state: "missing".into(),
            encoder,
            worker_count: 1,
            vad_threads: DEFAULT_DECODE_THREADS,
            decode_threads: DEFAULT_DECODE_THREADS,
            calibration_key: None,
            timing: None,
            uncertainty: CalibrationUncertainty {
                confidence: "none".into(),
                timing_relative_spread: None,
                eta_relative_range: None,
                note: "no matching complete host/tool/recipe/configuration calibration exists"
                    .into(),
            },
            eta: CalibrationEta {
                status: "uncertain_missing_calibration".into(),
                estimated_remaining_seconds: None,
                range_seconds: None,
                confidence: "none".into(),
                basis: "default_uncalibrated".into(),
                explanation:
                    "run explicit `rcdvlog calibrate run` before relying on a calibrated ETA".into(),
            },
            history_key: default_history_key,
            sample_compatibility: "unknown".into(),
        })
    }

    /// Seed a BatchEstimator only from a matching completed calibration. This
    /// does not create a synthetic sample count and does not turn explicit
    /// overrides into measured throughput.
    pub fn calibration_rate_history(
        &self,
        representation_kind: &str,
        recipe_key: &str,
        encoder: Option<&str>,
        worker_count: usize,
        media_sample_identity: Option<&str>,
    ) -> Result<RateHistory> {
        let Some(sample_identity) = media_sample_identity else {
            return Ok(RateHistory::default());
        };
        let Some(report) = self.reusable_calibration_report(
            representation_kind,
            recipe_key,
            Some(sample_identity),
        )?
        else {
            return Ok(RateHistory::default());
        };
        let Some(selected) = report.selected_settings.as_ref() else {
            return Ok(RateHistory::default());
        };
        if selected.worker_count != worker_count {
            return Ok(RateHistory::default());
        }
        if let Some(encoder) = encoder
            && selected.encoder.as_deref() != Some(encoder)
        {
            return Ok(RateHistory::default());
        }
        let Some(timing) = report
            .candidates
            .iter()
            .find(|candidate| candidate.settings == *selected)
            .and_then(|candidate| candidate.timing.as_ref())
        else {
            return Ok(RateHistory::default());
        };
        let Some(rate) = per_worker_rate(timing, selected.worker_count.max(1)) else {
            return Ok(RateHistory::default());
        };
        Ok(RateHistory {
            work_units_per_worker_second: rate,
            samples: timing.samples_ms.len() as u64,
        })
    }

    /// Seed an ETA estimate for a prepared work list. The returned estimate is
    /// deliberately uncertain (`None` rate) when calibration is missing,
    /// explicitly overridden, or measured for a different encoder/worker
    /// configuration. A range is attached whenever a calibrated rate is used.
    #[allow(clippy::too_many_arguments)]
    pub fn calibrated_batch_estimate(
        &self,
        representation_kind: &str,
        recipe_key: &str,
        task_work_units: &[u64],
        worker_count: usize,
        encoder: Option<&str>,
        media_sample_identity: Option<&str>,
        overrides: &ResourceOverrides,
    ) -> Result<BatchProgressEstimate> {
        let total_work = task_work_units.iter().copied().sum::<u64>();
        let selection = self.resolve_resource_selection(
            representation_kind,
            recipe_key,
            Some(total_work),
            media_sample_identity,
            overrides,
        )?;
        let compatible = selection.source == "calibration"
            && worker_count > 0
            && selection.worker_count == worker_count
            && match encoder {
                Some(encoder) => selection.encoder.as_deref() == Some(encoder),
                None => true,
            };
        let estimator = BatchEstimator::new(
            task_work_units,
            worker_count,
            if compatible {
                selection
                    .timing
                    .as_ref()
                    .and_then(|timing| per_worker_rate(timing, worker_count.max(1)))
                    .map(|rate| RateHistory {
                        work_units_per_worker_second: rate,
                        samples: selection
                            .timing
                            .as_ref()
                            .map(|timing| timing.samples_ms.len() as u64)
                            .unwrap_or(0),
                    })
                    .unwrap_or_default()
            } else {
                RateHistory::default()
            },
        );
        let mut estimate = estimator.estimate(task_work_units);
        if compatible {
            if let Some(seconds) = estimate.estimated_remaining_seconds {
                let relative = selection
                    .uncertainty
                    .eta_relative_range
                    .unwrap_or(0.25)
                    .max(0.02);
                estimate.confidence = selection.uncertainty.confidence.clone();
                estimate.estimated_remaining_seconds_range = Some([
                    (seconds * (1.0 - relative)).max(0.0),
                    seconds * (1.0 + relative),
                ]);
            }
        } else {
            estimate.confidence = "mismatched_configuration".into();
            estimate.estimated_remaining_seconds = None;
            estimate.estimated_remaining_seconds_range = None;
        }
        Ok(estimate)
    }
}
