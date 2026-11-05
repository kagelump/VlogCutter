//! Bounded session scheduling, execution and resumable derived results:
//! schedule geometry, native-origin and offset-sign correctness, batch caps
//! and scan-budget enforcement, truncation refusal, cancellation/resume,
//! identity-keyed reuse with staleness refusal, overlap-versus-coverage
//! accounting, and one end-to-end schedule->recognize leg on the generated
//! synthetic audio under `tests/source_audio_eval/synthetic`.

use super::*;
use crate::source_audio_pooled::pool_exact_offset_windows;
use crate::tv_review_sync::SyncMovieClock;
use crate::tv_soundtrack_landmarks::{OffsetEvidence, Recipe, Search, decode_wav};
use std::sync::atomic::{AtomicBool, Ordering};
use tempfile::tempdir;

fn evidence_with_frames(time_bin: u32, offset_bin: i32, frames: Vec<u32>) -> OffsetEvidence {
    OffsetEvidence {
        time_bin,
        offset_bin,
        offset_seconds_from_capture_start: f64::from(offset_bin)
            * source_audio_pooled::OFFSET_BIN_SECONDS,
        independent_anchors: frames.len() as u32,
        supporting_seconds: 1,
        frequency_diversity: 4,
        rarity_weight: frames.len() as f64,
        saturated_anchors: 0,
        query_anchor_frames: frames,
    }
}

fn fabricated_search(query_seconds: u64, offsets: Vec<OffsetEvidence>) -> Search {
    Search {
        schema_version: 1,
        recipe: Recipe::default(),
        search_policy: SearchPolicy::default(),
        movie_digest: "movie".into(),
        index_digest: "index".into(),
        query_pcm_digest: "query".into(),
        query_sample_count: query_seconds * u64::from(RATE),
        query_landmark_count: 0,
        matched_landmark_count: 0,
        candidate_pair_count: 0,
        complete: true,
        offsets,
    }
}

/// Evidence that passes every 10-second v3 gate: 50 independent anchors over
/// five one-second cells in `best_bin` against a small distant competitor.
fn accepting_search(best_bin: i32) -> Search {
    let mut cells = Vec::new();
    for time_bin in 0..5u32 {
        cells.push(evidence_with_frames(
            time_bin,
            best_bin,
            (0..10u32).map(|i| time_bin * 100 + i).collect(),
        ));
        cells.push(evidence_with_frames(
            time_bin,
            best_bin + 3,
            vec![900 + time_bin],
        ));
    }
    fabricated_search(10, cells)
}

fn pool_window(search: &Search, start_seconds: u32) -> PooledWindowResult {
    let request = PooledRecognitionRequest {
        search_policy: None,
        windows: Some(vec![PooledOffsetWindow {
            start_seconds,
            duration_seconds: 10,
        }]),
        mode_limit: None,
        include_mode_support: Some(true),
    };
    pool_exact_offset_windows(search, &request)
        .unwrap()
        .windows
        .into_iter()
        .next()
        .unwrap()
}

fn identity() -> SessionIdentity {
    SessionIdentity {
        recording_digest: "recording-digest".into(),
        index_identity: "index-identity".into(),
        recipe_identity: RECIPE_ID.to_string(),
        policy_identity: PooledRecognitionPolicy::frozen().policy_id.clone(),
        scheduling_identity: SchedulingIdentity {
            window_seconds: SCHEDULE_WINDOW_SECONDS,
            step_seconds: SCHEDULE_STEP_SECONDS,
            epochs: vec![CaptureEpoch {
                epoch_id: "e1".into(),
                start_sample: 0,
                end_sample: 15 * u64::from(RATE),
            }],
        },
        capture_clock: CaptureClock {
            clock_id: "capture-clock".into(),
            sample_rate: RATE,
            first_sample: 0,
            last_sample: 3_600 * u64::from(RATE),
            monotonic_origin_ns: 0,
            monotonic_tolerance_ns: 1_000,
        },
    }
}

