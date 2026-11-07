use super::*;
use crate::ServerConfig;
use rcdvlog_core::ProjectAssetAddedPayload;
use rcdvlog_sync::{BaselineEnvelope, RepresentationManifestFact};
use serde_json::json;
use std::collections::BTreeMap;
use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    manager: ProjectReplicaManager,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let data = root.path().join("state");
        let derived = root.path().join("derived");
        let backup = root.path().join("backup");
        fs::create_dir_all(&derived).unwrap();
        fs::create_dir_all(&backup).unwrap();
        let config = ServerConfig::from_values(BTreeMap::from([
            ("RCDVLOG_DATA_DIR".to_string(), data.display().to_string()),
            (
                "RCDVLOG_DERIVED_DIR".to_string(),
                derived.display().to_string(),
            ),
            (
                "RCDVLOG_BACKUP_DIR".to_string(),
                backup.display().to_string(),
            ),
            ("RCDVLOG_FFMPEG".to_string(), "/usr/bin/true".to_string()),
            ("RCDVLOG_FFPROBE".to_string(), "/usr/bin/true".to_string()),
            ("RCDVLOG_EXIFTOOL".to_string(), "/usr/bin/true".to_string()),
        ]))
        .unwrap();
        fs::create_dir_all(&config.data_dir).unwrap();
        let control =
            ControlCatalog::open_or_create(&config.data_dir, &config.diagnostics()).unwrap();
        let manager = control.project_replicas();
        Self {
            _root: root,
            manager,
        }
    }
}

fn baseline(root: &TempDir) -> BaselineEnvelope {
    let catalog = Catalog::open(root.path().join("desktop.sqlite3")).unwrap();
    catalog.set_local_node_id("node_mac").unwrap();
    let project = catalog.create_project("Trip").unwrap();
    catalog
        .add_portable_project_asset(
            &project.id,
            ProjectAssetAddedPayload {
                id: "asset_desktop".to_string(),
                fingerprint: "fingerprint".to_string(),
                filename: "clip.mov".to_string(),
                size_bytes: 5,
                duration_seconds: 1.0,
                frame_rate: 30.0,
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
            },
        )
        .unwrap();
    BaselineEnvelope::export(&catalog, &project.id, vec![], vec![]).unwrap()
}

