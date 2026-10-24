//! Bounded temporal sampling and domain validation for within-clip evidence.
//! Sparse samples describe coverage, never a certificate for every future cut.

use super::model::*;
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const RECIPE_VERSION: &str = "moments.full-span.dense-refine.v1";
pub const PROMPT_VERSION: &str = "moments.discover-refine.v1";
pub const SCHEMA_VERSION: &str = "moments.evidence.v1";
pub const TIME_MAPPING: &str = "proxy-v3.source-ms.identity.v1";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MomentPayload {
    #[default]
    Frames,
    FramesAndTranscript,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentRunOptions {
    pub model: String,
    pub provider: String,
    pub payload: MomentPayload,
    pub budget_usd: f64,
    pub coarse_frames: usize,
    pub refinement_frames: usize,
    pub max_candidates: usize,
    pub profile: EditProfile,
    pub force: bool,
    pub retry_uncertain: bool,
    #[serde(default)]
    pub allowed_models: Vec<String>,
    #[serde(default = "default_allow_fallback")]
    pub allow_fallback: bool,
    #[serde(default)]
    pub policy_id: Option<String>,
}

fn default_allow_fallback() -> bool {
    true
}

impl MomentRunOptions {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.provider == "openrouter",
            "moments provider must be explicitly selected as openrouter"
        );
        ensure!(
            crate::analysis::provider::model_spec(&self.model).is_some(),
            "unsupported moment model: {}",
            self.model
        );
        if !self.allowed_models.is_empty() {
            for model in &self.allowed_models {
                ensure!(
                    crate::analysis::provider::model_spec(model).is_some(),
                    "unsupported allowed moment model: {model}"
                );
            }
            ensure!(
                self.allowed_models.iter().any(|model| model == &self.model),
                "primary moment model is not in the allowed model list"
            );
        }
        ensure!(
            self.budget_usd.is_finite() && self.budget_usd > 0.0,
            "moment budget must be finite and positive"
        );
        ensure!(
            (8..=256).contains(&self.coarse_frames),
            "coarse frame budget must be 8..=256"
        );
        ensure!(
            (8..=1024).contains(&self.refinement_frames),
            "refinement frame budget must be 8..=1024"
        );
        ensure!(
            (1..=16).contains(&self.max_candidates),
            "candidate budget must be 1..=16"
        );
        let p = self.profile;
        ensure!(
            p.select_min_ms > 0
                && p.select_min_ms <= p.select_max_ms
                && p.select_max_ms <= p.hold_min_ms
                && p.hold_min_ms <= p.hold_max_ms
                && p.hold_max_ms <= 120_000,
            "invalid select/hold profile"
        );
        Ok(())
    }
}

impl MomentSpan {
    pub fn duration_ms(self) -> u32 {
        self.end_ms.saturating_sub(self.start_ms)
    }
    pub fn contains(self, other: Self) -> bool {
        self.start_ms <= other.start_ms
            && other.end_ms <= self.end_ms
            && other.start_ms < other.end_ms
    }
    pub fn overlaps(self, other: Self) -> bool {
        self.start_ms < other.end_ms && other.start_ms < self.end_ms
    }
}

/// Include both ends, with a deterministic bounded budget over the WHOLE span.
/// No head truncation and no claim that intervening frames were inspected.
pub fn spread_samples(span: MomentSpan, budget: usize) -> Vec<u32> {
    let length = span.duration_ms();
    if length == 0 || budget == 0 {
        return vec![];
    }
    if budget == 1 || length == 1 {
        return vec![span.start_ms];
    }
    let n = budget.min(length as usize);
    (0..n)
        .map(|i| span.start_ms + ((i as u64 * u64::from(length - 1)) / (n - 1) as u64) as u32)
        .collect()
}

pub fn sample_gap(span: MomentSpan, times: &[u32]) -> u32 {
    let mut last = span.start_ms;
    let mut largest = 0;
    for &at in times {
        largest = largest.max(at.saturating_sub(last));
        last = at;
    }
    largest.max(span.end_ms.saturating_sub(last))
}

