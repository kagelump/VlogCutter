use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use tempfile::tempdir;

fn cli_binary() -> &'static str {
    env!("CARGO_BIN_EXE_rcdvlog")
}

fn serve_json(mut stream: TcpStream, body: Value) -> Vec<String> {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end().to_string();
        if line.is_empty() {
            break;
        }
        lines.push(line);
    }
    let body = serde_json::to_vec(&body).unwrap();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
    lines
}

fn write_private_credential(path: &Path, token: &str) {
    fs::write(path, token).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn loopback_listener() -> Option<TcpListener> {
    match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => Some(listener),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => None,
        Err(error) => panic!("bind loopback fixture: {error}"),
    }
}

#[test]
fn remote_status_is_one_json_envelope_and_authenticates_without_leaking_secret() {
    let Some(listener) = loopback_listener() else {
        return;
    };
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (sender, receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let version_headers = serve_json(
            listener.accept().unwrap().0,
            serde_json::json!({
                "applicationVersion":"0.1.0",
                "protocolMajorVersion":1,
                "protocolMinorVersion":0,
                "controlSchemaVersion":8,
                "nodeId":"node_fixture"
            }),
        );
        let status_headers = serve_json(
            listener.accept().unwrap().0,
            serde_json::json!({"readiness":{"status":"ready"},"nodeId":"node_fixture"}),
        );
        sender.send((version_headers, status_headers)).unwrap();
    });

    let root = tempdir().unwrap();
    let credential = root.path().join("credential");
    let catalog = root.path().join("catalog.sqlite3");
    let secret = "device-secret-fixture";
    write_private_credential(&credential, secret);
    let output = Command::new(cli_binary())
        .arg("--catalog")
        .arg(&catalog)
        .args([
            "server",
            "--endpoint",
            &endpoint,
            "--credential-file",
            credential.to_str().unwrap(),
            "status",
        ])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(!stdout.contains(secret));
    assert_eq!(stdout.lines().count(), 1);
    let value: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["command"], "server.status");
    assert_eq!(
        value["data"]["request"]["credentialSource"],
        "owner_only_file"
    );

    let (version_headers, status_headers) = receiver.recv().unwrap();
    assert!(
        !version_headers
            .iter()
            .any(|header| header.to_ascii_lowercase().starts_with("authorization:"))
    );
    assert!(
        status_headers
            .iter()
            .any(|header| header.eq_ignore_ascii_case(&format!("authorization: Bearer {secret}")))
    );
    assert!(status_headers.iter().any(|header| {
        header
            .to_ascii_lowercase()
            .starts_with("x-request-id: req_cli_")
    }));
}

#[test]
fn remote_error_preserves_server_request_id_and_keeps_stdout_empty() {
    let Some(listener) = loopback_listener() else {
        return;
    };
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        serve_json(
            listener.accept().unwrap().0,
            serde_json::json!({"nodeId":"node_fixture"}),
        );
        let mut stream = listener.accept().unwrap().0;
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
        let body = serde_json::to_vec(&serde_json::json!({
            "code":"unauthorized",
            "requestId":"req_server_fixture"
        }))
        .unwrap();
        write!(
            stream,
            "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
    });
    let root = tempdir().unwrap();
    let credential = root.path().join("credential");
    write_private_credential(&credential, "secret-never-output");
    let output = Command::new(cli_binary())
        .args([
            "server",
            "--endpoint",
            &endpoint,
            "--credential-file",
            credential.to_str().unwrap(),
            "projects",
        ])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("secret-never-output"));
    assert_eq!(stderr.lines().count(), 1);
    let value: Value = serde_json::from_str(&stderr).unwrap();
    assert_eq!(value["error"]["code"], "unauthorized");
    assert_eq!(value["error"]["requestId"], "req_server_fixture");
}
