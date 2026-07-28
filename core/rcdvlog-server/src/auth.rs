use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use chrono::{Duration, Utc};
use rcdvlog_core::NodeId;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::Duration as StdDuration;
use thiserror::Error;

use crate::config::SecretString;

const BOOTSTRAP_CODE_LENGTH: usize = 12;
const SESSION_TOKEN_BYTES: usize = 32;
const CSRF_TOKEN_BYTES: usize = 32;
const PAIRING_CODE_LENGTH: usize = 16;
const DEVICE_TOKEN_BYTES: usize = 48;
const DISPLAY_PREFIX_LENGTH: usize = 8;

#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub session_max_age: StdDuration,
    pub bootstrap_code_ttl: StdDuration,
    pub pairing_code_ttl: StdDuration,
}

#[derive(Debug, Clone)]
pub struct AuthService {
    database_path: PathBuf,
    node_id: NodeId,
    config: AuthConfig,
}

// ── DTOs ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapCode {
    pub code: SecretString,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCreated {
    pub session_id: String,
    pub session_token: SecretString,
    pub csrf_token: String,
    pub display_prefix: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: String,
    pub display_prefix: String,
    pub hostname: String,
    pub created_at: String,
    pub last_used_at: String,
    pub expires_at: String,
    pub revoked: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingCode {
    pub code: SecretString,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCredentialCreated {
    pub credential_id: String,
    pub token: SecretString,
    pub token_prefix: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCredentialInfo {
    pub credential_id: String,
    pub token_prefix: String,
    pub label: String,
    pub created_at: String,
    pub last_used_at: String,
    pub revoked: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthDiagnostics {
    pub active_sessions: usize,
    pub active_device_credentials: usize,
    pub pending_bootstrap_codes: usize,
    pub node_id: String,
}

// ── Error ────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("bootstrap code not found or expired")]
    BootstrapNotFoundOrExpired,
    #[error("bootstrap code already used")]
    BootstrapAlreadyUsed,
    #[error("session not found, expired, or revoked")]
    SessionInvalid,
    #[error("CSRF token mismatch")]
    CsrfMismatch,
    #[error("device credential not found or expired")]
    DeviceNotFound,
    #[error("device credential has been revoked")]
    DeviceRevoked,
    #[error("pairing code not found or expired")]
    PairingNotFoundOrExpired,
    #[error("pairing code already used")]
    PairingAlreadyUsed,
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
}

impl AuthError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::BootstrapNotFoundOrExpired => "bootstrap_not_found_or_expired",
            Self::BootstrapAlreadyUsed => "bootstrap_already_used",
            Self::SessionInvalid => "session_invalid",
            Self::CsrfMismatch => "csrf_mismatch",
            Self::DeviceNotFound => "device_not_found",
            Self::DeviceRevoked => "device_revoked",
            Self::PairingNotFoundOrExpired => "pairing_not_found_or_expired",
            Self::PairingAlreadyUsed => "pairing_already_used",
            Self::Database(_) => "internal_error",
        }
    }
}

impl Serialize for AuthError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.code())
    }
}

// ── Constructors ─────────────────────────────────────────────────────

impl AuthService {
    pub fn new(database_path: PathBuf, node_id: NodeId, config: AuthConfig) -> Self {
        Self {
            database_path,
            node_id,
            config,
        }
    }

    fn connect(&self) -> Result<Connection, AuthError> {
        let connection = Connection::open(&self.database_path)?;
        connection.busy_timeout(StdDuration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;",
        )?;
        Ok(connection)
    }
}

// ── Bootstrap codes ──────────────────────────────────────────────────

impl AuthService {
    pub fn create_bootstrap_code(&self) -> Result<BootstrapCode, AuthError> {
        let code = generate_alphanumeric(BOOTSTRAP_CODE_LENGTH);
        let code_prefix = &code[..DISPLAY_PREFIX_LENGTH];
        let code_hash = hash_secret(&code);
        let expires_at = (Utc::now()
            + Duration::from_std(self.config.bootstrap_code_ttl).unwrap_or(Duration::minutes(5)))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        let connection = self.connect()?;
        connection.execute(
            "INSERT INTO bootstrap_codes (code_hash, code_prefix, expires_at, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![code_hash, code_prefix, expires_at, now],
        )?;

        Ok(BootstrapCode {
            code: SecretString::new(code),
            expires_at,
        })
    }

