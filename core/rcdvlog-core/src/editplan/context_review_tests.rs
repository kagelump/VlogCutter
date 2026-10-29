use super::*;
use crate::editplan::inspection::tests::fixture;
use crate::{
    EditPlanAudio, EditPlanOperation, PreviewExcerptRequest, PreviewExcerptScope, PreviewRequest,
    RecordReviewNoteRequest, ReviewAuthor, ReviewCategory, SourceAudio,
};

fn plan(catalog: &Catalog, asset: &str) -> EditPlanRevision {
    let project = catalog.get_asset(asset).unwrap().project_id;
    let mut cuts = (0..3)
        .map(|i| EditPlanOperation {
            operation_id: format!("cut-{i}"),
            position: i,
            asset_id: asset.into(),
            source_in_seconds: 0.5 + f64::from(i),
            source_out_seconds: 1.5 + f64::from(i),
            rationale: "Synthetic mechanics only".into(),
            audio: Some(EditPlanAudio::Silent),
        })
        .collect::<Vec<_>>();
    // Sound is owned by cut 2, while its placed interval coincides with cut 1.
    cuts[2].audio = Some(EditPlanAudio::Source(SourceAudio {
        version: 1,
        asset_id: asset.into(),
        source_in_seconds: 1.5,
        source_out_seconds: 2.5,
        timeline_start_seconds: 1.0,
        sync_offset_seconds: 0.0,
        gain_db: 0.0,
        fade_in_seconds: 0.2,
        fade_out_seconds: 0.2,
    }));
    catalog
        .create_edit_plan_from_base(&project, &cuts, None, None, None)
        .unwrap()
}
fn request(plan: &EditPlanRevision) -> PreviewExcerptRequest {
    PreviewExcerptRequest {
        revision_id: plan.revision_id.clone(),
        max_height: 144,
        scope: PreviewExcerptScope {
            first_operation_id: "cut-1".into(),
            last_operation_id: "cut-1".into(),
        },
    }
}
fn rms(path: &str) -> f64 {
    let out = std::process::Command::new("ffmpeg")
        .args([
            "-v", "error", "-ss", "0.3", "-i", path, "-t", "0.2", "-map", "0:a:0", "-ar", "48000",
            "-ac", "1", "-f", "f32le", "-",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let samples = out
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()) as f64)
        .collect::<Vec<_>>();
    (samples.iter().map(|s| s * s).sum::<f64>() / samples.len() as f64).sqrt()
}

