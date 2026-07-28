use crate::server_client::{ClientError, ServerClient};
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use rcdvlog_core::Catalog;
use rcdvlog_sync::{
    DEFAULT_MAX_OPERATIONS, ProtocolRange, ReconcileRequest, ReconcileResponse,
    apply_reconcile_operations, exact_sequence_recovery_page, sequence_recovery_requires_attention,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::RwLock;
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMsg;

// ── Wake events ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum WakeEvent {
    ProjectMutated { project_id: String },
    SyncNow { project_id: String },
    PeriodicTick,
    Shutdown,
}

// ── Sync state ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncState {
    Unpaired,
    Offline,
    Connecting,
    Syncing,
    Synced,
    Attention,
    Incompatible,
    CredentialRevoked,
}

impl SyncState {
    #[allow(dead_code)]
    fn label(self) -> &'static str {
        match self {
            Self::Unpaired => "Unpaired",
            Self::Offline => "Offline",
            Self::Connecting => "Connecting",
            Self::Syncing => "Syncing",
            Self::Synced => "Synced",
            Self::Attention => "Needs attention",
            Self::Incompatible => "Incompatible",
            Self::CredentialRevoked => "Credential revoked",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub project_id: String,
    pub state: SyncState,
    pub last_sync_at: Option<String>,
    pub last_error: Option<String>,
    pub local_pending_operations: u64,
    pub remote_pending_operations: u64,
    pub conflict_count: usize,
    pub conflict_details: Vec<ConflictDetail>,
    pub active_endpoint: Option<String>,
    pub backoff_until: Option<String>,
}

impl SyncStatus {
    pub fn unpaired(project_id: String) -> Self {
        Self {
            project_id,
            state: SyncState::Unpaired,
            last_sync_at: None,
            last_error: None,
            local_pending_operations: 0,
            remote_pending_operations: 0,
            conflict_count: 0,
            conflict_details: vec![],
            active_endpoint: None,
            backoff_until: None,
        }
    }

    fn with_state(project_id: String, state: SyncState) -> Self {
        Self {
            project_id,
            state,
            last_sync_at: None,
            last_error: None,
            local_pending_operations: 0,
            remote_pending_operations: 0,
            conflict_count: 0,
            conflict_details: vec![],
            active_endpoint: None,
            backoff_until: None,
        }
    }
}

// ── Conflict detail (surfaced in sync diagnostics) ───────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictDetail {
    pub conflict_id: String,
    pub conflict_type: String,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub incoming_operation_summary: Option<OperationSummary>,
    pub active_operation_summary: Option<OperationSummary>,
    pub expected_base_revision: Option<u64>,
    pub actual_revision: u64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationSummary {
    pub operation_id: String,
    pub operation_type: String,
    pub origin_node_id: String,
    pub origin_sequence: u64,
    /// Clock payload fields (present for clock event conflicts)
    pub camera: Option<String>,
    pub start_filename: Option<String>,
    pub offset_seconds: Option<i64>,
    pub label: Option<String>,
}

// ── Per-project backoff state (not serialized, internal only) ────────

#[derive(Debug, Clone, Default)]
struct Backoff {
    consecutive_failures: u64,
    next_retry_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Backoff {
    const BASE_MS: u64 = 1_000;
    const MAX_MS: u64 = 300_000; // 5 minutes
    const JITTER_FRACTION: f64 = 0.25;

    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.next_retry_at = None;
    }

    fn record_failure(&mut self) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let delay_ms = Self::BASE_MS
            .saturating_mul(
                2_u64.saturating_pow(self.consecutive_failures.saturating_sub(1) as u32),
            )
            .min(Self::MAX_MS);
        let jitter_range = ((delay_ms as f64) * Self::JITTER_FRACTION) as i64;
        let jitter_offset = rand::thread_rng().gen_range(-jitter_range..=jitter_range);
        let jittered = delay_ms as i64 + jitter_offset;
        self.next_retry_at = Some(chrono::Utc::now() + chrono::Duration::milliseconds(jittered));
    }

    fn can_retry(&self) -> bool {
        match self.next_retry_at {
            Some(until) => chrono::Utc::now() >= until,
            None => true,
        }
    }

