use super::*;
use crate::{THUMBNAIL_TASK_KIND, sampled_fingerprint, tool_command};
use std::io::{BufRead, BufReader, Write};
use std::ops::ControlFlow;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

fn catalog_with_asset() -> (TempDir, Catalog, String, PathBuf) {
    let temp = TempDir::new().unwrap();
    let catalog = Catalog::open(temp.path().join("catalog.sqlite3")).unwrap();
    let project = catalog
        .create_project("Representation claim fixture")
        .unwrap();
    let source = temp.path().join("source.mp4");
    fs::write(&source, b"synthetic source bytes").unwrap();
    crate::tests::insert_asset_at(&catalog, &project.id, "asset_claim", &source);
    (temp, catalog, project.id, source)
}

fn set_proxy_path(catalog: &Catalog, asset_id: &str, path: &Path) {
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET proxy_path = ?2, proxy_generation_id = NULL WHERE id = ?1",
            rusqlite::params![asset_id, path.to_string_lossy()],
        )
        .unwrap();
}

fn make_one_second_video(path: &Path) -> bool {
    match tool_command("ffmpeg").arg("-version").output() {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return false,
        Err(error) => panic!("could not run ffmpeg: {error}"),
    }
    let status = tool_command("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=navy:s=160x90:r=10:d=1",
            "-c:v",
            "mpeg4",
        ])
        .arg(path)
        .status()
        .expect("run ffmpeg to create the commit fixture");
    assert!(
        status.success(),
        "ffmpeg failed to create the commit fixture"
    );
    true
}

#[test]
fn same_asset_and_kind_contend_and_lockfile_persists_after_release() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("cache");
    let claim = RepresentationClaim::try_acquire(&root, "asset/../雪", RepresentationKind::Proxy)
        .unwrap()
        .unwrap();
    assert_eq!(claim.cache_root(), fs::canonicalize(&root).unwrap());
    assert_eq!(claim.asset_id(), "asset/../雪");
    assert_eq!(claim.kind(), RepresentationKind::Proxy);
    assert!(
        RepresentationClaim::try_acquire(&root, "asset/../雪", RepresentationKind::Proxy)
            .unwrap()
            .is_none()
    );
    drop(claim);

    let lock_directory = fs::canonicalize(&root).unwrap().join(LOCK_DIRECTORY);
    let lockfiles = fs::read_dir(&lock_directory)
        .unwrap()
        .collect::<io::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(lockfiles.len(), 1);
    assert!(lockfiles[0].path().is_file());
    assert!(
        RepresentationClaim::try_acquire(&root, "asset/../雪", RepresentationKind::Proxy)
            .unwrap()
            .is_some()
    );
}

#[test]
fn different_assets_and_kinds_have_independent_claims() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("cache");
    let first_proxy = RepresentationClaim::try_acquire(&root, "asset-1", RepresentationKind::Proxy)
        .unwrap()
        .unwrap();
    let first_thumbnail =
        RepresentationClaim::try_acquire(&root, "asset-1", RepresentationKind::Thumbnail)
            .unwrap()
            .unwrap();
    let second_proxy =
        RepresentationClaim::try_acquire(&root, "asset-2", RepresentationKind::Proxy)
            .unwrap()
            .unwrap();
    assert_eq!(first_proxy.asset_id(), "asset-1");
    assert_eq!(first_thumbnail.kind(), RepresentationKind::Thumbnail);
    assert_eq!(second_proxy.asset_id(), "asset-2");
}

