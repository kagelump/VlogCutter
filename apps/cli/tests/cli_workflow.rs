use rcdvlog_core::{Catalog, PROJECT_CATALOG_SCHEMA_VERSION};
use rusqlite::{Connection, params};
use serde_json::Value;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
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

fn invoke_with_stdin<I, S>(catalog: &Path, arguments: I, input: &str) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut child = Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .arg("--catalog")
        .arg(catalog)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run rcdvlog CLI with JSON stdin");
    child
        .stdin
        .take()
        .expect("stdin pipe")
        .write_all(input.as_bytes())
        .expect("write JSON stdin");
    child.wait_with_output().expect("collect CLI output")
}

fn chronology_apply(catalog: &Path, project: &str, request: Value) -> Value {
    let preview_output = invoke_with_stdin(
        catalog,
        [
            "chronology",
            "preview",
            "--project",
            project,
            "--input",
            "-",
        ],
        &request.to_string(),
    );
    assert!(
        preview_output.status.success(),
        "chronology preview failed: {}",
        String::from_utf8_lossy(&preview_output.stderr)
    );
    let preview: Value = serde_json::from_slice(&preview_output.stdout).expect("preview JSON");
    let output = invoke_with_stdin(
        catalog,
        ["chronology", "apply", "--project", project, "--input", "-"],
        &serde_json::json!({"kind":request["kind"], "preview":preview["data"]["data"]}).to_string(),
    );
    assert!(
        output.status.success(),
        "chronology apply failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("chronology JSON output")
}

fn invoke_with_path<I, S>(catalog: &Path, arguments: I, path: &OsStr) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .arg("--catalog")
        .arg(catalog)
        .args(arguments)
        .env("PATH", path)
        .output()
        .expect("run rcdvlog CLI with synthetic tool")
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

#[test]
fn camera_proxy_cli_commands_are_discoverable_and_have_clear_empty_missing_results() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let project = create_project(&catalog, "Synthetic camera proxy CLI");
    let card = directory.path().join("empty-card");
    fs::create_dir(&card).unwrap();
    let discovered = success(
        &catalog,
        [
            "camera-proxy",
            "discover",
            "--project",
            &project,
            "--card",
            card.to_str().unwrap(),
        ],
    );
    assert_eq!(discovered.json["data"], serde_json::json!([]));
    let missing = failure(
        &catalog,
        ["camera-proxy", "status", "--asset", "missing_asset"],
    );
    assert_eq!(missing["error"]["code"], "not_found");
    let registration = failure(
        &catalog,
        [
            "camera-proxy",
            "register",
            "--asset",
            "missing_asset",
            "--card",
            card.to_str().unwrap(),
            "--sidecar",
            card.join("MISSING.LRF").to_str().unwrap(),
        ],
    );
    assert_eq!(registration["error"]["code"], "not_found");
}

#[test]
fn camera_proxy_cli_round_trips_a_generated_lrf_without_changing_media() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipping generated media CLI round trip: ffmpeg unavailable");
        return;
    }
    let directory = tempdir().unwrap();
    let catalog_path = directory.path().join("catalog.sqlite3");
    let project = create_project(&catalog_path, "Synthetic LRF CLI round trip");
    let card = directory.path().join("card-one");
    let dcim = card.join("DCIM");
    fs::create_dir_all(&dcim).unwrap();
    let original = dcim.join("CLIP0001.MP4");
    let sidecar = dcim.join("CLIP0001.LRF");
    for (path, dimensions) in [(&original, "640x360"), (&sidecar, "320x180")] {
        let output = Command::new("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("color=c=green:s={dimensions}:r=24:d=2"),
                "-f",
                "lavfi",
                "-i",
                "anullsrc=r=48000:cl=stereo",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
                "-f",
                "mp4",
            ])
            .arg(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let original_bytes = fs::read(&original).unwrap();
    let sidecar_bytes = fs::read(&sidecar).unwrap();
    success(
        &catalog_path,
        [
            "ingest",
            "--project",
            &project,
            "--folder",
            card.to_str().unwrap(),
        ],
    );
    let catalog = Catalog::open_existing(&catalog_path).unwrap();
    let asset_id = catalog.list_assets(&project).unwrap().remove(0).id;

    let found = success(
        &catalog_path,
        [
            "camera-proxy",
            "discover",
            "--project",
            &project,
            "--card",
            card.to_str().unwrap(),
        ],
    );
    assert_eq!(found.json["data"][0]["status"], "candidate");
    assert_eq!(found.json["data"][0]["assetId"], asset_id);
    let registered = success(
        &catalog_path,
        [
            "camera-proxy",
            "register",
            "--asset",
            &asset_id,
            "--card",
            card.to_str().unwrap(),
            "--sidecar",
            sidecar.to_str().unwrap(),
        ],
    );
    assert_eq!(registered.json["data"]["status"], "registered");
    assert!(
        registered.json["data"]["fingerprint"]
            .as_str()
            .unwrap()
            .len()
            >= 64
    );
    let status = success(
        &catalog_path,
        ["camera-proxy", "status", "--asset", &asset_id],
    );
    assert_eq!(status.json["data"]["status"], "registered");
    assert_eq!(
        status.json["data"]["cardRoot"],
        card.canonicalize().unwrap().to_string_lossy().as_ref()
    );
    let asset = catalog.get_asset(&asset_id).unwrap();
    assert!(asset.proxy_path.is_none());
    assert_eq!(fs::read(&original).unwrap(), original_bytes);
    assert_eq!(fs::read(&sidecar).unwrap(), sidecar_bytes);
}

