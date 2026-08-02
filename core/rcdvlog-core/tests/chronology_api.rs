//! Acceptance oracle for the chronology write surface: capture sequences
//! (D01-T009), clock epochs (D01-T010), clip time exceptions (D01-T011), trip
//! phases (D01-T012), and constraints with contradiction blocking (D01-T013).
//!
//! Every assertion goes through the real `Catalog` seam — operations are
//! committed and materialized exactly as the desktop and CLI commit them, so a
//! materializer that disagrees with the write API fails here.

use rcdvlog_core::{
    Catalog, ChronologyError, ClockEpoch, ConstraintStrength, DisplayMode, PhaseKind,
    ProjectAssetAddedPayload, TripPhase,
};
use tempfile::tempdir;

struct Fixture {
    catalog: Catalog,
    project_id: String,
    _directory: tempfile::TempDir,
}

/// Builds a catalog with one camera's clips. `clips` is `(filename, camera,
/// captured_at_raw)`; asset ids are `asset_<filename stem>` so assertions read
/// clearly.
fn fixture(clips: &[(&str, &str, &str)]) -> Fixture {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Antarctica").unwrap();
    for (index, (filename, camera, raw)) in clips.iter().enumerate() {
        let stem = filename.split('.').next().unwrap();
        catalog
            .add_portable_project_asset(
                &project.id,
                ProjectAssetAddedPayload {
                    id: format!("asset_{stem}_{index}"),
                    fingerprint: format!("fingerprint_{index}"),
                    filename: (*filename).to_string(),
                    size_bytes: 1024,
                    duration_seconds: 4.0,
                    frame_rate: 30.0,
                    width: 1920,
                    height: 1080,
                    codec: "h264".to_string(),
                    camera: (*camera).to_string(),
                    captured_at_raw: Some((*raw).to_string()),
                    best_time: Some((*raw).to_string()),
                    metadata_rotation_degrees: 0,
                    default_rotation_degrees: 0,
                    created_at: "2026-07-20T00:00:00.000Z".to_string(),
                },
            )
            .unwrap();
    }
    Fixture {
        catalog,
        project_id: project.id,
        _directory: directory,
    }
}

fn asset_ids_in_order(fixture: &Fixture) -> Vec<String> {
    fixture
        .catalog
        .solve_chronology(&fixture.project_id)
        .unwrap()
        .assets
        .into_iter()
        .map(|entry| entry.asset_id)
        .collect()
}

/// D01-T009: a second card whose counter restarts is proposed as a split, and
/// applying the split moves exactly the clips at or above the counter.
#[test]
fn a_restarting_counter_is_proposed_as_a_split_and_splits_cleanly() {
    let fixture = fixture(&[
        ("C0001.MP4", "DJIPocket3", "2026-01-01T09:00:00Z"),
        ("C0002.MP4", "DJIPocket3", "2026-01-01T10:00:00Z"),
        // Second card: the counter starts over while time keeps moving.
        ("C0001.MP4", "DJIPocket3", "2026-01-02T09:00:00Z"),
        ("C0002.MP4", "DJIPocket3", "2026-01-02T10:00:00Z"),
    ]);

    let proposals = fixture
        .catalog
        .propose_capture_sequence_splits(&fixture.project_id)
        .unwrap();
    assert_eq!(proposals.len(), 1, "{proposals:?}");
    assert_eq!(proposals[0].at_counter, 1);

    let sequences = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap();
    assert_eq!(sequences.len(), 1, "migration groups by camera only");

    let created = fixture
        .catalog
        .split_capture_sequence(&fixture.project_id, &sequences[0].id, 2, "card two")
        .unwrap();
    let after = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap();
    assert_eq!(after.sequences.len(), 2);
    // Nothing is ever dropped by a split.
    assert_eq!(after.assets.len(), 4);
    let moved = after
        .assets
        .iter()
        .filter(|asset| asset.capture_sequence_id.as_deref() == Some(created.id.as_str()))
        .count();
    assert_eq!(moved, 2, "only counters >= 2 move");
}

