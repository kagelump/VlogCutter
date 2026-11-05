//! Process-level source-audio CLI workflow through the built binary: immutable
//! landmark index -> full-recording pooled session report -> shared-mapper
//! query mapping, with actual WAV decoding of the committed deterministic
//! synthetic fixtures. Covers the documented failure modes: movie/recording
//! digest disagreement, stale derived results, missing/corrupt media, path
//! handling, mid-run cancellation and unknown-utterance abstention (recording
//! timestamps kept, never a movie location).

use rcdvlog_core::{
    full_file_blake3,
    source_audio_pooled::PooledRecognitionPolicy,
    source_audio_segments::QueryMappingRequest,
    tv_review_sync::{SyncPolicy, SyncQuery},
};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const SYNTHETIC: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../core/rcdvlog-core/tests/source_audio_eval/synthetic"
);
const MANIFEST: &str = include_str!(
    "../../../core/rcdvlog-core/tests/source_audio_eval/synthetic/synthetic-ci-v1.json"
);
const RECIPE: &str = "source-audio-pooled-mvp-v1";
const RATE: u64 = 11_025;

fn manifest() -> Value {
    serde_json::from_str(MANIFEST).unwrap()
}

fn synth(name: &str) -> PathBuf {
    Path::new(SYNTHETIC).join(name)
}

/// CLI identities are BLAKE3 of the exact file bytes (the fixture manifest's
/// sha256 values belong to the JS eval runner, not this surface).
fn digest(path: &Path) -> String {
    full_file_blake3(path, &|| false).unwrap().hex_digest
}

fn cli(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rcdvlog"));
    command.env("RCDVLOG_RESOURCE_LOCK_ROOT", root.join("locks"));
    command
}

fn data_of(command: &str, output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{command}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["command"], command);
    envelope["data"].clone()
}

fn error_of(output: &Output) -> Value {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "errors never emit stdout data");
    let envelope: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(envelope["ok"], false);
    assert!(envelope["command"].is_string());
    let error = &envelope["error"];
    assert!(error["code"].is_string() && !error["code"].as_str().unwrap().is_empty());
    assert!(error["message"].is_string() && !error["message"].as_str().unwrap().is_empty());
    error.clone()
}

/// The movie clock of the committed synthetic movie, with the CLI's BLAKE3
/// identity and the manifest's declared rational timebase.
fn movie_clock() -> Value {
    let manifest = manifest();
    let clock = &manifest["movie"]["clock"];
    json!({
        "movieDigest": digest(&synth("movie.wav")),
        "timeBaseNumerator": clock["timeBaseNumerator"],
        "timeBaseDenominator": clock["timeBaseDenominator"],
        "originPts": clock["originPts"],
        "durationPts": clock["durationPts"],
    })
}

fn build_index(root: &Path, movie: &Path, output_name: &str) -> (PathBuf, Value) {
    let request_path = root.join("index-request.json");
    std::fs::write(
        &request_path,
        serde_json::to_vec(&json!({
            "movie": movie_clock(),
            "fftSize": 512,
            "hashesPerSecond": 100,
            "fanout": 3,
        }))
        .unwrap(),
    )
    .unwrap();
    let output_path = root.join(output_name);
    let output = cli(root)
        .args(["tv-review", "source-audio-landmark-index", "--input"])
        .arg(&request_path)
        .arg("--movie")
        .arg(movie)
        .arg("--output")
        .arg(&output_path)
        .output()
        .unwrap();
    let data = data_of("tv-review.source-audio-landmark-index", &output);
    assert_eq!(data["movieDigest"], movie_clock()["movieDigest"]);
    assert!(
        output_path.is_file(),
        "the immutable index artifact is written"
    );
    (output_path, data)
}

