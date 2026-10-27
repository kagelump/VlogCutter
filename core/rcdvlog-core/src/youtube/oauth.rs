use super::credentials::Credentials;
use super::{
    CredentialStore, Endpoints, UPLOAD_SCOPE, exact_scope, failure, http_client, now,
    response_json, transport,
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{RngCore, rngs::OsRng};
use reqwest::{Url, blocking::Client};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct LoginOptions {
    pub client_file: PathBuf,
    /// Zero selects an ephemeral desktop port. HTTPS callbacks require a fixed port.
    pub port: u16,
    pub timeout_seconds: u64,
    pub open_browser: bool,
    /// Registered HTTPS callback for a Web application client behind a trusted proxy.
    pub redirect_uri: Option<String>,
}

struct OAuthClient {
    id: String,
    secret: String,
    redirect_uri: Option<String>,
}

impl OAuthClient {
    fn load(options: &LoginOptions) -> Result<Self> {
        let file = File::open(&options.client_file).context("open OAuth client file")?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file(),
            "invalid input: OAuth client must be a regular file"
        );
        let mut bytes = Vec::new();
        file.take(65537)
            .read_to_end(&mut bytes)
            .context("read OAuth client file")?;
        ensure!(
            bytes.len() <= 65536,
            "invalid input: OAuth client file exceeds size limit"
        );
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| super::YoutubeError {
            code: "youtube.invalid_client", message: "Invalid OAuth client JSON; download a Desktop or Web application client from Google Cloud",
        })?;
        let web = value.get("web");
        let installed = value.get("installed");
        if web.is_some() == installed.is_some() {
            return failure(
                "youtube.invalid_client",
                "Use exactly one Google Desktop or Web application client",
            );
        }
        let (client, redirect_uri) = if let Some(web) = web {
            let Some(redirect) = &options.redirect_uri else {
                return failure(
                    "youtube.invalid_redirect_uri",
                    "A Web application client requires --redirect-uri with its registered HTTPS callback and a fixed --port",
                );
            };
            validate_https_redirect(redirect)?;
            if options.port == 0 {
                return failure(
                    "youtube.invalid_redirect_uri",
                    "HTTPS callback mode requires a fixed --port matching the trusted proxy's loopback target",
                );
            }
            if !web["redirect_uris"]
                .as_array()
                .is_some_and(|uris| uris.iter().any(|uri| uri.as_str() == Some(redirect)))
            {
                return failure(
                    "youtube.invalid_redirect_uri",
                    "The HTTPS callback must exactly match a redirect_uris entry in the downloaded Web client JSON",
                );
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                ensure!(
                    metadata.mode() & 0o077 == 0 && metadata.uid() == unsafe { libc::geteuid() },
                    "invalid input: Web OAuth client JSON must be owned by you with permissions 0600"
                );
            }
            (web, Some(redirect.clone()))
        } else {
            if options.redirect_uri.is_some() {
                return failure(
                    "youtube.invalid_client",
                    "An HTTPS callback requires a Google Web application client; Desktop clients use the local callback",
                );
            }
            (installed.unwrap(), None)
        };
        let id = client["client_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("invalid input: OAuth client ID missing")?
            .to_owned();
        let secret = client["client_secret"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("invalid input: OAuth client secret missing")?
            .to_owned();
        // Never use endpoint URLs from client JSON. Requests go only to Google.
        Ok(Self {
            id,
            secret,
            redirect_uri,
        })
    }
}

pub(super) fn validate_https_redirect(redirect: &str) -> Result<Url> {
    let invalid = || super::YoutubeError {
        code: "youtube.invalid_redirect_uri",
        message: "Use a canonical HTTPS callback URL with a DNS hostname and a non-root path, without credentials, query, fragment, or percent-encoded path",
    };
    let url = Url::parse(redirect).map_err(|_| invalid())?;
    let domain = url.domain().unwrap_or_default();
    if url.scheme() != "https"
        || !domain.contains('.')
        || domain == "localhost"
        || domain.ends_with(".localhost")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() == "/"
        || url.path().contains('%')
        || url.as_str() != redirect
    {
        return Err(invalid().into());
    }
    Ok(url)
}

#[derive(Clone)]
pub(super) struct CallbackRoute {
    url: Url,
    proxied: bool,
}

impl CallbackRoute {
    pub(super) fn desktop(port: u16) -> Self {
        Self {
            url: Url::parse(&format!("http://127.0.0.1:{port}/oauth/callback")).unwrap(),
            proxied: false,
        }
    }

    pub(super) fn https(redirect: &str) -> Result<Self> {
        Ok(Self {
            url: validate_https_redirect(redirect)?,
            proxied: true,
        })
    }

    fn authority(&self) -> String {
        let host = self.url.host_str().unwrap();
        match self.url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        }
    }
}

