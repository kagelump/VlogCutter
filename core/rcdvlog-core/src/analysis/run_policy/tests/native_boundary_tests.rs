//! Focused end-to-end regressions for the accounted native STT boundary.
//!
//! Every test uses synthetic PCM and a recording fake transport. No model is
//! loaded and no network call is made.

use super::*;
use crate::analysis::accounting::{
    AccountedNativeSttProvider, AnalysisAccountingContext, AnalysisBudgetAuthorityContext,
    AnalysisBudgetExhausted, AnalysisRequestLifecycle, analysis_native_stt_payload_fingerprint,
    analysis_policy_budget_scope_id,
};
use crate::analysis::openrouter_stt::{
    NativeSttAudioWindow, NativeSttCallResult, NativeSttProviderPolicy, NativeSttRequest,
    NativeSttTransport, OPENROUTER_STT_PROVIDER, OPENROUTER_WHISPER_MODEL, SttDataCollection,
    decode_native_stt_body, native_stt_request_cost_bound,
};
use crate::analysis::provider::{ProviderFailure, ProviderFailureCategory};
use crate::analysis::speech_audio::validate_wav_16k_mono;
use crate::analysis::windows::{SpeechRegion, pack_windows};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

fn native_policy_request(project: &str, asset: &str, budget: f64) -> RunPolicyRequest {
    RunPolicyRequest::native_speech_transcription(
        project,
        vec![asset.to_string()],
        OPENROUTER_STT_PROVIDER,
        OPENROUTER_WHISPER_MODEL,
        budget,
        Some("en".into()),
        NativeSttProviderPolicy::new(true, SttDataCollection::Deny),
        false,
    )
}

fn native_request() -> NativeSttRequest {
    NativeSttRequest::new(
        OPENROUTER_STT_PROVIDER,
        OPENROUTER_WHISPER_MODEL,
        Some("en".into()),
        0.0,
        NativeSttProviderPolicy::new(true, SttDataCollection::Deny),
    )
    .unwrap()
}

fn audio_window(source_start_ms: u32, duration_ms: u32) -> NativeSttAudioWindow {
    let source_end_ms = source_start_ms.checked_add(duration_ms).unwrap();
    let pcm = vec![0.1_f32; source_end_ms as usize * 16];
    let window = pack_windows(&[SpeechRegion {
        start_ms: source_start_ms,
        end_ms: source_end_ms,
    }])
    .remove(0);
    NativeSttAudioWindow::from_pcm(&pcm, &window).unwrap()
}

fn native_call(
    audio: &NativeSttAudioWindow,
    provider: &str,
    generation: &str,
    cost_usd: f64,
) -> NativeSttCallResult {
    let body = serde_json::to_vec(&json!({
        "text": "Synthetic native boundary transcript",
        "language": "en",
        "duration": audio.duration_ms() as f64 / 1_000.0,
        "segments": [{
            "id": 0,
            "start": 0.0,
            "end": 0.4,
            "text": "Synthetic native boundary transcript",
            "confidence": 0.9
        }],
        "model": OPENROUTER_WHISPER_MODEL,
        "provider": provider,
        "usage": {
            "cost": cost_usd,
            "seconds": audio.duration_ms() as f64 / 1_000.0,
            "input_tokens": 12,
            "output_tokens": 4
        }
    }))
    .unwrap();
    decode_native_stt_body(&body, audio.duration_ms(), Some(generation.into()), 47)
}

struct NativeFixture {
    _directory: tempfile::TempDir,
    catalog: Catalog,
    project: String,
    policy: RunPolicy,
    request: NativeSttRequest,
    context: AnalysisAccountingContext,
    run_id: String,
}

