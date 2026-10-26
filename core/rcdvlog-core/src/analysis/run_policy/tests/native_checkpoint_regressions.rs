//! Supervisor regressions for exact input and paid-result recovery.
use super::*;

#[test]
fn native_checkpoint_rejects_supplied_pcm_that_does_not_match_preparation() {
    let (_directory, catalog, _project, asset, policy, _request, _audio, _run, context) =
        authorized_native_setup();
    let request = native_policy_request();
    let preparation = two_window_preparation(&catalog, &policy, &asset);
    catalog
        .begin_native_transcript_checkpoint(&preparation)
        .unwrap();
    let foreign_pcm = vec![0.2_f32; 32_000];
    let foreign_audio =
        NativeSttAudioWindow::from_pcm(&foreign_pcm, &preparation.windows()[0]).unwrap();
    let result = transmit_window(
        &catalog,
        &policy,
        &request,
        &foreign_audio,
        &context.run_id,
        context.clone(),
        "different paid PCM",
    );
    let error = catalog
        .append_native_transcript_checkpoint(
            &preparation,
            NativeTranscriptCheckpointWindowInput::AccountedResult {
                request: &request,
                audio: &foreign_audio,
                result: &result,
            },
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("current prepared PCM"),
        "{error:#}"
    );
    assert!(
        catalog
            .native_transcript_checkpoint(&asset)
            .unwrap()
            .unwrap()
            .windows
            .is_empty()
    );
}

