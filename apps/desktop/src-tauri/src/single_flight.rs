//! At most one detached background task, started without being awaited.
//!
//! The sync loop must keep reconciling while a server-issued node job (a
//! minutes-long ffmpeg encode) runs. The loop therefore hands such work to a
//! `SingleFlight` slot instead of awaiting it: `try_spawn` returns at once,
//! refuses a second task while one is in flight, and exposes a cooperative
//! cancellation flag the task observes between units of work. The slot also
//! remembers which catalog the task was pinned to, so the loop can cancel it
//! when the open catalog changes.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::task::JoinHandle;

/// Cooperative cancellation shared with the running task.
#[derive(Clone, Default, Debug)]
pub(crate) struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct Running {
    handle: JoinHandle<()>,
    cancel: CancelFlag,
    pinned_catalog: PathBuf,
}

#[derive(Default)]
pub(crate) struct SingleFlight {
    running: Option<Running>,
}

impl SingleFlight {
    /// True while a spawned task has not finished.
    pub(crate) fn is_running(&self) -> bool {
        self.running
            .as_ref()
            .is_some_and(|running| !running.handle.is_finished())
    }

    /// Starts `make(cancel)` on the runtime and returns immediately, or
    /// returns `false` without calling `make` when a task is already in
    /// flight. Must be called from within a Tokio runtime.
    pub(crate) fn try_spawn<F, Fut>(&mut self, pinned_catalog: &Path, make: F) -> bool
    where
        F: FnOnce(CancelFlag) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        if self.is_running() {
            return false;
        }
        let cancel = CancelFlag::default();
        let handle = tokio::spawn(make(cancel.clone()));
        self.running = Some(Running {
            handle,
            cancel,
            pinned_catalog: pinned_catalog.to_path_buf(),
        });
        true
    }

    /// Cancels a running task that was pinned to a catalog other than
    /// `current`. The task stops before its next unit of work; a unit already
    /// in progress (an ffmpeg process) finishes against its pinned catalog.
    pub(crate) fn cancel_unless_pinned_to(&mut self, current: &Path) {
        if let Some(running) = &self.running
            && running.pinned_catalog != current
        {
            running.cancel.cancel();
        }
    }

    /// Cancels any running task.
    pub(crate) fn cancel(&mut self) {
        if let Some(running) = &self.running {
            running.cancel.cancel();
        }
    }
}

impl Drop for SingleFlight {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::Notify;

    #[tokio::test]
    async fn spawn_returns_while_the_task_is_pending_and_admits_only_one() {
        let release = Arc::new(Notify::new());
        let mut slot = SingleFlight::default();
        let path = PathBuf::from("/catalog-a/catalog.sqlite3");

        let gate = release.clone();
        assert!(slot.try_spawn(&path, |_| async move { gate.notified().await }));
        // The caller was not blocked on the pending task.
        assert!(slot.is_running());
        let mut second_ran = false;
        let admitted = slot.try_spawn(&path, |_| {
            second_ran = true;
            async {}
        });
        assert!(!admitted);
        assert!(!second_ran, "a refused task is never constructed");

        release.notify_one();
        for _ in 0..100 {
            if !slot.is_running() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!slot.is_running());
        assert!(slot.try_spawn(&path, |_| async {}));
    }

    #[tokio::test]
    async fn a_catalog_change_cancels_the_task_but_the_same_catalog_does_not() {
        let mut slot = SingleFlight::default();
        let pinned = PathBuf::from("/catalog-a/catalog.sqlite3");
        let observed = Arc::new(std::sync::Mutex::new(None::<CancelFlag>));
        let sink = observed.clone();
        let release = Arc::new(Notify::new());
        let gate = release.clone();
        slot.try_spawn(&pinned, |cancel| async move {
            *sink.lock().unwrap() = Some(cancel);
            gate.notified().await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let cancel = observed.lock().unwrap().clone().unwrap();

        slot.cancel_unless_pinned_to(&pinned);
        assert!(!cancel.is_cancelled());
        slot.cancel_unless_pinned_to(Path::new("/catalog-b/catalog.sqlite3"));
        assert!(cancel.is_cancelled());
        release.notify_one();
    }

    #[tokio::test]
    async fn dropping_the_slot_cancels_the_task() {
        let observed = Arc::new(std::sync::Mutex::new(None::<CancelFlag>));
        let sink = observed.clone();
        let release = Arc::new(Notify::new());
        let gate = release.clone();
        let mut slot = SingleFlight::default();
        slot.try_spawn(Path::new("/c"), |cancel| async move {
            *sink.lock().unwrap() = Some(cancel);
            gate.notified().await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let cancel = observed.lock().unwrap().clone().unwrap();
        drop(slot);
        assert!(cancel.is_cancelled());
        release.notify_one();
    }
}
