//! Crate-level `Catalog` tests (moved out of lib.rs).

use super::*;
use crate::analysis::store::{StoredTranscript, save_transcript};
use crate::analysis::vlm_store::record_ai_labels;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::fs;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tempfile::{TempDir, tempdir};

#[test]
fn analysis_budget_requires_finite_positive_value() {
    for value in [0.0, -0.01, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let options = AnalysisRunOptions {
            budget_usd: Some(value),
            ..Default::default()
        };
        assert!(
            resolve_analysis_budget(&options).is_err(),
            "accepted {value:?}"
        );
    }
    let options = AnalysisRunOptions {
        budget_usd: Some(0.01),
        ..Default::default()
    };
    assert_eq!(resolve_analysis_budget(&options).unwrap(), 0.01);
}

#[test]
fn invalid_budget_is_rejected_before_key_resolution_or_network_setup() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("budget.sqlite3")).unwrap();
    let error = catalog
        .analyze_missing_with_options(
            "missing-project",
            &AnalysisRunOptions {
                budget_usd: Some(f64::NAN),
                ..Default::default()
            },
            |_| ControlFlow::Continue(()),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("finite"));
}

/// Inserts an asset whose stored fingerprint is the real sampled fingerprint
/// of the file on disk, so location verification has something honest to
/// compare against. Deliberately avoids `ingest_folder`, which would probe
/// the file and require ffprobe.
pub(crate) fn insert_asset_at(catalog: &Catalog, project_id: &str, id: &str, path: &Path) {
    let size = fs::metadata(path).unwrap().len();
    let fingerprint = sampled_fingerprint(path, size).unwrap();
    catalog
        .connection()
        .unwrap()
        .execute(
            r#"
                INSERT INTO assets (
                    id, project_id, fingerprint, path, filename, size_bytes,
                    duration_seconds, frame_rate, width, height, codec, camera,
                    captured_at_raw, best_time, source_out, created_at,
                    orientation_probed
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 10, 30, 1920, 1080,
                          'h264', 'Sony (C-series)', NULL, NULL, 10, ?7, 1)
                "#,
            params![
                id,
                project_id,
                fingerprint,
                path.to_string_lossy(),
                path.file_name().unwrap().to_string_lossy(),
                size as i64,
                now(),
            ],
        )
        .unwrap();
}

fn location_state(catalog: &Catalog, asset_id: &str, path: &Path) -> Option<String> {
    catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT verification_state FROM project_asset_local_locations
                 WHERE asset_id = ?1 AND local_path = ?2",
            params![asset_id, path.to_string_lossy()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .unwrap()
}

/// The defect this epic exists to prevent: a card is reformatted and reused,
/// a new clip lands at the pathname an old asset remembers, and the old
/// asset quietly starts resolving to footage that is not its own.
#[test]
fn a_reused_pathname_never_binds_an_asset_to_different_content() {
    let (directory, catalog, project) = catalog_fixture();
    let card = directory.path().join("CARD/DCIM/100MSDCF");
    fs::create_dir_all(&card).unwrap();
    let clip = card.join("C0012.MP4");
    fs::write(&clip, b"the original clip, shot in Spain").unwrap();
    insert_asset_at(&catalog, &project.id, "asset_spain", &clip);

    // Verified once, this resolves without complaint.
    assert!(catalog.resolve_original("asset_spain").is_ok());
    assert!(catalog.get_asset("asset_spain").unwrap().original_available);

    // The card is reformatted and the camera writes a different clip to the
    // same DCIM pathname.
    fs::write(&clip, b"a completely different clip, shot in Iceland later").unwrap();

    assert!(
        !catalog.get_asset("asset_spain").unwrap().original_available,
        "an asset must not report an original it can no longer vouch for"
    );
    let error = catalog.resolve_original("asset_spain").unwrap_err();
    assert!(
        format!("{error:#}").contains("not this clip any more"),
        "unexpected error: {error:#}"
    );
    assert_eq!(
        location_state(&catalog, "asset_spain", &clip).as_deref(),
        Some("mismatch")
    );
}

/// A mismatch is a conclusion, not a transient failure. Restoring the right
/// bytes must not silently re-arm the location; only an explicit rescan does.
#[test]
fn a_mismatched_location_is_repaired_only_by_an_explicit_rescan() {
    let (directory, catalog, project) = catalog_fixture();
    let card = directory.path().join("CARD");
    fs::create_dir_all(&card).unwrap();
    let clip = card.join("C0012.MP4");
    fs::write(&clip, b"the original clip").unwrap();
    insert_asset_at(&catalog, &project.id, "asset_one", &clip);
    catalog.resolve_original("asset_one").unwrap();

    fs::write(&clip, b"different content entirely").unwrap();
    assert!(catalog.resolve_original("asset_one").is_err());

    // The right file comes back, but nothing has re-checked it yet.
    fs::write(&clip, b"the original clip").unwrap();
    assert!(
        !catalog.get_asset("asset_one").unwrap().original_available,
        "a location proven wrong must stay untrusted until it is rechecked"
    );

    // An ingest over the folder is that recheck.
    let report = catalog.ingest_folder(&project.id, &card).unwrap();
    assert_eq!(report.imported, 0);
    assert_eq!(
        location_state(&catalog, "asset_one", &clip).as_deref(),
        Some("verified")
    );
    assert!(catalog.get_asset("asset_one").unwrap().original_available);
}

/// Content already known to the project, discovered where the asset is not
/// currently looking, is the missing original — not a duplicate to discard.
#[test]
fn a_known_clip_found_at_a_new_path_repairs_the_asset() {
    let (directory, catalog, project) = catalog_fixture();
    let first_mount = directory.path().join("Volumes/CARD");
    let second_mount = directory.path().join("Volumes/CARD 1");
    fs::create_dir_all(&first_mount).unwrap();
    fs::create_dir_all(&second_mount).unwrap();
    let original = first_mount.join("C0001.MP4");
    fs::write(&original, b"clip bytes that do not change").unwrap();
    insert_asset_at(&catalog, &project.id, "asset_one", &original);

    // The volume remounts somewhere else, exactly as macOS does when a card
    // is inserted while the previous mount lingers.
    let moved = second_mount.join("C0001.MP4");
    fs::rename(&original, &moved).unwrap();
    assert!(!catalog.get_asset("asset_one").unwrap().original_available);

    let report = catalog.ingest_folder(&project.id, &second_mount).unwrap();
    assert_eq!(report.scanned, 1);
    assert_eq!(
        report.imported, 0,
        "repairing must not create a second asset"
    );
    assert_eq!(report.relinked, 1);
    assert_eq!(report.duplicates, 0);

    let repaired = catalog.get_asset("asset_one").unwrap();
    assert_eq!(repaired.path, moved.to_string_lossy());
    assert!(repaired.original_available);
    assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 1);
}

/// A second copy is worth remembering, but it is no reason to move an asset
/// off a location that already works.
#[test]
fn a_second_copy_is_recorded_without_disturbing_a_working_asset() {
    let (directory, catalog, project) = catalog_fixture();
    let nas = directory.path().join("nas");
    let card = directory.path().join("card");
    fs::create_dir_all(&nas).unwrap();
    fs::create_dir_all(&card).unwrap();
    let original = nas.join("C0001.MP4");
    fs::write(&original, b"clip bytes").unwrap();
    insert_asset_at(&catalog, &project.id, "asset_one", &original);
    catalog.resolve_original("asset_one").unwrap();

    let copy = card.join("C0001.MP4");
    fs::copy(&original, &copy).unwrap();
    let report = catalog.ingest_folder(&project.id, &card).unwrap();
    assert_eq!(report.duplicates, 1);
    assert_eq!(report.relinked, 0);

    let asset = catalog.get_asset("asset_one").unwrap();
    assert_eq!(
        asset.path,
        original.to_string_lossy(),
        "a working location must not be swapped out from under the asset"
    );
    // Both locations are nonetheless known, which is what lets a later
    // archive step reason about where this content already exists.
    assert_eq!(
        location_state(&catalog, "asset_one", &copy).as_deref(),
        Some("verified")
    );
}

/// The whole point of the repair flow: the user moved their footage, points
/// at where it went, and the project reconnects without gaining anything it
/// did not already have.
#[test]
fn locating_moved_originals_repairs_them_without_importing_anything_new() {
    let (directory, catalog, project) = catalog_fixture();
    let old_home = directory.path().join("local/footage");
    let new_home = directory.path().join("Volumes/Shuttle/footage");
    fs::create_dir_all(&old_home).unwrap();
    fs::create_dir_all(&new_home).unwrap();

    for (id, name, bytes) in [
        ("asset_one", "C0001.MP4", b"first clip ".as_slice()),
        ("asset_two", "C0002.MP4", b"second clip".as_slice()),
    ] {
        let path = old_home.join(name);
        fs::write(&path, bytes).unwrap();
        insert_asset_at(&catalog, &project.id, id, &path);
    }

    // The user drags the folder to an external drive behind the app's back,
    // and something unrelated to this project is sitting there too.
    for name in ["C0001.MP4", "C0002.MP4"] {
        fs::rename(old_home.join(name), new_home.join(name)).unwrap();
    }
    fs::write(new_home.join("SOMEONE_ELSES.MP4"), b"not this project").unwrap();

    catalog.detect_offline_volumes(&project.id).unwrap();
    assert_eq!(catalog.offline_asset_count(&project.id).unwrap(), 2);

    let report = catalog
        .relink_folder(&project.id, &new_home, |_, _, _| {})
        .unwrap();

    assert_eq!(report.scanned, 3);
    assert_eq!(report.repaired, 2);
    assert_eq!(
        report.unrecognized, 1,
        "media belonging to no asset here must be counted, never imported"
    );
    assert_eq!(report.still_missing, 0);
    assert_eq!(
        catalog.list_assets(&project.id).unwrap().len(),
        2,
        "locating must not enlarge the project"
    );
    for asset in catalog.list_assets(&project.id).unwrap() {
        assert!(asset.original_available);
        assert!(
            asset
                .path
                .starts_with(&new_home.to_string_lossy().to_string())
        );
    }

    // The offline-volume diagnostic that sent the user here is resolved.
    let unacknowledged = catalog
        .diagnostics(&project.id, &DiagnosticFilter::default())
        .unwrap();
    assert!(
        !unacknowledged
            .iter()
            .any(|diagnostic| diagnostic.kind == "media.volume_offline"),
        "a volume that came back must stop being reported as offline"
    );
}

/// Pointing at the wrong folder has to be a no-op the user can read, not an
/// import they have to undo.
#[test]
fn locating_against_an_unrelated_folder_changes_nothing() {
    let (directory, catalog, project) = catalog_fixture();
    let home = directory.path().join("footage");
    let elsewhere = directory.path().join("holiday-snaps");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&elsewhere).unwrap();
    let clip = home.join("C0001.MP4");
    fs::write(&clip, b"the missing clip").unwrap();
    insert_asset_at(&catalog, &project.id, "asset_one", &clip);
    fs::remove_file(&clip).unwrap();
    fs::write(elsewhere.join("VACATION.MP4"), b"unrelated footage").unwrap();

    let report = catalog
        .relink_folder(&project.id, &elsewhere, |_, _, _| {})
        .unwrap();

    assert_eq!(report.repaired, 0);
    assert_eq!(report.unrecognized, 1);
    assert_eq!(report.still_missing, 1);
    assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 1);
    assert_eq!(
        catalog.get_asset("asset_one").unwrap().path,
        clip.to_string_lossy(),
        "a failed search must leave the asset pointing where it was"
    );
}

/// Verification is cached: a second resolve of an unchanged file must not
/// re-hash it, or every proxy batch would re-read every original.
#[test]
fn an_unchanged_verified_location_is_not_rehashed() {
    let (directory, catalog, project) = catalog_fixture();
    let clip = directory.path().join("C0001.MP4");
    fs::write(&clip, b"clip bytes").unwrap();
    insert_asset_at(&catalog, &project.id, "asset_one", &clip);

    catalog.resolve_original("asset_one").unwrap();
    let first_verified_at: String = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT verified_at FROM project_asset_local_locations WHERE asset_id = 'asset_one'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    catalog.resolve_original("asset_one").unwrap();
    let second_verified_at: String = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT verified_at FROM project_asset_local_locations WHERE asset_id = 'asset_one'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        first_verified_at, second_verified_at,
        "an unchanged location must be trusted from its cached verification"
    );
}

/// An asset carried forward from an older catalog has never been verified.
/// It must still be usable, or upgrading would report a whole project
/// offline.
#[test]
fn an_unverified_legacy_location_stays_usable() {
    let (directory, catalog, project) = catalog_fixture();
    let clip = directory.path().join("C0001.MP4");
    fs::write(&clip, b"clip bytes").unwrap();
    insert_asset_at(&catalog, &project.id, "asset_one", &clip);
    assert!(
        location_state(&catalog, "asset_one", &clip).is_none(),
        "this fixture inserts no location row, standing in for a legacy catalog"
    );
    assert!(catalog.get_asset("asset_one").unwrap().original_available);
}

fn catalog_fixture() -> (TempDir, Catalog, Project) {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Test project").unwrap();
    (directory, catalog, project)
}

/// #76: a volume that stops answering leaves a read parked forever rather
/// than failing. The ingest has to give up on its own, because the
/// import's disconnect handling only runs on an `Err` — a call that never
/// returns bypasses every recovery path the import has.
#[cfg(unix)]
#[test]
fn an_ingest_whose_read_never_returns_gives_up_instead_of_hanging() {
    let (directory, catalog, project) = catalog_fixture();
    let unresponsive = directory.path().join("unresponsive.MP4");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&unresponsive)
            .status()
            .unwrap()
            .success(),
        "could not create the FIFO this test needs"
    );

    // Run the ingest on its own thread so a regression fails this test in
    // seconds instead of hanging the suite the way it hung the app.
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome =
            crate::stall::with_io_stall_timeout(std::time::Duration::from_millis(300), || {
                catalog.ingest_file_with_camera(&project.id, &unresponsive, None)
            });
        let _ = sender.send(outcome.err().map(|error| format!("{error:#}")));
    });

    match receiver.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(Some(error)) => assert!(
            error.contains("stopped answering"),
            "the failure must name the stall, got: {error}"
        ),
        Ok(None) => panic!("ingesting an unresponsive path unexpectedly succeeded"),
        Err(_) => panic!("the ingest never returned — this is the hang #76 is about"),
    }
}

#[test]
fn chronology_resolution_reserves_the_writer_before_loading_facts() {
    let (_directory, catalog, project) = catalog_fixture();
    catalog
        .add_portable_project_asset(
            &project.id,
            ProjectAssetAddedPayload {
                id: "asset_chronology_lock".to_string(),
                fingerprint: "fingerprint_chronology_lock".to_string(),
                filename: "C0001.MP4".to_string(),
                size_bytes: 100,
                duration_seconds: 10.0,
                frame_rate: 30.0,
                width: 1920,
                height: 1080,
                codec: "h264".to_string(),
                camera: "Sony".to_string(),
                captured_at_raw: Some("2026-01-01T09:00:00Z".to_string()),
                timestamp_candidates: None,
                best_time: Some("2026-01-01T09:00:00Z".to_string()),
                metadata_rotation_degrees: 0,
                default_rotation_degrees: 0,
                created_at: "2026-01-01T00:00:00Z".to_string(),
            },
        )
        .unwrap();
    let catalog_path = catalog.database_path().to_path_buf();
    let mut observed_reservation = false;

    let solution = catalog
        .resolve_chronology_with_hook(&project.id, || {
            let competing = Connection::open(&catalog_path).unwrap();
            competing.busy_timeout(std::time::Duration::ZERO).unwrap();
            let error = competing
                .execute_batch("BEGIN IMMEDIATE")
                .expect_err("a competing writer must wait until projection commits");
            assert!(
                error.to_string().contains("locked"),
                "unexpected competing-writer error: {error}"
            );
            observed_reservation = true;
        })
        .unwrap();

    assert!(observed_reservation);
    assert_eq!(solution.assets.len(), 1);
    assert_eq!(
        catalog
            .get_asset("asset_chronology_lock")
            .unwrap()
            .best_time
            .as_deref(),
        solution.assets[0].trip_time.as_deref()
    );
}

/// Writes a byte-identical stand-in for a generated proxy and points the
/// asset at it. Enough for fingerprinting and staleness tests, which never
/// decode the file.
fn attach_stub_proxy(catalog: &Catalog, asset_id: &str, directory: &Path, bytes: &[u8]) -> PathBuf {
    let proxy_path = directory.join(format!("{asset_id}.proxy.mp4"));
    fs::write(&proxy_path, bytes).unwrap();
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET proxy_path = ?2 WHERE id = ?1",
            params![asset_id, proxy_path.to_string_lossy()],
        )
        .unwrap();
    proxy_path
}

fn mark_asset_video_usable(catalog: &Catalog, asset_id: &str) {
    let asset = catalog.get_asset(asset_id).unwrap();
    catalog
        .store_video_validation(asset_id, &asset.fingerprint, "usable", None)
        .unwrap();
}

#[test]
fn proxy_fingerprint_changes_when_the_proxy_is_regenerated() {
    let (directory, catalog, project) = catalog_fixture();
    insert_asset(
        &catalog,
        &project.id,
        "a1",
        "DJI_0001.MP4",
        "DJI Pocket 3",
        "2026-04-01T09:00:00Z",
        Path::new("/volumes/nas/DJI_0001.MP4"),
    );
    attach_stub_proxy(&catalog, "a1", directory.path(), b"proxy version three");
    let first = catalog.proxy_fingerprint("a1").unwrap();
    attach_stub_proxy(&catalog, "a1", directory.path(), b"proxy regenerated anew");
    let second = catalog.proxy_fingerprint("a1").unwrap();
    assert_ne!(
        first, second,
        "a regenerated proxy must invalidate its transcript"
    );
}

#[test]
fn proxy_fingerprint_fails_clearly_without_a_proxy() {
    let (_directory, catalog, project) = catalog_fixture();
    insert_asset(
        &catalog,
        &project.id,
        "a1",
        "DJI_0001.MP4",
        "DJI Pocket 3",
        "2026-04-01T09:00:00Z",
        Path::new("/volumes/nas/DJI_0001.MP4"),
    );
    let error = catalog.proxy_fingerprint("a1").unwrap_err();
    assert!(error.to_string().contains("proxy is required"), "{error}");
}

