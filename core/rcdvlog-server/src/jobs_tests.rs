use super::*;
use crate::{ConfigDiagnostics, ControlCatalog};
use std::sync::Mutex;
use tempfile::TempDir;
struct FakeClock(Mutex<DateTime<Utc>>);
impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}
impl FakeClock {
    fn advance(&self, s: i64) {
        *self.0.lock().unwrap() += ChronoDuration::seconds(s)
    }
}
fn engine() -> (TempDir, JobEngine, Arc<FakeClock>) {
    let dir = TempDir::new().unwrap();
    let cfg = ConfigDiagnostics {
        data_dir: dir.path().to_path_buf(),
        temp_dir: dir.path().join("tmp"),
        derived_dir: dir.path().join("derived"),
        backup_dir: dir.path().join("backup"),
        allowed_original_mounts: vec![],
        listen_addr: "127.0.0.1:1".parse().unwrap(),
        shutdown_timeout_seconds: 1,
        trusted_proxy_cidr_count: 1,
        lan_hostname: "rcdvlog.local".into(),
        session_max_age_seconds: 86400,
        bootstrap_code_ttl_seconds: 300,
        pairing_code_ttl_seconds: 600,
        backup_interval_seconds: 86400,
        backup_daily_retention: 14,
        backup_weekly_retention: 8,
        minimum_free_bytes: 64 * 1024 * 1024,
        ffmpeg_command: "ffmpeg".into(),
        ffprobe_command: "ffprobe".into(),
        exiftool_command: "exiftool".into(),
        proxy_encoder_policy: rcdvlog_core::ProxyEncoderPolicy::Auto,
    };
    std::fs::create_dir_all(cfg.temp_dir.clone()).unwrap();
    let c = ControlCatalog::open_or_create(dir.path(), &cfg).unwrap();
    let clock = Arc::new(FakeClock(Mutex::new(Utc::now())));
    (
        dir,
        JobEngine::with_clock(
            c.database_path().to_path_buf(),
            c.node_id().clone(),
            clock.clone(),
        ),
        clock,
    )
}
fn job(n: u32, node: &NodeId) -> JobDefinition {
    JobDefinition {
        job_id: JobId::parse(format!("job_{n}")).unwrap(),
        job_type: "hash.v1".into(),
        payload_version: 1,
        payload: json!({"storageObjectId":"object"}),
        immutable_identity: json!({"input":"digest"}),
        requirements: JobRequirements {
            schema_version: 1,
            requires_original: true,
            requires_derived_write: false,
            capabilities: vec![],
        },
        target_node_id: node.clone(),
        project_id: None,
        media_object_id: None,
        storage_root_id: None,
        representation_id: None,
        recipe_key: "hash.v1".into(),
        job_class: JobClass::FullHash,
        idempotency_key: format!("key_{n}"),
        priority: 0,
        total_work_units: 100,
        total_items: 1,
        retry_policy: RetryPolicy {
            max_attempts: 2,
            initial_backoff_seconds: 2,
            max_backoff_seconds: 4,
        },
        dependencies: vec![],
    }
}
#[test]
fn callbacks_are_atomic_durable_idempotent_and_catch_up_after_completion() {
    let (_dir, engine, _clock) = engine();
    let definition = job(801, engine.node_id());
    engine.enqueue(definition.clone()).unwrap();
    let callbacks = engine.callbacks();
    let subscription = callbacks.subscribe(&definition.job_id, "editor").unwrap();
    assert_eq!(
        callbacks
            .subscribe(&definition.job_id, "editor")
            .unwrap()
            .subscription_id,
        subscription.subscription_id
    );
    assert!(callbacks.claim("editor", 30).unwrap().is_none());
    let claimed = engine
        .claim_next("worker", Duration::from_secs(60))
        .unwrap()
        .unwrap();
    let result = JobResultCommit {
        output_identity: Some("artifact".into()),
        details: json!({"artifact":"result.v1"}),
    };
    assert!(
        engine
            .complete(&claimed, result.clone(), |_| Err(
                JobEngineError::InvalidInput("rollback".into())
            ))
            .is_err()
    );
    assert!(callbacks.claim("editor", 30).unwrap().is_none());
    engine.complete(&claimed, result, |_| Ok(())).unwrap();
    // Reopening the service uses the same persisted outbox.
    let callbacks = engine.callbacks();
    let delivery = callbacks.claim("editor", 30).unwrap().unwrap();
    assert_eq!(delivery.delivery.event.state, JobState::Succeeded);
    assert_eq!(
        delivery.delivery.event.details,
        json!({"artifact":"result.v1"})
    );
    assert!(callbacks.claim("editor", 30).unwrap().is_none());
    callbacks
        .settle(&delivery.delivery.delivery_id, &delivery.lease_token, true)
        .unwrap();
    callbacks
        .settle(&delivery.delivery.delivery_id, &delivery.lease_token, true)
        .unwrap();
    callbacks
        .subscribe(&definition.job_id, "late-editor")
        .unwrap();
    let late = callbacks.claim("late-editor", 30).unwrap().unwrap();
    assert_eq!(
        late.delivery.event.event_sequence,
        delivery.delivery.event.event_sequence
    );
    assert!(
        !serde_json::to_string(&callbacks.list("editor", 1, None).unwrap())
            .unwrap()
            .contains(&delivery.lease_token)
    );
    assert!(callbacks.claim("editor", 30).unwrap().is_none());
}

#[test]
fn callbacks_recover_expired_leases_and_fence_stale_acknowledgements() {
    let (_dir, engine, clock) = engine();
    let definition = job(802, engine.node_id());
    engine.enqueue(definition.clone()).unwrap();
    engine
        .callbacks()
        .subscribe(&definition.job_id, "consumer")
        .unwrap();
    engine.request_cancel(&definition.job_id, None).unwrap();
    let callbacks = engine.callbacks();
    let first = callbacks.claim("consumer", 5).unwrap().unwrap();
    assert_eq!(first.delivery.event.state, JobState::Canceled);
    clock.advance(6);
    let second = callbacks.claim("consumer", 5).unwrap().unwrap();
    assert_eq!(first.delivery.delivery_id, second.delivery.delivery_id);
    assert_eq!(second.delivery.attempt_count, 2);
    assert!(matches!(
        callbacks.settle(&first.delivery.delivery_id, &first.lease_token, true),
        Err(JobEngineError::StaleLease)
    ));
    callbacks
        .settle(&second.delivery.delivery_id, &second.lease_token, false)
        .unwrap();
    let third = callbacks.claim("consumer", 5).unwrap().unwrap();
    assert_eq!(third.delivery.attempt_count, 3);
    callbacks
        .settle(&third.delivery.delivery_id, &third.lease_token, true)
        .unwrap();
    assert!(callbacks.claim("consumer", 5).unwrap().is_none());
}

#[test]
fn callbacks_ignore_retry_wait_and_preserve_terminal_failure_events() {
    let (_dir, engine, clock) = engine();
    let definition = job(803, engine.node_id());
    engine.enqueue(definition.clone()).unwrap();
    engine
        .callbacks()
        .subscribe(&definition.job_id, "editor")
        .unwrap();
    let first = engine
        .claim_next("worker", Duration::from_secs(60))
        .unwrap()
        .unwrap();
    engine
        .fail(
            &first,
            FailureKind::Retryable,
            "retryable_job_failure",
            json!({}),
        )
        .unwrap();
    assert!(engine.callbacks().claim("editor", 30).unwrap().is_none());
    clock.advance(5);
    let second = engine
        .claim_next("worker", Duration::from_secs(60))
        .unwrap()
        .unwrap();
    engine
        .fail(
            &second,
            FailureKind::Retryable,
            "retryable_job_failure",
            json!({"reason":"exhausted"}),
        )
        .unwrap();
    let delivery = engine.callbacks().claim("editor", 30).unwrap().unwrap();
    assert_eq!(delivery.delivery.event.state, JobState::Failed);
    assert_eq!(delivery.delivery.event.attempt_number, Some(2));
    assert_eq!(
        delivery.delivery.event.details,
        json!({"reason":"exhausted"})
    );
}

#[test]
fn callbacks_bound_pages_validate_targets_and_upgrade_existing_terminal_jobs() {
    let (_dir, engine, _clock) = engine();
    for n in 804..807 {
        let definition = job(n, engine.node_id());
        engine.enqueue(definition.clone()).unwrap();
        engine.request_cancel(&definition.job_id, None).unwrap();
        engine
            .callbacks()
            .subscribe(&definition.job_id, "editor")
            .unwrap();
    }
    let callbacks = engine.callbacks();
    let first = callbacks.list("editor", 2, None).unwrap();
    let second = callbacks.list("editor", 2, first.next_before).unwrap();
    assert_eq!(first.items.len(), 2);
    assert_eq!(second.items.len(), 1);
    assert!(second.next_before.is_none());
    assert!(callbacks.list("editor", 0, None).is_err());
    assert!(callbacks.list("editor", 1, Some(u64::MAX)).is_err());
    assert!(callbacks.claim("https://untrusted", 5).is_err());
    assert!(callbacks.claim("editor", 0).is_err());
    assert!(
        callbacks
            .subscribe(&JobId::parse("job_absent").unwrap(), "editor")
            .is_err()
    );
    // A different late consumer receives the original canonical event.
    let id = JobId::parse("job_804").unwrap();
    callbacks.subscribe(&id, "upgraded-consumer").unwrap();
    assert_eq!(
        callbacks
            .claim("upgraded-consumer", 5)
            .unwrap()
            .unwrap()
            .delivery
            .event
            .job_id,
        id
    );
}

