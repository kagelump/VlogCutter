//! Public generated-fixture acceptance for chronology-safe edit-plan placement.

use rcdvlog_core::{
    Catalog, EditPlanAudio, EditPlanOperation, EditPlanPatch, EditPlanPatchInput, EditPlanRevision,
    EditPlanStoryPolicy, OtioExportRequest, PatchAnchor, PreviewRequest, ProjectAssetAddedPayload,
    ProtectedStoryGroup, SourceAudio, StoryChronologyException, StoryCutIdentity, StoryDecision,
    StoryDecisionActor, StoryExceptionScope, StoryOpeningTeaser, StoryOrderEdge,
};
use std::fs;
use std::process::Command;
use tempfile::{TempDir, tempdir};

struct Fixture {
    catalog: Catalog,
    project_id: String,
    directory: TempDir,
}

fn fixture(times: &[(&str, Option<&str>)]) -> Fixture {
    let directory = tempdir().unwrap();
    let media = directory.path().join("generated media");
    fs::create_dir_all(&media).unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog
        .create_project("Generated chronology fixture")
        .unwrap();
    for (index, (id, captured_at)) in times.iter().enumerate() {
        let filename = format!("PUBLIC_{index:04}.MP4");
        let path = media.join(&filename);
        fs::write(&path, b"generated fixture bytes, not video").unwrap();
        catalog
            .add_portable_project_asset(
                &project.id,
                ProjectAssetAddedPayload {
                    id: (*id).to_string(),
                    fingerprint: format!("fingerprint_{id}"),
                    filename,
                    size_bytes: 1024,
                    duration_seconds: 60.0,
                    frame_rate: 30.0,
                    width: 1920,
                    height: 1080,
                    codec: "h264".to_string(),
                    camera: "GeneratedCamera".to_string(),
                    captured_at_raw: captured_at.map(str::to_string),
                    timestamp_candidates: None,
                    best_time: None,
                    metadata_rotation_degrees: 0,
                    default_rotation_degrees: 0,
                    created_at: "2026-11-01T00:00:00.000Z".to_string(),
                },
            )
            .unwrap();
        rusqlite::Connection::open(directory.path().join("catalog.sqlite3"))
            .unwrap()
            .execute(
                "UPDATE assets SET path=?2 WHERE id=?1",
                rusqlite::params![id, path.to_string_lossy()],
            )
            .unwrap();
    }
    Fixture {
        catalog,
        project_id: project.id,
        directory,
    }
}

fn operation(id: &str, asset: &str, start: f64, end: f64) -> EditPlanOperation {
    EditPlanOperation {
        operation_id: id.into(),
        position: 0,
        asset_id: asset.into(),
        source_in_seconds: start,
        source_out_seconds: end,
        rationale: format!("generated {id}"),
        audio: None,
    }
}

fn cut(id: &str, asset: &str, start: f64, end: f64) -> StoryCutIdentity {
    StoryCutIdentity {
        operation_id: id.into(),
        asset_id: asset.into(),
        source_in_seconds: start,
        source_out_seconds: end,
    }
}

fn decision(actor: StoryDecisionActor, rationale: &str) -> StoryDecision {
    StoryDecision {
        actor,
        rationale: rationale.into(),
        decision_id: None,
    }
}

fn teaser_policy(operation_id: &str, asset_id: &str) -> EditPlanStoryPolicy {
    EditPlanStoryPolicy {
        version: 1,
        revision_decision: None,
        opening_teasers: vec![StoryOpeningTeaser {
            operation_id: operation_id.into(),
            asset_id: asset_id.into(),
            source_in_seconds: 0.0,
            source_out_seconds: 2.0,
            rationale: "Intentional opening teaser".into(),
            decision: decision(StoryDecisionActor::Human, "Reviewed teaser"),
            chronology_dependency: String::new(),
        }],
        protected_groups: vec![],
        exceptions: vec![],
    }
}

fn patch(
    edits: Vec<EditPlanPatch>,
    story_policy: Option<EditPlanStoryPolicy>,
) -> EditPlanPatchInput {
    EditPlanPatchInput {
        version: 1,
        name: "generated chronology patch".into(),
        edits,
        story_policy,
    }
}

fn generate_av(path: &std::path::Path) {
    let output = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "3",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "could not generate public media fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn revision_count(fixture: &Fixture) -> usize {
    fixture
        .catalog
        .edit_plan_revisions(&fixture.project_id)
        .unwrap()
        .len()
}

fn story_exception(
    exception_id: &str,
    cuts: Vec<StoryCutIdentity>,
    edges: Vec<(&str, &str)>,
) -> StoryChronologyException {
    StoryChronologyException {
        exception_id: exception_id.into(),
        rationale: "Generated fixture explicitly chooses this story edge".into(),
        decision: decision(StoryDecisionActor::Agent, "Scoped editorial decision"),
        scope: StoryExceptionScope {
            cuts,
            edges: edges
                .into_iter()
                .map(|(earlier, later)| StoryOrderEdge {
                    earlier_operation_id: earlier.into(),
                    later_operation_id: later.into(),
                })
                .collect(),
            chronology_dependency: String::new(),
        },
    }
}

