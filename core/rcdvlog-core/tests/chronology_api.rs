//! Acceptance oracle for the chronology write surface: capture sequences
//! (D01-T009), clock epochs (D01-T010), clip time exceptions (D01-T011), trip
//! phases (D01-T012), and constraints with contradiction blocking (D01-T013).
//!
//! Every assertion goes through the real `Catalog` seam — operations are
//! committed and materialized exactly as the desktop and CLI commit them, so a
//! materializer that disagrees with the write API fails here.

use rcdvlog_core::{
    AnchorKind, ApplyOperationResult, CAPTURE_SEQUENCE_MERGED, CAPTURE_SEQUENCE_REASSIGNED,
    CAPTURE_SEQUENCE_SPLIT_EXACT, Catalog, ChronologyError, ChronologyImpactChange,
    ChronologyMutation, ChronologySolution, ClockAlternative, ClockBlockQuestion, ClockEpoch,
    ClockEpochImpactPreview, ClockQuestionDecision, ClockQuestionDecisionKind, ConstraintStrength,
    DisplayMode, HumanAnchor, PhaseKind, PhaseSplitDirection, ProjectAssetAddedPayload,
    ProxyExcerpt, TripPhase, VisualPlacementSide, solve,
};
use serde_json::json;
use tempfile::tempdir;

#[test]
fn timestamp_candidates_survive_operation_replay_and_portable_baseline() {
    let fixture = fixture(&[]);
    let candidate = rcdvlog_core::TimestampCandidate {
        origin: "ffprobe.format.tags.creation_time".into(),
        original_text: "2030-01-02T03:04:05Z".into(),
        parsed_value: Some("2030-01-02T03:04:05+00:00".into()),
        timezone_claim: Some("Z".into()),
        parse_status: "parsed".into(),
        semantics: Some("container_creation_time".into()),
    };
    fixture
        .catalog
        .add_portable_project_asset(
            &fixture.project_id,
            ProjectAssetAddedPayload {
                id: "asset_evidence".into(),
                fingerprint: "fingerprint_evidence".into(),
                filename: "20300102030415_clip.mp4".into(),
                size_bytes: 1,
                duration_seconds: 10.0,
                frame_rate: 30.0,
                width: 1,
                height: 1,
                codec: "h264".into(),
                camera: "Synthetic".into(),
                captured_at_raw: Some(candidate.original_text.clone()),
                timestamp_candidates: Some(vec![candidate.clone()]),
                best_time: Some(candidate.parsed_value.clone().unwrap()),
                metadata_rotation_degrees: 0,
                default_rotation_degrees: 0,
                created_at: "2030-01-02T00:00:00Z".into(),
            },
        )
        .unwrap();
    let page = fixture
        .catalog
        .chronology_evidence_page(&fixture.project_id, None, 0, 1)
        .unwrap();
    assert_eq!(
        page.assets[0].timestamp_candidates.as_deref(),
        Some(&[candidate.clone()][..])
    );

    let snapshot = fixture
        .catalog
        .export_project_baseline(&fixture.project_id)
        .unwrap();
    let directory = tempdir().unwrap();
    let restored = Catalog::import_project_baseline(
        directory.path().join("restored.sqlite3"),
        &snapshot,
        "synthetic-node",
        1,
        "digest",
    )
    .unwrap();
    let restored_page = restored
        .chronology_evidence_page(&fixture.project_id, None, 0, 1)
        .unwrap();
    assert_eq!(
        restored_page.assets[0].timestamp_candidates.as_deref(),
        Some(&[candidate][..])
    );
}

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
                    timestamp_candidates: None,
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

fn chronology_error_code(error: &anyhow::Error) -> &'static str {
    error
        .downcast_ref::<ChronologyError>()
        .expect("chronology methods return a typed chronology error")
        .code()
}

fn impact_changes(
    before: &ChronologySolution,
    after: &ChronologySolution,
) -> Vec<ChronologyImpactChange> {
    after
        .assets
        .iter()
        .enumerate()
        .filter_map(|(after_position, asset)| {
            let (before_position, previous) = before
                .assets
                .iter()
                .enumerate()
                .find(|(_, candidate)| candidate.asset_id == asset.asset_id)?;
            (before_position != after_position || previous.trip_time != asset.trip_time).then(
                || ChronologyImpactChange {
                    asset_id: asset.asset_id.clone(),
                    before: previous.trip_time.clone(),
                    after: asset.trip_time.clone(),
                    before_position,
                    after_position,
                },
            )
        })
        .collect()
}

fn add_portable_asset(
    fixture: &Fixture,
    id: &str,
    filename: &str,
    camera: &str,
    captured_at_raw: &str,
) {
    fixture
        .catalog
        .add_portable_project_asset(
            &fixture.project_id,
            ProjectAssetAddedPayload {
                id: id.to_string(),
                fingerprint: format!("fingerprint_{id}"),
                filename: filename.to_string(),
                size_bytes: 1024,
                duration_seconds: 4.0,
                frame_rate: 30.0,
                width: 1920,
                height: 1080,
                codec: "h264".to_string(),
                camera: camera.to_string(),
                captured_at_raw: Some(captured_at_raw.to_string()),
                timestamp_candidates: None,
                best_time: Some(captured_at_raw.to_string()),
                metadata_rotation_degrees: 0,
                default_rotation_degrees: 0,
                created_at: "2026-07-20T00:00:00.000Z".to_string(),
            },
        )
        .unwrap();
}

/// Swapping several cards through one camera is ordinary practice, and so is a
/// camera that simply rolls its counter over. Declining a boundary has to be a
/// durable answer, or the desk asks the same question forever and the only way
/// to silence it is to accept a split the user does not want.
#[test]
fn a_declined_counter_restart_stops_being_proposed() {
    let fixture = fixture(&[
        ("C0001.MP4", "DJIPocket3", "2026-01-01T09:00:00Z"),
        ("C0002.MP4", "DJIPocket3", "2026-01-01T10:00:00Z"),
        ("C0001.MP4", "DJIPocket3", "2026-01-02T09:00:00Z"),
        ("C0002.MP4", "DJIPocket3", "2026-01-02T10:00:00Z"),
    ]);
    let sequence_id = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap()[0]
        .id
        .clone();

    fixture
        .catalog
        .decline_capture_sequence_split(
            &fixture.project_id,
            &sequence_id,
            1,
            "one card, counter rolled over",
        )
        .unwrap();

    assert!(
        fixture
            .catalog
            .propose_capture_sequence_splits(&fixture.project_id)
            .unwrap()
            .is_empty(),
        "a declined boundary must not come back"
    );

    let declined = fixture
        .catalog
        .declined_capture_sequence_splits(&fixture.project_id)
        .unwrap();
    assert_eq!(declined.len(), 1);
    assert_eq!(declined[0].at_counter, 1);
    assert_eq!(declined[0].reason, "one card, counter rolled over");
    assert_eq!(declined[0].capture_sequence_id, sequence_id);
}

/// The decision must be reversible: it is one click, and the user who makes it
/// by mistake needs a way back that does not involve editing the catalog.
#[test]
fn a_declined_counter_restart_can_be_restored() {
    let fixture = fixture(&[
        ("C0001.MP4", "DJIPocket3", "2026-01-01T09:00:00Z"),
        ("C0002.MP4", "DJIPocket3", "2026-01-01T10:00:00Z"),
        ("C0001.MP4", "DJIPocket3", "2026-01-02T09:00:00Z"),
    ]);
    let sequence_id = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap()[0]
        .id
        .clone();

    fixture
        .catalog
        .decline_capture_sequence_split(&fixture.project_id, &sequence_id, 1, "")
        .unwrap();
    fixture
        .catalog
        .restore_capture_sequence_split(&fixture.project_id, &sequence_id, 1)
        .unwrap();

    let proposals = fixture
        .catalog
        .propose_capture_sequence_splits(&fixture.project_id)
        .unwrap();
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].at_counter, 1);
    assert!(
        fixture
            .catalog
            .declined_capture_sequence_splits(&fixture.project_id)
            .unwrap()
            .is_empty()
    );
}

/// A trip that burns three cards through one camera has two restarts. Declining
/// the first has to reveal the second rather than silencing the sequence: the
/// scan stops at the first *undeclined* boundary, not the first boundary.
#[test]
fn declining_one_boundary_reveals_the_next_restart() {
    let fixture = fixture(&[
        ("C0001.MP4", "DJIPocket3", "2026-01-01T09:00:00Z"),
        ("C0002.MP4", "DJIPocket3", "2026-01-01T10:00:00Z"),
        // Card two.
        ("C0001.MP4", "DJIPocket3", "2026-01-02T09:00:00Z"),
        ("C0005.MP4", "DJIPocket3", "2026-01-02T10:00:00Z"),
        // Card three restarts again, at a different counter.
        ("C0003.MP4", "DJIPocket3", "2026-01-03T09:00:00Z"),
    ]);
    let sequence_id = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap()[0]
        .id
        .clone();

    assert_eq!(
        fixture
            .catalog
            .propose_capture_sequence_splits(&fixture.project_id)
            .unwrap()[0]
            .at_counter,
        1
    );

    fixture
        .catalog
        .decline_capture_sequence_split(&fixture.project_id, &sequence_id, 1, "same card")
        .unwrap();

    let next = fixture
        .catalog
        .propose_capture_sequence_splits(&fixture.project_id)
        .unwrap();
    assert_eq!(
        next.len(),
        1,
        "the later restart is now the open question: {next:?}"
    );
    assert_eq!(next[0].at_counter, 3);
    assert_eq!(next[0].affected_asset_ids, ["asset_C0003_4".to_string()]);
}

