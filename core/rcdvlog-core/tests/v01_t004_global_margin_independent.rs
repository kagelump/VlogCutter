//! V01-T004 independent oracle for the ARCHITECTURE section 5 global margin.
//!
//! Audio comes from a generator separate from the implementer's LCG fixtures and
//! from the T003 xorshift chirp train: a wyrand (128-bit multiply) stream drives
//! exponential-sweep chirp trains with their own seeds. Everything goes through
//! the public plan -> run_batch -> evidence_page API on real WAV files in a
//! tempdir. The only private access is receipt editing, done through
//! serde_json::Value against the documented cache layout, to place rival peaks
//! at exact movie offsets.

use std::path::{Path, PathBuf};

use rcdvlog_core::{
    full_file_blake3,
    tv_review_sync::{CaptureClock, SyncMovieClock, SyncPolicy, SyncRequest},
    tv_soundtrack::{
        DistinctivePolicy, SoundtrackMatchReport, SoundtrackMatchState, SoundtrackPolicy,
    },
    tv_soundtrack_session::{
        SessionAudioFile, SessionAudioSegment, SessionBatch, SoundtrackSessionPlan,
        SoundtrackSessionSpec,
    },
};
use serde_json::{Value, json};

const RATE: u32 = 8000;
const MOVIE_SECONDS: usize = 150;
const BLOCK_SECONDS: usize = 6;
/// Occurrences 125 s apart: strictly more than the 120 s repeat in the charter.
const FIRST_AT: f64 = 10.0;
const SECOND_AT: f64 = 135.0;
/// Query centre is the middle of the 6 s block.
const CENTRE_OFFSET: f64 = 3.0;
const _: () = assert!(SECOND_AT - FIRST_AT > 120.0);

/// wyrand: a 64-bit stream using 128-bit multiplication. Seeds differ from all
/// other generators in this crate's tests.
struct Wy(u64);
impl Wy {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x5851_F42D_4C95_7F2D)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0xA076_1D64_78BD_642F);
        let t = u128::from(self.0).wrapping_mul(u128::from(self.0 ^ 0xE703_7ED1_A0B4_28DB));
        ((t >> 64) as u64) ^ (t as u64)
    }
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }
}

