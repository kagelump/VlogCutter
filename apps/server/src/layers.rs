//! Request middleware: request ids, transport/host validation, authentication, CSRF, and security headers.

use super::*;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(super) enum AuthPrincipal {
    Browser {
        session_id: String,
        csrf_token: String,
    },
    Device {
        credential_id: String,
    },
    /// Tailnet user identified by Tailscale Serve headers (opt-in).
    Tailnet {
        login: String,
    },
}

pub(super) async fn auth_middleware(
    State(state): State<HttpState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let auth = state.auth.clone();
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(|value| value.0.as_str())
        .unwrap_or("")
        .to_string();

    // Try device bearer token first
    if let Some(auth_header) = request.headers().get(header::AUTHORIZATION)
        && let Ok(value) = auth_header.to_str()
        && let Some(token) = value.strip_prefix("Bearer ")
    {
        match auth.validate_device(token) {
            Ok(credential_id) => {
                request
                    .extensions_mut()
                    .insert(AuthPrincipal::Device { credential_id });
                return next.run(request).await;
            }
            Err(AuthError::DeviceNotFound) => {}
            Err(AuthError::DeviceRevoked) => {
                return auth_error_response_with_request(AuthError::DeviceRevoked, &request_id);
            }
            Err(_) => {}
        }
    }

    // Tailscale Serve injects identity headers on tailnet-only requests.
    if state.foundation.config().tailscale_trusted_auth
        && let Some(login) = request
            .headers()
            .get("tailscale-user-login")
            .and_then(|v| v.to_str().ok())
            .filter(|v| !v.is_empty() && v.len() < 200)
    {
        let login = login.to_string();
        request
            .extensions_mut()
            .insert(AuthPrincipal::Tailnet { login });
        return next.run(request).await;
    }

    // Try session cookie
    if let Some(cookie_header) = request.headers().get(header::COOKIE)
        && let Ok(cookie_str) = cookie_header.to_str()
        && let Some((session_id, session_token)) = parse_session_cookie(cookie_str)
    {
        let csrf_token = request
            .headers()
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        match auth.validate_session(&session_id, &session_token) {
            Ok(()) => {
                request.extensions_mut().insert(AuthPrincipal::Browser {
                    session_id,
                    csrf_token,
                });
                return next.run(request).await;
            }
            Err(AuthError::SessionInvalid) => {}
            Err(e) => {
                return auth_error_response_with_request(e, &request_id);
            }
        }
    }

    auth_error_response_with_request(AuthError::SessionInvalid, &request_id)
}

pub(super) async fn require_csrf(
    State(state): State<HttpState>,
    Extension(principal): Extension<AuthPrincipal>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if request.method() == Method::GET || request.method() == Method::HEAD {
        return next.run(request).await;
    }

    if let AuthPrincipal::Browser {
        session_id,
        csrf_token,
    } = &principal
    {
        let request_id = request
            .extensions()
            .get::<RequestId>()
            .map(|request_id| request_id.0.as_str())
            .unwrap_or("");
        if csrf_token.is_empty() {
            return auth_error_response_with_request(AuthError::CsrfMismatch, request_id);
        }
        // Session was already validated by auth_middleware; only verify the
        // CSRF token against the stored hash.
        if state
            .auth
            .validate_csrf_token(session_id, csrf_token)
            .is_err()
        {
            return auth_error_response_with_request(AuthError::CsrfMismatch, request_id);
        }
    }
    next.run(request).await
}

pub(super) async fn limit_open_auth_attempts(
    State(state): State<HttpState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if !state.auth_rate_limiter.allow(Instant::now()) {
        let request_id = request
            .extensions()
            .get::<RequestId>()
            .cloned()
            .unwrap_or_else(|| RequestId(String::new()));
        return query_error_response(&request_id, "rate_limited", StatusCode::TOO_MANY_REQUESTS);
    }
    next.run(request).await
}

pub(super) async fn validate_transport_request(
    State(state): State<HttpState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .cloned()
        .unwrap_or_else(|| RequestId(String::new()));
    let headers = request.headers();
    let host = match validated_host(headers.get(header::HOST), state.foundation.config()) {
        Ok(host) => host,
        Err(code) => return query_error_response(&request_id, code, StatusCode::FORBIDDEN),
    };
    let forwarded_host = match validated_host(
        headers.get(header::HeaderName::from_static("x-forwarded-host")),
        state.foundation.config(),
    ) {
        Ok(host) => host,
        Err(code) => return query_error_response(&request_id, code, StatusCode::FORBIDDEN),
    };
    if host.is_some() && forwarded_host.is_some() && host != forwarded_host {
        return query_error_response(
            &request_id,
            "invalid_forwarded_headers",
            StatusCode::FORBIDDEN,
        );
    }
    if headers.contains_key(header::FORWARDED) {
        return query_error_response(
            &request_id,
            "invalid_forwarded_headers",
            StatusCode::FORBIDDEN,
        );
    }
    let forwarded_scheme = match headers
        .get(header::HeaderName::from_static("x-forwarded-proto"))
        .and_then(|value| value.to_str().ok())
    {
        Some("http") => Some("http"),
        Some("https") => Some("https"),
        Some(_) => {
            return query_error_response(
                &request_id,
                "invalid_forwarded_headers",
                StatusCode::FORBIDDEN,
            );
        }
        None => None,
    };
    if let Some(origin) = headers.get(header::ORIGIN) {
        let Ok(origin) = origin.to_str() else {
            return query_error_response(&request_id, "origin_mismatch", StatusCode::FORBIDDEN);
        };
        let Ok(uri) = origin.parse::<axum::http::Uri>() else {
            return query_error_response(&request_id, "origin_mismatch", StatusCode::FORBIDDEN);
        };
        let expected_host = forwarded_host.as_deref().or(host.as_deref());
        let actual_host = uri
            .authority()
            .map(|authority| authority.as_str().to_ascii_lowercase());
        let actual_scheme = uri.scheme_str();
        let expected_scheme = forwarded_scheme.unwrap_or("http");
        if actual_host.as_deref() != expected_host || actual_scheme != Some(expected_scheme) {
            return query_error_response(&request_id, "origin_mismatch", StatusCode::FORBIDDEN);
        }
    }
    next.run(request).await
}

