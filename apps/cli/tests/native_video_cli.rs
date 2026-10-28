use rcdvlog_core::Catalog;
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
#[test]
fn native_video_policy_inputs_are_typed_and_bounded_without_mutating_catalog() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("catalog.sqlite");
    let c = Catalog::open(&db).unwrap();
    c.create_project("native video CLI").unwrap();
    drop(c);
    let before = fs::read(&db).unwrap();
    let policy = dir.path().join("wrong-kind.json");
    fs::write(
        &policy,
        r#"{"project_id":"x","parameters":{"kind":"observation"}}"#,
    )
    .unwrap();
    let out = invoke(
        &db,
        &[
            "native-video",
            "preflight",
            "--policy-file",
            policy.to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert_eq!(fs::read(&db).unwrap(), before);
    let large = dir.path().join("large.json");
    fs::write(&large, vec![b' '; 8 * 1024 * 1024 + 1]).unwrap();
    let out = invoke(
        &db,
        &[
            "native-video",
            "preflight",
            "--policy-file",
            large.to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("exceeds the 8388608 byte limit"));
}

#[test]
fn local_native_video_workflow_preflights_plans_and_prepares_without_authorization() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("catalog.sqlite");
    let source = dir.path().join("source");
    fs::create_dir_all(&source).unwrap();
    let clip = source.join("clip.mp4");
    let ffmpeg = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=320x180:r=24:d=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(&clip)
        .status()
        .unwrap();
    assert!(ffmpeg.success());
    let create = invoke(
        &db,
        &["project", "create", "--name", "Native video CLI fixture"],
    );
    assert!(
        create.status.success(),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );
    let project: String =
        serde_json::from_slice::<serde_json::Value>(&create.stdout).unwrap()["data"]["id"]
            .as_str()
            .unwrap()
            .into();
    let ingested = invoke(
        &db,
        &[
            "ingest",
            "--project",
            &project,
            "--folder",
            source.to_str().unwrap(),
        ],
    );
    assert!(
        ingested.status.success(),
        "{}",
        String::from_utf8_lossy(&ingested.stderr)
    );
    let catalog = Catalog::open(&db).unwrap();
    let asset = catalog.list_assets(&project).unwrap().remove(0).id;
    drop(catalog);
    let policy = dir.path().join("policy.json");
    let body = serde_json::json!({"projectId":project,"assetIds":[asset],"provider":"gemini-direct","primaryModel":"gemini-3.5-flash-lite","allowedModels":[],"allowFallback":false,"payload":"video","includeProjectVocabulary":false,"budgetUsd":1.0,"parameters":{"kind":"native_video_review","options":{"version":1,"purpose":"moments","tier":"standard","representation":"original","navigation":"static","thinking":"minimal","sourceScopes":[{"assetId":asset,"sourceInSeconds":0.0,"sourceOutSeconds":2.0}],"fps":1.0,"maxWidth":320,"maxWindowSeconds":2,"maxMediaSeconds":2,"maxRequests":1,"maxOutputTokens":256,"mediaWorkers":1,"apiWorkers":1}}});
    fs::write(&policy, serde_json::to_vec(&body).unwrap()).unwrap();
    let preflight = invoke(
        &db,
        &[
            "native-video",
            "preflight",
            "--policy-file",
            policy.to_str().unwrap(),
        ],
    );
    assert!(
        preflight.status.success(),
        "{}",
        String::from_utf8_lossy(&preflight.stderr)
    );
    let preflight: serde_json::Value = serde_json::from_slice(&preflight.stdout).unwrap();
    let policy_id = preflight["data"]["policyId"].as_str().unwrap();
    let windows = invoke(
        &db,
        &[
            "native-video",
            "windows",
            "--policy-file",
            policy.to_str().unwrap(),
        ],
    );
    assert!(
        windows.status.success(),
        "{}",
        String::from_utf8_lossy(&windows.stderr)
    );
    let windows: serde_json::Value = serde_json::from_slice(&windows.stdout).unwrap();
    let window = &windows["data"][0];
    let window_file = dir.path().join("window.json");
    fs::write(&window_file, serde_json::to_vec(window).unwrap()).unwrap();
    let prepared = invoke(
        &db,
        &[
            "native-video",
            "prepare",
            "--policy-file",
            policy.to_str().unwrap(),
            "--window-file",
            window_file.to_str().unwrap(),
        ],
    );
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let prepared: serde_json::Value = serde_json::from_slice(&prepared.stdout).unwrap();
    let data = &prepared["data"];
    assert_eq!(data["audioIncluded"], false);
    assert_eq!(data["decodedAudioSamples"], 0);
    assert_eq!(data["clock"]["precision"], "sampledSecondsCorrespondence");
    assert_eq!(data["clock"]["sourceOffsetSeconds"], 0.0);
    assert_eq!(data["clock"]["secondsScale"], 1.0);
    assert_eq!(window["policyId"], policy_id);
    let auth = Catalog::open(&db)
        .unwrap()
        .load_run_authorization(policy_id);
    assert!(
        auth.is_err(),
        "preflight and local preparation must not authorize a provider send"
    );
}
