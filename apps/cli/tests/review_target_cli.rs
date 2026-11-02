//! Real local render/CLI contract; generated media only.
use rcdvlog_core::{Catalog, EditPlanOperation, PreviewRequest, ReviewTargetHandoff};
use serde_json::Value;
use std::{fs, process::Command};
use tempfile::tempdir;

#[test]
fn export_envelope_is_path_free_preserves_revisions_and_refuses_missing_or_changed_media() {
    let dir = tempdir().unwrap();
    let sources = dir.path().join("generated 日本語");
    fs::create_dir(&sources).unwrap();
    let generated = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=160x90:r=30:d=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=500:sample_rate=48000:duration=2",
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
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let path = dir.path().join("catalog.sqlite3");
    let catalog = Catalog::open(&path).unwrap();
    let project = catalog.create_project("Synthetic TV target").unwrap();
    catalog.ingest_folder(&project.id, &sources).unwrap();
    let asset = catalog.list_assets(&project.id).unwrap().remove(0);
    let plan = catalog
        .create_edit_plan_from_base(
            &project.id,
            &[EditPlanOperation {
                operation_id: "cut_generated".into(),
                position: 0,
                asset_id: asset.id,
                source_in_seconds: 0.0,
                source_out_seconds: 1.0,
                rationale: "private author text".into(),
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
    let artifact = preview.artifact_id.unwrap();
    drop(catalog);
    let before = plan.clone();
    let invoke = || {
        Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
            .arg("--catalog")
            .arg(&path)
            .args([
                "edit-plan",
                "review-target-export",
                "--artifact",
                &artifact,
                "--youtube-video-id",
                "synthetic01",
            ])
            .output()
            .unwrap()
    };
    let output = invoke();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["command"], "edit-plan.review-target-export");
    let handoff =
        ReviewTargetHandoff::from_json(&serde_json::to_vec(&envelope["data"]).unwrap()).unwrap();
    assert_eq!(handoff.target.revision_id, plan.revision_id);
    let observed = Catalog::open_read_only_existing(&path).unwrap();
    assert_eq!(
        observed.edit_plan_revision(&plan.revision_id).unwrap(),
        before
    );
    assert_eq!(observed.active_edit_plan(&project.id).unwrap(), None);
    drop(observed);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("private author text"));
    assert!(!text.contains(dir.path().to_str().unwrap()));
    assert!(!text.contains("generated.mp4"));
    let repeated = invoke();
    assert!(repeated.status.success());
    let repeated: Value = serde_json::from_slice(&repeated.stdout).unwrap();
    assert_eq!(
        repeated["data"]["targetDigest"],
        envelope["data"]["targetDigest"]
    );
    let historical = Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .arg("--catalog")
        .arg(&path)
        .args([
            "edit-plan",
            "historical-review-target-export",
            "--artifact",
            &artifact,
        ])
        .output()
        .unwrap();
    assert!(
        historical.status.success(),
        "{}",
        String::from_utf8_lossy(&historical.stderr)
    );
    assert!(historical.stderr.is_empty());
    let historical: Value = serde_json::from_slice(&historical.stdout).unwrap();
    assert_eq!(
        historical["data"]["purpose"],
        "historicalRenderedArtifactReview"
    );
    assert_eq!(
        historical["data"]["currentPlanRenderability"],
        "notEstablishedByThisReview"
    );
    assert!(
        ReviewTargetHandoff::from_json(&serde_json::to_vec(&historical["data"]).unwrap()).is_err()
    );
    for missing in [false, true] {
        if missing {
            fs::remove_file(&preview.output_path).unwrap();
        } else {
            fs::write(&preview.output_path, b"changed derived fixture").unwrap();
        }
        let output = invoke();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["ok"], false);
        assert_eq!(
            error["error"]["code"],
            "review_target_artifact_not_verified"
        );
    }
    let absent_catalog = dir.path().join("absent.sqlite3");
    let output = Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .arg("--catalog")
        .arg(&absent_catalog)
        .args(["edit-plan", "review-target-export", "--artifact", &artifact])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!absent_catalog.exists());
}
