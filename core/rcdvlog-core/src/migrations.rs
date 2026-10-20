use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

use crate::chronology::forward_migrate_project_chronology;

pub const PROJECT_CATALOG_SCHEMA_VERSION: u32 = 29;

const MIGRATION_TABLE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
    migration_id INTEGER PRIMARY KEY CHECK(migration_id > 0),
    name TEXT NOT NULL,
    source_identity TEXT NOT NULL,
    applied_at TEXT NOT NULL
);
"#;

#[derive(Debug, Clone, Copy)]
pub struct SqliteMigration {
    pub id: u32,
    pub name: &'static str,
    pub source_identity: &'static str,
    pub apply: fn(&Transaction<'_>) -> Result<()>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaMigrationRecord {
    pub id: u32,
    pub name: String,
    pub source_identity: String,
    pub applied_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingMigration {
    pub id: u32,
    pub name: String,
    pub source_identity: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationHistoryStatus {
    LegacyUnversioned,
    UpgradeAvailable,
    Current,
    NewerUnsupported,
    IncompatibleHistory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationHistoryInspection {
    pub history_present: bool,
    pub current_version: u32,
    pub latest_supported_version: u32,
    pub status: MigrationHistoryStatus,
    pub applied_migrations: Vec<SchemaMigrationRecord>,
    pub pending_migrations: Vec<PendingMigration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CatalogSchemaError {
    #[error(
        "catalog schema version {found} is newer than supported version {supported}; upgrade RCD VlogCutter before opening this catalog writable"
    )]
    NewerSchema { found: u32, supported: u32 },
    #[error("catalog migration history is incompatible at migration {migration_id}: {detail}")]
    IncompatibleHistory { migration_id: u32, detail: String },
    #[error("invalid migration definitions at migration {migration_id}: {detail}")]
    InvalidDefinitions { migration_id: u32, detail: String },
}

impl CatalogSchemaError {
    pub const fn code(&self) -> &'static str {
        "schema_incompatible"
    }
}

pub const PROJECT_CATALOG_MIGRATIONS: &[SqliteMigration] = &[
    SqliteMigration {
        id: 1,
        name: "catalog_baseline",
        source_identity: "project-catalog/0001-catalog-baseline/20260720",
        apply: create_catalog_baseline,
    },
    SqliteMigration {
        id: 2,
        name: "legacy_orientation_compatibility",
        source_identity: "project-catalog/0002-legacy-orientation-compatibility/20260720",
        apply: migrate_legacy_orientation,
    },
    SqliteMigration {
        id: 3,
        name: "legacy_annotation_compatibility",
        source_identity: "project-catalog/0003-legacy-annotation-compatibility/20260720",
        apply: migrate_legacy_annotations,
    },
    SqliteMigration {
        id: 4,
        name: "companion_identity_links",
        source_identity: "project-catalog/0004-companion-identity-links/20260720",
        apply: add_companion_identity_links,
    },
    SqliteMigration {
        id: 5,
        name: "operation_backed_project_mutations",
        source_identity: "project-catalog/0005-operation-backed-project-mutations/20260723",
        apply: add_operation_backed_project_mutations,
    },
    SqliteMigration {
        id: 6,
        name: "project_sync_state",
        source_identity: "project-catalog/0006-project-sync-state/20260723",
        apply: add_project_sync_state,
    },
    SqliteMigration {
        id: 7,
        name: "restore_sequence_recovery",
        source_identity: "project-catalog/0007-restore-sequence-recovery/20260728",
        apply: add_restore_sequence_recovery,
    },
    SqliteMigration {
        id: 8,
        name: "peer_operation_acknowledgements",
        source_identity: "project-catalog/0008-peer-operation-acknowledgements/20260728",
        apply: add_peer_operation_acknowledgements,
    },
    SqliteMigration {
        id: 9,
        name: "chronology_facts",
        source_identity: "project-catalog/0009-chronology-facts/20260731",
        apply: add_chronology_facts,
    },
    SqliteMigration {
        id: 10,
        name: "diagnostics",
        source_identity: "project-catalog/0010-diagnostics/20260803",
        apply: add_diagnostics,
    },
    SqliteMigration {
        id: 11,
        name: "pattern_filename_counters",
        source_identity: "project-catalog/0011-pattern-filename-counters/20260803",
        apply: backfill_pattern_filename_counters,
    },
    SqliteMigration {
        id: 12,
        name: "edit_plan",
        source_identity: "project-catalog/0012-edit-plan/20260803",
        apply: add_edit_plan,
    },
    SqliteMigration {
        id: 13,
        name: "transcripts",
        source_identity: "project-catalog/0013-transcripts/20260803",
        apply: add_transcripts,
    },
    SqliteMigration {
        id: 14,
        name: "clip_analysis",
        source_identity: "project-catalog/0014-clip-analysis/20260803",
        apply: add_clip_analysis,
    },
    SqliteMigration {
        id: 15,
        name: "analysis_runs",
        source_identity: "project-catalog/0015-analysis-runs/20260804",
        apply: add_analysis_runs,
    },
    SqliteMigration {
        id: 16,
        name: "clip_anchored_trip_phases",
        source_identity: "project-catalog/0016-clip-anchored-trip-phases/20260809",
        apply: add_clip_anchored_trip_phases,
    },
    SqliteMigration {
        id: 17,
        name: "verified_asset_locations",
        source_identity: "project-catalog/0016-verified-asset-locations/20260809",
        apply: add_verified_asset_locations,
    },
    SqliteMigration {
        id: 18,
        name: "archive_roots",
        source_identity: "project-catalog/0017-archive-roots/20260809",
        apply: add_archive_roots,
    },
    SqliteMigration {
        id: 19,
        name: "import_history",
        source_identity: "project-catalog/0019-import-history/20260810",
        apply: add_import_history,
    },
    SqliteMigration {
        id: 20,
        name: "declined_capture_sequence_splits",
        source_identity: "project-catalog/0020-declined-capture-sequence-splits/20260812",
        apply: add_declined_capture_sequence_splits,
    },
    SqliteMigration {
        id: 21,
        name: "camera_identity_mappings",
        source_identity: "project-catalog/0021-camera-identity-mappings/20260819",
        apply: add_camera_identity_mappings,
    },
    SqliteMigration {
        id: 22,
        name: "camera_identity_mapping_exif_evidence",
        source_identity: "project-catalog/0022-camera-identity-mapping-exif-evidence/20260819",
        apply: add_camera_identity_mapping_exif_evidence,
    },
    SqliteMigration {
        id: 23,
        name: "camera_identity_aliases",
        source_identity: "project-catalog/0023-camera-identity-aliases/20260819",
        apply: add_camera_identity_aliases,
    },
    SqliteMigration {
        id: 24,
        name: "import_cutoffs",
        source_identity: "project-catalog/0024-import-cutoffs/20260820",
        apply: add_import_cutoffs,
    },
    SqliteMigration {
        id: 25,
        name: "volume_identity",
        source_identity: "project-catalog/0025-volume-identity/20260820",
        apply: add_volume_identity,
    },
    SqliteMigration {
        id: 26,
        name: "asset_video_validation",
        source_identity: "project-catalog/0026-asset-video-validation/20261020",
        apply: add_asset_video_validation,
    },
    SqliteMigration {
        id: 27,
        name: "asset_timestamp_candidates",
        source_identity: "project-catalog/0027-asset-timestamp-candidates/20261020",
        apply: add_asset_timestamp_candidates,
    },
    SqliteMigration {
        id: 28,
        name: "camera_proxy_sidecars",
        source_identity: "project-catalog/0028-camera-proxy-sidecars/20261021",
        apply: add_camera_proxy_sidecars,
    },
    SqliteMigration {
        id: 29,
        name: "chronology_suggestions",
        source_identity: "project-catalog/0029-chronology-suggestions/20261021",
        apply: add_chronology_suggestions,
    },
];

fn add_chronology_suggestions(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"CREATE TABLE IF NOT EXISTS analysis_chronology_suggestions (
            request_key TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            facts_revision TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('running','completed')),
            result_json TEXT NOT NULL DEFAULT '',
            model TEXT NOT NULL DEFAULT '',
            attempts INTEGER NOT NULL DEFAULT 0,
            cost_usd REAL NOT NULL DEFAULT 0,
            prompt_tokens INTEGER NOT NULL DEFAULT 0,
            completion_tokens INTEGER NOT NULL DEFAULT 0,
            latency_ms INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS analysis_chronology_suggestions_project
            ON analysis_chronology_suggestions(project_id, created_at DESC);"#,
    )?;
    Ok(())
}

fn add_camera_proxy_sidecars(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS camera_proxy_sidecars (
        asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
        path TEXT NOT NULL,
        card_root TEXT NOT NULL,
        byte_length INTEGER NOT NULL,
        fingerprint TEXT NOT NULL,
        codec TEXT NOT NULL,
        width INTEGER NOT NULL,
        height INTEGER NOT NULL,
        duration_seconds REAL NOT NULL,
        verified_at TEXT NOT NULL,
        verification TEXT NOT NULL CHECK(verification = 'verified')
    );
    CREATE INDEX IF NOT EXISTS camera_proxy_sidecars_card_root ON camera_proxy_sidecars(card_root);",
    )?;
    Ok(())
}

fn add_asset_timestamp_candidates(transaction: &Transaction<'_>) -> Result<()> {
    add_column_if_missing(transaction, "assets", "timestamp_candidates_json", "TEXT")?;
    Ok(())
}

pub fn migrate_project_catalog(connection: &mut Connection) -> Result<()> {
    reconcile_divergent_import_migration_history(connection)?;
    run_sqlite_migrations(connection, PROJECT_CATALOG_MIGRATIONS)
}

