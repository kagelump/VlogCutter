//! Deterministic assembled-segment cases (constructed evidence, fabricated
//! accepted windows/support) and the read-only tv_review_sync adapter proof.

use super::*;
use crate::source_audio_pooled::{
    PooledModeSupportCell, PooledOffsetModeRow, PooledWindowAcceptance, PooledWindowResult,
};
use crate::tv_review_sync::SyncBoundaryKind;
use serde_json::Value;

const RATE: u32 = 11_025;
const CLOCK_ID: &str = "clock-1";

fn secs(seconds: u64) -> u64 {
    seconds * u64::from(RATE)
}

fn movie() -> SyncMovieClock {
    SyncMovieClock {
        movie_digest: "ab".repeat(32),
        time_base_numerator: 1,
        time_base_denominator: 1000,
        origin_pts: 0,
        duration_pts: 600_000,
    }
}

fn clock(first_sample: u64, last_sample: u64) -> CaptureClock {
    CaptureClock {
        clock_id: CLOCK_ID.into(),
        sample_rate: RATE,
        first_sample,
        last_sample,
        monotonic_origin_ns: 0,
        monotonic_tolerance_ns: 1_000,
    }
}

fn epoch(id: &str, start_sample: u64, end_sample: u64) -> CaptureEpoch {
    CaptureEpoch {
        epoch_id: id.into(),
        start_sample,
        end_sample,
    }
}

fn domain(start_sample: u64, end_sample: u64) -> SampleRange {
    SampleRange {
        start_sample,
        end_sample,
    }
}

/// Anchor frames consistent with their one-second cell (absolute feature
/// frames whose sample positions `frame * 128` fall inside cell `bin`).
fn frames(bin: u32) -> Vec<u32> {
    vec![bin * 87, bin * 87 + 10, bin * 87 + 20]
}

fn cell(bin: u32, frames: &[u32], evidence_id: &str) -> SupportCell {
    SupportCell {
        time_bin_seconds: f64::from(bin),
        query_anchor_frames: frames.to_vec(),
        evidence_id: evidence_id.into(),
    }
}

#[allow(clippy::too_many_arguments)]
fn window(
    epoch_start: u64,
    epoch_id: &str,
    id: &str,
    start_seconds: u32,
    offset_bin: Option<i32>,
    cells: Vec<SupportCell>,
    complete: bool,
    accepted: Option<bool>,
    failures: &[&str],
) -> AcceptedWindow {
    let rate = u64::from(RATE);
    AcceptedWindow {
        window_id: id.into(),
        capture_clock_id: CLOCK_ID.into(),
        epoch_id: epoch_id.into(),
        start_sample: epoch_start + u64::from(start_seconds) * rate,
        end_sample: epoch_start + u64::from(start_seconds) * rate + 10 * rate,
        window_start_seconds: f64::from(start_seconds),
        duration_seconds: 10,
        offset_seconds: offset_bin.map(|bin| f64::from(bin) * OFFSET_BIN_SECONDS),
        offset_bin,
        complete,
        accepted,
        failure_reasons: failures
            .iter()
            .map(|reason| (*reason).to_string())
            .collect(),
        supporting_cells: cells,
    }
}

fn accepted_window(
    epoch_start: u64,
    epoch_id: &str,
    id: &str,
    start_seconds: u32,
    offset_bin: i32,
    bins: &[u32],
) -> AcceptedWindow {
    let cells = bins
        .iter()
        .map(|bin| cell(*bin, &frames(*bin), &format!("{id}b{bin}")))
        .collect();
    window(
        epoch_start,
        epoch_id,
        id,
        start_seconds,
        Some(offset_bin),
        cells,
        true,
        Some(true),
        &[],
    )
}

fn assembly(
    capture_clock: CaptureClock,
    epochs: Vec<CaptureEpoch>,
    range: SampleRange,
    windows: Vec<AcceptedWindow>,
    boundaries: Vec<SyncBoundary>,
) -> SegmentAssemblyRequest {
    SegmentAssemblyRequest {
        capture_clock,
        movie: movie(),
        epochs,
        domain: range,
        windows,
        boundaries,
        recording_identity: Some("recording-identity".into()),
        index_identity: Some("index-identity".into()),
    }
}

/// Supported spans + envelope holes + unknown ranges must tile the requested
/// domain exactly, without overlap.
fn assert_accounting(report: &SegmentReport, range: SampleRange) {
    let mut pieces: Vec<(u64, u64)> = Vec::new();
    for segment in &report.segments {
        for span in &segment.supported_spans {
            pieces.push((span.start_sample, span.end_sample));
        }
        for span in &segment.unknown_spans {
            pieces.push((span.start_sample, span.end_sample));
        }
    }
    for unknown in &report.unknown_ranges {
        pieces.push((unknown.start_sample, unknown.end_sample));
    }
    pieces.sort();
    let mut expected = range.start_sample;
    for (start, end) in pieces {
        assert_eq!(start, expected, "accounting must be non-overlapping");
        assert!(end > start);
        expected = end;
    }
    assert_eq!(expected, range.end_sample, "accounting must be complete");
}

/// Zero invented movie positions: the serialized report carries no movie PTS
/// inside any unknown range or unknown span.
fn assert_unknowns_have_no_movie_position(report: &SegmentReport) {
    let value = serde_json::to_value(report).unwrap();
    let no_pts_keys = |value: &Value| {
        let mut stack = vec![value];
        while let Some(node) = stack.pop() {
            match node {
                Value::Object(map) => {
                    for (key, child) in map {
                        assert!(
                            !key.to_lowercase().contains("pts"),
                            "unknown must never carry a movie position: {key}"
                        );
                        stack.push(child);
                    }
                }
                Value::Array(items) => stack.extend(items.iter()),
                _ => {}
            }
        }
    };
    for unknown in value["unknownRanges"].as_array().unwrap() {
        no_pts_keys(unknown);
    }
    for segment in value["segments"].as_array().unwrap() {
        for span in segment["unknownSpans"].as_array().unwrap() {
            no_pts_keys(span);
        }
    }
}

fn boundary(id: &str, sample_index: u64, kind: SyncBoundaryKind) -> SyncBoundary {
    SyncBoundary {
        evidence_id: id.into(),
        clock_id: CLOCK_ID.into(),
        sample_index,
        kind,
    }
}

// ---------------------------------------------------------------------------
// Required deterministic cases (constructed evidence).

#[test]
fn isolated_cells_outside_a_windows_main_support_run_stay_unknown() {
    // Release-comparison regression (rewind capture): the window at 28 s is
    // accepted on its post-rewind run (cells 33-37), but a repetitive
    // soundtrack passage also matched its offset in cell 29, three seconds
    // before that run. Cell 29 is uncorroborated and must not become a
    // one-second segment at the wrong place.
    let range = domain(0, secs(40));
    let report = assemble(&assembly(
        clock(0, secs(40)),
        vec![epoch("e0", 0, secs(40))],
        range,
        vec![accepted_window(
            0,
            "e0",
            "w28",
            28,
            30_569,
            &[29, 33, 34, 35, 36, 37],
        )],
        vec![],
    ))
    .unwrap();
    assert_eq!(report.assembler_id, "source-audio-segments-mvp-v2");
    assert_eq!(report.segments.len(), 1, "{report:?}");
    assert_eq!(report.segments[0].start_sample, secs(33));
    assert_eq!(report.segments[0].end_sample, secs(38));
    assert!(
        report
            .unknown_ranges
            .iter()
            .any(|unknown| unknown.start_sample <= secs(29) && unknown.end_sample >= secs(30)),
        "cell 29 stays unknown"
    );
    assert_accounting(&report, range);
    assert_unknowns_have_no_movie_position(&report);

    // A gap of at most two unsupported seconds still joins the run, and of
    // two equally supported runs the earliest is kept.
    let report = assemble(&assembly(
        clock(0, secs(40)),
        vec![epoch("e0", 0, secs(40))],
        range,
        vec![accepted_window(
            0,
            "e0",
            "w10",
            10,
            25,
            &[10, 11, 13, 17, 18, 19],
        )],
        vec![],
    ))
    .unwrap();
    let supported: Vec<(u64, u64)> = report
        .segments
        .iter()
        .flat_map(|segment| segment.supported_spans.iter())
        .map(|span| (span.start_sample, span.end_sample))
        .collect();
    // Runs {10, 11, 13} (one unsupported second) and {17, 18, 19} tie.
    assert_eq!(supported, vec![(secs(10), secs(12)), (secs(13), secs(14))]);
    assert_accounting(&report, range);
}

