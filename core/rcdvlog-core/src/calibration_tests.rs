use super::*;

fn region(start_ms: u32, end_ms: u32) -> SpeechRegion {
    SpeechRegion { start_ms, end_ms }
}

#[test]
fn explicit_speech_thread_overrides_have_distinct_actual_history_keys() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let selection = |vad_threads, decode_threads| {
        catalog
            .resolve_resource_selection(
                SPEECH_REPRESENTATION_KIND,
                STT_RECIPE_KEY,
                None,
                None,
                &ResourceOverrides {
                    vad_threads: Some(vad_threads),
                    decode_threads: Some(decode_threads),
                    ..ResourceOverrides::default()
                },
            )
            .unwrap()
    };
    assert_ne!(selection(1, 4).history_key, selection(2, 4).history_key);
    assert_ne!(selection(2, 4).history_key, selection(2, 8).history_key);
    assert_eq!(selection(2, 8).source, "explicit_override");
}

#[cfg(unix)]
#[test]
fn another_live_process_is_not_interrupted_without_proc_metadata() {
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let live = owner_is_live(Some("external-owner"), Some(child.id()), None, None);
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(live, "portable liveness must not require Linux /proc");
}

#[test]
fn full_model_content_evidence_does_not_change_the_lightweight_cache_key() {
    let mut identity = fake_proxy_identity();
    identity.models.push(CalibrationModelIdentity {
        role: "voice_activity_detection".into(),
        name: "synthetic-model".into(),
        integrity: "present_unverified;stat:synthetic".into(),
        bytes: 10,
        content_digest: None,
    });
    let key = identity.cache_key().unwrap();
    identity.models[0].content_digest = Some("blake3:synthetic-evidence".into());
    assert_eq!(key, identity.cache_key().unwrap());
    identity.models[0].integrity = "present_unverified;stat:changed".into();
    assert_ne!(key, identity.cache_key().unwrap());
}

#[test]
fn varying_regions_across_trials_cannot_be_adopted_from_the_last_trial_only() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let options = SpeechCalibrationOptions {
        vad_threads: vec![1, 2],
        trials: 2,
        ..SpeechCalibrationOptions::default()
    };
    let calls = std::cell::Cell::new(0);
    let report = run_fake_speech(
        &catalog,
        &options,
        |threads| {
            let count = calls.get();
            calls.set(count + 1);
            Ok(vec![region(
                100,
                if threads == 2 && count == 2 { 450 } else { 400 },
            )])
        },
        |threads| {
            if threads == 1 {
                Duration::from_millis(10)
            } else {
                Duration::ZERO
            }
        },
    );
    assert_eq!(report.selected_settings.unwrap().vad_threads, 1);
    let rejected = report
        .candidates
        .iter()
        .find(|candidate| candidate.settings.vad_threads == 2)
        .unwrap();
    assert!(!rejected.succeeded);
    assert!(
        rejected
            .failures
            .iter()
            .any(|failure| failure.contains("between trials"))
    );
}

#[test]
fn speech_region_comparison_requires_exact_equal_intervals() {
    let baseline = vec![region(100, 500), region(700, 900)];
    let same = baseline.clone();
    assert!(compare_speech_regions(&baseline, &same, 1000).equivalent);

    let shifted = vec![region(100, 501), region(700, 900)];
    let comparison = compare_speech_regions(&baseline, &shifted, 1000);
    assert!(!comparison.equivalent);
    assert_eq!(comparison.mismatch_count, 1);
}

#[test]
fn speech_region_comparison_rejects_invalid_or_overlapping_regions() {
    let baseline = vec![region(100, 500)];
    for candidate in [
        vec![region(500, 400)],
        vec![region(100, 1001)],
        vec![region(100, 500), region(450, 600)],
    ] {
        let comparison = compare_speech_regions(&baseline, &candidate, 1000);
        assert!(!comparison.equivalent);
        assert!(!comparison.candidate_valid);
    }
}

#[test]
fn region_digest_is_stable() {
    let regions = vec![region(100, 500), region(700, 900)];
    assert_eq!(region_digest(&regions), region_digest(&regions));
    assert_ne!(region_digest(&regions), region_digest(&[region(100, 501)]));
}

