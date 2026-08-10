use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::{Catalog, Classification, ImportOutcome, ImportPreflight, now};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportHistoryEntry {
    pub id: String,
    pub project_id: String,
    pub source_path: String,
    pub destination_mode: String,
    pub destination_root_id: Option<String>,
    pub destination_name: String,
    pub destination_path: String,
    pub imported: usize,
    pub relinked: usize,
    pub skipped: usize,
    pub copied: usize,
    pub bytes_copied: u64,
    pub cancelled: bool,
    pub failures: Vec<crate::IngestFailure>,
    pub rolled_back_at: Option<String>,
    pub rolled_back_asset_count: usize,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSessionFile {
    pub asset_id: Option<String>,
    pub path: String,
    pub filename: String,
    pub classification: Classification,
    pub result: String,
    pub timestamp: Option<String>,
    pub duration_seconds: Option<f64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSessionDetail {
    #[serde(flatten)]
    pub entry: ImportHistoryEntry,
    pub files: Vec<ImportSessionFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRollbackPreview {
    pub session_id: String,
    pub removable_asset_count: usize,
    pub already_removed_asset_count: usize,
    pub modified_clips: Vec<ImportRollbackModifiedClip>,
    pub affected_phase_count: usize,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRollbackModifiedClip {
    pub asset_id: String,
    pub filename: String,
    pub reasons: Vec<String>,
}

impl Catalog {
    /// Persists the result immediately after an import finishes, including a
    /// cancellation. This is intentionally one small transaction after the
    /// asset commits: a stopped import remains resumable and its completed
    /// membership is still auditable/undoable.
    pub fn record_import_session(
        &self,
        project_id: &str,
        plan: &ImportPreflight,
        outcome: &ImportOutcome,
    ) -> Result<ImportSessionDetail> {
        self.require_project(project_id)?;
        let id = format!("import_{}", Uuid::new_v4().simple());
        let destination_mode = if plan.destination.copies {
            "archive_copy"
        } else {
            "reference"
        };
        let destination_root_id = plan
            .destination
            .archive_root
            .as_ref()
            .map(|root| root.id.clone());
        let destination_name = plan
            .destination
            .archive_root
            .as_ref()
            .map(|root| root.display_name.clone())
            .unwrap_or_else(|| "Originals stay in place".to_string());
        let failure_by_path: HashMap<&str, &str> = outcome
            .failures
            .iter()
            .map(|failure| (failure.path.as_str(), failure.error.as_str()))
            .collect();
        let mut imported = outcome.imported_asset_ids.iter();
        let mut relinked = outcome.relinked_asset_ids.iter();
        let mut files = Vec::with_capacity(plan.items.len());
        for item in &plan.items {
            let asset_id = match item.classification {
                Classification::New if !failure_by_path.contains_key(item.path.as_str()) => {
                    imported.next().cloned()
                }
                Classification::KnownRelinkable
                    if !failure_by_path.contains_key(item.path.as_str()) =>
                {
                    relinked.next().cloned()
                }
                _ => None,
            };
            let asset = asset_id
                .as_deref()
                .and_then(|asset_id| self.get_asset(asset_id).ok());
            let result = if let Some(error) = failure_by_path.get(item.path.as_str()) {
                let _ = error;
                "failed"
            } else if asset_id.is_some() {
                "completed"
            } else {
                "left_alone"
            };
            files.push(ImportSessionFile {
                asset_id,
                path: item.path.clone(),
                filename: item.filename.clone(),
                classification: item.classification,
                result: result.to_string(),
                timestamp: asset.as_ref().and_then(|asset| {
                    asset
                        .best_time
                        .clone()
                        .or_else(|| asset.captured_at_raw.clone())
                }),
                duration_seconds: asset.as_ref().map(|asset| asset.duration_seconds),
                error: failure_by_path
                    .get(item.path.as_str())
                    .map(|value| (*value).to_string())
                    .or_else(|| (!item.error.is_empty()).then(|| item.error.clone())),
            });
        }
        let created_at = now();
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        tx.execute("INSERT INTO import_sessions(session_id, project_id, source_path, destination_mode, destination_root_id, destination_name, destination_path, imported_count, relinked_count, skipped_count, copied_count, bytes_copied, cancelled, failures_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)", params![id, project_id, plan.source_path, destination_mode, destination_root_id, destination_name, plan.destination.resolved_path, outcome.imported as i64, outcome.relinked as i64, outcome.skipped as i64, outcome.copied as i64, outcome.bytes_copied as i64, outcome.cancelled, serde_json::to_string(&outcome.failures)?, created_at])?;
        for (ordinal, file) in files.iter().enumerate() {
            tx.execute("INSERT INTO import_session_files(session_id, ordinal, asset_id, path, filename, classification, result, timestamp, duration_seconds, error) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)", params![id, ordinal as i64, file.asset_id, file.path, file.filename, serde_json::to_value(file.classification)?.as_str(), file.result, file.timestamp, file.duration_seconds, file.error])?;
        }
        tx.commit()?;
        Ok(ImportSessionDetail {
            entry: ImportHistoryEntry {
                id,
                project_id: project_id.to_string(),
                source_path: plan.source_path.clone(),
                destination_mode: destination_mode.to_string(),
                destination_root_id,
                destination_name,
                destination_path: plan.destination.resolved_path.clone(),
                imported: outcome.imported,
                relinked: outcome.relinked,
                skipped: outcome.skipped,
                copied: outcome.copied,
                bytes_copied: outcome.bytes_copied,
                cancelled: outcome.cancelled,
                failures: outcome.failures.clone(),
                rolled_back_at: None,
                rolled_back_asset_count: 0,
                created_at,
            },
            files,
        })
    }

    pub fn list_import_sessions(&self, project_id: &str) -> Result<Vec<ImportHistoryEntry>> {
        self.require_project(project_id)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT session_id, project_id, source_path, destination_mode, destination_root_id, destination_name, destination_path, imported_count, relinked_count, skipped_count, copied_count, bytes_copied, cancelled, failures_json, rolled_back_at, rolled_back_asset_count, created_at FROM import_sessions WHERE project_id=?1 ORDER BY created_at DESC")?;
        statement
            .query_map([project_id], row_to_entry)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn import_session(
        &self,
        project_id: &str,
        session_id: &str,
    ) -> Result<ImportSessionDetail> {
        let entry = self.session_entry(project_id, session_id)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT asset_id, path, filename, classification, result, timestamp, duration_seconds, error FROM import_session_files WHERE session_id=?1 ORDER BY ordinal")?;
        let files = statement
            .query_map([session_id], |row| {
                Ok(ImportSessionFile {
                    asset_id: row.get(0)?,
                    path: row.get(1)?,
                    filename: row.get(2)?,
                    classification: serde_json::from_value(serde_json::Value::String(row.get(3)?))
                        .map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                5,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                    result: row.get(4)?,
                    timestamp: row.get(5)?,
                    duration_seconds: row.get(6)?,
                    error: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ImportSessionDetail { entry, files })
    }

    pub fn import_rollback_preview(
        &self,
        project_id: &str,
        session_id: &str,
    ) -> Result<ImportRollbackPreview> {
        let entry = self.session_entry(project_id, session_id)?;
        let imported_ids = self.session_imported_ids(session_id)?;
        let assets = self.list_assets(project_id)?;
        let by_id: HashMap<_, _> = assets
            .iter()
            .map(|asset| (asset.id.as_str(), asset))
            .collect();
        let mut modified_clips = Vec::new();
        let mut removable = 0;
        for id in &imported_ids {
            let Some(asset) = by_id.get(id.as_str()) else {
                continue;
            };
            removable += 1;
            let mut reasons = Vec::new();
            if !asset.notes.trim().is_empty() {
                reasons.push("notes".to_string());
            }
            if !asset.labels.is_empty() {
                reasons.push("labels".to_string());
            }
            if asset.removed {
                reasons.push("removed state".to_string());
            }
            if asset.rotation_degrees != asset.default_rotation_degrees {
                reasons.push("orientation".to_string());
            }
            if !asset.title.trim().is_empty() || !asset.description.trim().is_empty() {
                reasons.push("edited AI fields".to_string());
            }
            if asset.proxy_path.is_some() || asset.thumbnail_path.is_some() {
                reasons.push("derived review media".to_string());
            }
            if !reasons.is_empty() {
                modified_clips.push(ImportRollbackModifiedClip {
                    asset_id: id.clone(),
                    filename: asset.filename.clone(),
                    reasons,
                });
            }
        }
        let connection = self.connection()?;
        let placeholders = std::iter::repeat_n("?", imported_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let affected_phase_count = if imported_ids.is_empty() {
            0
        } else {
            connection.query_row(&format!("SELECT COUNT(*) FROM trip_phases WHERE start_anchor_asset_id IN ({placeholders}) OR end_anchor_asset_id IN ({placeholders})"), rusqlite::params_from_iter(imported_ids.iter().chain(imported_ids.iter())), |row| row.get::<_, i64>(0))? as usize
        };
        let already_removed = entry.rolled_back_asset_count.saturating_sub(removable);
        Ok(ImportRollbackPreview { session_id: session_id.to_string(), removable_asset_count: removable, already_removed_asset_count: already_removed, modified_clips, affected_phase_count, message: "This removes only project membership created by this import. Original files, archive copies, annotations, and derived review records remain in the catalog and return if the membership is restored. Cached bytes remain until cache cleanup.".to_string() })
    }

    pub fn rollback_import_membership(
        &self,
        project_id: &str,
        session_id: &str,
    ) -> Result<ImportRollbackPreview> {
        let preview = self.import_rollback_preview(project_id, session_id)?;
        if preview.removable_asset_count == 0 {
            return Ok(preview);
        }
        let ids = self.session_imported_ids(session_id)?;
        self.remove_project_asset_membership(project_id, &ids)?;
        let connection = self.connection()?;
        connection.execute("UPDATE import_sessions SET rolled_back_at=COALESCE(rolled_back_at, ?3), rolled_back_asset_count=rolled_back_asset_count + ?4 WHERE session_id=?1 AND project_id=?2", params![session_id, project_id, now(), preview.removable_asset_count as i64])?;
        self.import_rollback_preview(project_id, session_id)
    }

    fn session_entry(&self, project_id: &str, session_id: &str) -> Result<ImportHistoryEntry> {
        self.require_project(project_id)?;
        self.connection()?.query_row("SELECT session_id, project_id, source_path, destination_mode, destination_root_id, destination_name, destination_path, imported_count, relinked_count, skipped_count, copied_count, bytes_copied, cancelled, failures_json, rolled_back_at, rolled_back_asset_count, created_at FROM import_sessions WHERE session_id=?1 AND project_id=?2", params![session_id, project_id], row_to_entry).optional()?.ok_or_else(|| anyhow::anyhow!("import session not found: {session_id}"))
    }

    fn session_imported_ids(&self, session_id: &str) -> Result<Vec<String>> {
        let connection = self.connection()?;
        let mut stmt = connection.prepare("SELECT asset_id FROM import_session_files WHERE session_id=?1 AND classification='new' AND result='completed' AND asset_id IS NOT NULL ORDER BY ordinal")?;
        Ok(stmt
            .query_map([session_id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImportHistoryEntry> {
    let failures: String = row.get(13)?;
    Ok(ImportHistoryEntry {
        id: row.get(0)?,
        project_id: row.get(1)?,
        source_path: row.get(2)?,
        destination_mode: row.get(3)?,
        destination_root_id: row.get(4)?,
        destination_name: row.get(5)?,
        destination_path: row.get(6)?,
        imported: row.get::<_, i64>(7)? as usize,
        relinked: row.get::<_, i64>(8)? as usize,
        skipped: row.get::<_, i64>(9)? as usize,
        copied: row.get::<_, i64>(10)? as usize,
        bytes_copied: row.get::<_, i64>(11)? as u64,
        cancelled: row.get(12)?,
        failures: serde_json::from_str(&failures).unwrap_or_default(),
        rolled_back_at: row.get(14)?,
        rolled_back_asset_count: row.get::<_, i64>(15)? as usize,
        created_at: row.get(16)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::ProjectAssetAddedPayload;
    use crate::{AssetPatch, ImportDestination, ImportPreflight, PreflightItem};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn membership_rollback_hides_only_the_selected_asset_and_is_reversible() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Import history").unwrap();
        for id in ["asset_imported", "asset_existing"] {
            catalog
                .add_portable_project_asset(
                    &project.id,
                    ProjectAssetAddedPayload {
                        id: id.to_string(),
                        fingerprint: format!("fp_{id}"),
                        filename: format!("{id}.MP4"),
                        size_bytes: 1,
                        duration_seconds: 1.0,
                        frame_rate: 30.0,
                        width: 1,
                        height: 1,
                        codec: "h264".to_string(),
                        camera: "Test".to_string(),
                        captured_at_raw: None,
                        best_time: None,
                        metadata_rotation_degrees: 0,
                        default_rotation_degrees: 0,
                        created_at: now(),
                    },
                )
                .unwrap();
        }
        catalog
            .remove_project_asset_membership(&project.id, &["asset_imported".to_string()])
            .unwrap();
        assert_eq!(
            catalog
                .list_assets(&project.id)
                .unwrap()
                .iter()
                .map(|asset| asset.id.as_str())
                .collect::<Vec<_>>(),
            vec!["asset_existing"]
        );
        catalog
            .restore_project_asset_membership(&project.id, "asset_imported")
            .unwrap();
        assert_eq!(catalog.count_project_assets(&project.id).unwrap(), 2);
    }

    fn add_asset(catalog: &Catalog, project_id: &str, id: &str) {
        catalog
            .add_portable_project_asset(
                project_id,
                ProjectAssetAddedPayload {
                    id: id.to_string(),
                    fingerprint: format!("fp_{id}"),
                    filename: format!("{id}.MP4"),
                    size_bytes: 1,
                    duration_seconds: 1.0,
                    frame_rate: 30.0,
                    width: 1,
                    height: 1,
                    codec: "h264".to_string(),
                    camera: "Test".to_string(),
                    captured_at_raw: None,
                    best_time: None,
                    metadata_rotation_degrees: 0,
                    default_rotation_degrees: 0,
                    created_at: now(),
                },
            )
            .unwrap();
    }

    #[test]
    fn session_ledger_excludes_relinked_and_known_assets_from_membership_rollback() {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog.create_project("Import history").unwrap();
        add_asset(&catalog, &project.id, "asset_new");
        add_asset(&catalog, &project.id, "asset_relinked");
        add_asset(&catalog, &project.id, "asset_existing");
        catalog
            .update_asset(
                "asset_new",
                &AssetPatch {
                    notes: "Keep this annotation".to_string(),
                    labels: vec!["hero shot".to_string()],
                    removed: false,
                    rotation_degrees: 0,
                },
            )
            .unwrap();
        let proxy = directory.path().join("asset_new.proxy.mp4");
        let thumbnail = directory.path().join("asset_new.representative.jpg");
        fs::write(&proxy, b"derived proxy").unwrap();
        fs::write(&thumbnail, b"derived thumbnail").unwrap();
        let connection = catalog.connection().unwrap();
        connection
            .execute(
                "UPDATE assets SET proxy_path = ?2 WHERE id = ?1",
                rusqlite::params!["asset_new", proxy.to_string_lossy()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO asset_thumbnails(asset_id, quality, path, created_at) VALUES (?1, 10, ?2, ?3)",
                rusqlite::params!["asset_new", thumbnail.to_string_lossy(), now()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO asset_analyses(asset_id, recipe_key, prompt_version, provider, model, status, best_moment_start_ms, best_moment_end_ms, thumbnail_timestamp_ms, created_at) VALUES (?1, 'test.recipe', 'test.prompt', 'test', 'test-model', 'ok', 1000, 2000, 1500, ?2)",
                rusqlite::params!["asset_new", now()],
            )
            .unwrap();
        let plan = ImportPreflight {
            source_path: "/Volumes/CARD".to_string(),
            card: None,
            destination: ImportDestination {
                copies: false,
                archive_root: None,
                resolved_path: "/Volumes/CARD".to_string(),
                summary: "Originals stay in place".to_string(),
                bytes_to_copy: 0,
                available_bytes: None,
                ready: true,
                blocked_reason: String::new(),
            },
            scanned: 3,
            new_clips: 1,
            known_clips: 1,
            relinkable: 1,
            missing_proxies: 0,
            collisions: 0,
            unreadable: 0,
            items: vec![
                PreflightItem {
                    path: "/Volumes/CARD/new.MP4".to_string(),
                    relative_path: "new.MP4".to_string(),
                    filename: "new.MP4".to_string(),
                    size_bytes: 1,
                    classification: Classification::New,
                    asset_id: None,
                    error: String::new(),
                },
                PreflightItem {
                    path: "/Volumes/CARD/relinked.MP4".to_string(),
                    relative_path: "relinked.MP4".to_string(),
                    filename: "relinked.MP4".to_string(),
                    size_bytes: 1,
                    classification: Classification::KnownRelinkable,
                    asset_id: Some("asset_relinked".to_string()),
                    error: String::new(),
                },
                PreflightItem {
                    path: "/Volumes/CARD/existing.MP4".to_string(),
                    relative_path: "existing.MP4".to_string(),
                    filename: "existing.MP4".to_string(),
                    size_bytes: 1,
                    classification: Classification::KnownVerified,
                    asset_id: Some("asset_existing".to_string()),
                    error: String::new(),
                },
            ],
        };
        let detail = catalog
            .record_import_session(
                &project.id,
                &plan,
                &ImportOutcome {
                    imported: 1,
                    imported_asset_ids: vec!["asset_new".to_string()],
                    relinked: 1,
                    relinked_asset_ids: vec!["asset_relinked".to_string()],
                    skipped: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(detail.entry.source_path, "/Volumes/CARD");
        assert_eq!(detail.entry.destination_mode, "reference");
        assert_eq!(
            detail
                .files
                .iter()
                .filter(|file| file.result == "completed")
                .map(|file| file.asset_id.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("asset_new"), Some("asset_relinked")]
        );

        let before = catalog
            .import_rollback_preview(&project.id, &detail.entry.id)
            .unwrap();
        assert_eq!(before.removable_asset_count, 1);
        assert!(before.message.contains("remain in the catalog"));
        assert_eq!(before.modified_clips.len(), 1);
        assert_eq!(
            before.modified_clips[0].reasons,
            vec!["notes", "labels", "derived review media"]
        );
        let after_first = catalog
            .rollback_import_membership(&project.id, &detail.entry.id)
            .unwrap();
        let after_second = catalog
            .rollback_import_membership(&project.id, &detail.entry.id)
            .unwrap();
        assert_eq!(after_first.removable_asset_count, 0);
        assert_eq!(after_second.removable_asset_count, 0);
        assert!(
            catalog.get_asset("asset_new").is_ok(),
            "identity and locations survive rollback"
        );
        assert_eq!(
            catalog
                .list_assets(&project.id)
                .unwrap()
                .iter()
                .map(|asset| asset.id.as_str())
                .collect::<Vec<_>>(),
            vec!["asset_existing", "asset_relinked"]
        );
        assert_eq!(
            catalog
                .export_project_baseline(&project.id)
                .unwrap()
                .assets
                .iter()
                .map(|asset| asset.id.as_str())
                .collect::<Vec<_>>(),
            vec!["asset_existing", "asset_relinked"],
            "a fresh peer baseline must not revive rolled-back membership"
        );
        let hidden = catalog.get_asset("asset_new").unwrap();
        assert_eq!(hidden.notes, "Keep this annotation");
        assert_eq!(hidden.labels, ["hero shot"]);
        assert_eq!(hidden.proxy_path.as_deref(), proxy.to_str());
        assert_eq!(hidden.thumbnail_path.as_deref(), thumbnail.to_str());
        assert_eq!(hidden.thumbnail_quality, 10);
        let hidden_analysis = catalog.asset_analysis("asset_new").unwrap().unwrap();
        assert_eq!(hidden_analysis.analysis.best_moment_start_ms, Some(1_000));
        assert_eq!(hidden_analysis.analysis.best_moment_end_ms, Some(2_000));
        assert_eq!(hidden_analysis.analysis.thumbnail_timestamp_ms, Some(1_500));
        catalog
            .restore_project_asset_membership(&project.id, "asset_new")
            .unwrap();
        assert_eq!(catalog.count_project_assets(&project.id).unwrap(), 3);
        let restored = catalog.get_asset("asset_new").unwrap();
        assert_eq!(restored.notes, "Keep this annotation");
        assert_eq!(restored.labels, ["hero shot"]);
        assert_eq!(restored.proxy_path.as_deref(), proxy.to_str());
        assert_eq!(restored.thumbnail_path.as_deref(), thumbnail.to_str());
        assert_eq!(restored.thumbnail_quality, 10);
        let restored_analysis = catalog.asset_analysis("asset_new").unwrap().unwrap();
        assert_eq!(restored_analysis.analysis.best_moment_start_ms, Some(1_000));
        assert_eq!(restored_analysis.analysis.best_moment_end_ms, Some(2_000));
        assert_eq!(
            restored_analysis.analysis.thumbnail_timestamp_ms,
            Some(1_500)
        );
    }
}
