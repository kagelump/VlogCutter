//! Thin CLI adapter for explicit, bounded preparation workflows.

use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand, ValueEnum};
use rcdvlog_core::{Catalog, PreparationRunOptions, PreparationScope};
use serde_json::{Value, json};
use std::ops::ControlFlow;

#[derive(Debug, Subcommand)]
pub enum PreparationCommand {
    /// Inspect a bounded preparation manifest without migrations or writes.
    Preflight(PageArgs),
    /// Run explicitly selected local preparation stages.
    Run(RunArgs),
    /// Read bounded transcript or moment evidence for one selected asset.
    Evidence(EvidenceArgs),
}

#[derive(Debug, Args, Clone)]
pub struct ScopeArgs {
    #[arg(long)]
    project: String,
    #[arg(
        long = "asset",
        required_unless_present = "all_assets",
        conflicts_with = "all_assets"
    )]
    assets: Vec<String>,
    /// Select all project assets, including excluded assets.
    #[arg(long = "all")]
    all_assets: bool,
}

#[derive(Debug, Args)]
pub struct PageArgs {
    #[command(flatten)]
    scope: ScopeArgs,
    #[arg(long, default_value_t = 0)]
    offset: usize,
    #[arg(long, default_value_t = 10)]
    limit: usize,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[command(flatten)]
    scope: ScopeArgs,
    #[arg(long)]
    proxies: bool,
    #[arg(long)]
    transcribe: bool,
    #[arg(long)]
    revalidate: bool,
    /// Explicitly authorize the bounded paid moments stage.
    #[arg(long)]
    moments: bool,
    #[arg(long, value_parser = ["openrouter"])]
    provider: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long, value_parser = ["frames", "frames-and-transcript"])]
    payload: Option<String>,
    #[arg(long)]
    budget_usd: Option<f64>,
    /// Execute the exact paid moments policy and matching passed pilot.
    #[arg(long)]
    policy_id: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum EvidenceKind {
    Transcript,
    Moments,
}

#[derive(Debug, Args)]
pub struct EvidenceArgs {
    #[arg(long)]
    project: String,
    #[arg(long)]
    asset: String,
    #[arg(long, value_enum)]
    kind: EvidenceKind,
    #[arg(long, default_value_t = 0)]
    offset: usize,
    #[arg(long, default_value_t = 10)]
    limit: usize,
}

pub fn command_name(command: &PreparationCommand) -> &'static str {
    match command {
        PreparationCommand::Preflight(_) => "prepare.preflight",
        PreparationCommand::Run(_) => "prepare.run",
        PreparationCommand::Evidence(_) => "prepare.evidence",
    }
}

pub fn is_read_only(command: &PreparationCommand) -> bool {
    !matches!(command, PreparationCommand::Run(_))
}

fn scope(args: &ScopeArgs) -> PreparationScope {
    PreparationScope {
        project_id: args.project.clone(),
        asset_ids: args.assets.clone(),
        all_assets: args.all_assets,
    }
}

fn validate_page(offset: usize, limit: usize) -> Result<()> {
    ensure!(
        offset <= 1_000_000 && (1..=50).contains(&limit),
        "invalid page: offset must be <= 1000000 and limit must be 1..=50"
    );
    Ok(())
}

pub fn execute(catalog: &Catalog, command: &PreparationCommand) -> Result<Value> {
    match command {
        PreparationCommand::Preflight(args) => {
            validate_page(args.offset, args.limit)?;
            Ok(serde_json::to_value(catalog.preparation_preflight(
                &scope(&args.scope),
                args.offset,
                args.limit,
            )?)?)
        }
        PreparationCommand::Evidence(args) => {
            validate_page(args.offset, args.limit)?;
            ensure!(
                !args.project.trim().is_empty(),
                "preparation project is required"
            );
            let asset = catalog.get_asset(&args.asset)?;
            ensure!(
                asset.project_id == args.project,
                "asset does not belong to selected project"
            );
            match args.kind {
                EvidenceKind::Transcript => Ok(serde_json::to_value(
                    catalog.preparation_transcript_page(&args.asset, args.offset, args.limit)?,
                )?),
                EvidenceKind::Moments => Ok(serde_json::to_value(
                    catalog
                        .moment_evidence_page(&args.asset, args.offset, args.limit)?
                        .context("no moment evidence is stored for this asset")?,
                )?),
            }
        }
        PreparationCommand::Run(args) => {
            let policy_count = usize::from(args.provider.is_some())
                + usize::from(args.model.is_some())
                + usize::from(args.payload.is_some())
                + usize::from(args.budget_usd.is_some());
            ensure!(
                args.moments || policy_count == 0,
                "invalid input: moment policy flags require --moments"
            );
            ensure!(
                args.policy_id.is_none() || args.moments,
                "invalid input: --policy-id requires --moments"
            );
            let moments = if let Some(policy_id) = &args.policy_id {
                ensure!(
                    !args.scope.all_assets,
                    "invalid input: an authorized policy requires an explicit --asset scope"
                );
                ensure!(
                    policy_count == 0,
                    "invalid input: an authorized policy cannot be combined with provider, model, payload or budget overrides"
                );
                let authorization = catalog.load_run_authorization(policy_id)?;
                ensure!(
                    authorization.policy.kind == rcdvlog_core::AnalysisRunKind::MomentDiscovery,
                    "invalid input: policy does not authorize moment discovery"
                );
                Some(authorization.policy.moment_options()?)
            } else if args.moments {
                ensure!(
                    policy_count == 4,
                    "invalid input: --moments requires --provider, --model, --payload, and --budget-usd, or an authorized --policy-id"
                );
                let budget = args.budget_usd.unwrap();
                ensure!(
                    budget.is_finite() && budget > 0.0,
                    "invalid input: --budget-usd must be finite and positive"
                );
                Some(crate::moment_commands::default_options(
                    args.model.clone().unwrap(),
                    args.provider.clone().unwrap(),
                    args.payload.as_deref().unwrap(),
                    budget,
                ))
            } else {
                None
            };
            let options = PreparationRunOptions {
                generate_proxies: args.proxies,
                transcribe: args.transcribe,
                revalidate: args.revalidate,
                moments,
            };
            Ok(serde_json::to_value(catalog.prepare_for_edit(
                &scope(&args.scope),
                &options,
                |progress| {
                    eprintln!(
                        "{}",
                        json!({"event":"prepare.progress", "progress":progress})
                    );
                    ControlFlow::Continue(())
                },
            )?)?)
        }
    }
}
