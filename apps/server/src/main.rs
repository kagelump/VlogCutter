use anyhow::{Context, Result};
use clap::{ArgAction, Parser, Subcommand};
use rcdvlog_server::{
    HttpState, ShutdownCoordinator, bind_loopback, openapi_document, router, serve_until_shutdown,
};
use rcdvlog_server_core::{
    APPLICATION_VERSION, BackupJobHandler, BackupService, CONTROL_CATALOG_SCHEMA_VERSION,
    CatalogSchemaError, ConfigError, InstanceLockError, JobScheduler, JobSchedulerConfig,
    PROTOCOL_MAJOR_VERSION, PROTOCOL_MINOR_VERSION, RepresentationJobHandler, RuntimeFoundation,
    ScanJobHandler, ScanService, ScanSource, ScanToolConfig, ServerConfig, ServerJobHandler,
    compute_next_run_utc,
};
use serde::Serialize;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "rcdvlog-server",
    version,
    about = "Private companion server foundation for RCD VlogCutter"
)]
struct Arguments {
    #[arg(
        long,
        help = "Print application, protocol, and control-schema versions as JSON"
    )]
    version_json: bool,
    #[arg(long, help = "Print the Rust-owned OpenAPI contract as JSON")]
    openapi_json: bool,
    #[command(subcommand)]
    command: Option<LocalCommand>,
}

#[derive(Debug, Subcommand)]
enum LocalCommand {
    #[command(about = "Inspect a restore point without changing server state")]
    RestoreInspect {
        #[arg(long)]
        restore_point: PathBuf,
    },
    #[command(about = "Restore a verified point into a new data directory")]
    RestoreApply {
        #[arg(long)]
        restore_point: PathBuf,
        #[arg(long)]
        new_data_root: PathBuf,
        #[arg(long, action = ArgAction::SetTrue, required = true)]
        confirm_new_data_root: bool,
    },
    #[command(about = "Write a redacted local diagnostic bundle")]
    DiagnosticBundle {
        #[arg(long)]
        output_directory: PathBuf,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionOutput {
    application_version: &'static str,
    protocol_major_version: u32,
    protocol_minor_version: u32,
    control_schema_version: u32,
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    if arguments.version_json {
        let output = VersionOutput {
            application_version: APPLICATION_VERSION,
            protocol_major_version: PROTOCOL_MAJOR_VERSION,
            protocol_minor_version: PROTOCOL_MINOR_VERSION,
            control_schema_version: CONTROL_CATALOG_SCHEMA_VERSION,
        };
        println!(
            "{}",
            serde_json::to_string(&output).expect("serialize static version output")
        );
        return ExitCode::SUCCESS;
    }
    if arguments.openapi_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&openapi_document())
                .expect("serialize static OpenAPI document")
        );
        return ExitCode::SUCCESS;
    }
    if let Some(command) = arguments.command {
        initialize_logging();
        return match run_local_command(command) {
            Ok(value) => {
                println!(
                    "{}",
                    serde_json::to_string(&value).expect("serialize local command")
                );
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!(
                    "{}",
                    serde_json::json!({"ok":false,"error":{"code":"local_admin_failed","message":error.to_string()}})
                );
                ExitCode::FAILURE
            }
        };
    }
    initialize_logging();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(
                component = "runtime",
                code = "server_runtime_failed",
                error = %error,
                "tokio_runtime_initialization_failed"
            );
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(run());
    // A timed-out spawn_blocking readiness check may still be blocked in the kernel. Do not let
    // Tokio's default blocking-pool join extend the already bounded process shutdown.
    runtime.shutdown_timeout(Duration::ZERO);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let metadata = startup_error_metadata(&error);
            tracing::error!(
                component = "runtime",
                code = metadata.code,
                errorField = metadata.field.unwrap_or(""),
                action = metadata.action.unwrap_or(""),
                error = %format!("{error:#}"),
                "server_start_or_runtime_failed"
            );
            ExitCode::FAILURE
        }
    }
}

fn run_local_command(command: LocalCommand) -> Result<serde_json::Value> {
    let config = ServerConfig::from_environment().context("load server configuration")?;
    match command {
        LocalCommand::RestoreInspect { restore_point } => {
            let inspection = rcdvlog_server_core::BackupManager::inspect_restore_point_in_root(
                &config.backup_dir,
                &restore_point,
            )?;
            Ok(serde_json::json!({"ok":true,"result":inspection}))
        }
        LocalCommand::RestoreApply {
            restore_point,
            new_data_root,
            confirm_new_data_root,
        } => {
            anyhow::ensure!(
                confirm_new_data_root,
                "explicit restore confirmation is required"
            );
            let foundation = RuntimeFoundation::initialize(config)
                .context("initialize offline restore foundation")?;
            let result = foundation
                .catalog()
                .backups(foundation.config().clone())
                .restore_to_new_data_root(&restore_point, &new_data_root)?;
            Ok(serde_json::json!({"ok":true,"result":result}))
        }
        LocalCommand::DiagnosticBundle { output_directory } => {
            let foundation = RuntimeFoundation::initialize(config)
                .context("initialize offline diagnostic foundation")?;
            let result = foundation
                .catalog()
                .backups(foundation.config().clone())
                .generate_diagnostic_bundle(&output_directory, foundation.readiness())?;
            Ok(serde_json::json!({"ok":true,"result":result}))
        }
    }
}

