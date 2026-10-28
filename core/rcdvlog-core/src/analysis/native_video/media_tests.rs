//! Generated local-media checks for the native-video preparation boundary.
use super::*;
use crate::analysis::accounting::usage_pricing::PublishedServiceTier;
use crate::{
    AnalysisPayload, InspectionRepresentation, NativeVideoNavigation, NativeVideoOptions,
    NativeVideoPurpose, NativeVideoSourceScope, NativeVideoThinking, NativeVideoWindowRequest,
    RunPolicy, RunPolicyParameters, RunPolicyRequest,
};
use std::process::Command;

pub(in crate::analysis::native_video) fn setup(
    payload: AnalysisPayload,
) -> (tempfile::TempDir, Catalog, RunPolicy, String) {
    // Reuse the shared generated 60 fps source rather than embedding another fixture generator.
    let (dir, catalog, asset) = crate::editplan::inspection::tests::fixture();
    let project = catalog.list_projects().unwrap().remove(0);
    let options = NativeVideoOptions {
        version: 1,
        purpose: NativeVideoPurpose::Moments,
        tier: PublishedServiceTier::Standard,
        representation: InspectionRepresentation::Original,
        navigation: NativeVideoNavigation::Static,
        thinking: NativeVideoThinking::Minimal,
        source_scopes: vec![NativeVideoSourceScope {
            asset_id: asset.clone(),
            source_in_seconds: 0.0,
            source_out_seconds: 4.0,
        }],
        fps: 2.0,
        max_width: 160,
        max_window_seconds: 4,
        max_media_seconds: 4,
        max_requests: 1,
        max_output_tokens: 256,
        media_workers: 1,
        api_workers: 1,
    };
    let request = RunPolicyRequest {
        project_id: project.id,
        asset_ids: vec![asset.clone()],
        provider: NATIVE_VIDEO_PROVIDER.into(),
        primary_model: NATIVE_VIDEO_MODEL.into(),
        allowed_models: vec![NATIVE_VIDEO_MODEL.into()],
        allow_fallback: false,
        payload,
        include_project_vocabulary: false,
        budget_usd: 1.0,
        parameters: RunPolicyParameters::NativeVideoReview { options },
    };
    let policy = catalog
        .describe_run_policy_for_local_preparation(&request)
        .unwrap();
    assert!(
        policy.readiness.ready_for_authorization,
        "{:#?}",
        policy.readiness
    );
    (dir, catalog, policy, asset)
}

pub(in crate::analysis::native_video) fn window(
    policy: &RunPolicy,
    fps: f64,
) -> NativeVideoWindowRequest {
    NativeVideoWindowRequest {
        policy_id: policy.policy_id.clone(),
        scope_index: 0,
        source_in_seconds: 1.005,
        source_out_seconds: 2.005,
        fps,
        max_width: 160,
    }
}

fn request_for_asset(
    policy: &RunPolicy,
    project_id: &str,
    asset_id: &str,
    payload: AnalysisPayload,
) -> RunPolicyRequest {
    let mut request = policy.request();
    request.project_id = project_id.to_string();
    request.asset_ids = vec![asset_id.to_string()];
    request.payload = payload;
    if let RunPolicyParameters::NativeVideoReview { options } = &mut request.parameters {
        options.source_scopes = vec![NativeVideoSourceScope {
            asset_id: asset_id.to_string(),
            source_in_seconds: 0.0,
            source_out_seconds: 4.0,
        }];
    }
    request
}

