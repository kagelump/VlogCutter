use crate::review_discussions::tests::{calibration_event, cue_event};
use crate::review_targets::tests::{PROJECT, now};
use crate::review_transcripts::tests::{FakeRuntime, Fixture, fixture, local, run};
use crate::{ReviewSessionEventKind, ReviewSessionEventRequest, ReviewWorkspace};
use rcdvlog_core::tv_review_sync::SyncState;

fn event(
    f: &Fixture,
    id: &str,
    kind: ReviewSessionEventKind,
    frame: u64,
    detail: serde_json::Value,
) {
    f.catalog
        .review_sessions(0)
        .record_event(
            PROJECT,
            &f.session_id,
            &ReviewSessionEventRequest {
                event_id: id.into(),
                kind,
                audio_frame: frame,
                performance_ms: Some(1.0),
                declared_movie_seconds: None,
                detail: Some(detail),
            },
            now(),
        )
        .unwrap();
}

fn workspace(f: &Fixture) -> ReviewWorkspace {
    let run_id = run(f, &local(), &FakeRuntime::new()).run_id;
    f.catalog
        .review_discussions()
        .workspace(PROJECT, &run_id)
        .unwrap()
}

#[test]
fn a_capture_interruption_between_cues_is_never_interpolated() {
    let f = fixture(false, true);
    calibration_event(&f, "calibration_0", 0, 0);
    cue_event(&f, "cue_a", 0, 500.0, 0.0);
    cue_event(&f, "cue_b", 0, 5500.0, 5.0);
    // A discontinuity at 2.5 s splits the first utterance (~1–6 s).
    event(
        &f,
        "gap_1",
        ReviewSessionEventKind::CaptureDiscontinuity,
        120_000,
        serde_json::json!({"missingFrames": 480, "clockEpoch": 0}),
    );
    let w = workspace(&f);
    assert_eq!(w.sync.boundaries, 1);
    let first = &w.utterances[0];
    assert!(
        first
            .sync
            .slices
            .iter()
            .any(|s| s.recording.end_sample == 120_000 || s.recording.start_sample == 120_000)
    );
    // Neither side of the interruption has two consistent anchors: no cut.
    assert!(
        first
            .sync
            .slices
            .iter()
            .all(|s| s.state != SyncState::Estimated)
    );
    assert!(first.candidate_operation_ids.is_empty());
    assert!(first.movie_time.is_none());
}

#[test]
fn each_clock_epoch_is_its_own_capture_clock_and_queries_split_at_it() {
    let f = fixture(false, true);
    calibration_event(&f, "calibration_0", 0, 0);
    // After a suspend/resume the browser starts a new clock at 3 s.
    calibration_event(&f, "calibration_1", 1, 144_000);
    cue_event(&f, "cue_a", 0, 500.0, 0.0);
    let w = workspace(&f);
    assert_eq!(w.sync.calibrated_clocks, 2);
    let first = &w.utterances[0];
    let clocks: Vec<_> = first
        .sync
        .slices
        .iter()
        .map(|s| s.recording.clock_id.as_str())
        .collect();
    assert!(clocks.contains(&"mic_epoch_0") && clocks.contains(&"mic_epoch_1"));
    assert_eq!(first.sync.uncovered_frames, 0);
    // One cue and no extrapolation: nothing is estimated.
    assert!(first.candidate_operation_ids.is_empty());
}

#[test]
fn inconsistent_calibration_or_a_cue_from_an_uncalibrated_epoch_is_not_used() {
    let f = fixture(false, true);
    calibration_event(&f, "calibration_0", 0, 0);
    // Same clock id, different performance epoch: the clock is refused.
    event(
        &f,
        "calibration_bad",
        ReviewSessionEventKind::ClockSample,
        48_000,
        serde_json::json!({
            "schema": "review-capture-calibration.v1",
            "clockId": "mic_epoch_0",
            "clockEpoch": 0,
            "sampleRate": 48_000,
            "firstSample": 0,
            "monotonicOriginNs": 0,
            "monotonicToleranceNs": 3_000_000,
            "performanceEpochMs": 10.0,
            "uncertaintyMs": 3.0,
        }),
    );
    let w = workspace(&f);
    assert_eq!(w.sync.status, "pending");
    assert!(w.sync.diagnostics.iter().any(|d| d.contains("disagree")));

    let g = fixture(false, true);
    calibration_event(&g, "calibration_0", 0, 0);
    cue_event(&g, "cue_epoch_7", 7, 500.0, 0.0);
    let w = workspace(&g);
    assert_eq!(w.sync.status, "noAnchors");
    assert!(w.sync.diagnostics.iter().any(|d| d.contains("cue_epoch_7")));
    assert!(
        w.utterances
            .iter()
            .all(|u| u.candidate_operation_ids.is_empty())
    );
}
