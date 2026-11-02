//! Grouped editorial discussions over a review-session transcript (#225).
//!
//! This is a *review workspace*, not a feedback owner. It combines:
//! - machine utterances from a transcription run (#224), never rewritten;
//! - append-only human corrections of text, speaker, language and movie time;
//! - a movie-time estimate from the session clock to the verified movie
//!   (`SessionClockMapping`; today only the manual-cue adapter, to be replaced
//!   by Codex's shared #223 engine);
//! - picture candidates from the shared Rust review-target lookup (#221).
//!
//! Discussions group utterances. Items inside them keep the difference between
//! an observation, a candidate edit, a speaker's claim, a retraction, a note and
//! an explicit human decision. Agent-authored items are always provisional and
//! can never be decisions. Exported feedback is a set of core-compatible
//! `RecordReviewNoteRequest`s for the owning catalog's existing
//! `edit-plan feedback record` command, with persistent reconciliation receipts;
//! nothing here writes canonical feedback, a plan or a project catalog.
use crate::{
    ReviewTargetActor, ReviewTargetHandoff, ReviewTranscriptionRun, ReviewUtterance,
    open_read_only_connection, open_writable_connection,
};
use anyhow::Context;
use chrono::{SecondsFormat, Utc};
use rcdvlog_core::{
    RecordReviewNoteOnceRequest, RecordReviewNoteRequest, ReviewAuthor, ReviewCategory,
    ReviewChange,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

pub const REVIEW_REPORT_SCHEMA: &str = "review-report.v1";
/// Human reaction time to a first frame is not measured; this is the stated
/// uncertainty of a manual cue, widened by unknown clock drift over time.
const MANUAL_CUE_UNCERTAINTY_SECONDS: f64 = 0.75;
const DRIFT_PER_MINUTE_SECONDS: f64 = 0.05;
const HEURISTIC_BREAK_SECONDS: f64 = 6.0;
const MAX_TEXT: usize = 4096;
const MAX_DISCUSSION_UTTERANCES: usize = 500;

#[derive(Debug, Error)]
pub enum ReviewDiscussionError {
    #[error("project is not registered on this server")]
    ProjectNotFound,
    #[error("not found")]
    NotFound,
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("conflict: {0}")]
    Conflict(&'static str),
    #[error("only a signed-in human reviewer may record this")]
    HumanRequired,
    #[error(transparent)]
    Transcript(#[from] crate::ReviewTranscriptError),
    #[error(transparent)]
    Infrastructure(#[from] anyhow::Error),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
}

impl ReviewDiscussionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ProjectNotFound => "project_replica_not_found",
            Self::NotFound => "not_found",
            Self::InvalidInput(_) => "invalid_input",
            Self::Conflict(code) => code,
            Self::HumanRequired => "review_human_required",
            Self::Transcript(error) => error.code(),
            Self::Infrastructure(_) | Self::Database(_) => "internal_error",
        }
    }
}

fn invalid(message: impl Into<String>) -> ReviewDiscussionError {
    ReviewDiscussionError::InvalidInput(message.into())
}

fn now_text() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn bounded_text(value: &str) -> Result<(), ReviewDiscussionError> {
    if value.trim().is_empty() || value.len() > MAX_TEXT {
        return Err(invalid("text must contain 1-4096 bytes"));
    }
    Ok(())
}

/// Who is acting. Browser sessions are people; device credentials may be
/// agents and can never assert human authorship.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthorKind {
    Human,
    Agent,
}

#[derive(Debug, Clone)]
pub struct ReviewActor {
    pub actor: ReviewTargetActor,
    /// True only for an authenticated browser session.
    pub may_assert_human: bool,
}

// ── Session clock → movie time ──────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MovieTimeEstimate {
    pub movie_seconds: f64,
    pub uncertainty_seconds: f64,
    /// `manualCue` (reviewer declaration), `humanCorrection`; later `sharedSync` (#223).
    pub source: String,
    pub anchor_event_id: Option<String>,
}

/// Seam for the shared synchronizer. Implementations map recording seconds to
/// movie seconds with an honest uncertainty, or return `None` (unanchored).
pub trait SessionClockMapping {
    fn estimate(&self, recording_seconds: f64) -> Option<MovieTimeEstimate>;
    fn description(&self) -> SyncDescription;
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncDescription {
    pub status: &'static str,
    pub source: &'static str,
    pub note: &'static str,
}

/// Interim adapter: the latest manual cue declares "recording t₀ shows movie m₀",
/// assuming 1:1 clock rate. Replace with the shared #223 mapper; do not extend.
pub struct ManualCueMapping {
    cue: Option<(f64, f64, String)>,
    movie_duration: f64,
}

impl ManualCueMapping {
    pub fn from_session(
        connection: &Connection,
        session_id: &str,
        movie_duration: f64,
    ) -> anyhow::Result<Self> {
        let cue = connection
            .query_row(
                "SELECT e.event_id, e.audio_frame, e.declared_movie_seconds, s.sample_rate
                 FROM review_session_events e JOIN review_sessions s ON s.session_id = e.session_id
                 WHERE e.session_id=?1 AND e.kind='manualSyncCue' AND e.declared_movie_seconds IS NOT NULL
                 ORDER BY e.sequence DESC LIMIT 1",
                [session_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, f64>(2)?, r.get::<_, u32>(3)?)),
            )
            .optional()?
            .map(|(id, frame, movie, rate)| (frame as f64 / f64::from(rate), movie, id));
        Ok(Self {
            cue,
            movie_duration,
        })
    }
}

