//! Acceptance oracle for the edit-plan model (D01-T019), validation
//! (D01-T020), and deterministic OTIO export (D01-T022).

use rcdvlog_core::{
    Catalog, EditPlanError, EditPlanOperation, OtioExportRequest, ProjectAssetAddedPayload,
    otio_document,
};
use std::fs;
use tempfile::{TempDir, tempdir};

struct Fixture {
    catalog: Catalog,
    project_id: String,
    directory: TempDir,
}

/// A project whose media files really exist on disk, so validation's
/// reachability checks exercise the real filesystem seam.
fn fixture(count: usize) -> Fixture {
    let directory = tempdir().unwrap();
    let media = directory.path().join("media");
    fs::create_dir_all(&media).unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Antarctica").unwrap();
    for index in 0..count {
        let path = media.join(format!("C{index:04}.MP4"));
        fs::write(&path, b"not really video, but really present").unwrap();
        catalog
            .add_portable_project_asset(
                &project.id,
                ProjectAssetAddedPayload {
                    id: format!("asset_{index}"),
                    fingerprint: format!("fingerprint_{index}"),
                    filename: format!("C{index:04}.MP4"),
                    size_bytes: 1024,
                    duration_seconds: 60.0,
                    frame_rate: 30.0,
                    width: 1920,
                    height: 1080,
                    codec: "h264".to_string(),
                    camera: "SonyA7C".to_string(),
                    captured_at_raw: Some("2026-01-01T00:00:00Z".to_string()),
                    best_time: None,
                    metadata_rotation_degrees: 0,
                    default_rotation_degrees: 0,
                    created_at: "2026-07-20T00:00:00.000Z".to_string(),
                },
            )
            .unwrap();
        // `add_portable_project_asset` records no local path; point the asset
        // at the file that actually exists.
        rusqlite::Connection::open(directory.path().join("catalog.sqlite3"))
            .unwrap()
            .execute(
                "UPDATE assets SET path=?2 WHERE id=?1",
                rusqlite::params![format!("asset_{index}"), path.to_string_lossy()],
            )
            .unwrap();
    }
    Fixture {
        catalog,
        project_id: project.id,
        directory,
    }
}

fn cut(asset_index: usize, from: f64, to: f64) -> EditPlanOperation {
    EditPlanOperation {
        operation_id: String::new(),
        position: 0,
        asset_id: format!("asset_{asset_index}"),
        source_in_seconds: from,
        source_out_seconds: to,
        rationale: format!("shot {asset_index}"),
    }
}

/// Oracle 1: positions are assigned from the given order, and reading back
/// returns that order.
#[test]
fn a_revision_persists_its_operations_in_order() {
    let fixture = fixture(3);
    let revision = fixture
        .catalog
        .create_edit_plan_revision(
            &fixture.project_id,
            &[cut(2, 0.0, 5.0), cut(0, 1.0, 4.0), cut(1, 2.0, 9.0)],
            None,
            None,
        )
        .unwrap();
    let positions: Vec<u32> = revision
        .operations
        .iter()
        .map(|operation| operation.position)
        .collect();
    assert_eq!(positions, vec![0, 1, 2]);
    let assets: Vec<&str> = revision
        .operations
        .iter()
        .map(|operation| operation.asset_id.as_str())
        .collect();
    assert_eq!(assets, vec!["asset_2", "asset_0", "asset_1"]);
    assert_eq!(revision.duration_seconds(), 5.0 + 3.0 + 7.0);
}

