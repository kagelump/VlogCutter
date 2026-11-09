//! Review-session speech transcription (#224).
//!
//! Transcribes a recorded TV voice review (#222) into timed utterances on the
//! **recording clock**. It reuses the shared Rust speech pieces — the
//! `SttEngine` seam, Silero VAD gate, 30-second window packing, coverage
//! re-split, the whisper.cpp engine and the strict native OpenRouter transport —
//! but keeps its results in its own tables. Nothing here writes source-asset
//! transcripts, project annotations, edit plans or feedback.
//!
//! Policy decisions:
//! - The VAD gate always runs locally first; silence is never sent to a model.
//! - Recording gaps are never bridged: audio is processed in contiguous
//!   segments (≤10 min) inside received ranges only.
//! - Quality gates flag a window's utterances rather than discarding the whole
//!   conversation; flagged text is kept as evidence and marked suppressed.
//! - Every decoded window is cached by `(run, segment, window, fingerprint)`.
//!   A retry reuses it, so billable work is never repeated.
//! - Cloud dispatch markers avoid duplicate sends after a worker crash. Costs
//!   are informational receipts after execution. Explicit cloud retries create
//!   new runs; no spending or acknowledgement gate is applied.
//! - Whisper output is not speaker evidence. Without a diarization backend
//!   every utterance is `unknown` speaker and the transcript says so.
use crate::{
    ClaimedJob, FailureKind, JobClass, JobDefinition, JobEngine, JobHandlerFailure, JobId,
    JobRequirements, JobResultCommit, RetryPolicy, ReviewTargetActor, open_read_only_connection,
    open_writable_connection,
};
use anyhow::{Context, anyhow};
use chrono::{SecondsFormat, Utc};
use rcdvlog_core::NativeSttProtocolError;
use rcdvlog_core::review_speech::{DecodedSegment, RejectReason, reject_reason};
use rcdvlog_core::review_speech::{
    NativeSttCost, NativeSttOutcome, NativeSttRequest, NativeSttTransport, OPENROUTER_STT_PROVIDER,
    OPENROUTER_WHISPER_MODEL, OpenRouterSttClient,
};
use rcdvlog_core::review_speech::{
    NativeSttModelChoice, NativeSttProviderError, canonical_native_stt_language,
    discover_native_stt_models,
};
use rcdvlog_core::review_speech::{PackedWindow, SpeechRegion, pack_windows, translate_span};
use rcdvlog_core::review_speech::{SttEngine, VAD_MODEL, WHISPER_MODEL};
use rcdvlog_core::review_speech::{needs_resplit, resplit};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

mod live;
pub(crate) use live::migrate_live;
pub use live::{LIVE_REVIEW_MODEL, LiveReviewLine, LiveReviewTranscript};

pub const REVIEW_TRANSCRIPT_SCHEMA: &str = "review-transcript.v1";
pub const REVIEW_TRANSCRIBE_JOB_TYPE: &str = "review_session.transcribe.v1";
pub const REVIEW_TRANSCRIBE_RECIPE: &str = "review-stt.vad-silero-v5.win30.seg600.v1";
const SEGMENT_SECONDS: u64 = 600;
const ASR_SAMPLE_RATE: u32 = 16_000;
/// Languages this product expects reviewers to speak; others are flagged, not dropped.
const EXPECTED_LANGUAGES: &[&str] = &["en", "zh"];
const NEAR_GAP_SECONDS: f64 = 1.0;

pub const REVIEW_TRANSCRIPT_LIMITATIONS: &[&str] = &[
    "Times are on the microphone recording clock, not movie time. Synchronization is a separate step.",
    "No speaker diarization backend is configured: every utterance is speaker `unknown`. Whisper output is not speaker evidence.",
    "TV dialogue and background speech are not separated from the reviewers; transcribed film audio may appear as utterances.",
    "Language tags are reported by the model per segment and are not verified; unexpected languages are flagged, not removed.",
    "Traditional Chinese display text is an OpenCC (s2tw) character projection; the raw transcript text is preserved unchanged.",
    "Recording gaps are never transcribed or filled.",
];

