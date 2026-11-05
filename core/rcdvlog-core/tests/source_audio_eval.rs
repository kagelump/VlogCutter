//! Locks the vendored source-audio evaluation assets and the deterministic
//! synthetic fixtures under `tests/source_audio_eval/`.
//!
//! The frozen policy bytes, the sanitized expected metrics provenance and the
//! synthetic fixture digests are constants of the source-audio MVP evidence;
//! see `tests/source_audio_eval/README.md`.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

const POLICY_SHA256: &str = "75b006faea7287bd31ce000b9fd2439290f41fdcf3a6ee8e2b22f6eea19efd4e";
const RESULTS_SHA256: &str = "41dcc1ee05923f1897b8a1f902606ca5cdaad9edddd0349c59c031f5d23eabb7";
const MANIFEST_SHA256: &str = "80ae9adc10138f33f72b168c57f79ba5d1f9a1dc803a581700ad36daeabb6b44";
const BASELINE_POLICY_SHA256: &str =
    "1be66d6b58c3ca41bdd910f26d3c53c121a02867205be84cb6d67ee7a1be4438";
const OFFSET_CONVENTION: &str = "movieElapsedSeconds = recordingSecondsWithinEpoch + offsetSeconds";

fn eval_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/source_audio_eval")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_file(path: &Path) -> String {
    hex(&Sha256::digest(fs::read(path).unwrap()))
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn node_available() -> bool {
    Command::new("node").arg("--version").output().is_ok()
}

/// Minimal canonical PCM WAV header reader (fmt + data chunks).
fn wav_shape(path: &Path) -> (u32, u32) {
    let bytes = fs::read(path).unwrap();
    assert!(bytes.len() >= 44, "wav too short: {path:?}");
    assert_eq!(&bytes[0..4], b"RIFF", "not RIFF: {path:?}");
    assert_eq!(&bytes[8..12], b"WAVE", "not WAVE: {path:?}");
    assert_eq!(&bytes[12..16], b"fmt ", "missing fmt chunk: {path:?}");
    let u16le = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]) as u32;
    let u32le = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    assert_eq!(u16le(20), 1, "not PCM: {path:?}");
    assert_eq!(u16le(22), 1, "not mono: {path:?}");
    assert_eq!(u16le(34), 16, "not 16-bit: {path:?}");
    assert_eq!(&bytes[36..40], b"data", "missing data chunk: {path:?}");
    let sample_rate = u32le(24);
    let frames = u32le(40) / 2;
    assert_eq!(
        bytes.len() as u32,
        44 + frames * 2,
        "trailing bytes: {path:?}"
    );
    (sample_rate, frames)
}

fn collect_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|v| collect_strings(v, out)),
        Value::Object(map) => map.values().for_each(|v| collect_strings(v, out)),
        _ => {}
    }
}

/// Maximal runs of lowercase hex characters in a string.
fn hex_runs(s: &str) -> Vec<usize> {
    let mut runs = Vec::new();
    let mut current = 0;
    for c in s.chars() {
        if c.is_ascii_digit() || ('a'..='f').contains(&c) {
            current += 1;
        } else {
            if current > 0 {
                runs.push(current);
            }
            current = 0;
        }
    }
    if current > 0 {
        runs.push(current);
    }
    runs
}

#[test]
fn vendored_policy_matches_frozen_sha256() {
    let path = eval_root().join("policies/pooled-recognition-policy-v3-anchor-aware.json");
    assert_eq!(sha256_file(&path), POLICY_SHA256);
    let policy = read_json(&path);
    assert_eq!(policy["schemaVersion"], 1);
    assert_eq!(
        policy["policyId"],
        "rc6-v2-density100-exact-bin-complete-evidence-anchor-identity-calibrated-v3"
    );
    assert_eq!(policy["acceptance"]["10"]["minimumAnchors"], 33);
    assert_eq!(policy["acceptance"]["10"]["minimumOccupiedTimeBins"], 5);
    assert_eq!(
        policy["acceptance"]["10"]["minimumDistinctCompetitorRatio"],
        2.0
    );
    assert_eq!(policy["acceptance"]["10"]["minimumUniqueAnchorMargin"], 2.0);
    assert_eq!(policy["completenessRequired"], true);
}

