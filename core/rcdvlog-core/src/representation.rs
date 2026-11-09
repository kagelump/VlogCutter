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
    OrientationReview,
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
            Self::OrientationReview => "orientation-review",
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
        self.analysis_proxy_source_with_cancellation(asset, &|| false)
    }

    pub(crate) fn analysis_proxy_source_with_cancellation(
        &self,
        asset: &Asset,
        cancel: &dyn Fn() -> bool,
    ) -> Result<AnalysisProxySource> {
        ensure!(
            !asset.removed,
            "asset is explicitly excluded from preparation"
        );
        self.validated_review_proxy_source(asset, cancel)
    }

    /// Explicit editing/review may address a source hidden from labeling.
    /// Keep preparation's removed filter separate from representation safety.
    pub(crate) fn validated_review_proxy_source(
        &self,
        asset: &Asset,
        cancel: &dyn Fn() -> bool,
    ) -> Result<AnalysisProxySource> {
        ensure!(
            asset.duration_seconds.is_finite() && asset.duration_seconds > 0.0,
            "source has no finite positive duration"
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
        let fingerprint = crate::full_file_blake3(&proxy, cancel)?.hex_digest;
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
#[path = "representation_tests.rs"]
mod tests;
