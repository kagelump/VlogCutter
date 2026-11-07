use anyhow::Result;
use chrono::{SecondsFormat, Utc};
use rcdvlog_core::{
    Catalog, ConflictProjection, OperationBatchResult, PROJECT_CATALOG_SCHEMA_VERSION,
    ProjectBaselineSnapshot, ProjectOperation, SequenceRecoveryProjection, SequenceRecoveryState,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub struct SyncProcessGuard {
    _file: File,
}

impl SyncProcessGuard {
    pub fn try_acquire(catalog_path: &Path) -> Result<Option<Self>> {
        use fs2::FileExt;
        let lock_path = sync_control_root(catalog_path).join("sync-manager.lock");
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

/// Result of asking a [`SyncGuardTracker`] to cover a catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardState {
    /// This tracker holds the sync guard for the requested catalog.
    Held,
    /// Another actor holds the requested catalog's guard; do not sync it.
    HeldElsewhere,
}

/// Holds the sync guard for exactly one catalog at a time and follows the
/// catalog the caller currently intends to sync. Moving to another catalog
/// releases the previous guard first, so a swapped-away catalog is never
/// squatted on and a catalog is never synced without its own guard.
#[derive(Default)]
pub struct SyncGuardTracker {
    held: Option<(PathBuf, SyncProcessGuard)>,
}

impl SyncGuardTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Path whose guard is currently held, if any.
    pub fn guarded_path(&self) -> Option<&Path> {
        self.held.as_ref().map(|(path, _)| path.as_path())
    }

    /// Ensure the guard held is the one for `catalog_path`.
    pub fn ensure(&mut self, catalog_path: &Path) -> Result<GuardState> {
        if self.guarded_path() == Some(catalog_path) {
            return Ok(GuardState::Held);
        }
        // Release the old catalog's guard before touching the new one.
        self.held = None;
        match SyncProcessGuard::try_acquire(catalog_path)? {
            Some(guard) => {
                self.held = Some((catalog_path.to_path_buf(), guard));
                Ok(GuardState::Held)
            }
            None => Ok(GuardState::HeldElsewhere),
        }
    }

    /// Release any held guard.
    pub fn release(&mut self) {
        self.held = None;
    }
}

/// Outcome of applying a reconcile response to the catalog the cycle started with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinnedApplyOutcome {
    Applied,
    /// The open catalog is no longer the one the cycle was built from; nothing was written.
    CatalogChanged,
}

/// Apply a reconcile response to `pinned`, the catalog the request was built
/// from, only if `current_path` (the catalog open now) is still that catalog.
/// Acknowledgements are catalog-global and every desktop catalog shares one
/// node id, so a response must never be applied to a different catalog.
pub fn apply_pinned_reconcile_response(
    pinned: &Catalog,
    current_path: &Path,
    server_node_id: &str,
    response: &ReconcileResponse,
) -> Result<PinnedApplyOutcome> {
    if pinned.database_path() != current_path {
        return Ok(PinnedApplyOutcome::CatalogChanged);
    }
    if !response.operations.is_empty() {
        apply_reconcile_operations(pinned, &response.operations)?;
    }
    pinned.record_peer_operation_acknowledgements(server_node_id, &response.committed_cursors)?;
    Ok(PinnedApplyOutcome::Applied)
}

pub fn signal_running_sync_manager(catalog_path: &Path, project_id: &str) -> Result<()> {
    anyhow::ensure!(
        !project_id.is_empty()
            && project_id.len() <= 255
            && project_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
        "project ID is invalid"
    );
    let directory = sync_control_root(catalog_path).join("wake");
    fs::create_dir_all(&directory)?;
    let identity = blake3::hash(project_id.as_bytes()).to_hex();
    let target = directory.join(format!("{}.request", &identity[..32]));
    let temporary = directory.join(format!(".{}.tmp", &identity[..32]));
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(project_id.as_bytes())?;
    file.sync_all()?;
    fs::rename(temporary, target)?;
    Ok(())
}

