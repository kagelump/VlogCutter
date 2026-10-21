//! The diagnostics recorder and reader (D01-T027).
//!
//! One durable place to record anything the user needs to know about, and one
//! reader to get it back out, so a failure is never discoverable only by
//! reading a terminal log or a SQLite table by hand.
//!
//! Two rules shape everything here:
//!
//! - **Deduplicate, don't flood.** Ingesting a folder with 400 unreadable
//!   sidecar files must produce one diagnostic saying so, not 400 rows. A
//!   surface that floods is one users learn to ignore, which is the same as not
//!   having one.
//! - **Acknowledge, don't delete.** The application never deletes a diagnostic.
//!   Acknowledging hides it; the condition happening again resurfaces it, with
//!   its occurrence history intact.

pub mod model;

pub use model::{
    Diagnostic, DiagnosticCounts, DiagnosticFilter, DiagnosticRecord, DiagnosticSeverity,
    DiagnosticSubject,
};

use crate::Catalog;
use crate::now;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

fn row_to_diagnostic(row: &rusqlite::Row<'_>) -> rusqlite::Result<Diagnostic> {
    let severity: String = row.get(2)?;
    let subject_type: String = row.get(4)?;
    let subject_id: String = row.get(5)?;
    let context: String = row.get(7)?;
    Ok(Diagnostic {
        id: row.get(0)?,
        dedupe_key: row.get(1)?,
        // A severity this build does not recognize still has to be *visible*;
        // showing it as an error is the safe direction to be wrong in.
        severity: DiagnosticSeverity::parse(&severity).unwrap_or(DiagnosticSeverity::Error),
        kind: row.get(3)?,
        subject: DiagnosticSubject::from_columns(&subject_type, &subject_id),
        message: row.get(6)?,
        context: serde_json::from_str(&context).unwrap_or(serde_json::Value::Null),
        occurrence_count: row.get::<_, i64>(8)? as u64,
        first_seen_at: row.get(9)?,
        last_seen_at: row.get(10)?,
        acknowledged_at: row.get(11)?,
    })
}

const SELECT_COLUMNS: &str = "id, dedupe_key, severity, kind, subject_type, subject_id, message, \
     context_json, occurrence_count, first_seen_at, last_seen_at, acknowledged_at";

/// True when the catalog has been migrated far enough to carry diagnostics. A
/// read against an older catalog reports nothing rather than failing.
fn diagnostics_table_exists(connection: &Connection) -> Result<bool> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='diagnostics'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}

impl Catalog {
    /// Records one condition.
    ///
    /// Recording a `dedupe_key` that already exists for this project increments
    /// `occurrence_count`, refreshes `last_seen_at`, and clears any
    /// acknowledgement — the condition happened again, so the user should see
    /// it again. `first_seen_at` is never moved.
    pub fn record_diagnostic(
        &self,
        project_id: &str,
        record: DiagnosticRecord,
    ) -> Result<Diagnostic> {
        self.require_project(project_id)?;
        let connection = self.connection()?;
        let (subject_type, subject_id) = record.subject.columns();
        let timestamp = now();
        connection.execute(
            "INSERT INTO diagnostics(id, project_id, dedupe_key, severity, kind, subject_type, subject_id, message, context_json, occurrence_count, first_seen_at, last_seen_at, acknowledged_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?10, NULL)
             ON CONFLICT(project_id, dedupe_key) DO UPDATE SET
                 occurrence_count = occurrence_count + 1,
                 last_seen_at = excluded.last_seen_at,
                 severity = excluded.severity,
                 message = excluded.message,
                 context_json = excluded.context_json,
                 subject_type = excluded.subject_type,
                 subject_id = excluded.subject_id,
                 acknowledged_at = NULL",
            params![
                format!("diagnostic_{}", Uuid::new_v4().simple()),
                project_id,
                record.dedupe_key,
                record.severity.as_str(),
                record.kind,
                subject_type,
                subject_id,
                record.message,
                serde_json::to_string(&record.context)?,
                timestamp,
            ],
        )?;
        Ok(connection.query_row(
            &format!(
                "SELECT {SELECT_COLUMNS} FROM diagnostics WHERE project_id=?1 AND dedupe_key=?2"
            ),
            params![project_id, record.dedupe_key],
            row_to_diagnostic,
        )?)
    }