/// Each request sees at most eight coarse frames; adjacent windows share their
/// boundary sample so a payoff straddling a request boundary can be recognized.
pub fn coarse_windows(duration_ms: u32, frame_budget: usize) -> Vec<(MomentSpan, Vec<u32>)> {
    let span = MomentSpan {
        start_ms: 0,
        end_ms: duration_ms,
    };
    let mut count = ((duration_ms as usize / 1000) + 2).min(frame_budget);
    // Shared boundary images count against the transmitted-frame budget too.
    while count > 2 && count + (count - 2) / 7 > frame_budget {
        count -= 1;
    }
    let samples = spread_samples(span, count);
    let mut result = Vec::new();
    let mut start = 0;
    while start < samples.len() {
        let end = (start + 8).min(samples.len());
        let from = if start == 0 { 0 } else { samples[start] };
        let to = if end == samples.len() {
            duration_ms
        } else {
            samples[end - 1].saturating_add(1).min(duration_ms)
        };
        if to > from {
            result.push((
                MomentSpan {
                    start_ms: from,
                    end_ms: to,
                },
                samples[start..end].to_vec(),
            ));
        }
        if end == samples.len() {
            break;
        }
        start = end - 1;
    }
    result
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Proposal {
    pub event: String,
    pub role: String,
    pub reason: String,
    pub score: f64,
    pub core_start_ms: u32,
    pub core_end_ms: u32,
    pub payoff_start_ms: u32,
    pub payoff_end_ms: u32,
    pub setup: Option<MomentSpan>,
    pub context: Option<MomentSpan>,
    pub long_hold: bool,
    pub evidence_keys: Vec<String>,
    pub uncertainty: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepReply {
    pub moments: Vec<Proposal>,
    pub uncertain: bool,
    pub reason: String,
}

pub fn output_schema(max_moments: usize, duration_ms: u32, keys: &[String]) -> Value {
    let time = json!({"type":"integer","minimum":0,"maximum":duration_ms});
    let span = json!({"anyOf":[{"type":"null"},{"type":"object","additionalProperties":false,"properties":{"startMs":time.clone(),"endMs":time.clone()},"required":["startMs","endMs"]}]});
    let text = |n| json!({"type":"string","maxLength":n});
    let props = json!({
        "event":text(120),"role":{"type":"string","enum":["action","dialogue","atmosphere","transition","hold"]},"reason":text(400),
        "score":{"type":"number","minimum":0,"maximum":1},
        "coreStartMs":time,"coreEndMs":time,"payoffStartMs":time,"payoffEndMs":time,
        "setup":span,"context":span,"longHold":{"type":"boolean"},
        "evidenceKeys":{"type":"array","minItems":1,"maxItems":16,"items":{"type":"string","enum":keys}},
        "uncertainty":{"type":"array","maxItems":8,"items":text(240)}
    });
    let required = props
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    json!({"type":"object","additionalProperties":false,"properties":{
        "moments":{"type":"array","maxItems":max_moments,"items":{"type":"object","additionalProperties":false,"properties":props,"required":required}},
        "uncertain":{"type":"boolean"},"reason":text(400)},"required":["moments","uncertain","reason"]})
}

pub fn validate_reply(
    body: &str,
    span: MomentSpan,
    profile: EditProfile,
    allowed_keys: &[String],
    limit: usize,
) -> Result<StepReply> {
    ensure!(body.len() <= 32_768, "moment response exceeds byte limit");
    let json: Value = serde_json::from_str(body)?;
    let schema = output_schema(limit, span.end_ms, allowed_keys);
    ensure!(
        json["moments"].as_array().is_some_and(|values| values
            .iter()
            .all(|m| m.get("setup").is_some() && m.get("context").is_some())),
        "moment response omits required nullable fields"
    );
    let mut reply: StepReply = serde_json::from_value(json.clone())?;
    ensure!(
        reply.moments.len() <= limit && reply.reason.chars().count() <= 400,
        "moment response exceeds bounds"
    );
    ensure!(
        !reply.uncertain || !reply.reason.trim().is_empty(),
        "uncertain moment evidence requires a review reason"
    );
    let frame_times = allowed_keys
        .iter()
        .filter_map(|key| key.strip_prefix("frame:")?.parse::<u32>().ok())
        .collect::<Vec<_>>();
    let max_gap = sample_gap(span, &frame_times);
    for m in &mut reply.moments {
        let core = MomentSpan {
            start_ms: m.core_start_ms,
            end_ms: m.core_end_ms,
        };
        let payoff = MomentSpan {
            start_ms: m.payoff_start_ms,
            end_ms: m.payoff_end_ms,
        };
        ensure!(
            span.contains(core) && core.contains(payoff),
            "moment core/payoff outside inspected span"
        );
        let maximum = if m.long_hold {
            profile.hold_max_ms
        } else {
            profile.select_max_ms
        };
        ensure!(
            core.duration_ms() <= maximum,
            "moment core exceeds configured profile"
        );
        ensure!(
            m.score.is_finite() && (0.0..=1.0).contains(&m.score),
            "invalid moment score"
        );
        ensure!(
            !m.event.trim().is_empty()
                && m.event.chars().count() <= 120
                && !m.reason.trim().is_empty()
                && m.reason.chars().count() <= 400,
            "invalid moment explanation"
        );
        ensure!(
            ["action", "dialogue", "atmosphere", "transition", "hold"].contains(&m.role.as_str()),
            "invalid moment role"
        );
        ensure!(
            !m.evidence_keys.is_empty()
                && m.evidence_keys.len() <= 16
                && m.evidence_keys.iter().all(|key| allowed_keys.contains(key)),
            "moment references unavailable evidence"
        );
        ensure!(
            m.uncertainty.len() <= 8 && m.uncertainty.iter().all(|s| s.chars().count() <= 240),
            "moment uncertainty exceeds bounds"
        );
        ensure!(
            m.evidence_keys
                .iter()
                .filter_map(|key| key.strip_prefix("frame:")?.parse::<u32>().ok())
                .any(|at| at.saturating_add(max_gap) >= payoff.start_ms
                    && at <= payoff.end_ms.saturating_add(max_gap)),
            "moment payoff is not grounded in supplied visual evidence"
        );
        if m.role == "dialogue" {
            ensure!(
                m.evidence_keys
                    .iter()
                    .any(|key| key.starts_with("utterance:")),
                "dialogue moment requires accepted timed speech evidence"
            );
        }
        let minimum = if m.long_hold {
            profile.hold_min_ms
        } else {
            profile.select_min_ms
        }
        .min(span.duration_ms());
        if core.duration_ms() < minimum {
            m.uncertainty.push(
                "core is shorter than the configured edit profile; review necessary handles".into(),
            );
        }
        if let Some(setup) = m.setup {
            ensure!(
                span.contains(setup) && setup.end_ms <= payoff.start_ms,
                "invalid moment setup"
            );
        }
        if let Some(context) = m.context {
            ensure!(
                span.contains(context) && context.contains(core),
                "invalid moment context"
            );
        }
    }
    crate::analysis::schema::validate_json_structure(&json, &schema)
        .map_err(|error| anyhow::anyhow!("{}", error.as_reason()))?;
    Ok(reply)
}

pub fn validate_dialogue_evidence(reply: &StepReply, evidence: &[MomentEvidenceRef]) -> Result<()> {
    for proposal in reply.moments.iter().filter(|m| m.role == "dialogue") {
        let payoff = MomentSpan {
            start_ms: proposal.payoff_start_ms,
            end_ms: proposal.payoff_end_ms,
        };
        ensure!(
            evidence.iter().any(|reference| {
                reference.kind == "accepted_transcript"
                    && proposal.evidence_keys.contains(&reference.key)
                    && reference.span.is_some_and(|span| span.overlaps(payoff))
            }),
            "dialogue payoff does not overlap cited accepted timed speech"
        );
        let core = MomentSpan {
            start_ms: proposal.core_start_ms,
            end_ms: proposal.core_end_ms,
        };
        ensure!(
            evidence
                .iter()
                .filter(|reference| {
                    reference.kind == "accepted_transcript"
                        && proposal.evidence_keys.contains(&reference.key)
                        && reference.span.is_some_and(|span| span.overlaps(payoff))
                })
                .all(|reference| {
                    let utterance = reference.span.expect("filtered timed speech");
                    core.contains(utterance)
                        || proposal
                            .context
                            .is_some_and(|context| context.contains(utterance))
                }),
            "dialogue core cuts cited speech without preserving the full utterance in context"
        );
    }
    Ok(())
}

pub fn candidate(
    asset_id: &str,
    fingerprint: &str,
    proposal: &Proposal,
    allowed: MomentSpan,
    refined: bool,
    evidence: &[MomentEvidenceRef],
    dialogue: &[MomentSpan],
) -> MomentCandidate {
    let event = proposal.event.to_lowercase();
    let id = blake3::hash(
        format!(
            "{asset_id}\0{fingerprint}\0{event}\0{}\0{}",
            proposal.payoff_start_ms, proposal.payoff_end_ms
        )
        .as_bytes(),
    )
    .to_hex()
    .to_string();
    let core = MomentSpan {
        start_ms: proposal.core_start_ms,
        end_ms: proposal.core_end_ms,
    };
    MomentCandidate {
        id: format!("moment_{}", &id[..24]),
        rank: 0,
        score: proposal.score,
        core,
        payoff: MomentSpan {
            start_ms: proposal.payoff_start_ms,
            end_ms: proposal.payoff_end_ms,
        },
        setup: proposal.setup,
        context: proposal.context,
        allowed_range: allowed,
        reason: proposal.reason.clone(),
        event: proposal.event.clone(),
        role: proposal.role.clone(),
        evidence: evidence
            .iter()
            .filter(|e| proposal.evidence_keys.contains(&e.key))
            .cloned()
            .collect(),
        uncertainty: proposal.uncertainty.clone(),
        dialogue_boundaries: dialogue
            .iter()
            .copied()
            .filter(|s| s.overlaps(core))
            .collect(),
        refined,
        long_hold: proposal.long_hold,
    }
}

/// Only nearby detections of the same event/payoff are duplicates. Two events
/// with the same description separated by dead time must remain independent.
pub fn rank_and_deduplicate(
    mut candidates: Vec<MomentCandidate>,
    limit: usize,
) -> Vec<MomentCandidate> {
    candidates.sort_by(|a, b| {
        b.refined
            .cmp(&a.refined)
            .then(b.score.total_cmp(&a.score))
            .then(a.payoff.start_ms.cmp(&b.payoff.start_ms))
            .then(a.id.cmp(&b.id))
    });
    let mut result: Vec<MomentCandidate> = vec![];
    for candidate in candidates {
        let anchor =
            (u64::from(candidate.payoff.start_ms) + u64::from(candidate.payoff.end_ms)) / 2;
        if let Some(duplicate) = result.iter_mut().find(|other| {
            let other_anchor =
                (u64::from(other.payoff.start_ms) + u64::from(other.payoff.end_ms)) / 2;
            other.event.to_lowercase() == candidate.event.to_lowercase()
                && (other.payoff.overlaps(candidate.payoff)
                    || (anchor.abs_diff(other_anchor) <= 500
                        && other.evidence.iter().any(|e| {
                            e.kind == "proxy_frame"
                                && candidate.evidence.iter().any(|c| c.key == e.key)
                        })))
        }) {
            for uncertainty in candidate.uncertainty {
                if !duplicate.uncertainty.contains(&uncertainty) {
                    duplicate.uncertainty.push(uncertainty);
                }
            }
            let mut keys: BTreeSet<String> =
                duplicate.evidence.iter().map(|e| e.key.clone()).collect();
            for e in candidate.evidence {
                if keys.insert(e.key.clone()) {
                    duplicate.evidence.push(e);
                }
            }
            continue;
        }
        result.push(candidate);
    }
    result.truncate(limit);
    for (index, candidate) in result.iter_mut().enumerate() {
        candidate.rank = (index + 1) as u32;
    }
    result
}

pub fn refinement_span(
    candidate: &MomentCandidate,
    duration_ms: u32,
    profile: EditProfile,
) -> MomentSpan {
    let context = candidate.context.unwrap_or(candidate.core);
    // Include setup/payoff on both sides; longer events keep their evidence even
    // when the core select is short. Bound extraction to the configured hold.
    let start = if candidate.long_hold || candidate.role == "hold" {
        let midpoint = ((u64::from(candidate.payoff.start_ms) + u64::from(candidate.payoff.end_ms))
            / 2) as u32;
        midpoint.saturating_sub(profile.hold_max_ms / 2 + 2000)
    } else {
        context.start_ms.saturating_sub(2000)
    };
    let max_length = profile.hold_max_ms.saturating_add(4000);
    let end = if candidate.long_hold || candidate.role == "hold" {
        start.saturating_add(max_length).min(duration_ms)
    } else {
        context
            .end_ms
            .saturating_add(2000)
            .min(duration_ms)
            .min(start.saturating_add(max_length))
    };
    MomentSpan {
        start_ms: start,
        end_ms: end,
    }
}

pub fn require_page(offset: usize, limit: usize) -> Result<()> {
    if limit == 0 || limit > 50 || offset > 1_000_000 {
        bail!("invalid moment page; limit must be 1..=50 and offset <= 1000000");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_candidate(
        id: &str,
        start: u32,
        end: u32,
        uncertainty: &[&str],
    ) -> MomentCandidate {
        MomentCandidate {
            id: id.into(),
            rank: 0,
            score: 0.8,
            core: MomentSpan {
                start_ms: start,
                end_ms: end,
            },
            payoff: MomentSpan {
                start_ms: start + 100,
                end_ms: end - 100,
            },
            setup: None,
            context: None,
            allowed_range: MomentSpan {
                start_ms: 0,
                end_ms: 20_000,
            },
            reason: "synthetic evidence".into(),
            event: "penguin enters water".into(),
            role: "action".into(),
            evidence: vec![MomentEvidenceRef {
                kind: "proxy_frame".into(),
                key: format!("frame:{start}"),
                span: None,
            }],
            uncertainty: uncertainty.iter().map(|s| (*s).into()).collect(),
            dialogue_boundaries: vec![],
            refined: true,
            long_hold: false,
        }
    }

    fn dialogue_proposal() -> Proposal {
        Proposal {
            event: "speaker mentions the landing".into(),
            role: "dialogue".into(),
            reason: "timed speech overlaps the payoff".into(),
            score: 0.8,
            core_start_ms: 2_000,
            core_end_ms: 4_000,
            payoff_start_ms: 3_000,
            payoff_end_ms: 3_500,
            setup: None,
            context: None,
            long_hold: false,
            evidence_keys: vec!["utterance:0".into()],
            uncertainty: vec![],
        }
    }

    #[test]
    fn sparse_sampling_spans_the_entire_interval_and_late_windows() {
        let times = spread_samples(
            MomentSpan {
                start_ms: 0,
                end_ms: 60_000,
            },
            8,
        );
        assert_eq!(times.first(), Some(&0));
        assert_eq!(times.last(), Some(&59_999));
        assert!(times.iter().any(|time| *time > 50_000));
        let windows = coarse_windows(60_000, 64);
        assert!(windows.last().unwrap().0.end_ms == 60_000);
        assert!(windows.last().unwrap().1.iter().any(|time| *time > 50_000));
        assert!(windows.iter().all(|(_, times)| times.len() <= 8));
    }

    #[test]
    fn nearby_same_event_dedupes_but_disjoint_payoffs_and_uncertainty_survive() {
        let merged = rank_and_deduplicate(
            vec![
                synthetic_candidate("first", 1_000, 5_000, &[]),
                synthetic_candidate("duplicate", 2_000, 6_000, &["speech boundary unclear"]),
            ],
            8,
        );
        assert_eq!(merged.len(), 1);
        assert!(
            merged[0]
                .uncertainty
                .contains(&"speech boundary unclear".into())
        );

        let separate = rank_and_deduplicate(
            vec![
                synthetic_candidate("early", 1_000, 4_000, &[]),
                synthetic_candidate("late", 10_000, 13_000, &[]),
            ],
            8,
        );
        assert_eq!(separate.len(), 2, "dead time separates independent payoffs");
    }

    #[test]
    fn separate_same_event_payoffs_have_distinct_stable_ids() {
        let proposal = |start, end| Proposal {
            event: "same recurring action".into(),
            role: "action".into(),
            reason: "synthetic".into(),
            score: 0.8,
            core_start_ms: start - 1_000,
            core_end_ms: end + 1_000,
            payoff_start_ms: start,
            payoff_end_ms: end,
            setup: None,
            context: None,
            long_hold: false,
            evidence_keys: vec![format!("frame:{start}")],
            uncertainty: vec![],
        };
        let span = MomentSpan {
            start_ms: 0,
            end_ms: 10_000,
        };
        let first_evidence = vec![MomentEvidenceRef {
            kind: "proxy_frame".into(),
            key: "frame:1000".into(),
            span: None,
        }];
        let second_evidence = vec![MomentEvidenceRef {
            kind: "proxy_frame".into(),
            key: "frame:1200".into(),
            span: None,
        }];
        let first = candidate(
            "asset",
            "fingerprint",
            &proposal(1_000, 1_100),
            span,
            true,
            &first_evidence,
            &[],
        );
        let second = candidate(
            "asset",
            "fingerprint",
            &proposal(1_200, 1_300),
            span,
            true,
            &second_evidence,
            &[],
        );
        let separate = rank_and_deduplicate(vec![first, second], 8);
        assert_eq!(separate.len(), 2);
        assert_ne!(
            separate[0].id, separate[1].id,
            "distinct payoffs must not collide in stable identity"
        );
    }

    #[test]
    fn dialogue_payoff_must_overlap_cited_timed_utterance() {
        let reply = StepReply {
            moments: vec![dialogue_proposal()],
            uncertain: false,
            reason: String::new(),
        };
        let utterance = MomentEvidenceRef {
            kind: "accepted_transcript".into(),
            key: "utterance:0".into(),
            span: Some(MomentSpan {
                start_ms: 3_100,
                end_ms: 3_900,
            }),
        };
        assert!(validate_dialogue_evidence(&reply, std::slice::from_ref(&utterance)).is_ok());
        let unrelated = MomentEvidenceRef {
            span: Some(MomentSpan {
                start_ms: 500,
                end_ms: 1_500,
            }),
            ..utterance
        };
        assert!(validate_dialogue_evidence(&reply, &[unrelated]).is_err());
    }

    #[test]
    fn dialogue_core_must_not_cut_a_cited_utterance_without_preserving_context() {
        let reply = StepReply {
            moments: vec![dialogue_proposal()],
            uncertain: false,
            reason: String::new(),
        };
        let full_utterance = MomentEvidenceRef {
            kind: "accepted_transcript".into(),
            key: "utterance:0".into(),
            span: Some(MomentSpan {
                start_ms: 1_000,
                end_ms: 5_000,
            }),
        };
        assert!(
            validate_dialogue_evidence(&reply, &[full_utterance]).is_err(),
            "a clipped utterance needs uncertainty or a context span preserving its full boundary"
        );
    }

    #[test]
    fn dialogue_context_may_preserve_the_full_cited_utterance() {
        let mut proposal = dialogue_proposal();
        proposal.context = Some(MomentSpan {
            start_ms: 500,
            end_ms: 5_500,
        });
        let reply = StepReply {
            moments: vec![proposal],
            uncertain: false,
            reason: String::new(),
        };
        let full_utterance = MomentEvidenceRef {
            kind: "accepted_transcript".into(),
            key: "utterance:0".into(),
            span: Some(MomentSpan {
                start_ms: 1_000,
                end_ms: 5_000,
            }),
        };
        assert!(validate_dialogue_evidence(&reply, &[full_utterance]).is_ok());
    }
}
