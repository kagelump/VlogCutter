//! Scratch directories, generated or representative sample media, and PCM decoding for calibration runs.

use super::*;

pub(super) struct ScratchDirectory(pub(super) PathBuf);
impl Drop for ScratchDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) fn create_scratch_root(catalog: &Catalog) -> Result<ScratchDirectory> {
    let parent = catalog
        .database_path()
        .parent()
        .ok_or_else(|| anyhow!("catalog has no parent directory"))?;
    let root = parent
        .join("cache")
        .join("calibration-staging")
        .join(uuid::Uuid::new_v4().simple().to_string());
    fs::create_dir_all(&root)
        .with_context(|| format!("create calibration scratch root {}", root.display()))?;
    Ok(ScratchDirectory(root))
}

pub(super) fn run_ffmpeg_fixture(
    root: &Path,
    seconds: u32,
    with_audio: bool,
    audio_only: bool,
) -> Result<PathBuf> {
    let ffmpeg = resolve_tool_path("ffmpeg");
    let output = root.join(if audio_only {
        "calibration.wav"
    } else {
        "calibration.mp4"
    });
    let mut args = vec![
        OsString::from("-hide_banner"),
        OsString::from("-loglevel"),
        OsString::from("error"),
        OsString::from("-y"),
        OsString::from("-f"),
        OsString::from("lavfi"),
        OsString::from("-i"),
        OsString::from(if audio_only {
            "sine=frequency=440:sample_rate=16000".to_string()
        } else {
            "testsrc2=size=320x180:rate=30".to_string()
        }),
    ];
    if with_audio && !audio_only {
        args.extend([
            OsString::from("-f"),
            OsString::from("lavfi"),
            OsString::from("-i"),
            OsString::from("sine=frequency=440:sample_rate=48000"),
        ]);
    }
    args.extend([
        OsString::from("-t"),
        OsString::from(seconds.to_string()),
        OsString::from("-c:v"),
        OsString::from("mpeg4"),
        OsString::from("-q:v"),
        OsString::from("5"),
    ]);
    if with_audio && !audio_only {
        args.extend([
            OsString::from("-c:a"),
            OsString::from("aac"),
            OsString::from("-shortest"),
        ]);
    } else if audio_only {
        args.extend([OsString::from("-c:a"), OsString::from("pcm_s16le")]);
    }
    args.push(output.as_os_str().to_owned());
    let result = crate::prerequisites::run_bounded_command(
        &ffmpeg,
        &args,
        Duration::from_secs(30),
        64 * 1024,
    )
    .map_err(|error| anyhow!("generate calibration media failed: {error}"))?;
    ensure!(
        result.status.success(),
        "generate calibration media failed: {}",
        bounded_text(&String::from_utf8_lossy(&result.stderr), 1024)
    );
    Ok(output)
}

pub(super) struct SourceFacts {
    pub(super) path: PathBuf,
    pub(super) duration_ms: u32,
    pub(super) duration_seconds: f64,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) has_audio: bool,
}

pub(super) fn probe_source_facts(path: &Path) -> Result<SourceFacts> {
    let document = crate::probe_media_document(path)?;
    let stream = crate::require_video_stream(&document)?;
    let duration_seconds = crate::video_duration(&document, stream)
        .ok_or_else(|| anyhow!("calibration source has no finite duration"))?;
    ensure!(
        duration_seconds > 0.0 && duration_seconds * 1000.0 < u32::MAX as f64,
        "calibration source duration is outside the supported range"
    );
    let ffprobe = resolve_tool_path("ffprobe");
    let has_audio = probe_source_audio_present(&ffprobe, path)?;
    Ok(SourceFacts {
        path: path.to_path_buf(),
        duration_ms: (duration_seconds * 1000.0).round() as u32,
        duration_seconds,
        width: stream
            .get("width")
            .and_then(|value| value.as_u64())
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
        height: stream
            .get("height")
            .and_then(|value| value.as_u64())
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
        has_audio,
    })
}

pub(super) fn generated_sample_identity(seconds: u32, kind: &str) -> String {
    let descriptor = serde_json::json!({
        "kind": "generated",
        "surface": kind,
        "seconds": seconds,
        "video": "testsrc2-320x180-30fps-mpeg4",
        "audio": "sine-440hz-48k-aac",
        "recipe": RESOURCE_CALIBRATION_RECIPE_VERSION,
    });
    let digest = blake3::hash(&serde_json::to_vec(&descriptor).unwrap_or_default())
        .to_hex()
        .to_string();
    format!("generated:{digest}")
}

pub(super) fn representative_sample_identity(path: &Path) -> Result<(String, u64)> {
    let hash = crate::full_file_blake3(path, &|| false)
        .with_context(|| "fingerprint representative calibration media")?;
    Ok((format!("blake3:{}", hash.hex_digest), hash.byte_count))
}

pub(super) fn decode_calibration_pcm(source_path: &Path) -> Result<Vec<f32>> {
    let pcm_scratch = std::env::temp_dir().join(format!(
        "rcdvlog-calibration-pcm-{}.wav",
        uuid::Uuid::new_v4().simple()
    ));
    let pcm = read_pcm(source_path).or_else(|_| {
        let wav_path = pcm_scratch.clone();
        let ffmpeg = resolve_tool_path("ffmpeg");
        let args = [
            OsString::from("-hide_banner"),
            OsString::from("-loglevel"),
            OsString::from("error"),
            OsString::from("-y"),
            OsString::from("-i"),
            source_path.as_os_str().to_owned(),
            OsString::from("-ar"),
            OsString::from("16000"),
            OsString::from("-ac"),
            OsString::from("1"),
            OsString::from("-c:a"),
            OsString::from("pcm_s16le"),
            wav_path.as_os_str().to_owned(),
        ];
        let result = crate::prerequisites::run_bounded_command(
            &ffmpeg,
            &args,
            Duration::from_secs(30),
            64 * 1024,
        )
        .map_err(|error| anyhow!("extract calibration PCM: {error}"))?;
        ensure!(
            result.status.success(),
            "extract calibration PCM with ffmpeg failed"
        );
        read_pcm(&wav_path)
    });
    let _ = fs::remove_file(&pcm_scratch);
    pcm
}
