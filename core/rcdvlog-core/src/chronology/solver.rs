//! The deterministic chronology solver (D01-T008), plus contradiction
//! detection (D01-T013).
//!
//! `solve` is a pure function from persisted evidence to a derived solution:
//! per-asset trip time, local display time, confidence, evidence, uncertainty,
//! and one total order. Identical facts always produce an identical solution,
//! including identically ordered evidence, so the desktop, the CLI, and export
//! can never disagree about order.
//!
//! Purity rules, enforced by review: no I/O, no `SystemTime`/`Instant`, no
//! environment reads, no randomness, and no `HashMap`/`HashSet` anywhere on a
//! path that reaches output. Lookups use sorted slices and binary search.

use super::model::{
    AssetTimeHypothesis, ChronologyFacts, ClipTimeException, ClockEpoch, ConstraintStrength,
    TripPhase,
};
pub use super::phases::DisplayTime;
use super::phases::{coverage_evidence, display_time, parse_utc};
use chrono::{Duration, SecondsFormat};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// Confidence assigned to a trip time the user set explicitly through a clip
/// time exception. User authority outranks every derived hypothesis.
const CONFIDENCE_USER_EXCEPTION: f32 = 1.0;
/// Confidence assigned to a raw camera timestamp with no clock epoch covering
/// it: the camera's clock is believed but uncorrected.
const CONFIDENCE_RAW_ONLY: f32 = 0.5;
/// Confidence assigned when no trip time could be derived at all.
const CONFIDENCE_NONE: f32 = 0.0;
/// Uncertainty attributed to an uncorrected camera clock, in seconds.
const UNCERTAINTY_RAW_ONLY: u32 = 3600;

/// One reason the solver reached the conclusion it did. Ordered and
/// deduplicated by `(kind, detail)` so evidence never inherits the insertion
/// order of an unordered collection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    pub kind: String,
    pub detail: String,
    pub weight: f32,
}

impl Evidence {
    fn new(kind: &str, detail: impl Into<String>, weight: f32) -> Self {
        Self {
            kind: kind.to_string(),
            detail: detail.into(),
            weight,
        }
    }
}

/// The total order key. `asset_id` is unique within a project, so no two keys
/// can ever compare equal unless they name the same asset — ties are
/// structurally impossible, not merely unlikely.
///
/// A null `trip_time_millis` sorts *after* every known time, and a null
/// `filename_counter` after every known counter, so unplaced clips collect at
/// the end of their sequence instead of at the start of the trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderKey {
    pub trip_time_millis: Option<i64>,
    pub capture_sequence_id: String,
    pub filename_counter: Option<u64>,
    pub filename: String,
    pub asset_id: String,
}

/// `None` sorts last. `Option`'s derived ordering puts `None` first, which is
/// the wrong end for an unplaced clip.
fn compare_none_last<T: Ord>(left: &Option<T>, right: &Option<T>) -> Ordering {
    match (left, right) {
        (Some(a), Some(b)) => a.cmp(b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

impl Ord for OrderKey {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_none_last(&self.trip_time_millis, &other.trip_time_millis)
            .then_with(|| self.capture_sequence_id.cmp(&other.capture_sequence_id))
            .then_with(|| compare_none_last(&self.filename_counter, &other.filename_counter))
            .then_with(|| self.filename.cmp(&other.filename))
            .then_with(|| self.asset_id.cmp(&other.asset_id))
    }
}

impl PartialOrd for OrderKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The solver's conclusion about one asset. Every field is derived; none of it
/// is persisted except through the projection helper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetChronology {
    pub asset_id: String,
    pub raw_time: Option<String>,
    pub trip_time: Option<String>,
    pub display_time: Option<DisplayTime>,
    pub confidence: f32,
    pub evidence: Vec<Evidence>,
    pub uncertainty_seconds: u32,
    pub order_key: OrderKey,
}

/// Why a set of facts cannot all be true at once (D01-T013).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContradictionKind {
    /// Hard constraints form a cycle.
    Cycle,
    /// A hard constraint disagrees with derived times.
    IntervalConflict,
    /// A clip exception places a clip against a hard constraint.
    ExceptionConflict,
    /// A clip-anchored phase can no longer resolve both anchors into a usable
    /// chronological interval. The stored fallback remains visible.
    PhaseAnchorConflict,
}

