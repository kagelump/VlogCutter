use super::*;
use crate::review_discussions::tests::calibration_event;
use crate::review_targets::tests::{PROJECT, actor, now};
use crate::review_transcripts::tests::{FakeRuntime, Fixture, RATE, fixture};
use crate::{
    ChunkUpload, FinalizeReviewSessionRequest, RecordingEndReason, ReviewSessionEventKind,
    ReviewSessionEventRequest,
};

fn tick(f: &Fixture, engine: &mut Option<Box<dyn SttEngine>>) {
    f.catalog
        .review_transcripts()
        .live_tick(
            &FakeRuntime::new(),
            engine,
            &f.catalog.jobs(),
            &|| false,
            &f._root.path().join("heavy.lock"),
            &now().to_rfc3339(),
        )
        .unwrap();
}
fn snapshot(f: &Fixture) -> LiveReviewTranscript {
    f.catalog
        .review_transcripts()
        .live_transcript(PROJECT, &f.session_id, &FakeRuntime::new())
        .unwrap()
}
fn enable(f: &Fixture) {
    f.catalog
        .review_transcripts()
        .enable_live(PROJECT, &f.session_id, &actor(), &FakeRuntime::new())
        .unwrap();
}
fn append(f: &Fixture, sequence: u32, start_seconds: u64, seconds: u32) {
    let samples = super::super::tests::tone(f64::from(seconds), &[(0.5, f64::from(seconds) - 0.5)]);
    let pcm: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    f.catalog
        .review_sessions(0)
        .put_chunk(
            PROJECT,
            &f.session_id,
            &ChunkUpload {
                sequence,
                start_frame: start_seconds * u64::from(RATE),
                frame_count: seconds * RATE,
                sha256: hex::encode(Sha256::digest(&pcm)),
                capture_end_performance_ms: None,
            },
            &pcm,
            now(),
        )
        .unwrap();
}
fn player(f: &Fixture, id: &str, audio: f64, movie: f64, state: i64) {
    f.catalog.review_sessions(0).record_event(PROJECT, &f.session_id, &ReviewSessionEventRequest {
            event_id: id.into(), kind: ReviewSessionEventKind::ClockSample, audio_frame: (audio * f64::from(RATE)) as u64,
            performance_ms: Some(audio * 1000.0), declared_movie_seconds: None,
            detail: Some(serde_json::json!({"clockEpoch":0,"method":"playerObservation","uncertaintyMs":6,
                "playback":{"schema":"youtube-player-observation.v1","positionSeconds":movie,"state":state,"rate":1,"observedPerformanceMs":audio*1000.0}})),
        }, now()).unwrap();
}

