use super::*;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

fn tools_available() -> bool {
    crate::tool_command("ffmpeg")
        .arg("-version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
        && crate::tool_command("ffprobe")
            .arg("-version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
}

fn make_source_with_audio(directory: &Path) -> PathBuf {
    let unrotated = directory.join("unrotated.mp4");
    let rotated = directory.join("source.mp4");
    let status = crate::tool_command("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=24:duration=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(&unrotated)
        .status()
        .unwrap();
    assert!(status.success(), "synthetic source generation failed");
    let status = crate::tool_command("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-display_rotation:v:0",
            "90",
            "-i",
        ])
        .arg(&unrotated)
        .args(["-c", "copy"])
        .arg(&rotated)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "synthetic rotated source generation failed"
    );
    rotated
}

fn software_plan() -> ProxyEncoderPlan {
    ProxyEncoderPlan {
        policy: ProxyEncoderPolicy::Software,
        preferred_encoder: ProxyEncoderKind::Software,
        targets: vec![ProxyEncoderTarget::software()],
        hardware_unavailable: None,
        software_usable: true,
        limitations: vec![],
    }
}

#[cfg(unix)]
fn fake_ffmpeg(directory: &Path, name: &str, behavior: &str) -> PathBuf {
    let real = crate::resolve_tool_path("ffmpeg");
    let path = directory.join(name);
    let script = format!(
        "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = h264_vaapi ]; then\n{behavior}\n  fi\ndone\nexec \"{}\" \"$@\"\n",
        real.display()
    );
    let mut file = fs::File::create(&path).unwrap();
    file.write_all(script.as_bytes()).unwrap();
    let mut permissions = file.metadata().unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

#[cfg(unix)]
#[test]
fn cancellation_kills_a_silent_encoder_and_does_not_start_fallback() {
    let directory = TempDir::new().unwrap();
    // Execute a checked-in fixture rather than writing an executable just
    // before spawn. Parallel tests fork children, which can briefly inherit
    // a writable script descriptor and make Linux execve fail with ETXTBSY.
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/silent-encoder.sh");
    let output = directory.path().join("canceled.mp4");
    // Signal only after the child process has actually started. Cancel on
    // that handshake rather than an absolute wall-clock deadline so a slow
    // process start cannot race with the fallback decision.
    let started_marker = PathBuf::from(format!("{}.started", output.display()));
    let mut plan = software_plan();
    // A cancellation must suppress even an otherwise valid fallback.
    plan.targets.push(ProxyEncoderTarget::software());
    let started = Instant::now();
    let failure = run_proxy_transcode_with_plan_and_cancellation(
        &script,
        Path::new("unused-ffprobe"),
        Path::new("unused-source"),
        &output,
        1.0,
        false,
        &plan,
        |_| {},
        || started_marker.exists(),
    )
    .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(
        failure.message.contains("canceled"),
        "unexpected failure message: {}",
        failure.message
    );
    assert_eq!(
        failure.outcome.attempts.len(),
        1,
        "cancellation must suppress fallback; message={}; attempts={:#?}",
        failure.message,
        failure.outcome.attempts
    );
    assert!(
        failure.outcome.attempts[0]
            .diagnostic
            .as_deref()
            .is_some_and(|diagnostic| diagnostic.contains("canceled")),
        "initial encoder attempt did not end by cancellation; message={}; attempts={:#?}",
        failure.message,
        failure.outcome.attempts
    );
    assert!(!output.exists());
}

#[test]
fn damaged_h264_packets_fail_full_decode_despite_valid_headers() {
    if !tools_available() {
        return;
    }
    let directory = TempDir::new().unwrap();
    make_source_with_audio(directory.path());
    let source = directory.path().join("unrotated.mp4");
    let ffmpeg = crate::resolve_tool_path("ffmpeg");
    let ffprobe = crate::resolve_tool_path("ffprobe");
    let packets = Command::new(&ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_packets",
            "-show_entries",
            "packet=pos,size",
            "-of",
            "json",
        ])
        .arg(&source)
        .output()
        .unwrap();
    let packets: Value = serde_json::from_slice(&packets.stdout).unwrap();
    let packet = &packets["packets"][1];
    let position: usize = packet["pos"].as_str().unwrap().parse().unwrap();
    let size: usize = packet["size"].as_str().unwrap().parse().unwrap();
    let mut bytes = fs::read(&source).unwrap();
    // Corrupt a complete noninitial AVCC sample, including its NAL length
    // fields. The moov headers remain readable, but the video bitstream
    // cannot be decoded.
    bytes[position..position + size].fill(0xff);
    let damaged = directory.path().join("damaged.mp4");
    fs::write(&damaged, bytes).unwrap();
    assert_eq!(
        crate::require_video_stream(&ffprobe_document(&ffprobe, &damaged).unwrap()).unwrap()["codec_name"],
        "h264"
    );
    let error = validate_proxy_output(&ffmpeg, &ffprobe, &damaged, 1.0, true).unwrap_err();
    assert!(format!("{error:#}").contains("decode"), "{error:#}");
}

#[test]
fn truncated_audio_is_rejected_even_when_video_and_audio_headers_are_valid() {
    if !tools_available() {
        return;
    }
    let directory = TempDir::new().unwrap();
    let source = make_source_with_audio(directory.path());
    let output = directory.path().join("short-audio.mp4");
    let ffprobe = crate::resolve_tool_path("ffprobe");
    let status = crate::tool_command("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(&source)
        .args([
            "-display_rotation:v:0",
            "0",
            "-f",
            "lavfi",
            "-i",
            "sine=duration=0.2",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c:v",
            "libx264",
            "-c:a",
            "aac",
            "-map_metadata",
            "-1",
        ])
        .arg(&output)
        .status()
        .unwrap();
    assert!(status.success());
    let error = validate_source_correspondence(&ffprobe, &source, &output).unwrap_err();
    assert!(format!("{error:#}").contains("audio duration"), "{error:#}");
}

#[test]
fn policy_parsing_is_explicit_and_rejects_unknown_values() {
    assert_eq!(
        ProxyEncoderPolicy::parse("auto").unwrap(),
        ProxyEncoderPolicy::Auto
    );
    assert_eq!(
        ProxyEncoderPolicy::parse("software").unwrap(),
        ProxyEncoderPolicy::Software
    );
    assert!(ProxyEncoderPolicy::parse("vaapi").is_err());
    assert_eq!(ProxyEncoderPolicy::default(), ProxyEncoderPolicy::Auto);
}

#[cfg(target_os = "linux")]
#[test]
fn rate_history_keys_separate_actual_encoders_and_worker_counts() {
    assert_eq!(
        proxy_rate_history_key(ProxyEncoderKind::Software, 2),
        "proxy.h264-720p.orientation-v3.linux-software-w2-v1"
    );
    assert_eq!(
        proxy_rate_history_key(ProxyEncoderKind::Vaapi, 1),
        "proxy.h264-720p.orientation-v3.linux-vaapi-qp23-w1-v1"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn vaapi_recipe_uses_one_device_for_hardware_decode_scale_and_encode() {
    let target = ProxyEncoderTarget::vaapi("/dev/dri/renderD128".to_string());
    let command = proxy_ffmpeg_command_with_program(
        Path::new("ffmpeg"),
        Path::new("source.mp4"),
        Path::new("proxy.mp4"),
        &target,
    );
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-init_hw_device", "vaapi=va:/dev/dri/renderD128"])
    );
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-filter_hw_device", "va"])
    );
    assert!(args.windows(2).any(|pair| pair == ["-hwaccel", "vaapi"]));
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-hwaccel_output_format", "vaapi"])
    );
    assert!(args.windows(2).any(|pair| pair == ["-c:v", "h264_vaapi"]));
    assert!(args.iter().any(|arg| arg.starts_with("scale_vaapi=")));
    assert!(args.windows(2).any(|pair| pair == ["-map", "0:a:0?"]));
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-display_rotation:v:0", "0"])
    );
    assert!(args.iter().any(|arg| arg == "-noautorotate"));
    assert!(args.windows(2).any(|pair| pair == ["-map_metadata", "-1"]));
}

