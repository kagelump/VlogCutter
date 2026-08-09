//! Acceptance oracle for the deterministic chronology solver (D01-T008),
//! trip-phase display (D01-T012), and contradiction reporting (D01-T013).

use rcdvlog_core::{
    AssetTimeHypothesis, ChronologyFacts, ClipTimeException, ClockEpoch, ConstraintStrength,
    ContradictionKind, DisplayMode, EventOrderConstraint, PhaseKind, TripPhase, solve,
};
use std::time::Instant;

fn asset(
    id: &str,
    sequence: &str,
    filename: &str,
    raw: Option<&str>,
    counter: Option<u64>,
) -> AssetTimeHypothesis {
    AssetTimeHypothesis {
        asset_id: id.to_string(),
        filename: filename.to_string(),
        captured_at_raw: raw.map(str::to_string),
        capture_sequence_id: Some(sequence.to_string()),
        filename_counter: counter,
        duration_seconds: 4.0,
    }
}

fn empty(project: &str) -> ChronologyFacts {
    ChronologyFacts {
        project_id: project.to_string(),
        assets: Vec::new(),
        sequences: Vec::new(),
        epochs: Vec::new(),
        exceptions: Vec::new(),
        phases: Vec::new(),
        constraints: Vec::new(),
    }
}

/// Oracle 1 and 2: the same facts solve identically, and shuffling every
/// collection independently changes nothing.
#[test]
fn shuffling_every_collection_leaves_the_solution_identical() {
    let mut facts = empty("project_shuffle");
    for n in 0..120u64 {
        facts.assets.push(asset(
            &format!("asset_{n:04}"),
            if n % 3 == 0 { "seq_a" } else { "seq_b" },
            &format!("C{n:04}.MP4"),
            Some("2026-03-01T12:00:00Z"),
            Some(n),
        ));
    }
    for n in 0..6u64 {
        facts.epochs.push(ClockEpoch {
            id: format!("epoch_{n}"),
            capture_sequence_id: if n % 2 == 0 { "seq_a" } else { "seq_b" }.to_string(),
            start_counter: n * 20,
            end_counter: Some(n * 20 + 19),
            offset_seconds: (n as i64) * 60 - 180,
            confidence: 0.9,
            evidence: vec![format!("evidence_{n}")],
            label: format!("epoch {n}"),
        });
    }
    facts.phases.push(TripPhase {
        id: "phase_tokyo".to_string(),
        label: "Tokyo".to_string(),
        kind: PhaseKind::Location,
        start_anchor_asset_id: None,
        end_anchor_asset_id: None,
        start_trip_time: "2026-03-01T00:00:00Z".to_string(),
        end_trip_time: "2026-03-02T00:00:00Z".to_string(),
        timezone: Some("Asia/Tokyo".to_string()),
        origin_timezone: None,
        destination_timezone: None,
        display_mode: DisplayMode::Local,
    });
    facts.exceptions.push(ClipTimeException {
        asset_id: "asset_0005".to_string(),
        trip_time: "2026-03-01T13:30:00Z".to_string(),
        reason: "user placed".to_string(),
    });

    let expected = solve(&facts);
    assert_eq!(solve(&facts), expected, "solving twice must be identical");

    // A deterministic pseudo-shuffle across 64 distinct permutations. Every
    // collection is permuted independently, so a solver that leaned on one
    // collection's incoming order would break here.
    for seed in 1..=64usize {
        let mut shuffled = facts.clone();
        permute(&mut shuffled.assets, seed);
        permute(&mut shuffled.epochs, seed * 7 + 1);
        permute(&mut shuffled.phases, seed * 3 + 2);
        permute(&mut shuffled.exceptions, seed * 5 + 3);
        permute(&mut shuffled.constraints, seed * 11 + 4);
        assert_eq!(solve(&shuffled), expected, "seed {seed}");
    }
}

