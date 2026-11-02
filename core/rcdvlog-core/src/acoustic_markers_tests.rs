use super::*;

const IDENTITY: &str = "57874921a84c987b85fedff5273180a9202be05b590702fe6234b2cab20d1638";

/// Deterministic pseudo-random noise in [-1, 1].
fn noise(seed: u64, n: usize) -> Vec<f32> {
    let mut state = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (0..n)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) as f64 / (1u64 << 31) as f64 * 2.0 - 1.0) as f32
        })
        .collect()
}

/// A stand-in soundtrack: chords plus noise at a realistic level.
fn soundtrack(seconds: f64) -> Vec<f32> {
    let n = (seconds * 48_000.0) as usize;
    let hiss = noise(7, n);
    (0..n)
        .map(|i| {
            let t = i as f64 / 48_000.0;
            let music = 0.12 * (2.0 * PI * 220.0 * t).sin()
                + 0.08 * (2.0 * PI * 330.0 * t).sin()
                + 0.05 * (2.0 * PI * 1760.0 * t).sin() * (2.0 * PI * 0.5 * t).sin();
            (music as f32) + 0.02 * hiss[i]
        })
        .collect()
}

/// Two talkers: amplitude-modulated noise bursts in the speech band, at 16 kHz.
fn conversation(seconds: f64, level: f32) -> Vec<f32> {
    let n = (seconds * 16_000.0) as usize;
    let raw = noise(11, n);
    let mut previous = 0.0_f32;
    (0..n)
        .map(|i| {
            let t = i as f64 / 16_000.0;
            // Crude 1-pole low-pass to put energy below ~3 kHz, syllable-rate envelope.
            previous = 0.7 * previous + 0.3 * raw[i];
            let envelope = (0.5 + 0.5 * (2.0 * PI * 4.0 * t).sin()).powi(2)
                * if (t as u64).is_multiple_of(3) {
                    0.2
                } else {
                    1.0
                };
            level * previous * envelope as f32 * 2.5
        })
        .collect()
}

fn mixed_movie(seconds: f64, identity: &str) -> (Vec<f32>, MarkerPlan) {
    let plan = plan_markers(identity, seconds, 30.0, 2.0, -12.0).unwrap();
    let mut audio = soundtrack(seconds);
    mix_markers(&mut audio, 1, &plan).unwrap();
    (audio, plan)
}

/// What a laptop hears: the movie at `gain`, delayed by `offset` seconds,
/// downsampled to 16 kHz, plus conversation and room noise.
fn recording(movie: &[f32], offset: f64, gain: f32, talk_level: f32) -> Vec<f32> {
    let movie16 = resample(movie, 48_000, 16_000);
    let lead = (offset * 16_000.0) as usize;
    let total = lead + movie16.len() + 16_000;
    let talk = conversation(total as f64 / 16_000.0 + 1.0, talk_level);
    let room = noise(23, total);
    (0..total)
        .map(|i| {
            let tv = if i >= lead && i - lead < movie16.len() {
                movie16[i - lead] * gain
            } else {
                0.0
            };
            tv + talk[i] + 0.01 * room[i]
        })
        .collect()
}

#[test]
fn payload_round_trips_and_crc_rejects_every_single_bit_flip() {
    let payload = MarkerPayload {
        version: 1,
        export_key: export_key_for(IDENTITY),
        movie_deciseconds: 12_345,
    };
    let bits = payload.to_bits();
    assert_eq!(bits.len(), 56);
    assert_eq!(MarkerPayload::from_bits(&bits), Some(payload));
    for i in 0..bits.len() {
        let mut flipped = bits.clone();
        flipped[i] = !flipped[i];
        assert_eq!(MarkerPayload::from_bits(&flipped), None, "bit {i}");
    }
    assert_ne!(export_key_for(IDENTITY), export_key_for("another digest"));
}

#[test]
fn plan_places_markers_on_a_tenth_second_grid_inside_the_movie() {
    let plan = plan_markers(IDENTITY, 95.0, 30.0, 2.0, -12.0).unwrap();
    assert_eq!(plan.marker_seconds, vec![2.0, 32.0, 62.0, 92.0]);
    assert!(
        plan.marker_seconds
            .iter()
            .all(|t| t + marker_duration_seconds() <= 95.0)
    );
    assert!(plan_markers(IDENTITY, 95.0, 1.0, 2.0, -12.0).is_err());
    assert!(plan_markers(IDENTITY, 95.0, 30.0, 2.0, 0.0).is_err());
    assert!(
        plan_markers(IDENTITY, 1.0, 30.0, 2.0, -12.0)
            .unwrap()
            .marker_seconds
            .is_empty()
    );
}