    /// Reads diagnostics back, newest occurrence first.
    ///
    /// Ordered by `last_seen_at` descending then `id` ascending, so the order
    /// is total and deterministic even when two conditions were recorded in the
    /// same millisecond.
    pub fn diagnostics(
        &self,
        project_id: &str,
        filter: &DiagnosticFilter,
    ) -> Result<Vec<Diagnostic>> {
        self.require_project(project_id)?;
        let connection = self.connection()?;
        if !diagnostics_table_exists(&connection)? {
            return Ok(Vec::new());
        }
        let mut statement = connection.prepare(&format!(
            "SELECT {SELECT_COLUMNS} FROM diagnostics WHERE project_id=?1
             ORDER BY last_seen_at DESC, id ASC"
        ))?;
        let rows = statement
            .query_map([project_id], row_to_diagnostic)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .filter(|found| filter.include_acknowledged || found.acknowledged_at.is_none())
            .filter(|found| {
                filter
                    .severity_at_least
                    .is_none_or(|floor| found.severity >= floor)
            })
            .filter(|found| {
                filter
                    .kind_prefix
                    .as_deref()
                    .is_none_or(|prefix| found.kind.starts_with(prefix))
            })
            .collect())
    }

    /// Marks a diagnostic as seen by the user. The row is never deleted: the
    /// condition's history survives, and a re-occurrence resurfaces it.
    pub fn acknowledge_diagnostic(&self, project_id: &str, diagnostic_id: &str) -> Result<()> {
        self.require_project(project_id)?;
        self.connection()?.execute(
            "UPDATE diagnostics SET acknowledged_at=?3 WHERE project_id=?1 AND id=?2",
            params![project_id, diagnostic_id, now()],
        )?;
        Ok(())
    }

    /// Unacknowledged counts by severity, for a badge or a status line.
    pub fn diagnostic_counts(&self, project_id: &str) -> Result<DiagnosticCounts> {
        let unacknowledged = self.diagnostics(project_id, &DiagnosticFilter::default())?;
        let mut counts = DiagnosticCounts::default();
        for found in unacknowledged {
            match found.severity {
                DiagnosticSeverity::Error => counts.errors += 1,
                DiagnosticSeverity::Warning => counts.warnings += 1,
                DiagnosticSeverity::Info => counts.infos += 1,
            }
        }
        Ok(counts)
    }

    /// Producer: a file the ingest could not read.
    ///
    /// Deduped by containing folder, so a card full of `._*` sidecar files
    /// yields one diagnostic with a count, not one row per file.
    pub fn record_unreadable_media(
        &self,
        project_id: &str,
        containing_folder: &str,
        filename: &str,
        reason: &str,
    ) -> Result<Diagnostic> {
        self.record_diagnostic(
            project_id,
            DiagnosticRecord {
                dedupe_key: format!("ingest.unreadable_media:{containing_folder}"),
                severity: DiagnosticSeverity::Warning,
                kind: "ingest.unreadable_media".to_string(),
                subject: DiagnosticSubject::Project,
                message: format!(
                    "Some files in {containing_folder} could not be read as usable video and were skipped, most recently {filename}: {reason}. Check that file contains a video stream and valid video metadata."
                ),
                context: serde_json::json!({
                    "folder": containing_folder,
                    "mostRecentFilename": filename,
                    "mostRecentReason": reason,
                }),
            },
        )
    }

    /// Records a rejected media candidate independently so every source keeps
    /// its own filename and probe diagnosis. AppleDouble sidecars continue to
    /// use the folder-aggregated recorder above.
    pub fn record_unreadable_media_file(
        &self,
        project_id: &str,
        containing_folder: &str,
        filename: &str,
        reason: &str,
    ) -> Result<Diagnostic> {
        self.record_diagnostic(project_id, DiagnosticRecord {
            dedupe_key: format!("ingest.unreadable_media:{containing_folder}:{filename}"),
            severity: DiagnosticSeverity::Warning,
            kind: "ingest.unreadable_media".to_string(),
            subject: DiagnosticSubject::Project,
            message: format!("{filename} could not be read as usable video and was skipped: {reason}. Check that the file contains video frames and valid video metadata."),
            context: serde_json::json!({
                "folder": containing_folder,
                "filename": filename,
                "reason": reason,
                "mostRecentFilename": filename,
                "mostRecentReason": reason,
                "bytesReachable": true,
                "usableVideo": false,
            }),
        })
    }

    /// Records a reachable media candidate that ffprobe cannot identify as video.
    /// These are per-file so each rejected asset remains diagnosable after ingest.
    pub fn record_no_video_stream(
        &self,
        project_id: &str,
        containing_folder: &str,
        filename: &str,
        reason: &str,
    ) -> Result<Diagnostic> {
        self.record_diagnostic(project_id, DiagnosticRecord {
            dedupe_key: format!("ingest.no_video_stream:{containing_folder}:{filename}"),
            severity: DiagnosticSeverity::Warning,
            kind: "ingest.no_video_stream".to_string(),
            subject: DiagnosticSubject::Project,
            message: format!("{filename} is reachable but has no usable video stream and was skipped. Check that the intended source contains video frames and valid video metadata."),
            context: serde_json::json!({ "folder": containing_folder, "filename": filename, "reason": reason, "bytesReachable": true, "usableVideo": false }),
        })
    }

    /// Records per-file video streams that exist but lack usable technical metadata.
    pub fn record_invalid_video_stream(
        &self,
        project_id: &str,
        containing_folder: &str,
        filename: &str,
        reason: &str,
    ) -> Result<Diagnostic> {
        self.record_diagnostic(project_id, DiagnosticRecord {
            dedupe_key: format!("ingest.invalid_video_stream:{containing_folder}:{filename}"),
            severity: DiagnosticSeverity::Warning,
            kind: "ingest.invalid_video_stream".to_string(),
            subject: DiagnosticSubject::Project,
            message: format!("{filename} contains a video stream with unusable technical metadata and was skipped. Repair or replace the source only after confirming what it contains."),
            context: serde_json::json!({ "folder": containing_folder, "filename": filename, "reason": reason, "bytesReachable": true, "usableVideo": false }),
        })
    }

    /// Producer: an operation this build does not recognize and has retained.
    ///
    /// Supplements `quarantined_operations()` rather than replacing it: that
    /// surface is the authoritative list, this one is how the user finds out
    /// the list is non-empty.
    pub fn record_quarantined_operation(
        &self,
        project_id: &str,
        operation_id: &str,
        operation_type: &str,
    ) -> Result<Diagnostic> {
        self.record_diagnostic(
            project_id,
            DiagnosticRecord {
                dedupe_key: format!("sync.operation_quarantined:{operation_type}"),
                severity: DiagnosticSeverity::Warning,
                kind: "sync.operation_quarantined".to_string(),
                subject: DiagnosticSubject::Operation {
                    operation_id: operation_id.to_string(),
                },
                message: format!(
                    "This version does not understand the operation type '{operation_type}'. It has been kept and will apply once a newer version opens this project."
                ),
                context: serde_json::json!({ "operationType": operation_type }),
            },
        )
    }

    /// Producer: a clip whose camera nothing could identify.
    ///
    /// Deduped by containing folder, because an unidentified camera is a
    /// property of a card, not of each clip on it.
    pub fn record_unidentified_camera(
        &self,
        project_id: &str,
        containing_folder: &str,
        filename: &str,
    ) -> Result<Diagnostic> {
        self.record_diagnostic(
            project_id,
            DiagnosticRecord {
                dedupe_key: format!("camera.unidentified:{containing_folder}"),
                severity: DiagnosticSeverity::Warning,
                kind: "camera.unidentified".to_string(),
                subject: DiagnosticSubject::Project,
                message: format!(
                    "Clips in {containing_folder} carry no camera in their metadata and match no known filename pattern, so they are grouped as an unknown camera. Naming the capture sequence will separate them."
                ),
                context: serde_json::json!({
                    "folder": containing_folder,
                    "exampleFilename": filename,
                }),
            },
        )
    }

    /// Producer: a camera identified by a pattern that is good enough to group
    /// a card but not to claim a manufacturer.
    pub fn record_weak_pattern_match(
        &self,
        project_id: &str,
        pattern_id: &str,
        label: &str,
        capture_sequence_id: &str,
    ) -> Result<Diagnostic> {
        self.record_diagnostic(
            project_id,
            DiagnosticRecord {
                dedupe_key: format!("camera.weak_pattern_match:{pattern_id}"),
                severity: DiagnosticSeverity::Info,
                kind: "camera.weak_pattern_match".to_string(),
                subject: DiagnosticSubject::CaptureSequence {
                    capture_sequence_id: capture_sequence_id.to_string(),
                },
                message: format!(
                    "These clips were guessed as '{label}' from their filenames alone. That is enough to keep the card together but not to name the camera — rename the capture sequence if it is wrong."
                ),
                context: serde_json::json!({ "patternId": pattern_id, "label": label }),
            },
        )
    }

    /// Producer: source media that is not currently reachable.
    pub fn record_offline_volume(
        &self,
        project_id: &str,
        volume_hint: &str,
        affected_assets: u64,
    ) -> Result<Diagnostic> {
        self.record_diagnostic(
            project_id,
            DiagnosticRecord {
                dedupe_key: format!("media.volume_offline:{volume_hint}"),
                severity: DiagnosticSeverity::Error,
                kind: "media.volume_offline".to_string(),
                subject: DiagnosticSubject::Volume {
                    volume_hint: volume_hint.to_string(),
                },
                message: format!(
                    "{volume_hint} is not mounted, so {affected_assets} clips cannot be played or exported."
                ),
                context: serde_json::json!({
                    "volumeHint": volume_hint,
                    "affectedAssets": affected_assets,
                }),
            },
        )
    }

    /// Scans this project's assets for source media that is not reachable and
    /// records one diagnostic per missing volume root.
    ///
    /// Returns the diagnostics it recorded. A project whose media is all
    /// present records nothing and returns an empty vector.
    pub fn detect_offline_volumes(&self, project_id: &str) -> Result<Vec<Diagnostic>> {
        let assets = self.list_assets(project_id)?;
        // Group by volume root deterministically: sorted pairs, no HashMap, so
        // the diagnostics land in a stable order across runs.
        let mut missing: Vec<(String, u64)> = Vec::new();
        for asset in &assets {
            if asset.path.is_empty() || std::path::Path::new(&asset.path).exists() {
                continue;
            }
            let root = volume_root(&asset.path);
            match missing.iter_mut().find(|(existing, _)| *existing == root) {
                Some((_, count)) => *count += 1,
                None => missing.push((root, 1)),
            }
        }
        missing.sort();
        missing
            .into_iter()
            .map(|(root, count)| self.record_offline_volume(project_id, &root, count))
            .collect()
    }
}

/// The mount root a path lives under, which is what the user recognizes as
/// "the card" or "the drive". Falls back to the parent directory off macOS's
/// `/Volumes` convention.
fn volume_root(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() > 2 && parts[1] == "Volumes" {
        return format!("/Volumes/{}", parts[2]);
    }
    std::path::Path::new(path)
        .parent()
        .map(|parent| parent.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}
