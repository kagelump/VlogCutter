use super::*;
use crate::tests::insert_asset_at;
use std::fs;
use tempfile::tempdir;

fn sequence_hint(
    relative_path: &str,
    camera_key: &str,
    volume_hint: &str,
    folder: &str,
    prefix: &str,
    counter: Option<u64>,
    observed_seconds: Option<u64>,
) -> ImportSequenceCandidate {
    ImportSequenceCandidate {
        relative_path: relative_path.to_string(),
        camera_key: camera_key.to_string(),
        volume_hint: volume_hint.to_string(),
        folder: folder.to_string(),
        prefix: prefix.to_string(),
        counter,
        observed_millis: observed_seconds.map(|seconds| seconds * 1_000),
    }
}

fn proposal_evidence(
    candidates: Vec<ImportSequenceCandidate>,
) -> Vec<ImportSequenceBoundaryEvidence> {
    let (_, _, boundaries) = propose_import_runs(candidates);
    assert_eq!(boundaries.len(), 1);
    boundaries.into_iter().next().unwrap().evidence
}

#[test]
fn sequence_detector_covers_every_frozen_boundary_kind() {
    let volume = proposal_evidence(vec![
        sequence_hint("A/C0001.MP4", "Sony", "card-a", "A", "C", Some(1), Some(1)),
        sequence_hint("B/C0002.MP4", "Sony", "card-b", "B", "C", Some(2), Some(2)),
    ]);
    assert!(matches!(
        volume.as_slice(),
        [ImportSequenceBoundaryEvidence::VolumeChange { .. }]
    ));

    let counter = proposal_evidence(vec![
        sequence_hint("A/C0241.MP4", "Sony", "card", "A", "C", Some(241), Some(1)),
        sequence_hint("A/C0001.MP4", "Sony", "card", "A", "C", Some(1), Some(2)),
    ]);
    assert!(counter.iter().any(|found| matches!(
        found,
        ImportSequenceBoundaryEvidence::CounterReset {
            previous: 241,
            next: 1
        }
    )));

    let folder = proposal_evidence(vec![
        sequence_hint("A/C0042.MP4", "Sony", "card", "A", "C", Some(42), Some(1)),
        sequence_hint("B/C0001.MP4", "Sony", "card", "B", "C", Some(1), Some(2)),
    ]);
    assert!(folder.iter().any(|found| matches!(
        found,
        ImportSequenceBoundaryEvidence::FolderReset { from, to }
            if from == "A" && to == "B"
    )));

    let prefix = proposal_evidence(vec![
        sequence_hint("A/C0001.MP4", "Camera", "card", "A", "C", Some(1), Some(1)),
        sequence_hint(
            "A/DJI_0002.MP4",
            "Camera",
            "card",
            "A",
            "DJI",
            Some(2),
            Some(2),
        ),
    ]);
    assert!(matches!(
        prefix.as_slice(),
        [ImportSequenceBoundaryEvidence::PrefixChange { .. }]
    ));

    let discontinuity = proposal_evidence(vec![
        sequence_hint("A/C0001.MP4", "Sony", "card", "A", "C", Some(1), Some(1)),
        sequence_hint(
            "A/C0002.MP4",
            "Sony",
            "card",
            "A",
            "C",
            Some(2),
            Some(1 + LARGE_CAPTURE_SEQUENCE_GAP_SECONDS),
        ),
    ]);
    assert!(matches!(
        discontinuity.as_slice(),
        [ImportSequenceBoundaryEvidence::LargeDiscontinuity { gap_seconds }]
            if *gap_seconds == LARGE_CAPTURE_SEQUENCE_GAP_SECONDS
    ));
}

#[test]
fn sequence_proposals_are_deterministic_and_count_the_affected_run() {
    let candidates = vec![
        sequence_hint("A/C0001.MP4", "Sony", "card", "A", "C", Some(1), Some(1)),
        sequence_hint("A/C0002.MP4", "Sony", "card", "A", "C", Some(2), Some(2)),
        sequence_hint("B/C0001.MP4", "Sony", "card", "B", "C", Some(1), Some(3)),
        sequence_hint("B/C0002.MP4", "Sony", "card", "B", "C", Some(2), Some(4)),
    ];
    let mut reversed = candidates.clone();
    reversed.reverse();
    let (ordered, runs, boundaries) = propose_import_runs(candidates);
    let (reordered, reruns, reboundaries) = propose_import_runs(reversed);
    assert_eq!(ordered, reordered);
    assert_eq!(runs, reruns);
    assert_eq!(boundaries, reboundaries);
    assert_eq!(runs.len(), 2);
    assert_eq!(boundaries[0].affected_clip_count, 2);
    assert_eq!(boundaries[0].after_relative_path, "B/C0001.MP4");
}

#[test]
fn import_preflight_surfaces_folder_and_counter_boundary_evidence() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = directory.path().join("SONY_SD");
    let first_folder = mount.join("DCIM/100MSDCF");
    let second_folder = mount.join("DCIM/101MSDCF");
    fs::create_dir_all(&first_folder).unwrap();
    fs::create_dir_all(&second_folder).unwrap();
    let first = first_folder.join("C0241.MP4");
    let second = second_folder.join("C0001.MP4");
    fs::write(&first, b"first card run").unwrap();
    fs::write(&second, b"second card run").unwrap();
    let base = UNIX_EPOCH + std::time::Duration::from_secs(100);
    fs::File::open(&first)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(base))
        .unwrap();
    fs::File::open(&second)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(base + std::time::Duration::from_secs(1)))
        .unwrap();

    let plan = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();

    assert_eq!(plan.sequence_preview.new_clip_count, 2);
    assert_eq!(plan.sequence_preview.proposed_sequence_count, 2);
    assert_eq!(plan.sequence_preview.suggested_new_sequence_count, 2);
    assert_eq!(plan.sequence_preview.boundaries.len(), 1);
    let boundary = &plan.sequence_preview.boundaries[0];
    assert_eq!(boundary.after_relative_path, "DCIM/101MSDCF/C0001.MP4");
    assert_eq!(boundary.affected_clip_count, 1);
    assert!(boundary.evidence.iter().any(|found| matches!(
        found,
        ImportSequenceBoundaryEvidence::CounterReset {
            previous: 241,
            next: 1
        }
    )));
    assert!(
        boundary
            .evidence
            .iter()
            .any(|found| matches!(found, ImportSequenceBoundaryEvidence::FolderReset { .. }))
    );
    assert_eq!(plan.sequence_preview.runs.len(), 2);
    assert_eq!(
        plan.sequence_preview.runs[1].decision,
        ImportSequenceRunDecision::ProposeNew
    );
    assert_eq!(plan.sequence_preview.runs[1].clip_count, 1);
    assert_eq!(
        plan.sequence_preview.runs[1].first_relative_path,
        "DCIM/101MSDCF/C0001.MP4"
    );
    assert_eq!(
        plan.sequence_preview.runs[1].last_relative_path,
        "DCIM/101MSDCF/C0001.MP4"
    );
    assert!(
        plan.sequence_preview.runs[1]
            .evidence
            .iter()
            .any(|found| matches!(found, ImportSequenceBoundaryEvidence::CounterReset { .. }))
    );
    assert!(catalog.list_assets(&project.id).unwrap().is_empty());
}

#[test]
fn import_preflight_names_the_existing_sequence_a_run_would_extend() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture(
        directory.path(),
        &[("C0101.MP4", b"one"), ("C0102.MP4", b"two")],
    );
    let card = inspect_card(&mount).unwrap().unwrap();
    let existing = catalog
        .declare_capture_sequence(
            &project.id,
            "Sony (C-series)",
            "Sony SD-A",
            &card.session_id,
            "C",
        )
        .unwrap();
    let operations_before = catalog.list_operations(&project.id).unwrap().len();

    let plan = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();

    assert_eq!(plan.sequence_preview.runs.len(), 1);
    let run = &plan.sequence_preview.runs[0];
    assert_eq!(run.decision, ImportSequenceRunDecision::ExtendExisting);
    assert_eq!(
        run.matched_existing_sequence_id.as_deref(),
        Some(existing.id.as_str())
    );
    assert_eq!(
        run.matched_existing_sequence_label.as_deref(),
        Some("Sony SD-A")
    );
    assert_eq!(run.compared_existing_sequence_id, None);
    assert_eq!(run.first_relative_path, "DCIM/100MSDCF/C0101.MP4");
    assert_eq!(run.last_relative_path, "DCIM/100MSDCF/C0102.MP4");
    assert_eq!(run.clip_count, 2);
    assert_eq!(run.camera_key, "Sony (C-series)");
    assert!(run.reason.contains("Sony SD-A"));
    // A clean extension is routine: nothing here needs a second look.
    assert!(!run.needs_review);
    assert_eq!(
        catalog.list_operations(&project.id).unwrap().len(),
        operations_before
    );
    assert!(catalog.list_assets(&project.id).unwrap().is_empty());
}

#[test]
fn import_preflight_explains_volume_and_prefix_mismatch_against_a_named_sequence() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture(directory.path(), &[("C0101.MP4", b"new card")]);
    let card = inspect_card(&mount).unwrap().unwrap();
    let first_existing = catalog
        .declare_capture_sequence(
            &project.id,
            "Sony (C-series)",
            "Sony SD-A",
            "card_previous_session",
            "SONY",
        )
        .unwrap();
    let second_existing = catalog
        .declare_capture_sequence(
            &project.id,
            "Sony (C-series)",
            "Sony SD-B",
            "card_other_session",
            "CX",
        )
        .unwrap();
    let compared = if first_existing.id < second_existing.id {
        &first_existing
    } else {
        &second_existing
    };
    let operations_before = catalog.list_operations(&project.id).unwrap().len();

    let plan = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();

    let run = &plan.sequence_preview.runs[0];
    assert_eq!(run.decision, ImportSequenceRunDecision::ProposeNew);
    assert_eq!(run.matched_existing_sequence_id, None);
    assert_eq!(
        run.compared_existing_sequence_id.as_deref(),
        Some(compared.id.as_str())
    );
    assert_eq!(
        run.compared_existing_sequence_label.as_deref(),
        Some(compared.label.as_str())
    );
    assert!(run.evidence.iter().any(|found| matches!(
        found,
        ImportSequenceBoundaryEvidence::VolumeChange { from, to }
            if from == &compared.volume_hint && to == &card.session_id
    )));
    assert!(run.evidence.iter().any(|found| matches!(
        found,
        ImportSequenceBoundaryEvidence::PrefixChange { from, to }
            if from == &compared.filename_prefix && to == "C"
    )));
    assert!(run.reason.contains(&compared.label));
    // Card/filename evidence actually disagrees with the compared
    // sequence — this is the one case that genuinely needs a look.
    assert!(run.needs_review);
    assert_eq!(plan.sequence_preview.matched_existing_sequence_count, 0);
    assert_eq!(plan.sequence_preview.suggested_new_sequence_count, 1);
    assert_eq!(
        catalog.list_operations(&project.id).unwrap().len(),
        operations_before
    );
    assert!(catalog.list_assets(&project.id).unwrap().is_empty());
}

