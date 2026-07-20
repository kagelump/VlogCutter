use anyhow::{Context, Result};
use clap::Parser;
use rcdvlog_server::{HttpState, ShutdownCoordinator, bind_loopback, router, serve_until_shutdown};
use rcdvlog_server_core::{
    APPLICATION_VERSION, CONTROL_CATALOG_SCHEMA_VERSION, CatalogSchemaError, ConfigError,
    InstanceLockError, PROTOCOL_MAJOR_VERSION, PROTOCOL_MINOR_VERSION, RuntimeFoundation,
    ServerConfig,
};
use serde::Serialize;
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

async fn run() -> Result<()> {
    let config = ServerConfig::from_environment().context("load server configuration")?;
    tracing::info!(
        component = "runtime",
        config = ?config.diagnostics(),
        "configuration_validated"
    );
    let foundation = Arc::new(
        RuntimeFoundation::initialize(config).context("initialize server runtime foundation")?,
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

    let result = serve_until_shutdown(
        listener,
        router(HttpState::new(foundation.clone())),
        shutdown,
        foundation.config().shutdown_timeout,
    )
    .await;

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
