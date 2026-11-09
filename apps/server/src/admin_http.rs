//! Admin overview and audit log.

use super::*;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AdminOverviewResponse {
    pub(super) application_version: &'static str,
    pub(super) protocol_major: u32,
    pub(super) protocol_minor: u32,
    pub(super) control_schema_version: u32,
    pub(super) node_id: String,
    pub(super) readiness: ReadinessReport,
    pub(super) config: rcdvlog_server_core::ConfigDiagnostics,
    pub(super) auth: AuthDiagnostics,
    pub(super) derived_available_bytes: Option<u64>,
    pub(super) backup_available_bytes: Option<u64>,
}

pub(super) async fn admin_overview(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    let readiness = state.readiness_gate.evaluate().await;
    let auth = match state.auth.diagnostics() {
        Ok(value) => value,
        Err(error) => return auth_error_response_with_request(error, &request_id.0),
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
pub(super) struct AdminAuditQuery {
    #[serde(default = "default_admin_limit")]
    pub(super) limit: u32,
    pub(super) before: Option<u64>,
}

pub(super) fn default_admin_limit() -> u32 {
    50
}

pub(super) async fn list_admin_audit(
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

pub(super) fn audit_admin(
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
        AuthPrincipal::Tailnet { login } => (AuditActorType::BrowserSession, login.as_str()),
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
