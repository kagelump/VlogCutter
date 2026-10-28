//! Acceptance oracle for the edit-plan model (D01-T019), validation
//! (D01-T020), and deterministic OTIO export (D01-T022).

use rcdvlog_core::{
    Catalog, EditPlanError, EditPlanOperation, OtioExportRequest, PreviewRequest,
    ProjectAssetAddedPayload, otio_document,
};
use std::fs;
use std::process::Command;
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
                    timestamp_candidates: None,
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
        audio: None,
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

/// Preview segments from mixed frame rates and audio layouts must share one
/// stream format before the concat demuxer joins them.
#[test]
fn preview_normalizes_mixed_video_and_audio_streams() {
    if Command::new("ffmpeg").arg("-version").output().is_err()
        || Command::new("ffprobe").arg("-version").output().is_err()
    {
        eprintln!("skipping preview media regression: ffmpeg/ffprobe unavailable");
        return;
    }

    let fixture = fixture(2);
    let first = fixture.directory.path().join("media/C0000.MP4");
    let second = fixture.directory.path().join("media/C0001.MP4");
    for (path, rate, with_audio) in [(&first, "60000/1001", true), (&second, "30000/1001", false)] {
        let mut command = Command::new("ffmpeg");
        command.args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=size=640x360:rate={rate}"),
        ]);
        if with_audio {
            command.args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100"]);
        }
        command.args([
            "-t",
            "2",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
        ]);
        if with_audio {
            command.args(["-c:a", "aac", "-ar", "44100"]);
        }
        let output = command.arg(path).output().unwrap();
        assert!(
            output.status.success(),
            "could not generate mixed-media fixture: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let revision = fixture
        .catalog
        .create_edit_plan_revision(
            &fixture.project_id,
            &[cut(0, 0.0, 1.7), cut(1, 0.0, 1.7)],
            None,
            None,
        )
        .unwrap();
    let result = fixture
        .catalog
        .render_preview(
            &PreviewRequest {
                revision_id: revision.revision_id,
                max_height: 720,
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap();

    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration:stream=codec_type,width,height,r_frame_rate,time_base,sample_rate,channels",
            "-of",
            "json",
        ])
        .arg(&result.output_path)
        .output()
        .unwrap();
    assert!(output.status.success());
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let duration = document["format"]["duration"]
        .as_str()
        .unwrap()
        .parse::<f64>()
        .unwrap();
    assert!(
        (duration - 3.4).abs() < 0.1,
        "unexpected preview duration: {duration}"
    );
    assert!((result.duration_seconds - duration).abs() < f64::EPSILON);

    let streams = document["streams"].as_array().unwrap();
    let video = streams
        .iter()
        .find(|stream| stream["codec_type"] == "video")
        .unwrap();
    assert_eq!(video["width"], 1280);
    assert_eq!(video["height"], 720);
    assert_eq!(video["r_frame_rate"], "30000/1001");
    assert_eq!(video["time_base"], "1/30000");
    let audio = streams
        .iter()
        .find(|stream| stream["codec_type"] == "audio")
        .unwrap();
    assert_eq!(audio["sample_rate"], "48000");
    assert_eq!(audio["channels"], 2);
    let tone_rms = decoded_audio_rms(&result.output_path, "0.3");
    let silent_rms = decoded_audio_rms(&result.output_path, "2.2");
    assert!(
        tone_rms > 0.01,
        "the source tone was replaced or lost: RMS={tone_rms}"
    );
    assert!(
        silent_rms < 0.003,
        "the silent clip gained audible audio: RMS={silent_rms}"
    );
}

fn decoded_audio_rms(path: &str, start_seconds: &str) -> f64 {
    let output = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            path,
            "-ss",
            start_seconds,
            "-t",
            "0.4",
            "-vn",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-f",
            "s16le",
            "pipe:1",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let samples: Vec<i16> = output
        .stdout
        .as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| i16::from_le_bytes(*bytes))
        .collect();
    assert!(!samples.is_empty());
    let mean_square = samples
        .iter()
        .map(|sample| f64::from(*sample).powi(2))
        .sum::<f64>()
        / samples.len() as f64;
    mean_square.sqrt() / f64::from(i16::MAX)
}

