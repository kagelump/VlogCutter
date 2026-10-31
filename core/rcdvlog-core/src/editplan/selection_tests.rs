//! Synthetic source locators and human decisions test mechanics, not editorial acceptance.
use super::*;
use crate::moments::{MomentCandidate, MomentSpan};
use crate::{EditPlanAudio, RangeDecisionOwner};
use std::process::Command;

fn fixture() -> (tempfile::TempDir, Catalog, String) {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("generated sources 日本語");
    std::fs::create_dir(&media).unwrap();
    let output = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=32x18:r=10:d=100",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(media.join("public.mp4"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let catalog = Catalog::open(dir.path().join("catalog.sqlite3")).unwrap();
    let project = catalog
        .create_project("Generated sequence selection")
        .unwrap();
    assert_eq!(
        catalog.ingest_folder(&project.id, &media).unwrap().imported,
        1
    );
    let asset = catalog.list_assets(&project.id).unwrap()[0].id.clone();
    (dir, catalog, asset)
}
fn span(start: u32, end: u32) -> MomentSpan {
    MomentSpan {
        start_ms: start,
        end_ms: end,
    }
}
fn moment(id: &str, start: u32, end: u32, score: f64, role: &str, event: &str) -> MomentCandidate {
    MomentCandidate {
        id: id.into(),
        rank: 1,
        score,
        core: span(start, end),
        payoff: span(start + (end - start) / 2, end),
        setup: None,
        context: None,
        allowed_range: span(0, 100_000),
        reason: format!("Generated event {event} has a visible complete payoff."),
        event: event.into(),
        role: role.into(),
        evidence: vec![],
        uncertainty: vec!["Synthetic authored locator, not a listening verdict.".into()],
        dialogue_boundaries: vec![],
        refined: false,
        long_hold: false,
    }
}
fn request(
    catalog: &Catalog,
    asset: &str,
    candidates: Vec<MomentCandidate>,
) -> SequenceSelectionRequest {
    let asset = catalog.get_asset(asset).unwrap();
    SequenceSelectionRequest {
        project_id: asset.project_id,
        expected_base_revision_id: None,
        expected_active_revision_id: None,
        input: SequenceSelectionInput {
            version: 1,
            decision_owner: RangeDecisionOwner::Human,
            goal: "Travel, action and arrival without filler".into(),
            sequences: vec![SequenceIntent {
                sequence_id: "chapter".into(),
                goal: "Show the useful events and arrival".into(),
                pacing: SequencePacingProfile::default(),
                sources: vec![SelectionMomentSource::Declared {
                    asset_id: asset.id,
                    expected_source_fingerprint: asset.fingerprint,
                    rationale: "Explicit generated test locators".into(),
                    candidates,
                }],
                role_claims: vec![],
                retained_choices: vec![],
                hero_candidates: vec![],
                budget_seconds: Some(60.0),
                hard_budget: false,
                major_place_change: false,
                place_context: None,
            }],
            include: vec![],
            exclude: vec![],
            counts: vec![],
            relationships: vec![],
            budget_seconds: Some(60.0),
            hard_budget: false,
            minimum_score: 0.5,
            repetition_penalty: 0.5,
            coverage_bonus: 0.1,
        },
    }
}
fn key(asset: &str, id: &str) -> SelectionCandidateKey {
    SelectionCandidateKey {
        sequence_id: "chapter".into(),
        asset_id: asset.into(),
        candidate_id: id.into(),
    }
}
fn selected_ids(p: &SelectionProposal) -> Vec<&str> {
    p.metadata
        .assignments
        .iter()
        .filter_map(|a| a.candidate.as_ref().map(|k| k.candidate_id.as_str()))
        .collect()
}

