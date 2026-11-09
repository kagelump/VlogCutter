use batch_estimator::{BatchEstimate, BatchEstimator, RateHistory};
use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use rcdvlog_core::{
    JobId, MediaEstimatorEnvironment, MediaEstimatorIdentity, MediaObjectId, NodeId, ProjectId,
    RepresentationId, StorageRootId, media_estimator_process_key,
};
use rcdvlog_sync::NodeTargetedJob;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::catalog::{open_read_only_connection, open_writable_connection};

const MAX_PAYLOAD_BYTES: usize = 65_536;
const MAX_IDENTITY_BYTES: usize = 16_384;
const MAX_EVENT_BYTES: usize = 8_192;
const MAX_ERROR_BYTES: usize = 8_192;
const MIN_LEASE_DURATION: Duration = Duration::from_millis(10);
const MAX_LEASE_DURATION: Duration = Duration::from_secs(3600);
const MAX_POLL_INTERVAL: Duration = Duration::from_secs(60);
const MAX_CANCELLATION_POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    RetryWait,
    Succeeded,
    Failed,
    Canceled,
}
impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Canceled)
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::RetryWait => "retry_wait",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }
}
fn job_state(value: String) -> Result<JobState, JobEngineError> {
    match value.as_str() {
        "queued" => Ok(JobState::Queued),
        "running" => Ok(JobState::Running),
        "retry_wait" => Ok(JobState::RetryWait),
        "succeeded" => Ok(JobState::Succeeded),
        "failed" => Ok(JobState::Failed),
        "canceled" => Ok(JobState::Canceled),
        _ => Err(JobEngineError::CorruptState(value)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobClass {
    Proxy,
    FullHash,
    Lightweight,
}
impl JobClass {
    /// #53's one shared local-heavy class predicate. Future local VLM or other
    /// heavy workers must join this set rather than introduce a parallel lane.
    pub fn is_local_heavy(self) -> bool {
        matches!(self, Self::Proxy | Self::FullHash)
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Proxy => "proxy",
            Self::FullHash => "full_hash",
            Self::Lightweight => "lightweight",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimScope {
    Any,
    Class(JobClass),
    Heavy,
    HeavyOne(JobClass),
    Lightweight,
}

fn job_class(value: String) -> Result<JobClass, JobEngineError> {
    match value.as_str() {
        "proxy" => Ok(JobClass::Proxy),
        "full_hash" => Ok(JobClass::FullHash),
        "lightweight" => Ok(JobClass::Lightweight),
        _ => Err(JobEngineError::CorruptState(value)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    Retryable,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff_seconds: u32,
    pub max_backoff_seconds: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobRequirements {
    pub schema_version: u32,
    pub requires_original: bool,
    pub requires_derived_write: bool,
    pub capabilities: Vec<ServerCapability>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerCapability {
    OriginalRead,
    DerivedWrite,
    Ffmpeg,
    Ffprobe,
    Exiftool,
}
impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff_seconds: 5,
            max_backoff_seconds: 300,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobDefinition {
    pub job_id: JobId,
    pub job_type: String,
    pub payload_version: u32,
    pub payload: Value,
    pub immutable_identity: Value,
    /// Typed, schema-versioned capability requirements for the handler.
    pub requirements: JobRequirements,
    pub target_node_id: NodeId,
    pub project_id: Option<ProjectId>,
    pub media_object_id: Option<MediaObjectId>,
    pub storage_root_id: Option<StorageRootId>,
    pub representation_id: Option<RepresentationId>,
    pub recipe_key: String,
    pub job_class: JobClass,
    pub idempotency_key: String,
    pub priority: i32,
    pub total_work_units: u64,
    pub total_items: u64,
    pub retry_policy: RetryPolicy,
    pub dependencies: Vec<JobId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimedJob {
    pub job: JobDefinition,
    pub state: JobState,
    pub attempt_number: u32,
    pub lease_owner: String,
    pub lease_token: String,
    pub lease_expires_at: String,
    pub completed_work_units: u64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobProgress {
    pub completed_items: u64,
    pub failed_items: u64,
    pub total_items: u64,
    pub completed_work_units: u64,
    pub total_work_units: u64,
    pub estimate: BatchEstimate,
}

/// A safe, read-only view of durable job state. Lease expiry is exposed for
/// diagnosis, while the lease owner and bearer token remain private.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSnapshot {
    pub definition: JobDefinition,
    pub state: JobState,
    pub attempt_count: u32,
    pub retryable: bool,
    pub available_at: String,
    pub cancel_requested_at: Option<String>,
    pub lease_active: bool,
    pub lease_expires_at: Option<String>,
    pub completed_work_units: u64,
    pub total_work_units: u64,
    pub completed_items: u64,
    pub failed_items: u64,
    pub total_items: u64,
    pub error: Option<JobSnapshotError>,
    pub output_identity: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSnapshotError {
    pub code: String,
    pub details: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobListItem {
    pub enqueue_sequence: u64,
    /// Reconstructed from durable counters and persisted recipe throughput.
    /// One durable job is executed by one worker; scheduler class concurrency
    /// controls how many jobs may run beside it.
    pub estimate: BatchEstimate,
    #[serde(flatten)]
    pub snapshot: JobSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobPage {
    pub items: Vec<JobListItem>,
    pub next_before: Option<u64>,
}

/// The scheduler's actual concurrency and the individual remaining task weights.
/// These are transient estimation inputs; durable counters remain authoritative.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEstimateInput {
    pub worker_count: usize,
    pub remaining_task_work_units: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobResultCommit {
    pub output_identity: Option<String>,
    pub details: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemCompletion {
    pub work_units: u64,
    pub succeeded: bool,
    pub worker_elapsed_millis: u64,
}

/// Result reported by a node that executed a node-targeted job.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeJobCompletion {
    Succeeded {
        #[serde(skip_serializing_if = "Option::is_none")]
        output_identity: Option<String>,
        details: Value,
    },
    Failed {
        code: String,
        details: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobEvent {
    pub event_sequence: u64,
    pub job_id: JobId,
    pub attempt_number: Option<u32>,
    pub event_type: String,
    pub state: JobState,
    pub details: Value,
    pub created_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobEventPage {
    pub events: Vec<JobEvent>,
    pub next_after: Option<u64>,
}

#[derive(Debug, Error)]
pub enum JobEngineError {
    #[error("invalid job input: {0}")]
    InvalidInput(String),
    #[error("job {0} was not found")]
    NotFound(String),
    #[error("idempotency key conflicts with immutable job identity")]
    IdempotencyConflict,
    #[error("lease is no longer current")]
    StaleLease,
    #[error("job is canceled")]
    Canceled,
    #[error("job state is not eligible: {0}")]
    InvalidState(String),
    #[error("corrupt durable job state: {0}")]
    CorruptState(String),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Infrastructure(#[from] anyhow::Error),
}

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}
#[derive(Debug, Default)]
pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[derive(Clone)]
pub struct JobEngine {
    database_path: PathBuf,
    node_id: NodeId,
    clock: Arc<dyn Clock>,
    /// Exact host/tool/device identity used to validate representation timing
    /// reuse. Adapters set this from their runtime media-tool configuration;
    /// unit tests may leave it unset to model an unknown environment.
    media_estimator_environment: Option<MediaEstimatorEnvironment>,
}

struct JobSnapshotRow {
    state: String,
    attempt_count: u32,
    available_at: String,
    cancel_requested_at: Option<String>,
    lease_expires_at: Option<String>,
    completed_work_units: i64,
    total_work_units: i64,
    completed_items: i64,
    failed_items: i64,
    total_items: i64,
    error_code: Option<String>,
    error_details_json: Option<String>,
    output_identity: Option<String>,
    created_at: String,
    started_at: Option<String>,
    updated_at: String,
    completed_at: Option<String>,
}

impl JobEngine {
    pub fn callbacks(&self) -> crate::JobCallbacks {
        crate::JobCallbacks {
            database_path: self.database_path.clone(),
            clock: self.clock.clone(),
        }
    }
    pub(crate) fn new(database_path: PathBuf, node_id: NodeId) -> Self {
        Self::with_clock(database_path, node_id, Arc::new(SystemClock))
    }
    pub fn with_clock(database_path: PathBuf, node_id: NodeId, clock: Arc<dyn Clock>) -> Self {
        Self {
            database_path,
            node_id,
            clock,
            media_estimator_environment: None,
        }
    }

    /// Bind the exact configured media-tool environment to this engine. This
    /// clone-safe builder is used by every server-side read path so HTTP and
    /// job workers evaluate the same compatibility rules.
    pub fn with_media_estimator_environment(
        mut self,
        environment: MediaEstimatorEnvironment,
    ) -> Self {
        self.media_estimator_environment = Some(environment);
        self
    }

    pub fn media_estimator_environment(&self) -> Option<&MediaEstimatorEnvironment> {
        self.media_estimator_environment.as_ref()
    }

    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }
    pub fn enqueue(&self, definition: JobDefinition) -> Result<JobDefinition, JobEngineError> {
        validate_definition(&definition)?;
        let now = timestamp(self.clock.now());
        let p = canonical_json(&definition.payload, MAX_PAYLOAD_BYTES)?;
        let immutable_identity =
            canonical_json(&definition.immutable_identity, MAX_IDENTITY_BYTES)?;
        let idempotency_document = canonical_json(
            &json!({
                "immutableIdentity": definition.immutable_identity,
                "payload": definition.payload,
                "requirements": definition.requirements,
                "targetNodeId": definition.target_node_id,
                "projectId": definition.project_id,
                "mediaObjectId": definition.media_object_id,
                "storageRootId": definition.storage_root_id,
                "representationId": definition.representation_id,
                "recipeKey": definition.recipe_key,
                "jobClass": definition.job_class,
                "priority": definition.priority,
                "totalWorkUnits": definition.total_work_units,
                "totalItems": definition.total_items,
                "retryPolicy": definition.retry_policy,
                "dependencies": definition.dependencies,
            }),
            MAX_PAYLOAD_BYTES,
        )?;
        let idempotency_hash = blake3::hash(idempotency_document.as_bytes())
            .to_hex()
            .to_string();
        let requirements = serde_json::to_value(&definition.requirements)?;
        let requirements = canonical_json(&requirements, MAX_IDENTITY_BYTES)?;
        let mut c = open_writable_connection(&self.database_path)?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((id,hash))=tx.query_row("SELECT job_id, idempotency_identity_hash FROM jobs WHERE target_node_id=?1 AND job_type=?2 AND idempotency_key=?3",params![definition.target_node_id.as_str(),definition.job_type,definition.idempotency_key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()? {
            if hash != idempotency_hash {return Err(JobEngineError::IdempotencyConflict)};
            let existing=load_definition(&tx,&id)?; tx.commit()?; return Ok(existing)
        }
        tx.execute(
            "UPDATE job_enqueue_sequences SET last_sequence=last_sequence+1 WHERE singleton_id=1",
            [],
        )?;
        let seq: i64 = tx.query_row(
            "SELECT last_sequence FROM job_enqueue_sequences WHERE singleton_id=1",
            [],
            |r| r.get(0),
        )?;
        tx.execute("INSERT INTO jobs(job_id,job_type,payload_version,payload_json,immutable_identity_json,requirements_json,target_node_id,project_id,media_object_id,storage_root_id,representation_id,recipe_key,job_class,idempotency_key,priority,idempotency_identity_hash,enqueue_sequence,state,attempt_count,max_attempts,retry_initial_seconds,retry_max_seconds,available_at,total_work_units,total_items,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,'queued',0,?18,?19,?20,?21,?22,?23,?21,?21)",params![definition.job_id.as_str(),definition.job_type,definition.payload_version,p,immutable_identity,requirements,definition.target_node_id.as_str(),definition.project_id.as_ref().map(ProjectId::as_str),definition.media_object_id.as_ref().map(MediaObjectId::as_str),definition.storage_root_id.as_ref().map(StorageRootId::as_str),definition.representation_id.as_ref().map(RepresentationId::as_str),definition.recipe_key,definition.job_class.as_str(),definition.idempotency_key,definition.priority,idempotency_hash,seq,definition.retry_policy.max_attempts,definition.retry_policy.initial_backoff_seconds,definition.retry_policy.max_backoff_seconds,now,definition.total_work_units as i64,definition.total_items as i64])?;
        for dep in &definition.dependencies {
            let cycle:Option<i64>=tx.query_row("WITH RECURSIVE reachable(job_id) AS (SELECT depends_on_job_id FROM job_dependencies WHERE job_id=?1 UNION SELECT d.depends_on_job_id FROM job_dependencies d JOIN reachable r ON d.job_id=r.job_id) SELECT 1 FROM reachable WHERE job_id=?2 LIMIT 1",params![dep.as_str(),definition.job_id.as_str()],|r|r.get(0)).optional()?;
            if cycle.is_some() {
                return Err(JobEngineError::InvalidInput(
                    "job dependency cycle is forbidden".into(),
                ));
            }
            tx.execute(
                "INSERT INTO job_dependencies(job_id,depends_on_job_id) VALUES(?1,?2)",
                params![definition.job_id.as_str(), dep.as_str()],
            )?;
        }
        emit(
            &tx,
            definition.job_id.as_str(),
            None,
            "enqueued",
            JobState::Queued,
            &json!({"enqueueSequence":seq}),
            &now,
        )?;
        tx.commit()?;
        Ok(definition)
    }

    /// Enqueue a job with a pre-determined job_id. The definition's job_id is
    /// ignored; `known_id` is used instead. Used for scan root jobs where the
    /// ID must be stable before the job row exists.
    pub fn enqueue_with_id(
        &self,
        known_id: &str,
        definition: JobDefinition,
    ) -> Result<JobDefinition, JobEngineError> {
        let job_id = JobId::parse(known_id)
            .map_err(|e| JobEngineError::InvalidInput(format!("invalid known job id: {e}")))?;
        // Check if already exists
        let existing = {
            let mut c = open_read_only_connection(&self.database_path)?;
            let tx = c.transaction()?;
            load_definition(&tx, known_id).ok()
        };
        if let Some(def) = existing {
            return Ok(def);
        }
        let mut definition = definition;
        definition.job_id = job_id;
        self.enqueue(definition)
    }

    /// Reads durable job state without running migrations or mutating the catalog.
    pub fn get_job(&self, id: &JobId) -> Result<JobSnapshot, JobEngineError> {
        let mut connection = open_read_only_connection(&self.database_path)?;
        let transaction = connection.transaction()?;
        let row: Option<JobSnapshotRow> = transaction
            .query_row(
                "SELECT state,attempt_count,available_at,cancel_requested_at,lease_expires_at,completed_work_units,total_work_units,completed_items,failed_items,total_items,error_code,error_details_json,output_identity,created_at,started_at,updated_at,completed_at FROM jobs WHERE job_id=?1",
                [id.as_str()],
                |row| Ok(JobSnapshotRow {
                    state: row.get(0)?,
                    attempt_count: row.get(1)?,
                    available_at: row.get(2)?,
                    cancel_requested_at: row.get(3)?,
                    lease_expires_at: row.get(4)?,
                    completed_work_units: row.get(5)?,
                    total_work_units: row.get(6)?,
                    completed_items: row.get(7)?,
                    failed_items: row.get(8)?,
                    total_items: row.get(9)?,
                    error_code: row.get(10)?,
                    error_details_json: row.get(11)?,
                    output_identity: row.get(12)?,
                    created_at: row.get(13)?,
                    started_at: row.get(14)?,
                    updated_at: row.get(15)?,
                    completed_at: row.get(16)?,
                }),
            )
            .optional()?;
        let row = row.ok_or_else(|| JobEngineError::NotFound(id.to_string()))?;
        let definition = load_definition(&transaction, id.as_str())?;
        let error = match (row.error_code, row.error_details_json) {
            (None, None) => None,
            (Some(code), Some(details_json)) => {
                valid_token(&code, "error code")?;
                let details: Value = serde_json::from_str(&details_json)?;
                validate_event(&details, MAX_ERROR_BYTES)?;
                Some(JobSnapshotError { code, details })
            }
            _ => {
                return Err(JobEngineError::CorruptState(
                    "job error code and details are inconsistent".into(),
                ));
            }
        };
        transaction.commit()?;
        let lease_active = row
            .lease_expires_at
            .as_deref()
            .is_some_and(|expiry| expiry > timestamp(self.clock.now()).as_str());
        let state = job_state(row.state)?;
        let retryable =
            state == JobState::Failed && row.attempt_count < definition.retry_policy.max_attempts;
        Ok(JobSnapshot {
            definition,
            state,
            attempt_count: row.attempt_count,
            retryable,
            available_at: row.available_at,
            cancel_requested_at: row.cancel_requested_at,
            lease_active,
            lease_expires_at: row.lease_expires_at,
            completed_work_units: snapshot_counter(row.completed_work_units, "completed work")?,
            total_work_units: snapshot_counter(row.total_work_units, "total work")?,
            completed_items: snapshot_counter(row.completed_items, "completed items")?,
            failed_items: snapshot_counter(row.failed_items, "failed items")?,
            total_items: snapshot_counter(row.total_items, "total items")?,
            error,
            output_identity: row.output_identity,
            created_at: row.created_at,
            started_at: row.started_at,
            updated_at: row.updated_at,
            completed_at: row.completed_at,
        })
    }

    pub fn list_jobs(
        &self,
        limit: u32,
        before: Option<u64>,
        state: Option<JobState>,
    ) -> Result<JobPage, JobEngineError> {
        if limit == 0 || limit > 200 {
            return Err(JobEngineError::InvalidInput(
                "limit must be between 1 and 200".to_string(),
            ));
        }
        let before = before
            .map(i64::try_from)
            .transpose()
            .map_err(|_| JobEngineError::InvalidInput("before is too large".to_string()))?;
        let connection = open_read_only_connection(&self.database_path)?;
        let mut statement = connection.prepare(
            "SELECT job_id,enqueue_sequence FROM jobs WHERE (?1 IS NULL OR enqueue_sequence < ?1) AND (?2 IS NULL OR state = ?2) ORDER BY enqueue_sequence DESC LIMIT ?3",
        )?;
        let rows = statement
            .query_map(
                params![before, state.map(JobState::as_str), i64::from(limit + 1)],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        drop(connection);
        let has_more = rows.len() > limit as usize;
        let mut items = Vec::with_capacity(rows.len().min(limit as usize));
        for (job_id, enqueue_sequence) in rows.into_iter().take(limit as usize) {
            let snapshot = self.get_job(
                &JobId::parse(job_id)
                    .map_err(|error| JobEngineError::CorruptState(error.to_string()))?,
            )?;
            let estimate = self.estimate_snapshot(&snapshot)?;
            items.push(JobListItem {
                enqueue_sequence: u64::try_from(enqueue_sequence).map_err(|_| {
                    JobEngineError::CorruptState("negative enqueue sequence".to_string())
                })?,
                estimate,
                snapshot,
            });
        }
        let next_before = has_more
            .then(|| items.last().map(|item| item.enqueue_sequence))
            .flatten();
        Ok(JobPage { items, next_before })
    }

    fn estimate_snapshot(&self, snapshot: &JobSnapshot) -> Result<BatchEstimate, JobEngineError> {
        let connection = open_read_only_connection(&self.database_path)?;
        let media_job = is_media_representation_job(&snapshot.definition);
        let (history, compatibility, uncertainty_reason) =
            if snapshot.state == JobState::Running && snapshot.lease_active {
                if media_job {
                    match load_attempt_estimator_identity(
                        &connection,
                        snapshot.definition.job_id.as_str(),
                        snapshot.attempt_count,
                    )? {
                        Some(identity)
                            if self.media_estimator_environment.as_ref().is_some_and(
                                |environment| {
                                    identity.is_compatible_with(
                                        environment,
                                        &snapshot.definition.recipe_key,
                                    )
                                },
                            ) =>
                        {
                            let rate_key =
                                identity.qualified_history_key(&identity.execution_recipe_key);
                            (
                                qualified_rate_history_for_read(
                                    &connection,
                                    self.node_id.as_str(),
                                    &rate_key,
                                    &identity,
                                )?,
                                TimingCompatibility::Compatible,
                                "no_qualified_media_observations",
                            )
                        }
                        Some(_) => (
                            RateHistory::default(),
                            TimingCompatibility::Unknown,
                            "stale_estimator_context",
                        ),
                        None => (
                            RateHistory::default(),
                            TimingCompatibility::Unknown,
                            "missing_attempt_estimator_context",
                        ),
                    }
                } else {
                    (
                        legacy_rate_history_for_read(
                            &connection,
                            self.node_id.as_str(),
                            &snapshot.definition.recipe_key,
                        )?,
                        TimingCompatibility::Legacy,
                        "no_history",
                    )
                }
            } else {
                let reason = match snapshot.state {
                    JobState::Queued | JobState::RetryWait => "waiting_for_eligible_slot",
                    JobState::Running => "interrupted_attempt",
                    JobState::Succeeded | JobState::Failed | JobState::Canceled => {
                        "terminal_not_executing"
                    }
                };
                (RateHistory::default(), TimingCompatibility::Unknown, reason)
            };
        let estimator = BatchEstimator::with_explicit_progress(
            snapshot.total_items as usize,
            snapshot.total_work_units,
            1,
            history,
            snapshot.completed_items as usize,
            snapshot.completed_work_units,
        );
        let remaining = snapshot
            .total_work_units
            .saturating_sub(snapshot.completed_work_units);
        let mut estimate = estimator.estimate(if remaining == 0 {
            &[]
        } else {
            std::slice::from_ref(&remaining)
        });
        match compatibility {
            TimingCompatibility::Compatible => {
                estimate = annotate_measured_estimate(estimate, history);
            }
            TimingCompatibility::Unknown | TimingCompatibility::UnverifiedMedia => {
                estimate = uncertain_estimate(estimate, uncertainty_reason);
            }
            TimingCompatibility::Legacy => {}
        }
        Ok(estimate)
    }

    pub fn retry_failed(&self, id: &JobId) -> Result<JobSnapshot, JobEngineError> {
        let now = timestamp(self.clock.now());
        let mut connection = open_writable_connection(&self.database_path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (state, attempt_count, max_attempts): (String, u32, u32) = transaction
            .query_row(
                "SELECT state,attempt_count,max_attempts FROM jobs WHERE job_id=?1",
                [id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| JobEngineError::NotFound(id.to_string()))?;
        if state != "failed" || attempt_count >= max_attempts {
            return Err(JobEngineError::InvalidState(format!(
                "failed job has {attempt_count} of {max_attempts} attempts consumed"
            )));
        }
        transaction.execute(
            "UPDATE jobs SET state='queued',available_at=?1,cancel_requested_at=NULL,cancel_reason=NULL,error_code=NULL,error_details_json=NULL,completed_at=NULL,updated_at=?1 WHERE job_id=?2 AND state='failed'",
            params![now, id.as_str()],
        )?;
        emit(
            &transaction,
            id.as_str(),
            None,
            "manual_retry_queued",
            JobState::Queued,
            &json!({}),
            &now,
        )?;
        transaction.commit()?;
        self.get_job(id)
    }

    /// List queued jobs targeted at a specific (non-server) node.
    /// Used during reconciliation to surface Mac-targeted work requests.
    pub fn pending_jobs_for_node(
        &self,
        target_node_id: &NodeId,
        project_id: &ProjectId,
    ) -> Result<Vec<NodeTargetedJob>, JobEngineError> {
        let connection = open_read_only_connection(&self.database_path)?;
        let mut statement = connection.prepare(
            r#"
            SELECT j.job_id, j.job_type, j.media_object_id, j.project_id,
                   j.representation_id, j.recipe_key, j.payload_json, j.created_at
            FROM jobs j
            WHERE j.target_node_id = ?1
              AND j.project_id = ?2
              AND j.state = 'queued'
              AND j.cancel_requested_at IS NULL
            ORDER BY j.priority DESC, j.enqueue_sequence ASC
            "#,
        )?;
        let rows = statement.query_map(
            rusqlite::params![target_node_id.as_str(), project_id.as_str()],
            |row| {
                let kind = if row.get::<_, String>(5)?.contains("thumbnail") {
                    "thumbnail"
                } else {
                    "proxy"
                };
                let payload: Value =
                    serde_json::from_str(&row.get::<_, String>(6)?).unwrap_or_default();
                Ok(NodeTargetedJob {
                    job_id: row.get(0)?,
                    job_type: row.get(1)?,
                    media_object_id: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    project_id: row.get(3)?,
                    representation_id: row.get(4)?,
                    kind: kind.to_string(),
                    payload,
                    created_at: row.get(8)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn claim_next(
        &self,
        lease_owner: &str,
        lease_for: Duration,
    ) -> Result<Option<ClaimedJob>, JobEngineError> {
        self.claim_next_for_class(lease_owner, lease_for, None)
    }

    pub fn claim_next_for_class(
        &self,
        lease_owner: &str,
        lease_for: Duration,
        class: Option<JobClass>,
    ) -> Result<Option<ClaimedJob>, JobEngineError> {
        self.claim_next_scoped(
            lease_owner,
            lease_for,
            class.map(ClaimScope::Class).unwrap_or(ClaimScope::Any),
        )
    }

    /// Claim the next eligible local-heavy job across every heavy class in one
    /// shared queue. #53 accepts one heavy slot, so proxy/transcode and
    /// full-hash work cannot occupy separate lanes. Explicitly queued work is
    /// intentional LIFO here: the newest eligible heavy job runs next.
    pub fn claim_next_heavy(
        &self,
        lease_owner: &str,
        lease_for: Duration,
    ) -> Result<Option<ClaimedJob>, JobEngineError> {
        self.claim_next_scoped(lease_owner, lease_for, ClaimScope::Heavy)
    }

    /// Claim across only the heavy classes explicitly enabled by the server
    /// configuration. The durable queue remains one shared eligible pool, so an
    /// empty proxy queue cannot idle an enabled full-hash class (or vice versa).
    pub fn claim_next_heavy_enabled(
        &self,
        lease_owner: &str,
        lease_for: Duration,
        proxy_enabled: bool,
        full_hash_enabled: bool,
    ) -> Result<Option<ClaimedJob>, JobEngineError> {
        match (proxy_enabled, full_hash_enabled) {
            (true, true) => self.claim_next_scoped(lease_owner, lease_for, ClaimScope::Heavy),
            (true, false) => self.claim_next_scoped(
                lease_owner,
                lease_for,
                ClaimScope::HeavyOne(JobClass::Proxy),
            ),
            (false, true) => self.claim_next_scoped(
                lease_owner,
                lease_for,
                ClaimScope::HeavyOne(JobClass::FullHash),
            ),
            (false, false) => Ok(None),
        }
    }

    /// Claim the next eligible lightweight job. Lightweight work may overlap
    /// the shared heavy slot and remains FIFO within its safety limit.
    pub fn claim_next_lightweight(
        &self,
        lease_owner: &str,
        lease_for: Duration,
    ) -> Result<Option<ClaimedJob>, JobEngineError> {
        self.claim_next_scoped(lease_owner, lease_for, ClaimScope::Lightweight)
    }

    fn claim_next_scoped(
        &self,
        lease_owner: &str,
        lease_for: Duration,
        scope: ClaimScope,
    ) -> Result<Option<ClaimedJob>, JobEngineError> {
        validate_lease_duration(lease_for)?;
        valid_token(lease_owner, "lease owner")?;
        let now = self.clock.now();
        let now_s = timestamp(now);
        let expiry = timestamp(
            now + ChronoDuration::from_std(lease_for).unwrap_or(ChronoDuration::seconds(30)),
        );
        let mut c = open_writable_connection(&self.database_path)?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        settle_dependency_terminal(&tx, &now_s)?;
        promote_retry_wait(&tx, &now_s)?;
        let dependency = " AND j.cancel_requested_at IS NULL AND NOT EXISTS(SELECT 1 FROM job_dependencies d JOIN jobs parent ON parent.job_id=d.depends_on_job_id WHERE d.job_id=j.job_id AND parent.state!='succeeded')";
        let id = match scope {
            ClaimScope::Any => tx.query_row(&format!("SELECT j.job_id FROM jobs j WHERE j.target_node_id=?1 AND j.state='queued' AND j.available_at<=?2{dependency} ORDER BY j.priority DESC,j.enqueue_sequence ASC LIMIT 1"), params![self.node_id.as_str(), now_s], |row| row.get::<_, String>(0)).optional()?,
            ClaimScope::Class(class) => tx.query_row(&format!("SELECT j.job_id FROM jobs j WHERE j.target_node_id=?1 AND j.job_class=?2 AND j.state='queued' AND j.available_at<=?3{dependency} ORDER BY j.priority DESC,j.enqueue_sequence ASC LIMIT 1"), params![self.node_id.as_str(), class.as_str(), now_s], |row| row.get::<_, String>(0)).optional()?,
            ClaimScope::Heavy => tx.query_row(&format!("SELECT j.job_id FROM jobs j WHERE j.target_node_id=?1 AND j.job_class IN ('proxy','full_hash') AND j.state='queued' AND j.available_at<=?2{dependency} ORDER BY j.priority DESC,j.enqueue_sequence DESC LIMIT 1"), params![self.node_id.as_str(), now_s], |row| row.get::<_, String>(0)).optional()?,
            ClaimScope::HeavyOne(class) => tx.query_row(&format!("SELECT j.job_id FROM jobs j WHERE j.target_node_id=?1 AND j.job_class=?2 AND j.state='queued' AND j.available_at<=?3{dependency} ORDER BY j.priority DESC,j.enqueue_sequence DESC LIMIT 1"), params![self.node_id.as_str(), class.as_str(), now_s], |row| row.get::<_, String>(0)).optional()?,
            ClaimScope::Lightweight => tx.query_row(&format!("SELECT j.job_id FROM jobs j WHERE j.target_node_id=?1 AND j.job_class='lightweight' AND j.state='queued' AND j.available_at<=?2{dependency} ORDER BY j.priority DESC,j.enqueue_sequence ASC LIMIT 1"), params![self.node_id.as_str(), now_s], |row| row.get::<_, String>(0)).optional()?,
        };
        let Some(id) = id else {
            tx.commit()?;
            return Ok(None);
        };
        let token = format!("lease_{}", Uuid::new_v4().simple());
        let changed=tx.execute("UPDATE jobs SET state='running',attempt_count=attempt_count+1,lease_owner=?1,lease_token=?2,lease_expires_at=?3,heartbeat_at=?4,started_at=COALESCE(started_at,?4),updated_at=?4 WHERE job_id=?5 AND state='queued' AND cancel_requested_at IS NULL",params![lease_owner,token,expiry,now_s,id])?;
        if changed != 1 {
            return Err(JobEngineError::StaleLease);
        }
        let attempt: u32 = tx.query_row(
            "SELECT attempt_count FROM jobs WHERE job_id=?1",
            [&id],
            |r| r.get(0),
        )?;
        tx.execute("INSERT INTO job_attempts(job_id,attempt_number,lease_owner,lease_token,state,started_at) VALUES(?1,?2,?3,?4,'running',?5)",params![id,attempt,lease_owner,token,now_s])?;
        emit(
            &tx,
            &id,
            Some(attempt),
            "claimed",
            JobState::Running,
            &json!({"leaseOwner":lease_owner}),
            &now_s,
        )?;
        let definition = load_definition(&tx, &id)?;
        let completed: i64 = tx.query_row(
            "SELECT completed_work_units FROM jobs WHERE job_id=?1",
            [&id],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(Some(ClaimedJob {
            job: definition,
            state: JobState::Running,
            attempt_number: attempt,
            lease_owner: lease_owner.into(),
            lease_token: token,
            lease_expires_at: expiry,
            completed_work_units: completed as u64,
            created_at: now_s,
        }))
    }
    pub fn heartbeat(&self, job: &ClaimedJob, lease_for: Duration) -> Result<(), JobEngineError> {
        validate_lease_duration(lease_for)?;
        self.with_current_attempt(job, |tx, now| {
            let expiry = timestamp(
                self.clock.now()
                    + ChronoDuration::from_std(lease_for).unwrap_or(ChronoDuration::seconds(30)),
            );
            tx.execute(
                "UPDATE jobs SET heartbeat_at=?1,lease_expires_at=?2,updated_at=?1 WHERE job_id=?3",
                params![now, expiry, job.job.job_id.as_str()],
            )?;
            Ok(())
        })
    }
    pub fn report_progress(
        &self,
        job: &ClaimedJob,
        completed: u64,
        estimate_input: ProgressEstimateInput,
    ) -> Result<JobProgress, JobEngineError> {
        self.with_current_attempt(job, |tx, now| {
            let (old, total, recipe, completed_items, failed_items, total_items): (i64, i64, String, i64, i64, i64) = tx.query_row(
                "SELECT completed_work_units,total_work_units,recipe_key,completed_items,failed_items,total_items FROM jobs WHERE job_id=?1",
                [job.job.job_id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )?;
            if completed < (old as u64) || completed > (total as u64) {
                return Err(JobEngineError::InvalidInput(
                    "progress must be monotonic and within total work".into(),
                ));
            }
            validate_progress_estimate_input(
                job.job.job_class,
                &estimate_input,
                completed_items as u64,
                total_items as u64,
                completed,
                total as u64,
            )?;
            tx.execute(
                "UPDATE jobs SET completed_work_units=?1,updated_at=?2 WHERE job_id=?3",
                params![completed as i64, now, job.job.job_id.as_str()],
            )?;
            let media_job = is_media_representation_job(&job.job);
            let (hist, compatibility, uncertainty_reason) = if media_job {
                match load_attempt_estimator_identity(
                    tx,
                    job.job.job_id.as_str(),
                    job.attempt_number,
                )? {
                    Some(identity)
                        if self
                            .media_estimator_environment
                            .as_ref()
                            .is_some_and(|environment| {
                                identity.is_compatible_with(environment, &recipe)
                            }) =>
                    {
                        let rate_key =
                            identity.qualified_history_key(&identity.execution_recipe_key);
                        (
                            qualified_rate_history(
                                tx,
                                self.node_id.as_str(),
                                &rate_key,
                                &identity,
                            )?,
                            TimingCompatibility::Compatible,
                            "no_qualified_media_observations",
                        )
                    }
                    Some(_) => (
                        RateHistory::default(),
                        TimingCompatibility::Unknown,
                        "stale_estimator_context",
                    ),
                    None => (
                        RateHistory::default(),
                        TimingCompatibility::Unknown,
                        "missing_attempt_estimator_context",
                    ),
                }
            } else {
                (
                    rate_history(tx, self.node_id.as_str(), &recipe)?,
                    TimingCompatibility::Legacy,
                    "no_history",
                )
            };
            let estimator = BatchEstimator::with_explicit_progress(
                total_items as usize,
                total as u64,
                estimate_input.worker_count,
                hist,
                completed_items as usize,
                completed,
            );
            let mut estimate = estimator.estimate(&estimate_input.remaining_task_work_units);
            match compatibility {
                TimingCompatibility::Compatible => {
                    estimate = annotate_measured_estimate(estimate, hist);
                }
                TimingCompatibility::Unknown | TimingCompatibility::UnverifiedMedia => {
                    estimate = uncertain_estimate(estimate, uncertainty_reason);
                }
                TimingCompatibility::Legacy => {}
            }
            emit(
                tx,
                job.job.job_id.as_str(),
                Some(job.attempt_number),
                "progress",
                JobState::Running,
                &json!({"completedWorkUnits":completed}),
                now,
            )?;
            Ok(JobProgress {
                completed_items: completed_items as u64,
                failed_items: failed_items as u64,
                total_items: total_items as u64,
                completed_work_units: completed,
                total_work_units: total as u64,
                estimate,
            })
        })
    }
    pub fn record_item_completion(
        &self,
        job: &ClaimedJob,
        item: ItemCompletion,
        estimate_input: ProgressEstimateInput,
    ) -> Result<JobProgress, JobEngineError> {
        self.record_item_completion_with_rate_key(job, item, estimate_input, None)
    }

    /// Record success/failure and, when provided, train throughput under the
    /// encoder/worker key that actually produced the output rather than the
    /// job's semantic representation recipe.
    pub fn record_item_completion_with_rate_key(
        &self,
        job: &ClaimedJob,
        item: ItemCompletion,
        estimate_input: ProgressEstimateInput,
        rate_key: Option<&str>,
    ) -> Result<JobProgress, JobEngineError> {
        self.record_item_completion_internal(
            job,
            item,
            estimate_input,
            RateObservation::Legacy(rate_key),
        )
    }

    /// Record a media item together with the exact estimator identity observed
    /// by the handler. Successful observations train only this qualified key;
    /// failed or interrupted attempts update counters but never learn a rate.
    /// The semantic representation recipe remains on the job and is checked
    /// but is never rewritten by this method.
    pub fn record_item_completion_with_estimator_identity(
        &self,
        job: &ClaimedJob,
        item: ItemCompletion,
        estimate_input: ProgressEstimateInput,
        estimator_identity: Option<&MediaEstimatorIdentity>,
    ) -> Result<JobProgress, JobEngineError> {
        self.record_item_completion_internal(
            job,
            item,
            estimate_input,
            RateObservation::Media(estimator_identity),
        )
    }

    fn record_item_completion_internal(
        &self,
        job: &ClaimedJob,
        item: ItemCompletion,
        estimate_input: ProgressEstimateInput,
        observation: RateObservation<'_>,
    ) -> Result<JobProgress, JobEngineError> {
        self.with_current_attempt(job, |tx, now| {
            let (completed_work, total_work, completed_items, failed_items, total_items, recipe): (i64, i64, i64, i64, i64, String) = tx.query_row(
                "SELECT completed_work_units,total_work_units,completed_items,failed_items,total_items,recipe_key FROM jobs WHERE job_id=?1",
                [job.job.job_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )?;
            if completed_items >= total_items || item.work_units > (total_work as u64).saturating_sub(completed_work as u64) {
                return Err(JobEngineError::InvalidInput("item completion exceeds declared job totals".into()));
            }

            let (history_recipe, estimator_identity, compatibility) = match observation {
                RateObservation::Legacy(rate_key) => {
                    if is_media_representation_job(&job.job) {
                        // The old semantic/rate-key-only path has no durable
                        // estimator context. It may advance work counters but
                        // must not seed or train a media timing.
                        (
                            recipe.clone(),
                            None,
                            TimingCompatibility::UnverifiedMedia,
                        )
                    } else {
                        (
                            rate_key.unwrap_or(&recipe).to_string(),
                            None,
                            TimingCompatibility::Legacy,
                        )
                    }
                }
                RateObservation::Media(identity) => {
                    let Some(identity) = identity else {
                        return Err(JobEngineError::InvalidInput(
                            "media item completion requires an estimator identity".into(),
                        ));
                    };
                    if identity.semantic_recipe_key != recipe {
                        return Err(JobEngineError::InvalidInput(
                            "estimator identity does not match the immutable semantic recipe".into(),
                        ));
                    }
                    let compatibility = match self.media_estimator_environment.as_ref() {
                        Some(environment) if identity.is_compatible_with(environment, &recipe) => {
                            TimingCompatibility::Compatible
                        }
                        Some(_) => {
                            return Err(JobEngineError::InvalidInput(
                                "estimator identity does not match the configured media-tool environment"
                                    .into(),
                            ));
                        }
                        None => TimingCompatibility::Unknown,
                    };
                    (
                        identity.qualified_history_key(&identity.execution_recipe_key),
                        Some(identity),
                        compatibility,
                    )
                }
            };

            let next_completed = completed_items + 1;
            let next_failed = failed_items + i64::from(!item.succeeded);
            let next_work = completed_work + item.work_units as i64;
            validate_progress_estimate_input(
                job.job.job_class,
                &estimate_input,
                next_completed as u64,
                total_items as u64,
                next_work as u64,
                total_work as u64,
            )?;
            tx.execute("UPDATE jobs SET completed_work_units=?1,completed_items=?2,failed_items=?3,updated_at=?4 WHERE job_id=?5", params![next_work,next_completed,next_failed,now,job.job.job_id.as_str()])?;

            if let Some(identity) = estimator_identity {
                persist_attempt_estimator_identity(
                    tx,
                    job.job.job_id.as_str(),
                    job.attempt_number,
                    identity,
                    &history_recipe,
                )?;
            }

            let mut history = match compatibility {
                TimingCompatibility::Compatible => qualified_rate_history(
                    tx,
                    self.node_id.as_str(),
                    &history_recipe,
                    estimator_identity.expect("compatible observations carry an identity"),
                )?,
                TimingCompatibility::Legacy => {
                    rate_history(tx, self.node_id.as_str(), &history_recipe)?
                }
                TimingCompatibility::UnverifiedMedia | TimingCompatibility::Unknown => {
                    RateHistory::default()
                }
            };
            if item.succeeded {
                history.observe(item.work_units, Duration::from_millis(item.worker_elapsed_millis));
                match compatibility {
                    TimingCompatibility::Compatible | TimingCompatibility::Unknown => {
                        if let Some(identity) = estimator_identity {
                            // Unknown persist provenance but consumers will not
                            // present the rate as an established ETA until a
                            // configured environment proves compatibility.
                            persist_qualified_rate_history(
                                tx,
                                self.node_id.as_str(),
                                &history_recipe,
                                identity,
                                history,
                                now,
                            )?;
                        }
                    }
                    TimingCompatibility::Legacy => persist_rate_history(
                        tx,
                        self.node_id.as_str(),
                        &history_recipe,
                        history,
                        now,
                    )?,
                    TimingCompatibility::UnverifiedMedia => {}
                }
            }
            let estimator = BatchEstimator::with_explicit_progress(
                total_items as usize,
                total_work as u64,
                estimate_input.worker_count,
                history,
                next_completed as usize,
                next_work as u64,
            );
            let mut estimate = estimator.estimate(&estimate_input.remaining_task_work_units);
            match compatibility {
                TimingCompatibility::Compatible => {
                    estimate = annotate_measured_estimate(estimate, history);
                }
                TimingCompatibility::Unknown => {
                    estimate = uncertain_estimate(
                        estimate,
                        "estimator_environment_not_configured",
                    );
                }
                TimingCompatibility::UnverifiedMedia => {
                    estimate = uncertain_estimate(estimate, "media_requires_estimator_context");
                }
                TimingCompatibility::Legacy => {}
            }
            emit(tx, job.job.job_id.as_str(), Some(job.attempt_number), "item_completed", JobState::Running, &json!({"succeeded":item.succeeded,"workUnits":item.work_units}), now)?;
            Ok(JobProgress { completed_items: next_completed as u64, failed_items: next_failed as u64, total_items: total_items as u64, completed_work_units: next_work as u64, total_work_units: total_work as u64, estimate })
        })
    }

    /// Persist the estimator context for an attempt without teaching rate
    /// history. Failure and fallback paths use this to retain actual execution
    /// provenance while keeping failed overhead out of successful rates.
    pub fn attach_attempt_estimator_identity(
        &self,
        job: &ClaimedJob,
        estimator_identity: &MediaEstimatorIdentity,
    ) -> Result<(), JobEngineError> {
        self.with_current_attempt(job, |tx, _now| {
            let recipe: String = tx.query_row(
                "SELECT recipe_key FROM jobs WHERE job_id=?1",
                [job.job.job_id.as_str()],
                |row| row.get(0),
            )?;
            if estimator_identity.semantic_recipe_key != recipe {
                return Err(JobEngineError::InvalidInput(
                    "estimator identity does not match the immutable semantic recipe".into(),
                ));
            }
            if let Some(environment) = self.media_estimator_environment.as_ref()
                && !estimator_identity.is_compatible_with(environment, &recipe)
            {
                return Err(JobEngineError::InvalidInput(
                    "estimator identity does not match the configured media-tool environment"
                        .into(),
                ));
            }
            persist_attempt_estimator_identity(
                tx,
                job.job.job_id.as_str(),
                job.attempt_number,
                estimator_identity,
                &estimator_identity.qualified_history_key(&estimator_identity.execution_recipe_key),
            )
        })
    }

    pub fn request_cancel(&self, id: &JobId, reason: Option<&str>) -> Result<(), JobEngineError> {
        let now = timestamp(self.clock.now());
        let reason = reason.map(redact_text).transpose()?;
        let mut c = open_writable_connection(&self.database_path)?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state: String = tx
            .query_row(
                "SELECT state FROM jobs WHERE job_id=?1",
                [id.as_str()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| JobEngineError::NotFound(id.to_string()))?;
        let st = job_state(state)?;
        match st {
            JobState::Queued | JobState::RetryWait => {
                tx.execute("UPDATE jobs SET state='canceled',cancel_requested_at=?1,cancel_reason=?2,updated_at=?1,completed_at=?1 WHERE job_id=?3",params![now,reason,id.as_str()])?;
                emit(
                    &tx,
                    id.as_str(),
                    None,
                    "canceled",
                    JobState::Canceled,
                    &json!({"reason":reason}),
                    &now,
                )?
            }
            JobState::Running => {
                tx.execute("UPDATE jobs SET cancel_requested_at=COALESCE(cancel_requested_at,?1),cancel_reason=COALESCE(cancel_reason,?2),updated_at=?1 WHERE job_id=?3",params![now,reason,id.as_str()])?;
                emit(
                    &tx,
                    id.as_str(),
                    None,
                    "cancel_requested",
                    JobState::Running,
                    &json!({"reason":reason}),
                    &now,
                )?
            }
            _ => {}
        }
        tx.commit()?;
        Ok(())
    }
    pub fn cancellation_requested(&self, job: &ClaimedJob) -> Result<bool, JobEngineError> {
        let c = open_writable_connection(&self.database_path)?;
        let now = timestamp(self.clock.now());
        let state: Option<(String, Option<String>, String, Option<String>)> = c
            .query_row(
                "SELECT state,cancel_requested_at,lease_token,lease_expires_at FROM jobs WHERE job_id=?1",
                [job.job.job_id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((state, cancel, token, expires_at)) = state else {
            return Err(JobEngineError::NotFound(job.job.job_id.to_string()));
        };
        if token != job.lease_token
            || state != "running"
            || expires_at.as_deref() <= Some(now.as_str())
        {
            return Err(JobEngineError::StaleLease);
        };
        Ok(cancel.is_some())
    }
    pub fn complete<F>(
        &self,
        job: &ClaimedJob,
        result: JobResultCommit,
        commit: F,
    ) -> Result<(), JobEngineError>
    where
        F: FnOnce(&Transaction<'_>) -> Result<(), JobEngineError>,
    {
        self.finish_success(job, result, commit)
    }
    fn finish_success<F>(
        &self,
        job: &ClaimedJob,
        result: JobResultCommit,
        commit: F,
    ) -> Result<(), JobEngineError>
    where
        F: FnOnce(&Transaction<'_>) -> Result<(), JobEngineError>,
    {
        validate_event(&result.details, MAX_EVENT_BYTES)?;
        let output = result
            .output_identity
            .as_deref()
            .map(redact_text)
            .transpose()?;
        let now = timestamp(self.clock.now());
        let mut c = open_writable_connection(&self.database_path)?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current:i64=tx.query_row("SELECT COUNT(*) FROM jobs j JOIN job_attempts a ON a.job_id=j.job_id AND a.attempt_number=?2 WHERE j.job_id=?1 AND j.state='running' AND j.lease_token=?3 AND j.lease_owner=?4 AND j.lease_expires_at>?5 AND a.state='running' AND a.lease_token=?3",params![job.job.job_id.as_str(),job.attempt_number,job.lease_token,job.lease_owner,now],|r|r.get(0))?;
        if current != 1 {
            return Err(JobEngineError::StaleLease);
        }
        let canceled: Option<String> = tx.query_row(
            "SELECT cancel_requested_at FROM jobs WHERE job_id=?1",
            [job.job.job_id.as_str()],
            |r| r.get(0),
        )?;
        if canceled.is_some() {
            ack_cancel(&tx, job, &now)?;
            tx.commit()?;
            return Err(JobEngineError::Canceled);
        }
        commit(&tx)?;
        let changed=tx.execute("UPDATE jobs SET state='succeeded',output_identity=?1,lease_owner=NULL,lease_token=NULL,lease_expires_at=NULL,completed_work_units=total_work_units,completed_items=total_items,updated_at=?2,completed_at=?2,error_code=NULL,error_details_json=NULL WHERE job_id=?3 AND state='running' AND lease_token=?4 AND cancel_requested_at IS NULL",params![output,now,job.job.job_id.as_str(),job.lease_token])?;
        if changed != 1 {
            return Err(JobEngineError::Canceled);
        }
        tx.execute("UPDATE job_attempts SET state='succeeded',finished_at=?1 WHERE job_id=?2 AND attempt_number=?3 AND lease_token=?4",params![now,job.job.job_id.as_str(),job.attempt_number,job.lease_token])?;
        emit(
            &tx,
            job.job.job_id.as_str(),
            Some(job.attempt_number),
            "succeeded",
            JobState::Succeeded,
            &result.details,
            &now,
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn fail(
        &self,
        job: &ClaimedJob,
        kind: FailureKind,
        code: &str,
        details: Value,
    ) -> Result<JobState, JobEngineError> {
        valid_token(code, "error code")?;
        validate_event(&details, MAX_ERROR_BYTES)?;
        self.with_current_attempt(job, |tx, now| {
            let (attempt, max_attempts, max_backoff): (u32, u32, u32) = tx.query_row(
                "SELECT attempt_count,max_attempts,retry_max_seconds FROM jobs WHERE job_id=?1",
                [job.job.job_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            let canceled: Option<String> = tx.query_row("SELECT cancel_requested_at FROM jobs WHERE job_id=?1", [job.job.job_id.as_str()], |row| row.get(0))?;
            if canceled.is_some() { ack_cancel(tx, job, now)?; return Ok(JobState::Canceled); }
            let state = if kind == FailureKind::Retryable && attempt < max_attempts { JobState::RetryWait } else { JobState::Failed };
            let backoff = capped_backoff_seconds(job.job.retry_policy.initial_backoff_seconds, max_backoff, attempt);
            let available = if state == JobState::RetryWait { timestamp(self.clock.now() + ChronoDuration::seconds(backoff as i64)) } else { now.clone() };
            let detail_json = canonical_json(&details, MAX_ERROR_BYTES)?;
            tx.execute("UPDATE jobs SET state=?1,available_at=?2,lease_owner=NULL,lease_token=NULL,lease_expires_at=NULL,error_code=?3,error_details_json=?4,updated_at=?5,completed_at=CASE WHEN ?1='failed' THEN ?5 ELSE NULL END WHERE job_id=?6", params![state.as_str(),available,code,detail_json,now,job.job.job_id.as_str()])?;
            tx.execute("UPDATE job_attempts SET state='failed',finished_at=?1,error_code=?2,error_details_json=?3 WHERE job_id=?4 AND attempt_number=?5 AND lease_token=?6", params![now,code,canonical_json(&details,MAX_ERROR_BYTES)?,job.job.job_id.as_str(),job.attempt_number,job.lease_token])?;
            emit(tx, job.job.job_id.as_str(), Some(job.attempt_number), if state == JobState::RetryWait { "retry_scheduled" } else { "failed" }, state, &details, now)?;
            Ok(state)
        })
    }

    /// Complete a node-targeted job that was executed externally (on the Mac).
    /// Unlike `complete()`, this does not require a lease — node-targeted jobs
    /// are never claimed by the server scheduler.
    pub fn complete_node_targeted_job(
        &self,
        job_id: &JobId,
        target_node_id: &NodeId,
        status: &NodeJobCompletion,
    ) -> Result<(), JobEngineError> {
        let now = timestamp(self.clock.now());
        let mut connection = open_writable_connection(&self.database_path)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        // Verify the job is queued and belongs to this target node
        let current: (String, String) = tx
            .query_row(
                "SELECT state, target_node_id FROM jobs WHERE job_id=?1",
                [job_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| JobEngineError::NotFound(job_id.to_string()))?;

        if current.0 != "queued" {
            return Err(JobEngineError::CorruptState(format!(
                "job {} is in state '{}', expected 'queued'",
                job_id.as_str(),
                current.0
            )));
        }
        if current.1 != target_node_id.as_str() {
            return Err(JobEngineError::CorruptState(format!(
                "job {} belongs to node '{}', not '{}'",
                job_id.as_str(),
                current.1,
                target_node_id.as_str()
            )));
        }

        let canceled: Option<String> = tx.query_row(
            "SELECT cancel_requested_at FROM jobs WHERE job_id=?1",
            [job_id.as_str()],
            |row| row.get(0),
        )?;
        if canceled.is_some() {
            ack_cancel_simple(&tx, job_id.as_str(), &now)?;
            tx.commit()?;
            return Ok(());
        }

        match status {
            NodeJobCompletion::Succeeded {
                output_identity,
                details,
            } => {
                let output = output_identity.as_deref().map(redact_text).transpose()?;
                validate_event(details, MAX_EVENT_BYTES)?;
                tx.execute(
                    "UPDATE jobs SET state='succeeded', output_identity=?2, \
                     completed_work_units=total_work_units, completed_items=total_items, \
                     updated_at=?3, completed_at=?3, error_code=NULL, error_details_json=NULL \
                     WHERE job_id=?1 AND state='queued' AND cancel_requested_at IS NULL",
                    params![job_id.as_str(), output, now],
                )?;
                emit(
                    &tx,
                    job_id.as_str(),
                    None,
                    "succeeded",
                    JobState::Succeeded,
                    details,
                    &now,
                )?;
            }
            NodeJobCompletion::Failed { code, details } => {
                valid_token(code, "error code")?;
                validate_event(details, MAX_ERROR_BYTES)?;
                let detail_json = canonical_json(details, MAX_ERROR_BYTES)?;
                tx.execute(
                    "UPDATE jobs SET state='failed', error_code=?2, error_details_json=?3, \
                     updated_at=?4, completed_at=?4 WHERE job_id=?1 AND state='queued' \
                     AND cancel_requested_at IS NULL",
                    params![job_id.as_str(), code, detail_json, now],
                )?;
                emit(
                    &tx,
                    job_id.as_str(),
                    None,
                    "failed",
                    JobState::Failed,
                    details,
                    &now,
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn recover_expired(&self) -> Result<u64, JobEngineError> {
        let now = timestamp(self.clock.now());
        let lease_expired_details = canonical_json(&json!({}), MAX_ERROR_BYTES)?;
        let mut c = open_writable_connection(&self.database_path)?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut stmt=tx.prepare("SELECT job_id,attempt_count,max_attempts,cancel_requested_at FROM jobs WHERE state='running' AND lease_expires_at<?1")?;
        let rows: Vec<(String, u32, u32, Option<String>)> = stmt
            .query_map([&now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        for (id, attempt, max, cancel) in &rows {
            tx.execute("UPDATE job_attempts SET state='expired',finished_at=?1 WHERE job_id=?2 AND attempt_number=?3 AND state='running'",params![now,id,attempt])?;
            let state = if cancel.is_some() {
                JobState::Canceled
            } else if attempt >= max {
                JobState::Failed
            } else {
                JobState::RetryWait
            };
            tx.execute("UPDATE jobs SET state=?1,available_at=?2,lease_owner=NULL,lease_token=NULL,lease_expires_at=NULL,updated_at=?2,error_code='lease_expired',error_details_json=?3,completed_at=CASE WHEN ?1 IN ('canceled','failed') THEN ?2 ELSE NULL END WHERE job_id=?4",params![state.as_str(),now,lease_expired_details,id])?;
            emit(
                &tx,
                id,
                Some(*attempt),
                if state == JobState::Canceled {
                    "canceled"
                } else if state == JobState::Failed {
                    "failed"
                } else {
                    "lease_expired"
                },
                state,
                &json!({}),
                &now,
            )?
        }
        tx.commit()?;
        Ok(rows.len() as u64)
    }
    pub fn replay_events(&self, after: u64, limit: u32) -> Result<JobEventPage, JobEngineError> {
        if limit == 0 || limit > 500 {
            return Err(JobEngineError::InvalidInput(
                "event limit must be 1..=500".into(),
            ));
        }
        if after > i64::MAX as u64 {
            return Err(JobEngineError::InvalidInput(
                "event cursor exceeds SQLite integer range".into(),
            ));
        }
        let c = open_writable_connection(&self.database_path)?;
        let mut stmt=c.prepare("SELECT event_sequence,job_id,attempt_number,event_type,state,details_json,created_at FROM job_events WHERE event_sequence>?1 ORDER BY event_sequence ASC LIMIT ?2")?;
        let events = stmt
            .query_map(params![after as i64, limit], event_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        let next_after = events.last().map(|e| e.event_sequence);
        Ok(JobEventPage { events, next_after })
    }
    fn with_current_attempt<T>(
        &self,
        job: &ClaimedJob,
        f: impl FnOnce(&Transaction<'_>, &String) -> Result<T, JobEngineError>,
    ) -> Result<T, JobEngineError> {
        let now = timestamp(self.clock.now());
        let mut c = open_writable_connection(&self.database_path)?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let n=tx.execute("UPDATE jobs SET updated_at=updated_at WHERE job_id=?1 AND state='running' AND lease_token=?2 AND lease_owner=?3 AND lease_expires_at>?4",params![job.job.job_id.as_str(),job.lease_token,job.lease_owner,now])?;
        if n != 1 {
            return Err(JobEngineError::StaleLease);
        }
        let n=tx.execute("UPDATE job_attempts SET state=state WHERE job_id=?1 AND attempt_number=?2 AND state='running' AND lease_token=?3",params![job.job.job_id.as_str(),job.attempt_number,job.lease_token])?;
        if n != 1 {
            return Err(JobEngineError::StaleLease);
        }
        let out = f(&tx, &now)?;
        tx.commit()?;
        Ok(out)
    }
}

fn validate_definition(d: &JobDefinition) -> Result<(), JobEngineError> {
    valid_token(&d.job_type, "job type")?;
    valid_token(&d.recipe_key, "recipe key")?;
    valid_token(&d.idempotency_key, "idempotency key")?;
    if d.payload_version == 0 {
        return Err(JobEngineError::InvalidInput(
            "payloadVersion must be positive".into(),
        ));
    }
    if d.total_work_units > i64::MAX as u64 || d.total_items > i64::MAX as u64 {
        return Err(JobEngineError::InvalidInput(
            "job totals exceed SQLite integer range".into(),
        ));
    }
    if !(1..=100).contains(&d.retry_policy.max_attempts)
        || !(1..=86400).contains(&d.retry_policy.initial_backoff_seconds)
        || !(d.retry_policy.initial_backoff_seconds..=86400)
            .contains(&d.retry_policy.max_backoff_seconds)
    {
        return Err(JobEngineError::InvalidInput(
            "retry policy is outside safe bounds".into(),
        ));
    }
    if !d.payload.is_object() || !d.immutable_identity.is_object() {
        return Err(JobEngineError::InvalidInput(
            "payload and immutable identity must be typed JSON objects".into(),
        ));
    }
    if d.requirements.schema_version != 1 || d.requirements.capabilities.len() > 5 {
        return Err(JobEngineError::InvalidInput(
            "requirements are outside the typed bounded contract".into(),
        ));
    }
    forbidden_command(&d.payload)?;
    canonical_json(&d.payload, MAX_PAYLOAD_BYTES)?;
    canonical_json(&d.immutable_identity, MAX_IDENTITY_BYTES)?;
    Ok(())
}
fn snapshot_counter(value: i64, field: &str) -> Result<u64, JobEngineError> {
    u64::try_from(value).map_err(|_| {
        JobEngineError::CorruptState(format!("job snapshot {field} counter is negative"))
    })
}
fn forbidden_command(v: &Value) -> Result<(), JobEngineError> {
    match v {
        Value::Object(m) => {
            for (k, v) in m {
                if matches!(
                    k.to_ascii_lowercase().as_str(),
                    "command" | "argv" | "executable" | "shell" | "program"
                ) {
                    return Err(JobEngineError::InvalidInput(
                        "arbitrary executable command payloads are forbidden".into(),
                    ));
                }
                forbidden_command(v)?
            }
        }
        Value::Array(a) => {
            for v in a {
                forbidden_command(v)?
            }
        }
        _ => {}
    }
    Ok(())
}
fn valid_token(s: &str, field: &str) -> Result<(), JobEngineError> {
    if s.is_empty() || s.len() > 255 || s.chars().any(|c| c.is_control() || c.is_whitespace()) {
        Err(JobEngineError::InvalidInput(format!("{field} is invalid")))
    } else {
        Ok(())
    }
}
fn redact_text(s: &str) -> Result<String, JobEngineError> {
    if s.len() > 1024
        || s.contains('/')
        || s.contains('\\')
        || s.to_ascii_lowercase().contains("secret")
        || s.to_ascii_lowercase().contains("token")
    {
        return Err(JobEngineError::InvalidInput(
            "error/output text may not contain paths or secrets".into(),
        ));
    }
    Ok(s.into())
}
fn canonical_json(v: &Value, max: usize) -> Result<String, JobEngineError> {
    let s = serde_json::to_string(v)?;
    if s.len() > max {
        return Err(JobEngineError::InvalidInput(
            "JSON payload exceeds its bounded contract".into(),
        ));
    }
    Ok(s)
}
fn validate_event(v: &Value, max: usize) -> Result<(), JobEngineError> {
    forbidden_command(v)?;
    validate_safe_details(v)?;
    canonical_json(v, max).map(|_| ())
}
fn validate_safe_details(value: &Value) -> Result<(), JobEngineError> {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                let key = key.to_ascii_lowercase();
                if ["secret", "token", "password", "authorization", "credential"]
                    .iter()
                    .any(|needle| key.contains(needle))
                {
                    return Err(JobEngineError::InvalidInput(
                        "details may not contain secrets".into(),
                    ));
                }
                validate_safe_details(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_safe_details(value)?;
            }
        }
        Value::String(value) => {
            let lower = value.to_ascii_lowercase();
            if value.starts_with('/')
                || value.starts_with('\\')
                || value.starts_with("file:")
                || (value.len() > 2
                    && value.as_bytes()[1] == b':'
                    && matches!(value.as_bytes()[2], b'\\' | b'/'))
                || lower.contains("bearer ")
            {
                return Err(JobEngineError::InvalidInput(
                    "details may not contain filesystem paths or secrets".into(),
                ));
            }
        }
        _ => {}
    }
    Ok(())
}
fn timestamp(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}
fn capped_backoff_seconds(initial: u32, maximum: u32, attempt: u32) -> u32 {
    let shift = attempt.saturating_sub(1).min(31);
    initial.saturating_mul(1_u32 << shift).min(maximum)
}
fn emit(
    tx: &Transaction<'_>,
    id: &str,
    attempt: Option<u32>,
    typ: &str,
    state: JobState,
    details: &Value,
    now: &str,
) -> Result<(), JobEngineError> {
    valid_token(typ, "event type")?;
    validate_event(details, MAX_EVENT_BYTES)?;
    tx.execute("INSERT INTO job_events(job_id,attempt_number,event_type,state,details_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![id,attempt,typ,state.as_str(),canonical_json(details,MAX_EVENT_BYTES)?,now])?;
    crate::job_callbacks::record_terminal(
        tx,
        &JobEvent {
            event_sequence: tx.last_insert_rowid() as u64,
            job_id: JobId::parse(id.to_owned())
                .map_err(|_| JobEngineError::InvalidInput("invalid job ID".into()))?,
            attempt_number: attempt,
            event_type: typ.into(),
            state,
            details: details.clone(),
            created_at: now.into(),
        },
    )?;
    Ok(())
}
fn promote_retry_wait(tx: &Transaction<'_>, now: &str) -> Result<(), JobEngineError> {
    let mut s=tx.prepare("SELECT job_id FROM jobs WHERE state='retry_wait' AND available_at<=?1 AND cancel_requested_at IS NULL")?;
    let ids: Vec<String> = s
        .query_map([now], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    drop(s);
    for id in ids {
        tx.execute(
            "UPDATE jobs SET state='queued',updated_at=?1 WHERE job_id=?2 AND state='retry_wait'",
            params![now, id],
        )?;
        emit(
            tx,
            &id,
            None,
            "retry_ready",
            JobState::Queued,
            &json!({}),
            now,
        )?
    }
    Ok(())
}
fn settle_dependency_terminal(tx: &Transaction<'_>, now: &str) -> Result<(), JobEngineError> {
    let mut s=tx.prepare("SELECT DISTINCT child.job_id FROM jobs child JOIN job_dependencies d ON d.job_id=child.job_id JOIN jobs parent ON parent.job_id=d.depends_on_job_id WHERE child.state IN ('queued','retry_wait') AND parent.state IN ('failed','canceled')")?;
    let ids: Vec<String> = s.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    drop(s);
    for id in ids {
        tx.execute("UPDATE jobs SET state='canceled',cancel_requested_at=COALESCE(cancel_requested_at,?1),cancel_reason='dependency_terminal',updated_at=?1,completed_at=?1 WHERE job_id=?2 AND state IN ('queued','retry_wait')",params![now,id])?;
        emit(
            tx,
            &id,
            None,
            "dependency_terminal",
            JobState::Canceled,
            &json!({"reason":"dependency_terminal"}),
            now,
        )?
    }
    Ok(())
}
fn ack_cancel(tx: &Transaction<'_>, job: &ClaimedJob, now: &str) -> Result<(), JobEngineError> {
    tx.execute("UPDATE jobs SET state='canceled',lease_owner=NULL,lease_token=NULL,lease_expires_at=NULL,updated_at=?1,completed_at=?1 WHERE job_id=?2 AND lease_token=?3",params![now,job.job.job_id.as_str(),job.lease_token])?;
    tx.execute("UPDATE job_attempts SET state='canceled',finished_at=?1 WHERE job_id=?2 AND attempt_number=?3 AND lease_token=?4",params![now,job.job.job_id.as_str(),job.attempt_number,job.lease_token])?;
    emit(
        tx,
        job.job.job_id.as_str(),
        Some(job.attempt_number),
        "canceled",
        JobState::Canceled,
        &json!({}),
        now,
    )
}

/// Acknowledge cancellation for a node-targeted job that has no lease.
fn ack_cancel_simple(tx: &Transaction<'_>, job_id: &str, now: &str) -> Result<(), JobEngineError> {
    tx.execute(
        "UPDATE jobs SET state='canceled', updated_at=?1, completed_at=?1 WHERE job_id=?2",
        params![now, job_id],
    )?;
    emit(
        tx,
        job_id,
        None,
        "canceled",
        JobState::Canceled,
        &json!({}),
        now,
    )
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimingCompatibility {
    Legacy,
    UnverifiedMedia,
    Compatible,
    Unknown,
}

enum RateObservation<'a> {
    Legacy(Option<&'a str>),
    Media(Option<&'a MediaEstimatorIdentity>),
}

/// Representation jobs use source-byte weights just like other media work, but
/// their old semantic recipe key is not an estimator identity. Only a
/// per-attempt identity observed by the actual handler can qualify their rate.
fn is_media_representation_job(definition: &JobDefinition) -> bool {
    definition.job_type == "representation.generate.v1"
        || definition.representation_id.is_some()
        || definition.recipe_key.starts_with("proxy.")
        || definition.recipe_key.starts_with("thumbnail.")
}

fn uncertain_estimate(mut estimate: BatchEstimate, reason: &str) -> BatchEstimate {
    estimate.work_units_per_worker_second = None;
    estimate.estimated_remaining_seconds = None;
    estimate.estimated_remaining_seconds_range = None;
    estimate.confidence = format!("uncertain_{reason}");
    estimate
}

fn annotate_measured_estimate(mut estimate: BatchEstimate, history: RateHistory) -> BatchEstimate {
    if estimate.estimated_remaining_seconds.is_some() {
        let eta = estimate.estimated_remaining_seconds.unwrap_or_default();
        match history.samples {
            0 => {
                return uncertain_estimate(estimate, "no_qualified_media_observations");
            }
            1 => {
                estimate.confidence = "low_single_qualified_observation".into();
                estimate.estimated_remaining_seconds_range =
                    Some([(eta * 0.5).max(0.0), (eta * 2.0).max(0.0)]);
            }
            2..=3 => {
                estimate.confidence = "measured_qualified_history_small_sample".into();
                estimate.estimated_remaining_seconds_range =
                    Some([(eta * 0.75).max(0.0), (eta * 1.5).max(0.0)]);
            }
            _ => {
                estimate.confidence = "measured_qualified_history".into();
            }
        }
    } else {
        estimate.confidence = "uncertain_no_qualified_media_observations".into();
    }
    estimate
}

fn qualified_rate_history(
    tx: &Transaction<'_>,
    node: &str,
    rate_key: &str,
    identity: &MediaEstimatorIdentity,
) -> Result<RateHistory, JobEngineError> {
    let identity_json = serde_json::to_string(identity)?;
    tx.query_row(
        "SELECT work_units_per_worker_second,samples FROM job_rate_history WHERE node_id=?1 AND recipe_key=?2 AND estimator_identity_json=?3",
        params![node, rate_key, identity_json],
        |row| {
            Ok(RateHistory {
                work_units_per_worker_second: row.get(0)?,
                samples: row.get::<_, i64>(1)? as u64,
            })
        },
    )
    .optional()
    .map(|history| history.unwrap_or_default())
    .map_err(Into::into)
}

fn qualified_rate_history_for_read(
    connection: &Connection,
    node: &str,
    rate_key: &str,
    identity: &MediaEstimatorIdentity,
) -> Result<RateHistory, JobEngineError> {
    let identity_json = serde_json::to_string(identity)?;
    connection
        .query_row(
            "SELECT work_units_per_worker_second,samples FROM job_rate_history WHERE node_id=?1 AND recipe_key=?2 AND estimator_identity_json=?3",
            params![node, rate_key, identity_json],
            |row| {
                Ok(RateHistory {
                    work_units_per_worker_second: row.get(0)?,
                    samples: row.get::<_, i64>(1)? as u64,
                })
            },
        )
        .optional()
        .map(|history| history.unwrap_or_default())
        .map_err(Into::into)
}

fn legacy_rate_history_for_read(
    connection: &Connection,
    node: &str,
    recipe: &str,
) -> Result<RateHistory, JobEngineError> {
    connection
        .query_row(
            "SELECT work_units_per_worker_second,samples FROM job_rate_history WHERE node_id=?1 AND recipe_key=?2",
            params![node, recipe],
            |row| {
                Ok(RateHistory {
                    work_units_per_worker_second: row.get(0)?,
                    samples: row.get::<_, i64>(1)? as u64,
                })
            },
        )
        .optional()
        .map(|history| history.unwrap_or_default())
        .map_err(Into::into)
}

fn persist_qualified_rate_history(
    tx: &Transaction<'_>,
    node_id: &str,
    rate_key: &str,
    identity: &MediaEstimatorIdentity,
    history: RateHistory,
    now: &str,
) -> Result<(), JobEngineError> {
    let identity_json = serde_json::to_string(identity)?;
    if identity_json.len() > 32_768 {
        return Err(JobEngineError::InvalidInput(
            "estimator identity exceeds the durable bound".into(),
        ));
    }
    tx.execute(
        "INSERT INTO job_rate_history(node_id,recipe_key,work_units_per_worker_second,samples,updated_at,semantic_recipe_key,representation_kind,estimator_identity_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(node_id,recipe_key) DO UPDATE SET work_units_per_worker_second=excluded.work_units_per_worker_second,samples=excluded.samples,updated_at=excluded.updated_at,semantic_recipe_key=excluded.semantic_recipe_key,representation_kind=excluded.representation_kind,estimator_identity_json=excluded.estimator_identity_json",
        params![
            node_id,
            rate_key,
            history.work_units_per_worker_second,
            history.samples as i64,
            now,
            identity.semantic_recipe_key,
            identity.representation_kind,
            identity_json
        ],
    )?;
    Ok(())
}

fn persist_attempt_estimator_identity(
    tx: &Transaction<'_>,
    job_id: &str,
    attempt_number: u32,
    identity: &MediaEstimatorIdentity,
    rate_key: &str,
) -> Result<(), JobEngineError> {
    let identity_json = serde_json::to_string(identity)?;
    if identity_json.len() > 32_768 || rate_key.len() > 1_024 {
        return Err(JobEngineError::InvalidInput(
            "estimator context exceeds the durable bound".into(),
        ));
    }
    let process_key = media_estimator_process_key();
    let updated = tx.execute(
        "UPDATE job_attempts SET estimator_identity_json=?1,estimator_rate_key=?2,estimator_process_key=?3 WHERE job_id=?4 AND attempt_number=?5 AND state='running'",
        params![identity_json, rate_key, process_key, job_id, attempt_number],
    )?;
    if updated != 1 {
        return Err(JobEngineError::InvalidInput(
            "estimator context can only be attached to the current running attempt".into(),
        ));
    }
    Ok(())
}

fn load_attempt_estimator_identity(
    connection: &Connection,
    job_id: &str,
    attempt_number: u32,
) -> Result<Option<MediaEstimatorIdentity>, JobEngineError> {
    let row: Option<(Option<String>, Option<String>)> = connection
        .query_row(
            "SELECT estimator_identity_json,estimator_process_key FROM job_attempts WHERE job_id=?1 AND attempt_number=?2 AND state='running'",
            params![job_id, attempt_number],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((identity_json, process_key)) = row else {
        return Ok(None);
    };
    if process_key.as_deref() != Some(media_estimator_process_key()) {
        // A running lease from another process/restart cannot supply a live
        // ETA even if its persisted identity is otherwise well formed.
        return Ok(None);
    }
    Ok(identity_json.and_then(|value| serde_json::from_str::<MediaEstimatorIdentity>(&value).ok()))
}

fn rate_history(
    tx: &Transaction<'_>,
    node: &str,
    recipe: &str,
) -> Result<RateHistory, JobEngineError> {
    tx.query_row("SELECT work_units_per_worker_second,samples FROM job_rate_history WHERE node_id=?1 AND recipe_key=?2",params![node,recipe],|r|Ok(RateHistory{work_units_per_worker_second:r.get(0)?,samples:r.get::<_,i64>(1)? as u64})).optional()?.ok_or_else(||JobEngineError::InvalidState("missing rate history is represented by default".into())).or_else(|e|if matches!(e,JobEngineError::InvalidState(_)){Ok(RateHistory::default())}else{Err(e)})
}

fn persist_rate_history(
    tx: &Transaction<'_>,
    node_id: &str,
    recipe_key: &str,
    history: RateHistory,
    now: &str,
) -> Result<(), JobEngineError> {
    tx.execute(
        "INSERT INTO job_rate_history(node_id,recipe_key,work_units_per_worker_second,samples,updated_at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(node_id,recipe_key) DO UPDATE SET work_units_per_worker_second=excluded.work_units_per_worker_second,samples=excluded.samples,updated_at=excluded.updated_at",
        params![node_id, recipe_key, history.work_units_per_worker_second, history.samples as i64, now],
    )?;
    Ok(())
}

fn validate_progress_estimate_input(
    class: JobClass,
    input: &ProgressEstimateInput,
    completed_items: u64,
    total_items: u64,
    completed_work_units: u64,
    total_work_units: u64,
) -> Result<(), JobEngineError> {
    let class_worker_cap = match class {
        JobClass::Proxy | JobClass::FullHash => 1,
        JobClass::Lightweight => 2,
    };
    if input.worker_count == 0 || input.worker_count > class_worker_cap {
        return Err(JobEngineError::InvalidInput(
            "workerCount exceeds the job class concurrency limit".into(),
        ));
    }
    let remaining_items = total_items.checked_sub(completed_items).ok_or_else(|| {
        JobEngineError::CorruptState("completed item count exceeds the declared total".into())
    })?;
    let remaining_work = total_work_units
        .checked_sub(completed_work_units)
        .ok_or_else(|| {
            JobEngineError::CorruptState("completed work exceeds the declared total".into())
        })?;
    let expected_task_count = usize::try_from(remaining_items).map_err(|_| {
        JobEngineError::InvalidInput("remaining item count cannot fit this platform".into())
    })?;
    if input.remaining_task_work_units.len() != expected_task_count {
        return Err(JobEngineError::InvalidInput(
            "remainingTaskWorkUnits must contain every remaining item".into(),
        ));
    }
    if remaining_items > 0 && input.worker_count > expected_task_count {
        return Err(JobEngineError::InvalidInput(
            "workerCount cannot exceed remaining item count".into(),
        ));
    }
    let supplied_work = input
        .remaining_task_work_units
        .iter()
        .try_fold(0_u64, |sum, work| sum.checked_add(*work))
        .ok_or_else(|| {
            JobEngineError::InvalidInput("remainingTaskWorkUnits overflow u64".into())
        })?;
    if supplied_work != remaining_work {
        return Err(JobEngineError::InvalidInput(
            "remainingTaskWorkUnits must sum to the durable remaining work".into(),
        ));
    }
    Ok(())
}
fn load_definition(tx: &Transaction<'_>, id: &str) -> Result<JobDefinition, JobEngineError> {
    let mut definition = tx.query_row("SELECT job_id,job_type,payload_version,payload_json,immutable_identity_json,requirements_json,target_node_id,project_id,media_object_id,storage_root_id,representation_id,recipe_key,job_class,idempotency_key,priority,total_work_units,total_items,max_attempts,retry_initial_seconds,retry_max_seconds FROM jobs WHERE job_id=?1",[id],|r|{let job_id=JobId::parse(r.get::<_,String>(0)?).map_err(|_|rusqlite::Error::InvalidQuery)?;let node=NodeId::parse(r.get::<_,String>(6)?).map_err(|_|rusqlite::Error::InvalidQuery)?;let project_id=r.get::<_,Option<String>>(7)?.map(ProjectId::parse).transpose().map_err(|_|rusqlite::Error::InvalidQuery)?;let media_object_id=r.get::<_,Option<String>>(8)?.map(MediaObjectId::parse).transpose().map_err(|_|rusqlite::Error::InvalidQuery)?;let storage_root_id=r.get::<_,Option<String>>(9)?.map(StorageRootId::parse).transpose().map_err(|_|rusqlite::Error::InvalidQuery)?;let representation_id=r.get::<_,Option<String>>(10)?.map(RepresentationId::parse).transpose().map_err(|_|rusqlite::Error::InvalidQuery)?;Ok(JobDefinition{job_id,job_type:r.get(1)?,payload_version:r.get(2)?,payload:serde_json::from_str(&r.get::<_,String>(3)?).map_err(|_|rusqlite::Error::InvalidQuery)?,immutable_identity:serde_json::from_str(&r.get::<_,String>(4)?).map_err(|_|rusqlite::Error::InvalidQuery)?,requirements:serde_json::from_str(&r.get::<_,String>(5)?).map_err(|_|rusqlite::Error::InvalidQuery)?,target_node_id:node,project_id,media_object_id,storage_root_id,representation_id,recipe_key:r.get(11)?,job_class:job_class(r.get(12)?).map_err(|_|rusqlite::Error::InvalidQuery)?,idempotency_key:r.get(13)?,priority:r.get(14)?,total_work_units:r.get::<_,i64>(15)? as u64,total_items:r.get::<_,i64>(16)? as u64,retry_policy:RetryPolicy{max_attempts:r.get(17)?,initial_backoff_seconds:r.get(18)?,max_backoff_seconds:r.get(19)?},dependencies:Vec::new()})})?;
    let mut statement = tx.prepare(
        "SELECT depends_on_job_id FROM job_dependencies WHERE job_id=?1 ORDER BY depends_on_job_id",
    )?;
    definition.dependencies = statement
        .query_map([id], |row| {
            JobId::parse(row.get::<_, String>(0)?).map_err(|_| rusqlite::Error::InvalidQuery)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(definition)
}
fn event_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<JobEvent> {
    let id = JobId::parse(r.get::<_, String>(1)?).map_err(|_| rusqlite::Error::InvalidQuery)?;
    let state = job_state(r.get(4)?).map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(JobEvent {
        event_sequence: r.get::<_, i64>(0)? as u64,
        job_id: id,
        attempt_number: r.get(2)?,
        event_type: r.get(3)?,
        state,
        details: serde_json::from_str(&r.get::<_, String>(5)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        created_at: r.get(6)?,
    })
}

/// Handler work receives only a typed durable job and cooperative cancellation.
/// Media-specific spawning/arguments belong to Tasks 5/6.
#[derive(Debug, Clone)]
pub struct JobHandlerFailure {
    pub kind: FailureKind,
    pub code: String,
    pub details: Value,
}
pub trait JobHandler: Send + Sync {
    fn run(
        &self,
        job: ClaimedJob,
        cancellation: CancellationToken,
    ) -> Result<JobResultCommit, JobHandlerFailure>;
    fn commit_result(
        &self,
        _tx: &Transaction<'_>,
        _job: &ClaimedJob,
        _result: &JobResultCommit,
    ) -> Result<(), JobEngineError> {
        Ok(())
    }
}
pub trait CancellableChild {
    fn request_graceful_stop(&mut self) -> std::io::Result<()>;
    fn has_exited(&mut self) -> std::io::Result<bool>;
    fn force_stop(&mut self) -> std::io::Result<()>;
}
pub async fn cancel_child_safely(
    child: &mut dyn CancellableChild,
    token: CancellationToken,
    grace: Duration,
) -> std::io::Result<()> {
    token.cancelled().await;
    child.request_graceful_stop()?;
    if child.has_exited()? {
        return Ok(());
    }
    tokio::time::sleep(grace).await;
    if child.has_exited()? {
        Ok(())
    } else {
        child.force_stop()
    }
}

#[derive(Debug, Clone)]
pub struct JobSchedulerConfig {
    /// Enables the proxy class when positive; zero disables it. Proxy and
    /// full-hash still share exactly one heavy slot when both are enabled.
    pub proxy_workers: usize,
    /// Enables the full-hash class when positive; zero disables it. Proxy and
    /// full-hash still share exactly one heavy slot when both are enabled.
    pub full_hash_workers: usize,
    pub lightweight_workers: usize,
    pub poll_interval: Duration,
    pub lease_duration: Duration,
    pub cancellation_poll_interval: Duration,
    /// Optional cross-process local-heavy lock used by the server adapter. The
    /// in-process scheduler already has one shared heavy permit.
    pub local_heavy_lock_path: Option<PathBuf>,
}
impl Default for JobSchedulerConfig {
    fn default() -> Self {
        Self {
            proxy_workers: 1,
            full_hash_workers: 1,
            lightweight_workers: 2,
            poll_interval: Duration::from_millis(200),
            lease_duration: Duration::from_secs(30),
            cancellation_poll_interval: Duration::from_millis(200),
            local_heavy_lock_path: None,
        }
    }
}
impl JobSchedulerConfig {
    fn heavy_workers(&self) -> usize {
        usize::from(self.proxy_workers > 0 || self.full_hash_workers > 0)
    }

    fn proxy_enabled(&self) -> bool {
        self.proxy_workers > 0
    }

    fn full_hash_enabled(&self) -> bool {
        self.full_hash_workers > 0
    }

    pub fn validate(&self) -> Result<(), JobEngineError> {
        if self.proxy_workers > 1
            || self.full_hash_workers > 1
            || self.lightweight_workers > 2
            || self.proxy_workers + self.full_hash_workers + self.lightweight_workers == 0
            || self.poll_interval.is_zero()
            || self.poll_interval > MAX_POLL_INTERVAL
            || self.cancellation_poll_interval.is_zero()
            || self.cancellation_poll_interval > MAX_CANCELLATION_POLL_INTERVAL
            || validate_lease_duration(self.lease_duration).is_err()
        {
            Err(JobEngineError::InvalidInput(
                "worker configuration exceeds safe server limits".into(),
            ))
        } else {
            Ok(())
        }
    }
}
fn validate_lease_duration(value: Duration) -> Result<(), JobEngineError> {
    if value < MIN_LEASE_DURATION || value > MAX_LEASE_DURATION {
        return Err(JobEngineError::InvalidInput(
            "lease duration must be 10ms through one hour".into(),
        ));
    }
    Ok(())
}
pub struct JobScheduler {
    engine: JobEngine,
    config: JobSchedulerConfig,
    shutdown: CancellationToken,
}
impl JobScheduler {
    pub fn new(
        engine: JobEngine,
        config: JobSchedulerConfig,
        shutdown: CancellationToken,
    ) -> Result<Self, JobEngineError> {
        config.validate()?;
        Ok(Self {
            engine,
            config,
            shutdown,
        })
    }
    pub async fn run<H: JobHandler + 'static>(
        &self,
        handler: Arc<H>,
    ) -> Result<(), JobEngineError> {
        // One shared local-heavy permit and a separately bounded lightweight
        // pool. A worker cannot be idle merely because one representation-kind
        // queue emptied: each permit claims the next eligible job across its
        // whole resource group using one shared durable queue.
        let heavy_limit = Arc::new(Semaphore::new(self.config.heavy_workers()));
        let lightweight_limit = Arc::new(Semaphore::new(self.config.lightweight_workers));
        let mut running = JoinSet::new();
        loop {
            self.engine.recover_expired()?;
            let mut claimed_any = false;
            for heavy in [true, false] {
                if self.shutdown.is_cancelled() {
                    break;
                }
                let (permit, job) = if heavy {
                    let Ok(permit) = heavy_limit.clone().try_acquire_owned() else {
                        continue;
                    };
                    let job = self.engine.claim_next_heavy_enabled(
                        "server-local-worker",
                        self.config.lease_duration,
                        self.config.proxy_enabled(),
                        self.config.full_hash_enabled(),
                    )?;
                    (permit, job)
                } else {
                    let Ok(permit) = lightweight_limit.clone().try_acquire_owned() else {
                        continue;
                    };
                    let job = self.engine.claim_next_lightweight(
                        "server-local-worker",
                        self.config.lease_duration,
                    )?;
                    (permit, job)
                };
                let Some(job) = job else {
                    drop(permit);
                    continue;
                };
                claimed_any = true;
                let engine = self.engine.clone();
                let handler = handler.clone();
                let shutdown = self.shutdown.clone();
                let cancellation = self.shutdown.child_token();
                let heartbeat_every = self.config.lease_duration / 3;
                let lease_duration = self.config.lease_duration;
                let cancellation_poll = self.config.cancellation_poll_interval;
                let local_heavy_lock_path = self.config.local_heavy_lock_path.clone();
                running.spawn(async move {
                    let _permit = permit;
                    run_claimed_job(
                        engine,
                        handler,
                        job,
                        ClaimedJobRuntime {
                            shutdown,
                            cancellation,
                            heartbeat_every,
                            lease_duration,
                            cancellation_poll,
                            local_heavy_lock_path,
                        },
                    )
                    .await
                });
            }
            while let Some(joined) = running.try_join_next() {
                let _ = joined; // A job error is persisted; it never kills the scheduler.
            }
            if self.shutdown.is_cancelled() {
                while let Some(joined) = running.join_next().await {
                    let _ = joined;
                }
                break;
            }
            if !claimed_any {
                tokio::select! {
                    _ = self.shutdown.cancelled() => {},
                    _ = tokio::time::sleep(self.config.poll_interval) => {},
                }
            }
        }
        Ok(())
    }
}

struct ClaimedJobRuntime {
    shutdown: CancellationToken,
    cancellation: CancellationToken,
    heartbeat_every: Duration,
    lease_duration: Duration,
    cancellation_poll: Duration,
    local_heavy_lock_path: Option<PathBuf>,
}

async fn run_claimed_job<H: JobHandler + 'static>(
    engine: JobEngine,
    handler: Arc<H>,
    job: ClaimedJob,
    runtime: ClaimedJobRuntime,
) -> Result<(), JobEngineError> {
    let lease_lost = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_cancellation = runtime.cancellation.clone();
    let lock_cancellation = runtime.cancellation.clone();
    let worker_job = job.clone();
    let commit_handler = handler.clone();
    let heavy = job.job.job_class.is_local_heavy();
    let local_heavy_lock_path = runtime.local_heavy_lock_path.clone();
    let blocking = tokio::task::spawn_blocking(move || {
        // Cross-process coordination is acquired only for the one heavy class.
        // Lightweight/cloud jobs never touch this slot. The advisory host-user
        // lock supplements, rather than replaces, the durable in-process permit.
        let _heavy_lease = if heavy {
            match local_heavy_lock_path.as_deref() {
                Some(path) => match rcdvlog_core::acquire_local_heavy_at_cancellable(path, || {
                    lock_cancellation.is_cancelled()
                }) {
                    Ok(lease) => {
                        let _ = lease.scope();
                        Some(lease)
                    }
                    Err(error) => {
                        return Err(JobHandlerFailure {
                            kind: FailureKind::Retryable,
                            code: "local_heavy_lock_failed".into(),
                            details: json!({"error": format!("{error:#}")}),
                        });
                    }
                },
                None => None,
            }
        } else {
            None
        };
        handler.run(worker_job, worker_cancellation)
    });
    tokio::pin!(blocking);
    let mut heartbeat = tokio::time::interval(
        runtime
            .heartbeat_every
            .min(Duration::from_secs(30))
            .max(Duration::from_millis(10)),
    );
    let mut cancellation_check = tokio::time::interval(runtime.cancellation_poll);
    loop {
        tokio::select! {
            result = &mut blocking => {
                match result.map_err(|error| JobEngineError::Infrastructure(error.into()))? {
                    Ok(result) => return engine.complete(&job, result.clone(), |tx| commit_handler.commit_result(tx, &job, &result)),
                    Err(failure) => {
                        if engine.cancellation_requested(&job).unwrap_or(false) {
                            return engine.complete(&job, JobResultCommit { output_identity: None, details: json!({"outcome":"canceled"}) }, |_| Ok(()));
                        }
                        if lease_lost.load(std::sync::atomic::Ordering::SeqCst) {
                            // A stale worker must never overwrite recovery's decision.
                            return Ok(());
                        }
                        if runtime.shutdown.is_cancelled() {
                            let _ = engine.fail(&job, FailureKind::Retryable, "server_shutdown", json!({"code":"server_shutdown"}));
                            return Ok(());
                        }
                        let _ = engine.fail(&job, failure.kind, &failure.code, failure.details);
                        return Ok(());
                    }
                }
            }
            _ = heartbeat.tick() => {
                if engine.heartbeat(&job, runtime.lease_duration).is_err() {
                    lease_lost.store(true, std::sync::atomic::Ordering::SeqCst);
                    runtime.cancellation.cancel();
                }
            }
            _ = cancellation_check.tick() => {
                if engine.cancellation_requested(&job).unwrap_or(false) {
                    runtime.cancellation.cancel();
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "jobs_tests.rs"]
mod tests;
