use super::*;
use crate::editplan::inspection::tests::fixture;

fn plans(catalog: &Catalog, asset: &str) -> (EditPlanRevision, EditPlanRevision) {
    let project = catalog.get_asset(asset).unwrap().project_id;
    let cuts = (0..3)
        .map(|i| EditPlanOperation {
            operation_id: format!("cut-{i}"),
            position: i,
            asset_id: asset.into(),
            source_in_seconds: 0.5 + f64::from(i),
            source_out_seconds: 1.5 + f64::from(i),
            rationale: "Generated A/B mechanics only".into(),
            audio: None,
        })
        .collect::<Vec<_>>();
    let a = catalog
        .create_edit_plan_from_base(&project, &cuts, None, None, None)
        .unwrap();
    let mut changed = cuts;
    changed[1].source_out_seconds -= 1.0 / 60.0;
    let b = catalog
        .create_edit_plan_from_base(&project, &changed, None, Some(&a.revision_id), None)
        .unwrap();
    (a, b)
}
fn request(a: &EditPlanRevision, b: &EditPlanRevision) -> ShortComparisonRequest {
    ShortComparisonRequest {
        project_id: a.project_id.clone(),
        intent: "Compare the generated one-source-frame endpoint change with surrounding sound"
            .into(),
        max_height: 144,
        variants: [("A", a), ("B", b)]
            .into_iter()
            .map(|(name, plan)| ComparisonVariantRequest {
                name: name.into(),
                revision_id: plan.revision_id.clone(),
                scope: PreviewExcerptScope {
                    first_operation_id: "cut-0".into(),
                    last_operation_id: "cut-2".into(),
                },
            })
            .collect(),
    }
}
fn preference(comparison: &ShortComparison, b: &EditPlanRevision) -> ComparisonPreferenceRequest {
    ComparisonPreferenceRequest {
        comparison_id: comparison.id.clone(),
        preferred_variant: "B".into(),
        expected_preferred_revision_id: b.revision_id.clone(),
        expected_active_revision_id: "none".into(),
        decision_owner: RangeDecisionOwner::Human,
        rationale: "Explicit synthetic human-input fixture, not a real listening acceptance".into(),
        retain_operation_ids: vec!["cut-1".into()],
        create_draft: true,
    }
}

