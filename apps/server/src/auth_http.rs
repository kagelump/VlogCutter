//! Bootstrap, session, pairing, and device endpoints.

use super::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BootstrapExchangeRequest {
    pub(super) code: String,
    pub(super) hostname: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SessionCreatedResponse {
    pub(super) request_id: String,
    pub(super) session_id: String,
    pub(super) display_prefix: String,
    pub(super) expires_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SessionsListResponse {
    pub(super) request_id: String,
    pub(super) sessions: Vec<SessionInfo>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SessionRefreshResponse {
    pub(super) request_id: String,
    pub(super) session: SessionInfo,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PairingCodeResponse {
    pub(super) request_id: String,
    pub(super) code: String,
    pub(super) expires_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PairingExchangeRequest {
    pub(super) code: String,
    #[serde(default = "default_device_label")]
    pub(super) label: String,
}

pub(super) fn default_device_label() -> String {
    "MacBook Pro".to_string()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DeviceCreatedResponse {
    pub(super) request_id: String,
    pub(super) credential_id: String,
    pub(super) token: String,
    pub(super) token_prefix: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DevicesListResponse {
    pub(super) request_id: String,
    pub(super) devices: Vec<DeviceCredentialInfo>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AuthDiagnosticsResponse {
    pub(super) request_id: String,
    pub(super) diagnostics: AuthDiagnostics,
}

// ── Auth principal ─────────────────────────────────────────────────

pub(super) async fn exchange_bootstrap_code(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Json(body): Json<BootstrapExchangeRequest>,
) -> Response {
    let hostname = body.hostname.as_deref().unwrap_or("rcdvlog.local");
    match state.auth.exchange_bootstrap(&body.code, hostname) {
        Ok(session) => {
            let cookie_value = format!(
                "rcdvlog_session={}:{}",
                session.session_id,
                session.session_token.expose()
            );
            let set_cookie = format!(
                "{}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=86400",
                cookie_value
            );
            let mut response = Json(SessionCreatedResponse {
                request_id: request_id.0,
                session_id: session.session_id,
                display_prefix: session.display_prefix,
                expires_at: session.expires_at,
            })
            .into_response();
            response.headers_mut().insert(
                header::SET_COOKIE,
                HeaderValue::from_str(&set_cookie).expect("valid cookie header"),
            );
            response.headers_mut().insert(
                header::HeaderName::from_static("x-csrf-token"),
                HeaderValue::from_str(&session.csrf_token).expect("valid csrf token"),
            );
            response
        }
        Err(error) => auth_error_response_with_request(error, &request_id.0),
    }
}

pub(super) async fn list_sessions(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    match state.auth.list_sessions() {
        Ok(sessions) => Json(SessionsListResponse {
            request_id: request_id.0,
            sessions,
        })
        .into_response(),
        Err(error) => auth_error_response_with_request(error, &request_id.0),
    }
}

pub(super) async fn revoke_session(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(session_id): AxumPath<String>,
) -> Response {
    match state.auth.revoke_session(&session_id) {
        Ok(()) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "session.revoked",
            "browser_session",
            Some(&session_id),
            serde_json::json!({}),
        ) {
            Ok(()) => {
                let current = matches!(&principal, AuthPrincipal::Browser { session_id: current, .. } if current == &session_id);
                let mut response = Json(ErrorResponse {
                    request_id: request_id.0,
                    code: "session_revoked",
                })
                .into_response();
                if current {
                    response.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_static("rcdvlog_session=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0"));
                }
                response
            }
            Err(response) => *response,
        },
        Err(error) => auth_error_response_with_request(error, &request_id.0),
    }
}

pub(super) async fn refresh_session(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
) -> Response {
    let session_id = match &principal {
        AuthPrincipal::Browser { session_id, .. } => session_id.clone(),
        _ => {
            return auth_error_response_with_request(AuthError::SessionInvalid, &request_id.0);
        }
    };
    match state.auth.refresh_session(&session_id) {
        Ok(session) => Json(SessionRefreshResponse {
            request_id: request_id.0,
            session,
        })
        .into_response(),
        Err(error) => auth_error_response_with_request(error, &request_id.0),
    }
}

pub(super) async fn create_pairing_code(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
) -> Response {
    let result = match &principal {
        AuthPrincipal::Browser { session_id, .. } => state.auth.create_pairing_code(session_id),
        AuthPrincipal::Device { .. } => state.auth.create_pairing_code_for_device(),
    };
    match result {
        Ok(pairing) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "pairing.created",
            "pairing_code",
            None,
            serde_json::json!({"expiresAt":pairing.expires_at}),
        ) {
            Ok(()) => Json(PairingCodeResponse {
                request_id: request_id.0,
                code: pairing.code.expose().to_string(),
                expires_at: pairing.expires_at,
            })
            .into_response(),
            Err(response) => *response,
        },
        Err(error) => auth_error_response_with_request(error, &request_id.0),
    }
}

pub(super) async fn exchange_pairing_code(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Json(body): Json<PairingExchangeRequest>,
) -> Response {
    match state.auth.exchange_pairing(&body.code, &body.label) {
        Ok(device) => Json(DeviceCreatedResponse {
            request_id: request_id.0,
            credential_id: device.credential_id,
            token: device.token.expose().to_string(),
            token_prefix: device.token_prefix,
        })
        .into_response(),
        Err(error) => auth_error_response_with_request(error, &request_id.0),
    }
}

pub(super) async fn list_devices(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    match state.auth.list_devices() {
        Ok(devices) => Json(DevicesListResponse {
            request_id: request_id.0,
            devices,
        })
        .into_response(),
        Err(error) => auth_error_response_with_request(error, &request_id.0),
    }
}

pub(super) async fn revoke_device(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(credential_id): AxumPath<String>,
) -> Response {
    match state.auth.revoke_device(&credential_id) {
        Ok(()) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "device.revoked",
            "device_credential",
            Some(&credential_id),
            serde_json::json!({}),
        ) {
            Ok(()) => Json(ErrorResponse {
                request_id: request_id.0,
                code: "device_revoked",
            })
            .into_response(),
            Err(response) => *response,
        },
        Err(error) => auth_error_response_with_request(error, &request_id.0),
    }
}

pub(super) async fn auth_diagnostics(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    match state.auth.diagnostics() {
        Ok(diag) => Json(AuthDiagnosticsResponse {
            request_id: request_id.0,
            diagnostics: diag,
        })
        .into_response(),
        Err(error) => auth_error_response_with_request(error, &request_id.0),
    }
}
