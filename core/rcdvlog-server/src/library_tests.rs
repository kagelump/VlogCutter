use super::*;
use crate::{ControlCatalog, ServerConfig};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    catalog: ControlCatalog,
    library: StorageLibrary,
}

fn fixture() -> Fixture {
    let root = TempDir::new().unwrap();
    let data = root.path().join("state");
    let derived = root.path().join("derived");
    let backups = root.path().join("backups");
    fs::create_dir_all(&data).unwrap();
    fs::create_dir_all(&derived).unwrap();
    fs::create_dir_all(&backups).unwrap();
    let config = ServerConfig::from_values(BTreeMap::from([
        ("RCDVLOG_DATA_DIR".to_string(), data.display().to_string()),
        (
            "RCDVLOG_DERIVED_DIR".to_string(),
            derived.display().to_string(),
        ),
        (
            "RCDVLOG_BACKUP_DIR".to_string(),
            backups.display().to_string(),
        ),
    ]))
    .unwrap();
    let catalog = ControlCatalog::open_or_create(&data, &config.diagnostics()).unwrap();
    let library = catalog.storage_library();
    Fixture {
        _root: root,
        catalog,
        library,
    }
}

fn id_root(value: &str) -> StorageRootId {
    StorageRootId::parse(value).unwrap()
}

fn id_node(value: &str) -> NodeId {
    NodeId::parse(value).unwrap()
}

fn id_media(value: &str) -> MediaObjectId {
    MediaObjectId::parse(value).unwrap()
}

fn id_object(value: &str) -> StorageObjectId {
    StorageObjectId::parse(value).unwrap()
}

fn create_root(library: &StorageLibrary, id: &str, immutable: bool) -> StorageRootId {
    let id = id_root(id);
    library
        .create_storage_root(NewStorageRoot {
            id: id.clone(),
            display_name: format!("Root {id}"),
            immutable_original: immutable,
        })
        .unwrap();
    id
}

fn confirmed_capability_input(
    node: &NodeId,
    root: &StorageRootId,
    subpath: Option<&str>,
) -> NodeRootCapabilityInput {
    NodeRootCapabilityInput {
        node_id: node.clone(),
        storage_root_id: root.clone(),
        root_subpath: subpath.map(|value| RootRelativePath::parse(value).unwrap()),
        protocol: StorageProtocol::Nfs,
        access_mode: StorageAccessMode::ReadOnly,
        verification_status: BindingVerificationStatus::Confirmed,
        enabled: true,
        association_evidence: Some(RootAssociationEvidence {
            matching_relative_paths: 2,
            matching_quick_fingerprints: 1,
            matching_full_hashes: 1,
            mismatches: 0,
            sampled_relative_paths: vec![RootRelativePath::parse("Trips/2026/sample.mp4").unwrap()],
            captured_at: "2026-07-20T00:00:00Z".to_string(),
        }),
        confirmation: RootAssociationConfirmation {
            status: RootAssociationConfirmationStatus::Confirmed,
            confirmed_by_node_id: Some(node.clone()),
            confirmed_at: Some("2026-07-20T00:01:00Z".to_string()),
        },
    }
}

fn confirmed_capability(
    library: &StorageLibrary,
    node: &NodeId,
    root: &StorageRootId,
    subpath: Option<&str>,
) -> NodeRootCapability {
    library
        .upsert_node_root_capability(confirmed_capability_input(node, root, subpath))
        .unwrap()
}

fn confirmed_local_binding(
    library: &StorageLibrary,
    root: &StorageRootId,
    mount: &Path,
    subpath: Option<&str>,
) -> NodeRootBindingAdmin {
    let node = library.local_node_id().clone();
    let capability = confirmed_capability_input(&node, root, subpath);
    library
        .upsert_local_node_root_binding(LocalNodeRootBindingInput {
            storage_root_id: capability.storage_root_id,
            local_mount_path: mount.to_path_buf(),
            root_subpath: capability.root_subpath,
            protocol: capability.protocol,
            access_mode: capability.access_mode,
            verification_status: capability.verification_status,
            enabled: capability.enabled,
            association_evidence: capability.association_evidence,
            confirmation: capability.confirmation,
        })
        .unwrap()
}

fn create_media(library: &StorageLibrary, id: &str) -> MediaObjectId {
    let id = id_media(id);
    library
        .create_media_object(NewMediaObject {
            id: id.clone(),
            size_bytes: None,
            quick_fingerprint: None,
            full_content_hash: None,
            verification_state: GlobalMediaVerificationState::Unverified,
        })
        .unwrap();
    id
}

fn create_object(
    library: &StorageLibrary,
    id: &str,
    root: &StorageRootId,
    path: &str,
) -> StorageObjectId {
    let id = id_object(id);
    library
        .create_storage_object(NewStorageObject {
            id: id.clone(),
            storage_root_id: root.clone(),
            relative_path: RootRelativePath::parse(path).unwrap(),
            size_bytes: Some(7),
            modified_at: Some("2026-07-20T00:00:00Z".to_string()),
        })
        .unwrap();
    id
}

