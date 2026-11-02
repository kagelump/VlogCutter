use super::*;
use crate::review_targets::tests::{PROJECT, actor, catalog, now, request, valid};
use crate::{ControlCatalog, ReviewTargetHandoff};
use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    catalog: ControlCatalog,
    sessions: ReviewSessions,
    target_id: String,
}

fn fixture() -> Fixture {
    let root = TempDir::new().unwrap();
    let catalog = catalog(&root);
    let target_id = catalog
        .review_targets()
        .import(PROJECT, &request(valid()), &actor(), now())
        .unwrap()
        .review_target
        .summary
        .review_target_id;
    let sessions = catalog.review_sessions(0);
    Fixture {
        _root: root,
        catalog,
        sessions,
        target_id,
    }
}

fn create_request(target: &str, key: &str) -> CreateReviewSessionRequest {
    CreateReviewSessionRequest {
        review_target_id: target.into(),
        client_session_key: key.into(),
        consent: RecordingConsent {
            all_present_acknowledged: true,
            additional_speakers_disclosed: true,
        },
        audio: RecordingAudioFormat {
            sample_rate: 48_000,
            channels: 1,
            encoding: "pcm_s16le".into(),
        },
        capture: RecordingCaptureSettings {
            echo_cancellation: Some(false),
            noise_suppression: Some(false),
            auto_gain_control: Some(false),
            clock: "audioWorkletCurrentFrame".into(),
            base_latency_seconds: Some(0.01),
            user_agent_family: Some("chromium".into()),
        },
        client_started_performance_ms: Some(1234.5),
    }
}

/// `frames` mono samples of a deterministic ramp starting at `start`.
fn pcm(start: u64, frames: u32) -> Vec<u8> {
    (0..u64::from(frames))
        .flat_map(|i| (((start + i) % 30_000) as i16).to_le_bytes())
        .collect()
}

fn upload(
    f: &Fixture,
    session: &str,
    sequence: u32,
    start: u64,
    frames: u32,
) -> Result<ChunkReceipt, ReviewSessionError> {
    let bytes = pcm(start, frames);
    f.sessions.put_chunk(
        PROJECT,
        session,
        &ChunkUpload {
            sequence,
            start_frame: start,
            frame_count: frames,
            sha256: sha256_hex(&bytes),
            capture_end_performance_ms: Some(2000.0 + f64::from(sequence)),
        },
        &bytes,
        now(),
    )
}

fn finalize(
    f: &Fixture,
    session: &str,
    frames: u64,
    reason: RecordingEndReason,
) -> Result<ReviewSessionDetail, ReviewSessionError> {
    f.sessions.finalize(
        PROJECT,
        session,
        &FinalizeReviewSessionRequest {
            final_frame_count: frames,
            end_reason: reason,
        },
        now(),
    )
}

