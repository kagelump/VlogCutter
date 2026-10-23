//! Transcript persistence.
//!
//! A transcript is a derived artifact identified by its recipe key plus the
//! fingerprint of the proxy it came from. Either changing makes the stored
//! result stale, which is what lets a "transcribe missing" run be idempotent
//! while still regenerating anything a proxy rebuild invalidated.

use anyhow::{Context, Result, bail};
use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::analysis::STT_RECIPE_KEY;
use crate::analysis::quality::DecodedSegment;

/// Terminal states of a transcription attempt.
///
/// `NoSpeech`, `NoAudio`, `NoAcceptedSpeech`, and `Rejected` are successful,
/// terminal outcomes. Only `Failed` is retryable by an ordinary batch.
/// `NoSpeech` means the VAD gate ran and found no speech; it does not mean the
/// provider happened to return nothing after speech was detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptStatus {
    Speech,
    NoSpeech,
    NoAudio,
    NoAcceptedSpeech,
    Rejected,
    Failed,
}

impl TranscriptStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TranscriptStatus::Speech => "speech",
            TranscriptStatus::NoSpeech => "no_speech",
            TranscriptStatus::NoAudio => "no_audio",
            TranscriptStatus::NoAcceptedSpeech => "no_accepted_speech",
            TranscriptStatus::Rejected => "rejected",
            TranscriptStatus::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "speech" => Ok(TranscriptStatus::Speech),
            "no_speech" => Ok(TranscriptStatus::NoSpeech),
            "no_audio" => Ok(TranscriptStatus::NoAudio),
            "no_accepted_speech" => Ok(TranscriptStatus::NoAcceptedSpeech),
            "rejected" => Ok(TranscriptStatus::Rejected),
            "failed" => Ok(TranscriptStatus::Failed),
            other => bail!("unknown transcript status: {other}"),
        }
    }
}

/// Why an attempt ended in its terminal status.
///
/// The status records the broad result while this reason preserves the
/// distinction between literal silence, absent audio, an empty provider
/// response, a quality rejection, and a processing failure. `Legacy...` records
/// are migration-only compatibility evidence: older schemas did not store
/// enough information to prove literal silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptOutcomeReason {
    Accepted,
    NoAudioStream,
    NoDetectedSpeech,
    EmptyProviderOutput,
    QualityRejected,
    ProcessingFailed,
    LegacyNoSpeechUnverified,
}

