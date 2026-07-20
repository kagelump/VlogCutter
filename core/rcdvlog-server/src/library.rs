use chrono::{SecondsFormat, Utc};
use rcdvlog_core::{
    BindingVerificationStatus, GlobalMediaVerificationState, LocationVerificationState,
    MediaObjectId, NodeAvailabilityState, NodeId, ObservationGenerationState, ObservationStatus,
    OperationId, ProjectAssetId, ProjectId, RootAssociationConfirmation,
    RootAssociationConfirmationStatus, RootAssociationEvidence, RootRelativePath,
    StorageAccessMode, StorageObjectId, StorageProtocol, StorageRootId,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

use crate::catalog::{open_read_only_connection, open_writable_connection};
use crate::storage_path::{
    PathAccessIntent, PathResolutionError, resolve_existing_binding_path, validate_local_mount_path,
};

const MAX_PAGE_SIZE: u32 = 200;
const MAX_DISPLAY_NAME_BYTES: usize = 255;

#[derive(Debug, Error)]
pub enum StorageLibraryError {
    #[error("invalid {field}: {reason}")]
    InvalidInput { field: &'static str, reason: String },
    #[error("{entity} {id} was not found")]
    NotFound { entity: &'static str, id: String },
    #[error("observation generation {generation_id} is {state}, expected in_progress")]
    InvalidGenerationState {
        generation_id: String,
        state: String,
    },
    #[error(
        "storage object {storage_object_id} already has an observation in generation {generation_id}"
    )]
    ObservationAlreadyRecorded {
        generation_id: String,
        storage_object_id: String,
    },
    #[error("storage object {storage_object_id} is outside observation generation scope")]
    ObservationOutsideScope { storage_object_id: String },
    #[error(transparent)]
    Path(#[from] PathResolutionError),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Infrastructure(#[from] anyhow::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageRoot {
    pub id: StorageRootId,
    pub display_name: String,
    pub immutable_original: bool,
    pub enabled: bool,
    pub disabled_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct NewStorageRoot {
    pub id: StorageRootId,
    pub display_name: String,
    pub immutable_original: bool,
}

#[derive(Debug, Clone, Default)]
pub struct StorageRootUpdate {
    pub display_name: Option<String>,
    pub enabled: Option<bool>,
}

/// Local administrative projection. Its absolute path must never be copied into
/// a project operation or synchronization payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeRootBindingAdmin {
    pub node_id: NodeId,
    pub storage_root_id: StorageRootId,
    pub local_mount_path: PathBuf,
    pub root_subpath: Option<RootRelativePath>,
    pub protocol: StorageProtocol,
    pub access_mode: StorageAccessMode,
    pub verification_status: BindingVerificationStatus,
    pub enabled: bool,
    pub association_evidence: Option<RootAssociationEvidence>,
    pub confirmation: RootAssociationConfirmation,
    pub created_at: String,
    pub updated_at: String,
}

/// Synchronization-safe statement that a node can access a logical root. It
/// deliberately has no node-local absolute mount path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeRootCapability {
    pub node_id: NodeId,
    pub storage_root_id: StorageRootId,
    pub root_subpath: Option<RootRelativePath>,
    pub protocol: StorageProtocol,
    pub access_mode: StorageAccessMode,
    pub verification_status: BindingVerificationStatus,
    pub enabled: bool,
    pub association_evidence: Option<RootAssociationEvidence>,
    pub confirmation: RootAssociationConfirmation,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct NodeRootCapabilityInput {
    pub node_id: NodeId,
    pub storage_root_id: StorageRootId,
    pub root_subpath: Option<RootRelativePath>,
    pub protocol: StorageProtocol,
    pub access_mode: StorageAccessMode,
    pub verification_status: BindingVerificationStatus,
    pub enabled: bool,
    pub association_evidence: Option<RootAssociationEvidence>,
    pub confirmation: RootAssociationConfirmation,
}

#[derive(Debug, Clone)]
pub struct LocalNodeRootBindingInput {
    pub storage_root_id: StorageRootId,
    pub local_mount_path: PathBuf,
    pub root_subpath: Option<RootRelativePath>,
    pub protocol: StorageProtocol,
    pub access_mode: StorageAccessMode,
    pub verification_status: BindingVerificationStatus,
    pub enabled: bool,
    pub association_evidence: Option<RootAssociationEvidence>,
    pub confirmation: RootAssociationConfirmation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaObject {
    pub id: MediaObjectId,
    pub size_bytes: Option<u64>,
    pub quick_fingerprint: Option<String>,
    pub full_content_hash: Option<String>,
    pub verification_state: GlobalMediaVerificationState,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct NewMediaObject {
    pub id: MediaObjectId,
    pub size_bytes: Option<u64>,
    pub quick_fingerprint: Option<String>,
    pub full_content_hash: Option<String>,
    pub verification_state: GlobalMediaVerificationState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageObject {
    pub id: StorageObjectId,
    pub storage_root_id: StorageRootId,
    pub relative_path: RootRelativePath,
    pub size_bytes: Option<u64>,
    pub modified_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct NewStorageObject {
    pub id: StorageObjectId,
    pub storage_root_id: StorageRootId,
    pub relative_path: RootRelativePath,
    pub size_bytes: Option<u64>,
    pub modified_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaObjectLocation {
    pub media_object_id: MediaObjectId,
    pub storage_object_id: StorageObjectId,
    pub verification_state: LocationVerificationState,
    pub linked_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectMediaMembershipProjection {
    pub project_id: ProjectId,
    pub project_asset_id: ProjectAssetId,
    pub media_object_id: MediaObjectId,
    pub source_operation_id: OperationId,
    pub projected_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryCursor {
    pub created_at: String,
    pub media_object_id: MediaObjectId,
}

#[derive(Debug, Clone)]
pub struct MediaLibraryQuery {
    pub limit: u32,
    pub after: Option<LibraryCursor>,
    pub verification_state: Option<GlobalMediaVerificationState>,
}

impl Default for MediaLibraryQuery {
    fn default() -> Self {
        Self {
            limit: 50,
            after: None,
            verification_state: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaLibraryItem {
    pub media: MediaObject,
    pub storage_location_count: u64,
    pub project_membership_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaLibraryPage {
    pub items: Vec<MediaLibraryItem>,
    pub next_cursor: Option<LibraryCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationScope {
    pub storage_root_id: StorageRootId,
    pub relative_prefix: Option<RootRelativePath>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationGeneration {
    pub id: String,
    pub node_id: NodeId,
    pub sequence: u64,
    pub scope: ObservationScope,
    pub state: ObservationGenerationState,
    pub started_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AssetObservationInput {
    pub storage_object_id: StorageObjectId,
    pub status: ObservationStatus,
    pub observed_size_bytes: Option<u64>,
    pub observed_quick_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageLocationAvailability {
    pub node_id: NodeId,
    pub storage_object_id: StorageObjectId,
    pub state: NodeAvailabilityState,
    pub completed_generation_id: Option<String>,
    pub completed_generation_sequence: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaAvailability {
    pub media_object_id: MediaObjectId,
    pub queried_node_id: NodeId,
    pub state: NodeAvailabilityState,
    pub locations: Vec<StorageLocationAvailability>,
}

#[derive(Debug, Clone)]
pub struct StorageLibrary {
    database_path: PathBuf,
    local_node_id: NodeId,
}

impl StorageLibrary {
    pub(crate) fn new(database_path: PathBuf, local_node_id: NodeId) -> Self {
        Self {
            database_path,
            local_node_id,
        }
    }

    pub fn local_node_id(&self) -> &NodeId {
        &self.local_node_id
    }

    pub fn create_storage_root(
        &self,
        input: NewStorageRoot,
    ) -> Result<StorageRoot, StorageLibraryError> {
        validate_display_name(&input.display_name)?;
        let now = now();
        let connection = open_writable_connection(&self.database_path)?;
        connection.execute(
            r#"
            INSERT INTO storage_roots (
                storage_root_id, display_name, immutable_original, enabled,
                disabled_at, created_at, updated_at
            ) VALUES (?1, ?2, ?3, 1, NULL, ?4, ?4)
            "#,
            params![
                input.id.as_str(),
                input.display_name,
                input.immutable_original,
                now,
            ],
        )?;
        self.get_storage_root(&input.id)
    }

    pub fn get_storage_root(&self, id: &StorageRootId) -> Result<StorageRoot, StorageLibraryError> {
        let connection = open_read_only_connection(&self.database_path)?;
        connection
            .query_row(
                r#"
                SELECT storage_root_id, display_name, immutable_original, enabled,
                       disabled_at, created_at, updated_at
                FROM storage_roots
                WHERE storage_root_id = ?1
                "#,
                [id.as_str()],
                storage_root_from_row,
            )
            .optional()?
            .ok_or_else(|| not_found("storage root", id.as_str()))
    }

    pub fn list_storage_roots(
        &self,
        include_disabled: bool,
    ) -> Result<Vec<StorageRoot>, StorageLibraryError> {
        let connection = open_read_only_connection(&self.database_path)?;
        let mut statement = connection.prepare(
            r#"
            SELECT storage_root_id, display_name, immutable_original, enabled,
                   disabled_at, created_at, updated_at
            FROM storage_roots
            WHERE enabled = 1 OR ?1
            ORDER BY display_name COLLATE NOCASE, storage_root_id
            "#,
        )?;
        Ok(statement
            .query_map([include_disabled], storage_root_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn update_storage_root(
        &self,
        id: &StorageRootId,
        update: StorageRootUpdate,
    ) -> Result<StorageRoot, StorageLibraryError> {
        if let Some(name) = &update.display_name {
            validate_display_name(name)?;
        }
        let existing = self.get_storage_root(id)?;
        let display_name = update.display_name.unwrap_or(existing.display_name);
        let enabled = update.enabled.unwrap_or(existing.enabled);
        let disabled_at = if enabled {
            None
        } else {
            existing.disabled_at.or_else(|| Some(now()))
        };
        let connection = open_writable_connection(&self.database_path)?;
        let updated = connection.execute(
            r#"
            UPDATE storage_roots
            SET display_name = ?2, enabled = ?3, disabled_at = ?4, updated_at = ?5
            WHERE storage_root_id = ?1
            "#,
            params![id.as_str(), display_name, enabled, disabled_at, now()],
        )?;
        if updated == 0 {
            return Err(not_found("storage root", id.as_str()));
        }
        self.get_storage_root(id)
    }

    pub fn disable_storage_root(
        &self,
        id: &StorageRootId,
    ) -> Result<StorageRoot, StorageLibraryError> {
        self.update_storage_root(
            id,
            StorageRootUpdate {
                display_name: None,
                enabled: Some(false),
            },
        )
    }

    pub fn upsert_node_root_capability(
        &self,
        input: NodeRootCapabilityInput,
    ) -> Result<NodeRootCapability, StorageLibraryError> {
        let node_id = input.node_id.clone();
        let root_id = input.storage_root_id.clone();
        self.upsert_node_root_record(input, None)?;
        self.get_node_root_capability(&node_id, &root_id)
    }

    pub fn upsert_local_node_root_binding(
        &self,
        input: LocalNodeRootBindingInput,
    ) -> Result<NodeRootBindingAdmin, StorageLibraryError> {
        validate_local_mount_path(&input.local_mount_path)?;
        let mount = input
            .local_mount_path
            .to_str()
            .ok_or_else(|| StorageLibraryError::InvalidInput {
                field: "localMountPath",
                reason: "must be valid Unicode".to_string(),
            })?
            .to_string();
        let root_id = input.storage_root_id.clone();
        self.upsert_node_root_record(
            NodeRootCapabilityInput {
                node_id: self.local_node_id.clone(),
                storage_root_id: input.storage_root_id,
                root_subpath: input.root_subpath,
                protocol: input.protocol,
                access_mode: input.access_mode,
                verification_status: input.verification_status,
                enabled: input.enabled,
                association_evidence: input.association_evidence,
                confirmation: input.confirmation,
            },
            Some(&mount),
        )?;
        self.get_local_node_root_binding_admin(&root_id)
    }

    fn upsert_node_root_record(
        &self,
        input: NodeRootCapabilityInput,
        local_mount_path: Option<&str>,
    ) -> Result<(), StorageLibraryError> {
        if local_mount_path.is_some() && input.node_id != self.local_node_id {
            return Err(StorageLibraryError::InvalidInput {
                field: "nodeId",
                reason: "only the local server node may own a mount path in this catalog"
                    .to_string(),
            });
        }
        let root = self.get_storage_root(&input.storage_root_id)?;
        if root.immutable_original && input.access_mode != StorageAccessMode::ReadOnly {
            return Err(StorageLibraryError::InvalidInput {
                field: "accessMode",
                reason: "immutable original roots require read_only bindings".to_string(),
            });
        }
        validate_confirmation(&input.verification_status, &input.confirmation)?;
        if input
            .association_evidence
            .as_ref()
            .is_some_and(|evidence| evidence.sampled_relative_paths.len() > 32)
        {
            return Err(StorageLibraryError::InvalidInput {
                field: "associationEvidence.sampledRelativePaths",
                reason: "cannot contain more than 32 diagnostic samples".to_string(),
            });
        }
        let evidence_json = input
            .association_evidence
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let now = now();
        let connection = open_writable_connection(&self.database_path)?;
        connection.execute(
            r#"
            INSERT INTO node_root_bindings (
                node_id, storage_root_id, local_mount_path, root_subpath, protocol,
                access_mode, verification_status, enabled, association_evidence_json,
                confirmation_status, confirmed_by_node_id, confirmed_at, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)
            ON CONFLICT(node_id, storage_root_id) DO UPDATE SET
                local_mount_path = COALESCE(excluded.local_mount_path, node_root_bindings.local_mount_path),
                root_subpath = excluded.root_subpath,
                protocol = excluded.protocol,
                access_mode = excluded.access_mode,
                verification_status = excluded.verification_status,
                enabled = excluded.enabled,
                association_evidence_json = excluded.association_evidence_json,
                confirmation_status = excluded.confirmation_status,
                confirmed_by_node_id = excluded.confirmed_by_node_id,
                confirmed_at = excluded.confirmed_at,
                updated_at = excluded.updated_at
            "#,
            params![
                input.node_id.as_str(),
                input.storage_root_id.as_str(),
                local_mount_path,
                input.root_subpath.as_ref().map(RootRelativePath::as_str),
                input.protocol.as_str(),
                input.access_mode.as_str(),
                input.verification_status.as_str(),
                input.enabled,
                evidence_json,
                input.confirmation.status.as_str(),
                input
                    .confirmation
                    .confirmed_by_node_id
                    .as_ref()
                    .map(NodeId::as_str),
                input.confirmation.confirmed_at,
                now,
            ],
        )?;
        Ok(())
    }

    pub fn get_node_root_capability(
        &self,
        node_id: &NodeId,
        root_id: &StorageRootId,
    ) -> Result<NodeRootCapability, StorageLibraryError> {
        let connection = open_read_only_connection(&self.database_path)?;
        connection
            .query_row(
                r#"
                SELECT node_id, storage_root_id, root_subpath, protocol,
                       access_mode, verification_status, enabled, association_evidence_json,
                       confirmation_status, confirmed_by_node_id, confirmed_at,
                       created_at, updated_at
                FROM node_root_bindings
                WHERE node_id = ?1 AND storage_root_id = ?2
                "#,
                params![node_id.as_str(), root_id.as_str()],
                capability_from_row,
            )
            .optional()?
            .ok_or_else(|| not_found("node root capability", &format!("{node_id}/{root_id}")))
    }

    pub fn list_node_root_capabilities(
        &self,
        root_id: Option<&StorageRootId>,
    ) -> Result<Vec<NodeRootCapability>, StorageLibraryError> {
        let connection = open_read_only_connection(&self.database_path)?;
        let sql = r#"
            SELECT node_id, storage_root_id, root_subpath, protocol,
                   access_mode, verification_status, enabled, association_evidence_json,
                   confirmation_status, confirmed_by_node_id, confirmed_at,
                   created_at, updated_at
            FROM node_root_bindings
            WHERE ?1 IS NULL OR storage_root_id = ?1
            ORDER BY storage_root_id, node_id
        "#;
        let mut statement = connection.prepare(sql)?;
        Ok(statement
            .query_map([root_id.map(StorageRootId::as_str)], capability_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get_local_node_root_binding_admin(
        &self,
        root_id: &StorageRootId,
    ) -> Result<NodeRootBindingAdmin, StorageLibraryError> {
        let connection = open_read_only_connection(&self.database_path)?;
        connection
            .query_row(
                r#"
                SELECT node_id, storage_root_id, local_mount_path, root_subpath, protocol,
                       access_mode, verification_status, enabled, association_evidence_json,
                       confirmation_status, confirmed_by_node_id, confirmed_at,
                       created_at, updated_at
                FROM node_root_bindings
                WHERE node_id = ?1 AND storage_root_id = ?2 AND local_mount_path IS NOT NULL
                "#,
                params![self.local_node_id.as_str(), root_id.as_str()],
                binding_from_row,
            )
            .optional()?
            .ok_or_else(|| {
                not_found(
                    "local node root binding",
                    &format!("{}/{root_id}", self.local_node_id),
                )
            })
    }

    pub fn list_local_node_root_bindings_admin(
        &self,
    ) -> Result<Vec<NodeRootBindingAdmin>, StorageLibraryError> {
        let connection = open_read_only_connection(&self.database_path)?;
        let mut statement = connection.prepare(
            r#"
            SELECT node_id, storage_root_id, local_mount_path, root_subpath, protocol,
                   access_mode, verification_status, enabled, association_evidence_json,
                   confirmation_status, confirmed_by_node_id, confirmed_at,
                   created_at, updated_at
            FROM node_root_bindings
            WHERE node_id = ?1 AND local_mount_path IS NOT NULL
            ORDER BY storage_root_id
            "#,
        )?;
        Ok(statement
            .query_map([self.local_node_id.as_str()], binding_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn set_node_root_binding_enabled(
        &self,
        node_id: &NodeId,
        root_id: &StorageRootId,
        enabled: bool,
    ) -> Result<NodeRootCapability, StorageLibraryError> {
        let connection = open_writable_connection(&self.database_path)?;
        let updated = connection.execute(
            r#"
            UPDATE node_root_bindings SET enabled = ?3, updated_at = ?4
            WHERE node_id = ?1 AND storage_root_id = ?2
            "#,
            params![node_id.as_str(), root_id.as_str(), enabled, now()],
        )?;
        if updated == 0 {
            return Err(not_found(
                "node root capability",
                &format!("{node_id}/{root_id}"),
            ));
        }
        self.get_node_root_capability(node_id, root_id)
    }

    pub fn resolve_local_storage_object(
        &self,
        storage_object_id: &StorageObjectId,
        intent: PathAccessIntent,
    ) -> Result<PathBuf, StorageLibraryError> {
        let object = self.get_storage_object(storage_object_id)?;
        let root = self.get_storage_root(&object.storage_root_id)?;
        let binding = self.get_local_node_root_binding_admin(&object.storage_root_id)?;
        if !root.enabled
            || !binding.enabled
            || binding.verification_status != BindingVerificationStatus::Confirmed
            || binding.confirmation.status != RootAssociationConfirmationStatus::Confirmed
        {
            return Err(StorageLibraryError::InvalidInput {
                field: "binding",
                reason: "storage resolution requires an enabled, explicitly confirmed binding"
                    .to_string(),
            });
        }
        Ok(resolve_existing_binding_path(
            &binding.local_mount_path,
            binding.root_subpath.as_ref(),
            &object.relative_path,
            root.immutable_original,
            binding.access_mode,
            intent,
        )?)
    }

    pub fn create_media_object(
        &self,
        input: NewMediaObject,
    ) -> Result<MediaObject, StorageLibraryError> {
        validate_media_identity(&input)?;
        let now = now();
        let connection = open_writable_connection(&self.database_path)?;
        connection.execute(
            r#"
            INSERT INTO media_objects (
                media_object_id, size_bytes, quick_fingerprint, full_content_hash,
                verification_state, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
            "#,
            params![
                input.id.as_str(),
                to_i64(input.size_bytes, "sizeBytes")?,
                input.quick_fingerprint,
                input.full_content_hash,
                input.verification_state.as_str(),
                now,
            ],
        )?;
        self.get_media_object(&input.id)
    }

    pub fn get_media_object(&self, id: &MediaObjectId) -> Result<MediaObject, StorageLibraryError> {
        let connection = open_read_only_connection(&self.database_path)?;
        connection
            .query_row(
                r#"
                SELECT media_object_id, size_bytes, quick_fingerprint, full_content_hash,
                       verification_state, created_at, updated_at
                FROM media_objects WHERE media_object_id = ?1
                "#,
                [id.as_str()],
                media_object_from_row,
            )
            .optional()?
            .ok_or_else(|| not_found("media object", id.as_str()))
    }

    pub fn create_storage_object(
        &self,
        input: NewStorageObject,
    ) -> Result<StorageObject, StorageLibraryError> {
        self.get_storage_root(&input.storage_root_id)?;
        let now = now();
        let connection = open_writable_connection(&self.database_path)?;
        connection.execute(
            r#"
            INSERT INTO storage_objects (
                storage_object_id, storage_root_id, relative_path, size_bytes,
                modified_at, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
            "#,
            params![
                input.id.as_str(),
                input.storage_root_id.as_str(),
                input.relative_path.as_str(),
                to_i64(input.size_bytes, "sizeBytes")?,
                input.modified_at,
                now,
            ],
        )?;
        self.get_storage_object(&input.id)
    }

    pub fn get_storage_object(
        &self,
        id: &StorageObjectId,
    ) -> Result<StorageObject, StorageLibraryError> {
        let connection = open_read_only_connection(&self.database_path)?;
        connection
            .query_row(
                r#"
                SELECT storage_object_id, storage_root_id, relative_path, size_bytes,
                       modified_at, created_at, updated_at
                FROM storage_objects WHERE storage_object_id = ?1
                "#,
                [id.as_str()],
                storage_object_from_row,
            )
            .optional()?
            .ok_or_else(|| not_found("storage object", id.as_str()))
    }

    pub fn link_media_object_location(
        &self,
        media_object_id: &MediaObjectId,
        storage_object_id: &StorageObjectId,
        verification_state: LocationVerificationState,
    ) -> Result<MediaObjectLocation, StorageLibraryError> {
        let media = self.get_media_object(media_object_id)?;
        self.get_storage_object(storage_object_id)?;
        if verification_state == LocationVerificationState::Verified
            && media.verification_state != GlobalMediaVerificationState::Verified
        {
            return Err(StorageLibraryError::InvalidInput {
                field: "verificationState",
                reason: "a location cannot be verified before its media object".to_string(),
            });
        }
        let now = now();
        let connection = open_writable_connection(&self.database_path)?;
        connection.execute(
            r#"
            INSERT INTO media_object_locations (
                media_object_id, storage_object_id, verification_state, linked_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?4)
            ON CONFLICT(media_object_id, storage_object_id) DO UPDATE SET
                verification_state = excluded.verification_state,
                updated_at = excluded.updated_at
            "#,
            params![
                media_object_id.as_str(),
                storage_object_id.as_str(),
                verification_state.as_str(),
                now,
            ],
        )?;
        connection
            .query_row(
                r#"
            SELECT media_object_id, storage_object_id, verification_state, linked_at, updated_at
            FROM media_object_locations
            WHERE media_object_id = ?1 AND storage_object_id = ?2
            "#,
                params![media_object_id.as_str(), storage_object_id.as_str()],
                location_from_row,
            )
            .map_err(Into::into)
    }

    pub fn upsert_project_media_membership_projection(
        &self,
        projection: ProjectMediaMembershipProjection,
    ) -> Result<ProjectMediaMembershipProjection, StorageLibraryError> {
        self.get_media_object(&projection.media_object_id)?;
        let connection = open_writable_connection(&self.database_path)?;
        connection.execute(
            r#"
            INSERT INTO project_media_memberships (
                project_id, project_asset_id, media_object_id, source_operation_id, projected_at
            ) VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(project_id, project_asset_id) DO UPDATE SET
                media_object_id = excluded.media_object_id,
                source_operation_id = excluded.source_operation_id,
                projected_at = excluded.projected_at
            "#,
            params![
                projection.project_id.as_str(),
                projection.project_asset_id.as_str(),
                projection.media_object_id.as_str(),
                projection.source_operation_id.as_str(),
                projection.projected_at,
            ],
        )?;
        self.get_project_media_membership_projection(
            &projection.project_id,
            &projection.project_asset_id,
        )
    }

    pub fn get_project_media_membership_projection(
        &self,
        project_id: &ProjectId,
        asset_id: &ProjectAssetId,
    ) -> Result<ProjectMediaMembershipProjection, StorageLibraryError> {
        let connection = open_read_only_connection(&self.database_path)?;
        connection
            .query_row(
                r#"
                SELECT project_id, project_asset_id, media_object_id,
                       source_operation_id, projected_at
                FROM project_media_memberships
                WHERE project_id = ?1 AND project_asset_id = ?2
                "#,
                params![project_id.as_str(), asset_id.as_str()],
                membership_from_row,
            )
            .optional()?
            .ok_or_else(|| {
                not_found(
                    "project media membership projection",
                    &format!("{project_id}/{asset_id}"),
                )
            })
    }

    pub fn list_media(
        &self,
        query: MediaLibraryQuery,
    ) -> Result<MediaLibraryPage, StorageLibraryError> {
        if query.limit == 0 || query.limit > MAX_PAGE_SIZE {
            return Err(StorageLibraryError::InvalidInput {
                field: "limit",
                reason: format!("must be between 1 and {MAX_PAGE_SIZE}"),
            });
        }
        let connection = open_read_only_connection(&self.database_path)?;
        let fetch_limit = i64::from(query.limit) + 1;
        let (after_created, after_id) = query
            .after
            .as_ref()
            .map(|cursor| {
                (
                    Some(cursor.created_at.as_str()),
                    Some(cursor.media_object_id.as_str()),
                )
            })
            .unwrap_or((None, None));
        let mut items = if let Some(state) = query.verification_state {
            let mut statement = connection.prepare(LIBRARY_QUERY_WITH_STATE)?;
            statement
                .query_map(
                    params![state.as_str(), after_created, after_id, fetch_limit],
                    library_item_from_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            let mut statement = connection.prepare(LIBRARY_QUERY_ALL)?;
            statement
                .query_map(
                    params![after_created, after_id, fetch_limit],
                    library_item_from_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let has_more = items.len() > query.limit as usize;
        items.truncate(query.limit as usize);
        let next_cursor = has_more.then(|| {
            let last = items.last().expect("a page with more rows has a last item");
            LibraryCursor {
                created_at: last.media.created_at.clone(),
                media_object_id: last.media.id.clone(),
            }
        });
        Ok(MediaLibraryPage { items, next_cursor })
    }

    pub fn start_observation_generation(
        &self,
        generation_id: impl Into<String>,
        node_id: &NodeId,
        scope: ObservationScope,
    ) -> Result<ObservationGeneration, StorageLibraryError> {
        let generation_id = generation_id.into();
        validate_opaque_id("generationId", &generation_id)?;
        let root = self.get_storage_root(&scope.storage_root_id)?;
        if !root.enabled {
            return Err(StorageLibraryError::InvalidInput {
                field: "storageRootId",
                reason: "cannot start an observation for a disabled root".to_string(),
            });
        }
        let binding = self.get_node_root_capability(node_id, &scope.storage_root_id)?;
        if !binding.enabled
            || binding.verification_status != BindingVerificationStatus::Confirmed
            || binding.confirmation.status != RootAssociationConfirmationStatus::Confirmed
        {
            return Err(StorageLibraryError::InvalidInput {
                field: "binding",
                reason: "observation requires an enabled, explicitly confirmed binding".to_string(),
            });
        }
        if let Some(binding_prefix) = &binding.root_subpath {
            let scope_is_within_binding = scope
                .relative_prefix
                .as_ref()
                .is_some_and(|scope_prefix| scope_prefix.is_within(binding_prefix));
            if !scope_is_within_binding {
                return Err(StorageLibraryError::InvalidInput {
                    field: "scope.relativePrefix",
                    reason:
                        "a partial binding may observe only its rootSubpath or a narrower scope"
                            .to_string(),
                });
            }
        }
        let mut connection = open_writable_connection(&self.database_path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            r#"
            INSERT INTO node_observation_sequences(node_id, last_sequence)
            VALUES (?1, 0)
            ON CONFLICT(node_id) DO NOTHING
            "#,
            [node_id.as_str()],
        )?;
        transaction.execute(
            "UPDATE node_observation_sequences SET last_sequence = last_sequence + 1 WHERE node_id = ?1",
            [node_id.as_str()],
        )?;
        let sequence: i64 = transaction.query_row(
            "SELECT last_sequence FROM node_observation_sequences WHERE node_id = ?1",
            [node_id.as_str()],
            |row| row.get(0),
        )?;
        let started_at = now();
        transaction.execute(
            r#"
            INSERT INTO observation_generations (
                generation_id, node_id, generation_sequence, storage_root_id,
                scope_relative_path, state, started_at, completed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, 'in_progress', ?6, NULL)
            "#,
            params![
                generation_id,
                node_id.as_str(),
                sequence,
                scope.storage_root_id.as_str(),
                scope.relative_prefix.as_ref().map(RootRelativePath::as_str),
                started_at,
            ],
        )?;
        transaction.commit()?;
        Ok(ObservationGeneration {
            id: generation_id,
            node_id: node_id.clone(),
            sequence: sequence as u64,
            scope,
            state: ObservationGenerationState::InProgress,
            started_at,
            completed_at: None,
        })
    }

    pub fn record_asset_observation(
        &self,
        generation_id: &str,
        input: AssetObservationInput,
    ) -> Result<(), StorageLibraryError> {
        validate_opaque_id("generationId", generation_id)?;
        if let Some(fingerprint) = &input.observed_quick_fingerprint {
            validate_blake3("observedQuickFingerprint", fingerprint)?;
        }
        let object = self.get_storage_object(&input.storage_object_id)?;
        let mut connection = open_writable_connection(&self.database_path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let generation = load_generation(&transaction, generation_id)?;
        if generation.state != ObservationGenerationState::InProgress {
            return Err(invalid_generation_state(&generation));
        }
        if object.storage_root_id != generation.scope.storage_root_id
            || generation
                .scope
                .relative_prefix
                .as_ref()
                .is_some_and(|prefix| !object.relative_path.is_within(prefix))
        {
            return Err(StorageLibraryError::ObservationOutsideScope {
                storage_object_id: input.storage_object_id.into_string(),
            });
        }
        let result = transaction.execute(
            r#"
            INSERT INTO node_asset_observations (
                generation_id, node_id, storage_root_id, storage_object_id, status,
                observed_size_bytes, observed_quick_fingerprint, observed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![
                generation.id,
                generation.node_id.as_str(),
                generation.scope.storage_root_id.as_str(),
                input.storage_object_id.as_str(),
                input.status.as_str(),
                to_i64(input.observed_size_bytes, "observedSizeBytes")?,
                input.observed_quick_fingerprint,
                now(),
            ],
        );
        match result {
            Ok(_) => {
                transaction.commit()?;
                Ok(())
            }
            Err(error) if is_unique_constraint(&error) => {
                Err(StorageLibraryError::ObservationAlreadyRecorded {
                    generation_id: generation_id.to_string(),
                    storage_object_id: input.storage_object_id.into_string(),
                })
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn complete_observation_generation(
        &self,
        generation_id: &str,
    ) -> Result<ObservationGeneration, StorageLibraryError> {
        validate_opaque_id("generationId", generation_id)?;
        let mut connection = open_writable_connection(&self.database_path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut generation = load_generation(&transaction, generation_id)?;
        match generation.state {
            ObservationGenerationState::Completed => {
                transaction.commit()?;
                return Ok(generation);
            }
            ObservationGenerationState::InProgress => {}
            ObservationGenerationState::Abandoned => {
                return Err(invalid_generation_state(&generation));
            }
        }
        let completed_at = now();
        transaction.execute(
            r#"
            UPDATE observation_generations
            SET state = 'completed', completed_at = ?2
            WHERE generation_id = ?1 AND state = 'in_progress'
            "#,
            params![generation_id, completed_at],
        )?;
        transaction.commit()?;
        generation.state = ObservationGenerationState::Completed;
        generation.completed_at = Some(completed_at);
        Ok(generation)
    }

    pub fn abandon_observation_generation(
        &self,
        generation_id: &str,
    ) -> Result<ObservationGeneration, StorageLibraryError> {
        validate_opaque_id("generationId", generation_id)?;
        let mut connection = open_writable_connection(&self.database_path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut generation = load_generation(&transaction, generation_id)?;
        match generation.state {
            ObservationGenerationState::Abandoned => {
                transaction.commit()?;
                return Ok(generation);
            }
            ObservationGenerationState::InProgress => {}
            ObservationGenerationState::Completed => {
                return Err(invalid_generation_state(&generation));
            }
        }
        transaction.execute(
            "UPDATE observation_generations SET state = 'abandoned' WHERE generation_id = ?1",
            [generation_id],
        )?;
        transaction.commit()?;
        generation.state = ObservationGenerationState::Abandoned;
        Ok(generation)
    }

    pub fn media_availability(
        &self,
        media_object_id: &MediaObjectId,
        queried_node_id: &NodeId,
    ) -> Result<MediaAvailability, StorageLibraryError> {
        self.get_media_object(media_object_id)?;
        let connection = open_read_only_connection(&self.database_path)?;
        let mut statement = connection.prepare(
            r#"
            SELECT so.storage_object_id, so.storage_root_id, so.relative_path, sr.enabled,
                   mol.verification_state
            FROM media_object_locations mol
            JOIN storage_objects so ON so.storage_object_id = mol.storage_object_id
            JOIN storage_roots sr ON sr.storage_root_id = so.storage_root_id
            WHERE mol.media_object_id = ?1
            ORDER BY so.storage_root_id, so.relative_path, so.storage_object_id
            "#,
        )?;
        let storage_locations = statement
            .query_map([media_object_id.as_str()], |row| {
                Ok((
                    StorageObjectId::parse(row.get::<_, String>(0)?).map_err(stored_error(0))?,
                    StorageRootId::parse(row.get::<_, String>(1)?).map_err(stored_error(1))?,
                    RootRelativePath::parse(row.get::<_, String>(2)?).map_err(stored_error(2))?,
                    row.get::<_, bool>(3)?,
                    parse_stored(
                        LocationVerificationState::parse_stored,
                        row.get::<_, String>(4)?,
                        4,
                    )?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut locations = Vec::new();
        let mut has_location_mismatch = false;
        for (storage_object_id, root_id, relative_path, root_enabled, location_verification) in
            storage_locations
        {
            has_location_mismatch |= location_verification == LocationVerificationState::Mismatch;
            let bindings = load_bindings_for_availability(&connection, &root_id)?;
            for binding in bindings {
                if binding
                    .root_subpath
                    .as_ref()
                    .is_some_and(|prefix| !relative_path.is_within(prefix))
                {
                    continue;
                }
                let (state, generation) = if location_verification
                    == LocationVerificationState::Mismatch
                {
                    (NodeAvailabilityState::FingerprintMismatch, None)
                } else if !root_enabled
                    || !binding.enabled
                    || binding.verification_status != BindingVerificationStatus::Confirmed
                    || binding.confirmation_status != RootAssociationConfirmationStatus::Confirmed
                {
                    (NodeAvailabilityState::TemporarilyUnavailable, None)
                } else {
                    latest_completed_availability(
                        &connection,
                        &binding.node_id,
                        &root_id,
                        &relative_path,
                        &storage_object_id,
                    )?
                };
                locations.push(StorageLocationAvailability {
                    node_id: binding.node_id,
                    storage_object_id: storage_object_id.clone(),
                    state,
                    completed_generation_id: generation.as_ref().map(|value| value.0.clone()),
                    completed_generation_sequence: generation.map(|value| value.1),
                });
            }
        }
        locations.sort_by(|left, right| {
            left.node_id
                .cmp(&right.node_id)
                .then_with(|| left.storage_object_id.cmp(&right.storage_object_id))
        });
        let state = aggregate_availability(&locations, queried_node_id, has_location_mismatch);
        Ok(MediaAvailability {
            media_object_id: media_object_id.clone(),
            queried_node_id: queried_node_id.clone(),
            state,
            locations,
        })
    }
}

const LIBRARY_QUERY_ALL: &str = r#"
    SELECT m.media_object_id, m.size_bytes, m.quick_fingerprint, m.full_content_hash,
           m.verification_state, m.created_at, m.updated_at,
           (SELECT COUNT(*) FROM media_object_locations l WHERE l.media_object_id = m.media_object_id),
           (SELECT COUNT(*) FROM project_media_memberships p WHERE p.media_object_id = m.media_object_id)
    FROM media_objects m
    WHERE (?1 IS NULL OR m.created_at < ?1 OR (m.created_at = ?1 AND m.media_object_id > ?2))
    ORDER BY m.created_at DESC, m.media_object_id ASC
    LIMIT ?3
"#;

const LIBRARY_QUERY_WITH_STATE: &str = r#"
    SELECT m.media_object_id, m.size_bytes, m.quick_fingerprint, m.full_content_hash,
           m.verification_state, m.created_at, m.updated_at,
           (SELECT COUNT(*) FROM media_object_locations l WHERE l.media_object_id = m.media_object_id),
           (SELECT COUNT(*) FROM project_media_memberships p WHERE p.media_object_id = m.media_object_id)
    FROM media_objects m
    WHERE m.verification_state = ?1
      AND (?2 IS NULL OR m.created_at < ?2 OR (m.created_at = ?2 AND m.media_object_id > ?3))
    ORDER BY m.created_at DESC, m.media_object_id ASC
    LIMIT ?4
"#;

#[derive(Debug)]
struct AvailabilityBinding {
    node_id: NodeId,
    root_subpath: Option<RootRelativePath>,
    verification_status: BindingVerificationStatus,
    confirmation_status: RootAssociationConfirmationStatus,
    enabled: bool,
}

fn load_bindings_for_availability(
    connection: &rusqlite::Connection,
    root_id: &StorageRootId,
) -> rusqlite::Result<Vec<AvailabilityBinding>> {
    let mut statement = connection.prepare(
        r#"
        SELECT node_id, root_subpath, verification_status, confirmation_status, enabled
        FROM node_root_bindings
        WHERE storage_root_id = ?1
        ORDER BY node_id
        "#,
    )?;
    statement
        .query_map([root_id.as_str()], |row| {
            let subpath = row
                .get::<_, Option<String>>(1)?
                .map(RootRelativePath::parse)
                .transpose()
                .map_err(stored_error(1))?;
            Ok(AvailabilityBinding {
                node_id: NodeId::parse(row.get::<_, String>(0)?).map_err(stored_error(0))?,
                root_subpath: subpath,
                verification_status: parse_stored(
                    BindingVerificationStatus::parse_stored,
                    row.get::<_, String>(2)?,
                    2,
                )?,
                confirmation_status: parse_stored(
                    RootAssociationConfirmationStatus::parse_stored,
                    row.get::<_, String>(3)?,
                    3,
                )?,
                enabled: row.get(4)?,
            })
        })?
        .collect()
}

fn latest_completed_availability(
    connection: &rusqlite::Connection,
    node_id: &NodeId,
    root_id: &StorageRootId,
    relative_path: &RootRelativePath,
    storage_object_id: &StorageObjectId,
) -> Result<(NodeAvailabilityState, Option<(String, u64)>), StorageLibraryError> {
    let mut statement = connection.prepare(
        r#"
        SELECT generation_id, generation_sequence, scope_relative_path
        FROM observation_generations
        WHERE node_id = ?1 AND storage_root_id = ?2 AND state = 'completed'
        ORDER BY generation_sequence DESC
        "#,
    )?;
    let generations = statement
        .query_map(params![node_id.as_str(), root_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut covering = None;
    for (id, sequence, scope) in generations {
        let prefix = scope
            .map(RootRelativePath::parse)
            .transpose()
            .map_err(|error| StorageLibraryError::InvalidInput {
                field: "storedObservationScope",
                reason: error.to_string(),
            })?;
        if prefix
            .as_ref()
            .is_some_and(|prefix| !relative_path.is_within(prefix))
        {
            continue;
        }
        covering = Some((id, sequence as u64));
        break;
    }
    let Some((generation_id, sequence)) = covering else {
        return Ok((NodeAvailabilityState::TemporarilyUnavailable, None));
    };
    let status = connection
        .query_row(
            r#"
            SELECT status FROM node_asset_observations
            WHERE generation_id = ?1 AND storage_object_id = ?2
            "#,
            params![generation_id, storage_object_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let state = match status.as_deref() {
        Some("accessible") => NodeAvailabilityState::AccessibleHere,
        Some("temporarily_unavailable") => NodeAvailabilityState::TemporarilyUnavailable,
        Some("fingerprint_mismatch") => NodeAvailabilityState::FingerprintMismatch,
        Some(_) => NodeAvailabilityState::FingerprintMismatch,
        None => NodeAvailabilityState::MissingAfterCompletedObservation,
    };
    Ok((state, Some((generation_id, sequence))))
}

fn aggregate_availability(
    locations: &[StorageLocationAvailability],
    queried_node_id: &NodeId,
    has_location_mismatch: bool,
) -> NodeAvailabilityState {
    if has_location_mismatch
        || locations.iter().any(|location| {
            &location.node_id == queried_node_id
                && location.state == NodeAvailabilityState::FingerprintMismatch
        })
    {
        NodeAvailabilityState::FingerprintMismatch
    } else if locations.iter().any(|location| {
        &location.node_id == queried_node_id
            && location.state == NodeAvailabilityState::AccessibleHere
    }) {
        NodeAvailabilityState::AccessibleHere
    } else if locations.iter().any(|location| {
        &location.node_id != queried_node_id
            && location.state == NodeAvailabilityState::AccessibleHere
    }) {
        NodeAvailabilityState::AccessibleOnAnotherNode
    } else if locations
        .iter()
        .any(|location| location.state == NodeAvailabilityState::FingerprintMismatch)
    {
        NodeAvailabilityState::FingerprintMismatch
    } else if locations
        .iter()
        .any(|location| location.state == NodeAvailabilityState::MissingAfterCompletedObservation)
    {
        NodeAvailabilityState::MissingAfterCompletedObservation
    } else if locations
        .iter()
        .any(|location| location.state == NodeAvailabilityState::TemporarilyUnavailable)
    {
        NodeAvailabilityState::TemporarilyUnavailable
    } else {
        NodeAvailabilityState::Known
    }
}

fn load_generation(
    transaction: &Transaction<'_>,
    generation_id: &str,
) -> Result<ObservationGeneration, StorageLibraryError> {
    transaction
        .query_row(
            r#"
            SELECT generation_id, node_id, generation_sequence, storage_root_id,
                   scope_relative_path, state, started_at, completed_at
            FROM observation_generations WHERE generation_id = ?1
            "#,
            [generation_id],
            generation_from_row,
        )
        .optional()?
        .ok_or_else(|| not_found("observation generation", generation_id))
}

fn storage_root_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StorageRoot> {
    Ok(StorageRoot {
        id: StorageRootId::parse(row.get::<_, String>(0)?).map_err(stored_error(0))?,
        display_name: row.get(1)?,
        immutable_original: row.get(2)?,
        enabled: row.get(3)?,
        disabled_at: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn capability_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NodeRootCapability> {
    let evidence = row
        .get::<_, Option<String>>(7)?
        .map(|json| serde_json::from_str(&json))
        .transpose()
        .map_err(stored_error(7))?;
    let confirmed_by_node_id = row
        .get::<_, Option<String>>(9)?
        .map(NodeId::parse)
        .transpose()
        .map_err(stored_error(9))?;
    Ok(NodeRootCapability {
        node_id: NodeId::parse(row.get::<_, String>(0)?).map_err(stored_error(0))?,
        storage_root_id: StorageRootId::parse(row.get::<_, String>(1)?).map_err(stored_error(1))?,
        root_subpath: row
            .get::<_, Option<String>>(2)?
            .map(RootRelativePath::parse)
            .transpose()
            .map_err(stored_error(2))?,
        protocol: parse_stored(StorageProtocol::parse_stored, row.get(3)?, 3)?,
        access_mode: parse_stored(StorageAccessMode::parse_stored, row.get(4)?, 4)?,
        verification_status: parse_stored(BindingVerificationStatus::parse_stored, row.get(5)?, 5)?,
        enabled: row.get(6)?,
        association_evidence: evidence,
        confirmation: RootAssociationConfirmation {
            status: parse_stored(
                RootAssociationConfirmationStatus::parse_stored,
                row.get(8)?,
                8,
            )?,
            confirmed_by_node_id,
            confirmed_at: row.get(10)?,
        },
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn binding_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NodeRootBindingAdmin> {
    let evidence = row
        .get::<_, Option<String>>(8)?
        .map(|json| serde_json::from_str(&json))
        .transpose()
        .map_err(stored_error(8))?;
    let confirmed_by_node_id = row
        .get::<_, Option<String>>(10)?
        .map(NodeId::parse)
        .transpose()
        .map_err(stored_error(10))?;
    Ok(NodeRootBindingAdmin {
        node_id: NodeId::parse(row.get::<_, String>(0)?).map_err(stored_error(0))?,
        storage_root_id: StorageRootId::parse(row.get::<_, String>(1)?).map_err(stored_error(1))?,
        local_mount_path: PathBuf::from(row.get::<_, String>(2)?),
        root_subpath: row
            .get::<_, Option<String>>(3)?
            .map(RootRelativePath::parse)
            .transpose()
            .map_err(stored_error(3))?,
        protocol: parse_stored(StorageProtocol::parse_stored, row.get(4)?, 4)?,
        access_mode: parse_stored(StorageAccessMode::parse_stored, row.get(5)?, 5)?,
        verification_status: parse_stored(BindingVerificationStatus::parse_stored, row.get(6)?, 6)?,
        enabled: row.get(7)?,
        association_evidence: evidence,
        confirmation: RootAssociationConfirmation {
            status: parse_stored(
                RootAssociationConfirmationStatus::parse_stored,
                row.get(9)?,
                9,
            )?,
            confirmed_by_node_id,
            confirmed_at: row.get(11)?,
        },
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}

fn media_object_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MediaObject> {
    Ok(MediaObject {
        id: MediaObjectId::parse(row.get::<_, String>(0)?).map_err(stored_error(0))?,
        size_bytes: from_i64(row.get(1)?, 1)?,
        quick_fingerprint: row.get(2)?,
        full_content_hash: row.get(3)?,
        verification_state: parse_stored(
            GlobalMediaVerificationState::parse_stored,
            row.get(4)?,
            4,
        )?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn storage_object_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StorageObject> {
    Ok(StorageObject {
        id: StorageObjectId::parse(row.get::<_, String>(0)?).map_err(stored_error(0))?,
        storage_root_id: StorageRootId::parse(row.get::<_, String>(1)?).map_err(stored_error(1))?,
        relative_path: RootRelativePath::parse(row.get::<_, String>(2)?)
            .map_err(stored_error(2))?,
        size_bytes: from_i64(row.get(3)?, 3)?,
        modified_at: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn location_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MediaObjectLocation> {
    Ok(MediaObjectLocation {
        media_object_id: MediaObjectId::parse(row.get::<_, String>(0)?).map_err(stored_error(0))?,
        storage_object_id: StorageObjectId::parse(row.get::<_, String>(1)?)
            .map_err(stored_error(1))?,
        verification_state: parse_stored(LocationVerificationState::parse_stored, row.get(2)?, 2)?,
        linked_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

fn membership_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ProjectMediaMembershipProjection> {
    Ok(ProjectMediaMembershipProjection {
        project_id: ProjectId::parse(row.get::<_, String>(0)?).map_err(stored_error(0))?,
        project_asset_id: ProjectAssetId::parse(row.get::<_, String>(1)?)
            .map_err(stored_error(1))?,
        media_object_id: MediaObjectId::parse(row.get::<_, String>(2)?).map_err(stored_error(2))?,
        source_operation_id: OperationId::parse(row.get::<_, String>(3)?)
            .map_err(stored_error(3))?,
        projected_at: row.get(4)?,
    })
}

fn library_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MediaLibraryItem> {
    Ok(MediaLibraryItem {
        media: media_object_from_row(row)?,
        storage_location_count: from_i64(Some(row.get(7)?), 7)?.unwrap_or(0),
        project_membership_count: from_i64(Some(row.get(8)?), 8)?.unwrap_or(0),
    })
}

fn generation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ObservationGeneration> {
    Ok(ObservationGeneration {
        id: row.get(0)?,
        node_id: NodeId::parse(row.get::<_, String>(1)?).map_err(stored_error(1))?,
        sequence: from_i64(Some(row.get(2)?), 2)?.unwrap_or(0),
        scope: ObservationScope {
            storage_root_id: StorageRootId::parse(row.get::<_, String>(3)?)
                .map_err(stored_error(3))?,
            relative_prefix: row
                .get::<_, Option<String>>(4)?
                .map(RootRelativePath::parse)
                .transpose()
                .map_err(stored_error(4))?,
        },
        state: parse_stored(ObservationGenerationState::parse_stored, row.get(5)?, 5)?,
        started_at: row.get(6)?,
        completed_at: row.get(7)?,
    })
}

fn validate_display_name(value: &str) -> Result<(), StorageLibraryError> {
    if value.trim().is_empty()
        || value.len() > MAX_DISPLAY_NAME_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(StorageLibraryError::InvalidInput {
            field: "displayName",
            reason: "must be 1-255 bytes and contain no control characters".to_string(),
        });
    }
    Ok(())
}

fn validate_confirmation(
    verification_status: &BindingVerificationStatus,
    confirmation: &RootAssociationConfirmation,
) -> Result<(), StorageLibraryError> {
    let complete =
        confirmation.confirmed_by_node_id.is_some() && confirmation.confirmed_at.is_some();
    let empty = confirmation.confirmed_by_node_id.is_none() && confirmation.confirmed_at.is_none();
    if confirmation.status == RootAssociationConfirmationStatus::Confirmed {
        if !complete || *verification_status != BindingVerificationStatus::Confirmed {
            return Err(StorageLibraryError::InvalidInput {
                field: "confirmation",
                reason:
                    "confirmed associations require confirmer, time, and confirmed verification"
                        .to_string(),
            });
        }
    } else if !empty {
        return Err(StorageLibraryError::InvalidInput {
            field: "confirmation",
            reason: "pending or rejected associations cannot carry confirmation metadata"
                .to_string(),
        });
    }
    Ok(())
}

fn validate_media_identity(input: &NewMediaObject) -> Result<(), StorageLibraryError> {
    if let Some(hash) = &input.quick_fingerprint {
        validate_blake3("quickFingerprint", hash)?;
    }
    if let Some(hash) = &input.full_content_hash {
        validate_blake3("fullContentHash", hash)?;
    }
    if input.verification_state == GlobalMediaVerificationState::Verified
        && (input.full_content_hash.is_none() || input.size_bytes.is_none())
    {
        return Err(StorageLibraryError::InvalidInput {
            field: "verificationState",
            reason: "verified media requires a full BLAKE3 hash and size".to_string(),
        });
    }
    let _ = to_i64(input.size_bytes, "sizeBytes")?;
    Ok(())
}

fn validate_blake3(field: &'static str, value: &str) -> Result<(), StorageLibraryError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(StorageLibraryError::InvalidInput {
            field,
            reason: "must be a 64-character lowercase hexadecimal BLAKE3 digest".to_string(),
        });
    }
    Ok(())
}

fn validate_opaque_id(field: &'static str, value: &str) -> Result<(), StorageLibraryError> {
    if value.is_empty()
        || value.len() > 255
        || value.chars().any(char::is_whitespace)
        || value.chars().any(char::is_control)
    {
        return Err(StorageLibraryError::InvalidInput {
            field,
            reason: "must be a non-empty opaque ID without whitespace or control characters"
                .to_string(),
        });
    }
    Ok(())
}

fn to_i64(value: Option<u64>, field: &'static str) -> Result<Option<i64>, StorageLibraryError> {
    value
        .map(|value| {
            i64::try_from(value).map_err(|_| StorageLibraryError::InvalidInput {
                field,
                reason: "exceeds SQLite's signed 64-bit integer range".to_string(),
            })
        })
        .transpose()
}

fn from_i64(value: Option<i64>, column: usize) -> rusqlite::Result<Option<u64>> {
    value
        .map(|value| {
            u64::try_from(value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    column,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })
        })
        .transpose()
}

fn parse_stored<T>(
    parser: impl FnOnce(&str) -> Option<T>,
    value: String,
    column: usize,
) -> rusqlite::Result<T> {
    parser(&value).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            format!("invalid stored enum value {value:?}").into(),
        )
    })
}

fn stored_error<E>(column: usize) -> impl FnOnce(E) -> rusqlite::Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    move |error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    }
}

fn is_unique_constraint(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

fn not_found(entity: &'static str, id: &str) -> StorageLibraryError {
    StorageLibraryError::NotFound {
        entity,
        id: id.to_string(),
    }
}

fn invalid_generation_state(generation: &ObservationGeneration) -> StorageLibraryError {
    StorageLibraryError::InvalidGenerationState {
        generation_id: generation.id.clone(),
        state: generation.state.as_str().to_string(),
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ControlCatalog, ServerConfig};
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    struct Fixture {
        _root: TempDir,
        catalog: ControlCatalog,
        library: StorageLibrary,
    }

    fn fixture() -> Fixture {
        let root = TempDir::new().unwrap();
        let data = root.path().join("state");
        let derived = root.path().join("derived");
        let backups = root.path().join("backups");
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(&derived).unwrap();
        fs::create_dir_all(&backups).unwrap();
        let config = ServerConfig::from_values(BTreeMap::from([
            ("RCDVLOG_DATA_DIR".to_string(), data.display().to_string()),
            (
                "RCDVLOG_DERIVED_DIR".to_string(),
                derived.display().to_string(),
            ),
            (
                "RCDVLOG_BACKUP_DIR".to_string(),
                backups.display().to_string(),
            ),
        ]))
        .unwrap();
        let catalog = ControlCatalog::open_or_create(&data, &config.diagnostics()).unwrap();
        let library = catalog.storage_library();
        Fixture {
            _root: root,
            catalog,
            library,
        }
    }

    fn id_root(value: &str) -> StorageRootId {
        StorageRootId::parse(value).unwrap()
    }

    fn id_node(value: &str) -> NodeId {
        NodeId::parse(value).unwrap()
    }

    fn id_media(value: &str) -> MediaObjectId {
        MediaObjectId::parse(value).unwrap()
    }

    fn id_object(value: &str) -> StorageObjectId {
        StorageObjectId::parse(value).unwrap()
    }

    fn create_root(library: &StorageLibrary, id: &str, immutable: bool) -> StorageRootId {
        let id = id_root(id);
        library
            .create_storage_root(NewStorageRoot {
                id: id.clone(),
                display_name: format!("Root {id}"),
                immutable_original: immutable,
            })
            .unwrap();
        id
    }

    fn confirmed_capability_input(
        node: &NodeId,
        root: &StorageRootId,
        subpath: Option<&str>,
    ) -> NodeRootCapabilityInput {
        NodeRootCapabilityInput {
            node_id: node.clone(),
            storage_root_id: root.clone(),
            root_subpath: subpath.map(|value| RootRelativePath::parse(value).unwrap()),
            protocol: StorageProtocol::Nfs,
            access_mode: StorageAccessMode::ReadOnly,
            verification_status: BindingVerificationStatus::Confirmed,
            enabled: true,
            association_evidence: Some(RootAssociationEvidence {
                matching_relative_paths: 2,
                matching_quick_fingerprints: 1,
                matching_full_hashes: 1,
                mismatches: 0,
                sampled_relative_paths: vec![
                    RootRelativePath::parse("Trips/2026/sample.mp4").unwrap(),
                ],
                captured_at: "2026-07-20T00:00:00Z".to_string(),
            }),
            confirmation: RootAssociationConfirmation {
                status: RootAssociationConfirmationStatus::Confirmed,
                confirmed_by_node_id: Some(node.clone()),
                confirmed_at: Some("2026-07-20T00:01:00Z".to_string()),
            },
        }
    }

    fn confirmed_capability(
        library: &StorageLibrary,
        node: &NodeId,
        root: &StorageRootId,
        subpath: Option<&str>,
    ) -> NodeRootCapability {
        library
            .upsert_node_root_capability(confirmed_capability_input(node, root, subpath))
            .unwrap()
    }

    fn confirmed_local_binding(
        library: &StorageLibrary,
        root: &StorageRootId,
        mount: &Path,
        subpath: Option<&str>,
    ) -> NodeRootBindingAdmin {
        let node = library.local_node_id().clone();
        let capability = confirmed_capability_input(&node, root, subpath);
        library
            .upsert_local_node_root_binding(LocalNodeRootBindingInput {
                storage_root_id: capability.storage_root_id,
                local_mount_path: mount.to_path_buf(),
                root_subpath: capability.root_subpath,
                protocol: capability.protocol,
                access_mode: capability.access_mode,
                verification_status: capability.verification_status,
                enabled: capability.enabled,
                association_evidence: capability.association_evidence,
                confirmation: capability.confirmation,
            })
            .unwrap()
    }

    fn create_media(library: &StorageLibrary, id: &str) -> MediaObjectId {
        let id = id_media(id);
        library
            .create_media_object(NewMediaObject {
                id: id.clone(),
                size_bytes: None,
                quick_fingerprint: None,
                full_content_hash: None,
                verification_state: GlobalMediaVerificationState::Unverified,
            })
            .unwrap();
        id
    }

    fn create_object(
        library: &StorageLibrary,
        id: &str,
        root: &StorageRootId,
        path: &str,
    ) -> StorageObjectId {
        let id = id_object(id);
        library
            .create_storage_object(NewStorageObject {
                id: id.clone(),
                storage_root_id: root.clone(),
                relative_path: RootRelativePath::parse(path).unwrap(),
                size_bytes: Some(7),
                modified_at: Some("2026-07-20T00:00:00Z".to_string()),
            })
            .unwrap();
        id
    }

    fn link(library: &StorageLibrary, media: &MediaObjectId, object: &StorageObjectId) {
        library
            .link_media_object_location(media, object, LocationVerificationState::Unverified)
            .unwrap();
    }

    fn observe(
        library: &StorageLibrary,
        generation: &str,
        object: &StorageObjectId,
        status: ObservationStatus,
    ) {
        library
            .record_asset_observation(
                generation,
                AssetObservationInput {
                    storage_object_id: object.clone(),
                    status,
                    observed_size_bytes: Some(7),
                    observed_quick_fingerprint: None,
                },
            )
            .unwrap();
    }

    #[test]
    fn logical_root_resolves_through_distinct_full_and_partial_bindings_read_only() {
        let fixture = fixture();
        let root_id = create_root(&fixture.library, "root_main_nas", true);
        let mac = id_node("node_mac");
        let server = fixture.catalog.node_id().clone();
        let mac_mount = fixture._root.path().join("Mac NAS");
        let server_mount = fixture._root.path().join("server-subtree");
        fs::create_dir_all(mac_mount.join("Trips/2026/日本")).unwrap();
        fs::create_dir_all(server_mount.join("日本")).unwrap();
        fs::write(mac_mount.join("Trips/2026/日本/clip 01.MP4"), b"fixture").unwrap();
        fs::write(server_mount.join("日本/clip 01.MP4"), b"fixture").unwrap();
        let mac_capability = confirmed_capability(&fixture.library, &mac, &root_id, None);
        let server_binding = confirmed_local_binding(
            &fixture.library,
            &root_id,
            &server_mount,
            Some("Trips/2026"),
        );
        assert_eq!(server_binding.local_mount_path, server_mount);
        assert_eq!(
            server_binding.confirmation.status,
            RootAssociationConfirmationStatus::Confirmed
        );
        assert_eq!(
            server_binding
                .association_evidence
                .unwrap()
                .matching_full_hashes,
            1
        );
        let object = create_object(
            &fixture.library,
            "storage_clip",
            &root_id,
            "Trips/2026/日本/clip 01.MP4",
        );

        assert!(matches!(
            fixture.library.start_observation_generation(
                "generation_partial_invalid_whole_root",
                &server,
                ObservationScope {
                    storage_root_id: root_id.clone(),
                    relative_prefix: None,
                },
            ),
            Err(StorageLibraryError::InvalidInput {
                field: "scope.relativePrefix",
                ..
            })
        ));
        let partial_generation = fixture
            .library
            .start_observation_generation(
                "generation_partial_valid",
                &server,
                ObservationScope {
                    storage_root_id: root_id.clone(),
                    relative_prefix: Some(RootRelativePath::parse("Trips/2026/日本").unwrap()),
                },
            )
            .unwrap();
        assert_eq!(
            partial_generation.scope.relative_prefix.unwrap().as_str(),
            "Trips/2026/日本"
        );
        fixture
            .library
            .abandon_observation_generation("generation_partial_valid")
            .unwrap();

        let logical = RootRelativePath::parse("Trips/2026/日本/clip 01.MP4").unwrap();
        assert_eq!(
            resolve_existing_binding_path(
                &mac_mount,
                None,
                &logical,
                true,
                StorageAccessMode::ReadOnly,
                PathAccessIntent::Read,
            )
            .unwrap(),
            mac_mount
                .join("Trips/2026/日本/clip 01.MP4")
                .canonicalize()
                .unwrap()
        );
        let mac_capability_json = serde_json::to_value(&mac_capability).unwrap();
        assert!(mac_capability_json.get("localMountPath").is_none());
        assert!(
            !mac_capability_json
                .to_string()
                .contains(mac_mount.to_str().unwrap())
        );
        let capabilities_json = serde_json::to_value(
            fixture
                .library
                .list_node_root_capabilities(Some(&root_id))
                .unwrap(),
        )
        .unwrap()
        .to_string();
        assert!(!capabilities_json.contains("localMountPath"));
        assert!(!capabilities_json.contains(mac_mount.to_str().unwrap()));
        assert!(!capabilities_json.contains(server_mount.to_str().unwrap()));
        let local_admin = fixture
            .library
            .list_local_node_root_bindings_admin()
            .unwrap();
        assert_eq!(local_admin.len(), 1);
        assert_eq!(local_admin[0].node_id, server);
        let connection = open_read_only_connection(fixture.catalog.database_path()).unwrap();
        let remote_path: Option<String> = connection
            .query_row(
                "SELECT local_mount_path FROM node_root_bindings WHERE node_id = ?1 AND storage_root_id = ?2",
                params![mac.as_str(), root_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(remote_path.is_none());
        assert_eq!(
            fixture
                .library
                .resolve_local_storage_object(&object, PathAccessIntent::Read)
                .unwrap(),
            server_mount
                .join("日本/clip 01.MP4")
                .canonicalize()
                .unwrap()
        );
        assert!(matches!(
            fixture
                .library
                .resolve_local_storage_object(&object, PathAccessIntent::Write),
            Err(StorageLibraryError::Path(PathResolutionError {
                reason: crate::PathResolutionErrorReason::ImmutableOriginal,
                ..
            }))
        ));
        assert!(!mac_mount.join(".rcdvlog-storage-root").exists());
        assert!(!server_mount.join(".rcdvlog-storage-root").exists());
    }

    #[test]
    fn local_resolution_requires_enabled_confirmed_association() {
        let fixture = fixture();
        let root = create_root(&fixture.library, "root_resolution_state", true);
        let server = fixture.catalog.node_id().clone();
        let mount = fixture._root.path().join("resolution-mount");
        fs::create_dir_all(&mount).unwrap();
        let object = create_object(
            &fixture.library,
            "storage_resolution_state",
            &root,
            "clip.mp4",
        );

        for (verification, confirmation) in [
            (
                BindingVerificationStatus::Pending,
                RootAssociationConfirmationStatus::Pending,
            ),
            (
                BindingVerificationStatus::Rejected,
                RootAssociationConfirmationStatus::Rejected,
            ),
            (
                BindingVerificationStatus::Unavailable,
                RootAssociationConfirmationStatus::Pending,
            ),
            (
                BindingVerificationStatus::Mismatch,
                RootAssociationConfirmationStatus::Pending,
            ),
        ] {
            fixture
                .library
                .upsert_local_node_root_binding(LocalNodeRootBindingInput {
                    storage_root_id: root.clone(),
                    local_mount_path: mount.clone(),
                    root_subpath: None,
                    protocol: StorageProtocol::Nfs,
                    access_mode: StorageAccessMode::ReadOnly,
                    verification_status: verification,
                    enabled: true,
                    association_evidence: None,
                    confirmation: RootAssociationConfirmation {
                        status: confirmation,
                        confirmed_by_node_id: None,
                        confirmed_at: None,
                    },
                })
                .unwrap();
            assert!(matches!(
                fixture
                    .library
                    .resolve_local_storage_object(&object, PathAccessIntent::Read,),
                Err(StorageLibraryError::InvalidInput {
                    field: "binding",
                    ..
                })
            ));
        }

        confirmed_local_binding(&fixture.library, &root, &mount, None);
        fixture
            .library
            .set_node_root_binding_enabled(&server, &root, false)
            .unwrap();
        assert!(
            fixture
                .library
                .resolve_local_storage_object(&object, PathAccessIntent::Read)
                .is_err()
        );
        fixture
            .library
            .set_node_root_binding_enabled(&server, &root, true)
            .unwrap();
        fixture.library.disable_storage_root(&root).unwrap();
        assert!(
            fixture
                .library
                .resolve_local_storage_object(&object, PathAccessIntent::Read)
                .is_err()
        );
    }

    #[test]
    fn media_locations_membership_projection_and_cursor_pages_remain_separate() {
        let fixture = fixture();
        let root = create_root(&fixture.library, "root_library", true);
        let media = create_media(&fixture.library, "media_shared");
        let first_storage =
            create_object(&fixture.library, "storage_first", &root, "Trips/A/clip.mp4");
        let second_storage = create_object(
            &fixture.library,
            "storage_second",
            &root,
            "Archive/copy.mp4",
        );
        link(&fixture.library, &media, &first_storage);
        link(&fixture.library, &media, &second_storage);

        let connection = open_writable_connection(fixture.catalog.database_path()).unwrap();
        for (project, path) in [
            ("project_one", "projects/one/catalog.sqlite3"),
            ("project_two", "projects/two/catalog.sqlite3"),
        ] {
            connection
                .execute(
                    r#"
                    INSERT INTO project_registry(
                        project_id, replica_relative_path, state, registered_at, created_at, updated_at
                    ) VALUES (?1, ?2, 'registered', ?3, ?3, ?3)
                    "#,
                    params![project, path, "2026-07-20T00:00:00Z"],
                )
                .unwrap();
        }
        for (project, asset, operation) in [
            ("project_one", "asset_one", "operation_one"),
            ("project_two", "asset_two", "operation_two"),
        ] {
            fixture
                .library
                .upsert_project_media_membership_projection(ProjectMediaMembershipProjection {
                    project_id: ProjectId::parse(project).unwrap(),
                    project_asset_id: ProjectAssetId::parse(asset).unwrap(),
                    media_object_id: media.clone(),
                    source_operation_id: OperationId::parse(operation).unwrap(),
                    projected_at: "2026-07-20T00:02:00Z".to_string(),
                })
                .unwrap();
        }

        let item = fixture
            .library
            .list_media(MediaLibraryQuery::default())
            .unwrap()
            .items
            .into_iter()
            .find(|item| item.media.id == media)
            .unwrap();
        assert_eq!(item.storage_location_count, 2);
        assert_eq!(item.project_membership_count, 2);

        let columns = connection
            .prepare("SELECT name FROM pragma_table_info('project_media_memberships')")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<BTreeSet<_>>>()
            .unwrap();
        for forbidden in ["notes", "labels", "removed", "rotation", "clock_event"] {
            assert!(!columns.contains(forbidden));
        }

        create_media(&fixture.library, "media_alpha");
        create_media(&fixture.library, "media_zulu");
        let expected = fixture
            .library
            .list_media(MediaLibraryQuery {
                limit: 200,
                ..MediaLibraryQuery::default()
            })
            .unwrap()
            .items
            .into_iter()
            .map(|item| item.media.id)
            .collect::<Vec<_>>();
        let first = fixture
            .library
            .list_media(MediaLibraryQuery {
                limit: 2,
                ..MediaLibraryQuery::default()
            })
            .unwrap();
        assert_eq!(first.items.len(), 2);
        let second = fixture
            .library
            .list_media(MediaLibraryQuery {
                limit: 2,
                after: first.next_cursor.clone(),
                verification_state: None,
            })
            .unwrap();
        let paged = first
            .items
            .into_iter()
            .chain(second.items)
            .map(|item| item.media.id)
            .collect::<Vec<_>>();
        assert_eq!(paged, expected);
        assert!(second.next_cursor.is_none());
    }

    #[test]
    fn verified_media_requires_consistent_full_identity() {
        let fixture = fixture();
        let hash = "a".repeat(64);
        let missing_size = fixture.library.create_media_object(NewMediaObject {
            id: id_media("media_missing_size"),
            size_bytes: None,
            quick_fingerprint: None,
            full_content_hash: Some(hash.clone()),
            verification_state: GlobalMediaVerificationState::Verified,
        });
        assert!(matches!(
            missing_size,
            Err(StorageLibraryError::InvalidInput {
                field: "verificationState",
                ..
            })
        ));
        fixture
            .library
            .create_media_object(NewMediaObject {
                id: id_media("media_verified"),
                size_bytes: Some(100),
                quick_fingerprint: Some("b".repeat(64)),
                full_content_hash: Some(hash.clone()),
                verification_state: GlobalMediaVerificationState::Verified,
            })
            .unwrap();
        let contradictory = fixture.library.create_media_object(NewMediaObject {
            id: id_media("media_contradictory"),
            size_bytes: Some(101),
            quick_fingerprint: None,
            full_content_hash: Some(hash),
            verification_state: GlobalMediaVerificationState::Verified,
        });
        assert!(matches!(
            contradictory,
            Err(StorageLibraryError::Database(_))
        ));
    }

    #[test]
    fn observation_generations_publish_atomically_by_started_sequence_and_scope() {
        let fixture = fixture();
        let root = create_root(&fixture.library, "root_observed", true);
        let server = fixture.catalog.node_id().clone();
        let mac = id_node("node_mac_observed");
        let server_mount = fixture._root.path().join("server-mount");
        fs::create_dir_all(&server_mount).unwrap();
        confirmed_local_binding(&fixture.library, &root, &server_mount, None);

        let media = create_media(&fixture.library, "media_observed");
        let inside = create_object(
            &fixture.library,
            "storage_inside",
            &root,
            "Trips/2026/inside.mp4",
        );
        let outside = create_object(
            &fixture.library,
            "storage_outside",
            &root,
            "Archive/outside.mp4",
        );
        link(&fixture.library, &media, &inside);
        link(&fixture.library, &media, &outside);

        assert_eq!(
            fixture
                .library
                .media_availability(&media, &server)
                .unwrap()
                .state,
            NodeAvailabilityState::TemporarilyUnavailable
        );
        let full = fixture
            .library
            .start_observation_generation(
                "generation_full",
                &server,
                ObservationScope {
                    storage_root_id: root.clone(),
                    relative_prefix: None,
                },
            )
            .unwrap();
        assert_eq!(full.sequence, 1);
        observe(
            &fixture.library,
            "generation_full",
            &inside,
            ObservationStatus::Accessible,
        );
        observe(
            &fixture.library,
            "generation_full",
            &outside,
            ObservationStatus::Accessible,
        );
        assert_eq!(
            fixture
                .library
                .media_availability(&media, &server)
                .unwrap()
                .state,
            NodeAvailabilityState::TemporarilyUnavailable
        );
        fixture
            .library
            .complete_observation_generation("generation_full")
            .unwrap();
        assert_eq!(
            fixture
                .library
                .media_availability(&media, &server)
                .unwrap()
                .state,
            NodeAvailabilityState::AccessibleHere
        );

        let narrow = fixture
            .library
            .start_observation_generation(
                "generation_narrow",
                &server,
                ObservationScope {
                    storage_root_id: root.clone(),
                    relative_prefix: Some(RootRelativePath::parse("Trips/2026").unwrap()),
                },
            )
            .unwrap();
        assert_eq!(narrow.sequence, 2);
        fixture
            .library
            .complete_observation_generation("generation_narrow")
            .unwrap();
        let scoped = fixture.library.media_availability(&media, &server).unwrap();
        let inside_state = scoped
            .locations
            .iter()
            .find(|location| location.storage_object_id == inside)
            .unwrap();
        let outside_state = scoped
            .locations
            .iter()
            .find(|location| location.storage_object_id == outside)
            .unwrap();
        assert_eq!(
            inside_state.state,
            NodeAvailabilityState::MissingAfterCompletedObservation
        );
        assert_eq!(inside_state.completed_generation_sequence, Some(2));
        assert_eq!(outside_state.state, NodeAvailabilityState::AccessibleHere);
        assert_eq!(outside_state.completed_generation_sequence, Some(1));

        let late_old = fixture
            .library
            .start_observation_generation(
                "generation_late_old",
                &server,
                ObservationScope {
                    storage_root_id: root.clone(),
                    relative_prefix: Some(RootRelativePath::parse("Trips/2026").unwrap()),
                },
            )
            .unwrap();
        observe(
            &fixture.library,
            "generation_late_old",
            &inside,
            ObservationStatus::FingerprintMismatch,
        );
        let newer = fixture
            .library
            .start_observation_generation(
                "generation_newer",
                &server,
                ObservationScope {
                    storage_root_id: root.clone(),
                    relative_prefix: Some(RootRelativePath::parse("Trips/2026").unwrap()),
                },
            )
            .unwrap();
        assert!(newer.sequence > late_old.sequence);
        observe(
            &fixture.library,
            "generation_newer",
            &inside,
            ObservationStatus::Accessible,
        );
        fixture
            .library
            .complete_observation_generation("generation_newer")
            .unwrap();
        fixture
            .library
            .complete_observation_generation("generation_late_old")
            .unwrap();
        let after_late_completion = fixture.library.media_availability(&media, &server).unwrap();
        let inside_state = after_late_completion
            .locations
            .iter()
            .find(|location| location.storage_object_id == inside)
            .unwrap();
        assert_eq!(inside_state.state, NodeAvailabilityState::AccessibleHere);
        assert_eq!(
            inside_state.completed_generation_sequence,
            Some(newer.sequence)
        );

        let interrupted = fixture
            .library
            .start_observation_generation(
                "generation_interrupted",
                &server,
                ObservationScope {
                    storage_root_id: root.clone(),
                    relative_prefix: Some(RootRelativePath::parse("Trips/2026").unwrap()),
                },
            )
            .unwrap();
        observe(
            &fixture.library,
            "generation_interrupted",
            &inside,
            ObservationStatus::TemporarilyUnavailable,
        );
        let ignored = fixture.library.media_availability(&media, &server).unwrap();
        let inside_state = ignored
            .locations
            .iter()
            .find(|location| location.storage_object_id == inside)
            .unwrap();
        assert_eq!(inside_state.state, NodeAvailabilityState::AccessibleHere);
        assert_ne!(
            inside_state.completed_generation_sequence,
            Some(interrupted.sequence)
        );

        confirmed_capability(&fixture.library, &mac, &root, None);
        let mac_generation = fixture
            .library
            .start_observation_generation(
                "generation_mac",
                &mac,
                ObservationScope {
                    storage_root_id: root.clone(),
                    relative_prefix: None,
                },
            )
            .unwrap();
        assert_eq!(mac_generation.sequence, 1);
        observe(
            &fixture.library,
            "generation_mac",
            &inside,
            ObservationStatus::Accessible,
        );
        fixture
            .library
            .complete_observation_generation("generation_mac")
            .unwrap();
        fixture
            .library
            .set_node_root_binding_enabled(&server, &root, false)
            .unwrap();
        assert_eq!(
            fixture
                .library
                .media_availability(&media, &server)
                .unwrap()
                .state,
            NodeAvailabilityState::AccessibleOnAnotherNode
        );

        fixture.library.disable_storage_root(&root).unwrap();
        let disabled = fixture.library.media_availability(&media, &server).unwrap();
        assert_eq!(
            disabled.state,
            NodeAvailabilityState::TemporarilyUnavailable
        );
        assert_eq!(disabled.locations.len(), 4);
        assert_eq!(fixture.library.get_media_object(&media).unwrap().id, media);
        assert_eq!(
            fixture.library.get_storage_object(&inside).unwrap().id,
            inside
        );
    }

    #[test]
    fn availability_is_queried_node_aware_and_location_mismatch_is_authoritative() {
        let fixture = fixture();
        let root = create_root(&fixture.library, "root_availability_precedence", true);
        let server = fixture.catalog.node_id().clone();
        let remote = id_node("node_remote_available");
        let unbound = id_node("node_unbound_query");
        let media = create_media(&fixture.library, "media_availability_precedence");
        let object = create_object(
            &fixture.library,
            "storage_availability_precedence",
            &root,
            "Trips/clip.mp4",
        );
        link(&fixture.library, &media, &object);

        assert_eq!(
            fixture
                .library
                .media_availability(&media, &server)
                .unwrap()
                .state,
            NodeAvailabilityState::Known
        );

        let server_mount = fixture._root.path().join("availability-server");
        fs::create_dir_all(&server_mount).unwrap();
        confirmed_local_binding(&fixture.library, &root, &server_mount, None);
        confirmed_capability(&fixture.library, &remote, &root, None);

        fixture
            .library
            .start_observation_generation(
                "generation_server_mismatch",
                &server,
                ObservationScope {
                    storage_root_id: root.clone(),
                    relative_prefix: None,
                },
            )
            .unwrap();
        observe(
            &fixture.library,
            "generation_server_mismatch",
            &object,
            ObservationStatus::FingerprintMismatch,
        );
        fixture
            .library
            .complete_observation_generation("generation_server_mismatch")
            .unwrap();

        fixture
            .library
            .start_observation_generation(
                "generation_remote_accessible",
                &remote,
                ObservationScope {
                    storage_root_id: root.clone(),
                    relative_prefix: None,
                },
            )
            .unwrap();
        observe(
            &fixture.library,
            "generation_remote_accessible",
            &object,
            ObservationStatus::Accessible,
        );
        fixture
            .library
            .complete_observation_generation("generation_remote_accessible")
            .unwrap();

        assert_eq!(
            fixture
                .library
                .media_availability(&media, &server)
                .unwrap()
                .state,
            NodeAvailabilityState::FingerprintMismatch
        );
        assert_eq!(
            fixture
                .library
                .media_availability(&media, &unbound)
                .unwrap()
                .state,
            NodeAvailabilityState::AccessibleOnAnotherNode
        );

        fixture
            .library
            .link_media_object_location(&media, &object, LocationVerificationState::Mismatch)
            .unwrap();
        assert_eq!(
            fixture
                .library
                .media_availability(&media, &unbound)
                .unwrap()
                .state,
            NodeAvailabilityState::FingerprintMismatch
        );
    }

    #[test]
    fn partial_binding_scope_cannot_supersede_older_full_root_outside_its_subtree() {
        let fixture = fixture();
        let root = create_root(&fixture.library, "root_partial_generation", true);
        let server = fixture.catalog.node_id().clone();
        let mount = fixture._root.path().join("partial-generation-mount");
        fs::create_dir_all(&mount).unwrap();
        confirmed_local_binding(&fixture.library, &root, &mount, None);
        let media = create_media(&fixture.library, "media_partial_generation");
        let inside = create_object(
            &fixture.library,
            "storage_partial_inside",
            &root,
            "Trips/inside.mp4",
        );
        let outside = create_object(
            &fixture.library,
            "storage_partial_outside",
            &root,
            "Archive/outside.mp4",
        );
        link(&fixture.library, &media, &inside);
        link(&fixture.library, &media, &outside);

        fixture
            .library
            .start_observation_generation(
                "generation_before_partial",
                &server,
                ObservationScope {
                    storage_root_id: root.clone(),
                    relative_prefix: None,
                },
            )
            .unwrap();
        observe(
            &fixture.library,
            "generation_before_partial",
            &inside,
            ObservationStatus::Accessible,
        );
        observe(
            &fixture.library,
            "generation_before_partial",
            &outside,
            ObservationStatus::Accessible,
        );
        fixture
            .library
            .complete_observation_generation("generation_before_partial")
            .unwrap();

        confirmed_local_binding(&fixture.library, &root, &mount, Some("Trips"));
        assert!(
            fixture
                .library
                .start_observation_generation(
                    "generation_partial_illegal_full",
                    &server,
                    ObservationScope {
                        storage_root_id: root.clone(),
                        relative_prefix: None,
                    },
                )
                .is_err()
        );
        fixture
            .library
            .start_observation_generation(
                "generation_after_partial",
                &server,
                ObservationScope {
                    storage_root_id: root,
                    relative_prefix: Some(RootRelativePath::parse("Trips").unwrap()),
                },
            )
            .unwrap();
        fixture
            .library
            .complete_observation_generation("generation_after_partial")
            .unwrap();

        let availability = fixture.library.media_availability(&media, &server).unwrap();
        let inside_detail = availability
            .locations
            .iter()
            .find(|location| location.storage_object_id == inside)
            .unwrap();
        assert_eq!(
            inside_detail.state,
            NodeAvailabilityState::MissingAfterCompletedObservation
        );
        assert!(
            !availability
                .locations
                .iter()
                .any(|location| location.storage_object_id == outside)
        );

        let connection = open_read_only_connection(fixture.catalog.database_path()).unwrap();
        let preserved_old_observation: u32 = connection
            .query_row(
                r#"
                SELECT COUNT(*) FROM node_asset_observations
                WHERE generation_id = 'generation_before_partial' AND storage_object_id = ?1
                "#,
                [outside.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(preserved_old_observation, 1);
    }

    #[test]
    fn observation_writes_are_unique_scoped_and_completion_is_idempotent() {
        let fixture = fixture();
        let root = create_root(&fixture.library, "root_generation_rules", true);
        let server = fixture.catalog.node_id().clone();
        let mount = fixture._root.path().join("mount");
        fs::create_dir_all(&mount).unwrap();
        confirmed_local_binding(&fixture.library, &root, &mount, None);
        let inside = create_object(
            &fixture.library,
            "storage_generation_inside",
            &root,
            "Trips/inside.mp4",
        );
        let outside = create_object(
            &fixture.library,
            "storage_generation_outside",
            &root,
            "Archive/outside.mp4",
        );
        fixture
            .library
            .start_observation_generation(
                "generation_rules",
                &server,
                ObservationScope {
                    storage_root_id: root,
                    relative_prefix: Some(RootRelativePath::parse("Trips").unwrap()),
                },
            )
            .unwrap();
        observe(
            &fixture.library,
            "generation_rules",
            &inside,
            ObservationStatus::Accessible,
        );
        assert!(matches!(
            fixture.library.record_asset_observation(
                "generation_rules",
                AssetObservationInput {
                    storage_object_id: inside.clone(),
                    status: ObservationStatus::Accessible,
                    observed_size_bytes: None,
                    observed_quick_fingerprint: None,
                },
            ),
            Err(StorageLibraryError::ObservationAlreadyRecorded { .. })
        ));
        assert!(matches!(
            fixture.library.record_asset_observation(
                "generation_rules",
                AssetObservationInput {
                    storage_object_id: outside,
                    status: ObservationStatus::Accessible,
                    observed_size_bytes: None,
                    observed_quick_fingerprint: None,
                },
            ),
            Err(StorageLibraryError::ObservationOutsideScope { .. })
        ));
        let first = fixture
            .library
            .complete_observation_generation("generation_rules")
            .unwrap();
        let retry = fixture
            .library
            .complete_observation_generation("generation_rules")
            .unwrap();
        assert_eq!(first, retry);
        assert!(matches!(
            fixture.library.record_asset_observation(
                "generation_rules",
                AssetObservationInput {
                    storage_object_id: inside,
                    status: ObservationStatus::Accessible,
                    observed_size_bytes: None,
                    observed_quick_fingerprint: None,
                },
            ),
            Err(StorageLibraryError::InvalidGenerationState { .. })
        ));
    }
}
