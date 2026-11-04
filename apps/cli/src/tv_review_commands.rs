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
    collections::{BTreeMap, BTreeSet},
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
    #[serde(default)]
    pooled_offset_windows: Option<Vec<PooledOffsetWindow>>,
    #[serde(default)]
    pooled_offset_mode_limit: Option<u8>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PooledOffsetWindow {
    start_seconds: u32,
    duration_seconds: u16,
}

#[derive(Default)]
struct PooledOffsetMode {
    summed_cell_anchor_count: u64,
    rarity_weight: f64,
    saturated_anchors: u64,
    query_anchor_frames: BTreeSet<u32>,
    occupied_time_bins: Vec<u32>,
    frequency_diversity_by_time_bin: Vec<u32>,
}

impl PooledOffsetMode {
    fn add(&mut self, evidence: &LandmarkOffsetEvidence) {
        self.summed_cell_anchor_count += u64::from(evidence.independent_anchors);
        self.query_anchor_frames
            .extend(evidence.query_anchor_frames.iter().copied());
        self.rarity_weight += evidence.rarity_weight;
        self.saturated_anchors += u64::from(evidence.saturated_anchors);
        self.occupied_time_bins.push(evidence.time_bin);
        self.frequency_diversity_by_time_bin
            .push(evidence.frequency_diversity);
    }

    fn independent_anchor_count(&self) -> u64 {
        self.query_anchor_frames.len() as u64
    }

    fn shared_query_anchors(&self, other: &Self) -> usize {
        self.query_anchor_frames
            .intersection(&other.query_anchor_frames)
            .count()
    }

    fn support_span_seconds(&self) -> u32 {
        match (
            self.occupied_time_bins.first(),
            self.occupied_time_bins.last(),
        ) {
            (Some(first), Some(last)) => last - first + 1,
            _ => 0,
        }
    }

