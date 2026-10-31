//! Durable audiovisual review coverage, distinct from notes and plan decisions.
//!
//! Coverage records are revision/artifact bound. Only an explicit human
//! playback attestation can satisfy a human review gap; machine observations
//! remain useful provenance but never become a human listening/picture claim.
use super::{EditPlanRevision, PreviewArtifactState, ReviewDecisionActor};
use crate::Catalog;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MAX_INTERVALS: usize = 256;
const MAX_EVIDENCE_REFS: usize = 32;
const MAX_PAGE: usize = 100;
const MAX_QUEUE: usize = 100;
const MAX_HISTORICAL_ROWS: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewCoverageModality {
    Picture,
    Listening,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewCoverageKind {
    Sampled,
    Continuous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewCoverageMethod {
    HumanPlayback,
    AgentReview,
    ModelReview,
    SparseSampling,
    TranscriptInspection,
    DecodeCheck,
    EnergyCheck,
}

impl ReviewCoverageMethod {
    fn is_human_playback(self) -> bool {
        self == Self::HumanPlayback
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewCoverageInterval {
    pub modality: ReviewCoverageModality,
    /// Interval on the verified preview movie clock, in seconds.
    pub movie_start_seconds: f64,
    pub movie_end_seconds: f64,
    pub coverage: ReviewCoverageKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordReviewCoverageRequest {
    pub project_id: String,
    pub revision_id: String,
    pub operation_id: String,
    pub artifact_id: String,
    pub actor: ReviewDecisionActor,
    pub method: ReviewCoverageMethod,
    /// Must be explicitly set by the human playback caller. A method name or
    /// an agent-provided actor value alone is not an attestation.
    #[serde(default)]
    pub explicit_human_playback: bool,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub uncertainty: Option<String>,
    pub intervals: Vec<ReviewCoverageInterval>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCoverageRecord {
    pub record_id: String,
    pub project_id: String,
    pub revision_id: String,
    pub operation_id: String,
    pub artifact_id: String,
    pub actor: ReviewDecisionActor,
    pub method: ReviewCoverageMethod,
    pub explicit_human_playback: bool,
    pub recorded_at: String,
    pub dependency_key: String,
    pub evidence_refs: Vec<String>,
    pub uncertainty: Option<String>,
    pub intervals: Vec<ReviewCoverageInterval>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCoverageModalityReport {
    pub continuous_covered_seconds: f64,
    pub sampled_seconds: f64,
    pub gap_count: usize,
    pub gaps: Vec<ReviewCoverageGap>,
    pub gaps_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCoverageGap {
    pub movie_start_seconds: f64,
    pub movie_end_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCoverageCutReport {
    pub operation_id: String,
    pub position: u32,
    pub asset_id: String,
    pub movie_start_seconds: f64,
    pub movie_end_seconds: f64,
    pub mapping: String,
    pub picture: ReviewCoverageModalityReport,
    pub listening: ReviewCoverageModalityReport,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCoverageQueueItem {
    pub operation_id: String,
    pub modality: ReviewCoverageModality,
    pub movie_start_seconds: f64,
    pub movie_end_seconds: f64,
    pub duration_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCoverageReport {
    pub schema_ready: bool,
    pub project_id: String,
    pub revision_id: String,
    pub verified: bool,
    pub history_truncated: bool,
    pub cuts: Vec<ReviewCoverageCutReport>,
    pub total_gaps: usize,
    pub next_review_queue: Vec<ReviewCoverageQueueItem>,
    pub queue_truncated: bool,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCoveragePage {
    pub schema_ready: bool,
    pub revision_id: String,
    pub total: usize,
    pub next_offset: Option<usize>,
    pub items: Vec<ReviewCoverageRecord>,
}

/// Migration 64. Root migration dispatch owns when this is called.
pub(crate) fn migrate(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS edit_review_coverage (
            record_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            revision_id TEXT NOT NULL REFERENCES edit_plan_revisions(revision_id) ON DELETE CASCADE,
            operation_id TEXT NOT NULL,
            artifact_id TEXT NOT NULL,
            actor TEXT NOT NULL CHECK(actor IN ('human','agent','unknown')),
            method TEXT NOT NULL CHECK(method IN ('humanPlayback','agentReview','modelReview','sparseSampling','transcriptInspection','decodeCheck','energyCheck')),
            explicit_human_playback INTEGER NOT NULL CHECK(explicit_human_playback IN (0,1)),
            recorded_at TEXT NOT NULL,
            dependency_key TEXT NOT NULL,
            target_json TEXT NOT NULL,
            source_key TEXT NOT NULL,
            evidence_json TEXT NOT NULL,
            uncertainty TEXT
        );
        CREATE INDEX IF NOT EXISTS edit_review_coverage_revision
            ON edit_review_coverage(revision_id, recorded_at, record_id);
        CREATE TABLE IF NOT EXISTS edit_review_coverage_interval (
            record_id TEXT NOT NULL REFERENCES edit_review_coverage(record_id) ON DELETE CASCADE,
            ordinal INTEGER NOT NULL,
            modality TEXT NOT NULL CHECK(modality IN ('picture','listening')),
            coverage TEXT NOT NULL CHECK(coverage IN ('sampled','continuous')),
            movie_start REAL NOT NULL CHECK(movie_start >= 0),
            movie_end REAL NOT NULL CHECK(movie_end > movie_start),
            relative_start REAL NOT NULL,
            relative_end REAL NOT NULL,
            PRIMARY KEY(record_id, ordinal)
        );"
    )?;
    Ok(())
}

fn component(value: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty() && value.len() <= 255 && !value.chars().any(char::is_control),
        "invalid input: review coverage identity is invalid"
    );
    Ok(())
}

fn id() -> String {
    format!("coverage_{}", Uuid::new_v4().simple())
}

fn kind_name(kind: ReviewCoverageKind) -> &'static str {
    match kind {
        ReviewCoverageKind::Sampled => "sampled",
        ReviewCoverageKind::Continuous => "continuous",
    }
}
fn modality_name(modality: ReviewCoverageModality) -> &'static str {
    match modality {
        ReviewCoverageModality::Picture => "picture",
        ReviewCoverageModality::Listening => "listening",
    }
}
fn method_name(method: ReviewCoverageMethod) -> &'static str {
    match method {
        ReviewCoverageMethod::HumanPlayback => "humanPlayback",
        ReviewCoverageMethod::AgentReview => "agentReview",
        ReviewCoverageMethod::ModelReview => "modelReview",
        ReviewCoverageMethod::SparseSampling => "sparseSampling",
        ReviewCoverageMethod::TranscriptInspection => "transcriptInspection",
        ReviewCoverageMethod::DecodeCheck => "decodeCheck",
        ReviewCoverageMethod::EnergyCheck => "energyCheck",
    }
}
fn parse_modality(s: &str) -> Result<ReviewCoverageModality> {
    match s {
        "picture" => Ok(ReviewCoverageModality::Picture),
        "listening" => Ok(ReviewCoverageModality::Listening),
        _ => anyhow::bail!("invalid coverage modality"),
    }
}
fn parse_kind(s: &str) -> Result<ReviewCoverageKind> {
    match s {
        "sampled" => Ok(ReviewCoverageKind::Sampled),
        "continuous" => Ok(ReviewCoverageKind::Continuous),
        _ => anyhow::bail!("invalid coverage kind"),
    }
}
fn parse_method(s: &str) -> Result<ReviewCoverageMethod> {
    match s {
        "humanPlayback" => Ok(ReviewCoverageMethod::HumanPlayback),
        "agentReview" => Ok(ReviewCoverageMethod::AgentReview),
        "modelReview" => Ok(ReviewCoverageMethod::ModelReview),
        "sparseSampling" => Ok(ReviewCoverageMethod::SparseSampling),
        "transcriptInspection" => Ok(ReviewCoverageMethod::TranscriptInspection),
        "decodeCheck" => Ok(ReviewCoverageMethod::DecodeCheck),
        "energyCheck" => Ok(ReviewCoverageMethod::EnergyCheck),
        _ => anyhow::bail!("invalid coverage method"),
    }
}
fn parse_actor(s: &str) -> Result<ReviewDecisionActor> {
    match s {
        "human" => Ok(ReviewDecisionActor::Human),
        "agent" => Ok(ReviewDecisionActor::Agent),
        "unknown" => Ok(ReviewDecisionActor::Unknown),
        _ => anyhow::bail!("invalid coverage actor"),
    }
}

fn normalized_target(plan: &EditPlanRevision, operation_id: &str) -> Result<super::ReviewTarget> {
    let index = plan
        .operations
        .iter()
        .position(|cut| cut.operation_id == operation_id)
        .context("cut not found in revision")?;
    let mut target = super::feedback::target(plan, operation_id)?;
    let mut starts = Vec::with_capacity(plan.operations.len());
    let mut cursor = 0.0;
    for cut in &plan.operations {
        starts.push(cursor);
        cursor += cut.source_out_seconds - cut.source_in_seconds;
    }
    let normalize = |snapshot: &mut super::ReviewCutSnapshot, start: f64| {
        snapshot.position = 0;
        if let Some(super::EditPlanAudio::Source(audio)) = &mut snapshot.audio {
            audio.timeline_start_seconds -= start;
        }
    };
    normalize(&mut target.cut, starts[index]);
    if let (Some(previous), Some(previous_index)) = (&mut target.previous_cut, index.checked_sub(1))
    {
        normalize(previous, starts[previous_index]);
    }
    if let (Some(next), Some(next_index)) = (
        &mut target.next_cut,
        plan.operations.get(index + 1).map(|_| index + 1),
    ) {
        normalize(next, starts[next_index]);
    }
    Ok(target)
}

fn source_dependency_key(
    plan: &EditPlanRevision,
    cut: &super::EditPlanOperation,
    artifact: &super::PreviewArtifact,
    source_fingerprint: &str,
    rotation_degrees: u16,
) -> Result<String> {
    let source = artifact
        .identity
        .as_ref()
        .and_then(|identity| identity.sources.iter().find(|s| s.asset_id == cut.asset_id))
        .context("preview artifact has no source representation for cut")?;
    let identity = artifact
        .identity
        .as_ref()
        .context("preview artifact has no input identity")?;
    let mut relevant_ids = std::collections::BTreeSet::from([cut.asset_id.clone()]);
    if let Some(previous) = plan
        .operations
        .iter()
        .position(|o| o.operation_id == cut.operation_id)
        .and_then(|index| index.checked_sub(1))
        .and_then(|index| plan.operations.get(index))
    {
        relevant_ids.insert(previous.asset_id.clone());
    }
    if let Some(next) = plan
        .operations
        .iter()
        .position(|o| o.operation_id == cut.operation_id)
        .and_then(|index| plan.operations.get(index + 1))
    {
        relevant_ids.insert(next.asset_id.clone());
    }
    relevant_ids.extend(super::feedback::audio_context_sources(
        plan,
        &cut.operation_id,
    )?);
    let mut representation_dependencies = identity
        .sources
        .iter()
        .filter(|source| relevant_ids.contains(&source.asset_id))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        representation_dependencies
            .iter()
            .any(|source| source.asset_id == cut.asset_id),
        "preview artifact lacks the reviewed cut's source representation"
    );
    representation_dependencies.sort_by(|a, b| a.asset_id.cmp(&b.asset_id));
    Ok(blake3::hash(&serde_json::to_vec(&serde_json::json!({
        "sourceFingerprint": source_fingerprint,
        "rotationDegrees": rotation_degrees,
        "representation": source,
        "representationDependencies": representation_dependencies,
        "renderRecipe": identity.recipe,
        "maxHeight": identity.max_height,
        "tools": identity.tools,
        "toolContentFingerprints": identity.tool_content_fingerprints,
    }))?)
    .to_hex()
    .to_string())
}

fn dependency_key(target: &super::ReviewTarget, source_key: &str) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(&serde_json::json!({
        "target": target,
        "sourceKey": source_key,
    }))?)
    .to_hex()
    .to_string())
}

fn planned_preview_bounds(plan: &EditPlanRevision) -> Vec<(f64, f64)> {
    let mut cumulative_seconds = 0.0;
    let mut previous_frame = 0_u64;
    plan.operations
        .iter()
        .map(|cut| {
            cumulative_seconds += cut.source_out_seconds - cut.source_in_seconds;
            let end_frame = (cumulative_seconds * 30000.0 / 1001.0).round().max(0.0) as u64;
            let start = previous_frame as f64 * 1001.0 / 30000.0;
            let end = end_frame as f64 * 1001.0 / 30000.0;
            previous_frame = end_frame;
            (start, end)
        })
        .collect()
}

fn load_record(connection: &Connection, record_id: &str) -> Result<ReviewCoverageRecord> {
    let row: (String,String,String,String,String,String,i64,String,String,String,Option<String>) = connection.query_row(
        "SELECT project_id,revision_id,operation_id,artifact_id,actor,method,explicit_human_playback,recorded_at,dependency_key,evidence_json,uncertainty FROM edit_review_coverage WHERE record_id=?1", [record_id],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?)))
        .context("read review coverage")?;
    let mut stmt = connection.prepare("SELECT modality,coverage,movie_start,movie_end FROM edit_review_coverage_interval WHERE record_id=?1 ORDER BY ordinal")?;
    let rows = stmt.query_map([record_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, f64>(2)?,
            r.get::<_, f64>(3)?,
        ))
    })?;
    let mut intervals = Vec::new();
    for row in rows {
        let (m, k, a, b) = row?;
        intervals.push(ReviewCoverageInterval {
            modality: parse_modality(&m)?,
            coverage: parse_kind(&k)?,
            movie_start_seconds: a,
            movie_end_seconds: b,
        });
    }
    Ok(ReviewCoverageRecord {
        record_id: record_id.into(),
        project_id: row.0,
        revision_id: row.1,
        operation_id: row.2,
        artifact_id: row.3,
        actor: parse_actor(&row.4)?,
        method: parse_method(&row.5)?,
        explicit_human_playback: row.6 != 0,
        recorded_at: row.7,
        dependency_key: row.8,
        evidence_refs: serde_json::from_str(&row.9)?,
        uncertainty: row.10,
        intervals,
    })
}

