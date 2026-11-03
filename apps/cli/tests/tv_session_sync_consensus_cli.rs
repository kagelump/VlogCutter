//! CLI wiring for `tv-review soundtrack-session-sync --consensus FILE` (§9).
//! Without the flag the command must behave exactly as before; a missing,
//! unreadable, malformed or foreign consensus file must fail with a clear
//! envelope error rather than a panic.
use rcdvlog_core::{
    tv_review_sync::{CaptureClock, SyncMovieClock, SyncPolicy, SyncQuery, SyncRequest},
    tv_soundtrack::SoundtrackPolicy,
    tv_soundtrack_consensus::{ConsensusSegment, SoundtrackConsensus},
    tv_soundtrack_session::{SessionAudioSegment, SoundtrackSessionSpec},
};
use serde_json::{Value, json};
use std::{path::Path, process::Command};

/// Runs the binary and returns (success, raw stdout, raw stderr).
fn run_raw(args: &[&str]) -> (bool, Vec<u8>, Vec<u8>) {
    let out = Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .args(args)
        .output()
        .unwrap();
    (out.status.success(), out.stdout, out.stderr)
}

/// Failure envelopes go to stderr and must always be JSON (no panic output).
fn run_failure(args: &[&str]) -> Value {
    let (ok, _stdout, stderr) = run_raw(args);
    assert!(!ok, "command unexpectedly succeeded");
    serde_json::from_slice(&stderr).unwrap_or_else(|e| {
        panic!(
            "failure envelope is not JSON ({e}): {}",
            String::from_utf8_lossy(&stderr)
        )
    })
}

/// Creates a session plan over a 60 s native capture and returns the plan path,
/// the empty cache path and the utterance query file path.
fn fixture(root: &Path) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let digest = "b".repeat(64);
    let spec = SoundtrackSessionSpec {
        schema_version: 1,
        sync: SyncRequest {
            schema_version: 1,
            session_id: "sync_consensus_cli".into(),
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
    let (ok, stdout, stderr) = run_raw(&[
        "tv-review",
        "soundtrack-session-plan",
        "--input",
        input.to_str().unwrap(),
    ]);
    assert!(ok, "{}", String::from_utf8_lossy(&stderr));
    let planned: Value = serde_json::from_slice(&stdout).unwrap();
    let plan_path = root.join("plan.json");
    std::fs::write(&plan_path, serde_json::to_vec(&planned["data"]).unwrap()).unwrap();

    let cache = root.join("empty cache");
    std::fs::create_dir(&cache).unwrap();
    let queries = root.join("utterances.json");
    std::fs::write(
        &queries,
        serde_json::to_vec(&vec![SyncQuery {
            clock_id: "native".into(),
            start_sample: 10 * 48000,
            end_sample: 12 * 48000,
        }])
        .unwrap(),
    )
    .unwrap();
    (plan_path, cache, queries)
}

fn sync_args<'a>(plan: &'a str, cache: &'a str, queries: &'a str) -> Vec<&'a str> {
    vec![
        "tv-review",
        "soundtrack-session-sync",
        "--plan",
        plan,
        "--cache",
        cache,
        "--queries",
        queries,
    ]
}

fn plan_digest_of(plan_path: &Path) -> String {
    let plan: Value = serde_json::from_slice(&std::fs::read(plan_path).unwrap()).unwrap();
    plan["planDigest"].as_str().unwrap().to_string()
}

fn valid_consensus(plan_digest: &str) -> SoundtrackConsensus {
    SoundtrackConsensus {
        schema_version: 1,
        plan_digest: plan_digest.into(),
        segments: vec![ConsensusSegment {
            segment_id: "segment_0".into(),
            clock_id: "native".into(),
            offset_seconds: 0.0,
            mic_start_sample: 0,
            mic_end_sample: 60 * 48000,
            supporting_queries: vec![0],
            residual_rms_seconds: 0.01,
            drift_ppm: 0.0,
        }],
        boundaries: vec![],
        unsupported_queries: vec![],
    }
}

#[test]
fn sync_without_consensus_is_unchanged_and_consensus_segments_are_copied() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (plan, cache, queries) = fixture(root);
    let (plan_s, cache_s, queries_s) = (
        plan.to_str().unwrap(),
        cache.to_str().unwrap(),
        queries.to_str().unwrap(),
    );
    let args = sync_args(plan_s, cache_s, queries_s);

    // Baseline: the existing command, captured in this test.
    let (ok, baseline, stderr) = run_raw(&args);
    assert!(ok, "{}", String::from_utf8_lossy(&stderr));
    let baseline_value: Value = serde_json::from_slice(&baseline).unwrap();
    assert_eq!(baseline_value["ok"], json!(true));
    assert_eq!(baseline_value["data"]["mappingPurpose"], "noCutMapping");
    assert!(
        baseline_value["data"]["evidence"]["sync"]
            .get("consensusSegments")
            .is_none(),
        "no consensus segments without the flag: {baseline_value}"
    );

    // Same arguments again must give byte-identical output.
    let (ok, again, _) = run_raw(&args);
    assert!(ok);
    assert_eq!(again, baseline, "output without --consensus must be stable");

    // With a valid consensus file only the copied segments differ.
    let consensus_path = root.join("consensus.json");
    std::fs::write(
        &consensus_path,
        serde_json::to_vec(&valid_consensus(&plan_digest_of(&plan))).unwrap(),
    )
    .unwrap();
    let mut with_args = args.clone();
    with_args.extend(["--consensus", consensus_path.to_str().unwrap()]);
    let (ok, with, stderr) = run_raw(&with_args);
    assert!(ok, "{}", String::from_utf8_lossy(&stderr));
    let mut with_value: Value = serde_json::from_slice(&with).unwrap();
    assert_eq!(
        with_value["data"]["evidence"]["sync"]["consensusSegments"][0]["segmentId"],
        "segment_0"
    );
    with_value["data"]["evidence"]["sync"]
        .as_object_mut()
        .unwrap()
        .remove("consensusSegments");
    assert_eq!(with_value, baseline_value);
}

#[test]
fn unreadable_or_malformed_consensus_is_rejected_with_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (plan, cache, queries) = fixture(root);
    let digest = plan_digest_of(&plan);
    let (plan_s, cache_s, queries_s) = (
        plan.to_str().unwrap(),
        cache.to_str().unwrap(),
        queries.to_str().unwrap(),
    );

    let missing = root.join("does-not-exist.json");
    let malformed = root.join("malformed.json");
    std::fs::write(&malformed, b"{not json").unwrap();
    let unknown_field = root.join("unknown-field.json");
    let mut extra = serde_json::to_value(valid_consensus(&digest)).unwrap();
    extra["bogus"] = json!(1);
    std::fs::write(&unknown_field, serde_json::to_vec(&extra).unwrap()).unwrap();
    let foreign = root.join("foreign.json");
    std::fs::write(
        &foreign,
        serde_json::to_vec(&valid_consensus(&"c".repeat(64))).unwrap(),
    )
    .unwrap();
    let directory = root.join("a directory");
    std::fs::create_dir(&directory).unwrap();

    for bad in [&missing, &malformed, &unknown_field, &directory, &foreign] {
        let mut args = sync_args(plan_s, cache_s, queries_s);
        args.extend(["--consensus", bad.to_str().unwrap()]);
        let envelope = run_failure(&args);
        assert_eq!(envelope["ok"], json!(false), "{envelope}");
        let message = envelope["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("invalid input") && !message.is_empty(),
            "{}: unclear error: {envelope}",
            bad.display()
        );
    }
}
