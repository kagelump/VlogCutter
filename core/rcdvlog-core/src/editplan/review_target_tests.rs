use super::*;
use crate::{EditPlanOperation, PreviewRequest, editplan::inspection::tests::fixture};
use serde_json::{Value, json};

fn synthetic() -> ReviewTargetHandoff {
    let mut h: ReviewTargetHandoff =
        serde_json::from_str(include_str!("../../tests/fixtures/review-target/v1.json")).unwrap();
    // Pin fixture digest to the same canonical Rust serialization consumers validate.
    assert_eq!(
        h.target_digest,
        ReviewTargetHandoff::snapshot_digest(&h.target).unwrap()
    );
    h.validate().unwrap();
    h.verification.verified_at = "2026-01-01T00:00:00Z".into();
    h
}
fn reseal(h: &mut ReviewTargetHandoff) {
    h.target_digest = ReviewTargetHandoff::snapshot_digest(&h.target).unwrap();
}

#[test]
fn v1_fixture_keeps_split_repeated_sources_and_independent_audio_distinct() {
    let h = synthetic();
    assert_eq!(h.target.cuts[0].asset_id, h.target.cuts[1].asset_id);
    assert_ne!(h.target.cuts[0].operation_id, h.target.cuts[1].operation_id);
    assert_eq!(h.picture_candidates(0.0, 0.0).unwrap(), ["editop_split_a"]);
    assert_eq!(h.picture_candidates(2.0, 2.0).unwrap(), ["editop_split_b"]);
    assert_eq!(
        h.picture_candidates(1.9, 2.1).unwrap(),
        ["editop_split_a", "editop_split_b"]
    );
    assert_eq!(h.picture_candidates(4.0, 4.0).unwrap(), ["editop_return"]);
    assert!(
        h.picture_candidates(6.0, 6.01).unwrap().is_empty(),
        "audio/container tail is not a picture cut"
    );
    assert!(h.picture_candidates(-1.0, 1.0).is_err());
    assert!(h.picture_candidates(f64::NAN, 1.0).is_err());
    let Some(EditPlanAudio::Source(a)) = &h.target.cuts[2].declared_audio else {
        panic!("independent audio missing")
    };
    assert_eq!(a.asset_id, "asset_sound_only");
    assert_eq!(a.timeline_start_seconds, 1.0);
    let json = serde_json::to_vec(&h).unwrap();
    assert_eq!(ReviewTargetHandoff::from_json(&json).unwrap(), h);
}

#[test]
fn v1_rejects_mismatch_incomplete_mapping_unsafe_clocks_and_private_fields() {
    let base = synthetic();
    for mutate in [
        |h: &mut ReviewTargetHandoff| h.schema_version = 2,
        |h: &mut ReviewTargetHandoff| h.target.cuts[1].operation_id = "editop_split_a".into(),
        |h: &mut ReviewTargetHandoff| h.target.cuts[1].movie_in_pts += 1,
        |h: &mut ReviewTargetHandoff| h.target.cuts[1].movie_in_pts -= 1,
        |h: &mut ReviewTargetHandoff| h.target.cuts[1].movie_out_pts = 0,
        |h: &mut ReviewTargetHandoff| h.target.cuts[0].position = 2,
        |h: &mut ReviewTargetHandoff| h.target.movie.video_frame_count += 1,
        |h: &mut ReviewTargetHandoff| h.target.movie.time_base_denominator = 0,
        |h: &mut ReviewTargetHandoff| h.target.movie.origin_pts = i64::MAX,
        |h: &mut ReviewTargetHandoff| h.target.movie.duration_seconds = 5.0,
        |h: &mut ReviewTargetHandoff| h.target.project_id = "/private/path".into(),
        |h: &mut ReviewTargetHandoff| {
            h.target.youtube_video_id = Some("https://youtu.be/example".into())
        },
        |h: &mut ReviewTargetHandoff| h.verification.output_matches = false,
        |h: &mut ReviewTargetHandoff| h.verification.inputs_match = false,
        |h: &mut ReviewTargetHandoff| h.verification.constraints_match = false,
        |h: &mut ReviewTargetHandoff| h.verification.fully_decoded = false,
        |h: &mut ReviewTargetHandoff| h.target.cuts.clear(),
    ] {
        let mut h = base.clone();
        mutate(&mut h);
        reseal(&mut h);
        assert!(h.validate().is_err());
    }
    let mut h = base.clone();
    h.target.revision_id = "revision_other".into();
    assert!(
        h.validate().is_err(),
        "changed payload cannot reuse identity"
    );
    let mut value = serde_json::to_value(&base).unwrap();
    value["target"]["sourcePath"] = json!("/private/source.mov");
    assert!(ReviewTargetHandoff::from_json(&serde_json::to_vec(&value).unwrap()).is_err());
    let mut value = serde_json::to_value(&base).unwrap();
    value["target"]["cuts"][0]["filename"] = json!("private.mov");
    assert!(ReviewTargetHandoff::from_json(&serde_json::to_vec(&value).unwrap()).is_err());
    assert!(ReviewTargetHandoff::from_json(&vec![b' '; REVIEW_TARGET_MAX_BYTES + 1]).is_err());
}

