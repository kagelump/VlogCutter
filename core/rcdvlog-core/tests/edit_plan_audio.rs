//! Generated indexed frames and known source tones verify actual split edits.
use rcdvlog_core::{
    Catalog, EditPlanAudio, EditPlanOperation, EditPlanRevision, PreviewRequest, SourceAudio,
    otio_document,
};
use std::{fs, path::Path, process::Command};
use tempfile::{TempDir, tempdir};

fn generated_fixture() -> (TempDir, Catalog, String, Vec<String>) {
    let dir = tempdir().unwrap();
    let media = dir.path().join("indexed sources 日本語");
    fs::create_dir(&media).unwrap();
    for (index, (rate, color, frequency)) in [
        (30, [220, 10, 10], Some("if(lt(t,2),440,880)")),
        (60, [10, 10, 220], Some("660")),
        (24, [10, 180, 10], None),
        (30, [180, 180, 10], Some("220")),
    ]
    .into_iter()
    .enumerate()
    {
        let frames = dir.path().join(format!("indexed-{index}.rgb"));
        let mut bytes = Vec::new();
        for frame in 0..rate * 4 {
            for y in 0..90 {
                for x in 0..160 {
                    // A gray counter patch encodes source-frame time; colors
                    // identify the source independently of the audio waveform.
                    let gray = (20.0 + frame as f64 / rate as f64 * 40.0).round() as u8;
                    let pixel = if x < 24 && y < 24 { [gray; 3] } else { color };
                    bytes.extend_from_slice(&pixel);
                }
            }
        }
        fs::write(&frames, bytes).unwrap();
        let mut command = Command::new("ffmpeg");
        command
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
            ])
            .arg(rate.to_string())
            .arg("-i")
            .arg(&frames);
        if let Some(frequency) = frequency {
            command.args(["-f", "lavfi", "-i"]).arg(format!(
                "aevalsrc='0.1*sin(2*PI*({frequency})*t)':s={}:d={}",
                if index == 0 { 44100 } else { 48000 },
                if index == 3 { 1 } else { 4 }
            ));
        }
        command
            .args([
                "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-t", "4",
            ])
            .arg(media.join(format!("source-{index}.mp4")));
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let catalog = Catalog::open(dir.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Generated J/L review").unwrap();
    assert_eq!(
        catalog.ingest_folder(&project.id, &media).unwrap().imported,
        4
    );
    let mut assets = catalog.list_assets(&project.id).unwrap();
    assets.sort_by(|a, b| a.filename.cmp(&b.filename));
    let ids = assets.into_iter().map(|asset| asset.id).collect();
    (dir, catalog, project.id, ids)
}

fn source_audio(asset: &str, source_in: f64, source_out: f64, timeline: f64) -> EditPlanAudio {
    EditPlanAudio::Source(SourceAudio {
        version: 1,
        asset_id: asset.into(),
        source_in_seconds: source_in,
        source_out_seconds: source_out,
        timeline_start_seconds: timeline,
        sync_offset_seconds: 0.0,
        gain_db: -6.0,
        fade_in_seconds: 0.0,
        fade_out_seconds: 0.0,
    })
}
fn plan(catalog: &Catalog, project: &str, ids: &[String]) -> EditPlanRevision {
    let cuts = vec![
        EditPlanOperation {
            operation_id: "outgoing".into(),
            position: 0,
            asset_id: ids[0].clone(),
            source_in_seconds: 1.0,
            source_out_seconds: 2.0,
            rationale: "retain outgoing thought".into(),
            audio: Some(source_audio(&ids[0], 1.0, 2.5, 0.0)),
        },
        EditPlanOperation {
            operation_id: "incoming".into(),
            position: 1,
            asset_id: ids[1].clone(),
            source_in_seconds: 1.0,
            source_out_seconds: 2.0,
            rationale: "incoming audio leads picture".into(),
            audio: Some(source_audio(&ids[1], 0.5, 2.5, 0.5)),
        },
        EditPlanOperation {
            operation_id: "cover".into(),
            position: 2,
            asset_id: ids[2].clone(),
            source_in_seconds: 0.0,
            source_out_seconds: 1.0,
            rationale: "cover retained dialogue".into(),
            audio: Some(EditPlanAudio::Silent),
        },
    ];
    catalog
        .create_edit_plan_from_base(project, &cuts, None, None, None)
        .unwrap()
}

