use super::*;
use rusqlite::{Connection, params};
use tempfile::tempdir;

fn parse(arguments: &[&str]) -> Cli {
    Cli::try_parse_from(arguments).unwrap()
}

fn insert_cli_asset(
    catalog_path: &Path,
    project_id: &str,
    id: &str,
    filename: &str,
    notes: &str,
    labels_json: &str,
    removed: bool,
) {
    Connection::open(catalog_path)
        .unwrap()
        .execute(
            r#"
                INSERT INTO assets (
                    id, project_id, fingerprint, path, filename, size_bytes,
                    duration_seconds, frame_rate, width, height, codec, camera,
                    captured_at_raw, best_time, notes, labels_json, removed,
                    created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, 100, 10, 30, 1920, 1080,
                          'h264', 'DJI', '2026-07-18T00:00:00Z',
                          '2026-07-18T00:00:00.000Z', ?6, ?7, ?8,
                          '2026-07-18T00:00:00.000Z')
                "#,
            params![
                id,
                project_id,
                format!("fingerprint-{id}"),
                format!("/missing/{filename}"),
                filename,
                notes,
                labels_json,
                removed,
            ],
        )
        .unwrap();
}

#[test]
fn creates_lists_and_summarizes_a_project() {
    let directory = tempdir().unwrap();
    let catalog = directory.path().join("catalog.sqlite3");
    let catalog_arg = catalog.to_string_lossy();
    let create = parse(&[
        "rcdvlog",
        "--catalog",
        &catalog_arg,
        "project",
        "create",
        "--name",
        "CLI test",
    ]);
    let created = execute(&create).unwrap();
    let project_id = created["id"].as_str().unwrap();

    let list = parse(&["rcdvlog", "--catalog", &catalog_arg, "project", "list"]);
    assert_eq!(execute(&list).unwrap().as_array().unwrap().len(), 1);

    let status = parse(&[
        "rcdvlog",
        "--catalog",
        &catalog_arg,
        "project",
        "status",
        "--project",
        project_id,
    ]);
    assert_eq!(execute(&status).unwrap()["media"]["assets"], 0);
}

#[test]
fn clap_contract_has_all_agent_commands() {
    let help = Cli::command().render_long_help().to_string();
    for command in [
        "doctor",
        "project",
        "camera-mappings",
        "ingest",
        "clips",
        "clock",
        "proxy",
        "thumbnails",
        "transcribe",
        "analyze",
        "models",
        "moments",
        "labels",
        "prepare",
        "calibrate",
    ] {
        assert!(help.contains(command));
    }
}

#[test]
fn transcribe_subcommands_parse() {
    assert!(Cli::try_parse_from(["rcdvlog", "transcribe", "missing", "--project", "p1"]).is_ok());
    assert!(Cli::try_parse_from(["rcdvlog", "transcribe", "generate", "--asset", "a1"]).is_ok());
    assert!(
        Cli::try_parse_from([
            "rcdvlog",
            "transcribe",
            "batch",
            "--project",
            "p1",
            "--asset",
            "a1",
            "--asset",
            "a2",
        ])
        .is_ok()
    );
    assert!(Cli::try_parse_from(["rcdvlog", "transcribe", "status", "--project", "p1"]).is_ok());
}

#[test]
fn calibration_and_resource_override_flags_parse() {
    let status = parse(&["rcdvlog", "calibrate", "status", "--recipe", "speech"]);
    assert_eq!(command_name(&status.command), "calibrate.status");
    let capabilities = parse(&["rcdvlog", "calibrate", "capabilities"]);
    assert_eq!(
        command_name(&capabilities.command),
        "calibrate.capabilities"
    );
    let run = parse(&[
        "rcdvlog",
        "calibrate",
        "run",
        "--recipe",
        "proxy",
        "--worker-counts",
        "1,2",
        "--trials",
        "2",
        "--force",
    ]);
    assert_eq!(command_name(&run.command), "calibrate.run");
    let transcribe = parse(&[
        "rcdvlog",
        "transcribe",
        "batch",
        "--project",
        "p1",
        "--asset",
        "a1",
        "--vad-threads",
        "2",
        "--decode-threads",
        "4",
    ]);
    assert_eq!(command_name(&transcribe.command), "transcribe.batch");
    let proxy = parse(&[
        "rcdvlog",
        "proxy",
        "missing",
        "--project",
        "p1",
        "--workers",
        "2",
    ]);
    assert_eq!(command_name(&proxy.command), "proxy.missing");
}

