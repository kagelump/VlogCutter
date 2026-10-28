//! A bounded local tool session for an external host or video reviewer.
//! No model execution, paid admission, human decision or edit mutation lives here.
use super::{SourceInspection, SourceInspectionRequest, SourceTrimRequest};
use crate::Catalog;
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    cell::Cell,
    fs::OpenOptions,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InspectionReviewRequest {
    pub seed_evidence_id: String,
    pub asset_id: String,
    pub max_steps: u32,
    pub max_total_frames: u32,
    /// Counts every attempted window including handles, even on cache hits.
    pub max_media_seconds: f64,
    pub max_width: u32,
    pub allow_audio: bool,
    pub allow_transcript: bool,
}

#[cfg(test)]
#[path = "inspection_review_controls_tests.rs"]
mod control_tests;
#[cfg(test)]
#[path = "inspection_review_tests.rs"]
mod tests;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectionReviewSession {
    pub id: String,
    pub request: InspectionReviewRequest,
    pub source_fingerprint: String,
    pub representation_fingerprint: String,
    pub constraints_revision: u64,
    pub window_in_seconds: f64,
    pub window_out_seconds: f64,
    pub state: String,
    pub cancel_requested: bool,
    pub next_step: u32,
    pub frames_reserved: u32,
    pub media_seconds_reserved: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InspectionReviewCaller {
    Host,
    Reviewer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InspectionObservationKind {
    CoveredEdge,
    CoveredMiddle,
    UnusableRange,
    OrientationQuestion,
    IncompleteAction,
    IncompleteSpeech,
    MeaningfulDark,
    NoDefectObserved,
    Uncertain,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InspectionObservation {
    pub inspected_evidence_id: String,
    pub kind: InspectionObservationKind,
    pub source_in_seconds: f64,
    pub source_out_seconds: f64,
    pub observation: String,
    pub uncertainty: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "tool",
    content = "input",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum InspectionReviewTool {
    Inspect(SourceInspectionRequest),
    SuggestTrim(SourceTrimRequest),
    Observe(InspectionObservation),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InspectionReviewStepRequest {
    pub session_id: String,
    pub step_index: u32,
    pub caller: InspectionReviewCaller,
    pub request: InspectionReviewTool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectionReviewStep {
    pub step_index: u32,
    pub request: InspectionReviewStepRequest,
    pub state: String,
    pub result: Option<Value>,
}

fn span(a: f64, b: f64, start: f64, end: f64) -> Result<()> {
    ensure!(
        a.is_finite() && b.is_finite() && a >= start && b > a && b <= end,
        "invalid input: reviewer request exceeds the frozen source window"
    );
    Ok(())
}

impl Catalog {
    pub fn create_inspection_review(
        &self,
        request: &InspectionReviewRequest,
    ) -> Result<InspectionReviewSession> {
        ensure!(
            (1..=32).contains(&request.max_steps)
                && (1..=512).contains(&request.max_total_frames)
                && request.max_media_seconds.is_finite()
                && request.max_media_seconds > 0.0
                && request.max_media_seconds <= 600.0
                && (160..=1280).contains(&request.max_width)
                && (!request.allow_transcript || request.allow_audio),
            "invalid input: inspection review limits"
        );
        let seed =
            self.require_current_source_inspection(&request.seed_evidence_id, &request.asset_id)?;
        let asset = self.get_asset(&request.asset_id)?;
        let constraints = self.source_range_constraints(&asset.id, 0, 1)?;
        let session = InspectionReviewSession {
            id: format!("inspect_review_{}", uuid::Uuid::new_v4().simple()),
            request: request.clone(),
            source_fingerprint: seed.source_fingerprint,
            representation_fingerprint: seed.representation_fingerprint,
            constraints_revision: constraints.revision,
            window_in_seconds: seed.window_in_seconds,
            window_out_seconds: seed.window_out_seconds,
            state: "open".into(),
            cancel_requested: false,
            next_step: 0,
            frames_reserved: 0,
            media_seconds_reserved: 0.0,
        };
        self.connection()?.execute("INSERT INTO inspection_review_sessions(id,project_id,asset_id,body_json,updated_at) VALUES(?1,?2,?3,?4,?5)",
            params![session.id, asset.project_id, asset.id, serde_json::to_string(&session)?, crate::now()])?;
        Ok(session)
    }

    /// Pure SQLite observation. No hashing, probing, repair or inferred freshness.
    pub fn inspection_review_status(&self, id: &str) -> Result<InspectionReviewSession> {
        self.connection()?.query_row("SELECT body_json,state,cancel_requested,next_step,frames_reserved,media_seconds_reserved FROM inspection_review_sessions WHERE id=?1", [id], |r| {
            Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?,r.get::<_,bool>(2)?,r.get::<_,u32>(3)?,r.get::<_,u32>(4)?,r.get::<_,f64>(5)?))
        }).optional()?.context("inspection review session not found").and_then(|row| {
            let mut session: InspectionReviewSession = serde_json::from_str(&row.0)?;
            session.state = row.1; session.cancel_requested = row.2; session.next_step = row.3;
            session.frames_reserved = row.4; session.media_seconds_reserved = row.5; Ok(session)
        })
    }

    pub fn inspection_review_steps(
        &self,
        id: &str,
        after: Option<u32>,
        limit: u32,
    ) -> Result<Vec<InspectionReviewStep>> {
        ensure!(
            (1..=32).contains(&limit),
            "invalid input: review page limit must be 1-32"
        );
        self.inspection_review_status(id)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT step_index,request_json,state,result_json FROM inspection_review_steps WHERE session_id=?1 AND step_index>?2 ORDER BY step_index LIMIT ?3")?;
        let rows = statement.query_map(
            params![id, after.map(i64::from).unwrap_or(-1), limit],
            |r| {
                Ok((
                    r.get::<_, u32>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        )?;
        rows.map(|row| {
            let (step_index, request, state, result) = row?;
            Ok(InspectionReviewStep {
                step_index,
                request: serde_json::from_str(&request)?,
                state,
                result: result.map(|s| serde_json::from_str(&s)).transpose()?,
            })
        })
        .collect()
    }

    pub fn cancel_inspection_review(&self, id: &str) -> Result<InspectionReviewSession> {
        self.inspection_review_status(id)?;
        self.connection()?.execute("UPDATE inspection_review_sessions SET cancel_requested=1,state='cancelled',updated_at=?2 WHERE id=?1 AND state='open'",params![id,crate::now()])?;
        self.inspection_review_status(id)
    }

    fn inspection_review_claim(&self, id: &str) -> Result<std::fs::File> {
        let root = super::preview_artifacts::managed_child(
            &super::preview_artifacts::project_directory(self)?,
            "cache",
        )?;
        let root = super::preview_artifacts::managed_child(&root, "inspection-reviews")?;
        let path = root.join(format!("{}.lock", blake3::hash(id.as_bytes()).to_hex()));
        ensure!(
            !std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()),
            "invalid input: review claim is a symlink"
        );
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        fs2::FileExt::try_lock_exclusive(&file).context("inspection review already in progress")?;
        Ok(file)
    }

    fn require_inspection_review_current(
        &self,
        session: &InspectionReviewSession,
        cancel: &dyn Fn() -> bool,
    ) -> Result<SourceInspection> {
        let seed = self.require_current_source_inspection_cancellable(
            &session.request.seed_evidence_id,
            &session.request.asset_id,
            cancel,
        )?;
        ensure!(
            seed.source_fingerprint == session.source_fingerprint
                && seed.representation_fingerprint == session.representation_fingerprint
                && self
                    .source_range_constraints(&session.request.asset_id, 0, 1)?
                    .revision
                    == session.constraints_revision,
            "stale evidence: inspection review source or human constraints changed"
        );
        Ok(seed)
    }

    pub fn finish_inspection_review(&self, id: &str) -> Result<InspectionReviewSession> {
        let session = self.inspection_review_status(id)?;
        let _claim = self.inspection_review_claim(id)?;
        ensure!(
            session.state == "open" && !session.cancel_requested,
            "inspection review is terminal"
        );
        self.require_inspection_review_current(&session, &|| false)?;
        self.connection()?.execute("UPDATE inspection_review_sessions SET state='complete',updated_at=?2 WHERE id=?1 AND state='open' AND cancel_requested=0",params![id,crate::now()])?;
        self.inspection_review_status(id)
    }

    /// Both callers use identical primitives and bounds. Each admitted attempt
    /// consumes its limits durably before work; failed/interrupted work is not
    /// refunded. A byte-identical completed step is safely observable again.
    pub fn inspection_review_step(
        &self,
        request: &InspectionReviewStepRequest,
        cancel: &dyn Fn() -> bool,
    ) -> Result<InspectionReviewStep> {
        let body = serde_json::to_string(request)?;
        ensure!(
            body.len() <= 64 * 1024,
            "invalid input: review request exceeds 64 KiB"
        );
        let session = self.inspection_review_status(&request.session_id)?;
        let _claim = self.inspection_review_claim(&session.id)?;
        let last_poll = Cell::new(None::<Instant>);
        let stopped = Cell::new(false);
        let cancelled = || {
            if cancel() || stopped.get() {
                return true;
            }
            if last_poll
                .get()
                .is_none_or(|at| at.elapsed() >= Duration::from_millis(100))
            {
                last_poll.set(Some(Instant::now()));
                stopped.set(
                    self.inspection_review_status(&session.id)
                        .map_or(true, |s| s.cancel_requested),
                );
            }
            stopped.get()
        };
        if let Some(previous) = self
            .inspection_review_steps(&session.id, request.step_index.checked_sub(1), 1)?
            .into_iter()
            .find(|s| s.step_index == request.step_index)
        {
            ensure!(
                serde_json::to_string(&previous.request)? == body && previous.state == "complete",
                "review step already attempted; use the next index without resetting limits"
            );
            self.require_inspection_review_current(&session, &cancelled)?;
            let evidence = match &previous.request.request {
                InspectionReviewTool::Inspect(_) => previous
                    .result
                    .as_ref()
                    .and_then(|v| v.pointer("/inspection/evidenceId"))
                    .and_then(Value::as_str)
                    .context("review step is missing inspection evidence")?,
                InspectionReviewTool::SuggestTrim(r) => &r.inspected_evidence_id,
                InspectionReviewTool::Observe(o) => &o.inspected_evidence_id,
            };
            self.require_review_step_evidence(&session, evidence, &cancelled)?;
            return Ok(previous);
        }
        ensure!(
            !cancelled() && session.state == "open" && !session.cancel_requested,
            "inspection review stopped"
        );
        let seed = self.require_inspection_review_current(&session, &cancelled)?;
        let (frames, seconds) = match &request.request {
            InspectionReviewTool::Inspect(r) => {
                ensure!(
                    r.asset_id == session.request.asset_id
                        && r.representation == seed.request.representation
                        && r.max_width <= session.request.max_width
                        && (!r.include_audio || session.request.allow_audio)
                        && (!r.include_transcript || session.request.allow_transcript),
                    "invalid input: review tool scope or payload denied"
                );
                ensure!(
                    r.before_seconds.is_finite()
                        && r.before_seconds >= 0.0
                        && r.after_seconds.is_finite()
                        && r.after_seconds >= 0.0,
                    "invalid input: review handles"
                );
                span(
                    r.source_in_seconds - r.before_seconds,
                    r.source_out_seconds + r.after_seconds,
                    session.window_in_seconds,
                    session.window_out_seconds,
                )?;
                let count = u64::from(r.sample_count)
                    + r.source_times_seconds.len() as u64
                    + r.exact_representation_pts.len() as u64;
                ensure!(
                    count <= 64,
                    "invalid input: review frame request exceeds 64"
                );
                (
                    count as u32,
                    r.source_out_seconds + r.after_seconds - r.source_in_seconds + r.before_seconds,
                )
            }
            InspectionReviewTool::SuggestTrim(r) => {
                ensure!(
                    r.asset_id == session.request.asset_id,
                    "invalid input: review trim asset mismatch"
                );
                span(
                    r.source_in_seconds,
                    r.source_out_seconds,
                    session.window_in_seconds,
                    session.window_out_seconds,
                )?;
                self.require_review_step_evidence(&session, &r.inspected_evidence_id, &cancelled)?;
                (0, 0.0)
            }
            InspectionReviewTool::Observe(o) => {
                ensure!(
                    !o.observation.trim().is_empty()
                        && o.observation.len() <= 2048
                        && !o.uncertainty.trim().is_empty()
                        && o.uncertainty.len() <= 2048,
                    "invalid input: advisory observations require bounded uncertainty"
                );
                let inspected = self.require_review_step_evidence(
                    &session,
                    &o.inspected_evidence_id,
                    &cancelled,
                )?;
                span(
                    o.source_in_seconds,
                    o.source_out_seconds,
                    inspected.window_in_seconds,
                    inspected.window_out_seconds,
                )?;
                if matches!(o.kind, InspectionObservationKind::IncompleteSpeech) {
                    ensure!(
                        inspected
                            .audio
                            .as_ref()
                            .is_some_and(|a| a.playable.is_some()),
                        "invalid input: speech observation requires inspected sound"
                    );
                }
                (0, 0.0)
            }
        };
        ensure!(
            request.step_index == session.next_step,
            "stale revision: unexpected review step index"
        );
        if session.next_step >= session.request.max_steps
            || session.frames_reserved + frames > session.request.max_total_frames
            || session.media_seconds_reserved + seconds > session.request.max_media_seconds
        {
            self.connection()?.execute("UPDATE inspection_review_sessions SET state='exhausted',updated_at=?2 WHERE id=?1 AND state='open'",params![session.id,crate::now()])?;
            anyhow::bail!("inspection review limits exhausted");
        }
        {
            let mut connection = self.connection()?;
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let changed=tx.execute("UPDATE inspection_review_sessions SET next_step=next_step+1,frames_reserved=frames_reserved+?3,media_seconds_reserved=media_seconds_reserved+?4,updated_at=?5 WHERE id=?1 AND next_step=?2 AND state='open' AND cancel_requested=0",params![session.id,request.step_index,frames,seconds,crate::now()])?;
            ensure!(changed == 1, "inspection review stopped or changed");
            tx.execute("INSERT INTO inspection_review_steps(session_id,step_index,request_json,state) VALUES(?1,?2,?3,'running')",params![session.id,request.step_index,body])?;
            tx.commit()?;
        }
        let outcome = (|| -> Result<Value> {
            ensure!(!cancelled(), "inspection review cancelled");
            let result = match &request.request {
                InspectionReviewTool::Inspect(r) => {
                    serde_json::to_value(self.inspect_source_window(r, &cancelled)?)?
                }
                InspectionReviewTool::SuggestTrim(r) => {
                    serde_json::to_value(self.suggest_source_trim_cancellable(r, &cancelled)?)?
                }
                InspectionReviewTool::Observe(o) => {
                    json!({"advisory":true,"observation":o,"limitations":["This is an external reviewer's observation, not a confirmed defect, clean-range certification, exact edit or human decision. Unsampled frames remain uninspected; dark scenes, maps, hands and close-ups need context."]})
                }
            };
            self.require_inspection_review_current(&session, &cancelled)?;
            ensure!(!cancelled(), "inspection review cancelled");
            ensure!(
                serde_json::to_vec(&result)?.len() <= 1024 * 1024,
                "review tool result exceeds 1 MiB"
            );
            Ok(result)
        })();
        let state = if outcome.is_ok() {
            "complete"
        } else {
            "failed"
        };
        let result = match outcome {
            Ok(v) => v,
            Err(e) => json!({"error":e.to_string(),"limitsConsumed":true}),
        };
        self.connection()?.execute("UPDATE inspection_review_steps SET state=?3,result_json=?4 WHERE session_id=?1 AND step_index=?2",params![session.id,request.step_index,state,serde_json::to_string(&result)?])?;
        // A stale/failed attempt still consumes its bounds and never mutates human facts.
        Ok(InspectionReviewStep {
            step_index: request.step_index,
            request: request.clone(),
            state: state.into(),
            result: Some(result),
        })
    }

    fn require_review_step_evidence(
        &self,
        session: &InspectionReviewSession,
        evidence_id: &str,
        cancel: &dyn Fn() -> bool,
    ) -> Result<SourceInspection> {
        let inspected = self.require_current_source_inspection_cancellable(
            evidence_id,
            &session.request.asset_id,
            cancel,
        )?;
        let seed = self.require_current_source_inspection_cancellable(
            &session.request.seed_evidence_id,
            &session.request.asset_id,
            cancel,
        )?;
        let belongs = evidence_id == session.request.seed_evidence_id
            || self
                .inspection_review_steps(&session.id, None, 32)?
                .iter()
                .any(|step| {
                    step.state == "complete"
                        && matches!(&step.request.request, InspectionReviewTool::Inspect(_))
                        && step
                            .result
                            .as_ref()
                            .and_then(|v| v.pointer("/inspection/evidenceId"))
                            .and_then(Value::as_str)
                            == Some(evidence_id)
                });
        ensure!(
            belongs
                && inspected.representation_fingerprint == session.representation_fingerprint
                && inspected.mapping == seed.mapping
                && inspected.request.representation == seed.request.representation,
            "invalid input: inspection citation is outside this review session"
        );
        Ok(inspected)
    }
}
