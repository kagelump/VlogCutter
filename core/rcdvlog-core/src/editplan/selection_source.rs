//! Bounded source/candidate snapshots. This layer reads existing evidence; it
//! never dispatches a provider, transcribes, or manufactures inspected frames.
use super::{selection::RECIPE, selection_model::*};
use crate::moments::MomentSpan;
use crate::{Catalog, EditPlanOperation, MomentCandidate};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub(super) struct Candidate {
    pub key: SelectionCandidateKey,
    pub operation_id: String,
    pub moment: MomentCandidate,
    pub evidence: SelectionEvidenceIdentity,
    pub role: String,
    pub activity: SelectionActivity,
    pub bucket: usize,
    pub duration_seconds: f64,
}
#[derive(Debug, Clone, Serialize)]
pub(super) struct SourceGuard {
    asset_id: String,
    fingerprint: String,
    duration_seconds: f64,
    rotation_degrees: u16,
    moment_identity: Option<String>,
    row_fingerprint: Option<String>,
}
pub(super) struct Prepared {
    pub candidates: Vec<Candidate>,
    pub guards: Vec<SourceGuard>,
    pub constraints_key: String,
    pub identity: String,
    pub constraint_scope: super::EditPlanRevision,
}
fn digest(value: &impl Serialize) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}
fn validate_span(span: MomentSpan, duration: f64) -> Result<()> {
    ensure!(
        span.start_ms < span.end_ms && f64::from(span.end_ms) / 1000.0 <= duration,
        "invalid input: moment interval is inverted, empty or outside source media"
    );
    Ok(())
}
fn contains(outer: MomentSpan, inner: MomentSpan) -> bool {
    outer.start_ms <= inner.start_ms && inner.end_ms <= outer.end_ms
}
fn validate_candidate(moment: &MomentCandidate, duration: f64) -> Result<()> {
    ensure!(
        !moment.id.is_empty()
            && moment.id.len() <= 255
            && moment.score.is_finite()
            && (0.0..=1.0).contains(&moment.score),
        "invalid input: candidate identity or score invalid"
    );
    for span in [moment.core, moment.payoff, moment.allowed_range] {
        validate_span(span, duration)?;
    }
    ensure!(
        contains(moment.allowed_range, moment.core) && contains(moment.core, moment.payoff),
        "invalid input: candidate core/payoff exceed declared source bounds"
    );
    for span in moment
        .setup
        .iter()
        .chain(moment.context.iter())
        .chain(moment.dialogue_boundaries.iter())
    {
        validate_span(*span, duration)?;
        ensure!(
            contains(moment.allowed_range, *span),
            "invalid input: candidate context/utterance exceeds allowed source range"
        );
    }
    ensure!(
        !moment.reason.trim().is_empty()
            && moment.reason.len() <= 4096
            && !moment.event.trim().is_empty()
            && moment.event.len() <= 255
            && !moment.role.trim().is_empty()
            && moment.role.len() <= 255
            && moment.evidence.len() <= 128
            && moment.uncertainty.len() <= 64
            && moment.dialogue_boundaries.len() <= 256,
        "invalid input: candidate labels or evidence exceed bounds"
    );
    ensure!(
        moment.uncertainty.iter().all(|s| s.len() <= 4096)
            && moment.evidence.iter().all(|e| !e.key.is_empty()
                && e.key.len() <= 1024
                && !e.kind.is_empty()
                && e.kind.len() <= 255),
        "invalid input: oversized candidate evidence/uncertainty"
    );
    Ok(())
}
fn inferred_activity(role: &str) -> SelectionActivity {
    match role.trim().to_lowercase().as_str() {
        "movement" | "travel" | "transition" | "transit" => SelectionActivity::Movement,
        "dialogue" | "speech" | "narration" | "interview" => SelectionActivity::Dialogue,
        "waiting" | "passive" | "standing around" => SelectionActivity::Passive,
        "action" | "payoff" | "reaction" | "wildlife" | "sighting" => SelectionActivity::Active,
        _ => SelectionActivity::Uncertain,
    }
}
impl Prepared {
    pub fn require_current(&self, connection: &Connection) -> Result<()> {
        for guard in &self.guards {
            let (fp, duration, rotation): (String, f64, u16) = connection.query_row(
                "SELECT fingerprint,duration_seconds,rotation_degrees FROM assets WHERE id=?1",
                [&guard.asset_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            ensure!(
                fp == guard.fingerprint
                    && duration == guard.duration_seconds
                    && rotation == guard.rotation_degrees,
                "selection source identity changed during planning"
            );
            if let Some(identity) = &guard.moment_identity {
                let body: Option<String> = connection
                    .query_row(
                        "SELECT body_json FROM moment_artifacts WHERE asset_id=?1 AND identity=?2",
                        params![guard.asset_id, identity],
                        |r| r.get(0),
                    )
                    .optional()?;
                ensure!(
                    body.is_some_and(|body| Some(
                        blake3::hash(body.as_bytes()).to_hex().to_string()
                    ) == guard.row_fingerprint),
                    "moment evidence changed during planning"
                );
            }
        }
        Ok(())
    }
}
pub(super) fn prepare(catalog: &Catalog, request: &SequenceSelectionRequest) -> Result<Prepared> {
    super::selection_logic::validate_input(&request.input)?;
    catalog.require_project(&request.project_id)?;
    let base = request
        .expected_base_revision_id
        .as_ref()
        .map(|id| catalog.edit_plan_revision(id))
        .transpose()?;
    if let Some(base) = &base {
        ensure!(
            base.project_id == request.project_id,
            "invalid input: selection base belongs to another project"
        );
    }
    let mut candidates = Vec::new();
    let mut guards = Vec::new();
    let mut keys = BTreeSet::new();
    let mut all_sources = BTreeSet::new();
    let mut rows = BTreeMap::new();
    for sequence in &request.input.sequences {
        let mut assets = BTreeSet::new();
        for (source_index, source) in sequence.sources.iter().enumerate() {
            let asset_id = match source {
                SelectionMomentSource::Current { asset_id, .. }
                | SelectionMomentSource::Declared { asset_id, .. } => asset_id,
            };
            ensure!(
                assets.insert(asset_id),
                "invalid input: list an asset once per sequence; put its disjoint moments in the same source entry"
            );
            let asset = catalog.get_asset(asset_id)?;
            ensure!(
                asset.project_id == request.project_id
                    && asset.duration_seconds.is_finite()
                    && asset.duration_seconds > 0.0,
                "invalid input: selection asset belongs to another project or has unknown duration"
            );
            all_sources.insert(asset_id.clone());
            let (moments, mut provenance, identity, row_fingerprint) = match source {
                SelectionMomentSource::Current {
                    expected_moment_identity,
                    candidate_ids,
                    ..
                } => {
                    let cache_key = (asset_id.clone(), expected_moment_identity.clone());
                    let (artifact, current, row_hash) = if let Some(row) = rows.get(&cache_key) {
                        Clone::clone(row)
                    } else {
                        let row = catalog
                            .selection_moment_artifact(asset_id, expected_moment_identity)?
                            .ok_or_else(|| SequenceSelectionError::EvidenceUnavailable {
                                asset_id: asset_id.clone(),
                            })?;
                        rows.insert(cache_key, row.clone());
                        row
                    };
                    ensure!(
                        current,
                        SequenceSelectionError::StaleEvidence {
                            asset_id: asset_id.clone()
                        }
                    );
                    ensure!(
                        artifact.status != crate::moments::MomentStatus::Unavailable,
                        SequenceSelectionError::EvidenceUnavailable {
                            asset_id: asset_id.clone()
                        }
                    );
                    let requested = candidate_ids.iter().collect::<BTreeSet<_>>();
                    ensure!(
                        requested.len() == candidate_ids.len()
                            && candidate_ids
                                .iter()
                                .all(|id| artifact.candidates.iter().any(|c| &c.id == id)),
                        "invalid input: unknown/duplicate requested moment ID"
                    );
                    let provenance=SelectionEvidenceIdentity{source_fingerprint:asset.fingerprint.clone(),moment_identity:artifact.identity.clone(),candidate_fingerprint:String::new(),origin:"canonicalMoments".into(),
                        provider:Some(artifact.provenance.provider),requested_model:Some(artifact.provenance.requested_model),actual_model:Some(artifact.provenance.actual_model),
                        prompt_version:Some(artifact.provenance.prompt_version),schema_version:Some(artifact.provenance.schema_version),recipe_version:artifact.provenance.recipe_version,
                        policy_fingerprint:artifact.provenance.configuration_hash,representation_fingerprint:Some(artifact.provenance.representation_fingerprint),
                        sampling_limits:vec!["Moment evidence is a locator/refinement proposal, not continuous frame, word or editorial verification.".into(),
                            format!("{} recorded coverage segments; candidate refinement does not restore discarded representation frames.",artifact.coverage.len())]};
                    let moments = artifact
                        .candidates
                        .into_iter()
                        .filter(|c| requested.is_empty() || requested.contains(&c.id))
                        .collect();
                    (
                        moments,
                        provenance,
                        Some(expected_moment_identity.clone()),
                        Some(row_hash),
                    )
                }
                SelectionMomentSource::Declared {
                    expected_source_fingerprint,
                    rationale,
                    candidates,
                    ..
                } => {
                    ensure!(
                        asset.fingerprint == *expected_source_fingerprint,
                        SequenceSelectionError::StaleEvidence {
                            asset_id: asset_id.clone()
                        }
                    );
                    let identity =
                        digest(&(asset_id, expected_source_fingerprint, candidates, rationale))?;
                    let provenance=SelectionEvidenceIdentity{source_fingerprint:asset.fingerprint.clone(),moment_identity:identity,candidate_fingerprint:String::new(),origin:"declaredLocators".into(),
                        provider:None,requested_model:None,actual_model:None,prompt_version:None,schema_version:None,recipe_version:"human-declared-locators.v1".into(),policy_fingerprint:digest(rationale)?,representation_fingerprint:None,
                        sampling_limits:vec!["Authored candidate locators are not provider receipts, inspected source frames or verified word boundaries.".into()]};
                    (candidates.clone(), provenance, None, None)
                }
            };
            guards.push(SourceGuard {
                asset_id: asset.id.clone(),
                fingerprint: asset.fingerprint.clone(),
                duration_seconds: asset.duration_seconds,
                rotation_degrees: asset.rotation_degrees,
                moment_identity: identity,
                row_fingerprint,
            });
            let mut source_ids = BTreeSet::new();
            for moment in moments {
                validate_candidate(&moment, asset.duration_seconds)?;
                ensure!(
                    source_ids.insert(moment.id.clone()),
                    "invalid input: duplicate moment ID within a source"
                );
                let key = SelectionCandidateKey {
                    sequence_id: sequence.sequence_id.clone(),
                    asset_id: asset.id.clone(),
                    candidate_id: moment.id.clone(),
                };
                ensure!(
                    keys.insert(key.clone()),
                    "invalid input: duplicate selection candidate"
                );
                provenance.candidate_fingerprint = digest(&moment)?;
                let operation_id = format!(
                    "select_{}",
                    &digest(&(
                        "candidate-cut.v1",
                        &key,
                        &provenance.moment_identity,
                        &provenance.candidate_fingerprint
                    ))?[..32]
                );
                let claim = sequence.role_claims.iter().find(|c| c.key == key);
                let role = claim.map_or_else(
                    || moment.role.trim().to_lowercase(),
                    |c| c.role.trim().to_lowercase(),
                );
                let activity = claim.map_or_else(|| inferred_activity(&role), |c| c.activity);
                let midpoint =
                    (f64::from(moment.payoff.start_ms) + f64::from(moment.payoff.end_ms)) / 2000.0;
                let fraction = (source_index as f64 + midpoint / asset.duration_seconds)
                    / sequence.sources.len().max(1) as f64;
                let bucket = ((fraction * 3.0).floor() as usize).min(2);
                candidates.push(Candidate {
                    key,
                    operation_id,
                    moment,
                    evidence: provenance.clone(),
                    role,
                    activity,
                    bucket,
                    duration_seconds: asset.duration_seconds,
                });
                ensure!(
                    candidates.len() <= super::selection::MAX_CANDIDATES,
                    "invalid input: at most 10000 candidate moments per selection"
                );
            }
        }
    }
    let mut scope = super::EditPlanRevision {
        revision_id: "selection-input-scope".into(),
        project_id: request.project_id.clone(),
        script_revision_id: None,
        parent_revision_id: request.expected_base_revision_id.clone(),
        active: false,
        operations: all_sources
            .iter()
            .enumerate()
            .map(|(position, asset)| EditPlanOperation {
                operation_id: format!("scope-{position}"),
                position: position as u32,
                asset_id: asset.clone(),
                source_in_seconds: 0.0,
                source_out_seconds: 1.0,
                rationale: String::new(),
                audio: None,
            })
            .collect(),
        created_at: String::new(),
    };
    // Retained choices can reference audio from another asset; use their full
    // source identities in the final policy/currentness key too.
    for decision in super::feedback::hard_choices(&catalog.connection()?, &request.project_id)? {
        let retained = decision
            .retained_target
            .context("retained choice has no target")?;
        let mut retained_sources = vec![retained.cut.asset_id.clone()];
        if let Some(super::EditPlanAudio::Source(audio)) = &retained.cut.audio {
            retained_sources.push(audio.asset_id.clone());
        }
        for asset_id in retained_sources {
            if !guards.iter().any(|guard| guard.asset_id == asset_id) {
                let asset = catalog.get_asset(&asset_id)?;
                guards.push(SourceGuard {
                    asset_id,
                    fingerprint: asset.fingerprint,
                    duration_seconds: asset.duration_seconds,
                    rotation_degrees: asset.rotation_degrees,
                    moment_identity: None,
                    row_fingerprint: None,
                });
            }
        }
        scope.operations.push(EditPlanOperation {
            operation_id: retained.cut.operation_id,
            position: scope.operations.len() as u32,
            asset_id: retained.cut.asset_id,
            source_in_seconds: retained.cut.source_in_seconds,
            source_out_seconds: retained.cut.source_out_seconds,
            rationale: String::new(),
            audio: retained.cut.audio,
        });
    }
    if let Some(base) = &base {
        for cut in &base.operations {
            if let Some(super::EditPlanAudio::Source(audio)) = &cut.audio {
                let asset = catalog.get_asset(&audio.asset_id)?;
                ensure!(
                    asset.project_id == request.project_id,
                    "invalid input: base sound source belongs to another project"
                );
                if !guards.iter().any(|g| g.asset_id == asset.id) {
                    guards.push(SourceGuard {
                        asset_id: asset.id.clone(),
                        fingerprint: asset.fingerprint,
                        duration_seconds: asset.duration_seconds,
                        rotation_degrees: asset.rotation_degrees,
                        moment_identity: None,
                        row_fingerprint: None,
                    });
                }
                scope.operations.push(cut.clone());
            }
        }
    }
    let constraints_key = super::ranges::constraints_key(&catalog.connection()?, &scope)?;
    let identity = digest(&(
        RECIPE,
        &request.input,
        &request.expected_base_revision_id,
        &guards,
        &constraints_key,
        candidates
            .iter()
            .map(|c| (&c.key, &c.evidence))
            .collect::<Vec<_>>(),
    ))?;
    Ok(Prepared {
        candidates,
        guards,
        constraints_key,
        identity,
        constraint_scope: scope,
    })
}
