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
    /// Zero selects an ephemeral port. Set a port explicitly for SSH forwarding.
    pub port: u16,
    pub timeout_seconds: u64,
    pub open_browser: bool,
}

pub fn login(
    store: &CredentialStore,
    options: &LoginOptions,
    mut progress: impl FnMut(Value),
) -> Result<super::AuthStatus> {
    ensure!(
        (1..=3600).contains(&options.timeout_seconds),
        "invalid input: OAuth timeout must be 1..3600 seconds"
    );
    let _lock = store.lock()?;
    let mut bytes = Vec::new();
    let file = File::open(&options.client_file).context("open OAuth desktop client file")?;
    ensure!(
        file.metadata()?.is_file(),
        "invalid input: OAuth client must be a regular file"
    );
    file.take(65537)
        .read_to_end(&mut bytes)
        .context("read OAuth client file")?;
    ensure!(
        bytes.len() <= 65536,
        "invalid input: OAuth client file exceeds size limit"
    );
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| super::YoutubeError {
        code: "youtube.invalid_client",
        message: "Invalid OAuth client JSON; download a Desktop app client from Google Cloud",
    })?;
    let installed = value.get("installed").context(
        "invalid input: a Google Desktop app OAuth client is required (installed object missing)",
    )?;
    let client_id = installed["client_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("invalid input: OAuth client ID missing")?;
    let client_secret = installed["client_secret"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("invalid input: OAuth desktop client secret missing")?;
    // Never use endpoint URLs from downloaded JSON; requests only go to Google.
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, options.port))
        .context("bind OAuth loopback listener; select a free --port and matching SSH forward")?;
    let port = listener.local_addr()?.port();
    let redirect = format!("http://127.0.0.1:{port}/oauth/callback");
    let verifier = random_secret();
    let state = random_secret();
    let endpoints = Endpoints::default();
    let authorization = authorization_url(&endpoints, client_id, &redirect, &verifier, &state)?;
    progress(json!({
        "event":"youtube.oauth.authorization_required", "authorizationUrl":authorization,
        "redirectUri":redirect, "port":port, "expiresInSeconds":options.timeout_seconds,
        "scope":UPLOAD_SCOPE,
        "sshForward":format!("ssh -N -L {port}:127.0.0.1:{port} <upload-host>"),
    }));
    if options.open_browser {
        open_browser(&authorization);
    }
    let code = receive_code(
        &listener,
        &state,
        Duration::from_secs(options.timeout_seconds),
    )?;
    let credentials = exchange(
        &http_client()?,
        &endpoints,
        client_id,
        client_secret,
        &redirect,
        &verifier,
        &code,
    )?;
    store.save(&credentials)?;
    store.status()
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

pub(super) fn receive_code(
    listener: &TcpListener,
    state: &str,
    timeout: Duration,
) -> Result<String> {
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
                match callback(&mut stream, listener.local_addr()?.port(), state) {
                    Ok(Some(code)) => {
                        reply(
                            &mut stream,
                            "200 OK",
                            "Authorization received. Return to VlogCutter to confirm it completed.",
                        );
                        return Ok(code);
                    }
                    Ok(None) => reply(
                        &mut stream,
                        "400 Bad Request",
                        "Invalid authorization callback.",
                    ),
                    Err(error) if error.downcast_ref::<super::YoutubeError>().is_some() => {
                        reply(
                            &mut stream,
                            "400 Bad Request",
                            "Authorization was declined. Return to VlogCutter.",
                        );
                        return Err(error);
                    }
                    Err(_) => reply(
                        &mut stream,
                        "400 Bad Request",
                        "Invalid authorization callback.",
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

fn callback(stream: &mut TcpStream, port: u16, expected_state: &str) -> Result<Option<String>> {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    while bytes.len() < 8192 {
        if stream.read(&mut byte)? == 0 {
            return Ok(None);
        }
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    if !bytes.ends_with(b"\r\n\r\n") {
        return Ok(None);
    }
    let request = std::str::from_utf8(&bytes)?;
    let mut lines = request.split("\r\n");
    let first: Vec<_> = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    if first.len() != 3 || first[0] != "GET" || !first[1].starts_with("/oauth/callback?") {
        return Ok(None);
    }
    let mut hosts = Vec::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            return Ok(None);
        };
        if name.eq_ignore_ascii_case("origin") {
            return Ok(None);
        }
        if name.eq_ignore_ascii_case("host") {
            hosts.push(value.trim());
        }
    }
    if hosts != [format!("127.0.0.1:{port}")] {
        return Ok(None);
    }
    let url = Url::parse(&format!("http://127.0.0.1:{port}{}", first[1]))?;
    let pairs: Vec<_> = url.query_pairs().collect();
    let states: Vec<_> = pairs
        .iter()
        .filter(|(k, _)| k == "state")
        .map(|(_, v)| v.as_ref())
        .collect();
    // Fixed-length hashes prevent content-dependent comparison of the state value.
    if states.len() != 1
        || Sha256::digest(states[0].as_bytes()) != Sha256::digest(expected_state.as_bytes())
    {
        return Ok(None);
    }
    if pairs.iter().any(|(k, _)| k == "error") {
        return failure(
            "youtube.auth_denied",
            "Google authorization was declined; no credential was saved",
        );
    }
    let codes: Vec<_> = pairs
        .iter()
        .filter(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string())
        .collect();
    Ok(if codes.len() == 1 && !codes[0].is_empty() {
        Some(codes[0].clone())
    } else {
        None
    })
}

fn reply(stream: &mut TcpStream, status: &str, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
        body.len()
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
