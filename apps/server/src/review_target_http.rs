//! Thin authenticated adapter over `rcdvlog_server_core::ReviewTargets` (#221).
//! Validation, digesting, deduplication and conflict policy live in Rust core.
use super::*;
use rcdvlog_server_core::{
    HISTORICAL_REVIEW_ACKNOWLEDGEMENT, HistoricalReviewTargetImportRequest, ImportOutcome,
    REVIEW_TARGET_LIST_MAX_LIMIT, ReviewTargetActor, ReviewTargetImportRequest,
    ReviewTargetImportResult, ReviewTargetServiceError,
};

/// The handoff validator caps the snapshot at 16 MiB; leave room for the envelope.
pub(super) const MAX_REVIEW_TARGET_BODY_BYTES: usize = 16 * 1024 * 1024 + 64 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ListQuery {
    #[serde(default = "default_admin_limit")]
    limit: u32,
    before: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct YoutubeRequest {
    youtube_video_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CandidateQuery {
    start_seconds: f64,
    end_seconds: f64,
}

fn actor(principal: &AuthPrincipal) -> ReviewTargetActor {
    match principal {
        AuthPrincipal::Browser { session_id, .. } => {
            ReviewTargetActor::new(AuditActorType::BrowserSession, session_id.as_str())
        }
        AuthPrincipal::Device { credential_id } => {
            ReviewTargetActor::new(AuditActorType::DeviceCredential, credential_id.as_str())
        }
        AuthPrincipal::Tailnet { login } => {
            ReviewTargetActor::new(AuditActorType::BrowserSession, login.as_str())
        }
    }
}

fn error_response(request_id: &RequestId, error: ReviewTargetServiceError) -> Response {
    let status = match &error {
        ReviewTargetServiceError::ProjectNotFound | ReviewTargetServiceError::NotFound => {
            StatusCode::NOT_FOUND
        }
        ReviewTargetServiceError::Rejected { code, .. }
            if *code == "review_target_conflict" || *code == "review_target_contract_conflict" =>
        {
            StatusCode::CONFLICT
        }
        ReviewTargetServiceError::Rejected { .. } | ReviewTargetServiceError::InvalidInput(_) => {
            StatusCode::BAD_REQUEST
        }
        ReviewTargetServiceError::Infrastructure(_) | ReviewTargetServiceError::Database(_) => {
            tracing::error!(request_id = %request_id.0, error = %error, "review target request failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    query_error_response(request_id, error.code(), status)
}

async fn blocking<T: Send + 'static>(
    request_id: &RequestId,
    work: impl FnOnce() -> Result<T, ReviewTargetServiceError> + Send + 'static,
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

pub(super) async fn list(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(project_id): AxumPath<String>,
    query: Result<Query<ListQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
    };
    if query.limit == 0 || query.limit > REVIEW_TARGET_LIST_MAX_LIMIT {
        return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
    }
    let targets = state.foundation.catalog().review_targets();
    match blocking(&request_id, move || {
        targets.list(&project_id, query.limit, query.before, chrono::Utc::now())
    })
    .await
    {
        Ok(page) => Json(page).into_response(),
        Err(response) => response,
    }
}

pub(super) async fn preflight(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(project_id): AxumPath<String>,
    body: Result<Json<ReviewTargetImportRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
    };
    let targets = state.foundation.catalog().review_targets();
    match blocking(&request_id, move || {
        targets.preflight(&project_id, &body, chrono::Utc::now())
    })
    .await
    {
        Ok(report) => Json(report).into_response(),
        Err(response) => response,
    }
}

pub(super) async fn import(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(project_id): AxumPath<String>,
    body: Result<Json<ReviewTargetImportRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
    };
    let targets = state.foundation.catalog().review_targets();
    let importer = actor(&principal);
    let result = blocking(&request_id, move || {
        targets.import(&project_id, &body, &importer, chrono::Utc::now())
    })
    .await;
    audited_import(
        &state,
        &principal,
        &request_id,
        "review_target.imported",
        result,
    )
}

pub(super) async fn preflight_historical(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(project_id): AxumPath<String>,
    body: Result<Json<HistoricalReviewTargetImportRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
    };
    let targets = state.foundation.catalog().review_targets();
    match blocking(&request_id, move || {
        targets.preflight_historical(&project_id, &body, chrono::Utc::now())
    })
    .await
    {
        Ok(report) => Json(report).into_response(),
        Err(response) => response,
    }
}

pub(super) async fn import_historical(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(project_id): AxumPath<String>,
    body: Result<Json<HistoricalReviewTargetImportRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
    };
    let targets = state.foundation.catalog().review_targets();
    let importer = actor(&principal);
    let result = blocking(&request_id, move || {
        targets.import_historical(&project_id, &body, &importer, chrono::Utc::now())
    })
    .await;
    audited_import(
        &state,
        &principal,
        &request_id,
        "review_target.historical_imported",
        result,
    )
}

