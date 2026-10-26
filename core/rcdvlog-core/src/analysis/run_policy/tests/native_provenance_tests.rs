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

use crate::RepresentationKind;
use crate::analysis::accounting::NativeSttAccountedResult;
use crate::analysis::store::native_provenance::{
    NativeTranscriptCheckpoint, NativeTranscriptCheckpointWindowInput, NativeTranscriptWindowReuse,
};
use crate::analysis::windows::{PackedRegion, PackedWindow};
use crate::transcription::{NativeSpeechPreparation, NativeSpeechPreparationEvidence};
use std::path::PathBuf;

fn two_window_preparation(
    catalog: &Catalog,
    policy: &RunPolicy,
    asset_id: &str,
) -> NativeSpeechPreparation {
    let scope = policy
        .scope
        .iter()
        .find(|scope| scope.asset_id == asset_id)
        .unwrap()
        .clone();
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
    let windows = vec![
        PackedWindow {
            regions: vec![PackedRegion {
                window_offset_ms: 0,
                source_start_ms: 0,
                duration_ms: 500,
            }],
        },
        PackedWindow {
            regions: vec![PackedRegion {
                window_offset_ms: 0,
                source_start_ms: 1_000,
                duration_ms: 500,
            }],
        },
    ];
    NativeSpeechPreparation::test_preparation(evidence, proxy_path, proxy_claim, speech_claim)
        .test_with_speech(vec![0.1_f32; 32_000], windows)
}

fn native_call_for_window(
    audio: &NativeSttAudioWindow,
    include_cost: bool,
    text: &str,
) -> NativeSttCallResult {
    let seconds = f64::from(audio.duration_ms()) / 1_000.0;
    let mut payload = json!({
        "text": text,
        "language": "english",
        "duration": seconds,
        "segments": [{
            "id": 0,
            "start": 0.0,
            "end": seconds,
            "text": text
        }],
        "model": OPENROUTER_WHISPER_MODEL,
        "provider": "openai"
    });
    if include_cost {
        payload["usage"] = json!({
            "cost": 0.002,
            "seconds": seconds,
            "input_tokens": 12,
            "output_tokens": 4
        });
    }
    decode_native_stt_body(
        serde_json::to_vec(&payload).unwrap().as_slice(),
        audio.duration_ms(),
        Some(format!(
            "native-checkpoint-generation-{}",
            text.replace(' ', "-")
        )),
        11,
    )
}

fn transmit_window(
    catalog: &Catalog,
    policy: &RunPolicy,
    request: &NativeSttRequest,
    audio: &NativeSttAudioWindow,
    run_id: &str,
    context: AnalysisAccountingContext,
    text: &str,
) -> NativeSttAccountedResult {
    transmit_native(
        catalog,
        policy,
        request,
        audio,
        run_id,
        context,
        native_call_for_window(audio, true, text),
    )
}

#[test]
fn native_checkpoint_prefix_roundtrip_reuse_and_pilot_seed_without_send() {
    let (_directory, catalog, _project, asset, policy, _request, _audio, _run_id, context) =
        authorized_native_setup();
    let request = native_policy_request();
    let preparation = two_window_preparation(&catalog, &policy, &asset);
    let first = preparation.materialize_window(0).unwrap();

    let opened = catalog
        .begin_native_transcript_checkpoint(&preparation)
        .unwrap();
    assert_eq!(opened.coverage.planned_window_count, 2);
    assert_eq!(opened.coverage.completed_window_count, 0);
    assert!(opened.windows.is_empty());
    assert_eq!(opened.status, TranscriptStatus::Failed);
    assert_eq!(
        opened.outcome_reason,
        TranscriptOutcomeReason::NativeCoverageIncomplete
    );
    assert!(!opened.outcome_reason.is_terminal());
    assert!(catalog.prompt_transcript(&asset).unwrap().is_none());

    let first_result = transmit_window(
        &catalog,
        &policy,
        &request,
        &first,
        &context.run_id,
        context.clone(),
        "first paid window",
    );
    // Keep this test focused on direct accounted-result append; the pilot seed
    // path is exercised by a separate focused test.
    let first_checkpoint = catalog
        .append_native_transcript_checkpoint(
            &preparation,
            NativeTranscriptCheckpointWindowInput::AccountedResult {
                request: &request,
                audio: &first,
                result: &first_result,
            },
        )
        .unwrap();
    assert_eq!(first_checkpoint.windows.len(), 1);
    assert_eq!(first_checkpoint.coverage.remaining_window_count, 1);
    assert_eq!(
        first_checkpoint.windows[0].candidates[0].text,
        "first paid window"
    );
    assert!(catalog.prompt_transcript(&asset).unwrap().is_none());
    assert!(
        catalog
            .native_transcript_checkpoint(&asset)
            .unwrap()
            .unwrap()
            .windows
            .len()
            == 1
    );

    // The same request/result is idempotent and does not create another result.
    let idempotent = catalog
        .append_native_transcript_checkpoint(
            &preparation,
            NativeTranscriptCheckpointWindowInput::AccountedResult {
                request: &request,
                audio: &first,
                result: &first_result,
            },
        )
        .unwrap();
    assert_eq!(idempotent.windows.len(), 1);

    // A live current probe reuses the first exact payload and reports the second
    // window as genuinely missing, with no provider side effect.
    let reuse = catalog
        .probe_native_transcript_checkpoint_reuse(&preparation, &request)
        .unwrap();
    assert!(matches!(
        reuse.windows[0],
        NativeTranscriptWindowReuse::Cached(_)
    ));
    assert!(matches!(
        reuse.windows[1],
        NativeTranscriptWindowReuse::Missing { window_index: 1 }
    ));
    assert_eq!(
        catalog
            .list_analysis_requests(&policy.project_id, Some(&policy.policy_id), 10)
            .unwrap()
            .len(),
        1,
        "reuse probe must not issue a request"
    );

    let second = preparation.materialize_window(1).unwrap();
    let second_result = transmit_window(
        &catalog,
        &policy,
        &request,
        &second,
        &context.run_id,
        context.clone(),
        "second paid window",
    );
    let complete_prefix = catalog
        .append_native_transcript_checkpoint(
            &preparation,
            NativeTranscriptCheckpointWindowInput::AccountedResult {
                request: &request,
                audio: &second,
                result: &second_result,
            },
        )
        .unwrap();
    assert_eq!(complete_prefix.coverage.planned_window_count, 2);
    assert_eq!(complete_prefix.coverage.completed_window_count, 2);
    assert_eq!(complete_prefix.coverage.remaining_window_count, 0);
    assert_eq!(
        complete_prefix.status,
        TranscriptStatus::Failed,
        "C9 never finalizes a full Speech artifact"
    );
    assert_eq!(
        complete_prefix.outcome_reason,
        TranscriptOutcomeReason::NativeCoverageIncomplete
    );
    assert_eq!(
        complete_prefix.windows[1].candidates[0].text,
        "second paid window"
    );
    assert!(
        catalog.prompt_transcript(&asset).unwrap().is_none(),
        "incomplete native candidates must never enter AI context"
    );
    let all_cached = catalog
        .probe_native_transcript_checkpoint_reuse(&preparation, &request)
        .unwrap();
    assert!(
        all_cached
            .windows
            .iter()
            .all(|state| matches!(state, NativeTranscriptWindowReuse::Cached(_)))
    );
}

