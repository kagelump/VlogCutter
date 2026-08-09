//! Classifying a source before touching the catalog.
//!
//! The import dialog has to answer "what is new, what is already here, and where
//! is this going" *before* the user commits to anything. That answer has to be
//! produced without mutating a thing, because a preview that half-imports is not
//! a preview.
//!
//! One classifier serves both entry points. Pointed at a mounted card it drives
//! the import dialog; pointed at a folder the user picked it drives the repair
//! flow. The questions are the same — is this content known, and can this asset
//! reach its original — so the code is too.
//!
//! Cost discipline: identity comes from the sampled fingerprint (size plus the
//! first and last 1 MiB), so a card of a few hundred 4K clips classifies in
//! seconds. ffprobe and ExifTool are not run here at all; they are import's
//! problem, and only for files that turn out to be new.

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::cards::{CardCandidate, card_session_slug, inspect_card};
use crate::{
    ArchiveRoot, Catalog, has_current_proxy, is_media, locations, sampled_media_fingerprint,
};

/// What import would do with one candidate file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Classification {
    /// Content this project has never seen. Import creates an asset.
    New,
    /// Known, and the asset can already reach its original. Nothing to do.
    KnownVerified,
    /// Known, but the asset's original is offline and this is it. The whole
    /// reason a duplicate must not simply be skipped.
    KnownRelinkable,
    /// Known and reachable, but its proxy is missing or stale.
    KnownMissingProxy,
    /// Something already occupies the archive path this file would take, and it
    /// is different content. Reported, never overwritten.
    PathCollision,
    /// Could not be read well enough to classify.
    Unreadable,
}

/// One classified file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightItem {
    pub path: String,
    /// Path relative to the source root, which becomes the archive-relative path
    /// and so preserves the camera's own folder structure.
    pub relative_path: String,
    pub filename: String,
    pub size_bytes: u64,
    pub classification: Classification,
    /// The asset this content already belongs to, when it is known.
    pub asset_id: Option<String>,
    /// Why a file could not be classified. Empty unless `Unreadable`.
    pub error: String,
}

/// Where an import would put originals, stated plainly enough to show a user.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportDestination {
    /// True when originals are copied; false when they are referenced in place.
    pub copies: bool,
    /// The archive root originals land in, when copying.
    pub archive_root: Option<ArchiveRoot>,
    /// Where under that root, or the source folder when referencing in place.
    pub resolved_path: String,
    /// A human-readable statement of what happens to the originals, so the
    /// dialog never has to infer it.
    pub summary: String,
    /// Bytes the copy would write. Zero when referencing in place.
    pub bytes_to_copy: u64,
    /// Free bytes at the destination, when it is reachable.
    pub available_bytes: Option<u64>,
    /// True when the destination is reachable and has room for the copy.
    pub ready: bool,
    /// Why the destination is not ready. Empty when it is.
    pub blocked_reason: String,
}

/// The whole answer the import dialog renders.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreflight {
    pub source_path: String,
    /// Set when the source is a recognizable camera card.
    pub card: Option<CardCandidate>,
    pub destination: ImportDestination,
    pub scanned: usize,
    pub new_clips: usize,
    /// Known content that needs nothing.
    pub known_clips: usize,
    /// Known content whose original is offline and is present here.
    pub relinkable: usize,
    /// Clips this import would leave needing a proxy.
    pub missing_proxies: usize,
    pub collisions: usize,
    pub unreadable: usize,
    /// Every file, so the result stays inspectable after the dialog closes.
    pub items: Vec<PreflightItem>,
}

impl Catalog {
    /// Classifies everything at `source` against `project_id` without mutating
    /// anything.
    ///
    /// `archive_root_id` names where originals would be copied. Passing `None`
    /// means reference-in-place, which is what a non-removable source gets.
    pub fn preflight_import<F>(
        &self,
        project_id: &str,
        source: impl AsRef<Path>,
        archive_root_id: Option<&str>,
        mut on_progress: F,
    ) -> Result<ImportPreflight>
    where
        F: FnMut(usize, usize, &Path),
    {
        self.require_project(project_id)?;
        let source = source.as_ref();
        if !source.is_dir() {
            anyhow::bail!("import source is not a directory: {}", source.display());
        }
        let card = inspect_card(source)?;

        let candidates: Vec<PathBuf> = WalkDir::new(source)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.into_path())
            .filter(|path| is_media(path))
            .collect();

        let total = candidates.len();
        let mut items = Vec::with_capacity(total);
        for (index, path) in candidates.iter().enumerate() {
            items.push(self.classify(project_id, source, path, archive_root_id, card.as_ref()));
            on_progress(index + 1, total, path);
        }

