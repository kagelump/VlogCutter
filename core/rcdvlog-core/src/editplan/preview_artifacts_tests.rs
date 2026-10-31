use super::*;
use crate::editplan::inspection::tests::fixture;
use crate::{AssetPatch, EditPlanOperation, PreviewProgress};
use std::{io::Write, time::Duration};

fn plan(catalog: &Catalog, asset: &str) -> EditPlanRevision {
    let project = catalog.get_asset(asset).unwrap().project_id;
    catalog
        .create_edit_plan_from_base(
            &project,
            &(0..2)
                .map(|position| EditPlanOperation {
                    operation_id: format!("stable-{position}"),
                    position,
                    asset_id: asset.into(),
                    source_in_seconds: 0.5 + f64::from(position),
                    source_out_seconds: 1.5 + f64::from(position),
                    rationale: "Generated media mechanics only".into(),
                    audio: None,
                })
                .collect::<Vec<_>>(),
            None,
            None,
            None,
        )
        .unwrap()
}
fn request(plan: &EditPlanRevision) -> PreviewRequest {
    PreviewRequest {
        revision_id: plan.revision_id.clone(),
        max_height: 144,
    }
}

#[test]
fn completed_previews_keep_unique_bytes_and_actual_decoded_picture_clocks() {
    let (_dir, catalog, asset) = fixture();
    let plan = plan(&catalog, &asset);
    let first = catalog
        .render_preview(&request(&plan), &mut |_| {}, &|| false)
        .unwrap();
    let id = first.artifact_id.as_deref().unwrap();
    let bytes = fs::read(&first.output_path).unwrap();
    let status = catalog.preview_artifact_status(id, false).unwrap();
    assert_eq!(status.observed_state, PreviewArtifactState::Complete);
    assert_eq!(
        status.current_for_request, None,
        "path presence is not fresh verification"
    );
    assert_eq!(
        status.active_revision_id, None,
        "a rendered draft is not activated"
    );
    let qa = status.artifact.validation.unwrap();
    assert!(qa.fully_decoded);
    assert_eq!(qa.cuts.len(), 2);
    assert_eq!(
        qa.cuts.iter().map(|c| c.output_frame_count).sum::<u64>(),
        qa.video_frame_count
    );
    assert_eq!(qa.duration_seconds, first.duration_seconds);
    assert_eq!(qa.audio_sample_rate, 48000);
    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_frames",
            "-show_entries",
            "frame=best_effort_timestamp,duration,pkt_duration",
            "-of",
            "json",
        ])
        .arg(&first.output_path)
        .output()
        .unwrap();
    assert!(probe.status.success());
    let decoded: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    let frames = decoded["frames"].as_array().unwrap();
    let boundary = frames[qa.cuts[0].output_frame_count as usize]["best_effort_timestamp"]
        .as_i64()
        .unwrap();
    let scale = |v: i64| {
        v * qa.video_time_base_numerator * qa.movie_time_base_denominator
            / qa.video_time_base_denominator
    };
    assert_eq!(qa.cuts[0].movie_out_pts, scale(boundary));
    assert_eq!(qa.cuts[1].movie_in_pts, scale(boundary));
    let last = frames.last().unwrap();
    let end = last["best_effort_timestamp"].as_i64().unwrap()
        + last["duration"]
            .as_i64()
            .or_else(|| last["pkt_duration"].as_i64())
            .unwrap();
    assert_eq!(qa.cuts[1].movie_out_pts, scale(end));
    assert_eq!(
        catalog
            .preview_artifact_status(id, true)
            .unwrap()
            .current_for_request,
        Some(true)
    );
    let second = catalog
        .render_preview(&request(&plan), &mut |_| {}, &|| false)
        .unwrap();
    assert_ne!(first.output_path, second.output_path);
    assert_ne!(first.artifact_id, second.artifact_id);
    assert_eq!(fs::read(&first.output_path).unwrap(), bytes);
    let old = catalog.preview_artifact_status(id, true).unwrap();
    assert!(!old.latest_attempt);
    assert_eq!(old.current_for_request, Some(true));
    assert!(old.verification.unwrap().output_matches);
    let page = catalog.preview_artifacts(&plan.revision_id, 0, 1).unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.next_offset, Some(1));
    assert_eq!(page.items[0].artifact_id, second.artifact_id.unwrap());
    assert_eq!(
        catalog
            .preview_artifacts(&plan.revision_id, 1, 1)
            .unwrap()
            .items[0]
            .artifact_id,
        id
    );
    assert!(
        catalog
            .cancel_preview_artifact(id)
            .unwrap_err()
            .downcast_ref::<PreviewArtifactError>()
            .is_some()
    );
}