#[test]
fn native_checkpoint_pilot_sample_seeds_without_transport_and_unknown_cost_stays_uncertain() {
    let (_directory, catalog, _project, asset, policy, _request, _audio, _run_id, context) =
        authorized_native_setup();
    let request = native_policy_request();
    let preparation = two_window_preparation(&catalog, &policy, &asset);
    let first = preparation.materialize_window(0).unwrap();
    catalog
        .begin_native_transcript_checkpoint(&preparation)
        .unwrap();
    let call = native_call_for_window(&first, false, "paid but cost unknown");
    let result = transmit_native(
        &catalog,
        &policy,
        &request,
        &first,
        &context.run_id,
        context.clone(),
        call,
    );
    let record = catalog
        .analysis_request(&result.request_key)
        .unwrap()
        .unwrap();
    assert_eq!(record.lifecycle, crate::AnalysisRequestLifecycle::Uncertain);
    assert!(record.uncertain_usage_reserve_usd > 0.0);
    let scope = preparation.evidence().scope.clone();
    let sample = crate::NativePilotPaidSample::from_accounted_result(
        &policy, &scope, &request, &first, &result,
    )
    .unwrap();
    let running = super::native_pilot_evidence_tests::install_running_pilot(
        &catalog,
        &policy,
        &context.run_id,
        &asset,
    );
    catalog
        .attach_native_pilot_evidence(
            &running,
            crate::NativePilotEvidence::with_sample(sample.clone()),
            Some((&request, &first)),
        )
        .unwrap()
        .unwrap();
    let mut altered = sample.clone();
    if let crate::NativePilotSampleOutcome::Evaluated { candidates, .. } = &mut altered.outcome {
        candidates[0].text = "mutated cached candidate".into();
    }
    assert!(
        catalog
            .append_native_transcript_checkpoint(
                &preparation,
                NativeTranscriptCheckpointWindowInput::PilotSample {
                    request: &request,
                    audio: &first,
                    sample: &altered
                }
            )
            .is_err(),
        "a supplied clone must equal canonical pilot evidence"
    );
    assert!(
        catalog
            .native_transcript_checkpoint(&asset)
            .unwrap()
            .unwrap()
            .windows
            .is_empty()
    );
    let before_requests = catalog
        .list_analysis_requests(&policy.project_id, Some(&policy.policy_id), 10)
        .unwrap()
        .len();
    let checkpoint = catalog
        .append_native_transcript_checkpoint(
            &preparation,
            NativeTranscriptCheckpointWindowInput::PilotSample {
                request: &request,
                audio: &first,
                sample: &sample,
            },
        )
        .unwrap();
    assert_eq!(checkpoint.windows.len(), 1);
    assert_eq!(
        catalog
            .list_analysis_requests(&policy.project_id, Some(&policy.policy_id), 10)
            .unwrap()
            .len(),
        before_requests,
        "seeding from the exact C7 sample must not send or reserve"
    );
    let provenance = catalog
        .native_transcript_provenance(&asset)
        .unwrap()
        .unwrap();
    assert_eq!(
        provenance.windows[0].accounting.request.lifecycle,
        crate::AnalysisRequestLifecycle::Uncertain
    );
    assert!(
        provenance.windows[0]
            .accounting
            .request
            .uncertain_usage_reserve_usd
            > 0.0
    );
    assert_eq!(
        provenance.windows[0].candidates.as_ref().unwrap()[0].text,
        "paid but cost unknown"
    );
}

