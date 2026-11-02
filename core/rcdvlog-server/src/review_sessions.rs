//! Private, server-managed TV voice-review recording evidence (#222).
//!
//! A session binds one microphone capture to one immutable imported review
//! target (#221). The browser uploads untouched PCM in bounded, sequenced,
//! SHA-256-addressed chunks on the microphone's own sample clock; the server
//! stamps every chunk and event with its own receive time, refuses overlaps and
//! conflicting retries, and computes coverage and gaps at finalization instead
//! of trusting the client. Missing audio is reported as missing; it is never
//! synthesized or silently truncated.
//!
//! This evidence is distinct from source-asset transcripts and from canonical
//! edit-plan revisions. Nothing here writes feedback, a plan, or a transcript.
use crate::{ReviewTargetActor, open_read_only_connection, open_writable_connection};
use anyhow::Context;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;
use uuid::Uuid;

pub const REVIEW_SESSION_MAX_CHUNK_BYTES: usize = 2 * 1024 * 1024;
pub const REVIEW_SESSION_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Recording must be finalized within this window; later sessions are
/// reported as abandoned but remain finalizable and fully accounted for.
pub const REVIEW_SESSION_RECORDING_LIFETIME_HOURS: i64 = 12;
pub const REVIEW_SESSION_MAX_ACTIVE: u32 = 4;
pub const REVIEW_SESSION_MAX_EVENTS: u32 = 10_000;
pub const REVIEW_SESSION_LIST_MAX_LIMIT: u32 = 200;
const SUPPORTED_SAMPLE_RATES: &[u32] = &[16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 96_000];
const EVIDENCE_DIRECTORY: &str = "review-evidence";

