//! Transient filesystem claims for generated media representations.
//!
//! SQLite remains the durable source of truth. These OS-managed locks only
//! coordinate simultaneous attempts to create the same cache representation.

use crate::{
    Asset, Catalog, PROXY_CACHE_VERSION, ProxyGenerationOutcome,
    insert_proxy_generation_provenance, probe_media_stream,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use fs2::FileExt;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

const LOCK_DIRECTORY: &str = ".representation-locks";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepresentationKind {
    Proxy,
    Thumbnail,
    Moments,
    /// Exclusive ownership of one asset's local speech write/result window.
    /// Local transcription and a prepared native-cloud request share this
    /// claim so one path cannot replace the other's result while it is active.
    Speech,
}

impl RepresentationKind {
    fn stable_name(self) -> &'static str {
        match self {
            Self::Proxy => "proxy",
            Self::Thumbnail => "thumbnail",
            Self::Moments => "moments",
            Self::Speech => "speech",
        }
    }
}

impl fmt::Display for RepresentationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.stable_name())
    }
}

/// Stable domain error for a request that cannot wait for an existing claim.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{kind} representation for asset {asset_id} is already being generated")]
pub struct RepresentationBusy {
    pub asset_id: String,
    pub kind: RepresentationKind,
}

impl RepresentationBusy {
    pub const CODE: &'static str = "representation_busy";
}

/// A cross-process claim held for the lifetime of the guard.
///
/// Lockfiles intentionally persist after unlock. Removing one can split
/// ownership between processes that opened different inodes at the same path.
#[derive(Debug)]
pub struct RepresentationClaim {
    _file: File,
    cache_root: PathBuf,
    asset_id: String,
    kind: RepresentationKind,
}

impl RepresentationClaim {
    /// Try to own one cache representation. Contention is `Ok(None)`; I/O
    /// errors remain actionable failures rather than looking like a busy item.
    pub(crate) fn try_acquire(
        cache_root: &Path,
        asset_id: &str,
        kind: RepresentationKind,
    ) -> Result<Option<Self>> {
        fs::create_dir_all(cache_root).with_context(|| {
            format!("create representation cache root {}", cache_root.display())
        })?;
        let cache_root = fs::canonicalize(cache_root).with_context(|| {
            format!("resolve representation cache root {}", cache_root.display())
        })?;
        let lock_directory = cache_root.join(LOCK_DIRECTORY);
        fs::create_dir_all(&lock_directory).with_context(|| {
            format!(
                "create representation lock directory {}",
                lock_directory.display()
            )
        })?;

        let path = lock_directory.join(lock_filename(asset_id, kind));
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .with_context(|| format!("open representation claim {}", path.display()))?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self {
                _file: file,
                cache_root,
                asset_id: asset_id.to_owned(),
                kind,
            })),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error)
                .with_context(|| format!("acquire representation claim {}", path.display())),
        }
    }

    pub(crate) fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    pub fn asset_id(&self) -> &str {
        &self.asset_id
    }

    pub(crate) fn kind(&self) -> RepresentationKind {
        self.kind
    }
}

/// Owned staging transaction shared by generation and authenticated downloads.
/// Its unique staging path also isolates writes still draining after cancellation.
#[derive(Debug)]
pub struct ProxyWrite {
    claim: RepresentationClaim,
    temporary: PathBuf,
    output: PathBuf,
}

impl ProxyWrite {
    pub fn temporary_path(&self) -> &Path {
        &self.temporary
    }
}

impl Drop for ProxyWrite {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.temporary);
    }
}

pub(crate) struct AnalysisProxySource {
    pub(crate) proxy: PathBuf,
    pub(crate) duration_ms: u32,
    pub(crate) last_safe_sample_ms: u32,
    pub(crate) fingerprint: String,
    pub(crate) sampled_fingerprint: String,
}

