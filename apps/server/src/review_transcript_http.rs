//! Thin authenticated adapter over `rcdvlog_server_core::ReviewTranscripts` (#224).
use super::*;
use rcdvlog_server_core::{
    ModelDirSpeechRuntime, ReviewTargetActor, ReviewTranscriptError, ReviewTranscriptionRequest,
    ReviewTranscripts,
};

fn transcripts(state: &HttpState) -> ReviewTranscripts {
    state.foundation.catalog().review_transcripts()
}

fn actor(principal: &AuthPrincipal) -> ReviewTargetActor {
    match principal {
        AuthPrincipal::Browser { session_id, .. } => {
            ReviewTargetActor::new(AuditActorType::BrowserSession, session_id.as_str())
        }
        AuthPrincipal::Device { credential_id } => {
            ReviewTargetActor::new(AuditActorType::DeviceCredential, credential_id.as_str())
        }
    }
}

fn error_response(request_id: &RequestId, error: ReviewTranscriptError) -> Response {
    let status = match &error {
        ReviewTranscriptError::ProjectNotFound | ReviewTranscriptError::NotFound => {
            StatusCode::NOT_FOUND
        }
        ReviewTranscriptError::InvalidInput(_) => StatusCode::BAD_REQUEST,
        ReviewTranscriptError::NotReady(_) | ReviewTranscriptError::Conflict => {
            StatusCode::CONFLICT
        }
        ReviewTranscriptError::Infrastructure(_) | ReviewTranscriptError::Database(_) => {
            tracing::error!(request_id = %request_id.0, code = error.code(), "review transcription request failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    query_error_response(request_id, error.code(), status)
}

async fn blocking<T: Send + 'static>(
    request_id: &RequestId,
    work: impl FnOnce() -> Result<T, ReviewTranscriptError> + Send + 'static,
) -> Result<T, Response> {
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(error_response(request_id, error)),
        Err(_) => Err(query_error_response(
            request_id,
            "internal_error",
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

pub(super) async fn preflight(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, session_id)): AxumPath<(String, String)>,
    body: Result<Json<ReviewTranscriptionRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
    };
    let transcripts = transcripts(&state);
    match blocking(&request_id, move || {
        transcripts.preflight(
            &project_id,
            &session_id,
            &body,
            &ModelDirSpeechRuntime::from_environment(),
        )
    })
    .await
    {
        Ok(report) => Json(report).into_response(),
        Err(response) => response,
    }
}

pub(super) async fn start(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath((project_id, session_id)): AxumPath<(String, String)>,
    body: Result<Json<ReviewTranscriptionRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
    };
    let transcripts = transcripts(&state);
    let jobs = state.foundation.jobs();
    let requester = actor(&principal);
    let run = match blocking(&request_id, move || {
        transcripts.start(
            &project_id,
            &session_id,
            &body,
            &ModelDirSpeechRuntime::from_environment(),
            &jobs,
            &requester,
        )
    })
    .await
    {
        Ok(run) => run,
        Err(response) => return response,
    };
    if let Err(response) = audit_admin(
        &state,
        &principal,
        &request_id,
        "review_transcription.requested",
        "review_transcription",
        Some(&run.run_id),
        serde_json::json!({
            "sessionId": run.session_id,
            "backend": run.backend,
            "model": run.model,
            "authorizedMaxUsd": run.spend.authorized_max_usd,
        }),
    ) {
        return *response;
    }
    Json(run).into_response()
}

pub(super) async fn list(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, session_id)): AxumPath<(String, String)>,
) -> Response {
    let transcripts = transcripts(&state);
    match blocking(&request_id, move || {
        transcripts.list(&project_id, &session_id)
    })
    .await
    {
        Ok(items) => Json(serde_json::json!({ "items": items })).into_response(),
        Err(response) => response,
    }
}

pub(super) async fn get_run(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, run_id)): AxumPath<(String, String)>,
) -> Response {
    let transcripts = transcripts(&state);
    match blocking(&request_id, move || transcripts.get(&project_id, &run_id)).await {
        Ok(run) => Json(run).into_response(),
        Err(response) => response,
    }
}

pub(super) async fn cancel(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath((project_id, run_id)): AxumPath<(String, String)>,
) -> Response {
    let transcripts = transcripts(&state);
    let jobs = state.foundation.jobs();
    let run = match blocking(&request_id, move || {
        transcripts.cancel(&project_id, &run_id, &jobs)
    })
    .await
    {
        Ok(run) => run,
        Err(response) => return response,
    };
    if let Err(response) = audit_admin(
        &state,
        &principal,
        &request_id,
        "review_transcription.cancel_requested",
        "review_transcription",
        Some(&run.run_id),
        serde_json::json!({}),
    ) {
        return *response;
    }
    Json(run).into_response()
}

