//! V01-T005 independent oracle for ARCHITECTURE section 6 (speech mask).
//!
//! Audio comes from a SplitMix64 generator with its own seed scheme, which
//! differs from the implementer's LCG fixtures and from the T003 xorshift and
//! T004 wyrand chirp trains. Speech-like interference is built differently
//! again: amplitude-modulated formant tones (a harmonic series of a pitch with
//! three formant peaks, modulated at syllable rate), not chirps.
//!
//! Every mask is built through `SoundtrackSessionSpec` JSON (`speechMask`), and
//! every result is read through the public plan -> run_batch -> evidence_page
//! API. The only private access is reading the cached receipts by their
//! documented layout (`cache/<plan_digest>/task-<n>.json`), as in T004.

use std::{
    f64::consts::TAU,
    path::{Path, PathBuf},
};

use rcdvlog_core::{
    full_file_blake3,
    tv_review_sync::{CaptureClock, SyncMovieClock, SyncPolicy, SyncRequest},
    tv_soundtrack::{
        DistinctivePolicy, SoundtrackCapture, SoundtrackIndex, SoundtrackMatchReport,
        SoundtrackMatchState, SoundtrackPeak, SoundtrackPolicy, SoundtrackReferenceSpec,
    },
    tv_soundtrack_session::{
        SessionAudioFile, SessionAudioSegment, SessionBatch, SessionEvidencePage,
        SoundtrackSessionPlan, SoundtrackSessionSpec,
    },
};
use serde_json::{Value, json};

const RATE: u32 = 8000;
const MOVIE_SECONDS: usize = 150;
/// The excerpt is the whole 6 s query. Its true centre is FIRST_AT + 3 s.
const EXCERPT_SECONDS: usize = 6;
const FIRST_AT: f64 = 10.0;
const TRUE_CENTRE: f64 = FIRST_AT + 3.0;
const CENTRE_TOLERANCE: f64 = 0.1;

/// SplitMix64 with a seed scheme private to this file: every stream seed is
/// `SEED_SCHEME ^ (tag * odd constant)`.
const SEED_SCHEME: u64 = 0x7E57_5EED_0000_0000;
fn seed(tag: u64) -> u64 {
    SEED_SCHEME ^ tag.wrapping_mul(0xD6E8_FEB8_6659_FD93)
}