/// `identity()` bound to the real index identity of `index`, as run_session
/// validates.
fn identity_for(index: &Index) -> SessionIdentity {
    let mut identity = identity();
    identity.index_identity = index.index_digest.clone();
    identity
}

fn request_with<'a>(
    identity: SessionIdentity,
    pcm: &'a [i16],
    pcm_origin_sample: u64,
    derived_data_dir: &Path,
) -> SessionRunRequest<'a> {
    SessionRunRequest {
        identity,
        pcm,
        pcm_origin_sample,
        derived_data_dir: derived_data_dir.to_path_buf(),
        search_policy: None,
        mode_limit: None,
    }
}

fn silence_index() -> Index {
    let silence = vec![0i16; RATE as usize * 10];
    Index::from_pcm(
        SyncMovieClock {
            movie_digest: "a".repeat(64),
            time_base_numerator: 1,
            time_base_denominator: 11025,
            origin_pts: 0,
            duration_pts: 110_250,
        },
        &silence,
        Recipe::default(),
        &|| false,
    )
    .unwrap()
}

fn epoch_starts(schedule: &SessionSchedule, epoch_id: &str) -> Vec<f64> {
    schedule
        .windows
        .iter()
        .filter(|window| window.epoch_id == epoch_id)
        .map(|window| window.window_start_seconds)
        .collect()
}

#[test]
fn schedule_regular_grid_final_window_and_exact_dedup() {
    let rate = u64::from(RATE);
    let mut identity = identity();
    identity.scheduling_identity.epochs = vec![
        CaptureEpoch {
            epoch_id: "dedup".into(),
            start_sample: 0,
            end_sample: 22 * rate,
        },
        CaptureEpoch {
            epoch_id: "added".into(),
            start_sample: 22 * rate,
            end_sample: 43 * rate,
        },
        CaptureEpoch {
            epoch_id: "odd".into(),
            start_sample: 43 * rate,
            end_sample: 58 * rate,
        },
        CaptureEpoch {
            epoch_id: "edge".into(),
            start_sample: 58 * rate,
            end_sample: 68 * rate,
        },
    ];
    let schedule = build_schedule(&identity).unwrap();
    // 22 s: the regular grid reaches the epoch end (last start 12); the
    // identical final window is deduplicated.
    assert_eq!(
        epoch_starts(&schedule, "dedup"),
        vec![0.0, 2.0, 4.0, 6.0, 8.0, 10.0, 12.0]
    );
    // 21 s: the grid stops at 10; the final full window ending at the epoch
    // end is added at 11.
    assert_eq!(
        epoch_starts(&schedule, "added"),
        vec![0.0, 2.0, 4.0, 6.0, 8.0, 10.0, 11.0]
    );
    // 15 s: grid 0, 2, 4 plus the final window at 5.
    assert_eq!(epoch_starts(&schedule, "odd"), vec![0.0, 2.0, 4.0, 5.0]);
    // 10 s edge: exactly one window (the final window deduplicates onto it).
    assert_eq!(epoch_starts(&schedule, "edge"), vec![0.0]);
    assert!(schedule.short_epochs.is_empty());

    // Native sample origins are exact at the original capture rate, computed
    // per epoch from that epoch's own recording-time origin.
    for window in &schedule.windows {
        let epoch = identity
            .scheduling_identity
            .epochs
            .iter()
            .find(|epoch| epoch.epoch_id == window.epoch_id)
            .unwrap();
        assert_eq!(
            window.start_sample - epoch.start_sample,
            (window.window_start_seconds * rate as f64) as u64
        );
        assert_eq!(window.end_sample, window.start_sample + 10 * rate);
        assert!(window.end_sample <= epoch.end_sample);
        assert_eq!(window.duration_seconds, 10);
    }
    // The added final window ends exactly at the epoch end.
    let last = schedule
        .windows
        .iter()
        .rfind(|window| window.epoch_id == "added")
        .unwrap();
    assert_eq!(last.start_sample, 22 * rate + 11 * rate);
    assert_eq!(last.end_sample, 43 * rate);
    // Each window carries its own deterministic evidence id.
    let ids: BTreeSet<&str> = schedule
        .windows
        .iter()
        .map(|window| window.window_evidence_id.as_str())
        .collect();
    assert_eq!(ids.len(), schedule.windows.len());
}

