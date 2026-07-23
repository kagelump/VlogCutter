use anyhow::{Context, Result, anyhow};
use axum::body::Body;
use axum::extract::{Extension, Path as AxumPath, State};
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rcdvlog_server_core::{
    APPLICATION_VERSION, CONTROL_CATALOG_SCHEMA_VERSION, PROTOCOL_MAJOR_VERSION,
    PROTOCOL_MINOR_VERSION, ReadinessReport, Representation, RepresentationError, RepresentationId,
    RepresentationRequest, RuntimeFoundation,
};
use serde::Serialize;
use std::future::IntoFuture;
use std::net::SocketAddr;
use std::sync::Arc;
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

type ReadinessEvaluator = dyn Fn() -> ReadinessReport + Send + Sync + 'static;

#[derive(Clone)]
pub struct HttpState {
    foundation: Arc<RuntimeFoundation>,
    readiness_gate: ReadinessGate,
}

#[derive(Clone)]
struct ReadinessGate {
    evaluator: Arc<ReadinessEvaluator>,
    single_flight: Arc<Semaphore>,
    response_timeout: Duration,
}

impl HttpState {
    pub fn new(foundation: Arc<RuntimeFoundation>) -> Self {
        let readiness_foundation = foundation.clone();
        Self::with_readiness_evaluator(foundation, DEFAULT_READINESS_TIMEOUT, move || {
            readiness_foundation.readiness()
        })
    }

    fn with_readiness_evaluator(
        foundation: Arc<RuntimeFoundation>,
        response_timeout: Duration,
        evaluator: impl Fn() -> ReadinessReport + Send + Sync + 'static,
    ) -> Self {
        Self {
            foundation,
            readiness_gate: ReadinessGate {
                evaluator: Arc::new(evaluator),
                single_flight: Arc::new(Semaphore::new(1)),
                response_timeout,
            },
        }
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

pub fn router(state: HttpState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/api/v1/health", get(health))
        .route("/api/v1/ready", get(ready))
        .route("/api/v1/version", get(version))
        .route("/api/v1/representations", post(request_representation))
        .route(
            "/api/v1/representations/{representation_id}",
            get(get_representation),
        )
        .route(
            "/api/v1/media/{representation_id}/content",
            get(media_content).head(media_content),
        )
        .fallback(not_found)
        .with_state(state)
        .layer(middleware::from_fn(request_context))
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
    use rcdvlog_server_core::ServerConfig;
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
            std::fs::create_dir_all(&derived).unwrap();
            std::fs::create_dir_all(&backup).unwrap();
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

    #[tokio::test]
    async fn media_content_supports_head_and_single_ranges_without_exposing_paths() {
        let fixture = Fixture::new(false);
        let bytes = b"0123456789abcdef";
        let id = fixture.seed_media_content(bytes);
        let uri = format!("/api/v1/media/{}/content", id.as_str());

        let partial = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header(header::RANGE, "bytes=3-7")
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
        let state = HttpState::with_readiness_evaluator(
            fixture.foundation.clone(),
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
}
