//! V01-T008 independent oracle for ARCHITECTURE section 9 (consensus-gated
//! interpolation in the sync mapper).
//!
//! The request is written by hand as JSON and parsed through the public
//! `SyncRequest::from_json` (which also runs `validate`). Mapping goes through
//! the public `SyncRequest::map`. Expected uncertainties are computed here from
//! the section 9 formula, not from the implementer's helpers.

use rcdvlog_core::tv_review_sync::{SyncRequest, SyncSlice, SyncState};
use serde_json::{Value, json};

const SAMPLE_RATE: u64 = 48_000;
const TICKS_PER_SECOND: f64 = 90_000.0;
const ORIGIN_PTS: i64 = 90_000;

/// Movie clock: 90 kHz, origin 1 s, 200 s long.
fn movie() -> Value {
    json!({
        "movieDigest": "b".repeat(64),
        "timeBaseNumerator": 1,
        "timeBaseDenominator": 90000,
        "originPts": ORIGIN_PTS,
        "durationPts": 18_000_000u64
    })
}

fn policy(residual_tolerance: f64, unobserved_ppm: f64) -> Value {
    json!({
        "minimumConfidence": 0.8,
        "maxAnchorGapSeconds": 15.0,
        "maxExtrapolationSeconds": 0.0,
        "maxDriftPpm": 2000.0,
        "residualToleranceSeconds": residual_tolerance,
        "unobservedUncertaintyPpm": unobserved_ppm
    })
}

/// One mic clock, 0..120 s at 48 kHz, monotonic origin 3 s.
fn clock() -> Value {
    json!([{
        "clockId": "mic",
        "sampleRate": SAMPLE_RATE,
        "firstSample": 0,
        "lastSample": SAMPLE_RATE * 120,
        "monotonicOriginNs": 3_000_000_000u64,
        "monotonicToleranceNs": 2_000_000u64
    }])
}

/// Anchor at `mic_seconds` (on the mic clock) pointing to `movie_seconds`.
/// Uncertainty fields are explicit so the expected formula can be computed.
fn anchor(
    id: &str,
    mic_seconds: f64,
    movie_seconds: f64,
    movie_unc: u64,
    capture_unc: u32,
) -> Value {
    let sample = (mic_seconds * SAMPLE_RATE as f64) as u64;
    json!({
        "evidenceId": id,
        "capture": {
            "clockId": "mic",
            "sampleIndex": sample,
            "monotonicNs": 3_000_000_000u64 + (mic_seconds * 1e9) as u64
        },
        "moviePts": ORIGIN_PTS + (movie_seconds * TICKS_PER_SECOND) as i64,
        "source": "manualCue",
        "confidence": 0.99,
        "movieUncertaintyTicks": movie_unc,
        "captureUncertaintySamples": capture_unc,
        "candidateGroupId": null
    })
}

/// Segment on the mic clock covering `start..end` seconds with the given offset.
fn segment(id: &str, start: f64, end: f64, offset: f64, residual: f64) -> Value {
    json!({
        "segmentId": id,
        "clockId": "mic",
        "offsetSeconds": offset,
        "micStartSample": (start * SAMPLE_RATE as f64) as u64,
        "micEndSample": (end * SAMPLE_RATE as f64) as u64,
        "supportingQueries": [0, 1, 2],
        "residualRmsSeconds": residual,
        "driftPpm": 0.0
    })
}

fn request(anchors: Vec<Value>, boundaries: Vec<Value>, segments: Vec<Value>, p: Value) -> Value {
    json!({
        "schemaVersion": 1,
        "sessionId": "t008_oracle",
        "targetDigest": "a".repeat(64),
        "movie": movie(),
        "captureClocks": clock(),
        "anchors": anchors,
        "boundaries": boundaries,
        "policy": p,
        "consensusSegments": segments
    })
}

fn parse(v: &Value) -> SyncRequest {
    SyncRequest::from_json(serde_json::to_string(v).unwrap().as_bytes())
        .expect("hand-written request must validate")
}

fn query(start: f64, end: f64) -> rcdvlog_core::tv_review_sync::SyncQuery {
    rcdvlog_core::tv_review_sync::SyncQuery {
        clock_id: "mic".into(),
        start_sample: (start * SAMPLE_RATE as f64) as u64,
        end_sample: (end * SAMPLE_RATE as f64) as u64,
    }
}

fn map_one(r: &SyncRequest, start: f64, end: f64) -> SyncSlice {
    let report = r.map(&[query(start, end)], &|| false).unwrap();
    assert_eq!(
        report.slices.len(),
        1,
        "query with no boundaries maps to one slice"
    );
    report.slices.into_iter().next().unwrap()
}

