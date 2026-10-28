//! All human decisions below are explicit synthetic test inputs, not claims of
//! editorial listening or pacing acceptance.
use super::*;
use crate::editplan::inspection::tests::fixture;
use crate::{PreviewRequest, RangeDecisionOwner};

fn plan(catalog: &Catalog, asset: &str) -> EditPlanRevision {
    let project = catalog.get_asset(asset).unwrap().project_id;
    catalog
        .create_edit_plan_from_base(
            &project,
            &(0..2)
                .map(|position| EditPlanOperation {
                    operation_id: format!("cut-{position}"),
                    position,
                    asset_id: asset.into(),
                    source_in_seconds: 0.5 + f64::from(position),
                    source_out_seconds: 1.5 + f64::from(position),
                    rationale: "generated review fixture".into(),
                    audio: None,
                })
                .collect::<Vec<_>>(),
            None,
            None,
            None,
        )
        .unwrap()
}
fn note(catalog: &Catalog, plan: &EditPlanRevision) -> ReviewFeedbackView {
    catalog
        .record_review_note(&RecordReviewNoteRequest {
            project_id: plan.project_id.clone(),
            revision_id: plan.revision_id.clone(),
            operation_id: "cut-0".into(),
            author: ReviewAuthor::Human,
            category: ReviewCategory::Endpoint,
            observation: "Synthetic human requests a closer endpoint.".into(),
            viewed_artifact_id: None,
            movie_time_seconds: None,
            evidence_refs: vec![],
        })
        .unwrap()
}
fn propose(
    catalog: &Catalog,
    note: &ReviewFeedbackView,
    change: ReviewChange,
) -> ReviewFeedbackView {
    catalog
        .propose_review_change(&ProposeReviewChangeRequest {
            note_id: note.note.note_id.clone(),
            expected_revision: note.revision,
            author: ReviewAuthor::Reviewer,
            change,
            rationale: "Explicit generated proposal; no listening claim.".into(),
        })
        .unwrap()
}
fn accept(
    catalog: &Catalog,
    view: &ReviewFeedbackView,
    hard: Option<ReviewHardChoice>,
) -> ReviewFeedbackView {
    catalog
        .decide_review_feedback(&DecideReviewFeedbackRequest {
            note_id: view.note.note_id.clone(),
            expected_revision: view.revision,
            proposal_id: if hard.is_none() {
                view.latest_proposal.as_ref().map(|p| p.proposal_id.clone())
            } else {
                None
            },
            decision_owner: RangeDecisionOwner::Human,
            action: ReviewDecisionAction::Accepted,
            rationale: "Synthetic human decision for test mechanics.".into(),
            hard_choice: hard,
        })
        .unwrap()
}
fn apply(
    catalog: &Catalog,
    base: &EditPlanRevision,
    view: &ReviewFeedbackView,
) -> Result<ApplyReviewFeedbackResult> {
    catalog.apply_review_feedback(&ApplyReviewFeedbackRequest {
        project_id: base.project_id.clone(),
        base_revision_id: base.revision_id.clone(),
        expected_active_revision_id: None,
        accepted: vec![AcceptedReviewReference {
            note_id: view.note.note_id.clone(),
            expected_revision: view.revision,
            proposal_id: view.latest_proposal.as_ref().unwrap().proposal_id.clone(),
        }],
    })
}

