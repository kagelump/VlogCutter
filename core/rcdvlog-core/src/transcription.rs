//! Explicit, ID-validated transcription batches for incremental preparation.
//!
//! The whole-project `transcribe_missing` command remains the compatibility
//! surface for a full sweep. This module adds the narrower operation an
//! incremental caller needs: select exact assets, reuse current fingerprinted
//! transcripts by default, and open the speech engine once for the batch.

use crate::analysis::engine::SttEngine;
use crate::analysis::store::{
    StoredTranscript, TranscriptStatus, is_current, load_transcript, save_transcript,
};
use crate::{Asset, AssetVideoValidationStatus, Catalog, ModelStatus, TranscribeOutcome};
use anyhow::{Context, Result, ensure};
use batch_estimator::BatchEstimator;
use rusqlite::OptionalExtension;
use serde::Serialize;
use std::collections::HashSet;
use std::ops::ControlFlow;
use std::time::Instant;

/// A bounded cap shared with the preparation scope. A caller that needs more
/// should split the work into explicit batches rather than hiding unbounded
/// work behind one request.
const MAX_SELECTED_ASSETS: usize = 10_000;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct TranscriptionSelectionError {
    pub code: &'static str,
    pub message: String,
}

fn selection_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    TranscriptionSelectionError {
        code,
        message: message.into(),
    }
    .into()
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct TranscriptionReadinessError {
    pub code: &'static str,
    pub message: String,
}

fn readiness_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    TranscriptionReadinessError {
        code,
        message: message.into(),
    }
    .into()
}