#[test]
fn software_smoke_validates_source_time_audio_and_raw_orientation() {
    if !tools_available() {
        eprintln!("ffmpeg or ffprobe unavailable; skipping software proxy smoke test");
        return;
    }
    let directory = TempDir::new().unwrap();
    let source = make_source_with_audio(directory.path());
    let output = directory.path().join("proxy.mp4");
    let ffmpeg = crate::resolve_tool_path("ffmpeg");
    let ffprobe = crate::resolve_tool_path("ffprobe");
    let source_audio = probe_source_audio_present(&ffprobe, &source).unwrap();
    assert!(source_audio);
    let outcome = run_proxy_transcode_with_plan(
        &ffmpeg,
        &ffprobe,
        &source,
        &output,
        1.0,
        source_audio,
        &software_plan(),
        |_| {},
    )
    .unwrap();
    assert_eq!(outcome.selected_encoder, "software");
    assert_eq!(outcome.recipe_key, PROXY_SOFTWARE_RECIPE_KEY);
    let validation = outcome.validation.as_ref().unwrap();
    assert!(validation.decoded);
    assert_eq!(validation.metadata_rotation_degrees, 0);
    assert_eq!(validation.audio_stream_count, 1);
    assert_eq!(validation.audio_codec.as_deref(), Some("aac"));
    assert!((validation.duration_seconds - 1.0).abs() <= validation.duration_tolerance_seconds);
    assert!(output.is_file());
    assert!(probe_source_audio_present(&ffprobe, &output).unwrap());
}

