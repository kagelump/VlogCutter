//! V01-T003 independent distinctive-acceptance oracle (ARCHITECTURE section 4).
//!
//! Audio comes from a generator separate from the implementer's LCG fixtures and
//! from the T002 SplitMix64 oracle: a xorshift64* stream drives a random chirp
//! train, Kellet-style pseudo-pink noise, white noise and a metronome bait.
use rcdvlog_core::{
    tv_review_sync::SyncMovieClock,
    tv_soundtrack::{
        DistinctivePolicy, SoundtrackCapture, SoundtrackIndex, SoundtrackMatchReport,
        SoundtrackMatchState, SoundtrackPolicy, SoundtrackReferenceSpec,
    },
};

const RATE: usize = 8000;

/// xorshift64* with a seed scheme distinct from the implementer's tests.
struct Xs(u64);
impl Xs {
    fn seeded(seed: u64) -> Self {
        let mut s = seed.wrapping_mul(0xA076_1D64_78BD_642F) ^ 0xE703_7ED1_A0B4_28DB;
        if s == 0 {
            s = 0x1234_5678_9ABC_DEF1;
        }
        Self(s)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }
}

/// Random chirp train: Hann-windowed linear sweeps at random times and rates.
fn chirp_train(rng: &mut Xs, n: usize) -> Vec<f64> {
    let mut out = vec![0.0; n];
    let mut t = 0usize;
    while t < n {
        let len = (rng.range(0.04, 0.22) * RATE as f64) as usize;
        let f0 = rng.range(180.0, 2600.0);
        let f1 = rng.range(180.0, 2600.0);
        let amp = rng.range(0.3, 1.0);
        let mut phase = 0.0;
        for k in 0..len.min(n - t) {
            let u = k as f64 / len as f64;
            let f = f0 + (f1 - f0) * u;
            phase += std::f64::consts::TAU * f / RATE as f64;
            let win = 0.5 - 0.5 * (std::f64::consts::TAU * u).cos();
            out[t + k] += amp * win * phase.sin();
        }
        t += len + (rng.range(0.0, 0.12) * RATE as f64) as usize;
    }
    out
}

