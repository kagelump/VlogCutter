//! Real CLI subprocess, HTTP and callback-process contracts. The HTTP fixture
//! controls terminal transitions without requiring media work or paid models.
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};
use tempfile::{TempDir, tempdir};

fn respond(mut stream: TcpStream, body: &Value) -> (String, Value) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request = String::new();
    reader.read_line(&mut request).unwrap();
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).unwrap();
    let payload = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    let status = body
        .get("fixtureHttpStatus")
        .and_then(Value::as_u64)
        .unwrap_or(200);
    let mut response = body.clone();
    if request.contains("/settle") && status == 200 {
        response["state"] = json!(if payload["succeeded"] == true {
            "delivered"
        } else {
            "pending"
        });
    }
    let bytes = serde_json::to_vec(&response).unwrap();
    write!(stream,"HTTP/1.1 {status} Fixture\r\ncontent-type: application/json\r\nconnection: close\r\ncontent-length: {}\r\n\r\n",bytes.len()).unwrap();
    stream.write_all(&bytes).unwrap();
    // An empty URL query is transport-equivalent to no query.
    (request.trim().replace("? HTTP/1.1", " HTTP/1.1"), payload)
}

fn fixture(responses: Vec<Value>) -> (String, thread::JoinHandle<Vec<(String, Value)>>) {
    let listener =
        TcpListener::bind("127.0.0.1:0").expect("loopback HTTP fixture must be permitted");
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        responses
            .into_iter()
            .map(|body| {
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => break respond(stream, &body),
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(error) => panic!("expected CLI request: {error}"),
                    }
                }
            })
            .collect()
    });
    (endpoint, handle)
}

fn version() -> Value {
    json!({"nodeId":"node_wait_fixture","protocolMajorVersion":1,"protocolMinorVersion":0})
}

fn snapshot(state: &str) -> Value {
    json!({
        "definition":{"jobId":"job_wait","jobType":"test.v1","payloadVersion":1,"payload":{},"immutableIdentity":{},"requirements":{"schemaVersion":1,"requiresOriginal":false,"requiresDerivedWrite":false,"capabilities":[]},"targetNodeId":"node_wait_fixture","projectId":null,"mediaObjectId":null,"storageRootId":null,"representationId":null,"recipeKey":"test.v1","jobClass":"lightweight","idempotencyKey":"wait","priority":0,"totalWorkUnits":1,"totalItems":1,"retryPolicy":{"maxAttempts":2,"initialBackoffSeconds":1,"maxBackoffSeconds":2},"dependencies":[]},
        "state":state,"attemptCount":1,"retryable":false,"availableAt":"2026-10-22T00:00:00Z","cancelRequestedAt":null,"leaseActive":false,"leaseExpiresAt":null,"completedWorkUnits":0,"totalWorkUnits":1,"completedItems":0,"failedItems":0,"totalItems":1,"error":null,"outputIdentity":null,"createdAt":"2026-10-22T00:00:00Z","startedAt":null,"updatedAt":"2026-10-22T00:00:00Z","completedAt":null
    })
}

fn cli(root: &TempDir, endpoint: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rcdvlog"));
    command
        .env("RCDVLOG_SERVER_TOKEN", "fixture-secret-not-for-callback")
        .args([
            "--catalog",
            root.path()
                .join("must-not-create.sqlite3")
                .to_str()
                .unwrap(),
            "server",
            "--endpoint",
            endpoint,
            "jobs",
        ]);
    command
}

fn envelope(output: &Output) -> Value {
    let bytes = if output.status.success() {
        &output.stdout
    } else {
        &output.stderr
    };
    assert_eq!(
        String::from_utf8_lossy(bytes).lines().count(),
        1,
        "{output:?}"
    );
    assert!(!String::from_utf8_lossy(bytes).contains("fixture-secret-not-for-callback"));
    serde_json::from_slice(bytes).unwrap()
}

