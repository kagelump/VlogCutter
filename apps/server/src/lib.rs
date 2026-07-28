#![recursion_limit = "256"]

use anyhow::{Context, Result, anyhow};
use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::rejection::QueryRejection;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Extension, OriginalUri, Path as AxumPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use futures::SinkExt;
use futures::stream::StreamExt;
use rcdvlog_server_core::library_query::{LibraryFilters, LibraryQuery, LibrarySort};
use rcdvlog_server_core::{
    APPLICATION_VERSION, AddToProjectRequest, AuditActorType, AuthDiagnostics, AuthError,
    AuthService, BaselineEnvelope, BaselinePreview, BindingVerificationStatus,
    CONTROL_CATALOG_SCHEMA_VERSION, DeviceCredentialInfo, GlobalMediaVerificationState,
    JobEngineError, JobId, JobState, LocalNodeRootBindingInput, MediaAvailability, MediaObject,
    MediaObjectId, MediaSearchFacts, NewScanSchedule, NewStorageRoot, NodeAvailabilityState,
    NodeId, NodeJobCompletion, PROTOCOL_MAJOR_VERSION, PROTOCOL_MINOR_VERSION, ProjectId,
    ProjectReplica, ProjectReplicaError, ReadinessReport, ReconcileRequest, ReconcileResponse,
    Representation, RepresentationError, RepresentationId, RepresentationKind,
    RepresentationRequest, RootAssociationConfirmation, RootAssociationConfirmationStatus,
    RootRelativePath, RuntimeFoundation, ScanScheduleUpdate, ScanService, ScanServiceError,
    ScanToolConfig, SessionInfo, StorageAccessMode, StorageLibraryError, StorageProtocol,
    StorageRootId, StorageRootUpdate,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::IntoFuture;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::time::timeout;
use tokio_util::io::ReaderStream;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use uuid::Uuid;

pub const REQUEST_ID_HEADER: &str = "x-request-id";
const MAX_REQUEST_ID_BYTES: usize = 64;
const DEFAULT_READINESS_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_HTTP_BODY_BYTES: usize = 64 * 1024 * 1024;

type ReadinessEvaluator = dyn Fn() -> ReadinessReport + Send + Sync + 'static;

#[derive(Clone)]
pub struct HttpState {
    foundation: Arc<RuntimeFoundation>,
    auth: Arc<AuthService>,
    readiness_gate: ReadinessGate,
    ws_channels: Arc<WsChannels>,
    web_root: Option<Arc<PathBuf>>,
}

#[derive(Clone)]
struct ReadinessGate {
    evaluator: Arc<ReadinessEvaluator>,
    single_flight: Arc<Semaphore>,
    response_timeout: Duration,
}

// ── WebSocket hint broadcast ──────────────────────────────────────────

/// Per-project broadcast channels for sync hints.
/// Tokio broadcast drops slow consumers — acceptable since hints are
/// ephemeral; every reconnect triggers a full HTTP cursor reconcile.
#[derive(Clone)]
struct WsChannels {
    channels: Arc<StdRwLock<HashMap<String, tokio::sync::broadcast::Sender<String>>>>,
}

impl WsChannels {
    fn new() -> Self {
        Self {
            channels: Arc::new(StdRwLock::new(HashMap::new())),
        }
    }

    fn subscribe(&self, project_id: &str) -> tokio::sync::broadcast::Receiver<String> {
        let mut map = self.channels.write().unwrap();
        map.entry(project_id.to_string())
            .or_insert_with(|| tokio::sync::broadcast::channel::<String>(16).0)
            .subscribe()
    }

    fn broadcast(&self, project_id: &str, hint: &str) {
        if let Ok(map) = self.channels.read()
            && let Some(tx) = map.get(project_id)
        {
            let _ = tx.send(hint.to_string());
        }
    }
}

impl HttpState {
    pub fn new(foundation: Arc<RuntimeFoundation>) -> Self {
        let readiness_foundation = foundation.clone();
        let auth = Arc::new(foundation.auth().clone());
        Self::with_readiness_evaluator(foundation, auth, DEFAULT_READINESS_TIMEOUT, move || {
            readiness_foundation.readiness()
        })
    }

    fn with_readiness_evaluator(
        foundation: Arc<RuntimeFoundation>,
        auth: Arc<AuthService>,
        response_timeout: Duration,
        evaluator: impl Fn() -> ReadinessReport + Send + Sync + 'static,
    ) -> Self {
        Self {
            foundation,
            auth,
            readiness_gate: ReadinessGate {
                evaluator: Arc::new(evaluator),
                single_flight: Arc::new(Semaphore::new(1)),
                response_timeout,
            },
            ws_channels: Arc::new(WsChannels::new()),
            web_root: std::env::var_os("RCDVLOG_WEB_DIR")
                .map(PathBuf::from)
                .map(Arc::new),
        }
    }

    pub fn with_web_root(mut self, web_root: impl Into<PathBuf>) -> Self {
        self.web_root = Some(Arc::new(web_root.into()));
        self
    }
}

impl ReadinessGate {
    async fn evaluate(&self) -> ReadinessReport {
        let permit = match self.single_flight.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                return ReadinessReport::evaluation_unavailable("readiness_check_busy");
            }
        };
        let evaluator = self.evaluator.clone();
        let evaluation = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            evaluator()
        });
        match timeout(self.response_timeout, evaluation).await {
            Ok(Ok(report)) => report,
            Ok(Err(_)) => ReadinessReport::evaluation_unavailable("readiness_check_failed"),
            Err(_) => ReadinessReport::evaluation_unavailable("readiness_check_timeout"),
        }
    }
}

