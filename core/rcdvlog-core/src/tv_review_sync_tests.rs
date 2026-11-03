use super::*;
fn fixture() -> SyncRequest {
    SyncRequest {
        schema_version: 1,
        session_id: "session_synthetic".into(),
        target_digest: "a".repeat(64),
        movie: SyncMovieClock {
            movie_digest: "b".repeat(64),
            time_base_numerator: 1,
            time_base_denominator: 90000,
            origin_pts: 90000,
            duration_pts: 9000000,
        },
        capture_clocks: vec![CaptureClock {
            clock_id: "mic".into(),
            sample_rate: 48000,
            first_sample: 0,
            last_sample: 48000 * 120,
            monotonic_origin_ns: 3000000000,
            monotonic_tolerance_ns: 2_000_000,
        }],
        anchors: vec![],
        boundaries: vec![],
        policy: SyncPolicy::default(),
        consensus_segments: vec![],
    }
}
fn anchor(
    r: &SyncRequest,
    id: &str,
    second: f64,
    movie: f64,
    source: SyncAnchorSource,
) -> SyncAnchor {
    SyncAnchor {
        evidence_id: id.into(),
        capture: CapturePosition {
            clock_id: "mic".into(),
            sample_index: (second * 48000.0) as u64,
            monotonic_ns: 3000000000 + (second * 1e9) as u64,
        },
        movie_pts: r.movie.origin_pts + (movie * 90000.0) as i64,
        source,
        confidence: 0.99,
        movie_uncertainty_ticks: 900,
        capture_uncertainty_samples: 240,
        candidate_group_id: None,
    }
}
fn q(a: f64, b: f64) -> SyncQuery {
    SyncQuery {
        clock_id: "mic".into(),
        start_sample: (a * 48000.0) as u64,
        end_sample: (b * 48000.0) as u64,
    }
}
#[test]
fn manual_start_is_bounded_approximate_and_drift_is_measured() {
    let mut r = fixture();
    r.anchors
        .push(anchor(&r, "start", 2.0, 10.0, SyncAnchorSource::ManualCue));
    assert_eq!(
        r.map(&[q(3.0, 4.0)], &|| false).unwrap().slices[0].state,
        SyncState::Unknown
    );
    r.policy.max_extrapolation_seconds = 5.0;
    let e = r.map(&[q(3.0, 4.0)], &|| false).unwrap();
    let s = &e.slices[0];
    assert_eq!(s.state, SyncState::Estimated);
    assert_eq!(s.candidates[0].start_pts, 1080000.0);
    assert!(s.diagnostics.contains(&"assumed_uninterrupted_1x".into()));
    assert!(s.candidates[0].uncertainty_ticks > 900);
    r.anchors.push(anchor(
        &r,
        "later",
        12.0,
        20.01,
        SyncAnchorSource::AcousticTimecode,
    ));
    let e = r.map(&[q(3.0, 4.0)], &|| false).unwrap();
    let a = &e.slices[0].candidates[0];
    assert!((a.measured_drift_ppm.unwrap() - 1000.0).abs() < 1e-6);
    let expected = 90000.0 + 11.001 * 90000.0;
    assert!((a.start_pts - expected).abs() < 0.01);
    assert!(a.lower_pts as f64 <= expected && a.upper_pts as f64 >= expected);
    assert_eq!(
        r.map(&[q(30.0, 31.0)], &|| false).unwrap().slices[0].state,
        SyncState::Unknown
    );
}
#[test]
fn discontinuities_missing_markers_seek_and_rewind_never_interpolate() {
    let mut r = fixture();
    r.anchors = vec![
        anchor(&r, "a", 0.0, 0.0, SyncAnchorSource::ManualCue),
        anchor(&r, "b", 10.0, 10.0, SyncAnchorSource::PassiveSoundtrack),
    ];
    r.boundaries.push(SyncBoundary {
        evidence_id: "buffer".into(),
        clock_id: "mic".into(),
        sample_index: 5 * 48000,
        kind: SyncBoundaryKind::Buffering,
    });
    let e = r.map(&[q(4.0, 6.0)], &|| false).unwrap();
    assert_eq!(e.slices.len(), 2);
    assert!(e.slices.iter().all(|s| s.state == SyncState::Unknown));
    r.boundaries.clear();
    r.anchors[1].movie_pts = 90000 + 30 * 90000;
    assert_eq!(
        r.map(&[q(4.0, 6.0)], &|| false).unwrap().slices[0].state,
        SyncState::Conflicting
    );
    r.anchors[1].movie_pts = 90000;
    assert_eq!(
        r.map(&[q(4.0, 6.0)], &|| false).unwrap().slices[0].state,
        SyncState::Conflicting
    );
    r.anchors[1] = anchor(&r, "far", 100.0, 100.0, SyncAnchorSource::AcousticTimecode);
    assert_eq!(
        r.map(&[q(4.0, 6.0)], &|| false).unwrap().slices[0].state,
        SyncState::Unknown
    );
}
#[test]
fn repeated_soundtrack_conflicts_weak_points_and_raw_evidence_survive() {
    let mut r = fixture();
    r.anchors = vec![
        anchor(&r, "manual", 2.0, 10.0, SyncAnchorSource::ManualCue),
        anchor(&r, "auto", 2.0, 50.0, SyncAnchorSource::PassiveSoundtrack),
    ];
    let e = r.map(&[q(2.0, 2.0)], &|| false).unwrap();
    assert_eq!(e.slices[0].state, SyncState::Conflicting);
    assert_eq!(e.slices[0].candidates.len(), 2);
    r.anchors[1].confidence = 0.2;
    let e = r.map(&[q(2.0, 2.0)], &|| false).unwrap();
    assert_eq!(e.slices[0].state, SyncState::Anchored);
    assert_eq!(e.rejected_evidence_ids, ["auto"]);
    assert_eq!(e.anchors, r.anchors);
    r.anchors[0].candidate_group_id = Some("repeated".into());
    r.anchors[1].candidate_group_id = Some("repeated".into());
    assert_eq!(
        r.map(&[q(2.0, 2.0)], &|| false).unwrap().slices[0].state,
        SyncState::Conflicting
    );
}
#[test]
fn independent_runs_preserve_one_to_many_movie_locations() {
    let mut r = fixture();
    r.policy.max_extrapolation_seconds = 1.0;
    r.anchors = vec![
        anchor(&r, "first", 2.0, 10.0, SyncAnchorSource::ManualCue),
        anchor(&r, "rewound", 12.0, 10.0, SyncAnchorSource::ManualCue),
    ];
    r.boundaries.push(SyncBoundary {
        evidence_id: "seek".into(),
        clock_id: "mic".into(),
        sample_index: 12 * 48000,
        kind: SyncBoundaryKind::Rewind,
    });
    let e = r.map(&[q(2.0, 2.0), q(12.0, 12.0)], &|| false).unwrap();
    assert_eq!(
        e.slices[0].candidates[0].start_pts,
        e.slices[1].candidates[0].start_pts
    );
    let mut other = r.capture_clocks[0].clone();
    other.clock_id = "mic_restart".into();
    other.sample_rate = 44100;
    other.monotonic_origin_ns = 4000000000;
    r.capture_clocks.push(other);
    let mut a = anchor(&r, "restart", 0.0, 20.0, SyncAnchorSource::ManualCue);
    a.capture.clock_id = "mic_restart".into();
    a.capture.monotonic_ns = 4000000000;
    r.anchors.push(a);
    let e = r
        .map(
            &[SyncQuery {
                clock_id: "mic_restart".into(),
                start_sample: 44100,
                end_sample: 44100,
            }],
            &|| false,
        )
        .unwrap();
    assert_eq!(
        e.slices[0].candidates[0].start_pts,
        90000.0 + 21.0 * 90000.0
    );
}
#[test]
fn bounded_versioned_input_cancellation_and_determinism() {
    let mut r = fixture();
    r.anchors
        .push(anchor(&r, "a", 1.0, 1.0, SyncAnchorSource::ManualCue));
    let bytes = serde_json::to_vec(&r).unwrap();
    assert_eq!(SyncRequest::from_json(&bytes).unwrap(), r);
    assert_eq!(
        r.map(&[q(1.0, 1.0)], &|| false).unwrap(),
        r.map(&[q(1.0, 1.0)], &|| false).unwrap()
    );
    assert!(r.map(&[q(1.0, 1.0)], &|| true).is_err());
    assert!(SyncRequest::from_json(&vec![b' '; SYNC_MAX_BYTES + 1]).is_err());
    r.anchors[0].capture.monotonic_ns += 100000000;
    assert!(r.validate().is_err());
    r.anchors[0].capture.monotonic_ns -= 100000000;
    r.schema_version = 2;
    assert!(r.validate().is_err());
}
#[test]
fn exact_target_lookup_uses_uncertainty_and_nonzero_pts_origin() {
    let target: ReviewTargetHandoff =
        serde_json::from_str(include_str!("../tests/fixtures/review-target/v1.json")).unwrap();
    let mut r = fixture();
    r.target_digest = target.target_digest.clone();
    r.movie.movie_digest = target.target.movie.blake3.clone();
    r.movie.duration_pts = 540900;
    // The fixture has a 6.01-second container with 6 seconds of pictures.
    r.movie.duration_pts = (target.target.movie.duration_seconds * 90000.0).round() as u64;
    r.anchors.push(anchor(
        &r,
        "boundary",
        1.0,
        2.0,
        SyncAnchorSource::ManualCue,
    ));
    let report = r.map_to_cuts(&[q(1.0, 1.0)], &target, &|| false).unwrap();
    assert_eq!(
        report.slices[0].candidate_operation_ids,
        ["editop_split_a", "editop_split_b"]
    );
    r.target_digest = "f".repeat(64);
    assert!(r.map_to_cuts(&[q(1.0, 1.0)], &target, &|| false).is_err());
}

