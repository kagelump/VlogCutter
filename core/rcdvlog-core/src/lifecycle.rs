//! Understanding and reclaiming local storage, and removing a project (#16).
//!
//! Two operations live here, and they are separated on purpose because they
//! carry very different risk:
//!
//! - **Cache cleanup** removes data the application can make again. It touches
//!   only `<catalog dir>/cache`, never the catalog and never a byte of media.
//! - **Project removal** deletes the user's own work — annotations, chronology,
//!   import history. It is guarded by an audit that has to pass first.
//!
//! ## The line this module must not cross
//!
//! Originals and archive copies are never deleted, moved, renamed, or
//! rewritten (`AGENTS.md` §2, and the import epic's §2.1 amendment which only
//! ever *adds* a copy). Nothing here deletes by path pattern or by walking
//! upward. Cleanup deletes inside one `cache` directory whose location is
//! derived from the catalog file, and `audit_project_removal` refuses to
//! declare a folder removable when anything else has been put inside it — an
//! archive root, a referenced original, another project's catalog rows, or any
//! file this application did not create.
//!
//! ## Why deleting cache files needs no bookkeeping
//!
//! `proxy_available` and `thumbnail_available` are derived at read time from
//! whether the file is there (`lib.rs`'s asset row mapper). A deleted proxy is
//! therefore *immediately* and correctly reported as missing, with no row to
//! update and no window in which the catalog claims something that is gone.
//! That is also why cleanup never needs the originals mounted: it reads
//! nothing about the media, it only removes files it made.

use anyhow::{Context, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::Catalog;

/// A kind of regenerable local data. Ordered as the UI lists it: biggest and
/// most obviously disposable first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CacheKind {
    /// Laptop review proxies. Remade from the originals, so remaking them
    /// needs the source media reachable again.
    Proxies,
    /// Queue and ribbon posters. Remade from a proxy or an original.
    Thumbnails,
    /// Extracted audio for speech analysis.
    Audio,
    /// Decoded frame batches held for vision analysis.
    FramePacks,
    /// Rendered edit-plan preview workspaces.
    EditPreviews,
    /// Posters the import dialog decoded for its file list.
    ImportPreviews,
    /// Downloaded analysis models. Regenerable only by downloading them
    /// again, which is why they are reported apart from everything else.
    AnalysisModels,
}

/// Whether a category belongs to one project or to the catalog that holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CacheScope {
    /// Removing it affects this project only.
    Project,
    /// Shared by every project in this catalog. Normally that is one project —
    /// the desktop creates a folder per project — but a catalog is allowed to
    /// hold more, and a shared cache must say so before it is cleared.
    Catalog,
}

/// One measured category of disposable data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheCategory {
    pub kind: CacheKind,
    pub scope: CacheScope,
    pub bytes: u64,
    pub file_count: usize,
}

/// What one project is currently costing on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectStorageReport {
    pub project_id: String,
    pub catalog_path: String,
    /// The catalog file itself, plus its write-ahead log when one exists.
    /// Reported so the user can see that clearing every cache still leaves
    /// their work behind, and how little it weighs.
    pub catalog_bytes: u64,
    pub categories: Vec<CacheCategory>,
    /// Every category summed, so a caller never re-adds them differently.
    pub cache_bytes: u64,
}

/// What a cleanup actually removed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheCleanupReport {
    pub removed_bytes: u64,
    pub removed_files: usize,
    /// Files that could not be removed — a permission problem, a file another
    /// process still holds open. Reported rather than raised: a cleanup that
    /// freed most of what it promised has still done useful work, and the
    /// catalog is untouched either way.
    pub failures: Vec<crate::IngestFailure>,
}

