use super::*;
use crate::ProviderReceiptEvidence;
use crate::analysis::quality::native_stt::{
    NativeSttQualityDecision, NativeSttQualityEvaluation, evaluate_native_stt_quality,
};
use crate::analysis::store::{
    NativeSttWindowObservation, StoredTranscript, TranscriptOutcomeReason, TranscriptStatus,
    save_native_transcript, save_transcript,
};
use serde_json::{Value, json};

fn configured_native_call(
    audio: &NativeSttAudioWindow,
    include_cost: bool,
    confidence: Option<f64>,
) -> NativeSttCallResult {
    let mut call = valid_native_call(audio, include_cost);
    if let NativeSttOutcome::Usable(transcript) = &mut call.outcome {
        transcript.confidence = confidence;
    }
    call
}

fn evaluation_for(
    result: &crate::analysis::accounting::NativeSttAccountedResult,
    audio: &NativeSttAudioWindow,
) -> NativeSttQualityEvaluation {
    let NativeSttOutcome::Usable(provider_transcript) = result.outcome() else {
        panic!("expected a usable native provider response");
    };
    evaluate_native_stt_quality(provider_transcript, audio)
}

fn provider_segment_count(result: &crate::analysis::accounting::NativeSttAccountedResult) -> u32 {
    match result.outcome() {
        NativeSttOutcome::Usable(transcript) => transcript.segments.len() as u32,
        NativeSttOutcome::Unusable(_) => 0,
    }
}

fn native_transcript_for_evaluation(
    policy: &RunPolicy,
    asset_id: &str,
    audio: &NativeSttAudioWindow,
    result: &crate::analysis::accounting::NativeSttAccountedResult,
    evaluation: &NativeSttQualityEvaluation,
) -> StoredTranscript {
    let scope = policy
        .scope
        .iter()
        .find(|scope| scope.asset_id == asset_id)
        .expect("native scope for fixture asset");
    let (status, outcome_reason, segments) = match evaluation.decision {
        NativeSttQualityDecision::Accepted => (
            TranscriptStatus::Speech,
            TranscriptOutcomeReason::Accepted,
            evaluation.segments.clone(),
        ),
        NativeSttQualityDecision::ReviewRequired => (
            TranscriptStatus::ReviewRequired,
            TranscriptOutcomeReason::QualityEvidenceIncomplete,
            evaluation.segments.clone(),
        ),
        NativeSttQualityDecision::Rejected => (
            TranscriptStatus::Rejected,
            TranscriptOutcomeReason::QualityRejected,
            Vec::new(),
        ),
    };
    StoredTranscript {
        asset_id: asset_id.to_string(),
        recipe_key: policy.recipe_version.clone(),
        source_fingerprint: scope
            .sampled_representation_fingerprint
            .clone()
            .expect("approved sampled fingerprint"),
        status,
        outcome_reason,
        reject_reason: evaluation.reason_code().to_string(),
        primary_language: evaluation.language.clone().unwrap_or_default(),
        speech_seconds: f64::from(audio.duration_ms()) / 1_000.0,
        window_count: 1,
        provider_segment_count: provider_segment_count(result),
        mean_token_probability: None,
        repetition_ratio: evaluation.metrics.repetition_ratio,
        segments,
    }
}

fn observation_for(
    result: &crate::analysis::accounting::NativeSttAccountedResult,
    evaluation: &NativeSttQualityEvaluation,
) -> NativeSttWindowObservation {
    NativeSttWindowObservation {
        request_key: result.request_key.clone(),
        latency_ms: result.latency_ms(),
        reported_language: evaluation.reported_language.clone(),
        quality: crate::analysis::store::NativeSttWindowQualityObservation::from_evaluation(
            evaluation,
        )
        .unwrap(),
    }
}

fn transmit_native(
    catalog: &Catalog,
    policy: &RunPolicy,
    request: &NativeSttRequest,
    audio: &NativeSttAudioWindow,
    run_id: &str,
    context: AnalysisAccountingContext,
    call: NativeSttCallResult,
) -> crate::analysis::accounting::NativeSttAccountedResult {
    let transport =
        CountingNativeTransport::new(catalog, run_id, FakeNativeResponse::Ok(Box::new(call)));
    AccountedNativeSttProvider::new(catalog, policy, context, request, audio, &transport)
        .transmit()
        .unwrap()
}

