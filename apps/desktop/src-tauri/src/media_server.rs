//! A loopback HTTP origin that serves review media to the webview.
//!
//! `WKWebView` does not produce video frames for media delivered through a
//! `WKURLSchemeHandler` custom scheme, which is what Tauri's `asset://` is on
//! macOS: an element that has never been played reports `readyState 4`, correct
//! dimensions and no error, and holds nothing paintable. That is a property of
//! the transport, so the fix is a different transport rather than a workaround
//! at the call site. See `docs/tasks/desktop/MEDIA_DELIVERY.md`.
//!
//! A loopback listener is reachable by every process on the machine and, for
//! some request shapes, by any web page the user has open. The defences below
//! are requirements, not optimizations, and each one has a test:
//!
//! 1. Bind `127.0.0.1` on an ephemeral port. Never `0.0.0.0`.
//! 2. Require the per-session bearer token before any catalog or disk access.
//! 3. Accept asset ids, never filesystem paths, so there is no traversal
//!    surface to defend.
//! 4. Re-validate the path the catalog resolved against the storage roots, so a
//!    corrupted or hostile catalog row cannot escape either.
//! 5. Reject unexpected `Host` and `Origin` headers. `Host` is the DNS-rebinding
//!    defence: a public page can resolve its own hostname to `127.0.0.1`, but it
//!    cannot make the browser send our `Host`.
//! 6. `GET` and `HEAD` only.
//! 7. Never log the token or a full media path.
//! 8. Stop with the app, dropping the token with it.

use anyhow::Context as _;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodFilter, on};
use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, TcpListener as StdTcpListener};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_util::io::ReaderStream;

/// Origins the app itself can run under. A media element issues a no-CORS `GET`
/// and so normally sends no `Origin` at all; anything that does send one and is
/// not us is refused.
const ALLOWED_ORIGINS: &[&str] = &[
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
    "http://localhost:1420",
    "http://127.0.0.1:1420",
];

/// 256 bits from the OS CSPRNG, comfortably above the 128-bit floor.
const TOKEN_BYTES: usize = 32;

/// The base URL and per-session token the frontend needs to build media URLs.
/// Minted at startup, never written to disk, and dropped with the process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MediaOrigin {
    pub base_url: String,
    pub token: String,
    /// A ready-to-fetch diagnostics URL, token included. `Some` only in a debug
    /// build started with `RCDVLOG_MEDIA_DEBUG=1`; always `None` in a release
    /// binary, which is also how the frontend knows not to publish its dev
    /// handle. See [`dev`].
    pub debug_url: Option<String>,
}

/// Which representation of a clip the caller wants. The frontend resolves this
/// exactly as it always has — proxy when one is available, otherwise the
/// original — and names its choice here so the two sides cannot disagree about
/// what is on screen. Auto may use a verified camera sidecar only after both
/// generated proxy and original are unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Representation {
    #[default]
    Auto,
    Proxy,
    /// Camera supplied verified `.LRF` sidecar, independent of generated recipes.
    CameraProxy,
    Original,
}

impl Representation {
    /// Only the dev diagnostics name a representation in words.
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Proxy => "proxy",
            Self::CameraProxy => "camera_proxy",
            Self::Original => "original",
        }
    }
}

/// The paths a catalog row records for one asset. Neither is trusted: both are
/// re-validated against the storage roots before anything is opened.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AssetMedia {
    pub original: Option<PathBuf>,
    pub proxy: Option<PathBuf>,
    pub camera_proxy: Option<PathBuf>,
}

/// The catalog behind the origin. Implemented over the open project catalog by
/// the app and over fixtures by the tests.
pub(crate) trait MediaCatalog: Send + Sync + 'static {
    /// Resolve an asset id through the catalog. `None` means "not in the open
    /// catalog", which the caller reports as 404.
    fn locate(&self, asset_id: &str) -> Option<AssetMedia>;

    /// Roots that media is allowed to live under. An empty list denies
    /// everything, which is the safe direction to fail in.
    fn storage_roots(&self) -> Vec<PathBuf>;
}

struct OriginState {
    token: String,
    authority: String,
    catalog: Arc<dyn MediaCatalog>,
    /// Read by the dev diagnostics only.
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    started: Instant,
    /// `Some` only in a debug build with the mode opted into. See [`dev`].
    #[cfg(debug_assertions)]
    debug: Option<Arc<dev::Log>>,
}

