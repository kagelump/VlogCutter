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