#[test]
fn published_fixture_matches_schema_and_core_contract() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../tests/fixtures/tv-review-sync/manual-v1.json"
    ))
    .unwrap();
    let request =
        SyncRequest::from_json(&serde_json::to_vec(&fixture["request"]).unwrap()).unwrap();
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../docs/contracts/tv-review-sync-v1.schema.json"
    ))
    .unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&fixture["request"])
        .unwrap();
    let queries: Vec<SyncQuery> = serde_json::from_value(fixture["queries"].clone()).unwrap();
    assert_eq!(
        request.map(&queries, &|| false).unwrap().slices[0].state,
        SyncState::Estimated
    );
}

#[test]
fn synthetic_affine_error_stays_inside_reported_bounds_across_sample_rates() {
    let mut max_error_ticks = 0.0f64;
    for rate in [8000, 16000, 44100, 48000, 96000] {
        for ppm in [-1500.0, -300.0, 0.0, 700.0, 1500.0] {
            let mut r = fixture();
            r.capture_clocks[0].sample_rate = rate;
            r.capture_clocks[0].last_sample = rate as u64 * 120;
            for (id, t) in [("a", 1.0), ("b", 11.0)] {
                let mut a = anchor(
                    &r,
                    id,
                    t,
                    10.0 + t * (1.0 + ppm / 1e6),
                    SyncAnchorSource::AcousticTimecode,
                );
                a.capture.sample_index = (t * rate as f64) as u64;
                r.anchors.push(a);
            }
            for t in [2.0, 3.5, 7.0, 10.0] {
                let sample = (t * rate as f64) as u64;
                let report = r
                    .map(
                        &[SyncQuery {
                            clock_id: "mic".into(),
                            start_sample: sample,
                            end_sample: sample,
                        }],
                        &|| false,
                    )
                    .unwrap();
                let e = &report.slices[0].candidates[0];
                let truth = 90000.0 + (10.0 + t * (1.0 + ppm / 1e6)) * 90000.0;
                max_error_ticks = max_error_ticks.max((e.start_pts - truth).abs());
                assert!(e.lower_pts as f64 <= truth && e.upper_pts as f64 >= truth);
            }
        }
    }
    assert!(
        max_error_ticks <= 1.0,
        "max injected-affine error {max_error_ticks} ticks"
    );
}