/// A running loopback origin. Dropping it, or calling [`MediaServer::stop`],
/// shuts the listener down and drops the token.
pub(crate) struct MediaServer {
    origin: MediaOrigin,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
}

impl MediaServer {
    /// Bind `127.0.0.1:0`, read back the assigned port, and start serving.
    ///
    /// Failure is returned rather than swallowed: a clear startup error is much
    /// easier to act on than media that silently never loads.
    pub(crate) fn start(catalog: Arc<dyn MediaCatalog>) -> anyhow::Result<Self> {
        let listener = StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .context("bind the loopback media origin")?;
        listener
            .set_nonblocking(true)
            .context("configure the loopback media origin")?;
        let port = listener
            .local_addr()
            .context("read the loopback media origin port")?
            .port();
        let (state, origin) = OriginState::mint(port, catalog);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        tauri::async_runtime::spawn(async move {
            match TcpListener::from_std(listener) {
                Ok(listener) => serve(listener, state, shutdown_rx).await,
                Err(error) => eprintln!("[media-origin] listener failed: {error}"),
            }
        });

        // The token is deliberately absent from this line.
        println!("[media-origin] serving review media on {}", origin.base_url);
        #[cfg(debug_assertions)]
        if origin.debug_url.is_some() {
            // The URL carries the token, so this is deliberately the only place
            // it is ever printed, and only when the mode was asked for.
            println!(
                "[{}] diagnostics at {}",
                dev::COMPILED_MARKER,
                origin.debug_url.as_deref().unwrap_or_default()
            );
        }
        Ok(Self {
            origin,
            shutdown: Mutex::new(Some(shutdown_tx)),
        })
    }

    pub(crate) fn origin(&self) -> MediaOrigin {
        self.origin.clone()
    }

    pub(crate) fn stop(&self) {
        if let Ok(mut shutdown) = self.shutdown.lock() {
            drop(shutdown.take());
        }
    }
}

impl Drop for MediaServer {
    fn drop(&mut self) {
        self.stop();
    }
}

impl OriginState {
    fn mint(port: u16, catalog: Arc<dyn MediaCatalog>) -> (Arc<Self>, MediaOrigin) {
        // A release build has no diagnostics to enable, whatever the
        // environment says.
        #[cfg(debug_assertions)]
        let diagnostics = dev::enabled();
        #[cfg(not(debug_assertions))]
        let diagnostics = false;
        Self::mint_with(port, catalog, diagnostics)
    }

    fn mint_with(
        port: u16,
        catalog: Arc<dyn MediaCatalog>,
        diagnostics: bool,
    ) -> (Arc<Self>, MediaOrigin) {
        #[cfg(not(debug_assertions))]
        let _ = diagnostics;
        let token = generate_token(TOKEN_BYTES);
        let base_url = format!("http://127.0.0.1:{port}");

        #[cfg(debug_assertions)]
        let debug = diagnostics.then(|| Arc::new(dev::Log::default()));
        #[cfg(debug_assertions)]
        let debug_url = debug
            .is_some()
            .then(|| format!("{base_url}/debug/media?t={token}"));
        #[cfg(not(debug_assertions))]
        let debug_url = None;

        let origin = MediaOrigin {
            base_url,
            token: token.clone(),
            debug_url,
        };
        let state = Arc::new(Self {
            token,
            authority: format!("127.0.0.1:{port}"),
            catalog,
            started: Instant::now(),
            #[cfg(debug_assertions)]
            debug,
        });
        (state, origin)
    }
}

fn router(state: Arc<OriginState>) -> Router {
    // `GET` and `HEAD` only. Every other method falls through to 405 without
    // reaching a handler.
    let router = Router::new()
        .route(
            "/media/{asset_id}",
            on(MethodFilter::GET.or(MethodFilter::HEAD), media),
        )
        .route(
            "/healthz",
            on(MethodFilter::GET.or(MethodFilter::HEAD), healthz),
        );

    // The dev seam is a route that does not exist in a release binary, rather
    // than one that exists and refuses.
    #[cfg(debug_assertions)]
    let router = if state.debug.is_some() {
        router.route("/debug/media", axum::routing::get(dev::diagnostics))
    } else {
        router
    };

    router.with_state(state)
}

async fn serve(listener: TcpListener, state: Arc<OriginState>, shutdown: oneshot::Receiver<()>) {
    let served = axum::serve(listener, router(state).into_make_service())
        .with_graceful_shutdown(async move {
            let _ = shutdown.await;
        })
        .await;
    if let Err(error) = served {
        eprintln!("[media-origin] stopped: {error}");
    }
}