/// Anchors 60 s apart in mic time (2 s and 62 s) and movie time (10 s and 70 s):
/// movie offset is +8 s on both. Both have identical uncertainty so the
/// "anchor uncertainty" term is unambiguous.
fn far_anchors() -> Vec<Value> {
    vec![
        anchor("near_left", 2.0, 10.0, 900, 240),
        anchor("near_right", 62.0, 70.0, 900, 240),
    ]
}

#[test]
fn sixty_second_gap_inside_one_segment_is_consensus_interpolated_with_section_nine_uncertainty() {
    // Default-style values: residual 0.02 s, unobserved 5000 ppm.
    let r = parse(&request(
        far_anchors(),
        vec![],
        vec![segment("seg_one", 0.0, 100.0, 8.0, 0.02)],
        policy(0.1, 5000.0),
    ));
    let s = map_one(&r, 20.0, 21.0);
    assert_eq!(s.state, SyncState::ConsensusInterpolated);
    assert_eq!(s.diagnostics, ["consensus_segment:seg_one"]);
    assert_eq!(s.candidates.len(), 1);
    let c = &s.candidates[0];
    // Movie = mic + 8 s on the interpolated line.
    assert!((c.start_pts - (ORIGIN_PTS as f64 + 28.0 * TICKS_PER_SECOND)).abs() < 0.01);
    assert!((c.end_pts - (ORIGIN_PTS as f64 + 29.0 * TICKS_PER_SECOND)).abs() < 0.01);
    assert!(c.lower_pts as f64 <= c.start_pts && c.upper_pts as f64 >= c.end_pts);

    // Section 9: anchor uncertainty + residual_rms + unobserved_ppm * gap * 1e-6,
    // all in movie ticks. Anchor = 900 + 240/48000 s * 90000 = 1350 ticks.
    let anchor_ticks = 900.0 + 240.0 / SAMPLE_RATE as f64 * TICKS_PER_SECOND;
    let residual_ticks = 0.02 * TICKS_PER_SECOND;
    let gap_seconds = 60.0;
    let unobserved_ticks = 5000.0 * gap_seconds * 1e-6 * TICKS_PER_SECOND;
    let expected = anchor_ticks + residual_ticks + unobserved_ticks; // 30150
    assert!(
        (c.uncertainty_ticks as f64) >= expected && (c.uncertainty_ticks as f64) <= expected + 1.0,
        "uncertainty_ticks {} vs section 9 expected {expected}",
        c.uncertainty_ticks
    );
    assert_eq!(c.uncertainty_ticks, 30150);
}

#[test]
fn section_nine_uncertainty_formula_holds_for_a_second_parameter_set() {
    // Different residual and unobserved rates, and a non-zero capture term.
    let anchors = vec![
        anchor("l", 5.0, 13.0, 1000, 480),
        anchor("r", 95.0, 103.0, 1000, 480),
    ];
    let r = parse(&request(
        anchors,
        vec![],
        vec![segment("seg_two", 0.0, 120.0, 8.0, 0.05)],
        policy(0.1, 2000.0),
    ));
    let s = map_one(&r, 40.0, 41.0);
    assert_eq!(s.state, SyncState::ConsensusInterpolated);
    let c = &s.candidates[0];
    let anchor_ticks = 1000.0 + 480.0 / SAMPLE_RATE as f64 * TICKS_PER_SECOND; // 1010
    let residual_ticks = 0.05 * TICKS_PER_SECOND; // 4500
    let unobserved_ticks = 2000.0 * 90.0 * 1e-6 * TICKS_PER_SECOND; // 16200
    let expected = anchor_ticks + residual_ticks + unobserved_ticks; // 21710
    assert!(
        (c.uncertainty_ticks as f64) >= expected && (c.uncertainty_ticks as f64) <= expected + 1.0,
        "uncertainty_ticks {} vs section 9 expected {expected}",
        c.uncertainty_ticks
    );
}

#[test]
fn same_far_anchors_without_a_segment_stay_unknown() {
    let r = parse(&request(far_anchors(), vec![], vec![], policy(0.1, 5000.0)));
    let s = map_one(&r, 20.0, 21.0);
    assert_eq!(s.state, SyncState::Unknown);
    assert!(s.candidates.is_empty());
    assert!(s.diagnostics.contains(&"gap_or_discontinuity".to_string()));
    assert!(
        !s.diagnostics
            .iter()
            .any(|d| d.starts_with("consensus_segment"))
    );
}

#[test]
fn boundary_inside_the_gap_refuses_consensus_interpolation() {
    let boundary = json!({
        "evidenceId": "pause_in_gap",
        "clockId": "mic",
        "sampleIndex": 30 * SAMPLE_RATE,
        "kind": "pause"
    });
    let r = parse(&request(
        far_anchors(),
        vec![boundary],
        vec![segment("seg_one", 0.0, 100.0, 8.0, 0.02)],
        policy(0.1, 5000.0),
    ));
    let s = map_one(&r, 20.0, 21.0);
    assert_ne!(s.state, SyncState::ConsensusInterpolated);
    assert_eq!(s.state, SyncState::Unknown);
    assert!(s.candidates.is_empty());
}

