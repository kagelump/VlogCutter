use super::*;
use rcdvlog_core::{ClockEpoch, MediaVerificationState, ProjectAssetAddedPayload};
use serde_json::json;
use tempfile::TempDir;

#[test]
fn process_guard_serializes_sync_and_wake_requests_are_bounded() {
    let root = TempDir::new().unwrap();
    let catalog_path = root.path().join("catalog.sqlite3");
    fs::write(&catalog_path, b"").unwrap();
    let guard = SyncProcessGuard::try_acquire(&catalog_path)
        .unwrap()
        .expect("first process acquires guard");
    assert!(
        SyncProcessGuard::try_acquire(&catalog_path)
            .unwrap()
            .is_none()
    );

    signal_running_sync_manager(&catalog_path, "project_fixture").unwrap();
    signal_running_sync_manager(&catalog_path, "project_fixture").unwrap();
    assert_eq!(
        drain_sync_manager_requests(&catalog_path).unwrap(),
        vec!["project_fixture"]
    );
    assert!(
        drain_sync_manager_requests(&catalog_path)
            .unwrap()
            .is_empty()
    );
    assert!(signal_running_sync_manager(&catalog_path, "../escape").is_err());

    drop(guard);
    assert!(
        SyncProcessGuard::try_acquire(&catalog_path)
            .unwrap()
            .is_some()
    );
}

fn new_catalog(root: &TempDir, filename: &str, node: &str) -> Catalog {
    let catalog = Catalog::open(root.path().join(filename)).unwrap();
    catalog.set_local_node_id(node).unwrap();
    catalog
}

fn asset(id: &str, filename: &str) -> ProjectAssetAddedPayload {
    ProjectAssetAddedPayload {
        id: id.to_string(),
        fingerprint: format!("fingerprint-{id}"),
        filename: filename.to_string(),
        size_bytes: 123,
        duration_seconds: 2.5,
        frame_rate: 24.0,
        width: 1920,
        height: 1080,
        codec: "h264".to_string(),
        camera: "camera".to_string(),
        captured_at_raw: None,
        timestamp_candidates: None,
        best_time: None,
        metadata_rotation_degrees: 0,
        default_rotation_degrees: 0,
        created_at: "2026-07-23T00:00:00Z".to_string(),
    }
}

fn timed_asset(
    id: &str,
    filename: &str,
    camera: &str,
    captured_at: &str,
) -> ProjectAssetAddedPayload {
    ProjectAssetAddedPayload {
        camera: camera.to_string(),
        captured_at_raw: Some(captured_at.to_string()),
        timestamp_candidates: None,
        best_time: Some(captured_at.to_string()),
        ..asset(id, filename)
    }
}

#[test]
fn selected_project_baseline_is_path_free_and_imports_equivalently() {
    let root = TempDir::new().unwrap();
    let source = new_catalog(&root, "source.sqlite3", "node_mac");
    let selected = source.create_project("Selected").unwrap();
    let other = source.create_project("Other").unwrap();
    source
        .add_portable_project_asset(&selected.id, asset("asset_selected", "selected.mov"))
        .unwrap();
    source
        .add_portable_project_asset(&other.id, asset("asset_other", "other.mov"))
        .unwrap();

    let envelope = BaselineEnvelope::export(&source, &selected.id, vec![], vec![]).unwrap();
    envelope.validate().unwrap();
    assert_eq!(envelope.payload.project.project_id, selected.id);
    assert_eq!(envelope.payload.project.assets.len(), 1);
    assert_eq!(envelope.payload.project.assets[0].id, "asset_selected");
    assert!(
        !serde_json::to_string(&envelope)
            .unwrap()
            .contains(root.path().to_str().unwrap())
    );

    let imported = Catalog::import_project_baseline(
        root.path().join("imported.sqlite3"),
        &envelope.payload.project,
        "node_server",
        BASELINE_VERSION,
        &envelope.digest,
    )
    .unwrap();
    let projects = imported.list_projects().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].id, selected.id);
    assert_eq!(imported.list_assets(&selected.id).unwrap().len(), 1);
    assert_eq!(
        imported.list_operations(&selected.id).unwrap(),
        envelope.payload.project.operations
    );
}

