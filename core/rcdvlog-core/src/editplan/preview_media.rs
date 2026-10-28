//! Preview identities and actual decoded-output evidence. Frame counts/PTS come
//! from decoders; plan seconds never stand in for an encoded movie clock.
use super::{
    EditPlanRevision, PreviewArtifact, PreviewArtifactVerification, PreviewCutMapping,
    PreviewInputIdentity, PreviewMediaValidation, PreviewRequest, PreviewSourceIdentity,
};
use crate::{
    Asset, Catalog, calibration_tool_identity_for_path, full_file_blake3, resolve_tool_path,
    tool_command,
};
use anyhow::{Context, Result, ensure};
use serde::{
    Deserialize, Deserializer,
    de::{SeqAccess, Visitor},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    fs::{self, File},
    io::BufReader,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

pub(super) const RECIPE: &str =
    "preview.immutable-receipt.h264-30000-1001.stereo48.actual-clock.v1";
const MOVIE_TICKS: i64 = 240000;
const MAX_FRAMES: usize = 1_800_000;
const MAX_METADATA_BYTES: u64 = 256 * 1024 * 1024;

pub(super) struct PreparedSources {
    pub identity: PreviewInputIdentity,
    pub paths: HashMap<String, PathBuf>,
    stamps: HashMap<PathBuf, FileStamp>,
}

#[derive(Clone, PartialEq, Eq)]
struct FileStamp {
    length: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    file_id: (u64, u64),
}
impl FileStamp {
    fn read(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            file_id: (metadata.dev(), metadata.ino()),
        })
    }
}
impl PreparedSources {
    // Originals are immutable by contract. Detect ordinary in-place writes and
    // atomic representation/tool replacement before publishing reusable work.
    pub fn require_unchanged(&self) -> Result<()> {
        for (path, stamp) in &self.stamps {
            ensure!(
                &FileStamp::read(path)? == stamp,
                "preview source or tool changed during segment rendering"
            );
        }
        Ok(())
    }
}
fn stable_hash(path: &Path, cancel: &dyn Fn() -> bool) -> Result<(String, FileStamp)> {
    let before = FileStamp::read(path)?;
    let hash = full_file_blake3(path, cancel)?.hex_digest;
    ensure!(
        FileStamp::read(path)? == before,
        "preview input changed while hashing"
    );
    Ok((hash, before))
}
fn tool_identities() -> Vec<crate::CalibrationToolIdentity> {
    vec![
        calibration_tool_identity_for_path(
            "preview_encode",
            "ffmpeg",
            &resolve_tool_path("ffmpeg"),
        ),
        calibration_tool_identity_for_path(
            "preview_decode_probe",
            "ffprobe",
            &resolve_tool_path("ffprobe"),
        ),
    ]
}
fn command_output(
    command: &mut Command,
    cancel: &dyn Fn() -> bool,
) -> Result<std::process::Output> {
    let output = crate::stall::output_with_timeout_and_cancellation(
        command,
        "preview media metadata",
        Duration::from_secs(180),
        &cancel,
    )?;
    if !output.status.success() {
        return Err(crate::media_tool_failure(
            "preview metadata",
            output.status,
            "selected media could not be inspected".into(),
        ));
    }
    ensure!(
        output.stdout.len() <= 1024 * 1024,
        "preview media header exceeds metadata bound"
    );
    Ok(output)
}
fn clocks(path: &Path, cancel: &dyn Fn() -> bool) -> Result<Value> {
    let mut command = tool_command("ffprobe");
    command.args(["-v","error","-show_entries",
        "format=duration,start_time:stream=index,codec_type,codec_name,width,height,pix_fmt,time_base,start_pts,duration_ts,duration,r_frame_rate,avg_frame_rate,sample_rate,channels:stream_side_data=rotation",
        "-of","json"]).arg(path);
    serde_json::from_slice(&command_output(&mut command, cancel)?.stdout)
        .context("parse preview media clocks")
}
pub(super) fn prepare(
    catalog: &Catalog,
    plan: &EditPlanRevision,
    request: &PreviewRequest,
    assets: &[Asset],
    cancel: &dyn Fn() -> bool,
) -> Result<PreparedSources> {
    let mut ids = BTreeSet::new();
    for cut in &plan.operations {
        ids.insert(cut.asset_id.as_str());
        if let Some(super::EditPlanAudio::Source(audio)) = &cut.audio {
            ids.insert(audio.asset_id.as_str());
        }
    }
    let assets_by_id: HashMap<_, _> = assets
        .iter()
        .map(|asset| (asset.id.as_str(), asset))
        .collect();
    let mut sources = Vec::new();
    let mut paths = HashMap::new();
    let mut hashes = HashMap::<PathBuf, (String, FileStamp)>::new();
    let mut stamps = HashMap::new();
    for id in ids {
        ensure!(!cancel(), "preview cancelled while identifying media");
        let asset = assets_by_id
            .get(id)
            .context("preview source asset not found")?;
        // This is the renderer's established proxy-first/original-fallback
        // policy, including legacy representation identity rather than silently
        // relabelling an old proxy as a current source-frame correspondence.
        let proxy = asset
            .proxy_path
            .as_deref()
            .filter(|p| Path::new(p).is_file());
        let path = PathBuf::from(proxy.unwrap_or(&asset.path));
        ensure!(
            path.is_file(),
            "media offline: preview selected source is unavailable"
        );
        let canonical = fs::canonicalize(&path)?;
        let (fingerprint, stamp) = if let Some((hash, stamp)) = hashes.get(&canonical) {
            ensure!(
                FileStamp::read(&path)? == *stamp,
                "preview selected input changed during preparation"
            );
            (hash.clone(), stamp.clone())
        } else {
            let result = stable_hash(&path, cancel)?;
            hashes.insert(canonical, result.clone());
            result
        };
        stamps.insert(path.clone(), stamp);
        let representation = if proxy.is_some() {
            format!("generatedProxy:{:?}", asset.proxy_representation)
        } else {
            "original".into()
        };
        let media_clocks = clocks(&path, cancel)?;
        let mapping = json!({"selectedMedia":media_clocks,"sourceDurationSeconds":asset.duration_seconds,
            "sourceFrameRate":asset.frame_rate,"indexedSourceDimensions":[asset.width,asset.height],
            "mappingPrecision":if proxy.is_some() {"secondsCorrespondence"} else {"sourceFileClock"}});
        sources.push(PreviewSourceIdentity {
            asset_id: id.into(),
            source_fingerprint: asset.fingerprint.clone(),
            representation,
            representation_fingerprint: fingerprint,
            rotation_degrees: asset.rotation_degrees,
            clocks: mapping,
        });
        paths.insert(id.into(), path);
    }
    let mut plan_identity = json!({"revisionId":plan.revision_id,"projectId":plan.project_id,
        "parentRevisionId":plan.parent_revision_id,"scriptRevisionId":plan.script_revision_id,"operations":plan.operations});
    if let Some(selection) =
        super::selection::metadata_fingerprint(&catalog.connection()?, &plan.revision_id)?
    {
        plan_identity["selection"] = json!(selection);
    }
    let plan_digest = blake3::hash(&serde_json::to_vec(&plan_identity)?)
        .to_hex()
        .to_string();
    let tools = tool_identities();
    let mut tool_content_fingerprints = Vec::new();
    for name in ["ffmpeg", "ffprobe"] {
        let path = resolve_tool_path(name);
        let (hash, stamp) = stable_hash(&path, cancel)?;
        tool_content_fingerprints.push(hash);
        stamps.insert(path, stamp);
    }
    Ok(PreparedSources {
        identity: PreviewInputIdentity {
            recipe: RECIPE.into(),
            revision_id: plan.revision_id.clone(),
            plan_digest,
            max_height: request.max_height,
            constraints_key: super::ranges::constraints_key(&catalog.connection()?, plan)?,
            sources,
            tools,
            tool_content_fingerprints,
        },
        paths,
        stamps,
    })
}