#[test]
fn callbacks_capture_dependency_cancellation_and_claim_concurrently_once() {
    let (_dir, engine, _clock) = engine();
    let parent = job(808, engine.node_id());
    let mut child = job(809, engine.node_id());
    child.dependencies.push(parent.job_id.clone());
    engine.enqueue(parent.clone()).unwrap();
    engine.enqueue(child.clone()).unwrap();
    engine
        .callbacks()
        .subscribe(&child.job_id, "dependency-editor")
        .unwrap();
    engine.request_cancel(&parent.job_id, None).unwrap();
    assert!(
        engine
            .claim_next("worker", Duration::from_secs(60))
            .unwrap()
            .is_none()
    );
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let workers = (0..2)
        .map(|_| {
            let callbacks = engine.callbacks();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                callbacks.claim("dependency-editor", 30).unwrap()
            })
        })
        .collect::<Vec<_>>();
    let claims = workers
        .into_iter()
        .filter_map(|w| w.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].delivery.event.state, JobState::Canceled);
    assert_eq!(claims[0].delivery.event.event_type, "dependency_terminal");
}

fn estimate_input(worker_count: usize, remaining_task_work_units: &[u64]) -> ProgressEstimateInput {
    ProgressEstimateInput {
        worker_count,
        remaining_task_work_units: remaining_task_work_units.to_vec(),
    }
}
#[test]
fn transitions_idempotency_cancellation_and_retry_are_durable() {
    let (_d, e, clock) = engine();
    let j = job(1, e.node_id());
    e.enqueue(j.clone()).unwrap();
    assert!(e.enqueue(j.clone()).is_ok());
    let mut bad = j.clone();
    bad.immutable_identity = json!({"input":"other"});
    assert!(matches!(
        e.enqueue(bad),
        Err(JobEngineError::IdempotencyConflict)
    ));
    let claimed = e
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let running = e.get_job(&claimed.job.job_id).unwrap();
    assert!(running.lease_active);
    assert_eq!(
        running.lease_expires_at.as_deref(),
        Some(claimed.lease_expires_at.as_str())
    );
    let serialized = serde_json::to_value(&running).unwrap();
    assert!(serialized.get("leaseOwner").is_none());
    assert!(serialized.get("leaseToken").is_none());
    e.report_progress(&claimed, 50, estimate_input(1, &[50]))
        .unwrap();
    assert_eq!(
        e.fail(
            &claimed,
            FailureKind::Retryable,
            "media_offline",
            json!({"safe":"detail"})
        )
        .unwrap(),
        JobState::RetryWait
    );
    let retrying = e.get_job(&claimed.job.job_id).unwrap();
    assert!(!retrying.lease_active);
    assert!(retrying.lease_expires_at.is_none());
    clock.advance(2);
    let again = e
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    e.request_cancel(&again.job.job_id, Some("stop")).unwrap();
    assert!(e.cancellation_requested(&again).unwrap());
    assert!(matches!(
        e.complete(
            &again,
            JobResultCommit {
                output_identity: None,
                details: json!({}),
            },
            |_| Ok(())
        ),
        Err(JobEngineError::Canceled)
    ));
    assert!(
        e.replay_events(0, 100)
            .unwrap()
            .events
            .iter()
            .any(|v| v.event_type == "canceled")
    );
}

#[test]
fn snapshots_expose_durable_lifecycle_state_and_reusable_outputs() {
    let (_dir, engine, _clock) = engine();

    let queued = job(2, engine.node_id());
    engine.enqueue(queued.clone()).unwrap();
    let queued_snapshot = engine.get_job(&queued.job_id).unwrap();
    assert_eq!(queued_snapshot.state, JobState::Queued);
    assert_eq!(queued_snapshot.attempt_count, 0);
    assert_eq!(queued_snapshot.completed_work_units, 0);
    assert!(queued_snapshot.error.is_none());
    assert!(queued_snapshot.output_identity.is_none());
    assert!(queued_snapshot.started_at.is_none());
    assert!(queued_snapshot.completed_at.is_none());

    let running = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let running_snapshot = engine.get_job(&queued.job_id).unwrap();
    assert_eq!(running_snapshot.state, JobState::Running);
    assert_eq!(running_snapshot.attempt_count, 1);
    assert!(running_snapshot.started_at.is_some());
    assert!(running_snapshot.completed_at.is_none());
    engine
        .fail(
            &running,
            FailureKind::Retryable,
            "retryable",
            json!({"reason":"offline"}),
        )
        .unwrap();
    let retry_snapshot = engine.get_job(&queued.job_id).unwrap();
    assert_eq!(retry_snapshot.state, JobState::RetryWait);
    assert_eq!(
        retry_snapshot
            .error
            .as_ref()
            .map(|error| error.code.as_str()),
        Some("retryable")
    );
    assert_eq!(
        retry_snapshot
            .error
            .as_ref()
            .map(|error| error.details.clone()),
        Some(json!({"reason":"offline"}))
    );

    let failed = job(3, engine.node_id());
    engine.enqueue(failed.clone()).unwrap();
    let failed_claim = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    engine
        .fail(
            &failed_claim,
            FailureKind::Terminal,
            "terminal",
            json!({"reason":"bad_input"}),
        )
        .unwrap();
    let failed_snapshot = engine.get_job(&failed.job_id).unwrap();
    assert_eq!(failed_snapshot.state, JobState::Failed);
    assert!(failed_snapshot.completed_at.is_some());
    assert_eq!(
        failed_snapshot
            .error
            .as_ref()
            .map(|error| error.code.as_str()),
        Some("terminal")
    );

    let canceled = job(4, engine.node_id());
    engine.enqueue(canceled.clone()).unwrap();
    engine
        .request_cancel(&canceled.job_id, Some("stop"))
        .unwrap();
    let canceled_snapshot = engine.get_job(&canceled.job_id).unwrap();
    assert_eq!(canceled_snapshot.state, JobState::Canceled);
    assert!(canceled_snapshot.cancel_requested_at.is_some());
    assert!(canceled_snapshot.completed_at.is_some());

    let succeeded = job(5, engine.node_id());
    engine.enqueue(succeeded.clone()).unwrap();
    let succeeded_claim = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    engine
        .complete(
            &succeeded_claim,
            JobResultCommit {
                output_identity: Some("immutable-output-v1".into()),
                details: json!({"registered":true}),
            },
            |_| Ok(()),
        )
        .unwrap();
    let succeeded_snapshot = engine.get_job(&succeeded.job_id).unwrap();
    assert_eq!(succeeded_snapshot.state, JobState::Succeeded);
    assert_eq!(
        succeeded_snapshot.output_identity.as_deref(),
        Some("immutable-output-v1")
    );
    assert_eq!(succeeded_snapshot.completed_work_units, 100);
    assert_eq!(succeeded_snapshot.completed_items, 1);

    let mut duplicate = succeeded.clone();
    duplicate.job_id = JobId::parse("job_5_duplicate").unwrap();
    let returned = engine.enqueue(duplicate).unwrap();
    assert_eq!(returned.job_id, succeeded.job_id);
    assert_eq!(
        engine
            .get_job(&returned.job_id)
            .unwrap()
            .output_identity
            .as_deref(),
        Some("immutable-output-v1")
    );
    assert!(matches!(
        engine.get_job(&JobId::parse("job_missing").unwrap()),
        Err(JobEngineError::NotFound(_))
    ));
}

#[test]
fn snapshots_are_read_only_and_available_during_an_uncommitted_writer() {
    let (_dir, engine, _clock) = engine();
    let definition = job(6, engine.node_id());
    engine.enqueue(definition.clone()).unwrap();
    let mut writer = open_writable_connection(&engine.database_path).unwrap();
    let transaction = writer.transaction().unwrap();
    transaction
        .execute(
            "UPDATE jobs SET priority=999 WHERE job_id=?1",
            [definition.job_id.as_str()],
        )
        .unwrap();

    let snapshot = engine.get_job(&definition.job_id).unwrap();
    assert_eq!(snapshot.state, JobState::Queued);
    assert_eq!(snapshot.definition.priority, 0);
    drop(transaction);
}
#[test]
fn claims_are_atomic_and_ten_thousand_queue_is_indexed() {
    let (_d, e, _) = engine();
    for n in 0..10_000 {
        e.enqueue(job(n, e.node_id())).unwrap();
    }
    let second = e.clone();
    let a = std::thread::spawn(move || second.claim_next("one", Duration::from_secs(10)).unwrap());
    let b = e.claim_next("two", Duration::from_secs(10)).unwrap();
    let a = a.join().unwrap();
    assert_ne!(
        a.as_ref().map(|v| v.job.job_id.as_str()),
        b.as_ref().map(|v| v.job.job_id.as_str())
    );
}

#[test]
fn expired_or_stale_attempt_cannot_mutate_progress_or_terminal_state() {
    let (_dir, engine, clock) = engine();
    engine.enqueue(job(77, engine.node_id())).unwrap();
    let claimed = engine
        .claim_next("worker", Duration::from_secs(1))
        .unwrap()
        .unwrap();
    clock.advance(2);
    assert!(matches!(
        engine.heartbeat(&claimed, Duration::from_secs(1)),
        Err(JobEngineError::StaleLease)
    ));
    assert!(matches!(
        engine.report_progress(&claimed, 1, estimate_input(1, &[99])),
        Err(JobEngineError::StaleLease)
    ));
    assert!(matches!(
        engine.fail(&claimed, FailureKind::Terminal, "failed", json!({})),
        Err(JobEngineError::StaleLease)
    ));
    assert!(matches!(
        engine.cancellation_requested(&claimed),
        Err(JobEngineError::NotFound(_)) | Err(JobEngineError::StaleLease)
    ));
}