#[test]
fn clean_round_trip_yields_exact_movie_timestamps() {
    let (movie, plan) = mixed_movie(95.0, IDENTITY);
    let report = detect_markers(&movie, 48_000, plan.export_key).unwrap();
    let times: Vec<f64> = report.anchors.iter().map(|a| a.movie_seconds).collect();
    assert_eq!(times, plan.marker_seconds);
    for anchor in &report.anchors {
        assert!(
            (anchor.recording_seconds - anchor.movie_seconds).abs() < 0.0005,
            "{anchor:?}"
        );
    }
    assert!(report.rejected.is_empty());
}

#[test]
fn markers_survive_conversation_room_noise_and_a_volume_drop() {
    let (movie, plan) = mixed_movie(95.0, IDENTITY);
    let mut heard = recording(&movie, 3.25, 0.5, 0.15);
    // Someone turns the TV down after 45 s of recording.
    let cut = (45.0 * 16_000.0) as usize;
    heard[cut..].iter_mut().for_each(|s| *s *= 0.4);
    let report = detect_markers(&heard, 16_000, plan.export_key).unwrap();
    let times: Vec<f64> = report.anchors.iter().map(|a| a.movie_seconds).collect();
    assert_eq!(
        times, plan.marker_seconds,
        "rejected: {:?}",
        report.rejected
    );
    for anchor in &report.anchors {
        assert!((anchor.offset_seconds - 3.25).abs() < 0.002, "{anchor:?}");
        assert!(anchor.mean_bit_confidence > 0.2);
    }
}

#[test]
fn a_different_export_is_rejected_not_anchored() {
    let (movie, plan) = mixed_movie(65.0, "another review target");
    let heard = recording(&movie, 1.0, 0.5, 0.1);
    let report = detect_markers(&heard, 16_000, export_key_for(IDENTITY)).unwrap();
    assert!(report.anchors.is_empty());
    assert_eq!(
        report
            .rejected
            .iter()
            .filter(|r| r.reason == "exportKeyMismatch")
            .count(),
        plan.marker_seconds.len()
    );
}

#[test]
fn conversation_and_music_without_markers_produce_no_anchors() {
    let movie = soundtrack(120.0);
    let heard = recording(&movie, 2.0, 0.6, 0.3);
    let report = detect_markers(&heard, 16_000, export_key_for(IDENTITY)).unwrap();
    assert!(report.anchors.is_empty(), "{:?}", report.anchors);
    // Pure noise too.
    let report = detect_markers(&noise(99, 16_000 * 60), 16_000, export_key_for(IDENTITY)).unwrap();
    assert!(report.anchors.is_empty());
}

#[test]
fn a_rewatch_produces_inconsistent_offsets_instead_of_a_smooth_guess() {
    let (movie, plan) = mixed_movie(95.0, IDENTITY);
    // Watch 0–40 s, then seek back to 25 s and watch to the end.
    let first = &movie[..(40.0 * 48_000.0) as usize];
    let second = &movie[(25.0 * 48_000.0) as usize..];
    let mut played = first.to_vec();
    played.extend_from_slice(second);
    let heard = recording(&played, 1.0, 0.5, 0.1);
    let report = detect_markers(&heard, 16_000, plan.export_key).unwrap();
    let times: Vec<f64> = report.anchors.iter().map(|a| a.movie_seconds).collect();
    assert_eq!(times, vec![2.0, 32.0, 32.0, 62.0, 92.0]);
    let offsets: Vec<f64> = report
        .anchors
        .iter()
        .map(|a| (a.offset_seconds * 10.0).round() / 10.0)
        .collect();
    assert_eq!(offsets, vec![1.0, 1.0, 16.0, 16.0, 16.0]);
}

#[test]
fn a_truncated_recording_reports_the_cut_marker() {
    let (movie, plan) = mixed_movie(40.0, IDENTITY);
    let cut = &movie[..(33.0 * 48_000.0) as usize];
    let report = detect_markers(cut, 48_000, plan.export_key).unwrap();
    assert_eq!(report.anchors.len(), 1);
    assert!(report.rejected.iter().any(|r| r.reason == "truncated"));
}

