use serde_json::Value;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

fn server_binary() -> &'static str {
    env!("CARGO_BIN_EXE_rcdvlog-server")
}

#[test]
fn binary_reports_all_versions_as_machine_readable_json() {
    let output = Command::new(server_binary())
        .arg("--version-json")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["applicationVersion"].is_string());
    assert_eq!(value["protocolMajorVersion"], 1);
    assert_eq!(value["protocolMinorVersion"], 0);
    assert_eq!(
        value["controlSchemaVersion"],
        rcdvlog_server_core::CONTROL_CATALOG_SCHEMA_VERSION
    );
}

#[test]
fn invalid_configuration_exits_with_structured_redacted_log() {
    let secret_like_value = "should-never-appear-in-the-error";
    let output = Command::new(server_binary())
        .env("RCDVLOG_LISTEN_ADDR", "0.0.0.0:8090")
        .env("RCDVLOG_FFMPEG", secret_like_value)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    let line = stderr.lines().last().unwrap();
    let value: Value = serde_json::from_str(line).unwrap();
    assert!(value["timestamp"].is_string());
    assert_eq!(value["level"], "ERROR");
    assert_eq!(value["component"], "runtime");
    assert_eq!(value["code"], "invalid_configuration");
    assert_eq!(value["errorField"], "listenAddr");
    assert!(!stderr.contains("0.0.0.0"));
    assert!(!stderr.contains(secret_like_value));
}

fn configured_command(data_dir: &std::path::Path) -> Command {
    let mut command = Command::new(server_binary());
    command
        .env("RCDVLOG_DATA_DIR", data_dir)
        .env("RCDVLOG_MINIMUM_FREE_BYTES", "67108864")
        .env("RCDVLOG_FFMPEG", "/usr/bin/true")
        .env("RCDVLOG_FFPROBE", "/usr/bin/true")
        .env("RCDVLOG_EXIFTOOL", "/usr/bin/true");
    command
}

#[test]
fn admin_bootstrap_writes_secret_only_to_a_new_owner_only_file() {
    let root = tempdir().unwrap();
    let data_dir = root.path().join("data");
    let output_file = root.path().join("bootstrap.secret");
    let output = configured_command(&data_dir)
        .args([
            "admin",
            "bootstrap",
            "create",
            "--output-file",
            output_file.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let secret = fs::read_to_string(&output_file).unwrap();
    assert!(!secret.trim().is_empty());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["result"]["credential"], "owner_only_file");
    assert!(!String::from_utf8_lossy(&output.stdout).contains(secret.trim()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(output_file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn exclusive_admin_command_refuses_a_live_server_data_directory() {
    let root = tempdir().unwrap();
    let data_dir = root.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    let _lock = rcdvlog_server_core::ServerInstanceLock::acquire(&data_dir).unwrap();
    let output = configured_command(&data_dir)
        .args([
            "admin",
            "diagnostics",
            "bundle",
            "--output-directory",
            root.path().join("diagnostics").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let lines = String::from_utf8(output.stderr).unwrap();
    assert_eq!(lines.lines().count(), 1, "{lines}");
    let value: Value = serde_json::from_str(&lines).unwrap();
    assert_eq!(value["error"]["code"], "server_already_running");
    assert_eq!(value["error"]["retryable"], true);
}

#[test]
fn static_backup_inspection_does_not_touch_the_live_data_directory() {
    let root = tempdir().unwrap();
    let data_dir = root.path().join("live-data");
    let backup_dir = root.path().join("backups");
    fs::create_dir_all(&data_dir).unwrap();
    fs::create_dir_all(&backup_dir).unwrap();
    let sentinel = data_dir.join("sentinel");
    fs::write(&sentinel, b"immutable").unwrap();
    let before = blake3::hash(&fs::read(&sentinel).unwrap());
    let output = configured_command(&data_dir)
        .env("RCDVLOG_BACKUP_DIR", &backup_dir)
        .args([
            "admin",
            "backup",
            "inspect",
            "--backup",
            backup_dir.join("missing").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["result"]["valid"], false);
    assert_eq!(before, blake3::hash(&fs::read(&sentinel).unwrap()));
    assert!(!data_dir.join("control.sqlite3").exists());
}
