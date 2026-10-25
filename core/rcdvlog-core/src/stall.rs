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
    use std::io::Read;
    use std::process::Stdio;
    if is_cancelled() {
        bail!("{what} canceled");
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
    let stdout_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = child_stdout.read_to_end(&mut buffer);
        buffer
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = child_stderr.read_to_end(&mut buffer);
        buffer
    });

    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            // The child has exited, so both pipes are closed and these joins
            // cannot block.
            return Ok(std::process::Output {
                status,
                stdout: stdout_reader.join().unwrap_or_default(),
                stderr: stderr_reader.join().unwrap_or_default(),
            });
        }
        if is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            bail!("{what} canceled");
        }
        if started.elapsed() >= limit {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "{what} stopped answering after {}s and was killed — the volume it reads from is no longer responding",
                limit.as_secs()
            );
        }
        std::thread::sleep(POLL_INTERVAL);
    }
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
