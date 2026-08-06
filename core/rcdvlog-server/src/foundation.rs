use anyhow::{Context, Result, anyhow};
use rusqlite::OptionalExtension;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::auth::{AuthConfig, AuthService};
use crate::{ConfigError, ConfigErrorReason, ControlCatalog, ServerConfig, ServerInstanceLock};

#[derive(Debug)]
pub struct RuntimeFoundation {
    config: ServerConfig,
    catalog: ControlCatalog,
    auth_service: AuthService,
    _instance_lock: ServerInstanceLock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessStatus {
    Ready,
    Degraded,
    NotReady,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessCheck {
    pub name: String,
    pub status: CheckStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl ReadinessCheck {
    pub fn ok(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Ok,
            code: None,
        }
    }

    pub fn warning(name: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Warning,
            code: Some(code.into()),
        }
    }

    pub fn error(name: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Error,
            code: Some(code.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessReport {
    pub status: ReadinessStatus,
    pub checks: Vec<ReadinessCheck>,
}

impl ReadinessReport {
    pub fn from_checks(checks: Vec<ReadinessCheck>) -> Self {
        let status = if checks
            .iter()
            .any(|check| check.status == CheckStatus::Error)
        {
            ReadinessStatus::NotReady
        } else if checks
            .iter()
            .any(|check| check.status == CheckStatus::Warning)
        {
            ReadinessStatus::Degraded
        } else {
            ReadinessStatus::Ready
        };
        Self { status, checks }
    }

    pub fn is_ready(&self) -> bool {
        self.status != ReadinessStatus::NotReady
    }

    pub fn evaluation_unavailable(code: impl Into<String>) -> Self {
        Self::from_checks(vec![ReadinessCheck::error("readiness_evaluation", code)])
    }
}

impl RuntimeFoundation {
    pub fn initialize(config: ServerConfig) -> Result<Self> {
        config.validate()?;
        prepare_local_state(&config)?;
        let instance_lock = ServerInstanceLock::acquire(&config.data_dir)?;
        let catalog = ControlCatalog::open_or_create(&config.data_dir, &config.diagnostics())?;
        let auth_service = AuthService::new(
            config
                .data_dir
                .join(super::catalog::CONTROL_DATABASE_FILENAME),
            catalog.node_id().clone(),
            AuthConfig {
                session_max_age: config.session_max_age,
                bootstrap_code_ttl: config.bootstrap_code_ttl,
                pairing_code_ttl: config.pairing_code_ttl,
            },
        );
        // Recovery is a short durable transition; no handler is started here.
        // Task 5 will register concrete server-only handlers with the scheduler.
        catalog.jobs().recover_expired()?;
        Ok(Self {
            config,
            catalog,
            auth_service,
            _instance_lock: instance_lock,
        })
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub fn catalog(&self) -> &ControlCatalog {
        &self.catalog
    }

    pub fn auth(&self) -> &AuthService {
        &self.auth_service
    }

    pub fn jobs(&self) -> crate::JobEngine {
        self.catalog.jobs()
    }

    pub fn readiness(&self) -> ReadinessReport {
        let mut checks = vec![
            match self.catalog.check_usable() {
                Ok(()) => ReadinessCheck::ok("control_catalog"),
                Err(_) => ReadinessCheck::error("control_catalog", "control_catalog_unavailable"),
            },
            match self.auth_service.diagnostics() {
                Ok(_) => ReadinessCheck::ok("authentication"),
                Err(_) => ReadinessCheck::error("authentication", "auth_service_unavailable"),
            },
            inspect_external_directory("data_directory", &self.config.data_dir, true),
            inspect_free_space(
                "data_disk_space",
                &self.config.data_dir,
                self.config.minimum_free_bytes,
                false,
            ),
            inspect_external_directory("temp_directory", &self.config.temp_dir, true),
            inspect_resolved_path_separation(&self.config),
            inspect_external_directory("derived_directory", &self.config.derived_dir, true),
            inspect_free_space(
                "derived_disk_space",
                &self.config.derived_dir,
                self.config.minimum_free_bytes,
                false,
            ),
            degrade(inspect_external_directory(
                "backup_directory",
                &self.config.backup_dir,
                true,
            )),
            inspect_free_space(
                "backup_disk_space",
                &self.config.backup_dir,
                self.config.minimum_free_bytes,
                true,
            ),
            inspect_control_integrity(self.catalog.database_path()),
            inspect_project_replicas(self.catalog.database_path(), &self.config.data_dir),
            inspect_expired_job_leases(self.catalog.database_path()),
            inspect_backup_freshness(self.catalog.database_path(), self.config.backup_interval),
            inspect_hlc_clock_sanity(self.catalog.database_path(), &self.config.data_dir),
        ];
        for index in 0..self.config.allowed_original_mounts.len() {
            checks.push(degrade(inspect_external_directory(
                format!("original_mount_{index}"),
                &self.config.allowed_original_mounts[index],
                false,
            )));
        }
        checks.extend([
            probe_tool("ffmpeg", &self.config.ffmpeg_command, "-version"),
            probe_tool("ffprobe", &self.config.ffprobe_command, "-version"),
            probe_tool("exiftool", &self.config.exiftool_command, "-ver"),
        ]);
        ReadinessReport::from_checks(checks)
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.catalog.checkpoint()
    }
}

fn prepare_local_state(config: &ServerConfig) -> Result<()> {
    let existing_data_ancestor = config
        .data_dir
        .ancestors()
        .find(|path| path.exists())
        .ok_or_else(|| anyhow!("dataDir has no accessible existing ancestor"))?;
    ensure_local_database_filesystem(existing_data_ancestor)?;
    fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("create local data directory {}", config.data_dir.display()))?;
    ensure_owned_directory(&config.data_dir, "dataDir")?;
    fs::create_dir_all(&config.temp_dir).with_context(|| {
        format!(
            "create local temporary directory {}",
            config.temp_dir.display()
        )
    })?;
    ensure_owned_directory(&config.temp_dir, "tempDir")?;

    let canonical_data = config
        .data_dir
        .canonicalize()
        .context("resolve local data directory")?;
    let canonical_temp = config
        .temp_dir
        .canonicalize()
        .context("resolve local temporary directory")?;
    if !canonical_temp.starts_with(&canonical_data) {
        return Err(ConfigError {
            code: "invalid_configuration",
            field: "tempDir",
            reason: ConfigErrorReason::MustBeInsideDataDirectory,
        }
        .into());
    }
    ensure_local_database_filesystem(&canonical_data)?;
    Ok(())
}

fn inspect_resolved_path_separation(config: &ServerConfig) -> ReadinessCheck {
    let canonical_data = match config.data_dir.canonicalize() {
        Ok(path) => path,
        Err(_) => {
            return ReadinessCheck::error("resolved_path_separation", "path_resolution_failed");
        }
    };
    let canonical_temp = match config.temp_dir.canonicalize() {
        Ok(path) => path,
        Err(_) => {
            return ReadinessCheck::error("resolved_path_separation", "path_resolution_failed");
        }
    };
    let derived = match canonicalize_if_present(&config.derived_dir) {
        Ok(path) => path,
        Err(_) => {
            return ReadinessCheck::error("resolved_path_separation", "path_resolution_failed");
        }
    };
    let backup = match canonicalize_if_present(&config.backup_dir) {
        Ok(path) => path,
        Err(_) => {
            return ReadinessCheck::error("resolved_path_separation", "path_resolution_failed");
        }
    };
    if derived.as_ref().is_some_and(|path| {
        path.as_path() == canonical_data.as_path() || paths_overlap(path, &canonical_temp)
    }) {
        return ReadinessCheck::error("resolved_path_separation", "unsafe_path_overlap");
    }
    if backup.as_ref().is_some_and(|path| {
        path.as_path() == canonical_data.as_path()
            || paths_overlap(path, &canonical_temp)
            || derived
                .as_ref()
                .is_some_and(|derived| paths_overlap(path, derived))
    }) {
        return ReadinessCheck::error("resolved_path_separation", "unsafe_path_overlap");
    }
    let writable = [Some(canonical_data), Some(canonical_temp), derived, backup];
    for original in &config.allowed_original_mounts {
        let original = match canonicalize_if_present(original) {
            Ok(original) => original,
            Err(_) => {
                return ReadinessCheck::error("resolved_path_separation", "path_resolution_failed");
            }
        };
        let Some(original) = original else {
            continue;
        };
        if writable
            .iter()
            .flatten()
            .any(|path| paths_overlap(&original, path))
        {
            return ReadinessCheck::error("resolved_path_separation", "unsafe_path_overlap");
        }
    }
    ReadinessCheck::ok("resolved_path_separation")
}

fn canonicalize_if_present(path: &Path) -> std::io::Result<Option<PathBuf>> {
    match path.canonicalize() {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn degrade(check: ReadinessCheck) -> ReadinessCheck {
    if check.status == CheckStatus::Error {
        ReadinessCheck::warning(
            check.name,
            check
                .code
                .unwrap_or_else(|| "optional_resource_unavailable".into()),
        )
    } else {
        check
    }
}

fn inspect_free_space(
    name: &str,
    path: &Path,
    minimum_bytes: u64,
    optional: bool,
) -> ReadinessCheck {
    match fs2::available_space(path) {
        Ok(available) if available >= minimum_bytes => ReadinessCheck::ok(name),
        Ok(_) if optional => ReadinessCheck::warning(name, "low_disk_space"),
        Ok(_) => ReadinessCheck::error(name, "low_disk_space"),
        Err(_) if optional => ReadinessCheck::warning(name, "disk_space_unavailable"),
        Err(_) => ReadinessCheck::error(name, "disk_space_unavailable"),
    }
}

fn read_only_sqlite(path: &Path) -> rusqlite::Result<rusqlite::Connection> {
    rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
}

fn inspect_control_integrity(path: &Path) -> ReadinessCheck {
    let result = read_only_sqlite(path).and_then(|connection| {
        connection.query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))
    });
    match result {
        Ok(result) if result == "ok" => ReadinessCheck::ok("control_integrity"),
        _ => ReadinessCheck::error("control_integrity", "control_catalog_corrupt"),
    }
}

fn registered_replica_paths(control_path: &Path) -> rusqlite::Result<Vec<String>> {
    let connection = read_only_sqlite(control_path)?;
    let mut statement = connection.prepare(
        "SELECT replica_relative_path FROM project_registry WHERE state='registered' ORDER BY project_id LIMIT 32",
    )?;
    statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()
}

fn inspect_project_replicas(control_path: &Path, data_root: &Path) -> ReadinessCheck {
    let Ok(paths) = registered_replica_paths(control_path) else {
        return ReadinessCheck::error("project_replicas", "project_registry_unavailable");
    };
    for relative in paths {
        let relative = Path::new(&relative);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return ReadinessCheck::warning("project_replicas", "project_replica_path_unsafe");
        }
        let path = data_root.join(relative);
        if !path.is_file() {
            return ReadinessCheck::warning("project_replicas", "project_replica_missing");
        }
        // Keep per-request readiness bounded: opening the replica and reading its
        // migration/project roots catches unavailable and structurally broken
        // catalogs without walking every page of every project database.
        let valid = read_only_sqlite(&path).is_ok_and(|connection| {
            connection
                .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                    row.get::<_, i64>(0)
                })
                .and_then(|_| {
                    connection.query_row("SELECT COUNT(*) FROM projects", [], |row| {
                        row.get::<_, i64>(0)
                    })
                })
                .is_ok()
        });
        if !valid {
            return ReadinessCheck::warning("project_replicas", "project_replica_corrupt");
        }
    }
    ReadinessCheck::ok("project_replicas")
}

fn inspect_expired_job_leases(control_path: &Path) -> ReadinessCheck {
    let expired = read_only_sqlite(control_path).and_then(|connection| {
        connection.query_row(
            "SELECT COUNT(*) FROM jobs WHERE state='running' AND lease_expires_at < ?1",
            [chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)],
            |row| row.get::<_, i64>(0),
        )
    });
    match expired {
        Ok(0) => ReadinessCheck::ok("job_leases"),
        Ok(_) => ReadinessCheck::warning("job_leases", "expired_job_leases"),
        Err(_) => ReadinessCheck::error("job_leases", "job_engine_unavailable"),
    }
}

