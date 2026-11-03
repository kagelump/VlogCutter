//! V01-T009 independent oracle: recipe v2 (ARCHITECTURE section 7) through
//! the public session API (plan -> run_batch -> evidence_page) on real WAV
//! files. Audio comes from a splitmix64-driven generator of gated formant
//! tones, separate from every other fixture generator in this crate.

use std::path::{Path, PathBuf};

use rcdvlog_core::{
    full_file_blake3,
    tv_review_sync::{CaptureClock, SyncMovieClock, SyncPolicy, SyncRequest},
    tv_soundtrack::{SOUNDTRACK_RECIPE, SOUNDTRACK_RECIPE_V2, SoundtrackIndex, SoundtrackPolicy},
    tv_soundtrack_session::{
        SessionAudioFile, SessionAudioSegment, SessionBatch, SoundtrackSessionPlan,
        SoundtrackSessionSpec,
    },
};

const RATE: u32 = 8000;
const MOVIE_SECONDS: usize = 90;
const BLOCK_SECONDS: usize = 8;
const BLOCK_AT: f64 = 40.0;

struct Mix(u64);
impl Mix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Gated formant-like tones: random 80-400 ms notes, random pitch, with
/// random 0-150 ms rests; `quiet_every` makes every n-th second -34 dB.
fn tones(seed: u64, seconds: usize, quiet_every: Option<usize>) -> Vec<f32> {
    let n = seconds * RATE as usize;
    let mut rng = Mix(seed);
    let mut out = vec![0.0f32; n];
    let mut i = 0usize;
    while i < n {
        let len = (0.08 + 0.32 * rng.unit()) * f64::from(RATE);
        let f0 = 120.0 + 500.0 * rng.unit();
        let f1 = f0 * (2.0 + 2.0 * rng.unit());
        for k in 0..len as usize {
            if i + k >= n {
                break;
            }
            let t = k as f64 / f64::from(RATE);
            let env = (std::f64::consts::PI * k as f64 / len).sin();
            out[i + k] = (0.25
                * env
                * ((std::f64::consts::TAU * f0 * t).sin()
                    + 0.5 * (std::f64::consts::TAU * f1 * t).sin()))
                as f32;
        }
        i += len as usize + (0.15 * rng.unit() * f64::from(RATE)) as usize;
    }
    for (j, x) in out.iter_mut().enumerate() {
        *x += ((rng.unit() - 0.5) * 0.01) as f32;
        if let Some(q) = quiet_every
            && (j / RATE as usize) % q == q - 1
        {
            *x *= 0.02;
        }
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

struct Session {
    _dir: tempfile::TempDir,
    movie: PathBuf,
    mic: PathBuf,
    movie_pcm: Vec<f32>,
}

fn session(mic_talker: bool) -> Session {
    let dir = tempfile::tempdir().unwrap();
    let mut movie_pcm = tones(0x7009_0001, MOVIE_SECONDS, Some(2));
    let start = (BLOCK_AT * f64::from(RATE)) as usize;
    let mut mic_pcm = movie_pcm[start..start + BLOCK_SECONDS * RATE as usize].to_vec();
    if mic_talker {
        // A talker in the room over the quiet seconds of the soundtrack.
        let talker = tones(0x7009_7A1C, BLOCK_SECONDS, None);
        for (j, x) in mic_pcm.iter_mut().enumerate() {
            if ((start + j) / RATE as usize) % 2 == 1 {
                *x += 0.6 * talker[j];
            }
        }
    }
    // Keep the movie exactly as indexed (no talker).
    movie_pcm.truncate(MOVIE_SECONDS * RATE as usize);
    let movie = dir.path().join("final mix.wav");
    write_wav(&movie, &movie_pcm);
    let mic = dir.path().join("room.wav");
    write_wav(&mic, &mic_pcm);
    Session {
        _dir: dir,
        movie,
        mic,
        movie_pcm,
    }
}

fn plan(s: &Session, recipe: Option<&str>) -> SoundtrackSessionPlan {
    let frames = (BLOCK_SECONDS * RATE as usize) as u64;
    SoundtrackSessionPlan::new(SoundtrackSessionSpec {
        schema_version: 1,
        sync: SyncRequest {
            schema_version: 1,
            session_id: "t009_session".into(),
            target_digest: "d".repeat(64),
            movie: SyncMovieClock {
                movie_digest: full_file_blake3(&s.movie, &|| false).unwrap().hex_digest,
                time_base_numerator: 1,
                time_base_denominator: 90000,
                origin_pts: 0,
                duration_pts: MOVIE_SECONDS as u64 * 90000,
            },
            capture_clocks: vec![CaptureClock {
                clock_id: "mic".into(),
                sample_rate: RATE,
                first_sample: 0,
                last_sample: frames,
                monotonic_origin_ns: 1,
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
            frame_count: frames as u32,
            wav_blake3: full_file_blake3(&s.mic, &|| false).unwrap().hex_digest,
        }],
        reference_seconds: 30,
        query_seconds: 8,
        query_step_seconds: 8,
        match_policy: SoundtrackPolicy::default(),
        speech_mask: vec![],
        recipe: recipe.map(str::to_owned),
    })
    .unwrap()
}

fn run(s: &Session, p: &SoundtrackSessionPlan, cache: &Path) {
    let mut next = 0;
    let files = vec![SessionAudioFile {
        segment_id: "room".into(),
        path: s.mic.clone(),
    }];
    while next < p.task_count {
        let r = p
            .run_batch(
                SessionBatch {
                    movie: &s.movie,
                    files: &files,
                    cache,
                    start_task: next,
                    max_tasks: 64,
                },
                &|| false,
            )
            .unwrap();
        next = r.next_task;
    }
}

fn anchors(p: &SoundtrackSessionPlan, cache: &Path) -> Vec<f64> {
    p.evidence_page(cache, 0, 1, &|| false).unwrap().queries[0]
        .anchors
        .iter()
        .map(|a| a.movie_pts as f64 / 90000.0)
        .collect()
}

#[test]
fn v1_and_v2_sessions_have_separate_identities_caches_and_indexes() {
    let s = session(false);
    let p1 = plan(&s, None);
    let p2 = plan(&s, Some(SOUNDTRACK_RECIPE_V2));
    assert_ne!(p1.plan_digest, p2.plan_digest);
    assert!(p1.references.iter().all(|r| r.recipe.is_none()));
    assert!(
        p2.references
            .iter()
            .all(|r| r.recipe.as_deref() == Some(SOUNDTRACK_RECIPE_V2))
    );
    let cache = s._dir.path().join("cache");
    run(&s, &p1, &cache);
    run(&s, &p2, &cache);
    // Two distinct cache roots, each holding its own recipe's indexes.
    let roots: Vec<_> = std::fs::read_dir(&cache)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(roots.len(), 2, "{roots:?}");
    for (p, recipe) in [(&p1, SOUNDTRACK_RECIPE), (&p2, SOUNDTRACK_RECIPE_V2)] {
        let index_path = cache.join(&p.plan_digest).join("index-0.json");
        let index = SoundtrackIndex::from_json(&std::fs::read(index_path).unwrap()).unwrap();
        assert_eq!(index.recipe, recipe);
        assert_eq!(index.frame_energy.is_empty(), recipe == SOUNDTRACK_RECIPE);
        // Both anchor the clean block at its centre.
        let a = anchors(p, &cache);
        assert_eq!(a.len(), 1, "{recipe}: {a:?}");
        assert!((a[0] - (BLOCK_AT + BLOCK_SECONDS as f64 / 2.0)).abs() < 0.05);
    }
    // Indexes of the same window under the two recipes have different digests.
    let i1 = SoundtrackIndex::from_json(
        &std::fs::read(cache.join(&p1.plan_digest).join("index-0.json")).unwrap(),
    )
    .unwrap();
    let i2 = SoundtrackIndex::from_json(
        &std::fs::read(cache.join(&p2.plan_digest).join("index-0.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(i1.pcm_digest, i2.pcm_digest);
    assert_ne!(i1.index_digest, i2.index_digest);
    assert!(!s.movie_pcm.is_empty());
}

#[test]
fn v2_anchors_a_room_recording_with_a_talker_over_quiet_soundtrack() {
    let s = session(true);
    let p2 = plan(&s, Some(SOUNDTRACK_RECIPE_V2));
    let cache = s._dir.path().join("cache");
    run(&s, &p2, &cache);
    let a = anchors(&p2, &cache);
    assert_eq!(a.len(), 1, "{a:?}");
    assert!((a[0] - (BLOCK_AT + BLOCK_SECONDS as f64 / 2.0)).abs() < 0.05);
    // v1 is recorded for comparison only: it must never anchor elsewhere.
    let p1 = plan(&s, None);
    run(&s, &p1, &cache);
    for t in anchors(&p1, &cache) {
        assert!((t - (BLOCK_AT + BLOCK_SECONDS as f64 / 2.0)).abs() < 0.05);
    }
}

#[test]
fn unknown_or_v1_named_recipes_are_rejected() {
    let s = session(false);
    let ok = plan(&s, None);
    let mut spec = ok.spec.clone();
    spec.recipe = Some("soundtrack.something-else.v9".into());
    assert!(SoundtrackSessionPlan::new(spec).is_err());
    let mut spec = ok.spec.clone();
    spec.recipe = Some(SOUNDTRACK_RECIPE.into());
    assert!(
        SoundtrackSessionPlan::new(spec).is_err(),
        "v1 is spelt None, never by name"
    );
}
