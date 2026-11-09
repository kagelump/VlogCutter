use super::*;
use std::collections::HashMap;
use std::io::Write as _;
use tempfile::TempDir;

struct FixtureCatalog {
    assets: HashMap<String, AssetMedia>,
    roots: Vec<PathBuf>,
}

impl MediaCatalog for FixtureCatalog {
    fn locate(&self, asset_id: &str) -> Option<AssetMedia> {
        self.assets.get(asset_id).cloned()
    }

    fn storage_roots(&self) -> Vec<PathBuf> {
        self.roots.clone()
    }
}

struct RunningOrigin {
    origin: MediaOrigin,
    _shutdown: oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
}

impl RunningOrigin {
    fn media_url(&self, asset_id: &str, query: &str) -> String {
        format!("{}/media/{asset_id}?{query}", self.origin.base_url)
    }
}

fn write_fixture(directory: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = directory.join(name);
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(bytes).unwrap();
    path
}

async fn start_fixture(catalog: FixtureCatalog) -> RunningOrigin {
    start_fixture_with(catalog, false).await
}

/// `diagnostics` is passed explicitly rather than through the environment,
/// so these tests do not race each other over a process-wide variable.
async fn start_fixture_with(catalog: FixtureCatalog, diagnostics: bool) -> RunningOrigin {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (state, origin) = OriginState::mint_with(port, Arc::new(catalog), diagnostics);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let handle = tokio::spawn(serve(listener, state, shutdown_rx));
    RunningOrigin {
        origin,
        _shutdown: shutdown_tx,
        handle,
    }
}

/// One clip whose proxy and original both exist under an allowed root.
fn clip_catalog(directory: &TempDir, bytes: &[u8]) -> FixtureCatalog {
    let original = write_fixture(directory.path(), "C0163.MP4", bytes);
    let proxy = write_fixture(directory.path(), "C0163.proxy.mp4", &bytes[..1024]);
    FixtureCatalog {
        assets: HashMap::from([(
            "asset_1".to_string(),
            AssetMedia {
                original: Some(original),
                proxy: Some(proxy),
                camera_proxy: None,
            },
        )]),
        roots: vec![directory.path().to_path_buf()],
    }
}

async fn clip_fixture() -> (TempDir, RunningOrigin, Vec<u8>) {
    let directory = TempDir::new().unwrap();
    let bytes: Vec<u8> = (0..4096u32).map(|index| (index % 251) as u8).collect();
    let catalog = clip_catalog(&directory, &bytes);
    let server = start_fixture(catalog).await;
    (directory, server, bytes)
}

#[test]
fn token_is_at_least_128_bits_and_url_safe() {
    let token = generate_token(TOKEN_BYTES);
    assert!(token.len() >= 22, "{token} is shorter than 128 bits");
    assert!(
        token
            .chars()
            .all(|character| character.is_ascii_alphanumeric()
                || character == '-'
                || character == '_'),
        "{token} is not URL safe"
    );
    assert_ne!(token, generate_token(TOKEN_BYTES));
}

#[test]
fn secret_comparison_rejects_prefixes_and_lengths() {
    assert!(secret_matches("abc123", "abc123"));
    assert!(!secret_matches("abc123", "abc12"));
    assert!(!secret_matches("abc123", "abc1234"));
    assert!(!secret_matches("abc123", "abc124"));
}

#[test]
fn host_and_origin_checks_accept_only_the_app() {
    let authority = "127.0.0.1:49512";
    assert!(host_matches(
        Some(&HeaderValue::from_static("127.0.0.1:49512")),
        authority
    ));
    assert!(!host_matches(
        Some(&HeaderValue::from_static("attacker.example:49512")),
        authority
    ));
    assert!(!host_matches(
        Some(&HeaderValue::from_static("127.0.0.1:1")),
        authority
    ));
    assert!(!host_matches(None, authority));

    // A media element sends no Origin at all; that is the normal case.
    assert!(origin_allowed(None));
    assert!(origin_allowed(Some(&HeaderValue::from_static(
        "tauri://localhost"
    ))));
    assert!(origin_allowed(Some(&HeaderValue::from_static(
        "http://localhost:1420"
    ))));
    assert!(!origin_allowed(Some(&HeaderValue::from_static(
        "https://attacker.example"
    ))));
    assert!(!origin_allowed(Some(&HeaderValue::from_static("null"))));
}

