use super::*;
use proptest::prelude::*;
use serde_json::json;
use tempfile::{TempDir, tempdir};

fn fixture() -> (TempDir, Catalog) {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let connection = catalog.connection().unwrap();
    connection.execute("INSERT INTO projects(id, name, created_at) VALUES ('project_test', 'Test', '2026-01-01T00:00:00Z')", []).unwrap();
    connection.execute(
        "INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, notes, labels_json, removed, rotation_degrees, created_at) VALUES ('asset_test', 'project_test', 'fingerprint', '/offline/clip.mp4', 'clip.mp4', 100, 10, 30, 1920, 1080, 'h264', 'DJI', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00.000Z', 'baseline', '[\"arrival\"]', 0, 0, '2026-01-01T00:00:00Z')",
        [],
    ).unwrap();
    (directory, catalog)
}

fn operation(
    id: &str,
    node: &str,
    sequence: u64,
    wall: i64,
    operation_type: &str,
    payload: Value,
) -> ProjectOperation {
    ProjectOperation {
        operation_id: id.to_string(),
        project_id: "project_test".to_string(),
        origin_node_id: node.to_string(),
        origin_sequence: sequence,
        logical_time: HybridLogicalTime {
            wall_millis: wall,
            counter: 0,
            node_id: node.to_string(),
        },
        aggregate_type: "projectAsset".to_string(),
        aggregate_id: "asset_test".to_string(),
        operation_type: operation_type.to_string(),
        payload_version: 1,
        base_revision: None,
        payload,
    }
}

/// The judgment fields as they stand, for a patch that changes one of them.
fn patch_from(asset: &Asset) -> AssetPatch {
    AssetPatch {
        notes: asset.notes.clone(),
        labels: asset.labels.clone(),
        removed: asset.removed,
        rotation_degrees: asset.rotation_degrees,
        expected_revision: Some(asset.revision),
    }
}

#[test]
fn asset_revision_starts_at_zero_and_advances_with_each_write() {
    let (_directory, catalog) = fixture();
    assert_eq!(catalog.get_asset("asset_test").unwrap().revision, 0);

    let mut patch = patch_from(&catalog.get_asset("asset_test").unwrap());
    patch.notes = "first".to_string();
    let after = catalog.update_asset("asset_test", &patch).unwrap();

    assert!(after.revision > 0, "a write must advance the revision");
    assert_eq!(
        catalog.list_assets("project_test").unwrap()[0].revision,
        after.revision
    );
}

#[test]
fn explicit_orientation_decision_confirms_default_and_later_rotation_is_mismatched() {
    let (_directory, catalog) = fixture();
    catalog.connection().unwrap().execute(
        "UPDATE assets SET width=1080,height=1920,default_rotation_degrees=90,rotation_degrees=90,orientation_probed=1 WHERE id='asset_test'", [],
    ).unwrap();
    let revision = catalog.get_asset("asset_test").unwrap().revision;
    let confirmed = catalog
        .decide_orientation(
            "asset_test",
            revision,
            OrientationDecision {
                status: "confirmed".into(),
                rotation_degrees: Some(90),
            },
        )
        .unwrap();
    assert_eq!(confirmed.rotation_degrees, 90);
    assert_eq!(
        confirmed.orientation_decision.unwrap().rotation_degrees,
        Some(90)
    );
    let snapshot = catalog.export_project_baseline("project_test").unwrap();
    assert_eq!(
        snapshot.assets[0]
            .orientation_decision
            .as_ref()
            .unwrap()
            .status,
        "confirmed"
    );
    let restored_dir = tempdir().unwrap();
    let restored = Catalog::import_project_baseline(
        restored_dir.path().join("restored.sqlite3"),
        &snapshot,
        "node_restored",
        1,
        "test-digest",
    )
    .unwrap();
    assert_eq!(
        restored
            .get_asset("asset_test")
            .unwrap()
            .orientation_decision
            .unwrap()
            .rotation_degrees,
        Some(90)
    );
    catalog.set_rotation("asset_test", 180).unwrap();
    let changed = catalog.get_asset("asset_test").unwrap();
    assert_eq!(changed.rotation_degrees, 180);
    assert_eq!(
        changed.orientation_decision.unwrap().rotation_degrees,
        Some(90)
    );
    let queue = catalog
        .orientation_review_queue("project_test", 10, 0)
        .unwrap();
    assert!(
        queue.items[0]
            .review_reasons
            .contains(&"confirmed_value_mismatch".to_string())
    );
}

