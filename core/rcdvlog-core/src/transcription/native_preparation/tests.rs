use super::*;
use crate::analysis::engine::legacy_model_dir;
use crate::analysis::openrouter_stt::{
    OPENROUTER_STT_PROVIDER, OPENROUTER_WHISPER_MODEL, SttDataCollection,
};
use crate::analysis::speech_audio::{
    MAX_SPEECH_WAV_BYTES, SPEECH_BITS_PER_SAMPLE, SPEECH_CHANNELS, SPEECH_SAMPLE_RATE_HZ,
};
use crate::{NativeSttProviderPolicy, RepresentationBusy, RunPolicyRequest};
use hound::{WavSpec, WavWriter};
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tempfile::TempDir;

struct Fixture {
    _directory: TempDir,
    catalog: Catalog,
    project_id: String,
    asset_id: String,
    proxy_path: PathBuf,
    policy_id: String,
}

fn media_tools_available() -> bool {
    ["ffmpeg", "ffprobe"].into_iter().all(|tool| {
        crate::tool_command(tool)
            .arg("-version")
            .output()
            .is_ok_and(|output| output.status.success())
    })
}

fn fixture(with_audio: bool, duration_seconds: u32) -> Option<Fixture> {
    if !media_tools_available() {
        eprintln!("skipping native speech fixture: ffmpeg/ffprobe unavailable");
        return None;
    }
    let directory = TempDir::new().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Native speech fixture").unwrap();
    let asset_id = "native-asset";
    let source = directory.path().join("source.mp4");
    let mut command = crate::tool_command("ffmpeg");
    command.args([
        "-y",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
    ]);
    command.arg(format!("color=c=black:s=160x90:r=2:d={duration_seconds}"));
    if with_audio {
        command
            .args(["-f", "lavfi", "-i"])
            .arg(format!(
                "sine=frequency=440:sample_rate=16000:duration={duration_seconds}"
            ))
            .args(["-map", "0:v:0", "-map", "1:a:0", "-c:a", "aac", "-shortest"]);
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
        .arg(&source)
        .status()
        .expect("generate synthetic native speech fixture");
    assert!(status.success(), "synthetic native speech fixture failed");
    let proxy_path = directory.path().join("native-asset.proxy.v3.mp4");
    fs::copy(&source, &proxy_path).unwrap();
    crate::tests::insert_asset_at(&catalog, &project.id, asset_id, &source);
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET duration_seconds=?2,source_out=?2,frame_rate=2,width=160,height=90,orientation_probed=1,proxy_path=?3 WHERE id=?1",
            rusqlite::params![asset_id, f64::from(duration_seconds), proxy_path.to_string_lossy()],
        )
        .unwrap();
    catalog.validate_asset_video(asset_id).unwrap();
    let model_directory = legacy_model_dir(catalog.database_path()).unwrap();
    fs::create_dir_all(&model_directory).unwrap();
    fs::write(model_directory.join(VAD_MODEL.file_name), b"fake VAD bytes").unwrap();
    let request = RunPolicyRequest::native_speech_transcription(
        project.id.clone(),
        vec![asset_id.to_string()],
        OPENROUTER_STT_PROVIDER,
        OPENROUTER_WHISPER_MODEL,
        1.0,
        Some("en".into()),
        NativeSttProviderPolicy::new(true, SttDataCollection::Deny),
        false,
    );
    let policy_id = catalog
        .authorize_run_policy(&request)
        .unwrap()
        .policy
        .policy_id;
    Some(Fixture {
        _directory: directory,
        catalog,
        project_id: project.id,
        asset_id: asset_id.to_string(),
        proxy_path,
        policy_id,
    })
}

