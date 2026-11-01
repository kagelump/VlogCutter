//! Process-level JSON contract for chronology-safe sparse edit-plan insertion.

use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
use tempfile::tempdir;

fn invoke(path: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .arg("--catalog")
        .arg(path)
        .args(args)
        .output()
        .unwrap()
}
fn success(path: &Path, args: &[&str]) -> Value {
    let output = invoke(path, args);
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
fn failure(path: &Path, args: &[&str], code: &str) -> Value {
    let output = invoke(path, args);
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "failed commands must keep stdout empty"
    );
    let envelope: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], code, "{envelope}");
    envelope
}

fn find_asset(connection: &rusqlite::Connection, project: &str, needle: &str) -> String {
    let mut statement = connection
        .prepare("SELECT id,path,filename FROM assets WHERE project_id=?1 ORDER BY id")
        .unwrap();
    let rows: Vec<(String, String, String)> = statement
        .query_map([project], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    rows.clone()
        .into_iter()
        .find(|(_, path, filename)| path.contains(needle) || filename.contains(needle))
        .map(|(id, _, _)| id)
        .unwrap_or_else(|| panic!("generated asset {needle} was not ingested; rows={rows:?}"))
}

fn generate_video(path: &Path, frequency: u32) {
    let output = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=30",
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency={frequency}:sample_rate=48000"),
            "-t",
            "2",
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
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "could not generate public fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_auto_insertion_is_persisted_wrong_anchor_is_atomic_and_scoped_override_is_visible() {
    let dir = tempdir().unwrap();
    let catalog = dir.path().join("catalog.sqlite3");
    let media = dir.path().join("generated public media");
    fs::create_dir(&media).unwrap();
    generate_video(&media.join("PUBLIC_boarding.mp4"), 440);
    generate_video(&media.join("PUBLIC_terminal.mp4"), 550);
    generate_video(&media.join("PUBLIC_hotel.mp4"), 660);

    let project = success(
        &catalog,
        &["project", "create", "--name", "Generated chronology CLI"],
    );
    let project = project["id"].as_str().unwrap();
    success(
        &catalog,
        &[
            "ingest",
            "--project",
            project,
            "--folder",
            media.to_str().unwrap(),
        ],
    );
    let connection = rusqlite::Connection::open(&catalog).unwrap();
    let boarding = find_asset(&connection, project, "PUBLIC_boarding");
    let terminal = find_asset(&connection, project, "PUBLIC_terminal");
    let hotel = find_asset(&connection, project, "PUBLIC_hotel");
    for (asset_id, captured_at) in [
        (&boarding, "2026-11-01T01:00:00Z"),
        (&terminal, "2026-11-01T00:30:00Z"),
        (&hotel, "2026-11-01T02:20:00Z"),
    ] {
        connection
            .execute(
                "UPDATE assets SET captured_at_raw=?2 WHERE id=?1",
                rusqlite::params![asset_id, captured_at],
            )
            .unwrap();
    }

    let plan_input = dir.path().join("plan.json");
    fs::write(
        &plan_input,
        serde_json::to_vec(&json!({
            "operations": [
                {"operationId":"boarding","position":0,"assetId":boarding,
                 "sourceInSeconds":0.0,"sourceOutSeconds":1.0,"rationale":"boarding"},
                {"operationId":"hotel","position":1,"assetId":hotel,
                 "sourceInSeconds":0.0,"sourceOutSeconds":1.0,"rationale":"hotel"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    let create_args = [
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
    ];
    let base = success(&catalog, &create_args);
    let base_id = base["revisionId"].as_str().unwrap().to_string();
    assert!(base["storyPolicy"].is_null());

    let auto_patch = dir.path().join("auto-patch.json");
    fs::write(
        &auto_patch,
        serde_json::to_vec(&json!({
            "version":1,
            "name":"automatic insertion",
            "edits":[{
                "kind":"insert",
                "operation":{"operationId":"terminal","position":0,"assetId":terminal,
                    "sourceInSeconds":0.0,"sourceOutSeconds":1.0,"rationale":"terminal"}
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let patch_args = [
        "edit-plan",
        "patch",
        "--project",
        project,
        "--expected-base",
        base_id.as_str(),
        "--expected-active",
        "none",
        "--input",
        auto_patch.to_str().unwrap(),
    ];
    let patched = success(&catalog, &patch_args);
    assert_eq!(patched["revision"]["operations"][0]["assetId"], terminal);
    assert_eq!(patched["revision"]["operations"][1]["assetId"], boarding);
    assert_eq!(patched["revision"]["operations"][2]["assetId"], hotel);
    let revision_id = patched["revision"]["revisionId"].as_str().unwrap();
    let read = success(&catalog, &["edit-plan", "get", "--revision", revision_id]);
    assert_eq!(read["revisionId"], patched["revision"]["revisionId"]);
    assert_eq!(read["operations"], patched["revision"]["operations"]);
    assert_eq!(read["storyPolicy"], patched["revision"]["storyPolicy"]);

    let before_failure =
        success(&catalog, &["edit-plan", "list", "--project", project])["revisions"]
            .as_array()
            .unwrap()
            .len();
    let wrong_patch = dir.path().join("wrong-anchor.json");
    fs::write(
        &wrong_patch,
        serde_json::to_vec(&json!({
            "version":1,
            "name":"wrong explicit anchor",
            "edits":[{
                "kind":"insert",
                "anchor":{"kind":"after","operationId":"boarding"},
                "operation":{"operationId":"terminal","position":0,"assetId":terminal,
                    "sourceInSeconds":0.0,"sourceOutSeconds":1.0,"rationale":"terminal"}
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let wrong_args = [
        "edit-plan",
        "patch",
        "--project",
        project,
        "--expected-base",
        base_id.as_str(),
        "--expected-active",
        "none",
        "--input",
        wrong_patch.to_str().unwrap(),
    ];
    failure(&catalog, &wrong_args, "edit_plan_chronology_conflict");
    assert_eq!(
        success(&catalog, &["edit-plan", "list", "--project", project])["revisions"]
            .as_array()
            .unwrap()
            .len(),
        before_failure,
        "wrong anchor must not persist a partial revision"
    );

    let override_patch = dir.path().join("override-patch.json");
    fs::write(
        &override_patch,
        serde_json::to_vec(&json!({
            "version":1,
            "name":"narrow scoped override",
            "storyPolicy":{
                "version":1,
                "openingTeasers":[],
                "protectedGroups":[],
                "exceptions":[{
                    "exceptionId":"reviewed-terminal-after-boarding",
                    "rationale":"Generated fixture deliberately chooses a backward story edge",
                    "decision":{"actor":"agent","rationale":"Scoped editorial choice"},
                    "scope":{
                        "cuts":[
                            {"operationId":"boarding","assetId":boarding,"sourceInSeconds":0.0,"sourceOutSeconds":1.0},
                            {"operationId":"terminal","assetId":terminal,"sourceInSeconds":0.0,"sourceOutSeconds":1.0}
                        ],
                        "edges":[{"earlierOperationId":"boarding","laterOperationId":"terminal"}],
                        "chronologyDependency":""
                    }
                }]
            },
            "edits":[{
                "kind":"insert",
                "anchor":{"kind":"after","operationId":"boarding"},
                "operation":{"operationId":"terminal","position":0,"assetId":terminal,
                    "sourceInSeconds":0.0,"sourceOutSeconds":1.0,"rationale":"terminal"}
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let override_args = [
        "edit-plan",
        "patch",
        "--project",
        project,
        "--expected-base",
        base_id.as_str(),
        "--expected-active",
        "none",
        "--input",
        override_patch.to_str().unwrap(),
    ];
    let overridden = success(&catalog, &override_args);
    assert_eq!(
        overridden["revision"]["storyPolicy"]["exceptions"][0]["decision"]["actor"],
        "agent"
    );
    assert_eq!(
        overridden["revision"]["operations"][1]["operationId"],
        "terminal"
    );
}