struct SplitMix(u64);
impl SplitMix {
    fn new(seed: u64) -> Self {
        Self(seed)
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

/// Exponential-sweep chirp train (background programme and excerpt).
fn chirp_train(tag: u64, seconds: usize) -> Vec<f32> {
    let n = seconds * RATE as usize;
    let mut rng = SplitMix::new(seed(tag));
    let mut out = vec![0.0f32; n];
    let mut t = 0usize;
    while t < n {
        let len = (rng.range(0.03, 0.15) * f64::from(RATE)) as usize;
        let lo = rng.range(300.0, 900.0).ln();
        let hi = rng.range(900.0, 3000.0).ln();
        let amp = rng.range(0.2, 0.5);
        let mut phase = 0.0f64;
        for k in 0..len.min(n - t) {
            let u = k as f64 / len as f64;
            let f = (lo + (hi - lo) * u).exp();
            phase += TAU * f / f64::from(RATE);
            let env = (std::f64::consts::PI * u).sin().powi(2);
            out[t + k] += (amp * env * phase.sin()) as f32;
        }
        t += len + (rng.range(0.0, 0.08) * f64::from(RATE)) as usize;
    }
    out
}

/// Speech-like loud burst: a harmonic series on a pitch f0, weighted by three
/// formant resonances, modulated at syllable rate. Peak amplitude is 0.9.
fn speech_burst(tag: u64, seconds: usize) -> Vec<f32> {
    let n = seconds * RATE as usize;
    let mut rng = SplitMix::new(seed(tag));
    let f0 = rng.range(105.0, 150.0);
    let formants = [
        rng.range(450.0, 800.0),
        rng.range(1000.0, 1800.0),
        rng.range(2200.0, 2800.0),
    ];
    let syllable_hz = rng.range(3.6, 4.8);
    let am_phase = rng.range(0.0, TAU);
    let mut partials = Vec::new();
    let mut total = 0.0;
    for h in 1.. {
        let fh = h as f64 * f0;
        if fh > 3600.0 {
            break;
        }
        let amp = formants
            .iter()
            .map(|&ff| (-((fh - ff) / 160.0).powi(2)).exp())
            .sum::<f64>()
            + 0.02;
        total += amp;
        partials.push((fh, amp, rng.range(0.0, TAU)));
    }
    (0..n)
        .map(|t| {
            let s = t as f64 / f64::from(RATE);
            let env = (0.5 - 0.5 * (TAU * syllable_hz * s + am_phase).cos()).powi(2);
            let v: f64 = partials
                .iter()
                .map(|(fh, amp, ph)| amp * (TAU * fh * s + ph).sin())
                .sum();
            (0.9 * env * v / total) as f32
        })
        .collect()
}

fn write_wav(path: &Path, pcm: &[f32]) {
    let mut out = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: RATE,
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

/// Sample index at 8 kHz for a time in seconds.
fn at(seconds: f64) -> u64 {
    (seconds * f64::from(RATE)).round() as u64
}

/// Movie: chirp background with the excerpt written at each occurrence.
fn movie_pcm(excerpt: &[f32], occurrences: &[f64]) -> Vec<f32> {
    let mut movie = chirp_train(0x4D4F_5649, MOVIE_SECONDS);
    for &t in occurrences {
        let start = at(t) as usize;
        movie[start..start + excerpt.len()].copy_from_slice(excerpt);
    }
    movie
}

/// Adds `burst` samples at the positions inside each `[a, b)` interval.
fn bury(mic: &mut [f32], burst: &[f32], intervals: &[(u64, u64)]) {
    for &(a, b) in intervals {
        for i in a as usize..b as usize {
            mic[i] += burst[i];
        }
    }
}

fn intervals_seconds(spans: &[(f64, f64)]) -> Vec<(u64, u64)> {
    spans.iter().map(|&(a, b)| (at(a), at(b))).collect()
}

struct Setup {
    _dir: tempfile::TempDir,
    plan: SoundtrackSessionPlan,
    movie: PathBuf,
    files: Vec<SessionAudioFile>,
    cache: PathBuf,
}

/// Builds the session through `SoundtrackSessionSpec` JSON. `mask` intervals
/// (capture samples) go into `speechMask` only when non-empty.
fn setup(
    movie_samples: &[f32],
    mic: &[f32],
    mask: &[(u64, u64)],
    policy: SoundtrackPolicy,
) -> Setup {
    setup_q(movie_samples, mic, mask, policy, 6, 12)
}

fn setup_q(
    movie_samples: &[f32],
    mic: &[f32],
    mask: &[(u64, u64)],
    policy: SoundtrackPolicy,
    query_seconds: u32,
    reference_seconds: u32,
) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let movie = dir.path().join("movie mix.wav");
    write_wav(&movie, movie_samples);
    let mic_path = dir.path().join("room mic.wav");
    write_wav(&mic_path, mic);
    let movie_digest = full_file_blake3(&movie, &|| false).unwrap().hex_digest;
    let mic_digest = full_file_blake3(&mic_path, &|| false).unwrap().hex_digest;
    let mic_frames = mic.len() as u64;
    let base = SoundtrackSessionSpec {
        schema_version: 1,
        sync: SyncRequest {
            schema_version: 1,
            session_id: "t005_independent".into(),
            target_digest: "d".repeat(64),
            movie: SyncMovieClock {
                movie_digest,
                time_base_numerator: 1,
                time_base_denominator: 90000,
                origin_pts: 90000,
                duration_pts: MOVIE_SECONDS as u64 * 90000,
            },
            capture_clocks: vec![CaptureClock {
                clock_id: "mic".into(),
                sample_rate: RATE,
                first_sample: 0,
                last_sample: mic_frames,
                monotonic_origin_ns: 2_000_000_000,
                monotonic_tolerance_ns: 1,
            }],
            anchors: vec![],
            boundaries: vec![],
            policy: SyncPolicy::default(),
            consensus_segments: vec![],
        },
        segments: vec![SessionAudioSegment {
            segment_id: "room".into(),
            clock_id: "mic".into(),
            first_sample: 0,
            frame_count: mic_frames as u32,
            wav_blake3: mic_digest,
        }],
        reference_seconds,
        query_seconds,
        query_step_seconds: query_seconds,
        match_policy: policy,
        speech_mask: vec![],
        recipe: None,
    };
    let mut value = serde_json::to_value(&base).unwrap();
    if !mask.is_empty() {
        value["speechMask"] = json!(
            mask.iter()
                .map(|(a, b)| json!({"clockId": "mic", "startSample": a, "endSample": b}))
                .collect::<Vec<_>>()
        );
    }
    let spec: SoundtrackSessionSpec = serde_json::from_value(value).unwrap();
    let plan = SoundtrackSessionPlan::new(spec).unwrap();
    assert_eq!(plan.queries.len(), 1, "one query expected");
    assert!(plan.references.len() >= 2, "expected several references");
    let cache = dir.path().join("cache");
    Setup {
        plan,
        movie,
        files: vec![SessionAudioFile {
            segment_id: "room".into(),
            path: mic_path,
        }],
        cache,
        _dir: dir,
    }
}

fn run_all(s: &Setup) {
    let result = s
        .plan
        .run_batch(
            SessionBatch {
                movie: &s.movie,
                files: &s.files,
                cache: &s.cache,
                start_task: 0,
                max_tasks: 128,
            },
            &|| false,
        )
        .unwrap();
    assert_eq!(result.next_task, s.plan.task_count);
    assert!(!result.cancelled);
}

fn page(s: &Setup) -> SessionEvidencePage {
    s.plan.evidence_page(&s.cache, 0, 1, &|| false).unwrap()
}

/// Every cached per-reference report, in task order.
fn reports(s: &Setup) -> Vec<SoundtrackMatchReport> {
    (0..s.plan.task_count)
        .map(|ti| {
            let path = s
                .cache
                .join(&s.plan.plan_digest)
                .join(format!("task-{ti}.json"));
            let receipt: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            serde_json::from_value(receipt["report"].clone()).unwrap()
        })
        .collect()
}

fn peaks_of(s: &Setup) -> Vec<Vec<SoundtrackPeak>> {
    reports(s).into_iter().map(|r| r.best_peaks).collect()
}

fn states_of(s: &Setup) -> Vec<SoundtrackMatchState> {
    reports(s).into_iter().map(|r| r.state).collect()
}

fn digests_of(s: &Setup) -> Vec<Option<String>> {
    reports(s).into_iter().map(|r| r.mask_digest).collect()
}

fn reasons_of(s: &Setup) -> Vec<String> {
    reports(s).into_iter().map(|r| r.reason).collect()
}

/// Movie-time seconds of an anchor's query centre.
fn anchor_seconds(s: &Setup, pts: i64) -> f64 {
    (pts - s.plan.spec.sync.movie.origin_pts) as f64 / 90000.0
}

/// Excerpt used as the clean query in most scenarios.
fn excerpt() -> Vec<f32> {
    chirp_train(0x0E5C_E4F7, EXCERPT_SECONDS)
}

// ---------------------------------------------------------------------------
// 1. Oracle: masked-span noise leaves similarity, peaks and state bit-equal.
// ---------------------------------------------------------------------------

/// Masked span: 2.0 s to 3.0 s of the 6 s query. Unmasked: 5 s, with 1 s in
/// every third, so the query is not insufficient.
const MASKED_SPAN: (f64, f64) = (2.0, 3.0);

#[test]
fn masked_span_noise_leaves_similarity_peaks_and_state_bit_equal() {
    let ex = excerpt();
    let movie = movie_pcm(&ex, &[FIRST_AT]);
    let mask = intervals_seconds(&[MASKED_SPAN]);
    let noise_a = speech_burst(0xA0A0_0001, EXCERPT_SECONDS);
    let noise_b = speech_burst(0xB0B0_0002, EXCERPT_SECONDS);

    // Clean run: the excerpt alone. Noisy runs: loud noise added only inside
    // the masked span, so the unmasked samples are identical in all three.
    let clean = ex.clone();
    let mut noisy_a = ex.clone();
    let mut noisy_b = ex.clone();
    bury(&mut noisy_a, &noise_a, &mask);
    bury(&mut noisy_b, &noise_b, &mask);

    let s_clean = setup(&movie, &clean, &mask, distinctive_policy());
    let s_a = setup(&movie, &noisy_a, &mask, distinctive_policy());
    let s_b = setup(&movie, &noisy_b, &mask, distinctive_policy());
    for s in [&s_clean, &s_a, &s_b] {
        run_all(s);
    }

    // Non-vacuity: the two noisy queries really differ, and both differ from clean.
    assert_ne!(noisy_a, noisy_b);
    assert_ne!(noisy_a, clean);
    assert_ne!(s_a.plan.plan_digest, s_b.plan.plan_digest);

    // The clean run must find the true excerpt, otherwise the comparison is empty.
    let clean_page = page(&s_clean);
    assert_eq!(clean_page.queries[0].state, "candidate", "{clean_page:?}");
    let clean_seconds = anchor_seconds(&s_clean, clean_page.queries[0].anchors[0].movie_pts);
    assert!(
        (clean_seconds - TRUE_CENTRE).abs() < CENTRE_TOLERANCE,
        "{clean_seconds}"
    );

    // Bit-equal similarities, peaks and states across all three runs.
    for other in [&s_a, &s_b] {
        let p0 = peaks_of(&s_clean);
        let p1 = peaks_of(other);
        assert_eq!(p0, p1, "best peaks differ under masked noise");
        for (x, y) in p0.iter().flatten().zip(p1.iter().flatten()) {
            assert_eq!(x.similarity.to_bits(), y.similarity.to_bits());
        }
        assert_eq!(states_of(&s_clean), states_of(other), "states differ");
        let r0 = reports(&s_clean);
        let r1 = reports(other);
        for (x, y) in r0.iter().zip(r1.iter()) {
            assert_eq!(
                x.best_similarity.map(f64::to_bits),
                y.best_similarity.map(f64::to_bits)
            );
            assert_eq!(
                x.runner_up_similarity.map(f64::to_bits),
                y.runner_up_similarity.map(f64::to_bits)
            );
        }
    }

    // mask_digest is set on every report, and it depends only on the mask.
    let d_clean = digests_of(&s_clean);
    assert!(!d_clean.is_empty());
    for d in d_clean
        .iter()
        .chain(digests_of(&s_a).iter())
        .chain(digests_of(&s_b).iter())
    {
        let d = d
            .as_ref()
            .expect("mask_digest must be set when a mask is supplied");
        assert_eq!(d.len(), 64);
        assert!(
            d.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
    }
    assert_eq!(d_clean, digests_of(&s_a));
    assert_eq!(d_clean, digests_of(&s_b));
    eprintln!(
        "SCENARIO1 clean/noiseA/noiseB state={:?} mask_digest={:?} best_peaks0={:?}",
        clean_page.queries[0].state,
        d_clean.first(),
        peaks_of(&s_clean).first()
    );
}

// ---------------------------------------------------------------------------
// 2. 60% speech-burst scenario.
// ---------------------------------------------------------------------------

/// 0.6 s bursts every 1.0 s over the 6 s query: 3.6 s of 6 s (60%) masked.
/// Every third keeps at least 0.8 s unmasked.
fn burst_spans() -> Vec<(f64, f64)> {
    (0..EXCERPT_SECONDS)
        .map(|k| (k as f64, k as f64 + 0.6))
        .collect()
}

fn burst_mic() -> (Vec<f32>, Vec<(u64, u64)>) {
    let ex = excerpt();
    let mask = intervals_seconds(&burst_spans());
    let burst = speech_burst(0xB0B0_0060, EXCERPT_SECONDS);
    let mut mic = ex;
    bury(&mut mic, &burst, &mask);
    (mic, mask)
}

#[test]
fn speech_burst_60pct_with_mask_matches_at_true_position() {
    let (mic, mask) = burst_mic();
    let masked: u64 = mask.iter().map(|(a, b)| b - a).sum();
    assert_eq!(
        masked * 10,
        (EXCERPT_SECONDS as u64 * RATE as u64) * 6,
        "60% of the query"
    );
    let movie = movie_pcm(&excerpt(), &[FIRST_AT]);
    let s = setup(&movie, &mic, &mask, distinctive_policy());
    run_all(&s);
    let pg = page(&s);
    let ev = &pg.queries[0];
    let r = reports(&s);
    eprintln!(
        "SCENARIO2 masked state={} anchors={:?} best={:?} reasons={:?} digest0={:?}",
        ev.state,
        ev.anchors
            .iter()
            .map(|a| anchor_seconds(&s, a.movie_pts))
            .collect::<Vec<_>>(),
        r.iter().map(|x| x.best_similarity).collect::<Vec<_>>(),
        r.iter().map(|x| x.reason.clone()).collect::<Vec<_>>(),
        r.first().and_then(|x| x.mask_digest.clone())
    );
    // The mask must not be insufficient, otherwise this is not the scenario.
    assert!(
        r.iter().all(|x| x.reason != "masked_insufficient"),
        "burst mask was insufficient: {:?}",
        r.iter().map(|x| &x.reason).collect::<Vec<_>>()
    );
    assert_eq!(ev.state, "candidate", "{ev:?}");
    assert_eq!(ev.anchors.len(), 1, "{ev:?}");
    let seconds = anchor_seconds(&s, ev.anchors[0].movie_pts);
    assert!(
        (seconds - TRUE_CENTRE).abs() < CENTRE_TOLERANCE,
        "anchor at {seconds} s, expected {TRUE_CENTRE} s"
    );
    // No Matched report may sit on the burst's own content: every Matched
    // reference must have its best peak at the true excerpt position.
    for rep in r
        .iter()
        .filter(|x| x.state == SoundtrackMatchState::Matched)
    {
        let top = rep.best_peaks.first().expect("matched report has peaks");
        assert!(
            (top.movie_seconds - TRUE_CENTRE).abs() < CENTRE_TOLERANCE,
            "Matched on non-true content at {} s",
            top.movie_seconds
        );
    }
    assert!(r.iter().all(|x| x.mask_digest.is_some()));
}

/// 12 s query variant. Bursts of 1.2 s every 2.0 s: 7.2 s of 12 s (60%) masked,
/// with 0.8 s unmasked gaps. Each third (4 s) holds two gaps, which is enough
/// after the frame-support loss (about 0.19 s per gap, see `mask_weights`).
const LONG_QUERY: usize = 12;
const LONG_REFERENCE: u32 = 20;
const LONG_TRUE_CENTRE: f64 = FIRST_AT + LONG_QUERY as f64 / 2.0;

fn long_burst_spans() -> Vec<(f64, f64)> {
    (0..6)
        .map(|k| (k as f64 * 2.0, k as f64 * 2.0 + 1.2))
        .collect()
}

fn long_burst_mic() -> (Vec<f32>, Vec<(u64, u64)>) {
    let ex = chirp_train(0x0E5C_E4F7_0012, LONG_QUERY);
    let mask = intervals_seconds(&long_burst_spans());
    let burst = speech_burst(0xB0B0_0012, LONG_QUERY);
    let mut mic = ex;
    bury(&mut mic, &burst, &mask);
    (mic, mask)
}

fn long_movie() -> Vec<f32> {
    movie_pcm(&chirp_train(0x0E5C_E4F7_0012, LONG_QUERY), &[FIRST_AT])
}

#[test]
fn speech_burst_12s_60pct_with_mask_matches_at_true_position() {
    let (mic, mask) = long_burst_mic();
    let masked: u64 = mask.iter().map(|(a, b)| b - a).sum();
    assert_eq!(
        masked * 10,
        (LONG_QUERY as u64 * RATE as u64) * 6,
        "60% of the query"
    );
    let s = setup_q(
        &long_movie(),
        &mic,
        &mask,
        distinctive_policy(),
        LONG_QUERY as u32,
        LONG_REFERENCE,
    );
    run_all(&s);
    let pg = page(&s);
    let ev = &pg.queries[0];
    let r = reports(&s);
    eprintln!(
        "SCENARIO2-12s masked state={} anchors={:?} reasons_unique={:?} best_first={:?}",
        ev.state,
        ev.anchors
            .iter()
            .map(|a| anchor_seconds(&s, a.movie_pts))
            .collect::<Vec<_>>(),
        {
            let mut u: Vec<_> = r.iter().map(|x| x.reason.clone()).collect();
            u.sort();
            u.dedup();
            u
        },
        r.iter()
            .filter_map(|x| x.best_similarity)
            .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))))
    );
    assert!(
        r.iter().all(|x| x.reason != "masked_insufficient"),
        "12 s burst mask was insufficient"
    );
    assert_eq!(ev.state, "candidate", "{ev:?}");
    assert_eq!(ev.anchors.len(), 1, "{ev:?}");
    let seconds = anchor_seconds(&s, ev.anchors[0].movie_pts);
    assert!(
        (seconds - LONG_TRUE_CENTRE).abs() < CENTRE_TOLERANCE,
        "anchor at {seconds} s, expected {LONG_TRUE_CENTRE} s"
    );
    for rep in r
        .iter()
        .filter(|x| x.state == SoundtrackMatchState::Matched)
    {
        let top = rep.best_peaks.first().expect("matched report has peaks");
        assert!(
            (top.movie_seconds - LONG_TRUE_CENTRE).abs() < CENTRE_TOLERANCE,
            "Matched on non-true content at {} s",
            top.movie_seconds
        );
    }
}