#[test]
fn refreshed_verification_preserves_identity_but_youtube_rebinding_does_not() {
    let h = synthetic();
    let now = "2026-01-01T00:01:00Z".parse().unwrap();
    assert!(h.require_fresh(now, 60).is_ok());
    assert!(h.require_fresh(now, 59).is_err());
    assert!(h.require_fresh(now, 0).is_err());
    assert!(
        h.require_fresh("2025-12-31T23:59:59Z".parse().unwrap(), 60)
            .is_err()
    );
    let mut refreshed = h.clone();
    refreshed.verification.verified_at = "2026-01-01T01:00:00Z".into();
    assert_eq!(
        h.target_digest,
        ReviewTargetHandoff::snapshot_digest(&refreshed.target).unwrap()
    );
    refreshed.target.youtube_video_id = Some("synthetic02".into());
    assert_ne!(
        h.target_digest,
        ReviewTargetHandoff::snapshot_digest(&refreshed.target).unwrap()
    );
}

#[test]
fn real_generated_movie_export_is_verified_repeatable_path_free_and_refuses_changed_bytes() {
    let (_temp, catalog, asset_id) = fixture();
    let asset = catalog.get_asset(&asset_id).unwrap();
    let plan = catalog
        .create_edit_plan_from_base(
            &asset.project_id,
            &[EditPlanOperation {
                operation_id: "cut_generated".into(),
                position: 0,
                asset_id,
                source_in_seconds: 0.5,
                source_out_seconds: 2.0,
                rationale: "must never appear in export".into(),
                audio: None,
            }],
            None,
            None,
            None,
        )
        .unwrap();
    let preview = catalog
        .render_preview(
            &PreviewRequest {
                revision_id: plan.revision_id.clone(),
                max_height: 144,
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap();
    let id = preview.artifact_id.unwrap();
    let first = catalog
        .export_review_target(&id, Some("synthetic01".into()), &|| false)
        .unwrap();
    let again = catalog
        .export_review_target(&id, Some("synthetic01".into()), &|| false)
        .unwrap();
    assert_eq!(first.target_digest, again.target_digest);
    assert_eq!(first.target.revision_id, plan.revision_id);
    assert_eq!(
        first.picture_candidates(0.0, 1.0).unwrap(),
        ["cut_generated"]
    );
    let bytes = serde_json::to_vec(&first).unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        !String::from_utf8(bytes)
            .unwrap()
            .contains("must never appear")
    );
    assert!(value["target"].get("path").is_none());
    assert!(catalog.export_review_target(&id, None, &|| true).is_err());
    // A newer revision must never retarget an older review snapshot.
    let next = catalog
        .create_edit_plan_from_base(
            &asset.project_id,
            &plan.operations,
            None,
            Some(&plan.revision_id),
            None,
        )
        .unwrap();
    assert_ne!(next.revision_id, plan.revision_id);
    let historic = catalog
        .export_review_target(&id, Some("synthetic01".into()), &|| false)
        .unwrap();
    assert_eq!(historic.target_digest, first.target_digest);
    let excerpt = catalog
        .render_preview_excerpt(
            &crate::PreviewExcerptRequest {
                revision_id: plan.revision_id.clone(),
                max_height: 144,
                scope: crate::PreviewExcerptScope {
                    first_operation_id: "cut_generated".into(),
                    last_operation_id: "cut_generated".into(),
                },
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap();
    assert!(
        catalog
            .export_review_target(&excerpt.artifact_id.unwrap(), None, &|| false)
            .is_err()
    );
    std::fs::write(&preview.output_path, b"replaced derived test movie").unwrap();
    assert!(catalog.export_review_target(&id, None, &|| false).is_err());
    std::fs::remove_file(&preview.output_path).unwrap();
    assert!(catalog.export_review_target(&id, None, &|| false).is_err());
}

#[test]
fn display_names_round_trip_without_changing_snapshot_identity() {
    let legacy = synthetic();
    assert!(legacy.display_name.is_none());
    assert!(
        serde_json::to_value(&legacy)
            .unwrap()
            .get("displayName")
            .is_none()
    );
    let named = legacy
        .clone()
        .with_display_name(Some(" RC7 ".into()))
        .unwrap();
    named.validate().unwrap();
    assert_eq!(named.display_name.as_deref(), Some("RC7"));
    assert_eq!(named.target_digest, legacy.target_digest);
    assert_eq!(named.target, legacy.target);
    assert_eq!(named.verification, legacy.verification);
    assert_eq!(
        ReviewTargetHandoff::from_json(&serde_json::to_vec(&named).unwrap()).unwrap(),
        named
    );
    for invalid in ["".to_string(), " ".into(), "RC7\n".into(), "x".repeat(121)] {
        assert!(
            legacy
                .clone()
                .with_display_name(Some(invalid.clone()))
                .is_err()
        );
        let mut untrusted = serde_json::to_value(&legacy).unwrap();
        untrusted["displayName"] = json!(invalid);
        assert!(ReviewTargetHandoff::from_json(&serde_json::to_vec(&untrusted).unwrap()).is_err());
    }
}
