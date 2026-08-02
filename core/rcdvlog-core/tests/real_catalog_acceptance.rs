//! End-to-end acceptance against a *real* ingested catalog rather than a
//! fixture this repository wrote (supports D01-T018 and D01-T024).
//!
//! Every automated test in this epic otherwise runs on fixtures somebody here
//! authored, which means they encode this repository's assumptions about what
//! footage looks like. This one runs against a catalog built by ingesting real
//! multi-camera footage.
//!
//! The catalog path is never recorded here. Supply it at invocation:
//!
//! ```sh
//! CATALOG_PATH=<path> cargo test -p rcdvlog-core --test real_catalog_acceptance -- --ignored --nocapture
//! ```
//!
//! Ignored by default so CI, which has no such catalog, stays green. The test
//! opens the catalog read-only for its chronology assertions and never touches
//! source media.

use rcdvlog_core::{Catalog, EditPlanOperation, OtioExportRequest, otio_document};
use std::path::PathBuf;

fn catalog_path() -> PathBuf {
    PathBuf::from(std::env::var("CATALOG_PATH").expect("set CATALOG_PATH to a real catalog"))
}

/// The chronology of a real corpus is total, deterministic, and complete: every
/// clip is ordered, no two share a position, and re-solving changes nothing.
#[test]
#[ignore = "requires a real ingested catalog at $CATALOG_PATH"]
fn a_real_corpus_solves_to_a_stable_total_order() {
    let catalog = Catalog::open_read_only_existing(catalog_path()).unwrap();
    let projects = catalog.list_projects().unwrap();
    assert!(!projects.is_empty(), "the catalog has no projects");

    for project in projects {
        let facts = catalog.chronology_facts(&project.id).unwrap();
        if facts.assets.is_empty() {
            continue;
        }
        let first = catalog.solve_chronology(&project.id).unwrap();
        let second = catalog.solve_chronology(&project.id).unwrap();
        assert_eq!(first, second, "re-solving must change nothing");

        println!(
            "{}: {} clips, {} sequences, {} unplaced, {} contradictions",
            project.name,
            first.assets.len(),
            facts.sequences.len(),
            first
                .assets
                .iter()
                .filter(|entry| entry.trip_time.is_none())
                .count(),
            first.contradictions.len()
        );

        // The order is total: strictly increasing, no ties anywhere.
        for pair in first.assets.windows(2) {
            assert!(
                pair[0].order_key < pair[1].order_key,
                "{} and {} are not strictly ordered",
                pair[0].asset_id,
                pair[1].asset_id
            );
        }

        // Every camera resolved to its own capture sequence: an anonymous
        // camera collapses every card into one sequence, which is the exact
        // collision D01-T028 exists to prevent.
        assert!(
            facts.sequences.len() > 1,
            "{}: a multi-camera corpus must not collapse into one sequence",
            project.name
        );
        for asset in &facts.assets {
            assert!(
                asset.capture_sequence_id.is_some(),
                "{} belongs to no capture sequence",
                asset.asset_id
            );
        }
    }
}

/// A plan built from real clips exports to a byte-stable OTIO document that
/// references the camera originals.
#[test]
#[ignore = "requires a real ingested catalog at $CATALOG_PATH"]
fn a_real_plan_exports_to_a_stable_otio_document() {
    let path = catalog_path();
    let catalog = Catalog::open(&path).unwrap();
    let project = catalog
        .list_projects()
        .unwrap()
        .into_iter()
        .find(|project| project.asset_count > 0)
        .expect("no project with assets");

    // The plan follows the solver's order, which is the point of doing
    // chronology first.
    let ordered = catalog.solve_chronology(&project.id).unwrap();
    let operations: Vec<EditPlanOperation> = ordered
        .assets
        .iter()
        .take(12)
        .enumerate()
        .map(|(position, entry)| EditPlanOperation {
            operation_id: String::new(),
            position: position as u32,
            asset_id: entry.asset_id.clone(),
            source_in_seconds: 0.0,
            source_out_seconds: 2.0,
            rationale: "acceptance run".to_string(),
        })
        .collect();
    assert!(!operations.is_empty());

    let existing = catalog.active_edit_plan(&project.id).unwrap();
    let revision = catalog
        .create_edit_plan_revision(
            &project.id,
            &operations,
            None,
            existing.as_ref().map(|plan| plan.revision_id.as_str()),
        )
        .unwrap();
    catalog
        .activate_edit_plan_revision(
            &revision.revision_id,
            existing.as_ref().map(|plan| plan.revision_id.as_str()),
        )
        .unwrap();

    let report = catalog.validate_edit_plan(&revision.revision_id).unwrap();
    for finding in &report.findings {
        println!(
            "  {} {:?}: {}",
            finding.code, finding.severity, finding.message
        );
    }

    let assets = catalog.list_assets(&project.id).unwrap();
    let document = otio_document(&revision, &assets, "Antarctica").unwrap();
    for _ in 0..8 {
        assert_eq!(
            otio_document(&revision, &assets, "Antarctica").unwrap(),
            document,
            "the OTIO document must be byte-stable"
        );
    }

    let output = path
        .parent()
        .unwrap()
        .join(format!("{}.otio", revision.revision_id));
    let result = catalog
        .export_otio(&OtioExportRequest {
            revision_id: revision.revision_id.clone(),
            output_path: output.to_string_lossy().into_owned(),
            expected_existing_digest: None,
            timeline_name: "Antarctica".to_string(),
        })
        .unwrap();
    println!(
        "exported {} clips to {} (digest {})",
        result.clip_count, result.output_path, result.output_digest
    );

    // The export references camera originals, never proxies.
    let parsed: serde_json::Value = serde_json::from_str(&document).unwrap();
    let clips = parsed["tracks"]["children"][0]["children"]
        .as_array()
        .unwrap();
    assert_eq!(clips.len(), operations.len());
    for clip in clips {
        let target = clip["media_reference"]["target_url"].as_str().unwrap();
        assert!(
            !target.contains("/cache/proxies/"),
            "the export referenced a proxy: {target}"
        );
    }
}
