//! Archive roots: the named places imported originals are allowed to land.
//!
//! Import copies footage off a card because the card is the one storage location
//! guaranteed to disappear. That destination is itself movable — an external
//! drive remounts, a NAS share changes host, footage graduates from the laptop
//! to a shuttle drive to the NAS — so recording where a file went as an absolute
//! path would recreate exactly the offline defect this epic set out to fix, at
//! archive scale.
//!
//! So an archive location is **a logical root plus a relative path**. The root is
//! a name the user recognizes; the binding is where that root is mounted on this
//! machine right now. Moving the archive is one rebind, not a per-asset repair.
//!
//! Write discipline lives with the copy path; this module only names places and
//! resolves them. Nothing here renames, overwrites, or deletes.

use anyhow::{Context, Result, bail};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::storage::RootRelativePath;
use crate::{Catalog, now};

/// A destination the user has named, as it looks from this machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveRoot {
    pub id: String,
    /// What the user calls it: "NAS Video", "Shuttle SSD".
    pub display_name: String,
    /// Where it is mounted on this node, if it has ever been bound here.
    pub local_mount_path: Option<String>,
    /// Whether that mount point is a reachable directory right now. A root that
    /// is merely unplugged is not an error — it is a state the UI reports.
    pub available: bool,
    /// Free bytes at the mount point, when it is reachable. The import dialog
    /// refuses to start a copy that will not fit.
    pub available_bytes: Option<u64>,
    pub created_at: String,
}

impl Catalog {
    /// Names a new destination and binds it to where it is mounted now.
    ///
    /// Naming and binding land together or not at all: a root with no binding is
    /// a destination the user cannot see, cannot use, and would have to discover
    /// in order to delete.
    pub fn create_archive_root(
        &self,
        display_name: &str,
        mount_path: &Path,
    ) -> Result<ArchiveRoot> {
        let display_name = display_name.trim();
        if display_name.is_empty() {
            bail!("an archive root needs a name you will recognize later");
        }
        if !mount_path.is_dir() {
            bail!(
                "{} is not a reachable folder — mount the drive first",
                mount_path.display()
            );
        }
        let id = format!("archive_{}", Uuid::new_v4().simple());
        let node_id = self.node_id()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let created_at = now();
        transaction.execute(
            "INSERT INTO archive_roots(archive_root_id, display_name, created_at) VALUES (?1, ?2, ?3)",
            params![id, display_name, created_at],
        )?;
        transaction.execute(
            "INSERT INTO archive_root_bindings(archive_root_id, node_id, local_mount_path, bound_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![id, node_id, mount_path.to_string_lossy(), created_at],
        )?;
        transaction.commit()?;
        drop(connection);
        self.archive_root(&id)
    }

