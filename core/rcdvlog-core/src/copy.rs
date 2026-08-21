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
//!   originals are immutable once written. This is enforced by the operation
//!   that publishes the file, not merely by an `exists()` check taken minutes
//!   earlier: copying a clip takes real time, and a target that appears during
//!   that window must survive too (#80).
//! - **Atomic.** Temp file in the destination directory, fsync, no-replace
//!   publication. A partial file is never visible under its final name, so an
//!   interrupted import leaves a card half-copied but never a truncated clip
//!   masquerading as a whole one.
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
use std::io::{ErrorKind, Read, Write};
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
        //
        // This is an early exit, not the guarantee: it saves copying a clip
        // that already has nowhere to land. The guarantee is `publish`, which
        // is create-only at the moment of publication and so also covers a
        // target that appears after this check (#80).
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

    match publish(temp, target)
        .with_context(|| format!("commit archive file {}", target.display()))?
    {
        Publication::Committed => {}
        Publication::TargetAppeared => {
            // Someone occupied the pathname while this clip was being copied.
            // Whatever is there now is not ours to judge, let alone replace;
            // the temp file is discarded by the caller and the card is
            // untouched, so a later run reclassifies the pathname and either
            // adopts identical content or reports a collision.
            bail!(
                "{} appeared while it was being copied; archived originals are never overwritten. \
                 Nothing was imported for this clip — re-run the import to reconcile it",
                target.display()
            );
        }
    }
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

/// What happened at the moment a finished temp file was given its final name.
#[derive(Debug, PartialEq, Eq)]
enum Publication {
    /// The final name did not exist and now names the copied bytes.
    Committed,
    /// The final name was occupied by the time we tried to publish. Whatever
    /// is there was left exactly as it was found.
    TargetAppeared,
}

/// Publishes a verified temp file under its final name, create-only.
///
/// A plain `rename` is not create-only: it replaces an existing destination.
/// The `exists()` check `commit_into_archive` starts with is minutes stale by
/// the time a large clip finishes copying, so publication has to carry the
/// invariant itself. Three tiers, best first, each of them no-clobber:
///
/// 1. The platform's no-replace rename (`renameat2(RENAME_NOREPLACE)`,
///    `renamex_np(RENAME_EXCL)`). One atomic operation that fails rather than
///    replace.
/// 2. A hard link, then unlinking the temp name. `link(2)` and
///    `CreateHardLinkW` refuse an existing destination, so the final name
///    still appears atomically and complete. A crash between the link and the
///    unlink leaves the correct bytes under the final name plus a temp file to
///    sweep up — never a wrong or partial target.
/// 3. Reserving the name with an exclusive create, then renaming the temp file
///    over the placeholder we now own. Last resort for filesystems with
///    neither of the above (FAT/exFAT external drives), where the alternative
///    would be to give up no-clobber entirely.
fn publish(temp: &Path, target: &Path) -> Result<Publication> {
    if let Some(result) = native_no_replace_rename(temp, target) {
        return match result {
            Ok(()) => Ok(Publication::Committed),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                Ok(Publication::TargetAppeared)
            }
            Err(error) => Err(error.into()),
        };
    }
    if let Some(publication) = publish_by_link(temp, target)? {
        return Ok(publication);
    }
    publish_by_reservation(temp, target)
}

/// Tier 2. `None` when this filesystem has no hard links to offer, which is
/// not a failure of the copy — the caller falls through to the next tier.
fn publish_by_link(temp: &Path, target: &Path) -> Result<Option<Publication>> {
    match fs::hard_link(temp, target) {
        Ok(()) => {
            // The final name is published. The temp name is now a second link
            // to the same bytes and is ours alone to remove.
            let _ = fs::remove_file(temp);
            Ok(Some(Publication::Committed))
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            Ok(Some(Publication::TargetAppeared))
        }
        Err(_) => Ok(None),
    }
}

/// Tier 3. An exclusive create is the one create-only primitive every
/// filesystem has. It reserves the pathname against any concurrent copy taking
/// this same route, and the rename that follows replaces only the empty
/// placeholder we just created.
fn publish_by_reservation(temp: &Path, target: &Path) -> Result<Publication> {
    match File::create_new(target) {
        // Dropped immediately: Windows will not rename over an open file.
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            return Ok(Publication::TargetAppeared);
        }
        Err(error) => return Err(error.into()),
    }
    if let Err(error) = fs::rename(temp, target) {
        // Leave no zero-byte file wearing a real clip's name.
        let _ = fs::remove_file(target);
        return Err(error.into());
    }
    Ok(Publication::Committed)
}

