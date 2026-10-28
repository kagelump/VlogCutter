//! Replenished bounded preparation/API stages; no second budget or scheduler.
use super::*;
use crate::{Catalog, RunPolicy, RunPolicyParameters};
use anyhow::{Context, Result, ensure};
use batch_estimator::BatchEstimator;
use rusqlite::params;
use std::{
    sync::{
        Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

enum Event {
    Media(usize, bool, Duration),
    Finished(usize, Box<Result<NativeVideoWindowResult>>),
}
impl Event {
    fn finished(index: usize, result: Result<NativeVideoWindowResult>) -> Self {
        Self::Finished(index, Box::new(result))
    }
}

/// Hashing calls its cancellation probe for each 64 KiB chunk. Keep that
/// probe in memory; persistently requested cancellation is observed at most
/// 100 ms apart across the entire queue, rather than opening a catalog for
/// every chunk of the media/tool executables.
struct RunCancellation<'a> {
    catalog: &'a Catalog,
    run_id: &'a str,
    user: &'a AtomicBool,
    external: &'a (dyn Fn() -> bool + Sync),
    last_poll: Mutex<Option<Instant>>,
}
struct StopWorkersOnFailure<'a> {
    cancel: &'a AtomicBool,
    stop: &'a AtomicBool,
    armed: bool,
}
impl Drop for StopWorkersOnFailure<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.cancel.store(true, Ordering::Release);
            self.stop.store(true, Ordering::Release);
        }
    }
}
impl RunCancellation<'_> {
    fn is_cancelled(&self) -> bool {
        if self.user.load(Ordering::Acquire) || (self.external)() {
            self.user.store(true, Ordering::Release);
            return true;
        }
        if let Ok(mut last) = self.last_poll.try_lock()
            && last.is_none_or(|at| at.elapsed() >= Duration::from_millis(100))
        {
            *last = Some(Instant::now());
            if self
                .catalog
                .native_video_cancel_requested(self.run_id)
                .unwrap_or(true)
            {
                self.user.store(true, Ordering::Release);
            }
        }
        self.user.load(Ordering::Acquire)
    }
}