#[test]
fn import_preflight_does_not_flag_a_second_run_that_only_lost_the_slot() {
    // Two runs of the same camera on one card, split by a folder/counter
    // reset — the same fixture shape as
    // `import_preflight_surfaces_folder_and_counter_boundary_evidence`,
    // but with a matching existing sequence declared first. Both runs
    // agree with it (empty volume/prefix hints match anything), so the
    // first one extends it and the second — which agrees just as much,
    // but lost the slot to the first — must not be flagged as a
    // condition needing review. It is a routine multi-run outcome, not a
    // disagreement with the sequence's evidence.
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let existing = catalog
        .declare_capture_sequence(&project.id, "Sony (C-series)", "Sony SD-A", "", "")
        .unwrap();
    let mount = directory.path().join("SONY_SD");
    let first_folder = mount.join("DCIM/100MSDCF");
    let second_folder = mount.join("DCIM/101MSDCF");
    fs::create_dir_all(&first_folder).unwrap();
    fs::create_dir_all(&second_folder).unwrap();
    let first = first_folder.join("C0241.MP4");
    let second = second_folder.join("C0001.MP4");
    fs::write(&first, b"first card run").unwrap();
    fs::write(&second, b"second card run").unwrap();
    let base = UNIX_EPOCH + std::time::Duration::from_secs(100);
    fs::File::open(&first)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(base))
        .unwrap();
    fs::File::open(&second)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(base + std::time::Duration::from_secs(1)))
        .unwrap();

    let plan = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();

    assert_eq!(plan.sequence_preview.runs.len(), 2);
    let extended = &plan.sequence_preview.runs[0];
    assert_eq!(extended.decision, ImportSequenceRunDecision::ExtendExisting);
    assert_eq!(
        extended.matched_existing_sequence_id.as_deref(),
        Some(existing.id.as_str())
    );
    assert!(!extended.needs_review);

    let displaced = &plan.sequence_preview.runs[1];
    assert_eq!(displaced.decision, ImportSequenceRunDecision::ProposeNew);
    assert_eq!(
        displaced.compared_existing_sequence_id.as_deref(),
        Some(existing.id.as_str())
    );
    assert!(displaced.reason.contains("already extends"));
    // The core message: agreeing with a sequence that another run in the
    // same batch already claimed is routine, not a disagreement.
    assert!(!displaced.needs_review);
    assert!(catalog.list_assets(&project.id).unwrap().is_empty());
}

// The tests above prove each detector fires on a synthetic layout built to
// trigger it. The ones below are the other half of the same acceptance
// question (#56): that an ordinary card, and a gap just under the
// threshold, do *not* produce a spurious boundary or evidence entry, and
// that the two evidence kinds not yet exercised through the real
// `preflight_import` entry point (`PrefixChange`, `LargeDiscontinuity`) are
// covered there too, not only at the low-level candidate-list layer.

#[test]
fn sequence_detector_does_not_flag_a_gap_under_the_discontinuity_threshold() {
    let (_ordered, runs, boundaries) = propose_import_runs(vec![
        sequence_hint("A/C0001.MP4", "Sony", "card", "A", "C", Some(1), Some(1)),
        sequence_hint(
            "A/C0002.MP4",
            "Sony",
            "card",
            "A",
            "C",
            Some(2),
            Some(LARGE_CAPTURE_SEQUENCE_GAP_SECONDS), // one second under the gap.
        ),
    ]);
    assert!(boundaries.is_empty());
    assert_eq!(runs.len(), 1);
    assert!(runs[0].boundary_evidence.is_empty());
}

#[test]
fn import_preflight_flags_no_boundary_for_ordinary_continuous_footage() {
    // One card, one folder, one filename prefix, an ascending counter, and
    // clips shot minutes apart: nothing here should ever look like a card
    // swap. This is the negative twin of the five positive fixtures above
    // and below — a detector that fired here would corrupt chronology on
    // every ordinary import.
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = directory.path().join("SONY_SD");
    let folder = mount.join("DCIM/100MSDCF");
    fs::create_dir_all(&folder).unwrap();
    let base = UNIX_EPOCH + std::time::Duration::from_secs(1_000);
    for (index, counter) in (1..=5u64).enumerate() {
        let path = folder.join(format!("C{counter:04}.MP4"));
        fs::write(&path, format!("clip {counter}").into_bytes()).unwrap();
        let shot_at = base + std::time::Duration::from_secs(index as u64 * 60);
        fs::File::open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(shot_at))
            .unwrap();
    }

    let plan = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();

    assert!(plan.sequence_preview.boundaries.is_empty());
    assert_eq!(plan.sequence_preview.runs.len(), 1);
    let run = &plan.sequence_preview.runs[0];
    assert_eq!(run.clip_count, 5);
    assert!(run.evidence.is_empty());
    assert!(!run.needs_review);
}

#[test]
fn import_preflight_surfaces_prefix_change_boundary_evidence() {
    // Same card, same folder, tight timing, and neither filename matches a
    // known camera pattern — so both resolve to the card's own camera
    // hint and only the filename prefix itself changes. `PrefixChange`
    // should be the only evidence, exercised here through the real scan
    // rather than a hand-built candidate list.
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = directory.path().join("SONY_SD");
    let folder = mount.join("DCIM/100MSDCF");
    fs::create_dir_all(&folder).unwrap();
    let first = folder.join("AAA0001.MP4");
    let second = folder.join("BBB0002.MP4");
    fs::write(&first, b"first pattern").unwrap();
    fs::write(&second, b"second pattern").unwrap();
    let base = UNIX_EPOCH + std::time::Duration::from_secs(200);
    fs::File::open(&first)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(base))
        .unwrap();
    fs::File::open(&second)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(base + std::time::Duration::from_secs(1)))
        .unwrap();

    let plan = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();

    assert_eq!(plan.sequence_preview.boundaries.len(), 1);
    let boundary = &plan.sequence_preview.boundaries[0];
    assert_eq!(boundary.after_relative_path, "DCIM/100MSDCF/BBB0002.MP4");
    assert!(matches!(
        boundary.evidence.as_slice(),
        [ImportSequenceBoundaryEvidence::PrefixChange { from, to }]
            if from == "AAA" && to == "BBB"
    ));
}

#[test]
fn import_preflight_surfaces_large_discontinuity_boundary_evidence() {
    // Same card, same folder, same filename prefix, a normally ascending
    // counter — only the clock jumps by more than
    // `LARGE_CAPTURE_SEQUENCE_GAP_SECONDS`, so `LargeDiscontinuity` should
    // be the only evidence, exercised here through the real scan.
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = directory.path().join("SONY_SD");
    let folder = mount.join("DCIM/100MSDCF");
    fs::create_dir_all(&folder).unwrap();
    let first = folder.join("C0001.MP4");
    let second = folder.join("C0002.MP4");
    fs::write(&first, b"before the gap").unwrap();
    fs::write(&second, b"after the gap").unwrap();
    let base = UNIX_EPOCH + std::time::Duration::from_secs(300);
    fs::File::open(&first)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(base))
        .unwrap();
    fs::File::open(&second)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(
            base + std::time::Duration::from_secs(LARGE_CAPTURE_SEQUENCE_GAP_SECONDS),
        ))
        .unwrap();

    let plan = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();

    assert_eq!(plan.sequence_preview.boundaries.len(), 1);
    let boundary = &plan.sequence_preview.boundaries[0];
    assert_eq!(boundary.after_relative_path, "DCIM/100MSDCF/C0002.MP4");
    assert!(matches!(
        boundary.evidence.as_slice(),
        [ImportSequenceBoundaryEvidence::LargeDiscontinuity { gap_seconds }]
            if *gap_seconds == LARGE_CAPTURE_SEQUENCE_GAP_SECONDS
    ));
}

#[test]
fn import_preflight_recognizes_a_reformatted_card_reusing_a_path_as_a_volume_change() {
    // The issue's named "reformat-reuse" scenario: a card is imported, then
    // physically reformatted and reused for an unrelated shoot before the
    // next import. `cards.rs` already proves a reformat changes
    // `session_id` even when paths and filenames repeat
    // (`card_identity_survives_new_footage_but_not_a_reformat`); this test
    // proves that identity change actually reaches the preview as
    // `VolumeChange` evidence against the sequence the first session
    // declared, isolated from any prefix or folder change.
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture(
        directory.path(),
        &[("C0101.MP4", b"day one"), ("C0102.MP4", b"day one more")],
    );
    let original_card = inspect_card(&mount).unwrap().unwrap();
    let original_sequence = catalog
        .declare_capture_sequence(
            &project.id,
            "Sony (C-series)",
            "Sony SD - Day 1",
            &original_card.session_id,
            "C",
        )
        .unwrap();

    // Reformat: the DCIM tree is wiped and rewritten with unrelated
    // footage that happens to reuse the same relative path and filename
    // prefix.
    fs::remove_dir_all(mount.join("DCIM")).unwrap();
    fs::create_dir_all(mount.join("DCIM/100MSDCF")).unwrap();
    fs::write(mount.join("DCIM/100MSDCF/C0101.MP4"), b"reshot entirely").unwrap();
    let reformatted_card = inspect_card(&mount).unwrap().unwrap();
    assert_ne!(reformatted_card.session_id, original_card.session_id);

    let plan = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();

    assert_eq!(plan.sequence_preview.runs.len(), 1);
    let run = &plan.sequence_preview.runs[0];
    assert_eq!(run.decision, ImportSequenceRunDecision::ProposeNew);
    assert_eq!(
        run.compared_existing_sequence_id.as_deref(),
        Some(original_sequence.id.as_str())
    );
    assert!(run.reason.contains(&original_sequence.label));
    assert!(run.needs_review);
    assert!(matches!(
        run.evidence.as_slice(),
        [ImportSequenceBoundaryEvidence::VolumeChange { from, to }]
            if from == &original_card.session_id && to == &reformatted_card.session_id
    ));
}

/// A card holding real, probeable media. Import creates assets, which runs
/// ffprobe, so byte blobs will not do here.
fn card_fixture_media(root: &Path, clips: &[(&str, &str)]) -> PathBuf {
    let mount = root.join("DJI_SD");
    let layout = mount.join("DCIM/100MSDCF");
    fs::create_dir_all(&layout).unwrap();
    for (name, color) in clips {
        let status = crate::tool_command("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("color=c={color}:s=64x64:r=5:d=1"),
                "-c:v",
                "mpeg4",
            ])
            .arg(layout.join(name))
            .status()
            .unwrap();
        assert!(status.success(), "failed to build fixture {name}");
    }
    mount
}