pub(super) fn verify_artifact(
    catalog: &Catalog,
    artifact: &PreviewArtifact,
) -> Result<PreviewArtifactVerification> {
    let mut limitations = Vec::new();
    let output_matches =
        if let (Some(result), Some(validation)) = (&artifact.result, &artifact.validation) {
            let path = Path::new(&result.output_path);
            if super::preview_artifacts::output_is_owned_file(catalog, artifact) {
                full_file_blake3(path, &|| false).is_ok_and(|h| {
                    h.hex_digest == validation.output_fingerprint
                        && h.byte_count == validation.byte_length
                        && validation.fully_decoded
                })
            } else {
                false
            }
        } else {
            false
        };
    if !output_matches {
        limitations.push(
            "The completed movie is missing, changed, or not a regular owned output file.".into(),
        );
    }
    let plan = catalog.edit_plan_revision(&artifact.revision_id)?;
    let constraint_key = super::ranges::constraints_key(&catalog.connection()?, &plan)?;
    let mut constraints_match = artifact
        .identity
        .as_ref()
        .is_some_and(|identity| identity.constraints_key == constraint_key);
    let current = prepare(
        catalog,
        &plan,
        &artifact.request,
        &catalog.list_assets(&artifact.project_id)?,
        &|| false,
    );
    let inputs_match = match current {
        Ok(current) => artifact
            .identity
            .as_ref()
            .is_some_and(|stored| stored == &current.identity),
        Err(_) => {
            limitations.push("Current selected source media could not be verified; prior completion is not current-input proof.".into());
            false
        }
    };
    let plan_renderable = catalog
        .validate_edit_plan(&artifact.revision_id)
        .is_ok_and(|r| r.renderable());
    constraints_match &=
        super::ranges::constraints_key(&catalog.connection()?, &plan)? == constraint_key;
    if !constraints_match {
        limitations.push("Human source constraints changed after this render.".into());
    }
    if !plan_renderable {
        limitations.push("The addressed plan has blocking current validation findings.".into());
    }
    limitations.push("Verification establishes bytes/technical coverage at this time, not complete words, editorial continuity or pacing.".into());
    Ok(PreviewArtifactVerification {
        verified_at: crate::now(),
        output_matches,
        inputs_match,
        constraints_match,
        plan_renderable,
        limitations,
    })
}

