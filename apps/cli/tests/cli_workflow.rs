use rcdvlog_core::{
    Catalog, PROJECT_CATALOG_SCHEMA_VERSION, ProjectAssetAddedPayload, RepresentationKind,
    RunPolicyRequest,
};
use rusqlite::{Connection, params};
use serde_json::Value;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use tempfile::tempdir;

struct Invocation {
    json: Value,
    stderr: String,
}

fn invoke<I, S>(catalog: &Path, arguments: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .arg("--catalog")
        .arg(catalog)
        .args(arguments)
        .output()
        .expect("run rcdvlog CLI")
}

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

fn chronology_apply(catalog: &Path, project: &str, request: Value) -> Value {
    let preview_output = invoke_with_stdin(
        catalog,
        [
            "chronology",
            "preview",
            "--project",
            project,
            "--input",
            "-",
        ],
        &request.to_string(),
    );
    assert!(
        preview_output.status.success(),
        "chronology preview failed: {}",
        String::from_utf8_lossy(&preview_output.stderr)
    );
    let preview: Value = serde_json::from_slice(&preview_output.stdout).expect("preview JSON");
    let output = invoke_with_stdin(
        catalog,
        ["chronology", "apply", "--project", project, "--input", "-"],
        &serde_json::json!({"kind":request["kind"], "preview":preview["data"]["data"]}).to_string(),
    );
    assert!(
        output.status.success(),
        "chronology apply failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("chronology JSON output")
}

fn invoke_with_path<I, S>(catalog: &Path, arguments: I, path: &OsStr) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .arg("--catalog")
        .arg(catalog)
        .args(arguments)
        .env("PATH", path)
        .output()
        .expect("run rcdvlog CLI with synthetic tool")
}

fn success<I, S>(catalog: &Path, arguments: I) -> Invocation
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = invoke(catalog, arguments);
    assert!(
        output.status.success(),
        "CLI failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let json: Value = serde_json::from_slice(&output.stdout).expect("single JSON stdout value");
    assert_eq!(json["ok"], true);
    Invocation {
        json,
        stderr: String::from_utf8(output.stderr).expect("UTF-8 stderr"),
    }
}

fn failure<I, S>(catalog: &Path, arguments: I) -> Value
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = invoke(catalog, arguments);
    assert!(!output.status.success(), "CLI unexpectedly succeeded");
    assert!(
        output.stdout.is_empty(),
        "failures must not write to stdout"
    );
    let json: Value = serde_json::from_slice(&output.stderr).expect("single JSON stderr value");
    assert_eq!(json["ok"], false);
    json
}

fn create_project(catalog: &Path, name: &str) -> String {
    success(catalog, ["project", "create", "--name", name]).json["data"]["id"]
        .as_str()
        .expect("project id")
        .to_string()
}

fn insert_moment_scope_asset(catalog: &Path, project: &str, id: &str, path: &Path) {
    fs::write(path, b"synthetic clip placeholder").unwrap();
    let connection = Connection::open(catalog).unwrap();
    connection.execute(
        "INSERT INTO assets(id,project_id,fingerprint,path,filename,size_bytes,duration_seconds,created_at)
         VALUES (?1,?2,?3,?4,?5,24,10,'2026-10-22T00:00:00Z')",
        params![id, project, format!("fp-{id}"), path.to_string_lossy(), path.file_name().unwrap().to_string_lossy()],
    ).unwrap();
    connection.execute(
        "INSERT INTO asset_video_validation(asset_id,fingerprint,status,diagnostic,validated_at)
         VALUES (?1,?2,'usable',NULL,'2026-10-22T00:00:00Z')",
        params![id, format!("fp-{id}")],
    ).unwrap();
}

#[test]
fn analysis_pilot_with_absent_credentials_stops_before_provider_construction() {
    let directory = tempdir().unwrap();
    let source_dir = directory.path().join("source");
    fs::create_dir(&source_dir).unwrap();
    let source = source_dir.join("synthetic.mp4");
    let generated = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=320x180:r=24:d=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&source)
        .status();
    let Ok(status) = generated else {
        eprintln!("skipping absent-credentials pilot test: ffmpeg unavailable");
        return;
    };
    if !status.success() {
        eprintln!("skipping absent-credentials pilot test: H.264 encoder unavailable");
        return;
    }
    let path = directory.path().join("catalog.sqlite3");
    let catalog = Catalog::open(&path).unwrap();
    let project = catalog
        .create_project("Synthetic absent credentials")
        .unwrap()
        .id;
    catalog.ingest_folder(&project, &source_dir).unwrap();
    let asset = catalog.list_assets(&project).unwrap().remove(0);
    catalog.generate_proxy(&asset.id).unwrap();
    let request = RunPolicyRequest::observation(
        project.clone(),
        vec![asset.id.clone()],
        rcdvlog_core::DEFAULT_MODEL,
        vec![
            rcdvlog_core::DEFAULT_MODEL.into(),
            rcdvlog_core::FALLBACK_MODEL.into(),
        ],
        true,
        1.0,
        false,
    );
    let authorization = catalog.authorize_run_policy(&request).unwrap();
    drop(catalog);

    let isolated_home = directory.path().join("isolated-home");
    fs::create_dir(&isolated_home).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .arg("--catalog")
        .arg(&path)
        .args([
            "analyze",
            "pilot",
            "--project",
            &project,
            "--asset",
            &asset.id,
            "--provider",
            "openrouter",
            "--primary-model",
            rcdvlog_core::DEFAULT_MODEL,
            "--allow-model",
            rcdvlog_core::DEFAULT_MODEL,
            "--allow-model",
            rcdvlog_core::FALLBACK_MODEL,
            "--budget-usd",
            "1.0",
            "--payload",
            "frames",
        ])
        .env_remove("OPENROUTER_API_KEY")
        .env("HOME", &isolated_home)
        .output()
        .expect("run absent-credentials pilot CLI");
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).expect("single JSON error");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no pilot request was made"),
        "unexpected error: {error}"
    );
    let catalog = Catalog::open(&path).unwrap();
    assert!(
        catalog
            .load_run_pilot(&authorization.policy.policy_id)
            .unwrap()
            .is_none(),
        "credential failure must not create a pilot transmission record"
    );
}

#[test]
fn analysis_policy_preflight_is_read_only_and_unsupported_payload_fails_before_authorization() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let project = create_project(&catalog, "Analysis policy CLI");
    let clip = directory.path().join("policy-fixture.mp4");
    insert_moment_scope_asset(&catalog, &project, "policy_asset", &clip);
    let before = fs::read(&catalog).unwrap();

    let first = success(
        &catalog,
        [
            "analyze",
            "preflight",
            "--project",
            &project,
            "--asset",
            "policy_asset",
            "--provider",
            "openrouter",
            "--primary-model",
            "qwen/qwen3.7-flash",
            "--budget-usd",
            "1.0",
            "--payload",
            "frames",
        ],
    );
    assert_eq!(first.json["data"]["scope"][0]["assetId"], "policy_asset");
    assert_eq!(first.json["data"]["provider"], "openrouter");
    assert_eq!(
        first.json["data"]["allowedModels"],
        serde_json::json!(["qwen/qwen3.7-flash", "google/gemini-2.5-flash-lite"])
    );
    assert!(
        first.json["data"].get("apiKey").is_none(),
        "preflight must never emit credentials"
    );
    assert_eq!(
        fs::read(&catalog).unwrap(),
        before,
        "preflight changed catalog bytes"
    );

    let unsupported = failure(
        &catalog,
        [
            "analyze",
            "authorize",
            "--project",
            &project,
            "--asset",
            "policy_asset",
            "--provider",
            "openrouter",
            "--primary-model",
            "qwen/qwen3.7-flash",
            "--budget-usd",
            "1.0",
            "--payload",
            "audio",
        ],
    );
    assert_eq!(unsupported["error"]["code"], "invalid_input");
    assert!(
        unsupported["error"]["message"]
            .as_str()
            .unwrap()
            .contains("audio")
    );

    let cannot_run_unapproved = failure(
        &catalog,
        ["analyze", "run", "--policy-id", "not-authorized"],
    );
    assert_eq!(cannot_run_unapproved["error"]["code"], "invalid_input");
    assert!(
        cannot_run_unapproved["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not authorized")
    );
}

#[test]
fn moments_plan_and_inventory_are_explicit_scoped_read_only_commands() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let project = create_project(&catalog, "Moment scope CLI");
    let clip = directory.path().join("fixture.mp4");
    insert_moment_scope_asset(&catalog, &project, "moment_asset", &clip);
    let before = fs::read(&catalog).unwrap();

    let planned = success(
        &catalog,
        [
            "moments",
            "plan",
            "--project",
            &project,
            "--asset",
            "moment_asset",
            "--provider",
            "openrouter",
            "--model",
            "qwen/qwen3.7-flash",
            "--payload",
            "frames",
            "--budget-usd",
            "0.5",
        ],
    );
    assert_eq!(
        planned.json["data"]["assetIds"],
        serde_json::json!(["moment_asset"])
    );
    assert_eq!(planned.json["data"]["coarseFramesPerAsset"], 96);
    assert_eq!(planned.json["data"]["refinementFramesPerAsset"], 384);
    assert_eq!(
        planned.json["data"]["runPolicy"]["scope"][0]["assetId"],
        "moment_asset"
    );
    assert_eq!(planned.json["data"]["runPolicy"]["budgetUsd"], 0.5);
    assert!(
        planned.json["data"]["runPolicy"]["readiness"]["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker["code"] == "source_not_ready")
    );

    let all_usable = success(
        &catalog,
        [
            "moments",
            "plan",
            "--project",
            &project,
            "--all-usable",
            "--provider",
            "openrouter",
            "--model",
            "qwen/qwen3.7-flash",
            "--payload",
            "frames",
            "--budget-usd",
            "0.5",
        ],
    );
    assert_eq!(
        all_usable.json["data"]["assetIds"],
        serde_json::json!(["moment_asset"])
    );

    let listed = success(
        &catalog,
        [
            "moments",
            "list",
            "--project",
            &project,
            "--offset",
            "0",
            "--limit",
            "1",
        ],
    );
    assert_eq!(listed.json["data"]["items"][0]["assetId"], "moment_asset");
    assert_eq!(listed.json["data"]["schemaReady"], true);

    let missing_evidence = invoke(&catalog, ["moments", "get", "--asset", "moment_asset"]);
    assert!(!missing_evidence.status.success());
    assert!(missing_evidence.stdout.is_empty());
    let get_error: Value = serde_json::from_slice(&missing_evidence.stderr).unwrap();
    assert_eq!(get_error["command"], "moments.get");
    assert!(
        get_error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no moment evidence")
    );
    assert_eq!(
        fs::read(&catalog).unwrap(),
        before,
        "read-only commands changed catalog bytes"
    );
}

#[test]
fn moments_cli_rejects_implicit_or_invalid_scope_with_json_errors() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let project = create_project(&catalog, "Moment invalid scope CLI");

    let missing_scope = invoke(
        &catalog,
        [
            "moments",
            "plan",
            "--project",
            &project,
            "--budget-usd",
            "1",
        ],
    );
    assert!(!missing_scope.status.success());
    assert!(missing_scope.stdout.is_empty());
    let parse_error: Value = serde_json::from_slice(&missing_scope.stderr).unwrap();
    assert_eq!(parse_error["ok"], false);
    assert_eq!(parse_error["error"]["code"], "invalid_arguments");

    for omitted in ["--provider", "--model", "--payload"] {
        let mut args = vec![
            "moments",
            "plan",
            "--project",
            &project,
            "--asset",
            "missing_asset",
            "--provider",
            "openrouter",
            "--model",
            "qwen/qwen3.7-flash",
            "--payload",
            "frames",
            "--budget-usd",
            "1",
        ];
        let option_index = args.iter().position(|arg| *arg == omitted).unwrap();
        args.drain(option_index..=option_index + 1);
        let missing_selection = invoke(&catalog, args);
        assert!(
            !missing_selection.status.success(),
            "{omitted} must be explicit"
        );
        assert!(missing_selection.stdout.is_empty());
        let error: Value = serde_json::from_slice(&missing_selection.stderr).unwrap();
        assert_eq!(error["error"]["code"], "invalid_arguments");
    }

    let empty_usable = failure(
        &catalog,
        [
            "moments",
            "plan",
            "--project",
            &project,
            "--all-usable",
            "--provider",
            "openrouter",
            "--model",
            "qwen/qwen3.7-flash",
            "--payload",
            "frames",
            "--budget-usd",
            "1",
        ],
    );
    assert!(
        empty_usable["error"]["message"]
            .as_str()
            .unwrap()
            .contains("selected no non-removed assets")
    );

    let clip = directory.path().join("budget-fixture.mp4");
    insert_moment_scope_asset(&catalog, &project, "budget_asset", &clip);
    let invalid_budget = invoke(
        &catalog,
        [
            "moments",
            "plan",
            "--project",
            &project,
            "--asset",
            "budget_asset",
            "--provider",
            "openrouter",
            "--model",
            "qwen/qwen3.7-flash",
            "--payload",
            "frames",
            "--budget-usd",
            "NaN",
        ],
    );
    assert!(!invalid_budget.status.success());
    assert!(invalid_budget.stdout.is_empty());
    let error: Value = serde_json::from_slice(&invalid_budget.stderr).unwrap();
    assert_eq!(error["ok"], false);
    assert_eq!(error["command"], "moments.plan");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("finite and positive"),
        "unexpected budget error: {error}"
    );
}

