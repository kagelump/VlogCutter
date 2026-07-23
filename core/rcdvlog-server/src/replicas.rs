use crate::{
    ControlCatalog, ProjectMediaMembershipProjection, StorageLibraryError,
    open_read_only_connection, open_writable_connection,
};
use chrono::{SecondsFormat, Utc};
use rcdvlog_core::{
    Asset, Catalog, MediaObjectId, MediaVerificationState, OperationId, PROJECT_ASSET_MEDIA_LINKED,
    ProjectAssetAddedPayload, ProjectAssetId, ProjectId,
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
}
