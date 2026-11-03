//! Offset consensus over one complete soundtrack session (ARCHITECTURE §8).
//! Pure derivation: the same plan and evidence always give byte-identical
//! output. The sync mapper never infers segments; it only copies this output
//! (§9).
use crate::{
    tv_review_sync::{SyncBoundary, SyncBoundaryKind, SyncQuery},
    tv_soundtrack_session::{SessionQueryEvidence, SoundtrackSessionPlan},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

/// Peaks below this similarity never vote for an offset (§8 step 1).
const MIN_PEAK_SIMILARITY: f64 = 0.25;
/// Offsets within this many seconds of a cluster's lowest member share it (§8 step 2).
const CLUSTER_HALF_WIDTH_SECONDS: f64 = 0.08;
/// A segment needs at least this many supporting queries (§8 step 3).
const MIN_SUPPORTING_QUERIES: usize = 3;
/// ... of which at least this many are Matched (§8 step 3).
const MIN_MATCHED_QUERIES: usize = 2;
/// Session evidence is read in pages of this many queries.
const EVIDENCE_PAGE_QUERIES: usize = 64;
/// Session query state for a query with exactly one clustered anchor.
const MATCHED_STATE: &str = "candidate";

/// A maximal run of queries that agree on one mic-to-movie offset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConsensusSegment {
    pub segment_id: String,
    /// Capture clock the sample bounds are expressed on.
    pub clock_id: String,
    /// Median of movie_seconds - mic_center_seconds over supporting queries.
    pub offset_seconds: f64,
    /// Centre of the first supporting query, in capture samples.
    pub mic_start_sample: u64,
    /// Centre of the last supporting query, in capture samples.
    pub mic_end_sample: u64,
    pub supporting_queries: Vec<usize>,
    pub residual_rms_seconds: f64,
    /// Least-squares slope times 1e6.
    pub drift_ppm: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SoundtrackConsensus {
    pub schema_version: u32,
    pub plan_digest: String,
    pub segments: Vec<ConsensusSegment>,
    /// Splits between segments reuse the existing sync boundary type.
    pub boundaries: Vec<SyncBoundary>,
    pub unsupported_queries: Vec<usize>,
}

/// Derives consensus from the complete evidence of `plan` read from `cache`.
/// An incomplete search fails with an error containing `search_incomplete`.
pub fn consensus(
    plan: &SoundtrackSessionPlan,
    cache: &Path,
    cancel: &dyn Fn() -> bool,
) -> Result<SoundtrackConsensus> {
    let mut evidence = Vec::with_capacity(plan.queries.len());
    while evidence.len() < plan.queries.len() {
        ensure!(!cancel(), "session evidence cancelled");
        let first = evidence.len();
        let page = plan.evidence_page(cache, first, EVIDENCE_PAGE_QUERIES, cancel)?;
        ensure!(
            page.plan_digest == plan.plan_digest && !page.queries.is_empty(),
            "invalid input: session evidence page identity"
        );
        evidence.extend(page.queries);
    }
    consensus_from_evidence(plan, &evidence, cancel)
}

/// Pure form of [`consensus`]: `evidence[i]` must describe `plan.queries[i]`.
pub fn consensus_from_evidence(
    plan: &SoundtrackSessionPlan,
    evidence: &[SessionQueryEvidence],
    cancel: &dyn Fn() -> bool,
) -> Result<SoundtrackConsensus> {
    plan.validate()?;
    let n = plan.queries.len();
    ensure!(
        evidence.len() <= n,
        "invalid input: session evidence exceeds plan"
    );
    ensure!(
        evidence.len() == n,
        "search_incomplete: {} of {} session queries have evidence",
        evidence.len(),
        n
    );
    for (i, e) in evidence.iter().enumerate() {
        ensure!(!cancel(), "session evidence cancelled");
        ensure!(
            e.query_index == i && e.recording == plan.queries[i].recording,
            "invalid input: session evidence query identity"
        );
        ensure!(
            e.total_references == plan.references.len()
                && e.completed_references == e.total_references,
            "search_incomplete: query {} searched {} of {} references",
            i,
            e.completed_references,
            e.total_references
        );
    }

    // Query centres on the capture clock (§8 step 1: mic_center_seconds).
    let centre: Vec<u64> = plan
        .queries
        .iter()
        .map(|q| (q.recording.start_sample + q.recording.end_sample) / 2)
        .collect();
    let centre_seconds: Vec<f64> = plan
        .queries
        .iter()
        .zip(&centre)
        .map(|(q, c)| *c as f64 / f64::from(q.capture.sample_rate))
        .collect();

    // Step 1: offsets from every peak at or above the vote threshold.
    let mut observations: Vec<(f64, usize, f64)> = Vec::new();
    for (qi, e) in evidence.iter().enumerate() {
        ensure!(!cancel(), "session evidence cancelled");
        for peak in &e.best_peaks {
            if peak.similarity.is_finite()
                && peak.similarity >= MIN_PEAK_SIMILARITY
                && peak.movie_seconds.is_finite()
            {
                observations.push((peak.movie_seconds - centre_seconds[qi], qi, peak.similarity));
            }
        }
    }
    observations.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));

    // Step 2: greedy clustering over sorted offsets.
    let mut seeds: Vec<f64> = Vec::new();
    let mut cluster_of = Vec::with_capacity(observations.len());
    for &(offset, _, _) in &observations {
        ensure!(!cancel(), "session evidence cancelled");
        match seeds.last() {
            Some(&seed) if offset - seed <= CLUSTER_HALF_WIDTH_SECONDS + 1e-9 => {}
            _ => seeds.push(offset),
        }
        cluster_of.push(seeds.len() - 1);
    }

    // Each query's best in-cluster peak: highest similarity, then lowest offset.
    let mut best: Vec<Option<Best>> = vec![None; n];
    for (k, &(offset, qi, similarity)) in observations.iter().enumerate() {
        if best[qi].is_none_or(|b| similarity > b.similarity) {
            best[qi] = Some(Best {
                similarity,
                offset,
                cluster: cluster_of[k],
            });
        }
    }

    // Mic order: clock, then centre sample, then query index.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        plan.queries[a]
            .recording
            .clock_id
            .cmp(&plan.queries[b].recording.clock_id)
            .then(centre[a].cmp(&centre[b]))
            .then(a.cmp(&b))
    });
    let coverage = coverage_ranges(plan);
    let contiguous = |a: &SyncQuery, b: &SyncQuery| -> bool {
        let lo = a.start_sample.min(b.start_sample);
        let hi = a.end_sample.max(b.end_sample);
        coverage
            .get(&a.clock_id)
            .is_some_and(|ranges| ranges.iter().any(|&(start, end)| start <= lo && hi <= end))
    };

    // Maximal runs: consecutive queries with a qualifying peak in one cluster,
    // on one contiguous clock. A query without a qualifying peak ends a run, and
    // so does a query whose best peak lies in a different cluster, Matched or not.
    let mut runs: Vec<Vec<(usize, Best)>> = Vec::new();
    let mut current: Vec<(usize, Best)> = Vec::new();
    for qi in order {
        ensure!(!cancel(), "session evidence cancelled");
        let Some(b) = best[qi] else {
            if !current.is_empty() {
                runs.push(std::mem::take(&mut current));
            }
            continue;
        };
        if let Some(&(prev, prev_best)) = current.last() {
            let breaks = prev_best.cluster != b.cluster
                || plan.queries[prev].recording.clock_id != plan.queries[qi].recording.clock_id
                || !contiguous(&plan.queries[prev].recording, &plan.queries[qi].recording);
            if breaks {
                runs.push(std::mem::take(&mut current));
            }
        }
        current.push((qi, b));
    }
    if !current.is_empty() {
        runs.push(current);
    }

    let mut segments: Vec<ConsensusSegment> = Vec::new();
    let mut boundaries: Vec<SyncBoundary> = Vec::new();
    let mut supported = vec![false; n];
    // Last supporting query of the previous accepted segment.
    let mut previous: Option<usize> = None;
    for run in runs {
        ensure!(!cancel(), "session evidence cancelled");
        let matched = run
            .iter()
            .filter(|(i, _)| evidence[*i].state == MATCHED_STATE)
            .count();
        if run.len() < MIN_SUPPORTING_QUERIES || matched < MIN_MATCHED_QUERIES {
            continue;
        }
        let members: Vec<usize> = run.iter().map(|(i, _)| *i).collect();
        let xs: Vec<f64> = members.iter().map(|&i| centre_seconds[i]).collect();
        let ys: Vec<f64> = run.iter().map(|(_, b)| b.offset).collect();
        let (slope, intercept) = least_squares(&xs, &ys);
        let drift_ppm = slope * 1e6;
        // §8 step 6: drift beyond the sync policy rejects the segment.
        if drift_ppm.abs() > plan.spec.sync.policy.max_drift_ppm {
            continue;
        }
        let residual_rms_seconds = (xs
            .iter()
            .zip(&ys)
            .map(|(x, y)| {
                let r = y - (intercept + slope * x);
                r * r
            })
            .sum::<f64>()
            / xs.len() as f64)
            .sqrt();
        let first = members[0];
        let last = members[members.len() - 1];
        let clock_id = plan.queries[first].recording.clock_id.clone();
        if let Some(prev) = previous.filter(|&p| plan.queries[p].recording.clock_id == clock_id) {
            // Boundary between two accepted segments, at the midpoint of their centres.
            let sample_index = (centre[prev] + centre[first]) / 2;
            let kind = if contiguous(
                &plan.queries[prev].recording,
                &plan.queries[first].recording,
            ) {
                SyncBoundaryKind::Resync
            } else {
                SyncBoundaryKind::CaptureInterrupted
            };
            boundaries.push(SyncBoundary {
                evidence_id: format!(
                    "consensus_boundary_{}",
                    digest(&(&plan.plan_digest, boundaries.len(), sample_index))?
                ),
                clock_id: clock_id.clone(),
                sample_index,
                kind,
            });
        }
        previous = Some(last);
        for &i in &members {
            supported[i] = true;
        }
        segments.push(ConsensusSegment {
            segment_id: format!(
                "consensus_{}",
                &digest(&(&plan.plan_digest, &members))?[..16]
            ),
            clock_id,
            offset_seconds: median(&ys),
            mic_start_sample: centre[first],
            mic_end_sample: centre[last],
            supporting_queries: members,
            residual_rms_seconds,
            drift_ppm,
        });
    }

    Ok(SoundtrackConsensus {
        schema_version: 1,
        plan_digest: plan.plan_digest.clone(),
        segments,
        boundaries,
        unsupported_queries: (0..n).filter(|&i| !supported[i]).collect(),
    })
}