#[cfg(target_os = "linux")]
#[test]
fn usable_linux_hardware_runs_the_full_validated_recipe_on_generated_media() {
    if !tools_available() {
        return;
    }
    clear_proxy_encoder_probe_cache();
    let probe = probe_proxy_encoder_capabilities(ProxyEncoderPolicy::Auto);
    let Some(hardware) = probe.selected_hardware_target() else {
        eprintln!("VAAPI hardware unavailable; hardware smoke coverage explicitly skipped");
        return;
    };
    let directory = TempDir::new().unwrap();
    let source = make_source_with_audio(directory.path());
    let output = directory.path().join("hardware-proxy.mp4");
    let plan = ProxyEncoderPlan {
        policy: ProxyEncoderPolicy::Auto,
        preferred_encoder: ProxyEncoderKind::Vaapi,
        targets: vec![hardware, ProxyEncoderTarget::software()],
        hardware_unavailable: None,
        software_usable: probe.software.usable,
        limitations: probe.limitations,
    };
    let outcome = run_proxy_transcode_with_plan(
        &crate::resolve_tool_path("ffmpeg"),
        &crate::resolve_tool_path("ffprobe"),
        &source,
        &output,
        1.0,
        true,
        &plan,
        |_| {},
    )
    .unwrap();
    assert_eq!(outcome.selected_encoder, "vaapi");
    assert_eq!(outcome.recipe_key, PROXY_VAAPI_RECIPE_KEY);
    assert!(!outcome.fallback_used);
    let validation = outcome.validation.as_ref().unwrap();
    assert!(validation.decoded);
    assert_eq!(validation.audio_stream_count, 1);
    assert_eq!(validation.metadata_rotation_degrees, 0);
    assert!(output.is_file());
}

