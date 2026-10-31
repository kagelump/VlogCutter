//! Generated visual controls test advisory containment, not classifier quality.
use crate::{
    Catalog, InspectionObservation, InspectionObservationKind, InspectionPurpose,
    InspectionRepresentation, InspectionReviewCaller, InspectionReviewRequest,
    InspectionReviewStepRequest, InspectionReviewTool, SourceInspectionRequest,
};
use std::{fs, process::Command};
use tempfile::tempdir;

fn control_frame(control: usize) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(160 * 90 * 3);
    for y in 0..90 {
        for x in 0..160 {
            let pixel = match control {
                // Upright map-like grid with roads, a blue water feature and marker.
                0 if x % 20 < 2 || y % 18 < 2 => [224, 213, 174],
                0 if (35..120).contains(&x) && (25..70).contains(&y) => [48, 128, 168],
                0 if (x as isize - 42).abs() < 4 && (y as isize - 36).abs() < 6 => [190, 45, 38],
                0 => [91, 147, 104],
                // Meaningful dark night sky with stars and a lit cabin window.
                1 if ((x * 37 + y * 19) % 113) < 2 => [205, 209, 228],
                1 if (112..148).contains(&x) && (42..75).contains(&y) => [113, 83, 59],
                1 if (119..141).contains(&x) && (49..68).contains(&y) => [231, 184, 91],
                1 => [7, 11, 24],
                // Close-up hand-like warm silhouette against a neutral field.
                _ if (x > 49 && x < 119 && y > 43 && y < 88)
                    || (x > 55 && x < 68 && y > 19 && y < 63)
                    || (x > 72 && x < 85 && y > 12 && y < 61)
                    || (x > 89 && x < 102 && y > 17 && y < 62)
                    || (x > 105 && x < 117 && y > 28 && y < 62) =>
                {
                    [190, 122, 91]
                }
                _ => [105, 117, 125],
            };
            rgb.extend_from_slice(&pixel);
        }
    }
    rgb
}

#[test]
fn generated_map_dark_and_hand_false_flags_remain_advisory() {
    let dir = tempdir().unwrap();
    let media = dir.path().join("controls");
    fs::create_dir(&media).unwrap();
    for (index, name) in ["upright_map", "meaningful_dark", "closeup_hand"]
        .iter()
        .enumerate()
    {
        let raw = dir.path().join(format!("{name}.rgb"));
        let frame = control_frame(index);
        let mut bytes = Vec::with_capacity(frame.len() * 24);
        for _ in 0..24 {
            bytes.extend_from_slice(&frame);
        }
        fs::write(&raw, bytes).unwrap();
        let output = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "rawvideo",
                "-pixel_format",
                "rgb24",
                "-video_size",
                "160x90",
                "-framerate",
                "24",
                "-i",
            ])
            .arg(raw)
            .args([
                "-an", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-r", "24", "-t", "1",
            ])
            .arg(media.join(format!("{name}.mp4")))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let catalog = Catalog::open(dir.path().join("catalog.sqlite3")).unwrap();
    let project = catalog
        .create_project("Generated inspection controls")
        .unwrap();
    assert_eq!(
        catalog.ingest_folder(&project.id, &media).unwrap().imported,
        3
    );
    for asset in catalog.list_assets(&project.id).unwrap() {
        let kind = match asset.filename.as_str() {
            "upright_map.mp4" => InspectionObservationKind::OrientationQuestion,
            "meaningful_dark.mp4" => InspectionObservationKind::CoveredMiddle,
            "closeup_hand.mp4" => InspectionObservationKind::CoveredEdge,
            _ => unreachable!("unexpected generated control"),
        };
        let before = catalog.get_asset(&asset.id).unwrap();
        let seed = catalog
            .inspect_source_window(
                &SourceInspectionRequest {
                    asset_id: asset.id.clone(),
                    representation: InspectionRepresentation::Original,
                    purpose: InspectionPurpose::Locator,
                    source_in_seconds: 0.0,
                    source_out_seconds: 1.0,
                    before_seconds: 0.0,
                    after_seconds: 0.0,
                    sample_count: 3,
                    max_width: 160,
                    source_times_seconds: vec![],
                    exact_representation_pts: vec![],
                    expected_representation_fingerprint: None,
                    include_audio: false,
                    include_transcript: false,
                    io_timeout_seconds: 180,
                    probe_timeout_seconds: 180,
                    decode_timeout_seconds: 180,
                },
                &|| false,
            )
            .unwrap()
            .inspection;
        let session = catalog
            .create_inspection_review(&InspectionReviewRequest {
                seed_evidence_id: seed.evidence_id.clone(),
                asset_id: asset.id.clone(),
                max_steps: 1,
                max_total_frames: 8,
                max_media_seconds: 1.0,
                max_width: 160,
                allow_audio: false,
                allow_transcript: false,
            })
            .unwrap();
        let result = catalog
            .inspection_review_step(
                &InspectionReviewStepRequest {
                    session_id: session.id,
                    step_index: 0,
                    caller: InspectionReviewCaller::Reviewer,
                    request: InspectionReviewTool::Observe(InspectionObservation {
                        inspected_evidence_id: seed.evidence_id,
                        kind,
                        source_in_seconds: 0.0,
                        source_out_seconds: 1.0,
                        observation: "deliberately false control flag".into(),
                        uncertainty: "synthetic control; requires human review".into(),
                    }),
                },
                &|| false,
            )
            .unwrap();
        assert_eq!(result.result.unwrap()["advisory"], true);
        let after = catalog.get_asset(&asset.id).unwrap();
        assert_eq!(
            (before.rotation_degrees, before.removed),
            (after.rotation_degrees, after.removed)
        );
        assert!(
            catalog
                .source_range_constraints(&asset.id, 0, 10)
                .unwrap()
                .items
                .is_empty()
        );
    }
}
