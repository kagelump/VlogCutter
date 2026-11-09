//! Unauthenticated status endpoints (root, health, ready, version) and static web serving.

use super::*;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct HealthResponse {
    pub(super) request_id: String,
    pub(super) status: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReadyResponse {
    pub(super) request_id: String,
    #[serde(flatten)]
    pub(super) report: ReadinessReport,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ProtocolVersion {
    pub(super) major: u32,
    pub(super) minor: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct VersionResponse {
    pub(super) request_id: String,
    pub(super) application_version: &'static str,
    pub(super) protocol_version: ProtocolVersion,
    pub(super) control_schema_version: u32,
    pub(super) node_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RootResponse {
    pub(super) request_id: String,
    pub(super) service: &'static str,
    pub(super) status: &'static str,
    pub(super) version_path: &'static str,
}

pub(super) async fn web_or_not_found(
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

pub(super) fn content_type(path: &Path) -> &'static str {
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

pub(super) async fn root(Extension(request_id): Extension<RequestId>) -> Json<RootResponse> {
    Json(RootResponse {
        request_id: request_id.0,
        service: "rcdvlog-server",
        status: "private_foundation",
        version_path: "/api/v1/version",
    })
}

pub(super) async fn health(Extension(request_id): Extension<RequestId>) -> Json<HealthResponse> {
    Json(HealthResponse {
        request_id: request_id.0,
        status: "live",
    })
}

pub(super) async fn ready(
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

pub(super) async fn version(
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

pub(super) async fn not_found(Extension(request_id): Extension<RequestId>) -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            request_id: request_id.0,
            code: "not_found",
        }),
    )
}