#[test]
fn opening_teaser_stays_first_while_an_earlier_terminal_shot_lands_before_boarding() {
    let fixture = fixture(&[
        ("teaser", Some("2026-11-01T00:20:00Z")),
        ("boarding", Some("2026-11-01T01:00:00Z")),
        ("terminal", Some("2026-11-01T00:30:00Z")),
        ("hotel", Some("2026-11-01T02:20:00Z")),
    ]);
    let base = fixture
        .catalog
        .create_edit_plan_revision_with_story_policy(
            &fixture.project_id,
            &[
                operation("teaser", "teaser", 0.0, 2.0),
                operation("boarding", "boarding", 0.0, 2.0),
                operation("hotel", "hotel", 0.0, 2.0),
            ],
            None,
            None,
            Some(&teaser_policy("teaser", "teaser")),
        )
        .unwrap();
    let result = fixture
        .catalog
        .patch_edit_plan(
            &fixture.project_id,
            &base.revision_id,
            None,
            &patch(
                vec![EditPlanPatch::Insert {
                    anchor: None,
                    operation: operation("terminal", "terminal", 0.0, 2.0),
                }],
                None,
            ),
        )
        .unwrap();
    let order: Vec<&str> = result
        .revision
        .operations
        .iter()
        .map(|cut| cut.asset_id.as_str())
        .collect();
    assert_eq!(order, vec!["teaser", "terminal", "boarding", "hotel"]);
    assert_eq!(
        result
            .revision
            .story_policy
            .as_ref()
            .unwrap()
            .opening_teasers[0]
            .operation_id,
        "teaser"
    );
}

#[test]
fn shuffled_batch_additions_are_placed_in_corrected_source_order() {
    let fixture = fixture(&[
        ("approach", Some("2026-11-01T02:00:00Z")),
        ("arrival", Some("2026-11-01T02:10:00Z")),
        ("hotel", Some("2026-11-01T02:20:00Z")),
        ("city", Some("2026-11-01T02:30:00Z")),
    ]);
    let base = fixture
        .catalog
        .create_edit_plan_revision(
            &fixture.project_id,
            &[
                operation("approach", "approach", 0.0, 2.0),
                operation("hotel", "hotel", 0.0, 2.0),
            ],
            None,
            None,
        )
        .unwrap();
    let first = fixture
        .catalog
        .patch_edit_plan(
            &fixture.project_id,
            &base.revision_id,
            None,
            &patch(
                vec![
                    EditPlanPatch::Insert {
                        anchor: None,
                        operation: operation("arrival", "arrival", 0.0, 2.0),
                    },
                    EditPlanPatch::Insert {
                        anchor: None,
                        operation: operation("city", "city", 0.0, 2.0),
                    },
                ],
                None,
            ),
        )
        .unwrap();
    let second = fixture
        .catalog
        .patch_edit_plan(
            &fixture.project_id,
            &base.revision_id,
            None,
            &patch(
                vec![
                    EditPlanPatch::Insert {
                        anchor: None,
                        operation: operation("city", "city", 0.0, 2.0),
                    },
                    EditPlanPatch::Insert {
                        anchor: None,
                        operation: operation("arrival", "arrival", 0.0, 2.0),
                    },
                ],
                None,
            ),
        )
        .unwrap();
    let order = |revision: &EditPlanRevision| {
        revision
            .operations
            .iter()
            .map(|cut| cut.asset_id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        order(&first.revision),
        vec!["approach", "arrival", "hotel", "city"]
    );
    assert_eq!(order(&first.revision), order(&second.revision));
}

#[test]
fn wrong_anchor_is_atomic_and_a_narrow_agent_exception_is_inspectable() {
    let fixture = fixture(&[
        ("boarding", Some("2026-11-01T01:00:00Z")),
        ("terminal", Some("2026-11-01T00:30:00Z")),
        ("hotel", Some("2026-11-01T02:20:00Z")),
    ]);
    let base = fixture
        .catalog
        .create_edit_plan_revision(
            &fixture.project_id,
            &[
                operation("boarding", "boarding", 0.0, 2.0),
                operation("hotel", "hotel", 0.0, 2.0),
            ],
            None,
            None,
        )
        .unwrap();
    let before = revision_count(&fixture);
    let error = fixture
        .catalog
        .patch_edit_plan(
            &fixture.project_id,
            &base.revision_id,
            None,
            &patch(
                vec![EditPlanPatch::Insert {
                    anchor: Some(PatchAnchor::After {
                        operation_id: "boarding".into(),
                    }),
                    operation: operation("terminal", "terminal", 0.0, 2.0),
                }],
                None,
            ),
        )
        .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<rcdvlog_core::EditPlanError>()
            .unwrap()
            .code(),
        "edit_plan_chronology_conflict"
    );
    assert_eq!(revision_count(&fixture), before, "failure must be atomic");

    let policy = EditPlanStoryPolicy {
        version: 1,
        revision_decision: None,
        opening_teasers: vec![],
        protected_groups: vec![],
        exceptions: vec![story_exception(
            "terminal-after-boarding",
            vec![
                cut("boarding", "boarding", 0.0, 2.0),
                cut("terminal", "terminal", 0.0, 2.0),
            ],
            vec![("boarding", "terminal")],
        )],
    };
    let result = fixture
        .catalog
        .patch_edit_plan(
            &fixture.project_id,
            &base.revision_id,
            None,
            &patch(
                vec![EditPlanPatch::Insert {
                    anchor: Some(PatchAnchor::After {
                        operation_id: "boarding".into(),
                    }),
                    operation: operation("terminal", "terminal", 0.0, 2.0),
                }],
                Some(policy),
            ),
        )
        .unwrap();
    let persisted = result.revision.story_policy.as_ref().unwrap();
    let exception = &persisted.exceptions[0];
    assert_eq!(exception.decision.actor, StoryDecisionActor::Agent);
    assert!(!exception.decision.human_approved());
    assert_eq!(
        fixture
            .catalog
            .edit_plan_story_policy(&result.revision.revision_id)
            .unwrap()
            .unwrap()
            .exceptions[0]
            .exception_id,
        "terminal-after-boarding"
    );

    let before = revision_count(&fixture);
    let bad_scope = EditPlanStoryPolicy {
        version: 1,
        revision_decision: None,
        opening_teasers: vec![],
        protected_groups: vec![],
        exceptions: vec![story_exception(
            "scope-drifted",
            vec![
                cut("boarding", "boarding", 0.0, 2.0),
                cut("terminal", "terminal", 10.0, 12.0),
            ],
            vec![("boarding", "terminal")],
        )],
    };
    let error = fixture
        .catalog
        .patch_edit_plan(
            &fixture.project_id,
            &base.revision_id,
            None,
            &patch(
                vec![EditPlanPatch::Insert {
                    anchor: Some(PatchAnchor::After {
                        operation_id: "boarding".into(),
                    }),
                    operation: operation("terminal", "terminal", 0.0, 2.0),
                }],
                Some(bad_scope),
            ),
        )
        .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<rcdvlog_core::EditPlanError>()
            .unwrap()
            .code(),
        "edit_plan_story_scope_stale"
    );
    assert_eq!(revision_count(&fixture), before);
}

