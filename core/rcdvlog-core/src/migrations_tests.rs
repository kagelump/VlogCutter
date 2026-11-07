use super::*;

/// The migration chain's last entry changes as features land. Tests that
/// exercise a specific migration must stop immediately before that id, not
/// relative to the current tail.
fn migrations_before_id(id: u32) -> &'static [SqliteMigration] {
    let index = PROJECT_CATALOG_MIGRATIONS
        .iter()
        .position(|migration| migration.id == id)
        .unwrap_or_else(|| panic!("migration {id} is not defined"));
    &PROJECT_CATALOG_MIGRATIONS[..index]
}
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
fn migration_40_adds_accounting_marker_and_untracked_evidence() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate_project_catalog(&mut connection).unwrap();
    assert!(column_exists(&connection, "analysis_runs", "accounting_contract").unwrap());
    let table_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
                 WHERE type='table' AND name='analysis_untracked_usage_evidence'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(table_count, 1);
    let trigger_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
                 WHERE type='trigger' AND name='analysis_untracked_usage_append_only'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(trigger_count, 1);
}

#[test]
fn migration_41_preserves_v40_segment_rows_and_makes_probability_nullable() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();
    run_sqlite_migrations(&mut connection, migrations_before_id(41)).unwrap();
    connection
        .execute_batch(
            r#"
                INSERT INTO projects(id, name, created_at)
                VALUES ('p1', 'p', '2026-01-01T00:00:00Z');
                INSERT INTO assets(
                    id, project_id, fingerprint, path, filename, size_bytes,
                    duration_seconds, created_at
                ) VALUES (
                    'a1', 'p1', 'fp', '/clip.mp4', 'clip.mp4', 1, 2.0,
                    '2026-01-01T00:00:00Z'
                );
                INSERT INTO asset_transcripts(
                    asset_id, recipe_key, source_kind, source_fingerprint, status,
                    outcome_reason, reject_reason, primary_language, speech_seconds,
                    window_count, provider_segment_count, mean_token_probability,
                    repetition_ratio, created_at
                ) VALUES (
                    'a1', 'stt.test.v1', 'proxy', 'proxy-fp', 'speech', 'accepted',
                    '', 'en', 2.0, 1, 2, 0.365, 0.0, '2026-01-01T00:00:00Z'
                );
                INSERT INTO transcript_segments(
                    asset_id, segment_index, start_ms, end_ms, language, text,
                    mean_token_probability
                ) VALUES
                    ('a1', 0, 0, 1000, 'en', 'legacy zero', 0.0),
                    ('a1', 1, 1000, 2000, 'en', 'legacy known', 0.73);
                "#,
        )
        .unwrap();

    migrate_project_catalog(&mut connection).unwrap();
    validate_project_catalog_migrations(&connection).unwrap();

    let probability_column: (String, i64, Option<String>) = connection
        .query_row(
            r#"SELECT name, "notnull", dflt_value
                   FROM pragma_table_info('transcript_segments')
                   WHERE name = 'mean_token_probability'"#,
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        probability_column,
        ("mean_token_probability".to_string(), 0, None)
    );

    let preserved: Vec<(String, i64, String, Option<f64>)> = connection
        .prepare(
            "SELECT asset_id, segment_index, text, mean_token_probability
                 FROM transcript_segments ORDER BY segment_index",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        preserved,
        vec![
            ("a1".to_string(), 0, "legacy zero".to_string(), Some(0.0)),
            ("a1".to_string(), 1, "legacy known".to_string(), Some(0.73)),
        ]
    );

    let foreign_key: (String, String, String, String) = connection
        .query_row(
            r#"SELECT "table", "from", "to", on_delete
                   FROM pragma_foreign_key_list('transcript_segments')"#,
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        foreign_key,
        (
            "assets".to_string(),
            "asset_id".to_string(),
            "id".to_string(),
            "CASCADE".to_string()
        )
    );
    let index_sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master
                 WHERE type = 'index' AND name = 'transcript_segments_time'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(index_sql.contains("ON transcript_segments(asset_id, start_ms)"));

    connection
        .execute(
            "INSERT INTO transcript_segments(
                    asset_id, segment_index, start_ms, end_ms, language, text,
                    mean_token_probability
                ) VALUES ('a1', 2, 2000, 3000, 'en', 'unknown', NULL)",
            [],
        )
        .unwrap();
    let unknown: Option<f64> = connection
        .query_row(
            "SELECT mean_token_probability FROM transcript_segments
                 WHERE asset_id = 'a1' AND segment_index = 2",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unknown, None);

    connection
        .execute("DELETE FROM assets WHERE id = 'a1'", [])
        .unwrap();
    let remaining: i64 = connection
        .query_row("SELECT COUNT(*) FROM transcript_segments", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(remaining, 0, "the rebuilt child FK must still cascade");
}

#[test]
fn migration_42_preserves_v41_transcript_history_and_admits_review_required() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();
    run_sqlite_migrations(&mut connection, migrations_before_id(42)).unwrap();
    connection
        .execute_batch(
            r#"
                INSERT INTO projects(id, name, created_at)
                VALUES ('p1', 'p', '2026-01-01T00:00:00Z');
                INSERT INTO assets(
                    id, project_id, fingerprint, path, filename, size_bytes,
                    duration_seconds, created_at
                ) VALUES
                    ('a1','p1','f1','/a1.mp4','a1.mp4',1,2.0,'2026-01-01T00:00:00Z'),
                    ('a2','p1','f2','/a2.mp4','a2.mp4',1,3.0,'2026-01-01T00:00:00Z'),
                    ('a3','p1','f3','/a3.mp4','a3.mp4',1,4.0,'2026-01-01T00:00:00Z'),
                    ('a4','p1','f4','/a4.mp4','a4.mp4',1,5.0,'2026-01-01T00:00:00Z'),
                    ('a5','p1','f5','/a5.mp4','a5.mp4',1,6.0,'2026-01-01T00:00:00Z'),
                    ('a6','p1','f6','/a6.mp4','a6.mp4',1,7.0,'2026-01-01T00:00:00Z'),
                    ('a7','p1','f7','/a7.mp4','a7.mp4',1,8.0,'2026-01-01T00:00:00Z');
                "#,
        )
        .unwrap();

    let cases = [
        (
            "a1",
            "speech",
            "accepted",
            "",
            "en",
            2.0_f64,
            1_i64,
            2_i64,
            Some(0.365_f64),
            Some(0.0_f64),
            "2026-01-01T00:00:01Z",
            "proxy",
        ),
        (
            "a2",
            "no_speech",
            "legacy_no_speech_unverified",
            "legacy",
            "",
            0.0,
            0,
            0,
            None,
            None,
            "2026-01-01T00:00:02Z",
            "proxy",
        ),
        (
            "a3",
            "no_audio",
            "no_audio_stream",
            "",
            "",
            0.0,
            0,
            0,
            None,
            None,
            "2026-01-01T00:00:03Z",
            "camera_sidecar",
        ),
        (
            "a4",
            "no_accepted_speech",
            "empty_provider_output",
            "",
            "ja",
            3.5,
            2,
            4,
            None,
            Some(0.25),
            "2026-01-01T00:00:04Z",
            "proxy",
        ),
        (
            "a5",
            "rejected",
            "quality_rejected",
            "repetition",
            "en",
            4.0,
            1,
            3,
            Some(0.91),
            Some(0.75),
            "2026-01-01T00:00:05Z",
            "proxy",
        ),
        (
            "a6",
            "failed",
            "processing_failed",
            "probe failed",
            "",
            0.0,
            0,
            0,
            None,
            None,
            "2026-01-01T00:00:06Z",
            "proxy",
        ),
    ];
    for case in cases {
        connection
            .execute(
                "INSERT INTO asset_transcripts(
                        asset_id, recipe_key, source_kind, source_fingerprint, status,
                        outcome_reason, reject_reason, primary_language, speech_seconds,
                        window_count, provider_segment_count, mean_token_probability,
                        repetition_ratio, created_at
                    ) VALUES (?1, 'stt.test.v1', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    case.0,
                    case.11,
                    format!("fingerprint-{}", case.0),
                    case.1,
                    case.2,
                    case.3,
                    case.4,
                    case.5,
                    case.6,
                    case.7,
                    case.8,
                    case.9,
                    case.10,
                ],
            )
            .unwrap();
    }
    connection
        .execute_batch(
            r#"
                INSERT INTO transcript_segments(
                    asset_id, segment_index, start_ms, end_ms, language, text,
                    mean_token_probability
                ) VALUES
                    ('a1', 0, 0, 1000, 'en', 'legacy zero', 0.0),
                    ('a1', 1, 1000, 2000, 'en', 'legacy unknown', NULL),
                    ('a2', 0, 0, 500, 'en', 'no speech candidate', 0.42);
                "#,
        )
        .unwrap();

    migrate_project_catalog(&mut connection).unwrap();
    validate_project_catalog_migrations(&connection).unwrap();

    let header_columns: Vec<String> = connection
        .prepare("SELECT name FROM pragma_table_info('asset_transcripts')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        header_columns,
        vec![
            "asset_id",
            "recipe_key",
            "source_kind",
            "source_fingerprint",
            "status",
            "outcome_reason",
            "reject_reason",
            "primary_language",
            "speech_seconds",
            "window_count",
            "provider_segment_count",
            "mean_token_probability",
            "repetition_ratio",
            "created_at",
            "native_policy_id",
            "native_quality_recipe_version",
            "native_window_plan_json",
            "native_window_plan_fingerprint",
        ]
    );

    #[derive(Debug, PartialEq)]
    struct HeaderRow {
        asset_id: String,
        recipe_key: String,
        source_kind: String,
        source_fingerprint: String,
        status: String,
        outcome_reason: String,
        reject_reason: String,
        primary_language: String,
        speech_seconds: f64,
        window_count: i64,
        provider_segment_count: i64,
        mean_token_probability: Option<f64>,
        repetition_ratio: Option<f64>,
        created_at: String,
    }
    let preserved: Vec<HeaderRow> = connection
        .prepare(
            "SELECT asset_id, recipe_key, source_kind, source_fingerprint, status,
                        outcome_reason, reject_reason, primary_language, speech_seconds,
                        window_count, provider_segment_count, mean_token_probability,
                        repetition_ratio, created_at
                 FROM asset_transcripts ORDER BY asset_id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok(HeaderRow {
                asset_id: row.get(0)?,
                recipe_key: row.get(1)?,
                source_kind: row.get(2)?,
                source_fingerprint: row.get(3)?,
                status: row.get(4)?,
                outcome_reason: row.get(5)?,
                reject_reason: row.get(6)?,
                primary_language: row.get(7)?,
                speech_seconds: row.get(8)?,
                window_count: row.get(9)?,
                provider_segment_count: row.get(10)?,
                mean_token_probability: row.get(11)?,
                repetition_ratio: row.get(12)?,
                created_at: row.get(13)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let expected: Vec<HeaderRow> = cases
        .iter()
        .map(|case| HeaderRow {
            asset_id: case.0.to_string(),
            recipe_key: "stt.test.v1".to_string(),
            source_kind: case.11.to_string(),
            source_fingerprint: format!("fingerprint-{}", case.0),
            status: case.1.to_string(),
            outcome_reason: case.2.to_string(),
            reject_reason: case.3.to_string(),
            primary_language: case.4.to_string(),
            speech_seconds: case.5,
            window_count: case.6,
            provider_segment_count: case.7,
            mean_token_probability: case.8,
            repetition_ratio: case.9,
            created_at: case.10.to_string(),
        })
        .collect();
    assert_eq!(preserved, expected);

    let preserved_segments: Vec<(String, i64, String, Option<f64>)> = connection
        .prepare(
            "SELECT asset_id, segment_index, text, mean_token_probability
                 FROM transcript_segments ORDER BY asset_id, segment_index",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        preserved_segments,
        vec![
            ("a1".to_string(), 0, "legacy zero".to_string(), Some(0.0)),
            ("a1".to_string(), 1, "legacy unknown".to_string(), None),
            (
                "a2".to_string(),
                0,
                "no speech candidate".to_string(),
                Some(0.42)
            ),
        ]
    );

    let primary_key: (i64, String) = connection
        .query_row(
            "SELECT pk, name FROM pragma_table_info('asset_transcripts')
                 WHERE name = 'asset_id'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(primary_key, (1, "asset_id".to_string()));
    let header_foreign_key: (String, String, String, String) = connection
        .query_row(
            "SELECT \"table\", \"from\", \"to\", on_delete
                 FROM pragma_foreign_key_list('asset_transcripts')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        header_foreign_key,
        (
            "assets".to_string(),
            "asset_id".to_string(),
            "id".to_string(),
            "CASCADE".to_string()
        )
    );
    let segment_foreign_key: (String, String, String, String) = connection
        .query_row(
            "SELECT \"table\", \"from\", \"to\", on_delete
                 FROM pragma_foreign_key_list('transcript_segments')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        segment_foreign_key,
        (
            "assets".to_string(),
            "asset_id".to_string(),
            "id".to_string(),
            "CASCADE".to_string()
        )
    );
    let index_sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master
                 WHERE type = 'index' AND name = 'transcript_segments_time'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(index_sql.contains("ON transcript_segments(asset_id, start_ms)"));

    assert!(
        connection
            .execute(
                "INSERT INTO asset_transcripts(
                        asset_id, recipe_key, source_kind, source_fingerprint, status,
                        outcome_reason, created_at
                    ) VALUES ('a7','stt.review.v1','proxy','review-source','banana',
                              'quality_evidence_incomplete','2026-01-01T00:00:07Z')",
                [],
            )
            .is_err(),
        "the rebuilt CHECK constraint must reject a bogus status"
    );
    connection
        .execute(
            "INSERT INTO asset_transcripts(
                    asset_id, recipe_key, source_kind, source_fingerprint, status,
                    outcome_reason, primary_language, speech_seconds, window_count,
                    provider_segment_count, created_at
                ) VALUES ('a7','stt.review.v1','proxy','review-source','review_required',
                          'quality_evidence_incomplete','en',2.0,1,2,
                          '2026-01-01T00:00:07Z')",
            [],
        )
        .unwrap();
    let review: (String, String) = connection
        .query_row(
            "SELECT status, outcome_reason FROM asset_transcripts WHERE asset_id = 'a7'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        review,
        (
            "review_required".to_string(),
            "quality_evidence_incomplete".to_string()
        )
    );

    connection
        .execute("DELETE FROM assets WHERE id = 'a1'", [])
        .unwrap();
    let removed_headers: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM asset_transcripts WHERE asset_id = 'a1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let removed_segments: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM transcript_segments WHERE asset_id = 'a1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(removed_headers, 0, "the rebuilt header FK must cascade");
    assert_eq!(removed_segments, 0, "child segments must still cascade");
    let remaining_segments: i64 = connection
        .query_row("SELECT COUNT(*) FROM transcript_segments", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        remaining_segments, 1,
        "unrelated segment history must remain"
    );
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
fn migration_30_backfills_available_attempt_provenance_conservatively() {
    let mut connection = Connection::open_in_memory().unwrap();
    run_sqlite_migrations(&mut connection, &PROJECT_CATALOG_MIGRATIONS[..29]).unwrap();
    connection
        .execute(
            "INSERT INTO projects(id,name,created_at) VALUES ('p1','p','2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    for id in ["a-single", "a-retried"] {
        connection
            .execute(
                "INSERT INTO assets(id,project_id,fingerprint,path,filename,size_bytes,duration_seconds,notes,labels_json,created_at) VALUES (?1,'p1',?2,?3,'x.mp4',1,1.0,'human note','[\"human label\"]','2026-01-01T00:00:00Z')",
                params![id, format!("fingerprint-{id}"), format!("/{id}.mp4")],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO asset_analyses(asset_id,recipe_key,prompt_version,provider,model,status,attempts,created_at) VALUES ('a-single','r','v','openrouter','served-model','ok',1,'2026-01-01T00:00:00Z'), ('a-retried','r','v','openrouter','served-model','ok',2,'2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

    migrate_project_catalog(&mut connection).unwrap();
    let single: (String, bool) = connection
        .query_row(
            "SELECT attempted_models_json,usage_complete FROM asset_analyses WHERE asset_id='a-single'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(single, ("[\"served-model\"]".into(), true));
    let retried: (String, bool) = connection
        .query_row(
            "SELECT attempted_models_json,usage_complete FROM asset_analyses WHERE asset_id='a-retried'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(retried, ("[]".into(), false));
    let preserved: Vec<(String, String, String)> = connection
        .prepare("SELECT id,notes,labels_json FROM assets ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        preserved,
        vec![
            (
                "a-retried".into(),
                "human note".into(),
                "[\"human label\"]".into()
            ),
            (
                "a-single".into(),
                "human note".into(),
                "[\"human label\"]".into()
            ),
        ]
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
        .prepare("SELECT name FROM pragma_table_info('camera_identity_merge_facts') ORDER BY cid")
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
fn migration_36_preserves_legacy_rows_and_admits_no_speech_is_unverified() {
    let mut connection = Connection::open_in_memory().unwrap();
    run_sqlite_migrations(&mut connection, migrations_before_id(36)).unwrap();
    connection
        .execute_batch(
            r#"
                INSERT INTO projects(id,name,created_at)
                    VALUES ('p1','project','2026-01-01T00:00:00Z');
                INSERT INTO assets(id,project_id,fingerprint,path,filename,size_bytes,duration_seconds,created_at)
                    VALUES ('a1','p1','fp1','/clip.mp4','clip.mp4',123,9.0,'2026-01-01T00:00:00Z');
                INSERT INTO asset_transcripts
                    (asset_id,recipe_key,source_kind,source_fingerprint,status,
                     reject_reason,primary_language,speech_seconds,window_count,created_at)
                VALUES
                    ('a1','old-recipe','proxy','proxy-fp','no_speech','','',0,0,'before');
                "#,
        )
        .unwrap();

    migrate_project_catalog(&mut connection).unwrap();

    let row = connection
        .query_row(
            "SELECT status, outcome_reason, source_fingerprint FROM asset_transcripts WHERE asset_id='a1'",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(row.0, "no_speech");
    assert_eq!(row.1, "legacy_no_speech_unverified");
    assert_eq!(row.2, "proxy-fp");

    // The migrated schema admits the new typed outcomes.
    for status in ["no_audio", "no_accepted_speech"] {
        connection
            .execute(
                "UPDATE asset_transcripts SET status=?1, outcome_reason='' WHERE asset_id='a1'",
                [status],
            )
            .unwrap();
    }
}

#[test]
fn migration_43_preserves_prior_transcript_rows_and_adds_native_provenance_schema() {
    let mut connection = Connection::open_in_memory().unwrap();
    run_sqlite_migrations(&mut connection, migrations_before_id(43)).unwrap();
    connection
        .execute_batch(
            r#"
                INSERT INTO projects(id,name,created_at)
                    VALUES ('p1','project','2026-01-01T00:00:00Z');
                INSERT INTO assets(id,project_id,fingerprint,path,filename,size_bytes,duration_seconds,created_at)
                    VALUES ('a1','p1','source-fp','/clip.mp4','clip.mp4',123,9.0,'2026-01-01T00:00:00Z');
                INSERT INTO asset_transcripts
                    (asset_id,recipe_key,source_kind,source_fingerprint,status,outcome_reason,
                     reject_reason,primary_language,speech_seconds,window_count,
                     provider_segment_count,mean_token_probability,repetition_ratio,created_at)
                    VALUES ('a1','stt.legacy','proxy','proxy-fp','speech','accepted',
                            '','en',2.5,2,3,0.73,NULL,'2026-01-01T00:00:01Z');
                INSERT INTO transcript_segments
                    (asset_id,segment_index,start_ms,end_ms,language,text,mean_token_probability)
                    VALUES
                    ('a1',0,0,1200,'en','legacy zero',0.0),
                    ('a1',1,1200,2500,'en','legacy known',0.73);
                "#,
        )
        .unwrap();
    let history_before: Vec<(u32, String)> = {
        let mut statement = connection
            .prepare("SELECT migration_id,name FROM schema_migrations ORDER BY migration_id")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };

    migrate_project_catalog(&mut connection).unwrap();

    assert!(column_exists(&connection, "asset_transcripts", "native_policy_id").unwrap());
    assert!(
        column_exists(
            &connection,
            "asset_transcripts",
            "native_quality_recipe_version"
        )
        .unwrap()
    );
    assert!(table_exists(&connection, "native_transcript_windows").unwrap());
    assert!(
        connection
            .execute(
                "UPDATE asset_transcripts SET native_policy_id='only-policy'
                     WHERE asset_id='a1'",
                [],
            )
            .is_err(),
        "native policy and quality recipe associations must be paired"
    );
    #[derive(Debug, PartialEq)]
    struct MigratedHeaderRow {
        recipe_key: String,
        source_kind: String,
        source_fingerprint: String,
        status: String,
        outcome_reason: String,
        speech_seconds: f64,
        window_count: i64,
        provider_segment_count: i64,
        mean_token_probability: f64,
        repetition_ratio: Option<f64>,
        created_at: String,
        native_policy_id: Option<String>,
        native_quality_recipe_version: Option<String>,
    }
    let after: MigratedHeaderRow = connection
        .query_row(
            "SELECT recipe_key,source_kind,source_fingerprint,status,outcome_reason,
                        speech_seconds,window_count,provider_segment_count,
                        mean_token_probability,repetition_ratio,created_at,
                        native_policy_id,native_quality_recipe_version
                 FROM asset_transcripts WHERE asset_id='a1'",
            [],
            |row| {
                Ok(MigratedHeaderRow {
                    recipe_key: row.get(0)?,
                    source_kind: row.get(1)?,
                    source_fingerprint: row.get(2)?,
                    status: row.get(3)?,
                    outcome_reason: row.get(4)?,
                    speech_seconds: row.get(5)?,
                    window_count: row.get(6)?,
                    provider_segment_count: row.get(7)?,
                    mean_token_probability: row.get(8)?,
                    repetition_ratio: row.get(9)?,
                    created_at: row.get(10)?,
                    native_policy_id: row.get(11)?,
                    native_quality_recipe_version: row.get(12)?,
                })
            },
        )
        .unwrap();
    assert_eq!(
        after,
        MigratedHeaderRow {
            recipe_key: "stt.legacy".into(),
            source_kind: "proxy".into(),
            source_fingerprint: "proxy-fp".into(),
            status: "speech".into(),
            outcome_reason: "accepted".into(),
            speech_seconds: 2.5,
            window_count: 2,
            provider_segment_count: 3,
            mean_token_probability: 0.73,
            repetition_ratio: None,
            created_at: "2026-01-01T00:00:01Z".into(),
            native_policy_id: None,
            native_quality_recipe_version: None,
        }
    );
    let segments: Vec<(i64, String, Option<f64>)> = connection
        .prepare(
            "SELECT segment_index,text,mean_token_probability
                 FROM transcript_segments WHERE asset_id='a1' ORDER BY segment_index",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        segments,
        vec![
            (0, "legacy zero".to_string(), Some(0.0)),
            (1, "legacy known".to_string(), Some(0.73)),
        ]
    );

    let history_after: Vec<(u32, String)> = {
        let mut statement = connection
            .prepare("SELECT migration_id,name FROM schema_migrations ORDER BY migration_id")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    assert_eq!(&history_after[..history_before.len()], &history_before[..]);
    assert_eq!(history_after.len(), PROJECT_CATALOG_SCHEMA_VERSION as usize);
    assert_eq!(history_after[history_before.len()].0, 43);
    assert_eq!(
        history_after.last().unwrap().0,
        PROJECT_CATALOG_SCHEMA_VERSION
    );
    let fk_violations: i64 = connection
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(fk_violations, 0);
    let window_columns: Vec<String> = connection
        .prepare("SELECT name FROM pragma_table_info('native_transcript_windows') ORDER BY cid")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        window_columns,
        [
            "asset_id",
            "window_index",
            "request_key",
            "latency_ms",
            "reported_language",
            "quality_json",
        ]
    );
    assert!(
        !window_columns
            .iter()
            .any(|column| matches!(column.as_str(), "text" | "body" | "audio" | "prompt")),
        "native provenance must not duplicate transcript or request bodies"
    );
}

#[test]
fn migration_31_adds_moment_tables_without_changing_existing_assets() {
    let mut connection = Connection::open_in_memory().unwrap();
    run_sqlite_migrations(&mut connection, &PROJECT_CATALOG_MIGRATIONS[..30]).unwrap();
    connection
        .execute_batch(
            "INSERT INTO projects(id,name,created_at) VALUES ('p1','project','before');
                 INSERT INTO assets(id,project_id,fingerprint,path,filename,size_bytes,duration_seconds,created_at)
                 VALUES ('a1','p1','fp1','/clip.mp4','clip.mp4',123,9.0,'before');",
        )
        .unwrap();

    migrate_project_catalog(&mut connection).unwrap();

    assert!(table_exists(&connection, "moment_artifacts").unwrap());
    assert!(table_exists(&connection, "moment_steps").unwrap());
    let retained: (String, String, i64) = connection
        .query_row(
            "SELECT project_id, path, size_bytes FROM assets WHERE id='a1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(retained, ("p1".into(), "/clip.mp4".into(), 123));
    assert_eq!(
        inspect_project_catalog_migrations(&connection)
            .unwrap()
            .current_version,
        PROJECT_CATALOG_SCHEMA_VERSION
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
    let previous = migrations_before_id(17);
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
#[test]
fn migration_44_preserves_pilot_rows_and_admits_no_provider_status() {
    let mut connection = Connection::open_in_memory().unwrap();
    run_sqlite_migrations(&mut connection, &PROJECT_CATALOG_MIGRATIONS[..43]).unwrap();
    connection
        .execute_batch(
            r#"
                INSERT INTO projects(id,name,created_at)
                    VALUES ('p44','project','2026-01-01T00:00:00Z');
                INSERT INTO analysis_run_authorizations(
                    policy_id,project_id,policy_json,authorized_at,accounting_contract)
                    VALUES ('policy44','p44','{}','2026-01-01T00:00:00Z','contract');
                INSERT INTO analysis_run_pilots(
                    policy_id,project_id,status,record_json,started_at,finished_at)
                    VALUES ('policy44','p44','passed','{"legacy":true}',
                            '2026-01-01T00:00:00Z','2026-01-01T00:01:00Z');
                "#,
        )
        .unwrap();
    assert!(
        connection
            .execute(
                "UPDATE analysis_run_pilots SET status='no_provider_required' WHERE policy_id='policy44'",
                [],
            )
            .is_err(),
        "the pre-44 schema must reject the new explicit status before migration"
    );

    migrate_project_catalog(&mut connection).unwrap();
    let preserved: (String, String, String, String) = connection
        .query_row(
            "SELECT project_id,status,record_json,started_at FROM analysis_run_pilots WHERE policy_id='policy44'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        preserved,
        (
            "p44".into(),
            "passed".into(),
            "{\"legacy\":true}".into(),
            "2026-01-01T00:00:00Z".into()
        )
    );
    connection
        .execute(
            "UPDATE analysis_run_pilots SET status='no_provider_required' WHERE policy_id='policy44'",
            [],
        )
        .unwrap();
    let status: String = connection
        .query_row(
            "SELECT status FROM analysis_run_pilots WHERE policy_id='policy44'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "no_provider_required");
    assert!(
        connection
            .execute(
                "UPDATE analysis_run_pilots SET status='invented' WHERE policy_id='policy44'",
                [],
            )
            .is_err()
    );
}

#[test]
fn migration_45_admits_incomplete_native_plan_without_rebuilding_native_children() {
    let mut connection = Connection::open_in_memory().unwrap();
    run_sqlite_migrations(&mut connection, &PROJECT_CATALOG_MIGRATIONS[..44]).unwrap();
    assert!(!column_exists(&connection, "asset_transcripts", "native_window_plan_json").unwrap());
    let native_columns_before: Vec<String> = connection
        .prepare("SELECT name FROM pragma_table_info('native_transcript_windows') ORDER BY cid")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let native_fk_count_before: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_foreign_key_list('native_transcript_windows')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let native_index_count_before: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_index_list('native_transcript_windows')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    connection
        .execute_batch(
            r#"
                INSERT INTO projects(id,name,created_at) VALUES ('p45','project','2026-01-01T00:00:00Z');
                INSERT INTO assets(id,project_id,fingerprint,path,filename,size_bytes,duration_seconds,created_at)
                    VALUES ('a45','p45','source-fp','/clip.mp4','clip.mp4',1,9.0,'2026-01-01T00:00:00Z');
                INSERT INTO assets(id,project_id,fingerprint,path,filename,size_bytes,duration_seconds,created_at)
                    VALUES ('a45-complete','p45','source-fp-2','/clip2.mp4','clip2.mp4',1,9.0,'2026-01-01T00:00:00Z');
                "#,
        )
        .unwrap();

    migrate_project_catalog(&mut connection).unwrap();
    assert!(column_exists(&connection, "asset_transcripts", "native_window_plan_json").unwrap());
    assert!(
        column_exists(
            &connection,
            "asset_transcripts",
            "native_window_plan_fingerprint"
        )
        .unwrap()
    );
    let native_columns_after: Vec<String> = connection
        .prepare("SELECT name FROM pragma_table_info('native_transcript_windows') ORDER BY cid")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(native_columns_after, native_columns_before);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_foreign_key_list('native_transcript_windows')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        native_fk_count_before
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_index_list('native_transcript_windows')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        native_index_count_before
    );

    assert!(
        connection
            .execute(
                "INSERT INTO asset_transcripts
                        (asset_id,recipe_key,source_kind,source_fingerprint,status,outcome_reason,
                         reject_reason,primary_language,speech_seconds,window_count,
                         provider_segment_count,mean_token_probability,repetition_ratio,created_at,
                         native_policy_id,native_quality_recipe_version)
                     VALUES ('a45','native.recipe','proxy','sampled-fp','failed','native_coverage_incomplete',
                             '','en',1.0,0,0,NULL,NULL,'2026-01-01T00:00:01Z',
                             'policy45','native-stt.quality.v1')",
                [],
            )
            .is_err(),
        "incomplete native coverage cannot be written without a positive plan"
    );
    connection
        .execute(
            "INSERT INTO asset_transcripts
                    (asset_id,recipe_key,source_kind,source_fingerprint,status,outcome_reason,
                     reject_reason,primary_language,speech_seconds,window_count,
                     provider_segment_count,mean_token_probability,repetition_ratio,created_at,
                     native_policy_id,native_quality_recipe_version,
                     native_window_plan_json,native_window_plan_fingerprint)
                 VALUES ('a45','native.recipe','proxy','sampled-fp','failed','native_coverage_incomplete',
                         '','en',1.0,0,0,NULL,NULL,'2026-01-01T00:00:01Z',
                         'policy45','native-stt.quality.v1',?,?)",
            rusqlite::params![
                r#"{"schemaVersion":"analysis.native-transcript-window-plan.v1","windows":[{"regions":[{"windowOffsetMs":0,"sourceStartMs":0,"durationMs":1000}]}]}"#,
                "0".repeat(64),
            ],
        )
        .unwrap();
    // An old complete native-equivalent header without a plan stays valid
    // and nullable after migration; no plan or candidates are guessed.
    connection
        .execute(
            "INSERT INTO asset_transcripts
                    (asset_id,recipe_key,source_kind,source_fingerprint,status,outcome_reason,
                     reject_reason,primary_language,speech_seconds,window_count,
                     provider_segment_count,mean_token_probability,repetition_ratio,created_at,
                     native_policy_id,native_quality_recipe_version)
                 VALUES ('a45-complete','native.recipe','proxy','sampled-fp-2','speech','accepted',
                         '','en',1.0,0,0,NULL,NULL,'2026-01-01T00:00:01Z',
                         'policy45','native-stt.quality.v1')",
            [],
        )
        .unwrap();
    let old_complete_plan: (Option<String>, Option<String>) = connection
        .query_row(
            "SELECT native_window_plan_json,native_window_plan_fingerprint
                 FROM asset_transcripts WHERE asset_id='a45-complete'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(old_complete_plan, (None, None));
    assert!(
        connection
            .execute(
                "UPDATE asset_transcripts SET native_window_plan_fingerprint=NULL WHERE asset_id='a45'",
                [],
            )
            .is_err(),
        "plan JSON and semantic fingerprint must be present together"
    );
}

#[test]
fn migration_46_adds_shared_provider_health_without_changing_accounting_rows() {
    let mut connection = Connection::open_in_memory().unwrap();
    run_sqlite_migrations(&mut connection, &PROJECT_CATALOG_MIGRATIONS[..45]).unwrap();
    connection
        .execute_batch(
            "INSERT INTO projects(id,name,created_at) VALUES ('p46','project','2026-01-01T00:00:00Z');",
        )
        .unwrap();
    assert!(!table_exists(&connection, "analysis_provider_health").unwrap());
    migrate_project_catalog(&mut connection).unwrap();
    assert!(table_exists(&connection, "analysis_provider_health").unwrap());
    let columns: Vec<String> = connection
        .prepare("SELECT name FROM pragma_table_info('analysis_provider_health') ORDER BY cid")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        columns,
        [
            "project_id",
            "budget_scope_id",
            "provider",
            "model",
            "consecutive_failures",
            "cooldown_until",
            "last_failure_category",
            "updated_at",
            "revision"
        ]
    );
    let primary_key_columns: Vec<String> = connection
        .prepare(
            "SELECT name FROM pragma_table_info('analysis_provider_health') WHERE pk>0 ORDER BY pk",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        primary_key_columns,
        ["project_id", "budget_scope_id", "provider", "model"]
    );
    assert_eq!(
        inspect_project_catalog_migrations(&connection)
            .unwrap()
            .current_version,
        PROJECT_CATALOG_SCHEMA_VERSION
    );
}

#[test]
fn migration_47_adds_nullable_orientation_decision_without_rewriting_assets() {
    let mut connection = Connection::open_in_memory().unwrap();
    run_sqlite_migrations(&mut connection, &PROJECT_CATALOG_MIGRATIONS[..46]).unwrap();
    connection.execute_batch(
        "INSERT INTO projects(id,name,created_at) VALUES ('p47','project','2026-01-01T00:00:00Z');
             INSERT INTO assets(id,project_id,fingerprint,path,filename,size_bytes,duration_seconds,created_at)
             VALUES ('a47','p47','fp','/offline/a.mp4','a.mp4',7,1,'2026-01-01T00:00:00Z');",
    ).unwrap();
    assert!(!column_exists(&connection, "assets", "orientation_decision_json").unwrap());
    migrate_project_catalog(&mut connection).unwrap();
    assert_eq!(
        connection.query_row("SELECT fingerprint,rotation_degrees,orientation_decision_json FROM assets WHERE id='a47'", [], |row| Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,Option<String>>(2)?))).unwrap(),
        ("fp".to_string(), 0, None),
    );
    assert_eq!(
        inspect_project_catalog_migrations(&connection)
            .unwrap()
            .current_version,
        PROJECT_CATALOG_SCHEMA_VERSION
    );
}

#[test]
fn migration_48_adds_identity_qualified_pipeline_smoke_receipts() {
    let mut connection = Connection::open_in_memory().unwrap();
    run_sqlite_migrations(&mut connection, &PROJECT_CATALOG_MIGRATIONS[..47]).unwrap();
    migrate_project_catalog(&mut connection).unwrap();
    connection.execute(
        "INSERT INTO pipeline_smoke_receipts(cache_key,identity_json,report_json,state,created_at,completed_at) VALUES('key','{}','{}','passed','start','done')",
        [],
    ).unwrap();
    assert_eq!(
        connection.query_row(
            "SELECT identity_json,report_json,state FROM pipeline_smoke_receipts WHERE cache_key='key'",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
        ).unwrap(),
        ("{}".into(), "{}".into(), "passed".into()),
    );
    assert!(connection.execute(
        "INSERT INTO pipeline_smoke_receipts(cache_key,identity_json,report_json,state,created_at,completed_at) VALUES('invalid','{}','{}','running','start','')",
        [],
    ).is_err());
    assert_eq!(
        inspect_project_catalog_migrations(&connection)
            .unwrap()
            .current_version,
        PROJECT_CATALOG_SCHEMA_VERSION,
    );
}
