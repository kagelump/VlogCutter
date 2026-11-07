//! A loopback HTTP origin that serves review media to the webview.
//!
//! `WKWebView` does not produce video frames for media delivered through a
//! `WKURLSchemeHandler` custom scheme, which is what Tauri's `asset://` is on
//! macOS: an element that has never been played reports `readyState 4`, correct
//! dimensions and no error, and holds nothing paintable. That is a property of
//! the transport, so the fix is a different transport rather than a workaround
//! at the call site. See `4c5eb2f:docs/tasks/desktop/MEDIA_DELIVERY.md`.
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
        Representation::Proxy => [media.proxy.as_ref(), None, None],
        Representation::CameraProxy => [media.camera_proxy.as_ref(), None, None],
        Representation::Original => [media.original.as_ref(), None, None],
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
#[path = "media_server_tests.rs"]
mod tests;
