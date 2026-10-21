//! Persistence for cloud clip observations and their derived frame packs.
//!
//! Human notes are intentionally absent from every write in this module.
//! Titles and descriptions are AI-owned fields, while labels still travel
//! through the catalog's typed label operations; this module only records the
//! local authorship side-table needed by force and consolidation.

use anyhow::{Context, Result, bail};
use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::analysis::framepack::FramePackManifest;
use crate::analysis::framepack::REPRESENTATION;
use crate::analysis::schema::{CameraMovement, ClipObservation, UnusableReason};
use crate::analysis::{FRAMEPACK_RECIPE_KEY, VLM_PROMPT_VERSION};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisStatus {
    Ok,
    Rejected,
    Failed,
}

impl AnalysisStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "ok" => Ok(Self::Ok),
            "rejected" => Ok(Self::Rejected),
            "failed" => Ok(Self::Failed),
            other => bail!("unknown analysis status: {other}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredAnalysis {
    pub asset_id: String,
    pub recipe_key: String,
    pub prompt_version: String,
    pub provider: String,
    pub model: String,
    pub status: AnalysisStatus,
    pub reject_reason: String,
    pub usable: bool,
    pub unusable_reason: Option<UnusableReason>,
    pub best_moment_start_ms: Option<u32>,
    pub best_moment_end_ms: Option<u32>,
    pub thumbnail_timestamp_ms: Option<u32>,
    pub camera_movement: CameraMovement,
    pub ocr_text: Vec<String>,
    pub transcript_used: bool,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cost_usd: f64,
    pub latency_ms: u64,
    pub attempts: u32,
    #[serde(default)]
    pub attempted_models: Vec<String>,
    /// False if any provider attempt returned without usage data.
    #[serde(default = "historical_usage_is_complete")]
    pub usage_complete: bool,
    pub created_at: String,
}

fn historical_usage_is_complete() -> bool {
    true
}

