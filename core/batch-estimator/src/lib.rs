//! Weighted ETA estimation for heterogeneous batch jobs.
//!
//! Callers choose the work unit: bytes for media processing, frames or tokens
//! for inference, or any other positive cost proxy. Each completion records the
//! task's work and its time on one worker. Remaining tasks are greedily assigned
//! across the configured worker count, and ETA is based on the slowest estimated
//! shard. [`RateHistory`] is serializable so applications can persist and seed
//! estimates across runs.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RateHistory {
    pub work_units_per_worker_second: f64,
    pub samples: u64,
}

impl RateHistory {
    pub fn observe(&mut self, work_units: u64, worker_duration: Duration) {
        let seconds = worker_duration.as_secs_f64();
        if work_units == 0 || !seconds.is_finite() || seconds <= 0.0 {
            return;
        }
        let observed_rate = work_units as f64 / seconds;
        if self.samples == 0
            || !self.work_units_per_worker_second.is_finite()
            || self.work_units_per_worker_second <= 0.0
        {
            self.work_units_per_worker_second = observed_rate;
            self.samples = 1;
            return;
        }
        let window = (self.samples + 1).min(8) as f64;
        let alpha = 1.0 / window;
        self.work_units_per_worker_second =
            self.work_units_per_worker_second * (1.0 - alpha) + observed_rate * alpha;
        self.samples = self.samples.saturating_add(1);
    }

