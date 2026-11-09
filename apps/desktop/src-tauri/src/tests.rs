use super::*;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn thumbnail_coordinator_deduplicates_overlapping_batches_until_release() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let coordinator = Arc::new(ThumbnailCoordinator::default());
    let first = coordinator
        .claim(&catalog, ["asset_a".to_string(), "asset_b".to_string()])
        .unwrap();
    assert_eq!(first.asset_ids, ["asset_a", "asset_b"]);

    let overlap = coordinator
        .claim(&catalog, ["asset_b".to_string(), "asset_c".to_string()])
        .unwrap();
    assert_eq!(overlap.asset_ids, ["asset_c"]);
    drop(first);

    let resumed = coordinator
        .claim(&catalog, ["asset_a".to_string(), "asset_b".to_string()])
        .unwrap();
    assert_eq!(resumed.asset_ids, ["asset_a", "asset_b"]);

    let external = catalog
        .try_claim_representation("asset_external", RepresentationKind::Thumbnail)
        .unwrap()
        .unwrap();
    let skipped = coordinator
        .claim(&catalog, ["asset_external".to_string()])
        .unwrap();
    assert!(skipped.asset_ids.is_empty());
    drop(external);
    assert_eq!(
        coordinator
            .claim(&catalog, ["asset_external".to_string()])
            .unwrap()
            .asset_ids,
        ["asset_external"]
    );
}