/// Content-dependent: for this burst layout the unmasked query is a candidate
/// at the correct 16.0 s position (observed), so "unmatched without the mask"
/// does not hold for this content. What the contract requires is that the
/// mask-on result sits at the true position, and that the mask-off result,
/// if it produces any anchor at all, is not a wrong-position anchor.
#[test]
fn speech_burst_12s_60pct_without_mask_has_no_wrong_position_anchor() {
    let (mic, mask) = long_burst_mic();

    let on = setup_q(
        &long_movie(),
        &mic,
        &mask,
        distinctive_policy(),
        LONG_QUERY as u32,
        LONG_REFERENCE,
    );
    run_all(&on);
    let on_page = page(&on);
    let on_ev = &on_page.queries[0];
    assert_eq!(on_ev.state, "candidate", "{on_ev:?}");
    assert_eq!(on_ev.anchors.len(), 1, "{on_ev:?}");
    let on_seconds = anchor_seconds(&on, on_ev.anchors[0].movie_pts);
    assert!(
        (on_seconds - LONG_TRUE_CENTRE).abs() <= 0.1,
        "mask-on anchor at {on_seconds} s, expected {LONG_TRUE_CENTRE} s"
    );

    let off = setup_q(
        &long_movie(),
        &mic,
        &[],
        distinctive_policy(),
        LONG_QUERY as u32,
        LONG_REFERENCE,
    );
    run_all(&off);
    let off_page = page(&off);
    let off_ev = &off_page.queries[0];
    eprintln!(
        "SCENARIO2-12s unmasked state={} anchors={} sync_anchors={}",
        off_ev.state,
        off_ev.anchors.len(),
        off_page.sync.anchors.len()
    );
    for anchor in &off_ev.anchors {
        let seconds = anchor_seconds(&off, anchor.movie_pts);
        assert!(
            (seconds - LONG_TRUE_CENTRE).abs() <= 0.1,
            "mask-off wrong-position anchor at {seconds} s, expected {LONG_TRUE_CENTRE} s"
        );
    }
}

