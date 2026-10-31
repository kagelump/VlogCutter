//! Giving up on I/O that has stopped answering.
//!
//! Every read in the import path lands on a removable volume, and a volume
//! that goes away mid-operation does not reliably return an error. Sleep/wake,
//! an enclosure dropping off the bus, or a userspace (FSKit) filesystem losing
//! its daemon can leave a thread parked in a read that simply never returns.
//! `std` offers no read timeout for regular files, so the only way to stay
//! answerable is to do the work somewhere abandonable and stop waiting for it.
//!
//! The worker thread is deliberately **not** killed: a thread blocked in the
//! kernel cannot be. It is abandoned, and exits on its own once the volume
//! errors out or comes back. A timeout is therefore a reason to stop the run —
//! never to retry the same path in a loop, which would leak one parked thread
//! per attempt.

use anyhow::{Context, Result, bail};
use std::cell::Cell;
use std::sync::mpsc;
use std::time::Duration;

/// How long a single filesystem operation may take before the volume under it
/// is presumed unresponsive. Generously above any real stat or short read,
/// including a cold spin-up, so a slow disk is never mistaken for a dead one.
pub(crate) const IO_STALL_TIMEOUT: Duration = Duration::from_secs(60);

/// How often a bounded child process is checked for exit. Short enough that a
/// kill lands promptly, long enough not to spin a core while ffprobe works.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(50);

thread_local! {
    static STALL_LIMIT: Cell<Duration> = const { Cell::new(IO_STALL_TIMEOUT) };
}

/// The stall limit in force on this thread.
pub(crate) fn io_stall_timeout() -> Duration {
    STALL_LIMIT.with(Cell::get)
}

/// Runs `body` under a shorter stall limit. The limit is thread-local, so a
/// test asserting on a stall costs milliseconds without shortening the limit
/// for tests running beside it.
#[cfg(test)]
pub(crate) fn with_io_stall_timeout<T>(limit: Duration, body: impl FnOnce() -> T) -> T {
    STALL_LIMIT.with(|cell| cell.set(limit));
    let outcome = body();
    STALL_LIMIT.with(|cell| cell.set(IO_STALL_TIMEOUT));
    outcome
}

/// Runs `work` on a worker thread, returning an error if it has not finished
/// within `limit`. `what` names the operation for the error message.
pub(crate) fn with_stall_timeout<T, F>(what: &str, limit: Duration, work: F) -> Result<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        // A closed receiver means we already gave up; the value is discarded
        // and the thread ends, which is the whole point of abandoning it.
        let _ = sender.send(work());
    });
    match receiver.recv_timeout(limit) {
        Ok(value) => Ok(value),
        Err(mpsc::RecvTimeoutError::Timeout) => bail!(
            "{what} stopped answering after {}s — the volume it reads from is no longer responding",
            limit.as_secs()
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            bail!("{what} ended without producing a result")
        }
    }
}

/// Runs filesystem/database work on one abandonable worker. The worker gets a
/// cooperative stop token; callers still return promptly if it is parked in a
/// kernel metadata/read operation. A timed-out attempt must not be retried on
/// the same path in a loop.
pub(crate) fn abandonable_io_with_timeout<T, F, C>(
    what: &str,
    limit: Duration,
    is_cancelled: &C,
    work: F,
) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(std::sync::Arc<std::sync::atomic::AtomicBool>) -> Result<T> + Send + 'static,
    C: Fn() -> bool + ?Sized,
{
    use std::sync::atomic::{AtomicBool, Ordering};
    if is_cancelled() {
        bail!("{what} canceled");
    }
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(work(worker_stop));
    });
    let started = std::time::Instant::now();
    loop {
        if is_cancelled() {
            stop.store(true, Ordering::Relaxed);
            bail!("{what} canceled");
        }
        match receiver.recv_timeout(POLL_INTERVAL) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("{what} ended without producing a result")
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if started.elapsed() >= limit {
            stop.store(true, Ordering::Relaxed);
            bail!("{what} stopped answering after {}s", limit.as_secs());
        }
    }
}

/// Runs `command` to completion, killing it if it outlives `limit`.
///
/// A media tool pointed at a volume that stopped answering inherits the stall:
/// it parks in its own read, and `Command::output` waits on it forever. The
/// child is killed and reaped on timeout, so a stalled import leaves no
/// orphaned `ffprobe`/`exiftool` behind.
pub(crate) fn output_with_timeout(
    command: &mut std::process::Command,
    what: &str,
    limit: Duration,
) -> Result<std::process::Output> {
    output_with_timeout_and_cancellation(command, what, limit, &|| false)
}

