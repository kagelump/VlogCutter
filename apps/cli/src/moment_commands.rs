//! CLI adapter for bounded, project-scoped moment discovery and review.

use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand};
use rcdvlog_core::{
    AnalysisPayload, AnalysisRunKind, AssetVideoValidationStatus, Catalog, EditProfile,
    MomentPayload, MomentRunOptions, RunPolicyParameters, RunPolicyRequest,
};
use serde_json::{Value, json};
use std::ops::ControlFlow;

#[derive(Debug, Subcommand)]
pub enum MomentCommand {
    /// Describe the exact policy for a proposed moment run without credentials or writes.
    Plan(MomentRunArgs),
    /// Persist explicit approval for the exact planned policy.
    Authorize(MomentRunArgs),
    /// Run one bounded capability/schema/source-time pilot for an authorized policy.
    Pilot(MomentRunArgs),
    /// Execute a stored policy and matching passed pilot over its exact scope.
    Run(AuthorizedRunArgs),
    /// Discover sparse candidate moments for an explicit scope.
    Discover(DiscoverArgs),
    /// List a bounded page of project assets and their moment status.
    List(PageArgs),
    /// Read a bounded page of candidate and evidence data for one asset.
    Get(GetArgs),
}

#[derive(Debug, Args, Clone)]
pub struct MomentRunArgs {
    #[arg(long)]
    project: String,
    #[arg(
        long = "asset",
        required_unless_present = "all_usable",
        conflicts_with = "all_usable"
    )]
    assets: Vec<String>,
    /// Explicitly select every non-removed asset validated as usable in this project.
    #[arg(long)]
    all_usable: bool,
    #[arg(long, value_parser = ["openrouter"])]
    provider: String,
    #[arg(long)]
    model: String,
    /// Models permitted for this run (repeat for each model).
    #[arg(long = "allow-model")]
    allowed_models: Vec<String>,
    /// Refuse automatic retry with the alternate model.
    #[arg(long)]
    no_fallback: bool,
    #[arg(long, value_parser = ["frames", "frames-and-transcript"])]
    payload: String,
    /// Maximum authorized spend for this run, in USD.
    #[arg(long)]
    budget_usd: f64,
    #[arg(long, default_value_t = 96)]
    coarse_frames: usize,
    #[arg(long, default_value_t = 384)]
    refinement_frames: usize,
    #[arg(long, default_value_t = 8)]
    max_candidates: usize,
    #[arg(long, default_value_t = 3000)]
    select_min_ms: u32,
    #[arg(long, default_value_t = 8000)]
    select_max_ms: u32,
    #[arg(long, default_value_t = 20000)]
    hold_min_ms: u32,
    #[arg(long, default_value_t = 30000)]
    hold_max_ms: u32,
    /// Explicitly recover an interrupted/unknown-usage pilot after review.
    #[arg(long)]
    retry_uncertain: bool,
}

#[derive(Debug, Args)]
pub struct DiscoverArgs {
    #[command(flatten)]
    run: MomentRunArgs,
    /// Explicitly replace completed candidate evidence.
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
pub struct AuthorizedRunArgs {
    #[arg(long)]
    policy_id: String,
    /// Explicitly replace completed candidate evidence.
    #[arg(long)]
    force: bool,
    /// Explicitly retry a step whose prior provider usage is unknown.
    #[arg(long)]
    retry_uncertain: bool,
}

#[derive(Debug, Args)]
pub struct PageArgs {
    #[arg(long)]
    project: String,
    #[arg(long, default_value_t = 0)]
    offset: usize,
    #[arg(long, default_value_t = 20)]
    limit: usize,
}

#[derive(Debug, Args)]
pub struct GetArgs {
    #[arg(long)]
    asset: String,
    #[arg(long, default_value_t = 0)]
    offset: usize,
    #[arg(long, default_value_t = 8)]
    limit: usize,
}

pub fn command_name(command: &MomentCommand) -> &'static str {
    match command {
        MomentCommand::Plan(_) => "moments.plan",
        MomentCommand::Authorize(_) => "moments.authorize",
        MomentCommand::Pilot(_) => "moments.pilot",
        MomentCommand::Run(_) => "moments.run",
        MomentCommand::Discover(_) => "moments.discover",
        MomentCommand::List(_) => "moments.list",
        MomentCommand::Get(_) => "moments.get",
    }
}

pub fn is_read_only(command: &MomentCommand) -> bool {
    matches!(
        command,
        MomentCommand::Plan(_) | MomentCommand::List(_) | MomentCommand::Get(_)
    )
}

fn scope(catalog: &Catalog, run: &MomentRunArgs) -> Result<Vec<String>> {
    if run.all_usable {
        let mut ids = Vec::new();
        for asset in catalog.list_assets(&run.project)? {
            if !asset.removed
                && catalog.asset_video_validation(&asset.id)?.status
                    == AssetVideoValidationStatus::Usable
            {
                ids.push(asset.id);
            }
        }
        ensure!(
            !ids.is_empty(),
            "--all-usable selected no non-removed assets with usable video validation"
        );
        Ok(ids)
    } else {
        ensure!(!run.assets.is_empty(), "select --asset or --all-usable");
        Ok(run.assets.clone())
    }
}