#[test]
fn camera_proxy_cli_commands_are_discoverable_and_have_clear_empty_missing_results() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let project = create_project(&catalog, "Synthetic camera proxy CLI");
    let card = directory.path().join("empty-card");
    fs::create_dir(&card).unwrap();
    let discovered = success(
        &catalog,
        [
            "camera-proxy",
            "discover",
            "--project",
            &project,
            "--card",
            card.to_str().unwrap(),
        ],
    );
    assert_eq!(discovered.json["data"], serde_json::json!([]));
    let missing = failure(
        &catalog,
        ["camera-proxy", "status", "--asset", "missing_asset"],
    );
    assert_eq!(missing["error"]["code"], "not_found");
    let registration = failure(
        &catalog,
        [
            "camera-proxy",
            "register",
            "--asset",
            "missing_asset",
            "--card",
            card.to_str().unwrap(),
            "--sidecar",
            card.join("MISSING.LRF").to_str().unwrap(),
        ],
    );
    assert_eq!(registration["error"]["code"], "not_found");
}

#[test]
fn camera_proxy_cli_round_trips_a_generated_lrf_without_changing_media() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipping generated media CLI round trip: ffmpeg unavailable");
        return;
    }
    let directory = tempdir().unwrap();
    let catalog_path = directory.path().join("catalog.sqlite3");
    let project = create_project(&catalog_path, "Synthetic LRF CLI round trip");
    let card = directory.path().join("card-one");
    let dcim = card.join("DCIM");
    fs::create_dir_all(&dcim).unwrap();
    let original = dcim.join("CLIP0001.MP4");
    let sidecar = dcim.join("CLIP0001.LRF");
    for (path, dimensions, rate) in [(&original, "640x360", 60), (&sidecar, "320x180", 30)] {
        let output = Command::new("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("color=c=green:s={dimensions}:r={rate}:d=2"),
                "-f",
                "lavfi",
                "-i",
                "anullsrc=r=48000:cl=stereo",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
                "-f",
                "mp4",
            ])
            .arg(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let organized = directory.path().join("organized");
    fs::create_dir_all(&organized).unwrap();
    let primary = organized.join("organized-name.MP4");
    fs::copy(&original, &primary).unwrap();
    success(
        &catalog_path,
        [
            "ingest",
            "--project",
            &project,
            "--folder",
            organized.to_str().unwrap(),
        ],
    );
    let original_bytes = fs::read(&original).unwrap();
    let sidecar_bytes = fs::read(&sidecar).unwrap();
    success(
        &catalog_path,
        [
            "ingest",
            "--project",
            &project,
            "--folder",
            card.to_str().unwrap(),
        ],
    );
    let catalog = Catalog::open_existing(&catalog_path).unwrap();
    let asset_id = catalog.list_assets(&project).unwrap().remove(0).id;

    let found = success(
        &catalog_path,
        [
            "camera-proxy",
            "discover",
            "--project",
            &project,
            "--card",
            card.to_str().unwrap(),
        ],
    );
    assert_eq!(found.json["data"][0]["status"], "candidate");
    assert_eq!(found.json["data"][0]["assetId"], asset_id);
    let wrong_card = directory.path().join("wrong-card");
    fs::create_dir_all(wrong_card.join("DCIM")).unwrap();
    let wrong_sidecar = wrong_card.join("DCIM/CLIP0001.LRF");
    fs::copy(&original, wrong_card.join("DCIM/CLIP0001.MP4")).unwrap();
    fs::copy(&sidecar, &wrong_sidecar).unwrap();
    let wrong = failure(
        &catalog_path,
        [
            "camera-proxy",
            "register",
            "--asset",
            &asset_id,
            "--card",
            wrong_card.to_str().unwrap(),
            "--sidecar",
            wrong_sidecar.to_str().unwrap(),
        ],
    );
    assert_eq!(wrong["error"]["code"], "invalid_input");
    assert!(
        wrong["error"]["message"]
            .as_str()
            .unwrap()
            .contains("association_mismatch:")
    );
    let registered = success(
        &catalog_path,
        [
            "camera-proxy",
            "register",
            "--asset",
            &asset_id,
            "--card",
            card.to_str().unwrap(),
            "--sidecar",
            sidecar.to_str().unwrap(),
        ],
    );
    assert_eq!(registered.json["data"]["status"], "registered");
    assert_eq!(
        registered.json["data"]["provenance"]["timeMapping"]["frameReduction"],
        2
    );
    assert_eq!(
        registered.json["data"]["provenance"]["originalLocation"],
        original.to_string_lossy().as_ref()
    );
    assert_eq!(
        registered.json["data"]["provenance"]["originalRelativePath"],
        "DCIM/CLIP0001.MP4"
    );
    assert!(
        registered.json["data"]["fingerprint"]
            .as_str()
            .unwrap()
            .len()
            >= 64
    );
    let status = success(
        &catalog_path,
        ["camera-proxy", "status", "--asset", &asset_id],
    );
    assert_eq!(status.json["data"]["status"], "registered");
    assert_eq!(
        status.json["data"]["cardRoot"],
        card.canonicalize().unwrap().to_string_lossy().as_ref()
    );
    let asset = catalog.get_asset(&asset_id).unwrap();
    assert!(asset.proxy_path.is_none());
    assert_eq!(fs::read(&original).unwrap(), original_bytes);
    assert_eq!(fs::read(&sidecar).unwrap(), sidecar_bytes);
    assert_eq!(asset.path, primary.to_string_lossy());
    fs::rename(&primary, primary.with_extension("offline")).unwrap();
    fs::rename(&original, original.with_extension("offline")).unwrap();
    let frames = success(
        &catalog_path,
        ["camera-proxy", "frame-pack", "--asset", &asset_id],
    );
    assert_eq!(frames.json["command"], "camera-proxy.frame-pack");
    assert_eq!(
        frames.json["data"]["representation"],
        "camera.h264-lrf.verified-v1"
    );
    assert!(
        !frames.json["data"]["timestampsMs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        frames.json["data"]["sourceFingerprint"]
            .as_str()
            .unwrap()
            .len()
            >= 64
    );
    let stale = directory.path().join("stale.LRF");
    fs::rename(&sidecar, &stale).unwrap();
    let result = failure(
        &catalog_path,
        ["camera-proxy", "frame-pack", "--asset", &asset_id],
    );
    assert!(
        result["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unavailable")
    );
}

#[test]
fn chronology_preview_is_reusable_revision_guarded_and_process_safe() {
    let directory = tempdir().expect("temporary catalog directory");
    let catalog = directory.path().join("catalog.sqlite3");
    let project = create_project(&catalog, "synthetic chronology");
    let request_path = directory.path().join("request.json");
    let sequence_request = serde_json::json!({
        "kind": "sequenceDeclare", "cameraKey": "Synthetic Camera", "label": "Card A",
        "volumeHint": "CARD-A", "filenamePrefix": "SYN_"
    });
    let sequence = chronology_apply(&catalog, &project, sequence_request);
    let sequence = sequence["data"]["data"]["sequence"]["id"]
        .as_str()
        .expect("sequence identity")
        .to_owned();

    let epoch_request = serde_json::json!({
        "kind": "epochSet", "epoch": {"id":"", "captureSequenceId":sequence,
        "startCounter":1,"endCounter":null,"offsetSeconds":3600,"confidence":0.9,
        "evidence":["synthetic review"],"label":"Synthetic clock correction"}
    });
    let output = invoke_with_stdin(
        &catalog,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            "-",
        ],
        &epoch_request.to_string(),
    );
    assert!(
        output.status.success(),
        "preview failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let preview: Value = serde_json::from_slice(&output.stdout).expect("preview JSON envelope");
    assert_eq!(preview["data"]["kind"], "epochSet");
    assert!(preview["data"]["data"]["factsToken"].as_str().is_some());
    assert_eq!(
        preview["data"]["data"]["epoch"]["captureSequenceId"],
        sequence
    );

    fs::write(
        &request_path,
        serde_json::json!({
            "kind":"merge", "fromSequenceId":"missing-sequence", "intoSequenceId":sequence
        })
        .to_string(),
    )
    .expect("write invalid reference request");
    let invalid = failure(
        &catalog,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            request_path.to_str().unwrap(),
        ],
    );
    assert_eq!(invalid["error"]["code"], "chronology.invalid_reference");

    fs::write(
        &request_path,
        serde_json::json!({"kind":"epochSet", "preview":preview["data"]["data"]}).to_string(),
    )
    .expect("write apply request");
    let applied = success(
        &catalog,
        [
            "chronology",
            "apply",
            "--project",
            &project,
            "--input",
            request_path.to_str().unwrap(),
        ],
    );
    assert_eq!(applied.json["data"]["data"]["offsetSeconds"], 3600);
    let stale = failure(
        &catalog,
        [
            "chronology",
            "apply",
            "--project",
            &project,
            "--input",
            request_path.to_str().unwrap(),
        ],
    );
    assert_eq!(stale["error"]["code"], "chronology.stale_preview");

    let epoch_id = preview["data"]["data"]["epoch"]["id"]
        .as_str()
        .expect("epoch identity");
    let clear_request = serde_json::json!({"kind":"epochClear", "epochId":epoch_id});
    let clear_preview_output = invoke_with_stdin(
        &catalog,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            "-",
        ],
        &clear_request.to_string(),
    );
    assert!(
        clear_preview_output.status.success(),
        "clear preview failed: {}",
        String::from_utf8_lossy(&clear_preview_output.stderr)
    );
    let clear_preview: Value =
        serde_json::from_slice(&clear_preview_output.stdout).expect("epoch clear preview JSON");
    assert_eq!(clear_preview["data"]["kind"], "epochClear");
    assert_eq!(clear_preview["data"]["data"]["epochId"], epoch_id);
    assert!(
        clear_preview["data"]["data"]["factsToken"]
            .as_str()
            .is_some()
    );
    fs::write(
        &request_path,
        serde_json::json!({
            "kind":"epochClear", "preview":clear_preview["data"]["data"]
        })
        .to_string(),
    )
    .expect("write epoch clear apply request");

    // Another chronology change invalidates the saved clear preview. The
    // adapter returns the same machine-readable stale code used by other kinds.
    chronology_apply(
        &catalog,
        &project,
        serde_json::json!({
            "kind":"sequenceDeclare", "cameraKey":"Synthetic Camera",
            "label":"Unrelated card", "volumeHint":"CARD-B", "filenamePrefix":"SYN_"
        }),
    );
    let stale_clear = failure(
        &catalog,
        [
            "chronology",
            "apply",
            "--project",
            &project,
            "--input",
            request_path.to_str().unwrap(),
        ],
    );
    assert_eq!(stale_clear["error"]["code"], "chronology.stale_preview");

    let refreshed_clear_preview = invoke_with_stdin(
        &catalog,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            "-",
        ],
        &clear_request.to_string(),
    );
    assert!(
        refreshed_clear_preview.status.success(),
        "refreshed clear preview failed: {}",
        String::from_utf8_lossy(&refreshed_clear_preview.stderr)
    );
    let refreshed_clear_preview: Value =
        serde_json::from_slice(&refreshed_clear_preview.stdout).unwrap();
    fs::write(
        &request_path,
        serde_json::json!({
            "kind":"epochClear", "preview":refreshed_clear_preview["data"]["data"]
        })
        .to_string(),
    )
    .expect("write refreshed epoch clear apply request");
    let cleared = success(
        &catalog,
        [
            "chronology",
            "apply",
            "--project",
            &project,
            "--input",
            request_path.to_str().unwrap(),
        ],
    );
    assert_eq!(cleared.json["data"]["data"]["applied"], true);
    let retry = failure(
        &catalog,
        [
            "chronology",
            "apply",
            "--project",
            &project,
            "--input",
            request_path.to_str().unwrap(),
        ],
    );
    assert_eq!(retry["error"]["code"], "chronology.invalid_reference");
}

