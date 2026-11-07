//! Process ownership and liveness of in-flight calibration runs.

use super::*;

#[derive(Debug, Clone)]
pub(super) struct CalibrationRunOwner {
    pub(super) owner_id: String,
    pub(super) pid: u32,
    pub(super) process_start: String,
}

pub(super) fn process_start_marker(pid: u32) -> Option<String> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, after_comm) = text.rsplit_once(") ")?;
    after_comm.split_whitespace().nth(19).map(str::to_string)
}

pub(super) fn current_run_owner() -> CalibrationRunOwner {
    let pid = std::process::id();
    CalibrationRunOwner {
        owner_id: uuid::Uuid::new_v4().simple().to_string(),
        pid,
        process_start: process_start_marker(pid).unwrap_or_default(),
    }
}

pub(super) fn active_calibration_runs() -> &'static Mutex<HashMap<String, CalibrationRunOwner>> {
    static ACTIVE: OnceLock<Mutex<HashMap<String, CalibrationRunOwner>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn active_run_key(database_path: &Path, cache_key: &str) -> String {
    format!("{}::{cache_key}", database_path.to_string_lossy())
}

pub(super) struct ActiveCalibrationCleanup {
    pub(super) database_path: PathBuf,
    pub(super) keys: Vec<String>,
}

impl Drop for ActiveCalibrationCleanup {
    fn drop(&mut self) {
        let mut active = active_calibration_runs()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for key in &self.keys {
            active.remove(&active_run_key(&self.database_path, key));
        }
    }
}

pub(super) fn owner_is_live(
    owner_id: Option<&str>,
    pid: Option<u32>,
    process_start: Option<&str>,
    heartbeat_at: Option<&str>,
) -> bool {
    let Some(pid) = pid.filter(|pid| *pid > 0) else {
        return false;
    };
    let active = active_calibration_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(owner_id) = owner_id
        && active
            .values()
            .any(|owner| owner.owner_id == owner_id && owner.pid == pid)
    {
        return true;
    }
    if pid == std::process::id() {
        return false;
    }
    if let Some(expected_start) = process_start.filter(|value| !value.is_empty())
        && let Some(actual_start) = process_start_marker(pid)
    {
        return actual_start == expected_start;
    }
    #[cfg(unix)]
    if i32::try_from(pid).is_err() {
        return false;
    }
    #[cfg(unix)]
    if let Ok(pid) = i32::try_from(pid) {
        let result = unsafe { libc::kill(pid, 0) };
        if result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) {
            return true;
        }
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return false;
        }
    }
    // An unobservable process is conservatively live while its last heartbeat
    // is fresh. It can never be treated as a completed calibration.
    heartbeat_at.is_some_and(recent_heartbeat)
}

pub(super) fn recent_heartbeat(value: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|time| {
            Utc::now()
                .signed_duration_since(time.with_timezone(&Utc))
                .to_std()
                .ok()
        })
        .is_some_and(|age| age <= Duration::from_secs(300))
}