#[test]
fn successful_and_failed_item_completions_preserve_rate_history_rules() {
    let (_dir, engine, _clock) = engine();
    let mut definition = job(78, engine.node_id());
    definition.total_items = 2;
    definition.total_work_units = 20;
    engine.enqueue(definition).unwrap();
    let claimed = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let first = engine
        .record_item_completion(
            &claimed,
            ItemCompletion {
                work_units: 10,
                succeeded: true,
                worker_elapsed_millis: 100,
            },
            estimate_input(1, &[10]),
        )
        .unwrap();
    assert_eq!((first.completed_items, first.failed_items), (1, 0));
    let second = engine
        .record_item_completion(
            &claimed,
            ItemCompletion {
                work_units: 10,
                succeeded: false,
                worker_elapsed_millis: 1,
            },
            estimate_input(1, &[]),
        )
        .unwrap();
    assert_eq!((second.completed_items, second.failed_items), (2, 1));
    assert_eq!(second.estimate.work_units_per_worker_second, Some(100.0));
}

#[test]
fn explicit_progress_weights_use_actual_lightweight_concurrency_and_durable_counters() {
    let (_dir, engine, _clock) = engine();
    let mut definition = job(79, engine.node_id());
    definition.job_class = JobClass::Lightweight;
    definition.total_items = 4;
    definition.total_work_units = 70;
    engine.enqueue(definition).unwrap();
    let claimed = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();

    let parallel = engine
        .record_item_completion(
            &claimed,
            ItemCompletion {
                work_units: 10,
                succeeded: true,
                worker_elapsed_millis: 100,
            },
            estimate_input(2, &[40, 10, 10]),
        )
        .unwrap();
    assert_eq!(
        (parallel.completed_items, parallel.completed_work_units),
        (1, 10)
    );
    assert_eq!(
        (
            parallel.estimate.completed_items,
            parallel.estimate.total_items,
            parallel.estimate.completed_work_units,
            parallel.estimate.total_work_units,
            parallel.estimate.worker_count,
        ),
        (1, 4, 10, 70, 2)
    );
    assert_eq!(parallel.estimate.estimated_remaining_seconds, Some(0.4));

    let serial = engine
        .report_progress(&claimed, 10, estimate_input(1, &[40, 10, 10]))
        .unwrap();
    assert_eq!(serial.estimate.worker_count, 1);
    assert_eq!(serial.estimate.estimated_remaining_seconds, Some(0.6));
}

#[test]
fn progress_estimate_input_must_match_the_durable_remaining_state() {
    let (_dir, engine, _clock) = engine();
    let mut definition = job(80, engine.node_id());
    definition.job_class = JobClass::Lightweight;
    definition.total_items = 2;
    definition.total_work_units = 20;
    engine.enqueue(definition).unwrap();
    let claimed = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();

    for input in [
        estimate_input(3, &[10, 10]),
        estimate_input(2, &[20]),
        estimate_input(2, &[10, 9]),
    ] {
        assert!(matches!(
            engine.report_progress(&claimed, 0, input),
            Err(JobEngineError::InvalidInput(_))
        ));
    }
    engine
        .record_item_completion(
            &claimed,
            ItemCompletion {
                work_units: 10,
                succeeded: false,
                worker_elapsed_millis: 1,
            },
            estimate_input(1, &[10]),
        )
        .unwrap();
    assert!(matches!(
        engine.report_progress(&claimed, 10, estimate_input(2, &[10])),
        Err(JobEngineError::InvalidInput(_))
    ));
}

fn estimator_environment(version: &str) -> MediaEstimatorEnvironment {
    use rcdvlog_core::{
        CalibrationDeviceIdentity, CalibrationHostIdentity, CalibrationToolIdentity,
    };
    MediaEstimatorEnvironment {
        schema_version: "media-estimator-environment.v1".into(),
        host: CalibrationHostIdentity {
            host_key: "host_a".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            logical_cpus: 4,
            application_build: "app_a".into(),
        },
        tools: vec![CalibrationToolIdentity {
            role: "media_transcode".into(),
            name: "ffmpeg".into(),
            version: version.into(),
            executable_key: "tool_a".into(),
        }],
        devices: vec![CalibrationDeviceIdentity {
            kind: "cpu".into(),
            name: "synthetic".into(),
            identity: "cpu_a".into(),
        }],
    }
}

fn media_identity(
    environment: MediaEstimatorEnvironment,
    encoder: &str,
    workers: usize,
) -> MediaEstimatorIdentity {
    let execution_key = format!("proxy.h264-720p.orientation-v3.linux-{encoder}-w{workers}-v1");
    MediaEstimatorIdentity::new(
        environment,
        rcdvlog_core::PROXY_REPRESENTATION_KIND,
        "proxy.h264-720p.orientation-v3",
        execution_key,
        Some(encoder),
        workers,
        0,
        0,
    )
}

fn media_job(n: u32, node: &NodeId) -> JobDefinition {
    let mut definition = job(n, node);
    definition.job_type = "representation.generate.v1".into();
    definition.representation_id =
        Some(RepresentationId::parse("representation_synthetic").unwrap());
    definition.recipe_key = "proxy.h264-720p.orientation-v3".into();
    definition.total_items = 2;
    definition.total_work_units = 20;
    definition
}

#[test]
fn unqualified_generic_proxy_history_never_seeds_a_current_media_eta() {
    let (_dir, engine, _clock) = engine();
    let definition = media_job(90, engine.node_id());
    engine.enqueue(definition.clone()).unwrap();
    let claimed = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let connection = open_writable_connection(&engine.database_path).unwrap();
    connection
        .execute(
            "INSERT INTO job_rate_history(node_id,recipe_key,work_units_per_worker_second,samples,updated_at) VALUES(?1,?2,100.0,8,?3)",
            params![
                engine.node_id().as_str(),
                "proxy.h264-720p.orientation-v3",
                "2026-01-01T00:00:00.000Z"
            ],
        )
        .unwrap();
    drop(connection);

    let progress = engine
        .report_progress(&claimed, 0, estimate_input(1, &[10, 10]))
        .unwrap();
    assert_eq!(progress.estimate.work_units_per_worker_second, None);
    assert_eq!(progress.estimate.estimated_remaining_seconds, None);
    assert!(
        progress.estimate.confidence.starts_with("uncertain_"),
        "{}",
        progress.estimate.confidence
    );

    let list = engine.list_jobs(10, None, None).unwrap();
    assert_eq!(list.items.len(), 1);
    assert_eq!(list.items[0].estimate.estimated_remaining_seconds, None);
    assert!(list.items[0].estimate.confidence.starts_with("uncertain_"));
}

