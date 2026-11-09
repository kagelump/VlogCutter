use super::*;
use crate::analysis::engine::SttEngine;
use crate::analysis::quality::DecodedSegment;
use crate::analysis::store::{TranscriptStatus, is_current};
use crate::analysis::windows::{PackedWindow, SpeechRegion};
use crate::{AssetPatch, ModelStatus};
use rusqlite::params;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::{TempDir, tempdir};

fn fixture() -> (TempDir, Catalog, String) {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog
        .create_project("Selected transcription fixture")
        .unwrap();
    (directory, catalog, project.id)
}

fn media_tools_available() -> bool {
    ["ffmpeg", "ffprobe"].into_iter().all(|tool| {
        crate::tool_command(tool)
            .arg("-version")
            .output()
            .is_ok_and(|output| output.status.success())
    })
}

fn add_placeholder_asset(catalog: &Catalog, project_id: &str, directory: &Path, id: &str) {
    let source = directory.join(format!("{id}.mp4"));
    fs::write(&source, format!("placeholder {id}")).unwrap();
    crate::tests::insert_asset_at(catalog, project_id, id, &source);
}

fn make_ready_asset(catalog: &Catalog, project_id: &str, directory: &Path, id: &str) {
    make_ready_asset_with_audio(catalog, project_id, directory, id, true);
}

fn make_ready_asset_with_audio(
    catalog: &Catalog,
    project_id: &str,
    directory: &Path,
    id: &str,
    with_audio: bool,
) {
    let source = directory.join(format!("{id}.mp4"));
    let color = &blake3::hash(id.as_bytes()).to_hex()[..6];
    let mut command = crate::tool_command("ffmpeg");
    command
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg(format!("color=c=0x{color}:s=160x90:r=24:d=1"));
    if with_audio {
        command
            .args(["-f", "lavfi", "-i"])
            .arg("sine=frequency=440:sample_rate=48000:duration=1")
            .args(["-map", "0:v:0", "-map", "1:a:0"]);
    } else {
        command.args(["-map", "0:v:0"]);
    }
    let status = command
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
        ])
        .args(if with_audio {
            vec!["-c:a", "aac", "-shortest"]
        } else {
            Vec::new()
        })
        .arg(&source)
        .status()
        .expect("run ffmpeg for selected transcription fixture");
    assert!(status.success(), "could not create synthetic H.264 fixture");
    crate::tests::insert_asset_at(catalog, project_id, id, &source);
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET duration_seconds=1,source_out=1,frame_rate=24,width=160,height=90,orientation_probed=1 WHERE id=?1",
            params![id],
        )
        .unwrap();
    catalog.validate_asset_video(id).unwrap();
    catalog.generate_proxy(id).unwrap();
}

#[derive(Clone)]
enum ScriptedResponse {
    NoSpeech,
    Decode(Vec<DecodedSegment>),
}

struct ScriptedEngine {
    detect_calls: Arc<AtomicUsize>,
    decode_calls: Arc<AtomicUsize>,
    script: std::cell::RefCell<std::collections::VecDeque<ScriptedResponse>>,
    pending_decode: std::cell::RefCell<Vec<DecodedSegment>>,
}

impl ScriptedEngine {
    fn new(
        detect_calls: Arc<AtomicUsize>,
        decode_calls: Arc<AtomicUsize>,
        script: Vec<ScriptedResponse>,
    ) -> Self {
        Self {
            detect_calls,
            decode_calls,
            script: std::cell::RefCell::new(script.into()),
            pending_decode: std::cell::RefCell::new(Vec::new()),
        }
    }
}

impl SttEngine for ScriptedEngine {
    fn detect_speech(&self, _pcm: &[f32]) -> Result<Vec<SpeechRegion>> {
        self.detect_calls.fetch_add(1, Ordering::SeqCst);
        match self
            .script
            .borrow_mut()
            .pop_front()
            .expect("one scripted provider response per selected asset")
        {
            ScriptedResponse::NoSpeech => Ok(Vec::new()),
            ScriptedResponse::Decode(segments) => {
                *self.pending_decode.borrow_mut() = segments;
                Ok(vec![SpeechRegion {
                    start_ms: 0,
                    end_ms: 1_000,
                }])
            }
        }
    }