    fn backoff_until(&self) -> Option<String> {
        self.next_retry_at
            .map(|until| until.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
    }
}

// ── Sync manager ──────────────────────────────────────────────────────

pub struct SyncManager {
    wake_tx: tokio::sync::mpsc::UnboundedSender<WakeEvent>,
}

impl SyncManager {
    /// Spawn the background sync loop. The returned `SyncManager` is used to
    /// signal the loop from Tauri commands.
    pub fn spawn(
        app_handle: AppHandle,
        catalog: Arc<RwLock<Catalog>>,
        settings: Arc<RwLock<crate::DesktopSettings>>,
        status: Arc<RwLock<HashMap<String, SyncStatus>>>,
    ) -> Self {
        let catalog_path = catalog
            .read()
            .ok()
            .map(|catalog| catalog.database_path().to_path_buf());
        let (wake_tx, wake_rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = SyncLoopHandle {
            app_handle,
            catalog,
            settings,
            status,
            backoffs: HashMap::new(),
            active_projects: HashSet::new(),
            wake_tx: wake_tx.clone(),
        };
        // `spawn` is called from Tauri's setup hook, which runs on the main
        // thread outside any Tokio runtime context — `tokio::spawn` would panic
        // there. `tauri::async_runtime::spawn` targets Tauri's global runtime.
        tauri::async_runtime::spawn(async move {
            let Some(catalog_path) = catalog_path else {
                return;
            };
            loop {
                match rcdvlog_sync::SyncProcessGuard::try_acquire(&catalog_path) {
                    Ok(Some(guard)) => {
                        let _guard = guard;
                        handle.run(wake_rx).await;
                        return;
                    }
                    Ok(None) | Err(_) => {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await
                    }
                }
            }
        });
        Self { wake_tx }
    }

    /// Signal that a project was mutated locally and needs sync.
    pub fn wake_project(&self, project_id: &str) {
        let _ = self.wake_tx.send(WakeEvent::ProjectMutated {
            project_id: project_id.to_string(),
        });
    }

    /// Signal an explicit "sync now" request.
    pub fn sync_now(&self, project_id: &str) {
        let _ = self.wake_tx.send(WakeEvent::SyncNow {
            project_id: project_id.to_string(),
        });
    }

    /// Create a no-op sync manager for tests (never spawns a background task).
    #[cfg(test)]
    pub fn spawn_test() -> Self {
        let (wake_tx, _wake_rx) = tokio::sync::mpsc::unbounded_channel();
        // The receiver is dropped immediately, so any wake events are no-ops.
        Self { wake_tx }
    }
}

impl Drop for SyncManager {
    fn drop(&mut self) {
        let _ = self.wake_tx.send(WakeEvent::Shutdown);
    }
}

// ── Loop state (lives on the spawned task) ───────────────────────────

struct SyncLoopHandle {
    app_handle: AppHandle,
    catalog: Arc<RwLock<Catalog>>,
    settings: Arc<RwLock<crate::DesktopSettings>>,
    status: Arc<RwLock<HashMap<String, SyncStatus>>>,
    backoffs: HashMap<String, Backoff>,
    active_projects: HashSet<String>,
    wake_tx: tokio::sync::mpsc::UnboundedSender<WakeEvent>,
}

impl SyncLoopHandle {
    const PERIODIC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

    async fn run(mut self, mut wake_rx: tokio::sync::mpsc::UnboundedReceiver<WakeEvent>) {
        let mut ws_task: Option<tokio::task::JoinHandle<()>> = None;
        loop {
            let event = tokio::time::timeout(Self::PERIODIC_INTERVAL, wake_rx.recv()).await;

            let mut targeted_project: Option<String> = None;

            match event {
                Ok(Some(WakeEvent::Shutdown)) => break,
                Ok(Some(WakeEvent::ProjectMutated { project_id })) => {
                    targeted_project = Some(project_id);
                }
                Ok(Some(WakeEvent::SyncNow { project_id })) => {
                    // Reset backoff for forced sync
                    if let Some(backoff) = self.backoffs.get_mut(&project_id) {
                        backoff.record_success();
                    }
                    targeted_project = Some(project_id);
                }
                Ok(Some(WakeEvent::PeriodicTick)) => {
                    // Periodic tick — fall through to reconcile all
                }
                Ok(None) => {
                    // Channel closed
                    break;
                }
                Err(_) => {
                    // The cross-process CLI wake path is intentionally polled at the
                    // existing bounded interval, so it never creates a second loop.
                    let requests = self
                        .catalog
                        .read()
                        .ok()
                        .and_then(|catalog| {
                            rcdvlog_sync::drain_sync_manager_requests(catalog.database_path()).ok()
                        })
                        .unwrap_or_default();
                    if let Some(project_id) = requests.first() {
                        targeted_project = Some(project_id.clone());
                        for project_id in requests.iter().skip(1) {
                            let _ = self.wake_tx.send(WakeEvent::SyncNow {
                                project_id: project_id.clone(),
                            });
                        }
                    }
                    // With no request this remains the normal periodic reconcile-all tick.
                }
            }

            let paired_projects = self.list_paired_projects();
            if paired_projects.is_empty() {
                // Still ensure WS is cleaned up if no paired projects
                ws_task = None;
                continue;
            }

            if let Some(ref project_id) = targeted_project {
                if paired_projects.contains(project_id) {
                    self.reconcile_project(project_id).await;
                }
            } else {
                for project_id in &paired_projects {
                    if self.should_attempt(project_id) {
                        self.reconcile_project(project_id).await;
                    }
                }
            }

            // Ensure WebSocket hint listener is running
            self.ensure_ws_client(&mut ws_task);
        }
        // Shutdown: drop ws_task, which aborts the spawned task
        drop(ws_task);
    }

