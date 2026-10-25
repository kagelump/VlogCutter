//! C8 native capability-pilot runtime tests.
//!
//! All media is generated locally with FFmpeg. VAD detection and provider I/O
//! are narrow injected seams; the real local preparation, SQLite accounting
//! boundary and pilot projection owners are exercised end to end.

use super::*;
use crate::analysis::accounting::{AccountedNativeSttProvider, AnalysisRequestLifecycle};
use crate::analysis::engine::{VAD_MODEL, legacy_model_dir};
use crate::analysis::openrouter_stt::{
    NativeSttAudioWindow, NativeSttCallResult, NativeSttProviderPolicy, NativeSttTransport,
    OPENROUTER_STT_PROVIDER, OPENROUTER_WHISPER_MODEL, SttDataCollection, decode_native_stt_body,
};
use crate::analysis::silero_vad::SpeechDetector;
use crate::analysis::windows::SpeechRegion;
use crate::transcription::NativeSpeechPreparer;
use crate::{
    NativePilotLocalOutcome, NativePilotSampleOutcome, RepresentationKind, RunPolicyRequest,
};
use anyhow::{Context, Result, bail};
use hound::{SampleFormat, WavSpec, WavWriter};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

fn media_tools_available() -> bool {
    ["ffmpeg", "ffprobe"].into_iter().all(|tool| {
        crate::tool_command(tool)
            .arg("-version")
            .output()
            .is_ok_and(|output| output.status.success())
    })
}

struct NativeRuntimeFixture {
    _directory: TempDir,
    catalog: Catalog,
    project_id: String,
    asset_ids: Vec<String>,
    request: RunPolicyRequest,
    policy_id: String,
}

fn generated_media_asset(
    catalog: &Catalog,
    project_id: &str,
    root: &Path,
    id: &str,
    seconds: u32,
    with_audio: bool,
) -> String {
    let source = root.join(format!("{id}.mp4"));
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
    command.arg(format!(
        "color=c=0x{}:s=160x90:r=2:d={seconds}",
        &blake3::hash(id.as_bytes()).to_hex()[..6]
    ));
    if with_audio {
        command
            .args(["-f", "lavfi", "-i"])
            .arg(format!(
                "sine=frequency=440:sample_rate=16000:duration={seconds}"
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
        .expect("generate synthetic native runtime fixture");
    assert!(status.success(), "synthetic native runtime fixture failed");

    let proxy = root.join(format!("{id}.proxy.v3.mp4"));
    std::fs::copy(&source, &proxy).unwrap();
    crate::tests::insert_asset_at(catalog, project_id, id, &source);
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET duration_seconds=?2,source_out=?2,frame_rate=2,width=160,height=90,orientation_probed=1,proxy_path=?3 WHERE id=?1",
            rusqlite::params![id, f64::from(seconds), proxy.to_string_lossy()],
        )
        .unwrap();
    catalog.validate_asset_video(id).unwrap();
    id.to_string()
}

fn runtime_fixture(with_audio: &[bool], seconds: u32) -> NativeRuntimeFixture {
    assert!(
        media_tools_available(),
        "native runtime tests require ffmpeg/ffprobe"
    );
    let directory = TempDir::new().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Synthetic native runtime").unwrap();
    let mut asset_ids = Vec::new();
    for (index, has_audio) in with_audio.iter().copied().enumerate() {
        asset_ids.push(generated_media_asset(
            &catalog,
            &project.id,
            directory.path(),
            &format!("native-runtime-{index}"),
            seconds,
            has_audio,
        ));
    }
    let request = RunPolicyRequest::native_speech_transcription(
        project.id.clone(),
        asset_ids.clone(),
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
    NativeRuntimeFixture {
        _directory: directory,
        catalog,
        project_id: project.id,
        asset_ids,
        request,
        policy_id,
    }
}

fn install_fake_vad_artifact(catalog: &Catalog) {
    let directory = legacy_model_dir(catalog.database_path()).unwrap();
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join(VAD_MODEL.file_name), b"fake VAD fixture").unwrap();
}

fn pcm(seconds: u32) -> Vec<f32> {
    (0..seconds as usize * 16_000)
        .map(|index| ((index % 128) as f32 / 128.0) - 0.5)
        .collect()
}

fn write_pcm_wav(path: &Path, samples: &[f32]) -> Result<()> {
    let mut writer = WavWriter::create(
        path,
        WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        },
    )
    .context("create synthetic extracted WAV")?;
    for sample in samples {
        writer
            .write_sample((sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16)
            .context("write synthetic extracted sample")?;
    }
    writer
        .finalize()
        .context("finalize synthetic extracted WAV")
}

struct SequenceDetector {
    calls: Arc<AtomicUsize>,
    sequence: Arc<Mutex<VecDeque<Vec<SpeechRegion>>>>,
}

impl SpeechDetector for SequenceDetector {
    fn detect_speech(&mut self, _pcm: &[f32]) -> Result<Vec<SpeechRegion>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .sequence
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected extra VAD call"))
    }
}

fn fake_preparer(
    audio_present: bool,
    samples: Vec<f32>,
    detectors: Vec<Vec<SpeechRegion>>,
    detector_loads: Arc<AtomicUsize>,
) -> (NativeSpeechPreparer, Arc<AtomicUsize>) {
    let mut preparer = NativeSpeechPreparer::new();
    preparer.probe_hook = Some(Box::new(move |_| Ok(audio_present)));
    preparer.extract_hook = Some(Box::new(move |_source, destination, _cancel| {
        write_pcm_wav(destination, &samples)
    }));
    let detector_calls = Arc::new(AtomicUsize::new(0));
    let calls = Arc::clone(&detector_calls);
    let mut detector = Some(Box::new(SequenceDetector {
        calls,
        sequence: Arc::new(Mutex::new(VecDeque::from(detectors))),
    }) as Box<dyn SpeechDetector>);
    preparer.set_test_loader(Box::new(move |_path, _threads| {
        detector_loads.fetch_add(1, Ordering::SeqCst);
        Ok(detector.take().expect("one detector load"))
    }));
    (preparer, detector_calls)
}

fn failing_model_preparer() -> NativeSpeechPreparer {
    let mut preparer = NativeSpeechPreparer::new();
    preparer.probe_hook = Some(Box::new(|_| Ok(true)));
    preparer.extract_hook = Some(Box::new(|_, destination, _| {
        write_pcm_wav(destination, &pcm(1))
    }));
    preparer.set_test_loader(Box::new(|_, _| bail!("synthetic corrupt VAD model")));
    preparer
}

type ResponseFactory =
    Box<dyn FnMut(&NativeSttAudioWindow) -> Result<NativeSttCallResult> + 'static>;

struct RecordingNativeTransport<'a> {
    catalog: &'a Catalog,
    expected_asset_id: Option<String>,
    prepare_calls: AtomicUsize,
    transcribe_calls: AtomicUsize,
    claims_held_at_transport: AtomicBool,
    heavy_released_at_transport: AtomicBool,
    captured_duration_ms: Mutex<Option<u32>>,
    fail_prepare: bool,
    mutate_asset_on_prepare: Option<String>,
    response: Mutex<ResponseFactory>,
}

