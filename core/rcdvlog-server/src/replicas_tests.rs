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
                "RCDVLOG_ATTACHED_PROJECT_ROOTS".to_string(),
                std::env::temp_dir().display().to_string(),
            ),
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

fn desktop_path(root: &TempDir) -> PathBuf {
    root.path().join("desktop.sqlite3").canonicalize().unwrap()
}

#[test]
fn attach_serves_the_cli_catalog_in_place_of_its_replica_and_detach_returns() {
    let fixture = Fixture::new();
    let source_root = TempDir::new().unwrap();
    let envelope = baseline(&source_root);
    let project_id = envelope.payload.project.project_id.clone();
    fixture.manager.import_baseline(&envelope).unwrap();
    // Work the CLI did after the replica was made never reached it.
    Catalog::open_existing(desktop_path(&source_root))
        .unwrap()
        .create_note_revision("asset_desktop", "made in the CLI")
        .unwrap();
    let note = |manager: &ProjectReplicaManager| {
        manager
            .open_catalog(&project_id)
            .unwrap()
            .get_asset("asset_desktop")
            .unwrap()
            .notes
    };
    assert_eq!(note(&fixture.manager), "");

    let attached = fixture
        .manager
        .attach_catalog(
            &desktop_path(&source_root),
            &[source_root.path().to_path_buf()],
            None,
        )
        .unwrap();
    assert!(attached.is_attached());
    assert_eq!(attached.project_id, project_id);
    assert_eq!(note(&fixture.manager), "made in the CLI");
    assert_eq!(
        fixture.manager.list_projects(10, None).unwrap().items.len(),
        1
    );
    // Sync never writes into an attached catalog.
    assert!(matches!(
        fixture.manager.import_baseline(&envelope),
        Err(ProjectReplicaError::Attached { .. })
    ));
    // The node-local path never leaves the server in a serialized replica.
    assert!(
        !serde_json::to_string(&attached)
            .unwrap()
            .contains("desktop.sqlite3")
    );

    let detached = fixture.manager.detach_catalog(&project_id).unwrap();
    assert!(!detached.is_attached());
    assert_eq!(note(&fixture.manager), "");
}

#[test]
fn attach_without_a_replica_registers_the_project_but_cannot_detach() {
    let fixture = Fixture::new();
    let source_root = TempDir::new().unwrap();
    let envelope = baseline(&source_root);
    let attached = fixture
        .manager
        .attach_catalog(
            &desktop_path(&source_root),
            &[source_root.path().to_path_buf()],
            None,
        )
        .unwrap();
    assert_eq!(attached.project_id, envelope.payload.project.project_id);
    assert_eq!(attached.origin_node_id.as_deref(), Some("node_mac"));
    let page = fixture.manager.list_projects(10, None).unwrap();
    assert_eq!(page.items[0].asset_count, 1);
    assert!(matches!(
        fixture.manager.detach_catalog(&attached.project_id),
        Err(ProjectReplicaError::AttachRefused(_))
    ));
}

#[test]
fn attach_refuses_catalogs_outside_the_allowed_roots() {
    let fixture = Fixture::new();
    let source_root = TempDir::new().unwrap();
    baseline(&source_root);
    let elsewhere = TempDir::new().unwrap();
    for roots in [vec![], vec![elsewhere.path().to_path_buf()]] {
        assert!(matches!(
            fixture
                .manager
                .attach_catalog(&desktop_path(&source_root), &roots, None),
            Err(ProjectReplicaError::AttachRefused(_))
        ));
    }
    assert!(matches!(
        fixture.manager.attach_catalog(
            &source_root.path().join("missing.sqlite3"),
            &[source_root.path().to_path_buf()],
            None
        ),
        Err(ProjectReplicaError::AttachRefused(_))
    ));
    assert!(
        fixture
            .manager
            .list_projects(10, None)
            .unwrap()
            .items
            .is_empty()
    );
}

#[test]
fn the_server_never_migrates_an_attached_catalog() {
    let fixture = Fixture::new();
    let source_root = TempDir::new().unwrap();
    baseline(&source_root);
    let path = desktop_path(&source_root);
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "DELETE FROM schema_migrations WHERE migration_id = (SELECT MAX(migration_id) FROM schema_migrations)",
            [],
        )
        .unwrap();
    let before: i64 = connection
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    let error = fixture
        .manager
        .attach_catalog(&path, &[source_root.path().to_path_buf()], None)
        .unwrap_err();
    assert!(
        matches!(error, ProjectReplicaError::AttachRefused(ref reason) if reason.contains("schema"))
    );
    let after: i64 = connection
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(before, after);
}