#[test]
fn reconciliation_converges_and_retries_are_idempotent() {
    let root = TempDir::new().unwrap();
    let mac = new_catalog(&root, "mac.sqlite3", "node_mac");
    let project = mac.create_project("Trip").unwrap();
    let baseline = BaselineEnvelope::export(&mac, &project.id, vec![], vec![]).unwrap();
    let server = Catalog::import_project_baseline(
        root.path().join("server.sqlite3"),
        &baseline.payload.project,
        "node_server",
        BASELINE_VERSION,
        &baseline.digest,
    )
    .unwrap();

    mac.add_portable_project_asset(&project.id, asset("asset_mac", "mac.mov"))
        .unwrap();
    server
        .add_portable_project_asset(&project.id, asset("asset_server", "server.mov"))
        .unwrap();
    let mac_operations = mac.list_operations(&project.id).unwrap();
    let request = ReconcileRequest {
        protocol_version: ProtocolRange::default(),
        project_schema_version: PROJECT_CATALOG_SCHEMA_VERSION,
        project_id: project.id.clone(),
        server_node_id: "node_server".to_string(),
        operation_cursors: BTreeMap::from([("node_mac".to_string(), 1)]),
        operations: mac_operations.clone(),
        observations: vec![],
        representation_manifests: vec![],
        maximum_response_operations: 10,
    };
    let response = reconcile(&server, &project.id, "node_server", &request).unwrap();
    assert_eq!(response.committed_cursors["node_mac"], 1);
    assert_eq!(response.operations.len(), 1);
    assert_eq!(response.operations[0].origin_node_id, "node_server");
    mac.apply_operation_batch("node_server", &response.operations)
        .unwrap();

    let retry = reconcile(&server, &project.id, "node_server", &request).unwrap();
    assert_eq!(retry.operations, response.operations);
    server
        .link_project_asset_media(
            "asset_mac",
            "media_provisional",
            MediaVerificationState::Provisional,
        )
        .unwrap();
    let link_request = ReconcileRequest {
        operation_cursors: BTreeMap::from([
            ("node_mac".to_string(), 1),
            ("node_server".to_string(), 1),
        ]),
        operations: vec![],
        ..request.clone()
    };
    let link_response = reconcile(&server, &project.id, "node_server", &link_request).unwrap();
    assert_eq!(link_response.operations.len(), 1);
    mac.apply_operation_batch("node_server", &link_response.operations)
        .unwrap();
    for catalog in [&mac, &server] {
        let linked = catalog.get_asset("asset_mac").unwrap();
        assert_eq!(linked.id, "asset_mac");
        assert_eq!(linked.media_object_id.as_deref(), Some("media_provisional"));
        assert_eq!(
            linked.media_verification_state,
            MediaVerificationState::Provisional
        );
    }
    assert_eq!(server.list_assets(&project.id).unwrap().len(), 2);
    assert_eq!(mac.list_assets(&project.id).unwrap().len(), 2);
    assert_eq!(
        mac.list_operations(&project.id).unwrap().len(),
        server.list_operations(&project.id).unwrap().len()
    );
}