#[test]
fn chronology_clock_question_cli_previews_applies_and_keeps_human_decision_visible() {
    let directory = tempdir().expect("temporary catalog directory");
    let catalog_path = directory.path().join("catalog.sqlite3");
    let project = create_project(&catalog_path, "synthetic clock question");
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
    let question = serde_json::json!({
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
    let preview_output = invoke_with_stdin(
        &catalog_path,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            "-",
        ],
        &serde_json::json!({"kind":"questionPropose","question":question}).to_string(),
    );
    assert!(
        preview_output.status.success(),
        "{}",
        String::from_utf8_lossy(&preview_output.stderr)
    );
    let preview: Value = serde_json::from_slice(&preview_output.stdout).unwrap();
    assert_eq!(
        preview["data"]["data"]["alternativeImpacts"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let proposal_apply = invoke_with_stdin(
        &catalog_path,
        ["chronology", "apply", "--project", &project, "--input", "-"],
        &serde_json::json!({"kind":"questionPropose","preview":preview["data"]["data"]})
            .to_string(),
    );
    assert!(
        proposal_apply.status.success(),
        "{}",
        String::from_utf8_lossy(&proposal_apply.stderr)
    );
    let listing = success(
        &catalog_path,
        ["chronology", "questions", "--project", &project],
    )
    .json;
    assert_eq!(
        listing["data"]["openQuestions"].as_array().unwrap().len(),
        1
    );

    let decision_preview = invoke_with_stdin(
        &catalog_path,
        ["chronology", "preview", "--project", &project, "--input", "-"],
        &serde_json::json!({"kind":"questionDecision","decision":{"questionId":"synthetic-question","kind":"accepted","alternativeId":"date-b","note":"synthetic owner anchor"}}).to_string(),
    );
    assert!(
        decision_preview.status.success(),
        "{}",
        String::from_utf8_lossy(&decision_preview.stderr)
    );
    let decision_preview: Value = serde_json::from_slice(&decision_preview.stdout).unwrap();
    let mut rejected_with_epoch = decision_preview["data"]["data"].clone();
    rejected_with_epoch["decision"]["kind"] = Value::String("rejected".into());
    rejected_with_epoch["decision"]["alternativeId"] = Value::Null;
    let rejected_attempt = invoke_with_stdin(
        &catalog_path,
        ["chronology", "apply", "--project", &project, "--input", "-"],
        &serde_json::json!({"kind":"questionDecision","preview":rejected_with_epoch}).to_string(),
    );
    assert!(!rejected_attempt.status.success());
    assert!(rejected_attempt.stdout.is_empty());
    let rejected_error: Value = serde_json::from_slice(&rejected_attempt.stderr).unwrap();
    assert_eq!(
        rejected_error["error"]["code"],
        "chronology.invalid_reference"
    );
    assert!(
        rejected_error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("rejected decision must not include a clock correction preview")
    );

    let mut mismatched_epoch = decision_preview["data"]["data"].clone();
    mismatched_epoch["clockEpochPreview"]["epoch"]["offsetSeconds"] =
        Value::Number((86_401_i64).into());
    let mismatch_attempt = invoke_with_stdin(
        &catalog_path,
        ["chronology", "apply", "--project", &project, "--input", "-"],
        &serde_json::json!({"kind":"questionDecision","preview":mismatched_epoch}).to_string(),
    );
    assert!(!mismatch_attempt.status.success());
    assert!(mismatch_attempt.stdout.is_empty());
    let mismatch_error: Value = serde_json::from_slice(&mismatch_attempt.stderr).unwrap();
    assert_eq!(
        mismatch_error["error"]["code"],
        "chronology.invalid_reference"
    );
    assert!(
        mismatch_error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("clock correction preview does not match the selected alternative")
    );
    let still_open = success(
        &catalog_path,
        ["chronology", "questions", "--project", &project],
    );
    assert_eq!(
        still_open.json["data"]["openQuestions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        still_open.json["data"]["humanDecisions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let epochs_after_failed_applies = success(
        &catalog_path,
        ["chronology", "epochs", "--project", &project],
    );
    assert!(
        epochs_after_failed_applies.json["data"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let applied = invoke_with_stdin(
        &catalog_path,
        ["chronology", "apply", "--project", &project, "--input", "-"],
        &serde_json::json!({"kind":"questionDecision","preview":decision_preview["data"]["data"]})
            .to_string(),
    );
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let listing = success(
        &catalog_path,
        ["chronology", "questions", "--project", &project],
    )
    .json;
    assert!(
        listing["data"]["openQuestions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        listing["data"]["humanDecisions"][0]["decision"]["kind"],
        "accepted"
    );
    assert_eq!(listing["data"]["humanDecisions"][0]["proposalRevision"], 1);
}

#[test]
fn chronology_direct_preview_apply_can_be_retried_after_cli_restart() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let project = create_project(&catalog, "synthetic retry");
    let request_path = directory.path().join("sequence-request.json");
    fs::write(
        &request_path,
        serde_json::json!({"kind":"sequenceDeclare", "cameraKey":"Synthetic", "label":"Card", "volumeHint":"CARD", "filenamePrefix":"SYN_"}).to_string(),
    ).unwrap();
    let preview = success(
        &catalog,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            request_path.to_str().unwrap(),
        ],
    )
    .json;
    let apply_path = directory.path().join("sequence-apply.json");
    fs::write(
        &apply_path,
        serde_json::json!({"kind":"sequenceDeclare", "preview":preview["data"]["data"]})
            .to_string(),
    )
    .unwrap();
    success(
        &catalog,
        [
            "chronology",
            "apply",
            "--project",
            &project,
            "--input",
            apply_path.to_str().unwrap(),
        ],
    );
    // A second process receives the same candidate after the caller lost its
    // response. The core recognizes the durable sequence and does not append a duplicate.
    success(
        &catalog,
        [
            "chronology",
            "apply",
            "--project",
            &project,
            "--input",
            apply_path.to_str().unwrap(),
        ],
    );
    let sequences = success(&catalog, ["chronology", "sequences", "--project", &project]);
    assert_eq!(
        sequences.json["data"]["sequences"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn chronology_exact_split_never_expands_after_a_later_import() {
    let directory = tempdir().expect("temporary catalog directory");
    let catalog = directory.path().join("catalog.sqlite3");
    let project = create_project(&catalog, "synthetic exact split");
    // Declare through the CLI so the sequence itself is an ordinary typed operation.
    let sequence_request = serde_json::json!({
        "kind":"sequenceDeclare", "cameraKey":"Synthetic Camera", "label":"Card",
        "volumeHint":"CARD", "filenamePrefix":"SYN_"
    });
    let sequence = chronology_apply(&catalog, &project, sequence_request);
    let sequence_id = sequence["data"]["data"]["sequence"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // These are synthetic rows in the temporary test catalog. The sequence is
    // created by the domain API; rows stand in for three later imported clips.
    let connection = Connection::open(&catalog).unwrap();
    for (asset_id, counter, captured) in [
        ("clip_one", 1_i64, "2026-01-01T00:00:00Z"),
        ("clip_two", 2, "2026-01-01T00:01:00Z"),
        ("clip_later", 2, "2026-01-01T00:02:00Z"),
    ] {
        connection.execute(
            "INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, notes, labels_json, removed, rotation_degrees, created_at) VALUES (?1, ?2, ?3, ?4, ?5, 100, 1, 30, 1920, 1080, 'h264', 'Synthetic Camera', ?6, NULL, '', '[]', 0, 0, '2026-01-01T00:00:00Z')",
            rusqlite::params![asset_id, project, format!("fingerprint-{asset_id}"), format!("/synthetic/{asset_id}.mp4"), format!("{asset_id}.mp4"), captured],
        ).unwrap();
        connection.execute(
            "INSERT INTO asset_capture_sequence(asset_id, project_id, capture_sequence_id, filename_counter) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![asset_id, project, sequence_id, counter],
        ).unwrap();
    }
    drop(connection);

    let preview_request = serde_json::json!({
        "kind":"split", "sourceSequenceId":sequence_id, "atCounter":2,
        "label":"Card B", "assetIds":["clip_two"]
    });
    let preview = invoke_with_stdin(
        &catalog,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            "-",
        ],
        &preview_request.to_string(),
    );
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let preview: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(
        preview["data"]["data"]["assetIds"],
        serde_json::json!(["clip_two"])
    );

    let later_connection = Connection::open(&catalog).unwrap();
    later_connection.execute(
        "INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, notes, labels_json, removed, rotation_degrees, created_at) VALUES ('clip_after_preview', ?1, 'fingerprint-later', '/synthetic/later.mp4', 'later.mp4', 100, 1, 30, 1920, 1080, 'h264', 'Synthetic Camera', NULL, NULL, '', '[]', 0, 0, '2026-01-01T00:00:00Z')",
        [&project],
    ).unwrap();
    later_connection.execute(
        "INSERT INTO asset_capture_sequence(asset_id, project_id, capture_sequence_id, filename_counter) VALUES ('clip_after_preview', ?1, ?2, 2)",
        rusqlite::params![project, sequence_id],
    ).unwrap();
    drop(later_connection);

    let stale_request = serde_json::json!({"kind":"split", "preview":preview["data"]["data"]});
    let stale = invoke_with_stdin(
        &catalog,
        ["chronology", "apply", "--project", &project, "--input", "-"],
        &stale_request.to_string(),
    );
    assert!(!stale.status.success());
    let stale: Value = serde_json::from_slice(&stale.stderr).unwrap();
    assert_eq!(stale["error"]["code"], "chronology.stale_preview");

    let refreshed = invoke_with_stdin(
        &catalog,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            "-",
        ],
        &preview_request.to_string(),
    );
    let refreshed: Value = serde_json::from_slice(&refreshed.stdout).unwrap();
    assert_eq!(
        refreshed["data"]["data"]["assetIds"],
        serde_json::json!(["clip_two"])
    );
    let apply = invoke_with_stdin(
        &catalog,
        ["chronology", "apply", "--project", &project, "--input", "-"],
        &serde_json::json!({"kind":"split", "preview":refreshed["data"]["data"]}).to_string(),
    );
    assert!(
        apply.status.success(),
        "{}",
        String::from_utf8_lossy(&apply.stderr)
    );
    let assignments = Connection::open(&catalog).unwrap();
    let moved: String = assignments
        .query_row(
            "SELECT capture_sequence_id FROM asset_capture_sequence WHERE asset_id='clip_two'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(moved, sequence_id);
    for asset_id in ["clip_later", "clip_after_preview"] {
        let retained: String = assignments
            .query_row(
                "SELECT capture_sequence_id FROM asset_capture_sequence WHERE asset_id=?1",
                [asset_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(retained, sequence_id);
    }

    let base_phase = chronology_apply(
        &catalog,
        &project,
        serde_json::json!({"kind":"phaseSet", "phase":{"id":"", "label":"Synthetic trip", "kind":"location", "startAnchorAssetId":null, "endAnchorAssetId":null, "startTripTime":"2026-01-01T00:00:00Z", "endTripTime":"2026-01-01T00:05:00Z", "timezone":"Etc/UTC", "originTimezone":null, "destinationTimezone":null, "displayMode":"local"}}),
    );
    let base_phase_id = base_phase["data"]["data"]["phase"]["id"].as_str().unwrap();
    let phase_split_request = serde_json::json!({"kind":"phaseSplit", "existingPhaseId":base_phase_id, "boundaryAssetId":"clip_two", "direction":"through", "phase":{"id":"", "label":"Synthetic first leg", "kind":"transit", "startAnchorAssetId":null, "endAnchorAssetId":null, "startTripTime":"2026-01-01T00:00:00Z", "endTripTime":"2026-01-01T00:01:00Z", "timezone":"Etc/UTC", "originTimezone":null, "destinationTimezone":null, "displayMode":"local"}});
    let phase_preview_output = invoke_with_stdin(
        &catalog,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            "-",
        ],
        &phase_split_request.to_string(),
    );
    assert!(
        phase_preview_output.status.success(),
        "{}",
        String::from_utf8_lossy(&phase_preview_output.stderr)
    );
    let phase_preview: Value = serde_json::from_slice(&phase_preview_output.stdout).unwrap();
    assert_eq!(
        phase_preview["data"]["data"]["affectedAssetIds"],
        serde_json::json!(["clip_later", "clip_one", "clip_two"])
    );
    let phase_split_apply = invoke_with_stdin(
        &catalog,
        ["chronology", "apply", "--project", &project, "--input", "-"],
        &serde_json::json!({"kind":"phaseSplit", "preview":phase_preview["data"]["data"]})
            .to_string(),
    );
    assert!(
        phase_split_apply.status.success(),
        "{}",
        String::from_utf8_lossy(&phase_split_apply.stderr)
    );

    chronology_apply(
        &catalog,
        &project,
        serde_json::json!({"kind":"sameCard", "sequenceId":sequence_id, "atCounter":2, "reason":"synthetic confirmation"}),
    );
    chronology_apply(
        &catalog,
        &project,
        serde_json::json!({"kind":"sameCardRestore", "sequenceId":sequence_id, "atCounter":2}),
    );
    chronology_apply(
        &catalog,
        &project,
        serde_json::json!({"kind":"exceptionSet", "assetId":"clip_one", "tripTime":"2026-01-01T01:00:00Z", "reason":"synthetic correction"}),
    );
    chronology_apply(
        &catalog,
        &project,
        serde_json::json!({"kind":"exceptionClear", "assetId":"clip_one"}),
    );
    let phase = chronology_apply(
        &catalog,
        &project,
        serde_json::json!({
            "kind":"phaseSet", "phase":{"id":"", "label":"Synthetic location", "kind":"location",
            "startAnchorAssetId":null,"endAnchorAssetId":null,
            "startTripTime":"2026-01-01T00:00:00Z","endTripTime":"2026-01-02T00:00:00Z",
            "timezone":"Etc/UTC","originTimezone":null,"destinationTimezone":null,"displayMode":"local"}
        }),
    );
    chronology_apply(
        &catalog,
        &project,
        serde_json::json!({"kind":"phaseRemove", "phaseId":phase["data"]["data"]["phase"]["id"]}),
    );
    let constraint = chronology_apply(
        &catalog,
        &project,
        serde_json::json!({"kind":"constraintSet", "earlierAssetId":"clip_one", "laterAssetId":"clip_two", "strength":"soft", "source":"synthetic test"}),
    );
    chronology_apply(
        &catalog,
        &project,
        serde_json::json!({"kind":"constraintClear", "constraintId":constraint["data"]["data"]["constraint"]["id"]}),
    );
}

#[test]
fn chronology_reassignment_and_merge_use_reusable_exact_previews() {
    let directory = tempdir().expect("temporary catalog directory");
    let catalog = directory.path().join("catalog.sqlite3");
    let project = create_project(&catalog, "synthetic sequence move");
    let declare = |label: &str| {
        chronology_apply(
            &catalog,
            &project,
            serde_json::json!({
                "kind":"sequenceDeclare", "cameraKey":"Synthetic Camera", "label":label,
                "volumeHint":label, "filenamePrefix":"SYN_"
            }),
        )["data"]["data"]["sequence"]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let original = declare("Original card");
    let destination = declare("Destination card");
    let connection = Connection::open(&catalog).unwrap();
    connection.execute(
        "INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, notes, labels_json, removed, rotation_degrees, created_at) VALUES ('clip_selected', ?1, 'fingerprint-selected', '/synthetic/selected.mp4', 'selected.mp4', 100, 1, 30, 1920, 1080, 'h264', 'Synthetic Camera', '2026-01-01T00:00:00Z', NULL, '', '[]', 0, 0, '2026-01-01T00:00:00Z')",
        [&project],
    ).unwrap();
    connection.execute(
        "INSERT INTO asset_capture_sequence(asset_id, project_id, capture_sequence_id, filename_counter) VALUES ('clip_selected', ?1, ?2, 1)",
        rusqlite::params![project, original],
    ).unwrap();
    drop(connection);

    let preview = invoke_with_stdin(
        &catalog,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            "-",
        ],
        &serde_json::json!({
            "kind":"reassign", "fromSequenceId":original,
            "intoSequenceId":destination, "assetIds":["clip_selected"]
        })
        .to_string(),
    );
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let preview: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(
        preview["data"]["data"]["assetIds"],
        serde_json::json!(["clip_selected"])
    );
    assert!(preview["data"]["data"]["factsToken"].as_str().is_some());
    let apply = invoke_with_stdin(
        &catalog,
        ["chronology", "apply", "--project", &project, "--input", "-"],
        &serde_json::json!({"kind":"reassign", "preview":preview["data"]["data"]}).to_string(),
    );
    assert!(
        apply.status.success(),
        "{}",
        String::from_utf8_lossy(&apply.stderr)
    );

    let merge_preview = invoke_with_stdin(
        &catalog,
        [
            "chronology",
            "preview",
            "--project",
            &project,
            "--input",
            "-",
        ],
        &serde_json::json!({
            "kind":"merge", "fromSequenceId":destination,
            "intoSequenceId":original
        })
        .to_string(),
    );
    assert!(
        merge_preview.status.success(),
        "{}",
        String::from_utf8_lossy(&merge_preview.stderr)
    );
    let merge_preview: Value = serde_json::from_slice(&merge_preview.stdout).unwrap();
    assert!(
        merge_preview["data"]["data"]["factsToken"]
            .as_str()
            .is_some()
    );
    let merge = invoke_with_stdin(
        &catalog,
        ["chronology", "apply", "--project", &project, "--input", "-"],
        &serde_json::json!({"kind":"merge", "preview":merge_preview["data"]["data"]}).to_string(),
    );
    assert!(
        merge.status.success(),
        "{}",
        String::from_utf8_lossy(&merge.stderr)
    );
    let connection = Connection::open(&catalog).unwrap();
    let assigned: String = connection
        .query_row(
            "SELECT capture_sequence_id FROM asset_capture_sequence WHERE asset_id='clip_selected'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(assigned, original);
}

fn available_tool(doctor: &Value, name: &str) -> Option<PathBuf> {
    doctor["data"]["tools"]
        .as_array()?
        .iter()
        .find(|tool| tool["name"] == name && tool["available"] == true)
        .and_then(|tool| tool["path"].as_str())
        .map(PathBuf::from)
}

#[test]
fn chronology_reads_schema_25_without_migration_and_reports_revision_drift_stably() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("schema-25.sqlite");
    let project_id = create_project(&catalog, "schema-25 chronology");
    {
        let connection = Connection::open(&catalog).unwrap();
        connection
            .execute(
                "INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, camera, created_at) VALUES ('asset_old', ?1, 'fingerprint', '', 'clip_0001.mp4', 0, 1.0, 'Camera', '2025-12-01T00:00:00Z')",
                [&project_id],
            )
            .unwrap();
        // Reconstruct the published schema-25 boundary: migrations 26 through
        // 30 were not applied, so their tables, columns, and history are absent.
        connection
            .execute_batch(
                "ALTER TABLE asset_analyses DROP COLUMN attempted_models_json;
             ALTER TABLE asset_analyses DROP COLUMN usage_complete;
             DROP TABLE analysis_chronology_suggestions;
             DROP TABLE camera_proxy_sidecars;
             ALTER TABLE assets DROP COLUMN timestamp_candidates_json;
             DROP TABLE asset_video_validation;
             DELETE FROM schema_migrations WHERE migration_id > 25;",
            )
            .unwrap();
    }

    let inspection = success(&catalog, ["catalog", "inspect"]);
    assert_eq!(inspection.json["data"]["schemaStatus"], "upgrade_available");
    let before_reads = fs::read(&catalog).unwrap();

    let status = success(&catalog, ["chronology", "status", "--project", &project_id]);
    assert_eq!(status.json["data"]["counts"]["assets"], 1);
    assert_eq!(status.json["data"]["orderedAssetIds"][0], "asset_old");

    let sequences = success(
        &catalog,
        ["chronology", "sequences", "--project", &project_id],
    );
    assert!(sequences.json["data"]["proposedSplits"].is_array());
    success(&catalog, ["chronology", "epochs", "--project", &project_id]);
    let evidence = success(
        &catalog,
        ["chronology", "evidence", "--project", &project_id],
    );
    assert_eq!(
        evidence.json["data"]["assets"][0]["asset"]["assetId"],
        "asset_old"
    );
    assert!(evidence.json["data"]["assets"][0]["timestampCandidates"].is_null());

    let questions = success(
        &catalog,
        ["chronology", "questions", "--project", &project_id],
    );
    assert!(questions.json["data"]["openQuestions"].is_array());
    assert!(questions.json["data"]["humanDecisions"].is_array());
    assert!(
        questions.json["data"]["openQuestions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        questions.json["data"]["humanDecisions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(fs::read(&catalog).unwrap(), before_reads);

    let stale = invoke(
        &catalog,
        [
            "chronology",
            "evidence",
            "--project",
            &project_id,
            "--facts-revision",
            "stale-revision",
        ],
    );
    assert!(!stale.status.success());
    assert!(stale.stdout.is_empty());
    let stderr: Value = serde_json::from_slice(&stale.stderr).unwrap();
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["command"], "chronology.evidence");
    assert_eq!(stderr["error"]["code"], "chronology.facts_changed");
    assert!(
        stderr["error"]["message"]
            .as_str()
            .unwrap()
            .contains("chronology.facts_changed")
    );
    assert_eq!(fs::read(&catalog).unwrap(), before_reads);
}

fn create_media_fixture(ffmpeg: &Path, output: &Path) {
    let status = Command::new(ffmpeg)
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=navy:s=640x360:r=30:d=1",
            "-metadata",
            "creation_time=2025-08-10T09:30:00+09:00",
            "-c:v",
            "mpeg4",
        ])
        .arg(output)
        .status()
        .expect("create fixture with ffmpeg");
    assert!(status.success(), "ffmpeg failed to create media fixture");
}

fn create_audio_only_fixture(ffmpeg: &Path, output: &Path) {
    let status = Command::new(ffmpeg)
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-vn",
            "-c:a",
            "aac",
        ])
        .arg(output)
        .status()
        .expect("create audio-only fixture");
    assert!(status.success());
}

fn create_zero_frame_fixture(ffmpeg: &Path, output: &Path) {
    let status = Command::new(ffmpeg)
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=64x64:r=1",
            "-frames:v",
            "0",
            "-c:v",
            "mpeg4",
        ])
        .arg(output)
        .status()
        .expect("create zero-frame fixture");
    assert!(
        status.success(),
        "ffmpeg failed to create zero-frame fixture"
    );
}

#[test]
fn ingest_quarantines_reachable_files_without_video_and_preserves_originals() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let doctor = success(&catalog, ["doctor"]);
    let Some(ffmpeg) = available_tool(&doctor.json, "ffmpeg") else {
        return;
    };
    if available_tool(&doctor.json, "ffprobe").is_none() {
        return;
    }
    let source_dir = directory.path().join("synthetic");
    fs::create_dir_all(&source_dir).unwrap();
    let audio = source_dir.join("audio-only.mp4");
    let zero = source_dir.join("zero-frame.mp4");
    create_audio_only_fixture(&ffmpeg, &audio);
    create_zero_frame_fixture(&ffmpeg, &zero);
    let audio_bytes = fs::read(&audio).unwrap();
    let zero_bytes = fs::read(&zero).unwrap();
    let project = create_project(&catalog, "Synthetic ingest");
    let ingest = success(
        &catalog,
        [
            "ingest",
            "--project",
            &project,
            "--folder",
            source_dir.to_str().unwrap(),
        ],
    );
    assert_eq!(ingest.json["data"]["imported"], 0);
    assert_eq!(ingest.json["data"]["failures"].as_array().unwrap().len(), 2);
    assert_eq!(fs::read(&audio).unwrap(), audio_bytes);
    assert_eq!(fs::read(&zero).unwrap(), zero_bytes);
    let clips = success(&catalog, ["clips", "list", "--project", &project]);
    assert_eq!(clips.json["data"]["total"], 0);
    let diagnostics = success(&catalog, ["diagnostics", "list", "--project", &project]);
    let items = diagnostics.json["data"]["diagnostics"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .all(|diagnostic| diagnostic["kind"] == "ingest.no_video_stream")
    );
    assert!(
        items
            .iter()
            .all(|diagnostic| diagnostic["context"]["usableVideo"] == false)
    );
    assert!(
        items
            .iter()
            .all(|diagnostic| diagnostic["context"]["reason"].as_str().is_some())
    );
}

#[test]
fn generic_unreadable_media_diagnostics_keep_each_filename_and_reason() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let doctor = success(&catalog, ["doctor"]);
    if available_tool(&doctor.json, "ffprobe").is_none() {
        return;
    }
    let source_dir = directory.path().join("synthetic");
    fs::create_dir_all(&source_dir).unwrap();
    let first = source_dir.join("broken-one.mp4");
    let second = source_dir.join("broken-two.mp4");
    fs::write(&first, b"synthetic invalid media one").unwrap();
    fs::write(&second, b"synthetic invalid media two").unwrap();
    let project = create_project(&catalog, "Synthetic unreadable media");

    let ingest = success(
        &catalog,
        [
            "ingest",
            "--project",
            &project,
            "--folder",
            source_dir.to_str().unwrap(),
        ],
    );
    assert_eq!(ingest.json["data"]["imported"], 0);
    let diagnostics = success(&catalog, ["diagnostics", "list", "--project", &project]);
    let items = diagnostics.json["data"]["diagnostics"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    let files = items
        .iter()
        .map(|item| item["context"]["filename"].as_str().unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        files,
        ["broken-one.mp4", "broken-two.mp4"].into_iter().collect()
    );
    assert!(
        items
            .iter()
            .all(|item| item["kind"] == "ingest.unreadable_media")
    );
    assert!(items.iter().all(|item| {
        item["context"]["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty())
    }));
}

#[cfg(unix)]
#[test]
fn legacy_asset_with_incomplete_video_stream_is_not_reported_usable() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let doctor = success(&catalog, ["doctor"]);
    let Some(ffmpeg) = available_tool(&doctor.json, "ffmpeg") else {
        return;
    };
    if available_tool(&doctor.json, "ffprobe").is_none() {
        return;
    }
    let media = directory.path().join("legacy.mp4");
    create_media_fixture(&ffmpeg, &media);
    let original_bytes = fs::read(&media).unwrap();
    let source_dir = directory.path().join("legacy-source");
    fs::create_dir_all(&source_dir).unwrap();
    let source = source_dir.join("legacy.mp4");
    fs::copy(&media, &source).unwrap();
    let project = create_project(&catalog, "Synthetic legacy row");
    let ingest = success(
        &catalog,
        [
            "ingest",
            "--project",
            &project,
            "--folder",
            source_dir.to_str().unwrap(),
        ],
    );
    let listed = success(&catalog, ["clips", "list", "--project", &project]);
    let asset_id = listed.json["data"]["items"][0]["id"].as_str().unwrap();
    assert_eq!(ingest.json["data"]["imported"], 1);
    Connection::open(&catalog)
        .unwrap()
        .execute(
            "DELETE FROM asset_video_validation WHERE asset_id = ?1",
            [asset_id],
        )
        .unwrap();

    let shim_dir = directory.path().join("shim");
    fs::create_dir_all(&shim_dir).unwrap();
    let shim = shim_dir.join("ffprobe");
    fs::write(&shim, "#!/bin/sh\nprintf '%s\\n' '{\"streams\":[{\"codec_type\":\"video\",\"width\":640,\"height\":360,\"codec_name\":\"mpeg4\"}],\"format\":{\"duration\":\"1.0\"}}'\n").unwrap();
    let mut permissions = fs::metadata(&shim).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&shim, permissions).unwrap();
    let output = invoke_with_path(
        &catalog,
        ["clips", "diagnose", "--asset", asset_id],
        shim_dir.as_os_str(),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let diagnosis: Value = serde_json::from_slice(&output.stdout).unwrap();
    let data = &diagnosis["data"];
    assert_eq!(
        data["representations"]["originalReadiness"]["bytesReachable"],
        true
    );
    assert_eq!(
        data["representations"]["originalReadiness"]["usableVideo"],
        false
    );
    assert_eq!(
        data["representations"]["originalReadiness"]["diagnosis"],
        "incomplete_video_stream"
    );
    assert_eq!(
        data["representations"]["catalogVideoValidation"]["status"],
        "unverified"
    );
    assert!(
        data["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("not usable video"))
    );
    assert!(
        data["recommendedActions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action.as_str().unwrap().contains("valid dimensions"))
    );

    let unverified_status = success(&catalog, ["project", "status", "--project", &project]);
    assert_eq!(
        unverified_status.json["data"]["analysisReadiness"]["totalClips"],
        0
    );
    assert_eq!(
        unverified_status.json["data"]["analysisReadiness"]["unverifiedVideoClips"],
        1
    );
    assert_eq!(
        unverified_status.json["data"]["analysisReadiness"]["reachableOriginals"],
        1
    );

    let operations_before = Connection::open(&catalog)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM project_operations", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    let validation = invoke_with_path(
        &catalog,
        ["clips", "validate-video", "--asset", asset_id],
        shim_dir.as_os_str(),
    );
    assert!(
        validation.status.success(),
        "{}",
        String::from_utf8_lossy(&validation.stderr)
    );
    let validation: Value = serde_json::from_slice(&validation.stdout).unwrap();
    assert_eq!(validation["data"]["status"], "quarantined");
    assert_eq!(validation["data"]["bytesReachable"], true);
    assert!(
        validation["data"]["diagnostic"]
            .as_str()
            .unwrap()
            .contains("frame rate")
    );
    let operations_after = Connection::open(&catalog)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM project_operations", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(operations_after, operations_before);

    let status = success(&catalog, ["project", "status", "--project", &project]);
    assert_eq!(status.json["data"]["media"]["assets"], 1);
    assert_eq!(status.json["data"]["media"]["durationSeconds"], 0.0);
    assert_eq!(
        status.json["data"]["media"]["videoValidation"]["usableVideo"],
        0
    );
    assert_eq!(
        status.json["data"]["media"]["videoValidation"]["quarantinedVideo"],
        1
    );
    assert_eq!(
        status.json["data"]["media"]["videoValidation"]["reachableOriginals"],
        1
    );
    assert_eq!(status.json["data"]["analysisReadiness"]["totalClips"], 0);
    assert_eq!(
        status.json["data"]["analysisReadiness"]["quarantinedVideoClips"],
        1
    );
    assert_eq!(
        status.json["data"]["analysisReadiness"]["reachableOriginals"],
        1
    );
    assert_eq!(fs::read(&source).unwrap(), original_bytes);
}

#[test]
fn process_contract_persists_across_invocations_and_routes_errors() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");

    let doctor = success(&catalog, ["doctor"]);
    assert_eq!(doctor.json["command"], "doctor");
    assert!(!catalog.exists(), "doctor must not mutate the catalog");

    let project_id = create_project(&catalog, "Process contract");
    let inspection = success(&catalog, ["catalog", "inspect"]);
    assert_eq!(
        inspection.json["data"]["currentSchemaVersion"],
        PROJECT_CATALOG_SCHEMA_VERSION
    );
    assert_eq!(inspection.json["data"]["schemaStatus"], "current");
    assert!(
        inspection.json["data"]["pendingMigrations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let list = success(&catalog, ["project", "list"]);
    assert_eq!(list.json["data"].as_array().unwrap().len(), 1);
    assert_eq!(list.json["data"][0]["id"], project_id);

    let status = success(&catalog, ["project", "status", "--project", &project_id]);
    assert_eq!(status.json["data"]["media"]["assets"], 0);

    let missing = failure(&catalog, ["clips", "get", "--asset", "asset_missing"]);
    assert_eq!(missing["command"], "clips.get");
    assert_eq!(missing["error"]["code"], "not_found");
}

#[test]
fn process_media_writers_return_stable_busy_code_without_running_tools() {
    let directory = tempdir().unwrap();
    let catalog_path = directory.path().join("catalog.sqlite3");
    let project_id = create_project(&catalog_path, "Busy media writer");
    Connection::open(&catalog_path)
        .unwrap()
        .execute(
            "INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, notes, labels_json, removed, rotation_degrees, created_at) VALUES ('asset_busy', ?1, 'synthetic-fingerprint', '/synthetic/asset_busy.mp4', 'asset_busy.mp4', 100, 1, 30, 1920, 1080, 'h264', 'Synthetic Camera', NULL, NULL, '', '[]', 0, 0, '2026-01-01T00:00:00Z')",
            [&project_id],
        )
        .unwrap();
    let catalog = Catalog::open(&catalog_path).unwrap();

    for (kind, arguments) in [
        (
            RepresentationKind::Proxy,
            vec!["proxy", "generate", "--asset", "asset_busy"],
        ),
        (
            RepresentationKind::Thumbnail,
            vec![
                "thumbnails",
                "generate",
                "--asset",
                "asset_busy",
                "--quality",
                "1",
            ],
        ),
    ] {
        let _claim = catalog
            .try_claim_representation("asset_busy", kind)
            .unwrap()
            .unwrap();
        let output = invoke(&catalog_path, arguments);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error envelope");
        assert_eq!(error["ok"], false);
        assert_eq!(error["error"]["code"], "representation_busy");
    }
}

#[cfg(unix)]
#[test]
fn media_tool_failures_keep_cli_stderr_as_one_redacted_json_value() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let media = directory.path().join("synthetic.mp4");
    let doctor = success(&catalog, ["doctor"]);
    let Some(ffmpeg) = available_tool(&doctor.json, "ffmpeg") else {
        return;
    };
    if available_tool(&doctor.json, "ffprobe").is_none() {
        return;
    }
    create_media_fixture(&ffmpeg, &media);
    let project_id = create_project(&catalog, "Synthetic media failure");
    success(
        &catalog,
        [
            "ingest",
            "--project",
            &project_id,
            "--folder",
            directory.path().to_str().unwrap(),
        ],
    );
    let asset_id = success(&catalog, ["clips", "list", "--project", &project_id]).json
        ["data"]["items"][0]["id"]
        .as_str()
        .expect("ingested synthetic asset")
        .to_owned();

    let shim_dir = directory.path().join("tool-shim");
    fs::create_dir_all(&shim_dir).unwrap();
    let shim = shim_dir.join("ffmpeg");
    fs::write(
        &shim,
        "#!/bin/sh\nprintf 'out_time_us=0\\nprogress=continue\\n'\nfor arg do case \"$arg\" in /*) printf 'synthetic decoder detail for %s\\n' \"$arg\" >&2;; esac; done\nprintf 'Invalid data found while processing input\\n' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
    let current_path = env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![shim_dir];
    paths.extend(env::split_paths(&current_path));
    let path = env::join_paths(paths).unwrap();

    for args in [
        vec!["proxy", "generate", "--asset", asset_id.as_str()],
        vec![
            "thumbnails",
            "generate",
            "--asset",
            asset_id.as_str(),
            "--quality",
            "1",
        ],
    ] {
        let output = invoke_with_path(&catalog, args, &path);
        assert!(!output.status.success(), "tool failure must exit nonzero");
        assert!(
            output.stdout.is_empty(),
            "tool failures must keep stdout empty"
        );
        let stderr = String::from_utf8(output.stderr).expect("CLI stderr is UTF-8");
        let error: Value = serde_json::from_str(&stderr).expect("stderr is exactly one JSON value");
        assert_eq!(error["ok"], false);
        assert_eq!(error["error"]["code"], "media_tool_failed");
        let message = error["error"]["message"].as_str().unwrap();
        assert!(message.contains("Invalid data found while processing input"));
        assert!(!message.contains(directory.path().to_str().unwrap()));
        assert!(message.len() < 4600, "diagnostic should remain bounded");
    }
}

#[test]
fn newer_catalog_schema_is_reported_without_attempting_a_downgrade() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    create_project(&catalog, "Future schema");
    Connection::open(&catalog)
        .unwrap()
        .execute(
            "INSERT INTO schema_migrations (migration_id, name, source_identity, applied_at) VALUES (?1, 'future', ?2, '2026-07-20T00:00:00.000Z')",
            params![PROJECT_CATALOG_SCHEMA_VERSION + 1, format!("future/{}", PROJECT_CATALOG_SCHEMA_VERSION + 1)],
        )
        .unwrap();

    let inspection = success(&catalog, ["catalog", "inspect"]);
    assert_eq!(
        inspection.json["data"]["currentSchemaVersion"],
        PROJECT_CATALOG_SCHEMA_VERSION + 1
    );
    assert_eq!(inspection.json["data"]["schemaStatus"], "newer_unsupported");
    let rejected = failure(&catalog, ["project", "list"]);
    assert_eq!(rejected["error"]["code"], "schema_incompatible");
    assert!(
        rejected["error"]["message"]
            .as_str()
            .unwrap()
            .contains("upgrade RCD VlogCutter")
    );
}

#[test]
fn read_commands_remain_available_during_an_uncommitted_writer() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let project_id = create_project(&catalog, "Committed project");

    let mut writer = Connection::open(&catalog).unwrap();
    let transaction = writer.transaction().unwrap();
    transaction
        .execute(
            "INSERT INTO projects (id, name, created_at) VALUES (?1, ?2, ?3)",
            params![
                "project_uncommitted",
                "Uncommitted project",
                "2026-07-18T00:00:00.000Z"
            ],
        )
        .unwrap();

    let list = success(&catalog, ["project", "list"]);
    let projects = list.json["data"].as_array().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0]["id"], project_id);
    transaction.rollback().unwrap();
}

fn agent_process<I, S>(home: &Path, path: Option<&OsStr>, arguments: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let data_home = home.join("xdg-data");
    let mut command = Command::new(env!("CARGO_BIN_EXE_rcdvlog"));
    command
        .env("HOME", home)
        .env("XDG_DATA_HOME", data_home)
        .env_remove("RCDVLOG_CATALOG")
        .args(arguments);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    command.output().expect("run actual rcdvlog binary")
}

fn agent_success<I, S>(home: &Path, path: Option<&OsStr>, arguments: I) -> Value
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = agent_process(home, path, arguments);
    assert!(
        output.status.success(),
        "CLI failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).expect("JSON success envelope");
    assert_eq!(value["ok"], true);
    value
}

fn agent_failure<I, S>(home: &Path, path: Option<&OsStr>, arguments: I) -> Value
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = agent_process(home, path, arguments);
    assert!(!output.status.success(), "CLI unexpectedly succeeded");
    assert!(output.stdout.is_empty(), "failure wrote to stdout");
    let value: Value = serde_json::from_slice(&output.stderr).expect("JSON error envelope");
    assert_eq!(value["ok"], false);
    value
}

#[test]
fn doctor_process_reports_running_build_and_strict_requirements_without_writes() {
    let directory = tempdir().unwrap();
    let home = directory.path().join("isolated home");
    fs::create_dir(&home).unwrap();
    let catalog = directory.path().join("not-created.sqlite3");

    let doctor = agent_success(&home, None, ["doctor"]);
    let data = &doctor["data"];
    let executable = &data["executable"];
    assert_eq!(doctor["command"], "doctor");
    assert_eq!(data["schemaVersion"], 1);
    assert!(executable["path"].as_str().unwrap().starts_with('/'));
    assert_eq!(
        executable["path"],
        fs::canonicalize(env!("CARGO_BIN_EXE_rcdvlog"))
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );
    assert!(executable["build"]["version"].as_str().is_some());
    assert!(executable["build"]["profile"].as_str().is_some());
    assert!(executable["build"]["target"].as_str().is_some());
    assert!(executable["build"]["rustc"].as_str().is_some());
    assert!(executable["build"].get("revision").is_some());
    if let Some(revision) = option_env!("RCDVLOG_BUILD_REVISION")
        && revision != "unknown"
    {
        assert_eq!(executable["build"]["revision"], revision);
    }
    assert!(
        executable["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|command| command == "doctor")
    );
    assert!(executable["capabilities"]["agent.readiness.v1"].is_number());
    assert_eq!(data["catalogStatus"]["status"], "catalog_required");
    assert!(!data["ready"].as_bool().unwrap());
    assert!(!catalog.exists(), "observational doctor created a catalog");
    assert!(!home.join("xdg-data").exists(), "doctor wrote model state");

    let missing_models = agent_failure(&home, None, ["doctor", "--require-models"]);
    assert_eq!(missing_models["error"]["code"], "model_unavailable");
    assert_eq!(missing_models["error"]["details"]["modelsReady"], false);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let fake_bin = directory.path().join("fake bin");
        fs::create_dir(&fake_bin).unwrap();
        let fake_tool = fake_bin.join("ffmpeg");
        let staging = fake_bin.join(".ffmpeg.tmp");
        fs::write(&staging, "#!/bin/sh\nprintf 'ffmpeg version 6.0.0\\n'\n").unwrap();
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o755)).unwrap();
        fs::rename(staging, &fake_tool).unwrap();
        let path = fake_bin.as_os_str();
        let rejected = agent_failure(&home, Some(path), ["doctor", "--require-tool", "ffmpeg"]);
        assert_eq!(rejected["error"]["code"], "media_prerequisite_failed");
        let tools = &rejected["error"]["details"]["tools"];
        assert_eq!(
            tools
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == "ffmpeg")
                .unwrap()["status"],
            "unsupported_version"
        );
        assert_eq!(
            tools
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == "ffmpeg")
                .unwrap()["path"],
            fake_tool.to_string_lossy().as_ref()
        );
    }
}

