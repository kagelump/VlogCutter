//! Supported CLI acceptance; generated media only, no custom catalog bridge.
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
use tempfile::tempdir;

fn invoke(path: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .arg("--catalog")
        .arg(path)
        .args(args)
        .output()
        .unwrap()
}
fn success(path: &Path, args: &[&str]) -> Value {
    let output = invoke(path, args);
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["ok"], true);
    envelope["data"].clone()
}
fn failure(path: &Path, args: &[&str], code: &str) {
    let output = invoke(path, args);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let envelope: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(envelope["error"]["code"], code, "{envelope}");
}
fn create(path: &Path, project: &str, input: &Path, base: &str, active: &str) -> Value {
    success(
        path,
        &[
            "edit-plan",
            "create",
            "--project",
            project,
            "--input",
            input.to_str().unwrap(),
            "--expected-base",
            base,
            "--expected-active",
            active,
        ],
    )
}

#[test]
fn edit_plan_cli_requires_explicit_catalog_and_revision_guards() {
    let output = Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .env_remove("RCDVLOG_CATALOG")
        .args(["edit-plan", "list", "--project", "unknown"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "invalid_input");
    let dir = tempdir().unwrap();
    let absent = dir.path().join("absent.sqlite3");
    failure(
        &absent,
        &["edit-plan", "list", "--project", "unknown"],
        "not_found",
    );
    assert!(!absent.exists());
    let output = invoke(&absent, &["edit-plan", "activate", "--revision", "unknown"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn generated_media_plan_create_fork_diff_validate_activate_and_preview() {
    for tool in ["ffmpeg", "ffprobe"] {
        assert!(
            Command::new(tool)
                .arg("-version")
                .output()
                .unwrap()
                .status
                .success(),
            "{tool} required for this media acceptance test"
        );
    }
    let dir = tempdir().unwrap();
    let catalog = dir.path().join("catalog.sqlite3");
    let media = dir.path().join("source media 日本語");
    fs::create_dir(&media).unwrap();
    let source = media.join("numbered fixture.mp4");
    let output = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=30:duration=3",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=3",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let project = success(
        &catalog,
        &["project", "create", "--name", "Synthetic plan review"],
    );
    let project = project["id"].as_str().unwrap();
    success(
        &catalog,
        &[
            "ingest",
            "--project",
            project,
            "--folder",
            media.to_str().unwrap(),
        ],
    );
    let listed = success(&catalog, &["clips", "list", "--project", project]);
    let assets = listed
        .as_array()
        .or_else(|| listed["items"].as_array())
        .unwrap();
    let asset = assets[0]["id"].as_str().unwrap();
    let inspection_input = dir.path().join("inspection.json");
    let mut inspection_request = json!({
        "assetId":asset,"representation":"original","purpose":"proposedBoundary",
        "sourceInSeconds":0.5,"sourceOutSeconds":1.5,"beforeSeconds":0.25,"afterSeconds":0.25,
        "sampleCount":3,"maxWidth":160,"includeAudio":true
    });
    fs::write(
        &inspection_input,
        serde_json::to_vec(&inspection_request).unwrap(),
    )
    .unwrap();
    let inspect_args = [
        "edit-plan",
        "inspect",
        "--input",
        inspection_input.to_str().unwrap(),
    ];
    let inspected = success(&catalog, &inspect_args);
    assert_eq!(
        inspected["inspection"]["frames"].as_array().unwrap().len(),
        3
    );
    assert_eq!(inspected["inspection"]["audio"]["state"], "complete");
    assert_eq!(
        inspected["inspection"]["mapping"]["precision"],
        "actualSourcePts"
    );
    assert_eq!(success(&catalog, &inspect_args)["reused"], true);
    let ranges_args = ["edit-plan", "ranges", "list", "--asset", asset];
    assert_eq!(
        success(&catalog, &ranges_args)["revision"],
        0,
        "inspection never confirms a human constraint"
    );
    let range_input = dir.path().join("human-range.json");
    fs::write(&range_input,serde_json::to_vec(&json!({"assetId":asset,"expectedRevision":0,
        "expectedSourceFingerprint":assets[0]["fingerprint"],"decision":"exclude","media":"picture","decisionOwner":"human",
        "sourceInSeconds":0.5,"sourceOutSeconds":0.6,"rationale":"Explicit generated human review",
        "inspectedEvidenceId":inspected["inspection"]["evidenceId"],"overridesEvidenceIds":["review_false_positive_override"]})).unwrap()).unwrap();
    let confirm_args = [
        "edit-plan",
        "ranges",
        "confirm",
        "--input",
        range_input.to_str().unwrap(),
    ];
    let confirmed = success(&catalog, &confirm_args);
    assert_eq!(confirmed["revision"], 1);
    let bytes = fs::read(&catalog).unwrap();
    assert_eq!(success(&catalog, &ranges_args), confirmed);
    assert_eq!(fs::read(&catalog).unwrap(), bytes);
    failure(&catalog, &confirm_args, "source_ranges_stale_revision");
    let trim_input = dir.path().join("trim.json");
    fs::write(
        &trim_input,
        serde_json::to_vec(
            &json!({"assetId":asset,"inspectedEvidenceId":inspected["inspection"]["evidenceId"],
        "media":"picture","sourceInSeconds":0.4,"sourceOutSeconds":0.8,"preferUtterances":false}),
        )
        .unwrap(),
    )
    .unwrap();
    let suggested = success(
        &catalog,
        &[
            "edit-plan",
            "suggest-trim",
            "--input",
            trim_input.to_str().unwrap(),
        ],
    );
    assert_eq!(suggested["constraintsRevision"], 1);
    assert_eq!(suggested["proposedRanges"].as_array().unwrap().len(), 2);
    assert_eq!(suggested["proposedRanges"][0]["sourceOutSeconds"], 0.5);
    assert_eq!(suggested["proposedRanges"][1]["sourceInSeconds"], 0.6);
    let constraint = confirmed["items"][0]["constraintId"].as_str().unwrap();
    let revoked = success(
        &catalog,
        &[
            "edit-plan",
            "ranges",
            "revoke",
            "--asset",
            asset,
            "--constraint",
            constraint,
            "--expected-revision",
            "1",
            "--rationale",
            "Human withdraws temporary fixture exclusion",
        ],
    );
    assert_eq!(revoked["revision"], 2);
    assert_eq!(revoked["items"][0]["active"], false);
    inspection_request["expectedRepresentationFingerprint"] = json!("stale");
    fs::write(
        &inspection_input,
        serde_json::to_vec(&inspection_request).unwrap(),
    )
    .unwrap();
    failure(&catalog, &inspect_args, "stale_evidence");
    inspection_request["expectedRepresentationFingerprint"] = Value::Null;
    inspection_request["sampleCount"] = json!(65);
    fs::write(
        &inspection_input,
        serde_json::to_vec(&inspection_request).unwrap(),
    )
    .unwrap();
    failure(&catalog, &inspect_args, "invalid_input");
    let exact_end = assets[0]["durationSeconds"].as_f64().unwrap();
    let input = dir.path().join("plan.json");
    fs::write(&input, serde_json::to_vec(&json!({"operations": [
        {"operationId":"first", "position":0,"assetId":asset,"sourceInSeconds":0.0,"sourceOutSeconds":1.0,"rationale":"opening"},
        {"operationId":"repeat", "position":1,"assetId":asset,"sourceInSeconds":2.0,"sourceOutSeconds":exact_end,"rationale":"complete end"}
    ]})).unwrap()).unwrap();
    let a = create(&catalog, project, &input, "none", "none");
    let a_id = a["revisionId"].as_str().unwrap();
    assert_eq!(a["active"], false);
    let bytes = fs::read(&catalog).unwrap();
    let read = success(&catalog, &["edit-plan", "get", "--revision", a_id]);
    assert_eq!(read, a);
    let report = success(&catalog, &["edit-plan", "validate", "--revision", a_id]);
    assert_eq!(report["renderable"], true);
    assert_eq!(
        fs::read(&catalog).unwrap(),
        bytes,
        "reads must not migrate/write"
    );
    failure(
        &catalog,
        &[
            "edit-plan",
            "activate",
            "--revision",
            a_id,
            "--expected-active",
            a_id,
        ],
        "edit_plan_stale_revision",
    );
    success(
        &catalog,
        &[
            "edit-plan",
            "activate",
            "--revision",
            a_id,
            "--expected-active",
            "none",
        ],
    );
    let mut cuts = a["operations"].as_array().unwrap().clone();
    cuts.swap(0, 1);
    cuts[0]["position"] = json!(0);
    cuts[1]["position"] = json!(1);
    cuts[1]["sourceInSeconds"] = json!(0.25);
    fs::write(
        &input,
        serde_json::to_vec(&json!({"operations":cuts})).unwrap(),
    )
    .unwrap();
    let b = create(&catalog, project, &input, a_id, a_id);
    let b_id = b["revisionId"].as_str().unwrap();
    assert_eq!(b["operations"].as_array().unwrap().len(), 2);
    let diff = success(
        &catalog,
        &["edit-plan", "diff", "--base", a_id, "--revision", b_id],
    );
    assert_eq!(diff["changes"].as_array().unwrap().len(), 2);
    let preview = success(
        &catalog,
        &[
            "edit-plan",
            "preview",
            "--revision",
            b_id,
            "--max-height",
            "144",
        ],
    );
    assert_eq!(preview["preview"]["clipCount"], 2);
    assert!(
        preview["preview"]["failures"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let output = preview["preview"]["outputPath"].as_str().unwrap();
    assert!(Path::new(output).is_file());
    assert!(
        Command::new("ffmpeg")
            .args(["-v", "error", "-i", output, "-f", "null", "-"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let artifact = preview["preview"]["artifactId"].as_str().unwrap();
    let stored = success(
        &catalog,
        &["edit-plan", "preview-status", "--artifact", artifact],
    );
    assert_eq!(stored["artifact"]["state"], "complete");
    assert!(stored["currentForRequest"].is_null());
    assert_eq!(
        stored["activeRevisionId"], a_id,
        "rendering a draft never activates it"
    );
    let verified = success(
        &catalog,
        &[
            "edit-plan",
            "preview-status",
            "--artifact",
            artifact,
            "--verify",
        ],
    );
    assert_eq!(verified["currentForRequest"], true);
    assert_eq!(verified["artifact"]["validation"]["fullyDecoded"], true);
    let page = success(
        &catalog,
        &[
            "edit-plan",
            "preview-list",
            "--revision",
            b_id,
            "--limit",
            "1",
        ],
    );
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["artifactId"], artifact);
    let waited = success(
        &catalog,
        &[
            "edit-plan",
            "preview-wait",
            "--artifact",
            artifact,
            "--timeout-seconds",
            "0",
        ],
    );
    assert_eq!(waited["observedState"], "complete");
    failure(
        &catalog,
        &["edit-plan", "preview-cancel", "--artifact", artifact],
        "preview_artifact_terminal",
    );
    failure(
        &catalog,
        &["edit-plan", "preview-status", "--artifact", "missing"],
        "not_found",
    );
    #[cfg(unix)]
    cancel_live_preview_child(&catalog, b_id, dir.path());
    success(
        &catalog,
        &[
            "edit-plan",
            "activate",
            "--revision",
            b_id,
            "--expected-active",
            a_id,
        ],
    );
    failure(
        &catalog,
        &[
            "edit-plan",
            "activate",
            "--revision",
            a_id,
            "--expected-active",
            a_id,
        ],
        "edit_plan_stale_revision",
    );
    assert_eq!(
        success(&catalog, &["edit-plan", "get", "--revision", a_id])["operations"],
        a["operations"]
    );
    cuts[0]["audio"] = json!({"mode":"source", "version":1, "assetId":asset,
        "sourceInSeconds":2.0,"sourceOutSeconds":2.5,"timelineStartSeconds":0.0,
        "syncOffsetSeconds":0.0,"gainDb":-6.0,"fadeInSeconds":0.02,"fadeOutSeconds":0.02});
    fs::write(
        &input,
        serde_json::to_vec(&json!({"operations":cuts})).unwrap(),
    )
    .unwrap();
    let audio = create(&catalog, project, &input, b_id, b_id);
    let audio_id = audio["revisionId"].as_str().unwrap();
    assert_eq!(audio["operations"][0]["audio"]["version"], 1);
    let diff = success(
        &catalog,
        &["edit-plan", "diff", "--base", b_id, "--revision", audio_id],
    );
    assert_eq!(diff["changes"][0]["changes"], json!(["audio"]));
    let audio_preview = success(
        &catalog,
        &[
            "edit-plan",
            "preview",
            "--revision",
            audio_id,
            "--max-height",
            "144",
        ],
    );
    assert_eq!(audio_preview["revisionId"], audio_id);
    assert_eq!(audio_preview["preview"]["clipCount"], 2);
    assert_eq!(
        success(&catalog, &["edit-plan", "list", "--project", project])["activeRevisionId"],
        b_id
    );
    cuts[0].as_object_mut().unwrap().remove("audio");
    cuts[0]["sourceOutSeconds"] = json!(exact_end + 1.0);
    fs::write(
        &input,
        serde_json::to_vec(&json!({"operations":cuts})).unwrap(),
    )
    .unwrap();
    let invalid = create(&catalog, project, &input, b_id, b_id);
    let invalid_id = invalid["revisionId"].as_str().unwrap();
    failure(
        &catalog,
        &[
            "edit-plan",
            "activate",
            "--revision",
            invalid_id,
            "--expected-active",
            b_id,
        ],
        "edit_plan_validation_failed",
    );
    assert_eq!(
        success(&catalog, &["edit-plan", "get", "--revision", invalid_id]),
        invalid
    );
    assert_eq!(
        success(&catalog, &["edit-plan", "list", "--project", project])["activeRevisionId"],
        b_id
    );
}

/// Exercise cancellation of a live media child in a separate CLI process,
/// without process-global tool overrides or races with other Rust tests.
#[cfg(unix)]
fn cancel_live_preview_child(catalog: &Path, revision: &str, parent: &Path) {
    use std::{
        os::unix::fs::PermissionsExt,
        process::Stdio,
        time::{Duration, Instant},
    };
    let tools = parent.join("controlled-preview-tools");
    fs::create_dir(&tools).unwrap();
    let wrapper = tools.join("ffmpeg");
    let marker = parent.join("controlled-preview-child.pid");
    fs::write(&wrapper, "#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then exec \"$RCDVLOG_TEST_REAL_FFMPEG\" \"$@\"; fi\necho $$ > \"$RCDVLOG_TEST_PID_FILE\"\nexec sleep 120\n").unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let real_ffmpeg = rcdvlog_core::resolve_tool_path("ffmpeg");
    let mut child = Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .arg("--catalog")
        .arg(catalog)
        .args([
            "edit-plan",
            "preview",
            "--revision",
            revision,
            "--max-height",
            "144",
        ])
        .env(
            "PATH",
            std::env::join_paths(
                std::iter::once(tools)
                    .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
            )
            .unwrap(),
        )
        .env("RCDVLOG_TEST_REAL_FFMPEG", real_ffmpeg)
        .env("RCDVLOG_TEST_PID_FILE", &marker)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while !marker.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("controlled preview exited before child started: {status}");
        }
        if started.elapsed() > Duration::from_secs(20) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("controlled child never started");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let page = success(
        catalog,
        &[
            "edit-plan",
            "preview-list",
            "--revision",
            revision,
            "--limit",
            "1",
        ],
    );
    let id = page["items"][0]["artifactId"].as_str().unwrap();
    assert_eq!(page["items"][0]["observedState"], "running");
    failure(
        catalog,
        &[
            "edit-plan",
            "preview-wait",
            "--artifact",
            id,
            "--timeout-seconds",
            "0",
        ],
        "preview_wait_timeout",
    );
    assert!(
        child.try_wait().unwrap().is_none(),
        "observation timeout cancelled the original child"
    );
    let cancelled = success(catalog, &["edit-plan", "preview-cancel", "--artifact", id]);
    assert_eq!(cancelled["artifact"]["cancelRequested"], true);
    let deadline = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(!status.success());
            break;
        }
        if deadline.elapsed() > Duration::from_secs(10) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("cancel did not reap the original render process");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let terminal = success(
        catalog,
        &[
            "edit-plan",
            "preview-wait",
            "--artifact",
            id,
            "--timeout-seconds",
            "1",
        ],
    );
    assert_eq!(terminal["observedState"], "cancelled");
    assert!(terminal["artifact"]["result"].is_null());
    let pid = fs::read_to_string(&marker).unwrap();
    let alive = Command::new("kill")
        .args(["-0", pid.trim()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!alive.success(), "cancelled media child was left alive");
}
