//! Explicit, ID-validated transcription batches for incremental preparation.
//!
//! The whole-project `transcribe_missing` command remains the compatibility
//! surface for a full sweep. This module adds the narrower operation an
//! incremental caller needs: select exact assets, reuse current fingerprinted
//! transcripts by default, and open the speech engine once for the batch.

use crate::analysis::engine::SttEngine;
use crate::analysis::store::{
    StoredTranscript, is_current_local, load_transcript, save_transcript,
};
use crate::analysis::stt::{
    failed_transcript, failed_transcript_with_evidence, failure_evidence, no_audio_transcript,
};
use crate::{
    Asset, AssetVideoValidationStatus, Catalog, ModelStatus, RepresentationKind, TranscribeOutcome,
};
use anyhow::{Context, Result, ensure};

#[allow(dead_code)] // Staged local owner for the next native orchestration slice.
mod native_preparation;
use batch_estimator::BatchEstimator;
#[allow(unused_imports)] // Consumed by the next native orchestration slice and focused tests.
pub(crate) use native_preparation::{
    NativeSpeechLocalOutcome, NativeSpeechPreparation, NativeSpeechPreparationCanceled,
    NativeSpeechPreparationEvidence, NativeSpeechPreparer, NativeVadEvidence,
};
use rusqlite::OptionalExtension;
use serde::Serialize;
use std::collections::HashSet;
use std::ops::ControlFlow;
use std::path::PathBuf;
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
    /// A valid video had no audio stream. This is a successful terminal result;
    /// no VAD, model, or provider work was attempted.
    NoAudio,
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
    /// `probing_audio`, `started`, `waiting_for_resource`, or `asset`.
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
    /// Subset of `transcribed` that completed without an audio stream.
    pub no_audio: usize,
    pub reused: usize,
    pub not_ready: usize,
    pub failed: usize,
    pub cancelled: bool,
    /// Number of selected assets not processed because the callback stopped.
    pub cancelled_assets: usize,
    /// Actual engine/resource provenance for this batch. Explicit overrides
    /// remain explicit; they are not relabelled as calibrated.
    pub resource_source: String,
    pub calibration_key: Option<String>,
    pub vad_threads: usize,
    pub decode_threads: usize,
    pub worker_count: usize,
    pub history_key: String,
    pub sample_compatibility: String,
    pub outcomes: Vec<TranscriptionAssetOutcome>,
}

enum PlannedAsset {
    Reused(TranscriptionAssetOutcome),
    NotReady(TranscriptionAssetOutcome),
    NoAudio {
        asset: Box<Asset>,
        source_fingerprint: String,
    },
    ProbeFailed {
        asset: Box<Asset>,
        source_fingerprint: String,
        reason: String,
    },
    Pending {
        asset: Box<Asset>,
        proxy_path: PathBuf,
        source_fingerprint: String,
    },
}

