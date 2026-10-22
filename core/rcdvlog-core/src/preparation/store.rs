use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TranscriptBinding {
    pub artifact_fingerprint: String,
    pub origin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ValidationReceipt {
    pub id: String,
    pub asset_id: String,
    pub project_id: String,
    pub source_fingerprint: String,
    pub source_duration_ms: u32,
    pub validated_end_ms: u32,
    pub representation_fingerprint: String,
    pub representation_recipe: String,
    pub validation_recipe: String,
    pub validator: String,
    pub decoded: bool,
    pub diagnostic: Option<String>,
    pub validated_at: String,
    pub transcript: Option<TranscriptBinding>,
    #[serde(default)]
    pub transcript_error: Option<String>,
}

pub(super) fn schema_ready(connection: &Connection) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='preparation_validation_receipts')",
        [],
        |row| row.get(0),
    )?)
}

pub(super) fn load(
    connection: &Connection,
    asset_id: &str,
    representation_fingerprint: &str,
) -> Result<Option<ValidationReceipt>> {
    if !schema_ready(connection)? {
        return Ok(None);
    }
    let body: Option<String> = connection.query_row(
        "SELECT body_json FROM preparation_validation_receipts WHERE asset_id=?1 AND representation_fingerprint=?2",
        params![asset_id, representation_fingerprint],
        |row| row.get(0),
    ).optional()?;
    body.map(|body| serde_json::from_str(&body).context("decode preparation validation receipt"))
        .transpose()
}

pub(super) fn has_history(connection: &Connection, asset_id: &str) -> Result<bool> {
    if !schema_ready(connection)? {
        return Ok(false);
    }
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM preparation_validation_receipts WHERE asset_id=?1)",
        [asset_id],
        |row| row.get(0),
    )?)
}

pub(super) fn bound_to_other_representation(
    connection: &Connection,
    asset_id: &str,
    representation_fingerprint: &str,
    transcript_fingerprint: &str,
) -> Result<bool> {
    if !schema_ready(connection)? {
        return Ok(false);
    }
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM preparation_validation_receipts WHERE asset_id=?1 AND representation_fingerprint<>?2 AND json_extract(body_json,'$.transcript.artifactFingerprint')=?3)",
        params![asset_id, representation_fingerprint, transcript_fingerprint],
        |row| row.get(0),
    )?)
}

pub(super) fn save(connection: &Connection, receipt: &ValidationReceipt) -> Result<()> {
    connection.execute(
        "INSERT INTO preparation_validation_receipts(asset_id,representation_fingerprint,body_json) VALUES(?1,?2,?3) ON CONFLICT(asset_id,representation_fingerprint) DO UPDATE SET body_json=excluded.body_json",
        params![receipt.asset_id, receipt.representation_fingerprint, serde_json::to_string(receipt)?],
    )?;
    Ok(())
}
