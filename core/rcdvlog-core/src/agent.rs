//! Read-only agent readiness and an explicit, node-local executable binding.
//! The binding is configuration, never a projection or alternate catalog.

use crate::prerequisites::run_bounded_command;
use crate::{
    MigrationHistoryStatus, ModelInstallProgress, ModelInstallReport, ModelStatus,
    PROJECT_CATALOG_SCHEMA_VERSION, ToolStatus,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::Duration;

const BINDING_NAME: &str = "cli-binding.json";
const BINDING_OWNER: &str = "rcdvlog.cli-binding";
const MAX_DOCUMENT_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliBuild {
    pub version: String,
    pub revision: Option<String>,
    pub dirty: bool,
    pub profile: String,
    pub target: String,
    pub rustc: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliExecutable {
    /// Canonical path reported by the executable actually running.
    pub path: PathBuf,
    pub build: CliBuild,
    pub capabilities: BTreeMap<String, u32>,
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogReadiness {
    pub path: Option<PathBuf>,
    pub read_only: bool,
    pub compatible: bool,
    pub current_schema_version: Option<u32>,
    pub supported_schema_version: u32,
    pub status: String,
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorReport {
    pub schema_version: u32,
    pub executable: CliExecutable,
    /// Kept for existing doctor consumers. No desktop catalog fallback.
    pub catalog: Option<PathBuf>,
    pub catalog_status: CatalogReadiness,
    pub tools: Vec<ToolStatus>,
    pub models: Vec<ModelStatus>,
    pub models_ready: bool,
    pub ready: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ReadinessRequirements {
    pub catalog: bool,
    pub tools: Vec<String>,
    pub models: bool,
    pub capabilities: Vec<String>,
    pub revision: Option<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct AgentReadinessError {
    pub code: &'static str,
    pub message: String,
    pub details: Option<Box<Value>>,
}

fn failure(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    AgentReadinessError {
        code,
        message: message.into(),
        details: None,
    }
    .into()
}

fn report_failure(code: &'static str, message: String, report: &DoctorReport) -> anyhow::Error {
    AgentReadinessError {
        code,
        message,
        details: Some(Box::new(json!(report))),
    }
    .into()
}

/// Inspect an explicit catalog without migration, directory creation, or media work.
pub fn catalog_readiness(path: Option<&Path>) -> CatalogReadiness {
    let mut result = CatalogReadiness {
        path: path.map(Path::to_path_buf), compatible: false,
        read_only: true,
        current_schema_version: None,
        supported_schema_version: PROJECT_CATALOG_SCHEMA_VERSION,
        status: "catalog_required".into(),
        remediation: Some("Pass --catalog with the intended existing project catalog; no desktop catalog is selected automatically.".into()),
    };
    let Some(path) = path else {
        return result;
    };
    if !path.is_file() {
        result.status = "catalog_missing".into();
        result.remediation = Some(
            "Select the existing project catalog explicitly. Readiness never creates a catalog."
                .into(),
        );
        return result;
    }
    match crate::inspect_catalog(path) {
        Ok(inspection) => {
            result.current_schema_version = Some(inspection.current_schema_version);
            if !inspection.missing_required_tables.is_empty()
                || (inspection.schema_status == MigrationHistoryStatus::Current
                    && (!inspection.pending_project_columns.is_empty()
                        || !inspection.pending_asset_columns.is_empty()))
            {
                result.status = "schema_incompatible".into();
            } else {
                match inspection.schema_status {
                    MigrationHistoryStatus::Current => {
                        result.compatible = true;
                        result.status = "current".into();
                        result.remediation = None;
                        return result;
                    }
                    MigrationHistoryStatus::LegacyUnversioned
                    | MigrationHistoryStatus::UpgradeAvailable => {
                        result.status = "catalog_upgrade_required".into();
                        result.remediation = Some("Back up the catalog and open it explicitly with the supported application to migrate, then re-check. Readiness never migrates.".into());
                        return result;
                    }
                    MigrationHistoryStatus::NewerUnsupported
                    | MigrationHistoryStatus::IncompatibleHistory => {
                        result.status = "schema_incompatible".into();
                    }
                }
            }
        }
        Err(_) => result.status = "schema_incompatible".into(),
    }
    result.remediation = Some("Select a compatible VlogCutter project catalog or upgrade the selected executable. No catalog repair was attempted.".into());
    result
}

/// The existing model resolver/policy, usable even before a catalog is opened.
pub fn speech_model_status(catalog_path: &Path) -> Result<Vec<ModelStatus>> {
    speech_models_in(crate::analysis::engine::model_dir(catalog_path)?)
}

fn speech_models_in(directory: PathBuf) -> Result<Vec<ModelStatus>> {
    use crate::analysis::engine::{VAD_MODEL, WHISPER_MODEL};
    Ok([
        (WHISPER_MODEL, "speech_to_text"),
        (VAD_MODEL, "voice_activity_detection"),
    ]
    .into_iter()
    .map(|(model, role)| model_status_in(&directory, &model, role))
    .collect())
}

fn model_status_in(
    directory: &Path,
    model: &crate::analysis::engine::ModelAsset,
    role: &str,
) -> ModelStatus {
    let path = directory.join(model.file_name);
    let present = path.is_file();
    let expected_blake3 = (!model.blake3.is_empty()).then(|| model.blake3.to_string());
    let (integrity, ready, detail) = if !present {
        (
            "missing",
            false,
            Some(format!(
                "not installed; expected at {} (run `rcdvlog models install` explicitly)",
                path.display()
            )),
        )
    } else if model.blake3.is_empty() {
        (
            "present_unverified",
            true,
            Some("present; this build has no pinned digest for the artifact".into()),
        )
    } else {
        match fs::read(&path) {
            Ok(bytes) => {
                let digest = blake3::hash(&bytes).to_hex().to_string();
                if digest == model.blake3 {
                    ("verified", true, None)
                } else {
                    (
                        "invalid",
                        false,
                        Some(format!(
                            "digest mismatch: expected {}, found {digest}",
                            model.blake3
                        )),
                    )
                }
            }
            Err(error) => (
                "invalid",
                false,
                Some(format!("model file is unreadable: {error}")),
            ),
        }
    };
    ModelStatus {
        name: model.file_name.to_string(),
        role: role.to_string(),
        installed: ready,
        ready,
        present,
        integrity: integrity.to_string(),
        detail,
        bytes: model.bytes,
        url: model.url.to_string(),
        directory: directory.to_string_lossy().into_owned(),
        path: path.to_string_lossy().into_owned(),
        expected_blake3,
    }
}

/// Explicitly download missing speech models into the directory this catalog
/// reads from. This is the only model-fetching path; status and transcription
/// never call it.
pub fn install_speech_models_with_progress<F>(
    catalog_path: &Path,
    on_progress: F,
) -> Result<ModelInstallReport>
where
    F: FnMut(ModelInstallProgress) -> ControlFlow<()>,
{
    use crate::analysis::engine::{VAD_MODEL, WHISPER_MODEL, model_dir};
    let directory = model_dir(catalog_path)?;
    let (cancelled, installed) =
        install_models_into(&directory, &[WHISPER_MODEL, VAD_MODEL], on_progress)?;
    Ok(ModelInstallReport {
        cancelled,
        installed,
        models: speech_model_status(catalog_path)?,
    })
}

fn install_models_into<F>(
    directory: &Path,
    models: &[crate::analysis::engine::ModelAsset],
    mut on_progress: F,
) -> Result<(bool, usize)>
where
    F: FnMut(ModelInstallProgress) -> ControlFlow<()>,
{
    use crate::analysis::engine::{install_model, model_is_installed};
    let mut cancelled = false;
    let mut installed = 0_usize;
    for model in models {
        if model_is_installed(directory, model) {
            continue;
        }
        if on_progress(ModelInstallProgress {
            name: model.file_name.to_string(),
            downloaded_bytes: 0,
            total_bytes: model.bytes,
        })
        .is_break()
        {
            return Ok((true, installed));
        }
        let mut cancelled_this_transfer = false;
        let result = install_model(directory, model, |downloaded, total| {
            let flow = on_progress(ModelInstallProgress {
                name: model.file_name.to_string(),
                downloaded_bytes: downloaded,
                total_bytes: total,
            });
            if flow.is_break() {
                cancelled_this_transfer = true;
            }
            flow
        });
        if cancelled_this_transfer {
            cancelled = true;
            break;
        }
        result?;
        installed += 1;
    }
    Ok((cancelled, installed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::engine::ModelAsset;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

    fn serve_body_once(body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = socket.read(&mut request);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(header.as_bytes());
            let _ = socket.write_all(&body);
        });
        format!("http://{address}/model.bin")
    }

    #[test]
    fn model_status_separates_presence_integrity_and_readiness() {
        let directory = tempdir().unwrap();
        let model = ModelAsset {
            file_name: "candidate.bin",
            url: "https://example.invalid/candidate.bin",
            blake3: "",
            bytes: 12,
        };
        let missing = model_status_in(directory.path(), &model, "speech_to_text");
        assert!(!missing.present);
        assert!(!missing.ready);
        assert_eq!(missing.integrity, "missing");
        assert!(missing.path.ends_with("candidate.bin"));

        fs::write(directory.path().join(model.file_name), b"hello model").unwrap();
        let present = model_status_in(directory.path(), &model, "speech_to_text");
        assert!(present.present);
        assert!(present.ready);
        assert_eq!(present.integrity, "present_unverified");

        let pinned = ModelAsset {
            blake3: "0000000000000000000000000000000000000000000000000000000000000000",
            ..model
        };
        let invalid = model_status_in(directory.path(), &pinned, "speech_to_text");
        assert!(invalid.present);
        assert!(!invalid.ready);
        assert!(!invalid.installed);
        assert_eq!(invalid.integrity, "invalid");
    }

    #[test]
    fn cancelled_install_reports_cancellation_and_removes_the_partial_file() {
        let directory = tempdir().unwrap();
        let url = serve_body_once(vec![7_u8; 4 * 1024 * 1024]);
        let leaked_url = Box::leak(url.into_boxed_str());
        let model = ModelAsset {
            file_name: "cancel.bin",
            url: leaked_url,
            blake3: "",
            bytes: 4 * 1024 * 1024,
        };
        let progress_calls = Arc::new(AtomicUsize::new(0));
        let calls = progress_calls.clone();
        let (cancelled, installed) =
            install_models_into(directory.path(), &[model], move |progress| {
                calls.fetch_add(1, Ordering::SeqCst);
                if progress.downloaded_bytes == 0 {
                    ControlFlow::Continue(())
                } else {
                    ControlFlow::Break(())
                }
            })
            .unwrap();
        assert!(cancelled);
        assert_eq!(installed, 0);
        assert_eq!(progress_calls.load(Ordering::SeqCst), 2);
        assert!(!directory.path().join("cancel.bin").exists());
        assert!(!directory.path().join("cancel.bin.partial").exists());
    }

    #[test]
    fn installation_can_cancel_before_network_or_directory_creation() {
        let directory = tempdir().unwrap();
        let missing = directory.path().join("models");
        let model = ModelAsset {
            file_name: "cancel.bin",
            url: "https://example.invalid/must-not-be-requested",
            blake3: "",
            bytes: 4,
        };
        assert_eq!(
            install_models_into(&missing, &[model], |_| ControlFlow::Break(())).unwrap(),
            (true, 0)
        );
        assert!(!missing.exists());
    }

    #[test]
    fn installation_verifies_then_reuses_a_completed_download() {
        let directory = tempdir().unwrap();
        let body = b"synthetic model".to_vec();
        let digest = blake3::hash(&body).to_hex().to_string();
        let url = serve_body_once(body.clone());
        let model = ModelAsset {
            file_name: "installed.bin",
            url: Box::leak(url.into_boxed_str()),
            blake3: Box::leak(digest.into_boxed_str()),
            bytes: body.len() as u64,
        };
        assert_eq!(
            install_models_into(directory.path(), &[model], |_| ControlFlow::Continue(())).unwrap(),
            (false, 1)
        );
        let status = model_status_in(directory.path(), &model, "speech_to_text");
        assert!(status.ready);
        assert_eq!(status.integrity, "verified");
        assert_eq!(
            install_models_into(directory.path(), &[model], |_| panic!(
                "must reuse the model"
            ))
            .unwrap(),
            (false, 0)
        );
        assert_eq!(
            fs::read(directory.path().join(model.file_name)).unwrap(),
            body
        );
    }
}

pub fn doctor_report(executable: CliExecutable, catalog: Option<&Path>) -> Result<DoctorReport> {
    let catalog_status = catalog_readiness(catalog);
    let models = match catalog {
        Some(path) => speech_model_status(path)?,
        None => speech_models_in(crate::analysis::engine::shared_model_dir()?)?,
    };
    let models_ready = models.iter().all(|model| model.installed);
    let tools = crate::prerequisite_status();
    let ready =
        catalog_status.compatible && models_ready && tools.iter().all(|tool| tool.supported);
    Ok(DoctorReport {
        schema_version: 1,
        executable,
        catalog: catalog.map(Path::to_path_buf),
        catalog_status,
        tools,
        models,
        models_ready,
        ready,
    })
}

/// Fail before scheduling work, with the inspected report on the error envelope.
pub fn ensure_agent_readiness(
    report: &DoctorReport,
    required: &ReadinessRequirements,
) -> Result<()> {
    for capability in &required.capabilities {
        if !report.executable.capabilities.contains_key(capability)
            && !report
                .executable
                .commands
                .iter()
                .any(|command| command == capability)
        {
            return Err(report_failure(
                "capability_unavailable",
                format!(
                    "Selected executable does not support {capability}. Build the documented pinned CLI and explicitly refresh the binding."
                ),
                report,
            ));
        }
    }
    if let Some(revision) = &required.revision
        && report.executable.build.revision.as_ref() != Some(revision)
    {
        return Err(report_failure("revision_mismatch", "Selected executable was built from a different revision. Rebuild the intended revision and explicitly refresh the binding.".into(), report));
    }
    if required.catalog && !report.catalog_status.compatible {
        let code = match report.catalog_status.status.as_str() {
            "catalog_required" => "catalog_required",
            "catalog_missing" => "catalog_missing",
            "catalog_upgrade_required" => "catalog_upgrade_required",
            _ => "schema_incompatible",
        };
        return Err(report_failure(
            code,
            report
                .catalog_status
                .remediation
                .clone()
                .unwrap_or_default(),
            report,
        ));
    }
    for name in &required.tools {
        let Some(tool) = report.tools.iter().find(|tool| &tool.name == name) else {
            return Err(report_failure(
                "capability_unavailable",
                format!(
                    "No prerequisite probe is supported for {name}; use ffmpeg, ffprobe, or exiftool."
                ),
                report,
            ));
        };
        if !tool.supported {
            return Err(report_failure(
                "media_prerequisite_failed",
                format!(
                    "{}: {} {}",
                    tool.name,
                    tool.reason.as_deref().unwrap_or("unsupported"),
                    tool.remediation
                        .as_deref()
                        .unwrap_or("Re-check the selected tool.")
                ),
                report,
            ));
        }
    }
    if required.models && !report.models_ready {
        let missing = report
            .models
            .iter()
            .filter(|model| !model.installed)
            .map(|model| {
                format!(
                    "{} ({} bytes) in {}",
                    model.name, model.bytes, model.directory
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        return Err(report_failure(
            "model_unavailable",
            format!(
                "Speech models are not installed: {missing}. Install explicitly through Settings, then re-check; readiness never downloads models."
            ),
            report,
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliSelection {
    Installed,
    Development,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CliBinding {
    pub owner: String,
    pub schema_version: u32,
    pub root: PathBuf,
    pub catalog: PathBuf,
    pub selection: CliSelection,
    /// The path the caller selected, retained so retargeted symlinks are detected.
    pub selected_path: PathBuf,
    pub executable: CliExecutable,
    pub blake3: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BindingCheck {
    pub binding_path: PathBuf,
    pub binding: CliBinding,
    pub readiness: DoctorReport,
    /// Structured argv; the caller must not interpolate these into a shell string.
    pub invocation: Vec<String>,
}

fn canonical(path: &Path, code: &'static str, kind: &str) -> Result<PathBuf> {
    fs::canonicalize(path).map_err(|_| failure(code, format!("{kind} is unavailable at {}. Select the intended existing path and explicitly refresh the binding.", path.display())))
}

fn binding_path(root: &Path) -> Result<PathBuf> {
    let directory = root.join(".vlogcutter");
    if let Ok(metadata) = fs::symlink_metadata(&directory)
        && !metadata.is_dir()
    {
        return Err(failure(
            "binding_invalid",
            "Managed .vlogcutter path must be a directory, without a symlink. Preserve the existing file and select a safe project workspace.",
        ));
    }
    for path in [&directory, &directory.join(BINDING_NAME)] {
        if let Ok(metadata) = fs::symlink_metadata(path)
            && metadata.file_type().is_symlink()
        {
            return Err(failure(
                "binding_invalid",
                "Managed binding paths must not be symlinks; preserve the existing path and select a safe project workspace.",
            ));
        }
    }
    Ok(directory.join(BINDING_NAME))
}

fn read_binding(path: &Path) -> Result<CliBinding> {
    if !path.is_file() {
        return Err(failure(
            "binding_missing",
            "No regular CLI binding file exists. Run workspace refresh explicitly.",
        ));
    }
    let mut file = File::open(path).map_err(|_| failure("binding_missing", "No readable CLI binding exists. Run workspace refresh with explicit --catalog, --executable and --selection."))?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_DOCUMENT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(failure(
            "binding_invalid",
            "CLI binding exceeds its bounded document size; it was not changed.",
        ));
    }
    let binding: CliBinding = serde_json::from_slice(&bytes)
        .map_err(|_| failure("binding_invalid", "CLI binding is invalid; preserve it before explicitly repairing the managed binding. Project instructions were not changed."))?;
    if binding.owner != BINDING_OWNER
        || binding.schema_version != 1
        || !binding.root.is_absolute()
        || !binding.catalog.is_absolute()
        || !binding.selected_path.is_absolute()
        || !binding.executable.path.is_absolute()
        || binding.blake3.len() != 64
        || !binding.blake3.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(failure(
            "binding_invalid",
            "Unrecognized CLI binding owner/schema; the file was not changed.",
        ));
    }
    Ok(binding)
}

fn executable_hash(path: &Path) -> Result<String> {
    if !path.is_file() {
        return Err(failure(
            "executable_unavailable",
            "Selected executable must be a regular file; no fallback was attempted.",
        ));
    }
    let mut file = File::open(path).map_err(|_| failure("executable_unavailable", "Cannot read the selected executable; repair its path or permissions and explicitly refresh."))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn probe_selected(path: &Path, catalog: &Path) -> Result<DoctorReport> {
    let output = run_bounded_command(path, &[
        OsString::from("--catalog"), catalog.as_os_str().to_owned(), OsString::from("doctor"),
    ], Duration::from_secs(20), MAX_DOCUMENT_BYTES as usize)
        .map_err(|error| failure("executable_unavailable", format!("Selected executable could not complete a bounded doctor check: {error}. Rebuild using the pinned bootstrap and explicitly refresh.")))?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(failure(
            "executable_unavailable",
            "Selected executable did not return the supported doctor contract. Rebuild using the pinned bootstrap; no alternative executable was selected.",
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        failure(
            "capability_unavailable",
            "Selected executable returned invalid doctor JSON; upgrade it and explicitly refresh.",
        )
    })?;
    if value["ok"] != true || value["command"] != "doctor" {
        return Err(failure(
            "capability_unavailable",
            "Selected executable does not support the required doctor envelope.",
        ));
    }
    let report: DoctorReport = serde_json::from_value(value["data"].clone())
        .map_err(|_| failure("capability_unavailable", "Selected executable lacks versioned executable/readiness metadata. Build the supported CLI and explicitly refresh."))?;
    if report.schema_version != 1
        || report.executable.path != path
        || report.catalog.as_deref() != Some(catalog)
        || report.executable.commands.len() > 256
        || report.executable.capabilities.len() > 64
        || report.tools.len() != 3
        || report.models.len() != 2
    {
        return Err(failure(
            "capability_unavailable",
            "Selected executable reported incompatible or inconsistent path/catalog/readiness metadata.",
        ));
    }
    Ok(report)
}

fn checked_result(
    path: PathBuf,
    binding: CliBinding,
    required: &ReadinessRequirements,
) -> Result<BindingCheck> {
    if canonical(
        &binding.selected_path,
        "executable_unavailable",
        "Selected executable",
    )? != binding.executable.path
        || executable_hash(&binding.selected_path)? != binding.blake3
    {
        return Err(failure(
            "binding_stale",
            "The selected executable moved, was rebuilt, or its symlink changed. Run workspace refresh explicitly; no fallback was attempted.",
        ));
    }
    let report = probe_selected(&binding.executable.path, &binding.catalog)?;
    if report.executable != binding.executable
        || executable_hash(&binding.selected_path)? != binding.blake3
    {
        return Err(failure(
            "binding_stale",
            "Executable identity changed during verification. Re-check the selected build and explicitly refresh.",
        ));
    }
    let mut required = required.clone();
    required.catalog = true;
    ensure_agent_readiness(&report, &required)?;
    let invocation = vec![
        binding.selected_path.to_string_lossy().into_owned(),
        "--catalog".into(),
        binding.catalog.to_string_lossy().into_owned(),
    ];
    Ok(BindingCheck {
        binding_path: path,
        binding,
        readiness: report,
        invocation,
    })
}

/// Refresh only the managed node-local binding. Never touch AGENTS.md/PROJECT.md.
pub fn refresh_cli_binding(
    root: &Path,
    catalog: &Path,
    executable: &Path,
    selection: CliSelection,
    required: &ReadinessRequirements,
) -> Result<BindingCheck> {
    if !executable.is_absolute() || !catalog.is_absolute() {
        return Err(failure(
            "invalid_arguments",
            "workspace refresh requires absolute --executable and --catalog paths.",
        ));
    }
    let root = canonical(root, "binding_invalid", "Workspace root")?;
    if !root.is_dir() {
        return Err(failure(
            "binding_invalid",
            "Workspace root must be an existing directory.",
        ));
    }
    let path = binding_path(&root)?;
    if path.exists() {
        read_binding(&path)?;
    } // Never overwrite an unrecognized file.
    let catalog = canonical(catalog, "catalog_missing", "Catalog")?;
    let resolved = canonical(executable, "executable_unavailable", "Selected executable")?;
    let digest = executable_hash(&resolved)?;
    let report = probe_selected(&resolved, &catalog)?;
    let binding = CliBinding {
        owner: BINDING_OWNER.into(),
        schema_version: 1,
        root,
        catalog,
        selection,
        selected_path: executable.to_path_buf(),
        executable: report.executable,
        blake3: digest,
    };
    let result = checked_result(path.clone(), binding, required)?;
    fs::create_dir_all(path.parent().expect("binding parent"))?;
    let temporary = path.with_file_name(format!(".cli-binding-{}.tmp", uuid::Uuid::new_v4()));
    let publish = || -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(&result.binding)?)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        // Recheck managed path safety before atomic publication.
        binding_path(&result.binding.root)?;
        fs::rename(&temporary, &path)?;
        Ok(())
    };
    let outcome = publish();
    if outcome.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    outcome?;
    Ok(result)
}

pub fn check_cli_binding(
    root: &Path,
    explicit_catalog: Option<&Path>,
    required: &ReadinessRequirements,
) -> Result<BindingCheck> {
    let root = canonical(root, "binding_invalid", "Workspace root")?;
    let path = binding_path(&root)?;
    let binding = read_binding(&path)?;
    if binding.root != root {
        return Err(failure(
            "binding_stale",
            "Project workspace moved. Run workspace refresh with its intended executable and catalog.",
        ));
    }
    if let Some(catalog) = explicit_catalog
        && canonical(catalog, "catalog_missing", "Catalog")? != binding.catalog
    {
        return Err(failure(
            "binding_catalog_mismatch",
            "Explicit catalog differs from the project binding. Refresh explicitly to choose a different catalog.",
        ));
    }
    checked_result(path, binding, required)
}