#[derive(Debug, Default, Deserialize)]
struct MediaQuery {
    /// The session token. Named `t` because it rides in the URL of every media
    /// element.
    #[serde(default)]
    t: Option<String>,
    #[serde(default)]
    rep: Option<Representation>,
}

/// Liveness for the frontend's restart-once recovery. Guarded like every other
/// route and touches neither the catalog nor the filesystem.
async fn healthz(
    State(state): State<Arc<OriginState>>,
    headers: HeaderMap,
    Query(query): Query<MediaQuery>,
) -> Response {
    if let Err(status) = guard(&state, &headers, query.t.as_deref()) {
        return status.into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn media(
    State(state): State<Arc<OriginState>>,
    method: Method,
    headers: HeaderMap,
    AxumPath(asset_id): AxumPath<String>,
    Query(query): Query<MediaQuery>,
) -> Response {
    let started = Instant::now();
    let representation = query.rep.unwrap_or_default();
    let (response, served) = serve_media(&state, &method, &headers, &asset_id, &query).await;

    #[cfg(debug_assertions)]
    if let Some(log) = &state.debug {
        log.record(dev::Record {
            asset_id: asset_id.clone(),
            representation: representation.as_str(),
            method: method.to_string(),
            range: headers
                .get(header::RANGE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
            status: response.status().as_u16(),
            outcome: served.outcome,
            resolved_path: served.resolved_path.clone(),
            planned_bytes: served.planned_bytes,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        });
    }
    let _ = (started, representation, served);
    response
}

/// What happened, beyond the status code. Only the dev diagnostics endpoint
/// reads this; a release build builds it and drops it.
#[cfg_attr(not(debug_assertions), allow(dead_code))]
struct Served {
    outcome: &'static str,
    resolved_path: Option<PathBuf>,
    planned_bytes: u64,
}

impl Served {
    fn refused(outcome: &'static str) -> Self {
        Self {
            outcome,
            resolved_path: None,
            planned_bytes: 0,
        }
    }
}

async fn serve_media(
    state: &Arc<OriginState>,
    method: &Method,
    headers: &HeaderMap,
    asset_id: &str,
    query: &MediaQuery,
) -> (Response, Served) {
    if let Err(status) = guard(state, headers, query.t.as_deref()) {
        return (status.into_response(), Served::refused("forbidden"));
    }

    let catalog = state.catalog.clone();
    let representation = query.rep.unwrap_or_default();
    let owned_id = asset_id.to_string();
    let lookup = tokio::task::spawn_blocking(move || {
        resolve_through_catalog(&catalog, &owned_id, representation)
    })
    .await;
    let path = match lookup {
        Ok(Ok(path)) => path,
        Ok(Err(failure)) => {
            return (
                StatusCode::NOT_FOUND.into_response(),
                Served::refused(failure.as_str()),
            );
        }
        Err(_) => {
            return (
                StatusCode::NOT_FOUND.into_response(),
                Served::refused("lookup_panicked"),
            );
        }
    };

    let Ok(metadata) = tokio::fs::metadata(&path).await else {
        return (
            StatusCode::NOT_FOUND.into_response(),
            Served::refused("missing"),
        );
    };
    let length = metadata.len();
    let etag = entity_tag(&metadata);

    // A stale `If-Range` means the client is holding bytes from a file that has
    // since changed — a regenerated proxy — so it gets the whole thing again.
    let range_applies = headers
        .get(header::IF_RANGE)
        .is_none_or(|value| value.as_bytes() == etag.as_bytes());
    let requested = if range_applies {
        headers.get(header::RANGE)
    } else {
        None
    };
    let selected = match parse_range(requested, length) {
        Ok(value) => value,
        Err(()) => {
            let mut response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{length}")).expect("valid content-range"),
            );
            headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            return (response, Served::refused("range_not_satisfiable"));
        }
    };
    let (start, end, status) = selected.map_or(
        (0, length.saturating_sub(1), StatusCode::OK),
        |(start, end)| (start, end, StatusCode::PARTIAL_CONTENT),
    );
    let response_length = if length == 0 { 0 } else { end - start + 1 };
    let served = Served {
        outcome: "ok",
        resolved_path: Some(path.clone()),
        planned_bytes: response_length,
    };

    let mut builder = Response::builder()
        .status(status)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, content_type(&path))
        .header(header::CONTENT_LENGTH, response_length.to_string())
        .header(header::ETAG, &etag)
        .header(header::CACHE_CONTROL, "private, max-age=0, must-revalidate")
        .header("x-content-type-options", "nosniff");
    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{length}"),
        );
    }
    if method == Method::HEAD {
        return (
            builder.body(Body::empty()).expect("valid media headers"),
            served,
        );
    }

    // Streamed from disk. A whole-file read of 4K footage is not an option, and
    // capping the range would cost round trips at exactly the wrong moment.
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(_) => {
            return (
                StatusCode::NOT_FOUND.into_response(),
                Served::refused("open_failed"),
            );
        }
    };
    if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return (
            StatusCode::NOT_FOUND.into_response(),
            Served::refused("seek_failed"),
        );
    }
    (
        builder
            .body(Body::from_stream(ReaderStream::new(
                file.take(response_length),
            )))
            .expect("valid media response"),
        served,
    )
}

