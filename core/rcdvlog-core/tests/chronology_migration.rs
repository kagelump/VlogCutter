//! D01-T007 acceptance oracle: migration 9 order preservation, the chronology
//! fact model, the eleven operation round-trips, legacy replay, the quarantine
//! policy, loader determinism, and the source-media invariant.
//!
//! The checked-in fixture catalogs under `tests/fixtures/chronology/` are
//! genuine schema-8 catalogs: each was built by running the first eight
//! catalog migrations and inserting rows directly. Regenerate them with:
//!
//! ```sh
//! cargo test -p rcdvlog-core --test chronology_migration regenerate_fixture_catalogs -- --ignored
//! ```

use rcdvlog_core::{
    ApplyOperationResult, CAPTURE_SEQUENCE_DECLARED, CAPTURE_SEQUENCE_MERGED,
    CAPTURE_SEQUENCE_SPLIT, CLIP_TIME_EXCEPTION_CLEARED, CLIP_TIME_EXCEPTION_SET,
    CLOCK_EPOCH_CLEARED, CLOCK_EPOCH_SET, CLOCK_EVENT_CREATED, Catalog, CatalogSchemaError,
    EVENT_ORDER_CONSTRAINT_CLEARED, EVENT_ORDER_CONSTRAINT_SET, HybridLogicalTime,
    OperationBatchResult, PROJECT_CATALOG_MIGRATIONS, ProjectOperation, TRIP_PHASE_REMOVED,
    TRIP_PHASE_SET, full_file_blake3, inspect_catalog, run_sqlite_migrations,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::{TempDir, tempdir};

const FIXTURES_ROOT: &str = "tests/fixtures/chronology";

// ---------------------------------------------------------------------------
// Schema-8 catalog construction (the fixture builder and the temp builders)
// ---------------------------------------------------------------------------

struct FixtureAsset<'a> {
    id: &'a str,
    filename: &'a str,
    camera: &'a str,
    captured_at_raw: &'a str,
    best_time: &'a str,
    offset_seconds: i64,
}

struct FixtureEvent<'a> {
    id: &'a str,
    camera: &'a str,
    start_filename: &'a str,
    offset_seconds: i64,
    label: &'a str,
}

