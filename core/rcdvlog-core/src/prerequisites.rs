//! Bounded, read-only checks for the media tools used by the core.

use crate::resolve_tool_path;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const PROBE_OUTPUT_LIMIT: usize = 64 * 1024;
const MEDIA_TOOLS: [&str; 3] = ["ffmpeg", "ffprobe", "exiftool"];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPrerequisiteStatus {
    /// Used only when deserializing legacy payloads that predate `status`.
    #[default]
    Unknown,
    Missing,
    Unexecutable,
    UnsupportedVersion,
    UnrecognizedVersion,
    ProbeFailed,
    Supported,
}

impl fmt::Display for ToolPrerequisiteStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Unknown => "unknown",
            Self::Missing => "missing",
            Self::Unexecutable => "unexecutable",
            Self::UnsupportedVersion => "unsupported_version",
            Self::UnrecognizedVersion => "unrecognized_version",
            Self::ProbeFailed => "probe_failed",
            Self::Supported => "supported",
        };
        f.write_str(label)
    }
}

/// Serializable media-tool status. `available` retains its historic meaning:
/// the resolved path names a regular file; it does not promise execute access
/// or a supported version. Use `status`/`supported` for readiness decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolStatus {
    pub name: String,
    pub path: String,
    pub available: bool,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub supported: bool,
    #[serde(default)]
    pub status: ToolPrerequisiteStatus,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub remediation: Option<String>,
}

/// A stable, inspectable failure returned by `ensure_media_prerequisites`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaPrerequisiteError {
    pub name: String,
    pub path: String,
    pub status: ToolPrerequisiteStatus,
    pub reason: String,
    pub remediation: Option<String>,
}

impl fmt::Display for MediaPrerequisiteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} is {}: {}", self.name, self.status, self.reason)?;
        if let Some(remediation) = &self.remediation {
            write!(f, " Remediation: {remediation}")?;
        }
        Ok(())
    }
}

impl std::error::Error for MediaPrerequisiteError {}

/// Probe and report each configured core media tool. The exact path returned
/// by `resolve_tool_path` is passed to the child process; there is no second
/// PATH lookup after resolution.
pub fn prerequisite_status() -> Vec<ToolStatus> {
    MEDIA_TOOLS
        .into_iter()
        .map(|name| inspect_tool(name, resolve_tool_path(name)))
        .collect()
}

/// Fail with a typed error unless every named tool is present and supported.
/// Unknown names fail as probe errors; the caller must name each prerequisite
/// it actually needs.
pub fn ensure_media_prerequisites(
    required_names: &[&str],
) -> std::result::Result<(), MediaPrerequisiteError> {
    for name in required_names {
        let status = inspect_tool(name, resolve_tool_path(name));
        if let Some(error) = prerequisite_error(status) {
            return Err(error);
        }
    }
    Ok(())
}

fn prerequisite_error(status: ToolStatus) -> Option<MediaPrerequisiteError> {
    if status.supported {
        return None;
    }
    Some(MediaPrerequisiteError {
        name: status.name,
        path: status.path,
        status: status.status,
        reason: status
            .reason
            .unwrap_or_else(|| "tool is not supported".to_string()),
        remediation: status.remediation,
    })
}

