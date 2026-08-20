//! Committing original bytes into an archive root.
//!
//! This is the one place in the application that writes original media, and it
//! is deliberately the most conservative code in the repository. `AGENTS.md` §2
//! is amended only far enough to permit *copying from a removable card into a
//! configured archive root* — the card is read-only throughout, and nothing here
//! renames, overwrites, truncates, or deletes anything, on the card or under the
//! archive root.
//!
//! Three rules, each load-bearing:
//!
//! - **Create-only.** An occupied target is a classification result
//!   (`PathCollision`), never a collision to resolve by clobbering. Archived
//!   originals are immutable once written.
//! - **Atomic.** Temp file in the destination directory, fsync, rename. A
//!   partial file is never visible under its final name, so an interrupted
//!   import leaves a card half-copied but never a truncated clip masquerading as
//!   a whole one.
//! - **Verified.** The full-file BLAKE3 is computed while writing and again from
//!   the committed file. The sampled fingerprint used for identity reads only
//!   the first and last 1 MiB and would not notice a corrupted middle, so it is
//!   not allowed to stand in for this.
//!
//! Resume needs no ledger. Because commit is atomic per file, a file either
//! committed or did not, and re-running an import re-classifies and copies only
//! what is missing.
//!
//! Committing is not the same as *registering*, though, and the gap between
//! them is real: the caller registers the asset after this module returns, so
//! an interruption in between leaves verified bytes under the final name that
//! the catalog cannot find by fingerprint. That is why resume compares content
//! at an occupied target instead of reading mere pathname occupancy as a
//! collision — a copy this code already committed is adopted, not refused.

use anyhow::{Context, Result, bail};
use blake3::Hasher;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Bytes moved per read/write cycle. Large enough to keep a spinning NAS
/// streaming, small enough that cancellation stays responsive.
const COPY_CHUNK: usize = 4 * 1024 * 1024;

/// A file safely committed into an archive root.
#[derive(Debug, Clone)]
pub(crate) struct CommittedCopy {
    pub path: PathBuf,
    pub size_bytes: u64,
}