#[test]
fn chronology_preview_is_reusable_revision_guarded_and_process_safe() {
    let directory = tempdir().expect("temporary catalog directory");
    let catalog = directory.path().join("catalog.sqlite3");
    let project = create_project(&catalog, "synthetic chronology");
    let request_path = directory.path().join("request.json");
    let sequence_request = serde_json::json!({
        "kind": "sequenceDeclare", "cameraKey": "Synthetic Camera", "label": "Card A",
        "volumeHint": "CARD-A", "filenamePrefix": "SYN_"
    });
    let sequence = chronology_apply(&catalog, &project, sequence_request);
    let sequence = sequence["data"]["data"]["sequence"]["id"]
        .as_str()
        .expect("sequence identity")
        .to_owned();

    let epoch_request = serde_json::json!({
        "kind": "epochSet", "epoch": {"id":"", "captureSequenceId":sequence,
        "startCounter":1,"endCounter":null,"offsetSeconds":3600,"confidence":0.9,
        "evidence":["synthetic review"],"label":"Synthetic clock correction"}
    });
    let output = invoke_with_stdin(
        &catalog,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            "-",
        ],
        &epoch_request.to_string(),
    );
    assert!(
        output.status.success(),
        "preview failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let preview: Value = serde_json::from_slice(&output.stdout).expect("preview JSON envelope");
    assert_eq!(preview["data"]["kind"], "epochSet");
    assert!(preview["data"]["data"]["factsToken"].as_str().is_some());
    assert_eq!(
        preview["data"]["data"]["epoch"]["captureSequenceId"],
        sequence
    );

    fs::write(
        &request_path,
        serde_json::json!({
            "kind":"merge", "fromSequenceId":"missing-sequence", "intoSequenceId":sequence
        })
        .to_string(),
    )
    .expect("write invalid reference request");
    let invalid = failure(
        &catalog,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            request_path.to_str().unwrap(),
        ],
    );
    assert_eq!(invalid["error"]["code"], "chronology.invalid_reference");

    fs::write(
        &request_path,
        serde_json::json!({"kind":"epochSet", "preview":preview["data"]["data"]}).to_string(),
    )
    .expect("write apply request");
    let applied = success(
        &catalog,
        [
            "chronology",
            "apply",
            "--project",
            &project,
            "--input",
            request_path.to_str().unwrap(),
        ],
    );
    assert_eq!(applied.json["data"]["data"]["offsetSeconds"], 3600);
    let stale = failure(
        &catalog,
        [
            "chronology",
            "apply",
            "--project",
            &project,
            "--input",
            request_path.to_str().unwrap(),
        ],
    );
    assert_eq!(stale["error"]["code"], "chronology.stale_preview");

    let epoch_id = preview["data"]["data"]["epoch"]["id"]
        .as_str()
        .expect("epoch identity");
    let clear_request = serde_json::json!({"kind":"epochClear", "epochId":epoch_id});
    let clear_preview_output = invoke_with_stdin(
        &catalog,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            "-",
        ],
        &clear_request.to_string(),
    );
    assert!(
        clear_preview_output.status.success(),
        "clear preview failed: {}",
        String::from_utf8_lossy(&clear_preview_output.stderr)
    );
    let clear_preview: Value =
        serde_json::from_slice(&clear_preview_output.stdout).expect("epoch clear preview JSON");
    assert_eq!(clear_preview["data"]["kind"], "epochClear");
    assert_eq!(clear_preview["data"]["data"]["epochId"], epoch_id);
    assert!(
        clear_preview["data"]["data"]["factsToken"]
            .as_str()
            .is_some()
    );
    fs::write(
        &request_path,
        serde_json::json!({
            "kind":"epochClear", "preview":clear_preview["data"]["data"]
        })
        .to_string(),
    )
    .expect("write epoch clear apply request");

    // Another chronology change invalidates the saved clear preview. The
    // adapter returns the same machine-readable stale code used by other kinds.
    chronology_apply(
        &catalog,
        &project,
        serde_json::json!({
            "kind":"sequenceDeclare", "cameraKey":"Synthetic Camera",
            "label":"Unrelated card", "volumeHint":"CARD-B", "filenamePrefix":"SYN_"
        }),
    );
    let stale_clear = failure(
        &catalog,
        [
            "chronology",
            "apply",
            "--project",
            &project,
            "--input",
            request_path.to_str().unwrap(),
        ],
    );
    assert_eq!(stale_clear["error"]["code"], "chronology.stale_preview");

    let refreshed_clear_preview = invoke_with_stdin(
        &catalog,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            "-",
        ],
        &clear_request.to_string(),
    );
    assert!(
        refreshed_clear_preview.status.success(),
        "refreshed clear preview failed: {}",
        String::from_utf8_lossy(&refreshed_clear_preview.stderr)
    );
    let refreshed_clear_preview: Value =
        serde_json::from_slice(&refreshed_clear_preview.stdout).unwrap();
    fs::write(
        &request_path,
        serde_json::json!({
            "kind":"epochClear", "preview":refreshed_clear_preview["data"]["data"]
        })
        .to_string(),
    )
    .expect("write refreshed epoch clear apply request");
    let cleared = success(
        &catalog,
        [
            "chronology",
            "apply",
            "--project",
            &project,
            "--input",
            request_path.to_str().unwrap(),
        ],
    );
    assert_eq!(cleared.json["data"]["data"]["applied"], true);
    let retry = failure(
        &catalog,
        [
            "chronology",
            "apply",
            "--project",
            &project,
            "--input",
            request_path.to_str().unwrap(),
        ],
    );
    assert_eq!(retry["error"]["code"], "chronology.invalid_reference");
}