#[derive(Deserialize)]
struct Frame {
    media_type: String,
    best_effort_timestamp: Option<i64>,
    duration: Option<i64>,
    pkt_duration: Option<i64>,
    nb_samples: Option<u64>,
}
#[derive(Default)]
struct Frames {
    video_pts: Vec<i64>,
    last_video_duration: Option<i64>,
    audio_frames: u64,
    audio_samples: u64,
}
impl<'de> Deserialize<'de> for Frames {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct FrameVisitor;
        impl<'de> Visitor<'de> for FrameVisitor {
            type Value = Frames;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("bounded decoded frame metadata")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> std::result::Result<Frames, A::Error> {
                let mut result = Frames::default();
                let mut count = 0;
                while let Some(frame) = sequence.next_element::<Frame>()? {
                    count += 1;
                    if count > MAX_FRAMES {
                        return Err(serde::de::Error::custom(
                            "decoded preview frame budget exceeded",
                        ));
                    }
                    if frame.media_type == "video" {
                        let pts = frame.best_effort_timestamp.ok_or_else(|| {
                            serde::de::Error::custom("decoded video frame has no timestamp")
                        })?;
                        result.video_pts.push(pts);
                        result.last_video_duration = frame.duration.or(frame.pkt_duration);
                    } else if frame.media_type == "audio" {
                        result.audio_frames += 1;
                        result.audio_samples += frame.nb_samples.unwrap_or(0);
                    }
                }
                Ok(result)
            }
        }
        deserializer.deserialize_seq(FrameVisitor)
    }
}
#[derive(Deserialize)]
struct Decoded {
    frames: Frames,
    streams: Vec<Value>,
    format: Value,
}
fn decoded(path: &Path, workspace: &Path, cancel: &dyn Fn() -> bool) -> Result<Decoded> {
    let json_path = workspace.join(format!("decoded-{}.json", uuid::Uuid::new_v4().simple()));
    let mut command = tool_command("ffprobe");
    command.args(["-v","error","-show_frames","-show_entries",
        "frame=media_type,best_effort_timestamp,duration,pkt_duration,nb_samples:frame_side_data=:stream=codec_type,codec_name,width,height,pix_fmt,time_base,start_pts,duration_ts,duration,r_frame_rate,sample_rate,channels:stream_side_data=rotation:format=duration,start_time",
        "-of","json=compact=1","-o"]).arg(&json_path).arg(path);
    let (status, diagnostic) = crate::run_captured_media_command_with_cancellation(
        &mut command,
        path,
        &json_path,
        &mut || cancel(),
    )?;
    if !status.success() || !diagnostic.is_empty() {
        return Err(crate::media_tool_failure(
            "decode preview evidence",
            status,
            diagnostic,
        ));
    }
    ensure!(
        fs::metadata(&json_path)?.len() <= MAX_METADATA_BYTES,
        "decoded preview metadata exceeds bounded resource policy"
    );
    let result = serde_json::from_reader(BufReader::new(File::open(&json_path)?))
        .context("parse decoded preview frame evidence");
    fs::remove_file(json_path)?;
    result
}
fn rational(value: &Value) -> Result<(i64, i64)> {
    let (n, d) = value
        .as_str()
        .and_then(|s| s.split_once('/'))
        .context("preview output has no rational time base")?;
    let n: i64 = n.parse()?;
    let d: i64 = d.parse()?;
    ensure!(
        n > 0 && d > 0 && n <= 1_000_000_000 && d <= 1_000_000_000,
        "invalid preview time base"
    );
    Ok((n, d))
}
fn ticks(pts: i64, n: i64, d: i64) -> Result<i64> {
    let scaled = i128::from(pts) * i128::from(n) * i128::from(MOVIE_TICKS);
    ensure!(
        scaled % i128::from(d) == 0,
        "preview output timestamps do not map exactly to the shared movie clock"
    );
    i64::try_from(scaled / i128::from(d)).context("preview clock overflow")
}
fn positive(value: &Value) -> Result<f64> {
    let value = value
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .context("preview duration missing")?;
    ensure!(value.is_finite() && value > 0.0, "invalid preview duration");
    Ok(value)
}
fn stream<'a>(decoded: &'a Decoded, kind: &str) -> Result<&'a Value> {
    decoded
        .streams
        .iter()
        .find(|s| s["codec_type"] == kind)
        .context("preview output is missing a required stream")
}
fn video_shape(decoded: &Decoded, width: u32, height: u32) -> Result<(i64, i64)> {
    let video = stream(decoded, "video")?;
    ensure!(
        video["codec_name"] == "h264"
            && video["pix_fmt"] == "yuv420p"
            && video["width"] == width
            && video["height"] == height,
        "preview output does not match the required H.264 canvas recipe"
    );
    ensure!(
        crate::stream_rotation(Some(video)) == 0,
        "preview output retains a display rotation"
    );
    ensure!(
        !decoded.frames.video_pts.is_empty()
            && decoded.frames.video_pts.windows(2).all(|p| p[1] > p[0]),
        "preview decoded frame coverage is empty or unordered"
    );
    rational(&video["time_base"])
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SegmentExpectation {
    pub width: u32,
    pub height: u32,
    pub seconds: f64,
    pub frames: Option<u64>,
    pub audio: bool,
}

pub(super) fn segment_frame_count(
    path: &Path,
    workspace: &Path,
    expected: &SegmentExpectation,
    cancel: &dyn Fn() -> bool,
) -> Result<u64> {
    let decoded = decoded(path, workspace, cancel)?;
    let (n, d) = video_shape(&decoded, expected.width, expected.height)?;
    let count = decoded.frames.video_pts.len() as u64;
    if let Some(expected) = expected.frames {
        ensure!(
            count == expected,
            "preview segment lost requested picture frames"
        );
    }
    let first = decoded.frames.video_pts[0];
    let last = *decoded.frames.video_pts.last().unwrap();
    let last_duration = decoded
        .frames
        .last_video_duration
        .filter(|v| *v > 0)
        .unwrap_or((d * 1001) / (n * 30000));
    let duration = (last - first + last_duration) as f64 * n as f64 / d as f64;
    ensure!(
        (duration - expected.seconds).abs() <= 0.067,
        "preview segment duration does not cover the requested source interval"
    );
    if expected.audio {
        let audio = stream(&decoded, "audio")?;
        ensure!(
            audio["codec_name"] == "aac"
                && audio["sample_rate"] == "48000"
                && audio["channels"] == 2
                && decoded.frames.audio_frames > 0
                && decoded.frames.audio_samples > 0,
            "preview segment audio is missing or does not match the linked/silent policy"
        );
    } else {
        ensure!(
            !decoded.streams.iter().any(|s| s["codec_type"] == "audio"),
            "picture-only preview segment unexpectedly contains sound"
        );
    }
    Ok(count)
}
pub(super) fn validate_movie(
    path: &Path,
    workspace: &Path,
    plan: &EditPlanRevision,
    request: &PreviewRequest,
    segment_frames: &[u64],
    cancel: &dyn Fn() -> bool,
) -> Result<PreviewMediaValidation> {
    let decoded = decoded(path, workspace, cancel)?;
    let height = request.max_height / 2 * 2;
    let width = (height * 16 / 9) / 2 * 2;
    let (n, d) = video_shape(&decoded, width, height)?;
    ensure!(
        segment_frames.len() == plan.operations.len()
            && segment_frames.iter().sum::<u64>() == decoded.frames.video_pts.len() as u64,
        "preview is incomplete: decoded movie frame count differs from all validated segments"
    );
    let audio = stream(&decoded, "audio")?;
    ensure!(
        audio["codec_name"] == "aac"
            && audio["sample_rate"] == "48000"
            && audio["channels"] == 2
            && decoded.frames.audio_frames > 0
            && decoded.frames.audio_samples > 0,
        "preview audio is absent or incompatible with the 48 kHz stereo policy"
    );
    let duration = positive(&decoded.format["duration"])?;
    // Picture intervals end at the last decoded picture frame boundary. AAC
    // priming/padding can make container duration longer; do not assign that
    // sound-only tail to a fictitious extra picture frame.
    let movie_origin = ticks(decoded.frames.video_pts[0], n, d)?;
    let last_duration = decoded
        .frames
        .last_video_duration
        .filter(|v| *v > 0)
        .unwrap_or((d * 1001) / (n * 30000));
    let movie_end = ticks(
        decoded
            .frames
            .video_pts
            .last()
            .unwrap()
            .checked_add(last_duration)
            .context("preview picture end overflow")?,
        n,
        d,
    )?;
    let mut cuts = Vec::new();
    let mut at = 0;
    for (operation, &count) in plan.operations.iter().zip(segment_frames) {
        let end = at + count as usize;
        let start = ticks(decoded.frames.video_pts[at], n, d)?;
        let out = if end < decoded.frames.video_pts.len() {
            ticks(decoded.frames.video_pts[end], n, d)?
        } else {
            movie_end
        };
        ensure!(out > start, "decoded cut has no movie duration");
        cuts.push(PreviewCutMapping {
            operation_id: operation.operation_id.clone(),
            asset_id: operation.asset_id.clone(),
            source_in_seconds: operation.source_in_seconds,
            source_out_seconds: operation.source_out_seconds,
            movie_in_pts: start,
            movie_out_pts: out,
            output_frame_count: count,
        });
        at = end;
    }
    let hash = full_file_blake3(path, cancel)?;
    Ok(PreviewMediaValidation {output_fingerprint:hash.hex_digest,byte_length:hash.byte_count,duration_seconds:duration,video_frame_count:decoded.frames.video_pts.len() as u64,
        video_time_base_numerator:n,video_time_base_denominator:d,video_origin_pts:decoded.frames.video_pts[0],movie_time_base_numerator:1,movie_time_base_denominator:MOVIE_TICKS,movie_origin_pts:movie_origin,
        width,height,audio_sample_rate:48000,audio_channels:2,fully_decoded:true,cuts,
        limitations:vec!["Movie cut boundaries use decoded output PTS. Source intervals remain the requested source-clock ranges, not a measured per-frame original correspondence.".into(),
            "Full technical decode/coverage does not establish complete words, source-audio content parity, continuity or editorial pacing.".into()]})
}
