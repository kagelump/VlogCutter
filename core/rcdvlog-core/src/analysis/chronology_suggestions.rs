//! Read-only cross-clip chronology evidence requests and model suggestions.
//!
//! This module deliberately has no chronology mutation dependency. Suggestions
//! remain review material and cannot change human-authored chronology facts.
use crate::analysis::accounting::stable_operation_digest;
use crate::analysis::accounting::store::associate_cache_result;
use crate::analysis::framepack::encode_frames;
use crate::analysis::openrouter::frame_data_uri;
use crate::analysis::provider::{
    ChainOutcome, DEFAULT_MODEL, ProviderChain, ProviderFailure, VlmProvider, VlmRequest,
    VlmResponse,
};
use crate::{AnalysisOperationAccountingSummary, Catalog, ChronologyFacts};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MAX_CLIPS: usize = 8;
pub const MAX_FRAMES_PER_CLIP: usize = 3;
pub const MAX_TOTAL_FRAMES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceFrame {
    pub id: String,
    pub asset_id: String,
    pub timestamp_ms: u32,
    /// Encoded from the selected proxy frame; originals are never accepted here.
    pub proxy_data_uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChronologyEvidenceRequest {
    pub facts_revision: String,
    pub frames: Vec<EvidenceFrame>,
    pub required_frame_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairMatch {
    pub earlier_asset_id: String,
    pub later_asset_id: String,
    pub supporting_frame_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChronologySuggestion {
    pub matches: Vec<PairMatch>,
    pub frame_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionReview {
    pub missing_frame_ids: Vec<String>,
    pub duplicate_frame_ids: Vec<String>,
    pub unknown_frame_ids: Vec<String>,
    pub semantic_inconsistencies: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionProvenance {
    /// Actual serving model when the canonical receipt provides one. This is
    /// empty (not inferred from the requested route) when receipt evidence is
    /// missing.
    pub model: String,
    pub attempts: u32,
    #[serde(default)]
    pub attempted_models: Vec<String>,
    #[serde(default = "usage_is_complete")]
    pub usage_complete: bool,
    /// Confirmed current-run ledger cost. Never an admission reservation or an
    /// unresolved usage reserve.
    pub cost_usd: f64,
    #[serde(default)]
    pub uncertain_usage_reserve_usd: f64,
    #[serde(default)]
    pub active_reserved_cost_usd: f64,
    #[serde(default)]
    pub committed_or_reserved_usd: f64,
    #[serde(default)]
    pub actual_provider: Option<String>,
    #[serde(default)]
    pub actual_model: Option<String>,
    #[serde(default)]
    pub provider_generation_id: Option<String>,
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub request_keys: Vec<String>,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub latency_ms: u64,
}

fn usage_is_complete() -> bool {
    false
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionResult {
    pub suggestion: ChronologySuggestion,
    pub review: SuggestionReview,
    pub provenance: SuggestionProvenance,
    #[serde(default)]
    pub cached: bool,
}

/// Local request preparation owned by the chronology module. Gathering proxy
/// evidence and freezing the exact request/source identity happen before the
/// accounting authority or any paid dispatch exists.
pub(crate) struct PreparedChronologyRequest {
    pub request: ChronologyEvidenceRequest,
    pub facts: ChronologyFacts,
    pub source_identities: Vec<Value>,
    pub source_fingerprint: String,
    pub request_identity: String,
    pub cache_request_key: String,
}

pub(crate) fn prepare_request(
    catalog: &Catalog,
    project_id: &str,
    asset_ids: &[String],
    chain: &ProviderChain,
    is_cancelled: &mut dyn FnMut() -> bool,
) -> Result<PreparedChronologyRequest> {
    ensure!(
        (2..=MAX_CLIPS).contains(&asset_ids.len()),
        "select 2..=8 clips"
    );
    ensure!(
        asset_ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            == asset_ids.len(),
        "each clip may be selected only once"
    );
    let facts = catalog.chronology_facts(project_id)?;
    let facts_revision = blake3::hash(&serde_json::to_vec(&facts)?)
        .to_hex()
        .to_string();
    let mut frames = Vec::new();
    let mut source_identities = Vec::new();
    // Frame extraction shares the existing local-heavy slot. The lease ends
    // with local preparation, before any ledger reservation or cloud request.
    let _heavy = catalog.acquire_local_heavy_cancellable(&mut *is_cancelled)?;
    for asset_id in asset_ids {
        if is_cancelled() {
            return Err(crate::ProviderFailure::cancelled().into());
        }
        let asset = catalog.get_asset(asset_id)?;
        ensure!(
            asset.project_id == project_id && !asset.removed,
            "clip is not an active member of this project"
        );
        let proxy = Catalog::require_proxy(&asset)?;
        let proxy_source = catalog.analysis_proxy_source(&asset)?;
        let manifest = catalog.analysis_frame_pack(&asset)?;
        let parent = catalog
            .database_path()
            .parent()
            .ok_or_else(|| anyhow::anyhow!("catalog has no parent directory"))?;
        let directory = parent
            .join("cache")
            .join("framepacks")
            .join(project_id)
            .join(asset_id)
            .join(&manifest.source_fingerprint);
        let paths = encode_frames(&proxy, &manifest, &directory)?;
        ensure!(
            !paths.is_empty() && paths.len() == manifest.timestamps_ms.len(),
            "proxy frame pack is empty or incomplete for {asset_id}"
        );
        source_identities.push(json!({
            "assetId": asset.id,
            "assetFingerprint": asset.fingerprint,
            "representationFingerprint": proxy_source.fingerprint,
            "sampledRepresentationFingerprint": proxy_source.sampled_fingerprint,
        }));
        // Two deterministic samples per selected clip keep cross-clip requests small.
        let indexes = if paths.len() == 1 {
            vec![0]
        } else {
            vec![0, paths.len() - 1]
        };
        for index in indexes {
            let timestamp_ms = manifest.timestamps_ms[index];
            let bytes = std::fs::read(&paths[index])
                .with_context(|| format!("read proxy frame {}", paths[index].display()))?;
            frames.push(EvidenceFrame {
                id: format!("{asset_id}@{timestamp_ms}"),
                asset_id: asset_id.clone(),
                timestamp_ms,
                proxy_data_uri: frame_data_uri(&bytes),
            });
        }
    }
    let required_frame_ids = frames.iter().map(|frame| frame.id.clone()).collect();
    let request = ChronologyEvidenceRequest {
        facts_revision,
        frames,
        required_frame_ids,
    };
    request.validate().map_err(anyhow::Error::msg)?;
    // Result-cache identity only. New paid work is admitted by the canonical
    // ledger; this key never acts as a second durable permit.
    let cache_request_key = blake3::hash(&serde_json::to_vec(&(
        &request,
        chain.current_default(),
        chain.fallback_model(),
    ))?)
    .to_hex()
    .to_string();
    let source_fingerprint = stable_operation_digest(&json!({
        "projectId": project_id,
        "factsRevision": request.facts_revision,
        "sources": &source_identities,
    }))?;
    let request_identity = stable_operation_digest(&json!({
        "request": &request,
        "primaryModel": chain.primary_model(),
        "allowedModels": chain.allowed_models(),
        "allowFallback": chain.allows_fallback(),
    }))?;
    Ok(PreparedChronologyRequest {
        request,
        facts,
        source_identities,
        source_fingerprint,
        request_identity,
        cache_request_key,
    })
}

/// Thin DTO mapping from the one canonical operation aggregate. This does not
/// compute a second balance: cost, reservation and uncertainty remain separate.
pub(crate) fn with_accounting_projection(
    mut result: SuggestionResult,
    summary: &AnalysisOperationAccountingSummary,
    run_id: &str,
) -> SuggestionResult {
    result.provenance.model = summary.actual_model.clone().unwrap_or_default();
    result.provenance.attempts = summary.request_count as u32;
    result.provenance.cost_usd = summary.confirmed_cost_usd;
    result.provenance.uncertain_usage_reserve_usd = summary.uncertain_usage_reserve_usd;
    result.provenance.active_reserved_cost_usd = summary.active_reserved_cost_usd;
    result.provenance.committed_or_reserved_usd = summary.committed_or_reserved_usd;
    result.provenance.usage_complete = summary.usage_complete;
    result.provenance.actual_provider = summary.actual_provider.clone();
    result.provenance.actual_model = summary.actual_model.clone();
    result.provenance.provider_generation_id = summary.provider_generation_id.clone();
    result.provenance.run_id = run_id.to_string();
    result.provenance.request_keys = summary.request_keys.clone();
    result
}

/// Recheck the exact human facts and selected proxy identities without writing
/// or retaining a transaction across transport I/O.
pub(crate) fn validate_source_unchanged(
    catalog: &Catalog,
    project_id: &str,
    request: &ChronologyEvidenceRequest,
    sources: &[Value],
) -> Result<()> {
    let facts = catalog.chronology_facts(project_id)?;
    let current_revision = blake3::hash(&serde_json::to_vec(&facts)?)
        .to_hex()
        .to_string();
    ensure!(
        current_revision == request.facts_revision,
        "chronology facts changed before paid dispatch"
    );
    for source in sources {
        let asset_id = source
            .get("assetId")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("source identity is missing its asset id"))?;
        let expected_asset = source
            .get("assetFingerprint")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("source identity is missing its asset fingerprint"))?;
        let expected_proxy = source
            .get("representationFingerprint")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                anyhow::anyhow!("source identity is missing its representation fingerprint")
            })?;
        let expected_sampled = source
            .get("sampledRepresentationFingerprint")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("source identity is missing its sampled fingerprint"))?;
        let asset = catalog.get_asset(asset_id)?;
        ensure!(
            asset.project_id == project_id && !asset.removed && asset.fingerprint == expected_asset,
            "selected clip source changed before paid dispatch"
        );
        let current = catalog.analysis_proxy_source(&asset)?;
        ensure!(
            current.fingerprint == expected_proxy
                && current.sampled_fingerprint == expected_sampled,
            "selected clip proxy representation changed before paid dispatch"
        );
    }
    Ok(())
}

/// Makes one bounded provider call and returns reviewable suggestions only.
/// Provider injection keeps this path testable without network or paid calls.
#[cfg(test)]
pub(crate) fn request_suggestion(
    provider: &dyn VlmProvider,
    chain: &ProviderChain,
    request: &ChronologyEvidenceRequest,
) -> Result<SuggestionResult> {
    request_suggestion_with_cancel(provider, chain, request, &mut || false)
}

fn suggestion_from_response(
    response: &VlmResponse,
    request: &ChronologyEvidenceRequest,
    attempts: u32,
    attempted_models: Vec<String>,
    usage_complete: bool,
) -> Result<SuggestionResult> {
    let value: Value =
        serde_json::from_str(&response.body).context("parse chronology suggestion JSON")?;
    let suggestion: ChronologySuggestion =
        serde_json::from_value(value).context("decode chronology suggestion")?;
    let review = review_suggestion(request, &suggestion);
    Ok(SuggestionResult {
        suggestion: suggestion.clone(),
        review,
        provenance: SuggestionProvenance {
            model: response.model.clone(),
            attempts,
            attempted_models,
            usage_complete,
            cost_usd: response.cost_usd,
            uncertain_usage_reserve_usd: 0.0,
            active_reserved_cost_usd: 0.0,
            committed_or_reserved_usd: response.cost_usd,
            actual_provider: None,
            actual_model: None,
            provider_generation_id: None,
            run_id: String::new(),
            request_keys: Vec::new(),
            prompt_tokens: response.prompt_tokens,
            completion_tokens: response.completion_tokens,
            latency_ms: response.latency_ms,
        },
        cached: false,
    })
}

/// Cancellation-aware chronology request. Each actual primary/fallback call is
/// admitted by the caller's accounted provider; local cancellation never
/// triggers a fallback attempt.
pub(crate) fn request_suggestion_with_cancel(
    provider: &dyn VlmProvider,
    chain: &ProviderChain,
    request: &ChronologyEvidenceRequest,
    is_cancelled: &mut dyn FnMut() -> bool,
) -> Result<SuggestionResult> {
    request.validate().map_err(anyhow::Error::msg)?;
    let schema = json!({
      "type":"object", "required":["frameIds","matches"], "additionalProperties":false,
      "properties": {
        "frameIds":{"type":"array","maxItems":16,"items":{"type":"string"}},
        "matches":{"type":"array","maxItems":28,"items":{"type":"object","required":["earlierAssetId","laterAssetId","supportingFrameIds"],"additionalProperties":false,"properties":{
          "earlierAssetId":{"type":"string"},"laterAssetId":{"type":"string"},"supportingFrameIds":{"type":"array","maxItems":16,"items":{"type":"string"}}
        }}}
      }
    });
    let images = request
        .frames
        .iter()
        .map(|frame| frame.proxy_data_uri.clone())
        .collect::<Vec<_>>();
    let prompt = format!(
        "Review relative visual chronology among these clips. Do not infer exact dates or create corrections. Cite frame IDs exactly as provided. Required once each in frameIds: {}. Frame manifest: {}",
        request.required_frame_ids.join(","),
        serde_json::to_string(
            &request
                .frames
                .iter()
                .map(|f| (&f.id, &f.asset_id, f.timestamp_ms))
                .collect::<Vec<_>>()
        )?
    );
    let request_payload = VlmRequest {
        prompt,
        frames: images,
        schema,
    };
    let first: ChainOutcome =
        chain.analyze_with_cancel(provider, &request_payload, is_cancelled)?;
    let first_attempts = first.attempts;
    let mut attempted_models = first.attempted_models.clone();
    match suggestion_from_response(
        &first.response,
        request,
        first.attempts,
        first.attempted_models.clone(),
        first.usage_complete,
    ) {
        Ok(result) => Ok(result),
        Err(default_error) if first.response.model == DEFAULT_MODEL => {
            chain.record_default_failure();
            let fallback_model = chain.fallback_model().ok_or_else(|| {
                anyhow::anyhow!(
                    "default chronology suggestion was invalid ({default_error:#}); no authorized fallback model remains"
                )
            })?;
            if is_cancelled() {
                return Err(ProviderFailure::cancelled().into());
            }
            let fallback =
                provider.analyze_with_cancel(fallback_model, &request_payload, is_cancelled)?;
            attempted_models.push(fallback_model.to_string());
            suggestion_from_response(
                &fallback,
                request,
                first_attempts + 1,
                attempted_models,
                first.usage_complete && fallback.receipt.usage_complete,
            )
            .with_context(|| {
                format!(
                    "default chronology suggestion was invalid ({default_error:#}); fallback was invalid"
                )
            })
        }
        Err(error) => Err(error).context("validate chronology suggestion"),
    }
}

pub(crate) fn load_cached(
    connection: &rusqlite::Connection,
    request_key: &str,
) -> Result<Option<SuggestionResult>> {
    let json: Option<String> = connection
        .query_row(
            "SELECT result_json FROM analysis_chronology_suggestions WHERE request_key=?1 AND status='completed'",
            [request_key],
            |row| row.get(0),
        )
        .optional()?;
    json.map(|value| {
        let mut result: SuggestionResult = serde_json::from_str(&value)?;
        result.cached = true;
        if !result.provenance.usage_complete && result.provenance.run_id.is_empty() {
            // Legacy cache amounts are returned evidence, not confirmed charges.
            let floor = result.provenance.cost_usd.max(0.0);
            result.provenance.cost_usd = 0.0;
            result.provenance.uncertain_usage_reserve_usd =
                result.provenance.uncertain_usage_reserve_usd.max(floor);
            result.provenance.committed_or_reserved_usd =
                result.provenance.uncertain_usage_reserve_usd;
        }
        Ok(result)
    })
    .transpose()
}

#[cfg(test)]
pub(crate) fn store_result(
    connection: &rusqlite::Connection,
    request_key: &str,
    result: &SuggestionResult,
) -> Result<()> {
    connection.execute(
        "UPDATE analysis_chronology_suggestions SET status='completed',result_json=?2,model=?3,attempts=?4,cost_usd=?5,prompt_tokens=?6,completion_tokens=?7,latency_ms=?8,updated_at=?9 WHERE request_key=?1 AND status='running'",
        rusqlite::params![request_key, serde_json::to_string(result)?, result.provenance.model, result.provenance.attempts, result.provenance.cost_usd, result.provenance.prompt_tokens, result.provenance.completion_tokens, result.provenance.latency_ms.min(i64::MAX as u64) as i64, chrono::Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

/// Persist a completed result-cache identity and every request association in
/// one IMMEDIATE transaction. A cache row is never visible without its ledger
/// association, and an association is never written before the cache content.
pub(crate) fn store_completed_with_associations(
    connection: &mut Connection,
    request_key: &str,
    facts_revision: &str,
    result: &SuggestionResult,
    request_keys: &[String],
) -> Result<()> {
    ensure!(
        !request_key.trim().is_empty() && !request_keys.is_empty(),
        "completed chronology result requires cache and request identities"
    );
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let inserted = transaction.execute(
        "INSERT INTO analysis_chronology_suggestions(
            request_key,project_id,facts_revision,status,result_json,model,attempts,
            cost_usd,prompt_tokens,completion_tokens,latency_ms,created_at,updated_at)
         VALUES(?1,(
             SELECT project_id FROM analysis_request_reservations WHERE request_key=?2
         ),?3,'completed',?4,?5,?6,?7,?8,?9,?10,?11,?11)",
        params![
            request_key,
            request_keys[0],
            facts_revision,
            serde_json::to_string(result)?,
            result.provenance.model,
            result.provenance.attempts,
            result.provenance.cost_usd,
            result.provenance.prompt_tokens,
            result.provenance.completion_tokens,
            result.provenance.latency_ms.min(i64::MAX as u64) as i64,
            crate::now(),
        ],
    )?;
    ensure!(
        inserted == 1,
        "completed chronology cache identity already exists; refusing to overwrite historical result evidence"
    );
    for ledger_request_key in request_keys {
        associate_cache_result(&transaction, ledger_request_key, request_key)?;
    }
    transaction.commit()?;
    Ok(())
}

/// Release the result-cache claim for a request only when the caller has
/// reliable evidence that no paid transmission occurred. The caller checks the
/// canonical ledger first; this function never touches a row that already has a
/// receipt or dispatch marker.
#[cfg(test)]
pub(crate) fn release_claimed(
    connection: &rusqlite::Connection,
    request_key: &str,
) -> Result<bool> {
    let deleted = connection.execute(
        "DELETE FROM analysis_chronology_suggestions
         WHERE request_key=?1 AND status='running'",
        [request_key],
    )?;
    Ok(deleted == 1)
}

#[cfg(test)]
pub(crate) fn reserve_request(
    connection: &rusqlite::Connection,
    request_key: &str,
    project_id: &str,
    facts_revision: &str,
) -> Result<bool> {
    let now = chrono::Utc::now().to_rfc3339();
    let inserted = connection.execute(
        "INSERT OR IGNORE INTO analysis_chronology_suggestions(request_key,project_id,facts_revision,status,created_at,updated_at) VALUES (?1,?2,?3,'running',?4,?4)",
        rusqlite::params![request_key, project_id, facts_revision, now],
    )?;
    Ok(inserted == 1)
}

impl ChronologyEvidenceRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.frames.is_empty() || self.frames.len() > MAX_TOTAL_FRAMES {
            return Err(format!(
                "evidence request must contain 1..={MAX_TOTAL_FRAMES} frames"
            ));
        }
        let assets: std::collections::HashSet<_> =
            self.frames.iter().map(|f| f.asset_id.as_str()).collect();
        if assets.len() < 2 {
            return Err("cross-clip evidence requires at least two distinct clips".into());
        }
        if assets.len() > MAX_CLIPS {
            return Err(format!("evidence request exceeds {MAX_CLIPS} clips"));
        }
        for asset in &assets {
            if self
                .frames
                .iter()
                .filter(|frame| frame.asset_id == *asset)
                .count()
                > MAX_FRAMES_PER_CLIP
            {
                return Err(format!(
                    "evidence request exceeds {MAX_FRAMES_PER_CLIP} frames for {asset}"
                ));
            }
        }
        if self
            .frames
            .iter()
            .any(|frame| !frame.proxy_data_uri.starts_with("data:image/jpeg;base64,"))
        {
            return Err("evidence frames must be selected proxy JPEGs".into());
        }
        let known: std::collections::HashSet<_> =
            self.frames.iter().map(|f| f.id.as_str()).collect();
        if known.len() != self.frames.len() {
            return Err("evidence frame IDs must be unique".into());
        }
        if self
            .required_frame_ids
            .iter()
            .any(|id| !known.contains(id.as_str()))
        {
            return Err("required frame IDs must name supplied frames".into());
        }
        Ok(())
    }
}

/// Reports coverage defects and checkable contradictions without repairing output.
pub fn review_suggestion(
    request: &ChronologyEvidenceRequest,
    suggestion: &ChronologySuggestion,
) -> SuggestionReview {
    let required: std::collections::HashSet<_> = request
        .required_frame_ids
        .iter()
        .map(String::as_str)
        .collect();
    let known: std::collections::HashSet<_> = request
        .frames
        .iter()
        .map(|frame| frame.id.as_str())
        .collect();
    let mut counts = std::collections::HashMap::<&str, usize>::new();
    for id in &suggestion.frame_ids {
        *counts.entry(id).or_default() += 1;
    }
    let missing_frame_ids = required
        .iter()
        .filter(|id| counts.get(**id).copied().unwrap_or(0) == 0)
        .map(|id| (*id).to_string())
        .collect();
    let duplicate_frame_ids = counts
        .iter()
        .filter(|(_, count)| **count > 1)
        .map(|(id, _)| (*id).to_string())
        .collect();
    let unknown_frame_ids = counts
        .keys()
        .filter(|id| !known.contains(**id))
        .map(|id| (*id).to_string())
        .collect();
    let mut inconsistencies = Vec::new();
    for pair in &suggestion.matches {
        if pair.earlier_asset_id == pair.later_asset_id {
            inconsistencies.push(format!(
                "match orders asset {} before itself",
                pair.earlier_asset_id
            ));
        }
        if pair.supporting_frame_ids.is_empty() {
            inconsistencies.push(format!(
                "match {} -> {} has no supporting frames",
                pair.earlier_asset_id, pair.later_asset_id
            ));
        } else if pair.supporting_frame_ids.iter().any(|id| {
            request
                .frames
                .iter()
                .find(|frame| frame.id == *id)
                .is_none_or(|frame| {
                    frame.asset_id != pair.earlier_asset_id && frame.asset_id != pair.later_asset_id
                })
        }) {
            inconsistencies.push(format!(
                "match {} -> {} cites a frame not belonging to either matched clip",
                pair.earlier_asset_id, pair.later_asset_id
            ));
        }
        if pair
            .supporting_frame_ids
            .iter()
            .any(|id| !known.contains(id.as_str()))
        {
            inconsistencies.push(format!(
                "match {} -> {} cites an unknown frame",
                pair.earlier_asset_id, pair.later_asset_id
            ));
        } else if pair
            .supporting_frame_ids
            .iter()
            .any(|id| !suggestion.frame_ids.contains(id))
        {
            inconsistencies.push(format!(
                "match {} -> {} cites a frame absent from frameIds",
                pair.earlier_asset_id, pair.later_asset_id
            ));
        }
        let supplied_assets: std::collections::HashSet<_> = request
            .frames
            .iter()
            .map(|frame| frame.asset_id.as_str())
            .collect();
        if !supplied_assets.contains(pair.earlier_asset_id.as_str())
            || !supplied_assets.contains(pair.later_asset_id.as_str())
        {
            inconsistencies.push(format!(
                "match {} -> {} names an asset absent from the evidence request",
                pair.earlier_asset_id, pair.later_asset_id
            ));
        }
    }
    SuggestionReview {
        missing_frame_ids,
        duplicate_frame_ids,
        unknown_frame_ids,
        semantic_inconsistencies: inconsistencies,
    }
}

/// Adds comparisons against the deterministic chronology solution. A mismatch
/// is surfaced for a human to inspect; it never changes either side.
pub fn review_suggestion_against_facts(
    request: &ChronologyEvidenceRequest,
    suggestion: &ChronologySuggestion,
    facts: &crate::chronology::model::ChronologyFacts,
) -> SuggestionReview {
    let mut review = review_suggestion(request, suggestion);
    let solution = crate::chronology::solve(facts);
    for pair in &suggestion.matches {
        let earlier = solution
            .assets
            .iter()
            .find(|asset| asset.asset_id == pair.earlier_asset_id);
        let later = solution
            .assets
            .iter()
            .find(|asset| asset.asset_id == pair.later_asset_id);
        if let (Some(earlier), Some(later)) = (earlier, later) {
            let earlier_time = earlier
                .trip_time
                .as_deref()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok());
            let later_time = later
                .trip_time
                .as_deref()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok());
            if matches!((earlier_time, later_time), (Some(left), Some(right)) if left > right) {
                review.semantic_inconsistencies.push(format!(
                    "suggested order {} before {} conflicts with the current solved times; inspect evidence",
                    pair.earlier_asset_id, pair.later_asset_id
                ));
            }
        }
    }
    review
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn request() -> ChronologyEvidenceRequest {
        ChronologyEvidenceRequest {
            facts_revision: "revision".into(),
            frames: ["a", "b", "c"]
                .into_iter()
                .enumerate()
                .map(|(index, asset)| EvidenceFrame {
                    id: format!("frame-{asset}"),
                    asset_id: format!("asset-{asset}"),
                    timestamp_ms: index as u32 * 10,
                    proxy_data_uri: "data:image/jpeg;base64,AA==".into(),
                })
                .collect(),
            required_frame_ids: vec!["frame-a".into(), "frame-b".into(), "frame-c".into()],
        }
    }

    #[test]
    fn coverage_and_semantic_problems_are_reported_for_review() {
        let output = ChronologySuggestion {
            matches: vec![PairMatch {
                earlier_asset_id: "a".into(),
                later_asset_id: "a".into(),
                supporting_frame_ids: vec!["ghost".into()],
            }],
            frame_ids: vec!["frame-a".into(), "frame-a".into(), "unknown".into()],
        };
        let review = review_suggestion(&request(), &output);
        assert_eq!(review.duplicate_frame_ids, vec!["frame-a"]);
        assert_eq!(review.unknown_frame_ids, vec!["unknown"]);
        assert!(review.semantic_inconsistencies.len() >= 3);
    }

    #[test]
    fn matches_must_cite_frames_from_the_pair_and_nonempty_support() {
        let request = request();
        let unrelated = ChronologySuggestion {
            matches: vec![PairMatch {
                earlier_asset_id: "asset-a".into(),
                later_asset_id: "asset-b".into(),
                supporting_frame_ids: vec!["frame-c".into()],
            }],
            frame_ids: vec!["frame-a".into(), "frame-b".into(), "frame-c".into()],
        };
        let review = review_suggestion(&request, &unrelated);
        assert!(
            review
                .semantic_inconsistencies
                .iter()
                .any(|issue| issue.contains("not belonging to either matched clip"))
        );

        let empty = ChronologySuggestion {
            matches: vec![PairMatch {
                earlier_asset_id: "asset-a".into(),
                later_asset_id: "asset-b".into(),
                supporting_frame_ids: vec![],
            }],
            frame_ids: vec!["frame-a".into(), "frame-b".into(), "frame-c".into()],
        };
        let review = review_suggestion(&request, &empty);
        assert!(
            review
                .semantic_inconsistencies
                .iter()
                .any(|issue| issue.contains("no supporting frames"))
        );
    }

    #[test]
    fn request_rejects_unbounded_or_non_proxy_frames() {
        let mut request = request();
        request.frames[0].proxy_data_uri = "file:///original.mov".into();
        assert!(request.validate().is_err());
    }

    struct FakeProvider {
        calls: Mutex<Vec<String>>,
    }
    impl VlmProvider for FakeProvider {
        fn analyze(
            &self,
            model: &str,
            _request: &VlmRequest,
        ) -> Result<crate::analysis::provider::VlmResponse> {
            self.calls.lock().unwrap().push(model.into());
            Ok(crate::analysis::provider::VlmResponse {
                model: model.into(),
                body: r#"{"frameIds":["frame-a","frame-b","frame-c"],"matches":[]}"#.into(),
                prompt_tokens: 9,
                completion_tokens: 4,
                cost_usd: 0.001,
                latency_ms: 12,
                receipt: crate::analysis::provider::VlmReceipt {
                    actual_model: Some(model.into()),
                    provider: Some("fake".into()),
                    generation_id: Some("fake-generation".into()),
                    usage_complete: true,
                },
            })
        }
    }

    #[test]
    fn fake_provider_returns_coverage_and_request_provenance_without_network() {
        let provider = FakeProvider {
            calls: Mutex::new(Vec::new()),
        };
        let chain = ProviderChain::with_policy(
            crate::analysis::provider::DEFAULT_MODEL,
            &[crate::analysis::provider::DEFAULT_MODEL.to_string()],
            false,
        )
        .unwrap();
        let result = request_suggestion(&provider, &chain, &request()).unwrap();
        assert_eq!(result.provenance.attempts, 1);
        assert_eq!(result.provenance.cost_usd, 0.001);
        assert!(result.review.missing_frame_ids.is_empty());
        assert_eq!(provider.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn local_release_only_removes_a_running_cache_claim() {
        let directory = tempfile::tempdir().unwrap();
        let catalog =
            crate::Catalog::open(directory.path().join("suggestion-release.sqlite3")).unwrap();
        let project = catalog.create_project("release").unwrap();
        let connection = catalog.connection().unwrap();
        assert!(reserve_request(&connection, "key", &project.id, "revision").unwrap());
        assert!(!reserve_request(&connection, "key", &project.id, "revision").unwrap());
        assert!(release_claimed(&connection, "key").unwrap());
        assert!(reserve_request(&connection, "key", &project.id, "revision").unwrap());
        let provider = FakeProvider {
            calls: Mutex::new(Vec::new()),
        };
        let chain = ProviderChain::with_policy(
            crate::analysis::provider::DEFAULT_MODEL,
            &[crate::analysis::provider::DEFAULT_MODEL.to_string()],
            false,
        )
        .unwrap();
        let result = request_suggestion(&provider, &chain, &request()).unwrap();
        store_result(&connection, "key", &result).unwrap();
        assert!(!release_claimed(&connection, "key").unwrap());
    }

    #[test]
    fn completed_suggestion_is_reloaded_without_repeating_a_provider_call() {
        let provider = FakeProvider {
            calls: Mutex::new(Vec::new()),
        };
        let chain = ProviderChain::with_policy(
            crate::analysis::provider::DEFAULT_MODEL,
            &[crate::analysis::provider::DEFAULT_MODEL.to_string()],
            false,
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let catalog_path = directory.path().join("suggestion.sqlite3");
        let catalog = crate::Catalog::open(&catalog_path).unwrap();
        let project = catalog.create_project("suggestion cache test").unwrap();
        let connection = catalog.connection().unwrap();
        assert!(reserve_request(&connection, "key", &project.id, "revision").unwrap());
        let result = request_suggestion(&provider, &chain, &request()).unwrap();
        store_result(&connection, "key", &result).unwrap();
        drop(connection);
        drop(catalog);
        let reopened = crate::Catalog::open(&catalog_path).unwrap();
        let reopened_connection = reopened.connection().unwrap();
        let project_fk: String = reopened_connection
            .query_row(
                "SELECT \"table\" FROM pragma_foreign_key_list('analysis_chronology_suggestions') WHERE \"from\"='project_id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(project_fk, "projects");
        let cached = load_cached(&reopened_connection, "key").unwrap().unwrap();
        assert!(cached.cached);
        assert_eq!(cached.provenance.cost_usd, 0.001);
        assert_eq!(provider.calls.lock().unwrap().len(), 1);
        assert!(!reserve_request(&reopened_connection, "key", &project.id, "revision").unwrap());
    }
    #[test]
    fn cached_legacy_missing_completeness_is_unknown_returned_cost_not_confirmed_charge() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = crate::Catalog::open(directory.path().join("legacy.sqlite3")).unwrap();
        let project = catalog.create_project("legacy").unwrap();
        let provider = FakeProvider {
            calls: Mutex::new(Vec::new()),
        };
        let result = request_suggestion(&provider, &ProviderChain::new(), &request()).unwrap();
        let mut raw = serde_json::to_value(result).unwrap();
        raw["provenance"]
            .as_object_mut()
            .unwrap()
            .remove("usageComplete");
        raw["provenance"]["costUsd"] = json!(0.02);
        let connection = catalog.connection().unwrap();
        connection.execute(
            "INSERT INTO analysis_chronology_suggestions(request_key,project_id,facts_revision,status,result_json,created_at,updated_at)
             VALUES('legacy',?1,'rev','completed',?2,'now','now')",
            params![project.id, raw.to_string()],
        ).unwrap();
        let loaded = load_cached(&connection, "legacy").unwrap().unwrap();
        assert!(!loaded.provenance.usage_complete);
        assert_eq!(loaded.provenance.cost_usd, 0.0);
        assert_eq!(loaded.provenance.uncertain_usage_reserve_usd, 0.02);
    }
}