impl SessionClockMapping for ManualCueMapping {
    fn estimate(&self, recording_seconds: f64) -> Option<MovieTimeEstimate> {
        let (cue_recording, cue_movie, id) = self.cue.as_ref()?;
        let movie = recording_seconds - cue_recording + cue_movie;
        if !(0.0..=self.movie_duration).contains(&movie) {
            return None;
        }
        let elapsed_minutes = (recording_seconds - cue_recording).abs() / 60.0;
        Some(MovieTimeEstimate {
            movie_seconds: movie,
            uncertainty_seconds: MANUAL_CUE_UNCERTAINTY_SECONDS
                + DRIFT_PER_MINUTE_SECONDS * elapsed_minutes,
            source: "manualCue".into(),
            anchor_event_id: Some(id.clone()),
        })
    }
    fn description(&self) -> SyncDescription {
        match self.cue {
            Some(_) => SyncDescription {
                status: "provisional",
                source: "manualCue",
                note: "Movie time is estimated from your manual cue, assuming the TV played without pauses. It is not measured synchronization; precise cut assignment needs confirmation.",
            },
            None => SyncDescription {
                status: "unanchored",
                source: "none",
                note: "No manual cue and no shared synchronization yet: remarks are on the recording clock only and cannot be assigned to cuts by time.",
            },
        }
    }
}

