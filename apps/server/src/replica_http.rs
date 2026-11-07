//! Project baseline preview/import and reconciliation.

use super::*;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BaselinePreviewResponse {
    pub(super) request_id: String,
    pub(super) preview: BaselinePreview,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BaselineImportResponse {
    pub(super) request_id: String,
    pub(super) replica: ProjectReplica,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReconcileHttpResponse {
    pub(super) request_id: String,
    #[serde(flatten)]
    pub(super) reconciliation: ReconcileResponse,
}

// ── Auth route handlers ──────────────────────────────────────────────

pub(super) async fn preview_project_baseline(
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

pub(super) async fn import_project_baseline(
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

pub(super) async fn reconcile_project(
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

pub(super) fn internal_project_replica_error(request_id: String) -> Response {
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

pub(super) fn project_replica_error(request_id: String, error: ProjectReplicaError) -> Response {
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
