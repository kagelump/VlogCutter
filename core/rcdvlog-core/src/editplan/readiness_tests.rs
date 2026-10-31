//! Generated-media regression coverage for sparse edits and source accounting.
use super::coverage::*;
use super::patch::*;
use super::*;
use crate::editplan::inspection::tests::fixture;
use std::{fs, io::Write};

fn cut(id: &str, asset: &str, start: f64, end: f64) -> EditPlanOperation {
    EditPlanOperation {
        operation_id: id.into(),
        position: 0,
        asset_id: asset.into(),
        source_in_seconds: start,
        source_out_seconds: end,
        rationale: "Generated fixture".into(),
        audio: None,
    }
}
fn base(catalog: &crate::Catalog, asset: &str) -> EditPlanRevision {
    let mut second = cut("second", asset, 1.5, 2.5);
    second.audio = Some(EditPlanAudio::Source(SourceAudio {
        version: 1,
        asset_id: asset.into(),
        source_in_seconds: 1.5,
        source_out_seconds: 2.5,
        timeline_start_seconds: 1.0,
        sync_offset_seconds: 0.0,
        gain_db: 0.0,
        fade_in_seconds: 0.0,
        fade_out_seconds: 0.0,
    }));
    catalog
        .create_edit_plan_from_base(
            &catalog.get_asset(asset).unwrap().project_id,
            &[cut("first", asset, 0.5, 1.5), second],
            None,
            None,
            None,
        )
        .unwrap()
}
fn patch(edits: Vec<EditPlanPatch>) -> EditPlanPatchInput {
    EditPlanPatchInput {
        version: 1,
        name: "generated alternative".into(),
        edits,
    }
}

#[test]
fn sparse_patch_is_atomic_preserves_identity_and_retimes_retained_audio() {
    let (_dir, catalog, asset) = fixture();
    let base = base(&catalog, &asset);
    let before = catalog.edit_plan_revisions(&base.project_id).unwrap().len();
    let invalid = patch(vec![
        EditPlanPatch::Omit {
            operation_id: "first".into(),
        },
        EditPlanPatch::Trim {
            operation_id: "missing".into(),
            source_in_seconds: 0.0,
            source_out_seconds: 1.0,
            rationale: "bad target".into(),
        },
    ]);
    assert!(
        catalog
            .patch_edit_plan(&base.project_id, &base.revision_id, None, &invalid)
            .is_err()
    );
    assert_eq!(
        catalog.edit_plan_revisions(&base.project_id).unwrap().len(),
        before
    );
    let mut inserted = cut("inserted", &asset, 2.5, 3.2);
    inserted.audio = Some(EditPlanAudio::Silent);
    let result = catalog
        .patch_edit_plan(
            &base.project_id,
            &base.revision_id,
            None,
            &patch(vec![
                EditPlanPatch::Insert {
                    anchor: PatchAnchor::Before {
                        operation_id: "first".into(),
                    },
                    operation: inserted,
                },
                EditPlanPatch::Trim {
                    operation_id: "first".into(),
                    source_in_seconds: 0.5,
                    source_out_seconds: 1.2,
                    rationale: "quicker connection".into(),
                },
            ]),
        )
        .unwrap();
    assert!(!result.revision.active);
    assert!(result.validation.renderable());
    assert_eq!(result.revision.operations[2].operation_id, "second");
    let Some(EditPlanAudio::Source(audio)) = &result.revision.operations[2].audio else {
        panic!("source audio lost")
    };
    assert!((audio.timeline_start_seconds - 1.4).abs() < 1e-9);
    assert_eq!(audio.source_in_seconds, 1.5);
    assert_eq!(catalog.edit_plan_revision(&base.revision_id).unwrap(), base);
    catalog
        .activate_edit_plan_revision(&base.revision_id, None)
        .unwrap();
    assert!(
        catalog
            .patch_edit_plan(
                &base.project_id,
                &base.revision_id,
                None,
                &patch(vec![EditPlanPatch::Omit {
                    operation_id: "first".into()
                }])
            )
            .is_err()
    );
}