#[test]
fn transcribe_generate_requires_an_asset() {
    assert!(Cli::try_parse_from(["rcdvlog", "transcribe", "generate"]).is_err());
}

#[test]
fn transcribe_status_requires_a_project() {
    assert!(Cli::try_parse_from(["rcdvlog", "transcribe", "status"]).is_err());
}

#[test]
fn speech_model_backend_flags_preserve_default_and_parse_explicit_native_selection() {
    let local = parse(&["rcdvlog", "models", "status"]);
    assert_eq!(command_name(&local.command), "models.status");
    let local_install = parse(&["rcdvlog", "models", "install"]);
    assert_eq!(command_name(&local_install.command), "models.install");

    let native = parse(&[
        "rcdvlog",
        "models",
        "status",
        "--backend",
        "native-openrouter",
        "--provider",
        "openrouter",
        "--model",
        "openai/whisper-1",
    ]);
    assert_eq!(command_name(&native.command), "models.status");
    let native_install = parse(&[
        "rcdvlog",
        "models",
        "install",
        "--backend",
        "native-openrouter",
        "--provider",
        "openrouter",
        "--model",
        "openai/whisper-1",
    ]);
    assert_eq!(command_name(&native_install.command), "models.install");

    let mut command = Cli::command();
    let models = command.find_subcommand_mut("models").unwrap();
    let status = models.find_subcommand_mut("status").unwrap();
    let help = status.render_long_help().to_string();
    assert!(help.contains("local-whisper"));
    assert!(help.contains("native-openrouter"));
    assert!(help.contains("--provider"));
    assert!(help.contains("--model"));
}

#[test]
fn analysis_and_label_subcommands_parse() {
    assert!(Cli::try_parse_from(["rcdvlog", "analyze", "missing", "--project", "p1"]).is_ok());
    assert!(
        Cli::try_parse_from([
            "rcdvlog",
            "analyze",
            "missing",
            "--project",
            "p1",
            "--force",
        ])
        .is_ok()
    );
    assert!(Cli::try_parse_from(["rcdvlog", "analyze", "generate", "--asset", "a1"]).is_ok());
    assert!(Cli::try_parse_from(["rcdvlog", "analyze", "status", "--project", "p1"]).is_ok());
    assert!(
        Cli::try_parse_from([
            "rcdvlog",
            "analyze",
            "missing",
            "--project",
            "p1",
            "--budget-usd",
            "1.5",
            "--primary-model",
            "google/gemini-2.5-flash-lite",
            "--allow-model",
            "google/gemini-2.5-flash-lite",
            "--no-fallback",
        ])
        .is_ok()
    );
    assert!(
        Cli::try_parse_from([
            "rcdvlog",
            "analyze",
            "suggest-chronology",
            "--project",
            "p1",
            "--asset",
            "a1",
            "--asset",
            "a2",
        ])
        .is_ok()
    );
    assert!(Cli::try_parse_from(["rcdvlog", "labels", "consolidate", "--project", "p1",]).is_ok());
    assert!(Cli::try_parse_from(["rcdvlog", "labels", "vocabulary", "--project", "p1",]).is_ok());
    assert!(
        Cli::try_parse_from([
            "rcdvlog",
            "analyze",
            "accounting",
            "inspect",
            "--project",
            "p1",
            "--operation",
            "pilot",
            "--limit",
            "20",
        ])
        .is_ok()
    );
    assert!(
        Cli::try_parse_from([
            "rcdvlog",
            "analyze",
            "accounting",
            "reconcile",
            "--project",
            "p1",
            "--request",
            "r1",
            "--local-no-upload",
        ])
        .is_ok()
    );
    assert!(
        Cli::try_parse_from([
            "rcdvlog",
            "analyze",
            "accounting",
            "reconcile",
            "--project",
            "p1",
            "--request",
            "r1",
            "--local-no-upload",
            "--unknown-outcome",
        ])
        .is_err()
    );
    assert!(
        Cli::try_parse_from([
            "rcdvlog",
            "analyze",
            "accounting",
            "retry-authorize",
            "--project",
            "p1",
            "--request",
            "r1",
        ])
        .is_ok()
    );
    assert!(
        Cli::try_parse_from([
            "rcdvlog",
            "analyze",
            "accounting",
            "targeted-retry-dispatch",
            "--project",
            "p1",
            "--request",
            "r1",
            "--expected-next-attempt-epoch",
            "1",
            "--payload-file",
            "/tmp/payload.json",
        ])
        .is_ok()
    );
}