/// One contradiction, naming the conflicting evidence. Hard contradictions
/// block apply; the message is written to be read by the person who has to fix
/// it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contradiction {
    pub kind: ContradictionKind,
    pub strength: ConstraintStrength,
    pub asset_ids: Vec<String>,
    pub evidence: Vec<Evidence>,
    pub message: String,
}

/// A complete derived chronology for one project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChronologySolution {
    pub assets: Vec<AssetChronology>,
    pub contradictions: Vec<Contradiction>,
}

/// Builds a sorted `(key, index)` lookup table. Binary search over this
/// replaces the `HashMap` a solver would normally reach for; the sort makes the
/// structure itself deterministic.
fn sorted_index<'a, T, F>(items: &'a [T], key: F) -> Vec<(&'a str, usize)>
where
    F: Fn(&'a T) -> &'a str,
{
    let mut index: Vec<(&str, usize)> = items
        .iter()
        .enumerate()
        .map(|(position, item)| (key(item), position))
        .collect();
    index.sort_unstable();
    index
}

fn lookup(index: &[(&str, usize)], key: &str) -> Option<usize> {
    index
        .binary_search_by(|(candidate, _)| (*candidate).cmp(key))
        .ok()
        .map(|position| index[position].1)
}

/// The epoch governing one asset, by the frozen range rule.
///
/// An epoch applies only inside its own capture sequence. A bounded range is
/// `[start_counter, end_counter]` inclusive; an asset with a NULL counter is
/// never inside a bounded range and is affected only by an open-ended epoch on
/// its sequence. When several epochs match, the most specific one wins — the
/// greatest `start_counter`, tie-broken by epoch id.
///
/// Crate-visible so the explanation surface (D01-T014) names the *same* epoch
/// the solver actually applied. An explanation that named a different one would
/// be worse than no explanation.
pub(crate) fn governing_epoch_for<'a>(
    epochs: &'a [ClockEpoch],
    sequence_id: &str,
    counter: Option<u64>,
) -> Option<&'a ClockEpoch> {
    let mut winner: Option<&ClockEpoch> = None;
    for epoch in epochs {
        if epoch.capture_sequence_id != sequence_id {
            continue;
        }
        let applies = match counter {
            Some(value) => {
                value >= epoch.start_counter && epoch.end_counter.is_none_or(|end| value <= end)
            }
            None => epoch.end_counter.is_none(),
        };
        if !applies {
            continue;
        }
        let better = match winner {
            None => true,
            Some(current) => {
                (epoch.start_counter, &epoch.id) > (current.start_counter, &current.id)
            }
        };
        if better {
            winner = Some(epoch);
        }
    }
    winner
}

fn find_exception<'a>(
    exceptions: &'a [ClipTimeException],
    index: &[(&str, usize)],
    asset_id: &str,
) -> Option<&'a ClipTimeException> {
    lookup(index, asset_id).map(|position| &exceptions[position])
}

