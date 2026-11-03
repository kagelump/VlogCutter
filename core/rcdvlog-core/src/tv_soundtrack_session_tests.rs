use super::*;
use crate::tv_review_sync::{CaptureClock, SyncMovieClock, SyncPolicy, SyncState};
use crate::tv_soundtrack::DistinctivePolicy;

fn signal(seconds: usize) -> Vec<f32> {
    let mut state = 37_u64;
    (0..seconds * 8000)
        .map(|i| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let t = i as f64 / 8000.0;
            let noise = ((state >> 32) as u32 as f64 / u32::MAX as f64 - 0.5) * 0.07;
            let tone = (std::f64::consts::TAU * (t * 173.0 + 19.0 * (t * 0.9).sin())).sin()
                + 0.7 * (std::f64::consts::TAU * (t * 613.0 + 31.0 * (t * 0.57).cos())).sin();
            ((0.6 + 0.3 * (t * 3.7).sin()) * (0.7 + 0.25 * (t * 8.3).sin()) * tone * 0.2 + noise)
                as f32
        })
        .collect()
}
fn wav(path: &Path, pcm: &[f32]) {
    let mut out = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 8000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for s in pcm {
        out.write_sample(*s).unwrap();
    }
    out.finalize().unwrap();
}
fn fixture(
    repeat: bool,
) -> (
    tempfile::TempDir,
    SoundtrackSessionPlan,
    PathBuf,
    Vec<SessionAudioFile>,
) {
    fixture_with(repeat, SoundtrackPolicy::default())
}
fn distinctive_policy() -> SoundtrackPolicy {
    SoundtrackPolicy {
        distinctive: Some(DistinctivePolicy {
            minimum_similarity: 0.5,
            minimum_margin: 0.25,
            minimum_z_score: 6.0,
        }),
        ..SoundtrackPolicy::default()
    }
}
fn fixture_with(
    repeat: bool,
    match_policy: SoundtrackPolicy,
) -> (
    tempfile::TempDir,
    SoundtrackSessionPlan,
    PathBuf,
    Vec<SessionAudioFile>,
) {
    let dir = tempfile::tempdir().unwrap();
    let block = signal(24);
    let mut movie_pcm = block.clone();
    if repeat {
        movie_pcm.extend(&block);
    }
    let movie = dir.path().join("final mix.wav");
    wav(&movie, &movie_pcm);
    let mic = dir.path().join("native mic.wav");
    wav(&mic, &block[6 * 8000..12 * 8000]);
    let plan = SoundtrackSessionPlan::new(SoundtrackSessionSpec {
        schema_version: 1,
        sync: SyncRequest {
            schema_version: 1,
            session_id: "synthetic_session".into(),
            target_digest: "a".repeat(64),
            movie: SyncMovieClock {
                movie_digest: full_file_blake3(&movie, &|| false).unwrap().hex_digest,
                time_base_numerator: 1,
                time_base_denominator: 90000,
                origin_pts: 90000,
                duration_pts: movie_pcm.len() as u64 / 8000 * 90000,
            },
            capture_clocks: vec![CaptureClock {
                clock_id: "mic".into(),
                sample_rate: 8000,
                first_sample: 16000,
                last_sample: 64000,
                monotonic_origin_ns: 1_000_000_000,
                monotonic_tolerance_ns: 1,
            }],
            anchors: vec![],
            boundaries: vec![],
            policy: SyncPolicy::default(),
            consensus_segments: vec![],
        },
        segments: vec![SessionAudioSegment {
            segment_id: "native".into(),
            clock_id: "mic".into(),
            first_sample: 16000,
            frame_count: 48000,
            wav_blake3: full_file_blake3(&mic, &|| false).unwrap().hex_digest,
        }],
        reference_seconds: 12,
        query_seconds: 6,
        query_step_seconds: 6,
        match_policy,
        speech_mask: vec![],
        recipe: None,
    })
    .unwrap();
    (
        dir,
        plan,
        movie,
        vec![SessionAudioFile {
            segment_id: "native".into(),
            path: mic,
        }],
    )
}
#[test]
fn complete_search_resumes_and_coalesces_overlapping_reference_confirmations() {
    let (dir, plan, movie, files) = fixture(false);
    assert_eq!(plan.references.len(), 3);
    assert_eq!(plan.queries.len(), 1);
    let cache = dir.path().join("private cache");
    let batch = plan
        .run_batch(
            SessionBatch {
                movie: &movie,
                files: &files,
                cache: &cache,
                start_task: 0,
                max_tasks: 1,
            },
            &|| false,
        )
        .unwrap();
    assert_eq!(batch.next_task, 1);
    let pending = plan.evidence_page(&cache, 0, 1, &|| false).unwrap();
    assert_eq!(pending.queries[0].state, "pending");
    assert!(
        pending.sync.anchors.is_empty(),
        "one reference match cannot establish full-movie uniqueness"
    );
    let batch = plan
        .run_batch(
            SessionBatch {
                movie: &movie,
                files: &files,
                cache: &cache,
                start_task: 0,
                max_tasks: 128,
            },
            &|| false,
        )
        .unwrap();
    assert_eq!(batch.reused, 1);
    assert_eq!(batch.computed, 2);
    let page = plan.evidence_page(&cache, 0, 1, &|| false).unwrap();
    assert_eq!(page.queries[0].state, "candidate", "{page:?}");
    assert_eq!(
        page.sync.anchors.len(),
        1,
        "same position seen in overlap must not become false ambiguity"
    );
    let anchor = &page.sync.anchors[0];
    assert!((anchor.movie_pts - 10 * 90000).abs() <= 1800);
    assert_eq!(anchor.capture.sample_index, 40000);
    let q = SyncQuery {
        clock_id: "mic".into(),
        start_sample: 40000,
        end_sample: 40000,
    };
    assert_eq!(
        page.sync.map(&[q], &|| false).unwrap().slices[0].state,
        SyncState::Anchored
    );
    assert_eq!(page, plan.evidence_page(&cache, 0, 1, &|| false).unwrap());
    assert_eq!(
        plan.run_batch(
            SessionBatch {
                movie: &movie,
                files: &files,
                cache: &cache,
                start_task: 0,
                max_tasks: 128
            },
            &|| false
        )
        .unwrap()
        .reused,
        3
    );
}
#[test]
fn distant_repeated_soundtrack_stays_ambiguous_after_all_references_complete() {
    let (dir, plan, movie, files) = fixture(true);
    let cache = dir.path().join("cache");
    plan.run_batch(
        SessionBatch {
            movie: &movie,
            files: &files,
            cache: &cache,
            start_task: 0,
            max_tasks: 128,
        },
        &|| false,
    )
    .unwrap();
    let page = plan.evidence_page(&cache, 0, 1, &|| false).unwrap();
    assert_eq!(page.queries[0].state, "ambiguous", "{page:?}");
    assert!(page.sync.anchors.len() >= 2);
    let sample = page.sync.anchors[0].capture.sample_index;
    let mapped = page
        .sync
        .map(
            &[SyncQuery {
                clock_id: "mic".into(),
                start_sample: sample,
                end_sample: sample,
            }],
            &|| false,
        )
        .unwrap();
    assert_eq!(mapped.slices[0].state, SyncState::Conflicting);
    assert!(mapped.slices[0].candidate_operation_ids.is_empty());
}
#[test]
fn plan_covers_reference_edges_and_original_audio_gaps_without_clock_invention() {
    let (_, plan, _, _) = fixture(false);
    let mut spec = plan.spec.clone();
    spec.sync.capture_clocks[0].last_sample += 80000;
    spec.segments.push(SessionAudioSegment {
        segment_id: "after_gap".into(),
        clock_id: "mic".into(),
        first_sample: 80000,
        frame_count: 48000,
        wav_blake3: "b".repeat(64),
    });
    let plan = SoundtrackSessionPlan::new(spec).unwrap();
    for i in 0..=18 * 100 {
        let start = f64::from(i) / 100.0;
        assert!(
            plan.references
                .iter()
                .any(|r| r.start_seconds <= start + 1e-9
                    && r.start_seconds + r.duration_seconds >= start + 6.0 - 1e-9)
        );
    }
    assert!(
        plan.queries
            .iter()
            .all(|q| q.recording.end_sample <= 64000 || q.recording.start_sample >= 80000)
    );
    let dir = tempfile::tempdir().unwrap();
    let page = plan.evidence_page(dir.path(), 0, 64, &|| false).unwrap();
    assert_eq!(page.sync.boundaries.len(), 2);
    assert_eq!(page.sync.capture_clocks, plan.spec.sync.capture_clocks);
    let mut forged = plan.clone();
    forged.references.pop();
    assert!(forged.validate().is_err());
    let mut bad = plan.spec.clone();
    bad.segments[1].first_sample = 60000;
    assert!(SoundtrackSessionPlan::new(bad).is_err());
}
#[test]
fn cancellation_and_modified_private_evidence_cannot_create_anchors() {
    let (dir, plan, movie, files) = fixture(false);
    let cache = dir.path().join("cache");
    let result = plan
        .run_batch(
            SessionBatch {
                movie: &movie,
                files: &files,
                cache: &cache,
                start_task: 0,
                max_tasks: 1,
            },
            &|| true,
        )
        .unwrap();
    assert!(result.cancelled);
    assert_eq!(result.next_task, 0);
    assert!(
        plan.evidence_page(&cache, 0, 1, &|| false)
            .unwrap()
            .sync
            .anchors
            .is_empty()
    );
    plan.run_batch(
        SessionBatch {
            movie: &movie,
            files: &files,
            cache: &cache,
            start_task: 0,
            max_tasks: 1,
        },
        &|| false,
    )
    .unwrap();
    let path = plan.receipt_path(&cache, 0);
    let mut receipt: TaskReceipt = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    receipt.report.capture.first_sample += 1;
    std::fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    assert!(plan.evidence_page(&cache, 0, 1, &|| false).is_err());
    wav(&files[0].path, &vec![0.0; 48000]);
    assert!(
        plan.run_batch(
            SessionBatch {
                movie: &movie,
                files: &files,
                cache: &cache,
                start_task: 1,
                max_tasks: 1
            },
            &|| false
        )
        .unwrap_err()
        .to_string()
        .contains("WAV digest mismatch")
    );
    assert!(
        plan.run_batch(
            SessionBatch {
                movie: &movie,
                files: &files,
                cache: &cache,
                start_task: plan.task_count + 1,
                max_tasks: 1
            },
            &|| false
        )
        .is_err()
    );
}