fn card_fixture(root: &Path, clips: &[(&str, &[u8])]) -> PathBuf {
    let mount = root.join("DJI_SD");
    let layout = mount.join("DCIM/100MSDCF");
    fs::create_dir_all(&layout).unwrap();
    for (name, bytes) in clips {
        fs::write(layout.join(name), bytes).unwrap();
    }
    mount
}

/// The number the dialog leads with, and the states behind it.
#[test]
fn preflight_separates_new_from_known_and_from_lost() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture(
        directory.path(),
        &[
            ("C0001.MP4", b"already imported and reachable"),
            ("C0002.MP4", b"already imported but gone missing"),
            ("C0003.MP4", b"brand new footage"),
        ],
    );
    let layout = mount.join("DCIM/100MSDCF");

    // One clip is known and still reachable on the card itself.
    insert_asset_at(
        &catalog,
        &project.id,
        "asset_here",
        &layout.join("C0001.MP4"),
    );
    // One is known but its original lived somewhere that is now gone.
    let lost = directory.path().join("old-drive/C0002.MP4");
    fs::create_dir_all(lost.parent().unwrap()).unwrap();
    fs::copy(layout.join("C0002.MP4"), &lost).unwrap();
    insert_asset_at(&catalog, &project.id, "asset_lost", &lost);
    fs::remove_file(&lost).unwrap();

    let preflight = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();

    assert_eq!(preflight.scanned, 3);
    assert_eq!(preflight.new_clips, 1);
    assert_eq!(preflight.relinkable, 1);
    // The known reachable clip and the new clip both need a proxy after import.
    assert_eq!(preflight.missing_proxies, 2);
    assert_eq!(preflight.unreadable, 0);
    assert_eq!(preflight.sequence_preview.new_clip_count, 1);
    assert_eq!(preflight.sequence_preview.proposed_sequence_count, 1);
    assert_eq!(preflight.sequence_preview.suggested_new_sequence_count, 1);
    assert_eq!(
        preflight
            .card
            .as_ref()
            .map(|card| card.camera_hint.as_str()),
        Some("Sony")
    );

    let relinkable = preflight
        .items
        .iter()
        .find(|item| item.classification == Classification::KnownRelinkable)
        .unwrap();
    assert_eq!(relinkable.asset_id.as_deref(), Some("asset_lost"));
    assert_eq!(relinkable.relative_path, "DCIM/100MSDCF/C0002.MP4");

    // #65: every readable candidate carries the fingerprint already spent
    // to classify it (the import-preview cache key) and a cheap
    // filesystem timestamp, without preflight running ffprobe.
    for item in &preflight.items {
        assert!(
            item.fingerprint.is_some(),
            "{} should carry the fingerprint classification already computed",
            item.filename
        );
        assert!(
            item.filesystem_modified_at.is_some(),
            "{} should carry a best-available filesystem timestamp",
            item.filename
        );
    }
}

/// A file preflight cannot read still classifies (as `Unreadable`) rather
/// than aborting the whole scan, and it still keeps the one field that
/// costs only a `stat`: a failed fingerprint read must not also lose the
/// cheap filesystem timestamp a caller could still show for the row.
#[test]
#[cfg(unix)]
fn an_unreadable_file_still_keeps_its_cheap_filesystem_timestamp() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture(directory.path(), &[("C0001.MP4", b"will be locked")]);
    let clip = mount.join("DCIM/100MSDCF/C0001.MP4");
    fs::set_permissions(&clip, fs::Permissions::from_mode(0o000)).unwrap();

    let preflight = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();

    assert_eq!(preflight.unreadable, 1);
    let item = &preflight.items[0];
    assert_eq!(item.classification, Classification::Unreadable);
    assert!(!item.error.is_empty());
    assert!(
        item.fingerprint.is_none(),
        "a file that could not be opened has no fingerprint to reuse"
    );
    assert!(
        item.filesystem_modified_at.is_some(),
        "a stat does not require read permission, so the timestamp still degrades gracefully rather than disappearing"
    );

    // Restore permissions so the temp directory can clean itself up.
    fs::set_permissions(&clip, fs::Permissions::from_mode(0o644)).unwrap();
}

/// A preview that half-imports is not a preview.
#[test]
fn preflight_leaves_the_catalog_untouched() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture(directory.path(), &[("C0001.MP4", b"new footage")]);

    let before = catalog.list_assets(&project.id).unwrap().len();
    let operations_before = catalog.list_operations(&project.id).unwrap().len();

    catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();

    assert_eq!(catalog.list_assets(&project.id).unwrap().len(), before);
    assert_eq!(
        catalog.list_operations(&project.id).unwrap().len(),
        operations_before
    );
}

/// The dialog must be able to say where footage is going without the user
/// inferring it.
#[test]
fn a_card_destination_names_the_root_and_the_resolved_path() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture(directory.path(), &[("C0001.MP4", b"new footage")]);
    let archive = directory.path().join("nas/VideoArchive");
    fs::create_dir_all(&archive).unwrap();
    let root = catalog.create_archive_root("NAS Video", &archive).unwrap();

    let preflight = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();

    let destination = &preflight.destination;
    assert!(destination.copies);
    assert!(destination.ready, "{}", destination.blocked_reason);
    assert_eq!(
        destination.archive_root.as_ref().unwrap().display_name,
        "NAS Video"
    );
    assert!(destination.resolved_path.contains("Spain 2026"));
    assert!(destination.resolved_path.contains("DJI_SD-"));
    assert_eq!(destination.bytes_to_copy, "new footage".len() as u64);
    assert!(destination.summary.contains("stay on the card too"));
}

#[test]
fn referencing_in_place_says_so_and_says_where() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let folder = directory.path().join("nas-share");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("C0001.MP4"), b"nas footage").unwrap();

    let preflight = catalog
        .preflight_import(&project.id, &folder, None, None, |_, _, _| true)
        .unwrap();

    assert!(!preflight.destination.copies);
    assert_eq!(preflight.destination.bytes_to_copy, 0);
    assert!(
        preflight
            .destination
            .summary
            .contains("stay where they are")
    );
    assert!(
        preflight.destination.resolved_path.contains("nas-share"),
        "the dialog must name the folder, not just the policy"
    );
}

/// A copy that will not fit is refused before it starts, not halfway through.
#[test]
fn an_unreachable_or_full_destination_blocks_the_import() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture(directory.path(), &[("C0001.MP4", b"footage")]);
    let archive = directory.path().join("shuttle");
    fs::create_dir_all(&archive).unwrap();
    let root = catalog
        .create_archive_root("Shuttle SSD", &archive)
        .unwrap();

    fs::remove_dir_all(&archive).unwrap();
    let preflight = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();
    assert!(!preflight.destination.ready);
    assert!(
        preflight
            .destination
            .blocked_reason
            .contains("not connected"),
        "{}",
        preflight.destination.blocked_reason
    );
}

/// An archived original is never overwritten by a card that reuses its name.
#[test]
fn an_occupied_archive_path_is_reported_rather_than_overwritten() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture(directory.path(), &[("C0001.MP4", b"reshot footage")]);
    let archive = directory.path().join("nas");
    fs::create_dir_all(&archive).unwrap();
    let root = catalog.create_archive_root("NAS Video", &archive).unwrap();

    // Something already sits exactly where this clip would land.
    let card = inspect_card(&mount).unwrap().unwrap();
    let target = catalog
        .archive_target(&project.id, &root.id, &card, "DCIM/100MSDCF/C0001.MP4")
        .unwrap();
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, b"different bytes entirely").unwrap();

    let preflight = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();
    assert_eq!(preflight.collisions, 1);
    assert_eq!(preflight.new_clips, 0);
}

/// The headline workflow: insert a card, click once, and get verified copies
/// in the archive — then insert it again with more footage and copy only the
/// new clips.
#[test]
fn a_card_imports_once_and_then_incrementally() {
    // Creating an asset probes the media, so this needs the real toolchain.
    if crate::tool_command("ffprobe")
        .arg("-version")
        .output()
        .is_err()
    {
        eprintln!("ffprobe is unavailable; skipping media integration test");
        return;
    }

    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture_media(
        directory.path(),
        &[("C0001.MP4", "navy"), ("C0002.MP4", "maroon")],
    );
    let archive = directory.path().join("nas/VideoArchive");
    fs::create_dir_all(&archive).unwrap();
    let root = catalog.create_archive_root("NAS Video", &archive).unwrap();

    let plan = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();
    assert_eq!(plan.new_clips, 2);

    let mut committed_progress_assets = Vec::new();
    let mut committed_progress_count = 0;
    let outcome = catalog
        .run_import(&project.id, &plan, true, |progress| {
            if let Some(asset) = progress.asset {
                // The live object is emitted only after the asset itself
                // is materialized, so a frontend may put it on screen
                // before the next file starts copying.
                assert_eq!(asset.project_id, project.id);
                assert!(asset.original_available);
                assert_eq!(progress.completed_items, committed_progress_count + 1);
                committed_progress_count += 1;
                committed_progress_assets.push(asset.id);
            } else {
                // Copy/read progress may advance bytes, but it cannot
                // announce a catalog row before the post-commit callback.
                assert_eq!(progress.completed_items, committed_progress_count);
            }
            true
        })
        .unwrap();
    assert_eq!(outcome.imported, 2);
    assert_eq!(outcome.imported_asset_ids.len(), 2);
    assert!(outcome.relinked_asset_ids.is_empty());
    assert_eq!(outcome.copied, 2);
    assert!(outcome.bytes_copied > 0);
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(committed_progress_assets, outcome.imported_asset_ids);

    // Originals landed under the archive root, keeping the camera's own
    // folder structure, and the card still has them.
    let card = plan.card.as_ref().unwrap();
    let archived = archive
        .join("Spain 2026")
        .join(crate::cards::card_session_slug(card))
        .join("DCIM/100MSDCF/C0001.MP4");
    assert_eq!(
        fs::read(&archived).unwrap(),
        fs::read(mount.join("DCIM/100MSDCF/C0001.MP4")).unwrap(),
        "the archived original must be byte-identical to the card's"
    );
    assert!(mount.join("DCIM/100MSDCF/C0001.MP4").is_file());

    // Assets point at the archive copy, not the card, so ejecting is safe.
    for asset in catalog.list_assets(&project.id).unwrap() {
        assert!(asset.path.starts_with(archive.to_string_lossy().as_ref()));
        assert!(asset.original_available);
    }

    // Shoot more footage and reinsert the same card.
    let extra = card_fixture_media(&directory.path().join("more"), &[("C0003.MP4", "olive")]);
    fs::copy(
        extra.join("DCIM/100MSDCF/C0003.MP4"),
        mount.join("DCIM/100MSDCF/C0003.MP4"),
    )
    .unwrap();
    let second_plan = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();
    assert_eq!(second_plan.scanned, 3);
    assert_eq!(second_plan.new_clips, 1, "only the new clip is new");
    assert!(
        second_plan.destination.bytes_to_copy > 0,
        "the second import must state the cost of only the new clip"
    );

    let second = catalog
        .run_import(&project.id, &second_plan, true, |_| true)
        .unwrap();
    assert_eq!(second.imported, 1);
    assert_eq!(second.imported_asset_ids.len(), 1);
    assert_eq!(second.copied, 1, "already-archived clips are not re-copied");
    assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 3);
}

