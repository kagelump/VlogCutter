//! One owner for verified encoded preview work. Cache files are immutable;
//! SQLite advertises only a fully decoded, hashed object, never a partial file.
use super::preview_artifacts::{managed_child, project_directory};
use super::{EditPlanOperation, PreviewInputIdentity, preview_media::SegmentExpectation};
use crate::Catalog;
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::Duration,
};

const SEGMENT_RECIPE: &str = "preview.segment.h264-hardcut.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    version: u32,
    project_id: String,
    segment_key: String,
    object_name: String,
    expectation: SegmentExpectation,
    frame_count: u64,
    fingerprint: String,
    byte_length: u64,
    fully_decoded: bool,
    validated_at: String,
}

pub(super) struct CachedSegment {
    pub path: PathBuf,
    pub frame_count: u64,
}
pub(super) struct SegmentClaim {
    _lock: File,
    directory: PathBuf,
    pub key: String,
    project: String,
    expected: SegmentExpectation,
    pub invalidated: bool,
    pub cached: Option<CachedSegment>,
}

pub(super) fn key(
    project: &str,
    identity: &PreviewInputIdentity,
    cut: &EditPlanOperation,
    expected: &SegmentExpectation,
) -> Result<String> {
    let source = identity
        .sources
        .iter()
        .find(|s| s.asset_id == cut.asset_id)
        .context("preview segment selected representation is not identified")?;
    // Revision/cut IDs, rationale and position are not encoding inputs. In a
    // split plan the cumulative frame-grid allocation IS an input. The linked
    // recipe includes source sound; picture-only segments are remixed later.
    let tools = identity
        .tools
        .iter()
        .map(|tool| serde_json::json!({"role":tool.role,"name":tool.name,"version":tool.version}))
        .collect::<Vec<_>>();
    let input = serde_json::json!({"version":SEGMENT_RECIPE,"projectId":project,
        "source":source,"sourceInSeconds":cut.source_in_seconds,"sourceOutSeconds":cut.source_out_seconds,
        "expectation":expected,"previewRecipe":identity.recipe,"tools":tools,
        "toolContentFingerprints":identity.tool_content_fingerprints});
    Ok(blake3::hash(&serde_json::to_vec(&input)?)
        .to_hex()
        .to_string())
}

impl SegmentClaim {
    pub fn acquire(
        catalog: &Catalog,
        project: &str,
        key: String,
        expected: SegmentExpectation,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Self> {
        ensure!(
            key.len() == 64 && key.bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid preview segment key"
        );
        let cache = managed_child(&project_directory(catalog)?, "cache")?;
        let root = managed_child(&cache, "preview-segments")?;
        let project_root = managed_child(&root, project)?;
        let directory = managed_child(&project_root, &key)?;
        let lock_path = directory.join(".segment.lock");
        ensure!(
            !fs::symlink_metadata(&lock_path).is_ok_and(|m| m.file_type().is_symlink()),
            "preview segment claim cannot follow a symlink"
        );
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        loop {
            ensure!(
                !cancel(),
                "preview cancelled while waiting for a segment claim"
            );
            match fs2::FileExt::try_lock_exclusive(&file) {
                Ok(()) => break,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                Err(e) => return Err(e.into()),
            }
        }
        managed_child(&directory, "objects")?;
        let body: Option<String> = catalog
            .connection()?
            .query_row(
                "SELECT body_json FROM preview_segments WHERE segment_key=?1 AND project_id=?2",
                params![key, project],
                |r| r.get(0),
            )
            .optional()?;
        let mut claim = Self {
            _lock: file,
            directory,
            key,
            project: project.into(),
            expected,
            invalidated: body.is_some(),
            cached: None,
        };
        if let Some(body) = body {
            // A malformed/damaged matching entry is repairable work, not a
            // reason to trust its filename or discard earlier preview movies.
            let checked = (|| -> Result<CachedSegment> {
                ensure!(body.len() <= 16384, "preview segment receipt is oversized");
                let receipt: Receipt = serde_json::from_str(&body)?;
                ensure!(
                    receipt.version == 1
                        && receipt.project_id == project
                        && receipt.segment_key == claim.key
                        && receipt.expectation == claim.expected
                        && receipt.fully_decoded
                        && receipt.frame_count > 0
                        && receipt.frame_count <= 1_800_000,
                    "preview segment receipt identity mismatch"
                );
                let path = claim.object_path(&receipt.object_name)?;
                ensure!(
                    fs::symlink_metadata(&path)?.file_type().is_file(),
                    "cached preview segment is not a regular file"
                );
                let hash = crate::full_file_blake3(&path, cancel)?;
                ensure!(
                    hash.hex_digest == receipt.fingerprint
                        && hash.byte_count == receipt.byte_length,
                    "cached preview segment bytes changed"
                );
                Ok(CachedSegment {
                    path,
                    frame_count: receipt.frame_count,
                })
            })();
            if let Ok(cached) = checked {
                claim.invalidated = false;
                claim.cached = Some(cached);
            }
            ensure!(
                !cancel(),
                "preview cancelled while verifying cached segment"
            );
        }
        Ok(claim)
    }
    fn object_path(&self, name: &str) -> Result<PathBuf> {
        ensure!(
            name.starts_with("segment_")
                && name.ends_with(".mp4")
                && name.len() == 44
                && name[8..40].bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid preview cache object locator"
        );
        Ok(self.directory.join("objects").join(name))
    }
    /// Called only after the renderer's actual stream/duration/decode checks.
    pub fn publish(
        &self,
        catalog: &Catalog,
        encoded: &Path,
        frames: u64,
        cancel: &dyn Fn() -> bool,
    ) -> Result<CachedSegment> {
        ensure!(
            frames > 0 && self.expected.frames.is_none_or(|count| count == frames),
            "preview segment coverage mismatch"
        );
        let hash = crate::full_file_blake3(encoded, cancel)?;
        let name = format!("segment_{}.mp4", uuid::Uuid::new_v4().simple());
        let path = self.object_path(&name)?;
        ensure!(!path.exists(), "preview cache object collision");
        ensure!(!cancel(), "preview cancelled before segment publication");
        // Same project-local filesystem. An interrupted rename/database commit
        // can leave an unadvertised owned object, never a valid-looking row.
        fs::rename(encoded, &path)?;
        let receipt = Receipt {
            version: 1,
            project_id: self.project.clone(),
            segment_key: self.key.clone(),
            object_name: name,
            expectation: self.expected.clone(),
            frame_count: frames,
            fingerprint: hash.hex_digest,
            byte_length: hash.byte_count,
            fully_decoded: true,
            validated_at: crate::now(),
        };
        catalog.connection()?.execute("INSERT INTO preview_segments(segment_key,project_id,body_json) VALUES(?1,?2,?3)
            ON CONFLICT(segment_key) DO UPDATE SET project_id=excluded.project_id,body_json=excluded.body_json",
            params![self.key,self.project,serde_json::to_string(&receipt)?])?;
        Ok(CachedSegment {
            path,
            frame_count: frames,
        })
    }
}

#[cfg(test)]
#[path = "preview_segments_tests.rs"]
mod tests;
