use super::*;
use crate::{
    tv_review_sync::{CaptureClock, SyncMovieClock, SyncPolicy, SyncRequest},
    tv_soundtrack::{SoundtrackPeak, SoundtrackPolicy},
    tv_soundtrack_session::{SessionAudioSegment, SoundtrackSessionSpec},
};

const RATE: f64 = 8000.0;
const WEAK: f64 = 0.3;
const STRONG: f64 = 0.6;

/// Pure plan: no media files are read by `SoundtrackSessionPlan::new`.
fn plan_with(segments: &[(u64, u32)], max_drift_ppm: f64) -> SoundtrackSessionPlan {
    SoundtrackSessionPlan::new(SoundtrackSessionSpec {
        schema_version: 1,
        sync: SyncRequest {
            schema_version: 1,
            session_id: "consensus_session".into(),
            target_digest: "a".repeat(64),
            movie: SyncMovieClock {
                movie_digest: "b".repeat(64),
                time_base_numerator: 1,
                time_base_denominator: 90000,
                origin_pts: 0,
                duration_pts: 600 * 90000,
            },
            capture_clocks: vec![CaptureClock {
                clock_id: "mic".into(),
                sample_rate: 8000,
                first_sample: 0,
                last_sample: 10_000_000,
                monotonic_origin_ns: 1_000_000_000,
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
        segments: segments
            .iter()
            .enumerate()
            .map(|(i, &(first_sample, frame_count))| SessionAudioSegment {
                segment_id: format!("seg{i}"),
                clock_id: "mic".into(),
                first_sample,
                frame_count,
                wav_blake3: "c".repeat(64),
            })
            .collect(),
        reference_seconds: 12,
        query_seconds: 6,
        query_step_seconds: 6,
        match_policy: SoundtrackPolicy::default(),
        speech_mask: vec![],
        recipe: None,
    })
    .unwrap()
}

/// One 30 s segment gives five 6 s queries; 60 s gives ten.
fn plan_seconds(seconds: u32) -> SoundtrackSessionPlan {
    plan_with(&[(0, seconds * 8000)], 2000.0)
}

fn centre_seconds(plan: &SoundtrackSessionPlan, i: usize) -> f64 {
    let r = &plan.queries[i].recording;
    ((r.start_sample + r.end_sample) / 2) as f64 / RATE
}

/// One query's peaks as (offset from the query centre, similarity), and its state.
#[derive(Clone)]
struct Row {
    peaks: Vec<(f64, f64)>,
    matched: bool,
}

fn weak(offset: f64) -> Row {
    Row {
        peaks: vec![(offset, WEAK)],
        matched: false,
    }
}

fn matched(offset: f64) -> Row {
    Row {
        peaks: vec![(offset, STRONG)],
        matched: true,
    }
}

fn evidence_for(plan: &SoundtrackSessionPlan, rows: &[Row]) -> Vec<SessionQueryEvidence> {
    rows.iter()
        .enumerate()
        .map(|(i, row)| SessionQueryEvidence {
            query_index: i,
            recording: plan.queries[i].recording.clone(),
            completed_references: plan.references.len(),
            total_references: plan.references.len(),
            state: if row.matched {
                MATCHED_STATE.into()
            } else {
                "unmatched".into()
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
                    movie_seconds: centre_seconds(plan, i) + offset,
                    subwindow_similarities: [similarity; 3],
                })
                .collect(),
        })
        .collect()
}

fn run(plan: &SoundtrackSessionPlan, rows: &[Row]) -> SoundtrackConsensus {
    consensus_from_evidence(plan, &evidence_for(plan, rows), &|| false).unwrap()
}

const O: f64 = -0.96;

#[test]
fn constant_offset_three_weak_two_matched_is_one_segment() {
    let plan = plan_seconds(30);
    let rows = [weak(O), matched(O), weak(O), weak(O), matched(O)];
    let out = run(&plan, &rows);
    assert_eq!(out.segments.len(), 1, "{out:?}");
    assert!(out.boundaries.is_empty());
    assert!(out.unsupported_queries.is_empty());
    let seg = &out.segments[0];
    assert_eq!(seg.supporting_queries, vec![0, 1, 2, 3, 4]);
    assert!((seg.offset_seconds - O).abs() < 1e-9);
    assert!(seg.drift_ppm.abs() < 1.0, "{}", seg.drift_ppm);
    assert!(seg.residual_rms_seconds < 1e-9);
    assert_eq!(seg.clock_id, "mic");
    assert_eq!(seg.mic_start_sample, 24_000);
    assert_eq!(seg.mic_end_sample, 216_000);
    assert_eq!(seg.segment_id.len(), "consensus_".len() + 16);
    // Identical inputs give byte-identical output.
    assert_eq!(
        serde_json::to_vec(&out).unwrap(),
        serde_json::to_vec(&run(&plan, &rows)).unwrap()
    );
}

#[test]
fn second_half_shifted_thirty_seconds_gives_two_segments_and_a_boundary() {
    let plan = plan_seconds(60);
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
    let out = run(&plan, &rows);
    assert_eq!(out.segments.len(), 2, "{out:?}");
    assert_eq!(out.segments[0].supporting_queries, vec![0, 1, 2, 3, 4]);
    assert_eq!(out.segments[1].supporting_queries, vec![5, 6, 7, 8, 9]);
    assert!((out.segments[0].offset_seconds - O).abs() < 1e-9);
    assert!((out.segments[1].offset_seconds - (O + 30.0)).abs() < 1e-9);
    assert_eq!(out.boundaries.len(), 1);
    let b = &out.boundaries[0];
    assert_eq!(b.clock_id, "mic");
    assert_eq!(b.kind, SyncBoundaryKind::Resync);
    // Midpoint of query 4's centre (216000) and query 5's centre (264000).
    assert_eq!(b.sample_index, 240_000);
    assert!(out.unsupported_queries.is_empty());
}

#[test]
fn single_matched_rival_splits_the_run() {
    let plan = plan_seconds(60);
    let rows = [
        matched(O),
        weak(O),
        matched(O),
        weak(O),
        // One Matched query at a rival offset ends the first run.
        matched(O + 30.0),
        weak(O + 30.0),
        weak(O + 30.0),
        matched(O + 30.0),
        // Two weak queries at the first offset cannot form a segment.
        weak(O),
        weak(O),
    ];
    let out = run(&plan, &rows);
    assert_eq!(out.segments.len(), 2, "{out:?}");
    assert_eq!(out.segments[0].supporting_queries, vec![0, 1, 2, 3]);
    assert_eq!(out.segments[1].supporting_queries, vec![4, 5, 6, 7]);
    assert_eq!(out.boundaries.len(), 1);
    // Midpoint of query 3's centre (168000) and query 4's centre (216000).
    assert_eq!(out.boundaries[0].sample_index, 192_000);
    assert_eq!(out.unsupported_queries, vec![8, 9]);
}

#[test]
fn random_offsets_give_no_segments() {
    let plan = plan_seconds(60);
    // Ten offsets, each at least one second from the others.
    let rows: Vec<Row> = (0..10).map(|i| matched(f64::from((i * 37) % 10))).collect();
    let out = run(&plan, &rows);
    assert!(out.segments.is_empty(), "{out:?}");
    assert!(out.boundaries.is_empty());
    assert_eq!(out.unsupported_queries, (0..10).collect::<Vec<_>>());
}

#[test]
fn sub_threshold_peaks_never_vote() {
    let plan = plan_seconds(30);
    let rows: Vec<Row> = (0..5)
        .map(|_| Row {
            peaks: vec![(O, 0.2)],
            matched: false,
        })
        .collect();
    let out = run(&plan, &rows);
    assert!(out.segments.is_empty());
    assert_eq!(out.unsupported_queries, vec![0, 1, 2, 3, 4]);
}

#[test]
fn drift_of_3000_ppm_is_rejected_by_policy() {
    // Offsets rise 3000 ppm across the segment; all stay within one cluster.
    let drifting: Vec<Row> = [true, false, true, false, false]
        .iter()
        .enumerate()
        .map(|(i, &m)| {
            let offset = O + 3000e-6 * (3.0 + 6.0 * i as f64);
            Row {
                peaks: vec![(offset, if m { STRONG } else { WEAK })],
                matched: m,
            }
        })
        .collect();

    let strict = plan_seconds(30);
    let out = run(&strict, &drifting);
    assert!(out.segments.is_empty(), "{out:?}");
    assert_eq!(out.unsupported_queries, vec![0, 1, 2, 3, 4]);

    // Control: the same evidence is accepted once the policy allows it, and the
    // reported drift is the injected 3000 ppm.
    let loose = plan_with(&[(0, 30 * 8000)], 5000.0);
    let out = run(&loose, &drifting);
    assert_eq!(out.segments.len(), 1, "{out:?}");
    assert!((out.segments[0].drift_ppm - 3000.0).abs() < 1e-3);
}

#[test]
fn tie_in_similarity_resolves_to_the_lower_offset() {
    let plan = plan_seconds(30);
    let rows: Vec<Row> = (0..5)
        .map(|_| Row {
            peaks: vec![(O + 0.5, STRONG), (O, STRONG)],
            matched: true,
        })
        .collect();
    let out = run(&plan, &rows);
    assert_eq!(out.segments.len(), 1, "{out:?}");
    assert!((out.segments[0].offset_seconds - O).abs() < 1e-9);
    assert!(out.unsupported_queries.is_empty());
}

#[test]
fn audio_gap_between_segments_breaks_the_run_with_capture_interrupted_boundary() {
    // Two 36 s captures separated by 12 s of missing audio; each holds six queries.
    let plan = plan_with(&[(0, 288_000), (384_000, 288_000)], 2000.0);
    assert_eq!(plan.queries.len(), 12);
    let rows = [
        matched(O),
        weak(O),
        matched(O),
        weak(O),
        weak(O),
        weak(O),
        weak(O),
        matched(O),
        weak(O),
        matched(O),
        weak(O),
        weak(O),
    ];
    let out = run(&plan, &rows);
    assert_eq!(out.segments.len(), 2, "{out:?}");
    assert_eq!(out.boundaries.len(), 1);
    assert_eq!(out.boundaries[0].kind, SyncBoundaryKind::CaptureInterrupted);
    assert_eq!(out.boundaries[0].sample_index, (264_000 + 408_000) / 2);
}

#[test]
fn incomplete_search_is_rejected_with_search_incomplete() {
    let plan = plan_seconds(30);
    let rows = [weak(O), matched(O), weak(O), weak(O), matched(O)];
    let mut evidence = evidence_for(&plan, &rows);
    evidence[2].completed_references -= 1;
    let err = consensus_from_evidence(&plan, &evidence, &|| false).unwrap_err();
    assert!(err.to_string().contains("search_incomplete"), "{err}");

    let full = evidence_for(&plan, &rows);
    let err = consensus_from_evidence(&plan, &full[..4], &|| false).unwrap_err();
    assert!(err.to_string().contains("search_incomplete"), "{err}");
}

#[test]
fn empty_cache_is_an_incomplete_search_through_the_public_entry_point() {
    let plan = plan_seconds(30);
    let dir = tempfile::tempdir().unwrap();
    let err = consensus(&plan, dir.path(), &|| false).unwrap_err();
    assert!(err.to_string().contains("search_incomplete"), "{err}");
}

#[test]
fn cancellation_stops_consensus() {
    let plan = plan_seconds(30);
    let rows = [weak(O), matched(O), weak(O), weak(O), matched(O)];
    let err = consensus_from_evidence(&plan, &evidence_for(&plan, &rows), &|| true).unwrap_err();
    assert!(err.to_string().contains("cancelled"), "{err}");
}