pub fn drain_sync_manager_requests(catalog_path: &Path) -> Result<Vec<String>> {
    let directory = sync_control_root(catalog_path).join("wake");
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let mut requests = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file()
            || entry.path().extension().and_then(|value| value.to_str()) != Some("request")
        {
            continue;
        }
        let value = fs::read_to_string(entry.path())?;
        if !value.is_empty() && value.len() <= 255 {
            requests.push(value);
        }
        fs::remove_file(entry.path())?;
    }
    requests.sort();
    requests.dedup();
    Ok(requests)
}

fn sync_control_root(catalog_path: &Path) -> PathBuf {
    catalog_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("cache")
        .join("sync-control")
}

pub const SYNC_PROTOCOL_MAJOR: u32 = 1;
pub const SYNC_PROTOCOL_MINOR: u32 = 0;
pub const BASELINE_VERSION: u32 = 1;
pub const DEFAULT_MAX_OPERATIONS: usize = 500;
pub const DEFAULT_MAX_FACTS_PER_CLASS: usize = 500;
pub const DEFAULT_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Return the next exact, contiguous page of server-origin operations that a
/// peer can safely replay after the server restores an older backup.
///
/// Missing or non-contiguous local history deliberately returns `None`: the
/// caller must surface the unresolved recovery instead of advancing the
/// allocator based only on a claimed cursor.
pub fn exact_sequence_recovery_page(
    response: &ReconcileResponse,
    server_node_id: &str,
    retained_operations: &[ProjectOperation],
) -> Option<Vec<ProjectOperation>> {
    let recovery = response.sequence_recovery.iter().find(|recovery| {
        recovery.server_origin_node_id == server_node_id
            && recovery.state == SequenceRecoveryState::Divergent
    })?;
    let start = recovery.divergence_start?;
    let end = recovery.divergence_end?;
    if start > end {
        return None;
    }
    let page_end = end.min(start.saturating_add(DEFAULT_MAX_OPERATIONS as u64 - 1));
    let mut operations = retained_operations
        .iter()
        .filter(|operation| {
            operation.origin_node_id == server_node_id
                && operation.origin_sequence >= start
                && operation.origin_sequence <= page_end
        })
        .cloned()
        .collect::<Vec<_>>();
    operations.sort_by_key(|operation| operation.origin_sequence);
    let expected_len = usize::try_from(page_end - start + 1).ok()?;
    if operations.len() != expected_len
        || operations
            .iter()
            .enumerate()
            .any(|(index, operation)| operation.origin_sequence != start + index as u64)
    {
        return None;
    }
    Some(operations)
}

pub fn sequence_recovery_requires_attention(response: &ReconcileResponse) -> bool {
    response
        .sequence_recovery
        .iter()
        .any(|recovery| recovery.state != SequenceRecoveryState::Completed)
}