impl TranscriptOutcomeReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            TranscriptOutcomeReason::Accepted => "accepted",
            TranscriptOutcomeReason::NoAudioStream => "no_audio_stream",
            TranscriptOutcomeReason::NoDetectedSpeech => "no_detected_speech",
            TranscriptOutcomeReason::EmptyProviderOutput => "empty_provider_output",
            TranscriptOutcomeReason::QualityRejected => "quality_rejected",
            TranscriptOutcomeReason::ProcessingFailed => "processing_failed",
            TranscriptOutcomeReason::LegacyNoSpeechUnverified => "legacy_no_speech_unverified",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "accepted" => Ok(TranscriptOutcomeReason::Accepted),
            "no_audio_stream" => Ok(TranscriptOutcomeReason::NoAudioStream),
            "no_detected_speech" => Ok(TranscriptOutcomeReason::NoDetectedSpeech),
            "empty_provider_output" => Ok(TranscriptOutcomeReason::EmptyProviderOutput),
            "quality_rejected" => Ok(TranscriptOutcomeReason::QualityRejected),
            "processing_failed" => Ok(TranscriptOutcomeReason::ProcessingFailed),
            "legacy_no_speech_unverified" => Ok(TranscriptOutcomeReason::LegacyNoSpeechUnverified),
            other => bail!("unknown transcript outcome reason: {other}"),
        }
    }

    /// Terminal outcomes are reusable by background batches. A failed attempt
    /// is deliberately excluded even when its source fingerprint matches.
    pub fn is_terminal(&self) -> bool {
        !matches!(self, TranscriptOutcomeReason::ProcessingFailed)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredTranscript {
    pub asset_id: String,
    pub recipe_key: String,
    pub source_fingerprint: String,
    pub status: TranscriptStatus,
    pub outcome_reason: TranscriptOutcomeReason,
    pub reject_reason: String,
    pub primary_language: String,
    /// Detected speech duration from the VAD regions. This is retained even
    /// when the provider returns no accepted text.
    pub speech_seconds: f64,
    /// Provider decode windows attempted, including coverage re-splits.
    pub window_count: u32,
    /// Raw decoded segments returned by the provider before text filtering and
    /// the quality gate. Together with `window_count`, this is the provider
    /// evidence a caller needs to judge whether targeted recovery is useful.
    pub provider_segment_count: u32,
    /// Quality evidence computed on the candidate segments. `None` means there
    /// were no candidate segments to measure.
    pub mean_token_probability: Option<f32>,
    pub repetition_ratio: Option<f32>,
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
                (asset_id, recipe_key, source_kind, source_fingerprint, status, outcome_reason,
                 reject_reason, primary_language, speech_seconds, window_count,
                 provider_segment_count, mean_token_probability, repetition_ratio, created_at)
            VALUES (?1, ?2, 'proxy', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            ON CONFLICT(asset_id) DO UPDATE SET
                recipe_key = excluded.recipe_key,
                source_fingerprint = excluded.source_fingerprint,
                status = excluded.status,
                outcome_reason = excluded.outcome_reason,
                reject_reason = excluded.reject_reason,
                primary_language = excluded.primary_language,
                speech_seconds = excluded.speech_seconds,
                window_count = excluded.window_count,
                provider_segment_count = excluded.provider_segment_count,
                mean_token_probability = excluded.mean_token_probability,
                repetition_ratio = excluded.repetition_ratio,
                created_at = excluded.created_at
            "#,
            params![
                transcript.asset_id,
                transcript.recipe_key,
                transcript.source_fingerprint,
                transcript.status.as_str(),
                transcript.outcome_reason.as_str(),
                transcript.reject_reason,
                transcript.primary_language,
                transcript.speech_seconds,
                transcript.window_count,
                transcript.provider_segment_count,
                transcript.mean_token_probability,
                transcript.repetition_ratio,
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
            "SELECT recipe_key, source_fingerprint, status, outcome_reason, reject_reason,
                    primary_language, speech_seconds, window_count, provider_segment_count,
                    mean_token_probability, repetition_ratio
             FROM asset_transcripts WHERE asset_id = ?1",
            params![asset_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, f64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<f64>>(9)?,
                    row.get::<_, Option<f64>>(10)?,
                ))
            },
        )
        .optional()?;
    let Some((
        recipe_key,
        source_fingerprint,
        status,
        outcome_reason,
        reject_reason,
        language,
        seconds,
        windows,
        provider_segments,
        mean_probability,
        repetition,
    )) = row
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
        outcome_reason: TranscriptOutcomeReason::parse(&outcome_reason)?,
        reject_reason,
        primary_language: language,
        speech_seconds: seconds,
        window_count: windows as u32,
        provider_segment_count: provider_segments as u32,
        mean_token_probability: mean_probability.map(|value| value as f32),
        repetition_ratio: repetition.map(|value| value as f32),
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
    let stored: Option<(String, String, String)> = connection
        .query_row(
            "SELECT recipe_key, source_fingerprint, status FROM asset_transcripts WHERE asset_id = ?1",
            params![asset_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    Ok(match stored {
        Some((recipe, fingerprint, status)) => {
            recipe == STT_RECIPE_KEY
                && fingerprint == source_fingerprint
                && status != TranscriptStatus::Failed.as_str()
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
            outcome_reason: match status {
                TranscriptStatus::Speech => TranscriptOutcomeReason::Accepted,
                TranscriptStatus::NoSpeech => TranscriptOutcomeReason::NoDetectedSpeech,
                TranscriptStatus::NoAudio => TranscriptOutcomeReason::NoAudioStream,
                TranscriptStatus::NoAcceptedSpeech => TranscriptOutcomeReason::EmptyProviderOutput,
                TranscriptStatus::Rejected => TranscriptOutcomeReason::QualityRejected,
                TranscriptStatus::Failed => TranscriptOutcomeReason::ProcessingFailed,
            },
            reject_reason: String::new(),
            primary_language: "ja".to_string(),
            speech_seconds: 6.0,
            window_count: 1,
            provider_segment_count: 0,
            mean_token_probability: None,
            repetition_ratio: None,
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
    fn a_no_audio_result_round_trips_with_its_source_and_reason() {
        let connection = catalog();
        let mut stored = transcript(TranscriptStatus::NoAudio, vec![]);
        stored.speech_seconds = 0.0;
        stored.window_count = 0;
        save_transcript(&connection, &stored).unwrap();
        let loaded = load_transcript(&connection, "a1").unwrap().unwrap();
        assert_eq!(loaded.status, TranscriptStatus::NoAudio);
        assert_eq!(
            loaded.outcome_reason,
            TranscriptOutcomeReason::NoAudioStream
        );
        assert_eq!(loaded.source_fingerprint, "proxy-fp-1");
    }

    #[test]
    fn detected_speech_with_empty_provider_output_keeps_duration_and_provider_evidence() {
        let connection = catalog();
        let mut stored = transcript(TranscriptStatus::NoAcceptedSpeech, vec![]);
        stored.outcome_reason = TranscriptOutcomeReason::EmptyProviderOutput;
        stored.speech_seconds = 4.25;
        stored.window_count = 1;
        stored.provider_segment_count = 3;
        save_transcript(&connection, &stored).unwrap();
        let loaded = load_transcript(&connection, "a1").unwrap().unwrap();
        assert_eq!(loaded.status, TranscriptStatus::NoAcceptedSpeech);
        assert_eq!(
            loaded.outcome_reason,
            TranscriptOutcomeReason::EmptyProviderOutput
        );
        assert!((loaded.speech_seconds - 4.25).abs() < 1e-6);
        assert_eq!(loaded.window_count, 1);
        assert_eq!(loaded.provider_segment_count, 3);
    }

    #[test]
    fn a_failed_attempt_is_not_reusable_even_with_a_matching_fingerprint() {
        let connection = catalog();
        let mut stored = transcript(TranscriptStatus::Failed, vec![]);
        stored.reject_reason = "probe failed".to_string();
        save_transcript(&connection, &stored).unwrap();
        assert!(!is_current(&connection, "a1", "proxy-fp-1").unwrap());
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
