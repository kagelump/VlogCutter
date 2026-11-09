//! Router and handler tests (moved out of lib.rs).

use super::*;
use axum::body::to_bytes;
use rcdvlog_server_core::{BaselineEnvelope, Catalog, ProjectAssetAddedPayload, ServerConfig};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use tempfile::TempDir;
use tower::ServiceExt;

struct Fixture {
    _root: TempDir,
    foundation: Arc<RuntimeFoundation>,
}

impl Fixture {
    fn new(missing_tool: bool) -> Self {
        let root = TempDir::new().unwrap();
        let data = root.path().join("state");
        let derived = root.path().join("derived");
        let backup = root.path().join("backup");
        let originals = root.path().join("originals");
        let attached = root.path().join("projects");
        std::fs::create_dir_all(&attached).unwrap();
        std::fs::create_dir_all(&derived).unwrap();
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::create_dir_all(&originals).unwrap();
        let tool = if missing_tool {
            "rcdvlog-tool-that-does-not-exist"
        } else {
            "/usr/bin/true"
        };
        let config = ServerConfig::from_values(BTreeMap::from([
            ("RCDVLOG_DATA_DIR".to_string(), data.display().to_string()),
            (
                "RCDVLOG_DERIVED_DIR".to_string(),
                derived.display().to_string(),
            ),
            (
                "RCDVLOG_BACKUP_DIR".to_string(),
                backup.display().to_string(),
            ),
            (
                "RCDVLOG_ALLOWED_ORIGINAL_MOUNTS".to_string(),
                originals.display().to_string(),
            ),
            (
                "RCDVLOG_ATTACHED_PROJECT_ROOTS".to_string(),
                attached.display().to_string(),
            ),
            ("RCDVLOG_FFMPEG".to_string(), tool.to_string()),
            ("RCDVLOG_FFPROBE".to_string(), tool.to_string()),
            ("RCDVLOG_EXIFTOOL".to_string(), tool.to_string()),
            // Floor the disk-capacity gate (default 5 GiB) so tests that
            // exercise representation work never depend on how much free
            // space the host happens to have. GH ubuntu runners can drop
            // under the default and turn those endpoints into 507s.
            (
                "RCDVLOG_MINIMUM_FREE_BYTES".to_string(),
                (64 * 1024 * 1024).to_string(),
            ),
        ]))
        .unwrap();
        let foundation = Arc::new(RuntimeFoundation::initialize(config).unwrap());
        Self {
            _root: root,
            foundation,
        }
    }

    fn app(&self) -> Router {
        router(HttpState::new(self.foundation.clone()))
    }

    fn app_with_auth_rate_limit(&self, maximum_attempts: usize) -> Router {
        router(
            HttpState::new(self.foundation.clone())
                .with_auth_rate_limit(maximum_attempts, Duration::from_secs(60)),
        )
    }

    fn seed_media_content(&self, bytes: &[u8]) -> RepresentationId {
        let id = RepresentationId::parse("representation_http_fixture").unwrap();
        let relative = "representations/http/fixture/representation_http_fixture.mp4";
        let path = self.foundation.config().derived_dir.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        let now = "2026-07-23T00:00:00Z";
        let connection = rcdvlog_server_core::open_writable_connection(
            self.foundation.catalog().database_path(),
        )
        .unwrap();
        connection.execute(
            "INSERT INTO media_objects(media_object_id,size_bytes,full_content_hash,verification_state,created_at,updated_at) VALUES(?1,?2,?3,'verified',?4,?4)",
            rusqlite::params!["media_http_fixture", bytes.len() as i64, "a".repeat(64), now],
        ).unwrap();
        connection.execute(
            "INSERT INTO representations(representation_id,media_object_id,source_content_hash,source_size_bytes,kind,recipe_key,settings_identity,state,output_content_hash,byte_length,content_type,created_at,completed_at,updated_at) VALUES(?1,'media_http_fixture',?2,?3,'proxy','proxy.h264-720p.orientation-v3',?4,'available',?5,?3,'video/mp4',?6,?6,?6)",
            rusqlite::params![id.as_str(), "a".repeat(64), bytes.len() as i64, "b".repeat(64), blake3::hash(bytes).to_hex().to_string(), now],
        ).unwrap();
        connection.execute(
            "INSERT INTO representation_locations(representation_id,node_id,location_kind,location_key,relative_path,state,verified_at,updated_at) VALUES(?1,?2,'shared_derived',?3,?3,'available',?4,?4)",
            rusqlite::params![id.as_str(), self.foundation.catalog().node_id().as_str(), relative, now],
        ).unwrap();
        id
    }
}