#[test]
fn preview_is_read_only_and_import_is_atomic_and_idempotent() {
    let fixture = Fixture::new();
    let source_root = TempDir::new().unwrap();
    let envelope = baseline(&source_root);
    open_writable_connection(fixture.manager.control.database_path())
        .unwrap()
        .execute(
            "INSERT INTO media_objects(media_object_id, size_bytes, quick_fingerprint, verification_state, created_at, updated_at) VALUES('media_preview', 5, 'fingerprint', 'provisional', ?1, ?1)",
            [now()],
        )
        .unwrap();
    let before: i64 = open_read_only_connection(fixture.manager.control.database_path())
        .unwrap()
        .query_row("SELECT COUNT(*) FROM project_registry", [], |row| {
            row.get(0)
        })
        .unwrap();
    let preview = fixture.manager.preview(&envelope).unwrap();
    assert_eq!(preview.asset_count, 1);
    assert_eq!(preview.proposed_library_matches, 1);
    let after: i64 = open_read_only_connection(fixture.manager.control.database_path())
        .unwrap()
        .query_row("SELECT COUNT(*) FROM project_registry", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(before, after);

    let imported = fixture.manager.import_baseline(&envelope).unwrap();
    assert!(!imported.reused_existing);
    assert_eq!(imported.baseline_digest, envelope.digest);
    assert_eq!(
        fixture
            .manager
            .open_catalog(&envelope.payload.project.project_id)
            .unwrap()
            .list_assets(&envelope.payload.project.project_id)
            .unwrap()
            .len(),
        1
    );
    let retried = fixture.manager.import_baseline(&envelope).unwrap();
    assert!(retried.reused_existing);
    assert_eq!(
        retried.replica_relative_path,
        imported.replica_relative_path
    );
}

#[test]
fn different_baseline_never_overwrites_registered_replica() {
    let fixture = Fixture::new();
    let source_root = TempDir::new().unwrap();
    let envelope = baseline(&source_root);
    let registered = fixture.manager.import_baseline(&envelope).unwrap();
    let replica_path = fixture
        .manager
        .resolve_relative_path(&registered.replica_relative_path)
        .unwrap();
    let bytes_before = fs::read(&replica_path).unwrap();

    let mut changed = envelope.clone();
    changed.payload.project.name = "Different".to_string();
    changed.digest = rcdvlog_sync::canonical_digest(&changed.payload).unwrap();
    assert!(matches!(
        fixture.manager.import_baseline(&changed),
        Err(ProjectReplicaError::DifferentBaseline { .. })
    ));
    assert_eq!(fs::read(replica_path).unwrap(), bytes_before);
}

#[test]
fn failed_temporary_import_leaves_no_registry_or_partial_replica() {
    let fixture = Fixture::new();
    let source_root = TempDir::new().unwrap();
    let mut envelope = baseline(&source_root);
    envelope.payload.representation_manifests = vec![
        RepresentationManifestFact {
            manifest_id: "same_manifest".to_string(),
            origin_node_id: "node_mac".to_string(),
            origin_sequence: 1,
            payload: json!({"representationId": "rep_1"}),
        },
        RepresentationManifestFact {
            manifest_id: "same_manifest".to_string(),
            origin_node_id: "node_server".to_string(),
            origin_sequence: 1,
            payload: json!({"representationId": "rep_2"}),
        },
    ];
    envelope.digest = rcdvlog_sync::canonical_digest(&envelope.payload).unwrap();
    assert!(fixture.manager.import_baseline(&envelope).is_err());
    assert!(matches!(
        fixture.manager.get(&envelope.payload.project.project_id),
        Err(ProjectReplicaError::NotFound { .. })
    ));
    let target = fixture
        .manager
        .resolve_relative_path(&replica_relative_path(&envelope.payload.project.project_id))
        .unwrap();
    assert!(!target.exists());
    let temp_entries = fs::read_dir(fixture.manager.data_dir.join("tmp"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(temp_entries.is_empty());
}

#[test]
fn membership_projection_repairs_after_project_commit_and_is_idempotent() {
    let fixture = Fixture::new();
    let source_root = TempDir::new().unwrap();
    let envelope = baseline(&source_root);
    fixture.manager.import_baseline(&envelope).unwrap();
    let connection = open_writable_connection(fixture.manager.control.database_path()).unwrap();
    connection.execute(
        "INSERT INTO media_objects(media_object_id, size_bytes, verification_state, created_at, updated_at) VALUES('media_fixture', 5, 'provisional', ?1, ?1)",
        [now()],
    ).unwrap();

    // Simulate a crash boundary: the authoritative project commit succeeds,
    // but the control-catalog projection has not run yet.
    let replica = fixture
        .manager
        .open_catalog(&envelope.payload.project.project_id)
        .unwrap();
    replica
        .link_project_asset_media(
            "asset_desktop",
            "media_fixture",
            MediaVerificationState::Provisional,
        )
        .unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM project_media_memberships",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        fixture
            .manager
            .repair_membership_projections(&envelope.payload.project.project_id)
            .unwrap(),
        1
    );
    fixture
        .manager
        .repair_membership_projections(&envelope.payload.project.project_id)
        .unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM project_media_memberships",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

fn seed_global_media(fixture: &Fixture, id: &str, state: GlobalMediaVerificationState) {
    fixture
        .manager
        .control
        .storage_library()
        .create_media_object(crate::NewMediaObject {
            id: MediaObjectId::parse(id).unwrap(),
            size_bytes: Some(42),
            quick_fingerprint: Some(blake3::hash(id.as_bytes()).to_hex().to_string()),
            full_content_hash: (state == GlobalMediaVerificationState::Verified)
                .then(|| "a".repeat(64)),
            verification_state: state,
        })
        .unwrap();
}

#[test]
fn add_to_project_deduplicates_from_replica_and_repairs_projection() {
    let fixture = Fixture::new();
    let source_root = TempDir::new().unwrap();
    let envelope = baseline(&source_root);
    let project_id = envelope.payload.project.project_id.clone();
    fixture.manager.import_baseline(&envelope).unwrap();
    seed_global_media(
        &fixture,
        "media_add_verified",
        GlobalMediaVerificationState::Verified,
    );
    let request = AddToProjectRequest {
        media_object_ids: vec![MediaObjectId::parse("media_add_verified").unwrap()],
        acknowledge_unverified: false,
        idempotency_key: "request-add-1".to_string(),
    };

    let preview = fixture
        .manager
        .preview_add_to_project(&project_id, &request)
        .unwrap();
    assert_eq!(preview.ready_count, 1);
    let first = fixture
        .manager
        .add_media_to_project(&project_id, &request)
        .unwrap();
    assert_eq!(first.added_count, 1);
    assert_eq!(first.blocked_count, 0);

    let retried = fixture
        .manager
        .add_media_to_project(&project_id, &request)
        .unwrap();
    assert_eq!(retried.added_count, 0);
    assert_eq!(retried.already_member_count, 1);
    let catalog = fixture.manager.open_catalog(&project_id).unwrap();
    assert_eq!(catalog.count_project_assets(&project_id).unwrap(), 2);
    let projected: i64 = open_read_only_connection(fixture.manager.control.database_path())
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM project_media_memberships WHERE media_object_id='media_add_verified'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(projected, 1);
}

#[test]
fn add_to_project_requires_acknowledgement_and_blocks_mismatch() {
    let fixture = Fixture::new();
    let source_root = TempDir::new().unwrap();
    let envelope = baseline(&source_root);
    let project_id = envelope.payload.project.project_id.clone();
    fixture.manager.import_baseline(&envelope).unwrap();
    seed_global_media(
        &fixture,
        "media_add_provisional",
        GlobalMediaVerificationState::Provisional,
    );
    seed_global_media(
        &fixture,
        "media_add_mismatch",
        GlobalMediaVerificationState::Mismatch,
    );
    let mut request = AddToProjectRequest {
        media_object_ids: vec![
            MediaObjectId::parse("media_add_provisional").unwrap(),
            MediaObjectId::parse("media_add_mismatch").unwrap(),
        ],
        acknowledge_unverified: false,
        idempotency_key: "request-add-unsafe".to_string(),
    };
    let blocked = fixture
        .manager
        .add_media_to_project(&project_id, &request)
        .unwrap();
    assert_eq!(blocked.added_count, 0);
    assert_eq!(blocked.blocked_count, 2);

    request.acknowledge_unverified = true;
    let acknowledged = fixture
        .manager
        .add_media_to_project(&project_id, &request)
        .unwrap();
    assert_eq!(acknowledged.added_count, 1);
    assert_eq!(acknowledged.blocked_count, 1);
    assert!(acknowledged.items.iter().any(|item| {
        item.media_object_id.as_str() == "media_add_mismatch"
            && item.disposition == AddToProjectDisposition::IdentityMismatch
    }));
}

#[test]
fn review_commands_return_canonical_state_and_stale_notes_still_commit() {
    let fixture = Fixture::new();
    let source_root = TempDir::new().unwrap();
    let envelope = baseline(&source_root);
    let project_id = envelope.payload.project.project_id.clone();
    fixture.manager.import_baseline(&envelope).unwrap();

    let before = fixture
        .manager
        .get_review_asset(&project_id, "asset_desktop")
        .unwrap();
    let note = fixture
        .manager
        .create_review_note(
            &project_id,
            "asset_desktop",
            "Web revision",
            before.materialized_revision.saturating_sub(1),
        )
        .unwrap();
    assert!(note.stale_base_revision);
    assert_eq!(
        note.asset.asset.active_note.as_deref(),
        Some("Web revision")
    );
    assert!(note.asset.note_revision_count >= 2);

    let label = fixture
        .manager
        .add_review_label(
            &project_id,
            "asset_desktop",
            "arrival",
            note.asset.materialized_revision,
        )
        .unwrap();
    assert!(!label.stale_base_revision);
    assert_eq!(label.asset.asset.labels, vec!["arrival"]);
    let rotated = fixture
        .manager
        .set_review_rotation(
            &project_id,
            "asset_desktop",
            90,
            label.asset.materialized_revision,
        )
        .unwrap();
    assert_eq!(rotated.asset.asset.rotation_degrees, 90);
    let removed = fixture
        .manager
        .set_review_removed(
            &project_id,
            "asset_desktop",
            true,
            rotated.asset.materialized_revision,
        )
        .unwrap();
    assert!(removed.asset.asset.removed);
    let restored = fixture
        .manager
        .remove_review_label(
            &project_id,
            "asset_desktop",
            "arrival",
            removed.asset.materialized_revision,
        )
        .unwrap();
    assert!(restored.asset.asset.labels.is_empty());
}

/// A registered replica with `count` portable assets named
/// `asset_000.mov` .. `asset_{count-1:03}.mov`, imported through the same
/// `import_baseline` path a real desktop sync would use.
struct AssetPageFixture {
    _server_root: TempDir,
    _source_root: TempDir,
    manager: ProjectReplicaManager,
    project_id: String,
}

fn portable_asset_payload(index: usize) -> ProjectAssetAddedPayload {
    ProjectAssetAddedPayload {
        id: format!("asset_{index:03}"),
        fingerprint: format!("fingerprint_{index:03}"),
        filename: format!("asset_{index:03}.mov"),
        size_bytes: 5,
        duration_seconds: 1.0,
        frame_rate: 30.0,
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

fn replica_fixture_with_assets(count: usize) -> AssetPageFixture {
    let fixture = Fixture::new();
    let source_root = TempDir::new().unwrap();
    let catalog = Catalog::open(source_root.path().join("desktop.sqlite3")).unwrap();
    catalog.set_local_node_id("node_mac").unwrap();
    let project = catalog.create_project("Trip").unwrap();
    for index in 0..count {
        catalog
            .add_portable_project_asset(&project.id, portable_asset_payload(index))
            .unwrap();
    }
    let envelope = BaselineEnvelope::export(&catalog, &project.id, vec![], vec![]).unwrap();
    let registered = fixture.manager.import_baseline(&envelope).unwrap();
    AssetPageFixture {
        _server_root: fixture._root,
        _source_root: source_root,
        manager: fixture.manager,
        project_id: registered.project_id,
    }
}

#[test]
fn project_assets_page_deterministically_and_exclude_absolute_paths() {
    let fixture = replica_fixture_with_assets(25);
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = fixture
            .manager
            .list_project_assets(&fixture.project_id, 10, cursor.as_deref(), None)
            .unwrap();
        for item in &page.items {
            let encoded = serde_json::to_string(item).unwrap();
            assert!(!encoded.contains('/'), "asset DTO leaked a path: {encoded}");
            seen.push(item.project_asset_id.clone());
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(seen.len(), 25);
    let mut sorted = seen.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 25, "pages repeated an asset");
}

#[test]
fn two_thousand_asset_baseline_remains_bounded_and_pages_exhaustively() {
    let fixture = replica_fixture_with_assets(2_000);
    let mut cursor = None;
    let mut seen = 0;
    loop {
        let page = fixture
            .manager
            .list_project_assets(&fixture.project_id, 200, cursor.as_deref(), None)
            .unwrap();
        assert!(page.items.len() <= 200);
        seen += page.items.len();
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(seen, 2_000);
    assert_eq!(
        fixture.manager.list_projects(10, None).unwrap().items[0].asset_count,
        2_000
    );
}

#[test]
fn project_asset_search_is_scoped_to_one_project() {
    let fixture = replica_fixture_with_assets(3);
    let page = fixture
        .manager
        .list_project_assets(&fixture.project_id, 10, None, Some("asset_00"))
        .unwrap();
    assert!(!page.items.is_empty());
    assert!(page.items.iter().all(|item| item.filename.contains("00")));
}

#[test]
fn listing_projects_reports_asset_counts() {
    let fixture = replica_fixture_with_assets(4);
    let page = fixture.manager.list_projects(10, None).unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].project_id, fixture.project_id);
    assert_eq!(page.items[0].asset_count, 4);
    assert_eq!(page.next_cursor, None);
}

#[test]
fn listing_projects_pages_deterministically_across_many_registered_replicas() {
    let fixture = Fixture::new();
    let mut project_ids: Vec<String> = Vec::new();
    for _ in 0..5 {
        let source = TempDir::new().unwrap();
        let envelope = baseline(&source);
        let registered = fixture.manager.import_baseline(&envelope).unwrap();
        project_ids.push(registered.project_id);
    }
    project_ids.sort();

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = fixture.manager.list_projects(2, cursor.as_deref()).unwrap();
        assert!(page.items.len() <= 2, "page exceeded its explicit limit");
        for item in &page.items {
            seen.push(item.project_id.clone());
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(
        seen, project_ids,
        "pages must be ordered by project_id and exhaustive"
    );
}

#[test]
fn list_projects_rejects_out_of_range_limits() {
    let fixture = replica_fixture_with_assets(1);
    assert!(matches!(
        fixture.manager.list_projects(0, None),
        Err(ProjectReplicaError::InvalidBaseline(_))
    ));
    assert!(matches!(
        fixture.manager.list_projects(201, None),
        Err(ProjectReplicaError::InvalidBaseline(_))
    ));
}

#[test]
fn listing_assets_for_an_unregistered_project_is_a_structured_error() {
    let fixture = replica_fixture_with_assets(1);
    let error = fixture
        .manager
        .list_project_assets("project_missing", 10, None, None)
        .unwrap_err();
    assert!(matches!(error, ProjectReplicaError::NotFound { .. }));
}

#[test]
fn list_project_assets_rejects_out_of_range_limits() {
    let fixture = replica_fixture_with_assets(1);
    assert!(matches!(
        fixture
            .manager
            .list_project_assets(&fixture.project_id, 0, None, None),
        Err(ProjectReplicaError::InvalidBaseline(_))
    ));
    assert!(matches!(
        fixture
            .manager
            .list_project_assets(&fixture.project_id, 201, None, None),
        Err(ProjectReplicaError::InvalidBaseline(_))
    ));
}

/// Documents the known limitation of the chosen search strategy: it is a
/// case-folded substring match over the raw `labels_json` text, not a
/// parsed-label match. A label of "covered" contains the literal
/// substring "red", so searching "red" surfaces it even though no label
/// equals "red". This is accepted for a bounded, per-project, human-typed
/// search (see the task report for the justification) - the test exists
/// to keep the assertion honest about actual behavior rather than
/// aspirational behavior.
#[test]
fn project_asset_search_over_labels_matches_json_substrings_including_false_positives() {
    let fixture = Fixture::new();
    let source_root = TempDir::new().unwrap();
    let catalog = Catalog::open(source_root.path().join("desktop.sqlite3")).unwrap();
    catalog.set_local_node_id("node_mac").unwrap();
    let project = catalog.create_project("Trip").unwrap();
    catalog
        .add_portable_project_asset(&project.id, portable_asset_payload(0))
        .unwrap();
    catalog.add_label("asset_000", "covered").unwrap();
    let envelope = BaselineEnvelope::export(&catalog, &project.id, vec![], vec![]).unwrap();
    let registered = fixture.manager.import_baseline(&envelope).unwrap();

    let page = fixture
        .manager
        .list_project_assets(&registered.project_id, 10, None, Some("red"))
        .unwrap();

    assert_eq!(
        page.items.len(),
        1,
        "labels_json substring match should hit 'covered'"
    );
    assert_eq!(page.items[0].labels, vec!["covered".to_string()]);
    assert!(
        !page.items[0].labels.iter().any(|label| label == "red"),
        "no label actually equals the search term - this is the accepted false positive"
    );
}