#[test]
fn mixing_never_clips_and_reports_uniform_gain() {
    let plan = plan_markers(IDENTITY, 10.0, 30.0, 2.0, -6.0).unwrap();
    let mut loud = vec![0.9_f32; 48_000 * 10 * 2];
    let report = mix_markers(&mut loud, 2, &plan).unwrap();
    assert!(report.output_peak <= 0.98 + 1e-6);
    assert!(report.gain_applied_db < 0.0);
    assert_eq!(report.markers_written, 1);
    let mut silent = vec![0.0_f32; 48_000 * 10];
    assert!(mix_markers(&mut silent, 1, &plan).unwrap().input_silent);
}

/// Real lossy codec in the loop when FFmpeg is available: AAC at 96 kb/s.
#[test]
fn markers_survive_an_aac_transcode() {
    if std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_err()
    {
        eprintln!("ffmpeg unavailable; skipping AAC round trip");
        return;
    }
    let (movie, plan) = mixed_movie(65.0, IDENTITY);
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("in.f32");
    let aac = dir.path().join("marked.m4a");
    let back = dir.path().join("out.f32");
    std::fs::write(
        &raw,
        movie
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect::<Vec<u8>>(),
    )
    .unwrap();
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-v", "error", "-y", "-f", "f32le", "-ar", "48000", "-ac", "1", "-i",
        ])
        .arg(&raw)
        .args(["-c:a", "aac", "-b:a", "96k"])
        .arg(&aac)
        .status()
        .unwrap();
    assert!(status.success());
    let status = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(&aac)
        .args(["-f", "f32le", "-ar", "16000", "-ac", "1"])
        .arg(&back)
        .status()
        .unwrap();
    assert!(status.success());
    let bytes = std::fs::read(&back).unwrap();
    let decoded: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let report = detect_markers(&decoded, 16_000, plan.export_key).unwrap();
    let times: Vec<f64> = report.anchors.iter().map(|a| a.movie_seconds).collect();
    assert_eq!(times, plan.marker_seconds);
    // AAC adds a fixed priming delay; anchors stay mutually consistent.
    let first = report.anchors[0].offset_seconds;
    assert!(
        report
            .anchors
            .iter()
            .all(|a| (a.offset_seconds - first).abs() < 0.001)
    );
    assert!(first.abs() < 0.05, "priming delay {first}");
}

fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok()
        && std::process::Command::new("ffprobe")
            .arg("-version")
            .output()
            .is_ok()
}

/// A synthetic 40 s H.264/AAC master and a handoff that verifies it.
fn synthetic_master(dir: &std::path::Path) -> (std::path::PathBuf, crate::ReviewTargetHandoff) {
    let movie = dir.join("master.mp4");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=30:duration=40",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=330:sample_rate=48000:duration=40",
            "-ac",
            "2",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-shortest",
        ])
        .arg(&movie)
        .status()
        .unwrap();
    assert!(status.success());
    let identity = super::file_identity(&movie).unwrap();
    let mut handoff: crate::ReviewTargetHandoff =
        serde_json::from_str(include_str!("../tests/fixtures/review-target/v1.json")).unwrap();
    let t = &mut handoff.target;
    t.movie.blake3 = identity.blake3;
    t.movie.byte_length = identity.byte_length;
    t.movie.duration_seconds = 40.0;
    t.movie.time_base_numerator = 1;
    t.movie.time_base_denominator = 30;
    t.movie.origin_pts = 0;
    t.movie.video_frame_count = 1200;
    t.cuts.truncate(1);
    t.cuts[0].movie_in_pts = 0;
    t.cuts[0].movie_out_pts = 1200;
    t.cuts[0].output_frame_count = 1200;
    t.youtube_video_id = None;
    handoff.target_digest = crate::ReviewTargetHandoff::snapshot_digest(&handoff.target).unwrap();
    handoff.validate().unwrap();
    (movie, handoff)
}

