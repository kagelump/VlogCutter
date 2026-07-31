//! Independent falsification tests for D01-T007 ("Chronology domain model and
//! operation vocabulary").
//!
//! Mandate: `docs/tasks/desktop/tasks/D01-T007-chronology-domain-model.md`,
//! "## Independent test charter". The principal invariant under attack:
//!
//! > Migration 9 replaces the flat clock-event model with the full persisted
//! > chronology fact model while producing the identical total review order
//! > for every existing project.
//!
//! Everything here is built by this file alone: the real production migration
//! runner is executed for migrations 1..=8 and rows are inserted directly into
//! the schema-8 tables, so these are genuinely old catalogs, not new catalogs
//! described as old. The pre-migration review order is captured with the exact
//! production ordering query (`ORDER BY best_time IS NULL, best_time, filename,
//! id`, from `Catalog::list_assets` in `src/lib.rs`), and the post-migration
//! order is captured through the public `Catalog::list_assets` seam. The
//! implementer's fixtures and tests are never imported.

use anyhow::Result;
use chrono::{DateTime, Duration, NaiveDateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use tempfile::TempDir;

use rcdvlog_core::{
    ApplyOperationResult, Catalog, CatalogSchemaError, HybridLogicalTime, OperationBatchResult,
    PROJECT_CATALOG_MIGRATIONS, ProjectOperation, run_sqlite_migrations,
};

const FIXTURE_DIR: &str = "tests/fixtures/chronology-independent";
const PROJECT: &str = "project_independent";

/// The production review-order query, byte-identical in semantics to
/// `Catalog::list_assets` (src/lib.rs, `ORDER BY a.best_time IS NULL,
/// a.best_time, a.filename, a.id`).
const REVIEW_ORDER_SQL: &str = "SELECT id FROM assets WHERE project_id = ?1 ORDER BY best_time IS NULL, best_time, filename, id";

// ---------------------------------------------------------------------------
// Fixture catalog specs
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct AssetSpec {
    id: &'static str,
    filename: &'static str,
    path: &'static str,
    camera: &'static str,
    captured_at_raw: Option<&'static str>,
    duration_seconds: f64,
}

struct ClockEventSpec {
    id: &'static str,
    camera: &'static str,
    start_filename: &'static str,
    offset_seconds: i64,
    label: &'static str,
}

struct CatalogSpec<'a> {
    name: &'static str,
    assets: &'a [AssetSpec],
    clock_events: &'a [ClockEventSpec],
}

const SPEC_ONE_CAMERA_NO_EVENTS: CatalogSpec = CatalogSpec {
    name: "one_camera_no_clock_events",
    assets: &[
        AssetSpec {
            id: "a1",
            filename: "C0001.MP4",
            path: "/offline/dji/C0001.MP4",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:00:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "a2",
            filename: "C0002.MP4",
            path: "/offline/dji/C0002.MP4",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:01:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "a3",
            filename: "C0003.MP4",
            path: "/offline/dji/C0003.MP4",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:02:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "a4",
            filename: "C0004.MP4",
            path: "/offline/dji/C0004.MP4",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:03:00Z"),
            duration_seconds: 10.0,
        },
    ],
    clock_events: &[],
};

const SPEC_ONE_CAMERA_ONE_EVENT: CatalogSpec = CatalogSpec {
    name: "one_camera_one_clock_event",
    assets: &[
        AssetSpec {
            id: "a1",
            filename: "C0001.MP4",
            path: "/offline/dji/C0001.MP4",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:00:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "a2",
            filename: "C0002.MP4",
            path: "/offline/dji/C0002.MP4",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:01:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "a3",
            filename: "C0003.MP4",
            path: "/offline/dji/C0003.MP4",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:02:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "a4",
            filename: "C0004.MP4",
            path: "/offline/dji/C0004.MP4",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:03:00Z"),
            duration_seconds: 10.0,
        },
    ],
    clock_events: &[ClockEventSpec {
        id: "clock_event_1",
        camera: "DJI",
        start_filename: "C0002.MP4",
        offset_seconds: 3600,
        label: "crossed noon",
    }],
};

