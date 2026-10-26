//! Transcript persistence.
//!
//! A transcript is a derived artifact identified by its recipe key plus the
//! fingerprint of the proxy it came from. Either changing makes the stored
//! result stale, which is what lets a "transcribe missing" run be idempotent
//! while still regenerating anything a proxy rebuild invalidated.

use anyhow::{Context, Result, bail, ensure};
use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::analysis::STT_RECIPE_KEY;
use crate::analysis::quality::DecodedSegment;

pub(crate) mod native_provenance;
pub(crate) mod native_recipe;
#[allow(unused_imports)]
pub(crate) use native_provenance::save_native_transcript;
pub use native_provenance::{
    NATIVE_TRANSCRIPT_PROVENANCE_SCHEMA_VERSION, NATIVE_TRANSCRIPT_WINDOW_PLAN_SCHEMA_VERSION,
    NativeSttWindowObservation, NativeSttWindowQualityObservation, NativeTranscriptCoverage,
    NativeTranscriptProvenance, NativeTranscriptWindowPlan, NativeTranscriptWindowProvenance,
};
pub use native_recipe::native_stt_transcript_recipe;

/// Terminal states of a transcription attempt.
///
/// `NoSpeech`, `NoAudio`, `NoAcceptedSpeech`, `Rejected`, and
/// `ReviewRequired` are successful, terminal outcomes. Only `Failed` is
/// retryable by an ordinary batch. `NoSpeech` means the VAD gate ran and found
/// no speech; it does not mean the provider happened to return nothing after
/// speech was detected.
///
/// `ReviewRequired` retains timed provider candidate text whose quality
/// evidence is incomplete. The candidates are explicitly unaccepted: they are
/// available to a human reviewer but are not accepted speech and must not be
/// forwarded as AI context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptStatus {
    Speech,
    NoSpeech,
    NoAudio,
    NoAcceptedSpeech,
    Rejected,
    Failed,
    ReviewRequired,
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
            TranscriptStatus::ReviewRequired => "review_required",
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
            "review_required" => Ok(TranscriptStatus::ReviewRequired),
            other => bail!("unknown transcript status: {other}"),
        }
    }
}

/// Why an attempt ended in its terminal status.
///
/// The status records the broad result while this reason preserves the
/// distinction between literal silence, absent audio, an empty provider
/// response, a quality rejection, a processing failure, and retained candidate
/// text with incomplete quality evidence. `Legacy...` records are migration-only
/// compatibility evidence: older schemas did not store enough information to
/// prove literal silence.
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
    QualityEvidenceIncomplete,
    /// The native checkpoint owner retained a contiguous, quality-evaluated
    /// prefix of a paid multi-window speech plan, but the full asset has not
    /// been finalized. This is nonterminal: candidates are not accepted and no
    /// generic batch may reuse them as a complete transcript.
    NativeCoverageIncomplete,
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
            TranscriptOutcomeReason::QualityEvidenceIncomplete => "quality_evidence_incomplete",
            TranscriptOutcomeReason::NativeCoverageIncomplete => "native_coverage_incomplete",
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
            "quality_evidence_incomplete" => Ok(TranscriptOutcomeReason::QualityEvidenceIncomplete),
            "native_coverage_incomplete" => Ok(TranscriptOutcomeReason::NativeCoverageIncomplete),
            other => bail!("unknown transcript outcome reason: {other}"),
        }
    }

    /// Terminal outcomes are reusable by background batches. A failed attempt
    /// is deliberately excluded even when its source fingerprint matches.
    pub fn is_terminal(&self) -> bool {
        !matches!(
            self,
            TranscriptOutcomeReason::ProcessingFailed
                | TranscriptOutcomeReason::NativeCoverageIncomplete
        )
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
    /// Quality evidence computed on the candidate segments. `None` means the
    /// measurement is absent: either there were no candidate segments, or one
    /// or more candidates lacked comparable token-probability evidence. It is
    /// never a fabricated zero/one confidence.
    pub mean_token_probability: Option<f32>,
    pub repetition_ratio: Option<f32>,
    pub segments: Vec<DecodedSegment>,
}

/// Reject malformed confidence evidence before it can be stored as if measured.
fn validate_probability_evidence(value: Option<f32>, label: &str) -> Result<()> {
    if let Some(probability) = value
        && (!probability.is_finite() || !(0.0..=1.0).contains(&probability))
    {
        bail!(
            "{label} token probability must be finite and in [0, 1] when present; got {probability}"
        );
    }
    Ok(())
}