#[test]
fn split_keeps_one_independent_audio_event_and_returns_durable_lineage() {
    let (_dir, catalog, asset) = fixture();
    let base = base(&catalog, &asset);
    let result = catalog
        .patch_edit_plan(
            &base.project_id,
            &base.revision_id,
            None,
            &patch(vec![EditPlanPatch::Split {
                operation_id: "second".into(),
                source_split_seconds: 2.0,
                new_operation_id: "second-payoff".into(),
                rationale: "separate action beat".into(),
            }]),
        )
        .unwrap();
    assert_eq!(result.revision.operations.len(), 3);
    assert_eq!(
        result.revision.operations[2].audio,
        Some(EditPlanAudio::Silent)
    );
    assert_eq!(
        result.lineage[0].parent_operation_id.as_deref(),
        Some("second")
    );
    let lineage:String=catalog.connection().unwrap().query_row(
        "SELECT json_extract(payload_json,'$.patchLineage[0].operationId') FROM project_operations WHERE json_extract(payload_json,'$.revisionId')=?1",
        [&result.revision.revision_id],|r|r.get(0)).unwrap();
    assert_eq!(lineage, "second-payoff");
    let metadata = catalog
        .edit_plan_patch_metadata(&result.revision.revision_id)
        .unwrap();
    assert_eq!(
        metadata.as_ref().unwrap()["lineage"][0]["operationId"],
        "second-payoff"
    );
    let baseline = catalog.export_project_baseline(&base.project_id).unwrap();
    assert!(baseline.operations.iter().any(|op| op.payload_version == 4));
    let peer_dir = tempfile::tempdir().unwrap();
    let peer_path = peer_dir.path().join("peer.sqlite3");
    let peer = crate::Catalog::import_project_baseline(
        &peer_path,
        &baseline,
        "node_patch_peer",
        1,
        "patch-baseline",
    )
    .unwrap();
    assert_eq!(
        peer.edit_plan_patch_metadata(&result.revision.revision_id)
            .unwrap(),
        metadata
    );
    for operation in &baseline.operations {
        peer.apply_operation(operation).unwrap();
    }
    assert_eq!(
        peer.edit_plan_patch_metadata(&result.revision.revision_id)
            .unwrap(),
        metadata
    );
    drop(peer);
    let reopened = crate::Catalog::open(peer_path).unwrap();
    assert_eq!(
        reopened
            .edit_plan_patch_metadata(&result.revision.revision_id)
            .unwrap(),
        metadata
    );
}

#[test]
fn usage_counts_unique_picture_sources_with_a_fixed_denominator_and_audio_separately() {
    let (dir, catalog, asset) = fixture();
    let first = catalog.get_asset(&asset).unwrap();
    let extra = dir.path().join("additional");
    fs::create_dir(&extra).unwrap();
    for n in 0..2 {
        let file = extra.join(format!("new-{n}.mp4"));
        fs::copy(&first.path, &file).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(file)
            .unwrap()
            .write_all(&[n, 1, 2, 3])
            .unwrap();
    }
    assert_eq!(
        catalog
            .ingest_folder(&first.project_id, &extra)
            .unwrap()
            .imported,
        2
    );
    let other: Vec<_> = catalog
        .list_assets(&first.project_id)
        .unwrap()
        .into_iter()
        .filter(|a| a.id != asset)
        .collect();
    let mut operations = vec![cut("A", &asset, 0.5, 1.0), cut("repeat", &asset, 1.0, 1.5)];
    operations[0].audio = Some(EditPlanAudio::Source(SourceAudio {
        version: 1,
        asset_id: other[0].id.clone(),
        source_in_seconds: 1.5,
        source_out_seconds: 2.0,
        timeline_start_seconds: 0.0,
        sync_offset_seconds: 0.0,
        gain_db: 0.0,
        fade_in_seconds: 0.0,
        fade_out_seconds: 0.0,
    }));
    let plan = catalog
        .create_edit_plan_from_base(&first.project_id, &operations, None, None, None)
        .unwrap();
    let report = catalog
        .picture_coverage(&PictureCoverageRequest {
            inventory_asset_ids: None,
            revision_id: plan.revision_id.clone(),
            base_revision_id: None,
            target_percent: Some(50.0),
            groups: vec![],
        })
        .unwrap();
    assert_eq!(report["inventorySources"], 3);
    assert_eq!(report["uniquePictureSources"], 1);
    assert_eq!(report["repeatedPictureUses"], 1);
    assert_eq!(report["targetUniqueSources"], 2);
    assert_eq!(report["audioOnlySources"].as_array().unwrap().len(), 1);
    let queue = catalog
        .unused_picture_sources(&plan.revision_id, None, 0, 1, false)
        .unwrap();
    assert_eq!(queue["total"], 2);
    assert_eq!(queue["items"].as_array().unwrap().len(), 1);
    assert_eq!(queue["nextOffset"], 1);
    let mut replacement = operations.clone();
    replacement[0].asset_id = other[1].id.clone();
    let next = catalog
        .create_edit_plan_from_base(
            &first.project_id,
            &replacement,
            None,
            Some(&plan.revision_id),
            None,
        )
        .unwrap();
    let delta = catalog
        .picture_coverage(&PictureCoverageRequest {
            inventory_asset_ids: None,
            revision_id: next.revision_id,
            base_revision_id: Some(plan.revision_id),
            target_percent: Some(50.0),
            groups: vec![],
        })
        .unwrap();
    assert_eq!(delta["uniquePictureSources"], 2);
    assert_eq!(delta["addedSources"].as_array().unwrap().len(), 1);
    assert_eq!(delta["retainedSources"].as_array().unwrap().len(), 1);
}

