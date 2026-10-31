use super::*;
use crate::{EditPlanOperation, PreviewExcerptRequest, PreviewExcerptScope, PreviewRequest};
use std::{fs, process::Command};

fn cut_plan(catalog: &Catalog, asset: &str) -> EditPlanRevision {
    let project = catalog.get_asset(asset).unwrap().project_id;
    catalog
        .create_edit_plan_from_base(
            &project,
            &[EditPlanOperation {
                operation_id: "coverage-cut".into(),
                position: 0,
                asset_id: asset.into(),
                source_in_seconds: 0.5,
                source_out_seconds: 1.5,
                rationale: "generated fixture".into(),
                audio: None,
            }],
            None,
            None,
            None,
        )
        .unwrap()
}

fn request(
    plan: &EditPlanRevision,
    asset: &str,
    artifact: &str,
    explicit: bool,
    method: ReviewCoverageMethod,
    intervals: Vec<ReviewCoverageInterval>,
) -> RecordReviewCoverageRequest {
    RecordReviewCoverageRequest {
        project_id: plan.project_id.clone(),
        revision_id: plan.revision_id.clone(),
        operation_id: "coverage-cut".into(),
        artifact_id: artifact.into(),
        actor: if explicit {
            ReviewDecisionActor::Human
        } else {
            ReviewDecisionActor::Agent
        },
        method,
        explicit_human_playback: explicit,
        evidence_refs: vec![format!("fixture:{asset}")],
        uncertainty: None,
        intervals,
    }
}

