use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};
use rcdvlog_core::Catalog;
use rcdvlog_sync::{
    DEFAULT_MAX_OPERATIONS, ProtocolRange, ReconcileRequest, ReconcileResponse,
    apply_reconcile_operations, exact_sequence_recovery_page,
};
use reqwest::Method;
use serde_json::{Value, json};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

const TOKEN_ENVIRONMENT_VARIABLE: &str = "RCDVLOG_SERVER_TOKEN";
const KEYCHAIN_SERVICE: &str = "com.rcd.vlogcutter.server";

mod job_agents;
pub use job_agents::JobAgentError;

#[derive(Debug, Args)]
pub struct ServerConnectionArgs {
    /// Server origin, for example https://rcdvlog.example.ts.net.
    #[arg(long, env = "RCDVLOG_SERVER_ENDPOINT")]
    pub endpoint: String,
    /// Owner-only file containing one paired device token.
    #[arg(long, env = "RCDVLOG_SERVER_CREDENTIAL_FILE")]
    pub credential_file: Option<PathBuf>,
    /// Bounded HTTP timeout in seconds.
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u64).range(1..=120))]
    pub timeout_seconds: u64,
}

#[derive(Debug, Subcommand)]
pub enum ServerCommand {
    /// Report server versions, node identity, readiness, storage capacity, and auth counts.
    Status,
    /// List bounded registered project replicas.
    Projects {
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
        limit: u32,
        #[arg(long)]
        after: Option<String>,
    },
    Library {
        #[command(subcommand)]
        command: LibraryCommand,
    },
    Storage {
        #[command(subcommand)]
        command: StorageCommand,
    },
    Scans {
        #[command(subcommand)]
        command: ScanCommand,
    },
    Jobs {
        #[command(subcommand)]
        command: JobCommand,
    },
    Sync {
        #[command(subcommand)]
        command: SyncCommand,
    },
    Representations {
        #[command(subcommand)]
        command: RepresentationCommand,
    },
    Credentials {
        #[command(subcommand)]
        command: CredentialCommand,
    },
    Backups {
        #[command(subcommand)]
        command: BackupCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum LibraryCommand {
    List {
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
        limit: u32,
        #[arg(long)]
        after: Option<String>,
        #[arg(long)]
        filename_prefix: Option<String>,
        #[arg(long)]
        camera: Option<String>,
    },
    Diagnose {
        #[arg(long)]
        media_object: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum StorageCommand {
    List,
    Diagnose {
        #[arg(long)]
        root: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ScanCommand {
    List {
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
        limit: u32,
        #[arg(long)]
        after: Option<String>,
    },
    Diagnose {
        #[arg(long)]
        scan: String,
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
        limit: u32,
    },
    Request {
        #[arg(long)]
        root: String,
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        idempotency_key: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum JobCommand {
    /// Quietly wait for a durable job to succeed, fail, or be canceled.
    Wait(job_agents::WaitArgs),
    /// Durable completion subscriptions and external callback delivery.
    Callbacks {
        #[command(subcommand)]
        command: job_agents::CallbackCommand,
    },
    List {
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
        limit: u32,
        #[arg(long)]
        before: Option<u64>,
        #[arg(long)]
        state: Option<String>,
    },
    Diagnose {
        #[arg(long)]
        job: String,
    },
    Retry {
        #[arg(long)]
        job: String,
    },
    Cancel {
        #[arg(long)]
        job: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum SyncCommand {
    Status,
    Diagnose {
        #[arg(long)]
        project: String,
    },
    /// Signal the desktop sync loop, or run one exclusive bounded reconciliation.
    Now {
        #[arg(long)]
        project: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum RepresentationCommand {
    Status {
        #[arg(long)]
        representation: String,
    },
    Request {
        #[arg(long)]
        media_object: String,
        #[arg(long, value_parser = ["proxy", "thumbnail"])]
        kind: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum CredentialCommand {
    List,
    Revoke {
        #[arg(long)]
        credential: String,
    },
    /// Create a pairing code and write it only to a new owner-only file.
    PairingCreate {
        #[arg(long)]
        output_file: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum BackupCommand {
    List,
    Diagnose {
        #[arg(long)]
        backup: String,
    },
    Create {
        #[arg(long)]
        idempotency_key: String,
    },
}

#[derive(Debug, thiserror::Error)]
#[error("remote server request failed ({code})")]
pub struct RemoteCommandError {
    pub code: String,
    pub request_id: Option<String>,
}

pub fn command_name(command: &ServerCommand) -> &'static str {
    match command {
        ServerCommand::Status => "server.status",
        ServerCommand::Projects { .. } => "server.projects",
        ServerCommand::Library { command } => match command {
            LibraryCommand::List { .. } => "server.library.list",
            LibraryCommand::Diagnose { .. } => "server.library.diagnose",
        },
        ServerCommand::Storage { command } => match command {
            StorageCommand::List => "server.storage.list",
            StorageCommand::Diagnose { .. } => "server.storage.diagnose",
        },
        ServerCommand::Scans { command } => match command {
            ScanCommand::List { .. } => "server.scans.list",
            ScanCommand::Diagnose { .. } => "server.scans.diagnose",
            ScanCommand::Request { .. } => "server.scans.request",
        },
        ServerCommand::Jobs { command } => match command {
            JobCommand::Wait(_) => "server.jobs.wait",
            JobCommand::Callbacks { command } => match command {
                job_agents::CallbackCommand::Register { .. } => "server.jobs.callbacks.register",
                job_agents::CallbackCommand::List { .. } => "server.jobs.callbacks.list",
                job_agents::CallbackCommand::Deliver { .. } => "server.jobs.callbacks.deliver",
            },
            JobCommand::List { .. } => "server.jobs.list",
            JobCommand::Diagnose { .. } => "server.jobs.diagnose",
            JobCommand::Retry { .. } => "server.jobs.retry",
            JobCommand::Cancel { .. } => "server.jobs.cancel",
        },
        ServerCommand::Sync { command } => match command {
            SyncCommand::Status => "server.sync.status",
            SyncCommand::Diagnose { .. } => "server.sync.diagnose",
            SyncCommand::Now { .. } => "server.sync.now",
        },
        ServerCommand::Representations { command } => match command {
            RepresentationCommand::Status { .. } => "server.representations.status",
            RepresentationCommand::Request { .. } => "server.representations.request",
        },
        ServerCommand::Credentials { command } => match command {
            CredentialCommand::List => "server.credentials.list",
            CredentialCommand::Revoke { .. } => "server.credentials.revoke",
            CredentialCommand::PairingCreate { .. } => "server.credentials.pairing-create",
        },
        ServerCommand::Backups { command } => match command {
            BackupCommand::List => "server.backups.list",
            BackupCommand::Diagnose { .. } => "server.backups.diagnose",
            BackupCommand::Create { .. } => "server.backups.create",
        },
    }
}

pub fn execute(
    connection: &ServerConnectionArgs,
    command: &ServerCommand,
    catalog_path: &Path,
) -> Result<Value> {
    if let ServerCommand::Sync {
        command: SyncCommand::Now { project },
    } = command
    {
        return sync_now(connection, catalog_path, project);
    }
    let client = RemoteClient::new(connection)?;
    let result = match command {
        ServerCommand::Status => client.get("/api/v1/admin/overview", &[])?,
        ServerCommand::Projects { limit, after } => client.get(
            "/api/v1/projects",
            &query([("limit", Some(limit.to_string())), ("after", after.clone())]),
        )?,
        ServerCommand::Library { command } => match command {
            LibraryCommand::List {
                limit,
                after,
                filename_prefix,
                camera,
            } => client.get(
                "/api/v1/library/media",
                &query([
                    ("limit", Some(limit.to_string())),
                    ("after", after.clone()),
                    ("filenamePrefix", filename_prefix.clone()),
                    ("camera", camera.clone()),
                ]),
            )?,
            LibraryCommand::Diagnose { media_object } => client.get(
                &format!("/api/v1/library/media/{media_object}"),
                &[],
            )?,
        },
        ServerCommand::Storage { command } => {
            let roots = client.get("/api/v1/admin/storage-roots", &[])?;
            let bindings = client.get("/api/v1/admin/storage-bindings", &[])?;
            match command {
                StorageCommand::List => json!({"roots":roots,"bindings":bindings}),
                StorageCommand::Diagnose { root } => json!({
                    "storedCanonicalState": roots["items"].as_array().and_then(|items| items.iter().find(|item| item["storageRootId"].as_str() == Some(root))),
                    "liveBindingObservation": bindings["items"].as_array().and_then(|items| items.iter().find(|item| item["storageRootId"].as_str() == Some(root))),
                    "warnings": [],
                    "recommendedExplicitMutations": ["server scans request --root <id> --idempotency-key <key>"],
                }),
            }
        }
        ServerCommand::Scans { command } => match command {
            ScanCommand::List { limit, after } => client.get(
                "/api/v1/admin/scans",
                &query([("limit", Some(limit.to_string())), ("after", after.clone())]),
            )?,
            ScanCommand::Diagnose { scan, limit } => client.get(
                &format!("/api/v1/admin/scans/{scan}/diagnostics"),
                &[("limit".into(), limit.to_string())],
            )?,
            ScanCommand::Request {
                root,
                scope,
                idempotency_key,
            } => client.post(
                "/api/v1/admin/scans",
                &json!({"storageRootId":root,"scopeRelativePath":scope,"idempotencyKey":idempotency_key}),
            )?,
        },
        ServerCommand::Jobs { command } => match command {
            JobCommand::Wait(args) => job_agents::wait(&client, args)?,
            JobCommand::Callbacks { command } => job_agents::callback(&client, command)?,
            JobCommand::List {
                limit,
                before,
                state,
            } => client.get(
                "/api/v1/admin/jobs",
                &query([
                    ("limit", Some(limit.to_string())),
                    ("before", before.map(|value| value.to_string())),
                    ("state", state.clone()),
                ]),
            )?,
            JobCommand::Diagnose { job } => {
                client.get(&format!("/api/v1/admin/jobs/{job}"), &[])?
            }
            JobCommand::Retry { job } => {
                client.post(&format!("/api/v1/admin/jobs/{job}/retry"), &json!({}))?
            }
            JobCommand::Cancel { job } => {
                client.post(&format!("/api/v1/admin/jobs/{job}/cancel"), &json!({}))?
            }
        },
        ServerCommand::Sync { command } => {
            let value = client.get("/api/v1/admin/sync", &[])?;
            match command {
                SyncCommand::Status => value,
                SyncCommand::Diagnose { project } => json!({
                    "endpoint": client.endpoint,
                    "storedCanonicalState": value["items"].as_array().and_then(|items| items.iter().find(|item| item["projectId"].as_str() == Some(project))),
                    "protocolIdentity": client.version,
                }),
                SyncCommand::Now { .. } => unreachable!(),
            }
        }
        ServerCommand::Representations { command } => match command {
            RepresentationCommand::Status { representation } => client.get(
                &format!("/api/v1/representations/{representation}"),
                &[],
            )?,
            RepresentationCommand::Request { media_object, kind } => client.post(
                "/api/v1/representations",
                &json!({"mediaObjectId":media_object,"kind":kind}),
            )?,
        },
        ServerCommand::Credentials { command } => match command {
            CredentialCommand::List => client.get("/api/v1/auth/devices", &[])?,
            CredentialCommand::Revoke { credential } => {
                client.delete(&format!("/api/v1/auth/devices/{credential}"))?
            }
            CredentialCommand::PairingCreate { output_file } => {
                let mut value = client.post("/api/v1/auth/pairing", &json!({}))?;
                let secret_value = value
                    .get_mut("code")
                    .map(Value::take)
                    .ok_or_else(|| anyhow!("pairing response omitted code"))?;
                let secret = secret_value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| anyhow!("pairing response omitted code"))?;
                write_secret_file(output_file, &secret)?;
                json!({
                    "created": true,
                    "expiresAt": value["expiresAt"],
                    "credentialOutput": "owner_only_file",
                })
            }
        },
        ServerCommand::Backups { command } => match command {
            BackupCommand::List => client.get("/api/v1/admin/backups", &[])?,
            BackupCommand::Diagnose { backup } => {
                client.get(&format!("/api/v1/admin/backups/{backup}"), &[])?
            }
            BackupCommand::Create { idempotency_key } => client.post(
                "/api/v1/admin/backups",
                &json!({"idempotencyKey":idempotency_key}),
            )?,
        },
    };
    Ok(json!({
        "request": {"endpoint":client.endpoint,"credentialSource":client.credential_source},
        "protocol": client.version,
        "result": result,
    }))
}

fn query<const N: usize>(values: [(&str, Option<String>); N]) -> Vec<(String, String)> {
    values
        .into_iter()
        .filter_map(|(key, value)| value.map(|value| (key.to_string(), value)))
        .collect()
}

#[derive(Debug)]
struct RemoteClient {
    http: reqwest::blocking::Client,
    endpoint: String,
    token: String,
    credential_source: &'static str,
    version: Value,
}

impl RemoteClient {
    fn new(arguments: &ServerConnectionArgs) -> Result<Self> {
        let endpoint = normalize_endpoint(&arguments.endpoint)?;
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(arguments.timeout_seconds))
            .build()?;
        let version = send_request(
            &http,
            &endpoint,
            None,
            Method::GET,
            "/api/v1/version",
            &[],
            None,
        )?;
        let node_id = version["nodeId"]
            .as_str()
            .ok_or_else(|| anyhow!("server version response omitted nodeId"))?;
        let (token, credential_source) = load_credential(arguments, node_id)?;
        Ok(Self {
            http,
            endpoint,
            token,
            credential_source,
            version,
        })
    }

    fn get(&self, path: &str, query: &[(String, String)]) -> Result<Value> {
        send_request(
            &self.http,
            &self.endpoint,
            Some(&self.token),
            Method::GET,
            path,
            query,
            None,
        )
    }

    fn post(&self, path: &str, body: &Value) -> Result<Value> {
        send_request(
            &self.http,
            &self.endpoint,
            Some(&self.token),
            Method::POST,
            path,
            &[],
            Some(body),
        )
    }

    fn delete(&self, path: &str) -> Result<Value> {
        send_request(
            &self.http,
            &self.endpoint,
            Some(&self.token),
            Method::DELETE,
            path,
            &[],
            None,
        )
    }
}

fn normalize_endpoint(value: &str) -> Result<String> {
    let url = reqwest::Url::parse(value).context("invalid server endpoint")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("server endpoint must be an HTTP(S) origin without credentials, query, or fragment");
    }
    Ok(value.trim_end_matches('/').to_string())
}

fn load_credential(
    arguments: &ServerConnectionArgs,
    server_node_id: &str,
) -> Result<(String, &'static str)> {
    if let Ok(token) = env::var(TOKEN_ENVIRONMENT_VARIABLE) {
        return validate_token(token).map(|token| (token, "environment"));
    }
    if let Some(path) = &arguments.credential_file {
        let metadata = fs::metadata(path).context("inspect server credential file")?;
        if !metadata.is_file() || metadata.len() > 4096 {
            bail!("server credential file must be a regular file no larger than 4096 bytes");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                bail!("server credential file must have owner-only permissions (0600)");
            }
        }
        return validate_token(fs::read_to_string(path)?).map(|token| (token, "owner_only_file"));
    }
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, server_node_id)
        .context("open paired server keychain entry")?;
    let token = entry
        .get_password()
        .context("read paired server credential from keychain")?;
    validate_token(token).map(|token| (token, "keychain"))
}

fn validate_token(value: String) -> Result<String> {
    let token = value.trim().to_string();
    if token.is_empty() || token.len() > 4096 || token.chars().any(char::is_whitespace) {
        bail!("server credential is malformed");
    }
    Ok(token)
}

fn send_request(
    http: &reqwest::blocking::Client,
    endpoint: &str,
    token: Option<&str>,
    method: Method,
    path: &str,
    query: &[(String, String)],
    body: Option<&Value>,
) -> Result<Value> {
    let mut url = reqwest::Url::parse(&format!("{endpoint}{path}"))?;
    url.query_pairs_mut().extend_pairs(query.iter());
    let request_id = format!("req_cli_{}", Uuid::new_v4().simple());
    let mut request = http
        .request(method, url)
        .header("x-request-id", &request_id);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    if let Some(body) = body {
        request = request.json(body);
    }
    let response = request.send().map_err(|_| RemoteCommandError {
        code: "server_unreachable".into(),
        request_id: Some(request_id.clone()),
    })?;
    let status = response.status();
    let value: Value = response.json().map_err(|_| RemoteCommandError {
        code: "internal_error".into(),
        request_id: Some(request_id.clone()),
    })?;
    if status.is_success() {
        return Ok(value);
    }
    let server_code = value["code"].as_str().unwrap_or("internal_error");
    let code = if rcdvlog_server_core::is_stable_error_code(server_code) {
        server_code
    } else if status.as_u16() == 401 {
        "unauthorized"
    } else if status.as_u16() == 403 {
        "forbidden"
    } else {
        "internal_error"
    };
    Err(RemoteCommandError {
        code: code.to_string(),
        request_id: value["requestId"]
            .as_str()
            .map(str::to_string)
            .or(Some(request_id)),
    }
    .into())
}

fn write_secret_file(path: &Path, secret: &str) -> Result<()> {
    if !path.is_absolute() {
        bail!("credential output file must be an absolute path");
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(secret.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn sync_now(
    connection: &ServerConnectionArgs,
    catalog_path: &Path,
    project_id: &str,
) -> Result<Value> {
    let Some(_guard) = rcdvlog_sync::SyncProcessGuard::try_acquire(catalog_path)? else {
        rcdvlog_sync::signal_running_sync_manager(catalog_path, project_id)?;
        return Ok(json!({
            "path": "signaled_running_desktop",
            "projectId": project_id,
            "reconciliationStartedByCli": false,
        }));
    };
    let client = RemoteClient::new(connection)?;
    let catalog = Catalog::open_existing(catalog_path)?;
    let project = catalog
        .list_projects()?
        .into_iter()
        .find(|project| project.id == project_id)
        .ok_or_else(|| anyhow!("project not found"))?;
    let server_node_id = project
        .paired_server_node_id
        .ok_or_else(|| anyhow!("project is not paired"))?;
    if client.version["nodeId"].as_str() != Some(server_node_id.as_str()) {
        return Err(RemoteCommandError {
            code: "conflict".into(),
            request_id: None,
        }
        .into());
    }
    let local_node_id = catalog
        .local_node_id()?
        .ok_or_else(|| anyhow!("catalog local node identity is missing"))?;
    let path = format!("/api/v1/internal/projects/{project_id}/reconcile");
    let mut sent = 0;
    let mut received = 0;
    let response = loop {
        let cursors = catalog.operation_cursors()?;
        let acknowledged_local = catalog
            .peer_operation_acknowledgements(&server_node_id)?
            .get(&local_node_id)
            .copied()
            .unwrap_or(0);
        let all_operations = catalog.list_operations(project_id)?;
        let operations = all_operations
            .iter()
            .filter(|operation| {
                operation.origin_node_id == local_node_id
                    && operation.origin_sequence > acknowledged_local
            })
            .take(DEFAULT_MAX_OPERATIONS)
            .cloned()
            .collect::<Vec<_>>();
        let request = ReconcileRequest {
            protocol_version: ProtocolRange::default(),
            project_schema_version: rcdvlog_core::PROJECT_CATALOG_SCHEMA_VERSION,
            project_id: project_id.to_string(),
            server_node_id: server_node_id.clone(),
            operation_cursors: cursors,
            operations,
            observations: vec![],
            representation_manifests: vec![],
            maximum_response_operations: DEFAULT_MAX_OPERATIONS,
        };
        let sent_this_page = request.operations.len();
        let value = client.post(&path, &serde_json::to_value(&request)?)?;
        let mut page: ReconcileResponse = serde_json::from_value(value)?;
        let maximum_recovery_pages = all_operations
            .len()
            .div_ceil(DEFAULT_MAX_OPERATIONS)
            .saturating_add(1);
        for _ in 0..maximum_recovery_pages {
            let Some(operations) =
                exact_sequence_recovery_page(&page, &server_node_id, &all_operations)
            else {
                break;
            };
            let mut recovery_request = request.clone();
            recovery_request.operations = operations;
            let value = client.post(&path, &serde_json::to_value(&recovery_request)?)?;
            page = serde_json::from_value(value)?;
        }
        if !page.operations.is_empty() {
            apply_reconcile_operations(&catalog, &page.operations)?;
        }
        catalog.record_peer_operation_acknowledgements(&server_node_id, &page.committed_cursors)?;
        if let Some(last_sent) = request.operations.last()
            && page
                .committed_cursors
                .get(&local_node_id)
                .copied()
                .unwrap_or(0)
                < last_sent.origin_sequence
        {
            bail!("server did not acknowledge the uploaded operation page");
        }
        sent += sent_this_page;
        received += page.operations.len();
        let has_more = page.has_more_operations || sent_this_page == DEFAULT_MAX_OPERATIONS;
        if !has_more {
            break page;
        }
    };
    Ok(json!({
        "path": "exclusive_cli_reconciliation",
        "projectId": project_id,
        "reconciliationStartedByCli": true,
        "sentOperations": sent,
        "receivedOperations": received,
        "hasMoreOperations": response.has_more_operations,
        "conflicts": response.conflicts,
        "sequenceRecovery": response.sequence_recovery,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn credential_file_must_be_owner_only_and_token_never_appears_in_errors() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("credential");
        fs::write(&path, "super-secret-token").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        }
        let arguments = ServerConnectionArgs {
            endpoint: "http://127.0.0.1:1".into(),
            credential_file: Some(path),
            timeout_seconds: 1,
        };
        let error = load_credential(&arguments, "node_fixture").unwrap_err();
        assert!(!format!("{error:#}").contains("super-secret-token"));
    }

    #[test]
    fn endpoint_rejects_embedded_credentials_and_query_strings() {
        assert!(normalize_endpoint("https://token@example.com").is_err());
        assert!(normalize_endpoint("https://example.com/?token=x").is_err());
        assert_eq!(
            normalize_endpoint("https://example.com/").unwrap(),
            "https://example.com"
        );
    }
}