#[cfg(unix)]
#[test]
fn workspace_process_binds_exact_unicode_executable_and_preserves_project_files() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let directory = tempdir().unwrap();
    let home = directory.path().join("isolated home");
    fs::create_dir(&home).unwrap();
    let catalog = directory.path().join("catalog with spaces.sqlite3");
    let project = agent_success(
        &home,
        None,
        [
            "--catalog",
            catalog.to_str().unwrap(),
            "project",
            "create",
            "--name",
            "Agent workspace process test",
        ],
    );
    assert_eq!(project["command"], "project.create");

    let root = directory.path().join("Project workspace Ω");
    fs::create_dir(&root).unwrap();
    let agents = root.join("AGENTS.md");
    let project_doc = root.join("PROJECT.md");
    fs::write(&agents, "authored instructions\n").unwrap();
    fs::write(&project_doc, "authored project notes\n").unwrap();
    let executable_dir = directory.path().join("Selected CLI Ω");
    fs::create_dir(&executable_dir).unwrap();
    let selected = executable_dir.join("rcdvlog executable");
    symlink(env!("CARGO_BIN_EXE_rcdvlog"), &selected).unwrap();

    let rejected_missing_catalog = agent_failure(
        &home,
        None,
        [
            "workspace",
            "refresh",
            "--root",
            root.to_str().unwrap(),
            "--executable",
            selected.to_str().unwrap(),
            "--selection",
            "development",
        ],
    );
    assert_eq!(
        rejected_missing_catalog["error"]["code"],
        "catalog_required"
    );
    assert!(!root.join(".vlogcutter").exists());

    let unbound_check = agent_failure(
        &home,
        None,
        ["workspace", "check", "--root", root.to_str().unwrap()],
    );
    assert_eq!(unbound_check["error"]["code"], "binding_missing");

    let relative_catalog = agent_failure(
        &home,
        None,
        [
            "--catalog",
            "relative-catalog.sqlite3",
            "workspace",
            "refresh",
            "--root",
            root.to_str().unwrap(),
            "--executable",
            selected.to_str().unwrap(),
            "--selection",
            "development",
        ],
    );
    assert_eq!(relative_catalog["error"]["code"], "invalid_arguments");
    assert!(!directory.path().join("relative-catalog.sqlite3").exists());

    let relative_executable = agent_failure(
        &home,
        None,
        [
            "--catalog",
            catalog.to_str().unwrap(),
            "workspace",
            "refresh",
            "--root",
            root.to_str().unwrap(),
            "--executable",
            "relative-rcdvlog",
            "--selection",
            "development",
        ],
    );
    assert_eq!(relative_executable["error"]["code"], "invalid_arguments");

    let refreshed = agent_success(
        &home,
        None,
        [
            "--catalog",
            catalog.to_str().unwrap(),
            "workspace",
            "refresh",
            "--root",
            root.to_str().unwrap(),
            "--executable",
            selected.to_str().unwrap(),
            "--selection",
            "development",
        ],
    );
    let binding = &refreshed["data"]["binding"];
    assert_eq!(
        binding["root"],
        fs::canonicalize(&root).unwrap().to_string_lossy().as_ref()
    );
    assert_eq!(
        binding["catalog"],
        fs::canonicalize(&catalog)
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(binding["selectedPath"], selected.to_string_lossy().as_ref());
    assert_eq!(binding["selection"], "development");
    assert_eq!(
        binding["executable"]["path"],
        fs::canonicalize(env!("CARGO_BIN_EXE_rcdvlog"))
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );

    let binding_path = root.join(".vlogcutter/cli-binding.json");
    let unknown = root.join(".vlogcutter/operator-notes.json");
    fs::write(&unknown, "{\"ownedBy\":\"operator\"}\n").unwrap();
    let binding_bytes = fs::read(&binding_path).unwrap();
    let binding_modified = fs::metadata(&binding_path).unwrap().modified().unwrap();
    let catalog_bytes = fs::read(&catalog).unwrap();
    let catalog_modified = fs::metadata(&catalog).unwrap().modified().unwrap();
    for _ in 0..2 {
        let checked = agent_success(
            &home,
            None,
            [
                "--catalog",
                catalog.to_str().unwrap(),
                "workspace",
                "check",
                "--root",
                root.to_str().unwrap(),
            ],
        );
        assert_eq!(
            checked["data"]["invocation"][0],
            selected.to_string_lossy().as_ref()
        );
        assert_eq!(checked["data"]["invocation"][1], "--catalog");
        assert_eq!(
            checked["data"]["invocation"][2],
            fs::canonicalize(&catalog)
                .unwrap()
                .to_string_lossy()
                .as_ref()
        );
        assert_eq!(
            checked["data"]["readiness"]["executable"]["path"],
            binding["executable"]["path"]
        );
    }
    assert_eq!(fs::read(&binding_path).unwrap(), binding_bytes);
    assert_eq!(
        fs::metadata(&binding_path).unwrap().modified().unwrap(),
        binding_modified
    );
    assert_eq!(fs::read(&catalog).unwrap(), catalog_bytes);
    assert_eq!(
        fs::metadata(&catalog).unwrap().modified().unwrap(),
        catalog_modified
    );
    assert_eq!(fs::read(&agents).unwrap(), b"authored instructions\n");
    assert_eq!(fs::read(&project_doc).unwrap(), b"authored project notes\n");
    assert_eq!(fs::read(&unknown).unwrap(), b"{\"ownedBy\":\"operator\"}\n");

    let wrong_revision = agent_failure(
        &home,
        None,
        [
            "--catalog",
            catalog.to_str().unwrap(),
            "workspace",
            "check",
            "--root",
            root.to_str().unwrap(),
            "--expect-revision",
            "0000000000000000000000000000000000000000",
        ],
    );
    assert_eq!(wrong_revision["error"]["code"], "revision_mismatch");
    let unsupported_capability = agent_failure(
        &home,
        None,
        [
            "--catalog",
            catalog.to_str().unwrap(),
            "workspace",
            "check",
            "--root",
            root.to_str().unwrap(),
            "--require-capability",
            "operator.magic.v99",
        ],
    );
    assert_eq!(
        unsupported_capability["error"]["code"],
        "capability_unavailable"
    );

    fs::remove_file(&selected).unwrap();
    let missing = agent_failure(
        &home,
        None,
        [
            "--catalog",
            catalog.to_str().unwrap(),
            "workspace",
            "check",
            "--root",
            root.to_str().unwrap(),
        ],
    );
    assert_eq!(missing["error"]["code"], "executable_unavailable");

    let marker = directory.path().join("fallback-was-executed");
    let selected_staging = executable_dir.join(".replacement.tmp");
    fs::write(
        &selected_staging,
        format!("#!/bin/sh\ntouch '{}'\nexit 0\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&selected_staging, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(&selected_staging, &selected).unwrap();
    let replaced = agent_failure(
        &home,
        None,
        [
            "--catalog",
            catalog.to_str().unwrap(),
            "workspace",
            "check",
            "--root",
            root.to_str().unwrap(),
        ],
    );
    assert_eq!(replaced["error"]["code"], "binding_stale");
    assert!(
        !marker.exists(),
        "changed selected executable was invoked as a fallback"
    );

    fs::remove_file(&selected).unwrap();
    symlink(env!("CARGO_BIN_EXE_rcdvlog"), &selected).unwrap();
    let repaired = agent_success(
        &home,
        None,
        [
            "--catalog",
            catalog.to_str().unwrap(),
            "workspace",
            "refresh",
            "--root",
            root.to_str().unwrap(),
            "--executable",
            selected.to_str().unwrap(),
            "--selection",
            "development",
        ],
    );
    assert_eq!(
        repaired["data"]["binding"]["selectedPath"],
        selected.to_string_lossy().as_ref()
    );
}

#[test]
fn strict_catalog_readiness_rejects_missing_legacy_and_newer_catalogs_read_only() {
    let directory = tempdir().unwrap();
    let home = directory.path().join("isolated home");
    fs::create_dir(&home).unwrap();
    let missing = directory.path().join("missing.sqlite3");
    let missing_error = agent_failure(
        &home,
        None,
        ["--catalog", missing.to_str().unwrap(), "doctor", "--check"],
    );
    assert_eq!(missing_error["error"]["code"], "catalog_missing");
    assert!(!missing.exists());

    let legacy = directory.path().join("legacy.sqlite3");
    agent_success(
        &home,
        None,
        [
            "--catalog",
            legacy.to_str().unwrap(),
            "project",
            "create",
            "--name",
            "Legacy readiness fixture",
        ],
    );
    Connection::open(&legacy)
        .unwrap()
        .execute_batch("DROP TABLE schema_migrations;")
        .unwrap();
    let legacy_bytes = fs::read(&legacy).unwrap();
    let legacy_error = agent_failure(
        &home,
        None,
        ["--catalog", legacy.to_str().unwrap(), "doctor", "--check"],
    );
    assert_eq!(legacy_error["error"]["code"], "catalog_upgrade_required");
    assert_eq!(fs::read(&legacy).unwrap(), legacy_bytes);

    let newer = directory.path().join("newer.sqlite3");
    agent_success(
        &home,
        None,
        [
            "--catalog",
            newer.to_str().unwrap(),
            "project",
            "create",
            "--name",
            "Newer readiness fixture",
        ],
    );
    Connection::open(&newer)
        .unwrap()
        .execute(
            "INSERT INTO schema_migrations (migration_id, name, source_identity, applied_at) VALUES (?1, 'future', ?2, '2026-01-01T00:00:00Z')",
            params![PROJECT_CATALOG_SCHEMA_VERSION + 1, format!("future/{}", PROJECT_CATALOG_SCHEMA_VERSION + 1)],
        )
        .unwrap();
    let newer_bytes = fs::read(&newer).unwrap();
    let newer_error = agent_failure(
        &home,
        None,
        ["--catalog", newer.to_str().unwrap(), "doctor", "--check"],
    );
    assert_eq!(newer_error["error"]["code"], "schema_incompatible");
    assert_eq!(fs::read(&newer).unwrap(), newer_bytes);
}

#[cfg(unix)]
#[test]
fn workspace_rejects_old_doctor_contracts_and_preserves_unrecognized_binding_files() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempdir().unwrap();
    let home = directory.path().join("home");
    fs::create_dir(&home).unwrap();
    let root = directory.path().join("project");
    fs::create_dir(&root).unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    create_project(&catalog, "Binding rejection fixture");
    let executable = directory.path().join("old executable");
    let staging = directory.path().join(".old-executable.tmp");
    fs::write(&staging, "#!/bin/sh\nprintf '%s\\n' '{\"ok\":true,\"command\":\"doctor\",\"data\":{\"tools\":[]}}'\n").unwrap();
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(staging, &executable).unwrap();
    let refresh = [
        "--catalog",
        catalog.to_str().unwrap(),
        "workspace",
        "refresh",
        "--root",
        root.to_str().unwrap(),
        "--executable",
        executable.to_str().unwrap(),
        "--selection",
        "installed",
    ];
    let rejected = agent_failure(&home, None, refresh);
    assert_eq!(rejected["error"]["code"], "capability_unavailable");
    assert!(!root.join(".vlogcutter").exists());

    let managed_directory = root.join(".vlogcutter");
    fs::write(&managed_directory, b"operator authored file").unwrap();
    let rejected = agent_failure(&home, None, refresh);
    assert_eq!(rejected["error"]["code"], "binding_invalid");
    assert_eq!(
        fs::read(&managed_directory).unwrap(),
        b"operator authored file"
    );
    fs::remove_file(&managed_directory).unwrap();
    fs::create_dir(&managed_directory).unwrap();
    let binding = managed_directory.join("cli-binding.json");
    fs::write(&binding, b"operator authored content").unwrap();
    let rejected = agent_failure(&home, None, refresh);
    assert_eq!(rejected["error"]["code"], "binding_invalid");
    assert_eq!(fs::read(binding).unwrap(), b"operator authored content");
}

#[test]
fn camera_mapping_commands_list_rename_and_clear_project_scoped_state() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let project_id = create_project(&catalog, "Camera mappings");
    let other_project_id = create_project(&catalog, "Other project");
    Connection::open(&catalog)
        .unwrap()
        .execute(
            "INSERT INTO camera_identity_mappings
             (id, project_id, folder_hint, filename_prefix, exif_hint, camera, created_at, updated_at)
             VALUES ('cammap_sony', ?1, 'Sony', 'C', 'SONY ILCE-7CM2', 'Sony A7C II',
                     '2026-08-19T00:00:00Z', '2026-08-19T00:00:00Z')",
            params![project_id],
        )
        .unwrap();

    let listed = success(
        &catalog,
        ["camera-mappings", "list", "--project", &project_id],
    );
    assert_eq!(listed.json["command"], "camera-mappings.list");
    assert_eq!(listed.json["data"].as_array().unwrap().len(), 1);
    assert_eq!(listed.json["data"][0]["id"], "cammap_sony");
    assert_eq!(listed.json["data"][0]["folderHint"], "Sony");
    assert_eq!(listed.json["data"][0]["filenamePrefix"], "C");
    assert_eq!(listed.json["data"][0]["exifHint"], "SONY ILCE-7CM2");
    assert_eq!(listed.json["data"][0]["camera"], "Sony A7C II");

    // Listing is a diagnostic read: it stays available and excludes
    // uncommitted rows while another connection owns the writer slot.
    let mut writer = Connection::open(&catalog).unwrap();
    let transaction = writer.transaction().unwrap();
    transaction
        .execute(
            "INSERT INTO camera_identity_mappings
             (id, project_id, folder_hint, filename_prefix, exif_hint, camera, created_at, updated_at)
             VALUES ('cammap_uncommitted', ?1, 'DJI', 'DJI', 'DJI FC8282', 'DJI Pocket 3',
                     '2026-08-19T00:00:00Z', '2026-08-19T00:00:00Z')",
            params![project_id],
        )
        .unwrap();
    let concurrent_list = success(
        &catalog,
        ["camera-mappings", "list", "--project", &project_id],
    );
    assert_eq!(concurrent_list.json["data"].as_array().unwrap().len(), 1);
    transaction.rollback().unwrap();

    let wrong_project = failure(
        &catalog,
        [
            "camera-mappings",
            "rename",
            "--project",
            &other_project_id,
            "--mapping",
            "cammap_sony",
            "--camera",
            "Sony A7C II backup",
        ],
    );
    assert_eq!(wrong_project["command"], "camera-mappings.rename");
    assert_eq!(wrong_project["error"]["code"], "not_found");

    let renamed = success(
        &catalog,
        [
            "camera-mappings",
            "rename",
            "--project",
            &project_id,
            "--mapping",
            "cammap_sony",
            "--camera",
            "Sony A7C II backup",
        ],
    );
    assert_eq!(renamed.json["command"], "camera-mappings.rename");
    assert_eq!(renamed.json["data"]["id"], "cammap_sony");
    assert_eq!(renamed.json["data"]["camera"], "Sony A7C II backup");

    let persisted = success(
        &catalog,
        ["camera-mappings", "list", "--project", &project_id],
    );
    assert_eq!(persisted.json["data"][0]["camera"], "Sony A7C II backup");

    let cleared = success(
        &catalog,
        [
            "camera-mappings",
            "clear",
            "--project",
            &project_id,
            "--mapping",
            "cammap_sony",
        ],
    );
    assert_eq!(cleared.json["command"], "camera-mappings.clear");
    assert_eq!(cleared.json["data"]["projectId"], project_id);
    assert_eq!(cleared.json["data"]["mappingId"], "cammap_sony");
    assert_eq!(cleared.json["data"]["cleared"], true);

    let empty = success(
        &catalog,
        ["camera-mappings", "list", "--project", &project_id],
    );
    assert!(empty.json["data"].as_array().unwrap().is_empty());

    // Clear is deliberately idempotent for retries and races with the UI.
    let cleared_again = success(
        &catalog,
        [
            "camera-mappings",
            "clear",
            "--project",
            &project_id,
            "--mapping",
            "cammap_sony",
        ],
    );
    assert_eq!(cleared_again.json["data"]["cleared"], true);
}