/// D01-T009: merging unions the asset sets and drops nothing.
#[test]
fn merging_capture_sequences_unions_the_assets() {
    let fixture = fixture(&[
        ("C0001.MP4", "DJIPocket3", "2026-01-01T09:00:00Z"),
        ("C0005.MP4", "DJIPocket3", "2026-01-01T10:00:00Z"),
    ]);
    let sequences = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap();
    let created = fixture
        .catalog
        .split_capture_sequence(&fixture.project_id, &sequences[0].id, 5, "card two")
        .unwrap();
    assert_eq!(
        fixture
            .catalog
            .capture_sequences(&fixture.project_id)
            .unwrap()
            .len(),
        2
    );

    fixture
        .catalog
        .merge_capture_sequences(&fixture.project_id, &created.id, &sequences[0].id)
        .unwrap();
    let after = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap();
    assert_eq!(after.sequences.len(), 1);
    assert_eq!(after.assets.len(), 2);
    assert!(
        after
            .assets
            .iter()
            .all(|asset| asset.capture_sequence_id.as_deref() == Some(sequences[0].id.as_str())),
        "every asset lands in the destination sequence"
    );
}

/// D01-T010: an epoch shifts only its own counter range, and clearing it
/// restores what the remaining epochs imply rather than a snapshot.
#[test]
fn a_clock_epoch_applies_to_its_range_and_clears_cleanly() {
    let fixture = fixture(&[
        ("C0001.MP4", "DJIPocket3", "2026-01-01T09:00:00Z"),
        ("C0009.MP4", "DJIPocket3", "2026-01-01T10:00:00Z"),
    ]);
    let sequences = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap();

    let epoch = fixture
        .catalog
        .set_clock_epoch(
            &fixture.project_id,
            &ClockEpoch {
                id: String::new(),
                capture_sequence_id: sequences[0].id.clone(),
                start_counter: 5,
                end_counter: None,
                offset_seconds: -3600,
                confidence: 1.0,
                evidence: vec!["user_set".to_string()],
                label: "card two ran an hour fast".to_string(),
            },
        )
        .unwrap();

    let solution = fixture
        .catalog
        .solve_chronology(&fixture.project_id)
        .unwrap();
    let shifted = solution
        .assets
        .iter()
        .find(|entry| entry.order_key.filename_counter == Some(9))
        .unwrap();
    assert_eq!(
        shifted.trip_time.as_deref(),
        Some("2026-01-01T09:00:00.000Z")
    );
    let untouched = solution
        .assets
        .iter()
        .find(|entry| entry.order_key.filename_counter == Some(1))
        .unwrap();
    assert_eq!(
        untouched.trip_time.as_deref(),
        Some("2026-01-01T09:00:00.000Z"),
        "counter 1 is below the epoch's range and keeps its raw time"
    );

    fixture
        .catalog
        .clear_clock_epoch(&fixture.project_id, &epoch.id)
        .unwrap();
    let restored = fixture
        .catalog
        .solve_chronology(&fixture.project_id)
        .unwrap();
    let back = restored
        .assets
        .iter()
        .find(|entry| entry.order_key.filename_counter == Some(9))
        .unwrap();
    assert_eq!(back.trip_time.as_deref(), Some("2026-01-01T10:00:00.000Z"));
}

/// D01-T010: an epoch whose end precedes its start is refused before any write.
#[test]
fn an_inverted_epoch_range_is_refused() {
    let fixture = fixture(&[("C0001.MP4", "DJIPocket3", "2026-01-01T09:00:00Z")]);
    let sequences = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap();
    let error = fixture
        .catalog
        .set_clock_epoch(
            &fixture.project_id,
            &ClockEpoch {
                id: String::new(),
                capture_sequence_id: sequences[0].id.clone(),
                start_counter: 10,
                end_counter: Some(2),
                offset_seconds: 0,
                confidence: 1.0,
                evidence: Vec::new(),
                label: String::new(),
            },
        )
        .unwrap_err();
    let typed = error.downcast_ref::<ChronologyError>().unwrap();
    assert_eq!(typed.code(), "chronology.invalid_interval");
    assert!(
        fixture
            .catalog
            .chronology_facts(&fixture.project_id)
            .unwrap()
            .epochs
            .is_empty(),
        "a refused epoch writes nothing"
    );
}