/// Apply a reconciliation response one origin at a time. A response can
/// legitimately include the caller's own operations as idempotent echoes as
/// well as operations created by the peer.
pub fn apply_reconcile_operations(
    catalog: &Catalog,
    operations: &[ProjectOperation],
) -> Result<Vec<OperationBatchResult>> {
    let mut grouped = BTreeMap::<String, Vec<ProjectOperation>>::new();
    for operation in operations {
        grouped
            .entry(operation.origin_node_id.clone())
            .or_default()
            .push(operation.clone());
    }
    grouped
        .into_iter()
        .map(|(origin, mut operations)| {
            operations.sort_by_key(|operation| operation.origin_sequence);
            catalog.apply_operation_batch(&origin, &operations)
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolRange {
    pub minimum_major: u32,
    pub minimum_minor: u32,
    pub maximum_major: u32,
    pub maximum_minor: u32,
}

impl Default for ProtocolRange {
    fn default() -> Self {
        Self {
            minimum_major: SYNC_PROTOCOL_MAJOR,
            minimum_minor: 0,
            maximum_major: SYNC_PROTOCOL_MAJOR,
            maximum_minor: SYNC_PROTOCOL_MINOR,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyncError {
    #[error("protocol versions are incompatible")]
    ProtocolIncompatible,
    #[error("project schema {found} is incompatible with required schema {required}")]
    SchemaIncompatible { found: u32, required: u32 },
    #[error("baseline version {found} is incompatible with required version {required}")]
    BaselineVersionIncompatible { found: u32, required: u32 },
    #[error("project ID mismatch: expected {expected}, received {received}")]
    ProjectMismatch { expected: String, received: String },
    #[error("server ID mismatch: expected {expected}, received {received}")]
    ServerMismatch { expected: String, received: String },
    #[error("sync page exceeds the configured {limit_kind} limit")]
    PageTooLarge { limit_kind: &'static str },
    #[error("operation sequence gap: {detail}")]
    SequenceGap { detail: String },
    #[error("baseline digest does not match its canonical payload")]
    BaselineDigestMismatch,
}

impl SyncError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ProtocolIncompatible => "sync_protocol_incompatible",
            Self::SchemaIncompatible { .. } => "sync_schema_incompatible",
            Self::BaselineVersionIncompatible { .. } => "baseline_version_incompatible",
            Self::ProjectMismatch { .. } => "sync_project_mismatch",
            Self::ServerMismatch { .. } => "sync_server_mismatch",
            Self::PageTooLarge { .. } => "sync_page_too_large",
            Self::SequenceGap { .. } => "sync_sequence_gap",
            Self::BaselineDigestMismatch => "baseline_digest_mismatch",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObservationFact {
    pub origin_node_id: String,
    pub origin_sequence: u64,
    pub scope_key: String,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepresentationManifestFact {
    pub manifest_id: String,
    pub origin_node_id: String,
    pub origin_sequence: u64,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BaselinePayload {
    pub baseline_version: u32,
    pub protocol_version: ProtocolRange,
    pub project_schema_version: u32,
    pub project: ProjectBaselineSnapshot,
    pub observations: Vec<ObservationFact>,
    pub representation_manifests: Vec<RepresentationManifestFact>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BaselineEnvelope {
    pub digest: String,
    pub payload: BaselinePayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselinePreview {
    pub project_id: String,
    pub project_name: String,
    pub asset_count: usize,
    pub operation_count: usize,
    pub conflict_count: usize,
    pub linked_media_count: usize,
    pub proposed_library_matches: usize,
    pub root_mapping_count: usize,
    pub representation_manifest_count: usize,
}

impl BaselineEnvelope {
    pub fn export_catalog(catalog: &Catalog, project_id: &str) -> Result<Self> {
        Self::export(
            catalog,
            project_id,
            observations(catalog)?,
            representation_manifests(catalog)?,
        )
    }

    pub fn export(
        catalog: &Catalog,
        project_id: &str,
        observations: Vec<ObservationFact>,
        representation_manifests: Vec<RepresentationManifestFact>,
    ) -> Result<Self> {
        let payload = BaselinePayload {
            baseline_version: BASELINE_VERSION,
            protocol_version: ProtocolRange::default(),
            project_schema_version: PROJECT_CATALOG_SCHEMA_VERSION,
            project: catalog.export_project_baseline(project_id)?,
            observations,
            representation_manifests,
        };
        let digest = canonical_digest(&payload)?;
        Ok(Self { digest, payload })
    }

    pub fn validate(&self) -> Result<(), SyncError> {
        negotiate(self.payload.protocol_version)?;
        if self.payload.baseline_version != BASELINE_VERSION {
            return Err(SyncError::BaselineVersionIncompatible {
                found: self.payload.baseline_version,
                required: BASELINE_VERSION,
            });
        }
        if self.payload.project_schema_version != PROJECT_CATALOG_SCHEMA_VERSION {
            return Err(SyncError::SchemaIncompatible {
                found: self.payload.project_schema_version,
                required: PROJECT_CATALOG_SCHEMA_VERSION,
            });
        }
        if canonical_digest(&self.payload).map_err(|_| SyncError::BaselineDigestMismatch)?
            != self.digest
        {
            return Err(SyncError::BaselineDigestMismatch);
        }
        validate_fact_sequences(
            &self
                .payload
                .observations
                .iter()
                .map(|fact| (&fact.origin_node_id, fact.origin_sequence))
                .collect::<Vec<_>>(),
        )?;
        validate_fact_sequences(
            &self
                .payload
                .representation_manifests
                .iter()
                .map(|fact| (&fact.origin_node_id, fact.origin_sequence))
                .collect::<Vec<_>>(),
        )?;
        for manifest in &self.payload.representation_manifests {
            validate_manifest_payload(&manifest.payload)?;
        }
        Ok(())
    }

    pub fn preview(&self) -> BaselinePreview {
        BaselinePreview {
            project_id: self.payload.project.project_id.clone(),
            project_name: self.payload.project.name.clone(),
            asset_count: self.payload.project.assets.len(),
            operation_count: self.payload.project.operations.len(),
            conflict_count: self.payload.project.conflicts.len(),
            linked_media_count: self
                .payload
                .project
                .assets
                .iter()
                .filter(|asset| asset.media_object_id.is_some())
                .count(),
            proposed_library_matches: 0,
            root_mapping_count: self.payload.project.root_mappings.len(),
            representation_manifest_count: self.payload.representation_manifests.len(),
        }
    }
}

pub fn canonical_digest<T: Serialize>(value: &T) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}

pub fn negotiate(range: ProtocolRange) -> Result<(u32, u32), SyncError> {
    if range.minimum_major > SYNC_PROTOCOL_MAJOR
        || range.maximum_major < SYNC_PROTOCOL_MAJOR
        || (range.minimum_major == SYNC_PROTOCOL_MAJOR && range.minimum_minor > SYNC_PROTOCOL_MINOR)
    {
        return Err(SyncError::ProtocolIncompatible);
    }
    Ok((SYNC_PROTOCOL_MAJOR, SYNC_PROTOCOL_MINOR))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReconcileRequest {
    pub protocol_version: ProtocolRange,
    pub project_schema_version: u32,
    pub project_id: String,
    pub server_node_id: String,
    pub operation_cursors: BTreeMap<String, u64>,
    pub operations: Vec<ProjectOperation>,
    pub observations: Vec<ObservationFact>,
    pub representation_manifests: Vec<RepresentationManifestFact>,
    #[serde(default = "default_max_operations")]
    pub maximum_response_operations: usize,
}

fn default_max_operations() -> usize {
    DEFAULT_MAX_OPERATIONS
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeTargetedJob {
    pub job_id: String,
    pub job_type: String,
    pub media_object_id: String,
    pub project_id: Option<String>,
    pub representation_id: Option<String>,
    /// Representation kind: "proxy" or "thumbnail".
    pub kind: String,
    /// Kind-specific parameters (e.g. quality for thumbnails).
    #[serde(default)]
    pub payload: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileResponse {
    pub protocol_major: u32,
    pub protocol_minor: u32,
    pub project_id: String,
    pub committed_cursors: BTreeMap<String, u64>,
    pub operations: Vec<ProjectOperation>,
    pub observation_cursors: BTreeMap<String, u64>,
    pub representation_cursors: BTreeMap<String, u64>,
    pub conflicts: Vec<ConflictProjection>,
    pub has_more_operations: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sequence_recovery: Vec<SequenceRecoveryProjection>,
    /// Jobs that the server has enqueued for the requesting node (e.g. Mac-targeted
    /// proxy/thumbnail generation). These are surfaced during reconciliation so the
    /// desktop can execute them locally and report results back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub node_targeted_jobs: Vec<NodeTargetedJob>,
}

pub fn reconcile(
    catalog: &Catalog,
    expected_project_id: &str,
    expected_server_node_id: &str,
    request: &ReconcileRequest,
) -> Result<ReconcileResponse> {
    let (major, minor) = negotiate(request.protocol_version)?;
    if request.project_schema_version != PROJECT_CATALOG_SCHEMA_VERSION {
        return Err(SyncError::SchemaIncompatible {
            found: request.project_schema_version,
            required: PROJECT_CATALOG_SCHEMA_VERSION,
        }
        .into());
    }
    if request.project_id != expected_project_id {
        return Err(SyncError::ProjectMismatch {
            expected: expected_project_id.to_string(),
            received: request.project_id.clone(),
        }
        .into());
    }
    if request.server_node_id != expected_server_node_id {
        return Err(SyncError::ServerMismatch {
            expected: expected_server_node_id.to_string(),
            received: request.server_node_id.clone(),
        }
        .into());
    }
    if request.operations.len() > DEFAULT_MAX_OPERATIONS
        || request.maximum_response_operations > DEFAULT_MAX_OPERATIONS
    {
        return Err(SyncError::PageTooLarge {
            limit_kind: "operation_count",
        }
        .into());
    }
    if request.observations.len() > DEFAULT_MAX_FACTS_PER_CLASS
        || request.representation_manifests.len() > DEFAULT_MAX_FACTS_PER_CLASS
    {
        return Err(SyncError::PageTooLarge {
            limit_kind: "fact_count",
        }
        .into());
    }
    if serde_json::to_vec(request)?.len() > DEFAULT_MAX_BYTES {
        return Err(SyncError::PageTooLarge { limit_kind: "byte" }.into());
    }

    let mut by_origin = BTreeMap::<String, Vec<ProjectOperation>>::new();
    for operation in &request.operations {
        if operation.project_id != expected_project_id {
            return Err(SyncError::ProjectMismatch {
                expected: expected_project_id.to_string(),
                received: operation.project_id.clone(),
            }
            .into());
        }
        by_origin
            .entry(operation.origin_node_id.clone())
            .or_default()
            .push(operation.clone());
    }
    for (origin, operations) in &mut by_origin {
        operations.sort_by_key(|operation| operation.origin_sequence);
        if operations
            .windows(2)
            .any(|pair| pair[1].origin_sequence != pair[0].origin_sequence + 1)
        {
            return Err(SyncError::SequenceGap {
                detail: format!("non-contiguous page for {origin}"),
            }
            .into());
        }
        catalog
            .apply_operation_batch(origin, operations)
            .map_err(|error| SyncError::SequenceGap {
                detail: format!("{error:#}"),
            })?;
    }
    let (observation_cursors, representation_cursors) = persist_facts(
        catalog,
        &request.observations,
        &request.representation_manifests,
    )?;
    let committed_cursors = catalog.operation_cursors()?;
    let all = catalog.list_operations(expected_project_id)?;
    let missing = all
        .into_iter()
        .filter(|operation| {
            operation.origin_sequence
                > request
                    .operation_cursors
                    .get(&operation.origin_node_id)
                    .copied()
                    .unwrap_or(0)
        })
        .collect::<Vec<_>>();
    let response_limit = request.maximum_response_operations.max(1);
    let has_more_operations = missing.len() > response_limit;
    let operations = missing.into_iter().take(response_limit).collect();
    let conflicts = catalog
        .operation_diagnostics(expected_project_id)?
        .unresolved_conflicts;
    let mut response = ReconcileResponse {
        protocol_major: major,
        protocol_minor: minor,
        project_id: expected_project_id.to_string(),
        committed_cursors,
        operations,
        observation_cursors,
        representation_cursors,
        conflicts,
        has_more_operations,
        sequence_recovery: vec![],
        node_targeted_jobs: vec![],
    };
    while serde_json::to_vec(&response)?.len() > DEFAULT_MAX_BYTES {
        if response.operations.len() <= 1 {
            return Err(SyncError::PageTooLarge {
                limit_kind: "response_byte",
            }
            .into());
        }
        response.operations.pop();
        response.has_more_operations = true;
    }
    Ok(response)
}

pub fn persist_baseline_facts(
    catalog: &Catalog,
    observations: &[ObservationFact],
    manifests: &[RepresentationManifestFact],
) -> Result<(BTreeMap<String, u64>, BTreeMap<String, u64>)> {
    persist_facts(catalog, observations, manifests)
}

fn persist_facts(
    catalog: &Catalog,
    observations: &[ObservationFact],
    manifests: &[RepresentationManifestFact],
) -> Result<(BTreeMap<String, u64>, BTreeMap<String, u64>)> {
    let mut connection = Connection::open(catalog.database_path())?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    let transaction = connection.transaction()?;
    let mut observation_groups = BTreeMap::<&str, Vec<&ObservationFact>>::new();
    for fact in observations {
        observation_groups
            .entry(&fact.origin_node_id)
            .or_default()
            .push(fact);
    }
    for (origin, facts) in observation_groups {
        let mut cursor = stored_fact_cursor(&transaction, "sync_observation_facts", origin)?;
        for fact in facts {
            let payload = serde_json::to_string(&fact.payload)?;
            if fact.origin_sequence <= cursor {
                let stored = transaction.query_row(
                    "SELECT scope_key, payload_json FROM sync_observation_facts WHERE origin_node_id=?1 AND origin_sequence=?2",
                    params![origin, fact.origin_sequence as i64],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                ).optional()?;
                if stored.as_ref() != Some(&(fact.scope_key.clone(), payload)) {
                    return Err(SyncError::SequenceGap {
                        detail: format!(
                            "observation {origin}/{} was acknowledged with different content",
                            fact.origin_sequence
                        ),
                    }
                    .into());
                }
                continue;
            }
            if fact.origin_sequence != cursor + 1 {
                return Err(SyncError::SequenceGap {
                    detail: format!(
                        "observation gap for {origin}: expected {}, received {}",
                        cursor + 1,
                        fact.origin_sequence
                    ),
                }
                .into());
            }
            transaction.execute(
                "INSERT INTO sync_observation_facts(origin_node_id, origin_sequence, scope_key, payload_json, recorded_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![origin, fact.origin_sequence as i64, fact.scope_key, payload, now()],
            )?;
            cursor = fact.origin_sequence;
        }
    }
    let mut manifest_groups = BTreeMap::<&str, Vec<&RepresentationManifestFact>>::new();
    for manifest in manifests {
        validate_manifest_payload(&manifest.payload)?;
        manifest_groups
            .entry(&manifest.origin_node_id)
            .or_default()
            .push(manifest);
    }
    for (origin, manifests) in manifest_groups {
        let mut cursor = stored_fact_cursor(&transaction, "sync_representation_manifests", origin)?;
        for manifest in manifests {
            let payload = serde_json::to_string(&manifest.payload)?;
            if manifest.origin_sequence <= cursor {
                let stored = transaction.query_row(
                    "SELECT manifest_id, payload_json FROM sync_representation_manifests WHERE origin_node_id=?1 AND origin_sequence=?2",
                    params![origin, manifest.origin_sequence as i64],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                ).optional()?;
                if stored.as_ref() != Some(&(manifest.manifest_id.clone(), payload)) {
                    return Err(SyncError::SequenceGap { detail: format!("representation manifest {origin}/{} was acknowledged with different content", manifest.origin_sequence) }.into());
                }
                continue;
            }
            if manifest.origin_sequence != cursor + 1 {
                return Err(SyncError::SequenceGap {
                    detail: format!(
                        "representation manifest gap for {origin}: expected {}, received {}",
                        cursor + 1,
                        manifest.origin_sequence
                    ),
                }
                .into());
            }
            transaction.execute(
                "INSERT INTO sync_representation_manifests(manifest_id, origin_node_id, origin_sequence, payload_json, recorded_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![manifest.manifest_id, origin, manifest.origin_sequence as i64, payload, now()],
            )?;
            cursor = manifest.origin_sequence;
        }
    }
    transaction.commit()?;
    let connection = Connection::open(catalog.database_path())?;
    Ok((
        fact_cursors(&connection, "sync_observation_facts")?,
        fact_cursors(&connection, "sync_representation_manifests")?,
    ))
}

fn fact_cursors(connection: &Connection, table: &str) -> Result<BTreeMap<String, u64>> {
    let sql = format!(
        "SELECT origin_node_id, MAX(origin_sequence) FROM {table} GROUP BY origin_node_id ORDER BY origin_node_id"
    );
    let mut statement = connection.prepare(&sql)?;
    Ok(statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn observations(catalog: &Catalog) -> Result<Vec<ObservationFact>> {
    let connection = Connection::open(catalog.database_path())?;
    let mut statement = connection.prepare(
        "SELECT origin_node_id, origin_sequence, scope_key, payload_json FROM sync_observation_facts ORDER BY origin_node_id, origin_sequence",
    )?;
    Ok(statement
        .query_map([], |row| {
            let payload: String = row.get(3)?;
            Ok(ObservationFact {
                origin_node_id: row.get(0)?,
                origin_sequence: row.get::<_, i64>(1)? as u64,
                scope_key: row.get(2)?,
                payload: serde_json::from_str(&payload).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        payload.len(),
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn latest_observations(catalog: &Catalog) -> Result<Vec<ObservationFact>> {
    let all = observations(catalog)?;
    let mut latest = BTreeMap::<(String, String), ObservationFact>::new();
    for fact in all {
        latest.insert((fact.origin_node_id.clone(), fact.scope_key.clone()), fact);
    }
    Ok(latest.into_values().collect())
}

pub fn representation_manifests(catalog: &Catalog) -> Result<Vec<RepresentationManifestFact>> {
    let connection = Connection::open(catalog.database_path())?;
    let mut statement = connection.prepare(
        "SELECT manifest_id, origin_node_id, origin_sequence, payload_json FROM sync_representation_manifests ORDER BY origin_node_id, origin_sequence",
    )?;
    Ok(statement
        .query_map([], |row| {
            let payload: String = row.get(3)?;
            Ok(RepresentationManifestFact {
                manifest_id: row.get(0)?,
                origin_node_id: row.get(1)?,
                origin_sequence: row.get::<_, i64>(2)? as u64,
                payload: serde_json::from_str(&payload).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        payload.len(),
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn stored_fact_cursor(
    transaction: &rusqlite::Transaction<'_>,
    table: &str,
    origin: &str,
) -> Result<u64> {
    let sql =
        format!("SELECT COALESCE(MAX(origin_sequence), 0) FROM {table} WHERE origin_node_id=?1");
    Ok(transaction.query_row(&sql, [origin], |row| row.get::<_, i64>(0))? as u64)
}

fn validate_fact_sequences(facts: &[(&String, u64)]) -> Result<(), SyncError> {
    let mut seen = BTreeSet::new();
    let mut cursors = BTreeMap::<&str, u64>::new();
    for (origin, sequence) in facts {
        if *sequence == 0 || !seen.insert(((*origin).clone(), *sequence)) {
            return Err(SyncError::SequenceGap {
                detail: "duplicate or zero fact sequence".into(),
            });
        }
        let expected = cursors.get(origin.as_str()).copied().unwrap_or(0) + 1;
        if *sequence != expected {
            return Err(SyncError::SequenceGap {
                detail: format!("fact gap for {origin}: expected {expected}, received {sequence}"),
            });
        }
        cursors.insert(origin, *sequence);
    }
    Ok(())
}

fn validate_manifest_payload(payload: &Value) -> Result<(), SyncError> {
    fn contains_job_progress(value: &Value) -> bool {
        match value {
            Value::Object(object) => object.iter().any(|(key, value)| {
                matches!(
                    key.as_str(),
                    "progress"
                        | "jobProgress"
                        | "jobState"
                        | "completedItems"
                        | "totalItems"
                        | "estimatedSecondsRemaining"
                ) || contains_job_progress(value)
            }),
            Value::Array(values) => values.iter().any(contains_job_progress),
            _ => false,
        }
    }
    if contains_job_progress(payload) {
        return Err(SyncError::SequenceGap {
            detail: "representation manifests cannot contain job progress state".into(),
        });
    }
    Ok(())
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn stored_baseline_digest(path: &std::path::Path) -> Result<Option<String>> {
    let connection = Connection::open(path)?;
    Ok(connection
        .query_row(
            "SELECT baseline_digest FROM project_baseline_metadata WHERE singleton_id=1",
            [],
            |row| row.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests;