#[test]
fn camera_merge_commands_preview_and_preserve_separate_cards() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let project_id = create_project(&catalog, "Camera merge");
    let connection = Connection::open(&catalog).unwrap();
    for (id, camera) in [("clip_wrong", "Sony wrong"), ("clip_right", "Sony")] {
        connection.execute(
            "INSERT INTO assets (id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, created_at) VALUES (?1, ?2, ?3, '', ?4, 1, 1, 30, 1920, 1080, 'h264', ?5, NULL, NULL, '2026-08-19T00:00:00Z')",
            params![id, project_id, format!("fp-{id}"), format!("{id}.MP4"), camera],
        ).unwrap();
    }
    for (id, camera) in [
        ("card_wrong", "Sony wrong"),
        ("card_right_a", "Sony"),
        ("card_right_b", "Sony"),
    ] {
        connection.execute(
            "INSERT INTO capture_sequences (id, project_id, camera_key, label, volume_hint, filename_prefix, created_at) VALUES (?1, ?2, ?3, ?1, '', '', '2026-08-19T00:00:00Z')",
            params![id, project_id, camera],
        ).unwrap();
    }
    connection.execute(
        "INSERT INTO asset_capture_sequence (asset_id, project_id, capture_sequence_id, filename_counter) VALUES ('clip_wrong', ?1, 'card_wrong', 1)",
        [&project_id],
    ).unwrap();
    connection.execute(
        "INSERT INTO clock_epochs (id, project_id, capture_sequence_id, start_counter, end_counter, offset_seconds, confidence, evidence_json, label, created_at) VALUES ('epoch_wrong', ?1, 'card_wrong', 0, NULL, 60, 1, '[]', 'kept', '2026-08-19T00:00:00Z')",
        [&project_id],
    ).unwrap();
    drop(connection);

    let preview = success(
        &catalog,
        [
            "cameras",
            "merge-preview",
            "--project",
            &project_id,
            "--from",
            "Sony wrong",
            "--into",
            "Sony",
        ],
    );
    assert_eq!(preview.json["command"], "cameras.merge-preview");
    assert_eq!(preview.json["data"]["affectedClipCount"], 1);
    assert_eq!(preview.json["data"]["affectedCaptureSequenceCount"], 1);
    assert_eq!(preview.json["data"]["destinationCaptureSequenceCount"], 2);

    let merged = success(
        &catalog,
        [
            "cameras",
            "merge",
            "--project",
            &project_id,
            "--from",
            "Sony wrong",
            "--into",
            "Sony",
        ],
    );
    assert_eq!(merged.json["command"], "cameras.merge");
    assert_eq!(merged.json["data"]["movedClipCount"], 1);
    assert_eq!(merged.json["data"]["movedCaptureSequenceCount"], 1);

    let connection = Connection::open(&catalog).unwrap();
    let cards: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM capture_sequences WHERE project_id=?1 AND camera_key='Sony'",
            [&project_id],
            |row| row.get(0),
        )
        .unwrap();
    let epoch_sequence: String = connection
        .query_row(
            "SELECT capture_sequence_id FROM clock_epochs WHERE id='epoch_wrong'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cards, 3);
    assert_eq!(epoch_sequence, "card_wrong");
}