impl<'a> RecordingNativeTransport<'a> {
    fn new(
        catalog: &'a Catalog,
        expected_asset_id: Option<String>,
        response: impl FnMut(&NativeSttAudioWindow) -> Result<NativeSttCallResult> + 'static,
    ) -> Self {
        Self {
            catalog,
            expected_asset_id,
            prepare_calls: AtomicUsize::new(0),
            transcribe_calls: AtomicUsize::new(0),
            claims_held_at_transport: AtomicBool::new(false),
            heavy_released_at_transport: AtomicBool::new(false),
            captured_duration_ms: Mutex::new(None),
            fail_prepare: false,
            mutate_asset_on_prepare: None,
            response: Mutex::new(Box::new(response)),
        }
    }

    fn failing_prepare(mut self) -> Self {
        self.fail_prepare = true;
        self
    }

    fn mutating_asset_on_prepare(mut self, asset_id: &str) -> Self {
        self.mutate_asset_on_prepare = Some(asset_id.to_string());
        self
    }
}

impl NativeSttTransport for RecordingNativeTransport<'_> {
    fn prepare(&self) -> Result<()> {
        self.prepare_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_prepare {
            bail!("synthetic native credential failure");
        }
        if let Some(asset_id) = &self.mutate_asset_on_prepare {
            self.catalog
                .connection()
                .unwrap()
                .execute(
                    "UPDATE assets SET duration_seconds=duration_seconds+1 WHERE id=?1",
                    [asset_id],
                )
                .unwrap();
        }
        Ok(())
    }

    fn transcribe(
        &self,
        _request: &crate::NativeSttRequest,
        audio: &NativeSttAudioWindow,
    ) -> Result<NativeSttCallResult> {
        self.transcribe_calls.fetch_add(1, Ordering::SeqCst);
        *self.captured_duration_ms.lock().unwrap() = Some(audio.duration_ms());
        if let Some(asset_id) = &self.expected_asset_id {
            self.claims_held_at_transport.store(
                self.catalog
                    .try_claim_representation(asset_id, RepresentationKind::Speech)
                    .unwrap()
                    .is_none(),
                Ordering::SeqCst,
            );
        }
        self.heavy_released_at_transport.store(
            self.catalog.try_acquire_local_heavy().unwrap().is_some(),
            Ordering::SeqCst,
        );
        (self.response.lock().unwrap())(audio)
    }
}

fn decoded_call(
    audio: &NativeSttAudioWindow,
    confidence: Option<f64>,
    cost: Option<f64>,
) -> NativeSttCallResult {
    let end = audio.duration_ms() as f64 / 1_000.0;
    let mut payload = json!({
        "text": "Synthetic bounded native speech",
        "language": "english",
        "duration": end,
        "segments": [{
            "id": 0,
            "start": 0.0,
            "end": end,
            "text": "Synthetic bounded native speech",
            "confidence": confidence,
        }],
        "model": OPENROUTER_WHISPER_MODEL,
        "provider": "openai"
    });
    if let Some(cost) = cost {
        payload["usage"] = json!({
            "cost": cost,
            "seconds": end,
            "input_tokens": 12,
            "output_tokens": 4
        });
    }
    decode_native_stt_body(
        serde_json::to_vec(&payload).unwrap().as_slice(),
        audio.duration_ms(),
        Some("native-runtime-generation".into()),
        17,
    )
}

fn accepted_response() -> impl FnMut(&NativeSttAudioWindow) -> Result<NativeSttCallResult> {
    |audio| Ok(decoded_call(audio, Some(0.9), Some(0.0001)))
}

fn run_with_preparer<C: FnMut() -> bool>(
    fixture: &NativeRuntimeFixture,
    transport: &dyn NativeSttTransport,
    preparer: &mut NativeSpeechPreparer,
    retry_uncertain: bool,
    is_cancelled: &mut C,
) -> Result<RunPilotRecord> {
    fixture
        .catalog
        .run_native_speech_pilot_with_transport_and_preparer(
            &fixture.request,
            transport,
            retry_uncertain,
            preparer,
            is_cancelled,
        )
}

fn evidence_sample(record: &RunPilotRecord) -> &crate::NativePilotPaidSample {
    record
        .native_evidence
        .as_ref()
        .and_then(|evidence| evidence.sample.as_ref())
        .expect("paid native sample")
}