#[test]
fn range_parser_handles_open_suffix_and_unsatisfiable_ranges() {
    assert_eq!(parse_range(None, 10), Ok(None));
    assert_eq!(
        parse_range(Some(&HeaderValue::from_static("bytes=4-")), 10),
        Ok(Some((4, 9)))
    );
    assert_eq!(
        parse_range(Some(&HeaderValue::from_static("bytes=-3")), 10),
        Ok(Some((7, 9)))
    );
    assert_eq!(
        parse_range(Some(&HeaderValue::from_static("bytes=2-4")), 10),
        Ok(Some((2, 4)))
    );
    assert!(parse_range(Some(&HeaderValue::from_static("bytes=10-")), 10).is_err());
    assert!(parse_range(Some(&HeaderValue::from_static("bytes=0-1,4-5")), 10).is_err());
    assert!(parse_range(Some(&HeaderValue::from_static("items=0-1")), 10).is_err());
}

#[test]
fn representation_resolution_matches_the_frontend_rule() {
    let directory = TempDir::new().unwrap();
    let original = write_fixture(directory.path(), "original.mp4", b"original");
    let proxy = write_fixture(directory.path(), "proxy.mp4", b"proxy");
    let camera_proxy = write_fixture(directory.path(), "camera.LRF", b"sidecar");
    let roots = vec![directory.path().to_path_buf()];
    let media = AssetMedia {
        original: Some(original.clone()),
        proxy: Some(proxy.clone()),
        camera_proxy: Some(camera_proxy.clone()),
    };

    assert_eq!(
        resolve_media_path(&media, Representation::Auto, &roots),
        Ok(proxy.canonicalize().unwrap())
    );
    assert_eq!(
        resolve_media_path(&media, Representation::Original, &roots),
        Ok(original.canonicalize().unwrap())
    );
    assert_eq!(
        resolve_media_path(&media, Representation::CameraProxy, &roots),
        Ok(camera_proxy.canonicalize().unwrap())
    );
    // Explicit sidecar playback never shadows normal generated/original selection.
    assert_eq!(
        resolve_media_path(&media, Representation::Auto, &roots),
        Ok(proxy.canonicalize().unwrap())
    );
    let offline_original = AssetMedia {
        original: Some(directory.path().join("offline.mp4")),
        proxy: None,
        camera_proxy: Some(camera_proxy.clone()),
    };
    assert_eq!(
        resolve_media_path(&offline_original, Representation::CameraProxy, &roots),
        Ok(camera_proxy.canonicalize().unwrap())
    );
    assert_eq!(
        resolve_media_path(&offline_original, Representation::Auto, &roots),
        Ok(camera_proxy.canonicalize().unwrap())
    );
    assert_eq!(Representation::CameraProxy.as_str(), "camera_proxy");

    // A recorded proxy that is no longer on disk is not silently swapped for
    // the original when the caller asked for the proxy...
    let missing_proxy = AssetMedia {
        original: Some(original.clone()),
        proxy: Some(directory.path().join("gone.mp4")),
        camera_proxy: None,
    };
    assert_eq!(
        resolve_media_path(&missing_proxy, Representation::Proxy, &roots),
        Err(ResolveFailure::Missing)
    );
    // ...but `auto` does fall back, because that is what the queue wants.
    assert_eq!(
        resolve_media_path(&missing_proxy, Representation::Auto, &roots),
        Ok(original.canonicalize().unwrap())
    );
    // A representation the catalog has no path for is named as such rather
    // than being reported as a missing file.
    assert_eq!(
        resolve_media_path(&AssetMedia::default(), Representation::Auto, &roots),
        Err(ResolveFailure::NoRepresentation)
    );
}

#[test]
fn paths_outside_the_storage_roots_are_refused() {
    let allowed = TempDir::new().unwrap();
    let elsewhere = TempDir::new().unwrap();
    let outside = write_fixture(elsewhere.path(), "secret.mp4", b"secret");
    let roots = vec![allowed.path().to_path_buf()];

    // A hostile catalog row naming an absolute path outside the roots.
    assert_eq!(
        resolve_media_path(
            &AssetMedia {
                original: Some(outside.clone()),
                proxy: None,
                camera_proxy: None,
            },
            Representation::Original,
            &roots,
        ),
        Err(ResolveFailure::OutsideRoots)
    );

    // The same row dressed up as a traversal out of an allowed root.
    let traversal = allowed
        .path()
        .join("..")
        .join(elsewhere.path().file_name().unwrap())
        .join("secret.mp4");
    assert_eq!(
        resolve_media_path(
            &AssetMedia {
                original: Some(traversal),
                proxy: None,
                camera_proxy: None,
            },
            Representation::Original,
            &roots,
        ),
        Err(ResolveFailure::OutsideRoots)
    );

    // A directory is not media, and no roots at all denies everything.
    assert_eq!(
        resolve_media_path(
            &AssetMedia {
                original: Some(allowed.path().to_path_buf()),
                proxy: None,
                camera_proxy: None,
            },
            Representation::Original,
            &roots,
        ),
        Err(ResolveFailure::NotAFile)
    );
    assert_eq!(
        resolve_media_path(
            &AssetMedia {
                original: Some(outside),
                proxy: None,
                camera_proxy: None,
            },
            Representation::Original,
            &[],
        ),
        Err(ResolveFailure::OutsideRoots)
    );
}