#[test]
fn explicit_overrides_validate_ranges_and_are_not_calibrated_eta() {
    let mut overrides = ResourceOverrides {
        vad_threads: Some(0),
        ..ResourceOverrides::default()
    };
    assert!(
        override_selection(
            SPEECH_REPRESENTATION_KIND,
            STT_RECIPE_KEY,
            &overrides,
            String::new()
        )
        .is_err()
    );
    overrides.vad_threads = Some(3);
    let selection = override_selection(
        SPEECH_REPRESENTATION_KIND,
        STT_RECIPE_KEY,
        &overrides,
        String::new(),
    )
    .unwrap();
    assert_eq!(selection.source, "explicit_override");
    assert_eq!(selection.vad_threads, 3);
    assert!(selection.eta.estimated_remaining_seconds.is_none());

    let unsupported = ResourceOverrides {
        encoder: Some("vaapi".into()),
        ..ResourceOverrides::default()
    };
    assert!(
        override_selection(
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            &unsupported,
            String::new(),
        )
        .is_err()
    );
    let parallel_speech = ResourceOverrides {
        worker_count: Some(2),
        ..ResourceOverrides::default()
    };
    assert!(
        override_selection(
            SPEECH_REPRESENTATION_KIND,
            STT_RECIPE_KEY,
            &parallel_speech,
            String::new(),
        )
        .is_err()
    );
}

#[test]
fn unbiased_resource_selection_never_runs_a_hardware_probe() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let before = crate::proxy_encoding::encoder_plan_resolution_count();
    let selection = catalog
        .resolve_resource_selection(
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            Some(1024),
            None,
            &ResourceOverrides::default(),
        )
        .unwrap();
    let after = crate::proxy_encoding::encoder_plan_resolution_count();
    assert_eq!(selection.source, "default_uncalibrated");
    assert_eq!(selection.encoder.as_deref(), Some("software"));
    assert_eq!(
        before, after,
        "ordinary selection/readiness must not initialize or benchmark an encoder"
    );
}

#[test]
fn resource_selection_entrypoint_rejects_overrides_it_cannot_apply() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    assert!(
        catalog
            .resolve_resource_selection(
                SPEECH_REPRESENTATION_KIND,
                STT_RECIPE_KEY,
                None,
                None,
                &ResourceOverrides {
                    worker_count: Some(2),
                    ..ResourceOverrides::default()
                },
            )
            .is_err()
    );
    assert!(
        catalog
            .resolve_resource_selection(
                PROXY_REPRESENTATION_KIND,
                PROXY_SOFTWARE_RECIPE_KEY,
                None,
                None,
                &ResourceOverrides {
                    encoder: Some("videotoolbox".into()),
                    ..ResourceOverrides::default()
                },
            )
            .is_err()
    );
    let forced_software = catalog
        .resolve_resource_selection(
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            None,
            None,
            &ResourceOverrides {
                encoder: Some("software".into()),
                ..ResourceOverrides::default()
            },
        )
        .unwrap();
    assert_eq!(forced_software.source, "explicit_override");
    assert_eq!(forced_software.encoder.as_deref(), Some("software"));
}

#[test]
fn one_sample_timing_reports_a_wide_range_not_false_precision() {
    let timing = CalibrationTiming::from_samples(vec![100], Some(100)).unwrap();
    let uncertainty = uncertainty_for(&timing, 1);
    let eta = eta_from_timing(Some(&timing), Some(300), &uncertainty, "test");
    assert_eq!(eta.estimated_remaining_seconds, Some(0.3));
    let range = eta.range_seconds.unwrap();
    assert!(range[0] < 0.3 && range[1] > 0.3);
    assert_eq!(uncertainty.confidence, "low_sample_count");
}

fn fake_proxy_report(
    catalog: &Catalog,
    identity: CalibrationIdentity,
) -> ResourceCalibrationReport {
    let cache_key = identity.cache_key().unwrap();
    catalog
        .begin_resource_calibration(&identity, &cache_key)
        .unwrap();
    let settings = identity.candidates[0].clone();
    let timing = CalibrationTiming::from_samples(vec![100], Some(100)).unwrap();
    let uncertainty = uncertainty_for(&timing, 1);
    let mut report = ResourceCalibrationReport {
        schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
        cache_key,
        state: CalibrationState::Complete,
        identity,
        started_at: timestamp(),
        completed_at: Some(timestamp()),
        candidates: vec![CalibrationCandidateOutcome {
            settings: settings.clone(),
            succeeded: true,
            timing: Some(timing),
            failures: vec![],
            output_identity: Some("synthetic-output".into()),
            quality: None,
            render_contract_equivalent: Some(true),
        }],
        selected_settings: Some(settings),
        selection_reason: "synthetic complete calibration".into(),
        quality: None,
        uncertainty,
        error: None,
    };
    report.identity.candidates = report
        .candidates
        .iter()
        .map(|candidate| candidate.settings.clone())
        .collect();
    catalog.persist_resource_calibration(&mut report).unwrap();
    report
}