fn link_count(catalog: &Catalog, asset_id: &str) -> i64 {
    catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM native_transcript_windows WHERE asset_id=?1",
            [asset_id],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn accepted_native_window_links_exact_accounting_and_read_projection() {
    let (_directory, catalog, project, asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let call = configured_native_call(&audio, true, Some(0.9));
    let result = transmit_native(&catalog, &policy, &request, &audio, &run_id, context, call);
    let evaluation = evaluation_for(&result, &audio);
    assert_eq!(evaluation.decision, NativeSttQualityDecision::Accepted);
    let transcript =
        native_transcript_for_evaluation(&policy, &asset, &audio, &result, &evaluation);
    let observation = observation_for(&result, &evaluation);

    {
        let mut connection = catalog.connection().unwrap();
        save_native_transcript(
            &mut connection,
            &policy.policy_id,
            &transcript,
            std::slice::from_ref(&observation),
        )
        .unwrap();
        // The exact same native output and request may be re-saved atomically.
        save_native_transcript(
            &mut connection,
            &policy.policy_id,
            &transcript,
            std::slice::from_ref(&observation),
        )
        .unwrap();
    }
    assert_eq!(link_count(&catalog, &asset), 1);

    let provenance = catalog
        .native_transcript_provenance(&asset)
        .unwrap()
        .expect("native provenance");
    assert_eq!(
        provenance.schema_version,
        crate::NATIVE_TRANSCRIPT_PROVENANCE_SCHEMA_VERSION
    );
    assert_eq!(provenance.status, TranscriptStatus::Speech);
    assert_eq!(provenance.policy.policy_id, policy.policy_id);
    assert_eq!(provenance.windows.len(), 1);
    let window = &provenance.windows[0];
    assert_eq!(window.window_index, 0);
    assert_eq!(window.latency_ms, 47);
    assert_eq!(window.reported_language.as_deref(), Some("english"));
    assert_eq!(window.quality.decision, NativeSttQualityDecision::Accepted);
    assert_eq!(window.accounting.request.provider, "openrouter");
    assert_eq!(
        window.accounting.request.actual_provider.as_deref(),
        Some("openai")
    );
    assert_eq!(
        window.accounting.request.actual_model.as_deref(),
        Some(OPENROUTER_WHISPER_MODEL)
    );
    assert!((window.accounting.request.confirmed_cost_usd - 0.002).abs() < 1e-12);
    assert_eq!(window.accounting.attempt_history.len(), 1);
    let projection_json = serde_json::to_string(&provenance).unwrap();
    assert!(!projection_json.contains("actualProviderMatchesRequested"));
    assert!(!projection_json.contains("actualModelMatchesRequested"));

    // The established local transcript wire shape remains byte-compatible and
    // contains no copied native provenance.
    let local_json = serde_json::to_string(
        &catalog
            .asset_transcript(&asset)
            .unwrap()
            .expect("canonical transcript"),
    )
    .unwrap();
    assert!(!local_json.contains("nativePolicyId"));
    assert!(!local_json.contains("nativeQualityRecipeVersion"));

    // A header cannot claim a different recipe from the frozen authorization.
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE asset_transcripts SET recipe_key='unbound-native-recipe' WHERE asset_id=?1",
            [&asset],
        )
        .unwrap();
    assert!(catalog.native_transcript_provenance(&asset).is_err());
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE asset_transcripts SET recipe_key=?2 WHERE asset_id=?1",
            params![asset, policy.recipe_version],
        )
        .unwrap();

    // Replacing the native artifact with ordinary local output clears the child
    // links and nullable header association in the same transaction.
    let local = StoredTranscript {
        asset_id: asset.clone(),
        recipe_key: crate::STT_RECIPE_KEY.into(),
        source_fingerprint: transcript.source_fingerprint.clone(),
        status: TranscriptStatus::Speech,
        outcome_reason: TranscriptOutcomeReason::Accepted,
        segments: vec![crate::analysis::quality::DecodedSegment {
            start_ms: 0,
            end_ms: 500,
            language: "en".into(),
            text: "local replacement".into(),
            mean_token_probability: Some(0.9),
        }],
        mean_token_probability: Some(0.9),
        ..transcript
    };
    save_transcript(&catalog.connection().unwrap(), &local).unwrap();
    assert!(
        catalog
            .native_transcript_provenance(&asset)
            .unwrap()
            .is_none()
    );
    assert_eq!(link_count(&catalog, &asset), 0);
    let header_association: (Option<String>, Option<String>) = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT native_policy_id,native_quality_recipe_version
             FROM asset_transcripts WHERE asset_id=?1",
            [&asset],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(header_association, (None, None));
    assert_eq!(
        catalog
            .list_analysis_requests(&project, Some(&policy.policy_id), 10)
            .unwrap()
            .len(),
        1,
        "local replacement must not rewrite or remove accounting rows"
    );
}