/// An agent's whole-clip write is refused when the clip moved under it.
///
/// This is the concurrent-writer case the CLI exists to serve: an agent
/// annotating the queue while a human reviews. A whole patch carries every
/// judgment field, so applying one built from a stale read does not merely
/// miss the other writer's change — it reverses it.
#[test]
fn a_stale_clip_update_is_refused_with_a_code_the_agent_can_act_on() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let project_id = create_project(&catalog, "Concurrent writers");
    Connection::open(&catalog)
        .unwrap()
        .execute(
            "INSERT INTO assets(id, project_id, fingerprint, path, filename, size_bytes, duration_seconds, frame_rate, width, height, codec, camera, captured_at_raw, best_time, notes, labels_json, removed, rotation_degrees, created_at) \
             VALUES ('asset_one', ?1, 'fingerprint', '/offline/clip.mp4', 'clip.mp4', 100, 10, 30, 1920, 1080, 'h264', 'DJI', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00.000Z', '', '[]', 0, 0, '2026-01-01T00:00:00Z')",
            params![project_id],
        )
        .unwrap();

    let read = success(&catalog, ["clips", "get", "--asset", "asset_one"]);
    let seen = read.json["data"]["revision"].as_u64().unwrap();

    // Somebody else writes first.
    success(
        &catalog,
        [
            "clips",
            "update",
            "--asset",
            "asset_one",
            "--add-label",
            "market",
        ],
    );

    let refused = failure(
        &catalog,
        [
            "clips",
            "update",
            "--asset",
            "asset_one",
            "--notes",
            "decided before the label landed",
            "--expected-revision",
            &seen.to_string(),
        ],
    );
    assert_eq!(refused["error"]["code"], "stale_revision");

    let after = success(&catalog, ["clips", "get", "--asset", "asset_one"]);
    assert_eq!(after.json["data"]["labels"][0], "market");
    assert_eq!(after.json["data"]["notes"], "");

    // Re-read, keep the edit, retry: both writers' work survives.
    let current = after.json["data"]["revision"].as_u64().unwrap();
    assert_ne!(current, seen);
    let settled = success(
        &catalog,
        [
            "clips",
            "update",
            "--asset",
            "asset_one",
            "--notes",
            "decided before the label landed",
            "--expected-revision",
            &current.to_string(),
        ],
    );
    assert_eq!(
        settled.json["data"]["notes"],
        "decided before the label landed"
    );
    assert_eq!(settled.json["data"]["labels"][0], "market");
}