        let bytes_to_copy = items
            .iter()
            .filter(|item| item.classification == Classification::New)
            .map(|item| item.size_bytes)
            .sum();
        let destination = self.describe_destination(
            project_id,
            source,
            archive_root_id,
            card.as_ref(),
            bytes_to_copy,
        )?;

        Ok(ImportPreflight {
            source_path: source.to_string_lossy().into_owned(),
            card,
            scanned: total,
            new_clips: count(&items, Classification::New),
            known_clips: count(&items, Classification::KnownVerified),
            relinkable: count(&items, Classification::KnownRelinkable),
            missing_proxies: count(&items, Classification::KnownMissingProxy),
            collisions: count(&items, Classification::PathCollision),
            unreadable: count(&items, Classification::Unreadable),
            destination,
            items,
        })
    }

    fn classify(
        &self,
        project_id: &str,
        source: &Path,
        path: &Path,
        archive_root_id: Option<&str>,
        card: Option<&CardCandidate>,
    ) -> PreflightItem {
        let relative_path = path
            .strip_prefix(source)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let filename = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut item = PreflightItem {
            path: path.to_string_lossy().into_owned(),
            relative_path: relative_path.clone(),
            filename,
            size_bytes: 0,
            classification: Classification::Unreadable,
            asset_id: None,
            error: String::new(),
        };
        match self.classify_inner(project_id, path, &relative_path, archive_root_id, card) {
            Ok((classification, asset_id, size_bytes)) => {
                item.classification = classification;
                item.asset_id = asset_id;
                item.size_bytes = size_bytes;
            }
            Err(error) => item.error = format!("{error:#}"),
        }
        item
    }

    fn classify_inner(
        &self,
        project_id: &str,
        path: &Path,
        relative_path: &str,
        archive_root_id: Option<&str>,
        card: Option<&CardCandidate>,
    ) -> Result<(Classification, Option<String>, u64)> {
        let size_bytes = std::fs::metadata(path)?.len();
        let fingerprint = sampled_media_fingerprint(path, size_bytes)?;
        let connection = self.connection()?;
        let known: Option<(String, String)> = connection
            .query_row(
                "SELECT id, path FROM assets WHERE project_id = ?1 AND fingerprint = ?2",
                params![project_id, fingerprint],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        let Some((asset_id, recorded_path)) = known else {
            // New content. The only remaining question is whether it can land
            // where it would be written.
            if let (Some(root_id), Some(card)) = (archive_root_id, card) {
                let target = self.archive_target(project_id, root_id, card, relative_path)?;
                if target.is_file() {
                    // Same name, different content. Overwriting an archived
                    // original is never the answer.
                    return Ok((Classification::PathCollision, None, size_bytes));
                }
            }
            return Ok((Classification::New, None, size_bytes));
        };

        // Known content. Can the asset actually reach an original right now?
        let recorded = locations::recorded_location(&connection, &asset_id, &recorded_path)?;
        let reachable = locations::location_is_available(
            &recorded_path,
            recorded.map(|found| found.state),
            recorded.and_then(|found| found.guard),
        );
        if !reachable {
            return Ok((Classification::KnownRelinkable, Some(asset_id), size_bytes));
        }
        drop(connection);
        let asset = self.get_asset(&asset_id)?;
        if has_current_proxy(&asset) {
            Ok((Classification::KnownVerified, Some(asset_id), size_bytes))
        } else {
            Ok((
                Classification::KnownMissingProxy,
                Some(asset_id),
                size_bytes,
            ))
        }
    }

    /// Where one card-relative file would be written under an archive root.
    pub(crate) fn archive_target(
        &self,
        project_id: &str,
        archive_root_id: &str,
        card: &CardCandidate,
        relative_path: &str,
    ) -> Result<PathBuf> {
        let relative = self.archive_relative_path(project_id, card, relative_path)?;
        self.resolve_in_archive_root(archive_root_id, &relative)
    }

    /// `<projectSlug>/<cardSessionSlug>/<cardRelativePath>`.
    ///
    /// The card-session component is what keeps a reformatted card from writing
    /// into the previous card's directory when both are labelled `UNTITLED` and
    /// both contain `C0001.MP4`.
    pub(crate) fn archive_relative_path(
        &self,
        project_id: &str,
        card: &CardCandidate,
        relative_path: &str,
    ) -> Result<String> {
        let project_name: String = self.connection()?.query_row(
            "SELECT name FROM projects WHERE id = ?1",
            params![project_id],
            |row| row.get(0),
        )?;
        let slug = directory_slug(&project_name);
        Ok(format!(
            "{slug}/{}/{relative_path}",
            card_session_slug(card)
        ))
    }

    fn describe_destination(
        &self,
        project_id: &str,
        source: &Path,
        archive_root_id: Option<&str>,
        card: Option<&CardCandidate>,
        bytes_to_copy: u64,
    ) -> Result<ImportDestination> {
        let Some(archive_root_id) = archive_root_id else {
            // Referenced in place. Say so plainly, and say where — a user who
            // does not know their footage is still on the card cannot make an
            // informed decision about ejecting it.
            return Ok(ImportDestination {
                copies: false,
                archive_root: None,
                resolved_path: source.to_string_lossy().into_owned(),
                summary: format!(
                    "Originals stay where they are, in {}",
                    source.to_string_lossy()
                ),
                bytes_to_copy: 0,
                available_bytes: None,
                ready: true,
                blocked_reason: String::new(),
            });
        };

        let root = self.archive_root(archive_root_id)?;
        let relative = match card {
            Some(card) => self.archive_relative_path(project_id, card, "")?,
            None => String::new(),
        };
        let resolved_path = match root.local_mount_path.as_deref() {
            Some(mount) => format!("{mount}/{}", relative.trim_end_matches('/')),
            None => String::new(),
        };
        let (ready, blocked_reason) = if !root.available {
            (
                false,
                format!("\"{}\" is not connected to this machine", root.display_name),
            )
        } else if root
            .available_bytes
            .is_some_and(|available| available < bytes_to_copy)
        {
            (
                false,
                format!(
                    "\"{}\" has {} free but this import needs {}",
                    root.display_name,
                    human_bytes(root.available_bytes.unwrap_or(0)),
                    human_bytes(bytes_to_copy),
                ),
            )
        } else {
            (true, String::new())
        };

        Ok(ImportDestination {
            summary: format!(
                "Originals are copied to \"{}\", and stay on the card too",
                root.display_name
            ),
            copies: true,
            resolved_path,
            bytes_to_copy,
            available_bytes: root.available_bytes,
            archive_root: Some(root),
            ready,
            blocked_reason,
        })
    }
}

