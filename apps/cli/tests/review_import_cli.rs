//! Generated owning-catalog import; retry simulates a committed lost response.
use rcdvlog_core::{
    Catalog, EditPlanOperation, PreviewRequest, RecordReviewNoteOnceRequest,
    RecordReviewNoteRequest, ReviewAuthor, ReviewCategory,
};
use serde_json::Value;
use std::{
    path::Path,
    process::{Command, Output},
};
fn invoke(catalog: &Path, input: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .arg("--catalog")
        .arg(catalog)
        .args(["edit-plan", "feedback", "record-once", "--input"])
        .arg(input)
        .output()
        .unwrap()
}
#[test]
fn retry_returns_the_committed_note_and_changed_import_payload_is_a_stable_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let sources = dir.path().join("generated sources");
    std::fs::create_dir(&sources).unwrap();
    let out = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=160x90:r=30:d=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(sources.join("generated.mp4"))
        .output()
        .unwrap();
    assert!(out.status.success());
    let catalog_path = dir.path().join("catalog.sqlite3");
    let catalog = Catalog::open(&catalog_path).unwrap();
    let project = catalog
        .create_project("Synthetic external feedback")
        .unwrap();
    catalog.ingest_folder(&project.id, &sources).unwrap();
    let asset = catalog.list_assets(&project.id).unwrap().remove(0);
    let plan = catalog
        .create_edit_plan_from_base(
            &project.id,
            &[EditPlanOperation {
                operation_id: "synthetic_cut".into(),
                position: 0,
                asset_id: asset.id,
                source_in_seconds: 0.0,
                source_out_seconds: 1.0,
                rationale: "Generated import fixture".into(),
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
    let mut request = RecordReviewNoteOnceRequest {
        import_id: "synthetic-server/session/discussion/item".into(),
        record: RecordReviewNoteRequest {
            project_id: project.id.clone(),
            revision_id: plan.revision_id.clone(),
            operation_id: "synthetic_cut".into(),
            author: ReviewAuthor::Human,
            category: ReviewCategory::Duration,
            observation: "Synthetic reviewer keeps the duration and requests a transition change."
                .into(),
            viewed_artifact_id: preview.artifact_id,
            movie_time_seconds: Some(0.5),
            evidence_refs: vec!["synthetic_utterance".into()],
        },
    };
    let input = dir.path().join("import.json");
    std::fs::write(&input, serde_json::to_vec(&request).unwrap()).unwrap();
    let first = invoke(&catalog_path, &input);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(first.stderr.is_empty());
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["command"], "edit-plan.feedback.record-once");
    assert_eq!(first["data"]["duplicate"], false);
    // The caller loses that response. Evidence can go offline before retry.
    std::fs::remove_file(preview.output_path).unwrap();
    let retry = invoke(&catalog_path, &input);
    assert!(retry.status.success());
    assert!(retry.stderr.is_empty());
    let retry: Value = serde_json::from_slice(&retry.stdout).unwrap();
    assert_eq!(retry["data"]["duplicate"], true);
    assert_eq!(retry["data"]["noteId"], first["data"]["noteId"]);
    assert_eq!(retry["data"]["feedback"]["historyTotal"], 1);
    assert!(retry["data"]["feedback"]["latestDecision"].is_null());
    request.record.observation = "Changed after original import".into();
    std::fs::write(&input, serde_json::to_vec(&request).unwrap()).unwrap();
    let conflict = invoke(&catalog_path, &input);
    assert!(!conflict.status.success());
    assert!(conflict.stdout.is_empty());
    let error: Value = serde_json::from_slice(&conflict.stderr).unwrap();
    assert_eq!(error["error"]["code"], "review_feedback_import_conflict");
    assert_eq!(catalog.active_edit_plan(&project.id).unwrap(), None);
    assert_eq!(catalog.edit_plan_revision(&plan.revision_id).unwrap(), plan);
}