#[test]
fn continuous_playback_coalesces_multiple_accepted_windows_to_one_segment() {
    let range = domain(0, secs(30));
    let request = assembly(
        clock(0, secs(30)),
        vec![epoch("e0", 0, secs(30))],
        range,
        vec![
            accepted_window(0, "e0", "w1", 0, -37, &(0..=9).collect::<Vec<u32>>()),
            accepted_window(0, "e0", "w2", 2, -37, &(2..=11).collect::<Vec<u32>>()),
            accepted_window(0, "e0", "w3", 4, -37, &(4..=13).collect::<Vec<u32>>()),
        ],
        vec![],
    );
    let report = assemble(&request).unwrap();
    assert_eq!(report.status, "complete");
    assert!(report.completeness.accounting_complete);
    assert_eq!(
        report.segments.len(),
        1,
        "uninterrupted support is one segment"
    );
    let segment = &report.segments[0];
    assert_eq!(segment.start_sample, 0);
    assert_eq!(segment.end_sample, secs(14));
    assert!((segment.offset_seconds - (-2.96)).abs() < 1e-9);
    assert_eq!(segment.playback_rate, 1.0);
    assert_eq!(segment.supported_spans.len(), 1);
    assert!(segment.unknown_spans.is_empty());
    assert_eq!(report.recognized_windows.len(), 3);
    assert!(
        report
            .recognized_windows
            .iter()
            .all(|window| window.retained)
    );
    assert_eq!(report.unknown_ranges.len(), 1);
    assert_eq!(report.unknown_ranges[0].start_sample, secs(14));
    assert_eq!(report.unknown_ranges[0].end_sample, secs(30));
    assert_eq!(report.unknown_ranges[0].reason, REASON_NO_ACCEPTED_SUPPORT);
    assert_accounting(&report, range);
    assert_unknowns_have_no_movie_position(&report);
}

#[test]
fn two_different_seeks_give_separate_offsets_and_unknown_transitions() {
    let range = domain(0, secs(60));
    let request = assembly(
        clock(0, secs(60)),
        vec![epoch("e0", 0, secs(60))],
        range,
        vec![
            accepted_window(0, "e0", "w1", 0, -37, &(0..=6).collect::<Vec<u32>>()),
            accepted_window(0, "e0", "w2", 8, 50, &(9..=15).collect::<Vec<u32>>()),
            accepted_window(0, "e0", "w3", 17, 300, &(18..=24).collect::<Vec<u32>>()),
        ],
        vec![],
    );
    let report = assemble(&request).unwrap();
    assert_eq!(report.segments.len(), 3);
    let offsets: Vec<f64> = report
        .segments
        .iter()
        .map(|segment| segment.offset_seconds)
        .collect();
    assert!((offsets[0] - (-2.96)).abs() < 1e-9);
    assert!((offsets[1] - 4.0).abs() < 1e-9);
    assert!((offsets[2] - 24.0).abs() < 1e-9);
    // Seeks never bridge: both unsupported transitions stay explicit unknown
    // ranges between incompatible offsets.
    let transitions: Vec<&UnknownRange> = report
        .unknown_ranges
        .iter()
        .filter(|unknown| unknown.reason == REASON_INCOMPATIBLE_TRANSITION)
        .collect();
    assert_eq!(transitions.len(), 2);
    assert_eq!(transitions[0].start_sample, secs(7));
    assert_eq!(transitions[0].end_sample, secs(9));
    assert_eq!(transitions[1].start_sample, secs(16));
    assert_eq!(transitions[1].end_sample, secs(18));
    assert_accounting(&report, range);
    assert_unknowns_have_no_movie_position(&report);
}

#[test]
fn weak_and_negative_windows_between_agreeing_runs_remain_unknown_ranges() {
    let range = domain(0, secs(30));
    let weak = window(
        0,
        "e0",
        "w2",
        4,
        Some(-37),
        vec![cell(5, &frames(5), "w2b5"), cell(6, &frames(6), "w2b6")],
        true,
        Some(false),
        &[crate::source_audio_pooled::REASON_MINIMUM_ANCHORS],
    );
    let request = assembly(
        clock(0, secs(30)),
        vec![epoch("e0", 0, secs(30))],
        range,
        vec![
            accepted_window(0, "e0", "w1", 0, -37, &(0..=4).collect::<Vec<u32>>()),
            weak,
            accepted_window(0, "e0", "w3", 6, -37, &(7..=11).collect::<Vec<u32>>()),
        ],
        vec![],
    );
    let report = assemble(&request).unwrap();
    // Both sides agree and the gap is only two seconds, but the weak window is
    // contradictory evidence: the gap never becomes an in-envelope hole.
    assert_eq!(report.segments.len(), 2);
    assert_eq!(report.segments[0].end_sample, secs(5));
    assert_eq!(report.segments[1].start_sample, secs(7));
    assert!(
        report
            .segments
            .iter()
            .all(|segment| segment.unknown_spans.is_empty())
    );
    let unknown = report
        .unknown_ranges
        .iter()
        .find(|unknown| unknown.start_sample == secs(5) && unknown.end_sample == secs(7))
        .expect("explicit unknown range between agreeing runs");
    assert_eq!(unknown.reason, REASON_WEAK_OR_NEGATIVE_EVIDENCE);
    // The weak window's cells never count as support.
    assert!(!report.recognized_windows[1].retained);
    assert_eq!(report.completeness.accepted_window_count, 2);
    assert_accounting(&report, range);
    assert_unknowns_have_no_movie_position(&report);
}

#[test]
fn overlapping_windows_do_not_multiply_support_and_incompatible_windows_conflict() {
    let range = domain(0, secs(30));
    let conflicting = window(
        0,
        "e0",
        "w3",
        5,
        Some(50),
        vec![cell(7, &[900, 901], "w3b7")],
        true,
        Some(true),
        &[],
    );
    let request = assembly(
        clock(0, secs(30)),
        vec![epoch("e0", 0, secs(30))],
        range,
        vec![
            accepted_window(0, "e0", "w1", 0, -37, &(0..=9).collect::<Vec<u32>>()),
            accepted_window(0, "e0", "w2", 2, -37, &(2..=11).collect::<Vec<u32>>()),
            conflicting,
        ],
        vec![],
    );
    let report = assemble(&request).unwrap();
    // Bin 7 has incompatible accepted hypotheses -> conflicting/unknown, kept
    // as an alternative and never continuity-picked.
    assert_eq!(report.alternatives.len(), 1);
    let alternative = &report.alternatives[0];
    assert_eq!(alternative.time_bin_seconds, 7.0);
    assert_eq!(alternative.resolution, RESOLUTION_CONFLICTING_UNKNOWN);
    assert_eq!(alternative.hypotheses.len(), 2);
    assert_eq!(alternative.hypotheses[0].offset_bin, -37);
    assert_eq!(alternative.hypotheses[1].offset_bin, 50);
    let unknown = report
        .unknown_ranges
        .iter()
        .find(|unknown| unknown.start_sample == secs(7) && unknown.end_sample == secs(8))
        .expect("conflicting cell stays an unknown range");
    assert_eq!(unknown.reason, REASON_CONFLICTING_OFFSETS);
    assert_eq!(report.segments.len(), 2);
    assert_eq!(report.segments[0].end_sample, secs(7));
    assert_eq!(report.segments[1].start_sample, secs(8));
    // Overlapping windows share anchor identities: counts are deduplicated
    // unions, never sums of repeated votes.
    assert_eq!(
        report.segments[0]
            .deduplicated_support
            .independent_anchor_ids,
        21
    );
    assert_eq!(
        report.segments[1]
            .deduplicated_support
            .independent_anchor_ids,
        12
    );
    assert_eq!(
        report.segments[0].deduplicated_support.contributing_windows,
        2
    );
    assert_accounting(&report, range);
    assert_unknowns_have_no_movie_position(&report);
}