/// The full-recording report request: explicit `source-audio-pooled-mvp-v1`
/// selection, capture clock, one whole-recording epoch, full domain and both
/// digest pins.
fn report_request(clock_id: &str, recording: &Path) -> Value {
    json!({
        "recipeIdentity": RECIPE,
        "movieDigest": digest(&synth("movie.wav")),
        "recordingDigest": digest(recording),
        "captureClock": {
            "clockId": clock_id,
            "sampleRate": RATE,
            "firstSample": 0,
            "lastSample": recording_frames(recording),
            "monotonicOriginNs": 0,
            "monotonicToleranceNs": 1,
        },
        "epochs": [ { "epochId": "e1", "startSample": 0, "endSample": recording_frames(recording) } ],
        "domain": { "startSample": 0, "endSample": recording_frames(recording) },
    })
}

/// Native sample count of a mono recording: per-channel WAV frames, exact for
/// any RIFF layout (committed fixtures and ffmpeg-concatenated test media).
fn recording_frames(path: &Path) -> u64 {
    u64::from(hound::WavReader::open(path).unwrap().duration())
}

fn run_report(
    root: &Path,
    name: &str,
    request: &Value,
    index: &Path,
    recording: &Path,
    cache: &Path,
) -> Output {
    let request_path = root.join(name);
    std::fs::write(&request_path, serde_json::to_vec(request).unwrap()).unwrap();
    cli(root)
        .args(["tv-review", "source-audio-session-report", "--input"])
        .arg(&request_path)
        .arg("--index")
        .arg(index)
        .arg("--recording")
        .arg(recording)
        .arg("--cache")
        .arg(cache)
        .output()
        .unwrap()
}

fn query_request(clock_id: &str, start_sample: u64, end_sample: u64) -> Value {
    serde_json::to_value(QueryMappingRequest {
        session_id: format!("{clock_id}-queries"),
        query: SyncQuery {
            clock_id: clock_id.into(),
            start_sample,
            end_sample,
        },
        policy: SyncPolicy::default(),
    })
    .unwrap()
}

fn run_map(root: &Path, name: &str, report: &Path, request: &Value) -> Output {
    let request_path = root.join(name);
    std::fs::write(&request_path, serde_json::to_vec(request).unwrap()).unwrap();
    cli(root)
        .args(["tv-review", "source-audio-map-query", "--report"])
        .arg(report)
        .arg("--input")
        .arg(&request_path)
        .output()
        .unwrap()
}

fn write_report_file(root: &Path, name: &str, report: &Value) -> PathBuf {
    let path = root.join(name);
    std::fs::write(&path, serde_json::to_vec(report).unwrap()).unwrap();
    path
}

/// Abstained mapping outcomes keep their recording timestamps and never carry
/// a movie location.
fn assert_abstained(result: &Value, state: &str, query: &Value) {
    assert_eq!(result["state"], state, "{result}");
    assert_eq!(
        result["query"], *query,
        "recording timestamps are kept verbatim"
    );
    assert!(result["supportAnchorIds"].as_array().unwrap().is_empty());
    assert!(
        result["slices"]
            .as_array()
            .unwrap()
            .iter()
            .all(|slice| slice["candidates"].as_array().unwrap().is_empty()),
        "no movie location for abstained queries: {result}"
    );
}

