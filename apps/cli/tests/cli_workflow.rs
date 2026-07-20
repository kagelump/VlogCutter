use rusqlite::{Connection, params};
use serde_json::Value;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::tempdir;

struct Invocation {
    json: Value,
    stderr: String,
}

fn invoke<I, S>(catalog: &Path, arguments: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .arg("--catalog")
        .arg(catalog)
        .args(arguments)
        .output()
        .expect("run rcdvlog CLI")
}

fn success<I, S>(catalog: &Path, arguments: I) -> Invocation
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = invoke(catalog, arguments);
    assert!(
        output.status.success(),
        "CLI failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let json: Value = serde_json::from_slice(&output.stdout).expect("single JSON stdout value");
    assert_eq!(json["ok"], true);
    Invocation {
        json,
        stderr: String::from_utf8(output.stderr).expect("UTF-8 stderr"),
    }
}

fn failure<I, S>(catalog: &Path, arguments: I) -> Value
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = invoke(catalog, arguments);
    assert!(!output.status.success(), "CLI unexpectedly succeeded");
    assert!(
        output.stdout.is_empty(),
        "failures must not write to stdout"
    );
    let json: Value = serde_json::from_slice(&output.stderr).expect("single JSON stderr value");
    assert_eq!(json["ok"], false);
    json
}

fn create_project(catalog: &Path, name: &str) -> String {
    success(catalog, ["project", "create", "--name", name]).json["data"]["id"]
        .as_str()
        .expect("project id")
        .to_string()
}

fn available_tool(doctor: &Value, name: &str) -> Option<PathBuf> {
    doctor["data"]["tools"]
        .as_array()?
        .iter()
        .find(|tool| tool["name"] == name && tool["available"] == true)
        .and_then(|tool| tool["path"].as_str())
        .map(PathBuf::from)
}

fn create_media_fixture(ffmpeg: &Path, output: &Path) {
    let status = Command::new(ffmpeg)
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=navy:s=640x360:r=30:d=1",
            "-metadata",
            "creation_time=2025-08-10T09:30:00+09:00",
            "-c:v",
            "mpeg4",
        ])
        .arg(output)
        .status()
        .expect("create fixture with ffmpeg");
    assert!(status.success(), "ffmpeg failed to create media fixture");
}

#[test]
fn process_contract_persists_across_invocations_and_routes_errors() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");

    let doctor = success(&catalog, ["doctor"]);
    assert_eq!(doctor.json["command"], "doctor");
    assert!(!catalog.exists(), "doctor must not mutate the catalog");

    let project_id = create_project(&catalog, "Process contract");
    let inspection = success(&catalog, ["catalog", "inspect"]);
    assert_eq!(inspection.json["data"]["currentSchemaVersion"], 4);
    assert_eq!(inspection.json["data"]["schemaStatus"], "current");
    assert!(
        inspection.json["data"]["pendingMigrations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let list = success(&catalog, ["project", "list"]);
    assert_eq!(list.json["data"].as_array().unwrap().len(), 1);
    assert_eq!(list.json["data"][0]["id"], project_id);

    let status = success(&catalog, ["project", "status", "--project", &project_id]);
    assert_eq!(status.json["data"]["media"]["assets"], 0);

    let missing = failure(&catalog, ["clips", "get", "--asset", "asset_missing"]);
    assert_eq!(missing["command"], "clips.get");
    assert_eq!(missing["error"]["code"], "not_found");
}

#[test]
fn newer_catalog_schema_is_reported_without_attempting_a_downgrade() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    create_project(&catalog, "Future schema");
    Connection::open(&catalog)
        .unwrap()
        .execute(
            "INSERT INTO schema_migrations (migration_id, name, source_identity, applied_at) VALUES (5, 'future', 'future/5', '2026-07-20T00:00:00.000Z')",
            [],
        )
        .unwrap();

    let inspection = success(&catalog, ["catalog", "inspect"]);
    assert_eq!(inspection.json["data"]["currentSchemaVersion"], 5);
    assert_eq!(inspection.json["data"]["schemaStatus"], "newer_unsupported");
    let rejected = failure(&catalog, ["project", "list"]);
    assert_eq!(rejected["error"]["code"], "schema_incompatible");
    assert!(
        rejected["error"]["message"]
            .as_str()
            .unwrap()
            .contains("upgrade RCD VlogCutter")
    );
}