#[test]
fn review_required_retry_history_and_late_receipt_enrich_projection_without_rewrite() {
    let (_directory, catalog, project, asset, policy, request, audio, first_run_id, first_context) =
        authorized_native_setup();
    let mut first_call = configured_native_call(&audio, false, None);
    first_call.receipt.generation_id = Some("native-generation-first".into());
    let first = transmit_native(
        &catalog,
        &policy,
        &request,
        &audio,
        &first_run_id,
        first_context,
        first_call,
    );
    let first_evaluation = evaluation_for(&first, &audio);
    assert_eq!(
        first_evaluation.decision,
        NativeSttQualityDecision::ReviewRequired
    );
    assert_eq!(
        catalog
            .analysis_request(&first.request_key)
            .unwrap()
            .unwrap()
            .lifecycle,
        AnalysisRequestLifecycle::Uncertain
    );

    let retry = catalog
        .authorize_analysis_request_retry_for_project(&project, &first.request_key)
        .unwrap();
    assert_eq!(retry.next_attempt_epoch, 1);
    let second_run_id = catalog
        .begin_accounted_analysis_run(&project, &policy.policy_id, "pilot")
        .unwrap();
    let scope = policy.scope.first().unwrap();
    let second_context = AnalysisAccountingContext {
        project_id: policy.project_id.clone(),
        policy_id: policy.policy_id.clone(),
        policy_fingerprint: policy.policy_id.clone(),
        run_id: second_run_id.clone(),
        operation: "pilot".into(),
        intended_attempt_key: format!("{}:{}", policy.kind.as_str(), scope.asset_id),
        source_fingerprint: scope.source_identity.clone(),
        provider: policy.provider.clone(),
        budget_scope_id: String::new(),
        budget_usd: policy.budget_usd,
        explicit_retry: true,
    };
    let mut second_call = configured_native_call(&audio, true, Some(0.9));
    second_call.receipt.generation_id = Some("native-generation-second".into());
    let second = transmit_native(
        &catalog,
        &policy,
        &request,
        &audio,
        &second_run_id,
        second_context,
        second_call,
    );
    let second_evaluation = evaluation_for(&second, &audio);
    assert_eq!(
        second_evaluation.decision,
        NativeSttQualityDecision::Accepted
    );

    let first_observation = observation_for(&first, &first_evaluation);
    let second_observation = observation_for(&second, &second_evaluation);
    let mut transcript =
        native_transcript_for_evaluation(&policy, &asset, &audio, &second, &second_evaluation);
    transcript.status = TranscriptStatus::ReviewRequired;
    transcript.outcome_reason = TranscriptOutcomeReason::QualityEvidenceIncomplete;
    transcript.window_count = 2;
    transcript.provider_segment_count += provider_segment_count(&first);
    transcript.repetition_ratio = second_evaluation.metrics.repetition_ratio;

    {
        let mut connection = catalog.connection().unwrap();
        save_native_transcript(
            &mut connection,
            &policy.policy_id,
            &transcript,
            &[first_observation, second_observation],
        )
        .unwrap();
    }

    let before_late = catalog
        .native_transcript_provenance(&asset)
        .unwrap()
        .expect("native provenance");
    assert_eq!(before_late.status, TranscriptStatus::ReviewRequired);
    assert_eq!(before_late.windows.len(), 2);
    assert_eq!(
        before_late.windows[0].accounting.request.lifecycle,
        AnalysisRequestLifecycle::Uncertain
    );
    assert!(
        before_late.windows[0]
            .accounting
            .request
            .uncertain_usage_reserve_usd
            > 0.0
    );
    assert_eq!(
        before_late.windows[1].accounting.request.lifecycle,
        AnalysisRequestLifecycle::Completed
    );
    assert_eq!(before_late.windows[0].accounting.attempt_history.len(), 2);
    assert_eq!(before_late.windows[1].accounting.attempt_history.len(), 2);
    let header_before: (String, String, String, Option<String>, Option<String>) = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT recipe_key,source_fingerprint,status,native_policy_id,
                    native_quality_recipe_version
             FROM asset_transcripts WHERE asset_id=?1",
            [&asset],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();

    let late_evidence = ProviderReceiptEvidence::from_native_stt_receipt(&NativeSttReceipt {
        generation_id: Some("native-generation-first".into()),
        actual_model: Some(OPENROUTER_WHISPER_MODEL.into()),
        actual_provider: Some("openai".into()),
        usage: NativeSttUsage {
            cost: NativeSttCost::Known(0.001),
            seconds: Some(1.0),
            input_tokens: Some(8),
            output_tokens: Some(4),
        },
    })
    .unwrap();
    let reconciled = catalog
        .record_analysis_request_receipt(&first.request_key, &late_evidence)
        .unwrap();
    assert_eq!(reconciled.lifecycle, AnalysisRequestLifecycle::Completed);
    assert!((reconciled.confirmed_cost_usd - 0.001).abs() < 1e-12);

    let after_late = catalog
        .native_transcript_provenance(&asset)
        .unwrap()
        .expect("native provenance after late receipt");
    assert_eq!(
        after_late.windows[0].accounting.request.lifecycle,
        AnalysisRequestLifecycle::Completed
    );
    assert!((after_late.windows[0].accounting.request.confirmed_cost_usd - 0.001).abs() < 1e-12);
    assert_eq!(
        after_late.windows[0]
            .accounting
            .request
            .actual_provider
            .as_deref(),
        Some("openai")
    );
    assert_eq!(after_late.windows[0].accounting.attempt_history.len(), 2);
    assert_eq!(
        after_late.windows[1].quality.decision,
        NativeSttQualityDecision::Accepted
    );
    assert_eq!(
        after_late.windows[0].quality.decision,
        NativeSttQualityDecision::ReviewRequired
    );
    let header_after: (String, String, String, Option<String>, Option<String>) = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT recipe_key,source_fingerprint,status,native_policy_id,
                    native_quality_recipe_version
             FROM asset_transcripts WHERE asset_id=?1",
            [&asset],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        header_after, header_before,
        "a late receipt must not rewrite the transcript or provenance header"
    );
    assert_eq!(
        catalog
            .list_analysis_requests(&project, Some(&policy.policy_id), 10)
            .unwrap()
            .len(),
        2,
        "late reconciliation must not issue another provider request"
    );
}

