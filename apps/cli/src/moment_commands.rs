//! CLI adapter for bounded, project-scoped moment discovery and review.

use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand};
use rcdvlog_core::{
    AssetVideoValidationStatus, Catalog, EditProfile, MomentPayload, MomentRunOptions,
};
use serde_json::{Value, json};
use std::ops::ControlFlow;

#[derive(Debug, Subcommand)]
pub enum MomentCommand {
    /// Validate an explicit scope and report the proposed bounded run without credentials or writes.
    Plan(MomentRunArgs),
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
}

#[derive(Debug, Args)]
pub struct DiscoverArgs {
    #[command(flatten)]
    run: MomentRunArgs,
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
        MomentCommand::Discover(_) => "moments.discover",
        MomentCommand::List(_) => "moments.list",
        MomentCommand::Get(_) => "moments.get",
    }
}

pub fn is_read_only(command: &MomentCommand) -> bool {
    !matches!(command, MomentCommand::Discover(_))
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
    MomentRunOptions {
        model: run.model.clone(),
        provider: run.provider.clone(),
        payload: match run.payload.as_str() {
            "frames-and-transcript" => MomentPayload::FramesAndTranscript,
            _ => MomentPayload::Frames,
        },
        budget_usd: run.budget_usd,
        coarse_frames: run.coarse_frames,
        refinement_frames: run.refinement_frames,
        max_candidates: run.max_candidates,
        profile: EditProfile {
            select_min_ms: run.select_min_ms,
            select_max_ms: run.select_max_ms,
            hold_min_ms: run.hold_min_ms,
            hold_max_ms: run.hold_max_ms,
        },
        force,
        retry_uncertain,
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
        MomentCommand::Discover(args) => {
            let ids = scope(catalog, &args.run)?;
            let options = options(&args.run, args.force, args.retry_uncertain);
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
