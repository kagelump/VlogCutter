use super::*;
use crate::editplan::inspection::tests::fixture;
use std::process::Command;

fn plan(catalog: &Catalog, asset: &str, audio: Option<EditPlanAudio>) -> EditPlanRevision {
    let project = catalog.get_asset(asset).unwrap().project_id;
    catalog
        .create_edit_plan_from_base(
            &project,
            &[EditPlanOperation {
                operation_id: "sound-cut".into(),
                position: 0,
                asset_id: asset.into(),
                source_in_seconds: 0.0,
                source_out_seconds: 4.0,
                rationale: "Generated sound-integrity fixture".into(),
                audio,
            }],
            None,
            None,
            None,
        )
        .unwrap()
}
fn request(catalog: &Catalog, plan: &EditPlanRevision) -> AudioIntegrityRequest {
    let scope = PreviewExcerptScope {
        first_operation_id: "sound-cut".into(),
        last_operation_id: "sound-cut".into(),
    };
    let preview = catalog
        .render_preview_excerpt(
            &PreviewExcerptRequest {
                revision_id: plan.revision_id.clone(),
                max_height: 144,
                scope: scope.clone(),
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap();
    AudioIntegrityRequest {
        revision_id: plan.revision_id.clone(),
        scope,
        artifact_id: preview.artifact_id,
        samples: vec![
            AudioIntegritySampleRequest {
                movie_in_seconds: 0.25,
                movie_out_seconds: 0.5,
            },
            AudioIntegritySampleRequest {
                movie_in_seconds: 2.0,
                movie_out_seconds: 2.5,
            },
        ],
    }
}
fn silence_video(source: &Path, output: &Path, audio: bool) {
    let mut c = Command::new("ffmpeg");
    c.args(["-y", "-v", "error", "-i"]).arg(source);
    if audio {
        c.args([
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=48000:cl=stereo",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            "-t",
            "4",
        ]);
    } else {
        c.args(["-map", "0:v:0", "-c:v", "copy", "-an"]);
    }
    let output = c.arg(output).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn sampled_source_silence_tone_intentional_derivatives_and_cache_are_honest_and_bounded() {
    let (_dir, catalog, asset) = fixture();
    let plan = plan(&catalog, &asset, None);
    let req = request(&catalog, &plan);
    let mut progress = Vec::new();
    let result = catalog
        .audit_selected_audio_with_progress(&req, &|| false, &mut |e| progress.push(e))
        .unwrap();
    assert_eq!(result.report.state, "complete");
    assert!(!result.reused);
    assert_eq!(result.report.pairs[0].original.audio_present, Some(true));
    assert_eq!(result.report.samples[0].state, "sampledSourceLowEnergy");
    assert_eq!(result.report.samples[1].state, "sampledEnergyPresent");
    assert!(result.report.samples[1].master.as_ref().unwrap().energy.rms > 0.01);
    assert!(
        result
            .report
            .derivatives
            .iter()
            .all(|d| d["expectedAudio"] == false
                && d["audioPresent"] == false
                && d["role"] == "pictureOnly")
    );
    assert!((result.report.sampled_energy_seconds - 0.75).abs() < 1e-9);
    assert!(result.report.unchecked_movie_ranges.len() >= 2);
    assert_eq!(progress.last().unwrap().stage, "complete");
    assert!(progress.iter().all(|p| p.worker_count == 1));
    let again = catalog.audit_selected_audio(&req, &|| false).unwrap();
    assert!(again.reused);
    assert_eq!(again.report.evidence_id, result.report.evidence_id);
    let listen = &again.report.samples[1]
        .rendered
        .as_ref()
        .unwrap()
        .playable
        .path;
    let decoded = Command::new("ffmpeg")
        .args(["-v", "error", "-xerror", "-i", listen, "-f", "null", "-"])
        .output()
        .unwrap();
    assert!(decoded.status.success());
    fs::write(listen, b"corrupt generated listening copy").unwrap();
    let repaired = catalog.audit_selected_audio(&req, &|| false).unwrap();
    assert!(!repaired.reused);
    assert_eq!(repaired.report.evidence_id, result.report.evidence_id);
    assert_ne!(
        repaired.report.samples[1]
            .rendered
            .as_ref()
            .unwrap()
            .playable
            .path,
        *listen
    );
    let read_only = Catalog::open_read_only_existing(catalog.database_path()).unwrap();
    assert_eq!(
        read_only
            .audio_integrity_evidence(&result.report.evidence_id)
            .unwrap()
            .samples
            .len(),
        2
    );
    // A still-valid final movie does not make its now-damaged picture cache current.
    let key = result.report.derivatives[0]["segmentKey"].as_str().unwrap();
    let body: String = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT body_json FROM preview_segments WHERE segment_key=?1",
            [key],
            |r| r.get(0),
        )
        .unwrap();
    let receipt: Value = serde_json::from_str(&body).unwrap();
    let segment = super::super::preview_artifacts::project_directory(&catalog)
        .unwrap()
        .join("cache/preview-segments")
        .join(&plan.project_id)
        .join(key)
        .join("objects")
        .join(receipt["objectName"].as_str().unwrap());
    fs::write(segment, b"damaged derived picture segment").unwrap();
    let changed = catalog.audit_selected_audio(&req, &|| false).unwrap();
    assert!(!changed.reused);
    assert_ne!(changed.report.evidence_id, result.report.evidence_id);
    assert_eq!(changed.report.state, "partial");
    assert_eq!(changed.report.derivatives[0]["state"], "unchecked");
    assert!(
        changed
            .report
            .exceptions
            .iter()
            .any(|e| e == "silent_derivative_policy_unchecked")
    );
}

#[test]
fn explicit_silence_is_not_master_silence_and_no_audio_is_not_a_quiet_stream() {
    let (dir, catalog, asset) = fixture();
    let muted = plan(&catalog, &asset, Some(EditPlanAudio::Silent));
    let req = request(&catalog, &muted);
    let muted = catalog.audit_selected_audio(&req, &|| false).unwrap();
    assert_eq!(muted.report.pairs[0].original.audio_present, Some(true));
    assert!(
        muted
            .report
            .samples
            .iter()
            .all(|s| s.state == "deliberateSilence" && s.master.is_none())
    );
    let folder = dir.path().join("no-audio-original");
    fs::create_dir(&folder).unwrap();
    silence_video(
        Path::new(&catalog.get_asset(&asset).unwrap().path),
        &folder.join("no-audio.mp4"),
        false,
    );
    let project = catalog.create_project("Generated no-audio source").unwrap();
    catalog.ingest_folder(&project.id, &folder).unwrap();
    let no_audio = catalog.list_assets(&project.id).unwrap().remove(0).id;
    let no_plan = plan(&catalog, &no_audio, None);
    let req = request(&catalog, &no_plan);
    let report = catalog
        .audit_selected_audio(&req, &|| false)
        .unwrap()
        .report;
    assert_eq!(report.pairs[0].state, "masterHasNoAudio");
    assert_eq!(report.pairs[0].original.audio_present, Some(false));
    assert!(
        report
            .samples
            .iter()
            .all(|s| s.state == "noSourceAudio" && s.master.is_none())
    );
}

#[test]
fn zeroed_selected_proxy_content_and_unavailable_master_are_not_successful_source_silence() {
    let (dir, catalog, asset) = fixture();
    let proxy = catalog.generate_proxy(&asset).unwrap().proxy_path.unwrap();
    let zeroed = dir.path().join("zeroed-proxy.mp4");
    silence_video(Path::new(&proxy), &zeroed, true);
    fs::rename(zeroed, &proxy).unwrap(); // fault injection into this test's derived representation
    let plan = plan(&catalog, &asset, None);
    let req = request(&catalog, &plan);
    let report = catalog
        .audit_selected_audio(&req, &|| false)
        .unwrap()
        .report;
    assert_eq!(report.pairs[0].state, "audioStreamsPresent");
    assert!(
        report
            .exceptions
            .iter()
            .any(|e| e.contains("suspected_master_sound_missing_in_selected_sample")),
        "{:?}",
        report.exceptions
    );
    let original = catalog.get_asset(&asset).unwrap().path;
    let offline = dir.path().join("offline-original.mp4");
    fs::rename(&original, &offline).unwrap();
    let report = catalog
        .audit_selected_audio(&req, &|| false)
        .unwrap()
        .report;
    assert_eq!(report.state, "partial");
    assert_eq!(report.pairs[0].original.audio_present, None);
    assert!(
        report
            .samples
            .iter()
            .all(|s| s.state != "sampledSourceLowEnergy")
    );
    fs::rename(&offline, &original).unwrap();
    // A registered proxy may remain current while its master pathname has new bytes.
    let replacement = dir.path().join("replaced-master.mp4");
    silence_video(Path::new(&original), &replacement, true);
    fs::copy(&replacement, &original).unwrap();
    let report = catalog
        .audit_selected_audio(&req, &|| false)
        .unwrap()
        .report;
    assert_eq!(report.rendered_inputs_match, Some(true));
    assert_eq!(
        report.pairs[0].original_matches_registered_identity,
        Some(false)
    );
    assert_eq!(report.state, "partial");
    assert!(
        report
            .samples
            .iter()
            .all(|s| s.master.is_none() && s.state == "masterUnchecked")
    );
}

#[test]
fn actual_zeroed_render_sample_is_flagged_and_changed_owned_output_is_unchecked() {
    let (dir, catalog, asset) = fixture();
    let source = catalog.get_asset(&asset).unwrap().path;
    let zeroed = dir.path().join("zeroed-render.mp4");
    silence_video(Path::new(&source), &zeroed, true);
    let root = dir.path().join("measured");
    fs::create_dir(&root).unwrap();
    let event = |path: String| super::super::audio::AudioEvent {
        source: path,
        source_in_samples: 2 * 48000,
        source_out_samples: 120000,
        timeline_start_samples: 0,
        gain_db: 0.0,
        fade_in_samples: 0,
        fade_out_samples: 0,
    };
    let expected = mix_sample(&root, "expected", &[event(source)], 0, 24000, &|| false).unwrap();
    let zero = mix_sample(
        &root,
        "zero",
        &[event(zeroed.to_string_lossy().into_owned())],
        0,
        24000,
        &|| false,
    )
    .unwrap();
    let (_, exceptions) = classify(
        Some(&expected.energy),
        &expected.energy,
        &zero.energy,
        true,
        false,
    );
    assert!(
        exceptions
            .iter()
            .any(|e| e == "suspected_source_sound_missing_in_rendered_sample")
    );
    let plan = plan(&catalog, &asset, None);
    let req = request(&catalog, &plan);
    let movie = catalog
        .preview_artifact_status(req.artifact_id.as_deref().unwrap(), false)
        .unwrap()
        .artifact
        .result
        .unwrap()
        .output_path;
    fs::write(movie, b"changed output after completed receipt").unwrap();
    let report = catalog
        .audit_selected_audio(&req, &|| false)
        .unwrap()
        .report;
    assert_eq!(report.state, "partial");
    assert!(report.samples.iter().all(|s| s.state == "unchecked"));
    assert!(
        report
            .exceptions
            .iter()
            .any(|e| e == "preview_bytes_or_inputs_unchecked")
    );
}

#[test]
fn bounded_presence_only_cancellation_and_invalid_sample_windows_do_not_publish_wrong_evidence() {
    let (_dir, catalog, asset) = fixture();
    let plan = plan(&catalog, &asset, None);
    let mut req = request(&catalog, &plan);
    assert!(catalog.audit_selected_audio(&req, &|| true).is_err());
    req.samples[0].movie_out_seconds = 9.0;
    assert!(catalog.audit_selected_audio(&req, &|| false).is_err());
    req.samples.clear();
    req.artifact_id = None;
    let report = catalog
        .audit_selected_audio(&req, &|| false)
        .unwrap()
        .report;
    assert_eq!(report.state, "presenceOnly");
    assert!(report.samples.is_empty());
    assert_eq!(report.sampled_energy_seconds, 0.0);
    assert_eq!(report.unchecked_movie_ranges.len(), 1);
}