#[test]
fn full_recording_workflow_indexes_reports_and_maps_movie_time() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let recording = synth("recording-continuous.wav");
    let (index, index_data) = build_index(root, &synth("movie.wav"), "index.json");

    // 2. Full-recording pooled match over the session schedule.
    let output = run_report(
        root,
        "report-request.json",
        &report_request("synthetic-ci-v1-continuous", &recording),
        &index,
        &recording,
        &root.join("cache"),
    );
    let data = data_of("tv-review.source-audio-session-report", &output);
    assert_eq!(data["schemaVersion"], 1);
    let report = &data["report"];
    assert_eq!(report["status"], "complete");
    assert_eq!(report["recipeIdentity"], RECIPE);
    assert_eq!(report["assemblerId"], "source-audio-segments-mvp-v1");
    assert_eq!(
        report["acceptancePolicyId"],
        PooledRecognitionPolicy::frozen().policy_id
    );
    assert_eq!(
        report["identities"],
        json!({
            "captureClockId": "synthetic-ci-v1-continuous",
            "recordingIdentity": digest(&recording),
            "indexIdentity": index_data["indexDigest"],
        })
    );
    // Continuous playback coalesces to one segment at the documented delay.
    let segments = report["segments"].as_array().unwrap();
    assert_eq!(segments.len(), 1, "{report}");
    let segment = &segments[0];
    let expected_offset = manifest()["cases"][0]["expected"]["offsetSeconds"]
        .as_f64()
        .unwrap();
    assert!(
        (segment["offsetSeconds"].as_f64().unwrap() - expected_offset).abs() <= 0.1,
        "{} must match the documented delay",
        segment["offsetSeconds"]
    );
    assert_eq!(segment["playbackRate"], 1.0);
    assert_eq!(segment["startSample"], 3 * RATE);
    assert_eq!(segment["endSample"], 15 * RATE);
    let offset = segment["offsetSeconds"].as_f64().unwrap();
    let span = &segment["supportedSpans"].as_array().unwrap()[0];
    // Raw movie PTS follow the frozen convention from the declared origin.
    assert!(
        (span["movieStartPts"].as_f64().unwrap()
            - (span["startSample"].as_f64().unwrap() + offset * RATE as f64))
            .abs()
            <= 1.0
    );
    assert!(
        (span["movieEndPts"].as_f64().unwrap()
            - (span["endSample"].as_f64().unwrap() + offset * RATE as f64))
            .abs()
            <= 1.0
    );
    // Accounting covers the requested domain exactly.
    let completeness = &report["completeness"];
    assert_eq!(completeness["accountingComplete"], true);
    assert_eq!(completeness["searchComplete"], true);
    assert_eq!(completeness["incompleteWindowCount"], 0);
    assert_eq!(completeness["requestedSeconds"], 15.0);
    assert_eq!(
        completeness["supportedSeconds"].as_f64().unwrap()
            + completeness["unknownSeconds"].as_f64().unwrap(),
        15.0
    );
    // The noise pre-roll stays an explicit unknown range with no movie position.
    let unknown = &report["unknownRanges"].as_array().unwrap()[0];
    assert_eq!(unknown["startSample"], 0);
    assert_eq!(unknown["endSample"], 3 * RATE);
    assert!(unknown.get("movieStartPts").is_none());
    // The session leg publishes window work separately from unique coverage.
    let session = &data["session"];
    assert_eq!(session["status"], "complete");
    assert_eq!(session["cancelled"], false);
    assert_eq!(
        session["windowAccounting"]["overlappingWindowWorkSeconds"],
        40.0
    );
    assert_eq!(
        session["windowAccounting"]["uniqueRecordingCoverageSeconds"],
        15.0
    );

    // The --output artifact carries the bare SegmentReport JSON.
    let output_path = root.join("report output.json");
    let output = cli(root)
        .args(["tv-review", "source-audio-session-report", "--input"])
        .arg(root.join("report-request.json"))
        .arg("--index")
        .arg(&index)
        .arg("--recording")
        .arg(&recording)
        .arg("--cache")
        .arg(root.join("cache"))
        .arg("--output")
        .arg(&output_path)
        .output()
        .unwrap();
    let second = data_of("tv-review.source-audio-session-report", &output)["report"].clone();
    let written: Value = serde_json::from_slice(&std::fs::read(&output_path).unwrap()).unwrap();
    assert_eq!(written, second, "--output receives the SegmentReport JSON");

    // 3. Map a recording-time query inside the supported span to movie time.
    let report_path = write_report_file(root, "report.json", report);
    let query = query_request("synthetic-ci-v1-continuous", 6 * RATE, 8 * RATE);
    let mapped = data_of(
        "tv-review.source-audio-map-query",
        &run_map(root, "query-mapped.json", &report_path, &query),
    );
    assert_eq!(mapped["state"], "mapped", "{mapped}");
    assert!(!mapped["supportAnchorIds"].as_array().unwrap().is_empty());
    let first = mapped["slices"].as_array().unwrap()[0].clone();
    let candidates = first["candidates"].as_array().unwrap();
    assert!(!candidates.is_empty(), "{mapped}");
    // recording [6, 8) s with the segment offset lands at movie (6 + offset) s.
    assert!(
        (candidates[0]["startPts"].as_f64().unwrap() - (6.0 * RATE as f64 + offset * RATE as f64))
            .abs()
            <= 2.0,
        "{} must follow movieElapsed = recordingWithinEpoch + offset",
        candidates[0]["startPts"]
    );

    // An utterance in the unsupported pre-roll abstains: recording timestamps
    // only, never a movie location.
    let unknown_query = query_request("synthetic-ci-v1-continuous", 8_268, 24_806);
    let abstained = data_of(
        "tv-review.source-audio-map-query",
        &run_map(root, "query-unknown.json", &report_path, &unknown_query),
    );
    assert_abstained(&abstained, "unknown", &unknown_query["query"]);
}

