use rcdvlog_core::Catalog;
use rusqlite::{Connection, params};
use serde_json::Value;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::tempdir;

fn invoke<I, S>(catalog: &Path, args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .arg("--catalog")
        .arg(catalog)
        .args(args)
        .output()
        .expect("run CLI")
}

fn success<I, S>(catalog: &Path, args: I) -> Value
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = invoke(catalog, args);
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], true);
    value["data"].clone()
}

fn failure<I, S>(catalog: &Path, args: I) -> Value
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = invoke(catalog, args);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["ok"], false);
    value
}

fn create_catalog(path: &Path) -> String {
    let catalog = Catalog::open(path).unwrap();
    catalog
        .create_project("Preparation CLI synthetic")
        .unwrap()
        .id
}

fn add_asset(path: &Path, project: &str, id: &str) {
    let source = path.with_file_name(format!("{id}.mp4"));
    fs::write(&source, b"synthetic test placeholder").unwrap();
    let connection = Connection::open(path).unwrap();
    connection.execute(
        "INSERT INTO assets(id,project_id,fingerprint,path,filename,size_bytes,duration_seconds,created_at) VALUES(?1,?2,?3,?4,?5,24,10,'2026-10-22T00:00:00Z')",
        params![id, project, format!("fp-{id}"), source.to_string_lossy(), source.file_name().unwrap().to_string_lossy()],
    ).unwrap();
}

#[test]
fn preparation_preflight_and_evidence_are_bounded_read_only_pages() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite3");
    let project = create_catalog(&path);
    for id in ["a", "b", "c"] {
        add_asset(&path, &project, id);
    }
    let before = fs::read(&path).unwrap();
    let first = success(
        &path,
        [
            "prepare",
            "preflight",
            "--project",
            &project,
            "--all",
            "--limit",
            "1",
        ],
    );
    assert_eq!(first["totalAssets"], 3);
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    assert_eq!(first["nextOffset"], 1);
    assert_eq!(first["pageOutcome"], "review_required");
    let second = success(
        &path,
        [
            "prepare",
            "preflight",
            "--project",
            &project,
            "--all",
            "--offset",
            "1",
            "--limit",
            "1",
        ],
    );
    assert_eq!(second["items"][0]["assetId"], "b");
    let transcript = success(
        &path,
        [
            "prepare",
            "evidence",
            "--project",
            &project,
            "--asset",
            "a",
            "--kind",
            "transcript",
            "--limit",
            "1",
        ],
    );
    assert_eq!(transcript["assetId"], "a");
    assert_eq!(transcript["segments"].as_array().unwrap().len(), 0);
    let no_moments = failure(
        &path,
        [
            "prepare",
            "evidence",
            "--project",
            &project,
            "--asset",
            "a",
            "--kind",
            "moments",
            "--offset",
            "0",
            "--limit",
            "1",
        ],
    );
    assert_eq!(no_moments["command"], "prepare.evidence");
    assert!(
        no_moments["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no moment evidence")
    );
    assert_eq!(
        fs::read(&path).unwrap(),
        before,
        "read-only preparation commands changed catalog bytes"
    );
}

#[test]
fn preparation_requires_existing_explicit_catalog_and_rejects_bad_scope_page_and_paid_policy() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite3");
    let project = create_catalog(&path);
    let missing = directory.path().join("must-not-be-created.sqlite3");
    let no_catalog = Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .env_remove("RCDVLOG_CATALOG")
        .env("XDG_DATA_HOME", directory.path())
        .args([
            "prepare",
            "preflight",
            "--project",
            &project,
            "--asset",
            "missing",
        ])
        .output()
        .unwrap();
    assert!(!no_catalog.status.success());
    let error: Value = serde_json::from_slice(&no_catalog.stderr).unwrap();
    assert_eq!(error["error"]["code"], "invalid_input");
    assert!(!missing.exists());

    let nonexistent = invoke(
        &missing,
        [
            "prepare",
            "preflight",
            "--project",
            &project,
            "--asset",
            "x",
        ],
    );
    assert!(!nonexistent.status.success());
    assert!(
        !missing.exists(),
        "prepare must not create a missing catalog"
    );

    let contradictory = failure(
        &path,
        [
            "prepare",
            "preflight",
            "--project",
            &project,
            "--asset",
            "x",
            "--all",
        ],
    );
    assert_eq!(contradictory["error"]["code"], "invalid_arguments");
    for args in [
        vec![
            "prepare",
            "preflight",
            "--project",
            &project,
            "--all",
            "--limit",
            "0",
        ],
        vec![
            "prepare",
            "preflight",
            "--project",
            &project,
            "--all",
            "--offset",
            "1000001",
        ],
    ] {
        let result = failure(&path, args);
        assert_eq!(result["error"]["code"], "invalid_input");
    }
    let partial = failure(
        &path,
        [
            "prepare",
            "run",
            "--project",
            &project,
            "--asset",
            "x",
            "--provider",
            "openrouter",
        ],
    );
    assert_eq!(partial["error"]["code"], "invalid_input");
    let without_authorization = failure(
        &path,
        [
            "prepare",
            "run",
            "--project",
            &project,
            "--asset",
            "x",
            "--provider",
            "openrouter",
            "--model",
            "model",
            "--payload",
            "frames",
            "--budget-usd",
            "0.2",
        ],
    );
    assert_eq!(without_authorization["error"]["code"], "invalid_input");
    let missing_policy = failure(
        &path,
        [
            "prepare",
            "run",
            "--project",
            &project,
            "--asset",
            "x",
            "--moments",
            "--provider",
            "openrouter",
            "--model",
            "model",
            "--payload",
            "frames",
        ],
    );
    assert_eq!(missing_policy["error"]["code"], "invalid_input");
}

#[test]
fn preparation_default_run_reuses_a_synthetic_offline_proxy_without_paid_work() {
    let directory = tempdir().unwrap();
    let source_dir = directory.path().join("source");
    fs::create_dir(&source_dir).unwrap();
    let source = source_dir.join("synthetic.mp4");
    let generated = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=320x180:r=24:d=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&source)
        .status();
    let Ok(status) = generated else {
        eprintln!("skipping synthetic CLI proxy test: ffmpeg unavailable");
        return;
    };
    if !status.success() {
        eprintln!("skipping synthetic CLI proxy test: ffmpeg cannot encode H.264");
        return;
    }
    let path = directory.path().join("catalog.sqlite3");
    let catalog = Catalog::open(&path).unwrap();
    let project = catalog
        .create_project("Synthetic offline preparation")
        .unwrap()
        .id;
    let report = catalog.ingest_folder(&project, &source_dir).unwrap();
    assert_eq!(report.imported, 1);
    let asset = catalog.list_assets(&project).unwrap().remove(0);
    catalog.generate_proxy(&asset.id).unwrap();
    drop(catalog);
    let first = success(
        &path,
        [
            "prepare",
            "run",
            "--project",
            &project,
            "--asset",
            &asset.id,
        ],
    );
    assert_eq!(
        first["manifest"]["items"][0]["representation"]["validation"]["state"],
        "ready"
    );
    let second = success(
        &path,
        [
            "prepare",
            "run",
            "--project",
            &project,
            "--asset",
            &asset.id,
        ],
    );
    assert_eq!(
        second["reused"], 1,
        "the current local receipt should be reused without rerunning paid stages"
    );
}
