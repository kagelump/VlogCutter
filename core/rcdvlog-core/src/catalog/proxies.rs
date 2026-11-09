//! Proxy generation, proxy batches, and proxy provenance / rate histories.

use crate::*;
use anyhow::{Context, Result, anyhow, bail};
use batch_estimator::{BatchEstimator, RateHistory};
use rusqlite::{Connection, params};
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Instant;

pub(crate) const PROXY_CACHE_VERSION: &str = "v3";
#[cfg(target_os = "macos")]
pub(crate) const PROXY_WORKERS: usize = 4;
#[cfg(not(target_os = "macos"))]
pub(crate) const PROXY_WORKERS: usize = 1;
#[cfg(all(test, target_os = "macos"))]
pub(crate) type ProxyEncoder = ProxyEncoderKind;

pub(crate) enum ProxyWorkerMessage {
    Progress {
        index: usize,
        completed_work_units: u64,
    },
    Finished(Box<ProxyWorkerFinished>),
}

#[derive(Debug)]
pub(crate) struct ProxyGenerated {
    pub(crate) asset: Asset,
    pub(crate) outcome: ProxyGenerationOutcome,
}

pub(crate) struct ProxyWorkerFinished {
    pub(crate) index: usize,
    pub(crate) asset: Asset,
    pub(crate) elapsed: std::time::Duration,
    pub(crate) result: Result<ProxyGenerated>,
}

pub(crate) fn proxy_task_kind_for_encoder(
    encoder: ProxyEncoderKind,
    worker_count: usize,
) -> String {
    proxy_rate_history_key(encoder, worker_count)
}

#[cfg(test)]
pub(crate) fn proxy_task_kind(worker_count: usize) -> String {
    let plan = resolve_proxy_encoder_plan(ProxyEncoderPolicy::Auto);
    proxy_task_kind_for_encoder(plan.preferred_encoder, worker_count)
}

pub(crate) fn has_current_proxy(asset: &Asset) -> bool {
    asset.proxy_available && asset.proxy_representation == Some(ProxyRepresentation::RawV3)
}

pub fn proxy_representation(path: impl AsRef<Path>) -> ProxyRepresentation {
    let path = path.as_ref().to_string_lossy();
    if path.ends_with(".v3.mp4") {
        ProxyRepresentation::RawV3
    } else if path.ends_with(".v2.mp4") {
        ProxyRepresentation::MetadataTaggedRawV2
    } else {
        ProxyRepresentation::MetadataBakedV1
    }
}