fn fake_proxy_identity() -> CalibrationIdentity {
    let candidate = CalibrationCandidateSettings {
        label: "policy=software;encoder=software;workers=1".into(),
        encoder: Some("software".into()),
        encoder_policy: Some("software".into()),
        worker_count: 1,
        vad_threads: 0,
        decode_threads: 0,
    };
    CalibrationIdentity {
        schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
        recipe_key: PROXY_SOFTWARE_RECIPE_KEY.into(),
        recipe_version: RESOURCE_CALIBRATION_RECIPE_VERSION.into(),
        representation_kind: PROXY_REPRESENTATION_KIND.into(),
        host: host_identity(),
        tools: tool_identities(),
        devices: hardware_device_identities(),
        models: vec![],
        media: CalibrationMediaIdentity {
            kind: "generated".into(),
            sample_identity: "generated:test".into(),
            duration_ms: 1_000,
            width: 320,
            height: 180,
            has_audio: true,
        },
        conditions: CalibrationConditions {
            trials: 1,
            local_heavy_slots: 1,
            generated_duration_seconds: Some(1),
            note: "test".into(),
        },
        candidates: vec![candidate.clone()],
        configuration_key: calibration_configuration_key(
            "generated",
            &[candidate],
            Some(ProxyEncoderPolicy::Software),
        )
        .unwrap(),
    }
}

#[test]
fn a_generated_contract_calibration_cannot_predict_unrelated_footage() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    fake_proxy_report(&catalog, fake_proxy_identity());
    let selection = catalog
        .resolve_resource_selection(
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            Some(1_000_000),
            None,
            &ResourceOverrides::default(),
        )
        .unwrap();
    assert_eq!(selection.source, "calibration");
    assert_eq!(selection.sample_compatibility, "generated_contract_only");
    assert!(selection.timing.is_none());
    assert!(selection.eta.estimated_remaining_seconds.is_none());
    assert!(
        catalog
            .calibration_rate_history(
                PROXY_REPRESENTATION_KIND,
                PROXY_SOFTWARE_RECIPE_KEY,
                Some("software"),
                1,
                None
            )
            .unwrap()
            .samples
            == 0
    );
}

#[test]
fn complete_matching_calibration_is_reused_and_seeds_eta() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let report = fake_proxy_report(&catalog, fake_proxy_identity());
    let selection = catalog
        .resolve_resource_selection(
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            Some(300),
            Some(&report.identity.media.sample_identity),
            &ResourceOverrides::default(),
        )
        .unwrap();
    assert_eq!(selection.source, "calibration");
    assert_eq!(selection.state, "complete");
    assert_eq!(selection.worker_count, 1);
    assert!(selection.eta.estimated_remaining_seconds.is_some());
    let range = selection.eta.range_seconds.unwrap();
    assert!(range[0] < range[1]);
    assert_eq!(selection.sample_compatibility, "exact_generated");
    assert_eq!(
        selection.uncertainty.eta_relative_range,
        Some(0.25),
        "one observation of the exact fixture still retains timing uncertainty"
    );
    let history = catalog
        .calibration_rate_history(
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            Some("software"),
            1,
            Some(&report.identity.media.sample_identity),
        )
        .unwrap();
    assert_eq!(history.samples, 1);
}