fn ffprobe_json(path: &std::path::Path, entries: &str) -> serde_json::Value {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_frames",
            "-show_entries",
            entries,
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn preparation_strips_audio_unless_policy_explicitly_includes_it() {
    let (_d, c, p, _) = setup(AnalysisPayload::Video);
    let silent = c
        .prepare_native_video_window(&p, &window(&p, 1.0), &|| false)
        .unwrap();
    assert!(!silent.view().audio_included);
    assert_eq!(silent.view().decoded_audio_samples, 0);
    let (_d, c, p, _) = setup(AnalysisPayload::VideoAndAudio);
    let audible = c
        .prepare_native_video_window(&p, &window(&p, 1.0), &|| false)
        .unwrap();
    assert!(audible.view().audio_included);
    assert!(audible.view().decoded_audio_samples > 0);
}

#[test]
fn first_source_pts_offset_is_measured_and_provider_fps_does_not_reencode_carrier() {
    let (_d, c, p, _) = setup(AnalysisPayload::Video);
    let first = c
        .prepare_native_video_window(&p, &window(&p, 1.0), &|| false)
        .unwrap();
    assert!(first.view().clock.source_offset_seconds >= 1.005);
    assert!(first.view().clock.source_offset_seconds < 1.04);
    assert!(first.view().clock.first_representation_pts > 0);
    assert!(
        f64::from(first.view().video_frame_count) / first.view().encoded_fps
            >= first.view().clock.local_valid_end_seconds - 1.0 / 240000.0
    );
    let again = c
        .prepare_native_video_window(&p, &window(&p, 2.0), &|| false)
        .unwrap();
    assert_eq!(again.view().identity, first.view().identity);
    assert_eq!(
        again.view().output_fingerprint,
        first.view().output_fingerprint
    );
    assert!(again.view().reused);
    assert_eq!(again.view().provider_fps, 2.0);
}

#[test]
fn corrupt_cache_object_is_replaced_by_a_unique_valid_object() {
    let (_d, c, p, _) = setup(AnalysisPayload::Video);
    let first = c
        .prepare_native_video_window(&p, &window(&p, 1.0), &|| false)
        .unwrap();
    let parent = c.database_path().parent().unwrap();
    let path = parent.join(&first.view().output_relative_path);
    std::fs::write(&path, b"corrupt").unwrap();
    let repaired = c
        .prepare_native_video_window(&p, &window(&p, 1.0), &|| false)
        .unwrap();
    assert_eq!(repaired.view().identity, first.view().identity);
    assert_ne!(
        repaired.view().output_relative_path,
        first.view().output_relative_path
    );
    assert_ne!(
        std::fs::read(parent.join(&repaired.view().output_relative_path)).unwrap(),
        b"corrupt"
    );
    assert!(!repaired.view().reused);
}

#[test]
fn changed_source_wrong_scope_and_cancellation_fail_closed() {
    let (_d, c, p, asset) = setup(AnalysisPayload::Video);
    assert!(
        c.prepare_native_video_window(&p, &window(&p, 1.0), &|| true)
            .is_err()
    );
    let mut out_of_scope = window(&p, 1.0);
    out_of_scope.source_out_seconds = 4.5;
    assert!(
        c.prepare_native_video_window(&p, &out_of_scope, &|| false)
            .is_err()
    );
    let mut changed = p.clone();
    changed.policy_id.push_str("stale");
    assert!(
        c.prepare_native_video_window(&changed, &window(&changed, 1.0), &|| false)
            .is_err()
    );
    let source = c.get_asset(&asset).unwrap();
    std::fs::write(&source.path, b"changed generated source").unwrap();
    assert!(
        c.prepare_native_video_window(&p, &window(&p, 1.0), &|| false)
            .is_err()
    );
}

#[test]
fn rotation_is_baked_once_with_landscape_orientation_and_no_display_matrix() {
    let (_dir, catalog, policy, asset_id) = setup(AnalysisPayload::Video);
    let asset = catalog.get_asset(&asset_id).unwrap();
    catalog
        .update_asset(
            &asset_id,
            &crate::AssetPatch {
                notes: asset.notes.clone(),
                labels: asset.labels.clone(),
                removed: asset.removed,
                rotation_degrees: 90,
                expected_revision: Some(asset.revision),
            },
        )
        .unwrap();
    let rotated_policy = catalog
        .describe_run_policy_for_local_preparation(&policy.request())
        .unwrap();
    let media = catalog
        .prepare_native_video_window(&rotated_policy, &window(&rotated_policy, 1.0), &|| false)
        .unwrap();
    let output = catalog
        .database_path()
        .parent()
        .unwrap()
        .join(&media.view().output_relative_path);
    let probe = ffprobe_json(
        &output,
        "stream=width,height:stream_tags=rotate:stream_side_data=rotation",
    );
    assert_eq!(probe["streams"][0]["width"], 90);
    assert_eq!(probe["streams"][0]["height"], 160);
    let stream = &probe["streams"][0];
    assert!(
        stream
            .get("tags")
            .and_then(|t| t.get("rotate"))
            .is_none_or(|v| v == "0")
    );
    assert!(
        stream
            .get("side_data_list")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|rows| rows.iter().all(|row| row
                .get("rotation")
                .is_none_or(|v| v.as_f64().unwrap_or(0.0) == 0.0)))
    );

    let decoded = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&output)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .unwrap();
    assert!(decoded.status.success());
    let red_at = |x: usize, y: usize| {
        let p = (y * 90 + x) * 3;
        decoded.stdout[p] > 130 && decoded.stdout[p + 1] < 90 && decoded.stdout[p + 2] < 90
    };
    assert!(
        (70..90).any(|x| (0..20).any(|y| red_at(x, y))),
        "red source corner should rotate to top-right"
    );
    assert!(
        !(0..20).any(|x| (0..20).any(|y| red_at(x, y))),
        "top-left marker should not remain or be rotated twice"
    );
}

