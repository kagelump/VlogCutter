//! Recompute selection receipts from the edit operations that exist now.
use super::{EditPlanOperation, EditPlanRevision, selection_model::*};
use std::collections::BTreeMap;

fn conflict(
    code: &str,
    blocking: bool,
    sequence_id: Option<&str>,
    candidate: Option<&SelectionCandidateKey>,
    operation_id: Option<&str>,
    message: impl Into<String>,
) -> SelectionConflict {
    SelectionConflict {
        candidate: candidate.cloned(),
        asset_id: candidate.map(|c| c.asset_id.clone()),
        code: code.into(),
        blocking,
        sequence_id: sequence_id.map(str::to_owned),
        operation_id: operation_id.map(str::to_owned),
        message: message.into(),
    }
}
fn key_present(
    plan: &EditPlanRevision,
    metadata: &SelectionMetadata,
    key: &SelectionCandidateKey,
) -> Option<String> {
    metadata
        .assignments
        .iter()
        .find(|a| {
            a.candidate.as_ref() == Some(key)
                && plan
                    .operations
                    .iter()
                    .any(|o| o.operation_id == a.operation_id && o.asset_id == a.asset_id)
        })
        .map(|a| a.operation_id.clone())
}
fn sorted_median(values: &[f64]) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    if v.is_empty() {
        0.0
    } else if v.len() % 2 == 1 {
        v[v.len() / 2]
    } else {
        (v[v.len() / 2 - 1] + v[v.len() / 2]) / 2.0
    }
}