#[test]
fn calibrated_eta_rejects_mismatched_encoder_or_workers_and_is_not_n_fold_inflated() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let mut identity = fake_proxy_identity();
    identity.candidates[0].worker_count = 2;
    identity.candidates[0].label = "policy=software;encoder=software;workers=2".into();
    identity.configuration_key = calibration_configuration_key(
        "generated",
        &identity.candidates,
        Some(ProxyEncoderPolicy::Software),
    )
    .unwrap();
    let cache_key = identity.cache_key().unwrap();
    catalog
        .begin_resource_calibration(&identity, &cache_key)
        .unwrap();
    let timing = CalibrationTiming::from_samples(vec![100], Some(200)).unwrap();
    let settings = identity.candidates[0].clone();
    let uncertainty = uncertainty_for(&timing, 1);
    let mut report = ResourceCalibrationReport {
        schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
        cache_key,
        state: CalibrationState::Complete,
        identity,
        started_at: timestamp(),
        completed_at: Some(timestamp()),
        candidates: vec![CalibrationCandidateOutcome {
            settings: settings.clone(),
            succeeded: true,
            timing: Some(timing),
            failures: vec![],
            output_identity: None,
            quality: None,
            render_contract_equivalent: Some(true),
        }],
        selected_settings: Some(settings),
        selection_reason: "synthetic".into(),
        quality: None,
        uncertainty,
        error: None,
    };
    catalog.persist_resource_calibration(&mut report).unwrap();

    let estimate = catalog
        .calibrated_batch_estimate(
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            &[100, 100],
            2,
            Some("software"),
            Some(&report.identity.media.sample_identity),
            &ResourceOverrides::default(),
        )
        .unwrap();
    let seconds = estimate.estimated_remaining_seconds.unwrap();
    assert!(
        (seconds - 0.1).abs() < 0.001,
        "aggregate 2000 units/s for two workers must seed 1000 units/worker/s, got {seconds}"
    );
    assert!(estimate.estimated_remaining_seconds_range.is_some());

    let mismatch_workers = catalog
        .calibrated_batch_estimate(
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            &[100, 100],
            1,
            Some("software"),
            Some(&report.identity.media.sample_identity),
            &ResourceOverrides::default(),
        )
        .unwrap();
    assert!(mismatch_workers.estimated_remaining_seconds.is_none());
    assert_eq!(mismatch_workers.confidence, "mismatched_configuration");

    let mismatch_encoder = catalog
        .calibrated_batch_estimate(
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            &[100, 100],
            2,
            Some("vaapi"),
            Some(&report.identity.media.sample_identity),
            &ResourceOverrides::default(),
        )
        .unwrap();
    assert!(mismatch_encoder.estimated_remaining_seconds.is_none());
}

#[test]
fn changed_host_identity_is_stale_and_never_supplies_an_eta() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let mut identity = fake_proxy_identity();
    identity.host.arch = "different-host".into();
    fake_proxy_report(&catalog, identity);
    let selection = catalog
        .resolve_resource_selection(
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            Some(300),
            None,
            &ResourceOverrides::default(),
        )
        .unwrap();
    assert_eq!(selection.source, "default_uncalibrated");
    assert_eq!(selection.state, "missing");
    assert!(selection.eta.estimated_remaining_seconds.is_none());
    let status = catalog
        .resource_calibration_status(
            Some(PROXY_SOFTWARE_RECIPE_KEY),
            Some(PROXY_REPRESENTATION_KIND),
        )
        .unwrap();
    assert!(!status.entries[0].host_tools_match_current);
    assert!(!status.entries[0].reusable);
}

#[test]
fn abandoned_calibration_recovery_does_not_block_catalog_readers() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite3");
    let catalog = Catalog::open(&path).unwrap();
    let mut report = fake_proxy_report(&catalog, fake_proxy_identity());
    report.state = CalibrationState::Running;
    report.completed_at = None;
    catalog.persist_resource_calibration(&mut report).unwrap();
    let writer = catalog.connection().unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let opened = Catalog::open(&path).expect("maintenance cannot force a write lock on readers");
    let status = opened.resource_calibration_status(None, None).unwrap();
    assert_eq!(status.entries[0].state, CalibrationState::Interrupted);
    assert!(!status.entries[0].reusable);
    writer.execute_batch("ROLLBACK").unwrap();
    opened.recover_interrupted_resource_calibrations().unwrap();
    let state: String = opened
        .connection()
        .unwrap()
        .query_row(
            "SELECT state FROM resource_calibrations WHERE cache_key=?1",
            [&report.cache_key],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "interrupted");
}