/// A reproducible permutation with no `rand` dependency: a full-cycle stride
/// swap driven only by the seed.
fn permute<T>(items: &mut [T], seed: usize) {
    if items.len() < 2 {
        return;
    }
    let len = items.len();
    for index in 0..len {
        let partner = (index * 31 + seed * 17 + 7) % len;
        items.swap(index, partner);
    }
}

/// Oracle 3: `OrderKey` is total. Assets identical in every field except id
/// still compare strictly, and no pair is ever equal without being the same
/// asset.
#[test]
fn order_keys_are_total_even_for_otherwise_identical_assets() {
    let mut facts = empty("project_total");
    for n in 0..200u64 {
        facts.assets.push(asset(
            &format!("asset_{n:04}"),
            "seq_a",
            "SAME.MP4",
            Some("2026-03-01T12:00:00Z"),
            Some(7),
        ));
    }
    let solution = solve(&facts);
    let keys: Vec<_> = solution
        .assets
        .iter()
        .map(|entry| entry.order_key.clone())
        .collect();
    for left in 0..keys.len() {
        for right in 0..keys.len() {
            let ordering = keys[left].cmp(&keys[right]);
            assert_eq!(
                ordering == std::cmp::Ordering::Equal,
                keys[left].asset_id == keys[right].asset_id,
                "keys compared equal without naming the same asset"
            );
        }
    }
    // Strictly increasing, so the emitted order has no ties at all.
    for pair in keys.windows(2) {
        assert!(pair[0] < pair[1]);
    }
}

/// Oracle 4 and 6: a null raw timestamp is not an error, and empty facts
/// solve rather than panicking.
#[test]
fn missing_timestamps_and_empty_facts_are_handled() {
    assert!(solve(&empty("project_empty")).assets.is_empty());

    let mut facts = empty("project_null");
    facts
        .assets
        .push(asset("a", "seq_a", "clip.mp4", None, None));
    facts.assets.push(asset(
        "b",
        "seq_a",
        "C0001.MP4",
        Some("2026-03-01T00:00:00Z"),
        Some(1),
    ));
    let solution = solve(&facts);
    assert_eq!(solution.assets[0].asset_id, "b");
    assert_eq!(solution.assets[1].asset_id, "a");
    assert_eq!(solution.assets[1].trip_time, None);
    assert_eq!(solution.assets[1].confidence, 0.0);
}

/// Oracle 7: a 5,000-asset project solves well inside the review-queue budget.
#[test]
fn five_thousand_assets_solve_inside_the_budget() {
    let mut facts = empty("project_perf");
    for n in 0..5_000u64 {
        facts.assets.push(asset(
            &format!("asset_{n:05}"),
            if n % 4 == 0 { "seq_a" } else { "seq_b" },
            &format!("C{n:05}.MP4"),
            Some("2026-03-01T12:00:00Z"),
            Some(n),
        ));
    }
    for n in 0..20u64 {
        facts.epochs.push(ClockEpoch {
            id: format!("epoch_{n:02}"),
            capture_sequence_id: "seq_b".to_string(),
            start_counter: n * 250,
            end_counter: Some(n * 250 + 249),
            offset_seconds: n as i64,
            confidence: 1.0,
            evidence: Vec::new(),
            label: String::new(),
        });
    }
    let started = Instant::now();
    let solution = solve(&facts);
    let elapsed = started.elapsed();
    assert_eq!(solution.assets.len(), 5_000);
    assert!(
        elapsed.as_millis() < 250,
        "solver took {elapsed:?}, over the 250 ms review-queue budget"
    );
}

