use chrono::{SecondsFormat, Utc};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use thiserror::Error;

use crate::{open_read_only_connection, open_writable_connection};

const MAX_DETAIL_BYTES: usize = 8_192;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditActorType {
    BrowserSession,
    DeviceCredential,
}

impl AuditActorType {
    fn as_str(&self) -> &'static str {
        match self {
            Self::BrowserSession => "browser_session",
            Self::DeviceCredential => "device_credential",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuditEvent {
    pub event_sequence: u64,
    pub occurred_at: String,
    pub actor_type: AuditActorType,
    pub actor_id: String,
    pub action: String,
    pub target_type: String,
    pub target_id: Option<String>,
    pub request_id: String,
    pub detail: Value,
}

#[derive(Debug, Error)]
pub enum AdminAuditError {
    #[error("invalid audit event: {0}")]
    InvalidInput(String),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Infrastructure(#[from] anyhow::Error),
}

#[derive(Clone)]
pub struct AdminAuditLog {
    database_path: PathBuf,
}

impl AdminAuditLog {
    pub(crate) fn new(database_path: PathBuf) -> Self {
        Self { database_path }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        actor_type: AuditActorType,
        actor_id: &str,
        action: &str,
        target_type: &str,
        target_id: Option<&str>,
        request_id: &str,
        detail: &Value,
    ) -> Result<AdminAuditEvent, AdminAuditError> {
        for (label, value, limit) in [
            ("actorId", actor_id, 255usize),
            ("action", action, 128),
            ("targetType", target_type, 128),
            ("requestId", request_id, 64),
        ] {
            if value.is_empty() || value.len() > limit {
                return Err(AdminAuditError::InvalidInput(format!(
                    "{label} has an invalid length"
                )));
            }
        }
        if target_id.is_some_and(|value| value.is_empty() || value.len() > 255) {
            return Err(AdminAuditError::InvalidInput(
                "targetId has an invalid length".to_string(),
            ));
        }
        reject_secret_keys(detail)?;
        let detail_json = serde_json::to_string(detail)?;
        if detail_json.len() > MAX_DETAIL_BYTES {
            return Err(AdminAuditError::InvalidInput(
                "detail exceeds the 8192-byte limit".to_string(),
            ));
        }
        let occurred_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let connection = open_writable_connection(&self.database_path)?;
        connection.execute(
            "INSERT INTO administrative_audit_events(occurred_at,actor_type,actor_id,action,target_type,target_id,request_id,detail_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![occurred_at, actor_type.as_str(), actor_id, action, target_type, target_id, request_id, detail_json],
        )?;
        let sequence = connection.last_insert_rowid();
        self.get(sequence)
    }

    pub fn list(
        &self,
        limit: u32,
        before: Option<u64>,
    ) -> Result<Vec<AdminAuditEvent>, AdminAuditError> {
        if limit == 0 || limit > 200 {
            return Err(AdminAuditError::InvalidInput(
                "limit must be between 1 and 200".to_string(),
            ));
        }
        let before = before
            .map(i64::try_from)
            .transpose()
            .map_err(|_| AdminAuditError::InvalidInput("before is too large".to_string()))?;
        let connection = open_read_only_connection(&self.database_path)?;
        let mut statement = connection.prepare(
            "SELECT event_sequence,occurred_at,actor_type,actor_id,action,target_type,target_id,request_id,detail_json FROM administrative_audit_events WHERE (?1 IS NULL OR event_sequence < ?1) ORDER BY event_sequence DESC LIMIT ?2",
        )?;
        statement
            .query_map(params![before, limit], audit_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(parse_event)
            .collect()
    }

    fn get(&self, sequence: i64) -> Result<AdminAuditEvent, AdminAuditError> {
        let connection = open_read_only_connection(&self.database_path)?;
        let event = connection
            .query_row(
                "SELECT event_sequence,occurred_at,actor_type,actor_id,action,target_type,target_id,request_id,detail_json FROM administrative_audit_events WHERE event_sequence=?1",
                [sequence],
                audit_from_row,
            )
            .optional()?
            .ok_or_else(|| AdminAuditError::InvalidInput("audit event disappeared".to_string()))?;
        parse_event(event)
    }
}

type AuditRow = (
    i64,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    String,
);

fn audit_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
    ))
}

fn parse_event(row: AuditRow) -> Result<AdminAuditEvent, AdminAuditError> {
    let actor_type = match row.2.as_str() {
        "browser_session" => AuditActorType::BrowserSession,
        "device_credential" => AuditActorType::DeviceCredential,
        value => {
            return Err(AdminAuditError::InvalidInput(format!(
                "unknown actor type {value}"
            )));
        }
    };
    Ok(AdminAuditEvent {
        event_sequence: u64::try_from(row.0)
            .map_err(|_| AdminAuditError::InvalidInput("negative event sequence".to_string()))?,
        occurred_at: row.1,
        actor_type,
        actor_id: row.3,
        action: row.4,
        target_type: row.5,
        target_id: row.6,
        request_id: row.7,
        detail: serde_json::from_str(&row.8)?,
    })
}

fn reject_secret_keys(value: &Value) -> Result<(), AdminAuditError> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let folded = key.to_ascii_lowercase();
                if ["token", "secret", "password", "credential", "cookie"]
                    .iter()
                    .any(|needle| folded.contains(needle))
                {
                    return Err(AdminAuditError::InvalidInput(format!(
                        "detail key {key} may contain a secret"
                    )));
                }
                reject_secret_keys(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                reject_secret_keys(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConfigDiagnostics, ControlCatalog};
    use tempfile::TempDir;

    fn catalog(root: &TempDir) -> ControlCatalog {
        let data = root.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        ControlCatalog::open_or_create(
            &data,
            &ConfigDiagnostics {
                data_dir: data.clone(),
                temp_dir: data.join("tmp"),
                derived_dir: data.join("derived"),
                backup_dir: data.join("backup"),
                allowed_original_mounts: vec![],
                listen_addr: "127.0.0.1:0".into(),
                shutdown_timeout_seconds: 1,
                trusted_proxy_cidr_count: 0,
                lan_hostname: "localhost".into(),
                session_max_age_seconds: 60,
                bootstrap_code_ttl_seconds: 60,
                pairing_code_ttl_seconds: 60,
                ffmpeg_command: "ffmpeg".into(),
                ffprobe_command: "ffprobe".into(),
                exiftool_command: "exiftool".into(),
            },
        )
        .unwrap()
    }

    #[test]
    fn audit_rows_survive_reopen_and_reject_secret_shaped_details() {
        let root = TempDir::new().unwrap();
        let control_catalog = catalog(&root);
        control_catalog
            .audit_log()
            .record(
                AuditActorType::BrowserSession,
                "session_1",
                "storage_root.created",
                "storage_root",
                Some("root_1"),
                "request_1",
                &serde_json::json!({"displayName":"Archive"}),
            )
            .unwrap();
        drop(control_catalog);
        let reopened = catalog(&root);
        assert_eq!(reopened.audit_log().list(10, None).unwrap().len(), 1);
        assert!(
            reopened
                .audit_log()
                .record(
                    AuditActorType::BrowserSession,
                    "session_1",
                    "bad",
                    "test",
                    None,
                    "request_2",
                    &serde_json::json!({"accessToken":"nope"})
                )
                .is_err()
        );
    }
}