impl StoredAnalysis {
    pub fn from_observation(asset_id: &str, model: &str, observation: &ClipObservation) -> Self {
        Self {
            asset_id: asset_id.to_string(),
            recipe_key: FRAMEPACK_RECIPE_KEY.to_string(),
            prompt_version: VLM_PROMPT_VERSION.to_string(),
            provider: "openrouter".to_string(),
            model: model.to_string(),
            status: AnalysisStatus::Ok,
            reject_reason: String::new(),
            usable: observation.usable,
            unusable_reason: observation.unusable_reason,
            best_moment_start_ms: observation.best_moment_start_ms,
            best_moment_end_ms: observation.best_moment_end_ms,
            thumbnail_timestamp_ms: observation.thumbnail_timestamp_ms,
            camera_movement: observation.camera_movement,
            ocr_text: observation.ocr_text.clone(),
            transcript_used: false,
            prompt_tokens: 0,
            completion_tokens: 0,
            cost_usd: 0.0,
            latency_ms: 0,
            attempts: 1,
            attempted_models: vec![model.to_string()],
            usage_complete: true,
            created_at: now(),
        }
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn enum_json<T: Serialize>(value: T) -> Result<String> {
    Ok(serde_json::to_string(&value)?.trim_matches('"').to_string())
}

pub fn save_analysis(connection: &Connection, analysis: &StoredAnalysis) -> Result<()> {
    connection.execute(
        r#"
        INSERT INTO asset_analyses
            (asset_id, recipe_key, prompt_version, provider, model, status, reject_reason,
             usable, unusable_reason, best_moment_start_ms, best_moment_end_ms,
             thumbnail_timestamp_ms, camera_movement, ocr_json, transcript_used,
             prompt_tokens, completion_tokens, cost_usd, latency_ms, attempts,
             attempted_models_json, usage_complete, created_at)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23)
        ON CONFLICT(asset_id) DO UPDATE SET
            recipe_key=excluded.recipe_key, prompt_version=excluded.prompt_version,
            provider=excluded.provider, model=excluded.model, status=excluded.status,
            reject_reason=excluded.reject_reason, usable=excluded.usable,
            unusable_reason=excluded.unusable_reason,
            best_moment_start_ms=excluded.best_moment_start_ms,
            best_moment_end_ms=excluded.best_moment_end_ms,
            thumbnail_timestamp_ms=excluded.thumbnail_timestamp_ms,
            camera_movement=excluded.camera_movement, ocr_json=excluded.ocr_json,
            transcript_used=excluded.transcript_used, prompt_tokens=excluded.prompt_tokens,
            completion_tokens=excluded.completion_tokens, cost_usd=excluded.cost_usd,
            latency_ms=excluded.latency_ms, attempts=excluded.attempts,
            attempted_models_json=excluded.attempted_models_json,
            usage_complete=excluded.usage_complete,
            created_at=excluded.created_at
        "#,
        params![
            analysis.asset_id,
            analysis.recipe_key,
            analysis.prompt_version,
            analysis.provider,
            analysis.model,
            analysis.status.as_str(),
            analysis.reject_reason,
            analysis.usable,
            analysis
                .unusable_reason
                .map(enum_json)
                .transpose()?
                .unwrap_or_default(),
            analysis.best_moment_start_ms,
            analysis.best_moment_end_ms,
            analysis.thumbnail_timestamp_ms,
            enum_json(analysis.camera_movement)?,
            serde_json::to_string(&analysis.ocr_text)?,
            analysis.transcript_used,
            analysis.prompt_tokens,
            analysis.completion_tokens,
            analysis.cost_usd,
            analysis.latency_ms as i64,
            analysis.attempts,
            serde_json::to_string(&analysis.attempted_models)?,
            analysis.usage_complete,
            analysis.created_at,
        ],
    )?;
    Ok(())
}

pub fn load_analysis(connection: &Connection, asset_id: &str) -> Result<Option<StoredAnalysis>> {
    connection
        .query_row(
            r#"
            SELECT recipe_key,prompt_version,provider,model,status,reject_reason,usable,
                   unusable_reason,best_moment_start_ms,best_moment_end_ms,
                   thumbnail_timestamp_ms,camera_movement,ocr_json,transcript_used,
                   prompt_tokens,completion_tokens,cost_usd,latency_ms,attempts,
                   attempted_models_json,usage_complete,created_at
            FROM asset_analyses WHERE asset_id=?1
            "#,
            [asset_id],
            |row| {
                let unusable: String = row.get(7)?;
                let movement: String = row.get(11)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, bool>(6)?,
                    unusable,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, Option<i64>>(10)?,
                    movement,
                    row.get::<_, String>(12)?,
                    row.get::<_, bool>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, i64>(15)?,
                    row.get::<_, f64>(16)?,
                    row.get::<_, i64>(17)?,
                    row.get::<_, i64>(18)?,
                    row.get::<_, String>(19)?,
                    row.get::<_, bool>(20)?,
                    row.get::<_, String>(21)?,
                ))
            },
        )
        .optional()?
        .map(|row| {
            Ok(StoredAnalysis {
                asset_id: asset_id.to_string(),
                recipe_key: row.0,
                prompt_version: row.1,
                provider: row.2,
                model: row.3,
                status: AnalysisStatus::parse(&row.4)?,
                reject_reason: row.5,
                usable: row.6,
                unusable_reason: if row.7.is_empty() {
                    None
                } else {
                    Some(
                        serde_json::from_str(&format!("\"{}\"", row.7))
                            .context("parse unusable reason")?,
                    )
                },
                best_moment_start_ms: row.8.map(|value| value as u32),
                best_moment_end_ms: row.9.map(|value| value as u32),
                thumbnail_timestamp_ms: row.10.map(|value| value as u32),
                camera_movement: serde_json::from_str(&format!("\"{}\"", row.11))
                    .context("parse camera movement")?,
                ocr_text: serde_json::from_str(&row.12).context("parse OCR JSON")?,
                transcript_used: row.13,
                prompt_tokens: row.14 as u32,
                completion_tokens: row.15 as u32,
                cost_usd: row.16,
                latency_ms: row.17 as u64,
                attempts: row.18 as u32,
                attempted_models: serde_json::from_str(&row.19)
                    .context("parse attempted analysis models JSON")?,
                usage_complete: row.20,
                created_at: row.21,
            })
        })
        .transpose()
}

pub fn save_frame_pack(
    connection: &Connection,
    asset_id: &str,
    manifest: &FramePackManifest,
) -> Result<()> {
    connection.execute(
        r#"
        INSERT INTO asset_frame_packs
            (asset_id,recipe_key,representation,source_fingerprint,manifest_json,created_at)
        VALUES (?1,?2,?3,?4,?5,?6)
        ON CONFLICT(asset_id) DO UPDATE SET recipe_key=excluded.recipe_key,
          representation=excluded.representation,source_fingerprint=excluded.source_fingerprint,
          manifest_json=excluded.manifest_json,created_at=excluded.created_at
        "#,
        params![
            asset_id,
            manifest.recipe_key,
            manifest.representation,
            manifest.source_fingerprint,
            serde_json::to_string(manifest)?,
            now(),
        ],
    )?;
    Ok(())
}