fn options(run: &MomentRunArgs, force: bool, retry_uncertain: bool) -> MomentRunOptions {
    let mut options = default_options(
        run.model.clone(),
        run.provider.clone(),
        run.payload.as_str(),
        run.budget_usd,
    );
    options.coarse_frames = run.coarse_frames;
    options.refinement_frames = run.refinement_frames;
    options.max_candidates = run.max_candidates;
    options.profile = EditProfile {
        select_min_ms: run.select_min_ms,
        select_max_ms: run.select_max_ms,
        hold_min_ms: run.hold_min_ms,
        hold_max_ms: run.hold_max_ms,
    };
    options.force = force;
    options.retry_uncertain = retry_uncertain;
    options.allowed_models = if run.allowed_models.is_empty() {
        vec![run.model.clone()]
    } else {
        run.allowed_models.clone()
    };
    options.allow_fallback = !run.no_fallback && options.allowed_models.len() > 1;
    options
}

fn policy_request(run: &MomentRunArgs, asset_ids: Vec<String>) -> Result<RunPolicyRequest> {
    let payload = match run.payload.as_str() {
        "frames-and-transcript" => AnalysisPayload::FramesAndTranscript,
        "frames" => AnalysisPayload::Frames,
        other => anyhow::bail!("invalid input: unsupported moment payload {other}"),
    };
    let allowed_models = if run.allowed_models.is_empty() {
        vec![run.model.clone()]
    } else {
        run.allowed_models.clone()
    };
    let allow_fallback = !run.no_fallback && allowed_models.len() > 1;
    Ok(RunPolicyRequest {
        project_id: run.project.clone(),
        asset_ids,
        provider: run.provider.clone(),
        primary_model: run.model.clone(),
        allowed_models,
        allow_fallback,
        payload,
        include_project_vocabulary: false,
        budget_usd: run.budget_usd,
        parameters: RunPolicyParameters::MomentDiscovery {
            coarse_frames_per_asset: run.coarse_frames,
            refinement_frames_per_asset: run.refinement_frames,
            max_candidates_per_asset: run.max_candidates,
            profile: EditProfile {
                select_min_ms: run.select_min_ms,
                select_max_ms: run.select_max_ms,
                hold_min_ms: run.hold_min_ms,
                hold_max_ms: run.hold_max_ms,
            },
            force: false,
            retry_uncertain: false,
        },
    })
}

pub fn default_options(
    model: String,
    provider: String,
    payload: &str,
    budget_usd: f64,
) -> MomentRunOptions {
    MomentRunOptions {
        allowed_models: vec![model.clone()],
        model,
        provider,
        payload: match payload {
            "frames-and-transcript" => MomentPayload::FramesAndTranscript,
            _ => MomentPayload::Frames,
        },
        budget_usd,
        coarse_frames: 96,
        refinement_frames: 384,
        max_candidates: 8,
        profile: EditProfile::default(),
        force: false,
        retry_uncertain: false,
        allow_fallback: false,
        policy_id: None,
    }
}

pub fn execute(catalog: &Catalog, command: &MomentCommand) -> Result<Value> {
    match command {
        MomentCommand::Plan(run) => {
            let ids = scope(catalog, run)?;
            let options = options(run, false, false);
            Ok(serde_json::to_value(catalog.plan_moments(
                &run.project,
                &ids,
                &options,
            )?)?)
        }
        MomentCommand::Authorize(run) => {
            let ids = scope(catalog, run)?;
            let request = policy_request(run, ids)?;
            Ok(serde_json::to_value(
                catalog.authorize_run_policy(&request)?,
            )?)
        }
        MomentCommand::Pilot(run) => {
            let ids = scope(catalog, run)?;
            let request = policy_request(run, ids)?;
            Ok(serde_json::to_value(
                catalog.run_run_policy_pilot(&request, run.retry_uncertain)?,
            )?)
        }
        MomentCommand::Run(args) => {
            catalog
                .authorized_policy_for_bulk(&args.policy_id, AnalysisRunKind::MomentDiscovery)?;
            ensure!(
                rcdvlog_core::api_credential_present(),
                "OpenRouter credentials are not available; no moment request was made"
            );
            let provider = rcdvlog_core::OpenRouterProvider::new(rcdvlog_core::resolve_api_key()?)?;
            ensure!(
                !args.force && !args.retry_uncertain,
                "force and uncertain-request recovery are bound by the authorized policy; change the policy instead"
            );
            Ok(serde_json::to_value(
                catalog.discover_authorized_moments_with_provider(
                    &args.policy_id,
                    &provider,
                    |progress| {
                        eprintln!(
                            "{}",
                            json!({
                                "event": "moments.run.progress",
                                "runId": progress.run_id,
                                "assetId": progress.asset_id,
                                "phase": progress.phase,
                                "costUsd": progress.cost_usd,
                                "estimate": progress.estimate,
                            })
                        );
                        ControlFlow::Continue(())
                    },
                )?,
            )?)
        }
        MomentCommand::Discover(args) => {
            let ids = scope(catalog, &args.run)?;
            let options = options(&args.run, args.force, args.run.retry_uncertain);
            Ok(serde_json::to_value(catalog.discover_moments_for_assets(
                &args.run.project,
                &ids,
                &options,
                |progress| {
                    eprintln!(
                        "{}",
                        json!({"event":"moments.progress", "progress":progress})
                    );
                    ControlFlow::Continue(())
                },
            )?)?)
        }
        MomentCommand::List(args) => Ok(serde_json::to_value(catalog.moment_inventory_page(
            &args.project,
            args.offset,
            args.limit,
        )?)?),
        MomentCommand::Get(args) => {
            let page = catalog
                .moment_evidence_page(&args.asset, args.offset, args.limit)?
                .context("no moment evidence is stored for this asset")?;
            Ok(serde_json::to_value(page)?)
        }
    }
}