/// Audit a created or refreshed import (a reuse changes nothing) and reply.
fn audited_import(
    state: &HttpState,
    principal: &AuthPrincipal,
    request_id: &RequestId,
    action: &str,
    result: Result<ReviewTargetImportResult, Response>,
) -> Response {
    let result = match result {
        Ok(result) => result,
        Err(response) => return response,
    };
    if result.outcome != ImportOutcome::Reused
        && let Err(response) = audit_admin(
            state,
            principal,
            request_id,
            action,
            "review_target",
            Some(&result.review_target.summary.review_target_id),
            serde_json::json!({
                "projectId": result.review_target.summary.project_id,
                "targetDigest": result.review_target.summary.target_digest,
                "contract": result.review_target.summary.contract,
                "outcome": result.outcome,
            }),
        )
    {
        return *response;
    }
    Json(result).into_response()
}

pub(super) async fn get(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, review_target_id)): AxumPath<(String, String)>,
) -> Response {
    let targets = state.foundation.catalog().review_targets();
    match blocking(&request_id, move || {
        targets.get(&project_id, &review_target_id, chrono::Utc::now())
    })
    .await
    {
        Ok(detail) => Json(detail).into_response(),
        Err(response) => response,
    }
}

pub(super) async fn rebind_youtube(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath((project_id, review_target_id)): AxumPath<(String, String)>,
    body: Result<Json<YoutubeRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
    };
    let targets = state.foundation.catalog().review_targets();
    let declarer = actor(&principal);
    let parent = review_target_id.clone();
    let result = match blocking(&request_id, move || {
        targets.rebind_youtube(
            &project_id,
            &review_target_id,
            body.youtube_video_id.as_deref(),
            &declarer,
            chrono::Utc::now(),
        )
    })
    .await
    {
        Ok(result) => result,
        Err(response) => return response,
    };
    if result.outcome == ImportOutcome::Created
        && let Err(response) = audit_admin(
            &state,
            &principal,
            &request_id,
            "review_target.youtube_declared",
            "review_target",
            Some(&result.review_target.summary.review_target_id),
            serde_json::json!({
                "derivedFromReviewTargetId": parent,
                "youtubeVideoId": result.review_target.summary.youtube.video_id,
            }),
        )
    {
        return *response;
    }
    Json(result).into_response()
}

pub(super) async fn candidates(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, review_target_id)): AxumPath<(String, String)>,
    query: Result<Query<CandidateQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
    };
    let targets = state.foundation.catalog().review_targets();
    match blocking(&request_id, move || {
        targets.candidates(
            &project_id,
            &review_target_id,
            query.start_seconds,
            query.end_seconds,
        )
    })
    .await
    {
        Ok(candidates) => Json(candidates).into_response(),
        Err(response) => response,
    }
}