#[test]
fn interrupted_run_is_never_reused_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite3");
    let catalog = Catalog::open(&path).unwrap();
    let mut report = fake_proxy_report(&catalog, fake_proxy_identity());
    report.state = CalibrationState::Running;
    report.completed_at = None;
    catalog.persist_resource_calibration(&mut report).unwrap();
    drop(catalog);

    let reopened = Catalog::open(&path).unwrap();
    let status = reopened
        .resource_calibration_status(
            Some(PROXY_SOFTWARE_RECIPE_KEY),
            Some(PROXY_REPRESENTATION_KIND),
        )
        .unwrap();
    assert_eq!(status.entries[0].state, CalibrationState::Interrupted);
    assert!(!status.entries[0].reusable);
    let selection = reopened
        .resolve_resource_selection(
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            Some(300),
            None,
            &ResourceOverrides::default(),
        )
        .unwrap();
    assert_eq!(selection.source, "default_uncalibrated");
}
#[test]
fn history_keys_are_qualified_by_environment_and_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let one_worker = catalog
        .qualified_history_key(
            STT_TASK_KEY,
            SPEECH_REPRESENTATION_KIND,
            STT_RECIPE_KEY,
            None,
            1,
            1,
            DEFAULT_DECODE_THREADS,
        )
        .unwrap();
    let two_workers = catalog
        .qualified_history_key(
            STT_TASK_KEY,
            SPEECH_REPRESENTATION_KIND,
            STT_RECIPE_KEY,
            None,
            2,
            1,
            DEFAULT_DECODE_THREADS,
        )
        .unwrap();
    let two_vad_threads = catalog
        .qualified_history_key(
            STT_TASK_KEY,
            SPEECH_REPRESENTATION_KIND,
            STT_RECIPE_KEY,
            None,
            1,
            2,
            DEFAULT_DECODE_THREADS,
        )
        .unwrap();
    assert_ne!(one_worker, two_workers);
    assert_ne!(one_worker, two_vad_threads);
    assert!(one_worker.starts_with(STT_TASK_KEY));
    assert!(one_worker.contains(".h"));
    assert!(one_worker.contains(".c"));
}

#[test]
fn file_and_model_identity_changes_invalidate_same_path_content() {
    let directory = tempfile::tempdir().unwrap();
    let tool_path = directory.path().join("ffmpeg");
    fs::write(&tool_path, b"tool-v1").unwrap();
    let first_tool = file_identity(&tool_path);
    fs::write(&tool_path, b"tool-v2-longer").unwrap();
    let second_tool = file_identity(&tool_path);
    assert_ne!(first_tool, second_tool);

    let model_path = directory.path().join("model.bin");
    fs::write(&model_path, b"model-v1").unwrap();
    let first_digest = model_content_digest(&model_path);
    fs::write(&model_path, b"model-v2").unwrap();
    let second_digest = model_content_digest(&model_path);
    assert_ne!(first_digest.0, second_digest.0);
}

#[test]
fn changed_device_or_model_identity_is_not_reusable() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let mut device_identity = fake_proxy_identity();
    device_identity.devices[0].identity = "different-device".into();
    fake_proxy_report(&catalog, device_identity);
    let selection = catalog
        .resolve_resource_selection(
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            Some(300),
            None,
            &ResourceOverrides::default(),
        )
        .unwrap();
    assert_eq!(selection.source, "default_uncalibrated");

    let mut tool_identity = fake_proxy_identity();
    tool_identity.tools[0].version = "different-tool-build".into();
    fake_proxy_report(&catalog, tool_identity);
    let status = catalog
        .resource_calibration_status(
            Some(PROXY_SOFTWARE_RECIPE_KEY),
            Some(PROXY_REPRESENTATION_KIND),
        )
        .unwrap();
    assert!(status.entries.iter().all(|entry| !entry.reusable));
}

#[test]
fn opening_a_second_catalog_does_not_interrupt_a_live_run() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite3");
    let catalog = Catalog::open(&path).unwrap();
    let identity = fake_proxy_identity();
    let cache_key = identity.cache_key().unwrap();
    catalog
        .begin_resource_calibration(&identity, &cache_key)
        .unwrap();
    let second = Catalog::open(&path).unwrap();
    let status = second
        .resource_calibration_status(
            Some(PROXY_SOFTWARE_RECIPE_KEY),
            Some(PROXY_REPRESENTATION_KIND),
        )
        .unwrap();
    assert_eq!(status.entries[0].state, CalibrationState::Running);
    assert!(!status.entries[0].reusable);

    active_calibration_runs()
        .lock()
        .unwrap()
        .remove(&active_run_key(&path, &cache_key));
    let connection = catalog.connection().unwrap();
    connection
        .execute(
            "UPDATE resource_calibrations SET owner_pid=?1,owner_process_start='abandoned' WHERE cache_key=?2",
            params![i64::from(u32::MAX - 1), cache_key],
        )
        .unwrap();
    let reopened = Catalog::open(&path).unwrap();
    let status = reopened
        .resource_calibration_status(
            Some(PROXY_SOFTWARE_RECIPE_KEY),
            Some(PROXY_REPRESENTATION_KIND),
        )
        .unwrap();
    assert_eq!(status.entries[0].state, CalibrationState::Interrupted);
}