#[test]
fn strongest_late_payoff_disjoint_events_and_sparse_runtime_are_not_early_repetition_or_filler() {
    let (_dir, catalog, asset) = fixture();
    let r = request(
        &catalog,
        &asset,
        vec![
            moment("repeat-a", 1_000, 5_000, 0.9, "travel", "walking"),
            moment("repeat-b", 7_000, 11_000, 0.9, "travel", "walking"),
            moment(
                "late-payoff",
                85_000,
                87_000,
                0.98,
                "arrival",
                "ship appears",
            ),
            moment("concise", 50_000, 51_000, 0.9, "action", "jump"),
            moment("waiting", 60_000, 68_000, 1.0, "waiting", "waiting"),
        ],
    );
    let p = catalog.preview_sequence_selection(&r).unwrap();
    let ids = selected_ids(&p);
    assert!(ids.contains(&"late-payoff") && ids.contains(&"concise"));
    assert_eq!(
        ids.iter().filter(|&&id| id.starts_with("repeat")).count(),
        1
    );
    assert!(!ids.contains(&"waiting"));
    assert!(p.report.earned_seconds < 15.0 && p.report.unfilled_seconds.unwrap() > 45.0);
    let late = p
        .metadata
        .assignments
        .iter()
        .find(|a| a.candidate.as_ref().unwrap().candidate_id == "late-payoff")
        .unwrap();
    assert_eq!(
        (
            late.chosen_source_in_seconds,
            late.chosen_source_out_seconds
        ),
        (85.0, 87.0)
    );
    let summary = &p.report.sequences[0];
    assert!(summary.early_count > 0 && summary.middle_count > 0 && summary.late_count > 0);
    assert_eq!(catalog.preview_sequence_selection(&r).unwrap(), p);
    assert_eq!(
        catalog
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM edit_plan_revisions", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        catalog
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM edit_selection_cache", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn movement_fragments_relocate_to_payoff_while_complete_utterance_hero_and_action_remain_whole() {
    let (_dir, catalog, asset) = fixture();
    let mut movement = moment("movement", 10_000, 30_000, 0.9, "travel", "approach");
    movement.payoff = span(28_000, 28_700);
    let mut speech = moment(
        "speech",
        40_000,
        42_000,
        0.95,
        "dialogue",
        "useful sentence",
    );
    speech.dialogue_boundaries = vec![span(39_000, 44_000), span(43_000, 45_000)];
    let mut action = moment(
        "action",
        60_000,
        70_000,
        0.95,
        "wildlife",
        "animal dives and resurfaces",
    );
    action.setup = Some(span(58_000, 60_000));
    let mut r = request(&catalog, &asset, vec![movement, speech, action]);
    let profile = &mut r.input.sequences[0].pacing;
    profile.preset = SequencePacingPreset::RapidMovement;
    profile.preferred_min_seconds = 0.5;
    profile.preferred_max_seconds = 2.0;
    profile.allow_montage_fragments = true;
    r.input.sequences[0].hero_candidates = vec![key(&asset, "action")];
    r.input.include.push(key(&asset, "speech"));
    let p = catalog.preview_sequence_selection(&r).unwrap();
    let find = |id: &str| {
        p.metadata
            .assignments
            .iter()
            .find(|a| a.candidate.as_ref().unwrap().candidate_id == id)
            .unwrap()
    };
    let movement = find("movement");
    assert!(movement.montage_fragment);
    assert!(
        movement.chosen_source_in_seconds >= 27.0 && movement.chosen_source_out_seconds >= 28.7
    );
    assert!(movement.chosen_source_out_seconds - movement.chosen_source_in_seconds <= 2.0);
    assert_eq!(
        (
            find("speech").chosen_source_in_seconds,
            find("speech").chosen_source_out_seconds
        ),
        (39.0, 45.0)
    );
    assert_eq!(
        (
            find("action").chosen_source_in_seconds,
            find("action").chosen_source_out_seconds
        ),
        (58.0, 70.0)
    );
    assert!(!p.report.conflicts.iter().any(|c| c.blocking));
    assert!(!p.report.long_take_review.is_empty());
}

#[test]
fn closure_counts_narrative_relationships_place_context_and_conflicting_budgets_are_explicit() {
    let (_dir, catalog, asset) = fixture();
    let mut r = request(
        &catalog,
        &asset,
        vec![
            moment("close-1", 80_000, 84_000, 0.9, "closure", "day ends"),
            moment("close-2", 90_000, 94_000, 0.95, "closure", "day ends"),
            moment("payoff", 20_000, 25_000, 0.99, "action", "landing"),
            moment("setup", 10_000, 14_000, 0.8, "setup", "approach"),
            moment(
                "establish",
                60_000,
                63_000,
                0.6,
                "arrival",
                "the named harbor",
            ),
        ],
    );
    r.input.counts.push(SelectionCountConstraint {
        sequence_id: Some("chapter".into()),
        role: "closure".into(),
        minimum: 1,
        maximum: 1,
        hard: true,
        rationale: "One closure avoids duplicated day-end cues".into(),
    });
    r.input.relationships.push(SelectionRelationship {
        before: key(&asset, "setup"),
        after: key(&asset, "payoff"),
        keep_together: true,
        hard: true,
        rationale: "Setup before landing".into(),
    });
    r.input.include.push(key(&asset, "payoff"));
    r.input.sequences[0].major_place_change = true;
    let missing = catalog.preview_sequence_selection(&r).unwrap();
    assert!(
        missing
            .report
            .conflicts
            .iter()
            .any(|c| c.code == "place_context_missing" && c.blocking)
    );
    r.input.sequences[0].place_context = Some(SequencePlaceContext {
        kind: PlaceContextKind::HumanSupplied,
        description: "The supplied arrival locator establishes the fictional harbor".into(),
        establishing_candidate: key(&asset, "establish"),
        decision_owner: RangeDecisionOwner::Human,
    });
    let p = catalog.preview_sequence_selection(&r).unwrap();
    let ids = selected_ids(&p);
    assert_eq!(ids[0], "establish");
    assert!(ids.iter().position(|id| *id == "setup") < ids.iter().position(|id| *id == "payoff"));
    assert_eq!(ids.iter().filter(|id| id.starts_with("close")).count(), 1);
    r.input.budget_seconds = Some(1.0);
    r.input.hard_budget = true;
    r.input.exclude.push(key(&asset, "setup"));
    let p = catalog.preview_sequence_selection(&r).unwrap();
    assert!(
        p.report
            .conflicts
            .iter()
            .any(|c| c.code == "required_choice_conflict")
    );
    assert!(
        p.report
            .conflicts
            .iter()
            .any(|c| c.code == "total_budget_exceeded" && c.blocking)
    );
    let draft = catalog.create_sequence_selection(&r).unwrap();
    assert!(!draft.validation.renderable());
    assert!(
        catalog
            .activate_edit_plan_revision(&draft.revision.revision_id, None)
            .is_err()
    );
}

#[test]
fn persistence_cache_identity_refined_feedback_inheritance_baseline_and_payload_downgrade_are_safe()
{
    let (dir, catalog, asset) = fixture();
    let r = request(
        &catalog,
        &asset,
        vec![
            moment("a", 1_000, 5_000, 0.9, "action", "first"),
            moment("b", 70_000, 72_000, 0.9, "arrival", "arrival"),
        ],
    );
    let first = catalog.create_sequence_selection(&r).unwrap();
    assert!(!first.revision.active);
    assert!(!first.report.reused);
    assert!(
        catalog
            .preview_sequence_selection(&r)
            .unwrap()
            .report
            .reused
    );
    let mut modified = r.clone();
    modified.input.sequences[0].pacing.preferred_min_seconds = 2.0;
    assert_ne!(
        catalog
            .preview_sequence_selection(&modified)
            .unwrap()
            .metadata
            .input_identity,
        first.report.input_identity
    );
    let mut cuts = first.revision.operations.clone();
    cuts[0].source_in_seconds += 0.25;
    cuts[0].audio = Some(EditPlanAudio::Silent);
    cuts.reverse();
    for (position, c) in cuts.iter_mut().enumerate() {
        c.position = position as u32;
    }
    let next = catalog
        .create_edit_plan_from_base(
            &r.project_id,
            &cuts,
            None,
            Some(&first.revision.revision_id),
            None,
        )
        .unwrap();
    let view = catalog.edit_plan_selection(&next.revision_id).unwrap();
    assert_eq!(
        view.metadata,
        catalog
            .edit_plan_selection_metadata(&first.revision.revision_id)
            .unwrap()
    );
    assert_eq!(
        view.report.as_ref().unwrap().earned_seconds,
        first.report.earned_seconds - 0.25
    );
    assert!(!view.report.unwrap().conflicts.iter().any(|c| c.blocking));
    let baseline = catalog.export_project_baseline(&r.project_id).unwrap();
    let peer = Catalog::import_project_baseline(
        dir.path().join("peer.sqlite3"),
        &baseline,
        "node_selection_peer",
        1,
        "selection-baseline",
    )
    .unwrap();
    assert_eq!(
        peer.edit_plan_selection(&next.revision_id).unwrap(),
        catalog.edit_plan_selection(&next.revision_id).unwrap()
    );
    let created = baseline
        .operations
        .iter()
        .find(|op| op.operation_type == "editPlanRevisionCreated")
        .unwrap();
    assert_eq!(created.payload_version, 3);
    let mut downgraded = created.clone();
    downgraded.operation_id = "selection-downgraded".into();
    downgraded.payload_version = 2;
    downgraded.origin_sequence += 1000;
    assert!(peer.apply_operation(&downgraded).is_err());
    for op in &baseline.operations {
        peer.apply_operation(op).unwrap();
    }
    assert_eq!(
        peer.edit_plan_selection(&next.revision_id).unwrap(),
        catalog.edit_plan_selection(&next.revision_id).unwrap()
    );
    let readonly = Catalog::open_read_only_existing(dir.path().join("catalog.sqlite3")).unwrap();
    assert_eq!(
        readonly
            .edit_plan_selection(&next.revision_id)
            .unwrap()
            .report
            .unwrap()
            .earned_seconds,
        first.report.earned_seconds - 0.25
    );
}

#[test]
fn semantic_ranges_are_rejected_and_long_adjacent_sources_are_not_manufactured_variety() {
    let (_dir, catalog, asset) = fixture();
    let mut r = request(
        &catalog,
        &asset,
        vec![moment("bad", 4_000, 5_000, 0.9, "travel", "invalid")],
    );
    if let SelectionMomentSource::Declared { candidates, .. } = &mut r.input.sequences[0].sources[0]
    {
        candidates[0].core = span(5000, 4000);
    }
    assert!(catalog.preview_sequence_selection(&r).is_err());
    if let SelectionMomentSource::Declared { candidates, .. } = &mut r.input.sequences[0].sources[0]
    {
        candidates[0] = moment("outside", 99_000, 101_000, 0.9, "travel", "outside");
    }
    assert!(catalog.preview_sequence_selection(&r).is_err());
    r = request(
        &catalog,
        &asset,
        vec![
            moment("a", 1_000, 5_000, 0.9, "action", "one"),
            moment("adjacent", 5_000, 8_000, 0.9, "action", "other"),
            moment("long", 30_000, 65_000, 0.99, "wildlife", "long proposal"),
        ],
    );
    let p = catalog.preview_sequence_selection(&r).unwrap();
    assert_eq!(p.operations.len(), 1);
    assert!(
        p.report
            .deferred_candidates
            .iter()
            .any(|k| k.candidate_id == "long")
    );
    r.input.include.push(key(&asset, "long"));
    let p = catalog.preview_sequence_selection(&r).unwrap();
    assert!(
        p.report
            .long_take_review
            .iter()
            .any(|i| i.duration_seconds == 35.0)
    );
    assert!(
        p.report
            .conflicts
            .iter()
            .any(|c| c.code == "take_above_preferred_upper")
    );
}

#[test]
fn retained_human_choices_and_canonical_exclusions_survive_reselection_without_duplicate_owners() {
    use crate::{
        ConfirmSourceRangeRequest, DecideReviewFeedbackRequest, RangeDecision, RangeMedia,
        RecordReviewNoteRequest, ReviewAuthor, ReviewCategory, ReviewDecisionAction,
        ReviewHardChoice,
    };
    let (_dir, catalog, asset) = fixture();
    let mut r = request(
        &catalog,
        &asset,
        vec![
            moment("a", 1_000, 5_000, 0.9, "action", "first"),
            moment("b", 70_000, 72_000, 0.9, "arrival", "arrival"),
        ],
    );
    let first = catalog.create_sequence_selection(&r).unwrap();
    let cut = first.revision.operations[0].clone();
    let note = catalog
        .record_review_note(&RecordReviewNoteRequest {
            project_id: r.project_id.clone(),
            revision_id: first.revision.revision_id.clone(),
            operation_id: cut.operation_id.clone(),
            author: ReviewAuthor::Human,
            category: ReviewCategory::Duration,
            observation: "Synthetic hard retention test input".into(),
            viewed_artifact_id: None,
            movie_time_seconds: None,
            evidence_refs: vec![],
        })
        .unwrap();
    catalog
        .decide_review_feedback(&DecideReviewFeedbackRequest {
            provenance: None,
            note_id: note.note.note_id.clone(),
            expected_revision: note.revision,
            proposal_id: None,
            decision_owner: Some(RangeDecisionOwner::Human),
            action: ReviewDecisionAction::Accepted,
            rationale: "Synthetic owner keeps this exact source interval".into(),
            hard_choice: Some(ReviewHardChoice::KeepPictureAndAudio),
        })
        .unwrap();
    r.expected_base_revision_id = Some(first.revision.revision_id.clone());
    r.input.exclude.push(key(&asset, "a"));
    let conflicting = catalog.preview_sequence_selection(&r).unwrap();
    assert!(
        conflicting
            .operations
            .iter()
            .any(|c| c.operation_id == cut.operation_id)
    );
    assert!(conflicting.report.conflicts.iter().any(|c| c.blocking));
    r.input.exclude.clear();
    if let SelectionMomentSource::Declared { candidates, .. } = &mut r.input.sequences[0].sources[0]
    {
        candidates.retain(|c| c.id != "a");
    }
    let retained = catalog.create_sequence_selection(&r).unwrap();
    assert!(
        retained
            .revision
            .operations
            .iter()
            .any(|c| c.operation_id == cut.operation_id
                && c.source_in_seconds == cut.source_in_seconds
                && c.source_out_seconds == cut.source_out_seconds)
    );
    let before = catalog
        .preview_sequence_selection(&r)
        .unwrap()
        .metadata
        .input_identity;
    catalog
        .confirm_source_range(&ConfirmSourceRangeRequest {
            asset_id: asset.clone(),
            expected_revision: 0,
            expected_source_fingerprint: catalog.get_asset(&asset).unwrap().fingerprint,
            constraint_id: None,
            decision: RangeDecision::Exclude,
            media: RangeMedia::Both,
            decision_owner: RangeDecisionOwner::Human,
            source_in_seconds: 70.0,
            source_out_seconds: 72.0,
            rationale: "Synthetic explicit excluded interval".into(),
            inspected_evidence_id: None,
            overrides_evidence_ids: vec![],
        })
        .unwrap();
    let after = catalog.preview_sequence_selection(&r).unwrap();
    assert_ne!(after.metadata.input_identity, before);
    assert!(!after.operations.iter().any(|c| c.source_in_seconds == 70.0));
    let prepared = crate::editplan::selection_source::prepare(&catalog, &r).unwrap();
    catalog
        .confirm_source_range(&ConfirmSourceRangeRequest {
            asset_id: asset.clone(),
            expected_revision: 1,
            expected_source_fingerprint: catalog.get_asset(&asset).unwrap().fingerprint,
            constraint_id: None,
            decision: RangeDecision::Exclude,
            media: RangeMedia::Both,
            decision_owner: RangeDecisionOwner::Human,
            source_in_seconds: 90.0,
            source_out_seconds: 91.0,
            rationale: "Synthetic concurrent constraint".into(),
            inspected_evidence_id: None,
            overrides_evidence_ids: vec![],
        })
        .unwrap();
    assert_ne!(
        crate::editplan::ranges::constraints_key(
            &catalog.connection().unwrap(),
            &prepared.constraint_scope
        )
        .unwrap(),
        prepared.constraints_key
    );
}

#[test]
fn immutable_intent_changes_invalidate_feedback_targets_and_unknown_new_cuts_cannot_evade_budgets()
{
    use crate::{RecordReviewNoteRequest, ReviewAuthor, ReviewCategory};
    let (_dir, catalog, asset) = fixture();
    let mut r = request(
        &catalog,
        &asset,
        vec![moment("a", 1_000, 5_000, 0.9, "action", "first")],
    );
    let first = catalog.create_sequence_selection(&r).unwrap();
    let note = catalog
        .record_review_note(&RecordReviewNoteRequest {
            project_id: r.project_id.clone(),
            revision_id: first.revision.revision_id.clone(),
            operation_id: first.revision.operations[0].operation_id.clone(),
            author: ReviewAuthor::Human,
            category: ReviewCategory::Duration,
            observation: "Synthetic review under original intent".into(),
            viewed_artifact_id: None,
            movie_time_seconds: None,
            evidence_refs: vec![],
        })
        .unwrap();
    r.expected_base_revision_id = Some(first.revision.revision_id.clone());
    r.input.goal = "A different authored story goal".into();
    let second = catalog.create_sequence_selection(&r).unwrap();
    assert_eq!(first.revision.operations, second.revision.operations);
    let status = catalog
        .review_feedback(&note.note.note_id, &second.revision.revision_id)
        .unwrap()
        .target_status;
    assert!(status.flags.contains(&"editorial_intent_changed".into()));
    assert!(status.requires_mapping);
    let mut cuts = second.revision.operations.clone();
    cuts.push(crate::EditPlanOperation {
        operation_id: "unassigned".into(),
        position: 1,
        asset_id: asset.clone(),
        source_in_seconds: 80.0,
        source_out_seconds: 90.0,
        rationale: "New cut needs explicit selection intent".into(),
        audio: None,
    });
    let third = catalog
        .create_edit_plan_from_base(
            &r.project_id,
            &cuts,
            None,
            Some(&second.revision.revision_id),
            None,
        )
        .unwrap();
    let report = catalog
        .edit_plan_selection(&third.revision_id)
        .unwrap()
        .report
        .unwrap();
    assert_eq!(report.earned_seconds, 14.0);
    assert!(
        report
            .conflicts
            .iter()
            .any(|c| c.code == "current_cut_unassigned" && c.blocking)
    );
}

#[test]
fn payload_three_recovery_and_immutable_id_collisions_do_not_rewrite_prior_intent() {
    let (dir, catalog, asset) = fixture();
    let r = request(
        &catalog,
        &asset,
        vec![moment("a", 1_000, 5_000, 0.9, "action", "first")],
    );
    let draft = catalog.create_sequence_selection(&r).unwrap();
    let prior = catalog
        .edit_plan_selection(&draft.revision.revision_id)
        .unwrap();
    let baseline = catalog.export_project_baseline(&r.project_id).unwrap();
    let mut collided = baseline
        .operations
        .iter()
        .find(|op| op.operation_type == "editPlanRevisionCreated")
        .unwrap()
        .clone();
    collided.operation_id = "different-op-same-revision".into();
    collided.origin_sequence += 1000;
    collided.base_revision=Some(catalog.connection().unwrap().query_row("SELECT revision FROM materialized_revisions WHERE aggregate_type='editPlan' AND aggregate_id=?1",[&r.project_id],|row|row.get::<_,i64>(0)).unwrap() as u64);
    collided.payload["selection"]["input"]["goal"] =
        serde_json::json!("Attempt to overwrite immutable intent");
    assert!(catalog.apply_operation(&collided).is_err());
    assert_eq!(
        catalog
            .edit_plan_selection(&draft.revision.revision_id)
            .unwrap(),
        prior
    );
    // Simulate an older build's retained-but-unmaterialized v3 operation.
    catalog.connection().unwrap().execute_batch("DELETE FROM edit_plan_operations; DELETE FROM edit_plan_revisions; DELETE FROM materialized_revisions WHERE aggregate_type='editPlan';").unwrap();
    let path = catalog.database_path().to_path_buf();
    drop(catalog);
    let restored = Catalog::open(&path).unwrap();
    assert_eq!(
        restored
            .edit_plan_selection(&draft.revision.revision_id)
            .unwrap(),
        prior
    );
    drop(restored);
    let again = Catalog::open(&path).unwrap();
    assert_eq!(
        again
            .edit_plan_selection(&draft.revision.revision_id)
            .unwrap(),
        prior
    );
    again
        .connection()
        .unwrap()
        .execute_batch("DROP TABLE schema_migrations")
        .unwrap();
    drop(again);
    let legacy = Catalog::open(dir.path().join("catalog.sqlite3")).unwrap();
    assert_eq!(
        legacy
            .edit_plan_selection(&draft.revision.revision_id)
            .unwrap(),
        prior
    );
}