#[derive(Debug, Error)]
pub enum ReviewSessionError {
    #[error("project is not registered on this server")]
    ProjectNotFound,
    #[error("review session or target not found")]
    NotFound,
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("review target is not ready for a new session")]
    TargetNotReady(Vec<&'static str>),
    #[error("too many active recording sessions")]
    Busy,
    #[error("session is no longer recording")]
    NotRecording,
    #[error("chunk conflicts with stored evidence")]
    ChunkConflict,
    #[error("chunk content does not match its declared hash")]
    ChunkCorrupt,
    #[error("session size or duration limit exceeded")]
    LimitExceeded,
    #[error("insufficient free disk space for review evidence")]
    LowDiskSpace,
    #[error("session audio is not available for download")]
    AudioUnavailable,
    #[error("idempotency key reused for a different session request")]
    IdempotencyConflict,
    #[error(transparent)]
    Infrastructure(#[from] anyhow::Error),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl ReviewSessionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ProjectNotFound => "project_replica_not_found",
            Self::NotFound => "not_found",
            Self::InvalidInput(_) => "invalid_input",
            Self::TargetNotReady(blockers) => blockers
                .first()
                .copied()
                .unwrap_or("review_target_verification_expired"),
            Self::Busy => "review_session_busy",
            Self::NotRecording => "review_session_not_recording",
            Self::ChunkConflict => "review_session_chunk_conflict",
            Self::ChunkCorrupt => "review_session_chunk_corrupt",
            Self::LimitExceeded => "review_session_limit_exceeded",
            Self::LowDiskSpace => "low_disk_space",
            Self::AudioUnavailable => "review_session_audio_unavailable",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::Infrastructure(_) | Self::Database(_) | Self::Io(_) => "internal_error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordingConsent {
    /// The person recording confirmed everyone present knows they are recorded.
    pub all_present_acknowledged: bool,
    /// Opt-in disclosure that a second (or further) speaker will be recorded.
    pub additional_speakers_disclosed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordingAudioFormat {
    pub sample_rate: u32,
    pub channels: u32,
    /// Only `pcm_s16le` (interleaved, little-endian) is accepted.
    pub encoding: String,
}

/// What the browser reported it actually applied. Recorded, not trusted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordingCaptureSettings {
    pub echo_cancellation: Option<bool>,
    pub noise_suppression: Option<bool>,
    pub auto_gain_control: Option<bool>,
    /// e.g. `audioWorkletCurrentFrame`.
    pub clock: String,
    pub base_latency_seconds: Option<f64>,
    pub user_agent_family: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateReviewSessionRequest {
    pub review_target_id: String,
    /// Client-generated idempotency key; retrying create returns the same session.
    pub client_session_key: String,
    pub consent: RecordingConsent,
    pub audio: RecordingAudioFormat,
    pub capture: RecordingCaptureSettings,
    /// `performance.now()` when capture started. Correlation only.
    pub client_started_performance_ms: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct ChunkUpload {
    pub sequence: u32,
    pub start_frame: u64,
    pub frame_count: u32,
    pub sha256: String,
    pub capture_end_performance_ms: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewSessionEventKind {
    /// Reviewer pressed the cue on a known video moment (e.g. first frame).
    ManualSyncCue,
    /// The audio thread skipped frames; `detail.missingFrames` says how many.
    CaptureDiscontinuity,
    /// The browser could not buffer a chunk locally; its frames are lost.
    LocalBufferOverflow,
    /// The input device ended or changed.
    DeviceLost,
    /// The page was hidden, closed or reloaded while capturing.
    PageInterrupted,
    /// A client/server clock correlation sample.
    ClockSample,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewSessionEventRequest {
    pub event_id: String,
    pub kind: ReviewSessionEventKind,
    /// Microphone sample-clock position, frames since capture start.
    pub audio_frame: u64,
    pub performance_ms: Option<f64>,
    /// For a manual cue: the movie time the reviewer says it marks (e.g. 0 for first frame).
    pub declared_movie_seconds: Option<f64>,
    #[serde(default)]
    pub detail: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecordingEndReason {
    Stopped,
    DeviceLost,
    Interrupted,
    Cancelled,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FinalizeReviewSessionRequest {
    /// Total frames the client captured (including any it could not upload).
    pub final_frame_count: u64,
    pub end_reason: RecordingEndReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewSessionState {
    Recording,
    Completed,
    Incomplete,
    Corrupt,
    Deleted,
}

impl ReviewSessionState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Recording => "recording",
            Self::Completed => "completed",
            Self::Incomplete => "incomplete",
            Self::Corrupt => "corrupt",
            Self::Deleted => "deleted",
        }
    }
    fn parse(value: &str) -> Self {
        match value {
            "completed" => Self::Completed,
            "incomplete" => Self::Incomplete,
            "corrupt" => Self::Corrupt,
            "deleted" => Self::Deleted,
            _ => Self::Recording,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkReceipt {
    pub session_id: String,
    pub sequence: u32,
    pub start_frame: u64,
    pub frame_count: u32,
    pub sha256: String,
    pub received_at: String,
    pub duplicate: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameRange {
    pub start_frame: u64,
    pub end_frame: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSessionCoverage {
    pub received_frames: u64,
    pub received_bytes: u64,
    pub chunk_count: u32,
    pub highest_sequence: Option<u32>,
    pub missing_sequences: Vec<u32>,
    /// Frame ranges inside `[0, finalFrameCount)` with no stored audio.
    pub gaps: Vec<FrameRange>,
    pub final_frame_count: Option<u64>,
    pub received_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSessionEvent {
    pub event_id: String,
    pub kind: ReviewSessionEventKind,
    pub audio_frame: u64,
    pub audio_seconds: f64,
    pub performance_ms: Option<f64>,
    pub declared_movie_seconds: Option<f64>,
    pub server_received_at: String,
    pub detail: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSessionChunkSummary {
    pub sequence: u32,
    pub start_frame: u64,
    pub frame_count: u32,
    pub byte_length: u32,
    pub sha256: String,
    pub received_at: String,
    pub capture_end_performance_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSessionSummary {
    pub session_id: String,
    pub project_id: String,
    pub review_target_id: String,
    pub target_digest: String,
    pub revision_id: String,
    pub viewed_artifact_id: String,
    pub state: ReviewSessionState,
    /// Recording past its lifetime without finalization.
    pub abandoned: bool,
    pub end_reason: Option<RecordingEndReason>,
    pub sample_rate: u32,
    pub channels: u32,
    pub encoding: String,
    pub created_at: String,
    pub finalized_at: Option<String>,
    pub deleted_at: Option<String>,
    pub received_seconds: f64,
    pub manual_cue_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSessionDetail {
    #[serde(flatten)]
    pub summary: ReviewSessionSummary,
    pub consent: RecordingConsent,
    pub capture: RecordingCaptureSettings,
    pub created_by: ReviewTargetActor,
    pub client_started_performance_ms: Option<f64>,
    pub recording_expires_at: String,
    pub coverage: ReviewSessionCoverage,
    pub chunks: Vec<ReviewSessionChunkSummary>,
    pub events: Vec<ReviewSessionEvent>,
    pub limits: ReviewSessionLimits,
    pub storage: ReviewSessionStorage,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSessionLimits {
    pub max_chunk_bytes: usize,
    pub max_session_bytes: u64,
    pub recording_lifetime_hours: i64,
}

/// Plain statement of where evidence lives and what happens to it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSessionStorage {
    pub location: &'static str,
    pub retention: &'static str,
    pub backup: &'static str,
}

const STORAGE: ReviewSessionStorage = ReviewSessionStorage {
    location: "Private server data directory on this companion server (never the derived/media root, never served publicly).",
    retention: "Kept until you delete it. Deletion removes the audio permanently and keeps a metadata tombstone.",
    backup: "Raw audio is not included in server backups; metadata is. Download the WAV if you need a copy.",
};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSessionPage {
    pub items: Vec<ReviewSessionSummary>,
    pub next_before: Option<i64>,
}

/// Contiguous stored audio for a completed session, for streaming as WAV.
#[derive(Debug, Clone)]
pub struct ReviewSessionAudio {
    pub sample_rate: u32,
    pub channels: u32,
    pub data_bytes: u64,
    pub chunk_paths: Vec<PathBuf>,
}

#[derive(Clone)]
pub struct ReviewSessions {
    database_path: PathBuf,
    evidence_root: PathBuf,
    minimum_free_bytes: u64,
}

fn timestamp(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn opaque(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn invalid(message: impl Into<String>) -> ReviewSessionError {
    ReviewSessionError::InvalidInput(message.into())
}

fn end_reason_str(reason: RecordingEndReason) -> &'static str {
    match reason {
        RecordingEndReason::Stopped => "stopped",
        RecordingEndReason::DeviceLost => "deviceLost",
        RecordingEndReason::Interrupted => "interrupted",
        RecordingEndReason::Cancelled => "cancelled",
    }
}

fn parse_end_reason(value: &str) -> Option<RecordingEndReason> {
    match value {
        "stopped" => Some(RecordingEndReason::Stopped),
        "deviceLost" => Some(RecordingEndReason::DeviceLost),
        "interrupted" => Some(RecordingEndReason::Interrupted),
        "cancelled" => Some(RecordingEndReason::Cancelled),
        _ => None,
    }
}

fn event_kind_str(kind: ReviewSessionEventKind) -> &'static str {
    match kind {
        ReviewSessionEventKind::ManualSyncCue => "manualSyncCue",
        ReviewSessionEventKind::CaptureDiscontinuity => "captureDiscontinuity",
        ReviewSessionEventKind::LocalBufferOverflow => "localBufferOverflow",
        ReviewSessionEventKind::DeviceLost => "deviceLost",
        ReviewSessionEventKind::PageInterrupted => "pageInterrupted",
        ReviewSessionEventKind::ClockSample => "clockSample",
    }
}

fn parse_event_kind(value: &str) -> ReviewSessionEventKind {
    match value {
        "manualSyncCue" => ReviewSessionEventKind::ManualSyncCue,
        "captureDiscontinuity" => ReviewSessionEventKind::CaptureDiscontinuity,
        "localBufferOverflow" => ReviewSessionEventKind::LocalBufferOverflow,
        "deviceLost" => ReviewSessionEventKind::DeviceLost,
        "pageInterrupted" => ReviewSessionEventKind::PageInterrupted,
        _ => ReviewSessionEventKind::ClockSample,
    }
}

fn free_bytes(path: &Path) -> Option<u64> {
    fs2::available_space(path).ok()
}

impl ReviewSessions {
    pub(crate) fn new(database_path: PathBuf, minimum_free_bytes: u64) -> Self {
        let evidence_root = database_path
            .parent()
            .map(|parent| parent.join(EVIDENCE_DIRECTORY))
            .unwrap_or_else(|| PathBuf::from(EVIDENCE_DIRECTORY));
        Self {
            database_path,
            evidence_root,
            minimum_free_bytes,
        }
    }

    fn session_directory(&self, session_id: &str) -> PathBuf {
        self.evidence_root.join(session_id)
    }

    fn chunk_path(&self, session_id: &str, sequence: u32) -> PathBuf {
        self.session_directory(session_id)
            .join(format!("{sequence:08}.pcm"))
    }

    fn project_registered(
        connection: &Connection,
        project_id: &str,
    ) -> Result<(), ReviewSessionError> {
        connection
            .query_row(
                "SELECT 1 FROM project_registry WHERE project_id=?1 AND state='registered'",
                [project_id],
                |_| Ok(()),
            )
            .optional()?
            .ok_or(ReviewSessionError::ProjectNotFound)
    }

    /// Create (or idempotently return) a recording session bound to an
    /// imported review target whose readiness is `ready`.
    pub fn create(
        &self,
        project_id: &str,
        request: &CreateReviewSessionRequest,
        actor: &ReviewTargetActor,
        now: DateTime<Utc>,
    ) -> Result<ReviewSessionDetail, ReviewSessionError> {
        if !opaque(&request.client_session_key, 64) || !opaque(&request.review_target_id, 64) {
            return Err(invalid(
                "opaque review target and client session key required",
            ));
        }
        if !request.consent.all_present_acknowledged {
            return Err(invalid(
                "recording requires acknowledgement that everyone present knows they are recorded",
            ));
        }
        let audio = &request.audio;
        if audio.encoding != "pcm_s16le"
            || !SUPPORTED_SAMPLE_RATES.contains(&audio.sample_rate)
            || !(1..=2).contains(&audio.channels)
        {
            return Err(invalid(
                "audio must be pcm_s16le, 1-2 channels, at a supported sample rate",
            ));
        }
        let capture = &request.capture;
        if !opaque(&capture.clock, 64)
            || capture
                .user_agent_family
                .as_deref()
                .is_some_and(|value| !opaque(value, 32))
            || capture
                .base_latency_seconds
                .is_some_and(|value| !value.is_finite() || !(0.0..=10.0).contains(&value))
            || request
                .client_started_performance_ms
                .is_some_and(|value| !value.is_finite() || value < 0.0)
        {
            return Err(invalid("invalid capture settings"));
        }

        let mut connection = open_writable_connection(&self.database_path)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::project_registered(&tx, project_id)?;

        if let Some((session_id, target)) = tx
            .query_row(
                "SELECT session_id, review_target_id FROM review_sessions
                 WHERE project_id=?1 AND client_session_key=?2",
                params![project_id, request.client_session_key],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if target != request.review_target_id {
                return Err(ReviewSessionError::IdempotencyConflict);
            }
            let detail = load_detail(&tx, project_id, &session_id, now)?;
            tx.commit()?;
            return Ok(detail);
        }

        let target = crate::ReviewTargets::new(self.database_path.clone());
        let readiness = match target.get_in(&tx, project_id, &request.review_target_id, now) {
            Ok(detail) => detail,
            Err(crate::ReviewTargetServiceError::NotFound) => {
                return Err(ReviewSessionError::NotFound);
            }
            Err(error) => return Err(anyhow::anyhow!(error).into()),
        };
        if !readiness.summary.readiness.ready {
            return Err(ReviewSessionError::TargetNotReady(
                readiness.summary.readiness.blockers.clone(),
            ));
        }
        let active: u32 = tx.query_row(
            "SELECT COUNT(*) FROM review_sessions WHERE state='recording' AND recording_expires_at > ?1",
            [timestamp(now)],
            |row| row.get(0),
        )?;
        if active >= REVIEW_SESSION_MAX_ACTIVE {
            return Err(ReviewSessionError::Busy);
        }
        let session_id = format!("rsession_{}", Uuid::new_v4().simple());
        tx.execute(
            "INSERT INTO review_sessions(
                session_id, project_id, review_target_id, target_digest, revision_id,
                viewed_artifact_id, client_session_key, state, consent_json, capture_json,
                sample_rate, channels, encoding, client_started_performance_ms,
                created_at, recording_expires_at, created_by_type, created_by_id)
             VALUES(?1,?2,?3,?4,?5,?6,?7,'recording',?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            params![
                session_id,
                project_id,
                request.review_target_id,
                readiness.summary.target_digest,
                readiness.summary.revision_id,
                readiness.summary.viewed_artifact_id,
                request.client_session_key,
                serde_json::to_string(&request.consent).context("serialize consent")?,
                serde_json::to_string(capture).context("serialize capture")?,
                audio.sample_rate,
                audio.channels,
                audio.encoding,
                request.client_started_performance_ms,
                timestamp(now),
                timestamp(now + Duration::hours(REVIEW_SESSION_RECORDING_LIFETIME_HOURS)),
                actor.actor_type,
                actor.actor_id,
            ],
        )?;
        let detail = load_detail(&tx, project_id, &session_id, now)?;
        tx.commit()?;
        Ok(detail)
    }

    /// Store one chunk. Identical retries return the original receipt;
    /// conflicting retries, overlaps and hash mismatches are refused.
    pub fn put_chunk(
        &self,
        project_id: &str,
        session_id: &str,
        chunk: &ChunkUpload,
        bytes: &[u8],
        now: DateTime<Utc>,
    ) -> Result<ChunkReceipt, ReviewSessionError> {
        if bytes.is_empty() || bytes.len() > REVIEW_SESSION_MAX_CHUNK_BYTES {
            return Err(invalid("chunk must be 1 byte to 2 MiB"));
        }
        if chunk.sha256.len() != 64 || !chunk.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid("sha256 must be 64 hex characters"));
        }
        if chunk
            .capture_end_performance_ms
            .is_some_and(|v| !v.is_finite() || v < 0.0)
        {
            return Err(invalid("invalid capture time"));
        }
        let declared = chunk.sha256.to_ascii_lowercase();
        if sha256_hex(bytes) != declared {
            return Err(ReviewSessionError::ChunkCorrupt);
        }

        let mut connection = open_writable_connection(&self.database_path)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::project_registered(&tx, project_id)?;
        let session = load_session_row(&tx, project_id, session_id)?;
        if let Some(existing) = tx
            .query_row(
                "SELECT start_frame, frame_count, sha256, received_at FROM review_session_chunks
                 WHERE session_id=?1 AND sequence=?2",
                params![session_id, chunk.sequence],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, u32>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?
        {
            if existing.0 as u64 == chunk.start_frame
                && existing.1 == chunk.frame_count
                && existing.2 == declared
            {
                return Ok(ChunkReceipt {
                    session_id: session_id.into(),
                    sequence: chunk.sequence,
                    start_frame: chunk.start_frame,
                    frame_count: chunk.frame_count,
                    sha256: declared,
                    received_at: existing.3,
                    duplicate: true,
                });
            }
            return Err(ReviewSessionError::ChunkConflict);
        }
        if session.state != ReviewSessionState::Recording {
            return Err(ReviewSessionError::NotRecording);
        }
        let frame_bytes = 2 * session.channels as usize;
        if chunk.frame_count == 0 || bytes.len() != chunk.frame_count as usize * frame_bytes {
            return Err(invalid("chunk length must equal frameCount × channels × 2"));
        }
        let end_frame = chunk
            .start_frame
            .checked_add(u64::from(chunk.frame_count))
            .ok_or_else(|| invalid("frame overflow"))?;
        let max_frames = u64::from(session.sample_rate) * 7 * 3600;
        if end_frame > max_frames {
            return Err(ReviewSessionError::LimitExceeded);
        }
        let overlap: Option<u32> = tx
            .query_row(
                "SELECT sequence FROM review_session_chunks
                 WHERE session_id=?1 AND start_frame < ?3 AND start_frame + frame_count > ?2 LIMIT 1",
                params![session_id, chunk.start_frame as i64, end_frame as i64],
                |row| row.get(0),
            )
            .optional()?;
        if overlap.is_some() {
            return Err(ReviewSessionError::ChunkConflict);
        }
        let stored: i64 = tx.query_row(
            "SELECT COALESCE(SUM(byte_length),0) FROM review_session_chunks WHERE session_id=?1",
            [session_id],
            |row| row.get(0),
        )?;
        if stored as u64 + bytes.len() as u64 > REVIEW_SESSION_MAX_BYTES {
            return Err(ReviewSessionError::LimitExceeded);
        }

        let directory = self.session_directory(session_id);
        fs::create_dir_all(&directory)?;
        if free_bytes(&directory)
            .is_some_and(|free| free < self.minimum_free_bytes.saturating_add(bytes.len() as u64))
        {
            return Err(ReviewSessionError::LowDiskSpace);
        }
        let path = self.chunk_path(session_id, chunk.sequence);
        let staging = directory.join(format!(
            ".{:08}.{}.tmp",
            chunk.sequence,
            Uuid::new_v4().simple()
        ));
        {
            let mut file = File::create(&staging)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        fs::rename(&staging, &path)?;
        let received_at = timestamp(now);
        tx.execute(
            "INSERT INTO review_session_chunks(
                session_id, sequence, start_frame, frame_count, byte_length, sha256,
                received_at, capture_end_performance_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                session_id,
                chunk.sequence,
                chunk.start_frame as i64,
                chunk.frame_count,
                bytes.len() as u32,
                declared,
                received_at,
                chunk.capture_end_performance_ms,
            ],
        )?;
        if let Err(error) = tx.commit() {
            // The row is the commit point; an orphan file is removed so it is
            // never mistaken for accepted evidence.
            let _ = fs::remove_file(&path);
            return Err(error.into());
        }
        Ok(ChunkReceipt {
            session_id: session_id.into(),
            sequence: chunk.sequence,
            start_frame: chunk.start_frame,
            frame_count: chunk.frame_count,
            sha256: declared,
            received_at,
            duplicate: false,
        })
    }

    /// Record a manual cue or capture-health event; idempotent by `eventId`.
    pub fn record_event(
        &self,
        project_id: &str,
        session_id: &str,
        event: &ReviewSessionEventRequest,
        now: DateTime<Utc>,
    ) -> Result<ReviewSessionEvent, ReviewSessionError> {
        if !opaque(&event.event_id, 64) {
            return Err(invalid("opaque eventId required"));
        }
        if event
            .performance_ms
            .is_some_and(|v| !v.is_finite() || v < 0.0)
            || event
                .declared_movie_seconds
                .is_some_and(|v| !v.is_finite() || !(0.0..=21_600.0).contains(&v))
        {
            return Err(invalid("invalid event time"));
        }
        let detail = match &event.detail {
            Some(value) => {
                let text = serde_json::to_string(value).context("serialize event detail")?;
                if text.len() > 2048 || !value.is_object() {
                    return Err(invalid("event detail must be an object of at most 2 KiB"));
                }
                Some(text)
            }
            None => None,
        };
        let mut connection = open_writable_connection(&self.database_path)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::project_registered(&tx, project_id)?;
        let session = load_session_row(&tx, project_id, session_id)?;
        let existing = load_events(&tx, session_id, session.sample_rate)?
            .into_iter()
            .find(|e| e.event_id == event.event_id);
        if let Some(existing) = existing {
            if existing.kind != event.kind || existing.audio_frame != event.audio_frame {
                return Err(ReviewSessionError::IdempotencyConflict);
            }
            return Ok(existing);
        }
        if session.state == ReviewSessionState::Deleted {
            return Err(ReviewSessionError::NotRecording);
        }
        let count: u32 = tx.query_row(
            "SELECT COUNT(*) FROM review_session_events WHERE session_id=?1",
            [session_id],
            |row| row.get(0),
        )?;
        if count >= REVIEW_SESSION_MAX_EVENTS {
            return Err(ReviewSessionError::LimitExceeded);
        }
        tx.execute(
            "INSERT INTO review_session_events(
                session_id, event_id, kind, audio_frame, performance_ms,
                declared_movie_seconds, server_received_at, detail_json)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                session_id,
                event.event_id,
                event_kind_str(event.kind),
                event.audio_frame as i64,
                event.performance_ms,
                event.declared_movie_seconds,
                timestamp(now),
                detail,
            ],
        )?;
        let stored = load_events(&tx, session_id, session.sample_rate)?
            .into_iter()
            .find(|e| e.event_id == event.event_id)
            .context("stored event")?;
        tx.commit()?;
        Ok(stored)
    }

    /// Close capture and compute coverage on the server. Re-hashes every
    /// stored chunk: a missing or modified file makes the session `corrupt`.
    /// Repeating an identical finalization returns the stored result.
    pub fn finalize(
        &self,
        project_id: &str,
        session_id: &str,
        request: &FinalizeReviewSessionRequest,
        now: DateTime<Utc>,
    ) -> Result<ReviewSessionDetail, ReviewSessionError> {
        let mut connection = open_writable_connection(&self.database_path)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::project_registered(&tx, project_id)?;
        let session = load_session_row(&tx, project_id, session_id)?;
        if session.state != ReviewSessionState::Recording {
            if session.final_frame_count == Some(request.final_frame_count)
                && session.end_reason == Some(request.end_reason)
            {
                let detail = load_detail(&tx, project_id, session_id, now)?;
                tx.commit()?;
                return Ok(detail);
            }
            return Err(ReviewSessionError::NotRecording);
        }
        let chunks = load_chunks(&tx, session_id)?;
        let max_end = chunks
            .iter()
            .map(|c| c.start_frame + u64::from(c.frame_count))
            .max()
            .unwrap_or(0);
        if request.final_frame_count < max_end {
            return Err(invalid(
                "finalFrameCount is smaller than audio already received",
            ));
        }
        let mut corrupt = false;
        for chunk in &chunks {
            let path = self.chunk_path(session_id, chunk.sequence);
            match fs::read(&path) {
                Ok(bytes) if sha256_hex(&bytes) == chunk.sha256 => {}
                _ => corrupt = true,
            }
        }
        let coverage = coverage(
            &chunks,
            Some(request.final_frame_count),
            session.sample_rate,
        );
        let state = if corrupt {
            ReviewSessionState::Corrupt
        } else if coverage.gaps.is_empty() && !chunks.is_empty() {
            ReviewSessionState::Completed
        } else {
            ReviewSessionState::Incomplete
        };
        tx.execute(
            "UPDATE review_sessions SET state=?2, final_frame_count=?3, end_reason=?4, finalized_at=?5
             WHERE session_id=?1",
            params![
                session_id,
                state.as_str(),
                request.final_frame_count as i64,
                end_reason_str(request.end_reason),
                timestamp(now),
            ],
        )?;
        let detail = load_detail(&tx, project_id, session_id, now)?;
        tx.commit()?;
        Ok(detail)
    }

    /// Permanently remove the audio; keep a metadata tombstone and events.
    pub fn delete(
        &self,
        project_id: &str,
        session_id: &str,
        now: DateTime<Utc>,
    ) -> Result<ReviewSessionDetail, ReviewSessionError> {
        let mut connection = open_writable_connection(&self.database_path)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::project_registered(&tx, project_id)?;
        let session = load_session_row(&tx, project_id, session_id)?;
        if session.state != ReviewSessionState::Deleted {
            tx.execute(
                "UPDATE review_sessions SET state='deleted', deleted_at=?2,
                    final_frame_count=COALESCE(final_frame_count,
                        (SELECT MAX(start_frame + frame_count) FROM review_session_chunks WHERE session_id=?1)),
                    finalized_at=COALESCE(finalized_at, ?2)
                 WHERE session_id=?1",
                params![session_id, timestamp(now)],
            )?;
            tx.execute(
                "UPDATE review_session_chunks SET deleted=1 WHERE session_id=?1",
                [session_id],
            )?;
        }
        let detail = load_detail(&tx, project_id, session_id, now)?;
        tx.commit()?;
        let directory = self.session_directory(session_id);
        if directory.exists() {
            fs::remove_dir_all(&directory)?;
        }
        Ok(detail)
    }

    pub fn get(
        &self,
        project_id: &str,
        session_id: &str,
        now: DateTime<Utc>,
    ) -> Result<ReviewSessionDetail, ReviewSessionError> {
        let connection = open_read_only_connection(&self.database_path)?;
        Self::project_registered(&connection, project_id)?;
        load_detail(&connection, project_id, session_id, now)
    }

    pub fn list(
        &self,
        project_id: &str,
        review_target_id: Option<&str>,
        limit: u32,
        before: Option<i64>,
        now: DateTime<Utc>,
    ) -> Result<ReviewSessionPage, ReviewSessionError> {
        if limit == 0 || limit > REVIEW_SESSION_LIST_MAX_LIMIT {
            return Err(invalid("limit must be 1..=200"));
        }
        let connection = open_read_only_connection(&self.database_path)?;
        Self::project_registered(&connection, project_id)?;
        let mut statement = connection.prepare(
            "SELECT sequence, session_id FROM review_sessions
             WHERE project_id=?1 AND (?2 IS NULL OR review_target_id=?2) AND sequence < ?3
             ORDER BY sequence DESC LIMIT ?4",
        )?;
        let rows = statement
            .query_map(
                params![
                    project_id,
                    review_target_id,
                    before.unwrap_or(i64::MAX),
                    i64::from(limit) + 1
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = rows.len() > limit as usize;
        let mut items = Vec::new();
        let mut last = None;
        for (sequence, id) in rows.into_iter().take(limit as usize) {
            items.push(load_detail(&connection, project_id, &id, now)?.summary);
            last = Some(sequence);
        }
        Ok(ReviewSessionPage {
            items,
            next_before: if has_more { last } else { None },
        })
    }

    /// Contiguous audio of a completed session, re-verified chunk by chunk.
    /// Incomplete, corrupt, recording and deleted sessions are refused rather
    /// than padded: missing audio is never synthesized.
    pub fn audio(
        &self,
        project_id: &str,
        session_id: &str,
    ) -> Result<ReviewSessionAudio, ReviewSessionError> {
        let connection = open_read_only_connection(&self.database_path)?;
        Self::project_registered(&connection, project_id)?;
        let session = load_session_row(&connection, project_id, session_id)?;
        if session.state != ReviewSessionState::Completed {
            return Err(ReviewSessionError::AudioUnavailable);
        }
        let mut chunks = load_chunks(&connection, session_id)?;
        chunks.sort_by_key(|c| c.start_frame);
        let mut paths = Vec::with_capacity(chunks.len());
        let mut data_bytes = 0_u64;
        for chunk in &chunks {
            let path = self.chunk_path(session_id, chunk.sequence);
            let bytes = fs::read(&path).map_err(|_| ReviewSessionError::AudioUnavailable)?;
            if sha256_hex(&bytes) != chunk.sha256 {
                return Err(ReviewSessionError::AudioUnavailable);
            }
            data_bytes += bytes.len() as u64;
            paths.push(path);
        }
        Ok(ReviewSessionAudio {
            sample_rate: session.sample_rate,
            channels: session.channels,
            data_bytes,
            chunk_paths: paths,
        })
    }

    /// Exact stored bytes of one chunk (any non-deleted state), re-verified.
    pub fn chunk_bytes(
        &self,
        project_id: &str,
        session_id: &str,
        sequence: u32,
    ) -> Result<Vec<u8>, ReviewSessionError> {
        let connection = open_read_only_connection(&self.database_path)?;
        Self::project_registered(&connection, project_id)?;
        let session = load_session_row(&connection, project_id, session_id)?;
        if session.state == ReviewSessionState::Deleted {
            return Err(ReviewSessionError::AudioUnavailable);
        }
        let chunk = load_chunks(&connection, session_id)?
            .into_iter()
            .find(|c| c.sequence == sequence)
            .ok_or(ReviewSessionError::NotFound)?;
        let bytes = fs::read(self.chunk_path(session_id, sequence))
            .map_err(|_| ReviewSessionError::AudioUnavailable)?;
        if sha256_hex(&bytes) != chunk.sha256 {
            return Err(ReviewSessionError::AudioUnavailable);
        }
        Ok(bytes)
    }
}

/// Canonical 44-byte RIFF/WAVE header for interleaved 16-bit PCM.
pub fn wav_header(sample_rate: u32, channels: u32, data_bytes: u64) -> Vec<u8> {
    let data = data_bytes.min(u64::from(u32::MAX - 36)) as u32;
    let block_align = (channels * 2) as u16;
    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&(36 + data).to_le_bytes());
    header.extend_from_slice(b"WAVEfmt ");
    header.extend_from_slice(&16_u32.to_le_bytes());
    header.extend_from_slice(&1_u16.to_le_bytes());
    header.extend_from_slice(&(channels as u16).to_le_bytes());
    header.extend_from_slice(&sample_rate.to_le_bytes());
    header.extend_from_slice(&(sample_rate * u32::from(block_align)).to_le_bytes());
    header.extend_from_slice(&block_align.to_le_bytes());
    header.extend_from_slice(&16_u16.to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&data.to_le_bytes());
    header
}

struct SessionRow {
    project_id: String,
    review_target_id: String,
    target_digest: String,
    revision_id: String,
    viewed_artifact_id: String,
    state: ReviewSessionState,
    consent: RecordingConsent,
    capture: RecordingCaptureSettings,
    sample_rate: u32,
    channels: u32,
    encoding: String,
    client_started_performance_ms: Option<f64>,
    created_at: String,
    recording_expires_at: String,
    created_by: ReviewTargetActor,
    final_frame_count: Option<u64>,
    end_reason: Option<RecordingEndReason>,
    finalized_at: Option<String>,
    deleted_at: Option<String>,
}

fn load_session_row(
    connection: &Connection,
    project_id: &str,
    session_id: &str,
) -> Result<SessionRow, ReviewSessionError> {
    let row = connection
        .query_row(
            "SELECT sequence, project_id, review_target_id, target_digest, revision_id,
                    viewed_artifact_id, state, consent_json, capture_json, sample_rate, channels,
                    encoding, client_started_performance_ms, created_at, recording_expires_at,
                    created_by_type, created_by_id, final_frame_count, end_reason, finalized_at,
                    deleted_at
             FROM review_sessions WHERE project_id=?1 AND session_id=?2",
            params![project_id, session_id],
            |row| {
                Ok((
                    (
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                    ),
                    (
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, u32>(9)?,
                        row.get::<_, u32>(10)?,
                        row.get::<_, String>(11)?,
                        row.get::<_, Option<f64>>(12)?,
                        row.get::<_, String>(13)?,
                    ),
                    (
                        row.get::<_, String>(14)?,
                        row.get::<_, String>(15)?,
                        row.get::<_, String>(16)?,
                        row.get::<_, Option<i64>>(17)?,
                        row.get::<_, Option<String>>(18)?,
                        row.get::<_, Option<String>>(19)?,
                        row.get::<_, Option<String>>(20)?,
                    ),
                ))
            },
        )
        .optional()?
        .ok_or(ReviewSessionError::NotFound)?;
    let (a, b, c) = row;
    Ok(SessionRow {
        project_id: a.1,
        review_target_id: a.2,
        target_digest: a.3,
        revision_id: a.4,
        viewed_artifact_id: a.5,
        state: ReviewSessionState::parse(&a.6),
        consent: serde_json::from_str(&b.0).context("decode consent")?,
        capture: serde_json::from_str(&b.1).context("decode capture")?,
        sample_rate: b.2,
        channels: b.3,
        encoding: b.4,
        client_started_performance_ms: b.5,
        created_at: b.6,
        recording_expires_at: c.0,
        created_by: ReviewTargetActor {
            actor_type: c.1,
            actor_id: c.2,
        },
        final_frame_count: c.3.map(|v| v as u64),
        end_reason: c.4.as_deref().and_then(parse_end_reason),
        finalized_at: c.5,
        deleted_at: c.6,
    })
}

fn load_chunks(
    connection: &Connection,
    session_id: &str,
) -> Result<Vec<ReviewSessionChunkSummary>, ReviewSessionError> {
    let mut statement = connection.prepare(
        "SELECT sequence, start_frame, frame_count, byte_length, sha256, received_at,
                capture_end_performance_ms
         FROM review_session_chunks WHERE session_id=?1 ORDER BY sequence",
    )?;
    let rows = statement
        .query_map([session_id], |row| {
            Ok(ReviewSessionChunkSummary {
                sequence: row.get(0)?,
                start_frame: row.get::<_, i64>(1)? as u64,
                frame_count: row.get(2)?,
                byte_length: row.get(3)?,
                sha256: row.get(4)?,
                received_at: row.get(5)?,
                capture_end_performance_ms: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn load_events(
    connection: &Connection,
    session_id: &str,
    sample_rate: u32,
) -> Result<Vec<ReviewSessionEvent>, ReviewSessionError> {
    let mut statement = connection.prepare(
        "SELECT event_id, kind, audio_frame, performance_ms, declared_movie_seconds,
                server_received_at, detail_json
         FROM review_session_events WHERE session_id=?1 ORDER BY audio_frame, sequence",
    )?;
    let rows = statement
        .query_map([session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<f64>>(3)?,
                row.get::<_, Option<f64>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|row| {
            Ok(ReviewSessionEvent {
                event_id: row.0,
                kind: parse_event_kind(&row.1),
                audio_frame: row.2 as u64,
                audio_seconds: row.2 as f64 / f64::from(sample_rate),
                performance_ms: row.3,
                declared_movie_seconds: row.4,
                server_received_at: row.5,
                detail: row
                    .6
                    .map(|text| serde_json::from_str(&text))
                    .transpose()
                    .context("decode event detail")?,
            })
        })
        .collect()
}

fn coverage(
    chunks: &[ReviewSessionChunkSummary],
    final_frame_count: Option<u64>,
    sample_rate: u32,
) -> ReviewSessionCoverage {
    let mut ranges: Vec<(u64, u64)> = chunks
        .iter()
        .map(|c| (c.start_frame, c.start_frame + u64::from(c.frame_count)))
        .collect();
    ranges.sort();
    let received_frames: u64 = ranges.iter().map(|(a, b)| b - a).sum();
    let mut gaps = Vec::new();
    if let Some(total) = final_frame_count {
        let mut cursor = 0_u64;
        for (start, end) in &ranges {
            if *start > cursor {
                gaps.push(FrameRange {
                    start_frame: cursor,
                    end_frame: *start,
                });
            }
            cursor = cursor.max(*end);
        }
        if total > cursor {
            gaps.push(FrameRange {
                start_frame: cursor,
                end_frame: total,
            });
        }
    }
    let highest_sequence = chunks.iter().map(|c| c.sequence).max();
    let present: std::collections::HashSet<u32> = chunks.iter().map(|c| c.sequence).collect();
    let missing_sequences = highest_sequence
        .map(|high| (0..high).filter(|s| !present.contains(s)).collect())
        .unwrap_or_default();
    ReviewSessionCoverage {
        received_frames,
        received_bytes: chunks.iter().map(|c| u64::from(c.byte_length)).sum(),
        chunk_count: chunks.len() as u32,
        highest_sequence,
        missing_sequences,
        gaps,
        final_frame_count,
        received_seconds: received_frames as f64 / f64::from(sample_rate),
    }
}

fn load_detail(
    connection: &Connection,
    project_id: &str,
    session_id: &str,
    now: DateTime<Utc>,
) -> Result<ReviewSessionDetail, ReviewSessionError> {
    let row = load_session_row(connection, project_id, session_id)?;
    let chunks = load_chunks(connection, session_id)?;
    let events = load_events(connection, session_id, row.sample_rate)?;
    let coverage = coverage(&chunks, row.final_frame_count, row.sample_rate);
    let abandoned = row.state == ReviewSessionState::Recording
        && DateTime::parse_from_rfc3339(&row.recording_expires_at)
            .map(|at| now > at)
            .unwrap_or(false);
    Ok(ReviewSessionDetail {
        summary: ReviewSessionSummary {
            session_id: session_id.to_string(),
            project_id: row.project_id,
            review_target_id: row.review_target_id,
            target_digest: row.target_digest,
            revision_id: row.revision_id,
            viewed_artifact_id: row.viewed_artifact_id,
            state: row.state,
            abandoned,
            end_reason: row.end_reason,
            sample_rate: row.sample_rate,
            channels: row.channels,
            encoding: row.encoding,
            created_at: row.created_at,
            finalized_at: row.finalized_at,
            deleted_at: row.deleted_at,
            received_seconds: coverage.received_seconds,
            manual_cue_count: events
                .iter()
                .filter(|e| e.kind == ReviewSessionEventKind::ManualSyncCue)
                .count() as u32,
        },
        consent: row.consent,
        capture: row.capture,
        created_by: row.created_by,
        client_started_performance_ms: row.client_started_performance_ms,
        recording_expires_at: row.recording_expires_at,
        coverage,
        chunks,
        events,
        limits: ReviewSessionLimits {
            max_chunk_bytes: REVIEW_SESSION_MAX_CHUNK_BYTES,
            max_session_bytes: REVIEW_SESSION_MAX_BYTES,
            recording_lifetime_hours: REVIEW_SESSION_RECORDING_LIFETIME_HOURS,
        },
        storage: STORAGE,
    })
}

pub(crate) fn migrate(tx: &Transaction<'_>) -> anyhow::Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE review_sessions (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL UNIQUE,
            project_id TEXT NOT NULL REFERENCES project_registry(project_id),
            review_target_id TEXT NOT NULL REFERENCES review_targets(review_target_id),
            target_digest TEXT NOT NULL,
            revision_id TEXT NOT NULL,
            viewed_artifact_id TEXT NOT NULL,
            client_session_key TEXT NOT NULL,
            state TEXT NOT NULL CHECK(state IN ('recording','completed','incomplete','corrupt','deleted')),
            consent_json TEXT NOT NULL CHECK(length(consent_json) <= 1024),
            capture_json TEXT NOT NULL CHECK(length(capture_json) <= 2048),
            sample_rate INTEGER NOT NULL CHECK(sample_rate > 0),
            channels INTEGER NOT NULL CHECK(channels BETWEEN 1 AND 2),
            encoding TEXT NOT NULL CHECK(encoding = 'pcm_s16le'),
            client_started_performance_ms REAL,
            created_at TEXT NOT NULL,
            recording_expires_at TEXT NOT NULL,
            created_by_type TEXT NOT NULL,
            created_by_id TEXT NOT NULL,
            final_frame_count INTEGER CHECK(final_frame_count IS NULL OR final_frame_count >= 0),
            end_reason TEXT CHECK(end_reason IS NULL OR end_reason IN ('stopped','deviceLost','interrupted','cancelled')),
            finalized_at TEXT,
            deleted_at TEXT,
            UNIQUE(project_id, client_session_key)
        );
        CREATE INDEX review_sessions_target ON review_sessions(project_id, review_target_id, sequence);
        CREATE TABLE review_session_chunks (
            session_id TEXT NOT NULL REFERENCES review_sessions(session_id),
            sequence INTEGER NOT NULL CHECK(sequence >= 0),
            start_frame INTEGER NOT NULL CHECK(start_frame >= 0),
            frame_count INTEGER NOT NULL CHECK(frame_count > 0),
            byte_length INTEGER NOT NULL CHECK(byte_length > 0 AND byte_length <= 2097152),
            sha256 TEXT NOT NULL CHECK(length(sha256) = 64),
            received_at TEXT NOT NULL,
            capture_end_performance_ms REAL,
            deleted INTEGER NOT NULL DEFAULT 0 CHECK(deleted IN (0,1)),
            PRIMARY KEY(session_id, sequence)
        );
        CREATE INDEX review_session_chunk_frames ON review_session_chunks(session_id, start_frame);
        CREATE TABLE review_session_events (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL REFERENCES review_sessions(session_id),
            event_id TEXT NOT NULL,
            kind TEXT NOT NULL CHECK(kind IN ('manualSyncCue','captureDiscontinuity','localBufferOverflow','deviceLost','pageInterrupted','clockSample')),
            audio_frame INTEGER NOT NULL CHECK(audio_frame >= 0),
            performance_ms REAL,
            declared_movie_seconds REAL,
            server_received_at TEXT NOT NULL,
            detail_json TEXT CHECK(detail_json IS NULL OR length(detail_json) <= 2048),
            UNIQUE(session_id, event_id)
        );
        "#,
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "review_sessions_tests.rs"]
mod tests;