pub fn login(
    store: &CredentialStore,
    options: &LoginOptions,
    progress: impl FnMut(Value),
) -> Result<super::AuthStatus> {
    login_with(
        store,
        options,
        &http_client()?,
        &Endpoints::default(),
        progress,
    )
}

pub(super) fn login_with(
    store: &CredentialStore,
    options: &LoginOptions,
    http: &Client,
    endpoints: &Endpoints,
    mut progress: impl FnMut(Value),
) -> Result<super::AuthStatus> {
    ensure!(
        (1..=3600).contains(&options.timeout_seconds),
        "invalid input: OAuth timeout must be 1..3600 seconds"
    );
    let client = OAuthClient::load(options)?;
    let _lock = store.lock()?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, options.port)).context(
        "bind OAuth loopback listener; select a free --port and matching proxy or SSH forward",
    )?;
    let port = listener.local_addr()?.port();
    let route = if let Some(redirect) = &client.redirect_uri {
        CallbackRoute::https(redirect)?
    } else {
        CallbackRoute::desktop(port)
    };
    let redirect = route.url.as_str();
    let verifier = random_secret();
    let state = random_secret();
    let authorization = authorization_url(endpoints, &client.id, redirect, &verifier, &state)?;
    let mut event = json!({
        "event":"youtube.oauth.authorization_required", "authorizationUrl":authorization,
        "redirectUri":redirect, "port":port, "expiresInSeconds":options.timeout_seconds,
        "scope":UPLOAD_SCOPE, "callbackMode":if route.proxied { "https_proxy" } else { "loopback" },
    });
    if route.proxied {
        let mut health = route.url.clone();
        health.set_path("/health");
        event["healthUrl"] = json!(health.as_str());
        event["loopbackTarget"] = json!(format!("http://127.0.0.1:{port}"));
    } else {
        event["sshForward"] = json!(format!("ssh -N -L {port}:127.0.0.1:{port} <upload-host>"));
    }
    progress(event);
    if options.open_browser {
        open_browser(&authorization);
    }
    let mut callback = receive_callback(
        &listener,
        &route,
        &state,
        Duration::from_secs(options.timeout_seconds),
    )?;
    let result = (|| -> Result<super::AuthStatus> {
        let credentials = exchange(
            http,
            endpoints,
            &client.id,
            &client.secret,
            redirect,
            &verifier,
            &callback.code,
        )?;
        store.save(&credentials)?;
        store.status()
    })();
    // The phone only sees success after token exchange and persistent storage.
    match &result {
        Ok(_) => reply(
            &mut callback.stream,
            "200 OK",
            "YouTube connected",
            "Your upload credential is saved on the upload host. You can return to Codex.",
            route.proxied,
        ),
        Err(_) => reply(
            &mut callback.stream,
            "400 Bad Request",
            "Authorization could not finish",
            "Authorization could not be confirmed. Return to Codex for the next step.",
            route.proxied,
        ),
    }
    result
}

fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub(super) fn authorization_url(
    endpoints: &Endpoints,
    client: &str,
    redirect: &str,
    verifier: &str,
    state: &str,
) -> Result<String> {
    let mut url = Url::parse(&endpoints.authorize)?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    url.query_pairs_mut().extend_pairs([
        ("client_id", client),
        ("redirect_uri", redirect),
        ("response_type", "code"),
        ("scope", UPLOAD_SCOPE),
        ("access_type", "offline"),
        ("prompt", "consent select_account"),
        ("include_granted_scopes", "false"),
        ("state", state),
        ("code_challenge_method", "S256"),
        ("code_challenge", &challenge),
    ]);
    Ok(url.into())
}

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let executable = "open";
    #[cfg(not(target_os = "macos"))]
    let executable = "xdg-open";
    // Browser output must never contaminate the CLI JSON envelope.
    let _ = Command::new(executable)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

pub(super) struct PendingCallback {
    pub code: String,
    pub stream: TcpStream,
}

enum CallbackRequest {
    Invalid,
    Health,
    Authorized(String),
    Denied,
}

