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
pub const DEFAULT_TRUSTED_PROXY_CIDRS: &str = "127.0.0.1/32,::1/128";
pub const DEFAULT_LAN_HOSTNAME: &str = "rcdvlog.local";
pub const DEFAULT_SESSION_MAX_AGE_SECONDS: u64 = 86400;
pub const DEFAULT_BOOTSTRAP_TTL_SECONDS: u64 = 300;
pub const DEFAULT_PAIRING_TTL_SECONDS: u64 = 600;
pub const DEFAULT_BACKUP_INTERVAL_SECONDS: u64 = 86_400;
pub const DEFAULT_BACKUP_DAILY_RETENTION: u64 = 14;
pub const DEFAULT_BACKUP_WEEKLY_RETENTION: u64 = 8;
pub const DEFAULT_MIN_FREE_BYTES: u64 = 5 * 1024 * 1024 * 1024;

const DATA_DIR_FIELD: &str = "dataDir";
const TEMP_DIR_FIELD: &str = "tempDir";
const DERIVED_DIR_FIELD: &str = "derivedDir";
const BACKUP_DIR_FIELD: &str = "backupDir";
const ORIGINAL_MOUNTS_FIELD: &str = "allowedOriginalMounts";
const LISTEN_ADDR_FIELD: &str = "listenAddr";
const SHUTDOWN_TIMEOUT_FIELD: &str = "shutdownTimeoutSeconds";
const TRUSTED_PROXY_CIDRS_FIELD: &str = "trustedProxyCidrs";
const LAN_HOSTNAME_FIELD: &str = "lanHostname";
const SESSION_MAX_AGE_FIELD: &str = "sessionMaxAgeSeconds";
const BOOTSTRAP_TTL_FIELD: &str = "bootstrapCodeTtlSeconds";
const PAIRING_TTL_FIELD: &str = "pairingCodeTtlSeconds";
const BACKUP_INTERVAL_FIELD: &str = "backupIntervalSeconds";
const BACKUP_DAILY_RETENTION_FIELD: &str = "backupDailyRetention";
const BACKUP_WEEKLY_RETENTION_FIELD: &str = "backupWeeklyRetention";
const MIN_FREE_BYTES_FIELD: &str = "minimumFreeBytes";
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
    InvalidHostname,
    InvalidCidr,
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
            Self::InvalidHostname => "must be a valid LAN hostname",
            Self::InvalidCidr => "must be a valid CIDR notation (e.g. 127.0.0.1/32)",
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
    pub trusted_proxy_cidrs: Vec<String>,
    pub lan_hostname: String,
    pub session_max_age: Duration,
    pub bootstrap_code_ttl: Duration,
    pub pairing_code_ttl: Duration,
    pub backup_interval: Duration,
    pub backup_daily_retention: usize,
    pub backup_weekly_retention: usize,
    pub minimum_free_bytes: u64,
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
    pub trusted_proxy_cidr_count: usize,
    pub lan_hostname: String,
    pub session_max_age_seconds: u64,
    pub bootstrap_code_ttl_seconds: u64,
    pub pairing_code_ttl_seconds: u64,
    pub backup_interval_seconds: u64,
    pub backup_daily_retention: usize,
    pub backup_weekly_retention: usize,
    pub minimum_free_bytes: u64,
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
            ("RCDVLOG_TRUSTED_PROXY_CIDRS", TRUSTED_PROXY_CIDRS_FIELD),
            ("RCDVLOG_LAN_HOSTNAME", LAN_HOSTNAME_FIELD),
            ("RCDVLOG_SESSION_MAX_AGE_SECONDS", SESSION_MAX_AGE_FIELD),
            ("RCDVLOG_BOOTSTRAP_CODE_TTL_SECONDS", BOOTSTRAP_TTL_FIELD),
            ("RCDVLOG_PAIRING_CODE_TTL_SECONDS", PAIRING_TTL_FIELD),
            ("RCDVLOG_BACKUP_INTERVAL_SECONDS", BACKUP_INTERVAL_FIELD),
            (
                "RCDVLOG_BACKUP_DAILY_RETENTION",
                BACKUP_DAILY_RETENTION_FIELD,
            ),
            (
                "RCDVLOG_BACKUP_WEEKLY_RETENTION",
                BACKUP_WEEKLY_RETENTION_FIELD,
            ),
            ("RCDVLOG_MINIMUM_FREE_BYTES", MIN_FREE_BYTES_FIELD),
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
        let trusted_proxy_cidrs = values
            .get("RCDVLOG_TRUSTED_PROXY_CIDRS")
            .map(|value| {
                value
                    .split(',')
                    .filter(|part| !part.is_empty())
                    .map(|part| part.trim().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| {
                DEFAULT_TRUSTED_PROXY_CIDRS
                    .split(',')
                    .map(|part| part.trim().to_string())
                    .collect()
            });
        let lan_hostname = values
            .get("RCDVLOG_LAN_HOSTNAME")
            .cloned()
            .unwrap_or_else(|| DEFAULT_LAN_HOSTNAME.to_string());
        let session_max_age_seconds = parse_u64_or_default(
            values.get("RCDVLOG_SESSION_MAX_AGE_SECONDS"),
            DEFAULT_SESSION_MAX_AGE_SECONDS,
        );
        let bootstrap_code_ttl_seconds = parse_u64_or_default(
            values.get("RCDVLOG_BOOTSTRAP_CODE_TTL_SECONDS"),
            DEFAULT_BOOTSTRAP_TTL_SECONDS,
        );
        let pairing_code_ttl_seconds = parse_u64_or_default(
            values.get("RCDVLOG_PAIRING_CODE_TTL_SECONDS"),
            DEFAULT_PAIRING_TTL_SECONDS,
        );
        let backup_interval_seconds = parse_u64_or_default(
            values.get("RCDVLOG_BACKUP_INTERVAL_SECONDS"),
            DEFAULT_BACKUP_INTERVAL_SECONDS,
        );
        let backup_daily_retention = parse_u64_or_default(
            values.get("RCDVLOG_BACKUP_DAILY_RETENTION"),
            DEFAULT_BACKUP_DAILY_RETENTION,
        );
        let backup_weekly_retention = parse_u64_or_default(
            values.get("RCDVLOG_BACKUP_WEEKLY_RETENTION"),
            DEFAULT_BACKUP_WEEKLY_RETENTION,
        );
        let minimum_free_bytes = parse_u64_or_default(
            values.get("RCDVLOG_MINIMUM_FREE_BYTES"),
            DEFAULT_MIN_FREE_BYTES,
        );

        let config = Self {
            data_dir,
            temp_dir,
            derived_dir,
            backup_dir,
            allowed_original_mounts,
            listen_addr,
            shutdown_timeout: Duration::from_secs(shutdown_timeout_seconds),
            trusted_proxy_cidrs,
            lan_hostname,
            session_max_age: Duration::from_secs(session_max_age_seconds),
            bootstrap_code_ttl: Duration::from_secs(bootstrap_code_ttl_seconds),
            pairing_code_ttl: Duration::from_secs(pairing_code_ttl_seconds),
            backup_interval: Duration::from_secs(backup_interval_seconds),
            backup_daily_retention: usize::try_from(backup_daily_retention).map_err(|_| {
                ConfigError::new(BACKUP_DAILY_RETENTION_FIELD, ConfigErrorReason::OutOfRange)
            })?,
            backup_weekly_retention: usize::try_from(backup_weekly_retention).map_err(|_| {
                ConfigError::new(BACKUP_WEEKLY_RETENTION_FIELD, ConfigErrorReason::OutOfRange)
            })?,
            minimum_free_bytes,
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
        for cidr in &self.trusted_proxy_cidrs {
            if cidr.is_empty() || !cidr.contains('/') {
                return Err(ConfigError::new(
                    TRUSTED_PROXY_CIDRS_FIELD,
                    ConfigErrorReason::InvalidCidr,
                ));
            }
        }
        if self.lan_hostname.is_empty() {
            return Err(ConfigError::new(
                LAN_HOSTNAME_FIELD,
                ConfigErrorReason::EmptyValue,
            ));
        }
        for (field, seconds) in [
            (SESSION_MAX_AGE_FIELD, self.session_max_age.as_secs()),
            (BOOTSTRAP_TTL_FIELD, self.bootstrap_code_ttl.as_secs()),
            (PAIRING_TTL_FIELD, self.pairing_code_ttl.as_secs()),
        ] {
            if !(30..=86400 * 365).contains(&seconds) {
                return Err(ConfigError::new(field, ConfigErrorReason::OutOfRange));
            }
        }
        if !(300..=86400 * 30).contains(&self.backup_interval.as_secs()) {
            return Err(ConfigError::new(
                BACKUP_INTERVAL_FIELD,
                ConfigErrorReason::OutOfRange,
            ));
        }
        if !(1..=365).contains(&self.backup_daily_retention) {
            return Err(ConfigError::new(
                BACKUP_DAILY_RETENTION_FIELD,
                ConfigErrorReason::OutOfRange,
            ));
        }
        if !(1..=104).contains(&self.backup_weekly_retention) {
            return Err(ConfigError::new(
                BACKUP_WEEKLY_RETENTION_FIELD,
                ConfigErrorReason::OutOfRange,
            ));
        }
        if self.minimum_free_bytes < 64 * 1024 * 1024 {
            return Err(ConfigError::new(
                MIN_FREE_BYTES_FIELD,
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
            trusted_proxy_cidr_count: self.trusted_proxy_cidrs.len(),
            lan_hostname: self.lan_hostname.clone(),
            session_max_age_seconds: self.session_max_age.as_secs(),
            bootstrap_code_ttl_seconds: self.bootstrap_code_ttl.as_secs(),
            pairing_code_ttl_seconds: self.pairing_code_ttl.as_secs(),
            backup_interval_seconds: self.backup_interval.as_secs(),
            backup_daily_retention: self.backup_daily_retention,
            backup_weekly_retention: self.backup_weekly_retention,
            minimum_free_bytes: self.minimum_free_bytes,
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

fn parse_u64_or_default(value: Option<&String>, default: u64) -> u64 {
    value.and_then(|v| v.parse::<u64>().ok()).unwrap_or(default)
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
