//! Bounded, resumable local orchestration over the final-mix matcher.
//! Plans/receipts are private derived evidence, never an edit-plan owner.
use crate::{
    full_file_blake3,
    tv_review_sync::{SyncAnchor, SyncBoundary, SyncBoundaryKind, SyncQuery, SyncRequest},
    tv_soundtrack::{
        PCM_RATE, SoundtrackCapture, SoundtrackIndex, SoundtrackMatchReport, SoundtrackMovie,
        SoundtrackPolicy, SoundtrackReferenceSpec,
    },
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const RECIPE: &str = "soundtrack-session.native-wav-ffmpeg8k.v1";
const MAX_JSON: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionAudioSegment {
    pub segment_id: String,
    pub clock_id: String,
    /// Frame zero of this WAV corresponds to this original capture sample.
    pub first_sample: u64,
    pub frame_count: u32,
    pub wav_blake3: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SoundtrackSessionSpec {
    pub schema_version: u32,
    /// Existing cues/markers and calibrated clocks; never inferred from WAV headers.
    pub sync: SyncRequest,
    pub segments: Vec<SessionAudioSegment>,
    pub reference_seconds: u32,
    pub query_seconds: u32,
    pub query_step_seconds: u32,
    pub match_policy: SoundtrackPolicy,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionMatchQuery {
    pub segment_id: String,
    pub recording: SyncQuery,
    pub capture: SoundtrackCapture,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SoundtrackSessionPlan {
    pub schema_version: u32,
    pub recipe: String,
    pub plan_digest: String,
    pub spec: SoundtrackSessionSpec,
    pub references: Vec<SoundtrackReferenceSpec>,
    pub queries: Vec<SessionMatchQuery>,
    pub task_count: usize,
    /// Short segments remain evidence even when too short for passive matching.
    pub short_segment_ids: Vec<String>,
}
/// Local operator bindings only. Never send paths or reference features to a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionAudioFile {
    pub segment_id: String,
    pub path: PathBuf,
}
pub struct SessionBatch<'a> {
    pub movie: &'a Path,
    pub files: &'a [SessionAudioFile],
    pub cache: &'a Path,
    pub start_task: usize,
    pub max_tasks: usize,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionBatchResult {
    pub plan_digest: String,
    pub next_task: usize,
    pub task_count: usize,
    pub computed: usize,
    pub reused: usize,
    pub cancelled: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TaskReceipt {
    schema_version: u32,
    plan_digest: String,
    task: usize,
    report_digest: String,
    report: SoundtrackMatchReport,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionQueryEvidence {
    pub query_index: usize,
    pub recording: SyncQuery,
    pub completed_references: usize,
    pub total_references: usize,
    pub state: String,
    /// All report digests retain the unmodified raw alternatives in the cache.
    pub report_digests: Vec<String>,
    pub supporting_evidence_ids: BTreeMap<String, Vec<String>>,
    pub anchors: Vec<SyncAnchor>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionEvidencePage {
    pub schema_version: u32,
    pub plan_digest: String,
    pub next_query: Option<usize>,
    pub queries: Vec<SessionQueryEvidence>,
    /// Only fully searched queries contribute passive anchors. Merge pages by
    /// plan digest before mapping a longer session; never discard alternatives.
    pub sync: SyncRequest,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionSyncEvidence {
    pub schema_version: u32,
    pub plan_digest: String,
    pub relevant_queries: Vec<SessionQueryEvidence>,
    pub search_complete: bool,
    pub sync: SyncRequest,
}
fn digest<T: Serialize>(v: &T) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(v)?).to_hex().to_string())
}
fn opaque(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 128
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn valid_digest(v: &str) -> bool {
    v.len() == 64
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let m = std::fs::metadata(path)?;
    ensure!(
        m.is_file() && m.len() <= limit as u64,
        "invalid input: bounded regular evidence file required"
    );
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit,
        "invalid input: evidence file exceeds bound"
    );
    Ok(bytes)
}
fn publish<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().context("invalid cache path")?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&serde_json::to_vec(value)?)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}
impl SoundtrackSessionPlan {
    pub fn new(spec: SoundtrackSessionSpec) -> Result<Self> {
        spec.sync.validate()?;
        ensure!(
            spec.schema_version == 1 && !spec.segments.is_empty() && spec.segments.len() <= 512,
            "invalid input: session version/segment bound"
        );
        ensure!(
            (4..=20).contains(&spec.query_seconds)
                && (1..=spec.query_seconds).contains(&spec.query_step_seconds)
                && (spec.query_seconds + 1..=120).contains(&spec.reference_seconds),
            "invalid input: session window policy"
        );
        // Validate the matching policy through its single shared validator.
        spec.match_policy.validate()?;
        let duration = spec.sync.movie.duration_pts as f64 / spec.sync.movie.ticks_per_second();
        ensure!(
            duration >= f64::from(spec.query_seconds),
            "invalid input: movie shorter than query window"
        );
        let width = f64::from(spec.reference_seconds).min(duration);
        let step = width - f64::from(spec.query_seconds);
        ensure!(
            step > 0.0 || width == duration,
            "invalid input: reference coverage step"
        );
        let mut references = Vec::new();
        let mut start = 0.0;
        loop {
            references.push(SoundtrackReferenceSpec {
                schema_version: 1,
                movie: spec.sync.movie.clone(),
                start_seconds: start,
                duration_seconds: width,
            });
            if start + width >= duration - 1e-9 {
                break;
            }
            start = (start + step).min(duration - width);
            ensure!(
                references.len() < 512,
                "invalid input: too many reference windows"
            );
        }
        let mut ids = HashSet::new();
        let mut intervals: BTreeMap<&str, Vec<(u64, u64)>> = BTreeMap::new();
        let mut queries = Vec::new();
        let mut short_segment_ids = Vec::new();
        for segment in &spec.segments {
            ensure!(
                opaque(&segment.segment_id)
                    && ids.insert(&segment.segment_id)
                    && valid_digest(&segment.wav_blake3),
                "invalid input: segment identity"
            );
            let clock = spec
                .sync
                .capture_clocks
                .iter()
                .find(|c| c.clock_id == segment.clock_id)
                .context("invalid input: unknown segment clock")?;
            let end = segment
                .first_sample
                .checked_add(u64::from(segment.frame_count))
                .context("invalid input: segment overflow")?;
            ensure!(
                segment.frame_count > 0
                    && segment.first_sample >= clock.first_sample
                    && end <= clock.last_sample,
                "invalid input: segment outside original capture clock"
            );
            let ranges = intervals.entry(&segment.clock_id).or_default();
            ensure!(
                !ranges
                    .iter()
                    .any(|&(a, b)| segment.first_sample < b && a < end),
                "invalid input: overlapping audio segments"
            );
            ranges.push((segment.first_sample, end));
            let samples = u64::from(clock.sample_rate) * u64::from(spec.query_seconds);
            let mut points = vec![segment.first_sample, end];
            points.extend(
                spec.sync
                    .boundaries
                    .iter()
                    .filter(|b| {
                        b.clock_id == segment.clock_id
                            && b.sample_index > segment.first_sample
                            && b.sample_index < end
                    })
                    .map(|b| b.sample_index),
            );
            points.sort_unstable();
            points.dedup();
            for range in points.windows(2) {
                if range[1] - range[0] < samples {
                    if !short_segment_ids.contains(&segment.segment_id) {
                        short_segment_ids.push(segment.segment_id.clone());
                    }
                    continue;
                }
                let mut first = range[0];
                loop {
                    let identity = digest(&(
                        RECIPE,
                        &spec.sync.session_id,
                        &segment.segment_id,
                        first,
                        samples,
                        &segment.wav_blake3,
                    ))?;
                    queries.push(SessionMatchQuery {
                        segment_id: segment.segment_id.clone(),
                        recording: SyncQuery {
                            clock_id: clock.clock_id.clone(),
                            start_sample: first,
                            end_sample: first + samples,
                        },
                        capture: SoundtrackCapture {
                            evidence_id: format!("query_{identity}"),
                            clock_id: clock.clock_id.clone(),
                            sample_rate: clock.sample_rate,
                            first_sample: first,
                            monotonic_origin_ns: clock.monotonic_origin_ns,
                        },
                    });
                    ensure!(
                        queries.len() <= 4096,
                        "invalid input: at most 4096 matching windows; increase query step or narrow capture"
                    );
                    if first + samples == range[1] {
                        break;
                    }
                    first = (first
                        + u64::from(clock.sample_rate) * u64::from(spec.query_step_seconds))
                    .min(range[1] - samples);
                }
            }
        }
        let task_count = references
            .len()
            .checked_mul(queries.len())
            .context("invalid task count")?;
        ensure!(
            task_count <= 1_000_000,
            "invalid input: at most one million bounded tasks; narrow the session"
        );
        let plan_digest = digest(&(RECIPE, &spec))?;
        let plan = Self {
            schema_version: 1,
            recipe: RECIPE.into(),
            plan_digest,
            spec,
            references,
            queries,
            task_count,
            short_segment_ids,
        };
        ensure!(
            serde_json::to_vec(&plan)?.len() <= MAX_JSON,
            "invalid input: planned session exceeds 4 MiB; narrow capture/evidence scope"
        );
        Ok(plan)
    }
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= MAX_JSON,
            "invalid input: session plan exceeds 4 MiB"
        );
        let plan: Self = serde_json::from_slice(bytes)?;
        plan.validate()?;
        Ok(plan)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            Self::new(self.spec.clone())? == *self,
            "invalid input: session plan identity/coverage mismatch"
        );
        Ok(())
    }
    fn cache(&self, root: &Path) -> PathBuf {
        root.join(&self.plan_digest)
    }
    fn receipt_path(&self, root: &Path, task: usize) -> PathBuf {
        self.cache(root).join(format!("task-{task}.json"))
    }
    fn receipt(&self, root: &Path, task: usize) -> Result<Option<TaskReceipt>> {
        let path = self.receipt_path(root, task);
        if !path.try_exists()? {
            return Ok(None);
        }
        let receipt: TaskReceipt = serde_json::from_slice(&read_bounded(&path, 65536)?)?;
        let report = &receipt.report;
        let query = &self.queries[task / self.references.len()];
        let reference = &self.references[task % self.references.len()];
        ensure!(
            receipt.schema_version == 1
                && receipt.plan_digest == self.plan_digest
                && receipt.task == task
                && receipt.report_digest == digest(report)?
                && report.schema_version == 1
                && report.recipe == crate::tv_soundtrack::SOUNDTRACK_RECIPE
                && report.reference_movie_digest == self.spec.sync.movie.movie_digest
                && report.capture == query.capture
                && report.policy == self.spec.match_policy
                && report.query_sample_count
                    == self.spec.query_seconds as usize * PCM_RATE as usize
                && report.candidates.len() <= self.spec.match_policy.maximum_candidates,
            "invalid input: cached match receipt identity mismatch"
        );
        let check = SyncRequest {
            schema_version: 1,
            session_id: self.spec.sync.session_id.clone(),
            target_digest: self.spec.sync.target_digest.clone(),
            movie: self.spec.sync.movie.clone(),
            capture_clocks: self.spec.sync.capture_clocks.clone(),
            anchors: report.candidates.iter().map(|c| c.anchor.clone()).collect(),
            boundaries: vec![],
            policy: self.spec.sync.policy.clone(),
        };
        check.validate()?;
        let tps = self.spec.sync.movie.ticks_per_second();
        for c in &report.candidates {
            let seconds = (c.anchor.movie_pts - self.spec.sync.movie.origin_pts) as f64 / tps;
            ensure!(
                seconds >= reference.start_seconds
                    && seconds <= reference.start_seconds + reference.duration_seconds
                    && c.anchor.capture.clock_id == query.recording.clock_id
                    && c.anchor.capture.sample_index
                        == (query.recording.start_sample + query.recording.end_sample) / 2,
                "invalid input: cached candidate outside searched window"
            );
        }
        Ok(Some(receipt))
    }
    /// The caller holds the shared heavy lease. One batch computes at most 128
    /// work units. A cancelled batch retains all committed receipts and cursor.
    pub fn run_batch(
        &self,
        batch: SessionBatch<'_>,
        cancel: &dyn Fn() -> bool,
    ) -> Result<SessionBatchResult> {
        let SessionBatch {
            movie,
            files,
            cache,
            start_task,
            max_tasks,
        } = batch;
        self.validate()?;
        ensure!(
            start_task <= self.task_count && (1..=128).contains(&max_tasks),
            "invalid input: batch cursor/budget"
        );
        ensure!(
            files.len() == self.spec.segments.len(),
            "invalid input: bind every declared audio segment exactly once"
        );
        let mut bindings = BTreeMap::new();
        for file in files {
            ensure!(
                bindings
                    .insert(file.segment_id.as_str(), file.path.as_path())
                    .is_none(),
                "invalid input: duplicate segment file binding"
            );
        }
        for s in &self.spec.segments {
            ensure!(
                bindings.contains_key(s.segment_id.as_str()),
                "invalid input: missing audio segment"
            );
        }
        std::fs::create_dir_all(self.cache(cache))?;
        let mut result = SessionBatchResult {
            plan_digest: self.plan_digest.clone(),
            next_task: start_task,
            task_count: self.task_count,
            computed: 0,
            reused: 0,
            cancelled: false,
        };
        let mut verified_movie = None;
        let mut verified_segments: BTreeMap<String, std::fs::Metadata> = BTreeMap::new();
        let mut query_pcm: Option<(usize, Vec<f32>)> = None;
        for task in start_task..(start_task + max_tasks).min(self.task_count) {
            if cancel() {
                result.cancelled = true;
                break;
            }
            if self.receipt(cache, task)?.is_some() {
                result.reused += 1;
                result.next_task = task + 1;
                continue;
            }
            let qi = task / self.references.len();
            let ri = task % self.references.len();
            let q = &self.queries[qi];
            let s = self
                .spec
                .segments
                .iter()
                .find(|s| s.segment_id == q.segment_id)
                .unwrap();
            let path = bindings[s.segment_id.as_str()];
            if !verified_segments.contains_key(&s.segment_id) {
                let metadata = std::fs::metadata(path)?;
                ensure!(
                    full_file_blake3(path, cancel)?.hex_digest == s.wav_blake3,
                    "invalid input: original capture WAV digest mismatch"
                );
                verified_segments.insert(s.segment_id.clone(), metadata);
            }
            let baseline = &verified_segments[&s.segment_id];
            let observed = std::fs::metadata(path)?;
            ensure!(
                baseline.len() == observed.len() && baseline.modified()? == observed.modified()?,
                "input_changed"
            );
            if query_pcm.as_ref().is_none_or(|(i, _)| *i != qi) {
                query_pcm = Some((qi, decode_query(path, s, q, cancel)?));
            }
            let index_path = self.cache(cache).join(format!("index-{ri}.json"));
            let index = if index_path.try_exists()? {
                SoundtrackIndex::from_json(&read_bounded(&index_path, 16 * 1024 * 1024)?)?
            } else {
                if verified_movie.is_none() {
                    verified_movie = Some(SoundtrackMovie::open(
                        movie,
                        self.spec.sync.movie.clone(),
                        cancel,
                    )?);
                }
                let index = verified_movie
                    .as_ref()
                    .unwrap()
                    .index(self.references[ri].clone(), cancel)?;
                publish(&index_path, &index)?;
                index
            };
            ensure!(
                index.reference == self.references[ri],
                "invalid input: cached reference mismatch"
            );
            let report = index.match_pcm(
                &query_pcm.as_ref().unwrap().1,
                q.capture.clone(),
                &self.spec.match_policy,
                cancel,
            )?;
            let receipt = TaskReceipt {
                schema_version: 1,
                plan_digest: self.plan_digest.clone(),
                task,
                report_digest: digest(&report)?,
                report,
            };
            publish(&self.receipt_path(cache, task), &receipt)?;
            result.computed += 1;
            result.next_task = task + 1;
        }
        Ok(result)
    }
    /// A page remains explicitly pending until every movie reference was
    /// searched for each query. Overlap confirmations are coalesced with raw
    /// receipts retained; distant repetitions remain conflicting alternatives.
    pub fn evidence_page(
        &self,
        cache: &Path,
        first: usize,
        count: usize,
        cancel: &dyn Fn() -> bool,
    ) -> Result<SessionEvidencePage> {
        self.validate()?;
        ensure!(
            first <= self.queries.len() && (1..=64).contains(&count),
            "invalid input: evidence page cursor/bound"
        );
        self.evidence_page_validated(cache, first, count, cancel)
    }
    fn evidence_page_validated(
        &self,
        cache: &Path,
        first: usize,
        count: usize,
        cancel: &dyn Fn() -> bool,
    ) -> Result<SessionEvidencePage> {
        let end = (first + count).min(self.queries.len());
        let mut page = SessionEvidencePage {
            schema_version: 1,
            plan_digest: self.plan_digest.clone(),
            next_query: (end < self.queries.len()).then_some(end),
            queries: vec![],
            sync: self.spec.sync.clone(),
        };
        // A missing-audio interval cannot be bridged by neighboring matches.
        for clock in &self.spec.sync.capture_clocks {
            let mut ranges: Vec<_> = self
                .spec
                .segments
                .iter()
                .filter(|s| s.clock_id == clock.clock_id)
                .map(|s| (s.first_sample, s.first_sample + u64::from(s.frame_count)))
                .collect();
            ranges.sort_unstable();
            for pair in ranges.windows(2).filter(|p| p[0].1 < p[1].0) {
                for sample in [pair[0].1, pair[1].0] {
                    page.sync.boundaries.push(SyncBoundary {
                        evidence_id: format!(
                            "audio_gap_{}",
                            digest(&(&self.plan_digest, &clock.clock_id, sample))?
                        ),
                        clock_id: clock.clock_id.clone(),
                        sample_index: sample,
                        kind: SyncBoundaryKind::CaptureInterrupted,
                    });
                }
            }
        }
        for qi in first..end {
            ensure!(!cancel(), "session evidence cancelled");
            let q = &self.queries[qi];
            let mut evidence = SessionQueryEvidence {
                query_index: qi,
                recording: q.recording.clone(),
                completed_references: 0,
                total_references: self.references.len(),
                state: "pending".into(),
                report_digests: vec![],
                supporting_evidence_ids: BTreeMap::new(),
                anchors: vec![],
            };
            let mut candidates = Vec::new();
            for ri in 0..self.references.len() {
                ensure!(!cancel(), "session evidence cancelled");
                if let Some(receipt) = self.receipt(cache, qi * self.references.len() + ri)? {
                    evidence.completed_references += 1;
                    evidence.report_digests.push(receipt.report_digest);
                    candidates.extend(receipt.report.candidates.into_iter().map(|c| c.anchor));
                }
            }
            if evidence.completed_references == evidence.total_references {
                candidates.sort_by_key(|a| a.movie_pts);
                // Only coalesce observations at the same search-grid location.
                // Never merge nearby but disjoint true alternatives by uncertainty.
                let tolerance = (0.020 * self.spec.sync.movie.ticks_per_second()).ceil() as u64;
                let mut clusters: Vec<Vec<SyncAnchor>> = vec![];
                for a in candidates {
                    if let Some(cluster) = clusters
                        .last_mut()
                        .filter(|c| c[0].movie_pts.abs_diff(a.movie_pts) <= tolerance)
                    {
                        cluster.push(a);
                    } else {
                        clusters.push(vec![a]);
                    }
                }
                if clusters.len() > 64 {
                    evidence.state = "tooManyAlternatives".into();
                } else {
                    for cluster in clusters {
                        let mut anchor = cluster
                            .iter()
                            .max_by(|a, b| a.confidence.total_cmp(&b.confidence))
                            .unwrap()
                            .clone();
                        let ids: Vec<_> = cluster.iter().map(|a| &a.evidence_id).collect();
                        anchor.evidence_id =
                            format!("session_match_{}", digest(&(&self.plan_digest, qi, ids))?);
                        evidence.supporting_evidence_ids.insert(
                            anchor.evidence_id.clone(),
                            cluster.iter().map(|a| a.evidence_id.clone()).collect(),
                        );
                        anchor.movie_uncertainty_ticks = cluster
                            .iter()
                            .map(|a| {
                                a.movie_uncertainty_ticks + a.movie_pts.abs_diff(anchor.movie_pts)
                            })
                            .max()
                            .unwrap();
                        evidence.anchors.push(anchor);
                    }
                    evidence.state = match evidence.anchors.len() {
                        0 => "unmatched",
                        1 => "candidate",
                        _ => "ambiguous",
                    }
                    .into();
                    page.sync.anchors.extend(evidence.anchors.clone());
                }
            }
            page.queries.push(evidence);
        }
        page.sync.validate()?;
        Ok(page)
    }
    /// Collect only windows capable of supporting the requested utterance
    /// intervals. Pending searches cannot supply a supposedly unique anchor.
    pub fn sync_evidence(
        &self,
        cache: &Path,
        utterances: &[SyncQuery],
        cancel: &dyn Fn() -> bool,
    ) -> Result<SessionSyncEvidence> {
        self.validate()?;
        self.spec.sync.map(utterances, cancel)?; // Shared validation, including query quotas.
        let mut result = SessionSyncEvidence {
            schema_version: 1,
            plan_digest: self.plan_digest.clone(),
            relevant_queries: vec![],
            search_complete: true,
            sync: self.spec.sync.clone(),
        };
        let mut ids: HashSet<String> = result
            .sync
            .anchors
            .iter()
            .map(|a| a.evidence_id.clone())
            .chain(result.sync.boundaries.iter().map(|a| a.evidence_id.clone()))
            .collect();
        for (qi, q) in self.queries.iter().enumerate() {
            let clock = self
                .spec
                .sync
                .capture_clocks
                .iter()
                .find(|c| c.clock_id == q.capture.clock_id)
                .unwrap();
            let margin = (self
                .spec
                .sync
                .policy
                .max_anchor_gap_seconds
                .max(self.spec.sync.policy.max_extrapolation_seconds)
                * f64::from(clock.sample_rate))
            .ceil() as u64;
            let center = (q.recording.start_sample + q.recording.end_sample) / 2;
            if !utterances.iter().any(|u| {
                u.clock_id == q.capture.clock_id
                    && center >= u.start_sample.saturating_sub(margin)
                    && center <= u.end_sample.saturating_add(margin)
            }) {
                continue;
            }
            ensure!(
                result.relevant_queries.len() < 128,
                "invalid input: at most 128 relevant passive windows; narrow utterance scope"
            );
            let page = self.evidence_page_validated(cache, qi, 1, cancel)?;
            for a in page.sync.anchors {
                if ids.insert(a.evidence_id.clone()) {
                    result.sync.anchors.push(a);
                }
            }
            for b in page.sync.boundaries {
                if ids.insert(b.evidence_id.clone()) {
                    result.sync.boundaries.push(b);
                }
            }
            let evidence = page.queries.into_iter().next().unwrap();
            result.search_complete &= evidence.completed_references == evidence.total_references;
            result.relevant_queries.push(evidence);
        }
        result.search_complete &= !result.relevant_queries.is_empty();
        result.sync.validate()?;
        Ok(result)
    }
}