#[test]
fn nonzero_representation_pts_maps_to_source_clock_and_zero_based_encoded_pts() {
    let (dir, catalog, policy, asset_id) = setup(AnalysisPayload::Video);
    let source = catalog.get_asset(&asset_id).unwrap();
    let remuxed = dir.path().join("offset-five-seconds.mp4");
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&source.path)
        .args([
            "-map",
            "0:v:0",
            "-c",
            "copy",
            "-output_ts_offset",
            "5",
            "-y",
        ])
        .arg(&remuxed)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let project = catalog
        .create_project("Nonzero representation PTS")
        .unwrap();
    catalog.ingest_folder(&project.id, dir.path()).unwrap();
    let remuxed_asset = catalog
        .list_assets(&project.id)
        .unwrap()
        .into_iter()
        .find(|a| a.path == remuxed.to_string_lossy())
        .unwrap();
    let request = request_for_asset(
        &policy,
        &project.id,
        &remuxed_asset.id,
        AnalysisPayload::Video,
    );
    let offset_policy = catalog
        .describe_run_policy_for_local_preparation(&request)
        .unwrap();
    let media = catalog
        .prepare_native_video_window(&offset_policy, &window(&offset_policy, 1.0), &|| false)
        .unwrap();
    assert!(
        media.view().clock.first_representation_pts as f64
            * media.view().clock.representation_time_base_numerator as f64
            / media.view().clock.representation_time_base_denominator as f64
            > 5.0
    );
    assert!((media.view().clock.source_offset_seconds - 1.0167).abs() < 0.02);
    let encoded = dir.path().join(&media.view().output_relative_path);
    let probe = ffprobe_json(
        &encoded,
        "frame=best_effort_timestamp,media_type:stream=codec_type",
    );
    let value = &probe["frames"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["media_type"] == "video")
        .unwrap()["best_effort_timestamp"];
    let first_pts = value
        .as_i64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .unwrap();
    assert_eq!(first_pts, 0);
}

#[test]
fn explicit_audio_payload_rejects_video_only_representation_without_fallback() {
    let (dir, catalog, policy, asset_id) = setup(AnalysisPayload::Video);
    let source = catalog.get_asset(&asset_id).unwrap();
    let media_dir = dir.path().join("video-only");
    std::fs::create_dir(&media_dir).unwrap();
    let video_only = media_dir.join("video-only.mp4");
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&source.path)
        .args([
            "-map",
            "0:v:0",
            "-an",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-y",
        ])
        .arg(&video_only)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let project = catalog.create_project("Video without audio").unwrap();
    catalog.ingest_folder(&project.id, &media_dir).unwrap();
    let video_asset = catalog.list_assets(&project.id).unwrap().remove(0);
    let request = request_for_asset(
        &policy,
        &project.id,
        &video_asset.id,
        AnalysisPayload::VideoAndAudio,
    );
    let policy = catalog
        .describe_run_policy_for_local_preparation(&request)
        .unwrap();
    assert!(!policy.readiness.ready_for_authorization);
    assert!(
        policy
            .readiness
            .blockers
            .iter()
            .any(|blocker| blocker.code == "native_video_source_audio_unavailable")
    );
    assert!(
        catalog
            .prepare_native_video_window(&policy, &window(&policy, 1.0), &|| false)
            .is_err()
    );
}