#[test]
fn excerpt_renders_only_whole_neighbor_cuts_reuses_segments_and_retains_distant_owned_sound() {
    let (_dir, catalog, asset) = fixture();
    let plan = plan(&catalog, &asset);
    let full = catalog
        .render_preview(
            &PreviewRequest {
                revision_id: plan.revision_id.clone(),
                max_height: 144,
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap();
    let excerpt = catalog
        .render_preview_excerpt(&request(&plan), &mut |_| {}, &|| false)
        .unwrap();
    assert_eq!(excerpt.clip_count, 1);
    assert!((excerpt.duration_seconds - 1.001).abs() < 0.06);
    assert_eq!(excerpt.segment_counts.as_ref().unwrap().reused, 1);
    assert!(
        rms(&excerpt.output_path) > 0.02,
        "sound from outside the pictured cut must survive"
    );
    let status = catalog
        .preview_artifact_status(excerpt.artifact_id.as_deref().unwrap(), true)
        .unwrap();
    assert_eq!(status.current_for_request, Some(true));
    assert_eq!(status.artifact.kind, "cutNeighborhood");
    let qa = status.artifact.validation.unwrap();
    assert_eq!(qa.cuts.len(), 1);
    assert_eq!(qa.cuts[0].operation_id, "cut-1");
    assert_eq!(qa.cuts[0].movie_in_pts, 0);
    assert_eq!(qa.cuts[0].source_in_seconds, 1.5);
    let full_status = catalog
        .preview_artifact_status(full.artifact_id.as_deref().unwrap(), true)
        .unwrap();
    assert_eq!(
        full_status.current_for_request,
        Some(true),
        "excerpt and full requests have independent latest-generation identities"
    );
    assert!(
        catalog
            .active_edit_plan(&plan.project_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn assembled_context_has_separate_source_handles_audio_mapping_and_revision_bound_advisory_feedback()
 {
    let (_dir, catalog, asset) = fixture();
    let plan = plan(&catalog, &asset);
    let excerpt = catalog
        .render_preview_excerpt(&request(&plan), &mut |_| {}, &|| false)
        .unwrap();
    let handle = SourceInspectionRequest {
        asset_id: asset.clone(),
        representation: InspectionRepresentation::Original,
        purpose: InspectionPurpose::ProposedBoundary,
        source_in_seconds: 1.5,
        source_out_seconds: 2.5,
        before_seconds: 0.25,
        after_seconds: 0.25,
        sample_count: 4,
        max_width: 160,
        source_times_seconds: vec![],
        exact_representation_pts: vec![],
        expected_representation_fingerprint: None,
        include_audio: true,
        include_transcript: false,
    };
    let context = catalog
        .assembled_review_context(
            &AssembledContextRequest {
                artifact_id: excerpt.artifact_id.clone().unwrap(),
                intent: "Complete the action and retain the thought under neighboring imagery"
                    .into(),
                source_handles: vec![handle],
                max_handle_media_seconds: 2.0,
            },
            &|| false,
        )
        .unwrap();
    assert_eq!(context.validation.cuts.len(), 1);
    assert!((context.full_timeline_origin_seconds - 1.001).abs() < 1e-9);
    assert_eq!(context.audio_mappings.len(), 1);
    assert_eq!(context.audio_mappings[0]["operationId"], "cut-2");
    assert_eq!(context.audio_mappings[0]["selectedAudioPresent"], true);
    assert_eq!(context.source_handles[0]["state"], "complete");
    assert!(context.source_handles[0]["inspection"]["audio"]["playable"].is_object());
    assert!(context.unresolved_questions.len() >= 3);
    let note = RecordReviewNoteRequest {
        project_id: plan.project_id.clone(),
        revision_id: plan.revision_id.clone(),
        operation_id: "cut-1".into(),
        author: ReviewAuthor::Reviewer,
        category: ReviewCategory::SoundContinuity,
        observation: "Synthetic proposed continuity check; listen before accepting".into(),
        viewed_artifact_id: excerpt.artifact_id.clone(),
        movie_time_seconds: Some(0.3),
        evidence_refs: vec![],
    };
    let view = catalog
        .record_assembled_review_note(&context.evidence_id, &note)
        .unwrap();
    assert!(view.note.evidence_refs.contains(&context.evidence_id));
    assert_eq!(view.note.target.revision_id, plan.revision_id);
    assert!(
        catalog
            .active_edit_plan(&plan.project_id)
            .unwrap()
            .is_none()
    );
    let read_only = Catalog::open_read_only_existing(catalog.database_path()).unwrap();
    assert_eq!(
        read_only
            .assembled_review_evidence(&context.evidence_id)
            .unwrap()
            .validation
            .cuts
            .len(),
        1
    );
    let mut changed = plan.operations.clone();
    if let Some(EditPlanAudio::Source(a)) = &mut changed[2].audio {
        a.gain_db = -3.0;
    }
    let draft = catalog
        .create_edit_plan_from_base(
            &plan.project_id,
            &changed,
            None,
            Some(&plan.revision_id),
            None,
        )
        .unwrap();
    let view = catalog
        .review_feedback(&view.note.note_id, &draft.revision_id)
        .unwrap();
    assert!(view.target_status.requires_mapping);
    std::fs::write(&excerpt.output_path, b"damaged fixture output").unwrap();
    assert!(
        catalog
            .record_assembled_review_note(&context.evidence_id, &note)
            .is_err()
    );
}

#[test]
fn invalid_excerpt_scope_cancel_and_unrelated_handles_fail_without_activation() {
    let (_dir, catalog, asset) = fixture();
    let plan = plan(&catalog, &asset);
    let mut req = request(&plan);
    req.scope.first_operation_id = "cut-2".into();
    req.scope.last_operation_id = "cut-0".into();
    assert!(
        catalog
            .render_preview_excerpt(&req, &mut |_| {}, &|| false)
            .is_err()
    );
    assert!(
        catalog
            .render_preview_excerpt(&request(&plan), &mut |_| {}, &|| true)
            .is_err()
    );
    assert!(
        catalog
            .active_edit_plan(&plan.project_id)
            .unwrap()
            .is_none()
    );
}