struct HistoricalCoverage {
    record: ReviewCoverageRecord,
    target: super::ReviewTarget,
    source_key: String,
    relative_intervals: Vec<(ReviewCoverageInterval, f64, f64)>,
}

fn load_project_records(
    connection: &Connection,
    project_id: &str,
) -> Result<(Vec<HistoricalCoverage>, bool)> {
    let total: usize = connection
        .query_row(
            "SELECT COUNT(*) FROM edit_review_coverage WHERE project_id=?1",
            [project_id],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        .max(0) as usize;
    let mut statement = connection.prepare(
        "SELECT record_id,target_json,source_key FROM edit_review_coverage WHERE project_id=?1 ORDER BY recorded_at DESC,record_id DESC LIMIT ?2",
    )?;
    let rows = statement.query_map(params![project_id, MAX_HISTORICAL_ROWS as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut records = Vec::new();
    for row in rows {
        let (id, target_json, source_key) = row?;
        let target = serde_json::from_str(&target_json).context("invalid stored review target")?;
        let record = load_record(connection, &id)?;
        let mut intervals = Vec::new();
        let mut stmt = connection.prepare("SELECT modality,coverage,movie_start,movie_end,relative_start,relative_end FROM edit_review_coverage_interval WHERE record_id=?1 ORDER BY ordinal")?;
        let values = stmt.query_map([&id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, f64>(5)?,
            ))
        })?;
        for value in values {
            let (modality, coverage, movie_start, movie_end, relative_start, relative_end) = value?;
            intervals.push((
                ReviewCoverageInterval {
                    modality: parse_modality(&modality)?,
                    coverage: parse_kind(&coverage)?,
                    movie_start_seconds: movie_start,
                    movie_end_seconds: movie_end,
                },
                relative_start,
                relative_end,
            ));
        }
        records.push(HistoricalCoverage {
            record,
            target,
            source_key,
            relative_intervals: intervals,
        });
    }
    Ok((records, total > MAX_HISTORICAL_ROWS))
}

impl Catalog {
    /// Records bounded intervals against a completed, freshly verified preview.
    pub fn record_review_coverage(
        &self,
        request: &RecordReviewCoverageRequest,
    ) -> Result<ReviewCoverageRecord> {
        for value in [
            &request.project_id,
            &request.revision_id,
            &request.operation_id,
            &request.artifact_id,
        ] {
            component(value)?;
        }
        ensure!(
            !request.intervals.is_empty() && request.intervals.len() <= MAX_INTERVALS,
            "invalid input: coverage requires 1-256 intervals"
        );
        ensure!(
            request.evidence_refs.len() <= MAX_EVIDENCE_REFS
                && request
                    .evidence_refs
                    .iter()
                    .all(|x| !x.trim().is_empty() && x.len() <= 1024),
            "invalid input: evidence references exceed bounds"
        );
        ensure!(
            request.uncertainty.as_ref().is_none_or(|s| s.len() <= 4096),
            "invalid input: uncertainty exceeds 4096 bytes"
        );
        ensure!(
            request.explicit_human_playback
                == (request.actor == ReviewDecisionActor::Human
                    && request.method.is_human_playback()),
            "invalid input: only explicit human playback may attest human review"
        );
        let plan = self.edit_plan_revision(&request.revision_id)?;
        ensure!(
            plan.project_id == request.project_id,
            "invalid input: revision belongs to another project"
        );
        let status = self.preview_artifact_status(&request.artifact_id, true)?;
        ensure!(
            status.observed_state == PreviewArtifactState::Complete
                && status.current_for_request == Some(true),
            "invalid input: review coverage requires a current verified completed artifact"
        );
        let artifact = status.artifact;
        ensure!(
            artifact.project_id == request.project_id
                && artifact.revision_id == request.revision_id,
            "invalid input: artifact is bound to another revision"
        );
        let mapping = artifact
            .validation
            .as_ref()
            .and_then(|v| {
                v.cuts
                    .iter()
                    .find(|c| c.operation_id == request.operation_id)
            })
            .context("invalid input: artifact does not map the requested cut")?;
        let validation = artifact
            .validation
            .as_ref()
            .context("verified artifact has no validation")?;
        let duration = validation.duration_seconds;
        let movie_seconds = |pts: i64| {
            (pts - validation.movie_origin_pts) as f64 * validation.movie_time_base_numerator as f64
                / validation.movie_time_base_denominator.max(1) as f64
        };
        let cut_start = movie_seconds(mapping.movie_in_pts);
        let cut_end = movie_seconds(mapping.movie_out_pts);
        for interval in &request.intervals {
            ensure!(
                interval.movie_start_seconds.is_finite()
                    && interval.movie_end_seconds.is_finite()
                    && interval.movie_start_seconds >= 0.0
                    && interval.movie_end_seconds > interval.movie_start_seconds
                    && interval.movie_end_seconds <= duration + 1e-6,
                "invalid input: coverage interval is outside preview movie"
            );
            ensure!(
                interval.movie_start_seconds < cut_end && interval.movie_end_seconds > cut_start,
                "invalid input: interval does not overlap cut"
            );
        }
        let cut = plan
            .operations
            .iter()
            .find(|c| c.operation_id == request.operation_id)
            .context("cut not in revision")?;
        let target = normalized_target(&plan, cut.operation_id.as_str())?;
        let asset = self.get_asset(&cut.asset_id)?;
        let source_key = source_dependency_key(
            &plan,
            cut,
            &artifact,
            &asset.fingerprint,
            asset.rotation_degrees,
        )?;
        let dependency = dependency_key(&target, &source_key)?;
        let cut_start = movie_seconds(mapping.movie_in_pts);
        let cut_end = movie_seconds(mapping.movie_out_pts);
        let now = crate::now();
        let record_id = id();
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO edit_review_coverage(record_id,project_id,revision_id,operation_id,artifact_id,actor,method,explicit_human_playback,recorded_at,dependency_key,target_json,source_key,evidence_json,uncertainty) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)", params![record_id,request.project_id,request.revision_id,request.operation_id,request.artifact_id,match request.actor { ReviewDecisionActor::Human=>"human",ReviewDecisionActor::Agent=>"agent",ReviewDecisionActor::Unknown=>"unknown" },method_name(request.method),request.explicit_human_playback as i64,now,dependency,serde_json::to_string(&target)?,source_key,serde_json::to_string(&request.evidence_refs)?,request.uncertainty])?;
        for (ordinal, interval) in request.intervals.iter().enumerate() {
            let relative_start = interval.movie_start_seconds.max(cut_start) - cut_start;
            let relative_end = interval.movie_end_seconds.min(cut_end) - cut_start;
            tx.execute("INSERT INTO edit_review_coverage_interval(record_id,ordinal,modality,coverage,movie_start,movie_end,relative_start,relative_end) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)", params![record_id,ordinal as i64,modality_name(interval.modality),kind_name(interval.coverage),interval.movie_start_seconds,interval.movie_end_seconds,relative_start,relative_end])?;
        }
        tx.commit()?;
        load_record(&self.connection()?, &record_id)
    }

    /// Returns per-cut gaps. Cross-revision carry is conservative: a record is
    /// eligible only when its cut dependency digest is identical. Historical
    /// records remain queryable regardless of currentness.
    pub fn review_coverage_report(
        &self,
        revision_id: &str,
        verify: bool,
    ) -> Result<ReviewCoverageReport> {
        let plan = self.edit_plan_revision(revision_id)?;
        let preview_page = self.preview_artifacts(revision_id, 0, MAX_PAGE)?;
        let mut current_artifacts = Vec::new();
        for item in preview_page.items {
            if item.observed_state != PreviewArtifactState::Complete {
                continue;
            }
            if let Ok(status) = self.preview_artifact_status(&item.artifact_id, verify) {
                if status.observed_state == PreviewArtifactState::Complete
                    && (!verify || status.current_for_request == Some(true))
                {
                    current_artifacts.push(status.artifact);
                }
            }
        }
        let connection = self.connection()?;
        if !crate::table_exists(&connection, "edit_review_coverage")? {
            return Ok(ReviewCoverageReport {
                schema_ready: false,
                project_id: plan.project_id,
                revision_id: revision_id.into(),
                verified: false,
                history_truncated: false,
                cuts: vec![],
                total_gaps: 0,
                next_review_queue: vec![],
                queue_truncated: false,
                limitations: vec!["Review coverage schema is unavailable in this catalog.".into()],
            });
        }
        let (historical, history_truncated) = load_project_records(&connection, &plan.project_id)?;
        let mut cuts = Vec::with_capacity(plan.operations.len());
        let mut queue = Vec::new();
        let mut total_gaps = 0;
        let planned_bounds = planned_preview_bounds(&plan);
        for (position, cut) in plan.operations.iter().enumerate() {
            let current_target = normalized_target(&plan, &cut.operation_id)?;
            let asset = self.get_asset(&cut.asset_id)?;
            let matching_artifact = current_artifacts.iter().find_map(|artifact| {
                let mapping = artifact
                    .validation
                    .as_ref()?
                    .cuts
                    .iter()
                    .find(|m| m.operation_id == cut.operation_id)?;
                let source_key = source_dependency_key(
                    &plan,
                    cut,
                    artifact,
                    &asset.fingerprint,
                    asset.rotation_degrees,
                )
                .ok()?;
                Some((artifact, mapping, source_key))
            });
            let (start, end, mapping_label) =
                if let Some((artifact, mapping, _)) = &matching_artifact {
                    let validation = artifact
                        .validation
                        .as_ref()
                        .context("validated preview has no validation")?;
                    let seconds = |pts: i64| {
                        (pts - validation.movie_origin_pts) as f64
                            * validation.movie_time_base_numerator as f64
                            / validation.movie_time_base_denominator.max(1) as f64
                    };
                    let origin = artifact
                        .excerpt
                        .as_ref()
                        .map(|scope| {
                            super::preview_excerpt::resolve(&plan, scope)
                                .map(|window| window.in_seconds())
                        })
                        .transpose()?
                        .unwrap_or(0.0);
                    let label = if artifact.excerpt.is_some() {
                        "verifiedExcerptRemappedToFullClock"
                    } else {
                        "verifiedFullPreviewClock"
                    };
                    (
                        origin + seconds(mapping.movie_in_pts),
                        origin + seconds(mapping.movie_out_pts),
                        label,
                    )
                } else {
                    let (start, end) = planned_bounds[position];
                    (start, end, "plannedPreviewClock30000Over1001")
                };
            let mut p = Vec::new();
            let mut l = Vec::new();
            let mut ps = Vec::new();
            let mut ls = Vec::new();
            if let Some((_, _, current_source_key)) = &matching_artifact {
                for item in &historical {
                    if item.record.operation_id != cut.operation_id
                        || !item.record.explicit_human_playback
                        || item.record.actor != ReviewDecisionActor::Human
                        || item.record.method != ReviewCoverageMethod::HumanPlayback
                        || &item.source_key != current_source_key
                        || !super::feedback::same_target(&item.target, &current_target)
                    {
                        continue;
                    }
                    if verify {
                        let Ok(old_status) =
                            self.preview_artifact_status(&item.record.artifact_id, true)
                        else {
                            continue;
                        };
                        if old_status.observed_state != PreviewArtifactState::Complete
                            || old_status.current_for_request != Some(true)
                        {
                            continue;
                        }
                    }
                    for (interval, relative_start, relative_end) in &item.relative_intervals {
                        let a = start + relative_start;
                        let b = start + relative_end;
                        if b <= a {
                            continue;
                        }
                        match (interval.modality, interval.coverage) {
                            (ReviewCoverageModality::Picture, ReviewCoverageKind::Continuous) => {
                                p.push((a, b))
                            }
                            (ReviewCoverageModality::Listening, ReviewCoverageKind::Continuous) => {
                                l.push((a, b))
                            }
                            (ReviewCoverageModality::Picture, ReviewCoverageKind::Sampled) => {
                                ps.push((a, b))
                            }
                            (ReviewCoverageModality::Listening, ReviewCoverageKind::Sampled) => {
                                ls.push((a, b))
                            }
                        }
                    }
                }
            }
            let picture = modality_report(start, end, &p, &ps);
            let listening = modality_report(start, end, &l, &ls);
            total_gaps += picture.gap_count + listening.gap_count;
            for (modality, report) in [
                (ReviewCoverageModality::Picture, &picture),
                (ReviewCoverageModality::Listening, &listening),
            ] {
                for gap in &report.gaps {
                    if queue.len() < MAX_QUEUE {
                        queue.push(ReviewCoverageQueueItem {
                            operation_id: cut.operation_id.clone(),
                            modality,
                            movie_start_seconds: gap.movie_start_seconds,
                            movie_end_seconds: gap.movie_end_seconds,
                            duration_seconds: gap.movie_end_seconds - gap.movie_start_seconds,
                        });
                    }
                }
            }
            cuts.push(ReviewCoverageCutReport {
                operation_id: cut.operation_id.clone(),
                position: position as u32,
                asset_id: cut.asset_id.clone(),
                movie_start_seconds: start,
                movie_end_seconds: end,
                mapping: mapping_label.into(),
                picture,
                listening,
            });
        }
        Ok(ReviewCoverageReport { schema_ready:true,project_id:plan.project_id,revision_id:revision_id.into(),verified:verify,history_truncated,cuts,total_gaps,next_review_queue:queue.clone(),queue_truncated:total_gaps>queue.len(),limitations:vec!["Only explicit human playback attestations count toward picture or listening coverage.".into(),"Sampled intervals are reported separately and never close a continuous-review gap.".into(),"Cross-revision carry requires identical stable cut, neighbors, source ranges, audio context, source fingerprint, orientation, and selected representation fingerprint.".into(),"Coverage is a playback locator on a rendered preview clock, not proof of comprehension, source-frame identity, speech completeness, or editorial acceptance.".into()] })
    }

    pub fn review_coverage_records(
        &self,
        revision_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<ReviewCoveragePage> {
        ensure!(
            limit <= MAX_PAGE && offset <= 1_000_000,
            "invalid input: coverage page exceeds bounds"
        );
        let connection = self.connection()?;
        let tx = connection.unchecked_transaction()?;
        if !crate::table_exists(&tx, "edit_review_coverage")? {
            return Ok(ReviewCoveragePage {
                schema_ready: false,
                revision_id: revision_id.into(),
                total: 0,
                next_offset: None,
                items: vec![],
            });
        }
        let exists: Option<(String,)> = tx
            .query_row(
                "SELECT project_id FROM edit_plan_revisions WHERE revision_id=?1",
                [revision_id],
                |r| Ok((r.get(0)?,)),
            )
            .optional()?;
        let Some((_project_id,)) = exists else {
            return Ok(ReviewCoveragePage {
                schema_ready: false,
                revision_id: revision_id.into(),
                total: 0,
                next_offset: None,
                items: vec![],
            });
        };
        let total: usize = tx
            .query_row(
                "SELECT COUNT(*) FROM edit_review_coverage WHERE revision_id=?1",
                [revision_id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0)
            .max(0) as usize;
        let mut stmt=tx.prepare("SELECT record_id FROM edit_review_coverage WHERE revision_id=?1 ORDER BY recorded_at,record_id LIMIT ?2 OFFSET ?3")?;
        let ids = stmt
            .query_map(params![revision_id, limit as i64, offset as i64], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut items = Vec::with_capacity(ids.len());
        for key in ids {
            items.push(load_record(&tx, &key)?);
        }
        let next = offset + items.len();
        Ok(ReviewCoveragePage {
            schema_ready: true,
            revision_id: revision_id.into(),
            total,
            next_offset: (next < total).then_some(next),
            items,
        })
    }
}

fn merge_ranges(ranges: &[(f64, f64)], start: f64, end: f64) -> Vec<(f64, f64)> {
    let mut sorted = ranges.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (a, b) in sorted {
        let a = a.max(start);
        let b = b.min(end);
        if b <= a {
            continue;
        }
        if let Some(last) = merged.last_mut() {
            if a <= last.1 + 1e-7 {
                last.1 = last.1.max(b);
                continue;
            }
        }
        merged.push((a, b));
    }
    merged
}
fn modality_report(
    start: f64,
    end: f64,
    continuous: &[(f64, f64)],
    sampled: &[(f64, f64)],
) -> ReviewCoverageModalityReport {
    let merged = merge_ranges(continuous, start, end);
    let mut gaps = Vec::new();
    let mut cursor = start;
    for (a, b) in &merged {
        if *a > cursor + 1e-7 {
            gaps.push(ReviewCoverageGap {
                movie_start_seconds: cursor,
                movie_end_seconds: *a,
            });
        }
        cursor = cursor.max(*b);
    }
    if cursor < end - 1e-7 {
        gaps.push(ReviewCoverageGap {
            movie_start_seconds: cursor,
            movie_end_seconds: end,
        });
    }
    let sampled = merge_ranges(sampled, start, end)
        .iter()
        .map(|(a, b)| b - a)
        .sum();
    let gap_count = gaps.len();
    ReviewCoverageModalityReport {
        continuous_covered_seconds: merged.iter().map(|(a, b)| b - a).sum(),
        sampled_seconds: sampled,
        gap_count,
        gaps_truncated: gap_count > 64,
        gaps: gaps.into_iter().take(64).collect(),
    }
}

#[cfg(test)]
#[path = "review_coverage_tests.rs"]
mod tests;
