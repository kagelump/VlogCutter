use serde::{Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

pub const DEFAULT_DATA_DIR: &str = "/var/lib/rcdvlog";
pub const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:8090";
pub const DEFAULT_SHUTDOWN_TIMEOUT_SECONDS: u64 = 10;

const DATA_DIR_FIELD: &str = "dataDir";
const TEMP_DIR_FIELD: &str = "tempDir";
const DERIVED_DIR_FIELD: &str = "derivedDir";
const BACKUP_DIR_FIELD: &str = "backupDir";
const ORIGINAL_MOUNTS_FIELD: &str = "allowedOriginalMounts";
const LISTEN_ADDR_FIELD: &str = "listenAddr";
const SHUTDOWN_TIMEOUT_FIELD: &str = "shutdownTimeoutSeconds";
const FFMPEG_FIELD: &str = "ffmpegCommand";
const FFPROBE_FIELD: &str = "ffprobeCommand";
const EXIFTOOL_FIELD: &str = "exiftoolCommand";

#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

impl Serialize for SecretString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str("[REDACTED]")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigErrorReason {
    MustBeAbsolute,
    ContainsTraversal,
    MustBeInsideDataDirectory,
    MustBeDistinct,
    OverlapsWritableDirectory,
    OverlapsOriginalMount,
    DuplicatePath,
    InvalidSocketAddress,
    ListenerMustBeLoopback,
    InvalidInteger,
    InvalidUnicode,
    OutOfRange,
    EmptyValue,
}

impl fmt::Display for ConfigErrorReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MustBeAbsolute => "must be an absolute path",
            Self::ContainsTraversal => "must not contain dot or parent traversal components",
            Self::MustBeInsideDataDirectory => "must be contained by the local data directory",
            Self::MustBeDistinct => "must identify a distinct directory",
            Self::OverlapsWritableDirectory => "must not overlap another writable directory",
            Self::OverlapsOriginalMount => "must not overlap an allowed original-media mount",
            Self::DuplicatePath => "contains the same path more than once",
            Self::InvalidSocketAddress => "must be an IP socket address",
            Self::ListenerMustBeLoopback => "must use an IP loopback address",
            Self::InvalidInteger => "must be an integer",
            Self::InvalidUnicode => "must contain valid Unicode",
            Self::OutOfRange => "is outside the supported range",
            Self::EmptyValue => "must not be empty",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize)]
#[error("invalid server configuration field {field}: {reason}")]
#[serde(rename_all = "camelCase")]
pub struct ConfigError {
    pub code: &'static str,
    pub field: &'static str,
    pub reason: ConfigErrorReason,
}