fn link(library: &StorageLibrary, media: &MediaObjectId, object: &StorageObjectId) {
    library
        .link_media_object_location(media, object, LocationVerificationState::Unverified)
        .unwrap();
}

fn observe(
    library: &StorageLibrary,
    generation: &str,
    object: &StorageObjectId,
    status: ObservationStatus,
) {
    library
        .record_asset_observation(
            generation,
            AssetObservationInput {
                storage_object_id: object.clone(),
                status,
                observed_size_bytes: Some(7),
                observed_quick_fingerprint: None,
            },
        )
        .unwrap();
}

#[test]
fn logical_root_resolves_through_distinct_full_and_partial_bindings_read_only() {
    let fixture = fixture();
    let root_id = create_root(&fixture.library, "root_main_nas", true);
    let mac = id_node("node_mac");
    let server = fixture.catalog.node_id().clone();
    let mac_mount = fixture._root.path().join("Mac NAS");
    let server_mount = fixture._root.path().join("server-subtree");
    fs::create_dir_all(mac_mount.join("Trips/2026/日本")).unwrap();
    fs::create_dir_all(server_mount.join("日本")).unwrap();
    fs::write(mac_mount.join("Trips/2026/日本/clip 01.MP4"), b"fixture").unwrap();
    fs::write(server_mount.join("日本/clip 01.MP4"), b"fixture").unwrap();
    let mac_capability = confirmed_capability(&fixture.library, &mac, &root_id, None);
    let server_binding = confirmed_local_binding(
        &fixture.library,
        &root_id,
        &server_mount,
        Some("Trips/2026"),
    );
    assert_eq!(server_binding.local_mount_path, server_mount);
    assert_eq!(
        server_binding.confirmation.status,
        RootAssociationConfirmationStatus::Confirmed
    );
    assert_eq!(
        server_binding
            .association_evidence
            .unwrap()
            .matching_full_hashes,
        1
    );
    let object = create_object(
        &fixture.library,
        "storage_clip",
        &root_id,
        "Trips/2026/日本/clip 01.MP4",
    );

    assert!(matches!(
        fixture.library.start_observation_generation(
            "generation_partial_invalid_whole_root",
            &server,
            ObservationScope {
                storage_root_id: root_id.clone(),
                relative_prefix: None,
            },
        ),
        Err(StorageLibraryError::InvalidInput {
            field: "scope.relativePrefix",
            ..
        })
    ));
    let partial_generation = fixture
        .library
        .start_observation_generation(
            "generation_partial_valid",
            &server,
            ObservationScope {
                storage_root_id: root_id.clone(),
                relative_prefix: Some(RootRelativePath::parse("Trips/2026/日本").unwrap()),
            },
        )
        .unwrap();
    assert_eq!(
        partial_generation.scope.relative_prefix.unwrap().as_str(),
        "Trips/2026/日本"
    );
    fixture
        .library
        .abandon_observation_generation("generation_partial_valid")
        .unwrap();

    let logical = RootRelativePath::parse("Trips/2026/日本/clip 01.MP4").unwrap();
    assert_eq!(
        resolve_existing_binding_path(
            &mac_mount,
            None,
            &logical,
            true,
            StorageAccessMode::ReadOnly,
            PathAccessIntent::Read,
        )
        .unwrap(),
        mac_mount
            .join("Trips/2026/日本/clip 01.MP4")
            .canonicalize()
            .unwrap()
    );
    let mac_capability_json = serde_json::to_value(&mac_capability).unwrap();
    assert!(mac_capability_json.get("localMountPath").is_none());
    assert!(
        !mac_capability_json
            .to_string()
            .contains(mac_mount.to_str().unwrap())
    );
    let capabilities_json = serde_json::to_value(
        fixture
            .library
            .list_node_root_capabilities(Some(&root_id))
            .unwrap(),
    )
    .unwrap()
    .to_string();
    assert!(!capabilities_json.contains("localMountPath"));
    assert!(!capabilities_json.contains(mac_mount.to_str().unwrap()));
    assert!(!capabilities_json.contains(server_mount.to_str().unwrap()));
    let local_admin = fixture
        .library
        .list_local_node_root_bindings_admin()
        .unwrap();
    assert_eq!(local_admin.len(), 1);
    assert_eq!(local_admin[0].node_id, server);
    let connection = open_read_only_connection(fixture.catalog.database_path()).unwrap();
    let remote_path: Option<String> = connection
        .query_row(
            "SELECT local_mount_path FROM node_root_bindings WHERE node_id = ?1 AND storage_root_id = ?2",
            params![mac.as_str(), root_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(remote_path.is_none());
    assert_eq!(
        fixture
            .library
            .resolve_local_storage_object(&object, PathAccessIntent::Read)
            .unwrap(),
        server_mount
            .join("日本/clip 01.MP4")
            .canonicalize()
            .unwrap()
    );
    assert!(matches!(
        fixture
            .library
            .resolve_local_storage_object(&object, PathAccessIntent::Write),
        Err(StorageLibraryError::Path(PathResolutionError {
            reason: crate::PathResolutionErrorReason::ImmutableOriginal,
            ..
        }))
    ));
    assert!(!mac_mount.join(".rcdvlog-storage-root").exists());
    assert!(!server_mount.join(".rcdvlog-storage-root").exists());
}

#[test]
fn local_resolution_requires_enabled_confirmed_association() {
    let fixture = fixture();
    let root = create_root(&fixture.library, "root_resolution_state", true);
    let server = fixture.catalog.node_id().clone();
    let mount = fixture._root.path().join("resolution-mount");
    fs::create_dir_all(&mount).unwrap();
    let object = create_object(
        &fixture.library,
        "storage_resolution_state",
        &root,
        "clip.mp4",
    );

    for (verification, confirmation) in [
        (
            BindingVerificationStatus::Pending,
            RootAssociationConfirmationStatus::Pending,
        ),
        (
            BindingVerificationStatus::Rejected,
            RootAssociationConfirmationStatus::Rejected,
        ),
        (
            BindingVerificationStatus::Unavailable,
            RootAssociationConfirmationStatus::Pending,
        ),
        (
            BindingVerificationStatus::Mismatch,
            RootAssociationConfirmationStatus::Pending,
        ),
    ] {
        fixture
            .library
            .upsert_local_node_root_binding(LocalNodeRootBindingInput {
                storage_root_id: root.clone(),
                local_mount_path: mount.clone(),
                root_subpath: None,
                protocol: StorageProtocol::Nfs,
                access_mode: StorageAccessMode::ReadOnly,
                verification_status: verification,
                enabled: true,
                association_evidence: None,
                confirmation: RootAssociationConfirmation {
                    status: confirmation,
                    confirmed_by_node_id: None,
                    confirmed_at: None,
                },
            })
            .unwrap();
        assert!(matches!(
            fixture
                .library
                .resolve_local_storage_object(&object, PathAccessIntent::Read,),
            Err(StorageLibraryError::InvalidInput {
                field: "binding",
                ..
            })
        ));
    }

    confirmed_local_binding(&fixture.library, &root, &mount, None);
    fixture
        .library
        .set_node_root_binding_enabled(&server, &root, false)
        .unwrap();
    assert!(
        fixture
            .library
            .resolve_local_storage_object(&object, PathAccessIntent::Read)
            .is_err()
    );
    fixture
        .library
        .set_node_root_binding_enabled(&server, &root, true)
        .unwrap();
    fixture.library.disable_storage_root(&root).unwrap();
    assert!(
        fixture
            .library
            .resolve_local_storage_object(&object, PathAccessIntent::Read)
            .is_err()
    );
}

#[test]
fn media_locations_membership_projection_and_cursor_pages_remain_separate() {
    let fixture = fixture();
    let root = create_root(&fixture.library, "root_library", true);
    let media = create_media(&fixture.library, "media_shared");
    let first_storage = create_object(&fixture.library, "storage_first", &root, "Trips/A/clip.mp4");
    let second_storage = create_object(
        &fixture.library,
        "storage_second",
        &root,
        "Archive/copy.mp4",
    );
    link(&fixture.library, &media, &first_storage);
    link(&fixture.library, &media, &second_storage);

    let connection = open_writable_connection(fixture.catalog.database_path()).unwrap();
    for (project, path) in [
        ("project_one", "projects/one/catalog.sqlite3"),
        ("project_two", "projects/two/catalog.sqlite3"),
    ] {
        connection
            .execute(
                r#"
                    INSERT INTO project_registry(
                        project_id, replica_relative_path, state, registered_at, created_at, updated_at
                    ) VALUES (?1, ?2, 'registered', ?3, ?3, ?3)
                    "#,
                params![project, path, "2026-07-20T00:00:00Z"],
            )
            .unwrap();
    }
    for (project, asset, operation) in [
        ("project_one", "asset_one", "operation_one"),
        ("project_two", "asset_two", "operation_two"),
    ] {
        fixture
            .library
            .upsert_project_media_membership_projection(ProjectMediaMembershipProjection {
                project_id: ProjectId::parse(project).unwrap(),
                project_asset_id: ProjectAssetId::parse(asset).unwrap(),
                media_object_id: media.clone(),
                source_operation_id: OperationId::parse(operation).unwrap(),
                projected_at: "2026-07-20T00:02:00Z".to_string(),
            })
            .unwrap();
    }

    let item = fixture
        .library
        .list_media(MediaLibraryQuery::default())
        .unwrap()
        .items
        .into_iter()
        .find(|item| item.media.id == media)
        .unwrap();
    assert_eq!(item.storage_location_count, 2);
    assert_eq!(item.project_membership_count, 2);

    let columns = connection
        .prepare("SELECT name FROM pragma_table_info('project_media_memberships')")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<BTreeSet<_>>>()
        .unwrap();
    for forbidden in ["notes", "labels", "removed", "rotation", "clock_event"] {
        assert!(!columns.contains(forbidden));
    }

    create_media(&fixture.library, "media_alpha");
    create_media(&fixture.library, "media_zulu");
    let expected = fixture
        .library
        .list_media(MediaLibraryQuery {
            limit: 200,
            ..MediaLibraryQuery::default()
        })
        .unwrap()
        .items
        .into_iter()
        .map(|item| item.media.id)
        .collect::<Vec<_>>();
    let first = fixture
        .library
        .list_media(MediaLibraryQuery {
            limit: 2,
            ..MediaLibraryQuery::default()
        })
        .unwrap();
    assert_eq!(first.items.len(), 2);
    let second = fixture
        .library
        .list_media(MediaLibraryQuery {
            limit: 2,
            after: first.next_cursor.clone(),
            verification_state: None,
        })
        .unwrap();
    let paged = first
        .items
        .into_iter()
        .chain(second.items)
        .map(|item| item.media.id)
        .collect::<Vec<_>>();
    assert_eq!(paged, expected);
    assert!(second.next_cursor.is_none());
}

#[test]
fn verified_media_requires_consistent_full_identity() {
    let fixture = fixture();
    let hash = "a".repeat(64);
    let missing_size = fixture.library.create_media_object(NewMediaObject {
        id: id_media("media_missing_size"),
        size_bytes: None,
        quick_fingerprint: None,
        full_content_hash: Some(hash.clone()),
        verification_state: GlobalMediaVerificationState::Verified,
    });
    assert!(matches!(
        missing_size,
        Err(StorageLibraryError::InvalidInput {
            field: "verificationState",
            ..
        })
    ));
    fixture
        .library
        .create_media_object(NewMediaObject {
            id: id_media("media_verified"),
            size_bytes: Some(100),
            quick_fingerprint: Some("b".repeat(64)),
            full_content_hash: Some(hash.clone()),
            verification_state: GlobalMediaVerificationState::Verified,
        })
        .unwrap();
    let contradictory = fixture.library.create_media_object(NewMediaObject {
        id: id_media("media_contradictory"),
        size_bytes: Some(101),
        quick_fingerprint: None,
        full_content_hash: Some(hash),
        verification_state: GlobalMediaVerificationState::Verified,
    });
    assert!(matches!(
        contradictory,
        Err(StorageLibraryError::Database(_))
    ));
}

#[test]
fn observation_generations_publish_atomically_by_started_sequence_and_scope() {
    let fixture = fixture();
    let root = create_root(&fixture.library, "root_observed", true);
    let server = fixture.catalog.node_id().clone();
    let mac = id_node("node_mac_observed");
    let server_mount = fixture._root.path().join("server-mount");
    fs::create_dir_all(&server_mount).unwrap();
    confirmed_local_binding(&fixture.library, &root, &server_mount, None);

    let media = create_media(&fixture.library, "media_observed");
    let inside = create_object(
        &fixture.library,
        "storage_inside",
        &root,
        "Trips/2026/inside.mp4",
    );
    let outside = create_object(
        &fixture.library,
        "storage_outside",
        &root,
        "Archive/outside.mp4",
    );
    link(&fixture.library, &media, &inside);
    link(&fixture.library, &media, &outside);

    assert_eq!(
        fixture
            .library
            .media_availability(&media, &server)
            .unwrap()
            .state,
        NodeAvailabilityState::TemporarilyUnavailable
    );
    let full = fixture
        .library
        .start_observation_generation(
            "generation_full",
            &server,
            ObservationScope {
                storage_root_id: root.clone(),
                relative_prefix: None,
            },
        )
        .unwrap();
    assert_eq!(full.sequence, 1);
    observe(
        &fixture.library,
        "generation_full",
        &inside,
        ObservationStatus::Accessible,
    );
    observe(
        &fixture.library,
        "generation_full",
        &outside,
        ObservationStatus::Accessible,
    );
    assert_eq!(
        fixture
            .library
            .media_availability(&media, &server)
            .unwrap()
            .state,
        NodeAvailabilityState::TemporarilyUnavailable
    );
    fixture
        .library
        .complete_observation_generation("generation_full")
        .unwrap();
    assert_eq!(
        fixture
            .library
            .media_availability(&media, &server)
            .unwrap()
            .state,
        NodeAvailabilityState::AccessibleHere
    );

    let narrow = fixture
        .library
        .start_observation_generation(
            "generation_narrow",
            &server,
            ObservationScope {
                storage_root_id: root.clone(),
                relative_prefix: Some(RootRelativePath::parse("Trips/2026").unwrap()),
            },
        )
        .unwrap();
    assert_eq!(narrow.sequence, 2);
    fixture
        .library
        .complete_observation_generation("generation_narrow")
        .unwrap();
    let scoped = fixture.library.media_availability(&media, &server).unwrap();
    let inside_state = scoped
        .locations
        .iter()
        .find(|location| location.storage_object_id == inside)
        .unwrap();
    let outside_state = scoped
        .locations
        .iter()
        .find(|location| location.storage_object_id == outside)
        .unwrap();
    assert_eq!(
        inside_state.state,
        NodeAvailabilityState::MissingAfterCompletedObservation
    );
    assert_eq!(inside_state.completed_generation_sequence, Some(2));
    assert_eq!(outside_state.state, NodeAvailabilityState::AccessibleHere);
    assert_eq!(outside_state.completed_generation_sequence, Some(1));

    let late_old = fixture
        .library
        .start_observation_generation(
            "generation_late_old",
            &server,
            ObservationScope {
                storage_root_id: root.clone(),
                relative_prefix: Some(RootRelativePath::parse("Trips/2026").unwrap()),
            },
        )
        .unwrap();
    observe(
        &fixture.library,
        "generation_late_old",
        &inside,
        ObservationStatus::FingerprintMismatch,
    );
    let newer = fixture
        .library
        .start_observation_generation(
            "generation_newer",
            &server,
            ObservationScope {
                storage_root_id: root.clone(),
                relative_prefix: Some(RootRelativePath::parse("Trips/2026").unwrap()),
            },
        )
        .unwrap();
    assert!(newer.sequence > late_old.sequence);
    observe(
        &fixture.library,
        "generation_newer",
        &inside,
        ObservationStatus::Accessible,
    );
    fixture
        .library
        .complete_observation_generation("generation_newer")
        .unwrap();
    fixture
        .library
        .complete_observation_generation("generation_late_old")
        .unwrap();
    let after_late_completion = fixture.library.media_availability(&media, &server).unwrap();
    let inside_state = after_late_completion
        .locations
        .iter()
        .find(|location| location.storage_object_id == inside)
        .unwrap();
    assert_eq!(inside_state.state, NodeAvailabilityState::AccessibleHere);
    assert_eq!(
        inside_state.completed_generation_sequence,
        Some(newer.sequence)
    );

    let interrupted = fixture
        .library
        .start_observation_generation(
            "generation_interrupted",
            &server,
            ObservationScope {
                storage_root_id: root.clone(),
                relative_prefix: Some(RootRelativePath::parse("Trips/2026").unwrap()),
            },
        )
        .unwrap();
    observe(
        &fixture.library,
        "generation_interrupted",
        &inside,
        ObservationStatus::TemporarilyUnavailable,
    );
    let ignored = fixture.library.media_availability(&media, &server).unwrap();
    let inside_state = ignored
        .locations
        .iter()
        .find(|location| location.storage_object_id == inside)
        .unwrap();
    assert_eq!(inside_state.state, NodeAvailabilityState::AccessibleHere);
    assert_ne!(
        inside_state.completed_generation_sequence,
        Some(interrupted.sequence)
    );

    confirmed_capability(&fixture.library, &mac, &root, None);
    let mac_generation = fixture
        .library
        .start_observation_generation(
            "generation_mac",
            &mac,
            ObservationScope {
                storage_root_id: root.clone(),
                relative_prefix: None,
            },
        )
        .unwrap();
    assert_eq!(mac_generation.sequence, 1);
    observe(
        &fixture.library,
        "generation_mac",
        &inside,
        ObservationStatus::Accessible,
    );
    fixture
        .library
        .complete_observation_generation("generation_mac")
        .unwrap();
    fixture
        .library
        .set_node_root_binding_enabled(&server, &root, false)
        .unwrap();
    assert_eq!(
        fixture
            .library
            .media_availability(&media, &server)
            .unwrap()
            .state,
        NodeAvailabilityState::AccessibleOnAnotherNode
    );

    fixture.library.disable_storage_root(&root).unwrap();
    let disabled = fixture.library.media_availability(&media, &server).unwrap();
    assert_eq!(
        disabled.state,
        NodeAvailabilityState::TemporarilyUnavailable
    );
    assert_eq!(disabled.locations.len(), 4);
    assert_eq!(fixture.library.get_media_object(&media).unwrap().id, media);
    assert_eq!(
        fixture.library.get_storage_object(&inside).unwrap().id,
        inside
    );
}

#[test]
fn availability_is_queried_node_aware_and_location_mismatch_is_authoritative() {
    let fixture = fixture();
    let root = create_root(&fixture.library, "root_availability_precedence", true);
    let server = fixture.catalog.node_id().clone();
    let remote = id_node("node_remote_available");
    let unbound = id_node("node_unbound_query");
    let media = create_media(&fixture.library, "media_availability_precedence");
    let object = create_object(
        &fixture.library,
        "storage_availability_precedence",
        &root,
        "Trips/clip.mp4",
    );
    link(&fixture.library, &media, &object);

    assert_eq!(
        fixture
            .library
            .media_availability(&media, &server)
            .unwrap()
            .state,
        NodeAvailabilityState::Known
    );

    let server_mount = fixture._root.path().join("availability-server");
    fs::create_dir_all(&server_mount).unwrap();
    confirmed_local_binding(&fixture.library, &root, &server_mount, None);
    confirmed_capability(&fixture.library, &remote, &root, None);

    fixture
        .library
        .start_observation_generation(
            "generation_server_mismatch",
            &server,
            ObservationScope {
                storage_root_id: root.clone(),
                relative_prefix: None,
            },
        )
        .unwrap();
    observe(
        &fixture.library,
        "generation_server_mismatch",
        &object,
        ObservationStatus::FingerprintMismatch,
    );
    fixture
        .library
        .complete_observation_generation("generation_server_mismatch")
        .unwrap();

    fixture
        .library
        .start_observation_generation(
            "generation_remote_accessible",
            &remote,
            ObservationScope {
                storage_root_id: root.clone(),
                relative_prefix: None,
            },
        )
        .unwrap();
    observe(
        &fixture.library,
        "generation_remote_accessible",
        &object,
        ObservationStatus::Accessible,
    );
    fixture
        .library
        .complete_observation_generation("generation_remote_accessible")
        .unwrap();

    assert_eq!(
        fixture
            .library
            .media_availability(&media, &server)
            .unwrap()
            .state,
        NodeAvailabilityState::FingerprintMismatch
    );
    assert_eq!(
        fixture
            .library
            .media_availability(&media, &unbound)
            .unwrap()
            .state,
        NodeAvailabilityState::AccessibleOnAnotherNode
    );

    fixture
        .library
        .link_media_object_location(&media, &object, LocationVerificationState::Mismatch)
        .unwrap();
    assert_eq!(
        fixture
            .library
            .media_availability(&media, &unbound)
            .unwrap()
            .state,
        NodeAvailabilityState::FingerprintMismatch
    );
}

#[test]
fn partial_binding_scope_cannot_supersede_older_full_root_outside_its_subtree() {
    let fixture = fixture();
    let root = create_root(&fixture.library, "root_partial_generation", true);
    let server = fixture.catalog.node_id().clone();
    let mount = fixture._root.path().join("partial-generation-mount");
    fs::create_dir_all(&mount).unwrap();
    confirmed_local_binding(&fixture.library, &root, &mount, None);
    let media = create_media(&fixture.library, "media_partial_generation");
    let inside = create_object(
        &fixture.library,
        "storage_partial_inside",
        &root,
        "Trips/inside.mp4",
    );
    let outside = create_object(
        &fixture.library,
        "storage_partial_outside",
        &root,
        "Archive/outside.mp4",
    );
    link(&fixture.library, &media, &inside);
    link(&fixture.library, &media, &outside);

    fixture
        .library
        .start_observation_generation(
            "generation_before_partial",
            &server,
            ObservationScope {
                storage_root_id: root.clone(),
                relative_prefix: None,
            },
        )
        .unwrap();
    observe(
        &fixture.library,
        "generation_before_partial",
        &inside,
        ObservationStatus::Accessible,
    );
    observe(
        &fixture.library,
        "generation_before_partial",
        &outside,
        ObservationStatus::Accessible,
    );
    fixture
        .library
        .complete_observation_generation("generation_before_partial")
        .unwrap();

    confirmed_local_binding(&fixture.library, &root, &mount, Some("Trips"));
    assert!(
        fixture
            .library
            .start_observation_generation(
                "generation_partial_illegal_full",
                &server,
                ObservationScope {
                    storage_root_id: root.clone(),
                    relative_prefix: None,
                },
            )
            .is_err()
    );
    fixture
        .library
        .start_observation_generation(
            "generation_after_partial",
            &server,
            ObservationScope {
                storage_root_id: root,
                relative_prefix: Some(RootRelativePath::parse("Trips").unwrap()),
            },
        )
        .unwrap();
    fixture
        .library
        .complete_observation_generation("generation_after_partial")
        .unwrap();

    let availability = fixture.library.media_availability(&media, &server).unwrap();
    let inside_detail = availability
        .locations
        .iter()
        .find(|location| location.storage_object_id == inside)
        .unwrap();
    assert_eq!(
        inside_detail.state,
        NodeAvailabilityState::MissingAfterCompletedObservation
    );
    assert!(
        !availability
            .locations
            .iter()
            .any(|location| location.storage_object_id == outside)
    );

    let connection = open_read_only_connection(fixture.catalog.database_path()).unwrap();
    let preserved_old_observation: u32 = connection
        .query_row(
            r#"
                SELECT COUNT(*) FROM node_asset_observations
                WHERE generation_id = 'generation_before_partial' AND storage_object_id = ?1
                "#,
            [outside.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(preserved_old_observation, 1);
}

#[test]
fn observation_writes_are_unique_scoped_and_completion_is_idempotent() {
    let fixture = fixture();
    let root = create_root(&fixture.library, "root_generation_rules", true);
    let server = fixture.catalog.node_id().clone();
    let mount = fixture._root.path().join("mount");
    fs::create_dir_all(&mount).unwrap();
    confirmed_local_binding(&fixture.library, &root, &mount, None);
    let inside = create_object(
        &fixture.library,
        "storage_generation_inside",
        &root,
        "Trips/inside.mp4",
    );
    let outside = create_object(
        &fixture.library,
        "storage_generation_outside",
        &root,
        "Archive/outside.mp4",
    );
    fixture
        .library
        .start_observation_generation(
            "generation_rules",
            &server,
            ObservationScope {
                storage_root_id: root,
                relative_prefix: Some(RootRelativePath::parse("Trips").unwrap()),
            },
        )
        .unwrap();
    observe(
        &fixture.library,
        "generation_rules",
        &inside,
        ObservationStatus::Accessible,
    );
    assert!(matches!(
        fixture.library.record_asset_observation(
            "generation_rules",
            AssetObservationInput {
                storage_object_id: inside.clone(),
                status: ObservationStatus::Accessible,
                observed_size_bytes: None,
                observed_quick_fingerprint: None,
            },
        ),
        Err(StorageLibraryError::ObservationAlreadyRecorded { .. })
    ));
    assert!(matches!(
        fixture.library.record_asset_observation(
            "generation_rules",
            AssetObservationInput {
                storage_object_id: outside,
                status: ObservationStatus::Accessible,
                observed_size_bytes: None,
                observed_quick_fingerprint: None,
            },
        ),
        Err(StorageLibraryError::ObservationOutsideScope { .. })
    ));
    let first = fixture
        .library
        .complete_observation_generation("generation_rules")
        .unwrap();
    let retry = fixture
        .library
        .complete_observation_generation("generation_rules")
        .unwrap();
    assert_eq!(first, retry);
    assert!(matches!(
        fixture.library.record_asset_observation(
            "generation_rules",
            AssetObservationInput {
                storage_object_id: inside,
                status: ObservationStatus::Accessible,
                observed_size_bytes: None,
                observed_quick_fingerprint: None,
            },
        ),
        Err(StorageLibraryError::InvalidGenerationState { .. })
    ));
}

#[test]
fn linking_a_location_refreshes_the_search_projection() {
    let fixture = fixture();
    let library = &fixture.library;
    let root = create_root(library, "root_a", true);
    let media = create_media(library, "media_a");
    let object = create_object(library, "object_a", &root, "Trips/C0103.MP4");
    link(library, &media, &object);

    let connection = open_read_only_connection(library.database_path()).unwrap();
    let filename: String = connection
        .query_row(
            "SELECT filename FROM media_search_index WHERE media_object_id = ?1",
            [media.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(filename, "C0103.MP4");
}

#[test]
fn verifying_a_media_object_updates_the_search_projection_verification_state() {
    let fixture = fixture();
    let library = &fixture.library;
    let root = create_root(library, "root_a", true);
    let media = create_media(library, "media_a");
    let object = create_object(library, "object_a", &root, "Trips/C0104.MP4");
    link(library, &media, &object);

    library
        .verify_media_object(&media, &"a".repeat(64), 7)
        .unwrap();

    let connection = open_read_only_connection(library.database_path()).unwrap();
    let verification_state: String = connection
        .query_row(
            "SELECT verification_state FROM media_search_index WHERE media_object_id = ?1",
            [media.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(verification_state, "verified");
}

fn corrupt_search_projection_filename(library: &StorageLibrary, media: &MediaObjectId) {
    let connection = open_writable_connection(library.database_path()).unwrap();
    connection
        .execute(
            "UPDATE media_search_index SET filename = 'STALE_PROJECTION' WHERE media_object_id = ?1",
            [media.as_str()],
        )
        .unwrap();
}

#[test]
fn updating_location_verification_refreshes_the_search_projection() {
    let fixture = fixture();
    let library = &fixture.library;
    let root = create_root(library, "root_a", true);
    let media = create_media(library, "media_a");
    let object = create_object(library, "object_a", &root, "Trips/C0105.MP4");
    link(library, &media, &object);

    // Hand-corrupt the projection so the assertion below can only pass
    // if `update_location_verification` actually refreshes it, not
    // because `link` already left the correct value behind.
    corrupt_search_projection_filename(library, &media);

    library
        .update_location_verification(&media, &object, LocationVerificationState::Provisional)
        .unwrap();

    let connection = open_read_only_connection(library.database_path()).unwrap();
    let filename: String = connection
        .query_row(
            "SELECT filename FROM media_search_index WHERE media_object_id = ?1",
            [media.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        filename, "C0105.MP4",
        "update_location_verification must refresh the search projection, \
             not leave a stale row in place"
    );
}

#[test]
fn relinking_storage_to_media_refreshes_both_the_old_and_new_search_projections() {
    let fixture = fixture();
    let library = &fixture.library;
    let root = create_root(library, "root_a", true);
    let old_media = create_media(library, "media_old");
    let new_media = create_media(library, "media_new");
    let object = create_object(library, "object_a", &root, "Trips/C0106.MP4");
    link(library, &old_media, &object);

    // Hand-corrupt both projections so the assertions below can only
    // pass if `relink_storage_to_media` actually refreshes each one.
    corrupt_search_projection_filename(library, &old_media);
    corrupt_search_projection_filename(library, &new_media);

    library
        .relink_storage_to_media(
            &old_media,
            &new_media,
            &object,
            LocationVerificationState::Verified,
        )
        .unwrap();

    let connection = open_read_only_connection(library.database_path()).unwrap();
    let (old_filename, old_primary, old_count): (String, Option<String>, u32) = connection
        .query_row(
            "SELECT filename, primary_storage_object_id, storage_location_count
                 FROM media_search_index WHERE media_object_id = ?1",
            [old_media.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        old_filename, "",
        "the old media object lost its only location and must show no filename"
    );
    assert_eq!(old_primary, None);
    assert_eq!(old_count, 0);

    let (new_filename, new_primary, new_count): (String, Option<String>, u32) = connection
        .query_row(
            "SELECT filename, primary_storage_object_id, storage_location_count
                 FROM media_search_index WHERE media_object_id = ?1",
            [new_media.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(new_filename, "C0106.MP4");
    assert_eq!(new_primary.as_deref(), Some(object.as_str()));
    assert_eq!(new_count, 1);
}

#[test]
fn search_pages_deterministically_across_a_full_archive() {
    let fixture = fixture();
    let library = &fixture.library;
    let root = create_root(library, "root_a", true);
    for index in 0..25 {
        let media = create_media(library, &format!("media_{index:03}"));
        let object = create_object(
            library,
            &format!("object_{index:03}"),
            &root,
            &format!("Trips/C{index:04}.MP4"),
        );
        link(library, &media, &object);
    }

    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = library
            .search_media(
                LibraryQuery {
                    limit: 10,
                    sort: LibrarySort::FilenameAsc,
                    filters: Default::default(),
                    after: cursor.clone(),
                },
                None,
            )
            .unwrap();
        seen.extend(page.items.iter().map(|item| item.filename.clone()));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(seen.len(), 25);
    let mut sorted = seen.clone();
    sorted.sort();
    assert_eq!(seen, sorted, "pages must not repeat or reorder rows");
}

#[test]
fn availability_filter_marks_itself_as_applied_and_never_pages_the_whole_archive() {
    let fixture = fixture();
    let library = &fixture.library;
    let root = create_root(library, "root_a", true);
    let media = create_media(library, "media_a");
    let object = create_object(library, "object_a", &root, "Trips/A.MP4");
    link(library, &media, &object);

    let page = library
        .search_media(
            LibraryQuery {
                limit: 10,
                sort: LibrarySort::CreatedDesc,
                filters: Default::default(),
                after: None,
            },
            Some(NodeAvailabilityState::AccessibleHere),
        )
        .unwrap();
    assert!(page.availability_filter_applied);
    assert!(
        page.items
            .iter()
            .all(|item| item.availability == NodeAvailabilityState::AccessibleHere)
    );
}

#[test]
fn search_media_can_return_an_empty_page_with_a_cursor_when_the_filter_matches_nothing_yet() {
    // No confirmed node/root binding is set up for `root_a`, so every one
    // of these rows resolves to `NodeAvailabilityState::Known` (see
    // `aggregate_availability`'s fallback when there are no locations to
    // reason about). Filtering for `AccessibleHere` therefore drops every
    // row on the first page, while the 15 rows created here (against a
    // limit of 10) guarantee there is a second page to advance to. This
    // is exactly the "empty items, but next_cursor is still Some" shape
    // the pagination contract documented on `search_media` calls out --
    // a caller must keep paging instead of treating the empty page as
    // the end of the result set.
    let fixture = fixture();
    let library = &fixture.library;
    let root = create_root(library, "root_a", true);
    for index in 0..15 {
        let media = create_media(library, &format!("media_{index:03}"));
        let object = create_object(
            library,
            &format!("object_{index:03}"),
            &root,
            &format!("Trips/C{index:04}.MP4"),
        );
        link(library, &media, &object);
    }

    let page = library
        .search_media(
            LibraryQuery {
                limit: 10,
                sort: LibrarySort::CreatedDesc,
                filters: Default::default(),
                after: None,
            },
            Some(NodeAvailabilityState::AccessibleHere),
        )
        .unwrap();

    assert!(page.availability_filter_applied);
    assert!(
        page.items.is_empty(),
        "every row on this page is Known, not AccessibleHere"
    );
    assert!(
        page.next_cursor.is_some(),
        "15 rows exist against a limit of 10, so a caller must keep paging \
             even though this page came back empty"
    );

    // Confirms the archive genuinely has more matching-eligible rows
    // beyond this page -- the cursor is not a dead end.
    let next_page = library
        .search_media(
            LibraryQuery {
                limit: 10,
                sort: LibrarySort::CreatedDesc,
                filters: Default::default(),
                after: page.next_cursor,
            },
            Some(NodeAvailabilityState::AccessibleHere),
        )
        .unwrap();
    assert!(next_page.availability_filter_applied);
    assert!(next_page.items.is_empty());
    assert!(next_page.next_cursor.is_none());
}

#[test]
fn search_does_not_load_the_archive_to_serve_one_page() {
    let fixture = fixture();
    let library = &fixture.library;
    let root = create_root(library, "root_a", true);
    for index in 0..2000 {
        let media = create_media(library, &format!("media_{index:05}"));
        let object = create_object(
            library,
            &format!("object_{index:05}"),
            &root,
            &format!("Trips/C{index:05}.MP4"),
        );
        link(library, &media, &object);
    }
    let page = library
        .search_media(
            LibraryQuery {
                limit: 50,
                sort: LibrarySort::CreatedDesc,
                filters: Default::default(),
                after: None,
            },
            None,
        )
        .unwrap();
    assert_eq!(page.items.len(), 50);
    assert!(page.next_cursor.is_some());
}
