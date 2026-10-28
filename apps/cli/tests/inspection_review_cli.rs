use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
use tempfile::tempdir;

fn invoke(catalog: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .arg("--catalog")
        .arg(catalog)
        .args(args)
        .output()
        .unwrap()
}
fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn review_session_cli_workflow_is_bounded_durable_and_read_only_on_status() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("catalog.sqlite");
    let source = dir.path().join("source");
    fs::create_dir_all(&source).unwrap();
    let encoded = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=24:duration=1",
            "-an",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-t",
            "1",
        ])
        .arg(source.join("generated.mp4"))
        .status()
        .unwrap();
    assert!(encoded.success());

    let created = success(invoke(
        &db,
        &["project", "create", "--name", "Inspection review CLI"],
    ));
    let project = created["data"]["id"].as_str().unwrap();
    let projects = success(invoke(&db, &["project", "list"]));
    assert_eq!(projects["data"][0]["id"], project);
    success(invoke(
        &db,
        &[
            "ingest",
            "--project",
            project,
            "--folder",
            source.to_str().unwrap(),
        ],
    ));
    let clips = success(invoke(&db, &["clips", "list", "--project", project]));
    let asset = clips["data"]["items"][0]["id"].as_str().unwrap();

    let inspect_input = dir.path().join("inspect.json");
    fs::write(
        &inspect_input,
        serde_json::to_vec(&json!({
            "assetId":asset,"representation":"original","purpose":"locator",
            "sourceInSeconds":0.0,"sourceOutSeconds":1.0,"sampleCount":2,"maxWidth":160
        }))
        .unwrap(),
    )
    .unwrap();
    let inspected = success(invoke(
        &db,
        &[
            "edit-plan",
            "inspect",
            "--input",
            inspect_input.to_str().unwrap(),
        ],
    ));
    let seed = inspected["data"]["inspection"]["evidenceId"]
        .as_str()
        .unwrap();
    let create_input = dir.path().join("session.json");
    fs::write(
        &create_input,
        serde_json::to_vec(&json!({
            "seedEvidenceId":seed,"assetId":asset,"maxSteps":2,"maxTotalFrames":8,
            "maxMediaSeconds":2.0,"maxWidth":160,"allowAudio":false,"allowTranscript":false
        }))
        .unwrap(),
    )
    .unwrap();
    let created_session = success(invoke(
        &db,
        &[
            "edit-plan",
            "review-session",
            "create",
            "--input",
            create_input.to_str().unwrap(),
        ],
    ));
    let session = created_session["data"]["id"].as_str().unwrap();

    let inspect_step = dir.path().join("step0.json");
    fs::write(&inspect_step, serde_json::to_vec(&json!({
        "sessionId":session,"stepIndex":0,"caller":"host","request":{"tool":"inspect","input":{
            "assetId":asset,"representation":"original","purpose":"locator","sourceInSeconds":0.0,
            "sourceOutSeconds":1.0,"sampleCount":2,"maxWidth":160
        }}
    })).unwrap()).unwrap();
    let step0 = success(invoke(
        &db,
        &[
            "edit-plan",
            "review-session",
            "step",
            "--input",
            inspect_step.to_str().unwrap(),
        ],
    ));
    let evidence = step0["data"]["result"]["inspection"]["evidenceId"]
        .as_str()
        .unwrap();
    let observe_step = dir.path().join("step1.json");
    fs::write(&observe_step, serde_json::to_vec(&json!({
        "sessionId":session,"stepIndex":1,"caller":"reviewer","request":{"tool":"observe","input":{
            "inspectedEvidenceId":evidence,"kind":"uncertain","sourceInSeconds":0.0,"sourceOutSeconds":1.0,
            "observation":"possible issue","uncertainty":"needs human playback review"
        }}
    })).unwrap()).unwrap();
    assert_eq!(
        success(invoke(
            &db,
            &[
                "edit-plan",
                "review-session",
                "step",
                "--input",
                observe_step.to_str().unwrap()
            ]
        ))["data"]["result"]["advisory"],
        true
    );

    let before_reads = fs::read(&db).unwrap();
    assert_eq!(
        success(invoke(
            &db,
            &[
                "edit-plan",
                "review-session",
                "status",
                "--session",
                session
            ]
        ))["data"]["nextStep"],
        2
    );
    assert_eq!(
        success(invoke(
            &db,
            &[
                "edit-plan",
                "review-session",
                "steps",
                "--session",
                session,
                "--limit",
                "32"
            ]
        ))["data"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(fs::read(&db).unwrap(), before_reads);
    assert_eq!(
        success(invoke(
            &db,
            &[
                "edit-plan",
                "review-session",
                "finish",
                "--session",
                session
            ]
        ))["data"]["state"],
        "complete"
    );

    let before_bad_inputs = fs::read(&db).unwrap();
    let malformed = dir.path().join("malformed.json");
    fs::write(&malformed, b"{").unwrap();
    assert!(
        !invoke(
            &db,
            &[
                "edit-plan",
                "review-session",
                "create",
                "--input",
                malformed.to_str().unwrap()
            ]
        )
        .status
        .success()
    );
    let oversized = dir.path().join("oversized.json");
    fs::write(&oversized, vec![b' '; 64 * 1024 + 1]).unwrap();
    assert!(
        !invoke(
            &db,
            &[
                "edit-plan",
                "review-session",
                "step",
                "--input",
                oversized.to_str().unwrap()
            ]
        )
        .status
        .success()
    );
    assert_eq!(fs::read(&db).unwrap(), before_bad_inputs);

    let missing = dir.path().join("missing.sqlite");
    assert!(
        !invoke(
            &missing,
            &[
                "edit-plan",
                "review-session",
                "status",
                "--session",
                "missing"
            ]
        )
        .status
        .success()
    );
    assert!(!missing.exists());
}
