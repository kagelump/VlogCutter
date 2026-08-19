use rcdvlog_core::PROJECT_CATALOG_SCHEMA_VERSION;
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
    assert_eq!(
        inspection.json["data"]["currentSchemaVersion"],
        PROJECT_CATALOG_SCHEMA_VERSION
    );
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
            "INSERT INTO schema_migrations (migration_id, name, source_identity, applied_at) VALUES (?1, 'future', ?2, '2026-07-20T00:00:00.000Z')",
            params![PROJECT_CATALOG_SCHEMA_VERSION + 1, format!("future/{}", PROJECT_CATALOG_SCHEMA_VERSION + 1)],
        )
        .unwrap();

    let inspection = success(&catalog, ["catalog", "inspect"]);
    assert_eq!(
        inspection.json["data"]["currentSchemaVersion"],
        PROJECT_CATALOG_SCHEMA_VERSION + 1
    );
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
fn camera_mapping_commands_list_rename_and_clear_project_scoped_state() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let project_id = create_project(&catalog, "Camera mappings");
    let other_project_id = create_project(&catalog, "Other project");
    Connection::open(&catalog)
        .unwrap()
        .execute(
            "INSERT INTO camera_identity_mappings
             (id, project_id, folder_hint, filename_prefix, exif_hint, camera, created_at, updated_at)
             VALUES ('cammap_sony', ?1, 'Sony', 'C', 'SONY ILCE-7CM2', 'Sony A7C II',
                     '2026-08-19T00:00:00Z', '2026-08-19T00:00:00Z')",
            params![project_id],
        )
        .unwrap();

    let listed = success(
        &catalog,
        ["camera-mappings", "list", "--project", &project_id],
    );
    assert_eq!(listed.json["command"], "camera-mappings.list");
    assert_eq!(listed.json["data"].as_array().unwrap().len(), 1);
    assert_eq!(listed.json["data"][0]["id"], "cammap_sony");
    assert_eq!(listed.json["data"][0]["folderHint"], "Sony");
    assert_eq!(listed.json["data"][0]["filenamePrefix"], "C");
    assert_eq!(listed.json["data"][0]["exifHint"], "SONY ILCE-7CM2");
    assert_eq!(listed.json["data"][0]["camera"], "Sony A7C II");

    // Listing is a diagnostic read: it stays available and excludes
    // uncommitted rows while another connection owns the writer slot.
    let mut writer = Connection::open(&catalog).unwrap();
    let transaction = writer.transaction().unwrap();
    transaction
        .execute(
            "INSERT INTO camera_identity_mappings
             (id, project_id, folder_hint, filename_prefix, exif_hint, camera, created_at, updated_at)
             VALUES ('cammap_uncommitted', ?1, 'DJI', 'DJI', 'DJI FC8282', 'DJI Pocket 3',
                     '2026-08-19T00:00:00Z', '2026-08-19T00:00:00Z')",
            params![project_id],
        )
        .unwrap();
    let concurrent_list = success(
        &catalog,
        ["camera-mappings", "list", "--project", &project_id],
    );
    assert_eq!(concurrent_list.json["data"].as_array().unwrap().len(), 1);
    transaction.rollback().unwrap();

    let wrong_project = failure(
        &catalog,
        [
            "camera-mappings",
            "rename",
            "--project",
            &other_project_id,
            "--mapping",
            "cammap_sony",
            "--camera",
            "Sony A7C II backup",
        ],
    );
    assert_eq!(wrong_project["command"], "camera-mappings.rename");
    assert_eq!(wrong_project["error"]["code"], "not_found");

    let renamed = success(
        &catalog,
        [
            "camera-mappings",
            "rename",
            "--project",
            &project_id,
            "--mapping",
            "cammap_sony",
            "--camera",
            "Sony A7C II backup",
        ],
    );
    assert_eq!(renamed.json["command"], "camera-mappings.rename");
    assert_eq!(renamed.json["data"]["id"], "cammap_sony");
    assert_eq!(renamed.json["data"]["camera"], "Sony A7C II backup");

    let persisted = success(
        &catalog,
        ["camera-mappings", "list", "--project", &project_id],
    );
    assert_eq!(persisted.json["data"][0]["camera"], "Sony A7C II backup");

    let cleared = success(
        &catalog,
        [
            "camera-mappings",
            "clear",
            "--project",
            &project_id,
            "--mapping",
            "cammap_sony",
        ],
    );
    assert_eq!(cleared.json["command"], "camera-mappings.clear");
    assert_eq!(cleared.json["data"]["projectId"], project_id);
    assert_eq!(cleared.json["data"]["mappingId"], "cammap_sony");
    assert_eq!(cleared.json["data"]["cleared"], true);

    let empty = success(
        &catalog,
        ["camera-mappings", "list", "--project", &project_id],
    );
    assert!(empty.json["data"].as_array().unwrap().is_empty());

    // Clear is deliberately idempotent for retries and races with the UI.
    let cleared_again = success(
        &catalog,
        [
            "camera-mappings",
            "clear",
            "--project",
            &project_id,
            "--mapping",
            "cammap_sony",
        ],
    );
    assert_eq!(cleared_again.json["data"]["cleared"], true);
}

