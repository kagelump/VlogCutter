use super::*;
use crate::tv_review_sync::{CaptureClock, SyncMovieClock, SyncPolicy, SyncState};

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
        match_policy: SoundtrackPolicy::default(),
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
    let merged = merge_best_peaks(vec![
        peak(0.6, 10.00),
        peak(0.9, 10.10),  // within 0.256 of 10.00 but higher: wins
        peak(0.7, 10.356), // exactly 0.256 s from the accepted 10.10: kept
        peak(0.5, 20.0),
        peak(0.8, 30.0),
        peak(0.4, 40.0),
        peak(0.3, 50.0), // fifth distinct peak: dropped by the cap
    ]);
    let got: Vec<(f64, f64)> = merged
        .iter()
        .map(|p| (p.similarity, p.movie_seconds))
        .collect();
    assert_eq!(
        got,
        vec![(0.9, 10.10), (0.8, 30.0), (0.7, 10.356), (0.5, 20.0)]
    );
    // Equal scores break ties by movie time ascending.
    let tie = merge_best_peaks(vec![peak(0.5, 9.0), peak(0.5, 3.0)]);
    assert_eq!(tie[0].movie_seconds, 3.0);
    assert!(merge_best_peaks(vec![]).is_empty());
}
