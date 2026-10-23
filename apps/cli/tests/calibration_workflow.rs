use rcdvlog_core::Catalog;
use serde_json::Value;
use std::env;
use std::ffi::OsStr;
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
        .env(
            "RCDVLOG_RESOURCE_LOCK_ROOT",
            catalog.with_extension("locks"),
        )
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

#[test]
fn calibrate_speech_without_models_reports_not_ready_and_adopts_nothing() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite3");
    let _catalog = Catalog::open(&path).unwrap();
    let before = std::fs::read(&path).unwrap();
    let capabilities = success(&path, ["calibrate", "capabilities"]);
    assert_eq!(
        capabilities["schemaVersion"],
        rcdvlog_core::RESOURCE_CALIBRATION_SCHEMA_VERSION
    );
    assert_eq!(capabilities["localHeavy"]["slots"], 1);
    assert_eq!(
        capabilities["representativeMedia"]["maxDurationSeconds"],
        rcdvlog_core::MAX_REPRESENTATIVE_MEDIA_DURATION_MS / 1000
    );
    let status = success(&path, ["calibrate", "status", "--recipe", "speech"]);
    assert_eq!(status["entries"].as_array().unwrap().len(), 0);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "read-only calibration status changed the catalog"
    );

    let report = success(
        &path,
        [
            "calibrate",
            "run",
            "--recipe",
            "speech",
            "--generated-duration-seconds",
            "1",
            "--trials",
            "1",
        ],
    );
    assert_eq!(report["state"], "not_ready");
    assert!(report["selectedSettings"].is_null());
    assert!(
        report["error"]
            .as_str()
            .unwrap()
            .contains("speech models are not ready")
    );
    let after = success(&path, ["calibrate", "status", "--recipe", "speech"]);
    assert_eq!(after["entries"][0]["state"], "not_ready");
    assert_eq!(after["entries"][0]["reusable"], false);
}

#[test]
fn calibrate_proxy_runs_a_bounded_generated_media_benchmark_and_reuses_it() {
    if Command::new("ffmpeg").arg("-version").output().is_err()
        || Command::new("ffprobe").arg("-version").output().is_err()
    {
        eprintln!("skipping generated proxy calibration: ffmpeg/ffprobe unavailable");
        return;
    }
    let directory = tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite3");
    let _catalog = Catalog::open(&path).unwrap();
    let report = success(
        &path,
        [
            "calibrate",
            "run",
            "--recipe",
            "proxy",
            "--policy",
            "software",
            "--worker-counts",
            "1",
            "--generated-duration-seconds",
            "1",
            "--trials",
            "1",
        ],
    );
    assert_eq!(report["state"], "complete");
    assert_eq!(report["selectedSettings"]["workerCount"], 1);
    assert_eq!(report["selectedSettings"]["encoder"], "software");
    assert_eq!(report["candidates"][0]["succeeded"], true);
    assert!(
        report["candidates"][0]["timing"]["samplesMs"][0]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(report["identity"]["tools"].as_array().unwrap().len() >= 2);
    assert!(
        !report["identity"]["media"]["sampleIdentity"]
            .as_str()
            .unwrap()
            .contains('/')
    );

    let cached = success(
        &path,
        [
            "calibrate",
            "run",
            "--recipe",
            "proxy",
            "--policy",
            "software",
            "--worker-counts",
            "1",
            "--generated-duration-seconds",
            "1",
            "--trials",
            "1",
        ],
    );
    assert_eq!(report["cacheKey"], cached["cacheKey"]);
    assert_eq!(
        report["candidates"], cached["candidates"],
        "an exact completed calibration must be returned without rerunning the benchmark"
    );

    let status = success(&path, ["calibrate", "status", "--recipe", "proxy"]);
    assert_eq!(status["entries"][0]["state"], "complete");
    assert_eq!(status["entries"][0]["reusable"], true);
    assert_eq!(
        status["entries"][0]["selectedSettings"]["encoder"],
        "software"
    );
}