#[test]
fn native_checkpoint_paid_marker_without_result_blocks_default_replay() {
    let (_directory, catalog, _project, asset, policy, _request, _audio, _run_id, context) =
        authorized_native_setup();
    let request = native_policy_request();
    let preparation = two_window_preparation(&catalog, &policy, &asset);
    let first = preparation.materialize_window(0).unwrap();
    catalog
        .begin_native_transcript_checkpoint(&preparation)
        .unwrap();
    let result = transmit_window(
        &catalog,
        &policy,
        &request,
        &first,
        &context.run_id,
        context.clone(),
        "receipt without checkpoint",
    );
    let reuse = catalog
        .probe_native_transcript_checkpoint_reuse(&preparation, &request)
        .unwrap();
    match &reuse.windows[0] {
        NativeTranscriptWindowReuse::RecoveryRequired {
            window_index,
            request_key,
        } => {
            assert_eq!(*window_index, 0);
            assert_eq!(request_key.as_deref(), Some(result.request_key.as_str()));
        }
        other => panic!("expected recovery-required marker, got {other:?}"),
    }
    assert_eq!(
        catalog
            .native_transcript_checkpoint(&asset)
            .unwrap()
            .unwrap()
            .windows
            .len(),
        0,
        "a receipt or marker never fabricates candidate text"
    );
}

#[test]
fn native_checkpoint_evaluated_append_is_quality_mapped_and_conflicts_roll_back() {
    let (_directory, catalog, _project, asset, policy, _request, _audio, _run_id, context) =
        authorized_native_setup();
    let request = native_policy_request();
    let preparation = two_window_preparation(&catalog, &policy, &asset);
    let first = preparation.materialize_window(0).unwrap();
    catalog
        .begin_native_transcript_checkpoint(&preparation)
        .unwrap();
    let result = transmit_window(
        &catalog,
        &policy,
        &request,
        &first,
        &context.run_id,
        context.clone(),
        "evaluated synthetic window",
    );
    let evaluation = evaluation_for(&result, &first);
    let observation = NativeSttWindowObservation {
        request_key: result.request_key.clone(),
        latency_ms: result.latency_ms(),
        reported_language: evaluation.reported_language.clone(),
        quality: crate::analysis::store::NativeSttWindowQualityObservation::from_evaluation(
            &evaluation,
        )
        .unwrap(),
    };
    let checkpoint = catalog
        .append_native_transcript_checkpoint(
            &preparation,
            NativeTranscriptCheckpointWindowInput::Evaluated {
                request: &request,
                audio: &first,
                observation: observation.clone(),
                candidates: evaluation.segments.clone(),
            },
        )
        .unwrap();
    assert_eq!(checkpoint.windows.len(), 1);

    let before = catalog
        .native_transcript_checkpoint(&asset)
        .unwrap()
        .unwrap();
    let mut conflicting = evaluation.segments.clone();
    conflicting[0].text = "forged candidate".into();
    let error = catalog
        .append_native_transcript_checkpoint(
            &preparation,
            NativeTranscriptCheckpointWindowInput::Evaluated {
                request: &request,
                audio: &first,
                observation,
                candidates: conflicting,
            },
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("conflicting result"));
    let after = catalog
        .native_transcript_checkpoint(&asset)
        .unwrap()
        .unwrap();
    assert_eq!(
        after, before,
        "a conflicting append must not mutate the paid prefix"
    );
    assert_eq!(
        after.windows[0].candidates[0].text,
        "evaluated synthetic window"
    );
}

