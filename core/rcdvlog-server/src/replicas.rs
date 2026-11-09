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
    /// Absolute path of a same-machine CLI catalog served directly instead of a
    /// replica. Node-local: never serialized into API or sync payloads.
    #[serde(skip)]
    pub attached_catalog_path: Option<PathBuf>,
}

impl ProjectReplica {
    pub fn is_attached(&self) -> bool {
        self.attached_catalog_path.is_some()
    }
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
    #[error("project {project_id} is attached to a local catalog and does not sync")]
    Attached { project_id: String },
    #[error("cannot attach catalog: {0}")]
    AttachRefused(String),
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
            Self::Attached { .. } => "project_attached",
            Self::AttachRefused(_) => "attached_catalog_refused",
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSyncOverview {
    pub project_id: String,
    pub registry_state: String,
    pub origin_node_id: Option<String>,
    pub server_node_id: String,
    pub asset_count: u64,
    pub operation_count: u64,
    pub latest_local_sequence: u64,
    pub pending_projection_repairs: u64,
    pub unresolved_conflict_count: usize,
    pub node_observation_count: u64,
    pub last_success_at: String,
    pub last_error_code: Option<String>,
    pub endpoint: Option<String>,
    pub compatibility: String,
    pub connectivity: String,
    pub recommendations: Vec<String>,
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
    /// Supported proxy in an explicitly allowed same-machine cache.
    pub local_proxy_available: bool,
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
    attached_project_roots: Vec<PathBuf>,
    protected_media_roots: Vec<PathBuf>,
}

impl ProjectReplicaManager {
    pub(crate) fn new(
        data_dir: PathBuf,
        control: ControlCatalog,
        attached_project_roots: Vec<PathBuf>,
        protected_media_roots: Vec<PathBuf>,
    ) -> Self {
        Self {
            data_dir,
            control,
            attached_project_roots,
            protected_media_roots,
        }
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
            if existing.is_attached() {
                return Err(ProjectReplicaError::Attached {
                    project_id: project_id.clone(),
                });
            }
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
        if let Some(path) = &replica.attached_catalog_path {
            // A CLI catalog is only ever opened at exactly the schema this build
            // supports, so the server never migrates the CLI's working data.
            let path = path
                .canonicalize()
                .map_err(|_| ProjectReplicaError::AttachRefused("catalog file not found".into()))?;
            if !inside_any_root(&path, &self.attached_project_roots)
                || path.starts_with(self.data_dir.canonicalize()?)
            {
                return Err(ProjectReplicaError::AttachRefused(
                    "catalog is no longer inside an allowed project root".into(),
                ));
            }
            require_current_project_schema(&path)?;
            return Catalog::open_shared_existing(path)
                .map_err(ProjectReplicaError::Infrastructure);
        }
        let path = self.resolve_relative_path(&replica.replica_relative_path)?;
        Catalog::open_existing(path).map_err(ProjectReplicaError::Infrastructure)
    }