/// Oracle 2 and 3: a plan derived from a superseded parent cannot silently
/// replace the newer one, and the loser is not lost.
#[test]
fn a_superseded_revision_cannot_replace_the_newer_one_and_is_not_lost() {
    let fixture = fixture(3);
    let a = fixture
        .catalog
        .create_edit_plan_revision(&fixture.project_id, &[cut(0, 0.0, 5.0)], None, None)
        .unwrap();
    fixture
        .catalog
        .activate_edit_plan_revision(&a.revision_id, None)
        .unwrap();

    // Two clients both derive from A.
    let b = fixture
        .catalog
        .create_edit_plan_revision(
            &fixture.project_id,
            &[cut(1, 0.0, 5.0)],
            None,
            Some(&a.revision_id),
        )
        .unwrap();
    let c = fixture
        .catalog
        .create_edit_plan_revision(
            &fixture.project_id,
            &[cut(2, 0.0, 5.0)],
            None,
            Some(&a.revision_id),
        )
        .unwrap();

    // C wins the race.
    fixture
        .catalog
        .activate_edit_plan_revision(&c.revision_id, Some(&a.revision_id))
        .unwrap();

    let error = fixture
        .catalog
        .activate_edit_plan_revision(&b.revision_id, Some(&a.revision_id))
        .unwrap_err();
    let typed = error.downcast_ref::<EditPlanError>().unwrap();
    assert_eq!(typed.code(), "edit_plan_stale_revision");
    assert!(
        format!("{typed}").contains(&c.revision_id),
        "the refusal must name the revision that superseded it: {typed}"
    );

    // B survives and can be re-derived from C.
    let recovered = fixture.catalog.edit_plan_revision(&b.revision_id).unwrap();
    assert_eq!(recovered.operations.len(), 1);
    fixture
        .catalog
        .activate_edit_plan_revision(&b.revision_id, Some(&c.revision_id))
        .unwrap();
    assert_eq!(
        fixture
            .catalog
            .active_edit_plan(&fixture.project_id)
            .unwrap()
            .unwrap()
            .revision_id,
        b.revision_id
    );

    // Passing None once a plan exists is refused, never treated as a force.
    let forced = fixture
        .catalog
        .create_edit_plan_revision(&fixture.project_id, &[], None, None)
        .unwrap_err();
    assert_eq!(
        forced.downcast_ref::<EditPlanError>().unwrap().code(),
        "edit_plan_stale_revision"
    );
}

/// Oracle 4 and 5: the two structural invariants are enforced by the database,
/// not by application discipline. Both are attacked by direct insert.
#[test]
fn the_database_makes_two_active_revisions_and_duplicate_positions_unrepresentable() {
    let fixture = fixture(2);
    let a = fixture
        .catalog
        .create_edit_plan_revision(&fixture.project_id, &[cut(0, 0.0, 5.0)], None, None)
        .unwrap();
    fixture
        .catalog
        .activate_edit_plan_revision(&a.revision_id, None)
        .unwrap();
    let b = fixture
        .catalog
        .create_edit_plan_revision(
            &fixture.project_id,
            &[cut(1, 0.0, 5.0)],
            None,
            Some(&a.revision_id),
        )
        .unwrap();

    let connection =
        rusqlite::Connection::open(fixture.directory.path().join("catalog.sqlite3")).unwrap();
    let second_active = connection.execute(
        "UPDATE edit_plan_revisions SET active=1 WHERE revision_id=?1",
        [&b.revision_id],
    );
    assert!(
        second_active.is_err(),
        "the partial unique index must reject a second active revision"
    );

    let duplicate_position = connection.execute(
        "INSERT INTO edit_plan_operations(operation_id, revision_id, position, asset_id, source_in_seconds, source_out_seconds, rationale)
         VALUES ('editop_duplicate', ?1, 0, 'asset_1', 0.0, 1.0, '')",
        [&a.revision_id],
    );
    assert!(
        duplicate_position.is_err(),
        "UNIQUE(revision_id, position) must reject two clips at one position"
    );
}

/// Oracle 7: an empty plan is a legitimate state.
#[test]
fn an_empty_plan_is_valid_and_reports_its_own_emptiness() {
    let fixture = fixture(1);
    let revision = fixture
        .catalog
        .create_edit_plan_revision(&fixture.project_id, &[], None, None)
        .unwrap();
    assert!(revision.is_empty());
    assert_eq!(revision.duration_seconds(), 0.0);
    assert!(
        fixture
            .catalog
            .validate_edit_plan(&revision.revision_id)
            .unwrap()
            .renderable()
    );
}

/// Oracle 11: export history is append-only.
#[test]
fn export_history_is_append_only() {
    let fixture = fixture(1);
    let revision = fixture
        .catalog
        .create_edit_plan_revision(&fixture.project_id, &[cut(0, 0.0, 5.0)], None, None)
        .unwrap();
    for index in 0..3 {
        fixture
            .catalog
            .record_edit_plan_export(&rcdvlog_core::EditPlanExport {
                export_id: format!("editexp_{index}"),
                revision_id: revision.revision_id.clone(),
                output_path: format!("/tmp/export_{index}.otio"),
                output_digest: format!("digest_{index}"),
                exported_at: format!("2026-08-0{}T00:00:00.000Z", index + 1),
            })
            .unwrap();
    }
    let exports = fixture
        .catalog
        .edit_plan_exports(&fixture.project_id)
        .unwrap();
    assert_eq!(exports.len(), 3, "no export is ever replaced");
}