#[test]
fn hardware_probe_never_claims_usable_without_initialization() {
    #[cfg(target_os = "linux")]
    {
        clear_proxy_encoder_probe_cache();
        let report = probe_proxy_encoder_capabilities(ProxyEncoderPolicy::Auto);
        assert_eq!(report.platform, "linux");
        assert!(!report.hardware.is_empty());
        for candidate in &report.hardware {
            if candidate.usable {
                assert!(candidate.initialization_tested);
                assert_eq!(candidate.device_accessible, Some(true));
                assert_eq!(candidate.status, "usable");
            } else {
                assert!(!candidate.status.is_empty());
                assert!(!candidate.detail.is_empty());
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let report = probe_proxy_encoder_capabilities(ProxyEncoderPolicy::Software);
        assert_eq!(report.selected_encoder, "software");
        assert_eq!(report.hardware[0].status, "not_requested");
    }
}

#[test]
fn non_h264_hardware_output_is_rejected_by_strict_validation() {
    if !tools_available() {
        return;
    }
    let directory = TempDir::new().unwrap();
    let source = directory.path().join("source.mp4");
    let status = crate::tool_command("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=navy:s=320x180:r=10:d=1",
            "-c:v",
            "mpeg4",
        ])
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success());
    let error = validate_proxy_output(
        &crate::resolve_tool_path("ffmpeg"),
        &crate::resolve_tool_path("ffprobe"),
        &source,
        1.0,
        false,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("h264"));
}

#[cfg(unix)]
#[test]
fn failed_hardware_process_removes_staging_and_falls_back_to_software() {
    if !tools_available() {
        return;
    }
    let directory = TempDir::new().unwrap();
    let source = make_source_with_audio(directory.path());
    let output = directory.path().join("proxy.mp4");
    let ffmpeg = fake_ffmpeg(
        directory.path(),
        "fake-ffmpeg-process-failure",
        "echo 'synthetic VAAPI device initialization failed' >&2\n    exit 1",
    );
    let ffprobe = crate::resolve_tool_path("ffprobe");
    let plan = ProxyEncoderPlan {
        policy: ProxyEncoderPolicy::Auto,
        preferred_encoder: ProxyEncoderKind::Vaapi,
        targets: vec![
            ProxyEncoderTarget::vaapi("/dev/dri/renderD128".to_string()),
            ProxyEncoderTarget::software(),
        ],
        hardware_unavailable: None,
        software_usable: true,
        limitations: vec![],
    };
    let outcome = run_proxy_transcode_with_plan(
        &ffmpeg,
        &ffprobe,
        &source,
        &output,
        1.0,
        true,
        &plan,
        |_| {},
    )
    .unwrap();
    assert_eq!(outcome.selected_encoder, "software");
    assert_eq!(outcome.recipe_key, PROXY_SOFTWARE_RECIPE_KEY);
    assert!(outcome.fallback_used);
    assert!(outcome.attempts.iter().any(|attempt| {
        attempt.encoder == "vaapi"
            && attempt.status == ProxyAttemptStatus::ProcessFailed
            && attempt
                .diagnostic
                .as_deref()
                .is_some_and(|value| value.contains("synthetic VAAPI"))
    }));
    assert!(output.is_file());
}