fn write_pcm_wav(path: &Path, samples: &[f32]) -> Result<()> {
    let mut writer = WavWriter::create(
        path,
        WavSpec {
            channels: SPEECH_CHANNELS,
            sample_rate: SPEECH_SAMPLE_RATE_HZ,
            bits_per_sample: SPEECH_BITS_PER_SAMPLE,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .context("create fake extracted WAV")?;
    for sample in samples {
        writer
            .write_sample((sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16)
            .context("write fake extracted sample")?;
    }
    writer.finalize().context("finalize fake extracted WAV")
}

fn pcm(seconds: u32) -> Vec<f32> {
    (0..seconds as usize * SPEECH_SAMPLE_RATE_HZ as usize)
        .map(|index| ((index % 128) as f32 / 128.0) - 0.5)
        .collect()
}

struct FakeDetector {
    calls: Arc<AtomicUsize>,
    regions: Vec<SpeechRegion>,
    error: Option<String>,
    on_detect: Option<Box<dyn FnMut()>>,
}

impl FakeDetector {
    fn regions(regions: Vec<SpeechRegion>) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            regions,
            error: None,
            on_detect: None,
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            regions: Vec::new(),
            error: Some(message.into()),
            on_detect: None,
        }
    }

    fn with_on_detect(mut self, action: impl FnMut() + 'static) -> Self {
        self.on_detect = Some(Box::new(action));
        self
    }
}

impl SpeechDetector for FakeDetector {
    fn detect_speech(&mut self, _pcm: &[f32]) -> Result<Vec<SpeechRegion>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(action) = self.on_detect.as_mut() {
            action();
        }
        if let Some(error) = &self.error {
            bail!("{error}");
        }
        Ok(self.regions.clone())
    }
}

fn preparer_with_fake_media(
    audio_present: bool,
    samples: Vec<f32>,
    detector: Box<dyn SpeechDetector>,
) -> NativeSpeechPreparer {
    let mut preparer = NativeSpeechPreparer::new();
    preparer.probe_hook = Some(Box::new(move |_| Ok(audio_present)));
    preparer.extract_hook = Some(Box::new(move |_source, destination, _cancel| {
        write_pcm_wav(destination, &samples)
    }));
    inject_detector(&mut preparer, detector);
    preparer
}

fn inject_detector(preparer: &mut NativeSpeechPreparer, detector: Box<dyn SpeechDetector>) {
    let mut detector = Some(detector);
    preparer.loader = Box::new(move |_, _| Ok(detector.take().expect("one fake detector load")));
}

fn assert_speech_and_proxy_claims_released(catalog: &Catalog, asset_id: &str) {
    let proxy = catalog
        .try_claim_representation(asset_id, RepresentationKind::Proxy)
        .unwrap();
    assert!(proxy.is_some(), "proxy claim leaked");
    drop(proxy);
    let speech = catalog
        .try_claim_representation(asset_id, RepresentationKind::Speech)
        .unwrap();
    assert!(speech.is_some(), "speech claim leaked");
    drop(speech);
}

fn assert_scratch_cleaned(catalog: &Catalog) {
    let audio_dir = catalog
        .database_path()
        .parent()
        .unwrap()
        .join("cache")
        .join("audio");
    let leftovers = fs::read_dir(&audio_dir)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(leftovers, 0, "native speech scratch files leaked");
}

#[test]
fn initial_cancellation_short_circuits_before_source_auth_or_claim_work() {
    let directory = TempDir::new().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let mut preparer = NativeSpeechPreparer::new();
    let error = preparer
        .prepare(
            &catalog,
            "not-an-authorized-policy",
            "not-an-asset",
            &ResourceOverrides::default(),
            &mut || true,
        )
        .unwrap_err();
    assert!(error.is::<NativeSpeechPreparationCanceled>(), "{error:#}");
    assert!(
        !directory
            .path()
            .join("cache/.representation-locks")
            .exists(),
        "cancellation must not create claim files"
    );
}