#[test]
fn two_second_gap_stays_inside_envelope_and_longer_gap_splits() {
    let range = domain(0, secs(40));
    let request = assembly(
        clock(0, secs(40)),
        vec![epoch("e0", 0, secs(40))],
        range,
        vec![
            accepted_window(0, "e0", "w1", 0, -37, &(0..=4).collect::<Vec<u32>>()),
            accepted_window(0, "e0", "w2", 6, -37, &(7..=11).collect::<Vec<u32>>()),
            accepted_window(0, "e0", "w3", 15, -37, &(16..=20).collect::<Vec<u32>>()),
        ],
        vec![],
    );
    let report = assemble(&request).unwrap();
    assert_eq!(report.segments.len(), 2);
    // The two-second gap (bins 5..6) is preserved inside the first envelope as
    // a recorded unknown span: no support, no movie position.
    let first = &report.segments[0];
    assert_eq!(first.start_sample, 0);
    assert_eq!(first.end_sample, secs(12));
    assert_eq!(first.supported_spans.len(), 2);
    assert_eq!(first.unknown_spans.len(), 1);
    assert_eq!(first.unknown_spans[0].start_sample, secs(5));
    assert_eq!(first.unknown_spans[0].end_sample, secs(7));
    assert_eq!(first.unknown_spans[0].reason, REASON_UNSUPPORTED_GAP);
    // The four-second gap (bins 12..15) splits the envelope.
    let second = &report.segments[1];
    assert_eq!(second.start_sample, secs(16));
    assert_eq!(second.end_sample, secs(21));
    assert!(second.unknown_spans.is_empty());
    let split = report
        .unknown_ranges
        .iter()
        .find(|unknown| unknown.start_sample == secs(12) && unknown.end_sample == secs(16))
        .expect("longer gap splits segments");
    assert_eq!(split.reason, REASON_NO_ACCEPTED_SUPPORT);
    assert_accounting(&report, range);
    assert_unknowns_have_no_movie_position(&report);
}

#[test]
fn pause_rewind_and_epoch_boundaries_prevent_interpolation() {
    let range = domain(0, secs(60));
    let request = assembly(
        clock(0, secs(60)),
        vec![epoch("e0", 0, secs(30)), epoch("e1", secs(30), secs(60))],
        range,
        vec![
            accepted_window(0, "e0", "w1", 0, -37, &(0..=9).collect::<Vec<u32>>()),
            accepted_window(0, "e0", "w2", 12, 50, &(12..=17).collect::<Vec<u32>>()),
            accepted_window(0, "e0", "w3", 20, -37, &(27..=29).collect::<Vec<u32>>()),
            accepted_window(secs(30), "e1", "w4", 1, -37, &(1..=3).collect::<Vec<u32>>()),
        ],
        vec![
            boundary("b-pause", secs(4) + 5_000, SyncBoundaryKind::Pause),
            boundary("b-rewind", secs(14) + 5_000, SyncBoundaryKind::Rewind),
        ],
    );
    let report = assemble(&request).unwrap();
    // The pause cell and the rewind cell stay unknown and split their runs.
    let pause = report
        .unknown_ranges
        .iter()
        .find(|unknown| unknown.start_sample == secs(4) && unknown.end_sample == secs(5))
        .expect("pause transition cell is unknown");
    assert_eq!(pause.reason, REASON_DECLARED_BOUNDARY);
    let rewind = report
        .unknown_ranges
        .iter()
        .find(|unknown| unknown.start_sample == secs(14) && unknown.end_sample == secs(15))
        .expect("rewind transition cell is unknown");
    assert_eq!(rewind.reason, REASON_DECLARED_BOUNDARY);
    // The epoch change prevents interpolation even though both sides carry the
    // SAME offset and only one second separates them.
    let epoch_split = report
        .unknown_ranges
        .iter()
        .find(|unknown| unknown.start_sample == secs(30) && unknown.end_sample == secs(31))
        .expect("epoch boundary stays unknown");
    assert_eq!(epoch_split.reason, REASON_NO_ACCEPTED_SUPPORT);
    let e0_tail = report
        .segments
        .iter()
        .find(|segment| segment.start_sample == secs(27))
        .expect("e0 tail segment");
    let e1_head = report
        .segments
        .iter()
        .find(|segment| segment.start_sample == secs(31))
        .expect("e1 head segment");
    assert_eq!(e0_tail.epoch_id, "e0");
    assert_eq!(e1_head.epoch_id, "e1");
    assert!((e0_tail.offset_seconds - e1_head.offset_seconds).abs() < 1e-9);
    assert!(
        report
            .segments
            .iter()
            .all(|segment| !(segment.start_sample < secs(30) && segment.end_sample > secs(30))),
        "no segment ever crosses a capture-epoch boundary"
    );
    assert_eq!(report.segments.len(), 6);
    assert_accounting(&report, range);
    assert_unknowns_have_no_movie_position(&report);
}

#[test]
fn chunk_edges_final_window_and_nonzero_native_origin_keep_offset_sign_and_pts() {
    // The epoch starts at a nonzero native sample origin; the requested span
    // [20, 40) recording-seconds-within-epoch at offset +100 maps to movie
    // [120, 140) s -> raw PTS [125000, 145000) at 1/1000 with origin 5000.
    let epoch_start = 130_000;
    let range = domain(epoch_start, epoch_start + secs(40));
    let mut movie_clock = movie();
    movie_clock.origin_pts = 5_000;
    // Cell bin 35 carries anchor frames straddling the 4096 feature-frame
    // chunk edge: absolute frame identity must survive chunking.
    let chunk_edge = window(
        epoch_start,
        "e0",
        "w4",
        30,
        Some(1250),
        (30..=39)
            .map(|bin| {
                let frame_list: Vec<u32> = if bin == 35 {
                    vec![4090, 4095, 4096, 4101]
                } else {
                    frames(bin)
                };
                cell(bin, &frame_list, &format!("w4b{bin}"))
            })
            .collect(),
        true,
        Some(true),
        &[],
    );
    let mut request = assembly(
        clock(epoch_start, epoch_start + secs(40)),
        vec![epoch("e0", epoch_start, epoch_start + secs(40))],
        range,
        vec![
            accepted_window(
                epoch_start,
                "e0",
                "w1",
                20,
                1250,
                &(20..=29).collect::<Vec<u32>>(),
            ),
            accepted_window(
                epoch_start,
                "e0",
                "w2",
                22,
                1250,
                &(22..=31).collect::<Vec<u32>>(),
            ),
            accepted_window(
                epoch_start,
                "e0",
                "w3",
                24,
                1250,
                &(24..=33).collect::<Vec<u32>>(),
            ),
            chunk_edge,
        ],
        vec![],
    );
    request.movie = movie_clock;
    let report = assemble(&request).unwrap();
    assert_eq!(report.segments.len(), 1);
    let segment = &report.segments[0];
    // The final scheduled window (ending at the epoch end) coalesces into the
    // same segment; native sample origins stay on the capture clock.
    assert_eq!(segment.start_sample, epoch_start + secs(20));
    assert_eq!(segment.end_sample, epoch_start + secs(40));
    assert!((segment.offset_seconds - 100.0).abs() < 1e-9);
    let span = &segment.supported_spans[0];
    // Offset sign: movieElapsed = recordingWithinEpoch + offset = 20 + 100.
    assert!((movie_elapsed_seconds(20.0, segment.offset_seconds) - 120.0).abs() < 1e-9);
    assert_eq!(span.movie_start_pts, 125_000);
    assert_eq!(span.movie_end_pts, 145_000);
    // Chunk-edge anchor frames keep their absolute feature-frame identities.
    let chunk_frames: Vec<u32> = segment
        .retained_support
        .iter()
        .find(|cell| cell.time_bin_seconds == 35.0)
        .expect("chunk-edge cell retained")
        .query_anchor_frames
        .clone();
    assert_eq!(chunk_frames, vec![4090, 4095, 4096, 4101]);
    assert_accounting(&report, range);
    assert_unknowns_have_no_movie_position(&report);
}