#[test]
fn archive_location_bookkeeping_failure_keeps_the_committed_clip_visible_with_a_warning() {
    if crate::tool_command("ffprobe")
        .arg("-version")
        .output()
        .is_err()
    {
        eprintln!("ffprobe is unavailable; skipping media integration test");
        return;
    }

    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture_media(directory.path(), &[("C0001.MP4", "navy")]);
    let archive = directory.path().join("nas");
    fs::create_dir_all(&archive).unwrap();
    let root = catalog.create_archive_root("NAS Video", &archive).unwrap();
    let plan = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();

    // Force only the post-ingest archive-location UPDATE to fail. The
    // operation-backed asset membership and its verified local location
    // have already committed by then, which is the boundary this
    // regression protects.
    catalog
        .connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fail_archive_location \
                 BEFORE UPDATE OF archive_root_id ON project_asset_local_locations \
                 WHEN NEW.archive_root_id IS NOT NULL \
                 BEGIN SELECT RAISE(FAIL, 'forced archive-location failure'); END;",
        )
        .unwrap();

    let mut emitted = Vec::new();
    let outcome = catalog
        .run_import(&project.id, &plan, true, |progress| {
            if let Some(asset) = progress.asset {
                emitted.push(asset);
            }
            true
        })
        .unwrap();

    assert!(
        outcome.failures.is_empty(),
        "a durable clip is never reported as failed"
    );
    assert_eq!(outcome.imported_asset_ids.len(), 1);
    assert_eq!(
        emitted.iter().map(|asset| &asset.id).collect::<Vec<_>>(),
        outcome.imported_asset_ids.iter().collect::<Vec<_>>()
    );
    assert_eq!(outcome.warnings.len(), 1);
    assert!(
        outcome.warnings[0]
            .error
            .contains("archive location was not recorded")
    );
    let assets = catalog.list_assets(&project.id).unwrap();
    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0].id, outcome.imported_asset_ids[0]);
    assert!(
        assets[0].original_available,
        "the verified archive copy remains usable"
    );
}

/// Importing must not start a copy it cannot finish.
#[test]
fn an_import_refuses_to_run_against_a_blocked_destination() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture(directory.path(), &[("C0001.MP4", b"footage")]);
    let archive = directory.path().join("shuttle");
    fs::create_dir_all(&archive).unwrap();
    let root = catalog
        .create_archive_root("Shuttle SSD", &archive)
        .unwrap();

    let plan = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();
    fs::remove_dir_all(&archive).unwrap();
    let replanned = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();

    assert!(
        catalog
            .run_import(&project.id, &replanned, true, |_| true)
            .is_err()
    );
    assert!(catalog.list_assets(&project.id).unwrap().is_empty());
    drop(plan);
}

/// Cancelling stops cleanly, and re-running picks up where it left off.
#[test]
fn a_cancelled_import_resumes_without_duplicating_work() {
    // Creating an asset probes the media, so this needs the real toolchain.
    if crate::tool_command("ffprobe")
        .arg("-version")
        .output()
        .is_err()
    {
        eprintln!("ffprobe is unavailable; skipping media integration test");
        return;
    }

    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture_media(
        directory.path(),
        &[
            ("C0001.MP4", "navy"),
            ("C0002.MP4", "maroon"),
            ("C0003.MP4", "green"),
        ],
    );
    let archive = directory.path().join("nas");
    fs::create_dir_all(&archive).unwrap();
    let root = catalog.create_archive_root("NAS Video", &archive).unwrap();

    let plan = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();
    let mut cancelled_progress_assets = Vec::new();
    let outcome = catalog
        .run_import(&project.id, &plan, true, |progress| {
            if let Some(asset) = progress.asset {
                cancelled_progress_assets.push(asset.id);
            }
            progress.completed_items < 2
        })
        .unwrap();
    assert!(outcome.cancelled);
    assert_eq!(outcome.imported, 2);
    assert_eq!(cancelled_progress_assets, outcome.imported_asset_ids);

    // Re-planning sees only what is left.
    let resumed_plan = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();
    assert_eq!(resumed_plan.new_clips, 1);
    let resumed = catalog
        .run_import(&project.id, &resumed_plan, true, |_| true)
        .unwrap();
    assert_eq!(resumed.imported, 1);
    assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 3);
}

/// #74: the end-to-end version of the reformat-reuse scenario, driven
/// entirely through the production path.
///
/// The sibling test above sets its sequence up with `declare_capture_
/// sequence`, a call no real import performs — which was the whole
/// complaint: the detectors were correct and unreachable. Here the first
/// import *is* the setup, and the evidence it leaves behind is what the
/// second import is measured against.
#[test]
fn a_real_import_records_its_card_so_a_reformat_is_caught_next_time() {
    if crate::tool_command("ffprobe")
        .arg("-version")
        .output()
        .is_err()
    {
        eprintln!("ffprobe is unavailable; skipping media integration test");
        return;
    }

    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture_media(
        directory.path(),
        &[("C0101.MP4", "navy"), ("C0102.MP4", "maroon")],
    );
    let original_card = inspect_card(&mount).unwrap().unwrap();

    // An ordinary one-click import. No chronology gate, no declaration.
    let plan = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();
    let outcome = catalog
        .run_import(&project.id, &plan, false, |_| true)
        .unwrap();
    assert_eq!(outcome.imported, 2);
    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);

    // The sequence it created now names the card the bytes came off.
    let sequences = catalog.capture_sequences(&project.id).unwrap();
    assert_eq!(sequences.len(), 1);
    let declared = &sequences[0];
    assert_eq!(declared.volume_hint, original_card.session_id);
    assert_eq!(declared.filename_prefix, "C");

    // Reformat: same mount path, same filename prefix, unrelated footage.
    fs::remove_dir_all(mount.join("DCIM")).unwrap();
    let reformatted = card_fixture_media(directory.path(), &[("C0101.MP4", "green")]);
    assert_eq!(reformatted, mount);
    let reformatted_card = inspect_card(&mount).unwrap().unwrap();
    assert_ne!(reformatted_card.session_id, original_card.session_id);

    let second = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();
    assert_eq!(second.sequence_preview.runs.len(), 1);
    let run = &second.sequence_preview.runs[0];
    assert_eq!(run.decision, ImportSequenceRunDecision::ProposeNew);
    assert!(run.needs_review, "a reused card has to be worth a look");
    assert_eq!(
        run.compared_existing_sequence_id.as_deref(),
        Some(declared.id.as_str())
    );
    assert!(
        run.evidence.iter().any(|found| matches!(
            found,
            ImportSequenceBoundaryEvidence::VolumeChange { from, to }
                if from == &original_card.session_id && to == &reformatted_card.session_id
        )),
        "{:?}",
        run.evidence
    );
}

/// The same card again — more footage, nothing reformatted — must stay
/// routine. A detector that fired here would make every ordinary second
/// import a question.
#[test]
fn re_importing_the_same_card_after_shooting_more_stays_routine() {
    if crate::tool_command("ffprobe")
        .arg("-version")
        .output()
        .is_err()
    {
        eprintln!("ffprobe is unavailable; skipping media integration test");
        return;
    }

    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture_media(directory.path(), &[("C0101.MP4", "navy")]);

    let plan = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();
    catalog
        .run_import(&project.id, &plan, false, |_| true)
        .unwrap();
    let recorded = catalog.capture_sequences(&project.id).unwrap()[0].clone();
    assert!(!recorded.volume_hint.is_empty());

    // More footage on the same card preserves its identity (`cards.rs`).
    card_fixture_media(directory.path(), &[("C0102.MP4", "maroon")]);
    let second = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();
    let run = &second.sequence_preview.runs[0];
    assert_eq!(run.decision, ImportSequenceRunDecision::ExtendExisting);
    assert!(!run.needs_review);
    assert_eq!(
        run.matched_existing_sequence_id.as_deref(),
        Some(recorded.id.as_str())
    );

    // And the second import does not overwrite what the first recorded.
    catalog
        .run_import(&project.id, &second, false, |_| true)
        .unwrap();
    let after = catalog.capture_sequences(&project.id).unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].volume_hint, recorded.volume_hint);
}

/// A project whose sequences predate this evidence keeps working: an empty
/// hint stays "matches anything" until a real import writes one, and a
/// folder import — which has no durable identity — never writes one.
#[test]
fn an_empty_hint_still_matches_anything_and_a_folder_import_writes_none() {
    if crate::tool_command("ffprobe")
        .arg("-version")
        .output()
        .is_err()
    {
        eprintln!("ffprobe is unavailable; skipping media integration test");
        return;
    }

    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();

    // A plain folder, not a card: nothing here is a durable identity.
    let folder = directory.path().join("nas-footage");
    fs::create_dir_all(&folder).unwrap();
    let staged = card_fixture_media(
        directory.path(),
        &[("C0101.MP4", "navy"), ("C0102.MP4", "maroon")],
    );
    fs::copy(
        staged.join("DCIM/100MSDCF/C0101.MP4"),
        folder.join("C0101.MP4"),
    )
    .unwrap();

    let plan = catalog
        .preflight_import(&project.id, &folder, None, None, |_, _, _| true)
        .unwrap();
    assert!(plan.card.is_none());
    catalog
        .run_import(&project.id, &plan, false, |_| true)
        .unwrap();

    let sequences = catalog.capture_sequences(&project.id).unwrap();
    assert_eq!(sequences.len(), 1);
    assert_eq!(sequences[0].volume_hint, "", "a mount path is not a name");

    // A later import against that empty-hinted sequence still extends it.
    // Genuinely different footage, or preflight would rightly call it known.
    fs::copy(
        staged.join("DCIM/100MSDCF/C0102.MP4"),
        folder.join("C0102.MP4"),
    )
    .unwrap();
    let second = catalog
        .preflight_import(&project.id, &folder, None, None, |_, _, _| true)
        .unwrap();
    assert_eq!(
        second.sequence_preview.runs[0].decision,
        ImportSequenceRunDecision::ExtendExisting
    );
}