#[test]
fn native_checkpoint_local_replacement_clears_plan_and_native_links_atomically() {
    let (_directory, catalog, _project, asset, policy, _request, _audio, _run_id, context) =
        authorized_native_setup();
    let request = native_policy_request();
    let preparation = two_window_preparation(&catalog, &policy, &asset);
    let first = preparation.materialize_window(0).unwrap();
    catalog
        .begin_native_transcript_checkpoint(&preparation)
        .unwrap();
    let result = transmit_window(
        &catalog,
        &policy,
        &request,
        &first,
        &context.run_id,
        context.clone(),
        "paid prefix before local replacement",
    );
    catalog
        .append_native_transcript_checkpoint(
            &preparation,
            NativeTranscriptCheckpointWindowInput::AccountedResult {
                request: &request,
                audio: &first,
                result: &result,
            },
        )
        .unwrap();
    assert_eq!(link_count(&catalog, &asset), 1);

    let connection = catalog.connection().unwrap();
    let source_fingerprint: String = connection
        .query_row(
            "SELECT source_fingerprint FROM asset_transcripts WHERE asset_id=?1",
            [&asset],
            |row| row.get(0),
        )
        .unwrap();
    save_transcript(
        &connection,
        &StoredTranscript {
            asset_id: asset.clone(),
            recipe_key: crate::STT_RECIPE_KEY.to_string(),
            source_fingerprint,
            status: TranscriptStatus::Speech,
            outcome_reason: TranscriptOutcomeReason::Accepted,
            reject_reason: String::new(),
            primary_language: "en".to_string(),
            speech_seconds: 1.0,
            window_count: 1,
            provider_segment_count: 1,
            mean_token_probability: Some(0.9),
            repetition_ratio: Some(0.0),
            segments: vec![crate::DecodedSegment {
                start_ms: 0,
                end_ms: 500,
                language: "en".to_string(),
                text: "local replacement".to_string(),
                mean_token_probability: Some(0.9),
            }],
        },
    )
    .unwrap();
    let row: (Option<String>, Option<String>, String) = connection
        .query_row(
            "SELECT native_window_plan_json,native_window_plan_fingerprint,status
             FROM asset_transcripts WHERE asset_id=?1",
            [&asset],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(row, (None, None, "speech".to_string()));
    assert_eq!(link_count(&catalog, &asset), 0);
}

#[test]
fn native_checkpoint_changed_source_rejects_current_reuse_but_history_stays_readable() {
    let (_directory, catalog, _project, asset, policy, _request, _audio, _run_id, context) =
        authorized_native_setup();
    let request = native_policy_request();
    let preparation = two_window_preparation(&catalog, &policy, &asset);
    let first = preparation.materialize_window(0).unwrap();
    catalog
        .begin_native_transcript_checkpoint(&preparation)
        .unwrap();
    let result = transmit_window(
        &catalog,
        &policy,
        &request,
        &first,
        &context.run_id,
        context.clone(),
        "historical checkpoint",
    );
    catalog
        .append_native_transcript_checkpoint(
            &preparation,
            NativeTranscriptCheckpointWindowInput::AccountedResult {
                request: &request,
                audio: &first,
                result: &result,
            },
        )
        .unwrap();

    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET duration_seconds=duration_seconds+1 WHERE id=?1",
            [&asset],
        )
        .unwrap();
    let reuse_error = catalog
        .probe_native_transcript_checkpoint_reuse(&preparation, &request)
        .unwrap_err();
    assert!(
        format!("{reuse_error:#}").contains("source identity changed")
            || format!("{reuse_error:#}").contains("source"),
        "changed source must reject current reuse: {reuse_error:#}"
    );
    let historical = catalog
        .native_transcript_checkpoint(&asset)
        .unwrap()
        .unwrap();
    assert_eq!(historical.windows.len(), 1);
    assert_eq!(
        historical.windows[0].candidates[0].text,
        "historical checkpoint"
    );
    let provenance = catalog
        .native_transcript_provenance(&asset)
        .unwrap()
        .unwrap();
    assert_eq!(provenance.coverage.unwrap().planned_window_count, 2);
}

#[test]
fn native_checkpoint_invalid_quality_and_sql_failure_never_replaces_paid_prefix() {
    let (_directory, catalog, _project, asset, policy, _request, _audio, _run_id, context) =
        authorized_native_setup();
    let request = native_policy_request();
    let preparation = two_window_preparation(&catalog, &policy, &asset);
    let first = preparation.materialize_window(0).unwrap();
    let second = preparation.materialize_window(1).unwrap();
    catalog
        .begin_native_transcript_checkpoint(&preparation)
        .unwrap();
    let first_result = transmit_window(
        &catalog,
        &policy,
        &request,
        &first,
        &context.run_id,
        context.clone(),
        "first stable prefix",
    );
    catalog
        .append_native_transcript_checkpoint(
            &preparation,
            NativeTranscriptCheckpointWindowInput::AccountedResult {
                request: &request,
                audio: &first,
                result: &first_result,
            },
        )
        .unwrap();
    let before = catalog
        .native_transcript_checkpoint(&asset)
        .unwrap()
        .unwrap();

    let second_result = transmit_window(
        &catalog,
        &policy,
        &request,
        &second,
        &context.run_id,
        context.clone(),
        "trigger-fail second prefix",
    );
    let evaluation = evaluation_for(&second_result, &second);
    let mut invalid_quality =
        crate::analysis::store::NativeSttWindowQualityObservation::from_evaluation(&evaluation)
            .unwrap();
    invalid_quality.decision = NativeSttQualityDecision::Rejected;
    invalid_quality.reason =
        crate::analysis::quality::native_stt::NativeSttQualityReason::InvalidTimestamp;
    let invalid = catalog.append_native_transcript_checkpoint(
        &preparation,
        NativeTranscriptCheckpointWindowInput::Evaluated {
            request: &request,
            audio: &second,
            observation: NativeSttWindowObservation {
                request_key: second_result.request_key.clone(),
                latency_ms: second_result.latency_ms(),
                reported_language: evaluation.reported_language.clone(),
                quality: invalid_quality,
            },
            candidates: Vec::new(),
        },
    );
    assert!(invalid.is_err(), "structurally invalid quality must deny");
    assert_eq!(
        catalog
            .native_transcript_checkpoint(&asset)
            .unwrap()
            .unwrap(),
        before
    );

    catalog
        .connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER synthetic_checkpoint_abort
             BEFORE INSERT ON transcript_segments
             WHEN NEW.text = 'trigger-fail second prefix'
             BEGIN SELECT RAISE(ABORT, 'synthetic checkpoint persistence failure'); END;",
        )
        .unwrap();
    let append = catalog.append_native_transcript_checkpoint(
        &preparation,
        NativeTranscriptCheckpointWindowInput::AccountedResult {
            request: &request,
            audio: &second,
            result: &second_result,
        },
    );
    assert!(append.is_err(), "synthetic SQL failure must surface");
    assert_eq!(
        catalog
            .native_transcript_checkpoint(&asset)
            .unwrap()
            .unwrap(),
        before,
        "SQL failure must roll back header/segments/window links together"
    );
    assert_eq!(link_count(&catalog, &asset), 1);
}

