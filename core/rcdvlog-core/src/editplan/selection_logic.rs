//! Pure chapter selection rules. Scores rank proposals; they never certify story quality.
use super::selection_model::*;
use anyhow::{Context, Result, ensure};

pub(super) fn validate_input(input: &SequenceSelectionInput) -> Result<()> {
    ensure!(
        input.version == 1 && !input.goal.trim().is_empty() && input.goal.len() <= 4096,
        "invalid input: version-1 human sequence intent and goal required"
    );
    ensure!(
        !input.sequences.is_empty() && input.sequences.len() <= 64,
        "invalid input: select 1-64 sequences"
    );
    ensure!(
        input.minimum_score.is_finite()
            && (0.0..=1.0).contains(&input.minimum_score)
            && input.repetition_penalty.is_finite()
            && (0.0..=10.0).contains(&input.repetition_penalty)
            && input.coverage_bonus.is_finite()
            && (0.0..=10.0).contains(&input.coverage_bonus),
        "invalid input: selection utility weights are out of bounds"
    );
    ensure!(
        input.include.len() <= 1000
            && input.exclude.len() <= 1000
            && input.counts.len() <= 256
            && input.relationships.len() <= 256,
        "invalid input: selection constraints exceed their bounds"
    );
    let budget = |b: Option<f64>| b.is_none_or(|v| v.is_finite() && v > 0.0 && v <= 21600.0);
    ensure!(
        budget(input.budget_seconds),
        "invalid input: runtime preference must be positive and at most six hours"
    );
    let mut sequences = std::collections::BTreeSet::new();
    let mut sources = 0;
    for sequence in &input.sequences {
        ensure!(
            !sequence.sequence_id.is_empty()
                && sequence.sequence_id.len() <= 255
                && sequences.insert(&sequence.sequence_id)
                && !sequence.goal.trim().is_empty()
                && sequence.goal.len() <= 4096,
            "invalid input: sequence identity/goal is invalid or duplicated"
        );
        let p = &sequence.pacing;
        ensure!(
            [
                p.preferred_min_seconds,
                p.preferred_max_seconds,
                p.long_review_seconds,
                p.preferred_upper_seconds
            ]
            .iter()
            .all(|v| v.is_finite() && *v > 0.0)
                && p.preferred_min_seconds <= p.preferred_max_seconds
                && p.preferred_max_seconds <= p.preferred_upper_seconds
                && p.preferred_upper_seconds <= 21600.0
                && p.preferred_long_holds <= 1000,
            "invalid input: pacing preferences are not finite, positive and ordered"
        );
        ensure!(
            budget(sequence.budget_seconds)
                && sequence.role_claims.len() <= 10000
                && sequence.retained_choices.len() <= 1024
                && sequence.hero_candidates.len() <= 1000,
            "invalid input: sequence preferences/claims exceed bounds"
        );
        sources += sequence.sources.len();
        for source in &sequence.sources {
            match source {
                SelectionMomentSource::Current {
                    asset_id,
                    expected_moment_identity,
                    candidate_ids,
                } => ensure!(
                    !asset_id.is_empty()
                        && !expected_moment_identity.is_empty()
                        && candidate_ids.len() <= 10000,
                    "invalid input: missing current moment identity"
                ),
                SelectionMomentSource::Declared {
                    asset_id,
                    expected_source_fingerprint,
                    rationale,
                    candidates,
                } => ensure!(
                    !asset_id.is_empty()
                        && !expected_source_fingerprint.is_empty()
                        && !rationale.trim().is_empty()
                        && rationale.len() <= 4096
                        && candidates.len() <= 10000,
                    "invalid input: declared locators require source identity and rationale"
                ),
            }
        }
    }
    ensure!(
        sources <= 4096 && serde_json::to_vec(input)?.len() <= 8 * 1024 * 1024,
        "invalid input: chapter selection source/input bound exceeded"
    );
    for constraint in &input.counts {
        ensure!(
            !constraint.role.trim().is_empty()
                && constraint.role.len() <= 255
                && constraint.minimum <= constraint.maximum
                && constraint.maximum <= 10000
                && !constraint.rationale.trim().is_empty(),
            "invalid input: invalid role-count constraint"
        );
    }
    Ok(())
}