#[derive(Debug, Clone)]
struct RequestId(String);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    request_id: String,
    status: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadyResponse {
    request_id: String,
    #[serde(flatten)]
    report: ReadinessReport,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProtocolVersion {
    major: u32,
    minor: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionResponse {
    request_id: String,
    application_version: &'static str,
    protocol_version: ProtocolVersion,
    control_schema_version: u32,
    node_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RootResponse {
    request_id: String,
    service: &'static str,
    status: &'static str,
    version_path: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorResponse {
    request_id: String,
    code: &'static str,
}

// ── Auth DTOs ──────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapCodeResponse {
    request_id: String,
    code: String,
    expires_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapExchangeRequest {
    code: String,
    hostname: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionCreatedResponse {
    request_id: String,
    session_id: String,
    display_prefix: String,
    expires_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionsListResponse {
    request_id: String,
    sessions: Vec<SessionInfo>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionRefreshResponse {
    request_id: String,
    session: SessionInfo,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PairingCodeResponse {
    request_id: String,
    code: String,
    expires_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairingExchangeRequest {
    code: String,
    #[serde(default = "default_device_label")]
    label: String,
}

fn default_device_label() -> String {
    "MacBook Pro".to_string()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceCreatedResponse {
    request_id: String,
    credential_id: String,
    token: String,
    token_prefix: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DevicesListResponse {
    request_id: String,
    devices: Vec<DeviceCredentialInfo>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthDiagnosticsResponse {
    request_id: String,
    diagnostics: AuthDiagnostics,
}

// ── Auth principal ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
#[allow(dead_code)]
enum AuthPrincipal {
    Browser {
        session_id: String,
        csrf_token: String,
    },
    Device {
        credential_id: String,
    },
}

#[derive(Clone)]
pub struct ShutdownCoordinator {
    token: CancellationToken,
}

impl Default for ShutdownCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl ShutdownCoordinator {
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
        }
    }

    pub fn cancel(&self) {
        self.token.cancel();
    }

    pub fn child_token(&self) -> CancellationToken {
        self.token.child_token()
    }

    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }
}

// ── Auth middleware ──────────────────────────────────────────────────

async fn auth_middleware(
    State(state): State<HttpState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let auth = state.auth.clone();

    // Try device bearer token first
    if let Some(auth_header) = request.headers().get(header::AUTHORIZATION)
        && let Ok(value) = auth_header.to_str()
        && let Some(token) = value.strip_prefix("Bearer ")
    {
        match auth.validate_device(token) {
            Ok(credential_id) => {
                request
                    .extensions_mut()
                    .insert(AuthPrincipal::Device { credential_id });
                return next.run(request).await;
            }
            Err(AuthError::DeviceNotFound) => {}
            Err(AuthError::DeviceRevoked) => {
                return auth_error_response(AuthError::DeviceRevoked);
            }
            Err(_) => {}
        }
    }

    // Try session cookie
    if let Some(cookie_header) = request.headers().get(header::COOKIE)
        && let Ok(cookie_str) = cookie_header.to_str()
        && let Some((session_id, session_token)) = parse_session_cookie(cookie_str)
    {
        let csrf_token = request
            .headers()
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        match auth.validate_session(&session_id, &session_token) {
            Ok(()) => {
                request.extensions_mut().insert(AuthPrincipal::Browser {
                    session_id,
                    csrf_token,
                });
                return next.run(request).await;
            }
            Err(AuthError::SessionInvalid) => {}
            Err(e) => {
                return auth_error_response(e);
            }
        }
    }

    auth_error_response(AuthError::SessionInvalid)
}

async fn require_csrf(
    State(state): State<HttpState>,
    Extension(principal): Extension<AuthPrincipal>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if request.method() == Method::GET || request.method() == Method::HEAD {
        return next.run(request).await;
    }

    if let AuthPrincipal::Browser {
        session_id,
        csrf_token,
    } = &principal
    {
        if csrf_token.is_empty() {
            return auth_error_response(AuthError::CsrfMismatch);
        }
        // Session was already validated by auth_middleware; only verify the
        // CSRF token against the stored hash.
        if state
            .auth
            .validate_csrf_token(session_id, csrf_token)
            .is_err()
        {
            return auth_error_response(AuthError::CsrfMismatch);
        }
    }
    next.run(request).await
}

async fn security_headers(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    headers.insert(
        header::HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("same-origin"),
    );
    response
}

fn parse_session_cookie(cookie_str: &str) -> Option<(String, String)> {
    for part in cookie_str.split(';') {
        let trimmed = part.trim();
        if let Some(value) = trimmed.strip_prefix("rcdvlog_session=") {
            let decoded = value.trim_matches('"');
            let mut parts = decoded.splitn(2, ':');
            let session_id = parts.next()?.to_string();
            let session_token = parts.next()?.to_string();
            if session_id.is_empty() || session_token.is_empty() {
                return None;
            }
            return Some((session_id, session_token));
        }
    }
    None
}

fn auth_error_response(error: AuthError) -> Response {
    let (status, code) = match &error {
        AuthError::SessionInvalid
        | AuthError::DeviceNotFound
        | AuthError::BootstrapNotFoundOrExpired
        | AuthError::PairingNotFoundOrExpired => (StatusCode::UNAUTHORIZED, error.code()),
        AuthError::DeviceRevoked
        | AuthError::BootstrapAlreadyUsed
        | AuthError::PairingAlreadyUsed => (StatusCode::FORBIDDEN, error.code()),
        AuthError::CsrfMismatch => (StatusCode::FORBIDDEN, error.code()),
        AuthError::Database(_) => (StatusCode::INTERNAL_SERVER_ERROR, error.code()),
    };
    (
        status,
        Json(ErrorResponse {
            request_id: String::new(),
            code,
        }),
    )
        .into_response()
}

pub fn router(state: HttpState) -> Router {
    // Auth routes (open to bootstrap/pairing exchange)
    let auth_open = Router::new()
        .route("/api/v1/auth/bootstrap", get(create_bootstrap_code))
        .route(
            "/api/v1/auth/bootstrap/exchange",
            post(exchange_bootstrap_code),
        )
        .route("/api/v1/auth/devices/exchange", post(exchange_pairing_code));

    // Auth routes (require browser session)
    let auth_session = Router::new()
        .route("/api/v1/auth/sessions", get(list_sessions))
        .route("/api/v1/auth/sessions/{session_id}", delete(revoke_session))
        .route("/api/v1/auth/sessions/refresh", post(refresh_session))
        .route("/api/v1/auth/pairing", post(create_pairing_code))
        .route("/api/v1/auth/devices", get(list_devices))
        .route(
            "/api/v1/auth/devices/{credential_id}",
            delete(revoke_device),
        )
        .route("/api/v1/auth/diagnostics", get(auth_diagnostics))
        .layer(middleware::from_fn_with_state(state.clone(), require_csrf))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    // Internal routes (require auth — device or session)
    let internal = Router::new()
        .route(
            "/api/v1/internal/projects/baseline/preview",
            post(preview_project_baseline),
        )
        .route(
            "/api/v1/internal/projects/baseline/import",
            post(import_project_baseline),
        )
        .route(
            "/api/v1/internal/projects/{project_id}/reconcile",
            post(reconcile_project),
        )
        .route("/api/v1/representations", post(request_representation))
        .route(
            "/api/v1/library/representations/query",
            post(query_representations),
        )
        .route(
            "/api/v1/representations/{representation_id}",
            get(get_representation),
        )
        .route(
            "/api/v1/media/{representation_id}/content",
            get(media_content).head(media_content),
        )
        .route(
            "/api/v1/internal/jobs/{job_id}/complete",
            post(complete_node_job),
        )
        .route("/api/v1/ws/events", get(ws_events_handler))
        .route("/api/v1/library/media", get(list_library_media))
        .route(
            "/api/v1/library/media/{media_object_id}",
            get(get_library_media_detail),
        )
        .route("/api/v1/projects", get(list_projects))
        .route(
            "/api/v1/projects/{project_id}/assets",
            get(list_project_assets),
        )
        .route(
            "/api/v1/projects/{project_id}/assets/add/preview",
            post(preview_add_to_project),
        )
        .route(
            "/api/v1/projects/{project_id}/assets/add",
            post(add_to_project),
        )
        .route(
            "/api/v1/projects/{project_id}/assets/{asset_id}/review",
            get(get_project_review_asset),
        )
        .route(
            "/api/v1/projects/{project_id}/assets/{asset_id}/notes",
            post(create_review_note),
        )
        .route(
            "/api/v1/projects/{project_id}/assets/{asset_id}/labels/add",
            post(add_review_label),
        )
        .route(
            "/api/v1/projects/{project_id}/assets/{asset_id}/labels/remove",
            post(remove_review_label),
        )
        .route(
            "/api/v1/projects/{project_id}/assets/{asset_id}/removed",
            post(set_review_removed),
        )
        .route(
            "/api/v1/projects/{project_id}/assets/{asset_id}/rotation",
            post(set_review_rotation),
        )
        .route("/api/v1/admin/overview", get(admin_overview))
        .route("/api/v1/admin/audit", get(list_admin_audit))
        .route(
            "/api/v1/admin/storage-roots",
            get(list_admin_storage_roots).post(create_admin_storage_root),
        )
        .route(
            "/api/v1/admin/storage-roots/{root_id}",
            patch(update_admin_storage_root),
        )
        .route(
            "/api/v1/admin/storage-bindings",
            get(list_admin_storage_bindings),
        )
        .route(
            "/api/v1/admin/storage-roots/{root_id}/binding",
            put(upsert_admin_storage_binding),
        )
        .route(
            "/api/v1/admin/storage-roots/{root_id}/binding/confirmation",
            post(confirm_admin_storage_binding),
        )
        .route(
            "/api/v1/admin/storage-roots/{root_id}/binding/verify",
            post(verify_admin_storage_binding),
        )
        .route(
            "/api/v1/admin/scans",
            get(list_admin_scans).post(request_admin_scan),
        )
        .route(
            "/api/v1/admin/scans/{scan_id}/diagnostics",
            get(list_admin_scan_diagnostics),
        )
        .route(
            "/api/v1/admin/scan-schedules",
            get(list_admin_scan_schedules).post(create_admin_scan_schedule),
        )
        .route(
            "/api/v1/admin/scan-schedules/{schedule_id}",
            patch(update_admin_scan_schedule),
        )
        .route("/api/v1/admin/jobs", get(list_admin_jobs))
        .route("/api/v1/admin/sync", get(list_admin_sync))
        .route("/api/v1/admin/jobs/{job_id}", get(get_admin_job))
        .route("/api/v1/admin/jobs/{job_id}/cancel", post(cancel_admin_job))
        .route("/api/v1/admin/jobs/{job_id}/retry", post(retry_admin_job))
        .route(
            "/api/v1/admin/representations/batch",
            post(request_admin_representation_batch),
        )
        .layer(middleware::from_fn_with_state(state.clone(), require_csrf))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    // Public routes (no auth)
    let public = Router::new()
        .route("/api/v1", get(root))
        .route("/api/openapi.json", get(openapi))
        .route("/api/v1/health", get(health))
        .route("/api/v1/ready", get(ready))
        .route("/api/v1/version", get(version));

    Router::new()
        .merge(public)
        .merge(auth_open)
        .merge(auth_session)
        .merge(internal)
        .fallback(web_or_not_found)
        .with_state(state)
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
        .layer(middleware::from_fn(security_headers))
        .layer(middleware::from_fn(request_context))
}

pub fn openapi_document() -> serde_json::Value {
    serde_json::json!({
        "openapi": "3.1.0",
        "info": {
            "title": "RCD VlogCutter Server API",
            "version": APPLICATION_VERSION
        },
        "paths": {
            "/api/v1/version": {
                "get": {
                    "operationId": "getServerVersion",
                    "responses": { "200": { "$ref": "#/components/responses/Version" } }
                }
            },
            "/api/v1/ready": {
                "get": {
                    "operationId": "getServerReadiness",
                    "responses": {
                        "200": { "$ref": "#/components/responses/Readiness" },
                        "503": { "$ref": "#/components/responses/Readiness" }
                    }
                }
            },
            "/api/v1/auth/bootstrap/exchange": {
                "post": {
                    "operationId": "exchangeBootstrapCode",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/BootstrapExchangeRequest" }
                            }
                        }
                    },
                    "responses": {
                        "200": { "$ref": "#/components/responses/SessionCreated" },
                        "401": { "$ref": "#/components/responses/Error" }
                    }
                }
            },
            "/api/v1/auth/sessions": {
                "get": {
                    "operationId": "listSessions",
                    "responses": {
                        "200": { "$ref": "#/components/responses/Sessions" },
                        "401": { "$ref": "#/components/responses/Error" }
                    }
                }
            },
            "/api/v1/library/media": {
                "get": {
                    "operationId": "listLibraryMedia",
                    "parameters": [
                        { "name": "limit", "in": "query", "schema": { "type": "integer", "minimum": 1, "maximum": 200 } },
                        { "name": "sort", "in": "query", "schema": { "$ref": "#/components/schemas/LibrarySort" } },
                        { "name": "verificationState", "in": "query", "schema": { "$ref": "#/components/schemas/GlobalMediaVerificationState" } },
                        { "name": "storageRootId", "in": "query", "schema": { "type": "string" } },
                        { "name": "camera", "in": "query", "schema": { "type": "string" } },
                        { "name": "filenamePrefix", "in": "query", "schema": { "type": "string" } },
                        { "name": "availability", "in": "query", "schema": { "$ref": "#/components/schemas/NodeAvailabilityState" } },
                        { "name": "after", "in": "query", "schema": { "type": "string" } }
                    ],
                    "responses": {
                        "200": { "$ref": "#/components/responses/LibrarySearchPage" },
                        "400": { "$ref": "#/components/responses/Error" },
                        "401": { "$ref": "#/components/responses/Error" }
                    }
                }
            },
            "/api/v1/library/media/{mediaObjectId}": {
                "get": {
                    "operationId": "getLibraryMediaDetail",
                    "parameters": [
                        { "name": "mediaObjectId", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "responses": {
                        "200": { "$ref": "#/components/responses/LibraryMediaDetail" },
                        "401": { "$ref": "#/components/responses/Error" },
                        "404": { "$ref": "#/components/responses/Error" }
                    }
                }
            },
            "/api/v1/projects": {
                "get": {
                    "operationId": "listProjects",
                    "parameters": [
                        { "name": "limit", "in": "query", "schema": { "type": "integer", "minimum": 1, "maximum": 200 } },
                        { "name": "after", "in": "query", "schema": { "type": "string" } }
                    ],
                    "responses": {
                        "200": { "$ref": "#/components/responses/ProjectPage" },
                        "400": { "$ref": "#/components/responses/Error" },
                        "401": { "$ref": "#/components/responses/Error" }
                    }
                }
            },
            "/api/v1/projects/{projectId}/assets": {
                "get": {
                    "operationId": "listProjectAssets",
                    "parameters": [
                        { "name": "projectId", "in": "path", "required": true, "schema": { "type": "string" } },
                        { "name": "limit", "in": "query", "schema": { "type": "integer", "minimum": 1, "maximum": 200 } },
                        { "name": "after", "in": "query", "schema": { "type": "string" } },
                        { "name": "search", "in": "query", "schema": { "type": "string" } }
                    ],
                    "responses": {
                        "200": { "$ref": "#/components/responses/ProjectAssetPage" },
                        "400": { "$ref": "#/components/responses/Error" },
                        "401": { "$ref": "#/components/responses/Error" },
                        "404": { "$ref": "#/components/responses/Error" }
                    }
                }
            },
            "/api/v1/projects/{projectId}/assets/add/preview": {
                "post": {
                    "operationId": "previewAddToProject",
                    "parameters": [
                        { "name": "projectId", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/AddToProjectRequest" } } } },
                    "responses": {
                        "200": { "$ref": "#/components/responses/AddToProjectPreview" },
                        "400": { "$ref": "#/components/responses/Error" },
                        "401": { "$ref": "#/components/responses/Error" },
                        "404": { "$ref": "#/components/responses/Error" }
                    }
                }
            },
            "/api/v1/projects/{projectId}/assets/add": {
                "post": {
                    "operationId": "addToProject",
                    "parameters": [
                        { "name": "projectId", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/AddToProjectRequest" } } } },
                    "responses": {
                        "200": { "$ref": "#/components/responses/AddToProjectResult" },
                        "400": { "$ref": "#/components/responses/Error" },
                        "401": { "$ref": "#/components/responses/Error" },
                        "404": { "$ref": "#/components/responses/Error" }
                    }
                }
            },
            "/api/v1/projects/{projectId}/assets/{assetId}/review": {
                "get": {
                    "operationId": "getProjectReviewAsset",
                    "parameters": [
                        { "name": "projectId", "in": "path", "required": true, "schema": { "type": "string" } },
                        { "name": "assetId", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "responses": { "200": { "$ref": "#/components/responses/ProjectReviewAsset" }, "401": { "$ref": "#/components/responses/Error" }, "404": { "$ref": "#/components/responses/Error" } }
                }
            },
            "/api/v1/projects/{projectId}/assets/{assetId}/notes": {
                "post": { "operationId": "createReviewNote", "parameters": [
                    { "name": "projectId", "in": "path", "required": true, "schema": { "type": "string" } },
                    { "name": "assetId", "in": "path", "required": true, "schema": { "type": "string" } }
                ], "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/NoteMutationRequest" } } } }, "responses": { "200": { "$ref": "#/components/responses/ReviewMutationResult" }, "400": { "$ref": "#/components/responses/Error" }, "401": { "$ref": "#/components/responses/Error" } } }
            },
            "/api/v1/projects/{projectId}/assets/{assetId}/labels/add": {
                "post": { "operationId": "addReviewLabel", "parameters": [
                    { "name": "projectId", "in": "path", "required": true, "schema": { "type": "string" } },
                    { "name": "assetId", "in": "path", "required": true, "schema": { "type": "string" } }
                ], "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/LabelMutationRequest" } } } }, "responses": { "200": { "$ref": "#/components/responses/ReviewMutationResult" }, "400": { "$ref": "#/components/responses/Error" }, "401": { "$ref": "#/components/responses/Error" } } }
            },
            "/api/v1/projects/{projectId}/assets/{assetId}/labels/remove": {
                "post": { "operationId": "removeReviewLabel", "parameters": [
                    { "name": "projectId", "in": "path", "required": true, "schema": { "type": "string" } },
                    { "name": "assetId", "in": "path", "required": true, "schema": { "type": "string" } }
                ], "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/LabelMutationRequest" } } } }, "responses": { "200": { "$ref": "#/components/responses/ReviewMutationResult" }, "400": { "$ref": "#/components/responses/Error" }, "401": { "$ref": "#/components/responses/Error" } } }
            },
            "/api/v1/projects/{projectId}/assets/{assetId}/removed": {
                "post": { "operationId": "setReviewRemoved", "parameters": [
                    { "name": "projectId", "in": "path", "required": true, "schema": { "type": "string" } },
                    { "name": "assetId", "in": "path", "required": true, "schema": { "type": "string" } }
                ], "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/RemovedMutationRequest" } } } }, "responses": { "200": { "$ref": "#/components/responses/ReviewMutationResult" }, "400": { "$ref": "#/components/responses/Error" }, "401": { "$ref": "#/components/responses/Error" } } }
            },
            "/api/v1/projects/{projectId}/assets/{assetId}/rotation": {
                "post": { "operationId": "setReviewRotation", "parameters": [
                    { "name": "projectId", "in": "path", "required": true, "schema": { "type": "string" } },
                    { "name": "assetId", "in": "path", "required": true, "schema": { "type": "string" } }
                ], "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/RotationMutationRequest" } } } }, "responses": { "200": { "$ref": "#/components/responses/ReviewMutationResult" }, "400": { "$ref": "#/components/responses/Error" }, "401": { "$ref": "#/components/responses/Error" } } }
            },
            "/api/v1/auth/sessions/{sessionId}": { "delete": { "operationId": "revokeSession", "parameters": [{ "name": "sessionId", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "$ref": "#/components/responses/AdminPayload" }, "401": { "$ref": "#/components/responses/Error" } } } },
            "/api/v1/auth/devices": { "get": { "operationId": "listDevices", "responses": { "200": { "$ref": "#/components/responses/AdminPayload" }, "401": { "$ref": "#/components/responses/Error" } } } },
            "/api/v1/auth/devices/{credentialId}": { "delete": { "operationId": "revokeDevice", "parameters": [{ "name": "credentialId", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "$ref": "#/components/responses/AdminPayload" }, "401": { "$ref": "#/components/responses/Error" } } } },
            "/api/v1/admin/overview": { "get": { "operationId": "getAdminOverview", "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/audit": { "get": { "operationId": "listAdminAudit", "parameters": [{"name":"limit","in":"query","schema":{"type":"integer","minimum":1,"maximum":200}}, {"name":"before","in":"query","schema":{"type":"integer"}}], "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/storage-roots": {
                "get": { "operationId": "listAdminStorageRoots", "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } },
                "post": { "operationId": "createAdminStorageRoot", "requestBody": { "$ref": "#/components/requestBodies/AdminPayload" }, "responses": { "201": { "$ref": "#/components/responses/AdminPayload" } } }
            },
            "/api/v1/admin/storage-roots/{rootId}": { "patch": { "operationId": "updateAdminStorageRoot", "parameters": [{"name":"rootId","in":"path","required":true,"schema":{"type":"string"}}], "requestBody": { "$ref": "#/components/requestBodies/AdminPayload" }, "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/storage-bindings": { "get": { "operationId": "listAdminStorageBindings", "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/storage-roots/{rootId}/binding": { "put": { "operationId": "upsertAdminStorageBinding", "parameters": [{"name":"rootId","in":"path","required":true,"schema":{"type":"string"}}], "requestBody": { "$ref": "#/components/requestBodies/AdminPayload" }, "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/storage-roots/{rootId}/binding/confirmation": { "post": { "operationId": "confirmAdminStorageBinding", "parameters": [{"name":"rootId","in":"path","required":true,"schema":{"type":"string"}}], "requestBody": { "$ref": "#/components/requestBodies/AdminPayload" }, "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/storage-roots/{rootId}/binding/verify": { "post": { "operationId": "verifyAdminStorageBinding", "parameters": [{"name":"rootId","in":"path","required":true,"schema":{"type":"string"}}], "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/scans": {
                "get": { "operationId": "listAdminScans", "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } },
                "post": { "operationId": "requestAdminScan", "requestBody": { "$ref": "#/components/requestBodies/AdminPayload" }, "responses": { "202": { "$ref": "#/components/responses/AdminPayload" } } }
            },
            "/api/v1/admin/scans/{scanId}/diagnostics": { "get": { "operationId": "listAdminScanDiagnostics", "parameters": [
                { "name": "scanId", "in": "path", "required": true, "schema": { "type": "string" } },
                { "name": "limit", "in": "query", "schema": { "type": "integer", "minimum": 1, "maximum": 200 } },
                { "name": "after", "in": "query", "schema": { "type": "string" } }
            ], "responses": { "200": { "$ref": "#/components/responses/AdminPayload" }, "404": { "$ref": "#/components/responses/Error" } } } },
            "/api/v1/admin/scan-schedules": {
                "get": { "operationId": "listAdminScanSchedules", "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } },
                "post": { "operationId": "createAdminScanSchedule", "requestBody": { "$ref": "#/components/requestBodies/AdminPayload" }, "responses": { "201": { "$ref": "#/components/responses/AdminPayload" } } }
            },
            "/api/v1/admin/scan-schedules/{scheduleId}": { "patch": { "operationId": "updateAdminScanSchedule", "parameters": [{"name":"scheduleId","in":"path","required":true,"schema":{"type":"string"}}], "requestBody": { "$ref": "#/components/requestBodies/AdminPayload" }, "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/jobs": { "get": { "operationId": "listAdminJobs", "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/jobs/{jobId}": { "get": { "operationId": "getAdminJob", "parameters": [{"name":"jobId","in":"path","required":true,"schema":{"type":"string"}}], "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/jobs/{jobId}/cancel": { "post": { "operationId": "cancelAdminJob", "parameters": [{"name":"jobId","in":"path","required":true,"schema":{"type":"string"}}], "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/jobs/{jobId}/retry": { "post": { "operationId": "retryAdminJob", "parameters": [{"name":"jobId","in":"path","required":true,"schema":{"type":"string"}}], "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/sync": { "get": { "operationId": "listAdminSync", "responses": { "200": { "$ref": "#/components/responses/AdminPayload" } } } },
            "/api/v1/admin/representations/batch": { "post": { "operationId": "requestAdminRepresentationBatch", "requestBody": { "$ref": "#/components/requestBodies/AdminPayload" }, "responses": { "202": { "$ref": "#/components/responses/AdminPayload" } } } }
        },
        "components": {
            "schemas": {
                "AdminPayload": { "type": "object", "additionalProperties": true },
                "BootstrapExchangeRequest": {
                    "type": "object",
                    "required": ["code"],
                    "properties": {
                        "code": { "type": "string" },
                        "hostname": { "type": ["string", "null"] }
                    }
                },
                "Error": {
                    "type": "object",
                    "required": ["requestId", "code"],
                    "properties": {
                        "requestId": { "type": "string" },
                        "code": { "type": "string" }
                    }
                },
                "ProtocolVersion": {
                    "type": "object",
                    "required": ["major", "minor"],
                    "properties": {
                        "major": { "type": "integer", "minimum": 0 },
                        "minor": { "type": "integer", "minimum": 0 }
                    }
                },
                "Version": {
                    "type": "object",
                    "required": ["requestId", "applicationVersion", "protocolVersion", "controlSchemaVersion", "nodeId"],
                    "properties": {
                        "requestId": { "type": "string" },
                        "applicationVersion": { "type": "string" },
                        "protocolVersion": { "$ref": "#/components/schemas/ProtocolVersion" },
                        "controlSchemaVersion": { "type": "integer", "minimum": 0 },
                        "nodeId": { "type": "string" }
                    }
                },
                "ReadinessCheck": {
                    "type": "object",
                    "required": ["name", "status"],
                    "properties": {
                        "name": { "type": "string" },
                        "status": { "type": "string", "enum": ["ok", "error"] },
                        "code": { "type": "string" }
                    }
                },
                "Readiness": {
                    "type": "object",
                    "required": ["requestId", "status", "checks"],
                    "properties": {
                        "requestId": { "type": "string" },
                        "status": { "type": "string", "enum": ["ready", "not_ready"] },
                        "checks": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/ReadinessCheck" }
                        }
                    }
                },
                "SessionInfo": {
                    "type": "object",
                    "required": ["sessionId", "displayPrefix", "hostname", "createdAt", "lastUsedAt", "expiresAt", "revoked"],
                    "properties": {
                        "sessionId": { "type": "string" },
                        "displayPrefix": { "type": "string" },
                        "hostname": { "type": "string" },
                        "createdAt": { "type": "string", "format": "date-time" },
                        "lastUsedAt": { "type": "string", "format": "date-time" },
                        "expiresAt": { "type": "string", "format": "date-time" },
                        "revoked": { "type": "boolean" }
                    }
                },
                "SessionCreated": {
                    "type": "object",
                    "required": ["requestId", "sessionId", "displayPrefix", "expiresAt"],
                    "properties": {
                        "requestId": { "type": "string" },
                        "sessionId": { "type": "string" },
                        "displayPrefix": { "type": "string" },
                        "expiresAt": { "type": "string", "format": "date-time" }
                    }
                },
                "Sessions": {
                    "type": "object",
                    "required": ["requestId", "sessions"],
                    "properties": {
                        "requestId": { "type": "string" },
                        "sessions": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/SessionInfo" }
                        }
                    }
                },
                "LibrarySort": { "type": "string", "enum": ["createdDesc", "filenameAsc", "sizeDesc"] },
                "GlobalMediaVerificationState": { "type": "string", "enum": ["unverified", "provisional", "verified", "mismatch"] },
                "NodeAvailabilityState": {
                    "type": "string",
                    "enum": [
                        "known",
                        "accessible_here",
                        "accessible_on_another_node",
                        "temporarily_unavailable",
                        "missing_after_completed_observation",
                        "fingerprint_mismatch"
                    ]
                },
                "MediaObject": {
                    "type": "object",
                    "required": ["id", "sizeBytes", "quickFingerprint", "fullContentHash", "verificationState", "createdAt", "updatedAt"],
                    "properties": {
                        "id": { "type": "string" },
                        "sizeBytes": { "type": ["integer", "null"], "minimum": 0 },
                        "quickFingerprint": { "type": ["string", "null"] },
                        "fullContentHash": { "type": ["string", "null"] },
                        "verificationState": { "$ref": "#/components/schemas/GlobalMediaVerificationState" },
                        "createdAt": { "type": "string", "format": "date-time" },
                        "updatedAt": { "type": "string", "format": "date-time" }
                    }
                },
                "LibrarySearchItem": {
                    "type": "object",
                    "required": ["media", "filename", "cameraMake", "cameraModel", "primaryStorageRootId", "storageLocationCount", "projectMembershipCount", "availability", "thumbnailRepresentationId"],
                    "properties": {
                        "media": { "$ref": "#/components/schemas/MediaObject" },
                        "filename": { "type": "string" },
                        "cameraMake": { "type": ["string", "null"] },
                        "cameraModel": { "type": ["string", "null"] },
                        "primaryStorageRootId": { "type": ["string", "null"] },
                        "storageLocationCount": { "type": "integer", "minimum": 0 },
                        "projectMembershipCount": { "type": "integer", "minimum": 0 },
                        "availability": { "$ref": "#/components/schemas/NodeAvailabilityState" },
                        "thumbnailRepresentationId": { "type": ["string", "null"] }
                    }
                },
                "LibrarySearchPage": {
                    "type": "object",
                    "required": ["items", "nextCursor", "availabilityFilterApplied"],
                    "properties": {
                        "items": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/LibrarySearchItem" }
                        },
                        "nextCursor": { "type": ["string", "null"] },
                        "availabilityFilterApplied": { "type": "boolean" }
                    }
                },
                "MediaSearchFacts": {
                    "type": "object",
                    "required": ["filename", "cameraMake", "cameraModel", "primaryStorageRootId", "storageLocationCount", "projectMembershipCount"],
                    "properties": {
                        "filename": { "type": "string" },
                        "cameraMake": { "type": ["string", "null"] },
                        "cameraModel": { "type": ["string", "null"] },
                        "primaryStorageRootId": { "type": ["string", "null"] },
                        "storageLocationCount": { "type": "integer", "minimum": 0 },
                        "projectMembershipCount": { "type": "integer", "minimum": 0 }
                    }
                },
                "StorageLocationAvailability": {
                    "type": "object",
                    "required": ["nodeId", "storageObjectId", "state", "completedGenerationId", "completedGenerationSequence"],
                    "properties": {
                        "nodeId": { "type": "string" },
                        "storageObjectId": { "type": "string" },
                        "state": { "$ref": "#/components/schemas/NodeAvailabilityState" },
                        "completedGenerationId": { "type": ["string", "null"] },
                        "completedGenerationSequence": { "type": ["integer", "null"], "minimum": 0 }
                    }
                },
                "MediaAvailability": {
                    "type": "object",
                    "required": ["mediaObjectId", "queriedNodeId", "state", "locations"],
                    "properties": {
                        "mediaObjectId": { "type": "string" },
                        "queriedNodeId": { "type": "string" },
                        "state": { "$ref": "#/components/schemas/NodeAvailabilityState" },
                        "locations": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/StorageLocationAvailability" }
                        }
                    }
                },
                "LibraryMediaDetail": {
                    "type": "object",
                    "required": ["media", "facts", "availability"],
                    "properties": {
                        "media": { "$ref": "#/components/schemas/MediaObject" },
                        "facts": { "$ref": "#/components/schemas/MediaSearchFacts" },
                        "availability": { "$ref": "#/components/schemas/MediaAvailability" }
                    }
                },
                "ProjectSummary": {
                    "type": "object",
                    "required": ["projectId", "state", "assetCount", "registeredAt", "updatedAt"],
                    "properties": {
                        "projectId": { "type": "string" },
                        "state": { "type": "string" },
                        "assetCount": { "type": "integer", "minimum": 0 },
                        "registeredAt": { "type": "string", "format": "date-time" },
                        "updatedAt": { "type": "string", "format": "date-time" }
                    }
                },
                "ProjectPage": {
                    "type": "object",
                    "required": ["items", "nextCursor"],
                    "properties": {
                        "items": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/ProjectSummary" }
                        },
                        "nextCursor": { "type": ["string", "null"] }
                    }
                },
                "ProjectAssetItem": {
                    "type": "object",
                    "required": ["projectAssetId", "filename", "camera", "mediaObjectId", "mediaVerificationState", "removed", "rotationDegrees", "labels", "activeNote", "durationSeconds", "bestTime"],
                    "properties": {
                        "projectAssetId": { "type": "string" },
                        "filename": { "type": "string" },
                        "camera": { "type": "string" },
                        "mediaObjectId": { "type": ["string", "null"] },
                        "mediaVerificationState": { "type": "string" },
                        "removed": { "type": "boolean" },
                        "rotationDegrees": { "type": "integer" },
                        "labels": { "type": "array", "items": { "type": "string" } },
                        "activeNote": { "type": ["string", "null"] },
                        "durationSeconds": { "type": "number" },
                        "bestTime": { "type": ["string", "null"], "format": "date-time" }
                    }
                },
                "ProjectAssetPage": {
                    "type": "object",
                    "required": ["items", "nextCursor"],
                    "properties": {
                        "items": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/ProjectAssetItem" }
                        },
                        "nextCursor": { "type": ["string", "null"] }
                    }
                },
                "AddToProjectRequest": {
                    "type": "object",
                    "required": ["mediaObjectIds", "acknowledgeUnverified", "idempotencyKey"],
                    "properties": {
                        "mediaObjectIds": { "type": "array", "minItems": 1, "maxItems": 100, "items": { "type": "string" } },
                        "acknowledgeUnverified": { "type": "boolean" },
                        "idempotencyKey": { "type": "string", "minLength": 1, "maxLength": 255 }
                    }
                },
                "AddToProjectDisposition": {
                    "type": "string",
                    "enum": ["ready", "added", "already_member", "acknowledgement_required", "identity_mismatch"]
                },
                "AddToProjectItem": {
                    "type": "object",
                    "required": ["mediaObjectId", "projectAssetId", "verificationState", "disposition", "remoteOnly"],
                    "properties": {
                        "mediaObjectId": { "type": "string" },
                        "projectAssetId": { "type": ["string", "null"] },
                        "verificationState": { "$ref": "#/components/schemas/GlobalMediaVerificationState" },
                        "disposition": { "$ref": "#/components/schemas/AddToProjectDisposition" },
                        "remoteOnly": { "type": "boolean" }
                    }
                },
                "AddToProjectPreview": {
                    "type": "object",
                    "required": ["projectId", "selectedCount", "readyCount", "alreadyMemberCount", "acknowledgementRequiredCount", "mismatchCount", "items"],
                    "properties": {
                        "projectId": { "type": "string" },
                        "selectedCount": { "type": "integer", "minimum": 0 },
                        "readyCount": { "type": "integer", "minimum": 0 },
                        "alreadyMemberCount": { "type": "integer", "minimum": 0 },
                        "acknowledgementRequiredCount": { "type": "integer", "minimum": 0 },
                        "mismatchCount": { "type": "integer", "minimum": 0 },
                        "items": { "type": "array", "items": { "$ref": "#/components/schemas/AddToProjectItem" } }
                    }
                },
                "AddToProjectResult": {
                    "type": "object",
                    "required": ["projectId", "idempotencyKey", "addedCount", "alreadyMemberCount", "blockedCount", "items"],
                    "properties": {
                        "projectId": { "type": "string" },
                        "idempotencyKey": { "type": "string" },
                        "addedCount": { "type": "integer", "minimum": 0 },
                        "alreadyMemberCount": { "type": "integer", "minimum": 0 },
                        "blockedCount": { "type": "integer", "minimum": 0 },
                        "items": { "type": "array", "items": { "$ref": "#/components/schemas/AddToProjectItem" } }
                    }
                },
                "ReviewRepresentation": {
                    "type": "object",
                    "required": ["representationId", "state", "availableOnServer"],
                    "properties": {
                        "representationId": { "type": "string" },
                        "state": { "type": "string" },
                        "availableOnServer": { "type": "boolean" }
                    }
                },
                "ProjectReviewAsset": {
                    "allOf": [
                        { "$ref": "#/components/schemas/ProjectAssetItem" },
                        { "type": "object", "required": ["materializedRevision", "activeNoteRevisionId", "noteRevisionCount", "proxy", "thumbnail", "originalAvailability", "unresolvedConflictCount"], "properties": {
                            "materializedRevision": { "type": "integer", "minimum": 0 },
                            "activeNoteRevisionId": { "type": ["string", "null"] },
                            "noteRevisionCount": { "type": "integer", "minimum": 0 },
                            "proxy": { "oneOf": [{ "$ref": "#/components/schemas/ReviewRepresentation" }, { "type": "null" }] },
                            "thumbnail": { "oneOf": [{ "$ref": "#/components/schemas/ReviewRepresentation" }, { "type": "null" }] },
                            "originalAvailability": { "oneOf": [{ "$ref": "#/components/schemas/NodeAvailabilityState" }, { "type": "null" }] },
                            "unresolvedConflictCount": { "type": "integer", "minimum": 0 }
                        } }
                    ]
                },
                "ReviewMutationResult": {
                    "type": "object",
                    "required": ["asset", "staleBaseRevision"],
                    "properties": {
                        "asset": { "$ref": "#/components/schemas/ProjectReviewAsset" },
                        "staleBaseRevision": { "type": "boolean" }
                    }
                },
                "NoteMutationRequest": { "type": "object", "required": ["text", "baseRevision"], "properties": { "text": { "type": "string", "maxLength": 100000 }, "baseRevision": { "type": "integer", "minimum": 0 } } },
                "LabelMutationRequest": { "type": "object", "required": ["label", "baseRevision"], "properties": { "label": { "type": "string" }, "baseRevision": { "type": "integer", "minimum": 0 } } },
                "RemovedMutationRequest": { "type": "object", "required": ["removed", "baseRevision"], "properties": { "removed": { "type": "boolean" }, "baseRevision": { "type": "integer", "minimum": 0 } } },
                "RotationMutationRequest": { "type": "object", "required": ["rotationDegrees", "baseRevision"], "properties": { "rotationDegrees": { "type": "integer", "enum": [0, 90, 180, 270] }, "baseRevision": { "type": "integer", "minimum": 0 } } }
            },
            "requestBodies": {
                "AdminPayload": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/AdminPayload" } } } }
            },
            "responses": {
                "AdminPayload": { "description": "Authenticated administrative projection or canonical mutation result", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/AdminPayload" } } } },
                "Version": { "description": "Server identity and protocol", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Version" } } } },
                "Readiness": { "description": "Readiness report", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Readiness" } } } },
                "SessionCreated": { "description": "Browser session created", "headers": { "x-csrf-token": { "schema": { "type": "string" } } }, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SessionCreated" } } } },
                "Sessions": { "description": "Browser sessions", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Sessions" } } } },
                "LibrarySearchPage": { "description": "A page of the global media library search", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/LibrarySearchPage" } } } },
                "LibraryMediaDetail": { "description": "One media object with its search facts and node-relative availability", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/LibraryMediaDetail" } } } },
                "ProjectPage": { "description": "A page of registered project replicas", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ProjectPage" } } } },
                "ProjectAssetPage": { "description": "A page of one project's assets", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ProjectAssetPage" } } } },
                "AddToProjectPreview": { "description": "A non-mutating preview of add-to-project outcomes", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/AddToProjectPreview" } } } },
                "AddToProjectResult": { "description": "Canonical add-to-project outcomes", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/AddToProjectResult" } } } },
                "ProjectReviewAsset": { "description": "Canonical project asset review projection", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ProjectReviewAsset" } } } },
                "ReviewMutationResult": { "description": "Canonical asset after an operation-backed review mutation", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ReviewMutationResult" } } } },
                "Error": { "description": "Stable server error", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Error" } } } }
            }
        }
    })
}

async fn openapi() -> Json<serde_json::Value> {
    Json(openapi_document())
}

async fn web_or_not_found(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    method: Method,
) -> Response {
    if uri.path() == "/api" || uri.path().starts_with("/api/") {
        return not_found(Extension(request_id)).await.into_response();
    }
    if method != Method::GET && method != Method::HEAD {
        return not_found(Extension(request_id)).await.into_response();
    }
    if uri.path() == "/" && state.web_root.is_none() {
        return root(Extension(request_id)).await.into_response();
    }
    let Some(web_root) = state.web_root else {
        return not_found(Extension(request_id)).await.into_response();
    };
    let requested = uri.path().trim_start_matches('/');
    let safe_path = Path::new(requested);
    if safe_path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
        && !requested.is_empty()
    {
        return not_found(Extension(request_id)).await.into_response();
    }
    let candidate = if requested.is_empty() {
        web_root.join("index.html")
    } else {
        web_root.join(safe_path)
    };
    let (path, is_index) = if candidate.is_file() {
        let is_index = candidate
            .file_name()
            .is_some_and(|name| name == "index.html");
        (candidate, is_index)
    } else if safe_path.extension().is_none() {
        (web_root.join("index.html"), true)
    } else {
        return not_found(Extension(request_id)).await.into_response();
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let mut response = Response::new(Body::from(bytes));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(content_type(&path)),
            );
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static(if is_index {
                    "no-cache"
                } else {
                    "public, max-age=31536000, immutable"
                }),
            );
            response
        }
        Err(_) => not_found(Extension(request_id)).await.into_response(),
    }
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|value| value.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("map" | "json") => "application/json",
        _ => "application/octet-stream",
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RepresentationResponse {
    request_id: String,
    representation: Representation,
    #[serde(skip_serializing_if = "Option::is_none")]
    job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reused: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueryRepresentationsRequest {
    media_object_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryRepresentationsResponse {
    request_id: String,
    representations: Vec<Representation>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BaselinePreviewResponse {
    request_id: String,
    preview: BaselinePreview,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BaselineImportResponse {
    request_id: String,
    replica: ProjectReplica,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReconcileHttpResponse {
    request_id: String,
    #[serde(flatten)]
    reconciliation: ReconcileResponse,
}

// ── Auth route handlers ──────────────────────────────────────────────

async fn create_bootstrap_code(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    match state.auth.create_bootstrap_code() {
        Ok(bootstrap) => {
            tracing::info!(
                component = "auth",
                requestId = %request_id.0,
                codePrefix = %&bootstrap.code.expose()[..8.min(bootstrap.code.expose().len())],
                "bootstrap_code_created"
            );
            Json(BootstrapCodeResponse {
                request_id: request_id.0,
                code: bootstrap.code.expose().to_string(),
                expires_at: bootstrap.expires_at,
            })
            .into_response()
        }
        Err(error) => auth_error_response(error),
    }
}

async fn exchange_bootstrap_code(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Json(body): Json<BootstrapExchangeRequest>,
) -> Response {
    let hostname = body.hostname.as_deref().unwrap_or("rcdvlog.local");
    match state.auth.exchange_bootstrap(&body.code, hostname) {
        Ok(session) => {
            let cookie_value = format!(
                "rcdvlog_session={}:{}",
                session.session_id,
                session.session_token.expose()
            );
            let set_cookie = format!(
                "{}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=86400",
                cookie_value
            );
            let mut response = Json(SessionCreatedResponse {
                request_id: request_id.0,
                session_id: session.session_id,
                display_prefix: session.display_prefix,
                expires_at: session.expires_at,
            })
            .into_response();
            response.headers_mut().insert(
                header::SET_COOKIE,
                HeaderValue::from_str(&set_cookie).expect("valid cookie header"),
            );
            response.headers_mut().insert(
                header::HeaderName::from_static("x-csrf-token"),
                HeaderValue::from_str(&session.csrf_token).expect("valid csrf token"),
            );
            response
        }
        Err(error) => auth_error_response(error),
    }
}

async fn list_sessions(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    match state.auth.list_sessions() {
        Ok(sessions) => Json(SessionsListResponse {
            request_id: request_id.0,
            sessions,
        })
        .into_response(),
        Err(error) => auth_error_response(error),
    }
}

async fn revoke_session(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(session_id): AxumPath<String>,
) -> Response {
    match state.auth.revoke_session(&session_id) {
        Ok(()) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "session.revoked",
            "browser_session",
            Some(&session_id),
            serde_json::json!({}),
        ) {
            Ok(()) => {
                let current = matches!(&principal, AuthPrincipal::Browser { session_id: current, .. } if current == &session_id);
                let mut response = Json(ErrorResponse {
                    request_id: request_id.0,
                    code: "session_revoked",
                })
                .into_response();
                if current {
                    response.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_static("rcdvlog_session=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0"));
                }
                response
            }
            Err(response) => *response,
        },
        Err(error) => auth_error_response(error),
    }
}

async fn refresh_session(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
) -> Response {
    let session_id = match &principal {
        AuthPrincipal::Browser { session_id, .. } => session_id.clone(),
        _ => return auth_error_response(AuthError::SessionInvalid),
    };
    match state.auth.refresh_session(&session_id) {
        Ok(session) => Json(SessionRefreshResponse {
            request_id: request_id.0,
            session,
        })
        .into_response(),
        Err(error) => auth_error_response(error),
    }
}

async fn create_pairing_code(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
) -> Response {
    let session_id = match &principal {
        AuthPrincipal::Browser { session_id, .. } => session_id.clone(),
        _ => return auth_error_response(AuthError::SessionInvalid),
    };
    match state.auth.create_pairing_code(&session_id) {
        Ok(pairing) => Json(PairingCodeResponse {
            request_id: request_id.0,
            code: pairing.code.expose().to_string(),
            expires_at: pairing.expires_at,
        })
        .into_response(),
        Err(error) => auth_error_response(error),
    }
}

async fn exchange_pairing_code(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Json(body): Json<PairingExchangeRequest>,
) -> Response {
    match state.auth.exchange_pairing(&body.code, &body.label) {
        Ok(device) => Json(DeviceCreatedResponse {
            request_id: request_id.0,
            credential_id: device.credential_id,
            token: device.token.expose().to_string(),
            token_prefix: device.token_prefix,
        })
        .into_response(),
        Err(error) => auth_error_response(error),
    }
}

async fn list_devices(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    match state.auth.list_devices() {
        Ok(devices) => Json(DevicesListResponse {
            request_id: request_id.0,
            devices,
        })
        .into_response(),
        Err(error) => auth_error_response(error),
    }
}

async fn revoke_device(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(credential_id): AxumPath<String>,
) -> Response {
    match state.auth.revoke_device(&credential_id) {
        Ok(()) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "device.revoked",
            "device_credential",
            Some(&credential_id),
            serde_json::json!({}),
        ) {
            Ok(()) => Json(ErrorResponse {
                request_id: request_id.0,
                code: "device_revoked",
            })
            .into_response(),
            Err(response) => *response,
        },
        Err(error) => auth_error_response(error),
    }
}

async fn auth_diagnostics(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    match state.auth.diagnostics() {
        Ok(diag) => Json(AuthDiagnosticsResponse {
            request_id: request_id.0,
            diagnostics: diag,
        })
        .into_response(),
        Err(error) => auth_error_response(error),
    }
}

async fn preview_project_baseline(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Json(envelope): Json<BaselineEnvelope>,
) -> Response {
    let manager = state.foundation.catalog().project_replicas();
    match tokio::task::spawn_blocking(move || manager.preview(&envelope)).await {
        Ok(Ok(preview)) => Json(BaselinePreviewResponse {
            request_id: request_id.0,
            preview,
        })
        .into_response(),
        Ok(Err(error)) => project_replica_error(request_id.0, error),
        Err(_) => internal_project_replica_error(request_id.0),
    }
}

async fn import_project_baseline(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Json(envelope): Json<BaselineEnvelope>,
) -> Response {
    let manager = state.foundation.catalog().project_replicas();
    match tokio::task::spawn_blocking(move || manager.import_baseline(&envelope)).await {
        Ok(Ok(replica)) => {
            let status = if replica.reused_existing {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            (
                status,
                Json(BaselineImportResponse {
                    request_id: request_id.0,
                    replica,
                }),
            )
                .into_response()
        }
        Ok(Err(error)) => project_replica_error(request_id.0, error),
        Err(_) => internal_project_replica_error(request_id.0),
    }
}

async fn reconcile_project(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(project_id): AxumPath<String>,
    Json(request): Json<ReconcileRequest>,
) -> Response {
    let manager = state.foundation.catalog().project_replicas();
    let ws_channels = state.ws_channels.clone();
    let jobs = state.foundation.jobs();
    let server_node_id = state.foundation.catalog().node_id().clone();
    match tokio::task::spawn_blocking(move || {
        let mut reconciliation = manager.reconcile(&project_id, &request)?;

        // Query pending Mac-targeted jobs for the requesting node.
        // The requesting node is the origin of the sent operations.
        let requestor_node_id: Option<NodeId> = request
            .operation_cursors
            .keys()
            .find(|k| *k != server_node_id.as_str())
            .and_then(|id| NodeId::parse(id).ok());

        if let (Some(requestor), Ok(project_id_parsed)) =
            (requestor_node_id, ProjectId::parse(&project_id))
            && let Ok(pending) = jobs.pending_jobs_for_node(&requestor, &project_id_parsed)
        {
            reconciliation.node_targeted_jobs = pending;
        }

        Ok(reconciliation)
    })
    .await
    {
        Ok(Ok(reconciliation)) => {
            // Broadcast a hint to WebSocket subscribers so other clients
            // wake up and reconcile via HTTP.
            let hint = serde_json::json!({
                "type": "hint",
                "projectId": reconciliation.project_id,
                "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            });
            ws_channels.broadcast(&reconciliation.project_id, &hint.to_string());
            Json(ReconcileHttpResponse {
                request_id: request_id.0,
                reconciliation,
            })
            .into_response()
        }
        Ok(Err(error)) => project_replica_error(request_id.0, error),
        Err(_) => internal_project_replica_error(request_id.0),
    }
}

fn internal_project_replica_error(request_id: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            request_id,
            code: "internal_error",
        }),
    )
        .into_response()
}

// ── WebSocket hint handler ──────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubscribeMessage {
    #[serde(rename = "type")]
    msg_type: String,
    project_id: String,
}

async fn ws_events_handler(
    ws: WebSocketUpgrade,
    State(state): State<HttpState>,
    Extension(_principal): Extension<AuthPrincipal>,
) -> Response {
    // Both browser sessions and device credentials receive non-canonical change
    // hints. Consumers must refetch the authenticated REST projection.
    ws.on_upgrade(move |socket| handle_ws_socket(socket, state))
}

async fn handle_ws_socket(socket: WebSocket, state: HttpState) {
    let (mut sender, mut receiver) = socket.split();

    // First message MUST be a subscribe message:
    //   {"type":"subscribe","projectId":"proj_..."}
    let first_msg = match receiver.next().await {
        Some(Ok(WsMessage::Text(text))) => text.to_string(),
        _ => return,
    };

    let subscribe: SubscribeMessage = match serde_json::from_str::<SubscribeMessage>(&first_msg) {
        Ok(msg) if msg.msg_type == "subscribe" => msg,
        _ => return,
    };

    let mut rx = state.ws_channels.subscribe(&subscribe.project_id);

    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(hint) => {
                        if sender.send(WsMessage::Text(hint.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            ws_msg = receiver.next() => {
                match ws_msg {
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Ok(WsMessage::Text(text))) => {
                        // Allow resubscribe to a different project
                        if let Ok(sub) = serde_json::from_str::<SubscribeMessage>(&text)
                            && sub.msg_type == "subscribe"
                        {
                            rx = state.ws_channels.subscribe(&sub.project_id);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn project_replica_error(request_id: String, error: ProjectReplicaError) -> Response {
    let status = match &error {
        ProjectReplicaError::InvalidBaseline(_)
        | ProjectReplicaError::UnsafeReplicaPath
        | ProjectReplicaError::Sync(_) => StatusCode::BAD_REQUEST,
        ProjectReplicaError::DifferentBaseline { .. } => StatusCode::CONFLICT,
        ProjectReplicaError::NotFound { .. } => StatusCode::NOT_FOUND,
        ProjectReplicaError::Storage(_)
        | ProjectReplicaError::Infrastructure(_)
        | ProjectReplicaError::Database(_)
        | ProjectReplicaError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ErrorResponse {
            request_id,
            code: error.code(),
        }),
    )
        .into_response()
}

async fn request_representation(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Json(request): Json<RepresentationRequest>,
) -> Response {
    let service = state
        .foundation
        .catalog()
        .representations(state.foundation.config().derived_dir.clone());
    match tokio::task::spawn_blocking(move || service.request(request)).await {
        Ok(Ok(result)) => {
            let status = if result.reused {
                StatusCode::OK
            } else {
                StatusCode::ACCEPTED
            };
            (
                status,
                Json(RepresentationResponse {
                    request_id: request_id.0,
                    representation: result.representation,
                    job_id: result.job_id.map(|id| id.to_string()),
                    reused: Some(result.reused),
                }),
            )
                .into_response()
        }
        Ok(Err(error)) => representation_error(request_id.0, error),
        Err(_) => representation_error(
            request_id.0,
            RepresentationError::InvalidInput("request worker failed".into()),
        ),
    }
}

async fn get_representation(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(raw_id): AxumPath<String>,
) -> Response {
    let Ok(id) = RepresentationId::parse(raw_id) else {
        return representation_error(request_id.0, RepresentationError::NotFound);
    };
    let service = state
        .foundation
        .catalog()
        .representations(state.foundation.config().derived_dir.clone());
    match tokio::task::spawn_blocking(move || service.get(&id)).await {
        Ok(Ok(representation)) => Json(RepresentationResponse {
            request_id: request_id.0,
            representation,
            job_id: None,
            reused: None,
        })
        .into_response(),
        Ok(Err(error)) => representation_error(request_id.0, error),
        Err(_) => representation_error(request_id.0, RepresentationError::NotFound),
    }
}

async fn query_representations(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Json(body): Json<QueryRepresentationsRequest>,
) -> Response {
    let service = state
        .foundation
        .catalog()
        .representations(state.foundation.config().derived_dir.clone());
    let media_ids: Vec<MediaObjectId> = match body
        .media_object_ids
        .iter()
        .map(MediaObjectId::parse)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(ids) => ids,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    request_id: request_id.0,
                    code: "invalid_media_object_id",
                }),
            )
                .into_response();
        }
    };
    match tokio::task::spawn_blocking(move || service.list_available(&media_ids)).await {
        Ok(Ok(representations)) => Json(QueryRepresentationsResponse {
            request_id: request_id.0,
            representations,
        })
        .into_response(),
        Ok(Err(error)) => representation_error(request_id.0, error),
        Err(_) => representation_error(
            request_id.0,
            RepresentationError::InvalidInput("query worker failed".into()),
        ),
    }
}

// ── Node-targeted job completion ─────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompleteNodeJobRequest {
    status: String,
    #[serde(default)]
    output_identity: Option<String>,
    #[serde(default)]
    details: serde_json::Value,
}

async fn complete_node_job(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(job_id): AxumPath<String>,
    Json(body): Json<CompleteNodeJobRequest>,
) -> Response {
    // Only device principals may report job results
    let is_device = matches!(&principal, AuthPrincipal::Device { .. });
    if !is_device {
        return (
            StatusCode::FORBIDDEN,
            Json(ErrorResponse {
                request_id: request_id.0,
                code: "forbidden",
            }),
        )
            .into_response();
    }

    let Ok(job_id_parsed) = JobId::parse(&job_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                request_id: request_id.0,
                code: "invalid_job_id",
            }),
        )
            .into_response();
    };

    let jobs = state.foundation.jobs();
    let server_node_id = state.foundation.catalog().node_id().clone();
    let representations = state
        .foundation
        .catalog()
        .representations(state.foundation.config().derived_dir.clone());

    let status = body.status.clone();
    let output_identity = body.output_identity.clone();
    let details = body.details.clone();

    // Run the completion + representation registration on a blocking thread
    let result = tokio::task::spawn_blocking(move || {
        let completion = match status.as_str() {
            "succeeded" => NodeJobCompletion::Succeeded {
                output_identity: output_identity.clone(),
                details: details.clone(),
            },
            "failed" => NodeJobCompletion::Failed {
                code: "node_reported_failure".to_string(),
                details: details.clone(),
            },
            _ => {
                return Err(RepresentationError::InvalidInput(format!(
                    "unknown status: {status}",
                )));
            }
        };

        jobs.complete_node_targeted_job(&job_id_parsed, &server_node_id, &completion)
            .map_err(|e| RepresentationError::InvalidInput(e.to_string()))?;

        // If succeeded and the job was a representation generation, register the output
        if status == "succeeded"
            && let Some(ref output_id) = output_identity
            && let Ok(rep_id) = RepresentationId::parse(output_id)
        {
            let hash = details
                .get("outputContentHash")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let byte_len = details
                .get("byteLength")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let local_path = details
                .get("localPath")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if byte_len > 0 && !local_path.is_empty() {
                let hash = if hash.is_empty() {
                    "remote_generated"
                } else {
                    hash
                };
                let _ = representations.register_remote_output(
                    &rep_id,
                    &server_node_id,
                    local_path,
                    hash,
                    byte_len,
                );
            }
        }
        Ok::<_, RepresentationError>(())
    })
    .await;

    match result {
        Ok(Ok(())) => Json(serde_json::json!({
            "requestId": request_id.0,
            "status": "ok"
        }))
        .into_response(),
        Ok(Err(error)) => representation_error(request_id.0, error),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                request_id: request_id.0,
                code: "internal_error",
            }),
        )
            .into_response(),
    }
}

async fn media_content(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(raw_id): AxumPath<String>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let Ok(id) = RepresentationId::parse(raw_id) else {
        return representation_error(request_id.0, RepresentationError::NotFound);
    };
    let service = state
        .foundation
        .catalog()
        .representations(state.foundation.config().derived_dir.clone());
    let lookup_id = id.clone();
    let lookup = tokio::task::spawn_blocking(move || {
        let representation = service.get(&lookup_id)?;
        let path = service.content_path(&lookup_id)?;
        Ok::<_, RepresentationError>((representation, path))
    })
    .await;
    let (representation, path) = match lookup {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => return representation_error(request_id.0, error),
        Err(_) => return representation_error(request_id.0, RepresentationError::NotFound),
    };
    let length = match tokio::fs::metadata(&path).await {
        Ok(metadata) => metadata.len(),
        Err(_) => return representation_error(request_id.0, RepresentationError::NotFound),
    };
    if representation.byte_length != Some(length) {
        return representation_error(request_id.0, RepresentationError::NotFound);
    }
    let selected = match parse_range(headers.get(header::RANGE), length) {
        Ok(value) => value,
        Err(()) => {
            let mut response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{length}")).expect("valid content-range"),
            );
            response
                .headers_mut()
                .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            return response;
        }
    };
    let (start, end, status) = selected.map_or(
        (0, length.saturating_sub(1), StatusCode::OK),
        |(start, end)| (start, end, StatusCode::PARTIAL_CONTENT),
    );
    let response_length = if length == 0 { 0 } else { end - start + 1 };
    let mut builder = Response::builder()
        .status(status)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, representation.content_type)
        .header(header::CONTENT_LENGTH, response_length.to_string())
        .header(header::ETAG, format!("\"{}\"", id.as_str()))
        .header(
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable",
        );
    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{length}"),
        );
    }
    if method == Method::HEAD {
        return builder.body(Body::empty()).expect("valid media headers");
    }
    let mut file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(_) => return representation_error(request_id.0, RepresentationError::NotFound),
    };
    if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return representation_error(request_id.0, RepresentationError::NotFound);
    }
    let stream = ReaderStream::new(file.take(response_length));
    builder
        .body(Body::from_stream(stream))
        .expect("valid media response")
}

fn parse_range(value: Option<&HeaderValue>, length: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(value) = value else { return Ok(None) };
    let value = value.to_str().map_err(|_| ())?;
    let spec = value.strip_prefix("bytes=").ok_or(())?;
    if spec.contains(',') || length == 0 {
        return Err(());
    }
    let (start, end) = spec.split_once('-').ok_or(())?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        let selected = suffix.min(length);
        return Ok(Some((length - selected, length - 1)));
    }
    let start = start.parse::<u64>().map_err(|_| ())?;
    if start >= length {
        return Err(());
    }
    let end = if end.is_empty() {
        length - 1
    } else {
        end.parse::<u64>().map_err(|_| ())?.min(length - 1)
    };
    if end < start {
        return Err(());
    }
    Ok(Some((start, end)))
}

fn representation_error(request_id: String, error: RepresentationError) -> Response {
    let status = match &error {
        RepresentationError::NotFound => StatusCode::NOT_FOUND,
        RepresentationError::InvalidInput(_) | RepresentationError::SourceNotVerified => {
            StatusCode::BAD_REQUEST
        }
        RepresentationError::MediaOffline | RepresentationError::DerivedRootUnavailable => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        RepresentationError::Database(_)
        | RepresentationError::Job(_)
        | RepresentationError::Infrastructure(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ErrorResponse {
            request_id,
            code: error.code(),
        }),
    )
        .into_response()
}

// ── Library and project browse ────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LibraryMediaParams {
    #[serde(default = "default_page_limit")]
    limit: u32,
    #[serde(default)]
    sort: Option<LibrarySort>,
    #[serde(default)]
    verification_state: Option<GlobalMediaVerificationState>,
    #[serde(default)]
    storage_root_id: Option<String>,
    #[serde(default)]
    camera: Option<String>,
    #[serde(default)]
    filename_prefix: Option<String>,
    #[serde(default)]
    availability: Option<NodeAvailabilityState>,
    #[serde(default)]
    after: Option<String>,
}

fn default_page_limit() -> u32 {
    50
}

fn query_error_response(
    request_id: &RequestId,
    code: &'static str,
    status: StatusCode,
) -> Response {
    (
        status,
        Json(ErrorResponse {
            request_id: request_id.0.clone(),
            code,
        }),
    )
        .into_response()
}

async fn list_library_media(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    params: Result<Query<LibraryMediaParams>, QueryRejection>,
) -> Response {
    let Query(params) = match params {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
        }
    };

    let storage_root_id = match params.storage_root_id.as_deref().map(StorageRootId::parse) {
        Some(Ok(value)) => Some(value),
        Some(Err(_)) => {
            return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
        }
        None => None,
    };

    let query = LibraryQuery {
        limit: params.limit,
        sort: params.sort.unwrap_or(LibrarySort::CreatedDesc),
        filters: LibraryFilters {
            verification_state: params.verification_state,
            storage_root_id,
            camera_folded: params.camera,
            filename_prefix: params.filename_prefix,
        },
        after: params.after,
    };

    match state
        .foundation
        .catalog()
        .storage_library()
        .search_media(query, params.availability)
    {
        Ok(page) => (StatusCode::OK, Json(page)).into_response(),
        Err(StorageLibraryError::Query(error)) => {
            query_error_response(&request_id, error.code(), StatusCode::BAD_REQUEST)
        }
        Err(StorageLibraryError::InvalidInput { .. }) => {
            query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST)
        }
        Err(StorageLibraryError::NotFound { .. }) => {
            query_error_response(&request_id, "not_found", StatusCode::NOT_FOUND)
        }
        Err(_) => query_error_response(
            &request_id,
            "internal_error",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

/// Combines the authoritative [`MediaObject`], its non-authoritative search
/// projection facts, and its node-relative availability into one detail
/// response. No field here is invented beyond what earlier tasks already
/// produce for a media object.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryMediaDetail {
    media: MediaObject,
    facts: MediaSearchFacts,
    availability: MediaAvailability,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LibraryMediaDetailParams {}

async fn get_library_media_detail(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(raw_media_object_id): AxumPath<String>,
    params: Result<Query<LibraryMediaDetailParams>, QueryRejection>,
) -> Response {
    if params.is_err() {
        return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
    }
    let Ok(media_object_id) = MediaObjectId::parse(raw_media_object_id) else {
        return query_error_response(&request_id, "not_found", StatusCode::NOT_FOUND);
    };
    let library = state.foundation.catalog().storage_library();
    let node_id = state.foundation.catalog().node_id().clone();

    let media = match library.get_media_object(&media_object_id) {
        Ok(media) => media,
        Err(error) => return library_error_response(&request_id, error),
    };
    let facts = match library.media_search_facts(&media_object_id) {
        Ok(facts) => facts,
        Err(error) => return library_error_response(&request_id, error),
    };
    let availability = match library.media_availability(&media_object_id, &node_id) {
        Ok(availability) => availability,
        Err(error) => return library_error_response(&request_id, error),
    };

    (
        StatusCode::OK,
        Json(LibraryMediaDetail {
            media,
            facts,
            availability,
        }),
    )
        .into_response()
}

fn library_error_response(request_id: &RequestId, error: StorageLibraryError) -> Response {
    match error {
        StorageLibraryError::Query(error) => {
            query_error_response(request_id, error.code(), StatusCode::BAD_REQUEST)
        }
        StorageLibraryError::InvalidInput { .. } => {
            query_error_response(request_id, "invalid_query", StatusCode::BAD_REQUEST)
        }
        StorageLibraryError::NotFound { .. } => {
            query_error_response(request_id, "not_found", StatusCode::NOT_FOUND)
        }
        _ => query_error_response(
            request_id,
            "internal_error",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListProjectsParams {
    #[serde(default = "default_page_limit")]
    limit: u32,
    #[serde(default)]
    after: Option<String>,
}

async fn list_projects(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    params: Result<Query<ListProjectsParams>, QueryRejection>,
) -> Response {
    let Query(params) = match params {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
        }
    };
    match state
        .foundation
        .catalog()
        .project_replicas()
        .list_projects(params.limit, params.after.as_deref())
    {
        Ok(page) => (StatusCode::OK, Json(page)).into_response(),
        Err(error) => project_read_error_response(&request_id, error),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProjectAssetParams {
    #[serde(default = "default_page_limit")]
    limit: u32,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    search: Option<String>,
}

async fn list_project_assets(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(project_id): AxumPath<String>,
    params: Result<Query<ProjectAssetParams>, QueryRejection>,
) -> Response {
    let Query(params) = match params {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
        }
    };
    match state
        .foundation
        .catalog()
        .project_replicas()
        .list_project_assets(
            &project_id,
            params.limit,
            params.after.as_deref(),
            params.search.as_deref(),
        ) {
        Ok(page) => (StatusCode::OK, Json(page)).into_response(),
        Err(error) => project_read_error_response(&request_id, error),
    }
}

async fn preview_add_to_project(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(project_id): AxumPath<String>,
    request: Result<Json<AddToProjectRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    match state
        .foundation
        .catalog()
        .project_replicas()
        .preview_add_to_project(&project_id, &request)
    {
        Ok(preview) => (StatusCode::OK, Json(preview)).into_response(),
        Err(error) => project_command_error_response(&request_id, error),
    }
}

async fn add_to_project(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(project_id): AxumPath<String>,
    request: Result<Json<AddToProjectRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    match state
        .foundation
        .catalog()
        .project_replicas()
        .add_media_to_project(&project_id, &request)
    {
        Ok(result) => {
            if result.added_count > 0 {
                state
                    .ws_channels
                    .broadcast(&project_id, "projectAssetsChanged");
            }
            (StatusCode::OK, Json(result)).into_response()
        }
        Err(error) => project_command_error_response(&request_id, error),
    }
}

fn project_command_error_response(request_id: &RequestId, error: ProjectReplicaError) -> Response {
    match error {
        ProjectReplicaError::NotFound { .. }
        | ProjectReplicaError::Storage(StorageLibraryError::NotFound { .. }) => {
            query_error_response(request_id, "not_found", StatusCode::NOT_FOUND)
        }
        ProjectReplicaError::InvalidBaseline(_) => {
            query_error_response(request_id, "invalid_input", StatusCode::BAD_REQUEST)
        }
        _ => query_error_response(
            request_id,
            "internal_error",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NoteMutationRequest {
    text: String,
    base_revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LabelMutationRequest {
    label: String,
    base_revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemovedMutationRequest {
    removed: bool,
    base_revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RotationMutationRequest {
    rotation_degrees: u16,
    base_revision: u64,
}

async fn get_project_review_asset(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, asset_id)): AxumPath<(String, String)>,
) -> Response {
    match state
        .foundation
        .catalog()
        .project_replicas()
        .get_review_asset(&project_id, &asset_id)
    {
        Ok(asset) => (StatusCode::OK, Json(asset)).into_response(),
        Err(error) => project_command_error_response(&request_id, error),
    }
}

async fn create_review_note(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, asset_id)): AxumPath<(String, String)>,
    request: Result<Json<NoteMutationRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    review_mutation_response(
        &state,
        &request_id,
        &project_id,
        state
            .foundation
            .catalog()
            .project_replicas()
            .create_review_note(&project_id, &asset_id, &request.text, request.base_revision),
    )
}

async fn add_review_label(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, asset_id)): AxumPath<(String, String)>,
    request: Result<Json<LabelMutationRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    review_mutation_response(
        &state,
        &request_id,
        &project_id,
        state
            .foundation
            .catalog()
            .project_replicas()
            .add_review_label(
                &project_id,
                &asset_id,
                &request.label,
                request.base_revision,
            ),
    )
}

async fn remove_review_label(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, asset_id)): AxumPath<(String, String)>,
    request: Result<Json<LabelMutationRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    review_mutation_response(
        &state,
        &request_id,
        &project_id,
        state
            .foundation
            .catalog()
            .project_replicas()
            .remove_review_label(
                &project_id,
                &asset_id,
                &request.label,
                request.base_revision,
            ),
    )
}

async fn set_review_removed(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, asset_id)): AxumPath<(String, String)>,
    request: Result<Json<RemovedMutationRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    review_mutation_response(
        &state,
        &request_id,
        &project_id,
        state
            .foundation
            .catalog()
            .project_replicas()
            .set_review_removed(
                &project_id,
                &asset_id,
                request.removed,
                request.base_revision,
            ),
    )
}

async fn set_review_rotation(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, asset_id)): AxumPath<(String, String)>,
    request: Result<Json<RotationMutationRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    review_mutation_response(
        &state,
        &request_id,
        &project_id,
        state
            .foundation
            .catalog()
            .project_replicas()
            .set_review_rotation(
                &project_id,
                &asset_id,
                request.rotation_degrees,
                request.base_revision,
            ),
    )
}

fn review_mutation_response(
    state: &HttpState,
    request_id: &RequestId,
    project_id: &str,
    result: Result<rcdvlog_server_core::ReviewMutationResult, ProjectReplicaError>,
) -> Response {
    match result {
        Ok(result) => {
            state
                .ws_channels
                .broadcast(project_id, "projectAssetChanged");
            (StatusCode::OK, Json(result)).into_response()
        }
        Err(error) => project_command_error_response(request_id, error),
    }
}

/// Error mapping for the read-only project browse endpoints (`GET
/// /api/v1/projects` and `GET /api/v1/projects/{projectId}/assets`).
/// Distinct from [`project_replica_error`], which maps the baseline
/// import/preview/reconcile write path onto its own, more granular set of
/// stable codes (`baseline_invalid`, `baseline_different`, ...). Those codes
/// describe a sync protocol violation; a read endpoint has no baseline to
/// violate, so it only ever needs three outcomes: the project does not
/// exist, the request itself was malformed (currently just an out-of-range
/// `limit`, which `list_project_assets` reports as `InvalidBaseline`), or
/// something unexpected happened.
fn project_read_error_response(request_id: &RequestId, error: ProjectReplicaError) -> Response {
    match error {
        ProjectReplicaError::NotFound { .. } => {
            query_error_response(request_id, "not_found", StatusCode::NOT_FOUND)
        }
        ProjectReplicaError::InvalidBaseline(_) => {
            query_error_response(request_id, "invalid_query", StatusCode::BAD_REQUEST)
        }
        _ => query_error_response(
            request_id,
            "internal_error",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

// ── Browser administration ─────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminOverviewResponse {
    application_version: &'static str,
    protocol_major: u32,
    protocol_minor: u32,
    control_schema_version: u32,
    node_id: String,
    readiness: ReadinessReport,
    config: rcdvlog_server_core::ConfigDiagnostics,
    auth: AuthDiagnostics,
    derived_available_bytes: Option<u64>,
    backup_available_bytes: Option<u64>,
}

async fn admin_overview(State(state): State<HttpState>) -> Response {
    let readiness = state.readiness_gate.evaluate().await;
    let auth = match state.auth.diagnostics() {
        Ok(value) => value,
        Err(error) => return auth_error_response(error),
    };
    Json(AdminOverviewResponse {
        application_version: APPLICATION_VERSION,
        protocol_major: PROTOCOL_MAJOR_VERSION,
        protocol_minor: PROTOCOL_MINOR_VERSION,
        control_schema_version: CONTROL_CATALOG_SCHEMA_VERSION,
        node_id: state.foundation.catalog().node_id().to_string(),
        readiness,
        config: state.foundation.config().diagnostics(),
        derived_available_bytes: fs2::available_space(&state.foundation.config().derived_dir).ok(),
        backup_available_bytes: fs2::available_space(&state.foundation.config().backup_dir).ok(),
        auth,
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AdminAuditQuery {
    #[serde(default = "default_admin_limit")]
    limit: u32,
    before: Option<u64>,
}

fn default_admin_limit() -> u32 {
    50
}

async fn list_admin_audit(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    query: Result<Query<AdminAuditQuery>, QueryRejection>,
) -> Response {
    let Query(query) = match query {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
        }
    };
    match state
        .foundation
        .catalog()
        .audit_log()
        .list(query.limit, query.before)
    {
        Ok(items) => Json(serde_json::json!({ "items": items })).into_response(),
        Err(_) => query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateStorageRootRequest {
    display_name: String,
    #[serde(default = "default_true")]
    immutable_original: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateStorageRootRequest {
    display_name: Option<String>,
    enabled: Option<bool>,
    #[serde(default)]
    confirm_disable: bool,
}

async fn list_admin_storage_roots(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    match state
        .foundation
        .catalog()
        .storage_library()
        .list_storage_roots(true)
    {
        Ok(items) => Json(serde_json::json!({ "items": items })).into_response(),
        Err(error) => storage_admin_error(&request_id, error),
    }
}

async fn create_admin_storage_root(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    request: Result<Json<CreateStorageRootRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    let id = match StorageRootId::parse(format!("root_{}", Uuid::new_v4().simple())) {
        Ok(id) => id,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    match state
        .foundation
        .catalog()
        .storage_library()
        .create_storage_root(NewStorageRoot {
            id: id.clone(),
            display_name: request.display_name.clone(),
            immutable_original: request.immutable_original,
        }) {
        Ok(root) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "storage_root.created",
            "storage_root",
            Some(id.as_str()),
            serde_json::json!({"displayName": request.display_name, "immutableOriginal": request.immutable_original}),
        ) {
            Ok(()) => (StatusCode::CREATED, Json(root)).into_response(),
            Err(response) => *response,
        },
        Err(error) => storage_admin_error(&request_id, error),
    }
}

async fn update_admin_storage_root(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(raw_id): AxumPath<String>,
    request: Result<Json<UpdateStorageRootRequest>, JsonRejection>,
) -> Response {
    let id = match StorageRootId::parse(raw_id) {
        Ok(id) => id,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    if request.enabled == Some(false) && !request.confirm_disable {
        return query_error_response(
            &request_id,
            "confirmation_required",
            StatusCode::BAD_REQUEST,
        );
    }
    match state
        .foundation
        .catalog()
        .storage_library()
        .update_storage_root(
            &id,
            StorageRootUpdate {
                display_name: request.display_name.clone(),
                enabled: request.enabled,
            },
        ) {
        Ok(root) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "storage_root.updated",
            "storage_root",
            Some(id.as_str()),
            serde_json::json!({"displayNameChanged": request.display_name.is_some(), "enabled": request.enabled}),
        ) {
            Ok(()) => Json(root).into_response(),
            Err(response) => *response,
        },
        Err(error) => storage_admin_error(&request_id, error),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpsertBindingRequest {
    local_mount_path: String,
    root_subpath: Option<RootRelativePath>,
    protocol: StorageProtocol,
    access_mode: StorageAccessMode,
    #[serde(default = "default_true")]
    enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConfirmBindingRequest {
    action: BindingConfirmationAction,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BindingConfirmationAction {
    Confirm,
    Reject,
}

async fn list_admin_storage_bindings(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    match state
        .foundation
        .catalog()
        .storage_library()
        .list_local_node_root_bindings_admin()
    {
        Ok(items) => Json(serde_json::json!({ "items": items })).into_response(),
        Err(error) => storage_admin_error(&request_id, error),
    }
}

async fn upsert_admin_storage_binding(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(raw_id): AxumPath<String>,
    request: Result<Json<UpsertBindingRequest>, JsonRejection>,
) -> Response {
    let root_id = match StorageRootId::parse(raw_id) {
        Ok(id) => id,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    let mount = PathBuf::from(&request.local_mount_path);
    if !admin_mount_is_allowed(state.foundation.config(), &mount) {
        return query_error_response(
            &request_id,
            "mount_path_not_allowed",
            StatusCode::BAD_REQUEST,
        );
    }
    let input = LocalNodeRootBindingInput {
        storage_root_id: root_id.clone(),
        local_mount_path: mount,
        root_subpath: request.root_subpath,
        protocol: request.protocol,
        access_mode: request.access_mode,
        verification_status: BindingVerificationStatus::Pending,
        enabled: request.enabled,
        association_evidence: None,
        confirmation: RootAssociationConfirmation {
            status: RootAssociationConfirmationStatus::Pending,
            confirmed_by_node_id: None,
            confirmed_at: None,
        },
    };
    match state
        .foundation
        .catalog()
        .storage_library()
        .upsert_local_node_root_binding(input)
    {
        Ok(binding) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "storage_binding.configured",
            "storage_root",
            Some(root_id.as_str()),
            serde_json::json!({"enabled": binding.enabled, "protocol": binding.protocol.as_str(), "accessMode": binding.access_mode.as_str(), "evidencePresent": binding.association_evidence.is_some()}),
        ) {
            Ok(()) => Json(binding).into_response(),
            Err(response) => *response,
        },
        Err(error) => storage_admin_error(&request_id, error),
    }
}

async fn verify_admin_storage_binding(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(raw_id): AxumPath<String>,
) -> Response {
    let root_id = match StorageRootId::parse(raw_id) {
        Ok(id) => id,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    let library = state.foundation.catalog().storage_library();
    let existing = match library.get_local_node_root_binding_admin(&root_id) {
        Ok(value) => value,
        Err(error) => return storage_admin_error(&request_id, error),
    };
    let evidence = match library.inspect_local_root_association(
        &root_id,
        &existing.local_mount_path,
        existing.root_subpath.as_ref(),
    ) {
        Ok(value) => value,
        Err(error) => return storage_admin_error(&request_id, error),
    };
    let verification_status = if evidence.mismatches == 0 && evidence.matching_relative_paths > 0 {
        BindingVerificationStatus::Pending
    } else {
        BindingVerificationStatus::Mismatch
    };
    let input = LocalNodeRootBindingInput {
        storage_root_id: root_id.clone(),
        local_mount_path: existing.local_mount_path,
        root_subpath: existing.root_subpath,
        protocol: existing.protocol,
        access_mode: existing.access_mode,
        verification_status,
        enabled: existing.enabled,
        association_evidence: Some(evidence.clone()),
        confirmation: RootAssociationConfirmation {
            status: RootAssociationConfirmationStatus::Pending,
            confirmed_by_node_id: None,
            confirmed_at: None,
        },
    };
    match library.upsert_local_node_root_binding(input) {
        Ok(binding) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "storage_binding.verified",
            "storage_root",
            Some(root_id.as_str()),
            serde_json::json!({"associationEvidence": evidence}),
        ) {
            Ok(()) => Json(binding).into_response(),
            Err(response) => *response,
        },
        Err(error) => storage_admin_error(&request_id, error),
    }
}

async fn confirm_admin_storage_binding(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(raw_id): AxumPath<String>,
    request: Result<Json<ConfirmBindingRequest>, JsonRejection>,
) -> Response {
    let root_id = match StorageRootId::parse(raw_id) {
        Ok(id) => id,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    let library = state.foundation.catalog().storage_library();
    let existing = match library.get_local_node_root_binding_admin(&root_id) {
        Ok(value) => value,
        Err(error) => return storage_admin_error(&request_id, error),
    };
    if matches!(request.action, BindingConfirmationAction::Confirm)
        && existing
            .association_evidence
            .as_ref()
            .is_none_or(|evidence| evidence.matching_relative_paths == 0 || evidence.mismatches > 0)
    {
        return query_error_response(
            &request_id,
            "association_evidence_required",
            StatusCode::BAD_REQUEST,
        );
    }
    let confirmed = matches!(request.action, BindingConfirmationAction::Confirm);
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let input = LocalNodeRootBindingInput {
        storage_root_id: root_id.clone(),
        local_mount_path: existing.local_mount_path,
        root_subpath: existing.root_subpath,
        protocol: existing.protocol,
        access_mode: existing.access_mode,
        verification_status: if confirmed {
            BindingVerificationStatus::Confirmed
        } else {
            BindingVerificationStatus::Rejected
        },
        enabled: existing.enabled,
        association_evidence: existing.association_evidence,
        confirmation: RootAssociationConfirmation {
            status: if confirmed {
                RootAssociationConfirmationStatus::Confirmed
            } else {
                RootAssociationConfirmationStatus::Rejected
            },
            confirmed_by_node_id: confirmed.then(|| state.foundation.catalog().node_id().clone()),
            confirmed_at: Some(now),
        },
    };
    match library.upsert_local_node_root_binding(input) {
        Ok(binding) => match audit_admin(
            &state,
            &principal,
            &request_id,
            if confirmed {
                "storage_binding.confirmed"
            } else {
                "storage_binding.rejected"
            },
            "storage_root",
            Some(root_id.as_str()),
            serde_json::json!({"evidence": binding.association_evidence}),
        ) {
            Ok(()) => Json(binding).into_response(),
            Err(response) => *response,
        },
        Err(error) => storage_admin_error(&request_id, error),
    }
}

fn admin_mount_is_allowed(config: &rcdvlog_server_core::ServerConfig, mount: &Path) -> bool {
    if !mount.is_absolute()
        || mount
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return false;
    }
    let Ok(resolved_mount) = mount.canonicalize() else {
        return false;
    };
    config.allowed_original_mounts.iter().any(|allowed| {
        allowed
            .canonicalize()
            .is_ok_and(|resolved_allowed| resolved_mount.starts_with(resolved_allowed))
    })
}

fn storage_admin_error(request_id: &RequestId, error: StorageLibraryError) -> Response {
    match error {
        StorageLibraryError::InvalidInput { .. } | StorageLibraryError::Path(_) => {
            query_error_response(request_id, "invalid_input", StatusCode::BAD_REQUEST)
        }
        StorageLibraryError::NotFound { .. } => {
            query_error_response(request_id, "not_found", StatusCode::NOT_FOUND)
        }
        _ => query_error_response(
            request_id,
            "internal_error",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

fn admin_scan_service(state: &HttpState) -> ScanService {
    ScanService::new(
        state.foundation.catalog().storage_library(),
        ScanToolConfig {
            ffprobe_program: PathBuf::from(&state.foundation.config().ffprobe_command),
            exiftool_program: (!state.foundation.config().exiftool_command.is_empty())
                .then(|| PathBuf::from(&state.foundation.config().exiftool_command)),
        },
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AdminScanQuery {
    #[serde(default = "default_admin_limit")]
    limit: u32,
    after: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RequestScanRequest {
    storage_root_id: StorageRootId,
    scope_relative_path: Option<RootRelativePath>,
    idempotency_key: String,
}

async fn list_admin_scans(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    query: Result<Query<AdminScanQuery>, QueryRejection>,
) -> Response {
    let Query(query) = match query {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
        }
    };
    match admin_scan_service(&state).list_scans(query.limit, query.after.as_deref()) {
        Ok(items) => Json(serde_json::json!({"items":items})).into_response(),
        Err(error) => scan_admin_error(&request_id, error),
    }
}

async fn request_admin_scan(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    request: Result<Json<RequestScanRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    if request.idempotency_key.trim().is_empty() || request.idempotency_key.len() > 255 {
        return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
    }
    match admin_scan_service(&state).request_scan_idempotent(
        &request.storage_root_id,
        request.scope_relative_path.as_ref(),
        &request.idempotency_key,
    ) {
        Ok(scan) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "scan.requested",
            "scan",
            Some(&scan.scan_id),
            serde_json::json!({"storageRootId": request.storage_root_id, "scopeRelativePath": request.scope_relative_path, "idempotencyKey": request.idempotency_key}),
        ) {
            Ok(()) => (StatusCode::ACCEPTED, Json(scan)).into_response(),
            Err(response) => *response,
        },
        Err(error) => scan_admin_error(&request_id, error),
    }
}

async fn list_admin_scan_diagnostics(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(scan_id): AxumPath<String>,
    query: Result<Query<AdminScanQuery>, QueryRejection>,
) -> Response {
    let Query(query) = match query {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
        }
    };
    let service = admin_scan_service(&state);
    if let Err(error) = service.get_scan(&scan_id) {
        return scan_admin_error(&request_id, error);
    }
    match service.list_scan_diagnostics(&scan_id, query.limit, query.after.as_deref()) {
        Ok(items) => Json(serde_json::json!({"items":items})).into_response(),
        Err(error) => scan_admin_error(&request_id, error),
    }
}

async fn list_admin_scan_schedules(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    match admin_scan_service(&state).list_schedules() {
        Ok(items) => Json(serde_json::json!({"items":items})).into_response(),
        Err(error) => scan_admin_error(&request_id, error),
    }
}

async fn create_admin_scan_schedule(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    request: Result<Json<NewScanSchedule>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    match admin_scan_service(&state).create_schedule(request) {
        Ok(schedule) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "scan_schedule.created",
            "scan_schedule",
            Some(&schedule.schedule_id),
            serde_json::json!({"timezone":schedule.timezone,"localTime":schedule.local_time,"enabled":schedule.enabled}),
        ) {
            Ok(()) => (StatusCode::CREATED, Json(schedule)).into_response(),
            Err(response) => *response,
        },
        Err(error) => scan_admin_error(&request_id, error),
    }
}

async fn update_admin_scan_schedule(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(schedule_id): AxumPath<String>,
    request: Result<Json<ScanScheduleUpdate>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    match admin_scan_service(&state).update_schedule(&schedule_id, request) {
        Ok(schedule) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "scan_schedule.updated",
            "scan_schedule",
            Some(&schedule_id),
            serde_json::json!({"timezone":schedule.timezone,"localTime":schedule.local_time,"enabled":schedule.enabled}),
        ) {
            Ok(()) => Json(schedule).into_response(),
            Err(response) => *response,
        },
        Err(error) => scan_admin_error(&request_id, error),
    }
}

fn scan_admin_error(request_id: &RequestId, error: ScanServiceError) -> Response {
    match error {
        ScanServiceError::InvalidInput { .. } => {
            query_error_response(request_id, "invalid_input", StatusCode::BAD_REQUEST)
        }
        ScanServiceError::NotFound { .. } => {
            query_error_response(request_id, "not_found", StatusCode::NOT_FOUND)
        }
        ScanServiceError::ScanAlreadyActive => {
            query_error_response(request_id, "scan_already_active", StatusCode::CONFLICT)
        }
        ScanServiceError::IdempotencyConflict => {
            query_error_response(request_id, "idempotency_conflict", StatusCode::CONFLICT)
        }
        _ => query_error_response(
            request_id,
            "internal_error",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AdminJobsQuery {
    #[serde(default = "default_admin_limit")]
    limit: u32,
    before: Option<u64>,
    state: Option<JobState>,
}

async fn list_admin_jobs(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    query: Result<Query<AdminJobsQuery>, QueryRejection>,
) -> Response {
    let Query(query) = match query {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
        }
    };
    match state
        .foundation
        .jobs()
        .list_jobs(query.limit, query.before, query.state)
    {
        Ok(page) => Json(page).into_response(),
        Err(error) => job_admin_error(&request_id, error),
    }
}

async fn list_admin_sync(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    match state
        .foundation
        .catalog()
        .project_replicas()
        .list_sync_overview()
    {
        Ok(items) => Json(serde_json::json!({"items":items})).into_response(),
        Err(error) => project_read_error_response(&request_id, error),
    }
}

async fn get_admin_job(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(raw_id): AxumPath<String>,
) -> Response {
    let id = match JobId::parse(raw_id) {
        Ok(id) => id,
        Err(_) => return query_error_response(&request_id, "not_found", StatusCode::NOT_FOUND),
    };
    match state.foundation.jobs().get_job(&id) {
        Ok(job) => Json(job).into_response(),
        Err(error) => job_admin_error(&request_id, error),
    }
}

async fn cancel_admin_job(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(raw_id): AxumPath<String>,
) -> Response {
    let id = match JobId::parse(raw_id) {
        Ok(id) => id,
        Err(_) => return query_error_response(&request_id, "not_found", StatusCode::NOT_FOUND),
    };
    match state
        .foundation
        .jobs()
        .request_cancel(&id, Some("browser administration request"))
    {
        Ok(()) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "job.cancel_requested",
            "job",
            Some(id.as_str()),
            serde_json::json!({}),
        ) {
            Ok(()) => match state.foundation.jobs().get_job(&id) {
                Ok(job) => Json(job).into_response(),
                Err(error) => job_admin_error(&request_id, error),
            },
            Err(response) => *response,
        },
        Err(error) => job_admin_error(&request_id, error),
    }
}

async fn retry_admin_job(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(raw_id): AxumPath<String>,
) -> Response {
    let id = match JobId::parse(raw_id) {
        Ok(id) => id,
        Err(_) => return query_error_response(&request_id, "not_found", StatusCode::NOT_FOUND),
    };
    match state.foundation.jobs().retry_failed(&id) {
        Ok(job) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "job.retry_queued",
            "job",
            Some(id.as_str()),
            serde_json::json!({"attemptCount":job.attempt_count}),
        ) {
            Ok(()) => Json(job).into_response(),
            Err(response) => *response,
        },
        Err(error) => job_admin_error(&request_id, error),
    }
}

fn job_admin_error(request_id: &RequestId, error: JobEngineError) -> Response {
    match error {
        JobEngineError::InvalidInput(_) => {
            query_error_response(request_id, "invalid_input", StatusCode::BAD_REQUEST)
        }
        JobEngineError::NotFound(_) => {
            query_error_response(request_id, "not_found", StatusCode::NOT_FOUND)
        }
        JobEngineError::InvalidState(_) => {
            query_error_response(request_id, "job_not_retryable", StatusCode::CONFLICT)
        }
        _ => query_error_response(
            request_id,
            "internal_error",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepresentationBatchRequest {
    media_object_ids: Vec<MediaObjectId>,
    kind: RepresentationKind,
    idempotency_key: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RepresentationBatchResult {
    selected_count: usize,
    reused_count: usize,
    queued_count: usize,
    succeeded_count: usize,
    failed_count: usize,
    skipped_count: usize,
    skipped: Vec<RepresentationBatchSkip>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RepresentationBatchSkip {
    media_object_id: MediaObjectId,
    reason: String,
}

async fn request_admin_representation_batch(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    request: Result<Json<RepresentationBatchRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    if request.media_object_ids.is_empty()
        || request.media_object_ids.len() > 200
        || request.idempotency_key.is_empty()
        || request.idempotency_key.len() > 255
    {
        return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
    }
    let service = state
        .foundation
        .catalog()
        .representations(state.foundation.config().derived_dir.clone());
    let mut reused_count = 0;
    let mut queued_count = 0;
    let mut succeeded_count = 0;
    let mut skipped = Vec::new();
    for media_object_id in &request.media_object_ids {
        match service.request(RepresentationRequest {
            media_object_id: media_object_id.clone(),
            kind: request.kind,
        }) {
            Ok(result) if result.reused => {
                reused_count += 1;
                succeeded_count += 1;
            }
            Ok(_) => queued_count += 1,
            Err(error) => skipped.push(RepresentationBatchSkip {
                media_object_id: media_object_id.clone(),
                reason: error.code().to_string(),
            }),
        }
    }
    let result = RepresentationBatchResult {
        selected_count: request.media_object_ids.len(),
        reused_count,
        queued_count,
        succeeded_count,
        failed_count: 0,
        skipped_count: skipped.len(),
        skipped,
    };
    match audit_admin(
        &state,
        &principal,
        &request_id,
        "representation_batch.requested",
        "representation_batch",
        None,
        serde_json::json!({"kind":match request.kind { RepresentationKind::Proxy => "proxy", RepresentationKind::Thumbnail => "thumbnail" },"selectedCount":result.selected_count,"reusedCount":result.reused_count,"queuedCount":result.queued_count,"skippedCount":result.skipped_count,"idempotencyKey":request.idempotency_key}),
    ) {
        Ok(()) => (StatusCode::ACCEPTED, Json(result)).into_response(),
        Err(response) => *response,
    }
}

fn audit_admin(
    state: &HttpState,
    principal: &AuthPrincipal,
    request_id: &RequestId,
    action: &str,
    target_type: &str,
    target_id: Option<&str>,
    detail: serde_json::Value,
) -> Result<(), Box<Response>> {
    let (actor_type, actor_id) = match principal {
        AuthPrincipal::Browser { session_id, .. } => {
            (AuditActorType::BrowserSession, session_id.as_str())
        }
        AuthPrincipal::Device { credential_id } => {
            (AuditActorType::DeviceCredential, credential_id.as_str())
        }
    };
    state
        .foundation
        .catalog()
        .audit_log()
        .record(
            actor_type,
            actor_id,
            action,
            target_type,
            target_id,
            &request_id.0,
            &detail,
        )
        .map(|_| {
            state.ws_channels.broadcast("__admin__", "adminChanged");
        })
        .map_err(|_| {
            Box::new(query_error_response(
                request_id,
                "audit_write_failed",
                StatusCode::INTERNAL_SERVER_ERROR,
            ))
        })
}

pub async fn bind_loopback(address: SocketAddr) -> Result<TcpListener> {
    if !address.ip().is_loopback() {
        return Err(anyhow!(
            "internal HTTP listener must use a loopback address"
        ));
    }
    TcpListener::bind(address)
        .await
        .with_context(|| format!("bind private server listener at {address}"))
}

pub async fn serve_until_shutdown(
    listener: TcpListener,
    app: Router,
    shutdown: ShutdownCoordinator,
    shutdown_timeout: Duration,
) -> Result<()> {
    let graceful_shutdown = shutdown.clone();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            graceful_shutdown.cancelled().await;
        })
        .into_future();
    tokio::pin!(server);

    tokio::select! {
        biased;
        _ = shutdown.cancelled() => {
            timeout(shutdown_timeout, &mut server)
                .await
                .map_err(|_| anyhow!("graceful shutdown exceeded {} seconds", shutdown_timeout.as_secs()))??;
        }
        result = &mut server => {
            result.context("internal HTTP server stopped unexpectedly")?;
        }
    }
    Ok(())
}

async fn root(Extension(request_id): Extension<RequestId>) -> Json<RootResponse> {
    Json(RootResponse {
        request_id: request_id.0,
        service: "rcdvlog-server",
        status: "private_foundation",
        version_path: "/api/v1/version",
    })
}

async fn health(Extension(request_id): Extension<RequestId>) -> Json<HealthResponse> {
    Json(HealthResponse {
        request_id: request_id.0,
        status: "live",
    })
}

async fn ready(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    let report = state.readiness_gate.evaluate().await;
    let status = if report.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(ReadyResponse {
            request_id: request_id.0,
            report,
        }),
    )
        .into_response()
}

async fn version(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
) -> Json<VersionResponse> {
    Json(VersionResponse {
        request_id: request_id.0,
        application_version: APPLICATION_VERSION,
        protocol_version: ProtocolVersion {
            major: PROTOCOL_MAJOR_VERSION,
            minor: PROTOCOL_MINOR_VERSION,
        },
        control_schema_version: CONTROL_CATALOG_SCHEMA_VERSION,
        node_id: state.foundation.catalog().node_id().to_string(),
    })
}

async fn not_found(Extension(request_id): Extension<RequestId>) -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            request_id: request_id.0,
            code: "not_found",
        }),
    )
}

async fn request_context(mut request: Request<Body>, next: Next) -> Response {
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| valid_request_id(value))
        .map(str::to_string)
        .unwrap_or_else(|| format!("request_{}", Uuid::new_v4().simple()));
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let started = Instant::now();
    let span = tracing::info_span!(
        "http_request",
        component = "http",
        requestId = %request_id,
        method = %method,
        path = %path,
    );
    let mut response = next.run(request).instrument(span.clone()).await;
    response.headers_mut().insert(
        header::HeaderName::from_static(REQUEST_ID_HEADER),
        HeaderValue::from_str(&request_id).expect("validated request ID is a valid header value"),
    );
    tracing::info!(
        parent: &span,
        component = "http",
        requestId = %request_id,
        status = response.status().as_u16(),
        elapsedMs = started.elapsed().as_millis() as u64,
        "request_complete"
    );
    response
}

fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REQUEST_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use rcdvlog_server_core::{BaselineEnvelope, Catalog, ProjectAssetAddedPayload, ServerConfig};
    use serde_json::Value;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};
    use tempfile::TempDir;
    use tower::ServiceExt;

    struct Fixture {
        _root: TempDir,
        foundation: Arc<RuntimeFoundation>,
    }

    impl Fixture {
        fn new(missing_tool: bool) -> Self {
            let root = TempDir::new().unwrap();
            let data = root.path().join("state");
            let derived = root.path().join("derived");
            let backup = root.path().join("backup");
            let originals = root.path().join("originals");
            std::fs::create_dir_all(&derived).unwrap();
            std::fs::create_dir_all(&backup).unwrap();
            std::fs::create_dir_all(&originals).unwrap();
            let tool = if missing_tool {
                "rcdvlog-tool-that-does-not-exist"
            } else {
                "/usr/bin/true"
            };
            let config = ServerConfig::from_values(BTreeMap::from([
                ("RCDVLOG_DATA_DIR".to_string(), data.display().to_string()),
                (
                    "RCDVLOG_DERIVED_DIR".to_string(),
                    derived.display().to_string(),
                ),
                (
                    "RCDVLOG_BACKUP_DIR".to_string(),
                    backup.display().to_string(),
                ),
                (
                    "RCDVLOG_ALLOWED_ORIGINAL_MOUNTS".to_string(),
                    originals.display().to_string(),
                ),
                ("RCDVLOG_FFMPEG".to_string(), tool.to_string()),
                ("RCDVLOG_FFPROBE".to_string(), tool.to_string()),
                ("RCDVLOG_EXIFTOOL".to_string(), tool.to_string()),
            ]))
            .unwrap();
            let foundation = Arc::new(RuntimeFoundation::initialize(config).unwrap());
            Self {
                _root: root,
                foundation,
            }
        }

        fn app(&self) -> Router {
            router(HttpState::new(self.foundation.clone()))
        }

        fn seed_media_content(&self, bytes: &[u8]) -> RepresentationId {
            let id = RepresentationId::parse("representation_http_fixture").unwrap();
            let relative = "representations/http/fixture/representation_http_fixture.mp4";
            let path = self.foundation.config().derived_dir.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
            let now = "2026-07-23T00:00:00Z";
            let connection = rcdvlog_server_core::open_writable_connection(
                self.foundation.catalog().database_path(),
            )
            .unwrap();
            connection.execute(
                "INSERT INTO media_objects(media_object_id,size_bytes,full_content_hash,verification_state,created_at,updated_at) VALUES(?1,?2,?3,'verified',?4,?4)",
                rusqlite::params!["media_http_fixture", bytes.len() as i64, "a".repeat(64), now],
            ).unwrap();
            connection.execute(
                "INSERT INTO representations(representation_id,media_object_id,source_content_hash,source_size_bytes,kind,recipe_key,settings_identity,state,output_content_hash,byte_length,content_type,created_at,completed_at,updated_at) VALUES(?1,'media_http_fixture',?2,?3,'proxy','proxy.h264-720p.orientation-v3',?4,'available',?5,?3,'video/mp4',?6,?6,?6)",
                rusqlite::params![id.as_str(), "a".repeat(64), bytes.len() as i64, "b".repeat(64), blake3::hash(bytes).to_hex().to_string(), now],
            ).unwrap();
            connection.execute(
                "INSERT INTO representation_locations(representation_id,node_id,location_kind,location_key,relative_path,state,verified_at,updated_at) VALUES(?1,?2,'shared_derived',?3,?3,'available',?4,?4)",
                rusqlite::params![id.as_str(), self.foundation.catalog().node_id().as_str(), relative, now],
            ).unwrap();
            id
        }
    }

    async fn json(response: Response) -> Value {
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn device_auth_token(fixture: &Fixture) -> String {
        let bootstrap = fixture.foundation.auth().create_bootstrap_code().unwrap();
        let session = fixture
            .foundation
            .auth()
            .exchange_bootstrap(bootstrap.code.expose(), "rcdvlog.local")
            .unwrap();
        let pairing = fixture
            .foundation
            .auth()
            .create_pairing_code(&session.session_id)
            .unwrap();
        let device = fixture
            .foundation
            .auth()
            .exchange_pairing(pairing.code.expose(), "Test Mac")
            .unwrap();
        device.token.expose().to_string()
    }

    fn browser_auth(fixture: &Fixture) -> (String, String, String) {
        let bootstrap = fixture.foundation.auth().create_bootstrap_code().unwrap();
        let session = fixture
            .foundation
            .auth()
            .exchange_bootstrap(bootstrap.code.expose(), "rcdvlog.local")
            .unwrap();
        (
            session.session_id,
            session.session_token.expose().to_string(),
            session.csrf_token,
        )
    }

    fn baseline_envelope(root: &TempDir) -> BaselineEnvelope {
        let catalog = Catalog::open(root.path().join("desktop.sqlite3")).unwrap();
        catalog.set_local_node_id("node_mac").unwrap();
        let project = catalog.create_project("HTTP Trip").unwrap();
        catalog
            .add_portable_project_asset(
                &project.id,
                ProjectAssetAddedPayload {
                    id: "asset_http".to_string(),
                    fingerprint: "fingerprint-http".to_string(),
                    filename: "http.mov".to_string(),
                    size_bytes: 10,
                    duration_seconds: 1.0,
                    frame_rate: 30.0,
                    width: 1920,
                    height: 1080,
                    codec: "h264".to_string(),
                    camera: "camera".to_string(),
                    captured_at_raw: None,
                    best_time: None,
                    metadata_rotation_degrees: 0,
                    default_rotation_degrees: 0,
                    created_at: "2026-07-23T00:00:00Z".to_string(),
                },
            )
            .unwrap();
        BaselineEnvelope::export(&catalog, &project.id, vec![], vec![]).unwrap()
    }

    #[tokio::test]
    async fn baseline_preview_import_and_reconcile_routes_preserve_protocol_errors() {
        let fixture = Fixture::new(false);
        let token = device_auth_token(&fixture);
        let source = TempDir::new().unwrap();
        let envelope = baseline_envelope(&source);
        let body = serde_json::to_vec(&envelope).unwrap();

        let preview = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/internal/projects/baseline/preview")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {}", token))
                    .body(Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(preview.status(), StatusCode::OK);
        let preview = json(preview).await;
        assert_eq!(preview["preview"]["assetCount"], 1);
        assert_eq!(
            preview["preview"]["projectId"],
            envelope.payload.project.project_id
        );

        let imported = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/internal/projects/baseline/import")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {}", token))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(imported.status(), StatusCode::CREATED);
        let imported = json(imported).await;
        assert_eq!(
            imported["replica"]["baselineDigest"],
            envelope.digest.as_str()
        );

        let bad_reconcile = serde_json::json!({
            "protocolVersion": {
                "minimumMajor": 99,
                "minimumMinor": 0,
                "maximumMajor": 99,
                "maximumMinor": 0
            },
            "projectSchemaVersion": rcdvlog_server_core::PROJECT_CATALOG_SCHEMA_VERSION,
            "projectId": envelope.payload.project.project_id,
            "serverNodeId": fixture.foundation.catalog().node_id().as_str(),
            "operationCursors": {},
            "operations": [],
            "observations": [],
            "representationManifests": [],
            "maximumResponseOperations": 10
        });
        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!(
                        "/api/v1/internal/projects/{}/reconcile",
                        envelope.payload.project.project_id
                    ))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {}", token))
                    .body(Body::from(serde_json::to_vec(&bad_reconcile).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json(response).await["code"], "sync_protocol_incompatible");
    }

    #[tokio::test]
    async fn media_content_supports_head_and_single_ranges_without_exposing_paths() {
        let fixture = Fixture::new(false);
        let token = device_auth_token(&fixture);
        let bytes = b"0123456789abcdef";
        let id = fixture.seed_media_content(bytes);
        let uri = format!("/api/v1/media/{}/content", id.as_str());

        let partial = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header(header::RANGE, "bytes=3-7")
                    .header(header::AUTHORIZATION, format!("Bearer {}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(partial.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(partial.headers()[header::CONTENT_RANGE], "bytes 3-7/16");
        assert_eq!(partial.headers()[header::ACCEPT_RANGES], "bytes");
        assert_eq!(
            to_bytes(partial.into_body(), 32).await.unwrap(),
            &bytes[3..=7]
        );

        let head = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri(&uri)
                    .header(header::AUTHORIZATION, format!("Bearer {}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(head.status(), StatusCode::OK);
        assert_eq!(head.headers()[header::CONTENT_LENGTH], "16");
        assert!(to_bytes(head.into_body(), 32).await.unwrap().is_empty());

        let invalid = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header(header::RANGE, "bytes=0-1,4-5")
                    .header(header::AUTHORIZATION, format!("Bearer {}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(invalid.headers()[header::CONTENT_RANGE], "bytes */16");
    }

    #[test]
    fn range_parser_handles_open_suffix_and_unsatisfiable_ranges() {
        assert_eq!(
            parse_range(Some(&HeaderValue::from_static("bytes=4-")), 10),
            Ok(Some((4, 9)))
        );
        assert_eq!(
            parse_range(Some(&HeaderValue::from_static("bytes=-3")), 10),
            Ok(Some((7, 9)))
        );
        assert!(parse_range(Some(&HeaderValue::from_static("bytes=10-")), 10).is_err());
        assert!(parse_range(Some(&HeaderValue::from_static("items=0-1")), 10).is_err());
    }

    #[tokio::test]
    async fn health_is_read_only_and_preserves_valid_incoming_request_id() {
        let fixture = Fixture::new(false);
        let database_modified = std::fs::metadata(fixture.foundation.catalog().database_path())
            .unwrap()
            .modified()
            .unwrap();
        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .header(REQUEST_ID_HEADER, "caller.safe-123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[REQUEST_ID_HEADER], "caller.safe-123");
        let body = json(response).await;
        assert_eq!(body["status"], "live");
        assert_eq!(body["requestId"], "caller.safe-123");
        assert_eq!(
            std::fs::metadata(fixture.foundation.catalog().database_path())
                .unwrap()
                .modified()
                .unwrap(),
            database_modified
        );
    }

    #[tokio::test]
    async fn invalid_request_id_is_replaced_and_readiness_uses_503() {
        let fixture = Fixture::new(true);
        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/ready")
                    .header(REQUEST_ID_HEADER, "unsafe request id")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let response_id = response.headers()[REQUEST_ID_HEADER]
            .to_str()
            .unwrap()
            .to_string();
        assert!(response_id.starts_with("request_"));
        let body = json(response).await;
        assert_eq!(body["status"], "not_ready");
        assert_eq!(body["requestId"], response_id);
    }

    #[tokio::test]
    async fn version_reports_all_compatibility_versions_and_node_identity() {
        let fixture = Fixture::new(false);
        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json(response).await;
        assert_eq!(body["applicationVersion"], APPLICATION_VERSION);
        assert_eq!(body["protocolVersion"]["major"], PROTOCOL_MAJOR_VERSION);
        assert_eq!(body["protocolVersion"]["minor"], PROTOCOL_MINOR_VERSION);
        assert_eq!(body["controlSchemaVersion"], CONTROL_CATALOG_SCHEMA_VERSION);
        assert_eq!(
            body["nodeId"],
            fixture.foundation.catalog().node_id().as_str()
        );
    }

    #[tokio::test]
    async fn serves_built_web_assets_with_spa_and_cache_policy() {
        let fixture = Fixture::new(false);
        let foundation_root = fixture
            .app()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(foundation_root.status(), StatusCode::OK);
        assert_eq!(json(foundation_root).await["status"], "private_foundation");

        let web_root = fixture._root.path().join("web");
        std::fs::create_dir_all(web_root.join("assets")).unwrap();
        std::fs::write(web_root.join("index.html"), b"<main>web shell</main>").unwrap();
        std::fs::write(web_root.join("assets/app-abc123.js"), b"export {};").unwrap();
        let app = router(HttpState::new(fixture.foundation.clone()).with_web_root(web_root));

        let index = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(index.status(), StatusCode::OK);
        assert_eq!(index.headers()[header::CACHE_CONTROL], "no-cache");
        assert_eq!(
            to_bytes(index.into_body(), 1024).await.unwrap(),
            "<main>web shell</main>"
        );

        let asset = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/assets/app-abc123.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(asset.status(), StatusCode::OK);
        assert_eq!(
            asset.headers()[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );

        let missing_api = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/not-real")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_api.status(), StatusCode::NOT_FOUND);
        assert_eq!(json(missing_api).await["code"], "not_found");
    }

    #[tokio::test]
    async fn exposes_the_rust_owned_openapi_contract() {
        let fixture = Fixture::new(false);
        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri("/api/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json(response).await;
        assert_eq!(body["openapi"], "3.1.0");
        assert!(body["paths"]["/api/v1/auth/bootstrap/exchange"].is_object());
    }

    #[tokio::test]
    async fn cancellation_stops_the_listener_within_the_bound() {
        let fixture = Fixture::new(false);
        let listener = match bind_loopback("127.0.0.1:0".parse().unwrap()).await {
            Ok(listener) => listener,
            Err(error)
                if error.chain().any(|cause| {
                    cause
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
                }) =>
            {
                // Some hermetic test sandboxes forbid every socket bind. The same test runs
                // normally on development hosts and Linux CI where loopback is available.
                return;
            }
            Err(error) => panic!("bind loopback test listener: {error:#}"),
        };
        let shutdown = ShutdownCoordinator::new();
        let cancel = shutdown.clone();
        let server = tokio::spawn(serve_until_shutdown(
            listener,
            fixture.app(),
            shutdown,
            Duration::from_secs(1),
        ));
        cancel.cancel();
        timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn readiness_timeout_is_bounded_and_repeated_requests_are_single_flight() {
        let fixture = Fixture::new(false);
        let evaluations = Arc::new(AtomicUsize::new(0));
        let evaluator_count = evaluations.clone();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let evaluator_release = release.clone();
        let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
        let started_sender = Arc::new(Mutex::new(Some(started_sender)));
        let evaluator_started_sender = started_sender.clone();
        let auth = Arc::new(fixture.foundation.auth().clone());
        let state = HttpState::with_readiness_evaluator(
            fixture.foundation.clone(),
            auth,
            Duration::from_millis(20),
            move || {
                evaluator_count.fetch_add(1, Ordering::SeqCst);
                if let Some(sender) = evaluator_started_sender.lock().unwrap().take() {
                    let _ = sender.send(());
                }
                let (lock, condition) = &*evaluator_release;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = condition.wait(released).unwrap();
                }
                ReadinessReport::from_checks(Vec::new())
            },
        );
        let app = router(state);

        let first_request = tokio::spawn(
            app.clone().oneshot(
                Request::builder()
                    .uri("/api/v1/ready")
                    .body(Body::empty())
                    .unwrap(),
            ),
        );
        timeout(Duration::from_secs(1), started_receiver)
            .await
            .unwrap()
            .unwrap();
        let response_wait_started = Instant::now();
        let timed_out = first_request.await.unwrap().unwrap();
        assert_eq!(timed_out.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(response_wait_started.elapsed() < Duration::from_secs(1));
        let body = json(timed_out).await;
        assert_eq!(body["checks"][0]["code"], "readiness_check_timeout");

        let health = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);

        let busy = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = json(busy).await;
        assert_eq!(body["checks"][0]["code"], "readiness_check_busy");

        let (lock, condition) = &*release;
        *lock.lock().unwrap() = true;
        condition.notify_all();
        assert_eq!(evaluations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn library_media_requires_authentication() {
        let fixture = Fixture::new(false);
        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/library/media?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn library_media_rejects_unsupported_query_combinations() {
        let fixture = Fixture::new(false);
        let token = device_auth_token(&fixture);

        for uri in [
            "/api/v1/library/media?limit=500",
            "/api/v1/library/media?limit=10&sort=filenameAsc&storageRootId=root_a",
            "/api/v1/library/media?limit=10&nonsense=1",
        ] {
            let response = fixture
                .app()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header(header::AUTHORIZATION, format!("Bearer {token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "uri: {uri}");
            let body = json(response).await;
            assert_eq!(body["code"], "invalid_query", "uri: {uri}");
        }
    }

    #[tokio::test]
    async fn a_cursor_cannot_be_replayed_against_another_sort() {
        let fixture = Fixture::new(false);
        let token = device_auth_token(&fixture);
        let cursor = rcdvlog_server_core::library_query::encode_cursor(
            rcdvlog_server_core::library_query::LibrarySort::CreatedDesc,
            &Default::default(),
            &rcdvlog_server_core::library_query::CursorKey::Created {
                created_at: "2026-07-20T00:00:00Z".to_string(),
                media_object_id: "media_a".to_string(),
            },
        );
        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/v1/library/media?limit=10&sort=filenameAsc&after={cursor}"
                    ))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json(response).await["code"], "invalid_query");
    }

    #[tokio::test]
    async fn project_asset_responses_contain_no_absolute_paths() {
        let fixture = Fixture::new(false);
        let token = device_auth_token(&fixture);
        let source = TempDir::new().unwrap();
        let envelope = baseline_envelope(&source);
        let project_id = envelope.payload.project.project_id.clone();
        let body = serde_json::to_vec(&envelope).unwrap();
        let imported = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/internal/projects/baseline/import")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(imported.status(), StatusCode::CREATED);

        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/projects/{project_id}/assets?limit=10"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let text = json(response).await.to_string();
        assert!(!text.contains(source.path().to_str().unwrap()));
        assert!(!text.contains("\"path\""));
        assert!(!text.contains("proxyPath"));
    }

    #[tokio::test]
    async fn add_to_project_route_is_bounded_idempotent_and_replica_authoritative() {
        let fixture = Fixture::new(false);
        let token = device_auth_token(&fixture);
        let source = TempDir::new().unwrap();
        let envelope = baseline_envelope(&source);
        let project_id = envelope.payload.project.project_id.clone();
        fixture
            .foundation
            .catalog()
            .project_replicas()
            .import_baseline(&envelope)
            .unwrap();
        fixture
            .foundation
            .catalog()
            .storage_library()
            .create_media_object(rcdvlog_server_core::NewMediaObject {
                id: MediaObjectId::parse("media_http_add").unwrap(),
                size_bytes: Some(10),
                quick_fingerprint: Some(blake3::hash(b"http-add").to_hex().to_string()),
                full_content_hash: Some("a".repeat(64)),
                verification_state: GlobalMediaVerificationState::Verified,
            })
            .unwrap();
        let body = serde_json::to_vec(&serde_json::json!({
            "mediaObjectIds": ["media_http_add"],
            "acknowledgeUnverified": false,
            "idempotencyKey": "http-add-1"
        }))
        .unwrap();

        for expected in ["added", "already_member"] {
            let response = fixture
                .app()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(format!("/api/v1/projects/{project_id}/assets/add"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .header(header::AUTHORIZATION, format!("Bearer {token}"))
                        .body(Body::from(body.clone()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(json(response).await["items"][0]["disposition"], expected);
        }
        assert_eq!(
            fixture
                .foundation
                .catalog()
                .project_replicas()
                .open_catalog(&project_id)
                .unwrap()
                .count_project_assets(&project_id)
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn library_media_detail_requires_authentication() {
        let fixture = Fixture::new(false);
        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/library/media/media_missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn library_media_detail_reports_not_found_for_unknown_media() {
        let fixture = Fixture::new(false);
        let token = device_auth_token(&fixture);
        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/library/media/media_missing")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(json(response).await["code"], "not_found");
    }

    #[tokio::test]
    async fn library_media_detail_rejects_unknown_query_parameters() {
        let fixture = Fixture::new(false);
        let token = device_auth_token(&fixture);
        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/library/media/media_missing?nonsense=1")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json(response).await["code"], "invalid_query");
    }

    #[tokio::test]
    async fn projects_requires_authentication() {
        let fixture = Fixture::new(false);
        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn projects_lists_registered_project_replicas() {
        let fixture = Fixture::new(false);
        let token = device_auth_token(&fixture);
        let source = TempDir::new().unwrap();
        let envelope = baseline_envelope(&source);
        let project_id = envelope.payload.project.project_id.clone();
        let body = serde_json::to_vec(&envelope).unwrap();
        let imported = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/internal/projects/baseline/import")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(imported.status(), StatusCode::CREATED);

        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/projects")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json(response).await;
        assert_eq!(body["items"][0]["projectId"], project_id);
        assert!(body["nextCursor"].is_null());
    }

    #[tokio::test]
    async fn projects_rejects_unknown_query_parameters() {
        let fixture = Fixture::new(false);
        let token = device_auth_token(&fixture);
        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/projects?limit=10&nonsense=1")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json(response).await["code"], "invalid_query");
    }

    #[tokio::test]
    async fn project_assets_requires_authentication() {
        let fixture = Fixture::new(false);
        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/projects/project_missing/assets")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn project_assets_rejects_unknown_query_parameters_and_missing_project() {
        let fixture = Fixture::new(false);
        let token = device_auth_token(&fixture);

        let unknown_param = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/projects/project_missing/assets?limit=10&nonsense=1")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unknown_param.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json(unknown_param).await["code"], "invalid_query");

        let missing_project = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/projects/project_missing/assets?limit=10")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_project.status(), StatusCode::NOT_FOUND);
        assert_eq!(json(missing_project).await["code"], "not_found");
    }

    #[tokio::test]
    async fn review_routes_return_canonical_state_and_commit_stale_note_revisions() {
        let fixture = Fixture::new(false);
        let token = device_auth_token(&fixture);
        let source = TempDir::new().unwrap();
        let envelope = baseline_envelope(&source);
        let project_id = envelope.payload.project.project_id.clone();
        fixture
            .foundation
            .catalog()
            .project_replicas()
            .import_baseline(&envelope)
            .unwrap();
        let path = format!("/api/v1/projects/{project_id}/assets/asset_http");

        let review = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri(format!("{path}/review"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(review.status(), StatusCode::OK);
        let initial = json(review).await;
        let stale_base = initial["materializedRevision"]
            .as_u64()
            .unwrap()
            .saturating_sub(1);

        let note = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("{path}/notes"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "text": "Browser revision",
                            "baseRevision": stale_base
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(note.status(), StatusCode::OK);
        let note = json(note).await;
        assert_eq!(note["asset"]["activeNote"], "Browser revision");
        assert_eq!(note["staleBaseRevision"], true);
        assert!(note["asset"]["noteRevisionCount"].as_u64().unwrap() >= 2);

        let label = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("{path}/labels/add"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "label": "keeper",
                            "baseRevision": note["asset"]["materializedRevision"]
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(label.status(), StatusCode::OK);
        assert_eq!(json(label).await["asset"]["labels"][0], "keeper");
    }

    #[tokio::test]
    async fn review_routes_require_authentication_and_validate_rotation() {
        let fixture = Fixture::new(false);
        let unauthenticated = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/projects/project_missing/assets/asset_missing/review")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        let token = device_auth_token(&fixture);
        let source = TempDir::new().unwrap();
        let envelope = baseline_envelope(&source);
        let project_id = envelope.payload.project.project_id.clone();
        fixture
            .foundation
            .catalog()
            .project_replicas()
            .import_baseline(&envelope)
            .unwrap();
        let invalid = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!(
                        "/api/v1/projects/{project_id}/assets/asset_http/rotation"
                    ))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(r#"{"rotationDegrees":45,"baseRevision":0}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json(invalid).await["code"], "invalid_input");
    }

    #[tokio::test]
    async fn storage_admin_requires_allowed_paths_evidence_confirmation_and_audits_mutations() {
        let fixture = Fixture::new(false);
        let token = device_auth_token(&fixture);
        let create = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/admin/storage-roots")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        r#"{"displayName":"Archive","immutableOriginal":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create.status(), StatusCode::CREATED);
        let root = json(create).await;
        let root_id = root["id"].as_str().unwrap();
        std::fs::write(
            fixture._root.path().join("originals/sample.mov"),
            b"0123456789",
        )
        .unwrap();
        fixture
            .foundation
            .catalog()
            .storage_library()
            .create_storage_object(rcdvlog_server_core::NewStorageObject {
                id: rcdvlog_server_core::StorageObjectId::parse("storage_admin_sample").unwrap(),
                storage_root_id: StorageRootId::parse(root_id).unwrap(),
                relative_path: RootRelativePath::parse("sample.mov").unwrap(),
                size_bytes: Some(10),
                modified_at: None,
            })
            .unwrap();

        let rejected_write = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri(format!("/api/v1/admin/storage-roots/{root_id}/binding"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "localMountPath": fixture._root.path().join("originals"),
                            "protocol": "local", "accessMode": "read_write"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected_write.status(), StatusCode::BAD_REQUEST);

        let disallowed = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri(format!("/api/v1/admin/storage-roots/{root_id}/binding"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "localMountPath": fixture.foundation.config().derived_dir,
                            "protocol": "local", "accessMode": "read_only"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(disallowed.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json(disallowed).await["code"], "mount_path_not_allowed");

        let binding = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri(format!("/api/v1/admin/storage-roots/{root_id}/binding"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "localMountPath": fixture._root.path().join("originals"),
                            "protocol": "local", "accessMode": "read_only"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(binding.status(), StatusCode::OK);
        assert_eq!(json(binding).await["confirmation"]["status"], "pending");

        let verified = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!(
                        "/api/v1/admin/storage-roots/{root_id}/binding/verify"
                    ))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(verified.status(), StatusCode::OK);
        let verified = json(verified).await;
        assert_eq!(verified["associationEvidence"]["matchingRelativePaths"], 1);
        assert_eq!(verified["associationEvidence"]["mismatches"], 0);

        let confirmed = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!(
                        "/api/v1/admin/storage-roots/{root_id}/binding/confirmation"
                    ))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(r#"{"action":"confirm"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(confirmed.status(), StatusCode::OK);
        assert_eq!(json(confirmed).await["confirmation"]["status"], "confirmed");

        let audit = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/admin/audit?limit=10")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let audit = json(audit).await;
        assert!(audit["items"].as_array().unwrap().len() >= 4);
        assert!(!audit.to_string().to_ascii_lowercase().contains("token"));
    }

    #[tokio::test]
    async fn representation_batch_skips_unverified_sources_instead_of_enqueuing() {
        let fixture = Fixture::new(false);
        let token = device_auth_token(&fixture);
        fixture
            .foundation
            .catalog()
            .storage_library()
            .create_media_object(rcdvlog_server_core::NewMediaObject {
                id: MediaObjectId::parse("media_unverified_admin").unwrap(),
                size_bytes: Some(100),
                quick_fingerprint: Some("a".repeat(64)),
                full_content_hash: None,
                verification_state: GlobalMediaVerificationState::Unverified,
            })
            .unwrap();
        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/admin/representations/batch")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(r#"{"mediaObjectIds":["media_unverified_admin"],"kind":"proxy","idempotencyKey":"batch-1"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = json(response).await;
        assert_eq!(body["queuedCount"], 0);
        assert_eq!(body["skipped"][0]["reason"], "source_not_verified");
    }

    #[tokio::test]
    async fn revoking_current_browser_session_commits_audit_and_expires_cookie() {
        let fixture = Fixture::new(false);
        let (session_id, session_token, csrf_token) = browser_auth(&fixture);
        let cookie = format!("rcdvlog_session={session_id}:{session_token}");
        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri(format!("/api/v1/auth/sessions/{session_id}"))
                    .header(header::COOKIE, &cookie)
                    .header("x-csrf-token", csrf_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get(header::SET_COOKIE)
                .unwrap()
                .to_str()
                .unwrap()
                .contains("Max-Age=0")
        );

        let denied = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/sessions")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let events = fixture
            .foundation
            .catalog()
            .audit_log()
            .list(10, None)
            .unwrap();
        assert!(
            events
                .iter()
                .any(|event| { event.action == "session.revoked" && event.actor_id == session_id })
        );
    }
}