#[cfg(unix)]
#[test]
fn invalid_hardware_output_is_removed_and_software_retry_is_validated() {
    if !tools_available() {
        return;
    }
    let directory = TempDir::new().unwrap();
    let source = make_source_with_audio(directory.path());
    let output = directory.path().join("proxy.mp4");
    let ffmpeg = fake_ffmpeg(
        directory.path(),
        "fake-ffmpeg-invalid-output",
        "printf 'not media' > \"$last\"\n    exit 0",
    );
    // The generic behavior above does not know the output argument. Replace
    // it with a script that finds the final argument and writes invalid bytes.
    let real = crate::resolve_tool_path("ffmpeg");
    let script = format!(
        "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = h264_vaapi ]; then\n    for last; do :; done\n    printf 'not media' > \"$last\"\n    exit 0\n  fi\ndone\nexec \"{}\" \"$@\"\n",
        real.display()
    );
    fs::write(&ffmpeg, script).unwrap();
    let mut permissions = fs::metadata(&ffmpeg).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&ffmpeg, permissions).unwrap();
    let ffprobe = crate::resolve_tool_path("ffprobe");
    let plan = ProxyEncoderPlan {
        policy: ProxyEncoderPolicy::Auto,
        preferred_encoder: ProxyEncoderKind::Vaapi,
        targets: vec![
            ProxyEncoderTarget::vaapi("/dev/dri/renderD128".to_string()),
            ProxyEncoderTarget::software(),
        ],
        hardware_unavailable: None,
        software_usable: true,
        limitations: vec![],
    };
    let outcome = run_proxy_transcode_with_plan(
        &ffmpeg,
        &ffprobe,
        &source,
        &output,
        1.0,
        true,
        &plan,
        |_| {},
    )
    .unwrap();
    assert_eq!(outcome.selected_encoder, "software");
    assert!(outcome.fallback_used);
    assert!(outcome.attempts.iter().any(|attempt| {
        attempt.encoder == "vaapi" && attempt.status == ProxyAttemptStatus::ValidationFailed
    }));
    assert!(output.is_file());
}

#[cfg(unix)]
#[test]
fn software_only_failure_records_no_fallback_claim() {
    if !tools_available() {
        return;
    }
    let directory = TempDir::new().unwrap();
    let source = make_source_with_audio(directory.path());
    let output = directory.path().join("proxy.mp4");
    let ffmpeg = directory.path().join("fake-software-fail");
    fs::write(&ffmpeg, "#!/bin/sh\nexit 1\n").unwrap();
    let mut permissions = fs::metadata(&ffmpeg).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&ffmpeg, permissions).unwrap();
    let failure = run_proxy_transcode_with_plan(
        &ffmpeg,
        &crate::resolve_tool_path("ffprobe"),
        &source,
        &output,
        1.0,
        true,
        &software_plan(),
        |_| {},
    )
    .unwrap_err();
    assert_eq!(failure.outcome.status, "failed");
    assert!(!failure.outcome.fallback_used);
    assert_eq!(failure.outcome.attempts.len(), 1);
    assert!(!output.exists());
}

#[cfg(unix)]
#[test]
fn both_encoder_failures_leave_no_staging_output() {
    if !tools_available() {
        return;
    }
    let directory = TempDir::new().unwrap();
    let source = make_source_with_audio(directory.path());
    let output = directory.path().join("proxy.mp4");
    let real = crate::resolve_tool_path("ffmpeg");
    let ffmpeg = directory.path().join("fake-ffmpeg-all-fail");
    fs::write(
        &ffmpeg,
        format!(
            "#!/bin/sh\necho 'hardware and software failed' >&2\nexit 1\n# {}\n",
            real.display()
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&ffmpeg).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&ffmpeg, permissions).unwrap();
    let ffprobe = crate::resolve_tool_path("ffprobe");
    let plan = ProxyEncoderPlan {
        policy: ProxyEncoderPolicy::Auto,
        preferred_encoder: ProxyEncoderKind::Vaapi,
        targets: vec![
            ProxyEncoderTarget::vaapi("/dev/dri/renderD128".to_string()),
            ProxyEncoderTarget::software(),
        ],
        hardware_unavailable: None,
        software_usable: true,
        limitations: vec![],
    };
    let failure = run_proxy_transcode_with_plan(
        &ffmpeg,
        &ffprobe,
        &source,
        &output,
        1.0,
        true,
        &plan,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(failure.outcome.status, "failed");
    assert_eq!(failure.outcome.recipe_key, PROXY_SOFTWARE_RECIPE_KEY);
    assert!(failure.outcome.fallback_used);
    assert_eq!(failure.outcome.attempts.len(), 2);
    assert!(!output.exists());
}