use super::{
    EditPlanOperation, EditPlanRevision, SourceTimeRange, TrimMedia,
    selection_source::{Candidate, Prepared},
};
use crate::{Catalog, moments::MomentSpan};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
struct Choice {
    operation: EditPlanOperation,
    assignment: SelectedMomentAssignment,
    score: f64,
    bucket: usize,
    mandatory: bool,
}
fn conflict(
    code: &str,
    blocking: bool,
    sequence: Option<&str>,
    candidate: Option<&SelectionCandidateKey>,
    operation: Option<&str>,
    message: String,
) -> SelectionConflict {
    SelectionConflict {
        candidate: candidate.cloned(),
        asset_id: candidate.map(|c| c.asset_id.clone()),
        code: code.into(),
        blocking,
        sequence_id: sequence.map(str::to_owned),
        operation_id: operation.map(str::to_owned),
        message,
    }
}
fn complete_span(candidate: &Candidate) -> MomentSpan {
    let m = &candidate.moment;
    let mut span = m.core;
    if let Some(setup) = m.setup {
        span.start_ms = span.start_ms.min(setup.start_ms);
        span.end_ms = span.end_ms.max(setup.end_ms);
    }
    span.start_ms = span.start_ms.min(m.payoff.start_ms);
    span.end_ms = span.end_ms.max(m.payoff.end_ms);
    loop {
        let previous = span;
        for utterance in &m.dialogue_boundaries {
            if utterance.start_ms < span.end_ms && span.start_ms < utterance.end_ms {
                span.start_ms = span.start_ms.min(utterance.start_ms);
                span.end_ms = span.end_ms.max(utterance.end_ms);
            }
        }
        if previous == span {
            break;
        }
    }
    span
}
fn candidate_choice(
    candidate: &Candidate,
    intent: &SequenceIntent,
    base: Option<&EditPlanRevision>,
) -> Result<Choice> {
    let complete = complete_span(candidate);
    let mut chosen = complete;
    let mut fragment = false;
    let speech = candidate.activity == SelectionActivity::Dialogue
        || !candidate.moment.dialogue_boundaries.is_empty();
    let hero = intent.hero_candidates.contains(&candidate.key);
    if intent.pacing.allow_montage_fragments
        && candidate.activity == SelectionActivity::Movement
        && !speech
        && !hero
        && f64::from(complete.end_ms - complete.start_ms) / 1000.0
            > intent.pacing.preferred_max_seconds
    {
        let payoff = candidate.moment.payoff;
        let payoff_seconds = f64::from(payoff.end_ms - payoff.start_ms) / 1000.0;
        let duration = payoff_seconds
            .max(intent.pacing.preferred_min_seconds)
            .min(intent.pacing.preferred_max_seconds.max(payoff_seconds));
        let length = (duration * 1000.0).round() as u32;
        let allowance = candidate.moment.allowed_range;
        let middle = u64::from(payoff.start_ms) + u64::from(payoff.end_ms - payoff.start_ms) / 2;
        let start = middle
            .saturating_sub(u64::from(length) / 2)
            .max(u64::from(allowance.start_ms));
        let end = (start + u64::from(length)).min(u64::from(allowance.end_ms));
        let start = end
            .saturating_sub(u64::from(length))
            .max(u64::from(allowance.start_ms));
        chosen = MomentSpan {
            start_ms: start as u32,
            end_ms: end as u32,
        };
        fragment = true;
    }
    ensure!(
        chosen.start_ms < chosen.end_ms
            && chosen.start_ms >= candidate.moment.allowed_range.start_ms
            && chosen.end_ms <= candidate.moment.allowed_range.end_ms,
        "candidate completeness exceeds allowed evidence range"
    );
    let source_in = f64::from(chosen.start_ms) / 1000.0;
    let source_out = f64::from(chosen.end_ms) / 1000.0;
    ensure!(
        source_out <= candidate.duration_seconds,
        "candidate completeness exceeds exact source duration"
    );
    let mut uncertainty = candidate.moment.uncertainty.clone();
    uncertainty.push("Moment/utterance timings remain proposals; inspect exact endpoints and listen before accepting them.".into());
    if fragment {
        uncertainty.push("This is an explicitly permitted movement montage fragment, not a claim to retain the complete action; linked source sound needs audible review.".into());
    }
    if speech && candidate.moment.dialogue_boundaries.is_empty() {
        uncertainty.push(
            "No timed utterance evidence is available; complete words have not been established."
                .into(),
        );
    }
    let rationale = format!(
        "{} — {}. {}{}",
        intent.goal,
        candidate.moment.reason,
        if fragment {
            "Deliberate movement fragment anchored on the supplied payoff, not the beginning of the source. "
        } else {
            "Retain the supplied setup/core/payoff and overlapping utterance envelope. "
        },
        if hero {
            "Explicitly selected hero; its duration is earned only by its supplied content and remains subject to playback review."
        } else {
            ""
        }
    );
    let audio = base
        .and_then(|p| {
            p.operations
                .iter()
                .find(|c| c.operation_id == candidate.operation_id)
        })
        .and_then(|c| c.audio.clone());
    let operation = EditPlanOperation {
        operation_id: candidate.operation_id.clone(),
        position: 0,
        asset_id: candidate.key.asset_id.clone(),
        source_in_seconds: source_in,
        source_out_seconds: source_out,
        rationale: rationale.clone(),
        audio,
    };
    let assignment = SelectedMomentAssignment {
        operation_id: operation.operation_id.clone(),
        sequence_id: intent.sequence_id.clone(),
        candidate: Some(candidate.key.clone()),
        retained_note_id: None,
        asset_id: operation.asset_id.clone(),
        role: candidate.role.clone(),
        activity: candidate.activity,
        event: candidate.moment.event.clone(),
        coverage_bucket: candidate.bucket as u8,
        original_core: candidate.moment.core,
        complete_span: complete,
        chosen_source_in_seconds: source_in,
        chosen_source_out_seconds: source_out,
        montage_fragment: fragment,
        rationale,
        evidence: candidate.evidence.clone(),
        uncertainty,
    };
    Ok(Choice {
        operation,
        assignment,
        score: candidate.moment.score,
        bucket: candidate.bucket,
        mandatory: false,
    })
}
fn seconds(choice: &Choice) -> f64 {
    choice.operation.source_out_seconds - choice.operation.source_in_seconds
}
fn same_source_intersects(a: &Choice, b: &Choice, adjacent: bool) -> bool {
    let pad = if adjacent { 1001.0 / 30000.0 } else { 0.0 };
    a.operation.asset_id == b.operation.asset_id
        && a.operation.source_in_seconds < b.operation.source_out_seconds + pad
        && b.operation.source_in_seconds < a.operation.source_out_seconds + pad
}
fn clean_choice(catalog: &Catalog, choice: &Choice) -> Result<bool> {
    let asset = catalog.get_asset(&choice.operation.asset_id)?;
    let records = super::ranges::load_for_asset(&catalog.connection()?, &asset.id)?;
    let (ranges, conflicts) = super::ranges::feasible_ranges(
        &records,
        &asset.fingerprint,
        asset.duration_seconds,
        TrimMedia::Picture,
    );
    let span = SourceTimeRange {
        source_in_seconds: choice.operation.source_in_seconds,
        source_out_seconds: choice.operation.source_out_seconds,
    };
    if !conflicts.is_empty() || !ranges.iter().any(|r| super::ranges::contains(*r, span)) {
        return Ok(false);
    }
    // Exact audio/presence/sync validation remains with the ordinary validator.
    // Respect explicit audio bounds conservatively for a linked proposal.
    if choice.operation.audio.is_none()
        && records.iter().any(|r| {
            r.active && matches!(r.media, super::RangeMedia::Audio | super::RangeMedia::Both)
        })
    {
        let (audio, conflicts) = super::ranges::feasible_ranges(
            &records,
            &asset.fingerprint,
            asset.duration_seconds,
            TrimMedia::Audio,
        );
        if !conflicts.is_empty() || !audio.iter().any(|r| super::ranges::contains(*r, span)) {
            return Ok(false);
        }
    }
    Ok(true)
}
fn normalized(value: &str) -> String {
    value.trim().to_lowercase()
}
fn counts_allow(
    input: &SequenceSelectionInput,
    selected: &BTreeSet<usize>,
    group: &BTreeSet<usize>,
    choices: &[Choice],
) -> bool {
    input.counts.iter().filter(|c| c.hard).all(|count| {
        selected
            .union(group)
            .filter(|&&index| {
                choices[index].assignment.role == normalized(&count.role)
                    && count
                        .sequence_id
                        .as_ref()
                        .is_none_or(|s| s == &choices[index].assignment.sequence_id)
            })
            .count()
            <= count.maximum as usize
    })
}
fn within_budgets(
    input: &SequenceSelectionInput,
    selected: &BTreeSet<usize>,
    group: &BTreeSet<usize>,
    choices: &[Choice],
) -> bool {
    let total = selected
        .union(group)
        .map(|&i| seconds(&choices[i]))
        .sum::<f64>();
    if total > input.budget_seconds.unwrap_or(21600.0).min(21600.0) + 1e-9 {
        return false;
    }
    input.sequences.iter().all(|sequence| {
        sequence.budget_seconds.is_none_or(|budget| {
            selected
                .union(group)
                .filter(|&&i| choices[i].assignment.sequence_id == sequence.sequence_id)
                .map(|&i| seconds(&choices[i]))
                .sum::<f64>()
                <= budget + 1e-9
        })
    })
}
fn expand_group(
    start: usize,
    input: &SequenceSelectionInput,
    index: &BTreeMap<SelectionCandidateKey, usize>,
) -> BTreeSet<usize> {
    let mut group = BTreeSet::from([start]);
    loop {
        let before = group.len();
        for edge in input
            .relationships
            .iter()
            .filter(|e| e.hard && e.keep_together)
        {
            if let (Some(&a), Some(&b)) = (index.get(&edge.before), index.get(&edge.after))
                && (group.contains(&a) || group.contains(&b))
            {
                group.insert(a);
                group.insert(b);
            }
        }
        if before == group.len() {
            break;
        }
    }
    group
}
fn utility(
    input: &SequenceSelectionInput,
    selected: &BTreeSet<usize>,
    group: &BTreeSet<usize>,
    choices: &[Choice],
) -> f64 {
    let mut score = 0.0;
    let mut seen = selected.clone();
    for &index in group.difference(selected) {
        let choice = &choices[index];
        let repeats = seen
            .iter()
            .filter(|&&i| {
                choices[i].assignment.sequence_id == choice.assignment.sequence_id
                    && normalized(&choices[i].assignment.event)
                        == normalized(&choice.assignment.event)
            })
            .count();
        let source_repeats = seen
            .iter()
            .filter(|&&i| {
                choices[i].assignment.sequence_id == choice.assignment.sequence_id
                    && choices[i].operation.asset_id == choice.operation.asset_id
            })
            .count();
        let bucket_new = !seen.iter().any(|&i| {
            choices[i].assignment.sequence_id == choice.assignment.sequence_id
                && choices[i].bucket == choice.bucket
        });
        let role_new = !seen.iter().any(|&i| {
            choices[i].assignment.sequence_id == choice.assignment.sequence_id
                && choices[i].assignment.role == choice.assignment.role
        });
        let activity = match choice.assignment.activity {
            SelectionActivity::Passive => -1.0,
            SelectionActivity::Active => 0.15,
            SelectionActivity::Movement => 0.05,
            _ => 0.0,
        };
        score += choice.score - input.minimum_score + activity
            - input.repetition_penalty * (repeats as f64 + source_repeats as f64 * 0.25)
            + if bucket_new {
                input.coverage_bonus
            } else {
                0.0
            }
            + if role_new { 0.05 } else { 0.0 };
        seen.insert(index);
    }
    score
}

