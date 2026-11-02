//! Catalog-free, read-only synchronization preview over bounded evidence JSON.
use anyhow::{Result, ensure};
use clap::Subcommand;
use rcdvlog_core::{
    ReviewTargetHandoff,
    tv_review_sync::{SYNC_MAX_BYTES, SyncQuery, SyncRequest},
};
use serde::Deserialize;
use serde_json::Value;
use std::{io::Read, path::PathBuf};
#[derive(Debug, Subcommand)]
pub enum TvReviewCommand {
    /// Preview recording-time alignment; never writes feedback or a timeline.
    SyncPreview {
        #[arg(long)]
        input: PathBuf,
    },
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewInput {
    request: SyncRequest,
    queries: Vec<SyncQuery>,
    target: Option<ReviewTargetHandoff>,
}
pub fn command_name(command: &TvReviewCommand) -> &'static str {
    match command {
        TvReviewCommand::SyncPreview { .. } => "tv-review.sync-preview",
    }
}
pub fn execute(command: &TvReviewCommand) -> Result<Value> {
    match command {
        TvReviewCommand::SyncPreview { input } => {
            let mut bytes = Vec::new();
            std::fs::File::open(input)?
                .take((SYNC_MAX_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() <= SYNC_MAX_BYTES,
                "invalid input: sync preview exceeds 4 MiB"
            );
            let input: PreviewInput = serde_json::from_slice(&bytes)?;
            Ok(serde_json::to_value(if let Some(target) = input.target {
                input
                    .request
                    .map_to_cuts(&input.queries, &target, &|| false)?
            } else {
                input.request.map(&input.queries, &|| false)?
            })?)
        }
    }
}
