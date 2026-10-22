use super::RemoteClient;
use anyhow::{Result, anyhow};
use clap::{Args, Subcommand};
use rcdvlog_server_core::{CallbackClaim, JobId, JobSnapshot, JobState};
use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, thiserror::Error)]
#[error("job agent operation failed ({code})")]
pub struct JobAgentError {
    pub code: &'static str,
    pub details: Value,
}

fn failure(code: &'static str, details: Value) -> anyhow::Error {
    JobAgentError { code, details }.into()
}

#[derive(Debug, Args)]
pub struct WaitArgs {
    #[arg(long)]
    job: String,
    /// Overall observation deadline; does not cancel or retry the job.
    #[arg(long, default_value_t = 1800, value_parser = clap::value_parser!(u64).range(0..=86400))]
    wait_seconds: u64,
    /// Polling happens inside this process, with no progress output.
    #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u64).range(10..=60000))]
    poll_interval_ms: u64,
}

#[derive(Debug, Subcommand)]
pub enum CallbackCommand {
    /// Subscribe an opaque consumer target to every terminal transition of a job.
    Register {
        #[arg(long)]
        job: String,
        #[arg(long)]
        target: String,
    },
    /// Inspect delivery receipts without revealing lease tokens.
    List {
        #[arg(long)]
        target: String,
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
        limit: u32,
        #[arg(long)]
        before: Option<u64>,
    },
    /// Wait for ONE delivery, execute a local program, then acknowledge it.
    /// The program receives a job.completion.v1 JSON envelope on stdin.
    Deliver {
        #[arg(long)]
        target: String,
        /// Absolute local executable path. Never sent to or run on the server.
        #[arg(long)]
        program: std::path::PathBuf,
        /// Repeatable literal argv entries; use --arg=--flag for leading hyphens.
        #[arg(long = "arg", allow_hyphen_values = false)]
        args: Vec<String>,
        #[arg(long, default_value_t = 1800, value_parser = clap::value_parser!(u64).range(0..=86400))]
        wait_seconds: u64,
        #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u64).range(10..=60000))]
        poll_interval_ms: u64,
        /// Maximum callback runtime; the delivery lease includes a safety margin.
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u32).range(1..=3300))]
        execution_timeout_seconds: u32,
    },
}

pub(super) fn wait(client: &RemoteClient, args: &WaitArgs) -> Result<Value> {
    JobId::parse(args.job.clone()).map_err(|_| failure("invalid_job_id", json!({})))?;
    let deadline = Instant::now() + Duration::from_secs(args.wait_seconds);
    loop {
        let value = client.get(&format!("/api/v1/admin/jobs/{}", args.job), &[])?;
        let snapshot: JobSnapshot = serde_json::from_value(value.clone())?;
        match snapshot.state {
            JobState::Succeeded => return Ok(value),
            JobState::Failed => return Err(failure("job_failed", value)),
            JobState::Canceled => return Err(failure("job_canceled", value)),
            _ => {}
        }
        if !pause(deadline, args.poll_interval_ms) {
            return Err(failure("job_wait_timeout", value));
        }
    }
}

