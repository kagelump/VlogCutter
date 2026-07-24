use rcdvlog_sync::{
    BaselineEnvelope, BaselinePreview as SyncBaselinePreview, ReconcileRequest, ReconcileResponse,
};
use serde::{Deserialize, Serialize};

/// Error kinds the desktop HTTP client can surface to the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientError {
    pub kind: String,
    pub message: String,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl ClientError {
    pub fn connection(message: impl Into<String>) -> Self {
        Self {
            kind: "connection".to_string(),
            message: message.into(),
        }
    }

    pub fn auth(message: impl Into<String>) -> Self {
        Self {
            kind: "auth".to_string(),
            message: message.into(),
        }
    }

    pub fn server(message: impl Into<String>) -> Self {
        Self {
            kind: "server".to_string(),
            message: message.into(),
        }
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self {
            kind: "protocol".to_string(),
            message: message.into(),
        }
    }
}

// ── Response DTOs ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerVersionInfo {
    pub node_id: String,
    pub application_version: String,
    pub protocol_major: u32,
    pub protocol_minor: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerBaselinePreview {
    pub request_id: String,
    pub preview: SyncBaselinePreview,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectReplicaDto {
    pub project_id: String,
    pub project_name: String,
    pub baseline_digest: String,
    pub asset_count: usize,
    pub operation_count: usize,
    pub reused_existing: bool,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerBaselineImport {
    pub request_id: String,
    pub replica: ProjectReplicaDto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerError {
    pub request_id: Option<String>,
    pub code: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReconcileHttpResponse {
    #[serde(flatten)]
    reconciliation: ReconcileResponse,
}

/// Server-side representation metadata, used by the desktop to decide
/// whether to download a server proxy/thumbnail.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepresentationDto {
    pub id: String,
    pub media_object_id: String,
    pub source_content_hash: String,
    pub source_size_bytes: u64,
    pub kind: String,
    pub recipe_key: String,
    pub state: String,
    pub output_content_hash: Option<String>,
    pub byte_length: Option<u64>,
    pub content_type: String,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub available_on_node: bool,
}

/// Payload sent by the Mac to report a node-targeted job result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteNodeJobRequest {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_identity: Option<String>,
    #[serde(default)]
    pub details: serde_json::Value,
}

// ── Client ─────────────────────────────────────────────────────────────

/// Authenticated HTTP client for the RCD VlogCutter server.
///
/// Reads the device bearer token from the macOS Keychain using the
/// server endpoint and stable node ID as the account identifier.
pub struct ServerClient {
    http: reqwest::Client,
    endpoint: String,
    #[allow(dead_code)]
    node_id: String,
    token: String,
}

impl ServerClient {
    /// Build a client by looking up the device token in Keychain.
    pub fn new(endpoint: String, node_id: String) -> Result<Self, ClientError> {
        let token = crate::keychain::get_server_credential(&endpoint, &node_id)
            .map_err(|e| ClientError::auth(format!("keychain read failed: {e}")))?
            .ok_or_else(|| {
                ClientError::auth(
                    "server credential not found in keychain — pair first".to_string(),
                )
            })?;

        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| ClientError::connection(format!("failed to create HTTP client: {e}")))?;

        Ok(Self {
            http,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            node_id,
            token,
        })
    }

    // ── Public API ─────────────────────────────────────────────────

    /// Fetch server identity and protocol version.
    /// Calls `GET /api/v1/version` (public, no auth needed before pairing).
    pub async fn fetch_version(endpoint: &str) -> Result<ServerVersionInfo, ClientError> {
        let url = format!("{}/api/v1/version", endpoint.trim_end_matches('/'));
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| ClientError::connection(format!("failed to create HTTP client: {e}")))?;

        let response = http.get(&url).send().await.map_err(|e| {
            if e.is_connect() {
                ClientError::connection(format!(
                    "cannot reach server at {endpoint}. Check that the server is running and accessible on this network."
                ))
            } else if e.is_timeout() {
                ClientError::connection(format!(
                    "server at {endpoint} timed out. Check network connectivity."
                ))
            } else {
                ClientError::connection(format!("connection error: {e}"))
            }
        })?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(ClientError::server(format!(
                "server identity check failed (HTTP {status}): {body}"
            )));
        }

        let value: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| ClientError::protocol(format!("invalid server response: {e}")))?;

        Ok(ServerVersionInfo {
            node_id: value["nodeId"]
                .as_str()
                .ok_or_else(|| ClientError::protocol("missing nodeId in version response"))?
                .to_string(),
            application_version: value["applicationVersion"]
                .as_str()
                .unwrap_or("unknown")
                .to_string(),
            protocol_major: value["protocolVersion"]["major"].as_u64().unwrap_or(0) as u32,
            protocol_minor: value["protocolVersion"]["minor"].as_u64().unwrap_or(0) as u32,
        })
    }

    /// Send a baseline envelope to the server for read-only preview.
    /// Calls `POST /api/v1/internal/projects/baseline/preview`.
    pub async fn preview_baseline(
        &self,
        envelope: &BaselineEnvelope,
    ) -> Result<SyncBaselinePreview, ClientError> {
        let url = format!(
            "{}/api/v1/internal/projects/baseline/preview",
            self.endpoint
        );

        let response = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .bearer_auth(&self.token)
            .json(envelope)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(self.parse_server_error(status, &body));
        }

        let result: ServerBaselinePreview = serde_json::from_str(&body)
            .map_err(|e| ClientError::protocol(format!("invalid preview response: {e}")))?;

        Ok(result.preview)
    }

    /// Import a baseline envelope on the server.
    /// Calls `POST /api/v1/internal/projects/baseline/import`.
    pub async fn import_baseline(
        &self,
        envelope: &BaselineEnvelope,
    ) -> Result<ProjectReplicaDto, ClientError> {
        let url = format!("{}/api/v1/internal/projects/baseline/import", self.endpoint);

        let response = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .bearer_auth(&self.token)
            .json(envelope)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(self.parse_server_error(status, &body));
        }

        let result: ServerBaselineImport = serde_json::from_str(&body)
            .map_err(|e| ClientError::protocol(format!("invalid import response: {e}")))?;

        Ok(result.replica)
    }

    /// Send a reconcile request to the server for incremental sync.
    /// Calls `POST /api/v1/internal/projects/{project_id}/reconcile`.
    pub async fn reconcile(
        &self,
        project_id: &str,
        request: &ReconcileRequest,
    ) -> Result<ReconcileResponse, ClientError> {
        let url = format!(
            "{}/api/v1/internal/projects/{}/reconcile",
            self.endpoint, project_id
        );

        let response = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .bearer_auth(&self.token)
            .json(request)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(self.parse_server_error(status, &body));
        }

        let result: ReconcileHttpResponse = serde_json::from_str(&body)
            .map_err(|e| ClientError::protocol(format!("invalid reconcile response: {e}")))?;

        Ok(result.reconciliation)
    }

    /// Query the server for available representations for a set of media objects.
    /// Calls `POST /api/v1/library/representations/query`.
    pub async fn query_representations(
        &self,
        media_object_ids: &[String],
    ) -> Result<Vec<RepresentationDto>, ClientError> {
        let url = format!("{}/api/v1/library/representations/query", self.endpoint);
        let body = serde_json::json!({ "mediaObjectIds": media_object_ids });

        let response = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(self.parse_server_error(status, &body));
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct QueryResponse {
            representations: Vec<RepresentationDto>,
        }

        let result: QueryResponse = serde_json::from_str(&body).map_err(|e| {
            ClientError::protocol(format!("invalid representations query response: {e}"))
        })?;

        Ok(result.representations)
    }

    /// Download representation bytes from the server.
    /// Calls `GET /api/v1/media/{representation_id}/content`.
    /// Returns the HTTP response for streaming.
    pub async fn download_representation(
        &self,
        representation_id: &str,
    ) -> Result<reqwest::Response, ClientError> {
        let url = format!(
            "{}/api/v1/media/{}/content",
            self.endpoint, representation_id
        );

        self.http
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e))
    }

    /// Report completion of a node-targeted job back to the server.
    /// Calls `POST /api/v1/internal/jobs/{job_id}/complete`.
    pub async fn complete_node_job(
        &self,
        job_id: &str,
        result: &CompleteNodeJobRequest,
    ) -> Result<(), ClientError> {
        let url = format!("{}/api/v1/internal/jobs/{}/complete", self.endpoint, job_id);

        let response = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .bearer_auth(&self.token)
            .json(result)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(self.parse_server_error(status, &body));
        }

        Ok(())
    }

    // ── Helpers ────────────────────────────────────────────────────

    fn map_reqwest_error(&self, error: reqwest::Error) -> ClientError {
        if error.is_connect() {
            ClientError::connection(format!(
                "cannot reach server at {}. Check network and that the server is running.",
                self.endpoint
            ))
        } else if error.is_timeout() {
            ClientError::connection(format!("request to {} timed out.", self.endpoint))
        } else if error.is_decode() {
            ClientError::protocol(format!("invalid response from server: {error}"))
        } else {
            ClientError::connection(format!("request failed: {error}"))
        }
    }

    fn parse_server_error(&self, status: reqwest::StatusCode, body: &str) -> ClientError {
        // Try to extract structured error code
        if let Ok(err) = serde_json::from_str::<ServerError>(body)
            && let Some(code) = err.code
        {
            match code.as_str() {
                "session_invalid" | "device_not_found" => {
                    return ClientError::auth(
                        "server rejected authentication. The device credential may have been revoked. Re-pair to continue.".to_string(),
                    );
                }
                "device_revoked" => {
                    return ClientError::auth(
                        "device credential has been revoked by the server. Re-pair to continue."
                            .to_string(),
                    );
                }
                "baseline_different" | "different_baseline" => {
                    return ClientError::server(format!(
                        "server already has a different baseline for this project (HTTP {status})"
                    ));
                }
                _ => {}
            }
        }

        match status {
            reqwest::StatusCode::UNAUTHORIZED => ClientError::auth(
                "authentication failed (HTTP 401). The device credential may have expired."
                    .to_string(),
            ),
            reqwest::StatusCode::FORBIDDEN => ClientError::auth(
                "access denied (HTTP 403). Check that the device credential is still valid."
                    .to_string(),
            ),
            reqwest::StatusCode::CONFLICT => {
                ClientError::server(format!("conflict (HTTP 409): {body}"))
            }
            _ => ClientError::server(format!("server error (HTTP {status}): {body}")),
        }
    }
}