#[test]
fn catalog_handles_and_single_writers_report_typed_busy() {
    let (_temp, first, _project_id, source) = catalog_with_asset();
    let asset = first.get_asset("asset_claim").unwrap();
    first
        .store_video_validation("asset_claim", &asset.fingerprint, "usable", None)
        .unwrap();
    let second = Catalog::open(first.database_path()).unwrap();

    let proxy = first
        .try_claim_representation("asset_claim", RepresentationKind::Proxy)
        .unwrap()
        .unwrap();
    assert!(
        second
            .try_claim_representation("asset_claim", RepresentationKind::Proxy)
            .unwrap()
            .is_none()
    );
    let proxy_error = second.generate_proxy("asset_claim").unwrap_err();
    assert!(proxy_error.downcast_ref::<RepresentationBusy>().is_some());
    drop(proxy);

    let thumbnail = first
        .try_claim_representation("asset_claim", RepresentationKind::Thumbnail)
        .unwrap()
        .unwrap();
    let thumbnail_error = second.generate_thumbnail("asset_claim", 1).unwrap_err();
    assert!(
        thumbnail_error
            .downcast_ref::<RepresentationBusy>()
            .is_some()
    );
    drop(thumbnail);
    assert!(source.is_file());
}

#[test]
fn busy_batch_items_are_skipped_without_failures_or_rate_samples() {
    let (_temp, catalog, project_id, source) = catalog_with_asset();
    let asset = catalog.get_asset("asset_claim").unwrap();
    catalog
        .store_video_validation("asset_claim", &asset.fingerprint, "usable", None)
        .unwrap();
    let asset_ids = vec!["asset_claim".to_string()];

    let thumb_history_before = catalog.load_batch_history(THUMBNAIL_TASK_KIND).unwrap();
    let _thumbnail_claim = catalog
        .try_claim_representation("asset_claim", RepresentationKind::Thumbnail)
        .unwrap()
        .unwrap();
    let mut thumbnail_progress = Vec::new();
    let thumbnails = catalog
        .generate_thumbnails_for_assets_with_progress(&asset_ids, 1, |event| {
            thumbnail_progress.push(event);
        })
        .unwrap();
    assert_eq!(thumbnails.skipped, 1);
    assert_eq!(thumbnails.generated, 0);
    assert!(thumbnails.failures.is_empty());
    assert_eq!(thumbnail_progress.len(), 2);
    assert_eq!(
        thumbnail_progress.last().unwrap().estimate.completed_items,
        1
    );
    assert_eq!(
        catalog.load_batch_history(THUMBNAIL_TASK_KIND).unwrap(),
        thumb_history_before
    );

    let proxy_task = crate::proxy_task_kind(1);
    let proxy_history_before = catalog.load_batch_history(&proxy_task).unwrap();
    let _proxy_claim = catalog
        .try_claim_representation("asset_claim", RepresentationKind::Proxy)
        .unwrap()
        .unwrap();
    let mut proxy_progress = Vec::new();
    let proxies = catalog
        .generate_missing_proxies_with_progress(&project_id, |event| {
            proxy_progress.push(event);
            ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(proxies.skipped, 1);
    assert_eq!(proxies.generated, 0);
    assert!(proxies.failures.is_empty());
    assert_eq!(proxy_progress.len(), 2);
    assert_eq!(proxy_progress.last().unwrap().estimate.completed_items, 1);
    assert_eq!(
        catalog.load_batch_history(&proxy_task).unwrap(),
        proxy_history_before
    );
    assert!(source.is_file());
}

#[test]
fn proxy_length_mismatch_preserves_previous_output_and_releases_claim() {
    let (_temp, catalog, _project_id, _source) = catalog_with_asset();
    let write = catalog.begin_proxy_write("asset_claim").unwrap().unwrap();
    let staging = write.temporary_path().to_path_buf();
    let output = write.output.clone();
    fs::write(&output, b"old proxy").unwrap();
    set_proxy_path(&catalog, "asset_claim", &output);
    let old_registration = catalog.get_asset("asset_claim").unwrap().proxy_path;
    fs::write(&staging, b"new bytes").unwrap();

    let error = catalog.commit_proxy_write(write, Some(999)).unwrap_err();
    assert!(format!("{error:#}").contains("expected 999"));
    assert_eq!(fs::read(&output).unwrap(), b"old proxy");
    assert_eq!(
        catalog.get_asset("asset_claim").unwrap().proxy_path,
        old_registration
    );
    assert!(!staging.exists());
    assert!(output.exists());
    assert!(
        catalog
            .try_claim_representation("asset_claim", RepresentationKind::Proxy)
            .unwrap()
            .is_some()
    );
}

#[test]
fn invalid_proxy_media_preserves_previous_output_and_releases_claim() {
    let (_temp, catalog, _project_id, _source) = catalog_with_asset();
    let write = catalog.begin_proxy_write("asset_claim").unwrap().unwrap();
    let staging = write.temporary_path().to_path_buf();
    let output = write.output.clone();
    fs::write(&output, b"old proxy").unwrap();
    set_proxy_path(&catalog, "asset_claim", &output);
    let old_registration = catalog.get_asset("asset_claim").unwrap().proxy_path;
    fs::write(&staging, b"not a media file").unwrap();

    assert!(catalog.commit_proxy_write(write, None).is_err());
    assert_eq!(fs::read(&output).unwrap(), b"old proxy");
    assert_eq!(
        catalog.get_asset("asset_claim").unwrap().proxy_path,
        old_registration
    );
    assert!(!staging.exists());
    assert!(
        catalog
            .try_claim_representation("asset_claim", RepresentationKind::Proxy)
            .unwrap()
            .is_some()
    );
}

#[cfg(unix)]
#[test]
fn stale_open_proxy_writer_cannot_modify_the_next_staging_file() {
    let (_temp, catalog, _project_id, _source) = catalog_with_asset();
    let first = catalog.begin_proxy_write("asset_claim").unwrap().unwrap();
    let stale_path = first.temporary_path().to_path_buf();
    let mut stale_writer = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&stale_path)
        .unwrap();
    drop(first);

    let second = catalog.begin_proxy_write("asset_claim").unwrap().unwrap();
    let second_path = second.temporary_path().to_path_buf();
    assert_ne!(stale_path, second_path);
    stale_writer.write_all(b"late stale bytes").unwrap();
    stale_writer.flush().unwrap();
    assert!(!second_path.exists());
}

#[test]
fn claims_reject_wrong_catalog_and_kind_and_read_only_catalogs() {
    let (_temp, catalog, _project_id, _source) = catalog_with_asset();
    let second_dir = TempDir::new().unwrap();
    let other = Catalog::open(second_dir.path().join("other.sqlite3")).unwrap();
    let thumbnail = catalog
        .try_claim_representation("asset_claim", RepresentationKind::Thumbnail)
        .unwrap()
        .unwrap();
    assert!(other.generate_thumbnail_with_claim(&thumbnail, 1).is_err());

    let proxy = catalog
        .try_claim_representation("asset_claim", RepresentationKind::Proxy)
        .unwrap()
        .unwrap();
    assert!(catalog.generate_thumbnail_with_claim(&proxy, 1).is_err());

    let read_only_dir = TempDir::new().unwrap();
    let read_only_path = read_only_dir.path().join("read-only.sqlite3");
    drop(Catalog::open(&read_only_path).unwrap());
    let read_only = Catalog::open_read_only_existing(&read_only_path).unwrap();
    assert!(
        read_only
            .try_claim_representation("asset_claim", RepresentationKind::Proxy)
            .is_err()
    );
    let lock_dir = read_only_path
        .parent()
        .unwrap()
        .join("cache")
        .join(LOCK_DIRECTORY);
    assert!(!lock_dir.exists());
}

#[test]
fn proxy_write_commits_valid_media_and_registers_the_output() {
    let (_temp, catalog, _project_id, source) = catalog_with_asset();
    if !make_one_second_video(&source) {
        eprintln!("ffmpeg is unavailable; skipping proxy commit media test");
        return;
    }
    // Keep the fixture row's content fingerprint aligned after generating
    // its media bytes, as the claim commit validates only the output.
    let size = fs::metadata(&source).unwrap().len();
    let fingerprint = sampled_fingerprint(&source, size).unwrap();
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET fingerprint = ?2, size_bytes = ?3 WHERE id = ?1",
            rusqlite::params!["asset_claim", fingerprint, size as i64],
        )
        .unwrap();
    let write = catalog.begin_proxy_write("asset_claim").unwrap().unwrap();
    let output = write.output.clone();
    fs::copy(&source, write.temporary_path()).unwrap();

    let mut blocker = catalog.connection().unwrap();
    let transaction = blocker
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let writer_catalog = catalog.clone();
    let commit = thread::spawn(move || writer_catalog.commit_proxy_write(write, Some(size)));
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !output.is_file() && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        output.is_file(),
        "proxy should publish before catalog registration"
    );
    assert!(
        catalog
            .try_claim_representation("asset_claim", RepresentationKind::Proxy)
            .unwrap()
            .is_none()
    );
    transaction.commit().unwrap();
    let asset = commit.join().unwrap().unwrap();
    let output = PathBuf::from(asset.proxy_path.unwrap());
    assert!(output.is_file());
    assert!(asset.proxy_available);
    assert!(
        output
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".v3.mp4"))
    );
    assert!(
        catalog
            .try_claim_representation("asset_claim", RepresentationKind::Proxy)
            .unwrap()
            .is_some()
    );
}

