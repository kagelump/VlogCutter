//! Thin authenticated adapter over `rcdvlog_server_core::ReviewSessions` (#222).
//! Raw audio, paths and client identifiers are never logged.
use super::*;
use axum::body::Bytes;
use rcdvlog_server_core::{
    ChunkUpload, CreateReviewSessionRequest, FinalizeReviewSessionRequest,
    REVIEW_SESSION_LIST_MAX_LIMIT, REVIEW_SESSION_MAX_CHUNK_BYTES, ReviewSessionError,
    ReviewSessionEventRequest, ReviewSessions, ReviewTargetActor, wav_header,
};
use std::sync::OnceLock;

pub(super) const MAX_CHUNK_BODY_BYTES: usize = REVIEW_SESSION_MAX_CHUNK_BYTES + 1024;
/// Server-wide bound on concurrent chunk writes; excess callers get 503 and retry.
const CONCURRENT_CHUNK_WRITES: usize = 4;

fn chunk_writes() -> Arc<Semaphore> {
    static PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    PERMITS
        .get_or_init(|| Arc::new(Semaphore::new(CONCURRENT_CHUNK_WRITES)))
        .clone()
}

fn sessions(state: &HttpState) -> ReviewSessions {
    state
        .foundation
        .catalog()
        .review_sessions(state.foundation.config().minimum_free_bytes)
}

fn actor(principal: &AuthPrincipal) -> ReviewTargetActor {
    match principal {
        AuthPrincipal::Browser { session_id, .. } => {
            ReviewTargetActor::new(AuditActorType::BrowserSession, session_id.as_str())
        }
        AuthPrincipal::Device { credential_id } => {
            ReviewTargetActor::new(AuditActorType::DeviceCredential, credential_id.as_str())
        }
    }
}