/// Validate the one deliberately new status/reason pairing.
///
/// `ReviewRequired` is not accepted speech: it is timed candidate text retained
/// for a human because a comparable quality metric was absent or incomplete.
/// The contract is intentionally limited to this new state so legacy and local
/// transcript fixtures keep their existing shape.
fn validate_review_required_contract(transcript: &StoredTranscript) -> Result<()> {
    match (transcript.status, transcript.outcome_reason) {
        (TranscriptStatus::ReviewRequired, TranscriptOutcomeReason::QualityEvidenceIncomplete) => {
            if transcript.segments.is_empty() {
                bail!("review_required transcript must retain at least one candidate segment");
            }
            for (index, segment) in transcript.segments.iter().enumerate() {
                if segment.text.trim().is_empty() {
                    bail!("review_required segment {index} must contain candidate text");
                }
                if segment.start_ms >= segment.end_ms {
                    bail!("review_required segment {index} must have a positive source-time span");
                }
            }
            Ok(())
        }
        (TranscriptStatus::ReviewRequired, reason) => bail!(
            "review_required transcript must use quality_evidence_incomplete, not {}",
            reason.as_str()
        ),
        (status, TranscriptOutcomeReason::QualityEvidenceIncomplete) => bail!(
            "quality_evidence_incomplete is only valid for review_required, not {}",
            status.as_str()
        ),
        (_, TranscriptOutcomeReason::NativeCoverageIncomplete) => bail!(
            "native_coverage_incomplete is owned exclusively by the native transcript checkpoint writer"
        ),
        _ => Ok(()),
    }
}

#[derive(Clone, Copy)]
pub(crate) struct NativeTranscriptWindowPlanRow<'a> {
    pub plan_json: &'a str,
    pub fingerprint: &'a str,
}

