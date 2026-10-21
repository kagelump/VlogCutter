//! Human review for clock blocks whose plausible corrections disagree by a
//! civil day or more. These questions keep discrete alternatives intact; they
//! are not probability intervals or solver confidence scores.
use super::model::ClockEpoch;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorKind {
    EventAtUtc,
    EarlierThan,
}

/// Evidence entered by a reviewer. Overlap assumptions are explicitly marked
/// as assumptions and never become hard solver constraints by themselves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanAnchor {
    pub kind: AnchorKind,
    pub asset_id: String,
    pub reference_asset_id: Option<String>,
    pub event_label: String,
    pub utc_time: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockAlternative {
    pub id: String,
    pub label: String,
    pub epoch: ClockEpoch,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyExcerpt {
    pub asset_id: String,
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockBlockQuestion {
    pub id: String,
    pub capture_sequence_id: String,
    pub start_counter: u64,
    pub end_counter: Option<u64>,
    pub alternatives: Vec<ClockAlternative>,
    pub proxy_excerpts: Vec<ProxyExcerpt>,
    pub anchor: HumanAnchor,
    /// Human-entered contextual evidence only. Same-operator/activity overlap
    /// is not inferred as a hard constraint.
    pub contextual_evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockQuestionDecisionKind {
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockQuestionDecision {
    pub question_id: String,
    pub kind: ClockQuestionDecisionKind,
    pub alternative_id: Option<String>,
    pub note: String,
}

/// A durable answer tied to the exact proposal revision it answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockQuestionOutcome {
    pub decision: ClockQuestionDecision,
    pub proposal_revision: u64,
}

impl ClockBlockQuestion {
    /// Reject malformed or unbounded proposals before they reach a write API.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() || self.capture_sequence_id.trim().is_empty() {
            return Err("question and capture sequence identities are required".into());
        }
        if self.end_counter.is_none() {
            return Err("clock question scope must have a finite end counter".into());
        }
        if self.end_counter.is_some_and(|end| end < self.start_counter) {
            return Err("question scope ends before it starts".into());
        }
        if self.start_counter > i64::MAX as u64
            || self.end_counter.is_some_and(|end| end > i64::MAX as u64)
        {
            return Err("question counter bounds exceed the catalog's supported range".into());
        }
        if self.alternatives.len() < 2 || self.alternatives.len() > 8 {
            return Err("a clock question must contain between two and eight alternatives".into());
        }
        if self.proxy_excerpts.len() > 20 || self.contextual_evidence.len() > 64 {
            return Err("clock question evidence exceeds the bounded bundle limits".into());
        }
        if self.anchor.event_label.trim().is_empty() {
            return Err("human anchor requires an event label".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut epoch_ids = std::collections::BTreeSet::new();
        let mut offsets = std::collections::BTreeSet::new();
        for alternative in &self.alternatives {
            if alternative.id.trim().is_empty() || !ids.insert(&alternative.id) {
                return Err("alternative identities must be nonempty and unique".into());
            }
            let epoch = &alternative.epoch;
            if epoch.id.trim().is_empty() || !epoch_ids.insert(&epoch.id) {
                return Err("alternative epoch identities must be nonempty and unique".into());
            }
            if !offsets.insert(epoch.offset_seconds) {
                return Err("clock alternatives must represent distinct offsets".into());
            }
            if epoch.capture_sequence_id != self.capture_sequence_id
                || epoch.start_counter != self.start_counter
                || epoch.end_counter != self.end_counter
            {
                return Err("alternative epoch must match the exact question scope".into());
            }
        }
        for excerpt in &self.proxy_excerpts {
            if excerpt.role != "selected_proxy" && excerpt.role != "independent_reference" {
                return Err(
                    "proxy excerpt role must be selected_proxy or independent_reference".into(),
                );
            }
            if excerpt.asset_id.is_empty()
                || !excerpt.start_seconds.is_finite()
                || !excerpt.end_seconds.is_finite()
                || excerpt.start_seconds < 0.0
                || excerpt.end_seconds <= excerpt.start_seconds
            {
                return Err("proxy excerpts require an asset and a positive finite range".into());
            }
        }
        if !self
            .proxy_excerpts
            .iter()
            .any(|excerpt| excerpt.role == "selected_proxy")
            || !self
                .proxy_excerpts
                .iter()
                .any(|excerpt| excerpt.role == "independent_reference")
        {
            return Err(
                "question requires selected proxy and independent reference excerpts".into(),
            );
        }
        match self.anchor.kind {
            AnchorKind::EventAtUtc if self.anchor.utc_time.as_deref().is_none_or(str::is_empty) => {
                return Err("event/date anchor requires a UTC time".into());
            }
            AnchorKind::EarlierThan
                if self
                    .anchor
                    .reference_asset_id
                    .as_deref()
                    .is_none_or(str::is_empty) =>
            {
                return Err("relative-order anchor requires a reference asset".into());
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal() -> ClockBlockQuestion {
        let epoch = |id: &str, offset| ClockEpoch {
            id: id.into(),
            capture_sequence_id: "synthetic-sequence".into(),
            start_counter: 100,
            end_counter: Some(140),
            offset_seconds: offset,
            confidence: 1.0,
            evidence: vec!["synthetic sidecar claim".into()],
            label: id.into(),
        };
        ClockBlockQuestion {
            id: "synthetic-question".into(),
            capture_sequence_id: "synthetic-sequence".into(),
            start_counter: 100,
            end_counter: Some(140),
            alternatives: vec![
                ClockAlternative {
                    id: "date-a".into(),
                    label: "Reference date A".into(),
                    epoch: epoch("epoch-a", 0),
                    evidence: vec!["synthetic reference excerpt".into()],
                },
                ClockAlternative {
                    id: "date-b".into(),
                    label: "Reference date B".into(),
                    epoch: epoch("epoch-b", 86_400),
                    evidence: vec!["synthetic reference excerpt".into()],
                },
            ],
            proxy_excerpts: vec![
                ProxyExcerpt {
                    asset_id: "synthetic-clip-1".into(),
                    start_seconds: 2.0,
                    end_seconds: 5.0,
                    role: "selected_proxy".into(),
                },
                ProxyExcerpt {
                    asset_id: "synthetic-reference-1".into(),
                    start_seconds: 1.0,
                    end_seconds: 3.0,
                    role: "independent_reference".into(),
                },
            ],
            anchor: HumanAnchor {
                kind: AnchorKind::EventAtUtc,
                asset_id: "synthetic-clip-1".into(),
                reference_asset_id: None,
                event_label: "synthetic event".into(),
                utc_time: Some("2030-01-01T12:00:00Z".into()),
            },
            contextual_evidence: vec!["same-operator overlap is reviewer-supplied context".into()],
        }
    }

    #[test]
    fn alternatives_keep_day_separation_as_discrete_choices() {
        let question = proposal();
        question.validate().unwrap();
        assert_eq!(
            question.alternatives[1].epoch.offset_seconds
                - question.alternatives[0].epoch.offset_seconds,
            86_400
        );
    }

    #[test]
    fn rejects_unbounded_or_mismatched_question_scope() {
        let mut question = proposal();
        question.alternatives[1].epoch.end_counter = None;
        assert!(
            question
                .validate()
                .unwrap_err()
                .contains("exact question scope")
        );
        question = proposal();
        question.alternatives.truncate(1);
        assert!(question.validate().unwrap_err().contains("two and eight"));
    }

    #[test]
    fn rejects_duplicate_offsets_after_the_first_alternative() {
        let mut question = proposal();
        let mut third = question.alternatives[1].clone();
        third.id = "date-c".into();
        third.epoch.id = "epoch-c".into();
        question.alternatives.push(third);
        assert!(
            question
                .validate()
                .unwrap_err()
                .contains("distinct offsets")
        );
    }
}