/// Renders an RFC 3339 UTC string with millisecond precision, matching the
/// existing `assets.best_time` format.
fn render_utc(value: chrono::DateTime<chrono::Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Derives everything the solver knows about one asset, before ordering.
fn solve_asset(
    asset: &AssetTimeHypothesis,
    epochs: &[ClockEpoch],
    exceptions: &[ClipTimeException],
    exception_index: &[(&str, usize)],
    phases: &[TripPhase],
) -> AssetChronology {
    let sequence_id = asset.capture_sequence_id.clone().unwrap_or_default();
    let mut evidence: Vec<Evidence> = Vec::new();

    let raw_instant = asset.captured_at_raw.as_deref().and_then(parse_utc);
    if let Some(raw) = asset.captured_at_raw.as_deref() {
        evidence.push(Evidence::new("raw_timestamp", raw, CONFIDENCE_RAW_ONLY));
    } else {
        evidence.push(Evidence::new(
            "no_raw_timestamp",
            "camera supplied no capture timestamp",
            CONFIDENCE_NONE,
        ));
    }

    let exception = find_exception(exceptions, exception_index, &asset.asset_id);
    let epoch = governing_epoch_for(epochs, &sequence_id, asset.filename_counter);

    // A clip time exception is user authority and overrides the derived
    // hypothesis outright; a clock epoch shifts the raw timestamp.
    let (trip_instant, confidence, uncertainty) = match exception {
        Some(override_time) => {
            evidence.push(Evidence::new(
                "clip_time_exception",
                if override_time.reason.is_empty() {
                    override_time.trip_time.clone()
                } else {
                    format!("{}: {}", override_time.trip_time, override_time.reason)
                },
                CONFIDENCE_USER_EXCEPTION,
            ));
            (
                parse_utc(&override_time.trip_time),
                CONFIDENCE_USER_EXCEPTION,
                0,
            )
        }
        None => match (raw_instant, epoch) {
            (Some(raw), Some(applied)) => {
                for entry in &applied.evidence {
                    evidence.push(Evidence::new(
                        "clock_epoch_evidence",
                        entry,
                        applied.confidence,
                    ));
                }
                evidence.push(Evidence::new(
                    "clock_epoch",
                    format!(
                        "{} applied {:+}s over {}",
                        applied.id, applied.offset_seconds, applied.capture_sequence_id
                    ),
                    applied.confidence,
                ));
                let shifted = raw + Duration::seconds(applied.offset_seconds);
                let slack = ((1.0 - applied.confidence.clamp(0.0, 1.0))
                    * UNCERTAINTY_RAW_ONLY as f32)
                    .round() as u32;
                (Some(shifted), applied.confidence.clamp(0.0, 1.0), slack)
            }
            (Some(raw), None) => (Some(raw), CONFIDENCE_RAW_ONLY, UNCERTAINTY_RAW_ONLY),
            (None, _) => (None, CONFIDENCE_NONE, u32::MAX),
        },
    };

    let trip_time = trip_instant.map(render_utc);

    if let Some(placed) = trip_time.as_deref() {
        match coverage_evidence(phases, placed) {
            Some((phase_id, true)) => evidence.push(Evidence::new(
                "trip_phase",
                format!("{phase_id} supplied the display timezone"),
                1.0,
            )),
            Some((phase_id, false)) => evidence.push(Evidence::new(
                "trip_phase",
                format!("{phase_id} named no resolvable timezone; displayed as UTC"),
                0.5,
            )),
            None => evidence.push(Evidence::new(
                "trip_phase",
                "no phase covers this trip time; displayed as UTC",
                0.5,
            )),
        }
    }

    let display = display_time(phases, trip_time.as_deref());

    // Ordered and deduplicated by a fixed rule, never by insertion order.
    evidence.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.detail.cmp(&right.detail))
    });
    evidence.dedup_by(|left, right| left.kind == right.kind && left.detail == right.detail);

    AssetChronology {
        asset_id: asset.asset_id.clone(),
        raw_time: asset.captured_at_raw.clone(),
        trip_time: trip_time.clone(),
        display_time: display,
        confidence,
        evidence,
        uncertainty_seconds: uncertainty,
        order_key: OrderKey {
            trip_time_millis: trip_instant.map(|value| value.timestamp_millis()),
            capture_sequence_id: sequence_id,
            filename_counter: asset.filename_counter,
            filename: asset.filename.clone(),
            asset_id: asset.asset_id.clone(),
        },
    }
}

/// The solver-owned projection for one asset when only its own chronology
/// facts need refreshing. Import materialization uses this rather than
/// reimplementing epoch/exception precedence or solving every project asset.
pub(crate) fn solve_one_asset(
    asset: &AssetTimeHypothesis,
    epochs: &[ClockEpoch],
    exceptions: &[ClipTimeException],
) -> AssetChronology {
    let exception_index = sorted_index(exceptions, |exception| exception.asset_id.as_str());
    solve_asset(asset, epochs, exceptions, &exception_index, &[])
}