async fn run() -> Result<()> {
    let config = ServerConfig::from_environment().context("load server configuration")?;
    tracing::info!(
        component = "runtime",
        config = ?config.diagnostics(),
        "configuration_validated"
    );
    let foundation = Arc::new(
        RuntimeFoundation::initialize(config.clone())
            .context("initialize server runtime foundation")?,
    );
    let listener = bind_loopback(foundation.config().listen_addr).await?;
    let local_address = listener.local_addr()?;
    tracing::info!(
        component = "runtime",
        nodeId = %foundation.catalog().node_id(),
        applicationVersion = APPLICATION_VERSION,
        protocolMajor = PROTOCOL_MAJOR_VERSION,
        protocolMinor = PROTOCOL_MINOR_VERSION,
        controlSchemaVersion = CONTROL_CATALOG_SCHEMA_VERSION,
        listenAddress = %local_address,
        "server_started"
    );

    let shutdown = ShutdownCoordinator::new();
    let signal_shutdown = shutdown.clone();
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        tracing::info!(component = "runtime", "shutdown_requested");
        signal_shutdown.cancel();
    });

    // ── Startup scan recovery ──────────────────────────────────────
    {
        let recovery_library = foundation.catalog().storage_library();
        let recovery_tool_config = ScanToolConfig {
            ffprobe_program: std::path::PathBuf::from(&config.ffprobe_command),
            exiftool_program: if config.exiftool_command.is_empty() {
                None
            } else {
                Some(std::path::PathBuf::from(&config.exiftool_command))
            },
        };
        let recovery_service = ScanService::new(recovery_library, recovery_tool_config);
        match recovery_service.recover_active_scan_jobs() {
            Ok(report) => {
                tracing::info!(
                    component = "recovery",
                    total_pending = report.total_pending,
                    recovered = report.recovered,
                    skipped = report.skipped,
                    "startup_scan_recovery_complete"
                );
            }
            Err(e) => {
                tracing::error!(
                    component = "recovery",
                    error = %e,
                    "startup_scan_recovery_failed"
                );
            }
        }
    }

    // ── Scan workers ────────────────────────────────────────────────
    let worker_catalog = foundation.catalog().clone();
    let worker_shutdown = shutdown.clone();
    let worker_config = foundation.config().clone();
    let scan_handle = tokio::spawn(async move {
        let tool_config = ScanToolConfig {
            ffprobe_program: std::path::PathBuf::from(&worker_config.ffprobe_command),
            exiftool_program: if worker_config.exiftool_command.is_empty() {
                None
            } else {
                Some(std::path::PathBuf::from(&worker_config.exiftool_command))
            },
        };
        let library = worker_catalog.storage_library();
        let scan_service = Arc::new(ScanService::new(library.clone(), tool_config));
        let scan_handler = ScanJobHandler::new(library.clone(), scan_service.clone());
        let representation_handler = RepresentationJobHandler::new(
            library,
            worker_catalog.jobs(),
            worker_config.derived_dir.clone(),
            std::path::PathBuf::from(&worker_config.ffmpeg_command),
            std::path::PathBuf::from(&worker_config.ffprobe_command),
        );
        let backup_manager = worker_catalog.backups(worker_config.clone());
        let backup_service = Arc::new(BackupService::new(
            backup_manager.clone(),
            worker_catalog.jobs(),
        ));
        let backup_handler = BackupJobHandler::new(backup_manager);
        let handler = Arc::new(ServerJobHandler::new(
            scan_handler,
            representation_handler,
            backup_handler,
        ));

        let engine = worker_catalog.jobs();
        let scheduler = match JobScheduler::new(
            engine,
            JobSchedulerConfig::default(),
            worker_shutdown.child_token(),
        ) {
            Ok(scheduler) => scheduler,
            Err(error) => {
                tracing::error!(
                    component = "scheduler",
                    error = %error,
                    "job_scheduler_creation_failed"
                );
                return;
            }
        };

        // Start the schedule loop in a separate task
        let schedule_shutdown = worker_shutdown.clone();
        let schedule_service = scan_service.clone();
        tokio::spawn(async move {
            run_schedule_loop(schedule_service, schedule_shutdown.child_token()).await;
        });
        let backup_schedule_shutdown = worker_shutdown.clone();
        let backup_interval = worker_config.backup_interval;
        tokio::spawn(async move {
            run_backup_schedule_loop(
                backup_service,
                backup_interval,
                backup_schedule_shutdown.child_token(),
            )
            .await;
        });

        tracing::info!(component = "scheduler", "job_scheduler_starting");
        if let Err(error) = scheduler.run(handler).await {
            tracing::error!(
                component = "scheduler",
                error = %error,
                "job_scheduler_stopped_unexpectedly"
            );
        }
        tracing::info!(component = "scheduler", "job_scheduler_stopped");
    });

    let result = serve_until_shutdown(
        listener,
        router(HttpState::new(foundation.clone())),
        shutdown,
        foundation.config().shutdown_timeout,
    )
    .await;

    // Drain the scan worker (scheduler + schedule loop)
    let _ = tokio::time::timeout(Duration::from_secs(30), scan_handle).await;

    foundation
        .checkpoint()
        .context("checkpoint control catalog during shutdown")?;
    result?;
    tracing::info!(component = "runtime", "server_stopped");
    Ok(())
}

