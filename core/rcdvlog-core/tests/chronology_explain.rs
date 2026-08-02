//! Acceptance oracle for chronology explanation and review-queue stability
//! (D01-T014).

use rcdvlog_core::{
    AppliedCorrection, Catalog, ClockEpoch, ConstraintStrength, DisplayMode, PhaseKind,
    ProjectAssetAddedPayload, QueuePosition, TripPhase,
};
use tempfile::{TempDir, tempdir};

struct Fixture {
    catalog: Catalog,
    project_id: String,
    _directory: TempDir,
}

/// A project of `count` clips on one camera, one clip an hour apart.
fn project(count: u64) -> Fixture {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Antarctica").unwrap();
    for index in 0..count {
        catalog
            .add_portable_project_asset(
                &project.id,
                ProjectAssetAddedPayload {
                    id: format!("asset_{index:04}"),
                    fingerprint: format!("fingerprint_{index}"),
                    filename: format!("C{index:04}.MP4"),
                    size_bytes: 1024,
                    duration_seconds: 4.0,
                    frame_rate: 30.0,
                    width: 1920,
                    height: 1080,
                    codec: "h264".to_string(),
                    camera: "SonyA7C".to_string(),
                    captured_at_raw: Some(format!("2026-01-01T{:02}:00:00Z", index % 24)),
                    best_time: None,
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

/// The review queue as the user sees it: solved order, removed clips dropped.
fn queue(fixture: &Fixture) -> Vec<String> {
    let removed: Vec<String> = fixture
        .catalog
        .list_assets(&fixture.project_id)
        .unwrap()
        .into_iter()
        .filter(|asset| asset.removed)
        .map(|asset| asset.id)
        .collect();
    fixture
        .catalog
        .solve_chronology(&fixture.project_id)
        .unwrap()
        .assets
        .into_iter()
        .map(|entry| entry.asset_id)
        .filter(|asset_id| !removed.contains(asset_id))
        .collect()
}

/// Oracle 1: the anchored asset survives a change that reorders the queue
/// around it.
#[test]
fn an_epoch_that_reorders_the_queue_does_not_lose_the_users_place() {
    let fixture = project(500);
    let ordered = queue(&fixture);
    let standing_on = ordered[250].clone();

    // Captured *before* the change. Capturing afterwards captures the damage.
    let position = fixture
        .catalog
        .capture_queue_position(&fixture.project_id, &standing_on)
        .unwrap();
    assert_eq!(position.anchor_asset_id, standing_on);
    assert_eq!(position.ordered_predecessors.len(), 10);

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
                start_counter: 250,
                end_counter: None,
                offset_seconds: -86_400,
                confidence: 1.0,
                evidence: Vec::new(),
                label: "half the trip shifted a day".to_string(),
            },
        )
        .unwrap();

    let reordered = queue(&fixture);
    assert_ne!(reordered, ordered, "the change must actually reorder");
    assert_eq!(
        fixture
            .catalog
            .restore_queue_position(&fixture.project_id, &position)
            .unwrap(),
        standing_on
    );
}

/// Oracle 2: when the anchor is gone, restore lands on the nearest surviving
/// predecessor by the pre-change order.
#[test]
fn a_removed_anchor_falls_back_to_its_nearest_surviving_predecessor() {
    let fixture = project(40);
    let ordered = queue(&fixture);
    let position = fixture
        .catalog
        .capture_queue_position(&fixture.project_id, &ordered[20])
        .unwrap();

    // The anchor and its two closest predecessors all disappear.
    for gone in [&ordered[20], &ordered[19], &ordered[18]] {
        fixture.catalog.set_removed(gone, true).unwrap();
    }

    let resumed = fixture
        .catalog
        .restore_queue_position(&fixture.project_id, &position)
        .unwrap();
    assert_eq!(
        resumed, ordered[17],
        "the nearest surviving predecessor wins"
    );
}

/// Oracle 3: when the anchor and every carried predecessor are gone, restore
/// still returns a valid asset rather than erroring.
#[test]
fn losing_the_anchor_and_all_predecessors_still_resumes_somewhere_valid() {
    let fixture = project(40);
    let ordered = queue(&fixture);
    let position = fixture
        .catalog
        .capture_queue_position(&fixture.project_id, &ordered[20])
        .unwrap();

    for gone in &ordered[10..=20] {
        fixture.catalog.set_removed(gone, true).unwrap();
    }

    let resumed = fixture
        .catalog
        .restore_queue_position(&fixture.project_id, &position)
        .unwrap();
    let surviving = queue(&fixture);
    assert!(
        surviving.contains(&resumed),
        "resumed on {resumed}, which is not in the queue"
    );
}

/// A position captured on an asset that is not in the queue at all degrades
/// gracefully rather than erroring.
#[test]
fn an_unknown_anchor_resumes_at_the_head_of_the_queue() {
    let fixture = project(5);
    let resumed = fixture
        .catalog
        .restore_queue_position(
            &fixture.project_id,
            &QueuePosition {
                anchor_asset_id: "asset_that_never_existed".to_string(),
                ordered_predecessors: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(resumed, queue(&fixture)[0]);
}

/// Oracle 4: a clip touched by all five kinds of correction reports all five,
/// in application order.
#[test]
fn all_five_correction_kinds_are_reported_in_application_order() {
    let fixture = project(4);
    let ordered = queue(&fixture);
    let subject = ordered[1].clone();
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
                start_counter: 0,
                end_counter: None,
                offset_seconds: 60,
                confidence: 0.9,
                evidence: vec!["user_set".to_string()],
                label: "camera ran a minute slow".to_string(),
            },
        )
        .unwrap();
    fixture
        .catalog
        .set_clip_time_exception(&subject, "2026-01-01T05:00:00Z", "placed by hand")
        .unwrap();
    fixture
        .catalog
        .set_trip_phase(
            &fixture.project_id,
            &TripPhase {
                id: String::new(),
                label: "Ushuaia".to_string(),
                kind: PhaseKind::Location,
                start_trip_time: "2026-01-01T00:00:00Z".to_string(),
                end_trip_time: "2026-01-02T00:00:00Z".to_string(),
                timezone: Some("America/Argentina/Ushuaia".to_string()),
                origin_timezone: None,
                destination_timezone: None,
                display_mode: DisplayMode::Local,
            },
        )
        .unwrap();
    fixture
        .catalog
        .set_event_order_constraint(
            &fixture.project_id,
            &subject,
            &ordered[3],
            ConstraintStrength::Hard,
            "user",
        )
        .unwrap();

    let explanation = fixture.catalog.explain_chronology(&subject).unwrap();
    let kinds: Vec<&str> = explanation
        .applied_corrections
        .iter()
        .map(|correction| match correction {
            AppliedCorrection::CaptureSequence { .. } => "captureSequence",
            AppliedCorrection::ClockEpoch { .. } => "clockEpoch",
            AppliedCorrection::ClipException { .. } => "clipException",
            AppliedCorrection::TripPhase { .. } => "tripPhase",
            AppliedCorrection::EventOrder { .. } => "eventOrder",
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "captureSequence",
            "clockEpoch",
            "clipException",
            "tripPhase",
            "eventOrder"
        ],
        "corrections must be listed in the solver's application order"
    );

    // The exception is user authority, so it wins the time outright.
    assert_eq!(
        explanation.trip_time.as_deref(),
        Some("2026-01-01T05:00:00.000Z")
    );
    assert_eq!(explanation.confidence, 1.0);
    assert_eq!(
        explanation.display_time.as_ref().unwrap().timezone.as_str(),
        "America/Argentina/Ushuaia"
    );
}

/// The explanation is derived, so two calls on unchanged facts are identical.
#[test]
fn explanations_are_deterministic() {
    let fixture = project(20);
    let subject = queue(&fixture)[7].clone();
    let first = fixture.catalog.explain_chronology(&subject).unwrap();
    let second = fixture.catalog.explain_chronology(&subject).unwrap();
    assert_eq!(first, second);
}
