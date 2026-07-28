use rcdvlog_server_core::{
    BASELINE_VERSION, BaselineEnvelope, Catalog, PROJECT_CATALOG_SCHEMA_VERSION,
    ProjectAssetAddedPayload, ProtocolRange, ReconcileRequest, ReconcileResponse,
    apply_reconcile_operations,
};
use reqwest::blocking::{Client, Response};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn server_binary() -> &'static str {
    env!("CARGO_BIN_EXE_rcdvlog-server")
}

struct ProcessFixture {
    root: TempDir,
    data: PathBuf,
    derived: PathBuf,
    backup: PathBuf,
    originals: PathBuf,
    endpoint: String,
    child: Option<Child>,
}

impl ProcessFixture {
    fn new() -> Option<Self> {
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return None,
            Err(error) => panic!("reserve loopback port: {error}"),
        };
        let address = listener.local_addr().unwrap();
        drop(listener);
        let root = TempDir::new().unwrap();
        let data = root.path().join("state");
        let derived = root.path().join("derived");
        let backup = root.path().join("backup");
        let originals = root.path().join("originals");
        for path in [&derived, &backup, &originals] {
            fs::create_dir_all(path).unwrap();
        }
        Some(Self {
            root,
            data,
            derived,
            backup,
            originals,
            endpoint: format!("http://{address}"),
            child: None,
        })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(server_binary());
        command
            .env("RCDVLOG_DATA_DIR", &self.data)
            .env("RCDVLOG_DERIVED_DIR", &self.derived)
            .env("RCDVLOG_BACKUP_DIR", &self.backup)
            .env("RCDVLOG_ALLOWED_ORIGINAL_MOUNTS", &self.originals)
            .env(
                "RCDVLOG_LISTEN_ADDR",
                self.endpoint.trim_start_matches("http://"),
            )
            .env("RCDVLOG_MINIMUM_FREE_BYTES", "67108864")
            .env("RCDVLOG_FFMPEG", "/usr/bin/true")
            .env("RCDVLOG_FFPROBE", "/usr/bin/true")
            .env("RCDVLOG_EXIFTOOL", "/usr/bin/true")
            .env("RUST_LOG", "error");
        command
    }

    fn create_bootstrap(&self, output: &Path) -> String {
        let result = self
            .command()
            .args([
                "admin",
                "bootstrap",
                "create",
                "--output-file",
                output.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        fs::read_to_string(output).unwrap().trim().to_string()
    }

    fn start(&mut self, client: &Client) {
        let child = self
            .command()
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        self.child = Some(child);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if client
                .get(format!("{}/api/v1/health", self.endpoint))
                .send()
                .is_ok_and(|response| response.status().is_success())
            {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("server process did not become healthy");
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for ProcessFixture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn json_response(response: Response) -> Value {
    let status = response.status();
    let value: Value = response.json().unwrap();
    assert!(status.is_success(), "HTTP {status}: {value}");
    value
}

fn bearer(
    client: &Client,
    endpoint: &str,
    token: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Value {
    let request = match method {
        "GET" => client.get(format!("{endpoint}{path}")),
        "POST" => client.post(format!("{endpoint}{path}")),
        _ => unreachable!(),
    }
    .bearer_auth(token);
    json_response(match body {
        Some(body) => request.json(&body).send().unwrap(),
        None => request.send().unwrap(),
    })
}

#[test]
fn two_node_process_reconciles_duplicate_pages_browser_edits_and_restart() {
    let Some(mut fixture) = ProcessFixture::new() else {
        return;
    };
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let bootstrap = fixture.create_bootstrap(&fixture.root.path().join("bootstrap.secret"));
    fixture.start(&client);

    let bootstrap_response = client
        .post(format!(
            "{}/api/v1/auth/bootstrap/exchange",
            fixture.endpoint
        ))
        .json(&json!({"code":bootstrap,"hostname":"process.test"}))
        .send()
        .unwrap();
    assert!(bootstrap_response.status().is_success());
    let cookie = bootstrap_response
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let csrf = bootstrap_response
        .headers()
        .get("x-csrf-token")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let pairing = json_response(
        client
            .post(format!("{}/api/v1/auth/pairing", fixture.endpoint))
            .header(reqwest::header::COOKIE, cookie)
            .header("x-csrf-token", csrf)
            .json(&json!({}))
            .send()
            .unwrap(),
    );
    let device = json_response(
        client
            .post(format!("{}/api/v1/auth/devices/exchange", fixture.endpoint))
            .json(&json!({"code":pairing["code"],"label":"Process Mac"}))
            .send()
            .unwrap(),
    );
    let token = device["token"].as_str().unwrap();
    let version = json_response(
        client
            .get(format!("{}/api/v1/version", fixture.endpoint))
            .send()
            .unwrap(),
    );
    let server_node_id = version["nodeId"].as_str().unwrap().to_string();

    let mac_path = fixture.root.path().join("mac-catalog.sqlite3");
    let mac = Catalog::open(&mac_path).unwrap();
    mac.set_local_node_id("node_process_mac").unwrap();
    let project = mac.create_project("Process trip").unwrap();
    mac.add_portable_project_asset(
        &project.id,
        ProjectAssetAddedPayload {
            id: "asset_process".into(),
            fingerprint: "fingerprint-process".into(),
            filename: "process.mov".into(),
            size_bytes: 10,
            duration_seconds: 1.0,
            frame_rate: 30.0,
            width: 1920,
            height: 1080,
            codec: "h264".into(),
            camera: "camera".into(),
            captured_at_raw: None,
            best_time: None,
            metadata_rotation_degrees: 0,
            default_rotation_degrees: 0,
            created_at: "2026-07-28T00:00:00Z".into(),
        },
    )
    .unwrap();
    let baseline_sequence = mac
        .list_operations(&project.id)
        .unwrap()
        .iter()
        .filter(|operation| operation.origin_node_id == "node_process_mac")
        .map(|operation| operation.origin_sequence)
        .max()
        .unwrap_or(0);
    let baseline = BaselineEnvelope::export(&mac, &project.id, vec![], vec![]).unwrap();
    bearer(
        &client,
        &fixture.endpoint,
        token,
        "POST",
        "/api/v1/internal/projects/baseline/import",
        Some(serde_json::to_value(&baseline).unwrap()),
    );

    mac.add_label("asset_process", "Mac label").unwrap();
    mac.create_note_revision("asset_process", "Mac note")
        .unwrap();
    let local_operations = mac
        .list_operations(&project.id)
        .unwrap()
        .into_iter()
        .filter(|operation| {
            operation.origin_node_id == "node_process_mac"
                && operation.origin_sequence > baseline_sequence
        })
        .collect::<Vec<_>>();
    let request = ReconcileRequest {
        protocol_version: ProtocolRange::default(),
        project_schema_version: PROJECT_CATALOG_SCHEMA_VERSION,
        project_id: project.id.clone(),
        server_node_id: server_node_id.clone(),
        operation_cursors: BTreeMap::new(),
        operations: local_operations,
        observations: vec![],
        representation_manifests: vec![],
        maximum_response_operations: 500,
    };
    let first: ReconcileResponse = serde_json::from_value(bearer(
        &client,
        &fixture.endpoint,
        token,
        "POST",
        &format!("/api/v1/internal/projects/{}/reconcile", project.id),
        Some(serde_json::to_value(&request).unwrap()),
    ))
    .unwrap();
    assert_eq!(
        first.committed_cursors["node_process_mac"],
        baseline_sequence + 2
    );
    let duplicate: ReconcileResponse = serde_json::from_value(bearer(
        &client,
        &fixture.endpoint,
        token,
        "POST",
        &format!("/api/v1/internal/projects/{}/reconcile", project.id),
        Some(serde_json::to_value(&request).unwrap()),
    ))
    .unwrap();
    assert_eq!(duplicate.committed_cursors, first.committed_cursors);

    let review = bearer(
        &client,
        &fixture.endpoint,
        token,
        "GET",
        &format!(
            "/api/v1/projects/{}/assets/asset_process/review",
            project.id
        ),
        None,
    );
    let revision = review["materializedRevision"].as_u64().unwrap();
    bearer(
        &client,
        &fixture.endpoint,
        token,
        "POST",
        &format!(
            "/api/v1/projects/{}/assets/asset_process/removed",
            project.id
        ),
        Some(json!({"removed":true,"baseRevision":revision})),
    );

    let receive = ReconcileRequest {
        operations: vec![],
        operation_cursors: mac.operation_cursors().unwrap(),
        ..request.clone()
    };
    let response: ReconcileResponse = serde_json::from_value(bearer(
        &client,
        &fixture.endpoint,
        token,
        "POST",
        &format!("/api/v1/internal/projects/{}/reconcile", project.id),
        Some(serde_json::to_value(&receive).unwrap()),
    ))
    .unwrap();
    assert!(!response.operations.is_empty());
    apply_reconcile_operations(&mac, &response.operations).unwrap();
    let converged = mac.get_asset("asset_process").unwrap();
    assert!(converged.removed);
    assert_eq!(converged.notes, "Mac note");
    assert_eq!(converged.labels, vec!["Mac label"]);

    fixture.stop();
    fixture.start(&client);
    let after_restart = ReconcileRequest {
        operations: request.operations.clone(),
        operation_cursors: mac.operation_cursors().unwrap(),
        ..request
    };
    let restarted: ReconcileResponse = serde_json::from_value(bearer(
        &client,
        &fixture.endpoint,
        token,
        "POST",
        &format!("/api/v1/internal/projects/{}/reconcile", project.id),
        Some(serde_json::to_value(&after_restart).unwrap()),
    ))
    .unwrap();
    assert!(restarted.operations.is_empty());
    assert_eq!(
        restarted.committed_cursors["node_process_mac"],
        baseline_sequence + 2
    );
    assert_eq!(BASELINE_VERSION, 1);
}