#[test]
fn orientation_batch_checks_all_revisions_before_any_decision_write() {
    let (_directory, catalog) = fixture();
    catalog.connection().unwrap().execute(
        "INSERT INTO assets(id,project_id,fingerprint,path,filename,size_bytes,duration_seconds,frame_rate,width,height,codec,camera,notes,labels_json,removed,rotation_degrees,created_at) VALUES('asset_second','project_test','f2','/offline/second.mp4','second.mp4',10,1,30,1920,1080,'h264','DJI','','[]',0,0,'2026-01-01T00:00:00Z')", [],
    ).unwrap();
    let before = catalog.get_asset("asset_test").unwrap().revision;
    let error = catalog
        .decide_orientation_batch(&[
            (
                "asset_test".into(),
                before,
                OrientationDecision {
                    status: "confirmed".into(),
                    rotation_degrees: Some(0),
                },
            ),
            (
                "asset_second".into(),
                1,
                OrientationDecision {
                    status: "unresolved".into(),
                    rotation_degrees: None,
                },
            ),
        ])
        .unwrap_err();
    assert!(error.to_string().contains("stale asset revision"));
    assert_eq!(
        catalog
            .get_asset("asset_test")
            .unwrap()
            .orientation_decision,
        None
    );
    assert_eq!(catalog.get_asset("asset_test").unwrap().revision, before);
    assert_eq!(catalog.get_asset("asset_second").unwrap().revision, 0);
    let current = catalog.get_asset("asset_test").unwrap();
    let unresolved = catalog
        .decide_orientation(
            "asset_test",
            current.revision,
            OrientationDecision {
                status: "unresolved".into(),
                rotation_degrees: None,
            },
        )
        .unwrap();
    assert_eq!(unresolved.rotation_degrees, current.rotation_degrees);
    assert_eq!(
        unresolved.orientation_decision.unwrap().status,
        "unresolved"
    );
}

#[test]
fn suspicious_orientation_queue_is_bounded_and_actionable_without_media() {
    let (_directory, catalog) = fixture();
    catalog.connection().unwrap().execute(
        "UPDATE assets SET width=1080,height=1920,default_rotation_degrees=90,rotation_degrees=90,orientation_probed=1 WHERE id='asset_test'", [],
    ).unwrap();
    let page = catalog
        .orientation_review_queue("project_test", 1, 0)
        .unwrap();
    assert_eq!(page.total, 1);
    assert!(
        page.items[0]
            .review_reasons
            .contains(&"portrait_dimensions_need_review".to_string())
    );
    assert!(!page.items[0].evidence_available);
    assert!(
        catalog
            .orientation_review_queue("project_test", 201, 0)
            .is_err()
    );
}