/// Copies one original into the archive, atomically and verified.
///
/// `on_bytes` reports cumulative I/O bytes for this file. With verification on,
/// the total reaches twice the file size: one pass writes and one pass re-reads.
/// Returning `false` cancels during either pass: the temp file is removed and
/// nothing is committed.
pub(crate) fn commit_into_archive<F>(
    source: &Path,
    target: &Path,
    verify: bool,
    mut on_bytes: F,
) -> Result<Option<CommittedCopy>>
where
    F: FnMut(u64) -> bool,
{
    if target.exists() {
        // Never overwrite. Preflight classifies this as a collision; reaching
        // here means the archive changed under us, which is still not licence to
        // destroy what is already there.
        bail!(
            "{} already exists; archived originals are never overwritten",
            target.display()
        );
    }
    let parent = target
        .parent()
        .with_context(|| format!("archive target has no directory: {}", target.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create archive directory {}", parent.display()))?;

    // The temp file lives in the destination directory so the rename is a
    // same-filesystem metadata operation and therefore atomic.
    let temp = parent.join(format!(
        ".rcdvlog-incoming-{}",
        uuid::Uuid::new_v4().simple()
    ));

    let outcome = write_and_verify(source, &temp, target, verify, &mut on_bytes);
    if outcome.is_err() || matches!(outcome, Ok(None)) {
        // A failed or cancelled copy leaves only a temp path we own.
        let _ = fs::remove_file(&temp);
    }
    outcome
}

fn write_and_verify<F>(
    source: &Path,
    temp: &Path,
    target: &Path,
    verify: bool,
    on_bytes: &mut F,
) -> Result<Option<CommittedCopy>>
where
    F: FnMut(u64) -> bool,
{
    let mut reader =
        File::open(source).with_context(|| format!("read original {}", source.display()))?;
    let mut writer =
        File::create(temp).with_context(|| format!("write archive temp {}", temp.display()))?;

    let mut hasher = Hasher::new();
    let mut buffer = vec![0u8; COPY_CHUNK];
    let mut size_bytes: u64 = 0;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        size_bytes += read as u64;
        if !on_bytes(size_bytes) {
            return Ok(None);
        }
    }
    // Durability before the rename: a rename that lands ahead of the data would
    // publish a name pointing at nothing after a crash.
    writer.flush()?;
    writer.sync_all()?;
    drop(writer);

    let written = hasher.finalize().to_hex().to_string();
    if verify {
        let Some(observed) = hash_file_with_progress(temp, size_bytes, on_bytes)? else {
            return Ok(None);
        };
        if observed != written {
            bail!(
                "verification failed for {} — the copy does not match what was read",
                source.display()
            );
        }
    }

    fs::rename(temp, target)
        .with_context(|| format!("commit archive file {}", target.display()))?;
    // fsync the directory so the new name itself survives a crash.
    if let Some(parent) = target.parent()
        && let Ok(directory) = File::open(parent)
    {
        let _ = directory.sync_all();
    }

    Ok(Some(CommittedCopy {
        path: target.to_path_buf(),
        size_bytes,
    }))
}

/// Full-file BLAKE3. Distinct from the sampled fingerprint used for identity,
/// which reads only the ends of a file and cannot detect a corrupted middle.
#[cfg(test)]
fn hash_file(path: &Path) -> Result<String> {
    hash_file_with_progress(path, 0, &mut |_| true)?.context("hash callback unexpectedly cancelled")
}

fn hash_file_with_progress<F>(
    path: &Path,
    progress_offset: u64,
    on_bytes: &mut F,
) -> Result<Option<String>>
where
    F: FnMut(u64) -> bool,
{
    let mut file = File::open(path)?;
    let mut hasher = Hasher::new();
    let mut buffer = vec![0u8; COPY_CHUNK];
    let mut bytes_read = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes_read += read as u64;
        if !on_bytes(progress_offset.saturating_add(bytes_read)) {
            return Ok(None);
        }
    }
    Ok(Some(hasher.finalize().to_hex().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn a_copy_lands_atomically_and_verifies() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("C0001.MP4");
        fs::write(&source, b"original camera bytes").unwrap();
        let target = directory.path().join("archive/trip/CARD-1/C0001.MP4");

        let committed = commit_into_archive(&source, &target, true, |_| true)
            .unwrap()
            .unwrap();

        assert_eq!(committed.size_bytes, 21);
        assert_eq!(fs::read(&target).unwrap(), b"original camera bytes");
        // Compare the bytes on disk, not a hash the copy reported about itself.
        assert_eq!(hash_file(&target).unwrap(), hash_file(&source).unwrap());
        assert!(fs::read(&source).is_ok(), "the card must be left untouched");
    }

    #[test]
    fn an_occupied_target_is_never_overwritten() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("C0001.MP4");
        fs::write(&source, b"new footage").unwrap();
        let target = directory.path().join("archive/C0001.MP4");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"footage already archived").unwrap();

        let error = commit_into_archive(&source, &target, true, |_| true).unwrap_err();
        assert!(format!("{error:#}").contains("never overwritten"));
        assert_eq!(
            fs::read(&target).unwrap(),
            b"footage already archived",
            "the archived original must survive untouched"
        );
    }

    /// An interrupted import must leave a card half-copied, never a truncated
    /// clip wearing a real filename.
    #[test]
    fn a_cancelled_copy_commits_nothing_and_leaves_no_debris() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("C0001.MP4");
        fs::write(&source, vec![7u8; COPY_CHUNK * 2 + 16]).unwrap();
        let target = directory.path().join("archive/C0001.MP4");

        let mut seen = 0;
        let committed = commit_into_archive(&source, &target, true, |_| {
            seen += 1;
            // Cancel after the first chunk has been written.
            seen < 2
        })
        .unwrap();

        assert!(committed.is_none());
        assert!(!target.exists(), "a cancelled copy must not appear");
        let debris: Vec<_> = fs::read_dir(target.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert!(debris.is_empty(), "the temp file must be cleaned up");
    }

    #[test]
    fn a_large_file_round_trips_across_chunk_boundaries() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("C0001.MP4");
        let bytes: Vec<u8> = (0..COPY_CHUNK * 2 + 1234)
            .map(|index| (index % 251) as u8)
            .collect();
        fs::write(&source, &bytes).unwrap();
        let target = directory.path().join("archive/C0001.MP4");

        let committed = commit_into_archive(&source, &target, true, |_| true)
            .unwrap()
            .unwrap();
        assert_eq!(committed.size_bytes, bytes.len() as u64);
        assert_eq!(fs::read(&target).unwrap(), bytes);
    }

    #[test]
    fn progress_reports_bytes_as_they_land() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("C0001.MP4");
        fs::write(&source, vec![1u8; COPY_CHUNK + 10]).unwrap();
        let target = directory.path().join("archive/C0001.MP4");

        let mut reports = Vec::new();
        commit_into_archive(&source, &target, false, |bytes| {
            reports.push(bytes);
            true
        })
        .unwrap();

        assert!(reports.len() >= 2);
        assert_eq!(*reports.last().unwrap(), (COPY_CHUNK + 10) as u64);
        assert!(
            reports.windows(2).all(|pair| pair[0] < pair[1]),
            "progress must be monotonic"
        );
    }
}
