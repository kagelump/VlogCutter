//! Focused C7 native pilot-evidence tests. Synthetic audio and a fake native
//! transport are used exclusively; no live STT or model load occurs.

use super::*;
use crate::NativePilotEvidence;
use crate::NativePilotLocalOutcome;
use crate::NativePilotLocalOutcomeFact;
use crate::NativePilotPaidSample;
use crate::NativePilotSampleOutcome;
use crate::ProviderReceiptEvidence;
use crate::RepresentationKind;
use crate::analysis::accounting::{
    AccountedNativeSttProvider, AnalysisRequestReconciliation, NativeSttAccountedResult,
};
use crate::analysis::openrouter_stt::{
    NativeSttCallResult, NativeSttCost, NativeSttOutcome, NativeSttReceipt, NativeSttUsage,
};
use crate::analysis::quality::native_stt::NativeSttQualityDecision;
use crate::transcription::{NativeSpeechPreparation, NativeSpeechPreparationEvidence};
use std::path::PathBuf;

fn running_pilot(policy: &RunPolicy, run_id: &str, asset_id: &str) -> RunPilotRecord {
    RunPilotRecord {
        schema_version: RUN_PILOT_SCHEMA_VERSION.into(),
        policy_id: policy.policy_id.clone(),
        project_id: policy.project_id.clone(),
        status: "running".into(),
        started_at: "2026-01-01T00:00:00Z".into(),
        finished_at: String::new(),
        run_id: run_id.into(),
        asset_id: asset_id.into(),
        requested_model: policy.primary_model.clone(),
        actual_model: None,
        provider: None,
        generation_id: None,
        attempted_models: Vec::new(),
        attempts: 0,
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
    }
}

pub(super) fn install_running_pilot(
    catalog: &Catalog,
    policy: &RunPolicy,
    run_id: &str,
    asset_id: &str,
) -> RunPilotRecord {
    let record = running_pilot(policy, run_id, asset_id);
    catalog.save_pilot(&record).unwrap();
    record
}

fn scope_for(policy: &RunPolicy, asset_id: &str) -> RunAssetScope {
    policy
        .scope
        .iter()
        .find(|scope| scope.asset_id == asset_id)
        .unwrap()
        .clone()
}

fn native_result_from_call<'a>(
    catalog: &'a Catalog,
    policy: &RunPolicy,
    request: &NativeSttRequest,
    audio: &NativeSttAudioWindow,
    run_id: &str,
    context: AnalysisAccountingContext,
    call: NativeSttCallResult,
) -> (NativeSttAccountedResult, CountingNativeTransport<'a>) {
    let transport =
        CountingNativeTransport::new(catalog, run_id, FakeNativeResponse::Ok(Box::new(call)));
    let result =
        AccountedNativeSttProvider::new(catalog, policy, context, request, audio, &transport)
            .transmit()
            .unwrap();
    (result, transport)
}

fn paid_sample(
    policy: &RunPolicy,
    request: &NativeSttRequest,
    audio: &NativeSttAudioWindow,
    result: &NativeSttAccountedResult,
) -> NativePilotPaidSample {
    let scope = scope_for(policy, &policy.scope[0].asset_id);
    NativePilotPaidSample::from_accounted_result(policy, &scope, request, audio, result).unwrap()
}

fn test_preparation(
    catalog: &Catalog,
    policy: &RunPolicy,
    asset_id: &str,
) -> NativeSpeechPreparation {
    let scope = scope_for(policy, asset_id);
    let asset = catalog.get_asset(asset_id).unwrap();
    let proxy_path = PathBuf::from(asset.proxy_path.as_deref().unwrap());
    let proxy_claim = catalog
        .try_claim_representation(asset_id, RepresentationKind::Proxy)
        .unwrap()
        .unwrap();
    let speech_claim = catalog
        .try_claim_representation(asset_id, RepresentationKind::Speech)
        .unwrap()
        .unwrap();
    let evidence = NativeSpeechPreparationEvidence {
        policy_id: policy.policy_id.clone(),
        project_id: policy.project_id.clone(),
        asset_id: asset_id.into(),
        provider: policy.provider.clone(),
        model: policy.primary_model.clone(),
        recipe: policy.recipe_version.clone(),
        scope: scope.clone(),
        proxy_full_fingerprint: scope.representation_fingerprint.clone().unwrap(),
        proxy_sampled_fingerprint: scope.sampled_representation_fingerprint.clone().unwrap(),
        source_duration_ms: scope.source_duration_ms,
        last_safe_sample_ms: scope.source_duration_ms,
        vad: None,
    };
    NativeSpeechPreparation::test_preparation(evidence, proxy_path, proxy_claim, speech_claim)
        .test_with_speech(
            vec![0.1_f32; 16_000],
            vec![native_audio_window().window().clone()],
        )
}