#[tokio::test]
async fn serves_whole_files_and_ranges_with_a_valid_token() {
    let (_directory, server, bytes) = clip_fixture().await;
    let token = &server.origin.token;

    let response = reqwest::get(server.media_url("asset_1", &format!("rep=original&t={token}")))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "video/mp4");
    assert_eq!(response.headers()["accept-ranges"], "bytes");
    assert_eq!(response.headers()["content-length"], "4096");
    assert_eq!(response.bytes().await.unwrap().to_vec(), bytes);

    let client = reqwest::Client::new();
    let partial = client
        .get(server.media_url("asset_1", &format!("rep=original&t={token}")))
        .header(header::RANGE, "bytes=1000-1099")
        .send()
        .await
        .unwrap();
    assert_eq!(partial.status(), 206);
    assert_eq!(partial.headers()["content-range"], "bytes 1000-1099/4096");
    assert_eq!(partial.bytes().await.unwrap().to_vec(), bytes[1000..1100]);

    let unsatisfiable = client
        .get(server.media_url("asset_1", &format!("rep=original&t={token}")))
        .header(header::RANGE, "bytes=99999-")
        .send()
        .await
        .unwrap();
    assert_eq!(unsatisfiable.status(), 416);
    assert_eq!(unsatisfiable.headers()["content-range"], "bytes */4096");

    // `HEAD` reports the full length and no body.
    let head = client
        .head(server.media_url("asset_1", &format!("rep=original&t={token}")))
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), 200);
    assert_eq!(head.headers()["content-length"], "4096");
    assert!(head.bytes().await.unwrap().is_empty());
}

#[tokio::test]
async fn proxy_is_preferred_and_selectable() {
    let (_directory, server, bytes) = clip_fixture().await;
    let token = &server.origin.token;

    for query in [format!("t={token}"), format!("rep=proxy&t={token}")] {
        let response = reqwest::get(server.media_url("asset_1", &query))
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.bytes().await.unwrap().to_vec(), bytes[..1024]);
    }
}

#[tokio::test]
async fn requests_without_a_valid_token_are_refused() {
    let (_directory, server, _bytes) = clip_fixture().await;
    let client = reqwest::Client::new();

    for query in [
        String::new(),
        "t=".to_string(),
        format!("t={}x", server.origin.token),
        format!("t={}", &server.origin.token[..8]),
    ] {
        let response = client
            .get(server.media_url("asset_1", &query))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 403, "query {query:?} should be refused");
    }

    // Including for an asset that does not exist: the token is checked
    // first, so a caller without one cannot even probe the catalog.
    let response = client
        .get(server.media_url("asset_missing", ""))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
}

#[tokio::test]
async fn foreign_origins_and_rebound_hosts_are_refused() {
    let (_directory, server, _bytes) = clip_fixture().await;
    let token = &server.origin.token;
    let client = reqwest::Client::new();
    let url = server.media_url("asset_1", &format!("rep=original&t={token}"));

    let foreign = client
        .get(&url)
        .header(header::ORIGIN, "https://attacker.example")
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), 403);

    // DNS rebinding: the connection lands on loopback but carries the
    // attacker's hostname.
    let rebound = client
        .get(&url)
        .header(header::HOST, "attacker.example")
        .send()
        .await
        .unwrap();
    assert_eq!(rebound.status(), 403);

    let ours = client
        .get(&url)
        .header(header::ORIGIN, "tauri://localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(ours.status(), 200);
}

#[tokio::test]
async fn unknown_assets_and_write_methods_are_refused() {
    let (_directory, server, _bytes) = clip_fixture().await;
    let token = &server.origin.token;
    let client = reqwest::Client::new();

    let unknown =
        reqwest::get(server.media_url("asset_from_another_project", &format!("t={token}")))
            .await
            .unwrap();
    assert_eq!(unknown.status(), 404);

    for method in [Method::POST, Method::PUT, Method::DELETE] {
        let response = client
            .request(
                method.clone(),
                server.media_url("asset_1", &format!("t={token}")),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            405,
            "{method} should not reach a handler"
        );
    }

    // There is no listing endpoint and no path endpoint to traverse.
    for path in ["/media/", "/media", "/", "/media/asset_1/../../etc/passwd"] {
        let response = client
            .get(format!("{}{path}?t={token}", server.origin.base_url))
            .send()
            .await
            .unwrap();
        assert!(
            response.status() == 404 || response.status() == 301,
            "{path} returned {}",
            response.status()
        );
    }
}

