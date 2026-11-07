//! Shared HTTP state: `HttpState`, readiness gate, auth rate limiter, WebSocket channels, request ids and error bodies.

use super::*;

pub const REQUEST_ID_HEADER: &str = "x-request-id";
pub(super) const MAX_REQUEST_ID_BYTES: usize = 64;
pub(super) const DEFAULT_READINESS_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const MAX_HTTP_BODY_BYTES: usize = 64 * 1024 * 1024;
pub(super) const MAX_AUTH_BODY_BYTES: usize = 64 * 1024;
pub(super) const AUTH_ATTEMPT_LIMIT: usize = 12;
pub(super) const AUTH_ATTEMPT_WINDOW: Duration = Duration::from_secs(60);

pub(super) type ReadinessEvaluator = dyn Fn() -> ReadinessReport + Send + Sync + 'static;

#[derive(Clone)]
pub struct HttpState {
    pub(super) foundation: Arc<RuntimeFoundation>,
    pub(super) auth: Arc<AuthService>,
    pub(super) readiness_gate: ReadinessGate,
    pub(super) ws_channels: Arc<WsChannels>,
    pub(super) auth_rate_limiter: Arc<AuthRateLimiter>,
    pub(super) web_root: Option<Arc<PathBuf>>,
}

#[derive(Clone)]
pub(super) struct ReadinessGate {
    pub(super) evaluator: Arc<ReadinessEvaluator>,
    pub(super) single_flight: Arc<Semaphore>,
    pub(super) response_timeout: Duration,
}

pub(super) struct AuthRateLimiter {
    pub(super) attempts: StdMutex<VecDeque<Instant>>,
    pub(super) maximum_attempts: usize,
    pub(super) window: Duration,
}

impl AuthRateLimiter {
    pub(super) fn new(maximum_attempts: usize, window: Duration) -> Self {
        Self {
            attempts: StdMutex::new(VecDeque::new()),
            maximum_attempts,
            window,
        }
    }

    pub(super) fn allow(&self, now: Instant) -> bool {
        let mut attempts = self.attempts.lock().unwrap();
        while attempts
            .front()
            .is_some_and(|attempt| now.duration_since(*attempt) >= self.window)
        {
            attempts.pop_front();
        }
        if attempts.len() >= self.maximum_attempts {
            return false;
        }
        attempts.push_back(now);
        true
    }
}

// ── WebSocket hint broadcast ──────────────────────────────────────────

/// Per-project broadcast channels for sync hints.
/// Tokio broadcast drops slow consumers — acceptable since hints are
/// ephemeral; every reconnect triggers a full HTTP cursor reconcile.
#[derive(Clone)]
pub(super) struct WsChannels {
    pub(super) channels: Arc<StdRwLock<HashMap<String, tokio::sync::broadcast::Sender<String>>>>,
}

impl WsChannels {
    pub(super) fn new() -> Self {
        Self {
            channels: Arc::new(StdRwLock::new(HashMap::new())),
        }
    }

    pub(super) fn subscribe(&self, project_id: &str) -> tokio::sync::broadcast::Receiver<String> {
        let mut map = self.channels.write().unwrap();
        map.entry(project_id.to_string())
            .or_insert_with(|| tokio::sync::broadcast::channel::<String>(16).0)
            .subscribe()
    }

    pub(super) fn broadcast(&self, project_id: &str, hint: &str) {
        if let Ok(map) = self.channels.read()
            && let Some(tx) = map.get(project_id)
        {
            let _ = tx.send(hint.to_string());
        }
    }
}

impl HttpState {
    pub fn new(foundation: Arc<RuntimeFoundation>) -> Self {
        let readiness_foundation = foundation.clone();
        let auth = Arc::new(foundation.auth().clone());
        Self::with_readiness_evaluator(foundation, auth, DEFAULT_READINESS_TIMEOUT, move || {
            readiness_foundation.readiness()
        })
    }

    pub(super) fn with_readiness_evaluator(
        foundation: Arc<RuntimeFoundation>,
        auth: Arc<AuthService>,
        response_timeout: Duration,
        evaluator: impl Fn() -> ReadinessReport + Send + Sync + 'static,
    ) -> Self {
        Self {
            foundation,
            auth,
            readiness_gate: ReadinessGate {
                evaluator: Arc::new(evaluator),
                single_flight: Arc::new(Semaphore::new(1)),
                response_timeout,
            },
            ws_channels: Arc::new(WsChannels::new()),
            auth_rate_limiter: Arc::new(AuthRateLimiter::new(
                AUTH_ATTEMPT_LIMIT,
                AUTH_ATTEMPT_WINDOW,
            )),
            web_root: std::env::var_os("RCDVLOG_WEB_DIR")
                .map(PathBuf::from)
                .map(Arc::new),
        }
    }

    pub fn with_web_root(mut self, web_root: impl Into<PathBuf>) -> Self {
        self.web_root = Some(Arc::new(web_root.into()));
        self
    }

    #[cfg(test)]
    pub(super) fn with_auth_rate_limit(
        mut self,
        maximum_attempts: usize,
        window: Duration,
    ) -> Self {
        self.auth_rate_limiter = Arc::new(AuthRateLimiter::new(maximum_attempts, window));
        self
    }
}

impl ReadinessGate {
    pub(super) async fn evaluate(&self) -> ReadinessReport {
        let permit = match self.single_flight.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                return ReadinessReport::evaluation_unavailable("readiness_check_busy");
            }
        };
        let evaluator = self.evaluator.clone();
        let evaluation = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            evaluator()
        });
        match timeout(self.response_timeout, evaluation).await {
            Ok(Ok(report)) => report,
            Ok(Err(_)) => ReadinessReport::evaluation_unavailable("readiness_check_failed"),
            Err(_) => ReadinessReport::evaluation_unavailable("readiness_check_timeout"),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct RequestId(pub(super) String);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ErrorResponse {
    pub(super) request_id: String,
    pub(super) code: &'static str,
}

// ── Auth DTOs ──────────────────────────────────────────────────────