#[test]
fn chronology_direct_preview_apply_can_be_retried_after_cli_restart() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let project = create_project(&catalog, "synthetic retry");
    let request_path = directory.path().join("sequence-request.json");
    fs::write(
        &request_path,
        serde_json::json!({"kind":"sequenceDeclare", "cameraKey":"Synthetic", "label":"Card", "volumeHint":"CARD", "filenamePrefix":"SYN_"}).to_string(),
    ).unwrap();
    let preview = success(
        &catalog,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            request_path.to_str().unwrap(),
        ],
    )
    .json;
    let apply_path = directory.path().join("sequence-apply.json");
    fs::write(
        &apply_path,
        serde_json::json!({"kind":"sequenceDeclare", "preview":preview["data"]["data"]})
            .to_string(),
    )
    .unwrap();
    success(
        &catalog,
        [
            "chronology",
            "apply",
            "--project",
            &project,
            "--input",
            apply_path.to_str().unwrap(),
        ],
    );
    // A second process receives the same candidate after the caller lost its
    // response. The core recognizes the durable sequence and does not append a duplicate.
    success(
        &catalog,
        [
            "chronology",
            "apply",
            "--project",
            &project,
            "--input",
            apply_path.to_str().unwrap(),
        ],
    );
    let sequences = success(&catalog, ["chronology", "sequences", "--project", &project]);
    assert_eq!(
        sequences.json["data"]["sequences"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn chronology_exact_split_never_expands_after_a_later_import() {
    let directory = tempdir().expect("temporary catalog directory");
    let catalog = directory.path().join("catalog.sqlite3");
    let project = create_project(&catalog, "synthetic exact split");
    // Declare through the CLI so the sequence itself is an ordinary typed operation.
    let sequence_request = serde_json::json!({
        "kind":"sequenceDeclare", "cameraKey":"Synthetic Camera", "label":"Card",
        "volumeHint":"CARD", "filenamePrefix":"SYN_"
    });
    let sequence = chronology_apply(&catalog, &project, sequence_request);
    let sequence_id = sequence["data"]["data"]["sequence"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // These are synthetic rows in the temporary test catalog. The sequence is
    // created by the domain API; rows stand in for three later imported clips.
    let connection = Connection::open(&catalog).unwrap();
    for (asset_id, counter, captured) in [
        ("clip_one", 1_i64, "2026-01-01T00:00:00Z"),
        ("clip_two", 2, "2026-01-01T00:01:00Z"),
        ("clip_later", 2, "2026-01-01T00:02:00Z"),
    ] {
        connection.execute(
            "INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, notes, labels_json, removed, rotation_degrees, created_at) VALUES (?1, ?2, ?3, ?4, ?5, 100, 1, 30, 1920, 1080, 'h264', 'Synthetic Camera', ?6, NULL, '', '[]', 0, 0, '2026-01-01T00:00:00Z')",
            rusqlite::params![asset_id, project, format!("fingerprint-{asset_id}"), format!("/synthetic/{asset_id}.mp4"), format!("{asset_id}.mp4"), captured],
        ).unwrap();
        connection.execute(
            "INSERT INTO asset_capture_sequence(asset_id, project_id, capture_sequence_id, filename_counter) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![asset_id, project, sequence_id, counter],
        ).unwrap();
    }
    drop(connection);

    let preview_request = serde_json::json!({
        "kind":"split", "sourceSequenceId":sequence_id, "atCounter":2,
        "label":"Card B", "assetIds":["clip_two"]
    });
    let preview = invoke_with_stdin(
        &catalog,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            "-",
        ],
        &preview_request.to_string(),
    );
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let preview: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(
        preview["data"]["data"]["assetIds"],
        serde_json::json!(["clip_two"])
    );

    let later_connection = Connection::open(&catalog).unwrap();
    later_connection.execute(
        "INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, notes, labels_json, removed, rotation_degrees, created_at) VALUES ('clip_after_preview', ?1, 'fingerprint-later', '/synthetic/later.mp4', 'later.mp4', 100, 1, 30, 1920, 1080, 'h264', 'Synthetic Camera', NULL, NULL, '', '[]', 0, 0, '2026-01-01T00:00:00Z')",
        [&project],
    ).unwrap();
    later_connection.execute(
        "INSERT INTO asset_capture_sequence(asset_id, project_id, capture_sequence_id, filename_counter) VALUES ('clip_after_preview', ?1, ?2, 2)",
        rusqlite::params![project, sequence_id],
    ).unwrap();
    drop(later_connection);

    let stale_request = serde_json::json!({"kind":"split", "preview":preview["data"]["data"]});
    let stale = invoke_with_stdin(
        &catalog,
        ["chronology", "apply", "--project", &project, "--input", "-"],
        &stale_request.to_string(),
    );
    assert!(!stale.status.success());
    let stale: Value = serde_json::from_slice(&stale.stderr).unwrap();
    assert_eq!(stale["error"]["code"], "chronology.stale_preview");

    let refreshed = invoke_with_stdin(
        &catalog,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            "-",
        ],
        &preview_request.to_string(),
    );
    let refreshed: Value = serde_json::from_slice(&refreshed.stdout).unwrap();
    assert_eq!(
        refreshed["data"]["data"]["assetIds"],
        serde_json::json!(["clip_two"])
    );
    let apply = invoke_with_stdin(
        &catalog,
        ["chronology", "apply", "--project", &project, "--input", "-"],
        &serde_json::json!({"kind":"split", "preview":refreshed["data"]["data"]}).to_string(),
    );
    assert!(
        apply.status.success(),
        "{}",
        String::from_utf8_lossy(&apply.stderr)
    );
    let assignments = Connection::open(&catalog).unwrap();
    let moved: String = assignments
        .query_row(
            "SELECT capture_sequence_id FROM asset_capture_sequence WHERE asset_id='clip_two'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(moved, sequence_id);
    for asset_id in ["clip_later", "clip_after_preview"] {
        let retained: String = assignments
            .query_row(
                "SELECT capture_sequence_id FROM asset_capture_sequence WHERE asset_id=?1",
                [asset_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(retained, sequence_id);
    }

    let base_phase = chronology_apply(
        &catalog,
        &project,
        serde_json::json!({"kind":"phaseSet", "phase":{"id":"", "label":"Synthetic trip", "kind":"location", "startAnchorAssetId":null, "endAnchorAssetId":null, "startTripTime":"2026-01-01T00:00:00Z", "endTripTime":"2026-01-01T00:05:00Z", "timezone":"Etc/UTC", "originTimezone":null, "destinationTimezone":null, "displayMode":"local"}}),
    );
    let base_phase_id = base_phase["data"]["data"]["phase"]["id"].as_str().unwrap();
    let phase_split_request = serde_json::json!({"kind":"phaseSplit", "existingPhaseId":base_phase_id, "boundaryAssetId":"clip_two", "direction":"through", "phase":{"id":"", "label":"Synthetic first leg", "kind":"transit", "startAnchorAssetId":null, "endAnchorAssetId":null, "startTripTime":"2026-01-01T00:00:00Z", "endTripTime":"2026-01-01T00:01:00Z", "timezone":"Etc/UTC", "originTimezone":null, "destinationTimezone":null, "displayMode":"local"}});
    let phase_preview_output = invoke_with_stdin(
        &catalog,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            "-",
        ],
        &phase_split_request.to_string(),
    );
    assert!(
        phase_preview_output.status.success(),
        "{}",
        String::from_utf8_lossy(&phase_preview_output.stderr)
    );
    let phase_preview: Value = serde_json::from_slice(&phase_preview_output.stdout).unwrap();
    assert_eq!(
        phase_preview["data"]["data"]["affectedAssetIds"],
        serde_json::json!(["clip_later", "clip_one", "clip_two"])
    );
    let phase_split_apply = invoke_with_stdin(
        &catalog,
        ["chronology", "apply", "--project", &project, "--input", "-"],
        &serde_json::json!({"kind":"phaseSplit", "preview":phase_preview["data"]["data"]})
            .to_string(),
    );
    assert!(
        phase_split_apply.status.success(),
        "{}",
        String::from_utf8_lossy(&phase_split_apply.stderr)
    );

    chronology_apply(
        &catalog,
        &project,
        serde_json::json!({"kind":"sameCard", "sequenceId":sequence_id, "atCounter":2, "reason":"synthetic confirmation"}),
    );
    chronology_apply(
        &catalog,
        &project,
        serde_json::json!({"kind":"sameCardRestore", "sequenceId":sequence_id, "atCounter":2}),
    );
    chronology_apply(
        &catalog,
        &project,
        serde_json::json!({"kind":"exceptionSet", "assetId":"clip_one", "tripTime":"2026-01-01T01:00:00Z", "reason":"synthetic correction"}),
    );
    chronology_apply(
        &catalog,
        &project,
        serde_json::json!({"kind":"exceptionClear", "assetId":"clip_one"}),
    );
    let phase = chronology_apply(
        &catalog,
        &project,
        serde_json::json!({
            "kind":"phaseSet", "phase":{"id":"", "label":"Synthetic location", "kind":"location",
            "startAnchorAssetId":null,"endAnchorAssetId":null,
            "startTripTime":"2026-01-01T00:00:00Z","endTripTime":"2026-01-02T00:00:00Z",
            "timezone":"Etc/UTC","originTimezone":null,"destinationTimezone":null,"displayMode":"local"}
        }),
    );
    chronology_apply(
        &catalog,
        &project,
        serde_json::json!({"kind":"phaseRemove", "phaseId":phase["data"]["data"]["phase"]["id"]}),
    );
    let constraint = chronology_apply(
        &catalog,
        &project,
        serde_json::json!({"kind":"constraintSet", "earlierAssetId":"clip_one", "laterAssetId":"clip_two", "strength":"soft", "source":"synthetic test"}),
    );
    chronology_apply(
        &catalog,
        &project,
        serde_json::json!({"kind":"constraintClear", "constraintId":constraint["data"]["data"]["constraint"]["id"]}),
    );
}

#[test]
fn chronology_reassignment_and_merge_use_reusable_exact_previews() {
    let directory = tempdir().expect("temporary catalog directory");
    let catalog = directory.path().join("catalog.sqlite3");
    let project = create_project(&catalog, "synthetic sequence move");
    let declare = |label: &str| {
        chronology_apply(
            &catalog,
            &project,
            serde_json::json!({
                "kind":"sequenceDeclare", "cameraKey":"Synthetic Camera", "label":label,
                "volumeHint":label, "filenamePrefix":"SYN_"
            }),
        )["data"]["data"]["sequence"]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let original = declare("Original card");
    let destination = declare("Destination card");
    let connection = Connection::open(&catalog).unwrap();
    connection.execute(
        "INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, notes, labels_json, removed, rotation_degrees, created_at) VALUES ('clip_selected', ?1, 'fingerprint-selected', '/synthetic/selected.mp4', 'selected.mp4', 100, 1, 30, 1920, 1080, 'h264', 'Synthetic Camera', '2026-01-01T00:00:00Z', NULL, '', '[]', 0, 0, '2026-01-01T00:00:00Z')",
        [&project],
    ).unwrap();
    connection.execute(
        "INSERT INTO asset_capture_sequence(asset_id, project_id, capture_sequence_id, filename_counter) VALUES ('clip_selected', ?1, ?2, 1)",
        rusqlite::params![project, original],
    ).unwrap();
    drop(connection);

    let preview = invoke_with_stdin(
        &catalog,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            "-",
        ],
        &serde_json::json!({
            "kind":"reassign", "fromSequenceId":original,
            "intoSequenceId":destination, "assetIds":["clip_selected"]
        })
        .to_string(),
    );
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let preview: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(
        preview["data"]["data"]["assetIds"],
        serde_json::json!(["clip_selected"])
    );
    assert!(preview["data"]["data"]["factsToken"].as_str().is_some());
    let apply = invoke_with_stdin(
        &catalog,
        ["chronology", "apply", "--project", &project, "--input", "-"],
        &serde_json::json!({"kind":"reassign", "preview":preview["data"]["data"]}).to_string(),
    );
    assert!(
        apply.status.success(),
        "{}",
        String::from_utf8_lossy(&apply.stderr)
    );

    let merge_preview = invoke_with_stdin(
        &catalog,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            "-",
        ],
        &serde_json::json!({
            "kind":"merge", "fromSequenceId":destination,
            "intoSequenceId":original
        })
        .to_string(),
    );
    assert!(
        merge_preview.status.success(),
        "{}",
        String::from_utf8_lossy(&merge_preview.stderr)
    );
    let merge_preview: Value = serde_json::from_slice(&merge_preview.stdout).unwrap();
    assert!(
        merge_preview["data"]["data"]["factsToken"]
            .as_str()
            .is_some()
    );
    let merge = invoke_with_stdin(
        &catalog,
        ["chronology", "apply", "--project", &project, "--input", "-"],
        &serde_json::json!({"kind":"merge", "preview":merge_preview["data"]["data"]}).to_string(),
    );
    assert!(
        merge.status.success(),
        "{}",
        String::from_utf8_lossy(&merge.stderr)
    );
    let connection = Connection::open(&catalog).unwrap();
    let assigned: String = connection
        .query_row(
            "SELECT capture_sequence_id FROM asset_capture_sequence WHERE asset_id='clip_selected'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(assigned, original);
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

fn create_audio_only_fixture(ffmpeg: &Path, output: &Path) {
    let status = Command::new(ffmpeg)
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-vn",
            "-c:a",
            "aac",
        ])
        .arg(output)
        .status()
        .expect("create audio-only fixture");
    assert!(status.success());
}

fn create_zero_frame_fixture(ffmpeg: &Path, output: &Path) {
    let status = Command::new(ffmpeg)
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=64x64:r=1",
            "-frames:v",
            "0",
            "-c:v",
            "mpeg4",
        ])
        .arg(output)
        .status()
        .expect("create zero-frame fixture");
    assert!(
        status.success(),
        "ffmpeg failed to create zero-frame fixture"
    );
}