/// D01-T012 oracle 3 and 4: a transit phase in dual mode reads departure-local
/// first, and a date-line crossing shows different calendar dates.
#[test]
fn a_dual_mode_transit_phase_reads_departure_local_first() {
    let mut facts = empty("project_transit");
    facts.assets.push(asset(
        "a",
        "seq_a",
        "C0001.MP4",
        Some("2026-01-15T03:00:00Z"),
        Some(1),
    ));
    facts.phases.push(TripPhase {
        id: "phase_crossing".to_string(),
        label: "Tokyo to Toronto".to_string(),
        kind: PhaseKind::Transit,
        start_anchor_asset_id: None,
        end_anchor_asset_id: None,
        start_trip_time: "2026-01-15T00:00:00Z".to_string(),
        end_trip_time: "2026-01-16T00:00:00Z".to_string(),
        timezone: None,
        origin_timezone: Some("Asia/Tokyo".to_string()),
        destination_timezone: Some("America/Toronto".to_string()),
        display_mode: DisplayMode::Dual,
    });
    let solution = solve(&facts);
    let display = solution.assets[0].display_time.as_ref().unwrap();
    assert_eq!(display.primary, "2026-01-15T12:00:00+09:00");
    assert_eq!(
        display.secondary.as_deref(),
        Some("2026-01-14T22:00:00-05:00")
    );
    assert_eq!(display.timezone, "Asia/Tokyo");
}

/// D01-T012 oracle 5: a daylight-saving transition inside one phase resolves
/// through `chrono-tz`, not through a stored offset.
#[test]
fn a_daylight_saving_boundary_inside_a_phase_shifts_the_displayed_offset() {
    let mut facts = empty("project_dst");
    facts.assets.push(asset(
        "before",
        "seq_a",
        "C0001.MP4",
        Some("2026-03-08T06:30:00Z"),
        Some(1),
    ));
    facts.assets.push(asset(
        "after",
        "seq_a",
        "C0002.MP4",
        Some("2026-03-08T07:30:00Z"),
        Some(2),
    ));
    facts.phases.push(TripPhase {
        id: "phase_ny".to_string(),
        label: "New York".to_string(),
        kind: PhaseKind::Location,
        start_anchor_asset_id: None,
        end_anchor_asset_id: None,
        start_trip_time: "2026-03-01T00:00:00Z".to_string(),
        end_trip_time: "2026-03-31T00:00:00Z".to_string(),
        timezone: Some("America/New_York".to_string()),
        origin_timezone: None,
        destination_timezone: None,
        display_mode: DisplayMode::Local,
    });
    let solution = solve(&facts);
    let before = solution
        .assets
        .iter()
        .find(|entry| entry.asset_id == "before")
        .unwrap();
    let after = solution
        .assets
        .iter()
        .find(|entry| entry.asset_id == "after")
        .unwrap();
    let before_display = before.display_time.as_ref().unwrap();
    let after_display = after.display_time.as_ref().unwrap();
    assert!(
        before_display.primary.ends_with("-05:00"),
        "{before_display:?}"
    );
    assert!(
        after_display.primary.ends_with("-04:00"),
        "{after_display:?}"
    );
}