/// The platform's create-only rename, or `None` where there is not one to use.
///
/// `None` means "ask the next tier": either this target has no such call, or
/// the kernel or filesystem under this particular path does not implement it.
/// An occupied destination comes back as `Some(Err(AlreadyExists))`, which is
/// a race outcome rather than a fault.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn native_no_replace_rename(temp: &Path, target: &Path) -> Option<std::io::Result<()>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    // From `uapi/linux/fs.h`. Spelled out rather than imported so the build
    // does not depend on which libc version exposes the constant, and invoked
    // through `syscall` for the same reason.
    const RENAME_NOREPLACE: libc::c_uint = 1;

    let (Ok(from), Ok(to)) = (
        CString::new(temp.as_os_str().as_bytes()),
        CString::new(target.as_os_str().as_bytes()),
    ) else {
        // An interior NUL cannot name a file we just wrote; let the portable
        // tiers produce the ordinary error.
        return None;
    };
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        return Some(Ok(()));
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        // Old kernel, or a filesystem whose rename ignores flags. Not this
        // copy's failure.
        Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::EOPNOTSUPP) => None,
        _ => Some(Err(error)),
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn native_no_replace_rename(temp: &Path, target: &Path) -> Option<std::io::Result<()>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let (Ok(from), Ok(to)) = (
        CString::new(temp.as_os_str().as_bytes()),
        CString::new(target.as_os_str().as_bytes()),
    ) else {
        return None;
    };
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) } == 0 {
        return Some(Ok(()));
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        // Not every volume type implements it — an exFAT card reader will not.
        Some(libc::ENOTSUP) | Some(libc::EINVAL) | Some(libc::ENOSYS) => None,
        _ => Some(Err(error)),
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
fn native_no_replace_rename(_temp: &Path, _target: &Path) -> Option<std::io::Result<()>> {
    None
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

    /// #80. The `exists()` check at the top of a copy is stale by the time a
    /// real clip finishes: a target that appears in between must survive.
    ///
    /// The race is injected deterministically rather than raced for. Progress
    /// reports cumulative bytes across both passes, so a report of exactly
    /// twice the file size is the last verification chunk — after the temp
    /// file is written and hashed, before it is published.
    #[test]
    fn a_target_that_appears_during_the_copy_is_not_overwritten() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("C0001.MP4");
        fs::write(&source, b"new footage").unwrap();
        let target = directory.path().join("archive/C0001.MP4");

        let mut raced = false;
        let error = commit_into_archive(&source, &target, true, |bytes| {
            if bytes == 22 && !raced {
                // Another import commits this pathname while we were copying.
                fs::create_dir_all(target.parent().unwrap()).unwrap();
                fs::write(&target, b"someone else's footage").unwrap();
                raced = true;
            }
            true
        })
        .unwrap_err();

        assert!(raced, "the race was never injected");
        assert!(format!("{error:#}").contains("never overwritten"));
        assert_eq!(
            fs::read(&target).unwrap(),
            b"someone else's footage",
            "the file that won the race must keep its own bytes"
        );
        assert_eq!(
            fs::read(&source).unwrap(),
            b"new footage",
            "the card must be left untouched"
        );
        let debris: Vec<_> = fs::read_dir(target.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .filter(|name| name != "C0001.MP4")
            .collect();
        assert!(
            debris.is_empty(),
            "the temp file must be cleaned up when the commit loses the race: {debris:?}"
        );
    }

    /// Each publication tier is create-only on its own, because which one runs
    /// depends on the filesystem the archive happens to live on.
    #[test]
    fn every_publication_tier_refuses_an_occupied_name() {
        for publish_one in [
            &publish as &dyn Fn(&Path, &Path) -> Result<Publication>,
            &|temp, target| publish_by_link(temp, target).map(Option::unwrap),
            &publish_by_reservation,
        ] {
            let directory = tempdir().unwrap();
            let temp = directory.path().join(".rcdvlog-incoming-1");
            let target = directory.path().join("C0001.MP4");

            fs::write(&temp, b"copied bytes").unwrap();
            assert_eq!(publish_one(&temp, &target).unwrap(), Publication::Committed);
            assert_eq!(fs::read(&target).unwrap(), b"copied bytes");
            assert!(!temp.exists(), "the temp name must not linger");

            fs::write(&temp, b"later bytes").unwrap();
            assert_eq!(
                publish_one(&temp, &target).unwrap(),
                Publication::TargetAppeared
            );
            assert_eq!(
                fs::read(&target).unwrap(),
                b"copied bytes",
                "the occupant must survive publication"
            );
            assert_eq!(
                fs::read(&temp).unwrap(),
                b"later bytes",
                "a lost race must not consume the temp file it declined to publish"
            );
        }
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