pub(super) fn receive_callback(
    listener: &TcpListener,
    route: &CallbackRoute,
    state: &str,
    timeout: Duration,
) -> Result<PendingCallback> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                stream.set_read_timeout(Some(remaining.min(Duration::from_secs(2))))?;
                stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                match callback(&mut stream, route, state) {
                    Ok(CallbackRequest::Authorized(code)) => {
                        return Ok(PendingCallback { code, stream });
                    }
                    Ok(CallbackRequest::Denied) => {
                        reply(
                            &mut stream,
                            "400 Bad Request",
                            "Authorization declined",
                            "No new upload credential was saved. You can return to Codex.",
                            route.proxied,
                        );
                        return failure(
                            "youtube.auth_denied",
                            "Google authorization was declined; no credential was saved",
                        );
                    }
                    Ok(CallbackRequest::Health) => reply(
                        &mut stream,
                        "200 OK",
                        "Callback ready",
                        "This private callback is reachable. Return to Codex and open the Google authorization link.",
                        route.proxied,
                    ),
                    _ => reply(
                        &mut stream,
                        "400 Bad Request",
                        "Invalid callback",
                        "This is not a valid authorization callback. Use the authorization link provided by Codex.",
                        route.proxied,
                    ),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(error) => return Err(error).context("accept OAuth callback"),
        }
    }
    failure(
        "youtube.auth_timeout",
        "OAuth login timed out; rerun youtube auth login",
    )
}

// Desktop fixtures retain their original small handshake helper.
#[cfg(test)]
pub(super) fn receive_code(
    listener: &TcpListener,
    state: &str,
    timeout: Duration,
) -> Result<String> {
    let route = CallbackRoute::desktop(listener.local_addr()?.port());
    let mut callback = receive_callback(listener, &route, state, timeout)?;
    reply(
        &mut callback.stream,
        "200 OK",
        "Callback received",
        "Return to Codex.",
        false,
    );
    Ok(callback.code)
}

fn callback(
    stream: &mut TcpStream,
    route: &CallbackRoute,
    expected_state: &str,
) -> Result<CallbackRequest> {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    while bytes.len() < 8192 {
        if stream.read(&mut byte)? == 0 {
            return Ok(CallbackRequest::Invalid);
        }
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    if !bytes.ends_with(b"\r\n\r\n") {
        return Ok(CallbackRequest::Invalid);
    }
    let request = std::str::from_utf8(&bytes)?;
    let mut lines = request.split("\r\n");
    let first: Vec<_> = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    if first.len() != 3 || first[0] != "GET" || !["HTTP/1.0", "HTTP/1.1"].contains(&first[2]) {
        return Ok(CallbackRequest::Invalid);
    }
    let authority = route.authority();
    let mut hosts = Vec::new();
    let mut forwarded_hosts = Vec::new();
    let mut forwarded_protos = Vec::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            return Ok(CallbackRequest::Invalid);
        };
        if name.eq_ignore_ascii_case("origin") || name.eq_ignore_ascii_case("forwarded") {
            return Ok(CallbackRequest::Invalid);
        }
        if name.eq_ignore_ascii_case("host") {
            hosts.push(value.trim());
        }
        if name.eq_ignore_ascii_case("x-forwarded-host") {
            forwarded_hosts.push(value.trim());
        }
        if name.eq_ignore_ascii_case("x-forwarded-proto") {
            forwarded_protos.push(value.trim());
        }
    }
    // The proxy must preserve Host. Forwarding headers never override it.
    if hosts != [authority.as_str()]
        || (!route.proxied && (!forwarded_hosts.is_empty() || !forwarded_protos.is_empty()))
        || (!forwarded_hosts.is_empty() && forwarded_hosts != [authority.as_str()])
        || (!forwarded_protos.is_empty() && forwarded_protos != ["https"])
    {
        return Ok(CallbackRequest::Invalid);
    }
    if route.proxied && first[1] == "/health" {
        return Ok(CallbackRequest::Health);
    }
    let Some((path, query)) = first[1].split_once('?') else {
        return Ok(CallbackRequest::Invalid);
    };
    if path != route.url.path() || query.contains('#') {
        return Ok(CallbackRequest::Invalid);
    }
    let url = Url::parse(&format!("{}?{query}", route.url))?;
    let pairs: Vec<_> = url.query_pairs().collect();
    let states: Vec<_> = pairs
        .iter()
        .filter(|(k, _)| k == "state")
        .map(|(_, v)| v.as_ref())
        .collect();
    if states.len() != 1
        || Sha256::digest(states[0].as_bytes()) != Sha256::digest(expected_state.as_bytes())
    {
        return Ok(CallbackRequest::Invalid);
    }
    let errors: Vec<_> = pairs.iter().filter(|(k, _)| k == "error").collect();
    let codes: Vec<_> = pairs
        .iter()
        .filter(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string())
        .collect();
    if errors.len() == 1 && codes.is_empty() {
        return Ok(CallbackRequest::Denied);
    }
    if !errors.is_empty() || codes.len() != 1 || codes[0].is_empty() {
        return Ok(CallbackRequest::Invalid);
    }
    Ok(CallbackRequest::Authorized(codes[0].clone()))
}