#[test]
fn a_complete_recording_is_contiguous_hashed_and_downloadable() {
    let f = fixture();
    let session = f
        .sessions
        .create(
            PROJECT,
            &create_request(&f.target_id, "client_key_1"),
            &actor(),
            now(),
        )
        .unwrap();
    let id = session.summary.session_id.clone();
    assert!(id.starts_with("rsession_"));
    assert_eq!(session.summary.state, ReviewSessionState::Recording);
    assert_eq!(session.summary.revision_id, "editrev_synthetic");
    // Idempotent create.
    let again = f
        .sessions
        .create(
            PROJECT,
            &create_request(&f.target_id, "client_key_1"),
            &actor(),
            now(),
        )
        .unwrap();
    assert_eq!(again.summary.session_id, id);

    // Out-of-order arrival is fine; retries of the identical chunk are duplicates.
    upload(&f, &id, 1, 4800, 4800).unwrap();
    upload(&f, &id, 0, 0, 4800).unwrap();
    assert!(upload(&f, &id, 0, 0, 4800).unwrap().duplicate);
    let cue = f
        .sessions
        .record_event(
            PROJECT,
            &id,
            &ReviewSessionEventRequest {
                event_id: "cue_1".into(),
                kind: ReviewSessionEventKind::ManualSyncCue,
                audio_frame: 2400,
                performance_ms: Some(1500.0),
                declared_movie_seconds: Some(0.0),
                detail: Some(serde_json::json!({"label": "videoFirstFrame"})),
            },
            now(),
        )
        .unwrap();
    assert_eq!(cue.audio_seconds, 0.05);

    let done = finalize(&f, &id, 9600, RecordingEndReason::Stopped).unwrap();
    assert_eq!(done.summary.state, ReviewSessionState::Completed);
    assert!(done.coverage.gaps.is_empty());
    assert_eq!(done.coverage.received_frames, 9600);
    assert_eq!(done.coverage.received_seconds, 0.2);
    assert_eq!(done.summary.manual_cue_count, 1);
    // Identical finalize is idempotent; a different one is refused.
    assert_eq!(
        finalize(&f, &id, 9600, RecordingEndReason::Stopped)
            .unwrap()
            .summary
            .state,
        ReviewSessionState::Completed
    );
    assert!(matches!(
        finalize(&f, &id, 9601, RecordingEndReason::Stopped),
        Err(ReviewSessionError::NotRecording)
    ));
    assert!(matches!(
        upload(&f, &id, 2, 9600, 10),
        Err(ReviewSessionError::NotRecording)
    ));

    let audio = f.sessions.audio(PROJECT, &id).unwrap();
    assert_eq!(audio.data_bytes, 9600 * 2);
    let mut joined = Vec::new();
    for path in &audio.chunk_paths {
        joined.extend(std::fs::read(path).unwrap());
    }
    assert_eq!(joined, pcm(0, 9600));
    let header = wav_header(audio.sample_rate, audio.channels, audio.data_bytes);
    assert_eq!(&header[0..4], b"RIFF");
    assert_eq!(
        u32::from_le_bytes(header[40..44].try_into().unwrap()),
        19_200
    );
    // Evidence lives in the private data directory, not a derived root.
    assert!(
        audio.chunk_paths[0]
            .to_string_lossy()
            .contains("review-evidence")
    );
}

#[test]
fn gaps_are_reported_never_filled_and_incomplete_audio_is_not_padded() {
    let f = fixture();
    let id = f
        .sessions
        .create(
            PROJECT,
            &create_request(&f.target_id, "key_gap"),
            &actor(),
            now(),
        )
        .unwrap()
        .summary
        .session_id;
    upload(&f, &id, 0, 0, 4800).unwrap();
    // Sequence 1 was lost locally (quota); the client reports it.
    f.sessions
        .record_event(
            PROJECT,
            &id,
            &ReviewSessionEventRequest {
                event_id: "overflow_1".into(),
                kind: ReviewSessionEventKind::LocalBufferOverflow,
                audio_frame: 4800,
                performance_ms: None,
                declared_movie_seconds: None,
                detail: Some(serde_json::json!({"lostFrames": 4800})),
            },
            now(),
        )
        .unwrap();
    upload(&f, &id, 2, 9600, 4800).unwrap();
    let done = finalize(&f, &id, 19_200, RecordingEndReason::DeviceLost).unwrap();
    assert_eq!(done.summary.state, ReviewSessionState::Incomplete);
    assert_eq!(
        done.summary.end_reason,
        Some(RecordingEndReason::DeviceLost)
    );
    assert_eq!(
        done.coverage.gaps,
        vec![
            FrameRange {
                start_frame: 4800,
                end_frame: 9600
            },
            FrameRange {
                start_frame: 14_400,
                end_frame: 19_200
            },
        ]
    );
    assert_eq!(done.coverage.missing_sequences, vec![1]);
    assert!(matches!(
        f.sessions.audio(PROJECT, &id),
        Err(ReviewSessionError::AudioUnavailable)
    ));
    // Individual received chunks stay retrievable.
    assert_eq!(
        f.sessions.chunk_bytes(PROJECT, &id, 2).unwrap(),
        pcm(9600, 4800)
    );
}

