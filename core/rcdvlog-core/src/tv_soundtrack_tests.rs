use super::*;
fn signal(seconds: usize, seed: u64) -> Vec<f32> {
    let mut state = seed;
    let mut out = Vec::new();
    for i in 0..seconds * PCM_RATE as usize {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let noise = ((state >> 32) as u32 as f64 / u32::MAX as f64 - 0.5) * 0.06;
        let t = i as f64 / PCM_RATE as f64;
        let shift = seed as f64 * 0.001;
        let envelope =
            (0.6 + 0.3 * (t * 3.7 + shift).sin()) * (0.7 + 0.25 * (t * 8.3 + shift).sin());
        let tone = (std::f64::consts::TAU * (t * 173.0 + 19.0 * (t * 0.9 + shift).sin())).sin()
            + 0.7 * (std::f64::consts::TAU * (t * 613.0 + 31.0 * (t * 0.57 + shift).cos())).sin()
            + 0.4 * (std::f64::consts::TAU * (t * 1411.0 + 13.0 * (t * 1.37 + shift).sin())).sin();
        out.push((envelope * tone * 0.2 + noise) as f32);
    }
    out
}
fn spec(n: usize) -> SoundtrackReferenceSpec {
    SoundtrackReferenceSpec {
        schema_version: 1,
        movie: SyncMovieClock {
            movie_digest: "a".repeat(64),
            time_base_numerator: 1,
            time_base_denominator: 90000,
            origin_pts: 90000,
            duration_pts: n as u64 * 90000,
        },
        start_seconds: 0.0,
        duration_seconds: n as f64,
        recipe: None,
    }
}
fn capture() -> SoundtrackCapture {
    SoundtrackCapture {
        evidence_id: "synthetic_query".into(),
        clock_id: "mic".into(),
        sample_rate: 48000,
        first_sample: 96000,
        monotonic_origin_ns: 1000000000,
    }
}
fn center_error(report: &SoundtrackMatchReport, start: f64, seconds: f64) -> f64 {
    let truth = 90000.0 + (start + seconds / 2.0) * 90000.0;
    (report.candidates[0].anchor.movie_pts as f64 - truth).abs() / 90000.0
}
#[test]
fn distinctive_gain_room_and_conversation_are_bounded_or_remain_unmatched() {
    let source = signal(36, 37);
    let index = SoundtrackIndex::from_pcm(spec(36), &source, &|| false).unwrap();
    let start = 8.32;
    let query = source[(start * 8000.0) as usize..(start * 8000.0) as usize + 6 * 8000].to_vec();
    let other = signal(6, 987);
    let mut filtered = 0.0f32;
    for mode in 0..4 {
        let mut transformed = query.clone();
        for i in 0..query.len() {
            transformed[i] = match mode {
                0 => query[i],
                1 => query[i] * 0.12,
                2 => {
                    filtered = 0.7 * filtered + 0.3 * query[i];
                    filtered + if i >= 240 { query[i - 240] * 0.25 } else { 0.0 }
                }
                _ => query[i] * 0.8 + other[i] * 0.25,
            };
        }
        let r = index
            .match_pcm(
                &transformed,
                capture(),
                &SoundtrackPolicy::default(),
                &|| false,
            )
            .unwrap();
        if mode < 2 {
            assert_eq!(r.state, SoundtrackMatchState::Matched, "mode {mode}: {r:?}");
        }
        if r.state == SoundtrackMatchState::Matched {
            assert!(center_error(&r, start, 6.0) <= 0.1, "mode {mode}: {r:?}");
        }
        assert!(r.comparison_products <= MAX_PRODUCTS);
    }
    let r = index
        .match_pcm(&query, capture(), &SoundtrackPolicy::default(), &|| false)
        .unwrap();
    let json = serde_json::to_vec(&index).unwrap();
    assert_eq!(SoundtrackIndex::from_json(&json).unwrap(), index);
    assert_eq!(
        r,
        index
            .match_pcm(&query, capture(), &SoundtrackPolicy::default(), &|| false)
            .unwrap()
    );
    assert!(
        index
            .match_pcm(&query, capture(), &SoundtrackPolicy::default(), &|| true)
            .is_err()
    );
}
#[test]
fn repeats_silence_and_unrelated_audio_do_not_force_unique_positions() {
    let block = signal(12, 91);
    let mut source = block.clone();
    source.extend(&block);
    let index = SoundtrackIndex::from_pcm(spec(24), &source, &|| false).unwrap();
    let query = &source[2 * 8000..8 * 8000];
    let r = index
        .match_pcm(query, capture(), &SoundtrackPolicy::default(), &|| false)
        .unwrap();
    assert_eq!(r.state, SoundtrackMatchState::Ambiguous, "{r:?}");
    assert!(r.candidates.len() >= 2);
    let zero = vec![0.0; 6 * 8000];
    assert_eq!(
        index
            .match_pcm(&zero, capture(), &SoundtrackPolicy::default(), &|| false)
            .unwrap()
            .state,
        SoundtrackMatchState::Unmatched
    );
    let unrelated = signal(6, 878123);
    let r = index
        .match_pcm(&unrelated, capture(), &SoundtrackPolicy::default(), &|| {
            false
        })
        .unwrap();
    assert_ne!(
        r.state,
        SoundtrackMatchState::Matched,
        "unrelated signal: {r:?}"
    );
    let mut inconsistent = index.clone();
    inconsistent.reference.duration_seconds -= 1.0;
    inconsistent.index_digest = inconsistent.digest().unwrap();
    assert!(
        inconsistent.validate().is_err(),
        "a resealed index cannot misstate its PCM duration"
    );
    let mut corrupt = index.clone();
    corrupt.features[10][0] += 0.1;
    assert!(corrupt.validate().is_err());
}
#[test]
fn passive_anchors_integrate_with_shared_mapper_and_manual_conflicts_stay_visible() {
    use crate::tv_review_sync::*;
    let source = signal(24, 29);
    let index = SoundtrackIndex::from_pcm(spec(24), &source, &|| false).unwrap();
    let r = index
        .match_pcm(
            &source[4 * 8000..10 * 8000],
            capture(),
            &SoundtrackPolicy::default(),
            &|| false,
        )
        .unwrap();
    assert_eq!(r.state, SoundtrackMatchState::Matched);
    let anchor = r.candidates[0].anchor.clone();
    let sample = anchor.capture.sample_index;
    let mut request = SyncRequest {
        schema_version: 1,
        session_id: "session_synthetic".into(),
        target_digest: "b".repeat(64),
        movie: spec(24).movie,
        capture_clocks: vec![CaptureClock {
            clock_id: "mic".into(),
            sample_rate: 48000,
            first_sample: 0,
            last_sample: 48000 * 30,
            monotonic_origin_ns: 1000000000,
            monotonic_tolerance_ns: 2000000,
        }],
        anchors: vec![anchor.clone()],
        boundaries: vec![],
        policy: SyncPolicy::default(),
        consensus_segments: vec![],
    };
    let q = SyncQuery {
        clock_id: "mic".into(),
        start_sample: sample,
        end_sample: sample,
    };
    assert_eq!(
        request
            .map(std::slice::from_ref(&q), &|| false)
            .unwrap()
            .slices[0]
            .state,
        SyncState::Anchored
    );
    let mut manual = anchor;
    manual.source = SyncAnchorSource::ManualCue;
    manual.evidence_id = "manual_conflict".into();
    manual.candidate_group_id = None;
    manual.movie_pts += 90000 * 10;
    request.anchors.push(manual);
    assert_eq!(
        request.map(&[q], &|| false).unwrap().slices[0].state,
        SyncState::Conflicting
    );
}
#[test]
fn actual_generated_codec_resampling_remains_evidence_not_hardware_acceptance() {
    let dir = tempfile::tempdir().unwrap();
    let source = signal(24, 47);
    let index = SoundtrackIndex::from_pcm(spec(24), &source, &|| false).unwrap();
    let path = dir.path().join("query.wav");
    let mut wav = hound::WavWriter::create(
        &path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 8000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for x in &source[6 * 8000..12 * 8000] {
        wav.write_sample(*x).unwrap();
    }
    wav.finalize().unwrap();
    let encoded = dir.path().join("query.m4a");
    let decoded = dir.path().join("resampled.wav");
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(&path)
        .args(["-ac", "2", "-ar", "44100", "-c:a", "aac", "-b:a", "96k"])
        .arg(&encoded)
        .output()
        .unwrap();
    assert!(out.status.success());
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(encoded)
        .args(["-ac", "1", "-ar", "8000", "-c:a", "pcm_f32le"])
        .arg(&decoded)
        .output()
        .unwrap();
    assert!(out.status.success());
    let pcm = read_pcm8k_wav(&decoded).unwrap();
    let r = index
        .match_pcm(&pcm, capture(), &SoundtrackPolicy::default(), &|| false)
        .unwrap();
    assert_eq!(r.state, SoundtrackMatchState::Matched, "{r:?}");
    assert!(center_error(&r, 6.0, pcm.len() as f64 / 8000.0) < 0.1);
}

#[test]
fn final_movie_decoder_checks_bytes_and_retains_reference_window_offset() {
    let dir = tempfile::tempdir().unwrap();
    let source = signal(20, 63);
    let wav_path = dir.path().join("mixed.wav");
    let mut wav = hound::WavWriter::create(
        &wav_path,
        hound::WavSpec {
            channels: 2,
            sample_rate: 8000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for x in &source {
        wav.write_sample(*x).unwrap();
        wav.write_sample(*x * 0.8).unwrap();
    }
    wav.finalize().unwrap();
    let movie = dir.path().join("final.mp4");
    let out = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=160x90:r=30:d=20",
            "-i",
        ])
        .arg(wav_path)
        .args([
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-shortest",
        ])
        .arg(&movie)
        .output()
        .unwrap();
    assert!(out.status.success());
    let mut reference = spec(20);
    reference.movie.movie_digest = full_file_blake3(&movie, &|| false).unwrap().hex_digest;
    reference.start_seconds = 4.0;
    reference.duration_seconds = 12.0;
    let index = SoundtrackIndex::from_movie(reference.clone(), &movie, &|| false).unwrap();
    let report = index
        .match_pcm(
            &source[8 * 8000..14 * 8000],
            capture(),
            &SoundtrackPolicy::default(),
            &|| false,
        )
        .unwrap();
    assert_eq!(report.state, SoundtrackMatchState::Matched, "{report:?}");
    assert!(center_error(&report, 8.0, 6.0) < 0.1);
    reference.movie.movie_digest = "f".repeat(64);
    assert!(SoundtrackIndex::from_movie(reference, &movie, &|| false).is_err());
}

#[test]
fn repeated_positions_in_separate_reference_windows_keep_unique_evidence_and_one_group() {
    use crate::tv_review_sync::*;
    let block = signal(12, 91);
    let mut first = spec(24);
    first.duration_seconds = 12.0;
    let mut second = first.clone();
    second.start_seconds = 12.0;
    let a = SoundtrackIndex::from_pcm(first, &block, &|| false).unwrap();
    let b = SoundtrackIndex::from_pcm(second, &block, &|| false).unwrap();
    let query = &block[2 * 8000..8 * 8000];
    let aa = a
        .match_pcm(query, capture(), &SoundtrackPolicy::default(), &|| false)
        .unwrap();
    let bb = b
        .match_pcm(query, capture(), &SoundtrackPolicy::default(), &|| false)
        .unwrap();
    assert_eq!(aa.state, SoundtrackMatchState::Matched);
    assert_eq!(bb.state, SoundtrackMatchState::Matched);
    let x = aa.candidates[0].anchor.clone();
    let y = bb.candidates[0].anchor.clone();
    assert_ne!(x.evidence_id, y.evidence_id);
    assert_eq!(x.candidate_group_id, y.candidate_group_id);
    let sample = x.capture.sample_index;
    let r = SyncRequest {
        schema_version: 1,
        session_id: "session_synthetic".into(),
        target_digest: "b".repeat(64),
        movie: spec(24).movie,
        capture_clocks: vec![CaptureClock {
            clock_id: "mic".into(),
            sample_rate: 48000,
            first_sample: 0,
            last_sample: 48000 * 30,
            monotonic_origin_ns: 1000000000,
            monotonic_tolerance_ns: 2000000,
        }],
        anchors: vec![x, y],
        boundaries: vec![],
        policy: SyncPolicy::default(),
        consensus_segments: vec![],
    };
    let q = SyncQuery {
        clock_id: "mic".into(),
        start_sample: sample,
        end_sample: sample,
    };
    assert_eq!(
        r.map(&[q], &|| false).unwrap().slices[0].state,
        SyncState::Conflicting
    );
}
#[test]
fn unmatched_buried_excerpt_reports_its_true_position_as_best_peak() {
    let source = signal(36, 37);
    let index = SoundtrackIndex::from_pcm(spec(36), &source, &|| false).unwrap();
    let start = 8.32;
    let at = (start * 8000.0) as usize;
    let clean = source[at..at + 6 * 8000].to_vec();
    let noise = signal(6, 987);
    let query: Vec<f32> = clean.iter().zip(&noise).map(|(q, n)| q + n).collect();
    let r = index
        .match_pcm(&query, capture(), &SoundtrackPolicy::default(), &|| false)
        .unwrap();
    assert_eq!(r.state, SoundtrackMatchState::Unmatched, "{r:?}");
    assert!(r.candidates.is_empty());
    assert!(!r.best_peaks.is_empty() && r.best_peaks.len() <= MAX_BEST_PEAKS);
    let top = &r.best_peaks[0];
    // Query centre is start + 3 s; the search grid is one hop (16 ms).
    assert!(
        (top.movie_seconds - (start + 3.0)).abs() <= 0.016,
        "{top:?}"
    );
    assert_eq!(top.reference_offset_frames, 520);
    assert_eq!(Some(top.similarity), r.best_similarity);
    assert!(
        top.subwindow_similarities
            .iter()
            .all(|s| (-1.0..=1.0).contains(s))
    );
    for pair in r.best_peaks.windows(2) {
        assert!(pair[0].similarity >= pair[1].similarity);
        assert_ne!(
            pair[0].reference_offset_frames,
            pair[1].reference_offset_frames
        );
    }
    // Reports are deterministic, including the new diagnostics.
    assert_eq!(
        r,
        index
            .match_pcm(&query, capture(), &SoundtrackPolicy::default(), &|| false)
            .unwrap()
    );
}
#[test]
fn matched_query_best_peak_is_the_top_candidate() {
    let source = signal(36, 37);
    let index = SoundtrackIndex::from_pcm(spec(36), &source, &|| false).unwrap();
    let start = 8.32;
    let at = (start * 8000.0) as usize;
    let query = source[at..at + 6 * 8000].to_vec();
    let r = index
        .match_pcm(&query, capture(), &SoundtrackPolicy::default(), &|| false)
        .unwrap();
    assert_eq!(r.state, SoundtrackMatchState::Matched, "{r:?}");
    let candidate = &r.candidates[0];
    let top = &r.best_peaks[0];
    assert_eq!(
        top.reference_offset_frames,
        candidate.reference_offset_frames
    );
    assert_eq!(top.similarity, candidate.similarity);
    // §3 movie_seconds is the query centre, identical to the candidate anchor.
    let tps = spec(36).movie.ticks_per_second();
    let anchor_seconds = (candidate.anchor.movie_pts - spec(36).movie.origin_pts) as f64 / tps;
    assert!((top.movie_seconds - anchor_seconds).abs() <= 1.0 / tps);
    assert!((top.movie_seconds - (start + 3.0)).abs() <= 0.016);
}
#[test]
fn quiet_and_flat_queries_leave_best_peaks_empty() {
    let source = signal(36, 37);
    let index = SoundtrackIndex::from_pcm(spec(36), &source, &|| false).unwrap();
    let quiet = vec![0.0; 6 * 8000];
    let r = index
        .match_pcm(&quiet, capture(), &SoundtrackPolicy::default(), &|| false)
        .unwrap();
    assert_eq!(r.reason, "quiet_query");
    assert!(r.best_peaks.is_empty());
    let flat = vec![0.05f32; 6 * 8000];
    let r = index
        .match_pcm(&flat, capture(), &SoundtrackPolicy::default(), &|| false)
        .unwrap();
    assert_eq!(r.reason, "insufficient_spectral_variation", "{r:?}");
    assert!(r.best_peaks.is_empty());
    let json = serde_json::to_value(&r).unwrap();
    assert!(
        json.get("bestPeaks").is_none(),
        "empty peaks must be omitted"
    );
}
fn distinctive_policy(sim: f64, margin: f64, z: f64) -> SoundtrackPolicy {
    SoundtrackPolicy {
        distinctive: Some(DistinctivePolicy {
            minimum_similarity: sim,
            minimum_margin: margin,
            minimum_z_score: z,
        }),
        ..SoundtrackPolicy::default()
    }
}
/// Excerpt of `source` at 8.32 s, mixed with `noise_gain * other` (cosine about 0.6 at 0.5).
fn buried_query(source: &[f32], noise_gain: f32) -> Vec<f32> {
    let at = (8.32 * 8000.0) as usize;
    let noise = signal(6, 987);
    source[at..at + 6 * 8000]
        .iter()
        .zip(&noise)
        .map(|(c, n)| c + noise_gain * n)
        .collect()
}
#[test]
fn distinctive_low_similarity_unique_peak_matches_with_its_own_reason_and_confidence() {
    let source = signal(36, 37);
    let index = SoundtrackIndex::from_pcm(spec(36), &source, &|| false).unwrap();
    let query = buried_query(&source, 0.5);
    let r = index
        .match_pcm(
            &query,
            capture(),
            &distinctive_policy(0.5, 0.25, 6.0),
            &|| false,
        )
        .unwrap();
    assert_eq!(r.state, SoundtrackMatchState::Matched, "{r:?}");
    assert_eq!(r.reason, "distinctive_peak_low_similarity", "{r:?}");
    let best = r.best_similarity.unwrap();
    assert!(
        (0.5..0.8).contains(&best),
        "not a low-similarity peak: {best}"
    );
    let z = r
        .distinctiveness
        .expect("distinctiveness is reported when set");
    assert!(z >= 6.0, "{z}");
    assert!(center_error(&r, 8.32, 6.0) <= 0.1, "{r:?}");
    let candidate = &r.candidates[0];
    assert_eq!(
        candidate.anchor.confidence,
        (0.5 + candidate.peak_margin).min(1.0)
    );
    assert_eq!(candidate.anchor.source, SyncAnchorSource::PassiveSoundtrack);
    // Deterministic, including the new fields.
    assert_eq!(
        r,
        index
            .match_pcm(
                &query,
                capture(),
                &distinctive_policy(0.5, 0.25, 6.0),
                &|| { false }
            )
            .unwrap()
    );
}
#[test]
fn distinctive_none_keeps_the_same_query_unmatched_with_v1_bytes() {
    let source = signal(36, 37);
    let index = SoundtrackIndex::from_pcm(spec(36), &source, &|| false).unwrap();
    let query = buried_query(&source, 0.5);
    let r = index
        .match_pcm(&query, capture(), &SoundtrackPolicy::default(), &|| false)
        .unwrap();
    assert_eq!(r.state, SoundtrackMatchState::Unmatched, "{r:?}");
    assert_eq!(r.reason, "weak_or_non_distinctive");
    assert!(r.candidates.is_empty());
    assert_eq!(r.distinctiveness, None);
    let json = serde_json::to_value(&r).unwrap();
    assert!(json.get("distinctiveness").is_none(), "{json}");
    assert!(json["policy"].get("distinctive").is_none(), "{json}");
    // Turning the policy on changes only what it is allowed to change.
    let on = index
        .match_pcm(
            &query,
            capture(),
            &distinctive_policy(0.5, 0.25, 6.0),
            &|| false,
        )
        .unwrap();
    assert_eq!(on.best_peaks, r.best_peaks);
    assert_eq!(on.best_similarity, r.best_similarity);
}
#[test]
fn distinctive_thresholds_are_each_required() {
    let source = signal(36, 37);
    let index = SoundtrackIndex::from_pcm(spec(36), &source, &|| false).unwrap();
    let query = buried_query(&source, 0.5);
    for policy in [
        distinctive_policy(0.7, 0.25, 6.0),
        distinctive_policy(0.5, 0.25, 20.0),
    ] {
        let r = index
            .match_pcm(&query, capture(), &policy, &|| false)
            .unwrap();
        assert_ne!(r.state, SoundtrackMatchState::Matched, "{r:?}");
        assert!(r.candidates.is_empty(), "{r:?}");
        assert_ne!(r.reason, "distinctive_peak_low_similarity");
    }
    // Repeated content at the same gain must not be accepted. At this gain the
    // two identical copies fail temporal support, so neither becomes a candidate.
    let block = signal(12, 91);
    let mut repeated = block.clone();
    repeated.extend(&block);
    let rep_index = SoundtrackIndex::from_pcm(spec(24), &repeated, &|| false).unwrap();
    let rep_query: Vec<f32> = repeated[2 * 8000..8 * 8000]
        .iter()
        .zip(&signal(6, 987))
        .map(|(c, n)| c + 0.5 * n)
        .collect();
    let rep = rep_index
        .match_pcm(
            &rep_query,
            capture(),
            &distinctive_policy(0.5, 0.25, 3.0),
            &|| false,
        )
        .unwrap();
    assert_ne!(rep.state, SoundtrackMatchState::Matched, "{rep:?}");
    assert!(rep.candidates.is_empty(), "{rep:?}");
}
#[test]
fn distinctive_path_never_skips_temporal_support() {
    // The raw best peak clears all three distinctive thresholds (similarity
    // 0.52, margin 0.26, z 4.18 with z threshold 3.0), but the last third of
    // the query is unsupported, so the report must stay unmatched.
    let source = signal(36, 37);
    let index = SoundtrackIndex::from_pcm(spec(36), &source, &|| false).unwrap();
    let at = (8.32 * 8000.0) as usize;
    let clean = source[at..at + 6 * 8000].to_vec();
    let noise = signal(6, 555);
    let query: Vec<f32> = (0..6 * 8000)
        .map(|i| {
            if i < 3 * 8000 {
                clean[i]
            } else {
                clean[i] + 3.0 * noise[i]
            }
        })
        .collect();
    let r = index
        .match_pcm(
            &query,
            capture(),
            &distinctive_policy(0.5, 0.25, 3.0),
            &|| false,
        )
        .unwrap();
    assert_eq!(r.state, SoundtrackMatchState::Unmatched, "{r:?}");
    assert_eq!(r.reason, "insufficient_temporal_support", "{r:?}");
    assert!(r.candidates.is_empty());
    assert!(r.distinctiveness.unwrap() >= 3.0);
}
#[test]
fn distinctive_path_rejects_unrelated_and_flat_evidence() {
    let source = signal(36, 37);
    let index = SoundtrackIndex::from_pcm(spec(36), &source, &|| false).unwrap();
    let policy = distinctive_policy(0.5, 0.25, 6.0);
    let unrelated = signal(6, 878123);
    let r = index
        .match_pcm(&unrelated, capture(), &policy, &|| false)
        .unwrap();
    assert_ne!(r.state, SoundtrackMatchState::Matched, "{r:?}");
    assert!(r.candidates.is_empty(), "{r:?}");
    assert!(r.distinctiveness.is_some());
    let flat = vec![0.05f32; 6 * 8000];
    let r = index
        .match_pcm(&flat, capture(), &policy, &|| false)
        .unwrap();
    assert_eq!(r.reason, "insufficient_spectral_variation", "{r:?}");
    assert_eq!(r.distinctiveness, None);
    assert!(r.candidates.is_empty());
}
#[test]
fn robust_z_is_median_normalised_and_none_without_spread() {
    let z = robust_z(&[1.0, 2.0, 3.0, 4.0, 100.0]).unwrap();
    assert!((z - (100.0 - 3.0) / (1.4826 * 1.0)).abs() < 1e-12, "{z}");
    // Majority-equal scores have MAD 0, so z is None and fails the z test.
    assert_eq!(robust_z(&[0.1, 0.1, 0.1, 0.1, 0.9]), None);
    assert_eq!(robust_z(&[0.4; 9]), None);
    assert_eq!(robust_z(&[]), None);
    assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), Some(2.5));
}
/// Deterministic loud, speech-like burst used only inside the masked span.
fn loud_burst(len: usize, seed: u64) -> Vec<f32> {
    let mut state = seed;
    (0..len)
        .map(|i| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let t = i as f64 / PCM_RATE as f64;
            let noise = ((state >> 32) as u32 as f64 / u32::MAX as f64 - 0.5) * 2.0;
            (0.9 * noise * (std::f64::consts::TAU * 700.0 * t).sin()) as f32
        })
        .collect()
}
fn mask_span(seconds_from: f64, seconds_to: f64) -> Vec<(u64, u64)> {
    vec![(
        (seconds_from * PCM_RATE as f64) as u64,
        (seconds_to * PCM_RATE as f64) as u64,
    )]
}
#[test]
fn mask_weights_zero_only_frames_whose_own_hop_overlaps_the_mask() {
    let len = 6 * PCM_RATE as usize;
    let weights = mask_weights(len, 0, PCM_RATE, &mask_span(2.0, 3.0));
    assert_eq!(weights.len(), feature_frames(len));
    assert!(weights.iter().all(|w| *w == 0.0 || *w == 1.0));
    // Frame n owns samples n*HOP+192..(n+1)*HOP+192 (its centre hop). Against
    // the mask [16000, 24000) that is frames 123..=185 (exact, no slack). The
    // frames 0..=122 and 186.. that only share support with the mask keep weight 1.
    let zeros: Vec<usize> = (0..weights.len()).filter(|&n| weights[n] == 0.0).collect();
    assert_eq!(zeros.first(), Some(&123));
    assert_eq!(zeros.last(), Some(&185));
    assert_eq!(zeros.len(), 185 - 123 + 1);
    assert!(weights[..123].iter().all(|w| *w == 1.0));
    assert!(weights[186..].iter().all(|w| *w == 1.0));
    // A 48 kHz capture with the same span maps to the identical frame weights.
    let native = mask_weights(len, 0, 48000, &[(2 * 48000, 3 * 48000)]);
    assert_eq!(native, weights);
}
#[test]
fn masked_span_noise_cannot_change_similarity_and_mask_digest_tracks_weights() {
    let source = signal(36, 37);
    let index = SoundtrackIndex::from_pcm(spec(36), &source, &|| false).unwrap();
    let start = 8.32;
    let clean = source[(start * 8000.0) as usize..(start * 8000.0) as usize + 6 * 8000].to_vec();
    let mask = mask_span(2.0, 3.0);
    let weights = mask_weights(clean.len(), 0, PCM_RATE, &mask);
    let mut reports = Vec::new();
    for seed in [5_u64, 991] {
        let mut query = clean.clone();
        for (dst, b) in query[16000..24000].iter_mut().zip(loud_burst(8000, seed)) {
            *dst = b;
        }
        reports.push(
            index
                .match_pcm_weighted(
                    &query,
                    capture(),
                    &SoundtrackPolicy::default(),
                    Some(&weights),
                    &|| false,
                )
                .unwrap(),
        );
    }
    let (a, b) = (&reports[0], &reports[1]);
    assert_ne!(a.query_pcm_digest, b.query_pcm_digest);
    assert!(a.best_similarity.is_some(), "{a:?}");
    assert_eq!(a.best_similarity, b.best_similarity);
    assert_eq!(a.runner_up_similarity, b.runner_up_similarity);
    assert_eq!(a.best_peaks, b.best_peaks);
    assert_eq!(a.state, b.state);
    assert_eq!(a.reason, b.reason);
    assert_eq!(a.mask_digest, b.mask_digest);
    assert_eq!(
        a.mask_digest.as_deref(),
        Some(weights_digest(&weights).as_str())
    );
    // Changing only the mask changes mask_digest.
    let other = mask_weights(clean.len(), 0, PCM_RATE, &mask_span(4.0, 5.0));
    let c = index
        .match_pcm_weighted(
            &clean,
            capture(),
            &SoundtrackPolicy::default(),
            Some(&other),
            &|| false,
        )
        .unwrap();
    assert_ne!(c.mask_digest, a.mask_digest);
    assert_eq!(
        c.mask_digest.as_deref(),
        Some(weights_digest(&other).as_str())
    );
}
#[test]
fn masked_query_with_too_little_unmasked_audio_is_masked_insufficient() {
    let source = signal(36, 37);
    let index = SoundtrackIndex::from_pcm(spec(36), &source, &|| false).unwrap();
    let start = 8.32;
    let clean = source[(start * 8000.0) as usize..(start * 8000.0) as usize + 6 * 8000].to_vec();
    // Under 2 s unmasked in total: only 1.5 s survives.
    let total_masked = mask_weights(clean.len(), 0, PCM_RATE, &mask_span(1.5, 6.0));
    let r = index
        .match_pcm_weighted(
            &clean,
            capture(),
            &SoundtrackPolicy::default(),
            Some(&total_masked),
            &|| false,
        )
        .unwrap();
    assert_eq!(r.reason, "masked_insufficient");
    assert_eq!(r.state, SoundtrackMatchState::Unmatched);
    assert!(r.candidates.is_empty() && r.best_peaks.is_empty());
    assert_eq!(
        r.mask_digest.as_deref(),
        Some(weights_digest(&total_masked).as_str())
    );
    // Enough in total, but the final third has no unmasked frames.
    let frames = feature_frames(clean.len());
    let mut last_third_masked = vec![1.0f32; frames];
    for w in &mut last_third_masked[2 * frames / 3..] {
        *w = 0.0;
    }
    let r = index
        .match_pcm_weighted(
            &clean,
            capture(),
            &SoundtrackPolicy::default(),
            Some(&last_third_masked),
            &|| false,
        )
        .unwrap();
    assert_eq!(r.reason, "masked_insufficient");
    assert_eq!(r.state, SoundtrackMatchState::Unmatched);
    // 34 unmasked frames in the final third: the last 2 read silenced samples
    // through their FFT window, leaving 32 effective frames (0.512 s), which
    // passes the per-third rule.
    let mut enough_third = vec![1.0f32; frames];
    for w in &mut enough_third[2 * frames / 3 + 34..] {
        *w = 0.0;
    }
    let r = index
        .match_pcm_weighted(
            &clean,
            capture(),
            &SoundtrackPolicy::default(),
            Some(&enough_third),
            &|| false,
        )
        .unwrap();
    assert_ne!(r.reason, "masked_insufficient", "{r:?}");
}
#[test]
fn all_ones_weights_match_unmasked_scores_and_absent_mask_is_v1_identical() {
    let source = signal(36, 37);
    let index = SoundtrackIndex::from_pcm(spec(36), &source, &|| false).unwrap();
    let start = 8.32;
    let query = source[(start * 8000.0) as usize..(start * 8000.0) as usize + 6 * 8000].to_vec();
    let plain = index
        .match_pcm(&query, capture(), &SoundtrackPolicy::default(), &|| false)
        .unwrap();
    let absent = index
        .match_pcm_weighted(
            &query,
            capture(),
            &SoundtrackPolicy::default(),
            None,
            &|| false,
        )
        .unwrap();
    assert_eq!(plain, absent);
    assert_eq!(plain.mask_digest, None);
    let json = serde_json::to_string(&plain).unwrap();
    assert!(!json.contains("maskDigest"), "{json}");
    let ones = vec![1.0f32; feature_frames(query.len())];
    let masked = index
        .match_pcm_weighted(
            &query,
            capture(),
            &SoundtrackPolicy::default(),
            Some(&ones),
            &|| false,
        )
        .unwrap();
    assert_eq!(masked.best_similarity, plain.best_similarity);
    assert_eq!(masked.runner_up_similarity, plain.runner_up_similarity);
    assert_eq!(masked.best_peaks, plain.best_peaks);
    assert_eq!(masked.state, plain.state);
    assert_eq!(masked.reason, plain.reason);
    assert_eq!(masked.candidates, plain.candidates);
    assert!(masked.mask_digest.is_some());
}
#[test]
fn malformed_speech_mask_weights_are_rejected() {
    let source = signal(36, 37);
    let index = SoundtrackIndex::from_pcm(spec(36), &source, &|| false).unwrap();
    let start = 8.32;
    let query = source[(start * 8000.0) as usize..(start * 8000.0) as usize + 6 * 8000].to_vec();
    let frames = feature_frames(query.len());
    let short = vec![1.0f32; frames - 1];
    let out_of_range = vec![1.5f32; frames];
    let nan = vec![f32::NAN; frames];
    for bad in [&short, &out_of_range, &nan] {
        assert!(
            index
                .match_pcm_weighted(
                    &query,
                    capture(),
                    &SoundtrackPolicy::default(),
                    Some(bad),
                    &|| false,
                )
                .is_err()
        );
    }
}
#[test]
fn effective_weights_drop_frames_that_read_silenced_samples() {
    let len = 6 * PCM_RATE as usize;
    let mask = mask_weights(len, 0, PCM_RATE, &mask_span(2.0, 3.0));
    let eff = effective_weights(len, &mask);
    assert_eq!(eff.len(), mask.len());
    // Masked frames stay zero; nothing gains weight.
    assert!(eff.iter().zip(&mask).all(|(e, m)| *e <= *m));
    // Silenced samples are those owned by frames 123..=185, i.e. [15936+192, ...).
    // Frame n reads [(n-8)*HOP, n*HOP+FFT): the last frame before the mask that
    // reads no silenced sample, and the first after it, bound the zero run.
    let silenced_lo = 123 * HOP + OWN_OFFSET;
    let silenced_hi = 186 * HOP + OWN_OFFSET;
    for (n, w) in eff.iter().enumerate() {
        let lo = n.saturating_sub(WARMUP - 1) * HOP;
        let hi = (n * HOP + FFT).min(len);
        let reads_silence = lo < silenced_hi && hi > silenced_lo;
        assert_eq!(*w == 0.0, reads_silence, "frame {n}");
    }
    // Frames well after the mask (lag history clear) keep their weight.
    assert_eq!(eff[200], 1.0);
    assert_eq!(
        eff[193], 0.0,
        "lag-6 history of frame 193 still reads silence"
    );
    // No zero in the mask means the mask is returned unchanged.
    let ones = vec![1.0_f32; mask.len()];
    assert_eq!(effective_weights(len, &ones), ones);
}
#[test]
fn clean_query_with_mask_boundaries_keeps_its_true_peak_near_one() {
    // Boundary frames next to silenced spans must not be scored as onsets: a
    // query that IS the reference, with three masked spans of interfering
    // noise, must still score close to an unmasked clean query.
    let source = signal(36, 37);
    let index = SoundtrackIndex::from_pcm(spec(36), &source, &|| false).unwrap();
    let start = 8.32;
    let s0 = (start * 8000.0) as usize;
    let clean = source[s0..s0 + 6 * 8000].to_vec();
    let mut mask = Vec::new();
    for k in 0..3 {
        mask.extend(mask_span(0.5 + 2.0 * k as f64, 1.1 + 2.0 * k as f64));
    }
    let weights = mask_weights(clean.len(), 0, PCM_RATE, &mask);
    let mut query = clean.clone();
    for (k, &(a, b)) in mask.iter().enumerate() {
        for (dst, x) in query[a as usize..b as usize]
            .iter_mut()
            .zip(loud_burst((b - a) as usize, 40 + k as u64))
        {
            *dst = x;
        }
    }
    let r = index
        .match_pcm_weighted(
            &query,
            capture(),
            &SoundtrackPolicy::default(),
            Some(&weights),
            &|| false,
        )
        .unwrap();
    let best = r.best_similarity.unwrap();
    assert!(best > 0.95, "masked clean query scored {best}: {r:?}");
    assert_eq!(r.state, SoundtrackMatchState::Matched, "{r:?}");
    assert!(center_error(&r, start, 6.0) < 0.05);
}
