//! Library media listing and detail.

use super::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct LibraryMediaParams {
    #[serde(default = "default_page_limit")]
    pub(super) limit: u32,
    #[serde(default)]
    pub(super) sort: Option<LibrarySort>,
    #[serde(default)]
    pub(super) verification_state: Option<GlobalMediaVerificationState>,
    #[serde(default)]
    pub(super) storage_root_id: Option<String>,
    #[serde(default)]
    pub(super) camera: Option<String>,
    #[serde(default)]
    pub(super) filename_prefix: Option<String>,
    #[serde(default)]
    pub(super) availability: Option<NodeAvailabilityState>,
    #[serde(default)]
    pub(super) after: Option<String>,
}

pub(super) fn default_page_limit() -> u32 {
    50
}

pub(super) fn has_media_work_capacity(state: &HttpState) -> bool {
    fs2::available_space(&state.foundation.config().derived_dir)
        .is_ok_and(|bytes| bytes >= state.foundation.config().minimum_free_bytes)
        && fs2::available_space(&state.foundation.config().data_dir)
            .is_ok_and(|bytes| bytes >= state.foundation.config().minimum_free_bytes)
}

pub(super) fn query_error_response(
    request_id: &RequestId,
    code: &'static str,
    status: StatusCode,
) -> Response {
    assert!(
        rcdvlog_server_core::is_stable_error_code(code),
        "unregistered HTTP error code: {code}"
    );
    (
        status,
        Json(ErrorResponse {
            request_id: request_id.0.clone(),
            code,
        }),
    )
        .into_response()
}

pub(super) async fn list_library_media(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    params: Result<Query<LibraryMediaParams>, QueryRejection>,
) -> Response {
    let Query(params) = match params {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
        }
    };

    let storage_root_id = match params.storage_root_id.as_deref().map(StorageRootId::parse) {
        Some(Ok(value)) => Some(value),
        Some(Err(_)) => {
            return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
        }
        None => None,
    };

    let query = LibraryQuery {
        limit: params.limit,
        sort: params.sort.unwrap_or(LibrarySort::CreatedDesc),
        filters: LibraryFilters {
            verification_state: params.verification_state,
            storage_root_id,
            camera_folded: params.camera,
            filename_prefix: params.filename_prefix,
        },
        after: params.after,
    };

    match state
        .foundation
        .catalog()
        .storage_library()
        .search_media(query, params.availability)
    {
        Ok(page) => (StatusCode::OK, Json(page)).into_response(),
        Err(StorageLibraryError::Query(error)) => {
            query_error_response(&request_id, error.code(), StatusCode::BAD_REQUEST)
        }
        Err(StorageLibraryError::InvalidInput { .. }) => {
            query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST)
        }
        Err(StorageLibraryError::NotFound { .. }) => {
            query_error_response(&request_id, "not_found", StatusCode::NOT_FOUND)
        }
        Err(_) => query_error_response(
            &request_id,
            "internal_error",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

/// Combines the authoritative [`MediaObject`], its non-authoritative search
/// projection facts, and its node-relative availability into one detail
/// response. No field here is invented beyond what earlier tasks already
/// produce for a media object.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LibraryMediaDetail {
    pub(super) media: MediaObject,
    pub(super) facts: MediaSearchFacts,
    pub(super) availability: MediaAvailability,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct LibraryMediaDetailParams {}

pub(super) async fn get_library_media_detail(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    AxumPath(raw_media_object_id): AxumPath<String>,
    params: Result<Query<LibraryMediaDetailParams>, QueryRejection>,
) -> Response {
    if params.is_err() {
        return query_error_response(&request_id, "invalid_query", StatusCode::BAD_REQUEST);
    }
    let Ok(media_object_id) = MediaObjectId::parse(raw_media_object_id) else {
        return query_error_response(&request_id, "not_found", StatusCode::NOT_FOUND);
    };
    let library = state.foundation.catalog().storage_library();
    let node_id = state.foundation.catalog().node_id().clone();

    let media = match library.get_media_object(&media_object_id) {
        Ok(media) => media,
        Err(error) => return library_error_response(&request_id, error),
    };
    let facts = match library.media_search_facts(&media_object_id) {
        Ok(facts) => facts,
        Err(error) => return library_error_response(&request_id, error),
    };
    let availability = match library.media_availability(&media_object_id, &node_id) {
        Ok(availability) => availability,
        Err(error) => return library_error_response(&request_id, error),
    };

    (
        StatusCode::OK,
        Json(LibraryMediaDetail {
            media,
            facts,
            availability,
        }),
    )
        .into_response()
}

pub(super) fn library_error_response(
    request_id: &RequestId,
    error: StorageLibraryError,
) -> Response {
    match error {
        StorageLibraryError::Query(error) => {
            query_error_response(request_id, error.code(), StatusCode::BAD_REQUEST)
        }
        StorageLibraryError::InvalidInput { .. } => {
            query_error_response(request_id, "invalid_query", StatusCode::BAD_REQUEST)
        }
        StorageLibraryError::NotFound { .. } => {
            query_error_response(request_id, "not_found", StatusCode::NOT_FOUND)
        }
        _ => query_error_response(
            request_id,
            "internal_error",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}
