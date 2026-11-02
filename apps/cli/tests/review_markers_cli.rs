//! `review-markers` process contract on generated media only (#226).
use rcdvlog_core::ReviewTargetHandoff;
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

fn rcdvlog(args: &[&str]) -> (bool, Value, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .args(args)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let value = if output.status.success() {
        serde_json::from_str(&stdout).unwrap()
    } else {
        assert!(stdout.is_empty(), "failure must leave stdout empty");
        serde_json::from_str(stderr.lines().last().unwrap()).unwrap()
    };
    (output.status.success(), value, stderr)
}

fn blake3_file(path: &Path) -> (String, u64) {
    let identity = rcdvlog_core::acoustic_markers::file_identity(path).unwrap();
    (identity.blake3, identity.byte_length)
}

#[test]
fn plan_export_and_detect_round_trip_through_the_cli() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("ffmpeg unavailable; skipping");
        return;
    }
    let dir = tempdir().unwrap();
    let movie = dir.path().join("master movie 日本語.mp4");
    assert!(
        Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=160x90:rate=30:duration=20",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=20",
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
                "-shortest"
            ])
            .arg(&movie)
            .status()
            .unwrap()
            .success()
    );
    let (digest, length) = blake3_file(&movie);
    let mut handoff: ReviewTargetHandoff = serde_json::from_str(include_str!(
        "../../../core/rcdvlog-core/tests/fixtures/review-target/v1.json"
    ))
    .unwrap();
    let t = &mut handoff.target;
    t.movie.blake3 = digest;
    t.movie.byte_length = length;
    t.movie.duration_seconds = 20.0;
    t.movie.time_base_numerator = 1;
    t.movie.time_base_denominator = 30;
    t.movie.origin_pts = 0;
    t.movie.video_frame_count = 600;
    t.cuts.truncate(1);
    t.cuts[0].movie_in_pts = 0;
    t.cuts[0].movie_out_pts = 600;
    t.cuts[0].output_frame_count = 600;
    handoff.target_digest = ReviewTargetHandoff::snapshot_digest(&handoff.target).unwrap();
    let handoff_path = dir.path().join("handoff.json");
    // The CLI success envelope is accepted as input, too.
    std::fs::write(&handoff_path, serde_json::to_vec(&serde_json::json!({"ok": true, "command": "edit-plan.review-target-export", "data": handoff})).unwrap()).unwrap();
    let handoff_arg = handoff_path.to_str().unwrap();

    let (ok, plan, _) = rcdvlog(&[
        "review-markers",
        "plan",
        "--handoff",
        handoff_arg,
        "--interval-seconds",
        "8",
    ]);
    assert!(ok);
    assert_eq!(plan["command"], "review-markers.plan");
    assert_eq!(
        plan["data"]["markerSeconds"],
        serde_json::json!([2.0, 10.0, 18.0])
    );

    let output = dir.path().join("review copy.mp4");
    let (ok, export, stderr) = rcdvlog(&[
        "review-markers",
        "export",
        "--handoff",
        handoff_arg,
        "--movie",
        movie.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--interval-seconds",
        "8",
    ]);
    assert!(ok, "{stderr}");
    let manifest = &export["data"];
    assert_eq!(manifest["schema"], "marked-review-copy.v1");
    assert_eq!(manifest["verification"]["detectedMarkers"], 3);
    assert_eq!(manifest["verification"]["pictureStreamMatchesMaster"], true);
    assert!(
        !export.to_string().contains(dir.path().to_str().unwrap()),
        "path-free"
    );
    assert!(output.with_extension("markers.json").is_file());
    assert_eq!(
        blake3_file(&movie).0,
        handoff.target.movie.blake3,
        "master untouched"
    );

    // Create-only.
    let (ok, error, _) = rcdvlog(&[
        "review-markers",
        "export",
        "--handoff",
        handoff_arg,
        "--movie",
        movie.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]);
    assert!(!ok);
    assert_eq!(error["error"]["code"], "invalid_input");

    // Detect from a WAV of the marked copy, as a recorder would capture it.
    let wav = dir.path().join("heard.wav");
    assert!(
        Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(&output)
            .args(["-ac", "1", "-ar", "48000", "-c:a", "pcm_s16le"])
            .arg(&wav)
            .status()
            .unwrap()
            .success()
    );
    let (ok, detection, _) = rcdvlog(&[
        "review-markers",
        "detect",
        "--handoff",
        handoff_arg,
        "--recording",
        wav.to_str().unwrap(),
    ]);
    assert!(ok);
    let anchors = detection["data"]["anchors"].as_array().unwrap();
    assert_eq!(
        anchors
            .iter()
            .map(|a| a["movieSeconds"].as_f64().unwrap())
            .collect::<Vec<_>>(),
        vec![2.0, 10.0, 18.0]
    );
}