#[test]
fn recording_shorter_than_ten_seconds_yields_valid_all_unknown_result() {
    let range = domain(0, secs(8));
    let request = assembly(
        clock(0, secs(8)),
        vec![epoch("e0", 0, secs(8))],
        range,
        vec![],
        vec![],
    );
    let report = assemble(&request).unwrap();
    assert_eq!(report.status, "complete");
    assert_eq!(report.segments.len(), 0);
    assert_eq!(report.unknown_ranges.len(), 1);
    assert_eq!(report.unknown_ranges[0].start_sample, 0);
    assert_eq!(report.unknown_ranges[0].end_sample, secs(8));
    assert_eq!(report.unknown_ranges[0].reason, REASON_SHORT_EPOCH);
    assert_eq!(report.completeness.supported_seconds, 0.0);
    assert_eq!(report.completeness.unknown_seconds, 8.0);
    // Empty totals serialize as positive zero, never `-0.0`.
    let completeness = serde_json::to_string(&report.completeness).unwrap();
    assert!(
        completeness.contains("\"supportedSeconds\":0.0"),
        "{completeness}"
    );
    assert!(
        completeness.contains("\"envelopeUnknownSeconds\":0.0"),
        "{completeness}"
    );
    assert_accounting(&report, range);
    assert_unknowns_have_no_movie_position(&report);
}

#[test]
fn cancellation_or_budget_exhaustion_never_turns_incomplete_windows_into_matches() {
    let range = domain(0, secs(30));
    // Budget exhaustion: a truncated search claiming acceptance.
    let truncated = window(
        0,
        "e0",
        "w1",
        0,
        Some(-37),
        (0..=9)
            .map(|bin| cell(bin, &frames(bin), &format!("w1b{bin}")))
            .collect(),
        false,
        Some(true),
        &[],
    );
    // Cancelled run: incomplete and rejected.
    let cancelled = window(
        0,
        "e0",
        "w2",
        10,
        Some(-37),
        (10..=19)
            .map(|bin| cell(bin, &frames(bin), &format!("w2b{bin}")))
            .collect(),
        false,
        Some(false),
        &[REASON_INCOMPLETE_EVIDENCE],
    );
    // Abstained window without an accepted verdict.
    let abstained = window(
        0,
        "e0",
        "w3",
        20,
        Some(-37),
        (20..=29)
            .map(|bin| cell(bin, &frames(bin), &format!("w3b{bin}")))
            .collect(),
        true,
        None,
        &[crate::source_audio_pooled::REASON_NOT_CALIBRATED],
    );
    let request = assembly(
        clock(0, secs(30)),
        vec![epoch("e0", 0, secs(30))],
        range,
        vec![truncated, cancelled, abstained],
        vec![],
    );
    let report = assemble(&request).unwrap();
    assert_eq!(report.status, "incomplete");
    assert_eq!(report.segments.len(), 0);
    assert!(
        report
            .unknown_ranges
            .iter()
            .all(|unknown| unknown.reason != REASON_CONFLICTING_OFFSETS)
    );
    assert_eq!(report.unknown_ranges.len(), 1);
    assert_eq!(report.unknown_ranges[0].start_sample, 0);
    assert_eq!(report.unknown_ranges[0].end_sample, secs(30));
    assert_eq!(report.completeness.supported_seconds, 0.0);
    assert_eq!(report.completeness.incomplete_window_count, 2);
    // The truncated window never matches and is reported with the frozen
    // incomplete-evidence reason.
    let first = &report.recognized_windows[0];
    assert!(!first.retained);
    assert!(
        first
            .failure_reasons
            .iter()
            .any(|r| r == REASON_INCOMPLETE_EVIDENCE)
    );
    let value = serde_json::to_value(&report).unwrap();
    assert!(
        serde_json::to_string(&value)
            .unwrap()
            .contains("\"segments\":[]"),
        "no supported span may exist"
    );
    assert_accounting(&report, range);
    assert_unknowns_have_no_movie_position(&report);
}

// ---------------------------------------------------------------------------
// tv_review_sync adapter: actual support only, abstention on unknown/gaps.

fn mapping_request(query_start: u64, query_end: u64) -> QueryMappingRequest {
    QueryMappingRequest {
        session_id: "sess-1".into(),
        query: SyncQuery {
            clock_id: CLOCK_ID.into(),
            start_sample: query_start,
            end_sample: query_end,
        },
        policy: SyncPolicy::default(),
    }
}

#[test]
fn tv_review_sync_adapter_passes_only_actual_support_and_abstains_on_unknown_and_gaps() {
    let range = domain(0, secs(60));
    // Two runs at incompatible offsets with a two-second transition, plus one
    // recorded hole (bins 5..6) inside the first run's envelope.
    let request = assembly(
        clock(0, secs(60)),
        vec![epoch("e0", 0, secs(60))],
        range,
        vec![
            accepted_window(0, "e0", "w1", 0, -37, &(0..=4).collect::<Vec<u32>>()),
            accepted_window(0, "e0", "w2", 6, -37, &(7..=11).collect::<Vec<u32>>()),
            accepted_window(0, "e0", "w3", 8, 50, &(13..=17).collect::<Vec<u32>>()),
        ],
        vec![],
    );
    let report = assemble(&request).unwrap();
    assert_eq!(report.segments.len(), 2);
    assert_eq!(report.segments[0].unknown_spans.len(), 1);

    // 1. A query inside one supported span maps through the EXISTING query
    //    path using only the retained support positions (no synthesized
    //    anchors: anchor count equals the retained frames).
    let mapped = map_recording_query(&report, &mapping_request(secs(7), secs(12))).unwrap();
    assert_eq!(mapped.state, MappedQueryState::PartiallyMapped);
    let retained_frames: usize = report.segments[0]
        .retained_support
        .iter()
        .map(|cell| cell.query_anchor_frames.len())
        .sum();
    assert_eq!(mapped.support_anchor_ids.len(), retained_frames);
    assert!(
        mapped
            .slices
            .iter()
            .any(|slice| !slice.candidates.is_empty()),
        "the mapper maps interior sub-ranges"
    );
    // The mapper may return less coverage than the envelope; the result is
    // preserved without backfilling from the segment offset.
    assert!(
        mapped
            .slices
            .iter()
            .any(|slice| slice.candidates.is_empty()),
        "unbracketed envelope edges stay unmapped"
    );

    // 2. A gap-crossing query abstains with recording timestamps only.
    let crossing = map_recording_query(&report, &mapping_request(secs(4), secs(8))).unwrap();
    assert_eq!(crossing.state, MappedQueryState::GapCrossing);
    assert!(crossing.slices.is_empty());
    assert_eq!(crossing.query.start_sample, secs(4));
    assert_eq!(crossing.query.end_sample, secs(8));

    // 3. A query inside the recorded hole keeps recording timestamps and no
    //    movie location.
    let hole = map_recording_query(&report, &mapping_request(secs(5), secs(6))).unwrap();
    assert_eq!(hole.state, MappedQueryState::Unknown);
    assert!(hole.slices.is_empty());

    // 4. A query across the incompatible transition abstains.
    let transition = map_recording_query(&report, &mapping_request(secs(11), secs(14))).unwrap();
    assert_eq!(transition.state, MappedQueryState::GapCrossing);
    assert!(transition.slices.is_empty());

    // 5. Two independent anchors are never synthesized from one pooled mode:
    //    a single retained anchor position reaches the mapper alone and the
    //    query abstains instead of being bridged.
    let single = assembly(
        clock(0, secs(60)),
        vec![epoch("e0", 0, secs(60))],
        range,
        vec![window(
            0,
            "e0",
            "w4",
            0,
            Some(-37),
            vec![cell(2, &[2 * 87], "w4b2")],
            true,
            Some(true),
            &[],
        )],
        vec![],
    );
    let single_report = assemble(&single).unwrap();
    let single_query =
        map_recording_query(&single_report, &mapping_request(secs(2), secs(3))).unwrap();
    assert_eq!(single_query.support_anchor_ids.len(), 1);
    assert!(
        single_query
            .slices
            .iter()
            .all(|slice| slice.candidates.is_empty()),
        "one pooled mode never becomes two mapper anchors"
    );
    assert_eq!(single_query.state, MappedQueryState::Unknown);
}

