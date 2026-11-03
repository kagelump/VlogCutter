//! V01-T007 independent oracle for ARCHITECTURE section 8 offset consensus.
//!
//! Evidence is synthesised here, not with the implementer's helpers: every
//! query row is written out explicitly as (offset, similarity, state) and turned
//! into `SessionQueryEvidence` by this file's own builder. Plans come from the
//! public `SoundtrackSessionPlan::new`. Only public API is used.

use rcdvlog_core::tv_review_sync::SyncBoundaryKind;
use rcdvlog_core::{
    tv_review_sync::{CaptureClock, SyncMovieClock, SyncPolicy, SyncRequest},
    tv_soundtrack::{SoundtrackPeak, SoundtrackPolicy},
    tv_soundtrack_consensus::{SoundtrackConsensus, consensus, consensus_from_evidence},
    tv_soundtrack_session::{
        SessionAudioSegment, SessionQueryEvidence, SoundtrackSessionPlan, SoundtrackSessionSpec,
    },
};
use std::collections::BTreeMap;

const RATE: u64 = 8000;
/// Matched state as the session layer names it for exactly one clustered anchor.
const STATE_MATCHED: &str = "candidate";
const STATE_UNMATCHED: &str = "unmatched";

/// One query, written as data: peaks are (offset from centre in s, similarity).
struct Row {
    peaks: Vec<(f64, f64)>,
    matched: bool,
}

fn weak(offset: f64) -> Row {
    Row {
        peaks: vec![(offset, 0.3)],
        matched: false,
    }
}

fn matched(offset: f64) -> Row {
    Row {
        peaks: vec![(offset, 0.6)],
        matched: true,
    }
}

fn silent() -> Row {
    Row {
        peaks: vec![],
        matched: false,
    }
}

/// Plan with one mic clock over a single contiguous segment of `seconds`.
/// Six-second queries stepped by six seconds give `seconds / 6` queries.
fn plan(seconds: u64, max_drift_ppm: f64) -> SoundtrackSessionPlan {
    SoundtrackSessionPlan::new(SoundtrackSessionSpec {
        schema_version: 1,
        sync: SyncRequest {
            schema_version: 1,
            session_id: "t007_oracle".into(),
            target_digest: "1".repeat(64),
            movie: SyncMovieClock {
                movie_digest: "2".repeat(64),
                time_base_numerator: 1,
                time_base_denominator: 90_000,
                origin_pts: 0,
                duration_pts: 600 * 90_000,
            },
            capture_clocks: vec![CaptureClock {
                clock_id: "mic".into(),
                sample_rate: RATE as u32,
                first_sample: 0,
                last_sample: 50_000_000,
                monotonic_origin_ns: 5_000_000_000,
                monotonic_tolerance_ns: 1,
            }],
            anchors: vec![],
            boundaries: vec![],
            policy: SyncPolicy {
                max_drift_ppm,
                ..SyncPolicy::default()
            },
            consensus_segments: vec![],
        },
        segments: vec![SessionAudioSegment {
            segment_id: "seg0".into(),
            clock_id: "mic".into(),
            first_sample: 0,
            frame_count: (seconds * RATE) as u32,
            wav_blake3: "3".repeat(64),
        }],
        reference_seconds: 12,
        query_seconds: 6,
        query_step_seconds: 6,
        match_policy: SoundtrackPolicy::default(),
        speech_mask: vec![],
        recipe: None,
    })
    .expect("plan")
}

/// Query centre in seconds on the mic clock, computed from the plan's recording
/// interval directly (not via the implementation's centre helper).
fn centre_s(plan: &SoundtrackSessionPlan, i: usize) -> f64 {
    let r = &plan.queries[i].recording;
    (r.start_sample + (r.end_sample - r.start_sample) / 2) as f64 / RATE as f64
}

/// Builds complete evidence for `rows`. `rows[i]` describes `plan.queries[i]`.
/// Peak `movie_seconds` is centre + offset, as the matcher reports it.
fn evidence(plan: &SoundtrackSessionPlan, rows: &[Row]) -> Vec<SessionQueryEvidence> {
    assert_eq!(rows.len(), plan.queries.len(), "row count must match plan");
    rows.iter()
        .enumerate()
        .map(|(i, row)| SessionQueryEvidence {
            query_index: i,
            recording: plan.queries[i].recording.clone(),
            completed_references: plan.references.len(),
            total_references: plan.references.len(),
            state: if row.matched {
                STATE_MATCHED.into()
            } else {
                STATE_UNMATCHED.into()
            },
            report_digests: vec![],
            supporting_evidence_ids: BTreeMap::new(),
            anchors: vec![],
            best_peaks: row
                .peaks
                .iter()
                .map(|&(offset, similarity)| SoundtrackPeak {
                    similarity,
                    reference_offset_frames: 0,
                    movie_seconds: centre_s(plan, i) + offset,
                    subwindow_similarities: [similarity; 3],
                })
                .collect(),
        })
        .collect()
}