fn project_folder_fixture() -> (tempfile::TempDir, PathBuf, Catalog, String) {
    let directory = tempdir().unwrap();
    let folder = directory.path().join("Doomed");
    fs::create_dir_all(&folder).unwrap();
    let catalog = Catalog::open(folder.join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Doomed").unwrap();
    (directory, folder, catalog, project.id)
}

#[test]
fn delete_project_is_refused_while_background_work_runs() {
    let (_directory, folder, catalog, project_id) = project_folder_fixture();
    let catalog_path = folder.join("catalog.sqlite3");
    let work = project_work::ProjectWork::default();
    let guard = work
        .begin(&project_id, project_work::WorkKind::NodeJobs)
        .unwrap();

    let error = delete_project_blocking(
        &work,
        catalog_path.to_str().unwrap(),
        folder.to_str().unwrap(),
        &project_id,
    )
    .unwrap_err();
    assert!(error.contains("still running"), "{error}");
    assert!(error.contains("server-requested proxy/thumbnail jobs"));
    assert!(folder.exists(), "a refused delete touches nothing");
    assert_eq!(catalog.list_projects().unwrap().len(), 1);

    drop(guard);
    let audit = delete_project_blocking(
        &work,
        catalog_path.to_str().unwrap(),
        folder.to_str().unwrap(),
        &project_id,
    )
    .unwrap();
    assert!(audit.folder_removable);
    assert!(!folder.exists());
}

#[test]
fn a_failed_delete_lifts_the_tombstone() {
    let (_directory, folder, _catalog, project_id) = project_folder_fixture();
    let work = project_work::ProjectWork::default();
    // The folder holds no catalog at this path, so the audit cannot run.
    let missing = folder.join("missing.sqlite3");
    assert!(
        delete_project_blocking(
            &work,
            missing.to_str().unwrap(),
            folder.to_str().unwrap(),
            &project_id,
        )
        .is_err()
    );
    assert!(
        work.begin(&project_id, project_work::WorkKind::NodeJobs)
            .is_ok(),
        "nothing was deleted, so background work may continue"
    );
}

#[test]
fn background_thumbnails_after_the_tombstone_never_start_or_create_cache_dirs() {
    let (_directory, folder, catalog, project_id) = project_folder_fixture();
    let catalog_path = folder.join("catalog.sqlite3");
    let work = project_work::ProjectWork::default();
    delete_project_blocking(
        &work,
        catalog_path.to_str().unwrap(),
        folder.to_str().unwrap(),
        &project_id,
    )
    .unwrap();
    assert!(!folder.exists());

    let mut started = false;
    schedule_background_thumbnails_with(
        catalog,
        Arc::new(ThumbnailCoordinator::default()),
        &work,
        &project_id,
        vec!["asset_late".to_string()],
        |_| {},
        |_task| started = true,
    );
    assert!(!started, "work for a deleted project must not launch");
    assert!(!folder.exists(), "no cache directory is resurrected");
}

#[test]
fn a_running_detached_thumbnail_task_blocks_delete_until_it_ends() {
    let (_directory, folder, catalog, project_id) = project_folder_fixture();
    let catalog_path = folder.join("catalog.sqlite3");
    let work = project_work::ProjectWork::default();
    let mut parked: Option<Box<dyn FnOnce() -> anyhow::Result<ThumbnailReport> + Send>> = None;
    schedule_background_thumbnails_with(
        catalog,
        Arc::new(ThumbnailCoordinator::default()),
        &work,
        &project_id,
        vec!["asset_a".to_string()],
        |_| {},
        |task| parked = Some(task),
    );
    let task = parked.expect("the task was scheduled");
    // Scheduled but not yet run: the work is already registered.
    assert!(
        delete_project_blocking(
            &work,
            catalog_path.to_str().unwrap(),
            folder.to_str().unwrap(),
            &project_id,
        )
        .is_err()
    );
    drop(task);
    assert!(work.active(&project_id).is_empty());
}

#[test]
fn detached_q1_failure_does_not_revoke_a_completed_smart_import_and_can_be_repaired() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("ffmpeg is unavailable; skipping media integration test");
        return;
    }

    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Thumbnail recovery").unwrap();
    let source_dir = directory.path().join("source");
    fs::create_dir_all(&source_dir).unwrap();
    let source = source_dir.join("C0001.MP4");
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=navy:s=64x64:r=5:d=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            source.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success());
    let backup = directory.path().join("C0001.backup.mp4");
    fs::copy(&source, &backup).unwrap();

    let plan = catalog
        .preflight_import(&project.id, &source_dir, None, None, |_, _, _| true)
        .unwrap();
    let outcome = catalog
        .run_import(&project.id, &plan, true, |_| true)
        .unwrap();
    assert_eq!(outcome.imported, 1);
    assert!(!outcome.cancelled);
    let imported_ids = outcome.imported_asset_ids.clone();
    let session = catalog
        .record_import_session(&project.id, &plan, &outcome)
        .unwrap();

    // The import has already reported success when its detached Q1 task
    // sees the original disappear and records a recoverable failure.
    fs::remove_file(&source).unwrap();
    let mut automatic_report = None;
    schedule_background_thumbnails_with(
        catalog.clone(),
        Arc::new(ThumbnailCoordinator::default()),
        &project_work::ProjectWork::default(),
        &project.id,
        imported_ids.clone(),
        |_| {},
        |task| automatic_report = Some(task().unwrap()),
    );
    let failed = automatic_report.unwrap();
    assert_eq!(failed.generated, 0);
    assert_eq!(failed.failures.len(), 1);
    assert_eq!(
        outcome.imported, 1,
        "thumbnail work cannot revise the import outcome"
    );
    assert_eq!(
        catalog
            .import_session(&project.id, &session.entry.id)
            .unwrap()
            .entry
            .imported,
        1,
        "the completed import ledger remains available after detached failure"
    );
    assert_eq!(catalog.list_assets(&project.id).unwrap().len(), 1);

    fs::copy(&backup, &source).unwrap();
    let repaired = catalog.generate_missing_thumbnails(&project.id, 1).unwrap();
    assert_eq!(repaired.generated, 1);
    assert!(repaired.failures.is_empty());
    assert_eq!(
        catalog
            .get_asset(&imported_ids[0])
            .unwrap()
            .thumbnail_quality,
        1
    );
}