const SPEC_TWO_CAMERAS_ONE_EVENT_EACH: CatalogSpec = CatalogSpec {
    name: "two_cameras_one_clock_event_each",
    assets: &[
        AssetSpec {
            id: "d1",
            filename: "C0001.MP4",
            path: "/offline/dji/C0001.MP4",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:00:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "g1",
            filename: "G0001.MP4",
            path: "/offline/gopro/G0001.MP4",
            camera: "GoPro",
            captured_at_raw: Some("2026-07-18T09:00:30Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "d2",
            filename: "C0002.MP4",
            path: "/offline/dji/C0002.MP4",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:01:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "g2",
            filename: "G0002.MP4",
            path: "/offline/gopro/G0002.MP4",
            camera: "GoPro",
            captured_at_raw: Some("2026-07-18T09:01:30Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "d3",
            filename: "C0003.MP4",
            path: "/offline/dji/C0003.MP4",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:02:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "g3",
            filename: "G0003.MP4",
            path: "/offline/gopro/G0003.MP4",
            camera: "GoPro",
            captured_at_raw: Some("2026-07-18T09:02:30Z"),
            duration_seconds: 10.0,
        },
    ],
    clock_events: &[
        ClockEventSpec {
            id: "e_dji",
            camera: "DJI",
            start_filename: "C0002.MP4",
            offset_seconds: 3600,
            label: "dji event",
        },
        ClockEventSpec {
            id: "e_gopro",
            camera: "GoPro",
            start_filename: "G0002.MP4",
            offset_seconds: 7200,
            label: "gopro event",
        },
    ],
};

/// Two cameras whose filenames use the SAME counter range (C0001..C0010), with
/// captured times interleaved so the review order alternates cameras.
static OVERLAPPING_COUNTER_ASSETS: LazyLock<[AssetSpec; 20]> =
    LazyLock::new(overlapping_counter_assets);
static SPEC_OVERLAPPING_COUNTER_RANGES: LazyLock<CatalogSpec> = LazyLock::new(|| CatalogSpec {
    name: "overlapping_counter_ranges",
    assets: &*OVERLAPPING_COUNTER_ASSETS,
    clock_events: &[
        ClockEventSpec {
            id: "e_dji",
            camera: "DJI",
            start_filename: "C0005.MP4",
            offset_seconds: 300,
            label: "dji shift",
        },
        ClockEventSpec {
            id: "e_gopro",
            camera: "GoPro",
            start_filename: "C0003.MP4",
            offset_seconds: 600,
            label: "gopro shift",
        },
    ],
});

/// Three overlapping clock events on one camera, one on the other
/// (acceptance oracle shape 4).
static TWO_CAMERAS_THREE_EVENTS_ASSETS: LazyLock<[AssetSpec; 12]> =
    LazyLock::new(two_cameras_three_events_assets);
static SPEC_TWO_CAMERAS_THREE_EVENTS: LazyLock<CatalogSpec> = LazyLock::new(|| CatalogSpec {
    name: "two_cameras_three_events_one_camera",
    assets: &*TWO_CAMERAS_THREE_EVENTS_ASSETS,
    clock_events: &[
        ClockEventSpec {
            id: "e1",
            camera: "DJI",
            start_filename: "C0001.MP4",
            offset_seconds: 0,
            label: "day start",
        },
        ClockEventSpec {
            id: "e2",
            camera: "DJI",
            start_filename: "C0005.MP4",
            offset_seconds: 3600,
            label: "timezone shift",
        },
        ClockEventSpec {
            id: "e3",
            camera: "DJI",
            start_filename: "C0009.MP4",
            offset_seconds: 7200,
            label: "past last asset",
        },
        ClockEventSpec {
            id: "e4",
            camera: "GoPro",
            start_filename: "G0001.MP4",
            offset_seconds: 1800,
            label: "gopro event",
        },
    ],
});

/// A camera whose clock event names a `start_filename` no asset has.
const SPEC_EVENT_START_FILENAME_MISSING: CatalogSpec = CatalogSpec {
    name: "clock_event_start_filename_missing",
    assets: &[
        AssetSpec {
            id: "s1",
            filename: "A0001.MP4",
            path: "/offline/sony/A0001.MP4",
            camera: "Sony",
            captured_at_raw: Some("2026-07-18T09:00:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "s2",
            filename: "A0002.MP4",
            path: "/offline/sony/A0002.MP4",
            camera: "Sony",
            captured_at_raw: Some("2026-07-18T09:01:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "s3",
            filename: "A0003.MP4",
            path: "/offline/sony/A0003.MP4",
            camera: "Sony",
            captured_at_raw: Some("2026-07-18T09:02:00Z"),
            duration_seconds: 10.0,
        },
    ],
    clock_events: &[ClockEventSpec {
        id: "e_ghost",
        camera: "Sony",
        start_filename: "NOMATCH_0042.MP4",
        offset_seconds: 7200,
        label: "ghost boundary",
    }],
};

/// An asset with NULL `captured_at_raw` (NULL best_time must sort last).
const SPEC_NULL_CAPTURED_AT: CatalogSpec = CatalogSpec {
    name: "null_captured_at",
    assets: &[
        AssetSpec {
            id: "i1",
            filename: "I0001.MP4",
            path: "/offline/insta/I0001.MP4",
            camera: "Insta360",
            captured_at_raw: Some("2026-07-18T09:00:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "i2",
            filename: "I0002.MP4",
            path: "/offline/insta/I0002.MP4",
            camera: "Insta360",
            captured_at_raw: None,
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "i3",
            filename: "I0003.MP4",
            path: "/offline/insta/I0003.MP4",
            camera: "Insta360",
            captured_at_raw: Some("2026-07-18T09:02:00Z"),
            duration_seconds: 10.0,
        },
    ],
    clock_events: &[ClockEventSpec {
        id: "e_insta",
        camera: "Insta360",
        start_filename: "I0001.MP4",
        offset_seconds: 600,
        label: "insta event",
    }],
};

/// Two assets with identical `captured_at_raw` AND identical filenames in
/// different folders; the review-order tie is broken by id.
const SPEC_IDENTICAL_RAW_AND_FILENAME: CatalogSpec = CatalogSpec {
    name: "identical_raw_and_filename_different_folders",
    assets: &[
        AssetSpec {
            id: "a1",
            filename: "C0001.MP4",
            path: "/vol/DCIM/100MEDIA/C0001.MP4",
            camera: "Canon",
            captured_at_raw: Some("2026-07-19T08:00:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "a2",
            filename: "C0001.MP4",
            path: "/vol/DCIM/200MEDIA/C0001.MP4",
            camera: "Canon",
            captured_at_raw: Some("2026-07-19T08:00:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "a3",
            filename: "C0002.MP4",
            path: "/vol/DCIM/100MEDIA/C0002.MP4",
            camera: "Canon",
            captured_at_raw: Some("2026-07-19T08:01:00Z"),
            duration_seconds: 10.0,
        },
    ],
    clock_events: &[ClockEventSpec {
        id: "e_canon",
        camera: "Canon",
        start_filename: "C0001.MP4",
        offset_seconds: 300,
        label: "canon event",
    }],
};

/// Non-ASCII filename prefix with a digit run, plus a best_time tie that is
/// broken by a non-ASCII filename comparison.
const SPEC_NON_ASCII_PREFIX: CatalogSpec = CatalogSpec {
    name: "non_ascii_prefix_digit_run",
    assets: &[
        AssetSpec {
            id: "n1",
            filename: "旅行_記録_0012.MOV",
            path: "/offline/japan/旅行_記録_0012.MOV",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:00:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "n2",
            filename: "旅行_記録_0013.MOV",
            path: "/offline/japan/旅行_記録_0013.MOV",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:01:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "n3",
            filename: "IMG_1234 (1).MOV",
            path: "/offline/japan/IMG_1234 (1).MOV",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:02:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "n4",
            filename: "沙滩_0007.MOV",
            path: "/offline/beach/沙滩_0007.MOV",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:03:00Z"),
            duration_seconds: 10.0,
        },
    ],
    clock_events: &[ClockEventSpec {
        id: "e_japan",
        camera: "DJI",
        start_filename: "旅行_記録_0012.MOV",
        offset_seconds: 60,
        label: "japan leg",
    }],
};

/// One camera whose filenames have no digit run (acceptance oracle shape 5).
const SPEC_NO_DIGIT_RUN: CatalogSpec = CatalogSpec {
    name: "no_digit_run_filenames",
    assets: &[
        AssetSpec {
            id: "m1",
            filename: "clip.mp4",
            path: "/offline/sony/clip.mp4",
            camera: "Sony",
            captured_at_raw: Some("2026-07-18T09:00:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "m2",
            filename: "beach.mov",
            path: "/offline/sony/beach.mov",
            camera: "Sony",
            captured_at_raw: Some("2026-07-18T09:01:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "m3",
            filename: "sunset.mov",
            path: "/offline/sony/sunset.mov",
            camera: "Sony",
            captured_at_raw: Some("2026-07-18T09:02:00Z"),
            duration_seconds: 10.0,
        },
    ],
    clock_events: &[ClockEventSpec {
        id: "e_sony",
        camera: "Sony",
        start_filename: "clip.mp4",
        offset_seconds: 300,
        label: "whole camera",
    }],
};

/// A filename whose digit run exceeds u64: must yield NULL, never panic.
const SPEC_HUGE_COUNTER: CatalogSpec = CatalogSpec {
    name: "huge_counter_filename",
    assets: &[
        AssetSpec {
            id: "h1",
            filename: "GX99999999999999999999.MP4",
            path: "/offline/dji/GX99999999999999999999.MP4",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:00:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "h2",
            filename: "GX010000.MP4",
            path: "/offline/dji/GX010000.MP4",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:01:00Z"),
            duration_seconds: 10.0,
        },
    ],
    clock_events: &[ClockEventSpec {
        id: "e_dji",
        camera: "DJI",
        start_filename: "GX010000.MP4",
        offset_seconds: 120,
        label: "dji event",
    }],
};

/// The frozen documented counter-parsing examples.
const SPEC_COUNTER_EXAMPLES: CatalogSpec = CatalogSpec {
    name: "counter_parsing_examples",
    assets: &[
        AssetSpec {
            id: "c1",
            filename: "C0101.MP4",
            path: "/offline/c/C0101.MP4",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:00:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "c2",
            filename: "DJI_0042.MP4",
            path: "/offline/c/DJI_0042.MP4",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:01:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "c3",
            filename: "clip.mp4",
            path: "/offline/c/clip.mp4",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:02:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "c4",
            filename: "GX010234.MP4",
            path: "/offline/c/GX010234.MP4",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:03:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "c5",
            filename: "IMG_1234 (1).MOV",
            path: "/offline/c/IMG_1234 (1).MOV",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:04:00Z"),
            duration_seconds: 10.0,
        },
        AssetSpec {
            id: "c6",
            filename: "GX99999999999999999999.MP4",
            path: "/offline/c/GX99999999999999999999.MP4",
            camera: "DJI",
            captured_at_raw: Some("2026-07-18T09:05:00Z"),
            duration_seconds: 10.0,
        },
    ],
    clock_events: &[],
};

/// 500 assets across three cameras with overlapping counter ranges and
/// interleaved captured times.
static LARGE_500_ASSETS: LazyLock<[AssetSpec; 500]> = LazyLock::new(large_500_assets);
static SPEC_LARGE_500: LazyLock<CatalogSpec> = LazyLock::new(|| CatalogSpec {
    name: "large_three_camera_500",
    assets: &*LARGE_500_ASSETS,
    clock_events: &[
        ClockEventSpec {
            id: "e_dji",
            camera: "DJI",
            start_filename: "C0050.MP4",
            offset_seconds: 3600,
            label: "dji noon",
        },
        ClockEventSpec {
            id: "e_gopro",
            camera: "GoPro",
            start_filename: "C0050.MP4",
            offset_seconds: 3600,
            label: "gopro noon",
        },
        ClockEventSpec {
            id: "e_insta",
            camera: "Insta360",
            start_filename: "I0050.MP4",
            offset_seconds: 7200,
            label: "insta noon",
        },
    ],
});

fn all_specs() -> Vec<&'static CatalogSpec<'static>> {
    vec![
        &SPEC_ONE_CAMERA_NO_EVENTS,
        &SPEC_ONE_CAMERA_ONE_EVENT,
        &SPEC_TWO_CAMERAS_ONE_EVENT_EACH,
        &*SPEC_OVERLAPPING_COUNTER_RANGES,
        &*SPEC_TWO_CAMERAS_THREE_EVENTS,
        &SPEC_EVENT_START_FILENAME_MISSING,
        &SPEC_NULL_CAPTURED_AT,
        &SPEC_IDENTICAL_RAW_AND_FILENAME,
        &SPEC_NON_ASCII_PREFIX,
        &SPEC_NO_DIGIT_RUN,
        &SPEC_HUGE_COUNTER,
        &SPEC_COUNTER_EXAMPLES,
        &*SPEC_LARGE_500,
    ]
}

fn leak_str(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

fn overlapping_counter_assets() -> [AssetSpec; 20] {
    let mut assets: [AssetSpec; 20] = core::array::from_fn(|index| {
        let camera = if index % 2 == 0 { "DJI" } else { "GoPro" };
        let counter = index / 2 + 1;
        let base = 9 * 3600 + (index * 120);
        let raw = leak_str(format!(
            "2026-07-18T{:02}:{:02}:00Z",
            base / 3600,
            (base % 3600) / 60
        ));
        AssetSpec {
            id: leak_str(format!("a{index}")),
            filename: leak_str(format!("C{counter:04}.MP4")),
            path: leak_str(format!("/offline/{camera}/C{counter:04}.MP4")),
            camera,
            captured_at_raw: Some(raw),
            duration_seconds: 10.0,
        }
    });
    // The order of rows in the table does not matter; the review order is
    // defined by the ORDER BY, but keep the ids ascending for readability.
    assets.sort_by_key(|a| a.id);
    assets
}

fn two_cameras_three_events_assets() -> [AssetSpec; 12] {
    let mut assets: Vec<AssetSpec> = Vec::with_capacity(12);
    for index in 0..8 {
        assets.push(AssetSpec {
            id: leak_str(format!("d{}", index + 1)),
            filename: leak_str(format!("C{:04}.MP4", index + 1)),
            path: leak_str(format!("/offline/dji/C{:04}.MP4", index + 1)),
            camera: "DJI",
            captured_at_raw: Some(leak_str(format!("2026-07-18T09:{:02}:00Z", index))),
            duration_seconds: 10.0,
        });
    }
    for index in 0..4 {
        assets.push(AssetSpec {
            id: leak_str(format!("g{}", index + 1)),
            filename: leak_str(format!("G{:04}.MP4", index + 1)),
            path: leak_str(format!("/offline/gopro/G{:04}.MP4", index + 1)),
            camera: "GoPro",
            captured_at_raw: Some(leak_str(format!("2026-07-18T10:{:02}:00Z", index))),
            duration_seconds: 10.0,
        });
    }
    assets.sort_by_key(|a| a.id);
    assets.try_into().expect("twelve assets")
}

fn large_500_assets() -> [AssetSpec; 500] {
    core::array::from_fn(|index| {
        let camera = match index % 3 {
            0 => "DJI",
            1 => "GoPro",
            _ => "Insta360",
        };
        let counter = index / 3 + 1;
        let prefix = if camera == "Insta360" { "I" } else { "C" };
        let base = 8 * 3600 + index * 60;
        let raw = if index % 100 == 99 {
            None
        } else {
            Some(leak_str(format!(
                "2026-07-20T{:02}:{:02}:00Z",
                base / 3600,
                (base % 3600) / 60
            )))
        };
        AssetSpec {
            id: leak_str(format!("a_{index:04}")),
            filename: leak_str(format!("{prefix}{counter:04}.MP4")),
            path: leak_str(format!("/offline/{camera}/{prefix}{counter:04}.MP4")),
            camera,
            captured_at_raw: raw,
            duration_seconds: 10.0,
        }
    })
}

// ---------------------------------------------------------------------------
// Catalog construction: migrations 1..=8 through the real runner, then direct
// row insertion, then the old-model projection.
// ---------------------------------------------------------------------------

/// Replicates `normalize_datetime` (src/lib.rs), the pre-migration production
/// projection used by `recalculate_clock`.
fn old_model_best_time(captured_at_raw: Option<&str>, offset_seconds: i64) -> Option<String> {
    let value = captured_at_raw?;
    if let Ok(datetime) = DateTime::parse_from_rfc3339(value) {
        return Some(
            (datetime.with_timezone(&Utc) + Duration::seconds(offset_seconds))
                .to_rfc3339_opts(SecondsFormat::Millis, true),
        );
    }
    for format in ["%Y-%m-%d %H:%M:%S", "%Y:%m:%d %H:%M:%S"] {
        if let Ok(datetime) = NaiveDateTime::parse_from_str(value, format) {
            return Some(
                (datetime.and_utc() + Duration::seconds(offset_seconds))
                    .to_rfc3339_opts(SecondsFormat::Millis, true),
            );
        }
    }
    None
}

/// An independently written implementation of the frozen counter rule
/// (extension stripped, trailing non-alphanumerics stripped, trailing digit
/// run parsed as u64, NULL on no run or overflow). Written differently from
/// the production parser on purpose.
fn independent_counter(filename: &str) -> Option<u64> {
    let stem = match filename.rfind('.') {
        Some(index) => &filename[..index],
        None => filename,
    };
    let stem = stem.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
    let digits: String = stem
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.chars().rev().collect::<String>().parse::<u64>().ok()
}

/// Runs migrations 1..=8 through the real production runner, inserts the
/// spec's rows directly, then applies the old-model projection. Returns the
/// path of the schema-8 catalog.
fn build_schema8_catalog(directory: &Path, spec: &CatalogSpec<'_>) -> PathBuf {
    let path = directory.join(format!("{}.sqlite3", spec.name));
    let mut connection = Connection::open(&path).expect("create schema-8 catalog");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    run_sqlite_migrations(&mut connection, &PROJECT_CATALOG_MIGRATIONS[..8])
        .expect("run migrations 1..=8");
    let transaction = connection
        .transaction()
        .expect("begin fixture insert transaction");
    transaction
        .execute(
            "INSERT INTO projects(id, name, created_at) VALUES (?1, ?2, ?3)",
            params![PROJECT, spec.name, "2026-07-01T00:00:00Z"],
        )
        .expect("insert project");
    for asset in spec.assets {
        transaction
            .execute(
                "INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    asset.id,
                    PROJECT,
                    format!("fingerprint_{}", asset.id),
                    asset.path,
                    asset.filename,
                    1000_i64,
                    asset.duration_seconds,
                    30.0,
                    1920_i32,
                    1080_i32,
                    "h264",
                    asset.camera,
                    asset.captured_at_raw,
                    "2026-07-01T00:00:00Z",
                ],
            )
            .expect("insert asset");
    }
    for event in spec.clock_events {
        transaction
            .execute(
                "INSERT INTO clock_events(id, project_id, camera, start_filename, offset_seconds, label, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    event.id,
                    PROJECT,
                    event.camera,
                    event.start_filename,
                    event.offset_seconds,
                    event.label,
                    "2026-07-18T09:00:00Z",
                ],
            )
            .expect("insert clock event");
    }
    // Old-model projection, replicating recalculate_clock (src/operations.rs):
    // per camera, the offset of the last event (ordered by start_filename,
    // then created_at, then insertion order) whose start_filename is
    // <= the asset's filename.
    let cameras: BTreeSet<&str> = spec
        .assets
        .iter()
        .map(|a| a.camera)
        .chain(spec.clock_events.iter().map(|e| e.camera))
        .collect();
    for camera in cameras {
        let mut events: Vec<(&str, &str, i64)> = spec
            .clock_events
            .iter()
            .filter(|e| e.camera == camera)
            .map(|e| (e.start_filename, e.id, e.offset_seconds))
            .collect();
        events.sort();
        for asset in spec.assets.iter().filter(|a| a.camera == camera) {
            let offset = events
                .iter()
                .filter(|(boundary, _, _)| *boundary <= asset.filename)
                .map(|(_, _, offset)| *offset)
                .next_back()
                .unwrap_or(0);
            let best = old_model_best_time(asset.captured_at_raw, offset);
            transaction
                .execute(
                    "UPDATE assets SET offset_seconds = ?2, best_time = ?3 WHERE id = ?1",
                    params![asset.id, offset, best],
                )
                .expect("project old-model best time");
        }
    }
    transaction.commit().expect("commit fixture inserts");
    path
}

fn fixture_catalog(spec: &CatalogSpec<'_>) -> (TempDir, PathBuf) {
    let committed = fixture_root().join(format!("{}.sqlite3", spec.name));
    let directory = tempfile::tempdir().expect("tempdir");
    if committed.is_file() {
        let copy = directory.path().join(format!("{}.sqlite3", spec.name));
        fs::copy(&committed, &copy).expect("copy committed schema-8 catalog");
        (directory, copy)
    } else {
        let built = build_schema8_catalog(directory.path(), spec);
        (directory, built)
    }
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE_DIR)
}

fn fixture_media_path() -> PathBuf {
    fixture_root().join("media").join("sample.mp4")
}

// ---------------------------------------------------------------------------
// Observation helpers
// ---------------------------------------------------------------------------

fn review_order(connection: &Connection, project_id: &str) -> Vec<String> {
    let mut statement = connection
        .prepare(REVIEW_ORDER_SQL)
        .expect("prepare review order query");
    statement
        .query_map([project_id], |row| row.get::<_, String>(0))
        .expect("run review order query")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect review order")
}

fn projection_snapshot(
    connection: &Connection,
    project_id: &str,
) -> Vec<(String, Option<String>, i64)> {
    let mut statement = connection
        .prepare(
            "SELECT id, best_time, offset_seconds FROM assets WHERE project_id = ?1 ORDER BY id",
        )
        .expect("prepare projection query");
    statement
        .query_map([project_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .expect("run projection query")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect projection")
}

fn assert_same_order(catalog_name: &str, before: &[String], after: &[String]) {
    assert_eq!(
        before.len(),
        after.len(),
        "{catalog_name}: asset count changed by migration: before {} after {}",
        before.len(),
        after.len()
    );
    for (index, (before_id, after_id)) in before.iter().zip(after).enumerate() {
        assert_eq!(
            before_id, after_id,
            "{catalog_name}: first differing review-order index {index}: before {before_id}, after {after_id}"
        );
    }
}

fn distinct_cameras(spec: &CatalogSpec<'_>) -> BTreeSet<&'static str> {
    spec.assets
        .iter()
        .map(|a| a.camera)
        .chain(spec.clock_events.iter().map(|e| e.camera))
        .collect()
}

/// Sequence id shared by migration 9 and the operation materializers,
/// per the frozen contract (`sequence_<project>_<blake3 hex of camera>`).
fn expected_sequence_id(project_id: &str, camera: &str) -> String {
    format!(
        "sequence_{}_{}",
        project_id,
        blake3::hash(camera.as_bytes()).to_hex()
    )
}

// ---------------------------------------------------------------------------
// Order-preservation verification for one catalog
// ---------------------------------------------------------------------------

fn verify_order_preservation(spec: &CatalogSpec<'_>) {
    let (_directory, path) = fixture_catalog(spec);
    let connection = Connection::open(&path).expect("open schema-8 catalog");
    let before_order = review_order(&connection, PROJECT);
    let before_projection = projection_snapshot(&connection, PROJECT);
    drop(connection);

    // The real migration seam: Catalog::open validates the history and runs
    // the production migration runner, which applies migration 9.
    let catalog = Catalog::open(&path).expect("migrate schema-8 catalog to schema 9");

    let after_order: Vec<String> = catalog
        .list_assets(PROJECT)
        .expect("list assets through production seam")
        .into_iter()
        .map(|asset| asset.id)
        .collect();
    assert_same_order(spec.name, &before_order, &after_order);

    let connection = Connection::open(&path).expect("reopen migrated catalog");
    let after_projection = projection_snapshot(&connection, PROJECT);
    assert_eq!(
        before_projection, after_projection,
        "{}: migration changed assets.best_time or assets.offset_seconds (projection write)",
        spec.name
    );

    let schema_version: i64 = connection
        .query_row(
            "SELECT MAX(migration_id) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .expect("read schema version");
    assert_eq!(
        schema_version, 9,
        "{}: catalog is not at schema 9",
        spec.name
    );
    let applied_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .expect("count applied migrations");
    assert_eq!(
        applied_count, 9,
        "{}: migration 9 applied exactly once",
        spec.name
    );

    // Every asset is assigned exactly one asset_capture_sequence row.
    let asset_count = spec.assets.len() as i64;
    let assigned: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM asset_capture_sequence WHERE project_id = ?1",
            [PROJECT],
            |row| row.get(0),
        )
        .expect("count assignments");
    assert_eq!(
        assigned, asset_count,
        "{}: expected one asset_capture_sequence row per asset",
        spec.name
    );
    let assigned_distinct: i64 = connection
        .query_row(
            "SELECT COUNT(DISTINCT asset_id) FROM asset_capture_sequence WHERE project_id = ?1",
            [PROJECT],
            |row| row.get(0),
        )
        .expect("count distinct assignments");
    assert_eq!(
        assigned_distinct, asset_count,
        "{}: an asset has more than one capture-sequence assignment",
        spec.name
    );

    // One capture_sequences row per distinct camera (assets union events).
    let expected_sequences = distinct_cameras(spec).len() as i64;
    let sequence_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM capture_sequences WHERE project_id = ?1",
            [PROJECT],
            |row| row.get(0),
        )
        .expect("count sequences");
    assert_eq!(
        sequence_count, expected_sequences,
        "{}: capture_sequences count does not match the distinct camera count",
        spec.name
    );
    for camera in distinct_cameras(spec) {
        let sequence_id = expected_sequence_id(PROJECT, camera);
        let (camera_key, label, volume_hint): (String, String, String) = connection
            .query_row(
                "SELECT camera_key, label, volume_hint FROM capture_sequences WHERE id = ?1",
                [&sequence_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read capture sequence");
        assert_eq!(camera_key, camera, "{}: camera_key carried over", spec.name);
        assert_eq!(
            label, camera,
            "{}: label defaults to the camera string",
            spec.name
        );
        assert_eq!(volume_hint, "", "{}: volume_hint is empty", spec.name);
    }

    // Clock events carried over one-to-one into clock_epochs.
    let epoch_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM clock_epochs WHERE project_id = ?1",
            [PROJECT],
            |row| row.get(0),
        )
        .expect("count epochs");
    assert_eq!(
        epoch_count,
        spec.clock_events.len() as i64,
        "{}: clock_epochs count does not match the pre-migration clock_events count",
        spec.name
    );
    for event in spec.clock_events {
        let (sequence_id, start_counter, end_counter, offset_seconds, confidence, evidence, label): (
            String,
            i64,
            Option<i64>,
            i64,
            f64,
            String,
            String,
        ) = connection
            .query_row(
                "SELECT capture_sequence_id, start_counter, end_counter, offset_seconds, confidence, evidence_json, label FROM clock_epochs WHERE id = ?1 AND project_id = ?2",
                params![event.id, PROJECT],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
            )
            .expect("read migrated epoch");
        assert_eq!(
            sequence_id,
            expected_sequence_id(PROJECT, event.camera),
            "{}: epoch {} lands on its camera's sequence",
            spec.name,
            event.id
        );
        assert_eq!(
            start_counter,
            independent_counter(event.start_filename).unwrap_or(0) as i64,
            "{}: epoch {} start counter",
            spec.name,
            event.id
        );
        assert_eq!(
            end_counter, None,
            "{}: epoch {} end counter is NULL",
            spec.name, event.id
        );
        assert_eq!(
            offset_seconds, event.offset_seconds,
            "{}: epoch {} offset",
            spec.name, event.id
        );
        assert_eq!(
            confidence, 1.0,
            "{}: epoch {} confidence",
            spec.name, event.id
        );
        assert_eq!(
            evidence, r#"["migrated_clock_event"]"#,
            "{}: epoch {} evidence",
            spec.name, event.id
        );
        assert_eq!(
            label, event.label,
            "{}: epoch {} label",
            spec.name, event.id
        );
    }
}

// ---------------------------------------------------------------------------
// Tests: order preservation per fixture catalog
// ---------------------------------------------------------------------------

#[test]
fn review_order_identical_single_camera_no_clock_events() {
    verify_order_preservation(&SPEC_ONE_CAMERA_NO_EVENTS);
}

#[test]
fn review_order_identical_single_camera_one_clock_event() {
    verify_order_preservation(&SPEC_ONE_CAMERA_ONE_EVENT);
}

#[test]
fn review_order_identical_two_cameras_one_clock_event_each() {
    verify_order_preservation(&SPEC_TWO_CAMERAS_ONE_EVENT_EACH);
}

#[test]
fn review_order_identical_overlapping_counter_ranges() {
    verify_order_preservation(&SPEC_OVERLAPPING_COUNTER_RANGES);
}

#[test]
fn review_order_identical_two_cameras_three_events_on_one_camera() {
    verify_order_preservation(&SPEC_TWO_CAMERAS_THREE_EVENTS);
}

#[test]
fn review_order_identical_clock_event_start_filename_missing_from_assets() {
    verify_order_preservation(&SPEC_EVENT_START_FILENAME_MISSING);
}

#[test]
fn review_order_identical_null_captured_at() {
    verify_order_preservation(&SPEC_NULL_CAPTURED_AT);
}

#[test]
fn review_order_identical_identical_raw_and_filename_in_different_folders() {
    verify_order_preservation(&SPEC_IDENTICAL_RAW_AND_FILENAME);
}

#[test]
fn review_order_identical_non_ascii_prefix_and_digit_run() {
    verify_order_preservation(&SPEC_NON_ASCII_PREFIX);
}

#[test]
fn review_order_identical_no_digit_run_filenames() {
    verify_order_preservation(&SPEC_NO_DIGIT_RUN);
}

#[test]
fn review_order_identical_huge_counter_filename() {
    verify_order_preservation(&SPEC_HUGE_COUNTER);
}

#[test]
fn review_order_identical_500_assets_across_three_cameras() {
    verify_order_preservation(&SPEC_LARGE_500);
}

#[test]
fn review_order_identical_for_every_fixture_catalog() {
    for spec in all_specs() {
        verify_order_preservation(spec);
    }
}

// ---------------------------------------------------------------------------
// Counter parsing against the frozen documented examples
// ---------------------------------------------------------------------------

#[test]
fn filename_counter_parsing_matches_the_frozen_documented_examples() {
    let (_directory, path) = fixture_catalog(&SPEC_COUNTER_EXAMPLES);
    let catalog = Catalog::open(&path).expect("migrate");
    let facts = catalog.chronology_facts(PROJECT).expect("load facts");
    let counters: std::collections::BTreeMap<&str, Option<u64>> = facts
        .assets
        .iter()
        .map(|asset| (asset.asset_id.as_str(), asset.filename_counter))
        .collect();
    assert_eq!(counters["c1"], Some(101), "C0101.MP4 -> 101");
    assert_eq!(counters["c2"], Some(42), "DJI_0042.MP4 -> 42");
    assert_eq!(counters["c3"], None, "clip.mp4 -> NULL");
    assert_eq!(counters["c4"], Some(10234), "GX010234.MP4 -> 10234");
    assert_eq!(counters["c5"], Some(1), "IMG_1234 (1).MOV -> 1");
    assert_eq!(counters["c6"], None, "u64 overflow -> NULL, no panic");
}

// ---------------------------------------------------------------------------
// Idempotence
// ---------------------------------------------------------------------------

#[test]
fn migrating_twice_changes_no_rows_and_no_review_order() {
    let spec = &SPEC_TWO_CAMERAS_ONE_EVENT_EACH;
    let (_directory, path) = fixture_catalog(spec);
    let connection = Connection::open(&path).expect("open schema-8 catalog");
    let before_order = review_order(&connection, PROJECT);
    drop(connection);

    let catalog = Catalog::open(&path).expect("first migration");
    let first_order: Vec<String> = catalog
        .list_assets(PROJECT)
        .expect("list assets")
        .into_iter()
        .map(|asset| asset.id)
        .collect();
    assert_same_order(spec.name, &before_order, &first_order);
    let first_counts = table_counts(&path);

    // Run the production migration runner a second time over the same file.
    let mut connection = Connection::open(&path).expect("reopen migrated catalog");
    run_sqlite_migrations(&mut connection, PROJECT_CATALOG_MIGRATIONS)
        .expect("second migration run");
    drop(connection);

    let second_counts = table_counts(&path);
    assert_eq!(
        first_counts, second_counts,
        "second migration run changed row counts"
    );
    let reopened = Catalog::open(&path).expect("reopen after second migration");
    let second_order: Vec<String> = reopened
        .list_assets(PROJECT)
        .expect("list assets after second migration")
        .into_iter()
        .map(|asset| asset.id)
        .collect();
    assert_eq!(
        first_order, second_order,
        "second migration changed review order"
    );
}

fn table_counts(path: &Path) -> (i64, i64, i64, i64, i64) {
    let connection = Connection::open(path).expect("open catalog for counts");
    let count = |sql: &str| -> i64 {
        connection
            .query_row(sql, [], |row| row.get(0))
            .expect("count rows")
    };
    (
        count("SELECT COUNT(*) FROM schema_migrations"),
        count("SELECT COUNT(*) FROM capture_sequences"),
        count("SELECT COUNT(*) FROM asset_capture_sequence"),
        count("SELECT COUNT(*) FROM clock_epochs"),
        count("SELECT COUNT(*) FROM clock_events"),
    )
}

// ---------------------------------------------------------------------------
// clock_events retention (rollback possible)
// ---------------------------------------------------------------------------

#[test]
fn clock_events_table_is_retained_with_its_original_rows() {
    let spec = &SPEC_ONE_CAMERA_ONE_EVENT;
    let (_directory, path) = fixture_catalog(spec);
    let connection = Connection::open(&path).expect("open schema-8 catalog");
    let before_rows = clock_event_rows(&connection);
    drop(connection);

    Catalog::open(&path).expect("migrate");

    let connection = Connection::open(&path).expect("reopen migrated catalog");
    let after_rows = clock_event_rows(&connection);
    assert_eq!(
        before_rows, after_rows,
        "clock_events rows changed; rollback of migration 9 would lose data"
    );
    let table_exists: i64 = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'clock_events')",
            [],
            |row| row.get(0),
        )
        .expect("clock_events table existence");
    assert_eq!(table_exists, 1, "clock_events table was dropped");
}

fn clock_event_rows(connection: &Connection) -> Vec<(String, String, String, i64, String, String)> {
    let mut statement = connection
        .prepare(
            "SELECT id, camera, start_filename, offset_seconds, label, created_at FROM clock_events WHERE project_id = ?1 ORDER BY id",
        )
        .expect("prepare clock_events snapshot");
    statement
        .query_map([PROJECT], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .expect("run clock_events snapshot")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect clock_events snapshot")
}

// ---------------------------------------------------------------------------
// Quarantine policy attack
// ---------------------------------------------------------------------------

#[test]
fn unknown_operation_type_with_future_payload_version_is_quarantined_not_fatal() {
    let (_directory, path) = fixture_catalog(&SPEC_ONE_CAMERA_ONE_EVENT);
    let catalog = Catalog::open(&path).expect("migrate");
    let wall = Utc::now().timestamp_millis();
    let operation = |id: &'static str,
                     sequence: u64,
                     aggregate_type: &'static str,
                     aggregate_id: &'static str,
                     operation_type: &'static str,
                     payload_version: u32,
                     payload: serde_json::Value| ProjectOperation {
        operation_id: id.to_string(),
        project_id: PROJECT.to_string(),
        origin_node_id: "node_peer".to_string(),
        origin_sequence: sequence,
        logical_time: HybridLogicalTime {
            wall_millis: wall,
            counter: 0,
            node_id: "node_peer".to_string(),
        },
        aggregate_type: aggregate_type.to_string(),
        aggregate_id: aggregate_id.to_string(),
        operation_type: operation_type.to_string(),
        payload_version,
        base_revision: None,
        payload,
    };
    let batch = [
        operation(
            "op_known_1",
            1,
            "projectAsset",
            "a1",
            "removedSet",
            1,
            json!({"removed": true}),
        ),
        operation(
            "op_known_2",
            2,
            "projectAsset",
            "a2",
            "clipTimeExceptionSet",
            1,
            json!({"tripTime": "2026-07-01T00:00:00Z", "reason": "charter batch"}),
        ),
        operation(
            "op_known_3",
            3,
            "projectAsset",
            "a3",
            "noteRevisionCreated",
            1,
            json!({"revisionId": "note_q", "text": "quarantine batch"}),
        ),
        operation(
            "op_unknown_1",
            4,
            "futureAggregate",
            "future_1",
            "someFutureOperation",
            2,
            json!({"futureField": "futureValue"}),
        ),
    ];
    let result: OperationBatchResult = catalog
        .apply_operation_batch("node_peer", &batch)
        .expect("batch with an unknown operation must not fail");
    assert_eq!(result.applied, 3, "the three known operations must apply");
    assert_eq!(
        result.quarantined, 1,
        "the unknown operation must be quarantined"
    );
    assert_eq!(result.conflicts, 0);
    assert_eq!(result.duplicates, 0);
    assert_eq!(
        result.committed_sequence, 4,
        "the batch cursor must advance"
    );

    // The three known operations materialized.
    assert!(
        catalog.get_asset("a1").expect("a1").removed,
        "removedSet applied"
    );
    assert_eq!(
        catalog.get_asset("a3").expect("a3").notes,
        "quarantine batch",
        "noteRevisionCreated applied"
    );
    let exception_count: i64 = connection_query(
        &path,
        "SELECT COUNT(*) FROM clip_time_exceptions WHERE project_id = ?1 AND asset_id = 'a2'",
        rusqlite::params![PROJECT],
        |row| row.get(0),
    )
    .expect("exception row");
    assert_eq!(exception_count, 1, "clipTimeExceptionSet applied");

    // The unknown operation is reported through the public loader...
    let quarantined = catalog
        .quarantined_operations(PROJECT)
        .expect("quarantined operations");
    assert_eq!(quarantined.len(), 1, "exactly one quarantined operation");
    assert_eq!(quarantined[0].operation_id, "op_unknown_1");
    assert_eq!(quarantined[0].operation_type, "someFutureOperation");
    assert_eq!(quarantined[0].aggregate_type, "futureAggregate");
    assert_eq!(quarantined[0].aggregate_id, "future_1");
    assert_eq!(
        quarantined[0].payload_version, 2,
        "future payload versions are not rejected"
    );

    // ...is durable in project_operations with its full payload...
    let connection = Connection::open(&path).expect("open catalog");
    let payload_json: String = connection
        .query_row(
            "SELECT payload_json FROM project_operations WHERE operation_id = 'op_unknown_1'",
            [],
            |row| row.get(0),
        )
        .expect("unknown operation payload");
    assert!(
        payload_json.contains("futureValue"),
        "unknown operation must be durable with its full payload"
    );
    let stored_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM project_operations WHERE project_id = ?1",
            [PROJECT],
            |row| row.get(0),
        )
        .expect("project_operations count");
    assert_eq!(stored_count, 4, "all four operations persisted");
    let revision: Option<i64> = connection
        .query_row(
            "SELECT revision FROM materialized_revisions WHERE project_id = ?1 AND aggregate_type = 'futureAggregate' AND aggregate_id = 'future_1'",
            [PROJECT],
            |row| row.get(0),
        )
        .optional()
        .expect("revision lookup");
    assert_eq!(
        revision, None,
        "no revision may advance for the unknown aggregate"
    );
    let cursor: i64 = connection
        .query_row(
            "SELECT committed_sequence FROM sync_origin_cursors WHERE origin_node_id = 'node_peer'",
            [],
            |row| row.get(0),
        )
        .expect("batch cursor");
    assert_eq!(cursor, 4, "peer must not resend the quarantined operation");
    // received_at must be the operation's created_at.
    let created_at: String = connection
        .query_row(
            "SELECT created_at FROM project_operations WHERE operation_id = 'op_unknown_1'",
            [],
            |row| row.get(0),
        )
        .expect("unknown operation created_at");
    assert_eq!(
        quarantined[0].received_at, created_at,
        "received_at is created_at"
    );
}

fn connection_query<T: rusqlite::types::FromSql>(
    path: &Path,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
    map: impl FnOnce(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<T> {
    let connection = Connection::open(path)?;
    Ok(connection.query_row(sql, params, map)?)
}

// ---------------------------------------------------------------------------
// Revision guard attack
// ---------------------------------------------------------------------------

#[test]
fn stale_base_revision_produces_conflict_rows_and_never_overwrites() {
    let (_directory, path) = fixture_catalog(&SPEC_ONE_CAMERA_ONE_EVENT);
    let catalog = Catalog::open(&path).expect("migrate");
    let connection = Connection::open(&path).expect("open catalog");
    let sequence_id: String = connection
        .query_row(
            "SELECT id FROM capture_sequences WHERE project_id = ?1 LIMIT 1",
            [PROJECT],
            |row| row.get(0),
        )
        .expect("capture sequence id");
    drop(connection);

    let wall = Utc::now().timestamp_millis();
    let epoch_set = |id: &'static str,
                     node: &'static str,
                     base: Option<u64>,
                     start_counter: u64,
                     offset: i64,
                     label: &'static str| ProjectOperation {
        operation_id: id.to_string(),
        project_id: PROJECT.to_string(),
        origin_node_id: node.to_string(),
        origin_sequence: 1,
        logical_time: HybridLogicalTime {
            wall_millis: wall,
            counter: 0,
            node_id: node.to_string(),
        },
        aggregate_type: "clockEpoch".to_string(),
        aggregate_id: "epoch_guard".to_string(),
        operation_type: "clockEpochSet".to_string(),
        payload_version: 1,
        base_revision: base,
        payload: json!({
            "epochId": "epoch_guard",
            "captureSequenceId": sequence_id,
            "startCounter": start_counter,
            "offsetSeconds": offset,
            "label": label,
        }),
    };
    let first = epoch_set("op_guard_1", "node_a", Some(0), 1, 30, "first");
    let stale_a = epoch_set("op_guard_2", "node_b", Some(0), 2, 999, "stale a");
    let stale_b = epoch_set("op_guard_3", "node_c", Some(0), 3, 999, "stale b");

    assert_eq!(
        catalog.apply_operation(&first).expect("apply first"),
        ApplyOperationResult::Applied,
        "base revision 0 matches the fresh aggregate"
    );
    assert_eq!(
        catalog.apply_operation(&stale_a).expect("apply stale a"),
        ApplyOperationResult::Conflict,
        "stale base revision must be refused"
    );
    assert_eq!(
        catalog.apply_operation(&stale_b).expect("apply stale b"),
        ApplyOperationResult::Conflict,
        "second stale application must also be refused"
    );

    // Never an overwrite: the epoch row still carries the first payload.
    let connection = Connection::open(&path).expect("open catalog");
    let (start_counter, offset_seconds, label): (i64, i64, String) = connection
        .query_row(
            "SELECT start_counter, offset_seconds, label FROM clock_epochs WHERE id = 'epoch_guard'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("epoch row");
    assert_eq!(
        (start_counter, offset_seconds, label.as_str()),
        (1, 30, "first"),
        "stale operations silently overwrote the epoch"
    );

    // Two durable conflict rows describing the staleness.
    let mut statement = connection
        .prepare(
            "SELECT expected_base_revision, actual_revision, conflict_type FROM operation_conflicts WHERE project_id = ?1 AND aggregate_id = 'epoch_guard' ORDER BY conflict_id",
        )
        .expect("prepare conflicts");
    let conflicts: Vec<(Option<i64>, i64, String)> = statement
        .query_map([PROJECT], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("run conflicts")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect conflicts");
    assert_eq!(conflicts.len(), 2, "expected two conflict rows");
    for (expected, actual, conflict_type) in &conflicts {
        assert_eq!(*expected, Some(0), "conflict records the expected base");
        assert_eq!(*actual, 1, "conflict records the actual revision");
        assert_eq!(conflict_type, "stale_base_revision", "conflict type");
    }

    // The aggregate revision advanced exactly once.
    let revision: i64 = connection
        .query_row(
            "SELECT revision FROM materialized_revisions WHERE project_id = ?1 AND aggregate_type = 'clockEpoch' AND aggregate_id = 'epoch_guard'",
            [PROJECT],
            |row| row.get(0),
        )
        .expect("clockEpoch revision");
    assert_eq!(
        revision, 1,
        "only the first application may advance the revision"
    );

    // Replaying the same operation id is a duplicate, not a third overwrite.
    assert_eq!(
        catalog.apply_operation(&first).expect("replay first"),
        ApplyOperationResult::Duplicate
    );
    let conflict_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM operation_conflicts WHERE project_id = ?1 AND aggregate_id = 'epoch_guard'",
            [PROJECT],
            |row| row.get(0),
        )
        .expect("conflict count after replay");
    assert_eq!(
        conflict_count, 2,
        "duplicate replay must not add a conflict row"
    );
}

// ---------------------------------------------------------------------------
// Media immutability
// ---------------------------------------------------------------------------

#[test]
fn media_file_is_untouched_by_migration_and_operation_cycle() {
    let media_path = fixture_media_path();
    ensure_media_file(&media_path);
    let before_hash = file_hash(&media_path);
    let before_entries = directory_entries(media_path.parent().expect("media dir"));

    // Build a schema-8 catalog whose asset path points at the media file.
    let directory = tempfile::tempdir().expect("tempdir");
    let path_string: &'static str =
        Box::leak(media_path.to_string_lossy().into_owned().into_boxed_str());
    let media_assets = [AssetSpec {
        id: "media_1",
        filename: "C0001.MP4",
        path: path_string,
        camera: "DJI",
        captured_at_raw: Some("2026-07-18T09:00:00Z"),
        duration_seconds: 10.0,
    }];
    let media_events = [ClockEventSpec {
        id: "e_media",
        camera: "DJI",
        start_filename: "C0001.MP4",
        offset_seconds: 60,
        label: "media event",
    }];
    let spec = CatalogSpec {
        name: "media_asset",
        assets: &media_assets,
        clock_events: &media_events,
    };
    let built = build_schema8_catalog(directory.path(), &spec);

    // Full migration plus a full chronology operation cycle.
    let catalog = Catalog::open(&built).expect("migrate");
    let wall = Utc::now().timestamp_millis();
    let connection = Connection::open(&built).expect("open catalog");
    let sequence_id: String = connection
        .query_row(
            "SELECT id FROM capture_sequences WHERE project_id = ?1",
            [PROJECT],
            |row| row.get(0),
        )
        .expect("sequence id");
    drop(connection);
    let epoch = ProjectOperation {
        operation_id: "op_media_epoch".to_string(),
        project_id: PROJECT.to_string(),
        origin_node_id: "node_media".to_string(),
        origin_sequence: 1,
        logical_time: HybridLogicalTime {
            wall_millis: wall,
            counter: 0,
            node_id: "node_media".to_string(),
        },
        aggregate_type: "clockEpoch".to_string(),
        aggregate_id: "epoch_media".to_string(),
        operation_type: "clockEpochSet".to_string(),
        payload_version: 1,
        base_revision: None,
        payload: json!({"epochId": "epoch_media", "captureSequenceId": sequence_id, "startCounter": 1, "offsetSeconds": 300, "label": "media epoch"}),
    };
    assert_eq!(
        catalog
            .apply_operation(&epoch)
            .expect("apply clockEpochSet"),
        ApplyOperationResult::Applied
    );
    let exception = ProjectOperation {
        operation_id: "op_media_exception".to_string(),
        project_id: PROJECT.to_string(),
        origin_node_id: "node_media".to_string(),
        origin_sequence: 2,
        logical_time: HybridLogicalTime {
            wall_millis: wall,
            counter: 1,
            node_id: "node_media".to_string(),
        },
        aggregate_type: "projectAsset".to_string(),
        aggregate_id: "media_1".to_string(),
        operation_type: "clipTimeExceptionSet".to_string(),
        payload_version: 1,
        base_revision: None,
        payload: json!({"tripTime": "2026-07-18T10:00:00Z", "reason": "media test"}),
    };
    assert_eq!(
        catalog
            .apply_operation(&exception)
            .expect("apply clipTimeExceptionSet"),
        ApplyOperationResult::Applied
    );

    let after_hash = file_hash(&media_path);
    let after_entries = directory_entries(media_path.parent().expect("media dir"));
    assert_eq!(
        before_hash, after_hash,
        "migration or operations wrote to the media file"
    );
    assert_eq!(
        before_entries, after_entries,
        "migration or operations created files next to the media file"
    );
}

fn ensure_media_file(path: &Path) {
    if path.is_file() {
        return;
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create media dir");
    }
    let mut bytes = b"D01-T007-independent-media-fixture\0".to_vec();
    bytes.extend((0..2048).map(|index| ((index * 31 + 7) % 251) as u8));
    fs::write(path, bytes).expect("write media fixture");
}

fn file_hash(path: &Path) -> String {
    let bytes = fs::read(path).expect("read media file");
    blake3::hash(&bytes).to_hex().to_string()
}

fn directory_entries(path: &Path) -> Vec<String> {
    let mut entries: Vec<String> = fs::read_dir(path)
        .expect("read directory")
        .map(|entry| {
            entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    entries.sort();
    entries
}

// ---------------------------------------------------------------------------
// Newer-schema refusal
// ---------------------------------------------------------------------------

#[test]
fn catalog_newer_than_this_build_is_refused_not_migrated_downward() {
    let (_directory, path) = fixture_catalog(&SPEC_ONE_CAMERA_ONE_EVENT);
    Catalog::open(&path).expect("migrate to schema 9");
    let connection = Connection::open(&path).expect("open catalog");
    connection
        .execute(
            "INSERT INTO schema_migrations(migration_id, name, source_identity, applied_at) VALUES (10, 'future_migration', 'project-catalog/0010-future/20990101', '2099-01-01T00:00:00Z')",
            [],
        )
        .expect("stamp a future migration");
    drop(connection);

    let error = Catalog::open(&path).expect_err("a schema-10 catalog must be refused");
    let schema_error = error
        .downcast_ref::<CatalogSchemaError>()
        .expect("error must be a CatalogSchemaError");
    match schema_error {
        CatalogSchemaError::NewerSchema { found, supported } => {
            assert_eq!(*found, 10);
            assert_eq!(*supported, 9);
        }
        other => panic!("expected NewerSchema, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Loader determinism
// ---------------------------------------------------------------------------

#[test]
fn chronology_facts_loader_is_deterministic_and_sorted() {
    let (_directory, path) = fixture_catalog(&SPEC_TWO_CAMERAS_THREE_EVENTS);
    let catalog = Catalog::open(&path).expect("migrate");
    let first = catalog.chronology_facts(PROJECT).expect("load facts");
    for _ in 0..32 {
        let facts = catalog.chronology_facts(PROJECT).expect("load facts again");
        assert_eq!(
            facts.assets, first.assets,
            "assets collection changed between calls"
        );
        assert_eq!(
            facts.sequences, first.sequences,
            "sequences collection changed between calls"
        );
        assert_eq!(
            facts.epochs, first.epochs,
            "epochs collection changed between calls"
        );
        assert_eq!(
            facts.exceptions, first.exceptions,
            "exceptions collection changed between calls"
        );
        assert_eq!(
            facts.phases, first.phases,
            "phases collection changed between calls"
        );
        assert_eq!(
            facts.constraints, first.constraints,
            "constraints collection changed between calls"
        );
    }
    assert!(
        first
            .assets
            .windows(2)
            .all(|pair| pair[0].asset_id <= pair[1].asset_id),
        "assets not sorted by id"
    );
    assert!(
        first
            .sequences
            .windows(2)
            .all(|pair| pair[0].id <= pair[1].id),
        "sequences not sorted by id"
    );
    assert!(
        first.epochs.windows(2).all(|pair| pair[0].id <= pair[1].id),
        "epochs not sorted by id"
    );
    assert!(
        first
            .exceptions
            .windows(2)
            .all(|pair| pair[0].asset_id <= pair[1].asset_id),
        "exceptions not sorted by asset_id"
    );
    assert!(
        first.phases.windows(2).all(|pair| pair[0].id <= pair[1].id),
        "phases not sorted by id"
    );
    assert!(
        first
            .constraints
            .windows(2)
            .all(|pair| pair[0].id <= pair[1].id),
        "constraints not sorted by id"
    );
}

// ---------------------------------------------------------------------------
// Legacy replay after migration
// ---------------------------------------------------------------------------

#[test]
fn legacy_clock_event_replay_after_migration_materializes_into_clock_epochs() {
    let (_directory, path) = fixture_catalog(&SPEC_ONE_CAMERA_ONE_EVENT);
    let catalog = Catalog::open(&path).expect("migrate");
    let wall = Utc::now().timestamp_millis();
    let replay = ProjectOperation {
        operation_id: "op_legacy_replay".to_string(),
        project_id: PROJECT.to_string(),
        origin_node_id: "node_legacy".to_string(),
        origin_sequence: 1,
        logical_time: HybridLogicalTime {
            wall_millis: wall,
            counter: 0,
            node_id: "node_legacy".to_string(),
        },
        aggregate_type: "clock".to_string(),
        aggregate_id: "DJI".to_string(),
        operation_type: "clockEventCreated".to_string(),
        payload_version: 1,
        base_revision: Some(0),
        payload: json!({"eventId": "clock_replay", "camera": "DJI", "startFilename": "C0003.MP4", "offsetSeconds": 1800, "label": "replayed"}),
    };
    assert_eq!(
        catalog
            .apply_operation(&replay)
            .expect("replay legacy clock event"),
        ApplyOperationResult::Applied,
        "the legacy operation log must still replay after migration 9"
    );

    let connection = Connection::open(&path).expect("open catalog");
    let events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM clock_events WHERE project_id = ?1",
            [PROJECT],
            |row| row.get(0),
        )
        .expect("clock_events count");
    assert_eq!(events, 2, "legacy replay wrote its clock_events row");
    let epochs: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM clock_epochs WHERE project_id = ?1",
            [PROJECT],
            |row| row.get(0),
        )
        .expect("clock_epochs count");
    assert_eq!(epochs, 2, "legacy replay materialized into clock_epochs");

    // The replayed epoch has exactly the shape migration 9 produces.
    let (sequence_id, start_counter, offset_seconds, evidence, label): (String, i64, i64, String, String) =
        connection
            .query_row(
                "SELECT capture_sequence_id, start_counter, offset_seconds, evidence_json, label FROM clock_epochs WHERE id = 'clock_replay'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .expect("replayed epoch");
    assert_eq!(sequence_id, expected_sequence_id(PROJECT, "DJI"));
    assert_eq!(start_counter, 3, "C0003.MP4 -> counter 3");
    assert_eq!(offset_seconds, 1800);
    assert_eq!(evidence, r#"["migrated_clock_event"]"#);
    assert_eq!(label, "replayed");

    // A second legacy event with a stale base revision conflicts instead of
    // overwriting, through the legacy guard.
    let stale = ProjectOperation {
        operation_id: "op_legacy_stale".to_string(),
        project_id: PROJECT.to_string(),
        origin_node_id: "node_legacy".to_string(),
        origin_sequence: 2,
        logical_time: HybridLogicalTime {
            wall_millis: wall,
            counter: 1,
            node_id: "node_legacy".to_string(),
        },
        aggregate_type: "clock".to_string(),
        aggregate_id: "DJI".to_string(),
        operation_type: "clockEventCreated".to_string(),
        payload_version: 1,
        base_revision: Some(0),
        payload: json!({"eventId": "clock_stale", "camera": "DJI", "startFilename": "C0004.MP4", "offsetSeconds": 9000, "label": "stale"}),
    };
    assert_eq!(
        catalog
            .apply_operation(&stale)
            .expect("apply stale legacy event"),
        ApplyOperationResult::Conflict,
        "legacy guard must refuse a stale base revision"
    );
    let events_after: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM clock_events WHERE project_id = ?1",
            [PROJECT],
            |row| row.get(0),
        )
        .expect("clock_events count after conflict");
    assert_eq!(
        events_after, 2,
        "conflicted legacy event must not materialize"
    );
    let conflict_type: String = connection
        .query_row(
            "SELECT conflict_type FROM operation_conflicts WHERE incoming_operation_id = 'op_legacy_stale'",
            [],
            |row| row.get(0),
        )
        .expect("legacy conflict type");
    assert_eq!(conflict_type, "stale_clock_base_revision");
}

// ---------------------------------------------------------------------------
// Fixture regeneration (committed artifacts provenance)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "regenerates the committed schema-8 catalogs; run with --ignored"]
fn rebuild_committed_fixture_catalogs() {
    let catalog_dir = fixture_root();
    fs::create_dir_all(&catalog_dir).expect("create catalog fixture dir");
    for spec in all_specs() {
        let directory = tempfile::tempdir().expect("tempdir");
        let built = build_schema8_catalog(directory.path(), spec);
        let destination = catalog_dir.join(format!("{}.sqlite3", spec.name));
        fs::copy(&built, &destination).expect("copy catalog into fixture dir");
    }
    ensure_media_file(&fixture_media_path());
}