fn inspect_tool(name: &str, path: PathBuf) -> ToolStatus {
    let available = path.is_file();
    let base = |status, version, supported, reason: Option<String>, remediation: Option<String>| {
        ToolStatus {
            name: name.to_string(),
            path: path.to_string_lossy().into_owned(),
            available,
            version,
            supported,
            status,
            reason,
            remediation,
        }
    };

    let Some(args) = version_args(name) else {
        return base(
            ToolPrerequisiteStatus::ProbeFailed,
            None,
            false,
            Some("no version probe is defined for this tool".to_string()),
            Some("Request one of ffmpeg, ffprobe, or exiftool.".to_string()),
        );
    };

    let metadata = match fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => {
            return base(
                ToolPrerequisiteStatus::Unexecutable,
                None,
                false,
                Some("resolved path is not a regular file".to_string()),
                Some(install_remediation(name, false)),
            );
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return base(
                ToolPrerequisiteStatus::Missing,
                None,
                false,
                Some("no file exists at the resolved path".to_string()),
                Some(install_remediation(name, false)),
            );
        }
        Err(error) => {
            return base(
                ToolPrerequisiteStatus::ProbeFailed,
                None,
                false,
                Some(format!("cannot inspect resolved path: {error}")),
                Some("Check permissions for the resolved tool path.".to_string()),
            );
        }
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return base(
                ToolPrerequisiteStatus::Unexecutable,
                None,
                false,
                Some("resolved file has no executable permission bits".to_string()),
                Some(install_remediation(name, false)),
            );
        }
    }

    let args = args.iter().map(OsString::from).collect::<Vec<_>>();
    let output = match run_bounded_command(&path, &args, PROBE_TIMEOUT, PROBE_OUTPUT_LIMIT) {
        Ok(output) => output,
        Err(BoundedCommandError::Spawn(error))
            if error.kind() == io::ErrorKind::PermissionDenied =>
        {
            return base(
                ToolPrerequisiteStatus::Unexecutable,
                None,
                false,
                Some(format!("resolved file cannot be executed: {error}")),
                Some(install_remediation(name, false)),
            );
        }
        Err(error) => {
            let reason = match &error {
                BoundedCommandError::TimedOut => {
                    format!("version probe exceeded {} seconds", PROBE_TIMEOUT.as_secs())
                }
                BoundedCommandError::OutputLimitExceeded => {
                    format!("version probe exceeded {PROBE_OUTPUT_LIMIT} bytes on stdout or stderr")
                }
                _ => format!("version probe failed: {error}"),
            };
            return base(
                ToolPrerequisiteStatus::ProbeFailed,
                None,
                false,
                Some(reason),
                Some(
                    "Check the tool installation and try the version command manually.".to_string(),
                ),
            );
        }
    };

    if !output.status.success() {
        return base(
            ToolPrerequisiteStatus::ProbeFailed,
            None,
            false,
            Some(format!(
                "version probe exited with {}",
                exit_description(&output.status)
            )),
            Some("Check the tool installation and try the version command manually.".to_string()),
        );
    }

    let Some(version) = parse_version(name, &output.stdout) else {
        return base(
            ToolPrerequisiteStatus::UnrecognizedVersion,
            None,
            false,
            Some("version probe did not report a recognized numeric version".to_string()),
            Some(install_remediation(name, false)),
        );
    };

    if matches!(name, "ffmpeg" | "ffprobe") {
        let Some(major) = numeric_major(&version) else {
            return base(
                ToolPrerequisiteStatus::UnrecognizedVersion,
                Some(version),
                false,
                Some("FFmpeg version does not begin with a numeric release".to_string()),
                Some(install_remediation(name, true)),
            );
        };
        if major < 7 {
            return base(
                ToolPrerequisiteStatus::UnsupportedVersion,
                Some(version.clone()),
                false,
                Some(format!(
                    "FFmpeg major version {major} is older than the required minimum 7"
                )),
                Some(install_remediation(name, true)),
            );
        }
    }

    base(
        ToolPrerequisiteStatus::Supported,
        Some(version),
        true,
        None,
        None,
    )
}

fn install_remediation(name: &str, upgrade: bool) -> String {
    #[cfg(target_os = "macos")]
    {
        let package = if name == "exiftool" {
            "exiftool"
        } else {
            "ffmpeg"
        };
        let action = if upgrade { "upgrade" } else { "install" };
        return format!("Run `brew {action} {package}`.");
    }
    #[cfg(target_os = "linux")]
    {
        let _action = if upgrade { "upgrade" } else { "install" };
        return format!(
            "Install/upgrade {name} with your Linux distribution's documented package, or place a supported executable on PATH (no automatic installation is performed)."
        );
    }
    #[allow(unreachable_code)]
    {
        let action = if upgrade { "upgrade" } else { "install" };
        format!("{action} a supported {name} release and make its executable available on PATH.")
    }
}

fn version_args(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "ffmpeg" | "ffprobe" => Some(&["-version"]),
        "exiftool" => Some(&["-ver"]),
        _ => None,
    }
}