pub(super) fn choose(
    catalog: &Catalog,
    request: &SequenceSelectionRequest,
    prepared: &Prepared,
) -> Result<SelectionProposal> {
    let input = &request.input;
    let base = request
        .expected_base_revision_id
        .as_ref()
        .map(|id| catalog.edit_plan_revision(id))
        .transpose()?;
    let inherited = base
        .as_ref()
        .map(|b| super::selection::load_metadata(&catalog.connection()?, &b.revision_id))
        .transpose()?
        .flatten();
    let mut choices = Vec::new();
    let mut conflicts = Vec::new();
    for candidate in &prepared.candidates {
        let intent = input
            .sequences
            .iter()
            .find(|s| s.sequence_id == candidate.key.sequence_id)
            .expect("prepared sequence");
        choices.push(candidate_choice(candidate, intent, base.as_ref())?);
    }
    let index = choices
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.assignment.candidate.clone().map(|k| (k, i)))
        .collect::<BTreeMap<_, _>>();
    // Unknown references and contradictory claims are explicit conflicts. No
    // guessed source, silently dropped mandatory shot or chronology inference.
    for key in input
        .include
        .iter()
        .chain(input.exclude.iter())
        .chain(input.sequences.iter().flat_map(|s| {
            s.hero_candidates
                .iter()
                .chain(s.place_context.iter().map(|p| &p.establishing_candidate))
        }))
        .chain(
            input
                .relationships
                .iter()
                .flat_map(|e| [&e.before, &e.after]),
        )
    {
        if !index.contains_key(key) {
            conflicts.push(conflict(
                "unknown_candidate",
                true,
                Some(&key.sequence_id),
                Some(key),
                None,
                "Referenced candidate is outside this bounded selection evidence.".into(),
            ));
        }
    }
    let mut excluded = BTreeSet::new();
    for key in &input.exclude {
        if let Some(&i) = index.get(key) {
            excluded.insert(i);
        }
    }
    for sequence in &input.sequences {
        let mut seen = BTreeSet::new();
        for claim in &sequence.role_claims {
            ensure!(
                claim.key.sequence_id == sequence.sequence_id
                    && index.contains_key(&claim.key)
                    && seen.insert(&claim.key)
                    && !claim.role.trim().is_empty()
                    && claim.role.len() <= 255
                    && !claim.rationale.trim().is_empty()
                    && claim.rationale.len() <= 4096,
                "invalid input: role claim must uniquely reference a candidate in its sequence with a rationale"
            );
        }
        if let Some(context) = &sequence.place_context {
            ensure!(
                context.establishing_candidate.sequence_id == sequence.sequence_id
                    && !context.description.trim().is_empty()
                    && context.description.len() <= 4096,
                "invalid input: place context requires a description and candidate in its sequence"
            );
        }
    }
    for count in &input.counts {
        ensure!(
            count
                .sequence_id
                .as_ref()
                .is_none_or(|s| input.sequences.iter().any(|q| &q.sequence_id == s)),
            "invalid input: count refers to unknown sequence"
        );
    }
    let mut mandatory = BTreeSet::new();
    for key in input
        .include
        .iter()
        .chain(input.sequences.iter().flat_map(|s| {
            s.hero_candidates
                .iter()
                .chain(s.place_context.iter().map(|p| &p.establishing_candidate))
        }))
    {
        if let Some(&i) = index.get(key) {
            mandatory.extend(expand_group(i, input, &index));
        }
    }
    // A retained choice is read from the feedback owner, never recreated from a
    // stale selector snapshot. Routing metadata is not a second hard lock.
    for decision in super::feedback::hard_choices(&catalog.connection()?, &request.project_id)? {
        let retained = decision
            .retained_target
            .context("retained choice lacks target")?;
        let explicit = input
            .sequences
            .iter()
            .flat_map(|s| {
                s.retained_choices
                    .iter()
                    .filter(|a| a.note_id == decision.note_id)
                    .map(move |a| (s, a))
            })
            .collect::<Vec<_>>();
        ensure!(
            explicit.len() <= 1,
            "invalid input: retained note assigned to multiple sequences"
        );
        let previous = inherited.as_ref().and_then(|m| {
            m.assignments
                .iter()
                .find(|a| a.operation_id == retained.cut.operation_id)
        });
        let routing = explicit
            .first()
            .map(|(s, a)| {
                (
                    s.sequence_id.clone(),
                    a.role.trim().to_lowercase(),
                    a.activity,
                    a.rationale.clone(),
                )
            })
            .or_else(|| {
                previous
                    .filter(|a| {
                        input
                            .sequences
                            .iter()
                            .any(|s| s.sequence_id == a.sequence_id)
                    })
                    .map(|a| {
                        (
                            a.sequence_id.clone(),
                            a.role.clone(),
                            a.activity,
                            a.rationale.clone(),
                        )
                    })
            });
        let Some((sequence, role, activity, rationale)) = routing else {
            conflicts.push(conflict("retained_choice_needs_sequence",true,None,None,Some(&retained.cut.operation_id),format!("Retained human choice from note {} needs an explicit sequence assignment; supersede it in feedback to release it.",decision.note_id)));
            continue;
        };
        ensure!(
            !role.is_empty()
                && role.len() <= 255
                && !rationale.trim().is_empty()
                && rationale.len() <= 4096,
            "invalid input: retained choice requires bounded role/rationale"
        );
        let existing = choices
            .iter()
            .position(|c| c.operation.operation_id == retained.cut.operation_id);
        let audio =
            if decision.hard_choice == Some(super::feedback_model::ReviewHardChoice::KeepPicture) {
                base.as_ref()
                    .and_then(|b| {
                        b.operations
                            .iter()
                            .find(|c| c.operation_id == retained.cut.operation_id)
                    })
                    .map(|c| c.audio.clone())
                    .unwrap_or_else(|| retained.cut.audio.clone())
            } else {
                retained.cut.audio.clone()
            };
        let operation = EditPlanOperation {
            operation_id: retained.cut.operation_id.clone(),
            position: 0,
            asset_id: retained.cut.asset_id.clone(),
            source_in_seconds: retained.cut.source_in_seconds,
            source_out_seconds: retained.cut.source_out_seconds,
            rationale: rationale.clone(),
            audio,
        };
        if let Some(i) = existing {
            if choices[i].mandatory
                && (choices[i].assignment.sequence_id != sequence
                    || choices[i].assignment.role != role)
            {
                conflicts.push(conflict("retained_choice_routing_conflict",true,Some(&sequence),None,Some(&operation.operation_id),"Two retained notes assign the same physical cut to conflicting sequence/role choices.".into()));
            }
            choices[i].operation = operation;
            choices[i].assignment.sequence_id = sequence;
            choices[i].assignment.role = role;
            choices[i].assignment.activity = activity;
            choices[i].assignment.retained_note_id = Some(decision.note_id);
            choices[i].mandatory = true;
            mandatory.insert(i);
        } else {
            let asset = catalog.get_asset(&operation.asset_id)?;
            let span = MomentSpan {
                start_ms: (operation.source_in_seconds * 1000.0).floor() as u32,
                end_ms: (operation.source_out_seconds * 1000.0).ceil() as u32,
            };
            let evidence=previous.map(|a|a.evidence.clone()).unwrap_or(SelectionEvidenceIdentity{source_fingerprint:asset.fingerprint,moment_identity:decision.decision_id.clone(),candidate_fingerprint:blake3::hash(&serde_json::to_vec(&retained.cut)?).to_hex().to_string(),origin:"retainedHumanChoice".into(),provider:None,requested_model:None,actual_model:None,prompt_version:None,schema_version:None,recipe_version:"human-feedback.v1".into(),policy_fingerprint:decision.decision_id,representation_fingerprint:None,sampling_limits:vec!["Explicit human retained source interval; no new model or frame evidence inferred.".into()]});
            let assignment = SelectedMomentAssignment {
                operation_id: operation.operation_id.clone(),
                sequence_id: sequence,
                candidate: None,
                retained_note_id: Some(decision.note_id),
                asset_id: operation.asset_id.clone(),
                role,
                activity,
                event: previous.map_or_else(|| "retained human choice".into(), |a| a.event.clone()),
                coverage_bucket: previous.map_or(1, |a| a.coverage_bucket),
                original_core: span,
                complete_span: span,
                chosen_source_in_seconds: operation.source_in_seconds,
                chosen_source_out_seconds: operation.source_out_seconds,
                montage_fragment: false,
                rationale,
                evidence,
                uncertainty: vec![
                    "Retained choice is preserved, not independently certified by this selector."
                        .into(),
                ],
            };
            mandatory.insert(choices.len());
            choices.push(Choice {
                operation,
                assignment,
                score: 1.0,
                bucket: 1,
                mandatory: true,
            });
        }
    }
    for &i in &mandatory {
        choices[i].mandatory = true;
    }
    let clean = choices
        .iter()
        .map(|c| clean_choice(catalog, c))
        .collect::<Result<Vec<_>>>()?;
    let mut selected = mandatory.clone();
    for &i in &mandatory {
        if excluded.contains(&i) || !clean[i] {
            conflicts.push(conflict("required_choice_conflict",true,Some(&choices[i].assignment.sequence_id),choices[i].assignment.candidate.as_ref(),Some(&choices[i].operation.operation_id),"Required/retained choice conflicts with an exclusion or current human source constraints; it has not been shortened or silently dropped.".into()));
        }
    }
    // Satisfy hard minima by strongest available complete groups first; report
    // impossibility rather than filling with unreferenced material.
    let mut evaluations = 0usize;
    for count in input.counts.iter().filter(|c| c.hard && c.minimum > 0) {
        if evaluations > 250_000 {
            break;
        }
        loop {
            let matches = |i: usize| {
                choices[i].assignment.role == normalized(&count.role)
                    && count
                        .sequence_id
                        .as_ref()
                        .is_none_or(|s| s == &choices[i].assignment.sequence_id)
            };
            if selected.iter().filter(|&&i| matches(i)).count() >= count.minimum as usize {
                break;
            }
            let mut options = choices
                .iter()
                .enumerate()
                .filter(|(i, _)| {
                    evaluations += 1;
                    evaluations <= 250_000 && !selected.contains(i) && matches(*i)
                })
                .map(|(i, _)| expand_group(i, input, &index))
                .filter(|g| {
                    g.iter().all(|i| !excluded.contains(i) && clean[*i])
                        && counts_allow(input, &selected, g, &choices)
                })
                .collect::<Vec<_>>();
            options.sort_by(|a, b| {
                utility(input, &selected, b, &choices)
                    .total_cmp(&utility(input, &selected, a, &choices))
                    .then_with(|| a.cmp(b))
            });
            let Some(group) = options.first() else {
                break;
            };
            selected.extend(group);
            for &i in group {
                choices[i].mandatory = true;
            }
        }
    }
    loop {
        let mut best: Option<(f64, BTreeSet<usize>)> = None;
        for (i, choice) in choices.iter().enumerate() {
            evaluations += 1;
            if evaluations > 250_000 {
                break;
            }
            if selected.contains(&i)
                || excluded.contains(&i)
                || !clean[i]
                || choice.score < input.minimum_score
            {
                continue;
            }
            let group = expand_group(i, input, &index);
            if group.iter().any(|&g| excluded.contains(&g) || !clean[g])
                || !counts_allow(input, &selected, &group, &choices)
                || !within_budgets(input, &selected, &group, &choices)
            {
                continue;
            }
            if input.sequences.iter().any(|sequence| {
                let before = selected
                    .iter()
                    .filter(|&&g| {
                        choices[g].assignment.sequence_id == sequence.sequence_id
                            && seconds(&choices[g]) > sequence.pacing.long_review_seconds
                    })
                    .count();
                let after = selected
                    .union(&group)
                    .filter(|&&g| {
                        choices[g].assignment.sequence_id == sequence.sequence_id
                            && seconds(&choices[g]) > sequence.pacing.long_review_seconds
                    })
                    .count();
                after > before && after > sequence.pacing.preferred_long_holds as usize
            }) {
                continue;
            }
            let added = group.difference(&selected).copied().collect::<Vec<_>>();
            if added.iter().any(|&g| {
                let sequence = input
                    .sequences
                    .iter()
                    .find(|s| s.sequence_id == choices[g].assignment.sequence_id)
                    .expect("sequence");
                let long = seconds(&choices[g]) > sequence.pacing.long_review_seconds;
                seconds(&choices[g]) > sequence.pacing.preferred_upper_seconds
                    || (long
                        && selected
                            .iter()
                            .filter(|&&j| {
                                choices[j].assignment.sequence_id == sequence.sequence_id
                                    && seconds(&choices[j]) > sequence.pacing.long_review_seconds
                            })
                            .count()
                            >= sequence.pacing.preferred_long_holds as usize)
                    || selected
                        .iter()
                        .any(|&j| same_source_intersects(&choices[g], &choices[j], true))
                    || added
                        .iter()
                        .any(|&j| j < g && same_source_intersects(&choices[g], &choices[j], true))
            }) {
                continue;
            }
            let score = utility(input, &selected, &group, &choices);
            if score > 0.0
                && best.as_ref().is_none_or(|(old, oldgroup)| {
                    score > *old + 1e-12 || ((score - *old).abs() <= 1e-12 && group < *oldgroup)
                })
            {
                best = Some((score, group));
            }
        }
        if evaluations > 250_000 {
            conflicts.push(conflict("selection_compute_limit",false,None,None,None,"Bounded selection evaluation limit reached; remaining candidates are deferred, not evidence of poor material.".into()));
            break;
        }
        let Some((_, group)) = best else {
            break;
        };
        selected.extend(group);
    }
    // Stable topological order respects authored sequence order. Source-order
    // coverage is diagnostic, never an automatic capture-clock correction.
    let mut ordered = Vec::new();
    for sequence in &input.sequences {
        let members = selected
            .iter()
            .copied()
            .filter(|&i| choices[i].assignment.sequence_id == sequence.sequence_id)
            .collect::<BTreeSet<_>>();
        let mut edges = BTreeSet::new();
        for edge in &input.relationships {
            if let (Some(&a), Some(&b)) = (index.get(&edge.before), index.get(&edge.after))
                && members.contains(&a)
                && members.contains(&b)
            {
                edges.insert((a, b));
            }
        }
        if let Some(establishing) = sequence
            .place_context
            .as_ref()
            .and_then(|p| index.get(&p.establishing_candidate))
        {
            for &other in &members {
                if other != *establishing {
                    edges.insert((*establishing, other));
                }
            }
        }
        let mut pending = members.clone();
        while !pending.is_empty() {
            let next = pending
                .iter()
                .copied()
                .find(|i| !edges.iter().any(|(a, b)| b == i && pending.contains(a)));
            if let Some(next) = next {
                ordered.push(next);
                pending.remove(&next);
            } else {
                conflicts.push(conflict("narrative_cycle",true,Some(&sequence.sequence_id),None,None,"Selected narrative relationships form a cycle; no automatic resolution was applied.".into()));
                ordered.extend(pending);
                break;
            }
        }
    }
    for (position, &i) in ordered.iter().enumerate() {
        let previous = position
            .checked_sub(1)
            .map(|p| choices[ordered[p]].assignment.event.as_str())
            .unwrap_or("chapter opening");
        let next = ordered
            .get(position + 1)
            .map(|&p| choices[p].assignment.event.as_str())
            .unwrap_or("chapter end");
        let links = input
            .relationships
            .iter()
            .filter(|edge| {
                choices[i]
                    .assignment
                    .candidate
                    .as_ref()
                    .is_some_and(|k| k == &edge.before || k == &edge.after)
            })
            .map(|edge| edge.rationale.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        let rationale = format!(
            "{} Declared neighbor context: after '{}', before '{}'. {} This ordering is a proposal requiring assembled playback review.",
            choices[i].assignment.rationale, previous, next, links
        );
        choices[i].assignment.rationale = rationale.clone();
        choices[i].operation.rationale = rationale;
    }
    for (i, choice) in choices
        .iter()
        .enumerate()
        .filter(|(i, _)| !selected.contains(i))
    {
        let sequence = input
            .sequences
            .iter()
            .find(|s| s.sequence_id == choice.assignment.sequence_id)
            .expect("sequence");
        if seconds(choice) > sequence.pacing.preferred_upper_seconds && !excluded.contains(&i) {
            conflicts.push(conflict("deferred_above_preferred_upper",false,Some(&sequence.sequence_id),choice.assignment.candidate.as_ref(),Some(&choice.operation.operation_id),format!("Deferred {:.3}s proposal exceeds the {:.3}s preference; revise the supplied complete beat or explicitly select it with a content rationale. It was not mechanically truncated.",seconds(choice),sequence.pacing.preferred_upper_seconds)));
        }
    }
    let operations = ordered
        .iter()
        .enumerate()
        .map(|(position, &i)| {
            let mut op = choices[i].operation.clone();
            op.position = position as u32;
            op
        })
        .collect::<Vec<_>>();
    let assignments = ordered
        .iter()
        .map(|&i| {
            let mut a = choices[i].assignment.clone();
            a.chosen_source_in_seconds = choices[i].operation.source_in_seconds;
            a.chosen_source_out_seconds = choices[i].operation.source_out_seconds;
            a
        })
        .collect();
    let metadata = SelectionMetadata {
        version: 1,
        recipe: super::selection::RECIPE.into(),
        input_identity: prepared.identity.clone(),
        input: input.clone(),
        assignments,
        constraints_key: prepared.constraints_key.clone(),
        planning_conflicts: conflicts,
    };
    let plan = super::EditPlanRevision {
        revision_id: "selection-proposal".into(),
        project_id: request.project_id.clone(),
        script_revision_id: None,
        parent_revision_id: request.expected_base_revision_id.clone(),
        active: false,
        operations: operations.clone(),
        created_at: String::new(),
    };
    let mut report = super::selection_report::report(&plan, &metadata);
    report.deferred_candidates = choices
        .iter()
        .enumerate()
        .filter(|(i, _)| !selected.contains(i))
        .filter_map(|(_, c)| c.assignment.candidate.clone())
        .collect();
    for item in &mut report.long_take_review {
        item.alternatives = report
            .deferred_candidates
            .iter()
            .filter(|k| {
                k.sequence_id == item.sequence_id
                    && index
                        .get(*k)
                        .is_some_and(|i| clean[*i] && !excluded.contains(i))
            })
            .take(8)
            .cloned()
            .collect();
    }
    Ok(SelectionProposal {
        operations,
        metadata,
        report,
    })
}