#[test]
fn observed_movie_proposal_human_acceptance_and_applied_revision_are_separate_durable_facts() {
    let (_dir, catalog, asset) = fixture();
    let base = plan(&catalog, &asset);
    let movie = catalog
        .render_preview(
            &PreviewRequest {
                revision_id: base.revision_id.clone(),
                max_height: 144,
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap();
    let observed = catalog
        .record_review_note(&RecordReviewNoteRequest {
            project_id: base.project_id.clone(),
            revision_id: base.revision_id.clone(),
            operation_id: "cut-0".into(),
            author: ReviewAuthor::Human,
            category: ReviewCategory::Endpoint,
            observation: "Synthetic note against this completed movie.".into(),
            viewed_artifact_id: movie.artifact_id.clone(),
            movie_time_seconds: Some(0.6),
            evidence_refs: vec![],
        })
        .unwrap();
    let evidence = observed.note.target.viewed_movie.as_ref().unwrap();
    assert_eq!(evidence.artifact_id, movie.artifact_id.unwrap());
    assert_eq!(evidence.picture_mapping.operation_id, "cut-0");
    assert_eq!(evidence.timestamp_mapping, "declaredPlaybackSeconds");
    assert_eq!(observed.resolution, "unresolved");
    assert_eq!(observed.revision, 1);
    let proposed = propose(
        &catalog,
        &observed,
        ReviewChange::Trim {
            source_in_seconds: 0.7,
            source_out_seconds: 1.4,
        },
    );
    assert!(matches!(
        apply(&catalog, &base, &proposed)
            .unwrap_err()
            .downcast_ref::<ReviewFeedbackError>(),
        Some(ReviewFeedbackError::NotAccepted)
    ));
    assert_eq!(
        catalog.edit_plan_revisions(&base.project_id).unwrap().len(),
        1
    );
    let accepted = accept(&catalog, &proposed, None);
    assert_eq!(accepted.resolution, "accepted");
    let result = apply(&catalog, &base, &accepted).unwrap();
    assert_eq!(
        result.revision.parent_revision_id,
        Some(base.revision_id.clone())
    );
    assert!(!result.revision.active);
    assert!(result.validation.renderable());
    assert_eq!(result.revision.operations[0].source_in_seconds, 0.7);
    assert_eq!(
        catalog
            .edit_plan_revision(&base.revision_id)
            .unwrap()
            .operations[0]
            .source_in_seconds,
        0.5
    );
    assert!(
        catalog
            .active_edit_plan(&base.project_id)
            .unwrap()
            .is_none()
    );
    assert!(result.diff.changes[0].changes.contains(&"range"));
    let report = catalog
        .review_feedback(&accepted.note.note_id, &result.revision.revision_id)
        .unwrap();
    assert_eq!(report.resolution, "addressed");
    assert_eq!(report.history_total, 4);
    assert_eq!(
        report.latest_decision.unwrap().decision_origin,
        ReviewDecisionOrigin::Host
    );
    assert_eq!(report.note.target.revision_id, base.revision_id);
    assert!(
        report
            .target_status
            .flags
            .iter()
            .any(|s| s == "source_range_changed")
    );
    assert!(matches!(
        apply(&catalog, &base, &accepted)
            .unwrap_err()
            .downcast_ref::<ReviewFeedbackError>(),
        Some(ReviewFeedbackError::StaleRevision { .. })
    ));
    assert_eq!(
        catalog.edit_plan_revisions(&base.project_id).unwrap().len(),
        2
    );
}

#[test]
fn reordered_split_and_reused_targets_are_flagged_and_only_explicit_mapping_can_retarget_an_accepted_change()
 {
    let (_dir, catalog, asset) = fixture();
    let base = plan(&catalog, &asset);
    let accepted = accept(
        &catalog,
        &propose(
            &catalog,
            &note(&catalog, &base),
            ReviewChange::Trim {
                source_in_seconds: 1.1,
                source_out_seconds: 1.4,
            },
        ),
        None,
    );
    let mut cuts = vec![
        base.operations[1].clone(),
        base.operations[0].clone(),
        base.operations[0].clone(),
    ];
    cuts[1].source_out_seconds = 1.0;
    cuts[2].operation_id = "split-tail".into();
    cuts[2].source_in_seconds = 1.0;
    for (index, cut) in cuts.iter_mut().enumerate() {
        cut.position = index as u32;
    }
    let next = catalog
        .create_edit_plan_from_base(&base.project_id, &cuts, None, Some(&base.revision_id), None)
        .unwrap();
    let flagged = catalog
        .review_feedback(&accepted.note.note_id, &next.revision_id)
        .unwrap();
    for expected in [
        "source_range_changed",
        "reordered",
        "neighbors_changed",
        "source_reuse_changed",
    ] {
        assert!(
            flagged.target_status.flags.iter().any(|s| s == expected),
            "{flagged:?}"
        );
    }
    assert_eq!(flagged.resolution, "unresolved");
    assert_eq!(
        flagged.latest_decision.as_ref().unwrap().state,
        ReviewDecisionState::Accepted
    );
    assert_eq!(flagged.note.target.cut.operation_id, "cut-0");
    assert_eq!(flagged.note.target.cut.position, 0);
    assert!(matches!(
        apply(&catalog, &next, &accepted)
            .unwrap_err()
            .downcast_ref::<ReviewFeedbackError>(),
        Some(ReviewFeedbackError::ChangedTarget)
    ));
    let mapped = catalog
        .map_review_target(&MapReviewTargetRequest {
            note_id: accepted.note.note_id.clone(),
            expected_revision: accepted.revision,
            proposal_id: Some(
                accepted
                    .latest_proposal
                    .as_ref()
                    .unwrap()
                    .proposal_id
                    .clone(),
            ),
            revision_id: next.revision_id.clone(),
            operation_id: "split-tail".into(),
            decision_owner: RangeDecisionOwner::Human,
            rationale: "Synthetic human explicitly maps this proposal onto the split tail.".into(),
        })
        .unwrap();
    assert!(
        !mapped
            .proposal_target_status
            .as_ref()
            .unwrap()
            .requires_mapping
    );
    let applied = apply(&catalog, &next, &mapped).unwrap();
    assert_eq!(
        applied
            .revision
            .operations
            .iter()
            .find(|c| c.operation_id == "split-tail")
            .unwrap()
            .source_in_seconds,
        1.1
    );
    assert_eq!(
        catalog
            .review_note(&accepted.note.note_id)
            .unwrap()
            .target
            .cut
            .operation_id,
        "cut-0"
    );
    let mut omitted = next.operations.clone();
    omitted.retain(|c| c.operation_id != "cut-0");
    let without = catalog
        .create_edit_plan_from_base(
            &base.project_id,
            &omitted,
            None,
            Some(&next.revision_id),
            None,
        )
        .unwrap();
    let missing = catalog
        .review_feedback(&accepted.note.note_id, &without.revision_id)
        .unwrap();
    assert!(
        missing
            .target_status
            .flags
            .iter()
            .any(|s| s == "cut_missing_or_split")
    );
    assert!(
        missing
            .target_status
            .candidate_operation_ids
            .contains(&"split-tail".into())
    );
    assert_eq!(
        missing.effective_target.cut.operation_id, "cut-0",
        "candidate overlap must never retarget an original observation"
    );
}

#[test]
fn explicit_human_hero_retention_survives_reselection_and_unaccepted_suggestions_until_explicit_withdrawal()
 {
    let (_dir, catalog, asset) = fixture();
    let base = plan(&catalog, &asset);
    let prior = catalog
        .render_preview(
            &PreviewRequest {
                revision_id: base.revision_id.clone(),
                max_height: 144,
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap();
    let bytes = std::fs::read(&prior.output_path).unwrap();
    let retained = accept(
        &catalog,
        &note(&catalog, &base),
        Some(ReviewHardChoice::KeepPicture),
    );
    let mut reordered = base.operations.clone();
    reordered.reverse();
    let reordered = catalog
        .create_edit_plan_from_base(
            &base.project_id,
            &reordered,
            None,
            Some(&base.revision_id),
            None,
        )
        .unwrap();
    assert!(
        catalog
            .validate_edit_plan(&reordered.revision_id)
            .unwrap()
            .renderable()
    );
    let mut trimmed = reordered.operations.clone();
    trimmed
        .iter_mut()
        .find(|c| c.operation_id == "cut-0")
        .unwrap()
        .source_in_seconds = 0.6;
    let trimmed = catalog
        .create_edit_plan_from_base(
            &base.project_id,
            &trimmed,
            None,
            Some(&reordered.revision_id),
            None,
        )
        .unwrap();
    assert!(
        catalog
            .validate_edit_plan(&trimmed.revision_id)
            .unwrap()
            .errors()
            .any(|f| f.code == "edit_plan_human_choice_violation")
    );
    assert!(
        catalog
            .activate_edit_plan_revision(&trimmed.revision_id, None)
            .is_err()
    );
    assert!(
        catalog
            .render_preview(
                &PreviewRequest {
                    revision_id: trimmed.revision_id.clone(),
                    max_height: 144
                },
                &mut |_| {},
                &|| false
            )
            .is_err()
    );
    assert_eq!(std::fs::read(&prior.output_path).unwrap(), bytes);
    let suggested = propose(&catalog, &retained, ReviewChange::Omit);
    assert!(
        !catalog
            .validate_edit_plan(&trimmed.revision_id)
            .unwrap()
            .renderable()
    );
    let mut request = DecideReviewFeedbackRequest {
        note_id: retained.note.note_id.clone(),
        expected_revision: suggested.revision,
        proposal_id: None,
        decision_owner: RangeDecisionOwner::Human,
        action: ReviewDecisionAction::Deferred,
        rationale: "Synthetic deferral must not withdraw a retained hero.".into(),
        hard_choice: None,
    };
    assert!(
        catalog
            .decide_review_feedback(&request)
            .unwrap_err()
            .to_string()
            .contains("supersede")
    );
    request.action = ReviewDecisionAction::Superseded;
    let released = catalog.decide_review_feedback(&request).unwrap();
    assert_eq!(released.resolution, "superseded");
    assert!(
        catalog
            .validate_edit_plan(&trimmed.revision_id)
            .unwrap()
            .renderable()
    );
    catalog
        .confirm_source_range(&crate::ConfirmSourceRangeRequest {
            asset_id: asset.clone(),
            expected_revision: 0,
            expected_source_fingerprint: catalog.get_asset(&asset).unwrap().fingerprint,
            constraint_id: None,
            decision: crate::RangeDecision::Exclude,
            media: crate::RangeMedia::Picture,
            decision_owner: RangeDecisionOwner::Human,
            source_in_seconds: 0.8,
            source_out_seconds: 0.9,
            rationale: "Synthetic canonical source exclusion remains authoritative.".into(),
            inspected_evidence_id: None,
            overrides_evidence_ids: vec![],
        })
        .unwrap();
    assert!(
        catalog
            .validate_edit_plan(&trimmed.revision_id)
            .unwrap()
            .errors()
            .any(|f| f.code == "edit_plan_confirmed_range_violation")
    );
    assert_eq!(
        catalog
            .source_range_constraints(&asset, 0, 20)
            .unwrap()
            .items
            .len(),
        1
    );
}

#[test]
fn portable_review_history_replays_without_local_movie_files_or_phantom_conflicts() {
    let (_dir, catalog, asset) = fixture();
    let base = plan(&catalog, &asset);
    let accepted = accept(
        &catalog,
        &propose(
            &catalog,
            &note(&catalog, &base),
            ReviewChange::Audio {
                audio: Some(super::super::EditPlanAudio::Silent),
            },
        ),
        None,
    );
    let applied = apply(&catalog, &base, &accepted).unwrap();
    assert!(
        applied
            .diff
            .changes
            .iter()
            .any(|c| c.changes.contains(&"audio"))
    );
    let baseline = catalog.export_project_baseline(&base.project_id).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let peer = Catalog::import_project_baseline(
        dir.path().join("peer.sqlite3"),
        &baseline,
        "node_feedback_peer",
        1,
        "feedback-baseline",
    )
    .unwrap();
    let first = peer
        .review_feedback(&accepted.note.note_id, &applied.revision.revision_id)
        .unwrap();
    assert_eq!(first.resolution, "addressed");
    assert_eq!(first.history_total, 4);
    let conflicts = peer
        .export_project_baseline(&base.project_id)
        .unwrap()
        .conflicts;
    for operation in &baseline.operations {
        peer.apply_operation(operation).unwrap();
    }
    assert_eq!(
        peer.review_feedback(&accepted.note.note_id, &applied.revision.revision_id)
            .unwrap(),
        first
    );
    assert_eq!(
        peer.export_project_baseline(&base.project_id)
            .unwrap()
            .conflicts,
        conflicts
    );
    peer.connection()
        .unwrap()
        .execute_batch("DROP TABLE schema_migrations")
        .unwrap();
    drop(peer);
    let reopened = Catalog::open(dir.path().join("peer.sqlite3")).unwrap();
    assert_eq!(
        reopened
            .review_feedback(&accepted.note.note_id, &applied.revision.revision_id)
            .unwrap(),
        first
    );
}

#[test]
fn concurrent_feedback_decisions_have_one_cas_winner_and_no_extra_fact() {
    let (_dir, catalog, asset) = fixture();
    let base = plan(&catalog, &asset);
    let observed = note(&catalog, &base);
    let left = Catalog::open(catalog.database_path()).unwrap();
    let right = Catalog::open(catalog.database_path()).unwrap();
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let run = |catalog: Catalog, action| {
            barrier.wait();
            catalog.decide_review_feedback(&DecideReviewFeedbackRequest {
                note_id: observed.note.note_id.clone(),
                expected_revision: 1,
                proposal_id: None,
                decision_owner: RangeDecisionOwner::Human,
                action,
                rationale: "Synthetic concurrent human decision.".into(),
                hard_choice: None,
            })
        };
        let a = scope.spawn(move || run(left, ReviewDecisionAction::Deferred));
        let b = scope.spawn(move || run(right, ReviewDecisionAction::Addressed));
        vec![a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .all(|e| matches!(
                e.downcast_ref::<ReviewFeedbackError>(),
                Some(ReviewFeedbackError::StaleRevision { .. })
            ))
    );
    let read = catalog
        .review_feedback(&observed.note.note_id, &base.revision_id)
        .unwrap();
    assert_eq!(read.revision, 2);
    assert_eq!(read.history_total, 2);
}

#[test]
fn accepting_a_new_hard_choice_during_render_blocks_publication_but_keeps_the_earlier_movie() {
    let (_dir, catalog, asset) = fixture();
    let base = plan(&catalog, &asset);
    let request = PreviewRequest {
        revision_id: base.revision_id.clone(),
        max_height: 144,
    };
    let prior = catalog
        .render_preview(&request, &mut |_| {}, &|| false)
        .unwrap();
    let bytes = std::fs::read(&prior.output_path).unwrap();
    let mut later = base.operations.clone();
    let mut hero = later[0].clone();
    hero.operation_id = "new-hero".into();
    hero.position = 2;
    hero.source_in_seconds = 2.5;
    hero.source_out_seconds = 3.5;
    later.push(hero);
    let later = catalog
        .create_edit_plan_from_base(
            &base.project_id,
            &later,
            None,
            Some(&base.revision_id),
            None,
        )
        .unwrap();
    let hero_note = catalog
        .record_review_note(&RecordReviewNoteRequest {
            project_id: base.project_id.clone(),
            revision_id: later.revision_id,
            operation_id: "new-hero".into(),
            author: ReviewAuthor::Human,
            category: ReviewCategory::FeaturedShot,
            observation: "Synthetic explicit hero choice.".into(),
            viewed_artifact_id: None,
            movie_time_seconds: None,
            evidence_refs: vec![],
        })
        .unwrap();
    let mut changed = false;
    let error = catalog
        .render_preview(
            &request,
            &mut |event| {
                if matches!(event, crate::PreviewProgress::ClipCompleted { .. }) && !changed {
                    accept(&catalog, &hero_note, Some(ReviewHardChoice::KeepPicture));
                    changed = true;
                }
            },
            &|| false,
        )
        .unwrap_err();
    assert!(changed);
    assert!(
        error.to_string().contains("constraints changed"),
        "{error:#}"
    );
    assert_eq!(std::fs::read(&prior.output_path).unwrap(), bytes);
    let latest = catalog.preview_artifacts(&base.revision_id, 0, 1).unwrap();
    assert_eq!(latest.items[0].state, crate::PreviewArtifactState::Failed);
    assert!(latest.items[0].output_path.is_none());
}

#[test]
fn bounded_history_always_reserves_space_to_withdraw_a_retained_human_choice() {
    let (_dir, catalog, asset) = fixture();
    let base = plan(&catalog, &asset);
    let mut current = note(&catalog, &base);
    for _ in 0..252 {
        current = propose(
            &catalog,
            &current,
            ReviewChange::Trim {
                source_in_seconds: 0.6,
                source_out_seconds: 1.4,
            },
        );
    }
    assert_eq!(current.revision, 253);
    assert_eq!(current.history.len(), 20);
    assert!(current.history_truncated);
    let retained = accept(&catalog, &current, Some(ReviewHardChoice::KeepPicture));
    assert_eq!(retained.revision, 254);
    assert!(
        catalog
            .propose_review_change(&ProposeReviewChangeRequest {
                note_id: retained.note.note_id.clone(),
                expected_revision: 254,
                author: ReviewAuthor::Reviewer,
                change: ReviewChange::Omit,
                rationale: "Synthetic exhausted proposal capacity.".into()
            })
            .is_err()
    );
    let released = catalog
        .decide_review_feedback(&DecideReviewFeedbackRequest {
            note_id: retained.note.note_id.clone(),
            expected_revision: 254,
            proposal_id: None,
            decision_owner: RangeDecisionOwner::Human,
            action: ReviewDecisionAction::Superseded,
            rationale: "Synthetic explicit withdrawal remains possible after history fills.".into(),
            hard_choice: None,
        })
        .unwrap();
    assert_eq!(released.revision, 255);
    assert!(
        hard_choices(&catalog.connection().unwrap(), &base.project_id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(released.note.target, retained.note.target);
    assert_eq!(released.history_total, 255);
}

#[test]
fn typed_replace_reorder_and_omit_proposals_keep_normal_diffs_and_active_guards() {
    let (_dir, catalog, asset) = fixture();
    let base = plan(&catalog, &asset);
    let replaced = accept(
        &catalog,
        &propose(
            &catalog,
            &note(&catalog, &base),
            ReviewChange::Replace {
                asset_id: asset.clone(),
                source_in_seconds: 0.6,
                source_out_seconds: 1.3,
                audio: Some(super::super::EditPlanAudio::Silent),
            },
        ),
        None,
    );
    let replacement = apply(&catalog, &base, &replaced).unwrap();
    assert!(replacement.diff.changes[0].changes.contains(&"range"));
    assert!(replacement.diff.changes[0].changes.contains(&"audio"));
    let moved = accept(
        &catalog,
        &propose(
            &catalog,
            &note(&catalog, &replacement.revision),
            ReviewChange::Reorder {
                before_operation_id: None,
            },
        ),
        None,
    );
    let reordered = apply(&catalog, &replacement.revision, &moved).unwrap();
    assert_eq!(reordered.revision.operations[1].operation_id, "cut-0");
    assert!(
        reordered
            .diff
            .changes
            .iter()
            .any(|c| c.changes.contains(&"order"))
    );
    let omitted = accept(
        &catalog,
        &propose(
            &catalog,
            &note(&catalog, &reordered.revision),
            ReviewChange::Omit,
        ),
        None,
    );
    catalog
        .activate_edit_plan_revision(&base.revision_id, None)
        .unwrap();
    assert!(matches!(
        apply(&catalog, &reordered.revision, &omitted)
            .unwrap_err()
            .downcast_ref::<super::super::EditPlanError>(),
        Some(super::super::EditPlanError::StaleRevision { .. })
    ));
    let result = catalog
        .apply_review_feedback(&ApplyReviewFeedbackRequest {
            project_id: base.project_id.clone(),
            base_revision_id: reordered.revision.revision_id.clone(),
            expected_active_revision_id: Some(base.revision_id.clone()),
            accepted: vec![AcceptedReviewReference {
                note_id: omitted.note.note_id.clone(),
                expected_revision: omitted.revision,
                proposal_id: omitted.latest_proposal.unwrap().proposal_id,
            }],
        })
        .unwrap();
    assert_eq!(result.revision.operations.len(), 1);
    assert_eq!(result.revision.operations[0].operation_id, "cut-1");
    assert!(
        result
            .diff
            .changes
            .iter()
            .any(|c| c.changes.contains(&"omitted"))
    );
    assert_eq!(
        catalog
            .active_edit_plan(&base.project_id)
            .unwrap()
            .unwrap()
            .revision_id,
        base.revision_id
    );
}

#[test]
fn old_schema_diagnosis_is_readonly_and_previously_unknown_feedback_replays_once_after_upgrade() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("schema54.sqlite3");
    let mut connection = Connection::open(&path).unwrap();
    crate::migrations::run_sqlite_migrations(
        &mut connection,
        &crate::migrations::PROJECT_CATALOG_MIGRATIONS[..54],
    )
    .unwrap();
    connection.execute_batch("INSERT INTO projects(id,name,created_at) VALUES('p','fixture','now'); INSERT INTO edit_plan_revisions(revision_id,project_id,created_at) VALUES('r','p','now');").unwrap();
    drop(connection);
    let reader = Catalog::open_read_only_existing(&path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let report = reader.review_feedback_report("p", "r", 0, 20).unwrap();
    assert!(!report.schema_ready);
    assert_eq!(report.total, 0);
    assert!(matches!(
        reader
            .review_feedback("absent", "r")
            .unwrap_err()
            .downcast_ref::<ReviewFeedbackError>(),
        Some(ReviewFeedbackError::NotFound { .. })
    ));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(!dir.path().join("cache").exists());
    let (_fixture, catalog, asset) = fixture();
    let base = plan(&catalog, &asset);
    let observed = note(&catalog, &base);
    let accepted = accept(
        &catalog,
        &propose(
            &catalog,
            &observed,
            ReviewChange::Trim {
                source_in_seconds: 0.6,
                source_out_seconds: 1.4,
            },
        ),
        None,
    );
    let applied = apply(&catalog, &base, &accepted).unwrap();
    let before = catalog
        .review_feedback(&accepted.note.note_id, &applied.revision.revision_id)
        .unwrap();
    let conflicts = catalog
        .export_project_baseline(&base.project_id)
        .unwrap()
        .conflicts;
    catalog.connection().unwrap().execute_batch("DELETE FROM edit_feedback_note_heads; DELETE FROM edit_feedback_facts; DELETE FROM materialized_revisions WHERE aggregate_type='editReview';").unwrap();
    let path = catalog.database_path().to_path_buf();
    drop(catalog);
    let restored = Catalog::open(&path).unwrap();
    assert_eq!(
        restored
            .review_feedback(&accepted.note.note_id, &applied.revision.revision_id)
            .unwrap(),
        before
    );
    drop(restored);
    let again = Catalog::open(&path).unwrap();
    assert_eq!(
        again
            .review_feedback(&accepted.note.note_id, &applied.revision.revision_id)
            .unwrap(),
        before
    );
    assert_eq!(
        again
            .export_project_baseline(&base.project_id)
            .unwrap()
            .conflicts,
        conflicts
    );
}

#[test]
fn sound_owned_by_a_distant_cut_invalidates_feedback_when_it_overlaps_the_reviewed_neighbors() {
    let (_dir, catalog, asset) = fixture();
    let project = catalog.get_asset(&asset).unwrap().project_id;
    let mut cuts = (0..4)
        .map(|position| EditPlanOperation {
            operation_id: format!("cut-{position}"),
            position,
            asset_id: asset.clone(),
            source_in_seconds: f64::from(position),
            source_out_seconds: f64::from(position) + 1.0,
            rationale: "Generated covered-sound context.".into(),
            audio: Some(super::super::EditPlanAudio::Silent),
        })
        .collect::<Vec<_>>();
    cuts[3].audio = Some(super::super::EditPlanAudio::Source(crate::SourceAudio {
        version: 1,
        asset_id: asset,
        source_in_seconds: 0.0,
        source_out_seconds: 3.0,
        timeline_start_seconds: 0.0,
        sync_offset_seconds: 0.0,
        gain_db: -6.0,
        fade_in_seconds: 0.0,
        fade_out_seconds: 0.0,
    }));
    let base = catalog
        .create_edit_plan_from_base(&project, &cuts, None, None, None)
        .unwrap();
    assert!(
        catalog
            .validate_edit_plan(&base.revision_id)
            .unwrap()
            .renderable()
    );
    let accepted = accept(
        &catalog,
        &propose(
            &catalog,
            &note(&catalog, &base),
            ReviewChange::Trim {
                source_in_seconds: 0.1,
                source_out_seconds: 0.9,
            },
        ),
        None,
    );
    assert_eq!(accepted.note.target.context_audio_event_count, 1);
    assert_eq!(
        accepted.note.target.context_audio_precision,
        "declaredTimeline48k"
    );
    let Some(super::super::EditPlanAudio::Source(audio)) = &mut cuts[3].audio else {
        panic!("fixture voice")
    };
    audio.gain_db = -12.0;
    let changed = catalog
        .create_edit_plan_from_base(&project, &cuts, None, Some(&base.revision_id), None)
        .unwrap();
    let view = catalog
        .review_feedback(&accepted.note.note_id, &changed.revision_id)
        .unwrap();
    assert_eq!(view.target_status.flags, vec!["audio_context_changed"]);
    assert_eq!(view.resolution, "unresolved");
    assert!(matches!(
        apply(&catalog, &changed, &accepted)
            .unwrap_err()
            .downcast_ref::<ReviewFeedbackError>(),
        Some(ReviewFeedbackError::ChangedTarget)
    ));
    let mut control = base.operations.clone();
    control[3].rationale = "Different editorial rationale, identical rendered sound.".into();
    let control = catalog
        .create_edit_plan_from_base(&project, &control, None, Some(&base.revision_id), None)
        .unwrap();
    assert!(
        !catalog
            .review_feedback(&accepted.note.note_id, &control.revision_id)
            .unwrap()
            .target_status
            .requires_mapping
    );
}
