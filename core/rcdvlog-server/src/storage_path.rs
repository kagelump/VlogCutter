use rcdvlog_core::{RootRelativePath, StorageAccessMode};
use serde::Serialize;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathAccessIntent {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathResolutionErrorReason {
    MountMustBeAbsolute,
    MountContainsTraversal,
    MountIsNotUnicode,
    LogicalPathOutsideBinding,
    MountUnavailable,
    CandidateUnavailable,
    SymlinkEscape,
    ImmutableOriginal,
    BindingReadOnly,
}

#[derive(Debug, Error, Serialize)]
#[error("cannot resolve storage path: {reason}")]
#[serde(rename_all = "camelCase")]
pub struct PathResolutionError {
    pub code: &'static str,
    pub reason: PathResolutionErrorReason,
}

impl std::fmt::Display for PathResolutionErrorReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::MountMustBeAbsolute => "binding mount path must be absolute",
            Self::MountContainsTraversal => {
                "binding mount path must not contain dot or parent traversal"
            }
            Self::MountIsNotUnicode => "binding mount path must be valid Unicode",
            Self::LogicalPathOutsideBinding => "logical path is outside the binding rootSubpath",
            Self::MountUnavailable => "binding mount is unavailable",
            Self::CandidateUnavailable => "resolved candidate is unavailable",
            Self::SymlinkEscape => "resolved candidate escapes the binding mount",
            Self::ImmutableOriginal => "immutable original roots cannot be opened for writing",
            Self::BindingReadOnly => "read-only bindings cannot be opened for writing",
        })
    }
}

impl PathResolutionError {
    fn new(reason: PathResolutionErrorReason) -> Self {
        Self {
            code: "unsafe_storage_path",
            reason,
        }
    }
}