/// The one place an asset id becomes a path. Blocking: it reads SQLite and the
/// filesystem, so callers run it on a blocking thread.
fn resolve_through_catalog(
    catalog: &Arc<dyn MediaCatalog>,
    asset_id: &str,
    representation: Representation,
) -> Result<PathBuf, ResolveFailure> {
    let media = catalog
        .locate(asset_id)
        .ok_or(ResolveFailure::UnknownAsset)?;
    resolve_media_path(&media, representation, &catalog.storage_roots())
}

/// Every check that must pass before a request is allowed to reach the catalog
/// or the filesystem.
fn guard(state: &OriginState, headers: &HeaderMap, token: Option<&str>) -> Result<(), StatusCode> {
    if !host_matches(headers.get(header::HOST), &state.authority) {
        return Err(StatusCode::FORBIDDEN);
    }
    if !origin_allowed(headers.get(header::ORIGIN)) {
        return Err(StatusCode::FORBIDDEN);
    }
    match token {
        Some(token) if secret_matches(&state.token, token) => Ok(()),
        _ => Err(StatusCode::FORBIDDEN),
    }
}

/// The DNS-rebinding defence. A page served from `evil.example` that resolves to
/// `127.0.0.1` still sends `Host: evil.example`, which is not ours.
fn host_matches(value: Option<&HeaderValue>, authority: &str) -> bool {
    value
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case(authority))
}

fn origin_allowed(value: Option<&HeaderValue>) -> bool {
    match value {
        None => true,
        Some(value) => value
            .to_str()
            .is_ok_and(|origin| ALLOWED_ORIGINS.contains(&origin)),
    }
}

fn secret_matches(expected: &str, provided: &str) -> bool {
    let expected = expected.as_bytes();
    let provided = provided.as_bytes();
    if expected.len() != provided.len() {
        return false;
    }
    expected
        .iter()
        .zip(provided)
        .fold(0u8, |accumulator, (left, right)| {
            accumulator | (left ^ right)
        })
        == 0
}

/// Why a request did not end in bytes. Every variant answers to 404 on the wire
/// — a caller learns nothing about which one it was — but the dev diagnostics
/// endpoint reports it, because "404" alone is a poor way to debug a black
/// frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolveFailure {
    /// The id is not in the open catalog.
    UnknownAsset,
    /// The catalog has no path for the representation that was asked for.
    NoRepresentation,
    /// The recorded path is not on disk.
    Missing,
    /// The recorded path exists but is a directory, device, or similar.
    NotAFile,
    /// The recorded path resolves outside every storage root. Requirement 4.
    OutsideRoots,
}

impl ResolveFailure {
    fn as_str(self) -> &'static str {
        match self {
            Self::UnknownAsset => "unknown_asset",
            Self::NoRepresentation => "no_representation",
            Self::Missing => "missing",
            Self::NotAFile => "not_a_file",
            Self::OutsideRoots => "outside_roots",
        }
    }
}

/// Pick a representation and prove the resulting path is one we are willing to
/// open. `Auto` falls back to the original when a recorded proxy has gone
/// missing, which is the behaviour the review queue already expects.
fn resolve_media_path(
    media: &AssetMedia,
    representation: Representation,
    roots: &[PathBuf],
) -> Result<PathBuf, ResolveFailure> {
    let candidates: [Option<&PathBuf>; 3] = match representation {
        Representation::Auto => [
            media.proxy.as_ref(),
            media.original.as_ref(),
            media.camera_proxy.as_ref(),
        ],
        Representation::Proxy => [media.proxy.as_ref(), None],
        Representation::CameraProxy => [media.camera_proxy.as_ref(), None],
        Representation::Original => [media.original.as_ref(), None],
    };
    let mut failure = ResolveFailure::NoRepresentation;
    for candidate in candidates.into_iter().flatten() {
        match validated_media_path(candidate, roots) {
            Ok(path) => return Ok(path),
            // The last candidate's reason is the useful one: with `auto` the
            // proxy is tried first and its absence is the ordinary case.
            Err(reason) => failure = reason,
        }
    }
    Err(failure)
}