#[allow(clippy::too_many_arguments)]
fn transmit_configured_window(
    catalog: &Catalog,
    policy: &RunPolicy,
    request: &NativeSttRequest,
    audio: &NativeSttAudioWindow,
    run_id: &str,
    context: AnalysisAccountingContext,
    text: &str,
    include_cost: bool,
    confidence: Option<f64>,
) -> NativeSttAccountedResult {
    let mut call = native_call_for_window(audio, include_cost, text);
    if let NativeSttOutcome::Usable(transcript) = &mut call.outcome {
        transcript.confidence = confidence;
    }
    transmit_native(catalog, policy, request, audio, run_id, context, call)
}

fn append_native_window(
    catalog: &Catalog,
    preparation: &NativeSpeechPreparation,
    request: &NativeSttRequest,
    audio: &NativeSttAudioWindow,
    result: &NativeSttAccountedResult,
) -> NativeTranscriptCheckpoint {
    catalog
        .append_native_transcript_checkpoint(
            preparation,
            NativeTranscriptCheckpointWindowInput::AccountedResult {
                request,
                audio,
                result,
            },
        )
        .unwrap()
}

#[test]
fn native_complete_checkpoint_finalizes_atomically_with_exact_terminal_coverage() {
    let (_directory, catalog, project, asset, policy, _request, _audio, _run_id, context) =
        authorized_native_setup();
    let request = native_policy_request();
    let preparation = two_window_preparation(&catalog, &policy, &asset);
    catalog
        .begin_native_transcript_checkpoint(&preparation)
        .unwrap();
    let first = preparation.materialize_window(0).unwrap();
    let second = preparation.materialize_window(1).unwrap();
    let first_result = transmit_configured_window(
        &catalog,
        &policy,
        &request,
        &first,
        &context.run_id,
        context.clone(),
        "atomic first window",
        true,
        Some(0.9),
    );
    let second_result = transmit_configured_window(
        &catalog,
        &policy,
        &request,
        &second,
        &context.run_id,
        context.clone(),
        "atomic second window",
        true,
        Some(0.8),
    );
    append_native_window(&catalog, &preparation, &request, &first, &first_result);
    let complete = append_native_window(&catalog, &preparation, &request, &second, &second_result);
    assert_eq!(complete.status, TranscriptStatus::Failed);
    let request_count_before = catalog
        .list_analysis_requests(&project, Some(&policy.policy_id), 10)
        .unwrap()
        .len();

    let finalized = catalog
        .finalize_native_transcript_checkpoint(&preparation, &request)
        .unwrap();
    assert_eq!(finalized.status, TranscriptStatus::Speech);
    assert_eq!(finalized.outcome_reason, TranscriptOutcomeReason::Accepted);
    assert_eq!(finalized.window_count, 2);
    assert_eq!(finalized.provider_segment_count, 2);
    assert_eq!(finalized.speech_seconds, 1.0);
    assert_eq!(finalized.repetition_ratio, Some(0.0));
    assert_eq!(finalized.mean_token_probability, None);
    assert_eq!(finalized.segments.len(), 2);
    assert!(
        finalized
            .segments
            .iter()
            .all(|segment| segment.mean_token_probability.is_none())
    );
    assert_eq!(
        catalog
            .list_analysis_requests(&project, Some(&policy.policy_id), 10)
            .unwrap()
            .len(),
        request_count_before,
        "finalization must not add or change accounting requests"
    );
    assert!(
        catalog
            .native_transcript_checkpoint(&asset)
            .unwrap()
            .is_none(),
        "terminal native artifacts are not incomplete checkpoints"
    );
    let provenance = catalog
        .native_transcript_provenance(&asset)
        .unwrap()
        .unwrap();
    assert_eq!(provenance.status, TranscriptStatus::Speech);
    let coverage = provenance
        .coverage
        .expect("terminal retained plan coverage");
    assert_eq!(coverage.planned_window_count, 2);
    assert_eq!(coverage.completed_window_count, 2);
    assert_eq!(coverage.remaining_window_count, 0);
    assert!(provenance.windows.iter().all(|window| {
        window
            .candidates
            .as_ref()
            .is_some_and(|candidates| !candidates.is_empty())
    }));
    let plan_columns: (Option<String>, Option<String>) = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT native_window_plan_json,native_window_plan_fingerprint
             FROM asset_transcripts WHERE asset_id=?1",
            [&asset],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(plan_columns.0.is_some() && plan_columns.1.is_some());
    // The shared caller-owned transaction helper validates the frozen plan
    // fingerprint itself rather than trusting only the higher-level finalizer.
    let observations: Vec<_> = provenance
        .windows
        .iter()
        .map(|window| NativeSttWindowObservation {
            request_key: window.accounting.request.request_key.clone(),
            latency_ms: window.latency_ms,
            reported_language: window.reported_language.clone(),
            quality: window.quality.clone(),
        })
        .collect();
    let mut connection = catalog.connection().unwrap();
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let error = crate::analysis::store::native_provenance::save_native_transcript_on_transaction(
        &transaction,
        &policy.policy_id,
        &finalized,
        &observations,
        Some(crate::analysis::store::NativeTranscriptWindowPlanRow {
            plan_json: plan_columns.0.as_deref().unwrap(),
            fingerprint: &"0".repeat(64),
        }),
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("fingerprint does not match"),
        "{error:#}"
    );
    drop(transaction);
    drop(connection);

    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE asset_transcripts SET created_at='sentinel-terminal-row' WHERE asset_id=?1",
            [&asset],
        )
        .unwrap();
    let idempotent = catalog
        .finalize_native_transcript_checkpoint(&preparation, &request)
        .unwrap();
    assert_eq!(idempotent, finalized);
    let created_at: String = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT created_at FROM asset_transcripts WHERE asset_id=?1",
            [&asset],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(created_at, "sentinel-terminal-row");
    assert_eq!(
        catalog
            .native_transcript_provenance(&asset)
            .unwrap()
            .unwrap()
            .coverage
            .unwrap()
            .completed_window_count,
        2
    );
    // A safe stored summary cannot mask repetition across independently safe
    // paid windows during idempotent finalization.
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE transcript_segments SET text=?1 WHERE asset_id=?2 AND segment_index=1",
            rusqlite::params![finalized.segments[0].text, asset],
        )
        .unwrap();
    let error = catalog
        .finalize_native_transcript_checkpoint(&preparation, &request)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("whole-clip repetition"),
        "{error:#}"
    );
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE transcript_segments SET text=?1 WHERE asset_id=?2 AND segment_index=1",
            rusqlite::params![finalized.segments[1].text, asset],
        )
        .unwrap();
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE asset_transcripts SET primary_language='ja' WHERE asset_id=?1",
            [&asset],
        )
        .unwrap();
    let error = catalog
        .finalize_native_transcript_checkpoint(&preparation, &request)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("summary disagrees"),
        "{error:#}"
    );
}