fn test_settings(projects_root: PathBuf) -> DesktopSettings {
    DesktopSettings {
        projects_root,
        node_id: "node_desktop_test".to_string(),
        autoplay_mode: default_autoplay_mode(),
        last_catalog_path: None,
        last_project_id: None,
        last_asset_id: None,
        last_context_updated_at: None,
        paired_server_node_id: None,
        paired_server_endpoint: None,
        paired_server_label: None,
        paired_at: None,
        paired_server_endpoints: None,
        make_proxies_on_import: true,
        last_import_archive_root_id: None,
        import_archive_root_ids_by_card_session: HashMap::new(),
    }
}

#[test]
fn analysis_progress_serializes_with_the_keys_the_frontend_mirrors() {
    let progress = AnalysisProgress {
        stage: AnalysisStage::Transcripts,
        stage_index: 1,
        stage_count: 3,
        asset: None,
        estimate: BatchProgressEstimate {
            completed_items: 2,
            total_items: 8,
            completed_work_units: 2,
            total_work_units: 8,
            worker_count: 1,
            work_units_per_worker_second: None,
            elapsed_seconds: 1.0,
            estimated_remaining_seconds: None,
            confidence: String::new(),
            estimated_remaining_seconds_range: None,
        },
        cost_usd: 0.5,
    };
    let value = serde_json::to_value(&progress).unwrap();
    assert_eq!(value["stage"], "transcripts");
    assert_eq!(value["stageIndex"], 1);
    assert_eq!(value["stageCount"], 3);
    assert_eq!(value["costUsd"], 0.5);
    assert!(value.get("estimate").is_some());
    assert!(value.get("asset").is_some(), "null asset stays present");
}

#[test]
fn each_stage_name_matches_the_run_record_value() {
    // The stage a run reached is persisted as this string and read back for
    // display, so the two spellings must not drift.
    assert_eq!(AnalysisStage::Proxies.as_str(), "proxies");
    assert_eq!(AnalysisStage::Transcripts.as_str(), "transcripts");
    assert_eq!(AnalysisStage::Analysis.as_str(), "analysis");
}

#[test]
fn a_cancel_from_a_previous_run_does_not_stop_the_next_one() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let state = AppState::new_for_test(
        catalog,
        test_settings(directory.path().join("projects")),
        directory.path().join("settings.json"),
    )
    .unwrap();

    assert!(!state.analysis_cancel.load(Ordering::Relaxed));
    state.analysis_cancel.store(true, Ordering::Relaxed);

    // `analyze_project` clears the flag as it starts rather than when a
    // cancel is handled; this models that entry point.
    state.analysis_cancel.store(false, Ordering::Relaxed);
    assert!(!state.analysis_cancel.load(Ordering::Relaxed));
}

#[test]
fn switches_to_an_existing_catalog_atomically() {
    let directory = tempdir().unwrap();
    let original_path = directory.path().join("original.sqlite3");
    let original = Catalog::open(&original_path).unwrap();
    let original_project = original.create_project("Original").unwrap();
    let state = AppState::new_for_test(
        original,
        test_settings(directory.path().join("projects")),
        directory.path().join("settings.json"),
    )
    .unwrap();

    let loaded_path = directory.path().join("loaded.sqlite3");
    let loaded_catalog = Catalog::open(&loaded_path).unwrap();
    let loaded_project = loaded_catalog.create_project("Loaded project").unwrap();
    let loaded = state.load_catalog(loaded_path.clone()).unwrap();
    assert_eq!(loaded.path, loaded_path.to_string_lossy());
    assert_eq!(loaded.projects.len(), 1);
    assert_eq!(loaded.projects[0].id, loaded_project.id);
    assert_eq!(state.catalog().unwrap().database_path(), loaded_path);
    let context = state.desktop_settings().unwrap();
    assert_eq!(
        context.last_catalog_path.as_deref(),
        Some(loaded_path.as_path())
    );
    assert_eq!(context.last_project_id, None);

    let missing = directory.path().join("missing.sqlite3");
    assert!(state.load_catalog(missing).is_err());
    let active_projects = state.catalog().unwrap().list_projects().unwrap();
    assert_eq!(active_projects.len(), 1);
    assert_eq!(active_projects[0].id, loaded_project.id);
    assert_ne!(active_projects[0].id, original_project.id);
}

