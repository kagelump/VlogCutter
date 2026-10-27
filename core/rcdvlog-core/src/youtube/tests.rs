use super::credentials::Credentials;
use super::*;
use crate::Catalog;
use anyhow::Result;
use reqwest::blocking::Client;
use serde_json::json;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tempfile::{TempDir, tempdir};

type Check = Box<dyn FnOnce(Request) + Send>;
struct Step {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
    check: Check,
    disconnect: bool,
}
struct Request {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn step(status: u16, body: Value, check: impl FnOnce(Request) + Send + 'static) -> Step {
    Step {
        status,
        headers: vec![],
        body: body.to_string(),
        check: Box::new(check),
        disconnect: false,
    }
}
fn begin() -> Step {
    let mut value = step(200, json!({}), |request| {
        assert_eq!(request.method, "POST");
        assert!(request.target.contains("uploadType=resumable"));
        assert_eq!(request.headers["authorization"], "Bearer fixture-access");
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["status"]["privacyStatus"], "private");
    });
    value.headers.push((
        "Location".into(),
        "$ORIGIN/upload/youtube/v3/videos?upload_id=fixture-session-secret".into(),
    ));
    value
}
fn probe(offset: u64, total: u64) -> Step {
    let mut value = step(308, json!({}), move |request| {
        assert_eq!(request.method, "PUT");
        assert_eq!(request.headers["content-range"], format!("bytes */{total}"));
        assert!(request.body.is_empty());
    });
    if offset > 0 {
        value
            .headers
            .push(("Range".into(), format!("bytes=0-{}", offset - 1)));
    }
    value
}
fn done() -> Value {
    json!({"id":"fixture_video","snippet":{"channelId":"fixture-channel"},"status":{"privacyStatus":"private"}})
}
fn mock(steps: Vec<Step>) -> (Endpoints, Client, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let endpoints = Endpoints {
        authorize: format!("{origin}/authorize"),
        token: format!("{origin}/token"),
        revoke: format!("{origin}/revoke"),
        upload: format!("{origin}/upload/youtube/v3/videos"),
    };
    let server = thread::spawn(move || {
        for step in steps {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => panic!("fixture did not receive expected request: {error}"),
                }
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let request = read_request(&mut socket);
            (step.check)(request);
            if step.disconnect {
                continue;
            }
            let body = step.body.replace("$ORIGIN", &origin);
            write!(
                socket,
                "HTTP/1.1 {} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n",
                step.status,
                body.len()
            )
            .unwrap();
            for (name, value) in step.headers {
                write!(socket, "{name}: {}\r\n", value.replace("$ORIGIN", &origin)).unwrap();
            }
            write!(socket, "\r\n{body}").unwrap();
        }
    });
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    (endpoints, client, server)
}
fn read_request(socket: &mut TcpStream) -> Request {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        socket.read_exact(&mut byte).unwrap();
        head.push(byte[0]);
        assert!(head.len() < 16384);
    }
    let head = String::from_utf8(head).unwrap();
    let mut lines = head.split("\r\n");
    let first: Vec<_> = lines.next().unwrap().split_whitespace().collect();
    let headers: BTreeMap<_, _> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let length = headers
        .get("content-length")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    socket.read_exact(&mut body).unwrap();
    Request {
        method: first[0].into(),
        target: first[1].into(),
        headers,
        body,
    }
}
fn credential() -> Credentials {
    Credentials {
        id: "fixture-identity".into(),
        client_id: "fixture-client".into(),
        client_secret: "fixture-client-secret".into(),
        refresh_token: "fixture-refresh-secret".into(),
        access_token: "fixture-access".into(),
        expires_at: now() + 3600,
        scope: UPLOAD_SCOPE.into(),
    }
}
fn store(directory: &TempDir) -> CredentialStore {
    let store =
        CredentialStore::new(Some(&directory.path().join("private/credentials.json"))).unwrap();
    let _lock = store.lock().unwrap();
    store.save(&credential()).unwrap();
    store
}
fn fixture(size: usize) -> (TempDir, Catalog, CredentialStore, UploadOptions) {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Upload fixture").unwrap();
    let store = store(&directory);
    let file = directory.path().join("render with spaces 日本.mp4");
    std::fs::write(&file, vec![b'x'; size]).unwrap();
    let options = UploadOptions {
        project_id: project.id,
        file,
        title: "Rough cut 2".into(),
        description: String::new(),
        privacy: "private".into(),
        cut_revision: Some("editrev_fixture".into()),
        restart_upload: false,
    };
    (directory, catalog, store, options)
}
fn code(error: &anyhow::Error) -> &str {
    error.downcast_ref::<YoutubeError>().unwrap().code
}

