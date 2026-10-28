//! One fixed endpoint, one POST, no redirects, retries or tier fallback.
//! Only the accounted native leaf may use this private transport.
use super::{
    NativeVideoOptions,
    protocol::{self, CallResult, PreparedRequest},
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub(crate) trait Transport: Send + Sync {
    fn prepare(&self) -> Result<()>;
    fn transmit(
        &self,
        request: &PreparedRequest,
        options: &NativeVideoOptions,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<CallResult>;
}

#[derive(Default)]
pub(crate) struct DirectTransport {
    ready: Mutex<Option<ReadyClient>>,
}
#[derive(Clone)]
struct ReadyClient {
    client: reqwest::Client,
    key: reqwest::header::HeaderValue,
    runtime: Arc<tokio::runtime::Runtime>,
}
impl DirectTransport {
    fn transmit_to(
        &self,
        endpoint: &str,
        request: &PreparedRequest,
        options: &NativeVideoOptions,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<CallResult> {
        let ReadyClient {
            client,
            key,
            runtime,
        } = self
            .ready
            .lock()
            .map_err(|_| anyhow::anyhow!("native provider preparation lock failed"))?
            .clone()
            .context("native transport was not prepared")?;
        ensure!(
            request.body.len() <= protocol::MAX_BODY_BYTES,
            "native body exceeds transport cap"
        );
        ensure!(
            !cancelled(),
            "native request cancelled after dispatch marker; upload status is uncertain"
        );
        let started = Instant::now();
        // The runtime lives with this transport, so pooled connections survive
        // individual calls and concurrent workers share the same I/O owner.
        runtime.block_on(async {
            let exchange=async {
                let mut response=client.post(endpoint).header("x-goog-api-key",key)
                    .header(reqwest::header::CONTENT_TYPE,"application/json").body(request.body.clone()).send().await
                    .map_err(|e|anyhow::anyhow!(if e.is_connect(){"native_connect_failed_after_dispatch_marker"}else if e.is_timeout(){"native_timeout_after_dispatch_marker"}else{"native_http_failed_after_dispatch_marker"}))?;
                let status=response.status().as_u16();
                let tier=response.headers().get("x-gemini-service-tier").map(|h|h.to_str().map(str::to_owned).unwrap_or_else(|_|"invalid-non-ascii-service-tier".into()));
                if response.content_length().is_some_and(|n|n>protocol::MAX_RESPONSE_BYTES as u64) { bail!("native response exceeds transport cap after dispatch"); }
                let mut raw=Vec::new();
                while let Some(chunk)=response.chunk().await.map_err(|_|anyhow::anyhow!("native response interrupted after dispatch"))? {
                    ensure!(raw.len()+chunk.len()<=protocol::MAX_RESPONSE_BYTES,"native response exceeds transport cap after dispatch");
                    raw.extend_from_slice(&chunk);
                }
                protocol::decode_response(status,raw,tier.as_deref(),u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),options)
            };
            tokio::pin!(exchange);
            let mut observation=tokio::time::interval(Duration::from_millis(50));
            loop {
                tokio::select! {
                    biased;
                    result=&mut exchange => return result,
                    _=observation.tick() => if cancelled() { bail!("native request cancelled after dispatch marker; upload status is uncertain"); }
                }
            }
        })
    }
}
impl Transport for DirectTransport {
    fn prepare(&self) -> Result<()> {
        let mut ready = self
            .ready
            .lock()
            .map_err(|_| anyhow::anyhow!("native provider preparation lock failed"))?;
        if ready.is_some() {
            return Ok(());
        }
        ensure!(
            tokio::runtime::Handle::try_current().is_err(),
            "native HTTP preparation requires a blocking worker"
        );
        let key = std::env::var("GEMINI_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .context("native provider credential unavailable")?;
        *ready = Some(make_client(&key, true)?);
        Ok(())
    }
    fn transmit(
        &self,
        request: &PreparedRequest,
        options: &NativeVideoOptions,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<CallResult> {
        self.transmit_to(protocol::ENDPOINT, request, options, cancelled)
    }
}

fn make_client(key: &str, https_only: bool) -> Result<ReadyClient> {
    let mut header = reqwest::header::HeaderValue::from_str(key)
        .map_err(|_| anyhow::anyhow!("native provider credential format invalid"))?;
    header.set_sensitive(true);
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .context("native HTTP runtime unavailable")?,
    );
    let _entered = runtime.enter();
    let client = reqwest::Client::builder()
        .https_only(https_only)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|_| anyhow::anyhow!("native provider HTTP configuration failed"))?;
    drop(_entered);
    Ok(ReadyClient {
        client,
        key: header,
        runtime,
    })
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