/// D01-T011: a clip time exception overrides the sequence, and clearing it
/// returns the clip to what its epochs imply. The raw timestamp is never
/// modified.
#[test]
fn a_clip_time_exception_overrides_and_clears_without_touching_the_raw_time() {
    let fixture = fixture(&[
        ("C0001.MP4", "DJIPocket3", "2026-01-01T09:00:00Z"),
        ("C0002.MP4", "DJIPocket3", "2026-01-01T10:00:00Z"),
    ]);
    let target = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap()
        .assets[0]
        .asset_id
        .clone();

    fixture
        .catalog
        .set_clip_time_exception(&target, "2026-01-03T12:00:00Z", "placed by hand")
        .unwrap();
    let solution = fixture
        .catalog
        .solve_chronology(&fixture.project_id)
        .unwrap();
    let placed = solution
        .assets
        .iter()
        .find(|entry| entry.asset_id == target)
        .unwrap();
    assert_eq!(
        placed.trip_time.as_deref(),
        Some("2026-01-03T12:00:00.000Z")
    );
    assert_eq!(placed.confidence, 1.0);
    assert_eq!(
        placed.raw_time.as_deref(),
        Some("2026-01-01T09:00:00Z"),
        "the camera's own claim is never edited"
    );

    fixture.catalog.clear_clip_time_exception(&target).unwrap();
    let restored = fixture
        .catalog
        .solve_chronology(&fixture.project_id)
        .unwrap();
    let back = restored
        .assets
        .iter()
        .find(|entry| entry.asset_id == target)
        .unwrap();
    assert_eq!(back.trip_time.as_deref(), Some("2026-01-01T09:00:00.000Z"));
}

/// D01-T012: an invalid IANA identifier is refused with a stable code before
/// any write, and removing a phase retimes nothing.
#[test]
fn trip_phases_validate_timezones_and_removal_retimes_nothing() {
    let fixture = fixture(&[("C0001.MP4", "DJIPocket3", "2026-01-15T03:00:00Z")]);

    let error = fixture
        .catalog
        .set_trip_phase(
            &fixture.project_id,
            &TripPhase {
                id: String::new(),
                label: "nowhere".to_string(),
                kind: PhaseKind::Location,
                start_trip_time: "2026-01-01T00:00:00Z".to_string(),
                end_trip_time: "2026-02-01T00:00:00Z".to_string(),
                timezone: Some("Mars/Olympus_Mons".to_string()),
                origin_timezone: None,
                destination_timezone: None,
                display_mode: DisplayMode::Local,
            },
        )
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<ChronologyError>().unwrap().code(),
        "chronology.invalid_timezone"
    );
    assert!(
        fixture
            .catalog
            .trip_phases(&fixture.project_id)
            .unwrap()
            .is_empty()
    );

    let phase = fixture
        .catalog
        .set_trip_phase(
            &fixture.project_id,
            &TripPhase {
                id: String::new(),
                label: "Tokyo".to_string(),
                kind: PhaseKind::Location,
                start_trip_time: "2026-01-01T00:00:00Z".to_string(),
                end_trip_time: "2026-02-01T00:00:00Z".to_string(),
                timezone: Some("Asia/Tokyo".to_string()),
                origin_timezone: None,
                destination_timezone: None,
                display_mode: DisplayMode::Local,
            },
        )
        .unwrap();
    let covered = fixture
        .catalog
        .solve_chronology(&fixture.project_id)
        .unwrap();
    assert_eq!(
        covered.assets[0].display_time.as_ref().unwrap().primary,
        "2026-01-15T12:00:00+09:00"
    );
    let trip_time_before = covered.assets[0].trip_time.clone();

    fixture
        .catalog
        .remove_trip_phase(&fixture.project_id, &phase.id)
        .unwrap();
    let uncovered = fixture
        .catalog
        .solve_chronology(&fixture.project_id)
        .unwrap();
    assert_eq!(
        uncovered.assets[0].trip_time, trip_time_before,
        "removing a phase changes display only, never trip time"
    );
    assert_eq!(
        uncovered.assets[0].display_time.as_ref().unwrap().phase_id,
        None
    );
}