#[test]
fn transcribe_missing_skips_clips_with_a_current_transcript() {
    let (directory, catalog, project) = catalog_fixture();
    insert_asset(
        &catalog,
        &project.id,
        "a1",
        "DJI_0001.MP4",
        "DJI Pocket 3",
        "2026-04-01T09:00:00Z",
        Path::new("/volumes/nas/DJI_0001.MP4"),
    );
    attach_stub_proxy(&catalog, "a1", directory.path(), b"proxy bytes");
    let fingerprint = catalog.proxy_fingerprint("a1").unwrap();
    save_transcript(
        &catalog.connection().unwrap(),
        &StoredTranscript {
            asset_id: "a1".to_string(),
            recipe_key: STT_RECIPE_KEY.to_string(),
            source_fingerprint: fingerprint,
            status: TranscriptStatus::NoSpeech,
            outcome_reason: crate::analysis::store::TranscriptOutcomeReason::NoDetectedSpeech,
            reject_reason: String::new(),
            primary_language: String::new(),
            speech_seconds: 0.0,
            window_count: 0,
            provider_segment_count: 0,
            mean_token_probability: None,
            repetition_ratio: None,
            segments: Vec::new(),
        },
    )
    .unwrap();

    // No models are installed, so reaching the engine would fail. Returning
    // a clean report proves the clip was skipped before that point.
    let report = catalog.transcribe_missing(&project.id).unwrap();
    assert_eq!(report.skipped, 1);
    assert_eq!(report.transcribed, 0);
    assert!(report.outcomes.is_empty());
}

#[test]
fn transcribe_missing_reports_a_corrupt_proxy_as_a_processing_failure() {
    let (directory, catalog, project) = catalog_fixture();
    insert_asset(
        &catalog,
        &project.id,
        "a1",
        "DJI_0001.MP4",
        "DJI Pocket 3",
        "2026-04-01T09:00:00Z",
        Path::new("/volumes/nas/DJI_0001.MP4"),
    );
    attach_stub_proxy(&catalog, "a1", directory.path(), b"proxy bytes");
    mark_asset_video_usable(&catalog, "a1");
    let report = catalog.transcribe_missing(&project.id).unwrap();
    assert_eq!(report.failed, 1);
    assert_eq!(report.transcribed, 0);
    assert_eq!(report.failures.len(), 1);
    assert!(
        report.failures[0].error.contains("probe audio"),
        "expected a probe failure, got: {}",
        report.failures[0].error
    );
    let stored = catalog.asset_transcript("a1").unwrap().unwrap();
    assert_eq!(stored.status, TranscriptStatus::Failed);
    assert_eq!(
        stored.outcome_reason,
        crate::analysis::store::TranscriptOutcomeReason::ProcessingFailed
    );
    assert!(!stored.source_fingerprint.is_empty());
    let coverage = catalog.transcript_coverage(&project.id).unwrap();
    assert_eq!(coverage.failed, 1);
    assert_eq!(coverage.missing, 0);
}

fn save_review_candidate(catalog: &Catalog, asset_id: &str, source_fingerprint: &str, text: &str) {
    save_transcript(
        &catalog.connection().unwrap(),
        &StoredTranscript {
            asset_id: asset_id.to_string(),
            recipe_key: STT_RECIPE_KEY.to_string(),
            source_fingerprint: source_fingerprint.to_string(),
            status: TranscriptStatus::ReviewRequired,
            outcome_reason:
                crate::analysis::store::TranscriptOutcomeReason::QualityEvidenceIncomplete,
            reject_reason: "review_missing_quality_evidence".to_string(),
            primary_language: "en".to_string(),
            speech_seconds: 2.0,
            window_count: 1,
            provider_segment_count: 1,
            mean_token_probability: None,
            repetition_ratio: None,
            segments: vec![DecodedSegment {
                start_ms: 250,
                end_ms: 2_250,
                language: "en".to_string(),
                text: text.to_string(),
                mean_token_probability: None,
            }],
        },
    )
    .unwrap();
}

#[test]
fn transcript_coverage_counts_review_required_separately_from_speech() {
    let (directory, catalog, project) = catalog_fixture();
    insert_asset(
        &catalog,
        &project.id,
        "review-only",
        "review-only.MP4",
        "DJI Pocket 3",
        "2026-04-01T09:00:00Z",
        Path::new("/volumes/nas/review-only.MP4"),
    );
    attach_stub_proxy(
        &catalog,
        "review-only",
        directory.path(),
        b"review candidate proxy",
    );
    let fingerprint = catalog.proxy_fingerprint("review-only").unwrap();
    save_review_candidate(&catalog, "review-only", &fingerprint, "retained candidate");

    let coverage = catalog.transcript_coverage(&project.id).unwrap();
    assert_eq!(coverage.total, 1);
    assert_eq!(coverage.review_required, 1);
    assert_eq!(
        coverage.speech, 0,
        "review candidates are not accepted speech"
    );
    assert_eq!(coverage.rejected, 0);
    assert_eq!(coverage.no_speech, 0);
    assert_eq!(coverage.no_audio, 0);
    assert_eq!(coverage.no_accepted_speech, 0);
    assert_eq!(coverage.failed, 0);
    assert_eq!(coverage.missing, 0);
}

#[test]
fn review_candidates_stay_human_readable_but_never_become_prompt_context() {
    let (directory, catalog, project) = catalog_fixture();
    insert_asset(
        &catalog,
        &project.id,
        "a1",
        "a1.MP4",
        "DJI Pocket 3",
        "2026-04-01T09:00:00Z",
        Path::new("/volumes/nas/a1.MP4"),
    );
    attach_stub_proxy(&catalog, "a1", directory.path(), b"prompt review proxy");
    let fingerprint = catalog.proxy_fingerprint("a1").unwrap();
    save_review_candidate(&catalog, "a1", &fingerprint, "REVIEW_CANDIDATE_CANARY");

    let stored = catalog.asset_transcript("a1").unwrap().unwrap();
    assert_eq!(stored.status, TranscriptStatus::ReviewRequired);
    assert_eq!(stored.segments[0].text, "REVIEW_CANDIDATE_CANARY");
    assert!(
        catalog.prompt_transcript("a1").unwrap().is_none(),
        "review candidates must be absent from AI prompt context"
    );
    let index = catalog.project_analysis_index(&project.id).unwrap();
    assert_eq!(index.len(), 1);
    assert_eq!(
        index[0].transcript_status.as_deref(),
        Some("review_required")
    );
    assert_eq!(index[0].transcript_text, "REVIEW_CANDIDATE_CANARY");

    // A historical local Speech row from a superseded proxy must not leak
    // either. The unified currentness owner applies the unchanged local
    // source/recipe rule before prompt context is gathered.
    save_transcript(
        &catalog.connection().unwrap(),
        &StoredTranscript {
            asset_id: "a1".to_string(),
            recipe_key: STT_RECIPE_KEY.to_string(),
            source_fingerprint: "stale-local-proxy-fingerprint".to_string(),
            status: TranscriptStatus::Speech,
            outcome_reason: crate::analysis::store::TranscriptOutcomeReason::Accepted,
            reject_reason: String::new(),
            primary_language: "en".to_string(),
            speech_seconds: 2.0,
            window_count: 1,
            provider_segment_count: 1,
            mean_token_probability: Some(0.8),
            repetition_ratio: Some(0.0),
            segments: vec![DecodedSegment {
                start_ms: 250,
                end_ms: 2_250,
                language: "en".to_string(),
                text: "STALE_LOCAL_CANARY".to_string(),
                mean_token_probability: Some(0.8),
            }],
        },
    )
    .unwrap();
    assert!(
        catalog.prompt_transcript("a1").unwrap().is_none(),
        "stale local speech must not become AI prompt context"
    );

    save_transcript(
        &catalog.connection().unwrap(),
        &StoredTranscript {
            asset_id: "a1".to_string(),
            recipe_key: STT_RECIPE_KEY.to_string(),
            source_fingerprint: fingerprint,
            status: TranscriptStatus::Speech,
            outcome_reason: crate::analysis::store::TranscriptOutcomeReason::Accepted,
            reject_reason: String::new(),
            primary_language: "en".to_string(),
            speech_seconds: 2.0,
            window_count: 1,
            provider_segment_count: 1,
            mean_token_probability: Some(0.8),
            repetition_ratio: Some(0.0),
            segments: vec![DecodedSegment {
                start_ms: 250,
                end_ms: 2_250,
                language: "en".to_string(),
                text: "ACCEPTED_SPEECH_CANARY".to_string(),
                mean_token_probability: Some(0.8),
            }],
        },
    )
    .unwrap();
    let accepted = catalog.prompt_transcript("a1").unwrap().unwrap();
    assert_eq!(accepted.status, TranscriptStatus::Speech);
    assert!(accepted.text.contains("ACCEPTED_SPEECH_CANARY"));
}

fn insert_asset(
    catalog: &Catalog,
    project_id: &str,
    id: &str,
    filename: &str,
    camera: &str,
    raw_time: &str,
    path: &Path,
) {
    catalog
        .connection()
        .unwrap()
        .execute(
            r#"
                INSERT INTO assets (
                    id, project_id, fingerprint, path, filename, size_bytes,
                    duration_seconds, frame_rate, width, height, codec, camera,
                    captured_at_raw, best_time, source_out, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, 100, 10, 30, 1920, 1080,
                          'h264', ?6, ?7, ?8, 10, ?9)
                "#,
            params![
                id,
                project_id,
                format!("fingerprint-{id}"),
                path.to_string_lossy(),
                filename,
                camera,
                raw_time,
                normalize_datetime(raw_time, 0),
                now(),
            ],
        )
        .unwrap();
}

#[test]
fn rename_camera_moves_only_the_named_lane_in_the_named_project() {
    let (_directory, catalog, project) = catalog_fixture();
    let other = catalog.create_project("Another trip").unwrap();
    for (id, camera) in [
        ("a1", UNKNOWN_CAMERA),
        ("a2", UNKNOWN_CAMERA),
        ("a3", "GoPro"),
    ] {
        insert_asset(
            &catalog,
            &project.id,
            id,
            &format!("{id}.MP4"),
            camera,
            "2026-04-01T09:00:00Z",
            Path::new("/volumes/nas/clip.MP4"),
        );
    }
    insert_asset(
        &catalog,
        &other.id,
        "b1",
        "b1.MP4",
        UNKNOWN_CAMERA,
        "2026-04-01T09:00:00Z",
        Path::new("/volumes/nas/other.MP4"),
    );

    let changed = catalog
        .rename_camera(&project.id, UNKNOWN_CAMERA, "  DJI Pocket 3  ")
        .unwrap();

    assert_eq!(changed, 2);
    let cameras = |id: &str| catalog.get_asset(id).unwrap().camera;
    assert_eq!(cameras("a1"), "DJI Pocket 3", "trims the typed name");
    assert_eq!(cameras("a2"), "DJI Pocket 3");
    assert_eq!(cameras("a3"), "GoPro", "another lane is untouched");
    assert_eq!(
        cameras("b1"),
        UNKNOWN_CAMERA,
        "the same lane in another project is untouched"
    );
}

#[test]
fn rename_camera_refuses_an_empty_name() {
    let (_directory, catalog, project) = catalog_fixture();
    insert_asset(
        &catalog,
        &project.id,
        "a1",
        "a1.MP4",
        "GoPro",
        "2026-04-01T09:00:00Z",
        Path::new("/volumes/nas/clip.MP4"),
    );
    assert!(catalog.rename_camera(&project.id, "GoPro", "   ").is_err());
    assert_eq!(catalog.get_asset("a1").unwrap().camera, "GoPro");
}

#[test]
fn creates_and_lists_projects() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let created = catalog.create_project("Canada 2026").unwrap();
    let projects = catalog.list_projects().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].id, created.id);
    assert_eq!(projects[0].name, "Canada 2026");
}

#[test]
fn opens_existing_catalogs_without_mutating_unrelated_databases() {
    let directory = tempdir().unwrap();
    let catalog_path = directory.path().join("catalog.sqlite3");
    let catalog = Catalog::open(&catalog_path).unwrap();
    let project = catalog.create_project("Existing").unwrap();
    drop(catalog);
    let reopened = Catalog::open_existing(&catalog_path).unwrap();
    assert_eq!(reopened.list_projects().unwrap()[0].id, project.id);

    let unrelated_path = directory.path().join("unrelated.sqlite3");
    Connection::open(&unrelated_path)
        .unwrap()
        .execute("CREATE TABLE unrelated (id INTEGER PRIMARY KEY)", [])
        .unwrap();
    assert!(Catalog::open_existing(&unrelated_path).is_err());
    let unrelated = Connection::open(&unrelated_path).unwrap();
    let vlog_tables: i64 = unrelated
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('projects', 'assets')",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(vlog_tables, 0);
    assert!(Catalog::open_existing(directory.path().join("missing.sqlite3")).is_err());
}