#[test]
fn one_frame_alternatives_keep_actual_mappings_reuse_neighbors_and_record_human_choices_without_activation()
 {
    let (_dir, catalog, asset) = fixture();
    let (a, b) = plans(&catalog, &asset);
    let comparison = catalog
        .create_short_comparison(&request(&a, &b), &|| false, &mut |_, _| {})
        .unwrap();
    assert_eq!(comparison.state, "complete");
    assert_eq!(comparison.variants.len(), 2);
    let variants = &comparison.variants;
    assert_eq!(
        variants[0].validation.cuts[1].output_frame_count,
        variants[1].validation.cuts[1].output_frame_count + 1
    );
    // B's final picture frame boundary rounds before its authored movie end.
    // Keep the full source-sound tail rather than clipping it to picture PTS.
    // Container duration is rounded on its millisecond clock; the decoded
    // non-silent tail below checks retained sound independently of that header.
    assert!(variants[1].preview.duration_seconds >= b.duration_seconds() - 0.001);
    let tail = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-ss"])
        .arg((b.duration_seconds() - 0.006).to_string())
        .arg("-i")
        .arg(&variants[1].preview.output_path)
        .args([
            "-t", "0.003", "-map", "0:a:0", "-ar", "48000", "-ac", "1", "-f", "f32le", "-",
        ])
        .output()
        .unwrap();
    assert!(tail.status.success());
    let samples = tail
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()) as f64)
        .collect::<Vec<_>>();
    let rms = (samples.iter().map(|s| s * s).sum::<f64>() / samples.len().max(1) as f64).sqrt();
    assert!(
        rms > 0.02,
        "the final retained source sound must remain audible beyond the rounded last picture frame: {rms}"
    );
    assert_eq!(
        variants[1].preview.segment_counts.as_ref().unwrap().reused,
        2
    );
    assert!(std::path::Path::new(&comparison.player.as_ref().unwrap().path).is_file());
    assert_eq!(
        catalog
            .short_comparison_status(&comparison.id, true)
            .unwrap()
            .current_for_request,
        Some(true)
    );
    let choice = catalog
        .prefer_short_comparison(&preference(&comparison, &b))
        .unwrap();
    assert!(choice.draft_failure.is_none(), "{:?}", choice.draft_failure);
    let draft_id = choice.draft_revision_id.as_deref().unwrap();
    let draft = catalog.edit_plan_revision(draft_id).unwrap();
    assert_eq!(
        draft.parent_revision_id.as_deref(),
        Some(b.revision_id.as_str())
    );
    assert_eq!(draft.operations, b.operations);
    assert_eq!(choice.note_ids.len(), 2);
    assert_eq!(
        catalog
            .comparison_choice(&choice.choice_id)
            .unwrap()
            .draft_revision_id,
        choice.draft_revision_id
    );
    assert_eq!(
        catalog
            .apply_comparison_choice(&choice.choice_id, "none")
            .unwrap()
            .draft_revision_id,
        choice.draft_revision_id
    );
    assert!(catalog.active_edit_plan(&a.project_id).unwrap().is_none());
    let note = catalog
        .review_feedback(&choice.note_ids[1], draft_id)
        .unwrap();
    assert!(note.note.evidence_refs.contains(&comparison.id));
    assert!(!note.target_status.requires_mapping);
    assert!(
        catalog
            .validate_edit_plan(&a.revision_id)
            .unwrap()
            .findings
            .iter()
            .any(|f| f.code == "edit_plan_human_choice_violation"),
        "accepted B endpoint must remain an explicit human constraint"
    );
    let read_only = Catalog::open_read_only_existing(catalog.database_path()).unwrap();
    assert_eq!(
        read_only
            .short_comparison_status(&comparison.id, false)
            .unwrap()
            .current_for_request,
        None
    );
}

#[test]
fn comparison_stale_media_and_active_guards_do_not_record_a_fake_preference_or_overwrite_newer_work()
 {
    let (_dir, catalog, asset) = fixture();
    let (a, b) = plans(&catalog, &asset);
    let comparison = catalog
        .create_short_comparison(&request(&a, &b), &|| false, &mut |_, _| {})
        .unwrap();
    catalog
        .activate_edit_plan_revision(&a.revision_id, None)
        .unwrap();
    assert!(
        catalog
            .prefer_short_comparison(&preference(&comparison, &b))
            .is_err()
    );
    assert_eq!(
        catalog
            .review_feedback_report(&a.project_id, &b.revision_id, 0, 20)
            .unwrap()
            .items
            .len(),
        0
    );
    let mut pref = preference(&comparison, &b);
    pref.expected_active_revision_id = a.revision_id.clone();
    pref.create_draft = false;
    pref.retain_operation_ids.clear();
    let choice = catalog.prefer_short_comparison(&pref).unwrap();
    assert!(choice.draft_revision_id.is_none());
    assert!(
        catalog
            .apply_comparison_choice(&choice.choice_id, "none")
            .is_err()
    );
    assert_eq!(
        catalog
            .active_edit_plan(&a.project_id)
            .unwrap()
            .unwrap()
            .revision_id,
        a.revision_id
    );
    std::fs::write(
        &comparison.variants[1].preview.output_path,
        b"damaged generated alternative",
    )
    .unwrap();
    assert_eq!(
        catalog
            .short_comparison_status(&comparison.id, true)
            .unwrap()
            .current_for_request,
        Some(false)
    );
    assert!(
        catalog
            .apply_comparison_choice(&choice.choice_id, &a.revision_id)
            .is_err()
    );
}

