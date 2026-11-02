//! Catalog-free local TV review diagnostics; canonical behavior stays in Rust.
use anyhow::{Result, ensure};
use clap::Subcommand;
use rcdvlog_core::{
    HistoricalRenderedArtifactReview, ReviewTargetHandoff,
    tv_review_sync::{SYNC_MAX_BYTES, SyncQuery, SyncRequest},
    tv_soundtrack::{
        SoundtrackCapture, SoundtrackIndex, SoundtrackPolicy, SoundtrackReferenceSpec,
        read_pcm8k_wav,
    },
    tv_soundtrack_session::{
        SessionAudioFile, SessionBatch, SoundtrackSessionPlan, SoundtrackSessionSpec,
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
    /// Plan complete-movie matching from native capture segments; paths stay local.
    SoundtrackSessionPlan {
        #[arg(long)]
        input: PathBuf,
    },
    /// Compute a bounded resumable batch; keep private indexes/receipts locally.
    SoundtrackSessionRun {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        movie: PathBuf,
        #[arg(long)]
        audio_files: PathBuf,
        #[arg(long)]
        cache: PathBuf,
        #[arg(long, default_value_t = 0)]
        start_task: usize,
        #[arg(long, default_value_t = 8)]
        max_tasks: usize,
        #[arg(long)]
        cancel_file: Option<PathBuf>,
    },
    /// Emit a page of complete-search anchors and explicit pending/unknown evidence.
    SoundtrackSessionEvidence {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        cache: PathBuf,
        #[arg(long, default_value_t = 0)]
        first_query: usize,
        #[arg(long, default_value_t = 32)]
        query_count: usize,
    },
    /// Map native utterance intervals using fully searched session evidence.
    SoundtrackSessionSync {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        cache: PathBuf,
        #[arg(long)]
        queries: PathBuf,
        #[arg(long, conflicts_with = "historical_target")]
        target: Option<PathBuf>,
        #[arg(long)]
        historical_target: Option<PathBuf>,
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
        TvReviewCommand::SoundtrackSessionPlan { .. } => "tv-review.soundtrack-session-plan",
        TvReviewCommand::SoundtrackSessionRun { .. } => "tv-review.soundtrack-session-run",
        TvReviewCommand::SoundtrackSessionEvidence { .. } => {
            "tv-review.soundtrack-session-evidence"
        }
        TvReviewCommand::SoundtrackSessionSync { .. } => "tv-review.soundtrack-session-sync",
    }
}
pub fn execute(command: &TvReviewCommand) -> Result<Value> {
    match command {
        TvReviewCommand::SoundtrackSessionSync {
            plan,
            cache,
            queries,
            target,
            historical_target,
        } => {
            let plan = SoundtrackSessionPlan::from_json(&read_bytes(plan, SYNC_MAX_BYTES)?)?;
            let queries: Vec<SyncQuery> = read_json(queries, 65536)?;
            let evidence = plan.sync_evidence(cache, &queries, &|| false)?;
            let (purpose, mapped) = if let Some(path) = target {
                let target = ReviewTargetHandoff::from_json(&read_bytes(path, 16 * 1024 * 1024)?)?;
                (
                    "strictVerifiedReviewTarget",
                    evidence.sync.map_to_cuts(&queries, &target, &|| false)?,
                )
            } else if let Some(path) = historical_target {
                let target = HistoricalRenderedArtifactReview::from_json(&read_bytes(
                    path,
                    16 * 1024 * 1024,
                )?)?;
                (
                    "historicalRenderedArtifactReview",
                    evidence
                        .sync
                        .map_to_historical_cuts(&queries, &target, &|| false)?,
                )
            } else {
                ("noCutMapping", evidence.sync.map(&queries, &|| false)?)
            };
            Ok(
                serde_json::json!({"schemaVersion":1,"mappingPurpose":purpose,"evidence":evidence,"report":mapped}),
            )
        }
        TvReviewCommand::SoundtrackSessionPlan { input } => Ok(serde_json::to_value(
            SoundtrackSessionPlan::new(read_json::<SoundtrackSessionSpec>(input, SYNC_MAX_BYTES)?)?,
        )?),
        TvReviewCommand::SoundtrackSessionRun {
            plan,
            movie,
            audio_files,
            cache,
            start_task,
            max_tasks,
            cancel_file,
        } => {
            let plan = SoundtrackSessionPlan::from_json(&read_bytes(plan, SYNC_MAX_BYTES)?)?;
            let files: Vec<SessionAudioFile> = read_json(audio_files, 1024 * 1024)?;
            let cancel = || cancel_file.as_ref().is_some_and(|p| p.exists());
            let _heavy = rcdvlog_core::acquire_local_heavy_at_cancellable(
                &rcdvlog_core::default_local_heavy_lock_path(),
                &cancel,
            )?;
            Ok(serde_json::to_value(plan.run_batch(
                SessionBatch {
                    movie,
                    files: &files,
                    cache,
                    start_task: *start_task,
                    max_tasks: *max_tasks,
                },
                &cancel,
            )?)?)
        }
        TvReviewCommand::SoundtrackSessionEvidence {
            plan,
            cache,
            first_query,
            query_count,
        } => {
            let plan = SoundtrackSessionPlan::from_json(&read_bytes(plan, SYNC_MAX_BYTES)?)?;
            Ok(serde_json::to_value(plan.evidence_page(
                cache,
                *first_query,
                *query_count,
                &|| false,
            )?)?)
        }
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