#[test]
fn a_new_asset_projects_an_existing_camera_clock_without_replaying_the_camera() {
    let (_directory, catalog) = fixture();
    catalog
        .create_clock_event_operation(
            "project_test",
            "DJI",
            "clip.mp4",
            3_600,
            "camera was one hour slow",
        )
        .unwrap();
    let corrected_existing = catalog.get_asset("asset_test").unwrap();

    let added = catalog
        .add_portable_project_asset(
            "project_test",
            ProjectAssetAddedPayload {
                id: "asset_later".to_string(),
                fingerprint: "later-fingerprint".to_string(),
                filename: "later.mp4".to_string(),
                size_bytes: 100,
                duration_seconds: 10.0,
                frame_rate: 30.0,
                width: 1920,
                height: 1080,
                codec: "h264".to_string(),
                camera: "DJI".to_string(),
                captured_at_raw: Some("2026-01-01T00:00:00Z".to_string()),
                timestamp_candidates: None,
                // This is what a probe would have supplied before an
                // existing correction is projected at import commit.
                best_time: Some("2026-01-01T00:00:00.000Z".to_string()),
                metadata_rotation_degrees: 0,
                default_rotation_degrees: 0,
                created_at: "2026-01-01T00:00:00Z".to_string(),
            },
        )
        .unwrap();

    assert_eq!(added.offset_seconds, 3_600);
    assert_eq!(added.best_time, corrected_existing.best_time);
    assert_eq!(
        catalog.get_asset("asset_later").unwrap().best_time,
        corrected_existing.best_time
    );
}

#[test]
fn a_new_asset_uses_the_solver_governing_epoch_for_its_capture_sequence() {
    let (_directory, catalog) = fixture();
    let payload = |id: &str, filename: &str| ProjectAssetAddedPayload {
        id: id.to_string(),
        fingerprint: format!("fingerprint-{id}"),
        filename: filename.to_string(),
        size_bytes: 100,
        duration_seconds: 10.0,
        frame_rate: 30.0,
        width: 1920,
        height: 1080,
        codec: "h264".to_string(),
        camera: "DJI".to_string(),
        captured_at_raw: Some("2026-01-01T00:00:00Z".to_string()),
        timestamp_candidates: None,
        best_time: Some("2026-01-01T00:00:00.000Z".to_string()),
        metadata_rotation_degrees: 0,
        default_rotation_degrees: 0,
        created_at: "2026-01-01T00:00:00Z".to_string(),
    };
    catalog
        .add_portable_project_asset("project_test", payload("asset_epoch_first", "DJI_0001.MP4"))
        .unwrap();
    let sequence_id = crate::chronology::capture_sequence_id_for("project_test", "DJI");
    catalog
        .set_clock_epoch(
            "project_test",
            &crate::chronology::ClockEpoch {
                id: "epoch_dji_after_2".to_string(),
                capture_sequence_id: sequence_id,
                start_counter: 2,
                end_counter: None,
                offset_seconds: 7_200,
                confidence: 1.0,
                evidence: vec!["manual clock calibration".to_string()],
                label: "DJI two hours slow".to_string(),
            },
        )
        .unwrap();

    let added = catalog
        .add_portable_project_asset("project_test", payload("asset_epoch_later", "DJI_0002.MP4"))
        .unwrap();

    assert_eq!(added.offset_seconds, 7_200);
    assert_eq!(added.best_time.as_deref(), Some("2026-01-01T02:00:00.000Z"));

    // An exception is user authority over the epoch. Re-run the same
    // bounded projector for this existing/reconnected-row shape and prove
    // its stored offset is the actual raw→exception shift, not the stale
    // epoch's offset.
    catalog
        .set_clip_time_exception(
            "asset_epoch_later",
            "2026-01-01T05:00:00Z",
            "the camera was reset after lunch",
        )
        .unwrap();
    let mut connection = catalog.connection().unwrap();
    let transaction = connection.transaction().unwrap();
    Catalog::project_new_asset_chronology_on_connection(
        &transaction,
        "project_test",
        "asset_epoch_later",
        &crate::chronology::capture_sequence_id_for("project_test", "DJI"),
        Some(2),
        "DJI_0002.MP4",
        Some("2026-01-01T00:00:00Z"),
    )
    .unwrap();
    transaction.commit().unwrap();
    let exceptional = catalog.get_asset("asset_epoch_later").unwrap();
    assert_eq!(exceptional.offset_seconds, 18_000);
    assert_eq!(
        exceptional.best_time.as_deref(),
        Some("2026-01-01T05:00:00.000Z")
    );
}

