use super::*;
use crate::editplan::inspection::tests::fixture;
use crate::{AssetPatch, EditPlanRevision, PreviewProgress, PreviewRequest};
use std::{cell::Cell, io::Write};

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
                    rationale: "Generated reuse mechanics".into(),
                    audio: None,
                })
                .collect::<Vec<_>>(),
            None,
            None,
            None,
        )
        .unwrap()
}
fn render(catalog: &Catalog, plan: &EditPlanRevision) -> super::super::PreviewResult {
    catalog
        .render_preview(
            &PreviewRequest {
                revision_id: plan.revision_id.clone(),
                max_height: 144,
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap()
}
fn counts(result: &super::super::PreviewResult, encoded: u32, reused: u32, invalidated: u32) {
    assert_eq!(
        result.segment_counts,
        Some(super::super::PreviewSegmentCounts {
            encoded,
            reused,
            invalidated
        })
    );
}
fn cache_object(catalog: &Catalog, project: &str, key: &str) -> PathBuf {
    let body: String = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT body_json FROM preview_segments WHERE segment_key=?1",
            [key],
            |r| r.get(0),
        )
        .unwrap();
    let receipt: Receipt = serde_json::from_str(&body).unwrap();
    project_directory(catalog)
        .unwrap()
        .join("cache/preview-segments")
        .join(project)
        .join(key)
        .join("objects")
        .join(receipt.object_name)
}

#[test]
fn interrupted_work_reuses_verified_segments_and_a_reorder_or_single_trim_is_scoped() {
    let (_dir, catalog, asset) = fixture();
    let plan = plan(&catalog, &asset);
    let stop = Cell::new(false);
    let request = PreviewRequest {
        revision_id: plan.revision_id.clone(),
        max_height: 144,
    };
    assert!(
        catalog
            .render_preview(
                &request,
                &mut |event| {
                    if matches!(event, PreviewProgress::ClipCompleted { .. }) {
                        stop.set(true);
                    }
                },
                &|| stop.get()
            )
            .is_err()
    );
    let result = render(&catalog, &plan);
    counts(&result, 1, 1, 0);
    let complete_bytes = fs::read(&result.output_path).unwrap();
    let mut reversed = plan.operations.clone();
    reversed.reverse();
    for (position, cut) in reversed.iter_mut().enumerate() {
        cut.position = position as u32;
    }
    let reordered = catalog
        .create_edit_plan_from_base(
            &plan.project_id,
            &reversed,
            None,
            Some(&plan.revision_id),
            None,
        )
        .unwrap();
    let reused = render(&catalog, &reordered);
    counts(&reused, 0, 2, 0);
    let status = catalog
        .preview_artifact_status(reused.artifact_id.as_deref().unwrap(), false)
        .unwrap();
    assert!(
        status
            .artifact
            .cut_outcomes
            .iter()
            .all(|c| c.reused && c.cache_key.is_some())
    );
    assert_eq!(
        status.artifact.validation.unwrap().cuts[0].operation_id,
        reversed[0].operation_id
    );
    reversed[0].source_in_seconds += 0.2;
    let trimmed = catalog
        .create_edit_plan_from_base(
            &plan.project_id,
            &reversed,
            None,
            Some(&reordered.revision_id),
            None,
        )
        .unwrap();
    counts(&render(&catalog, &trimmed), 1, 1, 0);
    assert_eq!(fs::read(result.output_path).unwrap(), complete_bytes);
}

#[test]
fn corrupted_or_missing_cached_objects_are_repaired_and_changed_inputs_never_reuse_old_work() {
    let (_dir, catalog, asset_id) = fixture();
    let plan = plan(&catalog, &asset_id);
    let first = render(&catalog, &plan);
    counts(&first, 2, 0, 0);
    let status = catalog
        .preview_artifact_status(first.artifact_id.as_deref().unwrap(), false)
        .unwrap();
    let key = status.artifact.cut_outcomes[0]
        .cache_key
        .as_deref()
        .unwrap();
    let old_object = cache_object(&catalog, &plan.project_id, key);
    fs::write(&old_object, b"incomplete encoded fixture").unwrap();
    let repaired = render(&catalog, &plan);
    counts(&repaired, 1, 1, 1);
    assert_eq!(
        fs::read(&old_object).unwrap(),
        b"incomplete encoded fixture",
        "repair retains the old object instead of overwriting evidence"
    );
    let current_object = cache_object(&catalog, &plan.project_id, key);
    assert_ne!(current_object, old_object);
    fs::remove_file(current_object).unwrap();
    counts(&render(&catalog, &plan), 1, 1, 1);
    let asset = catalog.get_asset(&asset_id).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&asset.path)
        .unwrap()
        .write_all(b"changed-fixture-content")
        .unwrap();
    counts(&render(&catalog, &plan), 2, 0, 0);
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
    counts(&render(&catalog, &plan), 2, 0, 0);
    assert!(Path::new(&first.output_path).is_file());
}