/// Rust-owned OpenAPI fragment, merged into the server document.
pub(super) fn openapi_extend(document: &mut serde_json::Value) {
    use serde_json::json;
    let project =
        json!({"name":"projectId","in":"path","required":true,"schema":{"type":"string"}});
    let target =
        json!({"name":"reviewTargetId","in":"path","required":true,"schema":{"type":"string"}});
    let body = |schema: &str| json!({"required":true,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{schema}")}}}});
    let ok = |schema: &str| json!({"description":schema,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{schema}")}}}});
    let error = json!({"$ref":"#/components/responses/Error"});
    let paths = json!({
        "/api/v1/projects/{projectId}/review-targets": {"get": {
            "operationId": "listReviewTargets",
            "parameters": [project.clone(),
                {"name":"limit","in":"query","schema":{"type":"integer","minimum":1,"maximum":REVIEW_TARGET_LIST_MAX_LIMIT}},
                {"name":"before","in":"query","schema":{"type":"integer"}}],
            "responses": {"200": ok("ReviewTargetPage"), "400": error.clone(), "401": error.clone(), "404": error.clone()}
        }},
        "/api/v1/projects/{projectId}/review-targets/preflight": {"post": {
            "operationId": "preflightReviewTarget",
            "description": "Non-mutating validation of a version-1 TV review target handoff against the shared Rust validator, freshness/availability policy and stored targets.",
            "parameters": [project.clone()],
            "requestBody": body("ReviewTargetImportRequest"),
            "responses": {"200": ok("ReviewTargetPreflight"), "400": error.clone(), "401": error.clone(), "404": error.clone()}
        }},
        "/api/v1/projects/{projectId}/review-targets/import": {"post": {
            "operationId": "importReviewTarget",
            "description": "Import or reuse an immutable review target. Never overwrites; a conflicting reuse of a viewed artifact returns 409 review_target_conflict.",
            "parameters": [project.clone()],
            "requestBody": body("ReviewTargetImportRequest"),
            "responses": {"200": ok("ReviewTargetImportResult"), "400": error.clone(), "401": error.clone(), "404": error.clone(), "409": error.clone()}
        }},
        "/api/v1/projects/{projectId}/review-targets/historical/preflight": {"post": {
            "operationId": "preflightHistoricalReviewTarget",
            "description": "Non-mutating validation of a HistoricalRenderedArtifactReview v1 (#232): integrity of an already rendered movie only. Requires the explicit acknowledgement; current sources, constraints and renderability are reported as not established.",
            "parameters": [project.clone()],
            "requestBody": body("HistoricalReviewTargetImportRequest"),
            "responses": {"200": ok("ReviewTargetPreflight"), "400": error.clone(), "401": error.clone(), "404": error.clone()}
        }},
        "/api/v1/projects/{projectId}/review-targets/historical/import": {"post": {
            "operationId": "importHistoricalReviewTarget",
            "description": "Import or reuse a historical rendered-artifact review under contract historicalRenderedArtifactReviewV1. It can never be read back as a strict review target; a snapshot already stored under the other contract returns 409 review_target_contract_conflict.",
            "parameters": [project.clone()],
            "requestBody": body("HistoricalReviewTargetImportRequest"),
            "responses": {"200": ok("ReviewTargetImportResult"), "400": error.clone(), "401": error.clone(), "404": error.clone(), "409": error.clone()}
        }},
        "/api/v1/projects/{projectId}/review-targets/{reviewTargetId}": {"get": {
            "operationId": "getReviewTarget",
            "parameters": [project.clone(), target.clone()],
            "responses": {"200": ok("ReviewTargetDetail"), "401": error.clone(), "404": error.clone()}
        }},
        "/api/v1/projects/{projectId}/review-targets/{reviewTargetId}/youtube": {"post": {
            "operationId": "declareReviewTargetYoutube",
            "description": "Declare or clear the YouTube video ID. Creates or reuses a separate immutable target; the original is retained.",
            "parameters": [project.clone(), target.clone()],
            "requestBody": body("ReviewTargetYoutubeRequest"),
            "responses": {"200": ok("ReviewTargetImportResult"), "400": error.clone(), "401": error.clone(), "404": error.clone()}
        }},
        "/api/v1/projects/{projectId}/review-targets/{reviewTargetId}/candidates": {"get": {
            "operationId": "getReviewTargetCandidates",
            "description": "Picture-locator candidates for an elapsed movie time point or range. Not a source trim or confirmed target.",
            "parameters": [project, target,
                {"name":"startSeconds","in":"query","required":true,"schema":{"type":"number","minimum":0}},
                {"name":"endSeconds","in":"query","required":true,"schema":{"type":"number","minimum":0}}],
            "responses": {"200": ok("ReviewTargetCandidates"), "400": error.clone(), "401": error.clone(), "404": error}
        }}
    });
    let open = json!({"type":"object","additionalProperties":true});
    let schemas = json!({
        "ReviewTargetImportRequest": {
            "type":"object","additionalProperties":false,"required":["handoff"],
            "properties":{
                "handoff":{"description":"The data member of edit-plan.review-target-export; see docs/contracts/review-target-v1.schema.json","type":"object"},
                "declaredIssuer":{"type":["string","null"],"maxLength":64},
                "availability":{"type":["object","null"],"additionalProperties":false,"required":["state","observedAt"],"properties":{
                    "state":{"type":"string","enum":["unknown","present","missing","modified"]},
                    "observedAt":{"type":"string","format":"date-time"}}},
                "maxVerificationAgeSeconds":{"type":["integer","null"],"minimum":1}
            }
        },
        "HistoricalReviewTargetImportRequest": {
            "type":"object","additionalProperties":false,"required":["review","acknowledgement"],
            "properties":{
                "review":{"description":"The data member of edit-plan.historical-review-target-export; see docs/contracts/historical-review-v1.schema.json","type":"object"},
                "acknowledgement":{"type":"string","enum":[HISTORICAL_REVIEW_ACKNOWLEDGEMENT]},
                "declaredIssuer":{"type":["string","null"],"maxLength":64},
                "availability":{"type":["object","null"],"additionalProperties":false,"required":["state","observedAt"],"properties":{
                    "state":{"type":"string","enum":["unknown","present","missing","modified"]},
                    "observedAt":{"type":"string","format":"date-time"}}},
                "maxVerificationAgeSeconds":{"type":["integer","null"],"minimum":1}
            }
        },
        "ReviewTargetYoutubeRequest": {
            "type":"object","additionalProperties":false,"required":["youtubeVideoId"],
            "properties":{"youtubeVideoId":{"type":["string","null"],"pattern":"^[A-Za-z0-9_-]{11}$"}}
        },
        "ReviewTargetPreflight": {"type":"object","required":["outcome","importable","checks","conflictingReviewTargetIds","maxVerificationAgeSeconds"],"properties":{
            "outcome":{"type":"string","enum":["new","duplicate","refresh","conflict","blocked"]},
            "importable":{"type":"boolean"},
            "checks":{"type":"array","items":{"type":"object","required":["name","status","detail"],"properties":{
                "name":{"type":"string"},"status":{"type":"string","enum":["pass","warn","fail"]},
                "code":{"type":["string","null"]},"detail":{"type":"string"}}}},
            "preview":{"type":["object","null"],"additionalProperties":true},
            "existingReviewTargetId":{"type":["string","null"]},
            "conflictingReviewTargetIds":{"type":"array","items":{"type":"string"}},
            "maxVerificationAgeSeconds":{"type":"integer"}}},
        "ReviewTargetPage": {"type":"object","required":["items"],"properties":{
            "items":{"type":"array","items":{"$ref":"#/components/schemas/ReviewTargetSummary"}},
            "nextBefore":{"type":["integer","null"]}}},
        "ReviewTargetSummary": open.clone(),
        "ReviewTargetDetail": open.clone(),
        "ReviewTargetImportResult": {"type":"object","required":["outcome","reviewTarget"],"properties":{
            "outcome":{"type":"string","enum":["created","reused","refreshed"]},
            "reviewTarget":{"$ref":"#/components/schemas/ReviewTargetDetail"}}},
        "ReviewTargetCandidates": {"type":"object","required":["reviewTargetId","startSeconds","endSeconds","operationIds"],"properties":{
            "reviewTargetId":{"type":"string"},"startSeconds":{"type":"number"},"endSeconds":{"type":"number"},
            "operationIds":{"type":"array","items":{"type":"string"}}}}
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
