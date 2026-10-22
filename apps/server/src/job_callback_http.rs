use super::*;
use rcdvlog_server_core::CallbackDelivery;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct TargetRequest {
    target: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ClaimRequest {
    target: String,
    lease_seconds: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct SettleRequest {
    lease_token: String,
    succeeded: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ListQuery {
    target: String,
    #[serde(default = "default_admin_limit")]
    limit: u32,
    before: Option<u64>,
}

pub(super) async fn subscribe(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(raw): AxumPath<String>,
    body: Result<Json<TargetRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
    };
    let Ok(job) = JobId::parse(raw) else {
        return query_error_response(&request_id, "not_found", StatusCode::NOT_FOUND);
    };
    match state
        .foundation
        .jobs()
        .callbacks()
        .subscribe(&job, &body.target)
    {
        Ok(subscription) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "job.callback_subscribed",
            "job",
            Some(job.as_str()),
            serde_json::json!({"target":body.target}),
        ) {
            Ok(()) => Json(subscription).into_response(),
            Err(response) => *response,
        },
        Err(error) => job_admin_error(&request_id, error),
    }
}

pub(super) async fn list(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    query: Result<Query<ListQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
    };
    match state
        .foundation
        .jobs()
        .callbacks()
        .list(&query.target, query.limit, query.before)
    {
        Ok(page) => Json(page).into_response(),
        Err(error) => job_admin_error(&request_id, error),
    }
}

pub(super) async fn claim(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    body: Result<Json<ClaimRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
    };
    match state
        .foundation
        .jobs()
        .callbacks()
        .claim(&body.target, body.lease_seconds)
    {
        Ok(claim) => {
            if let Some(claim) = &claim
                && let Err(response) = audit_admin(
                    &state,
                    &principal,
                    &request_id,
                    "job.callback_claimed",
                    "callback",
                    Some(&claim.delivery.delivery_id),
                    serde_json::json!({"target":body.target}),
                )
            {
                return *response;
            }
            Json(serde_json::json!({"claim":claim})).into_response()
        }
        Err(error) => job_admin_error(&request_id, error),
    }
}

pub(super) async fn settle(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(id): AxumPath<String>,
    body: Result<Json<SettleRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
    };
    match state
        .foundation
        .jobs()
        .callbacks()
        .settle(&id, &body.lease_token, body.succeeded)
    {
        Ok(delivery) => settled(&state, &principal, &request_id, delivery),
        Err(error) => job_admin_error(&request_id, error),
    }
}

fn settled(
    state: &HttpState,
    principal: &AuthPrincipal,
    request_id: &RequestId,
    delivery: CallbackDelivery,
) -> Response {
    match audit_admin(
        state,
        principal,
        request_id,
        "job.callback_settled",
        "callback",
        Some(&delivery.delivery_id),
        serde_json::json!({"state":delivery.state}),
    ) {
        Ok(()) => Json(delivery).into_response(),
        Err(response) => *response,
    }
}