#[test]
fn authorization_requests_only_upload_with_offline_consent_and_rfc_pkce() {
    let url = oauth::authorization_url(
        &Endpoints::default(),
        "client",
        "http://127.0.0.1:1234/oauth/callback",
        "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        "state",
    )
    .unwrap();
    let url = reqwest::Url::parse(&url).unwrap();
    let pairs: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(pairs["scope"], UPLOAD_SCOPE);
    assert_eq!(
        pairs["code_challenge"],
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    assert_eq!(pairs["code_challenge_method"], "S256");
    assert_eq!(pairs["access_type"], "offline");
    assert_eq!(pairs["include_granted_scopes"], "false");
    assert!(pairs["prompt"].contains("consent"));
}

#[test]
fn callback_ignores_wrong_state_duplicates_and_origin_then_accepts_valid_code() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let receiver = thread::spawn(move || {
        oauth::receive_code(&listener, "right", Duration::from_secs(5)).unwrap()
    });
    for (query, extra, expected) in [
        ("state=wrong&code=bad", "", 400),
        ("state=right&state=right&code=bad", "", 400),
        (
            "state=right&code=bad",
            "Origin: https://attacker.invalid\r\n",
            400,
        ),
        ("state=right&code=good", "", 200),
    ] {
        let mut socket = TcpStream::connect(address).unwrap();
        write!(
            socket,
            "GET /oauth/callback?{query} HTTP/1.1\r\nHost: {address}\r\n{extra}\r\n"
        )
        .unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).unwrap();
        assert!(response.starts_with(&format!("HTTP/1.1 {expected}")));
        assert!(!response.contains("code=good"));
    }
    assert_eq!(receiver.join().unwrap(), "good");
}

#[test]
fn callback_denial_and_timeout_are_actionable() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let receiver = thread::spawn(move || {
        oauth::receive_code(&listener, "right", Duration::from_secs(5)).unwrap_err()
    });
    let mut socket = TcpStream::connect(address).unwrap();
    write!(
        socket,
        "GET /oauth/callback?state=right&error=access_denied HTTP/1.1\r\nHost: {address}\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).unwrap();
    assert_eq!(code(&receiver.join().unwrap()), "youtube.auth_denied");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    assert_eq!(
        code(&oauth::receive_code(&listener, "state", Duration::from_millis(10)).unwrap_err()),
        "youtube.auth_timeout"
    );
}

#[test]
fn exchange_enforces_exact_scope_and_receives_pkce_in_form_body() {
    let (endpoints, client, server) = mock(vec![step(
        200,
        json!({"access_token":"fixture-access","refresh_token":"fixture-refresh-secret","expires_in":3600,"token_type":"Bearer","scope":UPLOAD_SCOPE}),
        |request| {
            assert_eq!(request.target, "/token");
            let form: BTreeMap<_, _> = reqwest::Url::parse(&format!(
                "http://localhost/?{}",
                String::from_utf8(request.body).unwrap()
            ))
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect();
            assert_eq!(form["code_verifier"], "verifier");
            assert_eq!(form["code"], "code");
            assert_eq!(form["grant_type"], "authorization_code");
        },
    )]);
    let result = oauth::exchange(
        &client, &endpoints, "client", "secret", "redirect", "verifier", "code",
    )
    .unwrap();
    assert_eq!(result.scope, UPLOAD_SCOPE);
    assert!(!result.refresh_token.is_empty());
    server.join().unwrap();
    for scope in [
        "https://www.googleapis.com/auth/youtube",
        "",
        &format!("{UPLOAD_SCOPE} openid"),
    ] {
        assert_eq!(
            code(&exact_scope(scope).unwrap_err()),
            "youtube.scope_mismatch"
        );
    }
}

#[test]
fn refresh_preserves_refresh_token_and_rejects_scope_expansion_without_overwriting() {
    let directory = tempdir().unwrap();
    let store = store(&directory);
    let mut credentials = credential();
    credentials.expires_at = 0;
    let (endpoints, client, server) = mock(vec![
        step(
            200,
            json!({"access_token":"new-access","expires_in":3600,"token_type":"Bearer"}),
            |request| {
                assert!(!request.target.contains("fixture-refresh-secret"));
                assert!(
                    String::from_utf8(request.body)
                        .unwrap()
                        .contains("grant_type=refresh_token")
                );
            },
        ),
        step(
            200,
            json!({"access_token":"overbroad-secret","expires_in":3600,"token_type":"Bearer","scope":format!("{UPLOAD_SCOPE} openid")}),
            |_| {},
        ),
    ]);
    oauth::refresh(&client, &endpoints, &store, &mut credentials, false).unwrap();
    assert_eq!(credentials.refresh_token, "fixture-refresh-secret");
    assert_eq!(store.load().unwrap().access_token, "new-access");
    let error = oauth::refresh(&client, &endpoints, &store, &mut credentials, true).unwrap_err();
    assert_eq!(code(&error), "youtube.scope_mismatch");
    assert!(!error.to_string().contains("overbroad-secret"));
    assert_eq!(store.load().unwrap().access_token, "new-access");
    server.join().unwrap();
}

