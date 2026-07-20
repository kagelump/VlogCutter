use serde_json::Value;
use std::process::Command;

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
