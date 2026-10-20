//! Read-only, revision-pinned chronology evidence pages.
use super::model::TimestampCandidate;
use super::model::{AssetTimeHypothesis, ChronologyFacts};
use super::solver::{AssetChronology, ChronologySolution, solve};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChronologyEvidenceAsset {
    pub asset: AssetTimeHypothesis,
    pub timestamp_candidates: Option<Vec<TimestampCandidate>>,
    pub candidate_comparisons: Vec<TimestampCandidateComparison>,
    pub solved: AssetChronology,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimestampCandidateComparison {
    pub left_origin: String,
    pub right_origin: String,
    pub relation: String,
    pub delta_seconds: Option<i64>,
    pub duration_delta_seconds: Option<i64>,
    pub duration_explains_difference: Option<bool>,
    pub interpretation: String,
}

fn compare_candidates(asset: &AssetTimeHypothesis) -> Vec<TimestampCandidateComparison> {
    let Some(candidates) = &asset.timestamp_candidates else {
        return Vec::new();
    };
    let mut comparisons = Vec::new();
    for left_index in 0..candidates.len() {
        for right_index in (left_index + 1)..candidates.len() {
            let left = &candidates[left_index];
            let right = &candidates[right_index];
            let left_time = left
                .parsed_value
                .as_deref()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok());
            let right_time = right
                .parsed_value
                .as_deref()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok());
            let delta = left_time
                .as_ref()
                .zip(right_time.as_ref())
                .map(|(a, b)| (a.timestamp() - b.timestamp()).abs());
            let left_semantics = left.semantics.as_deref();
            let right_semantics = right.semantics.as_deref();
            let is_start_end = matches!(
                (left_semantics, right_semantics),
                (Some("recording_start"), Some("recording_end"))
                    | (Some("recording_end"), Some("recording_start"))
            );
            let duration_delta = is_start_end
                .then(|| delta.map(|seconds| seconds - asset.duration_seconds.round() as i64))
                .flatten();
            comparisons.push(TimestampCandidateComparison {
                left_origin: left.origin.clone(), right_origin: right.origin.clone(),
                relation: if delta.is_some() { "instants_comparable" } else { "timezone_or_parse_prevents_comparison" }.into(),
                delta_seconds: delta,
                duration_delta_seconds: duration_delta,
                duration_explains_difference: duration_delta.map(|value| value.abs() <= 1),
                interpretation: if is_start_end {
                    "start/end interval is compared with clip duration; this does not imply a camera-clock correction".into()
                } else {
                    "offsets compare timestamp claims only; they do not identify a physical location or display timezone".into()
                },
            });
        }
    }
    comparisons
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChronologyEvidencePage {
    pub facts_revision: String,
    pub offset: usize,
    pub limit: usize,
    pub total_assets: usize,
    pub next_offset: Option<usize>,
    pub facts: ChronologyFacts,
    pub assets: Vec<ChronologyEvidenceAsset>,
    pub contradictions: Vec<super::solver::Contradiction>,
}