impl Catalog {
    /// Run only explicitly approved windows (or the deterministic policy split).
    /// Credentials are resolved by the leaf after canonical admission. A passed
    /// exact protocol pilot is mandatory, including for an injected transport.
    pub fn run_native_video_review(
        &self,
        policy_id: &str,
        windows: Option<&[NativeVideoWindowRequest]>,
        cancelled: &(dyn Fn() -> bool + Sync),
        on_progress: &mut dyn FnMut(NativeVideoProgress),
    ) -> Result<NativeVideoRunReport> {
        self.run_native_video_review_with_transport(
            policy_id,
            windows,
            &transport::DirectTransport::default(),
            cancelled,
            on_progress,
        )
    }
    pub(crate) fn run_native_video_review_with_transport(
        &self,
        policy_id: &str,
        windows: Option<&[NativeVideoWindowRequest]>,
        transport: &dyn transport::Transport,
        cancelled: &(dyn Fn() -> bool + Sync),
        on_progress: &mut dyn FnMut(NativeVideoProgress),
    ) -> Result<NativeVideoRunReport> {
        ensure!(!cancelled(), "native run cancelled before admission");
        let _claim = runtime::claim_policy(self, policy_id)?;
        let authorized =
            self.authorized_policy_for_bulk(policy_id, crate::AnalysisRunKind::NativeVideoReview)?;
        let policy = authorized.authorization.policy;
        let RunPolicyParameters::NativeVideoReview { options } = &policy.parameters else {
            anyhow::bail!("native parameters missing");
        };
        let planned = runtime::plan_native_video_windows(&policy)?;
        let windows = windows.unwrap_or(&planned);
        validate_windows(&policy, windows)?;
        let mut asset_windows = std::collections::BTreeMap::<&str, Vec<usize>>::new();
        for (index, window) in windows.iter().enumerate() {
            asset_windows
                .entry(&options.source_scopes[window.scope_index as usize].asset_id)
                .or_default()
                .push(index);
        }
        let run_id = self.begin_accounted_analysis_run(
            &policy.project_id,
            policy_id,
            "native_video_review",
        )?;
        let weights = windows
            .iter()
            .map(|r| ((r.source_out_seconds - r.source_in_seconds) * 1000.0).ceil() as u64)
            .collect::<Vec<_>>();
        let media_key = format!(
            "{}.media-w{}",
            NATIVE_VIDEO_MEDIA_RECIPE, options.media_workers
        );
        let api_key = format!(
            "{}.{}.{:?}.api-w{}",
            NATIVE_VIDEO_PROMPT_VERSION,
            options.tier.as_str(),
            options.thinking,
            options.api_workers
        );
        let mut media_estimator = BatchEstimator::new(
            &weights,
            options.media_workers as usize,
            self.load_batch_history(&media_key)?,
        );
        let mut api_estimator = BatchEstimator::new(
            &weights,
            options.api_workers as usize,
            self.load_batch_history(&api_key)?,
        );
        let initial = self.native_video_progress_snapshot(
            &run_id,
            &policy,
            "preparing",
            0,
            windows.len() as u32,
            &media_estimator,
            &api_estimator,
            &weights,
            &weights,
        )?;
        self.connection()?.execute("INSERT INTO native_video_run_details(run_id,policy_id,progress_json,updated_at) VALUES(?1,?2,?3,?4)",params![run_id,policy_id,serde_json::to_string(&initial)?,crate::now()])?;
        {
            let mut connection = self.connection()?;
            let transaction = connection.transaction()?;
            for (index, window) in windows.iter().enumerate() {
                transaction.execute("INSERT INTO native_video_run_windows(run_id,window_index,request_json) VALUES(?1,?2,?3)",params![run_id,index as u32,serde_json::to_string(window)?])?;
            }
            transaction.commit()?;
        }
        let stop_claiming = AtomicBool::new(false);
        let budget_stopped = AtomicBool::new(false);
        let user_cancelled = AtomicBool::new(false);
        let run_cancel = RunCancellation {
            catalog: self,
            run_id: &run_id,
            user: &user_cancelled,
            external: cancelled,
            last_poll: Mutex::new(None),
        };
        let next = AtomicUsize::new(0);
        let headroom = (Mutex::new(0u64), Condvar::new());
        let (prepared_tx, prepared_rx) =
            mpsc::sync_channel::<(usize, NativeVideoMedia)>(options.api_workers as usize);
        let prepared_rx = Mutex::new(prepared_rx);
        let (events_tx, events_rx) =
            mpsc::sync_channel(((options.api_workers + options.media_workers) * 2 + 1) as usize);
        let mut done = vec![false; windows.len()];
        let mut media_done = vec![false; windows.len()];
        let mut report = NativeVideoRunReport {
            run_id: run_id.clone(),
            policy_id: policy_id.into(),
            completed: 0,
            reused: 0,
            rejected: 0,
            interrupted: 0,
            cancelled: false,
            budget_stopped: false,
            evidence_ids: vec![],
            evidence_count: 0,
            failures: vec![],
            moment_artifacts: vec![],
            moment_artifact_count: 0,
            accounting: self.analysis_accounting_aggregate(policy_id, Some(&run_id))?,
        };
        let mut projected = std::collections::BTreeSet::new();
        // Join every worker before finalizing the canonical run or returning.
        // A local budget stop prevents new claims, independently of user cancel.
        let execution = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            std::thread::scope(|scope| -> Result<()> {
                let mut failure_guard = StopWorkersOnFailure {
                    cancel: &user_cancelled,
                    stop: &stop_claiming,
                    armed: true,
                };
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_progress(initial)))
                    .map_err(|_| anyhow::anyhow!("native progress observer failed"))?;
                for _ in 0..options.api_workers {
                    let receiver = &prepared_rx;
                    let events = events_tx.clone();
                    let catalog = self.clone();
                    let policy = &policy;
                    let run_id = &run_id;
                    let stop = &stop_claiming;
                    let user = &user_cancelled;
                    let budget = &budget_stopped;
                    let headroom = &headroom;
                    let run_cancel = &run_cancel;
                    scope.spawn(move || {
                        let cancel = || run_cancel.is_cancelled();
                        loop {
                            let item = match receiver.lock() {
                                Ok(guard) => guard.recv(),
                                Err(_) => break,
                            };
                            let Ok((index, media)) = item else {
                                break;
                            };
                            if cancel() {
                                user.store(true, Ordering::Release);
                                let _ = events.send(Event::finished(
                                    index,
                                    Err(anyhow::anyhow!("native_cancelled")),
                                ));
                                continue;
                            }
                            if stop.load(Ordering::Acquire) {
                                let _ = events.send(Event::finished(
                                    index,
                                    Err(anyhow::anyhow!("native_budget_stopped")),
                                ));
                                continue;
                            }
                            let wait = || loop {
                                if cancel() || stop.load(Ordering::Acquire) {
                                    return false;
                                }
                                let Ok(aggregate) = catalog
                                    .analysis_accounting_aggregate(&policy.policy_id, Some(run_id))
                                else {
                                    return false;
                                };
                                let Ok(quote) = cost_bound(options) else {
                                    return false;
                                };
                                if policy.budget_usd - aggregate.committed_or_reserved_usd + 1e-12
                                    >= quote.maximum_cost_usd
                                {
                                    return true;
                                }
                                if aggregate.active_reserved_cost_usd <= 0.0 {
                                    return false;
                                }
                                if let Ok(epoch) = headroom.0.lock() {
                                    let _ =
                                        headroom.1.wait_timeout(epoch, Duration::from_millis(100));
                                }
                            };
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    runtime::execute_window(
                                        &catalog,
                                        policy,
                                        run_id,
                                        "native_video_review",
                                        &media,
                                        transport,
                                        runtime::ExecutionControl {
                                            cancelled: &cancel,
                                            wait_for_headroom: &wait,
                                        },
                                    )
                                }))
                                .unwrap_or_else(|_| {
                                    Err(anyhow::anyhow!("native API worker panicked"))
                                });
                            if result.as_ref().is_err_and(|e| {
                                e.downcast_ref::<crate::AnalysisBudgetExhausted>().is_some()
                            }) {
                                budget.store(true, Ordering::Release);
                                stop.store(true, Ordering::Release);
                            }
                            if cancel() {
                                user.store(true, Ordering::Release);
                            }
                            if let Ok(mut epoch) = headroom.0.lock() {
                                *epoch = epoch.wrapping_add(1);
                                headroom.1.notify_all();
                            }
                            if events.send(Event::finished(index, result)).is_err() {
                                user.store(true, Ordering::Release);
                            }
                        }
                    });
                }
                for _ in 0..options.media_workers {
                    let tx = prepared_tx.clone();
                    let events = events_tx.clone();
                    let catalog = self.clone();
                    let policy = &policy;
                    let next = &next;
                    let stop = &stop_claiming;
                    let user = &user_cancelled;
                    let run_cancel = &run_cancel;
                    scope.spawn(move || {
                        let cancel = || run_cancel.is_cancelled();
                        loop {
                            if cancel() {
                                user.store(true, Ordering::Release);
                                break;
                            }
                            if stop.load(Ordering::Acquire) {
                                break;
                            }
                            let index = next.fetch_add(1, Ordering::AcqRel);
                            if index >= windows.len() {
                                break;
                            }
                            let start = Instant::now();
                            let prepared = catalog.prepare_native_video_window(
                                policy,
                                &windows[index],
                                &|| cancel() || stop.load(Ordering::Acquire),
                            );
                            if events
                                .send(Event::Media(
                                    index,
                                    prepared.as_ref().is_ok_and(|m| !m.view.reused),
                                    start.elapsed(),
                                ))
                                .is_err()
                            {
                                user.store(true, Ordering::Release);
                                break;
                            }
                            match prepared {
                                Ok(media) => {
                                    if tx.send((index, media)).is_err() {
                                        break;
                                    }
                                }
                                Err(error) => {
                                    if events.send(Event::finished(index, Err(error))).is_err() {
                                        user.store(true, Ordering::Release);
                                    }
                                }
                            }
                        }
                    });
                }
                drop(prepared_tx);
                drop(events_tx);
                for event in events_rx {
                    match event {
                        Event::Media(index, learn, duration) => {
                            media_done[index] = true;
                            if learn {
                                media_estimator.record_completion(weights[index], duration);
                            } else {
                                media_estimator.record_failure(weights[index]);
                            }
                        }
                        Event::Finished(index, result) => {
                            done[index] = true;
                            match *result {
                                Ok(result) => {
                                    self.connection()?.execute("UPDATE native_video_run_windows SET state=?3,evidence_id=?4,reused=?5 WHERE run_id=?1 AND window_index=?2",params![run_id,index as u32,result.state,result.evidence_id,result.reused])?;
                                    if result.reused {
                                        report.reused += 1;
                                    }
                                    if result.state == "completed" {
                                        report.completed += 1;
                                    } else {
                                        report.rejected += 1;
                                    }
                                    report.evidence_count += 1;
                                    if report.evidence_ids.len() < 100 {
                                        report.evidence_ids.push(result.evidence_id);
                                    }
                                    let latency = result
                                        .provenance
                                        .usage
                                        .as_ref()
                                        .map(|u| u.latency_ms)
                                        .unwrap_or(0);
                                    if !result.reused && result.state == "completed" && latency > 0
                                    {
                                        api_estimator.record_completion(
                                            weights[index],
                                            Duration::from_millis(latency),
                                        );
                                    } else {
                                        api_estimator.record_failure(weights[index]);
                                    }
                                }
                                Err(error) => {
                                    self.connection()?.execute("UPDATE native_video_run_windows SET state='failed',failure_code=?3 WHERE run_id=?1 AND window_index=?2",params![run_id,index as u32,failure_code(&error)])?;
                                    report.interrupted += 1;
                                    api_estimator.record_failure(weights[index]);
                                    if report.failures.len() < 100 {
                                        report.failures.push(NativeVideoWindowFailure {
                                            window_index: index as u32,
                                            code: failure_code(&error).into(),
                                        });
                                    }
                                }
                            }
                            let asset = &options.source_scopes[windows[index].scope_index as usize]
                                .asset_id;
                            if !projected.contains(asset)
                                && asset_windows[asset.as_str()].iter().all(|&i| done[i])
                            {
                                project_moments(self, &policy, asset, &mut report, index as u32);
                                projected.insert(asset.clone());
                            }
                        }
                    }
                    let media_remaining = remaining(&weights, &media_done);
                    let api_remaining = remaining(&weights, &done);
                    let snapshot = self.native_video_progress_snapshot(
                        &run_id,
                        &policy,
                        "reviewing",
                        done.iter().filter(|&&v| v).count() as u32,
                        windows.len() as u32,
                        &media_estimator,
                        &api_estimator,
                        &media_remaining,
                        &api_remaining,
                    )?;
                    self.connection()?.execute("UPDATE native_video_run_details SET progress_json=?2,updated_at=?3 WHERE run_id=?1",params![run_id,serde_json::to_string(&snapshot)?,crate::now()])?;
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        on_progress(snapshot)
                    }))
                    .is_err()
                    {
                        user_cancelled.store(true, Ordering::Release);
                        stop_claiming.store(true, Ordering::Release);
                        headroom.1.notify_all();
                        anyhow::bail!("native progress observer failed");
                    }
                }
                failure_guard.armed = false;
                Ok(())
            })
        }));
        report.cancelled = user_cancelled.load(Ordering::Acquire)
            || cancelled()
            || self.native_video_cancel_requested(&run_id)?;
        report.budget_stopped = budget_stopped.load(Ordering::Acquire);
        for (index, window) in windows.iter().enumerate().filter(|(i, _)| done[*i]) {
            let asset = &options.source_scopes[window.scope_index as usize].asset_id;
            if projected.insert(asset.clone()) {
                project_moments(self, &policy, asset, &mut report, index as u32);
            }
        }
        self.connection()?.execute("UPDATE native_video_run_windows SET state='not_completed',failure_code=?2 WHERE run_id=?1 AND state='pending'",params![run_id,if report.cancelled{"native_cancelled_before_completion"}else if report.budget_stopped{"native_budget_stopped_before_completion"}else{"native_owner_incomplete"}])?;
        report.interrupted += done.iter().filter(|&&v| !v).count() as u32;
        let execution_error = match execution {
            Ok(result) => result.err(),
            Err(_) => Some(anyhow::anyhow!("native worker panicked")),
        };
        if execution_error.is_some()
            && !cancelled()
            && !self.native_video_cancel_requested(&run_id)?
        {
            // Internal aborts stop worker I/O but are not a user cancellation.
            report.cancelled = false;
        }
        report.accounting = self.analysis_accounting_aggregate(policy_id, Some(&run_id))?;
        let state = if report.cancelled {
            "cancelled"
        } else if report.budget_stopped {
            "budget_stopped"
        } else if execution_error.is_some() || report.interrupted > 0 || !report.failures.is_empty()
        {
            "failed"
        } else {
            "completed"
        };
        self.finish_analysis_run(
            &run_id,
            &crate::AnalysisRunOutcome {
                status: if state == "budget_stopped" {
                    "completed".into()
                } else {
                    state.into()
                },
                stage_reached: "native_video_review".into(),
                clips_analyzed: report.completed as usize,
                cost_usd: self
                    .analysis_accounting_aggregate_for_run(
                        policy_id,
                        "native_video_review",
                        &run_id,
                    )?
                    .committed_or_reserved_usd,
                budget_exhausted: report.budget_stopped,
                error: if execution_error.is_some() {
                    "native_worker_or_persistence_failure".into()
                } else {
                    String::new()
                },
                ..Default::default()
            },
        )?;
        let final_progress = self.native_video_progress_snapshot(
            &run_id,
            &policy,
            state,
            done.iter().filter(|&&v| v).count() as u32,
            windows.len() as u32,
            &media_estimator,
            &api_estimator,
            &remaining(&weights, &media_done),
            &remaining(&weights, &done),
        )?;
        self.connection()?.execute("UPDATE native_video_run_details SET report_json=?2,progress_json=?3,updated_at=?4 WHERE run_id=?1",params![run_id,serde_json::to_string(&report)?,serde_json::to_string(&final_progress)?,crate::now()])?;
        if execution_error.is_none() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                on_progress(final_progress)
            }));
        }
        self.save_batch_history(&media_key, media_estimator.history())?;
        self.save_batch_history(&api_key, api_estimator.history())?;
        Ok(report)
    }
    #[allow(clippy::too_many_arguments)]
    fn native_video_progress_snapshot(
        &self,
        run_id: &str,
        policy: &RunPolicy,
        stage: &str,
        completed: u32,
        total: u32,
        media: &BatchEstimator,
        api: &BatchEstimator,
        media_remaining: &[u64],
        api_remaining: &[u64],
    ) -> Result<NativeVideoProgress> {
        let RunPolicyParameters::NativeVideoReview { options } = &policy.parameters else {
            anyhow::bail!("native parameters missing");
        };
        let aggregate = self.analysis_accounting_aggregate(&policy.policy_id, Some(run_id))?;
        let media_estimate = media.estimate(media_remaining);
        let api_estimate = api.estimate(api_remaining);
        let sequence = (media_estimate.completed_items + api_estimate.completed_items) as u64
            + u64::from(!matches!(stage, "preparing" | "reviewing"));
        Ok(NativeVideoProgress {
            run_id: run_id.into(),
            sequence,
            stage: stage.into(),
            completed,
            total,
            media_workers: options.media_workers,
            api_workers: options.api_workers,
            estimated_cost_usd: aggregate.estimated_cost_usd,
            confirmed_cost_usd: aggregate.confirmed_cost_usd,
            uncertain_usage_reserve_usd: aggregate.uncertain_usage_reserve_usd,
            active_reserved_cost_usd: aggregate.active_reserved_cost_usd,
            committed_or_reserved_usd: aggregate.committed_or_reserved_usd,
            budget_usd: policy.budget_usd,
            media_estimate,
            api_estimate,
        })
    }
}
fn remaining(weights: &[u64], done: &[bool]) -> Vec<u64> {
    weights
        .iter()
        .zip(done)
        .filter_map(|(&w, &d)| (!d).then_some(w))
        .collect()
}
fn project_moments(
    catalog: &Catalog,
    policy: &RunPolicy,
    asset: &str,
    report: &mut NativeVideoRunReport,
    index: u32,
) {
    match catalog.publish_native_video_moments(policy, asset) {
        Ok(Some(artifact)) => {
            report.moment_artifact_count += 1;
            if report.moment_artifacts.len() < 100 {
                report.moment_artifacts.push(NativeVideoMomentProjection {
                    asset_id: asset.into(),
                    identity: artifact.identity,
                });
            }
        }
        Ok(None) => {}
        Err(_) => {
            if report.failures.len() < 100 {
                report.failures.push(NativeVideoWindowFailure {
                    window_index: index,
                    code: "native_moment_projection_requires_review".into(),
                });
            }
        }
    }
}
fn failure_code(error: &anyhow::Error) -> &'static str {
    if error
        .downcast_ref::<crate::AnalysisBudgetExhausted>()
        .is_some()
    {
        "native_budget_exhausted"
    } else {
        "native_window_incomplete_or_recovery_required"
    }
}
fn validate_windows(policy: &RunPolicy, windows: &[NativeVideoWindowRequest]) -> Result<()> {
    let RunPolicyParameters::NativeVideoReview { options } = &policy.parameters else {
        anyhow::bail!("native parameters missing");
    };
    ensure!(
        !windows.is_empty() && windows.len() <= options.max_requests as usize,
        "native request count outside policy"
    );
    let mut seconds = 0.0;
    for r in windows {
        let scope = options
            .source_scopes
            .get(r.scope_index as usize)
            .context("native scope missing")?;
        ensure!(
            r.policy_id == policy.policy_id
                && [r.source_in_seconds, r.source_out_seconds, r.fps]
                    .iter()
                    .all(|v| v.is_finite())
                && r.source_in_seconds >= scope.source_in_seconds
                && r.source_out_seconds <= scope.source_out_seconds
                && r.source_in_seconds < r.source_out_seconds
                && r.source_out_seconds - r.source_in_seconds
                    <= f64::from(options.max_window_seconds)
                && (0.25..=options.fps).contains(&r.fps)
                && (160..=options.max_width).contains(&r.max_width),
            "native requested window exceeds policy scope"
        );
        seconds += r.source_out_seconds - r.source_in_seconds;
    }
    ensure!(
        seconds <= f64::from(options.max_media_seconds) + 1e-7,
        "native media duration outside policy"
    );
    Ok(())
}