#[test]
fn seek_recording_separates_runs_and_never_maps_unknown_or_gap_crossing_queries() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let recording = synth("recording-seek.wav");
    let (index, _) = build_index(root, &synth("movie.wav"), "index.json");
    let data = data_of(
        "tv-review.source-audio-session-report",
        &run_report(
            root,
            "report-request.json",
            &report_request("synthetic-ci-v1-seek", &recording),
            &index,
            &recording,
            &root.join("cache"),
        ),
    );
    let report = &data["report"];
    let segments = report["segments"].as_array().unwrap();
    assert_eq!(segments.len(), 2, "the seek never bridges: {report}");
    // Both runs land at their documented delays and spans.
    assert_eq!(segments[0]["startSample"], 2 * RATE);
    assert_eq!(segments[0]["endSample"], 14 * RATE);
    assert!((segments[0]["offsetSeconds"].as_f64().unwrap() - -2.0).abs() <= 0.1);
    assert_eq!(segments[1]["startSample"], 15 * RATE);
    assert_eq!(segments[1]["endSample"], 27 * RATE);
    assert!((segments[1]["offsetSeconds"].as_f64().unwrap() - -15.0).abs() <= 0.1);
    // The transition is an explicit unknown range between incompatible offsets.
    let transition = report["unknownRanges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|unknown| unknown["startSample"] == 14 * RATE && unknown["endSample"] == 15 * RATE)
        .expect("seek transition stays unknown");
    assert_eq!(transition["reason"], "incompatible_offset_transition");
    assert!(transition.get("movieStartPts").is_none());

    let report_path = write_report_file(root, "report.json", report);
    // A query inside the transition abstains with recording timestamps only.
    let unknown_query = query_request("synthetic-ci-v1-seek", 156_555, 162_165);
    let abstained = data_of(
        "tv-review.source-audio-map-query",
        &run_map(root, "query-unknown.json", &report_path, &unknown_query),
    );
    assert_abstained(&abstained, "unknown", &unknown_query["query"]);
    // A query crossing the unknown transition never receives a movie location.
    let gap_query = query_request("synthetic-ci-v1-seek", 148_837, 170_887);
    let gap = data_of(
        "tv-review.source-audio-map-query",
        &run_map(root, "query-gap.json", &report_path, &gap_query),
    );
    assert_abstained(&gap, "gapCrossing", &gap_query["query"]);
    // The first run still maps to its movie time through the shared mapper.
    let mapped_query = query_request("synthetic-ci-v1-seek", 6 * RATE, 8 * RATE);
    let mapped = data_of(
        "tv-review.source-audio-map-query",
        &run_map(root, "query-mapped.json", &report_path, &mapped_query),
    );
    assert_eq!(mapped["state"], "mapped", "{mapped}");
    let candidates = mapped["slices"].as_array().unwrap()[0]["candidates"]
        .as_array()
        .unwrap();
    assert!(!candidates.is_empty());
    assert!(
        (candidates[0]["startPts"].as_f64().unwrap() - 4.0 * RATE as f64).abs() <= 2.0,
        "recording [6, 8) s at offset -2 maps to movie [4, 6) s: {}",
        candidates[0]["startPts"]
    );
}

