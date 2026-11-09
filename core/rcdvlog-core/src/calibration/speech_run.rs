//! `Catalog::calibrate_speech` and its engine-factory seam.

use super::*;

impl Catalog {
    /// Explicit bounded VAD calibration. It requires already-installed models;
    /// it never downloads. A generated sample without detected speech is
    /// reported as not-ready rather than fabricating a winning setting.
    #[allow(clippy::result_large_err)]
    pub fn calibrate_speech(
        &self,
        options: &SpeechCalibrationOptions,
    ) -> Result<ResourceCalibrationReport> {
        ensure!(
            !self.read_only,
            "calibration requires an explicitly writable catalog"
        );
        validate_trials(options.trials)?;
        validate_threads(&options.vad_threads, "VAD thread count")?;
        validate_speech_decode_threads(&options.decode_threads)?;
        validate_generated_duration(options.generated_duration_seconds)?;
        if let CalibrationMedia::Representative(path) = &options.media {
            validate_representative_media_metadata(path)?;
        }
        let sample_lease = if matches!(&options.media, CalibrationMedia::Representative(_)) {
            Some(self.acquire_local_heavy()?)
        } else {
            None
        };
        let mut candidates = Vec::new();
        for vad_threads in &options.vad_threads {
            candidates.push(CalibrationCandidateSettings {
                label: format!("vad={vad_threads};decode={DEFAULT_DECODE_THREADS};workers=1"),
                encoder: None,
                encoder_policy: None,
                worker_count: 1,
                vad_threads: *vad_threads,
                decode_threads: DEFAULT_DECODE_THREADS,
            });
        }
        candidates.sort();
        candidates.dedup();
        validate_candidate_budget(candidates.len(), options.trials)?;
        let models = self.model_status()?;
        let models_identity = model_identity(&models);
        let (media_identity, representative_path, representative_duration_ms) = match &options.media
        {
            CalibrationMedia::Generated => (
                CalibrationMediaIdentity {
                    kind: "generated".into(),
                    sample_identity: generated_sample_identity(
                        options.generated_duration_seconds,
                        "speech",
                    ),
                    duration_ms: options.generated_duration_seconds.saturating_mul(1000),
                    width: 0,
                    height: 0,
                    has_audio: true,
                },
                None,
                None,
            ),
            CalibrationMedia::Representative(path) => {
                let document = crate::probe_media_document(path)?;
                let duration_seconds = document
                    .get("format")
                    .and_then(|format| format.get("duration"))
                    .and_then(|duration| duration.as_str())
                    .and_then(|duration| duration.parse::<f64>().ok())
                    .filter(|duration| duration.is_finite() && *duration > 0.0)
                    .ok_or_else(|| anyhow!("calibration audio has no finite duration"))?;
                let duration_ms = (duration_seconds * 1000.0).round().min(u32::MAX as f64) as u32;
                validate_representative_duration(duration_ms)?;
                // Perform the full-content digest only after duration and
                // byte bounds pass. An arbitrary hours-long original is
                // never hashed/PCM-expanded as a bounded pilot.
                let (sample_identity, _) = representative_sample_identity(path)?;
                (
                    CalibrationMediaIdentity {
                        kind: "authorized_representative".into(),
                        sample_identity,
                        duration_ms,
                        width: 0,
                        height: 0,
                        has_audio: true,
                    },
                    Some(path.clone()),
                    Some(duration_ms),
                )
            }
        };
        let mut identity = CalibrationIdentity {
            schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
            recipe_key: STT_RECIPE_KEY.into(),
            recipe_version: RESOURCE_CALIBRATION_RECIPE_VERSION.into(),
            representation_kind: SPEECH_REPRESENTATION_KIND.into(),
            host: host_identity(),
            tools: tool_identities(),
            devices: hardware_device_identities(),
            models: models_identity,
            media: media_identity,
            conditions: CalibrationConditions {
                trials: options.trials,
                local_heavy_slots: 1,
                generated_duration_seconds: matches!(options.media, CalibrationMedia::Generated)
                    .then_some(options.generated_duration_seconds),
                note: "VAD candidates use fixed decode threads and are compared only against the same baseline regions; missing models are not downloaded".into(),
            },
            candidates: candidates.clone(),
            configuration_key: calibration_configuration_key(options.media.kind(), &candidates, None)?,
        };
        let cache_key = identity.cache_key()?;
        if !options.force
            && let Some(report) = self.exact_complete_calibration(&cache_key)?
        {
            return Ok(report);
        }
        let not_ready: Vec<String> = models
            .iter()
            .filter(|model| !model.ready)
            .map(|model| format!("{} ({})", model.name, model.integrity))
            .collect();
        if !not_ready.is_empty() {
            self.begin_resource_calibration(&identity, &cache_key)?;
            let started_at = timestamp();
            let mut report = not_ready_calibration_report(
                cache_key,
                identity,
                started_at,
                vec![],
                "speech models are not installed/ready",
                format!(
                    "speech models are not ready: {}; install them explicitly and rerun calibration",
                    not_ready.join(", ")
                ),
            );
            self.persist_resource_calibration(&mut report)?;
            return Ok(report);
        }

        let _heavy = match sample_lease {
            Some(lease) => lease,
            None => self.acquire_local_heavy()?,
        };
        self.begin_resource_calibration(&identity, &cache_key)?;
        let started_at = timestamp();
        for (identity_model, model) in identity.models.iter_mut().zip(models.iter()) {
            identity_model.content_digest = model_content_digest(Path::new(&model.path)).0;
        }
        let scratch = (representative_path.is_none())
            .then(|| create_scratch_root(self))
            .transpose()?;
        let generated_path = scratch
            .as_ref()
            .map(|scratch| {
                run_ffmpeg_fixture(&scratch.0, options.generated_duration_seconds, false, true)
            })
            .transpose()?;
        let source_path = match &options.media {
            CalibrationMedia::Generated => generated_path
                .as_ref()
                .ok_or_else(|| anyhow!("generated calibration audio was not created"))?,
            CalibrationMedia::Representative(_) => representative_path
                .as_ref()
                .ok_or_else(|| anyhow!("authorized representative media path missing"))?,
        };
        let duration_ms = match &options.media {
            CalibrationMedia::Generated => options.generated_duration_seconds.saturating_mul(1000),
            CalibrationMedia::Representative(_) => representative_duration_ms
                .ok_or_else(|| anyhow!("representative media duration missing"))?,
        };
        let pcm = match decode_calibration_pcm(source_path) {
            Ok(pcm) => pcm,
            Err(error) => {
                let mut report = failed_calibration_report(
                    cache_key,
                    identity,
                    started_at,
                    vec![],
                    format!("decode calibration audio: {error:#}"),
                );
                self.persist_resource_calibration(&mut report)?;
                return Ok(report);
            }
        };
        let model_dir = crate::analysis::engine::model_dir(self.database_path())?;
        let whisper = model_dir.join(WHISPER_MODEL.file_name);
        let vad = model_dir.join(VAD_MODEL.file_name);
        let run = self.run_speech_calibration_with_factory(
            options,
            &identity,
            &cache_key,
            &started_at,
            &pcm,
            duration_ms,
            |vad_threads, decode_threads| {
                Ok(Box::new(WhisperEngine::with_threads(
                    &whisper,
                    &vad,
                    vad_threads,
                    decode_threads,
                )?) as Box<dyn SttEngine>)
            },
        );
        match run {
            Ok(report) => Ok(report),
            Err(error) => {
                let mut report = failed_calibration_report(
                    cache_key,
                    identity,
                    started_at,
                    vec![],
                    format!("{error:#}"),
                );
                self.persist_resource_calibration(&mut report)?;
                Ok(report)
            }
        }
    }

