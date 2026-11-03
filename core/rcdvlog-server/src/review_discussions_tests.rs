use super::*;
use crate::review_targets::tests::{PROJECT, actor, now};
use crate::review_transcripts::tests::{FakeRuntime, Fixture, fixture, fixture_for, local, run};
use crate::{AuditActorType, ReviewSessionEventKind, ReviewSessionEventRequest};

fn human() -> ReviewActor {
    ReviewActor {
        actor: actor(),
        may_assert_human: true,
    }
}

fn device() -> ReviewActor {
    ReviewActor {
        actor: ReviewTargetActor::new(AuditActorType::DeviceCredential, "device_agent"),
        may_assert_human: false,
    }
}

/// A `review-capture-calibration.v1` clock sample: performance epoch 0,
/// sample 0 at monotonic 0 ns, ±3 ms.
pub(crate) fn calibration_event(f: &Fixture, event_id: &str, epoch: u64, first_sample: u64) {
    let origin_ns = 0u64;
    f.catalog
        .review_sessions(0)
        .record_event(
            PROJECT,
            &f.session_id,
            &ReviewSessionEventRequest {
                event_id: event_id.into(),
                kind: ReviewSessionEventKind::ClockSample,
                audio_frame: first_sample + 48_000,
                performance_ms: Some(1000.0 + first_sample as f64 / 48.0),
                declared_movie_seconds: None,
                detail: Some(serde_json::json!({
                    "schema": "review-capture-calibration.v1",
                    "method": "workletProbeBracket",
                    "clockId": format!("mic_epoch_{epoch}"),
                    "clockEpoch": epoch,
                    "sampleRate": 48_000,
                    "firstSample": first_sample,
                    "sampleIndex": first_sample + 48_000,
                    "monotonicNs": origin_ns + 1_000_000_000 + first_sample * 1_000_000 / 48,
                    "monotonicOriginNs": origin_ns,
                    "monotonicToleranceNs": 3_000_000,
                    "performanceEpochMs": 0.0,
                    "uncertaintyMs": 3.0,
                })),
            },
            now(),
        )
        .unwrap();
}

/// A manual cue pressed at `performance_ms` declaring `movie` seconds.
pub(crate) fn cue_event(f: &Fixture, event_id: &str, epoch: u64, performance_ms: f64, movie: f64) {
    f.catalog
        .review_sessions(0)
        .record_event(
            PROJECT,
            &f.session_id,
            &ReviewSessionEventRequest {
                event_id: event_id.into(),
                kind: ReviewSessionEventKind::ManualSyncCue,
                audio_frame: (performance_ms * 48.0) as u64,
                performance_ms: Some(performance_ms),
                declared_movie_seconds: Some(movie),
                detail: Some(serde_json::json!({"clockEpoch": epoch})),
            },
            now(),
        )
        .unwrap();
}

/// Transcribed fixture (utterances at ~1–4 s, ~6–9 s, ~12–15 s). With `cue`,
/// the capture clock is calibrated and the reviewer marked movie 0 at
/// recording 0.5 s and movie 5 at recording 5.5 s.
fn workspace_fixture(cue: bool) -> (Fixture, String, ReviewDiscussions) {
    let f = fixture(false, true);
    if cue {
        calibration_event(&f, "calibration_0", 0, 0);
        cue_event(&f, "cue_first_frame", 0, 500.0, 0.0);
        cue_event(&f, "cue_five", 0, 5500.0, 5.0);
    }
    let run_id = run(&f, &local(), &FakeRuntime::new()).run_id;
    let discussions = f.catalog.review_discussions();
    (f, run_id, discussions)
}

fn item(kind: ItemKind, text: &str) -> NewDiscussionItem {
    NewDiscussionItem {
        kind,
        author_kind: AuthorKind::Human,
        text: text.into(),
        speaker: None,
        quote_utterance_ids: vec![],
        category: None,
        change: None,
        decision: None,
        refers_to_item_id: None,
    }
}