/// Whether this project's folder can be removed outright, and what stands in
/// the way when it cannot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRemovalAudit {
    pub project_id: String,
    pub project_name: String,
    pub catalog_path: String,
    pub folder_path: String,
    pub catalog_bytes: u64,
    pub cache_bytes: u64,
    /// Clips whose membership this removal ends. They are catalog rows; not
    /// one byte of the media they point at is touched.
    pub asset_count: usize,
    /// Names of other projects sharing this catalog. Non-empty means the
    /// folder stays and only this project's rows and caches go.
    pub other_projects_in_catalog: Vec<String>,
    /// Archive roots that resolve inside this folder. A user is free to point
    /// an archive at their project folder, and if they did, the folder holds
    /// originals and must survive.
    pub archive_roots_inside: Vec<String>,
    /// Assets whose original file is inside this folder, for the same reason.
    pub originals_inside: usize,
    /// Anything in the folder this application did not put there. Listed, not
    /// summarized: a user deciding whether to lose a folder deserves to see
    /// exactly what else is in it.
    pub unexpected_entries: Vec<String>,
    /// True only when every check above came back clean.
    pub folder_removable: bool,
}

/// Files the desktop is known to create beside a catalog. Anything else in a
/// project folder is treated as the user's and blocks folder removal.
const KNOWN_FOLDER_ENTRIES: &[&str] = &[
    "catalog.sqlite3",
    "catalog.sqlite3-wal",
    "catalog.sqlite3-shm",
    "catalog.sqlite3-journal",
    "cache",
    ".DS_Store",
];

/// Bytes and file count under a directory. A missing directory is zero, not an
/// error: not having made any proxies yet is a normal state.
fn measure(directory: &Path) -> (u64, usize) {
    let mut bytes = 0_u64;
    let mut files = 0_usize;
    for entry in walkdir::WalkDir::new(directory)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        if let Ok(metadata) = entry.metadata() {
            bytes = bytes.saturating_add(metadata.len());
            files += 1;
        }
    }
    (bytes, files)
}

/// Removes a directory tree, counting what went and reporting what would not.
fn remove_tree(directory: &Path, report: &mut CacheCleanupReport) {
    if !directory.exists() {
        return;
    }
    let (bytes, files) = measure(directory);
    match std::fs::remove_dir_all(directory) {
        Ok(()) => {
            report.removed_bytes = report.removed_bytes.saturating_add(bytes);
            report.removed_files += files;
        }
        Err(error) => report.failures.push(crate::IngestFailure {
            path: directory.to_string_lossy().into_owned(),
            error: format!("{error}"),
        }),
    }
}

impl Catalog {
    /// `<catalog dir>/cache`, the only directory anything in this module
    /// deletes from.
    fn cache_root(&self) -> Result<PathBuf> {
        Ok(self
            .database_path()
            .parent()
            .ok_or_else(|| anyhow::anyhow!("catalog has no parent directory"))?
            .join("cache"))
    }

    /// Where each category lives. Thumbnails are the odd one out: they are
    /// keyed by asset rather than by project, so scoping them to a project
    /// means asking the catalog which assets are its own.
    fn cache_directories(&self, project_id: &str, kind: CacheKind) -> Result<Vec<PathBuf>> {
        let root = self.cache_root()?;
        Ok(match kind {
            CacheKind::Proxies => vec![root.join("proxies").join(project_id)],
            CacheKind::Audio => vec![root.join("audio").join(project_id)],
            CacheKind::FramePacks => vec![root.join("framepacks").join(project_id)],
            CacheKind::EditPreviews => vec![root.join("previews").join(project_id)],
            CacheKind::ImportPreviews => vec![root.join("import-preview")],
            CacheKind::AnalysisModels => vec![root.join("models")],
            CacheKind::Thumbnails => {
                let thumbnails = root.join("thumbnails");
                self.project_asset_ids(project_id)?
                    .into_iter()
                    .map(|asset_id| thumbnails.join(asset_id))
                    .collect()
            }
        })
    }