pub(super) fn callback(client: &RemoteClient, command: &CallbackCommand) -> Result<Value> {
    match command {
        CallbackCommand::Register { job, target } => {
            JobId::parse(job.clone()).map_err(|_| failure("invalid_job_id", json!({})))?;
            client.post(
                &format!("/api/v1/admin/jobs/{job}/callbacks"),
                &json!({"target":target}),
            )
        }
        CallbackCommand::List {
            target,
            limit,
            before,
        } => client.get(
            "/api/v1/admin/job-callbacks",
            &super::query([
                ("target", Some(target.clone())),
                ("limit", Some(limit.to_string())),
                ("before", before.map(|v| v.to_string())),
            ]),
        ),
        CallbackCommand::Deliver {
            target,
            program,
            args,
            wait_seconds,
            poll_interval_ms,
            execution_timeout_seconds,
        } => {
            if !program.is_absolute() || !program.is_file() {
                return Err(failure(
                    "invalid_input",
                    json!({"field":"program","requirement":"existing absolute executable path"}),
                ));
            }
            let deadline = Instant::now() + Duration::from_secs(*wait_seconds);
            let claim = loop {
                let value = client.post(
                    "/api/v1/admin/job-callbacks/claim",
                    &json!({"target":target,"leaseSeconds":execution_timeout_seconds+120}),
                )?;
                if let Some(value) = value.get("claim").filter(|v| !v.is_null()) {
                    break serde_json::from_value::<CallbackClaim>(value.clone())?;
                }
                if !pause(deadline, *poll_interval_ms) {
                    return Err(failure("callback_wait_timeout", json!({"target":target})));
                }
            };
            let payload = json!({"schema":"job.completion.v1","deliveryId":claim.delivery.delivery_id,"target":claim.delivery.target,"attempt":claim.delivery.attempt_count,"event":claim.delivery.event});
            let outcome = run_callback(
                program,
                args,
                &payload,
                Duration::from_secs((*execution_timeout_seconds).into()),
            );
            let succeeded = matches!(outcome, Ok(Some(0)));
            // A failed/ambiguous acknowledgement must never cause this process
            // to invoke the callback again. The durable lease governs recovery.
            let receipt = client.post(
                &format!(
                    "/api/v1/admin/job-callbacks/{}/settle",
                    claim.delivery.delivery_id
                ),
                &json!({"leaseToken":claim.lease_token,"succeeded":succeeded}),
            ).map_err(|error| failure("callback_ack_failed",json!({
                "deliveryId":claim.delivery.delivery_id,"target":target,
                "callbackSucceeded":succeeded,"acknowledgement":"unknown",
                "remoteCode":error.downcast_ref::<super::RemoteCommandError>().map(|e|e.code.as_str()),
            })))?;
            if succeeded {
                Ok(json!({"delivery":receipt,"callbackExitCode":0}))
            } else {
                let kind = match &outcome {
                    Ok(Some(_)) => "nonzero_exit",
                    Ok(None) => "timeout_or_signal",
                    Err(_) => "process_io_failed",
                };
                Err(failure(
                    "callback_failed",
                    json!({"delivery":receipt,"callbackExitCode":outcome.ok().flatten(),"callbackFailure":kind}),
                ))
            }
        }
    }
}

fn pause(deadline: Instant, interval_ms: u64) -> bool {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return false;
    }
    thread::sleep(remaining.min(Duration::from_millis(interval_ms)));
    // The caller performs one final state check at the deadline.
    true
}

fn run_callback(
    program: &std::path::Path,
    args: &[String],
    payload: &Value,
    timeout: Duration,
) -> Result<Option<i32>> {
    let mut command = Command::new(program);
    command.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        // Do not pass the server's bearer credential into another application.
        .env_remove(super::TOKEN_ENVIRONMENT_VARIABLE)
        .env_remove("RCDVLOG_SERVER_CREDENTIAL_FILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("callback stdin unavailable"))?;
    let mut bytes = serde_json::to_vec(payload)?;
    bytes.push(b'\n');
    // A consumer that never reads stdin must still respect the deadline.
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = sender.send(stdin.write_all(&bytes));
    });
    let deadline = Instant::now() + timeout;
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status.code()),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                kill_callback(&mut child);
                let _ = child.wait();
                break Ok(None);
            }
            Err(error) => {
                kill_callback(&mut child);
                let _ = child.wait();
                break Err(error.into());
            }
        }
    };
    if matches!(outcome, Ok(Some(0))) {
        // Confirmation is bounded even when a grandchild retains the pipe.
        receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| anyhow!("callback input delivery was not confirmed"))??;
    }
    outcome
}

fn kill_callback(child: &mut std::process::Child) {
    #[cfg(unix)]
    // The child starts its own process group. Kill descendants as well, so a
    // timed-out consumer cannot keep acting after its delivery is retried.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
}