#[test]
fn movie_and_recording_digest_disagreement_is_a_structured_error() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let recording = synth("recording-continuous.wav");

    // Wrong movie digest at index time: the movie file must hash to it.
    let request_path = root.join("index-request.json");
    let mut request = json!({
        "movie": movie_clock(),
        "fftSize": 512,
        "hashesPerSecond": 100,
        "fanout": 3,
    });
    request["movie"]["movieDigest"] = json!("f".repeat(64));
    std::fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
    let output = cli(root)
        .args(["tv-review", "source-audio-landmark-index", "--input"])
        .arg(&request_path)
        .arg("--movie")
        .arg(synth("movie.wav"))
        .arg("--output")
        .arg(root.join("index.json"))
        .output()
        .unwrap();
    let error = error_of(&output);
    assert_eq!(error["code"], "invalid_input");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("final movie digest mismatch"),
        "{error}"
    );

    let (index, _) = build_index(root, &synth("movie.wav"), "index.json");

    // A recording digest pin that disagrees with the recording file.
    let mut request = report_request("synthetic-ci-v1-continuous", &recording);
    request["recordingDigest"] = json!("f".repeat(64));
    let error = error_of(&run_report(
        root,
        "report-request-bad-recording.json",
        &request,
        &index,
        &recording,
        &root.join("cache"),
    ));
    assert_eq!(error["code"], "invalid_input");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("recording digest mismatch"),
        "{error}"
    );

    // A movie digest pin that disagrees with the indexed movie identity.
    let mut request = report_request("synthetic-ci-v1-continuous", &recording);
    request["movieDigest"] = json!("f".repeat(64));
    let error = error_of(&run_report(
        root,
        "report-request-bad-movie.json",
        &request,
        &index,
        &recording,
        &root.join("cache"),
    ));
    assert_eq!(error["code"], "invalid_input");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("source-audio index movie identity mismatch"),
        "{error}"
    );
}

#[test]
fn stale_derived_results_are_recomputed_or_refused_never_reused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let recording = synth("recording-continuous.wav");
    let (index, _) = build_index(root, &synth("movie.wav"), "index.json");
    let cache = root.join("cache");
    let request = report_request("synthetic-ci-v1-continuous", &recording);
    let run = |name: &str, request: &Value| {
        data_of(
            "tv-review.source-audio-session-report",
            &run_report(root, name, request, &index, &recording, &cache),
        )["session"]
            .clone()
    };

    // Fresh cache: every window is computed once.
    let first = run("request.json", &request);
    assert_eq!(first["status"], "complete");
    assert_eq!(first["reusedWindowCount"], 0);
    assert_eq!(
        first["computedWindowCount"],
        first["progress"]["totalWindows"]
    );
    // Unchanged identities reuse every derived window result.
    let second = run("request.json", &request);
    assert_eq!(
        second["reusedWindowCount"],
        first["progress"]["totalWindows"]
    );
    assert_eq!(second["computedWindowCount"], 0);
    // An identity change recomputes from scratch: nothing is silently reused.
    let mut changed = request.clone();
    changed["captureClock"]["clockId"] = json!("other-clock");
    let third = run("request-changed.json", &changed);
    assert_eq!(third["reusedWindowCount"], 0);
    assert_eq!(
        third["computedWindowCount"],
        third["progress"]["totalWindows"]
    );
    // Stale derived results planted under the right key are refused outright:
    // the identity change created a second store, so plant into the store the
    // original request actually loads.
    let store = std::fs::read_dir(&cache)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("source-audio-session-")
                && serde_json::from_slice::<Value>(&std::fs::read(path).unwrap()).is_ok_and(
                    |stored| {
                        stored["identity"]["captureClock"]["clockId"]
                            == "synthetic-ci-v1-continuous"
                    },
                )
        })
        .expect("the derived session store is written");
    let mut planted: Value = serde_json::from_slice(&std::fs::read(&store).unwrap()).unwrap();
    planted["identity"]["recordingDigest"] = json!("planted-recording");
    std::fs::write(&store, serde_json::to_vec(&planted).unwrap()).unwrap();
    let error = error_of(&run_report(
        root,
        "request.json",
        &request,
        &index,
        &recording,
        &cache,
    ));
    assert_eq!(error["code"], "invalid_input");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("refusing to reuse"),
        "{error}"
    );
}