#[test]
fn short_epochs_stay_unknown_and_surface_in_the_session_result() {
    let rate = u64::from(RATE);
    let mut identity = identity();
    identity.scheduling_identity.epochs = vec![CaptureEpoch {
        epoch_id: "short".into(),
        start_sample: 0,
        end_sample: 9 * rate,
    }];
    let schedule = build_schedule(&identity).unwrap();
    assert!(schedule.windows.is_empty());
    assert_eq!(schedule.short_epochs.len(), 1);
    assert_eq!(schedule.short_epochs[0].reason, SHORT_EPOCH_REASON);
    assert_eq!(schedule.short_epochs[0].duration_seconds, 9.0);

    let index = silence_index();
    identity.index_identity = index.index_digest.clone();
    let pcm = vec![0i16; 9 * RATE as usize];
    let dir = tempdir().unwrap();
    let result = run_session(
        &index,
        &request_with(identity, &pcm, 0, dir.path()),
        &mut |_progress| {},
        &|| false,
    )
    .unwrap();
    // The short epoch stays unknown and is surfaced explicitly.
    assert_eq!(result.status, "complete");
    assert!(result.windows.is_empty());
    assert_eq!(result.schedule.short_epochs.len(), 1);
    assert_eq!(result.progress.total_windows, 0);
    assert_eq!(result.window_accounting.window_count, 0);
}

#[test]
fn offset_sign_follows_the_frozen_design_example() {
    let rate = u64::from(RATE);
    let mut identity = identity();
    identity.scheduling_identity.epochs = vec![CaptureEpoch {
        epoch_id: "e".into(),
        start_sample: 0,
        end_sample: 40 * rate,
    }];
    let schedule = build_schedule(&identity).unwrap();
    let window = schedule
        .windows
        .iter()
        .find(|window| window.window_start_seconds == 20.0)
        .unwrap();
    // Recording [20, 30) within the epoch with a +100 s pooled mode maps to
    // movie [120, 130):
    // movieElapsedSeconds = recordingSecondsWithinEpoch + offsetSeconds.
    let mut cells = Vec::new();
    for time_bin in 20..25u32 {
        cells.push(evidence_with_frames(
            time_bin,
            1250,
            (0..10u32).map(|i| time_bin * 100 + i).collect(),
        ));
    }
    let pooled = pool_window(&fabricated_search(40, cells), 20);
    let result = session_window_result(window, 0.0, pooled, true);
    let offset = result.offset_seconds.unwrap();
    assert!((offset - 100.0).abs() < 1e-9);
    assert!((window.window_start_seconds + offset - 120.0).abs() < 1e-9);
    assert!(
        (window.window_start_seconds + f64::from(window.duration_seconds) + offset - 130.0).abs()
            < 1e-9
    );
    assert_eq!(result.chunk_start_seconds_within_epoch, Some(0.0));
}