fn parse_version(name: &str, stdout: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(stdout).ok()?;
    let line = text.lines().next()?.trim();
    let token = match name {
        "ffmpeg" => line
            .strip_prefix("ffmpeg version ")?
            .split_whitespace()
            .next()?,
        "ffprobe" => line
            .strip_prefix("ffprobe version ")?
            .split_whitespace()
            .next()?,
        "exiftool" => line,
        _ => return None,
    };
    let numeric = if matches!(name, "ffmpeg" | "ffprobe") {
        token.split('-').next().unwrap_or(token)
    } else {
        token
    };
    let parts = numeric.split('.').collect::<Vec<_>>();
    if parts.is_empty()
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    Some(token.to_string())
}

fn numeric_major(version: &str) -> Option<u32> {
    version.split(['.', '-']).next()?.parse().ok()
}

fn exit_description(status: &ExitStatus) -> String {
    status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "terminated by signal".to_string())
}

/// Output of a child command after both pipes have been drained concurrently.
#[derive(Debug)]
pub(crate) struct BoundedCommandOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: ExitStatus,
}

#[derive(Debug)]
pub(crate) enum BoundedCommandError {
    Spawn(io::Error),
    Wait(io::Error),
    Capture(io::Error),
    TimedOut,
    OutputLimitExceeded,
}

impl fmt::Display for BoundedCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "could not spawn child: {error}"),
            Self::Wait(error) => write!(f, "could not wait for child: {error}"),
            Self::Capture(error) => write!(f, "could not read child output: {error}"),
            Self::TimedOut => f.write_str("child process timed out"),
            Self::OutputLimitExceeded => f.write_str("child process exceeded output limit"),
        }
    }
}

/// Run a child with structured argv, concurrently drain both pipes, cap each
/// captured stream at `max_bytes`, and kill the child (and its Unix process
/// group) on timeout or overflow. Overflow is rejected rather than truncated
/// into apparently valid command output.
pub(crate) fn run_bounded_command(
    program: &Path,
    args: &[OsString],
    timeout: Duration,
    max_bytes: usize,
) -> std::result::Result<BoundedCommandOutput, BoundedCommandError> {
    let mut command = Command::new(program);
    command
        .args(args.iter().map(OsString::as_os_str))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command.spawn().map_err(BoundedCommandError::Spawn)?;
    let overflow = Arc::new(AtomicBool::new(false));
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let stdout_overflow = Arc::clone(&overflow);
    let stderr_overflow = Arc::clone(&overflow);
    let stdout_reader = thread::spawn(move || read_capped(stdout, max_bytes, stdout_overflow));
    let stderr_reader = thread::spawn(move || read_capped(stderr, max_bytes, stderr_overflow));

    let deadline = Instant::now() + timeout;
    let mut status = None;
    loop {
        if overflow.load(Ordering::Acquire) {
            kill_child_tree(&mut child);
            join_reader(stdout_reader).map_err(BoundedCommandError::Capture)?;
            join_reader(stderr_reader).map_err(BoundedCommandError::Capture)?;
            return Err(BoundedCommandError::OutputLimitExceeded);
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(child_status)) => status = Some(child_status),
                Ok(None) => {}
                Err(error) => {
                    kill_child_tree(&mut child);
                    join_reader(stdout_reader).map_err(BoundedCommandError::Capture)?;
                    join_reader(stderr_reader).map_err(BoundedCommandError::Capture)?;
                    return Err(BoundedCommandError::Wait(error));
                }
            }
        }
        if status.is_some() && stdout_reader.is_finished() && stderr_reader.is_finished() {
            break;
        }
        if Instant::now() >= deadline {
            // The direct child may have exited while a descendant still owns
            // an inherited pipe. Kill its process group in that case too.
            kill_child_tree(&mut child);
            join_reader(stdout_reader).map_err(BoundedCommandError::Capture)?;
            join_reader(stderr_reader).map_err(BoundedCommandError::Capture)?;
            return Err(BoundedCommandError::TimedOut);
        }
        thread::sleep(Duration::from_millis(10));
    }

    let stdout = join_reader(stdout_reader).map_err(BoundedCommandError::Capture)?;
    let stderr = join_reader(stderr_reader).map_err(BoundedCommandError::Capture)?;
    if overflow.load(Ordering::Acquire) {
        return Err(BoundedCommandError::OutputLimitExceeded);
    }
    Ok(BoundedCommandOutput {
        stdout,
        stderr,
        status: status.expect("child completed"),
    })
}

