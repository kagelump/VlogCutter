use anyhow::Result;
use chrono::{SecondsFormat, Utc};
use rcdvlog_core::{
    Catalog, ConflictProjection, PROJECT_CATALOG_SCHEMA_VERSION, ProjectBaselineSnapshot,
    ProjectOperation, SequenceRecoveryProjection,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const SYNC_PROTOCOL_MAJOR: u32 = 1;
pub const SYNC_PROTOCOL_MINOR: u32 = 0;
pub const BASELINE_VERSION: u32 = 1;
pub const DEFAULT_MAX_OPERATIONS: usize = 500;
pub const DEFAULT_MAX_FACTS_PER_CLASS: usize = 500;
pub const DEFAULT_MAX_BYTES: usize = 4 * 1024 * 1024;

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
mod tests {
    use super::*;
    use rcdvlog_core::{MediaVerificationState, ProjectAssetAddedPayload};
    use serde_json::json;
    use tempfile::TempDir;

    fn new_catalog(root: &TempDir, filename: &str, node: &str) -> Catalog {
        let catalog = Catalog::open(root.path().join(filename)).unwrap();
        catalog.set_local_node_id(node).unwrap();
        catalog
    }

    fn asset(id: &str, filename: &str) -> ProjectAssetAddedPayload {
        ProjectAssetAddedPayload {
            id: id.to_string(),
            fingerprint: format!("fingerprint-{id}"),
            filename: filename.to_string(),
            size_bytes: 123,
            duration_seconds: 2.5,
            frame_rate: 24.0,
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

    #[test]
    fn selected_project_baseline_is_path_free_and_imports_equivalently() {
        let root = TempDir::new().unwrap();
        let source = new_catalog(&root, "source.sqlite3", "node_mac");
        let selected = source.create_project("Selected").unwrap();
        let other = source.create_project("Other").unwrap();
        source
            .add_portable_project_asset(&selected.id, asset("asset_selected", "selected.mov"))
            .unwrap();
        source
            .add_portable_project_asset(&other.id, asset("asset_other", "other.mov"))
            .unwrap();

        let envelope = BaselineEnvelope::export(&source, &selected.id, vec![], vec![]).unwrap();
        envelope.validate().unwrap();
        assert_eq!(envelope.payload.project.project_id, selected.id);
        assert_eq!(envelope.payload.project.assets.len(), 1);
        assert_eq!(envelope.payload.project.assets[0].id, "asset_selected");
        assert!(
            !serde_json::to_string(&envelope)
                .unwrap()
                .contains(root.path().to_str().unwrap())
        );

        let imported = Catalog::import_project_baseline(
            root.path().join("imported.sqlite3"),
            &envelope.payload.project,
            "node_server",
            BASELINE_VERSION,
            &envelope.digest,
        )
        .unwrap();
        let projects = imported.list_projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, selected.id);
        assert_eq!(imported.list_assets(&selected.id).unwrap().len(), 1);
        assert_eq!(
            imported.list_operations(&selected.id).unwrap(),
            envelope.payload.project.operations
        );
    }

    #[test]
    fn reconciliation_converges_and_retries_are_idempotent() {
        let root = TempDir::new().unwrap();
        let mac = new_catalog(&root, "mac.sqlite3", "node_mac");
        let project = mac.create_project("Trip").unwrap();
        let baseline = BaselineEnvelope::export(&mac, &project.id, vec![], vec![]).unwrap();
        let server = Catalog::import_project_baseline(
            root.path().join("server.sqlite3"),
            &baseline.payload.project,
            "node_server",
            BASELINE_VERSION,
            &baseline.digest,
        )
        .unwrap();

        mac.add_portable_project_asset(&project.id, asset("asset_mac", "mac.mov"))
            .unwrap();
        server
            .add_portable_project_asset(&project.id, asset("asset_server", "server.mov"))
            .unwrap();
        let mac_operations = mac.list_operations(&project.id).unwrap();
        let request = ReconcileRequest {
            protocol_version: ProtocolRange::default(),
            project_schema_version: PROJECT_CATALOG_SCHEMA_VERSION,
            project_id: project.id.clone(),
            server_node_id: "node_server".to_string(),
            operation_cursors: BTreeMap::from([("node_mac".to_string(), 1)]),
            operations: mac_operations.clone(),
            observations: vec![],
            representation_manifests: vec![],
            maximum_response_operations: 10,
        };
        let response = reconcile(&server, &project.id, "node_server", &request).unwrap();
        assert_eq!(response.committed_cursors["node_mac"], 1);
        assert_eq!(response.operations.len(), 1);
        assert_eq!(response.operations[0].origin_node_id, "node_server");
        mac.apply_operation_batch("node_server", &response.operations)
            .unwrap();

        let retry = reconcile(&server, &project.id, "node_server", &request).unwrap();
        assert_eq!(retry.operations, response.operations);
        server
            .link_project_asset_media(
                "asset_mac",
                "media_provisional",
                MediaVerificationState::Provisional,
            )
            .unwrap();
        let link_request = ReconcileRequest {
            operation_cursors: BTreeMap::from([
                ("node_mac".to_string(), 1),
                ("node_server".to_string(), 1),
            ]),
            operations: vec![],
            ..request.clone()
        };
        let link_response = reconcile(&server, &project.id, "node_server", &link_request).unwrap();
        assert_eq!(link_response.operations.len(), 1);
        mac.apply_operation_batch("node_server", &link_response.operations)
            .unwrap();
        for catalog in [&mac, &server] {
            let linked = catalog.get_asset("asset_mac").unwrap();
            assert_eq!(linked.id, "asset_mac");
            assert_eq!(linked.media_object_id.as_deref(), Some("media_provisional"));
            assert_eq!(
                linked.media_verification_state,
                MediaVerificationState::Provisional
            );
        }
        assert_eq!(server.list_assets(&project.id).unwrap().len(), 2);
        assert_eq!(mac.list_assets(&project.id).unwrap().len(), 2);
        assert_eq!(
            mac.list_operations(&project.id).unwrap().len(),
            server.list_operations(&project.id).unwrap().len()
        );
    }

    #[test]
    fn sequence_gaps_do_not_advance_operation_or_fact_cursors() {
        let root = TempDir::new().unwrap();
        let mac = new_catalog(&root, "gap-mac.sqlite3", "node_mac");
        let project = mac.create_project("Trip").unwrap();
        let baseline = BaselineEnvelope::export(&mac, &project.id, vec![], vec![]).unwrap();
        let server = Catalog::import_project_baseline(
            root.path().join("gap-server.sqlite3"),
            &baseline.payload.project,
            "node_server",
            BASELINE_VERSION,
            &baseline.digest,
        )
        .unwrap();
        mac.add_portable_project_asset(&project.id, asset("asset_one", "one.mov"))
            .unwrap();
        mac.add_portable_project_asset(&project.id, asset("asset_two", "two.mov"))
            .unwrap();
        let operation_two = mac.list_operations(&project.id).unwrap().remove(1);
        let request = ReconcileRequest {
            protocol_version: ProtocolRange::default(),
            project_schema_version: PROJECT_CATALOG_SCHEMA_VERSION,
            project_id: project.id.clone(),
            server_node_id: "node_server".to_string(),
            operation_cursors: BTreeMap::new(),
            operations: vec![operation_two],
            observations: vec![],
            representation_manifests: vec![],
            maximum_response_operations: 10,
        };
        let error = reconcile(&server, &project.id, "node_server", &request).unwrap_err();
        assert_eq!(
            error.downcast_ref::<SyncError>().unwrap().code(),
            "sync_sequence_gap"
        );
        assert!(server.operation_cursors().unwrap().is_empty());

        let fact_error = persist_baseline_facts(
            &server,
            &[ObservationFact {
                origin_node_id: "node_mac".to_string(),
                origin_sequence: 2,
                scope_key: "asset_one/original".to_string(),
                payload: json!({"available": true}),
            }],
            &[],
        )
        .unwrap_err();
        assert_eq!(
            fact_error.downcast_ref::<SyncError>().unwrap().code(),
            "sync_sequence_gap"
        );
        let connection = Connection::open(server.database_path()).unwrap();
        assert_eq!(
            fact_cursors(&connection, "sync_observation_facts")
                .unwrap()
                .get("node_mac"),
            None
        );
    }

    #[test]
    fn observations_are_origin_scoped_and_manifests_exclude_job_progress() {
        let root = TempDir::new().unwrap();
        let catalog = new_catalog(&root, "facts.sqlite3", "node_server");
        let project = catalog.create_project("Trip").unwrap();
        let observations = vec![
            ObservationFact {
                origin_node_id: "node_mac".to_string(),
                origin_sequence: 1,
                scope_key: "asset/original".to_string(),
                payload: json!({"available": false}),
            },
            ObservationFact {
                origin_node_id: "node_mac".to_string(),
                origin_sequence: 2,
                scope_key: "asset/original".to_string(),
                payload: json!({"available": true}),
            },
            ObservationFact {
                origin_node_id: "node_server".to_string(),
                origin_sequence: 1,
                scope_key: "asset/original".to_string(),
                payload: json!({"available": false}),
            },
        ];
        let (cursors, _) = persist_baseline_facts(&catalog, &observations, &[]).unwrap();
        assert_eq!(cursors["node_mac"], 2);
        assert_eq!(cursors["node_server"], 1);
        let connection = Connection::open(catalog.database_path()).unwrap();
        let latest_mac: String = connection
            .query_row(
                "SELECT payload_json FROM sync_observation_facts WHERE origin_node_id='node_mac' AND scope_key='asset/original' ORDER BY origin_sequence DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&latest_mac).unwrap(),
            json!({"available": true})
        );

        let error = persist_baseline_facts(
            &catalog,
            &[],
            &[RepresentationManifestFact {
                manifest_id: "manifest_1".to_string(),
                origin_node_id: "node_server".to_string(),
                origin_sequence: 1,
                payload: json!({"representationId": "rep_1", "jobProgress": 0.5}),
            }],
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<SyncError>().unwrap().code(),
            "sync_sequence_gap"
        );
        assert_eq!(catalog.list_projects().unwrap()[0].id, project.id);
    }

    #[test]
    fn every_origin_preserving_delivery_interleaving_converges() {
        let root = TempDir::new().unwrap();
        let mac = new_catalog(&root, "permutation-mac.sqlite3", "node_mac");
        let project = mac.create_project("Trip").unwrap();
        mac.add_portable_project_asset(&project.id, asset("asset_shared", "shared.mov"))
            .unwrap();
        let baseline = BaselineEnvelope::export(&mac, &project.id, vec![], vec![]).unwrap();
        let server = Catalog::import_project_baseline(
            root.path().join("permutation-server.sqlite3"),
            &baseline.payload.project,
            "node_server",
            BASELINE_VERSION,
            &baseline.digest,
        )
        .unwrap();

        mac.create_note_revision("asset_shared", "mac note")
            .unwrap();
        mac.add_label("asset_shared", "Mac").unwrap();
        mac.set_removed("asset_shared", true).unwrap();
        server.add_label("asset_shared", "Server").unwrap();
        server.set_rotation("asset_shared", 90).unwrap();
        let mac_delta = mac
            .list_operations(&project.id)
            .unwrap()
            .into_iter()
            .filter(|operation| operation.origin_node_id == "node_mac")
            .skip(1)
            .collect::<Vec<_>>();
        let server_delta = server
            .list_operations(&project.id)
            .unwrap()
            .into_iter()
            .filter(|operation| operation.origin_node_id == "node_server")
            .collect::<Vec<_>>();

        fn interleave(
            left: &[ProjectOperation],
            right: &[ProjectOperation],
            prefix: &mut Vec<ProjectOperation>,
            output: &mut Vec<Vec<ProjectOperation>>,
        ) {
            if left.is_empty() && right.is_empty() {
                output.push(prefix.clone());
                return;
            }
            if let Some((first, rest)) = left.split_first() {
                prefix.push(first.clone());
                interleave(rest, right, prefix, output);
                prefix.pop();
            }
            if let Some((first, rest)) = right.split_first() {
                prefix.push(first.clone());
                interleave(left, rest, prefix, output);
                prefix.pop();
            }
        }

        let mut deliveries = Vec::new();
        interleave(&mac_delta, &server_delta, &mut Vec::new(), &mut deliveries);
        assert_eq!(deliveries.len(), 10);
        let mut expected = None;
        for (index, delivery) in deliveries.into_iter().enumerate() {
            let replica = Catalog::import_project_baseline(
                root.path().join(format!("permutation-{index}.sqlite3")),
                &baseline.payload.project,
                &format!("node_replica_{index}"),
                BASELINE_VERSION,
                &baseline.digest,
            )
            .unwrap();
            for operation in delivery {
                replica
                    .apply_operation_batch(
                        &operation.origin_node_id,
                        std::slice::from_ref(&operation),
                    )
                    .unwrap();
            }
            let asset = replica.get_asset("asset_shared").unwrap();
            let projection = (
                asset.notes,
                asset.labels,
                asset.removed,
                asset.rotation_degrees,
            );
            if let Some(expected) = &expected {
                assert_eq!(&projection, expected);
            } else {
                expected = Some(projection);
            }
        }
    }
}
