//! Catalog-free local TV review diagnostics; canonical behavior stays in Rust.
use anyhow::{Result, ensure};
use clap::Subcommand;
use rcdvlog_core::{
    ReviewTargetHandoff,
    tv_review_sync::{SYNC_MAX_BYTES, SyncQuery, SyncRequest},
    tv_soundtrack::{
        SoundtrackCapture, SoundtrackIndex, SoundtrackPolicy, SoundtrackReferenceSpec,
        read_pcm8k_wav,
    },
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    io::Read,
    path::{Path, PathBuf},
};
#[derive(Debug, Subcommand)]
pub enum TvReviewCommand {
    /// Preview recording-time alignment; never writes feedback or a timeline.
    SyncPreview {
        #[arg(long)]
        input: PathBuf,
    },
    /// Hash an exact local final movie and index a bounded final-audio window.
    SoundtrackReference {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        movie: PathBuf,
    },
    /// Match one 4..20 second local mono8k WAV to a reusable reference index.
    SoundtrackMatch {
        #[arg(long)]
        index: PathBuf,
        #[arg(long)]
        query: PathBuf,
        #[arg(long)]
        capture: PathBuf,
    },
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewInput {
    request: SyncRequest,
    queries: Vec<SyncQuery>,
    target: Option<ReviewTargetHandoff>,
}
fn read_bytes(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let metadata = std::fs::metadata(path)?;
    ensure!(
        metadata.is_file() && metadata.len() <= limit as u64,
        "invalid input: TV review JSON must be a bounded regular file"
    );
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit,
        "invalid input: TV review JSON exceeds size bound"
    );
    Ok(bytes)
}
fn read_json<T: DeserializeOwned>(path: &Path, limit: usize) -> Result<T> {
    Ok(serde_json::from_slice(&read_bytes(path, limit)?)?)
}
pub fn command_name(command: &TvReviewCommand) -> &'static str {
    match command {
        TvReviewCommand::SyncPreview { .. } => "tv-review.sync-preview",
        TvReviewCommand::SoundtrackReference { .. } => "tv-review.soundtrack-reference",
        TvReviewCommand::SoundtrackMatch { .. } => "tv-review.soundtrack-match",
    }
}
pub fn execute(command: &TvReviewCommand) -> Result<Value> {
    match command {
        TvReviewCommand::SyncPreview { input } => {
            let input: PreviewInput = read_json(input, SYNC_MAX_BYTES)?;
            Ok(serde_json::to_value(if let Some(target) = input.target {
                input
                    .request
                    .map_to_cuts(&input.queries, &target, &|| false)?
            } else {
                input.request.map(&input.queries, &|| false)?
            })?)
        }
        TvReviewCommand::SoundtrackReference { spec, movie } => {
            let spec: SoundtrackReferenceSpec = read_json(spec, 65536)?;
            spec.validate()?;
            let _heavy = rcdvlog_core::acquire_local_heavy_at(
                &rcdvlog_core::default_local_heavy_lock_path(),
            )?;
            Ok(serde_json::to_value(SoundtrackIndex::from_movie(
                spec,
                movie,
                &|| false,
            )?)?)
        }
        TvReviewCommand::SoundtrackMatch {
            index,
            query,
            capture,
        } => {
            let index = SoundtrackIndex::from_json(&read_bytes(index, 16 * 1024 * 1024)?)?;
            let capture: SoundtrackCapture = read_json(capture, 65536)?;
            let _heavy = rcdvlog_core::acquire_local_heavy_at(
                &rcdvlog_core::default_local_heavy_lock_path(),
            )?;
            Ok(serde_json::to_value(index.match_pcm(
                &read_pcm8k_wav(query)?,
                capture,
                &SoundtrackPolicy::default(),
                &|| false,
            )?)?)
        }
    }
}