/// D01-T012 oracle 6, 7 and 8: uncovered clips display as UTC, overlapping
/// phases resolve to the later-starting one, and a null timezone falls back to
/// UTC instead of failing.
#[test]
fn coverage_is_total_over_gaps_overlaps_and_null_timezones() {
    let mut facts = empty("project_coverage");
    facts.assets.push(asset(
        "uncovered",
        "seq_a",
        "C0001.MP4",
        Some("2026-06-01T00:00:00Z"),
        Some(1),
    ));
    facts.assets.push(asset(
        "overlapped",
        "seq_a",
        "C0002.MP4",
        Some("2026-01-10T00:00:00Z"),
        Some(2),
    ));
    facts.phases.push(TripPhase {
        id: "phase_early".to_string(),
        label: "early".to_string(),
        kind: PhaseKind::Location,
        start_anchor_asset_id: None,
        end_anchor_asset_id: None,
        start_trip_time: "2026-01-01T00:00:00Z".to_string(),
        end_trip_time: "2026-01-31T00:00:00Z".to_string(),
        timezone: Some("Asia/Tokyo".to_string()),
        origin_timezone: None,
        destination_timezone: None,
        display_mode: DisplayMode::Local,
    });
    facts.phases.push(TripPhase {
        id: "phase_late".to_string(),
        label: "late, no timezone".to_string(),
        kind: PhaseKind::Location,
        start_anchor_asset_id: None,
        end_anchor_asset_id: None,
        start_trip_time: "2026-01-05T00:00:00Z".to_string(),
        end_trip_time: "2026-01-31T00:00:00Z".to_string(),
        timezone: None,
        origin_timezone: None,
        destination_timezone: None,
        display_mode: DisplayMode::Local,
    });

    let solution = solve(&facts);
    let uncovered = solution
        .assets
        .iter()
        .find(|entry| entry.asset_id == "uncovered")
        .unwrap()
        .display_time
        .as_ref()
        .unwrap();
    assert_eq!(uncovered.phase_id, None);
    assert_eq!(uncovered.timezone, "UTC");

    // The later-starting phase wins the overlap, and its null timezone falls
    // back to UTC rather than borrowing the earlier phase's zone.
    let overlapped = solution
        .assets
        .iter()
        .find(|entry| entry.asset_id == "overlapped")
        .unwrap()
        .display_time
        .as_ref()
        .unwrap();
    assert_eq!(overlapped.phase_id.as_deref(), Some("phase_late"));
    assert_eq!(overlapped.timezone, "UTC");
}

/// D01-T013: a clip exception fighting a hard constraint is reported as an
/// exception conflict, naming both clips.
#[test]
fn an_exception_against_a_hard_constraint_is_reported() {
    let mut facts = empty("project_conflict");
    facts.assets.push(asset(
        "a",
        "seq_a",
        "C0001.MP4",
        Some("2026-01-01T09:00:00Z"),
        Some(1),
    ));
    facts.assets.push(asset(
        "b",
        "seq_a",
        "C0002.MP4",
        Some("2026-01-01T10:00:00Z"),
        Some(2),
    ));
    facts.exceptions.push(ClipTimeException {
        asset_id: "a".to_string(),
        trip_time: "2026-01-01T23:00:00Z".to_string(),
        reason: "misplaced by hand".to_string(),
    });
    facts.constraints.push(EventOrderConstraint {
        id: "constraint_1".to_string(),
        earlier_asset_id: "a".to_string(),
        later_asset_id: "b".to_string(),
        strength: ConstraintStrength::Hard,
        source: "user".to_string(),
    });

    let solution = solve(&facts);
    assert_eq!(solution.contradictions.len(), 1);
    assert_eq!(
        solution.contradictions[0].kind,
        ContradictionKind::ExceptionConflict
    );
    assert_eq!(solution.contradictions[0].asset_ids, vec!["a", "b"]);
    // The hard constraint still wins the emitted order.
    let order: Vec<&str> = solution
        .assets
        .iter()
        .map(|entry| entry.asset_id.as_str())
        .collect();
    assert_eq!(order, vec!["a", "b"]);
}

/// A soft constraint is a hint: it never reorders and never contradicts.
#[test]
fn a_soft_constraint_never_blocks_and_never_reorders() {
    let mut facts = empty("project_soft");
    facts.assets.push(asset(
        "a",
        "seq_a",
        "C0001.MP4",
        Some("2026-01-01T09:00:00Z"),
        Some(1),
    ));
    facts.assets.push(asset(
        "b",
        "seq_a",
        "C0002.MP4",
        Some("2026-01-01T10:00:00Z"),
        Some(2),
    ));
    facts.constraints.push(EventOrderConstraint {
        id: "constraint_soft".to_string(),
        earlier_asset_id: "b".to_string(),
        later_asset_id: "a".to_string(),
        strength: ConstraintStrength::Soft,
        source: "guess".to_string(),
    });
    let solution = solve(&facts);
    assert!(solution.contradictions.is_empty());
    let order: Vec<&str> = solution
        .assets
        .iter()
        .map(|entry| entry.asset_id.as_str())
        .collect();
    assert_eq!(order, vec!["a", "b"]);
}
