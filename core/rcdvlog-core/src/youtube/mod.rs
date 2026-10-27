//! Opt-in YouTube publishing. Credentials are node-local; the catalog owns the upload ledger.
mod credentials;
mod oauth;
mod upload;

pub use credentials::{AuthStatus, CredentialStore};
pub use oauth::{LoginOptions, login, revoke};
pub use upload::{UploadOptions, UploadRecord, UploadResult};

use anyhow::{Result, bail};
use reqwest::blocking::{Client, Response};
use serde_json::Value;
use std::io::Read;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const UPLOAD_SCOPE: &str = "https://www.googleapis.com/auth/youtube.upload";

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct YoutubeError {
    pub code: &'static str,
    pub message: &'static str,
}

fn failure<T>(code: &'static str, message: &'static str) -> Result<T> {
    Err(YoutubeError { code, message }.into())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// No response bodies, credentials, authorization codes, or resumable URLs in errors.
fn transport(_: reqwest::Error) -> anyhow::Error {
    YoutubeError {
        code: "youtube.network_error",
        message: "YouTube request failed; rerun the command to recover its saved session",
    }
    .into()
}

fn http_client() -> Result<Client> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(transport)
}

fn response_json(response: Response) -> Result<Value> {
    let mut bytes = Vec::new();
    response
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| YoutubeError {
            code: "youtube.protocol_error",
            message: "Unable to read YouTube response",
        })?;
    if bytes.len() > 65536 {
        return failure(
            "youtube.protocol_error",
            "YouTube response exceeded the size limit",
        );
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        YoutubeError {
            code: "youtube.protocol_error",
            message: "YouTube returned an invalid JSON response",
        }
        .into()
    })
}

#[derive(Clone)]
struct Endpoints {
    authorize: String,
    token: String,
    revoke: String,
    upload: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            authorize: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token: "https://oauth2.googleapis.com/token".into(),
            revoke: "https://oauth2.googleapis.com/revoke".into(),
            upload: "https://www.googleapis.com/upload/youtube/v3/videos".into(),
        }
    }
}

fn exact_scope(scope: &str) -> Result<()> {
    let scopes: Vec<_> = scope.split_whitespace().collect();
    if scopes != [UPLOAD_SCOPE] {
        return failure(
            "youtube.scope_mismatch",
            "Expected exactly youtube.upload; use a dedicated OAuth client and authorize again",
        );
    }
    Ok(())
}

pub(crate) fn migrate(transaction: &rusqlite::Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS youtube_uploads (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            credential_id TEXT NOT NULL,
            file_sha256 TEXT NOT NULL,
            file_path TEXT NOT NULL,
            file_size INTEGER NOT NULL CHECK(file_size > 0),
            metadata_json TEXT NOT NULL,
            cut_revision TEXT,
            state TEXT NOT NULL CHECK(state IN ('pending','initiating','uploading','complete')),
            session_uri TEXT,
            uploaded_bytes INTEGER NOT NULL DEFAULT 0,
            video_id TEXT,
            channel_id TEXT,
            actual_privacy TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(project_id, credential_id, file_sha256)
        );",
    )?;
    Ok(())
}

fn validate_text(value: &str, max_bytes: usize, label: &str) -> Result<()> {
    if value.len() > max_bytes || value.contains('\0') {
        bail!("invalid input: {label} is too long or contains a NUL character");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