#[test]
fn all_no_audio_finalizes_provider_free_without_model_or_credentials() {
    let fixture = runtime_fixture(&[false, false], 1);
    // Deliberately no VAD artifact. The default preparer may probe the real
    // proxies because no audio must be a terminal local outcome before model work.
    let mut preparer = NativeSpeechPreparer::new();
    let transport = RecordingNativeTransport::new(&fixture.catalog, None, accepted_response());
    let record =
        run_with_preparer(&fixture, &transport, &mut preparer, false, &mut || false).unwrap();
    assert_eq!(record.status, "no_provider_required");
    assert!(!record.validation.capability_observed);
    assert_eq!(record.attempts, 0);
    assert_eq!(record.cost_usd, 0.0);
    assert!(record.actual_model.is_none() && record.provider.is_none());
    assert_eq!(transport.prepare_calls.load(Ordering::SeqCst), 0);
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 0);
    assert!(
        fixture
            .catalog
            .list_analysis_requests(&fixture.project_id, Some(&fixture.policy_id), 100)
            .unwrap()
            .is_empty()
    );
    let evidence = record.native_evidence.unwrap();
    assert!(evidence.sample.is_none());
    assert_eq!(evidence.local_outcomes.len(), 2);
    assert!(
        evidence
            .local_outcomes
            .iter()
            .all(|fact| fact.outcome == NativePilotLocalOutcome::NoAudio)
    );
}

#[test]
fn first_silent_then_speech_uses_actual_asset_and_one_window_only() {
    let fixture = runtime_fixture(&[true, true, true], 33);
    install_fake_vad_artifact(&fixture.catalog);
    let detector_loads = Arc::new(AtomicUsize::new(0));
    let (mut preparer, detector_calls) = fake_preparer(
        true,
        pcm(33),
        vec![
            Vec::new(),
            vec![SpeechRegion {
                start_ms: 0,
                end_ms: 33_000,
            }],
        ],
        Arc::clone(&detector_loads),
    );
    let transport = RecordingNativeTransport::new(
        &fixture.catalog,
        Some(fixture.asset_ids[1].clone()),
        accepted_response(),
    );
    let record =
        run_with_preparer(&fixture, &transport, &mut preparer, false, &mut || false).unwrap();

    assert_eq!(transport.prepare_calls.load(Ordering::SeqCst), 1);
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 1);
    assert_eq!(detector_loads.load(Ordering::SeqCst), 1);
    assert_eq!(detector_calls.load(Ordering::SeqCst), 2);
    assert!(transport.claims_held_at_transport.load(Ordering::SeqCst));
    assert!(transport.heavy_released_at_transport.load(Ordering::SeqCst));
    let captured_duration = transport.captured_duration_ms.lock().unwrap().unwrap();
    assert_eq!(
        captured_duration, 30_000,
        "only the first bounded window is sent"
    );
    assert_eq!(record.asset_id, fixture.asset_ids[1]);
    assert_eq!(record.status, "passed");
    let evidence = record.native_evidence.as_ref().unwrap();
    assert_eq!(evidence.local_outcomes.len(), 1);
    assert_eq!(
        evidence.local_outcomes[0].scope.asset_id,
        fixture.asset_ids[0]
    );
    assert_eq!(
        evidence.local_outcomes[0].outcome,
        NativePilotLocalOutcome::NoSpeech
    );
    let sample = evidence.sample.as_ref().unwrap();
    assert_eq!(sample.scope.asset_id, fixture.asset_ids[1]);
    assert_eq!(sample.window.speech_ms(), 30_000);

    let request: (String, String) = fixture
        .catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT source_fingerprint,attempt_base_key FROM analysis_request_reservations ORDER BY created_at DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let policy = fixture
        .catalog
        .load_run_authorization(&fixture.policy_id)
        .unwrap()
        .policy;
    assert_eq!(request.0, policy.scope[1].source_identity);
    assert_ne!(request.0, policy.scope[0].source_identity);
    // The third selected source was never prepared or provider-requested.
    assert!(
        record.asset_id != fixture.asset_ids[2]
            && evidence
                .local_outcomes
                .iter()
                .all(|fact| fact.scope.asset_id != fixture.asset_ids[2])
    );
}

