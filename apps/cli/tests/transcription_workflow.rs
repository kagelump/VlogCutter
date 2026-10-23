use rcdvlog_core::Catalog;
use rusqlite::{Connection, params};
use serde_json::Value;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::tempdir;

fn invoke_with_data_home<I, S>(catalog: &Path, data_home: &Path, arguments: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .arg("--catalog")
        .arg(catalog)
        .args(arguments)
        .env("XDG_DATA_HOME", data_home)
        .env("HOME", data_home)
        .env_remove("RCDVLOG_CATALOG")
        .output()
        .expect("run rcdvlog CLI")
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "CLI failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).expect("single JSON stdout value");
    assert_eq!(value["ok"], true);
    value
}

fn failure(output: Output) -> Value {
    assert!(!output.status.success(), "CLI unexpectedly succeeded");
    assert!(
        output.stdout.is_empty(),
        "failures must not write to stdout"
    );
    let value: Value = serde_json::from_slice(&output.stderr).expect("single JSON stderr value");
    assert_eq!(value["ok"], false);
    value
}

fn create_catalog(path: &Path) -> String {
    let catalog = Catalog::open(path).unwrap();
    catalog
        .create_project("Transcription CLI synthetic")
        .unwrap()
        .id
}

fn create_project(path: &Path, name: &str) -> String {
    let catalog = Catalog::open(path).unwrap();
    catalog.create_project(name).unwrap().id
}

fn add_asset(path: &Path, project: &str, id: &str, with_proxy: bool) -> PathBuf {
    let source = path.with_file_name(format!("{id}.mp4"));
    fs::write(&source, b"synthetic placeholder").unwrap();
    let proxy = path.with_file_name(format!("{id}.proxy.mp4"));
    if with_proxy {
        fs::write(&proxy, format!("synthetic proxy for {id}")).unwrap();
    }
    let connection = Connection::open(path).unwrap();
    connection
        .execute(
            "INSERT INTO assets(id,project_id,fingerprint,path,filename,size_bytes,duration_seconds,created_at,proxy_path) VALUES(?1,?2,?3,?4,?5,24,10,'2026-10-22T00:00:00Z',?6)",
            params![
                id,
                project,
                format!("fp-{id}"),
                source.to_string_lossy(),
                source.file_name().unwrap().to_string_lossy(),
                with_proxy.then(|| proxy.to_string_lossy().into_owned()),
            ],
        )
        .unwrap();
    proxy
}

fn mark_usable(path: &Path, asset_id: &str, fingerprint: &str) {
    Connection::open(path)
        .unwrap()
        .execute(
            "INSERT INTO asset_video_validation(asset_id,fingerprint,status,diagnostic,validated_at) VALUES(?1,?2,'usable',NULL,'2026-10-22T00:00:00Z')",
            params![asset_id, fingerprint],
        )
        .unwrap();
}

#[test]
fn model_status_is_read_only_and_reports_identity_integrity_and_location() {
    let directory = tempdir().unwrap();
    let catalog_path = directory.path().join("catalog.sqlite3");
    let data_home = directory.path().join("data-home");
    fs::create_dir_all(&data_home).unwrap();
    create_catalog(&catalog_path);
    let before = fs::read(&catalog_path).unwrap();

    let output = invoke_with_data_home(&catalog_path, &data_home, ["models", "status"]);
    let value = success(output);
    assert_eq!(value["command"], "models.status");
    let models = value["data"].as_array().unwrap();
    assert_eq!(models.len(), 2);
    let whisper = models
        .iter()
        .find(|model| model["role"] == "speech_to_text")
        .unwrap();
    assert!(
        whisper["name"]
            .as_str()
            .unwrap()
            .contains("ggml-large-v3-turbo")
    );
    assert_eq!(whisper["present"], false);
    assert_eq!(whisper["ready"], false);
    assert_eq!(whisper["installed"], false);
    assert_eq!(whisper["integrity"], "missing");
    assert!(
        whisper["detail"]
            .as_str()
            .unwrap()
            .contains("models install")
    );
    assert!(
        whisper["path"]
            .as_str()
            .unwrap()
            .contains("com.rcd.vlogcutter")
    );
    assert_eq!(
        fs::read(&catalog_path).unwrap(),
        before,
        "model status must not change catalog bytes"
    );
    assert!(
        !data_home.join("com.rcd.vlogcutter").exists(),
        "read-only status must not create the managed model directory"
    );
}

#[test]
fn selected_batch_validates_empty_duplicate_unknown_and_wrong_project_ids_before_media_work() {
    let directory = tempdir().unwrap();
    let catalog_path = directory.path().join("catalog.sqlite3");
    let data_home = directory.path().join("data-home");
    fs::create_dir_all(&data_home).unwrap();
    let project = create_catalog(&catalog_path);
    let other = create_project(&catalog_path, "Other project");
    add_asset(&catalog_path, &project, "mine", false);
    add_asset(&catalog_path, &other, "other", false);

    let empty = failure(invoke_with_data_home(
        &catalog_path,
        &data_home,
        ["transcribe", "batch", "--project", &project],
    ));
    assert_eq!(empty["command"], "parse");
    assert_eq!(empty["error"]["code"], "invalid_arguments");

    for (arguments, expected_code) in [
        (
            vec![
                "transcribe",
                "batch",
                "--project",
                project.as_str(),
                "--asset",
                "mine",
                "--asset",
                "mine",
            ],
            "duplicate_asset_id",
        ),
        (
            vec![
                "transcribe",
                "batch",
                "--project",
                project.as_str(),
                "--asset",
                "missing",
            ],
            "unknown_asset_id",
        ),
        (
            vec![
                "transcribe",
                "batch",
                "--project",
                project.as_str(),
                "--asset",
                "other",
            ],
            "asset_project_mismatch",
        ),
    ] {
        let value = failure(invoke_with_data_home(&catalog_path, &data_home, arguments));
        assert_eq!(value["command"], "transcribe.batch");
        assert_eq!(value["error"]["code"], expected_code, "{value:#}");
    }

    let connection = Connection::open(&catalog_path).unwrap();
    let transcript_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM asset_transcripts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(transcript_count, 0);
    assert!(!data_home.join("com.rcd.vlogcutter").exists());
}

#[test]
fn selected_batch_reports_missing_models_explicitly_without_downloading_or_media_work() {
    let directory = tempdir().unwrap();
    let catalog_path = directory.path().join("catalog.sqlite3");
    let data_home = directory.path().join("data-home");
    fs::create_dir_all(&data_home).unwrap();
    let project = create_catalog(&catalog_path);
    add_asset(&catalog_path, &project, "ready", true);
    mark_usable(&catalog_path, "ready", "fp-ready");

    let value = failure(invoke_with_data_home(
        &catalog_path,
        &data_home,
        [
            "transcribe",
            "batch",
            "--project",
            &project,
            "--asset",
            "ready",
        ],
    ));
    assert_eq!(value["command"], "transcribe.batch");
    assert_eq!(value["error"]["code"], "speech_models_not_ready");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("models install")
    );
    assert!(
        !data_home.join("com.rcd.vlogcutter").exists(),
        "a batch must never download models implicitly"
    );
    let connection = Connection::open(&catalog_path).unwrap();
    let transcript_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM asset_transcripts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(transcript_count, 0);
}
