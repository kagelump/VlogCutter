use super::super::inspection::tests::fixture;
use super::*;
use crate::{
    EditPlanAudio, EditPlanOperation, InspectionPurpose, InspectionRepresentation, PreviewRequest,
    SourceAudio, SourceInspectionRequest,
};
use std::{fs, path::Path, process::Command};

fn decision(
    catalog: &Catalog,
    id: &str,
    start: f64,
    end: f64,
    kind: RangeDecision,
    media: RangeMedia,
    revision: u64,
) -> ConfirmSourceRangeRequest {
    ConfirmSourceRangeRequest {
        asset_id: id.into(),
        expected_revision: revision,
        expected_source_fingerprint: catalog.get_asset(id).unwrap().fingerprint,
        constraint_id: None,
        decision: kind,
        media,
        decision_owner: RangeDecisionOwner::Human,
        source_in_seconds: start,
        source_out_seconds: end,
        rationale: "Explicit synthetic human review decision".into(),
        inspected_evidence_id: None,
        overrides_evidence_ids: vec![],
    }
}
fn inspect(catalog: &Catalog, id: &str, audio: bool, transcript: bool) -> crate::SourceInspection {
    catalog
        .inspect_source_window(
            &SourceInspectionRequest {
                asset_id: id.into(),
                representation: InspectionRepresentation::Original,
                purpose: InspectionPurpose::InspectedBoundary,
                source_in_seconds: 0.5,
                source_out_seconds: 3.0,
                before_seconds: 0.0,
                after_seconds: 0.0,
                sample_count: 8,
                max_width: 160,
                source_times_seconds: vec![],
                exact_representation_pts: vec![],
                expected_representation_fingerprint: None,
                include_audio: audio,
                include_transcript: transcript,
            },
            &|| false,
        )
        .unwrap()
        .inspection
}
fn cut(id: &str, a: f64, b: f64) -> EditPlanOperation {
    EditPlanOperation {
        operation_id: "stable-cut".into(),
        position: 0,
        asset_id: id.into(),
        source_in_seconds: a,
        source_out_seconds: b,
        rationale: "Complete generated interval".into(),
        audio: None,
    }
}

#[test]
fn confirmed_ranges_are_human_owned_revision_guarded_portable_and_reversible() {
    let (dir, catalog, id) = fixture();
    let project = catalog.get_asset(&id).unwrap().project_id;
    let allowed = catalog
        .confirm_source_range(&decision(
            &catalog,
            &id,
            1.0,
            3.0,
            RangeDecision::Allow,
            RangeMedia::Both,
            0,
        ))
        .unwrap();
    assert_eq!(allowed.revision, 1);
    assert_eq!(allowed.feasible_picture_ranges.len(), 1);
    assert_eq!(
        allowed.feasible_picture_ranges[0],
        SourceTimeRange {
            source_in_seconds: 1.0,
            source_out_seconds: 3.0
        }
    );
    let mut excluded = decision(
        &catalog,
        &id,
        1.5,
        2.0,
        RangeDecision::Exclude,
        RangeMedia::Both,
        1,
    );
    excluded.overrides_evidence_ids = vec!["review_suspected_covered_range".into()];
    let reviewed = catalog.confirm_source_range(&excluded).unwrap();
    assert_eq!(reviewed.revision, 2);
    assert_eq!(reviewed.feasible_picture_ranges.len(), 2);
    assert_eq!(reviewed.feasible_picture_ranges[0].source_out_seconds, 1.5);
    assert_eq!(reviewed.feasible_picture_ranges[1].source_in_seconds, 2.0);
    let before = catalog.export_project_baseline(&project).unwrap();
    assert!(matches!(
        catalog
            .confirm_source_range(&excluded)
            .unwrap_err()
            .downcast_ref::<SourceRangeError>(),
        Some(SourceRangeError::StaleRevision {
            expected: 1,
            actual: 2
        })
    ));
    assert_eq!(
        catalog.export_project_baseline(&project).unwrap(),
        before,
        "stale decision must append no operation"
    );
    excluded.expected_revision = 2;
    excluded.expected_source_fingerprint = "other source".into();
    assert!(matches!(
        catalog
            .confirm_source_range(&excluded)
            .unwrap_err()
            .downcast_ref::<SourceRangeError>(),
        Some(SourceRangeError::StaleSource)
    ));
    let excluded_id = reviewed
        .items
        .iter()
        .find(|r| r.decision == RangeDecision::Exclude)
        .unwrap()
        .constraint_id
        .clone();
    // Baseline replay reconstructs constraints from typed operations, without
    // shipping local cache receipts or mutable media paths.
    let serialized = serde_json::to_string(&before.operations).unwrap();
    assert!(!serialized.contains(&dir.path().to_string_lossy().to_string()));
    let restored = Catalog::import_project_baseline(
        dir.path().join("peer/catalog.sqlite3"),
        &before,
        "node_range_peer",
        1,
        "baseline-ranges",
    )
    .unwrap();
    let peer = restored.source_range_constraints(&id, 0, 100).unwrap();
    assert_eq!(peer, reviewed);
    let revoked = catalog
        .revoke_source_range(
            &id,
            &excluded_id,
            2,
            "Human explicitly withdraws exclusion after playback",
        )
        .unwrap();
    assert_eq!(revoked.revision, 3);
    assert_eq!(revoked.total_items, 2);
    assert!(revoked.items.iter().any(|r| !r.active));
    assert_eq!(
        revoked.feasible_picture_ranges,
        allowed.feasible_picture_ranges
    );
    let path = catalog.database_path().to_path_buf();
    drop(catalog);
    let reopened = Catalog::open(path).unwrap();
    assert_eq!(
        reopened.source_range_constraints(&id, 0, 100).unwrap(),
        revoked
    );
    assert!(
        reopened
            .operation_diagnostics(&project)
            .unwrap()
            .unresolved_conflicts
            .is_empty()
    );
    let mut malformed = serde_json::to_value(decision(
        &reopened,
        &id,
        0.0,
        0.5,
        RangeDecision::Allow,
        RangeMedia::Picture,
        3,
    ))
    .unwrap();
    malformed["decisionOwner"] = serde_json::json!("model");
    assert!(serde_json::from_value::<ConfirmSourceRangeRequest>(malformed).is_err());
}