#[test]
fn offset_sign_holds_through_own_chunks_and_nonzero_epoch_origins() {
    // A non-integral epoch duration puts the final window at a fractional
    // start; it runs on its own chunk starting at the window itself, and the
    // chunk-local pooled offset must be rebased to the epoch convention.
    let native_rate = 22_050u32;
    let origin = 3 * u64::from(native_rate) + 1;
    let mut identity = identity();
    identity.capture_clock.sample_rate = native_rate;
    identity.scheduling_identity.epochs = vec![CaptureEpoch {
        epoch_id: "e".into(),
        start_sample: origin,
        end_sample: origin + 231_525, // 10.5 s at 22050 Hz
    }];
    let schedule = build_schedule(&identity).unwrap();
    assert_eq!(epoch_starts(&schedule, "e"), vec![0.0, 0.5]);
    // Nonzero native sample origins keep exact native ranges.
    assert_eq!(schedule.windows[0].start_sample, origin);
    assert_eq!(schedule.windows[1].start_sample, origin + 11_025);
    assert_eq!(schedule.windows[1].end_sample, origin + 231_525);

    // Own-chunk case: the pooled window is chunk-local at start 0 with a
    // +2.0 s mode; the epoch offset is 2.0 - 0.5 = 1.5 and the movie span is
    // [0.5 + 1.5, +10) = [2.0, 12.0).
    let pooled = pool_window(&accepting_search(25), 0);
    let result = session_window_result(&schedule.windows[1], 0.5, pooled, true);
    let offset = result.offset_seconds.unwrap();
    assert!((offset - 1.5).abs() < 1e-9);
    assert!((schedule.windows[1].window_start_seconds + offset - 2.0).abs() < 1e-9);
    assert_eq!(result.chunk_start_seconds_within_epoch, Some(0.5));
    // The epoch-chunk case rebases by exactly zero.
    let pooled = pool_window(&accepting_search(25), 0);
    let result = session_window_result(&schedule.windows[0], 0.0, pooled, true);
    assert!((result.offset_seconds.unwrap() - 2.0).abs() < 1e-9);
    assert_eq!(result.chunk_start_seconds_within_epoch, Some(0.0));
}

#[test]
fn resampling_to_the_extractor_rate_never_shifts_native_origins() {
    let native_rate = 22_050u32;
    let origin = 3 * u64::from(native_rate) + 1;
    let mut identity = identity();
    identity.capture_clock.sample_rate = native_rate;
    identity.scheduling_identity.epochs = vec![CaptureEpoch {
        epoch_id: "e".into(),
        start_sample: origin,
        end_sample: origin + 12 * u64::from(native_rate),
    }];
    let index = silence_index();
    identity.index_identity = index.index_digest.clone();
    // The decoded extractor-rate PCM starts 500 native samples before the
    // epoch origin; the extractor grid cannot represent the odd origin.
    let pcm_origin_sample = origin - 500;
    let pcm = vec![0i16; 132_550];
    let dir = tempdir().unwrap();
    let result = run_session(
        &index,
        &request_with(identity, &pcm, pcm_origin_sample, dir.path()),
        &mut |_progress| {},
        &|| false,
    )
    .unwrap();
    assert_eq!(result.status, "complete");
    let starts: Vec<u64> = result
        .windows
        .iter()
        .map(|window| window.start_sample)
        .collect();
    assert_eq!(starts, vec![origin, origin + 2 * u64::from(native_rate)]);
    for window in &result.windows {
        assert_eq!(
            window.end_sample,
            window.start_sample + 10 * u64::from(native_rate)
        );
    }
    assert_eq!(result.windows[1].window_start_seconds, 2.0);
}

#[test]
fn batch_planner_enforces_the_window_and_scan_caps() {
    // The safe first batch goes before the chunk's cell count is known.
    assert_eq!(planned_batch_size(300, None), SAFE_FIRST_BATCH);
    assert_eq!(planned_batch_size(10, None), 10);
    // Later batches respect both caps exactly.
    assert_eq!(planned_batch_size(300, Some(1_000)), MAX_WINDOWS_PER_CALL);
    assert_eq!(planned_batch_size(300, Some(250_000)), 200);
    assert_eq!(planned_batch_size(300, Some(10_000_000)), 5);
    assert_eq!(planned_batch_size(300, Some(50_000_000)), 1);
    assert_eq!(planned_batch_size(5, Some(0)), 5);
    // A call can never fit even one window: report infeasible, never pool.
    assert_eq!(planned_batch_size(1, Some(50_000_001)), 0);
    for cells in [
        1usize, 3, 12_345, 195_312, 195_313, 2_000_000, 49_999_999, 50_000_000, 60_000_000,
    ] {
        let size = max_windows_per_call(cells);
        assert!(size <= MAX_WINDOWS_PER_CALL);
        if size > 0 {
            assert!(size * cells <= MAX_CELL_SCANS_PER_CALL);
        }
    }
    // Even a maximal completed search (the searcher's 2,000,000-cell vote
    // cap) keeps the safe first batch within the scan budget.
    const {
        assert!(SAFE_FIRST_BATCH * 2_000_000 <= MAX_CELL_SCANS_PER_CALL);
    };
}