/// Two devices declining the same boundary, or one log replayed twice, must
/// converge on a single decision rather than erroring or duplicating.
#[test]
fn declining_the_same_boundary_twice_is_idempotent() {
    let fixture = fixture(&[
        ("C0001.MP4", "DJIPocket3", "2026-01-01T09:00:00Z"),
        ("C0001.MP4", "DJIPocket3", "2026-01-02T09:00:00Z"),
    ]);
    let sequence_id = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap()[0]
        .id
        .clone();

    fixture
        .catalog
        .decline_capture_sequence_split(&fixture.project_id, &sequence_id, 1, "first answer")
        .unwrap();
    fixture
        .catalog
        .decline_capture_sequence_split(&fixture.project_id, &sequence_id, 1, "same answer again")
        .unwrap();

    let declined = fixture
        .catalog
        .declined_capture_sequence_splits(&fixture.project_id)
        .unwrap();
    assert_eq!(declined.len(), 1);
    assert_eq!(declined[0].reason, "same answer again");
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
    assert_eq!(
        proposals[0].affected_asset_ids,
        ["asset_C0001_2".to_string(), "asset_C0002_3".to_string()],
        "a restarted counter selects the later card, not matching counters from both cards"
    );

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

#[test]
fn exact_split_preview_is_read_only_and_applies_the_previewed_card_and_identity() {
    let fixture = fixture(&[
        ("C0001.MP4", "DJIPocket3", "2026-01-01T09:00:00Z"),
        ("C0002.MP4", "DJIPocket3", "2026-01-01T10:00:00Z"),
        ("C0001.MP4", "DJIPocket3", "2026-01-02T09:00:00Z"),
        ("C0002.MP4", "DJIPocket3", "2026-01-02T10:00:00Z"),
    ]);
    let proposal = fixture
        .catalog
        .propose_capture_sequence_splits(&fixture.project_id)
        .unwrap()
        .remove(0);
    let source = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap()
        .remove(0);
    fixture
        .catalog
        .set_clock_epoch(
            &fixture.project_id,
            &ClockEpoch {
                id: "epoch_before_exact_split".to_string(),
                capture_sequence_id: source.id.clone(),
                start_counter: 1,
                end_counter: Some(2),
                offset_seconds: 3_600,
                confidence: 1.0,
                evidence: vec!["user_set".to_string()],
                label: "both cards inherited one offset".to_string(),
            },
        )
        .unwrap();
    let facts_before = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap();
    let solution_before = solve(&facts_before);
    let operations_before = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap();

    let preview = fixture
        .catalog
        .preview_capture_sequence_split(
            &fixture.project_id,
            &source.id,
            proposal.at_counter,
            "second card",
            &proposal.affected_asset_ids,
        )
        .unwrap();
    assert_eq!(preview.source_sequence_id, source.id);
    assert_eq!(preview.at_counter, 1);
    assert_eq!(preview.asset_ids, proposal.affected_asset_ids);
    assert_eq!(preview.created_sequence.label, "second card");
    assert_ne!(preview.created_sequence.id, source.id);
    assert!(!preview.impact.changes.is_empty());
    assert_eq!(
        fixture
            .catalog
            .chronology_facts(&fixture.project_id)
            .unwrap(),
        facts_before,
        "split preview must not create a sequence or move a clip"
    );
    assert_eq!(
        fixture
            .catalog
            .list_operations(&fixture.project_id)
            .unwrap(),
        operations_before,
        "split preview must not emit an operation"
    );

    let created = fixture
        .catalog
        .apply_capture_sequence_split(&fixture.project_id, &preview)
        .unwrap();
    assert_eq!(created, preview.created_sequence);
    let facts_after = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap();
    let moved: Vec<_> = facts_after
        .assets
        .iter()
        .filter(|asset| asset.capture_sequence_id.as_deref() == Some(created.id.as_str()))
        .map(|asset| asset.asset_id.as_str())
        .collect();
    assert_eq!(moved, ["asset_C0001_2", "asset_C0002_3"]);
    for asset_id in ["asset_C0001_0", "asset_C0002_1"] {
        assert_eq!(
            facts_after
                .assets
                .iter()
                .find(|asset| asset.asset_id == asset_id)
                .unwrap()
                .capture_sequence_id
                .as_deref(),
            Some(source.id.as_str()),
            "matching counters from the first card must stay in the source"
        );
    }
    let solution_after = solve(&facts_after);
    assert_eq!(
        impact_changes(&solution_before, &solution_after),
        preview.impact.changes,
        "the applied split must produce exactly the previewed solver diff"
    );
    let operations_after = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap();
    let new_operations = &operations_after[operations_before.len()..];
    assert_eq!(new_operations.len(), 1);
    assert_eq!(
        new_operations[0].operation_type,
        CAPTURE_SEQUENCE_SPLIT_EXACT
    );
    assert_eq!(new_operations[0].aggregate_id, source.id);
    assert_eq!(new_operations[0].payload["newSequenceId"], created.id);
    assert_eq!(
        new_operations[0].payload["assetIds"],
        json!(preview.asset_ids),
        "the durable split expansion is the exact confirmed identity set"
    );

    let replay_fixture = crate::fixture(&[
        ("C0001.MP4", "DJIPocket3", "2026-01-01T09:00:00Z"),
        ("C0002.MP4", "DJIPocket3", "2026-01-01T10:00:00Z"),
        ("C0001.MP4", "DJIPocket3", "2026-01-02T09:00:00Z"),
        ("C0002.MP4", "DJIPocket3", "2026-01-02T10:00:00Z"),
    ]);
    let replay_source = replay_fixture
        .catalog
        .capture_sequences(&replay_fixture.project_id)
        .unwrap()
        .remove(0);
    add_portable_asset(
        &replay_fixture,
        "asset_C0002_discovered_later",
        "C0002.MP4",
        "DJIPocket3",
        "2026-01-03T10:00:00Z",
    );
    let mut replay = new_operations[0].clone();
    replay.operation_id = "operation_replayed_exact_split".to_string();
    replay.project_id = replay_fixture.project_id.clone();
    replay.aggregate_id = replay_source.id.clone();
    assert_eq!(
        replay_fixture.catalog.apply_operation(&replay).unwrap(),
        ApplyOperationResult::Applied
    );
    assert_eq!(
        replay_fixture.catalog.apply_operation(&replay).unwrap(),
        ApplyOperationResult::Duplicate
    );
    let replayed = replay_fixture
        .catalog
        .chronology_facts(&replay_fixture.project_id)
        .unwrap();
    assert_eq!(
        replayed
            .assets
            .iter()
            .find(|asset| asset.asset_id == "asset_C0002_discovered_later")
            .unwrap()
            .capture_sequence_id
            .as_deref(),
        Some(replay_source.id.as_str()),
        "replay must not sweep a later matching counter into the confirmed card"
    );
    for asset_id in ["asset_C0001_2", "asset_C0002_3"] {
        assert_eq!(
            replayed
                .assets
                .iter()
                .find(|asset| asset.asset_id == asset_id)
                .unwrap()
                .capture_sequence_id
                .as_deref(),
            Some(created.id.as_str())
        );
    }
}

#[test]
fn exact_split_confirmation_refuses_changed_facts_without_partial_writes() {
    let fixture = fixture(&[
        ("C0001.MP4", "Sony", "2026-01-01T09:00:00Z"),
        ("C0002.MP4", "Sony", "2026-01-01T10:00:00Z"),
    ]);
    let source = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap()
        .remove(0);
    let preview = fixture
        .catalog
        .preview_capture_sequence_split(
            &fixture.project_id,
            &source.id,
            2,
            "card two",
            &["asset_C0002_1".to_string()],
        )
        .unwrap();
    add_portable_asset(
        &fixture,
        "asset_C0003_after_preview",
        "C0003.MP4",
        "Sony",
        "2026-01-01T11:00:00Z",
    );
    let operations_before_apply = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap();

    let error = fixture
        .catalog
        .apply_capture_sequence_split(&fixture.project_id, &preview)
        .unwrap_err();
    assert_eq!(
        chronology_error_code(&error),
        "chronology.invalid_reference"
    );
    assert!(format!("{error:#}").contains("preview is stale"));
    let facts = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap();
    assert!(
        facts
            .sequences
            .iter()
            .all(|sequence| sequence.id != preview.created_sequence.id),
        "a stale confirmation must not leave its declared sequence behind"
    );
    assert!(
        facts
            .assets
            .iter()
            .all(|asset| { asset.capture_sequence_id.as_deref() == Some(source.id.as_str()) })
    );
    assert_eq!(
        fixture
            .catalog
            .list_operations(&fixture.project_id)
            .unwrap(),
        operations_before_apply,
        "a stale exact split writes neither half of its transaction"
    );
}

#[test]
fn exact_split_materialization_refuses_an_invalid_identity_atomically() {
    let clips = [
        ("C0001.MP4", "Sony", "2026-01-01T09:00:00Z"),
        ("C0002.MP4", "Sony", "2026-01-01T10:00:00Z"),
    ];
    let author = fixture(&clips);
    let author_source = author
        .catalog
        .capture_sequences(&author.project_id)
        .unwrap()
        .remove(0);
    let preview = author
        .catalog
        .preview_capture_sequence_split(
            &author.project_id,
            &author_source.id,
            2,
            "card two",
            &["asset_C0002_1".to_string()],
        )
        .unwrap();
    author
        .catalog
        .apply_capture_sequence_split(&author.project_id, &preview)
        .unwrap();
    let operation = author
        .catalog
        .list_operations(&author.project_id)
        .unwrap()
        .into_iter()
        .find(|operation| operation.operation_type == CAPTURE_SEQUENCE_SPLIT_EXACT)
        .unwrap();

    let replica = crate::fixture(&clips);
    let replica_source = replica
        .catalog
        .capture_sequences(&replica.project_id)
        .unwrap()
        .remove(0);
    let operations_before = replica
        .catalog
        .list_operations(&replica.project_id)
        .unwrap();
    let mut invalid = operation;
    invalid.operation_id = "operation_invalid_exact_split".to_string();
    invalid.project_id = replica.project_id.clone();
    invalid.aggregate_id = replica_source.id.clone();
    invalid.payload["assetIds"] = json!(["asset_C0002_1", "asset_missing"]);

    let error = replica.catalog.apply_operation(&invalid).unwrap_err();
    assert!(format!("{error:#}").contains("asset_missing"));
    let facts = replica
        .catalog
        .chronology_facts(&replica.project_id)
        .unwrap();
    assert_eq!(
        facts.sequences.as_slice(),
        std::slice::from_ref(&replica_source)
    );
    assert!(
        facts
            .assets
            .iter()
            .all(|asset| asset.capture_sequence_id.as_deref() == Some(replica_source.id.as_str()))
    );
    assert_eq!(
        replica
            .catalog
            .list_operations(&replica.project_id)
            .unwrap(),
        operations_before,
        "the rejected operation row and every partial materialization roll back together"
    );
}

#[test]
fn exact_merge_preview_is_read_only_and_apply_matches_assets_epochs_and_impact() {
    let fixture = fixture(&[
        ("C0001.MP4", "Sony", "2026-01-01T09:00:00Z"),
        ("D0001.MP4", "DJI", "2026-01-01T10:00:00Z"),
    ]);
    let sequences = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap();
    let source = sequences
        .iter()
        .find(|sequence| sequence.camera_key == "Sony")
        .unwrap();
    let destination = sequences
        .iter()
        .find(|sequence| sequence.camera_key == "DJI")
        .unwrap();
    let epoch = fixture
        .catalog
        .set_clock_epoch(
            &fixture.project_id,
            &ClockEpoch {
                id: "epoch_merge_source".to_string(),
                capture_sequence_id: source.id.clone(),
                start_counter: 1,
                end_counter: None,
                offset_seconds: 3_600,
                confidence: 1.0,
                evidence: vec!["user_set".to_string()],
                label: "Sony clock".to_string(),
            },
        )
        .unwrap();
    let facts_before = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap();
    let solution_before = solve(&facts_before);
    let operations_before = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap();

    let preview = fixture
        .catalog
        .preview_capture_sequence_merge(&fixture.project_id, &source.id, &destination.id)
        .unwrap();
    assert_eq!(preview.asset_ids, ["asset_C0001_0"]);
    assert_eq!(
        preview.epoch_ids.as_slice(),
        std::slice::from_ref(&epoch.id)
    );
    assert!(!preview.impact.changes.is_empty());
    assert_eq!(
        fixture
            .catalog
            .chronology_facts(&fixture.project_id)
            .unwrap(),
        facts_before
    );
    assert_eq!(
        fixture
            .catalog
            .list_operations(&fixture.project_id)
            .unwrap(),
        operations_before
    );

    fixture
        .catalog
        .apply_capture_sequence_merge(&fixture.project_id, &preview)
        .unwrap();
    let facts_after = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap();
    assert!(
        facts_after
            .sequences
            .iter()
            .all(|sequence| sequence.id != source.id)
    );
    assert!(
        facts_after
            .assets
            .iter()
            .all(|asset| { asset.capture_sequence_id.as_deref() == Some(destination.id.as_str()) })
    );
    assert_eq!(facts_after.epochs[0].capture_sequence_id, destination.id);
    assert_eq!(
        impact_changes(&solution_before, &solve(&facts_after)),
        preview.impact.changes
    );
    let operations_after = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap();
    assert_eq!(operations_after.len(), operations_before.len() + 1);
    let operation = operations_after.last().unwrap();
    assert_eq!(operation.operation_type, CAPTURE_SEQUENCE_MERGED);
    assert_eq!(
        operation.payload,
        json!({
            "fromSequenceId": source.id,
            "intoSequenceId": destination.id,
            "assetIds": preview.asset_ids,
            "epochIds": preview.epoch_ids,
        }),
        "the durable merge records exactly the facts the user confirmed"
    );

    let replica = crate::fixture(&[
        ("C0001.MP4", "Sony", "2026-01-01T09:00:00Z"),
        ("D0001.MP4", "DJI", "2026-01-01T10:00:00Z"),
    ]);
    let replica_sequences = replica
        .catalog
        .capture_sequences(&replica.project_id)
        .unwrap();
    let replica_source = replica_sequences
        .iter()
        .find(|sequence| sequence.camera_key == "Sony")
        .unwrap();
    let replica_destination = replica_sequences
        .iter()
        .find(|sequence| sequence.camera_key == "DJI")
        .unwrap();
    replica
        .catalog
        .set_clock_epoch(
            &replica.project_id,
            &ClockEpoch {
                id: "epoch_merge_source".to_string(),
                capture_sequence_id: replica_source.id.clone(),
                start_counter: 1,
                end_counter: None,
                offset_seconds: 3_600,
                confidence: 1.0,
                evidence: vec!["user_set".to_string()],
                label: "Sony clock".to_string(),
            },
        )
        .unwrap();
    add_portable_asset(
        &replica,
        "asset_C0002_discovered_later",
        "C0002.MP4",
        "Sony",
        "2026-01-01T11:00:00Z",
    );
    replica
        .catalog
        .set_clock_epoch(
            &replica.project_id,
            &ClockEpoch {
                id: "epoch_merge_discovered_later".to_string(),
                capture_sequence_id: replica_source.id.clone(),
                start_counter: 2,
                end_counter: None,
                offset_seconds: 120,
                confidence: 1.0,
                evidence: vec!["later".to_string()],
                label: "later fact".to_string(),
            },
        )
        .unwrap();
    let mut replay = operation.clone();
    replay.operation_id = "operation_replayed_exact_merge".to_string();
    replay.project_id = replica.project_id.clone();
    replay.aggregate_id = replica_destination.id.clone();
    replay.payload["fromSequenceId"] = json!(replica_source.id);
    replay.payload["intoSequenceId"] = json!(replica_destination.id);
    assert_eq!(
        replica.catalog.apply_operation(&replay).unwrap(),
        ApplyOperationResult::Applied
    );
    let replayed = replica
        .catalog
        .chronology_facts(&replica.project_id)
        .unwrap();
    assert_eq!(
        replayed
            .assets
            .iter()
            .find(|asset| asset.asset_id == "asset_C0001_0")
            .unwrap()
            .capture_sequence_id
            .as_deref(),
        Some(replica_destination.id.as_str())
    );
    assert_eq!(
        replayed
            .assets
            .iter()
            .find(|asset| asset.asset_id == "asset_C0002_discovered_later")
            .unwrap()
            .capture_sequence_id
            .as_deref(),
        Some(replica_source.id.as_str()),
        "a later asset stays in the source instead of being swept into an older merge"
    );
    assert_eq!(
        replayed
            .epochs
            .iter()
            .find(|epoch| epoch.id == "epoch_merge_source")
            .unwrap()
            .capture_sequence_id,
        replica_destination.id
    );
    assert_eq!(
        replayed
            .epochs
            .iter()
            .find(|epoch| epoch.id == "epoch_merge_discovered_later")
            .unwrap()
            .capture_sequence_id,
        replica_source.id
    );
    assert!(
        replayed
            .sequences
            .iter()
            .any(|sequence| sequence.id == replica_source.id),
        "the source sequence remains while later unconfirmed facts still belong to it"
    );
}

#[test]
fn exact_merge_confirmation_refuses_a_stale_preview() {
    let fixture = fixture(&[
        ("C0001.MP4", "Sony", "2026-01-01T09:00:00Z"),
        ("D0001.MP4", "DJI", "2026-01-01T10:00:00Z"),
    ]);
    let sequences = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap();
    let source = sequences
        .iter()
        .find(|sequence| sequence.camera_key == "Sony")
        .unwrap();
    let destination = sequences
        .iter()
        .find(|sequence| sequence.camera_key == "DJI")
        .unwrap();
    let preview = fixture
        .catalog
        .preview_capture_sequence_merge(&fixture.project_id, &source.id, &destination.id)
        .unwrap();
    fixture
        .catalog
        .set_clock_epoch(
            &fixture.project_id,
            &ClockEpoch {
                id: "epoch_after_merge_preview".to_string(),
                capture_sequence_id: source.id.clone(),
                start_counter: 1,
                end_counter: None,
                offset_seconds: 60,
                confidence: 1.0,
                evidence: vec!["user_set".to_string()],
                label: "late evidence".to_string(),
            },
        )
        .unwrap();
    let operations_before_apply = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap();

    let error = fixture
        .catalog
        .apply_capture_sequence_merge(&fixture.project_id, &preview)
        .unwrap_err();
    assert_eq!(
        chronology_error_code(&error),
        "chronology.invalid_reference"
    );
    assert!(format!("{error:#}").contains("preview is stale"));
    assert_eq!(
        fixture
            .catalog
            .list_operations(&fixture.project_id)
            .unwrap(),
        operations_before_apply
    );
    assert!(
        fixture
            .catalog
            .capture_sequences(&fixture.project_id)
            .unwrap()
            .iter()
            .any(|sequence| sequence.id == source.id)
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

    let operations_before = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap();
    let preview_error = fixture
        .catalog
        .preview_clock_epoch_impact(
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
    assert_eq!(
        chronology_error_code(&preview_error),
        "chronology.invalid_interval"
    );
    assert_eq!(
        fixture
            .catalog
            .list_operations(&fixture.project_id)
            .unwrap(),
        operations_before,
        "a refused preview allocates neither epoch identity nor operation"
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
                start_anchor_asset_id: None,
                end_anchor_asset_id: None,
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
                start_anchor_asset_id: None,
                end_anchor_asset_id: None,
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

#[test]
fn a_visible_trip_phase_can_be_cut_from_either_side_atomically() {
    let fixture = fixture(&[
        ("C0001.MP4", "DJIPocket3", "2026-01-15T03:00:00Z"),
        ("C0002.MP4", "DJIPocket3", "2026-01-15T04:00:00Z"),
        ("C0003.MP4", "DJIPocket3", "2026-01-15T05:00:00Z"),
        ("C0004.MP4", "DJIPocket3", "2026-01-15T06:00:00Z"),
    ]);
    let ids = asset_ids_in_order(&fixture);
    let phase = fixture
        .catalog
        .set_trip_phase(
            &fixture.project_id,
            &TripPhase {
                id: String::new(),
                label: "Tokyo".to_string(),
                kind: PhaseKind::Location,
                start_anchor_asset_id: Some(ids[0].clone()),
                end_anchor_asset_id: Some(ids[3].clone()),
                start_trip_time: String::new(),
                end_trip_time: String::new(),
                timezone: Some("Asia/Tokyo".to_string()),
                origin_timezone: None,
                destination_timezone: None,
                display_mode: DisplayMode::Local,
            },
        )
        .unwrap();

    let operation_count = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap()
        .len();
    fixture
        .catalog
        .split_trip_phase(
            &fixture.project_id,
            &phase.id,
            "asset_not_in_the_phase",
            PhaseSplitDirection::From,
            &TripPhase {
                id: String::new(),
                label: "Invalid".to_string(),
                kind: PhaseKind::Location,
                start_anchor_asset_id: None,
                end_anchor_asset_id: None,
                start_trip_time: String::new(),
                end_trip_time: String::new(),
                timezone: None,
                origin_timezone: None,
                destination_timezone: None,
                display_mode: DisplayMode::Local,
            },
        )
        .unwrap_err();
    assert_eq!(
        fixture
            .catalog
            .list_operations(&fixture.project_id)
            .unwrap()
            .len(),
        operation_count,
        "an invalid split writes neither side"
    );

    let snow = fixture
        .catalog
        .split_trip_phase(
            &fixture.project_id,
            &phase.id,
            &ids[2],
            PhaseSplitDirection::From,
            &TripPhase {
                id: String::new(),
                label: "Snow festival".to_string(),
                kind: PhaseKind::Location,
                start_anchor_asset_id: None,
                end_anchor_asset_id: None,
                start_trip_time: String::new(),
                end_trip_time: String::new(),
                timezone: Some("Asia/Tokyo".to_string()),
                origin_timezone: None,
                destination_timezone: None,
                display_mode: DisplayMode::Local,
            },
        )
        .unwrap();
    let arrival = fixture
        .catalog
        .split_trip_phase(
            &fixture.project_id,
            &phase.id,
            &ids[0],
            PhaseSplitDirection::Through,
            &TripPhase {
                id: String::new(),
                label: "Arrival".to_string(),
                kind: PhaseKind::Location,
                start_anchor_asset_id: None,
                end_anchor_asset_id: None,
                start_trip_time: String::new(),
                end_trip_time: String::new(),
                timezone: Some("Asia/Tokyo".to_string()),
                origin_timezone: None,
                destination_timezone: None,
                display_mode: DisplayMode::Local,
            },
        )
        .unwrap();

    let solved = fixture
        .catalog
        .solve_chronology(&fixture.project_id)
        .unwrap();
    let phase_ids: Vec<_> = solved
        .assets
        .iter()
        .map(|asset| {
            asset
                .display_time
                .as_ref()
                .and_then(|display| display.phase_id.as_deref())
        })
        .collect();
    assert_eq!(
        phase_ids,
        vec![
            Some(arrival.id.as_str()),
            Some(phase.id.as_str()),
            Some(snow.id.as_str()),
            Some(snow.id.as_str()),
        ]
    );

    let phases = fixture.catalog.trip_phases(&fixture.project_id).unwrap();
    let remaining = phases.iter().find(|item| item.id == phase.id).unwrap();
    assert_eq!(
        remaining.start_anchor_asset_id.as_deref(),
        Some(ids[1].as_str())
    );
    assert_eq!(
        remaining.end_anchor_asset_id.as_deref(),
        Some(ids[1].as_str())
    );
}

#[test]
fn previewed_trip_phase_split_is_revision_safe_and_retry_idempotent() {
    let fixture = fixture(&[
        ("C0001.MP4", "DJIPocket3", "2026-01-15T03:00:00Z"),
        ("C0002.MP4", "DJIPocket3", "2026-01-15T04:00:00Z"),
        ("C0003.MP4", "DJIPocket3", "2026-01-15T05:00:00Z"),
    ]);
    let ids = asset_ids_in_order(&fixture);
    let phase = fixture
        .catalog
        .set_trip_phase(
            &fixture.project_id,
            &TripPhase {
                id: String::new(),
                label: "Synthetic whole trip".into(),
                kind: PhaseKind::Location,
                start_anchor_asset_id: Some(ids[0].clone()),
                end_anchor_asset_id: Some(ids[2].clone()),
                start_trip_time: String::new(),
                end_trip_time: String::new(),
                timezone: Some("Etc/UTC".into()),
                origin_timezone: None,
                destination_timezone: None,
                display_mode: DisplayMode::Local,
            },
        )
        .unwrap();
    let request = ChronologyMutation::PhaseSplit {
        existing_phase_id: phase.id.clone(),
        boundary_asset_id: ids[1].clone(),
        direction: PhaseSplitDirection::Through,
        phase: TripPhase {
            id: String::new(),
            label: "Synthetic first leg".into(),
            kind: PhaseKind::Transit,
            start_anchor_asset_id: None,
            end_anchor_asset_id: None,
            start_trip_time: String::new(),
            end_trip_time: String::new(),
            timezone: Some("Etc/UTC".into()),
            origin_timezone: None,
            destination_timezone: None,
            display_mode: DisplayMode::Local,
        },
    };
    let preview = fixture
        .catalog
        .preview_chronology_mutation(&fixture.project_id, &request)
        .unwrap();
    assert_eq!(preview.affected_asset_ids, ids);
    let before = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap()
        .len();
    fixture
        .catalog
        .apply_chronology_mutation_preview(&fixture.project_id, &preview)
        .unwrap();
    let applied_count = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap()
        .len();
    assert_eq!(applied_count, before + 2);
    fixture
        .catalog
        .apply_chronology_mutation_preview(&fixture.project_id, &preview)
        .unwrap();
    assert_eq!(
        fixture
            .catalog
            .list_operations(&fixture.project_id)
            .unwrap()
            .len(),
        applied_count
    );
    assert_eq!(
        fixture
            .catalog
            .trip_phases(&fixture.project_id)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn clip_anchored_phase_reflows_when_another_camera_is_corrected() {
    let fixture = fixture(&[
        ("A0001.MP4", "Sony", "2026-01-15T09:00:00Z"),
        ("A0002.MP4", "Sony", "2026-01-15T10:00:00Z"),
        ("D0001.MP4", "DJI", "2026-01-15T11:00:00Z"),
    ]);
    let before = fixture
        .catalog
        .solve_chronology(&fixture.project_id)
        .unwrap();
    let start = before.assets[0].asset_id.clone();
    let end = before.assets[1].asset_id.clone();
    let moving = before.assets[2].asset_id.clone();

    let phase = fixture
        .catalog
        .set_trip_phase(
            &fixture.project_id,
            &TripPhase {
                id: String::new(),
                label: "Tokyo morning".to_string(),
                kind: PhaseKind::Location,
                start_anchor_asset_id: Some(start.clone()),
                end_anchor_asset_id: Some(end.clone()),
                start_trip_time: String::new(),
                end_trip_time: String::new(),
                timezone: Some("Asia/Tokyo".to_string()),
                origin_timezone: None,
                destination_timezone: None,
                display_mode: DisplayMode::Local,
            },
        )
        .unwrap();
    assert_eq!(phase.start_anchor_asset_id.as_deref(), Some(start.as_str()));
    assert_eq!(phase.end_anchor_asset_id.as_deref(), Some(end.as_str()));
    assert_eq!(phase.start_trip_time, "2026-01-15T09:00:00.000Z");
    assert_eq!(phase.end_trip_time, "2026-01-15T10:00:00.000Z");

    let initially_uncovered = fixture
        .catalog
        .solve_chronology(&fixture.project_id)
        .unwrap();
    assert_eq!(
        initially_uncovered
            .assets
            .iter()
            .find(|asset| asset.asset_id == moving)
            .unwrap()
            .display_time
            .as_ref()
            .unwrap()
            .phase_id,
        None
    );

    let dji = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap()
        .into_iter()
        .find(|sequence| sequence.camera_key == "DJI")
        .unwrap();
    fixture
        .catalog
        .set_clock_epoch(
            &fixture.project_id,
            &ClockEpoch {
                id: String::new(),
                capture_sequence_id: dji.id,
                start_counter: 0,
                end_counter: None,
                offset_seconds: -5400,
                confidence: 1.0,
                evidence: vec!["user_set".to_string()],
                label: "DJI ran ninety minutes fast".to_string(),
            },
        )
        .unwrap();

    let corrected = fixture
        .catalog
        .solve_chronology(&fixture.project_id)
        .unwrap();
    let resolved_phase = fixture
        .catalog
        .trip_phases(&fixture.project_id)
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.id == phase.id)
        .unwrap();
    assert_eq!(resolved_phase.start_trip_time, "2026-01-15T09:00:00.000Z");
    assert_eq!(resolved_phase.end_trip_time, "2026-01-15T10:00:00.000Z");
    assert_eq!(
        corrected
            .assets
            .iter()
            .find(|asset| asset.asset_id == moving)
            .unwrap()
            .display_time
            .as_ref()
            .unwrap()
            .phase_id
            .as_deref(),
        Some(phase.id.as_str()),
        "the other camera joins the phase after its corrected time crosses the clip anchors"
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
    let operations_before = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap();
    let error = fixture
        .catalog
        .set_clock_epoch(&fixture.project_id, &candidate)
        .unwrap_err();
    assert_eq!(
        chronology_error_code(&error),
        "chronology.hard_contradiction"
    );
    assert_eq!(
        fixture
            .catalog
            .list_operations(&fixture.project_id)
            .unwrap(),
        operations_before,
        "a hard-blocked apply writes nothing even if an adapter bypasses the preview UI"
    );
}

#[test]
fn clock_epoch_impact_names_every_exact_time_and_position_change_without_writing() {
    let fixture = fixture(&[
        ("C0001.MP4", "DJIPocket3", "2026-01-01T09:00:00Z"),
        ("C0002.MP4", "DJIPocket3", "2026-01-01T10:00:00Z"),
        ("C0003.MP4", "DJIPocket3", "2026-01-01T11:00:00Z"),
    ]);
    let sequence = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap()
        .remove(0);
    let facts_before = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap();
    let solution_before = solve(&facts_before);
    let operations_before = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap();

    let preview = fixture
        .catalog
        .preview_clock_epoch_impact(
            &fixture.project_id,
            &ClockEpoch {
                id: String::new(),
                capture_sequence_id: sequence.id.clone(),
                start_counter: 2,
                end_counter: Some(2),
                offset_seconds: 7_200,
                confidence: 1.0,
                evidence: vec!["user_preview".to_string()],
                label: "move the middle clip".to_string(),
            },
        )
        .unwrap();
    assert!(preview.epoch.id.starts_with("epoch_"));
    assert_eq!(preview.epoch.capture_sequence_id, sequence.id);
    let impact = &preview.impact;

    assert_eq!(
        impact.changes,
        vec![
            ChronologyImpactChange {
                asset_id: "asset_C0003_2".to_string(),
                before: Some("2026-01-01T11:00:00.000Z".to_string()),
                after: Some("2026-01-01T11:00:00.000Z".to_string()),
                before_position: 2,
                after_position: 1,
            },
            ChronologyImpactChange {
                asset_id: "asset_C0002_1".to_string(),
                before: Some("2026-01-01T10:00:00.000Z".to_string()),
                after: Some("2026-01-01T12:00:00.000Z".to_string()),
                before_position: 1,
                after_position: 2,
            },
        ]
    );
    assert!(impact.contradictions.is_empty());
    assert!(!impact.blocked);
    assert_eq!(
        fixture
            .catalog
            .chronology_facts(&fixture.project_id)
            .unwrap(),
        facts_before,
        "previewing must not persist the candidate epoch"
    );
    assert_eq!(
        fixture
            .catalog
            .list_operations(&fixture.project_id)
            .unwrap(),
        operations_before,
        "previewing must not allocate an operation or node sequence"
    );

    fixture
        .catalog
        .apply_clock_epoch_preview(&fixture.project_id, &preview)
        .unwrap();
    let solution_after = fixture
        .catalog
        .solve_chronology(&fixture.project_id)
        .unwrap();
    let actual_changes: Vec<_> = solution_after
        .assets
        .iter()
        .enumerate()
        .filter_map(|(after_position, asset)| {
            let (before_position, previous) = solution_before
                .assets
                .iter()
                .enumerate()
                .find(|(_, candidate)| candidate.asset_id == asset.asset_id)?;
            (before_position != after_position || previous.trip_time != asset.trip_time).then(
                || ChronologyImpactChange {
                    asset_id: asset.asset_id.clone(),
                    before: previous.trip_time.clone(),
                    after: asset.trip_time.clone(),
                    before_position,
                    after_position,
                },
            )
        })
        .collect();
    assert_eq!(actual_changes, preview.impact.changes);
}

#[test]
fn clear_epoch_preview_reveals_the_underlying_epoch_and_matches_apply_without_writing() {
    let fixture = fixture(&[
        ("C0001.MP4", "DJIPocket3", "2026-01-01T09:00:00Z"),
        ("C0002.MP4", "DJIPocket3", "2026-01-01T10:00:00Z"),
    ]);
    let sequence = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap()
        .remove(0);
    fixture
        .catalog
        .set_clock_epoch(
            &fixture.project_id,
            &ClockEpoch {
                id: "epoch_broad".to_string(),
                capture_sequence_id: sequence.id.clone(),
                start_counter: 0,
                end_counter: None,
                offset_seconds: 3_600,
                confidence: 1.0,
                evidence: vec!["broad".to_string()],
                label: "broad correction".to_string(),
            },
        )
        .unwrap();
    fixture
        .catalog
        .set_clock_epoch(
            &fixture.project_id,
            &ClockEpoch {
                id: "epoch_narrow".to_string(),
                capture_sequence_id: sequence.id,
                start_counter: 2,
                end_counter: Some(2),
                offset_seconds: 7_200,
                confidence: 1.0,
                evidence: vec!["narrow".to_string()],
                label: "narrow correction".to_string(),
            },
        )
        .unwrap();
    let before = fixture
        .catalog
        .solve_chronology(&fixture.project_id)
        .unwrap();
    assert_eq!(
        before
            .assets
            .iter()
            .find(|asset| asset.asset_id == "asset_C0002_1")
            .unwrap()
            .trip_time
            .as_deref(),
        Some("2026-01-01T12:00:00.000Z")
    );
    let facts_before = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap();
    let operations_before = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap();

    let preview = fixture
        .catalog
        .preview_clear_clock_epoch_impact(&fixture.project_id, "epoch_narrow")
        .unwrap();
    assert_eq!(
        preview.impact.changes,
        [ChronologyImpactChange {
            asset_id: "asset_C0002_1".to_string(),
            before: Some("2026-01-01T12:00:00.000Z".to_string()),
            after: Some("2026-01-01T11:00:00.000Z".to_string()),
            before_position: 1,
            after_position: 1,
        }]
    );
    assert_eq!(
        fixture
            .catalog
            .chronology_facts(&fixture.project_id)
            .unwrap(),
        facts_before
    );
    assert_eq!(
        fixture
            .catalog
            .list_operations(&fixture.project_id)
            .unwrap(),
        operations_before
    );

    fixture
        .catalog
        .apply_clear_clock_epoch_preview(&fixture.project_id, &preview)
        .unwrap();
    let after = fixture
        .catalog
        .solve_chronology(&fixture.project_id)
        .unwrap();
    assert_eq!(
        after
            .assets
            .iter()
            .find(|asset| asset.asset_id == "asset_C0002_1")
            .unwrap()
            .trip_time
            .as_deref(),
        preview.impact.changes[0].after.as_deref()
    );
}

#[test]
fn clock_set_clear_and_visual_confirmation_refuse_changed_facts() {
    let set_fixture = fixture(&[
        ("C0001.MP4", "Sony", "2026-01-01T09:00:00Z"),
        ("D0001.MP4", "DJI", "2026-01-01T12:00:00Z"),
    ]);
    let set_sequences = set_fixture
        .catalog
        .capture_sequences(&set_fixture.project_id)
        .unwrap();
    let set_source = set_sequences
        .iter()
        .find(|sequence| sequence.camera_key == "Sony")
        .unwrap();
    let set_other = set_sequences
        .iter()
        .find(|sequence| sequence.camera_key == "DJI")
        .unwrap();
    let set_preview = set_fixture
        .catalog
        .preview_clock_epoch_impact(
            &set_fixture.project_id,
            &ClockEpoch {
                id: String::new(),
                capture_sequence_id: set_source.id.clone(),
                start_counter: 1,
                end_counter: None,
                offset_seconds: 300,
                confidence: 1.0,
                evidence: vec!["preview".to_string()],
                label: "Sony correction".to_string(),
            },
        )
        .unwrap();
    assert!(!set_preview.facts_token.is_empty());
    set_fixture
        .catalog
        .set_clock_epoch(
            &set_fixture.project_id,
            &ClockEpoch {
                id: "epoch_unrelated_set".to_string(),
                capture_sequence_id: set_other.id.clone(),
                start_counter: 1,
                end_counter: None,
                offset_seconds: 60,
                confidence: 1.0,
                evidence: vec!["other".to_string()],
                label: "DJI correction".to_string(),
            },
        )
        .unwrap();
    let operations_before_set_apply = set_fixture
        .catalog
        .list_operations(&set_fixture.project_id)
        .unwrap();
    let error = set_fixture
        .catalog
        .apply_clock_epoch_preview(&set_fixture.project_id, &set_preview)
        .unwrap_err();
    assert_eq!(
        chronology_error_code(&error),
        "chronology.invalid_reference"
    );
    assert!(format!("{error:#}").contains("preview is stale"));
    assert_eq!(
        set_fixture
            .catalog
            .list_operations(&set_fixture.project_id)
            .unwrap(),
        operations_before_set_apply
    );

    let clear_fixture = fixture(&[
        ("C0001.MP4", "Sony", "2026-01-01T09:00:00Z"),
        ("D0001.MP4", "DJI", "2026-01-01T12:00:00Z"),
    ]);
    let clear_sequences = clear_fixture
        .catalog
        .capture_sequences(&clear_fixture.project_id)
        .unwrap();
    let clear_source = clear_sequences
        .iter()
        .find(|sequence| sequence.camera_key == "Sony")
        .unwrap();
    let clear_other = clear_sequences
        .iter()
        .find(|sequence| sequence.camera_key == "DJI")
        .unwrap();
    clear_fixture
        .catalog
        .set_clock_epoch(
            &clear_fixture.project_id,
            &ClockEpoch {
                id: "epoch_to_clear_exactly".to_string(),
                capture_sequence_id: clear_source.id.clone(),
                start_counter: 1,
                end_counter: None,
                offset_seconds: 300,
                confidence: 1.0,
                evidence: vec!["preview".to_string()],
                label: "Sony correction".to_string(),
            },
        )
        .unwrap();
    let clear_preview = clear_fixture
        .catalog
        .preview_clear_clock_epoch_impact(&clear_fixture.project_id, "epoch_to_clear_exactly")
        .unwrap();
    assert!(!clear_preview.facts_token.is_empty());
    clear_fixture
        .catalog
        .set_clock_epoch(
            &clear_fixture.project_id,
            &ClockEpoch {
                id: "epoch_unrelated_clear".to_string(),
                capture_sequence_id: clear_other.id.clone(),
                start_counter: 1,
                end_counter: None,
                offset_seconds: 60,
                confidence: 1.0,
                evidence: vec!["other".to_string()],
                label: "DJI correction".to_string(),
            },
        )
        .unwrap();
    let operations_before_clear_apply = clear_fixture
        .catalog
        .list_operations(&clear_fixture.project_id)
        .unwrap();
    let error = clear_fixture
        .catalog
        .apply_clear_clock_epoch_preview(&clear_fixture.project_id, &clear_preview)
        .unwrap_err();
    assert_eq!(
        chronology_error_code(&error),
        "chronology.invalid_reference"
    );
    assert!(format!("{error:#}").contains("preview is stale"));
    assert_eq!(
        clear_fixture
            .catalog
            .list_operations(&clear_fixture.project_id)
            .unwrap(),
        operations_before_clear_apply
    );
    assert!(
        clear_fixture
            .catalog
            .chronology_facts(&clear_fixture.project_id)
            .unwrap()
            .epochs
            .iter()
            .any(|epoch| epoch.id == "epoch_to_clear_exactly")
    );

    let visual_fixture = fixture(&[
        ("A0001.MP4", "Sony", "2026-01-01T09:00:00Z"),
        ("D0001.MP4", "DJI", "2026-01-01T12:00:00Z"),
    ]);
    let visual_other = visual_fixture
        .catalog
        .capture_sequences(&visual_fixture.project_id)
        .unwrap()
        .into_iter()
        .find(|sequence| sequence.camera_key == "DJI")
        .unwrap();
    let visual_preview = visual_fixture
        .catalog
        .preview_visual_placement(
            &visual_fixture.project_id,
            &["asset_A0001_0".to_string()],
            "asset_D0001_1",
            VisualPlacementSide::Before,
            "place Sony",
        )
        .unwrap();
    assert!(!visual_preview.facts_token.is_empty());
    visual_fixture
        .catalog
        .set_clock_epoch(
            &visual_fixture.project_id,
            &ClockEpoch {
                id: "epoch_unrelated_visual".to_string(),
                capture_sequence_id: visual_other.id,
                start_counter: 1,
                end_counter: None,
                offset_seconds: 60,
                confidence: 1.0,
                evidence: vec!["other".to_string()],
                label: "DJI correction".to_string(),
            },
        )
        .unwrap();
    let visual_as_clock = ClockEpochImpactPreview {
        epoch: visual_preview.epoch,
        facts_token: visual_preview.facts_token,
        impact: visual_preview.impact,
    };
    let operations_before_visual_apply = visual_fixture
        .catalog
        .list_operations(&visual_fixture.project_id)
        .unwrap();
    let error = visual_fixture
        .catalog
        .apply_clock_epoch_preview(&visual_fixture.project_id, &visual_as_clock)
        .unwrap_err();
    assert_eq!(
        chronology_error_code(&error),
        "chronology.invalid_reference"
    );
    assert!(format!("{error:#}").contains("preview is stale"));
    assert_eq!(
        visual_fixture
            .catalog
            .list_operations(&visual_fixture.project_id)
            .unwrap(),
        operations_before_visual_apply
    );
}

#[test]
fn visual_placement_derives_a_raw_relative_epoch_and_apply_matches_the_preview() {
    let fixture = fixture(&[
        ("A0001.MP4", "Sony", "2026-01-01T09:00:00Z"),
        ("A0002.MP4", "Sony", "2026-01-01T10:00:00Z"),
        ("D0001.MP4", "DJI", "2026-01-01T12:00:00Z"),
    ]);
    let sony = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap()
        .into_iter()
        .find(|sequence| sequence.camera_key == "Sony")
        .unwrap();
    fixture
        .catalog
        .set_clock_epoch(
            &fixture.project_id,
            &ClockEpoch {
                id: "epoch_existing".to_string(),
                capture_sequence_id: sony.id.clone(),
                start_counter: 0,
                end_counter: None,
                offset_seconds: 3_600,
                confidence: 1.0,
                evidence: vec!["user_set".to_string()],
                label: "existing correction".to_string(),
            },
        )
        .unwrap();
    let selected = vec!["asset_A0001_0".to_string(), "asset_A0002_1".to_string()];
    let before = fixture
        .catalog
        .solve_chronology(&fixture.project_id)
        .unwrap();
    let facts_before = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap();
    let operations_before = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap();

    let preview = fixture
        .catalog
        .preview_visual_placement(
            &fixture.project_id,
            &selected,
            "asset_D0001_2",
            VisualPlacementSide::Before,
            "  Sony belongs before DJI  ",
        )
        .unwrap();

    assert!(
        !preview.epoch.id.is_empty(),
        "the exact previewed epoch identity must survive confirmation"
    );
    assert_eq!(preview.epoch.capture_sequence_id, sony.id);
    assert_eq!(preview.epoch.start_counter, 1);
    assert_eq!(preview.epoch.end_counter, Some(2));
    assert_eq!(
        preview.epoch.offset_seconds, 7_195,
        "clock epochs are offsets from raw camera time, not deltas from the current correction"
    );
    assert_eq!(preview.epoch.label, "Sony belongs before DJI");
    assert_eq!(preview.epoch.evidence, ["user_visual_placement"]);
    assert_eq!(
        preview
            .impact
            .changes
            .iter()
            .map(|change| (
                change.asset_id.as_str(),
                change.before.as_deref(),
                change.after.as_deref(),
                change.before_position,
                change.after_position,
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                "asset_A0001_0",
                Some("2026-01-01T10:00:00.000Z"),
                Some("2026-01-01T10:59:55.000Z"),
                0,
                0,
            ),
            (
                "asset_A0002_1",
                Some("2026-01-01T11:00:00.000Z"),
                Some("2026-01-01T11:59:55.000Z"),
                1,
                1,
            ),
        ]
    );
    assert!(!preview.impact.blocked);
    assert_eq!(
        fixture
            .catalog
            .chronology_facts(&fixture.project_id)
            .unwrap(),
        facts_before
    );
    assert_eq!(
        fixture
            .catalog
            .list_operations(&fixture.project_id)
            .unwrap(),
        operations_before
    );

    let stored = fixture
        .catalog
        .apply_clock_epoch_preview(
            &fixture.project_id,
            &ClockEpochImpactPreview {
                epoch: preview.epoch.clone(),
                facts_token: preview.facts_token.clone(),
                impact: preview.impact.clone(),
            },
        )
        .unwrap();
    assert_eq!(stored, preview.epoch);
    let after = fixture
        .catalog
        .solve_chronology(&fixture.project_id)
        .unwrap();
    let actual_changes: Vec<_> = after
        .assets
        .iter()
        .enumerate()
        .filter_map(|(after_position, asset)| {
            let (before_position, previous) = before
                .assets
                .iter()
                .enumerate()
                .find(|(_, candidate)| candidate.asset_id == asset.asset_id)?;
            (before_position != after_position || previous.trip_time != asset.trip_time).then(
                || ChronologyImpactChange {
                    asset_id: asset.asset_id.clone(),
                    before: previous.trip_time.clone(),
                    after: asset.trip_time.clone(),
                    before_position,
                    after_position,
                },
            )
        })
        .collect();
    assert_eq!(actual_changes, preview.impact.changes);
}

#[test]
fn visual_placement_rejects_candidates_an_epoch_cannot_represent() {
    let fixture = fixture(&[
        ("A0001.MP4", "Sony", "2026-01-01T09:00:00Z"),
        ("D0001.MP4", "DJI", "2026-01-01T10:00:00Z"),
        ("A0002.MP4", "Sony", "2026-01-01T11:00:00Z"),
        ("D0002.MP4", "DJI", "2026-01-01T12:00:00Z"),
        ("clip.MP4", "Sony", "2026-01-01T13:00:00Z"),
    ]);
    let operations_before = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap();

    let invalid_cases = [
        (
            Vec::<String>::new(),
            "asset_D0002_3",
            "select at least one clip",
        ),
        (
            vec!["asset_A0001_0".to_string()],
            "asset_A0001_0",
            "destination inside the selected run",
        ),
        (
            vec!["asset_A0001_0".to_string(), "asset_A0001_0".to_string()],
            "asset_D0002_3",
            "duplicate selected identity",
        ),
        (
            vec!["asset_not_here".to_string()],
            "asset_D0002_3",
            "unknown selected clip",
        ),
        (
            vec!["asset_A0001_0".to_string(), "asset_A0002_2".to_string()],
            "asset_D0002_3",
            "non-contiguous solved run",
        ),
        (
            vec!["asset_A0001_0".to_string(), "asset_D0001_1".to_string()],
            "asset_D0002_3",
            "run crossing capture sequences",
        ),
        (
            vec!["asset_clip_4".to_string()],
            "asset_D0002_3",
            "clip without a filename counter",
        ),
    ];
    for (asset_ids, target, context) in invalid_cases {
        let error = fixture
            .catalog
            .preview_visual_placement(
                &fixture.project_id,
                &asset_ids,
                target,
                VisualPlacementSide::Before,
                "invalid",
            )
            .expect_err(context);
        assert_eq!(
            chronology_error_code(&error),
            "chronology.invalid_reference"
        );
    }
    assert_eq!(
        fixture
            .catalog
            .list_operations(&fixture.project_id)
            .unwrap(),
        operations_before,
        "every refused visual candidate is read-only"
    );
}

#[test]
fn explicit_sequence_preview_can_scope_the_move_to_one_clip_and_matches_apply() {
    let fixture = fixture(&[
        ("C0001.MP4", "Sony", "2026-01-01T09:00:00Z"),
        ("C0002.MP4", "Sony", "2026-01-01T10:00:00Z"),
        ("C0003.MP4", "Sony", "2026-01-01T11:00:00Z"),
        ("D0001.MP4", "DJI", "2026-01-01T12:00:00Z"),
    ]);
    let sequences = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap();
    let source = sequences
        .iter()
        .find(|sequence| sequence.camera_key == "Sony")
        .unwrap();
    let destination = sequences
        .iter()
        .find(|sequence| sequence.camera_key == "DJI")
        .unwrap();
    let facts_before = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap();
    let operations_before = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap();
    let selected = ["asset_C0002_1".to_string()];

    let preview = fixture
        .catalog
        .preview_capture_sequence_assets(
            &fixture.project_id,
            &source.id,
            &destination.id,
            &selected,
        )
        .unwrap();
    assert_eq!(preview.from_sequence_id, source.id);
    assert_eq!(preview.into_sequence_id, destination.id);
    assert_eq!(preview.asset_ids, selected);
    assert_eq!(
        fixture
            .catalog
            .chronology_facts(&fixture.project_id)
            .unwrap(),
        facts_before
    );
    assert_eq!(
        fixture
            .catalog
            .list_operations(&fixture.project_id)
            .unwrap(),
        operations_before,
        "an explicit preview writes nothing"
    );

    let moved = fixture
        .catalog
        .apply_capture_sequence_reassignment(&fixture.project_id, &preview)
        .unwrap();
    assert_eq!(moved, selected);
    let facts_after = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap();
    assert_eq!(
        facts_after
            .assets
            .iter()
            .find(|asset| asset.asset_id == "asset_C0002_1")
            .unwrap()
            .capture_sequence_id
            .as_deref(),
        Some(destination.id.as_str())
    );
    assert_eq!(
        facts_after
            .assets
            .iter()
            .find(|asset| asset.asset_id == "asset_C0003_2")
            .unwrap()
            .capture_sequence_id
            .as_deref(),
        Some(source.id.as_str()),
        "single-clip scope never expands to following counters"
    );
}

#[test]
fn explicit_sequence_preview_and_apply_share_membership_validation() {
    let fixture = fixture(&[
        ("C0001.MP4", "Sony", "2026-01-01T09:00:00Z"),
        ("D0001.MP4", "DJI", "2026-01-01T10:00:00Z"),
    ]);
    let sequences = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap();
    let source = &sequences
        .iter()
        .find(|sequence| sequence.camera_key == "Sony")
        .unwrap()
        .id;
    let destination = &sequences
        .iter()
        .find(|sequence| sequence.camera_key == "DJI")
        .unwrap()
        .id;
    let operations_before = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap();
    let invalid_sets = [
        Vec::<String>::new(),
        vec!["asset_C0001_0".to_string(), "asset_C0001_0".to_string()],
        vec!["asset_D0001_1".to_string()],
        vec!["asset_not_here".to_string()],
    ];
    for asset_ids in invalid_sets {
        let preview_error = fixture
            .catalog
            .preview_capture_sequence_assets(&fixture.project_id, source, destination, &asset_ids)
            .unwrap_err();
        let apply_error = fixture
            .catalog
            .reassign_capture_sequence_assets(&fixture.project_id, source, destination, &asset_ids)
            .unwrap_err();
        assert_eq!(
            chronology_error_code(&preview_error),
            "chronology.invalid_reference"
        );
        assert_eq!(format!("{preview_error:#}"), format!("{apply_error:#}"));
    }
    assert_eq!(
        fixture
            .catalog
            .list_operations(&fixture.project_id)
            .unwrap(),
        operations_before,
        "invalid preview and apply candidates are both read-only"
    );
}

#[test]
fn following_reassignment_uses_the_actual_suffix_after_a_counter_restart() {
    let fixture = fixture(&[
        ("C0001.MP4", "Sony", "2026-01-01T09:00:00Z"),
        ("C0002.MP4", "Sony", "2026-01-01T10:00:00Z"),
        ("C0001.MP4", "Sony", "2026-01-02T09:00:00Z"),
        ("C0002.MP4", "Sony", "2026-01-02T10:00:00Z"),
        ("D0001.MP4", "DJI", "2026-01-03T09:00:00Z"),
    ]);
    let destination = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap()
        .into_iter()
        .find(|sequence| sequence.camera_key == "DJI")
        .unwrap();

    let preview = fixture
        .catalog
        .preview_capture_sequence_reassignment(
            &fixture.project_id,
            "asset_C0001_2",
            &destination.id,
        )
        .unwrap();
    assert_eq!(
        preview.asset_ids,
        ["asset_C0001_2".to_string(), "asset_C0002_3".to_string()],
        "matching C0001/C0002 counters from the earlier card are before the anchor, not following it"
    );
}

#[test]
fn reassignment_confirmation_refuses_an_unrelated_chronology_change() {
    let fixture = fixture(&[
        ("C0001.MP4", "Sony", "2026-01-01T09:00:00Z"),
        ("D0001.MP4", "DJI", "2026-01-01T10:00:00Z"),
    ]);
    let sequences = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap();
    let source = sequences
        .iter()
        .find(|sequence| sequence.camera_key == "Sony")
        .unwrap();
    let destination = sequences
        .iter()
        .find(|sequence| sequence.camera_key == "DJI")
        .unwrap();
    let preview = fixture
        .catalog
        .preview_capture_sequence_assets(
            &fixture.project_id,
            &source.id,
            &destination.id,
            &["asset_C0001_0".to_string()],
        )
        .unwrap();
    fixture
        .catalog
        .set_clock_epoch(
            &fixture.project_id,
            &ClockEpoch {
                id: "epoch_unrelated_reassignment_change".to_string(),
                capture_sequence_id: destination.id.clone(),
                start_counter: 1,
                end_counter: None,
                offset_seconds: 60,
                confidence: 1.0,
                evidence: vec!["other calibration".to_string()],
                label: "DJI adjustment".to_string(),
            },
        )
        .unwrap();
    let operations_before_apply = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap();

    let error = fixture
        .catalog
        .apply_capture_sequence_reassignment(&fixture.project_id, &preview)
        .unwrap_err();
    assert_eq!(
        chronology_error_code(&error),
        "chronology.invalid_reference"
    );
    assert!(format!("{error:#}").contains("preview is stale"));
    assert_eq!(
        fixture
            .catalog
            .list_operations(&fixture.project_id)
            .unwrap(),
        operations_before_apply
    );
    assert_eq!(
        fixture
            .catalog
            .chronology_facts(&fixture.project_id)
            .unwrap()
            .assets
            .into_iter()
            .find(|asset| asset.asset_id == "asset_C0001_0")
            .unwrap()
            .capture_sequence_id
            .as_deref(),
        Some(source.id.as_str())
    );
}

#[test]
fn reassignment_previews_and_applies_the_same_explicit_asset_identities() {
    let clips = [
        ("C0001.MP4", "Sony", "2026-01-01T09:00:00Z"),
        ("C0002.MP4", "Sony", "2026-01-01T10:00:00Z"),
        ("C0003.MP4", "Sony", "2026-01-01T11:00:00Z"),
        ("D0001.MP4", "DJI", "2026-01-01T12:00:00Z"),
    ];
    let fixture = fixture(&clips);
    let sequences = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap();
    let source = sequences
        .iter()
        .find(|sequence| sequence.camera_key == "Sony")
        .unwrap();
    let destination = sequences
        .iter()
        .find(|sequence| sequence.camera_key == "DJI")
        .unwrap();
    let facts_before = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap();
    let operations_before = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap();

    let preview = fixture
        .catalog
        .preview_capture_sequence_reassignment(
            &fixture.project_id,
            "asset_C0002_1",
            &destination.id,
        )
        .unwrap();
    assert_eq!(preview.from_sequence_id, source.id);
    assert_eq!(preview.into_sequence_id, destination.id);
    assert_eq!(
        preview.asset_ids,
        ["asset_C0002_1".to_string(), "asset_C0003_2".to_string()]
    );
    assert_eq!(
        fixture
            .catalog
            .chronology_facts(&fixture.project_id)
            .unwrap(),
        facts_before,
        "preview must not move memberships"
    );
    assert_eq!(
        fixture
            .catalog
            .list_operations(&fixture.project_id)
            .unwrap(),
        operations_before,
        "preview must not emit an operation"
    );

    add_portable_asset(
        &fixture,
        "asset_C0004_between",
        "C0004.MP4",
        "Sony",
        "2026-01-01T13:00:00Z",
    );

    let operations_before_stale_apply = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap();
    let stale_error = fixture
        .catalog
        .apply_capture_sequence_reassignment(&fixture.project_id, &preview)
        .unwrap_err();
    assert_eq!(
        chronology_error_code(&stale_error),
        "chronology.invalid_reference"
    );
    assert!(format!("{stale_error:#}").contains("preview is stale"));
    assert_eq!(
        fixture
            .catalog
            .list_operations(&fixture.project_id)
            .unwrap(),
        operations_before_stale_apply,
        "an intervening chronology fact invalidates confirmation without writing"
    );

    // The lower-level explicit identity seam remains useful to operation/CLI
    // callers and demonstrates that durable replay never expands identities.
    let moved = fixture
        .catalog
        .reassign_capture_sequence_assets(
            &fixture.project_id,
            &preview.from_sequence_id,
            &destination.id,
            &preview.asset_ids,
        )
        .unwrap();
    assert_eq!(moved, preview.asset_ids);
    let after = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap();
    for asset_id in &moved {
        assert_eq!(
            after
                .assets
                .iter()
                .find(|asset| &asset.asset_id == asset_id)
                .unwrap()
                .capture_sequence_id
                .as_deref(),
            Some(destination.id.as_str())
        );
    }
    assert_eq!(
        after
            .assets
            .iter()
            .find(|asset| asset.asset_id == "asset_C0001_0")
            .unwrap()
            .capture_sequence_id
            .as_deref(),
        Some(source.id.as_str())
    );
    assert_eq!(
        after
            .assets
            .iter()
            .find(|asset| asset.asset_id == "asset_C0004_between")
            .unwrap()
            .capture_sequence_id
            .as_deref(),
        Some(source.id.as_str()),
        "a clip imported after preview is not silently added at confirmation"
    );

    let operation = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap()
        .into_iter()
        .find(|operation| operation.operation_type == CAPTURE_SEQUENCE_REASSIGNED)
        .unwrap();
    assert_eq!(operation.aggregate_type, "captureSequence");
    assert_eq!(operation.aggregate_id, source.id);
    assert_eq!(
        operation.payload,
        json!({
            "fromSequenceId": source.id,
            "intoSequenceId": destination.id,
            "assetIds": preview.asset_ids,
        })
    );

    // Replay can arrive after a later import on another replica. The explicit
    // payload must move only the identities that were confirmed, never expand
    // the old counter range to include that newly discovered clip.
    let replay_fixture = crate::fixture(&clips);
    let replay_sequences = replay_fixture
        .catalog
        .capture_sequences(&replay_fixture.project_id)
        .unwrap();
    let replay_source = replay_sequences
        .iter()
        .find(|sequence| sequence.camera_key == "Sony")
        .unwrap();
    let replay_destination = replay_sequences
        .iter()
        .find(|sequence| sequence.camera_key == "DJI")
        .unwrap();
    add_portable_asset(
        &replay_fixture,
        "asset_C0004_later",
        "C0004.MP4",
        "Sony",
        "2026-01-01T13:00:00Z",
    );
    let mut replay = operation.clone();
    replay.operation_id = "operation_replayed_reassignment".to_string();
    replay.project_id = replay_fixture.project_id.clone();
    replay.aggregate_id = replay_source.id.clone();
    replay.payload = json!({
        "fromSequenceId": replay_source.id,
        "intoSequenceId": replay_destination.id,
        "assetIds": moved,
    });
    assert_eq!(
        replay_fixture.catalog.apply_operation(&replay).unwrap(),
        ApplyOperationResult::Applied
    );
    assert_eq!(
        replay_fixture.catalog.apply_operation(&replay).unwrap(),
        ApplyOperationResult::Duplicate,
        "replaying the same operation is idempotent"
    );
    let mut stale = replay.clone();
    stale.operation_id = "operation_stale_reassignment".to_string();
    stale.origin_sequence += 1;
    stale.payload = json!({
        "fromSequenceId": replay_source.id,
        "intoSequenceId": replay_destination.id,
        "assetIds": ["asset_C0001_0"],
    });
    assert_eq!(
        replay_fixture.catalog.apply_operation(&stale).unwrap(),
        ApplyOperationResult::Conflict,
        "a stale source-sequence revision cannot move a second set"
    );
    let replayed = replay_fixture
        .catalog
        .chronology_facts(&replay_fixture.project_id)
        .unwrap();
    assert_eq!(
        replayed
            .assets
            .iter()
            .find(|asset| asset.asset_id == "asset_C0004_later")
            .unwrap()
            .capture_sequence_id
            .as_deref(),
        Some(replay_source.id.as_str()),
        "later imports are not swept into an older confirmed reassignment"
    );
    assert_eq!(
        replayed
            .assets
            .iter()
            .find(|asset| asset.asset_id == "asset_C0001_0")
            .unwrap()
            .capture_sequence_id
            .as_deref(),
        Some(replay_source.id.as_str()),
        "the stale replay leaves source membership untouched"
    );
}

#[test]
fn reassignment_confirmation_refuses_a_stale_preview_as_one_atomic_write() {
    let fixture = fixture(&[
        ("C0001.MP4", "Sony", "2026-01-01T09:00:00Z"),
        ("C0002.MP4", "Sony", "2026-01-01T10:00:00Z"),
        ("D0001.MP4", "DJI", "2026-01-01T11:00:00Z"),
    ]);
    let sequences = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap();
    let source = sequences
        .iter()
        .find(|sequence| sequence.camera_key == "Sony")
        .unwrap();
    let destination = sequences
        .iter()
        .find(|sequence| sequence.camera_key == "DJI")
        .unwrap();
    let preview = fixture
        .catalog
        .preview_capture_sequence_reassignment(
            &fixture.project_id,
            "asset_C0001_0",
            &destination.id,
        )
        .unwrap();
    assert_eq!(
        preview.asset_ids,
        ["asset_C0001_0".to_string(), "asset_C0002_1".to_string()]
    );

    fixture
        .catalog
        .reassign_capture_sequence_assets(
            &fixture.project_id,
            &source.id,
            &destination.id,
            &["asset_C0002_1".to_string()],
        )
        .unwrap();
    let operations_before_stale_confirmation = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap();
    let error = fixture
        .catalog
        .apply_capture_sequence_reassignment(&fixture.project_id, &preview)
        .unwrap_err();
    assert_eq!(
        chronology_error_code(&error),
        "chronology.invalid_reference"
    );
    assert!(format!("{error:#}").contains("preview is stale"));
    assert_eq!(
        fixture
            .catalog
            .chronology_facts(&fixture.project_id)
            .unwrap()
            .assets
            .into_iter()
            .find(|asset| asset.asset_id == "asset_C0001_0")
            .unwrap()
            .capture_sequence_id
            .as_deref(),
        Some(source.id.as_str()),
        "the still-valid identity must not move when a sibling identity is stale"
    );
    assert_eq!(
        fixture
            .catalog
            .list_operations(&fixture.project_id)
            .unwrap(),
        operations_before_stale_confirmation,
        "a stale confirmation must not emit an operation"
    );
}

#[test]
fn reassignment_materialization_refuses_an_invalid_identity_atomically() {
    let fixture = fixture(&[
        ("C0001.MP4", "Sony", "2026-01-01T09:00:00Z"),
        ("C0002.MP4", "Sony", "2026-01-01T10:00:00Z"),
        ("D0001.MP4", "DJI", "2026-01-01T11:00:00Z"),
    ]);
    let sequences = fixture
        .catalog
        .capture_sequences(&fixture.project_id)
        .unwrap();
    let source = sequences
        .iter()
        .find(|sequence| sequence.camera_key == "Sony")
        .unwrap();
    let destination = sequences
        .iter()
        .find(|sequence| sequence.camera_key == "DJI")
        .unwrap();
    let template = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let mut invalid = template;
    invalid.operation_id = "operation_invalid_reassignment".to_string();
    invalid.origin_sequence += 10_000;
    invalid.aggregate_type = "captureSequence".to_string();
    invalid.aggregate_id = source.id.clone();
    invalid.operation_type = CAPTURE_SEQUENCE_REASSIGNED.to_string();
    invalid.base_revision = Some(0);
    invalid.payload = json!({
        "fromSequenceId": source.id,
        "intoSequenceId": destination.id,
        "assetIds": ["asset_C0002_1", "asset_not_in_source"],
    });
    let operations_before = fixture
        .catalog
        .list_operations(&fixture.project_id)
        .unwrap();

    let error = fixture.catalog.apply_operation(&invalid).unwrap_err();
    assert!(format!("{error:#}").contains("asset_not_in_source is not in source sequence"));
    let facts = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap();
    assert_eq!(
        facts
            .assets
            .iter()
            .find(|asset| asset.asset_id == "asset_C0002_1")
            .unwrap()
            .capture_sequence_id
            .as_deref(),
        Some(source.id.as_str()),
        "valid identities before the bad one must not move partially"
    );
    assert_eq!(
        fixture
            .catalog
            .list_operations(&fixture.project_id)
            .unwrap(),
        operations_before,
        "a rejected materialization must roll back its operation row"
    );
}

#[test]
fn chronology_snapshot_derives_solution_and_proposals_from_its_returned_facts() {
    let fixture = fixture(&[
        ("C0001.MP4", "DJIPocket3", "2026-01-01T09:00:00Z"),
        ("C0002.MP4", "DJIPocket3", "2026-01-01T10:00:00Z"),
        ("C0001.MP4", "DJIPocket3", "2026-01-02T09:00:00Z"),
        ("C0002.MP4", "DJIPocket3", "2026-01-02T10:00:00Z"),
    ]);

    let snapshot = fixture
        .catalog
        .chronology_snapshot(&fixture.project_id)
        .unwrap();
    assert_eq!(snapshot.solution, solve(&snapshot.facts));
    assert_eq!(
        snapshot.proposed_splits,
        fixture
            .catalog
            .propose_capture_sequence_splits(&fixture.project_id)
            .unwrap()
    );
    assert_eq!(snapshot.proposed_splits.len(), 1);
    assert_eq!(snapshot.proposed_splits[0].at_counter, 1);
    assert_eq!(snapshot.facts.assets.len(), snapshot.solution.assets.len());
}

fn synthetic_clock_question(fixture: &Fixture) -> ClockBlockQuestion {
    let facts = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap();
    let first = &facts.assets[0];
    let sequence = first.capture_sequence_id.clone().unwrap();
    let end_counter = first.filename_counter.unwrap() + 1;
    let reference = facts
        .assets
        .iter()
        .find(|asset| {
            asset.capture_sequence_id.as_deref() == Some(sequence.as_str())
                && asset
                    .filename_counter
                    .is_some_and(|counter| counter > end_counter)
        })
        .unwrap();
    let counter = first.filename_counter.unwrap();
    let epoch = |id: &str, offset_seconds| ClockEpoch {
        id: id.into(),
        capture_sequence_id: sequence.clone(),
        start_counter: counter,
        end_counter: Some(counter + 1),
        offset_seconds,
        confidence: 1.0,
        evidence: vec!["synthetic independent reference".into()],
        label: id.into(),
    };
    ClockBlockQuestion {
        id: "synthetic-clock-question".into(),
        capture_sequence_id: sequence.clone(),
        start_counter: counter,
        end_counter: Some(counter + 1),
        alternatives: vec![
            ClockAlternative {
                id: "date-a".into(),
                label: "Synthetic date A".into(),
                epoch: epoch("epoch-a", 0),
                evidence: vec!["synthetic reference A".into()],
            },
            ClockAlternative {
                id: "date-b".into(),
                label: "Synthetic date B".into(),
                epoch: epoch("epoch-b", 86_400),
                evidence: vec!["synthetic reference B".into()],
            },
        ],
        proxy_excerpts: vec![
            ProxyExcerpt {
                asset_id: first.asset_id.clone(),
                start_seconds: 0.0,
                end_seconds: 2.0,
                role: "selected_proxy".into(),
            },
            ProxyExcerpt {
                asset_id: reference.asset_id.clone(),
                start_seconds: 1.0,
                end_seconds: 3.0,
                role: "independent_reference".into(),
            },
        ],
        anchor: HumanAnchor {
            kind: AnchorKind::EventAtUtc,
            asset_id: first.asset_id.clone(),
            reference_asset_id: Some(reference.asset_id.clone()),
            event_label: "synthetic event".into(),
            utc_time: Some("2026-01-01T12:00:00Z".into()),
        },
        contextual_evidence: vec!["operator overlap supplied by reviewer".into()],
    }
}

#[test]
fn clock_question_accept_reject_revise_and_stale_preview_are_revision_safe() {
    let fixture = fixture(&[
        ("C0001.MP4", "SyntheticCamera", "2026-01-01T09:00:00Z"),
        ("C0002.MP4", "SyntheticCamera", "2026-01-01T10:00:00Z"),
        ("C0003.MP4", "SyntheticCamera", "2026-01-01T11:00:00Z"),
        ("C0001.MP4", "IndependentReference", "2025-12-31T12:00:00Z"),
    ]);
    let original_raw: Vec<_> = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap()
        .assets
        .iter()
        .map(|a| a.captured_at_raw.clone())
        .collect();
    let question = synthetic_clock_question(&fixture);
    let mut invalid_reference = question.clone();
    let selected_asset = invalid_reference
        .proxy_excerpts
        .iter()
        .find(|excerpt| excerpt.role == "selected_proxy")
        .unwrap()
        .asset_id
        .clone();
    invalid_reference
        .proxy_excerpts
        .iter_mut()
        .find(|excerpt| excerpt.role == "independent_reference")
        .unwrap()
        .asset_id = selected_asset;
    assert!(
        fixture
            .catalog
            .preview_clock_block_question(&fixture.project_id, &invalid_reference)
            .unwrap_err()
            .to_string()
            .contains("must be outside the questioned block")
    );
    let proposal_preview = fixture
        .catalog
        .preview_clock_block_question(&fixture.project_id, &question)
        .unwrap();
    assert_eq!(proposal_preview.alternative_impacts.len(), 2);
    assert_eq!(
        proposal_preview.alternative_impacts[1].epoch.offset_seconds
            - proposal_preview.alternative_impacts[0].epoch.offset_seconds,
        86_400
    );
    assert_eq!(
        fixture
            .catalog
            .apply_clock_block_question_preview(&fixture.project_id, &proposal_preview)
            .unwrap(),
        1
    );
    assert_eq!(
        fixture
            .catalog
            .chronology_snapshot(&fixture.project_id)
            .unwrap()
            .clock_questions,
        vec![question.clone()]
    );

    let rejected = ClockQuestionDecision {
        question_id: question.id.clone(),
        kind: ClockQuestionDecisionKind::Rejected,
        alternative_id: None,
        note: "synthetic answer: insufficient evidence".into(),
    };
    let preview = fixture
        .catalog
        .preview_clock_question_decision(&fixture.project_id, &rejected)
        .unwrap();
    let before_tampered_rejection = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap();
    let mut tampered_rejection = preview.clone();
    let mut forged_accepted = fixture
        .catalog
        .preview_clock_question_decision(
            &fixture.project_id,
            &ClockQuestionDecision {
                question_id: question.id.clone(),
                kind: ClockQuestionDecisionKind::Accepted,
                alternative_id: Some("date-b".into()),
                note: "synthetic candidate".into(),
            },
        )
        .unwrap();
    tampered_rejection.clock_epoch_preview = forged_accepted.clock_epoch_preview.take();
    let error = fixture
        .catalog
        .apply_clock_question_decision(&fixture.project_id, &tampered_rejection)
        .unwrap_err()
        .to_string();
    assert!(error.contains("chronology.invalid_reference"));
    assert!(error.contains("rejected decision must not include a clock correction preview"));
    assert_eq!(
        fixture
            .catalog
            .chronology_facts(&fixture.project_id)
            .unwrap(),
        before_tampered_rejection
    );
    assert_eq!(
        fixture
            .catalog
            .chronology_snapshot(&fixture.project_id)
            .unwrap()
            .clock_questions,
        vec![question.clone()]
    );
    fixture
        .catalog
        .apply_clock_question_decision(&fixture.project_id, &preview)
        .unwrap();
    let after_rejection = fixture
        .catalog
        .chronology_snapshot(&fixture.project_id)
        .unwrap();
    assert!(after_rejection.clock_questions.is_empty());
    assert_eq!(
        after_rejection.clock_question_decisions[0].decision.kind,
        ClockQuestionDecisionKind::Rejected
    );
    assert_eq!(
        after_rejection.clock_question_decisions[0].proposal_revision,
        1
    );

    // A revised proposal reopens the same question at the next aggregate revision.
    let mut revised = question.clone();
    revised
        .contextual_evidence
        .push("synthetic additional reference".into());
    assert_eq!(
        fixture
            .catalog
            .propose_clock_block_question(&fixture.project_id, &revised)
            .unwrap(),
        3
    );
    let accepted = ClockQuestionDecision {
        question_id: revised.id.clone(),
        kind: ClockQuestionDecisionKind::Accepted,
        alternative_id: Some("date-b".into()),
        note: "synthetic owner anchor accepted".into(),
    };
    let preview = fixture
        .catalog
        .preview_clock_question_decision(&fixture.project_id, &accepted)
        .unwrap();
    let mut mismatched_epoch = preview.clone();
    mismatched_epoch
        .clock_epoch_preview
        .as_mut()
        .unwrap()
        .epoch
        .offset_seconds += 1;
    let before_mismatched_acceptance = fixture
        .catalog
        .chronology_facts(&fixture.project_id)
        .unwrap();
    let error = fixture
        .catalog
        .apply_clock_question_decision(&fixture.project_id, &mismatched_epoch)
        .unwrap_err()
        .to_string();
    assert!(error.contains("chronology.invalid_reference"));
    assert!(error.contains("clock correction preview does not match the selected alternative"));
    assert_eq!(
        fixture
            .catalog
            .chronology_facts(&fixture.project_id)
            .unwrap(),
        before_mismatched_acceptance
    );
    assert_eq!(
        fixture
            .catalog
            .chronology_snapshot(&fixture.project_id)
            .unwrap()
            .clock_questions,
        vec![revised.clone()]
    );
    assert!(
        preview
            .clock_epoch_preview
            .as_ref()
            .unwrap()
            .impact
            .changes
            .len()
            <= 3
    );

    // Change chronology after preview to prove the facts token protects confirmation.
    fixture
        .catalog
        .set_clock_epoch(
            &fixture.project_id,
            &ClockEpoch {
                id: "independent-epoch".into(),
                capture_sequence_id: revised.capture_sequence_id.clone(),
                start_counter: 1,
                end_counter: Some(1),
                offset_seconds: 0,
                confidence: 1.0,
                evidence: vec!["synthetic".into()],
                label: "synthetic independent edit".into(),
            },
        )
        .unwrap();
    assert!(
        fixture
            .catalog
            .apply_clock_question_decision(&fixture.project_id, &preview)
            .unwrap_err()
            .to_string()
            .contains("stale")
    );
    assert_eq!(
        fixture
            .catalog
            .chronology_snapshot(&fixture.project_id)
            .unwrap()
            .clock_questions,
        vec![revised]
    );
    assert_eq!(
        fixture
            .catalog
            .chronology_facts(&fixture.project_id)
            .unwrap()
            .assets
            .iter()
            .map(|a| a.captured_at_raw.clone())
            .collect::<Vec<_>>(),
        original_raw
    );
}

/// Confirmed chronology changes refresh `best_time` and `offset_seconds`
/// before returning, and a later explicit re-solve is idempotent.
#[test]
fn confirmed_clock_change_projects_atomically_and_resolve_is_idempotent() {
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

    // The mutation already projected the new facts in its own transaction.
    // Projecting again changes nothing, so the columns stay reproducible.
    fixture
        .catalog
        .resolve_chronology(&fixture.project_id)
        .unwrap();
    assert_eq!(asset_ids_in_order(&fixture), ordered_before);
}