#[test]
fn a_patch_built_from_the_current_revision_is_applied() {
    let (_directory, catalog) = fixture();
    let mut patch = patch_from(&catalog.get_asset("asset_test").unwrap());
    patch.notes = "the station clock is legible".to_string();

    let updated = catalog.update_asset("asset_test", &patch).unwrap();

    assert_eq!(updated.notes, "the station clock is legible");
}

#[test]
fn a_patch_built_from_a_stale_read_is_refused_rather_than_applied() {
    let (_directory, catalog) = fixture();
    // The desktop reads, and a second writer — an agent annotating the
    // queue — commits before the human finishes typing.
    let stale = catalog.get_asset("asset_test").unwrap();
    let mut agent = patch_from(&stale);
    agent.labels = vec!["arrival".to_string(), "market".to_string()];
    catalog.update_asset("asset_test", &agent).unwrap();

    let mut human = patch_from(&stale);
    human.notes = "typed while the agent was writing".to_string();
    let refused = catalog.update_asset("asset_test", &human).unwrap_err();

    assert!(
        refused.to_string().starts_with(STALE_ASSET_REVISION),
        "expected a stale-revision refusal, got: {refused}"
    );
    // A whole patch carries every field, so applying this one would not
    // merely have missed the label — it would have removed it.
    let current = catalog.get_asset("asset_test").unwrap();
    assert_eq!(
        current.labels,
        vec!["arrival".to_string(), "market".to_string()]
    );
    assert_eq!(current.notes, "baseline");
}

#[test]
fn the_refused_write_leaves_no_operation_behind() {
    let (_directory, catalog) = fixture();
    let stale = catalog.get_asset("asset_test").unwrap();
    let mut agent = patch_from(&stale);
    agent.removed = true;
    catalog.update_asset("asset_test", &agent).unwrap();
    let before = catalog
        .operation_diagnostics("project_test")
        .unwrap()
        .operation_count;

    let mut human = patch_from(&stale);
    human.notes = "lost".to_string();
    assert!(catalog.update_asset("asset_test", &human).is_err());

    assert_eq!(
        catalog
            .operation_diagnostics("project_test")
            .unwrap()
            .operation_count,
        before,
    );
}

#[test]
fn re_reading_after_a_refusal_lets_the_same_edit_through() {
    let (_directory, catalog) = fixture();
    let stale = catalog.get_asset("asset_test").unwrap();
    let mut agent = patch_from(&stale);
    agent.labels = vec!["arrival".to_string(), "market".to_string()];
    catalog.update_asset("asset_test", &agent).unwrap();
    let mut human = patch_from(&stale);
    human.notes = "typed while the agent was writing".to_string();
    assert!(catalog.update_asset("asset_test", &human).is_err());

    // The prescribed recovery: read again, keep the human's delta, retry.
    let mut retry = patch_from(&catalog.get_asset("asset_test").unwrap());
    retry.notes = human.notes.clone();
    let settled = catalog.update_asset("asset_test", &retry).unwrap();

    assert_eq!(settled.notes, "typed while the agent was writing");
    assert_eq!(
        settled.labels,
        vec!["arrival".to_string(), "market".to_string()]
    );
}

#[test]
fn a_patch_with_no_expectation_keeps_the_previous_last_write_wins_behaviour() {
    let (_directory, catalog) = fixture();
    let stale = catalog.get_asset("asset_test").unwrap();
    let mut agent = patch_from(&stale);
    agent.labels = vec!["arrival".to_string(), "market".to_string()];
    catalog.update_asset("asset_test", &agent).unwrap();

    // Ingest-time and scripted writers have no revision to send, and must
    // keep working exactly as they did.
    let blind = AssetPatch {
        expected_revision: None,
        ..patch_from(&stale)
    };
    assert!(catalog.update_asset("asset_test", &blind).is_ok());
}