#[test]
fn execution_batches_windows_and_publishes_completed_counts() {
    let mut identity = identity();
    identity.scheduling_identity.epochs = vec![CaptureEpoch {
        epoch_id: "e".into(),
        start_sample: 0,
        end_sample: 60 * u64::from(RATE),
    }];
    let index = silence_index();
    identity.index_identity = index.index_digest.clone();
    let pcm = vec![0i16; 60 * RATE as usize + 128];
    let dir = tempdir().unwrap();
    let mut progress: Vec<SessionProgress> = Vec::new();
    let result = run_session(
        &index,
        &request_with(identity, &pcm, 0, dir.path()),
        &mut |update| progress.push(update),
        &|| false,
    )
    .unwrap();
    // 26 scheduled windows (grid 0..=50 step 2, final window deduplicated):
    // the first pooled call takes the safe batch of 25 and the observed
    // zero-cell count lets the remaining window fit in one call.
    assert_eq!(result.schedule.windows.len(), 26);
    let counts: Vec<usize> = progress
        .iter()
        .map(|update| update.completed_windows)
        .collect();
    assert_eq!(counts, vec![0, 25, 26]);
    assert_eq!(result.status, "complete");
    assert_eq!(result.reused_window_count, 0);
    assert_eq!(result.computed_window_count, 26);
    assert!(
        result
            .windows
            .iter()
            .all(|window| window.state == SessionWindowState::Completed)
    );
}

fn assert_truncated(result: &SessionWindowResult) {
    assert_eq!(result.state, SessionWindowState::Incomplete);
    assert_eq!(
        result.incomplete_reason.as_deref(),
        Some(REASON_TRUNCATED_SEARCH)
    );
    assert!(result.offset_seconds.is_none());
    assert!(result.offset_bin.is_none());
    assert!(result.acceptance.is_none());
    assert!(result.pooled.is_none());
    assert!(result.chunk_start_seconds_within_epoch.is_none());
    assert!(!result.is_accepted());
}

#[test]
fn truncated_searches_never_become_matches() {
    let rate = u64::from(RATE);
    let mut identity = identity();
    identity.scheduling_identity.epochs = vec![CaptureEpoch {
        epoch_id: "e".into(),
        start_sample: 0,
        end_sample: 10 * rate,
    }];
    let schedule = build_schedule(&identity).unwrap();
    let window = &schedule.windows[0];

    // Sanity: complete evidence with a passing verdict is accepted.
    let pooled = pool_window(&accepting_search(100), 0);
    assert_eq!(pooled.acceptance.accepted, Some(true));
    let result = session_window_result(window, 0.0, pooled, true);
    assert!(result.is_accepted());

    // Incomplete evidence accumulation is returned explicitly and never
    // accepted, even when the pooled verdict claims acceptance.
    let mut pooled = pool_window(&accepting_search(100), 0);
    pooled.acceptance.accepted = Some(true);
    let result = session_window_result(window, 0.0, pooled, false);
    assert_truncated(&result);

    // A truncated window row is never accepted either.
    let mut pooled = pool_window(&accepting_search(100), 0);
    pooled.complete = false;
    pooled.acceptance.accepted = Some(true);
    let result = session_window_result(window, 0.0, pooled, true);
    assert_truncated(&result);
}