// ---------------------------------------------------------------------------
// Pooled DTO contract: additive support evidence and round-trippable results.

#[test]
fn pooled_mode_support_is_opt_in_and_results_round_trip() {
    // The new field is omitted from serialized diagnostics unless requested,
    // keeping the demonstrated output byte-compatible when unset.
    let row = PooledOffsetModeRow {
        offset_bin: -37,
        offset_seconds: -2.96,
        independent_anchors: 3,
        summed_cell_anchor_count: 3,
        duplicate_anchor_votes_removed: 0,
        occupied_time_bins: 1,
        support_span_seconds: 1,
        rarity_weight: 3.0,
        frequency_diversity_median_per_time_bin: 4.0,
        saturated_anchors: 0,
        shared_query_anchors_with_best: 0,
        supporting_cells: None,
    };
    let json = serde_json::to_string(&row).unwrap();
    assert!(
        !json.contains("supportingCells"),
        "unset fields stay omitted: {json}"
    );
    let row = PooledOffsetModeRow {
        supporting_cells: Some(vec![PooledModeSupportCell {
            time_bin: 7,
            query_anchor_frames: vec![602, 612],
        }]),
        ..row
    };
    let json = serde_json::to_string(&row).unwrap();
    assert!(json.contains("\"supportingCells\""));
    assert!(json.contains("\"queryAnchorFrames\":[602,612]"));
    let round_tripped: PooledOffsetModeRow = serde_json::from_str(&json).unwrap();
    assert_eq!(round_tripped, row);

    // Frozen vocabulary and identity statics survive a persistence round trip.
    let acceptance = PooledWindowAcceptance {
        floors_calibrated: true,
        recipe_calibrated: true,
        accepted: Some(false),
        failure_reasons: vec![crate::source_audio_pooled::REASON_MINIMUM_ANCHORS],
        gate_failure_reasons: vec![crate::source_audio_pooled::REASON_MINIMUM_ANCHORS],
    };
    let json = serde_json::to_string(&acceptance).unwrap();
    let round_tripped: PooledWindowAcceptance = serde_json::from_str(&json).unwrap();
    assert_eq!(round_tripped, acceptance);
    assert!(
        serde_json::from_str::<PooledWindowAcceptance>(
            "{\"floorsCalibrated\":true,\"recipeCalibrated\":true,\"accepted\":null,\
             \"failureReasons\":[\"invented_reason\"],\"gateFailureReasons\":[]}"
        )
        .is_err(),
        "the frozen reason vocabulary is closed"
    );
}

#[test]
fn from_pooled_reads_best_mode_support_and_chunk_relative_cells() {
    let pooled = PooledWindowResult {
        start_seconds: 0,
        duration_seconds: 10,
        end_seconds: 10,
        complete: true,
        internal_offset_cell_count: 1,
        offset_cells_scanned: 1,
        offset_cells_pooled_for_window: 1,
        offset_cells_dropped: 0,
        ranked_offset_bin_count: 1,
        returned_offset_bin_count: 1,
        best_distinct_competitor: None,
        best_anchor_margin: None,
        best_unique_anchor_margin: None,
        modes: vec![PooledOffsetModeRow {
            offset_bin: -25,
            offset_seconds: -2.0,
            independent_anchors: 2,
            summed_cell_anchor_count: 2,
            duplicate_anchor_votes_removed: 0,
            occupied_time_bins: 1,
            support_span_seconds: 1,
            rarity_weight: 2.0,
            frequency_diversity_median_per_time_bin: 4.0,
            saturated_anchors: 0,
            shared_query_anchors_with_best: 0,
            supporting_cells: Some(vec![PooledModeSupportCell {
                time_bin: 3,
                query_anchor_frames: vec![258, 268],
            }]),
        }],
        acceptance: PooledWindowAcceptance {
            floors_calibrated: true,
            recipe_calibrated: true,
            accepted: Some(true),
            failure_reasons: vec![],
            gate_failure_reasons: vec![],
        },
    };
    // Epoch-chunk query: cells are already epoch-relative.
    let epoch_chunk = AcceptedWindow::from_pooled("w1", CLOCK_ID, "e0", 0, secs(10), None, &pooled);
    assert_eq!(epoch_chunk.window_start_seconds, 0.0);
    assert_eq!(epoch_chunk.supporting_cells[0].time_bin_seconds, 3.0);
    assert_eq!(epoch_chunk.offset_bin, Some(-25));
    // Fractional final window in its own chunk: explicit chunk start rebases
    // both the window and its chunk-relative cells into epoch coordinates.
    let fractional = AcceptedWindow::from_pooled(
        "w2",
        CLOCK_ID,
        "e0",
        5_512,
        5_512 + secs(10),
        Some(5_512.0 / f64::from(RATE)),
        &pooled,
    );
    assert!((fractional.window_start_seconds - 5_512.0 / f64::from(RATE)).abs() < 1e-12);
    assert!(
        (fractional.supporting_cells[0].time_bin_seconds - (5_512.0 / f64::from(RATE) + 3.0)).abs()
            < 1e-12
    );
    assert_eq!(fractional.accepted, Some(true));
    assert!(fractional.complete);
}

// ---------------------------------------------------------------------------
// Session-window bridging: assembler input for scheduled session results.

