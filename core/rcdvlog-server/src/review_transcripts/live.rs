//! Provisional, bounded local decoding. The immutable full-pass runs keep their
//! existing audio identity, caching, correction and cloud-authorization contracts.
use super::*;
use crate::{MovieTimeEstimate, ReviewTargets};
use rcdvlog_core::review_speech::{ModelAsset, install_model};
use std::time::Duration;

pub const LIVE_REVIEW_MODEL: ModelAsset = ModelAsset {
    file_name: "ggml-tiny.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin",
    blake3: "",
    bytes: 77_691_713,
};
const WINDOW_SECONDS: u64 = 30;
const STEP_SECONDS: u64 = 4;
const DISPLAY_LINES: usize = 200;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveReviewLine {
    pub utterance: ReviewUtterance,
    pub movie_time: Option<MovieTimeEstimate>,
    pub movie_time_ranges: Vec<crate::review_sync::MovieTimeRange>,
    pub asset_id: Option<String>,
    pub shot_number: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveReviewTranscript {
    pub enabled: bool,
    /// disabled, live, refining, complete, failed, stopped
    pub state: String,
    pub model: &'static str,
    pub model_available: bool,
    pub processed_seconds: f64,
    pub received_seconds: f64,
    pub revision: u64,
    pub final_run_id: Option<String>,
    pub error_code: Option<String>,
    pub sync: crate::SyncDescription,
    /// A bounded tail; the full transcript is available from the final run.
    pub lines: Vec<LiveReviewLine>,
}

impl ModelDirSpeechRuntime {
    pub fn install_live_models(&self) -> anyhow::Result<()> {
        for model in [&LIVE_REVIEW_MODEL, &VAD_MODEL] {
            install_model(&self.model_dir, model, |_, _| {
                std::ops::ControlFlow::Continue(())
            })?;
        }
        Ok(())
    }
}

impl ReviewTranscripts {
    /// Durable opt-in to local live decoding and an automatic local full pass.
    /// Missing live models do not prevent recording or scheduling the full pass.
    pub fn enable_live(
        &self,
        project_id: &str,
        session_id: &str,
        actor: &ReviewTargetActor,
        runtime: &dyn ReviewSpeechRuntime,
    ) -> Result<LiveReviewTranscript, ReviewTranscriptError> {
        let connection = open_writable_connection(&self.database_path)?;
        Self::project_registered(&connection, project_id)?;
        let audio = load_session_audio(&connection, project_id, session_id)?;
        if !matches!(
            audio.state.as_str(),
            "recording" | "completed" | "incomplete"
        ) {
            return Err(ReviewTranscriptError::Conflict);
        }
        connection.execute(
            "INSERT INTO review_live_transcripts(session_id, project_id, actor_json, state, updated_at)
             VALUES(?1,?2,?3,'live',?4)
             ON CONFLICT(session_id) DO UPDATE SET state='live', error_code=NULL, updated_at=excluded.updated_at
             WHERE review_live_transcripts.state='failed' AND review_live_transcripts.final_run_id IS NULL",
            params![session_id, project_id, serde_json::to_string(actor).context("encode actor")?, now_text()],
        )?;
        self.live_transcript(project_id, session_id, runtime)
    }

    /// Read-only projection. Movie mapping is recomputed as calibration/player
    /// evidence arrives, even when no new words have been decoded.
    pub fn live_transcript(
        &self,
        project_id: &str,
        session_id: &str,
        runtime: &dyn ReviewSpeechRuntime,
    ) -> Result<LiveReviewTranscript, ReviewTranscriptError> {
        let connection = open_read_only_connection(&self.database_path)?;
        Self::project_registered(&connection, project_id)?;
        let audio = load_session_audio(&connection, project_id, session_id)?;
        let row = connection.query_row(
            "SELECT state, processed_end_frame, revision, final_run_id, error_code FROM review_live_transcripts WHERE session_id=?1",
            [session_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64, r.get::<_, i64>(2)? as u64, r.get::<_, Option<String>>(3)?, r.get::<_, Option<String>>(4)?)),
        ).optional()?;
        let enabled = row.is_some();
        let (mut state, processed, revision, final_run_id, mut error_code) =
            row.unwrap_or(("disabled".into(), 0, 0, None, None));
        let mut utterances = Vec::new();
        // Deleted evidence must never be surfaced through the provisional view.
        if audio.state != "deleted" {
            let mut statement = connection.prepare(
                "SELECT payload_json FROM review_live_utterances WHERE session_id=?1 ORDER BY start_frame DESC, end_frame DESC LIMIT ?2",
            )?;
            for json in statement.query_map(params![session_id, DISPLAY_LINES as i64], |r| {
                r.get::<_, String>(0)
            })? {
                utterances.push(
                    serde_json::from_str::<ReviewUtterance>(&json?).context("decode live line")?,
                );
            }
            utterances.reverse();
        } else {
            state = "stopped".into();
        }
        if let Some(id) = final_run_id.as_deref() {
            let run = self.get(project_id, id)?;
            state = match run.state {
                ReviewTranscriptionState::Completed | ReviewTranscriptionState::NoSpeech => {
                    "complete"
                }
                ReviewTranscriptionState::Partial
                | ReviewTranscriptionState::Failed
                | ReviewTranscriptionState::Cancelled => "failed",
                _ => "refining",
            }
            .into();
            error_code = run.error_code;
        }
        let target_id: String = connection.query_row(
            "SELECT review_target_id FROM review_sessions WHERE session_id=?1",
            [session_id],
            |r| r.get(0),
        )?;
        let handoff = ReviewTargets::new(self.database_path.clone())
            .evidence(project_id, &target_id)
            .map_err(|e| anyhow!(e))?;
        let mut mapper =
            crate::review_sync::SessionSync::from_session(&connection, session_id, &handoff)?;
        let intervals: Vec<_> = utterances
            .iter()
            .map(|u| (u.start_frame, u.end_frame))
            .collect();
        let mapped = mapper.map_intervals(&intervals, &handoff);
        let movie = &handoff.target().movie;
        let tps = movie.time_base_denominator as f64 / movie.time_base_numerator as f64;
        let lines = utterances
            .into_iter()
            .zip(mapped)
            .map(|(utterance, sync)| {
                let movie_time = sync
                    .slices
                    .first()
                    .filter(|s| s.recording.start_sample == utterance.start_frame)
                    .and_then(|s| {
                        let state = serde_json::to_value(s.state).ok()?;
                        let state = state.as_str()?;
                        if !matches!(state, "anchored" | "estimated") || s.candidates.len() != 1 {
                            return None;
                        }
                        let c = &s.candidates[0];
                        Some(MovieTimeEstimate {
                            movie_seconds: (c.start_pts - movie.origin_pts as f64) / tps,
                            uncertainty_seconds: c.uncertainty_ticks as f64 / tps,
                            source: format!("sharedSync:{state}"),
                            evidence_ids: c.evidence_ids.clone(),
                        })
                    });
                let cut = movie_time.as_ref().and_then(|m| {
                    handoff.target().cuts.iter().enumerate().find(|(_, c)| {
                        let start = (c.movie_in_pts - movie.origin_pts) as f64 / tps;
                        let end = (c.movie_out_pts - movie.origin_pts) as f64 / tps;
                        start <= m.movie_seconds && m.movie_seconds < end
                    })
                });
                LiveReviewLine {
                    movie_time_ranges: sync.movie_time_ranges(audio.sample_rate, &handoff),
                    asset_id: cut.map(|(_, c)| c.asset_id.clone()),
                    shot_number: cut.map(|(i, _)| i + 1),
                    utterance,
                    movie_time,
                }
            })
            .collect();
        Ok(LiveReviewTranscript {
            enabled,
            state,
            model: LIVE_REVIEW_MODEL.file_name,
            model_available: runtime.live_models_present(),
            processed_seconds: processed as f64 / f64::from(audio.sample_rate),
            received_seconds: audio
                .chunks
                .iter()
                .map(|(_, _, n, _)| u64::from(*n))
                .sum::<u64>() as f64
                / f64::from(audio.sample_rate),
            revision,
            final_run_id,
            error_code,
            sync: mapper.description().clone(),
            lines,
        })
    }

    /// One bounded tick. Decoding skips to the newest contiguous received range
    /// after a backlog; the immutable full pass will cover *all* received audio.
    /// Holds the shared heavy lease only while loading/decoding, never while idle.
    fn live_tick(
        &self,
        runtime: &dyn ReviewSpeechRuntime,
        engine: &mut Option<Box<dyn SttEngine>>,
        jobs: &JobEngine,
        cancelled: &dyn Fn() -> bool,
        lock_path: &std::path::Path,
        now: &str,
    ) -> Result<(), ReviewTranscriptError> {
        {
            let connection = open_writable_connection(&self.database_path)?;
            connection.execute("UPDATE review_live_transcripts SET state='stopped' WHERE state='live' AND session_id IN
                (SELECT session_id FROM review_sessions WHERE state='recording' AND recording_expires_at<?1)", [now])?;
        }
        let rows = {
            let connection = open_read_only_connection(&self.database_path)?;
            let mut statement = connection.prepare(
                "SELECT l.project_id, l.session_id, l.processed_end_frame, l.actor_json FROM review_live_transcripts l
                 JOIN review_sessions s ON s.session_id=l.session_id WHERE l.state='live' OR (l.state='stopped' AND s.state IN ('completed','incomplete')) ORDER BY l.updated_at LIMIT 4",
            )?;
            statement
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)? as u64,
                        r.get::<_, String>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (project, session, processed, actor_json) in rows {
            if cancelled() {
                break;
            }
            let audio = {
                let connection = open_read_only_connection(&self.database_path)?;
                load_session_audio(&connection, &project, &session)?
            };
            if matches!(audio.state.as_str(), "completed" | "incomplete") {
                // Idempotent fingerprint/settings key makes crash recovery safe
                // between enqueueing the final job and recording its run id.
                let actor: ReviewTargetActor =
                    serde_json::from_str(&actor_json).context("decode actor")?;
                let request = ReviewTranscriptionRequest {
                    backend: ReviewTranscriptionBackend::Local,
                    model: None,
                    language_hint: None,
                    authorized_max_cost_usd: None,
                    cloud_upload_acknowledged: false,
                    zero_data_retention: false,
                };
                let result = self.start(&project, &session, &request, runtime, jobs, &actor);
                let connection = open_writable_connection(&self.database_path)?;
                match result {
                    Ok(run) => {
                        connection.execute("UPDATE review_live_transcripts SET state='refining', final_run_id=?2, error_code=NULL, updated_at=?3 WHERE session_id=?1", params![session, run.run_id, now_text()])?;
                    }
                    Err(error) => {
                        connection.execute("UPDATE review_live_transcripts SET state='failed', error_code=?2, updated_at=?3 WHERE session_id=?1", params![session, error.code(), now_text()])?;
                    }
                }
                continue;
            }
            if audio.state != "recording" {
                let connection = open_writable_connection(&self.database_path)?;
                connection.execute(
                    "UPDATE review_live_transcripts SET state='stopped' WHERE session_id=?1",
                    [&session],
                )?;
                continue;
            }
            if !runtime.live_models_present() {
                self.live_error(&session, "review_live_model_unavailable")?;
                continue;
            }
            let Some((range_start, end)) = audio.segments().last().copied() else {
                continue;
            };
            let step = STEP_SECONDS * u64::from(audio.sample_rate);
            if end <= processed
                || (end - processed < step && processed >= range_start)
                || end - range_start < step
            {
                continue;
            }
            let start =
                range_start.max(end.saturating_sub(WINDOW_SECONDS * u64::from(audio.sample_rate)));
            let Some(_lease) = rcdvlog_core::try_acquire_local_heavy_at(lock_path)? else {
                self.live_error(&session, "review_live_resource_busy")?;
                continue;
            };
            if engine.is_none() {
                match runtime.live_engine() {
                    Ok(loaded) => *engine = Some(loaded),
                    Err(_) => {
                        self.live_error(&session, "review_live_model_unavailable")?;
                        continue;
                    }
                }
            }
            let pcm = match self.read_segment_pcm(&audio, start, end) {
                Ok(pcm) => pcm,
                Err(_) => {
                    self.live_error(&session, "review_transcription_audio_unavailable")?;
                    continue;
                }
            };
            let result = decode_live(
                engine.as_ref().expect("loaded").as_ref(),
                &pcm,
                start,
                end,
                audio.sample_rate,
                &audio.gaps(),
                cancelled,
            );
            let utterances = match result {
                Ok(lines) => lines,
                Err(_) => {
                    self.live_error(&session, "review_transcription_decode_failed")?;
                    continue;
                }
            };
            if cancelled() {
                break;
            }
            let mut connection = open_writable_connection(&self.database_path)?;
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Finalize/delete can race a decode; don't publish after either.
            let state: String = tx.query_row(
                "SELECT state FROM review_sessions WHERE session_id=?1",
                [&session],
                |r| r.get(0),
            )?;
            if state != "recording" {
                continue;
            }
            tx.execute(
                "DELETE FROM review_live_utterances WHERE session_id=?1 AND end_frame>?2",
                params![session, start as i64],
            )?;
            for line in utterances {
                tx.execute("INSERT INTO review_live_utterances(session_id, utterance_id, start_frame, end_frame, payload_json) VALUES(?1,?2,?3,?4,?5)
                    ON CONFLICT(session_id, utterance_id) DO UPDATE SET payload_json=excluded.payload_json, start_frame=excluded.start_frame, end_frame=excluded.end_frame",
                    params![session, line.utterance_id, line.start_frame as i64, line.end_frame as i64, serde_json::to_string(&line).context("encode line")?])?;
            }
            tx.execute("UPDATE review_live_transcripts SET processed_end_frame=?2, revision=revision+1, error_code=NULL, updated_at=?3 WHERE session_id=?1", params![session, end as i64, now_text()])?;
            tx.commit()?;
        }
        Ok(())
    }

    fn live_error(&self, session: &str, code: &str) -> Result<(), ReviewTranscriptError> {
        let connection = open_writable_connection(&self.database_path)?;
        connection.execute(
            "UPDATE review_live_transcripts SET error_code=?2 WHERE session_id=?1",
            params![session, code],
        )?;
        Ok(())
    }

    /// Runs inside the server's existing lifecycle. Durable rows survive browser
    /// closure and server restart; the audio uploader never waits for this loop.
    pub fn run_live_worker(&self, jobs: JobEngine, shutdown: CancellationToken) {
        let runtime = ModelDirSpeechRuntime::from_environment();
        let mut engine = None;
        while !shutdown.is_cancelled() {
            if self
                .live_tick(
                    &runtime,
                    &mut engine,
                    &jobs,
                    &|| shutdown.is_cancelled(),
                    &rcdvlog_core::default_local_heavy_lock_path(),
                    &now_text(),
                )
                .is_err()
            {
                tracing::warn!(
                    code = "review_live_tick_failed",
                    "live review worker tick failed"
                );
            }
            // Avoid retaining the model when no session is capturing.
            if let Ok(connection) = open_read_only_connection(&self.database_path) {
                let active: i64 = connection
                    .query_row(
                        "SELECT COUNT(*) FROM review_live_transcripts WHERE state='live'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(1);
                if active == 0 {
                    engine = None;
                }
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

fn decode_live(
    engine: &dyn SttEngine,
    pcm: &[f32],
    start: u64,
    end: u64,
    rate: u32,
    gaps: &[crate::FrameRange],
    cancelled: &dyn Fn() -> bool,
) -> anyhow::Result<Vec<ReviewUtterance>> {
    let regions = engine.detect_speech(pcm)?;
    let mut windows = pack_windows(&regions);
    windows.reverse();
    let mut lines = Vec::new();
    while let Some(window) = windows.pop() {
        if cancelled() {
            break;
        }
        let decoded = engine.decode_window(pcm, &window)?;
        if needs_resplit(&window, &decoded) {
            let mut narrow = resplit(&window);
            narrow.reverse();
            windows.extend(narrow);
            continue;
        }
        let next = to_utterances(&window, &decoded, 0, lines.len() as u32, start, rate, gaps);
        for (i, mut line) in next.into_iter().enumerate() {
            line.start_frame = line.start_frame.clamp(start, end);
            line.end_frame = line.end_frame.clamp(line.start_frame, end);
            line.start_seconds = line.start_frame as f64 / f64::from(rate);
            line.end_seconds = line.end_frame as f64 / f64::from(rate);
            line.utterance_id = format!("live-{}-{}-{i}", line.start_frame, line.end_frame);
            if line.end_frame > line.start_frame {
                lines.push(line);
            }
        }
    }
    lines.sort_by_key(|u| (u.start_frame, u.end_frame));
    Ok(lines)
}

pub(crate) fn migrate_live(tx: &rusqlite::Transaction<'_>) -> anyhow::Result<()> {
    tx.execute_batch(
        "CREATE TABLE review_live_transcripts (
            session_id TEXT PRIMARY KEY REFERENCES review_sessions(session_id),
            project_id TEXT NOT NULL, actor_json TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('live','refining','failed','stopped')),
            processed_end_frame INTEGER NOT NULL DEFAULT 0 CHECK(processed_end_frame>=0), revision INTEGER NOT NULL DEFAULT 0 CHECK(revision>=0),
            final_run_id TEXT, error_code TEXT, updated_at TEXT NOT NULL);
         CREATE TABLE review_live_utterances (
            session_id TEXT NOT NULL REFERENCES review_live_transcripts(session_id),
            utterance_id TEXT NOT NULL, start_frame INTEGER NOT NULL, end_frame INTEGER NOT NULL,
            payload_json TEXT NOT NULL, PRIMARY KEY(session_id, utterance_id));
         CREATE INDEX review_live_lines_time ON review_live_utterances(session_id, start_frame, end_frame);"
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "live_tests.rs"]
mod tests;