#[derive(Debug, Error)]
pub enum ReviewTranscriptError {
    #[error("project is not registered on this server")]
    ProjectNotFound,
    #[error("review session or transcription not found")]
    NotFound,
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("transcription preflight failed")]
    NotReady(Vec<&'static str>),
    #[error("OpenRouter transcription models could not be loaded")]
    ModelsUnavailable,
    #[error("transcription request conflicts with an existing run")]
    Conflict,
    #[error(transparent)]
    Infrastructure(#[from] anyhow::Error),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
}

impl ReviewTranscriptError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ProjectNotFound => "project_replica_not_found",
            Self::NotFound => "not_found",
            Self::InvalidInput(_) => "invalid_input",
            Self::NotReady(blockers) => blockers
                .first()
                .copied()
                .unwrap_or("review_transcription_not_ready"),
            Self::Conflict => "conflict",
            Self::ModelsUnavailable => "review_transcription_models_unavailable",
            Self::Infrastructure(_) | Self::Database(_) => "internal_error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewTranscriptionBackend {
    /// whisper.cpp on this companion server, using installed shared models.
    Local,
    /// The registered native OpenRouter audio-transcription model.
    Openrouter,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewTranscriptionRequest {
    pub backend: ReviewTranscriptionBackend,
    /// For `openrouter`: a model from the native transcription catalog.
    #[serde(default)]
    pub model: Option<String>,
    /// Optional ISO 639-1 hint (cloud only). Omit for code-switching speech.
    #[serde(default)]
    pub language_hint: Option<String>,
    /// Legacy request field, accepted for saved-run compatibility; no longer enforced.
    #[serde(default)]
    pub authorized_max_cost_usd: Option<f64>,
    /// Legacy request field; selecting OpenRouter authorizes upload.
    #[serde(default)]
    pub cloud_upload_acknowledged: bool,
    #[serde(default)]
    pub zero_data_retention: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionEstimate {
    pub received_audio_seconds: f64,
    pub segment_count: usize,
    /// Upper bound on 30 s windows before VAD (actual is usually far fewer).
    pub max_windows: u64,
    pub price_usd_per_second: Option<f64>,
    /// Worst case: every window reserved at its full registered duration.
    pub max_cost_usd: Option<f64>,
    pub billing_note: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTranscriptionPreflight {
    pub ready: bool,
    pub blockers: Vec<&'static str>,
    pub warnings: Vec<&'static str>,
    pub backend: ReviewTranscriptionBackend,
    pub provider: String,
    pub model: String,
    pub audio_fingerprint: Option<String>,
    pub estimate: Option<TranscriptionEstimate>,
    pub existing_run_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewTranscriptionState {
    Queued,
    Running,
    Completed,
    NoSpeech,
    Partial,
    Failed,
    Cancelled,
}

impl ReviewTranscriptionState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::NoSpeech => "noSpeech",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
    fn parse(value: &str) -> Self {
        match value {
            "running" => Self::Running,
            "completed" => Self::Completed,
            "noSpeech" => Self::NoSpeech,
            "partial" => Self::Partial,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Queued,
        }
    }
    fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewUtterance {
    pub utterance_id: String,
    pub segment_index: u32,
    pub window_index: u32,
    pub start_frame: u64,
    pub end_frame: u64,
    pub start_seconds: f64,
    pub end_seconds: f64,
    /// Exactly as decoded. Never translated, converted or edited.
    pub text: String,
    /// Model-reported language tag; empty when the model reported none.
    pub language: String,
    /// `latin`, `hanTraditional`, `hanSimplified`, `hanAmbiguous`, `mixed`, `other`.
    pub script: String,
    /// Traditional Chinese display projection (equal to `text` when unchanged).
    pub display_text: String,
    pub display_projection: Option<String>,
    /// `unknown` unless a diarization backend supplies evidence.
    pub speaker: String,
    pub speaker_evidence: String,
    pub mean_token_probability: Option<f32>,
    pub flags: Vec<String>,
    /// True when a quality gate fired; the text is kept as evidence only.
    pub suppressed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTranscriptionSegment {
    pub segment_index: u32,
    pub start_frame: u64,
    pub end_frame: u64,
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub state: String,
    pub speech_seconds: f64,
    pub window_count: u32,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTranscriptionSpend {
    pub authorized_max_usd: Option<f64>,
    pub reserved_usd: f64,
    pub known_cost_usd: f64,
    pub unknown_cost_windows: u32,
    pub dispatched_windows: u32,
    pub cached_windows: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDiarization {
    pub status: &'static str,
    pub reason: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTranscriptionRun {
    pub schema: &'static str,
    pub run_id: String,
    pub project_id: String,
    pub session_id: String,
    pub review_target_id: String,
    pub revision_id: String,
    pub backend: ReviewTranscriptionBackend,
    pub provider: String,
    pub model: String,
    pub recipe: String,
    pub settings: ReviewTranscriptionRequest,
    pub audio_fingerprint: String,
    pub sample_rate: u32,
    pub state: ReviewTranscriptionState,
    pub error_code: Option<String>,
    pub attempt_count: u32,
    pub job_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub created_by: ReviewTargetActor,
    pub segments: Vec<ReviewTranscriptionSegment>,
    pub utterances: Vec<ReviewUtterance>,
    pub recording_gaps: Vec<crate::FrameRange>,
    pub spend: ReviewTranscriptionSpend,
    pub diarization: ReviewDiarization,
    pub limitations: &'static [&'static str],
    pub provider_errors: Vec<ReviewTranscriptionProviderError>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTranscriptionProviderError {
    pub segment_index: u32,
    pub window_index: u32,
    pub protocol_code: String,
    pub http_status: Option<u16>,
    pub code: Option<String>,
    pub message: String,
    pub generation_id: Option<String>,
}

/// Model availability and transports, behind one seam so tests use fakes.
pub trait ReviewSpeechRuntime: Send + Sync {
    fn live_models_present(&self) -> bool {
        false
    }
    fn live_engine(&self) -> anyhow::Result<Box<dyn SttEngine>> {
        anyhow::bail!("live model unavailable")
    }
    fn local_models_present(&self) -> bool;
    fn vad_model_present(&self) -> bool;
    /// VAD + whisper decode on this host.
    fn local_engine(&self) -> anyhow::Result<Box<dyn SttEngine>>;
    /// Local Silero VAD only, used to gate cloud requests.
    fn detect_speech(&self, pcm: &[f32]) -> anyhow::Result<Vec<SpeechRegion>>;
    fn cloud_models(&self) -> anyhow::Result<Vec<NativeSttModelChoice>> {
        Ok(vec![NativeSttModelChoice {
            id: OPENROUTER_WHISPER_MODEL.into(),
            name: "OpenAI: Whisper".into(),
        }])
    }
    fn cloud_credentials_present(&self) -> bool;
    fn cloud_transport(&self) -> anyhow::Result<Box<dyn NativeSttTransport>>;
}

/// Production runtime: installed shared models, never downloaded here.
pub struct ModelDirSpeechRuntime {
    model_dir: PathBuf,
}

impl ModelDirSpeechRuntime {
    /// `RCDVLOG_SPEECH_MODEL_DIR`, else the shared application model directory.
    pub fn from_environment() -> Self {
        let model_dir = std::env::var_os("RCDVLOG_SPEECH_MODEL_DIR")
            .map(PathBuf::from)
            .or_else(|| rcdvlog_core::review_speech::shared_model_dir().ok())
            .unwrap_or_else(|| PathBuf::from("models"));
        Self { model_dir }
    }
    fn whisper_path(&self) -> PathBuf {
        self.model_dir.join(WHISPER_MODEL.file_name)
    }
    fn vad_path(&self) -> PathBuf {
        self.model_dir.join(VAD_MODEL.file_name)
    }
}

impl ReviewSpeechRuntime for ModelDirSpeechRuntime {
    fn live_models_present(&self) -> bool {
        self.model_dir.join(LIVE_REVIEW_MODEL.file_name).is_file() && self.vad_path().is_file()
    }
    fn live_engine(&self) -> anyhow::Result<Box<dyn SttEngine>> {
        Ok(Box::new(rcdvlog_core::review_speech::WhisperEngine::new(
            &self.model_dir.join(LIVE_REVIEW_MODEL.file_name),
            &self.vad_path(),
        )?))
    }
    fn local_models_present(&self) -> bool {
        self.whisper_path().is_file() && self.vad_path().is_file()
    }
    fn vad_model_present(&self) -> bool {
        self.vad_path().is_file()
    }
    fn local_engine(&self) -> anyhow::Result<Box<dyn SttEngine>> {
        Ok(Box::new(rcdvlog_core::review_speech::WhisperEngine::new(
            &self.whisper_path(),
            &self.vad_path(),
        )?))
    }
    fn detect_speech(&self, pcm: &[f32]) -> anyhow::Result<Vec<SpeechRegion>> {
        rcdvlog_core::review_speech::detect_silero_speech(&self.vad_path(), 4, pcm)
    }
    fn cloud_models(&self) -> anyhow::Result<Vec<NativeSttModelChoice>> {
        discover_native_stt_models()
    }
    fn cloud_credentials_present(&self) -> bool {
        rcdvlog_core::review_speech::resolve_api_key().is_ok()
    }
    fn cloud_transport(&self) -> anyhow::Result<Box<dyn NativeSttTransport>> {
        Ok(Box::new(OpenRouterSttClient::from_environment()?))
    }
}

#[derive(Clone)]
pub struct ReviewTranscripts {
    database_path: PathBuf,
    evidence_root: PathBuf,
}

fn now_text() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn invalid(message: impl Into<String>) -> ReviewTranscriptError {
    ReviewTranscriptError::InvalidInput(message.into())
}

fn micro(usd: f64) -> i64 {
    (usd * 1_000_000.0).round() as i64
}

fn usd(micro: i64) -> f64 {
    micro as f64 / 1_000_000.0
}

struct ResolvedRequest {
    provider: String,
    model: String,
}

fn resolve_request(
    request: &ReviewTranscriptionRequest,
) -> Result<ResolvedRequest, ReviewTranscriptError> {
    match request.backend {
        ReviewTranscriptionBackend::Local => {
            if request.model.is_some()
                || request.language_hint.is_some()
                || request.authorized_max_cost_usd.is_some()
                || request.cloud_upload_acknowledged
                || request.zero_data_retention
            {
                return Err(invalid(
                    "local transcription takes no model, hint, budget or cloud settings",
                ));
            }
            Ok(ResolvedRequest {
                provider: "local".into(),
                model: WHISPER_MODEL.file_name.into(),
            })
        }
        ReviewTranscriptionBackend::Openrouter => {
            let model = request
                .model
                .clone()
                .unwrap_or_else(|| OPENROUTER_WHISPER_MODEL.to_string());
            NativeSttRequest::for_discovered_model(model.clone(), request.language_hint.clone())
                .map_err(|_| invalid("invalid native transcription model or language"))?;
            Ok(ResolvedRequest {
                provider: OPENROUTER_STT_PROVIDER.into(),
                model,
            })
        }
    }
}

fn settings_fingerprint(
    request: &ReviewTranscriptionRequest,
    resolved: &ResolvedRequest,
) -> String {
    let mut document = serde_json::json!({
        "recipe": REVIEW_TRANSCRIBE_RECIPE,
        "backend": request.backend,
        "provider": resolved.provider,
        "model": resolved.model,
        "languageHint": request.language_hint,
        "zeroDataRetention": false,
        "contract": "review-transcription-post-execution-cost-v2",
    });
    if request.backend == ReviewTranscriptionBackend::Openrouter {
        document["responseCompatibility"] =
            serde_json::json!("known-language-code.whisper-input-grid20ms.v1");
    }
    blake3::hash(document.to_string().as_bytes())
        .to_hex()
        .to_string()
}

struct SessionAudio {
    session_id: String,
    state: String,
    sample_rate: u32,
    channels: u32,
    final_frame_count: Option<u64>,
    chunks: Vec<(u32, u64, u32, String)>,
}

fn load_session_audio(
    connection: &Connection,
    project_id: &str,
    session_id: &str,
) -> Result<SessionAudio, ReviewTranscriptError> {
    let row = connection
        .query_row(
            "SELECT review_target_id, revision_id, state, sample_rate, channels, final_frame_count
             FROM review_sessions WHERE project_id=?1 AND session_id=?2",
            params![project_id, session_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, u32>(3)?,
                    r.get::<_, u32>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                ))
            },
        )
        .optional()?
        .ok_or(ReviewTranscriptError::NotFound)?;
    let mut statement = connection.prepare(
        "SELECT sequence, start_frame, frame_count, sha256 FROM review_session_chunks
         WHERE session_id=?1 ORDER BY start_frame",
    )?;
    let chunks = statement
        .query_map([session_id], |r| {
            Ok((
                r.get::<_, u32>(0)?,
                r.get::<_, i64>(1)? as u64,
                r.get::<_, u32>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SessionAudio {
        session_id: session_id.into(),
        state: row.2,
        sample_rate: row.3,
        channels: row.4,
        final_frame_count: row.5.map(|v| v as u64),
        chunks,
    })
}

impl SessionAudio {
    fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(format!(
            "pcm_s16le:{}:{}\n",
            self.sample_rate, self.channels
        ));
        for (sequence, start, count, sha) in &self.chunks {
            hasher.update(format!("{sequence}:{start}:{count}:{sha}\n"));
        }
        hex::encode(hasher.finalize())
    }

    /// Contiguous received ranges, split into ≤10-minute processing segments.
    fn segments(&self) -> Vec<(u64, u64)> {
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        for (_, start, count, _) in &self.chunks {
            let end = start + u64::from(*count);
            match ranges.last_mut() {
                Some(last) if last.1 == *start => last.1 = end,
                _ => ranges.push((*start, end)),
            }
        }
        let limit = SEGMENT_SECONDS * u64::from(self.sample_rate);
        let mut out = Vec::new();
        for (start, end) in ranges {
            let mut cursor = start;
            while cursor < end {
                let next = (cursor + limit).min(end);
                out.push((cursor, next));
                cursor = next;
            }
        }
        out
    }

    fn gaps(&self) -> Vec<crate::FrameRange> {
        let mut gaps = Vec::new();
        let mut cursor = 0_u64;
        for (_, start, count, _) in &self.chunks {
            if *start > cursor {
                gaps.push(crate::FrameRange {
                    start_frame: cursor,
                    end_frame: *start,
                });
            }
            cursor = cursor.max(start + u64::from(*count));
        }
        if let Some(total) = self.final_frame_count
            && total > cursor
        {
            gaps.push(crate::FrameRange {
                start_frame: cursor,
                end_frame: total,
            });
        }
        gaps
    }
}

impl ReviewTranscripts {
    pub(crate) fn new(database_path: PathBuf) -> Self {
        let evidence_root = database_path
            .parent()
            .map(|p| p.join("review-evidence"))
            .unwrap_or_else(|| PathBuf::from("review-evidence"));
        Self {
            database_path,
            evidence_root,
        }
    }

    fn project_registered(
        connection: &Connection,
        project_id: &str,
    ) -> Result<(), ReviewTranscriptError> {
        connection
            .query_row(
                "SELECT 1 FROM project_registry WHERE project_id=?1 AND state='registered'",
                [project_id],
                |_| Ok(()),
            )
            .optional()?
            .ok_or(ReviewTranscriptError::ProjectNotFound)
    }

    pub fn models(
        &self,
        project_id: &str,
        runtime: &dyn ReviewSpeechRuntime,
    ) -> Result<Vec<NativeSttModelChoice>, ReviewTranscriptError> {
        let connection = open_read_only_connection(&self.database_path)?;
        Self::project_registered(&connection, project_id)?;
        runtime
            .cloud_models()
            .map_err(|_| ReviewTranscriptError::ModelsUnavailable)
    }

    /// Checks recording and provider readiness. Never sends audio,
    /// downloads a model or initializes an engine.
    pub fn preflight(
        &self,
        project_id: &str,
        session_id: &str,
        request: &ReviewTranscriptionRequest,
        runtime: &dyn ReviewSpeechRuntime,
    ) -> Result<ReviewTranscriptionPreflight, ReviewTranscriptError> {
        let connection = open_read_only_connection(&self.database_path)?;
        Self::project_registered(&connection, project_id)?;
        let audio = load_session_audio(&connection, project_id, session_id)?;
        let resolved = resolve_request(request)?;
        let mut blockers = Vec::new();
        let mut warnings = vec!["diarization_unavailable", "tv_dialogue_not_separated"];
        if !matches!(audio.state.as_str(), "completed" | "incomplete") {
            blockers.push("review_transcription_session_not_finalized");
        }
        if audio.chunks.is_empty() {
            blockers.push("review_transcription_no_audio");
        }
        if audio.state == "incomplete" {
            warnings.push("recording_has_gaps");
        }
        match request.backend {
            ReviewTranscriptionBackend::Local => {
                if !runtime.local_models_present() {
                    blockers.push("review_transcription_local_model_unavailable");
                }
            }
            ReviewTranscriptionBackend::Openrouter => {
                if !runtime.vad_model_present() {
                    blockers.push("review_transcription_local_model_unavailable");
                }
                if !runtime.cloud_credentials_present() {
                    blockers.push("review_transcription_provider_unavailable");
                }
            }
        }
        let segments = audio.segments();
        let received_frames: u64 = segments.iter().map(|(a, b)| b - a).sum();
        let received_seconds = received_frames as f64 / f64::from(audio.sample_rate.max(1));
        let max_windows: u64 = segments
            .iter()
            .map(|(a, b)| ((b - a) as f64 / f64::from(audio.sample_rate) / 30.0).ceil() as u64)
            .sum();
        if request.backend == ReviewTranscriptionBackend::Openrouter && blockers.is_empty() {
            let models = runtime
                .cloud_models()
                .map_err(|_| ReviewTranscriptError::ModelsUnavailable)?;
            if !models.iter().any(|model| model.id == resolved.model) {
                return Err(invalid(
                    "model is not an eligible OpenRouter transcription model",
                ));
            }
        }
        let fingerprint = (!audio.chunks.is_empty()).then(|| audio.fingerprint());
        let existing_run_id = (request.backend == ReviewTranscriptionBackend::Local)
            .then_some(())
            .and(fingerprint.as_ref())
            .and_then(|fp| {
                connection
                    .query_row(
                        "SELECT run_id FROM review_transcription_runs WHERE idempotency_key=?1",
                        [idempotency_key(
                            session_id,
                            fp,
                            &settings_fingerprint(request, &resolved),
                        )],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()
                    .ok()
                    .flatten()
            });
        Ok(ReviewTranscriptionPreflight {
            ready: blockers.is_empty(),
            blockers,
            warnings,
            backend: request.backend,
            provider: resolved.provider,
            model: resolved.model,
            audio_fingerprint: fingerprint,
            estimate: Some(TranscriptionEstimate {
                received_audio_seconds: received_seconds,
                segment_count: segments.len(),
                max_windows,
                price_usd_per_second: None,
                max_cost_usd: None,
                billing_note: None,
            }),
            existing_run_id,
        })
    }

    /// Enqueue a durable job. Local retries reuse cached work; explicit cloud
    /// retries create fresh immutable attempts, while active work is deduplicated.
    pub fn start(
        &self,
        project_id: &str,
        session_id: &str,
        request: &ReviewTranscriptionRequest,
        runtime: &dyn ReviewSpeechRuntime,
        jobs: &JobEngine,
        actor: &ReviewTargetActor,
    ) -> Result<ReviewTranscriptionRun, ReviewTranscriptError> {
        let mut effective = request.clone();
        if effective.backend == ReviewTranscriptionBackend::Openrouter {
            effective.authorized_max_cost_usd = None;
            effective.cloud_upload_acknowledged = true;
            effective.zero_data_retention = false;
        }
        let request = &effective;
        let preflight = self.preflight(project_id, session_id, request, runtime)?;
        if !preflight.ready {
            return Err(ReviewTranscriptError::NotReady(preflight.blockers));
        }
        let resolved = resolve_request(request)?;
        let fingerprint = preflight
            .audio_fingerprint
            .clone()
            .context("audio fingerprint")?;
        let cloud = request.backend == ReviewTranscriptionBackend::Openrouter;
        let base_key = idempotency_key(
            session_id,
            &fingerprint,
            &settings_fingerprint(request, &resolved),
        );
        let key = if cloud {
            format!("{base_key}:{}", Uuid::new_v4().simple())
        } else {
            base_key.clone()
        };
        let mut connection = open_writable_connection(&self.database_path)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let audio = load_session_audio(&tx, project_id, session_id)?;
        let existing = if cloud {
            tx.query_row(
                "SELECT run_id, state, attempt_count FROM review_transcription_runs WHERE idempotency_key LIKE ?1 AND state IN ('queued','running') ORDER BY created_at DESC LIMIT 1",
                [format!("{base_key}:%")], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, u32>(2)?)),
            ).optional()?
        } else {
            tx.query_row("SELECT run_id, state, attempt_count FROM review_transcription_runs WHERE idempotency_key=?1",
                [&key], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, u32>(2)?))).optional()?
        };
        let (run_id, attempt) = match existing {
            Some((run_id, state, attempts)) => {
                let state = ReviewTranscriptionState::parse(&state);
                if !state.is_terminal()
                    || matches!(
                        state,
                        ReviewTranscriptionState::Completed | ReviewTranscriptionState::NoSpeech
                    )
                {
                    let run = load_run(&tx, project_id, &run_id)?;
                    tx.commit()?;
                    return Ok(run);
                }
                // Local retries resume cached work. Cloud retries create new runs.
                tx.execute(
                    "UPDATE review_transcription_runs SET state='queued', error_code=NULL, cancel_requested=0,
                        attempt_count=attempt_count+1, authorized_max_micro_usd=?3, updated_at=?2 WHERE run_id=?1",
                    params![run_id, now_text(), None::<i64>],
                )?;
                tx.execute(
                    "UPDATE review_transcription_segments SET state='pending', error_code=NULL
                     WHERE run_id=?1 AND state='failed' AND COALESCE(error_code,'') <> 'review_transcription_dispatch_outcome_unknown'",
                    [&run_id],
                )?;
                (run_id, attempts + 1)
            }
            None => {
                let run_id = format!("rtrans_{}", Uuid::new_v4().simple());
                tx.execute(
                    "INSERT INTO review_transcription_runs(
                        run_id, project_id, session_id, backend, provider, model, settings_json,
                        idempotency_key, audio_fingerprint, state, authorized_max_micro_usd,
                        attempt_count, created_at, updated_at, created_by_type, created_by_id)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'queued',?10,1,?11,?11,?12,?13)",
                    params![
                        run_id,
                        project_id,
                        session_id,
                        match request.backend {
                            ReviewTranscriptionBackend::Local => "local",
                            ReviewTranscriptionBackend::Openrouter => "openrouter",
                        },
                        resolved.provider,
                        resolved.model,
                        serde_json::to_string(request).context("serialize settings")?,
                        key,
                        fingerprint,
                        None::<i64>,
                        now_text(),
                        actor.actor_type,
                        actor.actor_id,
                    ],
                )?;
                for (index, (start, end)) in audio.segments().into_iter().enumerate() {
                    tx.execute(
                        "INSERT INTO review_transcription_segments(run_id, segment_index, start_frame, end_frame, state)
                         VALUES(?1,?2,?3,?4,'pending')",
                        params![run_id, index as u32, start as i64, end as i64],
                    )?;
                }
                (run_id, 1)
            }
        };
        let job_id = format!(
            "job_rtrans_{}_{attempt}",
            run_id.trim_start_matches("rtrans_")
        );
        tx.execute(
            "UPDATE review_transcription_runs SET job_id=?2 WHERE run_id=?1",
            params![run_id, job_id],
        )?;
        tx.commit()?;
        let definition = JobDefinition {
            job_id: JobId::parse(job_id.clone()).map_err(|e| anyhow!(e.to_string()))?,
            job_type: REVIEW_TRANSCRIBE_JOB_TYPE.into(),
            payload_version: 1,
            payload: serde_json::json!({"runId": run_id, "projectId": project_id}),
            immutable_identity: serde_json::json!({"runId": run_id, "attempt": attempt}),
            requirements: JobRequirements {
                schema_version: 1,
                requires_original: false,
                requires_derived_write: false,
                capabilities: vec![],
            },
            target_node_id: jobs.node_id().clone(),
            project_id: None,
            media_object_id: None,
            storage_root_id: None,
            representation_id: None,
            recipe_key: REVIEW_TRANSCRIBE_RECIPE.into(),
            // Scheduled as lightweight; local decoding takes the shared
            // local-heavy filesystem lease itself so it never overlaps proxies.
            job_class: JobClass::Lightweight,
            idempotency_key: job_id.clone(),
            priority: 0,
            total_work_units: audio.segments().len().max(1) as u64,
            total_items: audio.segments().len().max(1) as u64,
            retry_policy: RetryPolicy {
                max_attempts: 1,
                initial_backoff_seconds: 5,
                max_backoff_seconds: 5,
            },
            dependencies: vec![],
        };
        jobs.enqueue_with_id(&job_id, definition)
            .map_err(|e| anyhow!(e.to_string()))?;
        let connection = open_read_only_connection(&self.database_path)?;
        load_run(&connection, project_id, &run_id)
    }

    pub fn cancel(
        &self,
        project_id: &str,
        run_id: &str,
        jobs: &JobEngine,
    ) -> Result<ReviewTranscriptionRun, ReviewTranscriptError> {
        let connection = open_writable_connection(&self.database_path)?;
        Self::project_registered(&connection, project_id)?;
        let run = load_run(&connection, project_id, run_id)?;
        if !run.state.is_terminal() {
            connection.execute(
                "UPDATE review_transcription_runs SET cancel_requested=1, updated_at=?2 WHERE run_id=?1",
                params![run_id, now_text()],
            )?;
            if run.state == ReviewTranscriptionState::Queued {
                connection.execute(
                    "UPDATE review_transcription_runs SET state='cancelled', error_code='review_transcription_cancelled' WHERE run_id=?1",
                    [run_id],
                )?;
            }
            if let Some(job) = &run.job_id
                && let Ok(job) = JobId::parse(job.clone())
            {
                let _ = jobs.request_cancel(&job, Some("review transcription cancelled"));
            }
        }
        load_run(&connection, project_id, run_id)
    }

    pub fn get(
        &self,
        project_id: &str,
        run_id: &str,
    ) -> Result<ReviewTranscriptionRun, ReviewTranscriptError> {
        let connection = open_read_only_connection(&self.database_path)?;
        Self::project_registered(&connection, project_id)?;
        load_run(&connection, project_id, run_id)
    }

    pub fn list(
        &self,
        project_id: &str,
        session_id: &str,
    ) -> Result<Vec<ReviewTranscriptionRun>, ReviewTranscriptError> {
        let connection = open_read_only_connection(&self.database_path)?;
        Self::project_registered(&connection, project_id)?;
        let mut statement = connection.prepare(
            "SELECT run_id FROM review_transcription_runs WHERE project_id=?1 AND session_id=?2 ORDER BY created_at DESC LIMIT 50",
        )?;
        let ids = statement
            .query_map(params![project_id, session_id], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.iter()
            .map(|id| load_run(&connection, project_id, id))
            .collect()
    }

    /// Mark interrupted cloud dispatches unknown and release unsent
    /// reservations. Called by the handler before resuming a run.
    fn recover_dispatches(&self, run_id: &str) -> Result<(), ReviewTranscriptError> {
        let connection = open_writable_connection(&self.database_path)?;
        connection.execute(
            "UPDATE review_transcription_windows SET state='unknown', updated_at=?2
             WHERE run_id=?1 AND state='dispatched'",
            params![run_id, now_text()],
        )?;
        connection.execute(
            "UPDATE review_transcription_windows SET state='released', updated_at=?2
             WHERE run_id=?1 AND state='reserved'",
            params![run_id, now_text()],
        )?;
        connection.execute(
            "UPDATE review_transcription_segments SET state='failed', error_code='review_transcription_dispatch_outcome_unknown'
             WHERE run_id=?1 AND state<>'done' AND state<>'noSpeech' AND EXISTS(
                SELECT 1 FROM review_transcription_windows w WHERE w.run_id=?1
                  AND w.segment_index=review_transcription_segments.segment_index AND w.state='unknown')",
            [run_id],
        )?;
        Ok(())
    }

    /// Execute a run to a terminal state. Safe to call again after a crash:
    /// finished segments and cached windows are reused.
    pub fn execute(
        &self,
        run_id: &str,
        runtime: &dyn ReviewSpeechRuntime,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ReviewTranscriptionState, ReviewTranscriptError> {
        self.recover_dispatches(run_id)?;
        let (project_id, settings, backend) = {
            let connection = open_read_only_connection(&self.database_path)?;
            connection.query_row(
                "SELECT project_id, settings_json, backend FROM review_transcription_runs WHERE run_id=?1",
                [run_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
            ).optional()?.ok_or(ReviewTranscriptError::NotFound)?
        };
        let request: ReviewTranscriptionRequest =
            serde_json::from_str(&settings).context("decode run settings")?;
        let resolved = resolve_request(&request)?;
        let run = {
            let connection = open_read_only_connection(&self.database_path)?;
            load_run(&connection, &project_id, run_id)?
        };
        if run.state.is_terminal() {
            return Ok(run.state);
        }
        let audio = {
            let connection = open_read_only_connection(&self.database_path)?;
            load_session_audio(&connection, &project_id, &run.session_id)?
        };
        if audio.fingerprint() != run.audio_fingerprint {
            return self.finish(
                run_id,
                ReviewTranscriptionState::Failed,
                Some("review_transcription_audio_changed"),
            );
        }
        self.set_state(run_id, ReviewTranscriptionState::Running, None)?;

        // Local decoding joins the one shared local-heavy slot.
        let _lease = if backend == "local" {
            match rcdvlog_core::acquire_local_heavy_at_cancellable(
                &rcdvlog_core::default_local_heavy_lock_path(),
                is_cancelled,
            ) {
                Ok(lease) => Some(lease),
                Err(_) if is_cancelled() => {
                    return self.finish(
                        run_id,
                        ReviewTranscriptionState::Cancelled,
                        Some("review_transcription_cancelled"),
                    );
                }
                Err(_) => {
                    return self.finish(
                        run_id,
                        ReviewTranscriptionState::Failed,
                        Some("review_transcription_resource_unavailable"),
                    );
                }
            }
        } else {
            None
        };

        let engine: Box<dyn SttEngine> = match request.backend {
            ReviewTranscriptionBackend::Local => match runtime.local_engine() {
                Ok(engine) => engine,
                Err(_) => {
                    return self.finish(
                        run_id,
                        ReviewTranscriptionState::Failed,
                        Some("review_transcription_local_model_unavailable"),
                    );
                }
            },
            ReviewTranscriptionBackend::Openrouter => {
                let transport = match runtime.cloud_transport() {
                    Ok(transport) => transport,
                    Err(_) => {
                        return self.finish(
                            run_id,
                            ReviewTranscriptionState::Failed,
                            Some("review_transcription_provider_unavailable"),
                        );
                    }
                };
                let native = NativeSttRequest::for_discovered_model(
                    resolved.model.clone(),
                    request.language_hint.clone(),
                )?;
                Box::new(CloudEngine {
                    runtime,
                    transport,
                    request: native,
                })
            }
        };

        let segments = {
            let connection = open_read_only_connection(&self.database_path)?;
            let mut statement = connection.prepare(
                "SELECT segment_index, start_frame, end_frame, state FROM review_transcription_segments WHERE run_id=?1 ORDER BY segment_index",
            )?;
            statement
                .query_map([run_id], |r| {
                    Ok((
                        r.get::<_, u32>(0)?,
                        r.get::<_, i64>(1)? as u64,
                        r.get::<_, i64>(2)? as u64,
                        r.get::<_, String>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let gaps = audio.gaps();
        let ledger = WindowLedger {
            database_path: self.database_path.clone(),
            run_id: run_id.to_string(),
            cloud: request.backend == ReviewTranscriptionBackend::Openrouter,
        };
        for (index, start, end, state) in segments {
            if state != "pending" {
                continue;
            }
            if is_cancelled() || self.cancel_requested(run_id)? {
                return self.finish(
                    run_id,
                    ReviewTranscriptionState::Cancelled,
                    Some("review_transcription_cancelled"),
                );
            }
            let pcm = match self.read_segment_pcm(&audio, start, end) {
                Ok(pcm) => pcm,
                Err(_) => {
                    self.segment_failed(run_id, index, "review_transcription_audio_unavailable")?;
                    continue;
                }
            };
            let outcome = transcribe_segment(
                engine.as_ref(),
                &ledger,
                index,
                start,
                audio.sample_rate,
                &pcm,
                &gaps,
                &|| is_cancelled() || self.cancel_requested(run_id).unwrap_or(false),
            );
            match outcome {
                Ok(result) => self.segment_done(run_id, index, &result)?,
                Err(SegmentFailure::Cancelled) => {
                    return self.finish(
                        run_id,
                        ReviewTranscriptionState::Cancelled,
                        Some("review_transcription_cancelled"),
                    );
                }
                Err(SegmentFailure::DispatchUnknown) => {
                    self.segment_failed(
                        run_id,
                        index,
                        "review_transcription_dispatch_outcome_unknown",
                    )?;
                }
                Err(SegmentFailure::Failed(code)) => self.segment_failed(run_id, index, code)?,
            }
        }
        let connection = open_read_only_connection(&self.database_path)?;
        let states = {
            let mut statement = connection
                .prepare("SELECT state FROM review_transcription_segments WHERE run_id=?1")?;
            statement
                .query_map([run_id], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        drop(connection);
        let done = states.iter().filter(|s| *s == "done").count();
        let silent = states.iter().filter(|s| *s == "noSpeech").count();
        let (state, code) = if done + silent == states.len() {
            if done == 0 {
                (ReviewTranscriptionState::NoSpeech, None)
            } else {
                (ReviewTranscriptionState::Completed, None)
            }
        } else if done + silent == 0 {
            (
                ReviewTranscriptionState::Failed,
                Some("review_transcription_failed"),
            )
        } else {
            (
                ReviewTranscriptionState::Partial,
                Some("review_transcription_partial"),
            )
        };
        self.finish(run_id, state, code)
    }

    fn cancel_requested(&self, run_id: &str) -> Result<bool, ReviewTranscriptError> {
        let connection = open_read_only_connection(&self.database_path)?;
        Ok(connection.query_row(
            "SELECT cancel_requested FROM review_transcription_runs WHERE run_id=?1",
            [run_id],
            |r| r.get::<_, bool>(0),
        )?)
    }

    fn set_state(
        &self,
        run_id: &str,
        state: ReviewTranscriptionState,
        code: Option<&str>,
    ) -> Result<(), ReviewTranscriptError> {
        let connection = open_writable_connection(&self.database_path)?;
        connection.execute(
            "UPDATE review_transcription_runs SET state=?2, error_code=?3, updated_at=?4 WHERE run_id=?1",
            params![run_id, state.as_str(), code, now_text()],
        )?;
        Ok(())
    }

    fn finish(
        &self,
        run_id: &str,
        state: ReviewTranscriptionState,
        code: Option<&str>,
    ) -> Result<ReviewTranscriptionState, ReviewTranscriptError> {
        self.set_state(run_id, state, code)?;
        Ok(state)
    }

    fn segment_done(
        &self,
        run_id: &str,
        index: u32,
        result: &SegmentResult,
    ) -> Result<(), ReviewTranscriptError> {
        let connection = open_writable_connection(&self.database_path)?;
        connection.execute(
            "UPDATE review_transcription_segments SET state=?3, speech_ms=?4, window_count=?5, utterances_json=?6, error_code=NULL
             WHERE run_id=?1 AND segment_index=?2",
            params![
                run_id,
                index,
                if result.utterances.is_empty() && result.speech_ms == 0 { "noSpeech" } else { "done" },
                result.speech_ms,
                result.window_count,
                serde_json::to_string(&result.utterances).context("serialize utterances")?,
            ],
        )?;
        Ok(())
    }

    fn segment_failed(
        &self,
        run_id: &str,
        index: u32,
        code: &str,
    ) -> Result<(), ReviewTranscriptError> {
        let connection = open_writable_connection(&self.database_path)?;
        connection.execute(
            "UPDATE review_transcription_segments SET state='failed', error_code=?3 WHERE run_id=?1 AND segment_index=?2",
            params![run_id, index, code],
        )?;
        Ok(())
    }

    /// Reads `[start, end)` from verified chunk files, downmixes to mono and
    /// resamples to 16 kHz. The range never crosses a gap by construction.
    fn read_segment_pcm(
        &self,
        audio: &SessionAudio,
        start: u64,
        end: u64,
    ) -> anyhow::Result<Vec<f32>> {
        let channels = audio.channels as usize;
        let mut mono = Vec::with_capacity((end - start) as usize);
        for (sequence, chunk_start, count, sha) in &audio.chunks {
            let chunk_end = chunk_start + u64::from(*count);
            if chunk_end <= start || *chunk_start >= end {
                continue;
            }
            let path = self
                .evidence_root
                .join(&audio.session_id)
                .join(format!("{sequence:08}.pcm"));
            let bytes = std::fs::read(&path)?;
            if hex::encode(Sha256::digest(&bytes)) != *sha {
                return Err(anyhow!("stored chunk failed verification"));
            }
            let from = start.max(*chunk_start) - chunk_start;
            let to = end.min(chunk_end) - chunk_start;
            for frame in from..to {
                let mut sum = 0.0_f32;
                for c in 0..channels {
                    let offset = (frame as usize * channels + c) * 2;
                    sum +=
                        f32::from(i16::from_le_bytes([bytes[offset], bytes[offset + 1]])) / 32768.0;
                }
                mono.push(sum / channels as f32);
            }
        }
        Ok(resample_to(&mono, audio.sample_rate, ASR_SAMPLE_RATE))
    }
}

fn idempotency_key(session_id: &str, audio_fingerprint: &str, settings: &str) -> String {
    blake3::hash(format!("{session_id}\0{audio_fingerprint}\0{settings}").as_bytes())
        .to_hex()
        .to_string()
}

/// Band-limited resampling with a Hann-windowed sinc kernel. The cut-off sits
/// below the lower Nyquist rate so decimation does not alias room noise into
/// the speech band.
pub fn resample_to(input: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || input.is_empty() {
        return input.to_vec();
    }
    let ratio = f64::from(to_rate) / f64::from(from_rate);
    let cutoff = ratio.min(1.0) * 0.9;
    let half_width = (16.0 / cutoff).ceil() as i64;
    let output_len = ((input.len() as f64) * ratio).floor() as usize;
    let mut output = Vec::with_capacity(output_len);
    for n in 0..output_len {
        let center = n as f64 / ratio;
        let base = center.floor() as i64;
        let mut acc = 0.0_f64;
        let mut weight = 0.0_f64;
        for k in (base - half_width + 1)..=(base + half_width) {
            if k < 0 || k as usize >= input.len() {
                continue;
            }
            let x = center - k as f64;
            let sinc = if x.abs() < 1e-9 {
                1.0
            } else {
                let arg = std::f64::consts::PI * x * cutoff;
                arg.sin() / arg
            };
            let window = 0.5 + 0.5 * (std::f64::consts::PI * x / half_width as f64).cos();
            let w = sinc * window * cutoff;
            acc += f64::from(input[k as usize]) * w;
            weight += w;
        }
        output.push(if weight.abs() > 1e-12 {
            (acc / weight) as f32
        } else {
            0.0
        });
    }
    output
}

static S2TW: OnceLock<Option<ferrous_opencc::OpenCC>> = OnceLock::new();

fn has_han(text: &str) -> bool {
    text.chars().any(|c| matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F))
}

/// Classify script and project Han text to Traditional (Taiwan) characters.
/// Raw text is never changed; only `display_text` may differ.
pub fn script_and_display(text: &str) -> (String, String, Option<String>) {
    let latin = text.chars().any(|c| c.is_ascii_alphabetic());
    if !has_han(text) {
        let script = if latin { "latin" } else { "other" };
        return (script.into(), text.into(), None);
    }
    let converter = S2TW.get_or_init(|| {
        ferrous_opencc::OpenCC::from_config(ferrous_opencc::config::BuiltinConfig::S2tw).ok()
    });
    let Some(converter) = converter else {
        return ("hanAmbiguous".into(), text.into(), None);
    };
    let converted = converter.convert(text);
    let han_script = if converted == text {
        "hanTraditional"
    } else {
        "hanSimplified"
    };
    let script = if latin { "mixed" } else { han_script };
    if converted == text {
        (script.into(), text.into(), None)
    } else {
        (script.into(), converted, Some("openccS2tw".into()))
    }
}

struct SegmentResult {
    speech_ms: u32,
    window_count: u32,
    utterances: Vec<ReviewUtterance>,
}

enum SegmentFailure {
    Cancelled,
    DispatchUnknown,
    Failed(&'static str),
}

#[derive(Debug, Error)]
enum DispatchError {
    #[error("dispatch outcome unknown")]
    Unknown,
    #[error("provider returned unusable output")]
    Unusable(NativeSttProtocolError),
}

/// Stable, body-free reasons suitable for a transcript segment and UI.
fn provider_failure_code(reason: NativeSttProtocolError) -> &'static str {
    use NativeSttProtocolError::*;
    match reason {
        InvalidTimestamp | MissingTimestamp | OutOfOrderTimestamp => {
            "review_transcription_provider_invalid_timestamp"
        }
        MissingText | MissingLanguage | MissingDuration | MissingSegments => {
            "review_transcription_provider_incomplete_response"
        }
        HttpStatus(401 | 403) => "review_transcription_provider_auth_failed",
        HttpStatus(404) => "review_transcription_provider_no_route",
        HttpStatus(429) => "review_transcription_provider_rate_limited",
        HttpStatus(_) => "review_transcription_provider_http_error",
        _ => "review_transcription_provider_unusable",
    }
}

/// Durable per-window cache and, for cloud, the reservation ledger.
struct WindowLedger {
    database_path: PathBuf,
    run_id: String,
    cloud: bool,
}

fn window_fingerprint(segment: u32, window: &PackedWindow) -> String {
    let mut text = format!("{segment}");
    for region in &window.regions {
        text.push_str(&format!(":{:?}", region));
    }
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

impl WindowLedger {
    fn cached(
        &self,
        segment: u32,
        index: u32,
        fingerprint: &str,
    ) -> anyhow::Result<Option<Vec<DecodedSegment>>> {
        let connection = open_read_only_connection(&self.database_path)?;
        let row = connection
            .query_row(
                "SELECT window_fingerprint, result_json FROM review_transcription_windows
                 WHERE run_id=?1 AND segment_index=?2 AND window_index=?3 AND state='settled'",
                params![self.run_id, segment, index],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
            )
            .optional()?;
        match row {
            Some((stored, Some(json))) if stored == fingerprint => {
                match serde_json::from_str::<CachedWindow>(&json)? {
                    CachedWindow::Segments(rows)
                    | CachedWindow::Completed { segments: rows, .. } => {
                        Ok(Some(rows.into_iter().map(Into::into).collect()))
                    }
                    CachedWindow::Unusable { unusable, .. } => {
                        Err(DispatchError::Unusable(unusable).into())
                    }
                }
            }
            _ => Ok(None),
        }
    }

    /// Durable dispatch identity only. Costs never gate execution.
    fn mark_dispatched(
        &self,
        segment: u32,
        index: u32,
        fingerprint: &str,
    ) -> Result<(), DispatchError> {
        let connection =
            open_writable_connection(&self.database_path).map_err(|_| DispatchError::Unknown)?;
        let inserted = connection.execute(
            "INSERT OR IGNORE INTO review_transcription_windows(run_id,segment_index,window_index,window_fingerprint,state,reserved_micro_usd,cost_known,created_at,updated_at)
             VALUES(?1,?2,?3,?4,'dispatched',0,0,?5,?5)",
            params![self.run_id, segment, index, fingerprint, now_text()],
        ).map_err(|_| DispatchError::Unknown)?;
        if inserted != 1 {
            return Err(DispatchError::Unknown);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn settle(
        &self,
        segment: u32,
        index: u32,
        fingerprint: &str,
        cost_micro: Option<i64>,
        generation_id: Option<&str>,
        actual_provider: Option<&str>,
        actual_model: Option<&str>,
        latency_ms: Option<u64>,
        decoded: &[DecodedSegment],
        input_padding_ms: u32,
    ) -> anyhow::Result<()> {
        let connection = open_writable_connection(&self.database_path)?;
        let segments = decoded.iter().map(CachedSegment::from).collect::<Vec<_>>();
        let json = if input_padding_ms > 0 {
            serde_json::to_string(&CachedWindow::Completed {
                segments,
                input_padding_ms,
            })?
        } else {
            serde_json::to_string(&segments)?
        };
        connection.execute(
            "INSERT INTO review_transcription_windows(run_id, segment_index, window_index, window_fingerprint, state, reserved_micro_usd,
                cost_known, cost_micro_usd, generation_id, actual_provider, actual_model, latency_ms, result_json, created_at, updated_at)
             VALUES(?1,?2,?3,?4,'settled',0,?5,?6,?7,?8,?9,?10,?11,?12,?12)
             ON CONFLICT(run_id, segment_index, window_index) DO UPDATE SET state='settled', cost_known=excluded.cost_known,
                cost_micro_usd=excluded.cost_micro_usd, generation_id=excluded.generation_id, actual_provider=excluded.actual_provider,
                actual_model=excluded.actual_model, latency_ms=excluded.latency_ms, result_json=excluded.result_json,
                window_fingerprint=excluded.window_fingerprint, updated_at=excluded.updated_at",
            params![
                self.run_id,
                segment,
                index,
                fingerprint,
                cost_micro.is_some() || !self.cloud,
                cost_micro.or(if self.cloud { None } else { Some(0) }),
                generation_id,
                actual_provider,
                actual_model,
                latency_ms.map(|v| v as i64),
                json,
                now_text(),
            ],
        )?;
        Ok(())
    }

    fn settle_unusable(
        &self,
        segment: u32,
        index: u32,
        reason: NativeSttProtocolError,
        receipt: Option<CloudReceipt>,
    ) -> anyhow::Result<()> {
        let (cost, generation, provider, model, latency, provider_error) = receipt
            .map(|(c, g, p, m, l, e, _)| (c, g, p, m, Some(l as i64), e))
            .unwrap_or((None, None, None, None, None, None));
        let connection = open_writable_connection(&self.database_path)?;
        connection.execute(
            "UPDATE review_transcription_windows SET state='settled', cost_known=?4, cost_micro_usd=?5,
             generation_id=?6, actual_provider=?7, actual_model=?8, latency_ms=?9, result_json=?10, updated_at=?11
             WHERE run_id=?1 AND segment_index=?2 AND window_index=?3",
            params![self.run_id, segment, index, cost.is_some(), cost, generation, provider, model, latency,
                serde_json::to_string(&CachedWindow::Unusable { unusable: reason, provider_error })?, now_text()],
        )?;
        Ok(())
    }

    fn mark_unknown(&self, segment: u32, index: u32) {
        if let Ok(connection) = open_writable_connection(&self.database_path) {
            let _ = connection.execute(
                "UPDATE review_transcription_windows SET state='unknown', updated_at=?4 WHERE run_id=?1 AND segment_index=?2 AND window_index=?3",
                params![self.run_id, segment, index, now_text()],
            );
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum CachedWindow {
    Segments(Vec<CachedSegment>),
    Completed {
        segments: Vec<CachedSegment>,
        input_padding_ms: u32,
    },
    Unusable {
        unusable: NativeSttProtocolError,
        #[serde(default)]
        provider_error: Option<NativeSttProviderError>,
    },
}

#[derive(Serialize, Deserialize)]
struct CachedSegment {
    start_ms: u32,
    end_ms: u32,
    language: String,
    text: String,
    p: Option<f32>,
}

impl From<&DecodedSegment> for CachedSegment {
    fn from(s: &DecodedSegment) -> Self {
        Self {
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            language: s.language.clone(),
            text: s.text.clone(),
            p: s.mean_token_probability,
        }
    }
}

impl From<CachedSegment> for DecodedSegment {
    fn from(s: CachedSegment) -> Self {
        Self {
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            language: s.language,
            text: s.text,
            mean_token_probability: s.p,
        }
    }
}

/// Cloud decoding behind the shared `SttEngine` seam. VAD is local; each
/// window is one native request. The ledger wraps every call.
struct CloudEngine<'a> {
    runtime: &'a dyn ReviewSpeechRuntime,
    transport: Box<dyn NativeSttTransport>,
    request: NativeSttRequest,
}

/// (known cost µUSD, generation id, actual provider, actual model, latency ms).
type CloudReceipt = (
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
    u64,
    Option<NativeSttProviderError>,
    u32,
);

thread_local! {
    static LAST_CLOUD_RECEIPT: std::cell::RefCell<Option<CloudReceipt>> = const { std::cell::RefCell::new(None) };
}

impl SttEngine for CloudEngine<'_> {
    fn detect_speech(&self, pcm: &[f32]) -> anyhow::Result<Vec<SpeechRegion>> {
        self.runtime.detect_speech(pcm)
    }
    fn decode_window(
        &self,
        pcm: &[f32],
        window: &PackedWindow,
    ) -> anyhow::Result<Vec<DecodedSegment>> {
        let audio = self.request.prepare_audio(pcm, window)?;
        self.transport.prepare()?;
        let result = self
            .transport
            .transcribe(&self.request, &audio)
            .map_err(|_| DispatchError::Unknown)?;
        let cost = match result.receipt.usage.cost {
            NativeSttCost::Known(usd) => Some(micro(usd)),
            NativeSttCost::Unknown => None,
        };
        LAST_CLOUD_RECEIPT.with(|slot| {
            *slot.borrow_mut() = Some((
                cost,
                result.receipt.generation_id.clone(),
                result.receipt.actual_provider.clone(),
                result.receipt.actual_model.clone(),
                result.latency_ms,
                result.provider_error.clone(),
                audio.padding_ms(),
            ));
        });
        match result.outcome {
            NativeSttOutcome::Usable(transcript) => {
                let language = canonical_native_stt_language(&transcript.language)
                    .map_err(DispatchError::Unusable)?;
                Ok(transcript
                    .segments
                    .into_iter()
                    .map(|s| DecodedSegment {
                        start_ms: s.start_ms,
                        end_ms: s.end_ms,
                        language: language.clone(),
                        text: s.text,
                        mean_token_probability: None,
                    })
                    .collect())
            }
            NativeSttOutcome::Unusable(reason) => Err(DispatchError::Unusable(reason).into()),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn transcribe_segment(
    engine: &dyn SttEngine,
    ledger: &WindowLedger,
    segment_index: u32,
    segment_start_frame: u64,
    sample_rate: u32,
    pcm: &[f32],
    gaps: &[crate::FrameRange],
    is_cancelled: &dyn Fn() -> bool,
) -> Result<SegmentResult, SegmentFailure> {
    let regions = engine
        .detect_speech(pcm)
        .map_err(|_| SegmentFailure::Failed("review_transcription_vad_failed"))?;
    let speech_ms: u32 = regions.iter().map(SpeechRegion::duration_ms).sum();
    if regions.is_empty() || speech_ms == 0 {
        return Ok(SegmentResult {
            speech_ms: 0,
            window_count: 0,
            utterances: vec![],
        });
    }
    let mut pending: Vec<PackedWindow> = pack_windows(&regions);
    pending.reverse();
    let mut window_index = 0_u32;
    let mut utterances = Vec::new();
    while let Some(window) = pending.pop() {
        if is_cancelled() {
            return Err(SegmentFailure::Cancelled);
        }
        let index = window_index;
        window_index += 1;
        let fingerprint = window_fingerprint(segment_index, &window);
        let decoded = match ledger.cached(segment_index, index, &fingerprint) {
            Ok(Some(decoded)) => decoded,
            Err(error) => {
                return Err(match error.downcast_ref::<DispatchError>() {
                    Some(DispatchError::Unusable(reason)) => {
                        SegmentFailure::Failed(provider_failure_code(*reason))
                    }
                    _ => SegmentFailure::Failed("review_transcription_store_failed"),
                });
            }
            Ok(None) => {
                if ledger.cloud {
                    match ledger.mark_dispatched(segment_index, index, &fingerprint) {
                        Ok(()) => {}
                        Err(_) => return Err(SegmentFailure::DispatchUnknown),
                    }
                }
                LAST_CLOUD_RECEIPT.with(|slot| *slot.borrow_mut() = None);
                match engine.decode_window(pcm, &window) {
                    Ok(decoded) => {
                        let receipt = LAST_CLOUD_RECEIPT.with(|slot| slot.borrow_mut().take());
                        let (cost, generation, provider, model, latency, input_padding_ms) =
                            match receipt {
                                Some((c, g, p, m, l, _, padding)) => (c, g, p, m, Some(l), padding),
                                None => (None, None, None, None, None, 0),
                            };
                        ledger
                            .settle(
                                segment_index,
                                index,
                                &fingerprint,
                                cost,
                                generation.as_deref(),
                                provider.as_deref(),
                                model.as_deref(),
                                latency,
                                &decoded,
                                input_padding_ms,
                            )
                            .map_err(|_| {
                                SegmentFailure::Failed("review_transcription_store_failed")
                            })?;
                        decoded
                    }
                    Err(error) => {
                        if ledger.cloud {
                            if let Some(DispatchError::Unusable(reason)) =
                                error.downcast_ref::<DispatchError>()
                            {
                                // The response arrived: preserve its receipt even when the
                                // timed transcript is invalid. Never cache it as decoded text.
                                let receipt =
                                    LAST_CLOUD_RECEIPT.with(|slot| slot.borrow_mut().take());
                                ledger
                                    .settle_unusable(segment_index, index, *reason, receipt)
                                    .map_err(|_| {
                                        SegmentFailure::Failed("review_transcription_store_failed")
                                    })?;
                                return Err(SegmentFailure::Failed(provider_failure_code(*reason)));
                            }
                            // Sent or possibly sent with no response: keep the reservation.
                            ledger.mark_unknown(segment_index, index);
                            return Err(SegmentFailure::DispatchUnknown);
                        }
                        return Err(SegmentFailure::Failed("review_transcription_decode_failed"));
                    }
                }
            }
        };
        if needs_resplit(&window, &decoded) {
            // A language's speech went missing in a mixed window: decode each
            // region alone (the same rule the clip pipeline uses).
            let mut narrow = resplit(&window);
            narrow.reverse();
            pending.extend(narrow);
            continue;
        }
        utterances.extend(to_utterances(
            &window,
            &decoded,
            segment_index,
            index,
            segment_start_frame,
            sample_rate,
            gaps,
        ));
    }
    utterances.sort_by(|a, b| {
        a.start_frame
            .cmp(&b.start_frame)
            .then(a.end_frame.cmp(&b.end_frame))
    });
    Ok(SegmentResult {
        speech_ms,
        window_count: window_index,
        utterances,
    })
}

fn to_utterances(
    window: &PackedWindow,
    decoded: &[DecodedSegment],
    segment_index: u32,
    window_index: u32,
    segment_start_frame: u64,
    sample_rate: u32,
    gaps: &[crate::FrameRange],
) -> Vec<ReviewUtterance> {
    let kept: Vec<&DecodedSegment> = decoded
        .iter()
        .filter(|s| !s.text.trim().is_empty())
        .collect();
    let owned: Vec<DecodedSegment> = kept.iter().map(|s| (*s).clone()).collect();
    let gate = reject_reason(&owned).filter(|r| *r != RejectReason::MissingProbabilityEvidence);
    let rate = f64::from(sample_rate);
    kept.iter()
        .enumerate()
        .filter_map(|(i, segment)| {
            let (start_ms, end_ms) = translate_span(window, segment.start_ms, segment.end_ms)?;
            if start_ms >= end_ms {
                return None;
            }
            let start_frame =
                segment_start_frame + (u64::from(start_ms) * u64::from(sample_rate)) / 1000;
            let end_frame =
                segment_start_frame + (u64::from(end_ms) * u64::from(sample_rate)) / 1000;
            let (script, display_text, projection) = script_and_display(&segment.text);
            let mut flags = Vec::new();
            if let Some(reason) = gate {
                flags.push(format!("qualityGate:{}", reason.as_str()));
            }
            if segment.mean_token_probability.is_none() {
                flags.push("probabilityEvidenceUnavailable".into());
            } else if segment.mean_token_probability.is_some_and(|p| p < 0.4) {
                flags.push("lowConfidence".into());
            }
            if segment.language.is_empty() {
                flags.push("languageUnknown".into());
            } else if !EXPECTED_LANGUAGES.contains(&segment.language.as_str()) {
                flags.push("unexpectedLanguage".into());
            }
            let near = (NEAR_GAP_SECONDS * rate) as u64;
            if gaps
                .iter()
                .any(|g| start_frame < g.end_frame + near && g.start_frame < end_frame + near)
            {
                flags.push("nearRecordingGap".into());
            }
            Some(ReviewUtterance {
                utterance_id: format!("s{segment_index}w{window_index}u{i}"),
                segment_index,
                window_index,
                start_frame,
                end_frame,
                start_seconds: start_frame as f64 / rate,
                end_seconds: end_frame as f64 / rate,
                text: segment.text.trim().to_string(),
                language: segment.language.clone(),
                script,
                display_text: display_text.trim().to_string(),
                display_projection: projection,
                speaker: "unknown".into(),
                speaker_evidence: "diarizationUnavailable".into(),
                mean_token_probability: segment.mean_token_probability,
                flags,
                suppressed: gate.is_some(),
            })
        })
        .collect()
}

fn load_run(
    connection: &Connection,
    project_id: &str,
    run_id: &str,
) -> Result<ReviewTranscriptionRun, ReviewTranscriptError> {
    let row = connection
        .query_row(
            "SELECT r.session_id, r.backend, r.provider, r.model, r.settings_json, r.audio_fingerprint, r.state, r.error_code,
                    r.attempt_count, r.job_id, r.created_at, r.updated_at, r.created_by_type, r.created_by_id,
                    r.authorized_max_micro_usd, s.review_target_id, s.revision_id, s.sample_rate
             FROM review_transcription_runs r JOIN review_sessions s ON s.session_id=r.session_id
             WHERE r.project_id=?1 AND r.run_id=?2",
            params![project_id, run_id],
            |r| {
                Ok((
                    (r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?, r.get::<_, String>(5)?),
                    (r.get::<_, String>(6)?, r.get::<_, Option<String>>(7)?, r.get::<_, u32>(8)?, r.get::<_, Option<String>>(9)?, r.get::<_, String>(10)?, r.get::<_, String>(11)?),
                    (r.get::<_, String>(12)?, r.get::<_, String>(13)?, r.get::<_, Option<i64>>(14)?, r.get::<_, String>(15)?, r.get::<_, String>(16)?, r.get::<_, u32>(17)?),
                ))
            },
        )
        .optional()?
        .ok_or(ReviewTranscriptError::NotFound)?;
    let (a, b, c) = row;
    let settings: ReviewTranscriptionRequest =
        serde_json::from_str(&a.4).context("decode settings")?;
    let sample_rate = c.5;
    let rate = f64::from(sample_rate);
    let mut statement = connection.prepare(
        "SELECT segment_index, start_frame, end_frame, state, COALESCE(speech_ms,0), COALESCE(window_count,0), utterances_json, error_code
         FROM review_transcription_segments WHERE run_id=?1 ORDER BY segment_index",
    )?;
    let mut segments = Vec::new();
    let mut utterances = Vec::new();
    let rows = statement
        .query_map([run_id], |r| {
            Ok((
                r.get::<_, u32>(0)?,
                r.get::<_, i64>(1)? as u64,
                r.get::<_, i64>(2)? as u64,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, u32>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (index, start, end, state, speech_ms, windows, json, error) in rows {
        if let Some(json) = json {
            let decoded: Vec<ReviewUtterance> =
                serde_json::from_str(&json).context("decode utterances")?;
            utterances.extend(decoded);
        }
        segments.push(ReviewTranscriptionSegment {
            segment_index: index,
            start_frame: start,
            end_frame: end,
            start_seconds: start as f64 / rate,
            end_seconds: end as f64 / rate,
            state,
            speech_seconds: speech_ms as f64 / 1000.0,
            window_count: windows,
            error_code: error,
        });
    }
    let spend = connection.query_row(
        "SELECT COALESCE(SUM(CASE WHEN (state IN ('dispatched','unknown','reserved') OR (state='settled' AND cost_known=0)) THEN reserved_micro_usd ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN state='settled' AND cost_known=1 THEN cost_micro_usd ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN state='unknown' OR (state='settled' AND cost_known=0) THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN state IN ('settled','unknown','dispatched') AND (SELECT backend FROM review_transcription_runs WHERE run_id=?1)='openrouter' THEN 1 ELSE 0 END),0),
                COUNT(*)
         FROM review_transcription_windows WHERE run_id=?1",
        [run_id],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, u32>(2)?, r.get::<_, u32>(3)?, r.get::<_, u32>(4)?)),
    )?;
    let audio = load_session_audio(connection, project_id, &a.0)?;
    let provider_errors = {
        let mut statement = connection.prepare("SELECT segment_index,window_index,generation_id,result_json FROM review_transcription_windows WHERE run_id=?1 AND result_json IS NOT NULL ORDER BY segment_index,window_index")?;
        let rows = statement.query_map([run_id], |r| {
            Ok((
                r.get::<_, u32>(0)?,
                r.get::<_, u32>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut errors = Vec::new();
        for row in rows {
            let (segment_index, window_index, generation_id, json) = row?;
            if let Ok(CachedWindow::Unusable {
                unusable,
                provider_error,
            }) = serde_json::from_str(&json)
            {
                let (http_status, code, message) = match provider_error {
                    Some(e) => (Some(e.http_status), e.code, e.message),
                    None => (
                        match unusable {
                            NativeSttProtocolError::HttpStatus(status) => Some(status),
                            _ => None,
                        },
                        None,
                        format!(
                            "OpenRouter response could not be used: {}.",
                            unusable.code()
                        ),
                    ),
                };
                errors.push(ReviewTranscriptionProviderError {
                    segment_index,
                    window_index,
                    protocol_code: unusable.code().into(),
                    http_status,
                    code,
                    message,
                    generation_id,
                });
            }
        }
        errors
    };
    Ok(ReviewTranscriptionRun {
        provider_errors,
        schema: REVIEW_TRANSCRIPT_SCHEMA,
        run_id: run_id.to_string(),
        project_id: project_id.to_string(),
        session_id: a.0,
        review_target_id: c.3,
        revision_id: c.4,
        backend: settings.backend,
        provider: a.2,
        model: a.3,
        recipe: REVIEW_TRANSCRIBE_RECIPE.into(),
        settings,
        audio_fingerprint: a.5,
        sample_rate,
        state: ReviewTranscriptionState::parse(&b.0),
        error_code: b.1,
        attempt_count: b.2,
        job_id: b.3,
        created_at: b.4,
        updated_at: b.5,
        created_by: ReviewTargetActor {
            actor_type: c.0,
            actor_id: c.1,
        },
        segments,
        utterances,
        recording_gaps: audio.gaps(),
        spend: ReviewTranscriptionSpend {
            authorized_max_usd: c.2.map(usd),
            reserved_usd: usd(spend.0),
            known_cost_usd: usd(spend.1),
            unknown_cost_windows: spend.2,
            dispatched_windows: spend.3,
            cached_windows: spend.4,
        },
        diarization: ReviewDiarization {
            status: "unavailable",
            reason: "No diarization backend is configured. Speaker labels come only from explicit human correction.",
        },
        limitations: REVIEW_TRANSCRIPT_LIMITATIONS,
    })
}

/// Server job handler: executes one run per job.
#[derive(Clone)]
pub struct ReviewTranscriptionJobHandler {
    transcripts: ReviewTranscripts,
    runtime: Arc<dyn ReviewSpeechRuntime>,
    jobs: JobEngine,
}

impl ReviewTranscriptionJobHandler {
    pub fn new(
        transcripts: ReviewTranscripts,
        runtime: Arc<dyn ReviewSpeechRuntime>,
        jobs: JobEngine,
    ) -> Self {
        Self {
            transcripts,
            runtime,
            jobs,
        }
    }

    pub fn run(
        &self,
        job: ClaimedJob,
        cancellation: CancellationToken,
    ) -> Result<JobResultCommit, JobHandlerFailure> {
        let run_id = job
            .job
            .payload
            .get("runId")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let jobs = self.jobs.clone();
        let claimed = job.clone();
        let cancelled = move || {
            cancellation.is_cancelled() || jobs.cancellation_requested(&claimed).unwrap_or(false)
        };
        match self
            .transcripts
            .execute(&run_id, self.runtime.as_ref(), &cancelled)
        {
            Ok(state) => Ok(JobResultCommit {
                output_identity: Some(run_id),
                details: serde_json::json!({"state": state}),
            }),
            Err(error) => Err(JobHandlerFailure {
                kind: FailureKind::Terminal,
                code: error.code().to_string(),
                details: serde_json::json!({"runId": run_id}),
            }),
        }
    }
}

pub(crate) fn migrate(tx: &rusqlite::Transaction<'_>) -> anyhow::Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE review_transcription_runs (
            run_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES project_registry(project_id),
            session_id TEXT NOT NULL REFERENCES review_sessions(session_id),
            backend TEXT NOT NULL CHECK(backend IN ('local','openrouter')),
            provider TEXT NOT NULL,
            model TEXT NOT NULL,
            settings_json TEXT NOT NULL CHECK(length(settings_json) <= 4096),
            idempotency_key TEXT NOT NULL UNIQUE,
            audio_fingerprint TEXT NOT NULL,
            state TEXT NOT NULL CHECK(state IN ('queued','running','completed','noSpeech','partial','failed','cancelled')),
            authorized_max_micro_usd INTEGER CHECK(authorized_max_micro_usd IS NULL OR authorized_max_micro_usd > 0),
            attempt_count INTEGER NOT NULL DEFAULT 1,
            job_id TEXT,
            error_code TEXT,
            cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK(cancel_requested IN (0,1)),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            created_by_type TEXT NOT NULL,
            created_by_id TEXT NOT NULL
        );
        CREATE INDEX review_transcription_runs_session ON review_transcription_runs(project_id, session_id);
        CREATE TABLE review_transcription_segments (
            run_id TEXT NOT NULL REFERENCES review_transcription_runs(run_id),
            segment_index INTEGER NOT NULL,
            start_frame INTEGER NOT NULL,
            end_frame INTEGER NOT NULL CHECK(end_frame > start_frame),
            state TEXT NOT NULL CHECK(state IN ('pending','done','noSpeech','failed')),
            speech_ms INTEGER,
            window_count INTEGER,
            utterances_json TEXT,
            error_code TEXT,
            PRIMARY KEY(run_id, segment_index)
        );
        CREATE TABLE review_transcription_windows (
            run_id TEXT NOT NULL REFERENCES review_transcription_runs(run_id),
            segment_index INTEGER NOT NULL,
            window_index INTEGER NOT NULL,
            window_fingerprint TEXT NOT NULL,
            state TEXT NOT NULL CHECK(state IN ('reserved','dispatched','settled','unknown','released')),
            reserved_micro_usd INTEGER NOT NULL DEFAULT 0 CHECK(reserved_micro_usd >= 0),
            cost_known INTEGER NOT NULL DEFAULT 0,
            cost_micro_usd INTEGER,
            generation_id TEXT,
            actual_provider TEXT,
            actual_model TEXT,
            latency_ms INTEGER,
            result_json TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY(run_id, segment_index, window_index)
        );
        "#,
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "review_transcripts_tests.rs"]
pub(crate) mod tests;