#[test]
fn an_unchanged_patch_at_the_current_revision_is_still_a_no_op() {
    let (_directory, catalog) = fixture();
    let asset = catalog.get_asset("asset_test").unwrap();

    catalog
        .update_asset("asset_test", &patch_from(&asset))
        .unwrap();

    assert_eq!(
        catalog
            .operation_diagnostics("project_test")
            .unwrap()
            .operation_count,
        0
    );
}

#[test]
fn unchanged_patches_allocate_no_identity_sequence_or_operation() {
    let (_directory, catalog) = fixture();
    let asset = catalog.get_asset("asset_test").unwrap();
    catalog
        .update_asset(
            "asset_test",
            &AssetPatch {
                notes: asset.notes,
                labels: asset.labels,
                removed: asset.removed,
                rotation_degrees: asset.rotation_degrees,
                expected_revision: None,
            },
        )
        .unwrap();
    assert_eq!(catalog.local_node_id().unwrap(), None);
    assert_eq!(
        catalog
            .operation_diagnostics("project_test")
            .unwrap()
            .operation_count,
        0
    );
}

#[test]
fn compatibility_patch_is_atomic_and_emits_only_explicit_changes() {
    let (_directory, catalog) = fixture();
    let updated = catalog
        .update_asset(
            "asset_test",
            &AssetPatch {
                notes: "new note".to_string(),
                labels: vec!["market".to_string()],
                removed: true,
                rotation_degrees: 90,
                expected_revision: None,
            },
        )
        .unwrap();
    assert_eq!(updated.notes, "new note");
    assert_eq!(updated.labels, ["market"]);
    assert!(updated.removed);
    assert_eq!(updated.rotation_degrees, 90);
    assert_eq!(
        catalog
            .operation_diagnostics("project_test")
            .unwrap()
            .operation_count,
        5
    );

    let error = catalog
        .update_asset(
            "asset_test",
            &AssetPatch {
                notes: "must roll back".to_string(),
                labels: vec!["x".repeat(65)],
                removed: false,
                rotation_degrees: 180,
                expected_revision: None,
            },
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("64 characters"));
    let unchanged = catalog.get_asset("asset_test").unwrap();
    assert_eq!(unchanged.notes, "new note");
    assert_eq!(
        catalog
            .operation_diagnostics("project_test")
            .unwrap()
            .operation_count,
        5
    );
}

#[test]
fn note_and_lww_materialization_converge_under_reversed_delivery() {
    let (_left_dir, left) = fixture();
    let (_right_dir, right) = fixture();
    let early_note = operation(
        "op_note_early",
        "node_a",
        1,
        10,
        NOTE_REVISION_CREATED,
        json!({"revisionId":"note_early","text":"early"}),
    );
    let late_note = operation(
        "op_note_late",
        "node_b",
        1,
        20,
        NOTE_REVISION_CREATED,
        json!({"revisionId":"note_late","text":"late"}),
    );
    let early_removed = operation(
        "op_removed_early",
        "node_a",
        2,
        11,
        REMOVED_SET,
        json!({"removed":true}),
    );
    let late_removed = operation(
        "op_removed_late",
        "node_b",
        2,
        21,
        REMOVED_SET,
        json!({"removed":false}),
    );
    for value in [&early_note, &late_note, &early_removed, &late_removed] {
        left.apply_operation(value).unwrap();
    }
    for value in [&late_removed, &early_removed, &late_note, &early_note] {
        right.apply_operation(value).unwrap();
    }
    for catalog in [&left, &right] {
        let asset = catalog.get_asset("asset_test").unwrap();
        assert_eq!(asset.notes, "late");
        assert!(!asset.removed);
    }
    assert_eq!(
        left.apply_operation(&late_note).unwrap(),
        ApplyOperationResult::Duplicate
    );
    assert_eq!(left.list_operations("project_test").unwrap().len(), 4);
}