impl Catalog {
    /// Validate the accepted analysis proxy and return its time mapping and
    /// fingerprints for consumers that need identical source acceptance.
    pub(crate) fn analysis_proxy_source(&self, asset: &Asset) -> Result<AnalysisProxySource> {
        ensure!(
            asset.duration_seconds.is_finite() && asset.duration_seconds > 0.0,
            "source has no finite positive duration"
        );
        ensure!(
            !asset.removed,
            "asset is explicitly excluded from preparation"
        );
        ensure!(
            self.asset_video_validation(&asset.id)?.status
                == crate::AssetVideoValidationStatus::Usable,
            "asset is unverified or quarantined; explicit video validation required"
        );
        ensure!(
            crate::has_current_proxy(asset),
            "a current validated v3 proxy is required for moment discovery"
        );
        let proxy = Self::require_proxy(asset)?;
        let document = crate::probe_media_document(&proxy)?;
        let stream = crate::require_video_stream(&document)?;
        ensure!(
            stream["codec_name"].as_str() == Some("h264"),
            "current v3 moment input must be an H.264 proxy"
        );
        ensure!(
            stream["width"]
                .as_u64()
                .is_some_and(|width| width > 0 && width <= 1280),
            "proxy dimensions do not match the current recipe"
        );
        ensure!(
            crate::stream_rotation(Some(stream)) == 0,
            "proxy display rotation must be zero"
        );
        let duration = crate::video_duration(&document, stream)
            .ok_or_else(|| anyhow!("proxy has no valid duration"))?;
        ensure!(
            duration * 1000.0 < u32::MAX as f64,
            "source duration exceeds supported time range"
        );
        ensure!(
            (duration - asset.duration_seconds).abs()
                <= 0.25_f64.max(asset.duration_seconds * 0.01),
            "proxy/source duration mismatch; compatible time mapping must be verified"
        );
        let duration_ms = (duration.min(asset.duration_seconds) * 1000.0).round() as u32;
        ensure!(duration_ms > 0, "source has no positive duration");
        let frame_rate = stream["r_frame_rate"]
            .as_str()
            .and_then(crate::parse_rate)
            .filter(|rate| rate.is_finite() && *rate > 0.0)
            .ok_or_else(|| anyhow!("proxy has no finite positive frame rate"))?;
        let tail_ms = ((1000.0 / frame_rate).ceil().min(u32::MAX as f64) as u32).saturating_add(1);
        let fingerprint = crate::full_file_blake3(&proxy, &|| false)?.hex_digest;
        let sampled_fingerprint = self.proxy_fingerprint(&asset.id)?;
        Ok(AnalysisProxySource {
            proxy,
            duration_ms,
            last_safe_sample_ms: duration_ms.saturating_sub(tail_ms),
            fingerprint,
            sampled_fingerprint,
        })
    }

    fn representation_cache_root(&self) -> Result<PathBuf> {
        Ok(self
            .database_path
            .parent()
            .ok_or_else(|| anyhow!("catalog has no parent directory"))?
            .join("cache"))
    }

    /// Reserve before scheduling a writer. Busy work is skipped, never queued.
    pub fn try_claim_representation(
        &self,
        asset_id: &str,
        kind: RepresentationKind,
    ) -> Result<Option<RepresentationClaim>> {
        if self.read_only {
            bail!("cannot generate representations in a read-only catalog");
        }
        RepresentationClaim::try_acquire(&self.representation_cache_root()?, asset_id, kind)
    }

    pub(crate) fn validate_representation_claim(
        &self,
        claim: &RepresentationClaim,
        asset_id: &str,
        kind: RepresentationKind,
    ) -> Result<()> {
        if self.read_only
            || claim.asset_id() != asset_id
            || claim.kind() != kind
            || claim.cache_root() != fs::canonicalize(self.representation_cache_root()?)?
        {
            bail!("representation claim does not belong to this catalog, asset and kind");
        }
        Ok(())
    }

    pub(crate) fn claim_representation(
        &self,
        asset_id: &str,
        kind: RepresentationKind,
    ) -> Result<RepresentationClaim> {
        self.try_claim_representation(asset_id, kind)?
            .ok_or_else(|| {
                RepresentationBusy {
                    asset_id: asset_id.to_owned(),
                    kind,
                }
                .into()
            })
    }

    /// Begin a proxy transaction before HTTP requests, FFmpeg, or cache writes.
    pub fn begin_proxy_write(&self, asset_id: &str) -> Result<Option<ProxyWrite>> {
        let Some(claim) = self.try_claim_representation(asset_id, RepresentationKind::Proxy)?
        else {
            return Ok(None);
        };
        let asset = self.get_asset(asset_id)?;
        let directory = claim.cache_root().join("proxies").join(&asset.project_id);
        fs::create_dir_all(&directory)?;
        Ok(Some(ProxyWrite {
            temporary: directory.join(format!(
                "{}.{}.{}.tmp.mp4",
                asset.id,
                PROXY_CACHE_VERSION,
                uuid::Uuid::new_v4().simple()
            )),
            output: directory.join(format!("{}.{}.mp4", asset.id, PROXY_CACHE_VERSION)),
            claim,
        }))
    }

