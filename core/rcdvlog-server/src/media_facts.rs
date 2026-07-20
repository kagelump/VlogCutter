use chrono::{SecondsFormat, Utc};
use rcdvlog_core::{MediaObjectId, StorageObjectId};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MediaFactsError {
    #[error("invalid {field}: {reason}")]
    InvalidInput { field: &'static str, reason: String },
    #[error("{entity} {id} was not found")]
    NotFound { entity: &'static str, id: String },
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl From<MediaFactsError> for crate::jobs::JobEngineError {
    fn from(error: MediaFactsError) -> Self {
        crate::jobs::JobEngineError::Infrastructure(anyhow::anyhow!("{error}"))
    }
}

/// Immutable quick-fingerprint evidence for one storage-object version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickFingerprintFact {
    pub storage_object_id: StorageObjectId,
    pub recipe_key: String,
    pub recipe_version: String,
    pub size_bytes: u64,
    pub quick_fingerprint: String,
    pub captured_at: String,
}

/// Immutable full-hash evidence for one storage-object version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FullHashFact {
    pub storage_object_id: StorageObjectId,
    pub recipe_key: String,
    pub recipe_version: String,
    pub full_content_hash: String,
    pub byte_count: u64,
    pub pre_read_size_bytes: u64,
    pub pre_read_modified_ns: Option<i64>,
    pub post_read_size_bytes: u64,
    pub post_read_modified_ns: Option<i64>,
    pub captured_at: String,
}

/// Immutable technical-probe evidence for one storage-object version.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeFact {
    pub storage_object_id: StorageObjectId,
    pub recipe_key: String,
    pub recipe_version: String,
    pub probe_json: String,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub enrichment_status: String,
    pub enrichment_warning: Option<String>,
    pub captured_at: String,
}

/// Per-file diagnostic tied to a scan run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanFileDiagnostic {
    pub diagnostic_id: String,
    pub scan_id: String,
    pub storage_object_id: Option<StorageObjectId>,
    pub stage: String,
    pub code: String,
    pub safe_path_identity: String,
    pub retryable: bool,
    pub recorded_at: String,
}

/// Mismatch evidence when a quick-fingerprint candidate conflicts at full-hash time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaMismatchEvidence {
    pub mismatch_id: String,
    pub media_object_id: Option<MediaObjectId>,
    pub storage_object_id: Option<StorageObjectId>,
    pub mismatch_type: String,
    pub related_media_object_id: Option<MediaObjectId>,
    pub related_storage_object_id: Option<StorageObjectId>,
    pub recorded_at: String,
}