    /// Serves `project_id` from a same-machine CLI catalog instead of a replica.
    ///
    /// The catalog must resolve inside one of `allowed_roots`, outside the
    /// server's own data directory, and be at exactly the current project
    /// schema. An existing replica of the same project is re-pointed and its
    /// file kept, so review targets and sessions keyed by the project ID stay
    /// attached and [`Self::detach_catalog`] can return to it.
    pub fn attach_catalog(
        &self,
        catalog_path: &Path,
        allowed_roots: &[PathBuf],
        project_id: Option<&str>,
    ) -> Result<ProjectReplica, ProjectReplicaError> {
        let refuse = |reason: &str| ProjectReplicaError::AttachRefused(reason.to_string());
        let path = catalog_path
            .canonicalize()
            .map_err(|_| refuse("catalog file not found"))?;
        if !path.is_file() {
            return Err(refuse("catalog path is not a file"));
        }
        if !inside_any_root(&path, allowed_roots)
            || !inside_any_root(&path, &self.attached_project_roots)
        {
            return Err(refuse(
                "catalog is not inside RCDVLOG_ATTACHED_PROJECT_ROOTS",
            ));
        }
        if let Ok(data_dir) = self.data_dir.canonicalize()
            && path.starts_with(&data_dir)
        {
            return Err(refuse("catalog is inside the server data directory"));
        }
        crate::foundation::ensure_local_database_filesystem(&path)
            .map_err(|_| refuse("catalog must reside on a local filesystem"))?;
        require_current_project_schema(&path)?;
        let catalog =
            Catalog::open_read_only_existing(&path).map_err(ProjectReplicaError::Infrastructure)?;
        let projects = catalog
            .list_projects()
            .map_err(ProjectReplicaError::Infrastructure)?;
        let project = match project_id {
            Some(id) => projects
                .into_iter()
                .find(|project| project.id == id)
                .ok_or_else(|| refuse("catalog does not contain the requested project"))?,
            None if projects.len() == 1 => projects.into_iter().next().expect("one project"),
            None => {
                return Err(refuse(
                    "catalog contains several projects; pass --project-id",
                ));
            }
        };
        let origin_node_id = catalog
            .local_node_id()
            .map_err(ProjectReplicaError::Infrastructure)?;
        let path_text = path
            .to_str()
            .ok_or_else(|| refuse("catalog path is not valid UTF-8"))?
            .to_string();

        let now = now();
        let connection = open_writable_connection(self.control.database_path())
            .map_err(ProjectReplicaError::Infrastructure)?;
        let updated = connection.execute(
            "UPDATE project_registry SET attached_catalog_path=?2, state='registered', updated_at=?3 WHERE project_id=?1",
            params![project.id, path_text, now],
        )?;
        if updated == 0 {
            connection.execute(
                "INSERT INTO project_registry(project_id, replica_relative_path, state, registered_at, created_at, updated_at, origin_node_id, server_node_id, attached_catalog_path) VALUES (?1, ?2, 'registered', ?3, ?3, ?3, ?4, ?5, ?6)",
                params![
                    project.id,
                    attached_placeholder_path(&project.id),
                    now,
                    origin_node_id,
                    self.control.node_id().as_str(),
                    path_text,
                ],
            )?;
        }
        self.get(&project.id)
    }

    /// Returns an attached project to its replica. Refused when the project
    /// was attached without one, since that would leave it with no data.
    pub fn detach_catalog(&self, project_id: &str) -> Result<ProjectReplica, ProjectReplicaError> {
        let replica = self.get(project_id)?;
        if !replica.is_attached() {
            return Err(ProjectReplicaError::AttachRefused(
                "project is not attached".to_string(),
            ));
        }
        let replica_path = self.resolve_relative_path(&replica.replica_relative_path)?;
        if !replica_path.is_file() {
            return Err(ProjectReplicaError::AttachRefused(
                "project has no replica to return to; attach another catalog instead".to_string(),
            ));
        }
        let connection = open_writable_connection(self.control.database_path())
            .map_err(ProjectReplicaError::Infrastructure)?;
        connection.execute(
            "UPDATE project_registry SET attached_catalog_path=NULL, updated_at=?2 WHERE project_id=?1",
            params![project_id, now()],
        )?;
        self.get(project_id)
    }

    /// The thumbnail file the project's catalog records for one of its assets.
    /// The caller decides whether this node may serve that path.
    pub fn asset_thumbnail_path(
        &self,
        project_id: &str,
        asset_id: &str,
    ) -> Result<Option<PathBuf>, ProjectReplicaError> {
        let catalog = self.open_catalog(project_id)?;
        let Ok(asset) = catalog.get_asset(asset_id) else {
            return Ok(None);
        };
        if asset.project_id != project_id {
            return Ok(None);
        }
        Ok(asset.thumbnail_path.map(PathBuf::from))
    }

    /// A supported generated proxy from an attached CLI catalog. Never a source
    /// file or a path supplied by a browser or synchronized replica.
    pub fn asset_proxy_path(
        &self,
        project_id: &str,
        asset_id: &str,
    ) -> Result<Option<PathBuf>, ProjectReplicaError> {
        if !self.get(project_id)?.is_attached() {
            return Ok(None);
        }
        let catalog = self.open_catalog(project_id)?;
        let Ok(asset) = catalog.get_asset(asset_id) else {
            return Ok(None);
        };
        if asset.project_id != project_id {
            return Ok(None);
        }
        Ok(self.servable_local_proxy(&asset))
    }