#[test]
fn no_audio_is_terminal_without_model_extraction_or_heavy_work() {
    let Some(fixture) = fixture(false, 1) else {
        return;
    };
    let heavy = fixture.catalog.acquire_local_heavy().unwrap();
    let mut preparer = NativeSpeechPreparer::new();
    preparer.extract_hook = Some(Box::new(|_, _, _| -> Result<()> {
        panic!("no-audio preparation must not extract PCM")
    }));
    preparer.loader = Box::new(|_, _| -> Result<Box<dyn SpeechDetector>> {
        panic!("no-audio preparation must not load VAD")
    });
    let prepared = preparer
        .prepare(
            &fixture.catalog,
            &fixture.policy_id,
            &fixture.asset_id,
            &ResourceOverrides::default(),
            &mut || false,
        )
        .unwrap();
    assert_eq!(prepared.outcome(), NativeSpeechLocalOutcome::NoAudio);
    assert!(prepared.evidence().vad.is_none());
    assert_eq!(prepared.window_count(), 0);
    assert_eq!(prepared.speech_duration_ms(), 0);
    assert!(
        fixture
            .catalog
            .try_claim_representation(&fixture.asset_id, RepresentationKind::Proxy)
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .catalog
            .try_claim_representation(&fixture.asset_id, RepresentationKind::Speech)
            .unwrap()
            .is_none()
    );
    drop(prepared);
    drop(heavy);
    assert_speech_and_proxy_claims_released(&fixture.catalog, &fixture.asset_id);
    assert_scratch_cleaned(&fixture.catalog);
}

#[test]
fn invalid_resource_overrides_fail_even_for_a_no_audio_asset_before_probe() {
    let Some(fixture) = fixture(false, 1) else {
        return;
    };
    let mut preparer = NativeSpeechPreparer::new();
    preparer.probe_hook = Some(Box::new(|_| {
        panic!("invalid settings must fail before probing")
    }));
    for overrides in [
        ResourceOverrides {
            vad_threads: Some(0),
            ..Default::default()
        },
        ResourceOverrides {
            decode_threads: Some(0),
            ..Default::default()
        },
        ResourceOverrides {
            worker_count: Some(2),
            ..Default::default()
        },
        ResourceOverrides {
            encoder: Some("software".into()),
            ..Default::default()
        },
    ] {
        let error = preparer
            .prepare(
                &fixture.catalog,
                &fixture.policy_id,
                &fixture.asset_id,
                &overrides,
                &mut || false,
            )
            .unwrap_err();
        assert!(format!("{error:#}").contains("invalid input"), "{error:#}");
    }
    assert!(
        fixture
            .catalog
            .asset_transcript(&fixture.asset_id)
            .unwrap()
            .is_none()
    );
    assert_speech_and_proxy_claims_released(&fixture.catalog, &fixture.asset_id);
    assert_scratch_cleaned(&fixture.catalog);
}

#[test]
fn real_ffmpeg_extraction_feeds_the_standalone_vad_path() {
    let Some(fixture) = fixture(true, 2) else {
        return;
    };
    let mut preparer = NativeSpeechPreparer::new();
    inject_detector(&mut preparer, Box::new(FakeDetector::regions(Vec::new())));
    let prepared = preparer
        .prepare(
            &fixture.catalog,
            &fixture.policy_id,
            &fixture.asset_id,
            &ResourceOverrides::default(),
            &mut || false,
        )
        .unwrap();
    assert_eq!(prepared.outcome(), NativeSpeechLocalOutcome::NoSpeech);
    assert!(prepared.evidence().source_duration_ms > 0);
    assert!(prepared.evidence().vad.is_some());
    drop(prepared);
    assert_speech_and_proxy_claims_released(&fixture.catalog, &fixture.asset_id);
    assert_scratch_cleaned(&fixture.catalog);
}