#[test]
fn verified_silence_and_no_audio_are_zero_request_and_do_not_fabricate_identity() {
    let (_directory, catalog, project, asset, policy, _request, _audio, _run_id, _context) =
        authorized_native_setup();
    let scope = policy.scope.first().unwrap();
    let source_fingerprint = scope
        .sampled_representation_fingerprint
        .clone()
        .expect("approved sampled fingerprint");
    let no_speech = StoredTranscript {
        asset_id: asset.clone(),
        recipe_key: policy.recipe_version.clone(),
        source_fingerprint: source_fingerprint.clone(),
        status: TranscriptStatus::NoSpeech,
        outcome_reason: TranscriptOutcomeReason::NoDetectedSpeech,
        reject_reason: String::new(),
        primary_language: String::new(),
        speech_seconds: 0.0,
        window_count: 0,
        provider_segment_count: 0,
        mean_token_probability: None,
        repetition_ratio: None,
        segments: Vec::new(),
    };
    {
        let mut connection = catalog.connection().unwrap();
        save_native_transcript(&mut connection, &policy.policy_id, &no_speech, &[]).unwrap();
    }
    let provenance = catalog
        .native_transcript_provenance(&asset)
        .unwrap()
        .expect("verified silence provenance");
    assert_eq!(provenance.status, TranscriptStatus::NoSpeech);
    assert_eq!(
        provenance.outcome_reason,
        TranscriptOutcomeReason::NoDetectedSpeech
    );
    assert!(provenance.windows.is_empty());
    assert_eq!(link_count(&catalog, &asset), 0);

    let no_audio = StoredTranscript {
        status: TranscriptStatus::NoAudio,
        outcome_reason: TranscriptOutcomeReason::NoAudioStream,
        ..no_speech.clone()
    };
    {
        let mut connection = catalog.connection().unwrap();
        save_native_transcript(&mut connection, &policy.policy_id, &no_audio, &[]).unwrap();
    }
    let provenance = catalog
        .native_transcript_provenance(&asset)
        .unwrap()
        .expect("no-audio provenance");
    assert_eq!(provenance.status, TranscriptStatus::NoAudio);
    assert_eq!(
        provenance.outcome_reason,
        TranscriptOutcomeReason::NoAudioStream
    );
    assert!(provenance.windows.is_empty());

    // A legacy unverified silence outcome and provider-shaped status with zero
    // requests are not eligible for new native provenance.
    let unverified = StoredTranscript {
        outcome_reason: TranscriptOutcomeReason::LegacyNoSpeechUnverified,
        ..no_speech.clone()
    };
    {
        let mut connection = catalog.connection().unwrap();
        assert!(
            save_native_transcript(&mut connection, &policy.policy_id, &unverified, &[]).is_err()
        );
        let mut speech = no_speech.clone();
        speech.status = TranscriptStatus::Speech;
        speech.outcome_reason = TranscriptOutcomeReason::Accepted;
        speech.speech_seconds = 1.0;
        speech.segments = vec![crate::analysis::quality::DecodedSegment {
            start_ms: 0,
            end_ms: 500,
            language: "en".into(),
            text: "fabricated provider shape".into(),
            mean_token_probability: None,
        }];
        assert!(save_native_transcript(&mut connection, &policy.policy_id, &speech, &[]).is_err());
    }
    let provenance = catalog
        .native_transcript_provenance(&asset)
        .unwrap()
        .expect("prior no-audio artifact remains");
    assert_eq!(provenance.status, TranscriptStatus::NoAudio);
    assert!(provenance.windows.is_empty());
    assert_eq!(
        catalog
            .list_analysis_requests(&project, Some(&policy.policy_id), 10)
            .unwrap()
            .len(),
        0,
        "verified silence must make no provider request and invent no charge"
    );
}

fn assert_prior_native_artifact(catalog: &Catalog, asset_id: &str, request_key: &str) {
    let provenance = catalog
        .native_transcript_provenance(asset_id)
        .unwrap()
        .expect("prior native artifact remains");
    assert_eq!(provenance.status, TranscriptStatus::Speech);
    assert_eq!(provenance.windows.len(), 1);
    assert_eq!(
        provenance.windows[0].accounting.request.request_key,
        request_key
    );
    assert_eq!(link_count(catalog, asset_id), 1);
}