#[test]
fn changed_clock_facts_invalidate_an_existing_exception() {
    let fixture = fixture(&[
        ("boarding", Some("2026-11-01T01:00:00Z")),
        ("terminal", Some("2026-11-01T00:30:00Z")),
    ]);
    let policy = EditPlanStoryPolicy {
        version: 1,
        revision_decision: None,
        opening_teasers: vec![],
        protected_groups: vec![],
        exceptions: vec![story_exception(
            "clock-bound",
            vec![
                cut("boarding", "boarding", 0.0, 2.0),
                cut("terminal", "terminal", 0.0, 2.0),
            ],
            vec![("boarding", "terminal")],
        )],
    };
    let revision = fixture
        .catalog
        .create_edit_plan_revision_with_story_policy(
            &fixture.project_id,
            &[
                operation("boarding", "boarding", 0.0, 2.0),
                operation("terminal", "terminal", 0.0, 2.0),
            ],
            None,
            None,
            Some(&policy),
        )
        .unwrap();
    rusqlite::Connection::open(fixture.directory.path().join("catalog.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE assets SET captured_at_raw='2026-11-01T01:15:00Z' WHERE id='boarding'",
            [],
        )
        .unwrap();
    let report = fixture
        .catalog
        .validate_edit_plan(&revision.revision_id)
        .unwrap();
    assert!(!report.renderable());
    assert!(
        report
            .findings
            .iter()
            .any(|finding| { finding.code == "edit_plan_story_scope_stale" })
    );
}

#[test]
fn protected_groups_cannot_be_split_and_stale_group_identity_is_refused() {
    let fixture = fixture(&[
        ("setup", Some("2026-11-01T02:00:00Z")),
        ("payoff", Some("2026-11-01T02:10:00Z")),
        ("middle", Some("2026-11-01T02:05:00Z")),
        ("before", Some("2026-11-01T01:00:00Z")),
        ("closing", Some("2026-11-01T03:00:00Z")),
    ]);
    let policy = EditPlanStoryPolicy {
        version: 1,
        revision_decision: None,
        opening_teasers: vec![],
        protected_groups: vec![ProtectedStoryGroup {
            group_id: "setup-payoff".into(),
            cuts: vec![
                cut("setup", "setup", 0.0, 2.0),
                cut("payoff", "payoff", 0.0, 2.0),
            ],
            rationale: "The setup and payoff are one reviewed contiguous unit".into(),
            decision: decision(StoryDecisionActor::Human, "Keep the payoff adjacent"),
            chronology_dependency: String::new(),
        }],
        exceptions: vec![],
    };
    let base = fixture
        .catalog
        .create_edit_plan_revision_with_story_policy(
            &fixture.project_id,
            &[
                operation("setup", "setup", 0.0, 2.0),
                operation("payoff", "payoff", 0.0, 2.0),
                operation("closing", "closing", 0.0, 2.0),
            ],
            None,
            None,
            Some(&policy),
        )
        .unwrap();
    let before = revision_count(&fixture);
    let error = fixture
        .catalog
        .patch_edit_plan(
            &fixture.project_id,
            &base.revision_id,
            None,
            &patch(
                vec![EditPlanPatch::Insert {
                    anchor: None,
                    operation: operation("middle", "middle", 0.0, 2.0),
                }],
                None,
            ),
        )
        .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<rcdvlog_core::EditPlanError>()
            .unwrap()
            .code(),
        "edit_plan_chronology_conflict"
    );
    assert_eq!(revision_count(&fixture), before);

    let result = fixture
        .catalog
        .patch_edit_plan(
            &fixture.project_id,
            &base.revision_id,
            None,
            &patch(
                vec![EditPlanPatch::Insert {
                    anchor: None,
                    operation: operation("before", "before", 0.0, 2.0),
                }],
                None,
            ),
        )
        .unwrap();
    let order: Vec<&str> = result
        .revision
        .operations
        .iter()
        .map(|cut| cut.asset_id.as_str())
        .collect();
    assert_eq!(order, vec!["before", "setup", "payoff", "closing"]);

    let error = fixture
        .catalog
        .patch_edit_plan(
            &fixture.project_id,
            &base.revision_id,
            None,
            &patch(
                vec![EditPlanPatch::Reorder {
                    operation_id: "payoff".into(),
                    anchor: PatchAnchor::After {
                        operation_id: "closing".into(),
                    },
                }],
                None,
            ),
        )
        .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<rcdvlog_core::EditPlanError>()
            .unwrap()
            .code(),
        "edit_plan_story_policy_invalid"
    );
}

