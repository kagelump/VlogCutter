//! Explicitly opted-in JavaScript evaluation for a running development webview.
//!
//! This entire module is included from `lib.rs` only when debug assertions are
//! enabled. Keep the marker below in the listener log: the release-binary test
//! uses it to prove that this code was not linked into a shipped binary.

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::ffi::OsStr;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener as StdTcpListener};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{Manager, Runtime};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::http::{StatusCode, header::ORIGIN};
use tokio_tungstenite::tungstenite::protocol::Message;

pub(crate) const LISTEN_PORT: u16 = 47_961;
pub(crate) const COMPILED_MARKER: &str = "RCDVLOG_EVAL_BRIDGE_COMPILED";
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

pub(crate) fn start_if_enabled<R: Runtime>(app: &tauri::App<R>) -> anyhow::Result<()> {
    if !enabled(std::env::var_os("RCDVLOG_EVAL_BRIDGE").as_deref()) {
        return Ok(());
    }

    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, LISTEN_PORT);
    let listener = StdTcpListener::bind(address)?;
    listener.set_nonblocking(true)?;
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| anyhow::anyhow!("main webview is unavailable"))?;
    let evaluator = Arc::new(WebviewEvaluator { window });

    println!("[{COMPILED_MARKER}] listening on ws://{address}");
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

fn enabled(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new("1"))
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