#[test]
fn sampled_and_machine_evidence_never_close_human_continuous_gaps() {
    let (_dir, catalog, asset) = crate::editplan::inspection::tests::fixture();
    let plan = cut_plan(&catalog, &asset);
    let preview = catalog
        .render_preview(
            &PreviewRequest {
                revision_id: plan.revision_id.clone(),
                max_height: 144,
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap();
    let artifact = preview.artifact_id.unwrap();
    let verified = catalog.preview_artifact_status(&artifact, true).unwrap();
    assert_eq!(verified.observed_state, PreviewArtifactState::Complete);
    let validation = verified.artifact.validation.as_ref().unwrap();
    let mapping = &validation.cuts[0];
    let movie_seconds = |pts: i64| {
        (pts - validation.movie_origin_pts) as f64 * validation.movie_time_base_numerator as f64
            / validation.movie_time_base_denominator as f64
    };
    let cut_start = movie_seconds(mapping.movie_in_pts);
    let cut_end = movie_seconds(mapping.movie_out_pts);
    let quarter = cut_start + (cut_end - cut_start) * 0.25;
    let three_quarters = cut_start + (cut_end - cut_start) * 0.75;
    let edge = |a, b, modality, coverage| ReviewCoverageInterval {
        modality,
        movie_start_seconds: a,
        movie_end_seconds: b,
        coverage,
    };
    let human = request(
        &plan,
        &asset,
        &artifact,
        true,
        ReviewCoverageMethod::HumanPlayback,
        vec![
            edge(
                cut_start,
                quarter,
                ReviewCoverageModality::Picture,
                ReviewCoverageKind::Continuous,
            ),
            edge(
                quarter,
                cut_end,
                ReviewCoverageModality::Picture,
                ReviewCoverageKind::Sampled,
            ),
            edge(
                cut_start,
                cut_end,
                ReviewCoverageModality::Listening,
                ReviewCoverageKind::Continuous,
            ),
        ],
    );
    let stored = catalog.record_review_coverage(&human).unwrap();
    assert_eq!(stored.intervals.len(), 3);
    let machine = request(
        &plan,
        &asset,
        &artifact,
        false,
        ReviewCoverageMethod::SparseSampling,
        vec![edge(
            quarter,
            three_quarters,
            ReviewCoverageModality::Listening,
            ReviewCoverageKind::Continuous,
        )],
    );
    catalog.record_review_coverage(&machine).unwrap();
    let report = catalog
        .review_coverage_report(&plan.revision_id, true)
        .unwrap();
    assert!(report.verified);
    let cut = &report.cuts[0];
    assert!(cut.picture.continuous_covered_seconds > 0.0);
    assert!(cut.picture.sampled_seconds > 0.0);
    assert!(
        !cut.picture.gaps.is_empty(),
        "sampled spans must not close continuous picture gaps"
    );
    assert!(
        cut.listening.gaps.is_empty(),
        "explicit continuous human listening should cover the cut"
    );
    assert!(report.total_gaps > 0);
    let history = catalog
        .review_coverage_records(&plan.revision_id, 0, 10)
        .unwrap();
    assert_eq!(history.total, 2);
    assert_eq!(history.items.len(), 2);
}

#[test]
fn non_human_methods_cannot_claim_explicit_human_playback() {
    let (_dir, catalog, asset) = crate::editplan::inspection::tests::fixture();
    let plan = cut_plan(&catalog, &asset);
    let preview = catalog
        .render_preview(
            &PreviewRequest {
                revision_id: plan.revision_id.clone(),
                max_height: 144,
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap();
    let artifact = preview.artifact_id.unwrap();
    let verified = catalog.preview_artifact_status(&artifact, true).unwrap();
    let validation = verified.artifact.validation.as_ref().unwrap();
    let mapping = &validation.cuts[0];
    let start = (mapping.movie_in_pts - validation.movie_origin_pts) as f64
        * validation.movie_time_base_numerator as f64
        / validation.movie_time_base_denominator as f64;
    let end = (mapping.movie_out_pts - validation.movie_origin_pts) as f64
        * validation.movie_time_base_numerator as f64
        / validation.movie_time_base_denominator as f64;
    let mut req = request(
        &plan,
        &asset,
        &artifact,
        true,
        ReviewCoverageMethod::AgentReview,
        vec![ReviewCoverageInterval {
            modality: ReviewCoverageModality::Picture,
            movie_start_seconds: start,
            movie_end_seconds: start + (end - start) * 0.5,
            coverage: ReviewCoverageKind::Continuous,
        }],
    );
    assert!(catalog.record_review_coverage(&req).is_err());
    req.actor = ReviewDecisionActor::Agent;
    req.explicit_human_playback = false;
    assert!(catalog.record_review_coverage(&req).is_ok());
}

#[test]
fn range_union_caps_gaps_and_does_not_double_count_overlap() {
    let report = modality_report(
        0.0,
        4.0,
        &[(0.0, 2.0), (1.0, 3.0)],
        &[(2.0, 3.0), (2.5, 4.0)],
    );
    assert_eq!(report.continuous_covered_seconds, 3.0);
    assert_eq!(report.sampled_seconds, 2.0);
    assert_eq!(
        report.gaps,
        vec![ReviewCoverageGap {
            movie_start_seconds: 3.0,
            movie_end_seconds: 4.0
        }]
    );
}

#[test]
fn normalized_dependency_allows_a_prefix_shift_but_rejects_neighbor_change() {
    let cut = |operation_id: &str, position| EditPlanOperation {
        operation_id: operation_id.into(),
        position,
        asset_id: format!("asset-{operation_id}"),
        source_in_seconds: 1.0,
        source_out_seconds: 2.0,
        rationale: "fixture".into(),
        audio: None,
    };
    let revision = |revision_id: &str, operations| EditPlanRevision {
        revision_id: revision_id.into(),
        project_id: "p".into(),
        script_revision_id: None,
        parent_revision_id: None,
        active: false,
        operations,
        created_at: "now".into(),
    };
    // Review C well inside the sequence so the 100 ms audio-context window
    // has the same local neighbors before and after the prefix shift. A
    // review at B would genuinely change at the original timeline boundary:
    // the old window is clipped at zero while the shifted one includes the
    // prefix's final 100 ms.
    let original = revision(
        "r1",
        vec![cut("a", 0), cut("b", 1), cut("c", 2), cut("d", 3)],
    );
    let shifted = revision(
        "r2",
        vec![
            cut("prefix", 0),
            cut("a", 1),
            cut("b", 2),
            cut("c", 3),
            cut("d", 4),
        ],
    );
    let original_target = normalized_target(&original, "c").unwrap();
    let shifted_target = normalized_target(&shifted, "c").unwrap();
    assert!(super::super::feedback::same_target(
        &original_target,
        &shifted_target
    ));
    let changed = revision(
        "r3",
        vec![
            cut("prefix", 0),
            cut("a", 1),
            cut("b", 2),
            cut("c", 3),
            cut("different", 4),
        ],
    );
    let changed_target = normalized_target(&changed, "c").unwrap();
    assert!(!super::super::feedback::same_target(
        &original_target,
        &changed_target
    ));
}

#[test]
fn different_excerpt_mappings_report_one_global_preview_clock() {
    let (_dir, catalog, asset) = crate::editplan::inspection::tests::fixture();
    let project = catalog.get_asset(&asset).unwrap().project_id;
    let operations = (0..3)
        .map(|position| EditPlanOperation {
            operation_id: format!("clock-{position}"),
            position,
            asset_id: asset.clone(),
            source_in_seconds: 0.5 + f64::from(position),
            source_out_seconds: 1.5 + f64::from(position),
            rationale: "generated fixture".into(),
            audio: None,
        })
        .collect::<Vec<_>>();
    let plan = catalog
        .create_edit_plan_from_base(&project, &operations, None, None, None)
        .unwrap();
    let render = |first: &str, last: &str| {
        catalog
            .render_preview_excerpt(
                &PreviewExcerptRequest {
                    revision_id: plan.revision_id.clone(),
                    max_height: 144,
                    scope: PreviewExcerptScope {
                        first_operation_id: first.into(),
                        last_operation_id: last.into(),
                    },
                },
                &mut |_| {},
                &|| false,
            )
            .unwrap()
    };
    render("clock-0", "clock-1");
    let after_first = catalog
        .review_coverage_report(&plan.revision_id, false)
        .unwrap();
    render("clock-1", "clock-2");
    let after_second = catalog
        .review_coverage_report(&plan.revision_id, false)
        .unwrap();
    let global_second_start = 30.0 * 1001.0 / 30000.0;
    let global_third_start = 60.0 * 1001.0 / 30000.0;
    assert!((after_first.cuts[1].movie_start_seconds - global_second_start).abs() < 1e-6);
    assert!(
        (after_first.cuts[1].movie_start_seconds - after_second.cuts[1].movie_start_seconds).abs()
            < 1e-6
    );
    assert!((after_second.cuts[2].movie_start_seconds - global_third_start).abs() < 1e-6);
    assert_eq!(
        after_second.cuts[2].mapping,
        "verifiedExcerptRemappedToFullClock"
    );
}

#[test]
fn coverage_carries_after_different_source_prefix_with_stable_context() {
    let (dir, catalog, source_a) = crate::editplan::inspection::tests::fixture();
    let project = catalog.get_asset(&source_a).unwrap().project_id;
    let second_root = dir.path().join("prefix-source");
    fs::create_dir(&second_root).unwrap();
    let second_path = second_root.join("prefix.mp4");
    let encoded = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&catalog.get_asset(&source_a).unwrap().path)
        .args([
            "-map",
            "0",
            "-c",
            "copy",
            "-metadata",
            "comment=coverage-prefix",
            "-y",
        ])
        .arg(&second_path)
        .output()
        .unwrap();
    assert!(
        encoded.status.success(),
        "{}",
        String::from_utf8_lossy(&encoded.stderr)
    );
    assert_eq!(
        catalog
            .ingest_folder(&project, &second_root)
            .unwrap()
            .imported,
        1
    );
    let source_b = catalog
        .list_assets(&project)
        .unwrap()
        .into_iter()
        .find(|asset| asset.filename == "prefix.mp4")
        .unwrap()
        .id;
    assert_ne!(source_a, source_b);
    let op =
        |operation_id: &str, position, asset_id: &str, source_in, source_out| EditPlanOperation {
            operation_id: operation_id.into(),
            position,
            asset_id: asset_id.into(),
            source_in_seconds: source_in,
            source_out_seconds: source_out,
            rationale: "generated fixture".into(),
            audio: None,
        };
    let original_ops = vec![
        op("lead", 0, &source_b, 0.2, 0.5),
        op("before", 1, &source_b, 0.8, 1.2),
        op("middle", 2, &source_a, 0.5, 1.5),
        op("after", 3, &source_b, 1.8, 2.2),
    ];
    let original = catalog
        .create_edit_plan_from_base(&project, &original_ops, None, None, None)
        .unwrap();
    let viewed = catalog
        .render_preview(
            &PreviewRequest {
                revision_id: original.revision_id.clone(),
                max_height: 144,
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap();
    let artifact_id = viewed.artifact_id.unwrap();
    let verified = catalog.preview_artifact_status(&artifact_id, true).unwrap();
    let validation = verified.artifact.validation.as_ref().unwrap();
    let mapping = validation
        .cuts
        .iter()
        .find(|cut| cut.operation_id == "middle")
        .unwrap();
    let seconds = |pts: i64| {
        (pts - validation.movie_origin_pts) as f64 * validation.movie_time_base_numerator as f64
            / validation.movie_time_base_denominator as f64
    };
    let start = seconds(mapping.movie_in_pts);
    let end = seconds(mapping.movie_out_pts);
    catalog
        .record_review_coverage(&RecordReviewCoverageRequest {
            project_id: project.clone(),
            revision_id: original.revision_id.clone(),
            operation_id: "middle".into(),
            artifact_id,
            actor: ReviewDecisionActor::Human,
            method: ReviewCoverageMethod::HumanPlayback,
            explicit_human_playback: true,
            evidence_refs: vec!["fixture:watched-middle".into()],
            uncertainty: None,
            intervals: vec![
                ReviewCoverageInterval {
                    modality: ReviewCoverageModality::Picture,
                    movie_start_seconds: start,
                    movie_end_seconds: end,
                    coverage: ReviewCoverageKind::Continuous,
                },
                ReviewCoverageInterval {
                    modality: ReviewCoverageModality::Listening,
                    movie_start_seconds: start,
                    movie_end_seconds: end,
                    coverage: ReviewCoverageKind::Continuous,
                },
            ],
        })
        .unwrap();
    let mut prefixed = vec![op("prefix", 0, &source_b, 2.5, 2.7)];
    prefixed.extend(
        original_ops
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, mut cut)| {
                cut.position = (index + 1) as u32;
                cut
            }),
    );
    let revision = catalog
        .create_edit_plan_from_base(&project, &prefixed, None, Some(&original.revision_id), None)
        .unwrap();
    catalog
        .render_preview(
            &PreviewRequest {
                revision_id: revision.revision_id.clone(),
                max_height: 144,
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap();
    let report = catalog
        .review_coverage_report(&revision.revision_id, false)
        .unwrap();
    let middle = report
        .cuts
        .iter()
        .find(|cut| cut.operation_id == "middle")
        .unwrap();
    assert!(
        middle.picture.gaps.is_empty(),
        "unchanged middle picture interval should carry across a different-source prefix"
    );
    assert!(
        middle.listening.gaps.is_empty(),
        "unchanged contextual listening interval should carry across a different-source prefix"
    );
    assert_eq!(middle.mapping, "verifiedFullPreviewClock");
}

#[test]
fn coverage_rows_follow_authorized_project_and_revision_deletion() {
    let (_dir, catalog, _asset) = crate::editplan::inspection::tests::fixture();
    let connection = catalog.connection().unwrap();
    let mut statement = connection
        .prepare("PRAGMA foreign_key_list(edit_review_coverage)")
        .unwrap();
    let actions = statement
        .query_map([], |row| row.get::<_, String>(6))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(actions.len(), 2);
    assert!(actions.iter().all(|action| action == "CASCADE"));
}
