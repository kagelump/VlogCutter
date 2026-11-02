//! V01-T002 independent peak-diagnostics oracle (ARCHITECTURE section 3).
//!
//! Synthetic audio here comes from a generator separate from the implementer's
//! tests: a SplitMix64 stream feeding a random chirp train plus Kellet-style
//! pink noise. Fixtures are real 8 kHz mono float WAV files in a tempdir and
//! run through the public session plan (`run_batch`, `evidence_page`). Tests
//! also call the public `SoundtrackIndex::from_pcm` / `match_pcm` reports.
//!
//! Oracle items: (1) buried unmatched excerpt reports its true centre, (2)
//! matched best peak equals the top candidate, (3) overlapping references
//! dedupe one movie position, (4) quiet query leaves best_peaks empty, and
//! (5) wrong or ambiguous evidence never becomes a matched anchor.
use rcdvlog_core::{
    full_file_blake3,
    tv_review_sync::{CaptureClock, SyncMovieClock, SyncPolicy, SyncRequest},
    tv_soundtrack::{
        MAX_BEST_PEAKS, PEAK_MERGE_SECONDS, SoundtrackCapture, SoundtrackIndex,
        SoundtrackMatchReport, SoundtrackMatchState, SoundtrackPolicy, SoundtrackReferenceSpec,
    },
    tv_soundtrack_session::{
        SessionAudioFile, SessionAudioSegment, SessionBatch, SessionEvidencePage,
        SoundtrackSessionPlan, SoundtrackSessionSpec,
    },
};
use std::path::{Path, PathBuf};

const RATE: usize = 8000;
const TICKS_PER_SECOND: u32 = 90_000;
const MOVIE_SECONDS: usize = 60;
/// True excerpt start in the movie. Query centre is EXCERPT_START + 3 s.
const EXCERPT_START: f64 = 30.0;
const EXCERPT_CENTRE: f64 = EXCERPT_START + 3.0;
const TOLERANCE_SECONDS: f64 = 0.016;

/// SplitMix64 stream. Independent of the implementer's LCG fixtures.
struct Mix(u64);
impl Mix {
    fn seeded(base: u64, salt: u64) -> Self {
        Self(base ^ salt.wrapping_mul(0xD1B5_4A32_D192_ED03))
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }
}