#[test]
fn unknown_and_tied_times_are_bounded_conflicts_not_guesses() {
    let fixture = fixture(&[
        ("known", Some("2026-11-01T01:00:00Z")),
        ("unknown", None),
        ("tied_a", Some("2026-11-01T02:00:00Z")),
        ("tied_b", Some("2026-11-01T02:00:00Z")),
    ]);
    let base = fixture
        .catalog
        .create_edit_plan_revision(
            &fixture.project_id,
            &[
                operation("known", "known", 0.0, 2.0),
                operation("tied_a", "tied_a", 0.0, 2.0),
            ],
            None,
            None,
        )
        .unwrap();
    let error = fixture
        .catalog
        .patch_edit_plan(
            &fixture.project_id,
            &base.revision_id,
            None,
            &patch(
                vec![EditPlanPatch::Insert {
                    anchor: None,
                    operation: operation("unknown", "unknown", 0.0, 2.0),
                }],
                None,
            ),
        )
        .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<rcdvlog_core::EditPlanError>()
            .unwrap()
            .code(),
        "edit_plan_chronology_unresolved"
    );
    let error = fixture
        .catalog
        .patch_edit_plan(
            &fixture.project_id,
            &base.revision_id,
            None,
            &patch(
                vec![EditPlanPatch::Insert {
                    anchor: None,
                    operation: operation("tied_b", "tied_b", 0.0, 2.0),
                }],
                None,
            ),
        )
        .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<rcdvlog_core::EditPlanError>()
            .unwrap()
            .code(),
        "edit_plan_chronology_ambiguous"
    );
}

#[test]
fn automatic_insertion_retimes_existing_independent_source_audio() {
    let fixture = fixture(&[
        ("known", Some("2026-11-01T01:00:00Z")),
        ("earlier", Some("2026-11-01T00:30:00Z")),
    ]);
    generate_av(
        &fixture
            .directory
            .path()
            .join("generated media/PUBLIC_0000.MP4"),
    );
    generate_av(
        &fixture
            .directory
            .path()
            .join("generated media/PUBLIC_0001.MP4"),
    );
    let mut second = operation("second", "known", 1.5, 2.5);
    second.audio = Some(EditPlanAudio::Source(SourceAudio {
        version: 1,
        asset_id: "known".into(),
        source_in_seconds: 1.5,
        source_out_seconds: 2.5,
        timeline_start_seconds: 1.0,
        sync_offset_seconds: 0.0,
        gain_db: 0.0,
        fade_in_seconds: 0.0,
        fade_out_seconds: 0.0,
    }));
    let base = fixture
        .catalog
        .create_edit_plan_revision(
            &fixture.project_id,
            &[operation("first", "known", 0.5, 1.5), second],
            None,
            None,
        )
        .unwrap();
    let result = fixture
        .catalog
        .patch_edit_plan(
            &fixture.project_id,
            &base.revision_id,
            None,
            &patch(
                vec![EditPlanPatch::Insert {
                    anchor: None,
                    operation: operation("earlier", "earlier", 0.0, 1.0),
                }],
                None,
            ),
        )
        .unwrap();
    let order: Vec<&str> = result
        .revision
        .operations
        .iter()
        .map(|cut| cut.operation_id.as_str())
        .collect();
    assert_eq!(order, vec!["earlier", "first", "second"]);
    let Some(EditPlanAudio::Source(audio)) = &result.revision.operations[2].audio else {
        panic!("retained source audio was lost")
    };
    assert!((audio.timeline_start_seconds - 2.0).abs() < 1e-9);
    assert_eq!(audio.source_in_seconds, 1.5);
}

