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
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["ok"], true);
    envelope["data"].clone()
}

#[test]
fn assembled_context_cli_binds_evidence_notes_and_refuses_stale_media() {
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

    let created = success(
        &db,
        &["project", "create", "--name", "Assembled context CLI"],
    );
    let project = created["id"].as_str().unwrap();
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
    let clips = success(&db, &["clips", "list", "--project", project]);
    let assets = clips
        .as_array()
        .or_else(|| clips["items"].as_array())
        .unwrap();
    let asset = assets[0]["id"].as_str().unwrap();

    let plan_input = dir.path().join("plan.json");
    let operations = (0..3)
        .map(|i| {
            json!({
                "operationId":format!("cut-{i}"), "position":i, "assetId":asset,
                "sourceInSeconds":i as f64, "sourceOutSeconds":i as f64 + 1.0,
                "rationale":"Generated movement fixture"
            })
        })
        .collect::<Vec<_>>();
    fs::write(
        &plan_input,
        serde_json::to_vec(&json!({"operations":operations})).unwrap(),
    )
    .unwrap();
    let revision = success(
        &db,
        &[
            "edit-plan",
            "create",
            "--project",
            project,
            "--input",
            plan_input.to_str().unwrap(),
            "--expected-base",
            "none",
            "--expected-active",
            "none",
        ],
    );
    let revision_id = revision["revisionId"].as_str().unwrap();

    let bad_excerpt = dir.path().join("bad-excerpt.json");
    fs::write(
        &bad_excerpt,
        serde_json::to_vec(&json!({
            "revisionId":revision_id,"maxHeight":160,
            "scope":{"firstOperationId":"unknown","lastOperationId":"cut-1"}
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(
        !invoke(
            &db,
            &[
                "edit-plan",
                "excerpt",
                "--input",
                bad_excerpt.to_str().unwrap()
            ]
        )
        .status
        .success()
    );

    let excerpt_input = dir.path().join("excerpt.json");
    fs::write(
        &excerpt_input,
        serde_json::to_vec(&json!({
            "revisionId":revision_id,"maxHeight":160,
            "scope":{"firstOperationId":"cut-1","lastOperationId":"cut-2"}
        }))
        .unwrap(),
    )
    .unwrap();
    let excerpt = success(
        &db,
        &[
            "edit-plan",
            "excerpt",
            "--input",
            excerpt_input.to_str().unwrap(),
        ],
    );
    let artifact = excerpt["artifactId"].as_str().unwrap();

    let context_input = dir.path().join("context.json");
    fs::write(&context_input, serde_json::to_vec(&json!({
        "artifactId":artifact,"intent":"Review complete movement and sound continuity.",
        "maxHandleMediaSeconds":2.0,
        "sourceHandles":[{"assetId":asset,"representation":"original","purpose":"proposedBoundary",
            "sourceInSeconds":1.0,"sourceOutSeconds":2.0,"beforeSeconds":0.25,"afterSeconds":0.25,
            "sampleCount":4,"maxWidth":160,"includeAudio":true}]
    })).unwrap()).unwrap();
    let context = success(
        &db,
        &[
            "edit-plan",
            "context",
            "create",
            "--input",
            context_input.to_str().unwrap(),
        ],
    );
    let evidence = context["evidenceId"].as_str().unwrap();
    assert_eq!(context["sourceHandles"][0]["state"], "complete");
    assert_eq!(context["validation"]["cuts"][0]["operationId"], "cut-1");

    let before_get = fs::read(&db).unwrap();
    let retrieved = success(
        &db,
        &["edit-plan", "context", "get", "--evidence", evidence],
    );
    assert_eq!(retrieved, context);
    assert_eq!(
        fs::read(&db).unwrap(),
        before_get,
        "context get must be read-only"
    );

    let note_input = dir.path().join("note.json");
    fs::write(&note_input, serde_json::to_vec(&json!({
        "projectId":project,"revisionId":revision_id,"operationId":"cut-1","author":"reviewer",
        "category":"endpoint","observation":"Check the first movement endpoint against the held source handle.",
        "viewedArtifactId":artifact,"movieTimeSeconds":0.2,"evidenceRefs":[]
    })).unwrap()).unwrap();
    let note = success(
        &db,
        &[
            "edit-plan",
            "context",
            "note",
            "--evidence",
            evidence,
            "--input",
            note_input.to_str().unwrap(),
        ],
    );
    assert_eq!(note["note"]["target"]["revisionId"], revision_id);
    assert_eq!(
        note["note"]["target"]["viewedMovie"]["artifactId"],
        artifact
    );

    let output_path = context["movie"]["outputPath"].as_str().unwrap();
    fs::write(output_path, b"corrupted generated preview").unwrap();
    let before_stale_note = fs::read(&db).unwrap();
    let rejected = invoke(
        &db,
        &[
            "edit-plan",
            "context",
            "note",
            "--evidence",
            evidence,
            "--input",
            note_input.to_str().unwrap(),
        ],
    );
    assert!(
        !rejected.status.success(),
        "stale context note unexpectedly succeeded"
    );
    assert_eq!(
        fs::read(&db).unwrap(),
        before_stale_note,
        "stale note must not add feedback"
    );
}