fn error_response(request_id: &RequestId, error: ReviewSessionError) -> Response {
    let status = match &error {
        ReviewSessionError::ProjectNotFound | ReviewSessionError::NotFound => StatusCode::NOT_FOUND,
        ReviewSessionError::InvalidInput(_) | ReviewSessionError::ChunkCorrupt => {
            StatusCode::BAD_REQUEST
        }
        ReviewSessionError::TargetNotReady(_)
        | ReviewSessionError::NotRecording
        | ReviewSessionError::ChunkConflict
        | ReviewSessionError::AudioUnavailable
        | ReviewSessionError::AudioGap
        | ReviewSessionError::IdempotencyConflict => StatusCode::CONFLICT,
        ReviewSessionError::LimitExceeded => StatusCode::PAYLOAD_TOO_LARGE,
        ReviewSessionError::LowDiskSpace => StatusCode::INSUFFICIENT_STORAGE,
        ReviewSessionError::Busy => {
            let mut response =
                query_error_response(request_id, error.code(), StatusCode::SERVICE_UNAVAILABLE);
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("2"));
            return response;
        }
        ReviewSessionError::Infrastructure(_)
        | ReviewSessionError::Database(_)
        | ReviewSessionError::Io(_) => {
            // The error text may name evidence paths; log the stable code only.
            tracing::error!(request_id = %request_id.0, code = error.code(), "review session request failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    query_error_response(request_id, error.code(), status)
}

async fn blocking<T: Send + 'static>(
    request_id: &RequestId,
    work: impl FnOnce() -> Result<T, ReviewSessionError> + Send + 'static,
) -> Result<T, Response> {
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(error_response(request_id, error)),
        Err(_) => Err(query_error_response(
            request_id,
            "internal_error",
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ListQuery {
    review_target_id: Option<String>,
    #[serde(default = "default_admin_limit")]
    limit: u32,
    before: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ChunkQuery {
    start_frame: u64,
    frame_count: u32,
    sha256: String,
    capture_end_ms: Option<f64>,
}

pub(super) async fn list(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(project_id): AxumPath<String>,
    query: Result<Query<ListQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
    };
    if query.limit == 0 || query.limit > REVIEW_SESSION_LIST_MAX_LIMIT {
        return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
    }
    let sessions = sessions(&state);
    match blocking(&request_id, move || {
        sessions.list(
            &project_id,
            query.review_target_id.as_deref(),
            query.limit,
            query.before,
            chrono::Utc::now(),
        )
    })
    .await
    {
        Ok(page) => Json(page).into_response(),
        Err(response) => response,
    }
}

pub(super) async fn create(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(project_id): AxumPath<String>,
    body: Result<Json<CreateReviewSessionRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
    };
    let sessions = sessions(&state);
    let creator = actor(&principal);
    let detail = match blocking(&request_id, move || {
        sessions.create(&project_id, &body, &creator, chrono::Utc::now())
    })
    .await
    {
        Ok(detail) => detail,
        Err(response) => return response,
    };
    if let Err(response) = audit_admin(
        &state,
        &principal,
        &request_id,
        "review_session.started",
        "review_session",
        Some(&detail.summary.session_id),
        serde_json::json!({
            "reviewTargetId": detail.summary.review_target_id,
            "additionalSpeakersDisclosed": detail.consent.additional_speakers_disclosed,
        }),
    ) {
        return *response;
    }
    Json(detail).into_response()
}

pub(super) async fn get_session(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, session_id)): AxumPath<(String, String)>,
) -> Response {
    let sessions = sessions(&state);
    match blocking(&request_id, move || {
        sessions.get(&project_id, &session_id, chrono::Utc::now())
    })
    .await
    {
        Ok(detail) => Json(detail).into_response(),
        Err(response) => response,
    }
}

pub(super) async fn put_chunk(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, session_id, sequence)): AxumPath<(String, String, u32)>,
    query: Result<Query<ChunkQuery>, QueryRejection>,
    body: Bytes,
) -> Response {
    let Ok(Query(query)) = query else {
        return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
    };
    let Ok(permit) = chunk_writes().try_acquire_owned() else {
        return error_response(&request_id, ReviewSessionError::Busy);
    };
    let sessions = sessions(&state);
    let result = blocking(&request_id, move || {
        let _permit = permit;
        sessions.put_chunk(
            &project_id,
            &session_id,
            &ChunkUpload {
                sequence,
                start_frame: query.start_frame,
                frame_count: query.frame_count,
                sha256: query.sha256,
                capture_end_performance_ms: query.capture_end_ms,
            },
            &body,
            chrono::Utc::now(),
        )
    })
    .await;
    match result {
        Ok(receipt) => Json(receipt).into_response(),
        Err(response) => response,
    }
}

pub(super) async fn get_chunk(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, session_id, sequence)): AxumPath<(String, String, u32)>,
) -> Response {
    let sessions = sessions(&state);
    match blocking(&request_id, move || {
        sessions.chunk_bytes(&project_id, &session_id, sequence)
    })
    .await
    {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::CACHE_CONTROL, "private, no-store")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Err(response) => response,
    }
}

pub(super) async fn record_event(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, session_id)): AxumPath<(String, String)>,
    body: Result<Json<ReviewSessionEventRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
    };
    let sessions = sessions(&state);
    match blocking(&request_id, move || {
        sessions.record_event(&project_id, &session_id, &body, chrono::Utc::now())
    })
    .await
    {
        Ok(event) => Json(event).into_response(),
        Err(response) => response,
    }
}

pub(super) async fn finalize(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath((project_id, session_id)): AxumPath<(String, String)>,
    body: Result<Json<FinalizeReviewSessionRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
    };
    let sessions = sessions(&state);
    let detail = match blocking(&request_id, move || {
        sessions.finalize(&project_id, &session_id, &body, chrono::Utc::now())
    })
    .await
    {
        Ok(detail) => detail,
        Err(response) => return response,
    };
    if let Err(response) = audit_admin(
        &state,
        &principal,
        &request_id,
        "review_session.finalized",
        "review_session",
        Some(&detail.summary.session_id),
        serde_json::json!({ "state": detail.summary.state, "gapCount": detail.coverage.gaps.len() }),
    ) {
        return *response;
    }
    Json(detail).into_response()
}

