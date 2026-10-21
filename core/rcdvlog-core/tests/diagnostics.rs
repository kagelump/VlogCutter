//! Acceptance oracle for the diagnostics surface (D01-T027).

use rcdvlog_core::{
    Catalog, DiagnosticFilter, DiagnosticRecord, DiagnosticSeverity, DiagnosticSubject,
    ProjectAssetAddedPayload,
};
use std::path::Path;
use tempfile::tempdir;

fn record(dedupe_key: &str, kind: &str, severity: DiagnosticSeverity) -> DiagnosticRecord {
    DiagnosticRecord {
        dedupe_key: dedupe_key.to_string(),
        severity,
        kind: kind.to_string(),
        subject: DiagnosticSubject::Project,
        message: "something the user needs to know".to_string(),
        context: serde_json::json!({ "detail": 1 }),
    }
}

fn open(path: &Path) -> Catalog {
    Catalog::open(path.join("catalog.sqlite3")).unwrap()
}

/// Oracle 1: migration 10 is additive. An existing project's asset list and its
/// order are byte-identical across the upgrade, and `diagnostics` starts empty.
#[test]
fn migration_ten_is_additive_and_starts_empty() {
    let directory = tempdir().unwrap();
    let catalog = open(directory.path());
    let project = catalog.create_project("Antarctica").unwrap();
    for index in 0..5 {
        catalog
            .add_portable_project_asset(
                &project.id,
                ProjectAssetAddedPayload {
                    id: format!("asset_{index}"),
                    fingerprint: format!("fingerprint_{index}"),
                    filename: format!("C{index:04}.MP4"),
                    size_bytes: 1024,
                    duration_seconds: 4.0,
                    frame_rate: 30.0,
                    width: 1920,
                    height: 1080,
                    codec: "h264".to_string(),
                    camera: "DJIPocket3".to_string(),
                    captured_at_raw: Some("2026-01-01T00:00:00Z".to_string()),
                    timestamp_candidates: None,
                    best_time: Some("2026-01-01T00:00:00Z".to_string()),
                    metadata_rotation_degrees: 0,
                    default_rotation_degrees: 0,
                    created_at: "2026-07-20T00:00:00.000Z".to_string(),
                },
            )
            .unwrap();
    }
    let order_before: Vec<String> = catalog
        .list_assets(&project.id)
        .unwrap()
        .into_iter()
        .map(|asset| asset.id)
        .collect();
    drop(catalog);

    // Reopening runs the full migration chain again; nothing may move.
    let reopened = Catalog::open_existing(directory.path().join("catalog.sqlite3")).unwrap();
    let order_after: Vec<String> = reopened
        .list_assets(&project.id)
        .unwrap()
        .into_iter()
        .map(|asset| asset.id)
        .collect();
    assert_eq!(order_before, order_after);
    assert!(
        reopened
            .diagnostics(&project.id, &DiagnosticFilter::default())
            .unwrap()
            .is_empty()
    );
}

/// Oracle 2: a diagnostic survives relaunch with everything intact.
#[test]
fn a_diagnostic_survives_relaunch() {
    let directory = tempdir().unwrap();
    let catalog = open(directory.path());
    let project = catalog.create_project("Antarctica").unwrap();
    catalog
        .record_diagnostic(
            &project.id,
            DiagnosticRecord {
                dedupe_key: "media.volume_offline:/Volumes/Acasis".to_string(),
                severity: DiagnosticSeverity::Error,
                kind: "media.volume_offline".to_string(),
                subject: DiagnosticSubject::Volume {
                    volume_hint: "/Volumes/Acasis".to_string(),
                },
                message: "the card is not mounted".to_string(),
                context: serde_json::json!({ "affectedAssets": 420 }),
            },
        )
        .unwrap();
    drop(catalog);

    let reopened = Catalog::open_existing(directory.path().join("catalog.sqlite3")).unwrap();
    let found = reopened
        .diagnostics(&project.id, &DiagnosticFilter::default())
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].severity, DiagnosticSeverity::Error);
    assert_eq!(found[0].message, "the card is not mounted");
    assert_eq!(
        found[0].subject,
        DiagnosticSubject::Volume {
            volume_hint: "/Volumes/Acasis".to_string()
        }
    );
    assert_eq!(found[0].context["affectedAssets"], 420);
}

