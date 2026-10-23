//! Host-local serialization for local-heavy media work.
//!
//! #53 accepted one shared local-heavy operation slot across FFmpeg-heavy
//! proxy/transcode, full-hash, speech transcription, calibration and future
//! local VLM work. Safe cloud, lightweight metadata, review, and import work
//! may overlap. This module supplies:
//!
//! * an advisory same-host/same-user filesystem lock so separate CLI/server
//!   processes sharing that filesystem namespace do not silently create a
//!   second heavy lane; and
//! * an intentional process-local LIFO wait queue so newer explicit work is
//!   preferred and every waiter is cancellable.
//!
//! Ordering across independent processes is *not* a distributed/global LIFO
//! guarantee. It is only LIFO among waiters in this process; the filesystem
//! lease remains the cross-process exclusion boundary.

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::thread::ThreadId;
use std::time::Duration;

const RESOURCE_LOCK_DIRECTORY: &str = "rcdvlog-resource-locks";
const LOCAL_HEAVY_FILENAME: &str = "local-heavy.lock";
const WAIT_POLL: Duration = Duration::from_millis(10);

#[derive(Debug, thiserror::Error)]
#[error("local-heavy resource acquisition canceled")]
pub struct ResourceWaitCancelled;

#[derive(Debug, Default)]
struct WaitQueueState {
    next_ticket: u64,
    waiting: VecDeque<u64>,
}

#[derive(Debug)]
struct ProcessWaitQueue {
    state: Mutex<WaitQueueState>,
    changed: Condvar,
}

fn held_by_threads() -> &'static Mutex<HashMap<PathBuf, ThreadId>> {
    static HELD: OnceLock<Mutex<HashMap<PathBuf, ThreadId>>> = OnceLock::new();
    HELD.get_or_init(|| Mutex::new(HashMap::new()))
}

fn reject_nested_acquisition(path: &Path) -> Result<()> {
    let held = held_by_threads()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if held.get(path) == Some(&std::thread::current().id()) {
        bail!(
            "nested local-heavy acquisition on the same thread is not allowed; hold one lease around batch internals"
        );
    }
    Ok(())
}

fn mark_acquired(path: &Path) {
    held_by_threads()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(path.to_path_buf(), std::thread::current().id());
}

fn mark_released(path: &Path) {
    let mut held = held_by_threads()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    held.remove(path);
}

fn wait_queues() -> &'static Mutex<HashMap<PathBuf, Weak<ProcessWaitQueue>>> {
    static QUEUES: OnceLock<Mutex<HashMap<PathBuf, Weak<ProcessWaitQueue>>>> = OnceLock::new();
    QUEUES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn wait_queue_for(path: &Path) -> Arc<ProcessWaitQueue> {
    let mut queues = wait_queues()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    queues.retain(|_, queue| queue.strong_count() > 0);
    if let Some(queue) = queues.get(path).and_then(Weak::upgrade) {
        return queue;
    }
    let queue = Arc::new(ProcessWaitQueue {
        state: Mutex::new(WaitQueueState::default()),
        changed: Condvar::new(),
    });
    queues.insert(path.to_path_buf(), Arc::downgrade(&queue));
    queue
}

fn enqueue(queue: &ProcessWaitQueue) -> u64 {
    let mut state = queue
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let ticket = state.next_ticket;
    state.next_ticket = state.next_ticket.saturating_add(1);
    state.waiting.push_back(ticket);
    queue.changed.notify_all();
    ticket
}

fn remove_ticket(queue: &ProcessWaitQueue, ticket: u64) {
    let mut state = queue
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.waiting.retain(|candidate| *candidate != ticket);
    queue.changed.notify_all();
}

#[derive(Debug)]
pub struct LocalHeavyLease {
    file: File,
    path: PathBuf,
    queue: Arc<ProcessWaitQueue>,
}

impl LocalHeavyLease {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn scope(&self) -> &'static str {
        "same-host same-user filesystem lease with process-local LIFO waiting; not a distributed cluster lock"
    }
}

