#![recursion_limit = "256"]

mod admin_http;
mod admin_ops_http;
mod admin_storage_http;
mod auth_http;
mod events_http;
mod job_callback_http;
mod layers;
mod library_http;
mod media_http;
mod openapi;
mod project_http;
mod replica_http;
mod review_discussion_http;
mod review_session_http;
mod review_target_http;
mod review_transcript_http;
mod serve;
mod state;
mod status_http;

use anyhow::{Context, Result, anyhow};
use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::rejection::QueryRejection;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Extension, OriginalUri, Path as AxumPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use futures::SinkExt;
use futures::stream::StreamExt;
use rcdvlog_server_core::library_query::{LibraryFilters, LibraryQuery, LibrarySort};
use rcdvlog_server_core::{
    APPLICATION_VERSION, AddToProjectRequest, AuditActorType, AuthDiagnostics, AuthError,
    AuthService, BaselineEnvelope, BaselinePreview, BindingVerificationStatus,
    CONTROL_CATALOG_SCHEMA_VERSION, DeviceCredentialInfo, GlobalMediaVerificationState,
    JobEngineError, JobId, JobState, LocalNodeRootBindingInput, MediaAvailability, MediaObject,
    MediaObjectId, MediaSearchFacts, NewScanSchedule, NewStorageRoot, NodeAvailabilityState,
    NodeId, NodeJobCompletion, PROTOCOL_MAJOR_VERSION, PROTOCOL_MINOR_VERSION, ProjectId,
    ProjectReplica, ProjectReplicaError, ReadinessReport, ReconcileRequest, ReconcileResponse,
    Representation, RepresentationError, RepresentationId, RepresentationKind,
    RepresentationRequest, RootAssociationConfirmation, RootAssociationConfirmationStatus,
    RootRelativePath, RuntimeFoundation, ScanScheduleUpdate, ScanService, ScanServiceError,
    ScanToolConfig, SessionInfo, StorageAccessMode, StorageLibraryError, StorageProtocol,
    StorageRootId, StorageRootUpdate,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::future::IntoFuture;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::RwLock as StdRwLock;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::time::timeout;
use tokio_util::io::ReaderStream;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use uuid::Uuid;

use admin_http::*;
use admin_ops_http::*;
use admin_storage_http::*;
use auth_http::*;
use events_http::*;
use layers::*;
use library_http::*;
use media_http::*;
pub use openapi::*;
use project_http::*;
use replica_http::*;
pub use serve::*;
pub use state::*;
use status_http::*;

pub fn router(state: HttpState) -> Router {
    // Open auth routes exchange one-time credentials created through the
    // server-local admin command. Remote bootstrap creation is forbidden.
    let auth_open = Router::new()
        .route(
            "/api/v1/auth/bootstrap/exchange",
            post(exchange_bootstrap_code),
        )
        .route("/api/v1/auth/devices/exchange", post(exchange_pairing_code))
        .layer(DefaultBodyLimit::max(MAX_AUTH_BODY_BYTES))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            limit_open_auth_attempts,
        ));

    // Auth routes (require browser session)
    let auth_session = Router::new()
        .route("/api/v1/auth/sessions", get(list_sessions))
        .route("/api/v1/auth/sessions/{session_id}", delete(revoke_session))
        .route("/api/v1/auth/sessions/refresh", post(refresh_session))
        .route("/api/v1/auth/pairing", post(create_pairing_code))
        .route("/api/v1/auth/devices", get(list_devices))
        .route(
            "/api/v1/auth/devices/{credential_id}",
            delete(revoke_device),
        )
        .route("/api/v1/auth/diagnostics", get(auth_diagnostics))
        .layer(middleware::from_fn_with_state(state.clone(), require_csrf))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    // Internal routes (require auth — device or session)
    let internal = Router::new()
        .route(
            "/api/v1/internal/projects/baseline/preview",
            post(preview_project_baseline),
        )
        .route(
            "/api/v1/internal/projects/baseline/import",
            post(import_project_baseline),
        )
        .route(
            "/api/v1/internal/projects/{project_id}/reconcile",
            post(reconcile_project),
        )
        .route("/api/v1/representations", post(request_representation))
        .route(
            "/api/v1/library/representations/query",
            post(query_representations),
        )
        .route(
            "/api/v1/representations/{representation_id}",
            get(get_representation),
        )
        .route(
            "/api/v1/media/{representation_id}/content",
            get(media_content).head(media_content),
        )
        .route(
            "/api/v1/internal/jobs/{job_id}/complete",
            post(complete_node_job),
        )
        .route("/api/v1/ws/events", get(ws_events_handler))
        .route("/api/v1/library/media", get(list_library_media))
        .route(
            "/api/v1/library/media/{media_object_id}",
            get(get_library_media_detail),
        )
        .route("/api/v1/projects", get(list_projects))
        .route(
            "/api/v1/projects/{project_id}/assets",
            get(list_project_assets),
        )
        .route(
            "/api/v1/projects/{project_id}/assets/add/preview",
            post(preview_add_to_project),
        )
        .route(
            "/api/v1/projects/{project_id}/assets/add",
            post(add_to_project),
        )
        .route(
            "/api/v1/projects/{project_id}/assets/{asset_id}/review",
            get(get_project_review_asset),
        )
        .route(
            "/api/v1/projects/{project_id}/assets/{asset_id}/thumbnail",
            get(project_asset_thumbnail).head(project_asset_thumbnail),
        )
        .route(
            "/api/v1/projects/{project_id}/assets/{asset_id}/proxy",
            get(project_asset_proxy).head(project_asset_proxy),
        )
        .route(
            "/api/v1/projects/{project_id}/assets/{asset_id}/notes",
            post(create_review_note),
        )
        .route(
            "/api/v1/projects/{project_id}/assets/{asset_id}/labels/add",
            post(add_review_label),
        )
        .route(
            "/api/v1/projects/{project_id}/assets/{asset_id}/labels/remove",
            post(remove_review_label),
        )
        .route(
            "/api/v1/projects/{project_id}/assets/{asset_id}/removed",
            post(set_review_removed),
        )
        .route(
            "/api/v1/projects/{project_id}/assets/{asset_id}/rotation",
            post(set_review_rotation),
        )
        .route("/api/v1/admin/overview", get(admin_overview))
        .route("/api/v1/admin/audit", get(list_admin_audit))
        .route(
            "/api/v1/admin/storage-roots",
            get(list_admin_storage_roots).post(create_admin_storage_root),
        )
        .route(
            "/api/v1/admin/storage-roots/{root_id}",
            patch(update_admin_storage_root),
        )
        .route(
            "/api/v1/admin/storage-bindings",
            get(list_admin_storage_bindings),
        )
        .route(
            "/api/v1/admin/storage-roots/{root_id}/binding",
            put(upsert_admin_storage_binding),
        )
        .route(
            "/api/v1/admin/storage-roots/{root_id}/binding/confirmation",
            post(confirm_admin_storage_binding),
        )
        .route(
            "/api/v1/admin/storage-roots/{root_id}/binding/verify",
            post(verify_admin_storage_binding),
        )
        .route(
            "/api/v1/admin/scans",
            get(list_admin_scans).post(request_admin_scan),
        )
        .route(
            "/api/v1/admin/scans/{scan_id}/diagnostics",
            get(list_admin_scan_diagnostics),
        )
        .route(
            "/api/v1/admin/scan-schedules",
            get(list_admin_scan_schedules).post(create_admin_scan_schedule),
        )
        .route(
            "/api/v1/admin/scan-schedules/{schedule_id}",
            patch(update_admin_scan_schedule),
        )
        .route("/api/v1/admin/jobs", get(list_admin_jobs))
        .route("/api/v1/admin/sync", get(list_admin_sync))
        .route("/api/v1/admin/jobs/{job_id}", get(get_admin_job))
        .route("/api/v1/admin/jobs/{job_id}/cancel", post(cancel_admin_job))
        .route("/api/v1/admin/jobs/{job_id}/retry", post(retry_admin_job))
        .route(
            "/api/v1/admin/jobs/{job_id}/callbacks",
            post(job_callback_http::subscribe),
        )
        .route("/api/v1/admin/job-callbacks", get(job_callback_http::list))
        .route(
            "/api/v1/admin/job-callbacks/claim",
            post(job_callback_http::claim),
        )
        .route(
            "/api/v1/admin/job-callbacks/{delivery_id}/settle",
            post(job_callback_http::settle),
        )
        .route(
            "/api/v1/admin/backups",
            get(list_admin_backups).post(request_admin_backup),
        )
        .route("/api/v1/admin/backups/{backup_id}", get(get_admin_backup))
        .route(
            "/api/v1/admin/representations/batch",
            post(request_admin_representation_batch),
        )
        .layer(middleware::from_fn_with_state(state.clone(), require_csrf))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    // TV review targets (#221): same auth/CSRF guards, with a body limit sized
    // to the handoff validator's 16 MiB cap rather than the media default.
    let review_targets = Router::new()
        .route(
            "/api/v1/projects/{project_id}/review-targets",
            get(review_target_http::list),
        )
        .route(
            "/api/v1/projects/{project_id}/review-targets/preflight",
            post(review_target_http::preflight),
        )
        .route(
            "/api/v1/projects/{project_id}/review-targets/import",
            post(review_target_http::import),
        )
        .route(
            "/api/v1/projects/{project_id}/review-targets/historical/preflight",
            post(review_target_http::preflight_historical),
        )
        .route(
            "/api/v1/projects/{project_id}/review-targets/historical/import",
            post(review_target_http::import_historical),
        )
        .route(
            "/api/v1/projects/{project_id}/review-targets/{review_target_id}",
            get(review_target_http::get).patch(review_target_http::set_display_name),
        )
        .route(
            "/api/v1/projects/{project_id}/review-targets/{review_target_id}/youtube",
            post(review_target_http::rebind_youtube),
        )
        .route(
            "/api/v1/projects/{project_id}/review-targets/{review_target_id}/candidates",
            get(review_target_http::candidates),
        )
        .layer(DefaultBodyLimit::max(
            review_target_http::MAX_REVIEW_TARGET_BODY_BYTES,
        ))
        .layer(middleware::from_fn_with_state(state.clone(), require_csrf))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    // Public routes (no auth)
    let public = Router::new()
        .route("/api/v1", get(root))
        .route("/api/openapi.json", get(openapi))
        .route("/api/v1/health", get(health))
        .route("/api/v1/ready", get(ready))
        .route("/api/v1/version", get(version));

    Router::new()
        .merge(public)
        .merge(auth_open)
        .merge(auth_session)
        .merge(internal)
        .merge(review_targets)
        .merge(review_session_http::routes(&state))
        .merge(review_transcript_http::routes(&state))
        .merge(review_discussion_http::routes(&state))
        .fallback(web_or_not_found)
        .with_state(state.clone())
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
        .layer(middleware::from_fn_with_state(
            state,
            validate_transport_request,
        ))
        .layer(middleware::from_fn(security_headers))
        .layer(middleware::from_fn(request_context))
}

#[cfg(test)]
mod tests;