#[test]
fn speech_burst_60pct_without_mask_is_unmatched() {
    let (mic, _mask) = burst_mic();
    let movie = movie_pcm(&excerpt(), &[FIRST_AT]);
    let s = setup(&movie, &mic, &[], distinctive_policy());
    run_all(&s);
    let pg = page(&s);
    let ev = &pg.queries[0];
    let r = reports(&s);
    eprintln!(
        "SCENARIO2 unmasked state={} anchors={} best={:?} reasons={:?} peaks0={:?}",
        ev.state,
        ev.anchors.len(),
        r.iter().map(|x| x.best_similarity).collect::<Vec<_>>(),
        r.iter().map(|x| x.reason.clone()).collect::<Vec<_>>(),
        r.first().map(|x| x
            .best_peaks
            .iter()
            .map(|p| (p.similarity, p.movie_seconds))
            .collect::<Vec<_>>())
    );
    assert_eq!(ev.state, "unmatched", "{ev:?}");
    assert!(ev.anchors.is_empty(), "{ev:?}");
    assert!(pg.sync.anchors.is_empty(), "{pg:?}");
    assert!(r.iter().all(|x| x.state != SoundtrackMatchState::Matched));
}

// ---------------------------------------------------------------------------
// 3. masked_insufficient.
// ---------------------------------------------------------------------------

