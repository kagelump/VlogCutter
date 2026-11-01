//! Revision-local editorial story policy and chronology-safe placement.
//!
//! This module deliberately does not solve clocks. It consumes the existing
//! chronology solution and turns exact declared editorial exceptions into a
//! small, auditable set of allowed sequence edges.

use super::{EditPlanOperation, EditPlanRevision};
use crate::chronology::{ChronologyFacts, ChronologySolution};
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const EDIT_PLAN_STORY_POLICY_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoryDecisionActor {
    Agent,
    Human,
}

/// Who made a story decision and why. An agent decision is never rewritten as
/// human approval; callers that need approval must supply `Human` themselves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoryDecision {
    pub actor: StoryDecisionActor,
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
}

impl StoryDecision {
    pub fn human_approved(&self) -> bool {
        self.actor == StoryDecisionActor::Human
    }
}

/// Exact identity of one immutable cut. A stale range or asset identity makes
/// every policy item that names the cut invalid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoryCutIdentity {
    pub operation_id: String,
    pub asset_id: String,
    pub source_in_seconds: f64,
    pub source_out_seconds: f64,
}

impl StoryCutIdentity {
    pub fn from_operation(operation: &EditPlanOperation) -> Self {
        Self {
            operation_id: operation.operation_id.clone(),
            asset_id: operation.asset_id.clone(),
            source_in_seconds: operation.source_in_seconds,
            source_out_seconds: operation.source_out_seconds,
        }
    }

