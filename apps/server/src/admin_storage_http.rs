//! Admin storage roots and bindings.

use super::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CreateStorageRootRequest {
    pub(super) display_name: String,
    #[serde(default = "default_true")]
    pub(super) immutable_original: bool,
}

pub(super) fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct UpdateStorageRootRequest {
    pub(super) display_name: Option<String>,
    pub(super) enabled: Option<bool>,
    #[serde(default)]
    pub(super) confirm_disable: bool,
}

pub(super) async fn list_admin_storage_roots(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    match state
        .foundation
        .catalog()
        .storage_library()
        .list_storage_roots(true)
    {
        Ok(items) => Json(serde_json::json!({ "items": items })).into_response(),
        Err(error) => storage_admin_error(&request_id, error),
    }
}

pub(super) async fn create_admin_storage_root(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    request: Result<Json<CreateStorageRootRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    let id = match StorageRootId::parse(format!("root_{}", Uuid::new_v4().simple())) {
        Ok(id) => id,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    match state
        .foundation
        .catalog()
        .storage_library()
        .create_storage_root(NewStorageRoot {
            id: id.clone(),
            display_name: request.display_name.clone(),
            immutable_original: request.immutable_original,
        }) {
        Ok(root) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "storage_root.created",
            "storage_root",
            Some(id.as_str()),
            serde_json::json!({"displayName": request.display_name, "immutableOriginal": request.immutable_original}),
        ) {
            Ok(()) => (StatusCode::CREATED, Json(root)).into_response(),
            Err(response) => *response,
        },
        Err(error) => storage_admin_error(&request_id, error),
    }
}