#[test]
fn known_cost_native_sample_roundtrips_and_never_creates_full_asset_transcript() {
    let (_directory, catalog, _project, asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let (result, _transport) = native_result_from_call(
        &catalog,
        &policy,
        &request,
        &audio,
        &run_id,
        context,
        valid_native_call(&audio, true),
    );
    let request_record = catalog
        .analysis_request(&result.request_key)
        .unwrap()
        .unwrap();
    assert_eq!(
        request_record.lifecycle,
        AnalysisRequestLifecycle::Completed
    );
    assert!(request_record.confirmed_cost_usd > 0.0);
    let sample = paid_sample(&policy, &request, &audio, &result);
    assert_eq!(
        sample.coverage,
        crate::NativePilotSampleCoverage::PartialWindow
    );
    let running = install_running_pilot(&catalog, &policy, &run_id, &asset);
    let attached = catalog
        .attach_native_pilot_evidence(
            &running,
            NativePilotEvidence::with_sample(sample.clone()),
            Some((&request, &audio)),
        )
        .unwrap()
        .expect("current running pilot must accept native evidence");

    assert_eq!(attached.status, "running");
    assert_eq!(attached.cost_usd, running.cost_usd);
    assert_eq!(attached.confirmed_cost_usd, running.confirmed_cost_usd);
    assert_eq!(attached.validation, running.validation);
    let loaded = catalog.load_run_pilot(&policy.policy_id).unwrap().unwrap();
    assert_eq!(
        loaded.native_evidence.as_ref().unwrap().sample.as_ref(),
        Some(&sample)
    );
    let json = serde_json::to_string(&loaded).unwrap();
    assert!(json.contains("nativeEvidence"));
    assert!(!json.contains("input_audio"));
    assert!(!json.contains("wavBytes"));
    assert!(!json.contains("base64"));
    assert!(!json.contains("confirmedCostUsd\":0.002"));
    assert!(
        catalog.asset_transcript(&asset).unwrap().is_none(),
        "a partial pilot window must not become canonical full-asset speech"
    );
    let transcript_rows: i64 = catalog
        .connection()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM asset_transcripts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(transcript_rows, 0);
}

#[test]
fn unknown_cost_sample_is_cached_but_financial_reconciliation_does_not_invent_a_pass() {
    let (_directory, catalog, _project, asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let call = valid_native_call(&audio, false);
    let receipt = call.receipt.clone();
    let (result, _transport) =
        native_result_from_call(&catalog, &policy, &request, &audio, &run_id, context, call);
    let unknown_request = catalog
        .analysis_request(&result.request_key)
        .unwrap()
        .unwrap();
    assert_eq!(
        unknown_request.lifecycle,
        AnalysisRequestLifecycle::Uncertain
    );
    let sample = paid_sample(&policy, &request, &audio, &result);
    let running = install_running_pilot(&catalog, &policy, &run_id, &asset);
    let attached = catalog
        .attach_native_pilot_evidence(
            &running,
            NativePilotEvidence::with_sample(sample.clone()),
            Some((&request, &audio)),
        )
        .unwrap()
        .unwrap();
    assert_eq!(attached.status, "running");
    assert_eq!(attached.confirmed_cost_usd, 0.0);
    assert_eq!(attached.uncertain_usage_reserve_usd, 0.0);

    let known_receipt = NativeSttReceipt {
        usage: NativeSttUsage {
            cost: NativeSttCost::Known(0.002),
            ..receipt.usage
        },
        ..receipt
    };
    let evidence = ProviderReceiptEvidence::from_native_stt_receipt(&known_receipt).unwrap();
    catalog
        .reconcile_analysis_request(
            &result.request_key,
            &AnalysisRequestReconciliation::LateReceipt { receipt: evidence },
        )
        .unwrap();
    let settled = catalog
        .analysis_request(&result.request_key)
        .unwrap()
        .unwrap();
    assert_eq!(settled.lifecycle, AnalysisRequestLifecycle::Completed);
    assert!((settled.confirmed_cost_usd - 0.002).abs() < 1e-12);
    let reconciled_pilot = catalog.load_run_pilot(&policy.policy_id).unwrap().unwrap();
    let cached = reconciled_pilot
        .native_evidence
        .as_ref()
        .unwrap()
        .sample
        .as_ref()
        .unwrap();
    assert_eq!(cached, &sample);
    assert!(!reconciled_pilot.validation.capability_observed);
    assert_ne!(reconciled_pilot.status, "passed");
}

#[test]
fn review_candidates_are_unaccepted_and_rejected_text_is_discarded() {
    let (_directory, catalog, _project, asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let review_call = valid_native_call(&audio, true);
    let (review_result, _transport) = native_result_from_call(
        &catalog,
        &policy,
        &request,
        &audio,
        &run_id,
        context,
        review_call,
    );
    let review_sample = paid_sample(&policy, &request, &audio, &review_result);
    match &review_sample.outcome {
        NativePilotSampleOutcome::Evaluated {
            quality,
            candidates,
            ..
        } => {
            assert_eq!(quality.decision, NativeSttQualityDecision::ReviewRequired);
            assert!(!candidates.is_empty());
            assert!(
                candidates
                    .iter()
                    .all(|candidate| candidate.mean_token_probability.is_none())
            );
        }
        other => panic!("expected evaluated review outcome, got {other:?}"),
    }
    let running = install_running_pilot(&catalog, &policy, &run_id, &asset);
    catalog
        .attach_native_pilot_evidence(
            &running,
            NativePilotEvidence::with_sample(review_sample.clone()),
            Some((&request, &audio)),
        )
        .unwrap()
        .unwrap();
    assert!(
        !review_sample
            .as_window_observation()
            .unwrap()
            .quality
            .decision
            .eq(&NativeSttQualityDecision::Accepted)
    );

    let (_directory2, catalog2, _project2, asset2, policy2, request2, audio2, run_id2, context2) =
        authorized_native_setup();
    let mut low_call = valid_native_call(&audio2, true);
    let sentinel = "REJECTED_NATIVE_TEXT_MUST_NOT_LEAK";
    if let NativeSttOutcome::Usable(transcript) = &mut low_call.outcome {
        transcript.text = sentinel.into();
        transcript.segments[0].text = sentinel.into();
        transcript.segments[0].confidence = Some(0.01);
    }
    let (low_result, _transport) = native_result_from_call(
        &catalog2, &policy2, &request2, &audio2, &run_id2, context2, low_call,
    );
    let low_sample = paid_sample(&policy2, &request2, &audio2, &low_result);
    match &low_sample.outcome {
        NativePilotSampleOutcome::Evaluated {
            quality,
            candidates,
            ..
        } => {
            assert_eq!(quality.decision, NativeSttQualityDecision::Rejected);
            assert!(candidates.is_empty());
        }
        other => panic!("expected rejected evaluation, got {other:?}"),
    }
    assert!(
        !serde_json::to_string(&low_sample)
            .unwrap()
            .contains(sentinel)
    );
    let running2 = install_running_pilot(&catalog2, &policy2, &run_id2, &asset2);
    catalog2
        .attach_native_pilot_evidence(
            &running2,
            NativePilotEvidence::with_sample(low_sample),
            Some((&request2, &audio2)),
        )
        .unwrap()
        .unwrap();
}

#[test]
fn typed_protocol_unusable_sample_is_body_free_and_cached() {
    let (_directory, catalog, _project, asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let canary = "PRIVATE_PROVIDER_BODY_CANARY";
    let call = decode_native_stt_body(
        canary.as_bytes(),
        audio.duration_ms(),
        Some("native-malformed-generation".into()),
        31,
    );
    assert!(matches!(
        call.outcome,
        NativeSttOutcome::Unusable(crate::NativeSttProtocolError::InvalidJson)
    ));
    let (result, _transport) =
        native_result_from_call(&catalog, &policy, &request, &audio, &run_id, context, call);
    let sample = paid_sample(&policy, &request, &audio, &result);
    assert!(matches!(
        sample.outcome,
        NativePilotSampleOutcome::ProtocolUnusable {
            protocol_error: crate::NativeSttProtocolError::InvalidJson
        }
    ));
    assert!(sample.as_window_observation().is_none());
    let running = install_running_pilot(&catalog, &policy, &run_id, &asset);
    catalog
        .attach_native_pilot_evidence(
            &running,
            NativePilotEvidence::with_sample(sample),
            Some((&request, &audio)),
        )
        .unwrap()
        .unwrap();
    let loaded = catalog.load_run_pilot(&policy.policy_id).unwrap().unwrap();
    assert!(!serde_json::to_string(&loaded).unwrap().contains(canary));
}

#[test]
fn local_only_native_evidence_records_truthful_facts_without_a_paid_request() {
    let (_directory, catalog, _project, asset, policy, request, audio, run_id, _context) =
        authorized_native_setup();
    let scope = scope_for(&policy, &asset);
    let fact = NativePilotLocalOutcomeFact {
        scope,
        recipe_version: policy.recipe_version.clone(),
        outcome: NativePilotLocalOutcome::NoSpeech,
    };
    let running = install_running_pilot(&catalog, &policy, &run_id, &asset);
    let attached = catalog
        .attach_native_pilot_evidence(
            &running,
            NativePilotEvidence::with_local_outcomes(vec![fact.clone()]),
            None,
        )
        .unwrap()
        .unwrap();
    let evidence = attached.native_evidence.unwrap();
    assert!(evidence.sample.is_none());
    assert_eq!(evidence.local_outcomes, vec![fact]);
    assert!(
        catalog
            .list_analysis_requests(&policy.project_id, Some(&policy.policy_id), 10)
            .unwrap()
            .is_empty(),
        "local-only evidence must not invent a paid request"
    );
    assert!(catalog.asset_transcript(&asset).unwrap().is_none());
    // The request/audio are deliberately unused for local-only evidence.
    let _ = (&request, &audio);
}

#[test]
fn stale_payload_or_window_cannot_be_attached_and_preserves_previous_record() {
    let (_directory, catalog, _project, asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let (result, _transport) = native_result_from_call(
        &catalog,
        &policy,
        &request,
        &audio,
        &run_id,
        context,
        valid_native_call(&audio, true),
    );
    let sample = paid_sample(&policy, &request, &audio, &result);
    let running = install_running_pilot(&catalog, &policy, &run_id, &asset);
    let changed_window = pack_windows(&[SpeechRegion {
        start_ms: 0,
        end_ms: 500,
    }])
    .remove(0);
    let pcm = vec![0.1_f32; 16_000];
    let changed_audio = NativeSttAudioWindow::from_pcm(&pcm, &changed_window).unwrap();
    let error = catalog
        .attach_native_pilot_evidence(
            &running,
            NativePilotEvidence::with_sample(sample),
            Some((&request, &changed_audio)),
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("window") || format!("{error:#}").contains("payload"),
        "{error:#}"
    );
    let unchanged = catalog.load_run_pilot(&policy.policy_id).unwrap().unwrap();
    assert_eq!(unchanged, running);
}

#[test]
fn released_or_undispatched_request_without_receipt_is_denied() {
    let (_directory, catalog, _project, asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let reserved = catalog
        .reserve_analysis_native_stt_request(&context, &request, &audio)
        .unwrap();
    assert_eq!(reserved.lifecycle, AnalysisRequestLifecycle::Reserved);
    let call = valid_native_call(&audio, true);
    let fake_result = NativeSttAccountedResult {
        request_key: reserved.request_key.clone(),
        call,
    };
    let sample = paid_sample(&policy, &request, &audio, &fake_result);
    let running = install_running_pilot(&catalog, &policy, &run_id, &asset);
    let error = catalog
        .attach_native_pilot_evidence(
            &running,
            NativePilotEvidence::with_sample(sample),
            Some((&request, &audio)),
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("undispatched")
            || format!("{error:#}").contains("receipt")
            || format!("{error:#}").contains("settled"),
        "{error:#}"
    );
    assert_eq!(
        catalog.load_run_pilot(&policy.policy_id).unwrap().unwrap(),
        running
    );
}

#[test]
fn foreign_run_excluded_source_and_duplicate_sample_are_denied_without_overwrite() {
    let (_directory, catalog, _project, asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let (result, _transport) = native_result_from_call(
        &catalog,
        &policy,
        &request,
        &audio,
        &run_id,
        context,
        valid_native_call(&audio, true),
    );
    let sample = paid_sample(&policy, &request, &audio, &result);
    let second_run = catalog
        .begin_accounted_analysis_run(&policy.project_id, &policy.policy_id, "pilot")
        .unwrap();
    let second_running = install_running_pilot(&catalog, &policy, &second_run, &asset);
    let error = catalog
        .attach_native_pilot_evidence(
            &second_running,
            NativePilotEvidence::with_sample(sample.clone()),
            Some((&request, &audio)),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("run"), "{error:#}");

    let running = install_running_pilot(&catalog, &policy, &run_id, &asset);
    let mut foreign_scope = scope_for(&policy, &asset);
    foreign_scope.source_identity = "foreign-source-identity".into();
    let fact = NativePilotLocalOutcomeFact {
        scope: foreign_scope,
        recipe_version: policy.recipe_version.clone(),
        outcome: NativePilotLocalOutcome::NoAudio,
    };
    assert!(
        catalog
            .attach_native_pilot_evidence(
                &running,
                NativePilotEvidence::with_local_outcomes(vec![fact]),
                None,
            )
            .is_err()
    );
    assert_eq!(
        catalog.load_run_pilot(&policy.policy_id).unwrap().unwrap(),
        running
    );

    let attached = catalog
        .attach_native_pilot_evidence(
            &running,
            NativePilotEvidence::with_sample(sample.clone()),
            Some((&request, &audio)),
        )
        .unwrap()
        .unwrap();
    let duplicate_error = catalog
        .attach_native_pilot_evidence(
            &attached,
            NativePilotEvidence::with_sample(sample),
            Some((&request, &audio)),
        )
        .unwrap_err();
    assert!(format!("{duplicate_error:#}").contains("one bounded paid sample"));

    catalog
        .connection()
        .unwrap()
        .execute("UPDATE assets SET removed=1 WHERE id=?1", [&asset])
        .unwrap();
    let excluded_running = catalog.load_run_pilot(&policy.policy_id).unwrap().unwrap();
    let excluded_fact = NativePilotLocalOutcomeFact {
        scope: scope_for(&policy, &asset),
        recipe_version: policy.recipe_version.clone(),
        outcome: NativePilotLocalOutcome::NoAudio,
    };
    assert!(
        catalog
            .attach_native_pilot_evidence(
                &excluded_running,
                NativePilotEvidence::with_local_outcomes(vec![excluded_fact]),
                None,
            )
            .is_err()
    );
}

#[test]
fn stale_cas_trigger_rollback_and_legacy_vlm_json_are_safe() {
    let (_directory, catalog, _project, asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let (result, _transport) = native_result_from_call(
        &catalog,
        &policy,
        &request,
        &audio,
        &run_id,
        context,
        valid_native_call(&audio, true),
    );
    let sample = paid_sample(&policy, &request, &audio, &result);
    let running = install_running_pilot(&catalog, &policy, &run_id, &asset);
    let mut newer = running.clone();
    newer.status = "failed".into();
    newer.finished_at = "2026-01-01T00:00:10Z".into();
    newer.failure_code = "newer_owner".into();
    catalog.save_pilot(&newer).unwrap();
    assert!(
        catalog
            .attach_native_pilot_evidence(
                &running,
                NativePilotEvidence::with_sample(sample.clone()),
                Some((&request, &audio)),
            )
            .unwrap()
            .is_none(),
        "stale expected record must not replace a newer pilot"
    );
    assert_eq!(
        catalog.load_run_pilot(&policy.policy_id).unwrap().unwrap(),
        newer
    );

    let running = install_running_pilot(&catalog, &policy, &run_id, &asset);
    catalog
        .connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER native_pilot_evidence_abort BEFORE UPDATE ON analysis_run_pilots
             BEGIN SELECT RAISE(ABORT, 'synthetic evidence rollback'); END;",
        )
        .unwrap();
    assert!(
        catalog
            .attach_native_pilot_evidence(
                &running,
                NativePilotEvidence::with_sample(sample),
                Some((&request, &audio)),
            )
            .is_err()
    );
    catalog
        .connection()
        .unwrap()
        .execute_batch("DROP TRIGGER native_pilot_evidence_abort;")
        .unwrap();
    assert_eq!(
        catalog.load_run_pilot(&policy.policy_id).unwrap().unwrap(),
        running
    );

    let legacy_json = serde_json::to_value(&running).unwrap();
    assert!(legacy_json.get("nativeEvidence").is_none());
    let mut without_field = legacy_json.clone();
    without_field
        .as_object_mut()
        .unwrap()
        .remove("nativeEvidence");
    let decoded: RunPilotRecord = serde_json::from_value(without_field).unwrap();
    assert!(decoded.native_evidence.is_none());
    assert_eq!(serde_json::to_value(&decoded).unwrap(), legacy_json);
}

#[test]
fn reusable_sample_requires_exact_current_source_recipe_request_and_map() {
    let (_directory, catalog, _project, asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let (result, transport) = native_result_from_call(
        &catalog,
        &policy,
        &request,
        &audio,
        &run_id,
        context,
        valid_native_call(&audio, true),
    );
    let sample = paid_sample(&policy, &request, &audio, &result);
    let running = install_running_pilot(&catalog, &policy, &run_id, &asset);
    catalog
        .attach_native_pilot_evidence(
            &running,
            NativePilotEvidence::with_sample(sample.clone()),
            Some((&request, &audio)),
        )
        .unwrap()
        .unwrap();
    let transcribe_calls = transport.transcribe_calls.load(Ordering::SeqCst);

    let preparation = test_preparation(&catalog, &policy, &asset);
    let reused = catalog
        .reusable_native_pilot_sample(&preparation, &request, &audio)
        .unwrap()
        .expect("exact current sample must be reusable");
    assert_eq!(reused, sample);
    assert_eq!(
        transport.transcribe_calls.load(Ordering::SeqCst),
        transcribe_calls,
        "cache lookup must not perform provider work"
    );

    let changed_window = pack_windows(&[SpeechRegion {
        start_ms: 0,
        end_ms: 500,
    }])
    .remove(0);
    let pcm = vec![0.1_f32; 16_000];
    let changed_audio = NativeSttAudioWindow::from_pcm(&pcm, &changed_window).unwrap();
    assert!(
        catalog
            .reusable_native_pilot_sample(&preparation, &request, &changed_audio)
            .is_err()
    );
    drop(preparation);

    let recipe_preparation = {
        let scope = scope_for(&policy, &asset);
        let asset_row = catalog.get_asset(&asset).unwrap();
        let proxy_path = PathBuf::from(asset_row.proxy_path.as_deref().unwrap());
        let proxy_claim = catalog
            .try_claim_representation(&asset, RepresentationKind::Proxy)
            .unwrap()
            .unwrap();
        let speech_claim = catalog
            .try_claim_representation(&asset, RepresentationKind::Speech)
            .unwrap()
            .unwrap();
        NativeSpeechPreparation::test_preparation(
            NativeSpeechPreparationEvidence {
                policy_id: policy.policy_id.clone(),
                project_id: policy.project_id.clone(),
                asset_id: asset.clone(),
                provider: policy.provider.clone(),
                model: policy.primary_model.clone(),
                recipe: "foreign-native-recipe".into(),
                scope: scope.clone(),
                proxy_full_fingerprint: scope.representation_fingerprint.clone().unwrap(),
                proxy_sampled_fingerprint: scope
                    .sampled_representation_fingerprint
                    .clone()
                    .unwrap(),
                source_duration_ms: scope.source_duration_ms,
                last_safe_sample_ms: scope.source_duration_ms,
                vad: None,
            },
            proxy_path,
            proxy_claim,
            speech_claim,
        )
        .test_with_speech(vec![0.1_f32; 16_000], vec![audio.window().clone()])
    };
    assert!(
        catalog
            .reusable_native_pilot_sample(&recipe_preparation, &request, &audio)
            .is_err(),
        "wrong current recipe must not reuse the cached sample"
    );
    drop(recipe_preparation);

    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET fingerprint='foreign-source-fingerprint' WHERE id=?1",
            [&asset],
        )
        .unwrap();
    let source_preparation = test_preparation(&catalog, &policy, &asset);
    assert!(
        catalog
            .reusable_native_pilot_sample(&source_preparation, &request, &audio)
            .is_err(),
        "wrong current source must not reuse the cached sample"
    );
    assert!(
        catalog.asset_transcript(&asset).unwrap().is_none(),
        "lookup must not create a canonical full-asset transcript"
    );
}

#[test]
fn receipt_without_typed_sample_cannot_reconstruct_text() {
    let (_directory, catalog, _project, asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let (result, _transport) = native_result_from_call(
        &catalog,
        &policy,
        &request,
        &audio,
        &run_id,
        context,
        valid_native_call(&audio, true),
    );
    let running = install_running_pilot(&catalog, &policy, &run_id, &asset);
    catalog.save_pilot(&running).unwrap();
    let preparation = test_preparation(&catalog, &policy, &asset);
    assert!(
        catalog
            .reusable_native_pilot_sample(&preparation, &request, &audio)
            .unwrap()
            .is_none(),
        "a receipt alone must not reconstruct cached candidate evidence"
    );
    assert!(
        catalog
            .analysis_request(&result.request_key)
            .unwrap()
            .is_some()
    );
}

#[test]
fn cached_sample_revalidates_timestamps_language_and_token_probability() {
    let (_directory, catalog, _project, _asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let mut call = valid_native_call(&audio, true);
    if let NativeSttOutcome::Usable(transcript) = &mut call.outcome {
        transcript.confidence = Some(0.9);
        transcript.segments[0].confidence = Some(0.9);
    }
    let (result, _transport) =
        native_result_from_call(&catalog, &policy, &request, &audio, &run_id, context, call);
    let sample = paid_sample(&policy, &request, &audio, &result);
    sample.validate().unwrap();
    match &sample.outcome {
        NativePilotSampleOutcome::Evaluated { quality, .. } => {
            assert_eq!(quality.decision, NativeSttQualityDecision::Accepted)
        }
        other => panic!("expected accepted candidate outcome, got {other:?}"),
    }

    let mut bad_timestamp = sample.clone();
    if let NativePilotSampleOutcome::Evaluated { candidates, .. } = &mut bad_timestamp.outcome {
        candidates[0].end_ms = candidates[0].start_ms;
    }
    assert!(bad_timestamp.validate().is_err());

    let mut bad_probability = sample.clone();
    if let NativePilotSampleOutcome::Evaluated { candidates, .. } = &mut bad_probability.outcome {
        candidates[0].mean_token_probability = Some(1.0);
    }
    assert!(bad_probability.validate().is_err());

    let mut wrong_candidate_language = sample.clone();
    if let NativePilotSampleOutcome::Evaluated { candidates, .. } =
        &mut wrong_candidate_language.outcome
    {
        candidates[0].language = "ja".into();
    }
    assert!(wrong_candidate_language.validate().is_err());
    let mut repeated = sample.clone();
    if let NativePilotSampleOutcome::Evaluated { candidates, .. } = &mut repeated.outcome {
        candidates[0].end_ms = 400;
        let mut duplicate = candidates[0].clone();
        duplicate.start_ms = 500;
        duplicate.end_ms = 900;
        candidates.push(duplicate);
    }
    assert!(repeated.validate().is_err());

    let mut unknown_language = sample;
    if let NativePilotSampleOutcome::Evaluated {
        reported_language, ..
    } = &mut unknown_language.outcome
    {
        *reported_language = Some("unknown_language_canary".into());
    }
    let error = unknown_language.validate().unwrap_err();
    assert!(!format!("{error:#}").contains("unknown_language_canary"));

    let duplicate_scope = scope_for(&policy, &policy.scope[0].asset_id);
    let duplicate = NativePilotLocalOutcomeFact {
        scope: duplicate_scope.clone(),
        recipe_version: policy.recipe_version.clone(),
        outcome: NativePilotLocalOutcome::NoAudio,
    };
    let duplicate_evidence =
        NativePilotEvidence::with_local_outcomes(vec![duplicate.clone(), duplicate]);
    assert!(duplicate_evidence.validate().is_err());
}

#[test]
fn current_silence_and_other_prepared_windows_are_cache_misses_without_provider_work() {
    let (_directory, catalog, _project, asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let (result, transport) = native_result_from_call(
        &catalog,
        &policy,
        &request,
        &audio,
        &run_id,
        context,
        valid_native_call(&audio, true),
    );
    let running = install_running_pilot(&catalog, &policy, &run_id, &asset);
    catalog
        .attach_native_pilot_evidence(
            &running,
            NativePilotEvidence::with_sample(paid_sample(&policy, &request, &audio, &result)),
            Some((&request, &audio)),
        )
        .unwrap()
        .unwrap();
    let second_window = pack_windows(&[SpeechRegion {
        start_ms: 1000,
        end_ms: 2000,
    }])
    .remove(0);
    let pcm = vec![0.1_f32; 32_000];
    let second_audio = NativeSttAudioWindow::from_pcm(&pcm, &second_window).unwrap();
    // Supply two independently valid maps to exercise the lookup boundary's
    // per-window behavior through its prepared-media seam.
    let preparation = test_preparation(&catalog, &policy, &asset)
        .test_with_speech(pcm, vec![audio.window().clone(), second_window]);
    assert!(
        catalog
            .reusable_native_pilot_sample(&preparation, &request, &second_audio)
            .unwrap()
            .is_none()
    );
    assert!(
        catalog
            .reusable_native_pilot_sample(&preparation, &request, &audio)
            .unwrap()
            .is_some()
    );
    let different_pcm = vec![0.2_f32; 16_000];
    let different_audio = NativeSttAudioWindow::from_pcm(&different_pcm, audio.window()).unwrap();
    assert!(
        catalog
            .reusable_native_pilot_sample(&preparation, &request, &different_audio)
            .is_err()
    );
    let evidence = preparation.evidence().clone();
    drop(preparation);
    let asset_row = catalog.get_asset(&asset).unwrap();
    let no_speech = NativeSpeechPreparation::test_preparation(
        evidence,
        PathBuf::from(asset_row.proxy_path.unwrap()),
        catalog
            .claim_representation(&asset, RepresentationKind::Proxy)
            .unwrap(),
        catalog
            .claim_representation(&asset, RepresentationKind::Speech)
            .unwrap(),
    );
    assert!(
        catalog
            .reusable_native_pilot_sample(&no_speech, &request, &audio)
            .unwrap()
            .is_none()
    );
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn pilot_finalization_keeps_attached_evidence_and_cannot_overwrite_a_newer_run() {
    let (_directory, catalog, _project, asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let (result, _transport) = native_result_from_call(
        &catalog,
        &policy,
        &request,
        &audio,
        &run_id,
        context,
        valid_native_call(&audio, true),
    );
    let running = install_running_pilot(&catalog, &policy, &run_id, &asset);
    let sample = paid_sample(&policy, &request, &audio, &result);
    catalog
        .attach_native_pilot_evidence(
            &running,
            NativePilotEvidence::with_sample(sample.clone()),
            Some((&request, &audio)),
        )
        .unwrap()
        .unwrap();
    let validation = PilotValidation {
        structural_schema_valid: true,
        source_time_semantics_valid: true,
        capability_observed: true,
        detail: "synthetic observed protocol".into(),
    };
    let finalized = catalog
        .finalize_pilot_projection(&running, &Default::default(), validation.clone(), None)
        .unwrap();
    assert_eq!(finalized.status, "passed");
    assert_eq!(finalized.native_evidence.unwrap().sample, Some(sample));
    let new_run = catalog
        .begin_accounted_analysis_run(&policy.project_id, &policy.policy_id, "pilot")
        .unwrap();
    let newer = install_running_pilot(&catalog, &policy, &new_run, &asset);
    catalog
        .finalize_pilot_projection(&running, &Default::default(), validation, None)
        .unwrap();
    assert_eq!(
        catalog.load_run_pilot(&policy.policy_id).unwrap().unwrap(),
        newer
    );
}