#[test]
fn creates_discovers_and_configures_one_folder_per_project() {
    let directory = tempdir().unwrap();
    let scratch = Catalog::open(directory.path().join("scratch.sqlite3")).unwrap();
    let projects_root = directory.path().join("VideoProjects");
    let settings_path = directory.path().join("settings.json");
    let state = AppState::new_for_test(
        scratch,
        test_settings(projects_root.clone()),
        settings_path.clone(),
    )
    .unwrap();

    let first = state.create_project("Hokkaido: Winter/2026").unwrap();
    let first_folder = projects_root.join("Hokkaido- Winter-2026");
    assert!(first_folder.join("catalog.sqlite3").is_file());
    assert_eq!(
        state.desktop_settings().unwrap().last_project_id,
        Some(first.id.clone())
    );
    let second = state.create_project("Hokkaido: Winter/2026").unwrap();
    assert!(
        projects_root
            .join("Hokkaido- Winter-2026 2")
            .join("catalog.sqlite3")
            .is_file()
    );

    fs::create_dir(projects_root.join("Not a project")).unwrap();
    let library = state.project_library().unwrap();
    assert_eq!(library.root_path, projects_root.to_string_lossy());
    assert_eq!(library.projects.len(), 2);
    assert!(
        library
            .projects
            .iter()
            .any(|item| item.project.id == first.id)
    );
    assert!(
        library
            .projects
            .iter()
            .any(|item| item.project.id == second.id)
    );

    let alternate_root = directory.path().join("AlternateProjects");
    let alternate = state.set_projects_root(alternate_root.clone()).unwrap();
    let alternate_root = alternate_root.canonicalize().unwrap();
    assert_eq!(alternate.root_path, alternate_root.to_string_lossy());
    assert!(alternate.projects.is_empty());
    let persisted = load_settings(&settings_path, PathBuf::from("fallback"));
    assert_eq!(persisted.projects_root, alternate_root);
    assert_eq!(persisted.autoplay_mode, AutoplayMode::ProxyAvailable);

    let updated = state.set_autoplay_mode(AutoplayMode::Always).unwrap();
    assert_eq!(updated.autoplay_mode, AutoplayMode::Always);
    assert_eq!(updated.projects_root, alternate_root);
    let reloaded = load_settings(&settings_path, PathBuf::from("fallback"));
    assert_eq!(reloaded, updated);
    let alternate_project = state.create_project("Expo").unwrap();
    assert!(alternate_root.join("Expo/catalog.sqlite3").is_file());
    assert_eq!(
        state.catalog().unwrap().list_projects().unwrap()[0].id,
        alternate_project.id
    );
}

#[test]
fn old_settings_default_autoplay_to_proxy_media() {
    let directory = tempdir().unwrap();
    let settings_path = directory.path().join("settings.json");
    let projects_root = directory.path().join("projects");
    fs::write(
        &settings_path,
        serde_json::to_vec(&serde_json::json!({
            "projectsRoot": projects_root,
        }))
        .unwrap(),
    )
    .unwrap();

    let settings = load_settings(&settings_path, PathBuf::from("fallback"));
    assert_eq!(settings.projects_root, projects_root);
    assert_eq!(settings.autoplay_mode, AutoplayMode::ProxyAvailable);
    assert!(settings.import_archive_root_ids_by_card_session.is_empty());
}