pub(super) async fn delete_session(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath((project_id, session_id)): AxumPath<(String, String)>,
) -> Response {
    let sessions = sessions(&state);
    let detail = match blocking(&request_id, move || {
        sessions.delete(&project_id, &session_id, chrono::Utc::now())
    })
    .await
    {
        Ok(detail) => detail,
        Err(response) => return response,
    };
    if let Err(response) = audit_admin(
        &state,
        &principal,
        &request_id,
        "review_session.deleted",
        "review_session",
        Some(&detail.summary.session_id),
        serde_json::json!({}),
    ) {
        return *response;
    }
    Json(detail).into_response()
}

pub(super) async fn audio(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, session_id)): AxumPath<(String, String)>,
) -> Response {
    let sessions = sessions(&state);
    let filename_id = session_id.clone();
    let audio = match blocking(&request_id, move || {
        sessions.audio(&project_id, &session_id)
    })
    .await
    {
        Ok(audio) => audio,
        Err(response) => return response,
    };
    let header_bytes = wav_header(audio.sample_rate, audio.channels, audio.data_bytes);
    let length = header_bytes.len() as u64 + audio.data_bytes;
    let head =
        futures::stream::once(async move { Ok::<_, std::io::Error>(Bytes::from(header_bytes)) });
    let chunks = futures::stream::iter(audio.chunk_paths)
        .then(|path| async move { tokio::fs::read(path).await.map(Bytes::from) });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "audio/wav")
        .header(header::CONTENT_LENGTH, length)
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename_id}.wav\""),
        )
        .body(Body::from_stream(head.chain(chunks)))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct SnippetQuery {
    start_frame: u64,
    end_frame: u64,
}

/// A bounded WAV snippet for listening, from any non-deleted session. Inline,
/// uncached and never padded: a range touching missing audio is a 409.
pub(super) async fn snippet(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath((project_id, session_id)): AxumPath<(String, String)>,
    query: Result<Query<SnippetQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
    };
    let sessions = sessions(&state);
    let snippet = match blocking(&request_id, move || {
        sessions.snippet(&project_id, &session_id, query.start_frame, query.end_frame)
    })
    .await
    {
        Ok(snippet) => snippet,
        Err(response) => return response,
    };
    let mut body = wav_header(
        snippet.sample_rate,
        snippet.channels,
        snippet.pcm.len() as u64,
    );
    body.extend_from_slice(&snippet.pcm);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "audio/wav")
        .header(header::CONTENT_LENGTH, body.len())
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(header::CONTENT_DISPOSITION, "inline")
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

pub(super) fn routes(state: &HttpState) -> Router<HttpState> {
    let base = "/api/v1/projects/{project_id}/review-sessions";
    let chunks = Router::new()
        .route(
            &format!("{base}/{{session_id}}/chunks/{{sequence}}"),
            put(put_chunk).get(get_chunk),
        )
        .layer(DefaultBodyLimit::max(MAX_CHUNK_BODY_BYTES));
    Router::new()
        .route(base, get(list).post(create))
        .route(
            &format!("{base}/{{session_id}}"),
            axum::routing::get(get_session).delete(delete_session),
        )
        .route(&format!("{base}/{{session_id}}/events"), post(record_event))
        .route(&format!("{base}/{{session_id}}/finalize"), post(finalize))
        .route(&format!("{base}/{{session_id}}/audio.wav"), get(audio))
        .route(&format!("{base}/{{session_id}}/snippet.wav"), get(snippet))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .merge(chunks)
        .layer(middleware::from_fn_with_state(state.clone(), require_csrf))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
}