#[test]
fn full_plan_creation_is_inspectable_but_activation_and_replace_cannot_bypass_chronology() {
    let fixture = fixture(&[
        ("boarding", Some("2026-11-01T01:00:00Z")),
        ("terminal", Some("2026-11-01T00:30:00Z")),
        ("hotel", Some("2026-11-01T02:20:00Z")),
    ]);
    // Whole-plan creation is an immutable draft. The same validator blocks its
    // activation, preview and export paths rather than letting them bypass it.
    let invalid = fixture
        .catalog
        .create_edit_plan_revision(
            &fixture.project_id,
            &[
                operation("boarding", "boarding", 0.0, 2.0),
                operation("terminal", "terminal", 0.0, 2.0),
            ],
            None,
            None,
        )
        .unwrap();
    let report = fixture
        .catalog
        .validate_edit_plan(&invalid.revision_id)
        .unwrap();
    assert!(!report.renderable());
    let error = fixture
        .catalog
        .activate_edit_plan_revision(&invalid.revision_id, None)
        .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<rcdvlog_core::EditPlanError>()
            .unwrap()
            .code(),
        "edit_plan_chronology_conflict"
    );
    let error = fixture
        .catalog
        .render_preview(
            &PreviewRequest {
                revision_id: invalid.revision_id.clone(),
                max_height: 144,
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap_err();
    let preview_error = format!("{error:#}").to_lowercase();
    assert!(
        preview_error.contains("chronology") || preview_error.contains("validation"),
        "unexpected preview refusal: {preview_error}"
    );
    let otio_path = fixture.directory.path().join("invalid-export.otio");
    assert!(
        fixture
            .catalog
            .export_otio(&OtioExportRequest {
                revision_id: invalid.revision_id.clone(),
                output_path: otio_path.to_string_lossy().into_owned(),
                expected_existing_digest: None,
                timeline_name: "invalid".into(),
            })
            .is_err()
    );
    assert!(
        !otio_path.exists(),
        "finishing path must not write invalid order"
    );

    let base = fixture
        .catalog
        .create_edit_plan_revision(
            &fixture.project_id,
            &[
                operation("boarding", "boarding", 0.0, 2.0),
                operation("hotel", "hotel", 0.0, 2.0),
            ],
            None,
            None,
        )
        .unwrap();
    let error = fixture
        .catalog
        .patch_edit_plan(
            &fixture.project_id,
            &base.revision_id,
            None,
            &patch(
                vec![EditPlanPatch::Replace {
                    operation_id: "hotel".into(),
                    asset_id: "terminal".into(),
                    source_in_seconds: 0.0,
                    source_out_seconds: 2.0,
                    audio: None,
                    rationale: "Generated replacement".into(),
                }],
                None,
            ),
        )
        .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<rcdvlog_core::EditPlanError>()
            .unwrap()
            .code(),
        "edit_plan_chronology_conflict"
    );
}