#[test]
fn asset_thumbnail_path_is_what_the_attached_catalog_records() {
    let fixture = Fixture::new();
    let source_root = TempDir::new().unwrap();
    let envelope = baseline(&source_root);
    let project_id = envelope.payload.project.project_id.clone();
    let thumbnail = source_root
        .path()
        .join("cache/thumbnails/asset_desktop/q1.v2.jpg");
    fs::create_dir_all(thumbnail.parent().unwrap()).unwrap();
    fs::write(&thumbnail, b"jpeg").unwrap();
    rusqlite::Connection::open(desktop_path(&source_root))
        .unwrap()
        .execute(
            "INSERT INTO asset_thumbnails(asset_id, quality, path, created_at) VALUES ('asset_desktop', 1, ?1, '2026-11-08T00:00:00Z')",
            [thumbnail.display().to_string()],
        )
        .unwrap();
    fixture
        .manager
        .attach_catalog(
            &desktop_path(&source_root),
            &[source_root.path().to_path_buf()],
            None,
        )
        .unwrap();
    assert_eq!(
        fixture
            .manager
            .asset_thumbnail_path(&project_id, "asset_desktop")
            .unwrap(),
        Some(thumbnail)
    );
    assert_eq!(
        fixture
            .manager
            .asset_thumbnail_path(&project_id, "asset_missing")
            .unwrap(),
        None
    );
}
#[test]
fn attached_catalog_accepts_concurrent_cli_and_server_operations_without_changing_node_identity() {
    let fixture = Fixture::new();
    let root = TempDir::new().unwrap();
    let envelope = baseline(&root);
    let project_id = envelope.payload.project.project_id;
    fixture
        .manager
        .attach_catalog(&desktop_path(&root), &[root.path().to_path_buf()], None)
        .unwrap();
    let cli = Catalog::open_shared_existing(desktop_path(&root)).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let cli_barrier = barrier.clone();
    let writer = std::thread::spawn(move || {
        cli_barrier.wait();
        for index in 0..12 {
            cli.add_label("asset_desktop", &format!("cli-{index}"))
                .unwrap();
        }
    });
    barrier.wait();
    for index in 0..12 {
        fixture
            .manager
            .add_review_label(&project_id, "asset_desktop", &format!("web-{index}"), 0)
            .unwrap();
    }
    writer.join().unwrap();
    let cli = Catalog::open_read_only_existing(desktop_path(&root)).unwrap();
    let labels = cli.get_asset("asset_desktop").unwrap().labels;
    assert_eq!(labels.len(), 24);
    for index in 0..12 {
        assert!(labels.contains(&format!("cli-{index}")));
        assert!(labels.contains(&format!("web-{index}")));
    }
    assert_eq!(cli.local_node_id().unwrap().as_deref(), Some("node_mac"));
    assert_eq!(
        fixture
            .manager
            .get_review_asset(&project_id, "asset_desktop")
            .unwrap()
            .asset
            .labels,
        labels
    );
}