fn insert_schema8_rows(
    connection: &Connection,
    project_id: &str,
    assets: &[FixtureAsset<'_>],
    events: &[FixtureEvent<'_>],
) {
    connection
        .execute(
            "INSERT INTO projects(id, name, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![project_id, "Fixture", "2026-07-20T00:00:00.000Z"],
        )
        .unwrap();
    for asset in assets {
        connection
            .execute(
                r#"
                INSERT INTO assets (
                    id, project_id, fingerprint, path, filename, size_bytes,
                    duration_seconds, frame_rate, width, height, codec, camera,
                    captured_at_raw, best_time, offset_seconds, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, 1000, 10, 30, 1920, 1080,
                          'h264', ?6, ?7, ?8, ?9, '2026-07-20T00:00:00.000Z')
                "#,
                rusqlite::params![
                    asset.id,
                    project_id,
                    format!("fingerprint-{}", asset.id),
                    format!("/offline/fixture/{}", asset.filename),
                    asset.filename,
                    asset.camera,
                    asset.captured_at_raw,
                    asset.best_time,
                    asset.offset_seconds,
                ],
            )
            .unwrap();
    }
    for event in events {
        connection
            .execute(
                "INSERT INTO clock_events(id, project_id, camera, start_filename, offset_seconds, label, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    event.id,
                    project_id,
                    event.camera,
                    event.start_filename,
                    event.offset_seconds,
                    event.label,
                    "2026-07-20T01:00:00.000Z",
                ],
            )
            .unwrap();
    }
}

/// Creates a catalog at exactly schema 8 by running migrations 1-8 on an empty
/// file and inserting rows directly — genuinely old catalogs, not new
/// catalogs described as old.
fn build_schema8_catalog(
    directory: &TempDir,
    name: &str,
    project_id: &str,
    assets: &[FixtureAsset<'_>],
    events: &[FixtureEvent<'_>],
) -> PathBuf {
    let path = directory.path().join(name);
    let mut connection = Connection::open(&path).unwrap();
    run_sqlite_migrations(&mut connection, &PROJECT_CATALOG_MIGRATIONS[..8]).unwrap();
    insert_schema8_rows(&connection, project_id, assets, events);
    drop(connection);
    assert_eq!(inspect_catalog(&path).unwrap().current_schema_version, 8);
    path
}

/// The review order of record (`AGENTS.md` §9): `best_time IS NULL`,
/// `best_time`, `filename`, `id`.
fn ordered_asset_ids(connection: &Connection, project_id: &str) -> Vec<String> {
    let mut statement = connection
        .prepare(
            "SELECT id FROM assets WHERE project_id = ?1 ORDER BY best_time IS NULL, best_time, filename, id",
        )
        .unwrap();
    statement
        .query_map([project_id], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

/// `Catalog::connection` is crate-private, so the tests open their own
/// read-write connection to the catalog file for assertions. SQLite's WAL
/// mode makes concurrent connections safe.
fn open_connection(path: &Path) -> Connection {
    Connection::open(path).unwrap()
}

fn projection_columns(
    connection: &Connection,
    project_id: &str,
) -> Vec<(String, Option<String>, i64)> {
    let mut statement = connection
        .prepare(
            "SELECT id, best_time, offset_seconds FROM assets WHERE project_id = ?1 ORDER BY id",
        )
        .unwrap();
    statement
        .query_map([project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn operation(
    id: &str,
    project_id: &str,
    node: &str,
    sequence: u64,
    wall: i64,
    aggregate_type: &str,
    aggregate_id: &str,
    operation_type: &str,
    base_revision: Option<u64>,
    payload: Value,
) -> ProjectOperation {
    ProjectOperation {
        operation_id: id.to_string(),
        project_id: project_id.to_string(),
        origin_node_id: node.to_string(),
        origin_sequence: sequence,
        logical_time: HybridLogicalTime {
            wall_millis: wall,
            counter: 0,
            node_id: node.to_string(),
        },
        aggregate_type: aggregate_type.to_string(),
        aggregate_id: aggregate_id.to_string(),
        operation_type: operation_type.to_string(),
        payload_version: 1,
        base_revision,
        payload,
    }
}

fn apply_batch(catalog: &Catalog, node: &str, ops: Vec<ProjectOperation>) -> OperationBatchResult {
    let wall = 1_700_000_000_000_i64;
    let ops = ops
        .into_iter()
        .enumerate()
        .map(|(index, mut op)| {
            op.origin_node_id = node.to_string();
            op.origin_sequence = index as u64 + 1;
            op.logical_time = HybridLogicalTime {
                wall_millis: wall + index as i64,
                counter: 0,
                node_id: node.to_string(),
            };
            op
        })
        .collect::<Vec<_>>();
    catalog.apply_operation_batch(node, &ops).unwrap()
}

// ---------------------------------------------------------------------------
// Checked-in fixture shapes (also used by the regeneration test)
// ---------------------------------------------------------------------------

const FIXTURE_FILES: &[(&str, &str)] = &[
    ("one_camera_no_clock.sqlite3", "project_fixture_solo"),
    ("one_camera_one_clock.sqlite3", "project_fixture_single"),
    ("two_cameras_one_clock_each.sqlite3", "project_fixture_two"),
    (
        "two_cameras_three_clock_events.sqlite3",
        "project_fixture_three",
    ),
    (
        "no_digit_run_filenames.sqlite3",
        "project_fixture_no_counters",
    ),
];

fn fixture_one_camera_no_clock() -> (Vec<FixtureAsset<'static>>, Vec<FixtureEvent<'static>>) {
    (
        vec![
            FixtureAsset {
                id: "asset_solo_1",
                filename: "DJI_0001.MP4",
                camera: "DJI Pocket 3",
                captured_at_raw: "2026-07-18T09:00:00Z",
                best_time: "2026-07-18T09:00:00.000Z",
                offset_seconds: 0,
            },
            FixtureAsset {
                id: "asset_solo_2",
                filename: "DJI_0002.MP4",
                camera: "DJI Pocket 3",
                captured_at_raw: "2026-07-18T09:00:10Z",
                best_time: "2026-07-18T09:00:10.000Z",
                offset_seconds: 0,
            },
            FixtureAsset {
                id: "asset_solo_3",
                filename: "DJI_0003.MP4",
                camera: "DJI Pocket 3",
                captured_at_raw: "2026-07-18T09:00:20Z",
                best_time: "2026-07-18T09:00:20.000Z",
                offset_seconds: 0,
            },
            // Deliberate best_time tie: order falls back to filename, then id.
            FixtureAsset {
                id: "asset_solo_4",
                filename: "DJI_0004.MP4",
                camera: "DJI Pocket 3",
                captured_at_raw: "2026-07-18T09:00:20Z",
                best_time: "2026-07-18T09:00:20.000Z",
                offset_seconds: 0,
            },
        ],
        Vec::new(),
    )
}

fn fixture_one_camera_one_clock() -> (Vec<FixtureAsset<'static>>, Vec<FixtureEvent<'static>>) {
    (
        vec![
            FixtureAsset {
                id: "asset_single_1",
                filename: "C0101.MP4",
                camera: "Sony A7C II",
                captured_at_raw: "2026-07-18T10:00:00Z",
                best_time: "2026-07-18T10:00:00.000Z",
                offset_seconds: 0,
            },
            FixtureAsset {
                id: "asset_single_2",
                filename: "C0102.MP4",
                camera: "Sony A7C II",
                captured_at_raw: "2026-07-18T10:00:05Z",
                best_time: "2026-07-18T09:00:05.000Z",
                offset_seconds: -3600,
            },
            FixtureAsset {
                id: "asset_single_3",
                filename: "C0103.MP4",
                camera: "Sony A7C II",
                captured_at_raw: "2026-07-18T10:00:10Z",
                best_time: "2026-07-18T09:00:10.000Z",
                offset_seconds: -3600,
            },
        ],
        vec![FixtureEvent {
            id: "clock_single_1",
            camera: "Sony A7C II",
            start_filename: "C0102.MP4",
            offset_seconds: -3600,
            label: "one hour fast",
        }],
    )
}

fn fixture_two_cameras_one_clock_each() -> (Vec<FixtureAsset<'static>>, Vec<FixtureEvent<'static>>)
{
    (
        vec![
            FixtureAsset {
                id: "asset_two_gp1",
                filename: "GX010001.MP4",
                camera: "GoPro Hero 11",
                captured_at_raw: "2026-07-18T11:00:00Z",
                best_time: "2026-07-18T11:00:00.000Z",
                offset_seconds: 0,
            },
            FixtureAsset {
                id: "asset_two_gp2",
                filename: "GX010002.MP4",
                camera: "GoPro Hero 11",
                captured_at_raw: "2026-07-18T11:00:05Z",
                best_time: "2026-07-18T11:01:05.000Z",
                offset_seconds: 60,
            },
            FixtureAsset {
                id: "asset_two_ip1",
                filename: "IMG_0001.MOV",
                camera: "iPhone 15 Pro",
                captured_at_raw: "2026-07-18T11:30:00Z",
                best_time: "2026-07-18T11:30:00.000Z",
                offset_seconds: 0,
            },
            FixtureAsset {
                id: "asset_two_ip2",
                filename: "IMG_0002.MOV",
                camera: "iPhone 15 Pro",
                captured_at_raw: "2026-07-18T11:30:05Z",
                best_time: "2026-07-18T11:31:05.000Z",
                offset_seconds: 60,
            },
        ],
        vec![
            FixtureEvent {
                id: "clock_two_gp",
                camera: "GoPro Hero 11",
                start_filename: "GX010002.MP4",
                offset_seconds: 60,
                label: "border",
            },
            FixtureEvent {
                id: "clock_two_ip",
                camera: "iPhone 15 Pro",
                start_filename: "IMG_0002.MOV",
                offset_seconds: 60,
                label: "border",
            },
        ],
    )
}

fn fixture_two_cameras_three_clock_events()
-> (Vec<FixtureAsset<'static>>, Vec<FixtureEvent<'static>>) {
    let assets = [
        "asset_three_i1",
        "asset_three_i2",
        "asset_three_i3",
        "asset_three_i4",
        "asset_three_i5",
        "asset_three_i6",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, id)| {
        let seconds = index as i64 * 5;
        let raw = format!("2026-07-18T12:00:{seconds:02}Z");
        let offset: i64 = match index {
            0 | 1 => 60,
            2 | 3 => 120,
            _ => 180,
        };
        let best = format!(
            "2026-07-18T12:0{}:{:02}.000Z",
            (seconds + offset) / 60,
            (seconds + offset) % 60
        );
        FixtureAsset {
            id,
            filename: Box::leak(format!("C{:04}.MP4", index + 1).into_boxed_str()),
            camera: "Insta360 X4",
            captured_at_raw: Box::leak(raw.into_boxed_str()),
            best_time: Box::leak(best.into_boxed_str()),
            offset_seconds: offset,
        }
    })
    .collect::<Vec<_>>();
    let mut all = assets;
    all.push(FixtureAsset {
        id: "asset_three_d1",
        filename: "DJI_0001.MP4",
        camera: "DJI Mini 4 Pro",
        captured_at_raw: "2026-07-18T13:00:00Z",
        best_time: "2026-07-18T13:00:00.000Z",
        offset_seconds: 0,
    });
    all.push(FixtureAsset {
        id: "asset_three_d2",
        filename: "DJI_0002.MP4",
        camera: "DJI Mini 4 Pro",
        captured_at_raw: "2026-07-18T13:00:05Z",
        best_time: "2026-07-18T13:00:05.000Z",
        offset_seconds: 0,
    });
    (
        all,
        vec![
            FixtureEvent {
                id: "clock_three_i1",
                camera: "Insta360 X4",
                start_filename: "C0001.MP4",
                offset_seconds: 60,
                label: "day one",
            },
            FixtureEvent {
                id: "clock_three_i2",
                camera: "Insta360 X4",
                start_filename: "C0003.MP4",
                offset_seconds: 120,
                label: "day two",
            },
            FixtureEvent {
                id: "clock_three_i3",
                camera: "Insta360 X4",
                start_filename: "C0005.MP4",
                offset_seconds: 180,
                label: "day three",
            },
        ],
    )
}

fn fixture_no_digit_run_filenames() -> (Vec<FixtureAsset<'static>>, Vec<FixtureEvent<'static>>) {
    (
        vec![
            FixtureAsset {
                id: "asset_nc_1",
                filename: "clip.mp4",
                camera: "Canon R6",
                captured_at_raw: "2026-07-18T14:00:00Z",
                best_time: "2026-07-18T14:05:00.000Z",
                offset_seconds: 300,
            },
            FixtureAsset {
                id: "asset_nc_2",
                filename: "movie.mov",
                camera: "Canon R6",
                captured_at_raw: "2026-07-18T14:00:10Z",
                best_time: "2026-07-18T14:05:10.000Z",
                offset_seconds: 300,
            },
            FixtureAsset {
                id: "asset_nc_3",
                filename: "footage.MP4",
                camera: "Canon R6",
                captured_at_raw: "2026-07-18T14:00:20Z",
                best_time: "2026-07-18T14:05:20.000Z",
                offset_seconds: 300,
            },
        ],
        vec![FixtureEvent {
            id: "clock_nc_1",
            camera: "Canon R6",
            start_filename: "movie.mov",
            offset_seconds: 300,
            label: "whole camera",
        }],
    )
}

fn fixture_rows(name: &str) -> (Vec<FixtureAsset<'static>>, Vec<FixtureEvent<'static>>) {
    match name {
        "one_camera_no_clock.sqlite3" => fixture_one_camera_no_clock(),
        "one_camera_one_clock.sqlite3" => fixture_one_camera_one_clock(),
        "two_cameras_one_clock_each.sqlite3" => fixture_two_cameras_one_clock_each(),
        "two_cameras_three_clock_events.sqlite3" => fixture_two_cameras_three_clock_events(),
        "no_digit_run_filenames.sqlite3" => fixture_no_digit_run_filenames(),
        other => panic!("unknown fixture shape: {other}"),
    }
}

/// Regenerates the checked-in fixture catalogs and the fixture media file.
/// The builders are deterministic apart from the `created_at` values, which no
/// oracle compares.
#[test]
#[ignore = "regenerates checked-in fixture catalogs"]
fn regenerate_fixture_catalogs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURES_ROOT);
    fs::create_dir_all(root.join("media")).unwrap();
    let media = root.join("media").join("C0001.MP4");
    let bytes = (0..2048u32)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    fs::write(&media, bytes).unwrap();
    for (file, project_id) in FIXTURE_FILES {
        let path = root.join(file);
        let _ = fs::remove_file(&path);
        let mut connection = Connection::open(&path).unwrap();
        run_sqlite_migrations(&mut connection, &PROJECT_CATALOG_MIGRATIONS[..8]).unwrap();
        let (assets, events) = fixture_rows(file);
        insert_schema8_rows(&connection, project_id, &assets, &events);
        drop(connection);
        let inspection = inspect_catalog(&path).unwrap();
        assert_eq!(inspection.current_schema_version, 8);
        assert_eq!(inspection.pending_migrations.len(), 1);
    }
}

// ---------------------------------------------------------------------------
// Oracle 1: order preservation across all five fixture shapes
// ---------------------------------------------------------------------------

#[test]
fn order_is_byte_identical_before_and_after_migration_9_for_every_fixture_shape() {
    let directory = tempdir().unwrap();
    for (file, project_id) in FIXTURE_FILES {
        let path = directory.path().join(file);
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(FIXTURES_ROOT)
                .join(file),
            &path,
        )
        .unwrap();
        let before_connection = Connection::open(&path).unwrap();
        let before = ordered_asset_ids(&before_connection, project_id);
        let before_projections = projection_columns(&before_connection, project_id);
        drop(before_connection);

        let _catalog = Catalog::open_existing(&path).unwrap();
        let connection = open_connection(&path);
        let after = ordered_asset_ids(&connection, project_id);
        let after_projections = projection_columns(&connection, project_id);
        assert_eq!(
            before,
            after,
            "migration 9 reordered {file}: first differing index {}",
            before
                .iter()
                .zip(after.iter())
                .position(|(left, right)| left != right)
                .map_or(before.len().min(after.len()), |index| index)
        );
        assert_eq!(before_projections, after_projections);
        assert_eq!(inspect_catalog(&path).unwrap().current_schema_version, 9);
        eprintln!("{file}: {} assets, order preserved", after.len());
    }
}

// ---------------------------------------------------------------------------
// Oracle 2: counter parsing
// ---------------------------------------------------------------------------

#[test]
fn filename_counter_parsing_matches_the_oracle() {
    let directory = tempdir().unwrap();
    let (assets, events): (Vec<_>, Vec<_>) = (
        vec![
            FixtureAsset {
                id: "asset_counter_1",
                filename: "C0101.MP4",
                camera: "Counter Cam",
                captured_at_raw: "2026-07-18T09:00:00Z",
                best_time: "2026-07-18T09:00:00.000Z",
                offset_seconds: 0,
            },
            FixtureAsset {
                id: "asset_counter_2",
                filename: "DJI_0042.MP4",
                camera: "Counter Cam",
                captured_at_raw: "2026-07-18T09:00:10Z",
                best_time: "2026-07-18T09:00:10.000Z",
                offset_seconds: 0,
            },
            FixtureAsset {
                id: "asset_counter_3",
                filename: "clip.mp4",
                camera: "Counter Cam",
                captured_at_raw: "2026-07-18T09:00:20Z",
                best_time: "2026-07-18T09:00:20.000Z",
                offset_seconds: 0,
            },
            FixtureAsset {
                id: "asset_counter_4",
                filename: "GX010234.MP4",
                camera: "Counter Cam",
                captured_at_raw: "2026-07-18T09:00:30Z",
                best_time: "2026-07-18T09:00:30.000Z",
                offset_seconds: 0,
            },
            FixtureAsset {
                id: "asset_counter_5",
                filename: "IMG_1234 (1).MOV",
                camera: "Counter Cam",
                captured_at_raw: "2026-07-18T09:00:40Z",
                best_time: "2026-07-18T09:00:40.000Z",
                offset_seconds: 0,
            },
            FixtureAsset {
                id: "asset_counter_6",
                filename: "X99999999999999999999999999999.MP4",
                camera: "Counter Cam",
                captured_at_raw: "2026-07-18T09:00:50Z",
                best_time: "2026-07-18T09:00:50.000Z",
                offset_seconds: 0,
            },
        ],
        Vec::new(),
    );
    let path = build_schema8_catalog(
        &directory,
        "counters.sqlite3",
        "project_counters",
        &assets,
        &events,
    );
    let _catalog = Catalog::open_existing(&path).unwrap();
    let connection = open_connection(&path);
    let counters = connection
        .prepare(
            "SELECT acs.filename_counter FROM asset_capture_sequence acs JOIN assets a ON a.id = acs.asset_id WHERE a.project_id = ?1 ORDER BY a.id",
        )
        .unwrap()
        .query_map(["project_counters"], |row| row.get::<_, Option<i64>>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        counters,
        [
            Some(101),
            Some(42),
            None,
            Some(10234),
            Some(1),
            None, // digit run exceeds u64: NULL, no panic
        ]
    );
}

// ---------------------------------------------------------------------------
// Oracle 3 and 4: every asset assigned; clock events carried over
// ---------------------------------------------------------------------------

#[test]
fn every_asset_is_assigned_and_clock_events_carry_over_to_epochs() {
    let directory = tempdir().unwrap();
    for (file, project_id) in FIXTURE_FILES {
        let path = directory.path().join(file);
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(FIXTURES_ROOT)
                .join(file),
            &path,
        )
        .unwrap();
        let before_connection = Connection::open(&path).unwrap();
        let asset_count: i64 = before_connection
            .query_row(
                "SELECT COUNT(*) FROM assets WHERE project_id = ?1",
                [project_id],
                |row| row.get(0),
            )
            .unwrap();
        let event_count: i64 = before_connection
            .query_row(
                "SELECT COUNT(*) FROM clock_events WHERE project_id = ?1",
                [project_id],
                |row| row.get(0),
            )
            .unwrap();
        let legacy_events = before_connection
            .prepare("SELECT id, camera, start_filename, offset_seconds, label FROM clock_events WHERE project_id = ?1 ORDER BY id")
            .unwrap()
            .query_map([project_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        drop(before_connection);

        let _catalog = Catalog::open_existing(&path).unwrap();
        let connection = open_connection(&path);

        let assigned: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM asset_capture_sequence WHERE project_id = ?1",
                [project_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            assigned, asset_count,
            "{file}: every asset must have exactly one asset_capture_sequence row"
        );
        let unassigned: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM assets a WHERE a.project_id = ?1 AND NOT EXISTS (SELECT 1 FROM asset_capture_sequence acs WHERE acs.asset_id = a.id)",
                [project_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unassigned, 0);

        let epoch_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM clock_epochs WHERE project_id = ?1",
                [project_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            epoch_count, event_count,
            "{file}: every clock event carries into an epoch"
        );
        // clock_events is retained, not dropped.
        let retained: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM clock_events WHERE project_id = ?1",
                [project_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(retained, event_count);
        for (event_id, camera, start_filename, offset_seconds, label) in legacy_events {
            let (epoch_sequence, start_counter, epoch_offset, epoch_label, confidence, evidence, end_counter) = connection
                .query_row(
                    "SELECT capture_sequence_id, start_counter, offset_seconds, label, confidence, evidence_json, end_counter FROM clock_epochs WHERE id = ?1",
                    [&event_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, String>(3)?, row.get::<_, f64>(4)?, row.get::<_, String>(5)?, row.get::<_, Option<i64>>(6)?)),
                )
                .unwrap();
            let sequence_camera: String = connection
                .query_row(
                    "SELECT camera_key FROM capture_sequences WHERE id = ?1",
                    [&epoch_sequence],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(sequence_camera, camera);
            let expected_counter = trailing_counter(&start_filename).unwrap_or(0);
            assert_eq!(start_counter, expected_counter as i64);
            assert_eq!(epoch_offset, offset_seconds);
            assert_eq!(epoch_label, label);
            assert_eq!(confidence, 1.0);
            assert_eq!(evidence, r#"["migrated_clock_event"]"#);
            assert_eq!(end_counter, None);
        }
    }
}

/// Mirror of the migration-9 counter rule for test assertions.
fn trailing_counter(filename: &str) -> Option<u64> {
    let stem = match filename.rfind('.') {
        Some(extension) => &filename[..extension],
        None => filename,
    };
    let trimmed = stem.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
    let run_start = trimmed
        .rfind(|c: char| !c.is_ascii_digit())
        .map_or(0, |index| index + 1);
    let run = &trimmed[run_start..];
    if run.is_empty() {
        return None;
    }
    run.parse::<u64>().ok()
}

// ---------------------------------------------------------------------------
// Oracle 5 and 6: idempotence and newer-schema refusal
// ---------------------------------------------------------------------------

#[test]
fn running_the_migration_runner_twice_is_idempotent() {
    let directory = tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(FIXTURES_ROOT)
        .join("two_cameras_three_clock_events.sqlite3");
    let path = directory.path().join("idempotent.sqlite3");
    fs::copy(source, &path).unwrap();

    let catalog = Catalog::open_existing(&path).unwrap();
    let connection = open_connection(&path);
    let order_first = ordered_asset_ids(&connection, "project_fixture_three");
    let counts_first = table_counts(&connection, "project_fixture_three");
    drop(connection);
    drop(catalog);

    let _reopened = Catalog::open_existing(&path).unwrap();
    let connection = open_connection(&path);
    let order_second = ordered_asset_ids(&connection, "project_fixture_three");
    let counts_second = table_counts(&connection, "project_fixture_three");
    let inspection = inspect_catalog(&path).unwrap();
    assert_eq!(inspection.current_schema_version, 9);
    assert_eq!(
        inspection.schema_status,
        rcdvlog_core::MigrationHistoryStatus::Current
    );
    assert!(inspection.pending_migrations.is_empty());
    assert_eq!(order_first, order_second);
    assert_eq!(counts_first, counts_second);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        9
    );
}

fn table_counts(connection: &Connection, project_id: &str) -> BTreeMap<&'static str, i64> {
    [
        "assets",
        "capture_sequences",
        "asset_capture_sequence",
        "clock_epochs",
        "clock_events",
        "clip_time_exceptions",
        "trip_phases",
        "event_order_constraints",
    ]
    .into_iter()
    .map(|table| {
        let count = connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE project_id = ?1"),
                [project_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        (table, count)
    })
    .collect()
}

#[test]
fn a_catalog_at_schema_10_is_refused_as_newer() {
    let directory = tempdir().unwrap();
    let path = build_schema8_catalog(
        &directory,
        "newer.sqlite3",
        "project_newer",
        &[FixtureAsset {
            id: "asset_newer_1",
            filename: "A.MP4",
            camera: "Newer Cam",
            captured_at_raw: "2026-07-18T09:00:00Z",
            best_time: "2026-07-18T09:00:00.000Z",
            offset_seconds: 0,
        }],
        &[],
    );
    let catalog = Catalog::open_existing(&path).unwrap();
    let connection = open_connection(&path);
    connection
        .execute(
            "INSERT INTO schema_migrations (migration_id, name, source_identity, applied_at) VALUES (10, 'future_migration', 'project-catalog/0010-future/20990101', '2099-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
    drop(connection);
    drop(catalog);

    let error = Catalog::open_existing(&path).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<CatalogSchemaError>(),
        Some(CatalogSchemaError::NewerSchema {
            found: 10,
            supported: 9
        })
    ));
    let message = format!("{error:#}");
    assert!(message.contains("newer than supported version 9"));
    assert_eq!(
        inspect_catalog(&path).unwrap().schema_status,
        rcdvlog_core::MigrationHistoryStatus::NewerUnsupported
    );
}

// ---------------------------------------------------------------------------
// Oracle 7: eleven operation round-trips with the revision guard
// ---------------------------------------------------------------------------

fn project_with_asset() -> (TempDir, PathBuf, String) {
    let directory = tempdir().unwrap();
    let path = build_schema8_catalog(
        &directory,
        "roundtrip.sqlite3",
        "project_roundtrip",
        &[
            FixtureAsset {
                id: "asset_rt",
                filename: "C0001.MP4",
                camera: "Roundtrip Cam",
                captured_at_raw: "2026-07-18T09:00:00Z",
                best_time: "2026-07-18T09:00:00.000Z",
                offset_seconds: 0,
            },
            FixtureAsset {
                id: "asset_rt_2",
                filename: "C0002.MP4",
                camera: "Roundtrip Cam",
                captured_at_raw: "2026-07-18T09:00:10Z",
                best_time: "2026-07-18T09:00:10.000Z",
                offset_seconds: 0,
            },
        ],
        &[],
    );
    (directory, path, "project_roundtrip".to_string())
}

fn assert_conflict_recorded(
    connection: &Connection,
    project_id: &str,
    aggregate_type: &str,
    aggregate_id: &str,
    operation_id: &str,
) {
    let conflict: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM operation_conflicts WHERE project_id = ?1 AND aggregate_type = ?2 AND aggregate_id = ?3 AND incoming_operation_id = ?4 AND conflict_type = 'stale_base_revision'",
            rusqlite::params![project_id, aggregate_type, aggregate_id, operation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        conflict, 1,
        "a stale base revision must record a durable conflict"
    );
}

/// Reads the current materialized value of one fact row as JSON.
type FactReader = Box<dyn Fn(&Path, &str) -> Value>;

struct RoundTripCase {
    operation_type: &'static str,
    aggregate_type: &'static str,
    aggregate_id: &'static str,
    payload: Value,
    /// A conflicting operation with different values; if the revision guard
    /// failed it would visibly overwrite the first application.
    stale_payload: Value,
    /// The value that must remain materialized after the stale attempt.
    expected_value: Value,
    /// Reads the current materialized value as JSON.
    read: FactReader,
}

#[test]
fn eleven_operation_round_trips_materialize_once_and_refuse_stale_bases() {
    let (directory, path, project_id) = project_with_asset();
    let catalog = Catalog::open_existing(&path).unwrap();
    let connection = open_connection(&path);
    // Direct setup: a declared sequence plus split/merge sources with counters.
    connection
        .execute(
            "INSERT INTO capture_sequences(id, project_id, camera_key, label, volume_hint, filename_prefix, created_at) VALUES ('seq_declared', ?1, 'Roundtrip Cam', 'Roundtrip Cam', '', '', '2026-07-20T00:00:00.000Z'), ('seq_split_src', ?1, 'Roundtrip Cam', 'Split source', '', '', '2026-07-20T00:00:00.000Z'), ('seq_merge_into', ?1, 'Merge Cam', 'Into', '', '', '2026-07-20T00:00:00.000Z'), ('seq_merge_from', ?1, 'Merge Cam', 'From', '', '', '2026-07-20T00:00:00.000Z')",
            [&project_id],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, camera, created_at) VALUES ('asset_split_a', ?1, 'fp-a', '/offline/a.mp4', 'C0001.MP4', 1, 1, 'Roundtrip Cam', '2026-07-20T00:00:00.000Z'), ('asset_split_b', ?1, 'fp-b', '/offline/b.mp4', 'C0005.MP4', 1, 1, 'Roundtrip Cam', '2026-07-20T00:00:00.000Z'), ('asset_merge_1', ?1, 'fp-m1', '/offline/m1.mp4', 'C0001.MP4', 1, 1, 'Merge Cam', '2026-07-20T00:00:00.000Z'), ('asset_merge_2', ?1, 'fp-m2', '/offline/m2.mp4', 'C0002.MP4', 1, 1, 'Merge Cam', '2026-07-20T00:00:00.000Z')",
            [&project_id],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO asset_capture_sequence(asset_id, project_id, capture_sequence_id, filename_counter) VALUES ('asset_split_a', ?1, 'seq_split_src', 1), ('asset_split_b', ?1, 'seq_split_src', 5), ('asset_merge_1', ?1, 'seq_merge_from', 1), ('asset_merge_2', ?1, 'seq_merge_into', 2)",
            [&project_id],
        )
        .unwrap();
    drop(connection);

    let cases: Vec<RoundTripCase> = vec![
        RoundTripCase {
            operation_type: CAPTURE_SEQUENCE_DECLARED,
            aggregate_type: "captureSequence",
            aggregate_id: "seq_declared",
            payload: json!({"sequenceId":"seq_declared","cameraKey":"Roundtrip Cam","label":"Declared","volumeHint":"vol","filenamePrefix":"C"}),
            stale_payload: json!({"sequenceId":"seq_declared","cameraKey":"Other Cam","label":"Overwrite","volumeHint":"","filenamePrefix":"X"}),
            expected_value: json!("Declared"),
            read: Box::new(|catalog, project_id| {
                json!(open_connection(catalog).query_row("SELECT label FROM capture_sequences WHERE id='seq_declared' AND project_id=?1", [project_id], |row| row.get::<_, String>(0)).unwrap())
            }),
        },
        RoundTripCase {
            operation_type: CAPTURE_SEQUENCE_SPLIT,
            aggregate_type: "captureSequence",
            aggregate_id: "seq_split_src",
            payload: json!({"newSequenceId":"seq_split_new","cameraKey":"Roundtrip Cam","label":"Split","volumeHint":"","filenamePrefix":"","atCounter":3}),
            stale_payload: json!({"newSequenceId":"seq_split_other","cameraKey":"Roundtrip Cam","label":"Split","volumeHint":"","filenamePrefix":"","atCounter":1}),
            expected_value: json!("seq_split_new"),
            read: Box::new(|catalog, project_id| {
                json!(open_connection(catalog).query_row("SELECT capture_sequence_id FROM asset_capture_sequence WHERE asset_id='asset_split_b' AND project_id=?1", [project_id], |row| row.get::<_, String>(0)).unwrap())
            }),
        },
        RoundTripCase {
            operation_type: CAPTURE_SEQUENCE_MERGED,
            aggregate_type: "captureSequence",
            aggregate_id: "seq_merge_into",
            payload: json!({"fromSequenceId":"seq_merge_from","intoSequenceId":"seq_merge_into"}),
            stale_payload: json!({"fromSequenceId":"seq_declared","intoSequenceId":"seq_merge_into"}),
            expected_value: json!(2),
            read: Box::new(|catalog, project_id| {
                json!(open_connection(catalog).query_row("SELECT COUNT(*) FROM asset_capture_sequence WHERE project_id=?1 AND capture_sequence_id='seq_merge_into'", [project_id], |row| row.get::<_, i64>(0)).unwrap())
            }),
        },
        RoundTripCase {
            operation_type: CLOCK_EPOCH_SET,
            aggregate_type: "clockEpoch",
            aggregate_id: "epoch_rt",
            payload: json!({"epochId":"epoch_rt","captureSequenceId":"seq_declared","startCounter":1,"endCounter":5,"offsetSeconds":-60,"label":"epoch label","confidence":0.9,"evidence":["user_boundary"]}),
            stale_payload: json!({"epochId":"epoch_rt","captureSequenceId":"seq_declared","startCounter":1,"endCounter":null,"offsetSeconds":9999,"label":"overwrite","confidence":0.1,"evidence":[]}),
            expected_value: json!(-60),
            read: Box::new(|catalog, project_id| {
                json!(open_connection(catalog).query_row("SELECT offset_seconds FROM clock_epochs WHERE id='epoch_rt' AND project_id=?1", [project_id], |row| row.get::<_, i64>(0)).unwrap())
            }),
        },
        RoundTripCase {
            operation_type: CLOCK_EPOCH_CLEARED,
            aggregate_type: "clockEpoch",
            aggregate_id: "epoch_clear",
            payload: json!({"epochId":"epoch_clear"}),
            stale_payload: json!({"epochId":"epoch_clear"}),
            expected_value: json!(0),
            read: Box::new(|catalog, project_id| {
                json!(open_connection(catalog).query_row("SELECT COUNT(*) FROM clock_epochs WHERE id='epoch_clear' AND project_id=?1", [project_id], |row| row.get::<_, i64>(0)).unwrap())
            }),
        },
        RoundTripCase {
            operation_type: CLIP_TIME_EXCEPTION_SET,
            aggregate_type: "projectAsset",
            aggregate_id: "asset_rt",
            payload: json!({"tripTime":"2026-07-18T08:30:00.000Z","reason":"ferry boarding"}),
            stale_payload: json!({"tripTime":"2026-07-18T22:00:00.000Z","reason":"overwrite"}),
            expected_value: json!("2026-07-18T08:30:00.000Z"),
            read: Box::new(|catalog, project_id| {
                json!(open_connection(catalog).query_row("SELECT trip_time FROM clip_time_exceptions WHERE asset_id='asset_rt' AND project_id=?1", [project_id], |row| row.get::<_, String>(0)).unwrap())
            }),
        },
        RoundTripCase {
            operation_type: CLIP_TIME_EXCEPTION_CLEARED,
            aggregate_type: "projectAsset",
            aggregate_id: "asset_rt_2",
            payload: json!({}),
            stale_payload: json!({}),
            expected_value: json!(0),
            read: Box::new(|catalog, project_id| {
                json!(open_connection(catalog).query_row("SELECT COUNT(*) FROM clip_time_exceptions WHERE asset_id='asset_rt_2' AND project_id=?1", [project_id], |row| row.get::<_, i64>(0)).unwrap())
            }),
        },
        RoundTripCase {
            operation_type: TRIP_PHASE_SET,
            aggregate_type: "tripPhase",
            aggregate_id: "phase_rt",
            payload: json!({"phaseId":"phase_rt","label":"Canada","phaseKind":"location","startTripTime":"2026-07-18T00:00:00.000Z","endTripTime":"2026-07-20T00:00:00.000Z","timezone":"America/Toronto","originTimezone":null,"destinationTimezone":null,"displayMode":"local"}),
            stale_payload: json!({"phaseId":"phase_rt","label":"Overwrite","phaseKind":"transit","startTripTime":"2026-07-18T00:00:00.000Z","endTripTime":"2026-07-20T00:00:00.000Z","timezone":null,"originTimezone":"Asia/Tokyo","destinationTimezone":"America/Toronto","displayMode":"dual"}),
            expected_value: json!("Canada"),
            read: Box::new(|catalog, project_id| {
                json!(
                    open_connection(catalog)
                        .query_row(
                            "SELECT label FROM trip_phases WHERE id='phase_rt' AND project_id=?1",
                            [project_id],
                            |row| row.get::<_, String>(0)
                        )
                        .unwrap()
                )
            }),
        },
        RoundTripCase {
            operation_type: TRIP_PHASE_REMOVED,
            aggregate_type: "tripPhase",
            aggregate_id: "phase_remove",
            payload: json!({"phaseId":"phase_remove"}),
            stale_payload: json!({"phaseId":"phase_remove"}),
            expected_value: json!(0),
            read: Box::new(|catalog, project_id| {
                json!(open_connection(catalog).query_row("SELECT COUNT(*) FROM trip_phases WHERE id='phase_remove' AND project_id=?1", [project_id], |row| row.get::<_, i64>(0)).unwrap())
            }),
        },
        RoundTripCase {
            operation_type: EVENT_ORDER_CONSTRAINT_SET,
            aggregate_type: "eventOrderConstraint",
            aggregate_id: "constraint_rt",
            payload: json!({"constraintId":"constraint_rt","earlierAssetId":"asset_rt","laterAssetId":"asset_split_a","strength":"hard","source":"user"}),
            stale_payload: json!({"constraintId":"constraint_rt","earlierAssetId":"asset_rt","laterAssetId":"asset_split_a","strength":"soft","source":"overwrite"}),
            expected_value: json!("hard"),
            read: Box::new(|catalog, project_id| {
                json!(open_connection(catalog).query_row("SELECT strength FROM event_order_constraints WHERE id='constraint_rt' AND project_id=?1", [project_id], |row| row.get::<_, String>(0)).unwrap())
            }),
        },
        RoundTripCase {
            operation_type: EVENT_ORDER_CONSTRAINT_CLEARED,
            aggregate_type: "eventOrderConstraint",
            aggregate_id: "constraint_clear",
            payload: json!({"constraintId":"constraint_clear"}),
            stale_payload: json!({"constraintId":"constraint_clear"}),
            expected_value: json!(0),
            read: Box::new(|catalog, project_id| {
                json!(open_connection(catalog).query_row("SELECT COUNT(*) FROM event_order_constraints WHERE id='constraint_clear' AND project_id=?1", [project_id], |row| row.get::<_, i64>(0)).unwrap())
            }),
        },
    ];

    for (index, case) in cases.into_iter().enumerate() {
        let first = operation(
            &format!("op_{index}_first"),
            &project_id,
            "node_rt",
            index as u64 * 10 + 1,
            0,
            case.aggregate_type,
            case.aggregate_id,
            case.operation_type,
            Some(0),
            case.payload.clone(),
        );
        assert_eq!(
            catalog.apply_operation(&first).unwrap(),
            ApplyOperationResult::Applied,
            "{} must apply once",
            case.operation_type
        );
        assert_eq!(
            (case.read)(&path, &project_id),
            case.expected_value,
            "{} materialized the expected row",
            case.operation_type
        );

        let duplicate = operation(
            &format!("op_{index}_first"),
            &project_id,
            "node_rt",
            index as u64 * 10 + 1,
            0,
            case.aggregate_type,
            case.aggregate_id,
            case.operation_type,
            Some(0),
            case.payload.clone(),
        );
        assert_eq!(
            catalog.apply_operation(&duplicate).unwrap(),
            ApplyOperationResult::Duplicate,
            "{} must materialize exactly once",
            case.operation_type
        );
        assert_eq!((case.read)(&path, &project_id), case.expected_value);

        let stale = operation(
            &format!("op_{index}_stale"),
            &project_id,
            "node_rt",
            index as u64 * 10 + 2,
            0,
            case.aggregate_type,
            case.aggregate_id,
            case.operation_type,
            Some(0), // aggregate revision is now 1; 0 is stale
            case.stale_payload,
        );
        assert_eq!(
            catalog.apply_operation(&stale).unwrap(),
            ApplyOperationResult::Conflict,
            "{} with a stale base revision must conflict",
            case.operation_type
        );
        assert_eq!(
            (case.read)(&path, &project_id),
            case.expected_value,
            "{} must not overwrite on a stale base",
            case.operation_type
        );
        assert_conflict_recorded(
            &open_connection(&path),
            &project_id,
            case.aggregate_type,
            case.aggregate_id,
            &format!("op_{index}_stale"),
        );
    }
    let _ = directory;
}

// ---------------------------------------------------------------------------
// Oracle 8: legacy replay materializes into clock_epochs
// ---------------------------------------------------------------------------

#[test]
fn legacy_clock_event_replay_materializes_into_clock_epochs_like_the_migration() {
    let directory = tempdir().unwrap();
    let replay_assets = [FixtureAsset {
        id: "asset_replay_1",
        filename: "DJI_0001.MP4",
        camera: "DJI",
        captured_at_raw: "2026-07-18T09:00:00Z",
        best_time: "2026-07-18T09:00:00.000Z",
        offset_seconds: 0,
    }];
    let replay_path = build_schema8_catalog(
        &directory,
        "replay.sqlite3",
        "project_replay",
        &replay_assets,
        &[],
    );
    let replay_catalog = Catalog::open_existing(&replay_path).unwrap();
    let replay = operation(
        "op_replay_clock",
        "project_replay",
        "node_replay",
        1,
        0,
        "clock",
        "DJI",
        CLOCK_EVENT_CREATED,
        Some(0),
        json!({"eventId":"clock_replay","camera":"DJI","startFilename":"DJI_0002.MP4","offsetSeconds":-3600,"label":"replay"}),
    );
    assert_eq!(
        replay_catalog.apply_operation(&replay).unwrap(),
        ApplyOperationResult::Applied
    );
    let connection = open_connection(&replay_path);
    let (legacy_rows, epoch_rows): (i64, i64) = (
        connection
            .query_row(
                "SELECT COUNT(*) FROM clock_events WHERE project_id='project_replay'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
        connection
            .query_row(
                "SELECT COUNT(*) FROM clock_epochs WHERE project_id='project_replay'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
    );
    assert_eq!((legacy_rows, epoch_rows), (1, 1));

    // The equivalent legacy row, carried over by migration 9.
    let migrated_path = build_schema8_catalog(
        &directory,
        "migrated.sqlite3",
        "project_migrated",
        &replay_assets,
        &[FixtureEvent {
            id: "clock_migrated",
            camera: "DJI",
            start_filename: "DJI_0002.MP4",
            offset_seconds: -3600,
            label: "replay",
        }],
    );
    let _migrated_catalog = Catalog::open_existing(&migrated_path).unwrap();
    let migrated_connection = open_connection(&migrated_path);
    let replay_epoch = connection
        .query_row(
            "SELECT capture_sequence_id, start_counter, end_counter, offset_seconds, confidence, evidence_json, label FROM clock_epochs WHERE id='clock_replay'",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, f64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .unwrap();
    let migrated_epoch = migrated_connection
        .query_row(
            "SELECT capture_sequence_id, start_counter, end_counter, offset_seconds, confidence, evidence_json, label FROM clock_epochs WHERE id='clock_migrated'",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, f64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .unwrap();
    // Different projects derive different sequence ids, so compare every
    // epoch field except the sequence id, then compare the sequence cameras.
    assert_eq!(
        (
            replay_epoch.1,
            replay_epoch.2,
            replay_epoch.3,
            replay_epoch.4,
            replay_epoch.5,
            replay_epoch.6
        ),
        (
            migrated_epoch.1,
            migrated_epoch.2,
            migrated_epoch.3,
            migrated_epoch.4,
            migrated_epoch.5,
            migrated_epoch.6
        ),
        "legacy replay must produce the same epoch the migration produces"
    );
    let replay_sequence_camera: String = connection
        .query_row(
            "SELECT camera_key FROM capture_sequences WHERE id=?1",
            [&replay_epoch.0],
            |row| row.get(0),
        )
        .unwrap();
    let migrated_sequence_camera: String = migrated_connection
        .query_row(
            "SELECT camera_key FROM capture_sequences WHERE id=?1",
            [&migrated_epoch.0],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(replay_sequence_camera, "DJI");
    assert_eq!(migrated_sequence_camera, "DJI");
    assert_eq!(replay_epoch.1, 2, "start counter parsed from DJI_0002.MP4");
}

// ---------------------------------------------------------------------------
// Oracle 9: quarantine policy
// ---------------------------------------------------------------------------

#[test]
fn unknown_operations_are_quarantined_durably_without_failing_the_batch() {
    let directory = tempdir().unwrap();
    let path = build_schema8_catalog(
        &directory,
        "quarantine.sqlite3",
        "project_quarantine",
        &[
            FixtureAsset {
                id: "asset_q",
                filename: "C0001.MP4",
                camera: "Quarantine Cam",
                captured_at_raw: "2026-07-18T09:00:00Z",
                best_time: "2026-07-18T09:00:00.000Z",
                offset_seconds: 0,
            },
            FixtureAsset {
                id: "asset_q2",
                filename: "C0002.MP4",
                camera: "Quarantine Cam",
                captured_at_raw: "2026-07-18T09:00:10Z",
                best_time: "2026-07-18T09:00:10.000Z",
                offset_seconds: 0,
            },
        ],
        &[],
    );
    let catalog = Catalog::open_existing(&path).unwrap();

    let mut unknown = operation(
        "op_q_future",
        "project_quarantine",
        "node_q",
        0,
        0,
        "captureSequence",
        "seq_future",
        "someFutureOperation",
        Some(5),
        json!({"futureField":"futureValue","nested":{"a":1}}),
    );
    unknown.payload_version = 2; // newer payload version than this build knows
    let unknown_asset = operation(
        "op_q_future_asset",
        "project_quarantine",
        "node_q",
        0,
        0,
        "projectAsset",
        "asset_missing",
        "anotherFutureOperation",
        None,
        json!({"x": 1}),
    );
    let known_one = operation(
        "op_q_known_1",
        "project_quarantine",
        "node_q",
        0,
        0,
        "projectAsset",
        "asset_q",
        CLIP_TIME_EXCEPTION_SET,
        Some(0),
        json!({"tripTime":"2026-07-18T08:00:00.000Z","reason":"known"}),
    );
    let known_two = operation(
        "op_q_known_2",
        "project_quarantine",
        "node_q",
        0,
        0,
        "tripPhase",
        "phase_q",
        TRIP_PHASE_SET,
        Some(0),
        json!({"phaseId":"phase_q","label":"Known","phaseKind":"location","startTripTime":"2026-07-18T00:00:00.000Z","endTripTime":"2026-07-19T00:00:00.000Z","timezone":null,"originTimezone":null,"destinationTimezone":null,"displayMode":"utc"}),
    );
    let known_three = operation(
        "op_q_known_3",
        "project_quarantine",
        "node_q",
        0,
        0,
        "eventOrderConstraint",
        "constraint_q",
        EVENT_ORDER_CONSTRAINT_SET,
        Some(0),
        json!({"constraintId":"constraint_q","earlierAssetId":"asset_q","laterAssetId":"asset_q2","strength":"hard","source":"known"}),
    );

    let result = apply_batch(
        &catalog,
        "node_q",
        vec![unknown, unknown_asset, known_one, known_two, known_three],
    );
    assert_eq!(
        (
            result.applied,
            result.duplicates,
            result.conflicts,
            result.quarantined,
        ),
        (3, 0, 0, 2),
        "the known three applied; the unknown two were retained and did not fail the batch"
    );

    let quarantined = catalog
        .quarantined_operations("project_quarantine")
        .unwrap();
    assert_eq!(quarantined.len(), 2);
    let future = quarantined
        .iter()
        .find(|item| item.operation_id == "op_q_future")
        .expect("the unknown operation is reported");
    assert_eq!(future.operation_type, "someFutureOperation");
    assert_eq!(future.aggregate_type, "captureSequence");
    assert_eq!(future.aggregate_id, "seq_future");
    assert_eq!(future.payload_version, 2);
    assert!(!future.received_at.is_empty());

    // No aggregate revision advanced for either quarantined operation.
    let connection = open_connection(&path);
    for (aggregate_type, aggregate_id) in [
        ("captureSequence", "seq_future"),
        ("projectAsset", "asset_missing"),
    ] {
        let revision_rows: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM materialized_revisions WHERE project_id='project_quarantine' AND aggregate_type=?1 AND aggregate_id=?2",
                rusqlite::params![aggregate_type, aggregate_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            revision_rows, 0,
            "quarantined operations must not advance materialized_revisions"
        );
    }
    // The known operations materialized.
    let known_rows: i64 = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM clip_time_exceptions WHERE project_id='project_quarantine') + (SELECT COUNT(*) FROM trip_phases WHERE project_id='project_quarantine') + (SELECT COUNT(*) FROM event_order_constraints WHERE project_id='project_quarantine')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(known_rows, 3);
    drop(connection);

    // Durability: the quarantine survives a reopen with its full payload.
    drop(catalog);
    let reopened = Catalog::open_existing(&path).unwrap();
    let quarantined_after = reopened
        .quarantined_operations("project_quarantine")
        .unwrap();
    assert_eq!(quarantined_after.len(), 2);
    let stored_payload: String = open_connection(&path)
        .query_row(
            "SELECT payload_json FROM project_operations WHERE operation_id='op_q_future'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&stored_payload).unwrap(),
        json!({"futureField":"futureValue","nested":{"a":1}}),
        "the quarantined operation keeps its complete payload for the upgraded build"
    );
    let _ = directory;
}

// ---------------------------------------------------------------------------
// Oracle 10: loader determinism
// ---------------------------------------------------------------------------

#[test]
fn chronology_facts_loader_is_deterministic_across_repeated_calls() {
    let directory = tempdir().unwrap();
    let assets = [
        FixtureAsset {
            id: "asset_det_z",
            filename: "C0003.MP4",
            camera: "Det Cam",
            captured_at_raw: "2026-07-18T09:00:00Z",
            best_time: "2026-07-18T09:00:00.000Z",
            offset_seconds: 0,
        },
        FixtureAsset {
            id: "asset_det_a",
            filename: "C0001.MP4",
            camera: "Det Cam",
            captured_at_raw: "2026-07-18T09:00:10Z",
            best_time: "2026-07-18T09:00:10.000Z",
            offset_seconds: 0,
        },
        FixtureAsset {
            id: "asset_det_m",
            filename: "C0002.MP4",
            camera: "Det Cam",
            captured_at_raw: "2026-07-18T09:00:20Z",
            best_time: "2026-07-18T09:00:20.000Z",
            offset_seconds: 0,
        },
    ];
    let path = build_schema8_catalog(
        &directory,
        "determinism.sqlite3",
        "project_det",
        &assets,
        &[],
    );
    let catalog = Catalog::open_existing(&path).unwrap();
    let connection = open_connection(&path);
    // Insert facts in non-primary-key order to prove the loader sorts.
    connection
        .execute(
            "INSERT INTO capture_sequences(id, project_id, camera_key, label, volume_hint, filename_prefix, created_at) VALUES ('seq_det_z', ?1, 'Det Cam', 'z', '', '', '2026-07-20T00:00:00.000Z'), ('seq_det_a', ?1, 'Det Cam', 'a', '', '', '2026-07-20T00:00:00.000Z')",
            [&"project_det".to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO clock_epochs(id, project_id, capture_sequence_id, start_counter, end_counter, offset_seconds, confidence, evidence_json, label, created_at) VALUES ('epoch_det_b', ?1, 'seq_det_a', 1, NULL, 60, 1.0, '[]', 'b', '2026-07-20T00:00:00.000Z'), ('epoch_det_a', ?1, 'seq_det_a', 1, 3, 30, 1.0, '[]', 'a', '2026-07-20T00:00:00.000Z')",
            [&"project_det".to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO clip_time_exceptions(asset_id, project_id, trip_time, reason, created_at) VALUES ('asset_det_m', ?1, '2026-07-18T08:00:00.000Z', 'ferry', '2026-07-20T00:00:00.000Z'), ('asset_det_a', ?1, '2026-07-18T08:10:00.000Z', 'train', '2026-07-20T00:00:00.000Z')",
            [&"project_det".to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO trip_phases(id, project_id, label, phase_kind, start_trip_time, end_trip_time, timezone, origin_timezone, destination_timezone, display_mode, created_at) VALUES ('phase_det_b', ?1, 'Flight', 'transit', '2026-07-18T00:00:00.000Z', '2026-07-18T12:00:00.000Z', NULL, 'Asia/Tokyo', 'America/Toronto', 'dual', '2026-07-20T00:00:00.000Z'), ('phase_det_a', ?1, 'Canada', 'location', '2026-07-18T12:00:00.000Z', '2026-07-20T00:00:00.000Z', 'America/Toronto', NULL, NULL, 'local', '2026-07-20T00:00:00.000Z')",
            [&"project_det".to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO event_order_constraints(id, project_id, earlier_asset_id, later_asset_id, strength, source, created_at) VALUES ('constraint_det_b', ?1, 'asset_det_m', 'asset_det_z', 'soft', 'user', '2026-07-20T00:00:00.000Z'), ('constraint_det_a', ?1, 'asset_det_a', 'asset_det_m', 'hard', 'user', '2026-07-20T00:00:00.000Z')",
            [&"project_det".to_string()],
        )
        .unwrap();
    drop(connection);

    let first = catalog.chronology_facts("project_det").unwrap();
    assert_eq!(
        first
            .assets
            .iter()
            .map(|asset| asset.asset_id.as_str())
            .collect::<Vec<_>>(),
        ["asset_det_a", "asset_det_m", "asset_det_z"],
        "assets sorted by primary key"
    );
    // Three sequences: the two inserted directly plus the migration-created
    // one for the fixture camera ("Det Cam"), all sorted by primary key.
    let sequence_ids = first
        .sequences
        .iter()
        .map(|seq| seq.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(sequence_ids.len(), 3);
    assert_eq!(&sequence_ids[..2], ["seq_det_a", "seq_det_z"]);
    assert_eq!(first.sequences[2].camera_key, "Det Cam");
    assert_eq!(
        first
            .epochs
            .iter()
            .map(|epoch| epoch.id.as_str())
            .collect::<Vec<_>>(),
        ["epoch_det_a", "epoch_det_b"]
    );
    assert_eq!(
        first
            .exceptions
            .iter()
            .map(|ex| ex.asset_id.as_str())
            .collect::<Vec<_>>(),
        ["asset_det_a", "asset_det_m"]
    );
    assert_eq!(
        first
            .phases
            .iter()
            .map(|phase| phase.id.as_str())
            .collect::<Vec<_>>(),
        ["phase_det_a", "phase_det_b"]
    );
    assert_eq!(
        first
            .constraints
            .iter()
            .map(|constraint| constraint.id.as_str())
            .collect::<Vec<_>>(),
        ["constraint_det_a", "constraint_det_b"]
    );
    for _ in 0..32 {
        let again = catalog.chronology_facts("project_det").unwrap();
        assert_eq!(
            serde_json::to_string(&again).unwrap(),
            serde_json::to_string(&first).unwrap(),
            "chronology_facts must be deterministic across repeated calls"
        );
    }
}

// ---------------------------------------------------------------------------
// Oracle 11: source media untouched
// ---------------------------------------------------------------------------

#[test]
fn media_file_bytes_are_unchanged_across_migration_and_operation_cycles() {
    let media_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(FIXTURES_ROOT)
        .join("media")
        .join("C0001.MP4");
    let before = full_file_blake3(&media_path, &|| false).unwrap();

    let directory = tempdir().unwrap();
    let path = directory.path().join("media-cycle.sqlite3");
    let mut connection = Connection::open(&path).unwrap();
    run_sqlite_migrations(&mut connection, &PROJECT_CATALOG_MIGRATIONS[..8]).unwrap();
    connection
        .execute(
            "INSERT INTO projects(id, name, created_at) VALUES ('project_media', 'Media', '2026-07-20T00:00:00.000Z')",
            [],
        )
        .unwrap();
    connection
        .execute(
            r#"
            INSERT INTO assets (
                id, project_id, fingerprint, path, filename, size_bytes,
                duration_seconds, frame_rate, width, height, codec, camera,
                captured_at_raw, best_time, offset_seconds, created_at
            ) VALUES ('asset_media', 'project_media', 'fp-media', ?1, 'C0001.MP4',
                      2048, 1, 30, 640, 360, 'h264', 'Media Cam',
                      '2026-07-18T09:00:00Z', '2026-07-18T09:00:00.000Z', 0,
                      '2026-07-20T00:00:00.000Z'),
                     ('asset_media_2', 'project_media', 'fp-media-2', '/offline/media2.mp4', 'C0002.MP4',
                      1024, 1, 30, 640, 360, 'h264', 'Media Cam',
                      '2026-07-18T09:00:10Z', '2026-07-18T09:00:10.000Z', 0,
                      '2026-07-20T00:00:00.000Z')
            "#,
            [media_path.to_string_lossy()],
        )
        .unwrap();
    drop(connection);

    let catalog = Catalog::open_existing(&path).unwrap();
    // A full operation-application cycle across the vocabulary families.
    let ops = vec![
        operation(
            "op_media_1",
            "project_media",
            "node_media",
            0,
            0,
            "captureSequence",
            "seq_media",
            CAPTURE_SEQUENCE_DECLARED,
            Some(0),
            json!({"sequenceId":"seq_media","cameraKey":"Media Cam","label":"Media Cam","volumeHint":"","filenamePrefix":""}),
        ),
        operation(
            "op_media_2",
            "project_media",
            "node_media",
            0,
            0,
            "clockEpoch",
            "epoch_media",
            CLOCK_EPOCH_SET,
            Some(0),
            json!({"epochId":"epoch_media","captureSequenceId":"seq_media","startCounter":1,"endCounter":null,"offsetSeconds":-60,"label":"media epoch"}),
        ),
        operation(
            "op_media_3",
            "project_media",
            "node_media",
            0,
            0,
            "projectAsset",
            "asset_media",
            CLIP_TIME_EXCEPTION_SET,
            Some(0),
            json!({"tripTime":"2026-07-18T08:00:00.000Z","reason":"media exception"}),
        ),
        operation(
            "op_media_4",
            "project_media",
            "node_media",
            0,
            0,
            "tripPhase",
            "phase_media",
            TRIP_PHASE_SET,
            Some(0),
            json!({"phaseId":"phase_media","label":"Media","phaseKind":"location","startTripTime":"2026-07-18T00:00:00.000Z","endTripTime":"2026-07-19T00:00:00.000Z","timezone":"America/Toronto","originTimezone":null,"destinationTimezone":null,"displayMode":"local"}),
        ),
        operation(
            "op_media_5",
            "project_media",
            "node_media",
            0,
            0,
            "eventOrderConstraint",
            "constraint_media",
            EVENT_ORDER_CONSTRAINT_SET,
            Some(0),
            json!({"constraintId":"constraint_media","earlierAssetId":"asset_media","laterAssetId":"asset_media_2","strength":"soft","source":"media"}),
        ),
        operation(
            "op_media_6",
            "project_media",
            "node_media",
            0,
            0,
            "clock",
            "Media Cam",
            CLOCK_EVENT_CREATED,
            Some(0),
            json!({"eventId":"clock_media","camera":"Media Cam","startFilename":"C0001.MP4","offsetSeconds":-60,"label":"legacy media"}),
        ),
    ];
    let result = apply_batch(&catalog, "node_media", ops);
    assert_eq!((result.applied, result.quarantined), (6, 0));
    assert_eq!(
        catalog
            .chronology_facts("project_media")
            .unwrap()
            .epochs
            .len(),
        2
    );

    let after = full_file_blake3(&media_path, &|| false).unwrap();
    assert_eq!(before.hex_digest, after.hex_digest);
    assert_eq!(before.byte_count, after.byte_count);
    assert_eq!(fs::read(&media_path).unwrap().len(), 2048);
}

// ---------------------------------------------------------------------------
// Oracle 9 continued: the re-dispatch seam (a quarantined operation applies
// when a build that knows its type opens the catalog writable)
// ---------------------------------------------------------------------------

/// A minimal schema-8 catalog for the re-dispatch tests.
fn quarantined_style_catalog(directory: &TempDir, name: &str, project_id: &str) -> PathBuf {
    build_schema8_catalog(
        directory,
        name,
        project_id,
        &[FixtureAsset {
            id: "asset_rd",
            filename: "C0001.MP4",
            camera: "Redispatch Cam",
            captured_at_raw: "2026-07-18T09:00:00Z",
            best_time: "2026-07-18T09:00:00.000Z",
            offset_seconds: 0,
        }],
        &[],
    )
}

/// Inserts an operation row directly into `project_operations`, bypassing
/// `apply_operation` — exactly the shape an older build's quarantine leaves
/// behind: the row is durable, nothing was materialized, and no
/// `materialized_revisions` row exists for its aggregate.
fn insert_operation_row(connection: &Connection, operation: &ProjectOperation) {
    connection
        .execute(
            "INSERT INTO project_operations(operation_id, project_id, origin_node_id, origin_sequence, hlc_wall_millis, hlc_counter, aggregate_type, aggregate_id, operation_type, payload_version, base_revision, payload_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                operation.operation_id,
                operation.project_id,
                operation.origin_node_id,
                operation.origin_sequence as i64,
                operation.logical_time.wall_millis,
                operation.logical_time.counter as i64,
                operation.aggregate_type,
                operation.aggregate_id,
                operation.operation_type,
                operation.payload_version,
                operation.base_revision.map(|value| value as i64),
                serde_json::to_string(&operation.payload).unwrap(),
                "2026-07-25T00:00:00.000Z",
            ],
        )
        .unwrap();
}

#[test]
fn quarantined_known_chronology_operation_redispatch_materializes_on_writable_reopen() {
    let directory = tempdir().unwrap();
    let path = quarantined_style_catalog(&directory, "redispatch.sqlite3", "project_redispatch");
    let declared = operation(
        "op_rd_declared",
        "project_redispatch",
        "node_old_build",
        1,
        1_700_000_000_000,
        "captureSequence",
        "seq_rd_declared",
        CAPTURE_SEQUENCE_DECLARED,
        Some(0),
        json!({"sequenceId":"seq_rd_declared","cameraKey":"Redispatch Cam","label":"After upgrade","volumeHint":"vol","filenamePrefix":"C"}),
    );
    insert_operation_row(&open_connection(&path), &declared);

    // A build that knows the type opens the catalog writable: the operation
    // is re-dispatched from the persisted log and materialized.
    let catalog = Catalog::open_existing(&path).unwrap();
    let connection = open_connection(&path);
    let (label, camera_key): (String, String) = connection
        .query_row(
            "SELECT label, camera_key FROM capture_sequences WHERE id='seq_rd_declared' AND project_id='project_redispatch'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(label, "After upgrade");
    assert_eq!(camera_key, "Redispatch Cam");
    let (revision, last_operation_id): (i64, String) = connection
        .query_row(
            "SELECT revision, last_operation_id FROM materialized_revisions WHERE project_id='project_redispatch' AND aggregate_type='captureSequence' AND aggregate_id='seq_rd_declared'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (revision, last_operation_id),
        (1, "op_rd_declared".to_string())
    );
    drop(connection);

    // Idempotent: a second open applies nothing new.
    drop(catalog);
    let _reopened = Catalog::open_existing(&path).unwrap();
    let connection = open_connection(&path);
    let sequence_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM capture_sequences WHERE id='seq_rd_declared'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sequence_count, 1);
    let revision_after: i64 = connection
        .query_row(
            "SELECT revision FROM materialized_revisions WHERE project_id='project_redispatch' AND aggregate_type='captureSequence' AND aggregate_id='seq_rd_declared'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(revision_after, 1, "a second open must not re-apply the row");
    let _ = directory;
}

#[test]
fn unknown_operation_type_is_not_redispatch_applied_and_stays_reported() {
    let directory = tempdir().unwrap();
    let path = quarantined_style_catalog(&directory, "unknown.sqlite3", "project_unknown");
    let mut future = operation(
        "op_rd_future",
        "project_unknown",
        "node_old_build",
        1,
        1_700_000_000_000,
        "captureSequence",
        "seq_rd_future",
        "someFutureOperation",
        Some(5),
        json!({"futureField":"futureValue"}),
    );
    future.payload_version = 2;
    insert_operation_row(&open_connection(&path), &future);

    let catalog = Catalog::open_existing(&path).unwrap();
    // Not materialized: the capture sequence the future operation declared
    // must not exist.
    let sequence_count: i64 = open_connection(&path)
        .query_row(
            "SELECT COUNT(*) FROM capture_sequences WHERE id='seq_rd_future' AND project_id='project_unknown'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sequence_count, 0);
    // Still reported through the quarantine diagnostic surface.
    let quarantined = catalog.quarantined_operations("project_unknown").unwrap();
    assert_eq!(quarantined.len(), 1);
    assert_eq!(quarantined[0].operation_id, "op_rd_future");
    assert_eq!(quarantined[0].payload_version, 2);
    let _ = directory;
}

#[test]
fn known_type_row_with_future_payload_version_is_not_redispatch_applied() {
    let directory = tempdir().unwrap();
    let path = quarantined_style_catalog(&directory, "version2.sqlite3", "project_version2");
    let mut declared = operation(
        "op_rd_v2",
        "project_version2",
        "node_old_build",
        1,
        1_700_000_000_000,
        "captureSequence",
        "seq_rd_v2",
        CAPTURE_SEQUENCE_DECLARED,
        Some(0),
        json!({"sequenceId":"seq_rd_v2","cameraKey":"Redispatch Cam","label":"v2","volumeHint":"","filenamePrefix":""}),
    );
    declared.payload_version = 2;
    insert_operation_row(&open_connection(&path), &declared);

    let _catalog = Catalog::open_existing(&path).unwrap();
    let connection = open_connection(&path);
    let sequence_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM capture_sequences WHERE id='seq_rd_v2'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        sequence_count, 0,
        "a known type must keep rejecting payload version 2"
    );
    let revision_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM materialized_revisions WHERE project_id='project_version2' AND aggregate_type='captureSequence' AND aggregate_id='seq_rd_v2'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(revision_count, 0);
    let _ = directory;
}

#[test]
fn known_type_row_with_existing_conflict_is_not_redispatch_applied() {
    let directory = tempdir().unwrap();
    let path = quarantined_style_catalog(&directory, "conflicted.sqlite3", "project_conflicted");
    let declared = operation(
        "op_rd_conflicted",
        "project_conflicted",
        "node_old_build",
        1,
        1_700_000_000_000,
        "captureSequence",
        "seq_rd_conflicted",
        CAPTURE_SEQUENCE_DECLARED,
        Some(0),
        json!({"sequenceId":"seq_rd_conflicted","cameraKey":"Redispatch Cam","label":"Never","volumeHint":"","filenamePrefix":""}),
    );
    let connection = open_connection(&path);
    insert_operation_row(&connection, &declared);
    connection
        .execute(
            "INSERT INTO operation_conflicts(conflict_id, project_id, aggregate_type, aggregate_id, incoming_operation_id, active_operation_id, expected_base_revision, actual_revision, conflict_type, created_at) VALUES ('conflict_rd_1', 'project_conflicted', 'captureSequence', 'seq_rd_conflicted', 'op_rd_conflicted', NULL, 1, 2, 'stale_base_revision', '2026-07-25T00:00:00.000Z')",
            [],
        )
        .unwrap();
    drop(connection);

    let _catalog = Catalog::open_existing(&path).unwrap();
    let connection = open_connection(&path);
    let sequence_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM capture_sequences WHERE id='seq_rd_conflicted'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        sequence_count, 0,
        "a previously rejected operation must not re-apply"
    );
    let conflict_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM operation_conflicts WHERE project_id='project_conflicted'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(conflict_count, 1, "no new conflict row may be written");
    let revision_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM materialized_revisions WHERE project_id='project_conflicted'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(revision_count, 0, "no state change");
    let _ = directory;
}

#[test]
fn legacy_operation_log_is_never_redispatch_replayed() {
    let directory = tempdir().unwrap();
    let (assets, events) = fixture_one_camera_one_clock();
    let path = build_schema8_catalog(
        &directory,
        "legacy_replay.sqlite3",
        "project_legacy_replay",
        &assets,
        &events,
    );
    // A realistic schema-8 log: the legacy clockEventCreated row is durable
    // in project_operations with its aggregate revision, and its clock_events
    // row exists.
    let legacy = operation(
        "op_legacy_clock",
        "project_legacy_replay",
        "node_legacy",
        1,
        1_700_000_000_000,
        "clock",
        "Sony A7C II",
        CLOCK_EVENT_CREATED,
        Some(0),
        json!({"eventId":"clock_single_1","camera":"Sony A7C II","startFilename":"C0102.MP4","offsetSeconds":-3600,"label":"one hour fast"}),
    );
    let connection = open_connection(&path);
    insert_operation_row(&connection, &legacy);
    connection
        .execute(
            "INSERT INTO materialized_revisions(project_id, aggregate_type, aggregate_id, revision, last_operation_id) VALUES ('project_legacy_replay', 'clock', 'Sony A7C II', 1, 'op_legacy_clock')",
            [],
        )
        .unwrap();
    drop(connection);

    // `clock_epochs` does not exist until migration 9, so the first open both
    // migrates and (in the fixed build) re-dispatches; the first count check
    // happens after that open.
    let counts = |path: &Path| -> (i64, i64) {
        let connection = open_connection(path);
        let events: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM clock_events WHERE project_id='project_legacy_replay'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let epochs: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM clock_epochs WHERE project_id='project_legacy_replay'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        (events, epochs)
    };

    let _catalog = Catalog::open_existing(&path).unwrap();
    assert_eq!(
        counts(&path),
        (1, 1),
        "migration and re-dispatch must not replay the legacy clock event"
    );
    drop(_catalog);
    let _reopened = Catalog::open_existing(&path).unwrap();
    assert_eq!(counts(&path), (1, 1), "another open applies nothing new");
    let _ = directory;
}

#[test]
fn open_read_only_existing_never_redispatch_applies() {
    let directory = tempdir().unwrap();
    let path = quarantined_style_catalog(&directory, "readonly.sqlite3", "project_readonly");
    // Migrate to schema 9 first (a read-only open does not migrate).
    let _migrated = Catalog::open_existing(&path).unwrap();
    drop(_migrated);
    let declared = operation(
        "op_rd_readonly",
        "project_readonly",
        "node_old_build",
        1,
        1_700_000_000_000,
        "captureSequence",
        "seq_rd_readonly",
        CAPTURE_SEQUENCE_DECLARED,
        Some(0),
        json!({"sequenceId":"seq_rd_readonly","cameraKey":"Redispatch Cam","label":"Read only","volumeHint":"","filenamePrefix":""}),
    );
    insert_operation_row(&open_connection(&path), &declared);

    // A read-only open must be side-effect free: the quarantined-style row is
    // not applied.
    let read_only = Catalog::open_read_only_existing(&path).unwrap();
    let connection = open_connection(&path);
    let sequence_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM capture_sequences WHERE id='seq_rd_readonly'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        sequence_count, 0,
        "read-only open must not materialize anything"
    );
    let revision_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM materialized_revisions WHERE project_id='project_readonly'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(revision_count, 0);
    drop(read_only);
    drop(connection);

    // Sanity: the same row is re-dispatched by a writable open.
    let _catalog = Catalog::open_existing(&path).unwrap();
    let sequence_count: i64 = open_connection(&path)
        .query_row(
            "SELECT COUNT(*) FROM capture_sequences WHERE id='seq_rd_readonly'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sequence_count, 1);
    let _ = directory;
}

// ---------------------------------------------------------------------------
// F2: captureSequenceMerged self-merge and captureSequenceSplit id collision
// ---------------------------------------------------------------------------

#[test]
fn self_merge_errors_and_leaves_the_sequence_intact() {
    let (directory, path, project_id) = project_with_asset();
    let catalog = Catalog::open_existing(&path).unwrap();
    let connection = open_connection(&path);
    connection
        .execute(
            "INSERT INTO capture_sequences(id, project_id, camera_key, label, volume_hint, filename_prefix, created_at) VALUES ('seq_self', ?1, 'Roundtrip Cam', 'Self', '', '', '2026-07-20T00:00:00.000Z')",
            [&project_id],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, camera, created_at) VALUES ('asset_self_1', ?1, 'fp-s1', '/offline/s1.mp4', 'C0001.MP4', 1, 1, 'Roundtrip Cam', '2026-07-20T00:00:00.000Z'), ('asset_self_2', ?1, 'fp-s2', '/offline/s2.mp4', 'C0002.MP4', 1, 1, 'Roundtrip Cam', '2026-07-20T00:00:00.000Z')",
            [&project_id],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO asset_capture_sequence(asset_id, project_id, capture_sequence_id, filename_counter) VALUES ('asset_self_1', ?1, 'seq_self', 1), ('asset_self_2', ?1, 'seq_self', 2)",
            [&project_id],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO clock_epochs(id, project_id, capture_sequence_id, start_counter, offset_seconds, label, created_at) VALUES ('epoch_self', ?1, 'seq_self', 1, -60, 'self', '2026-07-20T00:00:00.000Z')",
            [&project_id],
        )
        .unwrap();
    drop(connection);

    let self_merge = operation(
        "op_self_merge",
        &project_id,
        "node_f2",
        1,
        1_700_000_000_000,
        "captureSequence",
        "seq_self",
        CAPTURE_SEQUENCE_MERGED,
        Some(0),
        json!({"fromSequenceId":"seq_self","intoSequenceId":"seq_self"}),
    );
    let error = catalog.apply_operation(&self_merge).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("requires two distinct capture sequences"),
        "unexpected error: {error}"
    );

    // Nothing was destroyed: the sequence, its assignments, and its epochs
    // are all still present, and no revision advanced.
    let connection = open_connection(&path);
    let sequence_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM capture_sequences WHERE id='seq_self' AND project_id=?1",
            [&project_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sequence_count, 1, "the sequence must survive a self-merge");
    let assignment_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM asset_capture_sequence WHERE capture_sequence_id='seq_self' AND project_id=?1",
            [&project_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        assignment_count, 2,
        "asset assignments must survive a self-merge"
    );
    let epoch_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM clock_epochs WHERE capture_sequence_id='seq_self' AND project_id=?1",
            [&project_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(epoch_count, 1, "clock epochs must survive a self-merge");
    let revision_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM materialized_revisions WHERE project_id=?1 AND aggregate_type='captureSequence' AND aggregate_id='seq_self'",
            [&project_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        revision_count, 0,
        "a failed self-merge must not advance the revision"
    );
    let _ = directory;
}

#[test]
fn split_into_existing_sequence_id_errors_and_leaves_assignments_unchanged() {
    let (directory, path, project_id) = project_with_asset();
    let catalog = Catalog::open_existing(&path).unwrap();
    let connection = open_connection(&path);
    connection
        .execute(
            "INSERT INTO capture_sequences(id, project_id, camera_key, label, volume_hint, filename_prefix, created_at) VALUES ('seq_split_src', ?1, 'Roundtrip Cam', 'Source', '', '', '2026-07-20T00:00:00.000Z'), ('seq_split_dst', ?1, 'Other Cam', 'Destination', '', '', '2026-07-20T00:00:00.000Z')",
            [&project_id],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, camera, created_at) VALUES ('asset_split_1', ?1, 'fp-s1', '/offline/s1.mp4', 'C0001.MP4', 1, 1, 'Roundtrip Cam', '2026-07-20T00:00:00.000Z'), ('asset_split_2', ?1, 'fp-s2', '/offline/s2.mp4', 'C0005.MP4', 1, 1, 'Roundtrip Cam', '2026-07-20T00:00:00.000Z'), ('asset_dst_1', ?1, 'fp-d1', '/offline/d1.mp4', 'GX010001.MP4', 1, 1, 'Other Cam', '2026-07-20T00:00:00.000Z')",
            [&project_id],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO asset_capture_sequence(asset_id, project_id, capture_sequence_id, filename_counter) VALUES ('asset_split_1', ?1, 'seq_split_src', 1), ('asset_split_2', ?1, 'seq_split_src', 5), ('asset_dst_1', ?1, 'seq_split_dst', 1)",
            [&project_id],
        )
        .unwrap();
    drop(connection);

    // The destination id collides with a sequence that already exists in this
    // project (another camera's derived id, for example).
    let split = operation(
        "op_split_collision",
        &project_id,
        "node_f2",
        1,
        1_700_000_000_000,
        "captureSequence",
        "seq_split_src",
        CAPTURE_SEQUENCE_SPLIT,
        Some(0),
        json!({"newSequenceId":"seq_split_dst","cameraKey":"Roundtrip Cam","label":"Split","volumeHint":"","filenamePrefix":"","atCounter":3}),
    );
    let error = catalog.apply_operation(&split).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("destination sequence already exists"),
        "unexpected error: {error}"
    );

    let connection = open_connection(&path);
    // The unrelated destination sequence's assignment is unchanged...
    let dst_assignment: String = connection
        .query_row(
            "SELECT capture_sequence_id FROM asset_capture_sequence WHERE asset_id='asset_dst_1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(dst_assignment, "seq_split_dst");
    // ...and no source asset moved into it either.
    let assignments: Vec<String> = {
        let mut statement = connection
            .prepare("SELECT capture_sequence_id FROM asset_capture_sequence WHERE project_id=?1 AND capture_sequence_id IN ('seq_split_src', 'seq_split_dst') ORDER BY asset_id")
            .unwrap();
        statement
            .query_map([&project_id], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    assert_eq!(
        assignments,
        ["seq_split_dst", "seq_split_src", "seq_split_src"],
        "assignments must not move into the existing destination sequence"
    );
    let revision_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM materialized_revisions WHERE project_id=?1 AND aggregate_type='captureSequence' AND aggregate_id='seq_split_src'",
            [&project_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        revision_count, 0,
        "a failed split must not advance the revision"
    );
    let _ = directory;
}

// ---------------------------------------------------------------------------
// Oracle 12: no projection writes
// ---------------------------------------------------------------------------

#[test]
fn migration_9_leaves_best_time_and_offset_seconds_untouched() {
    let directory = tempdir().unwrap();
    for (file, project_id) in FIXTURE_FILES {
        let path = directory.path().join(file);
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(FIXTURES_ROOT)
                .join(file),
            &path,
        )
        .unwrap();
        let before_connection = Connection::open(&path).unwrap();
        let before = projection_columns(&before_connection, project_id);
        drop(before_connection);
        let _catalog = Catalog::open_existing(&path).unwrap();
        let after = projection_columns(&open_connection(&path), project_id);
        assert_eq!(
            before, after,
            "{file}: migration 9 must not write the projection columns"
        );
    }
}
