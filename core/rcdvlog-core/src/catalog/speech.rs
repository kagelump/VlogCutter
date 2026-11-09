//! Speech model management and transcription batches.

use crate::analysis::audio::{AudioExtractor, FfmpegAudioExtractor};
use crate::analysis::engine::{SttEngine, VAD_MODEL, WHISPER_MODEL, ensure_model, model_dir};
use crate::analysis::store::{StoredTranscript, TranscriptCurrentState, save_transcript};
use crate::analysis::stt::transcribe_pcm;
use crate::analysis::stt::{
    failed_transcript, failed_transcript_with_evidence, failure_evidence, no_audio_transcript,
};
use crate::analysis::whisper_engine::{WhisperEngine, read_pcm};
use crate::*;
use anyhow::{Context, Result, anyhow, bail};
use batch_estimator::BatchEstimator;
use std::fs;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::Instant;
use uuid::Uuid;

pub(crate) fn transcribe_outcome(asset_id: &str, stored: &StoredTranscript) -> TranscribeOutcome {
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

pub(crate) enum ProjectPlannedAsset {
    NoAudio {
        asset: Asset,
        source_fingerprint: String,
    },
    ProbeFailed {
        asset: Asset,
        source_fingerprint: String,
        reason: String,
    },
    /// Existing native work cannot be proved current (incomplete paid prefix,
    /// stale source/recipe/quality, unproved history or foreign ownership).
    /// Generic missing must surface it for review/recovery and leave every
    /// canonical header/segment/window/accounting link untouched.
    NativeReviewRecovery { asset: Asset, reason: String },
    Ready {
        asset: Asset,
        proxy_path: PathBuf,
        source_fingerprint: String,
    },
}

impl Catalog {
    /// Reports whether each speech model is installed for this catalog.
    ///
    /// Read-only and offline: it never downloads and never creates directories.
    pub fn model_status(&self) -> Result<Vec<ModelStatus>> {
        speech_model_status(&self.database_path)
    }

    /// Reports readiness for an explicit backend selection without opening a
    /// second catalog, downloading, or loading an inference engine.
    pub fn model_status_for(&self, selection: &SpeechBackendSelection) -> Result<Vec<ModelStatus>> {
        speech_model_status_for(&self.database_path, selection)
    }

    /// Downloads whichever speech models are missing.
    ///
    /// This is the only path that fetches a model. It exists so installation is
    /// an explicit user action; no batch calls it. The legacy return type keeps
    /// desktop compatibility; use `install_speech_models_with_progress` when the
    /// caller needs an explicit cancellation outcome.
    pub fn install_models_with_progress<F>(&self, on_progress: F) -> Result<Vec<ModelStatus>>
    where
        F: FnMut(ModelInstallProgress) -> ControlFlow<()>,
    {
        let report = install_speech_models_with_progress(&self.database_path, on_progress)?;
        if report.cancelled {
            bail!("model download cancelled");
        }
        Ok(report.models)
    }

    /// Explicitly download only the artifacts required by the selected backend.
    ///
    /// This is a thin wrapper over the owning core operation, kept alongside the
    /// historical local-only method for callers that already hold a Catalog.
    pub fn install_models_with_progress_for<F>(
        &self,
        selection: &SpeechBackendSelection,
        on_progress: F,
    ) -> Result<Vec<ModelStatus>>
    where
        F: FnMut(ModelInstallProgress) -> ControlFlow<()>,
    {
        let report =
            install_speech_models_with_progress_for(&self.database_path, selection, on_progress)?;
        if report.cancelled {
            bail!("model download cancelled");
        }
        Ok(report.models)
    }

    /// Transcribes one clip, replacing any previous transcript.
    pub fn transcribe_asset(&self, asset_id: &str) -> Result<TranscribeOutcome> {
        self.transcribe_asset_with_resource_overrides(asset_id, &ResourceOverrides::default())
    }

    /// Transcribe one clip with explicit, validated resource overrides. An
    /// override is provenance-bearing and never presented as calibrated.
    ///
    /// Audio presence is probed before the heavy slot or speech engine is
    /// acquired. A valid clip with no audio therefore completes as `no_audio`
    /// without initializing models, and a probe error remains a processing
    /// failure rather than being reclassified as silence.
    pub fn transcribe_asset_with_resource_overrides(
        &self,
        asset_id: &str,
        overrides: &ResourceOverrides,
    ) -> Result<TranscribeOutcome> {
        // Validate even when a no-audio fast path will not open an engine.
        let selection = self.resolve_resource_selection(
            SPEECH_REPRESENTATION_KIND,
            STT_RECIPE_KEY,
            None,
            None,
            overrides,
        )?;
        let asset = self.get_asset(asset_id)?;
        // One shared speech claim covers the local probe/extract/engine path and
        // the eventual transcript replacement, so it cannot race a prepared
        // native request for the same asset.
        let _speech_claim = self.claim_representation(asset_id, RepresentationKind::Speech)?;
        let proxy_path = Self::require_proxy(&asset)?;
        let source_fingerprint = self.proxy_fingerprint(asset_id)?;
        let audio_present = match self.transcription_audio_present(&proxy_path) {
            Ok(present) => present,
            Err(error) => {
                let message = format!("{error:#}");
                save_transcript(
                    &self.connection()?,
                    &failed_transcript(asset_id, &source_fingerprint, message.clone()),
                )?;
                return Err(error);
            }
        };
        if !audio_present {
            let stored = no_audio_transcript(asset_id, &source_fingerprint);
            save_transcript(&self.connection()?, &stored)?;
            return Ok(transcribe_outcome(asset_id, &stored));
        }

        let _heavy = self.acquire_local_heavy()?;
        let engine = match self
            .open_stt_engine_with_threads(selection.vad_threads, selection.decode_threads)
        {
            Ok(engine) => engine,
            Err(error) => {
                let message = format!("{error:#}");
                save_transcript(
                    &self.connection()?,
                    &failed_transcript(asset_id, &source_fingerprint, message),
                )?;
                return Err(error);
            }
        };
        match self.transcribe_source_with_engine(
            asset_id,
            &proxy_path,
            &source_fingerprint,
            engine.as_ref(),
        ) {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                let message = format!("{error:#}");
                let (speech_seconds, window_count, provider_segment_count) =
                    failure_evidence(&error);
                save_transcript(
                    &self.connection()?,
                    &failed_transcript_with_evidence(
                        asset_id,
                        &source_fingerprint,
                        message,
                        speech_seconds,
                        window_count,
                        provider_segment_count,
                    ),
                )?;
                Err(error)
            }
        }
    }

    pub(crate) fn open_stt_engine_with_threads(
        &self,
        vad_threads: usize,
        decode_threads: usize,
    ) -> Result<Box<dyn SttEngine>> {
        let directory = model_dir(&self.database_path)?;
        let whisper = ensure_model(&directory, &WHISPER_MODEL)?;
        let vad = ensure_model(&directory, &VAD_MODEL)?;
        if vad_threads == 4 && decode_threads == 4 {
            Ok(Box::new(WhisperEngine::new(&whisper, &vad)?))
        } else {
            Ok(Box::new(WhisperEngine::with_threads(
                &whisper,
                &vad,
                vad_threads,
                decode_threads,
            )?))
        }
    }

    /// Probes the proxy that transcription will actually read. Reuses the
    /// reviewed FFprobe helper from proxy generation so corrupt media and tool
    /// failures stay errors; only a successful probe with no audio stream is a
    /// no-audio outcome.
    pub(crate) fn transcription_audio_present(&self, proxy_path: &Path) -> Result<bool> {
        let ffprobe = resolve_tool_program(Path::new("ffprobe"));
        probe_source_audio_present(&ffprobe, proxy_path)
            .with_context(|| format!("probe proxy audio {}", proxy_path.display()))
    }

    /// Extracts PCM from an already-probed audio-bearing proxy and runs the
    /// speech pipeline. Callers must probe first so the engine is never opened
    /// solely to discover that a valid video has no audio stream.
    pub(crate) fn transcribe_source_with_engine(
        &self,
        asset_id: &str,
        proxy_path: &Path,
        source_fingerprint: &str,
        engine: &dyn SttEngine,
    ) -> Result<TranscribeOutcome> {
        let asset = self.get_asset(asset_id)?;

        let parent = self
            .database_path
            .parent()
            .ok_or_else(|| anyhow!("catalog has no parent directory"))?;
        let audio_dir = parent.join("cache").join("audio").join(&asset.project_id);
        fs::create_dir_all(&audio_dir)?;
        let pcm_path = audio_dir.join(format!("{asset_id}.{}.16k.wav", Uuid::new_v4()));

        let result = (|| -> Result<StoredTranscript> {
            FfmpegAudioExtractor.extract_pcm16k(proxy_path, &pcm_path)?;
            // Read once per clip. The engine receives samples, never a path, so
            // a multi-window clip does not re-read and re-convert the whole file
            // for every window.
            let pcm = read_pcm(&pcm_path)?;
            anyhow::ensure!(!pcm.is_empty(), "audio stream decoded to no PCM samples");
            transcribe_pcm(engine, asset_id, source_fingerprint, &pcm)
        })();
        // The extracted PCM is scratch, not a retained representation, so it is
        // removed whether or not the decode succeeded.
        let _ = fs::remove_file(&pcm_path);
        let stored = result?;

        save_transcript(&self.connection()?, &stored)?;
        Ok(transcribe_outcome(asset_id, &stored))
    }

    pub fn transcribe_missing(&self, project_id: &str) -> Result<TranscribeReport> {
        self.transcribe_missing_with_progress(project_id, |_| ControlFlow::Continue(()))
    }

    /// Transcript coverage for a project, without transcribing anything.
    ///
    /// A stored transcript whose proxy fingerprint no longer matches counts as
    /// `missing`, because that is what a `transcribe missing` run would redo.
    pub fn transcript_coverage(&self, project_id: &str) -> Result<TranscriptCoverage> {
        let mut coverage = TranscriptCoverage {
            recipe_key: STT_RECIPE_KEY.to_string(),
            ..TranscriptCoverage::default()
        };
        for asset in self.list_assets(project_id)? {
            if asset.removed {
                continue;
            }
            coverage.total += 1;
            match self.current_transcript_state(&asset.id)? {
                TranscriptCurrentState::Missing
                | TranscriptCurrentState::ProtectedNative { .. } => {
                    // Incomplete/unproved native work remains visibly unaccepted
                    // and outside the analysis-ready set.
                    coverage.missing += 1;
                }
                TranscriptCurrentState::RetryableLocalFailed(_) => coverage.failed += 1,
                TranscriptCurrentState::CurrentLocal(stored)
                | TranscriptCurrentState::CurrentNative(stored) => match stored.status {
                    TranscriptStatus::Speech => coverage.speech += 1,
                    TranscriptStatus::NoSpeech => {
                        coverage.no_speech += 1;
                        if stored.outcome_reason
                            == crate::analysis::store::TranscriptOutcomeReason::LegacyNoSpeechUnverified
                        {
                            coverage.legacy_no_speech_unverified += 1;
                        }
                    }
                    TranscriptStatus::NoAudio => coverage.no_audio += 1,
                    TranscriptStatus::NoAcceptedSpeech => coverage.no_accepted_speech += 1,
                    TranscriptStatus::Rejected => coverage.rejected += 1,
                    TranscriptStatus::Failed => coverage.failed += 1,
                    TranscriptStatus::ReviewRequired => coverage.review_required += 1,
                },
            }
        }
        Ok(coverage)
    }

    /// Transcribes every clip in the project without a current transcript.
    ///
    /// Weighting is by clip, not source bytes, because decode cost tracks
    /// 30-second window count rather than file size. One worker: peak RSS is
    /// 1.15-1.38 GB and concurrent transcription alongside the four-worker
    /// proxy pipeline has never been measured.
    ///
    /// `on_progress` returning `ControlFlow::Break` stops the batch after the
    /// current clip. The callback is the cancellation point rather than a
    /// separate flag parameter, so a new progress site cannot be added that
    /// forgets to check for cancellation.
    pub fn transcribe_missing_with_progress<F>(
        &self,
        project_id: &str,
        on_progress: F,
    ) -> Result<TranscribeReport>
    where
        F: FnMut(TranscribeBatchProgress) -> ControlFlow<()>,
    {
        self.transcribe_missing_with_resource_overrides_and_progress(
            project_id,
            &ResourceOverrides::default(),
            on_progress,
        )
    }

    /// Explicit resource-override form of `transcribe_missing`.
    pub fn transcribe_missing_with_resource_overrides_and_progress<F>(
        &self,
        project_id: &str,
        overrides: &ResourceOverrides,
        mut on_progress: F,
    ) -> Result<TranscribeReport>
    where
        F: FnMut(TranscribeBatchProgress) -> ControlFlow<()>,
    {
        // Validate overrides and resolve the selected engine settings before
        // acquiring the heavy slot or opening models.
        let selection = self.resolve_resource_selection(
            SPEECH_REPRESENTATION_KIND,
            STT_RECIPE_KEY,
            None,
            None,
            overrides,
        )?;
        let assets = self.list_assets(project_id)?;
        let history_key = selection.history_key.clone();
        let mut report = TranscribeReport {
            transcribed: 0,
            no_audio: 0,
            failed: 0,
            skipped: 0,
            cancelled: false,
            resource_source: selection.source.clone(),
            calibration_key: selection.calibration_key.clone(),
            vad_threads: selection.vad_threads,
            decode_threads: selection.decode_threads,
            worker_count: selection.worker_count,
            history_key: history_key.clone(),
            sample_compatibility: selection.sample_compatibility.clone(),
            outcomes: Vec::new(),
            failures: Vec::new(),
        };
        let probe_estimator = BatchEstimator::new(
            &vec![1; assets.len()],
            selection.worker_count.max(1),
            Default::default(),
        );
        let mut pending: Vec<ProjectPlannedAsset> = Vec::new();
        {
            for asset in assets {
                let mut estimate = probe_estimator.estimate(&[]);
                estimate.estimated_remaining_seconds = None;
                estimate.estimated_remaining_seconds_range = None;
                estimate.confidence = "inspecting_audio".into();
                if on_progress(TranscribeBatchProgress {
                    outcome: None,
                    estimate,
                })
                .is_break()
                {
                    report.cancelled = true;
                    return Ok(report);
                }
                if asset.removed {
                    continue;
                }
                if self.asset_video_validation(&asset.id)?.status
                    != AssetVideoValidationStatus::Usable
                {
                    report.skipped += 1;
                    continue;
                }
                match self.current_transcript_state(&asset.id)? {
                    TranscriptCurrentState::CurrentLocal(_)
                    | TranscriptCurrentState::CurrentNative(_) => {
                        report.skipped += 1;
                        continue;
                    }
                    TranscriptCurrentState::ProtectedNative { reason, .. } => {
                        pending.push(ProjectPlannedAsset::NativeReviewRecovery { asset, reason });
                        continue;
                    }
                    TranscriptCurrentState::Missing
                    | TranscriptCurrentState::RetryableLocalFailed(_) => {}
                }
                let source_fingerprint = match self.proxy_fingerprint(&asset.id) {
                    Ok(fingerprint) => fingerprint,
                    // A clip with no usable proxy cannot be transcribed. It is
                    // reported as a per-clip processing failure rather than
                    // aborting the whole batch.
                    Err(error) => {
                        pending.push(ProjectPlannedAsset::ProbeFailed {
                            asset,
                            source_fingerprint: String::new(),
                            reason: format!("{error:#}"),
                        });
                        continue;
                    }
                };
                let proxy_path = match Self::require_proxy(&asset) {
                    Ok(path) => path,
                    Err(error) => {
                        pending.push(ProjectPlannedAsset::ProbeFailed {
                            asset,
                            source_fingerprint,
                            reason: format!("{error:#}"),
                        });
                        continue;
                    }
                };
                match self.transcription_audio_present(&proxy_path) {
                    Ok(false) => pending.push(ProjectPlannedAsset::NoAudio {
                        asset,
                        source_fingerprint,
                    }),
                    Ok(true) => pending.push(ProjectPlannedAsset::Ready {
                        asset,
                        proxy_path,
                        source_fingerprint,
                    }),
                    Err(error) => pending.push(ProjectPlannedAsset::ProbeFailed {
                        asset,
                        source_fingerprint,
                        reason: format!("probe audio before transcription: {error:#}"),
                    }),
                }
            }
        }

        let audio_pending = pending
            .iter()
            .filter(|item| matches!(item, ProjectPlannedAsset::Ready { .. }))
            .count();

        let task_work: Vec<u64> = vec![1; audio_pending];
        let mut estimator = BatchEstimator::new(
            &task_work,
            selection.worker_count.max(1),
            self.load_batch_history(&history_key)?,
        );
        let start = on_progress(TranscribeBatchProgress {
            outcome: None,
            estimate: estimator.estimate(&task_work),
        });
        if pending.is_empty() || start.is_break() {
            report.cancelled = start.is_break();
            return Ok(report);
        }

        let _heavy = if audio_pending > 0 {
            match self.wait_for_local_heavy(|| {
                let mut estimate = estimator.estimate(&task_work);
                estimate.confidence = "waiting_for_resource".into();
                estimate.estimated_remaining_seconds = None;
                on_progress(TranscribeBatchProgress {
                    outcome: None,
                    estimate,
                })
                .is_break()
            })? {
                Some(lease) => Some(lease),
                None => {
                    report.cancelled = true;
                    return Ok(report);
                }
            }
        } else {
            None
        };
        // Loading the model costs seconds and over a gigabyte of RSS, so it is
        // opened only for a clip that actually has an audio stream and only once
        // the batch is known to be non-empty and uncancelled.
        let mut engine: Option<Box<dyn SttEngine>> = None;
        let mut audio_processed = 0_usize;
        for item in pending.iter() {
            let remaining = &task_work[audio_processed.min(task_work.len())..];
            let flow;
            let asset = match item {
                ProjectPlannedAsset::NoAudio { asset, .. }
                | ProjectPlannedAsset::ProbeFailed { asset, .. }
                | ProjectPlannedAsset::NativeReviewRecovery { asset, .. }
                | ProjectPlannedAsset::Ready { asset, .. } => asset,
            };
            if let ProjectPlannedAsset::NativeReviewRecovery { asset, reason } = item {
                report.skipped += 1;
                report.failures.push(IngestFailure {
                    path: asset.path.clone(),
                    error: format!("native speech review/recovery required: {reason}"),
                });
                flow = on_progress(TranscribeBatchProgress {
                    outcome: None,
                    estimate: estimator.estimate(remaining),
                });
                if flow.is_break() {
                    report.cancelled = true;
                    break;
                }
                continue;
            }
            let Some(_speech_claim) =
                self.try_claim_representation(&asset.id, RepresentationKind::Speech)?
            else {
                report.failed += 1;
                report.failures.push(IngestFailure {
                    path: asset.path.clone(),
                    error: "speech transcription is already in progress for this asset".into(),
                });
                flow = on_progress(TranscribeBatchProgress {
                    outcome: None,
                    estimate: estimator.estimate(remaining),
                });
                if flow.is_break() {
                    report.cancelled = true;
                    break;
                }
                continue;
            };
            // Another writer may have produced native work after planning but
            // before this speech claim became available. Recheck under the
            // existing claim before any local replacement can clear paid links.
            match self.current_transcript_state(&asset.id)? {
                TranscriptCurrentState::CurrentNative(_)
                | TranscriptCurrentState::CurrentLocal(_) => {
                    report.skipped += 1;
                    if matches!(item, ProjectPlannedAsset::Ready { .. }) {
                        audio_processed += 1;
                    }
                    if on_progress(TranscribeBatchProgress {
                        outcome: None,
                        estimate: estimator
                            .estimate(&task_work[audio_processed.min(task_work.len())..]),
                    })
                    .is_break()
                    {
                        report.cancelled = true;
                        break;
                    }
                    continue;
                }
                TranscriptCurrentState::ProtectedNative { reason } => {
                    report.skipped += 1;
                    if matches!(item, ProjectPlannedAsset::Ready { .. }) {
                        audio_processed += 1;
                    }
                    report.failures.push(IngestFailure {
                        path: asset.path.clone(),
                        error: format!("native speech review/recovery required: {reason}"),
                    });
                    if on_progress(TranscribeBatchProgress {
                        outcome: None,
                        estimate: estimator
                            .estimate(&task_work[audio_processed.min(task_work.len())..]),
                    })
                    .is_break()
                    {
                        report.cancelled = true;
                        break;
                    }
                    continue;
                }
                TranscriptCurrentState::Missing
                | TranscriptCurrentState::RetryableLocalFailed(_) => {}
            }
            match item {
                ProjectPlannedAsset::NoAudio {
                    asset,
                    source_fingerprint,
                } => {
                    let stored = no_audio_transcript(&asset.id, source_fingerprint);
                    save_transcript(&self.connection()?, &stored)?;
                    let outcome = transcribe_outcome(&asset.id, &stored);
                    report.transcribed += 1;
                    report.no_audio += 1;
                    flow = on_progress(TranscribeBatchProgress {
                        outcome: Some(outcome.clone()),
                        estimate: estimator.estimate(remaining),
                    });
                    report.outcomes.push(outcome);
                }
                ProjectPlannedAsset::ProbeFailed {
                    asset,
                    source_fingerprint,
                    reason,
                } => {
                    let stored = failed_transcript(&asset.id, source_fingerprint, reason.clone());
                    save_transcript(&self.connection()?, &stored)?;
                    report.failed += 1;
                    report.failures.push(IngestFailure {
                        path: asset.path.clone(),
                        error: reason.clone(),
                    });
                    flow = on_progress(TranscribeBatchProgress {
                        outcome: None,
                        estimate: estimator.estimate(remaining),
                    });
                }
                ProjectPlannedAsset::NativeReviewRecovery { .. } => {
                    unreachable!("native review/recovery items are handled before claims")
                }
                ProjectPlannedAsset::Ready {
                    asset,
                    proxy_path,
                    source_fingerprint,
                } => {
                    if engine.is_none() {
                        engine = Some(self.open_stt_engine_with_threads(
                            selection.vad_threads,
                            selection.decode_threads,
                        )?);
                    }
                    let started = Instant::now();
                    let result = self.transcribe_source_with_engine(
                        &asset.id,
                        proxy_path,
                        source_fingerprint,
                        engine
                            .as_deref()
                            .expect("engine opened for audio-bearing asset"),
                    );
                    audio_processed += 1;
                    match result {
                        Ok(outcome) => {
                            estimator.record_completion(1, started.elapsed());
                            report.transcribed += 1;
                            flow = on_progress(TranscribeBatchProgress {
                                outcome: Some(outcome.clone()),
                                estimate: estimator
                                    .estimate(&task_work[audio_processed.min(task_work.len())..]),
                            });
                            report.outcomes.push(outcome);
                        }
                        Err(error) => {
                            estimator.record_failure(1);
                            let message = format!("{error:#}");
                            // Keep the exact source fingerprint, detected-speech
                            // duration, and partial provider evidence while
                            // leaving the failed row retryable.
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
                            save_transcript(&self.connection()?, &stored)?;
                            report.failed += 1;
                            report.failures.push(IngestFailure {
                                path: asset.path.clone(),
                                error: message,
                            });
                            flow = on_progress(TranscribeBatchProgress {
                                outcome: None,
                                estimate: estimator
                                    .estimate(&task_work[audio_processed.min(task_work.len())..]),
                            });
                        }
                    }
                }
            }
            if flow.is_break() {
                // Completed clips are already committed, so a later run resumes
                // from here rather than repeating them.
                report.cancelled = true;
                break;
            }
        }

        if audio_pending > 0 {
            self.save_batch_history(&history_key, estimator.history())?;
        }
        Ok(report)
    }
}
