use super::*;
use crate::review_targets::tests::{PROJECT, actor, catalog, now, request, valid};
use crate::{
    ChunkUpload, ControlCatalog, CreateReviewSessionRequest, FinalizeReviewSessionRequest,
    RecordingAudioFormat, RecordingCaptureSettings, RecordingConsent, RecordingEndReason,
};
use rcdvlog_core::review_speech::{
    NativeSttCallResult, NativeSttReceipt, NativeSttSegment, NativeSttTranscript, NativeSttUsage,
};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;

pub(crate) const RATE: u32 = 48_000;

/// Energy VAD over 10 ms frames at 16 kHz: deterministic and model-free.
fn energy_regions(pcm: &[f32]) -> Vec<SpeechRegion> {
    let mut regions: Vec<SpeechRegion> = Vec::new();
    for (index, frame) in pcm.chunks(160).enumerate() {
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt();
        if rms > 0.05 {
            let start = index as u32 * 10;
            match regions.last_mut() {
                Some(last) if last.end_ms + 200 >= start => last.end_ms = start + 10,
                _ => regions.push(SpeechRegion {
                    start_ms: start,
                    end_ms: start + 10,
                }),
            }
        }
    }
    regions
}

struct ScriptedEngine {
    texts: Mutex<Vec<(&'static str, &'static str)>>,
}

impl SttEngine for ScriptedEngine {
    fn detect_speech(&self, pcm: &[f32]) -> anyhow::Result<Vec<SpeechRegion>> {
        Ok(energy_regions(pcm))
    }
    fn decode_window(
        &self,
        _pcm: &[f32],
        window: &PackedWindow,
    ) -> anyhow::Result<Vec<DecodedSegment>> {
        let mut out = Vec::new();
        let mut offset = 0_u32;
        let mut texts = self.texts.lock().unwrap();
        for region in &window.regions {
            let length = region.duration_ms;
            let (language, text) = if texts.is_empty() {
                ("en", "okay")
            } else {
                texts.remove(0)
            };
            out.push(DecodedSegment {
                start_ms: offset,
                end_ms: offset + length,
                language: language.into(),
                text: text.into(),
                mean_token_probability: Some(0.8),
            });
            offset += length;
        }
        Ok(out)
    }
}

struct FakeTransport {
    calls: Arc<AtomicUsize>,
    fail_on_call: Option<usize>,
    unusable: Option<NativeSttProtocolError>,
    unknown_cost: bool,
}

