use rcdvlog_core::{
    BindingVerificationStatus, GlobalMediaVerificationState, LocationVerificationState,
    MediaObjectId, RootAssociationConfirmation, RootAssociationConfirmationStatus,
    RootAssociationEvidence, RootRelativePath, StorageAccessMode, StorageObjectId, StorageProtocol,
    StorageRootId,
};
use rcdvlog_server_core::{
    ControlCatalog, FailureKind, JobHandler, LocalNodeRootBindingInput, NewMediaObject,
    NewStorageObject, NewStorageRoot, RepresentationJobHandler, RepresentationKind,
    RepresentationRequest, RepresentationService, RepresentationState, ServerConfig,
    open_writable_connection,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

struct Fixture {
    _root: TempDir,
    original_root: PathBuf,
    original: PathBuf,
    derived: PathBuf,
    catalog: ControlCatalog,
    media_id: MediaObjectId,
}

fn tools_available() -> bool {
    ["ffmpeg", "ffprobe"].iter().all(|tool| {
        Command::new(tool)
            .arg("-version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    })
}

fn fixture() -> Fixture {
    let root = TempDir::new().unwrap();
    let data = root.path().join("state");
    let original_root = root.path().join("original");
    let derived = root.path().join("derived");
    let backup = root.path().join("backup");
    fs::create_dir_all(&data).unwrap();
    fs::create_dir_all(&original_root).unwrap();
    fs::create_dir_all(&derived).unwrap();
    fs::create_dir_all(&backup).unwrap();
    let original = original_root.join("source clip.mp4");
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=640x360:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(&original)
        .status()
        .unwrap();
    assert!(status.success());
    let bytes = fs::read(&original).unwrap();
    let full_hash = blake3::hash(&bytes).to_hex().to_string();
    let config = ServerConfig::from_values(BTreeMap::from([
        ("RCDVLOG_DATA_DIR".into(), data.display().to_string()),
        ("RCDVLOG_DERIVED_DIR".into(), derived.display().to_string()),
        ("RCDVLOG_BACKUP_DIR".into(), backup.display().to_string()),
        (
            "RCDVLOG_ALLOWED_ORIGINAL_MOUNTS".into(),
            original_root.display().to_string(),
        ),
        ("RCDVLOG_FFMPEG".into(), "ffmpeg".into()),
        ("RCDVLOG_FFPROBE".into(), "ffprobe".into()),
        ("RCDVLOG_EXIFTOOL".into(), "exiftool".into()),
    ]))
    .unwrap();
    let catalog = ControlCatalog::open_or_create(&data, &config.diagnostics()).unwrap();
    let library = catalog.storage_library();
    let root_id = StorageRootId::parse("root_representation_test").unwrap();
    library
        .create_storage_root(NewStorageRoot {
            id: root_id.clone(),
            display_name: "Original Fixture".into(),
            immutable_original: true,
        })
        .unwrap();
    library
        .upsert_local_node_root_binding(LocalNodeRootBindingInput {
            storage_root_id: root_id.clone(),
            local_mount_path: original_root.clone(),
            root_subpath: None,
            protocol: StorageProtocol::Local,
            access_mode: StorageAccessMode::ReadOnly,
            verification_status: BindingVerificationStatus::Confirmed,
            enabled: true,
            association_evidence: Some(RootAssociationEvidence {
                matching_relative_paths: 1,
                matching_quick_fingerprints: 0,
                matching_full_hashes: 1,
                mismatches: 0,
                sampled_relative_paths: vec![],
                captured_at: "2026-07-23T00:00:00Z".into(),
            }),
            confirmation: RootAssociationConfirmation {
                status: RootAssociationConfirmationStatus::Confirmed,
                confirmed_by_node_id: Some(library.local_node_id().clone()),
                confirmed_at: Some("2026-07-23T00:00:00Z".into()),
            },
        })
        .unwrap();
    let storage_id = StorageObjectId::parse("storage_representation_source").unwrap();
    library
        .create_storage_object(NewStorageObject {
            id: storage_id.clone(),
            storage_root_id: root_id,
            relative_path: RootRelativePath::parse("source clip.mp4").unwrap(),
            size_bytes: Some(bytes.len() as u64),
            modified_at: None,
        })
        .unwrap();
    let media_id = MediaObjectId::parse("media_representation_source").unwrap();
    library
        .create_media_object(NewMediaObject {
            id: media_id.clone(),
            size_bytes: Some(bytes.len() as u64),
            quick_fingerprint: None,
            full_content_hash: Some(full_hash),
            verification_state: GlobalMediaVerificationState::Verified,
        })
        .unwrap();
    library
        .link_media_object_location(&media_id, &storage_id, LocationVerificationState::Verified)
        .unwrap();
    Fixture {
        _root: root,
        original_root,
        original,
        derived,
        catalog,
        media_id,
    }
}

fn service(fixture: &Fixture) -> RepresentationService {
    fixture.catalog.representations(fixture.derived.clone())
}

fn handler(fixture: &Fixture) -> RepresentationJobHandler {
    RepresentationJobHandler::new(
        fixture.catalog.storage_library(),
        fixture.catalog.jobs(),
        fixture.derived.clone(),
        PathBuf::from("ffmpeg"),
        PathBuf::from("ffprobe"),
    )
}

fn execute_next(fixture: &Fixture) -> Result<(), String> {
    let engine = fixture.catalog.jobs();
    let claimed = engine
        .claim_next("representation-test", std::time::Duration::from_secs(30))
        .unwrap()
        .expect("queued representation job");
    let handler = handler(fixture);
    match handler.run(claimed.clone(), CancellationToken::new()) {
        Ok(result) => engine
            .complete(&claimed, result.clone(), |tx| {
                handler.commit_result(tx, &claimed, &result)
            })
            .map_err(|error| error.to_string()),
        Err(error) => {
            engine
                .fail(&claimed, error.kind, &error.code, error.details)
                .unwrap();
            Err(error.code)
        }
    }
}

#[test]
fn duplicate_requests_reuse_one_content_addressed_proxy_and_preserve_originals() {
    if !tools_available() {
        return;
    }
    let fixture = fixture();
    let original_before = fs::read(&fixture.original).unwrap();
    let connection = open_writable_connection(fixture.catalog.database_path()).unwrap();
    for (project, asset, operation) in [
        ("project_one", "asset_one", "operation_one"),
        ("project_two", "asset_two", "operation_two"),
    ] {
        connection.execute(
            "INSERT INTO project_registry(project_id,replica_relative_path,state,registered_at,created_at,updated_at) VALUES(?1,?2,'registered',?3,?3,?3)",
            rusqlite::params![project, format!("projects/{project}/catalog.sqlite3"), "2026-07-23T00:00:00Z"],
        ).unwrap();
        connection.execute(
            "INSERT INTO project_media_memberships(project_id,project_asset_id,media_object_id,source_operation_id,projected_at) VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![project, asset, fixture.media_id.as_str(), operation, "2026-07-23T00:00:00Z"],
        ).unwrap();
    }
    let first_service = service(&fixture);
    let second_service = service(&fixture);
    let first_media = fixture.media_id.clone();
    let second_media = fixture.media_id.clone();
    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(move || {
            first_service
                .request(RepresentationRequest {
                    media_object_id: first_media,
                    kind: RepresentationKind::Proxy,
                })
                .unwrap()
        });
        let second = scope.spawn(move || {
            second_service
                .request(RepresentationRequest {
                    media_object_id: second_media,
                    kind: RepresentationKind::Proxy,
                })
                .unwrap()
        });
        (first.join().unwrap(), second.join().unwrap())
    });
    assert_eq!(first.representation.id, second.representation.id);
    assert_eq!(first.job_id, second.job_id);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM representations WHERE media_object_id=?1",
                [fixture.media_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    execute_next(&fixture).unwrap();
    let available = service(&fixture).get(&first.representation.id).unwrap();
    assert_eq!(available.state, RepresentationState::Available);
    assert!(available.available_on_node);
    let path = service(&fixture).content_path(&available.id).unwrap();
    assert!(path.starts_with(&fixture.derived));
    assert_eq!(fs::read(&fixture.original).unwrap(), original_before);
    assert_eq!(fs::read_dir(&fixture.original_root).unwrap().count(), 1);

    let reused = service(&fixture)
        .request(RepresentationRequest {
            media_object_id: fixture.media_id.clone(),
            kind: RepresentationKind::Proxy,
        })
        .unwrap();
    assert!(reused.reused);
    assert!(reused.job_id.is_none());

    let project_local = fixture
        .original_root
        .parent()
        .unwrap()
        .join("project-cache/proxy.mp4");
    fs::create_dir_all(project_local.parent().unwrap()).unwrap();
    fs::copy(&path, &project_local).unwrap();
    service(&fixture)
        .register_existing_compatible_output(&available.id, &project_local, Path::new("ffprobe"))
        .unwrap();
    fs::remove_file(&path).unwrap();
    assert_eq!(
        service(&fixture).content_path(&available.id).unwrap(),
        project_local
    );
}

#[test]
fn thumbnail_recipe_generates_valid_bounded_jpeg() {
    if !tools_available() {
        return;
    }
    let fixture = fixture();
    let initial_needs = service(&fixture).list_needs(&fixture.media_id).unwrap();
    assert_eq!(initial_needs.len(), 2);
    let request = service(&fixture)
        .request(RepresentationRequest {
            media_object_id: fixture.media_id.clone(),
            kind: RepresentationKind::Thumbnail,
        })
        .unwrap();
    execute_next(&fixture).unwrap();
    let path = service(&fixture)
        .content_path(&request.representation.id)
        .unwrap();
    assert_eq!(&fs::read(path).unwrap()[..2], &[0xff, 0xd8]);
    let remaining_needs = service(&fixture).list_needs(&fixture.media_id).unwrap();
    assert_eq!(remaining_needs.len(), 1);
    assert_eq!(remaining_needs[0].kind, RepresentationKind::Proxy);
}

#[test]
fn cancellation_cleans_only_its_temp_and_never_publishes() {
    if !tools_available() {
        return;
    }
    let fixture = fixture();
    let request = service(&fixture)
        .request(RepresentationRequest {
            media_object_id: fixture.media_id.clone(),
            kind: RepresentationKind::Proxy,
        })
        .unwrap();
    let engine = fixture.catalog.jobs();
    let claimed = engine
        .claim_next("representation-test", std::time::Duration::from_secs(30))
        .unwrap()
        .unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let failure = handler(&fixture)
        .run(claimed.clone(), cancellation)
        .unwrap_err();
    assert_eq!(failure.code, "canceled");
    engine
        .fail(
            &claimed,
            FailureKind::Terminal,
            &failure.code,
            failure.details,
        )
        .unwrap();
    assert_eq!(
        service(&fixture)
            .get(&request.representation.id)
            .unwrap()
            .state,
        RepresentationState::Pending
    );
    assert!(walk_files(&fixture.derived).is_empty());

    let retried = service(&fixture)
        .request(RepresentationRequest {
            media_object_id: fixture.media_id.clone(),
            kind: RepresentationKind::Proxy,
        })
        .unwrap();
    assert_ne!(retried.job_id, request.job_id);
}

#[test]
fn restart_after_file_commit_reconciles_without_regeneration() {
    if !tools_available() {
        return;
    }
    let fixture = fixture();
    let request = service(&fixture)
        .request(RepresentationRequest {
            media_object_id: fixture.media_id.clone(),
            kind: RepresentationKind::Proxy,
        })
        .unwrap();
    let engine = fixture.catalog.jobs();
    let claimed = engine
        .claim_next(
            "representation-before-restart",
            std::time::Duration::from_secs(30),
        )
        .unwrap()
        .unwrap();
    let first_handler = handler(&fixture);
    let _result = first_handler
        .run(claimed.clone(), CancellationToken::new())
        .unwrap();
    let committed_file = walk_files(&fixture.derived).into_iter().next().unwrap();
    let committed_bytes = fs::read(&committed_file).unwrap();
    engine
        .fail(
            &claimed,
            FailureKind::Retryable,
            "server_shutdown",
            serde_json::json!({"code": "server_shutdown"}),
        )
        .unwrap();
    let connection = open_writable_connection(fixture.catalog.database_path()).unwrap();
    connection
        .execute(
            "UPDATE jobs SET state='queued', available_at=?1 WHERE job_id=?2",
            [
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                request.job_id.unwrap().to_string(),
            ],
        )
        .unwrap();
    let claimed = engine
        .claim_next(
            "representation-after-restart",
            std::time::Duration::from_secs(30),
        )
        .unwrap()
        .unwrap();
    let restarted_handler = handler(&fixture);
    let result = restarted_handler
        .run(claimed.clone(), CancellationToken::new())
        .unwrap();
    engine
        .complete(&claimed, result.clone(), |tx| {
            restarted_handler.commit_result(tx, &claimed, &result)
        })
        .unwrap();
    assert_eq!(fs::read(committed_file).unwrap(), committed_bytes);
    assert_eq!(
        service(&fixture)
            .get(&request.representation.id)
            .unwrap()
            .state,
        RepresentationState::Available
    );
}

#[test]
fn missing_derived_root_is_distinct_from_offline_original() {
    if !tools_available() {
        return;
    }
    let fixture = fixture();
    let request = service(&fixture)
        .request(RepresentationRequest {
            media_object_id: fixture.media_id.clone(),
            kind: RepresentationKind::Proxy,
        })
        .unwrap();
    fs::remove_dir(&fixture.derived).unwrap();
    fs::write(&fixture.derived, b"not a directory").unwrap();
    assert_eq!(
        execute_next(&fixture).unwrap_err(),
        "derived_root_unavailable"
    );
    fs::remove_file(&fixture.derived).unwrap();
    fs::create_dir(&fixture.derived).unwrap();
    fs::remove_file(&fixture.original).unwrap();
    let connection = open_writable_connection(fixture.catalog.database_path()).unwrap();
    connection
        .execute(
            "UPDATE jobs SET state='queued', available_at=?1, completed_at=NULL WHERE job_id=?2",
            [
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                request.job_id.unwrap().to_string(),
            ],
        )
        .unwrap();
    assert_eq!(execute_next(&fixture).unwrap_err(), "media_offline");
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .collect()
}