#[test]
fn transcribe_status_reports_coverage_on_an_empty_project() {
    let directory = tempdir().unwrap();
    let catalog_path = directory.path().join("catalog.sqlite3");
    let catalog = Catalog::open(&catalog_path).unwrap();
    let project = catalog.create_project("Coverage test").unwrap();
    let catalog_arg = catalog_path.to_string_lossy().to_string();

    let status = parse(&[
        "rcdvlog",
        "--catalog",
        &catalog_arg,
        "transcribe",
        "status",
        "--project",
        &project.id,
    ]);
    let value = execute(&status).unwrap();
    assert_eq!(value["total"], 0);
    assert_eq!(value["missing"], 0);
    assert_eq!(value["noAudio"], 0);
    assert_eq!(value["noAcceptedSpeech"], 0);
    assert_eq!(value["legacyNoSpeechUnverified"], 0);
    assert_eq!(value["recipeKey"], rcdvlog_core::STT_RECIPE_KEY);
}

#[test]
fn clip_update_is_a_patch_for_labels_and_removed_state() {
    let directory = tempdir().unwrap();
    let catalog_path = directory.path().join("catalog.sqlite3");
    let catalog = Catalog::open(&catalog_path).unwrap();
    let project = catalog.create_project("Patch test").unwrap();
    insert_cli_asset(
        &catalog_path,
        &project.id,
        "asset_a",
        "DJI_A.MP4",
        "original note",
        "[\"arrival\"]",
        false,
    );
    let catalog_arg = catalog_path.to_string_lossy();

    let notes_only = parse(&[
        "rcdvlog",
        "--catalog",
        &catalog_arg,
        "clips",
        "update",
        "--asset",
        "asset_a",
        "--notes",
        "changed note",
    ]);
    let updated = execute(&notes_only).unwrap();
    assert_eq!(updated["notes"], "changed note");
    assert_eq!(updated["labels"], json!(["arrival"]));
    assert_eq!(updated["removed"], false);

    let label_update = parse(&[
        "rcdvlog",
        "--catalog",
        &catalog_arg,
        "clips",
        "update",
        "--asset",
        "asset_a",
        "--remove-label",
        "ARRIVAL",
        "--add-label",
        "train",
        "--add-label",
        "Train",
        "--removed",
        "true",
    ]);
    let updated = execute(&label_update).unwrap();
    assert_eq!(updated["labels"], json!(["train"]));
    assert_eq!(updated["removed"], true);
    assert_eq!(updated["notes"], "changed note");
}

#[test]
fn clip_list_filters_before_paginating() {
    let directory = tempdir().unwrap();
    let catalog_path = directory.path().join("catalog.sqlite3");
    let catalog = Catalog::open(&catalog_path).unwrap();
    let project = catalog.create_project("Filter test").unwrap();
    for (id, filename, notes) in [
        ("asset_a", "DJI_A.MP4", "arrival exterior"),
        ("asset_b", "DJI_B.MP4", "arrival interior"),
        ("asset_c", "DJI_C.MP4", "departure"),
    ] {
        insert_cli_asset(&catalog_path, &project.id, id, filename, notes, "[]", false);
    }
    let catalog_arg = catalog_path.to_string_lossy();
    let command = parse(&[
        "rcdvlog",
        "--catalog",
        &catalog_arg,
        "clips",
        "list",
        "--project",
        &project.id,
        "--query",
        "ARRIVAL",
        "--limit",
        "1",
        "--offset",
        "1",
    ]);
    let result = execute(&command).unwrap();
    assert_eq!(result["total"], 2);
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["items"][0]["filename"], "DJI_B.MP4");
}

#[test]
fn proxy_probe_is_machine_readable_and_does_not_create_a_catalog() {
    let directory = tempdir().unwrap();
    let catalog_path = directory.path().join("not-created-probe.sqlite3");
    let catalog_arg = catalog_path.to_string_lossy();
    let probe = parse(&[
        "rcdvlog",
        "--catalog",
        &catalog_arg,
        "proxy",
        "probe",
        "--policy",
        "software",
    ]);
    assert_eq!(command_name(&probe.command), "proxy.probe");
    let result = execute(&probe).unwrap();
    assert_eq!(result["policy"], "software");
    assert_eq!(result["selectedEncoder"], "software");
    assert!(
        result["hardware"][0]["status"] == "not_requested"
            || result["hardware"][0]["status"] == "unsupported_platform"
    );
    assert!(!catalog_path.exists());
}

#[test]
fn proxy_generate_accepts_explicit_encoder_policy() {
    let generate = parse(&[
        "rcdvlog",
        "--catalog",
        "/synthetic/catalog.sqlite3",
        "proxy",
        "generate",
        "--asset",
        "asset_synthetic",
        "--policy",
        "software",
    ]);
    assert_eq!(command_name(&generate.command), "proxy.generate");
}

