use crate::{
    ClaimedJob, FailureKind, ItemCompletion, JobClass, JobDefinition, JobEngine, JobEngineError,
    JobHandler, JobHandlerFailure, JobRequirements, JobResultCommit, PathAccessIntent,
    ProgressEstimateInput, RetryPolicy, ServerCapability, StorageLibrary,
    open_read_only_connection, open_writable_connection,
};
use chrono::{SecondsFormat, Utc};
use rcdvlog_core::{
    JobId, MediaEstimatorEnvironment, MediaEstimatorIdentity, MediaObjectId, NodeId,
    PROXY_REPRESENTATION_KIND, ProxyEncoderKind, ProxyEncoderPolicy, RepresentationId,
    StorageObjectId, probe_source_audio_present, proxy_rate_history_key,
    resolve_proxy_encoder_plan_with_tools, resolve_tool_program,
    run_proxy_transcode_with_plan_and_cancellation,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

pub const PROXY_RECIPE_KEY: &str = "proxy.h264-720p.orientation-v3";
pub const THUMBNAIL_RECIPE_KEY: &str = "thumbnail.q1.orientation-v2";
const PROXY_SETTINGS: &str = "shared-proxy-policy-v2:display_rotation=0,noautorotate,maxwidth=1280,h264,yuv420p,aac128k,faststart,fps_mode=passthrough,map_metadata=-1,validated-source-correspondence;actual-encoder-in-provenance";
const THUMBNAIL_SETTINGS: &str =
    "ffmpeg:seek=duration*0.2,noautorotate,frames=1,scale=min(480,iw):-2,jpeg-q3";
const REPRESENTATION_JOB_TYPE: &str = "representation.generate.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepresentationKind {
    Proxy,
    Thumbnail,
}

