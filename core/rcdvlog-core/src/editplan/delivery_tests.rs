use super::*;
use crate::{
    AssembledContextRequest, EditPlanOperation, InspectionPurpose, InspectionRepresentation,
    PreviewExcerptRequest, PreviewExcerptScope, PreviewRequest, SourceInspectionRequest,
    editplan::inspection::tests::fixture,
};
use std::fs;

#[test]
fn verified_render_exports_stable_create_only_bundle_and_reuses_verified_files() {
    let (temp, catalog, asset_id) = fixture();
    let long_operation_id = "a".repeat(255);
    let project_id = catalog.get_asset(&asset_id).unwrap().project_id;
    let plan = catalog
        .create_edit_plan_from_base(
            &project_id,
            &[EditPlanOperation {
                operation_id: long_operation_id.clone(),
                position: 0,
                asset_id: asset_id.clone(),
                source_in_seconds: 0.5,
                source_out_seconds: 2.0,
                rationale: "generated test media".into(),
                audio: None,
            }],
            None,
            None,
            None,
        )
        .unwrap();
    let preview = catalog
        .render_preview_excerpt(
            &PreviewExcerptRequest {
                revision_id: plan.revision_id.clone(),
                max_height: 144,
                scope: PreviewExcerptScope {
                    first_operation_id: long_operation_id.clone(),
                    last_operation_id: long_operation_id.clone(),
                },
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap();
    let artifact_id = preview.artifact_id.unwrap();
    let context = catalog
        .assembled_review_context(
            &AssembledContextRequest {
                artifact_id: artifact_id.clone(),
                intent: "Review the complete generated action and sound.".into(),
                source_handles: vec![SourceInspectionRequest {
                    asset_id: asset_id.clone(),
                    representation: InspectionRepresentation::Original,
                    purpose: InspectionPurpose::ProposedBoundary,
                    source_in_seconds: 0.5,
                    source_out_seconds: 2.0,
                    before_seconds: 0.0,
                    after_seconds: 0.0,
                    sample_count: 2,
                    max_width: 160,
                    source_times_seconds: vec![],
                    exact_representation_pts: vec![],
                    expected_representation_fingerprint: None,
                    include_audio: true,
                    include_transcript: false,
                    io_timeout_seconds: 180,
                    probe_timeout_seconds: 180,
                    decode_timeout_seconds: 180,
                }],
                max_handle_media_seconds: 2.0,
            },
            &|| false,
        )
        .unwrap();
    let output_parent = temp.path().join("review output 日本語");
    let request = EditPlanBundleRequest {
        artifact_id: artifact_id.clone(),
        output_directory: output_parent,
        name: "review_r1".into(),
        contact_sheet: true,
        context_evidence_id: Some(context.evidence_id),
        picture_coverage: None,
    };
    let first = catalog
        .export_edit_plan_bundle(&request, &|| false)
        .unwrap();
    assert!(!first.reused);
    assert_eq!(
        fs::read(&first.movie_path).unwrap(),
        fs::read(&preview.output_path).unwrap()
    );
    assert!(first.manifest_path.is_file());
    let mapping: Value =
        serde_json::from_slice(&fs::read(first.bundle_directory.join("mapping.json")).unwrap())
            .unwrap();
    assert_eq!(mapping["cuts"][0]["operationId"], long_operation_id);
    assert_eq!(
        mapping["cuts"][0]["movieMapping"]["mapping"]["outputFrameCount"],
        validation_frame_count(&catalog, &artifact_id)
    );
    let html = fs::read_to_string(first.bundle_directory.join("index.html")).unwrap();
    assert!(html.contains("<video controls"));
    assert!(html.contains(&long_operation_id));
    assert!(html.contains("frame-0000-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-start.jpg"));
    assert!(html.contains("<audio controls"));
    let contact_frames = first
        .files
        .iter()
        .filter(|file| file.path.starts_with("frame-0000-"))
        .collect::<Vec<_>>();
    assert_eq!(contact_frames.len(), 3);
    assert!(
        contact_frames
            .iter()
            .all(|file| file.path.len() < 255 && first.bundle_directory.join(&file.path).is_file())
    );
    assert!(
        first
            .files
            .iter()
            .any(|file| file.path == "handle-00-audio.wav")
    );
    assert!(first.bundle_directory.join("contact-sheet.json").is_file());
    let second = catalog
        .export_edit_plan_bundle(&request, &|| false)
        .unwrap();
    assert!(second.reused);
    fs::remove_file(&first.manifest_path).unwrap();
    let resumed = catalog
        .export_edit_plan_bundle(&request, &|| false)
        .unwrap();
    assert!(
        resumed.reused,
        "owned pre-manifest outputs should resume after interruption"
    );
    fs::write(first.bundle_directory.join("movie.mp4"), b"damaged").unwrap();
    assert!(
        catalog
            .export_edit_plan_bundle(&request, &|| false)
            .is_err(),
        "damaged output must not be overwritten or reused"
    );
}

fn validation_frame_count(catalog: &Catalog, artifact_id: &str) -> Value {
    serde_json::json!(
        catalog
            .preview_artifact_status(artifact_id, false)
            .unwrap()
            .artifact
            .validation
            .unwrap()
            .cuts[0]
            .output_frame_count
    )
}

#[test]
fn refuses_existing_unowned_output_directory() {
    let (temp, catalog, asset_id) = fixture();
    let project_id = catalog.get_asset(&asset_id).unwrap().project_id;
    let plan = catalog
        .create_edit_plan_from_base(
            &project_id,
            &[EditPlanOperation {
                operation_id: "delivery-cut-1".into(),
                position: 0,
                asset_id,
                source_in_seconds: 0.5,
                source_out_seconds: 2.0,
                rationale: "generated test media".into(),
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
                revision_id: plan.revision_id,
                max_height: 144,
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap();
    let output_parent = temp.path().join("existing");
    fs::create_dir_all(output_parent.join("claimed")).unwrap();
    fs::write(output_parent.join("claimed/notes.txt"), b"keep").unwrap();
    let request = EditPlanBundleRequest {
        artifact_id: preview.artifact_id.unwrap(),
        output_directory: output_parent.clone(),
        name: "claimed".into(),
        contact_sheet: false,
        context_evidence_id: None,
        picture_coverage: None,
    };
    assert!(
        catalog
            .export_edit_plan_bundle(&request, &|| false)
            .is_err()
    );
    assert_eq!(
        fs::read(output_parent.join("claimed/notes.txt")).unwrap(),
        b"keep"
    );
}
