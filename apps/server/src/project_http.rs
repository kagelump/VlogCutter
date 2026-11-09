//! Project listing, assets, add-to-project, and review mutations.

use super::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ListProjectsParams {
    #[serde(default = "default_page_limit")]
    pub(super) limit: u32,
    #[serde(default)]
    pub(super) after: Option<String>,
}

pub(super) async fn list_projects(
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
pub(super) struct ProjectAssetParams {
    #[serde(default = "default_page_limit")]
    pub(super) limit: u32,
    #[serde(default)]
    pub(super) after: Option<String>,
    #[serde(default)]
    pub(super) search: Option<String>,
}

pub(super) async fn list_project_assets(
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

pub(super) async fn preview_add_to_project(
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

pub(super) async fn add_to_project(
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

pub(super) fn project_command_error_response(
    request_id: &RequestId,
    error: ProjectReplicaError,
) -> Response {
    match error {
        ProjectReplicaError::NotFound { .. }
        | ProjectReplicaError::Storage(StorageLibraryError::NotFound { .. }) => {
            query_error_response(request_id, "not_found", StatusCode::NOT_FOUND)
        }
        ProjectReplicaError::InvalidBaseline(_) => {
            query_error_response(request_id, "invalid_input", StatusCode::BAD_REQUEST)
        }
        ProjectReplicaError::Attached { .. } => {
            query_error_response(request_id, "project_attached", StatusCode::CONFLICT)
        }
        // An attached catalog at a different schema than this build is unusable until they match.
        ProjectReplicaError::AttachRefused(_) => query_error_response(
            request_id,
            "attached_catalog_refused",
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        _ => query_error_response(
            request_id,
            "internal_error",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct NoteMutationRequest {
    pub(super) text: String,
    pub(super) base_revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct LabelMutationRequest {
    pub(super) label: String,
    pub(super) base_revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RemovedMutationRequest {
    pub(super) removed: bool,
    pub(super) base_revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RotationMutationRequest {
    pub(super) rotation_degrees: u16,
    pub(super) base_revision: u64,
}

pub(super) async fn get_project_review_asset(
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

/// Streams only the current proxy recorded by an attached catalog. SQLite and
/// path checks run off the async executor; the file is streamed in bounded chunks.
pub(super) async fn project_asset_proxy(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, asset_id)): AxumPath<(String, String)>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let manager = state.foundation.catalog().project_replicas();
    let lookup =
        tokio::task::spawn_blocking(move || manager.asset_proxy_path(&project_id, &asset_id)).await;
    let path = match lookup {
        Ok(Ok(Some(path))) => path,
        Ok(Ok(None)) => {
            return query_error_response(&request_id, "not_found", StatusCode::NOT_FOUND);
        }
        Ok(Err(error)) => return project_command_error_response(&request_id, error),
        Err(_) => {
            return query_error_response(
                &request_id,
                "internal_error",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    let mut file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(_) => return query_error_response(&request_id, "not_found", StatusCode::NOT_FOUND),
    };
    let metadata = match file.metadata().await {
        Ok(metadata) if metadata.is_file() && metadata.len() > 0 => metadata,
        _ => return query_error_response(&request_id, "not_found", StatusCode::NOT_FOUND),
    };
    let length = metadata.len();
    let modified = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let etag = format!("\"{length:x}-{modified:x}\"");
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|value| value == etag)
    {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, etag)
            .header(header::CACHE_CONTROL, "private, max-age=0, must-revalidate")
            .body(Body::empty())
            .expect("valid proxy headers");
    }
    let range = if headers
        .get(header::IF_RANGE)
        .is_some_and(|value| value.to_str().ok() != Some(etag.as_str()))
    {
        None
    } else {
        headers.get(header::RANGE)
    };
    let selected = match parse_range(range, length) {
        Ok(selected) => selected,
        Err(()) => {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{length}"))
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CACHE_CONTROL, "private, no-store")
                .body(Body::empty())
                .expect("valid range refusal");
        }
    };
    let (start, end, status) = selected.map_or((0, length - 1, StatusCode::OK), |(start, end)| {
        (start, end, StatusCode::PARTIAL_CONTENT)
    });
    let count = end - start + 1;
    let mut builder = Response::builder()
        .status(status)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, "video/mp4")
        .header(header::CONTENT_LENGTH, count.to_string())
        .header(header::ETAG, etag)
        .header(header::CACHE_CONTROL, "private, max-age=0, must-revalidate");
    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{length}"),
        );
    }
    if method == Method::HEAD {
        return builder.body(Body::empty()).expect("valid proxy headers");
    }
    if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return query_error_response(&request_id, "not_found", StatusCode::NOT_FOUND);
    }
    builder
        .body(Body::from_stream(ReaderStream::new(file.take(count))))
        .expect("valid proxy stream")
}

/// Largest project-local thumbnail served; real ones are a few hundred KiB.
pub(super) const MAX_THUMBNAIL_BYTES: u64 = 20 * 1024 * 1024;

/// Serves the thumbnail a project's catalog records for an asset, typically a
/// same-machine CLI catalog's cache. Only a JPEG that resolves inside
/// `RCDVLOG_ATTACHED_PROJECT_ROOTS`, and outside the server's own data, temp and
/// backup directories, is ever read; anything else is reported as not found.
pub(super) async fn project_asset_thumbnail(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, asset_id)): AxumPath<(String, String)>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let recorded = match state
        .foundation
        .catalog()
        .project_replicas()
        .asset_thumbnail_path(&project_id, &asset_id)
    {
        Ok(Some(path)) => path,
        Ok(None) => return query_error_response(&request_id, "not_found", StatusCode::NOT_FOUND),
        Err(error) => return project_command_error_response(&request_id, error),
    };
    let config = state.foundation.config();
    let protected = [&config.data_dir, &config.temp_dir, &config.backup_dir];
    let Some((path, length, modified)) =
        servable_thumbnail(&recorded, &config.attached_project_roots, &protected)
    else {
        return query_error_response(&request_id, "not_found", StatusCode::NOT_FOUND);
    };
    let etag = format!("\"{length:x}-{modified:x}\"");
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == etag)
    {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, etag)
            .body(Body::empty())
            .expect("valid not-modified response");
    }
    let builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/jpeg")
        .header(header::CONTENT_LENGTH, length.to_string())
        .header(header::ETAG, etag)
        .header(header::CACHE_CONTROL, "private, max-age=300");
    if method == Method::HEAD {
        return builder
            .body(Body::empty())
            .expect("valid thumbnail headers");
    }
    match tokio::fs::read(&path).await {
        Ok(bytes) if bytes.len() as u64 == length => builder
            .body(Body::from(bytes))
            .expect("valid thumbnail response"),
        _ => query_error_response(&request_id, "not_found", StatusCode::NOT_FOUND),
    }
}

/// The canonical path, byte length and modification time (seconds) of a
/// recorded thumbnail this node may serve, or `None`.
pub(super) fn servable_thumbnail(
    recorded: &std::path::Path,
    allowed_roots: &[std::path::PathBuf],
    protected: &[&std::path::PathBuf],
) -> Option<(std::path::PathBuf, u64, u64)> {
    let path = recorded.canonicalize().ok()?;
    let is_jpeg = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "jpg" | "jpeg"));
    if !is_jpeg || !rcdvlog_server_core::inside_any_root(&path, allowed_roots) {
        return None;
    }
    if protected
        .iter()
        .filter_map(|directory| directory.canonicalize().ok())
        .any(|directory| path.starts_with(directory))
    {
        return None;
    }
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_THUMBNAIL_BYTES {
        return None;
    }
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs());
    Some((path, metadata.len(), modified))
}

pub(super) async fn create_review_note(
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

pub(super) async fn add_review_label(
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

pub(super) async fn remove_review_label(
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

pub(super) async fn set_review_removed(
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

pub(super) async fn set_review_rotation(
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

pub(super) fn review_mutation_response(
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
pub(super) fn project_read_error_response(
    request_id: &RequestId,
    error: ProjectReplicaError,
) -> Response {
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