impl Drop for LocalHeavyLease {
    fn drop(&mut self) {
        // Release the OS lease before waking waiters so the newest waiter can
        // immediately claim it. If another process wins the race, our waiters
        // remain queued and polling rather than being falsely granted.
        // Clear ownership before releasing the OS lease. Otherwise a newly
        // admitted holder could register itself and be erased by this drop.
        mark_released(&self.path);
        let _ = self.file.unlock();
        self.queue.changed.notify_all();
    }
}

/// Default host-wide lock path shared by CLI and server processes for this
/// user. `RCDVLOG_RESOURCE_LOCK_ROOT` overrides the root for tests or an
/// explicitly managed deployment.
pub fn default_local_heavy_lock_path() -> PathBuf {
    let root = std::env::var_os("RCDVLOG_RESOURCE_LOCK_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(RESOURCE_LOCK_DIRECTORY));
    #[cfg(unix)]
    let owner = unsafe { libc::getuid() }.to_string();
    #[cfg(not(unix))]
    let owner = "user".to_string();
    root.join(format!("{owner}-{LOCAL_HEAVY_FILENAME}"))
}

fn open_lock(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create resource lock directory {}", parent.display()))?;
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("open local-heavy resource lock {}", path.display()))
}

fn try_lock_file(file: &File) -> Result<bool> {
    match file.try_lock_exclusive() {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
        Err(error) => Err(error).context("acquire local-heavy resource lock"),
    }
}

/// Try to own the one shared local-heavy slot without waiting. This deliberately
/// bypasses the wait queue: an opportunistic caller either gets the slot now or
/// leaves immediately. Serializing ordinary operations goes through
/// `acquire_local_heavy_at`/`_cancellable` and therefore participates in LIFO.
pub fn try_acquire_local_heavy_at(path: &Path) -> Result<Option<LocalHeavyLease>> {
    let file = open_lock(path)?;
    if try_lock_file(&file)? {
        mark_acquired(path);
        Ok(Some(LocalHeavyLease {
            file,
            path: path.to_path_buf(),
            queue: wait_queue_for(path),
        }))
    } else {
        Ok(None)
    }
}

/// Wait for the one shared local-heavy slot. The lockfile intentionally
/// persists after release; deleting it could split ownership across processes.
pub fn acquire_local_heavy_at(path: &Path) -> Result<LocalHeavyLease> {
    acquire_local_heavy_at_cancellable(path, || false)
}

/// Wait for the one shared local-heavy slot while remaining cancellation-aware.
///
/// Waiters are served LIFO within this process. The closure is polled while
/// queued and while waiting for a cross-process holder; cancellation removes the
/// waiter immediately and never leaks a permit. Ordering across processes is
/// not globally LIFO because the filesystem lease is the only cross-process
/// primitive available here.
pub fn acquire_local_heavy_at_cancellable<F>(
    path: &Path,
    mut is_cancelled: F,
) -> Result<LocalHeavyLease>
where
    F: FnMut() -> bool,
{
    if is_cancelled() {
        return Err(ResourceWaitCancelled.into());
    }
    reject_nested_acquisition(path)?;
    let queue = wait_queue_for(path);
    let ticket = enqueue(&queue);
    let file = match open_lock(path) {
        Ok(file) => file,
        Err(error) => {
            remove_ticket(&queue, ticket);
            return Err(error);
        }
    };
    let mut state = queue
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    loop {
        // Progress/cancellation callbacks may read other domain state. Never
        // invoke them while holding the queue mutex.
        drop(state);
        let canceled = is_cancelled();
        state = queue
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if canceled {
            state.waiting.retain(|candidate| *candidate != ticket);
            queue.changed.notify_all();
            return Err(ResourceWaitCancelled.into());
        }
        if state.waiting.back() == Some(&ticket) {
            // The try-lock is nonblocking. Hold the queue mutex across this
            // decision so a newly enqueued ticket cannot be bypassed between
            // checking LIFO position and claiming the resource.
            match try_lock_file(&file) {
                Ok(true) => {
                    state.waiting.retain(|candidate| *candidate != ticket);
                    queue.changed.notify_all();
                    mark_acquired(path);
                    return Ok(LocalHeavyLease {
                        file,
                        path: path.to_path_buf(),
                        queue: Arc::clone(&queue),
                    });
                }
                Ok(false) => {
                    // A holder in another process owns the filesystem lease.
                    // Keep LIFO position while polling so cancellation and new
                    // in-process work remain responsive.
                }
                Err(error) => {
                    state.waiting.retain(|candidate| *candidate != ticket);
                    queue.changed.notify_all();
                    return Err(error);
                }
            }
            let (next, _) = queue
                .changed
                .wait_timeout(state, WAIT_POLL)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
        } else {
            let (next, _) = queue
                .changed
                .wait_timeout(state, WAIT_POLL)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
        }
    }
}