    fn median_frequency_diversity(&self) -> f64 {
        let mut values = self.frequency_diversity_by_time_bin.clone();
        values.sort_unstable();
        match values.len() {
            0 => 0.0,
            length if length % 2 == 1 => f64::from(values[length / 2]),
            length => f64::from(values[length / 2 - 1] + values[length / 2]) / 2.0,
        }
    }
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
            let pooled_offset_evidence = request
                .pooled_offset_windows
                .take()
                .map(|windows| {
                    pool_exact_offset_windows(
                        &search,
                        &windows,
                        usize::from(request.pooled_offset_mode_limit.take().unwrap_or(8)),
                    )
                })
                .transpose()?;
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
                "pooledOffsetEvidence":pooled_offset_evidence,
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

/// Pool complete per-second exact-offset cells before the bounded raw diagnostics
/// are selected. Windows are independent observations: no evidence is combined
/// across overlapping windows, and neighboring offset bins remain alternatives.
fn pool_exact_offset_windows(
    search: &LandmarkSearch,
    windows: &[PooledOffsetWindow],
    mode_limit: usize,
) -> Result<Value> {
    const MAX_WINDOWS: usize = 256;
    const MAX_MODE_LIMIT: usize = 64;
    const MAX_WINDOW_CELL_SCANS: usize = 50_000_000;
    ensure!(
        !windows.is_empty() && windows.len() <= MAX_WINDOWS,
        "invalid input: pooled offset window count must be 1..256"
    );
    ensure!(
        (1..=MAX_MODE_LIMIT).contains(&mode_limit),
        "invalid input: pooled offset mode limit must be 1..64"
    );
    ensure!(
        search
            .offsets
            .len()
            .checked_mul(windows.len())
            .is_some_and(|scans| scans <= MAX_WINDOW_CELL_SCANS),
        "invalid input: pooled offset cell-scan budget exceeded"
    );
    let query_seconds = search.query_sample_count as f64 / f64::from(tv_soundtrack_landmarks::RATE);
    let mut output_windows = Vec::with_capacity(windows.len());

    for window in windows {
        ensure!(
            matches!(window.duration_seconds, 10 | 20 | 30),
            "invalid input: pooled offset durations must be 10, 20 or 30 seconds"
        );
        let end_seconds = window
            .start_seconds
            .checked_add(u32::from(window.duration_seconds))
            .ok_or_else(|| anyhow::anyhow!("invalid input: pooled offset window overflow"))?;
        ensure!(
            f64::from(end_seconds) <= query_seconds + 1e-6,
            "invalid input: pooled offset window exceeds decoded query duration"
        );

        let mut by_offset: BTreeMap<i32, PooledOffsetMode> = BTreeMap::new();
        let mut pooled_offset_cell_count = 0usize;
        for evidence in &search.offsets {
            if evidence.time_bin >= window.start_seconds && evidence.time_bin < end_seconds {
                by_offset
                    .entry(evidence.offset_bin)
                    .or_default()
                    .add(evidence);
                pooled_offset_cell_count += 1;
            }
        }
        let mut ranked: Vec<(i32, PooledOffsetMode)> = by_offset.into_iter().collect();
        ranked.sort_by(|(left_bin, left), (right_bin, right)| {
            right
                .independent_anchor_count()
                .cmp(&left.independent_anchor_count())
                .then(left_bin.cmp(right_bin))
        });

        let best_competitor = ranked.first().and_then(|(best_bin, best)| {
            ranked
                .iter()
                .skip(1)
                .find(|(bin, _)| (i64::from(*bin) - i64::from(*best_bin)).abs() >= 2)
                .map(|(bin, mode)| {
                    let shared = best.shared_query_anchors(mode);
                    serde_json::json!({
                        "offsetBin": bin,
                        "offsetSeconds": f64::from(*bin) * 0.08,
                        "independentAnchors": mode.independent_anchor_count(),
                        "occupiedTimeBins": mode.occupied_time_bins.len(),
                        "sharedQueryAnchorsWithBest": shared,
                        "uniqueQueryAnchorsOutsideBest": mode.independent_anchor_count() as usize - shared,
                    })
                })
        });
        let best_anchor_margin = ranked.first().and_then(|(_, best)| {
            ranked
                .iter()
                .skip(1)
                .find(|(bin, _)| (i64::from(*bin) - i64::from(ranked[0].0)).abs() >= 2)
                .and_then(|(_, competitor)| {
                    (competitor.independent_anchor_count() > 0).then(|| {
                        ((best.independent_anchor_count() as f64
                            / competitor.independent_anchor_count() as f64)
                            * 10_000.0)
                            .round()
                            / 10_000.0
                    })
                })
        });
        let best_unique_anchor_margin = ranked.first().and_then(|(best_bin, best)| {
            ranked
                .iter()
                .skip(1)
                .find(|(bin, _)| (i64::from(*bin) - i64::from(*best_bin)).abs() >= 2)
                .and_then(|(_, competitor)| {
                    let shared = best.shared_query_anchors(competitor);
                    let best_unique = best.independent_anchor_count() as usize - shared;
                    let competitor_unique = competitor.independent_anchor_count() as usize - shared;
                    (competitor_unique > 0).then(|| {
                        ((best_unique as f64 / competitor_unique as f64) * 10_000.0).round()
                            / 10_000.0
                    })
                })
        });
        let modes: Vec<Value> = ranked
            .iter()
            .take(mode_limit)
            .map(|(offset_bin, mode)| {
                let shared_with_best = ranked
                    .first()
                    .map(|(_, best)| best.shared_query_anchors(mode))
                    .unwrap_or(0);
                serde_json::json!({
                    "offsetBin": offset_bin,
                    "offsetSeconds": f64::from(*offset_bin) * 0.08,
                    "independentAnchors": mode.independent_anchor_count(),
                    "summedCellAnchorCount": mode.summed_cell_anchor_count,
                    "duplicateAnchorVotesRemoved": mode.summed_cell_anchor_count - mode.independent_anchor_count(),
                    "occupiedTimeBins": mode.occupied_time_bins.len(),
                    "supportSpanSeconds": mode.support_span_seconds(),
                    "rarityWeight": (mode.rarity_weight * 1_000_000.0).round() / 1_000_000.0,
                    "frequencyDiversityMedianPerTimeBin": mode.median_frequency_diversity(),
                    "saturatedAnchors": mode.saturated_anchors,
                    "sharedQueryAnchorsWithBest": shared_with_best,
                })
            })
            .collect();
        output_windows.push(serde_json::json!({
            "startSeconds": window.start_seconds,
            "durationSeconds": window.duration_seconds,
            "endSeconds": end_seconds,
            "complete": search.complete,
            "internalOffsetCellCount": search.offsets.len(),
            "offsetCellsScanned": search.offsets.len(),
            "offsetCellsPooledForWindow": pooled_offset_cell_count,
            "offsetCellsDropped": 0,
            "rankedOffsetBinCount": ranked.len(),
            "returnedOffsetBinCount": modes.len(),
            "bestDistinctCompetitor": best_competitor,
            "bestAnchorMargin": best_anchor_margin,
            "bestUniqueAnchorMargin": best_unique_anchor_margin,
            "modes": modes,
        }));
    }

    Ok(serde_json::json!({
        "schemaVersion": 1,
        "status": if search.complete { "complete" } else { "incomplete" },
        "complete": search.complete,
        "searchComplete": search.complete,
        "internalOffsetCellCount": search.offsets.len(),
        "offsetCellScanComplete": true,
        "diagnosticTruncationAppliedAfterPooling": true,
        "queryAnchorIdentityAvailable": true,
        "anchorIdentitySerialized": false,
        "anchorIdentityScope": "query landmark frame index within the current decoded recording; available to the in-memory pool and omitted from serialized raw diagnostic rows",
        "aggregationScope": "exact offset bin per window; query anchor IDs deduplicate within a mode and quantify cross-mode overlap; no neighboring-bin merge and no cross-window accumulation",
        "modeLimit": mode_limit,
        "windows": output_windows,
    }))
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

#[cfg(test)]
mod pooled_offset_tests {
    use super::*;

    fn evidence(time_bin: u32, offset_bin: i32, anchors: u32) -> LandmarkOffsetEvidence {
        LandmarkOffsetEvidence {
            time_bin,
            offset_bin,
            offset_seconds_from_capture_start: f64::from(offset_bin) * 0.08,
            independent_anchors: anchors,
            supporting_seconds: 1,
            frequency_diversity: 4,
            rarity_weight: f64::from(anchors),
            saturated_anchors: 0,
            query_anchor_frames: (0..anchors)
                .map(|ordinal| time_bin * 1000 + ordinal)
                .collect(),
        }
    }

    fn search(query_seconds: u64, offsets: Vec<LandmarkOffsetEvidence>) -> LandmarkSearch {
        LandmarkSearch {
            schema_version: 1,
            recipe: LandmarkRecipe::default(),
            search_policy: LandmarkSearchPolicy::default(),
            movie_digest: "movie".into(),
            index_digest: "index".into(),
            query_pcm_digest: "query".into(),
            query_sample_count: query_seconds * u64::from(tv_soundtrack_landmarks::RATE),
            query_landmark_count: 0,
            matched_landmark_count: 0,
            candidate_pair_count: 0,
            complete: true,
            offsets,
        }
    }

    #[test]
    fn exact_offset_bins_remain_separate_and_adjacent_bins_have_no_distinct_competitor() {
        let search = search(
            10,
            vec![
                evidence(0, 100, 3),
                evidence(1, 100, 2),
                evidence(0, 101, 20),
            ],
        );
        let pooled = pool_exact_offset_windows(
            &search,
            &[PooledOffsetWindow {
                start_seconds: 0,
                duration_seconds: 10,
            }],
            8,
        )
        .unwrap();
        let window = &pooled["windows"][0];
        assert_eq!(window["modes"][0]["offsetBin"], 101);
        assert_eq!(window["modes"][0]["independentAnchors"], 20);
        assert_eq!(window["modes"][1]["offsetBin"], 100);
        assert_eq!(window["modes"][1]["independentAnchors"], 5);
        assert_eq!(window["modes"][1]["sharedQueryAnchorsWithBest"], 3);
        assert!(window["bestDistinctCompetitor"].is_null());
        assert!(window["bestUniqueAnchorMargin"].is_null());
        assert_eq!(window["offsetCellsPooledForWindow"], 3);
    }

    #[test]
    fn distinct_competitor_margin_uses_unique_anchor_identity() {
        let mut best0 = evidence(0, 100, 3);
        best0.query_anchor_frames = vec![1, 2, 3];
        let mut best1 = evidence(1, 100, 2);
        best1.query_anchor_frames = vec![4, 5];
        let mut competitor0 = evidence(0, 110, 3);
        competitor0.query_anchor_frames = vec![1, 2, 6];
        let mut competitor1 = evidence(1, 110, 2);
        competitor1.query_anchor_frames = vec![7];
        let pooled = pool_exact_offset_windows(
            &search(10, vec![best0, best1, competitor0, competitor1]),
            &[PooledOffsetWindow {
                start_seconds: 0,
                duration_seconds: 10,
            }],
            8,
        )
        .unwrap();
        let window = &pooled["windows"][0];
        assert_eq!(window["bestAnchorMargin"], 1.25);
        assert_eq!(
            window["bestDistinctCompetitor"]["sharedQueryAnchorsWithBest"],
            2
        );
        assert_eq!(window["bestUniqueAnchorMargin"], 1.5);
    }

    #[test]
    fn overlapping_windows_are_ranked_separately_without_cross_window_vote_sums() {
        let search = search(
            12,
            vec![evidence(0, 50, 2), evidence(5, 50, 4), evidence(10, 50, 3)],
        );
        let pooled = pool_exact_offset_windows(
            &search,
            &[
                PooledOffsetWindow {
                    start_seconds: 0,
                    duration_seconds: 10,
                },
                PooledOffsetWindow {
                    start_seconds: 2,
                    duration_seconds: 10,
                },
            ],
            8,
        )
        .unwrap();
        assert_eq!(pooled["windows"][0]["modes"][0]["independentAnchors"], 6);
        assert_eq!(pooled["windows"][1]["modes"][0]["independentAnchors"], 7);
        assert_eq!(pooled["windows"][0]["offsetCellsScanned"], 3);
        assert_eq!(pooled["windows"][1]["offsetCellsScanned"], 3);
        assert_eq!(pooled["windows"][0]["offsetCellsDropped"], 0);
        assert_eq!(pooled["windows"][1]["offsetCellsDropped"], 0);
    }

    #[test]
    fn incomplete_search_is_reported_even_when_all_returned_cells_are_scanned() {
        let mut search = search(10, vec![evidence(0, 50, 2)]);
        search.complete = false;
        let pooled = pool_exact_offset_windows(
            &search,
            &[PooledOffsetWindow {
                start_seconds: 0,
                duration_seconds: 10,
            }],
            8,
        )
        .unwrap();
        assert_eq!(pooled["complete"], false);
        assert_eq!(pooled["offsetCellScanComplete"], true);
        assert_eq!(pooled["windows"][0]["offsetCellsDropped"], 0);
    }
}