#[test]
fn ingest_quarantines_reachable_files_without_video_and_preserves_originals() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let doctor = success(&catalog, ["doctor"]);
    let Some(ffmpeg) = available_tool(&doctor.json, "ffmpeg") else {
        return;
    };
    if available_tool(&doctor.json, "ffprobe").is_none() {
        return;
    }
    let source_dir = directory.path().join("synthetic");
    fs::create_dir_all(&source_dir).unwrap();
    let audio = source_dir.join("audio-only.mp4");
    let zero = source_dir.join("zero-frame.mp4");
    create_audio_only_fixture(&ffmpeg, &audio);
    create_zero_frame_fixture(&ffmpeg, &zero);
    let audio_bytes = fs::read(&audio).unwrap();
    let zero_bytes = fs::read(&zero).unwrap();
    let project = create_project(&catalog, "Synthetic ingest");
    let ingest = success(
        &catalog,
        [
            "ingest",
            "--project",
            &project,
            "--folder",
            source_dir.to_str().unwrap(),
        ],
    );
    assert_eq!(ingest.json["data"]["imported"], 0);
    assert_eq!(ingest.json["data"]["failures"].as_array().unwrap().len(), 2);
    assert_eq!(fs::read(&audio).unwrap(), audio_bytes);
    assert_eq!(fs::read(&zero).unwrap(), zero_bytes);
    let clips = success(&catalog, ["clips", "list", "--project", &project]);
    assert_eq!(clips.json["data"]["total"], 0);
    let diagnostics = success(&catalog, ["diagnostics", "list", "--project", &project]);
    let items = diagnostics.json["data"]["diagnostics"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .all(|diagnostic| diagnostic["kind"] == "ingest.no_video_stream")
    );
    assert!(
        items
            .iter()
            .all(|diagnostic| diagnostic["context"]["usableVideo"] == false)
    );
    assert!(
        items
            .iter()
            .all(|diagnostic| diagnostic["context"]["reason"].as_str().is_some())
    );
}