#[test]
fn picture_exclusion_can_preserve_covered_dialogue_but_audio_exclusion_blocks_it() {
    let (dir, catalog, id) = fixture();
    let asset = catalog.get_asset(&id).unwrap();
    let media = dir.path().join("broll");
    fs::create_dir(&media).unwrap();
    assert!(
        Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=blue:s=160x90:r=30:d=3",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p"
            ])
            .arg(media.join("cover.mp4"))
            .status()
            .unwrap()
            .success()
    );
    catalog.ingest_folder(&asset.project_id, &media).unwrap();
    let broll = catalog
        .list_assets(&asset.project_id)
        .unwrap()
        .into_iter()
        .find(|a| a.id != id)
        .unwrap();
    let picture = catalog
        .confirm_source_range(&decision(
            &catalog,
            &id,
            1.5,
            2.0,
            RangeDecision::Exclude,
            RangeMedia::Picture,
            0,
        ))
        .unwrap();
    assert_eq!(
        picture.feasible_audio_ranges,
        vec![SourceTimeRange {
            source_in_seconds: 0.0,
            source_out_seconds: 4.0
        }]
    );
    let mut a = cut(&id, 1.0, 1.5);
    a.audio = Some(EditPlanAudio::Source(SourceAudio {
        version: 1,
        asset_id: id.clone(),
        source_in_seconds: 1.0,
        source_out_seconds: 2.5,
        timeline_start_seconds: 0.0,
        sync_offset_seconds: 0.0,
        gain_db: 0.0,
        fade_in_seconds: 0.0,
        fade_out_seconds: 0.0,
    }));
    let mut b = cut(&broll.id, 0.0, 1.0);
    b.operation_id = "cover".into();
    b.position = 1;
    b.audio = Some(EditPlanAudio::Silent);
    let plan = catalog
        .create_edit_plan_from_base(&asset.project_id, &[a, b], None, None, None)
        .unwrap();
    assert!(
        catalog
            .validate_edit_plan(&plan.revision_id)
            .unwrap()
            .renderable()
    );
    catalog
        .confirm_source_range(&decision(
            &catalog,
            &id,
            1.6,
            1.8,
            RangeDecision::Exclude,
            RangeMedia::Audio,
            1,
        ))
        .unwrap();
    let invalid = catalog.validate_edit_plan(&plan.revision_id).unwrap();
    assert!(
        invalid
            .errors()
            .any(|f| f.code == "edit_plan_confirmed_range_violation"
                && f.message.starts_with("Audio"))
    );
    assert!(
        catalog
            .activate_edit_plan_revision(&plan.revision_id, None)
            .is_err()
    );
    let body = catalog.get_asset(&id).unwrap();
    assert!(!body.removed);
    assert_eq!(body.notes, asset.notes);
    assert_eq!(body.rotation_degrees, asset.rotation_degrees);
}