struct StartupErrorMetadata {
    code: &'static str,
    field: Option<&'static str>,
    action: Option<&'static str>,
}

fn startup_error_metadata(error: &anyhow::Error) -> StartupErrorMetadata {
    if let Some(error) = error.downcast_ref::<ConfigError>() {
        StartupErrorMetadata {
            code: error.code,
            field: Some(error.field),
            action: None,
        }
    } else if let Some(error) = error.downcast_ref::<InstanceLockError>() {
        StartupErrorMetadata {
            code: error.code,
            field: Some(error.field),
            action: Some(error.action),
        }
    } else if let Some(error) = error.downcast_ref::<CatalogSchemaError>() {
        StartupErrorMetadata {
            code: error.code(),
            field: Some("controlCatalog"),
            action: Some("upgrade RCD VlogCutter Server before opening this control catalog"),
        }
    } else {
        StartupErrorMetadata {
            code: "server_runtime_failed",
            field: None,
            action: None,
        }
    }
}

async fn run_backup_schedule_loop(
    backup_service: Arc<BackupService>,
    interval: Duration,
    shutdown: tokio_util::sync::CancellationToken,
) {
    tracing::info!(
        component = "backup_schedule",
        "backup_schedule_loop_starting"
    );
    loop {
        if shutdown.is_cancelled() {
            return;
        }
        let seconds = interval.as_secs().max(1);
        let bucket = chrono::Utc::now().timestamp().max(0) as u64 / seconds;
        let key = format!("scheduled-backup-{bucket}");
        if let Err(error) = backup_service.request_backup(&key) {
            tracing::warn!(
                component = "backup_schedule",
                error = %error,
                "scheduled_backup_request_failed"
            );
        }
        let check_interval = interval.min(Duration::from_secs(300));
        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::info!(component = "backup_schedule", "backup_schedule_loop_stopped");
                return;
            }
            _ = tokio::time::sleep(check_interval) => {}
        }
    }
}

async fn run_schedule_loop(
    scan_service: Arc<ScanService>,
    shutdown: tokio_util::sync::CancellationToken,
) {
    tracing::info!(component = "schedule", "schedule_loop_starting");
    loop {
        if shutdown.is_cancelled() {
            return;
        }
        match scan_service.list_due_schedules() {
            Ok(schedules) => {
                for schedule in schedules {
                    if shutdown.is_cancelled() {
                        return;
                    }
                    let root_id = schedule.storage_root_id.clone();
                    let scope = schedule.scope_relative_path.clone();
                    match scan_service.request_scan(
                        ScanSource::Schedule,
                        Some(&schedule.schedule_id),
                        &root_id,
                        scope.as_ref(),
                    ) {
                        Ok(scan_run) => {
                            let next =
                                compute_next_run_utc(&schedule.local_time, &schedule.timezone)
                                    .unwrap_or_else(|_| {
                                        chrono::Utc::now()
                                            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                                    });
                            let _ = scan_service.record_schedule_trigger(
                                &schedule.schedule_id,
                                &scan_run.scan_id,
                                &next,
                            );
                            tracing::info!(
                                component = "schedule",
                                scheduleId = %schedule.schedule_id,
                                scanId = %scan_run.scan_id,
                                "schedule_triggered_scan"
                            );
                        }
                        Err(error) => {
                            tracing::error!(
                                component = "schedule",
                                scheduleId = %schedule.schedule_id,
                                error = %error,
                                "schedule_scan_request_failed"
                            );
                        }
                    }
                }
            }
            Err(error) => {
                tracing::error!(
                    component = "schedule",
                    error = %error,
                    "list_due_schedules_failed"
                );
            }
        }
        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::info!(component = "schedule", "schedule_loop_stopped");
                return;
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {}
        }
    }
}

fn initialize_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_current_span(true)
        .with_span_list(true)
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();
}

#[cfg(unix)]
async fn wait_for_shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    match signal(SignalKind::terminate()) {
        Ok(mut terminate) => {
            tokio::select! {
                result = tokio::signal::ctrl_c() => {
                    if let Err(error) = result {
                        tracing::error!(component = "runtime", error = %error, "ctrl_c_handler_failed");
                    }
                }
                _ = terminate.recv() => {}
            }
        }
        Err(error) => {
            tracing::error!(component = "runtime", error = %error, "sigterm_handler_failed");
            if let Err(error) = tokio::signal::ctrl_c().await {
                tracing::error!(component = "runtime", error = %error, "ctrl_c_handler_failed");
            }
        }
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(component = "runtime", error = %error, "ctrl_c_handler_failed");
    }
}