#[test]
fn silence_is_a_successful_zero_window_vad_outcome_with_claims_held() {
    let Some(fixture) = fixture(true, 2) else {
        return;
    };
    let mut preparer =
        preparer_with_fake_media(true, pcm(1), Box::new(FakeDetector::regions(Vec::new())));
    let prepared = preparer
        .prepare(
            &fixture.catalog,
            &fixture.policy_id,
            &fixture.asset_id,
            &ResourceOverrides::default(),
            &mut || false,
        )
        .unwrap();
    assert_eq!(prepared.outcome(), NativeSpeechLocalOutcome::NoSpeech);
    assert_eq!(prepared.window_count(), 0);
    assert!(prepared.evidence().vad.is_some());
    assert_eq!(prepared.evidence().provider, OPENROUTER_STT_PROVIDER);
    assert_eq!(prepared.evidence().model, OPENROUTER_WHISPER_MODEL);
    assert_eq!(prepared.evidence().scope.asset_id, fixture.asset_id);
    assert_eq!(prepared.evidence().project_id, fixture.project_id);
    assert!(!prepared.evidence().proxy_full_fingerprint.is_empty());
    assert!(
        fixture.catalog.try_acquire_local_heavy().unwrap().is_some(),
        "the local-heavy lease must be released before returning"
    );
    assert!(
        fixture
            .catalog
            .try_claim_representation(&fixture.asset_id, RepresentationKind::Speech)
            .unwrap()
            .is_none()
    );
    drop(prepared);
    assert_speech_and_proxy_claims_released(&fixture.catalog, &fixture.asset_id);
    assert_scratch_cleaned(&fixture.catalog);
}

#[test]
fn speech_prepares_lazy_multi_window_maps_using_the_strict_native_packer() {
    let Some(fixture) = fixture(true, 33) else {
        return;
    };
    let mut preparer = preparer_with_fake_media(
        true,
        pcm(32),
        Box::new(FakeDetector::regions(vec![SpeechRegion {
            start_ms: 0,
            end_ms: 32_000,
        }])),
    );
    let prepared = preparer
        .prepare(
            &fixture.catalog,
            &fixture.policy_id,
            &fixture.asset_id,
            &ResourceOverrides::default(),
            &mut || false,
        )
        .unwrap();
    assert_eq!(prepared.outcome(), NativeSpeechLocalOutcome::PreparedSpeech);
    assert_eq!(prepared.window_count(), 2);
    assert_eq!(prepared.region_count(), 2);
    assert_eq!(prepared.speech_duration_ms(), 32_000);
    assert_eq!(prepared.windows()[0].speech_ms(), 30_000);
    let NativeSpeechMedia::Speech {
        pcm: stored_pcm, ..
    } = &prepared.media
    else {
        panic!("expected prepared speech media");
    };
    let expected = NativeSttAudioWindow::from_pcm(stored_pcm, &prepared.windows()[0]).unwrap();
    let actual = prepared.materialize_window(0).unwrap();
    assert_eq!(actual.wav_bytes(), expected.wav_bytes());
    assert_eq!(actual.sample_count(), 30_000 * 16);
    assert!(actual.wav_bytes().len() <= MAX_SPEECH_WAV_BYTES);
    let authorization = fixture
        .catalog
        .load_run_authorization(&fixture.policy_id)
        .unwrap();
    assert_eq!(
        prepared.evidence().recipe,
        authorization.policy.recipe_version
    );
    assert_eq!(
        Some(&prepared.evidence().scope),
        authorization.policy.scope.first()
    );
    assert!(
        fixture.catalog.try_acquire_local_heavy().unwrap().is_some(),
        "the local-heavy lease must not cover future transport work"
    );
    assert!(
        fixture
            .catalog
            .begin_proxy_write(&fixture.asset_id)
            .unwrap()
            .is_none(),
        "native preparation must keep proxy regeneration excluded"
    );
    let error = fixture
        .catalog
        .transcribe_asset_with_resource_overrides(&fixture.asset_id, &ResourceOverrides::default())
        .unwrap_err();
    assert!(error.is::<RepresentationBusy>(), "{error:#}");
    assert!(
        fixture
            .catalog
            .asset_transcript(&fixture.asset_id)
            .unwrap()
            .is_none(),
        "a blocked local path must not overwrite native work"
    );

    fs::write(&fixture.proxy_path, b"source changed after preparation").unwrap();
    assert!(
        prepared.validate_source_current(&fixture.catalog).is_err(),
        "a changed proxy must invalidate prepared source identity"
    );
    drop(prepared);
    assert_speech_and_proxy_claims_released(&fixture.catalog, &fixture.asset_id);
    assert_scratch_cleaned(&fixture.catalog);
}

