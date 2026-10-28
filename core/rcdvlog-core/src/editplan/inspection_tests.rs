//! Generated numbered/time-coded pictures and sound prove inspection evidence.
use crate::{
    Catalog, InspectionPurpose, InspectionRepresentation, SourceInspectionError,
    SourceInspectionRequest,
};
use std::{
    fs,
    path::Path,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};
use tempfile::{TempDir, tempdir};

fn fixture() -> (TempDir, Catalog, String) {
    let dir = tempdir().unwrap();
    let media = dir.path().join("original sources 日本語");
    fs::create_dir(&media).unwrap();
    let raw = dir.path().join("numbered.rgb");
    let mut bytes = Vec::new();
    // 60 source frames/s: the gray field encodes exact frame time, a red top
    // left corner establishes orientation, black ranges model covered edges.
    for n in 0..240 {
        for y in 0..90 {
            for x in 0..160 {
                let gray = (30.0 + n as f64 * 0.7).round() as u8;
                let rgb = if x < 20 && y < 20 {
                    [220, 10, 10]
                } else if n < 15 || (150..165).contains(&n) || n >= 225 {
                    [0, 0, 0]
                } else {
                    [gray; 3]
                };
                bytes.extend_from_slice(&rgb);
            }
        }
    }
    fs::write(&raw, bytes).unwrap();
    let output = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "rawvideo",
            "-pixel_format",
            "rgb24",
            "-video_size",
            "160x90",
            "-framerate",
            "60",
            "-i",
        ])
        .arg(raw)
        .args([
            "-f",
            "lavfi",
            "-i",
            "aevalsrc='if(lt(t,1.5),0,0.1*sin(2*PI*440*t))':s=44100:d=4",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-t",
            "4",
        ])
        .arg(media.join("numbered.mp4"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let catalog = Catalog::open(dir.path().join("catalog.sqlite3")).unwrap();
    let project = catalog
        .create_project("Public generated source inspection")
        .unwrap();
    assert_eq!(
        catalog.ingest_folder(&project.id, &media).unwrap().imported,
        1
    );
    let id = catalog.list_assets(&project.id).unwrap()[0].id.clone();
    (dir, catalog, id)
}
fn request(asset: &str) -> SourceInspectionRequest {
    SourceInspectionRequest {
        asset_id: asset.into(),
        representation: InspectionRepresentation::Original,
        purpose: InspectionPurpose::ProposedBoundary,
        source_in_seconds: 1.0,
        source_out_seconds: 2.0,
        before_seconds: 0.25,
        after_seconds: 0.25,
        sample_count: 5,
        max_width: 160,
        source_times_seconds: vec![],
        exact_representation_pts: vec![],
        expected_representation_fingerprint: None,
        include_audio: false,
        include_transcript: false,
    }
}
fn gray(path: &str) -> u8 {
    let output = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            path,
            "-vf",
            "scale=160:90",
            "-frames:v",
            "1",
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "-",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    output.stdout[(45 * 160 + 80) * 3]
}

#[test]
fn actual_frame_pts_handles_exact_requests_audio_context_and_verified_reuse() {
    let (_dir, catalog, id) = fixture();
    let mut r = request(&id);
    r.include_audio = true;
    r.include_transcript = true;
    let first = catalog.inspect_source_window(&r, &|| false).unwrap();
    assert!(!first.reused);
    let evidence = &first.inspection;
    assert_eq!(evidence.window_in_seconds, 0.75);
    assert_eq!(evidence.window_out_seconds, 2.25);
    assert_eq!(evidence.mapping.precision, "actualSourcePts");
    assert_eq!(evidence.frames.len(), 5);
    for frame in &evidence.frames {
        let tick = frame.representation_pts as f64 * evidence.mapping.time_base_numerator as f64
            / evidence.mapping.time_base_denominator as f64;
        assert!((tick - frame.source_seconds).abs() < 1e-9);
        assert!((frame.excerpt_seconds - (frame.source_seconds - 0.75)).abs() < 1e-9);
        let expected = (30.0 + frame.source_seconds * 60.0 * 0.7).round() as i16;
        assert!(
            (i16::from(gray(&frame.image.path)) - expected).abs() < 8,
            "actual image does not match reported PTS: {frame:?}"
        );
    }
    let audio = evidence.audio.as_ref().unwrap();
    assert_eq!(audio.state, "complete");
    assert_eq!(audio.source_in_samples, 36000);
    assert_eq!(audio.source_out_samples, 108000);
    assert!(Path::new(&audio.playable.as_ref().unwrap().path).is_file());
    assert!(
        audio
            .waveform
            .iter()
            .any(|b| b.low_energy && b.excerpt_end_seconds < 0.6)
    );
    assert!(
        audio
            .waveform
            .iter()
            .any(|b| !b.low_energy && b.excerpt_start_seconds > 0.8)
    );
    assert!(evidence.utterances.is_empty());
    assert!(
        evidence
            .limitations
            .iter()
            .any(|s| s.contains("word timing is unavailable"))
    );
    assert_eq!(
        catalog
            .inspect_source_window(&r, &|| false)
            .unwrap()
            .inspection,
        *evidence
    );
    assert!(catalog.inspect_source_window(&r, &|| false).unwrap().reused);
    // Request exactly a previously decoded frame. It stays on its rational
    // representation clock and is bound to that exact representation hash.
    r.sample_count = 0;
    r.exact_representation_pts = vec![evidence.frames[2].representation_pts];
    r.expected_representation_fingerprint = Some(evidence.representation_fingerprint.clone());
    let precise = catalog
        .inspect_source_window(&r, &|| false)
        .unwrap()
        .inspection;
    assert_eq!(precise.frames.len(), 1);
    assert_eq!(
        precise.frames[0].representation_pts,
        evidence.frames[2].representation_pts
    );
    assert_eq!(
        gray(&precise.frames[0].image.path),
        gray(&evidence.frames[2].image.path)
    );
    r.expected_representation_fingerprint = Some("changed".into());
    assert!(matches!(
        catalog
            .inspect_source_window(&r, &|| false)
            .unwrap_err()
            .downcast_ref::<SourceInspectionError>(),
        Some(SourceInspectionError::StaleEvidence)
    ));
}

#[test]
fn exact_range_orientation_and_file_content_invalidate_evidence_without_mutating_judgments() {
    let (_dir, catalog, id) = fixture();
    let mut r = request(&id);
    let first = catalog
        .inspect_source_window(&r, &|| false)
        .unwrap()
        .inspection;
    fs::write(&first.frames[0].image.path, b"tampered JPEG").unwrap();
    let repair = catalog.inspect_source_window(&r, &|| false).unwrap();
    assert!(!repair.reused);
    assert_eq!(repair.inspection.evidence_id, first.evidence_id);
    assert_ne!(
        repair.inspection.frames[0].image.path,
        first.frames[0].image.path
    );
    r.source_in_seconds += 1.0 / 60.0;
    let range = catalog
        .inspect_source_window(&r, &|| false)
        .unwrap()
        .inspection;
    assert_ne!(range.evidence_id, first.evidence_id);
    let mut asset = catalog.get_asset(&id).unwrap();
    catalog
        .update_asset(
            &id,
            &crate::AssetPatch {
                notes: asset.notes.clone(),
                labels: asset.labels.clone(),
                removed: asset.removed,
                rotation_degrees: 90,
                expected_revision: Some(asset.revision),
            },
        )
        .unwrap();
    let rotated = catalog
        .inspect_source_window(&r, &|| false)
        .unwrap()
        .inspection;
    assert_eq!(rotated.rotation_degrees, 90);
    assert_ne!(rotated.evidence_id, range.evidence_id);
    let image = &rotated.frames[0].image.path;
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=width,height",
            "-of",
            "json",
            image,
        ])
        .output()
        .unwrap();
    let probe: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    assert_eq!(probe["streams"][0]["width"], 90);
    assert_eq!(probe["streams"][0]["height"], 160);
    asset = catalog.get_asset(&id).unwrap();
    assert_eq!(asset.rotation_degrees, 90);
    assert!(!asset.removed);
    assert_eq!(asset.notes, "");
    // Covered opening/middle and dark/close-up-style images remain evidence,
    // never automatic defect, rotation, removal, or accepted-range writes.
    r.source_in_seconds = 0.0;
    r.source_out_seconds = 4.0;
    r.before_seconds = 0.0;
    r.after_seconds = 0.0;
    r.source_times_seconds = vec![0.1, 2.6, 3.9];
    let covered = catalog
        .inspect_source_window(&r, &|| false)
        .unwrap()
        .inspection;
    assert!(covered.max_unsampled_gap_seconds > 0.1);
    assert!(covered.limitations.iter().any(|s| s.contains("No defect")));
    assert!(covered.audio.is_none());
    assert!(
        catalog
            .get_asset(&id)
            .unwrap()
            .orientation_decision
            .is_none()
    );
}