pub fn validate_local_mount_path(path: &Path) -> Result<(), PathResolutionError> {
    if !path.is_absolute() {
        return Err(PathResolutionError::new(
            PathResolutionErrorReason::MountMustBeAbsolute,
        ));
    }
    if path.to_str().is_none() {
        return Err(PathResolutionError::new(
            PathResolutionErrorReason::MountIsNotUnicode,
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(PathResolutionError::new(
            PathResolutionErrorReason::MountContainsTraversal,
        ));
    }
    Ok(())
}

/// Resolves an existing logical object through a node-local binding.
///
/// Canonicalization makes this intentionally unsuitable for creating files.
/// Generated outputs use a different, Task 6-owned store.
pub fn resolve_existing_binding_path(
    local_mount_path: &Path,
    root_subpath: Option<&RootRelativePath>,
    logical_path: &RootRelativePath,
    immutable_original: bool,
    access_mode: StorageAccessMode,
    intent: PathAccessIntent,
) -> Result<PathBuf, PathResolutionError> {
    validate_local_mount_path(local_mount_path)?;
    if intent == PathAccessIntent::Write {
        if immutable_original {
            return Err(PathResolutionError::new(
                PathResolutionErrorReason::ImmutableOriginal,
            ));
        }
        if access_mode == StorageAccessMode::ReadOnly {
            return Err(PathResolutionError::new(
                PathResolutionErrorReason::BindingReadOnly,
            ));
        }
    }

    let suffix = logical_path.suffix_after(root_subpath).ok_or_else(|| {
        PathResolutionError::new(PathResolutionErrorReason::LogicalPathOutsideBinding)
    })?;
    let canonical_mount = local_mount_path
        .canonicalize()
        .map_err(|_| PathResolutionError::new(PathResolutionErrorReason::MountUnavailable))?;
    let candidate = if suffix.is_empty() {
        local_mount_path.to_path_buf()
    } else {
        suffix
            .split('/')
            .fold(local_mount_path.to_path_buf(), |path, part| path.join(part))
    };
    let canonical_candidate = candidate
        .canonicalize()
        .map_err(|_| PathResolutionError::new(PathResolutionErrorReason::CandidateUnavailable))?;
    if !canonical_candidate.starts_with(&canonical_mount) {
        return Err(PathResolutionError::new(
            PathResolutionErrorReason::SymlinkEscape,
        ));
    }
    Ok(canonical_candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn full_and_partial_bindings_resolve_spaces_and_unicode() {
        let root = TempDir::new().unwrap();
        let partial_mount = root.path().join("mounted subtree");
        fs::create_dir_all(partial_mount.join("日本")).unwrap();
        fs::write(partial_mount.join("日本/clip 01.MP4"), b"fixture").unwrap();
        let logical = RootRelativePath::parse("Trips/2026/日本/clip 01.MP4").unwrap();
        let prefix = RootRelativePath::parse("Trips/2026").unwrap();
        let resolved = resolve_existing_binding_path(
            &partial_mount,
            Some(&prefix),
            &logical,
            true,
            StorageAccessMode::ReadOnly,
            PathAccessIntent::Read,
        )
        .unwrap();
        assert_eq!(
            resolved,
            partial_mount
                .join("日本/clip 01.MP4")
                .canonicalize()
                .unwrap()
        );

        let outside = RootRelativePath::parse("Trips/2025/clip.MP4").unwrap();
        assert_eq!(
            resolve_existing_binding_path(
                &partial_mount,
                Some(&prefix),
                &outside,
                true,
                StorageAccessMode::ReadOnly,
                PathAccessIntent::Read,
            )
            .unwrap_err()
            .reason,
            PathResolutionErrorReason::LogicalPathOutsideBinding
        );
    }

    #[cfg(unix)]
    #[test]
    fn canonical_resolution_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        let mount = root.path().join("mount");
        let outside = root.path().join("outside");
        fs::create_dir_all(&mount).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("clip.mp4"), b"outside").unwrap();
        symlink(&outside, mount.join("escape")).unwrap();
        let logical = RootRelativePath::parse("escape/clip.mp4").unwrap();
        assert_eq!(
            resolve_existing_binding_path(
                &mount,
                None,
                &logical,
                true,
                StorageAccessMode::ReadOnly,
                PathAccessIntent::Read,
            )
            .unwrap_err()
            .reason,
            PathResolutionErrorReason::SymlinkEscape
        );
    }

    #[test]
    fn immutable_and_read_only_roots_reject_write_intent_before_io() {
        let path = RootRelativePath::parse("clip.mp4").unwrap();
        let immutable = resolve_existing_binding_path(
            Path::new("/definitely/not/read"),
            None,
            &path,
            true,
            StorageAccessMode::ReadWrite,
            PathAccessIntent::Write,
        )
        .unwrap_err();
        assert_eq!(
            immutable.reason,
            PathResolutionErrorReason::ImmutableOriginal
        );

        let read_only = resolve_existing_binding_path(
            Path::new("/definitely/not/read"),
            None,
            &path,
            false,
            StorageAccessMode::ReadOnly,
            PathAccessIntent::Write,
        )
        .unwrap_err();
        assert_eq!(read_only.reason, PathResolutionErrorReason::BindingReadOnly);
    }

    #[cfg(unix)]
    #[test]
    fn filesystem_read_only_original_still_resolves_for_read_without_domain_writes() {
        use std::os::unix::fs::PermissionsExt;

        let root = TempDir::new().unwrap();
        let mount = root.path().join("read-only-original");
        fs::create_dir_all(&mount).unwrap();
        let file = mount.join("clip.mp4");
        fs::write(&file, b"original").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&mount, fs::Permissions::from_mode(0o555)).unwrap();

        let logical = RootRelativePath::parse("clip.mp4").unwrap();
        assert_eq!(
            resolve_existing_binding_path(
                &mount,
                None,
                &logical,
                true,
                StorageAccessMode::ReadOnly,
                PathAccessIntent::Read,
            )
            .unwrap(),
            file.canonicalize().unwrap()
        );
        assert_eq!(
            resolve_existing_binding_path(
                &mount,
                None,
                &logical,
                true,
                StorageAccessMode::ReadOnly,
                PathAccessIntent::Write,
            )
            .unwrap_err()
            .reason,
            PathResolutionErrorReason::ImmutableOriginal
        );

        fs::set_permissions(&mount, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    }
}