    pub fn matches(&self, operation: &EditPlanOperation) -> bool {
        self.operation_id == operation.operation_id
            && self.asset_id == operation.asset_id
            && (self.source_in_seconds - operation.source_in_seconds).abs() <= 1e-9
            && (self.source_out_seconds - operation.source_out_seconds).abs() <= 1e-9
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoryOpeningTeaser {
    pub operation_id: String,
    pub asset_id: String,
    pub source_in_seconds: f64,
    pub source_out_seconds: f64,
    pub rationale: String,
    pub decision: StoryDecision,
    /// Filled by the core before persistence. Supplied values are checked
    /// against current chronology facts.
    #[serde(default)]
    pub chronology_dependency: String,
}

impl StoryOpeningTeaser {
    pub fn cut(&self) -> StoryCutIdentity {
        StoryCutIdentity {
            operation_id: self.operation_id.clone(),
            asset_id: self.asset_id.clone(),
            source_in_seconds: self.source_in_seconds,
            source_out_seconds: self.source_out_seconds,
        }
    }
}

/// An exact ordered contiguous sequence the editor intentionally protects.
/// The group's own order is documentary story order, so validation never sorts
/// inside it. It is still not a project-wide chronology exemption.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtectedStoryGroup {
    pub group_id: String,
    pub cuts: Vec<StoryCutIdentity>,
    pub rationale: String,
    pub decision: StoryDecision,
    #[serde(default)]
    pub chronology_dependency: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoryOrderEdge {
    pub earlier_operation_id: String,
    pub later_operation_id: String,
}

impl StoryOrderEdge {
    pub fn key(&self) -> (String, String) {
        (
            self.earlier_operation_id.clone(),
            self.later_operation_id.clone(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoryExceptionScope {
    pub cuts: Vec<StoryCutIdentity>,
    pub edges: Vec<StoryOrderEdge>,
    #[serde(default)]
    pub chronology_dependency: String,
}

/// One narrowly scoped editorial exception. There is intentionally no wildcard
/// or project-wide disable form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoryChronologyException {
    pub exception_id: String,
    pub rationale: String,
    pub decision: StoryDecision,
    pub scope: StoryExceptionScope,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditPlanStoryPolicy {
    pub version: u32,
    #[serde(default)]
    pub opening_teasers: Vec<StoryOpeningTeaser>,
    #[serde(default)]
    pub protected_groups: Vec<ProtectedStoryGroup>,
    #[serde(default)]
    pub exceptions: Vec<StoryChronologyException>,
}

impl Default for EditPlanStoryPolicy {
    fn default() -> Self {
        Self {
            version: EDIT_PLAN_STORY_POLICY_VERSION,
            opening_teasers: Vec::new(),
            protected_groups: Vec::new(),
            exceptions: Vec::new(),
        }
    }
}

impl EditPlanStoryPolicy {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.opening_teasers.is_empty()
            && self.protected_groups.is_empty()
            && self.exceptions.is_empty()
    }

    pub fn validate_shape(&self) -> Result<()> {
        ensure!(
            self.version == EDIT_PLAN_STORY_POLICY_VERSION,
            "unsupported edit-plan story policy version {}",
            self.version
        );
        ensure!(
            self.opening_teasers.len() <= 64
                && self.protected_groups.len() <= 256
                && self.exceptions.len() <= 2_000,
            "edit-plan story policy exceeds its bounded collection limits"
        );
        let mut cut_ids = BTreeSet::new();
        for teaser in &self.opening_teasers {
            validate_decision("opening teaser", &teaser.rationale, &teaser.decision)?;
            ensure!(
                !teaser.operation_id.is_empty() && cut_ids.insert(teaser.operation_id.clone()),
                "opening teaser operation IDs must be non-empty and unique"
            );
        }
        let mut group_ids = BTreeSet::new();
        for group in &self.protected_groups {
            ensure!(
                !group.group_id.is_empty() && group_ids.insert(group.group_id.clone()),
                "protected group IDs must be non-empty and unique"
            );
            ensure!(
                !group.cuts.is_empty() && group.cuts.len() <= 512,
                "protected groups need 1-512 exact cuts"
            );
            validate_decision("protected group", &group.rationale, &group.decision)?;
            for cut in &group.cuts {
                ensure!(
                    !cut.operation_id.is_empty(),
                    "protected group cut IDs must be non-empty"
                );
                ensure!(
                    cut_ids.insert(cut.operation_id.clone()),
                    "cut {} appears more than once across opening teasers and protected groups",
                    cut.operation_id
                );
            }
        }
        let mut exception_ids = BTreeSet::new();
        for exception in &self.exceptions {
            ensure!(
                !exception.exception_id.is_empty()
                    && exception_ids.insert(exception.exception_id.clone()),
                "chronology exception IDs must be non-empty and unique"
            );
            validate_decision(
                "chronology exception",
                &exception.rationale,
                &exception.decision,
            )?;
            ensure!(
                !exception.scope.cuts.is_empty() && exception.scope.cuts.len() <= 64,
                "chronology exceptions need 1-64 exact cuts"
            );
            ensure!(
                !exception.scope.edges.is_empty() && exception.scope.edges.len() <= 256,
                "chronology exceptions need 1-256 exact adjacent edges"
            );
            let scope_ids: BTreeSet<&str> = exception
                .scope
                .cuts
                .iter()
                .map(|cut| cut.operation_id.as_str())
                .collect();
            ensure!(
                scope_ids.len() == exception.scope.cuts.len(),
                "chronology exception cut IDs must be unique"
            );
            let mut edge_keys = BTreeSet::new();
            for edge in &exception.scope.edges {
                ensure!(
                    scope_ids.contains(edge.earlier_operation_id.as_str())
                        && scope_ids.contains(edge.later_operation_id.as_str())
                        && edge_keys.insert(edge.key()),
                    "chronology exception edges must be unique and bind only declared cuts"
                );
            }
        }
        Ok(())
    }

    /// All exact cut identities named by the policy.
    pub fn cut_identities(&self) -> Vec<StoryCutIdentity> {
        let mut cuts = Vec::new();
        cuts.extend(self.opening_teasers.iter().map(StoryOpeningTeaser::cut));
        cuts.extend(
            self.protected_groups
                .iter()
                .flat_map(|group| group.cuts.iter().cloned()),
        );
        cuts.extend(
            self.exceptions
                .iter()
                .flat_map(|exception| exception.scope.cuts.iter().cloned()),
        );
        cuts
    }

    pub fn exact_edge_keys(&self) -> BTreeSet<(String, String)> {
        self.exceptions
            .iter()
            .flat_map(|exception| exception.scope.edges.iter().map(StoryOrderEdge::key))
            .collect()
    }
}

fn validate_decision(subject: &str, rationale: &str, decision: &StoryDecision) -> Result<()> {
    ensure!(
        !rationale.trim().is_empty(),
        "{subject} rationale must not be empty"
    );
    ensure!(
        !decision.rationale.trim().is_empty(),
        "{subject} decision rationale must not be empty"
    );
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct ChronologyPlacement {
    pub trip_time_millis: Option<i64>,
    pub trip_time: Option<String>,
    pub confidence: f32,
    pub uncertainty_seconds: u32,
}

pub(crate) fn placement_index(
    solution: &ChronologySolution,
) -> BTreeMap<String, ChronologyPlacement> {
    solution
        .assets
        .iter()
        .map(|asset| {
            let trip_time_millis = asset
                .trip_time
                .as_deref()
                .and_then(crate::chronology::parse_utc)
                .map(|time| time.timestamp_millis());
            (
                asset.asset_id.clone(),
                ChronologyPlacement {
                    trip_time_millis,
                    trip_time: asset.trip_time.clone(),
                    confidence: asset.confidence,
                    uncertainty_seconds: asset.uncertainty_seconds,
                },
            )
        })
        .collect()
}

/// A stable identity for only the chronology facts that affect the named
/// assets. Changing a camera epoch, exception, source timestamp or hard order
/// constraint for those assets invalidates exception decisions even if the
/// retained main plan happens to remain monotonic.
pub(crate) fn chronology_dependency_token(
    facts: &ChronologyFacts,
    asset_ids: impl IntoIterator<Item = String>,
) -> Result<String> {
    let asset_ids: BTreeSet<String> = asset_ids.into_iter().collect();
    let assets: Vec<_> = facts
        .assets
        .iter()
        .filter(|asset| asset_ids.contains(&asset.asset_id))
        .collect();
    let sequence_ids: BTreeSet<String> = assets
        .iter()
        .filter_map(|asset| asset.capture_sequence_id.clone())
        .collect();
    let sequences: Vec<_> = facts
        .sequences
        .iter()
        .filter(|sequence| sequence_ids.contains(&sequence.id))
        .collect();
    let epochs: Vec<_> = facts
        .epochs
        .iter()
        .filter(|epoch| sequence_ids.contains(&epoch.capture_sequence_id))
        .collect();
    let exceptions: Vec<_> = facts
        .exceptions
        .iter()
        .filter(|exception| asset_ids.contains(&exception.asset_id))
        .collect();
    let constraints: Vec<_> = facts
        .constraints
        .iter()
        .filter(|constraint| {
            asset_ids.contains(&constraint.earlier_asset_id)
                || asset_ids.contains(&constraint.later_asset_id)
        })
        .collect();
    let identity = serde_json::json!({
        "assets": assets,
        "sequences": sequences,
        "epochs": epochs,
        "exceptions": exceptions,
        "constraints": constraints,
    });
    Ok(blake3::hash(&serde_json::to_vec(&identity)?)
        .to_hex()
        .to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoryIssueKind {
    PolicyInvalid,
    StaleScope,
    ProtectedGroupViolation,
    ChronologyViolation,
    ChronologyAmbiguous,
    ChronologyUnresolved,
}

impl StoryIssueKind {
    pub(crate) fn is_error(self) -> bool {
        !matches!(self, Self::ChronologyAmbiguous | Self::ChronologyUnresolved)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct StoryIssue {
    pub kind: StoryIssueKind,
    pub position: Option<u32>,
    pub asset_id: Option<String>,
    pub message: String,
}

fn issue(
    kind: StoryIssueKind,
    position: Option<u32>,
    asset_id: Option<&str>,
    message: impl Into<String>,
) -> StoryIssue {
    StoryIssue {
        kind,
        position,
        asset_id: asset_id.map(str::to_string),
        message: message.into(),
    }
}

/// Validate a revision's policy and chronology against the existing solution.
/// The returned issues are deterministic: policy order first, then sequence
/// position and stable edge identity.
pub(crate) fn validate_revision_chronology(
    revision: &EditPlanRevision,
    facts: &ChronologyFacts,
    solution: &ChronologySolution,
) -> Vec<StoryIssue> {
    let mut issues = Vec::new();
    if let Some(policy) = &revision.story_policy
        && let Err(error) = policy.validate_shape()
    {
        return vec![issue(
            StoryIssueKind::PolicyInvalid,
            None,
            None,
            error.to_string(),
        )];
    }

    if !solution.contradictions.is_empty() {
        for contradiction in &solution.contradictions {
            issues.push(issue(
                StoryIssueKind::ChronologyViolation,
                None,
                None,
                format!(
                    "Corrected chronology has a blocking contradiction: {}",
                    contradiction.message
                ),
            ));
        }
    }

    let by_id: BTreeMap<&str, &EditPlanOperation> = revision
        .operations
        .iter()
        .map(|operation| (operation.operation_id.as_str(), operation))
        .collect();
    if by_id.len() != revision.operations.len() {
        issues.push(issue(
            StoryIssueKind::PolicyInvalid,
            None,
            None,
            "edit plan revision contains duplicate cut identities".to_string(),
        ));
        return issues;
    }
    let placement = placement_index(solution);

    let mut teaser_ids = BTreeSet::new();
    let mut protected_ids = BTreeSet::new();
    let mut valid_exception_edges = BTreeSet::new();
    let mut exception_edge_evidence: Vec<(&StoryOrderEdge, String)> = Vec::new();

    if let Some(policy) = &revision.story_policy {
        for (index, teaser) in policy.opening_teasers.iter().enumerate() {
            let Some(operation) = by_id.get(teaser.operation_id.as_str()) else {
                issues.push(issue(
                    StoryIssueKind::StaleScope,
                    None,
                    Some(&teaser.asset_id),
                    format!(
                        "Opening teaser {} refers to missing cut {}.",
                        index + 1,
                        teaser.operation_id
                    ),
                ));
                continue;
            };
            if !teaser.cut().matches(operation) {
                issues.push(issue(
                    StoryIssueKind::StaleScope,
                    Some(operation.position),
                    Some(&operation.asset_id),
                    format!(
                        "Opening teaser {} no longer matches cut {}'s exact asset/range identity.",
                        index + 1,
                        teaser.operation_id
                    ),
                ));
                continue;
            }
            if operation.position as usize != index {
                issues.push(issue(
                    StoryIssueKind::PolicyInvalid,
                    Some(operation.position),
                    Some(&operation.asset_id),
                    format!(
                        "Opening teaser {} must be an exact contiguous prefix at position {}.",
                        teaser.operation_id, index
                    ),
                ));
            }
            let token = chronology_dependency_token(facts, [operation.asset_id.clone()]);
            match token {
                Ok(token) if token != teaser.chronology_dependency => issues.push(issue(
                    StoryIssueKind::StaleScope,
                    Some(operation.position),
                    Some(&operation.asset_id),
                    format!(
                        "Opening teaser {} is stale because the chronology facts for its source changed.",
                        teaser.operation_id
                    ),
                )),
                Err(error) => issues.push(issue(
                    StoryIssueKind::PolicyInvalid,
                    Some(operation.position),
                    Some(&operation.asset_id),
                    format!("Opening teaser dependency could not be computed: {error}"),
                )),
                _ => {}
            }
            teaser_ids.insert(teaser.operation_id.clone());
        }

        for group in &policy.protected_groups {
            let mut positions = Vec::new();
            for cut in &group.cuts {
                let Some(operation) = by_id.get(cut.operation_id.as_str()) else {
                    issues.push(issue(
                        StoryIssueKind::StaleScope,
                        None,
                        Some(&cut.asset_id),
                        format!(
                            "Protected group {} refers to missing cut {}.",
                            group.group_id, cut.operation_id
                        ),
                    ));
                    continue;
                };
                if !cut.matches(operation) {
                    issues.push(issue(
                        StoryIssueKind::StaleScope,
                        Some(operation.position),
                        Some(&operation.asset_id),
                        format!(
                            "Protected group {} no longer matches cut {}'s exact asset/range identity.",
                            group.group_id, cut.operation_id
                        ),
                    ));
                    continue;
                }
                positions.push(operation.position as usize);
                protected_ids.insert(cut.operation_id.clone());
            }
            if positions.len() == group.cuts.len()
                && positions.windows(2).any(|pair| pair[1] != pair[0] + 1)
            {
                issues.push(issue(
                    StoryIssueKind::ProtectedGroupViolation,
                    positions.first().map(|position| *position as u32),
                    None,
                    format!(
                        "Protected group {} is no longer contiguous and in its documented order.",
                        group.group_id
                    ),
                ));
            }
            let assets: Vec<String> = group.cuts.iter().map(|cut| cut.asset_id.clone()).collect();
            match chronology_dependency_token(facts, assets) {
                Ok(token) if token != group.chronology_dependency => issues.push(issue(
                    StoryIssueKind::StaleScope,
                    positions.first().map(|position| *position as u32),
                    None,
                    format!(
                        "Protected group {} is stale because chronology facts for its cuts changed.",
                        group.group_id
                    ),
                )),
                Err(error) => issues.push(issue(
                    StoryIssueKind::PolicyInvalid,
                    None,
                    None,
                    format!("Protected group dependency could not be computed: {error}"),
                )),
                _ => {}
            }
        }

        for exception in &policy.exceptions {
            let mut exact = true;
            let assets: Vec<String> = exception
                .scope
                .cuts
                .iter()
                .map(|cut| cut.asset_id.clone())
                .collect();
            for cut in &exception.scope.cuts {
                let Some(operation) = by_id.get(cut.operation_id.as_str()) else {
                    exact = false;
                    issues.push(issue(
                        StoryIssueKind::StaleScope,
                        None,
                        Some(&cut.asset_id),
                        format!(
                            "Chronology exception {} refers to missing cut {}.",
                            exception.exception_id, cut.operation_id
                        ),
                    ));
                    continue;
                };
                if !cut.matches(operation) {
                    exact = false;
                    issues.push(issue(
                        StoryIssueKind::StaleScope,
                        Some(operation.position),
                        Some(&operation.asset_id),
                        format!(
                            "Chronology exception {} no longer matches cut {}'s exact asset/range identity.",
                            exception.exception_id, cut.operation_id
                        ),
                    ));
                }
            }
            for edge in &exception.scope.edges {
                let adjacency = by_id
                    .get(edge.earlier_operation_id.as_str())
                    .zip(by_id.get(edge.later_operation_id.as_str()))
                    .is_some_and(|(earlier, later)| later.position == earlier.position + 1);
                if !adjacency {
                    exact = false;
                    issues.push(issue(
                        StoryIssueKind::StaleScope,
                        None,
                        None,
                        format!(
                            "Chronology exception {} edge {} -> {} is no longer adjacent.",
                            exception.exception_id,
                            edge.earlier_operation_id,
                            edge.later_operation_id
                        ),
                    ));
                }
                exception_edge_evidence.push((edge, exception.exception_id.clone()));
            }
            match chronology_dependency_token(facts, assets) {
                Ok(token) if token != exception.scope.chronology_dependency => {
                    exact = false;
                    issues.push(issue(
                        StoryIssueKind::StaleScope,
                        None,
                        None,
                        format!(
                            "Chronology exception {} is stale because its source chronology facts changed.",
                            exception.exception_id
                        ),
                    ));
                }
                Err(error) => {
                    exact = false;
                    issues.push(issue(
                        StoryIssueKind::PolicyInvalid,
                        None,
                        None,
                        format!("Chronology exception dependency could not be computed: {error}"),
                    ));
                }
                _ => {}
            }
            if exact {
                for edge in &exception.scope.edges {
                    valid_exception_edges.insert(edge.key());
                }
            }
        }
    }

    let mut needs_exception_edges = BTreeSet::new();
    for pair in revision.operations.windows(2) {
        let earlier = &pair[0];
        let later = &pair[1];
        if teaser_ids.contains(&earlier.operation_id) {
            continue;
        }
        if protected_ids.contains(&earlier.operation_id)
            && protected_ids.contains(&later.operation_id)
        {
            continue;
        }
        let edge = (earlier.operation_id.clone(), later.operation_id.clone());
        let Some(earlier_time) = placement.get(&earlier.asset_id) else {
            needs_exception_edges.insert(edge.clone());
            if !valid_exception_edges.contains(&edge) {
                issues.push(issue(
                    StoryIssueKind::ChronologyUnresolved,
                    Some(later.position),
                    Some(&later.asset_id),
                    format!(
                        "Cut {} has no solved source chronology. Use a narrowly scoped story exception to choose an explicit placement.",
                        earlier.asset_id
                    ),
                ));
            }
            continue;
        };
        let Some(later_time) = placement.get(&later.asset_id) else {
            needs_exception_edges.insert(edge.clone());
            if !valid_exception_edges.contains(&edge) {
                issues.push(issue(
                    StoryIssueKind::ChronologyUnresolved,
                    Some(later.position),
                    Some(&later.asset_id),
                    format!(
                        "Cut {} has no solved source chronology. Use a narrowly scoped story exception to choose an explicit placement.",
                        later.asset_id
                    ),
                ));
            }
            continue;
        };
        match (earlier_time.trip_time_millis, later_time.trip_time_millis) {
            (Some(earlier_millis), Some(later_millis)) if earlier_millis > later_millis => {
                needs_exception_edges.insert(edge.clone());
                if !valid_exception_edges.contains(&edge) {
                    issues.push(issue(
                        StoryIssueKind::ChronologyViolation,
                        Some(later.position),
                        Some(&later.asset_id),
                        format!(
                            "Position {} ({}) is corrected source time {} with solver confidence {:.2} and {:.0}s uncertainty, but position {} ({}) is earlier at {}. Supply a narrowly scoped story exception or move the cut; filenames and retrieval rank are not story order.",
                            earlier.position,
                            earlier.asset_id,
                            earlier_time.trip_time.as_deref().unwrap_or("unknown"),
                            earlier_time.confidence,
                            earlier_time.uncertainty_seconds,
                            later.position,
                            later.asset_id,
                            later_time.trip_time.as_deref().unwrap_or("unknown")
                        ),
                    ));
                }
            }
            (Some(_), Some(_)) => {
                if earlier.asset_id != later.asset_id {
                    needs_exception_edges.insert(edge.clone());
                    if !valid_exception_edges.contains(&edge) {
                        issues.push(issue(
                            StoryIssueKind::ChronologyAmbiguous,
                            Some(later.position),
                            Some(&later.asset_id),
                            format!(
                                "Positions {} and {} have the same corrected source time. An exact scoped story decision is required for different-source ties.",
                                earlier.position, later.position
                            ),
                        ));
                    }
                } else if earlier.source_in_seconds >= later.source_out_seconds - 1e-9 {
                    needs_exception_edges.insert(edge.clone());
                    if !valid_exception_edges.contains(&edge) {
                        issues.push(issue(
                            StoryIssueKind::ChronologyViolation,
                            Some(later.position),
                            Some(&later.asset_id),
                            format!(
                                "Positions {} and {} run backward through the same source range at the same corrected time. Use forward source ranges or an exact scoped story exception.",
                                earlier.position, later.position
                            ),
                        ));
                    }
                } else if earlier.source_out_seconds > later.source_in_seconds + 1e-9 {
                    needs_exception_edges.insert(edge.clone());
                    if !valid_exception_edges.contains(&edge) {
                        issues.push(issue(
                            StoryIssueKind::ChronologyAmbiguous,
                            Some(later.position),
                            Some(&later.asset_id),
                            format!(
                                "Positions {} and {} reuse overlapping source ranges at the same corrected time; the inline order is ambiguous.",
                                earlier.position, later.position
                            ),
                        ));
                    }
                }
            }
            _ => {
                needs_exception_edges.insert(edge.clone());
                if !valid_exception_edges.contains(&edge) {
                    issues.push(issue(
                        StoryIssueKind::ChronologyUnresolved,
                        Some(later.position),
                        Some(&later.asset_id),
                        format!(
                            "Positions {} and {} cannot be compared because a solved source time is missing. Use an exact scoped story exception to choose the placement.",
                            earlier.position, later.position
                        ),
                    ));
                }
            }
        }
    }

    // An exception that no longer suppresses a real chronology ambiguity is
    // stale. It is not a general switch and must not linger as permission.
    if let Some(policy) = &revision.story_policy {
        for exception in &policy.exceptions {
            for edge in &exception.scope.edges {
                if !needs_exception_edges.contains(&edge.key()) {
                    issues.push(issue(
                        StoryIssueKind::StaleScope,
                        None,
                        None,
                        format!(
                            "Chronology exception {} no longer covers a backward chronology edge; remove or re-scope it.",
                            exception.exception_id
                        ),
                    ));
                }
            }
        }
    }

    // Stable output independent of evidence loading or collection order.
    issues.sort_by(|left, right| {
        left.position
            .cmp(&right.position)
            .then_with(|| format!("{:?}", left.kind).cmp(&format!("{:?}", right.kind)))
            .then_with(|| left.message.cmp(&right.message))
    });
    issues
}

/// Allowed edges for syntax-level placement simulation. Callers still run the
/// full validator after constructing the candidate.
pub(crate) fn allowed_exception_edges(
    policy: Option<&EditPlanStoryPolicy>,
) -> BTreeSet<(String, String)> {
    policy
        .map(EditPlanStoryPolicy::exact_edge_keys)
        .unwrap_or_default()
}

/// Strict backward edges only. Equal-source ties and unknown times are handled
/// separately by automatic placement and remain visible as warnings in a
/// persisted plan.
pub(crate) fn backward_edge_count(
    operations: &[EditPlanOperation],
    placement: &BTreeMap<String, ChronologyPlacement>,
    policy: Option<&EditPlanStoryPolicy>,
) -> usize {
    let teasers: BTreeSet<&str> = policy
        .into_iter()
        .flat_map(|policy| policy.opening_teasers.iter())
        .map(|teaser| teaser.operation_id.as_str())
        .collect();
    let protected: BTreeSet<&str> = policy
        .into_iter()
        .flat_map(|policy| policy.protected_groups.iter())
        .flat_map(|group| group.cuts.iter())
        .map(|cut| cut.operation_id.as_str())
        .collect();
    let allowed = allowed_exception_edges(policy);
    operations
        .windows(2)
        .filter(|pair| {
            !teasers.contains(pair[0].operation_id.as_str())
                && !(protected.contains(pair[0].operation_id.as_str())
                    && protected.contains(pair[1].operation_id.as_str()))
                && !allowed.contains(&(pair[0].operation_id.clone(), pair[1].operation_id.clone()))
        })
        .filter(|pair| {
            match (
                placement.get(&pair[0].asset_id),
                placement.get(&pair[1].asset_id),
            ) {
                (Some(left), Some(right)) => {
                    match (left.trip_time_millis, right.trip_time_millis) {
                        (Some(left), Some(right)) => left > right,
                        _ => false,
                    }
                }
                _ => false,
            }
        })
        .count()
}

pub(crate) fn validate_policy_exactness(
    revision: &EditPlanRevision,
    facts: &ChronologyFacts,
) -> Result<()> {
    let Some(policy) = &revision.story_policy else {
        return Ok(());
    };
    policy.validate_shape()?;
    let by_id: BTreeMap<&str, &EditPlanOperation> = revision
        .operations
        .iter()
        .map(|operation| (operation.operation_id.as_str(), operation))
        .collect();
    if by_id.len() != revision.operations.len() {
        bail!("edit-plan story policy requires unique cut identities");
    }
    for cut in policy.cut_identities() {
        let operation = by_id
            .get(cut.operation_id.as_str())
            .ok_or_else(|| anyhow::anyhow!("story policy cut {} is missing", cut.operation_id))?;
        ensure!(
            cut.matches(operation),
            "story policy cut {} no longer matches its exact source range",
            cut.operation_id
        );
    }
    let tokens = policy
        .opening_teasers
        .iter()
        .map(|teaser| {
            (
                "opening teaser",
                teaser.operation_id.as_str(),
                teaser.chronology_dependency.as_str(),
                vec![teaser.asset_id.clone()],
            )
        })
        .chain(policy.protected_groups.iter().map(|group| {
            (
                "protected group",
                group.group_id.as_str(),
                group.chronology_dependency.as_str(),
                group.cuts.iter().map(|cut| cut.asset_id.clone()).collect(),
            )
        }))
        .chain(policy.exceptions.iter().map(|exception| {
            (
                "chronology exception",
                exception.exception_id.as_str(),
                exception.scope.chronology_dependency.as_str(),
                exception
                    .scope
                    .cuts
                    .iter()
                    .map(|cut| cut.asset_id.clone())
                    .collect(),
            )
        }));
    for (subject, id, stored, asset_ids) in tokens {
        ensure!(
            !stored.is_empty(),
            "{subject} {id} has no chronology dependency token"
        );
        ensure!(
            stored == chronology_dependency_token(facts, asset_ids)?,
            "{subject} {id} is stale because its chronology facts changed"
        );
    }
    Ok(())
}

/// Fill exact dependency tokens for a newly constructed candidate. The core
/// owns stamping, so callers cannot accidentally suppress a scope by leaving
/// the token blank forever.
pub(crate) fn stamp_policy_dependencies(
    policy: &mut EditPlanStoryPolicy,
    facts: &ChronologyFacts,
) -> Result<()> {
    policy.validate_shape()?;
    for teaser in &mut policy.opening_teasers {
        let token = chronology_dependency_token(facts, [teaser.asset_id.clone()])?;
        if teaser.chronology_dependency.is_empty() {
            teaser.chronology_dependency = token;
        } else {
            ensure!(
                teaser.chronology_dependency == token,
                "opening teaser {} is stale because its chronology facts changed",
                teaser.operation_id
            );
        }
    }
    for group in &mut policy.protected_groups {
        let token =
            chronology_dependency_token(facts, group.cuts.iter().map(|cut| cut.asset_id.clone()))?;
        if group.chronology_dependency.is_empty() {
            group.chronology_dependency = token;
        } else {
            ensure!(
                group.chronology_dependency == token,
                "protected group {} is stale because its chronology facts changed",
                group.group_id
            );
        }
    }
    for exception in &mut policy.exceptions {
        let token = chronology_dependency_token(
            facts,
            exception.scope.cuts.iter().map(|cut| cut.asset_id.clone()),
        )?;
        if exception.scope.chronology_dependency.is_empty() {
            exception.scope.chronology_dependency = token;
        } else {
            ensure!(
                exception.scope.chronology_dependency == token,
                "chronology exception {} is stale because its chronology facts changed",
                exception.exception_id
            );
        }
    }
    Ok(())
}

// Stable descriptive aliases for adapter and integration callers.
pub type StoryPolicy = EditPlanStoryPolicy;
pub type ChronologyPolicy = EditPlanStoryPolicy;
pub type EditPlanChronologyPolicy = EditPlanStoryPolicy;
pub type OpeningTeaser = StoryOpeningTeaser;
pub type StoryTeaser = StoryOpeningTeaser;
pub type ProtectedGroup = ProtectedStoryGroup;
pub type ChronologyException = StoryChronologyException;
pub type ScopedChronologyException = StoryChronologyException;
pub type ExceptionScope = StoryExceptionScope;
pub type CutIdentity = StoryCutIdentity;
