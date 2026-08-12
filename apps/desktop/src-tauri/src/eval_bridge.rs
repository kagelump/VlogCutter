//! Explicitly opted-in JavaScript evaluation for a running development webview.
//!
//! This entire module is included from `lib.rs` only when debug assertions are
//! enabled. Keep the marker below in the listener log: the release-binary test
//! uses it to prove that this code was not linked into a shipped binary.

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::ffi::OsStr;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener as StdTcpListener};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{Manager, Runtime};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::http::{StatusCode, header::ORIGIN};
use tokio_tungstenite::tungstenite::protocol::Message;

pub(crate) const COMPILED_MARKER: &str = "RCDVLOG_EVAL_BRIDGE_COMPILED";
/// Pins the listener to one port instead of letting the kernel choose. For
/// tooling that cannot read the app's stdout and cannot read the address file.
const PORT_ENV: &str = "RCDVLOG_EVAL_BRIDGE_PORT";
/// Redirects the published address file. A concurrent harness sets this to a
/// path inside its own run directory so two runs never read each other's port.
const ADDRESS_FILE_ENV: &str = "RCDVLOG_EVAL_BRIDGE_ADDRESS_FILE";
/// Default address-file name, resolved inside the platform temp directory.
const DEFAULT_ADDRESS_FILE: &str = "rcdvlog-eval-bridge.json";
const RESULT_TIMEOUT: Duration = Duration::from_secs(30);

type EvalCallback = Box<dyn Fn(String) + Send>;

trait ScriptEvaluator: Send + Sync + 'static {
    fn evaluate(&self, script: &str, callback: EvalCallback) -> Result<(), String>;
}

struct WebviewEvaluator<R: Runtime> {
    window: tauri::WebviewWindow<R>,
}

impl<R: Runtime> ScriptEvaluator for WebviewEvaluator<R> {
    fn evaluate(&self, script: &str, callback: EvalCallback) -> Result<(), String> {
        self.window
            .eval_with_callback(wrapped_script(script), callback)
            .map_err(|error| error.to_string())
    }
}

/// Bind the loopback bridge, publish where it landed, and start serving.
///
/// The port is ephemeral unless [`PORT_ENV`] pins it, so a leaked previous
/// instance cannot take the address this one needs. Failure is returned rather
/// than acted on here; the caller logs and keeps launching, because a dev
/// convenience must never be able to stop the app from starting.
pub(crate) fn start_if_enabled<R: Runtime>(app: &tauri::App<R>) -> anyhow::Result<()> {
    if !enabled(std::env::var_os("RCDVLOG_EVAL_BRIDGE").as_deref()) {
        return Ok(());
    }

    let requested = requested_port(std::env::var_os(PORT_ENV).as_deref())?;
    let listener = StdTcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, requested))
        .with_context(|| format!("bind the eval bridge on 127.0.0.1:{requested}"))?;
    listener.set_nonblocking(true)?;
    let address = listener
        .local_addr()
        .context("read the eval bridge port back")?;
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| anyhow::anyhow!("main webview is unavailable"))?;
    let evaluator = Arc::new(WebviewEvaluator { window });

    // Two channels, because the two consumers differ: a harness that spawned
    // the app reads this line off stdout, and a shell client that did not
    // reads the file below. Both carry the same address; neither guesses.
    println!("{}", marker_line(&address));
    let path = address_file_path();
    if let Err(error) = publish_address(&path, &address) {
        eprintln!(
            "[eval-bridge] could not publish {}: {error:#} — pass --port {} to the client",
            path.display(),
            address.port()
        );
    }

    tauri::async_runtime::spawn(async move {
        let listener = match TcpListener::from_std(listener) {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("[eval-bridge] listener failed: {error}");
                return;
            }
        };
        serve(listener, evaluator).await;
    });
    Ok(())
}

/// Remove this process's address file on the way out, so the next client sees
/// "not running" rather than a port that now belongs to something else.
pub(crate) fn stop() {
    let path = address_file_path();
    match std::fs::read_to_string(&path) {
        // Only ever retract our own advertisement.
        Ok(contents) if published_pid(&contents) != Some(std::process::id()) => {}
        Ok(_) => {
            if let Err(error) = std::fs::remove_file(&path) {
                eprintln!("[eval-bridge] could not remove {}: {error}", path.display());
            }
        }
        Err(_) => {}
    }
}

fn enabled(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new("1"))
}

/// `0` — an ephemeral port — unless the override names one. A malformed
/// override is an error rather than a silent fall back to `0`: a listener that
/// comes up somewhere its consumer is not looking is the failure this module
/// is being changed to avoid.
fn requested_port(value: Option<&OsStr>) -> anyhow::Result<u16> {
    let Some(value) = value else {
        return Ok(0);
    };
    let text = value
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("{PORT_ENV} is not valid UTF-8"))?;
    text.parse::<u16>()
        .with_context(|| format!("{PORT_ENV} must be a port number, got {text:?}"))
}