#[test]
fn preview_applies_the_effective_asset_rotation_to_raw_media() {
    if Command::new("ffmpeg").arg("-version").output().is_err()
        || Command::new("ffprobe").arg("-version").output().is_err()
    {
        eprintln!("skipping preview rotation regression: ffmpeg/ffprobe unavailable");
        return;
    }

    let fixture = fixture(1);
    let source = fixture.directory.path().join("media/C0000.MP4");
    let generated = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:size=640x360:rate=30:duration=1",
            "-vf",
            "drawbox=x=0:y=0:w=320:h=180:color=red:t=fill,drawbox=x=320:y=0:w=320:h=180:color=green:t=fill,drawbox=x=0:y=180:w=320:h=180:color=blue:t=fill,drawbox=x=320:y=180:w=320:h=180:color=yellow:t=fill",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&source)
        .output()
        .unwrap();
    assert!(generated.status.success());
    rusqlite::Connection::open(fixture.directory.path().join("catalog.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE assets SET rotation_degrees=180 WHERE id='asset_0'",
            [],
        )
        .unwrap();

    let revision = fixture
        .catalog
        .create_edit_plan_revision(&fixture.project_id, &[cut(0, 0.0, 0.9)], None, None)
        .unwrap();
    let result = fixture
        .catalog
        .render_preview(
            &PreviewRequest {
                revision_id: revision.revision_id,
                max_height: 720,
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap();
    let top_left = decoded_rgb_pixel(&result.output_path, 160, 90);
    let bottom_right = decoded_rgb_pixel(&result.output_path, 1120, 630);
    assert!(
        top_left[0] > 150 && top_left[1] > 150 && top_left[2] < 100,
        "180-degree rotation should move the source's yellow lower-right tile to preview upper-left; got {top_left:?}"
    );
    assert!(
        bottom_right[0] > 150 && bottom_right[1] < 100 && bottom_right[2] < 100,
        "180-degree rotation should move the source's red upper-left tile to preview lower-right; got {bottom_right:?}"
    );
}

fn decoded_rgb_pixel(path: &str, x: u32, y: u32) -> [u8; 3] {
    let filter = format!("format=rgb24,crop=16:16:{x}:{y},scale=1:1");
    let output = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            path,
            "-ss",
            "0.5",
            "-frames:v",
            "1",
            "-vf",
            &filter,
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "pipe:1",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout.len(), 3);
    [output.stdout[0], output.stdout[1], output.stdout[2]]
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

#[test]
fn cut_identity_survives_draft_forks_and_diff_tracks_order_and_trim() {
    let fixture = fixture(2);
    let a = fixture
        .catalog
        .create_edit_plan_revision(
            &fixture.project_id,
            &[cut(0, 0.0, 5.0), cut(0, 10.0, 15.0), cut(1, 0.0, 60.0)],
            None,
            None,
        )
        .unwrap();
    let mut cuts = a.operations.clone();
    cuts.swap(0, 2);
    cuts[0].source_out_seconds = 59.99;
    let b = fixture
        .catalog
        .create_edit_plan_from_base(&fixture.project_id, &cuts, None, Some(&a.revision_id), None)
        .unwrap();
    assert_eq!(
        b.operations.len(),
        3,
        "shared IDs must never drop cuts on insertion"
    );
    assert_eq!(b.operations[0].operation_id, a.operations[2].operation_id);
    assert_eq!(
        b.parent_revision_id.as_deref(),
        Some(a.revision_id.as_str())
    );
    let diff = fixture
        .catalog
        .diff_edit_plans(&a.revision_id, &b.revision_id)
        .unwrap();
    assert_eq!(diff.changes.len(), 2);
    assert!(
        diff.changes
            .iter()
            .any(|change| change.changes == ["order", "range"])
    );
    assert_eq!(
        fixture.catalog.edit_plan_revision(&a.revision_id).unwrap(),
        a
    );
    let restored = Catalog::import_project_baseline(
        fixture.directory.path().join("restored.sqlite3"),
        &fixture
            .catalog
            .export_project_baseline(&fixture.project_id)
            .unwrap(),
        "restored-node",
        1,
        "test",
    )
    .unwrap();
    assert_eq!(
        restored
            .edit_plan_revision(&b.revision_id)
            .unwrap()
            .operations,
        b.operations
    );
}

#[test]
fn initial_revision_guards_are_symmetric_and_invalid_activation_preserves_draft() {
    let fixture = fixture(1);
    let a = fixture
        .catalog
        .create_edit_plan_revision(&fixture.project_id, &[cut(0, 0.0, 5.0)], None, None)
        .unwrap();
    let error = fixture
        .catalog
        .activate_edit_plan_revision(&a.revision_id, Some(&a.revision_id))
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<EditPlanError>().unwrap().code(),
        "edit_plan_stale_revision"
    );
    let error = fixture
        .catalog
        .create_edit_plan_from_base(
            &fixture.project_id,
            &a.operations,
            None,
            Some(&a.revision_id),
            Some(&a.revision_id),
        )
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<EditPlanError>().unwrap().code(),
        "edit_plan_stale_revision"
    );
    let invalid = fixture
        .catalog
        .create_edit_plan_revision(&fixture.project_id, &[cut(0, 50.0, 90.0)], None, None)
        .unwrap();
    let error = fixture
        .catalog
        .activate_edit_plan_revision(&invalid.revision_id, None)
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<EditPlanError>().unwrap().code(),
        "edit_plan_validation_failed"
    );
    assert_eq!(
        fixture
            .catalog
            .edit_plan_revision(&invalid.revision_id)
            .unwrap(),
        invalid
    );
    assert!(
        fixture
            .catalog
            .active_edit_plan(&fixture.project_id)
            .unwrap()
            .is_none()
    );
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(
            fixture
                .catalog
                .create_edit_plan_revision(&fixture.project_id, &[cut(0, 0.0, value)], None, None)
                .is_err()
        );
    }
    let mut duplicate = a.operations.clone();
    duplicate.push(duplicate[0].clone());
    assert!(
        fixture
            .catalog
            .create_edit_plan_from_base(
                &fixture.project_id,
                &duplicate,
                None,
                Some(&a.revision_id),
                None
            )
            .is_err()
    );
}

#[test]
fn concurrent_initial_activations_have_one_winner() {
    let fixture = fixture(1);
    let a = fixture
        .catalog
        .create_edit_plan_revision(&fixture.project_id, &[cut(0, 0.0, 5.0)], None, None)
        .unwrap();
    let b = fixture
        .catalog
        .create_edit_plan_revision(&fixture.project_id, &[cut(0, 5.0, 10.0)], None, None)
        .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let threads: Vec<_> = [a.revision_id, b.revision_id]
        .into_iter()
        .map(|revision| {
            let barrier = barrier.clone();
            let catalog = Catalog::open_existing(fixture.catalog.database_path()).unwrap();
            std::thread::spawn(move || {
                barrier.wait();
                catalog
                    .activate_edit_plan_revision(&revision, None)
                    .map(|_| ())
                    .map_err(|error| {
                        error
                            .downcast_ref::<EditPlanError>()
                            .unwrap()
                            .code()
                            .to_string()
                    })
            })
        })
        .collect();
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(code) if code == "edit_plan_stale_revision"))
            .count(),
        1
    );
}
