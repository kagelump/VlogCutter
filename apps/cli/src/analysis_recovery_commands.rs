//! Thin CLI adapter for canonical analysis accounting inspection,
//! reconciliation and explicitly authorized targeted retry.
//!
//! The Rust core owns ownership checks, durable authorization, reservation,
//! transport lifecycle and receipt settlement. This module only maps explicit
//! command arguments to those operations and emits their stable JSON results.

use anyhow::{Result, ensure};
use clap::{ArgGroup, Args, Subcommand};
use rcdvlog_core::{
    AnalysisRequestReconciliation, Catalog, read_provider_receipt_evidence_file,
    read_targeted_retry_payload_file,
};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum AnalysisAccountingCommand {
    /// Inspect exact accounting scopes, totals, request history and receipts.
    Inspect(InspectArgs),
    /// Reconcile one interrupted request from trusted evidence without network I/O.
    Reconcile(ReconcileArgs),
    /// Durably authorize the next bounded epoch for one particular prior request.
    RetryAuthorize(RetryAuthorizeArgs),
    /// Dispatch exactly one explicitly authorized original paid request.
    TargetedRetryDispatch(TargetedRetryDispatchArgs),
}

#[derive(Debug, Args)]
pub struct InspectArgs {
    #[arg(long)]
    pub project: String,
    #[arg(long)]
    pub policy: Option<String>,
    #[arg(long)]
    pub operation: Option<String>,
    #[arg(long)]
    pub run: Option<String>,
    #[arg(long)]
    pub request: Option<String>,
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
    #[arg(long, default_value_t = 0)]
    pub offset: usize,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("evidence")
        .required(true)
        .multiple(false)
        .args(["receipt_file", "local_no_upload", "unknown_outcome"])
))]
pub struct ReconcileArgs {
    #[arg(long)]
    pub project: String,
    #[arg(long)]
    pub request: String,
    /// Trusted user-provided typed provider receipt evidence JSON.
    #[arg(long)]
    pub receipt_file: Option<PathBuf>,
    /// Reliable local evidence that no upload occurred. Fails closed if the
    /// durable dispatch marker is present.
    #[arg(long)]
    pub local_no_upload: bool,
    /// Retain an honest unknown-usage reserve when evidence is insufficient.
    #[arg(long)]
    pub unknown_outcome: bool,
    /// Stable bounded failure category; defaults are local/unknown specific.
    #[arg(long)]
    pub failure_code: Option<String>,
}

#[derive(Debug, Args)]
pub struct RetryAuthorizeArgs {
    #[arg(long)]
    pub project: String,
    #[arg(long)]
    pub request: String,
}

#[derive(Debug, Args)]
pub struct TargetedRetryDispatchArgs {
    #[arg(long)]
    pub project: String,
    #[arg(long)]
    pub request: String,
    /// Exact next epoch shown by inspect after explicit retry authorization.
    #[arg(long)]
    pub expected_next_attempt_epoch: u32,
    /// User-supplied exact original `VlmRequest` JSON payload. It is never
    /// persisted or reconstructed from the stored payload fingerprint.
    #[arg(long)]
    pub payload_file: PathBuf,
}

pub fn command_name(command: &AnalysisAccountingCommand) -> &'static str {
    match command {
        AnalysisAccountingCommand::Inspect(_) => "analyze.accounting.inspect",
        AnalysisAccountingCommand::Reconcile(_) => "analyze.accounting.reconcile",
        AnalysisAccountingCommand::RetryAuthorize(_) => "analyze.accounting.retry-authorize",
        AnalysisAccountingCommand::TargetedRetryDispatch(_) => {
            "analyze.accounting.targeted-retry-dispatch"
        }
    }
}

pub fn is_read_only(command: &AnalysisAccountingCommand) -> bool {
    matches!(command, AnalysisAccountingCommand::Inspect(_))
}

pub fn execute(catalog: &Catalog, command: &AnalysisAccountingCommand) -> Result<Value> {
    match command {
        AnalysisAccountingCommand::Inspect(args) => {
            Ok(serde_json::to_value(catalog.inspect_analysis_accounting(
                &args.project,
                args.policy.as_deref(),
                args.operation.as_deref(),
                args.run.as_deref(),
                args.request.as_deref(),
                args.limit,
                args.offset,
            )?)?)
        }
        AnalysisAccountingCommand::Reconcile(args) => {
            let evidence = if let Some(path) = &args.receipt_file {
                ensure!(
                    !args.local_no_upload && !args.unknown_outcome,
                    "receipt evidence is mutually exclusive with local-no-upload and unknown-outcome"
                );
                AnalysisRequestReconciliation::LateReceipt {
                    receipt: read_provider_receipt_evidence_file(path)?,
                }
            } else if args.local_no_upload {
                AnalysisRequestReconciliation::LocalNoUpload {
                    failure_code: args
                        .failure_code
                        .clone()
                        .unwrap_or_else(|| "local_operator_confirmed_no_upload".into()),
                }
            } else {
                ensure!(
                    args.unknown_outcome,
                    "one reconciliation evidence mode is required"
                );
                AnalysisRequestReconciliation::UncertainOutcome {
                    failure_code: args
                        .failure_code
                        .clone()
                        .unwrap_or_else(|| "operator_reported_unknown_outcome".into()),
                }
            };
            Ok(serde_json::to_value(
                catalog.reconcile_analysis_request_for_project(
                    &args.project,
                    &args.request,
                    &evidence,
                )?,
            )?)
        }
        AnalysisAccountingCommand::RetryAuthorize(args) => Ok(serde_json::to_value(
            catalog.authorize_analysis_request_retry_for_project(&args.project, &args.request)?,
        )?),
        AnalysisAccountingCommand::TargetedRetryDispatch(args) => {
            let payload = read_targeted_retry_payload_file(&args.payload_file)?;
            Ok(serde_json::to_value(
                catalog.dispatch_targeted_analysis_retry(
                    &args.project,
                    &args.request,
                    args.expected_next_attempt_epoch,
                    &payload,
                )?,
            )?)
        }
    }
}
