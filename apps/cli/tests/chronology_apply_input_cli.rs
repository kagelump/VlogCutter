//! Process-level contract tests for `chronology apply` input handling.
//!
//! A preview is extracted from the success envelope at `data.data` and sent back
//! as `preview`. Malformed request or preview JSON is a caller error: it must
//! fail as `invalid_input` with an empty stdout and one JSON error envelope on
//! stderr, and it must not change the catalog.

use rcdvlog_core::{Catalog, ProjectAssetAddedPayload};
use serde_json::{Value, json};
use std::{
    ffi::OsStr,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};
use tempfile::tempdir;

fn invoke_with_stdin<I, S>(catalog: &Path, arguments: I, input: &str) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut child = Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .arg("--catalog")
        .arg(catalog)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run rcdvlog CLI with JSON stdin");
    child
        .stdin
        .take()
        .expect("stdin pipe")
        .write_all(input.as_bytes())
        .expect("write JSON stdin");
    child.wait_with_output().expect("collect CLI output")
}

fn success_with_stdin(catalog: &Path, project: &str, command: &str, input: &str) -> Value {
    let output = invoke_with_stdin(
        catalog,
        ["chronology", command, "--project", project, "--input", "-"],
        input,
    );
    assert!(
        output.status.success(),
        "chronology {command} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("success envelope on stdout")
}

/// Asserts the documented failure contract and returns the stderr envelope.
fn invalid_input(catalog: &Path, project: &str, command: &str, input: &str) -> Value {
    let output = invoke_with_stdin(
        catalog,
        ["chronology", command, "--project", project, "--input", "-"],
        input,
    );
    assert!(!output.status.success(), "malformed input must fail");
    assert!(
        output.stdout.is_empty(),
        "failures must not write to stdout"
    );
    let envelope: Value =
        serde_json::from_slice(&output.stderr).expect("single JSON error envelope on stderr");
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["command"], format!("chronology.{command}"));
    assert_eq!(envelope["error"]["code"], "invalid_input", "{envelope}");
    envelope
}

fn open_questions(catalog: &Path, project: &str) -> usize {
    let output = Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .arg("--catalog")
        .arg(catalog)
        .args(["chronology", "questions", "--project", project])
        .output()
        .expect("run questions listing");
    assert!(output.status.success());
    let listing: Value = serde_json::from_slice(&output.stdout).expect("questions JSON");
    listing["data"]["openQuestions"].as_array().unwrap().len()
}

/// A catalog with one synthetic clock-block question, previewed through the CLI.
/// Returns the catalog path, the project id, and the full preview envelope.
fn question_preview(directory: &Path) -> (std::path::PathBuf, String, Value) {
    let catalog_path = directory.join("catalog.sqlite3");
    let created = Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .arg("--catalog")
        .arg(&catalog_path)
        .args(["project", "create", "--name", "synthetic apply input"])
        .output()
        .expect("create project");
    assert!(created.status.success());
    let project: Value = serde_json::from_slice(&created.stdout).unwrap();
    let project = project["data"]["id"].as_str().unwrap().to_string();

    let catalog = Catalog::open(&catalog_path).unwrap();
    for (index, (filename, raw, camera)) in [
        ("C0001.MP4", "2030-01-01T09:00:00Z", "Synthetic Camera"),
        ("C0002.MP4", "2030-01-01T10:00:00Z", "Synthetic Camera"),
        ("R0001.MP4", "2029-12-31T12:00:00Z", "Reference Camera"),
    ]
    .into_iter()
    .enumerate()
    {
        catalog
            .add_portable_project_asset(
                &project,
                ProjectAssetAddedPayload {
                    id: format!("synthetic-asset-{index}"),
                    fingerprint: format!("synthetic-fingerprint-{index}"),
                    filename: filename.into(),
                    size_bytes: 1,
                    duration_seconds: 2.0,
                    frame_rate: 30.0,
                    width: 1,
                    height: 1,
                    codec: "h264".into(),
                    camera: camera.into(),
                    captured_at_raw: Some(raw.into()),
                    timestamp_candidates: None,
                    best_time: Some(raw.into()),
                    metadata_rotation_degrees: 0,
                    default_rotation_degrees: 0,
                    created_at: "2030-01-01T00:00:00Z".into(),
                },
            )
            .unwrap();
    }
    let facts = catalog.chronology_facts(&project).unwrap();
    let first = &facts.assets[0];
    let sequence = first.capture_sequence_id.clone().unwrap();
    let reference = facts
        .assets
        .iter()
        .find(|asset| asset.capture_sequence_id.as_deref() != Some(sequence.as_str()))
        .unwrap();
    let counter = first.filename_counter.unwrap();
    let question = json!({
        "id":"synthetic-question", "captureSequenceId":sequence,
        "startCounter":counter, "endCounter":counter + 1,
        "alternatives":[
            {"id":"date-a","label":"Synthetic date A","epoch":{"id":"synthetic-epoch-a","captureSequenceId":sequence,"startCounter":counter,"endCounter":counter + 1,"offsetSeconds":0,"confidence":1.0,"evidence":["synthetic reference A"],"label":"Date A"},"evidence":["synthetic reference A"]},
            {"id":"date-b","label":"Synthetic date B","epoch":{"id":"synthetic-epoch-b","captureSequenceId":sequence,"startCounter":counter,"endCounter":counter + 1,"offsetSeconds":86400,"confidence":1.0,"evidence":["synthetic reference B"],"label":"Date B"},"evidence":["synthetic reference B"]}
        ],
        "proxyExcerpts":[{"assetId":first.asset_id,"startSeconds":0.0,"endSeconds":1.0,"role":"selected_proxy"},{"assetId":reference.asset_id,"startSeconds":0.0,"endSeconds":1.0,"role":"independent_reference"}],
        "anchor":{"kind":"event_at_utc","assetId":first.asset_id,"referenceAssetId":reference.asset_id,"eventLabel":"Synthetic event","utcTime":"2030-01-01T12:00:00Z"},
        "contextualEvidence":["reviewer supplied overlap context"]
    });
    let preview = success_with_stdin(
        &catalog_path,
        &project,
        "preview",
        &json!({"kind":"questionPropose","question":question}).to_string(),
    );
    (catalog_path, project, preview)
}