#[test]
fn read_commands_remain_available_during_an_uncommitted_writer() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let project_id = create_project(&catalog, "Committed project");

    let mut writer = Connection::open(&catalog).unwrap();
    let transaction = writer.transaction().unwrap();
    transaction
        .execute(
            "INSERT INTO projects (id, name, created_at) VALUES (?1, ?2, ?3)",
            params![
                "project_uncommitted",
                "Uncommitted project",
                "2026-07-18T00:00:00.000Z"
            ],
        )
        .unwrap();

    let list = success(&catalog, ["project", "list"]);
    let projects = list.json["data"].as_array().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0]["id"], project_id);
    transaction.rollback().unwrap();
}

#[test]
fn media_workflow_runs_end_to_end_through_the_agent_cli() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let doctor = success(&catalog, ["doctor"]);
    let Some(ffmpeg) = available_tool(&doctor.json, "ffmpeg") else {
        eprintln!("ffmpeg is unavailable; skipping external-media integration test");
        return;
    };
    if available_tool(&doctor.json, "ffprobe").is_none() {
        eprintln!("ffprobe is unavailable; skipping external-media integration test");
        return;
    }

    let source_dir = directory.path().join("media with spaces");
    fs::create_dir_all(&source_dir).unwrap();
    let source = source_dir.join("C0001.MP4");
    create_media_fixture(&ffmpeg, &source);
    fs::write(source_dir.join("README.txt"), b"not media").unwrap();

    let project_id = create_project(&catalog, "Expo integration");
    let ingest = success(
        &catalog,
        [
            "ingest",
            "--project",
            &project_id,
            "--folder",
            source_dir.to_str().unwrap(),
        ],
    );
    assert_eq!(ingest.json["data"]["scanned"], 1);
    assert_eq!(ingest.json["data"]["imported"], 1);
    assert_eq!(ingest.json["data"]["failures"].as_array().unwrap().len(), 0);
    let progress: Value = serde_json::from_str(ingest.stderr.trim()).expect("JSON progress event");
    assert_eq!(progress["event"], "ingest.progress");
    assert_eq!(progress["completed"], 1);
    assert_eq!(progress["total"], 1);

    let duplicate = success(
        &catalog,
        [
            "ingest",
            "--project",
            &project_id,
            "--folder",
            source_dir.to_str().unwrap(),
        ],
    );
    assert_eq!(duplicate.json["data"]["imported"], 0);
    assert_eq!(duplicate.json["data"]["duplicates"], 1);

    let clips = success(&catalog, ["clips", "list", "--project", &project_id]);
    assert_eq!(clips.json["data"]["total"], 1);
    let asset = &clips.json["data"]["items"][0];
    let asset_id = asset["id"].as_str().unwrap().to_string();
    let duration = asset["durationSeconds"].as_f64().unwrap();
    assert!(duration > 0.0);
    assert_eq!(asset["width"], 640);
    assert_eq!(asset["height"], 360);

    let invalid_label = "x".repeat(65);
    let invalid = failure(
        &catalog,
        [
            "clips",
            "update",
            "--asset",
            &asset_id,
            "--add-label",
            &invalid_label,
        ],
    );
    assert_eq!(invalid["error"]["code"], "invalid_input");

    let updated = success(
        &catalog,
        [
            "clips",
            "update",
            "--asset",
            &asset_id,
            "--notes",
            "Opening station view",
            "--add-label",
            "station",
            "--add-label",
            "opening",
        ],
    );
    assert_eq!(updated.json["data"]["notes"], "Opening station view");
    assert_eq!(
        updated.json["data"]["labels"],
        serde_json::json!(["station", "opening"])
    );
    assert_eq!(updated.json["data"]["removed"], false);

    let rotated = success(
        &catalog,
        [
            "clips",
            "update",
            "--asset",
            &asset_id,
            "--rotation-degrees",
            "180",
        ],
    );
    assert_eq!(rotated.json["data"]["rotationDegrees"], 180);
    let invalid_rotation = failure(
        &catalog,
        [
            "clips",
            "update",
            "--asset",
            &asset_id,
            "--rotation-degrees",
            "45",
        ],
    );
    assert_eq!(invalid_rotation["error"]["code"], "invalid_input");

    let camera = updated.json["data"]["camera"].as_str().unwrap();
    let corrected = success(
        &catalog,
        [
            "clock",
            "apply",
            "--project",
            &project_id,
            "--camera",
            camera,
            "--start-filename",
            "C0001.MP4",
            "--offset-minutes",
            "-60",
            "--label",
            "Camera timezone changed",
        ],
    );
    assert_eq!(corrected.json["data"]["offsetSeconds"], -3600);
    let corrected_asset = success(&catalog, ["clips", "get", "--asset", &asset_id]);
    assert_eq!(corrected_asset.json["data"]["offsetSeconds"], -3600);

    let proxied = success(&catalog, ["proxy", "generate", "--asset", &asset_id]);
    let proxy_path = PathBuf::from(proxied.json["data"]["proxyPath"].as_str().unwrap());
    assert!(proxy_path.is_file());
    assert!(proxy_path.to_string_lossy().ends_with(".v3.mp4"));
    assert_eq!(proxied.json["data"]["proxyRepresentation"], "raw_v3");
    let diagnosed = success(&catalog, ["clips", "diagnose", "--asset", &asset_id]);
    assert_eq!(
        diagnosed.json["data"]["renderPlan"]["selectedRepresentation"],
        "proxy"
    );
    assert_eq!(
        diagnosed.json["data"]["renderPlan"]["finalOrientation"],
        "landscape"
    );
    let proxy_status = success(&catalog, ["proxy", "status", "--asset", &asset_id]);
    assert_eq!(proxy_status.json["data"]["summary"]["current"], 1);
    assert_eq!(
        proxy_status.json["data"]["items"][0]["validation"]["metadataRotationDegrees"],
        0
    );
    assert_eq!(
        proxy_status.json["data"]["items"][0]["regenerationRequired"],
        false
    );
    let missing = success(&catalog, ["proxy", "missing", "--project", &project_id]);
    assert_eq!(missing.json["data"]["generated"], 0);
    assert_eq!(missing.json["data"]["skipped"], 1);

    let thumbnails = success(
        &catalog,
        [
            "thumbnails",
            "missing",
            "--project",
            &project_id,
            "--quality",
            "1",
        ],
    );
    assert_eq!(thumbnails.json["data"]["quality"], 1);
    assert_eq!(thumbnails.json["data"]["generated"], 1);
    let thumbnail = success(&catalog, ["clips", "get", "--asset", &asset_id]);
    let thumbnail_path = PathBuf::from(thumbnail.json["data"]["thumbnailPath"].as_str().unwrap());
    assert!(thumbnail_path.is_file());
    assert!(thumbnail_path.starts_with(directory.path().join("cache/thumbnails")));
    let with_thumbnail = success(&catalog, ["project", "status", "--project", &project_id]);
    assert_eq!(with_thumbnail.json["data"]["media"]["thumbnailsReady"], 1);

    fs::rename(&source, source_dir.join("C0001.offline")).unwrap();
    let offline = success(&catalog, ["clips", "get", "--asset", &asset_id]);
    assert_eq!(offline.json["data"]["available"], true);
    let status = success(&catalog, ["project", "status", "--project", &project_id]);
    assert_eq!(status.json["data"]["media"]["originalsOffline"], 1);
    assert_eq!(status.json["data"]["media"]["proxiesReady"], 1);

    let removed = success(
        &catalog,
        ["clips", "update", "--asset", &asset_id, "--removed", "true"],
    );
    assert_eq!(removed.json["data"]["removed"], true);
    let active = success(&catalog, ["clips", "list", "--project", &project_id]);
    assert_eq!(active.json["data"]["total"], 0);
    let all = success(
        &catalog,
        [
            "clips",
            "list",
            "--project",
            &project_id,
            "--include-removed",
        ],
    );
    assert_eq!(all.json["data"]["total"], 1);
}