#[test]
fn native_save_rejects_stale_or_foreign_bindings_and_rolls_back_atomically() {
    let (_directory, catalog, project, asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let call = configured_native_call(&audio, true, Some(0.9));
    let result = transmit_native(&catalog, &policy, &request, &audio, &run_id, context, call);
    let evaluation = evaluation_for(&result, &audio);
    let transcript =
        native_transcript_for_evaluation(&policy, &asset, &audio, &result, &evaluation);
    let observation = observation_for(&result, &evaluation);
    {
        let mut connection = catalog.connection().unwrap();
        save_native_transcript(
            &mut connection,
            &policy.policy_id,
            &transcript,
            std::slice::from_ref(&observation),
        )
        .unwrap();
    }
    assert_prior_native_artifact(&catalog, &asset, &result.request_key);

    // Mismatched selected-representation fingerprint.
    let mut stale_source = transcript.clone();
    stale_source.source_fingerprint = "foreign-derived-representation".into();
    {
        let mut connection = catalog.connection().unwrap();
        assert!(
            save_native_transcript(
                &mut connection,
                &policy.policy_id,
                &stale_source,
                std::slice::from_ref(&observation),
            )
            .is_err()
        );
    }
    assert_prior_native_artifact(&catalog, &asset, &result.request_key);

    // Duplicate request keys within one input are rejected even for the same
    // asset, while an idempotent exact re-save above remains supported.
    let mut duplicate_shape = transcript.clone();
    duplicate_shape.window_count = 2;
    {
        let mut connection = catalog.connection().unwrap();
        assert!(
            save_native_transcript(
                &mut connection,
                &policy.policy_id,
                &duplicate_shape,
                &[observation.clone(), observation.clone()],
            )
            .is_err()
        );
    }
    assert_prior_native_artifact(&catalog, &asset, &result.request_key);

    // Both canonical exclusion signals are required.
    for exclusion in ["removed", "membership_removed"] {
        let connection = catalog.connection().unwrap();
        connection
            .execute(
                &format!("UPDATE assets SET {exclusion}=1 WHERE id=?1"),
                [&asset],
            )
            .unwrap();
        let mut write_connection = catalog.connection().unwrap();
        assert!(
            save_native_transcript(
                &mut write_connection,
                &policy.policy_id,
                &transcript,
                std::slice::from_ref(&observation),
            )
            .is_err(),
            "excluded via {exclusion}"
        );
        drop(write_connection);
        connection
            .execute(
                &format!("UPDATE assets SET {exclusion}=0 WHERE id=?1"),
                [&asset],
            )
            .unwrap();
    }
    assert_prior_native_artifact(&catalog, &asset, &result.request_key);

    // Actual foreign project ownership, not merely a nonexistent asset id.
    let foreign_project = catalog.create_project("foreign native project").unwrap();
    {
        let connection = catalog.connection().unwrap();
        connection
            .execute(
                "UPDATE assets SET project_id=?2 WHERE id=?1",
                params![asset, foreign_project.id],
            )
            .unwrap();
        let mut write_connection = catalog.connection().unwrap();
        assert!(
            save_native_transcript(
                &mut write_connection,
                &policy.policy_id,
                &transcript,
                std::slice::from_ref(&observation),
            )
            .is_err()
        );
        drop(write_connection);
        connection
            .execute(
                "UPDATE assets SET project_id=?2 WHERE id=?1",
                params![asset, project],
            )
            .unwrap();
    }
    assert_prior_native_artifact(&catalog, &asset, &result.request_key);

    // Actual accounted run now claims a foreign policy identity.
    {
        let connection = catalog.connection().unwrap();
        connection
            .execute(
                "UPDATE analysis_runs SET policy_id='foreign-policy' WHERE id=?1",
                [&run_id],
            )
            .unwrap();
        let mut write_connection = catalog.connection().unwrap();
        assert!(
            save_native_transcript(
                &mut write_connection,
                &policy.policy_id,
                &transcript,
                std::slice::from_ref(&observation),
            )
            .is_err()
        );
        drop(write_connection);
        connection
            .execute(
                "UPDATE analysis_runs SET policy_id=?2 WHERE id=?1",
                params![run_id, policy.policy_id],
            )
            .unwrap();
    }
    assert_prior_native_artifact(&catalog, &asset, &result.request_key);

    // Tampered policy JSON under the unchanged canonical policy id must be
    // rejected by the shared authorization-identity loader.
    {
        let connection = catalog.connection().unwrap();
        let original_json: String = connection
            .query_row(
                "SELECT policy_json FROM analysis_run_authorizations WHERE policy_id=?1",
                [&policy.policy_id],
                |row| row.get(0),
            )
            .unwrap();
        let mut tampered: Value = serde_json::from_str(&original_json).unwrap();
        tampered["budgetUsd"] = json!(policy.budget_usd + 1.0);
        connection
            .execute(
                "UPDATE analysis_run_authorizations SET policy_json=?2 WHERE policy_id=?1",
                params![policy.policy_id, tampered.to_string()],
            )
            .unwrap();
        let mut write_connection = catalog.connection().unwrap();
        assert!(
            save_native_transcript(
                &mut write_connection,
                &policy.policy_id,
                &transcript,
                std::slice::from_ref(&observation),
            )
            .is_err()
        );
        drop(write_connection);
        assert!(
            catalog.native_transcript_provenance(&asset).is_err(),
            "historical read must reject tampered policy JSON under the same id"
        );
        connection
            .execute(
                "UPDATE analysis_run_authorizations SET policy_json=?2 WHERE policy_id=?1",
                params![policy.policy_id, original_json],
            )
            .unwrap();
    }
    assert_prior_native_artifact(&catalog, &asset, &result.request_key);

    // A forced association-SQL failure must roll back the header, segments and
    // replacement links together; the previous association reappears.
    {
        let connection = catalog.connection().unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER forced_native_link_failure
                 BEFORE INSERT ON native_transcript_windows
                 WHEN NEW.asset_id = 'clip'
                 BEGIN
                     SELECT RAISE(ABORT, 'forced native provenance failure');
                 END;",
            )
            .unwrap();
        let mut changed = transcript.clone();
        changed.reject_reason = "must-roll-back".into();
        let mut write_connection = catalog.connection().unwrap();
        assert!(
            save_native_transcript(
                &mut write_connection,
                &policy.policy_id,
                &changed,
                std::slice::from_ref(&observation),
            )
            .is_err()
        );
        drop(write_connection);
        let reject_reason: String = connection
            .query_row(
                "SELECT reject_reason FROM asset_transcripts WHERE asset_id=?1",
                [&asset],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reject_reason, transcript.reject_reason);
        connection
            .execute_batch("DROP TRIGGER forced_native_link_failure")
            .unwrap();
    }
    assert_prior_native_artifact(&catalog, &asset, &result.request_key);
}