    fn project_asset_ids(&self, project_id: &str) -> Result<Vec<String>> {
        let connection = self.connection()?;
        let mut statement =
            connection.prepare("SELECT id FROM assets WHERE project_id = ?1 ORDER BY id")?;
        let rows = statement.query_map(params![project_id], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Measures what this project is costing, category by category.
    ///
    /// Reads nothing about the media and never touches it, so a project whose
    /// originals are on a drive in a drawer reports exactly as well as one
    /// whose card is still in the slot.
    pub fn project_storage_report(&self, project_id: &str) -> Result<ProjectStorageReport> {
        self.require_project(project_id)?;
        let database = self.database_path().to_path_buf();
        let mut catalog_bytes = std::fs::metadata(&database)
            .map(|data| data.len())
            .unwrap_or(0);
        for suffix in ["-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{suffix}", database.to_string_lossy()));
            catalog_bytes = catalog_bytes.saturating_add(
                std::fs::metadata(&sidecar)
                    .map(|data| data.len())
                    .unwrap_or(0),
            );
        }

        let mut categories = Vec::new();
        let mut cache_bytes = 0_u64;
        for kind in [
            CacheKind::Proxies,
            CacheKind::Thumbnails,
            CacheKind::Audio,
            CacheKind::FramePacks,
            CacheKind::EditPreviews,
            CacheKind::ImportPreviews,
            CacheKind::AnalysisModels,
        ] {
            let mut bytes = 0_u64;
            let mut file_count = 0_usize;
            for directory in self.cache_directories(project_id, kind)? {
                let (found_bytes, found_files) = measure(&directory);
                bytes = bytes.saturating_add(found_bytes);
                file_count += found_files;
            }
            cache_bytes = cache_bytes.saturating_add(bytes);
            categories.push(CacheCategory {
                kind,
                scope: match kind {
                    CacheKind::ImportPreviews | CacheKind::AnalysisModels => CacheScope::Catalog,
                    _ => CacheScope::Project,
                },
                bytes,
                file_count,
            });
        }

        Ok(ProjectStorageReport {
            project_id: project_id.to_string(),
            catalog_path: database.to_string_lossy().into_owned(),
            catalog_bytes,
            categories,
            cache_bytes,
        })
    }

    /// Deletes the named categories of disposable data.
    ///
    /// The catalog is never opened for writing here. Every fact the user
    /// authored survives, and the rows that describe these files need no
    /// update because availability is read from the filesystem — see this
    /// module's doc comment.
    pub fn clear_project_cache(
        &self,
        project_id: &str,
        kinds: &[CacheKind],
        mut on_progress: impl FnMut(usize, usize) -> bool,
    ) -> Result<CacheCleanupReport> {
        self.require_project(project_id)?;
        let mut report = CacheCleanupReport::default();
        // Deduplicated and ordered, so asking twice for the same kind cannot
        // double-count and the work is deterministic.
        let kinds: BTreeSet<CacheKind> = kinds.iter().copied().collect();
        let total = kinds.len();
        for (index, kind) in kinds.into_iter().enumerate() {
            if !on_progress(index, total) {
                break;
            }
            for directory in self.cache_directories(project_id, kind)? {
                remove_tree(&directory, &mut report);
            }
            if !on_progress(index + 1, total) {
                break;
            }
        }
        Ok(report)
    }