pub(crate) fn insert_proxy_generation_provenance(
    connection: &Connection,
    asset_id: &str,
    outcome: &ProxyGenerationOutcome,
) -> Result<()> {
    let attempts_json =
        serde_json::to_string(&outcome.attempts).context("serialize proxy encoder attempts")?;
    let validation_json = outcome
        .validation
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .context("serialize proxy output validation")?;
    connection.execute(
        r#"
        INSERT INTO proxy_generation_provenance (
            asset_id, recipe_key, policy, selected_encoder, device,
            fallback_used, status, elapsed_ms, attempts_json,
            validation_json, created_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
        "#,
        params![
            asset_id,
            outcome.recipe_key,
            outcome.policy.as_str(),
            outcome.selected_encoder,
            outcome.device,
            i64::from(outcome.fallback_used),
            outcome.status,
            outcome.elapsed_ms.min(i64::MAX as u64) as i64,
            attempts_json,
            validation_json,
            now(),
        ],
    )?;
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
pub(crate) fn proxy_ffmpeg_command(
    source: &Path,
    output: &Path,
    encoder: ProxyEncoder,
) -> std::process::Command {
    let target = ProxyEncoderTarget {
        encoder,
        device: None,
        recipe_key: encoder.recipe_key().to_string(),
    };
    proxy_encoding::proxy_ffmpeg_command_with_program(
        &resolve_tool("ffmpeg"),
        source,
        output,
        &target,
    )
}

pub(crate) fn work_units_from_fraction(work_units: u64, fraction: f64) -> u64 {
    if !fraction.is_finite() || fraction <= 0.0 {
        return 0;
    }
    (work_units as f64 * fraction.clamp(0.0, 1.0))
        .round()
        .min(work_units as f64) as u64
}

pub(crate) fn residual_work_units(
    task_work: &[u64],
    completed: &[bool],
    partial_work: &[u64],
) -> Vec<u64> {
    task_work
        .iter()
        .enumerate()
        .filter_map(|(index, weight)| {
            (!completed[index]).then_some(weight.saturating_sub(partial_work[index]))
        })
        .collect()
}

impl Catalog {
    /// Testable and adapter-reusable execution boundary. The caller supplies
    /// explicit media tools and may supply a plan so deterministic fallback can
    /// be exercised without pretending a device exists.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn generate_proxy_untracked_with_execution<F>(
        &self,
        asset_id: &str,
        refresh_orientation: bool,
        policy: ProxyEncoderPolicy,
        mut on_progress: F,
        ffmpeg: &Path,
        ffprobe: &Path,
        plan_override: Option<ProxyEncoderPlan>,
    ) -> Result<ProxyGenerated>
    where
        F: FnMut(f64),
    {
        let write = self
            .begin_proxy_write(asset_id)?
            .ok_or_else(|| RepresentationBusy {
                asset_id: asset_id.to_owned(),
                kind: RepresentationKind::Proxy,
            })?;
        let asset = if refresh_orientation {
            self.refresh_asset_orientation(asset_id)?
        } else {
            self.get_asset(asset_id)?
        };
        // Prove this is still the original clip before transcoding.
        let original = self.resolve_original(asset_id)?;
        let source_audio_present = probe_source_audio_present(ffprobe, &original)
            .context("probe source audio before proxy generation")?;
        let plan = plan_override.unwrap_or_else(|| resolve_proxy_encoder_plan(policy));
        let temporary = write.temporary_path();
        let outcome = match run_proxy_transcode_with_plan(
            ffmpeg,
            ffprobe,
            &original,
            temporary,
            asset.duration_seconds,
            source_audio_present,
            &plan,
            &mut on_progress,
        ) {
            Ok(outcome) => outcome,
            Err(failure) => {
                let _ = fs::remove_file(temporary);
                if let Err(error) =
                    self.persist_proxy_generation_provenance(asset_id, &failure.outcome)
                {
                    tracing::warn!(
                        target: "rcdvlog_core::proxy",
                        asset = %asset_id,
                        error = %error,
                        "failed to persist proxy generation failure provenance"
                    );
                }
                return Err(anyhow::Error::new(failure).context("ffmpeg proxy generation failed"));
            }
        };
        let outcome_for_result = outcome.clone();
        let asset = self.commit_generated_proxy_write(write, &outcome)?;
        Ok(ProxyGenerated {
            asset,
            outcome: outcome_for_result,
        })
    }

    pub fn generate_proxy(&self, asset_id: &str) -> Result<Asset> {
        self.generate_proxy_with_progress(asset_id, |_| {})
    }

    pub fn generate_proxy_with_progress<F>(&self, asset_id: &str, on_progress: F) -> Result<Asset>
    where
        F: FnMut(AssetBatchProgress),
    {
        self.generate_proxy_with_policy_and_progress(
            asset_id,
            ProxyEncoderPolicy::Auto,
            on_progress,
        )
    }

    /// Generate one proxy with an explicit encoder policy. `Auto` uses the
    /// shared capability probe and retains the software fallback; `Software`
    /// is deterministic and never initializes hardware.
    pub fn generate_proxy_with_policy(
        &self,
        asset_id: &str,
        policy: ProxyEncoderPolicy,
    ) -> Result<Asset> {
        self.generate_proxy_with_policy_and_progress(asset_id, policy, |_| {})
    }

    pub fn generate_proxy_with_policy_and_progress<F>(
        &self,
        asset_id: &str,
        policy: ProxyEncoderPolicy,
        on_progress: F,
    ) -> Result<Asset>
    where
        F: FnMut(AssetBatchProgress),
    {
        let _heavy = self.acquire_local_heavy()?;
        self.generate_proxy_with_orientation_policy_and_policy(asset_id, true, policy, on_progress)
    }

    pub(crate) fn generate_proxy_with_orientation_policy<F>(
        &self,
        asset_id: &str,
        refresh_orientation: bool,
        on_progress: F,
    ) -> Result<Asset>
    where
        F: FnMut(AssetBatchProgress),
    {
        self.generate_proxy_with_orientation_policy_and_policy(
            asset_id,
            refresh_orientation,
            ProxyEncoderPolicy::Auto,
            on_progress,
        )
    }

    pub(crate) fn generate_proxy_with_orientation_policy_and_policy<F>(
        &self,
        asset_id: &str,
        refresh_orientation: bool,
        policy: ProxyEncoderPolicy,
        mut on_progress: F,
    ) -> Result<Asset>
    where
        F: FnMut(AssetBatchProgress),
    {
        // Internal path: callers already own the one shared local-heavy slot.
        let asset = self.get_asset(asset_id)?;
        let task_work = [asset.size_bytes];
        let selection = self.resolve_resource_selection(
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            Some(asset.size_bytes),
            None,
            &ResourceOverrides::default(),
        )?;
        let effective_policy = if selection.source == "calibration"
            && selection.encoder.as_deref() == Some("software")
        {
            ProxyEncoderPolicy::Software
        } else {
            policy
        };
        let plan = resolve_proxy_encoder_plan(effective_policy);
        let planned_encoder = plan.preferred_encoder.as_str();
        let task_kind = proxy_task_kind_for_encoder(plan.preferred_encoder, 1);
        let history_key = self.qualified_history_key(
            &task_kind,
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            Some(planned_encoder),
            1,
            0,
            0,
        )?;
        let mut history = self.load_batch_history(&history_key)?;
        if history.samples == 0
            && selection.source == "calibration"
            && selection.worker_count == 1
            && selection.encoder.as_deref() == Some(planned_encoder)
            && let Some(rate) = selection
                .timing
                .as_ref()
                .and_then(|timing| timing.work_units_per_second)
        {
            history = RateHistory {
                work_units_per_worker_second: rate,
                samples: selection
                    .timing
                    .as_ref()
                    .map(|timing| timing.samples_ms.len() as u64)
                    .unwrap_or(0),
            };
        }
        let mut estimator = BatchEstimator::new(&task_work, 1, history);
        on_progress(AssetBatchProgress {
            asset: None,
            estimate: estimator.estimate(&task_work),
        });
        let started = Instant::now();
        let ffmpeg = resolve_tool("ffmpeg");
        let ffprobe = resolve_tool("ffprobe");
        let result = self.generate_proxy_untracked_with_execution(
            asset_id,
            refresh_orientation,
            policy,
            |fraction| {
                let partial_work = work_units_from_fraction(asset.size_bytes, fraction);
                let remaining_work = asset.size_bytes.saturating_sub(partial_work);
                on_progress(AssetBatchProgress {
                    asset: None,
                    estimate: estimator
                        .estimate_with_partial_progress(&[remaining_work], partial_work),
                });
            },
            &ffmpeg,
            &ffprobe,
            Some(plan),
        );
        let updated = match &result {
            Ok(generated) => {
                estimator.record_completion(asset.size_bytes, started.elapsed());
                let actual_encoder = ProxyEncoderKind::parse(&generated.outcome.selected_encoder)
                    .unwrap_or(ProxyEncoderKind::Software);
                let actual_task_kind = proxy_task_kind_for_encoder(actual_encoder, 1);
                let actual_history_key = self.qualified_history_key(
                    &actual_task_kind,
                    PROXY_REPRESENTATION_KIND,
                    PROXY_SOFTWARE_RECIPE_KEY,
                    Some(actual_encoder.as_str()),
                    1,
                    0,
                    0,
                )?;
                let mut history = self.load_batch_history(&actual_history_key)?;
                history.observe(asset.size_bytes, generated.outcome.successful_elapsed());
                self.save_batch_history(&actual_history_key, history)?;
                Some(generated.asset.clone())
            }
            Err(_) => {
                estimator.record_failure(asset.size_bytes);
                None
            }
        };
        on_progress(AssetBatchProgress {
            asset: updated,
            estimate: estimator.estimate(&[]),
        });
        result.map(|generated| generated.asset)
    }

    /// Fingerprint of the proxy a transcript would be derived from.
    ///
    /// Transcripts are pinned to the proxy file, so regenerating a proxy must
    /// make its transcript stale. This reuses the same sampled BLAKE3 scheme as
    /// asset identity rather than introducing a second one.
    pub fn proxy_fingerprint(&self, asset_id: &str) -> Result<String> {
        let asset = self.get_asset(asset_id)?;
        let proxy_path = Self::require_proxy(&asset)?;
        let size = fs::metadata(&proxy_path)
            .with_context(|| format!("stat proxy {}", proxy_path.display()))?
            .len();
        sampled_fingerprint(&proxy_path, size)
    }

    pub(crate) fn require_proxy(asset: &Asset) -> Result<PathBuf> {
        let path = asset
            .proxy_path
            .as_deref()
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .ok_or_else(|| {
                anyhow!(
                    "proxy is required to transcribe {}; generate it first",
                    asset.id
                )
            })?;
        Ok(path)
    }

    pub fn generate_missing_proxies(&self, project_id: &str) -> Result<ProxyReport> {
        self.generate_missing_proxies_with_progress(project_id, |_| ControlFlow::Continue(()))
    }

    /// Generate missing proxies with an explicit encoder policy.
    pub fn generate_missing_proxies_with_policy(
        &self,
        project_id: &str,
        policy: ProxyEncoderPolicy,
    ) -> Result<ProxyReport> {
        self.generate_missing_proxies_with_policy_and_progress(project_id, policy, |_| {
            ControlFlow::Continue(())
        })
    }

    /// Generates every missing or stale proxy using the safe default policy.
    ///
    /// `on_progress` returning `ControlFlow::Break` stops the batch; workers
    /// finish the clip in flight and then stop claiming new work, so a
    /// cancelled run still commits whatever it completed.
    pub fn generate_missing_proxies_with_progress<F>(
        &self,
        project_id: &str,
        on_progress: F,
    ) -> Result<ProxyReport>
    where
        F: FnMut(AssetBatchProgress) -> ControlFlow<()>,
    {
        self.generate_missing_proxies_with_policy_and_progress(
            project_id,
            ProxyEncoderPolicy::Auto,
            on_progress,
        )
    }

    pub fn generate_missing_proxies_with_policy_and_progress<F>(
        &self,
        project_id: &str,
        policy: ProxyEncoderPolicy,
        on_progress: F,
    ) -> Result<ProxyReport>
    where
        F: FnMut(AssetBatchProgress) -> ControlFlow<()>,
    {
        self.generate_missing_proxies_with_resource_overrides_and_progress(
            project_id,
            policy,
            &ResourceOverrides::default(),
            on_progress,
        )
    }

    /// Proxy batch entrypoint used by CLI adapters that can pass explicit
    /// encoder/worker overrides. Calibration-selected worker counts are used
    /// only from a complete host/tool/recipe/configuration match.
    pub fn generate_missing_proxies_with_resource_overrides_and_progress<F>(
        &self,
        project_id: &str,
        policy: ProxyEncoderPolicy,
        overrides: &ResourceOverrides,
        mut on_progress: F,
    ) -> Result<ProxyReport>
    where
        F: FnMut(AssetBatchProgress) -> ControlFlow<()>,
    {
        let assets = self.list_assets(project_id)?;
        let mut ready = Vec::new();
        let mut pending = Vec::new();
        for asset in assets {
            if asset.removed
                || self.asset_video_validation(&asset.id)?.status
                    != AssetVideoValidationStatus::Usable
                || has_current_proxy(&asset)
                || (!asset.original_available && asset.proxy_available)
            {
                ready.push(asset);
            } else {
                pending.push(asset);
            }
        }
        let mut report = ProxyReport {
            generated: 0,
            skipped: ready.len(),
            cancelled: false,
            resource_source: String::new(),
            calibration_key: None,
            encoder: None,
            worker_count: 0,
            history_key: String::new(),
            sample_compatibility: "unknown".into(),
            failures: Vec::new(),
        };
        if let Some(requested) = overrides.worker_count
            && !pending.is_empty()
            && requested > pending.len()
        {
            bail!(
                "invalid input: workerCount {requested} exceeds the {} eligible proxy assets; the override cannot be honored without idling workers",
                pending.len()
            );
        }
        let task_work: Vec<u64> = pending.iter().map(|asset| asset.size_bytes).collect();
        let total_work = task_work.iter().copied().sum::<u64>();
        let selection = self.resolve_resource_selection(
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            Some(total_work),
            None,
            overrides,
        )?;
        let requested_workers = overrides
            .worker_count
            .unwrap_or_else(|| {
                if selection.source == "calibration" {
                    selection.worker_count
                } else {
                    PROXY_WORKERS
                }
            })
            .max(1);
        let worker_count = pending
            .len()
            .clamp(1, MAX_CALIBRATION_WORKERS.min(requested_workers));
        tracing::info!(
            target: "rcdvlog_core::proxy",
            worker_count,
            pending = task_work.len(),
            calibration_source = %selection.source,
            "generate_missing_proxies: begin"
        );
        let waiting_estimator =
            BatchEstimator::new(&task_work, worker_count, RateHistory::default());
        let _heavy = match self.wait_for_local_heavy(|| {
            let mut estimate = waiting_estimator.estimate(&task_work);
            estimate.confidence = "waiting_for_resource".into();
            estimate.estimated_remaining_seconds = None;
            on_progress(AssetBatchProgress {
                asset: None,
                estimate,
            })
            .is_break()
        })? {
            Some(lease) => lease,
            None => {
                report.cancelled = true;
                return Ok(report);
            }
        };
        let effective_policy = if overrides.encoder.as_deref() == Some("software")
            || (overrides.encoder.is_none()
                && selection.source == "calibration"
                && selection.encoder.as_deref() == Some("software"))
        {
            ProxyEncoderPolicy::Software
        } else {
            policy
        };
        let plan = resolve_proxy_encoder_plan(effective_policy);
        let planned_encoder = plan.preferred_encoder.as_str();
        let task_kind = proxy_task_kind_for_encoder(plan.preferred_encoder, worker_count);
        let history_key = self.qualified_history_key(
            &task_kind,
            PROXY_REPRESENTATION_KIND,
            PROXY_SOFTWARE_RECIPE_KEY,
            Some(planned_encoder),
            worker_count,
            0,
            0,
        )?;
        let mut history = self.load_batch_history(&history_key)?;
        if history.samples == 0
            && selection.source == "calibration"
            && selection.worker_count == worker_count
            && selection.encoder.as_deref() == Some(planned_encoder)
            && let Some(rate) = selection
                .timing
                .as_ref()
                .and_then(|timing| timing.work_units_per_second)
        {
            history = RateHistory {
                work_units_per_worker_second: rate / worker_count.max(1) as f64,
                samples: selection
                    .timing
                    .as_ref()
                    .map(|timing| timing.samples_ms.len() as u64)
                    .unwrap_or(0),
            };
        }
        report.resource_source = selection.source.clone();
        report.calibration_key = selection.calibration_key.clone();
        report.encoder = Some(planned_encoder.to_string());
        report.worker_count = worker_count;
        report.history_key = history_key.clone();
        report.sample_compatibility = selection.sample_compatibility.clone();
        let mut estimator = BatchEstimator::new(&task_work, worker_count, history);
        let ffmpeg = resolve_tool("ffmpeg");
        let ffprobe = resolve_tool("ffprobe");
        let start = on_progress(AssetBatchProgress {
            asset: None,
            estimate: estimator.estimate(&task_work),
        });
        if pending.is_empty() || start.is_break() {
            report.cancelled = start.is_break();
            return Ok(report);
        }

        let queue = Arc::new(Mutex::new(
            pending.into_iter().enumerate().collect::<VecDeque<_>>(),
        ));
        let stop = Arc::new(AtomicBool::new(false));
        let (outcome_sender, outcome_receiver) = mpsc::channel();
        let mut completed = vec![false; task_work.len()];
        let mut actual_encoders = HashSet::new();

        thread::scope(|scope| -> Result<()> {
            for _ in 0..worker_count {
                let catalog = self.clone();
                let queue = Arc::clone(&queue);
                let stop = Arc::clone(&stop);
                let outcome_sender = outcome_sender.clone();
                let plan = plan.clone();
                let ffmpeg = ffmpeg.clone();
                let ffprobe = ffprobe.clone();
                scope.spawn(move || {
                    loop {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let Some((index, asset)) = queue
                            .lock()
                            .expect("proxy work queue lock poisoned")
                            .pop_front()
                        else {
                            break;
                        };
                        let asset_id = asset.id.clone();
                        tracing::debug!(
                            target: "rcdvlog_core::proxy",
                            index,
                            asset = %asset_id,
                            "proxy worker: spawn ffmpeg"
                        );
                        let started = Instant::now();
                        let result = catalog.generate_proxy_untracked_with_execution(
                            &asset.id,
                            true,
                            policy,
                            |fraction| {
                                let _ = outcome_sender.send(ProxyWorkerMessage::Progress {
                                    index,
                                    completed_work_units: work_units_from_fraction(
                                        asset.size_bytes,
                                        fraction,
                                    ),
                                });
                            },
                            &ffmpeg,
                            &ffprobe,
                            Some(plan.clone()),
                        );
                        match &result {
                            Ok(_) => tracing::debug!(
                                target: "rcdvlog_core::proxy",
                                index,
                                asset = %asset_id,
                                elapsed_ms = started.elapsed().as_millis() as u64,
                                "proxy worker: ffmpeg done"
                            ),
                            Err(error) => tracing::warn!(
                                target: "rcdvlog_core::proxy",
                                index,
                                asset = %asset_id,
                                error = %format!("{error:#}"),
                                "proxy worker: ffmpeg failed"
                            ),
                        }
                        if outcome_sender
                            .send(ProxyWorkerMessage::Finished(Box::new(
                                ProxyWorkerFinished {
                                    index,
                                    asset,
                                    elapsed: started.elapsed(),
                                    result,
                                },
                            )))
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            }
            drop(outcome_sender);

            let mut partial_work = vec![0_u64; task_work.len()];
            for message in outcome_receiver {
                let updated = match message {
                    ProxyWorkerMessage::Progress {
                        index,
                        completed_work_units,
                    } => {
                        partial_work[index] = completed_work_units.min(task_work[index]);
                        None
                    }
                    ProxyWorkerMessage::Finished(finished) => {
                        let ProxyWorkerFinished {
                            index,
                            asset,
                            elapsed,
                            result,
                        } = *finished;
                        completed[index] = true;
                        partial_work[index] = 0;
                        match result {
                            Ok(generated) => {
                                report.generated += 1;
                                estimator.record_completion(asset.size_bytes, elapsed);
                                let actual_encoder =
                                    ProxyEncoderKind::parse(&generated.outcome.selected_encoder)
                                        .unwrap_or(ProxyEncoderKind::Software);
                                actual_encoders.insert(actual_encoder.as_str().to_string());
                                let actual_task_kind =
                                    proxy_task_kind_for_encoder(actual_encoder, worker_count);
                                let actual_history_key = self.qualified_history_key(
                                    &actual_task_kind,
                                    PROXY_REPRESENTATION_KIND,
                                    PROXY_SOFTWARE_RECIPE_KEY,
                                    Some(actual_encoder.as_str()),
                                    worker_count,
                                    0,
                                    0,
                                )?;
                                let mut history = self.load_batch_history(&actual_history_key)?;
                                history.observe(
                                    asset.size_bytes,
                                    generated.outcome.successful_elapsed(),
                                );
                                self.save_batch_history(&actual_history_key, history)?;
                                Some(generated.asset)
                            }
                            Err(error) if error.is::<RepresentationBusy>() => {
                                // Advance completion without training throughput.
                                estimator.record_failure(asset.size_bytes);
                                report.skipped += 1;
                                None
                            }
                            Err(error) => {
                                estimator.record_failure(asset.size_bytes);
                                report.failures.push(IngestFailure {
                                    path: asset.path,
                                    error: format!("{error:#}"),
                                });
                                None
                            }
                        }
                    }
                };
                let remaining_work = residual_work_units(&task_work, &completed, &partial_work);
                let partial_completed_work = partial_work.iter().copied().sum();
                let flow = on_progress(AssetBatchProgress {
                    asset: updated,
                    estimate: estimator
                        .estimate_with_partial_progress(&remaining_work, partial_completed_work),
                });
                if flow.is_break() {
                    report.cancelled = true;
                    stop.store(true, Ordering::Relaxed);
                }
            }
            Ok(())
        })?;

        report.encoder = match actual_encoders.len() {
            0 => Some(planned_encoder.to_string()),
            1 => {
                let actual_encoder = actual_encoders.iter().next().cloned();
                if let Some(actual_encoder) = actual_encoder.as_deref() {
                    let actual_kind = ProxyEncoderKind::parse(actual_encoder)
                        .unwrap_or(ProxyEncoderKind::Software);
                    let actual_base = proxy_task_kind_for_encoder(actual_kind, worker_count);
                    report.history_key = self.qualified_history_key(
                        &actual_base,
                        PROXY_REPRESENTATION_KIND,
                        PROXY_SOFTWARE_RECIPE_KEY,
                        Some(actual_encoder),
                        worker_count,
                        0,
                        0,
                    )?;
                }
                actual_encoder
            }
            _ => Some("mixed".into()),
        };
        report
            .failures
            .sort_by(|left, right| left.path.cmp(&right.path));
        Ok(report)
    }

    /// Persist both successful and failed proxy-generation evidence. A failed
    /// row never mutates `assets.proxy_path`; the previously registered output
    /// remains canonical.
    pub(crate) fn persist_proxy_generation_provenance(
        &self,
        asset_id: &str,
        outcome: &ProxyGenerationOutcome,
    ) -> Result<()> {
        insert_proxy_generation_provenance(&self.connection()?, asset_id, outcome)
    }

    /// Latest-first proxy generation provenance for one asset. Attempts and
    /// validation are decoded from the canonical SQLite JSON columns.
    pub fn proxy_generation_provenance(
        &self,
        asset_id: &str,
        limit: usize,
    ) -> Result<Vec<ProxyGenerationProvenance>> {
        self.load_proxy_generation_provenance(asset_id, limit.clamp(1, 1_000) as i64, None)
    }

    pub(crate) fn load_proxy_generation_provenance(
        &self,
        asset_id: &str,
        limit: i64,
        id: Option<i64>,
    ) -> Result<Vec<ProxyGenerationProvenance>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT id, asset_id, recipe_key, policy, selected_encoder, device,
                   fallback_used, status, elapsed_ms, attempts_json,
                   validation_json, created_at
            FROM proxy_generation_provenance
            WHERE asset_id = ?1 AND (?3 IS NULL OR id = ?3)
            ORDER BY id DESC
            LIMIT ?2
            "#,
        )?;
        let rows = statement.query_map(params![asset_id, limit, id], |row| {
            let policy_value: String = row.get(3)?;
            let attempts_json: String = row.get(9)?;
            let validation_json: Option<String> = row.get(10)?;
            Ok(ProxyGenerationProvenance {
                id: row.get(0)?,
                asset_id: row.get(1)?,
                recipe_key: row.get(2)?,
                policy: ProxyEncoderPolicy::parse(&policy_value).map_err(|_| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        format!("invalid proxy encoder policy {policy_value}").into(),
                    )
                })?,
                selected_encoder: row.get(4)?,
                device: row.get(5)?,
                fallback_used: row.get::<_, i64>(6)? != 0,
                status: row.get(7)?,
                elapsed_ms: row.get::<_, i64>(8)? as u64,
                attempts: serde_json::from_str(&attempts_json).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        9,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                validation: validation_json
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()
                    .map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            10,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                created_at: row.get(11)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Provenance attached to the currently registered generated proxy. A later
    /// failed attempt never replaces this association; downloads clear it.
    pub fn registered_proxy_generation_provenance(
        &self,
        asset_id: &str,
    ) -> Result<Option<ProxyGenerationProvenance>> {
        let id: Option<i64> = self.connection()?.query_row(
            "SELECT proxy_generation_id FROM assets WHERE id = ?1",
            [asset_id],
            |row| row.get(0),
        )?;
        let Some(id) = id else {
            return Ok(None);
        };
        Ok(self
            .load_proxy_generation_provenance(asset_id, 1, Some(id))?
            .pop())
    }

    /// Encoder- and worker-count-aware proxy rate histories in this catalog.
    pub fn proxy_rate_histories(&self) -> Result<Vec<ProxyRateHistoryEntry>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT task_kind, work_units_per_worker_second, samples, updated_at
            FROM batch_performance
            WHERE task_kind LIKE 'proxy.h264-720p.orientation-v3%'
            ORDER BY task_kind
            "#,
        )?;
        let rows = statement.query_map([], |row| {
            let task_kind: String = row.get(0)?;
            let encoder = if task_kind.contains("videotoolbox") {
                "videotoolbox"
            } else if task_kind.contains("vaapi") {
                "vaapi"
            } else {
                "software"
            };
            let worker_count = task_kind
                .rsplit_once("-w")
                .and_then(|(_, suffix)| suffix.split('-').next())
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(1);
            Ok(ProxyRateHistoryEntry {
                task_kind,
                encoder: encoder.to_string(),
                worker_count,
                work_units_per_worker_second: row.get(1)?,
                samples: row.get::<_, i64>(2)? as u64,
                updated_at: row.get(3)?,
                scope: "Observed only on this catalog's host with its successful hardware/software mix and worker count; not a cross-corpus benchmark.".to_string(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