#[test]
fn current_verification_rejects_changed_source_orientation_and_output() {
    let (_dir, catalog, asset_id) = fixture();
    let plan = plan(&catalog, &asset_id);
    let result = catalog
        .render_preview(&request(&plan), &mut |_| {}, &|| false)
        .unwrap();
    let id = result.artifact_id.as_deref().unwrap();
    let asset = catalog.get_asset(&asset_id).unwrap();
    catalog
        .update_asset(
            &asset_id,
            &AssetPatch {
                notes: asset.notes,
                labels: asset.labels,
                removed: asset.removed,
                rotation_degrees: (asset.rotation_degrees + 180) % 360,
                expected_revision: Some(asset.revision),
            },
        )
        .unwrap();
    let rotated = catalog.preview_artifact_status(id, true).unwrap();
    assert_eq!(rotated.current_for_request, Some(false));
    assert!(!rotated.verification.unwrap().inputs_match);
    let rotated_asset = catalog.get_asset(&asset_id).unwrap();
    catalog
        .update_asset(
            &asset_id,
            &AssetPatch {
                notes: rotated_asset.notes,
                labels: rotated_asset.labels,
                removed: rotated_asset.removed,
                rotation_degrees: asset.rotation_degrees,
                expected_revision: Some(rotated_asset.revision),
            },
        )
        .unwrap();
    assert_eq!(
        catalog
            .preview_artifact_status(id, true)
            .unwrap()
            .current_for_request,
        Some(true)
    );
    // Ingest uses a sampled identity. A full selected-representation hash must
    // still notice changed bytes even while that catalog identity is unchanged.
    fs::OpenOptions::new()
        .append(true)
        .open(&asset.path)
        .unwrap()
        .write_all(b"changed-fixture-tail")
        .unwrap();
    let changed = catalog.preview_artifact_status(id, true).unwrap();
    assert!(!changed.verification.unwrap().inputs_match);
    fs::write(&result.output_path, b"truncated generated output").unwrap();
    let corrupt = catalog.preview_artifact_status(id, true).unwrap();
    assert!(corrupt.file_present);
    assert_eq!(corrupt.current_for_request, Some(false));
    assert!(!corrupt.verification.unwrap().output_matches);
    fs::remove_file(&result.output_path).unwrap();
    let missing = catalog.preview_artifact_status(id, false).unwrap();
    assert!(!missing.file_present);
    assert_eq!(missing.current_for_request, Some(false));
}

#[test]
fn external_cancellation_is_durable_and_preserves_the_prior_movie() {
    let (_dir, catalog, asset) = fixture();
    let plan = plan(&catalog, &asset);
    let request = request(&plan);
    let prior = catalog
        .render_preview(&request, &mut |_| {}, &|| false)
        .unwrap();
    let bytes = fs::read(&prior.output_path).unwrap();
    let mut cancelled_id = None;
    let mut terminal_sequence = None;
    let error = catalog
        .render_preview(
            &request,
            &mut |event| match event {
                PreviewProgress::ClipCompleted {
                    artifact_id: Some(id),
                    ..
                } if cancelled_id.is_none() => {
                    assert_ne!(Some(&id), prior.artifact_id.as_ref());
                    let status = catalog.cancel_preview_artifact(&id).unwrap();
                    assert!(status.artifact.cancel_requested);
                    cancelled_id = Some(id);
                }
                PreviewProgress::Cancelled { sequence, .. } => terminal_sequence = sequence,
                _ => {}
            },
            &|| false,
        )
        .unwrap_err();
    assert!(error.to_string().contains("cancel"));
    let id = cancelled_id.unwrap();
    let status = catalog.wait_preview_artifact(&id, Duration::ZERO).unwrap();
    assert_eq!(status.observed_state, PreviewArtifactState::Cancelled);
    assert_eq!(Some(status.artifact.sequence), terminal_sequence);
    assert!(status.artifact.result.is_none());
    assert!(status.artifact.output_relative_path.is_none());
    assert_eq!(fs::read(&prior.output_path).unwrap(), bytes);
    assert!(
        catalog
            .render_preview(&request, &mut |_| {}, &|| false)
            .is_ok()
    );
}

