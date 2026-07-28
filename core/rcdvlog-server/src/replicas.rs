use crate::{
    ControlCatalog, ProjectMediaMembershipProjection, StorageLibraryError,
    open_read_only_connection, open_writable_connection,
};
use chrono::{SecondsFormat, Utc};
use rcdvlog_core::{
    Asset, Catalog, GlobalMediaVerificationState, MediaObjectId, MediaVerificationState,
    NodeAvailabilityState, NormalizedMediaProbe, OperationId, PROJECT_ASSET_MEDIA_LINKED,
    ProjectAssetAddedPayload, ProjectAssetId, ProjectAssetItem, ProjectAssetPage, ProjectId,
    RepresentationId,
};
use rcdvlog_sync::{
    BASELINE_VERSION, BaselineEnvelope, BaselinePreview, ReconcileRequest, ReconcileResponse,
    SyncError, persist_baseline_facts, reconcile, stored_baseline_digest,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectReplica {
    pub project_id: String,
    pub replica_relative_path: String,
    pub state: String,
    pub baseline_version: u32,
    pub baseline_digest: String,
    pub origin_node_id: Option<String>,
    pub server_node_id: String,
    pub registered_at: String,
    pub updated_at: String,
    pub reused_existing: bool,
}

#[derive(Debug, Error)]
pub enum ProjectReplicaError {
    #[error("baseline validation failed: {0}")]
    InvalidBaseline(String),
    #[error("project {project_id} is already registered with a different baseline")]
    DifferentBaseline { project_id: String },
    #[error("project replica {project_id} was not found")]
    NotFound { project_id: String },
    #[error("project replica path is outside the server data directory")]
    UnsafeReplicaPath,
    #[error(transparent)]
    Sync(#[from] SyncError),
    #[error(transparent)]
    Storage(#[from] StorageLibraryError),
    #[error(transparent)]
    Infrastructure(#[from] anyhow::Error),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl ProjectReplicaError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidBaseline(_) => "baseline_invalid",
            Self::DifferentBaseline { .. } => "baseline_different",
            Self::NotFound { .. } => "project_replica_not_found",
            Self::UnsafeReplicaPath => "project_replica_path_unsafe",
            Self::Sync(error) => error.code(),
            Self::Storage(_) => "storage_library_error",
            Self::Infrastructure(_) | Self::Database(_) | Self::Io(_) => "internal_error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub project_id: String,
    pub state: String,
    pub asset_count: u64,
    pub registered_at: String,
    pub updated_at: String,
}

/// A cursor-paginated page of registered project replicas, ordered by the
/// stable raw `project_id`. Mirrors the shape [`rcdvlog_core::ProjectAssetPage`]
/// uses (raw-ID cursor, `limit + 1` overflow detection) rather than the
/// envelope-cursor scheme the global library routes use -- the project
/// count here is bounded by admin-registered replicas, not archive size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPage {
    pub items: Vec<ProjectSummary>,
    pub next_cursor: Option<String>,
}

pub const MAX_ADD_TO_PROJECT_ITEMS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddToProjectRequest {
    pub media_object_ids: Vec<MediaObjectId>,
    pub acknowledge_unverified: bool,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AddToProjectDisposition {
    Ready,
    Added,
    AlreadyMember,
    AcknowledgementRequired,
    IdentityMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddToProjectItem {
    pub media_object_id: MediaObjectId,
    pub project_asset_id: Option<ProjectAssetId>,
    pub verification_state: GlobalMediaVerificationState,
    pub disposition: AddToProjectDisposition,
    pub remote_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddToProjectPreview {
    pub project_id: String,
    pub selected_count: usize,
    pub ready_count: usize,
    pub already_member_count: usize,
    pub acknowledgement_required_count: usize,
    pub mismatch_count: usize,
    pub items: Vec<AddToProjectItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddToProjectResult {
    pub project_id: String,
    pub idempotency_key: String,
    pub added_count: usize,
    pub already_member_count: usize,
    pub blocked_count: usize,
    pub items: Vec<AddToProjectItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRepresentation {
    pub representation_id: RepresentationId,
    pub state: String,
    pub available_on_server: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectReviewAsset {
    #[serde(flatten)]
    pub asset: ProjectAssetItem,
    pub materialized_revision: u64,
    pub active_note_revision_id: Option<String>,
    pub note_revision_count: usize,
    pub proxy: Option<ReviewRepresentation>,
    pub thumbnail: Option<ReviewRepresentation>,
    pub original_availability: Option<NodeAvailabilityState>,
    pub unresolved_conflict_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewMutationResult {
    pub asset: ProjectReviewAsset,
    pub stale_base_revision: bool,
}

#[derive(Debug, Clone)]
pub struct ProjectReplicaManager {
    data_dir: PathBuf,
    control: ControlCatalog,
}

impl ProjectReplicaManager {
    pub(crate) fn new(data_dir: PathBuf, control: ControlCatalog) -> Self {
        Self { data_dir, control }
    }

    /// Validates and describes an import without creating a replica or changing
    /// either catalog.
    pub fn preview(
        &self,
        envelope: &BaselineEnvelope,
    ) -> Result<BaselinePreview, ProjectReplicaError> {
        envelope.validate()?;
        let mut preview = envelope.preview();
        let library = self.control.storage_library();
        preview.proposed_library_matches = envelope
            .payload
            .project
            .assets
            .iter()
            .filter(|asset| {
                let linked_exists = asset
                    .media_object_id
                    .as_deref()
                    .and_then(|id| MediaObjectId::parse(id).ok())
                    .is_some_and(|id| library.get_media_object(&id).is_ok());
                linked_exists
                    || library
                        .find_candidates_by_quick_fingerprint(&asset.fingerprint, asset.size_bytes)
                        .is_ok_and(|matches| !matches.is_empty())
            })
            .count();
        Ok(preview)
    }

    /// Imports into an explicit temporary SQLite file, validates it, checkpoints
    /// WAL state, atomically renames it, and only then registers the replica.
    pub fn import_baseline(
        &self,
        envelope: &BaselineEnvelope,
    ) -> Result<ProjectReplica, ProjectReplicaError> {
        self.preview(envelope)?;
        let project_id = &envelope.payload.project.project_id;
        if envelope
            .payload
            .project
            .paired_server_node_id
            .as_deref()
            .is_some_and(|server| server != self.control.node_id().as_str())
        {
            return Err(ProjectReplicaError::InvalidBaseline(
                "project is paired with another server".to_string(),
            ));
        }
        if let Some(mut existing) = self.get_optional(project_id)? {
            if existing.baseline_digest != envelope.digest {
                return Err(ProjectReplicaError::DifferentBaseline {
                    project_id: project_id.clone(),
                });
            }
            existing.reused_existing = true;
            return Ok(existing);
        }

        let relative_path = replica_relative_path(project_id);
        let target = self.resolve_relative_path(&relative_path)?;
        if target.exists() {
            if stored_baseline_digest(&target)
                .map_err(ProjectReplicaError::Infrastructure)?
                .as_deref()
                != Some(envelope.digest.as_str())
            {
                return Err(ProjectReplicaError::DifferentBaseline {
                    project_id: project_id.clone(),
                });
            }
            return self.register(envelope, &relative_path, false);
        }

        let projects_dir = self.data_dir.join("projects");
        let temp_dir = self.data_dir.join("tmp");
        fs::create_dir_all(&projects_dir)?;
        fs::create_dir_all(&temp_dir)?;
        let temp_path = temp_dir.join(format!("replica-import-{}.sqlite3", Uuid::new_v4()));
        let prepare = (|| -> Result<(), ProjectReplicaError> {
            let imported = Catalog::import_project_baseline(
                &temp_path,
                &envelope.payload.project,
                self.control.node_id().as_str(),
                BASELINE_VERSION,
                &envelope.digest,
            )
            .map_err(ProjectReplicaError::Infrastructure)?;
            imported
                .pair_project_with_server(project_id, self.control.node_id().as_str())
                .map_err(ProjectReplicaError::Infrastructure)?;
            persist_baseline_facts(
                &imported,
                &envelope.payload.observations,
                &envelope.payload.representation_manifests,
            )
            .map_err(ProjectReplicaError::Infrastructure)?;
            let imported_projects = imported
                .list_projects()
                .map_err(ProjectReplicaError::Infrastructure)?;
            if imported_projects.len() != 1 || imported_projects[0].id != *project_id {
                return Err(ProjectReplicaError::InvalidBaseline(
                    "import did not produce exactly the selected project".to_string(),
                ));
            }
            checkpoint_project(&temp_path)?;
            Ok(())
        })();
        if let Err(error) = prepare {
            remove_sqlite_family(&temp_path);
            return Err(error);
        }
        if !temp_path.exists() {
            return Err(ProjectReplicaError::Infrastructure(anyhow::anyhow!(
                "prepared replica disappeared before registration"
            )));
        }
        fs::rename(&temp_path, &target)?;
        let registration = self.register(envelope, &relative_path, false);
        if registration.is_err() {
            // The durable replica intentionally remains for deterministic recovery.
            // A retry will verify its digest and finish registry repair.
        }
        registration
    }

    pub fn get(&self, project_id: &str) -> Result<ProjectReplica, ProjectReplicaError> {
        self.get_optional(project_id)?
            .ok_or_else(|| ProjectReplicaError::NotFound {
                project_id: project_id.to_string(),
            })
    }

    pub fn open_catalog(&self, project_id: &str) -> Result<Catalog, ProjectReplicaError> {
        let replica = self.get(project_id)?;
        let path = self.resolve_relative_path(&replica.replica_relative_path)?;
        Catalog::open_existing(path).map_err(ProjectReplicaError::Infrastructure)
    }

    /// Pages every registered project replica, with its current asset
    /// count, by the stable raw `project_id` order. The project count is
    /// bounded by admin-registered replicas (not by archive size), but this
    /// is still a cursor-paginated, explicitly-limited collection endpoint
    /// like every other one in this surface.
    pub fn list_projects(
        &self,
        limit: u32,
        after: Option<&str>,
    ) -> Result<ProjectPage, ProjectReplicaError> {
        if limit == 0 || limit > 200 {
            return Err(ProjectReplicaError::InvalidBaseline(
                "limit must be between 1 and 200".to_string(),
            ));
        }
        let connection = open_read_only_connection(self.control.database_path())
            .map_err(ProjectReplicaError::Infrastructure)?;
        let mut rows: Vec<(String, String, String, String)> = {
            let mut statement = connection.prepare(
                "SELECT project_id, state, registered_at, updated_at
                 FROM project_registry
                 WHERE (?1 IS NULL OR project_id > ?1)
                 ORDER BY project_id ASC
                 LIMIT ?2",
            )?;
            statement
                .query_map(params![after, i64::from(limit + 1)], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };

        let has_more = rows.len() > limit as usize;
        rows.truncate(limit as usize);

        let mut items = Vec::with_capacity(rows.len());
        for (project_id, state, registered_at, updated_at) in rows {
            let catalog = self.open_catalog(&project_id)?;
            let asset_count = catalog
                .count_project_assets(&project_id)
                .map_err(ProjectReplicaError::Infrastructure)?;
            items.push(ProjectSummary {
                project_id,
                state,
                asset_count,
                registered_at,
                updated_at,
            });
        }
        let next_cursor = has_more.then(|| {
            items
                .last()
                .expect("a page with more rows has a last item")
                .project_id
                .clone()
        });
        Ok(ProjectPage { items, next_cursor })
    }

    /// Pages one project's assets by the stable `project_asset_id` order,
    /// optionally scoped by a substring search. Never merged with the global
    /// library query path — see design requirement 4 of the parent plan.
    pub fn list_project_assets(
        &self,
        project_id: &str,
        limit: u32,
        after: Option<&str>,
        search: Option<&str>,
    ) -> Result<ProjectAssetPage, ProjectReplicaError> {
        if limit == 0 || limit > 200 {
            return Err(ProjectReplicaError::InvalidBaseline(
                "limit must be between 1 and 200".to_string(),
            ));
        }
        self.get(project_id)?;
        let catalog = self.open_catalog(project_id)?;
        catalog
            .list_project_asset_page(project_id, limit, after, search)
            .map_err(ProjectReplicaError::Infrastructure)
    }

    pub fn get_review_asset(
        &self,
        project_id: &str,
        asset_id: &str,
    ) -> Result<ProjectReviewAsset, ProjectReplicaError> {
        let catalog = self.open_catalog(project_id)?;
        let asset = catalog
            .get_asset(asset_id)
            .map_err(ProjectReplicaError::Infrastructure)?;
        if asset.project_id != project_id {
            return Err(ProjectReplicaError::NotFound {
                project_id: project_id.to_string(),
            });
        }
        let revisions = catalog
            .asset_revision_diagnostics(asset_id)
            .map_err(ProjectReplicaError::Infrastructure)?;
        let unresolved_conflict_count = catalog
            .operation_diagnostics(project_id)
            .map_err(ProjectReplicaError::Infrastructure)?
            .unresolved_conflicts
            .into_iter()
            .filter(|conflict| conflict.aggregate_id == asset_id)
            .count();
        let (proxy, thumbnail, original_availability) =
            if let Some(media_object_id) = &asset.media_object_id {
                (
                    self.current_review_representation(
                        media_object_id,
                        "proxy",
                        crate::PROXY_RECIPE_KEY,
                    )?,
                    self.current_review_representation(
                        media_object_id,
                        "thumbnail",
                        crate::THUMBNAIL_RECIPE_KEY,
                    )?,
                    self.control
                        .storage_library()
                        .media_availability(
                            &MediaObjectId::parse(media_object_id).map_err(|error| {
                                ProjectReplicaError::InvalidBaseline(error.to_string())
                            })?,
                            self.control.node_id(),
                        )
                        .ok()
                        .map(|availability| availability.state),
                )
            } else {
                (None, None, None)
            };
        Ok(ProjectReviewAsset {
            asset: ProjectAssetItem {
                project_asset_id: asset.id,
                filename: asset.filename,
                camera: asset.camera,
                media_object_id: asset.media_object_id,
                media_verification_state: asset.media_verification_state.as_str().to_string(),
                removed: asset.removed,
                rotation_degrees: i32::from(asset.rotation_degrees),
                labels: asset.labels,
                active_note: (!asset.notes.is_empty()).then_some(asset.notes),
                duration_seconds: asset.duration_seconds,
                best_time: asset.best_time,
            },
            materialized_revision: revisions.materialized_revision,
            active_note_revision_id: revisions
                .note_revisions
                .iter()
                .find(|revision| revision.active)
                .map(|revision| revision.revision_id.clone()),
            note_revision_count: revisions.note_revisions.len(),
            proxy,
            thumbnail,
            original_availability,
            unresolved_conflict_count,
        })
    }

    pub fn create_review_note(
        &self,
        project_id: &str,
        asset_id: &str,
        text: &str,
        base_revision: u64,
    ) -> Result<ReviewMutationResult, ProjectReplicaError> {
        if text.len() > 100_000 {
            return Err(ProjectReplicaError::InvalidBaseline(
                "note must not exceed 100000 bytes".to_string(),
            ));
        }
        self.review_mutation(project_id, asset_id, base_revision, |catalog| {
            catalog.create_note_revision(asset_id, text)
        })
    }

    pub fn add_review_label(
        &self,
        project_id: &str,
        asset_id: &str,
        label: &str,
        base_revision: u64,
    ) -> Result<ReviewMutationResult, ProjectReplicaError> {
        self.review_mutation(project_id, asset_id, base_revision, |catalog| {
            catalog.add_label(asset_id, label)
        })
    }

    pub fn remove_review_label(
        &self,
        project_id: &str,
        asset_id: &str,
        label: &str,
        base_revision: u64,
    ) -> Result<ReviewMutationResult, ProjectReplicaError> {
        self.review_mutation(project_id, asset_id, base_revision, |catalog| {
            catalog.remove_label(asset_id, label)
        })
    }

    pub fn set_review_removed(
        &self,
        project_id: &str,
        asset_id: &str,
        removed: bool,
        base_revision: u64,
    ) -> Result<ReviewMutationResult, ProjectReplicaError> {
        self.review_mutation(project_id, asset_id, base_revision, |catalog| {
            catalog.set_removed(asset_id, removed)
        })
    }

    pub fn set_review_rotation(
        &self,
        project_id: &str,
        asset_id: &str,
        rotation_degrees: u16,
        base_revision: u64,
    ) -> Result<ReviewMutationResult, ProjectReplicaError> {
        self.review_mutation(project_id, asset_id, base_revision, |catalog| {
            catalog.set_rotation(asset_id, rotation_degrees)
        })
    }

    fn review_mutation(
        &self,
        project_id: &str,
        asset_id: &str,
        base_revision: u64,
        mutate: impl FnOnce(&Catalog) -> anyhow::Result<Asset>,
    ) -> Result<ReviewMutationResult, ProjectReplicaError> {
        let before = self.get_review_asset(project_id, asset_id)?;
        let catalog = self.open_catalog(project_id)?;
        mutate(&catalog).map_err(|error| {
            if error.to_string().starts_with("invalid") {
                ProjectReplicaError::InvalidBaseline(error.to_string())
            } else {
                ProjectReplicaError::Infrastructure(error)
            }
        })?;
        Ok(ReviewMutationResult {
            stale_base_revision: base_revision != before.materialized_revision,
            asset: self.get_review_asset(project_id, asset_id)?,
        })
    }

    fn current_review_representation(
        &self,
        media_object_id: &str,
        kind: &str,
        recipe_key: &str,
    ) -> Result<Option<ReviewRepresentation>, ProjectReplicaError> {
        let connection = open_read_only_connection(self.control.database_path())
            .map_err(ProjectReplicaError::Infrastructure)?;
        let row = connection
            .query_row(
                r#"
                SELECT r.representation_id, r.state,
                       EXISTS(
                           SELECT 1 FROM representation_locations l
                           WHERE l.representation_id = r.representation_id
                             AND l.node_id = ?4 AND l.state = 'available'
                       )
                FROM representations r
                WHERE r.media_object_id = ?1 AND r.kind = ?2 AND r.recipe_key = ?3
                ORDER BY r.completed_at DESC, r.created_at DESC, r.representation_id ASC
                LIMIT 1
                "#,
                params![
                    media_object_id,
                    kind,
                    recipe_key,
                    self.control.node_id().as_str()
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        row.map(|(id, state, available_on_server)| {
            Ok(ReviewRepresentation {
                representation_id: RepresentationId::parse(id)
                    .map_err(|error| ProjectReplicaError::InvalidBaseline(error.to_string()))?,
                state,
                available_on_server,
            })
        })
        .transpose()
    }

    pub fn preview_add_to_project(
        &self,
        project_id: &str,
        request: &AddToProjectRequest,
    ) -> Result<AddToProjectPreview, ProjectReplicaError> {
        validate_add_request(request)?;
        let catalog = self.open_catalog(project_id)?;
        let library = self.control.storage_library();
        let mut items = Vec::with_capacity(request.media_object_ids.len());
        for media_object_id in unique_media_ids(&request.media_object_ids) {
            let media = library.get_media_object(&media_object_id)?;
            let existing = catalog
                .project_asset_for_media_object(project_id, media_object_id.as_str())
                .map_err(ProjectReplicaError::Infrastructure)?;
            let disposition = if existing.is_some() {
                AddToProjectDisposition::AlreadyMember
            } else {
                match media.verification_state {
                    GlobalMediaVerificationState::Verified => AddToProjectDisposition::Ready,
                    GlobalMediaVerificationState::Unverified
                    | GlobalMediaVerificationState::Provisional => {
                        AddToProjectDisposition::AcknowledgementRequired
                    }
                    GlobalMediaVerificationState::Mismatch => {
                        AddToProjectDisposition::IdentityMismatch
                    }
                }
            };
            let availability =
                library.media_availability(&media_object_id, library.local_node_id())?;
            let remote_only = availability.state
                == rcdvlog_core::NodeAvailabilityState::AccessibleHere
                && !availability.locations.iter().any(|location| {
                    &location.node_id != library.local_node_id()
                        && location.state == rcdvlog_core::NodeAvailabilityState::AccessibleHere
                });
            items.push(AddToProjectItem {
                media_object_id,
                project_asset_id: existing
                    .as_ref()
                    .map(|asset| ProjectAssetId::parse(&asset.id))
                    .transpose()
                    .map_err(|error| ProjectReplicaError::InvalidBaseline(error.to_string()))?,
                verification_state: media.verification_state,
                disposition,
                remote_only,
            });
        }
        Ok(AddToProjectPreview {
            project_id: project_id.to_string(),
            selected_count: items.len(),
            ready_count: items
                .iter()
                .filter(|item| item.disposition == AddToProjectDisposition::Ready)
                .count(),
            already_member_count: items
                .iter()
                .filter(|item| item.disposition == AddToProjectDisposition::AlreadyMember)
                .count(),
            acknowledgement_required_count: items
                .iter()
                .filter(|item| item.disposition == AddToProjectDisposition::AcknowledgementRequired)
                .count(),
            mismatch_count: items
                .iter()
                .filter(|item| item.disposition == AddToProjectDisposition::IdentityMismatch)
                .count(),
            items,
        })
    }

    pub fn add_media_to_project(
        &self,
        project_id: &str,
        request: &AddToProjectRequest,
    ) -> Result<AddToProjectResult, ProjectReplicaError> {
        validate_add_request(request)?;
        let preview = self.preview_add_to_project(project_id, request)?;
        let catalog = self.open_catalog(project_id)?;
        let mut items = Vec::with_capacity(preview.items.len());

        for mut item in preview.items {
            if item.disposition == AddToProjectDisposition::AlreadyMember {
                items.push(item);
                continue;
            }
            if item.disposition == AddToProjectDisposition::IdentityMismatch
                || (item.disposition == AddToProjectDisposition::AcknowledgementRequired
                    && !request.acknowledge_unverified)
            {
                items.push(item);
                continue;
            }

            let asset_id = deterministic_server_asset_id(project_id, &item.media_object_id);
            let asset = match catalog.get_asset(asset_id.as_str()) {
                Ok(asset) => asset,
                Err(_) => {
                    let payload = self.portable_payload(&item.media_object_id, &asset_id)?;
                    catalog
                        .add_portable_project_asset(project_id, payload)
                        .map_err(ProjectReplicaError::Infrastructure)?
                }
            };
            let verification_state = match item.verification_state {
                GlobalMediaVerificationState::Verified => MediaVerificationState::Verified,
                GlobalMediaVerificationState::Unverified
                | GlobalMediaVerificationState::Provisional => MediaVerificationState::Provisional,
                GlobalMediaVerificationState::Mismatch => unreachable!("mismatches are blocked"),
            };
            catalog
                .link_project_asset_media(
                    &asset.id,
                    item.media_object_id.as_str(),
                    verification_state,
                )
                .map_err(ProjectReplicaError::Infrastructure)?;
            item.project_asset_id = Some(asset_id);
            item.disposition = AddToProjectDisposition::Added;
            items.push(item);
        }

        // The operation log is authoritative. If this process previously
        // stopped after its replica commit, this replay repairs the lagging
        // control-catalog membership projection without duplicating assets.
        self.repair_membership_projections(project_id)?;
        Ok(AddToProjectResult {
            project_id: project_id.to_string(),
            idempotency_key: request.idempotency_key.clone(),
            added_count: items
                .iter()
                .filter(|item| item.disposition == AddToProjectDisposition::Added)
                .count(),
            already_member_count: items
                .iter()
                .filter(|item| item.disposition == AddToProjectDisposition::AlreadyMember)
                .count(),
            blocked_count: items
                .iter()
                .filter(|item| {
                    matches!(
                        item.disposition,
                        AddToProjectDisposition::AcknowledgementRequired
                            | AddToProjectDisposition::IdentityMismatch
                    )
                })
                .count(),
            items,
        })
    }

    fn portable_payload(
        &self,
        media_object_id: &MediaObjectId,
        asset_id: &ProjectAssetId,
    ) -> Result<ProjectAssetAddedPayload, ProjectReplicaError> {
        let library = self.control.storage_library();
        let media = library.get_media_object(media_object_id)?;
        let facts = library.media_search_facts(media_object_id)?;
        let connection = open_read_only_connection(self.control.database_path())
            .map_err(ProjectReplicaError::Infrastructure)?;
        let probe_json = connection
            .query_row(
                r#"
                SELECT pf.probe_json
                FROM media_object_locations mol
                JOIN media_probe_facts pf ON pf.storage_object_id = mol.storage_object_id
                WHERE mol.media_object_id = ?1
                ORDER BY pf.captured_at DESC, pf.storage_object_id ASC
                LIMIT 1
                "#,
                [media_object_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let probe = probe_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<NormalizedMediaProbe>(raw).ok());
        let camera = probe
            .as_ref()
            .and_then(|probe| probe.camera_string.clone())
            .or_else(|| facts.camera_model.clone())
            .or(facts.camera_make.clone())
            .unwrap_or_default();
        let size_bytes = media.size_bytes.unwrap_or_default();
        Ok(ProjectAssetAddedPayload {
            id: asset_id.as_str().to_string(),
            fingerprint: media
                .full_content_hash
                .clone()
                .or(media.quick_fingerprint.clone())
                .unwrap_or_else(|| media_object_id.as_str().to_string()),
            filename: facts.filename,
            size_bytes,
            duration_seconds: probe.as_ref().map_or(0.0, |value| value.duration_seconds),
            frame_rate: probe.as_ref().map_or(0.0, |value| value.frame_rate),
            width: probe.as_ref().map_or(0, |value| value.encoded_width),
            height: probe.as_ref().map_or(0, |value| value.encoded_height),
            codec: probe
                .as_ref()
                .map_or_else(String::new, |value| value.codec.clone()),
            camera,
            captured_at_raw: probe
                .as_ref()
                .and_then(|value| value.captured_at_raw.clone()),
            best_time: probe.as_ref().and_then(|value| value.best_time.clone()),
            metadata_rotation_degrees: probe
                .as_ref()
                .map_or(0, |value| value.metadata_rotation_degrees),
            default_rotation_degrees: 0,
            created_at: media.created_at,
        })
    }

    pub fn reconcile(
        &self,
        project_id: &str,
        request: &ReconcileRequest,
    ) -> Result<ReconcileResponse, ProjectReplicaError> {
        let catalog = self.open_catalog(project_id)?;
        let response = reconcile(
            &catalog,
            project_id,
            self.control.node_id().as_str(),
            request,
        )
        .map_err(map_sync_error)?;
        self.repair_membership_projections(project_id)?;
        Ok(response)
    }

    pub fn add_project_asset(
        &self,
        project_id: &str,
        payload: ProjectAssetAddedPayload,
    ) -> Result<Asset, ProjectReplicaError> {
        let catalog = self.open_catalog(project_id)?;
        catalog
            .add_portable_project_asset(project_id, payload)
            .map_err(ProjectReplicaError::Infrastructure)
    }

    pub fn link_project_asset_media(
        &self,
        project_id: &str,
        asset_id: &str,
        media_object_id: &str,
        verification_state: MediaVerificationState,
    ) -> Result<Asset, ProjectReplicaError> {
        let media_id = MediaObjectId::parse(media_object_id)
            .map_err(|error| ProjectReplicaError::InvalidBaseline(error.to_string()))?;
        self.control.storage_library().get_media_object(&media_id)?;
        let catalog = self.open_catalog(project_id)?;
        let asset = catalog
            .link_project_asset_media(asset_id, media_object_id, verification_state)
            .map_err(ProjectReplicaError::Infrastructure)?;
        self.repair_membership_projections(project_id)?;
        Ok(asset)
    }

    /// Rebuilds the control-catalog projection from the durable operation log.
    /// This is deliberately replayable after a crash between the two commits.
    pub fn repair_membership_projections(
        &self,
        project_id: &str,
    ) -> Result<usize, ProjectReplicaError> {
        let catalog = self.open_catalog(project_id)?;
        let project_id_value = ProjectId::parse(project_id)
            .map_err(|error| ProjectReplicaError::InvalidBaseline(error.to_string()))?;
        let library = self.control.storage_library();
        let connection = open_writable_connection(self.control.database_path())
            .map_err(ProjectReplicaError::Infrastructure)?;
        let mut repaired = 0;
        for operation in catalog
            .list_operations(project_id)
            .map_err(ProjectReplicaError::Infrastructure)?
        {
            if operation.operation_type != PROJECT_ASSET_MEDIA_LINKED {
                continue;
            }
            let Some(media_id) = operation
                .payload
                .get("mediaObjectId")
                .and_then(|value| value.as_str())
            else {
                continue;
            };
            let projection = ProjectMediaMembershipProjection {
                project_id: project_id_value.clone(),
                project_asset_id: ProjectAssetId::parse(&operation.aggregate_id)
                    .map_err(|error| ProjectReplicaError::InvalidBaseline(error.to_string()))?,
                media_object_id: MediaObjectId::parse(media_id)
                    .map_err(|error| ProjectReplicaError::InvalidBaseline(error.to_string()))?,
                source_operation_id: OperationId::parse(&operation.operation_id)
                    .map_err(|error| ProjectReplicaError::InvalidBaseline(error.to_string()))?,
                projected_at: now(),
            };
            match library.upsert_project_media_membership_projection(projection) {
                Ok(_) => {
                    repaired += 1;
                    record_projection_repair(
                        &connection,
                        project_id,
                        &operation.operation_id,
                        "completed",
                        None,
                    )?;
                }
                Err(StorageLibraryError::NotFound { .. }) => {
                    // The project operation remains authoritative. The projection
                    // can be repaired once the referenced global media row arrives.
                    record_projection_repair(
                        &connection,
                        project_id,
                        &operation.operation_id,
                        "pending",
                        Some("media_object_not_found"),
                    )?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(repaired)
    }

    fn register(
        &self,
        envelope: &BaselineEnvelope,
        relative_path: &str,
        reused_existing: bool,
    ) -> Result<ProjectReplica, ProjectReplicaError> {
        let now = now();
        let connection = open_writable_connection(self.control.database_path())
            .map_err(ProjectReplicaError::Infrastructure)?;
        connection.execute(
            "INSERT INTO project_registry(project_id, replica_relative_path, state, registered_at, created_at, updated_at, baseline_version, baseline_digest, origin_node_id, server_node_id) VALUES (?1, ?2, 'registered', ?3, ?3, ?3, ?4, ?5, ?6, ?7)",
            params![
                envelope.payload.project.project_id,
                relative_path,
                now,
                BASELINE_VERSION,
                envelope.digest,
                envelope.payload.project.origin_node_id,
                self.control.node_id().as_str(),
            ],
        )?;
        let mut registered = self.get(&envelope.payload.project.project_id)?;
        registered.reused_existing = reused_existing;
        Ok(registered)
    }

    fn get_optional(
        &self,
        project_id: &str,
    ) -> Result<Option<ProjectReplica>, ProjectReplicaError> {
        let connection = open_read_only_connection(self.control.database_path())
            .map_err(ProjectReplicaError::Infrastructure)?;
        connection
            .query_row(
                "SELECT project_id, replica_relative_path, state, COALESCE(baseline_version, 0), COALESCE(baseline_digest, ''), origin_node_id, COALESCE(server_node_id, ''), registered_at, updated_at FROM project_registry WHERE project_id=?1",
                [project_id],
                |row| {
                    Ok(ProjectReplica {
                        project_id: row.get(0)?,
                        replica_relative_path: row.get(1)?,
                        state: row.get(2)?,
                        baseline_version: row.get::<_, i64>(3)? as u32,
                        baseline_digest: row.get(4)?,
                        origin_node_id: row.get(5)?,
                        server_node_id: row.get(6)?,
                        registered_at: row.get(7)?,
                        updated_at: row.get(8)?,
                        reused_existing: false,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    fn resolve_relative_path(&self, relative: &str) -> Result<PathBuf, ProjectReplicaError> {
        let relative = Path::new(relative);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(ProjectReplicaError::UnsafeReplicaPath);
        }
        Ok(self.data_dir.join(relative))
    }
}

fn validate_add_request(request: &AddToProjectRequest) -> Result<(), ProjectReplicaError> {
    if request.media_object_ids.is_empty()
        || request.media_object_ids.len() > MAX_ADD_TO_PROJECT_ITEMS
    {
        return Err(ProjectReplicaError::InvalidBaseline(format!(
            "mediaObjectIds must contain between 1 and {MAX_ADD_TO_PROJECT_ITEMS} items"
        )));
    }
    if request.idempotency_key.trim().is_empty() || request.idempotency_key.len() > 255 {
        return Err(ProjectReplicaError::InvalidBaseline(
            "idempotencyKey must contain between 1 and 255 bytes".to_string(),
        ));
    }
    Ok(())
}

fn unique_media_ids(values: &[MediaObjectId]) -> Vec<MediaObjectId> {
    let mut values = values.to_vec();
    values.sort();
    values.dedup();
    values
}

fn deterministic_server_asset_id(
    project_id: &str,
    media_object_id: &MediaObjectId,
) -> ProjectAssetId {
    let digest = blake3::hash(format!("{project_id}\0{}", media_object_id.as_str()).as_bytes());
    ProjectAssetId::parse(format!("asset_server_{}", &digest.to_hex()[..32]))
        .expect("a hex digest always forms a valid project asset ID")
}

fn replica_relative_path(project_id: &str) -> String {
    format!(
        "projects/{}.sqlite3",
        blake3::hash(project_id.as_bytes()).to_hex()
    )
}

fn checkpoint_project(path: &Path) -> Result<(), ProjectReplicaError> {
    let connection = rusqlite::Connection::open(path)?;
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(())
}

fn remove_sqlite_family(path: &Path) {
    for candidate in [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ] {
        let _ = fs::remove_file(candidate);
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn record_projection_repair(
    connection: &rusqlite::Connection,
    project_id: &str,
    operation_id: &str,
    state: &str,
    error_code: Option<&str>,
) -> Result<(), ProjectReplicaError> {
    let now = now();
    connection.execute(
        "INSERT INTO project_projection_repairs(project_id, source_operation_id, projection_kind, state, last_error_code, created_at, updated_at) VALUES (?1, ?2, 'project_media_membership', ?3, ?4, ?5, ?5) ON CONFLICT(project_id, source_operation_id, projection_kind) DO UPDATE SET state=excluded.state, last_error_code=excluded.last_error_code, updated_at=excluded.updated_at",
        params![project_id, operation_id, state, error_code, now],
    )?;
    Ok(())
}

fn map_sync_error(error: anyhow::Error) -> ProjectReplicaError {
    match error.downcast::<SyncError>() {
        Ok(error) => ProjectReplicaError::Sync(error),
        Err(error) => ProjectReplicaError::Infrastructure(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ServerConfig;
    use rcdvlog_core::ProjectAssetAddedPayload;
    use rcdvlog_sync::{BaselineEnvelope, RepresentationManifestFact};
    use serde_json::json;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    struct Fixture {
        _root: TempDir,
        manager: ProjectReplicaManager,
    }

    impl Fixture {
        fn new() -> Self {
            let root = TempDir::new().unwrap();
            let data = root.path().join("state");
            let derived = root.path().join("derived");
            let backup = root.path().join("backup");
            fs::create_dir_all(&derived).unwrap();
            fs::create_dir_all(&backup).unwrap();
            let config = ServerConfig::from_values(BTreeMap::from([
                ("RCDVLOG_DATA_DIR".to_string(), data.display().to_string()),
                (
                    "RCDVLOG_DERIVED_DIR".to_string(),
                    derived.display().to_string(),
                ),
                (
                    "RCDVLOG_BACKUP_DIR".to_string(),
                    backup.display().to_string(),
                ),
                ("RCDVLOG_FFMPEG".to_string(), "/usr/bin/true".to_string()),
                ("RCDVLOG_FFPROBE".to_string(), "/usr/bin/true".to_string()),
                ("RCDVLOG_EXIFTOOL".to_string(), "/usr/bin/true".to_string()),
            ]))
            .unwrap();
            fs::create_dir_all(&config.data_dir).unwrap();
            let control =
                ControlCatalog::open_or_create(&config.data_dir, &config.diagnostics()).unwrap();
            let manager = control.project_replicas();
            Self {
                _root: root,
                manager,
            }
        }
    }

    fn baseline(root: &TempDir) -> BaselineEnvelope {
        let catalog = Catalog::open(root.path().join("desktop.sqlite3")).unwrap();
        catalog.set_local_node_id("node_mac").unwrap();
        let project = catalog.create_project("Trip").unwrap();
        catalog
            .add_portable_project_asset(
                &project.id,
                ProjectAssetAddedPayload {
                    id: "asset_desktop".to_string(),
                    fingerprint: "fingerprint".to_string(),
                    filename: "clip.mov".to_string(),
                    size_bytes: 5,
                    duration_seconds: 1.0,
                    frame_rate: 30.0,
                    width: 1920,
                    height: 1080,
                    codec: "h264".to_string(),
                    camera: "camera".to_string(),
                    captured_at_raw: None,
                    best_time: None,
                    metadata_rotation_degrees: 0,
                    default_rotation_degrees: 0,
                    created_at: "2026-07-23T00:00:00Z".to_string(),
                },
            )
            .unwrap();
        BaselineEnvelope::export(&catalog, &project.id, vec![], vec![]).unwrap()
    }

    #[test]
    fn preview_is_read_only_and_import_is_atomic_and_idempotent() {
        let fixture = Fixture::new();
        let source_root = TempDir::new().unwrap();
        let envelope = baseline(&source_root);
        open_writable_connection(fixture.manager.control.database_path())
            .unwrap()
            .execute(
                "INSERT INTO media_objects(media_object_id, size_bytes, quick_fingerprint, verification_state, created_at, updated_at) VALUES('media_preview', 5, 'fingerprint', 'provisional', ?1, ?1)",
                [now()],
            )
            .unwrap();
        let before: i64 = open_read_only_connection(fixture.manager.control.database_path())
            .unwrap()
            .query_row("SELECT COUNT(*) FROM project_registry", [], |row| {
                row.get(0)
            })
            .unwrap();
        let preview = fixture.manager.preview(&envelope).unwrap();
        assert_eq!(preview.asset_count, 1);
        assert_eq!(preview.proposed_library_matches, 1);
        let after: i64 = open_read_only_connection(fixture.manager.control.database_path())
            .unwrap()
            .query_row("SELECT COUNT(*) FROM project_registry", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(before, after);

        let imported = fixture.manager.import_baseline(&envelope).unwrap();
        assert!(!imported.reused_existing);
        assert_eq!(imported.baseline_digest, envelope.digest);
        assert_eq!(
            fixture
                .manager
                .open_catalog(&envelope.payload.project.project_id)
                .unwrap()
                .list_assets(&envelope.payload.project.project_id)
                .unwrap()
                .len(),
            1
        );
        let retried = fixture.manager.import_baseline(&envelope).unwrap();
        assert!(retried.reused_existing);
        assert_eq!(
            retried.replica_relative_path,
            imported.replica_relative_path
        );
    }

    #[test]
    fn different_baseline_never_overwrites_registered_replica() {
        let fixture = Fixture::new();
        let source_root = TempDir::new().unwrap();
        let envelope = baseline(&source_root);
        let registered = fixture.manager.import_baseline(&envelope).unwrap();
        let replica_path = fixture
            .manager
            .resolve_relative_path(&registered.replica_relative_path)
            .unwrap();
        let bytes_before = fs::read(&replica_path).unwrap();

        let mut changed = envelope.clone();
        changed.payload.project.name = "Different".to_string();
        changed.digest = rcdvlog_sync::canonical_digest(&changed.payload).unwrap();
        assert!(matches!(
            fixture.manager.import_baseline(&changed),
            Err(ProjectReplicaError::DifferentBaseline { .. })
        ));
        assert_eq!(fs::read(replica_path).unwrap(), bytes_before);
    }

    #[test]
    fn failed_temporary_import_leaves_no_registry_or_partial_replica() {
        let fixture = Fixture::new();
        let source_root = TempDir::new().unwrap();
        let mut envelope = baseline(&source_root);
        envelope.payload.representation_manifests = vec![
            RepresentationManifestFact {
                manifest_id: "same_manifest".to_string(),
                origin_node_id: "node_mac".to_string(),
                origin_sequence: 1,
                payload: json!({"representationId": "rep_1"}),
            },
            RepresentationManifestFact {
                manifest_id: "same_manifest".to_string(),
                origin_node_id: "node_server".to_string(),
                origin_sequence: 1,
                payload: json!({"representationId": "rep_2"}),
            },
        ];
        envelope.digest = rcdvlog_sync::canonical_digest(&envelope.payload).unwrap();
        assert!(fixture.manager.import_baseline(&envelope).is_err());
        assert!(matches!(
            fixture.manager.get(&envelope.payload.project.project_id),
            Err(ProjectReplicaError::NotFound { .. })
        ));
        let target = fixture
            .manager
            .resolve_relative_path(&replica_relative_path(&envelope.payload.project.project_id))
            .unwrap();
        assert!(!target.exists());
        let temp_entries = fs::read_dir(fixture.manager.data_dir.join("tmp"))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(temp_entries.is_empty());
    }

    #[test]
    fn membership_projection_repairs_after_project_commit_and_is_idempotent() {
        let fixture = Fixture::new();
        let source_root = TempDir::new().unwrap();
        let envelope = baseline(&source_root);
        fixture.manager.import_baseline(&envelope).unwrap();
        let connection = open_writable_connection(fixture.manager.control.database_path()).unwrap();
        connection.execute(
            "INSERT INTO media_objects(media_object_id, size_bytes, verification_state, created_at, updated_at) VALUES('media_fixture', 5, 'provisional', ?1, ?1)",
            [now()],
        ).unwrap();

        // Simulate a crash boundary: the authoritative project commit succeeds,
        // but the control-catalog projection has not run yet.
        let replica = fixture
            .manager
            .open_catalog(&envelope.payload.project.project_id)
            .unwrap();
        replica
            .link_project_asset_media(
                "asset_desktop",
                "media_fixture",
                MediaVerificationState::Provisional,
            )
            .unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM project_media_memberships",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert_eq!(
            fixture
                .manager
                .repair_membership_projections(&envelope.payload.project.project_id)
                .unwrap(),
            1
        );
        fixture
            .manager
            .repair_membership_projections(&envelope.payload.project.project_id)
            .unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM project_media_memberships",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }

    fn seed_global_media(fixture: &Fixture, id: &str, state: GlobalMediaVerificationState) {
        fixture
            .manager
            .control
            .storage_library()
            .create_media_object(crate::NewMediaObject {
                id: MediaObjectId::parse(id).unwrap(),
                size_bytes: Some(42),
                quick_fingerprint: Some(blake3::hash(id.as_bytes()).to_hex().to_string()),
                full_content_hash: (state == GlobalMediaVerificationState::Verified)
                    .then(|| "a".repeat(64)),
                verification_state: state,
            })
            .unwrap();
    }

    #[test]
    fn add_to_project_deduplicates_from_replica_and_repairs_projection() {
        let fixture = Fixture::new();
        let source_root = TempDir::new().unwrap();
        let envelope = baseline(&source_root);
        let project_id = envelope.payload.project.project_id.clone();
        fixture.manager.import_baseline(&envelope).unwrap();
        seed_global_media(
            &fixture,
            "media_add_verified",
            GlobalMediaVerificationState::Verified,
        );
        let request = AddToProjectRequest {
            media_object_ids: vec![MediaObjectId::parse("media_add_verified").unwrap()],
            acknowledge_unverified: false,
            idempotency_key: "request-add-1".to_string(),
        };

        let preview = fixture
            .manager
            .preview_add_to_project(&project_id, &request)
            .unwrap();
        assert_eq!(preview.ready_count, 1);
        let first = fixture
            .manager
            .add_media_to_project(&project_id, &request)
            .unwrap();
        assert_eq!(first.added_count, 1);
        assert_eq!(first.blocked_count, 0);

        let retried = fixture
            .manager
            .add_media_to_project(&project_id, &request)
            .unwrap();
        assert_eq!(retried.added_count, 0);
        assert_eq!(retried.already_member_count, 1);
        let catalog = fixture.manager.open_catalog(&project_id).unwrap();
        assert_eq!(catalog.count_project_assets(&project_id).unwrap(), 2);
        let projected: i64 = open_read_only_connection(fixture.manager.control.database_path())
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM project_media_memberships WHERE media_object_id='media_add_verified'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(projected, 1);
    }

    #[test]
    fn add_to_project_requires_acknowledgement_and_blocks_mismatch() {
        let fixture = Fixture::new();
        let source_root = TempDir::new().unwrap();
        let envelope = baseline(&source_root);
        let project_id = envelope.payload.project.project_id.clone();
        fixture.manager.import_baseline(&envelope).unwrap();
        seed_global_media(
            &fixture,
            "media_add_provisional",
            GlobalMediaVerificationState::Provisional,
        );
        seed_global_media(
            &fixture,
            "media_add_mismatch",
            GlobalMediaVerificationState::Mismatch,
        );
        let mut request = AddToProjectRequest {
            media_object_ids: vec![
                MediaObjectId::parse("media_add_provisional").unwrap(),
                MediaObjectId::parse("media_add_mismatch").unwrap(),
            ],
            acknowledge_unverified: false,
            idempotency_key: "request-add-unsafe".to_string(),
        };
        let blocked = fixture
            .manager
            .add_media_to_project(&project_id, &request)
            .unwrap();
        assert_eq!(blocked.added_count, 0);
        assert_eq!(blocked.blocked_count, 2);

        request.acknowledge_unverified = true;
        let acknowledged = fixture
            .manager
            .add_media_to_project(&project_id, &request)
            .unwrap();
        assert_eq!(acknowledged.added_count, 1);
        assert_eq!(acknowledged.blocked_count, 1);
        assert!(acknowledged.items.iter().any(|item| {
            item.media_object_id.as_str() == "media_add_mismatch"
                && item.disposition == AddToProjectDisposition::IdentityMismatch
        }));
    }

    #[test]
    fn review_commands_return_canonical_state_and_stale_notes_still_commit() {
        let fixture = Fixture::new();
        let source_root = TempDir::new().unwrap();
        let envelope = baseline(&source_root);
        let project_id = envelope.payload.project.project_id.clone();
        fixture.manager.import_baseline(&envelope).unwrap();

        let before = fixture
            .manager
            .get_review_asset(&project_id, "asset_desktop")
            .unwrap();
        let note = fixture
            .manager
            .create_review_note(
                &project_id,
                "asset_desktop",
                "Web revision",
                before.materialized_revision.saturating_sub(1),
            )
            .unwrap();
        assert!(note.stale_base_revision);
        assert_eq!(
            note.asset.asset.active_note.as_deref(),
            Some("Web revision")
        );
        assert!(note.asset.note_revision_count >= 2);

        let label = fixture
            .manager
            .add_review_label(
                &project_id,
                "asset_desktop",
                "arrival",
                note.asset.materialized_revision,
            )
            .unwrap();
        assert!(!label.stale_base_revision);
        assert_eq!(label.asset.asset.labels, vec!["arrival"]);
        let rotated = fixture
            .manager
            .set_review_rotation(
                &project_id,
                "asset_desktop",
                90,
                label.asset.materialized_revision,
            )
            .unwrap();
        assert_eq!(rotated.asset.asset.rotation_degrees, 90);
        let removed = fixture
            .manager
            .set_review_removed(
                &project_id,
                "asset_desktop",
                true,
                rotated.asset.materialized_revision,
            )
            .unwrap();
        assert!(removed.asset.asset.removed);
        let restored = fixture
            .manager
            .remove_review_label(
                &project_id,
                "asset_desktop",
                "arrival",
                removed.asset.materialized_revision,
            )
            .unwrap();
        assert!(restored.asset.asset.labels.is_empty());
    }

    /// A registered replica with `count` portable assets named
    /// `asset_000.mov` .. `asset_{count-1:03}.mov`, imported through the same
    /// `import_baseline` path a real desktop sync would use.
    struct AssetPageFixture {
        _server_root: TempDir,
        _source_root: TempDir,
        manager: ProjectReplicaManager,
        project_id: String,
    }

    fn portable_asset_payload(index: usize) -> ProjectAssetAddedPayload {
        ProjectAssetAddedPayload {
            id: format!("asset_{index:03}"),
            fingerprint: format!("fingerprint_{index:03}"),
            filename: format!("asset_{index:03}.mov"),
            size_bytes: 5,
            duration_seconds: 1.0,
            frame_rate: 30.0,
            width: 1920,
            height: 1080,
            codec: "h264".to_string(),
            camera: "camera".to_string(),
            captured_at_raw: None,
            best_time: None,
            metadata_rotation_degrees: 0,
            default_rotation_degrees: 0,
            created_at: "2026-07-23T00:00:00Z".to_string(),
        }
    }

    fn replica_fixture_with_assets(count: usize) -> AssetPageFixture {
        let fixture = Fixture::new();
        let source_root = TempDir::new().unwrap();
        let catalog = Catalog::open(source_root.path().join("desktop.sqlite3")).unwrap();
        catalog.set_local_node_id("node_mac").unwrap();
        let project = catalog.create_project("Trip").unwrap();
        for index in 0..count {
            catalog
                .add_portable_project_asset(&project.id, portable_asset_payload(index))
                .unwrap();
        }
        let envelope = BaselineEnvelope::export(&catalog, &project.id, vec![], vec![]).unwrap();
        let registered = fixture.manager.import_baseline(&envelope).unwrap();
        AssetPageFixture {
            _server_root: fixture._root,
            _source_root: source_root,
            manager: fixture.manager,
            project_id: registered.project_id,
        }
    }

    #[test]
    fn project_assets_page_deterministically_and_exclude_absolute_paths() {
        let fixture = replica_fixture_with_assets(25);
        let mut seen: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = fixture
                .manager
                .list_project_assets(&fixture.project_id, 10, cursor.as_deref(), None)
                .unwrap();
            for item in &page.items {
                let encoded = serde_json::to_string(item).unwrap();
                assert!(!encoded.contains('/'), "asset DTO leaked a path: {encoded}");
                seen.push(item.project_asset_id.clone());
            }
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert_eq!(seen.len(), 25);
        let mut sorted = seen.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 25, "pages repeated an asset");
    }

    #[test]
    fn project_asset_search_is_scoped_to_one_project() {
        let fixture = replica_fixture_with_assets(3);
        let page = fixture
            .manager
            .list_project_assets(&fixture.project_id, 10, None, Some("asset_00"))
            .unwrap();
        assert!(!page.items.is_empty());
        assert!(page.items.iter().all(|item| item.filename.contains("00")));
    }

    #[test]
    fn listing_projects_reports_asset_counts() {
        let fixture = replica_fixture_with_assets(4);
        let page = fixture.manager.list_projects(10, None).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].project_id, fixture.project_id);
        assert_eq!(page.items[0].asset_count, 4);
        assert_eq!(page.next_cursor, None);
    }

    #[test]
    fn listing_projects_pages_deterministically_across_many_registered_replicas() {
        let fixture = Fixture::new();
        let mut project_ids: Vec<String> = Vec::new();
        for _ in 0..5 {
            let source = TempDir::new().unwrap();
            let envelope = baseline(&source);
            let registered = fixture.manager.import_baseline(&envelope).unwrap();
            project_ids.push(registered.project_id);
        }
        project_ids.sort();

        let mut seen: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = fixture.manager.list_projects(2, cursor.as_deref()).unwrap();
            assert!(page.items.len() <= 2, "page exceeded its explicit limit");
            for item in &page.items {
                seen.push(item.project_id.clone());
            }
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert_eq!(
            seen, project_ids,
            "pages must be ordered by project_id and exhaustive"
        );
    }

    #[test]
    fn list_projects_rejects_out_of_range_limits() {
        let fixture = replica_fixture_with_assets(1);
        assert!(matches!(
            fixture.manager.list_projects(0, None),
            Err(ProjectReplicaError::InvalidBaseline(_))
        ));
        assert!(matches!(
            fixture.manager.list_projects(201, None),
            Err(ProjectReplicaError::InvalidBaseline(_))
        ));
    }

    #[test]
    fn listing_assets_for_an_unregistered_project_is_a_structured_error() {
        let fixture = replica_fixture_with_assets(1);
        let error = fixture
            .manager
            .list_project_assets("project_missing", 10, None, None)
            .unwrap_err();
        assert!(matches!(error, ProjectReplicaError::NotFound { .. }));
    }

    #[test]
    fn list_project_assets_rejects_out_of_range_limits() {
        let fixture = replica_fixture_with_assets(1);
        assert!(matches!(
            fixture
                .manager
                .list_project_assets(&fixture.project_id, 0, None, None),
            Err(ProjectReplicaError::InvalidBaseline(_))
        ));
        assert!(matches!(
            fixture
                .manager
                .list_project_assets(&fixture.project_id, 201, None, None),
            Err(ProjectReplicaError::InvalidBaseline(_))
        ));
    }

    /// Documents the known limitation of the chosen search strategy: it is a
    /// case-folded substring match over the raw `labels_json` text, not a
    /// parsed-label match. A label of "covered" contains the literal
    /// substring "red", so searching "red" surfaces it even though no label
    /// equals "red". This is accepted for a bounded, per-project, human-typed
    /// search (see the task report for the justification) - the test exists
    /// to keep the assertion honest about actual behavior rather than
    /// aspirational behavior.
    #[test]
    fn project_asset_search_over_labels_matches_json_substrings_including_false_positives() {
        let fixture = Fixture::new();
        let source_root = TempDir::new().unwrap();
        let catalog = Catalog::open(source_root.path().join("desktop.sqlite3")).unwrap();
        catalog.set_local_node_id("node_mac").unwrap();
        let project = catalog.create_project("Trip").unwrap();
        catalog
            .add_portable_project_asset(&project.id, portable_asset_payload(0))
            .unwrap();
        catalog.add_label("asset_000", "covered").unwrap();
        let envelope = BaselineEnvelope::export(&catalog, &project.id, vec![], vec![]).unwrap();
        let registered = fixture.manager.import_baseline(&envelope).unwrap();

        let page = fixture
            .manager
            .list_project_assets(&registered.project_id, 10, None, Some("red"))
            .unwrap();

        assert_eq!(
            page.items.len(),
            1,
            "labels_json substring match should hit 'covered'"
        );
        assert_eq!(page.items[0].labels, vec!["covered".to_string()]);
        assert!(
            !page.items[0].labels.iter().any(|label| label == "red"),
            "no label actually equals the search term - this is the accepted false positive"
        );
    }
}