    fn decode_window(&self, _pcm: &[f32], _window: &PackedWindow) -> Result<Vec<DecodedSegment>> {
        self.decode_calls.fetch_add(1, Ordering::SeqCst);
        Ok(std::mem::take(&mut *self.pending_decode.borrow_mut()))
    }
}

#[derive(Clone)]
struct CountingEngine {
    detect_calls: Arc<AtomicUsize>,
}

impl SttEngine for CountingEngine {
    fn detect_speech(&self, _pcm: &[f32]) -> Result<Vec<SpeechRegion>> {
        self.detect_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }

    fn decode_window(&self, _pcm: &[f32], _window: &PackedWindow) -> Result<Vec<DecodedSegment>> {
        unreachable!("empty VAD output must never call the decoder")
    }
}

fn selected(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_string()).collect()
}

fn selection_code(error: &anyhow::Error) -> Option<&'static str> {
    error
        .downcast_ref::<TranscriptionSelectionError>()
        .map(|error| error.code)
}

#[test]
fn invalid_selections_fail_before_any_engine_or_callback_work() {
    let (directory, catalog, project_id) = fixture();
    let other_project = catalog.create_project("Other project").unwrap();
    add_placeholder_asset(&catalog, &project_id, directory.path(), "mine");
    add_placeholder_asset(&catalog, &other_project.id, directory.path(), "not-mine");
    let opens = Arc::new(AtomicUsize::new(0));
    let callbacks = Arc::new(AtomicUsize::new(0));

    let cases: Vec<(Vec<String>, &str)> = vec![
        (Vec::new(), "empty_asset_selection"),
        (selected(&["mine", "mine"]), "duplicate_asset_id"),
        (selected(&["missing"]), "unknown_asset_id"),
        (selected(&["not-mine"]), "asset_project_mismatch"),
    ];
    for (assets, expected_code) in cases {
        let open_count = opens.clone();
        let callback_count = callbacks.clone();
        let error = catalog
            .transcribe_selected_inner(
                &project_id,
                &assets,
                &TranscriptionBatchOptions::default(),
                None::<fn() -> Result<Vec<ModelStatus>>>,
                move || {
                    open_count.fetch_add(1, Ordering::SeqCst);
                    Ok(Box::new(CountingEngine {
                        detect_calls: Arc::new(AtomicUsize::new(0)),
                    }) as Box<dyn SttEngine>)
                },
                move |_| {
                    callback_count.fetch_add(1, Ordering::SeqCst);
                    ControlFlow::Continue(())
                },
            )
            .unwrap_err();
        assert_eq!(selection_code(&error), Some(expected_code), "{error:#}");
    }
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    assert_eq!(
        callbacks.load(Ordering::SeqCst),
        0,
        "invalid selection must not emit progress"
    );
}