/// The one line a spawning harness parses. Keep the shape stable: the release
/// binary test greps for the marker, and the harness greps for this whole line.
fn marker_line(address: &SocketAddr) -> String {
    format!("[{COMPILED_MARKER}] listening on ws://{address}")
}

fn address_file_path() -> PathBuf {
    match std::env::var_os(ADDRESS_FILE_ENV) {
        Some(path) => PathBuf::from(path),
        None => std::env::temp_dir().join(DEFAULT_ADDRESS_FILE),
    }
}

/// The client checks `pid` for liveness before trusting `address`, which is
/// what makes a file left behind by a killed app legible as stale.
fn address_document(address: &SocketAddr, pid: u32) -> String {
    json!({
        "address": format!("ws://{address}"),
        "port": address.port(),
        "pid": pid,
    })
    .to_string()
}

fn publish_address(path: &Path, address: &SocketAddr) -> anyhow::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(path, address_document(address, std::process::id()))
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn published_pid(contents: &str) -> Option<u32> {
    serde_json::from_str::<serde_json::Value>(contents)
        .ok()?
        .get("pid")?
        .as_u64()?
        .try_into()
        .ok()
}

async fn serve<E: ScriptEvaluator>(listener: TcpListener, evaluator: Arc<E>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let evaluator = evaluator.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = serve_connection(stream, evaluator).await {
                        eprintln!("[eval-bridge] connection failed: {error}");
                    }
                });
            }
            Err(error) => eprintln!("[eval-bridge] accept failed: {error}"),
        }
    }
}

// The handshake callback's error type is `tungstenite`'s, not ours: the
// rejection must be a whole HTTP response, and there is nothing here to box.
#[allow(clippy::result_large_err)]
async fn serve_connection<E: ScriptEvaluator>(
    stream: TcpStream,
    evaluator: Arc<E>,
) -> anyhow::Result<()> {
    let mut socket =
        tokio_tungstenite::accept_hdr_async(stream, |request: &Request, response: Response| {
            if request.headers().contains_key(ORIGIN) {
                return Err(Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .body(Some("browser origins are not allowed".into()))
                    .expect("valid rejection response"));
            }
            Ok(response)
        })
        .await?;

    while let Some(message) = socket.next().await {
        match message? {
            Message::Text(script) => {
                println!("[eval-bridge] evaluating:\n{script}");
                let (sender, receiver) = tokio::sync::oneshot::channel();
                let sender = Arc::new(Mutex::new(Some(sender)));
                let response = match evaluator.evaluate(
                    &script,
                    Box::new(move |result| {
                        if let Some(sender) =
                            sender.lock().ok().and_then(|mut sender| sender.take())
                        {
                            let _ = sender.send(decode_callback_result(result));
                        }
                    }),
                ) {
                    Ok(()) => match timeout(RESULT_TIMEOUT, receiver).await {
                        Ok(Ok(result)) => result,
                        Ok(Err(_)) => structured_error(
                            "EvalBridgeError",
                            "webview evaluation callback was dropped",
                        ),
                        Err(_) => structured_error(
                            "TimeoutError",
                            "webview evaluation did not finish within 30 seconds",
                        ),
                    },
                    Err(error) => structured_error("EvalBridgeError", &error),
                };
                socket.send(Message::Text(response.into())).await?;
            }
            Message::Binary(_) => {
                socket
                    .send(Message::Text(
                        structured_error("TypeError", "expected a UTF-8 text script").into(),
                    ))
                    .await?;
            }
            Message::Ping(payload) => socket.send(Message::Pong(payload)).await?,
            Message::Close(_) => break,
            Message::Pong(_) | Message::Frame(_) => {}
        }
    }
    Ok(())
}

fn wrapped_script(script: &str) -> String {
    let source = serde_json::to_string(script).expect("JavaScript source is serializable");
    format!(
        r#"(() => {{
  try {{
    const value = (0, eval)({source});
    const json = JSON.stringify(value);
    return json === undefined ? "null" : json;
  }} catch (error) {{
    return JSON.stringify({{
      error: {{
        name: error && error.name ? String(error.name) : "Error",
        message: error && error.message ? String(error.message) : String(error),
        stack: error && error.stack ? String(error.stack) : null
      }}
    }});
  }}
}})()"#
    )
}

fn decode_callback_result(result: String) -> String {
    serde_json::from_str::<String>(&result).unwrap_or_else(|error| {
        structured_error(
            "EvalBridgeError",
            &format!("could not decode webview evaluation result: {error}"),
        )
    })
}