#[test]
fn persistence_failure_rolls_back_report_and_observations_atomically() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let identity = fake_proxy_identity();
    let cache_key = identity.cache_key().unwrap();
    catalog
        .begin_resource_calibration(&identity, &cache_key)
        .unwrap();
    let connection = catalog.connection().unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_calibration_observation BEFORE INSERT ON resource_calibration_observations BEGIN SELECT RAISE(ABORT, 'injected observation failure'); END;",
        )
        .unwrap();
    let settings = identity.candidates[0].clone();
    let timing = CalibrationTiming::from_samples(vec![100], Some(100)).unwrap();
    let mut report = ResourceCalibrationReport {
        schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
        cache_key: cache_key.clone(),
        state: CalibrationState::Complete,
        identity,
        started_at: timestamp(),
        completed_at: Some(timestamp()),
        candidates: vec![CalibrationCandidateOutcome {
            settings: settings.clone(),
            succeeded: true,
            timing: Some(timing),
            failures: vec![],
            output_identity: None,
            quality: None,
            render_contract_equivalent: Some(true),
        }],
        selected_settings: Some(settings),
        selection_reason: "synthetic".into(),
        quality: None,
        uncertainty: CalibrationUncertainty {
            confidence: "measured".into(),
            timing_relative_spread: Some(0.0),
            eta_relative_range: Some(0.25),
            note: "synthetic".into(),
        },
        error: None,
    };
    assert!(catalog.persist_resource_calibration(&mut report).is_err());
    let (state, report_json): (String, Option<String>) = connection
        .query_row(
            "SELECT state,report_json FROM resource_calibrations WHERE cache_key=?1",
            [&cache_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "running");
    assert!(report_json.is_none());
    let observations: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM resource_calibration_observations WHERE cache_key=?1",
            [&cache_key],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(observations, 0);
    let run_key = active_run_key(catalog.database_path(), &cache_key);
    active_calibration_runs().lock().unwrap().remove(&run_key);
}

struct FakeSpeechEngine {
    regions: Vec<SpeechRegion>,
    delay: Duration,
}
impl SttEngine for FakeSpeechEngine {
    fn detect_speech(&self, _pcm: &[f32]) -> Result<Vec<SpeechRegion>> {
        std::thread::sleep(self.delay);
        Ok(self.regions.clone())
    }
    fn decode_window(
        &self,
        _pcm: &[f32],
        _window: &crate::analysis::windows::PackedWindow,
    ) -> Result<Vec<crate::analysis::quality::DecodedSegment>> {
        Ok(Vec::new())
    }
}

fn speech_identity(options: &SpeechCalibrationOptions) -> CalibrationIdentity {
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
    CalibrationIdentity {
        schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
        recipe_key: STT_RECIPE_KEY.into(),
        recipe_version: RESOURCE_CALIBRATION_RECIPE_VERSION.into(),
        representation_kind: SPEECH_REPRESENTATION_KIND.into(),
        host: host_identity(),
        tools: tool_identities(),
        devices: hardware_device_identities(),
        models: vec![],
        media: CalibrationMediaIdentity {
            kind: "generated".into(),
            sample_identity: generated_sample_identity(1, "speech"),
            duration_ms: 1000,
            width: 0,
            height: 0,
            has_audio: true,
        },
        conditions: CalibrationConditions {
            trials: options.trials,
            local_heavy_slots: 1,
            generated_duration_seconds: Some(1),
            note: "test".into(),
        },
        candidates: candidates.clone(),
        configuration_key: calibration_configuration_key("generated", &candidates, None).unwrap(),
    }
}

fn run_fake_speech(
    catalog: &Catalog,
    options: &SpeechCalibrationOptions,
    regions_for: impl Fn(usize) -> Result<Vec<SpeechRegion>> + Copy,
    delay_for: impl Fn(usize) -> Duration + Copy,
) -> ResourceCalibrationReport {
    let identity = speech_identity(options);
    let cache_key = identity.cache_key().unwrap();
    catalog
        .begin_resource_calibration(&identity, &cache_key)
        .unwrap();
    catalog
        .run_speech_calibration_with_factory(
            options,
            &identity,
            &cache_key,
            &timestamp(),
            &vec![0.0; 16000],
            1000,
            |vad_threads, _decode_threads| {
                Ok(Box::new(FakeSpeechEngine {
                    regions: regions_for(vad_threads)?,
                    delay: delay_for(vad_threads),
                }) as Box<dyn SttEngine>)
            },
        )
        .unwrap()
}