fn assert_insufficient(spans: &[(f64, f64)], label: &str) {
    let ex = excerpt();
    let movie = movie_pcm(&ex, &[FIRST_AT]);
    let mask = intervals_seconds(spans);
    let s = setup(&movie, &ex, &mask, distinctive_policy());
    run_all(&s);
    let pg = page(&s);
    let r = reports(&s);
    eprintln!(
        "SCENARIO3 {label} reasons={:?} states={:?}",
        reasons_of(&s),
        states_of(&s)
    );
    assert!(
        r.iter().all(|x| x.reason == "masked_insufficient"),
        "{label}: expected masked_insufficient, got {:?}",
        reasons_of(&s)
    );
    assert!(
        r.iter().all(|x| x.state == SoundtrackMatchState::Unmatched),
        "{label}"
    );
    assert!(pg.queries[0].anchors.is_empty(), "{label}: {pg:?}");
    assert!(pg.sync.anchors.is_empty(), "{label}");
}

#[test]
fn masked_insufficient_total_under_2s() {
    // 5 s masked leaves 1 s unmasked, under the 2 s total rule.
    assert_insufficient(&[(0.5, 5.5)], "total<2s");
}

#[test]
fn masked_insufficient_first_third_under_half_second() {
    // First third (0..2 s) fully masked; total unmasked is 4 s.
    assert_insufficient(&[(0.0, 2.0)], "first third empty");
}

