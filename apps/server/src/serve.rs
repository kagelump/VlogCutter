//! Listener binding and graceful shutdown.

use super::*;

#[derive(Clone)]
pub struct ShutdownCoordinator {
    pub(super) token: CancellationToken,
}

impl Default for ShutdownCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl ShutdownCoordinator {
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
        }
    }

    pub fn cancel(&self) {
        self.token.cancel();
    }

    pub fn child_token(&self) -> CancellationToken {
        self.token.child_token()
    }

    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }
}

// ── Auth middleware ──────────────────────────────────────────────────

pub async fn bind_loopback(address: SocketAddr) -> Result<TcpListener> {
    if !address.ip().is_loopback() {
        return Err(anyhow!(
            "internal HTTP listener must use a loopback address"
        ));
    }
    TcpListener::bind(address)
        .await
        .with_context(|| format!("bind private server listener at {address}"))
}

pub async fn serve_until_shutdown(
    listener: TcpListener,
    app: Router,
    shutdown: ShutdownCoordinator,
    shutdown_timeout: Duration,
) -> Result<()> {
    let graceful_shutdown = shutdown.clone();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            graceful_shutdown.cancelled().await;
        })
        .into_future();
    tokio::pin!(server);

    tokio::select! {
        biased;
        _ = shutdown.cancelled() => {
            timeout(shutdown_timeout, &mut server)
                .await
                .map_err(|_| anyhow!("graceful shutdown exceeded {} seconds", shutdown_timeout.as_secs()))??;
        }
        result = &mut server => {
            result.context("internal HTTP server stopped unexpectedly")?;
        }
    }
    Ok(())
}