/// Oracle 3 and 4: the same key counts rather than duplicating, and distinct
/// keys stay distinct.
#[test]
fn dedupe_counts_and_distinct_keys_stay_distinct() {
    let directory = tempdir().unwrap();
    let catalog = open(directory.path());
    let project = catalog.create_project("Antarctica").unwrap();

    for _ in 0..400 {
        catalog
            .record_diagnostic(
                &project.id,
                record(
                    "ingest.unreadable_media:/card",
                    "ingest.unreadable_media",
                    DiagnosticSeverity::Warning,
                ),
            )
            .unwrap();
    }
    let found = catalog
        .diagnostics(&project.id, &DiagnosticFilter::default())
        .unwrap();
    assert_eq!(found.len(), 1, "400 rejections must be one diagnostic");
    assert_eq!(found[0].occurrence_count, 400);
    assert!(found[0].first_seen_at <= found[0].last_seen_at);

    catalog
        .record_diagnostic(
            &project.id,
            record(
                "ingest.unreadable_media:/other",
                "ingest.unreadable_media",
                DiagnosticSeverity::Warning,
            ),
        )
        .unwrap();
    assert_eq!(
        catalog
            .diagnostics(&project.id, &DiagnosticFilter::default())
            .unwrap()
            .len(),
        2
    );
}

/// Oracle 5: acknowledgement hides, and a re-occurrence resurfaces the row with
/// its history intact.
#[test]
fn acknowledgement_hides_and_reoccurrence_resurfaces() {
    let directory = tempdir().unwrap();
    let catalog = open(directory.path());
    let project = catalog.create_project("Antarctica").unwrap();
    let created = catalog
        .record_diagnostic(
            &project.id,
            record(
                "sync.operation_quarantined:futureThing",
                "sync.operation_quarantined",
                DiagnosticSeverity::Warning,
            ),
        )
        .unwrap();

    catalog
        .acknowledge_diagnostic(&project.id, &created.id)
        .unwrap();
    assert!(
        catalog
            .diagnostics(&project.id, &DiagnosticFilter::default())
            .unwrap()
            .is_empty(),
        "acknowledged rows leave the default view"
    );
    // Never deleted: still there when explicitly asked for.
    assert_eq!(
        catalog
            .diagnostics(
                &project.id,
                &DiagnosticFilter {
                    include_acknowledged: true,
                    ..Default::default()
                }
            )
            .unwrap()
            .len(),
        1
    );

    let again = catalog
        .record_diagnostic(
            &project.id,
            record(
                "sync.operation_quarantined:futureThing",
                "sync.operation_quarantined",
                DiagnosticSeverity::Warning,
            ),
        )
        .unwrap();
    assert_eq!(again.acknowledged_at, None, "the condition happened again");
    assert_eq!(
        again.occurrence_count, 2,
        "history survives acknowledgement"
    );
    assert_eq!(
        catalog
            .diagnostics(&project.id, &DiagnosticFilter::default())
            .unwrap()
            .len(),
        1
    );
}