#[test]
fn compatible_actual_encoder_history_reaches_progress_and_stays_qualified() {
    let (_dir, engine, _clock) = engine();
    let environment = estimator_environment("ffmpeg-a");
    let engine = engine.with_media_estimator_environment(environment.clone());
    let definition = media_job(91, engine.node_id());
    engine.enqueue(definition).unwrap();
    let claimed = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let identity = media_identity(environment.clone(), "software", 1);
    let completion = engine
        .record_item_completion_with_estimator_identity(
            &claimed,
            ItemCompletion {
                work_units: 10,
                succeeded: true,
                worker_elapsed_millis: 100,
            },
            estimate_input(1, &[10]),
            Some(&identity),
        )
        .unwrap();
    assert_eq!(completion.estimate.estimated_remaining_seconds, Some(0.1));
    assert!(completion.estimate.confidence.contains("observation"));

    let listed = engine.list_jobs(10, None, None).unwrap();
    assert_eq!(
        listed.items[0].estimate.estimated_remaining_seconds,
        Some(0.1),
        "the read-only job projection must use the same qualified history"
    );

    let progress = engine
        .report_progress(&claimed, 10, estimate_input(1, &[10]))
        .unwrap();
    assert_eq!(progress.estimate.estimated_remaining_seconds, Some(0.1));
    assert!(progress.estimate.confidence.contains("observation"));

    let connection = open_writable_connection(&engine.database_path).unwrap();
    let (rate_key, semantic_recipe, identity_json): (String, String, String) = connection
        .query_row(
            "SELECT recipe_key,semantic_recipe_key,estimator_identity_json FROM job_rate_history WHERE semantic_recipe_key=?1",
            ["proxy.h264-720p.orientation-v3"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert!(rate_key.starts_with("proxy.h264-720p.orientation-v3"));
    assert!(rate_key.contains(".h"));
    assert!(rate_key.contains(".c"));
    assert_eq!(semantic_recipe, "proxy.h264-720p.orientation-v3");
    let parsed: MediaEstimatorIdentity = serde_json::from_str(&identity_json).unwrap();
    assert_eq!(parsed, identity);
}

#[test]
fn incompatible_tool_or_attempt_context_makes_media_eta_uncertain() {
    let (_dir, engine, _clock) = engine();
    let environment = estimator_environment("ffmpeg-a");
    let engine = engine.with_media_estimator_environment(environment.clone());
    let definition = media_job(92, engine.node_id());
    engine.enqueue(definition).unwrap();
    let claimed = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let identity = media_identity(environment, "software", 1);
    engine
        .record_item_completion_with_estimator_identity(
            &claimed,
            ItemCompletion {
                work_units: 10,
                succeeded: true,
                worker_elapsed_millis: 100,
            },
            estimate_input(1, &[10]),
            Some(&identity),
        )
        .unwrap();

    let changed_tool_engine = engine
        .clone()
        .with_media_estimator_environment(estimator_environment("ffmpeg-b"));
    let progress = changed_tool_engine
        .report_progress(&claimed, 10, estimate_input(1, &[10]))
        .unwrap();
    assert_eq!(progress.estimate.estimated_remaining_seconds, None);
    assert!(progress.estimate.confidence.contains("stale"));

    let changed_encoder_engine = engine
        .clone()
        .with_media_estimator_environment(estimator_environment("ffmpeg-a"));
    let changed_encoder = media_identity(estimator_environment("ffmpeg-a"), "vaapi", 1);
    changed_encoder_engine
        .attach_attempt_estimator_identity(&claimed, &changed_encoder)
        .unwrap();
    let progress = changed_encoder_engine
        .report_progress(&claimed, 10, estimate_input(1, &[10]))
        .unwrap();
    assert_eq!(progress.estimate.estimated_remaining_seconds, None);
    assert!(progress.estimate.confidence.starts_with("uncertain_"));
}

#[test]
fn legacy_rate_key_api_cannot_seed_or_train_media_timing() {
    let (_dir, engine, _clock) = engine();
    let definition = media_job(99, engine.node_id());
    engine.enqueue(definition).unwrap();
    let claimed = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let progress = engine
        .record_item_completion_with_rate_key(
            &claimed,
            ItemCompletion {
                work_units: 10,
                succeeded: true,
                worker_elapsed_millis: 100,
            },
            estimate_input(1, &[10]),
            Some("legacy.proxy.rate"),
        )
        .unwrap();
    assert_eq!(progress.estimate.work_units_per_worker_second, None);
    assert_eq!(progress.estimate.estimated_remaining_seconds, None);
    assert!(progress.estimate.confidence.starts_with("uncertain_"));
    let connection = open_writable_connection(&engine.database_path).unwrap();
    let rates: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM job_rate_history WHERE recipe_key='legacy.proxy.rate'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rates, 0);
}

#[test]
fn software_fallback_context_does_not_reuse_hardware_throughput() {
    let (_dir, engine, _clock) = engine();
    let environment = estimator_environment("ffmpeg-a");
    let engine = engine.with_media_estimator_environment(environment.clone());

    let mut failed_hardware_job = media_job(97, engine.node_id());
    failed_hardware_job.total_items = 1;
    failed_hardware_job.representation_id =
        Some(RepresentationId::parse("representation_hardware").unwrap());
    engine.enqueue(failed_hardware_job).unwrap();
    let hardware_claim = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let hardware_identity = media_identity(environment.clone(), "vaapi", 1);
    engine
        .record_item_completion_with_estimator_identity(
            &hardware_claim,
            ItemCompletion {
                work_units: 20,
                succeeded: true,
                worker_elapsed_millis: 100,
            },
            estimate_input(1, &[]),
            Some(&hardware_identity),
        )
        .unwrap();
    engine
        .complete(
            &hardware_claim,
            JobResultCommit {
                output_identity: None,
                details: json!({}),
            },
            |_| Ok(()),
        )
        .unwrap();

    let mut fallback_job = media_job(98, engine.node_id());
    fallback_job.representation_id =
        Some(RepresentationId::parse("representation_software").unwrap());
    engine.enqueue(fallback_job).unwrap();
    let fallback_claim = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let software_identity = media_identity(environment, "software", 1);
    engine
        .attach_attempt_estimator_identity(&fallback_claim, &software_identity)
        .unwrap();
    let progress = engine
        .report_progress(&fallback_claim, 0, estimate_input(1, &[10, 10]))
        .unwrap();
    assert_eq!(progress.estimate.work_units_per_worker_second, None);
    assert_eq!(progress.estimate.estimated_remaining_seconds, None);
    assert!(progress.estimate.confidence.starts_with("uncertain_"));
}

#[test]
fn restarted_process_context_is_not_reused_even_with_a_live_lease() {
    let (_dir, engine, _clock) = engine();
    let environment = estimator_environment("ffmpeg-a");
    let engine = engine.with_media_estimator_environment(environment.clone());
    let definition = media_job(100, engine.node_id());
    engine.enqueue(definition).unwrap();
    let claimed = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let identity = media_identity(environment, "software", 1);
    engine
        .record_item_completion_with_estimator_identity(
            &claimed,
            ItemCompletion {
                work_units: 10,
                succeeded: true,
                worker_elapsed_millis: 100,
            },
            estimate_input(1, &[10]),
            Some(&identity),
        )
        .unwrap();
    let connection = open_writable_connection(&engine.database_path).unwrap();
    connection
        .execute(
            "UPDATE job_attempts SET estimator_process_key='previous-process' WHERE job_id=?1 AND attempt_number=?2",
            params![claimed.job.job_id.as_str(), claimed.attempt_number],
        )
        .unwrap();
    drop(connection);
    let progress = engine
        .report_progress(&claimed, 10, estimate_input(1, &[10]))
        .unwrap();
    assert_eq!(progress.estimate.estimated_remaining_seconds, None);
    assert!(progress.estimate.confidence.contains("missing"));
}

#[test]
fn malformed_attempt_context_fails_closed_to_uncertainty() {
    let (_dir, engine, _clock) = engine();
    let environment = estimator_environment("ffmpeg-a");
    let engine = engine.with_media_estimator_environment(environment.clone());
    let definition = media_job(96, engine.node_id());
    engine.enqueue(definition).unwrap();
    let claimed = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let connection = open_writable_connection(&engine.database_path).unwrap();
    connection
        .execute(
            "UPDATE job_attempts SET estimator_identity_json='{not-json' WHERE job_id=?1 AND attempt_number=?2",
            params![claimed.job.job_id.as_str(), claimed.attempt_number],
        )
        .unwrap();
    drop(connection);
    let progress = engine
        .report_progress(&claimed, 0, estimate_input(1, &[10, 10]))
        .unwrap();
    assert_eq!(progress.estimate.estimated_remaining_seconds, None);
    assert!(progress.estimate.confidence.starts_with("uncertain_"));
}

#[test]
fn failed_attempts_do_not_train_the_qualified_media_rate() {
    let (_dir, engine, _clock) = engine();
    let environment = estimator_environment("ffmpeg-a");
    let engine = engine.with_media_estimator_environment(environment.clone());
    let definition = media_job(93, engine.node_id());
    engine.enqueue(definition).unwrap();
    let claimed = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let identity = media_identity(environment, "software", 1);
    let progress = engine
        .record_item_completion_with_estimator_identity(
            &claimed,
            ItemCompletion {
                work_units: 10,
                succeeded: false,
                worker_elapsed_millis: 5_000,
            },
            estimate_input(1, &[10]),
            Some(&identity),
        )
        .unwrap();
    assert_eq!(progress.estimate.estimated_remaining_seconds, None);
    assert!(progress.estimate.confidence.starts_with("uncertain_"));
    let connection = open_writable_connection(&engine.database_path).unwrap();
    let rates: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM job_rate_history WHERE estimator_identity_json IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rates, 0);
}

#[test]
fn expired_attempt_context_is_not_reused_after_recovery() {
    let (_dir, engine, clock) = engine();
    let environment = estimator_environment("ffmpeg-a");
    let engine = engine.with_media_estimator_environment(environment.clone());
    let definition = media_job(94, engine.node_id());
    engine.enqueue(definition).unwrap();
    let claimed = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let identity = media_identity(environment, "software", 1);
    engine
        .record_item_completion_with_estimator_identity(
            &claimed,
            ItemCompletion {
                work_units: 10,
                succeeded: true,
                worker_elapsed_millis: 100,
            },
            estimate_input(1, &[10]),
            Some(&identity),
        )
        .unwrap();

    clock.advance(11);
    engine.recover_expired().unwrap();
    let retried = engine.get_job(&claimed.job.job_id).unwrap();
    assert_eq!(retried.state, JobState::RetryWait);
    clock.advance(3);
    let reclaim = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    assert_eq!(reclaim.attempt_number, 2);
    let progress = engine
        .report_progress(&reclaim, 10, estimate_input(1, &[10]))
        .unwrap();
    assert_eq!(progress.estimate.estimated_remaining_seconds, None);
    assert!(progress.estimate.confidence.contains("missing"));
}

#[test]
fn queued_and_interrupted_non_media_jobs_do_not_promise_an_eta() {
    let (_dir, engine, _clock) = engine();
    let definition = job(95, engine.node_id());
    engine.enqueue(definition.clone()).unwrap();
    let list = engine.list_jobs(10, None, None).unwrap();
    assert_eq!(list.items[0].estimate.estimated_remaining_seconds, None);
    assert!(list.items[0].estimate.confidence.contains("waiting"));

    let claimed = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    engine
        .record_item_completion(
            &claimed,
            ItemCompletion {
                work_units: 100,
                succeeded: true,
                worker_elapsed_millis: 1_000,
            },
            estimate_input(1, &[]),
        )
        .unwrap();
    let running = engine.list_jobs(10, None, None).unwrap();
    assert_eq!(
        running.items[0].estimate.work_units_per_worker_second,
        Some(100.0),
        "running non-media history remains a valid measured estimate"
    );
    engine
        .complete(
            &claimed,
            JobResultCommit {
                output_identity: None,
                details: json!({}),
            },
            |_| Ok(()),
        )
        .unwrap();
    let list = engine.list_jobs(10, None, None).unwrap();
    // Terminal state is authoritative and does not claim a future ETA.
    assert_eq!(list.items[0].estimate.estimated_remaining_seconds, None);
    assert!(list.items[0].estimate.confidence.contains("terminal"));
}