#[test]
fn native_checkpoint_cannot_carry_an_old_prefix_into_changed_prepared_pcm() {
    let (_directory, catalog, _project, asset, policy, _request, _audio, _run, context) =
        authorized_native_setup();
    let request = native_policy_request();
    let preparation = two_window_preparation(&catalog, &policy, &asset);
    catalog
        .begin_native_transcript_checkpoint(&preparation)
        .unwrap();
    let first = preparation.materialize_window(0).unwrap();
    let result = transmit_window(
        &catalog,
        &policy,
        &request,
        &first,
        &context.run_id,
        context.clone(),
        "original prefix",
    );
    let before = catalog
        .append_native_transcript_checkpoint(
            &preparation,
            NativeTranscriptCheckpointWindowInput::AccountedResult {
                request: &request,
                audio: &first,
                result: &result,
            },
        )
        .unwrap();
    let maps = preparation.windows().to_vec();
    let preparation = preparation.test_with_speech(vec![0.2_f32; 32_000], maps);
    let second = preparation.materialize_window(1).unwrap();
    let result = transmit_window(
        &catalog,
        &policy,
        &request,
        &second,
        &context.run_id,
        context.clone(),
        "new decode second window",
    );
    let error = catalog
        .append_native_transcript_checkpoint(
            &preparation,
            NativeTranscriptCheckpointWindowInput::AccountedResult {
                request: &request,
                audio: &second,
                result: &result,
            },
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("completed window"),
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
fn native_checkpoint_recovery_fence_is_not_truncated_by_newer_released_requests() {
    let (_directory, catalog, _project, _asset, policy, _request, _audio, _run, context) =
        authorized_native_setup();
    let request = native_policy_request();
    let preparation = two_window_preparation(&catalog, &policy, &policy.scope[0].asset_id);
    catalog
        .begin_native_transcript_checkpoint(&preparation)
        .unwrap();
    let first = preparation.materialize_window(0).unwrap();
    let result = transmit_window(
        &catalog,
        &policy,
        &request,
        &first,
        &context.run_id,
        context.clone(),
        "older paid reply without checkpoint",
    );
    // Manufacture a large, zero-cost local-failure history. It must not hide an
    // older paid request when determining whether this exact payload is new.
    let mut connection = catalog.connection().unwrap();
    let columns = connection
        .prepare("SELECT name FROM pragma_table_info('analysis_request_reservations') ORDER BY cid")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let expressions: Vec<String> = columns
        .iter()
        .map(|column| match column.as_str() {
            "request_key" => "'noise-key-' || ?2".into(),
            "attempt_base_key" => "'noise-base-' || ?2".into(),
            "payload_fingerprint" => "?3".into(),
            "lifecycle" => "'released'".into(),
            "reserved_cost_usd" | "confirmed_cost_usd" | "uncertain_cost_usd" => "0".into(),
            "reported_cost_usd"
            | "dispatch_at"
            | "settled_at"
            | "provider_generation_id"
            | "actual_model"
            | "actual_provider" => "NULL".into(),
            "created_at" | "updated_at" => "'2099-01-01T00:00:00Z'".into(),
            _ => column.clone(),
        })
        .collect();
    let sql = format!(
        "INSERT INTO analysis_request_reservations ({}) SELECT {} FROM analysis_request_reservations WHERE request_key=?1",
        columns.join(","),
        expressions.join(",")
    );
    let transaction = connection.transaction().unwrap();
    for index in 0..1001 {
        let payload = blake3::hash(format!("different local payload {index}").as_bytes())
            .to_hex()
            .to_string();
        transaction
            .execute(&sql, rusqlite::params![result.request_key, index, payload])
            .unwrap();
    }
    transaction.commit().unwrap();
    let reuse = catalog
        .probe_native_transcript_checkpoint_reuse(&preparation, &request)
        .unwrap();
    assert!(
        matches!(&reuse.windows[0], NativeTranscriptWindowReuse::RecoveryRequired { request_key: Some(key), .. } if key == &result.request_key)
    );
    // A legacy/inconsistent released label cannot erase an existing marker.
    connection
        .execute(
            "UPDATE analysis_request_reservations SET lifecycle='released' WHERE request_key=?1",
            [&result.request_key],
        )
        .unwrap();
    let reuse = catalog
        .probe_native_transcript_checkpoint_reuse(&preparation, &request)
        .unwrap();
    assert!(
        matches!(&reuse.windows[0], NativeTranscriptWindowReuse::RecoveryRequired { request_key: Some(key), .. } if key == &result.request_key)
    );
}

#[test]
fn native_checkpoint_rejected_text_keeps_its_provider_segment_evidence() {
    let (_directory, catalog, _project, asset, policy, _request, _audio, _run, context) =
        authorized_native_setup();
    let request = native_policy_request();
    let preparation = two_window_preparation(&catalog, &policy, &asset);
    catalog
        .begin_native_transcript_checkpoint(&preparation)
        .unwrap();
    let first = preparation.materialize_window(0).unwrap();
    let mut call = native_call_for_window(&first, true, "low confidence candidate");
    if let NativeSttOutcome::Usable(transcript) = &mut call.outcome {
        transcript.confidence = Some(0.01);
    }
    let result = transmit_native(
        &catalog,
        &policy,
        &request,
        &first,
        &context.run_id,
        context.clone(),
        call,
    );
    let checkpoint = catalog
        .append_native_transcript_checkpoint(
            &preparation,
            NativeTranscriptCheckpointWindowInput::AccountedResult {
                request: &request,
                audio: &first,
                result: &result,
            },
        )
        .unwrap();
    assert!(checkpoint.windows[0].candidates.is_empty());
    assert_eq!(
        checkpoint.windows[0].observation.quality.decision,
        NativeSttQualityDecision::Rejected
    );
    let header = catalog.asset_transcript(&asset).unwrap().unwrap();
    assert_eq!(header.provider_segment_count, 1);
    assert!(header.segments.is_empty());
    assert_eq!(
        header.outcome_reason,
        TranscriptOutcomeReason::NativeCoverageIncomplete
    );
    assert!(catalog.prompt_transcript(&asset).unwrap().is_none());
}

#[test]
fn native_checkpoint_history_rejects_negative_source_times_instead_of_wrapping() {
    let (_directory, catalog, _project, asset, policy, _request, _audio, _run, context) =
        authorized_native_setup();
    let request = native_policy_request();
    let preparation = two_window_preparation(&catalog, &policy, &asset);
    catalog
        .begin_native_transcript_checkpoint(&preparation)
        .unwrap();
    let first = preparation.materialize_window(0).unwrap();
    let result = transmit_window(
        &catalog,
        &policy,
        &request,
        &first,
        &context.run_id,
        context.clone(),
        "valid original time span",
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
    catalog.connection().unwrap().execute(
        "UPDATE transcript_segments SET start_ms=-4294967296,end_ms=-4294966796 WHERE asset_id=?1", [&asset]).unwrap();
    assert!(catalog.native_transcript_checkpoint(&asset).is_err());
}