/// D01-T013: a hard constraint that would create a cycle is refused, and
/// nothing is written. A soft one in the same shape is accepted.
#[test]
fn a_cycle_forming_hard_constraint_is_refused_without_writing() {
    let fixture = fixture(&[
        ("C0001.MP4", "DJIPocket3", "2026-01-01T09:00:00Z"),
        ("C0002.MP4", "DJIPocket3", "2026-01-01T10:00:00Z"),
    ]);
    let assets = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap()
        .assets;
    let (first, second) = (assets[0].asset_id.clone(), assets[1].asset_id.clone());

    fixture
        .catalog
        .set_event_order_constraint(
            &fixture.project_id,
            &first,
            &second,
            ConstraintStrength::Hard,
            "user",
        )
        .unwrap();

    let error = fixture
        .catalog
        .set_event_order_constraint(
            &fixture.project_id,
            &second,
            &first,
            ConstraintStrength::Hard,
            "user",
        )
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<ChronologyError>().unwrap().code(),
        "chronology.hard_contradiction"
    );
    assert_eq!(
        fixture
            .catalog
            .chronology_facts(&fixture.project_id)
            .unwrap()
            .constraints
            .len(),
        1,
        "the refused constraint wrote nothing"
    );
    assert!(
        fixture
            .catalog
            .contradictions(&fixture.project_id)
            .unwrap()
            .is_empty()
    );

    // The same pair as a soft constraint is a hint, so it is never blocked.
    fixture
        .catalog
        .set_event_order_constraint(
            &fixture.project_id,
            &second,
            &first,
            ConstraintStrength::Soft,
            "guess",
        )
        .unwrap();
    assert!(
        fixture
            .catalog
            .contradictions(&fixture.project_id)
            .unwrap()
            .is_empty()
    );
}

/// D01-T013: the impact preview reports what an epoch would do without
/// writing it.
#[test]
fn the_impact_preview_writes_nothing() {
    let fixture = fixture(&[
        ("C0001.MP4", "DJIPocket3", "2026-01-01T09:00:00Z"),
        ("C0002.MP4", "DJIPocket3", "2026-01-01T10:00:00Z"),
    ]);
    let assets = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap()
        .assets;
    let sequences = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap();
    fixture
        .catalog
        .set_event_order_constraint(
            &fixture.project_id,
            &assets[0].asset_id,
            &assets[1].asset_id,
            ConstraintStrength::Hard,
            "user",
        )
        .unwrap();

    // Shifting the first clip a day forward puts it after the second, which
    // the hard constraint forbids.
    let candidate = ClockEpoch {
        id: "epoch_candidate".to_string(),
        capture_sequence_id: sequences[0].id.clone(),
        start_counter: 1,
        end_counter: Some(1),
        offset_seconds: 86_400,
        confidence: 1.0,
        evidence: Vec::new(),
        label: "candidate".to_string(),
    };
    let predicted = fixture
        .catalog
        .preview_clock_epoch(&fixture.project_id, &candidate)
        .unwrap();
    assert_eq!(predicted.len(), 1, "{predicted:?}");
    assert!(
        fixture
            .catalog
            .chronology_facts(&fixture.project_id)
            .unwrap()
            .epochs
            .is_empty(),
        "a preview writes nothing"
    );
}

/// The projection helper is the sole writer of `best_time` and
/// `offset_seconds`, and re-solving is idempotent.
#[test]
fn projection_writes_the_solution_and_is_idempotent() {
    let fixture = fixture(&[
        ("C0001.MP4", "DJIPocket3", "2026-01-01T09:00:00Z"),
        ("C0009.MP4", "DJIPocket3", "2026-01-01T10:00:00Z"),
    ]);
    let sequences = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap();
    fixture
        .catalog
        .set_clock_epoch(
            &fixture.project_id,
            &ClockEpoch {
                id: String::new(),
                capture_sequence_id: sequences[0].id.clone(),
                start_counter: 5,
                end_counter: None,
                offset_seconds: -3600,
                confidence: 1.0,
                evidence: Vec::new(),
                label: String::new(),
            },
        )
        .unwrap();

    let ordered_before = asset_ids_in_order(&fixture);
    fixture
        .catalog
        .resolve_chronology(&fixture.project_id)
        .unwrap();
    let shifted = fixture
        .catalog
        .list_assets(&fixture.project_id)
        .unwrap()
        .into_iter()
        .find(|asset| asset.filename == "C0009.MP4")
        .unwrap();
    assert_eq!(
        shifted.best_time.as_deref(),
        Some("2026-01-01T09:00:00.000Z")
    );
    assert_eq!(shifted.offset_seconds, -3600);

    // Projecting again changes nothing, so the columns stay reproducible.
    fixture
        .catalog
        .resolve_chronology(&fixture.project_id)
        .unwrap();
    assert_eq!(asset_ids_in_order(&fixture), ordered_before);
}
