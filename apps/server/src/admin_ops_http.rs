//! Admin scans, jobs, sync, backups, and representation batches.

use super::*;

pub(super) fn admin_scan_service(state: &HttpState) -> ScanService {
    ScanService::new(
        state.foundation.catalog().storage_library(),
        ScanToolConfig {
            ffprobe_program: PathBuf::from(&state.foundation.config().ffprobe_command),
            exiftool_program: (!state.foundation.config().exiftool_command.is_empty())
                .then(|| PathBuf::from(&state.foundation.config().exiftool_command)),
        },
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct AdminScanQuery {
    #[serde(default = "default_admin_limit")]
    pub(super) limit: u32,
    pub(super) after: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RequestScanRequest {
    pub(super) storage_root_id: StorageRootId,
    pub(super) scope_relative_path: Option<RootRelativePath>,
    pub(super) idempotency_key: String,
}

pub(super) async fn list_admin_scans(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    query: Result<Query<AdminScanQuery>, QueryRejection>,
) -> Response {
    let Query(query) = match query {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
        }
    };
    match admin_scan_service(&state).list_scans(query.limit, query.after.as_deref()) {
        Ok(items) => Json(serde_json::json!({"items":items})).into_response(),
        Err(error) => scan_admin_error(&request_id, error),
    }
}

pub(super) async fn request_admin_scan(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    request: Result<Json<RequestScanRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    if request.idempotency_key.trim().is_empty() || request.idempotency_key.len() > 255 {
        return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
    }
    match admin_scan_service(&state).request_scan_idempotent(
        &request.storage_root_id,
        request.scope_relative_path.as_ref(),
        &request.idempotency_key,
    ) {
        Ok(scan) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "scan.requested",
            "scan",
            Some(&scan.scan_id),
            serde_json::json!({"storageRootId": request.storage_root_id, "scopeRelativePath": request.scope_relative_path, "idempotencyKey": request.idempotency_key}),
        ) {
            Ok(()) => (StatusCode::ACCEPTED, Json(scan)).into_response(),
            Err(response) => *response,
        },
        Err(error) => scan_admin_error(&request_id, error),
    }
}

pub(super) async fn list_admin_scan_diagnostics(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(scan_id): AxumPath<String>,
    query: Result<Query<AdminScanQuery>, QueryRejection>,
) -> Response {
    let Query(query) = match query {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
        }
    };
    let service = admin_scan_service(&state);
    if let Err(error) = service.get_scan(&scan_id) {
        return scan_admin_error(&request_id, error);
    }
    match service.list_scan_diagnostics(&scan_id, query.limit, query.after.as_deref()) {
        Ok(items) => Json(serde_json::json!({"items":items})).into_response(),
        Err(error) => scan_admin_error(&request_id, error),
    }
}

pub(super) async fn list_admin_scan_schedules(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    match admin_scan_service(&state).list_schedules() {
        Ok(items) => Json(serde_json::json!({"items":items})).into_response(),
        Err(error) => scan_admin_error(&request_id, error),
    }
}

pub(super) async fn create_admin_scan_schedule(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    request: Result<Json<NewScanSchedule>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    match admin_scan_service(&state).create_schedule(request) {
        Ok(schedule) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "scan_schedule.created",
            "scan_schedule",
            Some(&schedule.schedule_id),
            serde_json::json!({"timezone":schedule.timezone,"localTime":schedule.local_time,"enabled":schedule.enabled}),
        ) {
            Ok(()) => (StatusCode::CREATED, Json(schedule)).into_response(),
            Err(response) => *response,
        },
        Err(error) => scan_admin_error(&request_id, error),
    }
}

pub(super) async fn update_admin_scan_schedule(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(schedule_id): AxumPath<String>,
    request: Result<Json<ScanScheduleUpdate>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    match admin_scan_service(&state).update_schedule(&schedule_id, request) {
        Ok(schedule) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "scan_schedule.updated",
            "scan_schedule",
            Some(&schedule_id),
            serde_json::json!({"timezone":schedule.timezone,"localTime":schedule.local_time,"enabled":schedule.enabled}),
        ) {
            Ok(()) => Json(schedule).into_response(),
            Err(response) => *response,
        },
        Err(error) => scan_admin_error(&request_id, error),
    }
}