#[test]
fn rebased_final_window_cells_stay_inside_the_window_at_48_khz() {
    use crate::source_audio_sessions::{SessionWindowResult, SessionWindowState};

    // Release-comparison regression: a 69.391 s epoch at 48 kHz schedules a
    // final window at 59.391 s (native sample 2_850_768). Its extractor chunk
    // starts at floor(2_850_768 * 11025 / 48000) / 11025 = 59.39093 s, i.e.
    // three native samples early, so chunk bin 0 used to land outside the
    // window and the whole report was refused.
    const NATIVE: u32 = 48_000;
    let start_sample = 2_850_768_u64;
    let end_sample = start_sample + 10 * u64::from(NATIVE);
    let window_start_seconds = start_sample as f64 / f64::from(NATIVE);
    let chunk_start = (start_sample * u64::from(FEATURE_RATE) / u64::from(NATIVE)) as f64
        / f64::from(FEATURE_RATE);
    assert!(chunk_start < window_start_seconds);
    let cells = (0..10)
        .map(|bin| PooledModeSupportCell {
            time_bin: bin,
            query_anchor_frames: vec![bin * 86 + 1, bin * 86 + 9, bin * 86 + 17, bin * 86 + 25],
        })
        .collect();
    let pooled = PooledWindowResult {
        start_seconds: 0,
        duration_seconds: 10,
        end_seconds: 10,
        complete: true,
        internal_offset_cell_count: 40,
        offset_cells_scanned: 40,
        offset_cells_pooled_for_window: 40,
        offset_cells_dropped: 0,
        ranked_offset_bin_count: 1,
        returned_offset_bin_count: 1,
        best_distinct_competitor: None,
        best_anchor_margin: None,
        best_unique_anchor_margin: None,
        modes: vec![PooledOffsetModeRow {
            offset_bin: 10_356,
            offset_seconds: 828.48,
            independent_anchors: 40,
            summed_cell_anchor_count: 40,
            duplicate_anchor_votes_removed: 0,
            occupied_time_bins: 10,
            support_span_seconds: 10,
            rarity_weight: 40.0,
            frequency_diversity_median_per_time_bin: 4.0,
            saturated_anchors: 0,
            shared_query_anchors_with_best: 0,
            supporting_cells: Some(cells),
        }],
        acceptance: PooledWindowAcceptance {
            floors_calibrated: true,
            recipe_calibrated: true,
            accepted: Some(true),
            failure_reasons: vec![],
            gate_failure_reasons: vec![],
        },
    };
    let offset = 828.48 - chunk_start;
    let window = SessionWindowResult {
        capture_clock_id: CLOCK_ID.into(),
        epoch_id: "e0".into(),
        window_evidence_id: format!("saw:{CLOCK_ID}:e0:{start_sample}-{end_sample}"),
        start_sample,
        end_sample,
        window_start_seconds,
        duration_seconds: 10,
        state: SessionWindowState::Completed,
        incomplete_reason: None,
        offset_seconds: Some(offset),
        offset_bin: Some((offset / OFFSET_BIN_SECONDS).round() as i32),
        chunk_start_seconds_within_epoch: Some(chunk_start),
        acceptance: Some(pooled.acceptance.clone()),
        pooled: Some(pooled),
    };
    let bridged = AcceptedWindow::from_session_window(&window);
    for (bin, cell) in bridged.supporting_cells.iter().enumerate() {
        assert_eq!(
            (cell.time_bin_seconds * f64::from(NATIVE)).round() as u64,
            start_sample + bin as u64 * u64::from(NATIVE),
            "cell {bin} must sit on the window's own one-second grid"
        );
    }
    let mut capture_clock = clock(0, end_sample);
    capture_clock.sample_rate = NATIVE;
    let report = assemble(&assembly(
        capture_clock,
        vec![epoch("e0", 0, end_sample)],
        domain(0, end_sample),
        vec![bridged],
        vec![],
    ))
    .unwrap();
    assert_eq!(report.segments.len(), 1);
    assert_eq!(report.segments[0].start_sample, start_sample);
    assert_eq!(report.segments[0].end_sample, end_sample);
    assert_accounting(&report, domain(0, end_sample));
}

#[test]
fn from_session_window_rebases_offsets_and_never_supports_incomplete_windows() {
    use crate::source_audio_sessions::{SessionWindowResult, SessionWindowState};

    let pooled = PooledWindowResult {
        start_seconds: 0,
        duration_seconds: 10,
        end_seconds: 10,
        complete: true,
        internal_offset_cell_count: 1,
        offset_cells_scanned: 1,
        offset_cells_pooled_for_window: 1,
        offset_cells_dropped: 0,
        ranked_offset_bin_count: 1,
        returned_offset_bin_count: 1,
        best_distinct_competitor: None,
        best_anchor_margin: None,
        best_unique_anchor_margin: None,
        modes: vec![PooledOffsetModeRow {
            offset_bin: 25,
            offset_seconds: 2.0,
            independent_anchors: 2,
            summed_cell_anchor_count: 2,
            duplicate_anchor_votes_removed: 0,
            occupied_time_bins: 1,
            support_span_seconds: 1,
            rarity_weight: 2.0,
            frequency_diversity_median_per_time_bin: 4.0,
            saturated_anchors: 0,
            shared_query_anchors_with_best: 0,
            supporting_cells: Some(vec![PooledModeSupportCell {
                time_bin: 3,
                query_anchor_frames: vec![258, 268],
            }]),
        }],
        acceptance: PooledWindowAcceptance {
            floors_calibrated: true,
            recipe_calibrated: true,
            accepted: Some(true),
            failure_reasons: vec![],
            gate_failure_reasons: vec![],
        },
    };
    let rate = u64::from(RATE);
    let start_sample = (0.5 * f64::from(RATE)) as u64;
    let end_sample = start_sample + 10 * rate;
    let completed = SessionWindowResult {
        capture_clock_id: CLOCK_ID.into(),
        epoch_id: "e0".into(),
        window_evidence_id: format!("saw:{CLOCK_ID}:e0:{start_sample}-{end_sample}"),
        start_sample,
        end_sample,
        window_start_seconds: start_sample as f64 / f64::from(RATE),
        duration_seconds: 10,
        state: SessionWindowState::Completed,
        incomplete_reason: None,
        // Epoch convention after the own-chunk rebase: 2.0 - 0.5.
        offset_seconds: Some(1.5),
        offset_bin: Some(25),
        chunk_start_seconds_within_epoch: Some(0.5),
        acceptance: Some(pooled.acceptance.clone()),
        pooled: Some(pooled),
    };
    let bridged = AcceptedWindow::from_session_window(&completed);
    assert_eq!(
        bridged.window_id,
        format!("saw-{CLOCK_ID}-e0-{start_sample}-{end_sample}")
    );
    assert_eq!(bridged.offset_seconds, Some(1.5));
    // The hypothesis bin is the nearest 80 ms bin of the reported epoch
    // offset, never the raw chunk-local bin.
    assert_eq!(
        bridged.offset_bin,
        Some((1.5 / OFFSET_BIN_SECONDS).round() as i32)
    );
    assert!((bridged.window_start_seconds - completed.window_start_seconds).abs() < 1e-12);
    // Chunk bin 3 is the window's own fourth cell: it sits on the window grid
    // (start sample 5512 = 0.49995 s), not on the sub-sample chunk origin.
    assert!(
        (bridged.supporting_cells[0].time_bin_seconds - (completed.window_start_seconds + 3.0))
            .abs()
            < 1e-12
    );
    assert_eq!(
        bridged.supporting_cells[0].query_anchor_frames,
        vec![258, 268]
    );
    assert!(bridged.complete);
    assert_eq!(bridged.accepted, Some(true));

    // The documented rebased pair validates and assembles at the epoch offset.
    let report = assemble(&assembly(
        clock(0, secs(30)),
        vec![epoch("e0", 0, secs(30))],
        domain(0, secs(30)),
        vec![AcceptedWindow::from_session_window(&completed)],
        vec![],
    ))
    .unwrap();
    assert_eq!(report.segments.len(), 1);
    assert!((report.segments[0].offset_seconds - 1.5).abs() < 1e-9);

    // An incomplete (cancelled/truncated/budget) window is explicit evidence
    // without support: no offset, no cells, never accepted or retained.
    let incomplete = SessionWindowResult {
        state: SessionWindowState::Incomplete,
        incomplete_reason: Some("cancelled".into()),
        offset_seconds: None,
        offset_bin: None,
        chunk_start_seconds_within_epoch: None,
        acceptance: None,
        pooled: None,
        ..completed
    };
    let bridged = AcceptedWindow::from_session_window(&incomplete);
    assert!(!bridged.complete);
    assert_eq!(bridged.accepted, None);
    assert!(bridged.offset_seconds.is_none() && bridged.offset_bin.is_none());
    assert!(bridged.supporting_cells.is_empty());
    let report = assemble(&assembly(
        clock(0, secs(30)),
        vec![epoch("e0", 0, secs(30))],
        domain(0, secs(30)),
        vec![AcceptedWindow::from_session_window(&incomplete)],
        vec![],
    ))
    .unwrap();
    assert_eq!(report.segments.len(), 0);
    assert!(!report.recognized_windows[0].retained);
    assert!(
        report.recognized_windows[0]
            .failure_reasons
            .iter()
            .any(|reason| reason == REASON_INCOMPLETE_EVIDENCE)
    );
}