#[test]
fn phase_cancellation_preserves_a_verified_historical_movie() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (_dir, catalog, asset) = fixture();
    let base = base(&catalog, &asset);
    let request = PreviewRequest {
        revision_id: base.revision_id.clone(),
        max_height: 144,
    };
    let mut events = Vec::new();
    let first = catalog
        .render_preview(&request, &mut |e| events.push(e), &|| false)
        .unwrap();
    let stages: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            PreviewProgress::Phase { stage, .. } => Some(stage.as_str()),
            _ => None,
        })
        .collect();
    for required in [
        "identifyingSources",
        "assembling",
        "mixingAudio",
        "muxingAudio",
        "decodingAndValidatingOutput",
        "verifyingInputs",
        "publishing",
    ] {
        assert!(stages.contains(&required), "{stages:?}");
    }
    assert!(events.iter().all(|e|!matches!(e,PreviewProgress::ClipCompleted {estimate_remaining_seconds:Some(v),..} if *v==0.0)));
    let cancel = AtomicBool::new(false);
    assert!(
        catalog
            .render_preview(
                &request,
                &mut |event| {
                    if matches!(event,PreviewProgress::Phase {stage,..} if stage=="assembling") {
                        cancel.store(true, Ordering::SeqCst);
                    }
                },
                &|| cancel.load(Ordering::SeqCst)
            )
            .is_err()
    );
    let status = catalog
        .preview_artifact_status(first.artifact_id.as_deref().unwrap(), true)
        .unwrap();
    assert!(!status.latest_attempt);
    assert_eq!(status.current_for_request, Some(true));
    assert!(std::path::Path::new(&first.output_path).is_file());
}

#[test]
fn sparse_patch_cannot_shorten_an_explicitly_retained_human_choice() {
    use crate::{
        DecideReviewFeedbackRequest, RecordReviewNoteRequest, ReviewAuthor, ReviewCategory,
        ReviewDecisionAction, ReviewDecisionProvenance, ReviewHardChoice,
    };
    let (_dir, catalog, asset) = fixture();
    let base = base(&catalog, &asset);
    let note = catalog
        .record_review_note(&RecordReviewNoteRequest {
            project_id: base.project_id.clone(),
            revision_id: base.revision_id.clone(),
            operation_id: "first".into(),
            author: ReviewAuthor::Human,
            category: ReviewCategory::FeaturedShot,
            observation: "Synthetic explicit keep fixture".into(),
            viewed_artifact_id: None,
            movie_time_seconds: None,
            evidence_refs: vec![],
        })
        .unwrap();
    catalog
        .decide_review_feedback(&DecideReviewFeedbackRequest {
            note_id: note.note.note_id,
            expected_revision: note.revision,
            proposal_id: None,
            decision_owner: None,
            provenance: Some(ReviewDecisionProvenance::human()),
            action: ReviewDecisionAction::Accepted,
            rationale: "Synthetic explicit human retention".into(),
            hard_choice: Some(ReviewHardChoice::KeepPictureAndAudio),
        })
        .unwrap();
    let count = catalog.edit_plan_revisions(&base.project_id).unwrap().len();
    assert!(
        catalog
            .patch_edit_plan(
                &base.project_id,
                &base.revision_id,
                None,
                &patch(vec![EditPlanPatch::Trim {
                    operation_id: "first".into(),
                    source_in_seconds: 0.6,
                    source_out_seconds: 1.4,
                    rationale: "Would violate protected range".into()
                }])
            )
            .is_err()
    );
    assert_eq!(
        catalog.edit_plan_revisions(&base.project_id).unwrap().len(),
        count
    );
}

#[test]
fn audit_sample_planner_returns_consumer_valid_bounds_at_both_limits() {
    let (_dir, catalog, asset) = fixture();
    let base = base(&catalog, &asset);
    let movie = catalog
        .render_preview_excerpt(
            &PreviewExcerptRequest {
                revision_id: base.revision_id,
                max_height: 144,
                scope: PreviewExcerptScope {
                    first_operation_id: "first".into(),
                    last_operation_id: "second".into(),
                },
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap();
    for width in [0.02, 0.333333, 1.0] {
        let planned = catalog
            .plan_audio_integrity_samples(&crate::AudioSamplePlanRequest {
                artifact_id: movie.artifact_id.clone().unwrap(),
                sample_seconds: width,
                max_samples: 32,
            })
            .unwrap();
        assert!(!planned.request.samples.is_empty());
        for sample in planned.request.samples {
            assert!((0.02..=1.0).contains(&(sample.movie_out_seconds - sample.movie_in_seconds)));
            assert!(
                sample.movie_in_seconds >= 0.0
                    && sample.movie_out_seconds <= movie.duration_seconds
            );
        }
    }
}