    pub fn usable_rate(self) -> Option<f64> {
        (self.samples > 0
            && self.work_units_per_worker_second.is_finite()
            && self.work_units_per_worker_second > 0.0)
            .then_some(self.work_units_per_worker_second)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BatchEstimate {
    pub completed_items: usize,
    pub total_items: usize,
    pub completed_work_units: u64,
    pub total_work_units: u64,
    pub worker_count: usize,
    pub work_units_per_worker_second: Option<f64>,
    pub elapsed_seconds: f64,
    pub estimated_remaining_seconds: Option<f64>,
    /// Explicit uncertainty for calibrated ETA consumers. Ordinary learned
    /// histories leave this unset rather than inventing a confidence level.
    #[serde(default)]
    pub confidence: String,
    #[serde(default)]
    pub estimated_remaining_seconds_range: Option<[f64; 2]>,
}

pub struct BatchEstimator {
    total_items: usize,
    total_work_units: u64,
    completed_items: usize,
    completed_work_units: u64,
    worker_count: usize,
    history: RateHistory,
    started_at: Instant,
}

impl BatchEstimator {
    pub fn new(task_work_units: &[u64], worker_count: usize, history: RateHistory) -> Self {
        Self {
            total_items: task_work_units.len(),
            total_work_units: task_work_units.iter().copied().sum(),
            completed_items: 0,
            completed_work_units: 0,
            worker_count: worker_count.max(1),
            history,
            started_at: Instant::now(),
        }
    }

    /// Restores durable item/work counters without adding a throughput sample.
    /// Callers use this when successful and failed item facts were committed
    /// before a process restart.
    pub fn with_progress(
        task_work_units: &[u64],
        worker_count: usize,
        history: RateHistory,
        completed_items: usize,
        completed_work_units: u64,
    ) -> Self {
        let mut estimator = Self::new(task_work_units, worker_count, history);
        estimator.completed_items = completed_items.min(estimator.total_items);
        estimator.completed_work_units = completed_work_units.min(estimator.total_work_units);
        estimator
    }
    pub fn with_explicit_progress(
        total_items: usize,
        total_work_units: u64,
        worker_count: usize,
        history: RateHistory,
        completed_items: usize,
        completed_work_units: u64,
    ) -> Self {
        let mut estimator = Self::new(&[], worker_count, history);
        estimator.total_items = total_items;
        estimator.total_work_units = total_work_units;
        estimator.completed_items = completed_items.min(total_items);
        estimator.completed_work_units = completed_work_units.min(total_work_units);
        estimator
    }

    pub fn record_completion(&mut self, work_units: u64, worker_duration: Duration) {
        self.completed_items = self.completed_items.saturating_add(1);
        self.completed_work_units = self.completed_work_units.saturating_add(work_units);
        self.history.observe(work_units, worker_duration);
    }

    pub fn record_failure(&mut self, work_units: u64) {
        self.completed_items = self.completed_items.saturating_add(1);
        self.completed_work_units = self.completed_work_units.saturating_add(work_units);
    }

    pub fn history(&self) -> RateHistory {
        self.history
    }

    pub fn estimate(&self, remaining_task_work_units: &[u64]) -> BatchEstimate {
        self.estimate_with_partial_progress(remaining_task_work_units, 0)
    }

    /// Estimates a running batch while some work has been completed inside
    /// tasks that have not finished yet. The caller supplies only the residual
    /// weights in `remaining_task_work_units`; partial work affects the progress
    /// counters but is never recorded as a throughput observation.
    pub fn estimate_with_partial_progress(
        &self,
        remaining_task_work_units: &[u64],
        partial_completed_work_units: u64,
    ) -> BatchEstimate {
        let rate = self.history.usable_rate();
        let remaining_seconds = rate.map(|rate| {
            estimated_parallel_work(remaining_task_work_units, self.worker_count) as f64 / rate
        });
        BatchEstimate {
            completed_items: self.completed_items,
            total_items: self.total_items,
            completed_work_units: self
                .completed_work_units
                .saturating_add(partial_completed_work_units)
                .min(self.total_work_units),
            total_work_units: self.total_work_units,
            worker_count: self.worker_count,
            work_units_per_worker_second: rate,
            elapsed_seconds: self.started_at.elapsed().as_secs_f64(),
            estimated_remaining_seconds: remaining_seconds,
            confidence: String::new(),
            estimated_remaining_seconds_range: None,
        }
    }
}

fn estimated_parallel_work(task_work_units: &[u64], worker_count: usize) -> u64 {
    if task_work_units.is_empty() {
        return 0;
    }
    let mut tasks = task_work_units.to_vec();
    tasks.sort_unstable_by(|left, right| right.cmp(left));
    let mut worker_loads = vec![0_u64; worker_count.max(1)];
    for task in tasks {
        let lightest = worker_loads
            .iter()
            .enumerate()
            .min_by(|(_, left), (_, right)| left.partial_cmp(right).unwrap_or(Ordering::Equal))
            .map(|(index, _)| index)
            .unwrap_or(0);
        worker_loads[lightest] = worker_loads[lightest].saturating_add(task);
    }
    worker_loads.into_iter().max().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_serial_work_from_persisted_history() {
        let estimator = BatchEstimator::new(
            &[100, 200, 300],
            1,
            RateHistory {
                work_units_per_worker_second: 100.0,
                samples: 4,
            },
        );
        let estimate = estimator.estimate(&[100, 200, 300]);
        assert_eq!(estimate.worker_count, 1);
        assert_eq!(estimate.total_work_units, 600);
        assert_eq!(estimate.estimated_remaining_seconds, Some(6.0));
    }

    #[test]
    fn uses_slowest_worker_shard_for_parallel_eta() {
        let estimator = BatchEstimator::new(
            &[9, 8, 7, 6],
            2,
            RateHistory {
                work_units_per_worker_second: 1.0,
                samples: 1,
            },
        );
        let estimate = estimator.estimate(&[9, 8, 7, 6]);
        assert_eq!(estimate.estimated_remaining_seconds, Some(15.0));
    }

    #[test]
    fn learns_rate_and_tracks_failures_without_poisoning_history() {
        let mut estimator = BatchEstimator::new(&[100, 100], 0, RateHistory::default());
        assert_eq!(
            estimator.estimate(&[100, 100]).estimated_remaining_seconds,
            None
        );
        estimator.record_completion(100, Duration::from_secs(2));
        let estimate = estimator.estimate(&[100]);
        assert_eq!(estimate.worker_count, 1);
        assert_eq!(estimate.completed_items, 1);
        assert_eq!(estimate.estimated_remaining_seconds, Some(2.0));
        let history = estimator.history();
        estimator.record_failure(100);
        assert_eq!(estimator.history(), history);
        assert_eq!(estimator.estimate(&[]).completed_items, 2);
    }

    #[test]
    fn partial_progress_reduces_remaining_eta_without_completing_an_item() {
        let estimator = BatchEstimator::new(
            &[100, 100],
            2,
            RateHistory {
                work_units_per_worker_second: 10.0,
                samples: 4,
            },
        );
        let estimate = estimator.estimate_with_partial_progress(&[40, 80], 80);
        assert_eq!(estimate.completed_items, 0);
        assert_eq!(estimate.completed_work_units, 80);
        assert_eq!(estimate.estimated_remaining_seconds, Some(8.0));
    }

    #[test]
    fn smooths_new_observations_with_a_bounded_window() {
        let mut history = RateHistory::default();
        history.observe(100, Duration::from_secs(1));
        history.observe(100, Duration::from_secs(2));
        assert_eq!(history.samples, 2);
        assert!((history.work_units_per_worker_second - 75.0).abs() < 0.001);
    }
}