#[test]
fn published_session_schema_bounds_and_shared_sync_contract_agree() {
    let (_, plan, _, _) = fixture(false);
    let mut schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../docs/contracts/tv-soundtrack-session-v1.schema.json"
    ))
    .unwrap();
    // Resolve the relative shared contract locally; no network schema fetch.
    let sync_schema = include_str!("../../../docs/contracts/tv-review-sync-v1.schema.json")
        .replace("#/$defs/", "#/properties/sync/$defs/");
    schema["properties"]["sync"] = serde_json::from_str(&sync_schema).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let value = serde_json::to_value(&plan.spec).unwrap();
    validator.validate(&value).unwrap();
    let bytes = serde_json::to_vec(&plan).unwrap();
    assert_eq!(SoundtrackSessionPlan::from_json(&bytes).unwrap(), plan);
    assert!(SoundtrackSessionPlan::from_json(&vec![b' '; MAX_JSON + 1]).is_err());
    let mut bad = value;
    bad["sync"]["captureClocks"][0]["sampleRate"] = serde_json::json!(0);
    assert!(validator.validate(&bad).is_err());
    let mut spec = plan.spec.clone();
    spec.sync.boundaries.push(SyncBoundary {
        evidence_id: "seek".into(),
        clock_id: "mic".into(),
        sample_index: 40000,
        kind: SyncBoundaryKind::Seek,
    });
    let split = SoundtrackSessionPlan::new(spec).unwrap();
    assert!(
        split.queries.is_empty(),
        "neither three-second side supports a six-second window"
    );
    assert_eq!(split.short_segment_ids, vec!["native"]);
    assert!(
        split
            .sync_evidence(
                tempfile::tempdir().unwrap().path(),
                &[SyncQuery {
                    clock_id: "mic".into(),
                    start_sample: 40000,
                    end_sample: 40000
                }],
                &|| false
            )
            .unwrap()
            .sync
            .anchors
            .is_empty()
    );
}
#[test]
fn session_best_peaks_merge_overlapping_reference_observations_once() {
    let (dir, plan, movie, files) = fixture(false);
    let cache = dir.path().join("cache");
    plan.run_batch(
        SessionBatch {
            movie: &movie,
            files: &files,
            cache: &cache,
            start_task: 0,
            max_tasks: 128,
        },
        &|| false,
    )
    .unwrap();
    // Raw receipts: the same movie position is seen by more than one reference.
    let mut raw = Vec::new();
    for ri in 0..plan.references.len() {
        let receipt = plan.receipt(&cache, ri).unwrap().unwrap();
        raw.extend(receipt.report.best_peaks.iter().cloned());
    }
    let near_true = raw
        .iter()
        .filter(|p| (p.movie_seconds - 9.0).abs() < 0.256)
        .count();
    assert!(
        near_true >= 2,
        "overlap must produce repeated observations: {raw:?}"
    );
    let page = plan.evidence_page(&cache, 0, 1, &|| false).unwrap();
    let evidence = &page.queries[0];
    assert_eq!(evidence.state, "candidate", "{page:?}");
    assert!(!evidence.best_peaks.is_empty() && evidence.best_peaks.len() <= MAX_BEST_PEAKS);
    for (i, a) in evidence.best_peaks.iter().enumerate() {
        for b in &evidence.best_peaks[i + 1..] {
            assert!(
                (a.movie_seconds - b.movie_seconds).abs() >= PEAK_MERGE_SECONDS,
                "{a:?} {b:?}"
            );
        }
    }
    for pair in evidence.best_peaks.windows(2) {
        assert!(pair[0].similarity >= pair[1].similarity);
    }
    // The merged best peak is the anchor position, with the §3 centre convention.
    let anchor = &page.sync.anchors[0];
    let tps = 90000.0;
    let anchor_seconds = (anchor.movie_pts - 90000) as f64 / tps;
    assert!(
        (evidence.best_peaks[0].movie_seconds - anchor_seconds).abs() <= 0.016,
        "{:?} vs anchor {anchor_seconds}",
        evidence.best_peaks[0]
    );
    assert_eq!(page, plan.evidence_page(&cache, 0, 1, &|| false).unwrap());
}
#[test]
fn ambiguous_session_query_keeps_its_peaks_as_diagnostics() {
    let (dir, plan, movie, files) = fixture(true);
    let cache = dir.path().join("cache");
    plan.run_batch(
        SessionBatch {
            movie: &movie,
            files: &files,
            cache: &cache,
            start_task: 0,
            max_tasks: 128,
        },
        &|| false,
    )
    .unwrap();
    let page = plan.evidence_page(&cache, 0, 1, &|| false).unwrap();
    let evidence = &page.queries[0];
    assert_eq!(evidence.state, "ambiguous", "{page:?}");
    assert!(!evidence.best_peaks.is_empty());
    assert!(evidence.best_peaks.len() <= MAX_BEST_PEAKS);
    // The two true repeats (movie 9 s and 33 s) both appear among the merged peaks.
    let positions: Vec<f64> = evidence
        .best_peaks
        .iter()
        .map(|p| p.movie_seconds)
        .collect();
    assert!(
        positions.iter().any(|s| (s - 9.0).abs() <= 0.016),
        "{positions:?}"
    );
    assert!(
        positions.iter().any(|s| (s - 33.0).abs() <= 0.016),
        "{positions:?}"
    );
}
#[test]
fn merge_keeps_highest_peak_within_merge_distance_and_orders_deterministically() {
    let peak = |similarity: f64, movie_seconds: f64| SoundtrackPeak {
        similarity,
        reference_offset_frames: 0,
        movie_seconds,
        subwindow_similarities: [0.0; 3],
    };
    let merged = merge_peaks(
        vec![
            peak(0.6, 10.00),
            peak(0.9, 10.10),  // within 0.256 of 10.00 but higher: wins
            peak(0.7, 10.356), // exactly 0.256 s from the accepted 10.10: kept
            peak(0.5, 20.0),
            peak(0.8, 30.0),
            peak(0.4, 40.0),
            peak(0.3, 50.0), // fifth distinct peak: dropped by the cap
        ],
        MAX_BEST_PEAKS,
    );
    let got: Vec<(f64, f64)> = merged
        .iter()
        .map(|p| (p.similarity, p.movie_seconds))
        .collect();
    assert_eq!(
        got,
        vec![(0.9, 10.10), (0.8, 30.0), (0.7, 10.356), (0.5, 20.0)]
    );
    // Equal scores break ties by movie time ascending.
    let tie = merge_peaks(vec![peak(0.5, 9.0), peak(0.5, 3.0)], MAX_BEST_PEAKS);
    assert_eq!(tie[0].movie_seconds, 3.0);
    assert!(merge_peaks(vec![], MAX_BEST_PEAKS).is_empty());
}

