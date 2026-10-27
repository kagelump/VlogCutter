//! Process-level JSON and observational contracts; no Google account/network needed.
use serde_json::Value;
use std::process::Command;
use tempfile::tempdir;

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
}

#[test]
fn auth_status_needs_no_catalog_and_does_not_create_credential_directories() {
    let directory = tempdir().unwrap();
    let file = directory.path().join("absent/credentials.json");
    let catalog = directory.path().join("absent-catalog.sqlite3");
    let output = binary()
        .arg("--catalog")
        .arg(&catalog)
        .args(["youtube", "--credential-file"])
        .arg(&file)
        .args(["auth", "status"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["command"], "youtube.auth.status");
    assert_eq!(result["data"]["authorized"], false);
    assert!(!directory.path().join("absent").exists());
    assert!(!catalog.exists());
}

#[test]
fn upload_and_history_require_explicit_catalog_and_project() {
    let directory = tempdir().unwrap();
    let output = binary()
        .env_remove("RCDVLOG_CATALOG")
        .args(["youtube", "history", "--project", "test"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let result: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(result["error"]["code"], "invalid_input");
    let output = binary()
        .arg("--catalog")
        .arg(directory.path().join("catalog.sqlite3"))
        .args([
            "youtube",
            "upload",
            "--file",
            "render.mp4",
            "--title",
            "Cut",
        ])
        .output()
        .unwrap();
    let result: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(result["error"]["code"], "invalid_arguments");
    assert!(!directory.path().join("catalog.sqlite3").exists());
}

#[test]
fn headless_login_emits_authorization_event_and_actionable_timeout_without_tokens() {
    let directory = tempdir().unwrap();
    let client = directory.path().join("client.json");
    std::fs::write(&client,r#"{"installed":{"client_id":"fixture-client","client_secret":"fixture-client-secret","token_uri":"https://attacker.invalid"}}"#).unwrap();
    let output = binary()
        .args(["youtube", "--credential-file"])
        .arg(directory.path().join("credentials.json"))
        .args(["auth", "login", "--client-file"])
        .arg(client)
        .args(["--no-browser", "--timeout-seconds", "1"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let lines: Vec<Value> = String::from_utf8(output.stderr)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["event"], "youtube.oauth.authorization_required");
    assert!(
        lines[0]["authorizationUrl"]
            .as_str()
            .unwrap()
            .starts_with("https://accounts.google.com/o/oauth2/v2/auth?")
    );
    assert!(lines[0]["port"].as_u64().unwrap() > 0);
    assert_eq!(lines[1]["error"]["code"], "youtube.auth_timeout");
    assert!(
        !serde_json::to_string(&lines)
            .unwrap()
            .contains("fixture-client-secret")
    );
    assert!(!directory.path().join("credentials.json").exists());
}

#[test]
fn login_rejects_web_clients_and_never_exposes_invalid_json_contents() {
    let directory = tempdir().unwrap();
    let client = directory.path().join("client.json");
    for contents in [
        r#"{"web":{"client_id":"fixture-client"}}"#,
        r#"{"secret":"fixture-secret" malformed"#,
    ] {
        std::fs::write(&client, contents).unwrap();
        let output = binary()
            .args(["youtube", "--credential-file"])
            .arg(directory.path().join("credentials.json"))
            .args(["auth", "login", "--client-file"])
            .arg(&client)
            .args(["--no-browser"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("fixture-secret"));
        let result: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(result["ok"], false);
    }
}

#[test]
fn history_uses_real_catalog_and_does_not_require_authorization() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let create = binary()
        .arg("--catalog")
        .arg(&catalog)
        .args(["project", "create", "--name", "Upload fixture"])
        .output()
        .unwrap();
    assert!(
        create.status.success(),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );
    let project: Value = serde_json::from_slice(&create.stdout).unwrap();
    let id = project["data"]["id"].as_str().unwrap();
    let output = binary()
        .arg("--catalog")
        .arg(&catalog)
        .args(["youtube", "--credential-file"])
        .arg(directory.path().join("absent/credentials.json"))
        .args(["history", "--project", id])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["data"]["uploads"], serde_json::json!([]));
    assert!(!directory.path().join("absent").exists());
}

#[test]
fn missing_upload_credential_reports_typed_auth_error_without_network() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let create = binary()
        .arg("--catalog")
        .arg(&catalog)
        .args(["project", "create", "--name", "Upload fixture"])
        .output()
        .unwrap();
    let project: Value = serde_json::from_slice(&create.stdout).unwrap();
    let id = project["data"]["id"].as_str().unwrap();
    let output = binary()
        .arg("--catalog")
        .arg(&catalog)
        .args(["youtube", "--credential-file"])
        .arg(directory.path().join("credentials.json"))
        .args([
            "upload",
            "--project",
            id,
            "--file",
            "missing.mp4",
            "--title",
            "Cut",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let result: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(result["error"]["code"], "youtube.auth_required");
}
