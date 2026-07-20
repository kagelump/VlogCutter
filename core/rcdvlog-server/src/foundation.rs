use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{ConfigError, ConfigErrorReason, ControlCatalog, ServerConfig, ServerInstanceLock};

#[derive(Debug)]
pub struct RuntimeFoundation {
    config: ServerConfig,
    catalog: ControlCatalog,
    _instance_lock: ServerInstanceLock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessStatus {
    Ready,
    NotReady,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
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
    fn ok(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Ok,
            code: None,
        }
    }

    fn error(name: impl Into<String>, code: impl Into<String>) -> Self {
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
        let status = if checks.iter().all(|check| check.status == CheckStatus::Ok) {
            ReadinessStatus::Ready
        } else {
            ReadinessStatus::NotReady
        };
        Self { status, checks }
    }

    pub fn is_ready(&self) -> bool {
        self.status == ReadinessStatus::Ready
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
        // Recovery is a short durable transition; no handler is started here.
        // Task 5 will register concrete server-only handlers with the scheduler.
        catalog.jobs().recover_expired()?;
        Ok(Self {
            config,
            catalog,
            _instance_lock: instance_lock,
        })
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub fn catalog(&self) -> &ControlCatalog {
        &self.catalog
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
            inspect_external_directory("data_directory", &self.config.data_dir, true),
            inspect_external_directory("temp_directory", &self.config.temp_dir, true),
            inspect_resolved_path_separation(&self.config),
            inspect_external_directory("derived_directory", &self.config.derived_dir, true),
            inspect_external_directory("backup_directory", &self.config.backup_dir, true),
        ];
        for index in 0..self.config.allowed_original_mounts.len() {
            checks.push(inspect_external_directory(
                format!("original_mount_{index}"),
                &self.config.allowed_original_mounts[index],
                false,
            ));
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