    fn servable_local_proxy(&self, asset: &Asset) -> Option<PathBuf> {
        // Raw v3 stores encoded frames without display rotation. Legacy baked
        // proxies are equivalent only when no source rotation was baked in.
        let supported = asset.proxy_representation
            == Some(rcdvlog_core::ProxyRepresentation::RawV3)
            || (asset.proxy_representation
                == Some(rcdvlog_core::ProxyRepresentation::MetadataBakedV1)
                && asset.metadata_rotation_degrees == 0);
        if !asset.proxy_available || !supported {
            return None;
        }
        let path = Path::new(asset.proxy_path.as_deref()?)
            .canonicalize()
            .ok()?;
        if !inside_any_root(&path, &self.attached_project_roots) {
            return None;
        }
        if self
            .protected_media_roots
            .iter()
            .filter_map(|root| root.canonicalize().ok())
            .any(|root| path.starts_with(root))
        {
            return None;
        }
        if Path::new(&asset.path)
            .canonicalize()
            .is_ok_and(|original| original == path)
        {
            return None;
        }
        // Check the resolved target too: a .v3.mp4 symlink may point at a different format.
        if path.extension().and_then(|ext| ext.to_str()) != Some("mp4")
            || Some(rcdvlog_core::proxy_representation(&path)) != asset.proxy_representation
        {
            return None;
        }
        let metadata = fs::metadata(&path).ok()?;
        (metadata.is_file() && metadata.len() > 0).then_some(path)
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

    pub fn list_sync_overview(&self) -> Result<Vec<ProjectSyncOverview>, ProjectReplicaError> {
        let projects = self.list_projects(200, None)?.items;
        let connection = open_read_only_connection(self.control.database_path())
            .map_err(ProjectReplicaError::Infrastructure)?;
        let mut result = Vec::with_capacity(projects.len());
        for project in projects {
            let replica = self.get(&project.project_id)?;
            let diagnostics = self
                .open_catalog(&project.project_id)?
                .operation_diagnostics(&project.project_id)
                .map_err(ProjectReplicaError::Infrastructure)?;
            let pending_projection_repairs: i64 = connection.query_row(
                "SELECT COUNT(*) FROM project_projection_repairs WHERE project_id=?1 AND state='pending'",
                [&project.project_id],
                |row| row.get(0),
            )?;
            let node_observation_count: i64 = connection.query_row(
                r#"
                SELECT COUNT(DISTINCT o.node_id || ':' || o.storage_object_id)
                FROM project_media_memberships pm
                JOIN media_object_locations ml ON ml.media_object_id = pm.media_object_id
                JOIN node_asset_observations o ON o.storage_object_id = ml.storage_object_id
                WHERE pm.project_id = ?1
                "#,
                [&project.project_id],
                |row| row.get(0),
            )?;
            let mut recommendations = Vec::new();
            if !diagnostics.unresolved_conflicts.is_empty() {
                recommendations.push("review_conflicts_on_desktop".to_string());
            }
            if pending_projection_repairs > 0 {
                recommendations.push("wait_for_projection_repair".to_string());
            }
            recommendations.push("reconcile_paired_mac_to_refresh_connectivity".to_string());
            result.push(ProjectSyncOverview {
                project_id: project.project_id,
                registry_state: project.state,
                origin_node_id: replica.origin_node_id,
                server_node_id: replica.server_node_id,
                asset_count: project.asset_count,
                operation_count: diagnostics.operation_count,
                latest_local_sequence: diagnostics.latest_local_sequence,
                pending_projection_repairs: u64::try_from(pending_projection_repairs).map_err(
                    |_| ProjectReplicaError::InvalidBaseline("negative repair count".to_string()),
                )?,
                unresolved_conflict_count: diagnostics.unresolved_conflicts.len(),
                node_observation_count: u64::try_from(node_observation_count).map_err(|_| {
                    ProjectReplicaError::InvalidBaseline("negative observation count".to_string())
                })?,
                last_success_at: project.updated_at,
                last_error_code: None,
                endpoint: None,
                compatibility: "compatible".to_string(),
                connectivity: "not_probed".to_string(),
                recommendations,
            });
        }
        Ok(result)
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
        let (proxy, thumbnail, mut original_availability) =
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
        let attached = self.get(project_id)?.is_attached();
        let local_proxy_available = attached && self.servable_local_proxy(&asset).is_some();
        if attached {
            original_availability = Some(if asset.original_available {
                NodeAvailabilityState::AccessibleHere
            } else {
                NodeAvailabilityState::TemporarilyUnavailable
            });
        }
        Ok(ProjectReviewAsset {
            local_proxy_available,
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
                description: (!asset.description.trim().is_empty()).then_some(asset.description),
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
            timestamp_candidates: probe
                .as_ref()
                .map(|value| value.timestamp_candidates.clone()),
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
        if self.get(project_id)?.is_attached() {
            return Err(ProjectReplicaError::Attached {
                project_id: project_id.to_string(),
            });
        }
        let catalog = self.open_catalog(project_id)?;
        let mut response = reconcile(
            &catalog,
            project_id,
            self.control.node_id().as_str(),
            request,
        )
        .map_err(map_sync_error)?;
        let replica = self.get(project_id)?;
        if let Some(peer_node_id) = replica.origin_node_id.as_deref() {
            let reported_server_sequence = request
                .operation_cursors
                .get(self.control.node_id().as_str())
                .copied()
                .unwrap_or(0);
            let connection = open_writable_connection(self.control.database_path())
                .map_err(ProjectReplicaError::Infrastructure)?;
            connection.execute(
                "INSERT INTO project_peer_sync_state(project_id,peer_node_id,acknowledged_server_sequence,last_reconciled_at) VALUES(?1,?2,?3,?4) ON CONFLICT(project_id,peer_node_id) DO UPDATE SET acknowledged_server_sequence=MAX(acknowledged_server_sequence,excluded.acknowledged_server_sequence),last_reconciled_at=excluded.last_reconciled_at",
                params![project_id, peer_node_id, i64::try_from(reported_server_sequence).map_err(|error| ProjectReplicaError::InvalidBaseline(error.to_string()))?, now()],
            )?;
            if let Some(recovery) = catalog
                .reconcile_sequence_recovery(peer_node_id, reported_server_sequence)
                .map_err(ProjectReplicaError::Infrastructure)?
            {
                let completed = recovery.state == rcdvlog_core::SequenceRecoveryState::Completed;
                response.sequence_recovery.push(recovery);
                if !completed {
                    response.operations.clear();
                    response.has_more_operations = false;
                }
            }
        }
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
                "SELECT project_id, replica_relative_path, state, COALESCE(baseline_version, 0), COALESCE(baseline_digest, ''), origin_node_id, COALESCE(server_node_id, ''), registered_at, updated_at, attached_catalog_path FROM project_registry WHERE project_id=?1",
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
                        attached_catalog_path: row.get::<_, Option<String>>(9)?.map(PathBuf::from),
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

/// A unique, never-created registry path for a project attached without a replica.
fn attached_placeholder_path(project_id: &str) -> String {
    format!(
        "attached/{}.sqlite3",
        blake3::hash(project_id.as_bytes()).to_hex()
    )
}

/// Whether `path` (already canonical) lies inside one of `roots`. Roots that do
/// not exist on this node match nothing.
pub fn inside_any_root(path: &Path, roots: &[PathBuf]) -> bool {
    roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .any(|root| path.starts_with(root))
}

fn require_current_project_schema(path: &Path) -> Result<(), ProjectReplicaError> {
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let inspection = rcdvlog_core::inspect_project_catalog_migrations(&connection)
        .map_err(ProjectReplicaError::Infrastructure)?;
    if inspection.status != rcdvlog_core::MigrationHistoryStatus::Current {
        return Err(ProjectReplicaError::AttachRefused(format!(
            "catalog schema is {} but this server supports exactly {}",
            inspection.current_version, inspection.latest_supported_version
        )));
    }
    Ok(())
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
#[path = "replicas_tests.rs"]
mod tests;