/// Requirement 4: the catalog says where the file is, and this says whether we
/// believe it. Canonicalization resolves symlinks and `..` before the prefix
/// test, so neither can be used to climb out of a root.
fn validated_media_path(candidate: &Path, roots: &[PathBuf]) -> Result<PathBuf, ResolveFailure> {
    let canonical = candidate
        .canonicalize()
        .map_err(|_| ResolveFailure::Missing)?;
    if !canonical.is_file() {
        return Err(ResolveFailure::NotAFile);
    }
    if !roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .any(|root| canonical.starts_with(&root))
    {
        return Err(ResolveFailure::OutsideRoots);
    }
    Ok(canonical)
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mp4" | "m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("mts" | "m2ts") => "video/mp2t",
        Some("avi") => "video/x-msvideo",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

fn entity_tag(metadata: &std::fs::Metadata) -> String {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |elapsed| elapsed.as_millis());
    format!("\"{:x}-{:x}\"", metadata.len(), modified)
}

/// One range only. A multipart response buys nothing for linear playback and
/// costs a second code path that would need its own tests.
fn parse_range(value: Option<&HeaderValue>, length: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(value) = value else { return Ok(None) };
    let value = value.to_str().map_err(|_| ())?;
    let spec = value.strip_prefix("bytes=").ok_or(())?;
    if spec.contains(',') || length == 0 {
        return Err(());
    }
    let (start, end) = spec.split_once('-').ok_or(())?;
    if start.is_empty() {
        let suffix = end.trim().parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        let selected = suffix.min(length);
        return Ok(Some((length - selected, length - 1)));
    }
    let start = start.trim().parse::<u64>().map_err(|_| ())?;
    if start >= length {
        return Err(());
    }
    let end = if end.trim().is_empty() {
        length - 1
    } else {
        end.trim().parse::<u64>().map_err(|_| ())?.min(length - 1)
    };
    if end < start {
        return Err(());
    }
    Ok(Some((start, end)))
}

/// The dev-only agent seam.
///
/// An agent debugging media has, until now, been able to see a black frame and
/// a 404 and nothing in between. This mounts one extra route on the origin —
/// `GET /debug/media` — that reports what the origin has been asked for, what
/// it resolved, and, on request, exactly why a given asset id would not
/// resolve. It composes with the existing eval bridge rather than adding a
/// second listener: `api.ts` publishes `window.__RCDVLOG_MEDIA_ORIGIN__` when
/// `debugUrl` is set, so
///
/// ```text
/// RCDVLOG_EVAL_BRIDGE=1 RCDVLOG_MEDIA_DEBUG=1 pnpm dev
/// pnpm eval -- 'window.__RCDVLOG_MEDIA_ORIGIN__.debugUrl'
/// curl -s "<that url>&asset=<asset id>" | jq
/// ```
///
/// gets an agent from a screenshot to a reason without guessing a port or a
/// token.
///
/// It is held to the same rules as the eval bridge, and for the same reason —
/// this reports full media paths, which requirement 7 keeps out of ordinary
/// logs:
///
/// - compiled out entirely unless `debug_assertions`,
/// - dormant unless `RCDVLOG_MEDIA_DEBUG` is exactly `1`,
/// - behind the same token, `Host` and `Origin` guard as the media route,
/// - read-only, and bounded to the last [`RECENT_LIMIT`] requests.
///
/// Never turn it into a release diagnostic or a runtime setting.
#[cfg(debug_assertions)]
pub(crate) mod dev {
    use super::*;
    use std::collections::VecDeque;
    use std::ffi::OsStr;

    /// The release-binary test greps for this; keep it in the JSON payload.
    pub(crate) const COMPILED_MARKER: &str = "RCDVLOG_MEDIA_DEBUG_COMPILED";

    /// Enough to cover a stage load plus its two neighbours several times over,
    /// and small enough that leaving the mode on costs nothing.
    const RECENT_LIMIT: usize = 50;