#[test]
fn bounds_explicit_representation_and_cancellation_fail_closed() {
    let (_dir, catalog, id) = fixture();
    let mut r = request(&id);
    r.before_seconds = 2.0;
    assert!(catalog.inspect_source_window(&r, &|| false).is_err());
    r = request(&id);
    r.source_out_seconds = 61.0;
    assert!(catalog.inspect_source_window(&r, &|| false).is_err());
    r = request(&id);
    r.sample_count = 65;
    assert!(catalog.inspect_source_window(&r, &|| false).is_err());
    r = request(&id);
    r.include_transcript = true;
    assert!(catalog.inspect_source_window(&r, &|| false).is_err());
    r = request(&id);
    r.exact_representation_pts = vec![0];
    assert!(catalog.inspect_source_window(&r, &|| false).is_err());
    r = request(&id);
    r.representation = InspectionRepresentation::GeneratedProxy;
    assert!(
        catalog.inspect_source_window(&r, &|| false).is_err(),
        "must not fall back to original"
    );
    r.representation = InspectionRepresentation::CameraProxy;
    assert!(catalog.inspect_source_window(&r, &|| false).is_err());
    r = request(&id);
    let calls = AtomicUsize::new(0);
    let cancelled =
        catalog.inspect_source_window(&r, &|| calls.fetch_add(1, Ordering::SeqCst) > 20);
    assert!(cancelled.is_err());
    // Failed/cancelled work never becomes reusable completed evidence, and the
    // heavy/inspection claims release so another request can finish.
    assert!(!catalog.inspect_source_window(&r, &|| false).unwrap().reused);
    assert!(catalog.inspect_source_window(&r, &|| false).unwrap().reused);
}