#[test]
fn input_display_matrix_cannot_survive_as_a_second_output_rotation() {
    let (dir, catalog, policy, asset) = setup(AnalysisPayload::Video);
    let original = catalog.get_asset(&asset).unwrap();
    let media = dir.path().join("source with display matrix");
    std::fs::create_dir(&media).unwrap();
    let output = media.join("rotated.mp4");
    let remux = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&original.path)
        .args([
            "-map",
            "0:v:0",
            "-c",
            "copy",
            "-metadata:s:v:0",
            "rotate=90",
        ])
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        remux.status.success(),
        "{}",
        String::from_utf8_lossy(&remux.stderr)
    );
    let project = catalog
        .create_project("Generated rotation metadata")
        .unwrap();
    catalog.ingest_folder(&project.id, &media).unwrap();
    let asset = catalog.list_assets(&project.id).unwrap().remove(0);
    catalog
        .update_asset(
            &asset.id,
            &crate::AssetPatch {
                notes: asset.notes,
                labels: asset.labels,
                removed: false,
                rotation_degrees: 90,
                expected_revision: Some(asset.revision),
            },
        )
        .unwrap();
    let mut request = policy.request();
    request.project_id = project.id;
    request.asset_ids = vec![asset.id.clone()];
    let RunPolicyParameters::NativeVideoReview { options } = &mut request.parameters else {
        unreachable!()
    };
    options.source_scopes[0].asset_id = asset.id;
    let policy = catalog
        .describe_run_policy_for_local_preparation(&request)
        .unwrap();
    let prepared = catalog
        .prepare_native_video_window(&policy, &window(&policy, 1.0), &|| false)
        .unwrap();
    assert_eq!((prepared.view().width, prepared.view().height), (90, 160));
    let output = catalog
        .database_path()
        .parent()
        .unwrap()
        .join(&prepared.view().output_relative_path);
    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream_side_data=rotation",
            "-of",
            "json",
        ])
        .arg(output)
        .output()
        .unwrap();
    assert!(probe.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    assert!(doc["streams"].as_array().unwrap().iter().all(|s| {
        s["side_data_list"].as_array().is_none_or(|list| {
            list.iter()
                .all(|v| v["rotation"].as_f64().is_none_or(|r| r.abs() < 1e-9))
        })
    }));
}

#[test]
fn cached_media_cannot_relabel_its_clock_or_alias_another_window() {
    let (_dir, catalog, policy, _) = setup(AnalysisPayload::Video);
    let request = window(&policy, 1.0);
    let first = catalog
        .prepare_native_video_window(&policy, &request, &|| false)
        .unwrap();
    let mut forged = first.view().clone();
    forged.clock.source_offset_seconds += 0.1;
    forged.clock.local_valid_end_seconds -= 0.1;
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE native_video_media SET report_json=?2 WHERE identity=?1",
            rusqlite::params![
                first.view().identity,
                serde_json::to_string(&forged).unwrap()
            ],
        )
        .unwrap();
    let fixed = catalog
        .prepare_native_video_window(&policy, &request, &|| false)
        .unwrap();
    assert!(!fixed.view().reused);
    assert_eq!(fixed.view().clock, first.view().clock);
    let mut another = request.clone();
    another.source_in_seconds = 2.0;
    another.source_out_seconds = 3.0;
    let other = catalog
        .prepare_native_video_window(&policy, &another, &|| false)
        .unwrap();
    let mut alias = fixed.view().clone();
    alias.output_relative_path = other.view().output_relative_path.clone();
    alias.output_fingerprint = other.view().output_fingerprint.clone();
    alias.byte_length = other.view().byte_length;
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE native_video_media SET report_json=?2 WHERE identity=?1",
            rusqlite::params![
                first.view().identity,
                serde_json::to_string(&alias).unwrap()
            ],
        )
        .unwrap();
    let restored = catalog
        .prepare_native_video_window(&policy, &request, &|| false)
        .unwrap();
    assert!(!restored.view().reused);
    assert_eq!(
        restored.view().output_fingerprint,
        first.view().output_fingerprint
    );
    assert_eq!(restored.view().clock, first.view().clock);
}