#[test]
fn chronology_apply_accepts_the_documented_data_data_preview() {
    let directory = tempdir().expect("temporary catalog directory");
    let (catalog, project, envelope) = question_preview(directory.path());
    assert_eq!(envelope["data"]["kind"], "questionPropose");
    assert_eq!(
        envelope["data"]["data"]["alternativeImpacts"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let request = json!({"kind":"questionPropose","preview":envelope["data"]["data"]});
    let applied = success_with_stdin(&catalog, &project, "apply", &request.to_string());
    assert_eq!(applied["ok"], true);
    assert_eq!(applied["data"]["kind"], "questionPropose");
    assert!(applied["data"]["data"]["revision"].is_number(), "{applied}");
    assert_eq!(open_questions(&catalog, &project), 1);
}

#[test]
fn chronology_apply_names_a_missing_preview_field_as_invalid_input() {
    let directory = tempdir().expect("temporary catalog directory");
    let (catalog, project, envelope) = question_preview(directory.path());
    let mut preview = envelope["data"]["data"].clone();
    preview.as_object_mut().unwrap().remove("question");
    let request = json!({"kind":"questionPropose","preview":preview});

    let error = invalid_input(&catalog, &project, "apply", &request.to_string());
    let message = error["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("missing field `question`"),
        "message should name the missing field: {message}"
    );
    assert_eq!(open_questions(&catalog, &project), 0);
}

#[test]
fn chronology_apply_rejects_the_wrong_nesting_level_as_invalid_input() {
    let directory = tempdir().expect("temporary catalog directory");
    let (catalog, project, envelope) = question_preview(directory.path());

    // The whole success envelope, instead of its data.data value.
    let whole_envelope = json!({"kind":"questionPropose","preview":envelope});
    let error = invalid_input(&catalog, &project, "apply", &whole_envelope.to_string());
    let message = error["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("data.data"),
        "message should name the expected level: {message}"
    );

    // The data value, one level short of data.data.
    let one_level_short = json!({"kind":"questionPropose","preview":envelope["data"]});
    let error = invalid_input(&catalog, &project, "apply", &one_level_short.to_string());
    let message = error["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("data.data"),
        "message should name the expected level: {message}"
    );
    assert_eq!(open_questions(&catalog, &project), 0);
}

#[test]
fn chronology_apply_rejects_non_json_input_as_invalid_input() {
    let directory = tempdir().expect("temporary catalog directory");
    let (catalog, project, _envelope) = question_preview(directory.path());

    let error = invalid_input(&catalog, &project, "apply", "this is not json");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("not valid JSON"),
        "message should say the request is not JSON: {message}"
    );
    assert_eq!(open_questions(&catalog, &project), 0);
}