#[test]
fn live_is_opt_in_durable_and_refines_once_after_finalization() {
    let f = fixture(false, false);
    assert!(!snapshot(&f).enabled);
    let mut engine = None;
    tick(&f, &mut engine);
    assert!(engine.is_none());
    enable(&f);
    tick(&f, &mut engine);
    let first = snapshot(&f);
    assert!(first.enabled);
    assert_eq!(first.revision, 1);
    assert!(!first.lines.is_empty());
    assert!(
        first
            .lines
            .iter()
            .all(|l| l.movie_time.is_none() && l.asset_id.is_none())
    );
    // Reopening the owner and repeating ticks neither appends duplicates nor decodes old windows.
    tick(&f, &mut engine);
    assert_eq!(snapshot(&f).revision, first.revision);
    f.catalog
        .review_sessions(0)
        .finalize(
            PROJECT,
            &f.session_id,
            &FinalizeReviewSessionRequest {
                final_frame_count: u64::from(RATE) * 20,
                end_reason: RecordingEndReason::Stopped,
            },
            now(),
        )
        .unwrap();
    // A restart loses only the model, not the provisional text or auto-finalize intent.
    tick(&f, &mut None);
    let refining = snapshot(&f);
    assert_eq!(refining.state, "refining");
    assert_eq!(refining.lines.len(), first.lines.len());
    let id = refining.final_run_id.unwrap();
    tick(&f, &mut None);
    assert_eq!(snapshot(&f).final_run_id.as_deref(), Some(id.as_str()));
    f.catalog
        .review_transcripts()
        .execute(&id, &FakeRuntime::new(), &|| false)
        .unwrap();
    assert_eq!(snapshot(&f).state, "complete");
    assert_eq!(
        f.catalog
            .review_transcripts()
            .list(PROJECT, &f.session_id)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn rolling_window_revises_overlap_and_skips_backlog_without_bridging_gaps() {
    let f = fixture(false, false);
    enable(&f);
    let mut engine = None;
    tick(&f, &mut engine);
    append(&f, 2, 20, 10);
    tick(&f, &mut engine);
    let second = snapshot(&f);
    assert_eq!(second.processed_seconds, 30.0);
    assert_eq!(second.revision, 2);
    let ids: std::collections::HashSet<_> = second
        .lines
        .iter()
        .map(|l| &l.utterance.utterance_id)
        .collect();
    assert_eq!(ids.len(), second.lines.len());
    // A gap followed by far newer audio: do not feed absent audio or an unbounded backlog to ASR.
    append(&f, 3, 120, 20);
    append(&f, 4, 140, 20);
    tick(&f, &mut engine);
    let third = snapshot(&f);
    assert_eq!(third.processed_seconds, 160.0);
    assert!(
        third
            .lines
            .iter()
            .all(|l| l.utterance.end_seconds <= 30.0 || l.utterance.start_seconds >= 130.0)
    );
    assert!(third.lines.iter().all(|l| l.utterance.end_seconds <= 160.0));
}

#[test]
fn silence_never_calls_decode_and_cancellation_never_publishes() {
    struct Silent;
    impl SttEngine for Silent {
        fn detect_speech(&self, _: &[f32]) -> anyhow::Result<Vec<SpeechRegion>> {
            Ok(vec![])
        }
        fn decode_window(
            &self,
            _: &[f32],
            _: &PackedWindow,
        ) -> anyhow::Result<Vec<DecodedSegment>> {
            panic!("silence reached ASR")
        }
    }
    assert!(
        decode_live(&Silent, &vec![0.0; 16000], 0, 48000, RATE, &[], &|| false)
            .unwrap()
            .is_empty()
    );
    let f = fixture(false, false);
    enable(&f);
    f.catalog
        .review_transcripts()
        .live_tick(
            &FakeRuntime::new(),
            &mut None,
            &f.catalog.jobs(),
            &|| true,
            &f._root.path().join("heavy.lock"),
            &now().to_rfc3339(),
        )
        .unwrap();
    assert_eq!(snapshot(&f).revision, 0);
}

#[test]
fn resource_contention_keeps_recording_and_draft_state() {
    let f = fixture(false, false);
    enable(&f);
    let path = f._root.path().join("heavy.lock");
    let held = rcdvlog_core::try_acquire_local_heavy_at(&path)
        .unwrap()
        .unwrap();
    tick(&f, &mut None);
    assert_eq!(
        snapshot(&f).error_code.as_deref(),
        Some("review_live_resource_busy")
    );
    assert_eq!(snapshot(&f).revision, 0);
    drop(held);
    tick(&f, &mut None);
    assert_eq!(snapshot(&f).revision, 1);
    assert!(snapshot(&f).error_code.is_none());
}

#[test]
fn fresh_sync_evidence_updates_thumbnails_without_redecoding_text() {
    let f = fixture(false, false);
    enable(&f);
    tick(&f, &mut None);
    assert!(snapshot(&f).lines.iter().all(|l| l.movie_time.is_none()));
    calibration_event(&f, "live_calibration", 0, 0);
    player(&f, "live_player_a", 0.5, 0.0, 1);
    player(&f, "live_player_b", 5.5, 5.0, 1);
    let mapped = snapshot(&f);
    assert_eq!(mapped.revision, 1);
    let first = &mapped.lines[0];
    assert!(first.movie_time.is_some());
    assert!(first.asset_id.is_some());
    assert!(first.shot_number.is_some());
    assert!(mapped.lines[1..].iter().all(|l| l.movie_time.is_none()));
    player(&f, "live_pause", 2.5, 2.0, 2);
    // No interpolation across a pause, even when the decoded line is unchanged.
    assert!(snapshot(&f).lines.iter().all(|l| l.movie_time.is_none()));
}

#[test]
fn missing_live_model_still_schedules_the_full_pass_and_deleted_audio_hides_drafts() {
    struct NoLive(FakeRuntime);
    impl ReviewSpeechRuntime for NoLive {
        fn local_models_present(&self) -> bool {
            true
        }
        fn vad_model_present(&self) -> bool {
            true
        }
        fn local_engine(&self) -> anyhow::Result<Box<dyn SttEngine>> {
            self.0.local_engine()
        }
        fn detect_speech(&self, p: &[f32]) -> anyhow::Result<Vec<SpeechRegion>> {
            self.0.detect_speech(p)
        }
        fn cloud_credentials_present(&self) -> bool {
            false
        }
        fn cloud_transport(&self) -> anyhow::Result<Box<dyn NativeSttTransport>> {
            anyhow::bail!("not allowed")
        }
    }
    let f = fixture(false, false);
    enable(&f);
    let owner = f.catalog.review_transcripts();
    owner
        .live_tick(
            &NoLive(FakeRuntime::new()),
            &mut None,
            &f.catalog.jobs(),
            &|| false,
            &f._root.path().join("heavy.lock"),
            &now().to_rfc3339(),
        )
        .unwrap();
    assert_eq!(
        snapshot(&f).error_code.as_deref(),
        Some("review_live_model_unavailable")
    );
    tick(&f, &mut None);
    assert!(!snapshot(&f).lines.is_empty());
    f.catalog
        .review_sessions(0)
        .delete(PROJECT, &f.session_id, now())
        .unwrap();
    assert!(snapshot(&f).lines.is_empty());
    tick(&f, &mut None);
    assert_eq!(snapshot(&f).state, "stopped");
    let completed = fixture(false, true);
    enable(&completed);
    completed
        .catalog
        .review_transcripts()
        .live_tick(
            &NoLive(FakeRuntime::new()),
            &mut None,
            &completed.catalog.jobs(),
            &|| false,
            &completed._root.path().join("heavy.lock"),
            &now().to_rfc3339(),
        )
        .unwrap();
    assert_eq!(snapshot(&completed).state, "refining");
    assert!(snapshot(&completed).final_run_id.is_some());
}
#[test]
#[ignore = "requires explicitly installed live/final models and a public JFK WAV fixture"]
fn real_live_model_latency_and_full_pass() {
    let dir = std::env::var_os("RCDVLOG_SPEECH_MODEL_DIR").expect("set model directory");
    let file = std::env::var_os("RCDVLOG_LIVE_TEST_WAV").expect("set public WAV fixture");
    let mut reader = hound::WavReader::open(file).unwrap();
    assert_eq!(reader.spec().sample_rate, 16000);
    assert_eq!(reader.spec().channels, 1);
    let pcm: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| f32::from(s.unwrap()) / 32768.0)
        .collect();
    let runtime = ModelDirSpeechRuntime {
        model_dir: PathBuf::from(dir),
    };
    let candidates: Vec<String> = std::env::var("RCDVLOG_LIVE_BENCHMARK_MODELS")
        .unwrap_or_else(|_| LIVE_REVIEW_MODEL.file_name.into())
        .split(',')
        .map(str::to_owned)
        .collect();
    let mut reports = Vec::new();
    for candidate in candidates {
        let started = std::time::Instant::now();
        let engine = rcdvlog_core::review_speech::WhisperEngine::new(
            &runtime.model_dir.join(&candidate),
            &runtime.vad_path(),
        )
        .unwrap();
        let loaded = started.elapsed().as_secs_f64();
        let mut measurements = Vec::new();
        for seconds in [4usize, 8, 11] {
            let window = &pcm[..(seconds * 16000).min(pcm.len())];
            let started = std::time::Instant::now();
            let lines = decode_live(
                &engine,
                window,
                0,
                window.len() as u64 * 3,
                RATE,
                &[],
                &|| false,
            )
            .unwrap();
            measurements.push(serde_json::json!({"audioSeconds":window.len() as f64/16000.0,"decodeSeconds":started.elapsed().as_secs_f64(),"lines":lines.len()}));
            assert!(!lines.is_empty());
            if seconds == 11 {
                assert!(
                    lines
                        .iter()
                        .any(|l| l.text.to_lowercase().contains("country")),
                    "expected speech content from {candidate}"
                );
            }
        }
        let report =
            serde_json::json!({"liveModel":candidate,"loadSeconds":loaded,"updates":measurements});
        println!("{report}");
        reports.push(report);
    }
    if std::env::var_os("RCDVLOG_LIVE_BENCHMARK_MODELS").is_none() {
        let started = std::time::Instant::now();
        let final_engine = runtime.local_engine().unwrap();
        let final_lines = decode_live(
            final_engine.as_ref(),
            &pcm,
            0,
            pcm.len() as u64 * 3,
            RATE,
            &[],
            &|| false,
        )
        .unwrap();
        let text = final_lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.to_lowercase().contains("country"));
        println!(
            "{}",
            serde_json::json!({"finalSeconds":started.elapsed().as_secs_f64(),"finalLines":final_lines.len(),"updates":reports})
        );
    }
}