fn structured_error(name: &str, message: &str) -> String {
    json!({
        "error": {
            "name": name,
            "message": message,
            "stack": null,
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    struct EchoEvaluator;

    impl ScriptEvaluator for EchoEvaluator {
        fn evaluate(&self, script: &str, callback: EvalCallback) -> Result<(), String> {
            let response = if script == "throw" {
                structured_error("Error", "broken")
            } else {
                script.to_string()
            };
            callback(serde_json::to_string(&response).unwrap());
            Ok(())
        }
    }

    #[test]
    fn opt_in_requires_exact_value() {
        assert!(!enabled(None));
        assert!(!enabled(Some(OsStr::new("true"))));
        assert!(enabled(Some(OsStr::new("1"))));
    }

    #[test]
    fn port_is_ephemeral_unless_the_override_names_one() {
        assert_eq!(requested_port(None).unwrap(), 0);
        assert_eq!(requested_port(Some(OsStr::new("47961"))).unwrap(), 47_961);

        // A typo must not quietly become "wherever the kernel put it".
        for bad in ["", "0x1", "70000", "47961 "] {
            let error = requested_port(Some(OsStr::new(bad)))
                .expect_err(&format!("{bad:?} is not a port"))
                .to_string();
            assert!(error.contains(PORT_ENV), "{error}");
        }
    }

    #[test]
    fn marker_line_is_parseable_by_a_spawning_harness() {
        let address: SocketAddr = "127.0.0.1:53125".parse().unwrap();
        let line = marker_line(&address);
        assert_eq!(
            line,
            "[RCDVLOG_EVAL_BRIDGE_COMPILED] listening on ws://127.0.0.1:53125"
        );

        // The harness contract, applied to the line the app actually prints.
        let parsed = line
            .split_once(&format!("[{COMPILED_MARKER}] listening on "))
            .map(|(_, address)| address)
            .unwrap();
        assert_eq!(parsed, "ws://127.0.0.1:53125");
    }

    #[test]
    fn published_address_names_the_port_and_the_owning_process() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested").join("bridge.json");
        let address: SocketAddr = "127.0.0.1:53125".parse().unwrap();

        publish_address(&path, &address).unwrap();
        let published: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(published["address"], "ws://127.0.0.1:53125");
        assert_eq!(published["port"], 53_125);
        assert_eq!(published["pid"], std::process::id());
        assert_eq!(published_pid(&address_document(&address, 4242)), Some(4242));
        assert_eq!(published_pid("not json"), None);
    }

    #[tokio::test]
    async fn ephemeral_bind_survives_a_held_port() {
        // The collision that used to abort the app at launch: something else
        // already owns the port a previous run used.
        let squatter = StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let held = squatter.local_addr().unwrap().port();

        assert!(
            StdTcpListener::bind((Ipv4Addr::LOCALHOST, held)).is_err(),
            "the fixed-port path still fails, which is why it is no longer the default"
        );
        let listener = StdTcpListener::bind(SocketAddrV4::new(
            Ipv4Addr::LOCALHOST,
            requested_port(None).unwrap(),
        ))
        .expect("an ephemeral bind cannot collide");
        assert_ne!(listener.local_addr().unwrap().port(), held);
    }

    #[test]
    fn wrapper_serializes_values_and_structured_errors() {
        let wrapper = wrapped_script("throw new Error('broken')");
        assert!(wrapper.contains("JSON.stringify(value)"));
        assert!(wrapper.contains("name:"));
        assert!(wrapper.contains("message:"));
        assert!(wrapper.contains("stack:"));
        assert!(wrapper.contains("throw new Error('broken')"));

        assert_eq!(decode_callback_result("\"42\"".into()), "42");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&structured_error("TypeError", "bad"))
                .unwrap(),
            json!({"error": {"name": "TypeError", "message": "bad", "stack": null}})
        );
    }

    #[tokio::test]
    async fn round_trip_keeps_connection_open() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve(listener, Arc::new(EchoEvaluator)));
        let (mut socket, _) = connect_async(format!("ws://{address}")).await.unwrap();

        socket.send(Message::Text("throw".into())).await.unwrap();
        let error = socket.next().await.unwrap().unwrap().into_text().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&error).unwrap(),
            json!({"error": {"name": "Error", "message": "broken", "stack": null}})
        );
        socket.send(Message::Text("second".into())).await.unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap().into_text().unwrap(),
            "second"
        );

        server.abort();
    }

    #[tokio::test]
    async fn rejects_foreign_origin() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve(listener, Arc::new(EchoEvaluator)));
        let mut request = format!("ws://{address}").into_client_request().unwrap();
        request
            .headers_mut()
            .insert(ORIGIN, "https://attacker.example".parse().unwrap());

        let error = connect_async(request).await.unwrap_err();
        assert!(error.to_string().contains("403"));
        server.abort();
    }
}