/// #66: the boundary is a rule about *this import*, and everything before
/// it stops being work without becoming a problem.
#[test]
fn a_cutoff_excludes_earlier_clips_without_calling_any_of_them_a_failure() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture(
        directory.path(),
        &[
            ("C0001.MP4", b"last spring"),
            ("C0002.MP4", b"last summer"),
            ("C0003.MP4", b"the trip"),
            ("C0004.MP4", b"the trip, later"),
        ],
    );

    let before = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();
    assert_eq!(before.new_clips, 4);
    assert_eq!(before.excluded_by_cutoff, 0);
    assert!(before.cutoff.is_none());

    let card = before.card.clone().unwrap();
    catalog
        .set_import_cutoff(
            &project.id,
            &card.session_id,
            &card.label,
            "DCIM/100MSDCF/C0003.MP4",
            "C0003.MP4",
            None,
        )
        .unwrap();

    let plan = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();
    assert_eq!(plan.scanned, 4, "the card still holds four clips");
    assert_eq!(plan.new_clips, 2);
    assert_eq!(plan.excluded_by_cutoff, 2);
    assert_eq!(plan.unreadable, 0);
    assert_eq!(plan.collisions, 0);
    assert_eq!(
        plan.cutoff.as_ref().map(|cutoff| cutoff.filename.as_str()),
        Some("C0003.MP4"),
        "a persistent filter has to be able to name itself"
    );
    // The clip the user pointed at is on the *inside* of the boundary.
    let excluded: Vec<&str> = plan
        .items
        .iter()
        .filter(|item| item.classification == Classification::ExcludedByCutoff)
        .map(|item| item.filename.as_str())
        .collect();
    assert_eq!(excluded, ["C0001.MP4", "C0002.MP4"]);
    assert_eq!(
        plan.destination.bytes_to_copy, 0,
        "nothing is copied in place"
    );
}

/// The other half of the criterion: excluded clips are not copied, not
/// ingested, and produce no failure. Needs real media, because this one
/// actually runs the import rather than only classifying it.
#[test]
fn an_excluded_clip_is_never_copied_ingested_or_reported() {
    if crate::tool_command("ffprobe")
        .arg("-version")
        .output()
        .is_err()
    {
        eprintln!("ffprobe is unavailable; skipping media integration test");
        return;
    }

    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture_media(
        directory.path(),
        &[
            ("C0001.MP4", "navy"),
            ("C0002.MP4", "maroon"),
            ("C0003.MP4", "green"),
        ],
    );
    let archive = directory.path().join("nas");
    fs::create_dir_all(&archive).unwrap();
    let root = catalog.create_archive_root("NAS Video", &archive).unwrap();
    let card = inspect_card(&mount).unwrap().unwrap();
    catalog
        .set_import_cutoff(
            &project.id,
            &card.session_id,
            &card.label,
            "DCIM/100MSDCF/C0002.MP4",
            "C0002.MP4",
            None,
        )
        .unwrap();

    let plan = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();
    assert_eq!(plan.excluded_by_cutoff, 1);

    let outcome = catalog
        .run_import(&project.id, &plan, true, |_| true)
        .unwrap();
    assert_eq!(outcome.imported, 2);
    assert_eq!(outcome.copied, 2);
    assert!(outcome.failures.is_empty());
    assert!(outcome.warnings.is_empty());

    let filenames: Vec<String> = catalog
        .list_assets(&project.id)
        .unwrap()
        .into_iter()
        .map(|asset| asset.filename)
        .collect();
    assert_eq!(filenames, ["C0002.MP4", "C0003.MP4"]);
    // Nothing landed in the archive for the clip the cutoff excluded.
    let archived: Vec<String> = WalkDir::new(&archive)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(!archived.contains(&"C0001.MP4".to_string()), "{archived:?}");
}

/// A cutoff answers "what should I import", never "what may this project
/// repair". An original the project has lost stays reachable through it.
#[test]
fn a_cutoff_never_hides_an_original_this_project_has_lost() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture(
        directory.path(),
        &[
            ("C0001.MP4", b"the lost clip"),
            ("C0002.MP4", b"the new clip"),
        ],
    );

    // C0001 belongs to the project already, and its original is offline.
    let gone = directory.path().join("old-drive/C0001.MP4");
    fs::create_dir_all(gone.parent().unwrap()).unwrap();
    fs::copy(mount.join("DCIM/100MSDCF/C0001.MP4"), &gone).unwrap();
    insert_asset_at(&catalog, &project.id, "asset_lost", &gone);
    fs::remove_file(&gone).unwrap();

    let card = inspect_card(&mount).unwrap().unwrap();
    catalog
        .set_import_cutoff(
            &project.id,
            &card.session_id,
            &card.label,
            "DCIM/100MSDCF/C0002.MP4",
            "C0002.MP4",
            None,
        )
        .unwrap();

    let plan = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();
    assert_eq!(plan.relinkable, 1, "the cutoff must not swallow a repair");
    assert_eq!(
        plan.relinkable_before_cutoff, 1,
        "and it must say so out loud"
    );
    assert_eq!(plan.excluded_by_cutoff, 0);
    assert_eq!(
        plan.items[0].classification,
        Classification::KnownRelinkable
    );
}

/// The rule follows the card, not the mount point, and not the camera.
#[test]
fn a_cutoff_survives_a_remount_and_does_not_reach_a_different_card() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let first_slot = directory.path().join("slot-a");
    fs::create_dir_all(&first_slot).unwrap();
    let mount = card_fixture(
        &first_slot,
        &[("C0001.MP4", b"last spring"), ("C0002.MP4", b"the trip")],
    );

    let card = inspect_card(&mount).unwrap().unwrap();
    catalog
        .set_import_cutoff(
            &project.id,
            &card.session_id,
            &card.label,
            "DCIM/100MSDCF/C0002.MP4",
            "C0002.MP4",
            None,
        )
        .unwrap();

    // The same volume, mounted somewhere else entirely.
    let second_slot = directory.path().join("slot-b");
    fs::create_dir_all(&second_slot).unwrap();
    let moved = second_slot.join("DJI_SD");
    fs::rename(&mount, &moved).unwrap();
    let plan = catalog
        .preflight_import(&project.id, &moved, None, None, |_, _, _| true)
        .unwrap();
    assert_eq!(plan.excluded_by_cutoff, 1);
    assert_eq!(plan.new_clips, 1);

    // A different card from the same camera arrives unfiltered: its own
    // first clip is not old footage just because another card's was.
    let other = card_fixture(
        &directory.path().join("other-camera-bag"),
        &[("C0001.MP4", b"a fresh card"), ("C0002.MP4", b"also fresh")],
    );
    let other_plan = catalog
        .preflight_import(&project.id, &other, None, None, |_, _, _| true)
        .unwrap();
    assert!(other_plan.cutoff.is_none());
    assert_eq!(other_plan.excluded_by_cutoff, 0);
    assert_eq!(other_plan.new_clips, 2);
}

/// Shooting more onto a partly imported card keeps its identity, so the
/// boundary keeps applying and the new footage is still found.
#[test]
fn newer_footage_on_the_same_card_is_still_discovered_past_the_cutoff() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture(
        directory.path(),
        &[("C0001.MP4", b"last spring"), ("C0002.MP4", b"the trip")],
    );
    let card = inspect_card(&mount).unwrap().unwrap();
    catalog
        .set_import_cutoff(
            &project.id,
            &card.session_id,
            &card.label,
            "DCIM/100MSDCF/C0002.MP4",
            "C0002.MP4",
            None,
        )
        .unwrap();

    fs::write(mount.join("DCIM/100MSDCF/C0003.MP4"), b"shot today").unwrap();
    let plan = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();
    assert_eq!(
        plan.card.as_ref().map(|found| found.session_id.as_str()),
        Some(card.session_id.as_str()),
        "adding footage must not change which card this is"
    );
    assert_eq!(plan.excluded_by_cutoff, 1);
    assert_eq!(plan.new_clips, 2);
}

/// #67: presence is a question about identity, not about a pathname. A
/// different card mounted where the interrupted one was must never be
/// mistaken for the source coming back.
#[test]
fn source_presence_tracks_card_identity_rather_than_the_mount_path() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture(
        directory.path(),
        &[("C0001.MP4", b"first"), ("C0002.MP4", b"second")],
    );

    let plan = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();
    assert!(import_source_present(&plan));

    // Unmounted: the whole volume is gone.
    let stash = directory.path().join("ejected");
    fs::rename(&mount, &stash).unwrap();
    assert!(!import_source_present(&plan));

    // A *different* card mounted at the same path is not this source.
    let intruder = card_fixture(&directory.path().join("other"), &[("C0009.MP4", b"other")]);
    fs::rename(&intruder, &mount).unwrap();
    assert!(!import_source_present(&plan));

    // The original card back in the slot is.
    fs::remove_dir_all(&mount).unwrap();
    fs::rename(&stash, &mount).unwrap();
    assert!(import_source_present(&plan));
}

/// A folder the user picked has no card identity to verify; its directory
/// still existing is the whole test.
#[test]
fn source_presence_for_a_plain_folder_is_just_its_directory() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let folder = directory.path().join("nas-footage");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("C0001.MP4"), b"in place").unwrap();

    let plan = catalog
        .preflight_import(&project.id, &folder, None, None, |_, _, _| true)
        .unwrap();
    assert!(plan.card.is_none());
    assert!(import_source_present(&plan));

    fs::remove_dir_all(&folder).unwrap();
    assert!(!import_source_present(&plan));
}

/// #76: the cancel flag is polled between copy chunks and again at the end
/// of an item, but not in between — and "in between" is where the whole
/// post-copy ingest lives. On a large clip that window is minutes long,
/// during which Cancel cannot be honoured.
#[test]
fn cancelling_after_the_bytes_land_registers_nothing() {
    if skip_without_ffprobe() {
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture_media(directory.path(), &[("C0001.MP4", "navy")]);
    let archive = directory.path().join("nas");
    fs::create_dir_all(&archive).unwrap();
    let root = catalog.create_archive_root("NAS Video", &archive).unwrap();

    let plan = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();

    // A clip visible under its final name means the copy has committed, so
    // the run is inside the post-copy window this test is about.
    let committed = archive.clone();
    let outcome = catalog
        .run_import(&project.id, &plan, true, move |_progress| {
            !walkdir::WalkDir::new(&committed)
                .into_iter()
                .filter_map(Result::ok)
                .any(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("MP4"))
                })
        })
        .unwrap();

    assert!(
        outcome.cancelled,
        "cancelling during the post-copy ingest must stop the run"
    );
    assert!(
        catalog.list_assets(&project.id).unwrap().is_empty(),
        "a cancelled item must not leave a registered asset behind"
    );
}