#[test]
fn doctor_does_not_create_the_selected_catalog() {
    let directory = tempdir().unwrap();
    let catalog_path = directory.path().join("not-created.sqlite3");
    let catalog_arg = catalog_path.to_string_lossy();
    let doctor = parse(&["rcdvlog", "--catalog", &catalog_arg, "doctor"]);
    let result = execute(&doctor).unwrap();
    assert_eq!(result["catalog"], catalog_arg.as_ref());
    assert!(!catalog_path.exists());
}

#[test]
fn catalog_inspection_reports_pending_schema_without_migrating() {
    let directory = tempdir().unwrap();
    let catalog_path = directory.path().join("legacy.sqlite3");
    let connection = Connection::open(&catalog_path).unwrap();
    connection
        .execute_batch("CREATE TABLE projects (id TEXT); CREATE TABLE assets (id TEXT);")
        .unwrap();
    drop(connection);
    let catalog_arg = catalog_path.to_string_lossy();
    let inspect = parse(&["rcdvlog", "--catalog", &catalog_arg, "catalog", "inspect"]);
    let result = execute(&inspect).unwrap();
    assert_eq!(result["readOnly"], true);
    assert_eq!(result["schemaHistoryPresent"], false);
    assert_eq!(result["currentSchemaVersion"], 0);
    assert_eq!(
        result["latestSupportedSchemaVersion"],
        rcdvlog_core::PROJECT_CATALOG_SCHEMA_VERSION
    );
    assert_eq!(result["schemaStatus"], "legacy_unversioned");
    assert_eq!(
        result["pendingMigrations"].as_array().unwrap().len(),
        rcdvlog_core::PROJECT_CATALOG_SCHEMA_VERSION as usize
    );
    assert!(
        result["pendingAssetColumns"]
            .as_array()
            .unwrap()
            .iter()
            .any(|column| column == "orientation_probed")
    );
    let columns = Connection::open(&catalog_path)
        .unwrap()
        .prepare("PRAGMA table_info(assets)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(!columns.iter().any(|column| column == "orientation_probed"));
    assert_eq!(
        Connection::open(&catalog_path)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn reports_stable_error_codes_and_requires_explicit_project_arguments() {
    assert_eq!(error_code("asset not found"), "not_found");
    // An agent's response to a stale write is to re-read and retry, which
    // no other failure calls for; the code has to say which one this is.
    assert_eq!(
        error_code("stale asset revision: expected revision 3, catalog is at 5"),
        "stale_revision",
    );
    assert_eq!(error_code("original is offline"), "media_offline");
    assert_eq!(error_code("invalid source range"), "invalid_input");
    assert_eq!(error_code("ffmpeg failed"), "media_tool_failed");
    assert_eq!(
        error_code("catalog schema version 5 is newer than supported version 4"),
        "schema_incompatible"
    );
    assert_eq!(error_code("database locked"), "internal_error");
    assert!(Cli::try_parse_from(["rcdvlog", "clips", "list"]).is_err());
    assert!(Cli::try_parse_from(["rcdvlog", "project", "status"]).is_err());
    assert!(Cli::try_parse_from(["rcdvlog", "camera-mappings", "list"]).is_err());
}

#[test]
fn server_command_families_have_stable_noninteractive_clap_shapes() {
    for suffix in [
        vec!["status"],
        vec!["projects", "--limit", "10"],
        vec!["library", "list"],
        vec!["library", "diagnose", "--media-object", "media_1"],
        vec!["storage", "list"],
        vec!["storage", "diagnose", "--root", "root_1"],
        vec!["scans", "list"],
        vec!["jobs", "diagnose", "--job", "job_1"],
        vec!["sync", "status"],
        vec!["representations", "status", "--representation", "rep_1"],
        vec!["credentials", "list"],
        vec!["backups", "diagnose", "--backup", "backup_1"],
    ] {
        let mut arguments = vec![
            "rcdvlog",
            "server",
            "--endpoint",
            "https://server.example.test",
            "--credential-file",
            "/tmp/credential",
        ];
        arguments.extend(suffix);
        assert!(Cli::try_parse_from(arguments).is_ok());
    }
    assert!(
        Cli::try_parse_from([
            "rcdvlog",
            "server",
            "--endpoint",
            "https://server.example.test",
            "status"
        ])
        .is_ok()
    );
    assert!(Cli::try_parse_from(["rcdvlog", "server", "status"]).is_err());
}