/// D01-T020: errors block, warnings do not, and the codes are the stable ones
/// the desktop and CLI reference.
#[test]
fn validation_separates_blocking_errors_from_informational_warnings() {
    let fixture = fixture(2);
    let revision = fixture
        .catalog
        .create_edit_plan_revision(
            &fixture.project_id,
            &[
                // Legal.
                cut(0, 0.0, 5.0),
                // Overlaps the first: legal but usually accidental.
                cut(0, 3.0, 8.0),
                // Past the end of a 60s clip.
                cut(1, 50.0, 90.0),
                // Ends before it starts.
                cut(1, 10.0, 5.0),
                // No duration.
                cut(1, 20.0, 20.0),
            ],
            None,
            None,
        )
        .unwrap();
    let report = fixture
        .catalog
        .validate_edit_plan(&revision.revision_id)
        .unwrap();
    assert!(!report.renderable(), "errors must block");

    let codes: Vec<&str> = report.errors().map(|finding| finding.code).collect();
    assert!(
        codes.contains(&"edit_plan_range_out_of_bounds"),
        "{codes:?}"
    );
    assert!(codes.contains(&"edit_plan_range_inverted"), "{codes:?}");
    assert!(codes.contains(&"edit_plan_range_zero_length"), "{codes:?}");

    let warnings: Vec<&str> = report.warnings().map(|finding| finding.code).collect();
    assert!(
        warnings.contains(&"edit_plan_source_range_overlap"),
        "{warnings:?}"
    );
    // A missing proxy degrades the preview but never blocks.
    assert!(
        warnings.contains(&"edit_plan_proxy_missing"),
        "{warnings:?}"
    );

    // Findings read down the timeline.
    let positions: Vec<Option<u32>> = report
        .findings
        .iter()
        .map(|finding| finding.position)
        .collect();
    let mut sorted = positions.clone();
    sorted.sort();
    assert_eq!(positions, sorted);
}

/// D01-T020: an offline original blocks, because the export conforms against
/// camera masters.
#[test]
fn an_offline_original_blocks_the_plan() {
    let fixture = fixture(1);
    let revision = fixture
        .catalog
        .create_edit_plan_revision(&fixture.project_id, &[cut(0, 0.0, 5.0)], None, None)
        .unwrap();
    assert!(
        fixture
            .catalog
            .validate_edit_plan(&revision.revision_id)
            .unwrap()
            .renderable()
    );

    fs::remove_file(fixture.directory.path().join("media/C0000.MP4")).unwrap();
    let report = fixture
        .catalog
        .validate_edit_plan(&revision.revision_id)
        .unwrap();
    assert!(!report.renderable());
    assert!(
        report
            .errors()
            .any(|finding| finding.code == "edit_plan_original_offline")
    );
}

/// D01-T022: the document is byte-stable across repeated serialization, and it
/// contains nothing environmental.
#[test]
fn the_otio_document_is_byte_stable_and_environment_free() {
    let fixture = fixture(3);
    let revision = fixture
        .catalog
        .create_edit_plan_revision(
            &fixture.project_id,
            &[cut(0, 1.0, 4.0), cut(2, 0.0, 10.0), cut(1, 5.0, 6.5)],
            None,
            None,
        )
        .unwrap();
    let assets = fixture.catalog.list_assets(&fixture.project_id).unwrap();

    let first = otio_document(&revision, &assets, "Antarctica").unwrap();
    for _ in 0..16 {
        assert_eq!(
            otio_document(&revision, &assets, "Antarctica").unwrap(),
            first,
            "serialization must be byte-stable"
        );
    }

    // Nothing environmental may reach the file.
    let user = std::env::var("USER").unwrap_or_default();
    if !user.is_empty() {
        assert!(
            !first.contains(&user),
            "the username leaked into the export"
        );
    }
    assert!(
        !first.contains("2026-08"),
        "a timestamp leaked into the export"
    );
    // One video track, hard cuts only: the writer must not invent structure the
    // model cannot express.
    for forbidden in ["transition", "effect", "Gap", "Filler", "fade", "opacity"] {
        assert!(
            !first.contains(forbidden),
            "the writer invented a '{forbidden}' the model cannot express"
        );
    }

    // Times are rational — a value and a rate — never a bare float duration.
    let parsed: serde_json::Value = serde_json::from_str(&first).unwrap();
    let clips = &parsed["tracks"]["children"][0]["children"];
    assert_eq!(clips.as_array().unwrap().len(), 3);
    assert_eq!(clips[0]["source_range"]["start_time"]["rate"], 30.0);
    assert_eq!(clips[0]["source_range"]["start_time"]["value"], 30.0);
    assert_eq!(clips[0]["source_range"]["duration"]["value"], 90.0);
    // The original, never a proxy.
    assert!(
        clips[0]["media_reference"]["target_url"]
            .as_str()
            .unwrap()
            .ends_with("C0000.MP4")
    );
}

