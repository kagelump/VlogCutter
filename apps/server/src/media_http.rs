//! Representation requests and queries, node job completion, and ranged media content.

use super::*;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RepresentationResponse {
    pub(super) request_id: String,
    pub(super) representation: Representation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reused: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct QueryRepresentationsRequest {
    pub(super) media_object_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct QueryRepresentationsResponse {
    pub(super) request_id: String,
    pub(super) representations: Vec<Representation>,
}

pub(super) async fn request_representation(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Json(request): Json<RepresentationRequest>,
) -> Response {
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
    match tokio::task::spawn_blocking(move || service.request(request)).await {
        Ok(Ok(result)) => {
            let status = if result.reused {
                StatusCode::OK
            } else {
                StatusCode::ACCEPTED
            };
            (
                status,
                Json(RepresentationResponse {
                    request_id: request_id.0,
                    representation: result.representation,
                    job_id: result.job_id.map(|id| id.to_string()),
                    reused: Some(result.reused),
                }),
            )
                .into_response()
        }
        Ok(Err(error)) => representation_error(request_id.0, error),
        Err(_) => representation_error(
            request_id.0,
            RepresentationError::InvalidInput("request worker failed".into()),
        ),
    }
}

pub(super) async fn get_representation(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(raw_id): AxumPath<String>,
) -> Response {
    let Ok(id) = RepresentationId::parse(raw_id) else {
        return representation_error(request_id.0, RepresentationError::NotFound);
    };
    let service = state
        .foundation
        .catalog()
        .representations(state.foundation.config().derived_dir.clone());
    match tokio::task::spawn_blocking(move || service.get(&id)).await {
        Ok(Ok(representation)) => Json(RepresentationResponse {
            request_id: request_id.0,
            representation,
            job_id: None,
            reused: None,
        })
        .into_response(),
        Ok(Err(error)) => representation_error(request_id.0, error),
        Err(_) => representation_error(request_id.0, RepresentationError::NotFound),
    }
}

pub(super) async fn query_representations(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Json(body): Json<QueryRepresentationsRequest>,
) -> Response {
    let service = state
        .foundation
        .catalog()
        .representations(state.foundation.config().derived_dir.clone());
    let media_ids: Vec<MediaObjectId> = match body
        .media_object_ids
        .iter()
        .map(MediaObjectId::parse)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(ids) => ids,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    request_id: request_id.0,
                    code: "invalid_media_object_id",
                }),
            )
                .into_response();
        }
    };
    match tokio::task::spawn_blocking(move || service.list_available(&media_ids)).await {
        Ok(Ok(representations)) => Json(QueryRepresentationsResponse {
            request_id: request_id.0,
            representations,
        })
        .into_response(),
        Ok(Err(error)) => representation_error(request_id.0, error),
        Err(_) => representation_error(
            request_id.0,
            RepresentationError::InvalidInput("query worker failed".into()),
        ),
    }
}

// ── Node-targeted job completion ─────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CompleteNodeJobRequest {
    pub(super) status: String,
    #[serde(default)]
    pub(super) output_identity: Option<String>,
    #[serde(default)]
    pub(super) details: serde_json::Value,
}

pub(super) async fn complete_node_job(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(job_id): AxumPath<String>,
    Json(body): Json<CompleteNodeJobRequest>,
) -> Response {
    // Only device principals may report job results
    let is_device = matches!(&principal, AuthPrincipal::Device { .. });
    if !is_device {
        return (
            StatusCode::FORBIDDEN,
            Json(ErrorResponse {
                request_id: request_id.0,
                code: "forbidden",
            }),
        )
            .into_response();
    }

    let Ok(job_id_parsed) = JobId::parse(&job_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                request_id: request_id.0,
                code: "invalid_job_id",
            }),
        )
            .into_response();
    };

    let jobs = state.foundation.jobs();
    let server_node_id = state.foundation.catalog().node_id().clone();
    let representations = state
        .foundation
        .catalog()
        .representations(state.foundation.config().derived_dir.clone());

    let status = body.status.clone();
    let output_identity = body.output_identity.clone();
    let details = body.details.clone();

    // Run the completion + representation registration on a blocking thread
    let result = tokio::task::spawn_blocking(move || {
        let completion = match status.as_str() {
            "succeeded" => NodeJobCompletion::Succeeded {
                output_identity: output_identity.clone(),
                details: details.clone(),
            },
            "failed" => NodeJobCompletion::Failed {
                code: "node_reported_failure".to_string(),
                details: details.clone(),
            },
            _ => {
                return Err(RepresentationError::InvalidInput(format!(
                    "unknown status: {status}",
                )));
            }
        };

        jobs.complete_node_targeted_job(&job_id_parsed, &server_node_id, &completion)
            .map_err(|e| RepresentationError::InvalidInput(e.to_string()))?;

        // If succeeded and the job was a representation generation, register the output
        if status == "succeeded"
            && let Some(ref output_id) = output_identity
            && let Ok(rep_id) = RepresentationId::parse(output_id)
        {
            let hash = details
                .get("outputContentHash")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let byte_len = details
                .get("byteLength")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let local_path = details
                .get("localPath")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if byte_len > 0 && !local_path.is_empty() {
                let hash = if hash.is_empty() {
                    "remote_generated"
                } else {
                    hash
                };
                let _ = representations.register_remote_output(
                    &rep_id,
                    &server_node_id,
                    local_path,
                    hash,
                    byte_len,
                );
            }
        }
        Ok::<_, RepresentationError>(())
    })
    .await;

    match result {
        Ok(Ok(())) => Json(serde_json::json!({
            "requestId": request_id.0,
            "status": "ok"
        }))
        .into_response(),
        Ok(Err(error)) => representation_error(request_id.0, error),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                request_id: request_id.0,
                code: "internal_error",
            }),
        )
            .into_response(),
    }
}