// ---------------------------------------------------------------------------
// Full-recording report entrypoint: the core-owned validation the CLI adapter
// only invokes (request shapes and digest pins are enforced here, not in the
// CLI).

/// A real small index over the committed synthetic movie for entrypoint
/// validation tests that must fail before any recognition work.
fn report_test_index() -> Index {
    let pcm = decode_wav(&Path::new(SYNTHETIC_ROOT).join("movie.wav"), &|| false).unwrap();
    Index::from_pcm(movie(), &pcm, Recipe::default(), &|| false).unwrap()
}

#[test]
fn report_rejects_domain_and_epoch_shapes_before_any_media_work() {
    let index = report_test_index();
    // The recording path is deliberately absent: structural validation must
    // fail before any media work touches the file system.
    let recording = Path::new("absent-recording.wav");
    let derived = Path::new("absent-derived");
    let valid = ReportRequest {
        recipe_identity: RECIPE_ID.into(),
        capture_clock: clock(0, secs(30)),
        epochs: vec![epoch("e0", 0, secs(30))],
        domain: domain(0, secs(30)),
        boundaries: vec![],
        movie_digest: None,
        recording_digest: None,
        pooled_mode_limit: None,
    };

    // A domain outside the capture clock is refused.
    let mut request = valid.clone();
    request.domain = domain(0, secs(40));
    let error = report(&index, recording, derived, &request, &mut |_| {}, &|| false).unwrap_err();
    assert!(
        format!("{error:#}").contains("domain must lie inside the capture clock"),
        "{error:#}"
    );

    // Unordered and outside-domain epochs are refused.
    let mut request = valid.clone();
    request.epochs = vec![epoch("e1", secs(5), secs(15)), epoch("e0", 0, secs(30))];
    let error = report(&index, recording, derived, &request, &mut |_| {}, &|| false).unwrap_err();
    assert!(
        format!("{error:#}").contains("epochs must be ordered and inside the requested domain"),
        "{error:#}"
    );
    let mut request = valid.clone();
    request.epochs = vec![epoch("e0", 0, secs(40))];
    let error = report(&index, recording, derived, &request, &mut |_| {}, &|| false).unwrap_err();
    assert!(
        format!("{error:#}").contains("epochs must be ordered and inside the requested domain"),
        "{error:#}"
    );

    // With valid shapes the pipeline proceeds to recording identity work.
    let error = report(&index, recording, derived, &valid, &mut |_| {}, &|| false).unwrap_err();
    assert!(
        format!("{error:#}").contains("stat file before hash"),
        "{error:#}"
    );
}

#[test]
fn report_rejects_a_recording_digest_mismatch_before_decoding() {
    let index = report_test_index();
    let recording = Path::new(SYNTHETIC_ROOT).join("recording-continuous.wav");
    let request = ReportRequest {
        recipe_identity: RECIPE_ID.into(),
        capture_clock: clock(0, secs(30)),
        epochs: vec![epoch("e0", 0, secs(30))],
        domain: domain(0, secs(30)),
        boundaries: vec![],
        movie_digest: None,
        recording_digest: Some("f".repeat(64)),
        pooled_mode_limit: None,
    };
    let error = report(
        &index,
        &recording,
        Path::new("absent-derived"),
        &request,
        &mut |_| {},
        &|| false,
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("recording digest mismatch"),
        "{error:#}"
    );
}

#[test]
fn report_rejects_a_recording_whose_native_rate_differs_from_the_capture_clock() {
    let index = report_test_index();
    let recording = Path::new(SYNTHETIC_ROOT).join("recording-continuous.wav");
    let mut capture_clock = clock(0, 30 * 48_000);
    capture_clock.sample_rate = 48_000;
    let request = ReportRequest {
        recipe_identity: RECIPE_ID.into(),
        capture_clock,
        epochs: vec![epoch("e0", 0, 15 * 48_000)],
        domain: domain(0, 15 * 48_000),
        boundaries: vec![],
        movie_digest: None,
        recording_digest: None,
        pooled_mode_limit: None,
    };
    let error = report(
        &index,
        &recording,
        Path::new("absent-derived"),
        &request,
        &mut |_| {},
        &|| false,
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains(
            "recording sample rate 11025 does not match the declared capture clock rate 48000"
        ),
        "{error:#}"
    );
}

#[test]
fn map_recording_query_rejects_unknown_report_schema_versions() {
    let current = assemble(&assembly(
        clock(0, secs(30)),
        vec![epoch("e0", 0, secs(30))],
        domain(0, secs(30)),
        vec![],
        vec![],
    ))
    .unwrap();
    let request = QueryMappingRequest {
        session_id: "queries".into(),
        query: SyncQuery {
            clock_id: CLOCK_ID.into(),
            start_sample: 0,
            end_sample: secs(1),
        },
        policy: SyncPolicy::default(),
    };
    // The known report schema is consumed...
    assert!(map_recording_query(&current, &request).is_ok());
    // ...but an unknown report schema version is rejected outright instead of
    // being consumed.
    let mut future = current.clone();
    future.schema_version = SEGMENT_REPORT_SCHEMA_VERSION + 1;
    let error = map_recording_query(&future, &request).unwrap_err();
    assert!(format!("{error:#}").contains("schema version"), "{error:#}");
}

// ---------------------------------------------------------------------------
// Integration leg: real generated audio through the extractor
// (source_audio_pooled::recognize), not fabricated input.

use crate::source_audio_pooled::{PooledOffsetWindow, PooledRecognitionRequest, recognize};
use crate::tv_soundtrack_landmarks::{Index, Recipe, decode_wav};
use sha2::{Digest, Sha256};
use std::path::Path;

const SYNTHETIC_ROOT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/source_audio_eval/synthetic"
);
const SYNTHETIC_MANIFEST: &str =
    include_str!("../tests/source_audio_eval/synthetic/synthetic-ci-v1.json");
const OFFSET_CONVENTION_TARGET: &str =
    "movieElapsedSeconds = recordingSecondsWithinEpoch + offsetSeconds";
const EXPECTED_CONTINUOUS_OFFSET: f64 = -3.0;

fn sha256_file(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap();
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    format!("{:x}", hasher.finalize())
}