#[test]
fn all_silent_audio_finalizes_provider_free_with_complete_local_facts() {
    let fixture = runtime_fixture(&[true, true], 1);
    install_fake_vad_artifact(&fixture.catalog);
    let detector_loads = Arc::new(AtomicUsize::new(0));
    let (mut preparer, _) =
        fake_preparer(true, pcm(1), vec![Vec::new(), Vec::new()], detector_loads);
    let transport = RecordingNativeTransport::new(&fixture.catalog, None, accepted_response());
    let record =
        run_with_preparer(&fixture, &transport, &mut preparer, false, &mut || false).unwrap();
    assert_eq!(record.status, "no_provider_required");
    assert!(!record.validation.capability_observed);
    assert_eq!(transport.prepare_calls.load(Ordering::SeqCst), 0);
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 0);
    let evidence = record.native_evidence.unwrap();
    assert_eq!(evidence.local_outcomes.len(), 2);
    assert!(
        evidence
            .local_outcomes
            .iter()
            .all(|fact| fact.outcome == NativePilotLocalOutcome::NoSpeech)
    );
    assert_eq!(
        fixture
            .catalog
            .list_analysis_requests(&fixture.project_id, Some(&fixture.policy_id), 100)
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn accepted_review_rejected_and_protocol_invalid_are_distinct_and_honest() {
    // Accepted quality: endpoint/source contract observed.
    let accepted = runtime_fixture(&[true], 1);
    install_fake_vad_artifact(&accepted.catalog);
    let loads = Arc::new(AtomicUsize::new(0));
    let (mut preparer, _) = fake_preparer(
        true,
        pcm(1),
        vec![vec![SpeechRegion {
            start_ms: 0,
            end_ms: 1_000,
        }]],
        loads,
    );
    let transport = RecordingNativeTransport::new(&accepted.catalog, None, accepted_response());
    let record =
        run_with_preparer(&accepted, &transport, &mut preparer, false, &mut || false).unwrap();
    assert_eq!(record.status, "passed");
    assert!(record.validation.capability_observed);
    assert!(
        record
            .observed_capabilities
            .iter()
            .any(|item| item.contains("quality recipe accepted"))
    );
    match &evidence_sample(&record).outcome {
        NativePilotSampleOutcome::Evaluated { quality, .. } => {
            assert_eq!(quality.decision, crate::NativeSttQualityDecision::Accepted);
        }
        other => panic!("expected accepted quality, got {other:?}"),
    }

    // Review-required quality: protocol/source contract observed, candidates
    // retained but explicitly unaccepted.
    let review = runtime_fixture(&[true], 1);
    install_fake_vad_artifact(&review.catalog);
    let loads = Arc::new(AtomicUsize::new(0));
    let (mut preparer, _) = fake_preparer(
        true,
        pcm(1),
        vec![vec![SpeechRegion {
            start_ms: 0,
            end_ms: 1_000,
        }]],
        loads,
    );
    let transport = RecordingNativeTransport::new(&review.catalog, None, |audio| {
        Ok(decoded_call(audio, None, Some(0.0001)))
    });
    let record =
        run_with_preparer(&review, &transport, &mut preparer, false, &mut || false).unwrap();
    assert_eq!(record.status, "passed");
    assert!(record.validation.capability_observed);
    assert!(
        record
            .observed_capabilities
            .iter()
            .any(|item| item.contains("explicitly unaccepted"))
    );
    match &evidence_sample(&record).outcome {
        NativePilotSampleOutcome::Evaluated {
            quality,
            candidates,
            ..
        } => {
            assert_eq!(
                quality.decision,
                crate::NativeSttQualityDecision::ReviewRequired
            );
            assert!(!candidates.is_empty());
        }
        other => panic!("expected review quality, got {other:?}"),
    }

    // Known-low confidence: rejected text is discarded and capability fails.
    let rejected = runtime_fixture(&[true], 1);
    install_fake_vad_artifact(&rejected.catalog);
    let loads = Arc::new(AtomicUsize::new(0));
    let (mut preparer, _) = fake_preparer(
        true,
        pcm(1),
        vec![vec![SpeechRegion {
            start_ms: 0,
            end_ms: 1_000,
        }]],
        loads,
    );
    let transport = RecordingNativeTransport::new(&rejected.catalog, None, |audio| {
        Ok(decoded_call(audio, Some(0.01), Some(0.0001)))
    });
    let record =
        run_with_preparer(&rejected, &transport, &mut preparer, false, &mut || false).unwrap();
    assert_eq!(record.status, "failed");
    assert!(!record.validation.capability_observed);
    assert!(record.failure_code.contains("quality_rejected"));
    match &evidence_sample(&record).outcome {
        NativePilotSampleOutcome::Evaluated {
            quality,
            candidates,
            ..
        } => {
            assert_eq!(quality.decision, crate::NativeSttQualityDecision::Rejected);
            assert!(candidates.is_empty(), "rejected text must be discarded");
        }
        other => panic!("expected rejected quality, got {other:?}"),
    }

    // Invalid protocol body: paid receipt is retained, capability is not.
    let invalid = runtime_fixture(&[true], 1);
    install_fake_vad_artifact(&invalid.catalog);
    let loads = Arc::new(AtomicUsize::new(0));
    let (mut preparer, _) = fake_preparer(
        true,
        pcm(1),
        vec![vec![SpeechRegion {
            start_ms: 0,
            end_ms: 1_000,
        }]],
        loads,
    );
    let transport = RecordingNativeTransport::new(&invalid.catalog, None, |audio| {
        let mut result = decode_native_stt_body(
            b"PRIVATE_PROVIDER_BODY_CANARY",
            audio.duration_ms(),
            Some("invalid-generation".into()),
            9,
        );
        // A paid but protocol-unusable reply still has a known invoice here so
        // this test isolates domain capability failure from unknown-cost
        // uncertainty, which is covered separately.
        result.receipt.usage.cost = crate::NativeSttCost::Known(0.0001);
        result.receipt.usage.seconds = Some(audio.duration_ms() as f64 / 1_000.0);
        Ok(result)
    });
    let record =
        run_with_preparer(&invalid, &transport, &mut preparer, false, &mut || false).unwrap();
    assert_eq!(record.status, "failed");
    assert!(!record.validation.capability_observed);
    assert!(
        !serde_json::to_string(&record)
            .unwrap()
            .contains("PRIVATE_PROVIDER_BODY_CANARY")
    );
    assert!(matches!(
        evidence_sample(&record).outcome,
        NativePilotSampleOutcome::ProtocolUnusable { .. }
    ));
}

#[test]
fn missing_key_and_corrupt_model_are_errors_not_silence() {
    // Corrupt/unloadable VAD model: local error, no provider preparation.
    let corrupt = runtime_fixture(&[true], 1);
    install_fake_vad_artifact(&corrupt.catalog);
    let mut preparer = failing_model_preparer();
    let transport = RecordingNativeTransport::new(&corrupt.catalog, None, accepted_response());
    let error =
        run_with_preparer(&corrupt, &transport, &mut preparer, false, &mut || false).unwrap_err();
    assert!(format!("{error:#}").contains("synthetic corrupt VAD model"));
    assert_eq!(transport.prepare_calls.load(Ordering::SeqCst), 0);
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 0);

    // Missing credential/transport configuration after local speech work but
    // before the durable dispatch marker: no transport send.
    let missing_key = runtime_fixture(&[true], 1);
    install_fake_vad_artifact(&missing_key.catalog);
    let loads = Arc::new(AtomicUsize::new(0));
    let (mut preparer, _) = fake_preparer(
        true,
        pcm(1),
        vec![vec![SpeechRegion {
            start_ms: 0,
            end_ms: 1_000,
        }]],
        loads,
    );
    let transport = RecordingNativeTransport::new(&missing_key.catalog, None, accepted_response())
        .failing_prepare();
    let error = run_with_preparer(&missing_key, &transport, &mut preparer, false, &mut || {
        false
    })
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("pre_upload_failed"),
        "{error:#}"
    );
    assert_eq!(transport.prepare_calls.load(Ordering::SeqCst), 1);
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 0);
    let requests = missing_key
        .catalog
        .list_analysis_requests(&missing_key.project_id, Some(&missing_key.policy_id), 100)
        .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].lifecycle, AnalysisRequestLifecycle::Released);
    assert_eq!(
        missing_key
            .catalog
            .analysis_request_needs_reconciliation(&requests[0].request_key)
            .unwrap(),
        None,
        "a locally released pre-upload request does not require provider reconciliation"
    );
}