#[test]
fn native_complete_finalization_sql_failure_and_current_pcm_mismatch_keep_c9_checkpoint() {
    let (_directory, catalog, _project, asset, policy, _request, _audio, _run_id, context) =
        authorized_native_setup();
    let request = native_policy_request();
    let preparation = two_window_preparation(&catalog, &policy, &asset);
    catalog
        .begin_native_transcript_checkpoint(&preparation)
        .unwrap();
    let first = preparation.materialize_window(0).unwrap();
    let second = preparation.materialize_window(1).unwrap();
    let first_result = transmit_configured_window(
        &catalog,
        &policy,
        &request,
        &first,
        &context.run_id,
        context.clone(),
        "trigger rollback first",
        true,
        Some(0.9),
    );
    let second_result = transmit_configured_window(
        &catalog,
        &policy,
        &request,
        &second,
        &context.run_id,
        context.clone(),
        "trigger rollback second",
        true,
        Some(0.9),
    );
    append_native_window(&catalog, &preparation, &request, &first, &first_result);
    append_native_window(&catalog, &preparation, &request, &second, &second_result);
    let before = catalog
        .native_transcript_checkpoint(&asset)
        .unwrap()
        .unwrap();
    catalog
        .connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER synthetic_assembly_abort
             BEFORE INSERT ON transcript_segments
             WHEN NEW.text = 'trigger rollback first'
             BEGIN SELECT RAISE(ABORT, 'synthetic assembly persistence failure'); END;",
        )
        .unwrap();
    assert!(
        catalog
            .finalize_native_transcript_checkpoint(&preparation, &request)
            .is_err(),
        "SQL failure must surface"
    );
    assert_eq!(
        catalog
            .native_transcript_checkpoint(&asset)
            .unwrap()
            .unwrap(),
        before,
        "failed finalization must retain the whole C9 checkpoint"
    );
    assert_eq!(link_count(&catalog, &asset), 2);
    catalog
        .connection()
        .unwrap()
        .execute_batch("DROP TRIGGER synthetic_assembly_abort")
        .unwrap();

    let maps = preparation.windows().to_vec();
    let changed_pcm = preparation.test_with_speech(vec![0.9_f32; 32_000], maps);
    let error = catalog
        .finalize_native_transcript_checkpoint(&changed_pcm, &request)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("prepared payload"),
        "{error:#}"
    );
    assert_eq!(
        catalog
            .native_transcript_checkpoint(&asset)
            .unwrap()
            .unwrap(),
        before
    );
}