#[test]
fn cancellation_leaves_resumable_state_and_resume_continues_without_redoing() {
    let mut identity = identity();
    identity.scheduling_identity.epochs = vec![CaptureEpoch {
        epoch_id: "e".into(),
        start_sample: 0,
        end_sample: 60 * u64::from(RATE),
    }];
    let index = silence_index();
    identity.index_identity = index.index_digest.clone();
    let pcm = vec![0i16; 60 * RATE as usize + 128];
    let dir = tempdir().unwrap();

    // Cancel after the first batch completes (25 of 26 windows).
    let cancel_flag = AtomicBool::new(false);
    let first = run_session(
        &index,
        &request_with(identity.clone(), &pcm, 0, dir.path()),
        &mut |update| {
            if update.completed_windows == 25 {
                cancel_flag.store(true, Ordering::SeqCst);
            }
        },
        &|| cancel_flag.load(Ordering::SeqCst),
    )
    .unwrap();
    assert!(first.cancelled);
    assert_eq!(first.status, "cancelled");
    assert_eq!(first.progress.completed_windows, 25);
    assert_eq!(first.computed_window_count, 25);
    let incomplete: Vec<&SessionWindowResult> = first
        .windows
        .iter()
        .filter(|window| window.state == SessionWindowState::Incomplete)
        .collect();
    assert_eq!(incomplete.len(), 1);
    assert_eq!(
        incomplete[0].incomplete_reason.as_deref(),
        Some("cancelled")
    );
    // The completed batch left resumable persisted state.
    let stored: SessionStoreFile =
        serde_json::from_slice(&std::fs::read(store_path(dir.path(), &identity).unwrap()).unwrap())
            .unwrap();
    assert_eq!(stored.windows.len(), 25);

    // A later run continues without redoing completed windows.
    let second = run_session(
        &index,
        &request_with(identity, &pcm, 0, dir.path()),
        &mut |_update| {},
        &|| false,
    )
    .unwrap();
    assert!(!second.cancelled);
    assert_eq!(second.status, "complete");
    assert_eq!(second.reused_window_count, 25);
    assert_eq!(second.computed_window_count, 1);
    assert!(
        second
            .windows
            .iter()
            .all(|window| window.state == SessionWindowState::Completed)
    );
    for (previous, current) in first.windows.iter().zip(&second.windows) {
        if previous.state == SessionWindowState::Completed {
            assert_eq!(previous, current);
        }
    }
}