fn stored_outcome(asset_id: &str, stored: &StoredTranscript) -> TranscribeOutcome {
    TranscribeOutcome {
        asset_id: asset_id.to_string(),
        status: stored.status,
        outcome_reason: stored.outcome_reason,
        source_fingerprint: stored.source_fingerprint.clone(),
        segment_count: stored.segments.len(),
        window_count: stored.window_count,
        speech_seconds: stored.speech_seconds,
        provider_segment_count: stored.provider_segment_count,
        mean_token_probability: stored.mean_token_probability,
        repetition_ratio: stored.repetition_ratio,
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
        self.transcribe_selected_with_resource_overrides(
            project_id,
            asset_ids,
            options,
            &crate::ResourceOverrides::default(),
            on_progress,
        )
    }

    /// Transcribe an exact selection while making explicit VAD/decode/worker
    /// overrides visible to the caller. Bounded validation happens before the
    /// heavy slot, engine open, or any transcript write.
    pub fn transcribe_selected_with_resource_overrides<F>(
        &self,
        project_id: &str,
        asset_ids: &[String],
        options: &TranscriptionBatchOptions,
        overrides: &crate::ResourceOverrides,
        on_progress: F,
    ) -> Result<TranscriptionBatchReport>
    where
        F: FnMut(TranscriptionBatchProgress) -> ControlFlow<()>,
    {
        let selection = self.resolve_resource_selection(
            crate::SPEECH_REPRESENTATION_KIND,
            crate::STT_RECIPE_KEY,
            None,
            None,
            overrides,
        )?;
        let history_key = selection.history_key.clone();
        self.transcribe_selected_inner_with_resource(
            project_id,
            asset_ids,
            options,
            selection.vad_threads,
            selection.decode_threads,
            selection.worker_count.max(1),
            &selection.source,
            selection.calibration_key.as_deref(),
            &selection.sample_compatibility,
            &history_key,
            Some(|| self.model_status()),
            || self.open_stt_engine_with_threads(selection.vad_threads, selection.decode_threads),
            on_progress,
        )
    }

    /// Test/fixture seam retaining uncalibrated default resource identity.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    fn transcribe_selected_inner<F, E, M>(
        &self,
        project_id: &str,
        asset_ids: &[String],
        options: &TranscriptionBatchOptions,
        model_preflight: Option<M>,
        open_engine: E,
        on_progress: F,
    ) -> Result<TranscriptionBatchReport>
    where
        F: FnMut(TranscriptionBatchProgress) -> ControlFlow<()>,
        E: FnOnce() -> Result<Box<dyn SttEngine>>,
        M: FnOnce() -> Result<Vec<ModelStatus>>,
    {
        self.transcribe_selected_inner_with_resource(
            project_id,
            asset_ids,
            options,
            4,
            4,
            1,
            "default_uncalibrated",
            None,
            "unknown",
            crate::analysis::STT_TASK_KEY,
            model_preflight,
            open_engine,
            on_progress,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn transcribe_selected_inner_with_resource<F, E, M>(
        &self,
        project_id: &str,
        asset_ids: &[String],
        options: &TranscriptionBatchOptions,
        vad_threads: usize,
        decode_threads: usize,
        worker_count: usize,
        resource_source: &str,
        calibration_key: Option<&str>,
        sample_compatibility: &str,
        history_key: &str,
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
        let total = assets.len();
        let mut report = TranscriptionBatchReport {
            project_id: project_id.to_string(),
            requested: total,
            transcribed: 0,
            no_audio: 0,
            reused: 0,
            not_ready: 0,
            failed: 0,
            cancelled: false,
            cancelled_assets: 0,
            resource_source: resource_source.to_string(),
            calibration_key: calibration_key.map(str::to_string),
            vad_threads,
            decode_threads,
            worker_count: worker_count.max(1),
            history_key: history_key.to_string(),
            sample_compatibility: sample_compatibility.to_string(),
            outcomes: Vec::with_capacity(total),
        };
        let probe_estimator =
            BatchEstimator::new(&vec![1; total], worker_count.max(1), Default::default());
        let connection = self.connection()?;
        let mut planned = Vec::with_capacity(assets.len());
        let mut pending_count = 0_usize;

        for asset in assets {
            // Check cancellation before every bounded FFprobe call, including
            // before the first one. Planning does not commit any outcomes.
            let mut estimate = probe_estimator.estimate(&[]);
            estimate.estimated_remaining_seconds = None;
            estimate.estimated_remaining_seconds_range = None;
            estimate.confidence = "inspecting_audio".into();
            if on_progress(TranscriptionBatchProgress {
                asset_id: Some(asset.id.clone()),
                phase: "probing_audio".into(),
                completed: 0,
                total,
                estimate,
                outcome: None,
            })
            .is_break()
            {
                report.cancelled = true;
                report.cancelled_assets = total;
                report.outcomes = asset_ids.iter().map(|id| cancelled_outcome(id)).collect();
                return Ok(report);
            }
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
            if !options.force && is_current_local(&connection, &asset.id, &source_fingerprint)? {
                let stored = load_transcript(&connection, &asset.id)?
                    .context("current transcript disappeared during selected transcription")?;
                planned.push(PlannedAsset::Reused(outcome(
                    &asset.id,
                    TranscriptionAssetState::Reused,
                    None,
                    Some(stored_outcome(&asset.id, &stored)),
                )));
                continue;
            }
            let proxy_path = match Self::require_proxy(&asset) {
                Ok(path) => path,
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
            match self.transcription_audio_present(&proxy_path) {
                Ok(false) => planned.push(PlannedAsset::NoAudio {
                    asset: Box::new(asset),
                    source_fingerprint,
                }),
                Ok(true) => {
                    pending_count += 1;
                    planned.push(PlannedAsset::Pending {
                        asset: Box::new(asset),
                        proxy_path,
                        source_fingerprint,
                    });
                }
                Err(error) => planned.push(PlannedAsset::ProbeFailed {
                    asset: Box::new(asset),
                    source_fingerprint,
                    reason: format!("probe audio before transcription: {error:#}"),
                }),
            }
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

        let task_work = vec![1; pending_count];
        let mut estimator = BatchEstimator::new(
            &task_work,
            worker_count.max(1),
            self.load_batch_history(history_key)?,
        );
        let mut attempted = 0;

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
                    PlannedAsset::NoAudio { asset, .. }
                    | PlannedAsset::ProbeFailed { asset, .. }
                    | PlannedAsset::Pending { asset, .. } => cancelled_outcome(&asset.id),
                })
                .collect();
            return Ok(report);
        }

        let _heavy = if pending_count > 0 {
            match self.wait_for_local_heavy(|| {
                let mut estimate = estimator.estimate(&task_work);
                estimate.confidence = "waiting_for_resource".into();
                estimate.estimated_remaining_seconds = None;
                on_progress(TranscriptionBatchProgress {
                    asset_id: None,
                    phase: "waiting_for_resource".into(),
                    completed: 0,
                    total,
                    estimate,
                    outcome: None,
                })
                .is_break()
            })? {
                Some(lease) => Some(lease),
                None => {
                    report.cancelled = true;
                    report.cancelled_assets = total;
                    report.outcomes = planned
                        .iter()
                        .map(|plan| match plan {
                            PlannedAsset::Reused(item) | PlannedAsset::NotReady(item) => {
                                cancelled_outcome(&item.asset_id)
                            }
                            PlannedAsset::NoAudio { asset, .. }
                            | PlannedAsset::ProbeFailed { asset, .. }
                            | PlannedAsset::Pending { asset, .. } => cancelled_outcome(&asset.id),
                        })
                        .collect();
                    return Ok(report);
                }
            }
        } else {
            None
        };
        let mut open_engine = Some(open_engine);
        let mut engine: Option<Box<dyn SttEngine>> = None;
        for (index, plan) in planned.iter().enumerate() {
            let planned_asset_id = match plan {
                PlannedAsset::Reused(item) | PlannedAsset::NotReady(item) => item.asset_id.as_str(),
                PlannedAsset::NoAudio { asset, .. }
                | PlannedAsset::ProbeFailed { asset, .. }
                | PlannedAsset::Pending { asset, .. } => asset.id.as_str(),
            };
            let writes_speech =
                !matches!(plan, PlannedAsset::Reused(_) | PlannedAsset::NotReady(_));
            // This is a nonblocking per-asset speech write claim. A prepared
            // native result owns the same claim until its caller is done, so a
            // local batch cannot overwrite that work even though the shared
            // heavy lane is held for the batch.
            let _speech_claim = if writes_speech {
                self.try_claim_representation(planned_asset_id, RepresentationKind::Speech)?
            } else {
                None
            };
            let item = if writes_speech && _speech_claim.is_none() {
                outcome(
                    planned_asset_id,
                    TranscriptionAssetState::NotReady,
                    Some("speech transcription is already in progress for this asset".into()),
                    None,
                )
            } else {
                match plan {
                    PlannedAsset::Reused(item) => item.clone(),
                    PlannedAsset::NotReady(item) => item.clone(),
                    PlannedAsset::NoAudio {
                        asset,
                        source_fingerprint,
                    } => {
                        let stored = no_audio_transcript(&asset.id, source_fingerprint);
                        save_transcript(&connection, &stored)?;
                        report.transcribed += 1;
                        report.no_audio += 1;
                        outcome(
                            &asset.id,
                            TranscriptionAssetState::NoAudio,
                            Some("video has no audio stream; no speech work was attempted".into()),
                            Some(stored_outcome(&asset.id, &stored)),
                        )
                    }
                    PlannedAsset::ProbeFailed {
                        asset,
                        source_fingerprint,
                        reason,
                    } => {
                        let stored =
                            failed_transcript(&asset.id, source_fingerprint, reason.clone());
                        save_transcript(&connection, &stored)?;
                        report.failed += 1;
                        outcome(
                            &asset.id,
                            TranscriptionAssetState::Failed,
                            Some(reason.to_string()),
                            Some(stored_outcome(&asset.id, &stored)),
                        )
                    }
                    PlannedAsset::Pending {
                        asset,
                        proxy_path,
                        source_fingerprint,
                    } => {
                        if engine.is_none() {
                            let open = open_engine.take().expect("engine factory is called once");
                            engine = Some(open()?);
                        }
                        let started = Instant::now();
                        let result = self.transcribe_source_with_engine(
                            &asset.id,
                            proxy_path,
                            source_fingerprint,
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
                                // claiming a usable transcript. The exact source
                                // fingerprint stays attached for recovery; is_current
                                // still treats failed attempts as retryable. Human-
                                // authored asset fields are never touched here.
                                let (speech_seconds, window_count, provider_segment_count) =
                                    failure_evidence(&error);
                                let stored = failed_transcript_with_evidence(
                                    &asset.id,
                                    source_fingerprint,
                                    message.clone(),
                                    speech_seconds,
                                    window_count,
                                    provider_segment_count,
                                );
                                save_transcript(&connection, &stored)?;
                                report.failed += 1;
                                outcome(
                                    &asset.id,
                                    TranscriptionAssetState::Failed,
                                    Some(message),
                                    Some(stored_outcome(&asset.id, &stored)),
                                )
                            }
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
                TranscriptionAssetState::Transcribed
                | TranscriptionAssetState::NoAudio
                | TranscriptionAssetState::Failed
                | TranscriptionAssetState::Cancelled => {}
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
                        PlannedAsset::NoAudio { asset, .. }
                        | PlannedAsset::ProbeFailed { asset, .. }
                        | PlannedAsset::Pending { asset, .. } => asset.id.clone(),
                    };
                    report.outcomes.push(cancelled_outcome(&asset_id));
                }
                report.cancelled_assets = total.saturating_sub(completed);
                break;
            }
        }

        if attempted > 0 {
            self.save_batch_history(history_key, estimator.history())?;
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
#[path = "transcription_tests.rs"]
mod tests;