#[derive(Debug, Clone, Default)]
pub struct TranscriptionBatchOptions {
    /// Regenerate a current transcript. Without this flag, a transcript whose
    /// recipe and proxy fingerprint are current is reused and the engine is not
    /// invoked for that asset.
    pub force: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionAssetState {
    /// The engine produced a new transcript.
    Transcribed,
    /// A current fingerprinted transcript existed and was not regenerated.
    Reused,
    /// A pre-transcription prerequisite was absent or unusable. No engine work
    /// was attempted for this asset.
    NotReady,
    /// Engine or transcript persistence failed for this asset. Other selected
    /// assets may still have completed.
    Failed,
    /// The caller cancelled before this asset was processed.
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionAssetOutcome {
    pub asset_id: String,
    pub state: TranscriptionAssetState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript: Option<TranscribeOutcome>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionBatchProgress {
    /// Present for per-asset events; absent for the initial batch event.
    pub asset_id: Option<String>,
    /// `started` or `asset`.
    pub phase: String,
    pub completed: usize,
    pub total: usize,
    pub estimate: crate::BatchProgressEstimate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<TranscriptionAssetOutcome>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionBatchReport {
    pub project_id: String,
    pub requested: usize,
    pub transcribed: usize,
    pub reused: usize,
    pub not_ready: usize,
    pub failed: usize,
    pub cancelled: bool,
    /// Number of selected assets not processed because the callback stopped.
    pub cancelled_assets: usize,
    pub outcomes: Vec<TranscriptionAssetOutcome>,
}

enum PlannedAsset {
    Reused(TranscriptionAssetOutcome),
    NotReady(TranscriptionAssetOutcome),
    Pending { asset: Box<Asset> },
}

fn stored_outcome(asset_id: &str, stored: &StoredTranscript) -> TranscribeOutcome {
    TranscribeOutcome {
        asset_id: asset_id.to_string(),
        status: stored.status,
        segment_count: stored.segments.len(),
        window_count: stored.window_count,
        speech_seconds: stored.speech_seconds,
    }
}

fn outcome(
    asset_id: &str,
    state: TranscriptionAssetState,
    reason: Option<String>,
    transcript: Option<TranscribeOutcome>,
) -> TranscriptionAssetOutcome {
    TranscriptionAssetOutcome {
        asset_id: asset_id.to_string(),
        state,
        reason,
        transcript,
    }
}

impl Catalog {
    /// Transcribe an exact, project-owned asset selection.
    ///
    /// Validation is intentionally all-or-nothing: empty selections, duplicate
    /// IDs, unknown IDs, and IDs owned by another project fail before any
    /// transcript write, engine open, callback, or media work. Removed assets,
    /// videos not durably validated as usable, and assets without a reachable
    /// proxy become per-asset `not_ready` outcomes.
    ///
    /// A shared speech-model prerequisite is checked once before the engine is
    /// opened. Missing or unusable models make the whole run fail explicitly;
    /// this function never installs or downloads a model.
    pub fn transcribe_selected_with_progress<F>(
        &self,
        project_id: &str,
        asset_ids: &[String],
        options: &TranscriptionBatchOptions,
        on_progress: F,
    ) -> Result<TranscriptionBatchReport>
    where
        F: FnMut(TranscriptionBatchProgress) -> ControlFlow<()>,
    {
        self.transcribe_selected_inner(
            project_id,
            asset_ids,
            options,
            Some(|| self.model_status()),
            || self.open_stt_engine(),
            on_progress,
        )
    }

    fn transcribe_selected_inner<F, E, M>(
        &self,
        project_id: &str,
        asset_ids: &[String],
        options: &TranscriptionBatchOptions,
        model_preflight: Option<M>,
        open_engine: E,
        mut on_progress: F,
    ) -> Result<TranscriptionBatchReport>
    where
        F: FnMut(TranscriptionBatchProgress) -> ControlFlow<()>,
        E: FnOnce() -> Result<Box<dyn SttEngine>>,
        M: FnOnce() -> Result<Vec<ModelStatus>>,
    {
        ensure!(
            !self.read_only,
            "selected transcription requires an explicitly writable catalog"
        );
        let assets = self.validate_selected_assets(project_id, asset_ids)?;
        let connection = self.connection()?;
        let mut planned = Vec::with_capacity(assets.len());
        let mut pending_count = 0_usize;

        for asset in assets {
            if asset.removed {
                planned.push(PlannedAsset::NotReady(outcome(
                    &asset.id,
                    TranscriptionAssetState::NotReady,
                    Some("asset is excluded".into()),
                    None,
                )));
                continue;
            }
            let validation = self.asset_video_validation(&asset.id)?;
            if validation.status != AssetVideoValidationStatus::Usable {
                let diagnostic = validation
                    .diagnostic
                    .as_deref()
                    .unwrap_or("video has not been validated as usable");
                planned.push(PlannedAsset::NotReady(outcome(
                    &asset.id,
                    TranscriptionAssetState::NotReady,
                    Some(format!(
                        "video is not ready for transcription: {diagnostic}"
                    )),
                    None,
                )));
                continue;
            }
            let source_fingerprint = match self.proxy_fingerprint(&asset.id) {
                Ok(fingerprint) => fingerprint,
                Err(error) => {
                    planned.push(PlannedAsset::NotReady(outcome(
                        &asset.id,
                        TranscriptionAssetState::NotReady,
                        Some(format!("{error:#}")),
                        None,
                    )));
                    continue;
                }
            };
            if !options.force && is_current(&connection, &asset.id, &source_fingerprint)? {
                let stored = load_transcript(&connection, &asset.id)?
                    .context("current transcript disappeared during selected transcription")?;
                if stored.status != TranscriptStatus::Failed {
                    planned.push(PlannedAsset::Reused(outcome(
                        &asset.id,
                        TranscriptionAssetState::Reused,
                        None,
                        Some(stored_outcome(&asset.id, &stored)),
                    )));
                    continue;
                }
            }
            pending_count += 1;
            planned.push(PlannedAsset::Pending {
                asset: Box::new(asset),
            });
        }

        if pending_count > 0
            && let Some(check_models) = model_preflight
        {
            let models = check_models()?;
            let unavailable: Vec<String> = models
                .iter()
                .filter(|model| !model.ready)
                .map(|model| format!("{} ({})", model.name, model.integrity))
                .collect();
            if !unavailable.is_empty() {
                return Err(readiness_error(
                    "speech_models_not_ready",
                    format!(
                        "speech models are not ready: {}; run `rcdvlog models install` explicitly before transcribing",
                        unavailable.join(", ")
                    ),
                ));
            }
        }

        let total = planned.len();
        let task_work = vec![1; pending_count];
        let mut estimator = BatchEstimator::new(
            &task_work,
            1,
            self.load_batch_history(crate::analysis::STT_TASK_KEY)?,
        );
        let mut attempted = 0;
        let mut report = TranscriptionBatchReport {
            project_id: project_id.to_string(),
            requested: total,
            transcribed: 0,
            reused: 0,
            not_ready: 0,
            failed: 0,
            cancelled: false,
            cancelled_assets: 0,
            outcomes: Vec::with_capacity(total),
        };

        let start = on_progress(TranscriptionBatchProgress {
            asset_id: None,
            phase: "started".into(),
            completed: 0,
            total,
            estimate: estimator.estimate(&task_work),
            outcome: None,
        });
        if start.is_break() {
            report.cancelled = true;
            report.cancelled_assets = total;
            report.outcomes = planned
                .iter()
                .map(|planned| match planned {
                    PlannedAsset::Reused(item) | PlannedAsset::NotReady(item) => {
                        cancelled_outcome(&item.asset_id)
                    }
                    PlannedAsset::Pending { asset, .. } => cancelled_outcome(&asset.id),
                })
                .collect();
            return Ok(report);
        }

        let mut open_engine = Some(open_engine);
        let mut engine: Option<Box<dyn SttEngine>> = None;
        for (index, plan) in planned.iter().enumerate() {
            let item = match plan {
                PlannedAsset::Reused(item) => item.clone(),
                PlannedAsset::NotReady(item) => item.clone(),
                PlannedAsset::Pending { asset } => {
                    if engine.is_none() {
                        let open = open_engine.take().expect("engine factory is called once");
                        engine = Some(open()?);
                    }
                    let started = Instant::now();
                    let result = self.transcribe_with_engine(
                        &asset.id,
                        engine.as_deref().expect("engine opened for pending asset"),
                    );
                    match result {
                        Ok(transcript) => {
                            estimator.record_completion(1, started.elapsed());
                            report.transcribed += 1;
                            outcome(
                                &asset.id,
                                TranscriptionAssetState::Transcribed,
                                None,
                                Some(transcript),
                            )
                        }
                        Err(error) => {
                            estimator.record_failure(1);
                            let message = format!("{error:#}");
                            // Preserve an explicit per-asset failure record without
                            // claiming a usable transcript. Human-authored asset
                            // fields are never touched by transcript persistence.
                            save_transcript(
                                &connection,
                                &StoredTranscript {
                                    asset_id: asset.id.clone(),
                                    recipe_key: crate::analysis::STT_RECIPE_KEY.to_string(),
                                    // Match whole-project transcription: failed
                                    // attempts must remain stale and retryable.
                                    source_fingerprint: String::new(),
                                    status: TranscriptStatus::Failed,
                                    reject_reason: message.clone(),
                                    primary_language: String::new(),
                                    speech_seconds: 0.0,
                                    window_count: 0,
                                    segments: Vec::new(),
                                },
                            )?;
                            report.failed += 1;
                            outcome(
                                &asset.id,
                                TranscriptionAssetState::Failed,
                                Some(message),
                                None,
                            )
                        }
                    }
                }
            };

            if matches!(plan, PlannedAsset::Pending { .. }) {
                attempted += 1;
            }

            match item.state {
                TranscriptionAssetState::Reused => report.reused += 1,
                TranscriptionAssetState::NotReady => report.not_ready += 1,
                TranscriptionAssetState::Transcribed | TranscriptionAssetState::Failed => {}
                TranscriptionAssetState::Cancelled => {}
            }
            let completed = index + 1;
            let flow = on_progress(TranscriptionBatchProgress {
                asset_id: Some(item.asset_id.clone()),
                phase: "asset".into(),
                completed,
                total,
                estimate: estimator.estimate(&task_work[attempted..]),
                outcome: Some(item.clone()),
            });
            report.outcomes.push(item);
            if flow.is_break() {
                report.cancelled = true;
                for remaining in planned.iter().skip(completed) {
                    let asset_id = match remaining {
                        PlannedAsset::Reused(item) | PlannedAsset::NotReady(item) => {
                            item.asset_id.clone()
                        }
                        PlannedAsset::Pending { asset, .. } => asset.id.clone(),
                    };
                    report.outcomes.push(cancelled_outcome(&asset_id));
                }
                report.cancelled_assets = total.saturating_sub(completed);
                break;
            }
        }

        if attempted > 0 {
            self.save_batch_history(crate::analysis::STT_TASK_KEY, estimator.history())?;
        }
        Ok(report)
    }

    fn validate_selected_assets(
        &self,
        project_id: &str,
        asset_ids: &[String],
    ) -> Result<Vec<Asset>> {
        if asset_ids.is_empty() {
            return Err(selection_error(
                "empty_asset_selection",
                "selected transcription requires at least one --asset",
            ));
        }
        if asset_ids.len() > MAX_SELECTED_ASSETS {
            return Err(selection_error(
                "selection_too_large",
                format!(
                    "selected transcription is limited to {MAX_SELECTED_ASSETS} assets per batch"
                ),
            ));
        }
        let mut seen = HashSet::new();
        for asset_id in asset_ids {
            if !seen.insert(asset_id.as_str()) {
                return Err(selection_error(
                    "duplicate_asset_id",
                    format!("duplicate selected asset id: {asset_id}"),
                ));
            }
        }

        let connection = self.connection()?;
        let project_exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
            [project_id],
            |row| row.get(0),
        )?;
        if !project_exists {
            return Err(selection_error(
                "project_not_found",
                format!("selected transcription project does not exist: {project_id}"),
            ));
        }

        let mut unknown = Vec::new();
        let mut wrong_project = Vec::new();
        for asset_id in asset_ids {
            let owner: Option<String> = connection
                .query_row(
                    "SELECT project_id FROM assets WHERE id=?1",
                    [asset_id],
                    |row| row.get(0),
                )
                .optional()?;
            match owner.as_deref() {
                None => unknown.push(asset_id.as_str()),
                Some(owner) if owner != project_id => wrong_project.push(asset_id.as_str()),
                Some(_) => {}
            }
        }
        if !unknown.is_empty() {
            return Err(selection_error(
                "unknown_asset_id",
                format!("unknown selected asset id(s): {}", unknown.join(", ")),
            ));
        }
        if !wrong_project.is_empty() {
            return Err(selection_error(
                "asset_project_mismatch",
                format!(
                    "selected asset id(s) do not belong to project {project_id}: {}",
                    wrong_project.join(", ")
                ),
            ));
        }

        asset_ids
            .iter()
            .map(|asset_id| self.get_asset(asset_id))
            .collect()
    }
}

fn cancelled_outcome(asset_id: &str) -> TranscriptionAssetOutcome {
    outcome(
        asset_id,
        TranscriptionAssetState::Cancelled,
        Some("cancelled before this asset was processed".into()),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::engine::SttEngine;
    use crate::analysis::quality::DecodedSegment;
    use crate::analysis::windows::{PackedWindow, SpeechRegion};
    use crate::{AssetPatch, ModelStatus};
    use rusqlite::params;
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::{TempDir, tempdir};

    fn fixture() -> (TempDir, Catalog, String) {
        let directory = tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let project = catalog
            .create_project("Selected transcription fixture")
            .unwrap();
        (directory, catalog, project.id)
    }

    fn media_tools_available() -> bool {
        ["ffmpeg", "ffprobe"].into_iter().all(|tool| {
            crate::tool_command(tool)
                .arg("-version")
                .output()
                .is_ok_and(|output| output.status.success())
        })
    }

    fn add_placeholder_asset(catalog: &Catalog, project_id: &str, directory: &Path, id: &str) {
        let source = directory.join(format!("{id}.mp4"));
        fs::write(&source, format!("placeholder {id}")).unwrap();
        crate::tests::insert_asset_at(catalog, project_id, id, &source);
    }

    fn make_ready_asset(catalog: &Catalog, project_id: &str, directory: &Path, id: &str) {
        let source = directory.join(format!("{id}.mp4"));
        let color = &blake3::hash(id.as_bytes()).to_hex()[..6];
        let status = crate::tool_command("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
            ])
            .arg(format!("color=c=0x{color}:s=160x90:r=24:d=1"))
            .args(["-f", "lavfi", "-i"])
            .arg("sine=frequency=440:sample_rate=48000:duration=1")
            .args([
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(&source)
            .status()
            .expect("run ffmpeg for selected transcription fixture");
        assert!(status.success(), "could not create synthetic H.264 fixture");
        crate::tests::insert_asset_at(catalog, project_id, id, &source);
        catalog
            .connection()
            .unwrap()
            .execute(
                "UPDATE assets SET duration_seconds=1,source_out=1,frame_rate=24,width=160,height=90,orientation_probed=1 WHERE id=?1",
                params![id],
            )
            .unwrap();
        catalog.validate_asset_video(id).unwrap();
        catalog.generate_proxy(id).unwrap();
    }

    #[derive(Clone)]
    struct CountingEngine {
        detect_calls: Arc<AtomicUsize>,
    }

    impl SttEngine for CountingEngine {
        fn detect_speech(&self, _pcm: &[f32]) -> Result<Vec<SpeechRegion>> {
            self.detect_calls.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }

        fn decode_window(
            &self,
            _pcm: &[f32],
            _window: &PackedWindow,
        ) -> Result<Vec<DecodedSegment>> {
            unreachable!("empty VAD output must never call the decoder")
        }
    }

    fn selected(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_string()).collect()
    }

    fn selection_code(error: &anyhow::Error) -> Option<&'static str> {
        error
            .downcast_ref::<TranscriptionSelectionError>()
            .map(|error| error.code)
    }

    #[test]
    fn invalid_selections_fail_before_any_engine_or_callback_work() {
        let (directory, catalog, project_id) = fixture();
        let other_project = catalog.create_project("Other project").unwrap();
        add_placeholder_asset(&catalog, &project_id, directory.path(), "mine");
        add_placeholder_asset(&catalog, &other_project.id, directory.path(), "not-mine");
        let opens = Arc::new(AtomicUsize::new(0));
        let callbacks = Arc::new(AtomicUsize::new(0));

        let cases: Vec<(Vec<String>, &str)> = vec![
            (Vec::new(), "empty_asset_selection"),
            (selected(&["mine", "mine"]), "duplicate_asset_id"),
            (selected(&["missing"]), "unknown_asset_id"),
            (selected(&["not-mine"]), "asset_project_mismatch"),
        ];
        for (assets, expected_code) in cases {
            let open_count = opens.clone();
            let callback_count = callbacks.clone();
            let error = catalog
                .transcribe_selected_inner(
                    &project_id,
                    &assets,
                    &TranscriptionBatchOptions::default(),
                    None::<fn() -> Result<Vec<ModelStatus>>>,
                    move || {
                        open_count.fetch_add(1, Ordering::SeqCst);
                        Ok(Box::new(CountingEngine {
                            detect_calls: Arc::new(AtomicUsize::new(0)),
                        }) as Box<dyn SttEngine>)
                    },
                    move |_| {
                        callback_count.fetch_add(1, Ordering::SeqCst);
                        ControlFlow::Continue(())
                    },
                )
                .unwrap_err();
            assert_eq!(selection_code(&error), Some(expected_code), "{error:#}");
        }
        assert_eq!(opens.load(Ordering::SeqCst), 0);
        assert_eq!(
            callbacks.load(Ordering::SeqCst),
            0,
            "invalid selection must not emit progress"
        );
    }

    #[test]
    fn ready_selection_opens_one_engine_and_reuses_then_forces_current_transcripts() {
        if !media_tools_available() {
            eprintln!(
                "skipping generated selected-transcription fixture: ffmpeg/ffprobe unavailable"
            );
            return;
        }
        let (directory, catalog, project_id) = fixture();
        for id in ["clip-b", "clip-a", "unrelated"] {
            make_ready_asset(&catalog, &project_id, directory.path(), id);
        }
        catalog
            .connection()
            .unwrap()
            .execute(
                "UPDATE assets SET notes='human note', title='human title', description='human description', labels_json='[\"human\"]' WHERE id='clip-a'",
                [],
            )
            .unwrap();
        let opens = Arc::new(AtomicUsize::new(0));
        let detects = Arc::new(AtomicUsize::new(0));
        let picks = selected(&["clip-b", "clip-a"]);

        let run = |force: bool| {
            let open_count = opens.clone();
            let detect_count = detects.clone();
            catalog
                .transcribe_selected_inner(
                    &project_id,
                    &picks,
                    &TranscriptionBatchOptions { force },
                    None::<fn() -> Result<Vec<ModelStatus>>>,
                    move || {
                        open_count.fetch_add(1, Ordering::SeqCst);
                        Ok(Box::new(CountingEngine {
                            detect_calls: detect_count,
                        }) as Box<dyn SttEngine>)
                    },
                    |_| ControlFlow::Continue(()),
                )
                .unwrap()
        };

        let first = run(false);
        assert_eq!(first.transcribed, 2);
        assert_eq!(first.reused, 0);
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        assert_eq!(detects.load(Ordering::SeqCst), 2);
        assert_eq!(
            first
                .outcomes
                .iter()
                .map(|outcome| outcome.asset_id.as_str())
                .collect::<Vec<_>>(),
            vec!["clip-b", "clip-a"],
            "selection order and exact membership must be preserved"
        );
        assert!(
            catalog.asset_transcript("unrelated").unwrap().is_none(),
            "an unselected asset must not receive transcript work"
        );

        let second = run(false);
        assert_eq!(second.reused, 2);
        assert_eq!(second.transcribed, 0);
        assert_eq!(
            opens.load(Ordering::SeqCst),
            1,
            "a fully reused batch must not open the engine"
        );
        assert_eq!(detects.load(Ordering::SeqCst), 2);

        let forced = run(true);
        assert_eq!(forced.transcribed, 2);
        assert_eq!(opens.load(Ordering::SeqCst), 2);
        assert_eq!(detects.load(Ordering::SeqCst), 4);

        let asset = catalog.get_asset("clip-a").unwrap();
        assert_eq!(asset.notes, "human note");
        assert_eq!(asset.title, "human title");
        assert_eq!(asset.description, "human description");
        assert_eq!(asset.labels, vec!["human"]);
    }

    #[test]
    fn missing_asset_prerequisites_are_not_ready_and_missing_models_fail_explicitly() {
        if !media_tools_available() {
            eprintln!("skipping generated readiness fixture: ffmpeg/ffprobe unavailable");
            return;
        }
        let (directory, catalog, project_id) = fixture();
        // Original-only asset: it validates as video but has no proxy.
        let source = directory.path().join("no-proxy.mp4");
        let status = crate::tool_command("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=160x90:r=24:d=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=1",
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(&source)
            .status()
            .unwrap();
        assert!(status.success());
        crate::tests::insert_asset_at(&catalog, &project_id, "no-proxy", &source);
        catalog
            .connection()
            .unwrap()
            .execute(
                "UPDATE assets SET duration_seconds=1,source_out=1,frame_rate=24,width=160,height=90,orientation_probed=1 WHERE id='no-proxy'",
                [],
            )
            .unwrap();
        catalog.validate_asset_video("no-proxy").unwrap();

        let opens = Arc::new(AtomicUsize::new(0));
        let open_count = opens.clone();
        let report = catalog
            .transcribe_selected_inner(
                &project_id,
                &selected(&["no-proxy"]),
                &TranscriptionBatchOptions::default(),
                None::<fn() -> Result<Vec<ModelStatus>>>,
                move || {
                    open_count.fetch_add(1, Ordering::SeqCst);
                    unreachable!("not-ready work must not open the engine")
                },
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        assert_eq!(report.not_ready, 1);
        assert_eq!(report.outcomes[0].state, TranscriptionAssetState::NotReady);
        assert_eq!(opens.load(Ordering::SeqCst), 0);

        make_ready_asset(&catalog, &project_id, directory.path(), "ready");
        let open_count = opens.clone();
        let error = catalog
            .transcribe_selected_inner(
                &project_id,
                &selected(&["ready"]),
                &TranscriptionBatchOptions::default(),
                Some(|| {
                    Ok(vec![ModelStatus {
                        name: "whisper".into(),
                        role: "speech_to_text".into(),
                        installed: false,
                        ready: false,
                        present: false,
                        integrity: "missing".into(),
                        detail: Some("missing".into()),
                        bytes: 1,
                        url: String::new(),
                        directory: "/models".into(),
                        path: "/models/whisper".into(),
                        expected_blake3: None,
                    }])
                }),
                move || {
                    open_count.fetch_add(1, Ordering::SeqCst);
                    unreachable!("model readiness must fail before engine initialization")
                },
                |_| ControlFlow::Continue(()),
            )
            .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<TranscriptionReadinessError>()
                .map(|error| error.code),
            Some("speech_models_not_ready")
        );
        assert_eq!(opens.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cancellation_stops_after_the_current_asset_and_reports_bounded_progress() {
        if !media_tools_available() {
            eprintln!("skipping generated cancellation fixture: ffmpeg/ffprobe unavailable");
            return;
        }
        let (directory, catalog, project_id) = fixture();
        for id in ["clip-a", "clip-b", "clip-c"] {
            make_ready_asset(&catalog, &project_id, directory.path(), id);
        }
        let opens = Arc::new(AtomicUsize::new(0));
        let detects = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(AtomicUsize::new(0));
        let open_count = opens.clone();
        let detect_count = detects.clone();
        let event_count = events.clone();
        let report = catalog
            .transcribe_selected_inner(
                &project_id,
                &selected(&["clip-a", "clip-b", "clip-c"]),
                &TranscriptionBatchOptions::default(),
                None::<fn() -> Result<Vec<ModelStatus>>>,
                move || {
                    open_count.fetch_add(1, Ordering::SeqCst);
                    Ok(Box::new(CountingEngine {
                        detect_calls: detect_count,
                    }) as Box<dyn SttEngine>)
                },
                move |progress| {
                    event_count.fetch_add(1, Ordering::SeqCst);
                    if progress.phase == "asset" && progress.completed == 1 {
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                },
            )
            .unwrap();
        assert!(report.cancelled);
        assert_eq!(report.transcribed, 1);
        assert_eq!(report.cancelled_assets, 2);
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        assert_eq!(detects.load(Ordering::SeqCst), 1);
        assert_eq!(events.load(Ordering::SeqCst), 2, "start plus one outcome");
        assert_eq!(
            report
                .outcomes
                .iter()
                .map(|outcome| outcome.state)
                .collect::<Vec<_>>(),
            vec![
                TranscriptionAssetState::Transcribed,
                TranscriptionAssetState::Cancelled,
                TranscriptionAssetState::Cancelled
            ]
        );
        assert!(catalog.asset_transcript("clip-b").unwrap().is_none());
        assert!(catalog.asset_transcript("clip-c").unwrap().is_none());
    }

    #[test]
    fn force_preserves_human_fields_even_when_transcript_storage_replaces_derived_rows() {
        if !media_tools_available() {
            eprintln!("skipping generated annotation fixture: ffmpeg/ffprobe unavailable");
            return;
        }
        let (directory, catalog, project_id) = fixture();
        make_ready_asset(&catalog, &project_id, directory.path(), "clip");
        let before = catalog.get_asset("clip").unwrap();
        let patch = AssetPatch {
            notes: "typed by a human".into(),
            labels: vec!["arrival".into()],
            removed: false,
            rotation_degrees: before.rotation_degrees,
            expected_revision: Some(before.revision),
        };
        catalog.update_asset("clip", &patch).unwrap();
        catalog
            .transcribe_selected_inner(
                &project_id,
                &selected(&["clip"]),
                &TranscriptionBatchOptions { force: true },
                None::<fn() -> Result<Vec<ModelStatus>>>,
                || {
                    Ok(Box::new(CountingEngine {
                        detect_calls: Arc::new(AtomicUsize::new(0)),
                    }) as Box<dyn SttEngine>)
                },
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        let after = catalog.get_asset("clip").unwrap();
        assert_eq!(after.notes, "typed by a human");
        assert_eq!(after.labels, vec!["arrival"]);
        assert!(after.revision > before.revision);
    }

    #[test]
    fn a_failed_asset_does_not_stop_the_batch_and_is_retried() {
        if !media_tools_available() {
            eprintln!("skipping generated retry fixture: ffmpeg/ffprobe unavailable");
            return;
        }
        struct FailsOnceEngine(Arc<AtomicUsize>);
        impl SttEngine for FailsOnceEngine {
            fn detect_speech(&self, _pcm: &[f32]) -> Result<Vec<SpeechRegion>> {
                if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                    anyhow::bail!("synthetic transient engine failure");
                }
                Ok(Vec::new())
            }
            fn decode_window(&self, _: &[f32], _: &PackedWindow) -> Result<Vec<DecodedSegment>> {
                unreachable!("no speech")
            }
        }
        let (directory, catalog, project) = fixture();
        for id in ["retry", "ready"] {
            make_ready_asset(&catalog, &project, directory.path(), id);
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let run = || {
            catalog
                .transcribe_selected_inner(
                    &project,
                    &selected(&["retry", "ready"]),
                    &TranscriptionBatchOptions::default(),
                    None::<fn() -> Result<Vec<ModelStatus>>>,
                    || Ok(Box::new(FailsOnceEngine(calls.clone())) as Box<dyn SttEngine>),
                    |_| ControlFlow::Continue(()),
                )
                .unwrap()
        };
        let first = run();
        assert_eq!((first.failed, first.transcribed, first.reused), (1, 1, 0));
        assert!(
            first.outcomes[0]
                .reason
                .as_deref()
                .unwrap()
                .contains("transient")
        );
        assert!(
            !is_current(
                &catalog.connection().unwrap(),
                "retry",
                &catalog.proxy_fingerprint("retry").unwrap()
            )
            .unwrap()
        );
        // Also recover failed rows written by a caller that kept the current
        // fingerprint: recipe identity alone must not make failure reusable.
        let mut stored = catalog.asset_transcript("retry").unwrap().unwrap();
        stored.source_fingerprint = catalog.proxy_fingerprint("retry").unwrap();
        save_transcript(&catalog.connection().unwrap(), &stored).unwrap();
        let second = run();
        assert_eq!(
            (second.failed, second.transcribed, second.reused),
            (0, 1, 1)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(
            catalog.asset_transcript("retry").unwrap().unwrap().status,
            TranscriptStatus::NoSpeech
        );
    }

    #[test]
    fn cancelling_at_start_does_not_initialize_the_engine_or_write_transcripts() {
        if !media_tools_available() {
            eprintln!("skipping generated start-cancel fixture: ffmpeg/ffprobe unavailable");
            return;
        }
        let (directory, catalog, project) = fixture();
        make_ready_asset(&catalog, &project, directory.path(), "ready");
        let report = catalog
            .transcribe_selected_inner(
                &project,
                &selected(&["ready"]),
                &TranscriptionBatchOptions::default(),
                None::<fn() -> Result<Vec<ModelStatus>>>,
                || panic!("cancelled batch must not initialize the engine"),
                |_| ControlFlow::Break(()),
            )
            .unwrap();
        assert!(report.cancelled);
        assert_eq!(report.cancelled_assets, 1);
        assert_eq!(report.outcomes[0].state, TranscriptionAssetState::Cancelled);
        assert!(catalog.asset_transcript("ready").unwrap().is_none());
    }
}