#[test]
fn media_workflow_runs_end_to_end_through_the_agent_cli() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let doctor = success(&catalog, ["doctor"]);
    let Some(ffmpeg) = available_tool(&doctor.json, "ffmpeg") else {
        eprintln!("ffmpeg is unavailable; skipping external-media integration test");
        return;
    };
    if available_tool(&doctor.json, "ffprobe").is_none() {
        eprintln!("ffprobe is unavailable; skipping external-media integration test");
        return;
    }

    let source_dir = directory.path().join("media with spaces");
    fs::create_dir_all(&source_dir).unwrap();
    let source = source_dir.join("C0001.MP4");
    create_media_fixture(&ffmpeg, &source);
    fs::write(source_dir.join("README.txt"), b"not media").unwrap();

    let project_id = create_project(&catalog, "Expo integration");
    let ingest = success(
        &catalog,
        [
            "ingest",
            "--project",
            &project_id,
            "--folder",
            source_dir.to_str().unwrap(),
            "--camera",
            "Synthetic selected camera",
        ],
    );
    assert_eq!(ingest.json["data"]["scanned"], 1);
    assert_eq!(ingest.json["data"]["imported"], 1);
    assert_eq!(ingest.json["data"]["failures"].as_array().unwrap().len(), 0);
    let imported = success(&catalog, ["clips", "list", "--project", &project_id]);
    let asset_id = imported.json["data"]["items"][0]["id"].as_str().unwrap();
    let asset = success(&catalog, ["clips", "get", "--asset", asset_id]);
    assert_eq!(asset.json["data"]["camera"], "Synthetic selected camera");
    let repair_preview = success(
        &catalog,
        [
            "cameras",
            "repair-preview",
            "--project",
            &project_id,
            "--camera",
            "Corrected Synthetic Camera",
            "--asset",
            asset_id,
        ],
    );
    let preview_path = directory.path().join("camera-repair-preview.json");
    fs::write(
        &preview_path,
        serde_json::to_vec(&repair_preview.json).unwrap(),
    )
    .unwrap();
    success(
        &catalog,
        [
            "cameras",
            "repair",
            "--preview",
            preview_path.to_str().unwrap(),
        ],
    );
    let repaired = success(&catalog, ["clips", "get", "--asset", asset_id]);
    assert_eq!(
        repaired.json["data"]["camera"],
        "Corrected Synthetic Camera"
    );
    let progress: Value = serde_json::from_str(ingest.stderr.trim()).expect("JSON progress event");
    assert_eq!(progress["event"], "ingest.progress");
    assert_eq!(progress["completed"], 1);
    assert_eq!(progress["total"], 1);

    let duplicate = success(
        &catalog,
        [
            "ingest",
            "--project",
            &project_id,
            "--folder",
            source_dir.to_str().unwrap(),
        ],
    );
    assert_eq!(duplicate.json["data"]["imported"], 0);
    assert_eq!(duplicate.json["data"]["duplicates"], 1);

    let clips = success(&catalog, ["clips", "list", "--project", &project_id]);
    assert_eq!(clips.json["data"]["total"], 1);
    let asset = &clips.json["data"]["items"][0];
    let asset_id = asset["id"].as_str().unwrap().to_string();
    let duration = asset["durationSeconds"].as_f64().unwrap();
    assert!(duration > 0.0);
    assert_eq!(asset["width"], 640);
    assert_eq!(asset["height"], 360);

    let invalid_label = "x".repeat(65);
    let invalid = failure(
        &catalog,
        [
            "clips",
            "update",
            "--asset",
            &asset_id,
            "--add-label",
            &invalid_label,
        ],
    );
    assert_eq!(invalid["error"]["code"], "invalid_input");

    let updated = success(
        &catalog,
        [
            "clips",
            "update",
            "--asset",
            &asset_id,
            "--notes",
            "Opening station view",
            "--add-label",
            "station",
            "--add-label",
            "opening",
        ],
    );
    assert_eq!(updated.json["data"]["notes"], "Opening station view");
    assert_eq!(
        updated.json["data"]["labels"],
        serde_json::json!(["station", "opening"])
    );
    assert_eq!(updated.json["data"]["removed"], false);

    let rotated = success(
        &catalog,
        [
            "clips",
            "update",
            "--asset",
            &asset_id,
            "--rotation-degrees",
            "180",
        ],
    );
    assert_eq!(rotated.json["data"]["rotationDegrees"], 180);
    let invalid_rotation = failure(
        &catalog,
        [
            "clips",
            "update",
            "--asset",
            &asset_id,
            "--rotation-degrees",
            "45",
        ],
    );
    assert_eq!(invalid_rotation["error"]["code"], "invalid_input");

    let camera = updated.json["data"]["camera"].as_str().unwrap();
    let corrected = success(
        &catalog,
        [
            "clock",
            "apply",
            "--project",
            &project_id,
            "--camera",
            camera,
            "--start-filename",
            "C0001.MP4",
            "--offset-minutes",
            "-60",
            "--label",
            "Camera timezone changed",
        ],
    );
    assert_eq!(corrected.json["data"]["offsetSeconds"], -3600);
    let corrected_asset = success(&catalog, ["clips", "get", "--asset", &asset_id]);
    assert_eq!(corrected_asset.json["data"]["offsetSeconds"], -3600);

    let proxied = success(&catalog, ["proxy", "generate", "--asset", &asset_id]);
    let proxy_path = PathBuf::from(proxied.json["data"]["proxyPath"].as_str().unwrap());
    assert!(proxy_path.is_file());
    assert!(proxy_path.to_string_lossy().ends_with(".v3.mp4"));
    assert_eq!(proxied.json["data"]["proxyRepresentation"], "raw_v3");
    let diagnosed = success(&catalog, ["clips", "diagnose", "--asset", &asset_id]);
    assert_eq!(
        diagnosed.json["data"]["renderPlan"]["selectedRepresentation"],
        "proxy"
    );
    assert_eq!(
        diagnosed.json["data"]["renderPlan"]["finalOrientation"],
        "landscape"
    );
    let proxy_status = success(&catalog, ["proxy", "status", "--asset", &asset_id]);
    assert_eq!(proxy_status.json["data"]["summary"]["current"], 1);
    assert_eq!(
        proxy_status.json["data"]["items"][0]["validation"]["metadataRotationDegrees"],
        0
    );
    assert_eq!(
        proxy_status.json["data"]["items"][0]["regenerationRequired"],
        false
    );
    let missing = success(&catalog, ["proxy", "missing", "--project", &project_id]);
    assert_eq!(missing.json["data"]["generated"], 0);
    assert_eq!(missing.json["data"]["skipped"], 1);

    let thumbnails = success(
        &catalog,
        [
            "thumbnails",
            "missing",
            "--project",
            &project_id,
            "--quality",
            "1",
        ],
    );
    assert_eq!(thumbnails.json["data"]["quality"], 1);
    assert_eq!(thumbnails.json["data"]["generated"], 1);
    let thumbnail = success(&catalog, ["clips", "get", "--asset", &asset_id]);
    let thumbnail_path = PathBuf::from(thumbnail.json["data"]["thumbnailPath"].as_str().unwrap());
    assert!(thumbnail_path.is_file());
    assert!(thumbnail_path.starts_with(directory.path().join("cache/thumbnails")));
    let with_thumbnail = success(&catalog, ["project", "status", "--project", &project_id]);
    assert_eq!(with_thumbnail.json["data"]["media"]["thumbnailsReady"], 1);

    fs::rename(&source, source_dir.join("C0001.offline")).unwrap();
    let offline = success(&catalog, ["clips", "get", "--asset", &asset_id]);
    assert_eq!(offline.json["data"]["available"], true);
    let status = success(&catalog, ["project", "status", "--project", &project_id]);
    assert_eq!(status.json["data"]["media"]["originalsOffline"], 1);
    assert_eq!(status.json["data"]["media"]["proxiesReady"], 1);

    let removed = success(
        &catalog,
        ["clips", "update", "--asset", &asset_id, "--removed", "true"],
    );
    assert_eq!(removed.json["data"]["removed"], true);
    let active = success(&catalog, ["clips", "list", "--project", &project_id]);
    assert_eq!(active.json["data"]["total"], 0);
    let all = success(
        &catalog,
        [
            "clips",
            "list",
            "--project",
            &project_id,
            "--include-removed",
        ],
    );
    assert_eq!(all.json["data"]["total"], 1);
}