/// The import branch and main both allocated migration 16 on 2026-08-09.
/// Catalogs opened by the import branch before the histories were merged carry
/// verified locations at 16 and, optionally, archive roots at 17. Preserve
/// those applied identities by shifting them forward, then install main's
/// migration 16 in the same transaction. Released/main catalogs already have
/// the canonical migration 16 and never enter this compatibility path.
fn reconcile_divergent_import_migration_history(connection: &mut Connection) -> Result<()> {
    if !table_exists(connection, "schema_migrations")? {
        return Ok(());
    }

    let import_16: Option<(String, String)> = connection
        .query_row(
            "SELECT name, source_identity FROM schema_migrations WHERE migration_id = 16",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if import_16
        .as_ref()
        .map(|(name, identity)| (name.as_str(), identity.as_str()))
        != Some((
            "verified_asset_locations",
            "project-catalog/0016-verified-asset-locations/20260809",
        ))
    {
        return Ok(());
    }

    let import_17: Option<(String, String)> = connection
        .query_row(
            "SELECT name, source_identity FROM schema_migrations WHERE migration_id = 17",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if import_17
        .as_ref()
        .map(|(name, identity)| (name.as_str(), identity.as_str()))
        .is_some_and(|migration| {
            migration
                != (
                    "archive_roots",
                    "project-catalog/0017-archive-roots/20260809",
                )
        })
    {
        return Ok(());
    }

    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("begin divergent import migration reconciliation")?;
    if import_17.is_some() {
        transaction.execute(
            "UPDATE schema_migrations SET migration_id = 18 WHERE migration_id = 17",
            [],
        )?;
    }
    transaction.execute(
        "UPDATE schema_migrations SET migration_id = 17 WHERE migration_id = 16",
        [],
    )?;
    add_clip_anchored_trip_phases(&transaction)?;
    transaction.execute(
        r#"
        INSERT INTO schema_migrations(migration_id, name, source_identity, applied_at)
        VALUES (16, 'clip_anchored_trip_phases',
                'project-catalog/0016-clip-anchored-trip-phases/20260809', ?1)
        "#,
        [Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)],
    )?;
    transaction
        .commit()
        .context("commit divergent import migration reconciliation")?;
    Ok(())
}

pub fn inspect_project_catalog_migrations(
    connection: &Connection,
) -> Result<MigrationHistoryInspection> {
    inspect_sqlite_migrations(connection, PROJECT_CATALOG_MIGRATIONS)
}

pub fn validate_project_catalog_migrations(connection: &Connection) -> Result<()> {
    let inspection = inspect_project_catalog_migrations(connection)?;
    validate_applied_history(&inspection, PROJECT_CATALOG_MIGRATIONS)
}

pub fn run_sqlite_migrations(
    connection: &mut Connection,
    migrations: &[SqliteMigration],
) -> Result<()> {
    validate_definitions(migrations)?;
    validate_applied_history(
        &inspect_sqlite_migrations(connection, migrations)?,
        migrations,
    )?;

    let applied = load_applied_migrations(connection)?
        .into_iter()
        .map(|migration| migration.id)
        .collect::<std::collections::BTreeSet<_>>();
    for migration in migrations {
        if applied.contains(&migration.id) {
            continue;
        }
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .with_context(|| {
                format!(
                    "begin catalog migration {} ({})",
                    migration.id, migration.name
                )
            })?;
        transaction.execute_batch(MIGRATION_TABLE_SQL)?;
        (migration.apply)(&transaction).with_context(|| {
            format!(
                "catalog migration {} ({}) failed; the prior schema is unchanged",
                migration.id, migration.name
            )
        })?;
        transaction.execute(
            r#"
            INSERT INTO schema_migrations (
                migration_id, name, source_identity, applied_at
            ) VALUES (?1, ?2, ?3, ?4)
            "#,
            params![
                migration.id,
                migration.name,
                migration.source_identity,
                Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            ],
        )?;
        transaction.commit().with_context(|| {
            format!(
                "commit catalog migration {} ({})",
                migration.id, migration.name
            )
        })?;
    }
    Ok(())
}

pub fn inspect_sqlite_migrations(
    connection: &Connection,
    migrations: &[SqliteMigration],
) -> Result<MigrationHistoryInspection> {
    validate_definitions(migrations)?;
    let history_present = table_exists(connection, "schema_migrations")?;
    let applied_migrations = load_applied_migrations(connection)?;
    let current_version = applied_migrations
        .iter()
        .map(|migration| migration.id)
        .max()
        .unwrap_or(0);
    let latest_supported_version = migrations.last().map(|migration| migration.id).unwrap_or(0);
    let applied_by_id = applied_migrations
        .iter()
        .map(|migration| (migration.id, migration))
        .collect::<BTreeMap<_, _>>();
    let pending_migrations = migrations
        .iter()
        .filter(|migration| !applied_by_id.contains_key(&migration.id))
        .map(|migration| PendingMigration {
            id: migration.id,
            name: migration.name.to_string(),
            source_identity: migration.source_identity.to_string(),
        })
        .collect::<Vec<_>>();

    let incompatible = applied_migrations.iter().any(|applied| {
        migrations
            .iter()
            .find(|known| known.id == applied.id)
            .is_none_or(|known| {
                known.name != applied.name || known.source_identity != applied.source_identity
            })
    }) || (1..=current_version).any(|id| !applied_by_id.contains_key(&id));
    let status = if current_version > latest_supported_version {
        MigrationHistoryStatus::NewerUnsupported
    } else if incompatible {
        MigrationHistoryStatus::IncompatibleHistory
    } else if !history_present {
        MigrationHistoryStatus::LegacyUnversioned
    } else if !pending_migrations.is_empty() {
        MigrationHistoryStatus::UpgradeAvailable
    } else {
        MigrationHistoryStatus::Current
    };

    Ok(MigrationHistoryInspection {
        history_present,
        current_version,
        latest_supported_version,
        status,
        applied_migrations,
        pending_migrations,
    })
}

fn validate_definitions(migrations: &[SqliteMigration]) -> Result<()> {
    let mut names = BTreeSet::new();
    let mut source_identities = BTreeSet::new();
    for (index, migration) in migrations.iter().enumerate() {
        let expected_id = index as u32 + 1;
        if migration.id != expected_id {
            return Err(CatalogSchemaError::InvalidDefinitions {
                migration_id: migration.id,
                detail: format!("expected monotonically ordered ID {expected_id}"),
            }
            .into());
        }
        if migration.name.is_empty() || migration.source_identity.is_empty() {
            return Err(CatalogSchemaError::InvalidDefinitions {
                migration_id: migration.id,
                detail: "name and source identity must be non-empty".to_string(),
            }
            .into());
        }
        if !names.insert(migration.name) {
            return Err(CatalogSchemaError::InvalidDefinitions {
                migration_id: migration.id,
                detail: format!("duplicate migration name: {}", migration.name),
            }
            .into());
        }
        if !source_identities.insert(migration.source_identity) {
            return Err(CatalogSchemaError::InvalidDefinitions {
                migration_id: migration.id,
                detail: format!(
                    "duplicate migration source identity: {}",
                    migration.source_identity
                ),
            }
            .into());
        }
    }
    Ok(())
}

fn validate_applied_history(
    inspection: &MigrationHistoryInspection,
    migrations: &[SqliteMigration],
) -> Result<()> {
    if inspection.current_version > inspection.latest_supported_version {
        return Err(CatalogSchemaError::NewerSchema {
            found: inspection.current_version,
            supported: inspection.latest_supported_version,
        }
        .into());
    }
    if inspection.status == MigrationHistoryStatus::IncompatibleHistory {
        let known = migrations
            .iter()
            .map(|migration| (migration.id, migration))
            .collect::<BTreeMap<_, _>>();
        let mismatch = inspection.applied_migrations.iter().find(|applied| {
            known.get(&applied.id).is_none_or(|expected| {
                expected.name != applied.name || expected.source_identity != applied.source_identity
            })
        });
        let (migration_id, detail) = if let Some(applied) = mismatch {
            (
                applied.id,
                "stored name or source identity does not match this application".to_string(),
            )
        } else {
            let first_gap = (1..=inspection.current_version)
                .find(|id| {
                    !inspection
                        .applied_migrations
                        .iter()
                        .any(|applied| applied.id == *id)
                })
                .unwrap_or(inspection.current_version);
            (first_gap, "migration history contains a gap".to_string())
        };
        return Err(CatalogSchemaError::IncompatibleHistory {
            migration_id,
            detail,
        }
        .into());
    }
    Ok(())
}

fn load_applied_migrations(connection: &Connection) -> Result<Vec<SchemaMigrationRecord>> {
    if !table_exists(connection, "schema_migrations")? {
        return Ok(Vec::new());
    }
    let mut statement = connection.prepare(
        r#"
        SELECT migration_id, name, source_identity, applied_at
        FROM schema_migrations
        ORDER BY migration_id
        "#,
    )?;
    Ok(statement
        .query_map([], |row| {
            Ok(SchemaMigrationRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                source_identity: row.get(2)?,
                applied_at: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )?)
}

fn column_exists(connection: &Connection, table: &str, column: &str) -> Result<bool> {
    let sql = format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1 LIMIT 1");
    Ok(connection
        .query_row(&sql, [column], |_| Ok(()))
        .optional()?
        .is_some())
}

fn add_column_if_missing(
    transaction: &Transaction<'_>,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<bool> {
    if column_exists(transaction, table, column)? {
        return Ok(false);
    }
    transaction.execute_batch(&format!(
        "ALTER TABLE {table} ADD COLUMN {column} {definition}"
    ))?;
    Ok(true)
}

fn create_catalog_baseline(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS projects (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS assets (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            fingerprint TEXT NOT NULL,
            path TEXT NOT NULL,
            filename TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            duration_seconds REAL NOT NULL,
            frame_rate REAL NOT NULL DEFAULT 30,
            width INTEGER NOT NULL DEFAULT 0,
            height INTEGER NOT NULL DEFAULT 0,
            codec TEXT NOT NULL DEFAULT '',
            camera TEXT NOT NULL DEFAULT 'Unknown camera',
            captured_at_raw TEXT,
            best_time TEXT,
            offset_seconds INTEGER NOT NULL DEFAULT 0,
            proxy_path TEXT,
            metadata_rotation_degrees INTEGER NOT NULL DEFAULT 0,
            default_rotation_degrees INTEGER NOT NULL DEFAULT 0,
            rotation_degrees INTEGER NOT NULL DEFAULT 0,
            orientation_probed INTEGER NOT NULL DEFAULT 1,
            notes TEXT NOT NULL DEFAULT '',
            labels_json TEXT NOT NULL DEFAULT '[]',
            removed INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT 'unreviewed',
            included INTEGER NOT NULL DEFAULT 0,
            source_in REAL NOT NULL DEFAULT 0,
            source_out REAL NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            UNIQUE(project_id, fingerprint)
        );

        CREATE INDEX IF NOT EXISTS assets_project_order
        ON assets(project_id, best_time, filename, id);

        CREATE TABLE IF NOT EXISTS clock_events (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            camera TEXT NOT NULL,
            start_filename TEXT NOT NULL,
            offset_seconds INTEGER NOT NULL,
            label TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS asset_thumbnails (
            asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            quality INTEGER NOT NULL CHECK(quality BETWEEN 1 AND 10),
            path TEXT NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY(asset_id, quality)
        );

        CREATE TABLE IF NOT EXISTS batch_performance (
            task_kind TEXT PRIMARY KEY,
            work_units_per_worker_second REAL NOT NULL,
            samples INTEGER NOT NULL,
            updated_at TEXT NOT NULL
        );
        "#,
    )?;
    Ok(())
}

fn migrate_legacy_orientation(transaction: &Transaction<'_>) -> Result<()> {
    let added_default_rotation = add_column_if_missing(
        transaction,
        "assets",
        "default_rotation_degrees",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(
        transaction,
        "assets",
        "metadata_rotation_degrees",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    let added_rotation = add_column_if_missing(
        transaction,
        "assets",
        "rotation_degrees",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(
        transaction,
        "assets",
        "orientation_probed",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    if added_default_rotation || added_rotation {
        transaction.execute(
            r#"
            UPDATE assets
            SET default_rotation_degrees = CASE WHEN height > width THEN 90 ELSE 0 END,
                rotation_degrees = CASE WHEN height > width THEN 90 ELSE 0 END
            "#,
            [],
        )?;
    }
    Ok(())
}

fn migrate_legacy_annotations(transaction: &Transaction<'_>) -> Result<()> {
    add_column_if_missing(
        transaction,
        "assets",
        "labels_json",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    add_column_if_missing(
        transaction,
        "assets",
        "removed",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    Ok(())
}

fn add_companion_identity_links(transaction: &Transaction<'_>) -> Result<()> {
    add_column_if_missing(transaction, "projects", "origin_node_id", "TEXT")?;
    add_column_if_missing(transaction, "projects", "paired_server_node_id", "TEXT")?;
    add_column_if_missing(transaction, "projects", "paired_at", "TEXT")?;
    add_column_if_missing(transaction, "assets", "media_object_id", "TEXT")?;
    add_column_if_missing(
        transaction,
        "assets",
        "media_verification_state",
        "TEXT NOT NULL DEFAULT 'unlinked' CHECK(media_verification_state IN ('unlinked', 'provisional', 'verified', 'mismatch'))",
    )?;
    transaction.execute_batch(
        r#"
        CREATE INDEX IF NOT EXISTS assets_media_object
        ON assets(media_object_id)
        WHERE media_object_id IS NOT NULL;
        "#,
    )?;
    Ok(())
}

fn add_operation_backed_project_mutations(transaction: &Transaction<'_>) -> Result<()> {
    add_column_if_missing(transaction, "clock_events", "operation_id", "TEXT")?;
    add_column_if_missing(transaction, "clock_events", "hlc_wall_millis", "INTEGER")?;
    add_column_if_missing(transaction, "clock_events", "hlc_counter", "INTEGER")?;
    add_column_if_missing(transaction, "clock_events", "origin_node_id", "TEXT")?;
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS catalog_identity (
            singleton_id INTEGER PRIMARY KEY CHECK(singleton_id = 1),
            node_id TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS operation_origin_sequences (
            origin_node_id TEXT PRIMARY KEY,
            last_sequence INTEGER NOT NULL CHECK(last_sequence >= 0)
        );

        CREATE TABLE IF NOT EXISTS operation_hlc_state (
            node_id TEXT PRIMARY KEY,
            wall_millis INTEGER NOT NULL CHECK(wall_millis >= 0),
            counter INTEGER NOT NULL CHECK(counter >= 0)
        );

        CREATE TABLE IF NOT EXISTS project_operations (
            operation_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            origin_node_id TEXT NOT NULL,
            origin_sequence INTEGER NOT NULL CHECK(origin_sequence > 0),
            hlc_wall_millis INTEGER NOT NULL CHECK(hlc_wall_millis >= 0),
            hlc_counter INTEGER NOT NULL CHECK(hlc_counter >= 0),
            aggregate_type TEXT NOT NULL,
            aggregate_id TEXT NOT NULL,
            operation_type TEXT NOT NULL,
            payload_version INTEGER NOT NULL CHECK(payload_version > 0),
            base_revision INTEGER CHECK(base_revision >= 0),
            payload_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            UNIQUE(origin_node_id, origin_sequence)
        );
        CREATE INDEX IF NOT EXISTS project_operations_project_order
        ON project_operations(project_id, hlc_wall_millis, hlc_counter, origin_node_id, operation_id);
        CREATE INDEX IF NOT EXISTS project_operations_aggregate
        ON project_operations(project_id, aggregate_type, aggregate_id, operation_id);

        CREATE TABLE IF NOT EXISTS materialized_revisions (
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            aggregate_type TEXT NOT NULL,
            aggregate_id TEXT NOT NULL,
            revision INTEGER NOT NULL CHECK(revision >= 0),
            last_operation_id TEXT,
            PRIMARY KEY(project_id, aggregate_type, aggregate_id)
        );

        CREATE TABLE IF NOT EXISTS project_asset_local_locations (
            asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            node_id TEXT NOT NULL,
            local_path TEXT NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY(asset_id, node_id)
        );

        CREATE TABLE IF NOT EXISTS note_revisions (
            revision_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            operation_id TEXT UNIQUE REFERENCES project_operations(operation_id),
            text TEXT NOT NULL,
            hlc_wall_millis INTEGER NOT NULL CHECK(hlc_wall_millis >= 0),
            hlc_counter INTEGER NOT NULL CHECK(hlc_counter >= 0),
            origin_node_id TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS note_revisions_active_order
        ON note_revisions(asset_id, hlc_wall_millis DESC, hlc_counter DESC, origin_node_id DESC, revision_id DESC);

        CREATE TABLE IF NOT EXISTS label_tags (
            add_tag TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            normalized_label TEXT NOT NULL,
            display_label TEXT NOT NULL,
            operation_id TEXT UNIQUE REFERENCES project_operations(operation_id),
            hlc_wall_millis INTEGER NOT NULL CHECK(hlc_wall_millis >= 0),
            hlc_counter INTEGER NOT NULL CHECK(hlc_counter >= 0),
            origin_node_id TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS label_tags_asset_value
        ON label_tags(asset_id, normalized_label, hlc_wall_millis, hlc_counter, origin_node_id, add_tag);

        CREATE TABLE IF NOT EXISTS label_tag_removals (
            remove_operation_id TEXT NOT NULL REFERENCES project_operations(operation_id),
            add_tag TEXT NOT NULL,
            normalized_label TEXT NOT NULL,
            PRIMARY KEY(remove_operation_id, add_tag)
        );
        CREATE INDEX IF NOT EXISTS label_tag_removals_tag ON label_tag_removals(add_tag);

        CREATE TABLE IF NOT EXISTS lww_registers (
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            aggregate_type TEXT NOT NULL,
            aggregate_id TEXT NOT NULL,
            field_name TEXT NOT NULL,
            value_json TEXT NOT NULL,
            operation_id TEXT REFERENCES project_operations(operation_id),
            hlc_wall_millis INTEGER NOT NULL CHECK(hlc_wall_millis >= 0),
            hlc_counter INTEGER NOT NULL CHECK(hlc_counter >= 0),
            origin_node_id TEXT NOT NULL,
            PRIMARY KEY(project_id, aggregate_type, aggregate_id, field_name)
        );

        CREATE TABLE IF NOT EXISTS operation_conflicts (
            conflict_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            aggregate_type TEXT NOT NULL,
            aggregate_id TEXT NOT NULL,
            incoming_operation_id TEXT NOT NULL UNIQUE REFERENCES project_operations(operation_id),
            active_operation_id TEXT,
            expected_base_revision INTEGER,
            actual_revision INTEGER NOT NULL,
            conflict_type TEXT NOT NULL,
            resolved_at TEXT,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS operation_conflicts_unresolved
        ON operation_conflicts(project_id, resolved_at, aggregate_type, aggregate_id);

        INSERT OR IGNORE INTO materialized_revisions(project_id, aggregate_type, aggregate_id, revision, last_operation_id)
        SELECT project_id, 'projectAsset', id, 0, NULL FROM assets;

        INSERT OR IGNORE INTO note_revisions(
            revision_id, project_id, asset_id, operation_id, text,
            hlc_wall_millis, hlc_counter, origin_node_id, created_at
        )
        SELECT 'baseline_note_' || id, project_id, id, NULL, notes,
               0, 0, 'baseline', created_at
        FROM assets;

        INSERT OR IGNORE INTO lww_registers(
            project_id, aggregate_type, aggregate_id, field_name, value_json,
            operation_id, hlc_wall_millis, hlc_counter, origin_node_id
        )
        SELECT project_id, 'projectAsset', id, 'removed',
               CASE WHEN removed = 1 THEN 'true' ELSE 'false' END,
               NULL, 0, 0, 'baseline'
        FROM assets;
        INSERT OR IGNORE INTO lww_registers(
            project_id, aggregate_type, aggregate_id, field_name, value_json,
            operation_id, hlc_wall_millis, hlc_counter, origin_node_id
        )
        SELECT project_id, 'projectAsset', id, 'rotationDegrees',
               CAST(rotation_degrees AS TEXT), NULL, 0, 0, 'baseline'
        FROM assets;

        INSERT OR IGNORE INTO materialized_revisions(project_id, aggregate_type, aggregate_id, revision, last_operation_id)
        SELECT project_id, 'clock', camera, COUNT(*), NULL
        FROM clock_events GROUP BY project_id, camera;
        "#,
    )?;

    let mut assets =
        transaction.prepare("SELECT id, project_id, labels_json, created_at FROM assets")?;
    let rows = assets
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(assets);
    for (asset_id, project_id, labels_json, created_at) in rows {
        let labels = serde_json::from_str::<Vec<String>>(&labels_json).unwrap_or_default();
        for (ordinal, label) in labels.into_iter().enumerate() {
            let normalized = label.trim().to_lowercase();
            transaction.execute(
                r#"
                INSERT INTO label_tags(
                    add_tag, project_id, asset_id, normalized_label, display_label,
                    operation_id, hlc_wall_millis, hlc_counter, origin_node_id, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, 0, ?6, 'baseline', ?7)
                ON CONFLICT(add_tag) DO NOTHING
                "#,
                params![
                    format!("baseline_label_{asset_id}_{ordinal}"),
                    project_id,
                    asset_id,
                    normalized,
                    label,
                    ordinal as i64,
                    created_at,
                ],
            )?;
        }
    }
    Ok(())
}

fn add_project_sync_state(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS project_baseline_metadata (
            singleton_id INTEGER PRIMARY KEY CHECK(singleton_id = 1),
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            baseline_version INTEGER NOT NULL CHECK(baseline_version > 0),
            baseline_digest TEXT NOT NULL,
            imported_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS project_storage_root_mappings (
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            storage_root_id TEXT NOT NULL,
            relative_prefix TEXT NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY(project_id, storage_root_id, relative_prefix)
        );

        CREATE TABLE IF NOT EXISTS sync_origin_cursors (
            origin_node_id TEXT PRIMARY KEY,
            committed_sequence INTEGER NOT NULL CHECK(committed_sequence >= 0),
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS sync_observation_facts (
            origin_node_id TEXT NOT NULL,
            origin_sequence INTEGER NOT NULL CHECK(origin_sequence > 0),
            scope_key TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            recorded_at TEXT NOT NULL,
            PRIMARY KEY(origin_node_id, origin_sequence)
        );
        CREATE INDEX IF NOT EXISTS sync_observation_scope
        ON sync_observation_facts(origin_node_id, scope_key, origin_sequence DESC);

        CREATE TABLE IF NOT EXISTS sync_representation_manifests (
            manifest_id TEXT PRIMARY KEY,
            origin_node_id TEXT NOT NULL,
            origin_sequence INTEGER NOT NULL CHECK(origin_sequence > 0),
            payload_json TEXT NOT NULL,
            recorded_at TEXT NOT NULL,
            UNIQUE(origin_node_id, origin_sequence)
        );
        "#,
    )?;
    Ok(())
}

fn add_restore_sequence_recovery(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS operation_sequence_recovery (
            peer_origin_node_id TEXT PRIMARY KEY,
            server_origin_node_id TEXT NOT NULL,
            manifest_watermark INTEGER NOT NULL CHECK(manifest_watermark >= 0),
            contained_watermark INTEGER NOT NULL CHECK(contained_watermark >= 0),
            reported_watermark INTEGER CHECK(reported_watermark IS NULL OR reported_watermark >= 0),
            state TEXT NOT NULL CHECK(state IN ('awaiting','divergent','completed')),
            divergence_start INTEGER,
            divergence_end INTEGER,
            decision TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            CHECK((state = 'divergent') = (divergence_start IS NOT NULL AND divergence_end IS NOT NULL)),
            CHECK(decision IS NULL OR state = 'completed')
        );
        CREATE INDEX IF NOT EXISTS operation_sequence_recovery_state
            ON operation_sequence_recovery(state, peer_origin_node_id);
        "#,
    )?;
    Ok(())
}

fn add_peer_operation_acknowledgements(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS sync_peer_operation_acknowledgements (
            peer_node_id TEXT NOT NULL,
            origin_node_id TEXT NOT NULL,
            committed_sequence INTEGER NOT NULL CHECK(committed_sequence >= 0),
            updated_at TEXT NOT NULL,
            PRIMARY KEY(peer_node_id, origin_node_id)
        );
        "#,
    )?;
    Ok(())
}

/// Migration 9: the persisted chronology fact model, frozen by D01-T007.
///
/// Six fact tables plus two ordering indexes. `clock_events` is retained, not
/// dropped: legacy operation replay still targets it and a rollback must
/// remain possible. Existing `clock_events` rows are forward-migrated into
/// `clock_epochs` by `forward_migrate_project_chronology`, which never writes
/// `assets.best_time` or `assets.offset_seconds` — recomputing projections is
/// precisely how a migration reorders a project.
/// Migration 10 (D01-T027). Purely additive: it creates one table and one
/// index and touches no existing row, so an existing project's asset list and
/// its order are byte-identical before and after.
/// Migration 11 (D01-T028). Fills `asset_capture_sequence.filename_counter`
/// where it is NULL and a camera filename pattern now supplies one.
///
/// It deliberately does *not* rewrite `assets.camera`. Doing so retroactively
/// would regroup capture sequences and reorder an existing project, which is
/// exactly what `CHRONOLOGY_MODEL.md`'s migration rules forbid. An existing
/// catalog whose cameras are anonymous is repaired by the user through
/// D01-T009's split and relabel.
///
/// Backfilling the counter is safe in a way that regrouping is not: migration 9
/// carries legacy clock events forward with `end_counter` NULL — open-ended —
/// and nothing emits a bounded epoch until D01-T010, so filling a NULL counter
/// cannot remove an asset from any epoch's coverage in any catalog that can
/// exist today.
/// Migration 12 (D01-T019). The edit-plan model.
///
/// Two invariants are enforced by the database rather than by application
/// discipline: `UNIQUE(revision_id, position)` makes two clips at one position
/// unrepresentable, and the partial unique index makes two active revisions for
/// one project unrepresentable.
///
/// There is deliberately no `track` column, no `transition` table, and no
/// `effect` column — not even reserved. That absence is the deliverable.
fn add_edit_plan(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS edit_plan_revisions (
            revision_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            script_revision_id TEXT,
            parent_revision_id TEXT,
            active INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS edit_plan_operations (
            operation_id TEXT PRIMARY KEY,
            revision_id TEXT NOT NULL REFERENCES edit_plan_revisions(revision_id) ON DELETE CASCADE,
            position INTEGER NOT NULL,
            asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            source_in_seconds REAL NOT NULL,
            source_out_seconds REAL NOT NULL,
            rationale TEXT NOT NULL DEFAULT '',
            UNIQUE(revision_id, position)
        );

        CREATE TABLE IF NOT EXISTS edit_plan_exports (
            export_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            revision_id TEXT NOT NULL REFERENCES edit_plan_revisions(revision_id) ON DELETE CASCADE,
            output_path TEXT NOT NULL,
            output_digest TEXT NOT NULL,
            exported_at TEXT NOT NULL
        );

        CREATE UNIQUE INDEX IF NOT EXISTS edit_plan_one_active
        ON edit_plan_revisions(project_id) WHERE active = 1;
        "#,
    )?;
    Ok(())
}

/// Migration 13. Local speech-to-text results as derived artifacts.
///
/// `asset_transcripts` is one row per asset because a transcript is identified
/// by its recipe key plus the fingerprint of the proxy it was derived from; a
/// second recipe supersedes the first rather than coexisting with it.
///
/// `status` distinguishes the three non-failure outcomes the pipeline can
/// produce. `no_speech` is a successful, meaningful result — the VAD gate found
/// nothing and no model was ever invoked — and must not be confused with
/// `failed`. Conflating them would retry every silent clip forever, and on the
/// measured corpus 64% of clips are silent.
fn add_transcripts(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS asset_transcripts (
            asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
            recipe_key TEXT NOT NULL,
            source_kind TEXT NOT NULL DEFAULT 'proxy',
            source_fingerprint TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('speech', 'no_speech', 'rejected', 'failed')),
            reject_reason TEXT NOT NULL DEFAULT '',
            primary_language TEXT NOT NULL DEFAULT '',
            speech_seconds REAL NOT NULL DEFAULT 0,
            window_count INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS transcript_segments (
            asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            segment_index INTEGER NOT NULL,
            start_ms INTEGER NOT NULL,
            end_ms INTEGER NOT NULL,
            language TEXT NOT NULL DEFAULT '',
            text TEXT NOT NULL,
            mean_token_probability REAL NOT NULL DEFAULT 0,
            PRIMARY KEY(asset_id, segment_index)
        );

        CREATE INDEX IF NOT EXISTS transcript_segments_time
        ON transcript_segments(asset_id, start_ms);
        "#,
    )?;
    Ok(())
}

/// Migration 14. Cloud vision analysis as derived artifacts.
///
/// `title` and `description` live on `assets` beside `notes` rather than inside
/// the analysis row: they are AI-written but human-editable, and an edit must
/// survive the analysis being deleted or re-run. `notes` remains human-only and
/// is never written by analysis.
///
/// `analysis_labels` records which labels analysis authored. It is deliberately
/// local and unsynced — the labels themselves flow through the existing typed
/// label operation, and adding an origin field to that payload would break
/// older nodes, whose deserializer denies unknown fields. Consolidation and
/// forced re-analysis consult this table so they never touch a human's label.
///
/// `asset_frame_packs.representation` plus `source_fingerprint` are what make a
/// stored pack detectably stale: the same recipe over a regenerated proxy does
/// not select the same frames.
/// Migration 15. Spend and outcome accounting for whole analysis runs.
///
/// Per-clip cost already lives in `asset_analyses`, but that cannot answer
/// "what did the last run cost" once clips are re-analyzed, nor distinguish a
/// run that was cancelled from one that finished. The desktop projects the cost
/// of a proposed run from these rows, so a user is told what a batch is likely
/// to spend before it starts rather than after.
///
/// Deliberately local and unsynced, like `analysis_labels`: a run is an event on
/// one node, not a shared project fact.
/// Names the places imported originals are allowed to land, so a destination can
/// be moved without stranding what is in it.
///
/// An archive root is a name the user recognizes ("NAS Video", "Shuttle SSD").
/// Its binding is where that root is mounted on this node right now. Storing an
/// absolute path on the asset instead would recreate the offline defect at
/// archive scale, because an external drive is itself a thing that remounts
/// elsewhere. See `docs/tasks/import/ARCHITECTURE.md` §4.1.
fn add_archive_roots(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS archive_roots (
            archive_root_id TEXT PRIMARY KEY,
            display_name TEXT NOT NULL CHECK(length(display_name) > 0),
            created_at TEXT NOT NULL
        );

        -- One binding per node: where this root is reachable on this machine.
        -- Rebinding is the whole repair when an archive drive moves.
        CREATE TABLE IF NOT EXISTS archive_root_bindings (
            archive_root_id TEXT NOT NULL REFERENCES archive_roots(archive_root_id) ON DELETE CASCADE,
            node_id TEXT NOT NULL,
            local_mount_path TEXT NOT NULL CHECK(length(local_mount_path) > 0),
            bound_at TEXT NOT NULL,
            PRIMARY KEY(archive_root_id, node_id)
        );
        "#,
    )?;
    // A location inside an archive root is stored as root plus relative path.
    // `local_path` stays populated as the resolved absolute path so every
    // existing reader is unchanged, but it is derived, not authoritative.
    for (column, definition) in [("archive_root_id", "TEXT"), ("root_relative_path", "TEXT")] {
        add_column_if_missing(
            transaction,
            "project_asset_local_locations",
            column,
            definition,
        )?;
    }
    Ok(())
}

/// Durable, project-local evidence for a smart-import run. Asset ids are kept
/// as plain text in the ledger deliberately: undoing membership must preserve
/// the audit record after the asset row (and its project-only derivatives) is
/// gone.
/// Records that a filename-counter restart is *not* a card change.
///
/// Split proposals are derived from the facts on every solve, so without a
/// durable "no" the same question comes back forever and the only way to
/// silence it is to accept a split the user does not want. Swapping several
/// cards through one camera is ordinary practice, and so is a camera that
/// simply rolls its counter over.
fn add_declined_capture_sequence_splits(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS declined_capture_sequence_splits (
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            capture_sequence_id TEXT NOT NULL,
            at_counter INTEGER NOT NULL,
            reason TEXT NOT NULL DEFAULT '',
            declined_at TEXT NOT NULL,
            PRIMARY KEY(project_id, capture_sequence_id, at_counter)
        );
        CREATE INDEX IF NOT EXISTS declined_splits_by_sequence
            ON declined_capture_sequence_splits(capture_sequence_id);
        "#,
    )?;
    Ok(())
}

fn add_import_history(transaction: &Transaction<'_>) -> Result<()> {
    add_column_if_missing(
        transaction,
        "assets",
        "membership_removed",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS import_sessions (
            session_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            source_path TEXT NOT NULL,
            destination_mode TEXT NOT NULL CHECK(destination_mode IN ('reference', 'archive_copy')),
            destination_root_id TEXT,
            destination_name TEXT NOT NULL DEFAULT '',
            destination_path TEXT NOT NULL DEFAULT '',
            imported_count INTEGER NOT NULL,
            relinked_count INTEGER NOT NULL,
            skipped_count INTEGER NOT NULL,
            copied_count INTEGER NOT NULL,
            bytes_copied INTEGER NOT NULL,
            cancelled INTEGER NOT NULL DEFAULT 0,
            failures_json TEXT NOT NULL DEFAULT '[]',
            rolled_back_at TEXT,
            rolled_back_asset_count INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS import_sessions_project_created
            ON import_sessions(project_id, created_at DESC);

        CREATE TABLE IF NOT EXISTS import_session_files (
            session_id TEXT NOT NULL REFERENCES import_sessions(session_id) ON DELETE CASCADE,
            ordinal INTEGER NOT NULL,
            asset_id TEXT,
            path TEXT NOT NULL,
            filename TEXT NOT NULL,
            classification TEXT NOT NULL,
            result TEXT NOT NULL,
            timestamp TEXT,
            duration_seconds REAL,
            error TEXT,
            PRIMARY KEY(session_id, ordinal)
        );
        CREATE INDEX IF NOT EXISTS import_session_files_asset
            ON import_session_files(session_id, asset_id);
        "#,
    )?;
    Ok(())
}

/// Durable per-project camera-identity evidence (#71): a corrected camera
/// name, keyed on a card's folder layout plus its filename prefix rather than
/// its mount path or volume label, so a reformatted or swapped card from the
/// same physical camera can default to the identity the user already chose.
/// See `cameras::mapping` for the evidence-strength rule and how import
/// preflight and asset ingest both consume it so the two cannot disagree.
fn add_camera_identity_mappings(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS camera_identity_mappings (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            folder_hint TEXT NOT NULL CHECK(length(folder_hint) > 0),
            filename_prefix TEXT NOT NULL CHECK(length(filename_prefix) > 0),
            camera TEXT NOT NULL CHECK(length(camera) > 0),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(project_id, folder_hint, filename_prefix)
        );
        CREATE INDEX IF NOT EXISTS camera_identity_mappings_project
            ON camera_identity_mappings(project_id);
        "#,
    )?;
    Ok(())
}

/// A reused card carries footage from before the project started, and the
/// user needs to say so once rather than on every reinsertion (#66).
///
/// Keyed on the *card session* (`cards::card_session_slug`'s identity), not on
/// a mount path and not on camera evidence. A mount path is transient. Camera
/// evidence is shared by every card that camera ever wrote, so keying on it
/// would silently hide the start of a brand-new card — the opposite of what
/// the user asked for. A card session survives remounts and reinsertion,
/// changes on reformat (so a reformatted card correctly starts clean), and is
/// preserved when more footage is shot onto a partly imported card.
///
/// The boundary itself is the card-relative path of the clip the user pointed
/// at, which is stable wherever the volume mounts, plus the filename and
/// best-available time so the rule can always name the clip that set it. A
/// persistent filter must never be invisible.
fn add_import_cutoffs(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS import_cutoffs (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            card_session_id TEXT NOT NULL CHECK(length(card_session_id) > 0),
            card_label TEXT NOT NULL,
            relative_path TEXT NOT NULL CHECK(length(relative_path) > 0),
            filename TEXT NOT NULL,
            captured_at TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(project_id, card_session_id)
        );
        CREATE INDEX IF NOT EXISTS import_cutoffs_project
            ON import_cutoffs(project_id);
        "#,
    )?;
    Ok(())
}

/// Gives volumes an identity of their own, so a remembered root path stops
/// meaning "whatever is mounted here right now".
///
/// The unique indexes carry the honesty rule. A volume that reports a UUID is
/// unique *by that UUID*, so it is the same volume wherever it mounts. A volume
/// that reports none is keyed by its path instead — enough to stay one row
/// rather than growing without bound, and deliberately not enough to prove
/// identity, which is why `volume_identity_is_verifiable` gates on the UUID
/// rather than on the row existing.
fn add_volume_identity(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS volumes (
            volume_id TEXT PRIMARY KEY,
            volume_uuid TEXT,
            label TEXT NOT NULL DEFAULT '',
            last_mount_path TEXT NOT NULL DEFAULT '',
            removable INTEGER NOT NULL DEFAULT 0,
            last_card_session_id TEXT,
            first_seen_at TEXT NOT NULL,
            last_seen_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS volumes_by_uuid
            ON volumes(volume_uuid) WHERE volume_uuid IS NOT NULL;
        CREATE UNIQUE INDEX IF NOT EXISTS volumes_unidentified_by_path
            ON volumes(last_mount_path) WHERE volume_uuid IS NULL;
        "#,
    )?;
    add_column_if_missing(transaction, "archive_root_bindings", "volume_id", "TEXT")?;
    add_column_if_missing(
        transaction,
        "project_asset_local_locations",
        "volume_id",
        "TEXT",
    )?;
    Ok(())
}

/// Persist explicit video validation without rewriting asset facts or operation history.
/// Existing rows intentionally begin unverified because migration cannot inspect media.
fn add_asset_video_validation(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS asset_video_validation (
            asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
            fingerprint TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('usable', 'quarantined')),
            diagnostic TEXT,
            validated_at TEXT NOT NULL,
            CHECK((status = 'usable' AND diagnostic IS NULL) OR
                  (status = 'quarantined' AND diagnostic IS NOT NULL))
        );
        CREATE INDEX IF NOT EXISTS asset_video_validation_status
            ON asset_video_validation(status);
        "#,
    )?;
    Ok(())
}

/// Adds the representative EXIF make/model to the sticky camera evidence key.
///
/// Migration 21 is intentionally kept byte-for-byte compatible with the first
/// committed #71 schema. A short-lived development revision nevertheless
/// created the four-column schema while still recording migration 21. Rebuild
/// from either shape here so both honest v21 catalogs and those development
/// catalogs advance safely to the same v22 schema without losing mappings.
fn add_camera_identity_mapping_exif_evidence(transaction: &Transaction<'_>) -> Result<()> {
    let already_has_exif = column_exists(transaction, "camera_identity_mappings", "exif_hint")?;
    transaction.execute_batch(
        r#"
        DROP TABLE IF EXISTS camera_identity_mappings_v22;
        CREATE TABLE camera_identity_mappings_v22 (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            folder_hint TEXT NOT NULL CHECK(length(folder_hint) > 0),
            filename_prefix TEXT NOT NULL CHECK(length(filename_prefix) > 0),
            exif_hint TEXT NOT NULL DEFAULT '',
            camera TEXT NOT NULL CHECK(length(camera) > 0),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(project_id, folder_hint, filename_prefix, exif_hint)
        );
        "#,
    )?;
    if already_has_exif {
        transaction.execute_batch(
            r#"
            INSERT INTO camera_identity_mappings_v22
                (id, project_id, folder_hint, filename_prefix, exif_hint, camera, created_at, updated_at)
            SELECT id, project_id, folder_hint, filename_prefix, exif_hint, camera, created_at, updated_at
            FROM camera_identity_mappings;
            "#,
        )?;
    } else {
        transaction.execute_batch(
            r#"
            INSERT INTO camera_identity_mappings_v22
                (id, project_id, folder_hint, filename_prefix, exif_hint, camera, created_at, updated_at)
            SELECT id, project_id, folder_hint, filename_prefix, '', camera, created_at, updated_at
            FROM camera_identity_mappings;
            "#,
        )?;
    }
    transaction.execute_batch(
        r#"
        DROP TABLE camera_identity_mappings;
        ALTER TABLE camera_identity_mappings_v22 RENAME TO camera_identity_mappings;
        CREATE INDEX camera_identity_mappings_project
            ON camera_identity_mappings(project_id);
        "#,
    )?;
    Ok(())
}

fn add_camera_identity_aliases(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS camera_identity_aliases (
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            source_camera TEXT NOT NULL,
            destination_camera TEXT NOT NULL,
            operation_id TEXT NOT NULL,
            hlc_wall_millis INTEGER NOT NULL,
            hlc_counter INTEGER NOT NULL,
            origin_node_id TEXT NOT NULL,
            PRIMARY KEY(project_id, source_camera)
        );

        CREATE INDEX IF NOT EXISTS camera_identity_aliases_destination
            ON camera_identity_aliases(project_id, destination_camera);

        CREATE TABLE IF NOT EXISTS camera_identity_merge_facts (
            operation_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            source_camera TEXT NOT NULL,
            destination_camera TEXT NOT NULL,
            hlc_wall_millis INTEGER NOT NULL,
            hlc_counter INTEGER NOT NULL,
            origin_node_id TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS camera_identity_merge_facts_project
            ON camera_identity_merge_facts(project_id);
        "#,
    )?;
    Ok(())
}

/// Gives a node-local media location a verification state, so "the original is
/// available" can stop meaning "some file exists at that pathname".
///
/// Two defects share that root cause. A card that reappears at a different mount
/// point was reported as a duplicate and the asset was left pointing at the old
/// offline path, and a reformatted card that reuses a pathname could make an
/// asset resolve to entirely different bytes. See
/// `docs/tasks/import/ARCHITECTURE.md` §3.2.
///
/// The primary key also relaxes to include the path, because an asset can
/// legitimately be reachable at more than one place on one node — a mounted card
/// and the archive copy taken from it.
fn add_verified_asset_locations(transaction: &Transaction<'_>) -> Result<()> {
    // Replayable: dropping `schema_migrations` re-runs this against a schema that
    // may already carry the rebuilt table.
    if column_exists(
        transaction,
        "project_asset_local_locations",
        "verification_state",
    )? {
        return Ok(());
    }
    // A table rebuild rather than ALTER, because the primary key changes. Nothing
    // references this table, so the rename cannot orphan a foreign key.
    transaction.execute_batch(
        r#"
        CREATE TABLE project_asset_local_locations_v2 (
            asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            node_id TEXT NOT NULL,
            local_path TEXT NOT NULL,
            created_at TEXT NOT NULL,
            verification_state TEXT NOT NULL DEFAULT 'unverified'
                CHECK(verification_state IN ('unverified', 'provisional', 'verified', 'mismatch')),
            verified_fingerprint TEXT,
            verified_size_bytes INTEGER CHECK(verified_size_bytes IS NULL OR verified_size_bytes >= 0),
            verified_mtime_millis INTEGER,
            verified_at TEXT,
            PRIMARY KEY(asset_id, node_id, local_path)
        );

        -- Existing rows are trusted enough to keep but not enough to call
        -- verified: nothing ever checked their bytes. They verify lazily on
        -- first use.
        INSERT INTO project_asset_local_locations_v2(
            asset_id, node_id, local_path, created_at, verification_state
        )
        SELECT asset_id, node_id, local_path, created_at, 'unverified'
        FROM project_asset_local_locations;

        DROP TABLE project_asset_local_locations;
        ALTER TABLE project_asset_local_locations_v2
            RENAME TO project_asset_local_locations;

        CREATE INDEX IF NOT EXISTS project_asset_local_locations_asset_state
            ON project_asset_local_locations(asset_id, verification_state);
        "#,
    )?;
    Ok(())
}

fn add_analysis_runs(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS analysis_runs (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            started_at TEXT NOT NULL,
            finished_at TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL CHECK(status IN ('running', 'completed', 'cancelled', 'failed')),
            stage_reached TEXT NOT NULL DEFAULT 'proxies',
            proxies_generated INTEGER NOT NULL DEFAULT 0,
            clips_transcribed INTEGER NOT NULL DEFAULT 0,
            clips_analyzed INTEGER NOT NULL DEFAULT 0,
            cost_usd REAL NOT NULL DEFAULT 0,
            budget_exhausted INTEGER NOT NULL DEFAULT 0,
            error TEXT NOT NULL DEFAULT ''
        );

        CREATE INDEX IF NOT EXISTS analysis_runs_project_started
            ON analysis_runs(project_id, started_at DESC);
        "#,
    )?;
    Ok(())
}

/// Migration 16. A phase can now retain the clips the user chose as its rough
/// start and end evidence. The existing timestamp columns remain required so
/// every older phase stays valid and an anchored phase has a durable fallback
/// if one of its anchors temporarily cannot be placed.
fn add_clip_anchored_trip_phases(transaction: &Transaction<'_>) -> Result<()> {
    add_column_if_missing(
        transaction,
        "trip_phases",
        "start_anchor_asset_id",
        "TEXT REFERENCES assets(id) ON DELETE RESTRICT",
    )?;
    add_column_if_missing(
        transaction,
        "trip_phases",
        "end_anchor_asset_id",
        "TEXT REFERENCES assets(id) ON DELETE RESTRICT",
    )?;
    Ok(())
}

fn add_clip_analysis(transaction: &Transaction<'_>) -> Result<()> {
    // Every migration must be replayable: dropping `schema_migrations` re-runs
    // the whole list against a schema that may already have these columns, and
    // `ADD COLUMN` is not idempotent the way `CREATE TABLE IF NOT EXISTS` is.
    for column in ["title", "description"] {
        if !column_exists(transaction, "assets", column)? {
            transaction.execute_batch(&format!(
                "ALTER TABLE assets ADD COLUMN {column} TEXT NOT NULL DEFAULT '';"
            ))?;
        }
    }
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS asset_frame_packs (
            asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
            recipe_key TEXT NOT NULL,
            representation TEXT NOT NULL,
            source_fingerprint TEXT NOT NULL,
            manifest_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS asset_analyses (
            asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
            recipe_key TEXT NOT NULL,
            prompt_version TEXT NOT NULL,
            provider TEXT NOT NULL,
            model TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('ok', 'rejected', 'failed')),
            reject_reason TEXT NOT NULL DEFAULT '',
            usable INTEGER NOT NULL DEFAULT 1,
            unusable_reason TEXT NOT NULL DEFAULT '',
            best_moment_start_ms INTEGER,
            best_moment_end_ms INTEGER,
            thumbnail_timestamp_ms INTEGER,
            camera_movement TEXT NOT NULL DEFAULT 'other',
            ocr_json TEXT NOT NULL DEFAULT '[]',
            transcript_used INTEGER NOT NULL DEFAULT 0,
            prompt_tokens INTEGER NOT NULL DEFAULT 0,
            completion_tokens INTEGER NOT NULL DEFAULT 0,
            cost_usd REAL NOT NULL DEFAULT 0,
            latency_ms INTEGER NOT NULL DEFAULT 0,
            attempts INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS analysis_labels (
            asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            normalized_label TEXT NOT NULL,
            display_label TEXT NOT NULL,
            proposed_label TEXT NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY(asset_id, normalized_label)
        );

        CREATE TABLE IF NOT EXISTS label_vocabulary (
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            normalized_label TEXT NOT NULL,
            display_label TEXT NOT NULL,
            usage_count INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            PRIMARY KEY(project_id, normalized_label)
        );
        "#,
    )?;
    Ok(())
}

fn backfill_pattern_filename_counters(transaction: &Transaction<'_>) -> Result<()> {
    let pending: Vec<(String, String)> = {
        let mut statement = transaction.prepare(
            "SELECT acs.asset_id, a.filename
             FROM asset_capture_sequence acs
             JOIN assets a ON a.id = acs.asset_id
             WHERE acs.filename_counter IS NULL
             ORDER BY acs.asset_id",
        )?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<(String, String)>>>()?
    };
    for (asset_id, filename) in pending {
        let Some(counter) = crate::cameras::pattern_counter(&filename) else {
            continue;
        };
        transaction.execute(
            "UPDATE asset_capture_sequence SET filename_counter = ?2 WHERE asset_id = ?1",
            rusqlite::params![asset_id, counter as i64],
        )?;
    }
    Ok(())
}

fn add_diagnostics(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS diagnostics (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            dedupe_key TEXT NOT NULL,
            severity TEXT NOT NULL CHECK(severity IN ('error', 'warning', 'info')),
            kind TEXT NOT NULL,
            subject_type TEXT NOT NULL,
            subject_id TEXT NOT NULL DEFAULT '',
            message TEXT NOT NULL,
            context_json TEXT NOT NULL DEFAULT '{}',
            occurrence_count INTEGER NOT NULL DEFAULT 1,
            first_seen_at TEXT NOT NULL,
            last_seen_at TEXT NOT NULL,
            acknowledged_at TEXT,
            UNIQUE(project_id, dedupe_key)
        );

        CREATE INDEX IF NOT EXISTS diagnostics_project_unacknowledged
        ON diagnostics(project_id, acknowledged_at, last_seen_at);
        "#,
    )?;
    Ok(())
}

fn add_chronology_facts(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS capture_sequences (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            camera_key TEXT NOT NULL,
            label TEXT NOT NULL,
            volume_hint TEXT NOT NULL DEFAULT '',
            filename_prefix TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS asset_capture_sequence (
            asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            capture_sequence_id TEXT NOT NULL REFERENCES capture_sequences(id) ON DELETE CASCADE,
            filename_counter INTEGER
        );

        CREATE TABLE IF NOT EXISTS clock_epochs (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            capture_sequence_id TEXT NOT NULL REFERENCES capture_sequences(id) ON DELETE CASCADE,
            start_counter INTEGER NOT NULL,
            end_counter INTEGER,
            offset_seconds INTEGER NOT NULL,
            confidence REAL NOT NULL DEFAULT 1.0,
            evidence_json TEXT NOT NULL DEFAULT '[]',
            label TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS clip_time_exceptions (
            asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            trip_time TEXT NOT NULL,
            reason TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS trip_phases (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            label TEXT NOT NULL,
            phase_kind TEXT NOT NULL CHECK(phase_kind IN ('location', 'transit')),
            start_trip_time TEXT NOT NULL,
            end_trip_time TEXT NOT NULL,
            timezone TEXT,
            origin_timezone TEXT,
            destination_timezone TEXT,
            display_mode TEXT NOT NULL CHECK(display_mode IN ('local', 'dual', 'utc')),
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS event_order_constraints (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            earlier_asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            later_asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            strength TEXT NOT NULL CHECK(strength IN ('hard', 'soft')),
            source TEXT NOT NULL,
            created_at TEXT NOT NULL,
            UNIQUE(project_id, earlier_asset_id, later_asset_id)
        );

        CREATE INDEX IF NOT EXISTS clock_epochs_sequence_range
        ON clock_epochs(project_id, capture_sequence_id, start_counter);

        CREATE INDEX IF NOT EXISTS trip_phases_project_range
        ON trip_phases(project_id, start_trip_time);
        "#,
    )?;
    let mut projects = transaction.prepare("SELECT id FROM projects ORDER BY id")?;
    let project_ids = projects
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(projects);
    for project_id in project_ids {
        forward_migrate_project_chronology(transaction, &project_id)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;

    #[test]
    fn migration_13_creates_transcript_tables() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrate_project_catalog(&mut connection).unwrap();

        let transcript_columns: Vec<String> = connection
            .prepare("SELECT name FROM pragma_table_info('asset_transcripts')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap();
        assert!(transcript_columns.contains(&"recipe_key".to_string()));
        assert!(transcript_columns.contains(&"source_fingerprint".to_string()));
        assert!(transcript_columns.contains(&"status".to_string()));

        let segment_columns: Vec<String> = connection
            .prepare("SELECT name FROM pragma_table_info('transcript_segments')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap();
        assert!(segment_columns.contains(&"start_ms".to_string()));
        assert!(segment_columns.contains(&"mean_token_probability".to_string()));
    }

    #[test]
    fn migration_14_adds_analysis_tables_and_ai_owned_columns() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrate_project_catalog(&mut connection).unwrap();
        for table in [
            "asset_frame_packs",
            "asset_analyses",
            "analysis_labels",
            "label_vocabulary",
        ] {
            let count: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "missing table {table}");
        }
        let asset_columns: Vec<String> = connection
            .prepare("SELECT name FROM pragma_table_info('assets')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap();
        assert!(asset_columns.contains(&"title".to_string()));
        assert!(asset_columns.contains(&"description".to_string()));
    }

    #[test]
    fn migration_15_adds_analysis_run_accounting() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrate_project_catalog(&mut connection).unwrap();
        let columns: Vec<String> = connection
            .prepare("SELECT name FROM pragma_table_info('analysis_runs')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap();
        for column in ["status", "stage_reached", "cost_usd", "budget_exhausted"] {
            assert!(columns.contains(&column.to_string()), "missing {column}");
        }
    }

    #[test]
    fn migration_21_fixture_upgrades_forward_to_exif_evidence() {
        let mut connection = Connection::open_in_memory().unwrap();
        run_sqlite_migrations(&mut connection, &PROJECT_CATALOG_MIGRATIONS[..21]).unwrap();
        assert!(!column_exists(&connection, "camera_identity_mappings", "exif_hint").unwrap());
        connection
            .execute(
                "INSERT INTO projects (id, name, created_at)
                 VALUES ('p1', 'p', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO camera_identity_mappings
                 (id, project_id, folder_hint, filename_prefix, camera, created_at, updated_at)
                 VALUES ('m1', 'p1', 'Sony', 'C', 'Sony A7C II', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();

        migrate_project_catalog(&mut connection).unwrap();
        let columns: Vec<String> = connection
            .prepare("SELECT name FROM pragma_table_info('camera_identity_mappings')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap();
        for column in [
            "id",
            "project_id",
            "folder_hint",
            "filename_prefix",
            "exif_hint",
            "camera",
            "created_at",
            "updated_at",
        ] {
            assert!(columns.contains(&column.to_string()), "missing {column}");
        }

        let preserved: (String, String) = connection
            .query_row(
                "SELECT camera, exif_hint FROM camera_identity_mappings WHERE id='m1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(preserved, ("Sony A7C II".to_string(), String::new()));
        // Weak evidence must be rejected at the schema level too, not only in
        // the Rust layer above it.
        assert!(
            connection
                .execute(
                    "INSERT INTO camera_identity_mappings
                     (id, project_id, folder_hint, filename_prefix, exif_hint, camera, created_at, updated_at)
                     VALUES ('m2', 'p1', '', 'C', '', 'Sony A7C II', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                    [],
                )
                .is_err()
        );
        // The same evidence signature (including exif_hint) cannot be
        // recorded twice for one project...
        assert!(
            connection
                .execute(
                    "INSERT INTO camera_identity_mappings
                     (id, project_id, folder_hint, filename_prefix, exif_hint, camera, created_at, updated_at)
                     VALUES ('m3', 'p1', 'Sony', 'C', '', 'Something else', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                    [],
                )
                .is_err()
        );
        // ...but a distinct exif_hint is a distinct row, even with the same
        // folder_hint/filename_prefix — the narrowing `cameras::mapping`
        // documents.
        connection
            .execute(
                "INSERT INTO camera_identity_mappings
                 (id, project_id, folder_hint, filename_prefix, exif_hint, camera, created_at, updated_at)
                 VALUES ('m4', 'p1', 'Sony', 'C', 'SONY ILME-FX3', 'Sony FX3', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
    }

    #[test]
    fn migration_22_accepts_the_short_lived_buggy_v21_shape() {
        let mut connection = Connection::open_in_memory().unwrap();
        run_sqlite_migrations(&mut connection, &PROJECT_CATALOG_MIGRATIONS[..20]).unwrap();
        connection
            .execute_batch(
                r#"
                CREATE TABLE camera_identity_mappings (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                    folder_hint TEXT NOT NULL CHECK(length(folder_hint) > 0),
                    filename_prefix TEXT NOT NULL CHECK(length(filename_prefix) > 0),
                    exif_hint TEXT NOT NULL DEFAULT '',
                    camera TEXT NOT NULL CHECK(length(camera) > 0),
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    UNIQUE(project_id, folder_hint, filename_prefix, exif_hint)
                );
                CREATE INDEX camera_identity_mappings_project
                    ON camera_identity_mappings(project_id);
                INSERT INTO projects (id, name, created_at)
                    VALUES ('p_buggy', 'p', '2026-01-01T00:00:00Z');
                INSERT INTO camera_identity_mappings
                    (id, project_id, folder_hint, filename_prefix, exif_hint, camera, created_at, updated_at)
                    VALUES ('m_buggy', 'p_buggy', 'Sony', 'C', 'SONY ILCE-7C', 'Sony A7C II', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
                INSERT INTO schema_migrations (migration_id, name, source_identity, applied_at)
                    VALUES (21, 'camera_identity_mappings', 'project-catalog/0021-camera-identity-mappings/20260819', '2026-08-19T00:00:00Z');
                "#,
            )
            .unwrap();

        migrate_project_catalog(&mut connection).unwrap();
        let preserved: (String, String) = connection
            .query_row(
                "SELECT camera, exif_hint FROM camera_identity_mappings WHERE id='m_buggy'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            preserved,
            ("Sony A7C II".to_string(), "SONY ILCE-7C".to_string())
        );
        assert_eq!(
            inspect_project_catalog_migrations(&connection)
                .unwrap()
                .current_version,
            PROJECT_CATALOG_SCHEMA_VERSION
        );
    }

    #[test]
    fn migration_23_upgrades_v22_catalogs_with_durable_camera_aliases() {
        let mut connection = Connection::open_in_memory().unwrap();
        run_sqlite_migrations(&mut connection, &PROJECT_CATALOG_MIGRATIONS[..22]).unwrap();
        assert!(!table_exists(&connection, "camera_identity_aliases").unwrap());
        assert!(!table_exists(&connection, "camera_identity_merge_facts").unwrap());

        migrate_project_catalog(&mut connection).unwrap();

        assert!(table_exists(&connection, "camera_identity_aliases").unwrap());
        assert!(table_exists(&connection, "camera_identity_merge_facts").unwrap());
        let columns = connection
            .prepare("SELECT name FROM pragma_table_info('camera_identity_aliases') ORDER BY cid")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            columns,
            [
                "project_id",
                "source_camera",
                "destination_camera",
                "operation_id",
                "hlc_wall_millis",
                "hlc_counter",
                "origin_node_id",
            ]
        );
        let fact_columns = connection
            .prepare(
                "SELECT name FROM pragma_table_info('camera_identity_merge_facts') ORDER BY cid",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            fact_columns,
            [
                "operation_id",
                "project_id",
                "source_camera",
                "destination_camera",
                "hlc_wall_millis",
                "hlc_counter",
                "origin_node_id",
            ]
        );
        assert_eq!(
            inspect_project_catalog_migrations(&connection)
                .unwrap()
                .current_version,
            PROJECT_CATALOG_SCHEMA_VERSION,
        );
    }

    #[test]
    fn the_schema_version_constant_tracks_the_migration_list() {
        assert_eq!(
            PROJECT_CATALOG_SCHEMA_VERSION,
            PROJECT_CATALOG_MIGRATIONS.last().unwrap().id,
            "bump PROJECT_CATALOG_SCHEMA_VERSION when adding a migration"
        );
    }

    #[test]
    fn migration_17_gives_locations_a_verification_state() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrate_project_catalog(&mut connection).unwrap();
        let columns: Vec<String> = connection
            .prepare("SELECT name FROM pragma_table_info('project_asset_local_locations')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap();
        for column in [
            "verification_state",
            "verified_fingerprint",
            "verified_size_bytes",
            "verified_mtime_millis",
            "verified_at",
        ] {
            assert!(columns.contains(&column.to_string()), "missing {column}");
        }
    }

    #[test]
    fn migration_17_keeps_existing_locations_and_admits_they_are_unverified() {
        // A catalog stopped one migration short, carrying a location row written
        // before verification existed.
        let mut connection = Connection::open_in_memory().unwrap();
        let previous = &PROJECT_CATALOG_MIGRATIONS[..PROJECT_CATALOG_MIGRATIONS.len() - 1];
        run_sqlite_migrations(&mut connection, previous).unwrap();
        connection
            .execute_batch(
                r#"
                INSERT INTO projects(id, name, created_at)
                VALUES ('p1', 'Legacy', '2026-01-01T00:00:00Z');
                INSERT INTO assets(
                    id, project_id, fingerprint, path, filename, size_bytes,
                    duration_seconds, created_at
                ) VALUES (
                    'a1', 'p1', 'fp', '/Volumes/NAS/C0001.MP4', 'C0001.MP4', 100, 10,
                    '2026-01-01T00:00:00Z'
                );
                INSERT INTO project_asset_local_locations(asset_id, node_id, local_path, created_at)
                VALUES ('a1', 'node_one', '/Volumes/NAS/C0001.MP4', '2026-01-01T00:00:00Z');
                "#,
            )
            .unwrap();

        migrate_project_catalog(&mut connection).unwrap();

        let (path, state): (String, String) = connection
            .query_row(
                "SELECT local_path, verification_state FROM project_asset_local_locations
                 WHERE asset_id = 'a1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(path, "/Volumes/NAS/C0001.MP4");
        assert_eq!(
            state, "unverified",
            "a location nothing ever checked must not claim to be verified"
        );

        // The relaxed primary key is what lets one asset hold a card location and
        // an archive location at the same time.
        connection
            .execute(
                "INSERT INTO project_asset_local_locations(asset_id, node_id, local_path, created_at)
                 VALUES ('a1', 'node_one', '/Volumes/CARD/C0001.MP4', '2026-01-02T00:00:00Z')",
                [],
            )
            .unwrap();
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM project_asset_local_locations WHERE asset_id = 'a1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    /// A catalog stopped at *any* earlier version upgrades to current. Stated as
    /// a property rather than against one named migration, so adding the next
    /// migration does not silently stop exercising the previous ones.
    #[test]
    fn a_catalog_stopped_at_any_earlier_version_upgrades_cleanly() {
        for stopped_after in 0..PROJECT_CATALOG_MIGRATIONS.len() {
            let mut connection = Connection::open_in_memory().unwrap();
            run_sqlite_migrations(
                &mut connection,
                &PROJECT_CATALOG_MIGRATIONS[..stopped_after],
            )
            .unwrap();

            migrate_project_catalog(&mut connection).unwrap();

            validate_project_catalog_migrations(&connection).unwrap();
            let applied: u32 = connection
                .query_row(
                    "SELECT MAX(migration_id) FROM schema_migrations",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                applied, PROJECT_CATALOG_SCHEMA_VERSION,
                "a catalog stopped after {stopped_after} migrations did not reach current"
            );
        }
    }

    #[test]
    fn migration_16_upgrades_a_catalog_stopped_at_the_previous_version() {
        // Replay through migration 15, the way a catalog from before main's
        // clip-anchor change would be, and then upgrade it.
        let mut connection = Connection::open_in_memory().unwrap();
        let previous = &PROJECT_CATALOG_MIGRATIONS[..15];
        run_sqlite_migrations(&mut connection, previous).unwrap();
        let before = column_exists(&connection, "trip_phases", "start_anchor_asset_id").unwrap();
        assert!(!before);

        migrate_project_catalog(&mut connection).unwrap();

        assert!(column_exists(&connection, "trip_phases", "start_anchor_asset_id").unwrap());
        assert!(column_exists(&connection, "trip_phases", "end_anchor_asset_id").unwrap());
        validate_project_catalog_migrations(&connection).unwrap();
    }

    #[test]
    fn divergent_import_branch_history_is_reconciled_without_replaying_applied_work() {
        let mut connection = Connection::open_in_memory().unwrap();
        run_sqlite_migrations(&mut connection, &PROJECT_CATALOG_MIGRATIONS[..15]).unwrap();

        let transaction = connection.transaction().unwrap();
        add_verified_asset_locations(&transaction).unwrap();
        transaction
            .execute(
                "INSERT INTO schema_migrations(migration_id, name, source_identity, applied_at)
                 VALUES (16, 'verified_asset_locations',
                         'project-catalog/0016-verified-asset-locations/20260809',
                         '2026-08-09T00:00:00Z')",
                [],
            )
            .unwrap();
        add_archive_roots(&transaction).unwrap();
        transaction
            .execute(
                "INSERT INTO schema_migrations(migration_id, name, source_identity, applied_at)
                 VALUES (17, 'archive_roots',
                         'project-catalog/0017-archive-roots/20260809',
                         '2026-08-09T00:00:01Z')",
                [],
            )
            .unwrap();
        transaction.commit().unwrap();

        migrate_project_catalog(&mut connection).unwrap();
        validate_project_catalog_migrations(&connection).unwrap();

        let history = load_applied_migrations(&connection).unwrap();
        assert_eq!(history.len(), PROJECT_CATALOG_SCHEMA_VERSION as usize);
        assert_eq!(history[15].name, "clip_anchored_trip_phases");
        assert_eq!(history[16].name, "verified_asset_locations");
        assert_eq!(history[17].name, "archive_roots");
        assert!(column_exists(&connection, "trip_phases", "start_anchor_asset_id").unwrap());
    }

    #[test]
    fn analysis_status_rejects_unknown_value() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrate_project_catalog(&mut connection).unwrap();
        connection
            .execute(
                "INSERT INTO projects (id, name, created_at)
                 VALUES ('p1','p','2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO assets (id, project_id, fingerprint, path, filename, size_bytes,
                 duration_seconds, created_at)
                 VALUES ('a1','p1','f1','/x.mp4','x.mp4',1,1.0,'2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        assert!(
            connection
                .execute(
                    "INSERT INTO asset_analyses (asset_id, recipe_key, prompt_version, provider,
                     model, status, created_at)
                     VALUES ('a1','r','v2','openrouter','m','banana','2026-01-01T00:00:00Z')",
                    [],
                )
                .is_err()
        );
    }

    #[test]
    fn transcript_status_rejects_unknown_value() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrate_project_catalog(&mut connection).unwrap();
        connection
            .execute(
                "INSERT INTO projects (id, name, created_at)
                 VALUES ('p1', 'p', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO assets (id, project_id, fingerprint, path, filename, size_bytes,
                 duration_seconds, created_at)
                 VALUES ('a1', 'p1', 'f1', '/x.mp4', 'x.mp4', 1, 1.0, '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        let bad = connection.execute(
            "INSERT INTO asset_transcripts (asset_id, recipe_key, source_kind, source_fingerprint,
             status, created_at)
             VALUES ('a1', 'r', 'proxy', 'pf', 'banana', '2026-01-01T00:00:00Z')",
            [],
        );
        assert!(
            bad.is_err(),
            "CHECK constraint should reject unknown status"
        );
    }

    fn first_test_migration(transaction: &Transaction<'_>) -> Result<()> {
        transaction.execute_batch(
            "CREATE TABLE retained (id INTEGER PRIMARY KEY, value TEXT NOT NULL); INSERT INTO retained (id, value) VALUES (1, 'before');",
        )?;
        Ok(())
    }

    fn failing_test_migration(transaction: &Transaction<'_>) -> Result<()> {
        transaction.execute_batch(
            "CREATE TABLE rolled_back (id INTEGER PRIMARY KEY); INSERT INTO retained (id, value) VALUES (2, 'during');",
        )?;
        bail!("deliberate migration failure")
    }

    #[test]
    fn a_failed_migration_rolls_back_its_schema_and_data_changes() {
        let mut connection = Connection::open_in_memory().unwrap();
        let migrations = [
            SqliteMigration {
                id: 1,
                name: "retained",
                source_identity: "test/1",
                apply: first_test_migration,
            },
            SqliteMigration {
                id: 2,
                name: "fails",
                source_identity: "test/2",
                apply: failing_test_migration,
            },
        ];
        let error = run_sqlite_migrations(&mut connection, &migrations).unwrap_err();
        assert!(format!("{error:#}").contains("deliberate migration failure"));
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM retained", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(!table_exists(&connection, "rolled_back").unwrap());
        assert_eq!(
            connection
                .query_row(
                    "SELECT MAX(migration_id) FROM schema_migrations",
                    [],
                    |row| { row.get::<_, Option<u32>>(0) }
                )
                .unwrap(),
            Some(1)
        );
    }

    #[test]
    fn migration_definitions_require_unique_names_and_source_identities() {
        for (second_name, second_source, expected) in [
            ("first", "test/2", "duplicate migration name"),
            ("second", "test/1", "duplicate migration source identity"),
        ] {
            let connection = Connection::open_in_memory().unwrap();
            let migrations = [
                SqliteMigration {
                    id: 1,
                    name: "first",
                    source_identity: "test/1",
                    apply: first_test_migration,
                },
                SqliteMigration {
                    id: 2,
                    name: second_name,
                    source_identity: second_source,
                    apply: failing_test_migration,
                },
            ];
            let error = inspect_sqlite_migrations(&connection, &migrations).unwrap_err();
            assert!(format!("{error:#}").contains(expected));
            assert!(!table_exists(&connection, "schema_migrations").unwrap());
        }
    }
}