#[test]
fn job_wait_observes_retry_wait_and_returns_one_quiet_result() {
    let root = tempdir().unwrap();
    let (endpoint, server) = fixture(vec![
        version(),
        snapshot("queued"),
        snapshot("running"),
        snapshot("retry_wait"),
        snapshot("succeeded"),
    ]);
    let output = cli(&root, &endpoint)
        .args([
            "wait",
            "--job",
            "job_wait",
            "--wait-seconds",
            "5",
            "--poll-interval-ms",
            "10",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    assert_eq!(envelope(&output)["data"]["result"]["state"], "succeeded");
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 5);
    assert!(
        requests[1..]
            .iter()
            .all(|(path, _)| path == "GET /api/v1/admin/jobs/job_wait HTTP/1.1")
    );
    assert!(!root.path().join("must-not-create.sqlite3").exists());
}

#[test]
fn job_wait_distinguishes_timeout_failed_and_canceled_without_mutation() {
    for (state, code) in [
        ("queued", "job_wait_timeout"),
        ("failed", "job_failed"),
        ("canceled", "job_canceled"),
    ] {
        let root = tempdir().unwrap();
        let (endpoint, server) = fixture(vec![version(), snapshot(state)]);
        let output = cli(&root, &endpoint)
            .args(["wait", "--job", "job_wait", "--wait-seconds", "0"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = envelope(&output);
        assert_eq!(error["error"]["code"], code);
        assert_eq!(error["error"]["details"]["state"], state);
        assert_eq!(server.join().unwrap().len(), 2);
    }
}

fn delivery() -> Value {
    json!({"deliveryId":"delivery_fixture","sequence":1,"subscriptionId":"callback_fixture","target":"editor","event":{"eventSequence":7,"jobId":"job_wait","attemptNumber":1,"eventType":"failed","state":"failed","details":{"code":"media_offline"},"createdAt":"2026-10-22T00:00:00Z"},"state":"leased","attemptCount":1,"availableAt":"2026-10-22T00:00:00Z","leaseExpiresAt":"2026-10-22T00:02:00Z"})
}

#[cfg(unix)]
fn callback_case(script: &str) -> (Output, Value, Value) {
    use std::os::unix::fs::PermissionsExt;
    let root = tempdir().unwrap();
    let program = root.path().join("callback with spaces");
    let payload = root.path().join("callback result.json");
    std::fs::write(&program, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (endpoint, server) = fixture(vec![
        version(),
        json!({"claim":null}),
        json!({"claim":{"delivery":delivery(),"leaseToken":"fixture-lease"}}),
        delivery(),
    ]);
    let output = cli(&root, &endpoint)
        .args([
            "callbacks",
            "deliver",
            "--target",
            "editor",
            "--program",
            program.to_str().unwrap(),
            "--arg",
            payload.to_str().unwrap(),
            "--arg=$(literal; argument)",
            "--wait-seconds",
            "5",
            "--poll-interval-ms",
            "10",
            "--execution-timeout-seconds",
            "1",
        ])
        .output()
        .unwrap();
    let requests = server.join().unwrap();
    assert_eq!(
        requests[3].0,
        "POST /api/v1/admin/job-callbacks/delivery_fixture/settle HTTP/1.1"
    );
    let body = std::fs::read(&payload)
        .ok()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or(Value::Null);
    (output, requests[3].1.clone(), body)
}

#[test]
#[cfg(unix)]
fn callback_receives_json_literal_arguments_and_no_server_credential() {
    let (output, ack, payload) = callback_case(
        "test -z \"$RCDVLOG_SERVER_TOKEN\" || exit 9\ntest \"$2\" = '$(literal; argument)' || exit 8\ncat > \"$1\"\nprintf 'stdout noise'\nprintf 'stderr noise' >&2",
    );
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    assert_eq!(ack, json!({"leaseToken":"fixture-lease","succeeded":true}));
    assert_eq!(payload["schema"], "job.completion.v1");
    assert_eq!(payload["deliveryId"], "delivery_fixture");
    assert_eq!(payload["event"]["state"], "failed");
    assert_eq!(envelope(&output)["data"]["result"]["callbackExitCode"], 0);
}

#[test]
#[cfg(unix)]
fn callback_nonzero_exit_and_timeout_release_delivery_for_retry() {
    for script in ["cat > \"$1\"\nexit 7", "sleep 30"] {
        let start = Instant::now();
        let (output, ack, _) = callback_case(script);
        assert!(!output.status.success());
        assert_eq!(envelope(&output)["error"]["code"], "callback_failed");
        assert_eq!(ack["succeeded"], false);
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}

#[test]
fn callback_timeout_claims_no_delivery_and_invokes_no_program() {
    let root = tempdir().unwrap();
    let (endpoint, server) = fixture(vec![version(), json!({"claim":null})]);
    let output = cli(&root, &endpoint)
        .args([
            "callbacks",
            "deliver",
            "--target",
            "editor",
            "--program",
            std::env::current_exe().unwrap().to_str().unwrap(),
            "--wait-seconds",
            "0",
        ])
        .output()
        .unwrap();
    assert_eq!(envelope(&output)["error"]["code"], "callback_wait_timeout");
    assert_eq!(server.join().unwrap().len(), 2);
}

#[test]
#[cfg(unix)]
fn callback_ack_failure_does_not_execute_the_consumer_twice() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempdir().unwrap();
    let program = root.path().join("accept-completion");
    let receipts = root.path().join("accepted.txt");
    std::fs::write(
        &program,
        "#!/bin/sh\ncat >/dev/null\nprintf 'accepted\\n' >> \"$1\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (endpoint, server) = fixture(vec![
        version(),
        json!({"claim":{"delivery":delivery(),"leaseToken":"fixture-lease"}}),
        json!({"fixtureHttpStatus":503,"code":"internal_error"}),
    ]);
    let output = cli(&root, &endpoint)
        .args([
            "callbacks",
            "deliver",
            "--target",
            "editor",
            "--program",
            program.to_str().unwrap(),
            "--arg",
            receipts.to_str().unwrap(),
            "--wait-seconds",
            "0",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = envelope(&output);
    assert_eq!(error["error"]["code"], "callback_ack_failed");
    assert_eq!(error["error"]["details"]["deliveryId"], "delivery_fixture");
    assert_eq!(error["error"]["details"]["callbackSucceeded"], true);
    assert_eq!(std::fs::read_to_string(receipts).unwrap(), "accepted\n");
    assert_eq!(server.join().unwrap().len(), 3);
}