#[test]
fn labels_are_add_wins_observed_remove_even_when_delivered_out_of_order() {
    let (_directory, catalog) = fixture();
    let remove = operation(
        "op_remove",
        "node_a",
        2,
        20,
        LABEL_REMOVED,
        json!({"normalizedLabel":"market","observedAddTags":["tag_observed"]}),
    );
    let observed_add = operation(
        "op_add_observed",
        "node_a",
        1,
        10,
        LABEL_ADDED,
        json!({"addTag":"tag_observed","label":"Market"}),
    );
    let concurrent_add = operation(
        "op_add_concurrent",
        "node_b",
        1,
        15,
        LABEL_ADDED,
        json!({"addTag":"tag_concurrent","label":"MARKET"}),
    );
    catalog.apply_operation(&remove).unwrap();
    catalog.apply_operation(&observed_add).unwrap();
    catalog.apply_operation(&concurrent_add).unwrap();
    assert_eq!(
        catalog.get_asset("asset_test").unwrap().labels,
        ["arrival", "MARKET"]
    );
}

#[test]
fn stale_clock_base_creates_a_conflict_without_replacing_chronology() {
    let (_directory, catalog) = fixture();
    let mut first = operation(
        "op_clock_first",
        "node_a",
        1,
        10,
        CLOCK_EVENT_CREATED,
        json!({"eventId":"clock_first","camera":"DJI","startFilename":"clip.mp4","offsetSeconds":60,"label":"first"}),
    );
    first.aggregate_type = "clock".to_string();
    first.aggregate_id = "DJI".to_string();
    first.base_revision = Some(0);
    let mut stale = operation(
        "op_clock_stale",
        "node_b",
        1,
        20,
        CLOCK_EVENT_CREATED,
        json!({"eventId":"clock_stale","camera":"DJI","startFilename":"clip.mp4","offsetSeconds":120,"label":"stale"}),
    );
    stale.aggregate_type = "clock".to_string();
    stale.aggregate_id = "DJI".to_string();
    stale.base_revision = Some(0);
    assert_eq!(
        catalog.apply_operation(&first).unwrap(),
        ApplyOperationResult::Applied
    );
    assert_eq!(
        catalog.apply_operation(&stale).unwrap(),
        ApplyOperationResult::Conflict
    );
    assert_eq!(catalog.get_asset("asset_test").unwrap().offset_seconds, 60);
    assert_eq!(
        catalog
            .operation_diagnostics("project_test")
            .unwrap()
            .unresolved_conflicts
            .len(),
        1
    );
}

#[test]
fn restore_sequence_recovery_blocks_reuse_and_records_divergence_decision() {
    let (_directory, catalog) = fixture();
    catalog.set_local_node_id("node_server").unwrap();
    let marked = catalog
        .mark_sequence_recovery("node_mac", "node_server", 0)
        .unwrap();
    assert_eq!(marked.state, SequenceRecoveryState::Awaiting);
    assert!(
        catalog
            .create_note_revision("asset_test", "blocked")
            .is_err()
    );

    let divergent = catalog
        .reconcile_sequence_recovery("node_mac", 7)
        .unwrap()
        .unwrap();
    assert_eq!(divergent.state, SequenceRecoveryState::Divergent);
    assert_eq!(divergent.divergence_start, Some(1));
    assert_eq!(divergent.divergence_end, Some(7));
    assert!(
        catalog
            .create_note_revision("asset_test", "still blocked")
            .is_err()
    );

    catalog
        .accept_sequence_divergence("node_mac", "accept_allocator_advance")
        .unwrap();
    catalog
        .create_note_revision("asset_test", "after decision")
        .unwrap();
    let operations = catalog.list_operations("project_test").unwrap();
    assert_eq!(operations.last().unwrap().origin_sequence, 8);
}