pub fn load_frame_pack(
    connection: &Connection,
    asset_id: &str,
) -> Result<Option<FramePackManifest>> {
    connection
        .query_row(
            "SELECT manifest_json FROM asset_frame_packs WHERE asset_id=?1",
            [asset_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|json| serde_json::from_str(&json).context("parse frame-pack manifest"))
        .transpose()
}

pub fn analysis_is_current(
    connection: &Connection,
    asset_id: &str,
    proxy_fingerprint: &str,
) -> Result<bool> {
    connection
        .query_row(
            r#"
        SELECT EXISTS(
          SELECT 1 FROM asset_analyses a JOIN asset_frame_packs f ON f.asset_id=a.asset_id
          WHERE a.asset_id=?1 AND a.recipe_key=?2 AND a.prompt_version=?3
            AND f.recipe_key=?2 AND f.representation=?4 AND f.source_fingerprint=?5
        )
        "#,
            params![
                asset_id,
                FRAMEPACK_RECIPE_KEY,
                VLM_PROMPT_VERSION,
                REPRESENTATION,
                proxy_fingerprint
            ],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

pub fn record_ai_labels(
    connection: &Connection,
    asset_id: &str,
    labels: &[(String, String)],
) -> Result<()> {
    let created_at = now();
    for (proposed, display) in labels {
        let normalized = normalize_label(display);
        if normalized.is_empty() {
            continue;
        }
        connection.execute(
            r#"
            INSERT INTO analysis_labels
                (asset_id,normalized_label,display_label,proposed_label,created_at)
            VALUES (?1,?2,?3,?4,?5)
            ON CONFLICT(asset_id,normalized_label) DO UPDATE SET
              display_label=excluded.display_label,proposed_label=excluded.proposed_label,
              created_at=excluded.created_at
            "#,
            params![
                asset_id,
                normalized,
                display.trim(),
                proposed.trim(),
                created_at
            ],
        )?;
    }
    Ok(())
}

pub fn ai_labels_for_asset(connection: &Connection, asset_id: &str) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT display_label FROM analysis_labels WHERE asset_id=?1 ORDER BY normalized_label",
    )?;
    Ok(statement
        .query_map([asset_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?)
}

pub fn clear_ai_labels(connection: &Connection, asset_id: &str) -> Result<()> {
    connection.execute("DELETE FROM analysis_labels WHERE asset_id=?1", [asset_id])?;
    Ok(())
}

pub fn set_title_and_description(
    connection: &Connection,
    asset_id: &str,
    title: &str,
    description: &str,
) -> Result<()> {
    connection.execute(
        "UPDATE assets SET title=?2, description=?3 WHERE id=?1",
        params![asset_id, title, description],
    )?;
    Ok(())
}

pub fn commit_thumbnail(
    connection: &Connection,
    asset_id: &str,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow::anyhow!("thumbnail has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = destination.with_extension("tmp.jpg");
    std::fs::copy(source, &temporary).context("stage VLM thumbnail")?;
    std::fs::rename(&temporary, destination).context("commit VLM thumbnail")?;
    connection.execute(
        r#"
        INSERT INTO asset_thumbnails(asset_id,quality,path,created_at) VALUES (?1,10,?2,?3)
        ON CONFLICT(asset_id,quality) DO UPDATE SET path=excluded.path,created_at=excluded.created_at
        "#,
        params![asset_id, destination.to_string_lossy(), now()],
    )?;
    Ok(())
}

pub fn normalize_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::migrate_project_catalog;

    fn database() -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        migrate_project_catalog(&mut connection).unwrap();
        connection
            .execute(
                "INSERT INTO projects(id,name,created_at) VALUES ('p1','p','2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        connection.execute("INSERT INTO assets(id,project_id,fingerprint,path,filename,size_bytes,duration_seconds,notes,created_at) VALUES ('a1','p1','f','/x','x.mp4',1,1.0,'human note','2026-01-01T00:00:00Z')", []).unwrap();
        connection
    }

    fn analysis() -> StoredAnalysis {
        StoredAnalysis {
            asset_id: "a1".into(),
            recipe_key: FRAMEPACK_RECIPE_KEY.into(),
            prompt_version: VLM_PROMPT_VERSION.into(),
            provider: "openrouter".into(),
            model: "m".into(),
            status: AnalysisStatus::Ok,
            reject_reason: String::new(),
            usable: true,
            unusable_reason: None,
            best_moment_start_ms: Some(1),
            best_moment_end_ms: Some(2),
            thumbnail_timestamp_ms: Some(1),
            camera_movement: CameraMovement::Pan,
            ocr_text: vec!["EXPO".into()],
            transcript_used: true,
            prompt_tokens: 12,
            completion_tokens: 4,
            cost_usd: 0.001,
            latency_ms: 44,
            attempts: 2,
            attempted_models: vec!["model-primary".into(), "model-fallback".into()],
            usage_complete: true,
            created_at: now(),
        }
    }

    #[test]
    fn an_analysis_round_trips_with_its_provenance() {
        let connection = database();
        save_analysis(&connection, &analysis()).unwrap();
        let loaded = load_analysis(&connection, "a1").unwrap().unwrap();
        assert_eq!(loaded.model, "m");
        assert_eq!(loaded.prompt_tokens, 12);
        assert_eq!(loaded.cost_usd, 0.001);
        assert_eq!(loaded.attempts, 2);
        assert_eq!(loaded.attempted_models, analysis().attempted_models);
        assert!(loaded.usage_complete);
    }

    #[test]
    fn provider_provenance_survives_catalog_reopen_for_success_and_incomplete_usage() {
        let directory = tempfile::tempdir().unwrap();
        let catalog_path = directory.path().join("analysis-provenance.sqlite3");
        let catalog = crate::Catalog::open(&catalog_path).unwrap();
        let project = catalog.create_project("analysis provenance test").unwrap();
        let connection = catalog.connection().unwrap();
        for id in ["a-success", "a-failed"] {
            connection
                .execute(
                    "INSERT INTO assets(id,project_id,fingerprint,path,filename,size_bytes,duration_seconds,created_at) VALUES (?1,?2,?3,?4,'x.mp4',1,1.0,'2026-01-01T00:00:00Z')",
                    rusqlite::params![id, project.id, format!("fingerprint-{id}"), format!("/{id}.mp4")],
                )
                .unwrap();
        }

        let mut success = analysis();
        success.asset_id = "a-success".into();
        success.model = "fallback-model".into();
        success.attempted_models = vec!["primary-model".into(), "fallback-model".into()];
        success.usage_complete = true;
        success.cost_usd = 0.002;
        save_analysis(&connection, &success).unwrap();

        let mut failed = analysis();
        failed.asset_id = "a-failed".into();
        failed.model.clear();
        failed.status = AnalysisStatus::Failed;
        failed.reject_reason = "both providers failed".into();
        failed.attempts = 2;
        failed.attempted_models = vec!["primary-model".into(), "fallback-model".into()];
        failed.usage_complete = false;
        failed.cost_usd = 0.001;
        save_analysis(&connection, &failed).unwrap();

        drop(connection);
        drop(catalog);
        let reopened = crate::Catalog::open(&catalog_path).unwrap();
        let connection = reopened.connection().unwrap();
        let loaded_success = load_analysis(&connection, "a-success").unwrap().unwrap();
        assert_eq!(loaded_success.attempted_models, success.attempted_models);
        assert!(loaded_success.usage_complete);
        assert_eq!(loaded_success.cost_usd, 0.002);

        let loaded_failed = load_analysis(&connection, "a-failed").unwrap().unwrap();
        assert_eq!(loaded_failed.status, AnalysisStatus::Failed);
        assert!(loaded_failed.model.is_empty());
        assert_eq!(loaded_failed.attempted_models, failed.attempted_models);
        assert!(!loaded_failed.usage_complete);
        assert_eq!(loaded_failed.cost_usd, 0.001);
    }

    #[test]
    fn a_frame_pack_from_a_different_proxy_is_stale() {
        let connection = database();
        let manifest = FramePackManifest {
            recipe_key: FRAMEPACK_RECIPE_KEY.into(),
            representation: REPRESENTATION.into(),
            source_fingerprint: "old".into(),
            timestamps_ms: vec![400],
        };
        save_frame_pack(&connection, "a1", &manifest).unwrap();
        save_analysis(&connection, &analysis()).unwrap();
        assert!(analysis_is_current(&connection, "a1", "old").unwrap());
        assert!(!analysis_is_current(&connection, "a1", "new").unwrap());
    }

    #[test]
    fn saving_an_analysis_never_writes_notes() {
        let connection = database();
        save_analysis(&connection, &analysis()).unwrap();
        set_title_and_description(&connection, "a1", "AI title", "AI description").unwrap();
        let note: String = connection
            .query_row("SELECT notes FROM assets WHERE id='a1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(note, "human note");
    }

    #[test]
    fn ai_labels_are_recorded_separately_from_human_labels() {
        let connection = database();
        connection
            .execute(
                "UPDATE assets SET labels_json='[\"human\",\"AI Label\"]' WHERE id='a1'",
                [],
            )
            .unwrap();
        record_ai_labels(&connection, "a1", &[("AI Label".into(), "AI Label".into())]).unwrap();
        assert_eq!(
            ai_labels_for_asset(&connection, "a1").unwrap(),
            vec!["AI Label"]
        );
    }
}