impl NativeSttTransport for FakeTransport {
    fn prepare(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn transcribe(
        &self,
        request: &NativeSttRequest,
        audio: &NativeSttAudioWindow,
    ) -> anyhow::Result<NativeSttCallResult> {
        assert!(
            !request
                .provider_policy()
                .zero_data_retention
                .unwrap_or(true)
        );
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_on_call == Some(call) {
            anyhow::bail!("connection reset after send");
        }
        Ok(NativeSttCallResult {
            provider_error: self.unusable.map(|_| NativeSttProviderError {
                http_status: 400,
                code: Some("unsupported_parameter".into()),
                message: "The selected provider rejected timestamp_granularities".into(),
            }),
            receipt: NativeSttReceipt {
                generation_id: Some(format!("gen-{call}")),
                actual_model: Some(request.model().into()),
                actual_provider: Some("OpenAI".into()),
                usage: NativeSttUsage {
                    cost: if self.unknown_cost {
                        NativeSttCost::Unknown
                    } else {
                        NativeSttCost::Known(0.001)
                    },
                    seconds: None,
                    input_tokens: None,
                    output_tokens: None,
                },
            },
            latency_ms: 5,
            outcome: self
                .unusable
                .map(NativeSttOutcome::Unusable)
                .unwrap_or_else(|| {
                    NativeSttOutcome::Usable(NativeSttTranscript {
                        text: "這裡太長".into(),
                        language: "zh".into(),
                        confidence: None,
                        duration_ms: audio.duration_ms(),
                        segments: vec![NativeSttSegment {
                            start_ms: 0,
                            end_ms: audio.duration_ms(),
                            text: "這裡太長".into(),
                            confidence: None,
                            avg_logprob: None,
                            words: vec![],
                        }],
                    })
                }),
        })
    }
}

pub(crate) struct FakeRuntime {
    models: bool,
    credentials: bool,
    calls: Arc<AtomicUsize>,
    fail_on_call: Option<usize>,
    unusable: Option<NativeSttProtocolError>,
    unknown_cost: bool,
}

impl FakeRuntime {
    pub(crate) fn new() -> Self {
        Self {
            models: true,
            credentials: true,
            calls: Arc::new(AtomicUsize::new(0)),
            fail_on_call: None,
            unusable: None,
            unknown_cost: false,
        }
    }
}

impl ReviewSpeechRuntime for FakeRuntime {
    fn live_models_present(&self) -> bool {
        self.models
    }
    fn live_engine(&self) -> anyhow::Result<Box<dyn SttEngine>> {
        self.local_engine()
    }
    fn local_models_present(&self) -> bool {
        self.models
    }
    fn vad_model_present(&self) -> bool {
        self.models
    }
    fn local_engine(&self) -> anyhow::Result<Box<dyn SttEngine>> {
        Ok(Box::new(ScriptedEngine {
            texts: Mutex::new(vec![
                ("en", "This shot is too long"),
                ("zh", "这个镜头太长了"),
                ("en", "cut 这里 please"),
            ]),
        }))
    }
    fn detect_speech(&self, pcm: &[f32]) -> anyhow::Result<Vec<SpeechRegion>> {
        Ok(energy_regions(pcm))
    }
    fn cloud_models(&self) -> anyhow::Result<Vec<NativeSttModelChoice>> {
        Ok(vec![
            NativeSttModelChoice {
                id: "openai/whisper-1".into(),
                name: "Whisper".into(),
            },
            NativeSttModelChoice {
                id: "google/gemini-3.5-transcribe".into(),
                name: "Gemini".into(),
            },
        ])
    }
    fn cloud_credentials_present(&self) -> bool {
        self.credentials
    }
    fn cloud_transport(&self) -> anyhow::Result<Box<dyn NativeSttTransport>> {
        Ok(Box::new(FakeTransport {
            calls: self.calls.clone(),
            fail_on_call: self.fail_on_call,
            unusable: self.unusable,
            unknown_cost: self.unknown_cost,
        }))
    }
}

/// `seconds` of 48 kHz mono audio with 1 kHz tone bursts where `speech` says.
pub(crate) fn tone(seconds: f64, speech: &[(f64, f64)]) -> Vec<i16> {
    (0..(seconds * f64::from(RATE)) as usize)
        .map(|i| {
            let t = i as f64 / f64::from(RATE);
            if speech.iter().any(|(a, b)| t >= *a && t < *b) {
                ((2.0 * std::f64::consts::PI * 1000.0 * t).sin() * 0.5 * 32767.0) as i16
            } else {
                0
            }
        })
        .collect()
}

pub(crate) struct Fixture {
    pub(crate) _root: TempDir,
    pub(crate) catalog: ControlCatalog,
    pub(crate) session_id: String,
}

/// 20 s recording: speech 1–4 s, 6–9 s, 12–15 s. `drop_second_chunk` loses 10–20 s.
pub(crate) fn fixture(drop_second_chunk: bool, finalize: bool) -> Fixture {
    fixture_for(drop_second_chunk, finalize, false)
}

/// As [`fixture`], against a historical rendered-artifact target when `historical`.
pub(crate) fn fixture_for(drop_second_chunk: bool, finalize: bool, historical: bool) -> Fixture {
    let root = TempDir::new().unwrap();
    let catalog = catalog(&root);
    let targets = catalog.review_targets();
    let imported = if historical {
        targets.import_historical(
            PROJECT,
            &crate::review_targets::tests::historical_request(
                crate::review_targets::tests::historical_valid(),
            ),
            &actor(),
            now(),
        )
    } else {
        targets.import(PROJECT, &request(valid()), &actor(), now())
    };
    let target_id = imported.unwrap().review_target.summary.review_target_id;
    let sessions = catalog.review_sessions(0);
    let session = sessions
        .create(
            PROJECT,
            &CreateReviewSessionRequest {
                review_target_id: target_id,
                client_session_key: "transcribe_key".into(),
                consent: RecordingConsent {
                    all_present_acknowledged: true,
                    additional_speakers_disclosed: true,
                },
                audio: RecordingAudioFormat {
                    sample_rate: RATE,
                    channels: 1,
                    encoding: "pcm_s16le".into(),
                },
                capture: RecordingCaptureSettings {
                    echo_cancellation: Some(false),
                    noise_suppression: Some(false),
                    auto_gain_control: Some(false),
                    clock: "audioWorkletCurrentFrame".into(),
                    base_latency_seconds: None,
                    user_agent_family: None,
                },
                client_started_performance_ms: None,
            },
            &actor(),
            now(),
        )
        .unwrap();
    let id = session.summary.session_id.clone();
    let samples = tone(20.0, &[(1.0, 4.0), (6.0, 9.0), (12.0, 15.0)]);
    for (sequence, part) in samples.chunks((RATE * 10) as usize).enumerate() {
        if drop_second_chunk && sequence == 1 {
            continue;
        }
        let bytes: Vec<u8> = part.iter().flat_map(|s| s.to_le_bytes()).collect();
        sessions
            .put_chunk(
                PROJECT,
                &id,
                &ChunkUpload {
                    sequence: sequence as u32,
                    start_frame: (sequence as u64) * u64::from(RATE) * 10,
                    frame_count: part.len() as u32,
                    sha256: hex::encode(Sha256::digest(&bytes)),
                    capture_end_performance_ms: None,
                },
                &bytes,
                now(),
            )
            .unwrap();
    }
    if finalize {
        sessions
            .finalize(
                PROJECT,
                &id,
                &FinalizeReviewSessionRequest {
                    final_frame_count: u64::from(RATE) * 20,
                    end_reason: RecordingEndReason::Stopped,
                },
                now(),
            )
            .unwrap();
    }
    Fixture {
        _root: root,
        catalog,
        session_id: id,
    }
}

pub(crate) fn local() -> ReviewTranscriptionRequest {
    ReviewTranscriptionRequest {
        backend: ReviewTranscriptionBackend::Local,
        model: None,
        language_hint: None,
        authorized_max_cost_usd: None,
        cloud_upload_acknowledged: false,
        zero_data_retention: false,
    }
}

fn cloud(cap: f64) -> ReviewTranscriptionRequest {
    ReviewTranscriptionRequest {
        backend: ReviewTranscriptionBackend::Openrouter,
        model: Some("openai/whisper-1".into()),
        language_hint: None,
        authorized_max_cost_usd: Some(cap),
        cloud_upload_acknowledged: true,
        zero_data_retention: true,
    }
}

pub(crate) fn run(
    f: &Fixture,
    request: &ReviewTranscriptionRequest,
    runtime: &FakeRuntime,
) -> ReviewTranscriptionRun {
    let transcripts = f.catalog.review_transcripts();
    let started = transcripts
        .start(
            PROJECT,
            &f.session_id,
            request,
            runtime,
            &f.catalog.jobs(),
            &actor(),
        )
        .unwrap();
    transcripts
        .execute(&started.run_id, runtime, &|| false)
        .unwrap();
    transcripts.get(PROJECT, &started.run_id).unwrap()
}

#[test]
fn local_transcription_produces_recording_clock_utterances_with_honest_speaker_and_script_fields() {
    let f = fixture(false, true);
    let runtime = FakeRuntime::new();
    let preflight = f
        .catalog
        .review_transcripts()
        .preflight(PROJECT, &f.session_id, &local(), &runtime)
        .unwrap();
    assert!(preflight.ready, "{:?}", preflight.blockers);
    assert!(preflight.warnings.contains(&"diarization_unavailable"));
    assert_eq!(preflight.estimate.as_ref().unwrap().max_cost_usd, None);

    let result = run(&f, &local(), &runtime);
    assert_eq!(result.state, ReviewTranscriptionState::Completed);
    assert_eq!(result.schema, "review-transcript.v1");
    assert_eq!(result.utterances.len(), 3);
    let starts: Vec<f64> = result.utterances.iter().map(|u| u.start_seconds).collect();
    for (actual, expected) in starts.iter().zip([1.0, 6.0, 12.0]) {
        assert!(
            (actual - expected).abs() < 0.15,
            "start {actual} vs {expected}"
        );
    }
    let zh = &result.utterances[1];
    assert_eq!(zh.text, "这个镜头太长了");
    assert_eq!(zh.display_text, "這個鏡頭太長了");
    assert_eq!(zh.script, "hanSimplified");
    assert_eq!(zh.display_projection.as_deref(), Some("openccS2tw"));
    assert_eq!(result.utterances[2].script, "mixed");
    assert!(
        result
            .utterances
            .iter()
            .all(|u| u.speaker == "unknown" && u.speaker_evidence == "diarizationUnavailable")
    );
    assert_eq!(result.diarization.status, "unavailable");
    assert_eq!(result.spend.known_cost_usd, 0.0);

    // Identical request for identical audio reuses the completed run.
    let again = f
        .catalog
        .review_transcripts()
        .start(
            PROJECT,
            &f.session_id,
            &local(),
            &runtime,
            &f.catalog.jobs(),
            &actor(),
        )
        .unwrap();
    assert_eq!(again.run_id, result.run_id);
    assert_eq!(again.state, ReviewTranscriptionState::Completed);
}

#[test]
fn gaps_are_never_transcribed_and_adjacent_speech_is_flagged() {
    let f = fixture(true, true);
    let runtime = FakeRuntime::new();
    let result = run(&f, &local(), &runtime);
    assert_eq!(result.recording_gaps.len(), 1);
    assert_eq!(result.segments.len(), 1);
    assert_eq!(result.segments[0].end_frame, u64::from(RATE) * 10);
    assert!(result.utterances.iter().all(|u| u.end_seconds <= 10.0));
    assert_eq!(result.utterances.len(), 2);
    assert!(
        !result.utterances[0]
            .flags
            .contains(&"nearRecordingGap".to_string())
    );
    let preflight = f
        .catalog
        .review_transcripts()
        .preflight(PROJECT, &f.session_id, &local(), &runtime)
        .unwrap();
    assert!(preflight.warnings.contains(&"recording_has_gaps"));
}

#[test]
fn unavailable_models_unfinalized_sessions_and_missing_credentials_are_blocked() {
    let f = fixture(false, false);
    let mut runtime = FakeRuntime::new();
    let transcripts = f.catalog.review_transcripts();
    let preflight = transcripts
        .preflight(PROJECT, &f.session_id, &local(), &runtime)
        .unwrap();
    assert_eq!(
        preflight.blockers,
        vec!["review_transcription_session_not_finalized"]
    );

    let f = fixture(false, true);
    let transcripts = f.catalog.review_transcripts();
    runtime.models = false;
    match transcripts.start(
        PROJECT,
        &f.session_id,
        &local(),
        &runtime,
        &f.catalog.jobs(),
        &actor(),
    ) {
        Err(error) => assert_eq!(error.code(), "review_transcription_local_model_unavailable"),
        Ok(_) => panic!("must not start without models"),
    }
    runtime.models = true;
    runtime.credentials = false;
    let mut unauthorized = cloud(1.0);
    unauthorized.cloud_upload_acknowledged = false;
    unauthorized.authorized_max_cost_usd = None;
    let preflight = transcripts
        .preflight(PROJECT, &f.session_id, &unauthorized, &runtime)
        .unwrap();
    assert_eq!(
        preflight.blockers,
        vec!["review_transcription_provider_unavailable"]
    );
    runtime.credentials = true;
    let mut unregistered = cloud(1.0);
    unregistered.model = Some("openai/gpt-4o".into());
    assert!(matches!(
        transcripts.preflight(PROJECT, &f.session_id, &unregistered, &runtime),
        Err(ReviewTranscriptError::InvalidInput(_))
    ));
    let mut cloudy_local = local();
    cloudy_local.authorized_max_cost_usd = Some(1.0);
    assert!(
        transcripts
            .preflight(PROJECT, &f.session_id, &cloudy_local, &runtime)
            .is_err()
    );
    assert_eq!(
        f.catalog
            .review_transcripts()
            .list(PROJECT, &f.session_id)
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn cloud_transcription_has_no_spending_or_checkbox_gate_and_reports_actual_cost() {
    let f = fixture(false, true);
    let runtime = FakeRuntime::new();
    let mut request = cloud(0.000001);
    request.authorized_max_cost_usd = None;
    request.cloud_upload_acknowledged = false;
    request.zero_data_retention = true; // Legacy input is ignored; default retention is used.
    request.model = Some("google/gemini-3.5-transcribe".into());
    let report = f
        .catalog
        .review_transcripts()
        .preflight(PROJECT, &f.session_id, &request, &runtime)
        .unwrap();
    assert!(report.ready);
    assert_eq!(report.estimate.unwrap().max_cost_usd, None);
    let transcripts = f.catalog.review_transcripts();
    let active = transcripts
        .start(
            PROJECT,
            &f.session_id,
            &request,
            &runtime,
            &f.catalog.jobs(),
            &actor(),
        )
        .unwrap();
    let duplicate = transcripts
        .start(
            PROJECT,
            &f.session_id,
            &request,
            &runtime,
            &f.catalog.jobs(),
            &actor(),
        )
        .unwrap();
    assert_eq!(active.run_id, duplicate.run_id);
    let result = run(&f, &request, &runtime);
    assert_eq!(result.state, ReviewTranscriptionState::Completed);
    assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.model, "google/gemini-3.5-transcribe");
    assert_eq!(result.spend.known_cost_usd, 0.001);
    assert_eq!(result.spend.authorized_max_usd, None);
    assert_eq!(result.spend.reserved_usd, 0.0);
    assert_eq!(result.spend.dispatched_windows, 1);
    assert_eq!(result.utterances[0].language, "zh");
    // Even a legacy low ceiling cannot prevent an explicitly requested run.
    let g = fixture(false, true);
    let runtime = FakeRuntime::new();
    assert_eq!(
        run(&g, &cloud(0.000001), &runtime).state,
        ReviewTranscriptionState::Completed
    );
}

#[test]
fn explicit_retry_of_a_lost_cloud_response_creates_a_fresh_attempt() {
    let f = fixture(false, true);
    let mut runtime = FakeRuntime::new();
    runtime.fail_on_call = Some(1);
    let first = run(&f, &cloud(1.0), &runtime);
    assert_eq!(first.state, ReviewTranscriptionState::Failed);
    assert_eq!(
        first.segments[0].error_code.as_deref(),
        Some("review_transcription_dispatch_outcome_unknown")
    );
    assert_eq!(first.spend.unknown_cost_windows, 1);
    assert_eq!(first.spend.reserved_usd, 0.0);
    assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);

    // Worker recovery stays idempotent; an explicit button click creates a new run.
    let transcripts = f.catalog.review_transcripts();
    let retried = transcripts
        .start(
            PROJECT,
            &f.session_id,
            &cloud(1.0),
            &runtime,
            &f.catalog.jobs(),
            &actor(),
        )
        .unwrap();
    assert_ne!(retried.run_id, first.run_id);
    assert_eq!(retried.attempt_count, 1);
    transcripts
        .execute(&retried.run_id, &runtime, &|| false)
        .unwrap();
    assert_eq!(runtime.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        transcripts.get(PROJECT, &first.run_id).unwrap().state,
        ReviewTranscriptionState::Failed
    );
}

#[test]
fn an_interrupted_run_resumes_from_cached_windows() {
    let f = fixture(false, true);
    let runtime = FakeRuntime::new();
    let transcripts = f.catalog.review_transcripts();
    let started = transcripts
        .start(
            PROJECT,
            &f.session_id,
            &cloud(1.0),
            &runtime,
            &f.catalog.jobs(),
            &actor(),
        )
        .unwrap();
    // Cancel before work: queued runs cancel immediately.
    let cancelled = transcripts
        .cancel(PROJECT, &started.run_id, &f.catalog.jobs())
        .unwrap();
    assert_eq!(cancelled.state, ReviewTranscriptionState::Cancelled);
    assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
    // Retry, complete once, then simulate a crash that lost the segment result.
    let retried = transcripts
        .start(
            PROJECT,
            &f.session_id,
            &cloud(1.0),
            &runtime,
            &f.catalog.jobs(),
            &actor(),
        )
        .unwrap();
    transcripts
        .execute(&retried.run_id, &runtime, &|| false)
        .unwrap();
    assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
    let connection = open_writable_connection(f.catalog.database_path()).unwrap();
    connection
        .execute("UPDATE review_transcription_segments SET state='pending', utterances_json=NULL WHERE run_id=?1", [&retried.run_id])
        .unwrap();
    connection
        .execute(
            "UPDATE review_transcription_runs SET state='running' WHERE run_id=?1",
            [&retried.run_id],
        )
        .unwrap();
    transcripts
        .execute(&retried.run_id, &runtime, &|| false)
        .unwrap();
    let after = transcripts.get(PROJECT, &retried.run_id).unwrap();
    assert_eq!(after.state, ReviewTranscriptionState::Completed);
    assert_eq!(after.utterances.len(), 1);
    assert_eq!(
        runtime.calls.load(Ordering::SeqCst),
        1,
        "cached window must not be resent"
    );
}

#[test]
fn cancellation_mid_run_stops_before_the_next_window() {
    let f = fixture(false, true);
    let runtime = FakeRuntime::new();
    let transcripts = f.catalog.review_transcripts();
    let started = transcripts
        .start(
            PROJECT,
            &f.session_id,
            &local(),
            &runtime,
            &f.catalog.jobs(),
            &actor(),
        )
        .unwrap();
    let state = transcripts
        .execute(&started.run_id, &runtime, &|| true)
        .unwrap();
    assert_eq!(state, ReviewTranscriptionState::Cancelled);
    assert!(
        transcripts
            .get(PROJECT, &started.run_id)
            .unwrap()
            .utterances
            .is_empty()
    );
}

#[test]
fn resampling_preserves_duration_and_in_band_tone() {
    let input: Vec<f32> = (0..48_000)
        .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48_000.0).sin() * 0.5)
        .collect();
    let output = resample_to(&input, 48_000, 16_000);
    assert_eq!(output.len(), 16_000);
    let rms = (output[1000..15000].iter().map(|s| s * s).sum::<f32>() / 14_000.0).sqrt();
    assert!((rms - 0.5 / 2f32.sqrt()).abs() < 0.02, "rms {rms}");
    let high: Vec<f32> = (0..48_000)
        .map(|i| (2.0 * std::f32::consts::PI * 15_000.0 * i as f32 / 48_000.0).sin() * 0.5)
        .collect();
    let aliased = resample_to(&high, 48_000, 16_000);
    let rms = (aliased[1000..15000].iter().map(|s| s * s).sum::<f32>() / 14_000.0).sqrt();
    assert!(
        rms < 0.05,
        "15 kHz must be filtered before decimation, rms {rms}"
    );
    assert_eq!(resample_to(&input, 16_000, 16_000).len(), 48_000);
}