fn native_setup(budget_usd: f64) -> NativeFixture {
    let (directory, catalog, project, asset) = fixture();
    let policy_request = native_policy_request(&project, &asset, budget_usd);
    let policy = catalog.describe_run_policy(&policy_request).unwrap();
    assert!(
        policy.readiness.ready_for_authorization,
        "synthetic native policy must be fully authorizable: {:#?}",
        policy.readiness
    );
    assert!(!policy.readiness.ready_for_pilot);
    catalog.authorize_run_policy(&policy_request).unwrap();
    let authority = AnalysisBudgetAuthorityContext {
        project_id: policy.project_id.clone(),
        policy_id: policy.policy_id.clone(),
        policy_fingerprint: policy.policy_id.clone(),
        operation: "pilot".into(),
        provider: policy.provider.clone(),
        budget_scope_id: String::new(),
        budget_usd: policy.budget_usd,
    };
    catalog
        .initialize_analysis_accounting_authority(&authority)
        .unwrap();
    let run_id = catalog
        .begin_accounted_analysis_run(&policy.project_id, &policy.policy_id, "pilot")
        .unwrap();
    let scope = policy.scope.first().unwrap();
    let context = AnalysisAccountingContext {
        project_id: policy.project_id.clone(),
        policy_id: policy.policy_id.clone(),
        policy_fingerprint: policy.policy_id.clone(),
        run_id: run_id.clone(),
        operation: "pilot".into(),
        intended_attempt_key: format!("{}:{}", policy.kind.as_str(), scope.asset_id),
        source_fingerprint: scope.source_identity.clone(),
        provider: policy.provider.clone(),
        budget_scope_id: String::new(),
        budget_usd: policy.budget_usd,
        explicit_retry: false,
    };
    NativeFixture {
        _directory: directory,
        catalog,
        project,
        policy,
        request: native_request(),
        context,
        run_id,
    }
}

struct RecordingNativeTransport<'a> {
    catalog: &'a Catalog,
    run_id: &'a str,
    response: NativeSttCallResult,
    cancelled_on_send: Option<Arc<AtomicBool>>,
    prepare_calls: AtomicUsize,
    transcribe_calls: AtomicUsize,
    marker_seen_before_call: AtomicUsize,
}

impl<'a> RecordingNativeTransport<'a> {
    fn new(catalog: &'a Catalog, run_id: &'a str, response: NativeSttCallResult) -> Self {
        Self {
            catalog,
            run_id,
            response,
            cancelled_on_send: None,
            prepare_calls: AtomicUsize::new(0),
            transcribe_calls: AtomicUsize::new(0),
            marker_seen_before_call: AtomicUsize::new(0),
        }
    }

    fn cancelling_on_send(mut self, cancelled: Arc<AtomicBool>) -> Self {
        self.cancelled_on_send = Some(cancelled);
        self
    }
}

impl NativeSttTransport for RecordingNativeTransport<'_> {
    fn prepare(&self) -> Result<()> {
        self.prepare_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn transcribe(
        &self,
        _request: &NativeSttRequest,
        _audio: &NativeSttAudioWindow,
    ) -> Result<NativeSttCallResult> {
        self.transcribe_calls.fetch_add(1, Ordering::SeqCst);
        let lifecycle: String = self
            .catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT lifecycle FROM analysis_request_reservations
                 WHERE run_id=?1 ORDER BY created_at DESC LIMIT 1",
                [self.run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(lifecycle, "dispatched");
        self.marker_seen_before_call.fetch_add(1, Ordering::SeqCst);
        if let Some(cancelled) = &self.cancelled_on_send {
            cancelled.store(true, Ordering::SeqCst);
        }
        Ok(self.response.clone())
    }
}

#[test]
fn native_catalog_admission_rejects_a_bounded_window_mapped_past_the_approved_asset() {
    let setup = native_setup(1.0);
    let scope = setup.policy.scope.first().unwrap();
    let request = setup.request.clone();
    let audio = audio_window(scope.source_duration_ms + 500, 500);
    assert!(audio.window().regions[0].source_start_ms > scope.source_duration_ms);
    assert_eq!(audio.duration_ms(), 500);
    validate_wav_16k_mono(audio.wav_bytes()).unwrap();

    let error = setup
        .catalog
        .reserve_analysis_native_stt_request(&setup.context, &request, &audio)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("exceeds the authorized source duration"),
        "{error:#}"
    );

    assert!(
        setup
            .catalog
            .list_analysis_requests(&setup.project, Some(&setup.policy.policy_id), 10)
            .unwrap()
            .is_empty()
    );
    let events: i64 = setup
        .catalog
        .connection()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM analysis_request_events", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(events, 0);
    let scope_id = analysis_policy_budget_scope_id(&setup.project, &setup.policy.policy_id);
    let snapshot = setup
        .catalog
        .analysis_budget_snapshot(&scope_id)
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.active_requests, 0);
    assert_eq!(snapshot.confirmed_cost_usd, 0.0);
    assert_eq!(snapshot.available_usd, snapshot.budget_usd);
}