#[test]
#[ignore = "requires installed live model and a public 16 kHz PCM16 WAV"]
fn real_live_upload_decode_persist_and_finalize() {
    let dir = std::env::var_os("RCDVLOG_SPEECH_MODEL_DIR").expect("model directory");
    let file = std::env::var_os("RCDVLOG_LIVE_TEST_WAV").expect("public WAV fixture");
    let runtime = ModelDirSpeechRuntime {
        model_dir: PathBuf::from(dir),
    };
    let mut reader = hound::WavReader::open(file).unwrap();
    assert_eq!(reader.spec().sample_rate, 16000);
    assert_eq!(reader.spec().channels, 1);
    let samples: Vec<i16> = reader.samples::<i16>().map(Result::unwrap).collect();
    let mut f = fixture(false, false);
    let sessions = f.catalog.review_sessions(0);
    let old = sessions.get(PROJECT, &f.session_id, now()).unwrap();
    let request = serde_json::from_value(serde_json::json!({
        "reviewTargetId": old.summary.review_target_id, "clientSessionKey": "real_live_speech",
        "consent": {"allPresentAcknowledged":true,"additionalSpeakersDisclosed":false},
        "audio":{"sampleRate":16000,"channels":1,"encoding":"pcm_s16le"},
        "capture":{"clock":"audioWorkletCurrentFrame","echoCancellation":false,"noiseSuppression":false,"autoGainControl":false},
        "clientStartedPerformanceMs":null
    })).unwrap();
    f.session_id = sessions
        .create(PROJECT, &request, &actor(), now())
        .unwrap()
        .summary
        .session_id;
    f.catalog
        .review_transcripts()
        .enable_live(PROJECT, &f.session_id, &actor(), &runtime)
        .unwrap();
    let mut engine = None;
    let mut uploaded = 0u64;
    for (sequence, part) in samples.chunks(16000 * 4).enumerate() {
        let bytes: Vec<u8> = part.iter().flat_map(|s| s.to_le_bytes()).collect();
        sessions
            .put_chunk(
                PROJECT,
                &f.session_id,
                &ChunkUpload {
                    sequence: sequence as u32,
                    start_frame: uploaded,
                    frame_count: part.len() as u32,
                    sha256: hex::encode(Sha256::digest(&bytes)),
                    capture_end_performance_ms: None,
                },
                &bytes,
                now(),
            )
            .unwrap();
        uploaded += part.len() as u64;
        f.catalog
            .review_transcripts()
            .live_tick(
                &runtime,
                &mut engine,
                &f.catalog.jobs(),
                &|| false,
                &f._root.path().join("live-real.lock"),
                &now().to_rfc3339(),
            )
            .unwrap();
    }
    let draft = f
        .catalog
        .review_transcripts()
        .live_transcript(PROJECT, &f.session_id, &runtime)
        .unwrap();
    assert!(draft.revision >= 2);
    assert!(
        draft
            .lines
            .iter()
            .any(|l| l.utterance.text.to_lowercase().contains("fellow"))
    );
    assert!(
        draft
            .lines
            .iter()
            .all(|l| l.utterance.end_frame <= uploaded)
    );
    assert_eq!(
        sessions
            .get(PROJECT, &f.session_id, now())
            .unwrap()
            .summary
            .state,
        crate::ReviewSessionState::Recording
    );
    sessions
        .finalize(
            PROJECT,
            &f.session_id,
            &FinalizeReviewSessionRequest {
                final_frame_count: uploaded,
                end_reason: RecordingEndReason::Stopped,
            },
            now(),
        )
        .unwrap();
    f.catalog
        .review_transcripts()
        .live_tick(
            &runtime,
            &mut engine,
            &f.catalog.jobs(),
            &|| false,
            &f._root.path().join("live-real.lock"),
            &now().to_rfc3339(),
        )
        .unwrap();
    let refining = f
        .catalog
        .review_transcripts()
        .live_transcript(PROJECT, &f.session_id, &runtime)
        .unwrap();
    assert_eq!(refining.state, "refining");
    assert!(refining.final_run_id.is_some());
    assert_eq!(refining.revision, draft.revision);
    println!(
        "{}",
        serde_json::json!({"model":LIVE_REVIEW_MODEL.file_name,"uploadedFrames":uploaded,"draftRevisions":draft.revision,"draftLines":draft.lines.len(),"finalQueued":true})
    );
}

#[test]
fn expired_capture_stops_live_work_and_recovery_still_schedules_final_transcription() {
    let f = fixture(false, false);
    enable(&f);
    let owner = f.catalog.review_transcripts();
    owner
        .live_tick(
            &FakeRuntime::new(),
            &mut None,
            &f.catalog.jobs(),
            &|| false,
            &f._root.path().join("heavy.lock"),
            "2099-01-01T00:00:00Z",
        )
        .unwrap();
    assert_eq!(snapshot(&f).state, "stopped");
    assert_eq!(snapshot(&f).revision, 0);
    f.catalog
        .review_sessions(0)
        .finalize(
            PROJECT,
            &f.session_id,
            &FinalizeReviewSessionRequest {
                final_frame_count: u64::from(RATE) * 20,
                end_reason: RecordingEndReason::Interrupted,
            },
            now(),
        )
        .unwrap();
    tick(&f, &mut None);
    assert_eq!(snapshot(&f).state, "refining");
    assert!(snapshot(&f).final_run_id.is_some());
}