/// Builds one deterministic page from the same core facts and solver used by the UI.
pub fn evidence_page(
    facts: ChronologyFacts,
    expected_revision: Option<&str>,
    offset: usize,
    limit: usize,
) -> anyhow::Result<ChronologyEvidencePage> {
    anyhow::ensure!(
        (1..=500).contains(&limit),
        "chronology export limit must be between 1 and 500"
    );
    let serialized = serde_json::to_vec(&facts)?;
    let revision = blake3::hash(&serialized).to_hex().to_string();
    if let Some(expected) = expected_revision {
        anyhow::ensure!(
            expected == revision,
            "chronology.facts_changed: restart pagination with the current facts revision"
        );
    }
    let solution: ChronologySolution = solve(&facts);
    let total = facts.assets.len();
    let end = offset.saturating_add(limit).min(total);
    let rows = facts.assets[offset.min(total)..end]
        .iter()
        .filter_map(|asset| {
            solution
                .assets
                .iter()
                .find(|solved| solved.asset_id == asset.asset_id)
                .map(|solved| ChronologyEvidenceAsset {
                    asset: asset.clone(),
                    timestamp_candidates: asset.timestamp_candidates.clone(),
                    candidate_comparisons: compare_candidates(asset),
                    solved: solved.clone(),
                })
        })
        .collect();
    let mut context = facts.clone();
    // The full asset collection is represented by this bounded page.
    context.assets.clear();
    Ok(ChronologyEvidencePage {
        facts_revision: revision,
        offset,
        limit,
        total_assets: total,
        next_offset: (end < total).then_some(end),
        facts: context,
        assets: rows,
        contradictions: solution.contradictions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chronology::model::ChronologyFacts;

    fn asset(id: &str, filename: &str, raw: Option<&str>) -> AssetTimeHypothesis {
        AssetTimeHypothesis {
            asset_id: id.into(),
            filename: filename.into(),
            captured_at_raw: raw.map(str::to_owned),
            capture_sequence_id: Some("seq".into()),
            filename_counter: Some(1),
            duration_seconds: 10.0,
            timestamp_candidates: None,
        }
    }
    fn make_facts(assets: Vec<AssetTimeHypothesis>) -> ChronologyFacts {
        ChronologyFacts {
            project_id: "synthetic".into(),
            assets,
            sequences: vec![],
            epochs: vec![],
            exceptions: vec![],
            phases: vec![],
            constraints: vec![],
        }
    }

    #[test]
    fn page_pins_revision_and_keeps_timestamp_observations_distinct() {
        let mut a = asset("a", "20300102120405_clip.mp4", Some("2030-01-02T03:04:05Z"));
        a.timestamp_candidates = Some(vec![
            TimestampCandidate {
                origin: "fixture.camera_creation".into(),
                original_text: "2030-01-02T03:04:05Z".into(),
                parsed_value: Some("2030-01-02T03:04:05+00:00".into()),
                timezone_claim: Some("Z".into()),
                parse_status: "parsed".into(),
                semantics: Some("recording_start".into()),
            },
            TimestampCandidate {
                origin: "fixture.filename".into(),
                original_text: "20300102030415".into(),
                parsed_value: Some("2030-01-02T03:04:15".into()),
                timezone_claim: None,
                parse_status: "parsed_without_timezone".into(),
                semantics: None,
            },
            TimestampCandidate {
                origin: "fixture.container_end".into(),
                original_text: "2030-01-02T03:04:15Z".into(),
                parsed_value: Some("2030-01-02T03:04:15+00:00".into()),
                timezone_claim: Some("Z".into()),
                parse_status: "parsed".into(),
                semantics: Some("recording_end".into()),
            },
        ]);
        let facts = make_facts(vec![a, asset("b", "clip.mp4", None)]);
        let first = evidence_page(facts.clone(), None, 0, 1).unwrap();
        let candidates = first.assets[0].timestamp_candidates.as_ref().unwrap();
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0].timezone_claim.as_deref(), Some("Z"));
        assert_eq!(candidates[1].semantics, None);
        assert_eq!(candidates[2].semantics.as_deref(), Some("recording_end"));
        assert_eq!(
            first.assets[0].candidate_comparisons[1].duration_delta_seconds,
            Some(0)
        );
        assert_eq!(
            first.assets[0].candidate_comparisons[1].delta_seconds,
            Some(10)
        );
        assert_eq!(
            first.assets[0].candidate_comparisons[1].duration_explains_difference,
            Some(true)
        );
        assert!(
            first.assets[0].candidate_comparisons[1]
                .interpretation
                .contains("correction")
        );
        assert!(first.next_offset.is_some());
        let second = evidence_page(facts.clone(), Some(&first.facts_revision), 1, 1).unwrap();
        assert_eq!(second.assets[0].timestamp_candidates, None);
        let changed = make_facts(vec![asset("c", "clip.mp4", None)]);
        assert!(
            evidence_page(changed, Some(&first.facts_revision), 1, 1)
                .unwrap_err()
                .to_string()
                .contains("facts_changed")
        );
        assert!(first.facts.assets.is_empty());
    }
}