fn samples(path: &Path, at: f64, duration: f64) -> Vec<f64> {
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .arg("-ss")
        .arg(at.to_string())
        .arg("-t")
        .arg(duration.to_string())
        .args(["-vn", "-ac", "1", "-ar", "48000", "-f", "f32le", "pipe:1"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let values: Vec<_> = output
        .stdout
        .chunks_exact(4)
        .map(|bytes| f64::from(f32::from_le_bytes(bytes.try_into().unwrap())))
        .collect();
    assert!(!values.is_empty());
    values
}
fn amplitude(samples: &[f64], frequency: f64) -> f64 {
    let (sin, cos) = samples
        .iter()
        .enumerate()
        .fold((0.0, 0.0), |(s, c), (i, v)| {
            let phase = 2.0 * std::f64::consts::PI * frequency * i as f64 / 48000.0;
            (s + v * phase.sin(), c + v * phase.cos())
        });
    2.0 * (sin * sin + cos * cos).sqrt() / samples.len() as f64
}
fn pixel(path: &Path, at: f64, x: u32, y: u32) -> [u8; 3] {
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .arg("-ss")
        .arg(at.to_string())
        .args(["-frames:v", "1", "-vf"])
        .arg(format!("format=rgb24,crop=8:8:{x}:{y},scale=1:1"))
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    output.stdout.try_into().unwrap()
}

#[test]
fn actual_render_preserves_j_l_overlaps_covered_audio_and_picture_sync() {
    let (_dir, catalog, project, ids) = generated_fixture();
    let revision = plan(&catalog, &project, &ids);
    let report = catalog.validate_edit_plan(&revision.revision_id).unwrap();
    assert!(report.renderable(), "{:?}", report.findings);
    catalog
        .activate_edit_plan_revision(&revision.revision_id, None)
        .unwrap();
    let result = catalog
        .render_preview(
            &PreviewRequest {
                revision_id: revision.revision_id.clone(),
                max_height: 180,
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap();
    assert!((result.duration_seconds - 3.0).abs() < 0.05);
    let path = Path::new(&result.output_path);
    for (at, expected) in [(0.2, 440.0), (1.7, 660.0), (2.2, 660.0)] {
        let values = samples(path, at, 0.15);
        assert!(
            amplitude(&values, expected) > 0.025,
            "missing source sound {expected} at {at}"
        );
    }
    let overlap = samples(path, 1.2, 0.15);
    assert!(
        amplitude(&overlap, 880.0) > 0.025,
        "outgoing source endpoint was silently relinked to picture"
    );
    assert!(
        amplitude(&overlap, 660.0) > 0.025,
        "incoming source audio lost from overlap"
    );
    let lead = samples(path, 0.7, 0.15);
    assert!(
        amplitude(&lead, 660.0) > 0.025,
        "J cut did not lead incoming picture"
    );
    let silence = samples(path, 2.7, 0.15);
    assert!(silence.iter().map(|value| value * value).sum::<f64>() / (silence.len() as f64) < 1e-6);
    assert!(pixel(path, 0.2, 160, 90)[0] > 180);
    assert!(pixel(path, 1.2, 160, 90)[2] > 180);
    assert!(pixel(path, 2.2, 160, 90)[1] > 140);
    let counter = pixel(path, 0.2, 8, 8)[0];
    assert!(
        (i32::from(counter) - 68).abs() <= 5,
        "source frame time counter differs from the mapped source window: {counter}"
    );
    assert!(
        otio_document(&revision, &catalog.list_assets(&project).unwrap(), "split")
            .unwrap_err()
            .to_string()
            .contains("does not yet support")
    );
    let export_path = _dir.path().join("unsupported.otio");
    assert!(
        catalog
            .export_otio(&rcdvlog_core::OtioExportRequest {
                revision_id: revision.revision_id.clone(),
                output_path: export_path.to_string_lossy().into_owned(),
                expected_existing_digest: None,
                timeline_name: "split".into(),
            })
            .is_err()
    );
    assert!(!export_path.exists());
    assert!(catalog.edit_plan_exports(&project).unwrap().is_empty());
    let snapshot = catalog.export_project_baseline(&project).unwrap();
    let created = snapshot
        .operations
        .iter()
        .find(|op| op.operation_type == "editPlanRevisionCreated")
        .unwrap();
    assert_eq!(
        created.payload_version, 2,
        "older clients must refuse the extended edit contract"
    );
    let mut mislabeled = created.clone();
    mislabeled.operation_id = "mislabeled-audio-v1".into();
    mislabeled.origin_sequence += 100;
    mislabeled.payload_version = 1;
    assert!(
        catalog
            .apply_operation(&mislabeled)
            .unwrap_err()
            .to_string()
            .contains("payload version 2")
    );
    let restored = Catalog::import_project_baseline(
        _dir.path().join("replica.sqlite3"),
        &catalog.export_project_baseline(&project).unwrap(),
        "replica",
        1,
        "fixture",
    )
    .unwrap();
    assert_eq!(
        restored
            .edit_plan_revision(&revision.revision_id)
            .unwrap()
            .operations,
        revision.operations
    );
}

#[test]
fn split_audio_rejects_missing_handles_no_audio_bad_sync_and_excess_overlaps() {
    let (_dir, catalog, project, ids) = generated_fixture();
    let revision = plan(&catalog, &project, &ids);
    for case in 0..9 {
        let mut cuts = revision.operations.clone();
        match case {
            0 => {
                if let Some(EditPlanAudio::Source(audio)) = &mut cuts[0].audio {
                    audio.source_out_seconds = 4.1;
                }
            }
            1 => cuts[0].audio = Some(source_audio(&ids[2], 0.0, 1.0, 0.0)),
            2 => {
                if let Some(EditPlanAudio::Source(audio)) = &mut cuts[0].audio {
                    audio.timeline_start_seconds = 0.2;
                }
            }
            3 => cuts[2].audio = Some(source_audio(&ids[0], 1.0, 2.5, 0.0)),
            4 => {
                if let Some(EditPlanAudio::Source(audio)) = &mut cuts[0].audio {
                    audio.fade_in_seconds = 0.3;
                }
            }
            5 => {
                if let Some(EditPlanAudio::Source(audio)) = &mut cuts[0].audio {
                    audio.version = 99;
                }
            }
            6 => {
                if let Some(EditPlanAudio::Source(audio)) = &mut cuts[0].audio {
                    audio.timeline_start_seconds = 3.0;
                }
            }
            7 => cuts[0].audio = Some(source_audio(&ids[3], 0.0, 2.0, 0.0)),
            8 => cuts[0].source_out_seconds = cuts[0].source_in_seconds + 0.0001,
            _ => unreachable!(),
        }
        let invalid = catalog
            .create_edit_plan_from_base(&project, &cuts, None, Some(&revision.revision_id), None)
            .unwrap();
        let report = catalog.validate_edit_plan(&invalid.revision_id).unwrap();
        assert!(!report.renderable(), "case {case}: {:?}", report.findings);
        assert!(
            report
                .errors()
                .any(|finding| finding.code == "edit_plan_audio_invalid")
        );
        assert!(
            catalog
                .activate_edit_plan_revision(&invalid.revision_id, None)
                .is_err()
        );
        assert_eq!(
            catalog.edit_plan_revision(&invalid.revision_id).unwrap(),
            invalid
        );
    }
    let mut cuts = revision.operations.clone();
    if let Some(EditPlanAudio::Source(audio)) = &mut cuts[0].audio {
        audio.gain_db = -12.0;
    }
    let changed = catalog
        .create_edit_plan_from_base(&project, &cuts, None, Some(&revision.revision_id), None)
        .unwrap();
    let diff = catalog
        .diff_edit_plans(&revision.revision_id, &changed.revision_id)
        .unwrap();
    assert_eq!(diff.changes.len(), 1);
    assert_eq!(diff.changes[0].changes, vec!["audio"]);
    assert_eq!(
        catalog
            .edit_plan_revision(&revision.revision_id)
            .unwrap()
            .operations,
        revision.operations
    );
}

#[test]
fn explicit_fades_gain_and_sync_offsets_are_preserved_without_implicit_ducking() {
    let (_dir, catalog, project, ids) = generated_fixture();
    let revision = plan(&catalog, &project, &ids);
    let mut cuts = revision.operations.clone();
    if let Some(EditPlanAudio::Source(audio)) = &mut cuts[0].audio {
        audio.fade_in_seconds = 0.2;
        audio.gain_db = -12.0;
    }
    let faded = catalog
        .create_edit_plan_from_base(&project, &cuts, None, Some(&revision.revision_id), None)
        .unwrap();
    let result = catalog
        .render_preview(
            &PreviewRequest {
                revision_id: faded.revision_id,
                max_height: 180,
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap();
    let path = Path::new(&result.output_path);
    let attack = amplitude(&samples(path, 0.01, 0.03), 440.0);
    let steady = amplitude(&samples(path, 0.3, 0.1), 440.0);
    assert!(
        steady > 0.015 && steady < 0.035,
        "explicit -12 dB gain missing: {steady}"
    );
    assert!(
        attack < steady * 0.35,
        "explicit fade missing: attack={attack} steady={steady}"
    );
    if let Some(EditPlanAudio::Source(audio)) = &mut cuts[0].audio {
        audio.source_in_seconds = 0.5;
        audio.source_out_seconds = 2.0;
        audio.sync_offset_seconds = -0.5;
    }
    let shifted = catalog
        .create_edit_plan_from_base(&project, &cuts, None, Some(&revision.revision_id), None)
        .unwrap();
    assert!(
        catalog
            .validate_edit_plan(&shifted.revision_id)
            .unwrap()
            .renderable()
    );
    assert_eq!(
        catalog
            .edit_plan_revision(&shifted.revision_id)
            .unwrap()
            .operations,
        cuts
    );
}

#[test]
fn omitted_audio_keeps_linked_sound_inside_a_split_audio_plan() {
    let (_dir, catalog, project, ids) = generated_fixture();
    let revision = plan(&catalog, &project, &ids);
    let mut cuts = revision.operations.clone();
    cuts[0].audio = None;
    cuts[1].audio = Some(EditPlanAudio::Silent);
    let linked = catalog
        .create_edit_plan_from_base(&project, &cuts, None, Some(&revision.revision_id), None)
        .unwrap();
    let result = catalog
        .render_preview(
            &PreviewRequest {
                revision_id: linked.revision_id,
                max_height: 180,
            },
            &mut |_| {},
            &|| false,
        )
        .unwrap();
    let path = Path::new(&result.output_path);
    assert!(amplitude(&samples(path, 0.3, 0.1), 440.0) > 0.07);
    assert!(amplitude(&samples(path, 1.2, 0.1), 660.0) < 0.001);
    assert!(amplitude(&samples(path, 1.2, 0.1), 880.0) < 0.001);
}

#[test]
fn render_claim_and_cancellation_preserve_the_prior_complete_preview() {
    use std::cell::Cell;
    let (_dir, catalog, project, ids) = generated_fixture();
    let revision = plan(&catalog, &project, &ids);
    let request = PreviewRequest {
        revision_id: revision.revision_id,
        max_height: 180,
    };
    let mut checked_claim = false;
    let result = catalog
        .render_preview(
            &request,
            &mut |event| {
                if matches!(event, rcdvlog_core::PreviewProgress::Started { .. }) {
                    let conflict = catalog
                        .render_preview(&request, &mut |_| {}, &|| false)
                        .unwrap_err();
                    assert!(conflict.to_string().contains("already in progress"));
                    checked_claim = true;
                }
            },
            &|| false,
        )
        .unwrap();
    assert!(checked_claim);
    let completed = fs::read(&result.output_path).unwrap();
    let cancelled = Cell::new(false);
    assert!(
        catalog
            .render_preview(
                &request,
                &mut |event| {
                    if matches!(event, rcdvlog_core::PreviewProgress::ClipCompleted { .. }) {
                        cancelled.set(true);
                    }
                },
                &|| cancelled.get()
            )
            .is_err()
    );
    assert_eq!(fs::read(&result.output_path).unwrap(), completed);
    assert!(
        catalog
            .render_preview(&request, &mut |_| {}, &|| false)
            .is_ok(),
        "cancelled writer must release its render claim"
    );
}