pub(crate) fn output_with_timeout_and_cancellation(
    command: &mut std::process::Command,
    what: &str,
    limit: Duration,
    is_cancelled: &impl Fn() -> bool,
) -> Result<std::process::Output> {
    output_with_timeout_cancellation_and_limit(command, what, limit, is_cancelled, usize::MAX)
}

pub(crate) fn output_with_timeout_cancellation_and_limit(
    command: &mut std::process::Command,
    what: &str,
    limit: Duration,
    is_cancelled: &impl Fn() -> bool,
    max_output_bytes: usize,
) -> Result<std::process::Output> {
    use std::io::Read;
    use std::process::Stdio;
    if is_cancelled() {
        bail!("{what} canceled");
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("start {what}"))?;

    // Drain both pipes on their own threads. Polling `try_wait` while a child
    // fills a pipe it cannot flush would deadlock the wait we are here to
    // bound, and turn a chatty tool into the very hang this guards against.
    let mut child_stdout = child.stdout.take().expect("stdout was piped");
    let mut child_stderr = child.stderr.take().expect("stderr was piped");
    let output_exceeded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stdout_exceeded = output_exceeded.clone();
    let stdout_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 16384];
        loop {
            match child_stdout.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    let room = max_output_bytes.saturating_sub(buffer.len());
                    buffer.extend_from_slice(&chunk[..count.min(room)]);
                    if count > room {
                        stdout_exceeded.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }
        buffer
    });
    let stderr_exceeded = output_exceeded.clone();
    let stderr_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 16384];
        loop {
            match child_stderr.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    let room = max_output_bytes.saturating_sub(buffer.len());
                    buffer.extend_from_slice(&chunk[..count.min(room)]);
                    if count > room {
                        stderr_exceeded.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }
        buffer
    });

    let started = std::time::Instant::now();
    let deadline = started + limit;
    let mut status = None;
    loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(observed) => status = observed,
                Err(error) => {
                    kill_child_tree(&mut child);
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err(error.into());
                }
            }
        }
        if status.is_some() && stdout_reader.is_finished() && stderr_reader.is_finished() {
            let stdout = stdout_reader.join().unwrap_or_default();
            let stderr = stderr_reader.join().unwrap_or_default();
            if output_exceeded.load(std::sync::atomic::Ordering::Relaxed) {
                bail!("{what} exceeded its {max_output_bytes}-byte output limit");
            }
            return Ok(std::process::Output {
                status: status.expect("child exited"),
                stdout,
                stderr,
            });
        }
        if is_cancelled() {
            kill_child_tree(&mut child);
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            bail!("{what} canceled");
        }
        if output_exceeded.load(std::sync::atomic::Ordering::Relaxed) {
            kill_child_tree(&mut child);
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            bail!("{what} exceeded its {max_output_bytes}-byte output limit and was killed");
        }
        if std::time::Instant::now() >= deadline {
            kill_child_tree(&mut child);
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            bail!(
                "{what} stopped answering after {}s and was killed — the volume it reads from is no longer responding",
                limit.as_secs()
            );
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn kill_child_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    unsafe {
        let pid = child.id() as i32;
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_that_exits_in_time_returns_its_output() {
        let mut command = std::process::Command::new("echo");
        command.arg("hello");
        let output = output_with_timeout(&mut command, "echo", Duration::from_secs(5)).unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
    }

    #[test]
    fn a_chatty_tool_is_killed_when_bounded_output_is_exceeded() {
        let mut command = std::process::Command::new("echo");
        command.arg("this output is deliberately too long");
        let error = output_with_timeout_cancellation_and_limit(
            &mut command,
            "bounded echo",
            Duration::from_secs(5),
            &|| false,
            4,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("output limit"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn descendants_cannot_keep_inspection_pipes_open_after_timeout_or_cancel() {
        fn spawn_pipe_holder(pid_path: &std::path::Path) -> std::process::Command {
            let mut command = std::process::Command::new("sh");
            command
                .arg("-c")
                .arg("sleep 30 & echo $! > \"$1\"; exit 0")
                .arg("sh")
                .arg(pid_path);
            command
        }
        fn assert_gone(pid_path: &std::path::Path) {
            let pid: i32 = std::fs::read_to_string(pid_path)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                let status = std::process::Command::new("ps")
                    .args(["-o", "stat=", "-p"])
                    .arg(pid.to_string())
                    .output()
                    .unwrap();
                let state = String::from_utf8_lossy(&status.stdout).trim().to_owned();
                if state.is_empty() || state.starts_with('Z') {
                    return;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "sleep descendant {pid} survived group termination"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let timeout_pid = directory.path().join("timeout-pid");
        let mut command = spawn_pipe_holder(&timeout_pid);
        let started = std::time::Instant::now();
        let error = output_with_timeout_and_cancellation(
            &mut command,
            "pipe holder timeout",
            Duration::from_millis(250),
            &|| false,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("stopped answering"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_gone(&timeout_pid);

        let cancel_pid = directory.path().join("cancel-pid");
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let error = std::thread::scope(|scope| {
            scope.spawn(|| {
                let deadline = std::time::Instant::now() + Duration::from_secs(2);
                while !cancel_pid.is_file() && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
            });
            let mut command = spawn_pipe_holder(&cancel_pid);
            output_with_timeout_and_cancellation(
                &mut command,
                "pipe holder cancellation",
                Duration::from_secs(5),
                &|| cancelled.load(std::sync::atomic::Ordering::Relaxed),
            )
            .unwrap_err()
            .to_string()
        });
        assert!(error.contains("canceled"), "{error}");
        assert_gone(&cancel_pid);
    }

    #[cfg(unix)]
    #[test]
    fn a_tool_that_never_exits_is_killed_rather_than_waited_on_forever() {
        let started = std::time::Instant::now();
        let mut command = std::process::Command::new("sleep");
        command.arg("300");

        let error = output_with_timeout(&mut command, "sleep", Duration::from_millis(300))
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("stopped answering"),
            "the error must name the stall, got: {error}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "waited far longer than the limit asked for"
        );
    }

    #[test]
    fn work_that_finishes_within_the_limit_returns_its_value() {
        let value = with_stall_timeout("a quick sum", Duration::from_secs(5), || 2 + 2).unwrap();
        assert_eq!(value, 4);
    }

    #[cfg(unix)]
    #[test]
    fn abandonable_source_observation_returns_on_timeout_and_cancellation() {
        fn fifo(path: &std::path::Path) {
            assert!(
                std::process::Command::new("mkfifo")
                    .arg(path)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        fn release_reader(path: &std::path::Path) {
            // Opening a writer releases the test worker from its blocked FIFO
            // open so this fixture itself does not leak an abandoned thread.
            drop(std::fs::OpenOptions::new().write(true).open(path).unwrap());
            std::thread::sleep(Duration::from_millis(50));
        }
        let directory = tempfile::tempdir().unwrap();
        let timed_out = directory.path().join("timeout-fifo");
        fifo(&timed_out);
        let worker_path = timed_out.clone();
        let error = abandonable_io_with_timeout(
            "inspection source stat",
            Duration::from_millis(100),
            &|| false,
            move |_| {
                std::fs::read(worker_path)?;
                Ok(())
            },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("stopped answering"), "{error}");
        release_reader(&timed_out);

        let cancelled_path = directory.path().join("cancel-fifo");
        fifo(&cancelled_path);
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let worker_path = cancelled_path.clone();
        let error = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(100));
                cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
            });
            abandonable_io_with_timeout(
                "inspection source stat",
                Duration::from_secs(2),
                &|| cancelled.load(std::sync::atomic::Ordering::Relaxed),
                move |_| {
                    std::fs::read(worker_path)?;
                    Ok(())
                },
            )
            .unwrap_err()
            .to_string()
        });
        assert!(error.contains("canceled"), "{error}");
        release_reader(&cancelled_path);
    }

    /// Reading a FIFO that has no writer is the one blocking read a test can
    /// create on demand: the open parks until a writer appears, exactly the
    /// way a read from a volume that stopped answering does.
    #[cfg(unix)]
    #[test]
    fn a_read_that_never_returns_gives_up_instead_of_blocking_forever() {
        let directory = tempfile::tempdir().unwrap();
        let fifo = directory.path().join("unresponsive");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success(),
            "could not create the FIFO this test needs"
        );

        let started = std::time::Instant::now();
        let outcome =
            with_stall_timeout("read unresponsive", Duration::from_millis(300), move || {
                std::fs::read(&fifo)
            });

        let error = outcome.unwrap_err().to_string();
        assert!(
            error.contains("stopped answering"),
            "the error must name the stall, got: {error}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "gave up far later than the limit asked for"
        );
    }
}