/// The best qualifying peak of one query (§8 step 1 and cluster assignment).
#[derive(Debug, Clone, Copy)]
struct Best {
    similarity: f64,
    offset: f64,
    cluster: usize,
}

/// Merged sample ranges of audio coverage per clock. Touching ranges merge.
fn coverage_ranges(plan: &SoundtrackSessionPlan) -> BTreeMap<String, Vec<(u64, u64)>> {
    let mut map: BTreeMap<String, Vec<(u64, u64)>> = BTreeMap::new();
    for s in &plan.spec.segments {
        let end = s.first_sample + u64::from(s.frame_count);
        map.entry(s.clock_id.clone())
            .or_default()
            .push((s.first_sample, end));
    }
    for ranges in map.values_mut() {
        ranges.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::new();
        for &(start, end) in ranges.iter() {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        *ranges = merged;
    }
    map
}

/// Ordinary least squares: returns (slope, intercept).
fn least_squares(xs: &[f64], ys: &[f64]) -> (f64, f64) {
    let n = xs.len() as f64;
    let mx = xs.iter().sum::<f64>() / n;
    let my = ys.iter().sum::<f64>() / n;
    let sxx: f64 = xs.iter().map(|x| (x - mx) * (x - mx)).sum();
    let sxy: f64 = xs.iter().zip(ys).map(|(x, y)| (x - mx) * (y - my)).sum();
    let slope = if sxx > 0.0 { sxy / sxx } else { 0.0 };
    (slope, my - slope * mx)
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let m = sorted.len();
    if m % 2 == 1 {
        sorted[m / 2]
    } else {
        (sorted[m / 2 - 1] + sorted[m / 2]) / 2.0
    }
}

fn digest<T: Serialize>(value: &T) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}

#[cfg(test)]
#[path = "tv_soundtrack_consensus_tests.rs"]
mod tests;