#[test]
fn conflicting_overlapping_corrupt_and_oversized_chunks_are_refused() {
    let f = fixture();
    let id = f
        .sessions
        .create(
            PROJECT,
            &create_request(&f.target_id, "key_bad"),
            &actor(),
            now(),
        )
        .unwrap()
        .summary
        .session_id;
    upload(&f, &id, 0, 0, 4800).unwrap();
    // Same sequence, different content.
    assert!(matches!(
        upload(&f, &id, 0, 1, 4800),
        Err(ReviewSessionError::ChunkConflict)
    ));
    // Overlapping frames under a new sequence.
    assert!(matches!(
        upload(&f, &id, 1, 4000, 4800),
        Err(ReviewSessionError::ChunkConflict)
    ));
    // Declared hash does not match the body.
    let bytes = pcm(4800, 10);
    let bad = f.sessions.put_chunk(
        PROJECT,
        &id,
        &ChunkUpload {
            sequence: 1,
            start_frame: 4800,
            frame_count: 10,
            sha256: "0".repeat(64),
            capture_end_performance_ms: None,
        },
        &bytes,
        now(),
    );
    assert!(matches!(bad, Err(ReviewSessionError::ChunkCorrupt)));
    // Length inconsistent with frame count.
    let wrong = f.sessions.put_chunk(
        PROJECT,
        &id,
        &ChunkUpload {
            sequence: 1,
            start_frame: 4800,
            frame_count: 11,
            sha256: sha256_hex(&bytes),
            capture_end_performance_ms: None,
        },
        &bytes,
        now(),
    );
    assert!(matches!(wrong, Err(ReviewSessionError::InvalidInput(_))));
    let huge = vec![0_u8; REVIEW_SESSION_MAX_CHUNK_BYTES + 2];
    let too_big = f.sessions.put_chunk(
        PROJECT,
        &id,
        &ChunkUpload {
            sequence: 9,
            start_frame: 100_000,
            frame_count: (huge.len() / 2) as u32,
            sha256: sha256_hex(&huge),
            capture_end_performance_ms: None,
        },
        &huge,
        now(),
    );
    assert!(matches!(too_big, Err(ReviewSessionError::InvalidInput(_))));
    // finalFrameCount below received audio is refused.
    assert!(matches!(
        finalize(&f, &id, 100, RecordingEndReason::Stopped),
        Err(ReviewSessionError::InvalidInput(_))
    ));
}

#[test]
fn a_modified_stored_chunk_makes_the_session_corrupt() {
    let f = fixture();
    let id = f
        .sessions
        .create(
            PROJECT,
            &create_request(&f.target_id, "key_corrupt"),
            &actor(),
            now(),
        )
        .unwrap()
        .summary
        .session_id;
    upload(&f, &id, 0, 0, 4800).unwrap();
    std::fs::write(f.sessions.chunk_path(&id, 0), pcm(1, 4800)).unwrap();
    let done = finalize(&f, &id, 4800, RecordingEndReason::Stopped).unwrap();
    assert_eq!(done.summary.state, ReviewSessionState::Corrupt);
    assert!(f.sessions.audio(PROJECT, &id).is_err());
}

#[test]
fn creation_requires_consent_a_ready_target_and_bounded_concurrency() {
    let f = fixture();
    let mut no_consent = create_request(&f.target_id, "key_consent");
    no_consent.consent.all_present_acknowledged = false;
    assert!(matches!(
        f.sessions.create(PROJECT, &no_consent, &actor(), now()),
        Err(ReviewSessionError::InvalidInput(_))
    ));
    let mut bad_format = create_request(&f.target_id, "key_format");
    bad_format.audio.encoding = "opus".into();
    assert!(
        f.sessions
            .create(PROJECT, &bad_format, &actor(), now())
            .is_err()
    );

    let stale = f.sessions.create(
        PROJECT,
        &create_request(&f.target_id, "key_stale"),
        &actor(),
        now() + Duration::days(8),
    );
    match stale {
        Err(error @ ReviewSessionError::TargetNotReady(_)) => {
            assert_eq!(error.code(), "review_target_verification_expired")
        }
        other => panic!("expected not ready, got {other:?}"),
    }
    assert!(matches!(
        f.sessions.create(
            PROJECT,
            &create_request("rtarget_missing", "key_missing"),
            &actor(),
            now()
        ),
        Err(ReviewSessionError::NotFound)
    ));

    for index in 0..REVIEW_SESSION_MAX_ACTIVE {
        f.sessions
            .create(
                PROJECT,
                &create_request(&f.target_id, &format!("key_active_{index}")),
                &actor(),
                now(),
            )
            .unwrap();
    }
    assert!(matches!(
        f.sessions.create(
            PROJECT,
            &create_request(&f.target_id, "key_over"),
            &actor(),
            now()
        ),
        Err(ReviewSessionError::Busy)
    ));
    // Reusing a key for another target is an idempotency conflict.
    let other = f
        .catalog
        .review_targets()
        .rebind_youtube(PROJECT, &f.target_id, Some("synthetic09"), &actor(), now())
        .unwrap()
        .review_target
        .summary
        .review_target_id;
    assert!(matches!(
        f.sessions.create(
            PROJECT,
            &create_request(&other, "key_active_0"),
            &actor(),
            now()
        ),
        Err(ReviewSessionError::IdempotencyConflict)
    ));
}