#[tokio::test]
async fn health_check_answers_only_with_the_token() {
    let (_directory, server, _bytes) = clip_fixture().await;
    let healthy = reqwest::get(format!(
        "{}/healthz?t={}",
        server.origin.base_url, server.origin.token
    ))
    .await
    .unwrap();
    assert_eq!(healthy.status(), 204);

    let anonymous = reqwest::get(format!("{}/healthz", server.origin.base_url))
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 403);
}

#[tokio::test]
async fn diagnostics_are_absent_unless_the_mode_was_asked_for() {
    let (_directory, server, _bytes) = clip_fixture().await;
    assert_eq!(server.origin.debug_url, None, "no debug URL to hand out");

    let response = reqwest::get(format!(
        "{}/debug/media?t={}",
        server.origin.base_url, server.origin.token
    ))
    .await
    .unwrap();
    assert_eq!(
        response.status(),
        404,
        "the route should not exist, not merely refuse"
    );
}

#[tokio::test]
async fn diagnostics_report_what_the_origin_did_and_why() {
    let directory = TempDir::new().unwrap();
    let bytes: Vec<u8> = (0..4096u32).map(|index| (index % 251) as u8).collect();
    let server = start_fixture_with(clip_catalog(&directory, &bytes), true).await;
    let token = server.origin.token.clone();
    let debug_url = server.origin.debug_url.clone().expect("a debug URL");
    assert!(debug_url.contains("/debug/media"));

    // Still guarded exactly like the media route.
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(format!("{}/debug/media", server.origin.base_url))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .get(&debug_url)
            .header(header::ORIGIN, "https://attacker.example")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );

    // Three requests with three different outcomes.
    client
        .get(server.media_url("asset_1", &format!("rep=proxy&t={token}")))
        .header(header::RANGE, "bytes=0-15")
        .send()
        .await
        .unwrap();
    client
        .get(server.media_url("asset_gone", &format!("t={token}")))
        .send()
        .await
        .unwrap();
    client
        .get(server.media_url("asset_1", "t=wrong"))
        .send()
        .await
        .unwrap();

    let report: serde_json::Value = reqwest::get(&debug_url)
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(report["marker"], dev::COMPILED_MARKER);
    assert_eq!(report["outcomes"]["ok"], 1);
    assert_eq!(report["outcomes"]["unknown_asset"], 1);
    assert_eq!(report["outcomes"]["forbidden"], 1);
    assert!(report["uptimeSeconds"].as_f64().unwrap() >= 0.0);
    assert!(!report["storageRoots"].as_array().unwrap().is_empty());

    let recent = report["recent"].as_array().unwrap();
    assert_eq!(
        recent.len(),
        3,
        "the three media requests, and not the diagnostics request itself"
    );
    assert_eq!(recent[0]["assetId"], "asset_1");
    assert_eq!(recent[0]["representation"], "proxy");
    assert_eq!(recent[0]["range"], "bytes=0-15");
    assert_eq!(recent[0]["status"], 206);
    assert_eq!(recent[0]["plannedBytes"], 16);
    assert!(
        recent[0]["resolvedPath"]
            .as_str()
            .unwrap()
            .ends_with("C0163.proxy.mp4")
    );
    assert_eq!(recent[1]["outcome"], "unknown_asset");
    // A refused request is recorded without anything the caller sent.
    assert_eq!(recent[2]["outcome"], "forbidden");
    assert_eq!(recent[2]["resolvedPath"], serde_json::Value::Null);
    assert!(!report.to_string().contains("t=wrong"));

    // The probe answers the question a 404 does not.
    let probed: serde_json::Value = reqwest::get(format!("{debug_url}&asset=asset_gone"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(probed["probe"]["outcome"], "unknown_asset");
    assert_eq!(probed["probe"]["inOpenCatalog"], false);

    let probed: serde_json::Value = reqwest::get(format!("{debug_url}&asset=asset_1&rep=original"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(probed["probe"]["outcome"], "ok");
    assert_eq!(probed["probe"]["representation"], "original");
    assert!(
        probed["probe"]["resolvedPath"]
            .as_str()
            .unwrap()
            .ends_with("C0163.MP4")
    );
    assert!(probed["probe"]["catalogPaths"]["proxy"].is_string());
}

#[tokio::test]
async fn shutdown_makes_the_origin_unreachable() {
    let (_directory, server, _bytes) = clip_fixture().await;
    let RunningOrigin {
        origin,
        _shutdown,
        handle,
    } = server;
    let url = format!("{}/healthz?t={}", origin.base_url, origin.token);
    assert_eq!(reqwest::get(&url).await.unwrap().status(), 204);

    drop(_shutdown);
    handle.await.unwrap();
    assert!(
        reqwest::Client::new()
            .get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .is_err(),
        "the origin answered after shutdown"
    );
}