#[test]
fn job_totals_and_event_cursors_reject_sqlite_integer_overflow() {
    let (_dir, engine, _clock) = engine();
    let mut oversized_work = job(81, engine.node_id());
    oversized_work.total_work_units = i64::MAX as u64 + 1;
    assert!(matches!(
        engine.enqueue(oversized_work),
        Err(JobEngineError::InvalidInput(_))
    ));
    let mut oversized_items = job(82, engine.node_id());
    oversized_items.total_items = i64::MAX as u64 + 1;
    assert!(matches!(
        engine.enqueue(oversized_items),
        Err(JobEngineError::InvalidInput(_))
    ));
    assert!(matches!(
        engine.replay_events(i64::MAX as u64 + 1, 1),
        Err(JobEngineError::InvalidInput(_))
    ));
}

#[test]
fn immutable_identity_and_dependencies_round_trip_without_idempotency_corruption() {
    let (_dir, engine, _clock) = engine();
    let parent = job(91, engine.node_id());
    engine.enqueue(parent.clone()).unwrap();
    let mut child = job(92, engine.node_id());
    child.immutable_identity = json!({"source":"original","digest":"abc"});
    child.dependencies = vec![parent.job_id.clone()];
    engine.enqueue(child.clone()).unwrap();
    let mut connection = open_writable_connection(&engine.database_path).unwrap();
    let transaction = connection.transaction().unwrap();
    let loaded = load_definition(&transaction, child.job_id.as_str()).unwrap();
    assert_eq!(loaded.immutable_identity, child.immutable_identity);
    assert_eq!(loaded.dependencies, child.dependencies);
}