// ── Workspace DTOs ──────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UtteranceCorrection {
    pub correction_id: String,
    pub field: String,
    pub value: serde_json::Value,
    pub reason: Option<String>,
    pub actor: ReviewTargetActor,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceUtterance {
    pub machine: ReviewUtterance,
    pub text: String,
    pub display_text: String,
    pub speaker: String,
    pub language: String,
    pub excluded: bool,
    pub movie_time: Option<MovieTimeEstimate>,
    /// Picture locators overlapping the estimate's uncertainty; never chosen automatically.
    pub candidate_operation_ids: Vec<String>,
    /// `previous` / `next` when the speaker appears to refer to another shot.
    pub reference_hint: Option<String>,
    pub corrections: Vec<UtteranceCorrection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemKind {
    Observation,
    CandidateEdit,
    SpeakerClaim,
    Retraction,
    Note,
    Decision,
}

impl ItemKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Observation => "observation",
            Self::CandidateEdit => "candidateEdit",
            Self::SpeakerClaim => "speakerClaim",
            Self::Retraction => "retraction",
            Self::Note => "note",
            Self::Decision => "decision",
        }
    }
    fn parse(value: &str) -> Self {
        match value {
            "candidateEdit" => Self::CandidateEdit,
            "speakerClaim" => Self::SpeakerClaim,
            "retraction" => Self::Retraction,
            "note" => Self::Note,
            "decision" => Self::Decision,
            _ => Self::Observation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DecisionAction {
    Accepted,
    Declined,
    Deferred,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewDiscussionItem {
    pub kind: ItemKind,
    pub author_kind: AuthorKind,
    pub text: String,
    #[serde(default)]
    pub speaker: Option<String>,
    #[serde(default)]
    pub quote_utterance_ids: Vec<String>,
    #[serde(default)]
    pub category: Option<ReviewCategory>,
    /// Typed change for a candidate edit, validated against the core type.
    #[serde(default)]
    pub change: Option<serde_json::Value>,
    #[serde(default)]
    pub decision: Option<DecisionAction>,
    #[serde(default)]
    pub refers_to_item_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscussionItem {
    pub item_id: String,
    pub kind: ItemKind,
    pub author_kind: AuthorKind,
    pub provisional: bool,
    pub actor: ReviewTargetActor,
    pub text: String,
    pub speaker: Option<String>,
    pub quote_utterance_ids: Vec<String>,
    pub category: Option<ReviewCategory>,
    pub change: Option<serde_json::Value>,
    pub decision: Option<DecisionAction>,
    pub refers_to_item_id: Option<String>,
    /// Derived: `open`, `accepted`, `declined`, `deferred`, `retracted`.
    pub status: String,
    pub created_at: String,
    pub canonical_note_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Discussion {
    pub discussion_id: String,
    pub title: String,
    pub grouping_source: String,
    pub state: String,
    pub utterance_ids: Vec<String>,
    pub start_recording_seconds: Option<f64>,
    pub end_recording_seconds: Option<f64>,
    pub languages: Vec<String>,
    pub speakers: Vec<String>,
    pub candidate_operation_ids: Vec<String>,
    pub target_operation_id: Option<String>,
    pub target_source: Option<String>,
    pub unanchored: bool,
    pub items: Vec<DiscussionItem>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewWorkspace {
    pub run_id: String,
    pub session_id: String,
    pub project_id: String,
    pub review_target_id: String,
    pub revision_id: String,
    pub viewed_artifact_id: String,
    pub target_digest: String,
    pub sync: SyncDescription,
    pub cuts: Vec<WorkspaceCut>,
    pub utterances: Vec<WorkspaceUtterance>,
    pub discussions: Vec<Discussion>,
    pub coverage: CoverageStatement,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceCut {
    pub operation_id: String,
    pub position: u32,
    pub asset_id: String,
    pub movie_in_seconds: f64,
    pub movie_out_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageStatement {
    pub audiovisual_review_attested: bool,
    pub note: &'static str,
}

const COVERAGE: CoverageStatement = CoverageStatement {
    audiovisual_review_attested: false,
    note: "A recording made while the TV played does not attest continuous human picture or listening review.",
};

#[derive(Clone)]
pub struct ReviewDiscussions {
    database_path: PathBuf,
}

/// Inputs shared by the workspace and the report.
struct Context_ {
    run: ReviewTranscriptionRun,
    handoff: ReviewTargetHandoff,
    mapping: ManualCueMapping,
}

fn reference_hint(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let previous = [
        "previous shot",
        "last shot",
        "shot before",
        "earlier shot",
        "上一個",
        "上一个",
        "前一個",
        "前一个",
        "剛才那個",
        "刚才那个",
    ];
    let next = [
        "next shot",
        "following shot",
        "shot after",
        "下一個",
        "下一个",
        "後一個",
        "后一个",
    ];
    if previous.iter().any(|p| lower.contains(p)) {
        Some("previous".into())
    } else if next.iter().any(|p| lower.contains(p)) {
        Some("next".into())
    } else {
        None
    }
}

const SPEAKERS: &[&str] = &["A", "B", "unknown", "overlap", "tv", "background"];

impl ReviewDiscussions {
    pub(crate) fn new(database_path: PathBuf) -> Self {
        Self { database_path }
    }

    fn project_registered(
        connection: &Connection,
        project_id: &str,
    ) -> Result<(), ReviewDiscussionError> {
        connection
            .query_row(
                "SELECT 1 FROM project_registry WHERE project_id=?1 AND state='registered'",
                [project_id],
                |_| Ok(()),
            )
            .optional()?
            .ok_or(ReviewDiscussionError::ProjectNotFound)
    }

    fn context(
        &self,
        connection: &Connection,
        project_id: &str,
        run_id: &str,
    ) -> Result<Context_, ReviewDiscussionError> {
        Self::project_registered(connection, project_id)?;
        let run =
            crate::ReviewTranscripts::new(self.database_path.clone()).get(project_id, run_id)?;
        let handoff = crate::ReviewTargets::new(self.database_path.clone())
            .handoff(project_id, &run.review_target_id)
            .map_err(|_| ReviewDiscussionError::NotFound)?;
        let mapping = ManualCueMapping::from_session(
            connection,
            &run.session_id,
            handoff.target.movie.duration_seconds,
        )?;
        Ok(Context_ {
            run,
            handoff,
            mapping,
        })
    }

    fn corrections(
        connection: &Connection,
        run_id: &str,
    ) -> Result<BTreeMap<String, Vec<UtteranceCorrection>>, ReviewDiscussionError> {
        let mut statement = connection.prepare(
            "SELECT correction_id, utterance_id, field, value_json, reason, actor_type, actor_id, created_at
             FROM review_utterance_corrections WHERE run_id=?1 ORDER BY sequence",
        )?;
        let rows = statement
            .query_map([run_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out: BTreeMap<String, Vec<UtteranceCorrection>> = BTreeMap::new();
        for (id, utterance, field, value, reason, actor_type, actor_id, at) in rows {
            out.entry(utterance).or_default().push(UtteranceCorrection {
                correction_id: id,
                field,
                value: serde_json::from_str(&value).context("decode correction")?,
                reason,
                actor: ReviewTargetActor {
                    actor_type,
                    actor_id,
                },
                created_at: at,
            });
        }
        Ok(out)
    }

    fn utterances(
        &self,
        connection: &Connection,
        context: &Context_,
    ) -> Result<Vec<WorkspaceUtterance>, ReviewDiscussionError> {
        let corrections = Self::corrections(connection, &context.run.run_id)?;
        let mut out = Vec::new();
        for machine in &context.run.utterances {
            let history = corrections
                .get(&machine.utterance_id)
                .cloned()
                .unwrap_or_default();
            let latest = |field: &str| {
                history
                    .iter()
                    .rev()
                    .find(|c| c.field == field)
                    .map(|c| c.value.clone())
            };
            let text = latest("text")
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| machine.text.clone());
            let display_text = if latest("text").is_some() {
                crate::script_and_display(&text).1
            } else {
                machine.display_text.clone()
            };
            let movie_time = match latest("movieTime") {
                Some(serde_json::Value::Null) => None,
                Some(value) => value.as_f64().map(|seconds| MovieTimeEstimate {
                    movie_seconds: seconds,
                    uncertainty_seconds: 0.0,
                    source: "humanCorrection".into(),
                    anchor_event_id: None,
                }),
                None => {
                    let middle = (machine.start_seconds + machine.end_seconds) / 2.0;
                    context.mapping.estimate(middle)
                }
            };
            let duration = context.handoff.target.movie.duration_seconds;
            let mut candidates = match &movie_time {
                Some(estimate) => {
                    let start = (estimate.movie_seconds - estimate.uncertainty_seconds)
                        .clamp(0.0, duration);
                    let end = (estimate.movie_seconds + estimate.uncertainty_seconds)
                        .clamp(0.0, duration);
                    context
                        .handoff
                        .picture_candidates(start, end)
                        .unwrap_or_default()
                }
                None => Vec::new(),
            };
            let hint = reference_hint(&text);
            if let (Some(hint), Some(first)) = (&hint, candidates.first().cloned()) {
                // Add the neighbour the speaker seems to mean; still only a candidate.
                let cuts = &context.handoff.target.cuts;
                if let Some(index) = cuts.iter().position(|c| c.operation_id == first) {
                    let neighbour = if hint == "previous" {
                        index.checked_sub(1)
                    } else {
                        Some(index + 1)
                    };
                    if let Some(cut) = neighbour.and_then(|i| cuts.get(i))
                        && !candidates.contains(&cut.operation_id)
                    {
                        candidates.insert(0, cut.operation_id.clone());
                    }
                }
            }
            out.push(WorkspaceUtterance {
                text,
                display_text,
                speaker: latest("speaker")
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| machine.speaker.clone()),
                language: latest("language")
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| machine.language.clone()),
                excluded: latest("excluded")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(machine.suppressed),
                movie_time,
                candidate_operation_ids: candidates,
                reference_hint: hint,
                corrections: history,
                machine: machine.clone(),
            });
        }
        Ok(out)
    }

    pub fn workspace(
        &self,
        project_id: &str,
        run_id: &str,
    ) -> Result<ReviewWorkspace, ReviewDiscussionError> {
        let connection = open_read_only_connection(&self.database_path)?;
        let context = self.context(&connection, project_id, run_id)?;
        let utterances = self.utterances(&connection, &context)?;
        let discussions = load_discussions(&connection, run_id, &utterances)?;
        let movie = &context.handoff.target.movie;
        let seconds = |pts: i64| {
            (pts - movie.origin_pts) as f64 * movie.time_base_numerator as f64
                / movie.time_base_denominator as f64
        };
        Ok(ReviewWorkspace {
            run_id: run_id.into(),
            session_id: context.run.session_id.clone(),
            project_id: project_id.into(),
            review_target_id: context.run.review_target_id.clone(),
            revision_id: context.handoff.target.revision_id.clone(),
            viewed_artifact_id: context.handoff.target.viewed_artifact_id.clone(),
            target_digest: context.handoff.target_digest.clone(),
            sync: context.mapping.description(),
            cuts: context
                .handoff
                .target
                .cuts
                .iter()
                .map(|c| WorkspaceCut {
                    operation_id: c.operation_id.clone(),
                    position: c.position,
                    asset_id: c.asset_id.clone(),
                    movie_in_seconds: seconds(c.movie_in_pts),
                    movie_out_seconds: seconds(c.movie_out_pts),
                })
                .collect(),
            utterances,
            discussions,
            coverage: COVERAGE,
        })
    }

    /// Append a correction; the machine utterance is never modified.
    #[allow(clippy::too_many_arguments)]
    pub fn correct_utterance(
        &self,
        project_id: &str,
        run_id: &str,
        utterance_id: &str,
        field: &str,
        value: serde_json::Value,
        reason: Option<String>,
        actor: &ReviewActor,
    ) -> Result<ReviewWorkspace, ReviewDiscussionError> {
        {
            let connection = open_read_only_connection(&self.database_path)?;
            let context = self.context(&connection, project_id, run_id)?;
            if !context
                .run
                .utterances
                .iter()
                .any(|u| u.utterance_id == utterance_id)
            {
                return Err(ReviewDiscussionError::NotFound);
            }
            let duration = context.handoff.target.movie.duration_seconds;
            let valid = match field {
                "text" => value
                    .as_str()
                    .is_some_and(|t| !t.trim().is_empty() && t.len() <= MAX_TEXT),
                "speaker" => value.as_str().is_some_and(|s| SPEAKERS.contains(&s)),
                "language" => value.as_str().is_some_and(|s| {
                    s.len() <= 8 && s.chars().all(|c| c.is_ascii_alphabetic() || c == '-')
                }),
                "movieTime" => {
                    value.is_null()
                        || value
                            .as_f64()
                            .is_some_and(|s| s.is_finite() && (0.0..=duration).contains(&s))
                }
                "excluded" => value.is_boolean(),
                _ => false,
            };
            if !valid {
                return Err(invalid("unsupported correction field or value"));
            }
            if field == "speaker" && !actor.may_assert_human {
                return Err(ReviewDiscussionError::HumanRequired);
            }
        }
        if reason.as_ref().is_some_and(|r| r.len() > 1024) {
            return Err(invalid("reason too long"));
        }
        let connection = open_writable_connection(&self.database_path)?;
        connection.execute(
            "INSERT INTO review_utterance_corrections(correction_id, run_id, utterance_id, field, value_json, reason, actor_type, actor_id, created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                format!("rcorr_{}", Uuid::new_v4().simple()),
                run_id,
                utterance_id,
                field,
                value.to_string(),
                reason,
                actor.actor.actor_type,
                actor.actor.actor_id,
                now_text(),
            ],
        )?;
        drop(connection);
        self.workspace(project_id, run_id)
    }

    /// Group utterances into provisional discussions by silence between them.
    /// Idempotent: does nothing when the run already has discussions.
    pub fn propose(
        &self,
        project_id: &str,
        run_id: &str,
        actor: &ReviewActor,
    ) -> Result<ReviewWorkspace, ReviewDiscussionError> {
        let workspace = self.workspace(project_id, run_id)?;
        if !workspace.discussions.is_empty() {
            return Ok(workspace);
        }
        let mut groups: Vec<Vec<&WorkspaceUtterance>> = Vec::new();
        for utterance in workspace.utterances.iter().filter(|u| !u.excluded) {
            match groups.last_mut() {
                Some(group)
                    if utterance.machine.start_seconds
                        - group.last().unwrap().machine.end_seconds
                        <= HEURISTIC_BREAK_SECONDS =>
                {
                    group.push(utterance)
                }
                _ => groups.push(vec![utterance]),
            }
        }
        let mut connection = open_writable_connection(&self.database_path)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: i64 = tx.query_row(
            "SELECT COUNT(*) FROM review_discussions WHERE run_id=?1",
            [run_id],
            |r| r.get(0),
        )?;
        if existing == 0 {
            for group in groups {
                let title: String = group[0].display_text.chars().take(60).collect();
                insert_discussion(
                    &tx,
                    project_id,
                    run_id,
                    &title,
                    "heuristic",
                    &group
                        .iter()
                        .map(|u| u.machine.utterance_id.clone())
                        .collect::<Vec<_>>(),
                    actor,
                )?;
            }
        }
        tx.commit()?;
        self.workspace(project_id, run_id)
    }

    pub fn create(
        &self,
        project_id: &str,
        run_id: &str,
        title: &str,
        utterance_ids: &[String],
        actor: &ReviewActor,
    ) -> Result<ReviewWorkspace, ReviewDiscussionError> {
        bounded_text(title)?;
        let workspace = self.workspace(project_id, run_id)?;
        validate_utterances(&workspace, utterance_ids)?;
        let connection = open_writable_connection(&self.database_path)?;
        insert_discussion(
            &connection,
            project_id,
            run_id,
            title,
            "human",
            utterance_ids,
            actor,
        )?;
        drop(connection);
        self.workspace(project_id, run_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &self,
        project_id: &str,
        discussion_id: &str,
        title: Option<String>,
        utterance_ids: Option<Vec<String>>,
        target_operation_id: Option<Option<String>>,
        unanchored: Option<bool>,
        state: Option<String>,
        actor: &ReviewActor,
    ) -> Result<ReviewWorkspace, ReviewDiscussionError> {
        let run_id = discussion_run(&self.database_path, project_id, discussion_id)?;
        let workspace = self.workspace(project_id, &run_id)?;
        if let Some(title) = &title {
            bounded_text(title)?;
        }
        if let Some(ids) = &utterance_ids {
            validate_utterances(&workspace, ids)?;
        }
        if let Some(Some(op)) = &target_operation_id {
            if !actor.may_assert_human {
                return Err(ReviewDiscussionError::HumanRequired);
            }
            if !workspace.cuts.iter().any(|c| &c.operation_id == op) {
                return Err(invalid(
                    "target must be a cut of this review target's revision",
                ));
            }
        }
        if let Some(state) = &state
            && !matches!(state.as_str(), "open" | "resolved" | "deferred")
        {
            return Err(invalid("state must be open, resolved or deferred"));
        }
        let connection = open_writable_connection(&self.database_path)?;
        if let Some(title) = title {
            connection.execute(
                "UPDATE review_discussions SET title=?2 WHERE discussion_id=?1",
                params![discussion_id, title],
            )?;
        }
        if let Some(ids) = utterance_ids {
            connection.execute(
                "UPDATE review_discussions SET utterance_ids_json=?2, grouping_source='human' WHERE discussion_id=?1",
                params![discussion_id, serde_json::to_string(&ids).context("ids")?],
            )?;
        }
        if let Some(target) = target_operation_id {
            connection.execute(
                "UPDATE review_discussions SET target_operation_id=?2, target_source=CASE WHEN ?2 IS NULL THEN NULL ELSE 'human' END, unanchored=CASE WHEN ?2 IS NULL THEN unanchored ELSE 0 END WHERE discussion_id=?1",
                params![discussion_id, target],
            )?;
        }
        if let Some(unanchored) = unanchored {
            connection.execute(
                "UPDATE review_discussions SET unanchored=?2, target_operation_id=CASE WHEN ?2 THEN NULL ELSE target_operation_id END, target_source=CASE WHEN ?2 THEN NULL ELSE target_source END WHERE discussion_id=?1",
                params![discussion_id, unanchored],
            )?;
        }
        if let Some(state) = state {
            connection.execute(
                "UPDATE review_discussions SET state=?2 WHERE discussion_id=?1",
                params![discussion_id, state],
            )?;
        }
        connection.execute(
            "UPDATE review_discussions SET updated_at=?2, updated_by_type=?3, updated_by_id=?4 WHERE discussion_id=?1",
            params![discussion_id, now_text(), actor.actor.actor_type, actor.actor.actor_id],
        )?;
        drop(connection);
        self.workspace(project_id, &run_id)
    }

    /// Append an item. Decisions are human-only and checked for consistency.
    pub fn add_item(
        &self,
        project_id: &str,
        discussion_id: &str,
        item: &NewDiscussionItem,
        actor: &ReviewActor,
    ) -> Result<ReviewWorkspace, ReviewDiscussionError> {
        bounded_text(&item.text)?;
        if item.author_kind == AuthorKind::Human && !actor.may_assert_human {
            return Err(ReviewDiscussionError::HumanRequired);
        }
        if item.kind == ItemKind::Decision && item.author_kind != AuthorKind::Human {
            return Err(ReviewDiscussionError::HumanRequired);
        }
        if let Some(speaker) = &item.speaker
            && !SPEAKERS.contains(&speaker.as_str())
        {
            return Err(invalid(
                "speaker must be A, B, unknown, overlap, tv or background",
            ));
        }
        if item.quote_utterance_ids.len() > 32 {
            return Err(invalid("at most 32 quoted utterances"));
        }
        let change = match (&item.kind, &item.change) {
            (ItemKind::CandidateEdit, Some(value)) => {
                serde_json::from_value::<ReviewChange>(value.clone())
                    .map_err(|e| invalid(format!("change: {e}")))?;
                Some(value.clone())
            }
            (_, Some(_)) => return Err(invalid("only a candidate edit carries a typed change")),
            (_, None) => None,
        };
        let run_id = discussion_run(&self.database_path, project_id, discussion_id)?;
        let workspace = self.workspace(project_id, &run_id)?;
        validate_utterances(&workspace, &item.quote_utterance_ids)?;
        let discussion = workspace
            .discussions
            .iter()
            .find(|d| d.discussion_id == discussion_id)
            .ok_or(ReviewDiscussionError::NotFound)?;
        let referred = match &item.refers_to_item_id {
            Some(id) => Some(
                discussion
                    .items
                    .iter()
                    .find(|i| &i.item_id == id)
                    .ok_or_else(|| invalid("refersToItemId must be an item in this discussion"))?,
            ),
            None => None,
        };
        match item.kind {
            ItemKind::Decision => {
                let decision = item
                    .decision
                    .ok_or_else(|| invalid("a decision needs accepted, declined or deferred"))?;
                let target = referred.ok_or_else(|| {
                    invalid("a decision refers to an observation or candidate edit")
                })?;
                if !matches!(target.kind, ItemKind::CandidateEdit | ItemKind::Observation) {
                    return Err(invalid(
                        "decisions apply to observations or candidate edits",
                    ));
                }
                if target.status == "retracted" && decision == DecisionAction::Accepted {
                    return Err(ReviewDiscussionError::Conflict("review_item_retracted"));
                }
                if decision == DecisionAction::Accepted && target.kind == ItemKind::CandidateEdit {
                    if discussion.target_operation_id.is_none() {
                        return Err(ReviewDiscussionError::Conflict("review_target_unconfirmed"));
                    }
                    if discussion.items.iter().any(|i| {
                        i.kind == ItemKind::CandidateEdit
                            && i.item_id != target.item_id
                            && i.status == "accepted"
                    }) {
                        return Err(ReviewDiscussionError::Conflict(
                            "review_conflicting_acceptance",
                        ));
                    }
                }
            }
            ItemKind::Retraction => {
                let target = referred
                    .ok_or_else(|| invalid("a retraction refers to the item it withdraws"))?;
                if !matches!(
                    target.kind,
                    ItemKind::CandidateEdit | ItemKind::Observation | ItemKind::SpeakerClaim
                ) {
                    return Err(invalid(
                        "only observations, claims or candidate edits can be retracted",
                    ));
                }
            }
            _ => {
                if item.decision.is_some() {
                    return Err(invalid("only a decision carries an action"));
                }
            }
        }
        let connection = open_writable_connection(&self.database_path)?;
        connection.execute(
            "INSERT INTO review_discussion_items(item_id, discussion_id, kind, author_kind, actor_type, actor_id, text, speaker,
                quote_utterance_ids_json, category, change_json, decision, refers_to_item_id, created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                format!("ritem_{}", Uuid::new_v4().simple()),
                discussion_id,
                item.kind.as_str(),
                match item.author_kind { AuthorKind::Human => "human", AuthorKind::Agent => "agent" },
                actor.actor.actor_type,
                actor.actor.actor_id,
                item.text,
                item.speaker,
                serde_json::to_string(&item.quote_utterance_ids).context("ids")?,
                item.category.and_then(|c| serde_json::to_value(c).ok().and_then(|v| v.as_str().map(str::to_string))),
                change.map(|c| c.to_string()),
                item.decision.map(|d| match d { DecisionAction::Accepted => "accepted", DecisionAction::Declined => "declined", DecisionAction::Deferred => "deferred" }),
                item.refers_to_item_id,
                now_text(),
            ],
        )?;
        connection.execute(
            "UPDATE review_discussions SET updated_at=?2 WHERE discussion_id=?1",
            params![discussion_id, now_text()],
        )?;
        drop(connection);
        self.workspace(project_id, &run_id)
    }

    /// Record that the owning catalog accepted this item's exported note.
    /// One receipt per item: a different note ID is a conflict, so a retry
    /// after a lost response cannot silently create a duplicate mapping.
    pub fn reconcile(
        &self,
        project_id: &str,
        item_id: &str,
        canonical_note_id: &str,
    ) -> Result<DiscussionItem, ReviewDiscussionError> {
        if canonical_note_id.is_empty()
            || canonical_note_id.len() > 255
            || canonical_note_id
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(invalid("canonical note id"));
        }
        let connection = open_writable_connection(&self.database_path)?;
        Self::project_registered(&connection, project_id)?;
        let (discussion_id, existing): (String, Option<String>) = connection
            .query_row(
                "SELECT i.discussion_id, i.canonical_note_id FROM review_discussion_items i JOIN review_discussions d ON d.discussion_id=i.discussion_id
                 WHERE i.item_id=?1 AND d.project_id=?2",
                params![item_id, project_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(ReviewDiscussionError::NotFound)?;
        match existing {
            Some(existing) if existing != canonical_note_id => {
                return Err(ReviewDiscussionError::Conflict(
                    "review_reconciliation_conflict",
                ));
            }
            Some(_) => {}
            None => {
                connection.execute(
                    "UPDATE review_discussion_items SET canonical_note_id=?2, reconciled_at=?3 WHERE item_id=?1",
                    params![item_id, canonical_note_id, now_text()],
                )?;
            }
        }
        let run_id = discussion_run(&self.database_path, project_id, &discussion_id)?;
        drop(connection);
        self.workspace(project_id, &run_id)?
            .discussions
            .into_iter()
            .flat_map(|d| d.items)
            .find(|i| i.item_id == item_id)
            .ok_or(ReviewDiscussionError::NotFound)
    }

    /// Bounded, deterministic agent-readable report with exportable requests.
    pub fn report(
        &self,
        project_id: &str,
        run_id: &str,
    ) -> Result<serde_json::Value, ReviewDiscussionError> {
        let workspace = self.workspace(project_id, run_id)?;
        let by_id: BTreeMap<&str, &WorkspaceUtterance> = workspace
            .utterances
            .iter()
            .map(|u| (u.machine.utterance_id.as_str(), u))
            .collect();
        let mut discussions = Vec::new();
        let mut feedback = Vec::new();
        let mut unresolved = Vec::new();
        for discussion in &workspace.discussions {
            let quotes: Vec<serde_json::Value> = discussion
                .utterance_ids
                .iter()
                .filter_map(|id| by_id.get(id.as_str()))
                .map(|u| {
                    serde_json::json!({
                        "utteranceId": u.machine.utterance_id,
                        "recordingSeconds": [u.machine.start_seconds, u.machine.end_seconds],
                        "movieTime": u.movie_time,
                        "speaker": u.speaker,
                        "language": u.language,
                        "originalText": u.text,
                        "displayText": u.display_text,
                        "machineText": u.machine.text,
                        "corrected": !u.corrections.is_empty(),
                        "excluded": u.excluded,
                    })
                })
                .collect();
            if discussion.target_operation_id.is_none() {
                unresolved.push(serde_json::json!({
                    "discussionId": discussion.discussion_id,
                    "question": if discussion.unanchored { "unanchored: no cut assignment by design" } else { "target cut not confirmed by a human" },
                    "candidateOperationIds": discussion.candidate_operation_ids,
                }));
            }
            for item in &discussion.items {
                let exportable = discussion.target_operation_id.is_some()
                    && matches!(item.kind, ItemKind::Observation | ItemKind::CandidateEdit)
                    && item.status != "retracted";
                if !exportable {
                    continue;
                }
                let movie_time = discussion
                    .utterance_ids
                    .iter()
                    .filter_map(|id| by_id.get(id.as_str()))
                    .find_map(|u| u.movie_time.clone());
                let mut evidence = vec![
                    workspace.session_id.clone(),
                    workspace.run_id.clone(),
                    discussion.discussion_id.clone(),
                    item.item_id.clone(),
                ];
                evidence.extend(
                    item.quote_utterance_ids
                        .iter()
                        .take(12)
                        .map(|id| format!("{}:{id}", workspace.run_id)),
                );
                let request = RecordReviewNoteRequest {
                    project_id: workspace.project_id.clone(),
                    revision_id: workspace.revision_id.clone(),
                    operation_id: discussion.target_operation_id.clone().unwrap_or_default(),
                    author: if item.author_kind == AuthorKind::Human {
                        ReviewAuthor::Human
                    } else {
                        ReviewAuthor::Reviewer
                    },
                    category: item
                        .category
                        .unwrap_or(if item.author_kind == AuthorKind::Human {
                            ReviewCategory::Intent
                        } else {
                            ReviewCategory::Preference
                        }),
                    observation: item.text.clone(),
                    viewed_artifact_id: Some(workspace.viewed_artifact_id.clone()),
                    movie_time_seconds: movie_time.as_ref().map(|m| m.movie_seconds),
                    evidence_refs: evidence.into_iter().take(16).collect(),
                };
                let accepted = discussion.items.iter().any(|d| {
                    d.kind == ItemKind::Decision
                        && d.refers_to_item_id.as_deref() == Some(&item.item_id)
                        && d.decision == Some(DecisionAction::Accepted)
                });
                // Stable, namespaced and immutable per item, so the owning
                // catalog's `record-once` never creates a second note after a
                // lost response (derived from ids only, never mutable text).
                let import_id = format!(
                    "rcdvlog-server/review/{}/{}/{}",
                    workspace.session_id, discussion.discussion_id, item.item_id
                );
                let record_once = RecordReviewNoteOnceRequest {
                    import_id: import_id.clone(),
                    record: request.clone(),
                };
                feedback.push(serde_json::json!({
                    "itemId": item.item_id,
                    "importId": import_id,
                    "discussionId": discussion.discussion_id,
                    "status": item.status,
                    "provisional": item.provisional,
                    "humanAccepted": accepted,
                    "movieTimePrecision": movie_time.as_ref().map(|m| m.source.clone()),
                    "canonicalNoteId": item.canonical_note_id,
                    "record": request,
                    "recordOnce": record_once,
                    "proposeChange": item.change,
                    "next": if item.canonical_note_id.is_some() { "reconciled" } else { "run `edit-plan feedback record-once --input` with `recordOnce` on the owning catalog, then POST the returned note ID as this item's reconciliation receipt; propose/decide only after a human acceptance" },
                }));
            }
            discussions.push(serde_json::json!({
                "discussionId": discussion.discussion_id,
                "title": discussion.title,
                "state": discussion.state,
                "groupingSource": discussion.grouping_source,
                "recordingSeconds": [discussion.start_recording_seconds, discussion.end_recording_seconds],
                "speakers": discussion.speakers,
                "languages": discussion.languages,
                "target": {"operationId": discussion.target_operation_id, "source": discussion.target_source, "unanchored": discussion.unanchored, "candidates": discussion.candidate_operation_ids},
                "utterances": quotes,
                "items": discussion.items,
            }));
        }
        Ok(serde_json::json!({
            "schema": REVIEW_REPORT_SCHEMA,
            "projectId": workspace.project_id,
            "revisionId": workspace.revision_id,
            "viewedArtifactId": workspace.viewed_artifact_id,
            "targetDigest": workspace.target_digest,
            "reviewTargetId": workspace.review_target_id,
            "sessionId": workspace.session_id,
            "transcriptionRunId": workspace.run_id,
            "sync": workspace.sync,
            "coverage": workspace.coverage,
            "rules": [
                "Only items with humanAccepted=true reflect an explicit human decision; provisional items are agent suggestions.",
                "Retracted items are kept for history and are never exported as feedback.",
                "movieTimeSeconds is a declared playback locator, not verified sync or a source trim.",
                "Notes target revision revisionId only; map them explicitly on the owning catalog if the plan has changed.",
                "Do not activate plans, patch timelines or make paid calls from this report without explicit authorization."
            ],
            "discussions": discussions,
            "feedback": feedback,
            "unresolved": unresolved,
        }))
    }
}

fn validate_utterances(
    workspace: &ReviewWorkspace,
    ids: &[String],
) -> Result<(), ReviewDiscussionError> {
    if ids.len() > MAX_DISCUSSION_UTTERANCES {
        return Err(invalid("too many utterances"));
    }
    for id in ids {
        if !workspace
            .utterances
            .iter()
            .any(|u| &u.machine.utterance_id == id)
        {
            return Err(invalid("utterance does not belong to this transcription"));
        }
    }
    Ok(())
}

fn discussion_run(
    database_path: &std::path::Path,
    project_id: &str,
    discussion_id: &str,
) -> Result<String, ReviewDiscussionError> {
    let connection = open_read_only_connection(database_path)?;
    connection
        .query_row(
            "SELECT run_id FROM review_discussions WHERE discussion_id=?1 AND project_id=?2",
            params![discussion_id, project_id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(ReviewDiscussionError::NotFound)
}

fn insert_discussion(
    connection: &Connection,
    project_id: &str,
    run_id: &str,
    title: &str,
    source: &str,
    ids: &[String],
    actor: &ReviewActor,
) -> Result<(), ReviewDiscussionError> {
    connection.execute(
        "INSERT INTO review_discussions(discussion_id, project_id, run_id, title, grouping_source, state, utterance_ids_json,
            unanchored, created_at, updated_at, created_by_type, created_by_id, updated_by_type, updated_by_id)
         VALUES(?1,?2,?3,?4,?5,'open',?6,0,?7,?7,?8,?9,?8,?9)",
        params![
            format!("rdisc_{}", Uuid::new_v4().simple()),
            project_id,
            run_id,
            title,
            source,
            serde_json::to_string(ids).context("ids")?,
            now_text(),
            actor.actor.actor_type,
            actor.actor.actor_id,
        ],
    )?;
    Ok(())
}

fn load_discussions(
    connection: &Connection,
    run_id: &str,
    utterances: &[WorkspaceUtterance],
) -> Result<Vec<Discussion>, ReviewDiscussionError> {
    let mut statement = connection.prepare(
        "SELECT discussion_id, title, grouping_source, state, utterance_ids_json, target_operation_id, target_source, unanchored, updated_at
         FROM review_discussions WHERE run_id=?1 ORDER BY created_at, discussion_id",
    )?;
    let rows = statement
        .query_map([run_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, bool>(7)?,
                r.get::<_, String>(8)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::new();
    for (id, title, source, state, ids_json, target, target_source, unanchored, updated_at) in rows
    {
        let ids: Vec<String> = serde_json::from_str(&ids_json).context("ids")?;
        let members: Vec<&WorkspaceUtterance> = utterances
            .iter()
            .filter(|u| ids.contains(&u.machine.utterance_id))
            .collect();
        let mut candidates: Vec<String> = Vec::new();
        let mut languages: Vec<String> = Vec::new();
        let mut speakers: Vec<String> = Vec::new();
        for member in &members {
            for candidate in &member.candidate_operation_ids {
                if !candidates.contains(candidate) {
                    candidates.push(candidate.clone());
                }
            }
            if !member.language.is_empty() && !languages.contains(&member.language) {
                languages.push(member.language.clone());
            }
            if !speakers.contains(&member.speaker) {
                speakers.push(member.speaker.clone());
            }
        }
        let items = load_items(connection, &id)?;
        out.push(Discussion {
            discussion_id: id,
            title,
            grouping_source: source,
            state,
            start_recording_seconds: members
                .iter()
                .map(|m| m.machine.start_seconds)
                .reduce(f64::min),
            end_recording_seconds: members
                .iter()
                .map(|m| m.machine.end_seconds)
                .reduce(f64::max),
            utterance_ids: ids,
            languages,
            speakers,
            candidate_operation_ids: candidates,
            target_operation_id: target,
            target_source,
            unanchored,
            items,
            updated_at,
        });
    }
    out.sort_by(|a, b| {
        a.start_recording_seconds
            .partial_cmp(&b.start_recording_seconds)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(out)
}

fn load_items(
    connection: &Connection,
    discussion_id: &str,
) -> Result<Vec<DiscussionItem>, ReviewDiscussionError> {
    let mut statement = connection.prepare(
        "SELECT item_id, kind, author_kind, actor_type, actor_id, text, speaker, quote_utterance_ids_json, category, change_json,
                decision, refers_to_item_id, created_at, canonical_note_id
         FROM review_discussion_items WHERE discussion_id=?1 ORDER BY sequence",
    )?;
    let rows = statement
        .query_map([discussion_id], |r| {
            Ok((
                (
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ),
                (
                    r.get::<_, String>(7)?,
                    r.get::<_, Option<String>>(8)?,
                    r.get::<_, Option<String>>(9)?,
                    r.get::<_, Option<String>>(10)?,
                    r.get::<_, Option<String>>(11)?,
                    r.get::<_, String>(12)?,
                    r.get::<_, Option<String>>(13)?,
                ),
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut items: Vec<DiscussionItem> = Vec::new();
    for (a, b) in rows {
        let author_kind = if a.2 == "human" {
            AuthorKind::Human
        } else {
            AuthorKind::Agent
        };
        items.push(DiscussionItem {
            item_id: a.0,
            kind: ItemKind::parse(&a.1),
            author_kind,
            provisional: author_kind == AuthorKind::Agent,
            actor: ReviewTargetActor {
                actor_type: a.3,
                actor_id: a.4,
            },
            text: a.5,
            speaker: a.6,
            quote_utterance_ids: serde_json::from_str(&b.0).context("ids")?,
            category: b
                .1
                .and_then(|c| serde_json::from_value(serde_json::Value::String(c)).ok()),
            change: b
                .2
                .map(|c| serde_json::from_str(&c))
                .transpose()
                .context("change")?,
            decision: b
                .3
                .and_then(|d| serde_json::from_value(serde_json::Value::String(d)).ok()),
            refers_to_item_id: b.4,
            status: "open".into(),
            created_at: b.5,
            canonical_note_id: b.6,
        });
    }
    // Derive statuses in order: the latest retraction or decision wins.
    let events: Vec<(String, String)> = items
        .iter()
        .filter_map(|i| {
            let target = i.refers_to_item_id.clone()?;
            let status = match i.kind {
                ItemKind::Retraction => "retracted".to_string(),
                ItemKind::Decision => match i.decision? {
                    DecisionAction::Accepted => "accepted".into(),
                    DecisionAction::Declined => "declined".into(),
                    DecisionAction::Deferred => "deferred".into(),
                },
                _ => return None,
            };
            Some((target, status))
        })
        .collect();
    for (target, status) in events {
        if let Some(item) = items.iter_mut().find(|i| i.item_id == target) {
            item.status = status;
        }
    }
    Ok(items)
}

pub(crate) fn migrate(tx: &rusqlite::Transaction<'_>) -> anyhow::Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE review_utterance_corrections (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            correction_id TEXT NOT NULL UNIQUE,
            run_id TEXT NOT NULL REFERENCES review_transcription_runs(run_id),
            utterance_id TEXT NOT NULL,
            field TEXT NOT NULL CHECK(field IN ('text','speaker','language','movieTime','excluded')),
            value_json TEXT NOT NULL CHECK(length(value_json) <= 8192),
            reason TEXT CHECK(reason IS NULL OR length(reason) <= 1024),
            actor_type TEXT NOT NULL,
            actor_id TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE INDEX review_utterance_corrections_run ON review_utterance_corrections(run_id, utterance_id);
        CREATE TABLE review_discussions (
            discussion_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES project_registry(project_id),
            run_id TEXT NOT NULL REFERENCES review_transcription_runs(run_id),
            title TEXT NOT NULL CHECK(length(title) <= 4096),
            grouping_source TEXT NOT NULL CHECK(grouping_source IN ('heuristic','human')),
            state TEXT NOT NULL CHECK(state IN ('open','resolved','deferred')),
            utterance_ids_json TEXT NOT NULL,
            target_operation_id TEXT,
            target_source TEXT CHECK(target_source IS NULL OR target_source = 'human'),
            unanchored INTEGER NOT NULL DEFAULT 0 CHECK(unanchored IN (0,1)),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            created_by_type TEXT NOT NULL,
            created_by_id TEXT NOT NULL,
            updated_by_type TEXT NOT NULL,
            updated_by_id TEXT NOT NULL
        );
        CREATE INDEX review_discussions_run ON review_discussions(run_id);
        CREATE TABLE review_discussion_items (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            item_id TEXT NOT NULL UNIQUE,
            discussion_id TEXT NOT NULL REFERENCES review_discussions(discussion_id),
            kind TEXT NOT NULL CHECK(kind IN ('observation','candidateEdit','speakerClaim','retraction','note','decision')),
            author_kind TEXT NOT NULL CHECK(author_kind IN ('human','agent')),
            actor_type TEXT NOT NULL,
            actor_id TEXT NOT NULL,
            text TEXT NOT NULL CHECK(length(text) <= 4096),
            speaker TEXT,
            quote_utterance_ids_json TEXT NOT NULL,
            category TEXT,
            change_json TEXT CHECK(change_json IS NULL OR length(change_json) <= 8192),
            decision TEXT CHECK(decision IS NULL OR decision IN ('accepted','declined','deferred')),
            refers_to_item_id TEXT REFERENCES review_discussion_items(item_id),
            created_at TEXT NOT NULL,
            canonical_note_id TEXT,
            reconciled_at TEXT,
            CHECK(kind <> 'decision' OR author_kind = 'human')
        );
        CREATE INDEX review_discussion_items_discussion ON review_discussion_items(discussion_id);
        "#,
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "review_discussions_tests.rs"]
mod tests;