pub(super) fn report(plan: &EditPlanRevision, metadata: &SelectionMetadata) -> SelectionReport {
    let input = &metadata.input;
    let operations: BTreeMap<&str, _> = plan
        .operations
        .iter()
        .map(|o| (o.operation_id.as_str(), o))
        .collect();
    let assigned_ids: std::collections::BTreeSet<&str> = metadata
        .assignments
        .iter()
        .filter(|a| {
            operations
                .get(a.operation_id.as_str())
                .is_some_and(|op| op.asset_id == a.asset_id)
        })
        .map(|a| a.operation_id.as_str())
        .collect();
    let mut conflicts = metadata.planning_conflicts.clone();
    let mut by_sequence: BTreeMap<&str, Vec<(&EditPlanOperation, &SelectedMomentAssignment)>> =
        BTreeMap::new();
    for a in &metadata.assignments {
        match operations.get(a.operation_id.as_str()) {
            Some(op) if op.asset_id==a.asset_id => by_sequence.entry(&a.sequence_id).or_default().push((*op,a)),
            _ => conflicts.push(conflict("assigned_cut_missing_or_changed",false,Some(&a.sequence_id),a.candidate.as_ref(),Some(&a.operation_id),"A previously assigned cut is absent or now points at a different source asset; verify required candidates and constraints.")),
        }
    }
    for (sequence, items) in &mut by_sequence {
        items.sort_by_key(|(o, _)| o.position);
        for (op, a) in items.iter() {
            let core_in = a.original_core.start_ms as f64 / 1000.0;
            let core_out = a.original_core.end_ms as f64 / 1000.0;
            let disjoint = op.source_out_seconds <= core_in || op.source_in_seconds >= core_out;
            let required = a.candidate.as_ref().is_some_and(|k| {
                input.include.contains(k)
                    || input.sequences.iter().any(|s| {
                        s.hero_candidates.contains(k)
                            || (s.major_place_change
                                && s.place_context
                                    .as_ref()
                                    .is_some_and(|p| &p.establishing_candidate == k))
                    })
            });
            if disjoint {
                conflicts.push(conflict("cut_outside_original_core_review",required,Some(sequence),a.candidate.as_ref(),Some(&op.operation_id),"The current cut is entirely outside its supplied original core; verify that the selected event is still present."));
            } else if (op.source_in_seconds - a.chosen_source_in_seconds).abs() > 0.001
                || (op.source_out_seconds - a.chosen_source_out_seconds).abs() > 0.001
            {
                conflicts.push(conflict("cut_trim_changed_review",false,Some(sequence),a.candidate.as_ref(),Some(&op.operation_id),"The current trim differs from the proposed complete span; inspect the actual cut and sound."));
            }
        }
    }
    for k in &input.include {
        if key_present(plan, metadata, k).is_none() {
            conflicts.push(conflict(
                "required_include_missing",
                true,
                Some(&k.sequence_id),
                Some(k),
                None,
                "An explicitly included candidate is absent from the current plan.",
            ));
        }
    }
    for k in &input.exclude {
        if let Some(id) = key_present(plan, metadata, k) {
            conflicts.push(conflict(
                "excluded_candidate_present",
                true,
                Some(&k.sequence_id),
                Some(k),
                Some(&id),
                "An explicitly excluded candidate remains in the current plan.",
            ));
        }
    }
    for seq in &input.sequences {
        for k in &seq.hero_candidates {
            if key_present(plan, metadata, k).is_none() {
                conflicts.push(conflict(
                    "required_hero_missing",
                    true,
                    Some(&seq.sequence_id),
                    Some(k),
                    None,
                    "A required hero candidate is absent from the current plan.",
                ));
            }
        }
        if seq.major_place_change {
            match &seq.place_context {
                None => conflicts.push(conflict(
                    "place_context_missing",
                    true,
                    Some(&seq.sequence_id),
                    None,
                    None,
                    "A major place change requires explicit context.",
                )),
                Some(ctx) => {
                    let establishing = key_present(plan, metadata, &ctx.establishing_candidate);
                    let first = by_sequence
                        .get(seq.sequence_id.as_str())
                        .and_then(|v| v.first())
                        .map(|(_, a)| a.operation_id.as_str());
                    if establishing.is_none() || first != establishing.as_deref() {
                        conflicts.push(conflict("place_establishing_cut_not_first",true,Some(&seq.sequence_id),Some(&ctx.establishing_candidate),establishing.as_deref(),"The actual establishing candidate must be the first operation in this sequence."));
                    }
                }
            }
        }
    }
    for rule in &input.relationships {
        let before = key_present(plan, metadata, &rule.before);
        let after = key_present(plan, metadata, &rule.after);
        if rule.keep_together && rule.hard && before.is_some() != after.is_some() {
            conflicts.push(conflict(
                "keep_together_split",
                true,
                Some(&rule.before.sequence_id),
                Some(&rule.before),
                before.as_deref().or(after.as_deref()),
                "A hard keep-together relationship has exactly one candidate selected.",
            ));
        }
        if let (Some(b), Some(a)) = (before, after) {
            let bp = operations
                .get(b.as_str())
                .map(|o| o.position)
                .unwrap_or(u32::MAX);
            let ap = operations
                .get(a.as_str())
                .map(|o| o.position)
                .unwrap_or(u32::MAX);
            if bp >= ap {
                conflicts.push(conflict(
                    "relationship_order_violated",
                    rule.hard,
                    Some(&rule.before.sequence_id),
                    Some(&rule.before),
                    Some(&b),
                    "The selected candidates do not appear in the requested before/after order.",
                ));
            }
        }
    }
    let mut sequences = Vec::new();
    let total = plan
        .operations
        .iter()
        .map(|op| (op.source_out_seconds - op.source_in_seconds).max(0.0))
        .sum::<f64>();
    let mut long_take_review = Vec::new();
    for op in plan
        .operations
        .iter()
        .filter(|o| !assigned_ids.contains(o.operation_id.as_str()))
    {
        conflicts.push(conflict("current_cut_unassigned",true,None,None,Some(&op.operation_id),"A current operation has no selection assignment; its duration is included in the total budget."));
    }
    for seq in &input.sequences {
        let items = by_sequence
            .get(seq.sequence_id.as_str())
            .cloned()
            .unwrap_or_default();
        let mut roles: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        let (mut active, mut passive, mut uncertain) = (0.0, 0.0, 0.0);
        let (mut early, mut middle, mut late) = (0, 0, 0);
        let mut earned = 0.0;
        let mut long_holds = 0u32;
        for (op, a) in &items {
            let duration = (op.source_out_seconds - op.source_in_seconds).max(0.0);
            earned += duration;
            roles.entry(a.role.clone()).or_default().push(duration);
            match a.activity {
                SelectionActivity::Active
                | SelectionActivity::Movement
                | SelectionActivity::Dialogue => active += duration,
                SelectionActivity::Passive => passive += duration,
                SelectionActivity::Uncertain => uncertain += duration,
            }
            match a.coverage_bucket {
                0 => early += 1,
                1 => middle += 1,
                _ => late += 1,
            }
            if duration > seq.pacing.long_review_seconds {
                long_holds += 1;
                long_take_review.push(SelectionReviewItem {operation_id:op.operation_id.clone(),sequence_id:seq.sequence_id.clone(),duration_seconds:duration,
                reason:format!("{} Event: {}. {} Long take exceeds the {} second review threshold; check that the hold earns its duration and whether a deliberate montage treatment fits.",a.rationale,a.event,a.uncertainty.join(" "),seq.pacing.long_review_seconds),alternatives:Vec::new()});
            }
            if duration > seq.pacing.preferred_upper_seconds {
                conflicts.push(conflict(
                    "take_above_preferred_upper",
                    false,
                    Some(&seq.sequence_id),
                    a.candidate.as_ref(),
                    Some(&op.operation_id),
                    "A take exceeds the preferred upper duration and needs editorial review.",
                ));
            }
        }
        if long_holds > seq.pacing.preferred_long_holds {
            conflicts.push(conflict("long_hold_count_above_preference",false,Some(&seq.sequence_id),None,None,format!("{} takes exceed the long-review threshold; preferred occasional long holds: {}.",long_holds,seq.pacing.preferred_long_holds)));
        }
        let budget = seq.budget_seconds;
        if let Some(max) = budget {
            if earned > max + 1e-9 {
                conflicts.push(conflict(
                    "sequence_budget_exceeded",
                    seq.hard_budget,
                    Some(&seq.sequence_id),
                    None,
                    None,
                    format!("Current sequence duration {earned:.3}s exceeds {max:.3}s."),
                ));
            }
        }
        let mut distributions = roles
            .into_iter()
            .map(|(role, v)| SelectionRoleDistribution {
                role,
                count: v.len() as u32,
                duration_seconds: v.iter().sum(),
                shortest_seconds: v.iter().copied().min_by(f64::total_cmp).unwrap_or(0.0),
                longest_seconds: v.iter().copied().max_by(f64::total_cmp).unwrap_or(0.0),
                median_seconds: sorted_median(&v),
            })
            .collect::<Vec<_>>();
        distributions.sort_by(|a, b| a.role.cmp(&b.role));
        sequences.push(SelectionSequenceSummary {
            sequence_id: seq.sequence_id.clone(),
            goal: seq.goal.clone(),
            earned_seconds: earned,
            preferred_budget_seconds: budget,
            unfilled_seconds: budget.map(|b| (b - earned).max(0.0)),
            active_seconds: active,
            passive_seconds: passive,
            uncertain_seconds: uncertain,
            early_count: early,
            middle_count: middle,
            late_count: late,
            roles: distributions,
        });
    }
    let mut events: BTreeMap<(&str, String), Vec<&SelectedMomentAssignment>> = BTreeMap::new();
    for a in &metadata.assignments {
        if assigned_ids.contains(a.operation_id.as_str()) {
            events
                .entry((&a.sequence_id, a.event.trim().to_lowercase()))
                .or_default()
                .push(a);
        }
    }
    for ((sequence, event), assignments) in events.iter().filter(|(_, v)| v.len() > 1) {
        conflicts.push(conflict("repeated_event_review",false,Some(sequence),assignments[0].candidate.as_ref(),Some(&assignments[0].operation_id),format!("{} selected cuts repeat event '{}'; verify each setup/pause/arrival/day-end cue is necessary. Reasons: {}",assignments.len(),event,assignments.iter().map(|a|a.rationale.as_str()).collect::<Vec<_>>().join("; "))));
    }
    for c in &input.counts {
        let count = metadata
            .assignments
            .iter()
            .filter(|a| {
                operations
                    .get(a.operation_id.as_str())
                    .is_some_and(|o| o.asset_id == a.asset_id)
                    && c.sequence_id.as_ref().is_none_or(|s| s == &a.sequence_id)
                    && a.role.eq_ignore_ascii_case(&c.role)
            })
            .count() as u32;
        if count < c.minimum || count > c.maximum {
            conflicts.push(conflict(
                "role_count_out_of_range",
                c.hard,
                c.sequence_id.as_deref(),
                None,
                None,
                format!(
                    "Role '{}' has {count} current cuts; requested range is {}–{}.",
                    c.role, c.minimum, c.maximum
                ),
            ));
        }
    }
    if let Some(budget) = input.budget_seconds {
        if total > budget + 1e-9 {
            conflicts.push(conflict(
                "total_budget_exceeded",
                input.hard_budget,
                None,
                None,
                None,
                format!("Current total duration {total:.3}s exceeds {budget:.3}s."),
            ));
        }
    }
    let preferred = input.budget_seconds;
    SelectionReport{duration_basis:"requestedSourceRanges".into(),recipe:metadata.recipe.clone(),input_identity:metadata.input_identity.clone(),reused:false,earned_seconds:total,preferred_budget_seconds:preferred,unfilled_seconds:preferred.map(|b|(b-total).max(0.0)),sequences,conflicts,long_take_review,deferred_candidates:Vec::new(),limitations:vec!["Durations are computed from current requested source ranges; they do not measure preview playback.".into(),"Candidate and utterance timings remain locator evidence; inspect picture and listen to audio before acceptance.".into(),"No filler or weak material is added to meet a preferred runtime.".into()]}
}