/// Kellet-style pseudo-pink noise from white draws.
fn pink(rng: &mut Xs, n: usize) -> Vec<f64> {
    let (mut b0, mut b1, mut b2, mut b3, mut b4, mut b5, mut b6) =
        (0.0f64, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    (0..n)
        .map(|_| {
            let white = rng.range(-1.0, 1.0);
            b0 = 0.99886 * b0 + white * 0.0555179;
            b1 = 0.99332 * b1 + white * 0.0750759;
            b2 = 0.96900 * b2 + white * 0.1538520;
            b3 = 0.86650 * b3 + white * 0.3104856;
            b4 = 0.55000 * b4 + white * 0.5329522;
            b5 = -0.7616 * b5 - white * 0.0168980;
            let y = b0 + b1 + b2 + b3 + b4 + b5 + b6 + white * 0.5362;
            b6 = white * 0.115926;
            y * 0.11
        })
        .collect()
}

/// Scene audio: chirp train plus pseudo-pink bed, peak-normalised near 0.5.
fn scene(seconds: f64, seed: u64) -> Vec<f32> {
    let n = (seconds * RATE as f64) as usize;
    let mut chirp_rng = Xs::seeded(seed);
    let mut pink_rng = Xs::seeded(seed ^ 0xFFFF_0000_1111);
    let c = chirp_train(&mut chirp_rng, n);
    let p = pink(&mut pink_rng, n);
    let raw: Vec<f64> = c.iter().zip(&p).map(|(a, b)| 0.5 * a + 0.25 * b).collect();
    let peak = raw.iter().fold(1e-9f64, |m, x| m.max(x.abs()));
    raw.iter().map(|x| (0.5 * x / peak) as f32).collect()
}

/// Flat white noise (spectrally flat, no temporal structure).
fn white(seconds: f64, seed: u64, gain: f64) -> Vec<f32> {
    let mut rng = Xs::seeded(seed);
    (0..(seconds * RATE as f64) as usize)
        .map(|_| (rng.range(-1.0, 1.0) * gain) as f32)
        .collect()
}

/// Metronome bait: 1 kHz decaying clicks every 0.5 s.
fn metronome(seconds: f64) -> Vec<f32> {
    let n = (seconds * RATE as f64) as usize;
    let mut out = vec![0.0f32; n];
    let mut t = 0usize;
    while t < n {
        for k in 0..(RATE / 200).min(n - t) {
            let env = (-(k as f64) / 40.0).exp();
            out[t + k] +=
                (0.6 * env * (std::f64::consts::TAU * 1000.0 * k as f64 / RATE as f64).sin())
                    as f32;
        }
        t += RATE / 2;
    }
    out
}

fn spec(seconds: usize) -> SoundtrackReferenceSpec {
    SoundtrackReferenceSpec {
        schema_version: 1,
        movie: SyncMovieClock {
            movie_digest: "b".repeat(64),
            time_base_numerator: 1,
            time_base_denominator: 90_000,
            origin_pts: 0,
            duration_pts: seconds as u64 * 90_000,
        },
        start_seconds: 0.0,
        duration_seconds: seconds as f64,
        recipe: None,
    }
}

fn capture() -> SoundtrackCapture {
    SoundtrackCapture {
        evidence_id: "t003_indep_query".into(),
        clock_id: "mic_t003".into(),
        sample_rate: 48_000,
        first_sample: 0,
        monotonic_origin_ns: 0,
    }
}

fn policy(d: Option<DistinctivePolicy>) -> SoundtrackPolicy {
    SoundtrackPolicy {
        distinctive: d,
        ..SoundtrackPolicy::default()
    }
}

fn run(index: &SoundtrackIndex, pcm: &[f32], p: &SoundtrackPolicy) -> SoundtrackMatchReport {
    index.match_pcm(pcm, capture(), p, &|| false).unwrap()
}

/// Mix a buried excerpt with independent scene audio; `gain` sets the SNR.
fn buried(reference: &[f32], at_seconds: f64, len_seconds: f64, gain: f32, seed: u64) -> Vec<f32> {
    let start = (at_seconds * RATE as f64) as usize;
    let len = (len_seconds * RATE as f64) as usize;
    let other = scene(len_seconds, seed);
    reference[start..start + len]
        .iter()
        .zip(&other)
        .map(|(c, o)| c + gain * o)
        .collect()
}

/// Repeated copy of the excerpt at 25 s, exact, so the query has two equal homes.
fn with_copy(base: &[f32], at_seconds: f64, src: &[f32]) -> Vec<f32> {
    let mut out = base.to_vec();
    let at = (at_seconds * RATE as f64) as usize;
    out[at..at + src.len()].copy_from_slice(src);
    out
}

fn add_metronome(base: &[f32], from: f64, to: f64) -> Vec<f32> {
    let mut out = base.to_vec();
    let click = metronome(to - from);
    let at = (from * RATE as f64) as usize;
    for (k, c) in click.iter().enumerate() {
        out[at + k] += c;
    }
    out
}

fn distinctive() -> DistinctivePolicy {
    DistinctivePolicy {
        minimum_similarity: 0.5,
        minimum_margin: 0.25,
        minimum_z_score: 6.0,
    }
}

const TRUTH_SECONDS: f64 = 33.0; // excerpt starts at 30 s, centre at 33 s
const TOLERANCE_SECONDS: f64 = 0.1;

fn centre_error_seconds(c: &rcdvlog_core::tv_soundtrack::SoundtrackCandidate, truth: f64) -> f64 {
    let truth_pts = truth * 90_000.0;
    (c.anchor.movie_pts as f64 - truth_pts).abs() / 90_000.0
}

/// Shared fixture: 40 s reference with a metronome bait section, and a 6 s
/// excerpt at 30 s mixed with independent scene audio at SNR gain 1.1.
fn fixture() -> (SoundtrackIndex, Vec<f32>, Vec<f32>) {
    let base = scene(40.0, 0x51);
    let reference = add_metronome(&base, 2.0, 8.0);
    let index = SoundtrackIndex::from_pcm(spec(40), &reference, &|| false).unwrap();
    let query = buried(&reference, 30.0, 6.0, 1.1, 0x77);
    (index, reference, query)
}

#[test]
fn oracle_unique_buried_excerpt_matches_with_distinctive_reason_and_confidence() {
    let (index, _reference, query) = fixture();
    let r = run(&index, &query, &policy(None));
    assert!(
        r.best_similarity.unwrap() < 0.8,
        "old path must not match: {r:?}"
    );
    let p = policy(Some(distinctive()));
    let d = run(&index, &query, &p);
    let best = d.best_similarity.unwrap();
    assert!(
        (0.5..=0.7).contains(&best),
        "calibration: best similarity {best}"
    );
    assert_eq!(d.state, SoundtrackMatchState::Matched, "{d:?}");
    assert_eq!(d.reason, "distinctive_peak_low_similarity", "{d:?}");
    let z = d.distinctiveness.expect("distinctiveness must be Some");
    assert!(z >= 6.0, "z {z} below 6.0");
    assert_eq!(d.candidates.len(), 1, "{d:?}");
    let c = &d.candidates[0];
    assert!(c.peak_margin >= 0.25, "margin {}", c.peak_margin);
    assert!(
        (c.anchor.confidence - (0.5 + c.peak_margin).min(1.0)).abs() < 1e-12,
        "confidence {} vs min(1, 0.5 + margin) {}",
        c.anchor.confidence,
        (0.5 + c.peak_margin).min(1.0)
    );
    assert!(
        centre_error_seconds(c, TRUTH_SECONDS) <= TOLERANCE_SECONDS,
        "centre error {} s: {c:?}",
        centre_error_seconds(c, TRUTH_SECONDS)
    );
}

#[test]
fn oracle_each_distinctive_threshold_is_required() {
    let (index, _reference, query) = fixture();
    let d = run(&index, &query, &policy(Some(distinctive())));
    assert_eq!(d.state, SoundtrackMatchState::Matched, "{d:?}");
    let best = d.best_similarity.unwrap();
    let z = d.distinctiveness.unwrap();
    for (name, dp) in [
        (
            "similarity",
            DistinctivePolicy {
                minimum_similarity: best + 0.02,
                ..distinctive()
            },
        ),
        (
            "z",
            DistinctivePolicy {
                minimum_z_score: z + 0.5,
                ..distinctive()
            },
        ),
    ] {
        let r = run(&index, &query, &policy(Some(dp)));
        assert_ne!(
            r.state,
            SoundtrackMatchState::Matched,
            "{name} threshold not enforced: {r:?}"
        );
        assert!(r.candidates.is_empty(), "{name}: candidate emitted {r:?}");
    }
    // Margin: a real supported runner-up at 12 s (noisier copy of the excerpt).
    let base = scene(40.0, 0x51);
    let excerpt = base[30 * RATE..36 * RATE].to_vec();
    let decoy: Vec<f32> = excerpt
        .iter()
        .zip(scene(6.0, 0x55))
        .map(|(c, o)| c + 1.5 * o)
        .collect();
    let reference = with_copy(&base, 12.0, &decoy);
    let index2 = SoundtrackIndex::from_pcm(spec(40), &reference, &|| false).unwrap();
    let q2 = buried(&base, 30.0, 6.0, 1.1, 0x77);
    let loose = run(
        &index2,
        &q2,
        &policy(Some(DistinctivePolicy {
            minimum_margin: 0.1,
            ..distinctive()
        })),
    );
    let real_margin = loose.candidates[0].peak_margin;
    assert!(
        real_margin < 1.0,
        "runner-up must be temporally supported: {loose:?}"
    );
    assert_eq!(
        loose.state,
        SoundtrackMatchState::Matched,
        "loose margin should accept: {loose:?}"
    );
    let tight = run(
        &index2,
        &q2,
        &policy(Some(DistinctivePolicy {
            minimum_margin: real_margin + 0.02,
            ..distinctive()
        })),
    );
    assert_ne!(
        tight.state,
        SoundtrackMatchState::Matched,
        "margin threshold not enforced: {tight:?}"
    );
    assert!(tight.candidates.is_empty(), "{tight:?}");
}

#[test]
fn oracle_distinctive_none_keeps_same_evidence_unmatched_and_omits_field() {
    let (index, _reference, query) = fixture();
    let r = run(&index, &query, &policy(None));
    assert_ne!(r.state, SoundtrackMatchState::Matched, "{r:?}");
    assert!(r.candidates.is_empty(), "{r:?}");
    assert!(r.distinctiveness.is_none());
    let json = serde_json::to_value(&r).unwrap();
    assert!(
        json.get("distinctiveness").is_none(),
        "field leaked: {json}"
    );
    assert!(
        json["policy"].get("distinctive").is_none(),
        "policy leaked: {json}"
    );
    assert_eq!(r, run(&index, &query, &policy(None)), "non-deterministic");
}

#[test]
fn oracle_repeated_content_at_two_distant_positions_is_not_matched() {
    let base = scene(40.0, 0x51);
    let excerpt = base[10 * RATE..16 * RATE].to_vec();
    let reference = with_copy(&base, 25.0, &excerpt);
    let index = SoundtrackIndex::from_pcm(spec(40), &reference, &|| false).unwrap();
    let query: Vec<f32> = excerpt
        .iter()
        .zip(scene(6.0, 0x77))
        .map(|(c, o)| c + 1.1 * o)
        .collect();
    let d = run(&index, &query, &policy(Some(distinctive())));
    assert_ne!(
        d.state,
        SoundtrackMatchState::Matched,
        "repeated content matched: {d:?}"
    );
    assert!(
        d.candidates.iter().all(|c| c.peak_margin < 0.25),
        "repeat accepted with a large margin: {d:?}"
    );
}

#[test]
fn oracle_short_repeat_with_partial_second_copy_must_not_inflate_margin() {
    // Second copy at 25 s shares 4 s of the 6 s excerpt but fails third-support.
    // Its raw score is a real competitor, so the margin must not ignore it.
    let base = scene(40.0, 0x51);
    let excerpt = base[10 * RATE..16 * RATE].to_vec();
    let mut partial = excerpt.clone();
    partial[0..2 * RATE].copy_from_slice(&scene(2.0, 0x99));
    let reference = with_copy(&base, 25.0, &partial);
    let index = SoundtrackIndex::from_pcm(spec(40), &reference, &|| false).unwrap();
    let query: Vec<f32> = excerpt
        .iter()
        .zip(scene(6.0, 0x77))
        .map(|(c, o)| c + 1.3 * o)
        .collect();
    let d = run(&index, &query, &policy(Some(distinctive())));
    let best_raw = d.best_peaks[0].similarity;
    let second_raw = d.best_peaks[1].similarity;
    let honest_margin = best_raw - second_raw;
    println!(
        "FP-PROBE state {:?} reason {} reported_margin {:?} confidence {:?} best_raw {best_raw} second_raw {second_raw} honest_margin {honest_margin} centre_error {:?}",
        d.state,
        d.reason,
        d.candidates.first().map(|c| c.peak_margin),
        d.candidates.first().map(|c| c.anchor.confidence),
        d.candidates.first().map(|c| centre_error_seconds(c, 13.0)),
    );
    if d.state == SoundtrackMatchState::Matched {
        assert!(
            centre_error_seconds(&d.candidates[0], 13.0) <= TOLERANCE_SECONDS,
            "wrong position: {d:?}"
        );
    }
    assert!(
        d.state != SoundtrackMatchState::Matched || honest_margin >= 0.25,
        "false positive risk: Matched with raw gap {honest_margin} < 0.25 to a temporally unsupported copy; reported margin {:?}",
        d.candidates.first().map(|c| c.peak_margin)
    );
}

#[test]
fn oracle_unrelated_flat_and_quiet_audio_is_not_matched() {
    let (index, _reference, _query) = fixture();
    let p = policy(Some(distinctive()));
    let unrelated: Vec<f32> = scene(6.0, 0xBEEF);
    let r = run(&index, &unrelated, &p);
    assert_ne!(
        r.state,
        SoundtrackMatchState::Matched,
        "unrelated matched: {r:?}"
    );
    let flat = white(6.0, 0x3C, 0.3);
    let r = run(&index, &flat, &p);
    assert_ne!(
        r.state,
        SoundtrackMatchState::Matched,
        "flat noise matched: {r:?}"
    );
    assert!(r.distinctiveness.is_none_or(|z| z < 6.0), "flat z: {r:?}");
    let quiet: Vec<f32> = scene(6.0, 0x77).iter().map(|x| x * 0.001).collect();
    let r = run(&index, &quiet, &p);
    assert_ne!(
        r.state,
        SoundtrackMatchState::Matched,
        "quiet matched: {r:?}"
    );
    let zero = vec![0.0f32; 6 * RATE];
    assert_eq!(
        run(&index, &zero, &p).state,
        SoundtrackMatchState::Unmatched
    );
}

#[test]
fn oracle_metronome_bait_never_yields_a_wrong_unique_anchor() {
    let (index, _reference, _query) = fixture();
    let p = policy(Some(distinctive()));
    let click = metronome(6.0);
    let r = run(&index, &click, &p);
    assert_ne!(
        r.state,
        SoundtrackMatchState::Matched,
        "periodic bait matched: {r:?}"
    );
    assert!(r.candidates.is_empty(), "{r:?}");
}