#[test]
fn wait_timeout_keeps_the_original_claim_and_lost_writer_is_explicit() {
    let (_dir, catalog, asset) = fixture();
    let plan = plan(&catalog, &asset);
    let request = request(&plan);
    let owner = claim(&catalog, &plan).unwrap();
    let attempt = PreviewAttempt::begin(&catalog, &plan, &request, &owner, None).unwrap();
    let id = attempt.artifact_id.clone();
    assert!(matches!(
        catalog
            .wait_preview_artifact(&id, Duration::ZERO)
            .unwrap_err()
            .downcast_ref::<PreviewArtifactError>(),
        Some(PreviewArtifactError::ObservationTimedOut { .. })
    ));
    assert_eq!(
        catalog
            .preview_artifact_status(&id, false)
            .unwrap()
            .observed_state,
        PreviewArtifactState::Running
    );
    assert!(matches!(
        claim(&catalog, &plan)
            .err()
            .unwrap()
            .downcast_ref::<PreviewArtifactError>(),
        Some(PreviewArtifactError::Busy { .. })
    ));
    drop(attempt);
    drop(owner); // Simulates abrupt loss without a false completed receipt.
    assert_eq!(
        catalog
            .wait_preview_artifact(&id, Duration::ZERO)
            .unwrap()
            .observed_state,
        PreviewArtifactState::Interrupted
    );
    let owner = claim(&catalog, &plan).unwrap();
    let next = PreviewAttempt::begin(&catalog, &plan, &request, &owner, None).unwrap();
    let old = catalog.preview_artifact_status(&id, false).unwrap();
    assert_eq!(old.artifact.state, PreviewArtifactState::Interrupted);
    assert!(!old.latest_attempt);
    assert_ne!(next.artifact_id, id);
    catalog.cancel_preview_artifact(&next.artifact_id).unwrap();
    assert!(next.cancellation_requested_now());
}

#[test]
fn progress_pages_are_bounded_without_rewriting_the_input_manifest() {
    let (_dir, catalog, asset) = fixture();
    let plan = plan(&catalog, &asset);
    let owner = claim(&catalog, &plan).unwrap();
    let attempt = PreviewAttempt::begin(&catalog, &plan, &request(&plan), &owner, None).unwrap();
    let large_manifest = "x".repeat(100_000);
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE preview_artifacts SET identity_json=?2 WHERE artifact_id=?1",
            params![attempt.artifact_id, large_manifest],
        )
        .unwrap();
    for position in 0..105 {
        attempt
            .outcome(&PreviewCutOutcome {
                operation_id: format!("cut-{position}"),
                position,
                asset_id: asset.clone(),
                state: "complete".into(),
                reused: false,
                cache_key: None,
                failure: None,
            })
            .unwrap();
    }
    let connection = catalog.connection().unwrap();
    let (body, identity): (String, String) = connection
        .query_row(
            "SELECT body_json,identity_json FROM preview_artifacts WHERE artifact_id=?1",
            [&attempt.artifact_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(body.len() < 4096);
    assert_eq!(identity, large_manifest);
    // Replace the deliberately non-JSON sentinel before exercising a full read.
    connection
        .execute(
            "UPDATE preview_artifacts SET identity_json=NULL WHERE artifact_id=?1",
            [&attempt.artifact_id],
        )
        .unwrap();
    let status = catalog
        .preview_artifact_status(&attempt.artifact_id, false)
        .unwrap();
    assert_eq!(status.artifact.outcome_total, 105);
    assert_eq!(status.artifact.cut_outcomes.len(), 100);
    assert!(status.artifact.outcomes_truncated);
}

#[test]
fn readonly_diagnosis_never_creates_a_preview_cache_or_migrates() {
    let (dir, catalog, asset) = fixture();
    let plan = plan(&catalog, &asset);
    let reader = Catalog::open_read_only_existing(catalog.database_path()).unwrap();
    let before =
        crate::migrations::inspect_project_catalog_migrations(&reader.connection().unwrap())
            .unwrap();
    let page = reader.preview_artifacts(&plan.revision_id, 0, 20).unwrap();
    assert_eq!(page.total, 0);
    assert!(page.schema_ready);
    assert!(reader.preview_artifact_status("absent", false).is_err());
    assert_eq!(
        crate::migrations::inspect_project_catalog_migrations(&reader.connection().unwrap())
            .unwrap(),
        before
    );
    assert!(!dir.path().join("cache/previews").exists());
}

#[cfg(unix)]
#[test]
fn substituted_cache_directory_is_never_used_to_read_or_write_external_files() {
    let (dir, catalog, asset) = fixture();
    let plan = plan(&catalog, &asset);
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("cache")).unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("cache/previews")).unwrap();
    assert!(
        catalog
            .render_preview(&request(&plan), &mut |_| {}, &|| false)
            .unwrap_err()
            .to_string()
            .contains("symlink")
    );
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    assert_eq!(
        catalog
            .preview_artifacts(&plan.revision_id, 0, 20)
            .unwrap()
            .total,
        0
    );
}