#[test]
fn masked_insufficient_middle_third_under_half_second() {
    assert_insufficient(&[(2.0, 4.0)], "middle third empty");
}

#[test]
fn masked_insufficient_positive_control_is_not_insufficient() {
    // Same machinery, 1 s masked: must NOT give masked_insufficient.
    let ex = excerpt();
    let movie = movie_pcm(&ex, &[FIRST_AT]);
    let s = setup(
        &movie,
        &ex,
        &intervals_seconds(&[MASKED_SPAN]),
        distinctive_policy(),
    );
    run_all(&s);
    let r = reasons_of(&s);
    eprintln!("SCENARIO3 control reasons={r:?}");
    assert!(r.iter().all(|x| x != "masked_insufficient"), "{r:?}");
}

// ---------------------------------------------------------------------------
// 4. All-ones weights reproduce unmasked scores exactly.
// ---------------------------------------------------------------------------

fn reference_clock() -> SyncMovieClock {
    SyncMovieClock {
        movie_digest: "a".repeat(64),
        time_base_numerator: 1,
        time_base_denominator: 90000,
        origin_pts: 0,
        duration_pts: MOVIE_SECONDS as u64 * 90000,
    }
}

#[test]
fn all_ones_weights_reproduce_unmasked_scores_exactly_direct() {
    let ex = excerpt();
    let mut ref_pcm = chirp_train(0x0BAD_F00D, 12);
    let start = at(3.0) as usize;
    ref_pcm[start..start + ex.len()].copy_from_slice(&ex);
    let reference = SoundtrackReferenceSpec {
        schema_version: 1,
        movie: reference_clock(),
        start_seconds: 0.0,
        duration_seconds: 12.0,
        recipe: None,
    };
    let index = SoundtrackIndex::from_pcm(reference, &ref_pcm, &|| false).unwrap();
    let capture = SoundtrackCapture {
        evidence_id: "ev_ones".into(),
        clock_id: "mic".into(),
        sample_rate: RATE,
        first_sample: 0,
        monotonic_origin_ns: 0,
    };
    let policy = distinctive_policy();
    let plain = index
        .match_pcm(&ex, capture.clone(), &policy, &|| false)
        .unwrap();
    let frames = (ex.len() - 512) / 128 + 1;
    let ones = vec![1.0f32; frames];
    let mut weighted = index
        .match_pcm_weighted(&ex, capture, &policy, Some(&ones), &|| false)
        .unwrap();
    assert!(!plain.best_peaks.is_empty(), "scenario must produce peaks");
    assert!(plain.mask_digest.is_none());
    assert!(weighted.mask_digest.is_some());
    assert_eq!(
        plain.best_peaks, weighted.best_peaks,
        "peaks must be bit-equal"
    );
    for (a, b) in plain.best_peaks.iter().zip(weighted.best_peaks.iter()) {
        assert_eq!(a.similarity.to_bits(), b.similarity.to_bits());
    }
    weighted.mask_digest = None;
    assert_eq!(
        serde_json::to_value(&plain).unwrap(),
        serde_json::to_value(&weighted).unwrap(),
        "all-ones report must equal the unmasked report apart from mask_digest"
    );
}