#[test]
fn migrates_legacy_catalogs_to_labels_removal_and_landscape_orientation() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(include_str!(
            "../tests/fixtures/catalog_pre_orientation_labels.sql"
        ))
        .unwrap();
    drop(connection);

    let bytes_before_read_only = fs::read(&path).unwrap();
    let read_only = Catalog::open_read_only_existing(&path).unwrap();
    let legacy_asset = read_only.get_asset("asset_portrait").unwrap();
    assert!(!legacy_asset.orientation_probed);
    assert_eq!(legacy_asset.metadata_rotation_degrees, 0);
    assert_eq!(legacy_asset.notes, "keep this note");
    assert!(legacy_asset.labels.is_empty());
    assert!(!legacy_asset.removed);
    assert_eq!(legacy_asset.media_object_id, None);
    assert_eq!(
        legacy_asset.media_verification_state,
        MediaVerificationState::Unlinked
    );
    drop(read_only);
    let inspection = inspect_catalog(&path).unwrap();
    assert_eq!(inspection.current_schema_version, 0);
    assert_eq!(
        inspection.schema_status,
        MigrationHistoryStatus::LegacyUnversioned
    );
    assert_eq!(
        inspection.pending_migrations.len(),
        PROJECT_CATALOG_SCHEMA_VERSION as usize
    );
    assert_eq!(fs::read(&path).unwrap(), bytes_before_read_only);
    let still_legacy = Connection::open(&path).unwrap();
    assert!(!asset_column_exists(&still_legacy, "orientation_probed").unwrap());
    assert!(!table_exists(&still_legacy, "schema_migrations").unwrap());
    drop(still_legacy);

    let catalog = Catalog::open_existing(&path).unwrap();
    let asset = catalog.get_asset("asset_portrait").unwrap();
    assert_eq!(asset.id, "asset_portrait");
    assert_eq!(asset.project_id, "project_old");
    assert_eq!(asset.metadata_rotation_degrees, 0);
    assert_eq!(asset.default_rotation_degrees, 90);
    assert_eq!(asset.rotation_degrees, 90);
    assert_eq!(asset.notes, "keep this note");
    assert!(asset.labels.is_empty());
    assert!(!asset.removed);
    assert_eq!(
        asset.proxy_path.as_deref(),
        Some("/legacy/cache/asset_portrait.v2.mp4")
    );
    assert_eq!(asset.media_object_id, None);
    assert_eq!(
        asset.media_verification_state,
        MediaVerificationState::Unlinked
    );
    let asset_json = serde_json::to_value(&asset).unwrap();
    assert_eq!(asset_json["mediaObjectId"], serde_json::Value::Null);
    assert_eq!(asset_json["mediaVerificationState"], "unlinked");
    let project_json = serde_json::to_value(&catalog.list_projects().unwrap()[0]).unwrap();
    assert_eq!(project_json["originNodeId"], serde_json::Value::Null);
    assert_eq!(project_json["pairedServerNodeId"], serde_json::Value::Null);
    assert_eq!(
        catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM clock_events WHERE project_id = 'project_old'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    let migrated = Connection::open(&path).unwrap();
    assert!(asset_column_exists(&migrated, "labels_json").unwrap());
    assert!(asset_column_exists(&migrated, "removed").unwrap());
    assert!(asset_column_exists(&migrated, "media_object_id").unwrap());
    assert_eq!(
            migrated
                .query_row(
                    "SELECT path FROM asset_thumbnails WHERE asset_id = 'asset_portrait' AND quality = 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "/legacy/cache/asset_portrait/q1.v2.jpg"
        );
    let migration_count = migrated
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(migration_count, PROJECT_CATALOG_SCHEMA_VERSION as i64);
}

#[test]
fn unversioned_current_catalog_upgrades_once_without_changing_ids_or_annotations() {
    type ArtifactRow = (String, String, String, String);
    type StepRow = (String, String, String, String, String);

    fn moment_rows(catalog: &Catalog) -> (ArtifactRow, StepRow, i64, i64) {
        let connection = catalog.connection().unwrap();
        let artifact = connection
            .query_row(
                "SELECT asset_id,identity,body_json,created_at FROM moment_artifacts",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        let step = connection
            .query_row(
                "SELECT asset_id,identity,step_key,body_json,created_at FROM moment_steps",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        let artifact_count = connection
            .query_row("SELECT COUNT(*) FROM moment_artifacts", [], |row| {
                row.get(0)
            })
            .unwrap();
        let step_count = connection
            .query_row("SELECT COUNT(*) FROM moment_steps", [], |row| row.get(0))
            .unwrap();
        (artifact, step, artifact_count, step_count)
    }

    let directory = tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite3");
    let catalog = Catalog::open(&path).unwrap();
    let project = catalog.create_project("Current catalog").unwrap();
    insert_asset(
        &catalog,
        &project.id,
        "asset_existing",
        "A.MP4",
        "Camera A",
        "2026-07-18T00:00:00Z",
        Path::new("/missing/A.MP4"),
    );
    let current = catalog.get_asset("asset_existing").unwrap();
    catalog
        .update_asset(
            "asset_existing",
            &AssetPatch {
                notes: "preserved note".to_string(),
                labels: vec!["arrival".to_string()],
                removed: true,
                rotation_degrees: current.rotation_degrees,
                expected_revision: None,
            },
        )
        .unwrap();
    catalog.connection().unwrap().execute(
            "INSERT INTO moment_artifacts(asset_id,identity,body_json,created_at) VALUES ('asset_existing','base-identity','{\"preserved\":\"artifact\"}','artifact-created')",
            [],
        ).unwrap();
    catalog.connection().unwrap().execute(
            "INSERT INTO moment_steps(asset_id,identity,step_key,body_json,created_at) VALUES ('asset_existing','base-identity','coarse:0','{\"preserved\":\"step\"}','step-created')",
            [],
        ).unwrap();
    let expected_moment_rows = moment_rows(&catalog);
    assert_eq!(expected_moment_rows.2, 1);
    assert_eq!(expected_moment_rows.3, 1);
    drop(catalog);

    Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TABLE schema_migrations")
        .unwrap();
    let before = inspect_catalog(&path).unwrap();
    assert_eq!(before.current_schema_version, 0);
    assert_eq!(
        before.pending_migrations.len(),
        PROJECT_CATALOG_SCHEMA_VERSION as usize
    );

    let upgraded = Catalog::open_existing(&path).unwrap();
    let upgraded_project = upgraded.list_projects().unwrap().remove(0);
    let upgraded_asset = upgraded.get_asset("asset_existing").unwrap();
    assert_eq!(upgraded_project.id, project.id);
    assert_eq!(upgraded_asset.id, "asset_existing");
    assert_eq!(upgraded_asset.notes, "preserved note");
    assert_eq!(upgraded_asset.labels, vec!["arrival"]);
    assert!(upgraded_asset.removed);
    assert_eq!(
        moment_rows(&upgraded),
        expected_moment_rows,
        "migration replay must preserve cached moment artifacts and resumable steps"
    );
    drop(upgraded);

    let first_history = inspect_catalog(&path).unwrap().applied_migrations;
    assert_eq!(first_history.len(), PROJECT_CATALOG_SCHEMA_VERSION as usize);
    let reopened = Catalog::open_existing(&path).unwrap();
    assert_eq!(
        reopened.get_asset("asset_existing").unwrap().id,
        "asset_existing"
    );
    assert_eq!(
        moment_rows(&reopened),
        expected_moment_rows,
        "reopening the upgraded catalog must retain the exact moment rows"
    );
    drop(reopened);
    let reopened_inspection = inspect_catalog(&path).unwrap();
    assert_eq!(
        reopened_inspection.current_schema_version,
        PROJECT_CATALOG_SCHEMA_VERSION
    );
    assert_eq!(
        reopened_inspection.schema_status,
        MigrationHistoryStatus::Current
    );
    assert!(reopened_inspection.pending_migrations.is_empty());
    assert_eq!(reopened_inspection.applied_migrations, first_history);
}

#[test]
fn migration_four_upgrades_to_operation_baselines_without_inventing_history() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("catalog-v4.sqlite3");
    let mut connection = Connection::open(&path).unwrap();
    migrations::run_sqlite_migrations(
        &mut connection,
        &migrations::PROJECT_CATALOG_MIGRATIONS[..4],
    )
    .unwrap();
    connection.execute("INSERT INTO projects(id, name, created_at) VALUES ('project_v4', 'Version four', '2026-01-01T00:00:00Z')", []).unwrap();
    connection.execute("INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, notes, labels_json, removed, rotation_degrees, created_at) VALUES ('asset_v4', 'project_v4', 'fingerprint-v4', '/offline/v4.mp4', 'v4.mp4', 100, 10, 30, 1920, 1080, 'h264', 'DJI', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00.000Z', 'kept note', '[\"kept label\"]', 1, 90, '2026-01-01T00:00:00Z')", []).unwrap();
    connection.execute("INSERT INTO clock_events(id, project_id, camera, start_filename, offset_seconds, label, created_at) VALUES ('clock_v4', 'project_v4', 'DJI', 'v4.mp4', 60, 'kept clock', '2026-01-01T00:00:00Z')", []).unwrap();
    drop(connection);

    let before = inspect_catalog(&path).unwrap();
    assert_eq!(before.current_schema_version, 4);
    assert_eq!(
        before.pending_migrations.len(),
        PROJECT_CATALOG_SCHEMA_VERSION as usize - 4
    );
    let catalog = Catalog::open_existing(&path).unwrap();
    let asset = catalog.get_asset("asset_v4").unwrap();
    assert_eq!(asset.notes, "kept note");
    assert_eq!(asset.labels, ["kept label"]);
    assert!(asset.removed);
    assert_eq!(asset.rotation_degrees, 90);
    assert_eq!(catalog.list_operations("project_v4").unwrap().len(), 0);
    let video_validation = catalog.asset_video_validation("asset_v4").unwrap();
    assert_eq!(
        video_validation.status,
        AssetVideoValidationStatus::Unverified
    );
    assert!(!video_validation.bytes_reachable);
    let diagnostics = catalog.asset_revision_diagnostics("asset_v4").unwrap();
    assert_eq!(diagnostics.materialized_revision, 0);
    assert_eq!(diagnostics.note_revisions.len(), 1);
    assert!(diagnostics.note_revisions[0].active);
    let connection = catalog.connection().unwrap();
    for table in [
        "project_baseline_metadata",
        "project_storage_root_mappings",
        "sync_origin_cursors",
        "sync_observation_facts",
        "sync_representation_manifests",
    ] {
        assert!(table_exists(&connection, table).unwrap(), "{table}");
    }
    assert_eq!(
            connection.query_row("SELECT revision FROM materialized_revisions WHERE project_id='project_v4' AND aggregate_type='clock' AND aggregate_id='DJI'", [], |row| row.get::<_, i64>(0)).unwrap(),
            1
        );
}

#[test]
fn writable_open_rejects_a_newer_catalog_with_an_actionable_error() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite3");
    drop(Catalog::open(&path).unwrap());
    Connection::open(&path)
            .unwrap()
            .execute(
                "INSERT INTO schema_migrations (migration_id, name, source_identity, applied_at) VALUES (?1, 'future', ?2, '2026-07-20T00:00:00.000Z')",
                params![
                    PROJECT_CATALOG_SCHEMA_VERSION + 1,
                    format!("future/{}", PROJECT_CATALOG_SCHEMA_VERSION + 1)
                ],
            )
            .unwrap();

    let inspection = inspect_catalog(&path).unwrap();
    assert_eq!(
        inspection.current_schema_version,
        PROJECT_CATALOG_SCHEMA_VERSION + 1
    );
    assert_eq!(
        inspection.schema_status,
        MigrationHistoryStatus::NewerUnsupported
    );
    let error = Catalog::open_existing(&path).unwrap_err();
    assert_eq!(
        error.downcast_ref::<CatalogSchemaError>().unwrap().code(),
        "schema_incompatible"
    );
    let message = format!("{error:#}");
    assert!(message.contains(&format!(
        "newer than supported version {PROJECT_CATALOG_SCHEMA_VERSION}"
    )));
    assert!(message.contains("upgrade RCD VlogCutter"));
}

#[test]
fn changed_migration_name_or_source_identity_is_inspectable_but_not_writable() {
    for (stored_name, stored_source_identity) in [
        (
            "changed_catalog_baseline",
            "project-catalog/0001-catalog-baseline/20260720",
        ),
        ("catalog_baseline", "changed/project-catalog/0001"),
    ] {
        let directory = tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite3");
        drop(Catalog::open(&path).unwrap());
        Connection::open(&path)
                .unwrap()
                .execute(
                    "UPDATE schema_migrations SET name = ?1, source_identity = ?2 WHERE migration_id = 1",
                    params![stored_name, stored_source_identity],
                )
                .unwrap();

        let before = inspect_catalog(&path).unwrap();
        assert_eq!(
            before.schema_status,
            MigrationHistoryStatus::IncompatibleHistory
        );
        assert!(before.pending_migrations.is_empty());
        let error = Catalog::open_existing(&path).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<CatalogSchemaError>(),
            Some(CatalogSchemaError::IncompatibleHistory {
                migration_id: 1,
                ..
            })
        ));
        let after = inspect_catalog(&path).unwrap();
        assert_eq!(after.applied_migrations, before.applied_migrations);
        assert_eq!(after.pending_migrations, before.pending_migrations);
        assert_eq!(
            after.schema_status,
            MigrationHistoryStatus::IncompatibleHistory
        );
    }
}

#[test]
fn migration_history_gap_is_rejected_without_applying_the_pending_migration() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite3");
    drop(Catalog::open(&path).unwrap());
    Connection::open(&path)
        .unwrap()
        .execute("DELETE FROM schema_migrations WHERE migration_id = 2", [])
        .unwrap();

    let before = inspect_catalog(&path).unwrap();
    assert_eq!(
        before.current_schema_version,
        PROJECT_CATALOG_SCHEMA_VERSION
    );
    assert_eq!(
        before.schema_status,
        MigrationHistoryStatus::IncompatibleHistory
    );
    assert_eq!(
        before
            .pending_migrations
            .iter()
            .map(|migration| migration.id)
            .collect::<Vec<_>>(),
        vec![2]
    );
    let error = Catalog::open_existing(&path).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<CatalogSchemaError>(),
        Some(CatalogSchemaError::IncompatibleHistory {
            migration_id: 2,
            ..
        })
    ));
    assert!(format!("{error:#}").contains("migration history contains a gap"));

    let after = inspect_catalog(&path).unwrap();
    assert_eq!(after.applied_migrations, before.applied_migrations);
    assert_eq!(after.pending_migrations, before.pending_migrations);
    assert_eq!(
        after.schema_status,
        MigrationHistoryStatus::IncompatibleHistory
    );
}

#[test]
fn normalizes_offset_datetimes() {
    assert_eq!(
        normalize_datetime("2026-07-18T09:00:00+09:00", -3600).as_deref(),
        Some("2026-07-17T23:00:00.000Z")
    );
    assert_eq!(
        normalize_datetime("2026:07:18 09:00:00", 60).as_deref(),
        Some("2026-07-18T09:01:00.000Z")
    );
    assert!(normalize_datetime("not a datetime", 0).is_none());
}

#[test]
fn validates_projects_labels_rotation_and_missing_ids() {
    let (_directory, catalog, project) = catalog_fixture();
    assert!(catalog.create_project("  ").is_err());
    insert_asset(
        &catalog,
        &project.id,
        "asset_a",
        "A.MP4",
        "Camera A",
        "2026-07-18T00:00:00Z",
        Path::new("/missing/A.MP4"),
    );
    let current = catalog.get_asset("asset_a").unwrap();
    let result = catalog.update_asset(
        "asset_a",
        &AssetPatch {
            notes: current.notes.clone(),
            labels: vec!["x".repeat(65)],
            removed: false,
            rotation_degrees: current.rotation_degrees,
            expected_revision: None,
        },
    );
    assert!(result.is_err());
    assert!(catalog.get_asset("asset_missing").is_err());
    assert!(catalog.list_assets("project_missing").is_err());
}

#[test]
fn persists_only_right_angle_orientation_overrides() {
    let (_directory, catalog, project) = catalog_fixture();
    insert_asset(
        &catalog,
        &project.id,
        "asset_a",
        "A.MP4",
        "Camera A",
        "2026-07-18T00:00:00Z",
        Path::new("/missing/A.MP4"),
    );
    let asset = catalog.get_asset("asset_a").unwrap();
    let updated = catalog
        .update_asset(
            &asset.id,
            &AssetPatch {
                notes: asset.notes.clone(),
                labels: asset.labels.clone(),
                removed: asset.removed,
                rotation_degrees: 180,
                expected_revision: None,
            },
        )
        .unwrap();
    assert_eq!(updated.rotation_degrees, 180);
    assert_eq!(landscape_default_rotation(1080, 1920, 0), 90);
    assert_eq!(landscape_default_rotation(1920, 1080, 90), 180);
    assert!(
        catalog
            .update_asset(
                &asset.id,
                &AssetPatch {
                    notes: asset.notes,
                    labels: asset.labels,
                    removed: asset.removed,
                    rotation_degrees: 45,
                    expected_revision: None,
                },
            )
            .is_err()
    );
}

#[test]
fn invalid_orientation_decisions_are_rejected_in_rows_and_baseline_import() {
    let (_directory, catalog, project) = catalog_fixture();
    let source_dir = tempfile::tempdir().unwrap();
    let source = source_dir.path().join("decision-corruption.mp4");
    std::fs::write(&source, b"synthetic").unwrap();
    insert_asset_at(&catalog, &project.id, "asset_decision_corruption", &source);
    let asset = catalog.get_asset("asset_decision_corruption").unwrap();
    let mut snapshot = catalog.export_project_baseline(&project.id).unwrap();
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET orientation_decision_json=?1 WHERE id=?2",
            rusqlite::params![r#"{"status":"confirmed","rotationDegrees":45}"#, asset.id],
        )
        .unwrap();
    assert!(catalog.get_asset(&asset.id).is_err());
    assert!(catalog.export_project_baseline(&project.id).is_err());

    snapshot.assets[0].orientation_decision = Some(OrientationDecision {
        status: "confirmed".into(),
        rotation_degrees: Some(45),
    });
    let destination = source_dir.path().join("should-not-exist.sqlite3");
    assert!(
        Catalog::import_project_baseline(&destination, &snapshot, "node", 1, "digest").is_err()
    );
    assert!(!destination.exists());
}

#[test]
fn orientation_evidence_decodes_asymmetric_raw_and_effective_pixels() {
    let (_directory, catalog, project) = catalog_fixture();
    let source_dir = tempfile::tempdir().unwrap();
    let source = source_dir.path().join("asymmetric.mp4");
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=yellow:s=64x32:r=1:d=1",
            "-vf",
            "drawbox=x=0:y=0:w=16:h=8:color=red:t=fill",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&source)
        .status();
    let Ok(status) = status else { return };
    if !status.success() {
        return;
    }
    catalog.ingest_file(&project.id, &source).unwrap();
    let asset = catalog.list_assets(&project.id).unwrap().remove(0);
    catalog.connection().unwrap().execute(
            "UPDATE assets SET metadata_rotation_degrees=90,default_rotation_degrees=90,rotation_degrees=90,orientation_probed=0 WHERE id=?1",
            [&asset.id],
        ).unwrap();
    let asset = catalog
        .decide_orientation(
            &asset.id,
            asset.revision,
            OrientationDecision {
                status: "confirmed".into(),
                rotation_degrees: Some(90),
            },
        )
        .unwrap();
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET orientation_probed=0 WHERE id=?1",
            [&asset.id],
        )
        .unwrap();
    let asset = catalog.refresh_asset_orientation(&asset.id).unwrap();
    catalog.connection().unwrap().execute(
            "UPDATE assets SET metadata_rotation_degrees=90,default_rotation_degrees=90,rotation_degrees=90 WHERE id=?1",
            [&asset.id],
        ).unwrap();
    let asset = catalog.get_asset(&asset.id).unwrap();
    assert_eq!(asset.rotation_degrees, 90);
    assert_eq!(
        asset.orientation_decision.unwrap().rotation_degrees,
        Some(90)
    );
    let evidence = catalog.orientation_evidence(&asset.id).unwrap();
    assert_eq!(evidence.representation, "original");
    let raw = decode_rgb(evidence.raw_frame_path.as_deref().unwrap());
    let effective = decode_rgb(evidence.effective_frame_path.as_deref().unwrap());
    assert_eq!((raw.0, raw.1), (32, 64));
    assert_eq!((effective.0, effective.1), (32, 64));
    assert!(is_red(raw.0, &raw.2, 28, 4));
    assert!(is_red(effective.0, &effective.2, 28, 4));
    assert!(!is_red(effective.0, &effective.2, 4, 4));
    catalog.generate_proxy(&asset.id).unwrap();
    std::fs::rename(&source, source_dir.path().join("original-offline.mp4")).unwrap();
    let offline_evidence = catalog.orientation_evidence(&asset.id).unwrap();
    assert_eq!(offline_evidence.representation, "proxy");
    assert!(!offline_evidence.original_available);
    let proxy_raw = decode_rgb(offline_evidence.raw_frame_path.as_deref().unwrap());
    let proxy_effective = decode_rgb(offline_evidence.effective_frame_path.as_deref().unwrap());
    assert_eq!((proxy_raw.0, proxy_raw.1), (32, 64));
    assert_eq!((proxy_effective.0, proxy_effective.1), (32, 64));
    assert!(is_red(proxy_raw.0, &proxy_raw.2, 28, 4));
    assert_eq!(
        is_red(proxy_raw.0, &proxy_raw.2, 28, 4),
        is_red(raw.0, &raw.2, 28, 4)
    );
    assert_eq!(
        is_red(proxy_effective.0, &proxy_effective.2, 28, 4),
        is_red(effective.0, &effective.2, 28, 4)
    );
}

fn decode_rgb(path: &str) -> (usize, usize, Vec<u8>) {
    let dimensions = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        dimensions.status.success(),
        "{}",
        String::from_utf8_lossy(&dimensions.stderr)
    );
    let dims: Value = serde_json::from_slice(&dimensions.stdout).unwrap();
    let stream = &dims["streams"][0];
    let width = stream["width"].as_u64().unwrap() as usize;
    let height = stream["height"].as_u64().unwrap() as usize;
    let pixels = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "pipe:1",
        ])
        .output()
        .unwrap();
    assert!(pixels.status.success());
    (width, height, pixels.stdout)
}

fn is_red(width: usize, pixels: &[u8], x: usize, y: usize) -> bool {
    let index = (y * width + x) * 3;
    pixels[index] > 180 && pixels[index + 1] < 80 && pixels[index + 2] < 80
}

