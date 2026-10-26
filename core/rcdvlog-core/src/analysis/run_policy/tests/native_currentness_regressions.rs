//! Supervisor regressions for preserving native work across generic local paths.
use super::*;

#[test]
fn missing_proxy_never_turns_a_paid_native_prefix_into_local_failure() {
    let (_dir, catalog, project, asset, policy, _, _, _, context) = authorized_native_setup();
    let request = native_policy_request();
    let preparation = two_window_preparation(&catalog, &policy, &asset);
    catalog
        .begin_native_transcript_checkpoint(&preparation)
        .unwrap();
    let audio = preparation.materialize_window(0).unwrap();
    let result = transmit_configured_window(
        &catalog,
        &policy,
        &request,
        &audio,
        &context.run_id,
        context.clone(),
        "paid before missing proxy",
        true,
        Some(0.9),
    );
    append_native_window(&catalog, &preparation, &request, &audio, &result);
    let before = catalog.asset_transcript(&asset).unwrap().unwrap();
    let proxy = catalog.get_asset(&asset).unwrap().proxy_path.unwrap();
    std::fs::remove_file(proxy).unwrap();
    let report = catalog.transcribe_missing(&project).unwrap();
    assert_eq!(report.transcribed, 0);
    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.error.contains("review/recovery required"))
    );
    assert_eq!(catalog.asset_transcript(&asset).unwrap().unwrap(), before);
    assert_eq!(link_count(&catalog, &asset), 1);
}

#[test]
fn native_prefix_created_after_planning_is_rechecked_under_the_speech_claim() {
    let (_dir, catalog, project, asset, policy, _, _, _, context) = authorized_native_setup();
    let request = native_policy_request();
    let mut progress = 0;
    let mut retained = None;
    let report = catalog
        .transcribe_missing_with_progress(&project, |_| {
            progress += 1;
            if progress == 2 {
                // Planning finished, but the generic writer has not taken its
                // speech claim. A native owner wins and completes one paid window.
                let preparation = two_window_preparation(&catalog, &policy, &asset);
                catalog
                    .begin_native_transcript_checkpoint(&preparation)
                    .unwrap();
                let audio = preparation.materialize_window(0).unwrap();
                let result = transmit_configured_window(
                    &catalog,
                    &policy,
                    &request,
                    &audio,
                    &context.run_id,
                    context.clone(),
                    "paid during planning gap",
                    true,
                    Some(0.9),
                );
                append_native_window(&catalog, &preparation, &request, &audio, &result);
                retained = catalog.asset_transcript(&asset).unwrap();
            }
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert!(
        retained.is_some(),
        "must exercise the planning-to-claim gap"
    );
    assert_eq!(report.transcribed, 0);
    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.error.contains("review/recovery required"))
    );
    assert_eq!(catalog.asset_transcript(&asset).unwrap(), retained);
    assert_eq!(link_count(&catalog, &asset), 1);
}

#[test]
fn malformed_native_header_is_a_protected_exception_without_canonical_rewrite() {
    let (_dir, catalog, project, asset, policy, _, _, _, _) = authorized_native_setup();
    let preparation = two_window_preparation(&catalog, &policy, &asset);
    catalog
        .begin_native_transcript_checkpoint(&preparation)
        .unwrap();
    catalog.connection().unwrap().execute(
        "UPDATE asset_transcripts SET outcome_reason='malformed-native-reason' WHERE asset_id=?1", [&asset],
    ).unwrap();
    assert!(matches!(
        catalog.current_transcript_state(&asset).unwrap(),
        crate::analysis::store::TranscriptCurrentState::ProtectedNative { .. }
    ));
    let report = catalog.transcribe_missing(&project).unwrap();
    assert_eq!(report.transcribed, 0);
    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.error.contains("review/recovery required"))
    );
    let reason: String = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT outcome_reason FROM asset_transcripts WHERE asset_id=?1",
            [&asset],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reason, "malformed-native-reason");
}

#[test]
fn protocol_unusable_rejected_window_cannot_become_current_terminal_evidence() {
    let (_dir, catalog, _project, asset, policy, _, _, _, context) = authorized_native_setup();
    let request = native_policy_request();
    let preparation = two_window_preparation(&catalog, &policy, &asset);
    let finalized = finalize_accepted_two_window_native(
        &catalog,
        &policy,
        &request,
        &preparation,
        &context,
        "repeat!",
        "repeat",
        true,
    );
    assert_eq!(finalized.status, TranscriptStatus::Rejected);
    assert!(
        catalog
            .current_transcript_state(&asset)
            .unwrap()
            .is_current()
    );
    let mut quality = catalog
        .native_transcript_provenance(&asset)
        .unwrap()
        .unwrap()
        .windows[0]
        .quality
        .clone();
    quality.decision = NativeSttQualityDecision::Rejected;
    quality.reason = crate::analysis::quality::native_stt::NativeSttQualityReason::InvalidTimestamp;
    catalog.connection().unwrap().execute(
        "UPDATE native_transcript_windows SET quality_json=?1 WHERE asset_id=?2 AND window_index=0",
        rusqlite::params![quality.encode_json().unwrap(), asset],
    ).unwrap();
    assert!(
        !catalog
            .current_transcript_state(&asset)
            .unwrap()
            .is_current(),
        "a paid protocol failure requires recovery, not a terminal quality cache hit"
    );
    assert!(catalog.prompt_transcript(&asset).unwrap().is_none());
    assert_eq!(
        catalog.asset_transcript(&asset).unwrap().unwrap(),
        finalized
    );
}
