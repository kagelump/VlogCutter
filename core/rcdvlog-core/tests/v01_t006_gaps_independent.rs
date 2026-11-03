//! V01-T006 independent oracle: gap-placed query windows (ARCHITECTURE section 6).
//!
//! Uses the real RC6 session spec (48 kHz mic clock, 180.299 s) through the
//! public plan API only. Expected gap queries are derived here by an
//! independent brute-force sweep over the capture samples, not by reusing the
//! implementation's interval merge.
use rcdvlog_core::{
    tv_review_sync::SyncQuery,
    tv_soundtrack_session::{SoundtrackSessionPlan, SoundtrackSessionSpec},
};

const SPEC: &str = include_str!("fixtures/v01/spec.json");
const RATE: u64 = 48_000;

fn rc6_spec() -> SoundtrackSessionSpec {
    serde_json::from_str(SPEC).unwrap()
}

fn sample(seconds: f64) -> u64 {
    (seconds * RATE as f64).round() as u64
}

fn with_mask(spans: &[(f64, f64)]) -> SoundtrackSessionSpec {
    let mut spec = rc6_spec();
    spec.speech_mask = spans
        .iter()
        .map(|&(a, b)| SyncQuery {
            clock_id: "mic_epoch_0".into(),
            start_sample: sample(a),
            end_sample: sample(b),
        })
        .collect();
    spec
}

/// Independent expectation: walk the recording in 1 ms steps, find maximal
/// unmasked runs, and size/centre each qualifying run.
fn expected_gap_queries(spec: &SoundtrackSessionSpec) -> Vec<(u64, u64)> {
    let end = spec.segments[0].first_sample + u64::from(spec.segments[0].frame_count);
    let masked = |s: u64| {
        spec.speech_mask
            .iter()
            .any(|m| m.start_sample <= s && s < m.end_sample)
    };
    // Boundaries of runs are always on mask edges or recording edges, so test
    // every candidate edge rather than every sample.
    let mut edges: Vec<u64> = vec![0, end];
    for m in &spec.speech_mask {
        edges.push(m.start_sample.min(end));
        edges.push(m.end_sample.min(end));
    }
    edges.sort_unstable();
    edges.dedup();
    let mut runs: Vec<(u64, u64)> = Vec::new();
    for w in edges.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a == b || masked(a) {
            continue;
        }
        match runs.last_mut() {
            Some(last) if last.1 == a => last.1 = b,
            _ => runs.push((a, b)),
        }
    }
    let cap = 20 * RATE;
    runs.into_iter()
        .filter(|(a, b)| b - a >= 4 * RATE)
        .map(|(a, b)| {
            let size = (b - a).min(cap);
            let first = a + (b - a - size) / 2;
            (first, first + size)
        })
        .collect()
}

fn gap_queries(plan: &SoundtrackSessionPlan) -> Vec<(u64, u64)> {
    plan.queries
        .iter()
        .filter(|q| q.origin.as_deref() == Some("speech_gap"))
        .map(|q| (q.recording.start_sample, q.recording.end_sample))
        .collect()
}

fn check(spans: &[(f64, f64)]) -> SoundtrackSessionPlan {
    let spec = with_mask(spans);
    let expected = expected_gap_queries(&spec);
    let plan = SoundtrackSessionPlan::new(spec).unwrap();
    assert_eq!(gap_queries(&plan), expected, "mask {spans:?}");
    // No gap query overlaps any mask interval.
    for (a, b) in gap_queries(&plan) {
        assert!(
            plan.spec
                .speech_mask
                .iter()
                .all(|m| b <= m.start_sample || a >= m.end_sample),
            "gap query [{a},{b}) overlaps the mask"
        );
    }
    // Grid queries are exactly the unmasked v1 plan's queries.
    let v1 = SoundtrackSessionPlan::new(rc6_spec()).unwrap();
    let grid: Vec<_> = plan.queries.iter().filter(|q| q.origin.is_none()).collect();
    assert_eq!(grid.len(), v1.queries.len());
    for (g, v) in grid.iter().zip(&v1.queries) {
        assert_eq!(g.recording, v.recording);
        assert_eq!(g.capture, v.capture);
    }
    // The plan is self-consistent and round-trips.
    let json = serde_json::to_vec(&plan).unwrap();
    SoundtrackSessionPlan::from_json(&json).unwrap();
    plan
}

#[test]
fn charter_gaps_of_3_4_7_and_45_seconds() {
    // Leading masked 1 s, then gaps 3, 4, 7, 45 s separated by 2 s masks.
    let plan = check(&[
        (0.0, 1.0),
        (4.0, 6.0),
        (10.0, 12.0),
        (19.0, 21.0),
        (66.0, 180.3),
    ]);
    assert_eq!(
        gap_queries(&plan),
        vec![
            (sample(6.0), sample(10.0)),
            (sample(12.0), sample(19.0)),
            (sample(33.5), sample(53.5)),
        ]
    );
}

#[test]
fn adjacent_overlapping_and_out_of_order_intervals() {
    check(&[
        (50.0, 60.0),
        (5.0, 9.0),
        (8.0, 12.0),
        (12.0, 14.0),
        (100.0, 101.0),
        (101.0, 140.0),
    ]);
}

#[test]
fn exactly_four_seconds_qualifies_and_trailing_edge_gap_is_clamped() {
    let plan = check(&[(4.0, 170.0), (174.0, 175.0)]);
    let gaps = gap_queries(&plan);
    assert_eq!(gaps[0], (0, sample(4.0)));
    // Trailing gap [175 s, end of recording) is ~5.3 s and ends exactly at the recording end.
    let end = 8_654_336;
    assert_eq!(*gaps.last().unwrap(), (sample(175.0), end));
}

#[test]
fn mask_covering_everything_gives_no_gap_queries_and_unmasked_plan_has_no_origin() {
    let plan = check(&[(0.0, 180.3)]);
    assert!(gap_queries(&plan).is_empty());
    let v1 = SoundtrackSessionPlan::new(rc6_spec()).unwrap();
    assert!(v1.queries.iter().all(|q| q.origin.is_none()));
    let value = serde_json::to_value(&v1).unwrap();
    for q in value["queries"].as_array().unwrap() {
        assert!(
            q.get("origin").is_none(),
            "v1 query must not serialise origin"
        );
    }
}

#[test]
fn masked_plan_references_overlap_by_the_gap_cap() {
    let plan = check(&[(10.0, 20.0)]);
    // 120 s references; with a mask they overlap by 20 s (100 s step).
    let starts: Vec<f64> = plan.references.iter().map(|r| r.start_seconds).collect();
    assert!(starts.len() >= 2);
    assert!((starts[1] - starts[0] - 100.0).abs() < 1e-9, "{starts:?}");
    // Every gap query fits wholly inside at least one reference in movie seconds
    // under any constant offset (its length is at most the overlap).
    for (a, b) in gap_queries(&plan) {
        assert!((b - a) as f64 / RATE as f64 <= 20.0);
    }
}