#[test]
fn remote_chronology_reconciliation_refreshes_projected_times_and_queue_order() {
    let root = TempDir::new().unwrap();
    let mac = new_catalog(&root, "chronology-mac.sqlite3", "node_mac");
    let project = mac.create_project("Chronology sync").unwrap();
    mac.add_portable_project_asset(
        &project.id,
        timed_asset("asset_sony", "A0001.MP4", "Sony", "2026-01-01T10:00:00Z"),
    )
    .unwrap();
    mac.add_portable_project_asset(
        &project.id,
        timed_asset("asset_dji", "D0001.MP4", "DJI", "2026-01-01T11:00:00Z"),
    )
    .unwrap();
    let baseline = BaselineEnvelope::export(&mac, &project.id, vec![], vec![]).unwrap();
    let server = Catalog::import_project_baseline(
        root.path().join("chronology-server.sqlite3"),
        &baseline.payload.project,
        "node_server",
        BASELINE_VERSION,
        &baseline.digest,
    )
    .unwrap();
    let dji_sequence = server
        .capture_sequences(&project.id)
        .unwrap()
        .into_iter()
        .find(|sequence| sequence.camera_key == "DJI")
        .unwrap();
    server
        .set_clock_epoch(
            &project.id,
            &ClockEpoch {
                id: "epoch_remote_dji".to_string(),
                capture_sequence_id: dji_sequence.id,
                start_counter: 0,
                end_counter: None,
                offset_seconds: -7_200,
                confidence: 1.0,
                evidence: vec!["remote_user".to_string()],
                label: "DJI was two hours fast".to_string(),
            },
        )
        .unwrap();
    let remote_operations: Vec<_> = server
        .list_operations(&project.id)
        .unwrap()
        .into_iter()
        .filter(|operation| operation.origin_node_id == "node_server")
        .collect();
    assert_eq!(remote_operations.len(), 1);
    assert_eq!(
        mac.list_assets(&project.id)
            .unwrap()
            .into_iter()
            .map(|asset| asset.id)
            .collect::<Vec<_>>(),
        ["asset_sony", "asset_dji"]
    );

    apply_reconcile_operations(&mac, &remote_operations).unwrap();

    let ordered = mac.list_assets(&project.id).unwrap();
    assert_eq!(
        ordered
            .iter()
            .map(|asset| asset.id.as_str())
            .collect::<Vec<_>>(),
        ["asset_dji", "asset_sony"],
        "queue reads use the corrected projected time immediately after sync"
    );
    assert_eq!(
        ordered[0].best_time.as_deref(),
        Some("2026-01-01T09:00:00.000Z")
    );
    assert_eq!(ordered[0].offset_seconds, -7_200);
}

#[test]
fn sequence_gaps_do_not_advance_operation_or_fact_cursors() {
    let root = TempDir::new().unwrap();
    let mac = new_catalog(&root, "gap-mac.sqlite3", "node_mac");
    let project = mac.create_project("Trip").unwrap();
    let baseline = BaselineEnvelope::export(&mac, &project.id, vec![], vec![]).unwrap();
    let server = Catalog::import_project_baseline(
        root.path().join("gap-server.sqlite3"),
        &baseline.payload.project,
        "node_server",
        BASELINE_VERSION,
        &baseline.digest,
    )
    .unwrap();
    mac.add_portable_project_asset(&project.id, asset("asset_one", "one.mov"))
        .unwrap();
    mac.add_portable_project_asset(&project.id, asset("asset_two", "two.mov"))
        .unwrap();
    let operation_two = mac.list_operations(&project.id).unwrap().remove(1);
    let request = ReconcileRequest {
        protocol_version: ProtocolRange::default(),
        project_schema_version: PROJECT_CATALOG_SCHEMA_VERSION,
        project_id: project.id.clone(),
        server_node_id: "node_server".to_string(),
        operation_cursors: BTreeMap::new(),
        operations: vec![operation_two],
        observations: vec![],
        representation_manifests: vec![],
        maximum_response_operations: 10,
    };
    let error = reconcile(&server, &project.id, "node_server", &request).unwrap_err();
    assert_eq!(
        error.downcast_ref::<SyncError>().unwrap().code(),
        "sync_sequence_gap"
    );
    assert!(server.operation_cursors().unwrap().is_empty());

    let fact_error = persist_baseline_facts(
        &server,
        &[ObservationFact {
            origin_node_id: "node_mac".to_string(),
            origin_sequence: 2,
            scope_key: "asset_one/original".to_string(),
            payload: json!({"available": true}),
        }],
        &[],
    )
    .unwrap_err();
    assert_eq!(
        fact_error.downcast_ref::<SyncError>().unwrap().code(),
        "sync_sequence_gap"
    );
    let connection = Connection::open(server.database_path()).unwrap();
    assert_eq!(
        fact_cursors(&connection, "sync_observation_facts")
            .unwrap()
            .get("node_mac"),
        None
    );
}