fn count(items: &[PreflightItem], classification: Classification) -> usize {
    items
        .iter()
        .filter(|item| item.classification == classification)
        .count()
}

/// A project name reduced to one safe path component.
fn directory_slug(name: &str) -> String {
    let slug: String = name
        .trim()
        .chars()
        .map(|character| {
            if character.is_control() || matches!(character, '/' | '\\' | ':') {
                '-'
            } else {
                character
            }
        })
        .collect();
    let slug = slug.trim_matches([' ', '.']);
    if slug.is_empty() {
        "project".to_string()
    } else {
        slug.to_string()
    }
}

/// Bytes as a person reads them. Import decisions are made on these numbers, so
/// they are rendered in the core rather than three times in three adapters.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::insert_asset_at;
    use std::fs;
    use tempfile::tempdir;

    fn card_fixture(root: &Path, clips: &[(&str, &[u8])]) -> PathBuf {
        let mount = root.join("DJI_SD");
        let layout = mount.join("DCIM/100MSDCF");
        fs::create_dir_all(&layout).unwrap();
        for (name, bytes) in clips {
            fs::write(layout.join(name), bytes).unwrap();
        }
        mount
    }

    /// The number the dialog leads with, and the states behind it.
    #[test]
    fn preflight_separates_new_from_known_and_from_lost() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let mount = card_fixture(
            directory.path(),
            &[
                ("C0001.MP4", b"already imported and reachable"),
                ("C0002.MP4", b"already imported but gone missing"),
                ("C0003.MP4", b"brand new footage"),
            ],
        );
        let layout = mount.join("DCIM/100MSDCF");

        // One clip is known and still reachable on the card itself.
        insert_asset_at(
            &catalog,
            &project.id,
            "asset_here",
            &layout.join("C0001.MP4"),
        );
        // One is known but its original lived somewhere that is now gone.
        let lost = directory.path().join("old-drive/C0002.MP4");
        fs::create_dir_all(lost.parent().unwrap()).unwrap();
        fs::copy(layout.join("C0002.MP4"), &lost).unwrap();
        insert_asset_at(&catalog, &project.id, "asset_lost", &lost);
        fs::remove_file(&lost).unwrap();

        let preflight = catalog
            .preflight_import(&project.id, &mount, None, |_, _, _| {})
            .unwrap();

        assert_eq!(preflight.scanned, 3);
        assert_eq!(preflight.new_clips, 1);
        assert_eq!(preflight.relinkable, 1);
        // Known and reachable, but it has no proxy yet.
        assert_eq!(preflight.missing_proxies, 1);
        assert_eq!(preflight.unreadable, 0);
        assert_eq!(
            preflight
                .card
                .as_ref()
                .map(|card| card.camera_hint.as_str()),
            Some("Sony")
        );

        let relinkable = preflight
            .items
            .iter()
            .find(|item| item.classification == Classification::KnownRelinkable)
            .unwrap();
        assert_eq!(relinkable.asset_id.as_deref(), Some("asset_lost"));
        assert_eq!(relinkable.relative_path, "DCIM/100MSDCF/C0002.MP4");
    }

    /// A preview that half-imports is not a preview.
    #[test]
    fn preflight_leaves_the_catalog_untouched() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let mount = card_fixture(directory.path(), &[("C0001.MP4", b"new footage")]);

        let before = catalog.list_assets(&project.id).unwrap().len();
        let operations_before = catalog.list_operations(&project.id).unwrap().len();

        catalog
            .preflight_import(&project.id, &mount, None, |_, _, _| {})
            .unwrap();

        assert_eq!(catalog.list_assets(&project.id).unwrap().len(), before);
        assert_eq!(
            catalog.list_operations(&project.id).unwrap().len(),
            operations_before
        );
    }

    /// The dialog must be able to say where footage is going without the user
    /// inferring it.
    #[test]
    fn a_card_destination_names_the_root_and_the_resolved_path() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let mount = card_fixture(directory.path(), &[("C0001.MP4", b"new footage")]);
        let archive = directory.path().join("nas/VideoArchive");
        fs::create_dir_all(&archive).unwrap();
        let root = catalog.create_archive_root("NAS Video", &archive).unwrap();

        let preflight = catalog
            .preflight_import(&project.id, &mount, Some(&root.id), |_, _, _| {})
            .unwrap();

        let destination = &preflight.destination;
        assert!(destination.copies);
        assert!(destination.ready, "{}", destination.blocked_reason);
        assert_eq!(
            destination.archive_root.as_ref().unwrap().display_name,
            "NAS Video"
        );
        assert!(destination.resolved_path.contains("Spain 2026"));
        assert!(destination.resolved_path.contains("DJI_SD-"));
        assert_eq!(destination.bytes_to_copy, "new footage".len() as u64);
        assert!(destination.summary.contains("stay on the card too"));
    }

    #[test]
    fn referencing_in_place_says_so_and_says_where() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let folder = directory.path().join("nas-share");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("C0001.MP4"), b"nas footage").unwrap();

        let preflight = catalog
            .preflight_import(&project.id, &folder, None, |_, _, _| {})
            .unwrap();

        assert!(!preflight.destination.copies);
        assert_eq!(preflight.destination.bytes_to_copy, 0);
        assert!(
            preflight
                .destination
                .summary
                .contains("stay where they are")
        );
        assert!(
            preflight.destination.resolved_path.contains("nas-share"),
            "the dialog must name the folder, not just the policy"
        );
    }

    /// A copy that will not fit is refused before it starts, not halfway through.
    #[test]
    fn an_unreachable_or_full_destination_blocks_the_import() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let mount = card_fixture(directory.path(), &[("C0001.MP4", b"footage")]);
        let archive = directory.path().join("shuttle");
        fs::create_dir_all(&archive).unwrap();
        let root = catalog
            .create_archive_root("Shuttle SSD", &archive)
            .unwrap();

        fs::remove_dir_all(&archive).unwrap();
        let preflight = catalog
            .preflight_import(&project.id, &mount, Some(&root.id), |_, _, _| {})
            .unwrap();
        assert!(!preflight.destination.ready);
        assert!(
            preflight
                .destination
                .blocked_reason
                .contains("not connected"),
            "{}",
            preflight.destination.blocked_reason
        );
    }

    /// An archived original is never overwritten by a card that reuses its name.
    #[test]
    fn an_occupied_archive_path_is_reported_rather_than_overwritten() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        let mount = card_fixture(directory.path(), &[("C0001.MP4", b"reshot footage")]);
        let archive = directory.path().join("nas");
        fs::create_dir_all(&archive).unwrap();
        let root = catalog.create_archive_root("NAS Video", &archive).unwrap();

        // Something already sits exactly where this clip would land.
        let card = inspect_card(&mount).unwrap().unwrap();
        let target = catalog
            .archive_target(&project.id, &root.id, &card, "DCIM/100MSDCF/C0001.MP4")
            .unwrap();
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"different bytes entirely").unwrap();

        let preflight = catalog
            .preflight_import(&project.id, &mount, Some(&root.id), |_, _, _| {})
            .unwrap();
        assert_eq!(preflight.collisions, 1);
        assert_eq!(preflight.new_clips, 0);
    }

    #[test]
    fn byte_sizes_render_for_a_person() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(62_400_000_000), "62.4 GB");
        assert_eq!(human_bytes(1_200_000_000_000), "1.2 TB");
    }
}