#[test]
fn missing_and_corrupt_media_are_structured_json_errors() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let recording = synth("recording-continuous.wav");
    let (index, _) = build_index(root, &synth("movie.wav"), "index.json");
    let request = report_request("synthetic-ci-v1-continuous", &recording);

    // Missing recording media.
    let error = error_of(&run_report(
        root,
        "request.json",
        &request,
        &index,
        &root.join("missing.wav"),
        &root.join("cache"),
    ));
    assert_eq!(error["code"], "not_found");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("source-audio recording media not found"),
        "{error}"
    );

    // Corrupt recording media (not decodable audio).
    let corrupt = root.join("corrupt.wav");
    std::fs::write(&corrupt, [0u8; 4096]).unwrap();
    let mut unpinned = request.clone();
    unpinned.as_object_mut().unwrap().remove("recordingDigest");
    let error = error_of(&run_report(
        root,
        "request-unpinned.json",
        &unpinned,
        &index,
        &corrupt,
        &root.join("cache"),
    ));
    // Corrupt media fails at the native-rate probe, before any decode work.
    assert_eq!(error["code"], "internal_error");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("media tool failed: source-audio capture"),
        "{error}"
    );

    // Missing and corrupt movie media at index time stay structured envelopes.
    let request_path = root.join("index-request.json");
    let write_index_request = |movie_digest: String| {
        let mut request = json!({
            "movie": movie_clock(),
            "fftSize": 512,
            "hashesPerSecond": 100,
            "fanout": 3,
        });
        request["movie"]["movieDigest"] = json!(movie_digest);
        std::fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
    };
    let invoke_index = |movie: &Path| {
        cli(root)
            .args(["tv-review", "source-audio-landmark-index", "--input"])
            .arg(&request_path)
            .arg("--movie")
            .arg(movie)
            .arg("--output")
            .arg(root.join("index-out.json"))
            .output()
            .unwrap()
    };
    write_index_request(digest(&synth("movie.wav")));
    let error = error_of(&invoke_index(&root.join("missing movie.mov")));
    assert!(
        error["message"].as_str().unwrap().contains("No such file"),
        "{error}"
    );
    // The corrupt movie pins its own digest so movie identity is satisfied and
    // the decode itself is what fails.
    let corrupt_movie = root.join("corrupt movie.mov");
    std::fs::write(&corrupt_movie, [1u8; 4096]).unwrap();
    write_index_request(digest(&corrupt_movie));
    let error = error_of(&invoke_index(&corrupt_movie));
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("media tool failed"),
        "{error}"
    );
}

