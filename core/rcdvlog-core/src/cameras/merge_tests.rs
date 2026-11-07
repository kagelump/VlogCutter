use super::*;
use crate::now;
use crate::operations::{
    CAPTURE_SEQUENCE_DECLARED, CaptureSequenceDeclaredPayload, HybridLogicalTime,
    PROJECT_ASSET_ADDED, ProjectAssetAddedPayload, ProjectOperation,
};
use rusqlite::params;
use tempfile::tempdir;

fn fixture() -> (tempfile::TempDir, Catalog, String) {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Camera merge").unwrap();
    let mut connection = catalog.connection().unwrap();
    for (id, camera) in [
        ("a_wrong_1", "Sony wrong"),
        ("a_wrong_2", "Sony wrong"),
        ("a_right", "Sony"),
    ] {
        connection.execute(
            "INSERT INTO assets (id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, created_at) VALUES (?1, ?2, ?3, '', ?4, 1, 1, 30, 1920, 1080, 'h264', ?5, NULL, NULL, ?6)",
            params![id, project.id, format!("fp-{id}"), format!("{id}.MP4"), camera, now()],
        ).unwrap();
    }
    for (id, camera) in [
        ("card_wrong_a", "Sony wrong"),
        ("card_wrong_b", "Sony wrong"),
        ("card_right_a", "Sony"),
        ("card_right_b", "Sony"),
    ] {
        connection.execute(
            "INSERT INTO capture_sequences (id, project_id, camera_key, label, volume_hint, filename_prefix, created_at) VALUES (?1, ?2, ?3, ?1, '', '', ?4)",
            params![id, project.id, camera, now()],
        ).unwrap();
    }
    for (asset, sequence) in [
        ("a_wrong_1", "card_wrong_a"),
        ("a_wrong_2", "card_wrong_b"),
        ("a_right", "card_right_a"),
    ] {
        connection.execute(
            "INSERT INTO asset_capture_sequence (asset_id, project_id, capture_sequence_id, filename_counter) VALUES (?1, ?2, ?3, 1)",
            params![asset, project.id, sequence],
        ).unwrap();
    }
    connection.execute(
        "INSERT INTO camera_identity_mappings (id, project_id, folder_hint, filename_prefix, exif_hint, camera, created_at, updated_at) VALUES ('map_wrong', ?1, 'Sony', 'C', 'SONY ILCE', 'Sony wrong', ?2, ?2)",
        params![project.id, now()],
    ).unwrap();
    connection.execute(
        "INSERT INTO clock_events (id, project_id, camera, start_filename, offset_seconds, label, created_at) VALUES ('legacy_clock_wrong', ?1, 'Sony wrong', 'a_wrong_1.MP4', 30, 'legacy', ?2)",
        params![project.id, now()],
    ).unwrap();
    let transaction = connection.transaction().unwrap();
    for asset_id in ["a_wrong_1", "a_wrong_2", "a_right"] {
        crate::operations::reconcile_asset_baseline(&transaction, &project.id, asset_id).unwrap();
    }
    transaction.commit().unwrap();
    drop(connection);
    let declarations = [
        ("card_wrong_a", "Sony wrong"),
        ("card_wrong_b", "Sony wrong"),
        ("card_right_a", "Sony"),
        ("card_right_b", "Sony"),
    ]
    .into_iter()
    .map(|(id, camera)| NewOperation {
        aggregate_type: "captureSequence",
        aggregate_id: id.to_string(),
        operation_type: CAPTURE_SEQUENCE_DECLARED,
        base_revision: Some(0),
        payload: serde_json::to_value(CaptureSequenceDeclaredPayload {
            sequence_id: id.to_string(),
            camera_key: camera.to_string(),
            label: id.to_string(),
            volume_hint: String::new(),
            filename_prefix: String::new(),
        })
        .unwrap(),
    })
    .collect();
    catalog.commit_local(&project.id, declarations).unwrap();
    catalog
        .set_clock_epoch(
            &project.id,
            &crate::ClockEpoch {
                id: "epoch_wrong".to_string(),
                capture_sequence_id: "card_wrong_a".to_string(),
                start_counter: 0,
                end_counter: None,
                offset_seconds: 30,
                confidence: 1.0,
                evidence: vec!["user_set".to_string()],
                label: "kept".to_string(),
            },
        )
        .unwrap();
    (directory, catalog, project.id)
}