#[test]
fn unknown_cost_is_uncertain_and_cached_without_automatic_resend() {
    let fixture = runtime_fixture(&[true], 1);
    install_fake_vad_artifact(&fixture.catalog);
    let loads = Arc::new(AtomicUsize::new(0));
    let (mut preparer, _) = fake_preparer(
        true,
        pcm(1),
        vec![vec![SpeechRegion {
            start_ms: 0,
            end_ms: 1_000,
        }]],
        loads,
    );
    let transport = RecordingNativeTransport::new(&fixture.catalog, None, |audio| {
        Ok(decoded_call(audio, Some(0.9), None))
    });
    let record =
        run_with_preparer(&fixture, &transport, &mut preparer, false, &mut || false).unwrap();
    assert_eq!(record.status, "uncertain");
    assert!(!record.usage_complete);
    assert!(record.uncertain_usage_reserve_usd > 0.0);
    assert!(evidence_sample(&record).latency_ms() > 0);
    assert!(matches!(
        evidence_sample(&record).outcome,
        NativePilotSampleOutcome::Evaluated { .. }
    ));

    // retry_uncertain cannot resend a typed unknown result.
    let second_transport =
        RecordingNativeTransport::new(&fixture.catalog, None, accepted_response());
    let error = run_with_preparer(
        &fixture,
        &second_transport,
        &mut preparer,
        true,
        &mut || false,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("reconcile"));
    assert_eq!(second_transport.transcribe_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn accepted_known_cost_is_cached_and_overrun_stops_shared_authority() {
    let fixture = runtime_fixture(&[true], 1);
    install_fake_vad_artifact(&fixture.catalog);
    let loads = Arc::new(AtomicUsize::new(0));
    let (mut preparer, _) = fake_preparer(
        true,
        pcm(1),
        vec![vec![SpeechRegion {
            start_ms: 0,
            end_ms: 1_000,
        }]],
        loads,
    );
    let transport = RecordingNativeTransport::new(&fixture.catalog, None, accepted_response());
    let record =
        run_with_preparer(&fixture, &transport, &mut preparer, false, &mut || false).unwrap();
    assert_eq!(record.status, "passed");
    assert!(record.usage_complete);
    assert_eq!(record.provider.as_deref(), Some("openai"));
    assert_eq!(
        record.actual_model.as_deref(),
        Some(OPENROUTER_WHISPER_MODEL)
    );
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 1);

    // Default replay, including retry_uncertain=true, returns the current cached
    // record with zero transport/model work.
    let repeat_transport =
        RecordingNativeTransport::new(&fixture.catalog, None, accepted_response());
    let cached = run_with_preparer(
        &fixture,
        &repeat_transport,
        &mut preparer,
        true,
        &mut || false,
    )
    .unwrap();
    assert_eq!(cached, record);
    assert_eq!(repeat_transport.transcribe_calls.load(Ordering::SeqCst), 0);

    // A reported charge above the conservative reservation must stop future
    // shared-authority dispatch. The pilot itself still has only one paid call.
    let overrun = runtime_fixture(&[true], 1);
    install_fake_vad_artifact(&overrun.catalog);
    let loads = Arc::new(AtomicUsize::new(0));
    let (mut preparer, _) = fake_preparer(
        true,
        pcm(1),
        vec![vec![SpeechRegion {
            start_ms: 0,
            end_ms: 1_000,
        }]],
        loads,
    );
    let transport = RecordingNativeTransport::new(&overrun.catalog, None, |audio| {
        Ok(decoded_call(audio, Some(0.9), Some(0.01)))
    });
    run_with_preparer(&overrun, &transport, &mut preparer, false, &mut || false).unwrap();
    let requests = overrun
        .catalog
        .list_analysis_requests(&overrun.project_id, Some(&overrun.policy_id), 100)
        .unwrap();
    assert_eq!(requests.len(), 1);
    let snapshot = overrun
        .catalog
        .analysis_budget_snapshot(&requests[0].budget_scope_id)
        .unwrap()
        .expect("native pilot budget authority");
    assert!(
        snapshot.dispatch_stopped,
        "receipt overrun must stop dispatch"
    );
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn early_cancel_has_zero_effects_and_post_reply_cancel_retains_paid_cache() {
    let fixture = runtime_fixture(&[true], 1);
    install_fake_vad_artifact(&fixture.catalog);
    let loads = Arc::new(AtomicUsize::new(0));
    let (mut preparer, _) = fake_preparer(
        true,
        pcm(1),
        vec![vec![SpeechRegion {
            start_ms: 0,
            end_ms: 1_000,
        }]],
        loads,
    );
    let transport = RecordingNativeTransport::new(&fixture.catalog, None, accepted_response());
    let error =
        run_with_preparer(&fixture, &transport, &mut preparer, false, &mut || true).unwrap_err();
    assert!(format!("{error:#}").contains("cancelled"));
    assert_eq!(transport.prepare_calls.load(Ordering::SeqCst), 0);
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 0);
    assert!(
        fixture
            .catalog
            .list_analysis_requests(&fixture.project_id, Some(&fixture.policy_id), 100)
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .catalog
            .load_run_pilot(&fixture.policy_id)
            .unwrap()
            .is_none()
    );

    let cancel_after_reply = Arc::new(AtomicBool::new(false));
    let cancel_for_transport = Arc::clone(&cancel_after_reply);
    let transport = RecordingNativeTransport::new(&fixture.catalog, None, move |audio| {
        cancel_for_transport.store(true, Ordering::SeqCst);
        Ok(decoded_call(audio, Some(0.9), Some(0.0001)))
    });
    let loads = Arc::new(AtomicUsize::new(0));
    let (mut preparer, _) = fake_preparer(
        true,
        pcm(1),
        vec![vec![SpeechRegion {
            start_ms: 0,
            end_ms: 1_000,
        }]],
        loads,
    );
    let error = run_with_preparer(&fixture, &transport, &mut preparer, false, &mut || {
        cancel_after_reply.load(Ordering::SeqCst)
    })
    .unwrap_err();
    assert!(format!("{error:#}").contains("after provider response"));
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 1);
    let cached = fixture
        .catalog
        .load_run_pilot(&fixture.policy_id)
        .unwrap()
        .unwrap();
    assert!(cached.native_evidence.as_ref().unwrap().sample.is_some());
    assert!(cached.usage_complete);
    let requests = fixture
        .catalog
        .list_analysis_requests(&fixture.project_id, Some(&fixture.policy_id), 100)
        .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].lifecycle, AnalysisRequestLifecycle::Completed);
    assert!(requests[0].confirmed_cost_usd > 0.0);

    let replay_transport =
        RecordingNativeTransport::new(&fixture.catalog, None, accepted_response());
    let replay = run_with_preparer(
        &fixture,
        &replay_transport,
        &mut preparer,
        true,
        &mut || false,
    )
    .unwrap();
    assert_eq!(replay, cached);
    assert_eq!(replay_transport.transcribe_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn stale_auth_budget_source_and_receipt_only_work_fail_closed() {
    // Missing canonical authorization.
    let no_auth = runtime_fixture(&[false], 1);
    no_auth
        .catalog
        .connection()
        .unwrap()
        .execute(
            "DELETE FROM analysis_run_authorizations WHERE policy_id=?1",
            [&no_auth.policy_id],
        )
        .unwrap();
    let mut preparer = NativeSpeechPreparer::new();
    let transport = RecordingNativeTransport::new(&no_auth.catalog, None, accepted_response());
    let error =
        run_with_preparer(&no_auth, &transport, &mut preparer, false, &mut || false).unwrap_err();
    assert!(format!("{error:#}").contains("authoriz"), "{error:#}");
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 0);

    // Budget below the native quote is denied before any owned run.
    let low_budget = runtime_fixture(&[true], 1);
    let mut request = low_budget.request.clone();
    request.budget_usd = 1e-9;
    let mut low_preparer = NativeSpeechPreparer::new();
    let low_transport =
        RecordingNativeTransport::new(&low_budget.catalog, None, accepted_response());
    let error = low_budget
        .catalog
        .run_native_speech_pilot_with_transport_and_preparer(
            &request,
            &low_transport,
            false,
            &mut low_preparer,
            &mut || false,
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("not ready for a pilot"));
    assert_eq!(low_transport.transcribe_calls.load(Ordering::SeqCst), 0);

    // Source changes during transport preparation are caught by the accounting
    // pre-dispatch guard before the single send.
    let stale = runtime_fixture(&[true], 1);
    install_fake_vad_artifact(&stale.catalog);
    let loads = Arc::new(AtomicUsize::new(0));
    let (mut preparer, _) = fake_preparer(
        true,
        pcm(1),
        vec![vec![SpeechRegion {
            start_ms: 0,
            end_ms: 1_000,
        }]],
        loads,
    );
    let transport = RecordingNativeTransport::new(&stale.catalog, None, accepted_response())
        .mutating_asset_on_prepare(&stale.asset_ids[0]);
    let error =
        run_with_preparer(&stale, &transport, &mut preparer, false, &mut || false).unwrap_err();
    assert!(
        format!("{error:#}").contains("pre_upload_failed"),
        "{error:#}"
    );
    assert_eq!(transport.prepare_calls.load(Ordering::SeqCst), 1);
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 0);

    // Receipt-only interrupted work (a running owned row with an accounted
    // receipt but no typed domain sample) refuses generic replay.
    let receipt_only = runtime_fixture(&[true], 1);
    install_fake_vad_artifact(&receipt_only.catalog);
    let policy = receipt_only
        .catalog
        .load_run_authorization(&receipt_only.policy_id)
        .unwrap()
        .policy;
    let request = crate::analysis::run_policy::native_stt_request_from_policy(&policy).unwrap();
    let audio = {
        let window = crate::analysis::windows::pack_windows(&[SpeechRegion {
            start_ms: 0,
            end_ms: 1_000,
        }])
        .remove(0);
        NativeSttAudioWindow::from_pcm(&pcm(1), &window).unwrap()
    };
    receipt_only
        .catalog
        .initialize_analysis_accounting_authority(
            &crate::analysis::run_policy::pilot_authority_context(&policy),
        )
        .unwrap();
    let run_id = receipt_only
        .catalog
        .begin_accounted_analysis_run(&policy.project_id, &policy.policy_id, "pilot")
        .unwrap();
    let context = crate::analysis::run_policy::pilot_accounting_context_for_scope(
        &policy,
        &run_id,
        &policy.scope[0],
        false,
    )
    .unwrap();
    let transport = RecordingNativeTransport::new(&receipt_only.catalog, None, accepted_response());
    AccountedNativeSttProvider::new(
        &receipt_only.catalog,
        &policy,
        context,
        &request,
        &audio,
        &transport,
    )
    .transmit()
    .unwrap();
    let running = RunPilotRecord {
        schema_version: RUN_PILOT_SCHEMA_VERSION.into(),
        policy_id: policy.policy_id.clone(),
        project_id: policy.project_id.clone(),
        status: "running".into(),
        started_at: "2026-01-01T00:00:00Z".into(),
        finished_at: String::new(),
        run_id,
        asset_id: policy.scope[0].asset_id.clone(),
        requested_model: policy.primary_model.clone(),
        actual_model: None,
        provider: None,
        generation_id: None,
        attempted_models: Vec::new(),
        attempts: 1,
        prompt_tokens: 0,
        completion_tokens: 0,
        cost_usd: 0.0,
        confirmed_cost_usd: 0.0,
        uncertain_usage_reserve_usd: 0.0,
        active_reserved_cost_usd: 0.0,
        usage_complete: true,
        failure_code: String::new(),
        prior_cost_usd: 0.0,
        prior_attempts: 0,
        validation: PilotValidation {
            structural_schema_valid: false,
            source_time_semantics_valid: false,
            capability_observed: false,
            detail: "pilot_running".into(),
        },
        observed_capabilities: Vec::new(),
        warning: PILOT_WARNING.into(),
        native_evidence: None,
    };
    receipt_only.catalog.save_pilot(&running).unwrap();
    let loads = Arc::new(AtomicUsize::new(0));
    let (mut preparer, _) = fake_preparer(
        true,
        pcm(1),
        vec![vec![SpeechRegion {
            start_ms: 0,
            end_ms: 1_000,
        }]],
        loads,
    );
    let replay_transport =
        RecordingNativeTransport::new(&receipt_only.catalog, None, accepted_response());
    let error = run_with_preparer(
        &receipt_only,
        &replay_transport,
        &mut preparer,
        true,
        &mut || false,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("interrupted or unknown usage"));
    assert_eq!(replay_transport.transcribe_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn missing_actual_identity_stays_none_and_explicit_model_mismatch_fails_closed() {
    // Provider omitted both actual identities: retain None and never substitute
    // either the requested broker (openrouter) or the requested model.
    let missing_identity = runtime_fixture(&[true], 1);
    install_fake_vad_artifact(&missing_identity.catalog);
    let loads = Arc::new(AtomicUsize::new(0));
    let (mut preparer, _) = fake_preparer(
        true,
        pcm(1),
        vec![vec![SpeechRegion {
            start_ms: 0,
            end_ms: 1_000,
        }]],
        loads,
    );
    let transport = RecordingNativeTransport::new(&missing_identity.catalog, None, |audio| {
        let end = audio.duration_ms() as f64 / 1_000.0;
        let payload = json!({
            "text": "Synthetic bounded native speech",
            "language": "english",
            "duration": end,
            "segments": [{
                "start": 0.0,
                "end": end,
                "text": "Synthetic bounded native speech",
                "confidence": 0.9
            }],
            "usage": {"cost": 0.0001, "seconds": end}
        });
        Ok(decode_native_stt_body(
            serde_json::to_vec(&payload).unwrap().as_slice(),
            audio.duration_ms(),
            Some("missing-identity-generation".into()),
            11,
        ))
    });
    let record = run_with_preparer(
        &missing_identity,
        &transport,
        &mut preparer,
        false,
        &mut || false,
    )
    .unwrap();
    assert_eq!(record.status, "passed");
    assert!(record.actual_model.is_none());
    assert!(record.provider.is_none());
    assert!(record.validation.capability_observed);

    // An unverified returned model identifier fails closed into review. No alias normalization or broker/upstream namespace equality is
    // attempted; the requested and actual identities remain distinct facts.
    let mismatch = runtime_fixture(&[true], 1);
    install_fake_vad_artifact(&mismatch.catalog);
    let loads = Arc::new(AtomicUsize::new(0));
    let (mut preparer, _) = fake_preparer(
        true,
        pcm(1),
        vec![vec![SpeechRegion {
            start_ms: 0,
            end_ms: 1_000,
        }]],
        loads,
    );
    let transport = RecordingNativeTransport::new(&mismatch.catalog, None, |audio| {
        let end = audio.duration_ms() as f64 / 1_000.0;
        let payload = json!({
            "text": "Synthetic bounded native speech",
            "language": "english",
            "duration": end,
            "segments": [{
                "start": 0.0,
                "end": end,
                "text": "Synthetic bounded native speech",
                "confidence": 0.9
            }],
            "model": "different/audio-model",
            "provider": "openai",
            "usage": {"cost": 0.0001, "seconds": end}
        });
        Ok(decode_native_stt_body(
            serde_json::to_vec(&payload).unwrap().as_slice(),
            audio.duration_ms(),
            Some("mismatch-generation".into()),
            12,
        ))
    });
    let record =
        run_with_preparer(&mismatch, &transport, &mut preparer, false, &mut || false).unwrap();
    assert_eq!(record.status, "failed");
    assert!(!record.validation.capability_observed);
    assert_eq!(
        record.actual_model.as_deref(),
        Some("different/audio-model")
    );
    assert_eq!(record.provider.as_deref(), Some("openai"));
    assert!(record.failure_code.contains("identity_requires_review"));
    // The typed paid sample is retained so a generic retry cannot resend it.
    assert!(!evidence_sample(&record).request_key().is_empty());
}

#[test]
fn force_replacement_policy_cannot_bypass_unknown_native_work_on_a_later_scope() {
    let fixture = runtime_fixture(&[true, true], 1);
    let policy = fixture
        .catalog
        .load_run_authorization(&fixture.policy_id)
        .unwrap()
        .policy;
    fixture
        .catalog
        .initialize_analysis_accounting_authority(
            &crate::analysis::run_policy::pilot_authority_context(&policy),
        )
        .unwrap();
    let request = crate::analysis::run_policy::native_stt_request_from_policy(&policy).unwrap();
    let audio = {
        let window = crate::analysis::windows::pack_windows(&[SpeechRegion {
            start_ms: 0,
            end_ms: 1_000,
        }])
        .remove(0);
        NativeSttAudioWindow::from_pcm(&pcm(1), &window).unwrap()
    };
    let run_id = fixture
        .catalog
        .begin_accounted_analysis_run(&policy.project_id, &policy.policy_id, "pilot")
        .unwrap();
    let context = crate::analysis::run_policy::pilot_accounting_context_for_scope(
        &policy,
        &run_id,
        &policy.scope[1],
        false,
    )
    .unwrap();
    let interrupted = RecordingNativeTransport::new(&fixture.catalog, None, |_audio| {
        bail!("synthetic native transport interruption after dispatch")
    });
    let _ = AccountedNativeSttProvider::new(
        &fixture.catalog,
        &policy,
        context,
        &request,
        &audio,
        &interrupted,
    )
    .transmit()
    .unwrap_err();
    let unresolved = fixture
        .catalog
        .list_analysis_requests(&fixture.project_id, Some(&fixture.policy_id), 100)
        .unwrap();
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].lifecycle, AnalysisRequestLifecycle::Uncertain);

    let RunPolicyParameters::NativeSpeechTranscription {
        language,
        provider_policy,
        ..
    } = fixture.request.parameters.clone()
    else {
        panic!("expected native parameters");
    };
    let mut forced = fixture.request.clone();
    forced.parameters = RunPolicyParameters::NativeSpeechTranscription {
        language,
        provider_policy,
        force: true,
    };
    let forced_policy_id = fixture
        .catalog
        .authorize_run_policy(&forced)
        .unwrap()
        .policy
        .policy_id;
    assert_ne!(forced_policy_id, fixture.policy_id);
    let mut preparer = NativeSpeechPreparer::new();
    let replacement = RecordingNativeTransport::new(&fixture.catalog, None, accepted_response());
    let error = fixture
        .catalog
        .run_native_speech_pilot_with_transport_and_preparer(
            &forced,
            &replacement,
            true,
            &mut preparer,
            &mut || false,
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("unresolved usage"),
        "{error:#}"
    );
    assert_eq!(replacement.prepare_calls.load(Ordering::SeqCst), 0);
    assert_eq!(replacement.transcribe_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn native_and_vlm_pilot_readiness_require_credentials_without_changing_policy_identity() {
    let (_directory, catalog, project, asset) = fixture();
    let request = observation_request(&project, &asset, 1.0);
    let missing = catalog
        .describe_run_policy_with_credentials(&request, false)
        .unwrap();
    let present = catalog
        .describe_run_policy_with_credentials(&request, true)
        .unwrap();
    assert!(missing.readiness.ready_for_authorization);
    assert!(!missing.readiness.ready_for_pilot);
    assert!(present.readiness.ready_for_pilot);
    assert_eq!(missing.policy_id, present.policy_id);
    assert_eq!(missing.capability_checks, present.capability_checks);

    let native = runtime_fixture(&[true], 1);
    install_fake_vad_artifact(&native.catalog);
    let missing = native
        .catalog
        .describe_run_policy_with_credentials(&native.request, false)
        .unwrap();
    let present = native
        .catalog
        .describe_run_policy_with_credentials(&native.request, true)
        .unwrap();
    assert!(missing.readiness.ready_for_authorization);
    assert!(!missing.readiness.ready_for_pilot);
    assert!(present.readiness.ready_for_pilot);
    assert_eq!(missing.policy_id, present.policy_id);
    assert_eq!(missing.capability_checks, present.capability_checks);
}

#[test]
fn an_earlier_source_change_during_silent_scan_cannot_finalize_provider_free() {
    let fixture = runtime_fixture(&[true, true], 1);
    install_fake_vad_artifact(&fixture.catalog);
    let changed_proxy = fixture
        .catalog
        .get_asset(&fixture.asset_ids[0])
        .unwrap()
        .proxy_path
        .unwrap();
    let loads = Arc::new(AtomicUsize::new(0));
    let (mut preparer, _) = fake_preparer(true, pcm(1), vec![Vec::new(), Vec::new()], loads);
    let mut extracts = 0;
    preparer.extract_hook = Some(Box::new(move |_, destination, _| {
        extracts += 1;
        if extracts == 2 {
            std::fs::write(
                &changed_proxy,
                b"earlier selected proxy changed after its claim was released",
            )?;
        }
        write_pcm_wav(destination, &pcm(1))
    }));
    let transport = RecordingNativeTransport::new(&fixture.catalog, None, accepted_response());
    let error =
        run_with_preparer(&fixture, &transport, &mut preparer, false, &mut || false).unwrap_err();
    assert!(
        format!("{error:#}").contains("changed during local speech scan"),
        "{error:#}"
    );
    assert_eq!(transport.prepare_calls.load(Ordering::SeqCst), 0);
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 0);
    assert!(
        fixture
            .catalog
            .load_run_pilot(&fixture.policy_id)
            .unwrap()
            .is_none()
    );
    let runs: i64 = fixture
        .catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM analysis_runs WHERE policy_id=?1",
            [&fixture.policy_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        runs, 0,
        "stale local evidence must not become a completed pilot projection"
    );
}

#[test]
fn existing_vlm_pilot_observed_capability_strings_remain_compatible() {
    let (_directory, catalog, project, asset) = fixture();
    let (_, _, record) = authorized_observation(&catalog, &project, &asset);
    assert_eq!(
        record.observed_capabilities,
        vec![
            "configured JSON schema accepted this response",
            "explicit source-time semantics accepted this response",
        ]
    );
    assert!(record.native_evidence.is_none());
}