#[test]
fn observations_are_origin_scoped_and_manifests_exclude_job_progress() {
    let root = TempDir::new().unwrap();
    let catalog = new_catalog(&root, "facts.sqlite3", "node_server");
    let project = catalog.create_project("Trip").unwrap();
    let observations = vec![
        ObservationFact {
            origin_node_id: "node_mac".to_string(),
            origin_sequence: 1,
            scope_key: "asset/original".to_string(),
            payload: json!({"available": false}),
        },
        ObservationFact {
            origin_node_id: "node_mac".to_string(),
            origin_sequence: 2,
            scope_key: "asset/original".to_string(),
            payload: json!({"available": true}),
        },
        ObservationFact {
            origin_node_id: "node_server".to_string(),
            origin_sequence: 1,
            scope_key: "asset/original".to_string(),
            payload: json!({"available": false}),
        },
    ];
    let (cursors, _) = persist_baseline_facts(&catalog, &observations, &[]).unwrap();
    assert_eq!(cursors["node_mac"], 2);
    assert_eq!(cursors["node_server"], 1);
    let connection = Connection::open(catalog.database_path()).unwrap();
    let latest_mac: String = connection
        .query_row(
            "SELECT payload_json FROM sync_observation_facts WHERE origin_node_id='node_mac' AND scope_key='asset/original' ORDER BY origin_sequence DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&latest_mac).unwrap(),
        json!({"available": true})
    );

    let error = persist_baseline_facts(
        &catalog,
        &[],
        &[RepresentationManifestFact {
            manifest_id: "manifest_1".to_string(),
            origin_node_id: "node_server".to_string(),
            origin_sequence: 1,
            payload: json!({"representationId": "rep_1", "jobProgress": 0.5}),
        }],
    )
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<SyncError>().unwrap().code(),
        "sync_sequence_gap"
    );
    assert_eq!(catalog.list_projects().unwrap()[0].id, project.id);
}

#[test]
fn every_origin_preserving_delivery_interleaving_converges() {
    let root = TempDir::new().unwrap();
    let mac = new_catalog(&root, "permutation-mac.sqlite3", "node_mac");
    let project = mac.create_project("Trip").unwrap();
    mac.add_portable_project_asset(&project.id, asset("asset_shared", "shared.mov"))
        .unwrap();
    let baseline = BaselineEnvelope::export(&mac, &project.id, vec![], vec![]).unwrap();
    let server = Catalog::import_project_baseline(
        root.path().join("permutation-server.sqlite3"),
        &baseline.payload.project,
        "node_server",
        BASELINE_VERSION,
        &baseline.digest,
    )
    .unwrap();

    mac.create_note_revision("asset_shared", "mac note")
        .unwrap();
    mac.add_label("asset_shared", "Mac").unwrap();
    mac.set_removed("asset_shared", true).unwrap();
    server.add_label("asset_shared", "Server").unwrap();
    server.set_rotation("asset_shared", 90).unwrap();
    let mac_delta = mac
        .list_operations(&project.id)
        .unwrap()
        .into_iter()
        .filter(|operation| operation.origin_node_id == "node_mac")
        .skip(1)
        .collect::<Vec<_>>();
    let server_delta = server
        .list_operations(&project.id)
        .unwrap()
        .into_iter()
        .filter(|operation| operation.origin_node_id == "node_server")
        .collect::<Vec<_>>();

    fn interleave(
        left: &[ProjectOperation],
        right: &[ProjectOperation],
        prefix: &mut Vec<ProjectOperation>,
        output: &mut Vec<Vec<ProjectOperation>>,
    ) {
        if left.is_empty() && right.is_empty() {
            output.push(prefix.clone());
            return;
        }
        if let Some((first, rest)) = left.split_first() {
            prefix.push(first.clone());
            interleave(rest, right, prefix, output);
            prefix.pop();
        }
        if let Some((first, rest)) = right.split_first() {
            prefix.push(first.clone());
            interleave(left, rest, prefix, output);
            prefix.pop();
        }
    }

    let mut deliveries = Vec::new();
    interleave(&mac_delta, &server_delta, &mut Vec::new(), &mut deliveries);
    assert_eq!(deliveries.len(), 10);
    let mut expected = None;
    for (index, delivery) in deliveries.into_iter().enumerate() {
        let replica = Catalog::import_project_baseline(
            root.path().join(format!("permutation-{index}.sqlite3")),
            &baseline.payload.project,
            &format!("node_replica_{index}"),
            BASELINE_VERSION,
            &baseline.digest,
        )
        .unwrap();
        for operation in delivery {
            replica
                .apply_operation_batch(&operation.origin_node_id, std::slice::from_ref(&operation))
                .unwrap();
        }
        let asset = replica.get_asset("asset_shared").unwrap();
        let projection = (
            asset.notes,
            asset.labels,
            asset.removed,
            asset.rotation_degrees,
        );
        if let Some(expected) = &expected {
            assert_eq!(&projection, expected);
        } else {
            expected = Some(projection);
        }
    }
}