#[test]
fn deletion_removes_audio_and_keeps_an_accounted_tombstone() {
    let f = fixture();
    let id = f
        .sessions
        .create(
            PROJECT,
            &create_request(&f.target_id, "key_delete"),
            &actor(),
            now(),
        )
        .unwrap()
        .summary
        .session_id;
    upload(&f, &id, 0, 0, 4800).unwrap();
    let directory = f.sessions.session_directory(&id);
    assert!(directory.exists());
    let deleted = f.sessions.delete(PROJECT, &id, now()).unwrap();
    assert_eq!(deleted.summary.state, ReviewSessionState::Deleted);
    assert!(!directory.exists());
    assert_eq!(deleted.coverage.chunk_count, 1);
    assert!(f.sessions.chunk_bytes(PROJECT, &id, 0).is_err());
    assert!(matches!(
        upload(&f, &id, 1, 4800, 10),
        Err(ReviewSessionError::NotRecording)
    ));
    // Deleting twice is harmless.
    f.sessions.delete(PROJECT, &id, now()).unwrap();
}

#[test]
fn an_unfinalized_session_past_its_lifetime_is_reported_abandoned_but_finalizable() {
    let f = fixture();
    let id = f
        .sessions
        .create(
            PROJECT,
            &create_request(&f.target_id, "key_abandon"),
            &actor(),
            now(),
        )
        .unwrap()
        .summary
        .session_id;
    upload(&f, &id, 0, 0, 4800).unwrap();
    let later = f
        .sessions
        .get(PROJECT, &id, now() + Duration::hours(13))
        .unwrap();
    assert!(later.summary.abandoned);
    let done = finalize(&f, &id, 4800, RecordingEndReason::Interrupted).unwrap();
    assert_eq!(done.summary.state, ReviewSessionState::Completed);
    assert!(!done.summary.abandoned);
    let page = f
        .sessions
        .list(PROJECT, Some(&f.target_id), 10, None, now())
        .unwrap();
    assert_eq!(page.items.len(), 1);
    // Evidence is separate from the immutable target.
    let handoff: ReviewTargetHandoff = f
        .catalog
        .review_targets()
        .handoff(PROJECT, &f.target_id)
        .unwrap();
    handoff.validate().unwrap();
}