/// Recognize `recording` through the extractor and assemble the report from
/// the pooled window results exactly as the session executor would.
fn recognize_and_assemble(
    index: &Index,
    movie_clock: &SyncMovieClock,
    recording: &str,
    capture_clock_id: &str,
    windows: &[(u32, u16)],
) -> SegmentReport {
    let root = Path::new(SYNTHETIC_ROOT);
    let pcm = decode_wav(&root.join(recording), &|| false).unwrap();
    let sample_count = pcm.len() as u64;
    let pooled = recognize(
        index,
        &pcm,
        &PooledRecognitionRequest {
            search_policy: None,
            windows: Some(
                windows
                    .iter()
                    .map(|(start_seconds, duration_seconds)| PooledOffsetWindow {
                        start_seconds: *start_seconds,
                        duration_seconds: *duration_seconds,
                    })
                    .collect(),
            ),
            mode_limit: Some(8),
            include_mode_support: Some(true),
        },
        &|| false,
    )
    .unwrap();
    let evidence = pooled.evidence.expect("windows requested");
    let windows: Vec<AcceptedWindow> = evidence
        .windows
        .iter()
        .enumerate()
        .map(|(ordinal, window)| {
            let start_sample = u64::from(window.start_seconds) * u64::from(RATE);
            AcceptedWindow::from_pooled(
                format!("w{ordinal}"),
                capture_clock_id,
                "e0",
                start_sample,
                start_sample + u64::from(window.duration_seconds) * u64::from(RATE),
                None,
                window,
            )
        })
        .collect();
    let request = assembly(
        clock(0, sample_count),
        vec![epoch("e0", 0, sample_count)],
        domain(0, sample_count),
        windows,
        vec![],
    );
    let mut request = request;
    request.capture_clock.clock_id = capture_clock_id.to_string();
    request.movie = movie_clock.clone();
    request.recording_identity = Some(recording.to_string());
    assemble(&request).unwrap()
}

#[test]
fn extractor_output_coalesces_continuous_playback_and_separates_seeks() {
    let manifest: Value = serde_json::from_str(SYNTHETIC_MANIFEST).unwrap();
    assert_eq!(manifest["offsetConvention"], OFFSET_CONVENTION_TARGET);
    let root = Path::new(SYNTHETIC_ROOT);
    // Fixture identities are locked to the committed bytes.
    assert_eq!(
        sha256_file(&root.join("recording-seek.wav")),
        manifest["integrationCases"][0]["recording"]["sha256"]
            .as_str()
            .unwrap()
    );
    let movie = &manifest["movie"];
    let movie_clock = SyncMovieClock {
        movie_digest: movie["sha256"].as_str().unwrap().to_string(),
        time_base_numerator: movie["clock"]["timeBaseNumerator"].as_u64().unwrap() as u32,
        time_base_denominator: movie["clock"]["timeBaseDenominator"].as_u64().unwrap() as u32,
        origin_pts: movie["clock"]["originPts"].as_i64().unwrap(),
        duration_pts: movie["clock"]["durationPts"].as_u64().unwrap(),
    };
    let movie_pcm = decode_wav(&root.join("movie.wav"), &|| false).unwrap();
    let index = Index::from_pcm(movie_clock.clone(), &movie_pcm, Recipe::default(), &|| {
        false
    })
    .unwrap();
    let ticks = 11_025.0;

    // 1. Continuous playback on the committed continuous fixture coalesces
    //    multiple accepted windows into one segment at the known delay -3.0.
    let continuous_windows: Vec<(u32, u16)> =
        manifest["cases"][0]["request"]["pooledOffsetWindows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|window| {
                (
                    window["startSeconds"].as_u64().unwrap() as u32,
                    window["durationSeconds"].as_u64().unwrap() as u16,
                )
            })
            .collect();
    assert_eq!(continuous_windows.len(), 2);
    let continuous = recognize_and_assemble(
        &index,
        &movie_clock,
        "recording-continuous.wav",
        "synthetic-ci-v1-continuous",
        &continuous_windows,
    );
    assert_eq!(
        continuous.segments.len(),
        1,
        "continuous playback coalesces"
    );
    assert!(
        continuous
            .recognized_windows
            .iter()
            .all(|window| window.retained)
    );
    let segment = &continuous.segments[0];
    assert!(
        (segment.offset_seconds - EXPECTED_CONTINUOUS_OFFSET).abs() <= 0.1,
        "extractor offset {} must match the known delay",
        segment.offset_seconds
    );
    // Raw movie PTS conversion follows the frozen convention on extractor
    // output: movieElapsed = recordingWithinEpoch + offsetSeconds.
    let span = &segment.supported_spans[0];
    let epoch = &continuous.epochs[0];
    let expected_start_pts =
        (span.start_sample - epoch.start_sample) as f64 / 11_025.0 + segment.offset_seconds;
    assert!((span.movie_start_pts as f64 - expected_start_pts * ticks).abs() <= 1.0);
    assert_accounting(&continuous, domain(0, secs(15)));
    assert_unknowns_have_no_movie_position(&continuous);

    // 2. The seek fixture (two distinct known delays) separates into two
    //    segments with an unknown transition: the seek never bridges.
    let seek = &manifest["integrationCases"][0];
    let seek_windows: Vec<(u32, u16)> = seek["pooledOffsetWindows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|window| {
            (
                window["startSeconds"].as_u64().unwrap() as u32,
                window["durationSeconds"].as_u64().unwrap() as u16,
            )
        })
        .collect();
    let seek_report = recognize_and_assemble(
        &index,
        &movie_clock,
        "recording-seek.wav",
        "synthetic-ci-v1-seek",
        &seek_windows,
    );
    assert_eq!(seek_report.segments.len(), 2, "the seek separates the runs");
    assert!(
        seek_report
            .recognized_windows
            .iter()
            .all(|window| window.retained)
    );
    let expected: Vec<f64> = seek["expected"]["windows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|window| window["expectedOffsetSeconds"].as_f64().unwrap())
        .collect();
    for (segment, expected_offsets) in [
        (&seek_report.segments[0], &expected[0..2]),
        (&seek_report.segments[1], &expected[2..4]),
    ] {
        assert!(
            expected_offsets
                .iter()
                .all(|offset| (segment.offset_seconds - offset).abs() <= 0.1),
            "segment offset {} must match the known delay",
            segment.offset_seconds
        );
    }
    let first = seek["expected"]["segments"][0]["supportedSecondsWithinEpoch"]
        .as_array()
        .unwrap();
    let second = seek["expected"]["segments"][1]["supportedSecondsWithinEpoch"]
        .as_array()
        .unwrap();
    assert_eq!(
        seek_report.segments[0].start_sample,
        secs(first[0].as_u64().unwrap())
    );
    assert_eq!(
        seek_report.segments[0].end_sample,
        secs(first[1].as_u64().unwrap())
    );
    assert_eq!(
        seek_report.segments[1].start_sample,
        secs(second[0].as_u64().unwrap())
    );
    assert_eq!(
        seek_report.segments[1].end_sample,
        secs(second[1].as_u64().unwrap())
    );
    // The transition stays an explicit unknown range between incompatible
    // offsets and never receives a movie position.
    let transition = seek["expected"]["unknownTransitionSecondsWithinEpoch"]
        .as_array()
        .unwrap();
    let unknown = seek_report
        .unknown_ranges
        .iter()
        .find(|unknown| {
            unknown.start_sample == secs(transition[0].as_u64().unwrap())
                && unknown.end_sample == secs(transition[1].as_u64().unwrap())
        })
        .expect("seek transition stays unknown");
    assert_eq!(unknown.reason, REASON_INCOMPATIBLE_TRANSITION);
    assert_accounting(&seek_report, domain(0, secs(28)));
    assert_unknowns_have_no_movie_position(&seek_report);
}

/// The committed seek fixture must be reproducible byte-identically from the
/// fixed-seed generator (skipped only when node is unavailable).
#[test]
fn seek_fixture_is_deterministic_from_the_fixed_seed_generator() {
    let root = Path::new(SYNTHETIC_ROOT);
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping seek fixture determinism check: node unavailable");
        return;
    }
    let out = tempfile::tempdir().unwrap();
    let output = std::process::Command::new("node")
        .arg(root.join("generate-synthetic-fixtures.mjs"))
        .arg("--out")
        .arg(out.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "generator failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for name in ["recording-seek.wav", "synthetic-ci-v1.json"] {
        assert_eq!(
            std::fs::read(out.path().join(name)).unwrap(),
            std::fs::read(root.join(name)).unwrap(),
            "{name} regeneration differs from committed bytes"
        );
    }
}
