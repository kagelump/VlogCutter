use rcdvlog_core::{tv_review_sync::SyncMovieClock, tv_soundtrack::*};
use serde_json::Value;
use std::process::Command;
#[test]
fn passive_cli_reuses_a_bounded_private_index_and_silence_stays_unmatched() {
    let dir = tempfile::tempdir().unwrap();
    let spec = SoundtrackReferenceSpec {
        schema_version: 1,
        movie: SyncMovieClock {
            movie_digest: "a".repeat(64),
            time_base_numerator: 1,
            time_base_denominator: 90000,
            origin_pts: 0,
            duration_pts: 12 * 90000,
        },
        start_seconds: 0.0,
        duration_seconds: 12.0,
    };
    let pcm: Vec<f32> = (0..12 * 8000)
        .map(|i| (i as f32 * 0.17).sin() * 0.1)
        .collect();
    let index = SoundtrackIndex::from_pcm(spec, &pcm, &|| false).unwrap();
    let index_path = dir.path().join("index.json");
    std::fs::write(&index_path, serde_json::to_vec(&index).unwrap()).unwrap();
    let query = dir.path().join("silence.wav");
    let created = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=8000:cl=mono",
            "-t",
            "6",
            "-c:a",
            "pcm_s16le",
        ])
        .arg(&query)
        .output()
        .unwrap();
    assert!(created.status.success());
    let capture = dir.path().join("capture.json");
    std::fs::write(
        &capture,
        serde_json::to_vec(&SoundtrackCapture {
            evidence_id: "query_synthetic".into(),
            clock_id: "mic".into(),
            sample_rate: 48000,
            first_sample: 0,
            monotonic_origin_ns: 0,
        })
        .unwrap(),
    )
    .unwrap();
    let invoke = || {
        Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
            .env("RCDVLOG_RESOURCE_LOCK_ROOT", dir.path().join("locks"))
            .args(["tv-review", "soundtrack-match", "--index"])
            .arg(&index_path)
            .arg("--query")
            .arg(&query)
            .arg("--capture")
            .arg(&capture)
            .output()
            .unwrap()
    };
    let out = invoke();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty());
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["command"], "tv-review.soundtrack-match");
    assert_eq!(result["data"]["state"], "unmatched");
    assert_eq!(result["data"]["reason"], "quiet_query");
    let mut corrupt = index;
    corrupt.index_digest = "b".repeat(64);
    std::fs::write(&index_path, serde_json::to_vec(&corrupt).unwrap()).unwrap();
    let out = invoke();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stderr).unwrap()["ok"],
        false
    );
}