/// #76: losing the *destination* mid-import is as normal as losing the
/// card, and stopping matters more: every remaining item would fail the
/// same way, each after a full stall timeout spent waiting on a volume
/// that is not coming back inside this run.
#[cfg(unix)]
#[test]
fn a_destination_lost_mid_import_stops_the_run_rather_than_failing_every_file() {
    use std::os::unix::fs::PermissionsExt;

    if skip_without_ffprobe() {
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture_media(
        directory.path(),
        &[
            ("C0001.MP4", "navy"),
            ("C0002.MP4", "maroon"),
            ("C0003.MP4", "green"),
        ],
    );
    let mount_parent = directory.path().join("mnt");
    fs::create_dir_all(&mount_parent).unwrap();
    let archive = mount_parent.join("nas");
    fs::create_dir_all(&archive).unwrap();
    let root = catalog.create_archive_root("NAS Video", &archive).unwrap();

    let plan = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();

    let mut unplugged = false;
    let outcome = catalog
        .run_import(&project.id, &plan, true, |progress| {
            if progress.asset.is_some() && !unplugged {
                // Unmounting takes the directory away and leaves a parent
                // the app cannot recreate it in, the way /Volumes is
                // root-owned. Without that, the next `create_dir_all`
                // would quietly rebuild the mount point on the internal
                // disk and copy into it.
                fs::remove_dir_all(&archive).unwrap();
                let mut permissions = fs::metadata(&mount_parent).unwrap().permissions();
                permissions.set_mode(0o555);
                fs::set_permissions(&mount_parent, permissions).unwrap();
                unplugged = true;
            }
            true
        })
        .unwrap();

    let mut permissions = fs::metadata(&mount_parent).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&mount_parent, permissions).unwrap();

    assert!(
        outcome.destination_disconnected,
        "losing the archive root must stop the run"
    );
    assert!(!outcome.source_disconnected, "the card never went anywhere");
    assert!(
        outcome.failures.is_empty(),
        "a disconnected destination leaves work to redo, not files to triage: {:?}",
        outcome.failures
    );
    assert!(
        outcome.imported >= 1,
        "whatever committed before the unplug still counts"
    );
}

/// #67: pulling the card mid-copy stops the run as an interruption. It is
/// not a cancellation, it produces no failed files, and everything already
/// committed survives so the remount can resume the remainder.
#[test]
fn a_source_lost_mid_import_stops_the_run_and_resumes_after_remount() {
    if crate::tool_command("ffprobe")
        .arg("-version")
        .output()
        .is_err()
    {
        eprintln!("ffprobe is unavailable; skipping media integration test");
        return;
    }

    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture_media(
        directory.path(),
        &[
            ("C0001.MP4", "navy"),
            ("C0002.MP4", "maroon"),
            ("C0003.MP4", "green"),
        ],
    );
    let archive = directory.path().join("nas");
    fs::create_dir_all(&archive).unwrap();
    let root = catalog.create_archive_root("NAS Video", &archive).unwrap();

    let plan = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();

    // Rename rather than delete, so the identical bytes can be remounted
    // and the card keeps the session identity resume has to match on.
    let stash = directory.path().join("ejected");
    let mut ejected = false;
    let outcome = catalog
        .run_import(&project.id, &plan, true, |progress| {
            if progress.asset.is_some() && !ejected {
                fs::rename(&mount, &stash).unwrap();
                ejected = true;
            }
            true
        })
        .unwrap();

    assert!(outcome.source_disconnected);
    assert!(
        !outcome.cancelled,
        "a pulled card is not the user cancelling"
    );
    assert!(
        outcome.failures.is_empty(),
        "the uncopied remainder is work still to do, not files needing attention: {:?}",
        outcome.failures
    );
    assert_eq!(outcome.imported, 1);
    assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 1);

    // Remounted: re-planning classifies the committed clip as known and
    // copies only what is left.
    fs::rename(&stash, &mount).unwrap();
    let resumed_plan = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();
    assert_eq!(resumed_plan.new_clips, 2);
    let resumed = catalog
        .run_import(&project.id, &resumed_plan, true, |_| true)
        .unwrap();
    assert!(!resumed.source_disconnected);
    assert_eq!(resumed.imported, 2);
    assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 3);
}

/// Importing a card that holds a clip the project lost track of reconnects
/// it instead of importing a second copy.
#[test]
fn importing_reconnects_lost_originals_rather_than_duplicating_them() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture(directory.path(), &[("C0001.MP4", b"the lost clip")]);

    let gone = directory.path().join("old-drive/C0001.MP4");
    fs::create_dir_all(gone.parent().unwrap()).unwrap();
    fs::copy(mount.join("DCIM/100MSDCF/C0001.MP4"), &gone).unwrap();
    insert_asset_at(&catalog, &project.id, "asset_lost", &gone);
    fs::remove_file(&gone).unwrap();

    let plan = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();
    assert_eq!(plan.relinkable, 1);
    assert_eq!(plan.new_clips, 0);

    let outcome = catalog
        .run_import(&project.id, &plan, true, |_| true)
        .unwrap();
    assert_eq!(outcome.relinked, 1);
    assert_eq!(outcome.imported, 0);
    assert_eq!(outcome.relinked_asset_ids, ["asset_lost"]);
    assert!(outcome.imported_asset_ids.is_empty());
    assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 1);
    assert!(catalog.get_asset("asset_lost").unwrap().original_available);
}

#[test]
fn reference_import_reactivates_a_rolled_back_asset_at_its_current_location() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture(directory.path(), &[("C0001.MP4", b"returned footage")]);
    let current = mount.join("DCIM/100MSDCF/C0001.MP4");
    let old = directory.path().join("old-drive/C0001.MP4");
    fs::create_dir_all(old.parent().unwrap()).unwrap();
    fs::copy(&current, &old).unwrap();
    insert_asset_at(&catalog, &project.id, "asset_rolled_back", &old);
    catalog
        .remove_project_asset_membership(&project.id, &["asset_rolled_back".to_string()])
        .unwrap();
    fs::remove_file(&old).unwrap();

    let plan = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();
    assert_eq!(plan.new_clips, 1);
    assert_eq!(plan.items[0].asset_id.as_deref(), Some("asset_rolled_back"));

    let outcome = catalog
        .run_import(&project.id, &plan, true, |_| true)
        .unwrap();
    assert_eq!(outcome.imported_asset_ids, ["asset_rolled_back"]);
    assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 1);
    let asset = catalog.get_asset("asset_rolled_back").unwrap();
    assert_eq!(asset.path, current.to_string_lossy());
    assert!(asset.original_available);
}

#[test]
fn locate_does_not_reactivate_a_rolled_back_membership() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture(directory.path(), &[("C0001.MP4", b"returned footage")]);
    let current = mount.join("DCIM/100MSDCF/C0001.MP4");
    insert_asset_at(&catalog, &project.id, "asset_rolled_back", &current);
    catalog
        .remove_project_asset_membership(&project.id, &["asset_rolled_back".to_string()])
        .unwrap();

    let report = catalog
        .relink_folder(&project.id, &mount, |_, _, _| {})
        .unwrap();
    assert_eq!(report.repaired, 0);
    assert_eq!(report.unrecognized, 1);
    assert!(catalog.list_assets(&project.id).unwrap().is_empty());
}

#[test]
fn archive_import_reactivates_and_continues_a_rolled_back_asset_without_a_duplicate() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture(directory.path(), &[("C0001.MP4", b"returned footage")]);
    let current = mount.join("DCIM/100MSDCF/C0001.MP4");
    let old = directory.path().join("old-drive/C0001.MP4");
    fs::create_dir_all(old.parent().unwrap()).unwrap();
    fs::copy(&current, &old).unwrap();
    insert_asset_at(&catalog, &project.id, "asset_rolled_back", &old);
    catalog
        .remove_project_asset_membership(&project.id, &["asset_rolled_back".to_string()])
        .unwrap();
    fs::remove_file(&old).unwrap();

    let archive = directory.path().join("archive");
    fs::create_dir_all(&archive).unwrap();
    let root = catalog.create_archive_root("Archive", &archive).unwrap();
    let plan = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();
    assert_eq!(plan.new_clips, 1, "an absent archive copy must be created");

    let first = catalog
        .run_import(&project.id, &plan, true, |_| true)
        .unwrap();
    assert_eq!(first.imported_asset_ids, ["asset_rolled_back"]);
    assert_eq!(first.copied, 1);
    let asset = catalog.get_asset("asset_rolled_back").unwrap();
    assert!(asset.path.starts_with(archive.to_string_lossy().as_ref()));
    assert!(asset.original_available);
    assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 1);
    let archive_binding: Option<String> = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT archive_root_id FROM project_asset_local_locations WHERE asset_id = ?1 AND local_path = ?2",
            rusqlite::params!["asset_rolled_back", asset.path],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(archive_binding.as_deref(), Some(root.id.as_str()));

    // The rollback leaves archive bytes in place. A later smart-import run
    // reuses that matching copy without overwriting it, records the
    // currently mounted source, and makes the new membership rollbackable.
    catalog
        .remove_project_asset_membership(&project.id, &["asset_rolled_back".to_string()])
        .unwrap();
    let matching_plan = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();
    let archive_path = PathBuf::from(&asset.path);
    let mut target_replaced = false;
    let mut failure_progress_assets = Vec::new();
    let changed_after_preflight = catalog
        .run_import(&project.id, &matching_plan, true, |progress| {
            if let Some(asset) = progress.asset {
                failure_progress_assets.push(asset.id);
            }
            if !target_replaced {
                fs::write(&archive_path, b"replaced footage").unwrap();
                target_replaced = true;
            }
            true
        })
        .unwrap();
    assert_eq!(changed_after_preflight.failures.len(), 1);
    assert!(
        changed_after_preflight.failures[0]
            .error
            .contains("archive target already exists with different content")
    );
    assert!(failure_progress_assets.is_empty());
    assert!(catalog.list_assets(&project.id).unwrap().is_empty());
    fs::write(&archive_path, b"returned footage").unwrap();

    let original_mtime = fs::metadata(&current).unwrap().modified().unwrap();
    // The card session anchor contains pathname, size, and mtime. Preserve
    // those values while changing the bytes to prove the archive target is
    // protected even when a reused pathname looks like the same card.
    fs::write(&current, b"replaced footage").unwrap();
    fs::File::open(&current)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
        .unwrap();
    let collision = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();
    assert_eq!(collision.collisions, 1);
    assert_eq!(collision.new_clips, 0);
    assert_eq!(collision.relinkable, 0);
    assert!(
        catalog.list_assets(&project.id).unwrap().is_empty(),
        "different bytes at a reused pathname must not reactivate the old asset"
    );

    fs::write(&current, b"returned footage").unwrap();
    fs::File::open(&current)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
        .unwrap();
    let continued = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();
    assert_eq!(continued.new_clips, 1);
    assert_eq!(continued.relinkable, 0);
    assert_eq!(continued.collisions, 0);
    let resumed = catalog
        .run_import(&project.id, &continued, true, |_| true)
        .unwrap();
    assert_eq!(resumed.imported_asset_ids, ["asset_rolled_back"]);
    assert_eq!(resumed.copied, 0);
    assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 1);
    assert!(
        catalog
            .get_asset("asset_rolled_back")
            .unwrap()
            .original_available
    );

    let current_location_count: i64 = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM project_asset_local_locations WHERE asset_id = ?1 AND local_path = ?2 AND verification_state = 'verified'",
            rusqlite::params!["asset_rolled_back", current.to_string_lossy()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(current_location_count, 1);

    let session = catalog
        .record_import_session(&project.id, &continued, &resumed)
        .unwrap();
    let preview = catalog
        .import_rollback_preview(&project.id, &session.entry.id)
        .unwrap();
    assert_eq!(preview.removable_asset_count, 1);
    catalog
        .rollback_import_membership(&project.id, &session.entry.id)
        .unwrap();
    assert!(
        catalog.list_assets(&project.id).unwrap().is_empty(),
        "the reactivated membership belongs to this new import session"
    );
}

