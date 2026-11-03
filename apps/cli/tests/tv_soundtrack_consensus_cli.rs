//! CLI wiring for `tv-review soundtrack-session-consensus`: an incomplete search
//! (no cached evidence) must be rejected with `search_incomplete`.
use rcdvlog_core::{
    tv_review_sync::{CaptureClock, SyncMovieClock, SyncPolicy, SyncRequest},
    tv_soundtrack::SoundtrackPolicy,
    tv_soundtrack_session::{SessionAudioSegment, SoundtrackSessionSpec},
};
use serde_json::{Value, json};
use std::process::Command;

fn run(args: &[&str]) -> (bool, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .args(args)
        .output()
        .unwrap();
    // Success envelopes go to stdout; failure envelopes go to stderr.
    let stream = if out.status.success() {
        &out.stdout
    } else {
        &out.stderr
    };
    let envelope: Value = serde_json::from_slice(stream).unwrap_or_else(|e| {
        panic!(
            "envelope is not JSON ({e}): {}",
            String::from_utf8_lossy(stream)
        )
    });
    (out.status.success(), envelope)
}

#[test]
fn consensus_on_empty_cache_is_rejected_as_search_incomplete() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let digest = "a".repeat(64);
    let spec = SoundtrackSessionSpec {
        schema_version: 1,
        sync: SyncRequest {
            schema_version: 1,
            session_id: "consensus_cli_empty_cache".into(),
            target_digest: digest.clone(),
            movie: SyncMovieClock {
                movie_digest: digest.clone(),
                time_base_numerator: 1,
                time_base_denominator: 90000,
                origin_pts: 0,
                duration_pts: 120 * 90000,
            },
            capture_clocks: vec![CaptureClock {
                clock_id: "native".into(),
                sample_rate: 48000,
                first_sample: 0,
                last_sample: 60 * 48000,
                monotonic_origin_ns: 1_000_000_000,
                monotonic_tolerance_ns: 1,
            }],
            anchors: vec![],
            boundaries: vec![],
            policy: SyncPolicy::default(),
            consensus_segments: vec![],
        },
        segments: vec![SessionAudioSegment {
            segment_id: "capture".into(),
            clock_id: "native".into(),
            first_sample: 0,
            frame_count: 60 * 48000,
            wav_blake3: digest,
        }],
        reference_seconds: 12,
        query_seconds: 6,
        query_step_seconds: 6,
        match_policy: SoundtrackPolicy::default(),
        speech_mask: vec![],
        recipe: None,
    };
    let input = root.join("spec.json");
    std::fs::write(&input, serde_json::to_vec(&spec).unwrap()).unwrap();
    let (ok, planned) = run(&[
        "tv-review",
        "soundtrack-session-plan",
        "--input",
        input.to_str().unwrap(),
    ]);
    assert!(ok, "{planned}");
    let plan_path = root.join("plan.json");
    std::fs::write(&plan_path, serde_json::to_vec(&planned["data"]).unwrap()).unwrap();

    let cache = root.join("empty cache");
    std::fs::create_dir(&cache).unwrap();
    let (ok, envelope) = run(&[
        "tv-review",
        "soundtrack-session-consensus",
        "--plan",
        plan_path.to_str().unwrap(),
        "--cache",
        cache.to_str().unwrap(),
    ]);
    assert!(!ok, "{envelope}");
    assert_eq!(envelope["ok"], json!(false));
    let message = envelope["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("search_incomplete"),
        "expected search_incomplete, got: {envelope}"
    );
}