/// Replace the canonical header, segments and any native associations in one
/// caller-owned transaction. Local callers pass `None` and therefore atomically
/// clear stale native provenance when replacing a cloud artifact with local
/// output. Native callers pass the frozen policy/quality identities and then
/// insert their exact request links before committing the same transaction.
pub(crate) fn write_transcript_rows(
    connection: &Connection,
    transcript: &StoredTranscript,
    native_policy_id: Option<&str>,
    native_quality_recipe_version: Option<&str>,
    native_window_plan: Option<NativeTranscriptWindowPlanRow<'_>>,
) -> Result<()> {
    ensure!(
        native_policy_id.is_some() == native_quality_recipe_version.is_some(),
        "native policy and quality recipe identities must be written together"
    );
    ensure!(
        native_window_plan.is_none()
            || (native_policy_id.is_some()
                && transcript.status == TranscriptStatus::Failed
                && transcript.outcome_reason == TranscriptOutcomeReason::NativeCoverageIncomplete),
        "a native transcript window plan is only valid on an incomplete native checkpoint"
    );
    let (plan_json, plan_fingerprint) = native_window_plan
        .map(|plan| (Some(plan.plan_json), Some(plan.fingerprint)))
        .unwrap_or((None, None));
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    connection
        .execute(
            "DELETE FROM native_transcript_windows WHERE asset_id = ?1",
            params![transcript.asset_id],
        )
        .context("clear previous native transcript window provenance")?;
    connection
        .execute(
            r#"
            INSERT INTO asset_transcripts
                (asset_id, recipe_key, source_kind, source_fingerprint, status, outcome_reason,
                 reject_reason, primary_language, speech_seconds, window_count,
                 provider_segment_count, mean_token_probability, repetition_ratio, created_at,
                 native_policy_id, native_quality_recipe_version,
                 native_window_plan_json, native_window_plan_fingerprint)
            VALUES (?1, ?2, 'proxy', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
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
                created_at = excluded.created_at,
                native_policy_id = excluded.native_policy_id,
                native_quality_recipe_version = excluded.native_quality_recipe_version,
                native_window_plan_json = excluded.native_window_plan_json,
                native_window_plan_fingerprint = excluded.native_window_plan_fingerprint
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
                native_policy_id,
                native_quality_recipe_version,
                plan_json,
                plan_fingerprint,
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
    Ok(())
}

/// Writes a transcript and its segments, replacing any previous result.
///
/// The header row, the removal of the previous segments/native associations and
/// the new segments are one transaction: a failure part-way through would
/// otherwise leave a header claiming `speech` above a truncated or empty segment
/// list, which is indistinguishable downstream from a genuinely short
/// transcript. Validation happens before the transaction, so an invalid review
/// candidate cannot replace a previously valid transcript.
pub fn save_transcript(connection: &Connection, transcript: &StoredTranscript) -> Result<()> {
    validate_review_required_contract(transcript)?;
    validate_probability_evidence(transcript.mean_token_probability, "transcript header mean")?;
    for (index, segment) in transcript.segments.iter().enumerate() {
        validate_probability_evidence(
            segment.mean_token_probability,
            &format!("segment {index} mean"),
        )?;
    }

    let transaction = connection.unchecked_transaction()?;
    write_transcript_rows(&transaction, transcript, None, None, None)?;
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
                mean_token_probability: row.get::<_, Option<f64>>(4)?.map(|value| value as f32),
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
                TranscriptStatus::ReviewRequired => {
                    TranscriptOutcomeReason::QualityEvidenceIncomplete
                }
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
            mean_token_probability: Some(0.73),
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
        assert_eq!(loaded.segments[0].mean_token_probability, Some(0.73));
    }

    #[test]
    fn unknown_segment_probability_round_trips_as_sql_null() {
        let connection = catalog();
        let mut stored = transcript(TranscriptStatus::Speech, vec![segment("unknown")]);
        stored.segments[0].mean_token_probability = None;
        save_transcript(&connection, &stored).unwrap();

        let loaded = load_transcript(&connection, "a1").unwrap().unwrap();
        assert_eq!(loaded.segments[0].mean_token_probability, None);
        let sql_value: Option<f64> = connection
            .query_row(
                "SELECT mean_token_probability FROM transcript_segments
                 WHERE asset_id = 'a1' AND segment_index = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sql_value, None, "absence must be stored as SQL NULL");
    }

    #[test]
    fn invalid_probability_does_not_replace_a_previous_valid_transcript() {
        let connection = catalog();
        let mut previous = transcript(TranscriptStatus::Speech, vec![segment("valid")]);
        previous.mean_token_probability = Some(0.73);
        save_transcript(&connection, &previous).unwrap();
        let before = load_transcript(&connection, "a1").unwrap().unwrap();

        for invalid in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
            let mut bad = transcript(TranscriptStatus::Speech, vec![segment("bad")]);
            bad.mean_token_probability = Some(0.73);
            bad.segments[0].mean_token_probability = Some(invalid);
            assert!(
                save_transcript(&connection, &bad).is_err(),
                "segment probability {invalid} must be rejected"
            );
            assert_eq!(load_transcript(&connection, "a1").unwrap().unwrap(), before);
        }

        let mut bad_header = transcript(TranscriptStatus::Speech, vec![segment("bad")]);
        bad_header.mean_token_probability = Some(1.5);
        assert!(save_transcript(&connection, &bad_header).is_err());
        assert_eq!(load_transcript(&connection, "a1").unwrap().unwrap(), before);
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
        save_transcript(
            &connection,
            &transcript(TranscriptStatus::Speech, vec![segment("known")]),
        )
        .unwrap();
        assert!(is_current(&connection, "a1", "proxy-fp-1").unwrap());
        let loaded = load_transcript(&connection, "a1").unwrap().unwrap();
        assert_eq!(loaded.segments[0].mean_token_probability, Some(0.73));
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
    #[test]
    fn malformed_probability_rejections_remain_persistable_terminal_outcomes() {
        use crate::analysis::engine::SttEngine;
        use crate::analysis::windows::{PackedWindow, SpeechRegion};
        struct InvalidProbabilityEngine(f32);
        impl SttEngine for InvalidProbabilityEngine {
            fn detect_speech(&self, _pcm: &[f32]) -> Result<Vec<SpeechRegion>> {
                Ok(vec![SpeechRegion {
                    start_ms: 0,
                    end_ms: 1_000,
                }])
            }
            fn decode_window(
                &self,
                _pcm: &[f32],
                _window: &PackedWindow,
            ) -> Result<Vec<DecodedSegment>> {
                Ok(vec![DecodedSegment {
                    start_ms: 0,
                    end_ms: 1_000,
                    language: "en".into(),
                    text: "unusable candidate".into(),
                    mean_token_probability: Some(self.0),
                }])
            }
        }
        let connection = catalog();
        for probability in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
            let result = crate::analysis::stt::transcribe_pcm(
                &InvalidProbabilityEngine(probability),
                "a1",
                "proxy-fp-invalid",
                &[],
            )
            .unwrap();
            assert_eq!(result.status, TranscriptStatus::Rejected);
            assert_eq!(
                result.outcome_reason,
                TranscriptOutcomeReason::QualityRejected
            );
            assert_eq!(result.reject_reason, "invalid_probability_evidence");
            assert_eq!(result.mean_token_probability, None);
            assert_eq!(result.provider_segment_count, 1);
            assert_eq!(result.speech_seconds, 1.0);
            assert!(result.segments.is_empty());
            save_transcript(&connection, &result).unwrap();
            let loaded = load_transcript(&connection, "a1").unwrap().unwrap();
            assert_eq!(loaded, result);
        }
    }
    #[test]
    fn review_required_candidates_round_trip_as_terminal_unaccepted_evidence() {
        let connection = catalog();
        let mut stored = transcript(
            TranscriptStatus::ReviewRequired,
            vec![segment("retained candidate")],
        );
        stored.segments[0].mean_token_probability = None;
        stored.reject_reason = "review_missing_quality_evidence".to_string();
        save_transcript(&connection, &stored).unwrap();

        assert_eq!(
            serde_json::to_string(&TranscriptStatus::ReviewRequired).unwrap(),
            "\"review_required\""
        );
        assert_eq!(
            serde_json::to_string(&TranscriptOutcomeReason::QualityEvidenceIncomplete).unwrap(),
            "\"quality_evidence_incomplete\""
        );
        let loaded = load_transcript(&connection, "a1").unwrap().unwrap();
        assert_eq!(loaded, stored);
        assert_eq!(loaded.status, TranscriptStatus::ReviewRequired);
        assert_eq!(
            loaded.outcome_reason,
            TranscriptOutcomeReason::QualityEvidenceIncomplete
        );
        assert_eq!(loaded.segments[0].text, "retained candidate");
        assert_eq!(loaded.segments[0].mean_token_probability, None);
        let segment_probability: Option<f64> = connection
            .query_row(
                "SELECT mean_token_probability FROM transcript_segments
                 WHERE asset_id = 'a1' AND segment_index = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            segment_probability, None,
            "review candidate evidence must stay SQL NULL, not confidence zero"
        );

        assert!(
            is_current(&connection, "a1", "proxy-fp-1").unwrap(),
            "a terminal review result is reusable under the same recipe/source"
        );
        assert!(
            !is_current(&connection, "a1", "proxy-fp-2").unwrap(),
            "a changed source still makes retained candidates stale"
        );

        stored.recipe_key = "stt.changed-recipe.v2".to_string();
        save_transcript(&connection, &stored).unwrap();
        assert!(
            !is_current(&connection, "a1", "proxy-fp-1").unwrap(),
            "a changed recipe still makes retained candidates stale"
        );
    }

    #[test]
    fn invalid_review_required_replacement_preserves_previous_header_and_segments() {
        let connection = catalog();
        let mut previous = transcript(
            TranscriptStatus::ReviewRequired,
            vec![segment("previous candidate")],
        );
        previous.segments[0].mean_token_probability = None;
        save_transcript(&connection, &previous).unwrap();
        let before = load_transcript(&connection, "a1").unwrap().unwrap();

        let mut wrong_reason = transcript(
            TranscriptStatus::ReviewRequired,
            vec![segment("bad candidate")],
        );
        wrong_reason.outcome_reason = TranscriptOutcomeReason::Accepted;

        let no_segments = transcript(TranscriptStatus::ReviewRequired, vec![]);

        let empty_text = transcript(TranscriptStatus::ReviewRequired, vec![segment("   ")]);

        let mut invalid_span =
            transcript(TranscriptStatus::ReviewRequired, vec![segment("bad span")]);
        invalid_span.segments[0].end_ms = invalid_span.segments[0].start_ms;

        let mut reason_on_other_status =
            transcript(TranscriptStatus::Speech, vec![segment("wrong owner")]);
        reason_on_other_status.outcome_reason = TranscriptOutcomeReason::QualityEvidenceIncomplete;

        for bad in [
            wrong_reason,
            no_segments,
            empty_text,
            invalid_span,
            reason_on_other_status,
        ] {
            assert!(
                save_transcript(&connection, &bad).is_err(),
                "invalid review replacement must be rejected"
            );
            assert_eq!(
                load_transcript(&connection, "a1").unwrap().unwrap(),
                before,
                "a rejected replacement must not alter the prior artifact"
            );
        }
    }
}