#[test]
fn camera_merge_commands_preview_and_preserve_separate_cards() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let project_id = create_project(&catalog, "Camera merge");
    let connection = Connection::open(&catalog).unwrap();
    for (id, camera) in [("clip_wrong", "Sony wrong"), ("clip_right", "Sony")] {
        connection.execute(
            "INSERT INTO assets (id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, created_at) VALUES (?1, ?2, ?3, '', ?4, 1, 1, 30, 1920, 1080, 'h264', ?5, NULL, NULL, '2026-08-19T00:00:00Z')",
            params![id, project_id, format!("fp-{id}"), format!("{id}.MP4"), camera],
        ).unwrap();
    }
    for (id, camera) in [
        ("card_wrong", "Sony wrong"),
        ("card_right_a", "Sony"),
        ("card_right_b", "Sony"),
    ] {
        connection.execute(
            "INSERT INTO capture_sequences (id, project_id, camera_key, label, volume_hint, filename_prefix, created_at) VALUES (?1, ?2, ?3, ?1, '', '', '2026-08-19T00:00:00Z')",
            params![id, project_id, camera],
        ).unwrap();
    }
    connection.execute(
        "INSERT INTO asset_capture_sequence (asset_id, project_id, capture_sequence_id, filename_counter) VALUES ('clip_wrong', ?1, 'card_wrong', 1)",
        [&project_id],
    ).unwrap();
    connection.execute(
        "INSERT INTO clock_epochs (id, project_id, capture_sequence_id, start_counter, end_counter, offset_seconds, confidence, evidence_json, label, created_at) VALUES ('epoch_wrong', ?1, 'card_wrong', 0, NULL, 60, 1, '[]', 'kept', '2026-08-19T00:00:00Z')",
        [&project_id],
    ).unwrap();
    drop(connection);

    let preview = success(
        &catalog,
        [
            "cameras",
            "merge-preview",
            "--project",
            &project_id,
            "--from",
            "Sony wrong",
            "--into",
            "Sony",
        ],
    );
    assert_eq!(preview.json["command"], "cameras.merge-preview");
    assert_eq!(preview.json["data"]["affectedClipCount"], 1);
    assert_eq!(preview.json["data"]["affectedCaptureSequenceCount"], 1);
    assert_eq!(preview.json["data"]["destinationCaptureSequenceCount"], 2);

    let merged = success(
        &catalog,
        [
            "cameras",
            "merge",
            "--project",
            &project_id,
            "--from",
            "Sony wrong",
            "--into",
            "Sony",
        ],
    );
    assert_eq!(merged.json["command"], "cameras.merge");
    assert_eq!(merged.json["data"]["movedClipCount"], 1);
    assert_eq!(merged.json["data"]["movedCaptureSequenceCount"], 1);

    let connection = Connection::open(&catalog).unwrap();
    let cards: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM capture_sequences WHERE project_id=?1 AND camera_key='Sony'",
            [&project_id],
            |row| row.get(0),
        )
        .unwrap();
    let epoch_sequence: String = connection
        .query_row(
            "SELECT capture_sequence_id FROM clock_epochs WHERE id='epoch_wrong'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cards, 3);
    assert_eq!(epoch_sequence, "card_wrong");
}

/// An agent's whole-clip write is refused when the clip moved under it.
///
/// This is the concurrent-writer case the CLI exists to serve: an agent
/// annotating the queue while a human reviews. A whole patch carries every
/// judgment field, so applying one built from a stale read does not merely
/// miss the other writer's change — it reverses it.
#[test]
fn a_stale_clip_update_is_refused_with_a_code_the_agent_can_act_on() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let project_id = create_project(&catalog, "Concurrent writers");
    Connection::open(&catalog)
        .unwrap()
        .execute(
            "INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, notes, labels_json, removed, rotation_degrees, created_at) \
             VALUES ('asset_one', ?1, 'fingerprint', '/offline/clip.mp4', 'clip.mp4', 100, 10, 30, 1920, 1080, 'h264', 'DJI', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00.000Z', '', '[]', 0, 0, '2026-01-01T00:00:00Z')",
            params![project_id],
        )
        .unwrap();

    let read = success(&catalog, ["clips", "get", "--asset", "asset_one"]);
    let seen = read.json["data"]["revision"].as_u64().unwrap();

    // Somebody else writes first.
    success(
        &catalog,
        [
            "clips",
            "update",
            "--asset",
            "asset_one",
            "--add-label",
            "market",
        ],
    );

    let refused = failure(
        &catalog,
        [
            "clips",
            "update",
            "--asset",
            "asset_one",
            "--notes",
            "decided before the label landed",
            "--expected-revision",
            &seen.to_string(),
        ],
    );
    assert_eq!(refused["error"]["code"], "stale_revision");

    let after = success(&catalog, ["clips", "get", "--asset", "asset_one"]);
    assert_eq!(after.json["data"]["labels"][0], "market");
    assert_eq!(after.json["data"]["notes"], "");

    // Re-read, keep the edit, retry: both writers' work survives.
    let current = after.json["data"]["revision"].as_u64().unwrap();
    assert_ne!(current, seen);
    let settled = success(
        &catalog,
        [
            "clips",
            "update",
            "--asset",
            "asset_one",
            "--notes",
            "decided before the label landed",
            "--expected-revision",
            &current.to_string(),
        ],
    );
    assert_eq!(
        settled.json["data"]["notes"],
        "decided before the label landed"
    );
    assert_eq!(settled.json["data"]["labels"][0], "market");
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