#[test]
fn orders_by_best_time_then_filename_and_counts_labeled_assets() {
    let (_directory, catalog, project) = catalog_fixture();
    for (id, filename, raw_time) in [
        ("asset_late", "B.MP4", "2026-07-18T03:00:00Z"),
        ("asset_tie_b", "D.MP4", "2026-07-18T02:00:00Z"),
        ("asset_tie_a", "C.MP4", "2026-07-18T02:00:00Z"),
    ] {
        insert_asset(
            &catalog,
            &project.id,
            id,
            filename,
            "Camera A",
            raw_time,
            Path::new("/missing/source.mp4"),
        );
    }
    let tie = catalog.get_asset("asset_tie_a").unwrap();
    catalog
        .update_asset(
            &tie.id,
            &AssetPatch {
                notes: "labeled".to_string(),
                labels: vec!["arrival".to_string()],
                removed: false,
                rotation_degrees: tie.rotation_degrees,
                expected_revision: None,
            },
        )
        .unwrap();

    let filenames: Vec<_> = catalog
        .list_assets(&project.id)
        .unwrap()
        .into_iter()
        .map(|asset| asset.filename)
        .collect();
    assert_eq!(filenames, ["C.MP4", "D.MP4", "B.MP4"]);
    let summary = catalog.list_projects().unwrap().remove(0);
    assert_eq!(summary.asset_count, 3);
    assert_eq!(summary.labeled_count, 1);
    assert_eq!(summary.removed_count, 0);
}

#[test]
fn clock_event_only_changes_matching_camera_at_and_after_boundary() {
    let (_directory, catalog, project) = catalog_fixture();
    for (id, filename, camera) in [
        ("a1", "C0001.MP4", "Camera A"),
        ("a2", "C0002.MP4", "Camera A"),
        ("a3", "C0003.MP4", "Camera A"),
        ("b2", "C0002.MP4", "Camera B"),
    ] {
        insert_asset(
            &catalog,
            &project.id,
            id,
            filename,
            camera,
            "2026-07-18T10:00:00Z",
            Path::new("/missing/source.mp4"),
        );
    }
    catalog
        .apply_clock_event(
            &project.id,
            "Camera A",
            "C0002.MP4",
            -3600,
            "timezone change",
        )
        .unwrap();

    assert_eq!(catalog.get_asset("a1").unwrap().offset_seconds, 0);
    for id in ["a2", "a3"] {
        let asset = catalog.get_asset(id).unwrap();
        assert_eq!(asset.offset_seconds, -3600);
        assert_eq!(asset.best_time.as_deref(), Some("2026-07-18T09:00:00.000Z"));
    }
    assert_eq!(catalog.get_asset("b2").unwrap().offset_seconds, 0);
}

#[test]
fn clock_epochs_preserve_later_boundaries_when_earlier_events_are_added() {
    let (_directory, catalog, project) = catalog_fixture();
    for (id, filename) in [
        ("a1", "C0001.MP4"),
        ("a2", "C0002.MP4"),
        ("a3", "C0003.MP4"),
    ] {
        insert_asset(
            &catalog,
            &project.id,
            id,
            filename,
            "Camera A",
            "2026-07-18T10:00:00Z",
            Path::new("/missing/source.mp4"),
        );
    }
    catalog
        .apply_clock_event(
            &project.id,
            "Camera A",
            "C0003.MP4",
            -7200,
            "later boundary",
        )
        .unwrap();
    catalog
        .apply_clock_event(
            &project.id,
            "Camera A",
            "C0002.MP4",
            -3600,
            "earlier boundary added later",
        )
        .unwrap();
    assert_eq!(catalog.get_asset("a1").unwrap().offset_seconds, 0);
    assert_eq!(catalog.get_asset("a2").unwrap().offset_seconds, -3600);
    assert_eq!(catalog.get_asset("a3").unwrap().offset_seconds, -7200);

    catalog
        .apply_clock_event(
            &project.id,
            "Camera A",
            "C0002.MP4",
            -1800,
            "replacement at same boundary",
        )
        .unwrap();
    assert_eq!(catalog.get_asset("a2").unwrap().offset_seconds, -1800);
    assert_eq!(catalog.get_asset("a3").unwrap().offset_seconds, -7200);
}

#[test]
fn proxy_only_asset_is_available_and_missing_proxy_is_reported() {
    let (directory, catalog, project) = catalog_fixture();
    insert_asset(
        &catalog,
        &project.id,
        "asset_offline",
        "OFFLINE.MP4",
        "Camera A",
        "2026-07-18T00:00:00Z",
        directory.path().join("missing.mp4").as_path(),
    );
    mark_asset_video_usable(&catalog, "asset_offline");
    assert!(!catalog.get_asset("asset_offline").unwrap().available);
    let report = catalog.generate_missing_proxies(&project.id).unwrap();
    assert_eq!(report.generated, 0);
    assert_eq!(report.failures.len(), 1);

    let proxy = directory.path().join("offline-proxy.mp4");
    fs::write(&proxy, b"proxy sentinel").unwrap();
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET proxy_path = ?2 WHERE id = ?1",
            params!["asset_offline", proxy.to_string_lossy()],
        )
        .unwrap();
    assert!(catalog.get_asset("asset_offline").unwrap().available);
    let report = catalog.generate_missing_proxies(&project.id).unwrap();
    assert_eq!(report.skipped, 1);
    assert!(report.failures.is_empty());
}

#[test]
fn recognizes_media_extensions_and_parses_rates() {
    assert!(is_media(Path::new("clip.MP4")));
    assert!(is_media(Path::new("clip.m2ts")));
    assert!(!is_media(Path::new("notes.txt")));
    assert_eq!(
        proxy_representation("asset.v3.mp4"),
        ProxyRepresentation::RawV3
    );
    assert_eq!(
        proxy_representation("asset.v2.mp4"),
        ProxyRepresentation::MetadataTaggedRawV2
    );
    assert_eq!(
        proxy_representation("asset.mp4"),
        ProxyRepresentation::MetadataBakedV1
    );
    assert!((parse_rate("60000/1001").unwrap() - 59.94005994).abs() < 0.0001);
    assert_eq!(parse_rate("30/0"), None);
    assert_eq!(parse_rate("invalid"), None);
}

#[test]
fn parses_ffmpeg_timestamp_progress_into_residual_byte_work() {
    assert_eq!(
        ffmpeg_progress_fraction("out_time_us=5000000", 10.0),
        Some(0.5)
    );
    assert_eq!(
        ffmpeg_progress_fraction("out_time_us=12000000", 10.0),
        Some(1.0)
    );
    assert_eq!(ffmpeg_progress_fraction("progress=continue", 10.0), None);
    assert_eq!(ffmpeg_progress_fraction("out_time_us=1", 0.0), None);
    assert_eq!(work_units_from_fraction(1_000, 0.25), 250);
    assert_eq!(
        residual_work_units(&[1_000, 500], &[false, true], &[250, 0]),
        vec![750]
    );
}

#[test]
fn media_diagnostics_redact_paths_and_bound_the_redacted_text() {
    let stderr = "failure ".to_owned() + &"/a ".repeat(2_000);
    let diagnostic = redact_media_diagnostic(&stderr, Path::new("/a"), Path::new("/output"));
    assert!(diagnostic.len() <= 4096);
    assert!(!diagnostic.contains("/a"));
    assert!(diagnostic.contains("<source>"));

    let quoted = redact_media_diagnostic(
        "could not read '/private area/clip.mp4': invalid data",
        Path::new("/source"),
        Path::new("/output"),
    );
    assert!(!quoted.contains("/private area/clip.mp4"));
    assert!(quoted.contains("invalid data"));
}

#[cfg(target_os = "macos")]
#[test]
fn mac_proxy_command_uses_videotoolbox_with_quality_matched_settings() {
    let command = proxy_ffmpeg_command(
        Path::new("source.mp4"),
        Path::new("proxy.mp4"),
        ProxyEncoder::VideoToolbox,
    );
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    assert!(
        args.windows(2)
            .any(|pair| pair == ["-hwaccel", "videotoolbox"])
    );
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-c:v", "h264_videotoolbox"])
    );
    assert!(args.windows(2).any(|pair| pair == ["-q:v", "36"]));
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-power_efficient", "1"])
    );
    assert!(args.windows(2).any(|pair| pair == ["-map", "0:v:0"]));
    assert!(args.windows(2).any(|pair| pair == ["-map", "0:a:0?"]));
    assert!(args.iter().any(|arg| arg.starts_with("scale=")));
    assert!(!args.iter().any(|arg| arg.starts_with("scale_vt=")));
    assert!(args.windows(2).any(|pair| pair == ["-progress", "pipe:1"]));
    assert!(args.windows(2).any(|pair| pair == ["-stats_period", "1"]));
}

#[cfg(target_os = "macos")]
#[test]
fn mac_missing_proxy_batch_uses_four_workers_and_commits_every_asset() {
    if tool_command("ffmpeg").arg("-version").output().is_err()
        || tool_command("ffprobe").arg("-version").output().is_err()
    {
        eprintln!("ffmpeg or ffprobe is unavailable; skipping media integration test");
        return;
    }
    let directory = tempdir().unwrap();
    let source_dir = directory.path().join("source");
    fs::create_dir_all(&source_dir).unwrap();
    for (index, color) in ["red", "green", "blue", "yellow"].into_iter().enumerate() {
        let source = source_dir.join(format!("C{index:04}.MP4"));
        let input = format!("color=c={color}:s=160x90:r=10:d=0.2");
        let status = tool_command("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                &input,
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(source)
            .status()
            .unwrap();
        assert!(status.success());
    }

    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Parallel proxies").unwrap();
    let ingest = catalog.ingest_folder(&project.id, &source_dir).unwrap();
    assert_eq!(ingest.imported, 4);

    let mut progress = Vec::new();
    let report = catalog
        .generate_missing_proxies_with_progress(&project.id, |event| {
            progress.push(event);
            ControlFlow::Continue(())
        })
        .unwrap();

    assert_eq!(report.generated, 4);
    assert!(report.failures.is_empty());
    assert!(progress.len() >= 9);
    assert_eq!(progress[0].estimate.worker_count, 4);
    assert!(progress.iter().any(|event| {
        event.estimate.completed_items == 0 && event.estimate.completed_work_units > 0
    }));
    assert_eq!(
        catalog
            .proxy_rate_histories()
            .unwrap()
            .iter()
            .filter(|entry| entry.worker_count == 4)
            .map(|entry| entry.samples)
            .sum::<u64>(),
        4
    );
    assert_eq!(
        progress.last().unwrap().estimate.completed_items,
        progress.last().unwrap().estimate.total_items
    );
    assert!(
        catalog
            .list_assets(&project.id)
            .unwrap()
            .iter()
            .all(|asset| {
                asset
                    .proxy_path
                    .as_deref()
                    .is_some_and(|path| Path::new(path).is_file())
            })
    );
}

#[test]
fn a_cancelled_proxy_batch_keeps_what_it_finished_and_resumes() {
    if tool_command("ffmpeg").arg("-version").output().is_err()
        || tool_command("ffprobe").arg("-version").output().is_err()
    {
        eprintln!("ffmpeg or ffprobe is unavailable; skipping media integration test");
        return;
    }
    let directory = tempdir().unwrap();
    let source_dir = directory.path().join("source");
    fs::create_dir_all(&source_dir).unwrap();
    // More clips than workers, so stopping is observable: at the moment the
    // first clip completes only the in-flight claims can still finish.
    let clip_count = PROXY_WORKERS * 2;
    for index in 0..clip_count {
        let source = source_dir.join(format!("C{index:04}.MP4"));
        // Each clip needs distinct content: identical bytes share a
        // fingerprint and ingest would deduplicate them to one asset.
        let shade = 0x20 + index * 0x10;
        let input = format!("color=c=0x{shade:02x}{shade:02x}{shade:02x}:s=160x90:r=10:d=0.2");
        let status = tool_command("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                &input,
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(source)
            .status()
            .unwrap();
        assert!(status.success());
    }

    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Cancelled proxies").unwrap();
    assert_eq!(
        catalog
            .ingest_folder(&project.id, &source_dir)
            .unwrap()
            .imported,
        clip_count
    );

    let report = catalog
        .generate_missing_proxies_with_progress(&project.id, |event| {
            // Stop once one clip has actually been committed, not on the
            // initial zero-progress event.
            if event.asset.is_some() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
        .unwrap();

    assert!(report.cancelled);
    assert!(report.generated >= 1, "the committed clip is kept");
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    // How many clips land before the stop takes effect is deliberately not
    // asserted: workers claim independently of the collector thread that
    // observes the cancellation, so the exact cut-off is a race. What must
    // hold is that finished work is committed and the rest is still pending.

    // Re-running resumes rather than repeating.
    let resumed = catalog.generate_missing_proxies(&project.id).unwrap();
    assert_eq!(resumed.skipped, report.generated);
    assert_eq!(resumed.generated, clip_count - report.generated);
    assert!(!resumed.cancelled);
    assert!(
        catalog
            .list_assets(&project.id)
            .unwrap()
            .iter()
            .all(|asset| asset
                .proxy_path
                .as_deref()
                .is_some_and(|path| Path::new(path).is_file()))
    );
}

#[test]
fn a_silent_clip_reads_back_as_a_transcript_rather_than_as_nothing() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Transcript reads").unwrap();
    let connection = catalog.connection().unwrap();
    connection
        .execute(
            "INSERT INTO assets(id,project_id,fingerprint,path,filename,size_bytes,
                                    duration_seconds,created_at)
                 VALUES ('a1',?1,'f1','/tmp/a1.mp4','a1.mp4',1,1,'2026-08-04T00:00:00Z')",
            [&project.id],
        )
        .unwrap();

    assert!(catalog.asset_transcript("a1").unwrap().is_none());

    save_transcript(
        &connection,
        &StoredTranscript {
            asset_id: "a1".to_string(),
            recipe_key: STT_RECIPE_KEY.to_string(),
            source_fingerprint: "proxy-1".to_string(),
            status: TranscriptStatus::NoSpeech,
            outcome_reason: crate::analysis::store::TranscriptOutcomeReason::NoDetectedSpeech,
            reject_reason: String::new(),
            primary_language: String::new(),
            speech_seconds: 0.0,
            window_count: 0,
            provider_segment_count: 0,
            mean_token_probability: None,
            repetition_ratio: None,
            segments: Vec::new(),
        },
    )
    .unwrap();

    let transcript = catalog.asset_transcript("a1").unwrap().unwrap();
    assert_eq!(transcript.status, TranscriptStatus::NoSpeech);
    assert!(transcript.segments.is_empty());
    // Silence is a result. Returning None here would make a silent clip
    // indistinguishable from one that was never transcribed.
    assert!(catalog.asset_analysis("a1").unwrap().is_none());
}

#[test]
fn readiness_projects_from_the_reference_measurement_until_a_project_has_history() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Readiness").unwrap();

    let readiness = catalog.analysis_readiness(&project.id).unwrap();
    assert_eq!(readiness.total_clips, 0);
    assert_eq!(readiness.missing_proxies, 0);
    assert_eq!(readiness.missing_transcripts, 0);
    assert_eq!(readiness.missing_analyses, 0);
    assert_eq!(readiness.cost_basis, AnalysisCostBasis::Reference);
    assert_eq!(
        readiness.observed_cost_per_clip_usd,
        REFERENCE_COST_PER_CLIP_USD
    );
    // Nothing to analyze means nothing to spend, whatever the per-clip rate.
    assert_eq!(readiness.estimated_cost_usd, 0.0);
    assert!(readiness.last_run.is_none());
    assert_eq!(readiness.models.len(), 2);
}

#[test]
fn a_run_record_carries_its_spend_and_stop_reason() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Run accounting").unwrap();

    let run_id = catalog.begin_analysis_run(&project.id).unwrap();
    let running = catalog.analysis_runs(&project.id, 10).unwrap();
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].status, "running");
    assert_eq!(running[0].finished_at, "");

    catalog
        .finish_analysis_run(
            &run_id,
            &AnalysisRunOutcome {
                status: "cancelled".to_string(),
                stage_reached: "analysis".to_string(),
                proxies_generated: 3,
                clips_transcribed: 2,
                clips_analyzed: 1,
                cost_usd: 0.25,
                budget_exhausted: true,
                error: String::new(),
            },
        )
        .unwrap();

    let finished = catalog.analysis_runs(&project.id, 10).unwrap();
    assert_eq!(finished.len(), 1, "finishing updates rather than appends");
    assert_eq!(finished[0].status, "cancelled");
    assert_eq!(finished[0].stage_reached, "analysis");
    assert_eq!(finished[0].proxies_generated, 3);
    assert_eq!(finished[0].clips_transcribed, 2);
    assert_eq!(finished[0].clips_analyzed, 1);
    assert_eq!(finished[0].cost_usd, 0.25);
    assert!(finished[0].budget_exhausted);
    assert!(!finished[0].finished_at.is_empty());

    let readiness = catalog.analysis_readiness(&project.id).unwrap();
    assert_eq!(readiness.last_run.unwrap().status, "cancelled");
}

#[test]
fn runs_are_scoped_to_their_project() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let first = catalog.create_project("First").unwrap();
    let second = catalog.create_project("Second").unwrap();
    catalog.begin_analysis_run(&first.id).unwrap();
    assert_eq!(catalog.analysis_runs(&first.id, 10).unwrap().len(), 1);
    assert!(catalog.analysis_runs(&second.id, 10).unwrap().is_empty());
}

#[test]
fn a_batch_cancelled_before_any_work_reports_no_generation() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Immediate cancel").unwrap();
    let report = catalog
        .generate_missing_proxies_with_progress(&project.id, |_| ControlFlow::Break(()))
        .unwrap();
    assert!(report.cancelled);
    assert_eq!(report.generated, 0);
}