#[test]
fn comparison_cancellation_preserves_completed_work_and_never_publishes_an_incomplete_player() {
    let (_dir, catalog, asset) = fixture();
    let (a, b) = plans(&catalog, &asset);
    let stopped = Cell::new(false);
    let comparison = catalog
        .create_short_comparison(&request(&a, &b), &|| stopped.get(), &mut |_, event| {
            if matches!(event, PreviewProgress::Finished { .. }) {
                stopped.set(true);
            }
        })
        .unwrap();
    assert_eq!(comparison.state, "cancelled");
    assert!(comparison.player.is_none());
    assert!(comparison.pending_artifact_id.is_some());
    assert!(
        catalog
            .preview_artifacts(&a.revision_id, 0, 20)
            .unwrap()
            .items
            .iter()
            .any(|p| p.state == PreviewArtifactState::Complete)
    );
    assert_eq!(
        catalog
            .short_comparison_status(&comparison.id, false)
            .unwrap()
            .current_for_request,
        Some(false)
    );
    assert!(catalog.active_edit_plan(&a.project_id).unwrap().is_none());
    let mut bad = request(&a, &b);
    bad.variants[1] = bad.variants[0].clone();
    bad.variants[1].name = "renamed same scope".into();
    assert!(
        catalog
            .create_short_comparison(&bad, &|| false, &mut |_, _| {})
            .is_err()
    );
}

#[test]
fn replacement_order_and_split_audio_variants_have_distinct_actual_context() {
    let (_dir, catalog, asset) = fixture();
    let (a, b) = plans(&catalog, &asset);
    let mut audio = a.operations.clone();
    for cut in &mut audio {
        cut.audio = Some(EditPlanAudio::Silent);
    }
    audio[1].audio = Some(EditPlanAudio::Source(crate::SourceAudio {
        version: 1,
        asset_id: asset.clone(),
        source_in_seconds: 1.25,
        source_out_seconds: 2.75,
        timeline_start_seconds: 0.75,
        sync_offset_seconds: 0.0,
        gain_db: 0.0,
        fade_in_seconds: 0.1,
        fade_out_seconds: 0.1,
    }));
    let c = catalog
        .create_edit_plan_from_base(&a.project_id, &audio, None, Some(&a.revision_id), None)
        .unwrap();
    let mut reordered = a.operations.clone();
    reordered.swap(0, 2);
    for (position, cut) in reordered.iter_mut().enumerate() {
        cut.position = position as u32;
    }
    reordered[1].source_in_seconds = 0.0;
    reordered[1].source_out_seconds = 1.0;
    let d = catalog
        .create_edit_plan_from_base(&a.project_id, &reordered, None, Some(&a.revision_id), None)
        .unwrap();
    let mut req = request(&a, &b);
    req.variants.push(ComparisonVariantRequest {
        name: "J/L sound".into(),
        revision_id: c.revision_id.clone(),
        scope: PreviewExcerptScope {
            first_operation_id: "cut-0".into(),
            last_operation_id: "cut-2".into(),
        },
    });
    req.variants.push(ComparisonVariantRequest {
        name: "Reorder and replace".into(),
        revision_id: d.revision_id.clone(),
        scope: PreviewExcerptScope {
            first_operation_id: "cut-2".into(),
            last_operation_id: "cut-0".into(),
        },
    });
    let comparison = catalog
        .create_short_comparison(&req, &|| false, &mut |_, _| {})
        .unwrap();
    assert_eq!(comparison.state, "complete");
    assert_eq!(comparison.variants.len(), 4);
    assert_eq!(comparison.variants[2].audio_mappings.len(), 1);
    assert_eq!(
        comparison.variants[2].audio_mappings[0]["operationId"],
        "cut-1"
    );
    assert_eq!(
        comparison.variants[2].audio_mappings[0]["excerptInSamples48k"],
        36000
    );
    assert_eq!(
        comparison.variants[3].validation.cuts[0].operation_id,
        "cut-2"
    );
    assert_eq!(
        comparison.variants[3].validation.cuts[1].source_in_seconds,
        0.0
    );
    assert_eq!(
        catalog
            .short_comparison_status(&comparison.id, true)
            .unwrap()
            .current_for_request,
        Some(true)
    );
}