#[test]
fn claims_release_when_returning_error_or_unwinding() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("cache");
    let fail_after_acquire = || -> Result<()> {
        let _claim = RepresentationClaim::try_acquire(&root, "asset", RepresentationKind::Proxy)?
            .expect("first acquisition succeeds");
        anyhow::bail!("synthetic writer error")
    };
    assert!(fail_after_acquire().is_err());
    assert!(
        RepresentationClaim::try_acquire(&root, "asset", RepresentationKind::Proxy)
            .unwrap()
            .is_some()
    );

    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _claim =
            RepresentationClaim::try_acquire(&root, "asset-panic", RepresentationKind::Proxy)
                .unwrap()
                .unwrap();
        panic!("synthetic unwind while claim held");
    }));
    assert!(unwind.is_err());
    assert!(
        RepresentationClaim::try_acquire(&root, "asset-panic", RepresentationKind::Proxy)
            .unwrap()
            .is_some()
    );
}

#[cfg(unix)]
#[test]
fn cross_process_contention_and_killed_holder_release_the_claim() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("cache");
    fs::create_dir_all(&root).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "representation::tests::claim_process_fixture",
            "--nocapture",
        ])
        .env("RCDVLOG_REPRESENTATION_CLAIM_ROOT", &root)
        .env("RCDVLOG_REPRESENTATION_CLAIM_ASSET", "cross-process-asset")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let handshake = loop {
        match receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(line)) if line == "CLAIMED" => break Ok(()),
            Ok(Ok(_)) => continue,
            Ok(Err(error)) => break Err(format!("read child handshake: {error}")),
            Err(error) => break Err(format!("timed out waiting for child claim: {error}")),
        }
    };
    if let Err(error) = handshake {
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
        panic!("{error}");
    }

    assert!(
        RepresentationClaim::try_acquire(&root, "cross-process-asset", RepresentationKind::Proxy)
            .unwrap()
            .is_none()
    );
    child.kill().unwrap();
    let _ = child.wait().unwrap();
    reader.join().unwrap();

    assert!(
        RepresentationClaim::try_acquire(&root, "cross-process-asset", RepresentationKind::Proxy)
            .unwrap()
            .is_some()
    );
}

#[cfg(unix)]
#[test]
fn claim_process_fixture() {
    let Ok(root) = std::env::var("RCDVLOG_REPRESENTATION_CLAIM_ROOT") else {
        return;
    };
    let asset_id = std::env::var("RCDVLOG_REPRESENTATION_CLAIM_ASSET").unwrap();
    let _claim =
        RepresentationClaim::try_acquire(Path::new(&root), &asset_id, RepresentationKind::Proxy)
            .unwrap()
            .expect("fixture process should acquire a free claim");
    println!("CLAIMED");
    std::io::stdout().flush().unwrap();
    loop {
        thread::park_timeout(Duration::from_secs(30));
    }
}