impl crate::Catalog {
    pub fn local_heavy_lock_path(&self) -> PathBuf {
        #[cfg(test)]
        {
            self.database_path().with_extension("local-heavy.test.lock")
        }
        #[cfg(not(test))]
        {
            default_local_heavy_lock_path()
        }
    }

    /// Try to claim the shared local-heavy slot. Contention is `Ok(None)`.
    pub fn try_acquire_local_heavy(&self) -> Result<Option<LocalHeavyLease>> {
        if self.read_only {
            bail!("cannot acquire a local-heavy lease from a read-only catalog");
        }
        try_acquire_local_heavy_at(&self.local_heavy_lock_path())
    }

    /// Wait for the shared local-heavy slot. Callers must not nested-acquire
    /// while already holding a lease; batch entrypoints acquire it once around
    /// their internal workers.
    pub fn acquire_local_heavy(&self) -> Result<LocalHeavyLease> {
        if self.read_only {
            bail!("cannot acquire a local-heavy lease from a read-only catalog");
        }
        acquire_local_heavy_at(&self.local_heavy_lock_path())
    }

    /// Wait cooperatively for one public batch operation. Throttle progress
    /// callbacks while queued; a canceled waiter never becomes a running job.
    pub(crate) fn wait_for_local_heavy<F>(
        &self,
        mut should_cancel: F,
    ) -> Result<Option<LocalHeavyLease>>
    where
        F: FnMut() -> bool,
    {
        // An uncontended operation has not waited and must retain its normal
        // progress contract. New explicit work is also the newest LIFO intent.
        if let Some(lease) = self.try_acquire_local_heavy()? {
            return Ok(Some(lease));
        }
        let mut last_poll = None;
        match self.acquire_local_heavy_cancellable(|| {
            if last_poll
                .is_some_and(|last: std::time::Instant| last.elapsed() < Duration::from_millis(100))
            {
                return false;
            }
            last_poll = Some(std::time::Instant::now());
            should_cancel()
        }) {
            Ok(lease) => Ok(Some(lease)),
            Err(error) if error.is::<ResourceWaitCancelled>() => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Cancellation-aware form for long-running public operations.
    pub fn acquire_local_heavy_cancellable<F>(&self, is_cancelled: F) -> Result<LocalHeavyLease>
    where
        F: FnMut() -> bool,
    {
        if self.read_only {
            bail!("cannot acquire a local-heavy lease from a read-only catalog");
        }
        acquire_local_heavy_at_cancellable(&self.local_heavy_lock_path(), is_cancelled)
    }
}

#[cfg(test)]
fn waiting_count_at(path: &Path) -> usize {
    wait_queue_for(path)
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .waiting
        .len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::thread;
    use tempfile::tempdir;

    #[test]
    fn one_shared_heavy_lease_excludes_a_second_caller() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("heavy.lock");
        let first = try_acquire_local_heavy_at(&path).unwrap();
        assert!(first.is_some());
        assert!(try_acquire_local_heavy_at(&path).unwrap().is_none());
        drop(first);
        assert!(try_acquire_local_heavy_at(&path).unwrap().is_some());
    }

    #[test]
    fn cancellable_acquisition_stops_waiting_without_owning_the_slot() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("heavy.lock");
        let held = try_acquire_local_heavy_at(&path).unwrap().unwrap();
        let error = acquire_local_heavy_at_cancellable(&path, || true).unwrap_err();
        assert!(error.to_string().contains("canceled"));
        assert_eq!(waiting_count_at(&path), 0);
        drop(held);
    }

