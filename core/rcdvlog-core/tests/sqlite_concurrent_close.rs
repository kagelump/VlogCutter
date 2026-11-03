//! Regression guard for #258.
//!
//! The catalog opens a fresh SQLite connection per operation, so worker threads
//! constantly open and close connections to the *same* WAL database file.
//! SQLite 3.51.1 (bundled by `libsqlite3-sys` 0.36 / `rusqlite` 0.38) had a
//! lock-order inversion in its unix VFS: when two connections to one WAL
//! database were closed at the same time, one thread held the per-inode lock
//! mutex while waiting for the global VFS mutex (`unixIsSharingShmNode`), and
//! the other held the global VFS mutex while waiting for the per-inode one
//! (`unixClose`). The process then hung forever with every thread parked on a
//! futex, which is what burned the CI job timeout in
//! `analysis::native_video::queue_tests`. SQLite 3.51.3 removed the global-mutex
//! acquisition from that path.
//!
//! This test closes pairs of connections to one WAL database at the same
//! instant, round after round, directly against the bundled library. The wait is
//! bounded, so a regression (or a dependency downgrade) fails this named test in
//! a minute instead of hanging the whole run.

use rusqlite::Connection;
use std::sync::{Arc, Barrier, mpsc};
use std::thread;
use std::time::Duration;

const THREADS: usize = 2;
const ROUNDS: usize = 10_000;
const LIMIT: Duration = Duration::from_secs(90);

#[test]
fn simultaneous_close_of_connections_to_one_wal_database_does_not_deadlock() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite3");
    {
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        connection
            .execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY, n INTEGER);")
            .unwrap();
    }

    let barrier = Arc::new(Barrier::new(THREADS));
    let (done_tx, done_rx) = mpsc::channel::<usize>();
    for worker in 0..THREADS {
        let path = path.clone();
        let barrier = Arc::clone(&barrier);
        let done = done_tx.clone();
        thread::spawn(move || {
            for round in 0..ROUNDS {
                // Mirrors `Catalog::connection()`: open, pragma, short use.
                let connection = Connection::open(&path).unwrap();
                connection
                    .pragma_update(None, "journal_mode", "WAL")
                    .unwrap();
                connection.busy_timeout(Duration::from_secs(30)).unwrap();
                let _: i64 = connection
                    .query_row("SELECT count(*) FROM t", [], |row| row.get(0))
                    .unwrap();
                let _ = round;
                // Release both closes together.
                barrier.wait();
                drop(connection);
                barrier.wait();
            }
            let _ = done.send(worker);
        });
    }
    drop(done_tx);

    let deadline = std::time::Instant::now() + LIMIT;
    let mut finished = 0;
    while finished < THREADS {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match done_rx.recv_timeout(remaining) {
            Ok(_) => finished += 1,
            Err(error) => panic!(
                "SQLite connection close deadlocked or stalled: {finished}/{THREADS} workers \
                 finished within {}s ({error}); see #258 and the bundled SQLite version",
                LIMIT.as_secs()
            ),
        }
    }
}
