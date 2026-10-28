use super::*;
use crate::{
    ConfirmSourceRangeRequest, InspectionPurpose, InspectionRepresentation, RangeDecision,
    RangeDecisionOwner, RangeMedia,
};

fn inspect(asset: &str) -> SourceInspectionRequest {
    SourceInspectionRequest {
        asset_id: asset.into(),
        representation: InspectionRepresentation::Original,
        purpose: InspectionPurpose::Locator,
        source_in_seconds: 0.0,
        source_out_seconds: 4.0,
        before_seconds: 0.0,
        after_seconds: 0.0,
        sample_count: 4,
        max_width: 160,
        source_times_seconds: vec![],
        exact_representation_pts: vec![],
        expected_representation_fingerprint: None,
        include_audio: false,
        include_transcript: false,
    }
}
fn setup(catalog: &Catalog, asset: &str, max_steps: u32) -> InspectionReviewSession {
    let seed = catalog
        .inspect_source_window(&inspect(asset), &|| false)
        .unwrap()
        .inspection;
    catalog
        .create_inspection_review(&InspectionReviewRequest {
            seed_evidence_id: seed.evidence_id,
            asset_id: asset.into(),
            max_steps,
            max_total_frames: 64,
            max_media_seconds: 12.0,
            max_width: 320,
            allow_audio: true,
            allow_transcript: false,
        })
        .unwrap()
}
fn step(
    session: &InspectionReviewSession,
    index: u32,
    request: InspectionReviewTool,
) -> InspectionReviewStepRequest {
    InspectionReviewStepRequest {
        session_id: session.id.clone(),
        step_index: index,
        caller: InspectionReviewCaller::Reviewer,
        request,
    }
}