#[test]
fn overlapping_alternative_queries_cannot_amplify_unbounded_reports() {
    let mut r = fixture();
    for second in [1.0, 5.0, 10.0] {
        for n in 0..8 {
            let id = format!("evidence_{}_{}", second as u32, n);
            r.anchors.push(anchor(
                &r,
                &id,
                second,
                second,
                SyncAnchorSource::PassiveSoundtrack,
            ));
        }
    }
    let err = r.map(&vec![q(2.0, 9.0); 256], &|| false).unwrap_err();
    assert!(err.to_string().contains("candidate budget"));
}

#[test]
fn broad_uncertainty_cannot_amplify_movie_operation_responses() {
    let mut target: ReviewTargetHandoff =
        serde_json::from_str(include_str!("../tests/fixtures/review-target/v1.json")).unwrap();
    let original = target.target.cuts.clone();
    target.target.cuts.clear();
    for i in 0..12 {
        for cut in &original {
            let mut c = cut.clone();
            c.operation_id = format!("{}_copy_{}", c.operation_id, i);
            c.position = target.target.cuts.len() as u32;
            c.movie_in_pts += i * 540000;
            c.movie_out_pts += i * 540000;
            target.target.cuts.push(c);
        }
    }
    target.target.movie.duration_seconds = 72.02;
    target.target.movie.video_frame_count = 2160;
    target.target_digest = ReviewTargetHandoff::snapshot_digest(&target.target).unwrap();
    target.validate().unwrap();
    let mut r = fixture();
    r.target_digest = target.target_digest.clone();
    r.movie.movie_digest = target.target.movie.blake3.clone();
    r.movie.duration_pts = (72.02 * 90000.0) as u64;
    let mut a = anchor(&r, "broad_manual", 1.0, 2.0, SyncAnchorSource::ManualCue);
    a.movie_uncertainty_ticks = r.movie.duration_pts;
    r.anchors.push(a);
    let err = r
        .map_to_cuts(&vec![q(1.0, 1.0); 256], &target, &|| false)
        .unwrap_err();
    assert!(err.to_string().contains("operation budget"));
}

