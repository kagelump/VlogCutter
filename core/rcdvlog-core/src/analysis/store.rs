//! Transcript persistence.
//!
//! A transcript is a derived artifact identified by its recipe key plus the
//! fingerprint of the proxy it came from. Either changing makes the stored
//! result stale, which is what lets a "transcribe missing" run be idempotent
//! while still regenerating anything a proxy rebuild invalidated.

use anyhow::{Context, Result, bail};
use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::analysis::STT_RECIPE_KEY;
use crate::analysis::quality::DecodedSegment;

/// The four terminal states of a transcription attempt.
///
/// `NoSpeech` is a success: the VAD gate found nothing and no model was
/// invoked. Conflating it with `Failed` would cause every silent clip to be
/// retried forever, and 64% of clips on the reference corpus are silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptStatus {
    Speech,
    NoSpeech,
    Rejected,
    Failed,
}

impl TranscriptStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TranscriptStatus::Speech => "speech",
            TranscriptStatus::NoSpeech => "no_speech",
            TranscriptStatus::Rejected => "rejected",
            TranscriptStatus::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "speech" => Ok(TranscriptStatus::Speech),
            "no_speech" => Ok(TranscriptStatus::NoSpeech),
            "rejected" => Ok(TranscriptStatus::Rejected),
            "failed" => Ok(TranscriptStatus::Failed),
            other => bail!("unknown transcript status: {other}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredTranscript {
    pub asset_id: String,
    pub recipe_key: String,
    pub source_fingerprint: String,
    pub status: TranscriptStatus,
    pub reject_reason: String,
    pub primary_language: String,
    pub speech_seconds: f64,
    pub window_count: u32,
    pub segments: Vec<DecodedSegment>,
}