/// #77: an interruption between `commit_into_archive` and asset
/// registration leaves verified bytes in the archive that the catalog has
/// never heard of. Resume must recognize its own committed copy by
/// content rather than refuse it because something occupies the pathname.
#[test]
fn an_archive_copy_committed_before_registration_is_adopted_not_called_a_collision() {
    if skip_without_ffprobe() {
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = card_fixture_media(directory.path(), &[("C0001.MP4", "red")]);
    let archive = directory.path().join("archive");
    fs::create_dir_all(&archive).unwrap();
    let root = catalog.create_archive_root("Archive", &archive).unwrap();

    let plan = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();
    let first = catalog
        .run_import(&project.id, &plan, true, |_| true)
        .unwrap();
    assert_eq!(first.copied, 1);
    let asset = catalog.list_assets(&project.id).unwrap().remove(0);
    let archive_path = PathBuf::from(&asset.path);
    let committed_bytes = fs::read(&archive_path).unwrap();

    // Exactly the state a stall, crash, or kill between commit and
    // registration leaves behind: the archive holds a verified copy that
    // the catalog has no asset for.
    catalog
        .connection()
        .unwrap()
        .execute(
            "DELETE FROM assets WHERE id = ?1",
            rusqlite::params![asset.id],
        )
        .unwrap();
    assert!(catalog.list_assets(&project.id).unwrap().is_empty());
    assert!(archive_path.is_file(), "the committed copy stays on disk");

    let resumed_plan = catalog
        .preflight_import(&project.id, &mount, Some(&root.id), None, |_, _, _| true)
        .unwrap();
    assert_eq!(
        resumed_plan.collisions, 0,
        "our own verified copy is not a collision"
    );
    assert_eq!(resumed_plan.new_clips, 1);

    let resumed = catalog
        .run_import(&project.id, &resumed_plan, true, |_| true)
        .unwrap();
    assert_eq!(resumed.imported, 1);
    assert_eq!(
        resumed.copied, 0,
        "the committed copy is adopted, never recopied"
    );
    assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 1);
    assert_eq!(
        fs::read(&archive_path).unwrap(),
        committed_bytes,
        "adoption must not rewrite the archived original"
    );
}

#[test]
fn byte_sizes_render_for_a_person() {
    assert_eq!(human_bytes(0), "0 B");
    assert_eq!(human_bytes(999), "999 B");
    assert_eq!(human_bytes(62_400_000_000), "62.4 GB");
    assert_eq!(human_bytes(1_200_000_000_000), "1.2 TB");
}

// --- #71: choosing and remembering a camera identity at import time ---

/// Same shape as `card_fixture_media`, but with a caller-chosen mount
/// name and folder layout so these tests can build cards from different
/// manufacturers and, deliberately, cards that share a mount name.
fn labeled_card_fixture_media(
    root: &Path,
    mount_name: &str,
    layout: &str,
    clips: &[(&str, &str)],
) -> PathBuf {
    let mount = root.join(mount_name);
    let folder = mount.join(layout);
    fs::create_dir_all(&folder).unwrap();
    for (name, color) in clips {
        let status = crate::tool_command("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("color=c={color}:s=64x64:r=5:d=1"),
                "-c:v",
                "mpeg4",
            ])
            .arg(folder.join(name))
            .status()
            .unwrap();
        assert!(status.success(), "failed to build fixture {name}");
    }
    mount
}

fn skip_without_ffprobe() -> bool {
    if crate::tool_command("ffprobe")
        .arg("-version")
        .output()
        .is_err()
    {
        eprintln!("ffprobe is unavailable; skipping media integration test");
        return true;
    }
    false
}