/// D01-T022: identifiers are derived from stable input, so re-exporting the
/// same revision produces the same clips rather than new ones.
#[test]
fn re_exporting_a_revision_produces_identical_clip_identifiers() {
    let fixture = fixture(2);
    let revision = fixture
        .catalog
        .create_edit_plan_revision(
            &fixture.project_id,
            &[cut(0, 0.0, 2.0), cut(1, 0.0, 2.0)],
            None,
            None,
        )
        .unwrap();
    let assets = fixture.catalog.list_assets(&fixture.project_id).unwrap();
    let document = otio_document(&revision, &assets, "Antarctica").unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&document).unwrap();
    let identifier =
        parsed["tracks"]["children"][0]["children"][0]["metadata"]["rcdvlog"]["clipId"]
            .as_str()
            .unwrap()
            .to_string();
    assert_eq!(identifier, format!("{}:00000", revision.revision_id));

    let again = otio_document(&revision, &assets, "Antarctica").unwrap();
    assert_eq!(document, again);
}

/// D01-T022: an existing output is never silently replaced.
#[test]
fn an_existing_export_is_never_silently_replaced() {
    let fixture = fixture(1);
    let revision = fixture
        .catalog
        .create_edit_plan_revision(&fixture.project_id, &[cut(0, 0.0, 5.0)], None, None)
        .unwrap();
    let output = fixture.directory.path().join("out/timeline.otio");
    let request = OtioExportRequest {
        revision_id: revision.revision_id.clone(),
        output_path: output.to_string_lossy().into_owned(),
        expected_existing_digest: None,
        timeline_name: "Antarctica".to_string(),
    };

    let first = fixture.catalog.export_otio(&request).unwrap();
    assert_eq!(first.clip_count, 1);
    assert!(output.exists());

    // A second export with `None` must refuse, naming the digest it found.
    let refused = fixture.catalog.export_otio(&request).unwrap_err();
    let typed = refused
        .downcast_ref::<rcdvlog_core::OtioExportError>()
        .unwrap();
    assert_eq!(typed.code(), "otio_output_exists");
    assert!(format!("{typed}").contains(&first.output_digest));

    // Supplying the digest actually on disk is the way to overwrite.
    fixture
        .catalog
        .export_otio(&OtioExportRequest {
            expected_existing_digest: Some(first.output_digest.clone()),
            ..request
        })
        .unwrap();

    // Both exports are recorded; history is append-only.
    assert_eq!(
        fixture
            .catalog
            .edit_plan_exports(&fixture.project_id)
            .unwrap()
            .len(),
        2
    );
}

/// D01-T019 oracle 12: the model cannot express a track, transition, or effect.
#[test]
fn the_edit_plan_schema_contains_no_track_transition_or_effect_column() {
    let fixture = fixture(1);
    let connection =
        rusqlite::Connection::open(fixture.directory.path().join("catalog.sqlite3")).unwrap();
    let mut statement = connection
        .prepare("SELECT sql FROM sqlite_master WHERE name LIKE 'edit_plan%'")
        .unwrap();
    let schema: String = statement
        .query_map([], |row| row.get::<_, Option<String>>(0))
        .unwrap()
        .filter_map(Result::ok)
        .flatten()
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    assert!(!schema.is_empty(), "the edit plan tables must exist");
    for forbidden in [
        "track",
        "transition",
        "effect",
        "filter",
        "opacity",
        "audio_level",
        "fade",
    ] {
        assert!(
            !schema.contains(forbidden),
            "the schema reserves '{forbidden}', which the scope decision forbids"
        );
    }
}