fn synthetic_proxy_source(directory: &Path) -> PathBuf {
    let source_dir = directory.join("proxy-source");
    fs::create_dir_all(&source_dir).unwrap();
    let source = source_dir.join("CLIP.MP4");
    let status = tool_command("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=24:duration=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success(), "could not create synthetic proxy source");
    source
}

#[test]
fn auto_proxy_generation_records_actual_encoder_recipe_timing_and_validation() {
    if tool_command("ffmpeg").arg("-version").output().is_err()
        || tool_command("ffprobe").arg("-version").output().is_err()
    {
        eprintln!("ffmpeg or ffprobe unavailable; skipping proxy provenance test");
        return;
    }
    let (directory, catalog, project) = catalog_fixture();
    let source = synthetic_proxy_source(directory.path());
    let source_dir = source.parent().unwrap();
    catalog.ingest_folder(&project.id, source_dir).unwrap();
    let asset = catalog.list_assets(&project.id).unwrap().remove(0);
    let proxied = catalog
        .generate_proxy_with_policy(&asset.id, ProxyEncoderPolicy::Auto)
        .unwrap();
    assert_eq!(
        proxied.proxy_representation,
        Some(ProxyRepresentation::RawV3)
    );
    assert!(
        proxied
            .proxy_path
            .as_deref()
            .is_some_and(|path| Path::new(path).is_file())
    );

    let provenance = catalog.proxy_generation_provenance(&asset.id, 10).unwrap();
    assert_eq!(provenance.len(), 1);
    let latest = &provenance[0];
    assert_eq!(latest.status, "succeeded");
    assert!(
        latest.fallback_used
            || matches!(latest.selected_encoder.as_str(), "vaapi" | "videotoolbox")
    );
    let validation = latest.validation.as_ref().unwrap();
    assert!(validation.decoded);
    assert_eq!(validation.metadata_rotation_degrees, 0);
    assert_eq!(validation.audio_stream_count, 1);
    assert_eq!(validation.audio_codec.as_deref(), Some("aac"));
    assert!(
        latest
            .attempts
            .iter()
            .any(|attempt| attempt.status == ProxyAttemptStatus::Succeeded)
    );

    let histories = catalog.proxy_rate_histories().unwrap();
    let matching = histories
        .iter()
        .find(|history| {
            history.encoder == latest.selected_encoder
                && history.task_kind.contains("proxy.h264-720p.orientation-v3")
        })
        .expect("actual encoder should have an encoder-aware rate history");
    assert_eq!(matching.samples, 1);
    assert!(matching.scope.contains("not a cross-corpus benchmark"));
}

#[cfg(unix)]
#[test]
fn both_encoder_failures_preserve_registered_proxy_and_record_failure() {
    use std::os::unix::fs::PermissionsExt;

    if tool_command("ffmpeg").arg("-version").output().is_err()
        || tool_command("ffprobe").arg("-version").output().is_err()
    {
        eprintln!("ffmpeg or ffprobe unavailable; skipping proxy failure provenance test");
        return;
    }
    let (directory, catalog, project) = catalog_fixture();
    let source = synthetic_proxy_source(directory.path());
    catalog
        .ingest_folder(&project.id, source.parent().unwrap())
        .unwrap();
    let asset = catalog.list_assets(&project.id).unwrap().remove(0);
    let first = catalog
        .generate_proxy_with_policy(&asset.id, ProxyEncoderPolicy::Software)
        .unwrap();
    let registered_path = PathBuf::from(first.proxy_path.as_deref().unwrap());
    let registered_bytes = fs::read(&registered_path).unwrap();

    let fake = directory.path().join("always-fail-ffmpeg");
    fs::write(
        &fake,
        "#!/bin/sh\necho 'synthetic hardware and software failure' >&2\nexit 1\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake, permissions).unwrap();
    let plan = ProxyEncoderPlan {
        policy: ProxyEncoderPolicy::Auto,
        preferred_encoder: ProxyEncoderKind::Vaapi,
        targets: vec![
            ProxyEncoderTarget {
                encoder: ProxyEncoderKind::Vaapi,
                device: Some("/dev/dri/renderD128".to_string()),
                recipe_key: PROXY_VAAPI_RECIPE_KEY.to_string(),
            },
            ProxyEncoderTarget {
                encoder: ProxyEncoderKind::Software,
                device: None,
                recipe_key: PROXY_SOFTWARE_RECIPE_KEY.to_string(),
            },
        ],
        hardware_unavailable: None,
        software_usable: true,
        limitations: vec![],
    };
    let error = catalog
        .generate_proxy_untracked_with_execution(
            &asset.id,
            true,
            ProxyEncoderPolicy::Auto,
            |_| {},
            &fake,
            &resolve_tool_path("ffprobe"),
            Some(plan),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("ffmpeg proxy generation failed"));

    let unchanged = catalog.get_asset(&asset.id).unwrap();
    assert_eq!(
        unchanged.proxy_path.as_deref(),
        Some(registered_path.to_string_lossy().as_ref())
    );
    assert_eq!(fs::read(&registered_path).unwrap(), registered_bytes);
    let provenance = catalog.proxy_generation_provenance(&asset.id, 10).unwrap();
    assert_eq!(provenance.len(), 2);
    assert_eq!(provenance[0].status, "failed");
    let registered = catalog
        .registered_proxy_generation_provenance(&asset.id)
        .unwrap()
        .unwrap();
    assert_eq!(registered.id, provenance[1].id);
    assert_eq!(registered.status, "succeeded");
    // A downloaded replacement has no locally generated encoder claim.
    let replacement = catalog.begin_proxy_write(&asset.id).unwrap().unwrap();
    fs::write(replacement.temporary_path(), &registered_bytes).unwrap();
    catalog.commit_proxy_write(replacement, None).unwrap();
    assert!(
        catalog
            .registered_proxy_generation_provenance(&asset.id)
            .unwrap()
            .is_none()
    );

    assert_eq!(provenance[0].attempts.len(), 2);
    assert!(
        provenance[0]
            .attempts
            .iter()
            .all(|attempt| { attempt.status == ProxyAttemptStatus::ProcessFailed })
    );
    assert!(provenance[0].validation.is_none());
    let proxy_directory = registered_path.parent().unwrap();
    assert!(
        fs::read_dir(proxy_directory)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp.mp4"))
    );
}

#[test]
fn queued_proxy_batch_cancels_before_encoder_probe_or_generation() {
    if tool_command("ffmpeg").arg("-version").output().is_err()
        || tool_command("ffprobe").arg("-version").output().is_err()
    {
        return;
    }
    let (directory, catalog, project) = catalog_fixture();
    let source = synthetic_proxy_source(directory.path());
    catalog
        .ingest_folder(&project.id, source.parent().unwrap())
        .unwrap();
    let asset = catalog.list_assets(&project.id).unwrap().remove(0);
    let _held = catalog.acquire_local_heavy().unwrap();
    let worker_catalog = catalog.clone();
    let project_id = project.id.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        sender
            .send(
                worker_catalog.generate_missing_proxies_with_progress(&project_id, |progress| {
                    assert_eq!(progress.estimate.confidence, "waiting_for_resource");
                    ControlFlow::Break(())
                }),
            )
            .unwrap();
    });
    let report = receiver
        .recv_timeout(std::time::Duration::from_secs(3))
        .expect("queued cancellation must remain responsive")
        .unwrap();
    worker.join().unwrap();
    assert!(report.cancelled);
    assert_eq!(report.generated, 0);
    assert!(catalog.get_asset(&asset.id).unwrap().proxy_path.is_none());
    assert!(
        catalog
            .proxy_generation_provenance(&asset.id, 10)
            .unwrap()
            .is_empty()
    );
    assert!(catalog.proxy_rate_histories().unwrap().is_empty());
}

#[test]
fn generated_proxy_commit_failure_restores_file_and_registered_provenance() {
    use std::io::Write;
    if tool_command("ffmpeg").arg("-version").output().is_err()
        || tool_command("ffprobe").arg("-version").output().is_err()
    {
        return;
    }
    let (directory, catalog, project) = catalog_fixture();
    let source = synthetic_proxy_source(directory.path());
    catalog
        .ingest_folder(&project.id, source.parent().unwrap())
        .unwrap();
    let asset = catalog.list_assets(&project.id).unwrap().remove(0);
    let generated = catalog
        .generate_proxy_with_policy(&asset.id, ProxyEncoderPolicy::Software)
        .unwrap();
    let path = PathBuf::from(generated.proxy_path.unwrap());
    // Distinguish the prior valid output from a deterministic regeneration.
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"previous_registered_proxy_marker")
        .unwrap();
    let old_bytes = fs::read(&path).unwrap();
    let old_provenance = catalog
        .registered_proxy_generation_provenance(&asset.id)
        .unwrap()
        .unwrap();
    // Fail at COMMIT, after the staging file has been atomically published.
    catalog
        .connection()
        .unwrap()
        .execute_batch(
            "CREATE TABLE proxy_commit_failure (
                asset_id TEXT REFERENCES assets(id) DEFERRABLE INITIALLY DEFERRED);
             CREATE TRIGGER fail_proxy_commit AFTER INSERT ON proxy_generation_provenance
             BEGIN INSERT INTO proxy_commit_failure VALUES ('missing-asset'); END;",
        )
        .unwrap();
    let error = catalog
        .generate_proxy_with_policy(&asset.id, ProxyEncoderPolicy::Software)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("register generated proxy"),
        "{error:#}"
    );
    assert_eq!(fs::read(&path).unwrap(), old_bytes);
    assert_eq!(
        catalog
            .registered_proxy_generation_provenance(&asset.id)
            .unwrap()
            .unwrap()
            .id,
        old_provenance.id
    );
    assert_eq!(
        catalog
            .proxy_generation_provenance(&asset.id, 20)
            .unwrap()
            .len(),
        1
    );
    assert!(fs::read_dir(path.parent().unwrap()).unwrap().all(|entry| {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        !name.contains("commit-backup") && !name.ends_with(".tmp.mp4")
    }));
}

#[test]
fn proxy_v3_clears_the_source_display_rotation() {
    if tool_command("ffmpeg").arg("-version").output().is_err()
        || tool_command("ffprobe").arg("-version").output().is_err()
    {
        eprintln!("ffmpeg or ffprobe is unavailable; skipping media integration test");
        return;
    }
    let directory = tempdir().unwrap();
    let source_dir = directory.path().join("source");
    fs::create_dir_all(&source_dir).unwrap();
    let unrotated = directory.path().join("unrotated.mp4");
    let source = source_dir.join("ROTATED.MP4");
    let create_status = tool_command("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=navy:s=640x360:r=30:d=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&unrotated)
        .status()
        .unwrap();
    assert!(create_status.success());
    let rotation_status = tool_command("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-display_rotation:v:0",
            "90",
            "-i",
        ])
        .arg(&unrotated)
        .args(["-c", "copy"])
        .arg(&source)
        .status()
        .unwrap();
    assert!(rotation_status.success());
    assert_eq!(
        probe_media_stream(&source)
            .unwrap()
            .metadata_rotation_degrees,
        90
    );

    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Rotated source").unwrap();
    catalog.ingest_folder(&project.id, &source_dir).unwrap();
    let asset = catalog.list_assets(&project.id).unwrap().remove(0);
    let proxied = catalog.generate_proxy(&asset.id).unwrap();
    let proxy_path = PathBuf::from(proxied.proxy_path.as_deref().unwrap());

    assert!(proxy_path.ends_with(format!("{}.v3.mp4", asset.id)));
    assert_eq!(
        proxied.proxy_representation,
        Some(ProxyRepresentation::RawV3)
    );
    let proxy_probe = probe_media_stream(proxy_path).unwrap();
    assert_eq!(proxy_probe.metadata_rotation_degrees, 0);
    assert_eq!((proxy_probe.width, proxy_probe.height), (640, 360));
}

#[test]
fn camera_proxy_discovery_is_card_scoped_and_registration_is_separate() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Synthetic sidecar fixture").unwrap();
    let mut asset_ids = Vec::new();
    let mut sidecars = Vec::new();
    for (card, color) in [("card-a", "red"), ("card-b", "blue")] {
        let root = directory.path().join(card);
        let dcim = root.join("DCIM");
        fs::create_dir_all(&dcim).unwrap();
        let original = dcim.join("CLIP0001.MP4");
        let sidecar = dcim.join("CLIP0001.LRF");
        for (target, size, audio) in [(&original, "640x360", true), (&sidecar, "320x180", true)] {
            let mut cmd = tool_command("ffmpeg");
            cmd.args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("color=c={color}:s={size}:r=24:d=2"),
            ]);
            if audio {
                cmd.args(["-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo"]);
            }
            cmd.args([
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
                "-f",
                "mp4",
            ])
            .arg(target);
            assert!(cmd.status().unwrap().success());
        }
        catalog.ingest_folder(&project.id, &root).unwrap();
        let asset = catalog
            .list_assets(&project.id)
            .unwrap()
            .into_iter()
            .find(|a| a.path == original.to_string_lossy())
            .unwrap();
        asset_ids.push(asset.id);
        sidecars.push((root, sidecar, original));
    }
    assert_ne!(asset_ids[0], asset_ids[1]);
    for i in 0..2 {
        let before = fs::read(&sidecars[i].1).unwrap();
        let original_before = fs::read(&sidecars[i].2).unwrap();
        let found = catalog
            .discover_camera_proxy_sidecars(&project.id, &sidecars[i].0)
            .unwrap();
        assert_eq!(
            found
                .iter()
                .filter(|x| x.asset_id == asset_ids[i] && x.status == "candidate")
                .count(),
            1,
            "{found:?}"
        );
        assert!(found.iter().all(|x| x.asset_id == asset_ids[i]));
        let registered = catalog
            .register_camera_proxy_sidecar(&asset_ids[i], &sidecars[i].0, &sidecars[i].1)
            .unwrap();
        assert_eq!(registered.status, "registered");
        assert_eq!(
            catalog
                .get_asset(&asset_ids[i])
                .unwrap()
                .proxy_representation,
            None
        );
        assert_eq!(
            catalog
                .camera_proxy_sidecar(&asset_ids[i])
                .unwrap()
                .unwrap()
                .status,
            "registered"
        );
        fs::remove_file(&sidecars[i].2).unwrap();
        let offline_asset = catalog.get_asset(&asset_ids[i]).unwrap();
        assert!(offline_asset.camera_proxy_available);
        assert!(offline_asset.available);
        let offline = catalog
            .resolve_camera_proxy_for_review(&asset_ids[i])
            .unwrap()
            .unwrap();
        // Registration records the canonical sidecar path (macOS temp dirs
        // sit behind the `/var` -> `/private/var` symlink).
        assert_eq!(
            offline.path,
            fs::canonicalize(&sidecars[i].1).unwrap().to_string_lossy()
        );
        fs::write(&sidecars[i].2, &original_before).unwrap();
        let mut changed = before.clone();
        changed.push(0);
        fs::write(&sidecars[i].1, &changed).unwrap();
        assert_eq!(
            catalog
                .camera_proxy_sidecar(&asset_ids[i])
                .unwrap()
                .unwrap()
                .status,
            "stale"
        );
        fs::write(&sidecars[i].1, &before[..before.len() / 2]).unwrap();
        assert!(
            catalog
                .register_camera_proxy_sidecar(&asset_ids[i], &sidecars[i].0, &sidecars[i].1)
                .is_err()
        );
        fs::write(&sidecars[i].1, &before).unwrap();
        assert_eq!(fs::read(&sidecars[i].1).unwrap(), before);
        assert_eq!(fs::read(&sidecars[i].2).unwrap(), original_before);
    }
    let wrong_card = catalog
        .discover_camera_proxy_sidecars(&project.id, &sidecars[0].0)
        .unwrap();
    assert!(wrong_card.iter().all(|x| x.asset_id != asset_ids[1]));
}