fn run_all(plan: &SoundtrackSessionPlan, movie: &Path, files: &[SessionAudioFile], cache: &Path) {
    plan.run_batch(
        SessionBatch {
            movie,
            files,
            cache,
            start_task: 0,
            max_tasks: 128,
        },
        &|| false,
    )
    .unwrap();
}
#[test]
fn distinctive_session_with_distant_repeat_emits_no_anchors() {
    // ARCHITECTURE §5: with distinctive set, a repeat seen by the references
    // is ambiguous, and no anchor may survive the global margin check.
    let (dir, plan, movie, files) = fixture_with(true, distinctive_policy());
    let cache = dir.path().join("cache");
    run_all(&plan, &movie, &files, &cache);
    let matched = (0..plan.references.len())
        .filter(|ri| {
            plan.receipt(&cache, *ri).unwrap().unwrap().report.state
                == SoundtrackMatchState::Matched
        })
        .count();
    assert!(matched >= 1, "a reference must report Matched on its own");
    let page = plan.evidence_page(&cache, 0, 1, &|| false).unwrap();
    let evidence = &page.queries[0];
    assert_eq!(evidence.state, "ambiguous", "{page:?}");
    assert!(evidence.anchors.is_empty());
    assert!(evidence.supporting_evidence_ids.is_empty());
    assert!(page.sync.anchors.is_empty());
    assert!(!evidence.best_peaks.is_empty());
}
#[test]
fn distinctive_session_single_occurrence_across_overlap_stays_matched() {
    // Both overlapping references see the same position; the rule must not
    // treat that observation as its own rival.
    let (dir, plan, movie, files) = fixture_with(false, distinctive_policy());
    let cache = dir.path().join("cache");
    run_all(&plan, &movie, &files, &cache);
    let page = plan.evidence_page(&cache, 0, 1, &|| false).unwrap();
    assert_eq!(page.queries[0].state, "candidate", "{page:?}");
    assert_eq!(page.sync.anchors.len(), 1);
}
/// Adds a rival peak to every cached receipt, below the top peak by `gap` and at
/// `seconds`, then rewrites each receipt with a fresh report digest.
fn inject_rival(plan: &SoundtrackSessionPlan, cache: &Path, gap: f64, seconds: f64) {
    for ri in 0..plan.references.len() {
        let path = plan.receipt_path(cache, ri);
        let mut receipt: TaskReceipt =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut rival = receipt.report.best_peaks[0].clone();
        rival.similarity -= gap;
        rival.movie_seconds = seconds;
        receipt.report.best_peaks.push(rival);
        receipt.report_digest = digest(&receipt.report).unwrap();
        std::fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    }
}
#[test]
fn distinctive_session_rival_breaking_margin_removes_anchor() {
    // A distant rival 0.01 below the matched peak breaks the applicable margin.
    let (dir, plan, movie, files) = fixture_with(false, distinctive_policy());
    let cache = dir.path().join("cache");
    run_all(&plan, &movie, &files, &cache);
    let baseline = plan.evidence_page(&cache, 0, 1, &|| false).unwrap();
    assert_eq!(baseline.queries[0].state, "candidate", "{baseline:?}");
    inject_rival(&plan, &cache, 0.01, 33.0);
    let page = plan.evidence_page(&cache, 0, 1, &|| false).unwrap();
    assert_eq!(page.queries[0].state, "ambiguous", "{page:?}");
    assert!(page.queries[0].anchors.is_empty());
    assert!(page.sync.anchors.is_empty());
}
#[test]
fn distinctive_session_rival_within_merge_distance_is_not_a_rival() {
    // The same observation, 0.1 s away and 0.01 lower, does not break margin.
    let (dir, plan, movie, files) = fixture_with(false, distinctive_policy());
    let cache = dir.path().join("cache");
    run_all(&plan, &movie, &files, &cache);
    let top = plan.receipt(&cache, 0).unwrap().unwrap().report.best_peaks[0].movie_seconds;
    inject_rival(&plan, &cache, 0.01, top + 0.1);
    let page = plan.evidence_page(&cache, 0, 1, &|| false).unwrap();
    assert_eq!(page.queries[0].state, "candidate", "{page:?}");
    assert_eq!(page.sync.anchors.len(), 1);
}
#[test]
fn distinctive_none_keeps_v1_repeat_behaviour_and_peaks() {
    // Without distinctive, the §5 check is absent: the v1 two-anchor result stays.
    let (dir, plan, movie, files) = fixture(true);
    let cache = dir.path().join("cache");
    run_all(&plan, &movie, &files, &cache);
    let page = plan.evidence_page(&cache, 0, 1, &|| false).unwrap();
    assert_eq!(page.queries[0].state, "ambiguous", "{page:?}");
    assert!(page.sync.anchors.len() >= 2);
}
fn with_mask(plan: &SoundtrackSessionPlan, mask: Vec<SyncQuery>) -> Result<SoundtrackSessionPlan> {
    let mut spec = plan.spec.clone();
    spec.speech_mask = mask;
    SoundtrackSessionPlan::new(spec)
}
#[test]
fn speech_mask_is_part_of_session_identity_and_every_receipt_mask_digest() {
    let (dir, plan, movie, files) = fixture(false);
    // The whole 6 s recording is masked: every query is masked_insufficient.
    let full = SyncQuery {
        clock_id: "mic".into(),
        start_sample: 16000,
        end_sample: 64000,
    };
    let fully_masked = with_mask(&plan, vec![full]).unwrap();
    assert_ne!(fully_masked.plan_digest, plan.plan_digest);
    let full_cache = dir.path().join("full");
    run_all(&fully_masked, &movie, &files, &full_cache);
    let page = fully_masked
        .evidence_page(&full_cache, 0, 1, &|| false)
        .unwrap();
    assert!(page.sync.anchors.is_empty(), "{page:?}");
    for task in 0..fully_masked.task_count {
        let receipt = fully_masked.receipt(&full_cache, task).unwrap().unwrap();
        assert_eq!(receipt.report.reason, "masked_insufficient");
        assert!(receipt.report.mask_digest.is_some());
    }
    // A partial mask (first 0.5 s) changes the identity and the mask digest.
    let partial = SyncQuery {
        clock_id: "mic".into(),
        start_sample: 16000,
        end_sample: 20000,
    };
    let partly_masked = with_mask(&plan, vec![partial]).unwrap();
    assert_ne!(partly_masked.plan_digest, fully_masked.plan_digest);
    assert_ne!(partly_masked.plan_digest, plan.plan_digest);
    let partial_cache = dir.path().join("partial");
    run_all(&partly_masked, &movie, &files, &partial_cache);
    let partial_receipt = partly_masked.receipt(&partial_cache, 0).unwrap().unwrap();
    let full_receipt = fully_masked.receipt(&full_cache, 0).unwrap().unwrap();
    assert_ne!(
        partial_receipt.report.mask_digest,
        full_receipt.report.mask_digest
    );
    assert_eq!(
        partial_receipt.report.mask_digest.as_deref(),
        partly_masked
            .query_weights(&partly_masked.queries[0])
            .as_deref()
            .map(weights_digest)
            .as_deref()
    );
    // Unmasked sessions keep no mask digest, so v1 receipts are unchanged.
    run_all(&plan, &movie, &files, &dir.path().join("clean"));
    let clean_receipt = plan.receipt(&dir.path().join("clean"), 0).unwrap().unwrap();
    assert_eq!(clean_receipt.report.mask_digest, None);
}
#[test]
fn speech_mask_rejects_unknown_clock_and_empty_intervals() {
    let (_dir, plan, _movie, _files) = fixture(false);
    let unknown = SyncQuery {
        clock_id: "nope".into(),
        start_sample: 16000,
        end_sample: 20000,
    };
    assert!(with_mask(&plan, vec![unknown]).is_err());
    let empty = SyncQuery {
        clock_id: "mic".into(),
        start_sample: 20000,
        end_sample: 20000,
    };
    assert!(with_mask(&plan, vec![empty]).is_err());
}
/// A plan-only spec (no audio is read by `SoundtrackSessionPlan::new`): an
/// 8 kHz mic clock of `mic_seconds`, a movie of `movie_seconds`.
fn long_spec(
    mic_seconds: u64,
    movie_seconds: u64,
    reference_seconds: u32,
    query_step_seconds: u32,
    mask_seconds: &[(f64, f64)],
) -> SoundtrackSessionSpec {
    let (_dir, plan, _movie, _files) = fixture(false);
    let mut spec = plan.spec.clone();
    spec.sync.movie.duration_pts = movie_seconds * 90000;
    spec.sync.capture_clocks[0].first_sample = 0;
    spec.sync.capture_clocks[0].last_sample = mic_seconds * 8000;
    spec.segments[0].first_sample = 0;
    spec.segments[0].frame_count = (mic_seconds * 8000) as u32;
    spec.reference_seconds = reference_seconds;
    spec.query_step_seconds = query_step_seconds;
    spec.speech_mask = mask_seconds
        .iter()
        .map(|&(a, b)| SyncQuery {
            clock_id: "mic".into(),
            start_sample: (a * 8000.0) as u64,
            end_sample: (b * 8000.0) as u64,
        })
        .collect();
    spec
}
fn gap_ranges(plan: &SoundtrackSessionPlan) -> Vec<(u64, u64)> {
    plan.queries
        .iter()
        .filter(|q| q.origin.as_deref() == Some("speech_gap"))
        .map(|q| (q.recording.start_sample, q.recording.end_sample))
        .collect()
}
#[test]
fn gap_queries_cover_each_long_unmasked_gap_once_sized_and_centred() {
    // Gaps: [0,3) 3 s (too short), [5,9) 4 s, [11,18) 7 s, [20,65) 45 s, then masked to 80 s.
    let spec = long_spec(
        80,
        120,
        30,
        6,
        &[(3.0, 5.0), (9.0, 11.0), (18.0, 20.0), (65.0, 80.0)],
    );
    let plan = SoundtrackSessionPlan::new(spec).unwrap();
    let s = |x: f64| (x * 8000.0) as u64;
    assert_eq!(
        gap_ranges(&plan),
        vec![(s(5.0), s(9.0)), (s(11.0), s(18.0)), (s(32.5), s(52.5))]
    );
    // Grid queries are unchanged and keep origin None; gap queries come after them.
    let grid: Vec<_> = plan
        .queries
        .iter()
        .take_while(|q| q.origin.is_none())
        .collect();
    assert_eq!(grid.len() + 3, plan.queries.len());
    assert!(
        grid.iter()
            .all(|q| q.recording.end_sample - q.recording.start_sample == s(6.0))
    );
    // Evidence ids are unique even if a gap query repeats a grid range.
    let ids: HashSet<_> = plan
        .queries
        .iter()
        .map(|q| &q.capture.evidence_id)
        .collect();
    assert_eq!(ids.len(), plan.queries.len());
    // With a mask, references overlap by the 20 s gap cap (30 s wide, 10 s step).
    assert_eq!(
        plan.references[1].start_seconds - plan.references[0].start_seconds,
        10.0
    );
    // The plan round-trips through validate().
    plan.validate().unwrap();
}
#[test]
fn gap_queries_clamp_to_recording_edges_and_merge_overlapping_mask_intervals() {
    // Overlapping/adjacent intervals merge into [10,30); edge gaps [0,10) and [30,50).
    let spec = long_spec(50, 120, 30, 6, &[(10.0, 20.0), (15.0, 25.0), (25.0, 30.0)]);
    let plan = SoundtrackSessionPlan::new(spec).unwrap();
    let s = |x: f64| (x * 8000.0) as u64;
    assert_eq!(
        gap_ranges(&plan),
        vec![(s(0.0), s(10.0)), (s(30.0), s(50.0))]
    );
    // Exactly 4.0 s qualifies; 3.999 s does not.
    let plan = SoundtrackSessionPlan::new(long_spec(40, 120, 30, 6, &[(4.0, 40.0)])).unwrap();
    assert_eq!(gap_ranges(&plan), vec![(0, s(4.0))]);
    let plan = SoundtrackSessionPlan::new(long_spec(40, 120, 30, 6, &[(3.999, 40.0)])).unwrap();
    assert!(gap_ranges(&plan).is_empty());
}
#[test]
fn gap_queries_respect_the_query_quota_longest_first() {
    // 4093 s at a 1 s grid step with 6 s queries -> 4088 grid queries, room for 8 gaps.
    let mut mask = Vec::new();
    let mut gaps = Vec::new();
    let mut t = 0.0;
    for k in 0..12 {
        let len = 4.0 + k as f64 * 0.5; // 4.0 .. 9.5 s
        gaps.push((t, t + len));
        mask.push((t + len, t + len + 1.0));
        t += len + 1.0;
    }
    mask.push((t, 4093.0));
    let plan = SoundtrackSessionPlan::new(long_spec(4093, 120, 30, 1, &mask)).unwrap();
    let grid: Vec<_> = plan.queries.iter().filter(|q| q.origin.is_none()).collect();
    let s = |x: f64| (x * 8000.0) as u64;
    // Gaps whose range equals a grid query are not duplicated, and do not use
    // a quota slot.
    let gaps: Vec<_> = gaps
        .into_iter()
        .filter(|&(a, b)| {
            !grid
                .iter()
                .any(|q| q.recording.start_sample == s(a) && q.recording.end_sample == s(b))
        })
        .collect();
    assert!(gaps.len() < 12, "the fixture contains a grid-identical gap");
    let kept = gap_ranges(&plan);
    assert_eq!(grid.len() + kept.len(), 4096);
    // The longest gaps survive, reported in recording order.
    let expected: Vec<_> = gaps[gaps.len() - kept.len()..]
        .iter()
        .map(|&(a, b)| (s(a), s(b)))
        .collect();
    assert_eq!(kept, expected);
}
#[test]
fn gap_query_longer_than_grid_is_searched_and_anchored_at_its_true_position() {
    let dir = tempfile::tempdir().unwrap();
    let movie_pcm = signal(60);
    let movie = dir.path().join("movie.wav");
    wav(&movie, &movie_pcm);
    // The mic hears movie 10..30 s; masked [0,2) and [16,20) leave a 14 s gap.
    let mic = dir.path().join("mic.wav");
    wav(&mic, &movie_pcm[10 * 8000..30 * 8000]);
    let mut spec = long_spec(20, 60, 30, 6, &[(0.0, 2.0), (16.0, 20.0)]);
    spec.sync.movie.movie_digest = full_file_blake3(&movie, &|| false).unwrap().hex_digest;
    spec.segments[0].wav_blake3 = full_file_blake3(&mic, &|| false).unwrap().hex_digest;
    let plan = SoundtrackSessionPlan::new(spec).unwrap();
    let s = |x: f64| (x * 8000.0) as u64;
    assert_eq!(gap_ranges(&plan), vec![(s(2.0), s(16.0))]);
    let files = vec![SessionAudioFile {
        segment_id: "native".into(),
        path: mic,
    }];
    let cache = dir.path().join("cache");
    run_all(&plan, &movie, &files, &cache);
    let qi = plan
        .queries
        .iter()
        .position(|q| q.origin.is_some())
        .unwrap();
    let page = plan.evidence_page(&cache, qi, 1, &|| false).unwrap();
    let ev = &page.queries[0];
    assert_eq!(ev.completed_references, ev.total_references);
    assert_eq!(ev.anchors.len(), 1, "{ev:?}");
    // Centre of mic [2,16) s is mic 9 s = movie 19 s (origin_pts is 1 s).
    let tps = 90000.0;
    let movie_s = ev.anchors[0].movie_pts as f64 / tps - 1.0;
    assert!((movie_s - 19.0).abs() < 0.05, "anchored at {movie_s}");
    let receipt = plan.receipt(&cache, qi * plan.references.len()).unwrap();
    assert!(receipt.is_some());
}
