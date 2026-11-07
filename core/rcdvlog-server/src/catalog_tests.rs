use super::*;
use crate::ServerConfig;
use rusqlite::Connection;
use std::collections::BTreeMap;
use tempfile::TempDir;

fn config(root: &TempDir) -> ServerConfig {
    let data = root.path().join("state");
    let derived = root.path().join("derived");
    let backups = root.path().join("backups");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&derived).unwrap();
    std::fs::create_dir_all(&backups).unwrap();
    ServerConfig::from_values(BTreeMap::from([
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
    .unwrap()
}

#[test]
fn callback_migration_upgrades_schema_ten_and_reopens_without_losing_jobs() {
    let root = TempDir::new().unwrap();
    let config = config(&root);
    let path = config.data_dir.join(CONTROL_DATABASE_FILENAME);
    let mut c = open_writable_connection(&path).unwrap();
    run_sqlite_migrations(&mut c, &CONTROL_CATALOG_MIGRATIONS[..10]).unwrap();
    // Seed a real terminal job using the pre-callback event schema. The
    // engine's new emitter deliberately is not used until after upgrade.
    c.execute("INSERT INTO operational_metadata(key,value,updated_at) VALUES('callback-sentinel','preserved','2026-10-22T00:00:00Z')",[]).unwrap();
    c.execute("INSERT INTO jobs(job_id,job_type,payload_version,payload_json,immutable_identity_json,requirements_json,target_node_id,recipe_key,job_class,idempotency_key,idempotency_identity_hash,enqueue_sequence,state,max_attempts,retry_initial_seconds,retry_max_seconds,available_at,total_work_units,total_items,created_at,updated_at,completed_at) VALUES('job_pre_callbacks','test.v1',1,'{}','{}',?1,'node_old','test.v1','lightweight','old-key',?2,1,'canceled',2,1,2,?3,1,1,?3,?3,?3)",params![r#"{"schemaVersion":1,"requiresOriginal":false,"requiresDerivedWrite":false,"capabilities":[]}"#,"a".repeat(64),"2026-10-22T00:00:00Z"]).unwrap();
    c.execute("INSERT INTO job_events(job_id,event_type,state,details_json,created_at) VALUES('job_pre_callbacks','canceled','canceled','{}','2026-10-22T00:00:00Z')",[]).unwrap();
    drop(c);
    for _ in 0..2 {
        let catalog =
            ControlCatalog::open_or_create(&config.data_dir, &config.diagnostics()).unwrap();
        assert_eq!(
            catalog.inspect_migrations().unwrap().status,
            MigrationHistoryStatus::Current
        );
        let c = open_read_only_connection(catalog.database_path()).unwrap();
        assert_eq!(
            c.query_row(
                "SELECT value FROM operational_metadata WHERE key='callback-sentinel'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "preserved"
        );
        assert_eq!(
            c.query_row(
                "SELECT state FROM jobs WHERE job_id='job_pre_callbacks'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "canceled"
        );
        let callbacks = catalog.jobs().callbacks();
        callbacks
            .subscribe(
                &crate::JobId::parse("job_pre_callbacks").unwrap(),
                "upgraded-editor",
            )
            .unwrap();
        let page = callbacks.list("upgraded-editor", 10, None).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].event.state, crate::JobState::Canceled);
    }
}

#[test]
fn first_start_migrates_and_restart_preserves_node_identity() {
    let root = TempDir::new().unwrap();
    let config = config(&root);
    let first = ControlCatalog::open_or_create(&config.data_dir, &config.diagnostics()).unwrap();
    let first_node_id = first.node_id().clone();
    drop(first);

    let second = ControlCatalog::open_or_create(&config.data_dir, &config.diagnostics()).unwrap();
    assert_eq!(second.node_id(), &first_node_id);
    assert_eq!(
        second.inspect_migrations().unwrap().status,
        MigrationHistoryStatus::Current
    );

    let connection = Connection::open(second.database_path()).unwrap();
    let migration_count: u32 = connection
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    let revision_count: u32 = connection
        .query_row("SELECT COUNT(*) FROM configuration_revisions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(migration_count, CONTROL_CATALOG_SCHEMA_VERSION);
    assert_eq!(revision_count, 1);
}

#[test]
fn only_one_instance_lock_can_be_held_for_a_data_directory() {
    let root = TempDir::new().unwrap();
    let first = ServerInstanceLock::acquire(root.path()).unwrap();
    let error = ServerInstanceLock::acquire(root.path()).unwrap_err();
    assert_eq!(
        error.downcast_ref::<InstanceLockError>().unwrap().code,
        "server_already_running"
    );
    drop(first);
    ServerInstanceLock::acquire(root.path()).unwrap();
}

#[test]
fn control_catalog_rejects_newer_migration_history_without_changing_it() {
    let root = TempDir::new().unwrap();
    let config = config(&root);
    let catalog = ControlCatalog::open_or_create(&config.data_dir, &config.diagnostics()).unwrap();
    let connection = Connection::open(catalog.database_path()).unwrap();
    connection
        .execute(
            r#"
                INSERT INTO schema_migrations (
                    migration_id, name, source_identity, applied_at
                ) VALUES (?1, 'future', 'server-control/future', '2026-07-20T00:00:00Z')
                "#,
            [CONTROL_CATALOG_SCHEMA_VERSION + 1],
        )
        .unwrap();
    drop(connection);

    let inspection = inspect_control_catalog_migrations(catalog.database_path()).unwrap();
    assert_eq!(inspection.status, MigrationHistoryStatus::NewerUnsupported);
    let error =
        ControlCatalog::open_or_create(&config.data_dir, &config.diagnostics()).unwrap_err();
    assert!(format!("{error:#}").contains("newer than supported"));
}

#[test]
fn scan_migration_upgrade_preserves_existing_rows() {
    let root = TempDir::new().unwrap();
    let database_path = root.path().join(CONTROL_DATABASE_FILENAME);
    let mut connection = open_writable_connection(&database_path).unwrap();
    // Run migrations 1-3
    run_sqlite_migrations(&mut connection, &CONTROL_CATALOG_MIGRATIONS[..3]).unwrap();
    // Insert existing data
    connection
        .execute(
            "INSERT INTO server_identity(singleton_id, node_id, created_at) VALUES (1, ?1, ?2)",
            params!["node_server_preserved", "2026-07-20T00:00:00Z"],
        )
        .unwrap();
    connection.execute(
        "INSERT INTO project_registry(project_id, replica_relative_path, state, registered_at, created_at, updated_at) VALUES (?1, ?2, 'registered', ?3, ?3, ?3)",
        params!["project_preserved", "projects/project_preserved/catalog.sqlite3", "2026-07-20T00:00:00Z"],
    ).unwrap();
    connection.execute(
        "INSERT INTO storage_roots(storage_root_id, display_name, immutable_original, created_at, updated_at) VALUES ('root_1', 'Root 1', 1, '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z')",
        [],
    ).unwrap();
    // Enqueue a job using params
    let reqs = r#"{"schema_version":1,"requires_original":false,"requires_derived_write":false,"capabilities":[]}"#;
    connection.execute(
        "INSERT INTO jobs(job_id, job_type, payload_version, payload_json, immutable_identity_json, requirements_json, target_node_id, recipe_key, job_class, idempotency_key, idempotency_identity_hash, enqueue_sequence, state, max_attempts, retry_initial_seconds, retry_max_seconds, available_at, total_work_units, total_items, created_at, updated_at) VALUES (?1, 'test.v1', 1, '{}', '{}', ?2, 'node_server_preserved', 'test.v1', 'lightweight', 'key_1', ?3, 1, 'queued', 3, 5, 300, '2026-07-20T00:00:00Z', 10, 1, '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z')",
        params!["job_keep", reqs, "a".repeat(64)],
    ).unwrap();

    // Run migration 4
    run_sqlite_migrations(&mut connection, CONTROL_CATALOG_MIGRATIONS).unwrap();

    // Verify existing rows survive
    let node: String = connection
        .query_row(
            "SELECT node_id FROM server_identity WHERE singleton_id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(node, "node_server_preserved");
    let project: String = connection
        .query_row("SELECT project_id FROM project_registry", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(project, "project_preserved");
    let root: String = connection
        .query_row("SELECT storage_root_id FROM storage_roots", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(root, "root_1");
    let job_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE job_id='job_keep'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(job_count, 1);

    // New tables exist
    connection
        .query_row("SELECT COUNT(*) FROM scan_runs", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    connection
        .query_row("SELECT COUNT(*) FROM scan_schedules", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    connection
        .query_row(
            "SELECT COUNT(*) FROM media_quick_fingerprint_facts",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    connection
        .query_row("SELECT COUNT(*) FROM media_full_hash_facts", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    connection
        .query_row("SELECT COUNT(*) FROM media_probe_facts", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    connection
        .query_row("SELECT COUNT(*) FROM scan_file_diagnostics", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    connection
        .query_row("SELECT COUNT(*) FROM media_mismatch_evidence", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();

    assert_eq!(
        inspect_sqlite_migrations(&connection, CONTROL_CATALOG_MIGRATIONS)
            .unwrap()
            .status,
        MigrationHistoryStatus::Current
    );
}

#[test]
fn storage_library_upgrade_preserves_server_identity_and_project_registry() {
    let root = TempDir::new().unwrap();
    let database_path = root.path().join(CONTROL_DATABASE_FILENAME);
    let mut connection = open_writable_connection(&database_path).unwrap();
    run_sqlite_migrations(&mut connection, &CONTROL_CATALOG_MIGRATIONS[..1]).unwrap();
    connection
        .execute(
            "INSERT INTO server_identity(singleton_id, node_id, created_at) VALUES (1, ?1, ?2)",
            params!["node_server_preserved", "2026-07-20T00:00:00Z"],
        )
        .unwrap();
    connection
        .execute(
            r#"
                INSERT INTO project_registry(
                    project_id, replica_relative_path, state, registered_at, created_at, updated_at
                ) VALUES (?1, ?2, 'registered', ?3, ?3, ?3)
                "#,
            params![
                "project_preserved",
                "projects/project_preserved/catalog.sqlite3",
                "2026-07-20T00:00:00Z"
            ],
        )
        .unwrap();

    run_sqlite_migrations(&mut connection, CONTROL_CATALOG_MIGRATIONS).unwrap();

    let node: String = connection
        .query_row(
            "SELECT node_id FROM server_identity WHERE singleton_id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let project: (String, String, String) = connection
        .query_row(
            "SELECT project_id, replica_relative_path, state FROM project_registry",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(node, "node_server_preserved");
    assert_eq!(
        project,
        (
            "project_preserved".to_string(),
            "projects/project_preserved/catalog.sqlite3".to_string(),
            "registered".to_string()
        )
    );
    assert_eq!(
        inspect_sqlite_migrations(&connection, CONTROL_CATALOG_MIGRATIONS)
            .unwrap()
            .status,
        MigrationHistoryStatus::Current
    );
}

#[test]
fn media_estimator_migration_preserves_legacy_rate_rows_and_adds_context() {
    let root = TempDir::new().unwrap();
    let database_path = root.path().join(CONTROL_DATABASE_FILENAME);
    let mut connection = open_writable_connection(&database_path).unwrap();
    run_sqlite_migrations(&mut connection, &CONTROL_CATALOG_MIGRATIONS[..11]).unwrap();
    connection
        .execute(
            "INSERT INTO job_rate_history(node_id,recipe_key,work_units_per_worker_second,samples,updated_at) VALUES('node_legacy','legacy.recipe',42.0,3,'2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
    run_sqlite_migrations(&mut connection, CONTROL_CATALOG_MIGRATIONS).unwrap();
    let (rate, semantic_recipe, estimator_identity): (f64, Option<String>, Option<String>) =
        connection
            .query_row(
                "SELECT work_units_per_worker_second,semantic_recipe_key,estimator_identity_json FROM job_rate_history WHERE recipe_key='legacy.recipe'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
    assert_eq!(rate, 42.0);
    assert!(semantic_recipe.is_none());
    assert!(estimator_identity.is_none());
    let attempt_column_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('job_attempts') WHERE name IN ('estimator_identity_json','estimator_process_key')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(attempt_column_count, 2);
}

#[test]
fn representation_migration_upgrade_preserves_verified_media_and_jobs() {
    let root = TempDir::new().unwrap();
    let database_path = root.path().join(CONTROL_DATABASE_FILENAME);
    let mut connection = open_writable_connection(&database_path).unwrap();
    run_sqlite_migrations(&mut connection, &CONTROL_CATALOG_MIGRATIONS[..4]).unwrap();
    connection.execute(
        "INSERT INTO media_objects(media_object_id,size_bytes,full_content_hash,verification_state,created_at,updated_at) VALUES('media_preserved',10,?1,'verified',?2,?2)",
        params!["a".repeat(64), "2026-07-23T00:00:00Z"],
    ).unwrap();
    connection
        .execute(
            r#"
            INSERT INTO jobs(
                job_id,job_type,payload_version,payload_json,immutable_identity_json,
                requirements_json,target_node_id,recipe_key,job_class,idempotency_key,
                priority,idempotency_identity_hash,enqueue_sequence,state,attempt_count,
                max_attempts,retry_initial_seconds,retry_max_seconds,available_at,
                total_work_units,total_items,created_at,updated_at
            ) VALUES(
                'job_preserved','fixture.v1',1,'{}','{}',?1,'node_preserved',
                'fixture.v1','lightweight','preserved',0,?2,1,'queued',0,3,1,5,
                ?3,1,1,?3,?3
            )
            "#,
            params![
                serde_json::json!({
                    "schemaVersion": 1,
                    "requiresOriginal": false,
                    "requiresDerivedWrite": false,
                    "capabilities": []
                })
                .to_string(),
                "b".repeat(64),
                "2026-07-23T00:00:00Z"
            ],
        )
        .unwrap();

    run_sqlite_migrations(&mut connection, CONTROL_CATALOG_MIGRATIONS).unwrap();

    assert_eq!(connection.query_row(
        "SELECT verification_state FROM media_objects WHERE media_object_id='media_preserved'",
        [], |row| row.get::<_, String>(0),
    ).unwrap(), "verified");
    assert_eq!(
        connection
            .query_row(
                "SELECT state FROM jobs WHERE job_id='job_preserved'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "queued"
    );
    assert_eq!(
        inspect_sqlite_migrations(&connection, CONTROL_CATALOG_MIGRATIONS)
            .unwrap()
            .status,
        MigrationHistoryStatus::Current
    );
}

#[test]
fn auth_migration_upgrade_preserves_existing_data() {
    let root = TempDir::new().unwrap();
    let database_path = root.path().join(CONTROL_DATABASE_FILENAME);
    let mut connection = open_writable_connection(&database_path).unwrap();
    run_sqlite_migrations(&mut connection, &CONTROL_CATALOG_MIGRATIONS[..6]).unwrap();
    connection
        .execute(
            "INSERT INTO server_identity(singleton_id, node_id, created_at) VALUES (1, ?1, ?2)",
            params!["node_server_auth_upgrade", "2026-07-23T00:00:00Z"],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO project_registry(project_id, replica_relative_path, state, registered_at, created_at, updated_at) VALUES (?1, ?2, 'registered', ?3, ?3, ?3)",
            params!["project_auth_upgrade", "projects/project_auth_upgrade/catalog.sqlite3", "2026-07-23T00:00:00Z"],
        )
        .unwrap();

    run_sqlite_migrations(&mut connection, CONTROL_CATALOG_MIGRATIONS).unwrap();

    let node: String = connection
        .query_row(
            "SELECT node_id FROM server_identity WHERE singleton_id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(node, "node_server_auth_upgrade");

    // New tables exist
    for table in &[
        "bootstrap_codes",
        "browser_sessions",
        "pairing_codes",
        "device_credentials",
    ] {
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
    }

    assert_eq!(
        inspect_sqlite_migrations(&connection, CONTROL_CATALOG_MIGRATIONS)
            .unwrap()
            .status,
        MigrationHistoryStatus::Current
    );
    assert_eq!(
        inspect_sqlite_migrations(&connection, CONTROL_CATALOG_MIGRATIONS)
            .unwrap()
            .applied_migrations
            .len(),
        CONTROL_CATALOG_SCHEMA_VERSION as usize
    );
}

#[test]
fn library_search_index_migration_backfills_existing_media() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("control.sqlite3");
    let mut connection = rusqlite::Connection::open(&path).unwrap();
    run_sqlite_migrations(&mut connection, &CONTROL_CATALOG_MIGRATIONS[..7]).unwrap();

    connection
        .execute_batch(
            r#"
            INSERT INTO storage_roots(storage_root_id, display_name, immutable_original, enabled, created_at, updated_at)
                VALUES ('root_a', 'Main NAS', 1, 1, '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
            INSERT INTO media_objects(media_object_id, size_bytes, quick_fingerprint, full_content_hash, verification_state, created_at, updated_at)
                VALUES ('media_a', 1024, 'quick', NULL, 'provisional', '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
            INSERT INTO storage_objects(storage_object_id, storage_root_id, relative_path, size_bytes, created_at, updated_at)
                VALUES ('object_a', 'root_a', 'Trips/2025/Sony/C0103.MP4', 1024, '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
            INSERT INTO media_object_locations(media_object_id, storage_object_id, verification_state, linked_at, updated_at)
                VALUES ('media_a', 'object_a', 'provisional', '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
            INSERT INTO media_probe_facts(storage_object_id, recipe_key, recipe_version, probe_json, camera_make, camera_model, captured_at)
                VALUES ('object_a', 'probe', '1', '{}', 'Sony', 'ILCE-7M4', '2026-07-20T00:00:00Z');
            "#,
        )
        .unwrap();

    run_sqlite_migrations(&mut connection, CONTROL_CATALOG_MIGRATIONS).unwrap();

    let row: (String, String, String, Option<String>, u32) = connection
        .query_row(
            "SELECT media_object_id, filename, filename_folded, camera_model, storage_location_count
                 FROM media_search_index WHERE media_object_id = 'media_a'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    assert_eq!(row.0, "media_a");
    assert_eq!(row.1, "C0103.MP4");
    assert_eq!(row.2, "c0103.mp4");
    assert_eq!(row.3.as_deref(), Some("ILCE-7M4"));
    assert_eq!(row.4, 1);
}

#[test]
fn library_search_index_migration_handles_backfill_edge_cases() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("control.sqlite3");
    let mut connection = rusqlite::Connection::open(&path).unwrap();
    run_sqlite_migrations(&mut connection, &CONTROL_CATALOG_MIGRATIONS[..7]).unwrap();

    connection
        .execute_batch(
            r#"
            INSERT INTO storage_roots(storage_root_id, display_name, immutable_original, enabled, created_at, updated_at) VALUES
                ('root_b1', 'Root B1', 1, 1, '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z'),
                ('root_b2', 'Root B2', 1, 1, '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');

            -- Case 1: two storage locations for one media object. The
            -- lexicographically lowest storage_object_id must win, and every
            -- derived column (root, filename, camera facts) must come from
            -- that same underlying row (bare-column / MIN resolution).
            INSERT INTO media_objects(media_object_id, size_bytes, quick_fingerprint, full_content_hash, verification_state, created_at, updated_at)
                VALUES ('media_multi', 2048, 'quick_multi', NULL, 'provisional', '2026-07-21T00:00:00Z', '2026-07-21T00:00:00Z');
            INSERT INTO storage_objects(storage_object_id, storage_root_id, relative_path, size_bytes, created_at, updated_at) VALUES
                ('object_b1_low', 'root_b1', 'Trips/2025/Sony/LOW.MP4', 2048, '2026-07-21T00:00:00Z', '2026-07-21T00:00:00Z'),
                ('object_b2_high', 'root_b2', 'Trips/2025/Canon/HIGH.MP4', 2048, '2026-07-21T00:00:00Z', '2026-07-21T00:00:00Z');
            INSERT INTO media_object_locations(media_object_id, storage_object_id, verification_state, linked_at, updated_at) VALUES
                ('media_multi', 'object_b1_low', 'provisional', '2026-07-21T00:00:00Z', '2026-07-21T00:00:00Z'),
                ('media_multi', 'object_b2_high', 'provisional', '2026-07-21T00:00:00Z', '2026-07-21T00:00:00Z');
            INSERT INTO media_probe_facts(storage_object_id, recipe_key, recipe_version, probe_json, camera_make, camera_model, captured_at) VALUES
                ('object_b1_low', 'probe', '1', '{}', 'Sony', 'ILCE-7M4', '2026-07-21T00:00:00Z'),
                ('object_b2_high', 'probe', '1', '{}', 'Canon', 'EOS R5', '2026-07-21T00:00:00Z');

            -- Case 2: relative path with no '/' at all -- filename is the whole path.
            INSERT INTO media_objects(media_object_id, size_bytes, quick_fingerprint, full_content_hash, verification_state, created_at, updated_at)
                VALUES ('media_no_slash', 512, 'quick_no_slash', NULL, 'provisional', '2026-07-22T00:00:00Z', '2026-07-22T00:00:00Z');
            INSERT INTO storage_objects(storage_object_id, storage_root_id, relative_path, size_bytes, created_at, updated_at)
                VALUES ('object_no_slash', 'root_b1', 'C0200.MP4', 512, '2026-07-22T00:00:00Z', '2026-07-22T00:00:00Z');
            INSERT INTO media_object_locations(media_object_id, storage_object_id, verification_state, linked_at, updated_at)
                VALUES ('media_no_slash', 'object_no_slash', 'provisional', '2026-07-22T00:00:00Z', '2026-07-22T00:00:00Z');

            -- Case 3: relative path with repeated separators -- filename is still
            -- just the final path component.
            INSERT INTO media_objects(media_object_id, size_bytes, quick_fingerprint, full_content_hash, verification_state, created_at, updated_at)
                VALUES ('media_repeated_sep', 768, 'quick_repeated_sep', NULL, 'provisional', '2026-07-23T00:00:00Z', '2026-07-23T00:00:00Z');
            INSERT INTO storage_objects(storage_object_id, storage_root_id, relative_path, size_bytes, created_at, updated_at)
                VALUES ('object_repeated_sep', 'root_b1', 'Trips//2025///Sony//C0999.MP4', 768, '2026-07-23T00:00:00Z', '2026-07-23T00:00:00Z');
            INSERT INTO media_object_locations(media_object_id, storage_object_id, verification_state, linked_at, updated_at)
                VALUES ('media_repeated_sep', 'object_repeated_sep', 'provisional', '2026-07-23T00:00:00Z', '2026-07-23T00:00:00Z');

            -- Case 4: media object with no storage location at all.
            INSERT INTO media_objects(media_object_id, size_bytes, quick_fingerprint, full_content_hash, verification_state, created_at, updated_at)
                VALUES ('media_no_location', NULL, 'quick_no_location', NULL, 'unverified', '2026-07-24T00:00:00Z', '2026-07-24T00:00:00Z');
            "#,
        )
        .unwrap();

    run_sqlite_migrations(&mut connection, CONTROL_CATALOG_MIGRATIONS).unwrap();

    // Case 1: bare-column MIN resolution must pick one consistent row.
    let multi: (
        String,
        Option<String>,
        String,
        String,
        Option<String>,
        Option<String>,
        u32,
    ) = connection
        .query_row(
            "SELECT primary_storage_object_id, primary_storage_root_id, filename, filename_folded,
                        camera_make, camera_model, storage_location_count
                 FROM media_search_index WHERE media_object_id = 'media_multi'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(multi.0, "object_b1_low");
    assert_eq!(multi.1.as_deref(), Some("root_b1"));
    assert_eq!(multi.2, "LOW.MP4");
    assert_eq!(multi.3, "low.mp4");
    assert_eq!(multi.4.as_deref(), Some("Sony"));
    assert_eq!(multi.5.as_deref(), Some("ILCE-7M4"));
    assert_eq!(multi.6, 2);

    // Case 2: no separator at all -- filename is the whole path.
    let filename: String = connection
        .query_row(
            "SELECT filename FROM media_search_index WHERE media_object_id = 'media_no_slash'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(filename, "C0200.MP4");

    // Case 3: repeated separators still yield only the final path component.
    let filename: String = connection
        .query_row(
            "SELECT filename FROM media_search_index WHERE media_object_id = 'media_repeated_sep'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(filename, "C0999.MP4");

    // Case 4: media object with no storage location still gets a row.
    let no_location: (String, Option<String>, Option<String>, u32) = connection
        .query_row(
            "SELECT filename, primary_storage_object_id, primary_storage_root_id, storage_location_count
                 FROM media_search_index WHERE media_object_id = 'media_no_location'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(no_location.0, "");
    assert_eq!(no_location.1, None);
    assert_eq!(no_location.2, None);
    assert_eq!(no_location.3, 0);
}

#[test]
fn opening_after_migration_8_reconciles_non_ascii_folding_from_the_backfill_path() {
    use crate::library_query::{LibraryFilters, LibraryQuery, LibrarySort, plan_library_query};

    let root = TempDir::new().unwrap();
    let config = config(&root);
    let database_path = config.data_dir.join(CONTROL_DATABASE_FILENAME);

    // Simulate a pre-existing catalog already at migration 7 (before
    // library_search_index ships) that already has a media object with a
    // non-ASCII filename -- exactly the kind of row migration 8's SQL
    // `LOWER()` backfill folds differently from Rust's `to_lowercase()`.
    {
        let mut connection = Connection::open(&database_path).unwrap();
        run_sqlite_migrations(&mut connection, &CONTROL_CATALOG_MIGRATIONS[..7]).unwrap();
        connection
            .execute_batch(
                r#"
                INSERT INTO storage_roots(storage_root_id, display_name, immutable_original, enabled, created_at, updated_at)
                    VALUES ('root_nonascii', 'Root NonAscii', 1, 1, '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
                INSERT INTO media_objects(media_object_id, size_bytes, quick_fingerprint, full_content_hash, verification_state, created_at, updated_at)
                    VALUES ('media_nonascii', 4096, 'quick_nonascii', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'verified', '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
                INSERT INTO storage_objects(storage_object_id, storage_root_id, relative_path, size_bytes, created_at, updated_at)
                    VALUES ('object_nonascii', 'root_nonascii', 'Trips/2025/ÄRLIG.MP4', 4096, '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
                INSERT INTO media_object_locations(media_object_id, storage_object_id, verification_state, linked_at, updated_at)
                    VALUES ('media_nonascii', 'object_nonascii', 'verified', '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
                "#,
            )
            .unwrap();

        // Documents the premise this whole reconciliation exists for:
        // SQLite's built-in LOWER() is ASCII-only in this build, so it
        // leaves the non-ASCII character untouched.
        let sql_lower: String = connection
            .query_row("SELECT LOWER('ÄRLIG.MP4')", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sql_lower, "Ärlig.mp4");
    }

    // Opening the catalog applies migration 8 (the ASCII-only SQL
    // backfill) and must also trigger the one-time reconciliation pass.
    let catalog = ControlCatalog::open_or_create(&config.data_dir, &config.diagnostics()).unwrap();

    let verify_connection = Connection::open(catalog.database_path()).unwrap();
    let filename_folded: String = verify_connection
        .query_row(
            "SELECT filename_folded FROM media_search_index WHERE media_object_id = 'media_nonascii'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        filename_folded, "ärlig.mp4",
        "reconciliation must fold with Rust's Unicode-aware to_lowercase, \
             not SQL LOWER()'s ASCII-only fold"
    );

    // Prove it end-to-end: a filenamePrefix query for the Rust-folded
    // prefix finds this media object, even though it only ever went
    // through the migration backfill -- never an explicit per-row
    // refresh call.
    let query = LibraryQuery {
        limit: 50,
        sort: LibrarySort::FilenameAsc,
        filters: LibraryFilters {
            filename_prefix: Some("är".to_string()),
            ..LibraryFilters::default()
        },
        after: None,
    };
    let plan = plan_library_query(&query).unwrap();
    let mut statement = verify_connection.prepare(&plan.sql).unwrap();
    let ids: Vec<String> = statement
        .query_map(rusqlite::params_from_iter(plan.bindings.iter()), |row| {
            row.get::<_, String>(0)
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        ids.contains(&"media_nonascii".to_string()),
        "non-ASCII filename must be findable by its Rust-folded prefix \
             after the backfill path, not only after a refresh: {ids:?}"
    );
}

/// Simulates the crash window the fix targets: migration 8 has already
/// applied (so it is no longer "pending") but the process died before
/// `open_or_create` ever recorded the reconciliation marker. A restart
/// must still notice the folding is unreconciled and retry it, rather
/// than reading "migration 8 not pending" and skipping forever.
#[test]
fn a_restart_after_a_crashed_reconciliation_retries_it_instead_of_skipping_it() {
    let root = TempDir::new().unwrap();
    let config = config(&root);
    let database_path = config.data_dir.join(CONTROL_DATABASE_FILENAME);

    {
        // Apply every migration directly, bypassing `open_or_create`
        // entirely, so migration 8's own ASCII-only SQL backfill runs
        // but no marker is ever written -- exactly what a crash between
        // "migration 8 committed" and "reconciliation marker written"
        // would leave behind.
        let mut connection = Connection::open(&database_path).unwrap();
        run_sqlite_migrations(&mut connection, &CONTROL_CATALOG_MIGRATIONS[..7]).unwrap();
        connection
            .execute_batch(
                r#"
                INSERT INTO storage_roots(storage_root_id, display_name, immutable_original, enabled, created_at, updated_at)
                    VALUES ('root_crash', 'Root Crash', 1, 1, '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
                INSERT INTO media_objects(media_object_id, size_bytes, quick_fingerprint, full_content_hash, verification_state, created_at, updated_at)
                    VALUES ('media_crash', 4096, 'quick_crash', 'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc', 'verified', '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
                INSERT INTO storage_objects(storage_object_id, storage_root_id, relative_path, size_bytes, created_at, updated_at)
                    VALUES ('object_crash', 'root_crash', 'Trips/2025/ÖVNING.MP4', 4096, '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
                INSERT INTO media_object_locations(media_object_id, storage_object_id, verification_state, linked_at, updated_at)
                    VALUES ('media_crash', 'object_crash', 'verified', '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
                "#,
            )
            .unwrap();

        // Migration 8 runs its ASCII-only SQL backfill here. No marker
        // is written by this raw path -- only `open_or_create` writes it.
        run_sqlite_migrations(&mut connection, CONTROL_CATALOG_MIGRATIONS).unwrap();

        let unreconciled: String = connection
            .query_row(
                "SELECT filename_folded FROM media_search_index WHERE media_object_id = 'media_crash'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            unreconciled, "Övning.mp4",
            "premise check: the raw ASCII-only backfill must leave the \
                 folding wrong before the simulated restart"
        );

        let marker_present = library_search_index_reconciliation_done(&connection).unwrap();
        assert!(
            !marker_present,
            "premise check: the crashed state must not carry the marker"
        );
    }

    // A normal restart through the real API must detect the missing
    // marker (independent of migration 8's pending status, which is now
    // false) and retry the reconciliation.
    let catalog = ControlCatalog::open_or_create(&config.data_dir, &config.diagnostics()).unwrap();

    let verify_connection = Connection::open(catalog.database_path()).unwrap();
    let filename_folded: String = verify_connection
        .query_row(
            "SELECT filename_folded FROM media_search_index WHERE media_object_id = 'media_crash'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        filename_folded, "övning.mp4",
        "the retried reconciliation must fold with Rust's to_lowercase"
    );
    assert!(
        library_search_index_reconciliation_done(&verify_connection).unwrap(),
        "a successful reconciliation must leave the durable marker behind"
    );
}

/// The marker must make reconciliation a one-time cost. Proven here by
/// corrupting a folded value by hand after the marker is set and
/// confirming a second normal open leaves it untouched -- if the second
/// open silently rebuilt the whole index again, this corruption would be
/// erased.
#[test]
fn a_second_open_does_not_rebuild_the_index_again() {
    let root = TempDir::new().unwrap();
    let config = config(&root);

    let first = ControlCatalog::open_or_create(&config.data_dir, &config.diagnostics()).unwrap();
    assert!(
        library_search_index_reconciliation_done(&Connection::open(first.database_path()).unwrap())
            .unwrap(),
        "the first open must leave the reconciliation marker behind"
    );
    drop(first);

    // Insert a row directly and hand-corrupt its folded column to a
    // sentinel that a rebuild would never produce.
    let connection = Connection::open(database_path_for(&config)).unwrap();
    connection
        .execute_batch(
            r#"
                INSERT INTO media_objects(media_object_id, size_bytes, quick_fingerprint, full_content_hash, verification_state, created_at, updated_at)
                    VALUES ('media_sentinel', 10, 'quick_sentinel', NULL, 'unverified', '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z');
                "#,
        )
        .unwrap();
    connection
        .execute(
            r#"
                INSERT INTO media_search_index (
                    media_object_id, filename, filename_folded, camera_folded,
                    verification_state, created_at, refreshed_at
                ) VALUES ('media_sentinel', 'SENTINEL', 'sentinel-not-rebuilt', '', 'unverified', '2026-07-20T00:00:00Z', '2026-07-20T00:00:00Z')
                "#,
            [],
        )
        .unwrap();
    drop(connection);

    let _second = ControlCatalog::open_or_create(&config.data_dir, &config.diagnostics()).unwrap();

    let verify_connection = Connection::open(database_path_for(&config)).unwrap();
    let folded: String = verify_connection
        .query_row(
            "SELECT filename_folded FROM media_search_index WHERE media_object_id = 'media_sentinel'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        folded, "sentinel-not-rebuilt",
        "a second open must not rebuild the index -- the hand-planted \
             sentinel would be overwritten by `refresh_media_search_row` if it did"
    );
}

fn database_path_for(config: &ServerConfig) -> PathBuf {
    config.data_dir.join(CONTROL_DATABASE_FILENAME)
}