impl ConfigError {
    fn new(field: &'static str, reason: ConfigErrorReason) -> Self {
        Self {
            code: "invalid_configuration",
            field,
            reason,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub data_dir: PathBuf,
    pub temp_dir: PathBuf,
    pub derived_dir: PathBuf,
    pub backup_dir: PathBuf,
    pub allowed_original_mounts: Vec<PathBuf>,
    pub listen_addr: SocketAddr,
    pub shutdown_timeout: Duration,
    pub ffmpeg_command: String,
    pub ffprobe_command: String,
    pub exiftool_command: String,
}

impl fmt::Debug for ServerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.diagnostics().fmt(formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigDiagnostics {
    pub data_dir: PathBuf,
    pub temp_dir: PathBuf,
    pub derived_dir: PathBuf,
    pub backup_dir: PathBuf,
    pub allowed_original_mounts: Vec<PathBuf>,
    pub listen_addr: String,
    pub shutdown_timeout_seconds: u64,
    pub ffmpeg_command: String,
    pub ffprobe_command: String,
    pub exiftool_command: String,
}

impl ServerConfig {
    pub fn from_environment() -> Result<Self, ConfigError> {
        let mut values = BTreeMap::new();
        for (key, field) in [
            ("RCDVLOG_DATA_DIR", DATA_DIR_FIELD),
            ("RCDVLOG_TEMP_DIR", TEMP_DIR_FIELD),
            ("RCDVLOG_DERIVED_DIR", DERIVED_DIR_FIELD),
            ("RCDVLOG_BACKUP_DIR", BACKUP_DIR_FIELD),
            ("RCDVLOG_ALLOWED_ORIGINAL_MOUNTS", ORIGINAL_MOUNTS_FIELD),
            ("RCDVLOG_LISTEN_ADDR", LISTEN_ADDR_FIELD),
            ("RCDVLOG_SHUTDOWN_TIMEOUT_SECONDS", SHUTDOWN_TIMEOUT_FIELD),
            ("RCDVLOG_FFMPEG", FFMPEG_FIELD),
            ("RCDVLOG_FFPROBE", FFPROBE_FIELD),
            ("RCDVLOG_EXIFTOOL", EXIFTOOL_FIELD),
        ] {
            match std::env::var(key) {
                Ok(value) => {
                    values.insert(key.to_string(), value);
                }
                Err(std::env::VarError::NotPresent) => {}
                Err(std::env::VarError::NotUnicode(_)) => {
                    return Err(ConfigError::new(field, ConfigErrorReason::InvalidUnicode));
                }
            }
        }
        Self::from_values(values)
    }

    pub fn from_values(values: BTreeMap<String, String>) -> Result<Self, ConfigError> {
        let data_dir = configured_path(&values, "RCDVLOG_DATA_DIR", DEFAULT_DATA_DIR);
        let temp_default = data_dir.join("tmp");
        let derived_default = data_dir.join("derived");
        let backup_default = data_dir.join("backups");
        let temp_dir = configured_optional_path(&values, "RCDVLOG_TEMP_DIR", temp_default);
        let derived_dir = configured_optional_path(&values, "RCDVLOG_DERIVED_DIR", derived_default);
        let backup_dir = configured_optional_path(&values, "RCDVLOG_BACKUP_DIR", backup_default);
        let allowed_original_mounts = values
            .get("RCDVLOG_ALLOWED_ORIGINAL_MOUNTS")
            .map(|value| {
                value
                    .split(',')
                    .filter(|part| !part.is_empty())
                    .map(PathBuf::from)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let listen_addr = values
            .get("RCDVLOG_LISTEN_ADDR")
            .map(String::as_str)
            .unwrap_or(DEFAULT_LISTEN_ADDR)
            .parse::<SocketAddr>()
            .map_err(|_| {
                ConfigError::new(LISTEN_ADDR_FIELD, ConfigErrorReason::InvalidSocketAddress)
            })?;
        let shutdown_timeout_seconds = match values.get("RCDVLOG_SHUTDOWN_TIMEOUT_SECONDS") {
            Some(value) => value.parse::<u64>().map_err(|_| {
                ConfigError::new(SHUTDOWN_TIMEOUT_FIELD, ConfigErrorReason::InvalidInteger)
            })?,
            None => DEFAULT_SHUTDOWN_TIMEOUT_SECONDS,
        };

        let config = Self {
            data_dir,
            temp_dir,
            derived_dir,
            backup_dir,
            allowed_original_mounts,
            listen_addr,
            shutdown_timeout: Duration::from_secs(shutdown_timeout_seconds),
            ffmpeg_command: configured_command(&values, "RCDVLOG_FFMPEG", "ffmpeg"),
            ffprobe_command: configured_command(&values, "RCDVLOG_FFPROBE", "ffprobe"),
            exiftool_command: configured_command(&values, "RCDVLOG_EXIFTOOL", "exiftool"),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_path(DATA_DIR_FIELD, &self.data_dir)?;
        validate_path(TEMP_DIR_FIELD, &self.temp_dir)?;
        validate_path(DERIVED_DIR_FIELD, &self.derived_dir)?;
        validate_path(BACKUP_DIR_FIELD, &self.backup_dir)?;
        for mount in &self.allowed_original_mounts {
            validate_path(ORIGINAL_MOUNTS_FIELD, mount)?;
        }

        if !self.temp_dir.starts_with(&self.data_dir) {
            return Err(ConfigError::new(
                TEMP_DIR_FIELD,
                ConfigErrorReason::MustBeInsideDataDirectory,
            ));
        }
        if self.derived_dir == self.data_dir {
            return Err(ConfigError::new(
                DERIVED_DIR_FIELD,
                ConfigErrorReason::MustBeDistinct,
            ));
        }
        if self.backup_dir == self.data_dir {
            return Err(ConfigError::new(
                BACKUP_DIR_FIELD,
                ConfigErrorReason::MustBeDistinct,
            ));
        }
        if paths_overlap(&self.temp_dir, &self.derived_dir) {
            return Err(ConfigError::new(
                DERIVED_DIR_FIELD,
                ConfigErrorReason::OverlapsWritableDirectory,
            ));
        }
        if paths_overlap(&self.temp_dir, &self.backup_dir)
            || paths_overlap(&self.derived_dir, &self.backup_dir)
        {
            return Err(ConfigError::new(
                BACKUP_DIR_FIELD,
                ConfigErrorReason::OverlapsWritableDirectory,
            ));
        }

        let mut unique_mounts = BTreeSet::new();
        for mount in &self.allowed_original_mounts {
            if !unique_mounts.insert(mount) {
                return Err(ConfigError::new(
                    ORIGINAL_MOUNTS_FIELD,
                    ConfigErrorReason::DuplicatePath,
                ));
            }
            for writable in [
                &self.data_dir,
                &self.temp_dir,
                &self.derived_dir,
                &self.backup_dir,
            ] {
                if paths_overlap(mount, writable) {
                    return Err(ConfigError::new(
                        ORIGINAL_MOUNTS_FIELD,
                        ConfigErrorReason::OverlapsOriginalMount,
                    ));
                }
            }
        }

        if !self.listen_addr.ip().is_loopback() {
            return Err(ConfigError::new(
                LISTEN_ADDR_FIELD,
                ConfigErrorReason::ListenerMustBeLoopback,
            ));
        }
        if !(1..=60).contains(&self.shutdown_timeout.as_secs()) {
            return Err(ConfigError::new(
                SHUTDOWN_TIMEOUT_FIELD,
                ConfigErrorReason::OutOfRange,
            ));
        }
        for (field, command) in [
            (FFMPEG_FIELD, self.ffmpeg_command.as_str()),
            (FFPROBE_FIELD, self.ffprobe_command.as_str()),
            (EXIFTOOL_FIELD, self.exiftool_command.as_str()),
        ] {
            if command.is_empty() {
                return Err(ConfigError::new(field, ConfigErrorReason::EmptyValue));
            }
        }
        Ok(())
    }

    pub fn diagnostics(&self) -> ConfigDiagnostics {
        ConfigDiagnostics {
            data_dir: self.data_dir.clone(),
            temp_dir: self.temp_dir.clone(),
            derived_dir: self.derived_dir.clone(),
            backup_dir: self.backup_dir.clone(),
            allowed_original_mounts: self.allowed_original_mounts.clone(),
            listen_addr: self.listen_addr.to_string(),
            shutdown_timeout_seconds: self.shutdown_timeout.as_secs(),
            ffmpeg_command: self.ffmpeg_command.clone(),
            ffprobe_command: self.ffprobe_command.clone(),
            exiftool_command: self.exiftool_command.clone(),
        }
    }
}

fn configured_path(values: &BTreeMap<String, String>, key: &str, default: &str) -> PathBuf {
    values
        .get(key)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

fn configured_optional_path(
    values: &BTreeMap<String, String>,
    key: &str,
    default: PathBuf,
) -> PathBuf {
    values.get(key).map(PathBuf::from).unwrap_or(default)
}

fn configured_command(values: &BTreeMap<String, String>, key: &str, default: &str) -> String {
    values
        .get(key)
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

fn validate_path(field: &'static str, path: &Path) -> Result<(), ConfigError> {
    if !path.is_absolute() {
        return Err(ConfigError::new(field, ConfigErrorReason::MustBeAbsolute));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(ConfigError::new(
            field,
            ConfigErrorReason::ContainsTraversal,
        ));
    }
    Ok(())
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn values() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("RCDVLOG_DATA_DIR".to_string(), "/srv/rcd/state".to_string()),
            (
                "RCDVLOG_DERIVED_DIR".to_string(),
                "/srv/rcd/derived".to_string(),
            ),
            (
                "RCDVLOG_BACKUP_DIR".to_string(),
                "/srv/rcd/backups".to_string(),
            ),
            (
                "RCDVLOG_ALLOWED_ORIGINAL_MOUNTS".to_string(),
                "/media/a,/media/b".to_string(),
            ),
        ])
    }

    #[test]
    fn defaults_are_loopback_and_temp_is_local_to_data() {
        let config = ServerConfig::from_values(BTreeMap::new()).unwrap();
        assert_eq!(config.data_dir, Path::new(DEFAULT_DATA_DIR));
        assert!(config.temp_dir.starts_with(&config.data_dir));
        assert!(config.listen_addr.ip().is_loopback());
        assert_eq!(config.shutdown_timeout.as_secs(), 10);
    }

    #[test]
    fn parses_mounts_and_redacted_diagnostics() {
        let config = ServerConfig::from_values(values()).unwrap();
        assert_eq!(config.allowed_original_mounts.len(), 2);
        let diagnostics = serde_json::to_value(config.diagnostics()).unwrap();
        assert_eq!(diagnostics["listenAddr"], json!("127.0.0.1:8090"));
    }

    #[test]
    fn rejects_lan_listener_and_mount_overlap_without_echoing_values() {
        let mut input = values();
        input.insert(
            "RCDVLOG_LISTEN_ADDR".to_string(),
            "0.0.0.0:8090".to_string(),
        );
        let error = ServerConfig::from_values(input).unwrap_err();
        assert_eq!(error.code, "invalid_configuration");
        assert_eq!(error.field, "listenAddr");
        assert!(!error.to_string().contains("0.0.0.0"));

        let mut input = values();
        input.insert(
            "RCDVLOG_ALLOWED_ORIGINAL_MOUNTS".to_string(),
            "/srv/rcd".to_string(),
        );
        let error = ServerConfig::from_values(input).unwrap_err();
        assert_eq!(error.field, "allowedOriginalMounts");
        assert_eq!(error.reason, ConfigErrorReason::OverlapsOriginalMount);
    }

    #[test]
    fn rejects_writable_directory_ancestry_overlap() {
        let mut input = values();
        input.insert(
            "RCDVLOG_BACKUP_DIR".to_string(),
            "/srv/rcd/derived/backup".to_string(),
        );
        let error = ServerConfig::from_values(input).unwrap_err();
        assert_eq!(error.field, "backupDir");
        assert_eq!(error.reason, ConfigErrorReason::OverlapsWritableDirectory);

        let mut input = values();
        input.insert(
            "RCDVLOG_DERIVED_DIR".to_string(),
            "/srv/rcd/state/tmp/derived".to_string(),
        );
        let error = ServerConfig::from_values(input).unwrap_err();
        assert_eq!(error.field, "derivedDir");
        assert_eq!(error.reason, ConfigErrorReason::OverlapsWritableDirectory);
    }

    #[test]
    fn secrets_never_debug_or_serialize_as_plaintext() {
        let secret = SecretString::new("do-not-leak");
        assert_eq!(format!("{secret:?}"), "[REDACTED]");
        assert_eq!(serde_json::to_value(&secret).unwrap(), json!("[REDACTED]"));
        assert_eq!(secret.expose(), "do-not-leak");
    }
}
