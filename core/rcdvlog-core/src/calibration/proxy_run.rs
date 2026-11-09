//! `Catalog::calibrate_proxy`.

use super::*;

impl Catalog {
    /// Explicit bounded proxy calibration. With `CalibrationMedia::Generated`
    /// this creates and removes synthetic media under managed cache scratch.
    #[allow(clippy::result_large_err)]
    pub fn calibrate_proxy(
        &self,
        options: &ProxyCalibrationOptions,
    ) -> Result<ResourceCalibrationReport> {
        ensure!(
            !self.read_only,
            "calibration requires an explicitly writable catalog"
        );
        validate_trials(options.trials)?;
        validate_worker_counts(&options.worker_counts)?;
        validate_generated_duration(options.generated_duration_seconds)?;
        if let CalibrationMedia::Representative(path) = &options.media {
            validate_representative_media_metadata(path)?;
        }
        let sample_lease = if matches!(&options.media, CalibrationMedia::Representative(_)) {
            Some(self.acquire_local_heavy()?)
        } else {
            None
        };
        let candidates = proxy_candidates(options.policy, &options.worker_counts);
        validate_candidate_budget(candidates.len(), options.trials)?;
        let (media_identity, representative_path) = match &options.media {
            CalibrationMedia::Generated => (
                CalibrationMediaIdentity {
                    kind: "generated".into(),
                    sample_identity: generated_sample_identity(
                        options.generated_duration_seconds,
                        "proxy",
                    ),
                    duration_ms: options.generated_duration_seconds.saturating_mul(1000),
                    width: 320,
                    height: 180,
                    has_audio: true,
                },
                None,
            ),
            CalibrationMedia::Representative(path) => {
                let facts = probe_source_facts(path)?;
                validate_representative_duration(facts.duration_ms)?;
                let (sample_identity, _) = representative_sample_identity(path)?;
                (
                    CalibrationMediaIdentity {
                        kind: "authorized_representative".into(),
                        sample_identity,
                        duration_ms: facts.duration_ms,
                        width: facts.width,
                        height: facts.height,
                        has_audio: facts.has_audio,
                    },
                    Some(path.clone()),
                )
            }
        };
        let recipe_key = PROXY_SOFTWARE_RECIPE_KEY;
        let identity = CalibrationIdentity {
            schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
            recipe_key: recipe_key.into(),
            recipe_version: RESOURCE_CALIBRATION_RECIPE_VERSION.into(),
            representation_kind: PROXY_REPRESENTATION_KIND.into(),
            host: host_identity(),
            tools: tool_identities(),
            devices: hardware_device_identities(),
            models: vec![],
            media: media_identity,
            conditions: CalibrationConditions {
                trials: options.trials,
                local_heavy_slots: 1,
                generated_duration_seconds: matches!(options.media, CalibrationMedia::Generated)
                    .then_some(options.generated_duration_seconds),
                note: "single shared local-heavy slot; concurrent worker candidates are measured inside that one slot".into(),
            },
            candidates: candidates.clone(),
            configuration_key: calibration_configuration_key(
                options.media.kind(),
                &candidates,
                Some(options.policy),
            )?,
        };
        let cache_key = identity.cache_key()?;
        if !options.force
            && let Some(report) = self.exact_complete_calibration(&cache_key)?
        {
            return Ok(report);
        }
        let _heavy = match sample_lease {
            Some(lease) => lease,
            None => self.acquire_local_heavy()?,
        };
        self.begin_resource_calibration(&identity, &cache_key)?;
        let started_at = timestamp();
        let mut measurements: Vec<CalibrationCandidateOutcome> = Vec::new();
        let mut accepted_reference: Option<(String, ProxyGenerationOutcome)> = None;

        let run = (|| -> Result<()> {
            let scratch = (representative_path.is_none())
                .then(|| create_scratch_root(self))
                .transpose()?;
            let generated_path = scratch
                .as_ref()
                .map(|scratch| {
                    run_ffmpeg_fixture(&scratch.0, options.generated_duration_seconds, true, false)
                })
                .transpose()?;
            let source_path = match &options.media {
                CalibrationMedia::Generated => generated_path
                    .as_ref()
                    .ok_or_else(|| anyhow!("generated calibration media was not created"))?,
                CalibrationMedia::Representative(_) => representative_path
                    .as_ref()
                    .ok_or_else(|| anyhow!("authorized representative media path missing"))?,
            };
            let facts = if representative_path.is_some() {
                probe_source_facts(source_path)?
            } else {
                SourceFacts {
                    path: source_path.clone(),
                    duration_ms: options.generated_duration_seconds.saturating_mul(1000),
                    duration_seconds: f64::from(options.generated_duration_seconds),
                    width: 320,
                    height: 180,
                    has_audio: true,
                }
            };
            let ffmpeg = resolve_tool_path("ffmpeg");
            let ffprobe = resolve_tool_path("ffprobe");
            let scratch_work = scratch.as_ref().map(|scratch| &scratch.0);
            for candidate in &candidates {
                self.touch_resource_calibration(&cache_key)?;
                let mut durations = Vec::new();
                let mut failures = Vec::new();
                let mut output_identity = None;
                let mut render_contract_equivalent = None;
                let mut observed_encoder = None;
                let plan = plan_for_candidate(options.policy, candidate);
                for trial in 0..options.trials {
                    let trial_dir = scratch_work
                        .map(|root| {
                            root.join(format!(
                                "candidate-{}-trial-{trial}",
                                blake3::hash(candidate.label.as_bytes()).to_hex()
                            ))
                        })
                        .unwrap_or_else(|| {
                            std::env::temp_dir().join(format!(
                                "rcdvlog-calibration-{}",
                                uuid::Uuid::new_v4().simple()
                            ))
                        });
                    if let Err(error) = fs::create_dir_all(&trial_dir) {
                        failures.push(format!("create trial output directory: {error}"));
                        break;
                    }
                    let worker_count = candidate.worker_count.max(1);
                    let started = Instant::now();
                    let outputs = std::thread::scope(|scope| {
                        let mut handles = Vec::new();
                        for worker in 0..worker_count {
                            let plan = plan.clone();
                            let source_path = &facts.path;
                            let ffmpeg = &ffmpeg;
                            let ffprobe = &ffprobe;
                            let duration_seconds = facts.duration_seconds;
                            let has_audio = facts.has_audio;
                            let output = trial_dir.join(format!("worker-{worker}.mp4"));
                            handles.push(scope.spawn(move || {
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    run_proxy_transcode_with_plan_and_cancellation(
                                        ffmpeg,
                                        ffprobe,
                                        source_path,
                                        &output,
                                        duration_seconds,
                                        has_audio,
                                        &plan,
                                        |_| {},
                                        || started.elapsed() >= Duration::from_secs(120),
                                    )
                                }))
                                .unwrap_or_else(|_| {
                                    Err(crate::ProxyTranscodeFailure {
                                        message: "calibration transcode panicked".into(),
                                        outcome: ProxyGenerationOutcome {
                                            policy: ProxyEncoderPolicy::Software,
                                            status: "failed".into(),
                                            selected_encoder: "software".into(),
                                            recipe_key: PROXY_SOFTWARE_RECIPE_KEY.into(),
                                            device: None,
                                            fallback_used: false,
                                            elapsed_ms: 0,
                                            attempts: vec![],
                                            validation: None,
                                        },
                                    })
                                })
                            }));
                        }
                        handles
                            .into_iter()
                            .map(|handle| {
                                handle.join().unwrap_or_else(|_| {
                                    Err(crate::ProxyTranscodeFailure {
                                        message: "calibration worker panicked".into(),
                                        outcome: ProxyGenerationOutcome {
                                            policy: ProxyEncoderPolicy::Software,
                                            status: "failed".into(),
                                            selected_encoder: "software".into(),
                                            recipe_key: PROXY_SOFTWARE_RECIPE_KEY.into(),
                                            device: None,
                                            fallback_used: false,
                                            elapsed_ms: 0,
                                            attempts: vec![],
                                            validation: None,
                                        },
                                    })
                                })
                            })
                            .collect::<Vec<_>>()
                    });
                    let elapsed_ms = started
                        .elapsed()
                        .as_millis()
                        .max(1)
                        .min(u128::from(u64::MAX)) as u64;
                    let mut trial_successes = Vec::new();
                    for result in outputs {
                        match result {
                            Ok(outcome)
                                if outcome.status == "succeeded"
                                    && outcome
                                        .validation
                                        .as_ref()
                                        .is_some_and(|validation| validation.decoded) =>
                            {
                                trial_successes.push(outcome);
                            }
                            Ok(outcome) => failures
                                .push(format!("candidate returned status {}", outcome.status)),
                            Err(failure) => failures.push(bounded_text(&failure.message, 512)),
                        }
                    }
                    if trial_successes.len() == worker_count
                        && failures.is_empty()
                        && let Some(first) = trial_successes.first()
                    {
                        observed_encoder = Some(first.selected_encoder.clone());
                        match &accepted_reference {
                            Some((_, baseline)) => {
                                match proxy_quality_equivalent(
                                    baseline,
                                    first,
                                    facts.duration_seconds,
                                ) {
                                    Ok(()) => render_contract_equivalent = Some(true),
                                    Err(error) => {
                                        render_contract_equivalent = Some(false);
                                        failures.push(format!(
                                            "quality comparison rejected candidate: {error:#}"
                                        ));
                                    }
                                }
                            }
                            None => {
                                accepted_reference = Some((candidate.label.clone(), first.clone()));
                                render_contract_equivalent = Some(true);
                            }
                        }
                        output_identity = Some({
                            let mut hasher = blake3::Hasher::new();
                            for outcome in &trial_successes {
                                hasher.update(outcome.selected_encoder.as_bytes());
                                hasher.update(&outcome.elapsed_ms.to_le_bytes());
                                if let Some(validation) = &outcome.validation {
                                    hasher.update(validation.codec.as_bytes());
                                    hasher.update(&validation.width.to_le_bytes());
                                    hasher.update(&validation.height.to_le_bytes());
                                }
                            }
                            hasher.finalize().to_hex().to_string()
                        });
                        durations.push(elapsed_ms);
                    }
                    let _ = fs::remove_dir_all(&trial_dir);
                }
                let work_units = facts
                    .path
                    .metadata()
                    .map(|metadata| metadata.len().saturating_mul(candidate.worker_count as u64))
                    .ok();
                let timing = CalibrationTiming::from_samples(durations, work_units);
                let success = timing.is_some() && failures.is_empty();
                let mut measured_settings = candidate.clone();
                if let Some(encoder) = observed_encoder {
                    measured_settings.encoder = Some(encoder);
                }
                measurements.push(CalibrationCandidateOutcome {
                    settings: measured_settings,
                    succeeded: success,
                    timing,
                    failures,
                    output_identity,
                    quality: None,
                    render_contract_equivalent,
                });
            }
            Ok(())
        })();

        let (baseline_label, baseline_outcome) = match (accepted_reference, &run) {
            (Some((label, _)), Ok(())) => {
                let baseline = measurements.iter().find(|measurement| {
                    measurement.settings.label == label && measurement.succeeded
                });
                match baseline {
                    Some(baseline) => (label, baseline),
                    None => {
                        let mut report = failed_calibration_report(
                            cache_key.clone(),
                            identity.clone(),
                            started_at,
                            measurements.clone(),
                            "baseline proxy candidate did not complete all trials".into(),
                        );
                        self.persist_resource_calibration(&mut report)?;
                        return Ok(report);
                    }
                }
            }
            (_, Err(error)) => {
                let mut report = failed_calibration_report(
                    cache_key.clone(),
                    identity.clone(),
                    started_at,
                    measurements.clone(),
                    format!("{error:#}"),
                );
                self.persist_resource_calibration(&mut report)?;
                return Ok(report);
            }
            _ => {
                let mut report = failed_calibration_report(
                    cache_key.clone(),
                    identity.clone(),
                    started_at,
                    measurements.clone(),
                    "no proxy candidate produced a valid baseline output".into(),
                );
                self.persist_resource_calibration(&mut report)?;
                return Ok(report);
            }
        };
        let baseline_settings = baseline_outcome.settings.clone();
        let mut adopted = None;
        let mut selection_reason = "no candidate improved on the validated baseline".to_string();
        for measurement in &measurements {
            if !measurement.succeeded || measurement.settings.label == baseline_label {
                continue;
            }
            if measurement.render_contract_equivalent != Some(true) {
                continue;
            }
            if timing_better(measurement, baseline_outcome)
                && adopted
                    .as_ref()
                    .is_none_or(|current: &&CalibrationCandidateOutcome| {
                        timing_better(measurement, current)
                    })
            {
                adopted = Some(measurement);
            }
        }
        if let Some(adopted) = adopted {
            selection_reason = format!(
                "selected the fastest quality-equivalent candidate {}",
                adopted.settings.label
            );
        }
        let adopted_label = adopted.map(|adopted| adopted.settings.label.clone());
        let selected_settings = adopted
            .map(|adopted| adopted.settings.clone())
            .unwrap_or(baseline_settings);
        let selected_timing = measurements
            .iter()
            .find(|measurement| measurement.settings == selected_settings)
            .and_then(|measurement| measurement.timing.as_ref());
        let uncertainty = selected_timing
            .map(|timing| uncertainty_for(timing, timing.samples_ms.len()))
            .unwrap_or_else(|| CalibrationUncertainty {
                confidence: "none".into(),
                timing_relative_spread: None,
                eta_relative_range: None,
                note: "no valid timing sample".into(),
            });
        let mut report = ResourceCalibrationReport {
            schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
            cache_key,
            state: CalibrationState::Complete,
            identity,
            started_at,
            completed_at: Some(timestamp()),
            candidates: measurements,
            selected_settings: Some(selected_settings),
            selection_reason,
            quality: Some(CalibrationQualityDecision {
                baseline_candidate: baseline_label,
                adopted_candidate: adopted_label,
                reason: "proxy candidates were accepted only when existing output validation and render/audio contracts matched the baseline".into(),
            }),
            uncertainty,
            error: None,
        };
        self.persist_resource_calibration(&mut report)?;
        Ok(report)
    }
}