#[test]
fn the_media_origin_reaches_the_frontend_but_never_the_settings_file() {
    let directory = tempdir().unwrap();
    let settings_path = directory.path().join("settings.json");
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let state = AppState::new_for_test(
        catalog,
        test_settings(directory.path().join("projects")),
        settings_path.clone(),
    )
    .unwrap();

    // Before the origin binds, the frontend is told there is none rather
    // than being handed a URL that would never answer.
    let view = state.settings_view(state.desktop_settings().unwrap());
    assert_eq!(view.media_origin, None);

    let origin = state.start_media_origin().unwrap();
    assert!(origin.base_url.starts_with("http://127.0.0.1:"));
    assert!(!origin.base_url.ends_with(":0"));
    assert!(origin.token.len() >= 22);

    let view =
        serde_json::to_value(state.settings_view(state.desktop_settings().unwrap())).unwrap();
    assert_eq!(view["mediaOrigin"]["baseUrl"], origin.base_url);
    assert_eq!(view["mediaOrigin"]["token"], origin.token);
    // The flattened settings still carry every field the frontend reads.
    assert_eq!(view["nodeId"], "node_desktop_test");
    assert!(view.get("projectsRoot").is_some());

    // Requirement: the token never touches disk.
    state.set_autoplay_mode(AutoplayMode::Always).unwrap();
    let persisted = fs::read_to_string(&settings_path).unwrap();
    assert!(
        !persisted.contains(&origin.token),
        "the session token was written to settings.json"
    );
    assert!(!persisted.contains("mediaOrigin"));
}

#[test]
fn the_media_origin_restarts_at_most_once_a_session() {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let state = AppState::new_for_test(
        catalog,
        test_settings(directory.path().join("projects")),
        directory.path().join("settings.json"),
    )
    .unwrap();

    let first = state.start_media_origin().unwrap();
    let second = state.restart_media_origin().unwrap();
    // A new port and, more importantly, a new token: the old one is gone.
    assert_ne!(first.token, second.token);
    assert_ne!(first.base_url, second.base_url);
    assert_eq!(state.media_origin(), Some(second));

    assert!(
        state.restart_media_origin().is_err(),
        "a second restart would turn a failing listener into a retry loop"
    );

    state.stop_media_origin();
    assert_eq!(state.media_origin(), None);
}

#[test]
fn storage_roots_cover_project_media_and_exclude_the_rest() {
    use media_server::MediaCatalog as _;

    let directory = tempdir().unwrap();
    let projects_root = directory.path().join("VideoProjects");
    let catalog_path = directory.path().join("elsewhere").join("catalog.sqlite3");
    fs::create_dir_all(catalog_path.parent().unwrap()).unwrap();
    let catalog = Catalog::open(&catalog_path).unwrap();
    let state = AppState::new_for_test(
        catalog,
        test_settings(projects_root.clone()),
        directory.path().join("settings.json"),
    )
    .unwrap();

    let roots = state.media_catalog.storage_roots();
    assert!(
        roots.contains(&projects_root),
        "the projects folder is a root"
    );
    assert!(
        roots.contains(&catalog_path.parent().unwrap().to_path_buf()),
        "a catalog opened outside the projects folder is still reachable"
    );
    assert!(
        !roots.iter().any(|root| root == Path::new("/")),
        "the whole filesystem is never a root"
    );
    assert!(!roots.iter().any(|root| root == Path::new("/etc")));
}

#[test]
fn media_paths_come_from_the_catalog_not_the_caller() {
    use media_server::MediaCatalog as _;

    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Media").unwrap();
    let state = AppState::new_for_test(
        catalog,
        test_settings(directory.path().join("projects")),
        directory.path().join("settings.json"),
    )
    .unwrap();

    // An id that is not in the open catalog resolves to nothing, which the
    // origin reports as 404 without ever touching the filesystem.
    assert_eq!(state.media_catalog.locate("asset_not_here"), None);
    assert!(
        state
            .catalog()
            .unwrap()
            .list_assets(&project.id)
            .unwrap()
            .is_empty()
    );
}
