//! Per-project registry of detached background work.
//!
//! Some desktop writers outlive the command that started them and never set
//! the frontend `busy` flag: the post-import Q1 thumbnail task, sync-manager
//! proxy downloads, and node-targeted proxy/thumbnail jobs. Each keeps a
//! `Catalog` clone pointing into the project folder, so deleting that folder
//! underneath them resurrects cache directories and ghost-writes against a
//! removed catalog.
//!
//! The registry makes that work visible at the backend boundary. A writer
//! registers with [`ProjectWork::begin`] and holds the returned guard for as
//! long as it may touch the project folder. [`ProjectWork::begin_delete`]
//! refuses while any guard is live and otherwise tombstones the project in the
//! same critical section, so a writer that arrives afterwards is refused
//! before it can recreate a cache directory. Refusing is deliberate: it is
//! the explicit integrity conflict the user can retry, and it needs no
//! cooperative cancellation protocol in the media writers.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

/// What kind of detached work is holding a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum WorkKind {
    /// The detached post-import Q1 thumbnail task.
    ImportThumbnails,
    /// Sync-manager download of server proxies.
    ProxyDownload,
    /// A server-issued node-targeted proxy/thumbnail job.
    NodeJobs,
}

impl WorkKind {
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Self::ImportThumbnails => "post-import thumbnail generation",
            Self::ProxyDownload => "server proxy download",
            Self::NodeJobs => "server-requested proxy/thumbnail jobs",
        }
    }
}

/// Why [`ProjectWork::begin`] declined to register work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProjectDeleted;

/// Why [`ProjectWork::begin_delete`] refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkStillRunning(pub Vec<WorkKind>);

impl fmt::Display for WorkStillRunning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kinds: Vec<&str> = self.0.iter().map(|kind| kind.describe()).collect();
        write!(
            f,
            "Cannot delete this project while background work is still running: {}. \
             Wait for it to finish, then try again.",
            kinds.join(", ")
        )
    }
}

impl std::error::Error for WorkStillRunning {}

#[derive(Default)]
struct Inner {
    active: BTreeMap<String, BTreeMap<WorkKind, usize>>,
    tombstones: HashSet<String>,
}

#[derive(Clone, Default)]
pub(crate) struct ProjectWork {
    inner: Arc<Mutex<Inner>>,
}

/// Held by a background writer for as long as it may touch the project folder.
#[must_use = "dropping the guard immediately unregisters the work"]
pub(crate) struct WorkGuard {
    registry: ProjectWork,
    project_id: String,
    kind: WorkKind,
}

impl Drop for WorkGuard {
    fn drop(&mut self) {
        let mut inner = self.registry.lock();
        if let Some(kinds) = inner.active.get_mut(&self.project_id) {
            if let Some(count) = kinds.get_mut(&self.kind) {
                *count -= 1;
                if *count == 0 {
                    kinds.remove(&self.kind);
                }
            }
            if kinds.is_empty() {
                inner.active.remove(&self.project_id);
            }
        }
    }
}

impl ProjectWork {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Registers work against a project. Refused once the project has been
    /// tombstoned, so the caller must not create cache directories or start
    /// ffmpeg for it.
    pub(crate) fn begin(
        &self,
        project_id: &str,
        kind: WorkKind,
    ) -> Result<WorkGuard, ProjectDeleted> {
        let mut inner = self.lock();
        if inner.tombstones.contains(project_id) {
            return Err(ProjectDeleted);
        }
        *inner
            .active
            .entry(project_id.to_string())
            .or_default()
            .entry(kind)
            .or_insert(0) += 1;
        drop(inner);
        Ok(WorkGuard {
            registry: self.clone(),
            project_id: project_id.to_string(),
            kind,
        })
    }

    /// Kinds of work currently running for a project.
    #[cfg(test)]
    pub(crate) fn active(&self, project_id: &str) -> Vec<WorkKind> {
        self.lock()
            .active
            .get(project_id)
            .map(|kinds| kinds.keys().copied().collect())
            .unwrap_or_default()
    }

    /// Atomically checks that no work is running and tombstones the project.
    /// The check and the tombstone share one critical section, so no writer
    /// can slip in between them.
    pub(crate) fn begin_delete(&self, project_id: &str) -> Result<(), WorkStillRunning> {
        let mut inner = self.lock();
        if let Some(kinds) = inner.active.get(project_id) {
            return Err(WorkStillRunning(kinds.keys().copied().collect()));
        }
        inner.tombstones.insert(project_id.to_string());
        Ok(())
    }

    /// Lifts a tombstone when a delete did not go through, so the project can
    /// resume background work.
    pub(crate) fn abort_delete(&self, project_id: &str) {
        self.lock().tombstones.remove(project_id);
    }

    #[cfg(test)]
    pub(crate) fn is_tombstoned(&self, project_id: &str) -> bool {
        self.lock().tombstones.contains(project_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_is_refused_while_work_runs_and_names_it() {
        let work = ProjectWork::default();
        let guard = work.begin("p1", WorkKind::ImportThumbnails).unwrap();
        let other = work.begin("p1", WorkKind::NodeJobs).unwrap();
        let error = work.begin_delete("p1").unwrap_err();
        assert_eq!(
            error.0,
            vec![WorkKind::ImportThumbnails, WorkKind::NodeJobs]
        );
        let message = error.to_string();
        assert!(message.contains("post-import thumbnail generation"));
        assert!(message.contains("still running"));
        // A refused delete must not tombstone: the project stays usable.
        assert!(!work.is_tombstoned("p1"));
        assert!(work.begin("p1", WorkKind::ProxyDownload).is_ok());

        drop(guard);
        assert!(work.begin_delete("p1").is_err());
        drop(other);
        assert!(work.begin_delete("p1").is_ok());
    }

    #[test]
    fn other_projects_do_not_block_delete() {
        let work = ProjectWork::default();
        let _guard = work.begin("p2", WorkKind::ProxyDownload).unwrap();
        assert!(work.begin_delete("p1").is_ok());
        assert_eq!(work.active("p2"), vec![WorkKind::ProxyDownload]);
    }

    #[test]
    fn work_arriving_after_the_tombstone_is_refused_until_aborted() {
        let work = ProjectWork::default();
        work.begin_delete("p1").unwrap();
        assert_eq!(
            work.begin("p1", WorkKind::NodeJobs).err(),
            Some(ProjectDeleted)
        );
        work.abort_delete("p1");
        assert!(work.begin("p1", WorkKind::NodeJobs).is_ok());
    }

    #[test]
    fn guards_count_per_kind_and_release_on_drop() {
        let work = ProjectWork::default();
        let first = work.begin("p1", WorkKind::NodeJobs).unwrap();
        let second = work.begin("p1", WorkKind::NodeJobs).unwrap();
        drop(first);
        assert_eq!(work.active("p1"), vec![WorkKind::NodeJobs]);
        drop(second);
        assert!(work.active("p1").is_empty());
    }
}