/// Exponential-sweep chirps with random onsets, lengths and gaps.
fn chirp_train(seed: u64, seconds: usize) -> Vec<f32> {
    let n = seconds * RATE as usize;
    let mut rng = Wy::new(seed);
    let mut out = vec![0.0f32; n];
    let mut t = 0usize;
    while t < n {
        let len = (rng.range(0.03, 0.15) * f64::from(RATE)) as usize;
        let lo = rng.range(300.0, 900.0).ln();
        let hi = rng.range(900.0, 3000.0).ln();
        let amp = rng.range(0.2, 0.6);
        let mut phase = 0.0f64;
        for k in 0..len.min(n - t) {
            let u = k as f64 / len as f64;
            let f = (lo + (hi - lo) * u).exp();
            phase += std::f64::consts::TAU * f / f64::from(RATE);
            let env = (std::f64::consts::PI * u).sin().powi(2);
            out[t + k] += (amp * env * phase.sin()) as f32;
        }
        t += len + (rng.range(0.0, 0.08) * f64::from(RATE)) as usize;
    }
    out
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

/// A movie of `MOVIE_SECONDS` of background chirps, with `block` written at each
/// occurrence start. The mic recording is the block itself.
struct Fixture {
    _dir: tempfile::TempDir,
    plan: SoundtrackSessionPlan,
    movie: PathBuf,
    files: Vec<SessionAudioFile>,
    cache: PathBuf,
}

fn fixture(occurrences: &[f64], match_policy: SoundtrackPolicy) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let block = chirp_train(0x0B10_C0DE_0004, BLOCK_SECONDS);
    let mut movie_pcm = chirp_train(0x00BA_C600_0004, MOVIE_SECONDS);
    for at in occurrences {
        let start = (*at * f64::from(RATE)) as usize;
        movie_pcm[start..start + block.len()].copy_from_slice(&block);
    }
    let movie = dir.path().join("movie mix.wav");
    write_wav(&movie, &movie_pcm);
    let mic = dir.path().join("room mic.wav");
    write_wav(&mic, &block);
    let movie_digest = full_file_blake3(&movie, &|| false).unwrap().hex_digest;
    let mic_digest = full_file_blake3(&mic, &|| false).unwrap().hex_digest;
    let mic_frames = block.len() as u64;
    let plan = SoundtrackSessionPlan::new(SoundtrackSessionSpec {
        schema_version: 1,
        sync: SyncRequest {
            schema_version: 1,
            session_id: "independent_session".into(),
            target_digest: "c".repeat(64),
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
        reference_seconds: 12,
        query_seconds: 6,
        query_step_seconds: 6,
        match_policy,
        speech_mask: vec![],
        recipe: None,
    })
    .unwrap();
    assert_eq!(plan.queries.len(), 1, "one 6 s query expected");
    assert!(
        plan.references.len() >= 20,
        "expected ~24 overlapping references"
    );
    let cache = dir.path().join("cache");
    Fixture {
        plan,
        movie,
        files: vec![SessionAudioFile {
            segment_id: "room".into(),
            path: mic,
        }],
        cache,
        _dir: dir,
    }
}

fn run_all(f: &Fixture) {
    let result = f
        .plan
        .run_batch(
            SessionBatch {
                movie: &f.movie,
                files: &f.files,
                cache: &f.cache,
                start_task: 0,
                max_tasks: 128,
            },
            &|| false,
        )
        .unwrap();
    assert_eq!(result.next_task, f.plan.task_count);
    assert!(!result.cancelled);
}

/// Movie-time seconds of an anchor's query centre.
fn anchor_seconds(f: &Fixture, pts: i64) -> f64 {
    (pts - f.plan.spec.sync.movie.origin_pts) as f64 / 90000.0
}

fn receipt_path(f: &Fixture, ri: usize) -> PathBuf {
    f.cache
        .join(&f.plan.plan_digest)
        .join(format!("task-{ri}.json"))
}

/// Adds a rival peak to every cached receipt. The rival copies the top peak,
/// lowered by `gap` similarity and moved to `movie_seconds`, and the report digest
/// is recomputed so the receipt still verifies.
fn inject_rival(f: &Fixture, gap: f64, movie_seconds: f64) {
    for ri in 0..f.plan.references.len() {
        let path = receipt_path(f, ri);
        let mut receipt: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let peaks = receipt["report"]["bestPeaks"].as_array_mut().unwrap();
        let mut rival = peaks[0].clone();
        let similarity = rival["similarity"].as_f64().unwrap() - gap;
        rival["similarity"] = json!(similarity);
        rival["movieSeconds"] = json!(movie_seconds);
        peaks.push(rival);
        let report: SoundtrackMatchReport =
            serde_json::from_value(receipt["report"].clone()).unwrap();
        receipt["reportDigest"] = json!(
            blake3::hash(&serde_json::to_vec(&report).unwrap())
                .to_hex()
                .to_string()
        );
        std::fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    }
}

/// Movie seconds of the globally strongest peak: the matched occurrence. Each
/// reference's own first peak is only local, so the maximum across references is
/// the peak the session margin is measured against.
fn top_peak_seconds(f: &Fixture) -> f64 {
    let mut best: Option<(f64, f64)> = None;
    for ri in 0..f.plan.references.len() {
        let receipt: Value =
            serde_json::from_slice(&std::fs::read(receipt_path(f, ri)).unwrap()).unwrap();
        let peak = &receipt["report"]["bestPeaks"][0];
        let similarity = peak["similarity"].as_f64().unwrap();
        let seconds = peak["movieSeconds"].as_f64().unwrap();
        if best.is_none_or(|(s, _)| similarity > s) {
            best = Some((similarity, seconds));
        }
    }
    let (_, seconds) = best.unwrap();
    assert!(
        (seconds - (FIRST_AT + CENTRE_OFFSET)).abs() < 0.5,
        "global top at {seconds} s, expected near the occurrence"
    );
    seconds
}

fn matched_references(f: &Fixture) -> usize {
    (0..f.plan.references.len())
        .filter(|ri| {
            let receipt: Value =
                serde_json::from_slice(&std::fs::read(receipt_path(f, *ri)).unwrap()).unwrap();
            let report: SoundtrackMatchReport =
                serde_json::from_value(receipt["report"].clone()).unwrap();
            report.state == SoundtrackMatchState::Matched
        })
        .count()
}

#[test]
fn repeat_120s_apart_under_distinctive_is_ambiguous_with_zero_anchors() {
    let f = fixture(&[FIRST_AT, SECOND_AT], distinctive_policy());
    run_all(&f);
    // Each reference alone must see a match, otherwise the falsification is vacuous.
    assert!(
        matched_references(&f) >= 1,
        "no single reference reported Matched; the scenario is vacuous"
    );
    let page = f.plan.evidence_page(&f.cache, 0, 1, &|| false).unwrap();
    let evidence = &page.queries[0];
    assert_eq!(evidence.state, "ambiguous", "{page:?}");
    assert!(evidence.anchors.is_empty(), "{evidence:?}");
    assert!(evidence.supporting_evidence_ids.is_empty());
    assert!(page.sync.anchors.is_empty(), "{page:?}");
    // Both occurrences must appear as merged peaks, so the rival was really seen.
    let near = |target: f64| {
        evidence
            .best_peaks
            .iter()
            .any(|p| (p.movie_seconds - (target + CENTRE_OFFSET)).abs() < 0.5)
    };
    assert!(near(FIRST_AT), "{:?}", evidence.best_peaks);
    assert!(near(SECOND_AT), "{:?}", evidence.best_peaks);
}

#[test]
fn single_unique_occurrence_under_distinctive_stays_candidate_with_one_anchor() {
    let f = fixture(&[FIRST_AT], distinctive_policy());
    run_all(&f);
    let page = f.plan.evidence_page(&f.cache, 0, 1, &|| false).unwrap();
    let evidence = &page.queries[0];
    assert_eq!(evidence.state, "candidate", "{page:?}");
    assert_eq!(evidence.anchors.len(), 1, "{evidence:?}");
    assert_eq!(page.sync.anchors.len(), 1);
    let seconds = anchor_seconds(&f, evidence.anchors[0].movie_pts);
    assert!(
        (seconds - (FIRST_AT + CENTRE_OFFSET)).abs() < 0.1,
        "anchor at {seconds} s, expected {} s",
        FIRST_AT + CENTRE_OFFSET
    );
}

#[test]
fn distinctive_none_keeps_v1_ambiguous_with_conflicting_alternative_anchors() {
    let f = fixture(&[FIRST_AT, SECOND_AT], SoundtrackPolicy::default());
    run_all(&f);
    let page = f.plan.evidence_page(&f.cache, 0, 1, &|| false).unwrap();
    let evidence = &page.queries[0];
    assert_eq!(evidence.state, "ambiguous", "{page:?}");
    // ARCHITECTURE section 2 (2026-11-03 amendment): v1 ambiguous keeps alternatives.
    assert!(evidence.anchors.len() >= 2, "{evidence:?}");
    for target in [FIRST_AT, SECOND_AT] {
        assert!(
            evidence
                .anchors
                .iter()
                .any(|a| (anchor_seconds(&f, a.movie_pts) - (target + CENTRE_OFFSET)).abs() < 0.1),
            "no alternative anchor near {} s: {evidence:?}",
            target + CENTRE_OFFSET
        );
    }
    assert_eq!(page.sync.anchors.len(), evidence.anchors.len());
}

#[test]
fn rival_inside_merge_distance_is_not_a_rival() {
    // 0.25 s is inside the 0.256 s neighbourhood: merged away, never a rival.
    let f = fixture(&[FIRST_AT], distinctive_policy());
    run_all(&f);
    let top = top_peak_seconds(&f);
    inject_rival(&f, 0.01, top + 0.25);
    let page = f.plan.evidence_page(&f.cache, 0, 1, &|| false).unwrap();
    assert_eq!(page.queries[0].state, "candidate", "{page:?}");
    assert_eq!(page.sync.anchors.len(), 1, "{page:?}");
}

#[test]
fn rival_beyond_merge_distance_breaks_margin_control() {
    // Control: 0.26 s is outside the neighbourhood, so the same rival must count.
    let f = fixture(&[FIRST_AT], distinctive_policy());
    run_all(&f);
    let top = top_peak_seconds(&f);
    inject_rival(&f, 0.01, top + 0.26);
    let page = f.plan.evidence_page(&f.cache, 0, 1, &|| false).unwrap();
    assert_eq!(page.queries[0].state, "ambiguous", "{page:?}");
    assert!(page.sync.anchors.is_empty(), "{page:?}");
}