#[test]
fn event_replay_compares_the_full_payload_and_clock_samples_declare_epoch_and_bound() {
    let f = fixture();
    let id = f
        .sessions
        .create(
            PROJECT,
            &create_request(&f.target_id, "key_events"),
            &actor(),
            now(),
        )
        .unwrap()
        .summary
        .session_id;
    let cue = ReviewSessionEventRequest {
        event_id: "cue_replay".into(),
        kind: ReviewSessionEventKind::ManualSyncCue,
        audio_frame: 4800,
        performance_ms: Some(2500.5),
        declared_movie_seconds: Some(0.0),
        detail: Some(serde_json::json!({"clockEpoch": 0, "method": "audioContextCurrentTime"})),
    };
    let first = f.sessions.record_event(PROJECT, &id, &cue, now()).unwrap();
    // An identical retry (e.g. after a lost response) returns the stored event.
    let replay = f.sessions.record_event(PROJECT, &id, &cue, now()).unwrap();
    assert_eq!(first.event_id, replay.event_id);
    // The same id with a different declared movie time, reading or detail conflicts.
    for changed in [
        ReviewSessionEventRequest {
            declared_movie_seconds: Some(12.0),
            ..cue.clone()
        },
        ReviewSessionEventRequest {
            performance_ms: Some(2600.0),
            ..cue.clone()
        },
        ReviewSessionEventRequest {
            detail: Some(serde_json::json!({"clockEpoch": 1})),
            ..cue.clone()
        },
        ReviewSessionEventRequest {
            detail: None,
            ..cue.clone()
        },
    ] {
        assert!(matches!(
            f.sessions.record_event(PROJECT, &id, &changed, now()),
            Err(ReviewSessionError::IdempotencyConflict)
        ));
    }

    let sample = ReviewSessionEventRequest {
        event_id: "clock_1".into(),
        kind: ReviewSessionEventKind::ClockSample,
        audio_frame: 9600,
        performance_ms: Some(3000.25),
        declared_movie_seconds: None,
        detail: Some(
            serde_json::json!({"clockEpoch": 0, "uncertaintyMs": 2.6667, "method": "getOutputTimestamp"}),
        ),
    };
    f.sessions
        .record_event(PROJECT, &id, &sample, now())
        .unwrap();
    for (event_id, performance_ms, detail) in [
        (
            "clock_no_epoch",
            Some(1.0),
            serde_json::json!({"uncertaintyMs": 1.0}),
        ),
        (
            "clock_no_bound",
            Some(1.0),
            serde_json::json!({"clockEpoch": 0}),
        ),
        (
            "clock_negative",
            Some(1.0),
            serde_json::json!({"clockEpoch": 0, "uncertaintyMs": -1.0}),
        ),
        (
            "clock_no_reading",
            None,
            serde_json::json!({"clockEpoch": 0, "uncertaintyMs": 1.0}),
        ),
    ] {
        let bad = ReviewSessionEventRequest {
            event_id: event_id.into(),
            performance_ms,
            detail: Some(detail),
            ..sample.clone()
        };
        assert!(matches!(
            f.sessions.record_event(PROJECT, &id, &bad, now()),
            Err(ReviewSessionError::InvalidInput(_))
        ));
    }
    // An epoch boundary marker carries no pairing.
    let boundary = ReviewSessionEventRequest {
        event_id: "clock_suspended".into(),
        performance_ms: None,
        detail: Some(
            serde_json::json!({"clockEpoch": 0, "transition": "suspended", "uncertaintyMs": null}),
        ),
        ..sample.clone()
    };
    f.sessions
        .record_event(PROJECT, &id, &boundary, now())
        .unwrap();
}

#[test]
fn native_frame_bounds_are_rejected_before_storage() {
    let f = fixture();
    let id = f
        .sessions
        .create(
            PROJECT,
            &create_request(&f.target_id, "frame_bounds"),
            &actor(),
            now(),
        )
        .unwrap()
        .summary
        .session_id;
    let event = ReviewSessionEventRequest {
        event_id: "too_far".into(),
        kind: ReviewSessionEventKind::ManualSyncCue,
        audio_frame: u64::MAX,
        performance_ms: Some(1500.0),
        declared_movie_seconds: Some(0.0),
        detail: Some(serde_json::json!({"clockEpoch": 0})),
    };
    assert!(matches!(
        f.sessions.record_event(PROJECT, &id, &event, now()),
        Err(ReviewSessionError::InvalidInput(_))
    ));
    assert!(
        f.sessions
            .get(PROJECT, &id, now())
            .unwrap()
            .events
            .is_empty()
    );
}

#[test]
fn unacknowledged_capture_tail_remains_incomplete_while_uploaded_bytes_are_usable() {
    let f = fixture();
    let id = f
        .sessions
        .create(
            PROJECT,
            &create_request(&f.target_id, "tail_unknown"),
            &actor(),
            now(),
        )
        .unwrap()
        .summary
        .session_id;
    upload(&f, &id, 0, 0, 4800).unwrap();
    f.sessions
        .record_event(
            PROJECT,
            &id,
            &ReviewSessionEventRequest {
                event_id: "tail".into(),
                kind: ReviewSessionEventKind::PageInterrupted,
                audio_frame: 4800,
                performance_ms: None,
                declared_movie_seconds: None,
                detail: Some(serde_json::json!({"captureTailStatus":"unknown"})),
            },
            now(),
        )
        .unwrap();
    let done = finalize(&f, &id, 4800, RecordingEndReason::Interrupted).unwrap();
    assert_eq!(done.summary.state, ReviewSessionState::Incomplete);
    assert_eq!(
        f.sessions.chunk_bytes(PROJECT, &id, 0).unwrap(),
        pcm(0, 4800)
    );
    assert!(f.sessions.audio(PROJECT, &id).is_err());
}