#[test]
fn fake_speech_engine_selects_fastest_valid_equivalent_candidate() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let options = SpeechCalibrationOptions {
        vad_threads: vec![1, 2],
        trials: 1,
        ..SpeechCalibrationOptions::default()
    };
    let report = run_fake_speech(
        &catalog,
        &options,
        |_| Ok(vec![region(100, 400), region(600, 900)]),
        |vad| {
            if vad == 1 {
                Duration::from_millis(20)
            } else {
                Duration::from_millis(2)
            }
        },
    );
    assert_eq!(report.state, CalibrationState::Complete);
    assert_eq!(report.selected_settings.as_ref().unwrap().vad_threads, 2);
    assert_eq!(
        report.quality.unwrap().adopted_candidate.as_deref(),
        Some("vad=2;decode=4;workers=1")
    );
}

#[test]
fn fake_speech_engine_rejects_faster_but_different_regions() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let options = SpeechCalibrationOptions {
        vad_threads: vec![1, 2],
        trials: 1,
        ..SpeechCalibrationOptions::default()
    };
    let report = run_fake_speech(
        &catalog,
        &options,
        |vad| {
            if vad == 1 {
                Ok(vec![region(100, 400)])
            } else {
                Ok(vec![region(100, 401)])
            }
        },
        |vad| {
            if vad == 1 {
                Duration::from_millis(20)
            } else {
                Duration::from_millis(1)
            }
        },
    );
    assert_eq!(report.state, CalibrationState::Complete);
    assert_eq!(report.selected_settings.as_ref().unwrap().vad_threads, 1);
    assert!(report.quality.unwrap().adopted_candidate.is_none());
    let rejected = report
        .candidates
        .iter()
        .find(|candidate| candidate.settings.vad_threads == 2)
        .unwrap();
    assert!(!rejected.succeeded);
}

#[test]
fn fake_speech_engine_failures_produce_failed_report_without_settings() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let options = SpeechCalibrationOptions {
        vad_threads: vec![1, 2],
        trials: 1,
        ..SpeechCalibrationOptions::default()
    };
    let report = run_fake_speech(
        &catalog,
        &options,
        |_| Err(anyhow!("engine exploded")),
        |_| Duration::ZERO,
    );
    assert_eq!(report.state, CalibrationState::Failed);
    assert!(report.selected_settings.is_none());
    assert!(
        report
            .candidates
            .iter()
            .all(|candidate| !candidate.succeeded)
    );
}

#[test]
fn exact_complete_calibration_is_reused_without_rerunning_benchmark() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let options = ProxyCalibrationOptions {
        media: CalibrationMedia::Generated,
        policy: ProxyEncoderPolicy::Software,
        worker_counts: vec![1],
        trials: 1,
        generated_duration_seconds: 1,
        force: false,
    };
    let candidates = proxy_candidates(options.policy, &options.worker_counts);
    let identity = CalibrationIdentity {
        schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
        recipe_key: PROXY_SOFTWARE_RECIPE_KEY.into(),
        recipe_version: RESOURCE_CALIBRATION_RECIPE_VERSION.into(),
        representation_kind: PROXY_REPRESENTATION_KIND.into(),
        host: host_identity(),
        tools: tool_identities(),
        devices: hardware_device_identities(),
        models: vec![],
        media: CalibrationMediaIdentity {
            kind: "generated".into(),
            sample_identity: generated_sample_identity(1, "proxy"),
            duration_ms: 1000,
            width: 320,
            height: 180,
            has_audio: true,
        },
        conditions: CalibrationConditions {
            trials: 1,
            local_heavy_slots: 1,
            generated_duration_seconds: Some(1),
            note: "single shared local-heavy slot; concurrent worker candidates are measured inside that one slot".into(),
        },
        candidates,
        configuration_key: calibration_configuration_key(
            options.media.kind(),
            &proxy_candidates(options.policy, &options.worker_counts),
            Some(options.policy),
        )
        .unwrap(),
    };
    let mut cached = fake_proxy_report(&catalog, identity);
    cached.selection_reason = "returned from exact cache".into();
    let cache_key = cached.cache_key.clone();
    catalog.persist_resource_calibration(&mut cached).unwrap();
    let report = catalog.calibrate_proxy(&options).unwrap();
    assert_eq!(report.cache_key, cache_key);
    assert_eq!(report.selection_reason, "returned from exact cache");
}