#[test]
fn query_past_the_segment_supporting_span_is_refused() {
    // Segment supporting span ends at 50 s; the right anchor (70 s) and the
    // utterance (45..55 s) run past its last supporting window.
    let anchors = vec![
        anchor("inside", 40.0, 48.0, 900, 240),
        anchor("outside", 70.0, 78.0, 900, 240),
    ];
    let r = parse(&request(
        anchors,
        vec![],
        vec![segment("seg_short", 0.0, 50.0, 8.0, 0.02)],
        policy(0.1, 5000.0),
    ));
    let s = map_one(&r, 45.0, 55.0);
    assert_ne!(s.state, SyncState::ConsensusInterpolated);
    assert_eq!(s.state, SyncState::Unknown);
    assert!(s.candidates.is_empty());
}

#[test]
fn left_anchor_before_segment_start_is_refused() {
    // Segment starts at 5 s; the left anchor (2 s) lies before it.
    let r = parse(&request(
        far_anchors(),
        vec![],
        vec![segment("seg_late", 5.0, 100.0, 8.0, 0.02)],
        policy(0.1, 5000.0),
    ));
    let s = map_one(&r, 20.0, 21.0);
    assert_ne!(s.state, SyncState::ConsensusInterpolated);
    assert_eq!(s.state, SyncState::Unknown);
}

#[test]
fn empty_consensus_segments_give_v1_bytes_and_identical_reports() {
    // Request without the field at all (the v1 shape) versus the same request
    // with an explicit empty list: both must serialise to identical v1 bytes
    // and map to identical reports.
    let mut without = request(far_anchors(), vec![], vec![], policy(0.1, 5000.0));
    without.as_object_mut().unwrap().remove("consensusSegments");
    let with_empty = request(far_anchors(), vec![], vec![], policy(0.1, 5000.0));

    let r_without = parse(&without);
    let r_empty = parse(&with_empty);

    let bytes_without = serde_json::to_string(&r_without).unwrap();
    let bytes_empty = serde_json::to_string(&r_empty).unwrap();
    assert_eq!(bytes_without, bytes_empty);
    assert!(!bytes_without.contains("consensusSegments"));
    // Round trip of the v1 text is byte-stable.
    assert_eq!(
        bytes_without,
        serde_json::to_string(&parse(&without)).unwrap()
    );

    let queries = [query(20.0, 21.0), query(2.0, 2.0), query(5.0, 6.0)];
    let rep_without = r_without.map(&queries, &|| false).unwrap();
    let rep_empty = r_empty.map(&queries, &|| false).unwrap();
    assert_eq!(rep_without, rep_empty);
    assert_eq!(
        serde_json::to_string(&rep_without).unwrap(),
        serde_json::to_string(&rep_empty).unwrap()
    );
    assert_eq!(rep_without.slices[0].state, SyncState::Unknown);
    assert_eq!(rep_without.slices[1].state, SyncState::Anchored);
}

#[test]
fn wrong_progress_inside_a_segment_is_not_consensus_interpolated() {
    // Movie advances 90 s over 60 s of mic time.
    let mut anchors = far_anchors();
    anchors[1] = anchor("near_right", 62.0, 98.0, 900, 240);
    let r = parse(&request(
        anchors,
        vec![],
        vec![segment("seg_one", 0.0, 100.0, 8.0, 0.02)],
        policy(0.1, 5000.0),
    ));
    let s = map_one(&r, 20.0, 21.0);
    assert_ne!(s.state, SyncState::ConsensusInterpolated);
    assert_eq!(s.state, SyncState::Conflicting);
    assert!(
        s.diagnostics
            .contains(&"inconsistent_progress_requires_resync".to_string())
    );
    assert!(s.candidates.is_empty());
}

#[test]
fn segment_offset_contradicting_the_anchors_is_not_consensus_interpolated() {
    // Review charter (ARCHITECTURE/packet V01-T008): consensus must not silently
    // override anchors that disagree with the segment offset beyond residual
    // tolerance. The anchors imply +8 s (movie - mic); the segment says -0.96 s.
    // The anchors themselves are mutually consistent (60 s mic, 60 s movie).
    let r = parse(&request(
        far_anchors(),
        vec![],
        vec![segment("seg_wrong_offset", 0.0, 100.0, -0.96, 0.02)],
        policy(0.1, 5000.0),
    ));
    let s = map_one(&r, 20.0, 21.0);
    assert_ne!(
        s.state,
        SyncState::ConsensusInterpolated,
        "segment offset -0.96 s contradicts anchor offset +8 s; diagnostics {:?}",
        s.diagnostics
    );
}