#[test]
fn movie_time_comes_from_the_shared_synchronizer_over_native_frames() {
    let (_f, run_id, discussions) = workspace_fixture(true);
    let workspace = discussions.workspace(PROJECT, &run_id).unwrap();
    assert_eq!(workspace.sync.status, "mapped");
    assert_eq!(workspace.sync.anchors, 2);
    assert_eq!(workspace.sync.policy.max_extrapolation_seconds, 0.0);
    assert_eq!(workspace.revision_id, "editrev_synthetic");
    assert!(!workspace.coverage.audiovisual_review_attested);
    // Between the two cues: estimated by the shared mapper, with its evidence.
    let first = &workspace.utterances[0];
    // The utterance (~1–6 s) runs past the last cue at 5.5 s: its start is
    // estimated, its tail unknown. Both slices are kept.
    assert_eq!(first.sync.state, "mixed");
    let slice = &first.sync.slices[0];
    assert_eq!(
        slice.state,
        rcdvlog_core::tv_review_sync::SyncState::Estimated
    );
    assert_eq!(slice.recording.clock_id, "mic_epoch_0");
    assert_eq!(slice.recording.start_sample, first.machine.start_frame);
    let tail = first.sync.slices.last().unwrap();
    assert_eq!(tail.state, rcdvlog_core::tv_review_sync::SyncState::Unknown);
    assert_eq!(tail.recording.end_sample, first.machine.end_frame);
    assert!(tail.candidate_operation_ids.is_empty());
    let estimate = first.movie_time.as_ref().unwrap();
    assert_eq!(estimate.source, "sharedSync:estimated");
    assert!((estimate.movie_seconds - 0.5).abs() < 0.01);
    assert!(estimate.uncertainty_seconds >= 0.25);
    assert!(
        estimate
            .evidence_ids
            .contains(&"cue_first_frame".to_string())
            && estimate.evidence_ids.contains(&"cue_five".to_string())
    );
    assert!(
        first
            .candidate_operation_ids
            .contains(&"editop_split_a".to_string())
    );
    // After the last cue there is no extrapolation: unknown, no candidates.
    for late in &workspace.utterances[1..] {
        assert_eq!(late.sync.state, "unknown");
        assert!(late.movie_time.is_none());
        assert!(late.candidate_operation_ids.is_empty());
    }
}

#[test]
fn without_calibration_synchronization_is_pending_even_with_a_cue() {
    let f = fixture(false, true);
    // A cue alone cannot place the recording clock: nothing is synthesized.
    cue_event(&f, "cue_uncalibrated", 0, 500.0, 0.0);
    let run_id = run(&f, &local(), &FakeRuntime::new()).run_id;
    let workspace = f
        .catalog
        .review_discussions()
        .workspace(PROJECT, &run_id)
        .unwrap();
    assert_eq!(workspace.sync.status, "pending");
    assert!(
        workspace
            .utterances
            .iter()
            .all(|u| u.sync.state == "pending"
                && u.movie_time.is_none()
                && u.candidate_operation_ids.is_empty())
    );
}

#[test]
fn without_any_sync_every_remark_is_pending() {
    let (_f, run_id, discussions) = workspace_fixture(false);
    let workspace = discussions.workspace(PROJECT, &run_id).unwrap();
    assert_eq!(workspace.sync.status, "pending");
    assert!(
        workspace
            .utterances
            .iter()
            .all(|u| u.movie_time.is_none() && u.candidate_operation_ids.is_empty())
    );
}

