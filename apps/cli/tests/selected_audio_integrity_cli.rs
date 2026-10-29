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
    let o = invoke(db, args);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["ok"], true);
    v["data"].clone()
}
fn put(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

#[test]
fn audio_audit_cli_measures_sources_and_repairs_stale_evidence_without_decisions() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("catalog.sqlite");
    let media = dir.path().join("media");
    fs::create_dir(&media).unwrap();
    let source = media.join("generated.mp4");
    assert!(
        Command::new("ffmpeg")
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
                "-shortest"
            ])
            .arg(&source)
            .status()
            .unwrap()
            .success()
    );

    let project = success(&db, &["project", "create", "--name", "Audio audit CLI"])["id"]
        .as_str()
        .unwrap()
        .to_owned();
    success(
        &db,
        &[
            "ingest",
            "--project",
            &project,
            "--folder",
            media.to_str().unwrap(),
        ],
    );
    let clips = success(&db, &["clips", "list", "--project", &project]);
    let assets = clips
        .as_array()
        .or_else(|| clips["items"].as_array())
        .unwrap();
    let asset = assets[0]["id"].as_str().unwrap();

    let plan_file = dir.path().join("plan.json");
    put(
        &plan_file,
        &json!({"operations":[{"operationId":"cut-0","position":0,"assetId":asset,
        "sourceInSeconds":0.0,"sourceOutSeconds":4.0,"rationale":"Generated tone fixture"}]}),
    );
    let revision = success(
        &db,
        &[
            "edit-plan",
            "create",
            "--project",
            &project,
            "--input",
            plan_file.to_str().unwrap(),
            "--expected-base",
            "none",
            "--expected-active",
            "none",
        ],
    );
    let revision_id = revision["revisionId"].as_str().unwrap();
    let scope = json!({"firstOperationId":"cut-0","lastOperationId":"cut-0"});
    let excerpt_file = dir.path().join("excerpt.json");
    put(
        &excerpt_file,
        &json!({"revisionId":revision_id,"maxHeight":160,"scope":scope}),
    );
    let excerpt = success(
        &db,
        &[
            "edit-plan",
            "excerpt",
            "--input",
            excerpt_file.to_str().unwrap(),
        ],
    );
    let artifact = excerpt["artifactId"].as_str().unwrap();
    let input = dir.path().join("audit.json");
    put(
        &input,
        &json!({"revisionId":revision_id,"scope":scope,"artifactId":artifact,
        "samples":[{"movieInSeconds":0.5,"movieOutSeconds":1.0}]}),
    );
    let args = [
        "edit-plan",
        "audio-audit",
        "create",
        "--input",
        input.to_str().unwrap(),
    ];
    let first = success(&db, &args);
    let report = &first["report"];
    assert_eq!(first["reused"], false);
    assert_eq!(report["pairs"][0]["original"]["audioPresent"], true);
    assert_eq!(report["samples"][0]["state"], "sampledEnergyPresent");
    assert_eq!(report["samples"][0]["exceptions"], json!([]));
    assert_eq!(report["sampledEnergySeconds"], 0.5);
    assert!(
        !report["uncheckedMovieRanges"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let evidence = report["evidenceId"].as_str().unwrap();
    let reused = success(&db, &args);
    assert_eq!(reused["reused"], true);
    assert_eq!(reused["report"], *report);

    let before_get = fs::read(&db).unwrap();
    let read = success(
        &db,
        &["edit-plan", "audio-audit", "get", "--evidence", evidence],
    );
    assert_eq!(read, *report);
    assert_eq!(
        fs::read(&db).unwrap(),
        before_get,
        "evidence get must be read-only"
    );

    let wav = report["samples"][0]["rendered"]["playable"]["path"]
        .as_str()
        .unwrap();
    fs::write(wav, b"damaged audit WAV").unwrap();
    let repaired = success(&db, &args);
    assert_eq!(
        repaired["reused"], false,
        "damaged evidence WAV must be regenerated"
    );

    fs::write(excerpt["outputPath"].as_str().unwrap(), b"damaged preview").unwrap();
    let stale = success(&db, &args);
    assert_eq!(stale["report"]["state"], "partial");
    assert_eq!(stale["report"]["samples"][0]["state"], "unchecked");
    assert_eq!(
        stale["report"]["pairs"][0]["original"]["audioPresent"],
        true
    );

    let before_invalid = fs::read(&db).unwrap();
    let malformed = dir.path().join("malformed.json");
    fs::write(&malformed, b"{}").unwrap();
    assert!(
        !invoke(
            &db,
            &[
                "edit-plan",
                "audio-audit",
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
                "audio-audit",
                "create",
                "--input",
                oversized.to_str().unwrap()
            ]
        )
        .status
        .success()
    );
    assert_eq!(
        fs::read(&db).unwrap(),
        before_invalid,
        "invalid inputs must not mutate the catalog"
    );
}