    /// Shared production/fake-engine implementation. The closure is the only
    /// engine seam; candidate validation, timing, region quality comparisons,
    /// selection and persistence are identical for real and fake engines.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_speech_calibration_with_factory<F>(
        &self,
        options: &SpeechCalibrationOptions,
        identity: &CalibrationIdentity,
        cache_key: &str,
        started_at: &str,
        pcm: &[f32],
        duration_ms: u32,
        mut open_engine: F,
    ) -> Result<ResourceCalibrationReport>
    where
        F: FnMut(usize, usize) -> Result<Box<dyn SttEngine>>,
    {
        let mut measurements = Vec::new();
        let mut regions_by_label: HashMap<String, Vec<SpeechRegion>> = HashMap::new();
        for candidate in &identity.candidates {
            self.touch_resource_calibration(cache_key).ok();
            let mut durations = Vec::new();
            let mut failures = Vec::new();
            let mut candidate_regions: Option<Vec<SpeechRegion>> = None;
            for _trial in 0..options.trials {
                let engine = match open_engine(candidate.vad_threads, candidate.decode_threads) {
                    Ok(engine) => engine,
                    Err(error) => {
                        failures.push(bounded_text(
                            &format!("load speech engine failed: {error:#}"),
                            512,
                        ));
                        continue;
                    }
                };
                let started = Instant::now();
                match engine.detect_speech(pcm) {
                    Ok(regions) => {
                        let (valid, reason) = speech_regions_valid(&regions, duration_ms);
                        if !valid {
                            failures.push(reason);
                        } else {
                            if let Some(previous) = &candidate_regions {
                                let comparison =
                                    compare_speech_regions(previous, &regions, duration_ms);
                                if !comparison.equivalent {
                                    failures.push(
                                        "VAD regions changed between trials for the same candidate"
                                            .into(),
                                    );
                                }
                            }
                            candidate_regions = Some(regions);
                            durations.push(
                                started
                                    .elapsed()
                                    .as_millis()
                                    .max(1)
                                    .min(u128::from(u64::MAX))
                                    as u64,
                            );
                        }
                    }
                    Err(error) => {
                        failures.push(bounded_text(&format!("VAD failed: {error:#}"), 512))
                    }
                }
            }
            let timing = CalibrationTiming::from_samples(durations, Some(1));
            let output_identity = candidate_regions
                .as_ref()
                .map(|regions| region_digest(regions));
            if let Some(regions) = candidate_regions.take() {
                regions_by_label.insert(candidate.label.clone(), regions);
            }
            measurements.push(CalibrationCandidateOutcome {
                settings: candidate.clone(),
                succeeded: timing.is_some() && failures.is_empty(),
                timing,
                failures,
                output_identity,
                quality: None,
                render_contract_equivalent: None,
            });
        }
        let baseline_index = measurements.iter().position(|measurement| {
            measurement.succeeded
                && regions_by_label
                    .get(&measurement.settings.label)
                    .is_some_and(|regions| !regions.is_empty())
        });
        let Some(baseline_index) = baseline_index else {
            let empty_valid_measurement = measurements.iter().any(|measurement| {
                measurement.succeeded
                    && regions_by_label
                        .get(&measurement.settings.label)
                        .is_some_and(Vec::is_empty)
            });
            let note = if empty_valid_measurement {
                "the calibration sample contained no detected speech"
            } else {
                "no VAD candidate produced a successful valid baseline region set"
            };
            let state = if empty_valid_measurement {
                CalibrationState::NotReady
            } else {
                CalibrationState::Failed
            };
            let mut report = ResourceCalibrationReport {
                schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
                cache_key: cache_key.to_string(),
                state,
                identity: identity.clone(),
                started_at: started_at.to_string(),
                completed_at: Some(timestamp()),
                candidates: measurements,
                selected_settings: None,
                selection_reason: "no VAD setting was adopted".into(),
                quality: None,
                uncertainty: CalibrationUncertainty {
                    confidence: "none".into(),
                    timing_relative_spread: None,
                    eta_relative_range: None,
                    note: note.into(),
                },
                error: Some(format!("{note}; no VAD thread setting was adopted")),
            };
            self.persist_resource_calibration(&mut report)?;
            return Ok(report);
        };
        let baseline_label = measurements[baseline_index].settings.label.clone();
        let baseline_regions = regions_by_label
            .get(&baseline_label)
            .cloned()
            .ok_or_else(|| anyhow!("baseline regions disappeared"))?;
        for measurement in &mut measurements {
            if let Some(regions) = regions_by_label.get(&measurement.settings.label) {
                let comparison = compare_speech_regions(&baseline_regions, regions, duration_ms);
                if measurement.settings.label == baseline_label {
                    measurement.quality = Some(SpeechRegionComparison {
                        equivalent: comparison.reference_valid,
                        ..comparison
                    });
                } else if measurement.succeeded && comparison.equivalent {
                    measurement.quality = Some(comparison);
                } else if measurement.succeeded {
                    measurement.failures.push(comparison.reason.clone());
                    measurement.succeeded = false;
                    measurement.quality = Some(comparison);
                }
            }
        }
        let baseline = &measurements[baseline_index];
        let mut adopted = None;
        for measurement in &measurements {
            if !measurement.succeeded || measurement.settings.label == baseline_label {
                continue;
            }
            if measurement
                .quality
                .as_ref()
                .is_some_and(|quality| quality.equivalent)
                && timing_better(measurement, baseline)
                && adopted
                    .as_ref()
                    .is_none_or(|current: &&CalibrationCandidateOutcome| {
                        timing_better(measurement, current)
                    })
            {
                adopted = Some(measurement);
            }
        }
        let adopted_label = adopted.map(|measurement| measurement.settings.label.clone());
        let selected_settings = adopted
            .map(|measurement| measurement.settings.clone())
            .unwrap_or_else(|| baseline.settings.clone());
        let selection_reason = match &adopted_label {
            Some(label) => {
                format!("selected the fastest valid quality-equivalent candidate {label}")
            }
            None => "baseline was the fastest valid quality-equivalent candidate".into(),
        };
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
            cache_key: cache_key.to_string(),
            state: CalibrationState::Complete,
            identity: identity.clone(),
            started_at: started_at.to_string(),
            completed_at: Some(timestamp()),
            candidates: measurements,
            selected_settings: Some(selected_settings),
            selection_reason,
            quality: Some(CalibrationQualityDecision {
                baseline_candidate: baseline_label,
                adopted_candidate: adopted_label,
                reason: "a faster VAD thread setting is adopted only when it has a successful, valid, measured region set exactly matching the baseline".into(),
            }),
            uncertainty,
            error: None,
        };
        self.persist_resource_calibration(&mut report)?;
        Ok(report)
    }
}