#[test]
fn media_round_trip_ingest_labels_proxy_notes_and_clock() {
    if tool_command("ffmpeg").arg("-version").output().is_err() {
        eprintln!("ffmpeg is unavailable; skipping media integration test");
        return;
    }
    let directory = tempdir().unwrap();
    let source_dir = directory.path().join("source");
    fs::create_dir_all(&source_dir).unwrap();
    let source = source_dir.join("C0001.MP4");
    let status = tool_command("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=navy:s=640x360:r=30:d=1",
            "-metadata",
            "creation_time=2026-07-18T09:00:00+09:00",
            "-c:v",
            "mpeg4",
        ])
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success());

    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Round trip").unwrap();
    let report = catalog.ingest_folder(&project.id, &source_dir).unwrap();
    assert_eq!(report.imported, 1);
    let ingest_operations = catalog.list_operations(&project.id).unwrap();
    assert_eq!(ingest_operations.len(), 1);
    assert_eq!(
        ingest_operations[0].operation_type,
        operations::PROJECT_ASSET_ADDED
    );
    assert!(ingest_operations[0].payload.get("path").is_none());

    fs::copy(&source, source_dir.join("COPY.MP4")).unwrap();
    fs::write(source_dir.join("README.txt"), b"ignored").unwrap();
    let mut progress = Vec::new();
    let duplicate_report = catalog
        .ingest_folder_with_progress(&project.id, &source_dir, |completed, total, path| {
            progress.push((completed, total, path.to_path_buf()));
        })
        .unwrap();
    assert_eq!(duplicate_report.scanned, 2);
    assert_eq!(duplicate_report.imported, 0);
    assert_eq!(duplicate_report.duplicates, 2);
    assert_eq!(progress.len(), 2);
    assert_eq!(progress.last().map(|item| (item.0, item.1)), Some((2, 2)));

    let asset = catalog.list_assets(&project.id).unwrap().remove(0);
    let updated = catalog
        .update_asset(
            &asset.id,
            &AssetPatch {
                notes: "Opening station view".to_string(),
                labels: vec!["station".to_string(), "opening".to_string()],
                removed: false,
                rotation_degrees: asset.rotation_degrees,
                expected_revision: None,
            },
        )
        .unwrap();
    assert_eq!(updated.notes, "Opening station view");
    assert_eq!(updated.labels, ["station", "opening"]);

    catalog
        .apply_clock_event(
            &project.id,
            &asset.camera,
            &asset.filename,
            -3600,
            "camera was one hour fast",
        )
        .unwrap();
    let corrected = catalog.get_asset(&asset.id).unwrap();
    assert_eq!(corrected.offset_seconds, -3600);

    let mut proxy_progress = Vec::new();
    let first_proxy_report = catalog
        .generate_missing_proxies_with_policy_and_progress(
            &project.id,
            ProxyEncoderPolicy::Software,
            |progress| {
                proxy_progress.push(progress);
                ControlFlow::Continue(())
            },
        )
        .unwrap();
    assert_eq!(first_proxy_report.generated, 1);
    assert!(proxy_progress.len() >= 3);
    assert_eq!(proxy_progress[0].estimate.completed_items, 0);
    assert!(proxy_progress.iter().any(|progress| {
        progress.estimate.completed_items == 0 && progress.estimate.completed_work_units > 0
    }));
    let completed_proxy_progress = proxy_progress.last().unwrap();
    assert_eq!(completed_proxy_progress.estimate.completed_items, 1);
    assert_eq!(completed_proxy_progress.estimate.total_items, 1);
    assert_eq!(completed_proxy_progress.estimate.worker_count, 1);
    assert_eq!(
        completed_proxy_progress
            .estimate
            .estimated_remaining_seconds,
        Some(0.0)
    );
    let first_encoder = ProxyEncoderKind::parse(
        &catalog
            .proxy_generation_provenance(&asset.id, 1)
            .unwrap()
            .remove(0)
            .selected_encoder,
    )
    .unwrap();
    let first_history_key = catalog
        .qualified_history_key(
            &proxy_task_kind_for_encoder(first_encoder, 1),
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            Some(first_encoder.as_str()),
            1,
            0,
            0,
        )
        .unwrap();
    assert_eq!(
        catalog
            .load_batch_history(&first_history_key)
            .unwrap()
            .samples,
        1
    );
    let proxied = completed_proxy_progress.asset.as_ref().unwrap();
    assert!(Path::new(proxied.proxy_path.as_deref().unwrap()).exists());
    assert!(proxied.proxy_path.as_deref().unwrap().ends_with(".v3.mp4"));
    assert_eq!(
        proxied.proxy_representation,
        Some(ProxyRepresentation::RawV3)
    );
    assert_eq!(
        probe_media_stream(proxied.proxy_path.as_deref().unwrap())
            .unwrap()
            .metadata_rotation_degrees,
        0
    );
    let proxy_report = catalog.generate_missing_proxies(&project.id).unwrap();
    assert_eq!(proxy_report.generated, 0);
    assert_eq!(proxy_report.skipped, 1);
    fs::remove_file(proxied.proxy_path.as_deref().unwrap()).unwrap();
    let mut single_proxy_progress = Vec::new();
    let regenerated_proxy = catalog
        .generate_proxy_with_policy_and_progress(
            &asset.id,
            ProxyEncoderPolicy::Software,
            |progress| {
                single_proxy_progress.push(progress);
            },
        )
        .unwrap();
    assert!(single_proxy_progress.len() >= 3);
    assert!(
        single_proxy_progress[0]
            .estimate
            .estimated_remaining_seconds
            .is_some()
    );
    assert_eq!(
        single_proxy_progress
            .last()
            .unwrap()
            .estimate
            .estimated_remaining_seconds,
        Some(0.0)
    );
    assert!(Path::new(regenerated_proxy.proxy_path.as_deref().unwrap()).is_file());
    let second_encoder = ProxyEncoderKind::parse(
        &catalog
            .proxy_generation_provenance(&asset.id, 1)
            .unwrap()
            .remove(0)
            .selected_encoder,
    )
    .unwrap();
    let second_history_key = catalog
        .qualified_history_key(
            &proxy_task_kind_for_encoder(second_encoder, 1),
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            Some(second_encoder.as_str()),
            1,
            0,
            0,
        )
        .unwrap();
    assert_eq!(
        catalog
            .load_batch_history(&second_history_key)
            .unwrap()
            .samples,
        2
    );

    assert!(catalog.generate_thumbnail(&asset.id, 10).is_err());
    let thumbnail = catalog.generate_thumbnail(&asset.id, 1).unwrap();
    let thumbnail_path = PathBuf::from(thumbnail.thumbnail_path.as_deref().unwrap());
    assert!(thumbnail_path.is_file());
    assert_eq!(thumbnail.thumbnail_quality, 1);
    assert!(thumbnail_path.ends_with(format!("{}/q1.v2.jpg", asset.id)));
    fs::remove_file(&thumbnail_path).unwrap();
    fs::rename(&source, source.with_extension("offline")).unwrap();
    let mut thumbnail_progress = Vec::new();
    let regenerated = catalog
        .generate_missing_thumbnails_with_progress(&project.id, 1, |progress| {
            thumbnail_progress.push(progress);
        })
        .unwrap();
    assert_eq!(regenerated.generated, 1);
    assert_eq!(thumbnail_progress.len(), 2);
    assert_eq!(thumbnail_progress[0].estimate.completed_items, 0);
    assert_eq!(thumbnail_progress[1].estimate.completed_items, 1);
    assert_eq!(
        thumbnail_progress[1].estimate.completed_work_units,
        asset.size_bytes
    );
    assert_eq!(
        catalog
            .load_batch_history(THUMBNAIL_TASK_KIND)
            .unwrap()
            .samples,
        1
    );
    let from_proxy = thumbnail_progress[1].asset.as_ref().unwrap();
    assert!(Path::new(from_proxy.thumbnail_path.as_deref().unwrap()).is_file());
    let thumbnail_report = catalog.generate_missing_thumbnails(&project.id, 1).unwrap();
    assert_eq!(thumbnail_report.generated, 0);
    assert_eq!(thumbnail_report.skipped, 1);

    // A targeted import tail must not sweep unrelated old Q0 assets.
    insert_asset(
        &catalog,
        &project.id,
        "asset_old_offline",
        "OLD.MP4",
        "Sony",
        "2026-07-18T10:00:00Z",
        &directory.path().join("offline/OLD.MP4"),
    );
    fs::remove_file(&thumbnail_path).unwrap();
    let targeted = catalog
        .generate_thumbnails_for_assets(&[asset.id.clone(), asset.id.clone()], 1)
        .unwrap();
    assert_eq!(targeted.generated, 1, "duplicate target ids are coalesced");
    assert!(targeted.failures.is_empty());
    assert_eq!(
        catalog
            .get_asset("asset_old_offline")
            .unwrap()
            .thumbnail_quality,
        0,
        "an unrelated offline asset is never swept"
    );

    // Without a current proxy, fallback must verify original identity rather
    // than trusting that the old pathname happens to exist again.
    fs::remove_file(&thumbnail_path).unwrap();
    fs::remove_file(regenerated_proxy.proxy_path.as_deref().unwrap()).unwrap();
    fs::write(&source, b"different footage at a reused card path").unwrap();
    let mismatch = catalog
        .generate_thumbnails_for_assets(std::slice::from_ref(&asset.id), 1)
        .unwrap();
    assert_eq!(mismatch.generated, 0);
    assert_eq!(mismatch.failures.len(), 1);
    assert!(
        mismatch.failures[0]
            .error
            .contains("not this clip any more")
    );
}

struct OperationFakeProvider {
    calls: std::sync::Mutex<Vec<String>>,
    behavior: OperationFakeBehavior,
    prepare_error: Option<ProviderFailureCategory>,
    receipt: VlmReceipt,
}

#[derive(Clone, Copy)]
enum OperationFakeBehavior {
    Valid,
    InvalidBody,
    TransportPrimaryThenValid,
}

impl OperationFakeProvider {
    fn new(behavior: OperationFakeBehavior) -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
            behavior,
            prepare_error: None,
            receipt: VlmReceipt {
                actual_model: Some(DEFAULT_MODEL.into()),
                provider: Some("fake".into()),
                generation_id: Some("fake-generation".into()),
                usage_complete: true,
            },
        }
    }

    fn with_prepare_error(mut self, category: ProviderFailureCategory) -> Self {
        self.prepare_error = Some(category);
        self
    }

    fn with_unknown_identity(mut self) -> Self {
        self.receipt = VlmReceipt {
            actual_model: None,
            provider: None,
            generation_id: None,
            usage_complete: true,
        };
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl VlmProvider for OperationFakeProvider {
    fn prepare(&self) -> Result<()> {
        if let Some(category) = self.prepare_error {
            return Err(match category {
                ProviderFailureCategory::LocalPreUpload => {
                    ProviderFailure::local_pre_upload().into()
                }
                ProviderFailureCategory::Cancelled => ProviderFailure::cancelled().into(),
                _ => ProviderFailure::local_pre_upload().into(),
            });
        }
        Ok(())
    }

    fn analysis_cost_bound(
        &self,
        _model: &str,
        _request: &VlmRequest,
    ) -> Result<AnalysisRequestCostBound> {
        Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
            0.01,
        ))
    }

    fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
        self.calls.lock().unwrap().push(model.to_string());
        match self.behavior {
            OperationFakeBehavior::InvalidBody => Ok(OperationFakeProvider::response(
                model,
                "not json",
                self.receipt.clone(),
            )),
            OperationFakeBehavior::TransportPrimaryThenValid if model == DEFAULT_MODEL => {
                Err(ProviderFailure::transport().into())
            }
            _ => Ok(OperationFakeProvider::response(
                model,
                r#"{"mappings":[{"from":"beach","to":"shore"}]}"#,
                self.receipt.clone(),
            )),
        }
    }
}

impl OperationFakeProvider {
    fn response(model: &str, body: &str, receipt: VlmReceipt) -> VlmResponse {
        VlmResponse {
            model: model.into(),
            body: body.into(),
            prompt_tokens: 11,
            completion_tokens: 3,
            cost_usd: 0.001,
            latency_ms: 7,
            receipt,
        }
    }
}

fn operation_fixture() -> (TempDir, Catalog, Project) {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("operations.sqlite3")).unwrap();
    let project = catalog.create_project("operation accounting").unwrap();
    let first = directory.path().join("first.mov");
    let second = directory.path().join("second.mov");
    fs::write(&first, b"first").unwrap();
    fs::write(&second, b"second").unwrap();
    insert_asset(
        &catalog,
        &project.id,
        "asset_one",
        "first.mov",
        "Sony",
        "",
        &first,
    );
    insert_asset(
        &catalog,
        &project.id,
        "asset_two",
        "second.mov",
        "Sony",
        "",
        &second,
    );
    record_ai_labels(
        &catalog.connection().unwrap(),
        "asset_one",
        &[("beach".into(), "Beach".into())],
    )
    .unwrap();
    record_ai_labels(
        &catalog.connection().unwrap(),
        "asset_two",
        &[("beach".into(), "Beaches".into())],
    )
    .unwrap();
    catalog.add_label("asset_one", "HumanTag").unwrap();
    (directory, catalog, project)
}

#[test]
fn consolidation_domain_fallback_uses_an_independent_ledger_attempt_and_keeps_ai_only_scope() {
    let (_directory, catalog, project) = operation_fixture();
    let provider = OperationFakeProvider {
        behavior: OperationFakeBehavior::Valid,
        calls: std::sync::Mutex::new(Vec::new()),
        prepare_error: None,
        receipt: VlmReceipt {
            actual_model: Some(FALLBACK_MODEL.into()),
            provider: Some("fake".into()),
            generation_id: Some("fallback-generation".into()),
            usage_complete: true,
        },
    };
    // The first call returns syntactically valid JSON that names no mapping,
    // so force a transport fallback explicitly with a chain wrapper fake.
    struct DomainFallbackProvider {
        inner: OperationFakeProvider,
    }
    impl VlmProvider for DomainFallbackProvider {
        fn analysis_cost_bound(
            &self,
            model: &str,
            request: &VlmRequest,
        ) -> Result<AnalysisRequestCostBound> {
            self.inner.analysis_cost_bound(model, request)
        }
        fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
            self.inner.calls.lock().unwrap().push(model.to_string());
            if model == DEFAULT_MODEL {
                Ok(OperationFakeProvider::response(
                    model,
                    r#"{"mappings":[{"from":"unknown-label","to":"x"}]}"#,
                    VlmReceipt {
                        actual_model: Some(DEFAULT_MODEL.into()),
                        provider: Some("fake".into()),
                        generation_id: Some("default-generation".into()),
                        usage_complete: true,
                    },
                ))
            } else {
                Ok(OperationFakeProvider::response(
                    model,
                    r#"{"mappings":[{"from":"beach","to":"shore"}]}"#,
                    VlmReceipt {
                        actual_model: Some(FALLBACK_MODEL.into()),
                        provider: Some("fake".into()),
                        generation_id: Some("fallback-generation".into()),
                        usage_complete: true,
                    },
                ))
            }
        }
    }
    let fallback_provider = DomainFallbackProvider { inner: provider };
    let chain = ProviderChain::with_policy(
        DEFAULT_MODEL,
        &[DEFAULT_MODEL.to_string(), FALLBACK_MODEL.to_string()],
        true,
    )
    .unwrap();
    let report = catalog
        .consolidate_labels_with_provider(&project.id, &fallback_provider, &chain, 1.0)
        .unwrap();
    assert_eq!(fallback_provider.inner.calls().len(), 2);
    assert_eq!(report.cost_usd, 0.002);
    assert!(report.usage_complete);
    assert_eq!(report.actual_model.as_deref(), Some(FALLBACK_MODEL));
    let requests = catalog
        .list_analysis_requests(&project.id, None, 20)
        .unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| request.lifecycle == AnalysisRequestLifecycle::Completed)
    );
    assert_eq!(
        requests
            .iter()
            .map(|request| request.confirmed_cost_usd)
            .sum::<f64>(),
        report.cost_usd
    );
    let asset = catalog.get_asset("asset_one").unwrap();
    assert!(asset.labels.iter().any(|label| label == "shore"));
    assert!(asset.labels.iter().any(|label| label == "HumanTag"));
}

#[test]
fn consolidation_local_prepare_failure_releases_without_fallback_or_receipt() {
    let (_directory, catalog, project) = operation_fixture();
    let provider = OperationFakeProvider::new(OperationFakeBehavior::Valid)
        .with_prepare_error(ProviderFailureCategory::LocalPreUpload);
    let chain = ProviderChain::with_policy(
        DEFAULT_MODEL,
        &[DEFAULT_MODEL.to_string(), FALLBACK_MODEL.to_string()],
        true,
    )
    .unwrap();
    let error = catalog
        .consolidate_labels_with_provider(&project.id, &provider, &chain, 1.0)
        .unwrap_err();
    assert_eq!(
        ProviderFailure::from_anyhow(&error).category,
        ProviderFailureCategory::LocalPreUpload
    );
    assert!(provider.calls().is_empty());
    let requests = catalog
        .list_analysis_requests(&project.id, None, 20)
        .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].lifecycle, AnalysisRequestLifecycle::Released);
    assert_eq!(requests[0].reserved_cost_usd, 0.0);
    assert!(
        catalog
            .analysis_provider_receipts(&requests[0].request_key)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn invalid_paid_consolidation_body_keeps_receipt_and_blocks_implicit_resend() {
    let (_directory, catalog, project) = operation_fixture();
    let provider = OperationFakeProvider::new(OperationFakeBehavior::InvalidBody);
    let chain =
        ProviderChain::with_policy(DEFAULT_MODEL, &[DEFAULT_MODEL.to_string()], false).unwrap();
    assert!(
        catalog
            .consolidate_labels_with_provider(&project.id, &provider, &chain, 1.0)
            .is_err()
    );
    assert_eq!(provider.calls().len(), 1);
    let requests = catalog
        .list_analysis_requests(&project.id, None, 20)
        .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].lifecycle, AnalysisRequestLifecycle::Completed);
    assert_eq!(requests[0].confirmed_cost_usd, 0.001);
    assert_eq!(requests[0].actual_provider.as_deref(), Some("fake"));
    assert_eq!(
        catalog
            .analysis_provider_receipts(&requests[0].request_key)
            .unwrap()
            .len(),
        1
    );

    // A second call with the same exact intent is denied before transport.
    assert!(
        catalog
            .consolidate_labels_with_provider(&project.id, &provider, &chain, 1.0)
            .is_err()
    );
    assert_eq!(provider.calls().len(), 1);
}

#[test]
fn transport_fallback_retains_uncertain_primary_reserve_separately() {
    let (_directory, catalog, project) = operation_fixture();
    let provider = OperationFakeProvider::new(OperationFakeBehavior::TransportPrimaryThenValid);
    let chain = ProviderChain::with_policy(
        DEFAULT_MODEL,
        &[DEFAULT_MODEL.to_string(), FALLBACK_MODEL.to_string()],
        true,
    )
    .unwrap();
    let report = catalog
        .consolidate_labels_with_provider(&project.id, &provider, &chain, 1.0)
        .unwrap();
    assert_eq!(provider.calls().len(), 2);
    assert!(report.uncertain_usage_reserve_usd >= 0.01);
    assert_eq!(report.cost_usd, 0.001);
    assert!(!report.usage_complete);
    let requests = catalog
        .list_analysis_requests(&project.id, None, 20)
        .unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .any(|request| request.lifecycle == AnalysisRequestLifecycle::Uncertain)
    );
    assert!(
        requests
            .iter()
            .any(|request| request.lifecycle == AnalysisRequestLifecycle::Completed)
    );
}

#[test]
fn missing_actual_identity_stays_none_in_consolidation_report() {
    let (_directory, catalog, project) = operation_fixture();
    let provider = OperationFakeProvider::new(OperationFakeBehavior::Valid).with_unknown_identity();
    let chain =
        ProviderChain::with_policy(DEFAULT_MODEL, &[DEFAULT_MODEL.to_string()], false).unwrap();
    let report = catalog
        .consolidate_labels_with_provider(&project.id, &provider, &chain, 1.0)
        .unwrap();
    assert_eq!(report.model, "");
    assert_eq!(report.actual_provider, None);
    assert_eq!(report.actual_model, None);
    assert_eq!(report.provider_generation_id, None);
    let requests = catalog
        .list_analysis_requests(&project.id, None, 20)
        .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].actual_provider, None);
    assert_eq!(requests[0].actual_model, None);
}

