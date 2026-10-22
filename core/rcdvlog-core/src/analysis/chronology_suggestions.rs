//! Read-only cross-clip chronology evidence requests and model suggestions.
//!
//! This module deliberately has no chronology mutation dependency. Suggestions
//! remain review material and cannot change human-authored chronology facts.
use crate::analysis::provider::{ChainOutcome, ProviderChain, VlmProvider, VlmRequest};
use anyhow::{Context, Result};
use rusqlite::OptionalExtension;
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
    pub model: String,
    pub attempts: u32,
    #[serde(default)]
    pub attempted_models: Vec<String>,
    #[serde(default = "usage_is_complete")]
    pub usage_complete: bool,
    pub cost_usd: f64,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub latency_ms: u64,
}

fn usage_is_complete() -> bool {
    true
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

/// Makes one bounded provider call and returns reviewable suggestions only.
/// Provider injection keeps this path testable without network or paid calls.
pub(crate) fn request_suggestion(
    provider: &dyn VlmProvider,
    chain: &ProviderChain,
    request: &ChronologyEvidenceRequest,
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
    let chained: ChainOutcome = chain.analyze(
        provider,
        &VlmRequest {
            prompt,
            frames: images,
            schema,
        },
    )?;
    let value: Value =
        serde_json::from_str(&chained.response.body).context("parse chronology suggestion JSON")?;
    let suggestion: ChronologySuggestion =
        serde_json::from_value(value).context("decode chronology suggestion")?;
    let review = review_suggestion(request, &suggestion);
    Ok(SuggestionResult {
        suggestion: suggestion.clone(),
        review,
        provenance: SuggestionProvenance {
            model: chained.response.model,
            attempts: chained.attempts,
            attempted_models: chained.attempted_models,
            usage_complete: chained.usage_complete,
            cost_usd: chained.response.cost_usd,
            prompt_tokens: chained.response.prompt_tokens,
            completion_tokens: chained.response.completion_tokens,
            latency_ms: chained.response.latency_ms,
        },
        cached: false,
    })
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
        Ok(result)
    })
    .transpose()
}

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
}