fn derive(plan: &SoundtrackSessionPlan, rows: &[Row]) -> SoundtrackConsensus {
    consensus_from_evidence(plan, &evidence(plan, rows), &|| false).expect("consensus")
}

fn json_bytes(c: &SoundtrackConsensus) -> Vec<u8> {
    serde_json::to_vec(c).expect("serialise consensus")
}

const O: f64 = -0.96;

#[test]
fn constant_offset_is_one_segment_with_true_offset_and_no_drift() {
    // Five queries: 3 weak, 2 matched, all at O.
    let p = plan(30, 2000.0);
    assert_eq!(p.queries.len(), 5);
    let out = derive(&p, &[weak(O), matched(O), weak(O), weak(O), matched(O)]);

    assert_eq!(out.segments.len(), 1, "{out:?}");
    assert!(out.boundaries.is_empty(), "{out:?}");
    assert!(out.unsupported_queries.is_empty(), "{out:?}");
    let s = &out.segments[0];
    assert_eq!(s.supporting_queries, vec![0, 1, 2, 3, 4]);
    assert!(
        (s.offset_seconds - O).abs() <= 0.01,
        "offset {} vs expected {O}",
        s.offset_seconds
    );
    assert!(
        s.residual_rms_seconds <= 0.01,
        "residual_rms {} not small",
        s.residual_rms_seconds
    );
    assert!(
        s.drift_ppm.abs() <= 1.0,
        "drift_ppm {} not near 0",
        s.drift_ppm
    );
    assert_eq!(s.clock_id, "mic");
}

#[test]
fn measured_drift_is_reported_for_a_known_slope() {
    // Positive control: a 1000 ppm slope is accepted and measured, so the
    // drift field is genuinely computed and not a constant zero.
    let p = plan(30, 2000.0);
    let slope = 1000e-6;
    let rows: Vec<Row> = (0..5)
        .map(|i| {
            let off = O + slope * centre_s(&p, i);
            if i == 1 || i == 4 {
                matched(off)
            } else {
                weak(off)
            }
        })
        .collect();
    let out = derive(&p, &rows);
    assert_eq!(out.segments.len(), 1, "{out:?}");
    let drift = out.segments[0].drift_ppm;
    assert!(
        (drift - 1000.0).abs() <= 5.0,
        "drift_ppm {drift} vs expected 1000"
    );
}

#[test]
fn drift_of_3000_ppm_is_rejected() {
    // Same structure as the accepted case but the offset walks at 3000 ppm.
    // With max_drift_ppm = 2000 the run must not become a segment.
    let p = plan(30, 2000.0);
    let slope = 3000e-6;
    let rows: Vec<Row> = (0..5)
        .map(|i| {
            let off = O + slope * centre_s(&p, i);
            if i == 1 || i == 4 {
                matched(off)
            } else {
                weak(off)
            }
        })
        .collect();
    let out = derive(&p, &rows);
    assert!(
        out.segments.is_empty(),
        "3000 ppm drift must be rejected, got segments {:?}",
        out.segments
    );
    assert_eq!(out.unsupported_queries, vec![0, 1, 2, 3, 4]);
    assert!(out.boundaries.is_empty());
}

#[test]
fn incomplete_search_is_rejected_with_search_incomplete() {
    let p = plan(30, 2000.0);
    let rows = [weak(O), matched(O), weak(O), weak(O), matched(O)];

    // Some references not searched for one query.
    let mut ev = evidence(&p, &rows);
    ev[2].completed_references = ev[2].total_references - 1;
    let err = consensus_from_evidence(&p, &ev, &|| false).expect_err("must reject");
    assert!(err.to_string().contains("search_incomplete"), "{err}");

    // Evidence shorter than the plan.
    let full = evidence(&p, &rows);
    let err = consensus_from_evidence(&p, &full[..4], &|| false).expect_err("must reject");
    assert!(err.to_string().contains("search_incomplete"), "{err}");

    // Through the public entry point with an empty cache.
    let dir = tempfile::tempdir().unwrap();
    let err = consensus(&p, dir.path(), &|| false).expect_err("must reject");
    assert!(err.to_string().contains("search_incomplete"), "{err}");
}

#[test]
fn output_is_byte_identical_across_two_runs() {
    let p = plan(60, 2000.0);
    let rows: Vec<Row> = (0..10)
        .map(|i| {
            let off = if i < 5 { O } else { O + 30.0 };
            if i % 2 == 1 { matched(off) } else { weak(off) }
        })
        .collect();
    let ev = evidence(&p, &rows);
    let a = consensus_from_evidence(&p, &ev, &|| false).unwrap();
    let b = consensus_from_evidence(&p, &ev, &|| false).unwrap();
    assert_eq!(json_bytes(&a), json_bytes(&b));
    // Plan identity is also deterministic across constructions.
    let p2 = plan(60, 2000.0);
    assert_eq!(p.plan_digest, p2.plan_digest);
    let c = consensus_from_evidence(&p2, &ev, &|| false).unwrap();
    assert_eq!(json_bytes(&a), json_bytes(&c));
}