    /// Everything the caller needs before offering to delete a project.
    ///
    /// `folder` is the directory the desktop believes holds this catalog. It is
    /// passed in rather than derived so the audit can state plainly what it
    /// examined, and so a caller cannot be handed a verdict about a directory
    /// it did not mean.
    pub fn audit_project_removal(
        &self,
        project_id: &str,
        folder: impl AsRef<Path>,
    ) -> Result<ProjectRemovalAudit> {
        self.require_project(project_id)?;
        let folder = folder.as_ref();
        let canonical_folder = folder
            .canonicalize()
            .unwrap_or_else(|_| folder.to_path_buf());
        let report = self.project_storage_report(project_id)?;

        let projects = self.list_projects()?;
        let project_name = projects
            .iter()
            .find(|found| found.id == project_id)
            .map(|found| found.name.clone())
            .unwrap_or_default();
        let other_projects_in_catalog: Vec<String> = projects
            .into_iter()
            .filter(|found| found.id != project_id)
            .map(|found| found.name)
            .collect();

        // A user may point an archive root anywhere, including at their own
        // project folder. If they did, this folder holds originals.
        let archive_roots_inside: Vec<String> = self
            .list_archive_roots()?
            .into_iter()
            .filter(|root| {
                root.local_mount_path
                    .as_deref()
                    .is_some_and(|path| is_inside(&canonical_folder, Path::new(path)))
            })
            .map(|root| root.display_name)
            .collect();

        let originals_inside = self
            .list_assets(project_id)?
            .into_iter()
            .filter(|asset| is_inside(&canonical_folder, Path::new(&asset.path)))
            .count();

        let mut unexpected_entries: Vec<String> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(folder) {
            for entry in entries.filter_map(Result::ok) {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !KNOWN_FOLDER_ENTRIES.contains(&name.as_str()) {
                    unexpected_entries.push(name);
                }
            }
        }
        unexpected_entries.sort();

        let folder_removable = other_projects_in_catalog.is_empty()
            && archive_roots_inside.is_empty()
            && originals_inside == 0
            && unexpected_entries.is_empty();

        Ok(ProjectRemovalAudit {
            project_id: project_id.to_string(),
            project_name,
            catalog_path: report.catalog_path,
            folder_path: folder.to_string_lossy().into_owned(),
            catalog_bytes: report.catalog_bytes,
            cache_bytes: report.cache_bytes,
            asset_count: self.list_assets(project_id)?.len(),
            other_projects_in_catalog,
            archive_roots_inside,
            originals_inside,
            unexpected_entries,
            folder_removable,
        })
    }

    /// Deletes one project's rows and its own disposable caches.
    ///
    /// This is the narrow removal: the catalog file and the folder survive,
    /// which is the only correct outcome when something else is in there. The
    /// desktop removes the folder afterwards, and only when
    /// `audit_project_removal` said it could.
    ///
    /// Trip phases are deleted first, deliberately. Their clip anchors are
    /// `ON DELETE RESTRICT` against `assets`, so a cascade that reached the
    /// assets first would abort the whole delete; removing the phases before
    /// their referents makes the order explicit instead of depending on how
    /// SQLite happens to walk the children.
    pub fn delete_project(&self, project_id: &str) -> Result<CacheCleanupReport> {
        self.require_project(project_id)?;
        let report = self.clear_project_cache(
            project_id,
            &[
                CacheKind::Proxies,
                CacheKind::Thumbnails,
                CacheKind::Audio,
                CacheKind::FramePacks,
                CacheKind::EditPreviews,
            ],
            |_, _| true,
        )?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction
            .execute(
                "DELETE FROM trip_phases WHERE project_id = ?1",
                params![project_id],
            )
            .context("remove trip phases")?;
        transaction
            .execute("DELETE FROM projects WHERE id = ?1", params![project_id])
            .context("remove project")?;
        transaction.commit()?;
        Ok(report)
    }
}