    pub(crate) fn enabled() -> bool {
        std::env::var_os("RCDVLOG_MEDIA_DEBUG").as_deref() == Some(OsStr::new("1"))
    }

    pub(crate) struct Record {
        pub asset_id: String,
        pub representation: &'static str,
        pub method: String,
        pub range: Option<String>,
        pub status: u16,
        pub outcome: &'static str,
        pub resolved_path: Option<PathBuf>,
        pub planned_bytes: u64,
        pub elapsed_ms: f64,
    }

    #[derive(Default)]
    pub(crate) struct Log {
        entries: Mutex<VecDeque<Record>>,
    }

    impl Log {
        pub(crate) fn record(&self, record: Record) {
            let Ok(mut entries) = self.entries.lock() else {
                return;
            };
            if entries.len() == RECENT_LIMIT {
                entries.pop_front();
            }
            entries.push_back(record);
        }

        fn snapshot(&self) -> serde_json::Value {
            let Ok(entries) = self.entries.lock() else {
                return serde_json::json!([]);
            };
            entries
                .iter()
                .map(|record| {
                    serde_json::json!({
                        "assetId": record.asset_id,
                        "representation": record.representation,
                        "method": record.method,
                        "range": record.range,
                        "status": record.status,
                        "outcome": record.outcome,
                        "resolvedPath": record.resolved_path,
                        "plannedBytes": record.planned_bytes,
                        "elapsedMs": (record.elapsed_ms * 100.0).round() / 100.0,
                    })
                })
                .collect()
        }

        fn counters(&self) -> serde_json::Value {
            let Ok(entries) = self.entries.lock() else {
                return serde_json::json!({});
            };
            let mut counts: std::collections::BTreeMap<&'static str, u64> = Default::default();
            for record in entries.iter() {
                *counts.entry(record.outcome).or_default() += 1;
            }
            serde_json::json!(counts)
        }
    }

    /// `GET /debug/media[?asset=<id>[&rep=proxy|original|auto]]`.
    pub(super) async fn diagnostics(
        State(state): State<Arc<OriginState>>,
        headers: HeaderMap,
        Query(query): Query<DebugQuery>,
    ) -> Response {
        if let Err(status) = guard(&state, &headers, query.t.as_deref()) {
            return status.into_response();
        }
        let Some(log) = state.debug.clone() else {
            return StatusCode::NOT_FOUND.into_response();
        };

        let catalog = state.catalog.clone();
        let probe = query.asset.clone().map(|asset_id| {
            let representation = query.rep.unwrap_or_default();
            let located = catalog.locate(&asset_id);
            let roots = catalog.storage_roots();
            let resolved = located
                .as_ref()
                .map_or(Err(ResolveFailure::UnknownAsset), |media| {
                    resolve_media_path(media, representation, &roots)
                });
            serde_json::json!({
                "assetId": asset_id,
                "representation": representation.as_str(),
                "inOpenCatalog": located.is_some(),
                "catalogPaths": {
                    "original": located.as_ref().and_then(|media| media.original.clone()),
                    "proxy": located.as_ref().and_then(|media| media.proxy.clone()),
                },
                "outcome": resolved.as_ref().map_or_else(|failure| failure.as_str(), |_| "ok"),
                "resolvedPath": resolved.ok(),
            })
        });

        axum::Json(serde_json::json!({
            "marker": COMPILED_MARKER,
            "baseUrl": format!("http://{}", state.authority),
            "uptimeSeconds": (state.started.elapsed().as_secs_f64() * 100.0).round() / 100.0,
            "storageRoots": catalog.storage_roots(),
            "outcomes": log.counters(),
            "recent": log.snapshot(),
            "probe": probe,
        }))
        .into_response()
    }

    #[derive(Debug, Default, Deserialize)]
    pub(super) struct DebugQuery {
        #[serde(default)]
        pub t: Option<String>,
        #[serde(default)]
        pub asset: Option<String>,
        #[serde(default)]
        pub rep: Option<Representation>,
    }
}

fn generate_token(byte_length: usize) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use rand::RngCore;
    let mut bytes = vec![0u8; byte_length];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(&bytes)
}

#[cfg(test)]
mod tests {
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

        let response =
            reqwest::get(server.media_url("asset_1", &format!("rep=original&t={token}")))
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

        let probed: serde_json::Value =
            reqwest::get(format!("{debug_url}&asset=asset_1&rep=original"))
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
}