#[test]
fn sanitized_metrics_carry_frozen_provenance_and_outcomes() {
    let doc = read_json(
        &eval_root().join("expected/m1-small-set-anchor-aware-results-v2.sanitized.json"),
    );
    let provenance = &doc["provenance"];
    assert_eq!(
        provenance["sourceFile"],
        "reproduction/m1-small-set-anchor-aware-results-v2.json"
    );
    assert_eq!(provenance["sourceFileSha256"], RESULTS_SHA256);
    assert!(
        provenance["sourceLocation"]
            .as_str()
            .unwrap()
            .contains("SOURCE_AUDIO_EVAL_ROOT")
    );
    assert_eq!(provenance["sanitizations"].as_array().unwrap().len(), 2);

    let results = &doc["sanitized"];
    assert_eq!(results["schemaVersion"], 1);
    assert_eq!(
        results["experimentId"],
        "rc6-v2-exact-bin-window-pooling-m1-v1"
    );
    assert_eq!(results["manifestSha256"], MANIFEST_SHA256);
    assert_eq!(results["baselinePolicySha256"], BASELINE_POLICY_SHA256);
    assert_eq!(results["calibratedPolicySha256"], POLICY_SHA256);
    assert_eq!(results["offsetToleranceSeconds"], 0.1);

    let cases = results["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 60);
    for (i, case) in cases.iter().enumerate() {
        assert_eq!(
            case["caseIndex"], i as u64,
            "case rows stay in source order"
        );
        assert!(case.get("sessionId").is_none(), "sessionIds are sanitized");
        assert_eq!(case["completeEvidence"], true);
    }

    // Frozen outcomes: 17/17 positive windows top-ranked at each density.
    for density in ["density50", "density100"] {
        let positives: Vec<&Value> = cases
            .iter()
            .filter(|c| {
                c["density"] == density && c["role"].as_str().unwrap().starts_with("positive")
            })
            .collect();
        assert_eq!(positives.len(), 17, "{density} positive windows");
        for case in &positives {
            assert_eq!(case["topModeCorrectWithin100ms"], true, "{density}");
            assert_eq!(case["correctModeInTopEight"], true, "{density}");
        }
    }

    // Density100 10-second v3 acceptance: 5/6 positives; the moderate
    // composite stays a documented rejection.
    let v3_positives: Vec<&Value> = cases
        .iter()
        .filter(|c| {
            c["density"] == "density100"
                && c["durationSeconds"] == 10
                && c["role"].as_str().unwrap().starts_with("positive")
        })
        .collect();
    assert_eq!(v3_positives.len(), 6);
    let accepted = v3_positives
        .iter()
        .filter(|c| c["policyV3AnchorAwareAccepted"] == Value::Bool(true))
        .count();
    assert_eq!(accepted, 5);
    let rejected: Vec<&&Value> = v3_positives
        .iter()
        .filter(|c| c["policyV3AnchorAwareAccepted"] == Value::Bool(false))
        .collect();
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0]["condition"], "moderate_composite_provisional");
    assert!(
        rejected[0]["policyV3FailureReasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "minimum_anchors")
    );

    // 0/13 negative accepts at both densities, by v3 and by the baseline.
    let negatives: Vec<&Value> = cases
        .iter()
        .filter(|c| c["role"].as_str().unwrap().starts_with("negative"))
        .collect();
    assert_eq!(negatives.len(), 26);
    for case in &negatives {
        assert_ne!(case["policyV3AnchorAwareAccepted"], Value::Bool(true));
        assert_eq!(case["baselinePolicyV1Accepted"], false);
    }

    // Top-ranked positive errors stay within 40 ms.
    let worst = cases
        .iter()
        .filter_map(|c| c["topModeOffsetErrorSeconds"].as_f64())
        .fold(0.0_f64, f64::max);
    assert!(worst <= 0.0401, "worst top-mode error {worst}");
}

#[test]
fn derived_assets_are_free_of_private_paths_and_ids() {
    let root = eval_root();
    let derived = [
        "expected/m1-small-set-anchor-aware-results-v2.sanitized.json",
        "synthetic/synthetic-ci-v1.json",
        "README.md",
        "runner/run-source-audio-eval.mjs",
        "synthetic/generate-synthetic-fixtures.mjs",
        "release/README.md",
        "release/release-comparison-v1.sanitized.json",
    ];
    // Fragments of the private evaluation root path and private media layout,
    // used only as leak-detection needles.
    let forbidden = [
        "editorial-review",
        "rc6-source-audio-eval-v2",
        "/home/",
        "inputs/m1-small-set",
        "reused-indexes",
    ];
    for name in derived {
        let text = fs::read_to_string(root.join(name)).unwrap();
        for needle in forbidden {
            assert!(
                !text.contains(needle),
                "{name} leaks private fragment {needle:?}"
            );
        }
    }
    // The sanitized metrics redact sessionId values (24-hex private filename
    // stems); only 64-hex SHA-256 identity fields may appear as hex runs.
    let mut strings = Vec::new();
    for sanitized in [
        "expected/m1-small-set-anchor-aware-results-v2.sanitized.json",
        "release/release-comparison-v1.sanitized.json",
    ] {
        collect_strings(&read_json(&root.join(sanitized)), &mut strings);
    }
    for s in &strings {
        for len in hex_runs(s) {
            assert_ne!(len, 24, "possible private sessionId stem in {s:?}");
        }
    }
}