#[test]
fn generated_media_preview_uses_safe_automatic_order_and_stays_synchronized() {
    let fixture = fixture(&[
        ("later", Some("2026-11-01T02:00:00Z")),
        ("earlier", Some("2026-11-01T01:00:00Z")),
    ]);
    generate_av(
        &fixture
            .directory
            .path()
            .join("generated media/PUBLIC_0000.MP4"),
    );
    generate_av(
        &fixture
            .directory
            .path()
            .join("generated media/PUBLIC_0001.MP4"),
    );
    let base = fixture
        .catalog
        .create_edit_plan_revision(
            &fixture.project_id,
            &[operation("later", "later", 0.0, 2.0)],
            None,
            None,
        )
        .unwrap();
    let result = fixture
        .catalog
        .patch_edit_plan(
            &fixture.project_id,
            &base.revision_id,
            None,
            &patch(
                vec![EditPlanPatch::Insert {
                    anchor: None,
                    operation: operation("earlier", "earlier", 0.0, 2.0),
                }],
                None,
            ),
        )
        .unwrap();
    assert_eq!(result.revision.operations[0].asset_id, "earlier");
    let preview = fixture
        .catalog
        .render_preview(
            &PreviewRequest {
                revision_id: result.revision.revision_id,
                max_height: 144,
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap();
    assert!((preview.duration_seconds - 4.0).abs() < 0.05);
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(&preview.output_path)
        .output()
        .unwrap();
    assert!(output.status.success());
    let duration: f64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap();
    assert!((duration - 4.0).abs() < 0.1);
    let artifact = preview
        .artifact_id
        .as_deref()
        .expect("managed preview identity");
    let before = fixture
        .catalog
        .preview_artifact_status(artifact, true)
        .unwrap();
    assert!(before.verification.unwrap().constraints_match);
    rusqlite::Connection::open(fixture.directory.path().join("catalog.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE assets SET captured_at_raw='2026-11-01T03:00:00Z' WHERE id='later'",
            [],
        )
        .unwrap();
    let after = fixture
        .catalog
        .preview_artifact_status(artifact, true)
        .unwrap();
    let verification = after.verification.unwrap();
    assert!(verification.plan_renderable);
    assert!(
        !verification.constraints_match,
        "changed source clocks must invalidate an old finishing receipt even when order remains forward"
    );
}

#[test]
fn repeated_teaser_footage_is_declared_exactly_and_a_trim_invalidates_its_scope() {
    let fixture = fixture(&[
        ("reused", Some("2026-11-01T01:00:00Z")),
        ("later", Some("2026-11-01T02:00:00Z")),
    ]);
    let policy = EditPlanStoryPolicy {
        version: 1,
        revision_decision: None,
        opening_teasers: vec![StoryOpeningTeaser {
            operation_id: "teaser".into(),
            asset_id: "reused".into(),
            source_in_seconds: 0.0,
            source_out_seconds: 2.0,
            rationale: "Opening teaser intentionally reuses the main payoff source".into(),
            decision: decision(StoryDecisionActor::Human, "Reviewed repeated footage"),
            chronology_dependency: String::new(),
        }],
        protected_groups: vec![],
        exceptions: vec![],
    };
    let base = fixture
        .catalog
        .create_edit_plan_revision_with_story_policy(
            &fixture.project_id,
            &[
                operation("teaser", "reused", 0.0, 2.0),
                operation("main", "reused", 0.0, 2.0),
                operation("later", "later", 0.0, 2.0),
            ],
            None,
            None,
            Some(&policy),
        )
        .unwrap();
    assert!(
        fixture
            .catalog
            .validate_edit_plan(&base.revision_id)
            .unwrap()
            .renderable()
    );

    let before = revision_count(&fixture);
    let error = fixture
        .catalog
        .patch_edit_plan(
            &fixture.project_id,
            &base.revision_id,
            None,
            &patch(
                vec![EditPlanPatch::Trim {
                    operation_id: "teaser".into(),
                    source_in_seconds: 0.25,
                    source_out_seconds: 2.0,
                    rationale: "Changed teaser range".into(),
                }],
                None,
            ),
        )
        .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<rcdvlog_core::EditPlanError>()
            .unwrap()
            .code(),
        "edit_plan_story_scope_stale"
    );
    assert_eq!(revision_count(&fixture), before);
}

#[test]
fn a_reviewed_rearranged_group_keeps_its_documented_internal_order_and_remains_atomic() {
    let fixture = fixture(&[
        ("hotel_first", Some("2026-11-01T02:00:00Z")),
        ("approach_second", Some("2026-11-01T01:00:00Z")),
        ("closing", Some("2026-11-01T03:00:00Z")),
        ("middle", Some("2026-11-01T01:30:00Z")),
    ]);
    let policy = EditPlanStoryPolicy {
        version: 1,
        revision_decision: None,
        opening_teasers: vec![],
        protected_groups: vec![ProtectedStoryGroup {
            group_id: "reviewed-camera-choice".into(),
            cuts: vec![
                cut("hotel_first", "hotel_first", 0.0, 2.0),
                cut("approach_second", "approach_second", 0.0, 2.0),
            ],
            rationale: "The editor chose this deliberate two-shot reveal order".into(),
            decision: decision(StoryDecisionActor::Human, "Reviewed rearrangement"),
            chronology_dependency: String::new(),
        }],
        exceptions: vec![],
    };
    let base = fixture
        .catalog
        .create_edit_plan_revision_with_story_policy(
            &fixture.project_id,
            &[
                operation("hotel_first", "hotel_first", 0.0, 2.0),
                operation("approach_second", "approach_second", 0.0, 2.0),
                operation("closing", "closing", 0.0, 2.0),
            ],
            None,
            None,
            Some(&policy),
        )
        .unwrap();
    assert!(
        fixture
            .catalog
            .validate_edit_plan(&base.revision_id)
            .unwrap()
            .renderable()
    );
    let before = revision_count(&fixture);
    let error = fixture
        .catalog
        .patch_edit_plan(
            &fixture.project_id,
            &base.revision_id,
            None,
            &patch(
                vec![EditPlanPatch::Insert {
                    anchor: None,
                    operation: operation("middle", "middle", 0.0, 2.0),
                }],
                None,
            ),
        )
        .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<rcdvlog_core::EditPlanError>()
            .unwrap()
            .code(),
        "edit_plan_chronology_conflict"
    );
    assert_eq!(revision_count(&fixture), before);
}

#[test]
fn story_policy_survives_baseline_roundtrip_and_legacy_revision_reads_are_unapproved() {
    let fixture = fixture(&[
        ("boarding", Some("2026-11-01T01:00:00Z")),
        ("terminal", Some("2026-11-01T00:30:00Z")),
    ]);
    let policy = EditPlanStoryPolicy {
        version: 1,
        revision_decision: None,
        opening_teasers: vec![],
        protected_groups: vec![],
        exceptions: vec![story_exception(
            "roundtrip",
            vec![
                cut("boarding", "boarding", 0.0, 2.0),
                cut("terminal", "terminal", 0.0, 2.0),
            ],
            vec![("boarding", "terminal")],
        )],
    };
    let revision = fixture
        .catalog
        .create_edit_plan_revision_with_story_policy(
            &fixture.project_id,
            &[
                operation("boarding", "boarding", 0.0, 2.0),
                operation("terminal", "terminal", 0.0, 2.0),
            ],
            None,
            None,
            Some(&policy),
        )
        .unwrap();
    let restored = Catalog::import_project_baseline(
        fixture.directory.path().join("restored.sqlite3"),
        &fixture
            .catalog
            .export_project_baseline(&fixture.project_id)
            .unwrap(),
        "generated-restored-node",
        1,
        "generated fixture",
    )
    .unwrap();
    let loaded = restored.edit_plan_revision(&revision.revision_id).unwrap();
    assert_eq!(loaded.story_policy, revision.story_policy);
    assert_eq!(
        loaded.story_policy.as_ref().unwrap().exceptions[0].exception_id,
        "roundtrip"
    );

    // A legacy row has NULL policy and must not be interpreted as approval.
    rusqlite::Connection::open(fixture.directory.path().join("catalog.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE edit_plan_revisions SET story_policy_json=NULL WHERE revision_id=?1",
            [&revision.revision_id],
        )
        .unwrap();
    let legacy = fixture
        .catalog
        .edit_plan_revision(&revision.revision_id)
        .unwrap();
    assert!(legacy.story_policy.is_none());
}

#[test]
fn forward_journey_has_no_spurious_tied_time_findings() {
    let f = fixture(&[
        ("early", Some("2026-11-01T01:00:00Z")),
        ("late", Some("2026-11-01T02:00:00Z")),
    ]);
    let plan = f
        .catalog
        .create_edit_plan_revision(
            &f.project_id,
            &[
                operation("early", "early", 0.0, 2.0),
                operation("late", "late", 0.0, 2.0),
            ],
            None,
            None,
        )
        .unwrap();
    let report = f.catalog.validate_edit_plan(&plan.revision_id).unwrap();
    assert!(
        report
            .findings
            .iter()
            .all(|finding| !finding.code.starts_with("edit_plan_chronology")),
        "{report:?}"
    );
}

#[test]
fn retiring_a_protected_group_requires_a_recorded_revision_decision() {
    let f = fixture(&[
        ("first", Some("2026-11-01T01:00:00Z")),
        ("insert", Some("2026-11-01T02:00:00Z")),
        ("last", Some("2026-11-01T03:00:00Z")),
    ]);
    let operations = [
        operation("first", "first", 0.0, 2.0),
        operation("last", "last", 0.0, 2.0),
    ];
    let policy = EditPlanStoryPolicy {
        protected_groups: vec![ProtectedStoryGroup {
            group_id: "two-shot-scene".into(),
            cuts: operations
                .iter()
                .map(StoryCutIdentity::from_operation)
                .collect(),
            rationale: "Preserve this scene".into(),
            decision: decision(StoryDecisionActor::Agent, "Provisional scene order"),
            chronology_dependency: String::new(),
        }],
        ..EditPlanStoryPolicy::default()
    };
    let base = f
        .catalog
        .create_edit_plan_revision_with_story_policy(
            &f.project_id,
            &operations,
            None,
            None,
            Some(&policy),
        )
        .unwrap();
    let edit = EditPlanPatch::Insert {
        anchor: Some(PatchAnchor::After {
            operation_id: "first".into(),
        }),
        operation: operation("insert", "insert", 0.0, 2.0),
    };
    let count = revision_count(&f);
    assert!(
        f.catalog
            .patch_edit_plan(
                &f.project_id,
                &base.revision_id,
                None,
                &patch(vec![edit.clone()], Some(EditPlanStoryPolicy::default()))
            )
            .is_err()
    );
    assert_eq!(revision_count(&f), count);
    let replacement = EditPlanStoryPolicy {
        revision_decision: Some(decision(
            StoryDecisionActor::Agent,
            "Replace the two-shot scene with this explicit three-shot progression",
        )),
        ..EditPlanStoryPolicy::default()
    };
    let result = f
        .catalog
        .patch_edit_plan(
            &f.project_id,
            &base.revision_id,
            None,
            &patch(vec![edit], Some(replacement)),
        )
        .unwrap();
    assert_eq!(result.revision.operations[1].operation_id, "insert");
    assert_eq!(
        result
            .revision
            .story_policy
            .unwrap()
            .revision_decision
            .unwrap()
            .actor,
        StoryDecisionActor::Agent
    );
}

#[test]
fn chronology_is_enforced_between_separate_protected_groups() {
    let f = fixture(&[
        ("early", Some("2026-11-01T01:00:00Z")),
        ("late", Some("2026-11-01T02:00:00Z")),
    ]);
    let policy = EditPlanStoryPolicy {
        protected_groups: ["late", "early"]
            .map(|name| ProtectedStoryGroup {
                group_id: name.into(),
                cuts: vec![cut(name, name, 0.0, 2.0)],
                rationale: "Protect this individual shot".into(),
                decision: decision(
                    StoryDecisionActor::Agent,
                    "Protect only this group's internal order",
                ),
                chronology_dependency: String::new(),
            })
            .to_vec(),
        ..EditPlanStoryPolicy::default()
    };
    let result = f.catalog.create_edit_plan_revision_with_story_policy(
        &f.project_id,
        &[
            operation("late", "late", 0.0, 2.0),
            operation("early", "early", 0.0, 2.0),
        ],
        None,
        None,
        Some(&policy),
    );
    assert!(
        result.is_err(),
        "protecting separate shots must not exempt the edge between them"
    );
}

#[test]
fn explicit_unknown_and_tied_anchors_require_scoped_decisions_too() {
    for unknown_time in [None, Some("2026-11-01T01:00:00Z")] {
        let f = fixture(&[
            ("known", Some("2026-11-01T01:00:00Z")),
            ("uncertain", unknown_time),
        ]);
        let base = f
            .catalog
            .create_edit_plan_revision(
                &f.project_id,
                &[operation("known", "known", 0.0, 2.0)],
                None,
                None,
            )
            .unwrap();
        let count = revision_count(&f);
        let result = f.catalog.patch_edit_plan(
            &f.project_id,
            &base.revision_id,
            None,
            &patch(
                vec![EditPlanPatch::Insert {
                    anchor: Some(PatchAnchor::End),
                    operation: operation("uncertain", "uncertain", 0.0, 2.0),
                }],
                None,
            ),
        );
        assert!(
            result.is_err(),
            "explicit anchors cannot bypass uncertainty: {result:?}"
        );
        assert_eq!(revision_count(&f), count);
        let draft = f
            .catalog
            .create_edit_plan_revision(
                &f.project_id,
                &[
                    operation("known", "known", 0.0, 2.0),
                    operation("uncertain", "uncertain", 0.0, 2.0),
                ],
                None,
                None,
            )
            .unwrap();
        assert!(
            !f.catalog
                .validate_edit_plan(&draft.revision_id)
                .unwrap()
                .renderable()
        );
        assert!(
            f.catalog
                .activate_edit_plan_revision(&draft.revision_id, None)
                .is_err()
        );
    }
}

#[test]
fn multiple_valid_slots_around_an_exception_require_an_explicit_choice() {
    let f = fixture(&[
        ("early", Some("2026-11-01T01:00:00Z")),
        ("middle", Some("2026-11-01T02:00:00Z")),
        ("late", Some("2026-11-01T03:00:00Z")),
    ]);
    let policy = EditPlanStoryPolicy {
        exceptions: vec![story_exception(
            "reverse",
            vec![
                cut("late", "late", 0.0, 2.0),
                cut("early", "early", 0.0, 2.0),
            ],
            vec![("late", "early")],
        )],
        ..EditPlanStoryPolicy::default()
    };
    let base = f
        .catalog
        .create_edit_plan_revision_with_story_policy(
            &f.project_id,
            &[
                operation("late", "late", 0.0, 2.0),
                operation("early", "early", 0.0, 2.0),
            ],
            None,
            None,
            Some(&policy),
        )
        .unwrap();
    let error = f
        .catalog
        .patch_edit_plan(
            &f.project_id,
            &base.revision_id,
            None,
            &patch(
                vec![EditPlanPatch::Insert {
                    anchor: None,
                    operation: operation("middle", "middle", 0.0, 2.0),
                }],
                None,
            ),
        )
        .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<rcdvlog_core::EditPlanError>()
            .unwrap()
            .code(),
        "edit_plan_chronology_ambiguous"
    );
    let explicit = f
        .catalog
        .patch_edit_plan(
            &f.project_id,
            &base.revision_id,
            None,
            &patch(
                vec![EditPlanPatch::Insert {
                    anchor: Some(PatchAnchor::Before {
                        operation_id: "late".into(),
                    }),
                    operation: operation("middle", "middle", 0.0, 2.0),
                }],
                None,
            ),
        )
        .unwrap();
    assert_eq!(explicit.revision.operations[0].operation_id, "middle");
}