/// The core of "the two cannot disagree": a user override must land, as
/// the exact same string, on the sequence preview's run *and* on every
/// asset ingest creates and the capture sequence it joins.
#[test]
fn a_camera_override_is_the_one_identity_the_sequence_preview_and_ingest_both_use() {
    if skip_without_ffprobe() || skip_without_exiftool() {
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    // C####.MP4 only weakly pattern-matches "Sony (C-series)" — a
    // manufacturer family, not the body the user actually owns. This is
    // the issue's own example of a wrong auto-detection.
    let mount = labeled_card_fixture_media(
        directory.path(),
        "UNTITLED",
        "DCIM/100MSDCF",
        &[("C0001.MP4", "navy"), ("C0002.MP4", "maroon")],
    );
    stamp_exif(
        &mount,
        "DCIM/100MSDCF",
        &["C0001.MP4", "C0002.MP4"],
        "SONY",
        "ILCE-7C",
    );

    let auto = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();
    assert_eq!(auto.camera_identity.camera, "SONY ILCE-7C");
    assert_eq!(auto.camera_identity.source, ImportCameraSource::Detected);

    let plan = catalog
        .preflight_import(&project.id, &mount, None, Some("Sony A7C II"), |_, _, _| {
            true
        })
        .unwrap();
    assert_eq!(
        plan.camera_identity,
        ResolvedImportCamera {
            camera: "Sony A7C II".to_string(),
            source: ImportCameraSource::UserSelected,
            evidence_strong: true,
        }
    );
    assert!(
        plan.sequence_preview
            .runs
            .iter()
            .all(|run| run.camera_key == "Sony A7C II"),
        "the sequence preview must use the chosen identity, not the weak pattern guess"
    );

    let outcome = catalog
        .run_import(&project.id, &plan, true, |_| true)
        .unwrap();
    assert_eq!(outcome.imported, 2);

    for asset in catalog.list_assets(&project.id).unwrap() {
        assert_eq!(asset.camera, "Sony A7C II");
    }
    let sequences = catalog.capture_sequences(&project.id).unwrap();
    assert_eq!(sequences.len(), 1);
    assert_eq!(sequences[0].camera_key, "Sony A7C II");
}

#[test]
fn merged_plain_detection_is_canonical_in_preflight_sequence_preview_and_import() {
    if skip_without_ffprobe() || skip_without_exiftool() {
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Merged camera import").unwrap();
    let connection = catalog.connection().unwrap();
    for (id, camera) in [("old", "DJI Pocket 3"), ("right", "Pocket")] {
        connection.execute(
            "INSERT INTO assets (id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, created_at) VALUES (?1, ?2, ?3, '', ?4, 1, 1, 5, 64, 64, 'mpeg4', ?5, NULL, NULL, ?6)",
            params![id, project.id, format!("fp-{id}"), format!("{id}.MP4"), camera, crate::now()],
        ).unwrap();
        connection.execute(
            "INSERT INTO capture_sequences (id, project_id, camera_key, label, volume_hint, filename_prefix, created_at) VALUES (?1, ?2, ?3, ?3, '', '', ?4)",
            params![format!("card-{id}"), project.id, camera, crate::now()],
        ).unwrap();
    }
    drop(connection);
    catalog
        .merge_camera(&project.id, "DJI Pocket 3", "Pocket")
        .unwrap();

    let mount = labeled_card_fixture_media(
        directory.path(),
        "CARD",
        "DCIM/DJI_001",
        &[("DJI_20251221092058_0001_D.MP4", "maroon")],
    );
    let plan = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();
    assert_eq!(plan.camera_identity.source, ImportCameraSource::Detected);
    assert_eq!(plan.camera_identity.camera, "Pocket");
    assert!(
        plan.sequence_preview
            .runs
            .iter()
            .all(|run| run.camera_key == "Pocket")
    );

    let outcome = catalog
        .run_import(&project.id, &plan, true, |_| true)
        .unwrap();
    assert_eq!(outcome.imported, 1);
    let imported = catalog
        .list_assets(&project.id)
        .unwrap()
        .into_iter()
        .find(|asset| asset.filename == "DJI_20251221092058_0001_D.MP4")
        .unwrap();
    assert_eq!(imported.camera, "Pocket");
}

#[test]
fn a_mapping_persistence_failure_warns_without_rolling_back_imported_assets() {
    if skip_without_ffprobe() || skip_without_exiftool() {
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = labeled_card_fixture_media(
        directory.path(),
        "UNTITLED",
        "DCIM/100MSDCF",
        &[("C0001.MP4", "navy")],
    );
    stamp_exif(&mount, "DCIM/100MSDCF", &["C0001.MP4"], "SONY", "ILCE-7C");
    let plan = catalog
        .preflight_import(&project.id, &mount, None, Some("Sony A7C II"), |_, _, _| {
            true
        })
        .unwrap();
    catalog
        .connection()
        .unwrap()
        .execute_batch("DROP TABLE camera_identity_mappings")
        .unwrap();

    let outcome = catalog
        .run_import(&project.id, &plan, true, |_| true)
        .unwrap();
    assert_eq!(outcome.imported, 1);
    assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 1);
    assert!(
        outcome
            .camera_mapping_warning
            .as_deref()
            .is_some_and(|warning| warning.contains("could not remember this camera choice"))
    );
}

/// The issue's named CUJ: wrong auto-detection, an explicit correction,
/// import, then a *reformatted card at a different mount path* comes
/// back and defaults to the corrected identity without the user
/// repeating themselves. Reopening the catalog between the two imports
/// stands in for an app restart: the mapping lives in SQLite, not in any
/// in-memory state this process happens to be holding.
#[test]
fn a_remembered_mapping_survives_a_reformatted_card_a_new_mount_path_and_a_restart() {
    if skip_without_ffprobe() || skip_without_exiftool() {
        return;
    }
    let directory = tempdir().unwrap();
    let catalog_path = directory.path().join("catalog.sqlite3");
    let catalog = Catalog::open(&catalog_path).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();

    let first_mount = labeled_card_fixture_media(
        &directory.path().join("first-insert"),
        "UNTITLED",
        "DCIM/100MSDCF",
        &[("C0001.MP4", "navy")],
    );
    stamp_exif(
        &first_mount,
        "DCIM/100MSDCF",
        &["C0001.MP4"],
        "SONY",
        "ILCE-7C",
    );
    let first_plan = catalog
        .preflight_import(
            &project.id,
            &first_mount,
            None,
            Some("Sony A7C II"),
            |_, _, _| true,
        )
        .unwrap();
    let first_outcome = catalog
        .run_import(&project.id, &first_plan, true, |_| true)
        .unwrap();
    assert_eq!(first_outcome.imported, 1);

    // "Restart the app": drop this handle and open a fresh one against
    // the same file, so nothing survives except what is actually
    // durable.
    drop(catalog);
    let catalog = Catalog::open(&catalog_path).unwrap();

    // A reformatted card, reinserted at a different mount path than the
    // first one and with fresh filenames, still carries the same folder
    // layout and filename prefix.
    let second_mount = labeled_card_fixture_media(
        &directory.path().join("second-insert-after-reformat"),
        "UNTITLED",
        "DCIM/100MSDCF",
        &[("C0001.MP4", "olive")],
    );
    stamp_exif(
        &second_mount,
        "DCIM/100MSDCF",
        &["C0001.MP4"],
        "SONY",
        "ILCE-7C",
    );
    assert_ne!(
        first_mount, second_mount,
        "the mount path must actually differ"
    );

    let second_plan = catalog
        .preflight_import(&project.id, &second_mount, None, None, |_, _, _| true)
        .unwrap();
    assert_eq!(
        second_plan.camera_identity,
        ResolvedImportCamera {
            camera: "Sony A7C II".to_string(),
            source: ImportCameraSource::Remembered,
            evidence_strong: true,
        },
        "a reformatted card at a new mount path must default to the remembered identity"
    );

    let second_outcome = catalog
        .run_import(&project.id, &second_plan, true, |_| true)
        .unwrap();
    assert_eq!(second_outcome.imported, 1);
    for asset in catalog.list_assets(&project.id).unwrap() {
        assert_eq!(asset.camera, "Sony A7C II");
    }
}

/// A recognizable vendor card whose representative clip has no EXIF is
/// still ambiguous: nothing here should be recorded, and
/// nothing should ever be silently offered back as a remembered
/// mapping — the dialog must keep showing this as a guess, not a fact.
#[test]
fn ambiguous_no_exif_evidence_is_never_recorded_or_applied() {
    if skip_without_ffprobe() {
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();
    let mount = labeled_card_fixture_media(
        directory.path(),
        "UNTITLED",
        "DCIM/100MSDCF",
        &[("C0001.MP4", "navy")],
    );

    let plan = catalog
        .preflight_import(&project.id, &mount, None, Some("Some Camera"), |_, _, _| {
            true
        })
        .unwrap();
    assert!(!plan.camera_identity.evidence_strong);

    catalog
        .run_import(&project.id, &plan, true, |_| true)
        .unwrap();
    assert!(
        catalog
            .list_camera_mappings(&project.id)
            .unwrap()
            .is_empty(),
        "weak evidence must never be recorded, even for an explicit user choice"
    );

    // Reinserting the very same card must not spuriously resolve to
    // "Some Camera" either — there was never strong enough evidence to
    // remember it by.
    let replan = catalog
        .preflight_import(&project.id, &mount, None, None, |_, _, _| true)
        .unwrap();
    assert_ne!(
        replan.camera_identity.source,
        ImportCameraSource::Remembered
    );
}

/// The issue's explicit negative case: a different camera that happens
/// to be mounted under the same generic volume label as a
/// previously-corrected one must not inherit that correction. The
/// mapping store never sees the volume label — only folder layout and
/// filename prefix — so a DJI card under the exact same "CARD" label a
/// Sony card used stays its own thing.
#[test]
fn a_different_camera_under_the_same_generic_label_does_not_inherit() {
    if skip_without_ffprobe() || skip_without_exiftool() {
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();

    let sony_mount = labeled_card_fixture_media(
        &directory.path().join("sony"),
        "CARD",
        "DCIM/100MSDCF",
        &[("C0001.MP4", "navy")],
    );
    stamp_exif(
        &sony_mount,
        "DCIM/100MSDCF",
        &["C0001.MP4"],
        "SONY",
        "ILCE-7C",
    );
    let sony_plan = catalog
        .preflight_import(
            &project.id,
            &sony_mount,
            None,
            Some("Sony A7C II"),
            |_, _, _| true,
        )
        .unwrap();
    catalog
        .run_import(&project.id, &sony_plan, true, |_| true)
        .unwrap();

    // A DJI Pocket 3, mounted under the identical "CARD" label.
    let dji_mount = labeled_card_fixture_media(
        &directory.path().join("dji"),
        "CARD",
        "DCIM/DJI_001",
        &[("DJI_20251221092058_0001_D.MP4", "maroon")],
    );
    stamp_exif(
        &dji_mount,
        "DCIM/DJI_001",
        &["DJI_20251221092058_0001_D.MP4"],
        "DJI",
        "Pocket 3",
    );
    assert_eq!(
        Path::new(&sony_mount).file_name(),
        Path::new(&dji_mount).file_name(),
        "both cards must share the same generic mount label for this test to prove anything"
    );

    let dji_plan = catalog
        .preflight_import(&project.id, &dji_mount, None, None, |_, _, _| true)
        .unwrap();
    assert_ne!(
        dji_plan.camera_identity.source,
        ImportCameraSource::Remembered,
        "a different camera must never inherit a mapping via a shared volume label"
    );
    assert_eq!(dji_plan.camera_identity.camera, "DJI Pocket 3");
}

fn skip_without_exiftool() -> bool {
    if crate::tool_command("exiftool")
        .arg("-ver")
        .output()
        .is_err()
    {
        eprintln!("exiftool is unavailable; skipping media integration test");
        return true;
    }
    false
}

/// Writes real EXIF Make/Model onto every clip a fixture just produced.
fn stamp_exif(mount: &Path, layout: &str, clips: &[&str], make: &str, model: &str) {
    for clip in clips {
        let status = crate::tool_command("exiftool")
            .arg("-overwrite_original")
            .arg(format!("-Make={make}"))
            .arg(format!("-Model={model}"))
            .arg(mount.join(layout).join(clip))
            .status()
            .unwrap();
        assert!(status.success(), "failed to stamp EXIF on {clip}");
    }
}

/// A detected EXIF identity is accurate but still not a user-authored
/// sticky correction. A later explicit choice is persisted under the same
/// representative EXIF evidence and wins on a matching new card.
#[test]
fn a_detected_identity_is_never_recorded_but_a_user_choice_is_and_wins_over_real_exif() {
    if skip_without_ffprobe() || skip_without_exiftool() {
        return;
    }
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Spain 2026").unwrap();

    // Card A: weak C-series filenames, real Sony EXIF. `probe_media`
    // will resolve these to the verbatim EXIF claim, not the pattern.
    let first_mount = labeled_card_fixture_media(
        &directory.path().join("card-a"),
        "UNTITLED",
        "DCIM/100MSDCF",
        &[("C0001.MP4", "navy")],
    );
    stamp_exif(
        &first_mount,
        "DCIM/100MSDCF",
        &["C0001.MP4"],
        "SONY",
        "ILCE-7C",
    );

    let first_plan = catalog
        .preflight_import(&project.id, &first_mount, None, None, |_, _, _| true)
        .unwrap();
    assert_eq!(
        first_plan.camera_identity.source,
        ImportCameraSource::Detected
    );
    assert_eq!(first_plan.camera_identity.camera, "SONY ILCE-7C");

    let first_outcome = catalog
        .run_import(&project.id, &first_plan, true, |_| true)
        .unwrap();
    assert_eq!(first_outcome.imported, 1);
    let assets = catalog.list_assets(&project.id).unwrap();
    assert_eq!(
        assets[0].camera, "SONY ILCE-7C",
        "preflight and ingest should agree on representative EXIF"
    );
    assert!(
        catalog
            .list_camera_mappings(&project.id)
            .unwrap()
            .is_empty(),
        "a plain Detected import — even one that turned out to disagree with real EXIF — must record nothing"
    );

    // Card B: same evidence (folder layout, filename prefix), reinserted
    // with nothing remembered yet, so it must still resolve on its own —
    // never `Remembered`, since nothing was ever recorded for it.
    let second_mount = labeled_card_fixture_media(
        &directory.path().join("card-b"),
        "UNTITLED",
        "DCIM/100MSDCF",
        &[("C0002.MP4", "maroon")],
    );
    stamp_exif(
        &second_mount,
        "DCIM/100MSDCF",
        &["C0002.MP4"],
        "SONY",
        "ILCE-7C",
    );
    let second_plan = catalog
        .preflight_import(&project.id, &second_mount, None, None, |_, _, _| true)
        .unwrap();
    assert_ne!(
        second_plan.camera_identity.source,
        ImportCameraSource::Remembered
    );

    // Now the user makes a real decision for this same card.
    let second_plan = catalog
        .preflight_import(
            &project.id,
            &second_mount,
            None,
            Some("Sony A7C II"),
            |_, _, _| true,
        )
        .unwrap();
    assert_eq!(
        second_plan.camera_identity.source,
        ImportCameraSource::UserSelected
    );
    catalog
        .run_import(&project.id, &second_plan, true, |_| true)
        .unwrap();
    assert_eq!(
        catalog.list_camera_mappings(&project.id).unwrap().len(),
        1,
        "a UserSelected import must record exactly one mapping"
    );

    // Card C: the remembered choice applies, and deliberately overrides
    // this card's own real EXIF — a human decision, not a pattern guess.
    let third_mount = labeled_card_fixture_media(
        &directory.path().join("card-c"),
        "UNTITLED",
        "DCIM/100MSDCF",
        &[("C0003.MP4", "olive")],
    );
    stamp_exif(
        &third_mount,
        "DCIM/100MSDCF",
        &["C0003.MP4"],
        "SONY",
        "ILCE-7C",
    );
    let third_plan = catalog
        .preflight_import(&project.id, &third_mount, None, None, |_, _, _| true)
        .unwrap();
    assert_eq!(
        third_plan.camera_identity.source,
        ImportCameraSource::Remembered
    );
    assert_eq!(third_plan.camera_identity.camera, "Sony A7C II");

    catalog
        .run_import(&project.id, &third_plan, true, |_| true)
        .unwrap();
    let third_asset = catalog
        .list_assets(&project.id)
        .unwrap()
        .into_iter()
        .find(|asset| asset.filename == "C0003.MP4")
        .unwrap();
    assert_eq!(
        third_asset.camera, "Sony A7C II",
        "a remembered user choice must win at ingest, even over this card's own real EXIF"
    );
}