    fn should_attempt(&self, project_id: &str) -> bool {
        if self.active_projects.contains(project_id) {
            return false;
        }
        self.backoffs.get(project_id).is_none_or(|b| b.can_retry())
    }

    fn list_paired_projects(&self) -> Vec<String> {
        let Ok(catalog) = self.catalog.read() else {
            return Vec::new();
        };
        match catalog.list_projects() {
            Ok(projects) => projects
                .into_iter()
                .filter(|p| p.paired_server_node_id.is_some())
                .map(|p| p.id)
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    // ── WebSocket hint client ───────────────────────────────────────

    fn ensure_ws_client(&self, ws_task: &mut Option<tokio::task::JoinHandle<()>>) {
        let needs_start = ws_task.as_ref().is_none_or(|t| t.is_finished());
        if !needs_start {
            return;
        }
        if let Some(settings) = self.read_ws_settings() {
            let wake_tx = self.wake_tx.clone();
            *ws_task = Some(tokio::spawn(async move {
                ws_client_loop(wake_tx, &settings.endpoint, &settings.token).await;
            }));
        }
    }

    fn read_ws_settings(&self) -> Option<WsSettings> {
        let settings = self.settings.read().ok()?;
        let endpoint = settings.paired_server_endpoint.as_deref()?.to_string();
        let node_id = settings.paired_server_node_id.as_deref()?;
        let token = crate::keychain::get_server_credential(&endpoint, node_id).ok()??;
        Some(WsSettings { endpoint, token })
    }

    async fn reconcile_project(&mut self, project_id: &str) {
        self.active_projects.insert(project_id.to_string());

        self.set_status(SyncStatus::with_state(
            project_id.to_string(),
            SyncState::Connecting,
        ));

        // ── Read current state (brief lock) ─────────────────────────
        let (settings, catalog) = {
            let s = match self.settings.read() {
                Ok(s) => s.clone(),
                Err(_) => {
                    self.set_status(SyncStatus::with_state(
                        project_id.to_string(),
                        SyncState::Attention,
                    ));
                    self.active_projects.remove(project_id);
                    return;
                }
            };
            let c = match self.catalog.read() {
                Ok(c) => c.clone(),
                Err(_) => {
                    self.set_status(SyncStatus::with_state(
                        project_id.to_string(),
                        SyncState::Attention,
                    ));
                    self.active_projects.remove(project_id);
                    return;
                }
            };
            (s, c)
        };

        let server_node_id = match settings.paired_server_node_id.as_deref().filter(|id| {
            catalog
                .list_projects()
                .ok()
                .and_then(|ps| ps.into_iter().find(|p| p.id == project_id))
                .and_then(|p| p.paired_server_node_id)
                .as_deref()
                == Some(id)
        }) {
            Some(id) => id.to_string(),
            None => {
                self.set_status(SyncStatus::unpaired(project_id.to_string()));
                self.active_projects.remove(project_id);
                return;
            }
        };

        let local_node_id = match catalog.local_node_id().unwrap_or(None) {
            Some(id) => id,
            None => {
                self.set_status(SyncStatus::with_state(
                    project_id.to_string(),
                    SyncState::Attention,
                ));
                self.active_projects.remove(project_id);
                return;
            }
        };

        // ── Build reconcile request ─────────────────────────────────
        let cursors = match catalog.operation_cursors() {
            Ok(c) => c,
            Err(_) => {
                self.active_projects.remove(project_id);
                return;
            }
        };

        let all_ops = match catalog.list_operations(project_id) {
            Ok(ops) => ops,
            Err(_) => {
                self.active_projects.remove(project_id);
                return;
            }
        };

        // Send local operations that the server hasn't acknowledged
        let peer_acknowledgements = catalog
            .peer_operation_acknowledgements(&server_node_id)
            .unwrap_or_default();
        let acknowledged_local = peer_acknowledgements
            .get(&local_node_id)
            .copied()
            .unwrap_or(0);
        let local_unacked = all_ops
            .iter()
            .filter(|op| {
                op.origin_node_id == local_node_id && op.origin_sequence > acknowledged_local
            })
            .cloned()
            .collect::<Vec<_>>();
        let has_more_local_operations = local_unacked.len() > DEFAULT_MAX_OPERATIONS;
        let local_unacked = local_unacked
            .into_iter()
            .take(DEFAULT_MAX_OPERATIONS)
            .collect();

        // The remote pending count is unknown until the server responds.
        let remote_unseen_count = 0;

        let request = ReconcileRequest {
            protocol_version: ProtocolRange::default(),
            project_schema_version: rcdvlog_core::PROJECT_CATALOG_SCHEMA_VERSION,
            project_id: project_id.to_string(),
            server_node_id: server_node_id.clone(),
            operation_cursors: cursors,
            operations: local_unacked,
            observations: Vec::new(),
            representation_manifests: Vec::new(),
            maximum_response_operations: DEFAULT_MAX_OPERATIONS,
        };

        let pending_local_count = request.operations.len() as u64;

        // Drop locks before HTTP call
        drop(settings);
        drop(catalog);

        // ── Endpoint selection ──────────────────────────────────────
        let endpoints = self.get_endpoints();
        if endpoints.is_empty() {
            self.set_status(SyncStatus::with_state(
                project_id.to_string(),
                SyncState::Offline,
            ));
            self.active_projects.remove(project_id);
            return;
        }

        self.set_status(SyncStatus::with_state(
            project_id.to_string(),
            SyncState::Syncing,
        ));

        let mut last_error: Option<ClientError> = None;
        let mut response: Option<ReconcileResponse> = None;
        let mut used_endpoint: Option<String> = None;

        for endpoint in &endpoints {
            // Verify server identity before sending credentials
            match ServerClient::fetch_version(endpoint).await {
                Ok(version) if version.node_id == server_node_id => {}
                Ok(_) => {
                    last_error = Some(ClientError::server(format!(
                        "server at {endpoint} has a different node ID — skipping"
                    )));
                    continue;
                }
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            }

            let client = match ServerClient::new(endpoint.clone(), server_node_id.clone()) {
                Ok(c) => c,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };

            match client.reconcile(project_id, &request).await {
                Ok(mut reconciled) => {
                    let maximum_recovery_pages = all_ops
                        .len()
                        .div_ceil(DEFAULT_MAX_OPERATIONS)
                        .saturating_add(1);
                    let mut recovery_pages = 0;
                    let mut recovery_failed = false;
                    while recovery_pages < maximum_recovery_pages {
                        let Some(operations) =
                            exact_sequence_recovery_page(&reconciled, &server_node_id, &all_ops)
                        else {
                            break;
                        };
                        recovery_pages += 1;
                        let mut recovery_request = request.clone();
                        recovery_request.operations = operations;
                        match client.reconcile(project_id, &recovery_request).await {
                            Ok(next) => reconciled = next,
                            Err(error) => {
                                last_error = Some(error);
                                recovery_failed = true;
                                break;
                            }
                        }
                    }
                    if recovery_failed {
                        continue;
                    }
                    response = Some(reconciled);
                    used_endpoint = Some(endpoint.clone());
                    break;
                }
                Err(e) => {
                    last_error = Some(e);
                }
            }
        }

        // ── Apply response ──────────────────────────────────────────
        match (response, used_endpoint.as_ref()) {
            (Some(resp), Some(endpoint)) => {
                // Re-acquire catalog lock to apply server operations
                let catalog = match self.catalog.read() {
                    Ok(c) => c.clone(),
                    Err(_) => {
                        self.active_projects.remove(project_id);
                        return;
                    }
                };

                if !resp.operations.is_empty()
                    && let Err(e) = apply_reconcile_operations(&catalog, &resp.operations)
                {
                    self.record_failure(project_id, Some(&format!("apply failed: {e}")));
                    self.active_projects.remove(project_id);
                    return;
                }
                if let Err(e) = catalog.record_peer_operation_acknowledgements(
                    &server_node_id,
                    &resp.committed_cursors,
                ) {
                    self.record_failure(project_id, Some(&format!("acknowledgement failed: {e}")));
                    self.active_projects.remove(project_id);
                    return;
                }

                self.record_success(project_id);

                // Enrich conflicts with operation payload summaries
                let conflict_details = self.enrich_conflicts(project_id, &resp.conflicts, &catalog);

                let mut status = SyncStatus {
                    project_id: project_id.to_string(),
                    state: SyncState::Synced,
                    last_sync_at: Some(
                        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    ),
                    last_error: None,
                    local_pending_operations: 0,
                    remote_pending_operations: if resp.has_more_operations {
                        resp.operations.len() as u64 + 1 // conservative estimate
                    } else {
                        0
                    },
                    conflict_count: resp.conflicts.len(),
                    conflict_details,
                    active_endpoint: Some(endpoint.clone()),
                    backoff_until: None,
                };

                if status.conflict_count > 0 || sequence_recovery_requires_attention(&resp) {
                    status.state = SyncState::Attention;
                }

                self.set_status(status);

                // ── Auto-download server proxies for assets missing local ones ──
                if let Some(client) = self.build_client(endpoint, &server_node_id) {
                    self.download_missing_proxies(&client, &catalog, project_id)
                        .await;
                }

                // ── Process Mac-targeted jobs from the server ──
                if !resp.node_targeted_jobs.is_empty() {
                    self.handle_node_targeted_jobs(
                        endpoint,
                        &server_node_id,
                        &catalog,
                        project_id,
                        &resp.node_targeted_jobs,
                    )
                    .await;
                }

                // If there are more operations to fetch, loop immediately
                if resp.has_more_operations || has_more_local_operations {
                    self.active_projects.remove(project_id);
                    // Recurse without backoff — pull next page
                    if self.should_attempt(project_id) {
                        Box::pin(self.reconcile_project(project_id)).await;
                        return;
                    }
                }
            }
            _ => {
                // All endpoints failed
                let error_msg = last_error
                    .as_ref()
                    .map(|e| e.message.clone())
                    .unwrap_or_else(|| "no configured endpoints".to_string());

                let state = match last_error.as_ref().map(|e| e.kind.as_str()) {
                    Some("auth") => {
                        // Check if it's a credential revocation
                        if error_msg.contains("revoked") {
                            SyncState::CredentialRevoked
                        } else {
                            SyncState::Attention
                        }
                    }
                    Some("protocol") => SyncState::Incompatible,
                    _ => SyncState::Offline,
                };

                self.record_failure(project_id, Some(&error_msg));

                let backoff_until = self
                    .backoffs
                    .get(project_id)
                    .and_then(|b| b.backoff_until());

                self.set_status(SyncStatus {
                    project_id: project_id.to_string(),
                    state,
                    last_error: Some(error_msg),
                    local_pending_operations: pending_local_count,
                    remote_pending_operations: remote_unseen_count as u64,
                    conflict_count: 0,
                    conflict_details: vec![],
                    active_endpoint: None,
                    last_sync_at: None,
                    backoff_until,
                });
            }
        }

        self.active_projects.remove(project_id);
    }

    // ── Helpers ────────────────────────────────────────────────────

    fn get_endpoints(&self) -> Vec<String> {
        let Ok(settings) = self.settings.read() else {
            return Vec::new();
        };
        // Primary endpoint first
        let mut urls = Vec::new();
        if let Some(ref primary) = settings.paired_server_endpoint {
            urls.push(primary.clone());
        }
        // Then alternates
        if let Some(ref alternates) = settings.paired_server_endpoints {
            for alt in alternates {
                let already_in = urls.contains(&alt.url);
                if !already_in {
                    urls.push(alt.url.clone());
                }
            }
        }
        urls
    }

    // ── Conflict enrichment ────────────────────────────────────────

    fn enrich_conflicts(
        &self,
        project_id: &str,
        conflicts: &[rcdvlog_core::ConflictProjection],
        catalog: &Catalog,
    ) -> Vec<ConflictDetail> {
        let all_ops = catalog.list_operations(project_id).unwrap_or_default();
        conflicts
            .iter()
            .map(|conflict| {
                let incoming_summary =
                    operation_summary_for(&all_ops, &conflict.incoming_operation_id);
                let active_summary = conflict
                    .active_operation_id
                    .as_deref()
                    .and_then(|op_id| operation_summary_for(&all_ops, op_id));
                ConflictDetail {
                    conflict_id: conflict.conflict_id.clone(),
                    conflict_type: conflict.conflict_type.clone(),
                    aggregate_type: conflict.aggregate_type.clone(),
                    aggregate_id: conflict.aggregate_id.clone(),
                    incoming_operation_summary: incoming_summary,
                    active_operation_summary: active_summary,
                    expected_base_revision: conflict.expected_base_revision,
                    actual_revision: conflict.actual_revision,
                    created_at: conflict.created_at.clone(),
                }
            })
            .collect()
    }

    fn record_success(&mut self, project_id: &str) {
        self.backoffs
            .entry(project_id.to_string())
            .or_default()
            .record_success();
    }

    fn record_failure(&mut self, project_id: &str, _error: Option<&str>) {
        self.backoffs
            .entry(project_id.to_string())
            .or_default()
            .record_failure();
    }

    fn set_status(&self, status: SyncStatus) {
        let project_id = status.project_id.clone();
        if let Ok(mut map) = self.status.write() {
            map.insert(project_id.clone(), status.clone());
        }
        let _ = self.app_handle.emit("sync-status-changed", status);
    }

    fn build_client(&self, endpoint: &str, server_node_id: &str) -> Option<ServerClient> {
        ServerClient::new(endpoint.to_string(), server_node_id.to_string()).ok()
    }

    /// After reconciliation, check for assets that have a media_object_id
    /// but no local proxy, and download compatible server representations.
    async fn download_missing_proxies(
        &self,
        client: &ServerClient,
        catalog: &Catalog,
        project_id: &str,
    ) {
        // Collect media_object_ids for assets missing local proxies
        let assets = match catalog.list_assets(project_id) {
            Ok(a) => a,
            Err(_) => return,
        };

        let missing: Vec<_> = assets
            .iter()
            .filter(|a| a.media_object_id.is_some() && !a.proxy_available && a.original_available)
            .collect();

        if missing.is_empty() {
            return;
        }

        let media_ids: Vec<String> = missing
            .iter()
            .filter_map(|a| a.media_object_id.clone())
            .collect();

        let representations = match client.query_representations(&media_ids).await {
            Ok(r) => r,
            Err(_) => return,
        };

        // Build a map from media_object_id -> representation
        // Pick the first available proxy for each media object
        let proxy_dir = match catalog.database_path().parent() {
            Some(p) => p.join("cache").join("proxies").join(project_id),
            None => return,
        };

        for asset in &missing {
            let Some(ref media_id) = asset.media_object_id else {
                continue;
            };
            let Some(rep) = representations
                .iter()
                .find(|r| r.media_object_id == *media_id && r.kind == "proxy")
            else {
                continue;
            };

            if let Err(e) =
                download_and_register_proxy(client, catalog, &asset.id, rep, &proxy_dir).await
            {
                // Log but continue — one failed download shouldn't block others
                eprintln!(
                    "failed to download proxy for asset {} (rep {}): {e}",
                    asset.id, rep.id
                );
            } else {
                // Emit updated asset to refresh UI
                if let Ok(updated) = catalog.get_asset(&asset.id) {
                    let _ = self.app_handle.emit("asset-updated", updated);
                }
            }
        }
    }

    /// Handle node-targeted jobs received from the server during reconciliation.
    /// For each job, check original availability, execute locally, and report
    /// the result back to the server.
    async fn handle_node_targeted_jobs(
        &self,
        endpoint: &str,
        server_node_id: &str,
        catalog: &Catalog,
        project_id: &str,
        jobs: &[rcdvlog_sync::NodeTargetedJob],
    ) {
        let Some(client) = self.build_client(endpoint, server_node_id) else {
            return;
        };

        let assets = match catalog.list_assets(project_id) {
            Ok(a) => a,
            Err(_) => return,
        };

        for job in jobs {
            // Find the local asset with matching media_object_id
            let Some(asset) = assets
                .iter()
                .find(|a| a.media_object_id.as_deref() == Some(&job.media_object_id))
            else {
                continue;
            };

            // Check original availability
            let original_path = std::path::Path::new(&asset.path);
            if !original_path.exists() {
                // Original is offline — skip, job stays queued on server
                continue;
            }

            let result = match job.kind.as_str() {
                "proxy" => match catalog.generate_proxy(&asset.id) {
                    Ok(updated) => {
                        let output_path = updated.proxy_path.as_deref().unwrap_or("");
                        let output = std::path::Path::new(output_path);
                        let byte_len = std::fs::metadata(output).ok().map(|m| m.len());
                        CompleteNodeJobResult::success(
                            job.representation_id.clone(),
                            byte_len,
                            output_path,
                        )
                    }
                    Err(e) => {
                        CompleteNodeJobResult::failure("proxy_generation_failed", &format!("{e:#}"))
                    }
                },
                "thumbnail" => match catalog.generate_thumbnail(&asset.id, 1) {
                    Ok(updated) => {
                        let thumb_path = updated.thumbnail_path.as_deref().unwrap_or("");
                        let output = std::path::Path::new(thumb_path);
                        let byte_len = std::fs::metadata(output).ok().map(|m| m.len());
                        CompleteNodeJobResult::success(
                            job.representation_id.clone(),
                            byte_len,
                            thumb_path,
                        )
                    }
                    Err(e) => CompleteNodeJobResult::failure(
                        "thumbnail_generation_failed",
                        &format!("{e:#}"),
                    ),
                },
                _ => continue,
            };

            // Report result back to server
            let request = result.to_complete_request();
            if let Err(e) = client.complete_node_job(&job.job_id, &request).await {
                eprintln!("failed to report job {} result to server: {e}", job.job_id);
            } else {
                // Emit updated asset to refresh UI
                if let Ok(updated) = catalog.get_asset(&asset.id) {
                    let _ = self.app_handle.emit("asset-updated", updated);
                }
            }
        }
    }
}

/// Helper for building the completion request from a job result.
struct CompleteNodeJobResult {
    status: String,
    output_identity: Option<String>,
    details: serde_json::Value,
}

impl CompleteNodeJobResult {
    fn success(
        representation_id: Option<String>,
        byte_length: Option<u64>,
        local_path: &str,
    ) -> Self {
        let mut details = serde_json::json!({
            "localPath": local_path,
        });
        if let Some(len) = byte_length {
            details["byteLength"] = serde_json::Value::Number(len.into());
        }
        Self {
            status: "succeeded".to_string(),
            output_identity: representation_id,
            details,
        }
    }

    fn failure(code: &str, message: &str) -> Self {
        Self {
            status: "failed".to_string(),
            output_identity: None,
            details: serde_json::json!({
                "code": code,
                "message": message,
            }),
        }
    }

    fn to_complete_request(&self) -> crate::server_client::CompleteNodeJobRequest {
        crate::server_client::CompleteNodeJobRequest {
            status: self.status.clone(),
            output_identity: self.output_identity.clone(),
            details: self.details.clone(),
        }
    }
}

// ── WebSocket client loop (runs as a spawned task) ──────────────────

struct WsSettings {
    endpoint: String,
    token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WsHint {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(default)]
    project_id: String,
}

async fn ws_client_loop(
    wake_tx: tokio::sync::mpsc::UnboundedSender<WakeEvent>,
    endpoint: &str,
    token: &str,
) {
    // Convert http(s):// to ws(s)://
    let ws_scheme = if endpoint.starts_with("https://") {
        "wss://"
    } else {
        "ws://"
    };
    let host = endpoint
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');
    let url = format!("{ws_scheme}{host}/api/v1/ws/events");

    loop {
        if let Ok((ws_stream, _)) = connect_async_with_auth(&url, token).await {
            let (mut write, mut read) = ws_stream.split();

            // Send a wildcard subscribe — the server filters by project,
            // and the sync loop ignores unknown project IDs.
            let subscribe = serde_json::json!({
                "type": "subscribe",
                "projectId": "*"
            });
            if write
                .send(WsMsg::Text(subscribe.to_string().into()))
                .await
                .is_err()
            {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }

            while let Some(msg) = read.next().await {
                match msg {
                    Ok(WsMsg::Text(text)) => {
                        if let Ok(hint) = serde_json::from_str::<WsHint>(&text)
                            && hint.msg_type == "hint"
                            && !hint.project_id.is_empty()
                        {
                            let _ = wake_tx.send(WakeEvent::ProjectMutated {
                                project_id: hint.project_id,
                            });
                        }
                    }
                    Ok(WsMsg::Close(_)) | Err(_) => break,
                    _ => {}
                }
            }
        }
        // Brief pause before reconnect
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

async fn connect_async_with_auth(
    url: &str,
    token: &str,
) -> Result<
    (
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        http::Response<Option<Vec<u8>>>,
    ),
    tokio_tungstenite::tungstenite::Error,
> {
    let origin = if let Some(rest) = url.strip_prefix("wss://") {
        format!("https://{}", rest.split('/').next().unwrap_or_default())
    } else if let Some(rest) = url.strip_prefix("ws://") {
        format!("http://{}", rest.split('/').next().unwrap_or_default())
    } else {
        String::new()
    };
    let request = http::Request::builder()
        .uri(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Origin", origin)
        .body(())
        .map_err(|_e| {
            tokio_tungstenite::tungstenite::Error::Http(Box::new(
                http::Response::builder()
                    .status(http::StatusCode::BAD_REQUEST)
                    .body(Some(b"invalid request".to_vec()))
                    .unwrap(),
            ))
        })?;
    connect_async(request).await
}

// ── Representation download ────────────────────────────────────────

const PROXY_CACHE_VERSION: &str = "v3";

/// Download a server representation and register it in the local project cache.
/// Uses authenticated streaming from `GET /api/v1/media/{id}/content`, verifies
/// the output with ffprobe, and atomically commits to the project-local proxy path.
async fn download_and_register_proxy(
    client: &ServerClient,
    catalog: &Catalog,
    asset_id: &str,
    representation: &crate::server_client::RepresentationDto,
    proxy_dir: &std::path::Path,
) -> Result<(), String> {
    let response = client
        .download_representation(&representation.id)
        .await
        .map_err(|e| format!("download request failed: {e}"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("server returned HTTP {status}"));
    }

    let expected_length = representation.byte_length;

    let output = proxy_dir.join(format!("{asset_id}.{PROXY_CACHE_VERSION}.mp4"));
    let temporary = proxy_dir.join(format!("{asset_id}.{PROXY_CACHE_VERSION}.tmp.mp4"));

    // Ensure the proxy directory exists
    std::fs::create_dir_all(proxy_dir).map_err(|e| format!("create proxy dir: {e}"))?;

    // Stream response body to temp file
    let mut temp_file = tokio::fs::File::create(&temporary)
        .await
        .map_err(|e| format!("create temp file: {e}"))?;
    let mut downloaded: u64 = 0;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("download stream error: {e}"))?;
        tokio::io::AsyncWriteExt::write_all(&mut temp_file, &chunk)
            .await
            .map_err(|e| format!("write temp file: {e}"))?;
        downloaded += chunk.len() as u64;
    }
    temp_file
        .flush()
        .await
        .map_err(|e| format!("flush temp file: {e}"))?;
    drop(temp_file);

    // Verify downloaded length if server reported it
    if let Some(expected) = expected_length
        && downloaded != expected
    {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!(
            "downloaded {downloaded} bytes, expected {expected}"
        ));
    }

    // Validate the downloaded proxy with ffprobe
    let result = tokio::task::spawn_blocking({
        let temp = temporary.clone();
        move || {
            let probe = rcdvlog_core::probe_media_stream(std::path::Path::new(&temp));
            match probe {
                Ok(info) if info.metadata_rotation_degrees == 0 => Ok(()),
                Ok(info) => Err(format!(
                    "proxy retains {}° display rotation",
                    info.metadata_rotation_degrees
                )),
                Err(e) => Err(format!("proxy validation failed: {e:#}")),
            }
        }
    })
    .await
    .map_err(|e| format!("validation task panicked: {e}"))?;
    result?;

    // Atomically commit
    std::fs::rename(&temporary, &output).map_err(|e| format!("commit proxy: {e}"))?;

    // Update the catalog
    let output_str = output.to_string_lossy().to_string();
    let asset_id_owned = asset_id.to_string();
    let db_path = catalog.database_path().to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let conn =
            rusqlite::Connection::open(&db_path).map_err(|e| format!("open catalog: {e}"))?;
        conn.execute(
            "UPDATE assets SET proxy_path = ?2 WHERE id = ?1",
            rusqlite::params![asset_id_owned, output_str],
        )
        .map_err(|e| format!("update proxy_path: {e}"))?;
        Ok(())
    })
    .await
    .map_err(|e| format!("catalog update panicked: {e}"))??;

    Ok(())
}

// ── Conflict enrichment helpers ──────────────────────────────────────

fn operation_summary_for(
    all_ops: &[rcdvlog_core::ProjectOperation],
    operation_id: &str,
) -> Option<OperationSummary> {
    let op = all_ops.iter().find(|o| o.operation_id == operation_id)?;

    let (camera, start_filename, offset_seconds, label) = if op.aggregate_type == "clock" {
        (
            op.payload
                .get("camera")
                .and_then(|v| v.as_str().map(String::from)),
            op.payload
                .get("startFilename")
                .and_then(|v| v.as_str().map(String::from)),
            op.payload.get("offsetSeconds").and_then(|v| v.as_i64()),
            op.payload
                .get("label")
                .and_then(|v| v.as_str().map(String::from)),
        )
    } else {
        (None, None, None, None)
    };

    Some(OperationSummary {
        operation_id: op.operation_id.clone(),
        operation_type: op.operation_type.clone(),
        origin_node_id: op.origin_node_id.clone(),
        origin_sequence: op.origin_sequence,
        camera,
        start_filename,
        offset_seconds,
        label,
    })
}