#[test]
fn marked_copy_preserves_the_master_and_picture_and_verifies_its_own_markers() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg unavailable; skipping marked copy export");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (movie, handoff) = synthetic_master(dir.path());
    let before = super::file_identity(&movie).unwrap();
    let output = dir.path().join("review-copy.mp4");
    let request = MarkedCopyRequest {
        handoff: &handoff,
        movie: &movie,
        output: &output,
        interval_seconds: 15.0,
        first_marker_seconds: 2.0,
        level_dbfs: -14.0,
        ffmpeg: std::path::Path::new("ffmpeg"),
        ffprobe: std::path::Path::new("ffprobe"),
    };
    let manifest = export_marked_review_copy(&request).unwrap();
    assert_eq!(manifest.plan.marker_seconds, vec![2.0, 17.0, 32.0]);
    assert_eq!(manifest.verification.detected_markers, 3);
    assert!(manifest.verification.picture_stream_matches_master);
    assert_eq!(manifest.master, before);
    assert_eq!(
        super::file_identity(&movie).unwrap(),
        before,
        "the master is never modified"
    );
    assert_ne!(manifest.marked.blake3, manifest.master.blake3);
    assert_eq!(manifest.marked, super::file_identity(&output).unwrap());
    assert_eq!(manifest.target_digest, handoff.target_digest);
    let text = serde_json::to_string(&manifest).unwrap();
    assert!(
        !text.contains(dir.path().to_str().unwrap()),
        "manifest is path-free"
    );

    // Create-only, and a movie that is not the verified master is refused.
    assert!(export_marked_review_copy(&request).is_err());
    let other = dir.path().join("other.mp4");
    let wrong = MarkedCopyRequest {
        movie: &output,
        output: &other,
        ..request.clone()
    };
    assert!(
        export_marked_review_copy(&wrong)
            .unwrap_err()
            .to_string()
            .contains("do not match")
    );
    assert!(!other.exists());
}

#[test]
fn detections_feed_the_shared_mapper_and_a_rewatch_is_not_interpolated() {
    use crate::tv_review_sync::*;
    let (movie, plan) = mixed_movie(95.0, IDENTITY);
    // Watch 0–40 s, seek back to 25 s, watch to the end; mic starts 1 s early.
    let mut played = movie[..(40.0 * 48_000.0) as usize].to_vec();
    played.extend_from_slice(&movie[(25.0 * 48_000.0) as usize..]);
    let heard = recording(&played, 1.0, 0.5, 0.1);
    let report = detect_markers(&heard, 16_000, plan.export_key).unwrap();
    let capture = CaptureClock {
        clock_id: "mic".into(),
        sample_rate: 16_000,
        first_sample: 0,
        last_sample: heard.len() as u64,
        monotonic_origin_ns: 1_000_000_000,
        monotonic_tolerance_ns: 2_000_000,
    };
    let clock = SyncMovieClock {
        movie_digest: "c".repeat(64),
        time_base_numerator: 1,
        time_base_denominator: 90_000,
        origin_pts: 90_000,
        duration_pts: 95 * 90_000,
    };
    let anchors = to_sync_anchors(&report, &capture, &clock).unwrap();
    assert_eq!(anchors.len(), 5);
    assert!(
        anchors
            .iter()
            .all(|a| a.source == SyncAnchorSource::AcousticTimecode)
    );
    assert_eq!(anchors[0].movie_pts, 90_000 + 2 * 90_000);
    // Distinct evidence even for the same movie time seen twice.
    assert_ne!(anchors[1].evidence_id, anchors[2].evidence_id);
    let policy = SyncPolicy {
        max_anchor_gap_seconds: 31.0,
        ..Default::default()
    };
    let request = SyncRequest {
        schema_version: 1,
        session_id: "session_synthetic".into(),
        target_digest: "b".repeat(64),
        movie: clock,
        capture_clocks: vec![capture],
        anchors: anchors.clone(),
        boundaries: vec![],
        policy,
    };
    let at = |seconds: f64| SyncQuery {
        clock_id: "mic".into(),
        start_sample: (seconds * 16_000.0) as u64,
        end_sample: (seconds * 16_000.0) as u64,
    };
    let first = anchors[0].capture.sample_index as f64 / 16_000.0;
    let mapped = request
        .map(&[at(18.0), at(36.0), at(first)], &|| false)
        .unwrap();
    // Before the seek: estimated between consistent markers (3 s and 33 s).
    assert_eq!(
        mapped.slices[0].state,
        SyncState::Estimated,
        "{:?}",
        mapped.slices[0]
    );
    assert_eq!(mapped.slices.last().unwrap().state, SyncState::Anchored);
    let estimate = &mapped.slices[0].candidates[0];
    let movie_seconds = (estimate.start_pts - 90_000.0) / 90_000.0;
    assert!((movie_seconds - 17.0).abs() < 0.01, "{movie_seconds}");
    // Spanning the seek (33 s → 48 s shows movie 32 s twice): never a 1x estimate.
    assert!(
        matches!(
            mapped.slices[1].state,
            SyncState::Conflicting | SyncState::Unknown
        ),
        "{:?}",
        mapped.slices[1]
    );
}
