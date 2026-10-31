//! Complete supported local editing/review/delivery loop; generated sources only.
use serde_json::{Value, json};
use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};
use tempfile::tempdir;

fn invoke_with_path(catalog: &Path, args: &[&str], path: Option<&OsStr>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rcdvlog"));
    command.arg("--catalog").arg(catalog).args(args);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    command.output().unwrap()
}

fn ok_with_path(catalog: &Path, args: &[&str], path: Option<&OsStr>) -> Value {
    let output = invoke_with_path(catalog, args, path);
    assert!(
        output.status.success(),
        "args={args:?}\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["ok"], true);
    envelope["data"].clone()
}

fn ok(catalog: &Path, args: &[&str]) -> Value {
    ok_with_path(catalog, args, None)
}

fn supported_tool_path(dir: &Path) -> OsString {
    let tools = dir.join("supported media tools");
    fs::create_dir(&tools).unwrap();
    for (name, arg, version) in [
        ("ffmpeg", "-version", "ffmpeg version 7.1.5"),
        ("ffprobe", "-version", "ffprobe version 7.1.5"),
        ("exiftool", "-ver", "13.36"),
    ] {
        let path = tools.join(name);
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nif [ \"${{1:-}}\" = \"{arg}\" ]; then\n  printf '%s\\n' '{version}'\n  exit 0\nfi\nexit 2\n"
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
    }

    let mut paths = vec![tools];
    if let Some(current) = env::var_os("PATH") {
        paths.extend(env::split_paths(&current));
    }
    env::join_paths(paths).unwrap()
}
fn input(dir: &Path, name: &str, value: &Value) -> String {
    let p = dir.join(name);
    fs::write(&p, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    p.to_str().unwrap().into()
}

#[test]
fn supported_cli_reuses_one_catalog_for_sparse_edits_review_and_delivery() {
    let dir = tempdir().unwrap();
    let catalog = dir.path().join("catalog.sqlite3");
    let sources = dir.path().join("generated sources 日本語");
    fs::create_dir(&sources).unwrap();
    for (i, color) in ["red", "blue", "green"].iter().enumerate() {
        let output = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("color=c={color}:s=160x90:r=30:d=4"),
                "-f",
                "lavfi",
                "-i",
                &format!(
                    "sine=frequency={}:sample_rate=48000:duration=4",
                    440 + i * 100
                ),
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(sources.join(format!("{i}.mp4")))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let project = ok(
        &catalog,
        &["project", "create", "--name", "Generated RC readiness"],
    )["id"]
        .as_str()
        .unwrap()
        .to_owned();
    ok(
        &catalog,
        &[
            "ingest",
            "--project",
            &project,
            "--folder",
            sources.to_str().unwrap(),
        ],
    );
    let listed = ok(&catalog, &["clips", "list", "--project", &project]);
    let assets = listed
        .as_array()
        .or_else(|| listed["items"].as_array())
        .unwrap();
    let a = assets[0]["id"].as_str().unwrap();
    let b = assets[1]["id"].as_str().unwrap();
    // The host toolchain is used for real generated-media work below. Keep the
    // workflow preflight deterministic across Linux distributions whose bundled
    // FFmpeg may be older than the supported minimum.
    let supported_path = supported_tool_path(dir.path());
    let ready = ok_with_path(
        &catalog,
        &["doctor", "--workflow", "delivery"],
        Some(&supported_path),
    );
    assert_eq!(ready["catalogStatus"]["compatible"], true);
    let plan = input(
        dir.path(),
        "base.json",
        &json!({"operations":[
            {"operationId":"first","position":0,"assetId":a,"sourceInSeconds":0.5,"sourceOutSeconds":1.5,"rationale":"complete first beat"},
            {"operationId":"second","position":1,"assetId":a,"sourceInSeconds":1.5,"sourceOutSeconds":2.5,"rationale":"retained dialogue",
                "audio":{"mode":"source","version":1,"assetId":a,"sourceInSeconds":1.5,"sourceOutSeconds":2.5,
                    "timelineStartSeconds":1.0,"syncOffsetSeconds":0.0,"gainDb":0.0,"fadeInSeconds":0.025,"fadeOutSeconds":0.04}}
        ]}),
    );
    let base = ok(
        &catalog,
        &[
            "edit-plan",
            "create",
            "--project",
            &project,
            "--input",
            &plan,
            "--expected-base",
            "none",
            "--expected-active",
            "none",
        ],
    );
    let base_id = base["revisionId"].as_str().unwrap();
    let patch = input(
        dir.path(),
        "patch.json",
        &json!({"version":1,"name":"Brisk connection","edits":[
        {"kind":"insert","anchor":{"kind":"before","operationId":"first"},"operation":{
            "operationId":"new-view","position":0,"assetId":b,"sourceInSeconds":0.2,"sourceOutSeconds":1.0,
            "rationale":"distinct unused perspective","audio":{"mode":"silent"}}}]}),
    );
    let draft = ok(
        &catalog,
        &[
            "edit-plan",
            "patch",
            "--project",
            &project,
            "--expected-base",
            base_id,
            "--expected-active",
            "none",
            "--input",
            &patch,
        ],
    );
    let revision = draft["revision"]["revisionId"].as_str().unwrap();
    assert_eq!(draft["revision"]["active"], false);
    assert!(
        (draft["revision"]["operations"][2]["audio"]["timelineStartSeconds"]
            .as_f64()
            .unwrap()
            - 1.8)
            .abs()
            < 1e-9
    );
    let report_input = input(
        dir.path(),
        "coverage.json",
        &json!({"revisionId":revision,"baseRevisionId":base_id,"targetPercent":50.0}),
    );
    let coverage = ok(
        &catalog,
        &["edit-plan", "coverage", "report", "--input", &report_input],
    );
    assert_eq!(coverage["inventorySources"], 3);
    assert_eq!(coverage["uniquePictureSources"], 2);
    assert_eq!(coverage["addedSources"].as_array().unwrap().len(), 1);
    let unused = ok(
        &catalog,
        &[
            "edit-plan",
            "coverage",
            "unused",
            "--revision",
            revision,
            "--limit",
            "1",
        ],
    );
    assert_eq!(unused["total"], 1);
    let excerpt = input(
        dir.path(),
        "excerpt.json",
        &json!({"revisionId":revision,"maxHeight":144,
        "scope":{"firstOperationId":"new-view","lastOperationId":"second"}}),
    );
    let first = ok(&catalog, &["edit-plan", "excerpt", "--input", &excerpt]);
    let artifact = first["artifactId"].as_str().unwrap();
    let first_status = ok(
        &catalog,
        &[
            "edit-plan",
            "preview-status",
            "--artifact",
            artifact,
            "--verify",
        ],
    );
    assert_eq!(first_status["currentForRequest"], true);
    let _second = ok(&catalog, &["edit-plan", "excerpt", "--input", &excerpt]);
    let historical = ok(
        &catalog,
        &[
            "edit-plan",
            "preview-status",
            "--artifact",
            artifact,
            "--verify",
        ],
    );
    assert_eq!(historical["latestAttempt"], false);
    assert_eq!(historical["currentForRequest"], true);
    let note_input = input(
        dir.path(),
        "note.json",
        &json!({"projectId":project,"revisionId":revision,"operationId":"new-view",
        "author":"reviewer","category":"preference","observation":"Generated provisional review fixture",
        "viewedArtifactId":artifact,"movieTimeSeconds":0.3,"evidenceRefs":[]}),
    );
    let note = ok(
        &catalog,
        &["edit-plan", "feedback", "record", "--input", &note_input],
    );
    let decision = input(
        dir.path(),
        "decision.json",
        &json!({"noteId":note["note"]["noteId"],"expectedRevision":note["revision"],
        "proposalId":null,"action":"accepted","rationale":"Delegated provisional choice",
        "hardChoice":null,"provenance":{"actor":"agent","authority":"delegatedEditing","confidence":"provisional"}}),
    );
    let decided = ok(
        &catalog,
        &["edit-plan", "feedback", "decide", "--input", &decision],
    );
    assert_eq!(decided["latestDecision"]["decisionOrigin"], "agent");
    let review = input(
        dir.path(),
        "review.json",
        &json!({"projectId":project,"revisionId":revision,
        "operationId":"new-view","artifactId":artifact,"actor":"agent","method":"sparseSampling",
        "explicitHumanPlayback":false,"evidenceRefs":["generated-acceptance"],"uncertainty":"Sampled frames only",
        "intervals":[{"modality":"picture","movieStartSeconds":0.1,"movieEndSeconds":0.2,"coverage":"sampled"}]}),
    );
    let recorded = ok(
        &catalog,
        &["edit-plan", "review-coverage", "record", "--input", &review],
    );
    assert_eq!(recorded["actor"], "agent");
    let reviewed = ok(
        &catalog,
        &[
            "edit-plan",
            "review-coverage",
            "list",
            "--revision",
            revision,
        ],
    );
    assert_eq!(reviewed["total"], 1);
    ok(
        &catalog,
        &[
            "edit-plan",
            "review-coverage",
            "report",
            "--revision",
            revision,
            "--verify",
        ],
    );
    let sample_input = input(
        dir.path(),
        "sample-plan.json",
        &json!({"artifactId":artifact,"sampleSeconds":0.5,"maxSamples":8}),
    );
    let sample = ok(
        &catalog,
        &["edit-plan", "audio-audit", "plan", "--input", &sample_input],
    );
    assert!(!sample["request"]["samples"].as_array().unwrap().is_empty());
    let audit_input = input(dir.path(), "audit.json", &sample["request"]);
    let audit = ok(
        &catalog,
        &[
            "edit-plan",
            "audio-audit",
            "create",
            "--input",
            &audit_input,
        ],
    );
    assert_eq!(audit["report"]["state"], "complete");
    let bundle_input = input(
        dir.path(),
        "bundle.json",
        &json!({"artifactId":artifact,"outputDirectory":dir.path().join("delivery"),
        "name":"generated-readiness","contactSheet":true}),
    );
    let bundle = ok(&catalog, &["edit-plan", "bundle", "--input", &bundle_input]);
    assert!(Path::new(bundle["manifestPath"].as_str().unwrap()).is_file());
    assert!(Path::new(bundle["moviePath"].as_str().unwrap()).is_file());
    assert_eq!(
        ok(&catalog, &["edit-plan", "bundle", "--input", &bundle_input])["reused"],
        true
    );
    let history = ok(&catalog, &["youtube", "history", "--project", &project]);
    assert_eq!(history["uploads"].as_array().unwrap().len(), 0);
    assert_eq!(
        ok(&catalog, &["edit-plan", "get", "--revision", base_id]),
        base
    );
}