#[test]
fn restore_sequence_recovery_completes_after_exact_operations_are_replayed() {
    let (_directory, catalog) = fixture();
    catalog.set_local_node_id("node_server").unwrap();
    catalog
        .mark_sequence_recovery("node_mac", "node_server", 0)
        .unwrap();
    let divergent = catalog
        .reconcile_sequence_recovery("node_mac", 2)
        .unwrap()
        .unwrap();
    assert_eq!(divergent.state, SequenceRecoveryState::Divergent);

    let first = operation(
        "op_recovered_1",
        "node_server",
        1,
        10,
        LABEL_ADDED,
        json!({"addTag":"tag_recovered_1","label":"recovered-one"}),
    );
    let second = operation(
        "op_recovered_2",
        "node_server",
        2,
        20,
        LABEL_ADDED,
        json!({"addTag":"tag_recovered_2","label":"recovered-two"}),
    );
    catalog
        .apply_operation_batch("node_server", &[first, second])
        .unwrap();
    let completed = catalog
        .reconcile_sequence_recovery("node_mac", 2)
        .unwrap()
        .unwrap();
    assert_eq!(completed.state, SequenceRecoveryState::Completed);
    assert_eq!(completed.contained_watermark, 2);

    catalog
        .create_note_revision("asset_test", "after exact recovery")
        .unwrap();
    let operations = catalog.list_operations("project_test").unwrap();
    assert_eq!(operations.last().unwrap().origin_sequence, 3);
}

#[test]
fn operation_cursors_include_local_history_and_peer_acknowledgements_are_monotonic() {
    let (_directory, catalog) = fixture();
    catalog.set_local_node_id("node_mac").unwrap();
    catalog
        .create_note_revision("asset_test", "local history")
        .unwrap();
    assert_eq!(catalog.operation_cursors().unwrap()["node_mac"], 1);

    catalog
        .record_peer_operation_acknowledgements(
            "node_server",
            &std::collections::BTreeMap::from([("node_mac".to_string(), 1)]),
        )
        .unwrap();
    catalog
        .record_peer_operation_acknowledgements(
            "node_server",
            &std::collections::BTreeMap::from([("node_mac".to_string(), 0)]),
        )
        .unwrap();
    assert_eq!(
        catalog
            .peer_operation_acknowledgements("node_server")
            .unwrap()["node_mac"],
        1
    );
}

#[test]
fn remote_hlc_beyond_future_skew_is_rejected_without_advancing_state() {
    let (_directory, catalog) = fixture();
    let future = Utc::now().timestamp_millis() + MAX_REMOTE_HLC_FUTURE_SKEW_MILLIS + 10_000;
    let incoming = operation(
        "op_future",
        "node_mac",
        1,
        future,
        REMOVED_SET,
        json!({"removed":true}),
    );
    assert!(
        catalog
            .apply_operation_batch("node_mac", &[incoming])
            .is_err()
    );
    assert!(catalog.operation_cursors().unwrap().is_empty());
    assert!(!catalog.get_asset("asset_test").unwrap().removed);
}

proptest! {
    #[test]
    fn lww_rotation_converges_for_shuffled_delivery(order_keys in any::<[u8; 3]>()) {
        let (_directory, catalog) = fixture();
        let operations = [
            operation("op_rotation_1", "node_a", 1, 10, ROTATION_SET, json!({"rotationDegrees":90})),
            operation("op_rotation_2", "node_b", 1, 30, ROTATION_SET, json!({"rotationDegrees":270})),
            operation("op_rotation_3", "node_a", 2, 20, ROTATION_SET, json!({"rotationDegrees":180})),
        ];
        let mut indices = [0_usize, 1, 2];
        indices.sort_by_key(|index| (order_keys[*index], *index));
        for index in indices {
            catalog.apply_operation(&operations[index]).unwrap();
        }
        prop_assert_eq!(catalog.get_asset("asset_test").unwrap().rotation_degrees, 270);
    }
}