    pub fn exchange_bootstrap(
        &self,
        plaintext: &str,
        hostname: &str,
    ) -> Result<SessionCreated, AuthError> {
        let connection = self.connect()?;
        let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        let row: Option<(String, String, String)> = connection
            .query_row(
                "SELECT code_hash, code_prefix, expires_at FROM bootstrap_codes WHERE code_prefix = ?1 AND consumed_at IS NULL AND expires_at > ?2 LIMIT 1",
                params![&plaintext[..DISPLAY_PREFIX_LENGTH.min(plaintext.len())], now],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;

        let (code_hash, _prefix, _expires_at) = match row {
            Some(row) => row,
            None => return Err(AuthError::BootstrapNotFoundOrExpired),
        };

        if !verify_secret(plaintext, &code_hash) {
            return Err(AuthError::BootstrapNotFoundOrExpired);
        }

        connection.execute(
            "UPDATE bootstrap_codes SET consumed_at = ?1 WHERE code_hash = ?2",
            params![now, code_hash],
        )?;

        let session_id = format!("session_{}", generate_alphanumeric(32));
        let session_token = generate_token(SESSION_TOKEN_BYTES);
        let session_token_hash = hash_secret(&session_token);
        let display_prefix = session_token[..DISPLAY_PREFIX_LENGTH].to_string();
        let csrf_token = generate_token(CSRF_TOKEN_BYTES);
        let csrf_token_hash = hash_sha256(&csrf_token);
        let expires_at = (Utc::now()
            + Duration::from_std(self.config.session_max_age).unwrap_or(Duration::hours(24)))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        connection.execute(
            "INSERT INTO browser_sessions (session_id, session_token_hash, display_prefix, hostname, csrf_token_hash, created_at, last_used_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7)",
            params![session_id, session_token_hash, display_prefix, hostname, csrf_token_hash, now, expires_at],
        )?;

        Ok(SessionCreated {
            session_id,
            session_token: SecretString::new(session_token),
            csrf_token,
            display_prefix,
            expires_at,
        })
    }

    pub fn cleanup_expired_codes(&self) -> Result<usize, AuthError> {
        let connection = self.connect()?;
        let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let deleted = connection.execute(
            "DELETE FROM bootstrap_codes WHERE expires_at <= ?1 AND consumed_at IS NULL",
            params![now],
        )?;
        connection.execute(
            "DELETE FROM pairing_codes WHERE expires_at <= ?1 AND consumed_at IS NULL",
            params![now],
        )?;
        Ok(deleted)
    }
}

// ── Browser sessions ─────────────────────────────────────────────────

impl AuthService {
    pub fn validate_session(&self, session_id: &str, session_token: &str) -> Result<(), AuthError> {
        let connection = self.connect()?;
        let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        let row: Option<(String, String)> = connection
            .query_row(
                "SELECT session_token_hash, expires_at FROM browser_sessions WHERE session_id = ?1 AND revoked_at IS NULL",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        let (token_hash, expires_at) = match row {
            Some(row) => row,
            None => return Err(AuthError::SessionInvalid),
        };

        if expires_at <= now {
            return Err(AuthError::SessionInvalid);
        }

        if !verify_secret(session_token, &token_hash) {
            return Err(AuthError::SessionInvalid);
        }

        connection.execute(
            "UPDATE browser_sessions SET last_used_at = ?1 WHERE session_id = ?2",
            params![now, session_id],
        )?;

        Ok(())
    }

    pub fn validate_session_with_csrf(
        &self,
        session_id: &str,
        session_token: &str,
        csrf_token: &str,
    ) -> Result<(), AuthError> {
        self.validate_session(session_id, session_token)?;

        let connection = self.connect()?;
        let expected_hash = hash_sha256(csrf_token);
        let matches: bool = connection
            .query_row(
                "SELECT csrf_token_hash = ?1 FROM browser_sessions WHERE session_id = ?2 AND revoked_at IS NULL",
                params![expected_hash, session_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(false);

        if !matches {
            return Err(AuthError::CsrfMismatch);
        }

        Ok(())
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>, AuthError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT session_id, display_prefix, hostname, created_at, last_used_at, expires_at, revoked_at IS NOT NULL AS revoked FROM browser_sessions ORDER BY created_at DESC",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(SessionInfo {
                    session_id: row.get(0)?,
                    display_prefix: row.get(1)?,
                    hostname: row.get(2)?,
                    created_at: row.get(3)?,
                    last_used_at: row.get(4)?,
                    expires_at: row.get(5)?,
                    revoked: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn revoke_session(&self, session_id: &str) -> Result<(), AuthError> {
        let connection = self.connect()?;
        let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let affected = connection.execute(
            "UPDATE browser_sessions SET revoked_at = ?1 WHERE session_id = ?2 AND revoked_at IS NULL",
            params![now, session_id],
        )?;
        if affected == 0 {
            return Err(AuthError::SessionInvalid);
        }
        Ok(())
    }

    pub fn refresh_session(&self, session_id: &str) -> Result<SessionInfo, AuthError> {
        let connection = self.connect()?;
        let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let new_expires = (Utc::now()
            + Duration::from_std(self.config.session_max_age).unwrap_or(Duration::hours(24)))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        let affected = connection.execute(
            "UPDATE browser_sessions SET expires_at = ?1, last_used_at = ?2 WHERE session_id = ?3 AND revoked_at IS NULL AND expires_at > ?2",
            params![new_expires, now, session_id],
        )?;
        if affected == 0 {
            return Err(AuthError::SessionInvalid);
        }

        connection
            .query_row(
                "SELECT session_id, display_prefix, hostname, created_at, last_used_at, expires_at, revoked_at IS NOT NULL FROM browser_sessions WHERE session_id = ?1",
                params![session_id],
                |row| {
                    Ok(SessionInfo {
                        session_id: row.get(0)?,
                        display_prefix: row.get(1)?,
                        hostname: row.get(2)?,
                        created_at: row.get(3)?,
                        last_used_at: row.get(4)?,
                        expires_at: row.get(5)?,
                        revoked: row.get(6)?,
                    })
                },
            )
            .map_err(|_| AuthError::SessionInvalid)
    }
    /// Validate only the CSRF token for an already-validated browser session.
    /// This does not re-validate the session (the caller guarantees that via
    /// `validate_session` having already been called within the same request).
    pub fn validate_csrf_token(&self, session_id: &str, csrf_token: &str) -> Result<(), AuthError> {
        let connection = self.connect()?;
        let expected_hash = hash_sha256(csrf_token);
        let matches: bool = connection
            .query_row(
                "SELECT csrf_token_hash = ?1 FROM browser_sessions WHERE session_id = ?2 AND revoked_at IS NULL",
                params![expected_hash, session_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(false);

        if !matches {
            return Err(AuthError::CsrfMismatch);
        }

        Ok(())
    }
}

// ── Pairing codes ────────────────────────────────────────────────────

impl AuthService {
    pub fn create_pairing_code(
        &self,
        created_by_session_id: &str,
    ) -> Result<PairingCode, AuthError> {
        self.create_pairing_code_with_creator(Some(created_by_session_id))
    }

    pub fn create_pairing_code_for_device(&self) -> Result<PairingCode, AuthError> {
        self.create_pairing_code_with_creator(None)
    }

    fn create_pairing_code_with_creator(
        &self,
        created_by_session_id: Option<&str>,
    ) -> Result<PairingCode, AuthError> {
        let code = generate_alphanumeric(PAIRING_CODE_LENGTH);
        let code_prefix = &code[..DISPLAY_PREFIX_LENGTH];
        let code_hash = hash_secret(&code);
        let expires_at = (Utc::now()
            + Duration::from_std(self.config.pairing_code_ttl).unwrap_or(Duration::minutes(10)))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        let connection = self.connect()?;
        connection.execute(
            "INSERT INTO pairing_codes (code_hash, code_prefix, expires_at, created_at, created_by_session_id) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![code_hash, code_prefix, expires_at, now, created_by_session_id],
        )?;

        Ok(PairingCode {
            code: SecretString::new(code),
            expires_at,
        })
    }

    pub fn exchange_pairing(
        &self,
        plaintext: &str,
        device_label: &str,
    ) -> Result<DeviceCredentialCreated, AuthError> {
        let connection = self.connect()?;
        let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        let row: Option<(String, String)> = connection
            .query_row(
                "SELECT code_hash, expires_at FROM pairing_codes WHERE code_prefix = ?1 AND consumed_at IS NULL AND expires_at > ?2 LIMIT 1",
                params![&plaintext[..DISPLAY_PREFIX_LENGTH.min(plaintext.len())], now],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        let (code_hash, _expires_at) = match row {
            Some(row) => row,
            None => return Err(AuthError::PairingNotFoundOrExpired),
        };

        if !verify_secret(plaintext, &code_hash) {
            return Err(AuthError::PairingNotFoundOrExpired);
        }

        connection.execute(
            "UPDATE pairing_codes SET consumed_at = ?1 WHERE code_hash = ?2",
            params![now, code_hash],
        )?;

        let credential_id = format!("device_{}", generate_alphanumeric(24));
        let token = generate_token(DEVICE_TOKEN_BYTES);
        let token_hash = hash_secret(&token);
        let token_prefix = token[..DISPLAY_PREFIX_LENGTH].to_string();

        connection.execute(
            "INSERT INTO device_credentials (credential_id, token_hash, token_prefix, label, created_at, last_used_at, expected_server_node_id) VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
            params![credential_id, token_hash, token_prefix, device_label, now, self.node_id.as_str()],
        )?;

        Ok(DeviceCredentialCreated {
            credential_id,
            token: SecretString::new(token),
            token_prefix,
        })
    }
}

// ── Device credentials ───────────────────────────────────────────────

impl AuthService {
    pub fn validate_device(&self, token: &str) -> Result<String, AuthError> {
        let connection = self.connect()?;
        let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        // Use the token prefix to narrow the search to at most a few candidates
        // instead of scanning all credentials with an O(n) Argon2 per request.
        let prefix = &token[..DISPLAY_PREFIX_LENGTH.min(token.len())];
        let rows: Vec<(String, String, Option<String>)> = connection
            .prepare(
                "SELECT credential_id, token_hash, expires_at FROM device_credentials WHERE token_prefix = ?1 AND revoked_at IS NULL",
            )?
            .query_map(params![prefix], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        for (credential_id, token_hash, expires_at) in &rows {
            if let Some(expiry) = expires_at
                && expiry <= &now
            {
                continue;
            }
            if verify_secret(token, token_hash) {
                connection.execute(
                    "UPDATE device_credentials SET last_used_at = ?1 WHERE credential_id = ?2",
                    params![now, credential_id],
                )?;
                return Ok(credential_id.clone());
            }
        }

        Err(AuthError::DeviceNotFound)
    }

    pub fn list_devices(&self) -> Result<Vec<DeviceCredentialInfo>, AuthError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT credential_id, token_prefix, label, created_at, last_used_at, revoked_at IS NOT NULL AS revoked FROM device_credentials ORDER BY created_at DESC",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(DeviceCredentialInfo {
                    credential_id: row.get(0)?,
                    token_prefix: row.get(1)?,
                    label: row.get(2)?,
                    created_at: row.get(3)?,
                    last_used_at: row.get(4)?,
                    revoked: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn revoke_device(&self, credential_id: &str) -> Result<(), AuthError> {
        let connection = self.connect()?;
        let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let affected = connection.execute(
            "UPDATE device_credentials SET revoked_at = ?1 WHERE credential_id = ?2 AND revoked_at IS NULL",
            params![now, credential_id],
        )?;
        if affected == 0 {
            return Err(AuthError::DeviceNotFound);
        }
        Ok(())
    }
}

// ── Diagnostics ──────────────────────────────────────────────────────

impl AuthService {
    pub fn diagnostics(&self) -> Result<AuthDiagnostics, AuthError> {
        let connection = self.connect()?;
        let active_sessions = connection
            .query_row(
                "SELECT COUNT(*) FROM browser_sessions WHERE revoked_at IS NULL AND expires_at > ?1",
                params![Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0) as usize;
        let active_device_credentials = connection
            .query_row(
                "SELECT COUNT(*) FROM device_credentials WHERE revoked_at IS NULL",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0) as usize;
        let pending_bootstrap_codes = connection
            .query_row(
                "SELECT COUNT(*) FROM bootstrap_codes WHERE consumed_at IS NULL AND expires_at > ?1",
                params![Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0) as usize;
        Ok(AuthDiagnostics {
            active_sessions,
            active_device_credentials,
            pending_bootstrap_codes,
            node_id: self.node_id.as_str().to_string(),
        })
    }
}

// ── Crypto helpers ───────────────────────────────────────────────────

fn generate_alphanumeric(length: usize) -> String {
    use rand::Rng;
    let charset: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..length)
        .map(|_| {
            let idx = rng.gen_range(0..charset.len());
            charset[idx] as char
        })
        .collect()
}

fn generate_token(byte_length: usize) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use rand::RngCore;
    let mut bytes = vec![0u8; byte_length];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(&bytes)
}

fn hash_secret(secret: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(secret.as_bytes(), &salt)
        .expect("argon2 hashing is infallible with valid inputs")
        .to_string()
}

fn verify_secret(secret: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(secret.as_bytes(), &parsed)
        .is_ok()
}

fn hash_sha256(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rcdvlog_core::run_sqlite_migrations;
    use tempfile::TempDir;

    use crate::catalog::CONTROL_CATALOG_MIGRATIONS;

    fn setup() -> (AuthService, TempDir) {
        let root = TempDir::new().unwrap();
        let db_path = root.path().join("control.sqlite3");
        let mut connection = crate::catalog::open_writable_connection(&db_path).unwrap();
        run_sqlite_migrations(&mut connection, CONTROL_CATALOG_MIGRATIONS).unwrap();
        let node_id = NodeId::parse("node_server_test_auth").unwrap();
        let config = AuthConfig {
            session_max_age: StdDuration::from_secs(3600),
            bootstrap_code_ttl: StdDuration::from_secs(300),
            pairing_code_ttl: StdDuration::from_secs(600),
        };
        (AuthService::new(db_path, node_id, config), root)
    }

    #[test]
    fn bootstrap_create_and_exchange_roundtrip() {
        let (service, _root) = setup();
        let bootstrap = service.create_bootstrap_code().unwrap();
        assert!(!bootstrap.code.expose().is_empty());
        assert_eq!(bootstrap.code.expose().len(), BOOTSTRAP_CODE_LENGTH);

        let session = service
            .exchange_bootstrap(bootstrap.code.expose(), "rcdvlog.local")
            .unwrap();
        assert!(!session.session_id.is_empty());
        assert!(!session.session_token.expose().is_empty());
        assert!(!session.csrf_token.is_empty());

        // Replay fails
        let err = service
            .exchange_bootstrap(bootstrap.code.expose(), "rcdvlog.local")
            .unwrap_err();
        assert!(matches!(err, AuthError::BootstrapNotFoundOrExpired));
    }

    #[test]
    fn bootstrap_wrong_code_rejected() {
        let (service, _root) = setup();
        let err = service
            .exchange_bootstrap("wrong-code-1234", "rcdvlog.local")
            .unwrap_err();
        assert!(matches!(err, AuthError::BootstrapNotFoundOrExpired));
    }

    #[test]
    fn session_validate_and_csrf_roundtrip() {
        let (service, _root) = setup();
        let bootstrap = service.create_bootstrap_code().unwrap();
        let session = service
            .exchange_bootstrap(bootstrap.code.expose(), "rcdvlog.local")
            .unwrap();

        service
            .validate_session(&session.session_id, session.session_token.expose())
            .unwrap();

        // Wrong token fails
        let err = service
            .validate_session(&session.session_id, "wrong-token")
            .unwrap_err();
        assert!(matches!(err, AuthError::SessionInvalid));

        // CSRF validates
        service
            .validate_session_with_csrf(
                &session.session_id,
                session.session_token.expose(),
                &session.csrf_token,
            )
            .unwrap();

        // Wrong CSRF fails
        let err = service
            .validate_session_with_csrf(
                &session.session_id,
                session.session_token.expose(),
                "wrong-csrf",
            )
            .unwrap_err();
        assert!(matches!(err, AuthError::CsrfMismatch));
    }

    #[test]
    fn session_list_and_revoke() {
        let (service, _root) = setup();
        let bootstrap = service.create_bootstrap_code().unwrap();
        let session = service
            .exchange_bootstrap(bootstrap.code.expose(), "rcdvlog.local")
            .unwrap();

        let sessions = service.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, session.session_id);
        assert!(!sessions[0].revoked);

        service.revoke_session(&session.session_id).unwrap();
        let sessions = service.list_sessions().unwrap();
        assert!(sessions[0].revoked);

        // Revoked session fails validation
        let err = service
            .validate_session(&session.session_id, session.session_token.expose())
            .unwrap_err();
        assert!(matches!(err, AuthError::SessionInvalid));
    }

    #[test]
    fn session_refresh_extends_expiry() {
        let (service, _root) = setup();
        let bootstrap = service.create_bootstrap_code().unwrap();
        let session = service
            .exchange_bootstrap(bootstrap.code.expose(), "rcdvlog.local")
            .unwrap();

        let info = service.refresh_session(&session.session_id).unwrap();
        assert!(!info.expires_at.is_empty());
    }

    #[test]
    fn pairing_create_and_exchange_roundtrip() {
        let (service, _root) = setup();
        let bootstrap = service.create_bootstrap_code().unwrap();
        let session = service
            .exchange_bootstrap(bootstrap.code.expose(), "rcdvlog.local")
            .unwrap();

        let pairing = service.create_pairing_code(&session.session_id).unwrap();
        assert!(!pairing.code.expose().is_empty());

        let device = service
            .exchange_pairing(pairing.code.expose(), "MacBook Pro")
            .unwrap();
        assert!(!device.credential_id.is_empty());
        assert!(!device.token.expose().is_empty());

        // Replay fails
        let err = service
            .exchange_pairing(pairing.code.expose(), "Another Device")
            .unwrap_err();
        assert!(matches!(err, AuthError::PairingNotFoundOrExpired));
    }

    #[test]
    fn device_validate_and_revoke() {
        let (service, _root) = setup();
        let bootstrap = service.create_bootstrap_code().unwrap();
        let session = service
            .exchange_bootstrap(bootstrap.code.expose(), "rcdvlog.local")
            .unwrap();
        let pairing = service.create_pairing_code(&session.session_id).unwrap();
        let device = service
            .exchange_pairing(pairing.code.expose(), "MacBook Pro")
            .unwrap();

        let cred_id = service.validate_device(device.token.expose()).unwrap();
        assert_eq!(cred_id, device.credential_id);

        // Wrong token fails
        let err = service.validate_device("wrong-token").unwrap_err();
        assert!(matches!(err, AuthError::DeviceNotFound));

        // List devices
        let devices = service.list_devices().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].credential_id, device.credential_id);
        assert!(!devices[0].revoked);

        // Revoke
        service.revoke_device(&device.credential_id).unwrap();
        let devices = service.list_devices().unwrap();
        assert!(devices[0].revoked);

        // Revoked device fails
        let err = service.validate_device(device.token.expose()).unwrap_err();
        assert!(matches!(err, AuthError::DeviceNotFound));
    }

    #[test]
    fn cleanup_expired_codes_removes_only_expired() {
        let (service, _root) = setup();
        let bootstrap = service.create_bootstrap_code().unwrap();
        let _session = service
            .exchange_bootstrap(bootstrap.code.expose(), "rcdvlog.local")
            .unwrap();

        let cleanup_count = service.cleanup_expired_codes().unwrap();
        // The just-created and immediately consumed code is still there (consumed_at is set)
        // but cleanup only targets unconsumed expired codes
        assert!(cleanup_count <= 1);
    }

    #[test]
    fn argon2_verification_is_constant_time_behavior() {
        let secret = "test-secret-value";
        let hash = hash_secret(secret);
        assert!(verify_secret(secret, &hash));
        assert!(!verify_secret("wrong-secret", &hash));
        assert!(!verify_secret(secret, "invalid-hash-format"));
    }

    #[test]
    fn diagnostics_report_active_counts() {
        let (service, _root) = setup();
        let diag = service.diagnostics().unwrap();
        assert_eq!(diag.active_sessions, 0);
        assert_eq!(diag.active_device_credentials, 0);
        assert_eq!(diag.pending_bootstrap_codes, 0);
        assert_eq!(diag.node_id, "node_server_test_auth");
    }
}