#[test]
fn invalid_grant_requires_reauthorization_and_revoke_retains_credentials_on_failure() {
    let directory = tempdir().unwrap();
    let store = store(&directory);
    let (endpoints, client, server) = mock(vec![
        step(
            400,
            json!({"error":"invalid_grant","error_description":"fixture-refresh-secret"}),
            |_| {},
        ),
        step(
            500,
            json!({"error":"temporary","message":"fixture-refresh-secret"}),
            |_| {},
        ),
        step(200, json!({}), |request| {
            assert_eq!(request.target, "/revoke");
            assert!(
                String::from_utf8(request.body)
                    .unwrap()
                    .contains("token=fixture-refresh-secret")
            );
        }),
    ]);
    let error = oauth::refresh(&client, &endpoints, &store, &mut credential(), true).unwrap_err();
    assert_eq!(code(&error), "youtube.auth_expired");
    assert!(!error.to_string().contains("fixture-refresh-secret"));
    assert_eq!(
        code(&oauth::revoke_with(&store, &client, &endpoints).unwrap_err()),
        "youtube.revoke_failed"
    );
    assert!(store.status().unwrap().authorized);
    oauth::revoke_with(&store, &client, &endpoints).unwrap();
    assert!(!store.status().unwrap().authorized);
    server.join().unwrap();
}

#[test]
fn credential_status_is_observational_and_locks_prevent_competing_writers() {
    let directory = tempdir().unwrap();
    let empty = CredentialStore::new(Some(&directory.path().join("absent/credential"))).unwrap();
    assert!(!empty.status().unwrap().authorized);
    assert!(!directory.path().join("absent").exists());
    let store = store(&directory);
    let _first = store.lock().unwrap();
    assert_eq!(code(&store.lock().unwrap_err()), "youtube.credential_busy");
    let status = serde_json::to_string(&store.status().unwrap()).unwrap();
    assert!(!status.contains("fixture-refresh-secret"));
    assert!(!status.contains("fixture-access"));
}

#[cfg(unix)]
#[test]
fn credential_files_are_private_and_symlinks_or_insecure_permissions_are_rejected() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let directory = tempdir().unwrap();
    let store = store(&directory);
    let path = directory.path().join("private/credentials.json");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.status().is_err());
    let link = directory.path().join("link.json");
    symlink(path, &link).unwrap();
    assert!(CredentialStore::new(Some(&link)).unwrap().status().is_err());
}