#[test]
fn cache_keys_cover_content_exact_intervals_mapping_recipe_tools_and_frame_allocation() {
    let (_dir, catalog, asset) = fixture();
    let plan = plan(&catalog, &asset);
    let prepared = super::super::preview_media::prepare(
        &catalog,
        &plan,
        &PreviewRequest {
            revision_id: plan.revision_id.clone(),
            max_height: 144,
        },
        &catalog.list_assets(&plan.project_id).unwrap(),
        &|| false,
        None,
    )
    .unwrap();
    let expected = SegmentExpectation {
        width: 256,
        height: 144,
        seconds: 1.0,
        frames: None,
        audio: true,
    };
    let cut = &plan.operations[0];
    let original = key(&plan.project_id, &prepared.identity, cut, &expected).unwrap();
    let mut moved = cut.clone();
    moved.position = 100;
    moved.operation_id = "different-id".into();
    moved.rationale = "different reason".into();
    assert_eq!(
        original,
        key(&plan.project_id, &prepared.identity, &moved, &expected).unwrap()
    );
    moved.source_in_seconds += 0.000_001;
    assert_ne!(
        original,
        key(&plan.project_id, &prepared.identity, &moved, &expected).unwrap()
    );
    let mut relocated_tool = prepared.identity.clone();
    relocated_tool.tools[0].executable_key = "different filesystem locator and stat".into();
    assert_eq!(
        original,
        key(&plan.project_id, &relocated_tool, cut, &expected).unwrap(),
        "identical executable content is reusable after a path/stat change"
    );
    for change in 0..6 {
        let mut identity = prepared.identity.clone();
        match change {
            0 => identity.sources[0].representation_fingerprint.push('x'),
            1 => identity.sources[0].clocks["mappingPrecision"] = serde_json::json!("changed"),
            2 => identity.sources[0].rotation_degrees = 180,
            3 => identity.recipe.push_str("changed"),
            4 => identity.tools[0].version.push_str("changed"),
            _ => identity.tool_content_fingerprints[0].push('x'),
        }
        assert_ne!(
            original,
            key(&plan.project_id, &identity, cut, &expected).unwrap(),
            "change {change}"
        );
    }
    for change in 0..3 {
        let mut changed = expected.clone();
        match change {
            0 => changed.frames = Some(30),
            1 => changed.audio = false,
            _ => changed.height = 180,
        }
        assert_ne!(
            original,
            key(&plan.project_id, &prepared.identity, cut, &changed).unwrap()
        );
    }
}

#[test]
fn a_second_segment_owner_waits_cancellably_and_never_unlinks_the_claim() {
    let (_dir, catalog, asset) = fixture();
    let plan = plan(&catalog, &asset);
    let expected = SegmentExpectation {
        width: 256,
        height: 144,
        seconds: 1.0,
        frames: None,
        audio: true,
    };
    let key = "a".repeat(64);
    let owner = SegmentClaim::acquire(
        &catalog,
        &plan.project_id,
        key.clone(),
        expected.clone(),
        &|| false,
    )
    .unwrap();
    let polls = Cell::new(0);
    assert!(
        SegmentClaim::acquire(
            &catalog,
            &plan.project_id,
            key.clone(),
            expected.clone(),
            &|| {
                polls.set(polls.get() + 1);
                polls.get() > 2
            }
        )
        .is_err()
    );
    assert!(owner.directory.join(".segment.lock").is_file());
    drop(owner);
    assert!(SegmentClaim::acquire(&catalog, &plan.project_id, key, expected, &|| false).is_ok());
}