#[test]
fn relative_absolute_and_space_paths_behave_identically() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // (variant directory name, file names, relative invocation)
    let variants: [(PathBuf, [&str; 3], bool); 3] = [
        (
            root.join("absolute"),
            ["movie.wav", "recording.wav", "index.json"],
            false,
        ),
        (
            root.join("relative"),
            ["movie.wav", "recording.wav", "index.json"],
            true,
        ),
        (
            root.join("paths with spaces"),
            ["movie copy.wav", "recording copy.wav", "index copy.json"],
            false,
        ),
    ];
    let mut reports = Vec::new();
    let mut mappings = Vec::new();
    for (variant, names, relative) in variants {
        std::fs::create_dir_all(&variant).unwrap();
        std::fs::copy(synth("movie.wav"), variant.join(names[0])).unwrap();
        std::fs::copy(synth("recording-continuous.wav"), variant.join(names[1])).unwrap();
        let movie = if relative {
            PathBuf::from(names[0])
        } else {
            variant.join(names[0])
        };
        let recording = if relative {
            PathBuf::from(names[1])
        } else {
            variant.join(names[1])
        };
        let index = if relative {
            PathBuf::from(names[2])
        } else {
            variant.join(names[2])
        };
        let cache = if relative {
            PathBuf::from("cache dir")
        } else {
            variant.join("cache dir")
        };
        let request_name = "report request.json";
        let request_path = variant.join(request_name);
        std::fs::write(
            &request_path,
            serde_json::to_vec(&report_request(
                "synthetic-ci-v1-continuous",
                &synth("recording-continuous.wav"),
            ))
            .unwrap(),
        )
        .unwrap();
        // The index is built through the CLI from the copied movie bytes.
        let index_request = variant.join("index request.json");
        std::fs::write(
            &index_request,
            serde_json::to_vec(&json!({
                "movie": movie_clock(),
                "fftSize": 512,
                "hashesPerSecond": 100,
                "fanout": 3,
            }))
            .unwrap(),
        )
        .unwrap();
        let output = cli(&variant)
            .current_dir(if relative { &variant } else { root })
            .args(["tv-review", "source-audio-landmark-index", "--input"])
            .arg(if relative {
                PathBuf::from("index request.json")
            } else {
                index_request.clone()
            })
            .arg("--movie")
            .arg(&movie)
            .arg("--output")
            .arg(&index)
            .output()
            .unwrap();
        data_of("tv-review.source-audio-landmark-index", &output);
        let output = cli(&variant)
            .current_dir(if relative { &variant } else { root })
            .args(["tv-review", "source-audio-session-report", "--input"])
            .arg(if relative {
                PathBuf::from(request_name)
            } else {
                request_path.clone()
            })
            .arg("--index")
            .arg(&index)
            .arg("--recording")
            .arg(&recording)
            .arg("--cache")
            .arg(&cache)
            .output()
            .unwrap();
        let data = data_of("tv-review.source-audio-session-report", &output);
        let report_path = if relative {
            PathBuf::from("report.json")
        } else {
            variant.join("report.json")
        };
        std::fs::write(
            if relative {
                variant.join("report.json")
            } else {
                report_path.clone()
            },
            serde_json::to_vec(&data["report"]).unwrap(),
        )
        .unwrap();
        // The report request file slot carries the mapping request.
        std::fs::write(
            if relative {
                variant.join(request_name)
            } else {
                request_path.clone()
            },
            serde_json::to_vec(&query_request(
                "synthetic-ci-v1-continuous",
                6 * RATE,
                8 * RATE,
            ))
            .unwrap(),
        )
        .unwrap();
        let output = cli(&variant)
            .current_dir(if relative { &variant } else { root })
            .args(["tv-review", "source-audio-map-query", "--report"])
            .arg(&report_path)
            .arg("--input")
            .arg(if relative {
                PathBuf::from(request_name)
            } else {
                request_path.clone()
            })
            .output()
            .unwrap();
        mappings.push(data_of("tv-review.source-audio-map-query", &output));
        reports.push(data);
    }
    assert_eq!(reports[0], reports[1], "relative and absolute paths agree");
    assert_eq!(reports[0], reports[2], "space-containing paths agree");
    assert_eq!(mappings[0], mappings[1]);
    assert_eq!(mappings[0], mappings[2]);
}