#[test]
fn invalid_vad_ranges_are_errors_and_release_claims() {
    let Some(fixture) = fixture(true, 3) else {
        return;
    };
    let mut preparer = preparer_with_fake_media(
        true,
        pcm(2),
        Box::new(FakeDetector::regions(vec![
            SpeechRegion {
                start_ms: 0,
                end_ms: 2_000,
            },
            SpeechRegion {
                start_ms: 1_000,
                end_ms: 3_000,
            },
        ])),
    );
    let error = preparer
        .prepare(
            &fixture.catalog,
            &fixture.policy_id,
            &fixture.asset_id,
            &ResourceOverrides::default(),
            &mut || false,
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("invalid or overlapping VAD"),
        "{error:#}"
    );
    assert_speech_and_proxy_claims_released(&fixture.catalog, &fixture.asset_id);
    assert_scratch_cleaned(&fixture.catalog);
}

#[test]
fn audio_beyond_the_selected_video_duration_cannot_become_a_speech_window() {
    let Some(fixture) = fixture(true, 2) else {
        return;
    };
    let mut preparer = preparer_with_fake_media(
        true,
        pcm(3),
        Box::new(FakeDetector::regions(vec![SpeechRegion {
            start_ms: 0,
            end_ms: 3000,
        }])),
    );
    let error = preparer
        .prepare(
            &fixture.catalog,
            &fixture.policy_id,
            &fixture.asset_id,
            &ResourceOverrides::default(),
            &mut || false,
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("selected source duration"),
        "{error:#}"
    );
    assert_speech_and_proxy_claims_released(&fixture.catalog, &fixture.asset_id);
    assert_scratch_cleaned(&fixture.catalog);
}

#[test]
fn corrupt_vad_is_an_error_not_silence_and_releases_claims() {
    let Some(fixture) = fixture(true, 2) else {
        return;
    };
    let mut preparer = preparer_with_fake_media(
        true,
        pcm(1),
        Box::new(FakeDetector::error("synthetic VAD corruption")),
    );
    let error = preparer
        .prepare(
            &fixture.catalog,
            &fixture.policy_id,
            &fixture.asset_id,
            &ResourceOverrides::default(),
            &mut || false,
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("synthetic VAD corruption"),
        "{error:#}"
    );
    assert_speech_and_proxy_claims_released(&fixture.catalog, &fixture.asset_id);
    assert_scratch_cleaned(&fixture.catalog);
}

#[test]
fn corrupt_or_empty_extraction_is_an_error_not_silence() {
    let Some(fixture) = fixture(true, 2) else {
        return;
    };
    let detector_calls = Arc::new(AtomicUsize::new(0));
    let mut corrupt = NativeSpeechPreparer::new();
    corrupt.probe_hook = Some(Box::new(|_| Ok(true)));
    corrupt.extract_hook = Some(Box::new(|_, destination, _| {
        fs::write(destination, b"not a WAV file").context("write corrupt extraction")
    }));
    let detector = FakeDetector::regions(Vec::new());
    inject_detector(
        &mut corrupt,
        Box::new(FakeDetector {
            calls: Arc::clone(&detector_calls),
            ..detector
        }),
    );
    let error = corrupt
        .prepare(
            &fixture.catalog,
            &fixture.policy_id,
            &fixture.asset_id,
            &ResourceOverrides::default(),
            &mut || false,
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("open extracted speech audio"),
        "{error:#}"
    );
    assert_eq!(detector_calls.load(Ordering::SeqCst), 0);
    assert_speech_and_proxy_claims_released(&fixture.catalog, &fixture.asset_id);
    assert_scratch_cleaned(&fixture.catalog);

    let detector_calls = Arc::new(AtomicUsize::new(0));
    let mut empty = NativeSpeechPreparer::new();
    empty.probe_hook = Some(Box::new(|_| Ok(true)));
    empty.extract_hook = Some(Box::new(|_, destination, _| {
        write_pcm_wav(destination, &[])
    }));
    inject_detector(
        &mut empty,
        Box::new(FakeDetector {
            calls: Arc::clone(&detector_calls),
            ..FakeDetector::regions(Vec::new())
        }),
    );
    let error = empty
        .prepare(
            &fixture.catalog,
            &fixture.policy_id,
            &fixture.asset_id,
            &ResourceOverrides::default(),
            &mut || false,
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("no PCM samples"), "{error:#}");
    assert_eq!(detector_calls.load(Ordering::SeqCst), 0);
    assert_speech_and_proxy_claims_released(&fixture.catalog, &fixture.asset_id);
    assert_scratch_cleaned(&fixture.catalog);
}