fn apply_phase_context(asset: &mut AssetChronology, phases: &[TripPhase]) {
    asset.evidence.retain(|entry| entry.kind != "trip_phase");
    if let Some(placed) = asset.trip_time.as_deref() {
        match coverage_evidence(phases, placed) {
            Some((phase_id, true)) => asset.evidence.push(Evidence::new(
                "trip_phase",
                format!("{phase_id} supplied the display timezone"),
                1.0,
            )),
            Some((phase_id, false)) => asset.evidence.push(Evidence::new(
                "trip_phase",
                format!("{phase_id} named no resolvable timezone; displayed as UTC"),
                0.5,
            )),
            None => asset.evidence.push(Evidence::new(
                "trip_phase",
                "no phase covers this trip time; displayed as UTC",
                0.5,
            )),
        }
    }
    asset.display_time = display_time(phases, asset.trip_time.as_deref());
    asset.evidence.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.detail.cmp(&right.detail))
    });
    asset
        .evidence
        .dedup_by(|left, right| left.kind == right.kind && left.detail == right.detail);
}

/// Resolves clip-backed boundaries against the current solved chronology.
/// Fixed-time phases pass through unchanged. If an anchor is missing,
/// unplaced, or crosses the other boundary, the entire phase keeps its stored
/// fallback interval and a soft contradiction makes that fallback visible.
pub fn resolve_anchored_phases(
    phases: &[TripPhase],
    assets: &[AssetChronology],
) -> (Vec<TripPhase>, Vec<Contradiction>) {
    let mut resolved = Vec::with_capacity(phases.len());
    let mut contradictions = Vec::new();

    for phase in phases {
        let mut candidate = phase.clone();
        let mut failed = Vec::new();
        if let Some(asset_id) = phase.start_anchor_asset_id.as_deref() {
            match assets.iter().find(|asset| asset.asset_id == asset_id) {
                Some(asset) if asset.trip_time.is_some() => {
                    candidate.start_trip_time = asset.trip_time.clone().unwrap_or_default();
                }
                _ => failed.push(format!("start anchor {asset_id} has no placed trip time")),
            }
        }
        if let Some(asset_id) = phase.end_anchor_asset_id.as_deref() {
            match assets.iter().find(|asset| asset.asset_id == asset_id) {
                Some(asset) if asset.trip_time.is_some() => {
                    candidate.end_trip_time = asset.trip_time.clone().unwrap_or_default();
                }
                _ => failed.push(format!("end anchor {asset_id} has no placed trip time")),
            }
        }

        let bounds_valid = match (
            parse_utc(&candidate.start_trip_time),
            parse_utc(&candidate.end_trip_time),
        ) {
            (Some(start), Some(end)) if start <= end => true,
            (Some(_), Some(_)) => {
                failed.push("start anchor now falls after the end anchor".to_string());
                false
            }
            _ => {
                failed.push("resolved phase bounds are not valid timestamps".to_string());
                false
            }
        };

        if failed.is_empty() && bounds_valid {
            resolved.push(candidate);
        } else {
            let asset_ids = [
                phase.start_anchor_asset_id.clone(),
                phase.end_anchor_asset_id.clone(),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            contradictions.push(Contradiction {
                kind: ContradictionKind::PhaseAnchorConflict,
                strength: ConstraintStrength::Soft,
                asset_ids,
                evidence: vec![Evidence::new("phase_anchor", failed.join("; "), 1.0)],
                message: format!(
                    "{} kept its last resolved range because its clip boundary needs review",
                    phase.label
                ),
            });
            resolved.push(phase.clone());
        }
    }

    (resolved, contradictions)
}

/// Reorders solved assets so hard constraints hold, keeping the result as
/// close to `OrderKey` order as the constraints allow.
///
/// This is Kahn's algorithm with the ready set kept sorted by `OrderKey`
/// position, which yields the unique lexicographically smallest topological
/// order — deterministic without any tie-break beyond `OrderKey` itself. Assets
/// left over belong to a cycle; they are appended in `OrderKey` order and
/// reported as a `Cycle` contradiction rather than silently dropped.
fn apply_hard_constraints(solved: &mut Vec<AssetChronology>, facts: &ChronologyFacts) {
    let hard: Vec<(usize, usize)> = {
        let index = sorted_index(solved, |asset| asset.asset_id.as_str());
        let mut edges: Vec<(usize, usize)> = facts
            .constraints
            .iter()
            .filter(|constraint| constraint.strength == ConstraintStrength::Hard)
            .filter_map(|constraint| {
                let earlier = lookup(&index, &constraint.earlier_asset_id)?;
                let later = lookup(&index, &constraint.later_asset_id)?;
                (earlier != later).then_some((earlier, later))
            })
            .collect();
        edges.sort_unstable();
        edges.dedup();
        edges
    };
    if hard.is_empty() {
        return;
    }

    let mut indegree = vec![0usize; solved.len()];
    for (_, later) in &hard {
        indegree[*later] += 1;
    }

    // `ready` holds positions in current (OrderKey) order; popping the smallest
    // keeps the output lexicographically minimal.
    let mut ready: Vec<usize> = (0..solved.len())
        .filter(|position| indegree[*position] == 0)
        .collect();
    let mut order: Vec<usize> = Vec::with_capacity(solved.len());
    while !ready.is_empty() {
        ready.sort_unstable();
        let next = ready.remove(0);
        order.push(next);
        for (earlier, later) in &hard {
            if *earlier == next {
                indegree[*later] -= 1;
                if indegree[*later] == 0 {
                    ready.push(*later);
                }
            }
        }
    }

    if order.len() < solved.len() {
        let mut placed = vec![false; solved.len()];
        for position in &order {
            placed[*position] = true;
        }
        for (position, seated) in placed.iter().enumerate() {
            if !seated {
                order.push(position);
            }
        }
    }

    let mut reordered: Vec<Option<AssetChronology>> = solved.drain(..).map(Some).collect();
    for position in order {
        if let Some(asset) = reordered[position].take() {
            solved.push(asset);
        }
    }
}

/// Detects contradictions between constraints, derived times, and exceptions
/// (D01-T013). Deterministic: ordered by kind, then by the sorted asset id list.
pub fn detect(facts: &ChronologyFacts, solved: &[AssetChronology]) -> Vec<Contradiction> {
    let index = sorted_index(solved, |asset| asset.asset_id.as_str());
    let exception_index = sorted_index(&facts.exceptions, |exception| exception.asset_id.as_str());
    let mut found: Vec<Contradiction> = Vec::new();

    // Cycle detection over hard constraints only. Soft constraints are hints
    // and can never block an apply.
    let mut edges: Vec<(usize, usize)> = facts
        .constraints
        .iter()
        .filter(|constraint| constraint.strength == ConstraintStrength::Hard)
        .filter_map(|constraint| {
            let earlier = lookup(&index, &constraint.earlier_asset_id)?;
            let later = lookup(&index, &constraint.later_asset_id)?;
            (earlier != later).then_some((earlier, later))
        })
        .collect();
    edges.sort_unstable();
    edges.dedup();

    let mut indegree = vec![0usize; solved.len()];
    for (_, later) in &edges {
        indegree[*later] += 1;
    }
    let mut ready: Vec<usize> = (0..solved.len())
        .filter(|position| indegree[*position] == 0)
        .collect();
    let mut settled = 0usize;
    while let Some(next) = ready.pop() {
        settled += 1;
        for (earlier, later) in &edges {
            if *earlier == next {
                indegree[*later] -= 1;
                if indegree[*later] == 0 {
                    ready.push(*later);
                }
            }
        }
    }
    if settled < solved.len() {
        let mut cyclic: Vec<String> = (0..solved.len())
            .filter(|position| indegree[*position] > 0)
            .map(|position| solved[position].asset_id.clone())
            .collect();
        cyclic.sort();
        found.push(Contradiction {
            kind: ContradictionKind::Cycle,
            strength: ConstraintStrength::Hard,
            evidence: cyclic
                .iter()
                .map(|id| Evidence::new("cycle_member", id.clone(), 1.0))
                .collect(),
            message: format!(
                "{} clips are bound in a cycle of hard ordering constraints, so no order can satisfy all of them",
                cyclic.len()
            ),
            asset_ids: cyclic,
        });
    }

    // A hard constraint that disagrees with the derived times. An exception on
    // either side makes it an `ExceptionConflict`, because that is the fact the
    // user set by hand and the one they need to see named.
    for constraint in &facts.constraints {
        if constraint.strength != ConstraintStrength::Hard {
            continue;
        }
        let (Some(earlier), Some(later)) = (
            lookup(&index, &constraint.earlier_asset_id),
            lookup(&index, &constraint.later_asset_id),
        ) else {
            continue;
        };
        if earlier == later {
            continue;
        }
        let (Some(earlier_time), Some(later_time)) = (
            solved[earlier].order_key.trip_time_millis,
            solved[later].order_key.trip_time_millis,
        ) else {
            continue;
        };
        if earlier_time <= later_time {
            continue;
        }
        let touches_exception = lookup(&exception_index, &constraint.earlier_asset_id).is_some()
            || lookup(&exception_index, &constraint.later_asset_id).is_some();
        let mut asset_ids = vec![
            constraint.earlier_asset_id.clone(),
            constraint.later_asset_id.clone(),
        ];
        asset_ids.sort();
        found.push(Contradiction {
            kind: if touches_exception {
                ContradictionKind::ExceptionConflict
            } else {
                ContradictionKind::IntervalConflict
            },
            strength: ConstraintStrength::Hard,
            asset_ids,
            evidence: vec![
                Evidence::new("constraint", constraint.id.clone(), 1.0),
                Evidence::new(
                    "derived_time",
                    format!(
                        "{} is derived at {:?} but must precede {} derived at {:?}",
                        constraint.earlier_asset_id,
                        solved[earlier].trip_time,
                        constraint.later_asset_id,
                        solved[later].trip_time
                    ),
                    1.0,
                ),
            ],
            message: format!(
                "{} must come before {}, but the derived times place it after",
                constraint.earlier_asset_id, constraint.later_asset_id
            ),
        });
    }

    found.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.asset_ids.cmp(&right.asset_ids))
    });
    found.dedup_by(|left, right| left.kind == right.kind && left.asset_ids == right.asset_ids);
    found
}