#[test]
fn shifted_second_half_gives_two_segments_and_a_boundary() {
    // Sixty seconds, ten queries. First five at O, last five at O + 30 s.
    let p = plan(60, 2000.0);
    let rows = [
        weak(O),
        matched(O),
        weak(O),
        weak(O),
        matched(O),
        weak(O + 30.0),
        matched(O + 30.0),
        weak(O + 30.0),
        weak(O + 30.0),
        matched(O + 30.0),
    ];
    let out = derive(&p, &rows);
    assert_eq!(out.segments.len(), 2, "{out:?}");
    assert_eq!(out.boundaries.len(), 1, "{out:?}");
    assert!((out.segments[0].offset_seconds - O).abs() <= 0.01);
    assert!((out.segments[1].offset_seconds - (O + 30.0)).abs() <= 0.01);
    assert_eq!(out.segments[0].supporting_queries, vec![0, 1, 2, 3, 4]);
    assert_eq!(out.segments[1].supporting_queries, vec![5, 6, 7, 8, 9]);
    let b = &out.boundaries[0];
    assert_eq!(b.clock_id, "mic");
    // Midpoint between the centres of query 4 and query 5 (27 s and 33 s).
    assert_eq!(b.sample_index, 30 * RATE);
    // The audio is one contiguous segment, so this is a resync, not a gap.
    assert_eq!(b.kind, SyncBoundaryKind::Resync);
}

#[test]
fn single_matched_rival_at_another_offset_splits_the_run() {
    // Sixty seconds, ten queries. Queries 0-2 at O, query 3 a single Matched
    // rival at O + 30, queries 4-8 at O (3 matched-or-weak with 2 matched).
    // The rival must split the O run, not be glued into it.
    let p = plan(60, 2000.0);
    let rows = [
        weak(O),
        matched(O),
        weak(O),
        matched(O + 30.0),
        weak(O),
        matched(O),
        weak(O),
        weak(O),
        matched(O),
        silent(),
    ];
    let out = derive(&p, &rows);
    assert_eq!(out.segments.len(), 1, "{out:?}");
    let s = &out.segments[0];
    assert_eq!(s.supporting_queries, vec![4, 5, 6, 7, 8], "{out:?}");
    assert!(
        (s.offset_seconds - O).abs() <= 0.01,
        "segment offset {} glued across rival",
        s.offset_seconds
    );
    assert_eq!(out.unsupported_queries, vec![0, 1, 2, 3, 9], "{out:?}");
}

#[test]
fn single_supporting_query_yields_no_segment() {
    let p = plan(30, 2000.0);
    // Only query 0 carries evidence, and it is the lone Matched query.
    let rows = [matched(O), silent(), silent(), silent(), silent()];
    let out = derive(&p, &rows);
    assert!(out.segments.is_empty(), "{out:?}");
    assert!(out.boundaries.is_empty(), "{out:?}");
    assert_eq!(out.unsupported_queries, vec![0, 1, 2, 3, 4]);
}

#[test]
fn two_matched_without_third_query_yields_no_segment() {
    // Two supporting queries is below the three-query minimum.
    let p = plan(30, 2000.0);
    let rows = [matched(O), matched(O), silent(), silent(), silent()];
    let out = derive(&p, &rows);
    assert!(out.segments.is_empty(), "{out:?}");
}

#[test]
fn three_weak_queries_without_matched_yield_no_segment() {
    let p = plan(30, 2000.0);
    let rows = [weak(O), weak(O), weak(O), silent(), silent()];
    let out = derive(&p, &rows);
    assert!(out.segments.is_empty(), "{out:?}");
}

#[test]
fn random_offsets_yield_no_segments() {
    // Deterministic pseudo-random offsets spread over the movie: no two
    // neighbours agree within 0.08 s, so nothing should cluster into a run.
    let p = plan(60, 2000.0);
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let rows: Vec<Row> = (0..10)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let off = (state >> 11) as f64 / (1u64 << 53) as f64 * 40.0 - 20.0;
            matched(off)
        })
        .collect();
    let out = derive(&p, &rows);
    assert!(
        out.segments.is_empty(),
        "random offsets produced segments: {:?}",
        out.segments
    );
    assert_eq!(out.unsupported_queries, (0..10).collect::<Vec<_>>());
}

#[test]
fn sub_threshold_peaks_never_vote() {
    // Similarity below 0.25 is not evidence, even when it sits at O.
    let p = plan(30, 2000.0);
    let faint = |off: f64| Row {
        peaks: vec![(off, 0.2)],
        matched: true,
    };
    let rows = [faint(O), faint(O), faint(O), faint(O), faint(O)];
    let out = derive(&p, &rows);
    assert!(out.segments.is_empty(), "{out:?}");
}
