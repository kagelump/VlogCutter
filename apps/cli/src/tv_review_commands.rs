//! Catalog-free local TV review diagnostics; canonical behavior stays in Rust.
use anyhow::{Result, ensure};
use clap::Subcommand;
use rcdvlog_core::{
    HistoricalRenderedArtifactReview, ReviewTargetHandoff,
    tv_review_sync::{SYNC_MAX_BYTES, SyncMovieClock, SyncQuery, SyncRequest},
    tv_soundtrack::{
        SoundtrackCapture, SoundtrackIndex, SoundtrackPolicy, SoundtrackReferenceSpec,
        read_pcm8k_wav,
    },
    tv_soundtrack_alignment::{self, Policy as LandmarkAlignmentPolicy},
    tv_soundtrack_consensus::{self, SoundtrackConsensus},
    tv_soundtrack_landmarks::{
        self, Index as LandmarkIndex, OffsetEvidence as LandmarkOffsetEvidence,
        Recipe as LandmarkRecipe, Search as LandmarkSearch, SearchPolicy as LandmarkSearchPolicy,
    },
    tv_soundtrack_session::{
        SessionAudioFile, SessionBatch, SoundtrackSessionPlan, SoundtrackSessionSpec,
    },
};

const DEFAULT_LANDMARK_OFFSET_EVIDENCE_PER_TILE: usize = 12;
const MAX_LANDMARK_OFFSET_EVIDENCE_PER_TILE: usize = 512;
const MAX_LANDMARK_DIAGNOSTIC_DURATION_SECONDS: f64 = 60.0;
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
        /// Optional SoundtrackConsensus JSON (from soundtrack-session-consensus) whose segments gate interpolation (§9).
        #[arg(long)]
        consensus: Option<PathBuf>,
    },
    /// Derive offset segments and boundaries from complete session evidence.
    SoundtrackSessionConsensus {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        cache: PathBuf,
    },
    /// Build an immutable, full-movie local landmark index for an exact movie identity.
    SourceAudioLandmarkIndex {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        movie: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        cancel_file: Option<PathBuf>,
    },
    /// Match a complete microphone recording and pass supported segments through the shared mapper.
    SourceAudioLandmarkMatch {
        #[arg(long)]
        index: PathBuf,
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        recording: PathBuf,
        #[arg(long)]
        cancel_file: Option<PathBuf>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LandmarkIndexInput {
    movie: SyncMovieClock,
    #[serde(default)]
    fft_size: Option<u16>,
    #[serde(default)]
    hashes_per_second: Option<u16>,
    #[serde(default)]
    fanout: Option<u8>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LandmarkMatchInput {
    sync: SyncRequest,
    capture_clock_id: String,
    #[serde(default)]
    policy: Option<LandmarkAlignmentPolicy>,
    #[serde(default)]
    search_policy: Option<LandmarkSearchPolicy>,
    #[serde(default)]
    diagnostic_offset_evidence_per_tile: Option<u16>,
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
        TvReviewCommand::SoundtrackSessionConsensus { .. } => {
            "tv-review.soundtrack-session-consensus"
        }
        TvReviewCommand::SourceAudioLandmarkIndex { .. } => "tv-review.source-audio-landmark-index",
        TvReviewCommand::SourceAudioLandmarkMatch { .. } => "tv-review.source-audio-landmark-match",
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
            consensus,
        } => {
            let plan = SoundtrackSessionPlan::from_json(&read_bytes(plan, SYNC_MAX_BYTES)?)?;
            let queries: Vec<SyncQuery> = read_json(queries, 65536)?;
            let mut evidence = plan.sync_evidence(cache, &queries, &|| false)?;
            if let Some(path) = consensus {
                let consensus: SoundtrackConsensus =
                    read_json(path, SYNC_MAX_BYTES).map_err(|e| {
                        anyhow::anyhow!(
                            "invalid input: --consensus must be a bounded SoundtrackConsensus JSON file: {e}"
                        )
                    })?;
                ensure!(
                    consensus.schema_version == 1 && consensus.plan_digest == plan.plan_digest,
                    "invalid input: --consensus does not belong to this session plan"
                );
                evidence.sync.consensus_segments = consensus.segments;
            }
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
        TvReviewCommand::SoundtrackSessionConsensus { plan, cache } => {
            let plan = SoundtrackSessionPlan::from_json(&read_bytes(plan, SYNC_MAX_BYTES)?)?;
            Ok(serde_json::to_value(tv_soundtrack_consensus::consensus(
                &plan,
                cache,
                &|| false,
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
        TvReviewCommand::SourceAudioLandmarkIndex {
            input,
            movie,
            output,
            cancel_file,
        } => {
            let request: LandmarkIndexInput = read_json(input, SYNC_MAX_BYTES)?;
            let cancel = || cancel_file.as_ref().is_some_and(|p| p.exists());
            let _heavy = rcdvlog_core::acquire_local_heavy_at_cancellable(
                &rcdvlog_core::default_local_heavy_lock_path(),
                &cancel,
            )?;
            let mut recipe = LandmarkRecipe::default();
            if let Some(v) = request.fft_size {
                recipe.fft_size = v;
            }
            recipe.id = if recipe.fft_size == 1024 {
                tv_soundtrack_landmarks::RECIPE_ID_1024.into()
            } else {
                tv_soundtrack_landmarks::RECIPE_ID_512.into()
            };
            if let Some(v) = request.hashes_per_second {
                recipe.hashes_per_second = v;
            }
            if let Some(v) = request.fanout {
                recipe.fanout = v;
            }
            let index = LandmarkIndex::from_movie(request.movie, movie, recipe, &cancel)?;
            let bytes = index.to_json()?;
            ensure!(!cancel(), "source-audio index write cancelled");
            write_immutable_artifact(output, &bytes)?;
            Ok(
                serde_json::json!({"schemaVersion":1,"indexDigest":index.index_digest,
                "movieDigest":index.movie.movie_digest,"pcmDigest":index.pcm_digest,
                "recipe":index.recipe,"sampleCount":index.sample_count,"frameCount":index.frame_count,
                "postingCount":index.postings.values().map(Vec::len).sum::<usize>(),"bytes":bytes.len()}),
            )
        }
        TvReviewCommand::SourceAudioLandmarkMatch {
            index,
            input,
            recording,
            cancel_file,
        } => {
            let mut request: LandmarkMatchInput = read_json(input, SYNC_MAX_BYTES)?;
            let cancel = || cancel_file.as_ref().is_some_and(|p| p.exists());
            let _heavy = rcdvlog_core::acquire_local_heavy_at_cancellable(
                &rcdvlog_core::default_local_heavy_lock_path(),
                &cancel,
            )?;
            let before = rcdvlog_core::full_file_blake3(recording, &cancel)?;
            let index = LandmarkIndex::from_json(&read_bytes(index, 64 * 1024 * 1024)?)?;
            ensure!(
                request.sync.movie == index.movie,
                "invalid input: source-audio index movie identity mismatch"
            );
            let clock = request
                .sync
                .capture_clocks
                .iter()
                .find(|c| c.clock_id == request.capture_clock_id)
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!("invalid input: requested capture clock is absent")
                })?;
            let pcm = tv_soundtrack_landmarks::decode_wav(recording, &cancel)?;
            let after = rcdvlog_core::full_file_blake3(recording, &cancel)?;
            ensure!(
                before.hex_digest == after.hex_digest && before.byte_count == after.byte_count,
                "input_changed"
            );
            ensure!(
                pcm.len() as f64 / f64::from(tv_soundtrack_landmarks::RATE)
                    <= (clock.last_sample - clock.first_sample) as f64
                        / f64::from(clock.sample_rate)
                        + 0.1,
                "invalid input: capture clock is shorter than recording audio"
            );
            let search = index.search_pcm_with_policy(
                &pcm,
                request.search_policy.take().unwrap_or_default(),
                &cancel,
            )?;
            let offset_evidence_limit = usize::from(
                request
                    .diagnostic_offset_evidence_per_tile
                    .take()
                    .unwrap_or(DEFAULT_LANDMARK_OFFSET_EVIDENCE_PER_TILE as u16),
            );
            ensure!(
                (1..=MAX_LANDMARK_OFFSET_EVIDENCE_PER_TILE).contains(&offset_evidence_limit),
                "invalid input: raw offset diagnostic limit must be 1..512 per tile"
            );
            if offset_evidence_limit > DEFAULT_LANDMARK_OFFSET_EVIDENCE_PER_TILE {
                ensure!(
                    search.query_sample_count as f64 / f64::from(tv_soundtrack_landmarks::RATE)
                        <= MAX_LANDMARK_DIAGNOSTIC_DURATION_SECONDS,
                    "invalid input: expanded raw offset diagnostics are limited to one-minute recordings"
                );
            }
            let offset_evidence_count = search.offsets.len();
            let offset_evidence = bounded_landmark_offset_evidence(&search, offset_evidence_limit);
            let policy = request.policy.take().unwrap_or_default();
            let alignment = tv_soundtrack_alignment::infer(&search, &clock, &index.movie, policy)?;
            let mapping = tv_soundtrack_alignment::attach_and_map(
                &mut request.sync,
                &alignment,
                &clock,
                &cancel,
            )?;
            Ok(
                serde_json::json!({"schemaVersion":1,"movieDigest":index.movie.movie_digest,
                "indexDigest":index.index_digest,"recordingDigest":before.hex_digest,
                "search":{"complete":search.complete,"queryLandmarkCount":search.query_landmark_count,
                    "matchedLandmarkCount":search.matched_landmark_count,
                    "candidatePairCount":search.candidate_pair_count,
                    "rawOffsetEvidenceCount":offset_evidence_count,
                    "rawOffsetEvidencePerTileLimit":offset_evidence_limit,
                    "rawOffsetEvidence":offset_evidence},
                "alignment":alignment,"mapping":mapping}),
            )
        }
    }
}

/// Preserve bounded raw offset modes before per-second alignment gates. These
/// diagnostics let an evaluator distinguish missing landmarks from evidence
/// discarded by segment inference without giving truth to the matcher.
fn bounded_landmark_offset_evidence(
    search: &LandmarkSearch,
    limit_per_tile: usize,
) -> Vec<LandmarkOffsetEvidence> {
    let mut output = Vec::new();
    let mut active_tile = None;
    let mut retained = 0usize;
    for evidence in &search.offsets {
        if active_tile != Some(evidence.time_bin) {
            active_tile = Some(evidence.time_bin);
            retained = 0;
        }
        if retained < limit_per_tile {
            output.push(evidence.clone());
            retained += 1;
        }
    }
    output
}

fn write_immutable_artifact(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    if path.exists() {
        let old = read_bytes(path, 64 * 1024 * 1024)?;
        ensure!(
            old == bytes,
            "invalid input: immutable source-audio index path already contains different bytes"
        );
        return Ok(());
    }
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("invalid output path"))?
        .to_string_lossy();
    let temp = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::hard_link(&temp, path)?;
        std::fs::remove_file(&temp)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if temp.exists() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}