#[test]
fn corrections_are_append_only_and_drive_the_effective_view() {
    let (_f, run_id, discussions) = workspace_fixture(true);
    let id = discussions.workspace(PROJECT, &run_id).unwrap().utterances[1]
        .machine
        .utterance_id
        .clone();
    discussions
        .correct_utterance(
            PROJECT,
            &run_id,
            &id,
            "text",
            serde_json::json!("上一個鏡頭太長了"),
            Some("ASR misheard".into()),
            &human(),
        )
        .unwrap();
    discussions
        .correct_utterance(
            PROJECT,
            &run_id,
            &id,
            "speaker",
            serde_json::json!("B"),
            None,
            &human(),
        )
        .unwrap();
    let workspace = discussions
        .correct_utterance(
            PROJECT,
            &run_id,
            &id,
            "movieTime",
            serde_json::json!(5.0),
            None,
            &human(),
        )
        .unwrap();
    let u = workspace
        .utterances
        .iter()
        .find(|u| u.machine.utterance_id == id)
        .unwrap();
    assert_eq!(
        u.machine.text, "这个镜头太长了",
        "machine evidence is preserved"
    );
    assert_eq!(u.text, "上一個鏡頭太長了");
    assert_eq!(u.speaker, "B");
    assert_eq!(u.corrections.len(), 3);
    assert_eq!(u.movie_time.as_ref().unwrap().source, "humanCorrection");
    assert_eq!(u.reference_hint.as_deref(), Some("previous"));
    // Movie 5.0 shows editop_return; "previous shot" adds editop_split_b as a candidate only.
    assert_eq!(
        u.candidate_operation_ids,
        vec!["editop_split_b", "editop_return"]
    );
    assert!(
        discussions
            .correct_utterance(
                PROJECT,
                &run_id,
                &id,
                "speaker",
                serde_json::json!("C"),
                None,
                &human()
            )
            .is_err()
    );
    assert!(matches!(
        discussions.correct_utterance(
            PROJECT,
            &run_id,
            &id,
            "speaker",
            serde_json::json!("A"),
            None,
            &device()
        ),
        Err(ReviewDiscussionError::HumanRequired)
    ));
    assert!(
        discussions
            .correct_utterance(
                PROJECT,
                &run_id,
                &id,
                "movieTime",
                serde_json::json!(60.0),
                None,
                &human()
            )
            .is_err()
    );
}