#[test]
fn traditional_display_is_a_projection_that_never_rewrites_raw_text() {
    assert_eq!(
        script_and_display("hello"),
        ("latin".into(), "hello".into(), None)
    );
    assert_eq!(script_and_display("這個很好").0, "hanTraditional");
    let (script, display, projection) = script_and_display("这个镜头");
    assert_eq!(script, "hanSimplified");
    assert_eq!(display, "這個鏡頭");
    assert_eq!(projection.as_deref(), Some("openccS2tw"));
    assert_eq!(script_and_display("cut 这里").0, "mixed");
}

#[test]
fn unusable_cloud_output_keeps_reason_receipt_and_allows_explicit_retry() {
    for unknown_cost in [false, true] {
        let f = fixture(false, true);
        let mut runtime = FakeRuntime::new();
        runtime.unusable = Some(NativeSttProtocolError::InvalidTimestamp);
        runtime.unknown_cost = unknown_cost;
        let first = run(&f, &cloud(1.0), &runtime);
        assert_eq!(first.state, ReviewTranscriptionState::Failed);
        assert_eq!(
            first.segments[0].error_code.as_deref(),
            Some("review_transcription_provider_invalid_timestamp")
        );
        assert!(first.utterances.is_empty());
        assert_eq!(
            first.spend.known_cost_usd,
            if unknown_cost { 0.0 } else { 0.001 }
        );
        assert_eq!(first.spend.unknown_cost_windows, u32::from(unknown_cost));
        assert_eq!(first.spend.reserved_usd, 0.0);
        let connection = open_read_only_connection(f.catalog.database_path()).unwrap();
        let (generation, diagnostic): (String, String) = connection.query_row(
            "SELECT generation_id, result_json FROM review_transcription_windows WHERE run_id=?1",
            [&first.run_id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(generation, "gen-1");
        let diagnostic: serde_json::Value = serde_json::from_str(&diagnostic).unwrap();
        assert_eq!(diagnostic["unusable"], "invalid_timestamp");
        assert_eq!(
            first.provider_errors[0].message,
            "The selected provider rejected timestamp_granularities"
        );
        assert_eq!(first.provider_errors[0].http_status, Some(400));
        let transcripts = f.catalog.review_transcripts();
        let retried = transcripts
            .start(
                PROJECT,
                &f.session_id,
                &cloud(1.0),
                &runtime,
                &f.catalog.jobs(),
                &actor(),
            )
            .unwrap();
        transcripts
            .execute(&retried.run_id, &runtime, &|| false)
            .unwrap();
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 2);
        let after = transcripts.get(PROJECT, &first.run_id).unwrap();
        assert_eq!(after.segments[0].error_code, first.segments[0].error_code);
        assert_eq!(after.spend.reserved_usd, first.spend.reserved_usd);
    }
}