fn decode_query(
    path: &Path,
    segment: &SessionAudioSegment,
    query: &SessionMatchQuery,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<f32>> {
    let reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    ensure!(
        spec.sample_rate == query.capture.sample_rate
            && (1..=2).contains(&spec.channels)
            && reader.duration() == segment.frame_count,
        "invalid input: WAV rate/channels/frames ({}/{}/{}) differ from native declaration ({}/1..2/{})",
        spec.sample_rate,
        spec.channels,
        reader.duration(),
        query.capture.sample_rate,
        segment.frame_count
    );
    let before = std::fs::metadata(path)?;
    let mut command = crate::tool_command("ffmpeg");
    command
        .args([
            "-v",
            "error",
            "-nostdin",
            "-protocol_whitelist",
            "file,pipe",
            "-ss",
        ])
        .arg(format!(
            "{:.12}",
            (query.recording.start_sample - segment.first_sample) as f64
                / f64::from(spec.sample_rate)
        ))
        .arg("-i")
        .arg(path)
        .arg("-t")
        .arg(format!(
            "{:.12}",
            (query.recording.end_sample - query.recording.start_sample) as f64
                / f64::from(spec.sample_rate)
        ))
        .args([
            "-map", "0:a:0", "-vn", "-ac", "1", "-ar", "8000", "-f", "f32le", "pipe:1",
        ]);
    let out = crate::stall::output_with_timeout_and_cancellation(
        &mut command,
        "native microphone window decode",
        std::time::Duration::from_secs(60),
        &cancel,
    )?;
    ensure!(out.status.success(), "media tool failed: microphone decode");
    let expected = ((query.recording.end_sample - query.recording.start_sample)
        * u64::from(PCM_RATE)
        / u64::from(spec.sample_rate)) as usize;
    ensure!(
        out.stdout.len() == expected * 4,
        "invalid input: resampled query duration mismatch"
    );
    let after = std::fs::metadata(path)?;
    ensure!(
        before.len() == after.len() && before.modified()? == after.modified()?,
        "input_changed"
    );
    Ok(out
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect())
}

#[cfg(test)]
#[path = "tv_soundtrack_session_tests.rs"]
mod tests;