#[test]
fn native_save_rejects_request_already_owned_by_another_transcript_asset() {
    let (_directory, catalog, _project, asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let call = configured_native_call(&audio, true, Some(0.9));
    let result = transmit_native(&catalog, &policy, &request, &audio, &run_id, context, call);
    let evaluation = evaluation_for(&result, &audio);
    let transcript =
        native_transcript_for_evaluation(&policy, &asset, &audio, &result, &evaluation);
    let observation = observation_for(&result, &evaluation);

    let foreign_asset = "foreign-asset";
    {
        let connection = catalog.connection().unwrap();
        connection
            .execute(
                "INSERT INTO assets
                    (id,project_id,fingerprint,path,filename,size_bytes,duration_seconds,created_at)
                 VALUES (?1,?2,'foreign-source-fp','/foreign.mp4','foreign.mp4',1,2.0,
                         '2026-01-01T00:00:00Z')",
                params![foreign_asset, policy.project_id],
            )
            .unwrap();
        let foreign_transcript = StoredTranscript {
            asset_id: foreign_asset.into(),
            recipe_key: crate::STT_RECIPE_KEY.into(),
            source_fingerprint: "foreign-derived-fp".into(),
            status: TranscriptStatus::Speech,
            outcome_reason: TranscriptOutcomeReason::Accepted,
            reject_reason: String::new(),
            primary_language: "en".into(),
            speech_seconds: 1.0,
            window_count: 0,
            provider_segment_count: 0,
            mean_token_probability: Some(0.9),
            repetition_ratio: Some(0.0),
            segments: vec![crate::analysis::quality::DecodedSegment {
                start_ms: 0,
                end_ms: 500,
                language: "en".into(),
                text: "foreign local transcript".into(),
                mean_token_probability: Some(0.9),
            }],
        };
        save_transcript(&connection, &foreign_transcript).unwrap();
        connection
            .execute(
                "UPDATE asset_transcripts
                 SET native_policy_id=?2,native_quality_recipe_version=?3
                 WHERE asset_id=?1",
                params![
                    foreign_asset,
                    policy.policy_id,
                    crate::NATIVE_STT_QUALITY_RECIPE_VERSION,
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO native_transcript_windows
                    (asset_id,window_index,request_key,latency_ms,reported_language,quality_json)
                 VALUES (?1,0,?2,47,?3,?4)",
                params![
                    foreign_asset,
                    result.request_key,
                    observation.reported_language,
                    serde_json::to_string(&observation.quality).unwrap(),
                ],
            )
            .unwrap();
    }

    let mut connection = catalog.connection().unwrap();
    let error = save_native_transcript(
        &mut connection,
        &policy.policy_id,
        &transcript,
        std::slice::from_ref(&observation),
    )
    .unwrap_err();
    assert!(error.to_string().contains("another asset"), "{error}");
    assert!(
        catalog
            .native_transcript_provenance(&asset)
            .unwrap()
            .is_none()
    );
    assert_eq!(link_count(&catalog, &asset), 0);
    assert_eq!(link_count(&catalog, foreign_asset), 1);
}

#[test]
fn native_save_rejects_actual_vlm_and_undispatched_native_requests() {
    let (_directory, catalog, project, asset, policy, native_request, audio, run_id, context) =
        authorized_native_setup();
    let call = configured_native_call(&audio, true, Some(0.9));
    let result = transmit_native(
        &catalog,
        &policy,
        &native_request,
        &audio,
        &run_id,
        context,
        call,
    );
    let evaluation = evaluation_for(&result, &audio);
    let transcript =
        native_transcript_for_evaluation(&policy, &asset, &audio, &result, &evaluation);

    // An actual VLM pilot request under the same project/asset cannot be
    // relabelled as a native audio window.
    let (_vlm_request_spec, vlm_policy, _pilot) =
        authorized_observation(&catalog, &project, &asset);
    let vlm_request = catalog
        .list_analysis_requests(&project, Some(&vlm_policy.policy_id), 10)
        .unwrap()
        .into_iter()
        .next()
        .expect("actual VLM request");
    let mut vlm_observation = observation_for(&result, &evaluation);
    vlm_observation.request_key = vlm_request.request_key;
    {
        let mut connection = catalog.connection().unwrap();
        let error = save_native_transcript(
            &mut connection,
            &policy.policy_id,
            &transcript,
            std::slice::from_ref(&vlm_observation),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("policy/project")
                || error.to_string().contains("source/provider/model")
                || error.to_string().contains("native audio")
                || error.to_string().contains("native speech operation"),
            "{error}"
        );
    }
    assert!(
        catalog
            .native_transcript_provenance(&asset)
            .unwrap()
            .is_none()
    );

    // A real native reservation is still not linkable before the durable
    // dispatch marker and receipt lifecycle.
    let provider_policy = NativeSttProviderPolicy::new(true, SttDataCollection::Deny);
    let forced_request = RunPolicyRequest::native_speech_transcription(
        project.clone(),
        vec![asset.clone()],
        OPENROUTER_STT_PROVIDER,
        OPENROUTER_WHISPER_MODEL,
        1.0,
        Some("en".into()),
        provider_policy,
        true,
    );
    let forced_policy = catalog.describe_run_policy(&forced_request).unwrap();
    assert!(forced_policy.readiness.ready_for_authorization);
    catalog.authorize_run_policy(&forced_request).unwrap();
    let authority = AnalysisBudgetAuthorityContext {
        project_id: forced_policy.project_id.clone(),
        policy_id: forced_policy.policy_id.clone(),
        policy_fingerprint: forced_policy.policy_id.clone(),
        operation: "pilot".into(),
        provider: forced_policy.provider.clone(),
        budget_scope_id: String::new(),
        budget_usd: forced_policy.budget_usd,
    };
    catalog
        .initialize_analysis_accounting_authority(&authority)
        .unwrap();
    let forced_run_id = catalog
        .begin_accounted_analysis_run(&project, &forced_policy.policy_id, "pilot")
        .unwrap();
    let forced_scope = forced_policy.scope.first().unwrap();
    let forced_context = AnalysisAccountingContext {
        project_id: forced_policy.project_id.clone(),
        policy_id: forced_policy.policy_id.clone(),
        policy_fingerprint: forced_policy.policy_id.clone(),
        run_id: forced_run_id,
        operation: "pilot".into(),
        intended_attempt_key: format!("{}:{}", forced_policy.kind.as_str(), forced_scope.asset_id),
        source_fingerprint: forced_scope.source_identity.clone(),
        provider: forced_policy.provider.clone(),
        budget_scope_id: String::new(),
        budget_usd: forced_policy.budget_usd,
        explicit_retry: false,
    };
    let reserved = catalog
        .reserve_analysis_native_stt_request(&forced_context, &native_request, &audio)
        .unwrap();
    assert_eq!(reserved.lifecycle, AnalysisRequestLifecycle::Reserved);
    let mut undispatched_transcript = StoredTranscript {
        asset_id: asset.clone(),
        recipe_key: forced_policy.recipe_version.clone(),
        source_fingerprint: forced_scope
            .sampled_representation_fingerprint
            .clone()
            .unwrap(),
        status: TranscriptStatus::Speech,
        outcome_reason: TranscriptOutcomeReason::Accepted,
        reject_reason: String::new(),
        primary_language: evaluation.language.clone().unwrap_or_default(),
        speech_seconds: f64::from(audio.duration_ms()) / 1_000.0,
        window_count: 1,
        provider_segment_count: 1,
        mean_token_probability: None,
        repetition_ratio: evaluation.metrics.repetition_ratio,
        segments: evaluation.segments.clone(),
    };
    let mut undispatched_observation = observation_for(&result, &evaluation);
    undispatched_observation.request_key = reserved.request_key.clone();
    {
        let mut connection = catalog.connection().unwrap();
        let error = save_native_transcript(
            &mut connection,
            &forced_policy.policy_id,
            &undispatched_transcript,
            std::slice::from_ref(&undispatched_observation),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("undispatched")
                || error.to_string().contains("active or released"),
            "{error}"
        );
    }
    assert!(
        catalog
            .native_transcript_provenance(&asset)
            .unwrap()
            .is_none()
    );
    assert_eq!(link_count(&catalog, &asset), 0);

    // The same native reservation passed under a foreign policy id is rejected
    // by canonical policy/request ownership before any write.
    undispatched_transcript.recipe_key = policy.recipe_version.clone();
    {
        let mut connection = catalog.connection().unwrap();
        assert!(
            save_native_transcript(
                &mut connection,
                &policy.policy_id,
                &undispatched_transcript,
                std::slice::from_ref(&undispatched_observation),
            )
            .is_err()
        );
    }
}

#[test]
fn provenance_projection_uses_one_read_snapshot_across_concurrent_local_replacement() {
    let (_directory, catalog, _project, asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let call = configured_native_call(&audio, true, Some(0.9));
    let result = transmit_native(&catalog, &policy, &request, &audio, &run_id, context, call);
    let evaluation = evaluation_for(&result, &audio);
    let transcript =
        native_transcript_for_evaluation(&policy, &asset, &audio, &result, &evaluation);
    let observation = observation_for(&result, &evaluation);
    {
        let mut connection = catalog.connection().unwrap();
        save_native_transcript(
            &mut connection,
            &policy.policy_id,
            &transcript,
            std::slice::from_ref(&observation),
        )
        .unwrap();
    }

    let reader = catalog.connection().unwrap();
    let read_snapshot = reader.unchecked_transaction().unwrap();
    let first = crate::analysis::store::native_provenance::read::load_native_transcript_provenance(
        &read_snapshot,
        &asset,
    )
    .unwrap()
    .expect("native provenance in read snapshot");

    let local = StoredTranscript {
        asset_id: asset.clone(),
        recipe_key: crate::STT_RECIPE_KEY.into(),
        source_fingerprint: transcript.source_fingerprint.clone(),
        status: TranscriptStatus::Speech,
        outcome_reason: TranscriptOutcomeReason::Accepted,
        reject_reason: String::new(),
        primary_language: "en".into(),
        speech_seconds: 0.5,
        window_count: 0,
        provider_segment_count: 0,
        mean_token_probability: Some(0.9),
        repetition_ratio: Some(0.0),
        segments: vec![crate::analysis::quality::DecodedSegment {
            start_ms: 0,
            end_ms: 500,
            language: "en".into(),
            text: "local replacement during read".into(),
            mean_token_probability: Some(0.9),
        }],
    };
    save_transcript(&catalog.connection().unwrap(), &local).unwrap();

    let second =
        crate::analysis::store::native_provenance::read::load_native_transcript_provenance(
            &read_snapshot,
            &asset,
        )
        .unwrap()
        .expect("same read snapshot still sees the prior native provenance");
    assert_eq!(first.windows.len(), 1);
    assert_eq!(second.windows.len(), 1);
    assert_eq!(
        first.windows[0].accounting.request.request_key,
        second.windows[0].accounting.request.request_key
    );
    read_snapshot.commit().unwrap();
    assert!(
        catalog
            .native_transcript_provenance(&asset)
            .unwrap()
            .is_none()
    );
}

#[test]
fn historical_native_policy_and_recipe_remain_readable_without_current_tariff_rules() {
    let (_directory, catalog, _project, asset, mut policy, _request, _audio, _run_id, _context) =
        authorized_native_setup();
    policy.recipe_version = "native-stt.transcript-recipe.historical.v0".into();
    policy.provider_cost_bound_contract = "native-stt.cost.historical.v0".into();
    policy.policy_id = run_policy_identity(&policy).unwrap();
    catalog
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO analysis_run_authorizations
            (policy_id,project_id,policy_json,authorized_at,accounting_contract)
         VALUES (?1,?2,?3,'2026-01-01T00:00:00Z','analysis.accounting.historical.v0')",
            params![
                policy.policy_id,
                policy.project_id,
                serde_json::to_string(&policy).unwrap()
            ],
        )
        .unwrap();
    let stored = StoredTranscript {
        asset_id: asset.clone(),
        recipe_key: policy.recipe_version.clone(),
        source_fingerprint: policy.scope[0]
            .sampled_representation_fingerprint
            .clone()
            .unwrap(),
        status: TranscriptStatus::NoAudio,
        outcome_reason: TranscriptOutcomeReason::NoAudioStream,
        reject_reason: String::new(),
        primary_language: String::new(),
        speech_seconds: 0.0,
        window_count: 0,
        provider_segment_count: 0,
        mean_token_probability: None,
        repetition_ratio: None,
        segments: vec![],
    };
    // A real historical fixture has its own internally consistent frozen
    // authorization identity, rather than a changed header under a new policy.
    let connection = catalog.connection().unwrap();
    save_transcript(&connection, &stored).unwrap();
    connection
        .execute(
            "UPDATE asset_transcripts SET native_policy_id=?2,
            native_quality_recipe_version='native-stt.quality.historical.v0' WHERE asset_id=?1",
            params![asset, policy.policy_id],
        )
        .unwrap();
    let provenance = catalog
        .native_transcript_provenance(&asset)
        .unwrap()
        .unwrap();
    assert_eq!(provenance.recipe_key, policy.recipe_version);
    assert_eq!(provenance.policy, policy);
    assert_eq!(provenance.source_fingerprint, stored.source_fingerprint);
    assert!(provenance.windows.is_empty());
    assert_eq!(provenance.status, TranscriptStatus::NoAudio);
}