    /// Validate, publish, and register a strict generated proxy plus its
    /// provenance transactionally. The runner has already checked metadata,
    /// source-time correspondence, audio correspondence, orientation, and
    /// decode. This method re-checks that the staged file remains probeable
    /// before the output is published. If SQLite fails after publication the
    /// previous registered output is restored.
    pub(crate) fn commit_generated_proxy_write(
        &self,
        write: ProxyWrite,
        outcome: &ProxyGenerationOutcome,
    ) -> Result<Asset> {
        let asset_id = write.claim.asset_id().to_owned();
        self.validate_representation_claim(&write.claim, &asset_id, RepresentationKind::Proxy)?;
        if outcome.status != "succeeded" {
            bail!("refusing to register a proxy generation outcome that did not succeed");
        }
        let validation = outcome
            .validation
            .as_ref()
            .ok_or_else(|| anyhow!("refusing to register a proxy without validation evidence"))?;
        if !validation.decoded || validation.time_correspondence.is_none() {
            bail!("refusing to register a proxy whose decode validation did not pass");
        }
        let probe = probe_media_stream(&write.temporary).context("revalidate generated proxy")?;
        if probe.metadata_rotation_degrees != 0 {
            bail!(
                "ffmpeg proxy validation failed: output retains a {}° display rotation",
                probe.metadata_rotation_degrees
            );
        }
        if probe.codec != "h264" || probe.width == 0 || probe.width > 1280 {
            bail!(
                "generated proxy metadata is outside the v3 recipe: codec={}, width={}",
                probe.codec,
                probe.width
            );
        }

        let backup = if write.output.is_file() {
            let backup = write.output.with_file_name(format!(
                ".{}.commit-backup.{}",
                write
                    .output
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("proxy"),
                uuid::Uuid::new_v4().simple()
            ));
            // Keep the registered pathname readable until the atomic rename.
            fs::hard_link(&write.output, &backup).context("preserve previous registered proxy")?;
            Some(backup)
        } else {
            None
        };

        let mut published = false;
        let publish = (|| -> Result<()> {
            let mut connection = self.connection()?;
            let transaction = connection.transaction()?;
            insert_proxy_generation_provenance(&transaction, &asset_id, outcome)?;
            transaction.execute(
                "UPDATE assets SET proxy_path = ?2, proxy_generation_id = ?3 WHERE id = ?1",
                params![
                    asset_id,
                    write.output.to_string_lossy(),
                    transaction.last_insert_rowid()
                ],
            )?;
            fs::rename(&write.temporary, &write.output).context("commit generated proxy")?;
            published = true;
            transaction.commit().context("register generated proxy")?;
            Ok(())
        })();
        if let Err(error) = publish {
            if published {
                if let Some(backup) = &backup {
                    fs::rename(backup, &write.output)
                        .context("restore previous registered proxy")?;
                } else {
                    let _ = fs::remove_file(&write.output);
                }
            } else if let Some(backup) = &backup {
                let _ = fs::remove_file(backup);
            }
            return Err(error);
        }
        if let Some(backup) = backup {
            let _ = fs::remove_file(backup);
        }
        self.get_asset(&asset_id)
    }

    /// Validate, publish, and register while still holding exclusive ownership.
    /// Catalog connections apply the same WAL/foreign-key/busy-timeout policy.
    pub fn commit_proxy_write(
        &self,
        write: ProxyWrite,
        expected_length: Option<u64>,
    ) -> Result<Asset> {
        let asset_id = write.claim.asset_id();
        self.validate_representation_claim(&write.claim, asset_id, RepresentationKind::Proxy)?;
        let length = fs::metadata(&write.temporary)?.len();
        if let Some(expected) = expected_length
            && length != expected
        {
            bail!("downloaded {length} bytes, expected {expected}");
        }
        let probe = probe_media_stream(&write.temporary).context("validate generated proxy")?;
        if probe.metadata_rotation_degrees != 0 {
            bail!(
                "ffmpeg proxy validation failed: output retains a {}° display rotation",
                probe.metadata_rotation_degrees
            );
        }
        fs::rename(&write.temporary, &write.output).context("commit generated proxy")?;
        self.connection()?.execute(
            "UPDATE assets SET proxy_path = ?2, proxy_generation_id = NULL WHERE id = ?1",
            params![asset_id, write.output.to_string_lossy()],
        )?;
        self.get_asset(asset_id)
    }
}

impl Drop for RepresentationClaim {
    fn drop(&mut self) {
        // Closing the file also releases an advisory lock; explicit unlock is
        // useful here so release occurs before the guard's fields are dropped.
        let _ = FileExt::unlock(&self._file);
    }
}

fn lock_filename(asset_id: &str, kind: RepresentationKind) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rcdvlog-representation-claim-v1\0");
    hasher.update(kind.stable_name().as_bytes());
    hasher.update(b"\0");
    hasher.update(asset_id.as_bytes());
    format!("{}.lock", hasher.finalize().to_hex())
}

#[cfg(test)]
mod tests {
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
        let claim =
            RepresentationClaim::try_acquire(&root, "asset/../雪", RepresentationKind::Proxy)
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
        let first_proxy =
            RepresentationClaim::try_acquire(&root, "asset-1", RepresentationKind::Proxy)
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
            let _claim =
                RepresentationClaim::try_acquire(&root, "asset", RepresentationKind::Proxy)?
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
            RepresentationClaim::try_acquire(
                &root,
                "cross-process-asset",
                RepresentationKind::Proxy
            )
            .unwrap()
            .is_none()
        );
        child.kill().unwrap();
        let _ = child.wait().unwrap();
        reader.join().unwrap();

        assert!(
            RepresentationClaim::try_acquire(
                &root,
                "cross-process-asset",
                RepresentationKind::Proxy
            )
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
        let _claim = RepresentationClaim::try_acquire(
            Path::new(&root),
            &asset_id,
            RepresentationKind::Proxy,
        )
        .unwrap()
        .expect("fixture process should acquire a free claim");
        println!("CLAIMED");
        std::io::stdout().flush().unwrap();
        loop {
            thread::park_timeout(Duration::from_secs(30));
        }
    }
}