pub(super) async fn update_admin_storage_root(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(raw_id): AxumPath<String>,
    request: Result<Json<UpdateStorageRootRequest>, JsonRejection>,
) -> Response {
    let id = match StorageRootId::parse(raw_id) {
        Ok(id) => id,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    if request.enabled == Some(false) && !request.confirm_disable {
        return query_error_response(
            &request_id,
            "confirmation_required",
            StatusCode::BAD_REQUEST,
        );
    }
    match state
        .foundation
        .catalog()
        .storage_library()
        .update_storage_root(
            &id,
            StorageRootUpdate {
                display_name: request.display_name.clone(),
                enabled: request.enabled,
            },
        ) {
        Ok(root) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "storage_root.updated",
            "storage_root",
            Some(id.as_str()),
            serde_json::json!({"displayNameChanged": request.display_name.is_some(), "enabled": request.enabled}),
        ) {
            Ok(()) => Json(root).into_response(),
            Err(response) => *response,
        },
        Err(error) => storage_admin_error(&request_id, error),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct UpsertBindingRequest {
    pub(super) local_mount_path: String,
    pub(super) root_subpath: Option<RootRelativePath>,
    pub(super) protocol: StorageProtocol,
    pub(super) access_mode: StorageAccessMode,
    #[serde(default = "default_true")]
    pub(super) enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ConfirmBindingRequest {
    pub(super) action: BindingConfirmationAction,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum BindingConfirmationAction {
    Confirm,
    Reject,
}

pub(super) async fn list_admin_storage_bindings(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    match state
        .foundation
        .catalog()
        .storage_library()
        .list_local_node_root_bindings_admin()
    {
        Ok(items) => Json(serde_json::json!({ "items": items })).into_response(),
        Err(error) => storage_admin_error(&request_id, error),
    }
}

pub(super) async fn upsert_admin_storage_binding(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(raw_id): AxumPath<String>,
    request: Result<Json<UpsertBindingRequest>, JsonRejection>,
) -> Response {
    let root_id = match StorageRootId::parse(raw_id) {
        Ok(id) => id,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    let mount = PathBuf::from(&request.local_mount_path);
    if !admin_mount_is_allowed(state.foundation.config(), &mount) {
        return query_error_response(
            &request_id,
            "mount_path_not_allowed",
            StatusCode::BAD_REQUEST,
        );
    }
    let input = LocalNodeRootBindingInput {
        storage_root_id: root_id.clone(),
        local_mount_path: mount,
        root_subpath: request.root_subpath,
        protocol: request.protocol,
        access_mode: request.access_mode,
        verification_status: BindingVerificationStatus::Pending,
        enabled: request.enabled,
        association_evidence: None,
        confirmation: RootAssociationConfirmation {
            status: RootAssociationConfirmationStatus::Pending,
            confirmed_by_node_id: None,
            confirmed_at: None,
        },
    };
    match state
        .foundation
        .catalog()
        .storage_library()
        .upsert_local_node_root_binding(input)
    {
        Ok(binding) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "storage_binding.configured",
            "storage_root",
            Some(root_id.as_str()),
            serde_json::json!({"enabled": binding.enabled, "protocol": binding.protocol.as_str(), "accessMode": binding.access_mode.as_str(), "evidencePresent": binding.association_evidence.is_some()}),
        ) {
            Ok(()) => Json(binding).into_response(),
            Err(response) => *response,
        },
        Err(error) => storage_admin_error(&request_id, error),
    }
}

pub(super) async fn verify_admin_storage_binding(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(raw_id): AxumPath<String>,
) -> Response {
    let root_id = match StorageRootId::parse(raw_id) {
        Ok(id) => id,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    let library = state.foundation.catalog().storage_library();
    let existing = match library.get_local_node_root_binding_admin(&root_id) {
        Ok(value) => value,
        Err(error) => return storage_admin_error(&request_id, error),
    };
    let evidence = match library.inspect_local_root_association(
        &root_id,
        &existing.local_mount_path,
        existing.root_subpath.as_ref(),
    ) {
        Ok(value) => value,
        Err(error) => return storage_admin_error(&request_id, error),
    };
    let verification_status = if evidence.mismatches == 0 && evidence.matching_relative_paths > 0 {
        BindingVerificationStatus::Pending
    } else {
        BindingVerificationStatus::Mismatch
    };
    let input = LocalNodeRootBindingInput {
        storage_root_id: root_id.clone(),
        local_mount_path: existing.local_mount_path,
        root_subpath: existing.root_subpath,
        protocol: existing.protocol,
        access_mode: existing.access_mode,
        verification_status,
        enabled: existing.enabled,
        association_evidence: Some(evidence.clone()),
        confirmation: RootAssociationConfirmation {
            status: RootAssociationConfirmationStatus::Pending,
            confirmed_by_node_id: None,
            confirmed_at: None,
        },
    };
    match library.upsert_local_node_root_binding(input) {
        Ok(binding) => match audit_admin(
            &state,
            &principal,
            &request_id,
            "storage_binding.verified",
            "storage_root",
            Some(root_id.as_str()),
            serde_json::json!({"associationEvidence": evidence}),
        ) {
            Ok(()) => Json(binding).into_response(),
            Err(response) => *response,
        },
        Err(error) => storage_admin_error(&request_id, error),
    }
}

pub(super) async fn confirm_admin_storage_binding(
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(principal): Extension<AuthPrincipal>,
    AxumPath(raw_id): AxumPath<String>,
    request: Result<Json<ConfirmBindingRequest>, JsonRejection>,
) -> Response {
    let root_id = match StorageRootId::parse(raw_id) {
        Ok(id) => id,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    let Json(request) = match request {
        Ok(value) => value,
        Err(_) => {
            return query_error_response(&request_id, "invalid_input", StatusCode::BAD_REQUEST);
        }
    };
    let library = state.foundation.catalog().storage_library();
    let existing = match library.get_local_node_root_binding_admin(&root_id) {
        Ok(value) => value,
        Err(error) => return storage_admin_error(&request_id, error),
    };
    if matches!(request.action, BindingConfirmationAction::Confirm)
        && existing
            .association_evidence
            .as_ref()
            .is_none_or(|evidence| evidence.matching_relative_paths == 0 || evidence.mismatches > 0)
    {
        return query_error_response(
            &request_id,
            "association_evidence_required",
            StatusCode::BAD_REQUEST,
        );
    }
    let confirmed = matches!(request.action, BindingConfirmationAction::Confirm);
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let input = LocalNodeRootBindingInput {
        storage_root_id: root_id.clone(),
        local_mount_path: existing.local_mount_path,
        root_subpath: existing.root_subpath,
        protocol: existing.protocol,
        access_mode: existing.access_mode,
        verification_status: if confirmed {
            BindingVerificationStatus::Confirmed
        } else {
            BindingVerificationStatus::Rejected
        },
        enabled: existing.enabled,
        association_evidence: existing.association_evidence,
        confirmation: RootAssociationConfirmation {
            status: if confirmed {
                RootAssociationConfirmationStatus::Confirmed
            } else {
                RootAssociationConfirmationStatus::Rejected
            },
            confirmed_by_node_id: confirmed.then(|| state.foundation.catalog().node_id().clone()),
            confirmed_at: Some(now),
        },
    };
    match library.upsert_local_node_root_binding(input) {
        Ok(binding) => match audit_admin(
            &state,
            &principal,
            &request_id,
            if confirmed {
                "storage_binding.confirmed"
            } else {
                "storage_binding.rejected"
            },
            "storage_root",
            Some(root_id.as_str()),
            serde_json::json!({"evidence": binding.association_evidence}),
        ) {
            Ok(()) => Json(binding).into_response(),
            Err(response) => *response,
        },
        Err(error) => storage_admin_error(&request_id, error),
    }
}

pub(super) fn admin_mount_is_allowed(
    config: &rcdvlog_server_core::ServerConfig,
    mount: &Path,
) -> bool {
    if !mount.is_absolute()
        || mount
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return false;
    }
    let Ok(resolved_mount) = mount.canonicalize() else {
        return false;
    };
    config.allowed_original_mounts.iter().any(|allowed| {
        allowed
            .canonicalize()
            .is_ok_and(|resolved_allowed| resolved_mount.starts_with(resolved_allowed))
    })
}

pub(super) fn storage_admin_error(request_id: &RequestId, error: StorageLibraryError) -> Response {
    match error {
        StorageLibraryError::InvalidInput { .. } | StorageLibraryError::Path(_) => {
            query_error_response(request_id, "invalid_input", StatusCode::BAD_REQUEST)
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