#[test]
fn a_retracted_suggestion_is_kept_but_never_accepted_and_acceptances_stay_consistent() {
    let (_f, run_id, discussions) = workspace_fixture(true);
    let workspace = discussions.propose(PROJECT, &run_id, &human()).unwrap();
    assert_eq!(workspace.discussions.len(), 1);
    assert_eq!(workspace.discussions[0].grouping_source, "heuristic");
    let discussion_id = workspace.discussions[0].discussion_id.clone();
    // Proposing again is idempotent.
    assert_eq!(
        discussions
            .propose(PROJECT, &run_id, &human())
            .unwrap()
            .discussions
            .len(),
        1
    );

    let mut longer = item(ItemKind::CandidateEdit, "Keep this shot longer.");
    longer.speaker = Some("A".into());
    longer.change =
        Some(serde_json::json!({"kind": "trim", "sourceInSeconds": 1.0, "sourceOutSeconds": 3.5}));
    let ws = discussions
        .add_item(PROJECT, &discussion_id, &longer, &human())
        .unwrap();
    let longer_id = ws.discussions[0].items[0].item_id.clone();
    let mut no = item(
        ItemKind::Retraction,
        "No — keep the duration, change the transition instead.",
    );
    no.speaker = Some("B".into());
    no.refers_to_item_id = Some(longer_id.clone());
    discussions
        .add_item(PROJECT, &discussion_id, &no, &human())
        .unwrap();
    let mut transition = item(
        ItemKind::CandidateEdit,
        "Change the transition; keep the duration.",
    );
    transition.speaker = Some("B".into());
    let ws = discussions
        .add_item(PROJECT, &discussion_id, &transition, &human())
        .unwrap();
    let transition_id = ws.discussions[0].items[2].item_id.clone();
    assert_eq!(ws.discussions[0].items[0].status, "retracted");

    let mut accept = item(ItemKind::Decision, "Accept");
    accept.decision = Some(DecisionAction::Accepted);
    accept.refers_to_item_id = Some(longer_id.clone());
    assert_eq!(
        discussions
            .add_item(PROJECT, &discussion_id, &accept, &human())
            .unwrap_err()
            .code(),
        "review_item_retracted"
    );
    accept.refers_to_item_id = Some(transition_id.clone());
    assert_eq!(
        discussions
            .add_item(PROJECT, &discussion_id, &accept, &human())
            .unwrap_err()
            .code(),
        "review_target_unconfirmed"
    );
    assert!(matches!(
        discussions.update(
            PROJECT,
            &discussion_id,
            None,
            None,
            Some(Some("editop_split_a".into())),
            None,
            None,
            &device()
        ),
        Err(ReviewDiscussionError::HumanRequired)
    ));
    assert!(
        discussions
            .update(
                PROJECT,
                &discussion_id,
                None,
                None,
                Some(Some("editop_missing".into())),
                None,
                None,
                &human()
            )
            .is_err()
    );
    discussions
        .update(
            PROJECT,
            &discussion_id,
            None,
            None,
            Some(Some("editop_split_a".into())),
            None,
            None,
            &human(),
        )
        .unwrap();
    let ws = discussions
        .add_item(PROJECT, &discussion_id, &accept, &human())
        .unwrap();
    assert_eq!(ws.discussions[0].items[2].status, "accepted");
    let ws = discussions
        .add_item(
            PROJECT,
            &discussion_id,
            &item(ItemKind::CandidateEdit, "Also trim it."),
            &human(),
        )
        .unwrap();
    accept.refers_to_item_id = Some(ws.discussions[0].items.last().unwrap().item_id.clone());
    assert_eq!(
        discussions
            .add_item(PROJECT, &discussion_id, &accept, &human())
            .unwrap_err()
            .code(),
        "review_conflicting_acceptance"
    );

    let report = discussions.report(PROJECT, &run_id).unwrap();
    assert_eq!(report["schema"], "review-report.v1");
    let feedback = report["feedback"].as_array().unwrap();
    assert!(
        feedback.iter().all(|f| f["itemId"] != longer_id),
        "retracted items are never exported"
    );
    let accepted: Vec<_> = feedback
        .iter()
        .filter(|f| f["humanAccepted"] == true)
        .collect();
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0]["itemId"], transition_id);
    // The exported request is exactly the core type the owning catalog accepts.
    let request: rcdvlog_core::RecordReviewNoteRequest =
        serde_json::from_value(accepted[0]["record"].clone()).unwrap();
    // The record-once input carries a stable id-only import identity.
    let once: rcdvlog_core::RecordReviewNoteOnceRequest =
        serde_json::from_value(accepted[0]["recordOnce"].clone()).unwrap();
    assert!(once.import_id.starts_with("rcdvlog-server/review/"));
    assert_eq!(once.import_id, accepted[0]["importId"]);
    assert!(!once.import_id.chars().any(char::is_whitespace));
    assert_eq!(request.operation_id, "editop_split_a");
    assert_eq!(request.revision_id, "editrev_synthetic");
    assert_eq!(
        request.viewed_artifact_id.as_deref(),
        Some("preview_synthetic")
    );
    assert!(request.evidence_refs.len() <= 16);
    assert!(request.movie_time_seconds.is_some());
    // The retraction quote is still in the discussion history.
    let items = report["discussions"][0]["items"].as_array().unwrap();
    assert!(items.iter().any(|i| i["kind"] == "retraction"));
}

#[test]
fn agents_may_suggest_but_never_decide_or_claim_human_authorship() {
    let (_f, run_id, discussions) = workspace_fixture(true);
    let ws = discussions.propose(PROJECT, &run_id, &device()).unwrap();
    let discussion_id = ws.discussions[0].discussion_id.clone();
    let mut suggestion = item(
        ItemKind::CandidateEdit,
        "Reviewers seem to agree the shot is long.",
    );
    suggestion.author_kind = AuthorKind::Agent;
    let ws = discussions
        .add_item(PROJECT, &discussion_id, &suggestion, &device())
        .unwrap();
    assert!(ws.discussions[0].items[0].provisional);
    let mut decide = item(ItemKind::Decision, "consensus");
    decide.author_kind = AuthorKind::Agent;
    decide.decision = Some(DecisionAction::Accepted);
    decide.refers_to_item_id = Some(ws.discussions[0].items[0].item_id.clone());
    assert!(matches!(
        discussions.add_item(PROJECT, &discussion_id, &decide, &device()),
        Err(ReviewDiscussionError::HumanRequired)
    ));
    let human_claim = item(ItemKind::Observation, "pretend human");
    assert!(matches!(
        discussions.add_item(PROJECT, &discussion_id, &human_claim, &device()),
        Err(ReviewDiscussionError::HumanRequired)
    ));
    let mut bad_change = item(ItemKind::CandidateEdit, "bad");
    bad_change.change = Some(serde_json::json!({"kind": "teleport"}));
    assert!(
        discussions
            .add_item(PROJECT, &discussion_id, &bad_change, &human())
            .is_err()
    );
    // Unconfirmed targets are reported as open questions, not exported.
    let report = discussions.report(PROJECT, &run_id).unwrap();
    assert!(report["feedback"].as_array().unwrap().is_empty());
    assert_eq!(report["unresolved"].as_array().unwrap().len(), 1);
}

