//! Piecewise source-audio alignment with an unknown state and shared-mapper adapter.
use crate::{
    tv_review_sync::{
        CaptureClock, CapturePosition, SyncAnchor, SyncAnchorSource, SyncBoundary,
        SyncBoundaryKind, SyncMovieClock, SyncQuery, SyncReport, SyncRequest,
    },
    tv_soundtrack_landmarks::{OffsetEvidence, Search},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

const TILE_SECONDS: f64 = crate::tv_soundtrack_landmarks::TIME_BIN_SECONDS;
const MAX_SEQUENCE_CANDIDATES_PER_TILE: usize = 128;
const MAX_REPORTED_ALTERNATIVES_PER_TILE: usize = 12;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Policy {
    pub minimum_tile_anchors: u32,
    pub minimum_tile_seconds: u32,
    pub minimum_frequency_diversity: u32,
    pub minimum_segment_tiles: u32,
    pub minimum_segment_anchors: u32,
    pub minimum_segment_seconds: u32,
    pub score_baseline_per_tile: f64,
    pub segment_change_penalty: f64,
    pub ambiguity_margin: f64,
    pub max_drift_ppm: f64,
    pub maximum_gap_seconds: f64,
    pub gap_penalty_per_second: f64,
    pub anchor_step_seconds: f64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            minimum_tile_anchors: 4,
            minimum_tile_seconds: 1,
            minimum_frequency_diversity: 4,
            minimum_segment_tiles: 6,
            minimum_segment_anchors: 10,
            minimum_segment_seconds: 6,
            score_baseline_per_tile: 2.1,
            segment_change_penalty: 3.0,
            ambiguity_margin: 0.75,
            max_drift_ppm: 2_000.0,
            maximum_gap_seconds: 2.0,
            gap_penalty_per_second: 0.75,
            anchor_step_seconds: 5.0,
        }
    }
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=20).contains(&self.minimum_tile_anchors)
                && (1..=5).contains(&self.minimum_tile_seconds)
                && (1..=32).contains(&self.minimum_frequency_diversity)
                && (2..=120).contains(&self.minimum_segment_tiles)
                && (4..=2000).contains(&self.minimum_segment_anchors)
                && (3..=600).contains(&self.minimum_segment_seconds)
                && self.score_baseline_per_tile.is_finite()
                && (0.1..=20.0).contains(&self.score_baseline_per_tile)
                && self.segment_change_penalty.is_finite()
                && (0.1..=100.0).contains(&self.segment_change_penalty)
                && self.ambiguity_margin.is_finite()
                && (0.0..=20.0).contains(&self.ambiguity_margin)
                && self.max_drift_ppm.is_finite()
                && (0.0..=2_000.0).contains(&self.max_drift_ppm)
                && self.maximum_gap_seconds.is_finite()
                && (0.0..=5.0).contains(&self.maximum_gap_seconds)
                && self.gap_penalty_per_second.is_finite()
                && (0.0..=20.0).contains(&self.gap_penalty_per_second)
                && self.anchor_step_seconds.is_finite()
                && (1.0..=10.0).contains(&self.anchor_step_seconds),
            "invalid input: source-audio alignment policy"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Segment {
    pub segment_id: String,
    pub recording_clock_id: String,
    pub reference_movie_digest: String,
    pub status: String,
    pub recording_start_sample: u64,
    pub recording_end_sample: u64,
    pub movie_start_seconds: f64,
    pub movie_end_seconds: f64,
    pub rate: f64,
    /// movie seconds = rate * (recording sample / native rate) + offsetSeconds
    pub offset_seconds: f64,
    pub tile_count: u32,
    pub independent_anchors: u32,
    pub unique_landmark_count: u32,
    pub supporting_seconds: u32,
    pub score: f64,
    pub residual_seconds: f64,
    pub boundary_uncertainty_seconds: f64,
    pub alignment_uncertainty_seconds: f64,
    pub runner_up_margin: Option<f64>,
    pub alternative_offsets: Vec<f64>,
    pub evidence_spans: Vec<EvidenceSpan>,
    pub inferred_spans: Vec<EvidenceSpan>,
    pub recipe_digest: String,
    pub index_digest: String,
    pub policy_digest: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceSpan {
    pub recording_start_sample: u64,
    pub recording_end_sample: u64,
    pub offset_seconds_from_capture_start: f64,
    pub independent_anchors: u32,
    pub supporting_seconds: u32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnknownSpan {
    pub recording_start_sample: u64,
    pub recording_end_sample: u64,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TileAlternatives {
    pub time_bin: u32,
    pub ambiguous: bool,
    pub alternatives: Vec<OffsetEvidence>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Report {
    pub schema_version: u32,
    pub recipe: String,
    pub movie_digest: String,
    pub recording_clock_id: String,
    pub index_digest: String,
    pub recipe_digest: String,
    pub policy_digest: String,
    pub policy: Policy,
    pub query_sample_count: u64,
    pub total_candidate_pairs: u64,
    pub segments: Vec<Segment>,
    pub unknown_spans: Vec<UnknownSpan>,
    pub alternatives: Vec<TileAlternatives>,
}

#[derive(Debug, Clone)]
struct Candidate {
    evidence: OffsetEvidence,
    score: f64,
}
#[derive(Clone, Copy)]
struct SegmentSupport {
    anchors: u32,
    seconds: u32,
    score: f64,
    origin: f64,
}
struct SegmentContext<'a> {
    movie: &'a SyncMovieClock,
    policy: &'a Policy,
    support: SegmentSupport,
    runner_up_margin: Option<f64>,
    recipe_digest: &'a str,
    policy_digest: &'a str,
}
#[derive(Debug, Clone)]
struct Node {
    value: f64,
    segment_count: u32,
    previous: usize,
    active: bool,
    offset: Option<f64>,
}

pub fn infer(
    search: &Search,
    clock: &CaptureClock,
    movie: &SyncMovieClock,
    policy: Policy,
) -> Result<Report> {
    policy.validate()?;
    ensure!(
        search.complete && search.schema_version == 1 && search.movie_digest == movie.movie_digest,
        "invalid input: complete landmark search for this movie required"
    );
    ensure!(
        clock.sample_rate > 0 && clock.last_sample > clock.first_sample,
        "invalid input: capture clock range"
    );
    ensure!(
        search.query_sample_count as f64 / crate::tv_soundtrack_landmarks::RATE as f64
            <= (clock.last_sample - clock.first_sample) as f64 / clock.sample_rate as f64 + 0.1,
        "invalid input: capture clock is shorter than decoded recording"
    );
    let recipe_digest = digest_json(&search.recipe)?;
    let policy_digest = digest_json(&(search.search_policy, &policy))?;
    let tile_count = ((search.query_sample_count as f64
        / crate::tv_soundtrack_landmarks::RATE as f64)
        / TILE_SECONDS)
        .ceil() as usize;
    ensure!(
        tile_count > 0 && tile_count <= 100_000,
        "invalid input: bounded capture duration required"
    );
    let mut by_tile: Vec<Vec<Candidate>> = vec![Vec::new(); tile_count];
    for evidence in &search.offsets {
        let Some(tile) = by_tile.get_mut(evidence.time_bin as usize) else {
            continue;
        };
        if evidence.independent_anchors < policy.minimum_tile_anchors
            || evidence.supporting_seconds < policy.minimum_tile_seconds
            || evidence.frequency_diversity < policy.minimum_frequency_diversity
        {
            continue;
        }
        // Log rarity plus capped, independent anchor/time/frequency support.
        let score = candidate_score(evidence);
        tile.push(Candidate {
            evidence: evidence.clone(),
            score,
        });
    }
    let mut alternatives = Vec::with_capacity(tile_count);
    let mut modes: Vec<Vec<Candidate>> = Vec::with_capacity(tile_count);
    let mut locally_ambiguous = vec![false; tile_count];
    for (time_bin, tile) in by_tile.iter_mut().enumerate() {
        tile.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(a.evidence.offset_bin.cmp(&b.evidence.offset_bin))
        });
        tile.dedup_by(|a, b| a.evidence.offset_bin == b.evidence.offset_bin);
        tile.truncate(MAX_SEQUENCE_CANDIDATES_PER_TILE);
        let rival = tile.iter().skip(1).find(|c| {
            (c.evidence.offset_seconds_from_capture_start
                - tile[0].evidence.offset_seconds_from_capture_start)
                .abs()
                > 1.0
        });
        if let Some(rival) = rival
            && tile[0].score - rival.score <= policy.ambiguity_margin
        {
            locally_ambiguous[time_bin] = true;
        }
        alternatives.push(TileAlternatives {
            time_bin: time_bin as u32,
            ambiguous: locally_ambiguous[time_bin],
            alternatives: tile
                .iter()
                .take(MAX_REPORTED_ALTERNATIVES_PER_TILE)
                .map(|c| c.evidence.clone())
                .collect(),
        });
        // A local rival is only a diagnostic. Keep every retained mode so the
        // sequence solver can resolve repeated or weak observations globally.
        modes.push(tile.clone());
    }

    let mut all_nodes: Vec<Vec<Node>> = Vec::with_capacity(tile_count);
    let mut prev = vec![Node {
        value: 0.0,
        segment_count: 0,
        previous: 0,
        active: false,
        offset: None,
    }];
    for candidates in &modes {
        let mut current = Vec::with_capacity(candidates.len() + 1);
        let mut unknown_best = 0usize;
        for i in 1..prev.len() {
            if better(&prev[i], &prev[unknown_best]) {
                unknown_best = i;
            }
        }
        current.push(Node {
            value: prev[unknown_best].value,
            segment_count: prev[unknown_best].segment_count,
            previous: unknown_best,
            active: false,
            offset: None,
        });
        for candidate in candidates {
            let mut best: Option<Node> = None;
            for (pi, p) in prev.iter().enumerate() {
                let candidate_offset = candidate.evidence.offset_seconds_from_capture_start;
                let offset_delta = p
                    .offset
                    .map(|x| (candidate_offset - x).abs())
                    .unwrap_or(f64::INFINITY);
                let continue_segment =
                    p.active && offset_delta <= continuity_limit(policy.max_drift_ppm);
                let starts = !continue_segment;
                let transition_penalty = if starts {
                    policy.segment_change_penalty
                } else {
                    offset_delta * 0.15
                };
                let value =
                    p.value + candidate.score - policy.score_baseline_per_tile - transition_penalty;
                let node = Node {
                    value,
                    segment_count: p.segment_count + u32::from(starts),
                    previous: pi,
                    active: true,
                    offset: Some(candidate_offset),
                };
                if best.as_ref().is_none_or(|b| better(&node, b)) {
                    best = Some(node);
                }
            }
            current.push(best.unwrap());
        }
        all_nodes.push(current.clone());
        prev = current;
    }
    let mut state = (0..prev.len())
        .max_by(|&a, &b| compare_nodes(&prev[a], &prev[b]))
        .unwrap_or(0);
    let mut chosen: Vec<Option<Candidate>> = vec![None; tile_count];
    for t in (0..tile_count).rev() {
        let node = &all_nodes[t][state];
        if state > 0 {
            chosen[t] = Some(modes[t][state - 1].clone());
        }
        state = node.previous;
    }
    // A low-ranked mode can win after its observations accumulate across time.
    // Keep it visible alongside the bounded top alternatives when the sequence
    // solver selects it, so the receipt remains explainable.
    for (tile, selected) in chosen.iter().enumerate() {
        let Some(selected) = selected else { continue };
        if !alternatives[tile].alternatives.iter().any(|e| {
            e.offset_bin == selected.evidence.offset_bin
                && e.offset_seconds_from_capture_start
                    == selected.evidence.offset_seconds_from_capture_start
        }) {
            alternatives[tile]
                .alternatives
                .push(selected.evidence.clone());
        }
    }
    let mut groups: Vec<Vec<(usize, Candidate)>> = Vec::new();
    for (t, c) in chosen.into_iter().enumerate() {
        let Some(c) = c else { continue };
        let joins = groups.last().and_then(|g| g.last()).is_some_and(|(pt, p)| {
            *pt + 1 == t
                && (p.evidence.offset_seconds_from_capture_start
                    - c.evidence.offset_seconds_from_capture_start)
                    .abs()
                    <= continuity_limit(policy.max_drift_ppm)
        });
        if joins {
            groups.last_mut().unwrap().push((t, c));
        } else {
            groups.push(vec![(t, c)]);
        }
    }
    let groups = merge_compatible_groups(groups, &modes, &locally_ambiguous, &policy);
    let origin = movie.origin_pts as f64 / movie.ticks_per_second();
    let mut segments = Vec::new();
    let mut ambiguous = locally_ambiguous;
    for group in groups {
        let anchors: u32 = group
            .iter()
            .map(|(_, c)| c.evidence.independent_anchors)
            .sum();
        let support_secs: u32 = group
            .iter()
            .map(|(_, c)| c.evidence.supporting_seconds)
            .sum();
        let net = group_path_score(&group, &policy);
        if group.len() < policy.minimum_segment_tiles as usize
            || anchors < policy.minimum_segment_anchors
            || support_secs < policy.minimum_segment_seconds
            || net <= 0.0
        {
            continue;
        }
        let competing = competing_group_score(&group, &modes, &policy);
        if competing.is_some_and(|score| net - score <= policy.ambiguity_margin) {
            for (tile, _) in &group {
                ambiguous[*tile] = true;
                alternatives[*tile].ambiguous = true;
            }
            continue;
        }
        // Neighboring tiles may each have a close alternative. A coherent
        // sequence can resolve those local ties, so clear only the accepted
        // group's local ambiguity markers after the global comparison.
        for (tile, _) in &group {
            ambiguous[*tile] = false;
            alternatives[*tile].ambiguous = false;
        }
        let context = SegmentContext {
            movie,
            policy: &policy,
            support: SegmentSupport {
                anchors,
                seconds: support_secs,
                score: net,
                origin,
            },
            runner_up_margin: competing.map(|score| net - score),
            recipe_digest: &recipe_digest,
            policy_digest: &policy_digest,
        };
        let Some(segment) = fit_segment(&group, search, clock, &context)? else {
            continue;
        };
        let movie_end = origin + movie.duration_pts as f64 / movie.ticks_per_second();
        if segment.movie_start_seconds >= origin - 1e-9
            && segment.movie_end_seconds <= movie_end + 1e-9
        {
            segments.push(segment);
        }
    }
    segments.sort_by_key(|s| s.recording_start_sample);
    let mut unknown_spans = Vec::new();
    let mut cursor = clock.first_sample;
    for s in &segments {
        if s.recording_start_sample > cursor {
            unknown_spans.push(UnknownSpan {
                recording_start_sample: cursor,
                recording_end_sample: s.recording_start_sample,
                reason: unknown_reason(cursor, s.recording_start_sample, clock, &ambiguous),
            });
        }
        cursor = cursor.max(s.recording_end_sample);
    }
    if cursor < clock.last_sample {
        unknown_spans.push(UnknownSpan {
            recording_start_sample: cursor,
            recording_end_sample: clock.last_sample,
            reason: unknown_reason(cursor, clock.last_sample, clock, &ambiguous),
        });
    }
    Ok(Report {
        schema_version: 1,
        recipe: search.recipe.id.clone(),
        movie_digest: search.movie_digest.clone(),
        recording_clock_id: clock.clock_id.clone(),
        index_digest: search.index_digest.clone(),
        recipe_digest,
        policy_digest,
        policy,
        query_sample_count: search.query_sample_count,
        total_candidate_pairs: search.candidate_pair_count,
        segments,
        unknown_spans,
        alternatives,
    })
}

fn fit_segment(
    group: &[(usize, Candidate)],
    search: &Search,
    clock: &CaptureClock,
    context: &SegmentContext<'_>,
) -> Result<Option<Segment>> {
    let movie = context.movie;
    let policy = context.policy;
    let SegmentSupport {
        anchors,
        seconds,
        score,
        origin,
    } = context.support;
    let rate = clock.sample_rate as f64;
    let capture_first = clock.first_sample as f64 / rate;
    let xs: Vec<f64> = group
        .iter()
        .map(|(tile, _)| (*tile as f64 + 0.5) * TILE_SECONDS)
        .collect();
    let ys: Vec<f64> = group
        .iter()
        .zip(&xs)
        .map(|((_, c), x)| origin + *x + c.evidence.offset_seconds_from_capture_start)
        .collect();
    let meanx = xs.iter().sum::<f64>() / xs.len() as f64;
    let meany = ys.iter().sum::<f64>() / ys.len() as f64;
    let cov = xs
        .iter()
        .zip(&ys)
        .map(|(x, y)| (x - meanx) * (y - meany))
        .sum::<f64>();
    let var = xs.iter().map(|x| (x - meanx).powi(2)).sum::<f64>();
    let slope = if var > 0.0 { cov / var } else { 1.0 };
    let intercept = meany - slope * meanx;
    let err0 = ys
        .iter()
        .zip(&xs)
        .map(|(y, x)| (y - (x + (meany - meanx))).powi(2))
        .sum::<f64>();
    let err1 = ys
        .iter()
        .zip(&xs)
        .map(|(y, x)| (y - (slope * x + intercept)).powi(2))
        .sum::<f64>();
    let span = xs.last().unwrap() - xs[0];
    let ppm = (slope - 1.0) * 1_000_000.0;
    let use_drift = xs.len() >= 3
        && span >= 15.0
        && ppm.abs() <= policy.max_drift_ppm
        && ppm.abs() >= 50.0
        && err1 < err0 * 0.75;
    let a = if use_drift { slope } else { 1.0 };
    let b_rel = if use_drift { intercept } else { meany - meanx };
    let offset = b_rel - a * capture_first;
    let query_duration =
        search.query_sample_count as f64 / crate::tv_soundtrack_landmarks::RATE as f64;
    let local_start = group.first().unwrap().0 as f64 * TILE_SECONDS;
    let local_end = ((group.last().unwrap().0 + 1) as f64 * TILE_SECONDS).min(query_duration);
    let mut start = clock.first_sample + (local_start * rate).round() as u64;
    let mut end = (clock.first_sample + (local_end * rate).round() as u64).min(clock.last_sample);
    let movie_end_bound = origin + movie.duration_pts as f64 / movie.ticks_per_second();
    let min_sample = ((origin - offset) / a * rate).ceil().max(0.0) as u64;
    let max_sample = ((movie_end_bound - offset) / a * rate).floor().max(0.0) as u64;
    start = start.max(min_sample);
    end = end.min(max_sample);
    if start >= end || a <= 0.0 {
        return Ok(None);
    }
    let movie_start = a * (start as f64 / rate) + offset;
    let movie_end = a * (end as f64 / rate) + offset;
    let residual = (if use_drift {
        err1 / xs.len() as f64
    } else {
        err0 / xs.len() as f64
    })
    .sqrt();
    let runner_up_margin = context.runner_up_margin;
    let mut alternative_offsets = Vec::new();
    let mut evidence_spans = Vec::with_capacity(group.len());
    for (tile, chosen) in group {
        let tile_candidates: Vec<_> = search
            .offsets
            .iter()
            .filter(|e| e.time_bin == *tile as u32)
            .filter(|e| {
                e.independent_anchors >= policy.minimum_tile_anchors
                    && e.supporting_seconds >= policy.minimum_tile_seconds
                    && e.frequency_diversity >= policy.minimum_frequency_diversity
            })
            .collect();
        if let Some(rival) = tile_candidates
            .iter()
            .filter(|e| {
                (e.offset_seconds_from_capture_start
                    - chosen.evidence.offset_seconds_from_capture_start)
                    .abs()
                    > 1.0
            })
            .max_by(|a, b| candidate_score(a).total_cmp(&candidate_score(b)))
        {
            alternative_offsets.push(rival.offset_seconds_from_capture_start);
        }
        let start = (clock.first_sample + (*tile as f64 * TILE_SECONDS * rate).round() as u64)
            .min(clock.last_sample);
        let end = (clock.first_sample
            + (((*tile + 1) as f64 * TILE_SECONDS).min(query_duration) * rate).round() as u64)
            .min(clock.last_sample)
            .max(start);
        evidence_spans.push(EvidenceSpan {
            recording_start_sample: start.max(clock.first_sample),
            recording_end_sample: end,
            offset_seconds_from_capture_start: chosen.evidence.offset_seconds_from_capture_start,
            independent_anchors: chosen.evidence.independent_anchors,
            supporting_seconds: chosen.evidence.supporting_seconds,
        });
    }
    let maximum_internal_gap = group
        .windows(2)
        .map(|pair| pair[1].0.saturating_sub(pair[0].0 + 1) as f64)
        .fold(0.0, f64::max);
    let mut inferred_spans = Vec::new();
    for pair in group.windows(2) {
        for tile in pair[0].0 + 1..pair[1].0 {
            let local_start = tile as f64 * TILE_SECONDS;
            let local_end = ((tile + 1) as f64 * TILE_SECONDS).min(query_duration);
            let start = (clock.first_sample as f64 + local_start * rate)
                .round()
                .min(clock.last_sample as f64) as u64;
            let end = (clock.first_sample as f64 + local_end * rate)
                .round()
                .min(clock.last_sample as f64) as u64;
            if start >= end {
                continue;
            }
            let center = (tile as f64 + 0.5) * TILE_SECONDS;
            inferred_spans.push(EvidenceSpan {
                recording_start_sample: start,
                recording_end_sample: end,
                offset_seconds_from_capture_start: a * center + intercept - origin - center,
                independent_anchors: 0,
                supporting_seconds: 0,
            });
        }
    }
    alternative_offsets.sort_by(f64::total_cmp);
    alternative_offsets.dedup_by(|a, b| (*a - *b).abs() < 0.08);
    alternative_offsets.truncate(8);
    let segment_id = {
        let d = blake3::hash(
            format!(
                "{}:{}:{}:{}",
                search.index_digest, clock.clock_id, start, end
            )
            .as_bytes(),
        )
        .to_hex()
        .to_string();
        format!("landmark_segment_{}", &d[..40])
    };
    Ok(Some(Segment {
        segment_id,
        recording_clock_id: clock.clock_id.clone(),
        reference_movie_digest: movie.movie_digest.clone(),
        status: "matched".into(),
        recording_start_sample: start,
        recording_end_sample: end,
        movie_start_seconds: movie_start,
        movie_end_seconds: movie_end,
        rate: a,
        offset_seconds: offset,
        tile_count: group.len() as u32,
        independent_anchors: anchors,
        unique_landmark_count: anchors,
        supporting_seconds: seconds,
        score,
        residual_seconds: residual,
        boundary_uncertainty_seconds: TILE_SECONDS,
        alignment_uncertainty_seconds: 0.04 + residual + maximum_internal_gap,
        runner_up_margin,
        alternative_offsets,
        evidence_spans,
        inferred_spans,
        recipe_digest: context.recipe_digest.into(),
        index_digest: search.index_digest.clone(),
        policy_digest: context.policy_digest.into(),
    }))
}
fn candidate_score(evidence: &OffsetEvidence) -> f64 {
    (1.0 + evidence.rarity_weight).ln()
        + 0.32 * f64::from(evidence.independent_anchors.min(40))
        + 0.40 * f64::from(evidence.supporting_seconds.min(1))
        + 0.03 * f64::from(evidence.frequency_diversity.min(20))
        - 0.12 * f64::from(evidence.saturated_anchors.min(evidence.independent_anchors))
}
fn group_path_score(group: &[(usize, Candidate)], policy: &Policy) -> f64 {
    group
        .iter()
        .enumerate()
        .map(|(i, (tile, candidate))| {
            let transition = if i == 0 {
                policy.segment_change_penalty
            } else {
                let (previous_tile, previous) = &group[i - 1];
                let gap = (*tile).saturating_sub(*previous_tile + 1) as f64;
                (candidate.evidence.offset_seconds_from_capture_start
                    - previous.evidence.offset_seconds_from_capture_start)
                    .abs()
                    * 0.15
                    + gap * policy.gap_penalty_per_second
            };
            candidate.score - policy.score_baseline_per_tile - transition
        })
        .sum()
}

fn merge_compatible_groups(
    groups: Vec<Vec<(usize, Candidate)>>,
    modes: &[Vec<Candidate>],
    ambiguous: &[bool],
    policy: &Policy,
) -> Vec<Vec<(usize, Candidate)>> {
    let mut merged: Vec<Vec<(usize, Candidate)>> = Vec::with_capacity(groups.len());
    for group in groups {
        let can_join = merged.last().is_some_and(|previous| {
            bridge_is_supported(previous, &group, modes, ambiguous, policy)
        });
        if can_join {
            merged.last_mut().unwrap().extend(group);
        } else {
            merged.push(group);
        }
    }
    merged
}

fn bridge_is_supported(
    previous: &[(usize, Candidate)],
    next: &[(usize, Candidate)],
    modes: &[Vec<Candidate>],
    ambiguous: &[bool],
    policy: &Policy,
) -> bool {
    let Some((previous_tile, previous_candidate)) = previous.last() else {
        return false;
    };
    let Some((next_tile, next_candidate)) = next.first() else {
        return false;
    };
    let missing = (*next_tile).saturating_sub(*previous_tile + 1);
    if missing == 0 || missing as f64 > policy.maximum_gap_seconds {
        return false;
    }
    let previous_offset = previous_candidate
        .evidence
        .offset_seconds_from_capture_start;
    let next_offset = next_candidate.evidence.offset_seconds_from_capture_start;
    if (next_offset - previous_offset).abs()
        > continuity_limit(policy.max_drift_ppm) * (missing as f64 + 1.0)
    {
        return false;
    }
    for tile in *previous_tile + 1..*next_tile {
        if ambiguous.get(tile).copied().unwrap_or(false) {
            return false;
        }
        let progress =
            (tile.saturating_sub(*previous_tile)) as f64 / (*next_tile - *previous_tile) as f64;
        let expected = previous_offset + (next_offset - previous_offset) * progress;
        if modes.get(tile).is_some_and(|candidates| {
            candidates.iter().any(|candidate| {
                (candidate.evidence.offset_seconds_from_capture_start - expected).abs() > 1.0
                    && candidate.score >= policy.score_baseline_per_tile
            })
        }) {
            // A well-supported competing mode in the gap may be a seek.
            return false;
        }
    }
    true
}

/// Score the strongest full-length, continuous alternative to a chosen group.
/// Excluding the chosen mode at the middle tile asks whether a different offset
/// path can explain the same interval, while continuity accumulates its evidence
/// across tiles instead of rejecting each tile's local ambiguity in isolation.
fn competing_group_score(
    group: &[(usize, Candidate)],
    modes: &[Vec<Candidate>],
    policy: &Policy,
) -> Option<f64> {
    let middle = group.get(group.len() / 2)?;
    let selected_offset = middle.1.evidence.offset_seconds_from_capture_start;
    let mut previous: Vec<(f64, f64)> = Vec::new();
    let mut previous_tile: Option<usize> = None;
    for (tile, _) in group {
        let tile = *tile;
        let candidates = modes.get(tile)?;
        let mut current = Vec::new();
        for candidate in candidates {
            if tile == middle.0
                && (candidate.evidence.offset_seconds_from_capture_start - selected_offset).abs()
                    <= 1.0
            {
                continue;
            }
            let local = candidate.score - policy.score_baseline_per_tile;
            let gap = previous_tile.map_or(0, |previous| tile.saturating_sub(previous + 1));
            if gap as f64 > policy.maximum_gap_seconds {
                continue;
            }
            let value = if previous_tile.is_none() {
                local - policy.segment_change_penalty
            } else if let Some(value) = previous
                .iter()
                .filter_map(|(offset, score)| {
                    let delta =
                        (candidate.evidence.offset_seconds_from_capture_start - offset).abs();
                    (delta <= continuity_limit(policy.max_drift_ppm) * (gap as f64 + 1.0))
                        .then_some(
                            score + local
                                - delta * 0.15
                                - gap as f64 * policy.gap_penalty_per_second,
                        )
                })
                .max_by(f64::total_cmp)
            {
                value
            } else {
                continue;
            };
            current.push((candidate.evidence.offset_seconds_from_capture_start, value));
        }
        if current.is_empty() {
            return None;
        }
        previous = current;
        previous_tile = Some(tile);
    }
    previous
        .iter()
        .map(|(_, score)| *score)
        .max_by(f64::total_cmp)
}

fn digest_json(value: &impl Serialize) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}
fn continuity_limit(ppm: f64) -> f64 {
    0.24 + ppm * 1e-6 * TILE_SECONDS
}
fn compare_nodes(a: &Node, b: &Node) -> std::cmp::Ordering {
    a.value
        .total_cmp(&b.value)
        .then_with(|| b.segment_count.cmp(&a.segment_count))
        .then_with(|| {
            b.offset
                .unwrap_or(f64::INFINITY)
                .total_cmp(&a.offset.unwrap_or(f64::INFINITY))
        })
}
fn better(a: &Node, b: &Node) -> bool {
    compare_nodes(a, b).is_gt()
}
fn unknown_reason(start: u64, end: u64, clock: &CaptureClock, ambiguous: &[bool]) -> String {
    let rate = clock.sample_rate as f64;
    let x =
        (start.saturating_sub(clock.first_sample) as f64 / rate / TILE_SECONDS).floor() as usize;
    let y = (end.saturating_sub(clock.first_sample) as f64 / rate / TILE_SECONDS).ceil() as usize;
    if ambiguous
        .get(x..y.min(ambiguous.len()))
        .is_some_and(|s| s.iter().any(|v| *v))
    {
        "competingOffsets".into()
    } else {
        "insufficientEvidence".into()
    }
}