#[test]
fn native_adapters_race_final_whole_policy_budget_with_typed_exhaustion_and_one_send() {
    let sample_request = native_request();
    let sample_audio = audio_window(0, 500);
    let sample_bound = native_stt_request_cost_bound(&sample_request, &sample_audio).unwrap();
    let full_quote = sample_bound.maximum_cost_usd;
    let duration_price_usd_per_second = sample_bound
        .audio
        .as_ref()
        .expect("native fixture must carry duration pricing")
        .duration_price_usd_per_second;
    // Match the policy estimate's exact multiplication order for the two
    // pre-VAD windows: one bulk window plus one separate pilot window.
    let setup = native_setup(2.0_f64 * 30.0_f64 * duration_price_usd_per_second);

    let first_response = native_call(&sample_audio, "openai", "native-race-prior", full_quote);
    let first_transport =
        RecordingNativeTransport::new(&setup.catalog, &setup.run_id, first_response);
    let first_result = AccountedNativeSttProvider::new(
        &setup.catalog,
        &setup.policy,
        setup.context.clone(),
        &setup.request,
        &sample_audio,
        &first_transport,
    )
    .transmit()
    .unwrap();
    let first_record = setup
        .catalog
        .analysis_request(&first_result.request_key)
        .unwrap()
        .unwrap();
    assert_eq!(first_record.lifecycle, AnalysisRequestLifecycle::Completed);
    assert!((first_record.confirmed_cost_usd - full_quote).abs() < 1e-12);
    let scope_id = analysis_policy_budget_scope_id(&setup.project, &setup.policy.policy_id);
    let before_race = setup
        .catalog
        .analysis_budget_snapshot(&scope_id)
        .unwrap()
        .unwrap();
    assert!((before_race.available_usd - full_quote).abs() < 1e-12);

    let second_catalog = Catalog::open(setup._directory.path().join("catalog.sqlite3")).unwrap();
    let race_run_a = setup
        .catalog
        .begin_accounted_analysis_run(&setup.project, &setup.policy.policy_id, "pilot")
        .unwrap();
    let race_run_b = second_catalog
        .begin_accounted_analysis_run(&setup.project, &setup.policy.policy_id, "pilot")
        .unwrap();
    let audio_a = audio_window(0, 400);
    let audio_b = audio_window(500, 600);
    assert_ne!(
        analysis_native_stt_payload_fingerprint(&setup.request, &audio_a).unwrap(),
        analysis_native_stt_payload_fingerprint(&setup.request, &audio_b).unwrap()
    );

    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for (runner, run_id, audio) in [
        (setup.catalog.clone(), race_run_a, audio_a),
        (second_catalog, race_run_b, audio_b),
    ] {
        let barrier = Arc::clone(&barrier);
        let policy = setup.policy.clone();
        let request = setup.request.clone();
        let mut context = setup.context.clone();
        context.run_id = run_id.clone();
        handles.push(thread::spawn(move || {
            let response = native_call(
                &audio,
                "openai",
                &format!("native-race-{run_id}"),
                full_quote,
            );
            let transport = RecordingNativeTransport::new(&runner, &run_id, response);
            let adapter = AccountedNativeSttProvider::new(
                &runner, &policy, context, &request, &audio, &transport,
            );
            barrier.wait();
            let result = adapter.transmit();
            (
                result,
                transport.prepare_calls.load(Ordering::SeqCst),
                transport.transcribe_calls.load(Ordering::SeqCst),
            )
        }));
    }
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.0.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.0.is_err()).count(), 1);
    let winner = results
        .iter()
        .find(|result| result.0.is_ok())
        .expect("one racing adapter must win the final quote");
    let loser = results
        .iter()
        .find(|result| result.0.is_err())
        .expect("one racing adapter must be denied");
    assert!(
        loser
            .0
            .as_ref()
            .unwrap_err()
            .downcast_ref::<AnalysisBudgetExhausted>()
            .is_some(),
        "final-budget denial must remain typed: {:#}",
        loser.0.as_ref().unwrap_err()
    );
    assert_eq!(winner.1, 1);
    assert_eq!(winner.2, 1);
    assert_eq!(loser.1, 0);
    assert_eq!(loser.2, 0);

    let winner_key = &winner.0.as_ref().unwrap().request_key;
    let winner_record = setup.catalog.analysis_request(winner_key).unwrap().unwrap();
    assert_eq!(winner_record.lifecycle, AnalysisRequestLifecycle::Completed);
    assert!((winner_record.reserved_cost_usd - full_quote).abs() < 1e-12);
    let records = setup
        .catalog
        .list_analysis_requests(&setup.project, Some(&setup.policy.policy_id), 10)
        .unwrap();
    assert_eq!(records.len(), 2, "only the prior call and one race winner");
    let after_race = setup
        .catalog
        .analysis_budget_snapshot(&scope_id)
        .unwrap()
        .unwrap();
    assert_eq!(after_race.active_requests, 0);
    assert!((after_race.confirmed_cost_usd - full_quote * 2.0).abs() < 1e-12);
    assert!(after_race.confirmed_cost_usd <= after_race.budget_usd + 1e-12);
}