#[test]
fn attached_access_rechecks_allowlist_and_schema_after_registration() {
    let mut fixture = Fixture::new();
    let root = TempDir::new().unwrap();
    let envelope = baseline(&root);
    let project_id = envelope.payload.project.project_id;
    let path = desktop_path(&root);
    fixture
        .manager
        .attach_catalog(&path, &[root.path().to_path_buf()], None)
        .unwrap();
    fixture.manager.attached_project_roots.clear();
    assert!(matches!(
        fixture.manager.open_catalog(&project_id),
        Err(ProjectReplicaError::AttachRefused(_))
    ));
    fixture
        .manager
        .attached_project_roots
        .push(root.path().to_path_buf());
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute("DELETE FROM schema_migrations WHERE migration_id=(SELECT MAX(migration_id) FROM schema_migrations)", []).unwrap();
    assert!(matches!(
        fixture.manager.open_catalog(&project_id),
        Err(ProjectReplicaError::AttachRefused(_))
    ));
    assert!(Catalog::open_shared_existing(&path).is_err());
    let count: u32 = connection
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, rcdvlog_core::PROJECT_CATALOG_SCHEMA_VERSION - 1);
}
#[test]
fn attached_proxy_availability_uses_the_cli_cache_without_a_media_library_link() {
    let mut fixture = Fixture::new();
    let root = TempDir::new().unwrap();
    let envelope = baseline(&root);
    let project_id = &envelope.payload.project.project_id;
    let path = desktop_path(&root);
    let original = root.path().join("original.mov");
    let proxy = root.path().join("clip.v3.mp4");
    fs::write(&original, b"12345").unwrap();
    fs::write(&proxy, b"generated mp4").unwrap();
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute(
        "UPDATE assets SET path=?1, proxy_path=?2 WHERE id='asset_desktop'",
        params![original.to_str().unwrap(), proxy.to_str().unwrap()],
    )
    .unwrap();
    fixture.manager.import_baseline(&envelope).unwrap();
    assert!(
        !fixture
            .manager
            .get_review_asset(project_id, "asset_desktop")
            .unwrap()
            .local_proxy_available
    );
    fixture
        .manager
        .attach_catalog(&path, &[root.path().to_path_buf()], None)
        .unwrap();
    let item = fixture
        .manager
        .get_review_asset(project_id, "asset_desktop")
        .unwrap();
    assert!(item.local_proxy_available);
    assert!(item.asset.media_object_id.is_none());
    assert!(item.proxy.is_none());
    assert_eq!(
        item.original_availability,
        Some(NodeAvailabilityState::AccessibleHere)
    );
    let serialized = serde_json::to_string(&item).unwrap();
    assert!(!serialized.contains(root.path().to_str().unwrap()));
    assert_eq!(
        fixture
            .manager
            .asset_proxy_path(project_id, "asset_desktop")
            .unwrap(),
        Some(proxy.canonicalize().unwrap())
    );
    // A reachable proxy remains playable when the original is offline.
    fs::remove_file(&original).unwrap();
    let item = fixture
        .manager
        .get_review_asset(project_id, "asset_desktop")
        .unwrap();
    assert!(item.local_proxy_available);
    assert_eq!(
        item.original_availability,
        Some(NodeAvailabilityState::TemporarilyUnavailable)
    );
    let legacy = root.path().join("camera-normalized-v1.mp4");
    fs::write(&legacy, b"normalized mp4").unwrap();
    c.execute(
        "UPDATE assets SET proxy_path=?1 WHERE id='asset_desktop'",
        [legacy.to_str().unwrap()],
    )
    .unwrap();
    assert!(
        fixture
            .manager
            .get_review_asset(project_id, "asset_desktop")
            .unwrap()
            .local_proxy_available
    );
    c.execute(
        "UPDATE assets SET metadata_rotation_degrees=90 WHERE id='asset_desktop'",
        [],
    )
    .unwrap();
    assert!(
        !fixture
            .manager
            .get_review_asset(project_id, "asset_desktop")
            .unwrap()
            .local_proxy_available
    );
    c.execute(
        "UPDATE assets SET metadata_rotation_degrees=0 WHERE id='asset_desktop'",
        [],
    )
    .unwrap();
    fixture.manager.attached_project_roots = vec![root.path().to_path_buf()];
    for recorded in [
        root.path().join("missing.v3.mp4"),
        root.path().join("old.v2.mp4"),
        root.path().join("empty.v3.mp4"),
    ] {
        if recorded.file_name().unwrap() != "missing.v3.mp4" {
            fs::write(
                &recorded,
                if recorded.to_string_lossy().contains("empty") {
                    &b""[..]
                } else {
                    &b"old"[..]
                },
            )
            .unwrap();
        }
        c.execute(
            "UPDATE assets SET proxy_path=?1 WHERE id='asset_desktop'",
            [recorded.to_str().unwrap()],
        )
        .unwrap();
        assert!(
            !fixture
                .manager
                .get_review_asset(project_id, "asset_desktop")
                .unwrap()
                .local_proxy_available
        );
    }
    let outside = fixture._root.path().join("outside.v3.mp4");
    fs::write(&outside, b"outside").unwrap();
    c.execute(
        "UPDATE assets SET proxy_path=?1 WHERE id='asset_desktop'",
        [outside.to_str().unwrap()],
    )
    .unwrap();
    assert!(
        fixture
            .manager
            .asset_proxy_path(project_id, "asset_desktop")
            .unwrap()
            .is_none()
    );
    #[cfg(unix)]
    {
        let link = root.path().join("escape.v3.mp4");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        c.execute(
            "UPDATE assets SET proxy_path=?1 WHERE id='asset_desktop'",
            [link.to_str().unwrap()],
        )
        .unwrap();
        assert!(
            fixture
                .manager
                .asset_proxy_path(project_id, "asset_desktop")
                .unwrap()
                .is_none()
        );
    }
    // Even overlapping allowlists cannot expose server state, or the original itself.
    fixture
        .manager
        .attached_project_roots
        .push(fixture._root.path().to_path_buf());
    let secret = fixture.manager.data_dir.join("private.v3.mp4");
    fs::write(&secret, b"private").unwrap();
    for (source, recorded) in [
        (original.as_path(), secret.as_path()),
        (proxy.as_path(), proxy.as_path()),
    ] {
        c.execute(
            "UPDATE assets SET path=?1, proxy_path=?2 WHERE id='asset_desktop'",
            params![source.to_str().unwrap(), recorded.to_str().unwrap()],
        )
        .unwrap();
        assert!(
            fixture
                .manager
                .asset_proxy_path(project_id, "asset_desktop")
                .unwrap()
                .is_none()
        );
    }
}