/// Writes a transcript and its segments, replacing any previous result.
///
/// The header row, the removal of the previous segments and the new segments
/// are one transaction: a failure part-way through would otherwise leave a
/// header claiming `speech` above a truncated or empty segment list, which is
/// indistinguishable downstream from a genuinely short transcript.
pub fn save_transcript(connection: &Connection, transcript: &StoredTranscript) -> Result<()> {
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let transaction = connection.unchecked_transaction()?;
    let connection = &transaction;
    connection
        .execute(
            r#"
            INSERT INTO asset_transcripts
                (asset_id, recipe_key, source_kind, source_fingerprint, status, reject_reason,
                 primary_language, speech_seconds, window_count, created_at)
            VALUES (?1, ?2, 'proxy', ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(asset_id) DO UPDATE SET
                recipe_key = excluded.recipe_key,
                source_fingerprint = excluded.source_fingerprint,
                status = excluded.status,
                reject_reason = excluded.reject_reason,
                primary_language = excluded.primary_language,
                speech_seconds = excluded.speech_seconds,
                window_count = excluded.window_count,
                created_at = excluded.created_at
            "#,
            params![
                transcript.asset_id,
                transcript.recipe_key,
                transcript.source_fingerprint,
                transcript.status.as_str(),
                transcript.reject_reason,
                transcript.primary_language,
                transcript.speech_seconds,
                transcript.window_count,
                now,
            ],
        )
        .context("save transcript row")?;

    connection
        .execute(
            "DELETE FROM transcript_segments WHERE asset_id = ?1",
            params![transcript.asset_id],
        )
        .context("clear previous transcript segments")?;

    {
        let mut statement = connection.prepare(
            "INSERT INTO transcript_segments
                (asset_id, segment_index, start_ms, end_ms, language, text, mean_token_probability)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for (index, segment) in transcript.segments.iter().enumerate() {
            statement.execute(params![
                transcript.asset_id,
                index as i64,
                segment.start_ms,
                segment.end_ms,
                segment.language,
                segment.text,
                segment.mean_token_probability,
            ])?;
        }
    }
    transaction.commit()?;
    Ok(())
}

pub fn load_transcript(
    connection: &Connection,
    asset_id: &str,
) -> Result<Option<StoredTranscript>> {
    let row = connection
        .query_row(
            "SELECT recipe_key, source_fingerprint, status, reject_reason, primary_language,
                    speech_seconds, window_count
             FROM asset_transcripts WHERE asset_id = ?1",
            params![asset_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, f64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((recipe_key, source_fingerprint, status, reject_reason, language, seconds, windows)) =
        row
    else {
        return Ok(None);
    };

    let mut statement = connection.prepare(
        "SELECT start_ms, end_ms, language, text, mean_token_probability
         FROM transcript_segments WHERE asset_id = ?1 ORDER BY segment_index",
    )?;
    let segments = statement
        .query_map(params![asset_id], |row| {
            Ok(DecodedSegment {
                start_ms: row.get::<_, i64>(0)? as u32,
                end_ms: row.get::<_, i64>(1)? as u32,
                language: row.get(2)?,
                text: row.get(3)?,
                mean_token_probability: row.get::<_, f64>(4)? as f32,
            })
        })?
        .collect::<rusqlite::Result<Vec<DecodedSegment>>>()?;

    Ok(Some(StoredTranscript {
        asset_id: asset_id.to_string(),
        recipe_key,
        source_fingerprint,
        status: TranscriptStatus::parse(&status)?,
        reject_reason,
        primary_language: language,
        speech_seconds: seconds,
        window_count: windows as u32,
        segments,
    }))
}

/// Whether the stored transcript was produced by the current recipe from the
/// current proxy. Anything else must be regenerated.
pub fn is_current(
    connection: &Connection,
    asset_id: &str,
    source_fingerprint: &str,
) -> Result<bool> {
    let stored: Option<(String, String)> = connection
        .query_row(
            "SELECT recipe_key, source_fingerprint FROM asset_transcripts WHERE asset_id = ?1",
            params![asset_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(match stored {
        Some((recipe, fingerprint)) => {
            recipe == STT_RECIPE_KEY && fingerprint == source_fingerprint
        }
        None => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::migrate_project_catalog;

    fn catalog() -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        migrate_project_catalog(&mut connection).unwrap();
        connection
            .execute(
                "INSERT INTO projects (id, name, created_at)
                 VALUES ('p1', 'p', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO assets (id, project_id, fingerprint, path, filename, size_bytes,
                 duration_seconds, created_at)
                 VALUES ('a1', 'p1', 'f1', '/x.mp4', 'x.mp4', 1, 20.0, '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        connection
    }

    fn transcript(status: TranscriptStatus, segments: Vec<DecodedSegment>) -> StoredTranscript {
        StoredTranscript {
            asset_id: "a1".to_string(),
            recipe_key: STT_RECIPE_KEY.to_string(),
            source_fingerprint: "proxy-fp-1".to_string(),
            status,
            reject_reason: String::new(),
            primary_language: "ja".to_string(),
            speech_seconds: 6.0,
            window_count: 1,
            segments,
        }
    }

    fn segment(text: &str) -> DecodedSegment {
        DecodedSegment {
            start_ms: 1_000,
            end_ms: 4_000,
            language: "ja".to_string(),
            text: text.to_string(),
            mean_token_probability: 0.73,
        }
    }

    #[test]
    fn a_saved_transcript_round_trips() {
        let connection = catalog();
        let stored = transcript(TranscriptStatus::Speech, vec![segment("次は新大阪です")]);
        save_transcript(&connection, &stored).unwrap();
        let loaded = load_transcript(&connection, "a1").unwrap().unwrap();
        assert_eq!(loaded.status, TranscriptStatus::Speech);
        assert_eq!(loaded.segments.len(), 1);
        assert_eq!(loaded.segments[0].text, "次は新大阪です");
        assert_eq!(loaded.segments[0].start_ms, 1_000);
        assert!((loaded.segments[0].mean_token_probability - 0.73).abs() < 1e-6);
    }

    #[test]
    fn a_no_speech_result_is_stored_with_no_segments() {
        let connection = catalog();
        save_transcript(&connection, &transcript(TranscriptStatus::NoSpeech, vec![])).unwrap();
        let loaded = load_transcript(&connection, "a1").unwrap().unwrap();
        assert_eq!(loaded.status, TranscriptStatus::NoSpeech);
        assert!(loaded.segments.is_empty());
    }

    #[test]
    fn resaving_replaces_the_previous_segments_rather_than_appending() {
        let connection = catalog();
        save_transcript(
            &connection,
            &transcript(
                TranscriptStatus::Speech,
                vec![segment("first"), segment("second")],
            ),
        )
        .unwrap();
        save_transcript(
            &connection,
            &transcript(TranscriptStatus::Speech, vec![segment("only")]),
        )
        .unwrap();
        let loaded = load_transcript(&connection, "a1").unwrap().unwrap();
        assert_eq!(loaded.segments.len(), 1);
        assert_eq!(loaded.segments[0].text, "only");
    }

    #[test]
    fn an_unanalyzed_asset_loads_as_none() {
        let connection = catalog();
        assert!(load_transcript(&connection, "a1").unwrap().is_none());
    }

    #[test]
    fn a_transcript_from_a_different_proxy_is_stale() {
        let connection = catalog();
        save_transcript(&connection, &transcript(TranscriptStatus::Speech, vec![])).unwrap();
        assert!(is_current(&connection, "a1", "proxy-fp-1").unwrap());
        // The proxy was regenerated, so its fingerprint changed.
        assert!(!is_current(&connection, "a1", "proxy-fp-2").unwrap());
    }

    #[test]
    fn a_transcript_from_a_different_recipe_is_stale() {
        let connection = catalog();
        let mut stored = transcript(TranscriptStatus::Speech, vec![]);
        stored.recipe_key = "stt.some-older-recipe.v0".to_string();
        save_transcript(&connection, &stored).unwrap();
        assert!(!is_current(&connection, "a1", "proxy-fp-1").unwrap());
    }

    #[test]
    fn a_missing_transcript_is_not_current() {
        let connection = catalog();
        assert!(!is_current(&connection, "a1", "proxy-fp-1").unwrap());
    }

    #[test]
    fn a_rejected_transcript_keeps_its_reason() {
        let connection = catalog();
        let mut stored = transcript(TranscriptStatus::Rejected, vec![]);
        stored.reject_reason = "repetition".to_string();
        save_transcript(&connection, &stored).unwrap();
        let loaded = load_transcript(&connection, "a1").unwrap().unwrap();
        assert_eq!(loaded.status, TranscriptStatus::Rejected);
        assert_eq!(loaded.reject_reason, "repetition");
    }
}