fn read_capped(
    mut reader: impl Read,
    max_bytes: usize,
    overflow: Arc<AtomicBool>,
) -> io::Result<Vec<u8>> {
    let mut captured = Vec::with_capacity(max_bytes.min(8192));
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(captured);
        }
        let room = max_bytes.saturating_sub(captured.len());
        captured.extend_from_slice(&buffer[..count.min(room)]);
        if count > room {
            overflow.store(true, Ordering::Release);
        }
    }
}

fn join_reader(handle: thread::JoinHandle<io::Result<Vec<u8>>>) -> io::Result<Vec<u8>> {
    handle
        .join()
        .map_err(|_| io::Error::other("output capture thread panicked"))?
}

fn kill_child_tree(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        let pid = child.id() as i32;
        // Each probe is placed in its own process group. This also terminates
        // shell-script children that inherited stdout/stderr from the probe.
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(all(test, unix))]
mod tests {
    // A fork in another fixture test can temporarily inherit a writable script
    // descriptor, causing Linux execve to return ETXTBSY before CLOEXEC runs.
    // Serialize fixture creation/execution, without retrying production errors.
    static SCRIPT_FIXTURES: std::sync::Mutex<()> = std::sync::Mutex::new(());
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;
    use tempfile::TempDir;

    fn executable(directory: &TempDir, name: &str, body: &str) -> PathBuf {
        let path = directory.path().join(name);
        // Publish the fixture atomically so parallel tests never race a child
        // spawn against an open-for-write executable (ETXTBSY on Linux).
        let temporary = directory.path().join(format!(".{name}.tmp"));
        fs::write(&temporary, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755)).unwrap();
        fs::rename(temporary, &path).unwrap();
        path
    }

    #[test]
    fn prerequisite_status_classifies_missing_and_unexecutable_tools() {
        let _fixture_guard = SCRIPT_FIXTURES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let temp = TempDir::new().unwrap();
        let missing = inspect_tool("ffmpeg", temp.path().join("missing-ffmpeg"));
        assert_eq!(missing.status, ToolPrerequisiteStatus::Missing);
        assert!(!missing.available);
        assert!(!missing.supported);
        assert!(missing.remediation.as_deref().unwrap().contains("PATH"));

        let path = temp.path().join("ffprobe");
        fs::write(&path, "#!/bin/sh\nprintf 'ffprobe version 7.0.0\\n'\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let unexecutable = inspect_tool("ffprobe", path);
        assert_eq!(unexecutable.status, ToolPrerequisiteStatus::Unexecutable);
        assert!(
            unexecutable.available,
            "legacy available means regular-file existence"
        );
        assert!(!unexecutable.supported);
    }

    #[test]
    fn ffmpeg_and_ffprobe_require_major_seven_and_accept_supported_versions() {
        let _fixture_guard = SCRIPT_FIXTURES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let temp = TempDir::new().unwrap();
        let old = executable(
            &temp,
            "ffmpeg-old",
            "printf 'ffmpeg version 6.1.2 Copyright\\n'",
        );
        let status = inspect_tool("ffmpeg", old);
        assert_eq!(status.status, ToolPrerequisiteStatus::UnsupportedVersion);
        assert_eq!(status.version.as_deref(), Some("6.1.2"));

        let current = executable(
            &temp,
            "ffmpeg-current",
            "printf 'ffmpeg version 7.0.1 Copyright\\n'",
        );
        let status = inspect_tool("ffmpeg", current);
        assert_eq!(
            status.status,
            ToolPrerequisiteStatus::Supported,
            "{status:?}"
        );
        assert!(status.supported);
        assert_eq!(status.version.as_deref(), Some("7.0.1"));

        let probe = executable(
            &temp,
            "ffprobe-current",
            "printf 'ffprobe version 8.0.0 Copyright\\n'",
        );
        let status = inspect_tool("ffprobe", probe);
        assert_eq!(
            status.status,
            ToolPrerequisiteStatus::Supported,
            "{status:?}"
        );
    }

    #[test]
    fn exiftool_requires_a_known_numeric_version() {
        let _fixture_guard = SCRIPT_FIXTURES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let temp = TempDir::new().unwrap();
        let path = executable(&temp, "exiftool", "printf '13.36\\n'");
        let status = inspect_tool("exiftool", path);
        assert_eq!(
            status.status,
            ToolPrerequisiteStatus::Supported,
            "{status:?}"
        );
        assert_eq!(status.version.as_deref(), Some("13.36"));
    }

    #[test]
    fn malformed_version_and_nonzero_exit_are_distinct_failures() {
        let _fixture_guard = SCRIPT_FIXTURES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let temp = TempDir::new().unwrap();
        let malformed = executable(
            &temp,
            "ffmpeg-malformed",
            "printf 'ffmpeg development build\\n'",
        );
        let malformed_status = inspect_tool("ffmpeg", malformed);
        assert_eq!(
            malformed_status.status,
            ToolPrerequisiteStatus::UnrecognizedVersion,
            "{malformed_status:?}"
        );

        let nonzero = executable(
            &temp,
            "ffprobe-error",
            "printf 'ffprobe version 7.0.0\\n'; exit 4",
        );
        let status = inspect_tool("ffprobe", nonzero);
        assert_eq!(status.status, ToolPrerequisiteStatus::ProbeFailed);
        assert!(status.reason.as_deref().unwrap().contains("exited with 4"));
    }

    #[test]
    fn bounded_command_times_out_and_kills_the_child() {
        let _fixture_guard = SCRIPT_FIXTURES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let temp = TempDir::new().unwrap();
        let path = executable(&temp, "slow-tool", "sleep 5");
        let result = run_bounded_command(&path, &[], Duration::from_millis(100), 1024);
        assert!(matches!(result, Err(BoundedCommandError::TimedOut)));
        assert_eq!(
            inspect_tool("ffmpeg", path).status,
            ToolPrerequisiteStatus::ProbeFailed,
            "the standard prerequisite probe reports timeout as a probe failure",
        );
    }

    #[test]
    fn bounded_command_rejects_and_caps_large_output() {
        let _fixture_guard = SCRIPT_FIXTURES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let temp = TempDir::new().unwrap();
        let path = executable(
            &temp,
            "noisy-ffmpeg",
            "printf 'ffmpeg version 7.0.0\\n'; yes x | head -c 100000 >/dev/null; yes x | head -c 100000",
        );
        let result = run_bounded_command(
            &path,
            &[OsString::from("-version")],
            Duration::from_secs(2),
            4096,
        );
        assert!(matches!(
            result,
            Err(BoundedCommandError::OutputLimitExceeded)
        ));
        assert_eq!(
            inspect_tool("ffmpeg", path).status,
            ToolPrerequisiteStatus::ProbeFailed
        );
    }

    #[test]
    fn ensure_returns_a_typed_error_for_a_unsupported_required_tool() {
        let _fixture_guard = SCRIPT_FIXTURES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let temp = TempDir::new().unwrap();
        let path = executable(&temp, "ffmpeg-old", "printf 'ffmpeg version 6.0.0\\n'");
        let status = inspect_tool("ffmpeg", path);
        let error = prerequisite_error(status).unwrap();
        assert_eq!(error.name, "ffmpeg");
        assert_eq!(error.status, ToolPrerequisiteStatus::UnsupportedVersion);
        assert!(error.to_string().contains("minimum 7"));
    }

    #[test]
    fn legacy_tool_status_payload_defaults_new_fields() {
        let _fixture_guard = SCRIPT_FIXTURES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let legacy = r#"{"name":"ffmpeg","path":"/usr/bin/ffmpeg","available":true}"#;
        let status: ToolStatus = serde_json::from_str(legacy).unwrap();
        assert_eq!(status.status, ToolPrerequisiteStatus::Unknown);
        assert!(status.version.is_none());
        assert!(!status.supported);
    }

    #[test]
    fn bounded_command_times_out_if_descendant_keeps_output_pipe_open() {
        let _fixture_guard = SCRIPT_FIXTURES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let temp = TempDir::new().unwrap();
        let path = executable(
            &temp,
            "parent-exits",
            "sh -c 'sleep 5; echo child' & exit 0",
        );
        let started = Instant::now();
        let result = run_bounded_command(&path, &[], Duration::from_millis(100), 1024);
        assert!(matches!(result, Err(BoundedCommandError::TimedOut)));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