#[test]
fn dependencies_gate_success_and_terminal_parent_propagates_cancellation() {
    let (_dir, engine, _clock) = engine();
    let parent = job(93, engine.node_id());
    let mut child = job(94, engine.node_id());
    child.dependencies = vec![parent.job_id.clone()];
    engine.enqueue(parent.clone()).unwrap();
    engine.enqueue(child.clone()).unwrap();
    let parent_claim = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    assert_eq!(parent_claim.job.job_id, parent.job_id);
    engine
        .complete(
            &parent_claim,
            JobResultCommit {
                output_identity: None,
                details: json!({}),
            },
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(
        engine
            .claim_next("worker", Duration::from_secs(10))
            .unwrap()
            .unwrap()
            .job
            .job_id,
        child.job_id
    );

    let failed = job(95, engine.node_id());
    let mut blocked = job(96, engine.node_id());
    blocked.dependencies = vec![failed.job_id.clone()];
    engine.enqueue(failed.clone()).unwrap();
    engine.enqueue(blocked.clone()).unwrap();
    let claimed = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    engine
        .fail(
            &claimed,
            FailureKind::Terminal,
            "terminal",
            json!({"code":"terminal"}),
        )
        .unwrap();
    engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap();
    assert!(
        engine
            .replay_events(0, 100)
            .unwrap()
            .events
            .iter()
            .any(
                |event| event.job_id == blocked.job_id && event.event_type == "dependency_terminal"
            )
    );
}

#[test]
fn completion_callback_and_terminal_state_share_one_transaction() {
    let (_dir, engine, _clock) = engine();
    engine.enqueue(job(97, engine.node_id())).unwrap();
    let claimed = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let failed = engine.complete(&claimed, JobResultCommit { output_identity: None, details: json!({}) }, |transaction| {
        transaction.execute("INSERT INTO operational_metadata(key,value,updated_at) VALUES('job-test','value','now')", [])?;
        Err(JobEngineError::InvalidInput("callback failure".into()))
    });
    assert!(failed.is_err());
    let connection = open_writable_connection(&engine.database_path).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM operational_metadata WHERE key='job-test'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    engine.complete(&claimed, JobResultCommit { output_identity: None, details: json!({}) }, |transaction| {
        transaction.execute("INSERT INTO operational_metadata(key,value,updated_at) VALUES('job-test','value','now')", [])?;
        Ok(())
    }).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM operational_metadata WHERE key='job-test'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert!(
        engine
            .replay_events(0, 100)
            .unwrap()
            .events
            .iter()
            .any(|event| event.event_type == "succeeded")
    );
}

#[test]
fn retry_backoff_is_capped_and_terminal_failures_do_not_requeue() {
    assert_eq!(capped_backoff_seconds(2, 5, 1), 2);
    assert_eq!(capped_backoff_seconds(2, 5, 2), 4);
    assert_eq!(capped_backoff_seconds(2, 5, 3), 5);
    let (_dir, engine, clock) = engine();
    let mut definition = job(98, engine.node_id());
    definition.retry_policy.max_backoff_seconds = 2;
    definition.retry_policy.max_attempts = 1;
    engine.enqueue(definition).unwrap();
    let claimed = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    assert_eq!(
        engine
            .fail(&claimed, FailureKind::Retryable, "offline", json!({}))
            .unwrap(),
        JobState::Failed
    );
    clock.advance(999);
    assert!(
        engine
            .claim_next("worker", Duration::from_secs(10))
            .unwrap()
            .is_none()
    );
}

#[test]
fn replay_is_paginated_and_pre_cancel_never_invokes_completion_callback() {
    let (_dir, engine, _clock) = engine();
    engine.enqueue(job(99, engine.node_id())).unwrap();
    let claimed = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    engine
        .request_cancel(&claimed.job.job_id, Some("stop"))
        .unwrap();
    let called = std::sync::atomic::AtomicBool::new(false);
    assert!(matches!(
        engine.complete(
            &claimed,
            JobResultCommit {
                output_identity: None,
                details: json!({})
            },
            |_| {
                called.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
        ),
        Err(JobEngineError::Canceled)
    ));
    assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
    let first = engine.replay_events(0, 1).unwrap();
    assert_eq!(first.events.len(), 1);
    let second = engine
        .replay_events(first.next_after.unwrap(), 100)
        .unwrap();
    assert!(!second.events.is_empty());
}

#[test]
fn recovery_preserves_live_lease_and_terminalizes_expired_variants() {
    let (_dir, engine, clock) = engine();
    let mut live = job(110, engine.node_id());
    live.retry_policy.max_attempts = 2;
    let mut expired = job(111, engine.node_id());
    expired.retry_policy.max_attempts = 2;
    let mut maxed = job(112, engine.node_id());
    maxed.retry_policy.max_attempts = 1;
    let canceled = job(113, engine.node_id());
    for definition in [
        live.clone(),
        expired.clone(),
        maxed.clone(),
        canceled.clone(),
    ] {
        engine.enqueue(definition).unwrap();
    }
    let live_claim = engine
        .claim_next("worker", Duration::from_secs(100))
        .unwrap()
        .unwrap();
    let expired_claim = engine
        .claim_next("worker", Duration::from_secs(1))
        .unwrap()
        .unwrap();
    let max_claim = engine
        .claim_next("worker", Duration::from_secs(1))
        .unwrap()
        .unwrap();
    let cancel_claim = engine
        .claim_next("worker", Duration::from_secs(1))
        .unwrap()
        .unwrap();
    engine
        .request_cancel(&cancel_claim.job.job_id, Some("stop"))
        .unwrap();
    clock.advance(2);
    assert_eq!(engine.recover_expired().unwrap(), 3);
    assert!(matches!(
        engine.heartbeat(&live_claim, Duration::from_secs(100)),
        Ok(())
    ));
    for (id, expected_state) in [
        (&expired_claim.job.job_id, JobState::RetryWait),
        (&max_claim.job.job_id, JobState::Failed),
        (&cancel_claim.job.job_id, JobState::Canceled),
    ] {
        let snapshot = engine.get_job(id).unwrap();
        assert_eq!(snapshot.state, expected_state);
        assert_eq!(
            snapshot.error.as_ref().map(|error| error.code.as_str()),
            Some("lease_expired")
        );
        assert_eq!(snapshot.error.map(|error| error.details), Some(json!({})));
    }
    let retry = engine
        .claim_next("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    assert_eq!(retry.job.job_id, expired_claim.job.job_id);
    assert!(
        engine
            .replay_events(0, 100)
            .unwrap()
            .events
            .iter()
            .any(|event| event.job_id == max_claim.job.job_id && event.state == JobState::Failed)
    );
    assert!(
        engine
            .replay_events(0, 100)
            .unwrap()
            .events
            .iter()
            .any(|event| event.job_id == cancel_claim.job.job_id
                && event.state == JobState::Canceled)
    );
}

#[test]
fn class_claim_query_uses_class_queue_index() {
    let (_dir, engine, _clock) = engine();
    engine.enqueue(job(114, engine.node_id())).unwrap();
    let connection = open_writable_connection(&engine.database_path).unwrap();
    let mut statement = connection.prepare("EXPLAIN QUERY PLAN SELECT j.job_id FROM jobs j WHERE j.target_node_id=?1 AND j.job_class=?2 AND j.state='queued' AND j.available_at<=?3 AND j.cancel_requested_at IS NULL ORDER BY j.priority DESC,j.enqueue_sequence ASC LIMIT 1").unwrap();
    let plan = statement
        .query_map(
            params![
                engine.node_id().as_str(),
                JobClass::FullHash.as_str(),
                timestamp(Utc::now())
            ],
            |row| row.get::<_, String>(3),
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join(" ");
    assert!(plan.contains("jobs_claimable_by_node_class"), "{plan}");
}

#[test]
fn idempotency_rejects_every_execution_field_change() {
    let (_dir, engine, _clock) = engine();
    let mut base = job(120, engine.node_id());
    base.project_id = Some(ProjectId::parse("project_a").unwrap());
    base.media_object_id = Some(MediaObjectId::parse("media_a").unwrap());
    base.storage_root_id = Some(StorageRootId::parse("root_a").unwrap());
    base.representation_id = Some(RepresentationId::parse("representation_a").unwrap());
    base.requirements.capabilities = vec![ServerCapability::Ffmpeg];
    let duplicate = engine.enqueue(base.clone()).unwrap();
    assert_eq!(
        engine.enqueue(base.clone()).unwrap().job_id,
        duplicate.job_id
    );
    type Mutation = Box<dyn Fn(&mut JobDefinition)>;
    let variants: Vec<Mutation> = vec![
        Box::new(|v| v.payload = json!({"changed":true})),
        Box::new(|v| v.immutable_identity = json!({"changed":true})),
        Box::new(|v| v.requirements.capabilities = vec![ServerCapability::Ffprobe]),
        Box::new(|v| v.project_id = Some(ProjectId::parse("project_b").unwrap())),
        Box::new(|v| v.media_object_id = Some(MediaObjectId::parse("media_b").unwrap())),
        Box::new(|v| v.storage_root_id = Some(StorageRootId::parse("root_b").unwrap())),
        Box::new(|v| {
            v.representation_id = Some(RepresentationId::parse("representation_b").unwrap())
        }),
        Box::new(|v| v.recipe_key = "other.v1".into()),
        Box::new(|v| v.job_class = JobClass::Lightweight),
        Box::new(|v| v.priority = 1),
        Box::new(|v| v.total_work_units = 101),
        Box::new(|v| v.total_items = 2),
        Box::new(|v| v.retry_policy.max_attempts = 4),
        Box::new(|v| v.retry_policy.initial_backoff_seconds = 3),
        Box::new(|v| v.retry_policy.max_backoff_seconds = 5),
        Box::new(|v| v.dependencies = vec![JobId::parse("job_dependency").unwrap()]),
    ];
    for mutate in variants {
        let mut candidate = base.clone();
        mutate(&mut candidate);
        assert!(matches!(
            engine.enqueue(candidate),
            Err(JobEngineError::IdempotencyConflict)
        ));
    }
}

struct CountingHandler {
    running: std::sync::atomic::AtomicUsize,
    maximum: std::sync::atomic::AtomicUsize,
}
impl JobHandler for CountingHandler {
    fn run(
        &self,
        _job: ClaimedJob,
        _cancellation: CancellationToken,
    ) -> Result<JobResultCommit, JobHandlerFailure> {
        let current = self
            .running
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        self.maximum
            .fetch_max(current, std::sync::atomic::Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(80));
        self.running
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        Ok(JobResultCommit {
            output_identity: None,
            details: json!({}),
        })
    }
}

#[tokio::test]
async fn scheduler_caps_lightweight_workers_and_drains_shutdown() {
    struct GateHandler {
        started: std::sync::mpsc::Sender<JobId>,
        release: Arc<std::sync::atomic::AtomicBool>,
        running: std::sync::atomic::AtomicUsize,
        maximum: std::sync::atomic::AtomicUsize,
    }
    impl JobHandler for GateHandler {
        fn run(
            &self,
            job: ClaimedJob,
            _cancellation: CancellationToken,
        ) -> Result<JobResultCommit, JobHandlerFailure> {
            let current = self
                .running
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            self.maximum
                .fetch_max(current, std::sync::atomic::Ordering::SeqCst);
            self.started.send(job.job.job_id).unwrap();
            while !self.release.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            self.running
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            Ok(JobResultCommit {
                output_identity: None,
                details: json!({}),
            })
        }
    }
    let (_dir, engine, _clock) = engine();
    for id in 130..133 {
        let mut definition = job(id, engine.node_id());
        definition.job_class = JobClass::Lightweight;
        engine.enqueue(definition).unwrap();
    }
    let shutdown = CancellationToken::new();
    let scheduler = JobScheduler::new(
        engine.clone(),
        JobSchedulerConfig::default(),
        shutdown.clone(),
    )
    .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handler = Arc::new(GateHandler {
        started: sender,
        release: release.clone(),
        running: std::sync::atomic::AtomicUsize::new(0),
        maximum: std::sync::atomic::AtomicUsize::new(0),
    });
    let observed = handler.clone();
    let task = tokio::spawn(async move { scheduler.run(handler).await });

    let started = tokio::task::spawn_blocking(move || {
        let first = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        let second = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        vec![first, second]
    })
    .await
    .unwrap();
    assert_eq!(started.len(), 2);
    shutdown.cancel();
    release.store(true, std::sync::atomic::Ordering::SeqCst);
    task.await.unwrap().unwrap();

    assert_eq!(
        observed.maximum.load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    let connection = open_writable_connection(&engine.database_path).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE state='succeeded'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        2
    );
    assert_eq!(
        connection
            .query_row("SELECT state FROM jobs WHERE job_id='job_132'", [], |row| {
                row.get::<_, String>(0)
            },)
            .unwrap(),
        "queued"
    );
}

#[tokio::test]
async fn scheduler_caps_proxy_and_hash_at_one() {
    for class in [JobClass::Proxy, JobClass::FullHash] {
        let (_dir, engine, _clock) = engine();
        for id in 140..142 {
            let mut definition = job(id, engine.node_id());
            definition.job_class = class;
            engine.enqueue(definition).unwrap();
        }
        let shutdown = CancellationToken::new();
        let scheduler =
            JobScheduler::new(engine, JobSchedulerConfig::default(), shutdown.clone()).unwrap();
        let handler = Arc::new(CountingHandler {
            running: std::sync::atomic::AtomicUsize::new(0),
            maximum: std::sync::atomic::AtomicUsize::new(0),
        });
        let observed = handler.clone();
        let task = tokio::spawn(async move { scheduler.run(handler).await });
        tokio::time::sleep(Duration::from_millis(30)).await;
        shutdown.cancel();
        task.await.unwrap().unwrap();
        assert_eq!(
            observed.maximum.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }
}

struct FakeChild {
    exited: bool,
    graceful: usize,
    forced: usize,
}
impl CancellableChild for FakeChild {
    fn request_graceful_stop(&mut self) -> std::io::Result<()> {
        self.graceful += 1;
        Ok(())
    }
    fn has_exited(&mut self) -> std::io::Result<bool> {
        Ok(self.exited)
    }
    fn force_stop(&mut self) -> std::io::Result<()> {
        self.forced += 1;
        Ok(())
    }
}
#[tokio::test]
async fn child_cancellation_only_forces_stubborn_processes() {
    let token = CancellationToken::new();
    token.cancel();
    let mut exited = FakeChild {
        exited: true,
        graceful: 0,
        forced: 0,
    };
    cancel_child_safely(&mut exited, token, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!((exited.graceful, exited.forced), (1, 0));
    let token = CancellationToken::new();
    token.cancel();
    let mut stubborn = FakeChild {
        exited: false,
        graceful: 0,
        forced: 0,
    };
    cancel_child_safely(&mut stubborn, token, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!((stubborn.graceful, stubborn.forced), (1, 1));
}

struct CancelAwareHandler {
    started: std::sync::mpsc::Sender<JobId>,
}
impl JobHandler for CancelAwareHandler {
    fn run(
        &self,
        job: ClaimedJob,
        cancellation: CancellationToken,
    ) -> Result<JobResultCommit, JobHandlerFailure> {
        let _ = self.started.send(job.job.job_id.clone());
        while !cancellation.is_cancelled() {
            std::thread::sleep(Duration::from_millis(2));
        }
        Err(JobHandlerFailure {
            kind: FailureKind::Retryable,
            code: "interrupted".into(),
            details: json!({}),
        })
    }
}
async fn wait_for_event(engine: &JobEngine, job_id: &JobId, state: JobState) {
    for _ in 0..100 {
        if engine
            .replay_events(0, 500)
            .unwrap()
            .events
            .iter()
            .any(|event| event.job_id == *job_id && event.state == state)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {state:?}");
}
#[tokio::test]
async fn durable_cancellation_reaches_blocked_handler_and_commits_canceled() {
    let (_dir, engine, _clock) = engine();
    let definition = job(150, engine.node_id());
    let id = definition.job_id.clone();
    engine.enqueue(definition).unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let shutdown = CancellationToken::new();
    let scheduler = JobScheduler::new(
        engine.clone(),
        JobSchedulerConfig::default(),
        shutdown.clone(),
    )
    .unwrap();
    let task = tokio::spawn(async move {
        scheduler
            .run(Arc::new(CancelAwareHandler { started: sender }))
            .await
    });
    assert_eq!(
        tokio::task::spawn_blocking(move || receiver.recv_timeout(Duration::from_secs(1)).unwrap())
            .await
            .unwrap(),
        id
    );
    engine.request_cancel(&id, Some("stop")).unwrap();
    wait_for_event(&engine, &id, JobState::Canceled).await;
    shutdown.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn lease_loss_cancels_blocked_handler_and_recovery_retries_stale_attempt() {
    let (_dir, engine, _clock) = engine();
    let definition = job(161, engine.node_id());
    let id = definition.job_id.clone();
    engine.enqueue(definition).unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let shutdown = CancellationToken::new();
    let config = JobSchedulerConfig {
        lease_duration: Duration::from_millis(20),
        ..JobSchedulerConfig::default()
    };
    let scheduler = JobScheduler::new(engine.clone(), config, shutdown.clone()).unwrap();
    let task = tokio::spawn(async move {
        scheduler
            .run(Arc::new(CancelAwareHandler { started: sender }))
            .await
    });
    assert_eq!(
        tokio::task::spawn_blocking(move || receiver.recv_timeout(Duration::from_secs(1)).unwrap())
            .await
            .unwrap(),
        id
    );
    let connection = open_writable_connection(&engine.database_path).unwrap();
    connection
        .execute(
            "UPDATE jobs SET lease_expires_at='2000-01-01T00:00:00.000Z' WHERE job_id=?1",
            [id.as_str()],
        )
        .unwrap();
    wait_for_event(&engine, &id, JobState::RetryWait).await;
    assert!(matches!(
        engine.complete(
            &ClaimedJob {
                job: job(999, engine.node_id()),
                state: JobState::Running,
                attempt_number: 1,
                lease_owner: "stale".into(),
                lease_token: "stale".into(),
                lease_expires_at: "2000-01-01T00:00:00.000Z".into(),
                completed_work_units: 0,
                created_at: "x".into()
            },
            JobResultCommit {
                output_identity: None,
                details: json!({})
            },
            |_| Ok(())
        ),
        Err(JobEngineError::StaleLease) | Err(JobEngineError::NotFound(_))
    ));
    shutdown.cancel();
    task.await.unwrap().unwrap();
}

struct FailOnceHandler(std::sync::atomic::AtomicUsize);
impl JobHandler for FailOnceHandler {
    fn run(
        &self,
        _job: ClaimedJob,
        _cancellation: CancellationToken,
    ) -> Result<JobResultCommit, JobHandlerFailure> {
        if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            Err(JobHandlerFailure {
                kind: FailureKind::Terminal,
                code: "intentional".into(),
                details: json!({}),
            })
        } else {
            Ok(JobResultCommit {
                output_identity: None,
                details: json!({}),
            })
        }
    }
}
#[tokio::test]
async fn handler_failure_is_persisted_and_scheduler_continues() {
    let (_dir, engine, _clock) = engine();
    let first = job(151, engine.node_id());
    let second = job(152, engine.node_id());
    let first_id = first.job_id.clone();
    let second_id = second.job_id.clone();
    engine.enqueue(first).unwrap();
    engine.enqueue(second).unwrap();
    let shutdown = CancellationToken::new();
    let scheduler = JobScheduler::new(
        engine.clone(),
        JobSchedulerConfig::default(),
        shutdown.clone(),
    )
    .unwrap();
    let task = tokio::spawn(async move {
        scheduler
            .run(Arc::new(FailOnceHandler(
                std::sync::atomic::AtomicUsize::new(0),
            )))
            .await
    });
    // Heavy work is intentionally LIFO: the newest eligible job runs
    // first, fails once here, and the older job then succeeds.
    wait_for_event(&engine, &second_id, JobState::Failed).await;
    wait_for_event(&engine, &first_id, JobState::Succeeded).await;
    shutdown.cancel();
    task.await.unwrap().unwrap();
}

#[test]
fn scheduler_and_lease_bounds_are_safe_and_allow_disabled_classes() {
    let mut config = JobSchedulerConfig {
        proxy_workers: 0,
        full_hash_workers: 0,
        lightweight_workers: 1,
        ..JobSchedulerConfig::default()
    };
    assert!(config.validate().is_ok());
    config.lightweight_workers = 0;
    assert!(config.validate().is_err());
    config.proxy_workers = 2;
    assert!(config.validate().is_err());
    config.proxy_workers = 0;
    config.lightweight_workers = 1;
    config.cancellation_poll_interval = Duration::ZERO;
    assert!(config.validate().is_err());
    config.cancellation_poll_interval = Duration::from_secs(2);
    assert!(config.validate().is_err());
    let (_dir, engine, _clock) = engine();
    engine.enqueue(job(160, engine.node_id())).unwrap();
    assert!(
        engine
            .claim_next("worker", Duration::from_millis(1))
            .is_err()
    );
    assert!(
        engine
            .claim_next("worker", Duration::from_secs(3601))
            .is_err()
    );
}

#[test]
fn safe_payload_and_details_contract_rejects_commands_secrets_and_paths() {
    for payload in [json!({"Command":"x"}), json!({"ARGV":["x"]})] {
        assert!(forbidden_command(&payload).is_err());
    }
    for detail in [
        json!({"Token":"x"}),
        json!({"nested":{"password":"x"}}),
        json!({"path":"/tmp/x"}),
        json!({"path":"\\\\server\\x"}),
        json!({"path":"C:\\x"}),
        json!({"path":"C:/x"}),
        json!({"path":"file:/x"}),
    ] {
        assert!(validate_event(&detail, MAX_EVENT_BYTES).is_err());
    }
    assert!(
        validate_event(
            &json!({"code":"media_offline","item":"clip_1"}),
            MAX_EVENT_BYTES
        )
        .is_ok()
    );
}

#[test]
fn heartbeat_extends_lease_without_replay_event() {
    let (_dir, engine, _clock) = engine();
    engine.enqueue(job(170, engine.node_id())).unwrap();
    let claimed = engine
        .claim_next("worker", Duration::from_secs(1))
        .unwrap()
        .unwrap();
    let before = engine.replay_events(0, 500).unwrap().events.len();
    engine.heartbeat(&claimed, Duration::from_secs(2)).unwrap();
    assert_eq!(engine.replay_events(0, 500).unwrap().events.len(), before);
}

#[tokio::test]
async fn scheduler_commit_hook_is_atomic() {
    struct Hook {
        fail: bool,
        started: Option<std::sync::mpsc::Sender<JobId>>,
    }
    impl JobHandler for Hook {
        fn run(
            &self,
            job: ClaimedJob,
            _: CancellationToken,
        ) -> Result<JobResultCommit, JobHandlerFailure> {
            if let Some(started) = &self.started {
                let _ = started.send(job.job.job_id);
            }
            Ok(JobResultCommit {
                output_identity: None,
                details: json!({}),
            })
        }

        fn commit_result(
            &self,
            tx: &Transaction<'_>,
            _: &ClaimedJob,
            _: &JobResultCommit,
        ) -> Result<(), JobEngineError> {
            tx.execute(
                "INSERT INTO operational_metadata(key,value,updated_at) VALUES('scheduler-hook','ok','now')",
                [],
            )?;
            if self.fail {
                Err(JobEngineError::InvalidInput("hook".into()))
            } else {
                Ok(())
            }
        }
    }
    let (_dir, engine, _clock) = engine();
    engine.enqueue(job(180, engine.node_id())).unwrap();
    let shutdown = CancellationToken::new();
    let scheduler = JobScheduler::new(
        engine.clone(),
        JobSchedulerConfig::default(),
        shutdown.clone(),
    )
    .unwrap();
    let task = tokio::spawn(async move {
        scheduler
            .run(Arc::new(Hook {
                fail: false,
                started: None,
            }))
            .await
    });
    wait_for_event(
        &engine,
        &JobId::parse("job_180").unwrap(),
        JobState::Succeeded,
    )
    .await;
    shutdown.cancel();
    task.await.unwrap().unwrap();

    let connection = open_writable_connection(&engine.database_path).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM operational_metadata WHERE key='scheduler-hook'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    drop(connection);

    let failed = job(183, engine.node_id());
    let failed_id = failed.job_id.clone();
    engine.enqueue(failed).unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let shutdown = CancellationToken::new();
    let scheduler = JobScheduler::new(
        engine.clone(),
        JobSchedulerConfig::default(),
        shutdown.clone(),
    )
    .unwrap();
    let task = tokio::spawn(async move {
        scheduler
            .run(Arc::new(Hook {
                fail: true,
                started: Some(sender),
            }))
            .await
    });
    assert_eq!(
        tokio::task::spawn_blocking(move || receiver.recv_timeout(Duration::from_secs(1)).unwrap())
            .await
            .unwrap(),
        failed_id
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    let connection = open_writable_connection(&engine.database_path).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM operational_metadata WHERE key='scheduler-hook'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT state FROM jobs WHERE job_id=?1",
                [failed_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "running"
    );
    assert!(
        !engine
            .replay_events(0, 500)
            .unwrap()
            .events
            .iter()
            .any(|event| event.job_id == failed_id && event.state == JobState::Succeeded)
    );
    connection
        .execute(
            "UPDATE jobs SET lease_expires_at='2000-01-01T00:00:00.000Z' WHERE job_id=?1",
            [failed_id.as_str()],
        )
        .unwrap();
    drop(connection);
    engine.recover_expired().unwrap();
    wait_for_event(&engine, &failed_id, JobState::RetryWait).await;
    shutdown.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn scheduler_preserves_retryable_and_terminal_failure_categories() {
    struct Categories(std::sync::atomic::AtomicUsize);
    impl JobHandler for Categories {
        fn run(
            &self,
            _: ClaimedJob,
            _: CancellationToken,
        ) -> Result<JobResultCommit, JobHandlerFailure> {
            let run = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(JobHandlerFailure {
                kind: if run == 0 {
                    FailureKind::Retryable
                } else {
                    FailureKind::Terminal
                },
                code: if run == 0 {
                    "retryable".into()
                } else {
                    "terminal".into()
                },
                details: json!({}),
            })
        }
    }
    let (_dir, engine, _clock) = engine();
    // Heavy selection is intentional LIFO. Queue the terminal case first
    // so the newest retryable probe observes the first handler failure.
    let retryable = job(181, engine.node_id());
    let terminal = job(182, engine.node_id());
    engine.enqueue(terminal.clone()).unwrap();
    engine.enqueue(retryable.clone()).unwrap();
    let shutdown = CancellationToken::new();
    let scheduler = JobScheduler::new(
        engine.clone(),
        JobSchedulerConfig::default(),
        shutdown.clone(),
    )
    .unwrap();
    let task = tokio::spawn(async move {
        scheduler
            .run(Arc::new(Categories(std::sync::atomic::AtomicUsize::new(0))))
            .await
    });
    wait_for_event(&engine, &retryable.job_id, JobState::RetryWait).await;
    wait_for_event(&engine, &terminal.job_id, JobState::Failed).await;
    shutdown.cancel();
    task.await.unwrap().unwrap();
}

#[test]
fn unsafe_handler_failure_is_rejected_without_losing_the_lease() {
    let (_dir, engine, _clock) = engine();
    engine.enqueue(job(184, engine.node_id())).unwrap();
    let claimed = engine
        .claim_next("worker", Duration::from_secs(1))
        .unwrap()
        .unwrap();

    assert!(matches!(
        engine.fail(
            &claimed,
            FailureKind::Terminal,
            "invalid error code",
            json!({"reason":"invalid"}),
        ),
        Err(JobEngineError::InvalidInput(_))
    ));
    assert!(matches!(
        engine.fail(
            &claimed,
            FailureKind::Terminal,
            "safe_code",
            json!({"path":"/private/unsafe"}),
        ),
        Err(JobEngineError::InvalidInput(_))
    ));
    engine.heartbeat(&claimed, Duration::from_secs(1)).unwrap();
}

#[tokio::test]
async fn shutdown_interruption_retries_but_success_after_shutdown_commits() {
    struct SucceedsAfterShutdown {
        started: std::sync::mpsc::Sender<JobId>,
    }
    impl JobHandler for SucceedsAfterShutdown {
        fn run(
            &self,
            job: ClaimedJob,
            cancellation: CancellationToken,
        ) -> Result<JobResultCommit, JobHandlerFailure> {
            self.started.send(job.job.job_id).unwrap();
            while !cancellation.is_cancelled() {
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(JobResultCommit {
                output_identity: None,
                details: json!({"finishedAfterShutdown":true}),
            })
        }
    }

    let (_dir, engine, _clock) = engine();
    let interrupted = job(185, engine.node_id());
    let interrupted_id = interrupted.job_id.clone();
    engine.enqueue(interrupted).unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let shutdown = CancellationToken::new();
    let scheduler = JobScheduler::new(
        engine.clone(),
        JobSchedulerConfig::default(),
        shutdown.clone(),
    )
    .unwrap();
    let task = tokio::spawn(async move {
        scheduler
            .run(Arc::new(CancelAwareHandler { started: sender }))
            .await
    });
    assert_eq!(
        tokio::task::spawn_blocking(move || receiver.recv_timeout(Duration::from_secs(1)).unwrap())
            .await
            .unwrap(),
        interrupted_id
    );
    shutdown.cancel();
    wait_for_event(&engine, &interrupted_id, JobState::RetryWait).await;
    task.await.unwrap().unwrap();
    let connection = open_writable_connection(&engine.database_path).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT error_code FROM jobs WHERE job_id=?1",
                [interrupted_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "server_shutdown"
    );
    drop(connection);

    let succeeds = job(186, engine.node_id());
    let succeeds_id = succeeds.job_id.clone();
    engine.enqueue(succeeds).unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let shutdown = CancellationToken::new();
    let scheduler = JobScheduler::new(
        engine.clone(),
        JobSchedulerConfig::default(),
        shutdown.clone(),
    )
    .unwrap();
    let task = tokio::spawn(async move {
        scheduler
            .run(Arc::new(SucceedsAfterShutdown { started: sender }))
            .await
    });
    assert_eq!(
        tokio::task::spawn_blocking(move || receiver.recv_timeout(Duration::from_secs(1)).unwrap())
            .await
            .unwrap(),
        succeeds_id
    );
    shutdown.cancel();
    wait_for_event(&engine, &succeeds_id, JobState::Succeeded).await;
    task.await.unwrap().unwrap();
}

#[test]
fn heavy_claim_is_lifo_across_representation_kinds_and_work_conserving() {
    let (_dir, engine, _clock) = engine();
    let proxy = job(190, engine.node_id());
    let proxy_id = proxy.job_id.clone();
    let mut hash = job(191, engine.node_id());
    hash.job_class = JobClass::FullHash;
    let hash_id = hash.job_id.clone();
    engine.enqueue(proxy).unwrap();
    engine.enqueue(hash).unwrap();

    // The newest eligible heavy job wins across classes.
    let first = engine
        .claim_next_heavy("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    assert_eq!(first.job.job_id, hash_id);
    let second = engine
        .claim_next_heavy("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    assert_eq!(second.job.job_id, proxy_id);
    assert!(
        engine
            .claim_next_heavy("worker", Duration::from_secs(10))
            .unwrap()
            .is_none()
    );

    // A queue containing only one representation kind is still eligible;
    // it cannot sit idle behind an empty sibling class.
    let mut only_hash = job(192, engine.node_id());
    only_hash.job_class = JobClass::FullHash;
    let only_hash_id = only_hash.job_id.clone();
    engine.enqueue(only_hash).unwrap();
    let claimed = engine
        .claim_next_heavy("worker", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    assert_eq!(claimed.job.job_id, only_hash_id);
}

#[test]
fn disabled_heavy_class_filters_shared_eligible_queue() {
    let (_dir, engine, _clock) = engine();
    let mut proxy = job(196, engine.node_id());
    proxy.job_class = JobClass::Proxy;
    let mut hash = job(197, engine.node_id());
    hash.job_class = JobClass::FullHash;
    let hash_id = hash.job_id.clone();
    engine.enqueue(proxy).unwrap();
    engine.enqueue(hash).unwrap();

    // Proxy is newer and would win LIFO in the union, but it is disabled.
    let claimed = engine
        .claim_next_heavy_enabled("worker", Duration::from_secs(10), false, true)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.job.job_id, hash_id);
    assert_eq!(claimed.job.job_class, JobClass::FullHash);

    // With only the disabled class queued, the eligible pool is empty
    // rather than manufacturing work the configuration forbade.
    let mut only_proxy = job(198, engine.node_id());
    only_proxy.job_class = JobClass::Proxy;
    let only_proxy_id = only_proxy.job_id.clone();
    engine.enqueue(only_proxy).unwrap();
    assert!(
        engine
            .claim_next_heavy_enabled("worker", Duration::from_secs(10), false, true,)
            .unwrap()
            .is_none(),
        "a configured-off class must never be claimed"
    );
    let claimed = engine
        .claim_next_heavy_enabled("worker", Duration::from_secs(10), true, false)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.job.job_id, only_proxy_id);
}

#[tokio::test]
async fn scheduler_serializes_heavy_classes_but_overlaps_safe_lightweight_work() {
    struct ResourceHandler {
        started: std::sync::mpsc::Sender<(JobId, JobClass)>,
        release: Arc<std::sync::atomic::AtomicBool>,
        heavy_running: std::sync::atomic::AtomicUsize,
        maximum_heavy: std::sync::atomic::AtomicUsize,
    }
    impl JobHandler for ResourceHandler {
        fn run(
            &self,
            job: ClaimedJob,
            _cancellation: CancellationToken,
        ) -> Result<JobResultCommit, JobHandlerFailure> {
            if job.job.job_class.is_local_heavy() {
                let current = self
                    .heavy_running
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1;
                self.maximum_heavy
                    .fetch_max(current, std::sync::atomic::Ordering::SeqCst);
            }
            self.started
                .send((job.job.job_id.clone(), job.job.job_class))
                .unwrap();
            while !self.release.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            if job.job.job_class.is_local_heavy() {
                self.heavy_running
                    .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(JobResultCommit {
                output_identity: None,
                details: json!({}),
            })
        }
    }

    let (_dir, engine, _clock) = engine();
    let proxy = job(193, engine.node_id());
    let mut hash = job(194, engine.node_id());
    hash.job_class = JobClass::FullHash;
    let mut light = job(195, engine.node_id());
    light.job_class = JobClass::Lightweight;
    engine.enqueue(proxy).unwrap();
    engine.enqueue(hash).unwrap();
    engine.enqueue(light).unwrap();

    let shutdown = CancellationToken::new();
    let scheduler = JobScheduler::new(
        engine.clone(),
        JobSchedulerConfig {
            local_heavy_lock_path: None,
            ..JobSchedulerConfig::default()
        },
        shutdown.clone(),
    )
    .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handler = Arc::new(ResourceHandler {
        started: sender,
        release: release.clone(),
        heavy_running: std::sync::atomic::AtomicUsize::new(0),
        maximum_heavy: std::sync::atomic::AtomicUsize::new(0),
    });
    let observed = handler.clone();
    let task = tokio::spawn(async move { scheduler.run(handler).await });

    let first_two = tokio::task::spawn_blocking(move || {
        vec![
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        ]
    })
    .await
    .unwrap();
    assert_eq!(
        first_two
            .iter()
            .filter(|(_, class)| class.is_local_heavy())
            .count(),
        1,
        "exactly one heavy job may run at a time"
    );
    assert_eq!(
        first_two
            .iter()
            .filter(|(_, class)| *class == JobClass::Lightweight)
            .count(),
        1,
        "lightweight work must overlap the heavy slot rather than wait for it"
    );

    release.store(true, std::sync::atomic::Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(40)).await;
    shutdown.cancel();
    task.await.unwrap().unwrap();
    assert_eq!(
        observed
            .maximum_heavy
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}