fn consensus_segment(
    id: &str,
    start_seconds: f64,
    end_seconds: f64,
) -> crate::tv_soundtrack_consensus::ConsensusSegment {
    crate::tv_soundtrack_consensus::ConsensusSegment {
        segment_id: id.into(),
        clock_id: "mic".into(),
        offset_seconds: -0.96,
        mic_start_sample: (start_seconds * 48000.0) as u64,
        mic_end_sample: (end_seconds * 48000.0) as u64,
        supporting_queries: vec![0, 1, 2],
        residual_rms_seconds: 0.02,
        drift_ppm: 0.0,
    }
}

/// Anchors 60 s apart in mic time (2 s and 62 s) and movie time (10 s and 70 s).
fn far_anchors(r: &SyncRequest) -> Vec<SyncAnchor> {
    vec![
        anchor(r, "near_left", 2.0, 10.0, SyncAnchorSource::ManualCue),
        anchor(
            r,
            "near_right",
            62.0,
            70.0,
            SyncAnchorSource::AcousticTimecode,
        ),
    ]
}

#[test]
fn consensus_interpolation_allowed_inside_one_segment_with_section_nine_uncertainty() {
    let mut r = fixture();
    r.anchors = far_anchors(&r);
    r.consensus_segments = vec![consensus_segment("seg_one", 0.0, 100.0)];
    r.consensus_segments[0].offset_seconds = 8.0;
    let e = r.map(&[q(20.0, 21.0)], &|| false).unwrap();
    let s = &e.slices[0];
    assert_eq!(s.state, SyncState::ConsensusInterpolated);
    assert_eq!(s.diagnostics, ["consensus_segment:seg_one"]);
    assert_eq!(s.candidates.len(), 1);
    let c = &s.candidates[0];
    assert!((c.start_pts - (90000.0 + 28.0 * 90000.0)).abs() < 0.01);
    assert!((c.measured_drift_ppm.unwrap()).abs() < 1e-6);
    // Anchor uncertainty 1350 ticks each (900 + 240 samples at 48 kHz),
    // residual 0.02 s = 1800 ticks, unobserved 5000 ppm over 60 s = 27000 ticks.
    assert_eq!(c.uncertainty_ticks, 1350 + 1800 + 27000);
    assert_eq!(c.uncertainty_ticks, 30150);
    assert!(c.lower_pts as f64 <= c.start_pts && c.upper_pts as f64 >= c.end_pts);
}