/// Append supported landmark anchors to the caller's existing cue/watermark evidence,
/// then ask the shared recording-to-movie mapper to produce the final slices.
pub fn attach_and_map(
    request: &mut SyncRequest,
    report: &Report,
    clock: &CaptureClock,
    cancel: &dyn Fn() -> bool,
) -> Result<SyncReport> {
    request.validate()?;
    ensure!(
        request.movie.movie_digest == report.movie_digest,
        "invalid input: landmark report/movie mismatch"
    );
    let c = request
        .capture_clocks
        .iter()
        .find(|c| c.clock_id == clock.clock_id)
        .ok_or_else(|| anyhow::anyhow!("invalid input: capture clock missing from sync request"))?;
    ensure!(
        c == clock,
        "invalid input: capture clock differs from sync request"
    );
    ensure!(
        report
            .segments
            .iter()
            .all(|s| (s.rate - 1.0).abs() * 1_000_000.0 <= request.policy.max_drift_ppm + 1e-6),
        "invalid input: inferred drift exceeds shared mapper policy"
    );
    let tps = request.movie.ticks_per_second();
    let mut previous_end = None;
    for segment in &report.segments {
        ensure!(!cancel(), "source-audio mapper adapter cancelled");
        if let Some(end) = previous_end
            && end < segment.recording_start_sample
        {
            let id = evidence_id(report, "boundary", end);
            if !request.boundaries.iter().any(|b| b.evidence_id == id) {
                request.boundaries.push(SyncBoundary {
                    evidence_id: id,
                    clock_id: clock.clock_id.clone(),
                    sample_index: end,
                    kind: SyncBoundaryKind::CaptureInterrupted,
                });
            }
        }
        if segment.recording_start_sample > clock.first_sample {
            let kind = if previous_end == Some(segment.recording_start_sample) {
                SyncBoundaryKind::Seek
            } else {
                SyncBoundaryKind::CaptureInterrupted
            };
            let id = evidence_id(report, "boundary", segment.recording_start_sample);
            if !request.boundaries.iter().any(|b| b.evidence_id == id) {
                request.boundaries.push(SyncBoundary {
                    evidence_id: id,
                    clock_id: clock.clock_id.clone(),
                    sample_index: segment.recording_start_sample,
                    kind,
                });
            }
        }
        let step = (report.policy.anchor_step_seconds * clock.sample_rate as f64).round() as u64;
        let mut runs: Vec<(u64, u64)> = Vec::new();
        for evidence in &segment.evidence_spans {
            if let Some((_, end)) = runs.last_mut()
                && *end >= evidence.recording_start_sample
            {
                *end = (*end).max(evidence.recording_end_sample);
            } else {
                runs.push((
                    evidence.recording_start_sample,
                    evidence.recording_end_sample,
                ));
            }
        }
        for (start, end) in runs {
            let mut sample = start;
            while sample < end {
                ensure!(!cancel(), "source-audio mapper adapter cancelled");
                push_anchor(request, report, segment, clock, tps, sample);
                sample = sample.saturating_add(step.max(1));
            }
            if end > start {
                push_anchor(request, report, segment, clock, tps, end - 1);
            }
        }
        previous_end = Some(segment.recording_end_sample);
    }
    if let Some(last) = report.segments.last()
        && last.recording_end_sample < clock.last_sample
    {
        let id = evidence_id(report, "boundary", last.recording_end_sample);
        if !request.boundaries.iter().any(|b| b.evidence_id == id) {
            request.boundaries.push(SyncBoundary {
                evidence_id: id,
                clock_id: clock.clock_id.clone(),
                sample_index: last.recording_end_sample,
                kind: SyncBoundaryKind::CaptureInterrupted,
            });
        }
    }
    request.validate()?;
    request.map(
        &[SyncQuery {
            clock_id: clock.clock_id.clone(),
            start_sample: clock.first_sample,
            end_sample: clock.last_sample,
        }],
        cancel,
    )
}
fn push_anchor(
    request: &mut SyncRequest,
    report: &Report,
    segment: &Segment,
    clock: &CaptureClock,
    ticks_per_second: f64,
    requested_sample: u64,
) {
    let Some(sample) = compatible_sample(requested_sample, segment, clock) else {
        return;
    };
    if !segment
        .evidence_spans
        .iter()
        .any(|span| span.recording_start_sample <= sample && sample < span.recording_end_sample)
    {
        return;
    }
    let record_time = sample as f64 / clock.sample_rate as f64;
    let movie_pts =
        ((segment.rate * record_time + segment.offset_seconds) * ticks_per_second).round() as i64;
    let monotonic = clock
        .monotonic_origin_ns
        .saturating_add((sample as f64 / clock.sample_rate as f64 * 1e9).round() as u64);
    let id = evidence_id(report, "anchor", sample);
    if !request.anchors.iter().any(|a| a.evidence_id == id) {
        let minimum_confidence = request.policy.minimum_confidence;
        request.anchors.push(SyncAnchor {
            evidence_id: id,
            capture: CapturePosition {
                clock_id: clock.clock_id.clone(),
                sample_index: sample,
                monotonic_ns: monotonic,
            },
            movie_pts,
            source: SyncAnchorSource::PassiveSoundtrack,
            // This is the shared mapper's acceptance gate, not a probability estimate.
            confidence: minimum_confidence,
            movie_uncertainty_ticks: (segment.alignment_uncertainty_seconds * ticks_per_second)
                .ceil() as u64,
            capture_uncertainty_samples: 1,
            candidate_group_id: None,
        });
    }
}
fn compatible_sample(sample: u64, segment: &Segment, clock: &CaptureClock) -> Option<u64> {
    let expected =
        clock.monotonic_origin_ns as f64 + sample as f64 / clock.sample_rate as f64 * 1e9;
    if (expected.round() - expected).abs() <= clock.monotonic_tolerance_ns as f64 {
        return Some(sample);
    }
    let step = u64::from(clock.sample_rate) / gcd(u64::from(clock.sample_rate), 1_000_000_000);
    let lower = sample / step * step;
    let upper = lower.saturating_add(step);
    [lower, upper]
        .into_iter()
        .filter(|candidate| {
            *candidate >= segment.recording_start_sample
                && *candidate < segment.recording_end_sample
                && *candidate >= clock.first_sample
                && *candidate <= clock.last_sample
        })
        .min_by_key(|candidate| candidate.abs_diff(sample))
}
fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a.max(1)
}
fn evidence_id(report: &Report, kind: &str, sample: u64) -> String {
    let d = blake3::hash(
        format!(
            "{}:{}:{}:{}",
            report.index_digest, report.movie_digest, kind, sample
        )
        .as_bytes(),
    )
    .to_hex()
    .to_string();
    format!("landmark_{}", &d[..40])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        tv_review_sync::{CaptureClock, SyncMovieClock, SyncPolicy},
        tv_soundtrack_landmarks::{Recipe, Search, SearchPolicy},
    };
    fn fixture(offsets: Vec<(u32, i32, u32)>) -> (Search, CaptureClock, SyncMovieClock) {
        let clock = CaptureClock {
            clock_id: "clock_one".into(),
            sample_rate: 48_000,
            first_sample: 0,
            last_sample: 48_000 * 50,
            monotonic_origin_ns: 1_000_000_000,
            monotonic_tolerance_ns: 0,
        };
        let movie = SyncMovieClock {
            movie_digest: "a".repeat(64),
            time_base_numerator: 1,
            time_base_denominator: 1000,
            origin_pts: 0,
            duration_pts: 200_000,
        };
        let evidence = offsets
            .into_iter()
            .map(|(t, o, a)| OffsetEvidence {
                time_bin: t,
                offset_bin: o,
                offset_seconds_from_capture_start: o as f64 * 0.08,
                independent_anchors: a,
                supporting_seconds: 2,
                frequency_diversity: 8,
                rarity_weight: f64::from(a),
                saturated_anchors: 0,
            })
            .collect();
        (
            Search {
                schema_version: 1,
                recipe: Recipe::default(),
                search_policy: SearchPolicy::default(),
                movie_digest: movie.movie_digest.clone(),
                index_digest: "b".repeat(64),
                query_pcm_digest: "c".repeat(64),
                query_sample_count: 48_000 * 50 / 48_000 * 11_025,
                candidate_pair_count: 1000,
                query_landmark_count: 1000,
                matched_landmark_count: 500,
                complete: true,
                offsets: evidence,
            },
            clock,
            movie,
        )
    }
    #[test]
    fn weak_individually_insufficient_tiles_accumulate_but_scattered_votes_stay_unknown() {
        let weak = (0..8).map(|t| (t, 25, 4)).collect();
        let (s, c, m) = fixture(weak);
        let r = infer(&s, &c, &m, Policy::default()).unwrap();
        assert_eq!(r.segments.len(), 1);
        let scattered = vec![(0, 25, 4), (2, 25, 4), (4, 25, 4), (6, 25, 4)];
        let (s, c, m) = fixture(scattered);
        let r = infer(&s, &c, &m, Policy::default()).unwrap();
        assert!(r.segments.is_empty());
    }
    #[test]
    fn seeks_are_separate_and_repeated_modes_are_ambiguous() {
        let mut cases: Vec<_> = (0..8).map(|t| (t, 25, 10)).collect();
        cases.extend((8..16).map(|t| (t, 125, 10)));
        let (s, c, m) = fixture(cases);
        let r = infer(&s, &c, &m, Policy::default()).unwrap();
        assert_eq!(r.segments.len(), 2);
        let mut repeated: Vec<_> = (0..8).map(|t| (t, 25, 10)).collect();
        repeated.extend((0..8).map(|t| (t, 125, 10)));
        let (s, c, m) = fixture(repeated);
        let r = infer(&s, &c, &m, Policy::default()).unwrap();
        assert!(r.segments.is_empty());
        assert!(r.alternatives.iter().any(|a| a.ambiguous));
    }
    #[test]
    fn globally_accumulated_weak_margin_resolves_local_offset_ties() {
        let mut evidence = Vec::new();
        for t in 0..8 {
            evidence.push((t, 25, 10));
            evidence.push((t, 125, 9));
        }
        let (s, c, m) = fixture(evidence);
        let r = infer(&s, &c, &m, Policy::default()).unwrap();
        assert_eq!(r.segments.len(), 1);
        assert!(r.segments[0].offset_seconds < 10.0);
        assert!(r.segments[0].runner_up_margin.unwrap() > Policy::default().ambiguity_margin);
        assert!(r.alternatives.iter().all(|a| !a.ambiguous));
    }
    #[test]
    fn low_ranked_mode_accumulates_across_tiles_beyond_reported_local_top_twelve() {
        let mut evidence = Vec::new();
        for tile in 0u32..8 {
            // Twelve locally stronger but discontinuous modes outrank the
            // repeated true mode. Only a sequence-level vote can recover it.
            for mode in 0..127 {
                let offset_bin = 600 + tile as i32 * 140 + mode;
                evidence.push(OffsetEvidence {
                    time_bin: tile,
                    offset_bin,
                    offset_seconds_from_capture_start: f64::from(offset_bin) * 0.08,
                    independent_anchors: 4,
                    supporting_seconds: 2,
                    frequency_diversity: 8,
                    rarity_weight: 4.0,
                    saturated_anchors: 0,
                });
            }
            evidence.push(OffsetEvidence {
                time_bin: tile,
                offset_bin: 25,
                offset_seconds_from_capture_start: 2.0,
                independent_anchors: 3,
                supporting_seconds: 2,
                frequency_diversity: 4,
                rarity_weight: 4.0,
                saturated_anchors: 0,
            });
        }
        let (mut search, clock, movie) = fixture(Vec::new());
        search.offsets = evidence;
        let policy = Policy {
            minimum_tile_anchors: 3,
            ..Policy::default()
        };
        let report = infer(&search, &clock, &movie, policy).unwrap();
        assert_eq!(report.segments.len(), 1);
        assert!((report.segments[0].offset_seconds - 2.0).abs() < 0.01);
        assert!(
            report
                .alternatives
                .iter()
                .take(8)
                .all(|tile| tile.alternatives.len() <= MAX_REPORTED_ALTERNATIVES_PER_TILE + 1)
        );
        assert!(
            report.alternatives.iter().take(8).all(|tile| tile
                .alternatives
                .last()
                .unwrap()
                .offset_bin
                == 25)
        );
    }
    #[test]
    fn bounded_same_offset_gap_is_inferred_and_not_used_as_mapper_evidence() {
        let evidence = (0..3).chain(4..8).map(|t| (t, 25, 10)).collect();
        let (s, c, m) = fixture(evidence);
        let r = infer(&s, &c, &m, Policy::default()).unwrap();
        assert_eq!(r.segments.len(), 1);
        let segment = &r.segments[0];
        assert_eq!(segment.tile_count, 7);
        assert_eq!(segment.inferred_spans.len(), 1);
        assert_eq!(segment.inferred_spans[0].recording_start_sample, 3 * 48_000);
        assert_eq!(segment.inferred_spans[0].recording_end_sample, 4 * 48_000);
        assert!(segment.alignment_uncertainty_seconds >= 1.0);

        let mut request = SyncRequest {
            schema_version: 1,
            session_id: "gap_fixture".into(),
            target_digest: m.movie_digest.clone(),
            movie: m.clone(),
            capture_clocks: vec![c.clone()],
            anchors: vec![],
            boundaries: vec![],
            policy: SyncPolicy {
                minimum_confidence: 0.8,
                max_anchor_gap_seconds: 15.0,
                max_extrapolation_seconds: 0.0,
                max_drift_ppm: 2_000.0,
                residual_tolerance_seconds: 0.1,
                unobserved_uncertainty_ppm: 1_000.0,
            },
            consensus_segments: vec![],
        };
        attach_and_map(&mut request, &r, &c, &|| false).unwrap();
        assert!(
            !request
                .anchors
                .iter()
                .any(|a| { (3 * 48_000..4 * 48_000).contains(&a.capture.sample_index) })
        );
    }
    #[test]
    fn weak_support_accumulates_across_one_gap_but_not_across_a_seek() {
        let configure_weak = |search: &mut Search| {
            for evidence in &mut search.offsets {
                evidence.independent_anchors = 2;
                evidence.supporting_seconds = 1;
                evidence.frequency_diversity = 2;
                evidence.rarity_weight = 10.0;
            }
        };
        let policy = Policy {
            minimum_tile_anchors: 1,
            minimum_frequency_diversity: 1,
            ..Policy::default()
        };

        // The two runs are each shorter than the six-tile segment minimum.
        // Their combined nine weak observations pass the global support gate.
        let evidence = (0..4).chain(5..10).map(|tile| (tile, 25, 2)).collect();
        let (mut search, clock, movie) = fixture(evidence);
        configure_weak(&mut search);
        let report = infer(&search, &clock, &movie, policy.clone()).unwrap();
        assert_eq!(report.segments.len(), 1);
        assert_eq!(report.segments[0].tile_count, 9);
        assert_eq!(report.segments[0].inferred_spans.len(), 1);

        // A discontinuous offset across the same unsupported tile is a seek;
        // neither short side is promoted into a segment or bridged.
        let mut evidence: Vec<_> = (0..4).map(|tile| (tile, 25, 2)).collect();
        evidence.extend((5..10).map(|tile| (tile, 125, 2)));
        let (mut search, clock, movie) = fixture(evidence);
        configure_weak(&mut search);
        let report = infer(&search, &clock, &movie, policy).unwrap();
        assert!(report.segments.is_empty());
        assert!(
            report
                .alternatives
                .iter()
                .any(|tile| tile.alternatives.iter().any(|e| e.offset_bin == 25))
        );
        assert!(
            report
                .alternatives
                .iter()
                .any(|tile| tile.alternatives.iter().any(|e| e.offset_bin == 125))
        );
    }
    #[test]
    fn bounded_gap_does_not_merge_distinct_offset_modes() {
        let mut evidence: Vec<_> = (0..8).map(|t| (t, 25, 10)).collect();
        evidence.extend((9..17).map(|t| (t, 125, 10)));
        let (s, c, m) = fixture(evidence);
        let r = infer(&s, &c, &m, Policy::default()).unwrap();
        assert_eq!(r.segments.len(), 2);
        assert!((r.segments[0].offset_seconds - r.segments[1].offset_seconds).abs() > 5.0);
        assert!(r.segments.iter().all(|s| s.inferred_spans.is_empty()));
    }
    #[test]
    fn known_clock_drift_is_fitted_without_splitting_the_alignment() {
        let (mut s, mut c, mut m) =
            fixture((0u32..100).map(|t| (t, 25 + (t / 40) as i32, 12)).collect());
        s.query_sample_count = 600 * 11_025;
        c.last_sample = 600 * 48_000;
        m.duration_pts = 1_000_000;
        let r = infer(&s, &c, &m, Policy::default()).unwrap();
        assert_eq!(r.segments.len(), 1);
        let ppm = (r.segments[0].rate - 1.0) * 1_000_000.0;
        assert!((ppm - 2_000.0).abs() < 100.0, "{ppm}");
    }
    #[test]
    fn index_identity_mismatch_is_rejected_before_mapping() {
        let (s, c, mut m) = fixture((0..8).map(|t| (t, 25, 12)).collect());
        m.movie_digest = "d".repeat(64);
        assert!(infer(&s, &c, &m, Policy::default()).is_err());
    }
    #[test]
    fn mapper_adapter_preserves_existing_cues_and_maps_supported_segment() {
        let (s, c, m) = fixture((0..8).map(|t| (t, 25, 12)).collect());
        let manual = SyncAnchor {
            evidence_id: "manual_cue".into(),
            capture: CapturePosition {
                clock_id: c.clock_id.clone(),
                sample_index: 0,
                monotonic_ns: c.monotonic_origin_ns,
            },
            movie_pts: 0,
            source: SyncAnchorSource::ManualCue,
            confidence: 1.0,
            movie_uncertainty_ticks: 1,
            capture_uncertainty_samples: 1,
            candidate_group_id: None,
        };
        let mut request = SyncRequest {
            schema_version: 1,
            session_id: "session_one".into(),
            target_digest: m.movie_digest.clone(),
            movie: m.clone(),
            capture_clocks: vec![c.clone()],
            anchors: vec![manual.clone()],
            boundaries: vec![],
            policy: SyncPolicy {
                minimum_confidence: 0.8,
                max_anchor_gap_seconds: 15.0,
                max_extrapolation_seconds: 0.0,
                max_drift_ppm: 2000.0,
                residual_tolerance_seconds: 0.1,
                unobserved_uncertainty_ppm: 1000.0,
            },
            consensus_segments: vec![],
        };
        let r = infer(&s, &c, &m, Policy::default()).unwrap();
        assert_eq!(r.segments.len(), 1);
        assert_eq!(r.segments[0].status, "matched");
        assert_eq!(r.segments[0].recording_clock_id, c.clock_id);
        assert_eq!(r.segments[0].reference_movie_digest, m.movie_digest);
        assert_eq!(r.segments[0].evidence_spans.len(), 8);
        assert!(r.segments[0].inferred_spans.is_empty());
        assert_eq!(r.segments[0].boundary_uncertainty_seconds, 1.0);
        let report = attach_and_map(&mut request, &r, &c, &|| false).unwrap();
        assert!(!report.slices.is_empty());
        assert!(!request.anchors.is_empty());
        // Reattaching is deterministic and idempotent.
        let old = request.anchors.clone();
        attach_and_map(&mut request, &r, &c, &|| false).unwrap();
        assert_eq!(request.anchors, old);
        assert!(request.anchors.contains(&manual));
    }
}