#[test]
fn derived_results_are_keyed_by_all_identities_and_staleness_is_refused() {
    let index = silence_index();
    let pcm = vec![0i16; 15 * RATE as usize + 64];
    let dir = tempdir().unwrap();
    let identity = identity_for(&index);
    let first = run_session(
        &index,
        &request_with(identity.clone(), &pcm, 0, dir.path()),
        &mut |_update| {},
        &|| false,
    )
    .unwrap();
    assert_eq!(first.computed_window_count, 4);
    // Unchanged identities reuse every derived window result.
    let second = run_session(
        &index,
        &request_with(identity.clone(), &pcm, 0, dir.path()),
        &mut |_update| {},
        &|| false,
    )
    .unwrap();
    assert_eq!(second.reused_window_count, 4);
    assert_eq!(second.computed_window_count, 0);

    // Any identity change refuses reuse: recording digest, capture clock and
    // scheduling identity recompute from scratch under a different key.
    let mut by_recording = identity.clone();
    by_recording.recording_digest = "other-recording".into();
    let mut by_clock = identity.clone();
    by_clock.capture_clock.clock_id = "other-clock".into();
    let mut by_schedule = identity.clone();
    by_schedule.scheduling_identity.step_seconds = 3;
    for variant in [by_recording, by_clock, by_schedule] {
        let run = run_session(
            &index,
            &request_with(variant.clone(), &pcm, 0, dir.path()),
            &mut |_update| {},
            &|| false,
        )
        .unwrap();
        assert_eq!(run.reused_window_count, 0, "{variant:?}");
        assert_eq!(
            run.computed_window_count,
            run.schedule.windows.len(),
            "{variant:?}"
        );
    }

    // Recipe, policy and index identities are validated against reality and
    // refuse to run at all when they change.
    let mut by_recipe = identity.clone();
    by_recipe.recipe_identity = "other-recipe".into();
    let err = run_session(
        &index,
        &request_with(by_recipe, &pcm, 0, dir.path()),
        &mut |_update| {},
        &|| false,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("recipe identity"), "{err}");
    let mut by_policy = identity.clone();
    by_policy.policy_identity = "other-policy".into();
    let err = run_session(
        &index,
        &request_with(by_policy, &pcm, 0, dir.path()),
        &mut |_update| {},
        &|| false,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("policy identity"), "{err}");
    let mut by_index = identity.clone();
    by_index.index_identity = "other-index".into();
    let err = run_session(
        &index,
        &request_with(by_index, &pcm, 0, dir.path()),
        &mut |_update| {},
        &|| false,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("index identity"), "{err}");

    // Stale derived results are refused even when planted under the right key.
    let mut planted = identity.clone();
    planted.recording_digest = "planted-recording".into();
    std::fs::copy(
        store_path(dir.path(), &identity).unwrap(),
        store_path(dir.path(), &planted).unwrap(),
    )
    .unwrap();
    let err = run_session(
        &index,
        &request_with(planted, &pcm, 0, dir.path()),
        &mut |_update| {},
        &|| false,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("refusing to reuse"), "{err}");

    // The original identity's file is untouched and still fully reusable.
    let third = run_session(
        &index,
        &request_with(identity, &pcm, 0, dir.path()),
        &mut |_update| {},
        &|| false,
    )
    .unwrap();
    assert_eq!(third.reused_window_count, 4);
    assert_eq!(third.computed_window_count, 0);
}

#[test]
fn overlap_accounting_separates_window_work_from_unique_coverage() {
    let rate = RATE;
    let ranges = [
        (0, 10 * u64::from(RATE)),
        (2 * u64::from(RATE), 12 * u64::from(RATE)),
        (4 * u64::from(RATE), 14 * u64::from(RATE)),
        (5 * u64::from(RATE), 15 * u64::from(RATE)),
    ];
    let accounting = coverage_accounting(rate, &ranges);
    assert_eq!(accounting.window_count, 4);
    assert!((accounting.overlapping_window_work_seconds - 40.0).abs() < 1e-9);
    assert!((accounting.unique_recording_coverage_seconds - 15.0).abs() < 1e-9);
    assert!((accounting.overlap_seconds - 25.0).abs() < 1e-9);
    // Overlapping windows never multiply unique coverage.
    let accepted = coverage_accounting(rate, &ranges[..2]);
    assert_eq!(accepted.window_count, 2);
    assert!((accepted.overlapping_window_work_seconds - 20.0).abs() < 1e-9);
    assert!((accepted.unique_recording_coverage_seconds - 12.0).abs() < 1e-9);
    assert!((accepted.overlap_seconds - 8.0).abs() < 1e-9);

    // Surfaced in the session result for the 15 s schedule (0, 2, 4, 5).
    let index = silence_index();
    let pcm = vec![0i16; 15 * RATE as usize + 64];
    let result = run_session(
        &index,
        &request_with(identity_for(&index), &pcm, 0, tempdir().unwrap().path()),
        &mut |_update| {},
        &|| false,
    )
    .unwrap();
    assert_eq!(result.window_accounting.window_count, 4);
    assert!((result.window_accounting.overlapping_window_work_seconds - 40.0).abs() < 1e-9);
    assert!((result.window_accounting.unique_recording_coverage_seconds - 15.0).abs() < 1e-9);
    // Silence never accepts: the accepted accounting stays empty.
    assert_eq!(result.accepted_accounting.window_count, 0);
    assert_eq!(
        result.accepted_accounting.overlapping_window_work_seconds,
        0.0
    );
}