#[test]
fn native_returned_cost_above_bound_is_confirmed_durably_stops_and_blocks_another_window() {
    let setup = native_setup(1.0);
    let quote = native_stt_request_cost_bound(&setup.request, &audio_window(0, 500))
        .unwrap()
        .maximum_cost_usd;
    let external_invoice = quote + 0.02;
    let first_audio = audio_window(0, 500);
    let first_response = native_call(
        &first_audio,
        "openai",
        "native-overrun-first",
        external_invoice,
    );
    let first_transport =
        RecordingNativeTransport::new(&setup.catalog, &setup.run_id, first_response);
    let first_result = AccountedNativeSttProvider::new(
        &setup.catalog,
        &setup.policy,
        setup.context.clone(),
        &setup.request,
        &first_audio,
        &first_transport,
    )
    .transmit()
    .unwrap();
    let first_record = setup
        .catalog
        .analysis_request(&first_result.request_key)
        .unwrap()
        .unwrap();
    assert_eq!(first_record.lifecycle, AnalysisRequestLifecycle::Completed);
    assert!((first_record.reserved_cost_usd - quote).abs() < 1e-12);
    assert!((first_record.confirmed_cost_usd - external_invoice).abs() < 1e-12);
    assert_eq!(first_record.reported_cost_usd, Some(external_invoice));
    assert_eq!(
        setup
            .catalog
            .analysis_provider_receipts(&first_result.request_key)
            .unwrap()
            .len(),
        1
    );
    let overrun_events = setup
        .catalog
        .analysis_request_events(&first_result.request_key)
        .unwrap();
    let overrun = overrun_events
        .iter()
        .find(|event| event.event_kind == "overrun")
        .expect("external overrun must remain durable evidence");
    assert_eq!(
        overrun.detail["reportedCostUsd"].as_f64(),
        Some(external_invoice)
    );

    let scope_id = analysis_policy_budget_scope_id(&setup.project, &setup.policy.policy_id);
    let stopped = setup
        .catalog
        .analysis_budget_snapshot(&scope_id)
        .unwrap()
        .unwrap();
    assert!(stopped.dispatch_stopped);
    assert_eq!(stopped.stop_reason, "reported_cost_exceeded_reservation");

    let reopened = Catalog::open(setup._directory.path().join("catalog.sqlite3")).unwrap();
    let reopened_snapshot = reopened
        .analysis_budget_snapshot(&scope_id)
        .unwrap()
        .unwrap();
    assert!(reopened_snapshot.dispatch_stopped);
    assert!((reopened_snapshot.confirmed_cost_usd - external_invoice).abs() < 1e-12);

    let new_run = reopened
        .begin_accounted_analysis_run(&setup.project, &setup.policy.policy_id, "pilot")
        .unwrap();
    let mut next_context = setup.context.clone();
    next_context.run_id = new_run.clone();
    let second_audio = audio_window(1_000, 500);
    let second_response = native_call(&second_audio, "openai", "native-overrun-second", quote);
    let second_transport = RecordingNativeTransport::new(&reopened, &new_run, second_response);
    let error = AccountedNativeSttProvider::new(
        &reopened,
        &setup.policy,
        next_context,
        &setup.request,
        &second_audio,
        &second_transport,
    )
    .transmit()
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("dispatch is stopped"),
        "{error:#}"
    );
    assert_eq!(second_transport.prepare_calls.load(Ordering::SeqCst), 0);
    assert_eq!(second_transport.transcribe_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        reopened
            .list_analysis_requests(&setup.project, Some(&setup.policy.policy_id), 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn native_cancellation_inside_send_still_retains_the_paid_receipt_and_never_repeats() {
    let setup = native_setup(1.0);
    let audio = audio_window(0, 500);
    let cost = native_stt_request_cost_bound(&setup.request, &audio)
        .unwrap()
        .maximum_cost_usd;
    let cancelled = Arc::new(AtomicBool::new(false));
    let response = native_call(&audio, "openai", "native-cancel-inside-send", cost);
    let transport = RecordingNativeTransport::new(&setup.catalog, &setup.run_id, response)
        .cancelling_on_send(Arc::clone(&cancelled));
    let result = AccountedNativeSttProvider::new(
        &setup.catalog,
        &setup.policy,
        setup.context.clone(),
        &setup.request,
        &audio,
        &transport,
    )
    .transmit_with_cancel(|| cancelled.load(Ordering::SeqCst))
    .unwrap();
    assert!(cancelled.load(Ordering::SeqCst));
    assert_eq!(transport.marker_seen_before_call.load(Ordering::SeqCst), 1);
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 1);
    let record = setup
        .catalog
        .analysis_request(&result.request_key)
        .unwrap()
        .unwrap();
    assert_eq!(record.lifecycle, AnalysisRequestLifecycle::Completed);
    assert!(record.dispatch_at.is_some());
    assert!((record.confirmed_cost_usd - cost).abs() < 1e-12);
    assert_eq!(
        setup
            .catalog
            .analysis_provider_receipts(&result.request_key)
            .unwrap()
            .len(),
        1
    );
    assert!(
        setup
            .catalog
            .analysis_request_events(&result.request_key)
            .unwrap()
            .iter()
            .all(|event| event.event_kind != "released"),
        "a post-marker paid response must not be locally released"
    );

    let new_run = setup
        .catalog
        .begin_accounted_analysis_run(&setup.project, &setup.policy.policy_id, "pilot")
        .unwrap();
    let mut repeat_context = setup.context.clone();
    repeat_context.run_id = new_run.clone();
    let repeat_transport = RecordingNativeTransport::new(
        &setup.catalog,
        &new_run,
        native_call(&audio, "openai", "native-cancel-repeat", cost),
    );
    let error = AccountedNativeSttProvider::new(
        &setup.catalog,
        &setup.policy,
        repeat_context,
        &setup.request,
        &audio,
        &repeat_transport,
    )
    .transmit()
    .unwrap_err();
    assert!(format!("{error:#}").contains("completed provider receipt"));
    assert_eq!(repeat_transport.prepare_calls.load(Ordering::SeqCst), 0);
    assert_eq!(repeat_transport.transcribe_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn native_receipt_persistence_failure_is_uncertain_one_call_and_reopen_never_replays() {
    let setup = native_setup(1.0);
    setup
        .catalog
        .connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER synthetic_native_receipt_persist_failure
             BEFORE INSERT ON analysis_provider_receipts
             WHEN NEW.provider='persist-fail-native'
             BEGIN
                 SELECT RAISE(ABORT, 'synthetic native receipt persistence failure');
             END;",
        )
        .unwrap();
    let audio = audio_window(0, 500);
    let cost = native_stt_request_cost_bound(&setup.request, &audio)
        .unwrap()
        .maximum_cost_usd;
    let transport = RecordingNativeTransport::new(
        &setup.catalog,
        &setup.run_id,
        native_call(
            &audio,
            "persist-fail-native",
            "native-persist-failure",
            cost,
        ),
    );
    let error = AccountedNativeSttProvider::new(
        &setup.catalog,
        &setup.policy,
        setup.context.clone(),
        &setup.request,
        &audio,
        &transport,
    )
    .transmit()
    .unwrap_err();
    assert_eq!(
        ProviderFailure::from_anyhow(&error).category,
        ProviderFailureCategory::LocalPostDispatch
    );
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 1);
    let requests = setup
        .catalog
        .list_analysis_requests(&setup.project, Some(&setup.policy.policy_id), 10)
        .unwrap();
    assert_eq!(requests.len(), 1);
    let uncertain = &requests[0];
    assert_eq!(uncertain.lifecycle, AnalysisRequestLifecycle::Uncertain);
    assert!(uncertain.dispatch_at.is_some());
    assert_eq!(uncertain.confirmed_cost_usd, 0.0);
    assert!(uncertain.uncertain_usage_reserve_usd >= uncertain.reserved_cost_usd);
    assert!(
        setup
            .catalog
            .analysis_provider_receipts(&uncertain.request_key)
            .unwrap()
            .is_empty()
    );

    let reopened = Catalog::open(setup._directory.path().join("catalog.sqlite3")).unwrap();
    let new_run = reopened
        .begin_accounted_analysis_run(&setup.project, &setup.policy.policy_id, "pilot")
        .unwrap();
    let mut repeat_context = setup.context.clone();
    repeat_context.run_id = new_run.clone();
    let repeat_transport = RecordingNativeTransport::new(
        &reopened,
        &new_run,
        native_call(&audio, "persist-fail-native", "native-persist-repeat", cost),
    );
    let error = AccountedNativeSttProvider::new(
        &reopened,
        &setup.policy,
        repeat_context,
        &setup.request,
        &audio,
        &repeat_transport,
    )
    .transmit()
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("uncertain"),
        "default replay must be blocked by durable uncertainty: {error:#}"
    );
    assert_eq!(repeat_transport.prepare_calls.load(Ordering::SeqCst), 0);
    assert_eq!(repeat_transport.transcribe_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        reopened
            .list_analysis_requests(&setup.project, Some(&setup.policy.policy_id), 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn native_cancelled_before_admission_does_not_read_authorization_or_prepare_transport() {
    let setup = native_setup(1.0);
    let audio = audio_window(0, 500);
    let cost = native_stt_request_cost_bound(&setup.request, &audio)
        .unwrap()
        .maximum_cost_usd;
    let transport = RecordingNativeTransport::new(
        &setup.catalog,
        &setup.run_id,
        native_call(&audio, "openai", "cancel-before-admission", cost),
    );
    // This identity has no authorization. An already-cancelled request must
    // stop before reading it, rather than returning an authorization failure.
    let mut unavailable_policy = setup.policy.clone();
    unavailable_policy.policy_id = "unavailable-native-policy".into();
    let mut context = setup.context.clone();
    context.policy_id = unavailable_policy.policy_id.clone();
    context.policy_fingerprint = unavailable_policy.policy_id.clone();
    let error = AccountedNativeSttProvider::new(
        &setup.catalog,
        &unavailable_policy,
        context,
        &setup.request,
        &audio,
        &transport,
    )
    .transmit_with_cancel(|| true)
    .unwrap_err();
    assert_eq!(
        ProviderFailure::from_anyhow(&error).category,
        ProviderFailureCategory::Cancelled
    );
    assert_eq!(transport.prepare_calls.load(Ordering::SeqCst), 0);
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 0);
    assert!(
        setup
            .catalog
            .list_analysis_requests(&setup.project, Some(&setup.policy.policy_id), 10)
            .unwrap()
            .is_empty()
    );
}