#[test]
fn generic_unreadable_media_diagnostics_keep_each_filename_and_reason() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let doctor = success(&catalog, ["doctor"]);
    if available_tool(&doctor.json, "ffprobe").is_none() {
        return;
    }
    let source_dir = directory.path().join("synthetic");
    fs::create_dir_all(&source_dir).unwrap();
    let first = source_dir.join("broken-one.mp4");
    let second = source_dir.join("broken-two.mp4");
    fs::write(&first, b"synthetic invalid media one").unwrap();
    fs::write(&second, b"synthetic invalid media two").unwrap();
    let project = create_project(&catalog, "Synthetic unreadable media");

    let ingest = success(
        &catalog,
        [
            "ingest",
            "--project",
            &project,
            "--folder",
            source_dir.to_str().unwrap(),
        ],
    );
    assert_eq!(ingest.json["data"]["imported"], 0);
    let diagnostics = success(&catalog, ["diagnostics", "list", "--project", &project]);
    let items = diagnostics.json["data"]["diagnostics"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    let files = items
        .iter()
        .map(|item| item["context"]["filename"].as_str().unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        files,
        ["broken-one.mp4", "broken-two.mp4"].into_iter().collect()
    );
    assert!(
        items
            .iter()
            .all(|item| item["kind"] == "ingest.unreadable_media")
    );
    assert!(items.iter().all(|item| {
        item["context"]["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty())
    }));
}

#[cfg(unix)]
#[test]
fn legacy_asset_with_incomplete_video_stream_is_not_reported_usable() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let doctor = success(&catalog, ["doctor"]);
    let Some(ffmpeg) = available_tool(&doctor.json, "ffmpeg") else {
        return;
    };
    if available_tool(&doctor.json, "ffprobe").is_none() {
        return;
    }
    let media = directory.path().join("legacy.mp4");
    create_media_fixture(&ffmpeg, &media);
    let original_bytes = fs::read(&media).unwrap();
    let source_dir = directory.path().join("legacy-source");
    fs::create_dir_all(&source_dir).unwrap();
    let source = source_dir.join("legacy.mp4");
    fs::copy(&media, &source).unwrap();
    let project = create_project(&catalog, "Synthetic legacy row");
    let ingest = success(
        &catalog,
        [
            "ingest",
            "--project",
            &project,
            "--folder",
            source_dir.to_str().unwrap(),
        ],
    );
    let listed = success(&catalog, ["clips", "list", "--project", &project]);
    let asset_id = listed.json["data"]["items"][0]["id"].as_str().unwrap();
    assert_eq!(ingest.json["data"]["imported"], 1);
    Connection::open(&catalog)
        .unwrap()
        .execute(
            "DELETE FROM asset_video_validation WHERE asset_id = ?1",
            [asset_id],
        )
        .unwrap();

    let shim_dir = directory.path().join("shim");
    fs::create_dir_all(&shim_dir).unwrap();
    let shim = shim_dir.join("ffprobe");
    fs::write(&shim, "#!/bin/sh\nprintf '%s\\n' '{\"streams\":[{\"codec_type\":\"video\",\"width\":640,\"height\":360,\"codec_name\":\"mpeg4\"}],\"format\":{\"duration\":\"1.0\"}}'\n").unwrap();
    let mut permissions = fs::metadata(&shim).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&shim, permissions).unwrap();
    let output = invoke_with_path(
        &catalog,
        ["clips", "diagnose", "--asset", asset_id],
        shim_dir.as_os_str(),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let diagnosis: Value = serde_json::from_slice(&output.stdout).unwrap();
    let data = &diagnosis["data"];
    assert_eq!(
        data["representations"]["originalReadiness"]["bytesReachable"],
        true
    );
    assert_eq!(
        data["representations"]["originalReadiness"]["usableVideo"],
        false
    );
    assert_eq!(
        data["representations"]["originalReadiness"]["diagnosis"],
        "incomplete_video_stream"
    );
    assert_eq!(
        data["representations"]["catalogVideoValidation"]["status"],
        "unverified"
    );
    assert!(
        data["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("not usable video"))
    );
    assert!(
        data["recommendedActions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action.as_str().unwrap().contains("valid dimensions"))
    );

    let unverified_status = success(&catalog, ["project", "status", "--project", &project]);
    assert_eq!(
        unverified_status.json["data"]["analysisReadiness"]["totalClips"],
        0
    );
    assert_eq!(
        unverified_status.json["data"]["analysisReadiness"]["unverifiedVideoClips"],
        1
    );
    assert_eq!(
        unverified_status.json["data"]["analysisReadiness"]["reachableOriginals"],
        1
    );

    let operations_before = Connection::open(&catalog)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM project_operations", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    let validation = invoke_with_path(
        &catalog,
        ["clips", "validate-video", "--asset", asset_id],
        shim_dir.as_os_str(),
    );
    assert!(
        validation.status.success(),
        "{}",
        String::from_utf8_lossy(&validation.stderr)
    );
    let validation: Value = serde_json::from_slice(&validation.stdout).unwrap();
    assert_eq!(validation["data"]["status"], "quarantined");
    assert_eq!(validation["data"]["bytesReachable"], true);
    assert!(
        validation["data"]["diagnostic"]
            .as_str()
            .unwrap()
            .contains("frame rate")
    );
    let operations_after = Connection::open(&catalog)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM project_operations", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(operations_after, operations_before);

    let status = success(&catalog, ["project", "status", "--project", &project]);
    assert_eq!(status.json["data"]["media"]["assets"], 1);
    assert_eq!(status.json["data"]["media"]["durationSeconds"], 0.0);
    assert_eq!(
        status.json["data"]["media"]["videoValidation"]["usableVideo"],
        0
    );
    assert_eq!(
        status.json["data"]["media"]["videoValidation"]["quarantinedVideo"],
        1
    );
    assert_eq!(
        status.json["data"]["media"]["videoValidation"]["reachableOriginals"],
        1
    );
    assert_eq!(status.json["data"]["analysisReadiness"]["totalClips"], 0);
    assert_eq!(
        status.json["data"]["analysisReadiness"]["quarantinedVideoClips"],
        1
    );
    assert_eq!(
        status.json["data"]["analysisReadiness"]["reachableOriginals"],
        1
    );
    assert_eq!(fs::read(&source).unwrap(), original_bytes);
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

#[cfg(unix)]
#[test]
fn media_tool_failures_keep_cli_stderr_as_one_redacted_json_value() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let media = directory.path().join("synthetic.mp4");
    let doctor = success(&catalog, ["doctor"]);
    let Some(ffmpeg) = available_tool(&doctor.json, "ffmpeg") else {
        return;
    };
    if available_tool(&doctor.json, "ffprobe").is_none() {
        return;
    }
    create_media_fixture(&ffmpeg, &media);
    let project_id = create_project(&catalog, "Synthetic media failure");
    success(
        &catalog,
        [
            "ingest",
            "--project",
            &project_id,
            "--folder",
            directory.path().to_str().unwrap(),
        ],
    );
    let asset_id = success(&catalog, ["clips", "list", "--project", &project_id]).json
        ["data"]["items"][0]["id"]
        .as_str()
        .expect("ingested synthetic asset")
        .to_owned();

    let shim_dir = directory.path().join("tool-shim");
    fs::create_dir_all(&shim_dir).unwrap();
    let shim = shim_dir.join("ffmpeg");
    fs::write(
        &shim,
        "#!/bin/sh\nprintf 'out_time_us=0\\nprogress=continue\\n'\nfor arg do case \"$arg\" in /*) printf 'synthetic decoder detail for %s\\n' \"$arg\" >&2;; esac; done\nprintf 'Invalid data found while processing input\\n' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
    let current_path = env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![shim_dir];
    paths.extend(env::split_paths(&current_path));
    let path = env::join_paths(paths).unwrap();

    for args in [
        vec!["proxy", "generate", "--asset", asset_id.as_str()],
        vec![
            "thumbnails",
            "generate",
            "--asset",
            asset_id.as_str(),
            "--quality",
            "1",
        ],
    ] {
        let output = invoke_with_path(&catalog, args, &path);
        assert!(!output.status.success(), "tool failure must exit nonzero");
        assert!(
            output.stdout.is_empty(),
            "tool failures must keep stdout empty"
        );
        let stderr = String::from_utf8(output.stderr).expect("CLI stderr is UTF-8");
        let error: Value = serde_json::from_str(&stderr).expect("stderr is exactly one JSON value");
        assert_eq!(error["ok"], false);
        assert_eq!(error["error"]["code"], "media_tool_failed");
        let message = error["error"]["message"].as_str().unwrap();
        assert!(message.contains("Invalid data found while processing input"));
        assert!(!message.contains(directory.path().to_str().unwrap()));
        assert!(message.len() < 4600, "diagnostic should remain bounded");
    }
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
            "--camera",
            "Synthetic selected camera",
        ],
    );
    assert_eq!(ingest.json["data"]["scanned"], 1);
    assert_eq!(ingest.json["data"]["imported"], 1);
    assert_eq!(ingest.json["data"]["failures"].as_array().unwrap().len(), 0);
    let imported = success(&catalog, ["clips", "list", "--project", &project_id]);
    let asset_id = imported.json["data"]["items"][0]["id"].as_str().unwrap();
    let asset = success(&catalog, ["clips", "get", "--asset", asset_id]);
    assert_eq!(asset.json["data"]["camera"], "Synthetic selected camera");
    let repair_preview = success(
        &catalog,
        [
            "cameras",
            "repair-preview",
            "--project",
            &project_id,
            "--camera",
            "Corrected Synthetic Camera",
            "--asset",
            asset_id,
        ],
    );
    let preview_path = directory.path().join("camera-repair-preview.json");
    fs::write(
        &preview_path,
        serde_json::to_vec(&repair_preview.json).unwrap(),
    )
    .unwrap();
    success(
        &catalog,
        [
            "cameras",
            "repair",
            "--preview",
            preview_path.to_str().unwrap(),
        ],
    );
    let repaired = success(&catalog, ["clips", "get", "--asset", asset_id]);
    assert_eq!(
        repaired.json["data"]["camera"],
        "Corrected Synthetic Camera"
    );
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