/// Whether `candidate` is the folder itself or sits underneath it.
///
/// Both sides are canonicalized when they exist, so a symlinked projects root
/// or a `..` in a stored path cannot smuggle an original past the check. A path
/// that cannot be canonicalized — an original on a drive that is not mounted —
/// is compared as written, which is the conservative reading: it can only add
/// a reason to refuse, never remove one.
fn is_inside(folder: &Path, candidate: &Path) -> bool {
    let resolved = candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.to_path_buf());
    resolved.starts_with(folder) || candidate.starts_with(folder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    struct Fixture {
        directory: tempfile::TempDir,
        folder: PathBuf,
        catalog: Catalog,
        project_id: String,
    }

    impl Fixture {
        /// An original on a card, i.e. outside the project folder entirely.
        /// A real file, because `insert_asset_at` fingerprints what it is told
        /// about rather than trusting the path.
        fn original_elsewhere(&self, asset_id: &str, filename: &str) -> PathBuf {
            let path = self.directory.path().join("card").join(filename);
            write(&path, b"the footage itself");
            crate::tests::insert_asset_at(&self.catalog, &self.project_id, asset_id, &path);
            path
        }
    }

    fn fixture() -> Fixture {
        let directory = tempdir().unwrap();
        let folder = directory.path().join("Spain 2026");
        fs::create_dir_all(&folder).unwrap();
        let catalog = Catalog::open(folder.join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Spain 2026").unwrap();
        Fixture {
            directory,
            folder,
            project_id: project.id,
            catalog,
        }
    }

    /// This project's name, or `None` once it is gone.
    fn project_named(catalog: &Catalog, project_id: &str) -> Option<String> {
        catalog
            .list_projects()
            .unwrap()
            .into_iter()
            .find(|project| project.id == project_id)
            .map(|project| project.name)
    }

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn category(report: &ProjectStorageReport, kind: CacheKind) -> &CacheCategory {
        report
            .categories
            .iter()
            .find(|category| category.kind == kind)
            .expect("every kind is reported")
    }

    #[test]
    fn storage_is_reported_by_category_and_says_what_is_shared() {
        let fixture = fixture();
        let cache = fixture.folder.join("cache");
        write(
            &cache
                .join("proxies")
                .join(&fixture.project_id)
                .join("a.v3.mp4"),
            &[7; 4096],
        );
        fixture.original_elsewhere("asset_one", "C0001.MP4");
        write(
            &cache.join("thumbnails").join("asset_one").join("q1.v2.jpg"),
            &[1; 256],
        );
        // Another asset's posters are not this project's to report.
        write(
            &cache
                .join("thumbnails")
                .join("asset_elsewhere")
                .join("q1.v2.jpg"),
            &[1; 999],
        );
        write(&cache.join("models").join("whisper.bin"), &[2; 128]);

        let report = fixture
            .catalog
            .project_storage_report(&fixture.project_id)
            .unwrap();
        assert_eq!(category(&report, CacheKind::Proxies).bytes, 4096);
        assert_eq!(category(&report, CacheKind::Proxies).file_count, 1);
        assert_eq!(category(&report, CacheKind::Thumbnails).bytes, 256);
        assert_eq!(category(&report, CacheKind::Audio).bytes, 0);
        // Models are shared by the catalog and cost a download to replace, so
        // they are reported apart from everything else rather than folded in.
        assert_eq!(
            category(&report, CacheKind::AnalysisModels).scope,
            CacheScope::Catalog
        );
        assert_eq!(
            category(&report, CacheKind::Proxies).scope,
            CacheScope::Project
        );
        assert_eq!(report.cache_bytes, 4096 + 256 + 128);
        assert!(
            report.catalog_bytes > 0,
            "the user's own work is measured too"
        );
    }

    #[test]
    fn cleanup_removes_only_the_categories_it_was_asked_for() {
        let fixture = fixture();
        let cache = fixture.folder.join("cache");
        let proxy = cache
            .join("proxies")
            .join(&fixture.project_id)
            .join("a.v3.mp4");
        let model = cache.join("models").join("whisper.bin");
        write(&proxy, &[7; 4096]);
        write(&model, &[2; 128]);

        let report = fixture
            .catalog
            .clear_project_cache(&fixture.project_id, &[CacheKind::Proxies], |_, _| true)
            .unwrap();
        assert_eq!(report.removed_bytes, 4096);
        assert_eq!(report.removed_files, 1);
        assert!(report.failures.is_empty());
        assert!(!proxy.exists());
        assert!(model.exists(), "an unasked-for category is left alone");
        // The catalog and everything in it survives untouched.
        assert!(project_named(&fixture.catalog, &fixture.project_id).is_some());
    }

    #[test]
    fn cleanup_needs_no_originals_and_never_reaches_outside_the_cache() {
        let fixture = fixture();
        let cache = fixture.folder.join("cache");
        write(
            &cache
                .join("proxies")
                .join(&fixture.project_id)
                .join("a.v3.mp4"),
            &[7; 16],
        );
        // An original on a volume that is not mounted, and a file beside the
        // catalog that is nobody's business but the user's.
        let offline = fixture.original_elsewhere("asset_offline", "C0001.MP4");
        fs::remove_file(&offline).unwrap();
        let neighbour = fixture.folder.join("my notes.txt");
        write(&neighbour, b"do not touch");

        let report = fixture
            .catalog
            .clear_project_cache(
                &fixture.project_id,
                &[CacheKind::Proxies, CacheKind::Thumbnails, CacheKind::Audio],
                |_, _| true,
            )
            .unwrap();
        assert_eq!(report.removed_files, 1);
        assert!(report.failures.is_empty());
        assert!(neighbour.exists());
        assert!(fixture.folder.join("catalog.sqlite3").exists());
        assert_eq!(
            fixture
                .catalog
                .list_assets(&fixture.project_id)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn asking_for_the_same_category_twice_does_not_count_it_twice() {
        let fixture = fixture();
        write(
            &fixture
                .folder
                .join("cache/proxies")
                .join(&fixture.project_id)
                .join("a.v3.mp4"),
            &[7; 32],
        );
        let report = fixture
            .catalog
            .clear_project_cache(
                &fixture.project_id,
                &[CacheKind::Proxies, CacheKind::Proxies],
                |_, _| true,
            )
            .unwrap();
        assert_eq!(report.removed_bytes, 32);
        assert_eq!(report.removed_files, 1);
    }

    #[test]
    fn an_ordinary_project_folder_is_removable_and_says_what_goes_with_it() {
        let fixture = fixture();
        write(
            &fixture
                .folder
                .join("cache/proxies")
                .join(&fixture.project_id)
                .join("a.v3.mp4"),
            &[7; 64],
        );
        fixture.original_elsewhere("asset_one", "C0001.MP4");

        let audit = fixture
            .catalog
            .audit_project_removal(&fixture.project_id, &fixture.folder)
            .unwrap();
        assert!(audit.folder_removable);
        assert_eq!(audit.project_name, "Spain 2026");
        assert_eq!(audit.asset_count, 1);
        assert_eq!(audit.cache_bytes, 64);
        assert!(audit.unexpected_entries.is_empty());
        assert!(audit.archive_roots_inside.is_empty());
        assert_eq!(audit.originals_inside, 0);
    }

    #[test]
    fn a_folder_holding_originals_is_never_declared_removable() {
        let fixture = fixture();

        // An archive root the user pointed at their own project folder.
        let archive = fixture.folder.join("archive");
        fs::create_dir_all(&archive).unwrap();
        let root = fixture
            .catalog
            .create_archive_root("Inside", &archive)
            .unwrap();
        let audit = fixture
            .catalog
            .audit_project_removal(&fixture.project_id, &fixture.folder)
            .unwrap();
        assert!(!audit.folder_removable);
        assert_eq!(audit.archive_roots_inside, [root.display_name]);
        // The stray directory is also visible as something nobody asked for.
        assert_eq!(audit.unexpected_entries, ["archive"]);
    }

    #[test]
    fn an_original_inside_the_folder_blocks_removal_on_its_own() {
        let fixture = fixture();
        let original = fixture.folder.join("footage").join("C0001.MP4");
        write(&original, b"the only copy");
        crate::tests::insert_asset_at(
            &fixture.catalog,
            &fixture.project_id,
            "asset_inside",
            &original,
        );

        let audit = fixture
            .catalog
            .audit_project_removal(&fixture.project_id, &fixture.folder)
            .unwrap();
        assert!(!audit.folder_removable);
        assert_eq!(audit.originals_inside, 1);
    }

    #[test]
    fn a_second_project_in_the_same_catalog_keeps_the_folder() {
        let fixture = fixture();
        let other = fixture.catalog.create_project("Okinawa 2026").unwrap();

        let audit = fixture
            .catalog
            .audit_project_removal(&fixture.project_id, &fixture.folder)
            .unwrap();
        assert!(!audit.folder_removable);
        assert_eq!(audit.other_projects_in_catalog, [other.name]);
    }

    #[test]
    fn deleting_a_project_leaves_every_other_project_intact() {
        let fixture = fixture();
        let keeper = fixture.catalog.create_project("Okinawa 2026").unwrap();
        fixture.original_elsewhere("asset_going", "C0001.MP4");
        let staying = fixture.directory.path().join("card").join("C0002.MP4");
        write(&staying, b"another clip");
        crate::tests::insert_asset_at(&fixture.catalog, &keeper.id, "asset_staying", &staying);
        let proxy = fixture
            .folder
            .join("cache/proxies")
            .join(&fixture.project_id)
            .join("a.v3.mp4");
        let keeper_proxy = fixture
            .folder
            .join("cache/proxies")
            .join(&keeper.id)
            .join("b.v3.mp4");
        write(&proxy, &[7; 16]);
        write(&keeper_proxy, &[7; 16]);

        fixture.catalog.delete_project(&fixture.project_id).unwrap();

        assert!(project_named(&fixture.catalog, &fixture.project_id).is_none());
        assert_eq!(
            project_named(&fixture.catalog, &keeper.id).as_deref(),
            Some("Okinawa 2026")
        );
        assert_eq!(fixture.catalog.list_assets(&keeper.id).unwrap().len(), 1);
        assert!(!proxy.exists());
        assert!(
            keeper_proxy.exists(),
            "another project's cache is not collateral"
        );
        assert!(fixture.folder.join("catalog.sqlite3").exists());
    }

    /// `trip_phases` anchors clips with `ON DELETE RESTRICT`, so a project that
    /// has any trip context is exactly the case a naive cascade fails on.
    #[test]
    fn a_project_with_anchored_trip_phases_still_deletes() {
        let fixture = fixture();
        fixture.original_elsewhere("asset_anchor", "C0001.MP4");
        fixture
            .catalog
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO trip_phases (id, project_id, label, phase_kind,
                     start_anchor_asset_id, end_anchor_asset_id, start_trip_time,
                     end_trip_time, timezone, origin_timezone, destination_timezone,
                     display_mode, created_at)
                 VALUES ('phase_1', ?1, 'Barcelona', 'location', 'asset_anchor', 'asset_anchor',
                     '2026-02-08T00:00:00Z', '2026-02-09T00:00:00Z', 'Europe/Madrid',
                     NULL, NULL, 'local', '2026-02-08T00:00:00Z')",
                params![fixture.project_id],
            )
            .unwrap();

        fixture.catalog.delete_project(&fixture.project_id).unwrap();
        assert!(project_named(&fixture.catalog, &fixture.project_id).is_none());
    }

    #[test]
    fn every_lifecycle_operation_refuses_an_unknown_project() {
        let fixture = fixture();
        assert!(
            fixture
                .catalog
                .project_storage_report("project_nope")
                .is_err()
        );
        assert!(
            fixture
                .catalog
                .clear_project_cache("project_nope", &[CacheKind::Proxies], |_, _| true)
                .is_err()
        );
        assert!(
            fixture
                .catalog
                .audit_project_removal("project_nope", &fixture.folder)
                .is_err()
        );
        assert!(fixture.catalog.delete_project("project_nope").is_err());
    }
}