fn ack_response(cursors: &[(&str, u64)]) -> ReconcileResponse {
    ReconcileResponse {
        protocol_major: 1,
        protocol_minor: 0,
        project_id: "project_fixture".to_string(),
        committed_cursors: cursors.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        operations: Vec::new(),
        observation_cursors: BTreeMap::new(),
        representation_cursors: BTreeMap::new(),
        conflicts: Vec::new(),
        has_more_operations: false,
        sequence_recovery: Vec::new(),
        node_targeted_jobs: Vec::new(),
    }
}

#[test]
fn catalog_swapped_mid_reconcile_is_not_acknowledged_into_the_new_catalog() {
    let root = TempDir::new().unwrap();
    // Both catalogs share the desktop node id, as every desktop catalog does.
    let a = new_catalog(&root, "a.sqlite3", "node_mac");
    let b = new_catalog(&root, "b.sqlite3", "node_mac");
    let open = std::cell::RefCell::new(a.database_path().to_path_buf());

    // Fake transport: the round trip is where the user opens catalog B.
    let transport = || {
        *open.borrow_mut() = b.database_path().to_path_buf();
        ack_response(&[("node_mac", 7)])
    };
    let response = transport();
    let current = open.borrow().clone();
    let outcome = apply_pinned_reconcile_response(&a, &current, "node_server", &response).unwrap();

    assert_eq!(outcome, PinnedApplyOutcome::CatalogChanged);
    assert!(
        b.peer_operation_acknowledgements("node_server")
            .unwrap()
            .is_empty()
    );
    assert!(
        a.peer_operation_acknowledgements("node_server")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn unswapped_reconcile_records_acknowledgements_on_the_pinned_catalog() {
    let root = TempDir::new().unwrap();
    let a = new_catalog(&root, "a.sqlite3", "node_mac");
    let b = new_catalog(&root, "b.sqlite3", "node_mac");
    let outcome = apply_pinned_reconcile_response(
        &a,
        a.database_path(),
        "node_server",
        &ack_response(&[("node_mac", 3)]),
    )
    .unwrap();
    assert_eq!(outcome, PinnedApplyOutcome::Applied);
    assert_eq!(
        a.peer_operation_acknowledgements("node_server").unwrap()["node_mac"],
        3
    );
    assert!(
        b.peer_operation_acknowledgements("node_server")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn guard_follows_the_catalog_and_never_syncs_unguarded() {
    let root = TempDir::new().unwrap();
    fs::create_dir_all(root.path().join("a")).unwrap();
    fs::create_dir_all(root.path().join("b")).unwrap();
    let a = new_catalog(&root, "a/catalog.sqlite3", "node_mac");
    let b = new_catalog(&root, "b/catalog.sqlite3", "node_mac");
    let (a_path, b_path) = (
        a.database_path().to_path_buf(),
        b.database_path().to_path_buf(),
    );
    let mut tracker = SyncGuardTracker::new();

    assert_eq!(tracker.ensure(&a_path).unwrap(), GuardState::Held);
    assert!(SyncProcessGuard::try_acquire(&a_path).unwrap().is_none());

    // Swap to B: A's guard is released so another actor can take it.
    assert_eq!(tracker.ensure(&b_path).unwrap(), GuardState::Held);
    assert_eq!(tracker.guarded_path(), Some(b_path.as_path()));
    let other_a = SyncProcessGuard::try_acquire(&a_path).unwrap();
    assert!(
        other_a.is_some(),
        "A must be free once the manager moved to B"
    );
    assert!(SyncProcessGuard::try_acquire(&b_path).unwrap().is_none());

    // Swap back to A while another actor holds it: not synced, nothing held.
    assert_eq!(tracker.ensure(&a_path).unwrap(), GuardState::HeldElsewhere);
    assert_eq!(tracker.guarded_path(), None);
    assert!(SyncProcessGuard::try_acquire(&b_path).unwrap().is_some());

    // Once the other actor leaves, A is guarded again.
    drop(other_a);
    assert_eq!(tracker.ensure(&a_path).unwrap(), GuardState::Held);
}