pub(super) fn routes(state: &HttpState) -> Router<HttpState> {
    Router::new()
        .route(
            "/api/v1/projects/{project_id}/review-sessions/{session_id}/transcriptions",
            get(list).post(start),
        )
        .route(
            "/api/v1/projects/{project_id}/review-sessions/{session_id}/transcriptions/preflight",
            post(preflight),
        )
        .route(
            "/api/v1/projects/{project_id}/review-transcriptions/{run_id}",
            get(get_run),
        )
        .route(
            "/api/v1/projects/{project_id}/review-transcriptions/{run_id}/cancel",
            post(cancel),
        )
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), require_csrf))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
}

pub(super) fn openapi_extend(document: &mut serde_json::Value) {
    use serde_json::json;
    let project =
        json!({"name":"projectId","in":"path","required":true,"schema":{"type":"string"}});
    let session =
        json!({"name":"sessionId","in":"path","required":true,"schema":{"type":"string"}});
    let run = json!({"name":"runId","in":"path","required":true,"schema":{"type":"string"}});
    let body = json!({"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/ReviewTranscriptionRequest"}}}});
    let ok = |schema: &str| json!({"description":schema,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{schema}")}}}});
    let error = json!({"$ref":"#/components/responses/Error"});
    let paths = json!({
        "/api/v1/projects/{projectId}/review-sessions/{sessionId}/transcriptions": {
            "get": {"operationId":"listReviewTranscriptions","parameters":[project.clone(),session.clone()],"responses":{"200":ok("ReviewTranscriptionList"),"404":error.clone()}},
            "post": {"operationId":"startReviewTranscription","description":"Create or reuse (by audio/provider/settings fingerprint) a durable transcription run. Cloud runs require explicit upload acknowledgement and a hard USD ceiling; no fallback.",
                "parameters":[project.clone(),session.clone()],"requestBody":body.clone(),
                "responses":{"200":ok("ReviewTranscriptionRun"),"400":error.clone(),"404":error.clone(),"409":error.clone()}}
        },
        "/api/v1/projects/{projectId}/review-sessions/{sessionId}/transcriptions/preflight": {
            "post": {"operationId":"preflightReviewTranscription","description":"Readiness and worst-case cost; never sends audio or loads a model.",
                "parameters":[project.clone(),session],"requestBody":body,"responses":{"200":ok("ReviewTranscriptionPreflight"),"400":error.clone(),"404":error.clone()}}
        },
        "/api/v1/projects/{projectId}/review-transcriptions/{runId}": {
            "get": {"operationId":"getReviewTranscription","parameters":[project.clone(),run.clone()],"responses":{"200":ok("ReviewTranscriptionRun"),"404":error.clone()}}
        },
        "/api/v1/projects/{projectId}/review-transcriptions/{runId}/cancel": {
            "post": {"operationId":"cancelReviewTranscription","parameters":[project,run],"responses":{"200":ok("ReviewTranscriptionRun"),"404":error}}
        }
    });
    let open = json!({"type":"object","additionalProperties":true});
    let schemas = json!({
        "ReviewTranscriptionRequest": {"type":"object","additionalProperties":false,"required":["backend"],"properties":{
            "backend":{"type":"string","enum":["local","openrouter"]},
            "model":{"type":["string","null"]},
            "languageHint":{"type":["string","null"],"pattern":"^[a-z]{2}$"},
            "authorizedMaxCostUsd":{"type":["number","null"],"exclusiveMinimum":0,"maximum":100},
            "cloudUploadAcknowledged":{"type":"boolean"},
            "zeroDataRetention":{"type":"boolean"}}},
        "ReviewTranscriptionPreflight": open.clone(),
        "ReviewTranscriptionRun": open.clone(),
        "ReviewTranscriptionList": {"type":"object","required":["items"],"properties":{"items":{"type":"array","items":open}}}
    });
    if let (Some(target), Some(source)) = (document["paths"].as_object_mut(), paths.as_object()) {
        target.extend(source.clone());
    }
    if let (Some(target), Some(source)) = (
        document["components"]["schemas"].as_object_mut(),
        schemas.as_object(),
    ) {
        target.extend(source.clone());
    }
}