#[test]
fn synthetic_manifest_matches_committed_media() {
    let root = eval_root().join("synthetic");
    let manifest = read_json(&root.join("synthetic-ci-v1.json"));
    assert_eq!(manifest["schemaVersion"], 1);
    assert_eq!(manifest["fixtureSet"], "synthetic-ci-v1");
    assert_eq!(manifest["offsetConvention"], OFFSET_CONVENTION);

    let movie = &manifest["movie"];
    let movie_path = root.join(movie["path"].as_str().unwrap());
    assert_eq!(sha256_file(&movie_path), movie["sha256"]);
    let (rate, frames) = wav_shape(&movie_path);
    assert_eq!(rate, movie["sampleRate"]);
    assert_eq!(frames, movie["sampleCount"]);
    let clock = &movie["clock"];
    assert_eq!(clock["movieDigest"], movie["sha256"]);
    assert_eq!(clock["durationPts"], movie["sampleCount"]);
    assert_eq!(clock["timeBaseDenominator"], rate);
    assert_eq!(clock["timeBaseNumerator"], 1);
    assert_eq!(clock["originPts"], 0);
    assert_eq!(
        manifest["indexRequest"]["movie"], *clock,
        "index and match share one movie identity"
    );

    let cases = manifest["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 1);
    let case = &cases[0];
    let recording = &case["recording"];
    let recording_path = root.join(recording["path"].as_str().unwrap());
    assert_eq!(sha256_file(&recording_path), recording["sha256"]);
    let (rate, frames) = wav_shape(&recording_path);
    assert_eq!(rate, recording["sampleRate"]);
    assert_eq!(
        u64::from(frames),
        recording["lastSample"].as_u64().unwrap() - recording["firstSample"].as_u64().unwrap()
    );
    assert_eq!(case["request"]["sync"]["movie"], *clock);
    assert_eq!(
        case["request"]["captureClockId"],
        recording["captureClockId"]
    );

    // Offset sign convention: the movie starts at recording time 3.0 s, so
    // movieElapsedSeconds = recordingSecondsWithinEpoch + offsetSeconds
    // gives 0.0 = 3.0 + offsetSeconds, i.e. offsetSeconds = -3.0.
    let expected = &case["expected"];
    assert_eq!(expected["offsetSeconds"], -3.0);
    assert!(expected["derivation"].as_str().unwrap().contains("-3.0"));
    let windows = case["request"]["pooledOffsetWindows"].as_array().unwrap();
    let expected_windows = expected["windows"].as_array().unwrap();
    assert_eq!(windows.len(), expected_windows.len());
    assert!(!windows.is_empty());
    for (window, expected_window) in windows.iter().zip(expected_windows) {
        assert_eq!(expected_window["startSeconds"], window["startSeconds"]);
        assert_eq!(
            expected_window["durationSeconds"],
            window["durationSeconds"]
        );
        assert_eq!(expected_window["expectedOffsetSeconds"], -3.0);
        assert_eq!(window["durationSeconds"], 10);
        assert!(
            window["startSeconds"].as_u64().unwrap() + 10 <= u64::from(frames) / u64::from(rate)
        );
    }
    assert_eq!(case["request"]["pooledOffsetModeLimit"], 8);
}

#[test]
fn synthetic_generator_reproduces_committed_bytes() {
    if !node_available() {
        eprintln!("skipping synthetic generator check: node unavailable");
        return;
    }
    let root = eval_root().join("synthetic");
    let first = tempdir().unwrap();
    let second = tempdir().unwrap();
    for out in [first.path(), second.path()] {
        let output = Command::new("node")
            .arg(root.join("generate-synthetic-fixtures.mjs"))
            .arg("--out")
            .arg(out)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "generator failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    for name in [
        "movie.wav",
        "recording-continuous.wav",
        "synthetic-ci-v1.json",
    ] {
        let committed = fs::read(root.join(name)).unwrap();
        assert_eq!(
            fs::read(first.path().join(name)).unwrap(),
            committed,
            "{name} regeneration differs from committed bytes"
        );
        assert_eq!(
            fs::read(second.path().join(name)).unwrap(),
            committed,
            "{name} is not deterministic across runs"
        );
    }
}

#[test]
fn evaluation_runner_dry_run_validates_plan() {
    if !node_available() {
        eprintln!("skipping runner dry-run check: node unavailable");
        return;
    }
    let root = eval_root();
    let out = tempdir().unwrap();
    let receipt_path = out.path().join("receipt.json");
    let output = Command::new("node")
        .arg(root.join("runner/run-source-audio-eval.mjs"))
        .arg("--binary")
        .arg(out.path().join("not-built-yet/rcdvlog"))
        .arg("--manifest")
        .arg(root.join("synthetic/synthetic-ci-v1.json"))
        .arg("--media-root")
        .arg(root.join("synthetic"))
        .arg("--output")
        .arg(&receipt_path)
        .arg("--dry-run")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "dry-run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt = read_json(&receipt_path);
    assert_eq!(receipt["mode"], "dry-run");
    assert_eq!(receipt["status"], "plan valid");
    assert_eq!(receipt["binaryPresent"], false);
    assert_eq!(receipt["offsetConvention"], OFFSET_CONVENTION);
    assert_eq!(receipt["indexStep"]["status"], "planned");
    let cases = receipt["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0]["status"], "planned");
    assert_eq!(cases[0]["expectedOffsetSeconds"], -3.0);
    let windows = cases[0]["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 2);
    for window in windows {
        assert_eq!(window["expectedOffsetSeconds"], -3.0);
        assert_eq!(window["topModeOffsetSeconds"], Value::Null);
    }
    assert!(
        !out.path().join("receipt.json.work").exists(),
        "dry-run must not execute"
    );
}
