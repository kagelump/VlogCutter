//! Thin authenticated adapter over `rcdvlog_server_core::ReviewDiscussions` (#225).
use super::*;
use rcdvlog_server_core::{
    NewDiscussionItem, ReviewActor, ReviewDiscussionError, ReviewDiscussions, ReviewTargetActor,
};

fn discussions(state: &HttpState) -> ReviewDiscussions {
    state.foundation.catalog().review_discussions()
}

/// Only an authenticated browser session is a person who may assert human authorship.
fn actor(principal: &AuthPrincipal) -> ReviewActor {
    match principal {
        AuthPrincipal::Browser { session_id, .. } => ReviewActor {
            actor: ReviewTargetActor::new(AuditActorType::BrowserSession, session_id.as_str()),
            may_assert_human: true,
        },
        AuthPrincipal::Tailnet { login } => ReviewActor {
            actor: ReviewTargetActor::new(AuditActorType::BrowserSession, login.as_str()),
            may_assert_human: true,
        },
        AuthPrincipal::Device { credential_id } => ReviewActor {
            actor: ReviewTargetActor::new(AuditActorType::DeviceCredential, credential_id.as_str()),
            may_assert_human: false,
        },
    }
}

fn error_response(request_id: &RequestId, error: ReviewDiscussionError) -> Response {
    let status = match &error {
        ReviewDiscussionError::ProjectNotFound | ReviewDiscussionError::NotFound => {
            StatusCode::NOT_FOUND
        }
        ReviewDiscussionError::InvalidInput(_) => StatusCode::BAD_REQUEST,
        ReviewDiscussionError::Conflict(_) => StatusCode::CONFLICT,
        ReviewDiscussionError::HumanRequired => StatusCode::FORBIDDEN,
        ReviewDiscussionError::Transcript(_) => StatusCode::NOT_FOUND,
        ReviewDiscussionError::Infrastructure(_) | ReviewDiscussionError::Database(_) => {
            tracing::error!(request_id = %request_id.0, code = error.code(), "review discussion request failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    query_error_response(request_id, error.code(), status)
}

async fn blocking<T: Serialize + Send + 'static>(
    request_id: RequestId,
    work: impl FnOnce() -> Result<T, ReviewDiscussionError> + Send + 'static,
) -> Response {
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => error_response(&request_id, error),
        Err(_) => query_error_response(
            &request_id,
            "internal_error",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CorrectionRequest {
    field: String,
    value: serde_json::Value,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CreateRequest {
    title: String,
    utterance_ids: Vec<String>,
}

fn double_option<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<String>>, D::Error> {
    Ok(Some(Option::deserialize(d)?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct UpdateRequest {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    utterance_ids: Option<Vec<String>>,
    #[serde(default, deserialize_with = "double_option")]
    target_operation_id: Option<Option<String>>,
    #[serde(default)]
    unanchored: Option<bool>,
    #[serde(default)]
    state: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ReconcileRequest {
    canonical_note_id: String,
}

macro_rules! body_or_400 {
    ($body:expr, $request_id:expr) => {
        match $body {
            Ok(Json(value)) => value,
            Err(_) => {
                return query_error_response(&$request_id, "invalid_input", StatusCode::BAD_REQUEST)
            }
        }
    };
}

pub(super) async fn workspace(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, run_id)): AxumPath<(String, String)>,
) -> Response {
    let discussions = discussions(&state);
    blocking(request_id, move || {
        discussions.workspace(&project_id, &run_id)
    })
    .await
}

pub(super) async fn report(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, run_id)): AxumPath<(String, String)>,
) -> Response {
    let discussions = discussions(&state);
    blocking(request_id, move || discussions.report(&project_id, &run_id)).await
}

pub(super) async fn propose(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath((project_id, run_id)): AxumPath<(String, String)>,
) -> Response {
    let discussions = discussions(&state);
    let actor = actor(&principal);
    blocking(request_id, move || {
        discussions.propose(&project_id, &run_id, &actor)
    })
    .await
}

pub(super) async fn create(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath((project_id, run_id)): AxumPath<(String, String)>,
    body: Result<Json<CreateRequest>, JsonRejection>,
) -> Response {
    let body = body_or_400!(body, request_id);
    let discussions = discussions(&state);
    let actor = actor(&principal);
    blocking(request_id, move || {
        discussions.create(
            &project_id,
            &run_id,
            &body.title,
            &body.utterance_ids,
            &actor,
        )
    })
    .await
}

pub(super) async fn correct(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath((project_id, run_id, utterance_id)): AxumPath<(String, String, String)>,
    body: Result<Json<CorrectionRequest>, JsonRejection>,
) -> Response {
    let body = body_or_400!(body, request_id);
    let discussions = discussions(&state);
    let actor = actor(&principal);
    blocking(request_id, move || {
        discussions.correct_utterance(
            &project_id,
            &run_id,
            &utterance_id,
            &body.field,
            body.value,
            body.reason,
            &actor,
        )
    })
    .await
}

pub(super) async fn update(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath((project_id, discussion_id)): AxumPath<(String, String)>,
    body: Result<Json<UpdateRequest>, JsonRejection>,
) -> Response {
    let body = body_or_400!(body, request_id);
    let discussions = discussions(&state);
    let actor = actor(&principal);
    blocking(request_id, move || {
        discussions.update(
            &project_id,
            &discussion_id,
            body.title,
            body.utterance_ids,
            body.target_operation_id,
            body.unanchored,
            body.state,
            &actor,
        )
    })
    .await
}

pub(super) async fn add_item(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath((project_id, discussion_id)): AxumPath<(String, String)>,
    body: Result<Json<NewDiscussionItem>, JsonRejection>,
) -> Response {
    let body = body_or_400!(body, request_id);
    let discussions = discussions(&state);
    let actor = actor(&principal);
    blocking(request_id, move || {
        discussions.add_item(&project_id, &discussion_id, &body, &actor)
    })
    .await
}

pub(super) async fn reconcile(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath((project_id, item_id)): AxumPath<(String, String)>,
    body: Result<Json<ReconcileRequest>, JsonRejection>,
) -> Response {
    let body = body_or_400!(body, request_id);
    if let Err(response) = audit_admin(
        &state,
        &principal,
        &request_id,
        "review_item.reconciled",
        "review_item",
        Some(&item_id),
        serde_json::json!({"canonicalNoteId": body.canonical_note_id}),
    ) {
        return *response;
    }
    let discussions = discussions(&state);
    blocking(request_id, move || {
        discussions.reconcile(&project_id, &item_id, &body.canonical_note_id)
    })
    .await
}

pub(super) fn routes(state: &HttpState) -> Router<HttpState> {
    let run = "/api/v1/projects/{project_id}/review-transcriptions/{run_id}";
    Router::new()
        .route(&format!("{run}/review"), get(workspace))
        .route(&format!("{run}/report"), get(report))
        .route(&format!("{run}/discussions/propose"), post(propose))
        .route(&format!("{run}/discussions"), post(create))
        .route(
            &format!("{run}/utterances/{{utterance_id}}/corrections"),
            post(correct),
        )
        .route(
            "/api/v1/projects/{project_id}/review-discussions/{discussion_id}",
            patch(update),
        )
        .route(
            "/api/v1/projects/{project_id}/review-discussions/{discussion_id}/items",
            post(add_item),
        )
        .route(
            "/api/v1/projects/{project_id}/review-discussion-items/{item_id}/reconciliation",
            post(reconcile),
        )
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), require_csrf))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
}

pub(super) fn openapi_extend(document: &mut serde_json::Value) {
    use serde_json::json;
    let p =
        |name: &str| json!({"name":name,"in":"path","required":true,"schema":{"type":"string"}});
    let open = json!({"type":"object","additionalProperties":true});
    let body = |schema: &str| json!({"required":true,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{schema}")}}}});
    let ok = |schema: &str| json!({"description":schema,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{schema}")}}}});
    let error = json!({"$ref":"#/components/responses/Error"});
    let responses = |schema: &str| json!({"200":ok(schema),"400":error.clone(),"403":error.clone(),"404":error.clone(),"409":error.clone()});
    let run = [p("projectId"), p("runId")];
    let paths = json!({
        "/api/v1/projects/{projectId}/review-transcriptions/{runId}/review": {"get": {"operationId":"getReviewWorkspace","parameters":run,"responses":responses("ReviewWorkspace")}},
        "/api/v1/projects/{projectId}/review-transcriptions/{runId}/report": {"get": {"operationId":"getReviewReport","description":"Deterministic review-report.v1 for a fresh agent, with core-compatible RecordReviewNoteRequest exports.","parameters":run,"responses":responses("ReviewReport")}},
        "/api/v1/projects/{projectId}/review-transcriptions/{runId}/discussions/propose": {"post": {"operationId":"proposeReviewDiscussions","parameters":run,"responses":responses("ReviewWorkspace")}},
        "/api/v1/projects/{projectId}/review-transcriptions/{runId}/discussions": {"post": {"operationId":"createReviewDiscussion","parameters":run,"requestBody":body("CreateReviewDiscussionRequest"),"responses":responses("ReviewWorkspace")}},
        "/api/v1/projects/{projectId}/review-transcriptions/{runId}/utterances/{utteranceId}/corrections": {"post": {"operationId":"correctReviewUtterance","parameters":[p("projectId"),p("runId"),p("utteranceId")],"requestBody":body("ReviewUtteranceCorrection"),"responses":responses("ReviewWorkspace")}},
        "/api/v1/projects/{projectId}/review-discussions/{discussionId}": {"patch": {"operationId":"updateReviewDiscussion","parameters":[p("projectId"),p("discussionId")],"requestBody":body("UpdateReviewDiscussionRequest"),"responses":responses("ReviewWorkspace")}},
        "/api/v1/projects/{projectId}/review-discussions/{discussionId}/items": {"post": {"operationId":"addReviewDiscussionItem","description":"Append an observation, candidate edit, speaker claim, retraction, note or (human only) decision.","parameters":[p("projectId"),p("discussionId")],"requestBody":body("NewReviewDiscussionItem"),"responses":responses("ReviewWorkspace")}},
        "/api/v1/projects/{projectId}/review-discussion-items/{itemId}/reconciliation": {"post": {"operationId":"reconcileReviewDiscussionItem","description":"Record the owning catalog's note ID for an exported item; a different ID is a 409.","parameters":[p("projectId"),p("itemId")],"requestBody":body("ReviewReconciliationRequest"),"responses":responses("ReviewDiscussionItem")}}
    });
    let schemas = json!({
        "ReviewWorkspace": open.clone(), "ReviewReport": open.clone(), "ReviewDiscussionItem": open.clone(),
        "CreateReviewDiscussionRequest": {"type":"object","additionalProperties":false,"required":["title","utteranceIds"],"properties":{"title":{"type":"string"},"utteranceIds":{"type":"array","items":{"type":"string"}}}},
        "ReviewUtteranceCorrection": {"type":"object","additionalProperties":false,"required":["field","value"],"properties":{"field":{"type":"string","enum":["text","speaker","language","movieTime","excluded"]},"value":{},"reason":{"type":["string","null"]}}},
        "UpdateReviewDiscussionRequest": {"type":"object","additionalProperties":false,"properties":{"title":{"type":"string"},"utteranceIds":{"type":"array","items":{"type":"string"}},"targetOperationId":{"type":["string","null"]},"unanchored":{"type":"boolean"},"state":{"type":"string","enum":["open","resolved","deferred"]}}},
        "NewReviewDiscussionItem": {"type":"object","additionalProperties":false,"required":["kind","authorKind","text"],"properties":{
            "kind":{"type":"string","enum":["observation","candidateEdit","speakerClaim","retraction","note","decision"]},
            "authorKind":{"type":"string","enum":["human","agent"]},"text":{"type":"string"},"speaker":{"type":["string","null"]},
            "quoteUtteranceIds":{"type":"array","items":{"type":"string"}},"category":{"type":["string","null"]},"change":{"type":["object","null"]},
            "decision":{"type":["string","null"],"enum":["accepted","declined","deferred",null]},"refersToItemId":{"type":["string","null"]}}},
        "ReviewReconciliationRequest": {"type":"object","additionalProperties":false,"required":["canonicalNoteId"],"properties":{"canonicalNoteId":{"type":"string"}}}
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_browser_sessions_may_assert_human_authorship() {
        let device = actor(&AuthPrincipal::Device {
            credential_id: "cred_agent".into(),
        });
        assert!(!device.may_assert_human);
        assert_eq!(device.actor.actor_type, "device_credential");
    }
}