fn synthetic_estimator_environment(tool_version: &str) -> MediaEstimatorEnvironment {
    MediaEstimatorEnvironment {
        schema_version: "media-estimator-environment.v1".into(),
        host: CalibrationHostIdentity {
            host_key: "host".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            logical_cpus: 4,
            application_build: "build-a".into(),
        },
        tools: vec![CalibrationToolIdentity {
            role: "media_transcode".into(),
            name: "ffmpeg".into(),
            version: tool_version.into(),
            executable_key: "exact-program-a".into(),
        }],
        devices: vec![CalibrationDeviceIdentity {
            kind: "cpu".into(),
            name: "synthetic".into(),
            identity: "cpu-a".into(),
        }],
    }
}

#[test]
fn media_estimator_keys_separate_semantic_identity_from_execution_and_environment() {
    let identity = MediaEstimatorIdentity::new(
        synthetic_estimator_environment("ffmpeg-a"),
        PROXY_REPRESENTATION_KIND,
        PROXY_SOFTWARE_RECIPE_KEY,
        "proxy.h264-720p.orientation-v3.linux-software-w1-v1",
        Some("software"),
        1,
        0,
        0,
    );
    let base = "proxy.h264-720p.orientation-v3.linux-software-w1-v1";
    let qualified = identity.qualified_history_key(base);
    assert!(qualified.starts_with(base));
    assert!(qualified.contains(".h"));
    assert!(qualified.contains(".c"));
    assert_eq!(
        identity.semantic_recipe_key, PROXY_SOFTWARE_RECIPE_KEY,
        "estimator qualification must not rewrite the semantic recipe"
    );
    assert!(identity.is_compatible_with(
        &synthetic_estimator_environment("ffmpeg-a"),
        PROXY_SOFTWARE_RECIPE_KEY
    ));

    let changed_tool = MediaEstimatorIdentity::new(
        synthetic_estimator_environment("ffmpeg-b"),
        PROXY_REPRESENTATION_KIND,
        PROXY_SOFTWARE_RECIPE_KEY,
        "proxy.h264-720p.orientation-v3.linux-software-w1-v1",
        Some("software"),
        1,
        0,
        0,
    );
    let changed_encoder = MediaEstimatorIdentity::new(
        synthetic_estimator_environment("ffmpeg-a"),
        PROXY_REPRESENTATION_KIND,
        PROXY_SOFTWARE_RECIPE_KEY,
        "proxy.h264-720p.orientation-v3.linux-vaapi-qp23-w1-v1",
        Some("vaapi"),
        1,
        0,
        0,
    );
    assert_ne!(qualified, changed_tool.qualified_history_key(base));
    assert_ne!(qualified, changed_encoder.qualified_history_key(base));
    assert!(!changed_tool.is_compatible_with(
        &synthetic_estimator_environment("ffmpeg-a"),
        PROXY_SOFTWARE_RECIPE_KEY
    ));
    assert!(!identity.is_compatible_with(
        &synthetic_estimator_environment("ffmpeg-a"),
        "thumbnail.q1.orientation-v2"
    ));
}

#[test]
fn configured_tool_identity_uses_the_exact_program_not_the_global_name() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("ffmpeg-a");
    let second = directory.path().join("ffmpeg-b");
    fs::write(&first, "#!/bin/sh\necho 'ffmpeg version synthetic-a'\n").unwrap();
    fs::write(&second, "#!/bin/sh\necho 'ffmpeg version synthetic-b'\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&first, &second] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let first_identity = calibration_tool_identity_for_path("media_transcode", "ffmpeg", &first);
    let second_identity = calibration_tool_identity_for_path("media_transcode", "ffmpeg", &second);
    assert!(first_identity.version.contains("synthetic-a"));
    assert!(second_identity.version.contains("synthetic-b"));
    assert_ne!(
        first_identity.executable_key,
        second_identity.executable_key
    );
}

#[test]
fn representative_media_is_rejected_before_hashing_when_over_duration_bound() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("too-long.mp4");
    fs::write(&path, b"not really a video").unwrap();
    // The byte bound is independent of media parsing and is checked first.
    let oversized = directory.path().join("oversized.mp4");
    let file = fs::File::create(&oversized).unwrap();
    file.set_len(MAX_REPRESENTATIVE_MEDIA_BYTES + 1).unwrap();
    let error = validate_representative_media_metadata(&oversized).unwrap_err();
    assert!(error.to_string().contains("bounded"));
    assert!(validate_representative_duration(MAX_REPRESENTATIVE_MEDIA_DURATION_MS + 1).is_err());
    assert!(validate_representative_duration(MAX_REPRESENTATIVE_MEDIA_DURATION_MS).is_ok());
}