#[test]
fn cancellation_mid_run_surfaces_cancelled_state_without_fabricated_results() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // A multi-batch workload: the seek fixture looped four times (112 s, 52
    // scheduled windows, two bounded batches of 25 + 27 windows).
    let long = root.join("long recording.wav");
    let mut concat = Command::new("ffmpeg");
    concat.args(["-v", "error", "-y"]);
    for _ in 0..4 {
        concat.arg("-i").arg(synth("recording-seek.wav"));
    }
    concat
        .args([
            "-filter_complex",
            "[0:a][1:a][2:a][3:a]concat=n=4:v=0:a=1",
            "-c:a",
            "pcm_s16le",
        ])
        .arg(&long);
    let output = concat.output().unwrap();
    assert!(output.status.success());
    let (index, _) = build_index(root, &synth("movie.wav"), "index.json");
    let request_path = root.join("report-request.json");
    std::fs::write(
        &request_path,
        serde_json::to_vec(&report_request("synthetic-ci-v1-long", &long)).unwrap(),
    )
    .unwrap();
    let cancel_file = root.join("cancel-me");
    // A cancel file that predates the run surfaces the existing lock-wait
    // cancellation error: the run never starts and nothing is fabricated.
    std::fs::write(&cancel_file, b"cancel").unwrap();
    let error = error_of(
        &cli(root)
            .args(["tv-review", "source-audio-session-report", "--input"])
            .arg(&request_path)
            .arg("--index")
            .arg(&index)
            .arg("--recording")
            .arg(&long)
            .arg("--cache")
            .arg(root.join("cache"))
            .arg("--cancel-file")
            .arg(&cancel_file)
            .output()
            .unwrap(),
    );
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("resource acquisition canceled"),
        "{error}"
    );
    std::fs::remove_file(&cancel_file).unwrap();
    let stdout_path = root.join("stdout.json");
    let stdout = std::fs::File::create(&stdout_path).unwrap();
    let mut child = cli(root)
        .args(["tv-review", "source-audio-session-report", "--input"])
        .arg(&request_path)
        .arg("--index")
        .arg(&index)
        .arg("--recording")
        .arg(&long)
        .arg("--cache")
        .arg(root.join("cache"))
        .arg("--cancel-file")
        .arg(&cancel_file)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Cancel after the second published progress event: the first bounded
    // batch is complete and persisted, later batches are still running.
    let mut events = 0;
    let stderr = child.stderr.take().unwrap();
    for line in BufReader::new(stderr).lines() {
        let line = line.unwrap();
        if line.contains("source-audio.session.progress") {
            events += 1;
            if events == 2 {
                std::fs::write(&cancel_file, b"cancel").unwrap();
            }
        }
    }
    assert_eq!(events, 2, "initial + one completed batch");
    let status = child.wait().unwrap();
    assert!(
        status.success(),
        "cancelled runs return their state, exit 0"
    );
    let envelope: Value = serde_json::from_slice(&std::fs::read(&stdout_path).unwrap()).unwrap();
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["command"], "tv-review.source-audio-session-report");
    let session = &envelope["data"]["session"];
    assert_eq!(session["status"], "cancelled");
    assert_eq!(session["cancelled"], true);
    let completed = session["progress"]["completedWindows"].as_u64().unwrap();
    let total = session["progress"]["totalWindows"].as_u64().unwrap();
    assert_eq!(total, 52);
    assert!(
        (25..total).contains(&completed),
        "the first batch completes, the run stops mid-flight: {completed}/{total}"
    );

    let report = &envelope["data"]["report"];
    assert_eq!(report["status"], "incomplete");
    let completeness = &report["completeness"];
    assert_eq!(completeness["searchComplete"], false);
    assert_eq!(
        completeness["incompleteWindowCount"].as_u64().unwrap(),
        total - completed
    );
    assert_eq!(completeness["accountingComplete"], true);
    assert_eq!(
        completeness["supportedSeconds"].as_f64().unwrap()
            + completeness["unknownSeconds"].as_f64().unwrap(),
        completeness["requestedSeconds"].as_f64().unwrap()
    );
    // No fabricated results: incomplete windows never support or retain, and
    // completed accepted windows are still the only source of segments.
    let windows = report["recognizedWindows"].as_array().unwrap();
    for window in windows {
        if window["complete"] == Value::Bool(false) {
            assert_eq!(window["retained"], false, "{window}");
            assert!(
                window["failureReasons"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|reason| reason == "incomplete_evidence"),
                "{window}"
            );
        } else {
            assert_eq!(window["retained"], window["accepted"], "{window}");
        }
    }
    assert!(
        !report["segments"].as_array().unwrap().is_empty(),
        "completed accepted windows still produce their supported segments"
    );
}