/// Filters narrow by severity floor and kind prefix, and the counts match.
#[test]
fn filters_and_counts_agree() {
    let directory = tempdir().unwrap();
    let catalog = open(directory.path());
    let project = catalog.create_project("Antarctica").unwrap();
    catalog
        .record_diagnostic(
            &project.id,
            record("a", "ingest.unreadable_media", DiagnosticSeverity::Warning),
        )
        .unwrap();
    catalog
        .record_diagnostic(
            &project.id,
            record("b", "media.volume_offline", DiagnosticSeverity::Error),
        )
        .unwrap();
    catalog
        .record_diagnostic(
            &project.id,
            record("c", "ingest.skipped_duplicate", DiagnosticSeverity::Info),
        )
        .unwrap();

    let errors_only = catalog
        .diagnostics(
            &project.id,
            &DiagnosticFilter {
                severity_at_least: Some(DiagnosticSeverity::Error),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(errors_only.len(), 1);

    let ingest_only = catalog
        .diagnostics(
            &project.id,
            &DiagnosticFilter {
                kind_prefix: Some("ingest.".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(ingest_only.len(), 2);

    let counts = catalog.diagnostic_counts(&project.id).unwrap();
    assert_eq!((counts.errors, counts.warnings, counts.infos), (1, 1, 1));
}

/// Oracle 8: diagnostics are local observations, not operations. Recording one
/// writes nothing to the operation log and moves no aggregate revision.
#[test]
fn diagnostics_are_not_operations() {
    let directory = tempdir().unwrap();
    let catalog = open(directory.path());
    let project = catalog.create_project("Antarctica").unwrap();
    let operations_before = catalog.list_operations(&project.id).unwrap().len();

    catalog
        .record_unreadable_media(
            &project.id,
            "/card/DCIM",
            "._C0001.MP4",
            "synthetic unreadable sidecar",
        )
        .unwrap();
    catalog
        .record_quarantined_operation(&project.id, "operation_1", "futureThing")
        .unwrap();
    catalog
        .record_offline_volume(&project.id, "/Volumes/Acasis", 420)
        .unwrap();

    assert_eq!(
        catalog.list_operations(&project.id).unwrap().len(),
        operations_before,
        "no diagnostic reaches the operation log"
    );
    assert_eq!(
        catalog
            .diagnostics(&project.id, &DiagnosticFilter::default())
            .unwrap()
            .len(),
        3
    );
}

/// Offline-volume detection groups by mount root and reports each root once.
#[test]
fn offline_volume_detection_groups_by_mount_root() {
    let directory = tempdir().unwrap();
    let catalog = open(directory.path());
    let project = catalog.create_project("Antarctica").unwrap();
    for index in 0..3 {
        catalog
            .add_portable_project_asset(
                &project.id,
                ProjectAssetAddedPayload {
                    id: format!("asset_{index}"),
                    fingerprint: format!("fingerprint_{index}"),
                    filename: format!("C{index:04}.MP4"),
                    size_bytes: 1024,
                    duration_seconds: 4.0,
                    frame_rate: 30.0,
                    width: 1920,
                    height: 1080,
                    codec: "h264".to_string(),
                    camera: "DJIPocket3".to_string(),
                    captured_at_raw: Some("2026-01-01T00:00:00Z".to_string()),
                    timestamp_candidates: None,
                    best_time: Some("2026-01-01T00:00:00Z".to_string()),
                    metadata_rotation_degrees: 0,
                    default_rotation_degrees: 0,
                    created_at: "2026-07-20T00:00:00.000Z".to_string(),
                },
            )
            .unwrap();
    }
    // Point every asset at a volume that is definitely not mounted. Written
    // directly because there is no user-facing way to relocate media, which is
    // exactly the condition this producer exists to notice.
    let connection = rusqlite::Connection::open(directory.path().join("catalog.sqlite3")).unwrap();
    for index in 0..3 {
        connection
            .execute(
                "UPDATE assets SET path=?2 WHERE id=?1",
                rusqlite::params![
                    format!("asset_{index}"),
                    format!("/Volumes/NoSuchCard/DCIM/C{index:04}.MP4")
                ],
            )
            .unwrap();
    }
    drop(connection);

    let recorded = catalog.detect_offline_volumes(&project.id).unwrap();
    assert_eq!(recorded.len(), 1, "one diagnostic per missing volume root");
    assert_eq!(recorded[0].kind, "media.volume_offline");
    assert_eq!(recorded[0].context["affectedAssets"], 3);
    assert_eq!(
        recorded[0].subject,
        DiagnosticSubject::Volume {
            volume_hint: "/Volumes/NoSuchCard".to_string()
        }
    );
}