/// Rust-owned OpenAPI fragment, merged into the server document.
pub(super) fn openapi_extend(document: &mut serde_json::Value) {
    use serde_json::json;
    let project =
        json!({"name":"projectId","in":"path","required":true,"schema":{"type":"string"}});
    let session =
        json!({"name":"sessionId","in":"path","required":true,"schema":{"type":"string"}});
    let sequence = json!({"name":"sequence","in":"path","required":true,"schema":{"type":"integer","minimum":0}});
    let body = |schema: &str| json!({"required":true,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{schema}")}}}});
    let ok = |schema: &str| json!({"description":schema,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{schema}")}}}});
    let error = json!({"$ref":"#/components/responses/Error"});
    let paths = json!({
        "/api/v1/projects/{projectId}/review-sessions": {
            "get": {"operationId":"listReviewSessions","parameters":[project.clone(),
                {"name":"reviewTargetId","in":"query","schema":{"type":"string"}},
                {"name":"limit","in":"query","schema":{"type":"integer","minimum":1,"maximum":REVIEW_SESSION_LIST_MAX_LIMIT}},
                {"name":"before","in":"query","schema":{"type":"integer"}}],
                "responses":{"200":ok("ReviewSessionPage"),"400":error.clone(),"404":error.clone()}},
            "post": {"operationId":"createReviewSession","description":"Start a private recording session bound to a ready, imported review target. Idempotent by clientSessionKey.",
                "parameters":[project.clone()],"requestBody":body("CreateReviewSessionRequest"),
                "responses":{"200":ok("ReviewSessionDetail"),"400":error.clone(),"404":error.clone(),"409":error.clone(),"503":error.clone()}}
        },
        "/api/v1/projects/{projectId}/review-sessions/{sessionId}": {
            "get": {"operationId":"getReviewSession","parameters":[project.clone(),session.clone()],
                "responses":{"200":ok("ReviewSessionDetail"),"404":error.clone()}},
            "delete": {"operationId":"deleteReviewSession","description":"Permanently delete the recorded audio; a metadata tombstone remains.",
                "parameters":[project.clone(),session.clone()],"responses":{"200":ok("ReviewSessionDetail"),"404":error.clone()}}
        },
        "/api/v1/projects/{projectId}/review-sessions/{sessionId}/chunks/{sequence}": {
            "put": {"operationId":"putReviewSessionChunk","description":"Upload one pcm_s16le chunk. Idempotent for identical content; conflicting or overlapping chunks return 409.",
                "parameters":[project.clone(),session.clone(),sequence.clone(),
                    {"name":"startFrame","in":"query","required":true,"schema":{"type":"integer","minimum":0}},
                    {"name":"frameCount","in":"query","required":true,"schema":{"type":"integer","minimum":1}},
                    {"name":"sha256","in":"query","required":true,"schema":{"type":"string","pattern":"^[0-9a-fA-F]{64}$"}},
                    {"name":"captureEndMs","in":"query","schema":{"type":"number","minimum":0}}],
                "requestBody":{"required":true,"content":{"application/octet-stream":{"schema":{"type":"string","format":"binary","maxLength":REVIEW_SESSION_MAX_CHUNK_BYTES}}}},
                "responses":{"200":ok("ReviewSessionChunkReceipt"),"400":error.clone(),"404":error.clone(),"409":error.clone(),"413":error.clone(),"503":error.clone(),"507":error.clone()}},
            "get": {"operationId":"getReviewSessionChunk","parameters":[project.clone(),session.clone(),sequence],
                "responses":{"200":{"description":"Stored chunk bytes","content":{"application/octet-stream":{"schema":{"type":"string","format":"binary"}}}},"404":error.clone(),"409":error.clone()}}
        },
        "/api/v1/projects/{projectId}/review-sessions/{sessionId}/events": {
            "post": {"operationId":"recordReviewSessionEvent","parameters":[project.clone(),session.clone()],"requestBody":body("ReviewSessionEventRequest"),
                "responses":{"200":ok("ReviewSessionEvent"),"400":error.clone(),"404":error.clone(),"409":error.clone()}}
        },
        "/api/v1/projects/{projectId}/review-sessions/{sessionId}/finalize": {
            "post": {"operationId":"finalizeReviewSession","parameters":[project.clone(),session.clone()],"requestBody":body("FinalizeReviewSessionRequest"),
                "responses":{"200":ok("ReviewSessionDetail"),"400":error.clone(),"404":error.clone(),"409":error.clone()}}
        },
        "/api/v1/projects/{projectId}/review-sessions/{sessionId}/audio.wav": {
            "get": {"operationId":"getReviewSessionAudio","description":"WAV of a completed session only. Incomplete audio is never padded.",
                "parameters":[project,session],
                "responses":{"200":{"description":"WAV audio","content":{"audio/wav":{"schema":{"type":"string","format":"binary"}}}},"404":error.clone(),"409":error.clone()}}
        },
        "/api/v1/projects/{projectId}/review-sessions/{sessionId}/snippet.wav": {
            "get": {"operationId":"getReviewSessionSnippet","description":"Bounded WAV (at most 30 seconds) of stored audio from any non-deleted session, including recoverable spans of an incomplete one. A range touching missing audio is 409 review_session_audio_gap; audio is never padded.",
                "parameters":[project,session,
                    {"name":"startFrame","in":"query","required":true,"schema":{"type":"integer","minimum":0}},
                    {"name":"endFrame","in":"query","required":true,"schema":{"type":"integer","minimum":1}}],
                "responses":{"200":{"description":"WAV audio","content":{"audio/wav":{"schema":{"type":"string","format":"binary"}}}},"400":error.clone(),"404":error.clone(),"409":error}}
        }
    });
    let open = json!({"type":"object","additionalProperties":true});
    let schemas = json!({
        "CreateReviewSessionRequest": {"type":"object","additionalProperties":false,
            "required":["reviewTargetId","clientSessionKey","consent","audio","capture"],
            "properties":{
                "reviewTargetId":{"type":"string"},
                "clientSessionKey":{"type":"string","pattern":"^[A-Za-z0-9_-]{1,64}$"},
                "consent":{"type":"object","additionalProperties":false,"required":["allPresentAcknowledged","additionalSpeakersDisclosed"],
                    "properties":{"allPresentAcknowledged":{"type":"boolean","const":true},"additionalSpeakersDisclosed":{"type":"boolean"}}},
                "audio":{"type":"object","additionalProperties":false,"required":["sampleRate","channels","encoding"],
                    "properties":{"sampleRate":{"type":"integer"},"channels":{"type":"integer","minimum":1,"maximum":2},"encoding":{"const":"pcm_s16le"}}},
                "capture":{"type":"object","additionalProperties":false,"required":["clock"],"properties":{
                    "echoCancellation":{"type":["boolean","null"]},"noiseSuppression":{"type":["boolean","null"]},
                    "autoGainControl":{"type":["boolean","null"]},"clock":{"type":"string"},
                    "baseLatencySeconds":{"type":["number","null"]},"userAgentFamily":{"type":["string","null"]}}},
                "clientStartedPerformanceMs":{"type":["number","null"]}}},
        "ReviewSessionEventRequest": {"type":"object","additionalProperties":false,"required":["eventId","kind","audioFrame"],
            "properties":{"eventId":{"type":"string"},
                "kind":{"type":"string","enum":["manualSyncCue","captureDiscontinuity","localBufferOverflow","deviceLost","pageInterrupted","clockSample"]},
                "audioFrame":{"type":"integer","minimum":0},"performanceMs":{"type":["number","null"]},
                "declaredMovieSeconds":{"type":["number","null"]},"detail":{"type":["object","null"]}}},
        "FinalizeReviewSessionRequest": {"type":"object","additionalProperties":false,"required":["finalFrameCount","endReason"],
            "properties":{"finalFrameCount":{"type":"integer","minimum":0},"endReason":{"type":"string","enum":["stopped","deviceLost","interrupted","cancelled"]}}},
        "ReviewSessionChunkReceipt": open.clone(),
        "ReviewSessionEvent": open.clone(),
        "ReviewSessionDetail": open.clone(),
        "ReviewSessionPage": {"type":"object","required":["items"],"properties":{"items":{"type":"array","items":open},"nextBefore":{"type":["integer","null"]}}}
    });
    if let (Some(target), Some(source)) = (document["paths"].as_object_mut(), paths.as_object()) {
        target.extend(source.clone());
    }
    if let (Some(target), Some(source)) = (
        document["components"]["schemas"].as_object_mut(),
        schemas.as_object(),
    ) {
        target.extend(source.clone());
    }
}
