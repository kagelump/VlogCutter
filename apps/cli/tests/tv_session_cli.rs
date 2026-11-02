//! Actual render -> final-mix capture -> resumable CLI search -> shared cut lookup.
//! All media and clock calibration are generated; this is not TV/mic acceptance.
use rcdvlog_core::{
    Catalog, EditPlanOperation, PreviewRequest, full_file_blake3,
    tv_review_sync::{CaptureClock, SyncMovieClock, SyncPolicy, SyncQuery, SyncRequest},
    tv_soundtrack::SoundtrackPolicy,
    tv_soundtrack_session::{SessionAudioSegment, SoundtrackSessionSpec},
};
use serde_json::{Value, json};
use std::{path::Path, process::Command};
fn media(command: &mut Command) {
    let out = command.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
fn invoke(root: &Path, args: &[&str]) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
        .env("RCDVLOG_RESOURCE_LOCK_ROOT", root.join("locks"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    v["data"].clone()
}
#[test]
fn actual_render_capture_resume_and_shared_historical_or_strict_mapping() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let source = root.join("generated source");
    std::fs::create_dir(&source).unwrap();
    media(Command::new("ffmpeg").args(["-v","error","-f","lavfi","-i","testsrc2=s=160x90:r=15:d=24","-f","lavfi","-i",
        "aevalsrc=(0.5+0.4*sin(3.7*t))*(sin(2*PI*(173*t+19*sin(0.9*t)))+0.7*sin(2*PI*(613*t+31*cos(0.57*t))))*0.2:s=48000:d=24",
        "-c:v","libx264","-pix_fmt","yuv420p","-c:a","aac","-shortest"]).arg(source.join("invented.mp4")));
    let catalog = Catalog::open(root.join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Synthetic session E2E").unwrap();
    catalog.ingest_folder(&project.id, &source).unwrap();
    let asset = catalog.list_assets(&project.id).unwrap().remove(0);
    let plan = catalog
        .create_edit_plan_from_base(
            &project.id,
            &(0..2)
                .map(|i| EditPlanOperation {
                    operation_id: format!("cut_{i}"),
                    position: i,
                    asset_id: asset.id.clone(),
                    source_in_seconds: f64::from(i) * 12.0,
                    source_out_seconds: f64::from(i + 1) * 12.0,
                    rationale: "Generated continuous sequence".into(),
                    audio: None,
                })
                .collect::<Vec<_>>(),
            None,
            None,
            None,
        )
        .unwrap();
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
    let handoff = catalog
        .export_review_target(&artifact, None, &|| false)
        .unwrap();
    let historical = catalog
        .review_historical_rendered_artifact(&artifact, &|| false)
        .unwrap();
    let wav = root.join("untouched native capture.wav");
    media(
        Command::new("ffmpeg")
            .args(["-v", "error", "-ss", "9", "-i"])
            .arg(&preview.output_path)
            .args([
                "-t",
                "6",
                "-map",
                "0:a:0",
                "-ac",
                "1",
                "-ar",
                "48000",
                "-c:a",
                "pcm_s16le",
            ])
            .arg(&wav),
    );
    let reader = hound::WavReader::open(&wav).unwrap();
    let frames = reader.duration();
    assert_eq!(reader.spec().sample_rate, 48000);
    assert_eq!(reader.spec().channels, 1);
    assert!(frames >= 288000, "actual decoded native frames: {frames}");
    let m = &handoff.target.movie;
    let spec = SoundtrackSessionSpec {
        schema_version: 1,
        sync: SyncRequest {
            schema_version: 1,
            session_id: "synthetic_e2e_session".into(),
            target_digest: handoff.target_digest.clone(),
            movie: SyncMovieClock {
                movie_digest: m.blake3.clone(),
                time_base_numerator: m.time_base_numerator as u32,
                time_base_denominator: m.time_base_denominator as u32,
                origin_pts: m.origin_pts,
                duration_pts: (m.duration_seconds * m.time_base_denominator as f64
                    / m.time_base_numerator as f64)
                    .round() as u64,
            },
            capture_clocks: vec![CaptureClock {
                clock_id: "native".into(),
                sample_rate: 48000,
                first_sample: 48000,
                last_sample: 48000 + u64::from(frames),
                monotonic_origin_ns: 1_000_000_000,
                monotonic_tolerance_ns: 1,
            }],
            anchors: vec![],
            boundaries: vec![],
            policy: SyncPolicy::default(),
        },
        segments: vec![SessionAudioSegment {
            segment_id: "capture".into(),
            clock_id: "native".into(),
            first_sample: 48000,
            frame_count: frames,
            wav_blake3: full_file_blake3(&wav, &|| false).unwrap().hex_digest,
        }],
        reference_seconds: 12,
        query_seconds: 6,
        query_step_seconds: 6,
        match_policy: SoundtrackPolicy::default(),
    };
    let input = root.join("spec.json");
    std::fs::write(&input, serde_json::to_vec(&spec).unwrap()).unwrap();
    let planned = invoke(
        root,
        &[
            "tv-review",
            "soundtrack-session-plan",
            "--input",
            input.to_str().unwrap(),
        ],
    );
    let plan_path = root.join("plan.json");
    std::fs::write(&plan_path, serde_json::to_vec(&planned).unwrap()).unwrap();
    let files = root.join("bindings.json");
    std::fs::write(
        &files,
        serde_json::to_vec(&json!([{"segmentId":"capture","path":wav}])).unwrap(),
    )
    .unwrap();
    let cache = root.join("private matching cache");
    let run = |max: &str| {
        invoke(
            root,
            &[
                "tv-review",
                "soundtrack-session-run",
                "--plan",
                plan_path.to_str().unwrap(),
                "--movie",
                &preview.output_path,
                "--audio-files",
                files.to_str().unwrap(),
                "--cache",
                cache.to_str().unwrap(),
                "--max-tasks",
                max,
            ],
        )
    };
    assert_eq!(run("1")["nextTask"], 1);
    let queries = root.join("utterances.json");
    std::fs::write(
        &queries,
        serde_json::to_vec(&vec![SyncQuery {
            clock_id: "native".into(),
            start_sample: 192000,
            end_sample: 192000,
        }])
        .unwrap(),
    )
    .unwrap();
    let target = root.join("target.json");
    std::fs::write(&target, serde_json::to_vec(&handoff).unwrap()).unwrap();
    let query = |option: &str| {
        invoke(
            root,
            &[
                "tv-review",
                "soundtrack-session-sync",
                "--plan",
                plan_path.to_str().unwrap(),
                "--cache",
                cache.to_str().unwrap(),
                "--queries",
                queries.to_str().unwrap(),
                option,
                target.to_str().unwrap(),
            ],
        )
    };
    let pending = query("--target");
    assert_eq!(pending["evidence"]["searchComplete"], false);
    assert_eq!(pending["report"]["slices"][0]["state"], "unknown");
    let completed = run("128");
    assert_eq!(completed["reused"], 1);
    assert_eq!(completed["nextTask"], planned["taskCount"]);
    let mapped = query("--target");
    assert_eq!(mapped["mappingPurpose"], "strictVerifiedReviewTarget");
    assert_eq!(mapped["evidence"]["searchComplete"], true);
    assert_eq!(
        mapped["report"]["slices"][0]["state"], "anchored",
        "{mapped}"
    );
    assert_eq!(
        mapped["report"]["slices"][0]["candidateOperationIds"],
        json!(["cut_0", "cut_1"])
    );
    std::fs::write(&target, serde_json::to_vec(&historical).unwrap()).unwrap();
    let mapped = query("--historical-target");
    assert_eq!(mapped["mappingPurpose"], "historicalRenderedArtifactReview");
    assert_eq!(
        mapped["report"]["slices"][0]["candidateOperationIds"],
        json!(["cut_0", "cut_1"])
    );
    assert!(
        rcdvlog_core::ReviewTargetHandoff::from_json(&std::fs::read(&target).unwrap()).is_err()
    );
    assert_eq!(catalog.active_edit_plan(&project.id).unwrap(), None);
    assert_eq!(catalog.edit_plan_revision(&plan.revision_id).unwrap(), plan);
}