pub(super) fn validated_host(
    value: Option<&HeaderValue>,
    config: &rcdvlog_server_core::ServerConfig,
) -> std::result::Result<Option<String>, &'static str> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.to_str().map_err(|_| "invalid_host")?;
    let authority = value
        .parse::<axum::http::uri::Authority>()
        .map_err(|_| "invalid_host")?;
    let hostname = authority.host().to_ascii_lowercase();
    let allowed = hostname == "localhost"
        || hostname == config.listen_addr.ip().to_string()
        || hostname == config.lan_hostname.to_ascii_lowercase()
        || hostname.ends_with(".ts.net");
    if !allowed {
        return Err("invalid_host");
    }
    Ok(Some(authority.as_str().to_ascii_lowercase()))
}

pub(super) async fn security_headers(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    headers.insert(
        header::HeaderName::from_static("referrer-policy"),
        // YouTube embeds require the cross-origin Referer to identify this app.
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers.insert(
        header::HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(
            "default-src 'self'; base-uri 'self'; frame-ancestors 'none'; object-src 'none'; connect-src 'self'; script-src 'self' https://www.youtube.com https://s.ytimg.com; frame-src https://www.youtube.com",
        ),
    );
    headers.insert(
        header::HeaderName::from_static("permissions-policy"),
        // Microphone is allowed for this origin only: TV voice review (#222)
        // records from Chrome. Camera and geolocation stay disabled.
        HeaderValue::from_static("camera=(), microphone=(self), geolocation=()"),
    );
    response
}

pub(super) fn parse_session_cookie(cookie_str: &str) -> Option<(String, String)> {
    for part in cookie_str.split(';') {
        let trimmed = part.trim();
        if let Some(value) = trimmed.strip_prefix("rcdvlog_session=") {
            let decoded = value.trim_matches('"');
            let mut parts = decoded.splitn(2, ':');
            let session_id = parts.next()?.to_string();
            let session_token = parts.next()?.to_string();
            if session_id.is_empty() || session_token.is_empty() {
                return None;
            }
            return Some((session_id, session_token));
        }
    }
    None
}

pub(super) fn auth_error_response_with_request(error: AuthError, request_id: &str) -> Response {
    let (status, code) = match &error {
        AuthError::SessionInvalid
        | AuthError::DeviceNotFound
        | AuthError::BootstrapNotFoundOrExpired
        | AuthError::PairingNotFoundOrExpired => (StatusCode::UNAUTHORIZED, error.code()),
        AuthError::DeviceRevoked
        | AuthError::BootstrapAlreadyUsed
        | AuthError::PairingAlreadyUsed => (StatusCode::FORBIDDEN, error.code()),
        AuthError::CsrfMismatch => (StatusCode::FORBIDDEN, error.code()),
        AuthError::Database(_) => (StatusCode::INTERNAL_SERVER_ERROR, error.code()),
    };
    (
        status,
        Json(ErrorResponse {
            request_id: request_id.to_string(),
            code,
        }),
    )
        .into_response()
}

pub(super) async fn request_context(mut request: Request<Body>, next: Next) -> Response {
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| valid_request_id(value))
        .map(str::to_string)
        .unwrap_or_else(|| format!("request_{}", Uuid::new_v4().simple()));
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let started = Instant::now();
    let span = tracing::info_span!(
        "http_request",
        component = "http",
        requestId = %request_id,
        method = %method,
        path = %path,
    );
    let mut response = next.run(request).instrument(span.clone()).await;
    response.headers_mut().insert(
        header::HeaderName::from_static(REQUEST_ID_HEADER),
        HeaderValue::from_str(&request_id).expect("validated request ID is a valid header value"),
    );
    tracing::info!(
        parent: &span,
        component = "http",
        requestId = %request_id,
        status = response.status().as_u16(),
        elapsedMs = started.elapsed().as_millis() as u64,
        "request_complete"
    );
    response
}

pub(super) fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REQUEST_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}