    #[test]
    fn waiters_are_served_lifo_and_cancel_cleanup_is_visible() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("heavy.lock");
        let held = try_acquire_local_heavy_at(&path).unwrap().unwrap();
        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let (started_tx, started_rx) = mpsc::channel();

        let mut handles = Vec::new();
        for name in ["older", "newer"] {
            let worker_path = path.clone();
            let order = Arc::clone(&order);
            let started_tx = started_tx.clone();
            handles.push(thread::spawn(move || {
                started_tx.send(()).unwrap();
                let lease = acquire_local_heavy_at(&worker_path).unwrap();
                order.lock().unwrap().push(name);
                drop(lease);
            }));
            started_rx.recv().unwrap();
            let expected = handles.len();
            for _ in 0..200 {
                if waiting_count_at(&path) == expected {
                    break;
                }
                thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(waiting_count_at(&path), expected);
        }
        // Wait until both have actually joined the process-local queue.
        for _ in 0..200 {
            if waiting_count_at(&path) == 2 {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(waiting_count_at(&path), 2);
        drop(held);
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(&*order.lock().unwrap(), &["newer", "older"]);
    }

    #[test]
    fn panicking_holder_releases_the_shared_slot() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("heavy.lock");
        let panicking_path = path.clone();
        let result = thread::spawn(move || {
            let _lease = acquire_local_heavy_at(&panicking_path).unwrap();
            panic!("synthetic heavy work failure");
        })
        .join();
        assert!(result.is_err());
        let lease = acquire_local_heavy_at(&path).expect("RAII release must survive a panic");
        assert!(lease.path().ends_with("heavy.lock"));
    }

    #[test]
    fn lightweight_metadata_work_can_overlap_the_held_heavy_slot() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("heavy.lock");
        let catalog_path = directory.path().join("catalog.sqlite3");
        let _catalog = crate::Catalog::open(&catalog_path).unwrap();
        let _heavy = acquire_local_heavy_at(&path).unwrap();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            // No local-heavy acquisition here: SQLite/model metadata reads are
            // safe to overlap and must not wait behind the heavy slot.
            let read_catalog = crate::Catalog::open_read_only_existing(&catalog_path).unwrap();
            sender.send(read_catalog.model_status().is_ok()).unwrap();
        });
        assert!(receiver.recv_timeout(Duration::from_secs(2)).unwrap());
    }

    #[test]
    fn nested_waiting_acquisition_fails_fast_instead_of_deadlocking() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("heavy.lock");
        let _held = acquire_local_heavy_at(&path).unwrap();
        let error = acquire_local_heavy_at(&path).unwrap_err();
        assert!(error.to_string().contains("nested local-heavy"));
    }

    #[test]
    fn cancellation_removes_only_the_canceled_waiter() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("heavy.lock");
        let held = try_acquire_local_heavy_at(&path).unwrap().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let waiter_path = path.clone();
        let waiter_cancel = Arc::clone(&cancel);
        let waiter = thread::spawn(move || {
            acquire_local_heavy_at_cancellable(&waiter_path, || {
                waiter_cancel.load(Ordering::SeqCst)
            })
        });
        for _ in 0..200 {
            if waiting_count_at(&path) == 1 {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        cancel.store(true, Ordering::SeqCst);
        assert!(waiter.join().unwrap().is_err());
        assert_eq!(waiting_count_at(&path), 0);
        drop(held);
    }

    #[test]
    fn catalog_lease_contention_is_explicit() {
        let directory = tempdir().unwrap();
        let catalog = crate::Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let path = directory.path().join("owned.lock");
        let held = acquire_local_heavy_at(&path).unwrap();
        assert_eq!(
            held.scope(),
            "same-host same-user filesystem lease with process-local LIFO waiting; not a distributed cluster lock"
        );
        let _ = catalog;
    }
}