#[test]
fn corrupt_probe_is_an_error_not_no_audio_and_releases_claims() {
    let Some(fixture) = fixture(true, 2) else {
        return;
    };
    let mut preparer = NativeSpeechPreparer::new();
    preparer.probe_hook = Some(Box::new(|_| bail!("synthetic corrupt probe")));
    preparer.extract_hook = Some(Box::new(|_, _, _| -> Result<()> {
        panic!("a failed probe must not extract PCM")
    }));
    inject_detector(&mut preparer, Box::new(FakeDetector::regions(Vec::new())));
    let error = preparer
        .prepare(
            &fixture.catalog,
            &fixture.policy_id,
            &fixture.asset_id,
            &ResourceOverrides::default(),
            &mut || false,
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("synthetic corrupt probe"),
        "{error:#}"
    );
    assert_speech_and_proxy_claims_released(&fixture.catalog, &fixture.asset_id);
    assert_scratch_cleaned(&fixture.catalog);
}

#[test]
fn cancellation_during_extraction_and_vad_removes_scratch_and_releases_claims() {
    let Some(fixture) = fixture(true, 2) else {
        return;
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let extract_cancel = Arc::clone(&cancel);
    let mut extraction = NativeSpeechPreparer::new();
    extraction.probe_hook = Some(Box::new(|_| Ok(true)));
    extraction.extract_hook = Some(Box::new(
        move |_, destination, _cancel: &mut dyn FnMut() -> bool| {
            write_pcm_wav(destination, &pcm(1))?;
            extract_cancel.store(true, Ordering::SeqCst);
            Ok(())
        },
    ));
    inject_detector(&mut extraction, Box::new(FakeDetector::regions(Vec::new())));
    let error = extraction
        .prepare(
            &fixture.catalog,
            &fixture.policy_id,
            &fixture.asset_id,
            &ResourceOverrides::default(),
            &mut || cancel.load(Ordering::SeqCst),
        )
        .unwrap_err();
    assert!(error.is::<NativeSpeechPreparationCanceled>(), "{error:#}");
    assert_speech_and_proxy_claims_released(&fixture.catalog, &fixture.asset_id);
    assert_scratch_cleaned(&fixture.catalog);

    let cancel = Arc::new(AtomicBool::new(false));
    let vad_cancel = Arc::clone(&cancel);
    let mut vad = preparer_with_fake_media(
        true,
        pcm(1),
        Box::new(
            FakeDetector::regions(vec![SpeechRegion {
                start_ms: 0,
                end_ms: 1_000,
            }])
            .with_on_detect(move || vad_cancel.store(true, Ordering::SeqCst)),
        ),
    );
    let error = vad
        .prepare(
            &fixture.catalog,
            &fixture.policy_id,
            &fixture.asset_id,
            &ResourceOverrides::default(),
            &mut || cancel.load(Ordering::SeqCst),
        )
        .unwrap_err();
    assert!(error.is::<NativeSpeechPreparationCanceled>(), "{error:#}");
    assert_speech_and_proxy_claims_released(&fixture.catalog, &fixture.asset_id);
    assert_scratch_cleaned(&fixture.catalog);
}

#[test]
fn source_change_during_vad_invalidates_before_the_guard_is_exposed() {
    let Some(fixture) = fixture(true, 2) else {
        return;
    };
    let proxy = fixture.proxy_path.clone();
    let mut preparer = preparer_with_fake_media(
        true,
        pcm(1),
        Box::new(
            FakeDetector::regions(vec![SpeechRegion {
                start_ms: 0,
                end_ms: 1_000,
            }])
            .with_on_detect(move || fs::write(&proxy, b"changed during VAD").unwrap()),
        ),
    );
    let error = preparer
        .prepare(
            &fixture.catalog,
            &fixture.policy_id,
            &fixture.asset_id,
            &ResourceOverrides::default(),
            &mut || false,
        )
        .unwrap_err();
    assert!(
        !format!("{error:#}").is_empty(),
        "changed source must fail, not return silence"
    );
    assert_speech_and_proxy_claims_released(&fixture.catalog, &fixture.asset_id);
    assert_scratch_cleaned(&fixture.catalog);
}

#[test]
fn changed_source_scope_fails_before_claims_or_extraction() {
    let Some(fixture) = fixture(true, 2) else {
        return;
    };
    fs::write(&fixture.proxy_path, b"changed before preparation").unwrap();
    let extract_calls = Arc::new(AtomicUsize::new(0));
    let extract_counter = Arc::clone(&extract_calls);
    let mut preparer = NativeSpeechPreparer::new();
    preparer.probe_hook = Some(Box::new(|_| Ok(true)));
    preparer.extract_hook = Some(Box::new(move |_, _, _| {
        extract_counter.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }));
    inject_detector(&mut preparer, Box::new(FakeDetector::regions(Vec::new())));
    let error = preparer
        .prepare(
            &fixture.catalog,
            &fixture.policy_id,
            &fixture.asset_id,
            &ResourceOverrides::default(),
            &mut || false,
        )
        .unwrap_err();
    assert!(!format!("{error:#}").is_empty());
    assert_eq!(extract_calls.load(Ordering::SeqCst), 0);
    assert_speech_and_proxy_claims_released(&fixture.catalog, &fixture.asset_id);
}

#[test]
fn preparation_selects_the_requested_entry_from_a_multi_asset_policy() {
    let Some(fixture) = fixture(true, 2) else {
        return;
    };
    let other_source = fixture._directory.path().join("other.mp4");
    let other_proxy = fixture._directory.path().join("other.proxy.v3.mp4");
    let mut other_bytes = fs::read(&fixture.proxy_path).unwrap();
    other_bytes.extend_from_slice(b"different original identity");
    fs::write(&other_source, other_bytes).unwrap();
    fs::copy(&fixture.proxy_path, &other_proxy).unwrap();
    crate::tests::insert_asset_at(
        &fixture.catalog,
        &fixture.project_id,
        "other",
        &other_source,
    );
    fixture.catalog.connection().unwrap().execute(
        "UPDATE assets SET duration_seconds=2,source_out=2,frame_rate=2,width=160,height=90,orientation_probed=1,proxy_path=?2 WHERE id=?1",
        rusqlite::params!["other", other_proxy.to_string_lossy()],
    ).unwrap();
    fixture.catalog.validate_asset_video("other").unwrap();
    let request = RunPolicyRequest::native_speech_transcription(
        fixture.project_id.clone(),
        vec!["other".into(), fixture.asset_id.clone()],
        OPENROUTER_STT_PROVIDER,
        OPENROUTER_WHISPER_MODEL,
        1.0,
        Some("en".into()),
        NativeSttProviderPolicy::new(true, SttDataCollection::Deny),
        false,
    );
    let policy = fixture
        .catalog
        .authorize_run_policy(&request)
        .unwrap()
        .policy;
    assert_eq!(policy.scope.len(), 2);
    let mut preparer =
        preparer_with_fake_media(true, pcm(1), Box::new(FakeDetector::regions(Vec::new())));
    let prepared = preparer
        .prepare(
            &fixture.catalog,
            &policy.policy_id,
            &fixture.asset_id,
            &ResourceOverrides::default(),
            &mut || false,
        )
        .unwrap();
    assert_eq!(prepared.evidence().scope.asset_id, fixture.asset_id);
    prepared.validate_source_current(&fixture.catalog).unwrap();
    drop(prepared);
    assert_speech_and_proxy_claims_released(&fixture.catalog, &fixture.asset_id);
}

#[test]
fn a_changed_vad_thread_override_reloads_the_context_before_reporting_it() {
    let Some(fixture) = fixture(true, 2) else {
        return;
    };
    let loads = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = Arc::clone(&loads);
    let mut preparer = NativeSpeechPreparer::new();
    preparer.probe_hook = Some(Box::new(|_| Ok(true)));
    preparer.extract_hook = Some(Box::new(|_, destination, _| {
        write_pcm_wav(destination, &pcm(1))
    }));
    preparer.loader = Box::new(move |path, threads| {
        assert_eq!(path.file_name().unwrap(), VAD_MODEL.file_name);
        recorded.lock().unwrap().push(threads);
        Ok(Box::new(FakeDetector::regions(Vec::new())))
    });
    for threads in [4, 4, 2, 2] {
        let overrides = ResourceOverrides {
            vad_threads: Some(threads),
            ..Default::default()
        };
        let prepared = preparer
            .prepare(
                &fixture.catalog,
                &fixture.policy_id,
                &fixture.asset_id,
                &overrides,
                &mut || false,
            )
            .unwrap();
        assert_eq!(prepared.evidence().vad.as_ref().unwrap().threads, threads);
        drop(prepared);
    }
    assert_eq!(*loads.lock().unwrap(), vec![4, 2]);
    assert_speech_and_proxy_claims_released(&fixture.catalog, &fixture.asset_id);
    assert_scratch_cleaned(&fixture.catalog);
}

#[test]
fn a_canceled_extractor_error_keeps_the_typed_cancellation_and_cleans_scratch() {
    let Some(fixture) = fixture(true, 2) else {
        return;
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let trigger = Arc::clone(&cancel);
    let mut preparer = NativeSpeechPreparer::new();
    preparer.probe_hook = Some(Box::new(|_| Ok(true)));
    preparer.extract_hook = Some(Box::new(move |_, destination, _| {
        write_pcm_wav(destination, &pcm(1))?;
        trigger.store(true, Ordering::SeqCst);
        bail!("media tool process canceled");
    }));
    let error = preparer
        .prepare(
            &fixture.catalog,
            &fixture.policy_id,
            &fixture.asset_id,
            &ResourceOverrides::default(),
            &mut || cancel.load(Ordering::SeqCst),
        )
        .unwrap_err();
    assert!(error.is::<NativeSpeechPreparationCanceled>(), "{error:#}");
    assert_speech_and_proxy_claims_released(&fixture.catalog, &fixture.asset_id);
    assert_scratch_cleaned(&fixture.catalog);
}

#[test]
fn standalone_vad_is_loaded_lazily_and_reused_for_a_sequential_batch() {
    let Some(fixture) = fixture(true, 2) else {
        return;
    };
    let model_directory = legacy_model_dir(fixture.catalog.database_path()).unwrap();
    fs::create_dir_all(&model_directory).unwrap();
    fs::write(model_directory.join(VAD_MODEL.file_name), b"fake VAD bytes").unwrap();
    let loads = Arc::new(AtomicUsize::new(0));
    let loader_loads = Arc::clone(&loads);
    let mut preparer = NativeSpeechPreparer::new();
    preparer.probe_hook = Some(Box::new(|_| Ok(true)));
    preparer.extract_hook = Some(Box::new(|_, destination, _| {
        write_pcm_wav(destination, &pcm(1))
    }));
    preparer.loader = Box::new(move |_path, _threads| {
        loader_loads.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeDetector::regions(Vec::new())) as Box<dyn SpeechDetector>)
    });

    for _ in 0..2 {
        let prepared = preparer
            .prepare(
                &fixture.catalog,
                &fixture.policy_id,
                &fixture.asset_id,
                &ResourceOverrides::default(),
                &mut || false,
            )
            .unwrap();
        assert_eq!(prepared.outcome(), NativeSpeechLocalOutcome::NoSpeech);
        drop(prepared);
    }
    assert_eq!(
        loads.load(Ordering::SeqCst),
        1,
        "one standalone VAD context should be reused across sequential preparations"
    );
    assert_speech_and_proxy_claims_released(&fixture.catalog, &fixture.asset_id);
    assert_scratch_cleaned(&fixture.catalog);
}