pub(super) fn scan_admin_error(request_id: &RequestId, error: ScanServiceError) -> Response {
    match error {
        ScanServiceError::InvalidInput { .. } => {
            query_error_response(request_id, "invalid_input", StatusCode::BAD_REQUEST)
        }
        ScanServiceError::NotFound { .. } => {
            query_error_response(request_id, "not_found", StatusCode::NOT_FOUND)
        }
        ScanServiceError::ScanAlreadyActive => {
            query_error_response(request_id, "scan_already_active", StatusCode::CONFLICT)
        }
        ScanServiceError::IdempotencyConflict => {
            query_error_response(request_id, "idempotency_conflict", StatusCode::CONFLICT)
        }
        _ => query_error_response(
            request_id,
            "internal_error",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct AdminJobsQuery {
    #[serde(default = "default_admin_limit")]
    pub(super) limit: u32,
    pub(super) before: Option<u64>,
    pub(super) state: Option<JobState>,
}

pub(super) async fn list_admin_jobs(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    query: Result<Query<AdminJobsQuery>, QueryRejection>,
) -> Response {
    let Query(query) = match query {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
        }
    };
    match state
        .foundation
        .jobs()
        .list_jobs(query.limit, query.before, query.state)
    {
        Ok(page) => Json(page).into_response(),
        Err(error) => job_admin_error(&request_id, error),
    }
}

pub(super) async fn list_admin_sync(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    match state
        .foundation
        .catalog()
        .project_replicas()
        .list_sync_overview()
    {
        Ok(items) => Json(serde_json::json!({"items":items})).into_response(),
        Err(error) => project_read_error_response(&request_id, error),
    }
}

pub(super) async fn get_admin_job(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(raw_id): AxumPath<String>,
) -> Response {
    let id = match JobId::parse(raw_id) {
        Ok(id) => id,
        Err(_) => return query_error_response(&request_id, "not_found", StatusCode::NOT_FOUND),
    };
    match state.foundation.jobs().get_job(&id) {
        Ok(job) => Json(job).into_response(),
        Err(error) => job_admin_error(&request_id, error),
    }
}

pub(super) async fn cancel_admin_job(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(raw_id): AxumPath<String>,
) -> Response {
    let id = match JobId::parse(raw_id) {
        Ok(id) => id,
        Err(_) => return query_error_response(&request_id, "not_found", StatusCode::NOT_FOUND),
    };
    match state
        .foundation
        .jobs()
        .request_cancel(&id, Some("browser administration request"))
    {
        Ok(()) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "job.cancel_requested",
            "job",
            Some(id.as_str()),
            serde_json::json!({}),
        ) {
            Ok(()) => match state.foundation.jobs().get_job(&id) {
                Ok(job) => Json(job).into_response(),
                Err(error) => job_admin_error(&request_id, error),
            },
            Err(response) => *response,
        },
        Err(error) => job_admin_error(&request_id, error),
    }
}

pub(super) async fn retry_admin_job(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(raw_id): AxumPath<String>,
) -> Response {
    let id = match JobId::parse(raw_id) {
        Ok(id) => id,
        Err(_) => return query_error_response(&request_id, "not_found", StatusCode::NOT_FOUND),
    };
    match state.foundation.jobs().retry_failed(&id) {
        Ok(job) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "job.retry_queued",
            "job",
            Some(id.as_str()),
            serde_json::json!({"attemptCount":job.attempt_count}),
        ) {
            Ok(()) => Json(job).into_response(),
            Err(response) => *response,
        },
        Err(error) => job_admin_error(&request_id, error),
    }
}