#[test]
fn end_to_end_schedule_and_recognize_on_the_generated_synthetic_audio() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/source_audio_eval/synthetic");
    let movie_pcm = decode_wav(&root.join("movie.wav"), &|| false).unwrap();
    let recording_pcm = decode_wav(&root.join("recording-continuous.wav"), &|| false).unwrap();
    let index = Index::from_pcm(
        SyncMovieClock {
            movie_digest: "8870e7be64c3257a3630b1348f5511df33e0a6e374eeb6f55974002d15d0e86a".into(),
            time_base_numerator: 1,
            time_base_denominator: 11025,
            origin_pts: 0,
            duration_pts: 132_300,
        },
        &movie_pcm,
        Recipe::default(),
        &|| false,
    )
    .unwrap();
    assert_eq!(recording_pcm.len(), 165_375);

    // Epoch 1: the whole 15 s recording with its recording-time origin at
    // capture sample 0. The movie is heard from its start at recording time
    // 3.0 s, so every window offset is 0 - 3 = -3 within bin quantization.
    let mut epoch1 = identity();
    epoch1.recording_digest = "synthetic-ci-v1-recording-continuous".into();
    epoch1.index_identity = index.index_digest.clone();
    epoch1.capture_clock.clock_id = "synthetic-ci-v1-continuous".into();
    epoch1.scheduling_identity.epochs = vec![CaptureEpoch {
        epoch_id: "e1".into(),
        start_sample: 0,
        end_sample: 165_375,
    }];
    let result = run_session(
        &index,
        &request_with(epoch1, &recording_pcm, 0, tempdir().unwrap().path()),
        &mut |_update| {},
        &|| false,
    )
    .unwrap();
    assert_eq!(
        epoch_starts(&result.schedule, "e1"),
        vec![0.0, 2.0, 4.0, 5.0]
    );
    assert_eq!(result.status, "complete");
    for window in &result.windows {
        assert_eq!(window.state, SessionWindowState::Completed);
    }
    // Windows covering 10 s of movie audio must rank the true offset first
    // and pass the v3 acceptance gates.
    for start in [4.0, 5.0] {
        let window = result
            .windows
            .iter()
            .find(|window| window.window_start_seconds == start)
            .unwrap();
        let offset = window
            .offset_seconds
            .expect("movie-covered window has modes");
        assert!((offset + 3.0).abs() <= 0.1, "{start}: {offset}");
        assert!(window.is_accepted(), "{start}: {:?}", window.acceptance);
    }

    // Epoch 2: a nonzero native recording-time origin at capture sample
    // 55125 (recording 5.0 s). The query chunk no longer starts at the
    // recording start and the epoch-relative offset flips sign: the movie
    // position equals the epoch-relative position plus +2.
    let mut epoch2 = identity();
    epoch2.recording_digest = "synthetic-ci-v1-recording-continuous".into();
    epoch2.index_identity = index.index_digest.clone();
    epoch2.capture_clock.clock_id = "synthetic-ci-v1-continuous".into();
    epoch2.scheduling_identity.epochs = vec![CaptureEpoch {
        epoch_id: "e2".into(),
        start_sample: 55_125,
        end_sample: 165_375,
    }];
    let result = run_session(
        &index,
        &request_with(epoch2, &recording_pcm, 0, tempdir().unwrap().path()),
        &mut |_update| {},
        &|| false,
    )
    .unwrap();
    assert_eq!(epoch_starts(&result.schedule, "e2"), vec![0.0]);
    let window = &result.windows[0];
    assert_eq!(window.start_sample, 55_125);
    assert_eq!(window.end_sample, 165_375);
    let offset = window
        .offset_seconds
        .expect("movie-covered window has modes");
    assert!((offset - 2.0).abs() <= 0.1, "{offset}");
    assert!(window.is_accepted(), "{:?}", window.acceptance);
}