#[test]
fn all_ones_session_mask_reproduces_unmasked_receipts() {
    // A mask interval beyond the 6 s recording covers no query frame, so every
    // weight is one while mask_digest is still set.
    let ex = excerpt();
    let movie = movie_pcm(&ex, &[FIRST_AT]);
    let plain = setup(&movie, &ex, &[], distinctive_policy());
    let ones = setup(&movie, &ex, &[(at(60.0), at(61.0))], distinctive_policy());
    run_all(&plain);
    run_all(&ones);
    let rp = reports(&plain);
    let ro = reports(&ones);
    assert!(
        rp.iter().any(|r| !r.best_peaks.is_empty()),
        "scenario must produce peaks"
    );
    assert_eq!(peaks_of(&plain), peaks_of(&ones));
    for (a, b) in rp.iter().zip(ro.iter()) {
        assert_eq!(a.state, b.state);
        assert_eq!(a.reason, b.reason);
        assert_eq!(
            a.best_similarity.map(f64::to_bits),
            b.best_similarity.map(f64::to_bits)
        );
        assert!(b.mask_digest.is_some());
        assert!(a.mask_digest.is_none());
    }
}

// ---------------------------------------------------------------------------
// 5. No mask: no mask_digest, and it is absent from serialised bytes.
// ---------------------------------------------------------------------------