    /// Points an existing root at where it is mounted now.
    ///
    /// This is the entire repair when an archive moves: every asset stored under
    /// the root resolves through the binding, so one rebind reconnects all of
    /// them at once. Assets are re-verified lazily on next use rather than
    /// re-hashed here, because an archive can hold thousands of clips and the
    /// user is waiting on a dialog.
    pub fn bind_archive_root(
        &self,
        archive_root_id: &str,
        mount_path: &Path,
    ) -> Result<ArchiveRoot> {
        if !mount_path.is_dir() {
            bail!(
                "{} is not a reachable folder — mount the drive first",
                mount_path.display()
            );
        }
        let mount_text = mount_path.to_string_lossy().into_owned();
        let node_id = self.node_id()?;
        let connection = self.connection()?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM archive_roots WHERE archive_root_id = ?1)",
            params![archive_root_id],
            |row| row.get(0),
        )?;
        if !exists {
            bail!("archive root not found: {archive_root_id}");
        }
        connection.execute(
            r#"
            INSERT INTO archive_root_bindings(archive_root_id, node_id, local_mount_path, bound_at)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(archive_root_id, node_id) DO UPDATE SET
                local_mount_path = excluded.local_mount_path,
                bound_at = excluded.bound_at
            "#,
            params![archive_root_id, node_id, mount_text, now()],
        )?;

        // Re-resolve every location stored under this root. `local_path` is a
        // derived cache; the root-relative path is what was true all along.
        connection.execute(
            r#"
            UPDATE project_asset_local_locations
            SET local_path = ?3 || '/' || root_relative_path,
                verification_state = 'unverified',
                verified_size_bytes = NULL,
                verified_mtime_millis = NULL
            WHERE archive_root_id = ?1 AND node_id = ?2 AND root_relative_path IS NOT NULL
            "#,
            params![archive_root_id, node_id, mount_text],
        )?;
        // Point each asset at its relocated copy when that is where it lived.
        connection.execute(
            r#"
            UPDATE assets
            SET path = (
                SELECT l.local_path FROM project_asset_local_locations l
                WHERE l.asset_id = assets.id AND l.archive_root_id = ?1 AND l.node_id = ?2
                LIMIT 1
            )
            WHERE EXISTS (
                SELECT 1 FROM project_asset_local_locations l
                WHERE l.asset_id = assets.id AND l.archive_root_id = ?1 AND l.node_id = ?2
            )
            "#,
            params![archive_root_id, node_id],
        )?;
        drop(connection);
        self.archive_root(archive_root_id)
    }

    pub fn archive_root(&self, archive_root_id: &str) -> Result<ArchiveRoot> {
        self.list_archive_roots()?
            .into_iter()
            .find(|root| root.id == archive_root_id)
            .with_context(|| format!("archive root not found: {archive_root_id}"))
    }

    /// Every named destination, newest first, each resolved against this machine.
    pub fn list_archive_roots(&self) -> Result<Vec<ArchiveRoot>> {
        let connection = self.connection()?;
        if !crate::table_exists(&connection, "archive_roots")? {
            return Ok(Vec::new());
        }
        let node_id = self.node_id()?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT r.archive_root_id, r.display_name, r.created_at, b.local_mount_path
            FROM archive_roots r
            LEFT JOIN archive_root_bindings b
                ON b.archive_root_id = r.archive_root_id AND b.node_id = ?1
            ORDER BY r.created_at DESC, r.archive_root_id ASC
            "#,
        )?;
        let rows = statement.query_map([&node_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut roots = Vec::new();
        for row in rows {
            let (id, display_name, created_at, local_mount_path) = row?;
            let available = local_mount_path
                .as_deref()
                .is_some_and(|path| Path::new(path).is_dir());
            roots.push(ArchiveRoot {
                id,
                display_name,
                available_bytes: available
                    .then(|| {
                        local_mount_path
                            .as_deref()
                            .and_then(|path| fs2::available_space(Path::new(path)).ok())
                    })
                    .flatten(),
                local_mount_path,
                available,
                created_at,
            });
        }
        Ok(roots)
    }

    /// Resolves a root-relative path to where it lives on this machine.
    ///
    /// The relative path is validated rather than trusted: it can originate from
    /// a camera's own folder structure on a mounted card, and a path that
    /// escapes the root would write outside the destination the user chose.
    pub fn resolve_in_archive_root(
        &self,
        archive_root_id: &str,
        relative: &str,
    ) -> Result<PathBuf> {
        let relative = RootRelativePath::parse(relative)
            .map_err(|error| anyhow::anyhow!("unusable media path: {error}"))?;
        let root = self.archive_root(archive_root_id)?;
        let mount = root.local_mount_path.with_context(|| {
            format!(
                "\"{}\" has never been connected to this machine",
                root.display_name
            )
        })?;
        if !root.available {
            bail!(
                "\"{}\" is not mounted at {mount} right now",
                root.display_name
            );
        }
        let resolved = Path::new(&mount).join(relative.as_str());
        // Containment is re-checked after joining, not only by parsing, so a
        // symlinked component cannot carry the write outside the root.
        let mount_canonical = Path::new(&mount).canonicalize()?;
        let existing_ancestor = resolved
            .ancestors()
            .find(|candidate| candidate.exists())
            .unwrap_or(Path::new(&mount));
        if !existing_ancestor
            .canonicalize()?
            .starts_with(&mount_canonical)
        {
            bail!("that media path resolves outside the archive root");
        }
        Ok(resolved)
    }

    /// True when this catalog has somewhere to put imported originals.
    pub fn has_available_archive_root(&self) -> Result<bool> {
        Ok(self.list_archive_roots()?.iter().any(|root| root.available))
    }

    /// Records that an asset's original lives under an archive root, so it keeps
    /// resolving when that root moves.
    ///
    /// Called by the copy path once a file has been committed into the archive;
    /// until then a location is an ordinary absolute path like any other.
    pub fn record_archive_location(
        &self,
        asset_id: &str,
        archive_root_id: &str,
        relative: &RootRelativePath,
        local_path: &Path,
    ) -> Result<()> {
        let node_id = self.node_id()?;
        self.connection()?.execute(
            r#"
            UPDATE project_asset_local_locations
            SET archive_root_id = ?3, root_relative_path = ?4
            WHERE asset_id = ?1 AND node_id = ?2 AND local_path = ?5
            "#,
            params![
                asset_id,
                node_id,
                archive_root_id,
                relative.as_str(),
                local_path.to_string_lossy(),
            ],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// The promise the whole model exists for: the archive drive moves, the user
    /// says where it went once, and every clip under it resolves again — no
    /// per-asset repair, no re-import.
    #[test]
    fn rebinding_a_root_relocates_every_asset_stored_under_it() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();

        let first_drive = directory.path().join("Volumes/Shuttle");
        let clip_relative = RootRelativePath::parse("2026-08-spain/CARD-4f2a/C0001.MP4").unwrap();
        let first_clip = first_drive.join(clip_relative.as_str());
        std::fs::create_dir_all(first_clip.parent().unwrap()).unwrap();
        std::fs::write(&first_clip, b"archived clip bytes").unwrap();

        let root = catalog
            .create_archive_root("Shuttle SSD", &first_drive)
            .unwrap();
        assert!(root.available);
        assert!(root.available_bytes.is_some());

        crate::tests::insert_asset_at(&catalog, &project.id, "asset_one", &first_clip);
        catalog.resolve_original("asset_one").unwrap();
        catalog
            .record_archive_location("asset_one", &root.id, &clip_relative, &first_clip)
            .unwrap();

        // The user copies the archive to a NAS share and plugs the shuttle drive
        // into a drawer. Nothing tells the app.
        let nas = directory.path().join("Volumes/nas/VideoArchive");
        let moved_clip = nas.join(clip_relative.as_str());
        std::fs::create_dir_all(moved_clip.parent().unwrap()).unwrap();
        std::fs::rename(&first_clip, &moved_clip).unwrap();
        assert!(!catalog.get_asset("asset_one").unwrap().original_available);

        catalog.bind_archive_root(&root.id, &nas).unwrap();

        let asset = catalog.get_asset("asset_one").unwrap();
        assert_eq!(
            Path::new(&asset.path).canonicalize().unwrap(),
            moved_clip.canonicalize().unwrap()
        );
        assert!(
            asset.original_available,
            "one rebind must reconnect everything stored under the root"
        );
        // And the relocated bytes are still verified before anything reads them.
        assert_eq!(
            catalog.resolve_original("asset_one").unwrap(),
            moved_clip.canonicalize().unwrap_or(moved_clip)
        );
    }

    #[test]
    fn an_unmounted_root_is_a_reported_state_not_an_error() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let drive = directory.path().join("Volumes/Shuttle");
        std::fs::create_dir_all(&drive).unwrap();
        let root = catalog.create_archive_root("Shuttle SSD", &drive).unwrap();
        assert!(catalog.has_available_archive_root().unwrap());

        std::fs::remove_dir_all(&drive).unwrap();

        let listed = catalog.list_archive_roots().unwrap();
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].available);
        assert_eq!(listed[0].available_bytes, None);
        assert_eq!(
            listed[0].local_mount_path.as_deref(),
            Some(drive.to_string_lossy().as_ref()),
            "an unplugged drive must still say where it is expected"
        );
        assert!(!catalog.has_available_archive_root().unwrap());

        let error = catalog
            .resolve_in_archive_root(&root.id, "trip/C0001.MP4")
            .unwrap_err();
        assert!(format!("{error:#}").contains("not mounted"), "{error:#}");
    }

    /// A card's own folder structure becomes the archive-relative path, so it is
    /// untrusted input. It must not be able to write outside the destination the
    /// user chose.
    #[test]
    fn a_relative_path_cannot_escape_the_archive_root() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let drive = directory.path().join("archive");
        std::fs::create_dir_all(&drive).unwrap();
        let root = catalog.create_archive_root("Archive", &drive).unwrap();

        for hostile in [
            "../outside.MP4",
            "/etc/passwd",
            "trip/../../outside.MP4",
            "trip/%2e%2e/outside.MP4",
            "",
        ] {
            assert!(
                catalog.resolve_in_archive_root(&root.id, hostile).is_err(),
                "accepted {hostile:?}"
            );
        }
        assert!(
            catalog
                .resolve_in_archive_root(&root.id, "2026-08-spain/CARD-4f2a/C0001.MP4")
                .is_ok()
        );
    }

    #[test]
    fn an_archive_root_needs_a_name_and_a_reachable_folder() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let drive = directory.path().join("archive");
        std::fs::create_dir_all(&drive).unwrap();

        assert!(catalog.create_archive_root("   ", &drive).is_err());
        assert!(
            catalog
                .create_archive_root("Nowhere", &directory.path().join("not-mounted"))
                .is_err()
        );
        assert!(catalog.list_archive_roots().unwrap().is_empty());
    }
}