impl RepresentationKind {
    pub fn recipe_key(self) -> &'static str {
        match self {
            Self::Proxy => PROXY_RECIPE_KEY,
            Self::Thumbnail => THUMBNAIL_RECIPE_KEY,
        }
    }

    fn settings(self) -> &'static str {
        match self {
            Self::Proxy => PROXY_SETTINGS,
            Self::Thumbnail => THUMBNAIL_SETTINGS,
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Proxy => "mp4",
            Self::Thumbnail => "jpg",
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            Self::Proxy => "video/mp4",
            Self::Thumbnail => "image/jpeg",
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Proxy => "proxy",
            Self::Thumbnail => "thumbnail",
        }
    }

    fn parse(value: &str) -> Result<Self, RepresentationError> {
        match value {
            "proxy" => Ok(Self::Proxy),
            "thumbnail" => Ok(Self::Thumbnail),
            _ => Err(RepresentationError::InvalidInput(
                "unknown representation kind".into(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepresentationState {
    Pending,
    Available,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Representation {
    pub id: RepresentationId,
    pub media_object_id: MediaObjectId,
    pub source_content_hash: String,
    pub source_size_bytes: u64,
    pub kind: RepresentationKind,
    pub recipe_key: String,
    pub state: RepresentationState,
    pub output_content_hash: Option<String>,
    pub byte_length: Option<u64>,
    pub content_type: String,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub available_on_node: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepresentationRequest {
    pub media_object_id: MediaObjectId,
    pub kind: RepresentationKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepresentationRequestResult {
    pub representation: Representation,
    pub job_id: Option<JobId>,
    pub reused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepresentationNeed {
    pub kind: RepresentationKind,
    pub current_recipe_key: String,
    pub reason: String,
}

#[derive(Debug, Error)]
pub enum RepresentationError {
    #[error("invalid representation request: {0}")]
    InvalidInput(String),
    #[error("media object is not full-hash verified")]
    SourceNotVerified,
    #[error("verified original media is offline")]
    MediaOffline,
    #[error("derived media root is unavailable")]
    DerivedRootUnavailable,
    #[error("representation was not found")]
    NotFound,
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Job(#[from] JobEngineError),
    #[error(transparent)]
    Infrastructure(#[from] anyhow::Error),
}

impl RepresentationError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "invalid_input",
            Self::SourceNotVerified => "source_not_verified",
            Self::MediaOffline => "media_offline",
            Self::DerivedRootUnavailable => "derived_root_unavailable",
            Self::NotFound => "not_found",
            Self::Database(_) | Self::Job(_) | Self::Infrastructure(_) => "internal_error",
        }
    }
}

#[derive(Clone)]
pub struct RepresentationService {
    library: StorageLibrary,
    jobs: JobEngine,
    derived_root: PathBuf,
}

impl RepresentationService {
    pub fn new(library: StorageLibrary, jobs: JobEngine, derived_root: PathBuf) -> Self {
        Self {
            library,
            jobs,
            derived_root,
        }
    }

    pub fn request(
        &self,
        request: RepresentationRequest,
    ) -> Result<RepresentationRequestResult, RepresentationError> {
        let media = self
            .library
            .get_media_object(&request.media_object_id)
            .map_err(|_| RepresentationError::NotFound)?;
        let source_hash = media
            .full_content_hash
            .ok_or(RepresentationError::SourceNotVerified)?;
        let source_size = media
            .size_bytes
            .ok_or(RepresentationError::SourceNotVerified)?;
        if media.verification_state.as_str() != "verified" {
            return Err(RepresentationError::SourceNotVerified);
        }
        let settings_identity = blake3::hash(request.kind.settings().as_bytes())
            .to_hex()
            .to_string();
        let id = representation_id(&source_hash, source_size, request.kind, &settings_identity)?;
        let relative_path = representation_relative_path(
            &source_hash,
            request.kind.recipe_key(),
            &id,
            request.kind.extension(),
        );
        let now = timestamp();
        let connection = open_writable_connection(self.library.database_path())?;
        connection.execute(
            r#"
            INSERT INTO representations (
                representation_id, media_object_id, source_content_hash, source_size_bytes,
                kind, recipe_key, settings_identity, state, content_type, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?9, ?9)
            ON CONFLICT(representation_id) DO NOTHING
            "#,
            params![
                id.as_str(),
                request.media_object_id.as_str(),
                source_hash,
                source_size as i64,
                request.kind.as_str(),
                request.kind.recipe_key(),
                settings_identity,
                request.kind.content_type(),
                now,
            ],
        )?;
        let representation = self.get(&id)?;
        if representation.state == RepresentationState::Available
            && self.validated_local_path(&representation).is_ok()
        {
            return Ok(RepresentationRequestResult {
                representation,
                job_id: None,
                reused: true,
            });
        }

        let connection = open_read_only_connection(self.library.database_path())?;
        if let Some(active) = connection.query_row(
            "SELECT job_id FROM jobs WHERE representation_id = ?1 AND state IN ('queued','running','retry_wait') ORDER BY enqueue_sequence DESC LIMIT 1",
            [id.as_str()], |row| row.get::<_, String>(0),
        ).optional()? {
            let active = JobId::parse(active).map_err(|error| RepresentationError::InvalidInput(error.to_string()))?;
            return Ok(RepresentationRequestResult {
                representation: self.get(&id)?, job_id: Some(active), reused: false,
            });
        }
        let generation: i64 = connection.query_row(
            "SELECT COUNT(*) + 1 FROM jobs WHERE representation_id = ?1",
            [id.as_str()],
            |row| row.get(0),
        )?;
        drop(connection);
        let job_id = JobId::parse(format!(
            "job_repr_{}_{}",
            &id.as_str()["representation_".len()..],
            generation
        ))
        .map_err(|error| RepresentationError::InvalidInput(error.to_string()))?;
        let definition = representation_job(
            job_id.clone(),
            self.jobs.node_id().clone(),
            &representation,
            relative_path.to_string_lossy().into_owned(),
            generation as u64,
        )?;
        let enqueued = match self.jobs.enqueue_with_id(job_id.as_str(), definition) {
            Ok(job) => job,
            Err(JobEngineError::Database(_)) => {
                let connection = open_read_only_connection(self.library.database_path())?;
                let active: String = connection.query_row(
                    "SELECT job_id FROM jobs WHERE representation_id = ?1 AND state IN ('queued','running','retry_wait') ORDER BY enqueue_sequence DESC LIMIT 1",
                    [id.as_str()], |row| row.get(0),
                )?;
                let active = JobId::parse(active)
                    .map_err(|error| RepresentationError::InvalidInput(error.to_string()))?;
                return Ok(RepresentationRequestResult {
                    representation: self.get(&id)?,
                    job_id: Some(active),
                    reused: false,
                });
            }
            Err(error) => return Err(error.into()),
        };
        Ok(RepresentationRequestResult {
            representation: self.get(&id)?,
            job_id: Some(enqueued.job_id),
            reused: false,
        })
    }

    pub fn get(&self, id: &RepresentationId) -> Result<Representation, RepresentationError> {
        let connection = open_read_only_connection(self.library.database_path())?;
        connection
            .query_row(
                r#"
            SELECT r.representation_id, r.media_object_id, r.source_content_hash,
                   r.source_size_bytes, r.kind, r.recipe_key, r.state,
                   r.output_content_hash, r.byte_length, r.content_type,
                   r.created_at, r.completed_at,
                   EXISTS(
                       SELECT 1 FROM representation_locations l
                       WHERE l.representation_id = r.representation_id
                         AND l.node_id = ?2 AND l.state = 'available'
                   )
            FROM representations r WHERE r.representation_id = ?1
            "#,
                params![id.as_str(), self.library.local_node_id().as_str()],
                representation_from_row,
            )
            .optional()?
            .ok_or(RepresentationError::NotFound)
    }

    pub fn list_missing_or_obsolete(
        &self,
        media_id: &MediaObjectId,
    ) -> Result<Vec<Representation>, RepresentationError> {
        let connection = open_read_only_connection(self.library.database_path())?;
        let mut statement = connection.prepare(
            r#"
            SELECT r.representation_id, r.media_object_id, r.source_content_hash,
                   r.source_size_bytes, r.kind, r.recipe_key, r.state,
                   r.output_content_hash, r.byte_length, r.content_type,
                   r.created_at, r.completed_at,
                   EXISTS(
                       SELECT 1 FROM representation_locations l
                       WHERE l.representation_id = r.representation_id
                         AND l.node_id = ?2 AND l.state = 'available'
                   )
            FROM representations r
            WHERE r.media_object_id = ?1
              AND (r.state != 'available' OR r.recipe_key NOT IN (?3, ?4)
                   OR (r.kind='proxy' AND r.settings_identity<>?5)
                   OR (r.kind='thumbnail' AND r.settings_identity<>?6))
            ORDER BY r.created_at, r.representation_id
            "#,
        )?;
        let rows = statement.query_map(
            params![
                media_id.as_str(),
                self.library.local_node_id().as_str(),
                PROXY_RECIPE_KEY,
                THUMBNAIL_RECIPE_KEY,
                blake3::hash(PROXY_SETTINGS.as_bytes()).to_hex().to_string(),
                blake3::hash(THUMBNAIL_SETTINGS.as_bytes())
                    .to_hex()
                    .to_string()
            ],
            representation_from_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_needs(
        &self,
        media_id: &MediaObjectId,
    ) -> Result<Vec<RepresentationNeed>, RepresentationError> {
        self.library
            .get_media_object(media_id)
            .map_err(|_| RepresentationError::NotFound)?;
        let connection = open_read_only_connection(self.library.database_path())?;
        let mut needs = Vec::new();
        for kind in [RepresentationKind::Proxy, RepresentationKind::Thumbnail] {
            let current: Option<(String, i64)> = connection
                .query_row(
                    r#"
                    SELECT r.state, EXISTS(
                        SELECT 1 FROM representation_locations l
                        WHERE l.representation_id=r.representation_id AND l.node_id=?4
                          AND l.state='available'
                    )
                    FROM representations r
                    WHERE r.media_object_id=?1 AND r.kind=?2 AND r.recipe_key=?3
                      AND r.settings_identity=?5
                    LIMIT 1
                    "#,
                    params![
                        media_id.as_str(),
                        kind.as_str(),
                        kind.recipe_key(),
                        self.library.local_node_id().as_str(),
                        blake3::hash(kind.settings().as_bytes())
                            .to_hex()
                            .to_string()
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if current
                .as_ref()
                .is_some_and(|(state, located)| state == "available" && *located != 0)
            {
                continue;
            }
            let obsolete: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM representations WHERE media_object_id=?1 AND kind=?2 AND (recipe_key<>?3 OR settings_identity<>?4))",
                params![media_id.as_str(), kind.as_str(), kind.recipe_key(), blake3::hash(kind.settings().as_bytes()).to_hex().to_string()], |row| row.get::<_, i64>(0),
            )? != 0;
            needs.push(RepresentationNeed {
                kind,
                current_recipe_key: kind.recipe_key().into(),
                reason: if obsolete {
                    "obsolete".into()
                } else {
                    "missing".into()
                },
            });
        }
        Ok(needs)
    }

    pub fn content_path(&self, id: &RepresentationId) -> Result<PathBuf, RepresentationError> {
        let representation = self.get(id)?;
        self.validated_local_path(&representation)
    }

    fn validated_local_path(
        &self,
        representation: &Representation,
    ) -> Result<PathBuf, RepresentationError> {
        let connection = open_read_only_connection(self.library.database_path())?;
        let mut statement = connection.prepare(
            r#"
            SELECT l.location_kind, l.relative_path, l.local_path FROM representation_locations l
            JOIN representations r ON r.representation_id = l.representation_id
            WHERE l.representation_id = ?1 AND l.node_id = ?2
              AND l.state = 'available' AND r.state = 'available'
            ORDER BY CASE l.location_kind WHEN 'shared_derived' THEN 0 ELSE 1 END,
                     l.verified_at DESC
            "#,
        )?;
        let locations = statement.query_map(
            params![
                representation.id.as_str(),
                self.library.local_node_id().as_str()
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )?;
        for location in locations {
            let (kind, relative, local) = location?;
            let path = if kind == "shared_derived" {
                let Some(relative) = relative else { continue };
                safe_join(&self.derived_root, &relative)?
            } else {
                let Some(local) = local else { continue };
                PathBuf::from(local)
            };
            if !path.is_file() {
                continue;
            }
            let Ok(output) = inspect_output(&path) else {
                continue;
            };
            if Some(output.byte_length) == representation.byte_length
                && representation.output_content_hash.as_deref() == Some(output.hash.as_str())
            {
                return Ok(path);
            }
        }
        Err(RepresentationError::NotFound)
    }

    /// Registers a representation that was generated on a remote node (e.g. the Mac).
    /// The remote node has already validated the output and reports the content hash
    /// and byte length. The server records the location but cannot validate the file locally.
    pub fn register_remote_output(
        &self,
        id: &RepresentationId,
        remote_node_id: &NodeId,
        local_path: &str,
        output_content_hash: &str,
        byte_length: u64,
    ) -> Result<Representation, RepresentationError> {
        let _representation = self.get(id)?;
        let location_key = blake3::hash(local_path.as_bytes()).to_hex().to_string();
        let now = timestamp();
        let mut connection = open_writable_connection(self.library.database_path())?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = tx.execute(
            "UPDATE representations SET state='available', output_content_hash=?2, byte_length=?3, \
             completed_at=COALESCE(completed_at,?4), updated_at=?4 \
             WHERE representation_id=?1 AND (state='pending' OR (output_content_hash=?2 AND byte_length=?3))",
            params![id.as_str(), output_content_hash, byte_length as i64, now],
        )?;
        if updated != 1 {
            return Err(RepresentationError::InvalidInput(
                "remote output conflicts with representation manifest".into(),
            ));
        }
        tx.execute(
            r#"
            INSERT INTO representation_locations(
                representation_id, node_id, location_kind, location_key,
                relative_path, local_path, state, verified_at, updated_at
            ) VALUES(?1, ?2, 'project_local', ?3, NULL, ?4, 'available', ?5, ?5)
            ON CONFLICT(representation_id, node_id, location_kind, location_key)
            DO UPDATE SET state='available', verified_at=excluded.verified_at,
                          updated_at=excluded.updated_at
            "#,
            params![
                id.as_str(),
                remote_node_id.as_str(),
                location_key,
                local_path,
                now
            ],
        )?;
        tx.commit()?;
        self.get(id)
    }

    /// Registers an existing current-recipe project/server-local output after
    /// validating its media contract. The absolute path remains node-local and
    /// is never returned by representation DTOs or used in media URLs.
    pub fn register_existing_compatible_output(
        &self,
        id: &RepresentationId,
        local_path: &Path,
        ffprobe_program: &Path,
    ) -> Result<Representation, RepresentationError> {
        if !local_path.is_absolute() || !local_path.is_file() {
            return Err(RepresentationError::InvalidInput(
                "existing output must be an absolute file path".into(),
            ));
        }
        let representation = self.get(id)?;
        validate_output(local_path, representation.kind, ffprobe_program)
            .map_err(RepresentationError::Infrastructure)?;
        let output = inspect_output(local_path).map_err(RepresentationError::Infrastructure)?;
        let path_text = local_path.to_str().ok_or_else(|| {
            RepresentationError::InvalidInput("existing output path must be Unicode".into())
        })?;
        let location_key = blake3::hash(path_text.as_bytes()).to_hex().to_string();
        let now = timestamp();
        let mut connection = open_writable_connection(self.library.database_path())?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = tx.execute(
            "UPDATE representations SET state='available', output_content_hash=?2, byte_length=?3, completed_at=COALESCE(completed_at,?4), updated_at=?4 WHERE representation_id=?1 AND (state='pending' OR (output_content_hash=?2 AND byte_length=?3))",
            params![id.as_str(), output.hash, output.byte_length as i64, now],
        )?;
        if updated != 1 {
            return Err(RepresentationError::InvalidInput(
                "existing output conflicts with representation manifest".into(),
            ));
        }
        tx.execute(
            r#"
            INSERT INTO representation_locations(
                representation_id,node_id,location_kind,location_key,relative_path,local_path,state,verified_at,updated_at
            ) VALUES(?1,?2,'project_local',?3,NULL,?4,'available',?5,?5)
            ON CONFLICT(representation_id,node_id,location_kind,location_key)
            DO UPDATE SET state='available',verified_at=excluded.verified_at,updated_at=excluded.updated_at
            "#,
            params![id.as_str(), self.library.local_node_id().as_str(), location_key, path_text, now],
        )?;
        tx.commit()?;
        self.get(id)
    }

    /// List all available representations for a set of media objects on this node.
    /// Used by the desktop to discover server-side representations for download.
    pub fn list_available(
        &self,
        media_ids: &[MediaObjectId],
    ) -> Result<Vec<Representation>, RepresentationError> {
        if media_ids.is_empty() {
            return Ok(Vec::new());
        }
        let connection = open_read_only_connection(self.library.database_path())?;
        let placeholders: Vec<String> = media_ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect();
        let sql = format!(
            r#"
            SELECT r.representation_id, r.media_object_id, r.source_content_hash,
                   r.source_size_bytes, r.kind, r.recipe_key, r.state,
                   r.output_content_hash, r.byte_length, r.content_type,
                   r.created_at, r.completed_at,
                   EXISTS(
                       SELECT 1 FROM representation_locations l
                       WHERE l.representation_id = r.representation_id
                         AND l.node_id = ?{} AND l.state = 'available'
                   )
            FROM representations r
            WHERE r.media_object_id IN ({})
              AND r.state = 'available'
            ORDER BY r.kind, r.created_at
            "#,
            media_ids.len() + 1,
            placeholders.join(", "),
        );
        let mut statement = connection.prepare(&sql)?;
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        for id in media_ids {
            params.push(Box::new(id.as_str().to_string()));
        }
        params.push(Box::new(self.library.local_node_id().as_str().to_string()));
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let rows = statement.query_map(param_refs.as_slice(), representation_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepresentationJobPayload {
    representation_id: RepresentationId,
    media_object_id: MediaObjectId,
    source_content_hash: String,
    source_size_bytes: u64,
    kind: RepresentationKind,
    relative_path: String,
}

#[derive(Clone)]
pub struct RepresentationJobHandler {
    library: StorageLibrary,
    jobs: JobEngine,
    derived_root: PathBuf,
    ffmpeg_program: PathBuf,
    ffprobe_program: PathBuf,
    proxy_encoder_policy: ProxyEncoderPolicy,
}

impl RepresentationJobHandler {
    pub fn new(
        library: StorageLibrary,
        jobs: JobEngine,
        derived_root: PathBuf,
        ffmpeg_program: PathBuf,
        ffprobe_program: PathBuf,
    ) -> Self {
        Self {
            library,
            jobs,
            derived_root,
            // Resolve configured bare command names once. The same resolved
            // executable is used for execution and estimator identity.
            ffmpeg_program: resolve_tool_program(&ffmpeg_program),
            ffprobe_program: resolve_tool_program(&ffprobe_program),
            proxy_encoder_policy: ProxyEncoderPolicy::Auto,
        }
    }

    pub fn with_proxy_encoder_policy(mut self, policy: ProxyEncoderPolicy) -> Self {
        self.proxy_encoder_policy = policy;
        self
    }

    fn estimator_environment(&self) -> MediaEstimatorEnvironment {
        // The handler's resolved programs are the authoritative execution
        // tools. A bound engine with a different identity is rejected by the
        // recording call instead of silently attributing timing to the wrong
        // binary.
        MediaEstimatorEnvironment::from_tools(&self.ffmpeg_program, &self.ffprobe_program)
    }

    fn attach_estimator_context(
        &self,
        job: &ClaimedJob,
        identity: &MediaEstimatorIdentity,
    ) -> Result<(), JobHandlerFailure> {
        self.jobs
            .clone()
            .with_media_estimator_environment(identity.environment.clone())
            .attach_attempt_estimator_identity(job, identity)
            .map_err(|_| retryable("job_progress_failed"))
    }

    fn execute(
        &self,
        job: &ClaimedJob,
        payload: &RepresentationJobPayload,
        cancellation: &CancellationToken,
    ) -> Result<GeneratedOutput, JobHandlerFailure> {
        let final_path =
            safe_join(&self.derived_root, &payload.relative_path).map_err(handler_error)?;
        let parent = final_path
            .parent()
            .ok_or_else(|| terminal("invalid_output_path"))?;
        fs::create_dir_all(parent).map_err(|_| retryable("derived_root_unavailable"))?;
        if !self.derived_root.is_dir() {
            return Err(retryable("derived_root_unavailable"));
        }
        if final_path.is_file() {
            if validate_output(&final_path, payload.kind, &self.ffprobe_program).is_ok() {
                return inspect_output(&final_path).map_err(handler_error);
            }
            return Err(terminal("existing_output_invalid"));
        }

        let source = verified_original_path(&self.library, &payload.media_object_id)?;
        verify_source_identity(
            &source,
            &payload.source_content_hash,
            payload.source_size_bytes,
            cancellation,
        )?;
        let temporary = parent.join(format!(
            ".{}.attempt-{}.tmp.{}",
            payload.representation_id.as_str(),
            job.attempt_number,
            payload.kind.extension()
        ));
        let _cleanup = TempArtifact(temporary.clone());
        let started = Instant::now();
        let mut proxy_success_elapsed = None;
        let (proxy_provenance, estimator_identity) = match payload.kind {
            RepresentationKind::Proxy => {
                let duration = media_duration(&source, &self.ffprobe_program)?;
                let source_audio_present =
                    probe_source_audio_present(&self.ffprobe_program, &source)
                        .map_err(|_| terminal("invalid_probe_output"))?;
                let plan = resolve_proxy_encoder_plan_with_tools(
                    self.proxy_encoder_policy,
                    &self.ffmpeg_program,
                    &self.ffprobe_program,
                );
                // Only a single-target plan has a known execution context
                // throughout the run. Hardware plans may fall back, so retain
                // uncertainty until their actual successful encoder is known.
                if let [target] = plan.targets.as_slice() {
                    let identity = MediaEstimatorIdentity::new(
                        self.estimator_environment(),
                        PROXY_REPRESENTATION_KIND,
                        payload.kind.recipe_key(),
                        proxy_rate_history_key(target.encoder, 1),
                        Some(target.encoder.as_str()),
                        1,
                        0,
                        0,
                    );
                    self.attach_estimator_context(job, &identity)?;
                }
                let outcome = run_proxy_transcode_with_plan_and_cancellation(
                    &self.ffmpeg_program,
                    &self.ffprobe_program,
                    &source,
                    &temporary,
                    duration,
                    source_audio_present,
                    &plan,
                    |_| {},
                    || cancellation.is_cancelled(),
                )
                .map_err(|failure| {
                    if cancellation.is_cancelled() {
                        return retryable("canceled");
                    }
                    tracing::warn!(
                        target: "rcdvlog_server::representations",
                        error = %failure.message,
                        "proxy generation failed after encoder fallback"
                    );
                    let provenance = server_safe_proxy_provenance(&failure.outcome)
                        .unwrap_or_else(|_| json!({"status": "failed"}));
                    // Preserve which encoder/context actually failed, but do
                    // not teach success rate history from failed overhead.
                    let attempted_encoder = failure
                        .outcome
                        .attempts
                        .last()
                        .and_then(|attempt| ProxyEncoderKind::parse(&attempt.encoder).ok())
                        .unwrap_or(ProxyEncoderKind::Software);
                    let execution_recipe_key = proxy_rate_history_key(attempted_encoder, 1);
                    let identity = MediaEstimatorIdentity::new(
                        self.estimator_environment(),
                        PROXY_REPRESENTATION_KIND,
                        payload.kind.recipe_key(),
                        execution_recipe_key,
                        Some(attempted_encoder.as_str()),
                        1,
                        0,
                        0,
                    );
                    let _ = self
                        .jobs
                        .clone()
                        .with_media_estimator_environment(identity.environment.clone())
                        .attach_attempt_estimator_identity(job, &identity);
                    JobHandlerFailure {
                        kind: FailureKind::Retryable,
                        code: "media_tool_failed".into(),
                        details: json!({
                            "code": "media_tool_failed",
                            "proxyProvenance": provenance,
                        }),
                    }
                })?;
                let actual_encoder = ProxyEncoderKind::parse(&outcome.selected_encoder)
                    .unwrap_or(ProxyEncoderKind::Software);
                proxy_success_elapsed = Some(outcome.successful_elapsed());
                let execution_recipe_key = proxy_rate_history_key(actual_encoder, 1);
                let identity = MediaEstimatorIdentity::new(
                    self.estimator_environment(),
                    PROXY_REPRESENTATION_KIND,
                    payload.kind.recipe_key(),
                    execution_recipe_key.clone(),
                    Some(actual_encoder.as_str()),
                    1,
                    0,
                    0,
                );
                let rate_key = identity.qualified_history_key(&execution_recipe_key);
                let mut provenance =
                    server_safe_proxy_provenance(&outcome).map_err(handler_error)?;
                provenance["rateHistoryKey"] = Value::String(rate_key);
                (Some(provenance), Some(identity))
            }
            RepresentationKind::Thumbnail => {
                let duration = media_duration(&source, &self.ffprobe_program)?;
                let seek = (duration.max(0.0) * 0.2).to_string();
                let execution_recipe_key = payload.kind.recipe_key().to_string();
                let identity = MediaEstimatorIdentity::new(
                    self.estimator_environment(),
                    "generated_thumbnail",
                    payload.kind.recipe_key(),
                    execution_recipe_key,
                    Some("ffmpeg-image"),
                    1,
                    0,
                    0,
                );
                self.attach_estimator_context(job, &identity)?;
                let mut command = Command::new(&self.ffmpeg_program);
                command
                    .args(["-y", "-hide_banner", "-loglevel", "error"])
                    .args(["-ss", &seek, "-noautorotate", "-i"])
                    .arg(&source)
                    .args([
                        "-frames:v",
                        "1",
                        "-vf",
                        "scale='min(480,iw)':-2:force_original_aspect_ratio=decrease",
                        "-q:v",
                        "3",
                    ])
                    .arg(&temporary)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                let mut child = command
                    .spawn()
                    .map_err(|_| retryable("media_tool_failed"))?;
                let status = wait_for_child(&mut child, cancellation)?;
                if !status.success() {
                    return Err(retryable("media_tool_failed"));
                }
                (None, Some(identity))
            }
        };
        validate_output(&temporary, payload.kind, &self.ffprobe_program)
            .map_err(|_| terminal("invalid_media_output"))?;
        if cancellation.is_cancelled() {
            return Err(retryable("canceled"));
        }
        match fs::rename(&temporary, &final_path) {
            Ok(()) => {}
            Err(_)
                if final_path.is_file()
                    && validate_output(&final_path, payload.kind, &self.ffprobe_program)
                        .is_ok() => {}
            Err(_) => return Err(retryable("derived_root_unavailable")),
        }
        let mut generated = inspect_output(&final_path).map_err(handler_error)?;
        generated.proxy_provenance = proxy_provenance;
        let elapsed = proxy_success_elapsed
            .unwrap_or_else(|| started.elapsed())
            .as_millis()
            .max(1)
            .min(u128::from(u64::MAX)) as u64;
        let jobs = estimator_identity.as_ref().map_or_else(
            || self.jobs.clone(),
            |identity| {
                self.jobs
                    .clone()
                    .with_media_estimator_environment(identity.environment.clone())
            },
        );
        jobs.record_item_completion_with_estimator_identity(
            job,
            ItemCompletion {
                work_units: job.job.total_work_units,
                succeeded: true,
                worker_elapsed_millis: elapsed,
            },
            ProgressEstimateInput {
                worker_count: 1,
                remaining_task_work_units: vec![],
            },
            estimator_identity.as_ref(),
        )
        .map_err(|_| retryable("job_progress_failed"))?;
        Ok(generated)
    }
}

impl JobHandler for RepresentationJobHandler {
    fn run(
        &self,
        job: ClaimedJob,
        cancellation: CancellationToken,
    ) -> Result<JobResultCommit, JobHandlerFailure> {
        if job.job.job_type != REPRESENTATION_JOB_TYPE || job.job.payload_version != 1 {
            return Err(terminal("unsupported_job_type"));
        }
        let payload: RepresentationJobPayload = serde_json::from_value(job.job.payload.clone())
            .map_err(|_| terminal("invalid_job_payload"))?;
        if job.job.representation_id.as_ref() != Some(&payload.representation_id)
            || job.job.media_object_id.as_ref() != Some(&payload.media_object_id)
            || job.job.recipe_key != payload.kind.recipe_key()
            || job
                .job
                .immutable_identity
                .get("sourceContentHash")
                .and_then(Value::as_str)
                != Some(payload.source_content_hash.as_str())
            || job
                .job
                .immutable_identity
                .get("sourceSizeBytes")
                .and_then(Value::as_u64)
                != Some(payload.source_size_bytes)
        {
            return Err(terminal("job_identity_mismatch"));
        }
        // A queued job retains its immutable settings identity across upgrades.
        // Never execute today's encoder policy under an older recipe's ID.
        let settings_identity = blake3::hash(payload.kind.settings().as_bytes())
            .to_hex()
            .to_string();
        let expected_id = representation_id(
            &payload.source_content_hash,
            payload.source_size_bytes,
            payload.kind,
            &settings_identity,
        )
        .map_err(|_| terminal("job_identity_mismatch"))?;
        if payload.representation_id != expected_id {
            return Err(terminal("obsolete_representation_settings"));
        }
        let generated = self.execute(&job, &payload, &cancellation)?;
        let mut details = json!({
            "representationId": payload.representation_id,
            "relativePath": payload.relative_path,
            "outputContentHash": generated.hash,
            "byteLength": generated.byte_length,
            "contentType": payload.kind.content_type(),
        });
        if let Some(provenance) = generated.proxy_provenance {
            details["proxyProvenance"] = provenance;
        }
        Ok(JobResultCommit {
            output_identity: Some(payload.representation_id.to_string()),
            details,
        })
    }

    fn commit_result(
        &self,
        tx: &Transaction<'_>,
        job: &ClaimedJob,
        result: &JobResultCommit,
    ) -> Result<(), JobEngineError> {
        let representation_id = result
            .details
            .get("representationId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                JobEngineError::InvalidInput("missing representation result ID".into())
            })?;
        let relative_path = result
            .details
            .get("relativePath")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                JobEngineError::InvalidInput("missing representation relative path".into())
            })?;
        let output_hash = result
            .details
            .get("outputContentHash")
            .and_then(Value::as_str)
            .ok_or_else(|| JobEngineError::InvalidInput("missing output hash".into()))?;
        let byte_length = result
            .details
            .get("byteLength")
            .and_then(Value::as_u64)
            .ok_or_else(|| JobEngineError::InvalidInput("missing byte length".into()))?;
        if job
            .job
            .representation_id
            .as_ref()
            .map(RepresentationId::as_str)
            != Some(representation_id)
            || safe_join(&self.derived_root, relative_path).is_err()
        {
            return Err(JobEngineError::InvalidInput(
                "representation result identity mismatch".into(),
            ));
        }
        let now = timestamp();
        let updated = tx.execute(
            r#"
            UPDATE representations SET state = 'available', output_content_hash = ?2,
                byte_length = ?3, completed_at = COALESCE(completed_at, ?4), updated_at = ?4
            WHERE representation_id = ?1
              AND (state = 'pending' OR (output_content_hash = ?2 AND byte_length = ?3))
            "#,
            params![representation_id, output_hash, byte_length as i64, now],
        )?;
        if updated != 1 {
            return Err(JobEngineError::InvalidInput(
                "representation result conflicts with manifest".into(),
            ));
        }
        tx.execute(
            r#"
            INSERT INTO representation_locations (
                representation_id, node_id, location_kind, location_key, relative_path, local_path,
                state, verified_at, updated_at
            ) VALUES (?1, ?2, 'shared_derived', ?3, ?3, NULL, 'available', ?4, ?4)
            ON CONFLICT(representation_id, node_id, location_kind, location_key)
            DO UPDATE SET state = 'available', verified_at = excluded.verified_at,
                          updated_at = excluded.updated_at
            "#,
            params![
                representation_id,
                self.library.local_node_id().as_str(),
                relative_path,
                now
            ],
        )?;
        Ok(())
    }
}

pub struct ServerJobHandler {
    scan: crate::ScanJobHandler,
    representations: RepresentationJobHandler,
    backups: crate::BackupJobHandler,
}

impl ServerJobHandler {
    pub fn new(
        scan: crate::ScanJobHandler,
        representations: RepresentationJobHandler,
        backups: crate::BackupJobHandler,
    ) -> Self {
        Self {
            scan,
            representations,
            backups,
        }
    }
}

impl JobHandler for ServerJobHandler {
    fn run(
        &self,
        job: ClaimedJob,
        cancellation: CancellationToken,
    ) -> Result<JobResultCommit, JobHandlerFailure> {
        if job.job.job_type == crate::BACKUP_JOB_TYPE {
            self.backups.run(job, cancellation)
        } else if job.job.job_type == REPRESENTATION_JOB_TYPE {
            self.representations.run(job, cancellation)
        } else {
            self.scan.run(job, cancellation)
        }
    }

    fn commit_result(
        &self,
        tx: &Transaction<'_>,
        job: &ClaimedJob,
        result: &JobResultCommit,
    ) -> Result<(), JobEngineError> {
        if job.job.job_type == crate::BACKUP_JOB_TYPE {
            self.backups.commit_result(tx, job, result)
        } else if job.job.job_type == REPRESENTATION_JOB_TYPE {
            self.representations.commit_result(tx, job, result)
        } else {
            self.scan.commit_result(tx, job, result)
        }
    }
}

fn server_safe_proxy_provenance(
    outcome: &rcdvlog_core::ProxyGenerationOutcome,
) -> anyhow::Result<Value> {
    // Job result details are durable SQLite facts and must not become a path
    // disclosure surface. Preserve the actual render-node identity but reduce
    // absolute device paths to their final component.
    let mut value = serde_json::to_value(outcome)?;
    let device = value
        .get("device")
        .and_then(Value::as_str)
        .map(safe_device_component);
    if let Some(device) = device {
        value["device"] = Value::String(device);
    }
    if let Some(attempts) = value.get_mut("attempts").and_then(Value::as_array_mut) {
        for attempt in attempts {
            let device = attempt
                .get("device")
                .and_then(Value::as_str)
                .map(safe_device_component);
            if let Some(device) = device {
                attempt["device"] = Value::String(device);
            }
        }
    }
    Ok(value)
}

fn safe_device_component(device: &str) -> String {
    Path::new(device)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(device)
        .to_string()
}

fn representation_id(
    source_hash: &str,
    source_size: u64,
    kind: RepresentationKind,
    settings_identity: &str,
) -> Result<RepresentationId, RepresentationError> {
    let identity = format!(
        "v1\0{source_hash}\0{source_size}\0{}\0{}\0{settings_identity}",
        kind.as_str(),
        kind.recipe_key()
    );
    RepresentationId::parse(format!(
        "representation_{}",
        blake3::hash(identity.as_bytes()).to_hex()
    ))
    .map_err(|error| RepresentationError::InvalidInput(error.to_string()))
}

fn representation_relative_path(
    source_hash: &str,
    recipe: &str,
    id: &RepresentationId,
    extension: &str,
) -> PathBuf {
    let (recipe_id, recipe_version) = recipe.rsplit_once('.').unwrap_or((recipe, "v1"));
    PathBuf::from("representations")
        .join(source_hash)
        .join(recipe_id)
        .join(recipe_version)
        .join(format!("{}.{}", id.as_str(), extension))
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf, RepresentationError> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(RepresentationError::InvalidInput(
            "unsafe representation path".into(),
        ));
    }
    let joined = root.join(relative);
    if !joined.starts_with(root) {
        return Err(RepresentationError::InvalidInput(
            "representation path escapes derived root".into(),
        ));
    }
    Ok(joined)
}

fn representation_job(
    job_id: JobId,
    node_id: NodeId,
    representation: &Representation,
    relative_path: String,
    generation: u64,
) -> Result<JobDefinition, RepresentationError> {
    Ok(JobDefinition {
        job_id,
        job_type: REPRESENTATION_JOB_TYPE.into(),
        payload_version: 1,
        payload: json!({
            "representationId": representation.id,
            "mediaObjectId": representation.media_object_id,
            "sourceContentHash": representation.source_content_hash,
            "sourceSizeBytes": representation.source_size_bytes,
            "kind": representation.kind,
            "relativePath": relative_path,
        }),
        immutable_identity: json!({
            "sourceContentHash": representation.source_content_hash,
            "sourceSizeBytes": representation.source_size_bytes,
            "recipeKey": representation.recipe_key,
        }),
        requirements: JobRequirements {
            schema_version: 1,
            requires_original: true,
            requires_derived_write: true,
            capabilities: vec![
                ServerCapability::OriginalRead,
                ServerCapability::DerivedWrite,
                ServerCapability::Ffmpeg,
                ServerCapability::Ffprobe,
            ],
        },
        target_node_id: node_id,
        project_id: None,
        media_object_id: Some(representation.media_object_id.clone()),
        storage_root_id: None,
        representation_id: Some(representation.id.clone()),
        recipe_key: representation.recipe_key.clone(),
        job_class: JobClass::Proxy,
        idempotency_key: format!("{}:{generation}", representation.id),
        priority: 0,
        total_work_units: representation.source_size_bytes.max(1),
        total_items: 1,
        retry_policy: RetryPolicy::default(),
        dependencies: vec![],
    })
}

fn representation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Representation> {
    let id = RepresentationId::parse(row.get::<_, String>(0)?)
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let media_object_id = MediaObjectId::parse(row.get::<_, String>(1)?)
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let kind = RepresentationKind::parse(&row.get::<_, String>(4)?)
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let state = match row.get::<_, String>(6)?.as_str() {
        "pending" => RepresentationState::Pending,
        "available" => RepresentationState::Available,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let source_size: i64 = row.get(3)?;
    let byte_length: Option<i64> = row.get(8)?;
    Ok(Representation {
        id,
        media_object_id,
        source_content_hash: row.get(2)?,
        source_size_bytes: source_size as u64,
        kind,
        recipe_key: row.get(5)?,
        state,
        output_content_hash: row.get(7)?,
        byte_length: byte_length.map(|value| value as u64),
        content_type: row.get(9)?,
        created_at: row.get(10)?,
        completed_at: row.get(11)?,
        available_on_node: row.get::<_, i64>(12)? != 0,
    })
}

fn verified_original_path(
    library: &StorageLibrary,
    media_id: &MediaObjectId,
) -> Result<PathBuf, JobHandlerFailure> {
    let connection = open_read_only_connection(library.database_path())
        .map_err(|_| retryable("catalog_unavailable"))?;
    let mut statement = connection
        .prepare(
            r#"
        SELECT l.storage_object_id FROM media_object_locations l
        JOIN media_objects m ON m.media_object_id = l.media_object_id
        WHERE l.media_object_id = ?1 AND l.verification_state = 'verified'
          AND m.verification_state = 'verified'
        ORDER BY l.storage_object_id
        "#,
        )
        .map_err(|_| retryable("catalog_unavailable"))?;
    let ids = statement
        .query_map([media_id.as_str()], |row| row.get::<_, String>(0))
        .map_err(|_| retryable("catalog_unavailable"))?;
    for id in ids.flatten() {
        let Ok(id) = StorageObjectId::parse(id) else {
            continue;
        };
        if let Ok(path) = library.resolve_local_storage_object(&id, PathAccessIntent::Read)
            && path.is_file()
        {
            return Ok(path);
        }
    }
    Err(retryable("media_offline"))
}

fn media_duration(path: &Path, ffprobe: &Path) -> Result<f64, JobHandlerFailure> {
    let output = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .map_err(|_| retryable("media_tool_failed"))?;
    if !output.status.success() {
        return Err(retryable("media_tool_failed"));
    }
    let document: Value =
        serde_json::from_slice(&output.stdout).map_err(|_| terminal("invalid_probe_output"))?;
    document
        .pointer("/format/duration")
        .and_then(Value::as_str)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| terminal("invalid_probe_output"))
}

fn validate_output(path: &Path, kind: RepresentationKind, ffprobe: &Path) -> anyhow::Result<()> {
    let output = Command::new(ffprobe)
        .args(["-v", "error", "-show_streams", "-show_entries", "stream=codec_name,codec_type,width,height,pix_fmt:stream_tags=rotate:stream_side_data=rotation", "-of", "json"])
        .arg(path).output()?;
    anyhow::ensure!(output.status.success(), "ffprobe failed");
    let document: Value = serde_json::from_slice(&output.stdout)?;
    let streams = document
        .get("streams")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("missing streams"))?;
    let video = streams
        .iter()
        .find(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("video"))
        .ok_or_else(|| anyhow::anyhow!("missing video"))?;
    let width = video
        .get("width")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    match kind {
        RepresentationKind::Proxy => {
            anyhow::ensure!(
                video.get("codec_name").and_then(Value::as_str) == Some("h264"),
                "not h264"
            );
            anyhow::ensure!(
                video.get("pix_fmt").and_then(Value::as_str) == Some("yuv420p"),
                "not yuv420p"
            );
            anyhow::ensure!(width <= 1280, "proxy too wide");
            let tag_rotation = video
                .pointer("/tags/rotate")
                .and_then(Value::as_str)
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(0.0);
            let side_rotation = video
                .get("side_data_list")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items
                        .iter()
                        .find_map(|item| item.get("rotation").and_then(Value::as_f64))
                })
                .unwrap_or(0.0);
            anyhow::ensure!(
                tag_rotation.abs() < 0.01 && side_rotation.abs() < 0.01,
                "proxy rotation is not zero"
            );
            for audio in streams
                .iter()
                .filter(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("audio"))
            {
                anyhow::ensure!(
                    audio.get("codec_name").and_then(Value::as_str) == Some("aac"),
                    "audio is not aac"
                );
            }
        }
        RepresentationKind::Thumbnail => {
            anyhow::ensure!(
                matches!(
                    video.get("codec_name").and_then(Value::as_str),
                    Some("mjpeg" | "jpeg")
                ),
                "not jpeg"
            );
            anyhow::ensure!(width <= 480, "thumbnail too wide");
            let mut file = File::open(path)?;
            let mut signature = [0_u8; 2];
            file.read_exact(&mut signature)?;
            anyhow::ensure!(signature == [0xff, 0xd8], "invalid jpeg signature");
        }
    }
    Ok(())
}

struct GeneratedOutput {
    hash: String,
    byte_length: u64,
    proxy_provenance: Option<Value>,
}

fn inspect_output(path: &Path) -> anyhow::Result<GeneratedOutput> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 1024 * 1024];
    let mut length = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        length += read as u64;
    }
    anyhow::ensure!(length > 0, "empty representation");
    Ok(GeneratedOutput {
        hash: hasher.finalize().to_hex().to_string(),
        byte_length: length,
        proxy_provenance: None,
    })
}

fn verify_source_identity(
    path: &Path,
    expected_hash: &str,
    expected_size: u64,
    cancellation: &CancellationToken,
) -> Result<(), JobHandlerFailure> {
    let metadata = fs::metadata(path).map_err(|_| retryable("media_offline"))?;
    if metadata.len() != expected_size {
        return Err(retryable("input_changed"));
    }
    let mut file = File::open(path).map_err(|_| retryable("media_offline"))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        if cancellation.is_cancelled() {
            return Err(retryable("canceled"));
        }
        let read = file
            .read(&mut buffer)
            .map_err(|_| retryable("media_offline"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    if hasher.finalize().to_hex().as_str() != expected_hash {
        return Err(retryable("input_changed"));
    }
    Ok(())
}

fn wait_for_child(
    child: &mut Child,
    cancellation: &CancellationToken,
) -> Result<std::process::ExitStatus, JobHandlerFailure> {
    loop {
        if cancellation.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(retryable("canceled"));
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(_) => return Err(retryable("media_tool_failed")),
        }
    }
}

struct TempArtifact(PathBuf);
impl Drop for TempArtifact {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn handler_error(_: impl std::fmt::Display) -> JobHandlerFailure {
    retryable("representation_io_failed")
}
fn retryable(code: &str) -> JobHandlerFailure {
    JobHandlerFailure {
        kind: FailureKind::Retryable,
        code: code.into(),
        details: json!({"code": code}),
    }
}
fn terminal(code: &str) -> JobHandlerFailure {
    JobHandlerFailure {
        kind: FailureKind::Terminal,
        code: code.into(),
        details: json!({"code": code}),
    }
}
fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_and_paths_are_deterministic_and_recipe_versioned() {
        let settings = blake3::hash(PROXY_SETTINGS.as_bytes()).to_hex().to_string();
        let first =
            representation_id(&"a".repeat(64), 42, RepresentationKind::Proxy, &settings).unwrap();
        let second =
            representation_id(&"a".repeat(64), 42, RepresentationKind::Proxy, &settings).unwrap();
        assert_eq!(first, second);
        let path = representation_relative_path(&"a".repeat(64), PROXY_RECIPE_KEY, &first, "mp4");
        assert!(path.starts_with(Path::new("representations").join("a".repeat(64))));
        assert!(path.ends_with(format!("{}.mp4", first.as_str())));
    }

    #[test]
    fn safe_join_rejects_absolute_and_traversal_paths() {
        let root = Path::new("/derived");
        assert!(safe_join(root, "representations/a/file.mp4").is_ok());
        assert!(safe_join(root, "/etc/passwd").is_err());
        assert!(safe_join(root, "representations/../original.mp4").is_err());
    }
}