#[test]
fn reduced_rate_proxy_never_claims_missing_source_frames_and_transcript_is_explicit_current_segment_evidence()
 {
    use crate::analysis::{
        quality::DecodedSegment,
        store::{StoredTranscript, save_transcript},
    };
    use crate::{STT_RECIPE_KEY, TranscriptOutcomeReason, TranscriptStatus};
    let (_dir, catalog, id) = fixture();
    let asset = catalog.get_asset(&id).unwrap();
    let write = catalog.begin_proxy_write(&id).unwrap().unwrap();
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-noautorotate", "-i"])
        .arg(&asset.path)
        .args([
            "-vf", "fps=30", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac",
        ])
        .arg(write.temporary_path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    catalog.commit_proxy_write(write, None).unwrap();
    let mut r = request(&id);
    r.representation = InspectionRepresentation::GeneratedProxy;
    r.sample_count = 64;
    r.source_in_seconds = 1.0;
    r.source_out_seconds = 1.2;
    r.before_seconds = 0.0;
    r.after_seconds = 0.0;
    let proxy = catalog
        .inspect_source_window(&r, &|| false)
        .unwrap()
        .inspection;
    assert_eq!(proxy.mapping.precision, "secondsCorrespondence");
    assert!(
        proxy.frames.len() <= 6,
        "64 requests cannot recover discarded 60fps frames"
    );
    assert!(
        proxy
            .frames
            .windows(2)
            .all(|p| p[0].representation_pts < p[1].representation_pts)
    );
    let connection = rusqlite::Connection::open(catalog.database_path()).unwrap();
    let mut transcript = StoredTranscript {
        asset_id: id.clone(),
        recipe_key: STT_RECIPE_KEY.into(),
        source_fingerprint: catalog.proxy_fingerprint(&id).unwrap(),
        status: TranscriptStatus::Speech,
        outcome_reason: TranscriptOutcomeReason::Accepted,
        reject_reason: String::new(),
        primary_language: "en".into(),
        speech_seconds: 1.0,
        window_count: 1,
        provider_segment_count: 1,
        mean_token_probability: Some(0.9),
        repetition_ratio: Some(0.0),
        segments: vec![DecodedSegment {
            start_ms: 900,
            end_ms: 1500,
            language: "en".into(),
            text: "Synthetic timing contract only".into(),
            mean_token_probability: Some(0.9),
        }],
    };
    save_transcript(&connection, &transcript).unwrap();
    // Frames-only requests do not read/transmit newly stored transcript text.
    let silent = catalog.inspect_source_window(&r, &|| false).unwrap();
    assert!(silent.reused);
    assert_eq!(silent.inspection.evidence_id, proxy.evidence_id);
    assert!(silent.inspection.utterances.is_empty());
    r.include_audio = true;
    r.include_transcript = true;
    let with_text = catalog
        .inspect_source_window(&r, &|| false)
        .unwrap()
        .inspection;
    assert_eq!(with_text.utterances.len(), 1);
    assert!(with_text.utterances[0].clipped_by_window);
    assert_eq!(with_text.utterances[0].source_in_seconds, 0.9);
    assert_eq!(with_text.utterances[0].excerpt_in_seconds, 0.0);
    assert!((with_text.utterances[0].excerpt_out_seconds - 0.2).abs() < 1e-9);
    transcript.recipe_key = "stale recipe".into();
    save_transcript(&connection, &transcript).unwrap();
    let stale = catalog
        .inspect_source_window(&r, &|| false)
        .unwrap()
        .inspection;
    assert!(stale.utterances.is_empty());
    assert_ne!(stale.evidence_id, with_text.evidence_id);
    transcript.recipe_key = STT_RECIPE_KEY.into();
    transcript.source_fingerprint = "stale bytes".into();
    save_transcript(&connection, &transcript).unwrap();
    assert!(
        catalog
            .inspect_source_window(&r, &|| false)
            .unwrap()
            .inspection
            .utterances
            .is_empty()
    );
}

#[test]
fn nonzero_container_origin_remaps_to_zero_source_and_delayed_audio_preserves_its_gap() {
    let (dir, catalog, id) = fixture();
    let original = catalog.get_asset(&id).unwrap().path;
    let media = dir.path().join("offset fixtures");
    fs::create_dir(&media).unwrap();
    let nonzero = media.join("nonzero.mp4");
    let output = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            &original,
            "-an",
            "-c:v",
            "copy",
            "-output_ts_offset",
            "5",
        ])
        .arg(&nonzero)
        .output()
        .unwrap();
    assert!(output.status.success());
    let delayed = media.join("delayed.mp4");
    let output = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            &original,
            "-itsoffset",
            "1.0",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=3",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c:v",
            "copy",
            "-c:a",
            "aac",
        ])
        .arg(&delayed)
        .output()
        .unwrap();
    assert!(output.status.success());
    let project = catalog.create_project("Offset contract").unwrap();
    assert_eq!(
        catalog.ingest_folder(&project.id, &media).unwrap().imported,
        2
    );
    let assets = catalog.list_assets(&project.id).unwrap();
    let offset = assets.iter().find(|a| a.filename == "nonzero.mp4").unwrap();
    let mut r = request(&offset.id);
    r.include_audio = true;
    let inspection = catalog
        .inspect_source_window(&r, &|| false)
        .unwrap()
        .inspection;
    assert!(inspection.mapping.representation_origin_pts > 0);
    assert_eq!(inspection.audio.as_ref().unwrap().state, "noAudioStream");
    for frame in &inspection.frames {
        assert!(
            (frame.representation_pts as f64 * inspection.mapping.time_base_numerator as f64
                / inspection.mapping.time_base_denominator as f64
                - 5.0
                - frame.source_seconds)
                .abs()
                < 1e-9
        );
        assert!(
            (i16::from(gray(&frame.image.path))
                - (30.0 + frame.source_seconds * 60.0 * 0.7).round() as i16)
                .abs()
                < 8
        );
    }
    let delayed = assets.iter().find(|a| a.filename == "delayed.mp4").unwrap();
    r = request(&delayed.id);
    r.source_in_seconds = 0.5;
    r.source_out_seconds = 2.5;
    r.before_seconds = 0.0;
    r.after_seconds = 0.0;
    r.include_audio = true;
    let inspected = catalog
        .inspect_source_window(&r, &|| false)
        .unwrap()
        .inspection;
    let audio = inspected.audio.unwrap();
    assert_eq!(audio.state, "completeWithLeadingGap");
    assert!((audio.stream_source_in_seconds.unwrap() - 0.978).abs() < 0.001);
    assert!((audio.leading_gap_seconds - 0.478).abs() < 0.001);
    assert!(
        audio
            .waveform
            .iter()
            .filter(|b| b.excerpt_end_seconds < 0.45)
            .all(|b| b.low_energy)
    );
    assert!(
        audio
            .waveform
            .iter()
            .filter(|b| b.excerpt_start_seconds > 0.6)
            .all(|b| !b.low_energy)
    );
    // The WAV remains on the source clock: the delayed tone is not shifted to
    // the start merely because audio has a different stream origin.
    let decoded = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            &audio.playable.unwrap().path,
            "-f",
            "s16le",
            "-c:a",
            "pcm_s16le",
            "-",
        ])
        .output()
        .unwrap();
    assert!(decoded.status.success());
    assert_eq!(decoded.stdout.len(), 96000 * 4);
}