async fn json(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn device_auth_token(fixture: &Fixture) -> String {
    let bootstrap = fixture.foundation.auth().create_bootstrap_code().unwrap();
    let session = fixture
        .foundation
        .auth()
        .exchange_bootstrap(bootstrap.code.expose(), "rcdvlog.local")
        .unwrap();
    let pairing = fixture
        .foundation
        .auth()
        .create_pairing_code(&session.session_id)
        .unwrap();
    let device = fixture
        .foundation
        .auth()
        .exchange_pairing(pairing.code.expose(), "Test Mac")
        .unwrap();
    device.token.expose().to_string()
}

#[tokio::test]
async fn callback_routes_authenticate_claim_and_acknowledge_terminal_events() {
    use rcdvlog_server_core::{JobClass, JobDefinition, JobRequirements, RetryPolicy};
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);
    let job = JobId::parse("job_callback_http").unwrap();
    fixture
        .foundation
        .jobs()
        .enqueue(JobDefinition {
            job_id: job.clone(),
            job_type: "test.v1".into(),
            payload_version: 1,
            payload: serde_json::json!({}),
            immutable_identity: serde_json::json!({}),
            requirements: JobRequirements {
                schema_version: 1,
                ..Default::default()
            },
            target_node_id: fixture.foundation.catalog().node_id().clone(),
            project_id: None,
            media_object_id: None,
            storage_root_id: None,
            representation_id: None,
            recipe_key: "test.v1".into(),
            job_class: JobClass::Lightweight,
            idempotency_key: "callback-http".into(),
            priority: 0,
            total_work_units: 1,
            total_items: 1,
            retry_policy: RetryPolicy::default(),
            dependencies: vec![],
        })
        .unwrap();
    let request = |method: &str, path: &str, body: Value, authenticated: bool| {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "rcdvlog.local")
            .header(header::CONTENT_TYPE, "application/json");
        if authenticated {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        builder
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    };
    let path = "/api/v1/admin/jobs/job_callback_http/callbacks";
    let denied = fixture
        .app()
        .oneshot(request(
            "POST",
            path,
            serde_json::json!({"target":"editor"}),
            false,
        ))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let response = fixture
        .app()
        .oneshot(request(
            "POST",
            path,
            serde_json::json!({"target":"editor","command":"must-not-run"}),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let registered = fixture
        .app()
        .oneshot(request(
            "POST",
            path,
            serde_json::json!({"target":"editor"}),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(registered.status(), StatusCode::OK);
    fixture
        .foundation
        .jobs()
        .request_cancel(&job, None)
        .unwrap();
    let claimed = fixture
        .app()
        .oneshot(request(
            "POST",
            "/api/v1/admin/job-callbacks/claim",
            serde_json::json!({"target":"editor","leaseSeconds":30}),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(claimed.status(), StatusCode::OK);
    let claimed = json(claimed).await;
    assert_eq!(claimed["claim"]["delivery"]["event"]["state"], "canceled");
    let id = claimed["claim"]["delivery"]["deliveryId"].as_str().unwrap();
    let lease = claimed["claim"]["leaseToken"].as_str().unwrap();
    let ack = format!("/api/v1/admin/job-callbacks/{id}/settle");
    let rejected = fixture
        .app()
        .oneshot(request(
            "POST",
            &ack,
            serde_json::json!({"leaseToken":"stale","succeeded":true}),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::CONFLICT);
    let response = fixture
        .app()
        .oneshot(request(
            "POST",
            &ack,
            serde_json::json!({"leaseToken":lease,"succeeded":true}),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json(response).await["state"], "delivered");
    let listed = fixture
        .app()
        .oneshot(request(
            "GET",
            "/api/v1/admin/job-callbacks?target=editor&limit=1",
            Value::Null,
            true,
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = json(listed).await;
    assert_eq!(listed["items"][0]["state"], "delivered");
    assert!(!listed.to_string().contains(lease));
    let claimed = fixture
        .app()
        .oneshot(request(
            "POST",
            "/api/v1/admin/job-callbacks/claim",
            serde_json::json!({"target":"editor","leaseSeconds":30}),
            true,
        ))
        .await
        .unwrap();
    assert!(json(claimed).await["claim"].is_null());
}

fn browser_auth(fixture: &Fixture) -> (String, String, String) {
    let bootstrap = fixture.foundation.auth().create_bootstrap_code().unwrap();
    let session = fixture
        .foundation
        .auth()
        .exchange_bootstrap(bootstrap.code.expose(), "rcdvlog.local")
        .unwrap();
    (
        session.session_id,
        session.session_token.expose().to_string(),
        session.csrf_token,
    )
}

fn baseline_envelope(root: &TempDir) -> BaselineEnvelope {
    let (catalog, project_id) = cli_catalog(&root.path().join("desktop.sqlite3"));
    BaselineEnvelope::export(&catalog, &project_id, vec![], vec![]).unwrap()
}

/// A CLI project catalog at `path` with one asset, `asset_http`.
fn cli_catalog(path: &std::path::Path) -> (Catalog, String) {
    let catalog = Catalog::open(path).unwrap();
    catalog.set_local_node_id("node_mac").unwrap();
    let project = catalog.create_project("HTTP Trip").unwrap();
    catalog
        .add_portable_project_asset(
            &project.id,
            ProjectAssetAddedPayload {
                id: "asset_http".to_string(),
                fingerprint: "fingerprint-http".to_string(),
                filename: "http.mov".to_string(),
                size_bytes: 10,
                duration_seconds: 1.0,
                frame_rate: 30.0,
                width: 1920,
                height: 1080,
                codec: "h264".to_string(),
                camera: "camera".to_string(),
                captured_at_raw: None,
                timestamp_candidates: None,
                best_time: None,
                metadata_rotation_degrees: 0,
                default_rotation_degrees: 0,
                created_at: "2026-07-23T00:00:00Z".to_string(),
            },
        )
        .unwrap();
    (catalog, project.id)
}

#[tokio::test]
async fn attached_projects_serve_the_cli_catalog_and_its_thumbnails_only_from_allowed_roots() {
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);
    let root = fixture._root.path().to_path_buf();
    let work = root.join("projects/work");
    std::fs::create_dir_all(work.join("cache/thumbnails/asset_http")).unwrap();
    let catalog_path = work.join("catalog.sqlite3");
    let (catalog, project_id) = cli_catalog(&catalog_path);
    let thumbnail = work.join("cache/thumbnails/asset_http/q1.v2.jpg");
    std::fs::write(&thumbnail, b"\xff\xd8jpeg-bytes").unwrap();
    let record_thumbnail = |path: &std::path::Path| {
        let connection = rusqlite::Connection::open(&catalog_path).unwrap();
        connection
            .execute(
                "DELETE FROM asset_thumbnails WHERE asset_id='asset_http'",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO asset_thumbnails(asset_id, quality, path, created_at) VALUES ('asset_http', 1, ?1, '2026-11-08T00:00:00Z')",
                [path.display().to_string()],
            )
            .unwrap();
    };
    record_thumbnail(&thumbnail);
    rusqlite::Connection::open(&catalog_path)
        .unwrap()
        .execute(
            "UPDATE assets SET description='Penguins on blue ice' WHERE id='asset_http'",
            [],
        )
        .unwrap();
    fixture
        .foundation
        .catalog()
        .project_replicas()
        .attach_catalog(
            &catalog_path,
            &fixture.foundation.config().attached_project_roots,
            None,
        )
        .unwrap();

    let request = |method: Method, uri: String, etag: Option<&str>| {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"));
        if let Some(etag) = etag {
            builder = builder.header(header::IF_NONE_MATCH, etag);
        }
        builder.body(Body::empty()).unwrap()
    };
    let thumbnail_uri = format!("/api/v1/projects/{project_id}/assets/asset_http/thumbnail");

    let anonymous = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri(&thumbnail_uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let wrong_project = fixture
        .app()
        .oneshot(request(
            Method::GET,
            "/api/v1/projects/not_this_project/assets/asset_http/thumbnail".into(),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(wrong_project.status(), StatusCode::NOT_FOUND);

    let served = fixture
        .app()
        .oneshot(request(Method::GET, thumbnail_uri.clone(), None))
        .await
        .unwrap();
    assert_eq!(served.status(), StatusCode::OK);
    assert_eq!(served.headers()[header::CONTENT_TYPE], "image/jpeg");
    let etag = served.headers()[header::ETAG].to_str().unwrap().to_string();
    assert_eq!(
        to_bytes(served.into_body(), 64).await.unwrap(),
        &b"\xff\xd8jpeg-bytes"[..]
    );
    let unchanged = fixture
        .app()
        .oneshot(request(Method::GET, thumbnail_uri.clone(), Some(&etag)))
        .await
        .unwrap();
    assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
    let head = fixture
        .app()
        .oneshot(request(Method::HEAD, thumbnail_uri.clone(), None))
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::OK);
    assert!(to_bytes(head.into_body(), 64).await.unwrap().is_empty());

    // Recorded paths outside the allowed roots, into the server's own data, or
    // not JPEG are never read, however the catalog records them.
    let outside = root.join("outside.jpg");
    std::fs::write(&outside, b"secret").unwrap();
    let escape = work.join("cache/thumbnails/asset_http/escape.jpg");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &escape).unwrap();
    let control = fixture.foundation.config().data_dir.join("control.sqlite3");
    let not_jpeg = work.join("cache/thumbnails/asset_http/q1.png");
    std::fs::write(&not_jpeg, b"png").unwrap();
    for refused in [
        outside.as_path(),
        escape.as_path(),
        control.as_path(),
        not_jpeg.as_path(),
    ] {
        record_thumbnail(refused);
        let response = fixture
            .app()
            .oneshot(request(Method::GET, thumbnail_uri.clone(), None))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{}",
            refused.display()
        );
    }

    let assets = fixture
        .app()
        .oneshot(request(
            Method::GET,
            format!("/api/v1/projects/{project_id}/assets"),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(assets.status(), StatusCode::OK);
    assert_eq!(
        json(assets).await["items"][0]["description"],
        "Penguins on blue ice"
    );

    // Sync never writes into an attached catalog.
    let envelope = BaselineEnvelope::export(&catalog, &project_id, vec![], vec![]).unwrap();
    let import = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/internal/projects/baseline/import")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(serde_json::to_vec(&envelope).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(import.status(), StatusCode::CONFLICT);
    assert_eq!(json(import).await["code"], "project_attached");
    let reconcile = fixture.app().oneshot(
        Request::builder().method(Method::POST)
            .uri(format!("/api/v1/internal/projects/{project_id}/reconcile"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from(serde_json::to_vec(&serde_json::json!({
                "protocolVersion": {"minimumMajor": 1, "minimumMinor": 0, "maximumMajor": 1, "maximumMinor": 0},
                "projectSchemaVersion": rcdvlog_server_core::PROJECT_CATALOG_SCHEMA_VERSION,
                "projectId": project_id, "serverNodeId": fixture.foundation.catalog().node_id().as_str(),
                "operationCursors": {}, "operations": [], "observations": [], "representationManifests": [], "maximumResponseOperations": 10
            })).unwrap())).unwrap()).await.unwrap();
    assert_eq!(reconcile.status(), StatusCode::CONFLICT);
    assert_eq!(json(reconcile).await["code"], "project_attached");
}

#[tokio::test]
async fn baseline_preview_import_and_reconcile_routes_preserve_protocol_errors() {
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);
    let source = TempDir::new().unwrap();
    let envelope = baseline_envelope(&source);
    let body = serde_json::to_vec(&envelope).unwrap();

    let preview = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/internal/projects/baseline/preview")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(preview.status(), StatusCode::OK);
    let preview = json(preview).await;
    assert_eq!(preview["preview"]["assetCount"], 1);
    assert_eq!(
        preview["preview"]["projectId"],
        envelope.payload.project.project_id
    );

    let imported = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/internal/projects/baseline/import")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(imported.status(), StatusCode::CREATED);
    let imported = json(imported).await;
    assert_eq!(
        imported["replica"]["baselineDigest"],
        envelope.digest.as_str()
    );

    let bad_reconcile = serde_json::json!({
        "protocolVersion": {
            "minimumMajor": 99,
            "minimumMinor": 0,
            "maximumMajor": 99,
            "maximumMinor": 0
        },
        "projectSchemaVersion": rcdvlog_server_core::PROJECT_CATALOG_SCHEMA_VERSION,
        "projectId": envelope.payload.project.project_id,
        "serverNodeId": fixture.foundation.catalog().node_id().as_str(),
        "operationCursors": {},
        "operations": [],
        "observations": [],
        "representationManifests": [],
        "maximumResponseOperations": 10
    });
    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!(
                    "/api/v1/internal/projects/{}/reconcile",
                    envelope.payload.project.project_id
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::from(serde_json::to_vec(&bad_reconcile).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json(response).await["code"], "sync_protocol_incompatible");
}

#[tokio::test]
async fn media_content_supports_head_and_single_ranges_without_exposing_paths() {
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);
    let bytes = b"0123456789abcdef";
    let id = fixture.seed_media_content(bytes);
    let uri = format!("/api/v1/media/{}/content", id.as_str());

    let partial = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri(&uri)
                .header(header::RANGE, "bytes=3-7")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(partial.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(partial.headers()[header::CONTENT_RANGE], "bytes 3-7/16");
    assert_eq!(partial.headers()[header::ACCEPT_RANGES], "bytes");
    assert_eq!(
        to_bytes(partial.into_body(), 32).await.unwrap(),
        &bytes[3..=7]
    );

    let head = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri(&uri)
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(head.headers()[header::CONTENT_LENGTH], "16");
    assert!(to_bytes(head.into_body(), 32).await.unwrap().is_empty());

    let invalid = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri(&uri)
                .header(header::RANGE, "bytes=0-1,4-5")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(invalid.headers()[header::CONTENT_RANGE], "bytes */16");
}

#[test]
fn range_parser_handles_open_suffix_and_unsatisfiable_ranges() {
    assert_eq!(
        parse_range(Some(&HeaderValue::from_static("bytes=4-")), 10),
        Ok(Some((4, 9)))
    );
    assert_eq!(
        parse_range(Some(&HeaderValue::from_static("bytes=-3")), 10),
        Ok(Some((7, 9)))
    );
    assert!(parse_range(Some(&HeaderValue::from_static("bytes=10-")), 10).is_err());
    assert!(parse_range(Some(&HeaderValue::from_static("items=0-1")), 10).is_err());
}

#[tokio::test]
async fn health_is_read_only_and_preserves_valid_incoming_request_id() {
    let fixture = Fixture::new(false);
    let database_modified = std::fs::metadata(fixture.foundation.catalog().database_path())
        .unwrap()
        .modified()
        .unwrap();
    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/health")
                .header(REQUEST_ID_HEADER, "caller.safe-123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[REQUEST_ID_HEADER], "caller.safe-123");
    let body = json(response).await;
    assert_eq!(body["status"], "live");
    assert_eq!(body["requestId"], "caller.safe-123");
    assert_eq!(
        std::fs::metadata(fixture.foundation.catalog().database_path())
            .unwrap()
            .modified()
            .unwrap(),
        database_modified
    );
}

#[tokio::test]
async fn invalid_request_id_is_replaced_and_readiness_uses_503() {
    let fixture = Fixture::new(true);
    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/ready")
                .header(REQUEST_ID_HEADER, "unsafe request id")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let response_id = response.headers()[REQUEST_ID_HEADER]
        .to_str()
        .unwrap()
        .to_string();
    assert!(response_id.starts_with("request_"));
    let body = json(response).await;
    assert_eq!(body["status"], "not_ready");
    assert_eq!(body["requestId"], response_id);
}

#[tokio::test]
async fn version_reports_all_compatibility_versions_and_node_identity() {
    let fixture = Fixture::new(false);
    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/version")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["applicationVersion"], APPLICATION_VERSION);
    assert_eq!(body["protocolVersion"]["major"], PROTOCOL_MAJOR_VERSION);
    assert_eq!(body["protocolVersion"]["minor"], PROTOCOL_MINOR_VERSION);
    assert_eq!(body["controlSchemaVersion"], CONTROL_CATALOG_SCHEMA_VERSION);
    assert_eq!(
        body["nodeId"],
        fixture.foundation.catalog().node_id().as_str()
    );
}

#[tokio::test]
async fn serves_built_web_assets_with_spa_and_cache_policy() {
    let fixture = Fixture::new(false);
    let foundation_root = fixture
        .app()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(foundation_root.status(), StatusCode::OK);
    assert_eq!(json(foundation_root).await["status"], "private_foundation");

    let web_root = fixture._root.path().join("web");
    std::fs::create_dir_all(web_root.join("assets")).unwrap();
    std::fs::write(web_root.join("index.html"), b"<main>web shell</main>").unwrap();
    std::fs::write(web_root.join("assets/app-abc123.js"), b"export {};").unwrap();
    let app = router(HttpState::new(fixture.foundation.clone()).with_web_root(web_root));

    let index = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/projects")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(index.status(), StatusCode::OK);
    assert_eq!(index.headers()[header::CACHE_CONTROL], "no-cache");
    // The IFrame API loads external scripts and creates a YouTube frame.
    // Keep these permissions explicit while retaining the same-origin API
    // boundary and the ban on embedding our app inside another site.
    assert_eq!(
        index.headers()["content-security-policy"],
        "default-src 'self'; base-uri 'self'; frame-ancestors 'none'; object-src 'none'; connect-src 'self'; script-src 'self' https://www.youtube.com https://s.ytimg.com; frame-src https://www.youtube.com"
    );
    assert_eq!(
        index.headers()["referrer-policy"],
        "strict-origin-when-cross-origin"
    );
    assert_eq!(index.headers()["x-frame-options"], "DENY");
    assert_eq!(
        index.headers()["permissions-policy"],
        "camera=(), microphone=(self), geolocation=()"
    );
    assert_eq!(
        to_bytes(index.into_body(), 1024).await.unwrap(),
        "<main>web shell</main>"
    );

    let asset = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/assets/app-abc123.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(asset.status(), StatusCode::OK);
    assert_eq!(
        asset.headers()[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );

    let missing_api = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/not-real")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_api.status(), StatusCode::NOT_FOUND);
    assert_eq!(json(missing_api).await["code"], "not_found");
}

#[tokio::test]
async fn exposes_the_rust_owned_openapi_contract() {
    let fixture = Fixture::new(false);
    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["openapi"], "3.1.0");
    assert!(body["paths"]["/api/v1/auth/bootstrap/exchange"].is_object());
    assert!(body["paths"]["/api/v1/admin/backups"].is_object());
    assert!(body["paths"]["/api/v1/admin/backups/{backupId}"].is_object());
}

#[tokio::test]
async fn device_cli_auth_errors_preserve_the_caller_request_id() {
    let fixture = Fixture::new(false);
    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/overview")
                .header(REQUEST_ID_HEADER, "req_cli_fixture")
                .header(header::AUTHORIZATION, "Bearer invalid-device-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()[REQUEST_ID_HEADER], "req_cli_fixture");
    let value = json(response).await;
    assert_eq!(value["code"], "session_invalid");
    assert_eq!(value["requestId"], "req_cli_fixture");
}

#[tokio::test]
async fn bootstrap_creation_is_local_only_and_open_exchanges_are_bounded() {
    let fixture = Fixture::new(false);
    let remote_creation = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/auth/bootstrap")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(remote_creation.status(), StatusCode::NOT_FOUND);

    let app = fixture.app_with_auth_rate_limit(2);
    for _ in 0..2 {
        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/auth/bootstrap/exchange")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"code":"invalid"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
    }
    let limited = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/auth/bootstrap/exchange")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"code":"invalid"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json(limited).await["code"], "rate_limited");

    let oversized = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/auth/bootstrap/exchange")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(vec![b'x'; MAX_AUTH_BODY_BYTES + 1]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn host_origin_forwarded_headers_and_websocket_origin_are_enforced() {
    let fixture = Fixture::new(false);
    let invalid_host = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/health")
                .header(header::HOST, "attacker.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid_host.status(), StatusCode::FORBIDDEN);
    assert_eq!(json(invalid_host).await["code"], "invalid_host");

    let foreign_origin = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/auth/bootstrap/exchange")
                .header(header::HOST, "rcdvlog.local")
                .header(header::ORIGIN, "https://foreign.example")
                .header("x-forwarded-proto", "https")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"code":"invalid"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(foreign_origin.status(), StatusCode::FORBIDDEN);
    assert_eq!(json(foreign_origin).await["code"], "origin_mismatch");

    let forwarded_mismatch = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/health")
                .header(header::HOST, "rcdvlog.local")
                .header("x-forwarded-host", "server.tailnet.ts.net")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forwarded_mismatch.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        json(forwarded_mismatch).await["code"],
        "invalid_forwarded_headers"
    );

    let token = device_auth_token(&fixture);
    let websocket = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/ws/events")
                .header(header::HOST, "rcdvlog.local")
                .header(header::ORIGIN, "https://foreign.example")
                .header("x-forwarded-proto", "https")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONNECTION, "upgrade")
                .header(header::UPGRADE, "websocket")
                .header("sec-websocket-version", "13")
                .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(websocket.status(), StatusCode::FORBIDDEN);
    assert_eq!(json(websocket).await["code"], "origin_mismatch");
}

#[tokio::test]
async fn device_credentials_can_create_pairing_codes_and_queue_backups() {
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);
    let pairing = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/auth/pairing")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(pairing.status(), StatusCode::OK);
    let pairing = json(pairing).await;
    assert!(
        pairing["code"]
            .as_str()
            .is_some_and(|code| !code.is_empty())
    );

    let backup = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/admin/backups")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"idempotencyKey":"cli-backup-fixture"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(backup.status(), StatusCode::ACCEPTED);
    let backup = json(backup).await;
    assert_eq!(backup["definition"]["jobType"], "backup.create.v1");

    let listed = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/backups")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    assert!(json(listed).await["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_stops_the_listener_within_the_bound() {
    let fixture = Fixture::new(false);
    let listener = match bind_loopback("127.0.0.1:0".parse().unwrap()).await {
        Ok(listener) => listener,
        Err(error)
            if error.chain().any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
            }) =>
        {
            // Some hermetic test sandboxes forbid every socket bind. The same test runs
            // normally on development hosts and Linux CI where loopback is available.
            return;
        }
        Err(error) => panic!("bind loopback test listener: {error:#}"),
    };
    let shutdown = ShutdownCoordinator::new();
    let cancel = shutdown.clone();
    let server = tokio::spawn(serve_until_shutdown(
        listener,
        fixture.app(),
        shutdown,
        Duration::from_secs(1),
    ));
    cancel.cancel();
    timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn readiness_timeout_is_bounded_and_repeated_requests_are_single_flight() {
    let fixture = Fixture::new(false);
    let evaluations = Arc::new(AtomicUsize::new(0));
    let evaluator_count = evaluations.clone();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let evaluator_release = release.clone();
    let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
    let started_sender = Arc::new(Mutex::new(Some(started_sender)));
    let evaluator_started_sender = started_sender.clone();
    let auth = Arc::new(fixture.foundation.auth().clone());
    let state = HttpState::with_readiness_evaluator(
        fixture.foundation.clone(),
        auth,
        Duration::from_millis(20),
        move || {
            evaluator_count.fetch_add(1, Ordering::SeqCst);
            if let Some(sender) = evaluator_started_sender.lock().unwrap().take() {
                let _ = sender.send(());
            }
            let (lock, condition) = &*evaluator_release;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = condition.wait(released).unwrap();
            }
            ReadinessReport::from_checks(Vec::new())
        },
    );
    let app = router(state);

    let first_request = tokio::spawn(
        app.clone().oneshot(
            Request::builder()
                .uri("/api/v1/ready")
                .body(Body::empty())
                .unwrap(),
        ),
    );
    timeout(Duration::from_secs(1), started_receiver)
        .await
        .unwrap()
        .unwrap();
    let response_wait_started = Instant::now();
    let timed_out = first_request.await.unwrap().unwrap();
    assert_eq!(timed_out.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(response_wait_started.elapsed() < Duration::from_secs(1));
    let body = json(timed_out).await;
    assert_eq!(body["checks"][0]["code"], "readiness_check_timeout");

    let health = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);

    let busy = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = json(busy).await;
    assert_eq!(body["checks"][0]["code"], "readiness_check_busy");

    let (lock, condition) = &*release;
    *lock.lock().unwrap() = true;
    condition.notify_all();
    assert_eq!(evaluations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn library_media_requires_authentication() {
    let fixture = Fixture::new(false);
    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/library/media?limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn library_media_rejects_unsupported_query_combinations() {
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);

    for uri in [
        "/api/v1/library/media?limit=500",
        "/api/v1/library/media?limit=10&sort=filenameAsc&storageRootId=root_a",
        "/api/v1/library/media?limit=10&nonsense=1",
    ] {
        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "uri: {uri}");
        let body = json(response).await;
        assert_eq!(body["code"], "invalid_query", "uri: {uri}");
    }
}

#[tokio::test]
async fn a_cursor_cannot_be_replayed_against_another_sort() {
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);
    let cursor = rcdvlog_server_core::library_query::encode_cursor(
        rcdvlog_server_core::library_query::LibrarySort::CreatedDesc,
        &Default::default(),
        &rcdvlog_server_core::library_query::CursorKey::Created {
            created_at: "2026-07-20T00:00:00Z".to_string(),
            media_object_id: "media_a".to_string(),
        },
    );
    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/v1/library/media?limit=10&sort=filenameAsc&after={cursor}"
                ))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json(response).await["code"], "invalid_query");
}

#[tokio::test]
async fn project_asset_responses_contain_no_absolute_paths() {
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);
    let source = TempDir::new().unwrap();
    let envelope = baseline_envelope(&source);
    let project_id = envelope.payload.project.project_id.clone();
    let body = serde_json::to_vec(&envelope).unwrap();
    let imported = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/internal/projects/baseline/import")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(imported.status(), StatusCode::CREATED);

    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/projects/{project_id}/assets?limit=10"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let text = json(response).await.to_string();
    assert!(!text.contains(source.path().to_str().unwrap()));
    assert!(!text.contains("\"path\""));
    assert!(!text.contains("proxyPath"));
}

#[tokio::test]
async fn add_to_project_route_is_bounded_idempotent_and_replica_authoritative() {
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);
    let source = TempDir::new().unwrap();
    let envelope = baseline_envelope(&source);
    let project_id = envelope.payload.project.project_id.clone();
    fixture
        .foundation
        .catalog()
        .project_replicas()
        .import_baseline(&envelope)
        .unwrap();
    fixture
        .foundation
        .catalog()
        .storage_library()
        .create_media_object(rcdvlog_server_core::NewMediaObject {
            id: MediaObjectId::parse("media_http_add").unwrap(),
            size_bytes: Some(10),
            quick_fingerprint: Some(blake3::hash(b"http-add").to_hex().to_string()),
            full_content_hash: Some("a".repeat(64)),
            verification_state: GlobalMediaVerificationState::Verified,
        })
        .unwrap();
    let body = serde_json::to_vec(&serde_json::json!({
        "mediaObjectIds": ["media_http_add"],
        "acknowledgeUnverified": false,
        "idempotencyKey": "http-add-1"
    }))
    .unwrap();

    for expected in ["added", "already_member"] {
        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/api/v1/projects/{project_id}/assets/add"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json(response).await["items"][0]["disposition"], expected);
    }
    assert_eq!(
        fixture
            .foundation
            .catalog()
            .project_replicas()
            .open_catalog(&project_id)
            .unwrap()
            .count_project_assets(&project_id)
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn library_media_detail_requires_authentication() {
    let fixture = Fixture::new(false);
    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/library/media/media_missing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn library_media_detail_reports_not_found_for_unknown_media() {
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);
    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/library/media/media_missing")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(json(response).await["code"], "not_found");
}

#[tokio::test]
async fn library_media_detail_rejects_unknown_query_parameters() {
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);
    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/library/media/media_missing?nonsense=1")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json(response).await["code"], "invalid_query");
}

#[tokio::test]
async fn projects_requires_authentication() {
    let fixture = Fixture::new(false);
    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/projects")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn projects_lists_registered_project_replicas() {
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);
    let source = TempDir::new().unwrap();
    let envelope = baseline_envelope(&source);
    let project_id = envelope.payload.project.project_id.clone();
    let body = serde_json::to_vec(&envelope).unwrap();
    let imported = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/internal/projects/baseline/import")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(imported.status(), StatusCode::CREATED);

    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/projects")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["items"][0]["projectId"], project_id);
    assert!(body["nextCursor"].is_null());
}

#[tokio::test]
async fn projects_rejects_unknown_query_parameters() {
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);
    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/projects?limit=10&nonsense=1")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json(response).await["code"], "invalid_query");
}

#[tokio::test]
async fn project_assets_requires_authentication() {
    let fixture = Fixture::new(false);
    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/projects/project_missing/assets")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn project_assets_rejects_unknown_query_parameters_and_missing_project() {
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);

    let unknown_param = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/projects/project_missing/assets?limit=10&nonsense=1")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unknown_param.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json(unknown_param).await["code"], "invalid_query");

    let missing_project = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/projects/project_missing/assets?limit=10")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_project.status(), StatusCode::NOT_FOUND);
    assert_eq!(json(missing_project).await["code"], "not_found");
}

#[tokio::test]
async fn review_routes_return_canonical_state_and_commit_stale_note_revisions() {
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);
    let source = TempDir::new().unwrap();
    let envelope = baseline_envelope(&source);
    let project_id = envelope.payload.project.project_id.clone();
    fixture
        .foundation
        .catalog()
        .project_replicas()
        .import_baseline(&envelope)
        .unwrap();
    let path = format!("/api/v1/projects/{project_id}/assets/asset_http");

    let review = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri(format!("{path}/review"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(review.status(), StatusCode::OK);
    let initial = json(review).await;
    let stale_base = initial["materializedRevision"]
        .as_u64()
        .unwrap()
        .saturating_sub(1);

    let note = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("{path}/notes"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "text": "Browser revision",
                        "baseRevision": stale_base
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(note.status(), StatusCode::OK);
    let note = json(note).await;
    assert_eq!(note["asset"]["activeNote"], "Browser revision");
    assert_eq!(note["staleBaseRevision"], true);
    assert!(note["asset"]["noteRevisionCount"].as_u64().unwrap() >= 2);

    let label = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("{path}/labels/add"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "label": "keeper",
                        "baseRevision": note["asset"]["materializedRevision"]
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(label.status(), StatusCode::OK);
    assert_eq!(json(label).await["asset"]["labels"][0], "keeper");
}

#[tokio::test]
async fn review_routes_require_authentication_and_validate_rotation() {
    let fixture = Fixture::new(false);
    let unauthenticated = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/projects/project_missing/assets/asset_missing/review")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let token = device_auth_token(&fixture);
    let source = TempDir::new().unwrap();
    let envelope = baseline_envelope(&source);
    let project_id = envelope.payload.project.project_id.clone();
    fixture
        .foundation
        .catalog()
        .project_replicas()
        .import_baseline(&envelope)
        .unwrap();
    let invalid = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!(
                    "/api/v1/projects/{project_id}/assets/asset_http/rotation"
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(r#"{"rotationDegrees":45,"baseRevision":0}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json(invalid).await["code"], "invalid_input");
}

#[tokio::test]
async fn storage_admin_requires_allowed_paths_evidence_confirmation_and_audits_mutations() {
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);
    let create = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/admin/storage-roots")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(
                    r#"{"displayName":"Archive","immutableOriginal":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);
    let root = json(create).await;
    let root_id = root["id"].as_str().unwrap();
    std::fs::write(
        fixture._root.path().join("originals/sample.mov"),
        b"0123456789",
    )
    .unwrap();
    fixture
        .foundation
        .catalog()
        .storage_library()
        .create_storage_object(rcdvlog_server_core::NewStorageObject {
            id: rcdvlog_server_core::StorageObjectId::parse("storage_admin_sample").unwrap(),
            storage_root_id: StorageRootId::parse(root_id).unwrap(),
            relative_path: RootRelativePath::parse("sample.mov").unwrap(),
            size_bytes: Some(10),
            modified_at: None,
        })
        .unwrap();

    let rejected_write = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri(format!("/api/v1/admin/storage-roots/{root_id}/binding"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "localMountPath": fixture._root.path().join("originals"),
                        "protocol": "local", "accessMode": "read_write"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected_write.status(), StatusCode::BAD_REQUEST);

    let disallowed = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri(format!("/api/v1/admin/storage-roots/{root_id}/binding"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "localMountPath": fixture.foundation.config().derived_dir,
                        "protocol": "local", "accessMode": "read_only"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(disallowed.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json(disallowed).await["code"], "mount_path_not_allowed");

    let binding = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri(format!("/api/v1/admin/storage-roots/{root_id}/binding"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "localMountPath": fixture._root.path().join("originals"),
                        "protocol": "local", "accessMode": "read_only"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(binding.status(), StatusCode::OK);
    assert_eq!(json(binding).await["confirmation"]["status"], "pending");

    let verified = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!(
                    "/api/v1/admin/storage-roots/{root_id}/binding/verify"
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(verified.status(), StatusCode::OK);
    let verified = json(verified).await;
    assert_eq!(verified["associationEvidence"]["matchingRelativePaths"], 1);
    assert_eq!(verified["associationEvidence"]["mismatches"], 0);

    let confirmed = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!(
                    "/api/v1/admin/storage-roots/{root_id}/binding/confirmation"
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(r#"{"action":"confirm"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(confirmed.status(), StatusCode::OK);
    assert_eq!(json(confirmed).await["confirmation"]["status"], "confirmed");

    let audit = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/audit?limit=10")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let audit = json(audit).await;
    assert!(audit["items"].as_array().unwrap().len() >= 4);
    assert!(!audit.to_string().to_ascii_lowercase().contains("token"));
}

#[tokio::test]
async fn representation_batch_skips_unverified_sources_instead_of_enqueuing() {
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);
    fixture
        .foundation
        .catalog()
        .storage_library()
        .create_media_object(rcdvlog_server_core::NewMediaObject {
            id: MediaObjectId::parse("media_unverified_admin").unwrap(),
            size_bytes: Some(100),
            quick_fingerprint: Some("a".repeat(64)),
            full_content_hash: None,
            verification_state: GlobalMediaVerificationState::Unverified,
        })
        .unwrap();
    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/admin/representations/batch")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(r#"{"mediaObjectIds":["media_unverified_admin"],"kind":"proxy","idempotencyKey":"batch-1"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = json(response).await;
    assert_eq!(body["queuedCount"], 0);
    assert_eq!(body["skipped"][0]["reason"], "source_not_verified");
}

#[tokio::test]
async fn revoking_current_browser_session_commits_audit_and_expires_cookie() {
    let fixture = Fixture::new(false);
    let (session_id, session_token, csrf_token) = browser_auth(&fixture);
    let cookie = format!("rcdvlog_session={session_id}:{session_token}");
    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/api/v1/auth/sessions/{session_id}"))
                .header(header::COOKIE, &cookie)
                .header("x-csrf-token", csrf_token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );

    let denied = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/auth/sessions")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let events = fixture
        .foundation
        .catalog()
        .audit_log()
        .list(10, None)
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| { event.action == "session.revoked" && event.actor_id == session_id })
    );
}

fn review_target_handoff(project_id: &str, youtube: Option<&str>) -> Value {
    let mut handoff: rcdvlog_server_core::ReviewTargetHandoff = serde_json::from_str(include_str!(
        "../../../core/rcdvlog-core/tests/fixtures/review-target/v1.json"
    ))
    .unwrap();
    handoff.target.project_id = project_id.to_string();
    handoff.target.youtube_video_id = youtube.map(str::to_string);
    handoff.target_digest =
        rcdvlog_server_core::ReviewTargetHandoff::snapshot_digest(&handoff.target).unwrap();
    handoff.verification.verified_at =
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    serde_json::to_value(handoff).unwrap()
}

#[tokio::test]
async fn review_target_routes_are_authenticated_csrf_guarded_and_never_overwrite() {
    let fixture = Fixture::new(false);
    let source = TempDir::new().unwrap();
    let envelope = baseline_envelope(&source);
    let project_id = envelope.payload.project.project_id.clone();
    fixture
        .foundation
        .catalog()
        .project_replicas()
        .import_baseline(&envelope)
        .unwrap();
    let (session_id, session_token, csrf) = browser_auth(&fixture);
    let cookie = format!("rcdvlog_session={session_id}:{session_token}");
    let base = format!("/api/v1/projects/{project_id}/review-targets");
    let post = |uri: String, body: Value, csrf: Option<&str>| {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, &cookie);
        if let Some(csrf) = csrf {
            builder = builder.header("x-csrf-token", csrf);
        }
        builder
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    };
    let request = serde_json::json!({
        "handoff": review_target_handoff(&project_id, None),
        "declaredIssuer": "Synthetic laptop"
    });

    let anonymous = fixture
        .app()
        .oneshot(Request::builder().uri(&base).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let no_csrf = fixture
        .app()
        .oneshot(post(format!("{base}/import"), request.clone(), None))
        .await
        .unwrap();
    assert_eq!(no_csrf.status(), StatusCode::FORBIDDEN);

    let preflight = fixture
        .app()
        .oneshot(post(
            format!("{base}/preflight"),
            request.clone(),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(preflight.status(), StatusCode::OK);
    let preflight = json(preflight).await;
    assert_eq!(preflight["outcome"], "new");
    assert_eq!(preflight["importable"], true);

    let imported = fixture
        .app()
        .oneshot(post(format!("{base}/import"), request.clone(), Some(&csrf)))
        .await
        .unwrap();
    assert_eq!(imported.status(), StatusCode::OK);
    let imported = json(imported).await;
    assert_eq!(imported["outcome"], "created");
    let target = &imported["reviewTarget"];
    let id = target["reviewTargetId"].as_str().unwrap().to_string();
    assert_eq!(target["revisionId"], "editrev_synthetic");
    assert_eq!(target["youtube"]["identityStatus"], "absent");
    assert_eq!(target["mapping"]["status"], "measuredComplete");
    assert_eq!(target["readiness"]["ready"], true);
    assert_eq!(target["provenance"]["importer"]["actorId"], session_id);
    assert!(!serde_json::to_string(target).unwrap().contains("/Volumes"));

    let again = fixture
        .app()
        .oneshot(post(format!("{base}/import"), request.clone(), Some(&csrf)))
        .await
        .unwrap();
    assert_eq!(json(again).await["outcome"], "reused");

    let patch_name = |body: Value, csrf_header: Option<&str>| {
        let mut builder = Request::builder()
            .method(Method::PATCH)
            .uri(format!("{base}/{id}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, &cookie);
        if let Some(csrf) = csrf_header {
            builder = builder.header("x-csrf-token", csrf);
        }
        builder
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    };
    let denied = fixture
        .app()
        .oneshot(patch_name(serde_json::json!({"displayName":"RC7"}), None))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    for invalid in [
        serde_json::json!({}),
        serde_json::json!({"displayName":42}),
        serde_json::json!({"displayName":""}),
        serde_json::json!({"displayName":"RC7\n"}),
    ] {
        let response = fixture
            .app()
            .oneshot(patch_name(invalid, Some(&csrf)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let renamed = fixture
        .app()
        .oneshot(patch_name(
            serde_json::json!({"displayName":" RC7 "}),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(renamed.status(), StatusCode::OK);
    let renamed = json(renamed).await;
    assert_eq!(renamed["displayName"], "RC7");
    assert_eq!(renamed["reviewTargetId"], target["reviewTargetId"]);
    assert_eq!(renamed["targetDigest"], target["targetDigest"]);
    assert_eq!(renamed["observations"], target["observations"]);
    let cleared = fixture
        .app()
        .oneshot(patch_name(
            serde_json::json!({"displayName":null}),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(cleared.status(), StatusCode::OK);
    assert_eq!(json(cleared).await["displayName"], Value::Null);

    let mut conflicting = request.clone();
    conflicting["handoff"]["target"]["movie"]["blake3"] = Value::String("d".repeat(64));
    let mut typed: rcdvlog_server_core::ReviewTargetSnapshot =
        serde_json::from_value(conflicting["handoff"]["target"].clone()).unwrap();
    typed.movie.blake3 = "d".repeat(64);
    conflicting["handoff"]["targetDigest"] =
        Value::String(rcdvlog_server_core::ReviewTargetHandoff::snapshot_digest(&typed).unwrap());
    let conflict = fixture
        .app()
        .oneshot(post(format!("{base}/import"), conflicting, Some(&csrf)))
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(json(conflict).await["code"], "review_target_conflict");

    let mut tampered = request.clone();
    tampered["handoff"]["target"]["cuts"][0]["sourceInSeconds"] = serde_json::json!(1.25);
    let tampered = fixture
        .app()
        .oneshot(post(format!("{base}/import"), tampered, Some(&csrf)))
        .await
        .unwrap();
    assert_eq!(tampered.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json(tampered).await["code"],
        "review_target_digest_mismatch"
    );

    let rebound = fixture
        .app()
        .oneshot(post(
            format!("{base}/{id}/youtube"),
            serde_json::json!({"youtubeVideoId": "synthetic01"}),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(rebound.status(), StatusCode::OK);
    let rebound = json(rebound).await;
    assert_eq!(
        rebound["reviewTarget"]["youtube"]["identityStatus"],
        "declaredUnverified"
    );
    assert_eq!(
        rebound["reviewTarget"]["provenance"]["derivedFromReviewTargetId"],
        id
    );

    let candidates = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "{base}/{id}/candidates?startSeconds=1.5&endSeconds=2.5"
                ))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(candidates.status(), StatusCode::OK);
    assert_eq!(
        json(candidates).await["operationIds"],
        serde_json::json!(["editop_split_a", "editop_split_b"])
    );

    let listed = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri(format!("{base}?limit=10"))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(json(listed).await["items"].as_array().unwrap().len(), 2);

    let missing = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/projects/project_absent/review-targets")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(json(missing).await["code"], "project_replica_not_found");

    let events = fixture
        .foundation
        .catalog()
        .audit_log()
        .list(20, None)
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.action == "review_target.imported")
            .count(),
        1
    );
    assert!(
        events
            .iter()
            .any(|e| e.action == "review_target.youtube_declared")
    );
}

fn historical_review(project_id: &str) -> Value {
    let handoff = review_target_handoff(project_id, None);
    serde_json::json!({
        "schemaVersion": 1,
        "purpose": "historicalRenderedArtifactReview",
        "targetDigest": handoff["targetDigest"],
        "target": handoff["target"],
        "verifiedAt": handoff["verification"]["verifiedAt"],
        "outputMatches": true,
        "mappingMatchesRevision": true,
        "storedFullyDecoded": true,
        "currentSourceVerification": "notEstablishedByThisReview",
        "currentConstraintsVerification": "notEstablishedByThisReview",
        "currentPlanRenderability": "notEstablishedByThisReview"
    })
}

#[tokio::test]
async fn historical_review_target_routes_need_acknowledgement_and_keep_their_contract() {
    let fixture = Fixture::new(false);
    let source = TempDir::new().unwrap();
    let envelope = baseline_envelope(&source);
    let project_id = envelope.payload.project.project_id.clone();
    fixture
        .foundation
        .catalog()
        .project_replicas()
        .import_baseline(&envelope)
        .unwrap();
    let (session_id, session_token, csrf) = browser_auth(&fixture);
    let cookie_value = format!("rcdvlog_session={session_id}:{session_token}");
    let base = format!("/api/v1/projects/{project_id}/review-targets");
    let post = |uri: String, body: Value, csrf: Option<&str>| {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, &cookie_value);
        if let Some(csrf) = csrf {
            builder = builder.header("x-csrf-token", csrf);
        }
        builder
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    };
    let review = historical_review(&project_id);

    // Through the strict route: named, not a generic parse failure.
    let strict = fixture
        .app()
        .oneshot(post(
            format!("{base}/import"),
            serde_json::json!({"handoff": review.clone()}),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(strict.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json(strict).await["code"],
        "review_target_historical_contract"
    );
    // Without the acknowledgement: refused.
    let unacknowledged = fixture
        .app()
        .oneshot(post(
            format!("{base}/historical/import"),
            serde_json::json!({"review": review.clone()}),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(unacknowledged.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json(unacknowledged).await["code"],
        "review_target_historical_acknowledgement_required"
    );
    let request = serde_json::json!({
        "review": review,
        "acknowledgement": "integrityOnlyCurrentRenderabilityNotEstablished"
    });
    let no_csrf = fixture
        .app()
        .oneshot(post(
            format!("{base}/historical/import"),
            request.clone(),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(no_csrf.status(), StatusCode::FORBIDDEN);
    let preflight = fixture
        .app()
        .oneshot(post(
            format!("{base}/historical/preflight"),
            request.clone(),
            Some(&csrf),
        ))
        .await
        .unwrap();
    let preflight = json(preflight).await;
    assert_eq!(preflight["outcome"], "new");
    assert_eq!(
        preflight["preview"]["contract"],
        "historicalRenderedArtifactReviewV1"
    );
    let imported = fixture
        .app()
        .oneshot(post(
            format!("{base}/historical/import"),
            request.clone(),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(imported.status(), StatusCode::OK);
    let imported = json(imported).await;
    let target = &imported["reviewTarget"];
    assert_eq!(target["contract"], "historicalRenderedArtifactReviewV1");
    assert_eq!(
        target["observations"][0]["verification"]["currentPlanRenderability"],
        "notEstablishedByThisReview"
    );
    assert!(
        target["limitations"][0]
            .as_str()
            .unwrap()
            .starts_with("Historical rendered-artifact review")
    );
    // The same snapshot through the strict route is a contract conflict.
    let conflict = fixture
        .app()
        .oneshot(post(
            format!("{base}/import"),
            serde_json::json!({"handoff": review_target_handoff(&project_id, None)}),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(
        json(conflict).await["code"],
        "review_target_contract_conflict"
    );
    let events = fixture
        .foundation
        .catalog()
        .audit_log()
        .list(20, None)
        .unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.action == "review_target.historical_imported")
    );
}

#[tokio::test]
async fn review_session_routes_record_hashed_chunks_finalize_and_delete_privately() {
    use sha2::Digest;
    let fixture = Fixture::new(false);
    let source = TempDir::new().unwrap();
    let envelope = baseline_envelope(&source);
    let project_id = envelope.payload.project.project_id.clone();
    fixture
        .foundation
        .catalog()
        .project_replicas()
        .import_baseline(&envelope)
        .unwrap();
    let (session_id, session_token, csrf) = browser_auth(&fixture);
    let cookie = format!("rcdvlog_session={session_id}:{session_token}");
    let target = fixture
        .foundation
        .catalog()
        .review_targets()
        .import(
            &project_id,
            &rcdvlog_server_core::ReviewTargetImportRequest {
                handoff: review_target_handoff(&project_id, None),
                declared_issuer: None,
                availability: None,
                max_verification_age_seconds: None,
            },
            &rcdvlog_server_core::ReviewTargetActor::new(
                AuditActorType::BrowserSession,
                &session_id,
            ),
            chrono::Utc::now(),
        )
        .unwrap()
        .review_target
        .summary
        .review_target_id;
    let base = format!("/api/v1/projects/{project_id}/review-sessions");
    let send =
        |method: Method, uri: String, content_type: &str, body: Vec<u8>, csrf: Option<&str>| {
            let mut builder = Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::COOKIE, &cookie);
            if let Some(csrf) = csrf {
                builder = builder.header("x-csrf-token", csrf);
            }
            builder.body(Body::from(body)).unwrap()
        };
    let create = serde_json::json!({
        "reviewTargetId": target,
        "clientSessionKey": "browser_key_1",
        "consent": {"allPresentAcknowledged": true, "additionalSpeakersDisclosed": true},
        "audio": {"sampleRate": 48000, "channels": 1, "encoding": "pcm_s16le"},
        "capture": {"echoCancellation": false, "noiseSuppression": false, "autoGainControl": false,
                    "clock": "audioWorkletCurrentFrame", "baseLatencySeconds": null, "userAgentFamily": "chromium"},
        "clientStartedPerformanceMs": 10.0
    });
    let created = fixture
        .app()
        .oneshot(send(
            Method::POST,
            base.clone(),
            "application/json",
            serde_json::to_vec(&create).unwrap(),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    assert_eq!(
        created.headers().get("permissions-policy").unwrap(),
        "camera=(), microphone=(self), geolocation=()"
    );
    let created = json(created).await;
    let id = created["sessionId"].as_str().unwrap().to_string();
    assert_eq!(created["state"], "recording");
    assert!(
        !serde_json::to_string(&created)
            .unwrap()
            .contains("review-evidence")
    );

    let pcm: Vec<u8> = (0..4800_i16).flat_map(|v| v.to_le_bytes()).collect();
    let sha: String = sha2::Sha256::digest(&pcm)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let chunk_uri = format!(
        "{base}/{id}/chunks/0?startFrame=0&frameCount=4800&sha256={sha}&captureEndMs=110.5"
    );
    let no_csrf = fixture
        .app()
        .oneshot(send(
            Method::PUT,
            chunk_uri.clone(),
            "application/octet-stream",
            pcm.clone(),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(no_csrf.status(), StatusCode::FORBIDDEN);
    let stored = fixture
        .app()
        .oneshot(send(
            Method::PUT,
            chunk_uri.clone(),
            "application/octet-stream",
            pcm.clone(),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(stored.status(), StatusCode::OK);
    assert_eq!(json(stored).await["duplicate"], false);
    let retry = fixture
        .app()
        .oneshot(send(
            Method::PUT,
            chunk_uri,
            "application/octet-stream",
            pcm.clone(),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(json(retry).await["duplicate"], true);
    let corrupt = fixture
        .app()
        .oneshot(send(
            Method::PUT,
            format!("{base}/{id}/chunks/1?startFrame=4800&frameCount=4800&sha256={sha}"),
            "application/octet-stream",
            vec![1; 9600],
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(corrupt.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json(corrupt).await["code"], "review_session_chunk_corrupt");

    let cue = fixture
        .app()
        .oneshot(send(
            Method::POST,
            format!("{base}/{id}/events"),
            "application/json",
            serde_json::to_vec(&serde_json::json!({"eventId":"cue_1","kind":"manualSyncCue","audioFrame":2400,"performanceMs":60.0,"declaredMovieSeconds":0.0})).unwrap(),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(cue.status(), StatusCode::OK);

    let early_audio = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri(format!("{base}/{id}/audio.wav"))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(early_audio.status(), StatusCode::CONFLICT);
    assert_eq!(
        json(early_audio).await["code"],
        "review_session_audio_unavailable"
    );

    let finalized = fixture
        .app()
        .oneshot(send(
            Method::POST,
            format!("{base}/{id}/finalize"),
            "application/json",
            serde_json::to_vec(&serde_json::json!({"finalFrameCount":4800,"endReason":"stopped"}))
                .unwrap(),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(finalized.status(), StatusCode::OK);
    let finalized = json(finalized).await;
    assert_eq!(finalized["state"], "completed");
    assert_eq!(finalized["manualCueCount"], 1);

    let audio = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri(format!("{base}/{id}/audio.wav"))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(audio.status(), StatusCode::OK);
    assert_eq!(
        audio.headers().get(header::CONTENT_TYPE).unwrap(),
        "audio/wav"
    );
    let wav = axum::body::to_bytes(audio.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(&wav[44..], pcm.as_slice());

    let deleted = fixture
        .app()
        .oneshot(send(
            Method::DELETE,
            format!("{base}/{id}"),
            "application/json",
            vec![],
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(json(deleted).await["state"], "deleted");
    let gone = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri(format!("{base}/{id}/audio.wav"))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(gone.status(), StatusCode::CONFLICT);
    let anonymous = fixture
        .app()
        .oneshot(Request::builder().uri(&base).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let events = fixture
        .foundation
        .catalog()
        .audit_log()
        .list(20, None)
        .unwrap();
    for action in [
        "review_session.started",
        "review_session.finalized",
        "review_session.deleted",
    ] {
        assert!(
            events.iter().any(|e| e.action == action),
            "missing {action}"
        );
    }
}

#[tokio::test]
async fn review_transcription_routes_refuse_unauthorized_cloud_dispatch() {
    let fixture = Fixture::new(false);
    let source = TempDir::new().unwrap();
    let envelope = baseline_envelope(&source);
    let project_id = envelope.payload.project.project_id.clone();
    fixture
        .foundation
        .catalog()
        .project_replicas()
        .import_baseline(&envelope)
        .unwrap();
    let (session_id, session_token, csrf) = browser_auth(&fixture);
    let cookie = format!("rcdvlog_session={session_id}:{session_token}");
    let actor =
        rcdvlog_server_core::ReviewTargetActor::new(AuditActorType::BrowserSession, &session_id);
    let target = fixture
        .foundation
        .catalog()
        .review_targets()
        .import(
            &project_id,
            &rcdvlog_server_core::ReviewTargetImportRequest {
                handoff: review_target_handoff(&project_id, None),
                declared_issuer: None,
                availability: None,
                max_verification_age_seconds: None,
            },
            &actor,
            chrono::Utc::now(),
        )
        .unwrap()
        .review_target
        .summary
        .review_target_id;
    let sessions = fixture.foundation.catalog().review_sessions(0);
    let recording = sessions.create(&project_id, &serde_json::from_value(serde_json::json!({
        "reviewTargetId": target, "clientSessionKey": "http_asr",
        "consent": {"allPresentAcknowledged": true, "additionalSpeakersDisclosed": false},
        "audio": {"sampleRate": 48000, "channels": 1, "encoding": "pcm_s16le"},
        "capture": {"echoCancellation": false, "noiseSuppression": false, "autoGainControl": false, "clock": "audioWorkletCurrentFrame", "baseLatencySeconds": null, "userAgentFamily": null},
        "clientStartedPerformanceMs": null
    })).unwrap(), &actor, chrono::Utc::now()).unwrap().summary.session_id;
    let pcm = vec![0_u8; 9600];
    use sha2::Digest;
    let sha: String = sha2::Sha256::digest(&pcm)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    sessions
        .put_chunk(
            &project_id,
            &recording,
            &rcdvlog_server_core::ChunkUpload {
                sequence: 0,
                start_frame: 0,
                frame_count: 4800,
                sha256: sha,
                capture_end_performance_ms: None,
            },
            &pcm,
            chrono::Utc::now(),
        )
        .unwrap();
    sessions
        .finalize(
            &project_id,
            &recording,
            &rcdvlog_server_core::FinalizeReviewSessionRequest {
                final_frame_count: 4800,
                end_reason: rcdvlog_server_core::RecordingEndReason::Stopped,
            },
            chrono::Utc::now(),
        )
        .unwrap();

    let base = format!("/api/v1/projects/{project_id}/review-sessions/{recording}/transcriptions");
    let post = |uri: String, body: serde_json::Value, csrf: Option<&str>| {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, &cookie);
        if let Some(csrf) = csrf {
            builder = builder.header("x-csrf-token", csrf);
        }
        builder
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    };
    let live_uri =
        format!("/api/v1/projects/{project_id}/review-sessions/{recording}/live-transcript");
    let unauth = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri(&live_uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);
    let no_csrf = fixture
        .app()
        .oneshot(post(live_uri.clone(), serde_json::json!({}), None))
        .await
        .unwrap();
    assert_eq!(no_csrf.status(), StatusCode::FORBIDDEN);
    let enabled = fixture
        .app()
        .oneshot(post(live_uri.clone(), serde_json::json!({}), Some(&csrf)))
        .await
        .unwrap();
    assert_eq!(enabled.status(), StatusCode::OK);
    assert_eq!(json(enabled).await["enabled"], true);
    let read = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri(&live_uri)
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(read.status(), StatusCode::OK);
    let value = json(read).await;
    assert_eq!(value["model"], "ggml-tiny.bin");
    assert_eq!(value["state"], "live");
    assert!(value["lines"].as_array().unwrap().is_empty());

    let unacknowledged = serde_json::json!({"backend": "openrouter", "model": "openai/whisper-1", "authorizedMaxCostUsd": 0.5, "cloudUploadAcknowledged": false});
    let no_csrf = fixture
        .app()
        .oneshot(post(base.clone(), unacknowledged.clone(), None))
        .await
        .unwrap();
    assert_eq!(no_csrf.status(), StatusCode::FORBIDDEN);
    let preflight = fixture
        .app()
        .oneshot(post(
            format!("{base}/preflight"),
            unacknowledged.clone(),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(preflight.status(), StatusCode::OK);
    let preflight = json(preflight).await;
    assert_eq!(preflight["ready"], false);
    assert!(
        preflight["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b == "review_transcription_cloud_not_authorized")
    );
    assert_eq!(preflight["estimate"]["maxCostUsd"], 0.003);
    let refused = fixture
        .app()
        .oneshot(post(base.clone(), unacknowledged, Some(&csrf)))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let fallback = fixture.app().oneshot(post(base.clone(), serde_json::json!({"backend": "openrouter", "model": "google/gemini-2.5-flash", "authorizedMaxCostUsd": 0.5, "cloudUploadAcknowledged": true}), Some(&csrf))).await.unwrap();
    assert_eq!(fallback.status(), StatusCode::BAD_REQUEST);
    let listed = fixture
        .app()
        .oneshot(
            Request::builder()
                .uri(&base)
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(json(listed).await["items"].as_array().unwrap().len(), 0);
}
#[tokio::test]
async fn attached_proxy_streams_seekable_mp4_and_never_exposes_originals_or_paths() {
    let fixture = Fixture::new(false);
    let token = device_auth_token(&fixture);
    let work = fixture._root.path().join("projects/playback");
    std::fs::create_dir_all(&work).unwrap();
    let db = work.join("catalog.sqlite3");
    let (_catalog, project) = cli_catalog(&db);
    let proxy = work.join("asset_http.v3.mp4");
    let bytes = include_bytes!("../tests/fixtures/proxy.v3.mp4");
    std::fs::write(&proxy, bytes).unwrap();
    let c = rusqlite::Connection::open(&db).unwrap();
    c.execute(
        "UPDATE assets SET proxy_path=?1 WHERE id='asset_http'",
        [proxy.to_str().unwrap()],
    )
    .unwrap();
    fixture
        .foundation
        .catalog()
        .project_replicas()
        .attach_catalog(
            &db,
            &fixture.foundation.config().attached_project_roots,
            None,
        )
        .unwrap();
    let uri = format!("/api/v1/projects/{project}/assets/asset_http/proxy");
    let make =
        |method: Method, path: &str, range: Option<&str>, conditional: Option<(&str, &str)>| {
            let mut req = Request::builder()
                .method(method)
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {token}"));
            if let Some(range) = range {
                req = req.header(header::RANGE, range);
            }
            if let Some((key, value)) = conditional {
                req = req.header(key, value);
            }
            req.body(Body::empty()).unwrap()
        };
    let anonymous = fixture
        .app()
        .oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let full = fixture
        .app()
        .oneshot(make(Method::GET, &uri, None, None))
        .await
        .unwrap();
    assert_eq!(full.status(), StatusCode::OK);
    assert_eq!(full.headers()[header::CONTENT_TYPE], "video/mp4");
    assert_eq!(full.headers()[header::ACCEPT_RANGES], "bytes");
    assert!(
        full.headers()[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .starts_with("private")
    );
    let etag = full.headers()[header::ETAG].to_str().unwrap().to_string();
    assert_eq!(
        to_bytes(full.into_body(), bytes.len() + 1)
            .await
            .unwrap()
            .as_ref(),
        &bytes[..]
    );
    for (range, start, end) in [
        ("bytes=4-15", 4, 15),
        ("bytes=-7", bytes.len() - 7, bytes.len() - 1),
        ("bytes=100-", 100, bytes.len() - 1),
    ] {
        let response = fixture
            .app()
            .oneshot(make(Method::GET, &uri, Some(range), None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers()[header::CONTENT_RANGE],
            format!("bytes {start}-{end}/{}", bytes.len())
        );
        assert_eq!(
            to_bytes(response.into_body(), bytes.len() + 1)
                .await
                .unwrap()
                .as_ref(),
            &bytes[start..=end]
        );
    }
    let head = fixture
        .app()
        .oneshot(make(Method::HEAD, &uri, Some("bytes=4-15"), None))
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(head.headers()[header::CONTENT_LENGTH], "12");
    assert!(to_bytes(head.into_body(), 1).await.unwrap().is_empty());
    let invalid = fixture
        .app()
        .oneshot(make(Method::GET, &uri, Some("bytes=999999999-"), None))
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        invalid.headers()[header::CONTENT_RANGE],
        format!("bytes */{}", bytes.len())
    );
    let cached = fixture
        .app()
        .oneshot(make(
            Method::GET,
            &uri,
            None,
            Some(("if-none-match", &etag)),
        ))
        .await
        .unwrap();
    assert_eq!(cached.status(), StatusCode::NOT_MODIFIED);
    let stale_range = fixture
        .app()
        .oneshot(make(
            Method::GET,
            &uri,
            Some("bytes=4-15"),
            Some(("if-range", "\"old\"")),
        ))
        .await
        .unwrap();
    assert_eq!(stale_range.status(), StatusCode::OK);
    let wrong = fixture
        .app()
        .oneshot(make(
            Method::GET,
            "/api/v1/projects/other/assets/asset_http/proxy",
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::NOT_FOUND);
    let review = fixture
        .app()
        .oneshot(make(
            Method::GET,
            &format!("/api/v1/projects/{project}/assets/asset_http/review"),
            None,
            None,
        ))
        .await
        .unwrap();
    let projection = json(review).await;
    assert_eq!(projection["localProxyAvailable"], true);
    assert_eq!(projection["proxy"], serde_json::Value::Null);
    assert!(!projection.to_string().contains(work.to_str().unwrap()));
    // Replacing or removing a proxy does not leave immutable stale cache metadata.
    std::fs::write(&proxy, &bytes[..bytes.len() - 1]).unwrap();
    let changed = fixture
        .app()
        .oneshot(make(Method::HEAD, &uri, None, None))
        .await
        .unwrap();
    assert_ne!(changed.headers()[header::ETAG], etag.as_str());
    std::fs::remove_file(&proxy).unwrap();
    let missing = fixture
        .app()
        .oneshot(make(Method::GET, &uri, None, None))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}