#[test]
fn interrupted_multichunk_upload_resumes_after_reopening_catalog_and_deduplicates() {
    let chunk = 8 * 1024 * 1024;
    let (directory, catalog, store, options) = fixture(chunk + 17);
    let mut accepted = step(308, json!({}), move |request| {
        assert_eq!(request.body.len(), chunk);
        assert_eq!(
            request.headers["content-range"],
            format!("bytes 0-{}/{}", chunk - 1, chunk + 17)
        );
    });
    accepted
        .headers
        .push(("Range".into(), format!("bytes=0-{}", chunk - 1)));
    let (endpoints, client, server) = mock(vec![
        begin(),
        probe(0, (chunk + 17) as u64),
        accepted,
        step(403, json!({}), |request| {
            assert_eq!(request.body.len(), 17);
        }),
        probe(chunk as u64, (chunk + 17) as u64),
        step(201, done(), move |request| {
            assert_eq!(request.body, vec![b'x'; 17]);
            assert_eq!(
                request.headers["content-range"],
                format!("bytes {chunk}-{}/{}", chunk + 16, chunk + 17)
            );
        }),
    ]);
    let error = catalog
        .youtube_upload_with(&store, &options, &client, &endpoints, |_| {})
        .unwrap_err();
    assert_eq!(code(&error), "youtube.upload_forbidden");
    drop(catalog);
    let catalog = Catalog::open_existing(directory.path().join("catalog.sqlite3")).unwrap();
    let history = catalog
        .youtube_upload_history(&options.project_id, 20)
        .unwrap();
    assert_eq!(history[0].uploaded_bytes, chunk as u64);
    assert!(
        !serde_json::to_string(&history)
            .unwrap()
            .contains("fixture-session-secret")
    );
    assert!(!format!("{:?}", history[0]).contains("fixture-session-secret"));
    let result = catalog
        .youtube_upload_with(&store, &options, &client, &endpoints, |_| {})
        .unwrap();
    assert_eq!(result.url, "https://youtu.be/fixture_video");
    assert!(!result.reused);
    assert!(!result.processing_verified);
    server.join().unwrap();
    let result = catalog
        .youtube_upload_with(&store, &options, &client, &endpoints, |_| {})
        .unwrap();
    assert!(result.reused);
    let mut changed = options.clone();
    changed.privacy = "public".into();
    assert_eq!(
        code(
            &catalog
                .youtube_upload_with(&store, &changed, &client, &endpoints, |_| {})
                .unwrap_err()
        ),
        "youtube.metadata_conflict"
    );
    assert_eq!(
        catalog
            .youtube_upload_history(&options.project_id, 20)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn lost_final_response_recovers_video_id_without_reuploading() {
    let (_directory, catalog, store, options) = fixture(100);
    let mut lost = step(201, done(), |request| {
        assert_eq!(request.body.len(), 100);
    });
    lost.disconnect = true;
    let (endpoints, client, server) = mock(vec![
        begin(),
        probe(0, 100),
        lost,
        step(201, done(), |request| {
            assert_eq!(request.headers["content-range"], "bytes */100");
            assert!(request.body.is_empty());
        }),
    ]);
    let result = catalog
        .youtube_upload_with(&store, &options, &client, &endpoints, |_| {})
        .unwrap();
    assert_eq!(result.upload.state, "complete");
    server.join().unwrap();
}

#[test]
fn uncertain_initiation_never_automatically_creates_a_second_upload() {
    let (_directory, catalog, store, options) = fixture(100);
    let mut lost = begin();
    lost.disconnect = true;
    let (endpoints, client, server) = mock(vec![lost]);
    let error = catalog
        .youtube_upload_with(&store, &options, &client, &endpoints, |_| {})
        .unwrap_err();
    assert_eq!(code(&error), "youtube.network_error");
    server.join().unwrap();
    assert_eq!(
        code(
            &catalog
                .youtube_upload_with(&store, &options, &client, &endpoints, |_| {})
                .unwrap_err()
        ),
        "youtube.outcome_unknown"
    );
    assert_eq!(
        catalog
            .youtube_upload_history(&options.project_id, 20)
            .unwrap()[0]
            .state,
        "initiating"
    );
}

#[test]
fn expired_session_needs_explicit_restart_and_then_finishes_in_same_invocation() {
    let (_directory, catalog, store, mut options) = fixture(100);
    let (endpoints, client, server) = mock(vec![
        begin(),
        step(404, json!({}), |_| {}),
        step(404, json!({}), |_| {}),
        begin(),
        probe(0, 100),
        step(201, done(), |_| {}),
    ]);
    assert_eq!(
        code(
            &catalog
                .youtube_upload_with(&store, &options, &client, &endpoints, |_| {})
                .unwrap_err()
        ),
        "youtube.session_expired"
    );
    options.restart_upload = true;
    assert_eq!(
        catalog
            .youtube_upload_with(&store, &options, &client, &endpoints, |_| {})
            .unwrap()
            .upload
            .state,
        "complete"
    );
    server.join().unwrap();
}

#[test]
fn malicious_session_redirect_cannot_receive_credentials_and_errors_stay_redacted() {
    let (_directory, catalog, store, options) = fixture(100);
    let mut start = begin();
    start.headers[0].1 = "https://attacker.invalid/upload?token=fixture-session-secret".into();
    let (endpoints, client, server) = mock(vec![start]);
    let error = catalog
        .youtube_upload_with(&store, &options, &client, &endpoints, |_| {})
        .unwrap_err();
    assert_eq!(code(&error), "youtube.protocol_error");
    assert!(!error.to_string().contains("fixture-session-secret"));
    server.join().unwrap();
    for uri in [
        "https://www.googleapis.com.evil.invalid/upload/youtube/v3/videos",
        "https://user:pass@www.googleapis.com/upload/youtube/v3/videos",
        "https://www.googleapis.com/elsewhere",
    ] {
        assert!(upload::validate_session(uri, &Endpoints::default()).is_err());
    }
}

#[test]
fn invalid_artifacts_and_projects_never_start_network_requests() -> Result<()> {
    let (_directory, catalog, store, mut options) = fixture(100);
    let (endpoints, client, server) = mock(vec![]);
    server.join().unwrap();
    options.title = String::new();
    assert!(
        catalog
            .youtube_upload_with(&store, &options, &client, &endpoints, |_| {})
            .is_err()
    );
    options.title = "Valid".into();
    options.project_id = "missing".into();
    assert!(
        catalog
            .youtube_upload_with(&store, &options, &client, &endpoints, |_| {})
            .unwrap_err()
            .to_string()
            .contains("project not found")
    );
    assert!(!catalog.connection()?.query_row(
        "SELECT EXISTS(SELECT 1 FROM youtube_uploads)",
        [],
        |row| row.get::<_, bool>(0)
    )?);
    Ok(())
}

#[test]
fn upload_refreshes_after_401_and_probes_before_resending_bytes() {
    let (_directory, catalog, store, options) = fixture(100);
    let refreshed_probe = step(308, json!({}), |request| {
        assert_eq!(request.headers["authorization"], "Bearer rotated-access");
        assert_eq!(request.headers["content-range"], "bytes */100");
    });
    let (endpoints, client, server) = mock(vec![
        begin(),
        probe(0, 100),
        step(401, json!({}), |_| {}),
        step(
            200,
            json!({"access_token":"rotated-access","expires_in":3600,"token_type":"Bearer"}),
            |request| {
                assert_eq!(request.target, "/token");
            },
        ),
        refreshed_probe,
        step(201, done(), |request| {
            assert_eq!(request.headers["authorization"], "Bearer rotated-access");
            assert_eq!(request.body.len(), 100);
        }),
    ]);
    catalog
        .youtube_upload_with(&store, &options, &client, &endpoints, |_| {})
        .unwrap();
    assert_eq!(
        store.load().unwrap().refresh_token,
        "fixture-refresh-secret"
    );
    server.join().unwrap();
}

#[test]
fn migration_49_upgrades_v48_and_unversioned_replay_preserves_upload_history() {
    let (directory, catalog, store, options) = fixture(100);
    // Simulate a genuine pre-feature schema, retaining existing project operations.
    catalog
        .connection()
        .unwrap()
        .execute_batch(
            "DROP TABLE youtube_uploads; DELETE FROM schema_migrations WHERE migration_id=49;",
        )
        .unwrap();
    let readonly =
        Catalog::open_read_only_existing(directory.path().join("catalog.sqlite3")).unwrap();
    assert_eq!(
        code(
            &readonly
                .youtube_upload_history(&options.project_id, 20)
                .unwrap_err()
        ),
        "schema_incompatible"
    );
    let catalog = Catalog::open_existing(directory.path().join("catalog.sqlite3")).unwrap();
    assert_eq!(catalog.list_projects().unwrap()[0].id, options.project_id);
    let (endpoints, client, server) = mock(vec![begin(), probe(0, 100), step(201, done(), |_| {})]);
    let original = catalog
        .youtube_upload_with(&store, &options, &client, &endpoints, |_| {})
        .unwrap();
    server.join().unwrap();
    catalog
        .connection()
        .unwrap()
        .execute("DELETE FROM schema_migrations", [])
        .unwrap();
    let reopened = Catalog::open_existing(directory.path().join("catalog.sqlite3")).unwrap();
    let record = &reopened
        .youtube_upload_history(&options.project_id, 20)
        .unwrap()[0];
    assert_eq!(record.id, original.upload.id);
    assert_eq!(record.video_id.as_deref(), Some("fixture_video"));
}

#[test]
fn exchange_rejects_missing_refresh_token_and_broader_grants() {
    let (endpoints, client, server) = mock(vec![
        step(
            200,
            json!({"access_token":"fixture-access","expires_in":3600,"token_type":"Bearer","scope":UPLOAD_SCOPE}),
            |_| {},
        ),
        step(
            200,
            json!({"access_token":"fixture-access","refresh_token":"fixture-refresh-secret","expires_in":3600,"token_type":"Bearer","scope":format!("{UPLOAD_SCOPE} openid")}),
            |_| {},
        ),
    ]);
    let first = oauth::exchange(
        &client, &endpoints, "client", "secret", "redirect", "verifier", "code",
    )
    .err()
    .unwrap();
    assert_eq!(code(&first), "youtube.protocol_error");
    let second = oauth::exchange(
        &client, &endpoints, "client", "secret", "redirect", "verifier", "code",
    )
    .err()
    .unwrap();
    assert_eq!(code(&second), "youtube.scope_mismatch");
    server.join().unwrap();
}