/// Turns persisted chronology evidence into a derived solution.
///
/// Pure. Deterministic. Total. The pipeline follows `PRD.md` §9.5: capture
/// chains, raw hypotheses, clock epochs, trip-context timezones, user
/// constraints, contradiction detection, best times, total order, confidence
/// and evidence.
pub fn solve(facts: &ChronologyFacts) -> ChronologySolution {
    let exception_index = sorted_index(&facts.exceptions, |exception| exception.asset_id.as_str());

    let mut assets: Vec<AssetChronology> = facts
        .assets
        .iter()
        .map(|asset| {
            solve_asset(
                asset,
                &facts.epochs,
                &facts.exceptions,
                &exception_index,
                &[],
            )
        })
        .collect();

    // `OrderKey` is total, so this sort is unambiguous with no tie-break left
    // to chance. `sort_unstable_by` is safe precisely because ties cannot occur.
    assets.sort_unstable_by(|left, right| left.order_key.cmp(&right.order_key));

    let mut contradictions = detect(facts, &assets);
    apply_hard_constraints(&mut assets, facts);
    let (resolved_phases, mut phase_contradictions) =
        resolve_anchored_phases(&facts.phases, &assets);
    for asset in &mut assets {
        apply_phase_context(asset, &resolved_phases);
    }
    contradictions.append(&mut phase_contradictions);
    contradictions.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.asset_ids.cmp(&right.asset_ids))
    });

    ChronologySolution {
        assets,
        contradictions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chronology::model::{DisplayMode, PhaseKind};

    fn asset(
        id: &str,
        filename: &str,
        raw: Option<&str>,
        counter: Option<u64>,
    ) -> AssetTimeHypothesis {
        AssetTimeHypothesis {
            asset_id: id.to_string(),
            filename: filename.to_string(),
            captured_at_raw: raw.map(str::to_string),
            capture_sequence_id: Some("seq_a".to_string()),
            filename_counter: counter,
            duration_seconds: 1.0,
        }
    }

    fn facts(assets: Vec<AssetTimeHypothesis>) -> ChronologyFacts {
        ChronologyFacts {
            project_id: "project".to_string(),
            assets,
            sequences: Vec::new(),
            epochs: Vec::new(),
            exceptions: Vec::new(),
            phases: Vec::new(),
            constraints: Vec::new(),
        }
    }

    #[test]
    fn empty_facts_solve_to_an_empty_solution() {
        let solution = solve(&facts(Vec::new()));
        assert!(solution.assets.is_empty());
        assert!(solution.contradictions.is_empty());
    }

    #[test]
    fn assets_without_epochs_order_by_raw_time_then_counter() {
        let solution = solve(&facts(vec![
            asset("b", "C0002.MP4", Some("2026-01-01T10:00:00Z"), Some(2)),
            asset("a", "C0001.MP4", Some("2026-01-01T09:00:00Z"), Some(1)),
            asset("c", "C0003.MP4", None, Some(3)),
        ]));
        let order: Vec<&str> = solution
            .assets
            .iter()
            .map(|entry| entry.asset_id.as_str())
            .collect();
        assert_eq!(order, vec!["a", "b", "c"]);
        // A missing raw timestamp is not an error: null trip time, minimum
        // confidence, stable position at the end.
        assert_eq!(solution.assets[2].trip_time, None);
        assert_eq!(solution.assets[2].confidence, CONFIDENCE_NONE);
    }

    #[test]
    fn a_clock_epoch_shifts_only_its_own_counter_range() {
        let mut base = facts(vec![
            asset("a", "C0001.MP4", Some("2026-01-01T09:00:00Z"), Some(1)),
            asset("b", "C0009.MP4", Some("2026-01-01T10:00:00Z"), Some(9)),
        ]);
        base.epochs.push(ClockEpoch {
            id: "epoch_1".to_string(),
            capture_sequence_id: "seq_a".to_string(),
            start_counter: 5,
            end_counter: None,
            offset_seconds: -3600,
            confidence: 1.0,
            evidence: vec!["user_set".to_string()],
            label: "card two".to_string(),
        });
        let solution = solve(&base);
        let a = &solution.assets.iter().find(|e| e.asset_id == "a").unwrap();
        let b = &solution.assets.iter().find(|e| e.asset_id == "b").unwrap();
        assert_eq!(a.trip_time.as_deref(), Some("2026-01-01T09:00:00.000Z"));
        assert_eq!(b.trip_time.as_deref(), Some("2026-01-01T09:00:00.000Z"));
        assert_eq!(a.confidence, CONFIDENCE_RAW_ONLY);
        assert_eq!(b.confidence, 1.0);
        // Ties in trip time are broken by sequence, counter, filename, id.
        assert_ne!(a.order_key, b.order_key);
    }

    #[test]
    fn a_null_counter_asset_is_untouched_by_a_bounded_epoch() {
        let mut base = facts(vec![asset(
            "a",
            "clip.mp4",
            Some("2026-01-01T09:00:00Z"),
            None,
        )]);
        base.epochs.push(ClockEpoch {
            id: "epoch_1".to_string(),
            capture_sequence_id: "seq_a".to_string(),
            start_counter: 0,
            end_counter: Some(9999),
            offset_seconds: -3600,
            confidence: 1.0,
            evidence: Vec::new(),
            label: String::new(),
        });
        let solution = solve(&base);
        assert_eq!(
            solution.assets[0].trip_time.as_deref(),
            Some("2026-01-01T09:00:00.000Z")
        );
    }

    #[test]
    fn a_clip_time_exception_overrides_the_epoch() {
        let mut base = facts(vec![asset(
            "a",
            "C0001.MP4",
            Some("2026-01-01T09:00:00Z"),
            Some(1),
        )]);
        base.epochs.push(ClockEpoch {
            id: "epoch_1".to_string(),
            capture_sequence_id: "seq_a".to_string(),
            start_counter: 0,
            end_counter: None,
            offset_seconds: -3600,
            confidence: 1.0,
            evidence: Vec::new(),
            label: String::new(),
        });
        base.exceptions.push(ClipTimeException {
            asset_id: "a".to_string(),
            trip_time: "2026-02-02T02:02:02Z".to_string(),
            reason: "user corrected".to_string(),
        });
        let solution = solve(&base);
        assert_eq!(
            solution.assets[0].trip_time.as_deref(),
            Some("2026-02-02T02:02:02.000Z")
        );
        assert_eq!(solution.assets[0].confidence, CONFIDENCE_USER_EXCEPTION);
        assert_eq!(solution.assets[0].uncertainty_seconds, 0);
    }

    #[test]
    fn shuffled_facts_produce_an_identical_solution() {
        let mut base = facts(
            (0..40)
                .map(|n| {
                    asset(
                        &format!("asset_{n:03}"),
                        &format!("C{n:04}.MP4"),
                        Some("2026-01-01T09:00:00Z"),
                        Some(n as u64),
                    )
                })
                .collect(),
        );
        base.phases.push(TripPhase {
            id: "phase_1".to_string(),
            label: "Tokyo".to_string(),
            kind: PhaseKind::Location,
            start_anchor_asset_id: None,
            end_anchor_asset_id: None,
            start_trip_time: "2026-01-01T00:00:00Z".to_string(),
            end_trip_time: "2026-01-02T00:00:00Z".to_string(),
            timezone: Some("Asia/Tokyo".to_string()),
            origin_timezone: None,
            destination_timezone: None,
            display_mode: DisplayMode::Local,
        });
        let expected = solve(&base);
        // A cheap deterministic shuffle: rotating and reversing exercises every
        // input permutation the loader could plausibly hand us.
        for rotation in 1..32 {
            let mut shuffled = base.clone();
            let count = shuffled.assets.len();
            shuffled.assets.rotate_left(rotation % count);
            if rotation % 2 == 0 {
                shuffled.assets.reverse();
            }
            assert_eq!(solve(&shuffled), expected, "rotation {rotation}");
        }
    }

    #[test]
    fn a_hard_constraint_reorders_against_derived_time_and_is_reported() {
        let mut base = facts(vec![
            asset("a", "C0001.MP4", Some("2026-01-01T09:00:00Z"), Some(1)),
            asset("b", "C0002.MP4", Some("2026-01-01T10:00:00Z"), Some(2)),
        ]);
        base.constraints
            .push(super::super::model::EventOrderConstraint {
                id: "constraint_1".to_string(),
                earlier_asset_id: "b".to_string(),
                later_asset_id: "a".to_string(),
                strength: ConstraintStrength::Hard,
                source: "user".to_string(),
            });
        let solution = solve(&base);
        let order: Vec<&str> = solution
            .assets
            .iter()
            .map(|entry| entry.asset_id.as_str())
            .collect();
        assert_eq!(order, vec!["b", "a"]);
        assert_eq!(solution.contradictions.len(), 1);
        assert_eq!(
            solution.contradictions[0].kind,
            ContradictionKind::IntervalConflict
        );
    }

    #[test]
    fn a_cycle_of_hard_constraints_is_reported_and_drops_nothing() {
        let mut base = facts(vec![
            asset("a", "C0001.MP4", Some("2026-01-01T09:00:00Z"), Some(1)),
            asset("b", "C0002.MP4", Some("2026-01-01T10:00:00Z"), Some(2)),
        ]);
        for (earlier, later) in [("a", "b"), ("b", "a")] {
            base.constraints
                .push(super::super::model::EventOrderConstraint {
                    id: format!("constraint_{earlier}{later}"),
                    earlier_asset_id: earlier.to_string(),
                    later_asset_id: later.to_string(),
                    strength: ConstraintStrength::Hard,
                    source: "user".to_string(),
                });
        }
        let solution = solve(&base);
        assert_eq!(solution.assets.len(), 2);
        assert!(
            solution
                .contradictions
                .iter()
                .any(|entry| entry.kind == ContradictionKind::Cycle)
        );
    }

    #[test]
    fn display_time_comes_from_the_covering_phase() {
        let mut base = facts(vec![asset(
            "a",
            "C0001.MP4",
            Some("2026-01-15T03:00:00Z"),
            Some(1),
        )]);
        base.phases.push(TripPhase {
            id: "phase_1".to_string(),
            label: "Tokyo".to_string(),
            kind: PhaseKind::Location,
            start_anchor_asset_id: None,
            end_anchor_asset_id: None,
            start_trip_time: "2026-01-01T00:00:00Z".to_string(),
            end_trip_time: "2026-02-01T00:00:00Z".to_string(),
            timezone: Some("Asia/Tokyo".to_string()),
            origin_timezone: None,
            destination_timezone: None,
            display_mode: DisplayMode::Local,
        });
        let solution = solve(&base);
        let display = solution.assets[0].display_time.as_ref().unwrap();
        assert_eq!(display.primary, "2026-01-15T12:00:00+09:00");
        assert_eq!(display.timezone, "Asia/Tokyo");
        assert_eq!(display.phase_id.as_deref(), Some("phase_1"));
    }
}