#[test]
fn same_far_anchors_without_a_segment_stay_unknown() {
    let mut r = fixture();
    r.anchors = far_anchors(&r);
    let e = r.map(&[q(20.0, 21.0)], &|| false).unwrap();
    assert_eq!(e.slices[0].state, SyncState::Unknown);
    assert!(e.slices[0].candidates.is_empty());
    assert!(
        e.slices[0]
            .diagnostics
            .contains(&"gap_or_discontinuity".into())
    );
}

#[test]
fn consensus_interpolation_refused_across_a_boundary_inside_the_gap() {
    let mut r = fixture();
    r.anchors = far_anchors(&r);
    r.consensus_segments = vec![consensus_segment("seg_one", 0.0, 100.0)];
    r.boundaries.push(SyncBoundary {
        evidence_id: "pause_in_gap".into(),
        clock_id: "mic".into(),
        sample_index: 30 * 48000,
        kind: SyncBoundaryKind::Pause,
    });
    let e = r.map(&[q(20.0, 21.0)], &|| false).unwrap();
    assert_eq!(e.slices[0].state, SyncState::Unknown);
    assert!(e.slices[0].candidates.is_empty());
}

#[test]
fn consensus_interpolation_refused_outside_the_supporting_span() {
    // Right anchor (62 s) lies past the segment's last supporting query.
    let mut r = fixture();
    r.anchors = far_anchors(&r);
    r.consensus_segments = vec![consensus_segment("seg_short", 0.0, 50.0)];
    assert_eq!(
        r.map(&[q(20.0, 21.0)], &|| false).unwrap().slices[0].state,
        SyncState::Unknown
    );
    // Left anchor (2 s) lies before the segment's first supporting query.
    r.consensus_segments = vec![consensus_segment("seg_late", 5.0, 100.0)];
    assert_eq!(
        r.map(&[q(20.0, 21.0)], &|| false).unwrap().slices[0].state,
        SyncState::Unknown
    );
    // A segment on another capture clock never applies.
    let mut clock_other = r.capture_clocks[0].clone();
    clock_other.clock_id = "mic_other".into();
    r.capture_clocks.push(clock_other);
    let mut other = consensus_segment("seg_other", 0.0, 100.0);
    other.clock_id = "mic_other".into();
    r.consensus_segments = vec![other];
    assert_eq!(
        r.map(&[q(20.0, 21.0)], &|| false).unwrap().slices[0].state,
        SyncState::Unknown
    );
}

#[test]
fn consensus_does_not_override_anchors_that_contradict_the_gap_progress() {
    // Movie advances 90 s over 60 s of mic time: far beyond drift and residual tolerance.
    let mut r = fixture();
    r.anchors = far_anchors(&r);
    r.anchors[1].movie_pts = 90000 + 100 * 90000;
    r.consensus_segments = vec![consensus_segment("seg_one", 0.0, 100.0)];
    let e = r.map(&[q(20.0, 21.0)], &|| false).unwrap();
    let s = &e.slices[0];
    assert_eq!(s.state, SyncState::Conflicting);
    assert!(
        s.diagnostics
            .contains(&"inconsistent_progress_requires_resync".into())
    );
    assert!(s.candidates.is_empty());
}

#[test]
fn empty_consensus_segments_leave_v1_mapping_and_bytes_unchanged() {
    let mut r = fixture();
    r.anchors = far_anchors(&r);
    assert!(r.consensus_segments.is_empty());
    let json = serde_json::to_string(&r).unwrap();
    assert!(!json.contains("consensusSegments"));
    let e = r.map(&[q(20.0, 21.0), q(2.0, 2.0)], &|| false).unwrap();
    assert_eq!(e.slices[0].state, SyncState::Unknown);
    assert_eq!(e.slices[1].state, SyncState::Anchored);
    let again = r.map(&[q(20.0, 21.0), q(2.0, 2.0)], &|| false).unwrap();
    assert_eq!(e, again);
}

#[test]
fn consensus_segment_input_is_validated_before_mapping() {
    let mut r = fixture();
    r.consensus_segments = vec![consensus_segment("seg_bad", 50.0, 10.0)];
    assert!(r.validate().is_err());
    r.consensus_segments = vec![consensus_segment("seg_ok", 10.0, 50.0)];
    r.consensus_segments[0].clock_id = "unknown_clock".into();
    assert!(r.validate().is_err());
}