#[test]
fn legacy_chronology_cache_cost_is_confirmed_once_and_pending_is_unknown() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("legacy-cache.sqlite3")).unwrap();
    let project = catalog.create_project("legacy cache").unwrap();
    catalog
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO analysis_chronology_suggestions(
                    request_key,project_id,facts_revision,status,result_json,model,attempts,
                    cost_usd,prompt_tokens,completion_tokens,latency_ms,created_at,updated_at)
                 VALUES('old-request',?1,'rev','completed',?2,'old-model',1,0.005,1,1,1,?3,?3)",
            params![
                project.id,
                r#"{"provenance":{"usageComplete":true}}"#,
                now()
            ],
        )
        .unwrap();
    let authority = AnalysisBudgetAuthorityContext {
        project_id: project.id.clone(),
        policy_id: String::new(),
        policy_fingerprint: "legacy-cache-policy".into(),
        operation: CHRONOLOGY_SUGGESTIONS_OPERATION.into(),
        provider: "openrouter".into(),
        budget_scope_id: String::new(),
        budget_usd: 1.0,
    };
    let first = catalog
        .initialize_analysis_accounting_authority(&authority)
        .unwrap();
    assert_eq!(first.legacy_confirmed_cost_usd, 0.005);
    assert!(first.legacy_usage_complete);
    assert!(!first.dispatch_stopped);
    let second = catalog
        .initialize_analysis_accounting_authority(&authority)
        .unwrap();
    assert_eq!(second.legacy_confirmed_cost_usd, 0.005);
    let aggregate = catalog
        .analysis_accounting_aggregate_for_operation_scope(
            &project.id,
            CHRONOLOGY_SUGGESTIONS_OPERATION,
            None,
            None,
        )
        .unwrap();
    assert_eq!(aggregate.legacy_confirmed_cost_usd, 0.005);
    assert_eq!(aggregate.confirmed_cost_usd, 0.005);

    // A separate pending legacy row has no complete receipt evidence and
    // therefore reserves unknown usage and stops further dispatch.
    let pending_project = catalog.create_project("pending cache").unwrap();
    catalog
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO analysis_chronology_suggestions(
                    request_key,project_id,facts_revision,status,result_json,model,attempts,
                    cost_usd,prompt_tokens,completion_tokens,latency_ms,created_at,updated_at)
                 VALUES('pending-request',?1,'rev','running','', '',0,0,0,0,0,?2,?2)",
            params![pending_project.id, now()],
        )
        .unwrap();
    let pending = catalog
        .initialize_analysis_accounting_authority(&AnalysisBudgetAuthorityContext {
            project_id: pending_project.id.clone(),
            policy_id: String::new(),
            policy_fingerprint: "pending-policy".into(),
            operation: CHRONOLOGY_SUGGESTIONS_OPERATION.into(),
            provider: "openrouter".into(),
            budget_scope_id: String::new(),
            budget_usd: 1.0,
        })
        .unwrap();
    assert!(pending.dispatch_stopped);
    assert!(pending.legacy_uncertain_usage_reserve_usd >= 1.0);
    assert!(!pending.legacy_usage_complete);
}

#[test]
fn ledger_linked_chronology_cache_is_not_reclassified_as_untracked_legacy_spend() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("cache-link.sqlite3")).unwrap();
    let project = catalog.create_project("cache link").unwrap();
    let authority = AnalysisBudgetAuthorityContext {
        project_id: project.id.clone(),
        policy_id: String::new(),
        policy_fingerprint: "cache-link-policy".into(),
        operation: CHRONOLOGY_SUGGESTIONS_OPERATION.into(),
        provider: "openrouter".into(),
        budget_scope_id: String::new(),
        budget_usd: 1.0,
    };
    catalog
        .initialize_analysis_accounting_authority(&authority)
        .unwrap();
    let run_id = catalog
        .begin_accounted_analysis_run(&project.id, "", CHRONOLOGY_SUGGESTIONS_OPERATION)
        .unwrap();
    let context = AnalysisAccountingContext {
        project_id: project.id.clone(),
        policy_id: String::new(),
        policy_fingerprint: "cache-link-policy".into(),
        run_id: run_id.clone(),
        operation: CHRONOLOGY_SUGGESTIONS_OPERATION.into(),
        intended_attempt_key: "cache-link-intent".into(),
        source_fingerprint: "cache-link-source".into(),
        provider: "openrouter".into(),
        budget_scope_id: String::new(),
        budget_usd: 1.0,
        explicit_retry: false,
    };
    let bound = AnalysisRequestCostBound::declared_for_synthetic_transport(0.1);
    let request = catalog
        .reserve_analysis_request(&context, DEFAULT_MODEL, "cache-link-payload", &bound)
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&request.request_key)
        .unwrap();
    catalog
        .record_analysis_request_receipt(
            &request.request_key,
            &ProviderReceiptEvidence {
                provider: "fake".into(),
                provider_generation_id: "generation".into(),
                actual_model: DEFAULT_MODEL.into(),
                reported_cost_usd: Some(0.001),
                prompt_tokens: Some(1),
                completion_tokens: Some(1),
                usage_complete: true,
                billed_audio_seconds: None,
                usage_pricing: None,
            },
        )
        .unwrap();
    catalog
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO analysis_chronology_suggestions(
                    request_key,project_id,facts_revision,status,result_json,model,attempts,
                    cost_usd,prompt_tokens,completion_tokens,latency_ms,created_at,updated_at)
                 VALUES('ledger-owned-cache',?1,'rev','completed',?2,'model',1,0.001,1,1,1,?3,?3)",
                params![
                    project.id,
                    serde_json::json!({"provenance":{"usageComplete":true,"runId":run_id,"requestKeys":[request.request_key]}}).to_string(),
                    now()
                ],
            )
            .unwrap();
    let foreign = catalog.create_project("foreign cache owner").unwrap();
    catalog
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO analysis_chronology_suggestions(
                request_key,project_id,facts_revision,status,result_json,created_at,updated_at)
             VALUES('foreign-cache',?1,'rev','completed',?2,?3,?3)",
                params![
                    foreign.id,
                    serde_json::json!({"provenance":{"runId":run_id,"requestKeys":[request.request_key]}})
                        .to_string(),
                    now()
                ],
            )
            .unwrap();
    assert!(
        catalog
            .associate_analysis_request_cache_result(&request.request_key, "foreign-cache")
            .is_err()
    );
    catalog
        .associate_analysis_request_cache_result(&request.request_key, "ledger-owned-cache")
        .unwrap();
    let reopened = catalog
        .initialize_analysis_accounting_authority(&authority)
        .unwrap();
    assert!(
        !reopened.dispatch_stopped,
        "our own ledger-linked cache row must not become untracked legacy spend"
    );
    let other_run = catalog
        .begin_accounted_analysis_run(&project.id, "", CHRONOLOGY_SUGGESTIONS_OPERATION)
        .unwrap();
    let connection = catalog.connection().unwrap();
    let columns = connection
        .prepare("PRAGMA table_info(analysis_request_reservations)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let selection = columns
        .iter()
        .map(|column| match column.as_str() {
            "request_key" => "'bulk-old-' || n".into(),
            "attempt_base_key" => "'bulk-base-' || n".into(),
            "run_id" => "?2".into(),
            "lifecycle" => "'released'".into(),
            "reserved_cost_usd" | "confirmed_cost_usd" | "uncertain_cost_usd" => "0.0".into(),
            "reported_cost_usd"
            | "actual_provider"
            | "actual_model"
            | "provider_generation_id"
            | "dispatch_at" => "NULL".into(),
            "created_at" => "'9999-01-01'".into(),
            _ => column.clone(),
        })
        .collect::<Vec<String>>()
        .join(",");
    connection.execute(&format!(
            "WITH RECURSIVE padding(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM padding WHERE n<1001)
             INSERT INTO analysis_request_reservations({}) SELECT {} FROM analysis_request_reservations,padding WHERE request_key=?1",
            columns.join(","), selection
        ), params![request.request_key, other_run]).unwrap();
    let summary = catalog
        .analysis_operation_accounting_summary(
            &project.id,
            CHRONOLOGY_SUGGESTIONS_OPERATION,
            &run_id,
        )
        .unwrap();
    assert_eq!(summary.request_count, 1);
    assert_eq!(summary.request_keys, vec![request.request_key]);

    assert_eq!(reopened.post_initialization_untracked_evidence_count, 0);
}

#[derive(Clone, Copy)]
enum ChronologyFakeBehavior {
    Valid,
    InvalidBody,
    TransportPrimaryThenValid,
    PanicAfterDispatch,
    SleepThenValid,
}

struct ChronologyFakeProvider {
    calls: Mutex<Vec<String>>,
    behavior: ChronologyFakeBehavior,
    prepare_error: Option<ProviderFailureCategory>,
    receipt: VlmReceipt,
    prepared: Arc<AtomicBool>,
}

impl ChronologyFakeProvider {
    fn new(behavior: ChronologyFakeBehavior) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            behavior,
            prepare_error: None,
            receipt: VlmReceipt {
                actual_model: Some(DEFAULT_MODEL.into()),
                provider: Some("fake".into()),
                generation_id: Some("chronology-generation".into()),
                usage_complete: true,
            },
            prepared: Arc::new(AtomicBool::new(false)),
        }
    }

    fn with_prepare_error(mut self, category: ProviderFailureCategory) -> Self {
        self.prepare_error = Some(category);
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn response(&self, model: &str) -> VlmResponse {
        let mut receipt = self.receipt.clone();
        if receipt.actual_model.is_some() {
            receipt.actual_model = Some(model.to_string());
        }
        if receipt.generation_id.is_some() {
            receipt.generation_id = Some(format!("chronology-generation-{model}"));
        }
        VlmResponse {
            model: model.into(),
            body: r#"{"frameIds":[],"matches":[]}"#.into(),
            prompt_tokens: 11,
            completion_tokens: 3,
            cost_usd: 0.001,
            latency_ms: 7,
            receipt,
        }
    }
}

impl VlmProvider for ChronologyFakeProvider {
    fn prepare(&self) -> Result<()> {
        if let Some(category) = self.prepare_error {
            return Err(match category {
                ProviderFailureCategory::Cancelled => ProviderFailure::cancelled().into(),
                _ => ProviderFailure::local_pre_upload().into(),
            });
        }
        self.prepared.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn analysis_cost_bound(
        &self,
        _model: &str,
        _request: &VlmRequest,
    ) -> Result<AnalysisRequestCostBound> {
        Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
            0.01,
        ))
    }

    fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
        self.calls.lock().unwrap().push(model.to_string());
        match self.behavior {
            ChronologyFakeBehavior::InvalidBody => Ok(VlmResponse {
                model: model.into(),
                body: "not json".into(),
                prompt_tokens: 11,
                completion_tokens: 3,
                cost_usd: 0.001,
                latency_ms: 7,
                receipt: self.receipt.clone(),
            }),
            ChronologyFakeBehavior::TransportPrimaryThenValid if model == DEFAULT_MODEL => {
                Err(ProviderFailure::transport().into())
            }
            ChronologyFakeBehavior::PanicAfterDispatch => {
                panic!("synthetic paid chronology interruption after dispatch marker")
            }
            ChronologyFakeBehavior::SleepThenValid => {
                std::thread::sleep(std::time::Duration::from_millis(750));
                // Charge the full declared bound so exactly one request
                // fits even when the shared preparation slot serializes
                // workers until the earlier response has already settled.
                let mut response = self.response(model);
                response.cost_usd = 0.01;
                Ok(response)
            }
            _ => Ok(self.response(model)),
        }
    }
}

fn synthetic_e2e_clip(directory: &Path, index: usize) -> PathBuf {
    let source_dir = directory.join(format!("source-{index}"));
    fs::create_dir_all(&source_dir).unwrap();
    let source = source_dir.join(format!("CLIP{index}.MP4"));
    let duration = 1.4 + index as f64 * 0.15;
    let mut command = tool_command("ffmpeg");
    command.args([
        "-y",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
    ]);
    command.arg(format!("testsrc2=size=320x180:rate=24:duration={duration}"));
    command.args(["-f", "lavfi", "-i"]);
    command.arg(format!(
        "sine=frequency={}:duration={duration}",
        440 + index * 37
    ));
    command
        .args([
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(&source);
    let status = command.status().unwrap();
    assert!(
        status.success(),
        "could not create synthetic chronology clip"
    );
    source
}

fn chronology_operation_fixture(
    clip_count: usize,
) -> Option<(TempDir, Catalog, Project, Vec<String>, PathBuf)> {
    if tool_command("ffmpeg").arg("-version").output().is_err()
        || tool_command("ffprobe").arg("-version").output().is_err()
    {
        eprintln!("ffmpeg or ffprobe unavailable; skipping chronology Catalog E2E test");
        return None;
    }
    let directory = tempdir().unwrap();
    let catalog_path = directory.path().join("chronology-e2e.sqlite3");
    let catalog = Catalog::open(&catalog_path).unwrap();
    let project = catalog.create_project("chronology accounting e2e").unwrap();
    let mut asset_ids = Vec::new();
    for index in 0..clip_count {
        let source = synthetic_e2e_clip(directory.path(), index);
        let source_dir = source.parent().unwrap();
        catalog.ingest_folder(&project.id, source_dir).unwrap();
        let filename = source.file_name().unwrap().to_string_lossy().to_string();
        let asset = catalog
            .list_assets(&project.id)
            .unwrap()
            .into_iter()
            .find(|asset| asset.filename == filename)
            .unwrap();
        let proxied = catalog
            .generate_proxy_with_policy(&asset.id, ProxyEncoderPolicy::Auto)
            .unwrap();
        assert!(
            proxied
                .proxy_path
                .as_deref()
                .is_some_and(|path| Path::new(path).is_file()),
            "chronology E2E asset must have a generated proxy"
        );
        asset_ids.push(asset.id);
    }
    Some((directory, catalog, project, asset_ids, catalog_path))
}

fn default_chronology_chain() -> ProviderChain {
    ProviderChain::with_policy(DEFAULT_MODEL, &[DEFAULT_MODEL.to_string()], false).unwrap()
}

fn fallback_chronology_chain() -> ProviderChain {
    ProviderChain::with_policy(
        DEFAULT_MODEL,
        &[DEFAULT_MODEL.to_string(), FALLBACK_MODEL.to_string()],
        true,
    )
    .unwrap()
}

#[test]
fn chronology_catalog_success_caches_canonical_provenance_and_reuses_without_paid_call() {
    let Some((_directory, catalog, project, asset_ids, catalog_path)) =
        chronology_operation_fixture(2)
    else {
        return;
    };
    let provider = ChronologyFakeProvider::new(ChronologyFakeBehavior::Valid);
    let chain = default_chronology_chain();
    let first = catalog
        .suggest_chronology_with_provider_and_budget(
            &project.id,
            &asset_ids,
            &provider,
            &chain,
            1.0,
        )
        .unwrap();
    assert_eq!(provider.calls().len(), 1);
    assert!(!first.cached);
    assert_eq!(first.provenance.cost_usd, 0.001);
    assert_eq!(first.provenance.uncertain_usage_reserve_usd, 0.0);
    assert!(first.provenance.usage_complete);
    assert_eq!(first.provenance.actual_provider.as_deref(), Some("fake"));
    assert_eq!(
        first.provenance.actual_model.as_deref(),
        Some(DEFAULT_MODEL)
    );
    assert_eq!(first.provenance.request_keys.len(), 1);
    assert!(!first.provenance.run_id.is_empty());
    let first_provenance = first.provenance.clone();

    let requests = catalog
        .list_analysis_requests(&project.id, None, 20)
        .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].lifecycle, AnalysisRequestLifecycle::Completed);
    assert_eq!(requests[0].confirmed_cost_usd, 0.001);
    assert_eq!(
        catalog
            .analysis_provider_receipts(&requests[0].request_key)
            .unwrap()
            .len(),
        1
    );

    drop(catalog);
    let reopened = Catalog::open(&catalog_path).unwrap();
    let cached_provider = ChronologyFakeProvider::new(ChronologyFakeBehavior::Valid);
    let cached = reopened
        .suggest_chronology_with_provider_and_budget(
            &project.id,
            &asset_ids,
            &cached_provider,
            &chain,
            1.0,
        )
        .unwrap();
    assert!(cached.cached);
    assert!(cached_provider.calls().is_empty());
    assert_eq!(cached.provenance, first_provenance);
    assert_eq!(
        reopened
            .list_analysis_requests(&project.id, None, 20)
            .unwrap()
            .len(),
        1,
        "cache reuse must not create another paid request"
    );
}

#[test]
fn chronology_catalog_transport_fallback_retains_both_attempts_and_whole_chain_cache() {
    let Some((_directory, catalog, project, asset_ids, catalog_path)) =
        chronology_operation_fixture(2)
    else {
        return;
    };
    let provider = ChronologyFakeProvider::new(ChronologyFakeBehavior::TransportPrimaryThenValid);
    let chain = fallback_chronology_chain();
    let first = catalog
        .suggest_chronology_with_provider_and_budget(
            &project.id,
            &asset_ids,
            &provider,
            &chain,
            1.0,
        )
        .unwrap();
    assert_eq!(provider.calls().len(), 2);
    assert_eq!(first.provenance.cost_usd, 0.001);
    assert!(first.provenance.uncertain_usage_reserve_usd >= 0.01);
    assert!(!first.provenance.usage_complete);
    assert_eq!(first.provenance.request_keys.len(), 2);
    let requests = catalog
        .list_analysis_requests(&project.id, None, 20)
        .unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .any(|request| request.lifecycle == AnalysisRequestLifecycle::Uncertain)
    );
    assert!(
        requests
            .iter()
            .any(|request| request.lifecycle == AnalysisRequestLifecycle::Completed)
    );
    let cache_links: i64 = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM analysis_request_events WHERE event_kind='cache_result'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cache_links, 2);

    drop(catalog);
    let reopened = Catalog::open(&catalog_path).unwrap();
    let cached_provider = ChronologyFakeProvider::new(ChronologyFakeBehavior::Valid);
    let cached = reopened
        .suggest_chronology_with_provider_and_budget(
            &project.id,
            &asset_ids,
            &cached_provider,
            &chain,
            1.0,
        )
        .unwrap();
    assert!(cached.cached);
    assert!(cached_provider.calls().is_empty());
    assert_eq!(cached.provenance, first.provenance);
}