#[test]
fn inspected_confirmation_requires_current_exact_evidence_and_never_repairs_what_the_human_saw() {
    let (_dir, catalog, id) = fixture();
    let report = inspect(&catalog, &id, true, false);
    let mut request = decision(
        &catalog,
        &id,
        1.0,
        2.0,
        RangeDecision::Allow,
        RangeMedia::Both,
        0,
    );
    request.inspected_evidence_id = Some(report.evidence_id.clone());
    request.source_out_seconds = 3.5;
    assert!(catalog.confirm_source_range(&request).is_err());
    request.source_out_seconds = 2.0;
    fs::write(&report.frames[0].image.path, b"corrupt evidence").unwrap();
    assert!(matches!(
        catalog
            .confirm_source_range(&request)
            .unwrap_err()
            .downcast_ref::<crate::SourceInspectionError>(),
        Some(crate::SourceInspectionError::StaleEvidence)
    ));
    assert_eq!(
        fs::read(&report.frames[0].image.path).unwrap(),
        b"corrupt evidence"
    );
    assert_eq!(
        catalog
            .source_range_constraints(&id, 0, 100)
            .unwrap()
            .revision,
        0
    );
    // Explicitly regenerate evidence and review again; confirmation binds the
    // snapshot but never calls generation as a side effect of the decision.
    catalog
        .inspect_source_window(&report.request, &|| false)
        .unwrap();
    let confirmed = catalog.confirm_source_range(&request).unwrap();
    assert_eq!(confirmed.revision, 1);
    let provenance = confirmed.items[0].inspection.as_ref().unwrap();
    assert_eq!(provenance.evidence_id, report.evidence_id);
    assert_eq!(provenance.mapping.precision, "actualSourcePts");
    assert!(
        serde_json::to_string(provenance)
            .unwrap()
            .find(".jpg")
            .is_none()
    );
}

#[test]
fn trim_suggestions_never_extend_a_whole_utterance_into_exclusions() {
    use crate::analysis::{
        quality::DecodedSegment,
        store::{StoredTranscript, save_transcript},
    };
    let (_dir, catalog, id) = fixture();
    catalog.generate_proxy(&id).unwrap();
    let mut transcript = StoredTranscript {
        asset_id: id.clone(),
        recipe_key: crate::STT_RECIPE_KEY.into(),
        source_fingerprint: catalog.proxy_fingerprint(&id).unwrap(),
        status: crate::TranscriptStatus::Speech,
        outcome_reason: crate::TranscriptOutcomeReason::Accepted,
        reject_reason: String::new(),
        primary_language: "en".into(),
        speech_seconds: 1.0,
        window_count: 1,
        provider_segment_count: 1,
        mean_token_probability: Some(0.9),
        repetition_ratio: Some(0.0),
        segments: vec![DecodedSegment {
            start_ms: 1000,
            end_ms: 2000,
            language: "en".into(),
            text: "Synthetic segment timing, not word or naturalness evidence".into(),
            mean_token_probability: Some(0.9),
        }],
    };
    save_transcript(&catalog.connection().unwrap(), &transcript).unwrap();
    let mut request = SourceInspectionRequest {
        asset_id: id.clone(),
        representation: InspectionRepresentation::GeneratedProxy,
        purpose: InspectionPurpose::InspectedBoundary,
        source_in_seconds: 0.5,
        source_out_seconds: 3.0,
        before_seconds: 0.0,
        after_seconds: 0.0,
        sample_count: 8,
        max_width: 160,
        source_times_seconds: vec![],
        exact_representation_pts: vec![],
        expected_representation_fingerprint: None,
        include_audio: true,
        include_transcript: true,
    };
    let report = catalog
        .inspect_source_window(&request, &|| false)
        .unwrap()
        .inspection;
    let trim = SourceTrimRequest {
        asset_id: id.clone(),
        inspected_evidence_id: report.evidence_id.clone(),
        media: TrimMedia::Audio,
        source_in_seconds: 1.2,
        source_out_seconds: 1.4,
        prefer_utterances: true,
    };
    let whole = catalog.suggest_source_trim(&trim).unwrap();
    assert_eq!(
        whole.proposed_ranges,
        vec![SourceTimeRange {
            source_in_seconds: 1.0,
            source_out_seconds: 2.0
        }]
    );
    assert!(whole.listening_required);
    catalog
        .confirm_source_range(&decision(
            &catalog,
            &id,
            1.5,
            1.6,
            RangeDecision::Exclude,
            RangeMedia::Audio,
            0,
        ))
        .unwrap();
    let constrained = catalog.suggest_source_trim(&trim).unwrap();
    assert!(constrained.proposed_ranges.is_empty());
    assert!(
        constrained
            .conflicts
            .iter()
            .any(|c| c.contains("crosses an exclusion"))
    );
    assert_eq!(
        catalog
            .source_range_constraints(&id, 0, 100)
            .unwrap()
            .revision,
        1,
        "suggestions cannot mutate decisions"
    );
    transcript.source_fingerprint = "changed".into();
    save_transcript(&catalog.connection().unwrap(), &transcript).unwrap();
    assert!(
        catalog.suggest_source_trim(&trim).is_err(),
        "stale segment evidence cannot be reused"
    );
    request.include_transcript = false;
    let frames = catalog
        .inspect_source_window(&request, &|| false)
        .unwrap()
        .inspection;
    let no_text = catalog
        .suggest_source_trim(&SourceTrimRequest {
            inspected_evidence_id: frames.evidence_id,
            ..trim
        })
        .unwrap();
    assert!(
        no_text
            .conflicts
            .iter()
            .any(|c| c.contains("No current accepted utterance"))
    );
}