fn reply(stream: &mut TcpStream, status: &str, title: &str, body: &str, https: bool) {
    // Only static application text is rendered, never callback parameters or errors.
    let html = format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{title}</title><style>body{{font:18px/1.6 system-ui,sans-serif;background:#f1f5f4;color:#142c28;margin:0;padding:12vh 24px}}main{{max-width:440px;margin:auto;background:white;padding:28px;border-radius:16px}}h1{{font-size:26px;line-height:1.2}}</style><main><h1>{title}</h1><p>{body}</p></main></html>"
    );
    let hsts = if https {
        "Strict-Transport-Security: max-age=31536000\r\n"
    } else {
        ""
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'\r\n{hsts}Connection: close\r\n\r\n{html}",
        html.len()
    );
}

pub(super) fn exchange(
    client: &Client,
    endpoints: &Endpoints,
    client_id: &str,
    client_secret: &str,
    redirect: &str,
    verifier: &str,
    code: &str,
) -> Result<Credentials> {
    let response = client
        .post(&endpoints.token)
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .map_err(transport)?;
    let success = response.status().is_success();
    let value = response_json(response)?;
    if !success {
        return oauth_error(&value);
    }
    let mut credentials = Credentials {
        id: uuid::Uuid::new_v4().to_string(),
        client_id: client_id.into(),
        client_secret: client_secret.into(),
        refresh_token: required(&value, "refresh_token")?,
        access_token: String::new(),
        expires_at: 0,
        scope: required(&value, "scope")?,
    };
    apply_tokens(&mut credentials, &value)?;
    Ok(credentials)
}

pub(super) fn refresh(
    client: &Client,
    endpoints: &Endpoints,
    store: &CredentialStore,
    credentials: &mut Credentials,
    force: bool,
) -> Result<()> {
    if !force && credentials.expires_at > now() + 60 {
        return Ok(());
    }
    let response = client
        .post(&endpoints.token)
        .form(&[
            ("client_id", credentials.client_id.as_str()),
            ("client_secret", &credentials.client_secret),
            ("refresh_token", &credentials.refresh_token),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .map_err(transport)?;
    let success = response.status().is_success();
    let value = response_json(response)?;
    if !success {
        return oauth_error(&value);
    }
    let mut updated = credentials.clone();
    apply_tokens(&mut updated, &value)?;
    store.save(&updated)?;
    *credentials = updated;
    Ok(())
}

fn apply_tokens(credentials: &mut Credentials, value: &Value) -> Result<()> {
    if let Some(scope) = value["scope"].as_str() {
        credentials.scope = scope.to_owned();
    }
    exact_scope(&credentials.scope)?;
    credentials.access_token = required(value, "access_token")?;
    ensure!(
        value["token_type"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("Bearer")),
        "invalid input: expected OAuth Bearer token"
    );
    let seconds = value["expires_in"]
        .as_u64()
        .filter(|s| *s > 0 && *s <= 86400)
        .context("invalid input: missing or invalid OAuth token expiry")?;
    credentials.expires_at = now() + seconds;
    if let Some(token) = value["refresh_token"].as_str().filter(|s| !s.is_empty()) {
        credentials.refresh_token = token.into();
    }
    Ok(())
}

fn required(value: &Value, key: &str) -> Result<String> {
    value[key].as_str().filter(|s| !s.is_empty()).map(str::to_owned)
        .ok_or_else(|| super::YoutubeError { code: "youtube.protocol_error", message: "OAuth token response is incomplete; authorize offline upload access again" }.into())
}

fn oauth_error<T>(value: &Value) -> Result<T> {
    if value["error"].as_str() == Some("invalid_grant") {
        failure(
            "youtube.auth_expired",
            "Google rejected the refresh token; set the OAuth app to In production and run youtube auth login again",
        )
    } else {
        failure(
            "youtube.auth_failed",
            "Google rejected authorization; check the Desktop client configuration and consent screen",
        )
    }
}

pub fn revoke(store: &CredentialStore) -> Result<Value> {
    revoke_with(store, &http_client()?, &Endpoints::default())
}

pub(super) fn revoke_with(
    store: &CredentialStore,
    client: &Client,
    endpoints: &Endpoints,
) -> Result<Value> {
    let _lock = store.lock()?;
    let Some(credentials) = store.load_optional()? else {
        return Ok(json!({"revoked":true,"alreadyAbsent":true}));
    };
    let response = client
        .post(&endpoints.revoke)
        .form(&[("token", &credentials.refresh_token)])
        .send()
        .map_err(transport)?;
    if !response.status().is_success() {
        let value = response_json(response)?;
        if value["error"].as_str() != Some("invalid_token") {
            return failure(
                "youtube.revoke_failed",
                "Google did not confirm revocation; local credentials were retained for retry",
            );
        }
    }
    store.delete()?;
    Ok(json!({"revoked":true,"alreadyAbsent":false}))
}