#[test]
fn host_and_reviewer_share_progressive_actual_pts_audio_tools_and_exact_reuse() {
    let (_dir, catalog, asset) = super::super::inspection::tests::fixture();
    let session = setup(&catalog, &asset, 8);
    let mut input = inspect(&asset);
    input.source_in_seconds = 1.5;
    input.source_out_seconds = 2.0;
    input.before_seconds = 0.25;
    input.after_seconds = 0.25;
    input.sample_count = 8;
    input.include_audio = true;
    let request = step(&session, 0, InspectionReviewTool::Inspect(input.clone()));
    let result = catalog.inspection_review_step(&request, &|| false).unwrap();
    assert_eq!(result.state, "complete");
    let inspected: crate::SourceInspectionResult =
        serde_json::from_value(result.result.clone().unwrap()).unwrap();
    assert!(
        inspected
            .inspection
            .audio
            .as_ref()
            .unwrap()
            .playable
            .is_some()
    );
    assert!(inspected.inspection.utterances.is_empty()); // no invented words/ASR
    assert!(
        inspected
            .inspection
            .frames
            .iter()
            .all(|f| f.source_seconds >= 1.25 && f.source_seconds < 2.25)
    );
    assert!(
        catalog
            .inspection_review_step(&request, &|| false)
            .unwrap()
            .result
            == result.result
    );
    assert_eq!(
        catalog
            .inspection_review_status(&session.id)
            .unwrap()
            .next_step,
        1
    );
    input.sample_count = 0;
    input.exact_representation_pts = vec![inspected.inspection.frames[2].representation_pts];
    input.expected_representation_fingerprint = Some(session.representation_fingerprint.clone());
    let mut exact = step(&session, 1, InspectionReviewTool::Inspect(input));
    exact.caller = InspectionReviewCaller::Host;
    let exact = catalog.inspection_review_step(&exact, &|| false).unwrap();
    assert_eq!(
        exact.result.unwrap()["inspection"]["frames"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        catalog
            .inspection_review_status(&session.id)
            .unwrap()
            .frames_reserved,
        9
    );
    assert_eq!(
        catalog.finish_inspection_review(&session.id).unwrap().state,
        "complete"
    );
}

#[test]
fn scope_payload_budget_and_stopping_limits_cannot_be_reset_by_step_replay() {
    let (_dir, catalog, asset) = super::super::inspection::tests::fixture();
    let session = setup(&catalog, &asset, 1);
    let mut input = inspect(&asset);
    input.after_seconds = 1.0;
    assert!(
        catalog
            .inspection_review_step(
                &step(&session, 0, InspectionReviewTool::Inspect(input.clone())),
                &|| false
            )
            .is_err()
    );
    input.after_seconds = 0.0;
    input.include_transcript = true;
    assert!(
        catalog
            .inspection_review_step(
                &step(&session, 0, InspectionReviewTool::Inspect(input)),
                &|| false
            )
            .is_err()
    );
    assert_eq!(
        catalog
            .inspection_review_status(&session.id)
            .unwrap()
            .next_step,
        0
    );
    let request = step(&session, 0, InspectionReviewTool::Inspect(inspect(&asset)));
    assert_eq!(
        catalog
            .inspection_review_step(&request, &|| false)
            .unwrap()
            .state,
        "complete"
    );
    assert!(
        catalog
            .inspection_review_step(&step(&session, 1, request.request.clone()), &|| false)
            .is_err()
    );
    assert_eq!(
        catalog.inspection_review_status(&session.id).unwrap().state,
        "exhausted"
    );
    assert_eq!(
        catalog
            .inspection_review_status(&session.id)
            .unwrap()
            .media_seconds_reserved,
        4.0
    );
    assert_eq!(
        catalog
            .inspection_review_step(&request, &|| false)
            .unwrap()
            .state,
        "complete"
    );
}

#[test]
fn admitted_failed_work_keeps_limits_and_cannot_be_silently_retried() {
    let (_dir, catalog, asset) = super::super::inspection::tests::fixture();
    let session = setup(&catalog, &asset, 4);
    let request = step(&session, 0, InspectionReviewTool::Inspect(inspect(&asset)));
    let result = catalog
        .inspection_review_step(&request, &|| {
            catalog
                .inspection_review_status(&session.id)
                .unwrap()
                .next_step
                > 0
        })
        .unwrap();
    assert_eq!(result.state, "failed");
    assert_eq!(result.result.unwrap()["limitsConsumed"], true);
    let status = catalog.inspection_review_status(&session.id).unwrap();
    assert_eq!(status.next_step, 1);
    assert_eq!(status.media_seconds_reserved, 4.0);
    assert!(catalog.inspection_review_step(&request, &|| false).is_err());
    assert_eq!(
        catalog
            .inspection_review_steps(&session.id, None, 1)
            .unwrap()[0]
            .state,
        "failed"
    );
}

#[test]
fn confirmed_exclusions_make_session_stale_and_cancellation_never_changes_human_facts() {
    let (_dir, catalog, asset) = super::super::inspection::tests::fixture();
    let session = setup(&catalog, &asset, 8);
    let constraints = catalog.source_range_constraints(&asset, 0, 1).unwrap();
    catalog
        .confirm_source_range(&ConfirmSourceRangeRequest {
            asset_id: asset.clone(),
            expected_revision: constraints.revision,
            expected_source_fingerprint: constraints.source_fingerprint,
            constraint_id: None,
            decision: RangeDecision::Exclude,
            media: RangeMedia::Both,
            decision_owner: RangeDecisionOwner::Human,
            source_in_seconds: 0.0,
            source_out_seconds: 0.25,
            rationale: "Human reviewed covered opening".into(),
            inspected_evidence_id: Some(session.request.seed_evidence_id.clone()),
            overrides_evidence_ids: vec![],
        })
        .unwrap();
    assert!(
        catalog
            .inspection_review_step(
                &step(&session, 0, InspectionReviewTool::Inspect(inspect(&asset))),
                &|| false
            )
            .is_err()
    );
    assert!(catalog.finish_inspection_review(&session.id).is_err());
    let next = setup(&catalog, &asset, 4);
    catalog.cancel_inspection_review(&next.id).unwrap();
    assert!(
        catalog
            .inspection_review_step(
                &step(&next, 0, InspectionReviewTool::Inspect(inspect(&asset))),
                &|| false
            )
            .is_err()
    );
    assert_eq!(
        catalog
            .source_range_constraints(&asset, 0, 100)
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        catalog
            .inspection_review_steps(&session.id, None, 32)
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn covered_and_negative_control_observations_stay_advisory_and_need_actual_citations() {
    let (_dir, catalog, asset) = super::super::inspection::tests::fixture();
    let session = setup(&catalog, &asset, 16);
    let before = catalog.get_asset(&asset).unwrap();
    // External fake reviewer outputs exercise false-positive containment. These
    // do not claim a classifier can distinguish a covered lens from a dark room.
    let controls = [
        (
            InspectionObservationKind::CoveredEdge,
            "suspected covered edge",
        ),
        (
            InspectionObservationKind::CoveredMiddle,
            "suspected covered middle",
        ),
        (
            InspectionObservationKind::NoDefectObserved,
            "upright map control",
        ),
        (
            InspectionObservationKind::MeaningfulDark,
            "meaningful dark scene control",
        ),
        (
            InspectionObservationKind::Uncertain,
            "hand/close-up control",
        ),
    ];
    for (index, (kind, text)) in controls.into_iter().enumerate() {
        let observation = InspectionObservation {
            inspected_evidence_id: session.request.seed_evidence_id.clone(),
            kind,
            source_in_seconds: 0.0,
            source_out_seconds: 4.0,
            observation: text.into(),
            uncertainty: "Only sampled pixels; requires context and human review".into(),
        };
        let result = catalog
            .inspection_review_step(
                &step(
                    &session,
                    index as u32,
                    InspectionReviewTool::Observe(observation),
                ),
                &|| false,
            )
            .unwrap();
        assert_eq!(result.result.unwrap()["advisory"], true);
    }
    let speech = InspectionObservation {
        inspected_evidence_id: session.request.seed_evidence_id.clone(),
        kind: InspectionObservationKind::IncompleteSpeech,
        source_in_seconds: 0.0,
        source_out_seconds: 1.0,
        observation: "clipped thought".into(),
        uncertainty: "uncertain".into(),
    };
    assert!(
        catalog
            .inspection_review_step(
                &step(&session, 5, InspectionReviewTool::Observe(speech)),
                &|| false
            )
            .is_err()
    );
    let after = catalog.get_asset(&asset).unwrap();
    assert_eq!(before.rotation_degrees, after.rotation_degrees);
    assert_eq!(before.removed, after.removed);
    assert_eq!(
        catalog
            .source_range_constraints(&asset, 0, 100)
            .unwrap()
            .items
            .len(),
        0
    );
    let read_only = Catalog::open_read_only_existing(catalog.database_path()).unwrap();
    assert_eq!(
        read_only
            .inspection_review_status(&session.id)
            .unwrap()
            .next_step,
        5
    );
    assert_eq!(
        read_only
            .inspection_review_steps(&session.id, None, 3)
            .unwrap()
            .len(),
        3
    );
}