#[test]
fn new_constraint_during_render_prevents_publication_and_preserves_completed_preview() {
    let (_dir, catalog, id) = fixture();
    let asset = catalog.get_asset(&id).unwrap();
    let plan = catalog
        .create_edit_plan_from_base(&asset.project_id, &[cut(&id, 1.0, 2.0)], None, None, None)
        .unwrap();
    let request = PreviewRequest {
        revision_id: plan.revision_id,
        max_height: 180,
    };
    let prior = catalog
        .render_preview(&request, &mut |_| {}, &|| false)
        .unwrap();
    let old = fs::read(&prior.output_path).unwrap();
    let mut changed = false;
    let mut finished = false;
    let result = catalog.render_preview(
        &request,
        &mut |progress| {
            if matches!(progress, crate::PreviewProgress::ClipCompleted { .. }) && !changed {
                catalog
                    .confirm_source_range(&decision(
                        &catalog,
                        &id,
                        1.5,
                        1.6,
                        RangeDecision::Exclude,
                        RangeMedia::Both,
                        0,
                    ))
                    .unwrap();
                changed = true;
            }
            if matches!(progress, crate::PreviewProgress::Finished { .. }) {
                finished = true;
            }
        },
        &|| false,
    );
    assert!(changed);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("constraints changed")
    );
    assert!(!finished);
    assert_eq!(fs::read(&prior.output_path).unwrap(), old);
    assert!(Path::new(&prior.output_path).is_file());
    assert!(
        catalog
            .render_preview(&request, &mut |_| {}, &|| false)
            .is_err()
    );
}

#[test]
fn concurrent_confirmations_share_an_atomic_source_constraint_revision() {
    let (_dir, catalog, id) = fixture();
    let one = decision(
        &catalog,
        &id,
        0.0,
        1.0,
        RangeDecision::Allow,
        RangeMedia::Both,
        0,
    );
    let two = decision(
        &catalog,
        &id,
        2.0,
        3.0,
        RangeDecision::Allow,
        RangeMedia::Both,
        0,
    );
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            catalog.confirm_source_range(&one)
        });
        let b = scope.spawn(|| {
            barrier.wait();
            catalog.confirm_source_range(&two)
        });
        vec![a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| r.as_ref().err().is_some_and(|e| matches!(
                e.downcast_ref::<SourceRangeError>(),
                Some(SourceRangeError::StaleRevision { .. })
            )))
            .count(),
        1
    );
    let saved = catalog.source_range_constraints(&id, 0, 100).unwrap();
    assert_eq!(saved.revision, 1);
    assert_eq!(saved.total_items, 1);
}