#[test]
fn database_cancellation_stops_a_resource_waiter_before_media_work() {
    let (_dir, catalog, asset) = fixture();
    let plan = plan(&catalog, &asset);
    let _busy_resource = catalog.acquire_local_heavy().unwrap();
    let mut id = None;
    let error = catalog
        .render_preview(
            &request(&plan),
            &mut |event| {
                if let PreviewProgress::Started {
                    artifact_id: Some(artifact),
                    ..
                } = event
                {
                    catalog.cancel_preview_artifact(&artifact).unwrap();
                    id = Some(artifact);
                }
            },
            &|| false,
        )
        .unwrap_err();
    assert!(error.to_string().contains("cancel"));
    let status = catalog
        .preview_artifact_status(&id.unwrap(), false)
        .unwrap();
    assert_eq!(status.observed_state, PreviewArtifactState::Cancelled);
    assert!(status.artifact.identity.is_none());
    assert!(status.artifact.cut_outcomes.is_empty());
}

#[test]
fn old_schema_preview_diagnosis_is_readonly_and_unversioned_receipts_survive_replay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("schema52.sqlite3");
    let mut connection = Connection::open(&path).unwrap();
    crate::migrations::run_sqlite_migrations(
        &mut connection,
        &crate::migrations::PROJECT_CATALOG_MIGRATIONS[..52],
    )
    .unwrap();
    connection.execute_batch("INSERT INTO projects(id,name,created_at) VALUES('project','fixture','now');
        INSERT INTO edit_plan_revisions(revision_id,project_id,active,created_at) VALUES('revision','project',0,'now');").unwrap();
    drop(connection);
    let catalog = Catalog::open_read_only_existing(&path).unwrap();
    let before = fs::read(&path).unwrap();
    let page = catalog.preview_artifacts("revision", 0, 20).unwrap();
    assert!(!page.schema_ready);
    assert_eq!(page.total, 0);
    assert!(matches!(
        catalog
            .preview_artifact_status("missing", false)
            .unwrap_err()
            .downcast_ref::<PreviewArtifactError>(),
        Some(PreviewArtifactError::NotFound { .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(!dir.path().join("cache").exists());
    drop(catalog);
    let catalog = Catalog::open(&path).unwrap();
    let plan = catalog.edit_plan_revision("revision").unwrap();
    let owner = claim(&catalog, &plan).unwrap();
    let attempt = PreviewAttempt::begin(&catalog, &plan, &request(&plan), &owner, None).unwrap();
    attempt
        .finish_error(&anyhow::anyhow!("fixture failed before rendering"), false)
        .unwrap();
    let id = attempt.artifact_id.clone();
    drop(attempt);
    drop(owner);
    catalog
        .connection()
        .unwrap()
        .execute_batch("DROP TABLE schema_migrations")
        .unwrap();
    drop(catalog);
    let reopened = Catalog::open(&path).unwrap();
    let status = reopened.preview_artifact_status(&id, false).unwrap();
    assert_eq!(status.artifact.state, PreviewArtifactState::Failed);
    assert_eq!(
        reopened.preview_artifacts("revision", 0, 20).unwrap().total,
        1
    );
}

#[test]
fn preview_progress_serializes_artifact_identity_and_fields_in_camel_case() {
    let value = serde_json::to_value(PreviewProgress::Started {
        artifact_id: Some("preview_fixture".into()),
        sequence: Some(3),
        clip_count: 2,
        worker_count: 1,
    })
    .unwrap();
    assert_eq!(
        value,
        serde_json::json!({"type":"started","artifactId":"preview_fixture","sequence":3,"clipCount":2,"workerCount":1})
    );
}