#[test]
fn chronology_catalog_invalid_paid_body_keeps_receipt_fails_run_and_blocks_resend() {
    let Some((_directory, catalog, project, asset_ids, _catalog_path)) =
        chronology_operation_fixture(2)
    else {
        return;
    };
    let provider = ChronologyFakeProvider::new(ChronologyFakeBehavior::InvalidBody);
    let chain = default_chronology_chain();
    assert!(
        catalog
            .suggest_chronology_with_provider_and_budget(
                &project.id,
                &asset_ids,
                &provider,
                &chain,
                1.0,
            )
            .is_err()
    );
    assert_eq!(provider.calls().len(), 1);
    let requests = catalog
        .list_analysis_requests(&project.id, None, 20)
        .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].lifecycle, AnalysisRequestLifecycle::Completed);
    assert_eq!(requests[0].confirmed_cost_usd, 0.001);
    assert_eq!(
        catalog
            .analysis_provider_receipts(&requests[0].request_key)
            .unwrap()
            .len(),
        1
    );
    let first_run = catalog.analysis_runs(&project.id, 1).unwrap().remove(0);
    assert_eq!(first_run.status, "failed");
    assert_eq!(first_run.cost_usd, 0.001);
    assert_eq!(first_run.error, "provider_error:unclassified");

    assert!(
        catalog
            .suggest_chronology_with_provider_and_budget(
                &project.id,
                &asset_ids,
                &provider,
                &chain,
                1.0,
            )
            .is_err()
    );
    assert_eq!(
        provider.calls().len(),
        1,
        "a completed receipt must block an implicit retry"
    );
}

#[test]
fn chronology_catalog_local_release_and_dispatched_crash_never_resend() {
    let Some((_directory, catalog, project, asset_ids, _catalog_path)) =
        chronology_operation_fixture(4)
    else {
        return;
    };
    let chain = default_chronology_chain();
    let local_provider = ChronologyFakeProvider::new(ChronologyFakeBehavior::Valid)
        .with_prepare_error(ProviderFailureCategory::LocalPreUpload);
    assert!(
        catalog
            .suggest_chronology_with_provider_and_budget(
                &project.id,
                &asset_ids[0..2],
                &local_provider,
                &chain,
                1.0,
            )
            .is_err()
    );
    assert!(local_provider.calls().is_empty());
    let released = catalog
        .list_analysis_requests(&project.id, None, 20)
        .unwrap();
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].lifecycle, AnalysisRequestLifecycle::Released);
    let scope = analysis_operation_budget_scope_id(&project.id, CHRONOLOGY_SUGGESTIONS_OPERATION);
    let snapshot = catalog.analysis_budget_snapshot(&scope).unwrap().unwrap();
    assert!(
        !snapshot.dispatch_stopped,
        "a proven local pre-upload failure must not poison the authority"
    );

    let crashing = ChronologyFakeProvider::new(ChronologyFakeBehavior::PanicAfterDispatch);
    let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        catalog.suggest_chronology_with_provider_and_budget(
            &project.id,
            &asset_ids[2..4],
            &crashing,
            &chain,
            1.0,
        )
    }));
    assert!(interrupted.is_err());
    assert_eq!(crashing.calls().len(), 1);
    let dispatches = catalog
        .list_analysis_requests(&project.id, None, 20)
        .unwrap();
    let dispatched = dispatches
        .iter()
        .find(|request| request.requested_model == DEFAULT_MODEL && request.dispatch_at.is_some())
        .unwrap();
    assert_eq!(dispatched.lifecycle, AnalysisRequestLifecycle::Dispatched);
    assert!(
        catalog
            .analysis_provider_receipts(&dispatched.request_key)
            .unwrap()
            .is_empty()
    );

    assert!(
        catalog
            .suggest_chronology_with_provider_and_budget(
                &project.id,
                &asset_ids[2..4],
                &crashing,
                &chain,
                1.0,
            )
            .is_err()
    );
    assert_eq!(
        crashing.calls().len(),
        1,
        "an interrupted dispatched request must fence an implicit resend"
    );
}

#[test]
fn chronology_catalog_legacy_pending_cache_remains_unknown_and_blocks_dispatch() {
    let Some((_directory, catalog, project, asset_ids, _catalog_path)) =
        chronology_operation_fixture(2)
    else {
        return;
    };
    catalog
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO analysis_chronology_suggestions(
                    request_key,project_id,facts_revision,status,result_json,model,attempts,
                    cost_usd,prompt_tokens,completion_tokens,latency_ms,created_at,updated_at)
                 VALUES('legacy-pending',?1,'rev','running','', '',0,0,0,0,0,?2,?2)",
            params![project.id, now()],
        )
        .unwrap();
    let provider = ChronologyFakeProvider::new(ChronologyFakeBehavior::Valid);
    let chain = default_chronology_chain();
    assert!(
        catalog
            .suggest_chronology_with_provider_and_budget(
                &project.id,
                &asset_ids,
                &provider,
                &chain,
                1.0,
            )
            .is_err()
    );
    assert!(provider.calls().is_empty());
    let (status, result_json): (String, String) = catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT status,result_json FROM analysis_chronology_suggestions WHERE request_key='legacy-pending'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
    assert_eq!(status, "running");
    assert!(result_json.is_empty());
    assert!(
        catalog
            .list_analysis_requests(&project.id, None, 20)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn chronology_catalog_final_budget_race_across_independent_catalogs() {
    let Some((_directory, catalog, project, asset_ids, catalog_path)) =
        chronology_operation_fixture(4)
    else {
        return;
    };
    drop(catalog);
    let provider = Arc::new(ChronologyFakeProvider::new(
        ChronologyFakeBehavior::SleepThenValid,
    ));
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    for selected in [asset_ids[0..2].to_vec(), asset_ids[2..4].to_vec()] {
        let provider = Arc::clone(&provider);
        let barrier = Arc::clone(&barrier);
        let catalog_path = catalog_path.clone();
        let project_id = project.id.clone();
        workers.push(std::thread::spawn(move || {
            let catalog = Catalog::open(&catalog_path).unwrap();
            let chain = default_chronology_chain();
            barrier.wait();
            catalog
                .suggest_chronology_with_provider_and_budget(
                    &project_id,
                    &selected,
                    provider.as_ref(),
                    &chain,
                    0.015,
                )
                .map(|_| ())
        }));
    }
    barrier.wait();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        results.iter().filter(|result| result.is_ok()).count(),
        1,
        "concurrent catalogs must serialize the final conservative bound"
    );
    let error = results
        .iter()
        .find_map(|result| result.as_ref().err())
        .unwrap();
    assert!(
        error.to_string().contains("budget_exhausted"),
        "unexpected losing-race error: {error:#}"
    );
    assert_eq!(
        provider.calls().len(),
        1,
        "the losing worker must be denied before transport"
    );
}

struct LabelSourceMutatingProvider {
    database: PathBuf,
    during_prepare: bool,
    calls: Mutex<usize>,
}

impl LabelSourceMutatingProvider {
    fn new(database: PathBuf, during_prepare: bool) -> Self {
        Self {
            database,
            during_prepare,
            calls: Mutex::new(0),
        }
    }

    fn calls(&self) -> usize {
        *self.calls.lock().unwrap()
    }

    fn mutate_ai_label_source(&self) {
        let catalog = Catalog::open(&self.database).unwrap();
        record_ai_labels(
            &catalog.connection().unwrap(),
            "asset_two",
            &[("concurrent".into(), "Concurrent".into())],
        )
        .unwrap();
    }

    fn valid_response(&self, model: &str) -> VlmResponse {
        VlmResponse {
            model: model.into(),
            body: r#"{"mappings":[{"from":"beach","to":"shore"}]}"#.into(),
            prompt_tokens: 11,
            completion_tokens: 3,
            cost_usd: 0.001,
            latency_ms: 7,
            receipt: VlmReceipt {
                actual_model: Some(model.into()),
                provider: Some("fake".into()),
                generation_id: Some("mutating-generation".into()),
                usage_complete: true,
            },
        }
    }
}

impl VlmProvider for LabelSourceMutatingProvider {
    fn prepare(&self) -> Result<()> {
        if self.during_prepare {
            self.mutate_ai_label_source();
        }
        Ok(())
    }

    fn analysis_cost_bound(
        &self,
        _model: &str,
        _request: &VlmRequest,
    ) -> Result<AnalysisRequestCostBound> {
        Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
            0.01,
        ))
    }

    fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
        *self.calls.lock().unwrap() += 1;
        if !self.during_prepare {
            self.mutate_ai_label_source();
        }
        Ok(self.valid_response(model))
    }
}

struct CancelDuringPrepareProvider {
    prepared: Arc<AtomicBool>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl VlmProvider for CancelDuringPrepareProvider {
    fn prepare(&self) -> Result<()> {
        self.prepared.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn analysis_cost_bound(
        &self,
        _model: &str,
        _request: &VlmRequest,
    ) -> Result<AnalysisRequestCostBound> {
        Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
            0.01,
        ))
    }

    fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(VlmResponse {
            model: model.into(),
            body: r#"{"mappings":[{"from":"beach","to":"shore"}]}"#.into(),
            prompt_tokens: 11,
            completion_tokens: 3,
            cost_usd: 0.001,
            latency_ms: 7,
            receipt: VlmReceipt {
                actual_model: Some(model.into()),
                provider: Some("fake".into()),
                generation_id: Some("cancelled-generation".into()),
                usage_complete: true,
            },
        })
    }
}

#[test]
fn consolidation_source_change_after_paid_response_keeps_receipt_and_closes_failed_run() {
    let (_directory, catalog, project) = operation_fixture();
    let provider = LabelSourceMutatingProvider::new(catalog.database_path().to_path_buf(), false);
    let chain = default_chronology_chain();
    let error = catalog
        .consolidate_labels_with_provider(&project.id, &provider, &chain, 1.0)
        .unwrap_err();
    assert!(provider.calls() == 1, "unexpected error: {error:#}");
    let requests = catalog
        .list_analysis_requests(&project.id, None, 20)
        .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].lifecycle, AnalysisRequestLifecycle::Completed);
    assert_eq!(requests[0].confirmed_cost_usd, 0.001);
    assert_eq!(
        catalog
            .analysis_provider_receipts(&requests[0].request_key)
            .unwrap()
            .len(),
        1
    );
    let run = catalog.analysis_runs(&project.id, 1).unwrap().remove(0);
    assert_eq!(run.status, "failed");
    assert_eq!(run.cost_usd, 0.001);
    assert_eq!(run.error, "provider_error:unclassified");
    let asset = catalog.get_asset("asset_one").unwrap();
    assert!(!asset.labels.iter().any(|label| label == "shore"));
    assert!(asset.labels.iter().any(|label| label == "HumanTag"));
}

#[test]
fn consolidation_prepare_source_mutation_is_denied_before_dispatch_marker() {
    let (_directory, catalog, project) = operation_fixture();
    let provider = LabelSourceMutatingProvider::new(catalog.database_path().to_path_buf(), true);
    let chain = default_chronology_chain();
    assert!(
        catalog
            .consolidate_labels_with_provider(&project.id, &provider, &chain, 1.0)
            .is_err()
    );
    assert_eq!(provider.calls(), 0);
    let requests = catalog
        .list_analysis_requests(&project.id, None, 20)
        .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].lifecycle, AnalysisRequestLifecycle::Released);
    assert!(requests[0].dispatch_at.is_none());
    assert!(
        catalog
            .analysis_provider_receipts(&requests[0].request_key)
            .unwrap()
            .is_empty()
    );
    let run = catalog.analysis_runs(&project.id, 1).unwrap().remove(0);
    assert_eq!(run.status, "failed");
    assert_eq!(run.cost_usd, 0.0);
    let scope = analysis_operation_budget_scope_id(&project.id, LABEL_CONSOLIDATION_OPERATION);
    assert!(
        !catalog
            .analysis_budget_snapshot(&scope)
            .unwrap()
            .unwrap()
            .dispatch_stopped
    );
    let asset = catalog.get_asset("asset_one").unwrap();
    assert!(!asset.labels.iter().any(|label| label == "shore"));
    assert!(asset.labels.iter().any(|label| label == "HumanTag"));
}

#[test]
fn consolidation_cancellation_before_dispatch_makes_zero_actual_calls() {
    let (_directory, catalog, project) = operation_fixture();
    let provider = CancelDuringPrepareProvider {
        prepared: Arc::new(AtomicBool::new(false)),
        calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    };
    let chain = fallback_chronology_chain();
    let prepared_for_cancel = Arc::clone(&provider.prepared);
    let error = catalog
        .consolidate_labels_with_provider_and_cancel(
            &project.id,
            &provider,
            &chain,
            1.0,
            &mut || prepared_for_cancel.load(Ordering::SeqCst),
        )
        .unwrap_err();
    assert!(error.to_string().contains("cancelled") || error.to_string().contains("cancel"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(chain.default_failures(), 0);
    let requests = catalog
        .list_analysis_requests(&project.id, None, 20)
        .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].lifecycle, AnalysisRequestLifecycle::Released);
    assert!(
        catalog
            .analysis_provider_receipts(&requests[0].request_key)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn consolidation_cancellation_after_invalid_primary_prevents_domain_fallback_call() {
    let (_directory, catalog, project) = operation_fixture();
    let provider = OperationFakeProvider::new(OperationFakeBehavior::InvalidBody);
    let chain = fallback_chronology_chain();
    assert!(
        catalog
            .consolidate_labels_with_provider_and_cancel(
                &project.id,
                &provider,
                &chain,
                1.0,
                &mut || !provider.calls().is_empty(),
            )
            .is_err()
    );
    assert_eq!(provider.calls().len(), 1);
    assert_eq!(chain.default_failures(), 1);
    let requests = catalog
        .list_analysis_requests(&project.id, None, 20)
        .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].lifecycle, AnalysisRequestLifecycle::Completed);
    assert_eq!(requests[0].confirmed_cost_usd, 0.001);
    let run = catalog.analysis_runs(&project.id, 1).unwrap().remove(0);
    assert_eq!(run.status, "failed");
    assert_eq!(run.cost_usd, 0.001);
}

#[test]
fn chronology_completed_cache_and_links_roll_back_together_on_association_failure() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("atomic-cache.sqlite3")).unwrap();
    let project = catalog.create_project("atomic cache").unwrap();
    let authority = AnalysisBudgetAuthorityContext {
        project_id: project.id.clone(),
        policy_id: String::new(),
        policy_fingerprint: "atomic-cache-policy".into(),
        operation: CHRONOLOGY_SUGGESTIONS_OPERATION.into(),
        provider: "openrouter".into(),
        budget_scope_id: String::new(),
        budget_usd: 1.0,
    };
    catalog
        .initialize_analysis_accounting_authority(&authority)
        .unwrap();
    let run_id = catalog
        .begin_accounted_analysis_run(&project.id, "", CHRONOLOGY_SUGGESTIONS_OPERATION)
        .unwrap();
    let context = AnalysisAccountingContext {
        project_id: project.id.clone(),
        policy_id: String::new(),
        policy_fingerprint: "atomic-cache-policy".into(),
        run_id: run_id.clone(),
        operation: CHRONOLOGY_SUGGESTIONS_OPERATION.into(),
        intended_attempt_key: "atomic-cache-intent".into(),
        source_fingerprint: "atomic-cache-source".into(),
        provider: "openrouter".into(),
        budget_scope_id: String::new(),
        budget_usd: 1.0,
        explicit_retry: false,
    };
    let reservation = catalog
        .reserve_analysis_request(
            &context,
            DEFAULT_MODEL,
            "atomic-cache-payload",
            &AnalysisRequestCostBound::declared_for_synthetic_transport(0.01),
        )
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&reservation.request_key)
        .unwrap();
    catalog
        .record_analysis_request_receipt(
            &reservation.request_key,
            &ProviderReceiptEvidence {
                provider: "fake".into(),
                provider_generation_id: "atomic-cache-generation".into(),
                actual_model: DEFAULT_MODEL.into(),
                reported_cost_usd: Some(0.001),
                prompt_tokens: Some(1),
                completion_tokens: Some(1),
                usage_complete: true,
                billed_audio_seconds: None,
                usage_pricing: None,
            },
        )
        .unwrap();
    let result: SuggestionResult = serde_json::from_value(serde_json::json!({
        "suggestion": {"frameIds": [], "matches": []},
        "review": {
            "missingFrameIds": [],
            "duplicateFrameIds": [],
            "unknownFrameIds": [],
            "semanticInconsistencies": []
        },
        "provenance": {
            "model": DEFAULT_MODEL,
            "attempts": 1,
            "attemptedModels": [DEFAULT_MODEL],
            "usageComplete": true,
            "costUsd": 0.001,
            "uncertainUsageReserveUsd": 0.0,
            "activeReservedCostUsd": 0.0,
            "committedOrReservedUsd": 0.001,
            "actualProvider": "fake",
            "actualModel": DEFAULT_MODEL,
            "providerGenerationId": "atomic-cache-generation",
            "runId": run_id,
            "requestKeys": [reservation.request_key.clone()],
            "promptTokens": 1,
            "completionTokens": 1,
            "latencyMs": 1
        },
        "cached": false
    }))
    .unwrap();
    let mut connection = catalog.connection().unwrap();
    let missing_request = "missing-ledger-request".to_string();
    assert!(
        analysis::chronology_suggestions::store_completed_with_associations(
            &mut connection,
            "atomic-cache-result",
            "atomic-facts-revision",
            &result,
            &[reservation.request_key.clone(), missing_request],
        )
        .is_err()
    );
    let cache_rows: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM analysis_chronology_suggestions WHERE request_key='atomic-cache-result'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(cache_rows, 0);
    let link_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM analysis_request_events
                 WHERE event_kind='cache_result'
                   AND json_extract(detail_json,'$.cacheKey')='atomic-cache-result'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(link_rows, 0);
    assert_eq!(
        catalog
            .analysis_provider_receipts(&reservation.request_key)
            .unwrap()
            .len(),
        1,
        "the paid receipt is external evidence and survives cache/link rollback"
    );
}
#[test]
fn chronology_cancelled_while_heavy_slot_is_busy_creates_no_run_or_paid_request() {
    let Some((_directory, catalog, project, assets, _)) = chronology_operation_fixture(2) else {
        return;
    };
    let provider = ChronologyFakeProvider::new(ChronologyFakeBehavior::Valid);
    let lease = catalog.acquire_local_heavy().unwrap();
    let result = catalog.suggest_chronology_with_provider_and_budget_and_cancel(
        &project.id,
        &assets,
        &provider,
        &default_chronology_chain(),
        1.0,
        &mut || true,
    );
    assert!(result.is_err());
    assert!(provider.calls().is_empty());
    assert!(catalog.analysis_runs(&project.id, 10).unwrap().is_empty());
    assert!(
        catalog
            .list_analysis_requests(&project.id, None, 10)
            .unwrap()
            .is_empty()
    );
    drop(lease);
}