#[test]
fn exact_repair_changes_asset_and_sequence_identity_without_merging_cards() {
    let (_directory, catalog, project_id) = fixture();
    let selection = vec!["a_wrong_1".to_string()];
    let preview = catalog
        .preview_camera_repair(&project_id, "DJI Pocket 3", &selection)
        .unwrap();
    assert_eq!(preview.capture_sequence_ids, vec!["card_wrong_a"]);
    assert_eq!(catalog.apply_camera_repair(&preview).unwrap(), 1);
    let asset = catalog.get_asset("a_wrong_1").unwrap();
    assert_eq!(asset.camera, "DJI Pocket 3");
    let connection = catalog.connection().unwrap();
    let camera: String = connection
        .query_row(
            "SELECT camera_key FROM capture_sequences WHERE id='card_wrong_a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let other: String = connection
        .query_row(
            "SELECT camera_key FROM capture_sequences WHERE id='card_wrong_b'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(camera, "DJI Pocket 3");
    assert_eq!(other, "Sony wrong");
    assert!(
        catalog
            .apply_camera_repair(&preview)
            .unwrap_err()
            .to_string()
            .contains("stale")
    );
}

#[test]
fn exact_unknown_repair_keeps_an_unselected_card_and_its_counter_space() {
    let (_directory, catalog, project_id) = fixture();
    let connection = catalog.connection().unwrap();
    connection
        .execute(
            "UPDATE assets SET camera='Unknown camera' WHERE id IN ('a_wrong_1','a_wrong_2')",
            [],
        )
        .unwrap();
    connection.execute("UPDATE assets SET filename=CASE id WHEN 'a_wrong_1' THEN 'synthetic-alpha.mp4' ELSE 'synthetic-beta.mp4' END WHERE id IN ('a_wrong_1','a_wrong_2')",[]).unwrap();
    connection.execute("UPDATE capture_sequences SET camera_key='Unknown camera' WHERE id IN ('card_wrong_a','card_wrong_b')",[]).unwrap();
    connection
        .execute(
            "UPDATE asset_capture_sequence SET filename_counter=42 WHERE asset_id='a_wrong_1'",
            [],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE asset_capture_sequence SET filename_counter=7 WHERE asset_id='a_wrong_2'",
            [],
        )
        .unwrap();
    drop(connection);

    let preview = catalog
        .preview_camera_repair(&project_id, "DJI Pocket 3", &["a_wrong_1".into()])
        .unwrap();
    assert_eq!(preview.capture_sequence_ids, vec!["card_wrong_a"]);
    catalog.apply_camera_repair(&preview).unwrap();

    let connection = catalog.connection().unwrap();
    let selected:(String,String,i64)=connection.query_row("SELECT a.camera,acs.capture_sequence_id,acs.filename_counter FROM assets a JOIN asset_capture_sequence acs ON acs.asset_id=a.id WHERE a.id='a_wrong_1'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    let retained:(String,String,String,i64)=connection.query_row("SELECT a.camera,acs.capture_sequence_id,cs.camera_key,acs.filename_counter FROM assets a JOIN asset_capture_sequence acs ON acs.asset_id=a.id JOIN capture_sequences cs ON cs.id=acs.capture_sequence_id WHERE a.id='a_wrong_2'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    assert_eq!(selected, ("DJI Pocket 3".into(), "card_wrong_a".into(), 42));
    assert_eq!(
        retained,
        (
            "Unknown camera".into(),
            "card_wrong_b".into(),
            "Unknown camera".into(),
            7
        )
    );
}

#[test]
fn partial_card_repair_creates_a_separate_sequence() {
    let (_directory, catalog, project_id) = fixture();
    catalog.connection().unwrap().execute("UPDATE asset_capture_sequence SET capture_sequence_id='card_wrong_a' WHERE asset_id='a_wrong_2'",[]).unwrap();
    let preview = catalog
        .preview_camera_repair(&project_id, "DJI Pocket 3", &["a_wrong_1".into()])
        .unwrap();
    assert_eq!(preview.new_sequences.len(), 1);
    catalog.apply_camera_repair(&preview).unwrap();
    let connection = catalog.connection().unwrap();
    let selected: String = connection
        .query_row(
            "SELECT capture_sequence_id FROM asset_capture_sequence WHERE asset_id='a_wrong_1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let retained: String = connection
        .query_row(
            "SELECT capture_sequence_id FROM asset_capture_sequence WHERE asset_id='a_wrong_2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(selected, retained);
    assert_eq!(retained, "card_wrong_a");
    let camera: String = connection
        .query_row(
            "SELECT camera_key FROM capture_sequences WHERE id=?1",
            [selected],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(camera, "DJI Pocket 3");
}

#[test]
fn preview_repairs_legacy_null_counter_without_changing_filename() {
    let (_directory, catalog, project_id) = fixture();
    catalog.connection().unwrap().execute("UPDATE assets SET filename='20300102030405_DJI_0042_D.MP4',captured_at_raw='2030-01-02T03:04:05' WHERE id='a_wrong_1'",[]).unwrap();
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE asset_capture_sequence SET filename_counter=NULL WHERE asset_id='a_wrong_1'",
            [],
        )
        .unwrap();
    let preview = catalog
        .preview_camera_repair(&project_id, "DJI Pocket 3", &["a_wrong_1".into()])
        .unwrap();
    assert_eq!(preview.counters_to_repair, 1);
    catalog.apply_camera_repair(&preview).unwrap();
    let connection = catalog.connection().unwrap();
    let (filename,counter,raw):(String,i64,String)=connection.query_row("SELECT a.filename,acs.filename_counter,a.captured_at_raw FROM assets a JOIN asset_capture_sequence acs ON acs.asset_id=a.id WHERE a.id='a_wrong_1'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get::<_,Option<String>>(2)?.unwrap_or_default()))).unwrap();
    assert_eq!(filename, "20300102030405_DJI_0042_D.MP4");
    assert_eq!(counter, 42);
    assert_eq!(raw, "2030-01-02T03:04:05");
}

#[test]
fn stale_preview_refuses_before_any_repair_write() {
    let (_directory, catalog, project_id) = fixture();
    let preview = catalog
        .preview_camera_repair(&project_id, "DJI Pocket 3", &["a_wrong_1".into()])
        .unwrap();
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET camera='changed after preview' WHERE id='a_wrong_1'",
            [],
        )
        .unwrap();
    assert!(
        catalog
            .apply_camera_repair(&preview)
            .unwrap_err()
            .to_string()
            .contains("stale")
    );
    assert_eq!(
        catalog.get_asset("a_wrong_1").unwrap().camera,
        "changed after preview"
    );
    let operations: i64 = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM project_operations WHERE operation_type='cameraAssetsRepaired'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(operations, 0);
}

#[test]
fn merge_preserves_cards_and_epochs_while_moving_all_identity_projections() {
    let (_directory, catalog, project_id) = fixture();
    let before_operations: i64 = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM project_operations WHERE project_id=?1",
            [&project_id],
            |row| row.get(0),
        )
        .unwrap();

    let preview = catalog
        .preview_camera_merge(&project_id, "Sony wrong", "Sony")
        .unwrap();
    assert_eq!(preview.affected_clip_count, 2);
    assert_eq!(preview.affected_capture_sequence_count, 2);
    assert_eq!(preview.destination_capture_sequence_count, 2);
    assert_eq!(preview.affected_mapping_count, 1);

    let report = catalog
        .merge_camera(&project_id, "Sony wrong", "Sony")
        .unwrap();
    assert_eq!(report.moved_clip_count, 2);
    assert_eq!(report.moved_capture_sequence_count, 2);
    assert_eq!(report.moved_mapping_count, 1);

    let connection = catalog.connection().unwrap();
    let source_assets: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM assets WHERE project_id=?1 AND camera='Sony wrong'",
            [&project_id],
            |row| row.get(0),
        )
        .unwrap();
    let destination_sequences: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM capture_sequences WHERE project_id=?1 AND camera_key='Sony'",
            [&project_id],
            |row| row.get(0),
        )
        .unwrap();
    let epoch_sequence: String = connection
        .query_row(
            "SELECT capture_sequence_id FROM clock_epochs WHERE id='epoch_wrong'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mapping_camera: String = connection
        .query_row(
            "SELECT camera FROM camera_identity_mappings WHERE id='map_wrong'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let legacy_clock_camera: String = connection
        .query_row(
            "SELECT camera FROM clock_events WHERE id='legacy_clock_wrong'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let after_operations: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM project_operations WHERE project_id=?1",
            [&project_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source_assets, 0);
    assert_eq!(
        destination_sequences, 4,
        "destination may already contain multiple cards"
    );
    assert_eq!(
        epoch_sequence, "card_wrong_a",
        "epoch keeps its card identity"
    );
    assert_eq!(
        mapping_camera, "Sony",
        "future matching cards stay in the merged lane"
    );
    assert_eq!(
        legacy_clock_camera, "Sony",
        "legacy camera-clock rows follow the typed merge"
    );
    assert_eq!(
        after_operations,
        before_operations + 1,
        "identity repair is an immutable project operation"
    );
    let operation_type: String = connection.query_row(
        "SELECT operation_type FROM project_operations WHERE project_id=?1 ORDER BY origin_sequence DESC LIMIT 1",
        [&project_id],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(operation_type, CAMERA_IDENTITY_MERGED);

    let merge_wall: i64 = connection.query_row(
        "SELECT hlc_wall_millis FROM project_operations WHERE project_id=?1 AND operation_type=?2",
        params![project_id, CAMERA_IDENTITY_MERGED],
        |row| row.get(0),
    ).unwrap();
    drop(connection);
    let late_declaration = ProjectOperation {
        operation_id: "op_late_old_camera_card".to_string(),
        project_id: project_id.clone(),
        origin_node_id: "node_remote_camera".to_string(),
        origin_sequence: 1,
        logical_time: HybridLogicalTime {
            wall_millis: merge_wall + 1,
            counter: 0,
            node_id: "node_remote_camera".to_string(),
        },
        aggregate_type: "captureSequence".to_string(),
        aggregate_id: "card_late".to_string(),
        operation_type: CAPTURE_SEQUENCE_DECLARED.to_string(),
        payload_version: 1,
        base_revision: Some(0),
        payload: serde_json::json!({
            "sequenceId": "card_late",
            "cameraKey": "Sony wrong",
            "label": "Late replayed card",
            "volumeHint": "",
            "filenamePrefix": "C"
        }),
    };
    catalog.apply_operation(&late_declaration).unwrap();
    let late_camera: String = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT camera_key FROM capture_sequences WHERE id='card_late'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        late_camera, "Sony",
        "replayed old-camera facts follow the durable alias"
    );

    let late_asset = ProjectOperation {
        operation_id: "op_late_old_camera_asset".to_string(),
        project_id: project_id.clone(),
        origin_node_id: "node_remote_camera".to_string(),
        origin_sequence: 2,
        logical_time: HybridLogicalTime {
            wall_millis: merge_wall + 2,
            counter: 0,
            node_id: "node_remote_camera".to_string(),
        },
        aggregate_type: "projectAsset".to_string(),
        aggregate_id: "a_late".to_string(),
        operation_type: PROJECT_ASSET_ADDED.to_string(),
        payload_version: 1,
        base_revision: Some(0),
        payload: serde_json::to_value(ProjectAssetAddedPayload {
            id: "a_late".to_string(),
            fingerprint: "fp-a-late".to_string(),
            filename: "C0099.MP4".to_string(),
            size_bytes: 1,
            duration_seconds: 1.0,
            frame_rate: 30.0,
            width: 1920,
            height: 1080,
            codec: "h264".to_string(),
            camera: "Sony wrong".to_string(),
            captured_at_raw: None,
            timestamp_candidates: None,
            best_time: None,
            metadata_rotation_degrees: 0,
            default_rotation_degrees: 0,
            created_at: now(),
        })
        .unwrap(),
    };
    catalog.apply_operation(&late_asset).unwrap();
    assert_eq!(catalog.get_asset("a_late").unwrap().camera, "Sony");

    let late_clock = ProjectOperation {
        operation_id: "op_late_old_camera_clock".to_string(),
        project_id: project_id.clone(),
        origin_node_id: "node_remote_camera".to_string(),
        origin_sequence: 3,
        logical_time: HybridLogicalTime {
            wall_millis: merge_wall + 3,
            counter: 0,
            node_id: "node_remote_camera".to_string(),
        },
        aggregate_type: "clock".to_string(),
        aggregate_id: "Sony wrong".to_string(),
        operation_type: crate::operations::CLOCK_EVENT_CREATED.to_string(),
        payload_version: 1,
        base_revision: Some(0),
        payload: serde_json::json!({
            "eventId": "late_old_camera_clock",
            "camera": "Sony wrong",
            "startFilename": "C0099.MP4",
            "offsetSeconds": 45,
            "label": "late remote clock"
        }),
    };
    catalog.apply_operation(&late_clock).unwrap();
    let connection = catalog.connection().unwrap();
    let stored_camera: String = connection
        .query_row(
            "SELECT camera FROM clock_events WHERE id='late_old_camera_clock'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let epoch_camera: String = connection
        .query_row(
            "SELECT s.camera_key FROM clock_epochs e JOIN capture_sequences s ON s.id=e.capture_sequence_id WHERE e.id='late_old_camera_clock'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_camera, "Sony");
    assert_eq!(epoch_camera, "Sony");
}

#[test]
fn merge_requires_two_existing_distinct_project_cameras() {
    let (_directory, catalog, project_id) = fixture();
    assert!(
        catalog
            .preview_camera_merge(&project_id, "Sony", "Sony")
            .is_err()
    );
    assert!(
        catalog
            .preview_camera_merge(&project_id, "Sony wrong", "Missing")
            .is_err()
    );
    assert!(
        catalog
            .preview_camera_merge("missing", "Sony wrong", "Sony")
            .is_err()
    );
}

#[test]
fn baseline_round_trip_replays_merge_without_collapsing_source_cards() {
    let (directory, catalog, project_id) = fixture();
    catalog
        .merge_camera(&project_id, "Sony wrong", "Sony")
        .unwrap();
    let snapshot = catalog.export_project_baseline(&project_id).unwrap();
    let restored = Catalog::import_project_baseline(
        directory.path().join("restored.sqlite3"),
        &snapshot,
        "node_camera_restore",
        1,
        "camera-merge-digest",
    )
    .unwrap();
    let connection = restored.connection().unwrap();
    let cards = connection.prepare(
        "SELECT id, camera_key FROM capture_sequences WHERE project_id=?1 AND id LIKE 'card_%' ORDER BY id",
    ).unwrap().query_map([&project_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    assert_eq!(cards.len(), 4);
    assert!(cards.iter().all(|(_, camera)| camera == "Sony"));
    let assignments = connection.prepare(
        "SELECT asset_id, capture_sequence_id FROM asset_capture_sequence WHERE project_id=?1 AND asset_id LIKE 'a_wrong_%' ORDER BY asset_id",
    ).unwrap().query_map([&project_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    assert_eq!(
        assignments,
        vec![
            ("a_wrong_1".to_string(), "card_wrong_a".to_string()),
            ("a_wrong_2".to_string(), "card_wrong_b".to_string()),
        ]
    );
    let epoch_sequence: String = connection
        .query_row(
            "SELECT capture_sequence_id FROM clock_epochs WHERE id='epoch_wrong'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(epoch_sequence, "card_wrong_a");
}

fn convergence_fixture(name: &str) -> (tempfile::TempDir, Catalog, String) {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join(format!("{name}.sqlite3"))).unwrap();
    let project = catalog.create_project(name).unwrap();
    let connection = catalog.connection().unwrap();
    for camera in ["A", "B", "C"] {
        connection.execute(
            "INSERT INTO assets (id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, created_at) VALUES (?1, ?2, ?3, '', ?4, 1, 1, 30, 1, 1, 'h264', ?5, NULL, NULL, ?6)",
            params![format!("asset_{camera}"), project.id, format!("fp_{camera}"), format!("{camera}.MP4"), camera, now()],
        ).unwrap();
        connection.execute(
            "INSERT INTO capture_sequences (id, project_id, camera_key, label, volume_hint, filename_prefix, created_at) VALUES (?1, ?2, ?3, ?3, '', '', ?4)",
            params![format!("card_{camera}"), project.id, camera, now()],
        ).unwrap();
        connection.execute(
            "INSERT INTO camera_identity_mappings (id, project_id, folder_hint, filename_prefix, exif_hint, camera, created_at, updated_at) VALUES (?1, ?2, ?3, ?3, ?3, ?3, ?4, ?4)",
            params![format!("mapping_{camera}"), project.id, camera, now()],
        ).unwrap();
        connection.execute(
            "INSERT INTO clock_events (id, project_id, camera, start_filename, offset_seconds, label, created_at) VALUES (?1, ?2, ?3, 'A.MP4', 0, ?3, ?4)",
            params![format!("clock_{camera}"), project.id, camera, now()],
        ).unwrap();
    }
    (directory, catalog, project.id)
}

fn merge_fact(
    project_id: &str,
    operation_id: &str,
    origin: &str,
    wall_millis: i64,
    source: &str,
    destination: &str,
) -> ProjectOperation {
    ProjectOperation {
        operation_id: operation_id.to_string(),
        project_id: project_id.to_string(),
        origin_node_id: origin.to_string(),
        origin_sequence: 1,
        logical_time: HybridLogicalTime {
            wall_millis,
            counter: 0,
            node_id: origin.to_string(),
        },
        aggregate_type: "cameraIdentity".to_string(),
        aggregate_id: source.to_string(),
        operation_type: CAMERA_IDENTITY_MERGED.to_string(),
        payload_version: 1,
        base_revision: None,
        payload: serde_json::json!({
            "sourceCamera": source,
            "destinationCamera": destination,
            "assetIds": [],
            "captureSequenceIds": [],
            "assignments": []
        }),
    }
}

fn projected_cameras(
    catalog: &Catalog,
    project_id: &str,
) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
    let connection = catalog.connection().unwrap();
    let assets = connection
        .prepare("SELECT camera FROM assets WHERE project_id=?1 ORDER BY id")
        .unwrap()
        .query_map([project_id], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let sequences = connection
        .prepare("SELECT camera_key FROM capture_sequences WHERE project_id=?1 ORDER BY id")
        .unwrap()
        .query_map([project_id], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let mappings = connection
        .prepare("SELECT camera FROM camera_identity_mappings WHERE project_id=?1 ORDER BY id")
        .unwrap()
        .query_map([project_id], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let clocks = connection
        .prepare("SELECT camera FROM clock_events WHERE project_id=?1 ORDER BY id")
        .unwrap()
        .query_map([project_id], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    (assets, sequences, mappings, clocks)
}

#[test]
fn chained_merge_facts_converge_in_both_delivery_orders() {
    let (_left_dir, left, left_project) = convergence_fixture("chain-left");
    let (_right_dir, right, right_project) = convergence_fixture("chain-right");
    let left_ab = merge_fact(&left_project, "op_ab", "node_ab", 10, "A", "B");
    let left_bc = merge_fact(&left_project, "op_bc", "node_bc", 20, "B", "C");
    left.apply_operation(&left_ab).unwrap();
    left.apply_operation(&left_bc).unwrap();
    let right_ab = merge_fact(&right_project, "op_ab", "node_ab", 10, "A", "B");
    let right_bc = merge_fact(&right_project, "op_bc", "node_bc", 20, "B", "C");
    right.apply_operation(&right_bc).unwrap();
    right.apply_operation(&right_ab).unwrap();

    let left_projection = projected_cameras(&left, &left_project);
    let right_projection = projected_cameras(&right, &right_project);
    assert!(left_projection.0.iter().all(|camera| camera == "C"));
    assert!(left_projection.1.iter().all(|camera| camera == "C"));
    assert!(left_projection.2.iter().all(|camera| camera == "C"));
    assert!(left_projection.3.iter().all(|camera| camera == "C"));
    assert_eq!(left_projection, right_projection);
}

#[test]
fn opposing_merge_facts_converge_without_an_apply_order_cycle() {
    let (_left_dir, left, left_project) = convergence_fixture("cycle-left");
    let (_right_dir, right, right_project) = convergence_fixture("cycle-right");
    let left_ab = merge_fact(&left_project, "op_ab", "node_ab", 10, "A", "B");
    let left_ba = merge_fact(&left_project, "op_ba", "node_ba", 20, "B", "A");
    left.apply_operation(&left_ab).unwrap();
    left.apply_operation(&left_ba).unwrap();
    let right_ab = merge_fact(&right_project, "op_ab", "node_ab", 10, "A", "B");
    let right_ba = merge_fact(&right_project, "op_ba", "node_ba", 20, "B", "A");
    right.apply_operation(&right_ba).unwrap();
    right.apply_operation(&right_ab).unwrap();

    let left_projection = projected_cameras(&left, &left_project);
    let right_projection = projected_cameras(&right, &right_project);
    assert!(left_projection.0.iter().take(2).all(|camera| camera == "A"));
    assert!(left_projection.1.iter().take(2).all(|camera| camera == "A"));
    assert!(left_projection.2.iter().take(2).all(|camera| camera == "A"));
    assert!(left_projection.3.iter().take(2).all(|camera| camera == "A"));
    assert_eq!(left_projection, right_projection);
}