#[test]
fn ready_selection_opens_one_engine_and_reuses_then_forces_current_transcripts() {
    if !media_tools_available() {
        eprintln!("skipping generated selected-transcription fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let (directory, catalog, project_id) = fixture();
    for id in ["clip-b", "clip-a", "unrelated"] {
        make_ready_asset(&catalog, &project_id, directory.path(), id);
    }
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET notes='human note', title='human title', description='human description', labels_json='[\"human\"]' WHERE id='clip-a'",
            [],
        )
        .unwrap();
    let opens = Arc::new(AtomicUsize::new(0));
    let detects = Arc::new(AtomicUsize::new(0));
    let picks = selected(&["clip-b", "clip-a"]);

    let run = |force: bool| {
        let open_count = opens.clone();
        let detect_count = detects.clone();
        catalog
            .transcribe_selected_inner(
                &project_id,
                &picks,
                &TranscriptionBatchOptions { force },
                None::<fn() -> Result<Vec<ModelStatus>>>,
                move || {
                    open_count.fetch_add(1, Ordering::SeqCst);
                    Ok(Box::new(CountingEngine {
                        detect_calls: detect_count,
                    }) as Box<dyn SttEngine>)
                },
                |_| ControlFlow::Continue(()),
            )
            .unwrap()
    };

    let first = run(false);
    assert_eq!(first.transcribed, 2);
    assert_eq!(first.reused, 0);
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    assert_eq!(detects.load(Ordering::SeqCst), 2);
    assert_eq!(
        first
            .outcomes
            .iter()
            .map(|outcome| outcome.asset_id.as_str())
            .collect::<Vec<_>>(),
        vec!["clip-b", "clip-a"],
        "selection order and exact membership must be preserved"
    );
    assert!(
        catalog.asset_transcript("unrelated").unwrap().is_none(),
        "an unselected asset must not receive transcript work"
    );

    let second = run(false);
    assert_eq!(second.reused, 2);
    assert_eq!(second.transcribed, 0);
    assert_eq!(
        opens.load(Ordering::SeqCst),
        1,
        "a fully reused batch must not open the engine"
    );
    assert_eq!(detects.load(Ordering::SeqCst), 2);

    let forced = run(true);
    assert_eq!(forced.transcribed, 2);
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    assert_eq!(detects.load(Ordering::SeqCst), 4);

    let asset = catalog.get_asset("clip-a").unwrap();
    assert_eq!(asset.notes, "human note");
    assert_eq!(asset.title, "human title");
    assert_eq!(asset.description, "human description");
    assert_eq!(asset.labels, vec!["human"]);
}

#[test]
fn missing_asset_prerequisites_are_not_ready_and_missing_models_fail_explicitly() {
    if !media_tools_available() {
        eprintln!("skipping generated readiness fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let (directory, catalog, project_id) = fixture();
    // Original-only asset: it validates as video but has no proxy.
    let source = directory.path().join("no-proxy.mp4");
    let status = crate::tool_command("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=160x90:r=24:d=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success());
    crate::tests::insert_asset_at(&catalog, &project_id, "no-proxy", &source);
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET duration_seconds=1,source_out=1,frame_rate=24,width=160,height=90,orientation_probed=1 WHERE id='no-proxy'",
            [],
        )
        .unwrap();
    catalog.validate_asset_video("no-proxy").unwrap();

    let opens = Arc::new(AtomicUsize::new(0));
    let open_count = opens.clone();
    let report = catalog
        .transcribe_selected_inner(
            &project_id,
            &selected(&["no-proxy"]),
            &TranscriptionBatchOptions::default(),
            None::<fn() -> Result<Vec<ModelStatus>>>,
            move || {
                open_count.fetch_add(1, Ordering::SeqCst);
                unreachable!("not-ready work must not open the engine")
            },
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(report.not_ready, 1);
    assert_eq!(report.outcomes[0].state, TranscriptionAssetState::NotReady);
    assert_eq!(opens.load(Ordering::SeqCst), 0);

    make_ready_asset(&catalog, &project_id, directory.path(), "ready");
    let open_count = opens.clone();
    let error = catalog
        .transcribe_selected_inner(
            &project_id,
            &selected(&["ready"]),
            &TranscriptionBatchOptions::default(),
            Some(|| {
                Ok(vec![ModelStatus {
                    name: "whisper".into(),
                    role: "speech_to_text".into(),
                    installed: false,
                    ready: false,
                    present: false,
                    integrity: "missing".into(),
                    detail: Some("missing".into()),
                    bytes: 1,
                    url: String::new(),
                    directory: "/models".into(),
                    path: "/models/whisper".into(),
                    expected_blake3: None,
                }])
            }),
            move || {
                open_count.fetch_add(1, Ordering::SeqCst);
                unreachable!("model readiness must fail before engine initialization")
            },
            |_| ControlFlow::Continue(()),
        )
        .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<TranscriptionReadinessError>()
            .map(|error| error.code),
        Some("speech_models_not_ready")
    );
    assert_eq!(opens.load(Ordering::SeqCst), 0);
}

#[test]
fn cancellation_stops_after_the_current_asset_and_reports_bounded_progress() {
    if !media_tools_available() {
        eprintln!("skipping generated cancellation fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let (directory, catalog, project_id) = fixture();
    for id in ["clip-a", "clip-b", "clip-c"] {
        make_ready_asset(&catalog, &project_id, directory.path(), id);
    }
    let opens = Arc::new(AtomicUsize::new(0));
    let detects = Arc::new(AtomicUsize::new(0));
    let events = Arc::new(AtomicUsize::new(0));
    let open_count = opens.clone();
    let detect_count = detects.clone();
    let event_count = events.clone();
    let report = catalog
        .transcribe_selected_inner(
            &project_id,
            &selected(&["clip-a", "clip-b", "clip-c"]),
            &TranscriptionBatchOptions::default(),
            None::<fn() -> Result<Vec<ModelStatus>>>,
            move || {
                open_count.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(CountingEngine {
                    detect_calls: detect_count,
                }) as Box<dyn SttEngine>)
            },
            move |progress| {
                event_count.fetch_add(1, Ordering::SeqCst);
                if progress.phase == "asset" && progress.completed == 1 {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            },
        )
        .unwrap();
    assert!(report.cancelled);
    assert_eq!(report.transcribed, 1);
    assert_eq!(report.cancelled_assets, 2);
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    assert_eq!(detects.load(Ordering::SeqCst), 1);
    assert_eq!(
        events.load(Ordering::SeqCst),
        5,
        "three probe checks, start and one outcome"
    );
    assert_eq!(
        report
            .outcomes
            .iter()
            .map(|outcome| outcome.state)
            .collect::<Vec<_>>(),
        vec![
            TranscriptionAssetState::Transcribed,
            TranscriptionAssetState::Cancelled,
            TranscriptionAssetState::Cancelled
        ]
    );
    assert!(catalog.asset_transcript("clip-b").unwrap().is_none());
    assert!(catalog.asset_transcript("clip-c").unwrap().is_none());
}

#[test]
fn force_preserves_human_fields_even_when_transcript_storage_replaces_derived_rows() {
    if !media_tools_available() {
        eprintln!("skipping generated annotation fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let (directory, catalog, project_id) = fixture();
    make_ready_asset(&catalog, &project_id, directory.path(), "clip");
    let before = catalog.get_asset("clip").unwrap();
    let patch = AssetPatch {
        notes: "typed by a human".into(),
        labels: vec!["arrival".into()],
        removed: false,
        rotation_degrees: before.rotation_degrees,
        expected_revision: Some(before.revision),
    };
    catalog.update_asset("clip", &patch).unwrap();
    catalog
        .transcribe_selected_inner(
            &project_id,
            &selected(&["clip"]),
            &TranscriptionBatchOptions { force: true },
            None::<fn() -> Result<Vec<ModelStatus>>>,
            || {
                Ok(Box::new(CountingEngine {
                    detect_calls: Arc::new(AtomicUsize::new(0)),
                }) as Box<dyn SttEngine>)
            },
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    let after = catalog.get_asset("clip").unwrap();
    assert_eq!(after.notes, "typed by a human");
    assert_eq!(after.labels, vec!["arrival"]);
    assert!(after.revision > before.revision);
}

#[test]
fn no_audio_video_completes_without_engine_or_model_initialization_and_is_reused() {
    if !media_tools_available() {
        eprintln!("skipping generated no-audio fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let (directory, catalog, project) = fixture();
    make_ready_asset_with_audio(&catalog, &project, directory.path(), "no-audio", false);
    let opens = Arc::new(AtomicUsize::new(0));
    let run = || {
        let opens = opens.clone();
        catalog
            .transcribe_selected_inner(
                &project,
                &selected(&["no-audio"]),
                &TranscriptionBatchOptions::default(),
                Some(|| -> Result<Vec<ModelStatus>> {
                    panic!("a no-audio batch must not require speech models")
                }),
                move || {
                    opens.fetch_add(1, Ordering::SeqCst);
                    panic!("a no-audio batch must not initialize the engine")
                },
                |_| ControlFlow::Continue(()),
            )
            .unwrap()
    };

    let first = run();
    assert_eq!(first.transcribed, 1);
    assert_eq!(first.no_audio, 1);
    assert_eq!(first.failed, 0);
    assert_eq!(first.outcomes[0].state, TranscriptionAssetState::NoAudio);
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    let stored = catalog.asset_transcript("no-audio").unwrap().unwrap();
    assert_eq!(stored.status, TranscriptStatus::NoAudio);
    assert_eq!(
        stored.outcome_reason,
        crate::analysis::store::TranscriptOutcomeReason::NoAudioStream
    );
    assert!(!stored.source_fingerprint.is_empty());
    assert_eq!(
        stored.source_fingerprint,
        catalog.proxy_fingerprint("no-audio").unwrap()
    );
    assert_eq!(stored.provider_segment_count, 0);
    assert_eq!(stored.window_count, 0);

    let second = run();
    assert_eq!(second.reused, 1);
    assert_eq!(second.transcribed, 0);
    assert_eq!(second.no_audio, 0);
    assert_eq!(opens.load(Ordering::SeqCst), 0);

    let error = catalog
        .transcribe_asset_with_resource_overrides(
            "no-audio",
            &crate::ResourceOverrides {
                vad_threads: Some(0),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("vadThreads"));

    // The single-clip compatibility path and the whole-project path use the
    // same probe-before-engine rule. Neither may require installed models
    // merely to persist a no-audio result.
    let single = catalog.transcribe_asset("no-audio").unwrap();
    assert_eq!(single.status, TranscriptStatus::NoAudio);

    make_ready_asset_with_audio(
        &catalog,
        &project,
        directory.path(),
        "whole-no-audio",
        false,
    );
    let whole = catalog.transcribe_missing(&project).unwrap();
    assert_eq!(whole.transcribed, 1);
    assert_eq!(whole.no_audio, 1);
    assert_eq!(whole.failed, 0);
    assert_eq!(
        catalog
            .asset_transcript("whole-no-audio")
            .unwrap()
            .unwrap()
            .status,
        TranscriptStatus::NoAudio
    );
    let coverage = catalog.transcript_coverage(&project).unwrap();
    assert_eq!(coverage.total, 2);
    assert_eq!(coverage.no_audio, 2);
    assert_eq!(coverage.missing, 0);
    assert_eq!(coverage.failed, 0);
}

#[test]
fn corrupt_proxy_remains_a_processing_failure_not_silence_and_is_retryable() {
    if !media_tools_available() {
        eprintln!("skipping generated corrupt fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let (directory, catalog, project) = fixture();
    make_ready_asset(&catalog, &project, directory.path(), "corrupt");
    let proxy = catalog.get_asset("corrupt").unwrap().proxy_path.unwrap();
    std::fs::write(&proxy, b"not a media container").unwrap();
    let expected_fingerprint = catalog.proxy_fingerprint("corrupt").unwrap();

    let report = catalog
        .transcribe_selected_inner(
            &project,
            &selected(&["corrupt"]),
            &TranscriptionBatchOptions::default(),
            Some(|| -> Result<Vec<ModelStatus>> {
                panic!("probe failure must not require speech models")
            }),
            || panic!("probe failure must not initialize the engine"),
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(report.failed, 1);
    assert_eq!(report.transcribed, 0);
    assert_eq!(report.outcomes[0].state, TranscriptionAssetState::Failed);
    let stored = catalog.asset_transcript("corrupt").unwrap().unwrap();
    assert_eq!(stored.status, TranscriptStatus::Failed);
    assert_eq!(
        stored.outcome_reason,
        crate::analysis::store::TranscriptOutcomeReason::ProcessingFailed
    );
    assert_eq!(stored.source_fingerprint, expected_fingerprint);
    assert!(stored.reject_reason.contains("probe audio"));
    assert!(
        !is_current(
            &catalog.connection().unwrap(),
            "corrupt",
            &stored.source_fingerprint
        )
        .unwrap(),
        "a processing failure must remain retryable"
    );
}

#[test]
fn processing_failure_after_vad_keeps_detected_duration_and_source_fingerprint() {
    if !media_tools_available() {
        eprintln!("skipping generated decode-failure fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    struct FailsOnDecode;
    impl SttEngine for FailsOnDecode {
        fn detect_speech(&self, _pcm: &[f32]) -> Result<Vec<SpeechRegion>> {
            Ok(vec![SpeechRegion {
                start_ms: 0,
                end_ms: 1_000,
            }])
        }

        fn decode_window(
            &self,
            _pcm: &[f32],
            _window: &PackedWindow,
        ) -> Result<Vec<DecodedSegment>> {
            anyhow::bail!("synthetic decode failure")
        }
    }

    let (directory, catalog, project) = fixture();
    make_ready_asset(&catalog, &project, directory.path(), "decode-fail");
    let expected_fingerprint = catalog.proxy_fingerprint("decode-fail").unwrap();
    let report = catalog
        .transcribe_selected_inner(
            &project,
            &selected(&["decode-fail"]),
            &TranscriptionBatchOptions::default(),
            None::<fn() -> Result<Vec<ModelStatus>>>,
            || Ok(Box::new(FailsOnDecode) as Box<dyn SttEngine>),
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(report.failed, 1);
    let stored = catalog.asset_transcript("decode-fail").unwrap().unwrap();
    assert_eq!(stored.status, TranscriptStatus::Failed);
    assert_eq!(
        stored.outcome_reason,
        crate::analysis::store::TranscriptOutcomeReason::ProcessingFailed
    );
    assert!((stored.speech_seconds - 1.0).abs() < 1e-6);
    assert_eq!(stored.window_count, 1);
    assert_eq!(stored.source_fingerprint, expected_fingerprint);
    assert!(stored.reject_reason.contains("synthetic decode failure"));
}

#[test]
fn generated_audio_fixtures_distinguish_silence_empty_output_and_quality_rejections() {
    if !media_tools_available() {
        eprintln!("skipping generated speech-outcome fixtures: ffmpeg/ffprobe unavailable");
        return;
    }
    let (directory, catalog, project) = fixture();
    for id in ["silent", "empty", "repeated", "low-confidence", "accepted"] {
        make_ready_asset(&catalog, &project, directory.path(), id);
    }
    let repeat = |start_ms: u32| DecodedSegment {
        start_ms,
        end_ms: start_ms + 100,
        language: "en".into(),
        text: "same hallucinated line".into(),
        mean_token_probability: Some(0.7),
    };
    let repeated = (0..6).map(|index| repeat(index * 100)).collect();
    let script = vec![
        ScriptedResponse::NoSpeech,
        ScriptedResponse::Decode(Vec::new()),
        ScriptedResponse::Decode(repeated),
        ScriptedResponse::Decode(vec![DecodedSegment {
            start_ms: 0,
            end_ms: 900,
            language: "en".into(),
            text: "uncertain".into(),
            mean_token_probability: Some(0.1),
        }]),
        ScriptedResponse::Decode(vec![DecodedSegment {
            start_ms: 0,
            end_ms: 900,
            language: "en".into(),
            text: "accepted speech".into(),
            mean_token_probability: Some(0.9),
        }]),
    ];
    let detects = Arc::new(AtomicUsize::new(0));
    let decodes = Arc::new(AtomicUsize::new(0));
    let detect_count = detects.clone();
    let decode_count = decodes.clone();
    let report = catalog
        .transcribe_selected_inner(
            &project,
            &selected(&["silent", "empty", "repeated", "low-confidence", "accepted"]),
            &TranscriptionBatchOptions::default(),
            None::<fn() -> Result<Vec<ModelStatus>>>,
            move || {
                Ok(
                    Box::new(ScriptedEngine::new(detect_count, decode_count, script))
                        as Box<dyn SttEngine>,
                )
            },
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(report.transcribed, 5);
    assert_eq!(report.failed, 0);
    assert_eq!(detects.load(Ordering::SeqCst), 5);
    assert_eq!(decodes.load(Ordering::SeqCst), 4);

    let silent = catalog.asset_transcript("silent").unwrap().unwrap();
    assert_eq!(silent.status, TranscriptStatus::NoSpeech);
    assert_eq!(
        silent.outcome_reason,
        crate::analysis::store::TranscriptOutcomeReason::NoDetectedSpeech
    );

    let empty = catalog.asset_transcript("empty").unwrap().unwrap();
    assert_eq!(empty.status, TranscriptStatus::NoAcceptedSpeech);
    assert_eq!(
        empty.outcome_reason,
        crate::analysis::store::TranscriptOutcomeReason::EmptyProviderOutput
    );
    assert!((empty.speech_seconds - 1.0).abs() < 1e-6);
    assert_eq!(empty.window_count, 1);
    assert_eq!(empty.provider_segment_count, 0);
    assert_eq!(
        empty.source_fingerprint,
        catalog.proxy_fingerprint("empty").unwrap()
    );

    let repeated = catalog.asset_transcript("repeated").unwrap().unwrap();
    assert_eq!(repeated.status, TranscriptStatus::Rejected);
    assert_eq!(
        repeated.outcome_reason,
        crate::analysis::store::TranscriptOutcomeReason::QualityRejected
    );
    assert_eq!(repeated.reject_reason, "repetition");
    assert!(repeated.repetition_ratio.is_some_and(|ratio| ratio >= 0.3));

    let low = catalog.asset_transcript("low-confidence").unwrap().unwrap();
    assert_eq!(low.status, TranscriptStatus::Rejected);
    assert_eq!(low.reject_reason, "low_token_probability");
    assert!(low.mean_token_probability.is_some_and(|value| value < 0.2));

    let accepted = catalog.asset_transcript("accepted").unwrap().unwrap();
    assert_eq!(accepted.status, TranscriptStatus::Speech);
    assert_eq!(
        accepted.outcome_reason,
        crate::analysis::store::TranscriptOutcomeReason::Accepted
    );

    // The terminal non-failure outcomes are current and are not retried.
    let reused = catalog
        .transcribe_selected_inner(
            &project,
            &selected(&["silent", "empty", "repeated", "low-confidence", "accepted"]),
            &TranscriptionBatchOptions::default(),
            Some(|| -> Result<Vec<ModelStatus>> {
                panic!("reused outcomes must not require speech models")
            }),
            || panic!("reused outcomes must not initialize the engine"),
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(reused.reused, 5);
    assert_eq!(reused.transcribed, 0);

    let coverage = catalog.transcript_coverage(&project).unwrap();
    assert_eq!(coverage.total, 5);
    assert_eq!(coverage.speech, 1);
    assert_eq!(coverage.no_speech, 1);
    assert_eq!(coverage.no_audio, 0);
    assert_eq!(coverage.no_accepted_speech, 1);
    assert_eq!(coverage.rejected, 2);
    assert_eq!(coverage.failed, 0);
    assert_eq!(coverage.missing, 0);
}

#[test]
fn a_failed_asset_does_not_stop_the_batch_and_is_retried() {
    if !media_tools_available() {
        eprintln!("skipping generated retry fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    struct FailsOnceEngine(Arc<AtomicUsize>);
    impl SttEngine for FailsOnceEngine {
        fn detect_speech(&self, _pcm: &[f32]) -> Result<Vec<SpeechRegion>> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                anyhow::bail!("synthetic transient engine failure");
            }
            Ok(Vec::new())
        }
        fn decode_window(&self, _: &[f32], _: &PackedWindow) -> Result<Vec<DecodedSegment>> {
            unreachable!("no speech")
        }
    }
    let (directory, catalog, project) = fixture();
    for id in ["retry", "ready"] {
        make_ready_asset(&catalog, &project, directory.path(), id);
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let run = || {
        catalog
            .transcribe_selected_inner(
                &project,
                &selected(&["retry", "ready"]),
                &TranscriptionBatchOptions::default(),
                None::<fn() -> Result<Vec<ModelStatus>>>,
                || Ok(Box::new(FailsOnceEngine(calls.clone())) as Box<dyn SttEngine>),
                |_| ControlFlow::Continue(()),
            )
            .unwrap()
    };
    let first = run();
    assert_eq!((first.failed, first.transcribed, first.reused), (1, 1, 0));
    assert!(
        first.outcomes[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("transient")
    );
    assert!(
        !is_current(
            &catalog.connection().unwrap(),
            "retry",
            &catalog.proxy_fingerprint("retry").unwrap()
        )
        .unwrap()
    );
    // Also recover failed rows written by a caller that kept the current
    // fingerprint: recipe identity alone must not make failure reusable.
    let mut stored = catalog.asset_transcript("retry").unwrap().unwrap();
    stored.source_fingerprint = catalog.proxy_fingerprint("retry").unwrap();
    save_transcript(&catalog.connection().unwrap(), &stored).unwrap();
    let second = run();
    assert_eq!(
        (second.failed, second.transcribed, second.reused),
        (0, 1, 1)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        catalog.asset_transcript("retry").unwrap().unwrap().status,
        TranscriptStatus::NoSpeech
    );
}

#[test]
fn a_speech_claim_prevents_a_selected_batch_from_overwriting_native_work() {
    if !media_tools_available() {
        eprintln!("skipping generated claim fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let (directory, catalog, project) = fixture();
    make_ready_asset(&catalog, &project, directory.path(), "claimed");
    let claim = catalog
        .claim_representation("claimed", RepresentationKind::Speech)
        .unwrap();
    let report = catalog
        .transcribe_selected_inner(
            &project,
            &selected(&["claimed"]),
            &TranscriptionBatchOptions::default(),
            None::<fn() -> Result<Vec<ModelStatus>>>,
            || panic!("a busy speech claim must not open the local engine"),
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(report.outcomes[0].state, TranscriptionAssetState::NotReady);
    assert!(
        report.outcomes[0]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("already in progress"))
    );
    assert!(catalog.asset_transcript("claimed").unwrap().is_none());
    drop(claim);
    assert!(
        catalog
            .try_claim_representation("claimed", RepresentationKind::Speech)
            .unwrap()
            .is_some()
    );
}

#[test]
fn cancelling_at_start_does_not_initialize_the_engine_or_write_transcripts() {
    if !media_tools_available() {
        eprintln!("skipping generated start-cancel fixture: ffmpeg/ffprobe unavailable");
        return;
    }
    let (directory, catalog, project) = fixture();
    make_ready_asset(&catalog, &project, directory.path(), "ready");
    let report = catalog
        .transcribe_selected_inner(
            &project,
            &selected(&["ready"]),
            &TranscriptionBatchOptions::default(),
            None::<fn() -> Result<Vec<ModelStatus>>>,
            || panic!("cancelled batch must not initialize the engine"),
            |_| ControlFlow::Break(()),
        )
        .unwrap();
    assert!(report.cancelled);
    assert_eq!(report.cancelled_assets, 1);
    assert_eq!(report.outcomes[0].state, TranscriptionAssetState::Cancelled);
    assert!(catalog.asset_transcript("ready").unwrap().is_none());
}