#[test]
fn previously_quarantined_decisions_apply_after_upgrade_once_and_survive_unversioned_replay() {
    let (dir, catalog, id) = fixture();
    let project = catalog.get_asset(&id).unwrap().project_id;
    let peer_path = dir.path().join("old-peer.sqlite3");
    let peer = Catalog::import_project_baseline(
        &peer_path,
        &catalog.export_project_baseline(&project).unwrap(),
        "older_peer",
        1,
        "fixture",
    )
    .unwrap();
    catalog
        .confirm_source_range(&decision(
            &catalog,
            &id,
            0.5,
            1.0,
            RangeDecision::Exclude,
            RangeMedia::Both,
            0,
        ))
        .unwrap();
    let operation = catalog
        .list_operations(&project)
        .unwrap()
        .into_iter()
        .find(|op| op.operation_type == SOURCE_RANGE_CONFIRMED)
        .unwrap();
    let mut unknown = operation.clone();
    unknown.operation_type = "futureRangeType".into();
    assert_eq!(
        peer.apply_operation(&unknown).unwrap(),
        ApplyOperationResult::Quarantined
    );
    assert_eq!(
        peer.source_range_constraints(&id, 0, 100).unwrap().revision,
        0
    );
    // Test-only fixture for the upgrade seam: an older build retained this
    // exact operation without recognizing its type. Restoring its known name
    // simulates a new build recognizing it; product code never edits the log.
    peer.connection()
        .unwrap()
        .execute(
            "UPDATE project_operations SET operation_type=?1 WHERE operation_id=?2",
            params![SOURCE_RANGE_CONFIRMED, operation.operation_id],
        )
        .unwrap();
    drop(peer);
    let upgraded = Catalog::open(&peer_path).unwrap();
    let saved = upgraded.source_range_constraints(&id, 0, 100).unwrap();
    assert_eq!(saved.revision, 1);
    assert_eq!(saved.items.len(), 1);
    assert_eq!(saved.items[0].last_operation_id, operation.operation_id);
    upgraded
        .connection()
        .unwrap()
        .execute("DROP TABLE schema_migrations", [])
        .unwrap();
    drop(upgraded);
    let replayed = Catalog::open(&peer_path).unwrap();
    assert_eq!(
        replayed.source_range_constraints(&id, 0, 100).unwrap(),
        saved
    );
    assert!(
        replayed
            .operation_diagnostics(&project)
            .unwrap()
            .unresolved_conflicts
            .is_empty()
    );
    drop(replayed);
    assert_eq!(
        Catalog::open(peer_path)
            .unwrap()
            .source_range_constraints(&id, 0, 100)
            .unwrap(),
        saved
    );
}

#[test]
fn labeling_visibility_does_not_change_explicit_inspection_or_human_range_decisions() {
    let (_dir, catalog, id) = fixture();
    catalog.generate_proxy(&id).unwrap();
    let report = catalog
        .inspect_source_window(
            &SourceInspectionRequest {
                asset_id: id.clone(),
                representation: InspectionRepresentation::GeneratedProxy,
                purpose: InspectionPurpose::InspectedBoundary,
                source_in_seconds: 0.5,
                source_out_seconds: 3.0,
                before_seconds: 0.0,
                after_seconds: 0.0,
                sample_count: 4,
                max_width: 160,
                source_times_seconds: vec![],
                exact_representation_pts: vec![],
                expected_representation_fingerprint: None,
                include_audio: false,
                include_transcript: false,
            },
            &|| false,
        )
        .unwrap()
        .inspection;
    catalog.set_removed(&id, true).unwrap();
    let hidden = catalog.get_asset(&id).unwrap();
    assert!(hidden.removed);
    assert!(
        catalog.analysis_proxy_source(&hidden).is_err(),
        "preparation still skips removed assets"
    );
    assert!(
        catalog
            .inspect_source_window(&report.request, &|| false)
            .unwrap()
            .reused,
        "explicit source review is independent of labeling visibility"
    );
    let mut request = decision(
        &catalog,
        &id,
        1.0,
        2.0,
        RangeDecision::Allow,
        RangeMedia::Picture,
        0,
    );
    request.inspected_evidence_id = Some(report.evidence_id);
    let confirmed = catalog.confirm_source_range(&request).unwrap();
    assert_eq!(confirmed.revision, 1);
    assert!(catalog.get_asset(&id).unwrap().removed);
}