#[test]
fn native_complete_pilot_seed_finalizes_unknown_usage_without_new_request() {
    let (_directory, catalog, project, asset, policy, _request, _audio, _run_id, context) =
        authorized_native_setup();
    let request = native_policy_request();
    let preparation = two_window_preparation(&catalog, &policy, &asset);
    let first = preparation.materialize_window(0).unwrap();
    catalog
        .begin_native_transcript_checkpoint(&preparation)
        .unwrap();
    let first_result = transmit_configured_window(
        &catalog,
        &policy,
        &request,
        &first,
        &context.run_id,
        context.clone(),
        "unknown cost first window",
        false,
        Some(0.9),
    );
    let scope = preparation.evidence().scope.clone();
    let sample = crate::NativePilotPaidSample::from_accounted_result(
        &policy,
        &scope,
        &request,
        &first,
        &first_result,
    )
    .unwrap();
    let running = super::native_pilot_evidence_tests::install_running_pilot(
        &catalog,
        &policy,
        &context.run_id,
        &asset,
    );
    catalog
        .attach_native_pilot_evidence(
            &running,
            crate::NativePilotEvidence::with_sample(sample.clone()),
            Some((&request, &first)),
        )
        .unwrap()
        .unwrap();
    let first_checkpoint = catalog
        .append_native_transcript_checkpoint(
            &preparation,
            NativeTranscriptCheckpointWindowInput::PilotSample {
                request: &request,
                audio: &first,
                sample: &sample,
            },
        )
        .unwrap();
    assert_eq!(first_checkpoint.windows.len(), 1);

    let second = preparation.materialize_window(1).unwrap();
    let second_result = transmit_configured_window(
        &catalog,
        &policy,
        &request,
        &second,
        &context.run_id,
        context.clone(),
        "known cost second window",
        true,
        Some(0.9),
    );
    append_native_window(&catalog, &preparation, &request, &second, &second_result);
    let request_count_before = catalog
        .list_analysis_requests(&project, Some(&policy.policy_id), 10)
        .unwrap()
        .len();
    let finalized = catalog
        .finalize_native_transcript_checkpoint(&preparation, &request)
        .unwrap();
    assert_eq!(finalized.status, TranscriptStatus::Speech);
    assert_eq!(
        catalog
            .list_analysis_requests(&project, Some(&policy.policy_id), 10)
            .unwrap()
            .len(),
        request_count_before,
        "finalization from the C7 paid seed must not pay again"
    );
    let provenance = catalog
        .native_transcript_provenance(&asset)
        .unwrap()
        .unwrap();
    assert_eq!(
        provenance.windows[0].accounting.request.lifecycle,
        crate::AnalysisRequestLifecycle::Uncertain
    );
    assert!(
        provenance.windows[0]
            .accounting
            .request
            .uncertain_usage_reserve_usd
            > 0.0
    );
    assert_eq!(
        provenance.windows[1].accounting.request.lifecycle,
        crate::AnalysisRequestLifecycle::Completed
    );
}

#[test]
fn native_complete_finalization_rejects_wrong_policy_request_or_changed_source() {
    let (_directory, catalog, _project, asset, policy, _request, _audio, _run_id, context) =
        authorized_native_setup();
    let request = native_policy_request();
    let preparation = two_window_preparation(&catalog, &policy, &asset);
    catalog
        .begin_native_transcript_checkpoint(&preparation)
        .unwrap();
    let first = preparation.materialize_window(0).unwrap();
    let second = preparation.materialize_window(1).unwrap();
    let first_result = transmit_configured_window(
        &catalog,
        &policy,
        &request,
        &first,
        &context.run_id,
        context.clone(),
        "policy first window",
        true,
        Some(0.9),
    );
    let second_result = transmit_configured_window(
        &catalog,
        &policy,
        &request,
        &second,
        &context.run_id,
        context.clone(),
        "policy second window",
        true,
        Some(0.9),
    );
    append_native_window(&catalog, &preparation, &request, &first, &first_result);
    append_native_window(&catalog, &preparation, &request, &second, &second_result);
    let before = catalog
        .native_transcript_checkpoint(&asset)
        .unwrap()
        .unwrap();

    let wrong_language = NativeSttRequest::new(
        OPENROUTER_STT_PROVIDER,
        OPENROUTER_WHISPER_MODEL,
        Some("es".into()),
        0.0,
        NativeSttProviderPolicy::new(true, SttDataCollection::Deny),
    )
    .unwrap();
    assert!(
        catalog
            .finalize_native_transcript_checkpoint(&preparation, &wrong_language)
            .is_err()
    );
    assert_eq!(
        catalog
            .native_transcript_checkpoint(&asset)
            .unwrap()
            .unwrap(),
        before
    );

    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET duration_seconds=duration_seconds+1 WHERE id=?1",
            [&asset],
        )
        .unwrap();
    assert!(
        catalog
            .finalize_native_transcript_checkpoint(&preparation, &request)
            .is_err()
    );
    assert_eq!(
        catalog
            .native_transcript_checkpoint(&asset)
            .unwrap()
            .unwrap(),
        before
    );

    catalog
        .connection()
        .unwrap()
        .execute("UPDATE assets SET removed=1 WHERE id=?1", [&asset])
        .unwrap();
    assert!(
        catalog
            .finalize_native_transcript_checkpoint(&preparation, &request)
            .is_err()
    );
}

