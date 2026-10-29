use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
use tempfile::tempdir;

fn invoke(db: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .arg("--catalog")
        .arg(db)
        .args(args)
        .output()
        .unwrap()
}

fn success(db: &Path, args: &[&str]) -> Value {
    let output = invoke(db, args);
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["ok"], true);
    envelope["data"].clone()
}

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

#[test]
fn short_comparison_cli_verifies_prefers_and_applies_an_inactive_draft() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("catalog.sqlite");
    let media = dir.path().join("media");
    fs::create_dir(&media).unwrap();
    let source = media.join("generated.mp4");
    let encoded = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=24:duration=4",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=4",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(&source)
        .status()
        .unwrap();
    assert!(encoded.success());

    let project_result = success(
        &db,
        &["project", "create", "--name", "Comparison CLI fixture"],
    );
    let project = project_result["id"].as_str().unwrap();
    success(
        &db,
        &[
            "ingest",
            "--project",
            project,
            "--folder",
            media.to_str().unwrap(),
        ],
    );
    let clip_list = success(&db, &["clips", "list", "--project", project]);
    let assets = clip_list
        .as_array()
        .or_else(|| clip_list["items"].as_array())
        .unwrap();
    let asset = assets[0]["id"].as_str().unwrap();

    let cuts = (0..3)
        .map(|i| {
            json!({
                "operationId":format!("cut-{i}"),"position":i,"assetId":asset,
                "sourceInSeconds":i as f64,"sourceOutSeconds":i as f64 + 1.0,
                "rationale":"Generated fixture cut"
            })
        })
        .collect::<Vec<_>>();
    let plan_a_input = dir.path().join("plan-a.json");
    write_json(&plan_a_input, &json!({"operations":cuts}));
    let plan_a = success(
        &db,
        &[
            "edit-plan",
            "create",
            "--project",
            project,
            "--input",
            plan_a_input.to_str().unwrap(),
            "--expected-base",
            "none",
            "--expected-active",
            "none",
        ],
    );
    let rev_a = plan_a["revisionId"].as_str().unwrap();
    let revision_a = success(&db, &["edit-plan", "get", "--revision", rev_a]);
    let mut shifted = revision_a["operations"].as_array().unwrap().clone();
    for cut in &mut shifted {
        cut["sourceInSeconds"] = json!(cut["sourceInSeconds"].as_f64().unwrap() + 1.0 / 30.0);
        cut["sourceOutSeconds"] = json!(cut["sourceOutSeconds"].as_f64().unwrap() + 1.0 / 30.0);
    }
    let plan_b_input = dir.path().join("plan-b.json");
    write_json(&plan_b_input, &json!({"operations":shifted}));
    let plan_b = success(
        &db,
        &[
            "edit-plan",
            "create",
            "--project",
            project,
            "--input",
            plan_b_input.to_str().unwrap(),
            "--expected-base",
            rev_a,
            "--expected-active",
            "none",
        ],
    );
    let rev_b = plan_b["revisionId"].as_str().unwrap();

    let comparison_input = dir.path().join("comparison.json");
    write_json(
        &comparison_input,
        &json!({
            "projectId":project,"intent":"Compare a one-frame source offset in the same three-cut sequence.","maxHeight":144,
            "variants":[
                {"name":"Baseline A","revisionId":rev_a,"scope":{"firstOperationId":"cut-0","lastOperationId":"cut-2"}},
                {"name":"Shifted B","revisionId":rev_b,"scope":{"firstOperationId":"cut-0","lastOperationId":"cut-2"}}
            ]
        }),
    );
    let comparison = success(
        &db,
        &[
            "edit-plan",
            "compare",
            "create",
            "--input",
            comparison_input.to_str().unwrap(),
        ],
    );
    let comparison_id = comparison["id"].as_str().unwrap();

    let before_reads = fs::read(&db).unwrap();
    let status_args = [
        "edit-plan",
        "compare",
        "status",
        "--comparison",
        comparison_id,
    ];
    let status = success(&db, &status_args);
    assert_eq!(status["observedState"], "complete");
    assert_eq!(
        fs::read(&db).unwrap(),
        before_reads,
        "unverified status must be read-only"
    );
    let verified = success(
        &db,
        &[
            "edit-plan",
            "compare",
            "status",
            "--comparison",
            comparison_id,
            "--verify",
        ],
    );
    assert_eq!(verified["currentForRequest"], true);
    let variants = verified["comparison"]["variants"].as_array().unwrap();
    assert_eq!(variants.len(), 2);
    for variant in variants {
        assert!((variant["preview"]["durationSeconds"].as_f64().unwrap() - 3.0).abs() < 0.1);
        assert_eq!(variant["validation"]["cuts"].as_array().unwrap().len(), 3);
    }
    let first_a = variants[0]["validation"]["cuts"][0]["sourceInSeconds"]
        .as_f64()
        .unwrap();
    let first_b = variants[1]["validation"]["cuts"][0]["sourceInSeconds"]
        .as_f64()
        .unwrap();
    assert!((first_b - first_a - 1.0 / 30.0).abs() < 1e-6);
    let player_path = verified["comparison"]["player"]["path"].as_str().unwrap();
    assert!(Path::new(player_path).is_file());
    let player = fs::read_to_string(player_path).unwrap();
    assert!(player.contains("\"Baseline A\""));
    assert!(player.contains("\"Shifted B\""));

    let preference_input = dir.path().join("preference.json");
    write_json(
        &preference_input,
        &json!({
            "comparisonId":comparison_id,"preferredVariant":"Shifted B","expectedPreferredRevisionId":rev_b,
            "expectedActiveRevisionId":"none","decisionOwner":"human",
            "rationale":"Generated test choice only; not a real human quality review.",
            "retainOperationIds":["cut-1"],"createDraft":true
        }),
    );
    let choice = success(
        &db,
        &[
            "edit-plan",
            "compare",
            "prefer",
            "--input",
            preference_input.to_str().unwrap(),
        ],
    );
    assert!(!choice["noteIds"].as_array().unwrap().is_empty());
    let draft_id = choice["draftRevisionId"].as_str().unwrap();
    let draft = success(&db, &["edit-plan", "get", "--revision", draft_id]);
    assert_eq!(draft["active"], false);
    assert_eq!(draft["parentRevisionId"], rev_b);

    let before_choice = fs::read(&db).unwrap();
    let choice_read = success(
        &db,
        &[
            "edit-plan",
            "compare",
            "choice",
            "--choice",
            choice["choiceId"].as_str().unwrap(),
        ],
    );
    assert_eq!(choice_read["draftRevisionId"], draft_id);
    assert_eq!(
        fs::read(&db).unwrap(),
        before_choice,
        "choice read must be read-only"
    );
    let reapplied = success(
        &db,
        &[
            "edit-plan",
            "compare",
            "apply",
            "--choice",
            choice["choiceId"].as_str().unwrap(),
            "--expected-active",
            "none",
        ],
    );
    assert_eq!(
        reapplied["draftRevisionId"], draft_id,
        "apply must be idempotent"
    );

    fs::write(
        &variants[0]["preview"]["outputPath"].as_str().unwrap(),
        b"damaged preview",
    )
    .unwrap();
    let unverified = success(&db, &status_args);
    assert_ne!(
        unverified["currentForRequest"], true,
        "unverified status must not certify damaged bytes"
    );
    let before_rejected_preference = fs::read(&db).unwrap();
    let stale_preference = json!({
        "comparisonId":comparison_id,"preferredVariant":"Baseline A","expectedPreferredRevisionId":rev_a,
        "expectedActiveRevisionId":"none","decisionOwner":"human","rationale":"stale test",
        "retainOperationIds":[],"createDraft":false
    });
    write_json(&preference_input, &stale_preference);
    let rejected = invoke(
        &db,
        &[
            "edit-plan",
            "compare",
            "prefer",
            "--input",
            preference_input.to_str().unwrap(),
        ],
    );
    assert!(
        !rejected.status.success(),
        "stale variant preference unexpectedly succeeded"
    );
    assert_eq!(
        fs::read(&db).unwrap(),
        before_rejected_preference,
        "stale preference must not add feedback"
    );
}