pub(super) async fn media_content(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(raw_id): AxumPath<String>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let Ok(id) = RepresentationId::parse(raw_id) else {
        return representation_error(request_id.0, RepresentationError::NotFound);
    };
    let service = state
        .foundation
        .catalog()
        .representations(state.foundation.config().derived_dir.clone());
    let lookup_id = id.clone();
    let lookup = tokio::task::spawn_blocking(move || {
        let representation = service.get(&lookup_id)?;
        let path = service.content_path(&lookup_id)?;
        Ok::<_, RepresentationError>((representation, path))
    })
    .await;
    let (representation, path) = match lookup {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => return representation_error(request_id.0, error),
        Err(_) => return representation_error(request_id.0, RepresentationError::NotFound),
    };
    let length = match tokio::fs::metadata(&path).await {
        Ok(metadata) => metadata.len(),
        Err(_) => return representation_error(request_id.0, RepresentationError::NotFound),
    };
    if representation.byte_length != Some(length) {
        return representation_error(request_id.0, RepresentationError::NotFound);
    }
    let selected = match parse_range(headers.get(header::RANGE), length) {
        Ok(value) => value,
        Err(()) => {
            let mut response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{length}")).expect("valid content-range"),
            );
            response
                .headers_mut()
                .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            return response;
        }
    };
    let (start, end, status) = selected.map_or(
        (0, length.saturating_sub(1), StatusCode::OK),
        |(start, end)| (start, end, StatusCode::PARTIAL_CONTENT),
    );
    let response_length = if length == 0 { 0 } else { end - start + 1 };
    let mut builder = Response::builder()
        .status(status)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, representation.content_type)
        .header(header::CONTENT_LENGTH, response_length.to_string())
        .header(header::ETAG, format!("\"{}\"", id.as_str()))
        .header(
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable",
        );
    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{length}"),
        );
    }
    if method == Method::HEAD {
        return builder.body(Body::empty()).expect("valid media headers");
    }
    let mut file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(_) => return representation_error(request_id.0, RepresentationError::NotFound),
    };
    if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return representation_error(request_id.0, RepresentationError::NotFound);
    }
    let stream = ReaderStream::new(file.take(response_length));
    builder
        .body(Body::from_stream(stream))
        .expect("valid media response")
}

pub(super) fn parse_range(
    value: Option<&HeaderValue>,
    length: u64,
) -> Result<Option<(u64, u64)>, ()> {
    let Some(value) = value else { return Ok(None) };
    let value = value.to_str().map_err(|_| ())?;
    let spec = value.strip_prefix("bytes=").ok_or(())?;
    if spec.contains(',') || length == 0 {
        return Err(());
    }
    let (start, end) = spec.split_once('-').ok_or(())?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        let selected = suffix.min(length);
        return Ok(Some((length - selected, length - 1)));
    }
    let start = start.parse::<u64>().map_err(|_| ())?;
    if start >= length {
        return Err(());
    }
    let end = if end.is_empty() {
        length - 1
    } else {
        end.parse::<u64>().map_err(|_| ())?.min(length - 1)
    };
    if end < start {
        return Err(());
    }
    Ok(Some((start, end)))
}

pub(super) fn representation_error(request_id: String, error: RepresentationError) -> Response {
    let status = match &error {
        RepresentationError::NotFound => StatusCode::NOT_FOUND,
        RepresentationError::InvalidInput(_) | RepresentationError::SourceNotVerified => {
            StatusCode::BAD_REQUEST
        }
        RepresentationError::MediaOffline | RepresentationError::DerivedRootUnavailable => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        RepresentationError::Database(_)
        | RepresentationError::Job(_)
        | RepresentationError::Infrastructure(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ErrorResponse {
            request_id,
            code: error.code(),
        }),
    )
        .into_response()
}

// ── Library and project browse ────────────────────────────────────────