#[test]
fn terminal_native_provenance_local_replacement_and_legacy_complete_wire_shape() {
    let (_directory, catalog, project, asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let prepared = two_window_preparation(&catalog, &policy, &asset);
    let first = prepared.materialize_window(0).unwrap();
    let second = prepared.materialize_window(1).unwrap();
    catalog
        .begin_native_transcript_checkpoint(&prepared)
        .unwrap();
    let first_result = transmit_configured_window(
        &catalog,
        &policy,
        &request,
        &first,
        &run_id,
        context.clone(),
        "local replacement first",
        true,
        Some(0.9),
    );
    let second_result = transmit_configured_window(
        &catalog,
        &policy,
        &request,
        &second,
        &run_id,
        context.clone(),
        "local replacement second",
        true,
        Some(0.9),
    );
    append_native_window(&catalog, &prepared, &request, &first, &first_result);
    append_native_window(&catalog, &prepared, &request, &second, &second_result);
    let finalized = catalog
        .finalize_native_transcript_checkpoint(&prepared, &request)
        .unwrap();
    let request_count = catalog
        .list_analysis_requests(&project, Some(&policy.policy_id), 10)
        .unwrap()
        .len();

    let local = StoredTranscript {
        asset_id: asset.clone(),
        recipe_key: crate::STT_RECIPE_KEY.into(),
        source_fingerprint: finalized.source_fingerprint.clone(),
        status: TranscriptStatus::Speech,
        outcome_reason: TranscriptOutcomeReason::Accepted,
        reject_reason: String::new(),
        primary_language: "en".into(),
        speech_seconds: 1.0,
        window_count: 1,
        provider_segment_count: 1,
        mean_token_probability: Some(0.9),
        repetition_ratio: Some(0.0),
        segments: vec![crate::analysis::quality::DecodedSegment {
            start_ms: 0,
            end_ms: 500,
            language: "en".into(),
            text: "ordinary local replacement".into(),
            mean_token_probability: Some(0.9),
        }],
    };
    save_transcript(&catalog.connection().unwrap(), &local).unwrap();
    assert!(
        catalog
            .native_transcript_provenance(&asset)
            .unwrap()
            .is_none()
    );
    let cleared: (Option<String>, Option<String>, i64) = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT native_window_plan_json,native_window_plan_fingerprint,
                    (SELECT COUNT(*) FROM native_transcript_windows WHERE asset_id=?1)
             FROM asset_transcripts WHERE asset_id=?1",
            [&asset],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(cleared, (None, None, 0));
    assert_eq!(
        catalog
            .list_analysis_requests(&project, Some(&policy.policy_id), 10)
            .unwrap()
            .len(),
        request_count,
        "ordinary local replacement must not touch canonical accounting"
    );

    // A legacy complete native artifact without a plan remains readable and keeps
    // the omitted-coverage wire shape: no fabricated terminal plan/progress.
    let call = configured_native_call(&audio, true, Some(0.9));
    let result = transmit_native(&catalog, &policy, &request, &audio, &run_id, context, call);
    let evaluation = evaluation_for(&result, &audio);
    let legacy = native_transcript_for_evaluation(&policy, &asset, &audio, &result, &evaluation);
    let observation = observation_for(&result, &evaluation);
    let mut connection = catalog.connection().unwrap();
    save_native_transcript(
        &mut connection,
        &policy.policy_id,
        &legacy,
        std::slice::from_ref(&observation),
    )
    .unwrap();
    drop(connection);
    let provenance = catalog
        .native_transcript_provenance(&asset)
        .unwrap()
        .unwrap();
    assert!(provenance.coverage.is_none());
    let json = serde_json::to_string(&provenance).unwrap();
    assert!(!json.contains("\"coverage\""));
}

#[test]
fn terminal_clip_repetition_rejection_discards_text_and_replays_idempotently() {
    let (_directory, catalog, _project, asset, policy, _request, _audio, _run_id, context) =
        authorized_native_setup();
    let request = native_policy_request();
    let preparation = two_window_preparation(&catalog, &policy, &asset);
    catalog
        .begin_native_transcript_checkpoint(&preparation)
        .unwrap();
    let first = preparation.materialize_window(0).unwrap();
    let second = preparation.materialize_window(1).unwrap();
    let mut first_call = native_call_for_window(&first, true, "repeated across windows");
    first_call.receipt.generation_id = Some("terminal-repeat-first".into());
    if let NativeSttOutcome::Usable(transcript) = &mut first_call.outcome {
        transcript.confidence = Some(0.9);
    }
    let first_result = transmit_native(
        &catalog,
        &policy,
        &request,
        &first,
        &context.run_id,
        context.clone(),
        first_call,
    );
    let mut second_call = native_call_for_window(&second, true, "repeated across windows");
    second_call.receipt.generation_id = Some("terminal-repeat-second".into());
    if let NativeSttOutcome::Usable(transcript) = &mut second_call.outcome {
        transcript.confidence = Some(0.9);
    }
    let second_result = transmit_native(
        &catalog,
        &policy,
        &request,
        &second,
        &context.run_id,
        context.clone(),
        second_call,
    );
    append_native_window(&catalog, &preparation, &request, &first, &first_result);
    append_native_window(&catalog, &preparation, &request, &second, &second_result);

    let finalized = catalog
        .finalize_native_transcript_checkpoint(&preparation, &request)
        .unwrap();
    assert_eq!(finalized.status, TranscriptStatus::Rejected);
    assert_eq!(
        finalized.outcome_reason,
        TranscriptOutcomeReason::QualityRejected
    );
    assert_eq!(finalized.reject_reason, "repetition");
    assert_eq!(finalized.repetition_ratio, Some(0.5));
    assert!(finalized.segments.is_empty());
    let provenance = catalog
        .native_transcript_provenance(&asset)
        .unwrap()
        .unwrap();
    assert_eq!(provenance.status, TranscriptStatus::Rejected);
    assert_eq!(provenance.windows.len(), 2);
    assert!(provenance.windows.iter().all(|window| {
        window.quality.decision == NativeSttQualityDecision::Accepted
            && window
                .candidates
                .as_ref()
                .is_some_and(|candidates| candidates.is_empty())
    }));
    assert_eq!(link_count(&catalog, &asset), 2);

    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE asset_transcripts SET created_at='sentinel-rejected' WHERE asset_id=?1",
            [&asset],
        )
        .unwrap();
    let replay = catalog
        .finalize_native_transcript_checkpoint(&preparation, &request)
        .unwrap();
    assert_eq!(replay, finalized);
    let created_at: String = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT created_at FROM asset_transcripts WHERE asset_id=?1",
            [&asset],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(created_at, "sentinel-rejected");
}
#[path = "native_checkpoint_regressions.rs"]
mod native_checkpoint_regressions;