fn inspect_backup_freshness(control_path: &Path, interval: Duration) -> ReadinessCheck {
    let latest = read_only_sqlite(control_path).and_then(|connection| {
        connection
            .query_row(
                "SELECT completed_at FROM backup_sets WHERE state='completed' ORDER BY completed_at DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
    });
    let Ok(Some(latest)) = latest else {
        return ReadinessCheck::warning("backup_freshness", "backup_missing_or_stale");
    };
    let fresh = chrono::DateTime::parse_from_rfc3339(&latest)
        .ok()
        .is_some_and(|latest| {
            chrono::Utc::now().signed_duration_since(latest.with_timezone(&chrono::Utc))
                <= chrono::Duration::from_std(interval.saturating_mul(2))
                    .unwrap_or(chrono::Duration::MAX)
        });
    if fresh {
        ReadinessCheck::ok("backup_freshness")
    } else {
        ReadinessCheck::warning("backup_freshness", "backup_missing_or_stale")
    }
}

fn inspect_hlc_clock_sanity(control_path: &Path, data_root: &Path) -> ReadinessCheck {
    let Ok(paths) = registered_replica_paths(control_path) else {
        return ReadinessCheck::error("hlc_clock", "project_registry_unavailable");
    };
    let now = chrono::Utc::now().timestamp_millis();
    let future_limit = now.saturating_add(rcdvlog_core::MAX_REMOTE_HLC_FUTURE_SKEW_MILLIS);
    for relative in paths {
        let path = data_root.join(relative);
        let highest = read_only_sqlite(&path).and_then(|connection| {
            connection.query_row(
                "SELECT COALESCE(MAX(wall_millis),0) FROM operation_hlc_state",
                [],
                |row| row.get::<_, i64>(0),
            )
        });
        if highest.is_ok_and(|value| value > future_limit) {
            return ReadinessCheck::warning("hlc_clock", "clock_skew_detected");
        }
    }
    ReadinessCheck::ok("hlc_clock")
}

fn ensure_owned_directory(path: &Path, field: &str) -> Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("inspect server configuration field {field}"))?;
    if !metadata.is_dir() {
        return Err(anyhow!(
            "server configuration field {field} is not a directory"
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions and does not dereference memory.
        let effective_uid = unsafe { libc::geteuid() };
        if metadata.uid() != effective_uid {
            return Err(anyhow!(
                "server configuration field {field} must be owned by the server process user"
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn ensure_local_database_filesystem(path: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    const CIFS_SUPER_MAGIC: u64 = 0xff53_4d42;
    let path = CString::new(path.as_os_str().as_bytes())
        .context("local data directory contains an unsupported NUL byte")?;
    let mut statistics = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: path is a valid NUL-terminated string and statistics points to writable memory.
    let result = unsafe { libc::statfs(path.as_ptr(), statistics.as_mut_ptr()) };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("inspect data-directory filesystem");
    }
    // SAFETY: a successful statfs call initialized the structure.
    let filesystem_type = unsafe { statistics.assume_init() }.f_type as u64;
    if filesystem_type == libc::NFS_SUPER_MAGIC as u64
        || filesystem_type == libc::SMB_SUPER_MAGIC as u64
        || filesystem_type == CIFS_SUPER_MAGIC
    {
        return Err(anyhow!(
            "dataDir must use a local filesystem; NFS and SMB/CIFS are not supported for live SQLite state"
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn ensure_local_database_filesystem(_path: &Path) -> Result<()> {
    // Portable APIs cannot reliably distinguish local storage from every network or FUSE mount.
    // Compose supplies a plain Docker-local volume; non-Linux operators must keep dataDir local.
    Ok(())
}

fn inspect_external_directory(
    name: impl Into<String>,
    path: &Path,
    expect_writable_metadata: bool,
) -> ReadinessCheck {
    let name = name.into();
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return ReadinessCheck::error(name, "directory_unavailable"),
    };
    if !metadata.is_dir() {
        return ReadinessCheck::error(name, "not_a_directory");
    }
    if !has_effective_directory_access(path, expect_writable_metadata) {
        let code = if expect_writable_metadata {
            "directory_not_writable"
        } else {
            "directory_unreadable"
        };
        return ReadinessCheck::error(name, code);
    }
    ReadinessCheck::ok(name)
}

#[cfg(unix)]
fn has_effective_directory_access(path: &Path, require_write: bool) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut mode = libc::R_OK | libc::X_OK;
    if require_write {
        mode |= libc::W_OK;
    }
    // SAFETY: path is a valid NUL-terminated string. AT_EACCESS selects effective IDs.
    unsafe { libc::faccessat(libc::AT_FDCWD, path.as_ptr(), mode, libc::AT_EACCESS) == 0 }
}

#[cfg(not(unix))]
fn has_effective_directory_access(path: &Path, require_write: bool) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    fs::read_dir(path).is_ok() && (!require_write || !metadata.permissions().readonly())
}

fn probe_tool(name: &str, command: &str, version_flag: &str) -> ReadinessCheck {
    let status = Command::new(command)
        .arg(version_flag)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => ReadinessCheck::ok(format!("tool_{name}")),
        Ok(_) => ReadinessCheck::error(format!("tool_{name}"), "tool_failed"),
        Err(_) => ReadinessCheck::error(format!("tool_{name}"), "tool_missing"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn configured(root: &TempDir, include_missing_tool: bool) -> ServerConfig {
        let data = root.path().join("state");
        let derived = root.path().join("derived");
        let backup = root.path().join("backup");
        let original = root.path().join("original");
        fs::create_dir_all(&derived).unwrap();
        fs::create_dir_all(&backup).unwrap();
        fs::create_dir_all(&original).unwrap();
        let tool = if include_missing_tool {
            "rcdvlog-tool-that-does-not-exist"
        } else {
            "/usr/bin/true"
        };
        ServerConfig::from_values(BTreeMap::from([
            ("RCDVLOG_DATA_DIR".to_string(), data.display().to_string()),
            (
                "RCDVLOG_DERIVED_DIR".to_string(),
                derived.display().to_string(),
            ),
            (
                "RCDVLOG_BACKUP_DIR".to_string(),
                backup.display().to_string(),
            ),
            (
                "RCDVLOG_ALLOWED_ORIGINAL_MOUNTS".to_string(),
                original.display().to_string(),
            ),
            ("RCDVLOG_FFMPEG".to_string(), tool.to_string()),
            ("RCDVLOG_FFPROBE".to_string(), tool.to_string()),
            ("RCDVLOG_EXIFTOOL".to_string(), tool.to_string()),
            // Floor the disk-capacity gate (default 5 GiB) so readiness tests
            // never depend on how much free space the host happens to have.
            // GH ubuntu runners frequently sit below the default, which turned
            // the non-optional data/derived disk checks into readiness errors.
            (
                "RCDVLOG_MINIMUM_FREE_BYTES".to_string(),
                (64 * 1024 * 1024).to_string(),
            ),
        ]))
        .unwrap()
    }

    #[test]
    fn initialization_creates_only_local_state_and_reports_ready() {
        let root = TempDir::new().unwrap();
        let config = configured(&root, false);
        let derived_before = fs::metadata(&config.derived_dir)
            .unwrap()
            .modified()
            .unwrap();
        let original_before = fs::metadata(&config.allowed_original_mounts[0])
            .unwrap()
            .modified()
            .unwrap();

        let foundation = RuntimeFoundation::initialize(config.clone()).unwrap();
        assert!(config.data_dir.exists());
        assert!(config.temp_dir.exists());
        assert!(foundation.readiness().is_ready());
        assert_eq!(
            fs::metadata(&config.derived_dir)
                .unwrap()
                .modified()
                .unwrap(),
            derived_before
        );
        assert_eq!(
            fs::metadata(&config.allowed_original_mounts[0])
                .unwrap()
                .modified()
                .unwrap(),
            original_before
        );
    }

    #[test]
    fn missing_tools_and_external_paths_are_readiness_failures_not_startup_failures() {
        let root = TempDir::new().unwrap();
        let config = configured(&root, true);
        fs::remove_dir(&config.backup_dir).unwrap();
        let foundation = RuntimeFoundation::initialize(config).unwrap();
        let report = foundation.readiness();
        assert!(!report.is_ready());
        assert!(report.checks.iter().any(|check| {
            check.name == "backup_directory"
                && check.code.as_deref() == Some("directory_unavailable")
        }));
        assert!(report.checks.iter().any(|check| {
            check.name == "resolved_path_separation" && check.status == CheckStatus::Ok
        }));
        assert!(
            report
                .checks
                .iter()
                .filter(|check| check.code.as_deref() == Some("tool_missing"))
                .count()
                == 3
        );
    }

    #[cfg(unix)]
    #[test]
    fn external_resolution_failures_do_not_block_startup_and_prevent_readiness() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        let config = configured(&root, false);
        fs::remove_dir(&config.derived_dir).unwrap();
        symlink(&config.derived_dir, &config.derived_dir).unwrap();

        let foundation = RuntimeFoundation::initialize(config).unwrap();
        let report = foundation.readiness();
        assert!(!report.is_ready());
        assert!(report.checks.iter().any(|check| {
            check.name == "resolved_path_separation"
                && check.code.as_deref() == Some("path_resolution_failed")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn non_writable_derived_directory_is_reported_without_a_write_probe() {
        use std::os::unix::fs::PermissionsExt;

        // Root legitimately has effective write access despite directory mode bits.
        // SAFETY: geteuid has no preconditions and does not dereference memory.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let root = TempDir::new().unwrap();
        let config = configured(&root, false);
        let foundation = RuntimeFoundation::initialize(config.clone()).unwrap();
        fs::set_permissions(&config.derived_dir, fs::Permissions::from_mode(0o555)).unwrap();
        let report = foundation.readiness();
        fs::set_permissions(&config.derived_dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(report.checks.iter().any(|check| {
            check.name == "derived_directory"
                && check.code.as_deref() == Some("directory_not_writable")
        }));
    }
}