#[test]
fn no_mask_report_has_no_mask_digest_and_omits_it_when_serialised() {
    let ex = excerpt();
    let movie = movie_pcm(&ex, &[FIRST_AT]);
    let s = setup(&movie, &ex, &[], distinctive_policy());
    run_all(&s);
    for ti in 0..s.plan.task_count {
        let path = s
            .cache
            .join(&s.plan.plan_digest)
            .join(format!("task-{ti}.json"));
        let receipt: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert!(
            receipt["report"].get("maskDigest").is_none(),
            "receipt {ti} carries maskDigest without a mask"
        );
        assert!(!receipt.to_string().contains("maskDigest"));
    }
    assert!(
        !serde_json::to_value(&s.plan.spec)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("speechMask")
    );
    // Direct report serialisation without a mask.
    let reference = SoundtrackReferenceSpec {
        schema_version: 1,
        movie: reference_clock(),
        start_seconds: 0.0,
        duration_seconds: 12.0,
        recipe: None,
    };
    let index =
        SoundtrackIndex::from_pcm(reference, &movie[..12 * RATE as usize], &|| false).unwrap();
    let report = index
        .match_pcm(
            &ex,
            SoundtrackCapture {
                evidence_id: "ev_nomask".into(),
                clock_id: "mic".into(),
                sample_rate: RATE,
                first_sample: 0,
                monotonic_origin_ns: 0,
            },
            &distinctive_policy(),
            &|| false,
        )
        .unwrap();
    assert!(report.mask_digest.is_none());
    let text = serde_json::to_string(&report).unwrap();
    assert!(!text.contains("maskDigest"), "{text}");
}

// ---------------------------------------------------------------------------
// 6. Session identity and fully masked queries.
// ---------------------------------------------------------------------------

#[test]
fn changing_only_the_mask_changes_plan_digest_and_mask_digest() {
    let ex = excerpt();
    let movie = movie_pcm(&ex, &[FIRST_AT]);
    let none = setup(&movie, &ex, &[], distinctive_policy());
    let a = setup(
        &movie,
        &ex,
        &intervals_seconds(&[(2.0, 3.0)]),
        distinctive_policy(),
    );
    let a_again = setup(
        &movie,
        &ex,
        &intervals_seconds(&[(2.0, 3.0)]),
        distinctive_policy(),
    );
    let b = setup(
        &movie,
        &ex,
        &intervals_seconds(&[(2.0, 3.5)]),
        distinctive_policy(),
    );
    assert_eq!(
        a.plan.plan_digest, a_again.plan.plan_digest,
        "plan digest must be deterministic"
    );
    assert_ne!(none.plan.plan_digest, a.plan.plan_digest);
    assert_ne!(
        a.plan.plan_digest, b.plan.plan_digest,
        "mask change must change cache root"
    );
    run_all(&a);
    run_all(&b);
    let da = digests_of(&a);
    let db = digests_of(&b);
    assert!(da.iter().all(Option::is_some) && db.iter().all(Option::is_some));
    assert_ne!(da, db, "mask change must change mask_digest");
    // The cache directories are distinct, so neither run reads the other's receipts.
    assert_ne!(
        a.cache.join(&a.plan.plan_digest),
        b.cache.join(&b.plan.plan_digest)
    );
}

#[test]
fn fully_masked_query_gives_no_anchors() {
    let ex = excerpt();
    let movie = movie_pcm(&ex, &[FIRST_AT]);
    let s = setup(&movie, &ex, &[(0, ex.len() as u64)], distinctive_policy());
    run_all(&s);
    let pg = page(&s);
    let r = reports(&s);
    eprintln!("SCENARIO6 fully masked reasons={:?}", reasons_of(&s));
    assert!(r.iter().all(|x| x.reason == "masked_insufficient"));
    assert!(pg.queries[0].anchors.is_empty(), "{pg:?}");
    assert!(pg.sync.anchors.is_empty(), "{pg:?}");
    assert_ne!(pg.queries[0].state, "candidate");
    assert_ne!(pg.queries[0].state, "ambiguous");
}