#[test]
fn reconciliation_receipts_are_single_and_conflicts_are_refused() {
    let (_f, run_id, discussions) = workspace_fixture(true);
    let ws = discussions.propose(PROJECT, &run_id, &human()).unwrap();
    let discussion_id = ws.discussions[0].discussion_id.clone();
    discussions
        .update(
            PROJECT,
            &discussion_id,
            None,
            None,
            Some(Some("editop_split_a".into())),
            None,
            None,
            &human(),
        )
        .unwrap();
    let ws = discussions
        .add_item(
            PROJECT,
            &discussion_id,
            &item(ItemKind::Observation, "The opening is rushed."),
            &human(),
        )
        .unwrap();
    let item_id = ws.discussions[0].items[0].item_id.clone();
    let first = discussions
        .reconcile(PROJECT, &item_id, "reviewnote_1")
        .unwrap();
    assert_eq!(first.canonical_note_id.as_deref(), Some("reviewnote_1"));
    discussions
        .reconcile(PROJECT, &item_id, "reviewnote_1")
        .unwrap();
    assert_eq!(
        discussions
            .reconcile(PROJECT, &item_id, "reviewnote_2")
            .unwrap_err()
            .code(),
        "review_reconciliation_conflict"
    );
    let report = discussions.report(PROJECT, &run_id).unwrap();
    assert_eq!(report["feedback"][0]["canonicalNoteId"], "reviewnote_1");
    assert_eq!(report["feedback"][0]["next"], "reconciled");
    // Marking a discussion unanchored clears its target and removes it from export.
    discussions
        .update(
            PROJECT,
            &discussion_id,
            None,
            None,
            None,
            Some(true),
            None,
            &human(),
        )
        .unwrap();
    assert!(
        discussions.report(PROJECT, &run_id).unwrap()["feedback"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_historical_target_maps_through_the_historical_boundary_and_says_so() {
    let f = fixture_for(false, true, true);
    calibration_event(&f, "calibration_0", 0, 0);
    cue_event(&f, "cue_first_frame", 0, 500.0, 0.0);
    cue_event(&f, "cue_five", 0, 5500.0, 5.0);
    let run_id = run(&f, &local(), &FakeRuntime::new()).run_id;
    let discussions = f.catalog.review_discussions();
    let workspace = discussions.workspace(PROJECT, &run_id).unwrap();
    assert_eq!(
        workspace.target_contract,
        crate::ReviewTargetContract::HistoricalRenderedArtifactReviewV1
    );
    // Same shared mapper and evidence as a strict target.
    assert_eq!(workspace.sync.status, "mapped");
    let first = &workspace.utterances[0];
    assert_eq!(
        first.movie_time.as_ref().unwrap().source,
        "sharedSync:estimated"
    );
    assert!(
        first
            .candidate_operation_ids
            .contains(&"editop_split_a".to_string())
    );
    let report = discussions.report(PROJECT, &run_id).unwrap();
    assert_eq!(
        report["targetContract"],
        "historicalRenderedArtifactReviewV1"
    );
    assert!(
        report["rules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r.as_str().unwrap().contains("no render, edit, activation"))
    );
}