/// Kellet-style pink noise driven by the given stream.
fn pink(rng: &mut Mix, n: usize) -> Vec<f64> {
    let (mut b0, mut b1, mut b2, mut b3, mut b4, mut b5, mut b6) =
        (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    (0..n)
        .map(|_| {
            let w = rng.range(-1.0, 1.0);
            b0 = 0.99886 * b0 + w * 0.0555179;
            b1 = 0.99332 * b1 + w * 0.0750759;
            b2 = 0.96900 * b2 + w * 0.1538520;
            b3 = 0.86650 * b3 + w * 0.3104856;
            b4 = 0.55000 * b4 + w * 0.5329522;
            b5 = -0.7616 * b5 - w * 0.0168980;
            let out = b0 + b1 + b2 + b3 + b4 + b5 + b6 + w * 0.5362;
            b6 = w * 0.115926;
            out * 0.11
        })
        .collect()
}

/// Random chirp train with non-periodic sweeps, plus pink bed.
fn chirp_movie(seed: u64, seconds: usize) -> Vec<f32> {
    let n = seconds * RATE;
    let mut rng = Mix::seeded(seed, 1);
    let mut out = vec![0.0f64; n];
    let mut t = 0usize;
    while t < n {
        let dur = ((rng.range(0.12, 0.45)) * RATE as f64) as usize;
        let f0 = rng.range(180.0, 1400.0);
        let f1 = rng.range(260.0, 3400.0);
        let amp = rng.range(0.3, 0.9);
        let mut phase = 0.0f64;
        for k in 0..dur.min(n - t) {
            let u = k as f64 / dur as f64;
            phase += std::f64::consts::TAU * (f0 + (f1 - f0) * u) / RATE as f64;
            let env = 0.5 - 0.5 * (std::f64::consts::TAU * u).cos();
            out[t + k] += amp * env * phase.sin();
        }
        t += dur + ((rng.range(0.05, 0.35)) * RATE as f64) as usize;
    }
    let bed = pink(&mut Mix::seeded(seed, 2), n);
    out.iter()
        .zip(bed)
        .map(|(c, p)| (0.2 * c + 0.04 * p) as f32)
        .collect()
}

/// Unrelated room-like audio: different salt and no chirps at all.
fn room_noise(seed: u64, n: usize) -> Vec<f32> {
    pink(&mut Mix::seeded(seed, 7), n)
        .into_iter()
        .map(|x| (x * 0.5) as f32)
        .collect()
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

/// Add noise at the requested signal-to-noise ratio (dB) relative to `clean`.
fn bury(clean: &[f32], snr_db: f64, seed: u64) -> Vec<f32> {
    let noise = room_noise(seed, clean.len());
    let scale = rms(clean) * 10f64.powf(-snr_db / 20.0) / rms(&noise);
    clean
        .iter()
        .zip(noise)
        .map(|(c, n)| c + (f64::from(n) * scale) as f32)
        .collect()
}

fn slice_seconds(pcm: &[f32], start: f64, seconds: f64) -> Vec<f32> {
    let a = (start * RATE as f64).round() as usize;
    pcm[a..a + (seconds * RATE as f64).round() as usize].to_vec()
}

fn write_wav(path: &Path, pcm: &[f32]) {
    let mut out = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: RATE as u32,
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

/// PCM-only reports never hash the movie file, so a fixed valid digest suffices.
fn pcm_clock() -> SyncMovieClock {
    SyncMovieClock {
        movie_digest: "d".repeat(64),
        time_base_numerator: 1,
        time_base_denominator: TICKS_PER_SECOND,
        origin_pts: 0,
        duration_pts: MOVIE_SECONDS as u64 * u64::from(TICKS_PER_SECOND),
    }
}

fn movie_clock(movie: &Path) -> SyncMovieClock {
    SyncMovieClock {
        movie_digest: full_file_blake3(movie, &|| false).unwrap().hex_digest,
        time_base_numerator: 1,
        time_base_denominator: TICKS_PER_SECOND,
        origin_pts: 0,
        duration_pts: MOVIE_SECONDS as u64 * u64::from(TICKS_PER_SECOND),
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    movie: PathBuf,
    files: Vec<SessionAudioFile>,
    plan: SoundtrackSessionPlan,
}

/// Build a real session: movie WAV, one native mic WAV, 6 s queries, 12 s refs.
fn session(movie_pcm: &[f32], mic_pcm: &[f32]) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let movie = dir.path().join("final mix.wav");
    write_wav(&movie, movie_pcm);
    let mic = dir.path().join("native mic.wav");
    write_wav(&mic, mic_pcm);
    let frames = mic_pcm.len() as u32;
    let plan = SoundtrackSessionPlan::new(SoundtrackSessionSpec {
        schema_version: 1,
        sync: SyncRequest {
            schema_version: 1,
            session_id: "t002_independent".into(),
            target_digest: "b".repeat(64),
            movie: movie_clock(&movie),
            capture_clocks: vec![CaptureClock {
                clock_id: "mic".into(),
                sample_rate: RATE as u32,
                first_sample: 0,
                last_sample: u64::from(frames),
                monotonic_origin_ns: 2_000_000_000,
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
            first_sample: 0,
            frame_count: frames,
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
    let files = vec![SessionAudioFile {
        segment_id: "native".into(),
        path: mic,
    }];
    Fixture {
        dir,
        movie,
        files,
        plan,
    }
}

/// Run every batch to completion, then read the complete evidence page.
fn evidence(fx: &Fixture) -> SessionEvidencePage {
    let cache = fx.dir.path().join("private cache");
    let mut start = 0;
    loop {
        let r = fx
            .plan
            .run_batch(
                SessionBatch {
                    movie: &fx.movie,
                    files: &fx.files,
                    cache: &cache,
                    start_task: start,
                    max_tasks: 128,
                },
                &|| false,
            )
            .unwrap();
        assert!(!r.cancelled);
        if r.next_task >= r.task_count {
            break;
        }
        start = r.next_task;
    }
    fx.plan
        .evidence_page(&cache, 0, fx.plan.queries.len(), &|| false)
        .unwrap()
}

/// Reference window decoded directly from PCM (bypasses ffmpeg, same recipe).
fn window_index(movie_pcm: &[f32], clock: &SyncMovieClock, start: f64) -> SoundtrackIndex {
    let reference = SoundtrackReferenceSpec {
        schema_version: 1,
        movie: clock.clone(),
        start_seconds: start,
        duration_seconds: 12.0,
        recipe: None,
    };
    SoundtrackIndex::from_pcm(reference, &slice_seconds(movie_pcm, start, 12.0), &|| false).unwrap()
}

fn capture() -> SoundtrackCapture {
    SoundtrackCapture {
        evidence_id: "mic_window_t002".into(),
        clock_id: "mic".into(),
        sample_rate: RATE as u32,
        first_sample: 0,
        monotonic_origin_ns: 2_000_000_000,
    }
}

fn movie_pcm() -> Vec<f32> {
    chirp_movie(0x7A3C_5E21_9D04_B6F1, MOVIE_SECONDS)
}

#[test]
fn buried_excerpt_best_peak_is_query_centre_end_to_end() {
    let movie = movie_pcm();
    let excerpt = slice_seconds(&movie, EXCERPT_START, 6.0);
    // Heavy room noise: similarity must stay below the 0.8 acceptance bar.
    let noisy = bury(&excerpt, -3.0, 0x51DE_0001);
    let mut mic = noisy;
    mic.extend(vec![0.0f32; 6 * RATE]); // second query is silence
    let fx = session(&movie, &mic);
    let page = evidence(&fx);

    let q = &page.queries[0];
    assert_eq!(q.completed_references, q.total_references);
    assert_eq!(
        q.state, "unmatched",
        "buried excerpt must stay unmatched: {q:?}"
    );
    assert!(
        q.anchors.is_empty(),
        "unmatched query produced anchors: {q:?}"
    );
    assert!(page.sync.anchors.is_empty());

    assert!(!q.best_peaks.is_empty(), "unmatched query lost its peaks");
    assert!(q.best_peaks.len() <= MAX_BEST_PEAKS);
    let top = &q.best_peaks[0];
    let error = (top.movie_seconds - EXCERPT_CENTRE).abs();
    assert!(
        error <= TOLERANCE_SECONDS,
        "best_peaks[0].movie_seconds {} vs true centre {EXCERPT_CENTRE}: error {error}",
        top.movie_seconds
    );
    // Centre, not start: the start would be 3 s away.
    assert!((top.movie_seconds - EXCERPT_START).abs() > 2.9);
    for pair in q.best_peaks.windows(2) {
        assert!(
            pair[0].similarity >= pair[1].similarity,
            "peaks not sorted by similarity: {:?}",
            q.best_peaks
        );
    }
    let json = serde_json::to_value(q).unwrap();
    assert!(
        json.get("bestPeaks").is_some(),
        "non-empty bestPeaks must serialise"
    );
}

#[test]
fn quiet_query_has_empty_best_peaks_and_omitted_json() {
    let movie = movie_pcm();
    let quiet = vec![0.0f32; 6 * RATE];
    let index = window_index(&movie, &pcm_clock(), 24.0);
    let report = index
        .match_pcm(&quiet, capture(), &SoundtrackPolicy::default(), &|| false)
        .unwrap();
    assert_eq!(report.reason, "quiet_query");
    assert!(report.best_peaks.is_empty());
    let json = serde_json::to_value(&report).unwrap();
    assert!(
        json.get("bestPeaks").is_none(),
        "empty bestPeaks must be omitted from JSON"
    );

    // Session path: second query is silence, so every reference is quiet.
    let excerpt = slice_seconds(&movie, EXCERPT_START, 6.0);
    let mut mic = excerpt;
    mic.extend(quiet);
    let fx = session(&movie, &mic);
    let page = evidence(&fx);
    let q = &page.queries[1];
    assert!(q.best_peaks.is_empty(), "quiet session query: {q:?}");
    assert!(q.anchors.is_empty());
    let json = serde_json::to_value(q).unwrap();
    assert!(json.get("bestPeaks").is_none());
}

#[test]
fn matched_query_best_peak_equals_top_candidate() {
    let movie = movie_pcm();
    let clock = pcm_clock();
    let clean = slice_seconds(&movie, EXCERPT_START, 6.0);
    // Reference 30..42 holds the excerpt at its start; reference 24..36 has it
    // at 6 s. Either is a valid matched window; check the one at 24 s too.
    let index = window_index(&movie, &clock, 24.0);
    let report: SoundtrackMatchReport = index
        .match_pcm(&clean, capture(), &SoundtrackPolicy::default(), &|| false)
        .unwrap();
    assert_eq!(report.state, SoundtrackMatchState::Matched, "{report:?}");
    let top = &report.best_peaks[0];
    let candidate = &report.candidates[0];
    // Peak offset and candidate offset must be the same frame.
    assert_eq!(
        top.reference_offset_frames, candidate.reference_offset_frames,
        "best_peaks[0] is not candidates[0]"
    );
    assert_eq!(top.similarity, candidate.similarity);
    let anchor_seconds = candidate.anchor.movie_pts as f64 / f64::from(TICKS_PER_SECOND);
    assert!(
        (top.movie_seconds - anchor_seconds).abs() <= 1.0 / f64::from(TICKS_PER_SECOND),
        "best peak {} vs anchor {anchor_seconds}",
        top.movie_seconds
    );
    assert!((top.movie_seconds - EXCERPT_CENTRE).abs() <= TOLERANCE_SECONDS);
}

#[test]
fn overlapping_references_dedupe_one_movie_position() {
    let movie = movie_pcm();
    let clock = pcm_clock();
    let mut mic = bury(
        &slice_seconds(&movie, EXCERPT_START, 6.0),
        -3.0,
        0x51DE_0001,
    );
    mic.extend(vec![0.0f32; 6 * RATE]);

    // Precondition: two overlapping windows both see the same position.
    let query = &mic[..6 * RATE];
    let mut raw = Vec::new();
    for start in [24.0, 30.0] {
        let report = window_index(&movie, &clock, start)
            .match_pcm(query, capture(), &SoundtrackPolicy::default(), &|| false)
            .unwrap();
        let near = report
            .best_peaks
            .iter()
            .any(|p| (p.movie_seconds - EXCERPT_CENTRE).abs() < PEAK_MERGE_SECONDS);
        assert!(near, "reference {start} lacks a peak near the true centre");
        raw.extend(report.best_peaks);
    }

    let fx = session(&movie, &mic);
    let page = evidence(&fx);
    let merged = &page.queries[0].best_peaks;
    assert!(!merged.is_empty() && merged.len() <= MAX_BEST_PEAKS);
    let near = merged
        .iter()
        .filter(|p| (p.movie_seconds - EXCERPT_CENTRE).abs() < PEAK_MERGE_SECONDS)
        .count();
    assert_eq!(near, 1, "same movie position was not deduped: {merged:?}");
    for (i, a) in merged.iter().enumerate() {
        for b in &merged[i + 1..] {
            assert!(
                (a.movie_seconds - b.movie_seconds).abs() >= PEAK_MERGE_SECONDS,
                "merged peaks closer than 0.256 s: {a:?} {b:?}"
            );
        }
    }
    assert!((merged[0].movie_seconds - EXCERPT_CENTRE).abs() <= TOLERANCE_SECONDS);
}

#[test]
fn unrelated_audio_stays_unmatched_without_anchor() {
    let movie = movie_pcm();
    let mic = room_noise(0xBEEF_1234, 6 * RATE);
    let fx = session(&movie, &mic);
    let page = evidence(&fx);
    let q = &page.queries[0];
    assert!(
        q.state == "unmatched" || q.state == "ambiguous",
        "unrelated audio state: {}",
        q.state
    );
    assert!(
        q.anchors.is_empty(),
        "unrelated audio produced anchors: {q:?}"
    );
    assert!(page.sync.anchors.is_empty());
}

#[test]
fn repeated_excerpt_ambiguous_produces_no_anchor() {
    // The same 6 s excerpt appears at 10 s and 40 s: two distant positions.
    let mut movie = movie_pcm();
    let repeated = slice_seconds(&movie, 10.0, 6.0);
    let a = 40 * RATE;
    movie[a..a + repeated.len()].copy_from_slice(&repeated);
    let mic = repeated.clone();
    let fx = session(&movie, &mic);
    let page = evidence(&fx);
    let q = &page.queries[0];
    assert_eq!(q.state, "ambiguous", "{q:?}");
    // Charter: ambiguous evidence must never produce an anchor.
    assert!(
        q.anchors.is_empty() && page.sync.anchors.is_empty(),
        "ambiguous query produced {} anchors",
        q.anchors.len()
    );
}