/// Persists a quick-fingerprint fact into the control catalog.
pub(crate) fn upsert_quick_fingerprint_fact(
    transaction: &Transaction<'_>,
    fact: &QuickFingerprintFact,
) -> Result<(), MediaFactsError> {
    transaction.execute(
        r#"
        INSERT INTO media_quick_fingerprint_facts (
            storage_object_id, recipe_key, recipe_version,
            size_bytes, quick_fingerprint, captured_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ON CONFLICT(storage_object_id, recipe_key, recipe_version) DO UPDATE SET
            size_bytes = excluded.size_bytes,
            quick_fingerprint = excluded.quick_fingerprint,
            captured_at = excluded.captured_at
        "#,
        params![
            fact.storage_object_id.as_str(),
            fact.recipe_key,
            fact.recipe_version,
            fact.size_bytes as i64,
            fact.quick_fingerprint,
            fact.captured_at,
        ],
    )?;
    Ok(())
}

/// Persists a full-hash fact into the control catalog.
pub(crate) fn upsert_full_hash_fact(
    transaction: &Transaction<'_>,
    fact: &FullHashFact,
) -> Result<(), MediaFactsError> {
    transaction.execute(
        r#"
        INSERT INTO media_full_hash_facts (
            storage_object_id, recipe_key, recipe_version,
            full_content_hash, byte_count,
            pre_read_size_bytes, pre_read_modified_ns,
            post_read_size_bytes, post_read_modified_ns,
            captured_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
        ON CONFLICT(storage_object_id, recipe_key, recipe_version) DO UPDATE SET
            full_content_hash = excluded.full_content_hash,
            byte_count = excluded.byte_count,
            pre_read_size_bytes = excluded.pre_read_size_bytes,
            pre_read_modified_ns = excluded.pre_read_modified_ns,
            post_read_size_bytes = excluded.post_read_size_bytes,
            post_read_modified_ns = excluded.post_read_modified_ns,
            captured_at = excluded.captured_at
        "#,
        params![
            fact.storage_object_id.as_str(),
            fact.recipe_key,
            fact.recipe_version,
            fact.full_content_hash,
            fact.byte_count as i64,
            fact.pre_read_size_bytes as i64,
            fact.pre_read_modified_ns,
            fact.post_read_size_bytes as i64,
            fact.post_read_modified_ns,
            fact.captured_at,
        ],
    )?;
    Ok(())
}

/// Persists a probe fact into the control catalog.
pub(crate) fn upsert_probe_fact(
    transaction: &Transaction<'_>,
    fact: &ProbeFact,
) -> Result<(), MediaFactsError> {
    transaction.execute(
        r#"
        INSERT INTO media_probe_facts (
            storage_object_id, recipe_key, recipe_version,
            probe_json, camera_make, camera_model,
            enrichment_status, enrichment_warning, captured_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        ON CONFLICT(storage_object_id, recipe_key, recipe_version) DO UPDATE SET
            probe_json = excluded.probe_json,
            camera_make = excluded.camera_make,
            camera_model = excluded.camera_model,
            enrichment_status = excluded.enrichment_status,
            enrichment_warning = excluded.enrichment_warning,
            captured_at = excluded.captured_at
        "#,
        params![
            fact.storage_object_id.as_str(),
            fact.recipe_key,
            fact.recipe_version,
            fact.probe_json,
            fact.camera_make,
            fact.camera_model,
            fact.enrichment_status,
            fact.enrichment_warning,
            fact.captured_at,
        ],
    )?;
    Ok(())
}

/// Persists a file diagnostic record.
pub(crate) fn insert_file_diagnostic(
    transaction: &Transaction<'_>,
    diagnostic: &ScanFileDiagnostic,
) -> Result<(), MediaFactsError> {
    transaction.execute(
        r#"
        INSERT INTO scan_file_diagnostics (
            diagnostic_id, scan_id, storage_object_id,
            stage, code, safe_path_identity, retryable, recorded_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        "#,
        params![
            diagnostic.diagnostic_id,
            diagnostic.scan_id,
            diagnostic
                .storage_object_id
                .as_ref()
                .map(StorageObjectId::as_str),
            diagnostic.stage,
            diagnostic.code,
            diagnostic.safe_path_identity,
            diagnostic.retryable,
            diagnostic.recorded_at,
        ],
    )?;
    Ok(())
}

/// Persists mismatch evidence.
#[allow(dead_code)]
pub(crate) fn insert_mismatch_evidence(
    transaction: &Transaction<'_>,
    evidence: &MediaMismatchEvidence,
) -> Result<(), MediaFactsError> {
    transaction.execute(
        r#"
        INSERT INTO media_mismatch_evidence (
            mismatch_id, media_object_id, storage_object_id,
            mismatch_type, related_media_object_id, related_storage_object_id,
            recorded_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        "#,
        params![
            evidence.mismatch_id,
            evidence.media_object_id.as_ref().map(MediaObjectId::as_str),
            evidence
                .storage_object_id
                .as_ref()
                .map(StorageObjectId::as_str),
            evidence.mismatch_type,
            evidence
                .related_media_object_id
                .as_ref()
                .map(MediaObjectId::as_str),
            evidence
                .related_storage_object_id
                .as_ref()
                .map(StorageObjectId::as_str),
            evidence.recorded_at,
        ],
    )?;
    Ok(())
}

/// Finds a candidate media object by quick fingerprint + size.
#[allow(dead_code)]
pub(crate) fn find_candidate_by_quick_fingerprint(
    transaction: &Transaction<'_>,
    quick_fingerprint: &str,
    size_bytes: u64,
) -> Result<Option<MediaObjectId>, MediaFactsError> {
    let id_str: Option<String> = transaction
        .query_row(
            r#"
            SELECT mo.media_object_id
            FROM media_objects mo
            WHERE mo.quick_fingerprint = ?1 AND mo.size_bytes = ?2
              AND mo.verification_state IN ('provisional', 'verified')
            LIMIT 1
            "#,
            params![quick_fingerprint, size_bytes as i64],
            |row| row.get(0),
        )
        .optional()?;
    id_str
        .map(|s| {
            MediaObjectId::parse(s).map_err(|error| {
                MediaFactsError::Database(rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                ))
            })
        })
        .transpose()
}

/// Finds a verified media object by full hash + size.
#[allow(dead_code)]
pub(crate) fn find_verified_by_full_hash(
    transaction: &Transaction<'_>,
    full_content_hash: &str,
    byte_count: u64,
) -> Result<Option<MediaObjectId>, MediaFactsError> {
    let id_str: Option<String> = transaction
        .query_row(
            r#"
            SELECT media_object_id FROM media_objects
            WHERE full_content_hash = ?1 AND size_bytes = ?2
              AND verification_state = 'verified'
            LIMIT 1
            "#,
            params![full_content_hash, byte_count as i64],
            |row| row.get(0),
        )
        .optional()?;
    id_str
        .map(|s| {
            MediaObjectId::parse(s).map_err(|error| {
                MediaFactsError::Database(rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                ))
            })
        })
        .transpose()
}

/// Loads a quick-fingerprint fact for a storage object.
#[allow(dead_code)]
pub(crate) fn get_quick_fingerprint_fact(
    transaction: &Transaction<'_>,
    storage_object_id: &StorageObjectId,
    recipe_key: &str,
    recipe_version: &str,
) -> Result<Option<QuickFingerprintFact>, MediaFactsError> {
    let row = transaction
        .query_row(
            r#"
            SELECT storage_object_id, recipe_key, recipe_version,
                   size_bytes, quick_fingerprint, captured_at
            FROM media_quick_fingerprint_facts
            WHERE storage_object_id = ?1 AND recipe_key = ?2 AND recipe_version = ?3
            "#,
            params![storage_object_id.as_str(), recipe_key, recipe_version],
            |row| {
                Ok(QuickFingerprintFact {
                    storage_object_id: StorageObjectId::parse(row.get::<_, String>(0)?)
                        .map_err(stored_error(0))?,
                    recipe_key: row.get(1)?,
                    recipe_version: row.get(2)?,
                    size_bytes: from_i64_row(row.get(3)?, 3)?,
                    quick_fingerprint: row.get(4)?,
                    captured_at: row.get(5)?,
                })
            },
        )
        .optional()?;
    match row {
        Some(fact) => Ok(Some(fact)),
        None => Ok(None),
    }
}

/// Loads a full-hash fact for a storage object.
#[allow(dead_code)]
pub(crate) fn get_full_hash_fact(
    transaction: &Transaction<'_>,
    storage_object_id: &StorageObjectId,
    recipe_key: &str,
    recipe_version: &str,
) -> Result<Option<FullHashFact>, MediaFactsError> {
    let row = transaction
        .query_row(
            r#"
            SELECT storage_object_id, recipe_key, recipe_version,
                   full_content_hash, byte_count,
                   pre_read_size_bytes, pre_read_modified_ns,
                   post_read_size_bytes, post_read_modified_ns,
                   captured_at
            FROM media_full_hash_facts
            WHERE storage_object_id = ?1 AND recipe_key = ?2 AND recipe_version = ?3
            "#,
            params![storage_object_id.as_str(), recipe_key, recipe_version],
            |row| {
                Ok(FullHashFact {
                    storage_object_id: StorageObjectId::parse(row.get::<_, String>(0)?)
                        .map_err(stored_error(0))?,
                    recipe_key: row.get(1)?,
                    recipe_version: row.get(2)?,
                    full_content_hash: row.get(3)?,
                    byte_count: from_i64_row(row.get(4)?, 4)?,
                    pre_read_size_bytes: from_i64_row(row.get(5)?, 5)?,
                    pre_read_modified_ns: row.get(6)?,
                    post_read_size_bytes: from_i64_row(row.get(7)?, 7)?,
                    post_read_modified_ns: row.get(8)?,
                    captured_at: row.get(9)?,
                })
            },
        )
        .optional()?;
    match row {
        Some(fact) => Ok(Some(fact)),
        None => Ok(None),
    }
}

#[allow(dead_code)]
fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[allow(dead_code)]
fn from_i64_row(value: i64, column: usize) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

#[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::open_writable_connection;
    use crate::{ControlCatalog, ServerConfig};
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, ControlCatalog) {
        let root = TempDir::new().unwrap();
        let data = root.path().join("state");
        let derived = root.path().join("derived");
        let backups = root.path().join("backups");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&derived).unwrap();
        std::fs::create_dir_all(&backups).unwrap();
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
        (root, catalog)
    }

    fn create_test_storage(tx: &Transaction<'_>, storage_id: &str, root_id: &str) {
        tx.execute(
            "INSERT OR IGNORE INTO storage_roots(storage_root_id, display_name, immutable_original, created_at, updated_at) VALUES (?1, ?1, 0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            rusqlite::params![root_id],
        ).unwrap();
        // Use storage_id as relative_path to ensure uniqueness
        let path = format!("{storage_id}.mp4");
        tx.execute(
            "INSERT OR IGNORE INTO storage_objects(storage_object_id, storage_root_id, relative_path, created_at, updated_at) VALUES (?1, ?2, ?3, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            rusqlite::params![storage_id, root_id, path],
        ).unwrap();
    }

    #[test]
    fn quick_fingerprint_facts_can_be_upserted_and_retrieved() {
        let (_root, catalog) = fixture();
        let conn = open_writable_connection(catalog.database_path()).unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        create_test_storage(&tx, "storage_quick", "root_quick");

        let storage_id = StorageObjectId::parse("storage_quick").unwrap();
        let fact = QuickFingerprintFact {
            storage_object_id: storage_id.clone(),
            recipe_key: "scan.quick-fingerprint.sampled-v1".into(),
            recipe_version: "v1".into(),
            size_bytes: 1024,
            quick_fingerprint: "a".repeat(64),
            captured_at: now(),
        };
        upsert_quick_fingerprint_fact(&tx, &fact).unwrap();

        let got =
            get_quick_fingerprint_fact(&tx, &storage_id, &fact.recipe_key, &fact.recipe_version)
                .unwrap()
                .unwrap();
        assert_eq!(got.quick_fingerprint, fact.quick_fingerprint);
        assert_eq!(got.size_bytes, 1024);
    }

    #[test]
    fn full_hash_facts_can_be_upserted_and_retrieved() {
        let (_root, catalog) = fixture();
        let conn = open_writable_connection(catalog.database_path()).unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        create_test_storage(&tx, "storage_full", "root_full");

        let storage_id = StorageObjectId::parse("storage_full").unwrap();
        let fact = FullHashFact {
            storage_object_id: storage_id.clone(),
            recipe_key: "scan.full-hash.blake3-v1".into(),
            recipe_version: "v1".into(),
            full_content_hash: "b".repeat(64),
            byte_count: 2048,
            pre_read_size_bytes: 2048,
            pre_read_modified_ns: Some(1000),
            post_read_size_bytes: 2048,
            post_read_modified_ns: Some(1000),
            captured_at: now(),
        };
        upsert_full_hash_fact(&tx, &fact).unwrap();

        let got = get_full_hash_fact(&tx, &storage_id, &fact.recipe_key, &fact.recipe_version)
            .unwrap()
            .unwrap();
        assert_eq!(got.full_content_hash, fact.full_content_hash);
        assert_eq!(got.byte_count, 2048);
    }

    #[test]
    fn probe_facts_can_be_upserted() {
        let (_root, catalog) = fixture();
        let conn = open_writable_connection(catalog.database_path()).unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        create_test_storage(&tx, "storage_probe", "root_probe");

        let storage_id = StorageObjectId::parse("storage_probe").unwrap();
        let fact = ProbeFact {
            storage_object_id: storage_id,
            recipe_key: "scan.media-probe.ffprobe-v1".into(),
            recipe_version: "v1".into(),
            probe_json: r#"{"duration":1.0}"#.into(),
            camera_make: Some("Sony".into()),
            camera_model: Some("A7".into()),
            enrichment_status: "ok".into(),
            enrichment_warning: None,
            captured_at: now(),
        };
        upsert_probe_fact(&tx, &fact).unwrap();
    }

    #[test]
    fn diagnostics_and_mismatch_evidence_can_be_inserted() {
        let (_root, catalog) = fixture();
        let conn = open_writable_connection(catalog.database_path()).unwrap();
        let tx = conn.unchecked_transaction().unwrap();

        let diag = ScanFileDiagnostic {
            diagnostic_id: "diag_1".into(),
            scan_id: "scan_1".into(),
            storage_object_id: None,
            stage: "probe".into(),
            code: "ffprobe_failed".into(),
            safe_path_identity: "hashed-path-abc".into(),
            retryable: true,
            recorded_at: now(),
        };
        // Create supporting FK rows: storage roots first, then scan runs, then objects
        // create_test_storage creates root_mm entry
        tx.execute("INSERT OR IGNORE INTO storage_roots(storage_root_id, display_name, immutable_original, created_at, updated_at) VALUES ('root_mm', 'root_mm', 0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
        tx.execute(
            "INSERT INTO scan_runs(scan_id, node_id, storage_root_id, generation_id, root_job_id, source, state, created_at) VALUES ('scan_1', 'node_x', 'root_mm', 'generation_1', 'job_root_1', 'manual', 'queued', '2026-01-01T00:00:00Z')",
            [],
        ).unwrap();
        insert_file_diagnostic(&tx, &diag).unwrap();

        create_test_storage(&tx, "storage_a", "root_mm");
        create_test_storage(&tx, "storage_b", "root_mm");
        tx.execute("INSERT OR IGNORE INTO media_objects(media_object_id, verification_state, created_at, updated_at) VALUES ('media_a', 'provisional', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
        tx.execute("INSERT OR IGNORE INTO media_objects(media_object_id, verification_state, created_at, updated_at) VALUES ('media_b', 'provisional', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();

        let evidence = MediaMismatchEvidence {
            mismatch_id: "mismatch_1".into(),
            media_object_id: Some(MediaObjectId::parse("media_a").unwrap()),
            storage_object_id: Some(StorageObjectId::parse("storage_a").unwrap()),
            mismatch_type: "quick_fingerprint_collision".into(),
            related_media_object_id: Some(MediaObjectId::parse("media_b").unwrap()),
            related_storage_object_id: Some(StorageObjectId::parse("storage_b").unwrap()),
            recorded_at: now(),
        };
        insert_mismatch_evidence(&tx, &evidence).unwrap();
    }
}