pub(super) fn job_admin_error(request_id: &RequestId, error: JobEngineError) -> Response {
    match error {
        JobEngineError::StaleLease => {
            query_error_response(request_id, "conflict", StatusCode::CONFLICT)
        }
        JobEngineError::InvalidInput(_) => {
            query_error_response(request_id, "invalid_input", StatusCode::BAD_REQUEST)
        }
        JobEngineError::NotFound(_) => {
            query_error_response(request_id, "not_found", StatusCode::NOT_FOUND)
        }
        JobEngineError::InvalidState(_) => {
            query_error_response(request_id, "job_not_retryable", StatusCode::CONFLICT)
        }
        _ => query_error_response(
            request_id,
            "internal_error",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct BackupRequest {
    pub(super) idempotency_key: String,
}

pub(super) async fn list_admin_backups(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    let manager = state
        .foundation
        .catalog()
        .backups(state.foundation.config().clone());
    match manager.list_restore_points() {
        Ok(items) => Json(serde_json::json!({
            "items": items,
            "dailyRetention": state.foundation.config().backup_daily_retention,
            "weeklyRetention": state.foundation.config().backup_weekly_retention,
        }))
        .into_response(),
        Err(error) => backup_admin_error(&request_id, error),
    }
}

pub(super) async fn get_admin_backup(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(backup_id): AxumPath<String>,
) -> Response {
    let manager = state
        .foundation
        .catalog()
        .backups(state.foundation.config().clone());
    match manager.list_restore_points() {
        Ok(items) => match items.into_iter().find(|item| {
            item.manifest
                .as_ref()
                .is_some_and(|manifest| manifest.backup_id == backup_id)
        }) {
            Some(item) => Json(item).into_response(),
            None => query_error_response(&request_id, "not_found", StatusCode::NOT_FOUND),
        },
        Err(error) => backup_admin_error(&request_id, error),
    }
}

pub(super) async fn request_admin_backup(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    request: Result<Json<BackupRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(request) => request,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    let manager = state
        .foundation
        .catalog()
        .backups(state.foundation.config().clone());
    let service = rcdvlog_server_core::BackupService::new(manager, state.foundation.jobs());
    match service.request_backup(&request.idempotency_key) {
        Ok(job) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "backup.requested",
            "backup",
            None,
            serde_json::json!({"jobId":job.definition.job_id,"idempotencyKey":request.idempotency_key}),
        ) {
            Ok(()) => (StatusCode::ACCEPTED, Json(job)).into_response(),
            Err(response) => *response,
        },
        Err(error) => backup_admin_error(&request_id, error),
    }
}

pub(super) fn backup_admin_error(
    request_id: &RequestId,
    error: rcdvlog_server_core::BackupError,
) -> Response {
    let status = match &error {
        rcdvlog_server_core::BackupError::InvalidInput(_) => StatusCode::BAD_REQUEST,
        rcdvlog_server_core::BackupError::NotFound => StatusCode::NOT_FOUND,
        rcdvlog_server_core::BackupError::Incompatible(_)
        | rcdvlog_server_core::BackupError::Verification(_) => StatusCode::CONFLICT,
        rcdvlog_server_core::BackupError::Canceled
        | rcdvlog_server_core::BackupError::BusyTimeout => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    query_error_response(request_id, error.code(), status)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RepresentationBatchRequest {
    pub(super) media_object_ids: Vec<MediaObjectId>,
    pub(super) kind: RepresentationKind,
    pub(super) idempotency_key: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RepresentationBatchResult {
    pub(super) selected_count: usize,
    pub(super) reused_count: usize,
    pub(super) queued_count: usize,
    pub(super) succeeded_count: usize,
    pub(super) failed_count: usize,
    pub(super) skipped_count: usize,
    pub(super) skipped: Vec<RepresentationBatchSkip>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RepresentationBatchSkip {
    pub(super) media_object_id: MediaObjectId,
    pub(super) reason: String,
}

pub(super) async fn request_admin_representation_batch(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    request: Result<Json<RepresentationBatchRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    if request.media_object_ids.is_empty()
        || request.media_object_ids.len() > 200
        || request.idempotency_key.is_empty()
        || request.idempotency_key.len() > 255
    {
        return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
    }
    if !has_media_work_capacity(&state) {
        return query_error_response(
            &request_id,
            "low_disk_space",
            StatusCode::INSUFFICIENT_STORAGE,
        );
    }
    let service = state
        .foundation
        .catalog()
        .representations(state.foundation.config().derived_dir.clone());
    let mut reused_count = 0;
    let mut queued_count = 0;
    let mut succeeded_count = 0;
    let mut skipped = Vec::new();
    for media_object_id in &request.media_object_ids {
        match service.request(RepresentationRequest {
            media_object_id: media_object_id.clone(),
            kind: request.kind,
        }) {
            Ok(result) if result.reused => {
                reused_count += 1;
                succeeded_count += 1;
            }
            Ok(_) => queued_count += 1,
            Err(error) => skipped.push(RepresentationBatchSkip {
                media_object_id: media_object_id.clone(),
                reason: error.code().to_string(),
            }),
        }
    }
    let result = RepresentationBatchResult {
        selected_count: request.media_object_ids.len(),
        reused_count,
        queued_count,
        succeeded_count,
        failed_count: 0,
        skipped_count: skipped.len(),
        skipped,
    };
    match audit_admin(
        &state,
        &principal,
        &request_id,
        "representation_batch.requested",
        "representation_batch",
        None,
        serde_json::json!({"kind":match request.kind { RepresentationKind::Proxy => "proxy", RepresentationKind::Thumbnail => "thumbnail" },"selectedCount":result.selected_count,"reusedCount":result.reused_count,"queuedCount":result.queued_count,"skippedCount":result.skipped_count,"idempotencyKey":request.idempotency_key}),
    ) {
        Ok(()) => (StatusCode::ACCEPTED, Json(result)).into_response(),
        Err(response) => *response,
    }
}
