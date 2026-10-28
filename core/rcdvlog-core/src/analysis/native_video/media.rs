//! Source-scoped local media preparation, shared with exact source inspection.
//! Preparing a file is not a cloud transmission or an authorization permit.
use super::reply::PayloadWitness;
use super::*;
use crate::editplan::{inspection::SelectedSource, inspection_media};
use crate::{Asset, Catalog, InspectionTimeMapping};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

pub(super) const MAX_MEDIA_BYTES: u64 = 14_000_000;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeVideoMediaView {
    pub identity: String,
    pub asset_id: String,
    pub recipe: String,
    pub output_relative_path: String,
    pub output_fingerprint: String,
    pub byte_length: u64,
    pub width: u32,
    pub height: u32,
    pub video_frame_count: u32,
    pub encoded_fps: f64,
    pub provider_fps: f64,
    pub encoded_duration_seconds: f64,
    pub audio_included: bool,
    pub representation_audio_in_samples: Option<u64>,
    pub representation_audio_out_samples: Option<u64>,
    pub decoded_audio_samples: u64,
    pub clock: NativeVideoClock,
    pub tools_fingerprint: String,
    pub reused: bool,
    pub limitations: Vec<String>,
}
#[derive(Clone)]
pub(super) struct TranscriptPart {
    pub key: String,
    pub local_in_seconds: f64,
    pub local_out_seconds: f64,
    pub text: String,
    pub source_span: crate::analysis::moments::MomentSpan,
}
pub struct NativeVideoMedia {
    pub(super) view: NativeVideoMediaView,
    pub(super) request: NativeVideoWindowRequest,
    pub(super) asset: Asset,
    pub(super) selected: SelectedSource,
    pub(super) bytes: Arc<Vec<u8>>,
    pub(super) transcript: Vec<TranscriptPart>,
    pub(super) witness: PayloadWitness,
}
impl NativeVideoMedia {
    pub fn view(&self) -> &NativeVideoMediaView {
        &self.view
    }
    pub fn byte_length(&self) -> u64 {
        self.view.byte_length
    }
    pub(crate) fn request(&self) -> &NativeVideoWindowRequest {
        &self.request
    }
    pub(crate) fn require_current(
        &self,
        catalog: &Catalog,
        options: &NativeVideoOptions,
        cancel: &dyn Fn() -> bool,
    ) -> Result<()> {
        catalog.require_inspection_source_current(
            &self.asset,
            &self.selected,
            options.representation,
            cancel,
        )?;
        ensure!(
            tool_fingerprint(cancel)? == self.view.tools_fingerprint,
            "native video media tools changed after preparation"
        );
        ensure!(
            blake3::hash(self.bytes.as_ref()).to_hex().as_str() == self.view.output_fingerprint,
            "prepared native video bytes changed"
        );
        Ok(())
    }
    /// Pure proof against the canonical policy loaded inside admission's
    /// IMMEDIATE transaction. Filesystem freshness is checked again by the leaf
    /// immediately before the durable dispatch marker.
    pub(crate) fn validate_policy_binding(
        &self,
        policy: &crate::RunPolicy,
        source_identity: &str,
        intended_key: &str,
    ) -> Result<()> {
        ensure!(
            policy.kind == crate::AnalysisRunKind::NativeVideoReview
                && policy.readiness.ready_for_authorization,
            "native media requires a ready native video policy"
        );
        let crate::RunPolicyParameters::NativeVideoReview { options } = &policy.parameters else {
            anyhow::bail!("native policy parameters missing");
        };
        validate_options(options)?;
        let scope = options
            .source_scopes
            .get(self.request.scope_index as usize)
            .context("native source scope missing")?;
        let asset_scope = policy
            .scope
            .iter()
            .find(|s| s.asset_id == self.asset.id)
            .context("native asset outside authorized policy")?;
        ensure!(
            self.request.policy_id == policy.policy_id
                && scope.asset_id == self.asset.id
                && asset_scope.source_identity == source_identity
                && asset_scope.source_fingerprint == self.asset.fingerprint
                && asset_scope.representation_fingerprint.as_deref()
                    == Some(self.selected.fingerprint.as_str()),
            "native media/source/policy identity mismatch"
        );
        ensure!(
            intended_key
                == format!(
                    "native_video_review:{}:{}",
                    self.asset.id, self.view.identity
                ),
            "native intended request identity mismatch"
        );
        ensure!(
            self.request.source_in_seconds >= scope.source_in_seconds
                && self.request.source_out_seconds <= scope.source_out_seconds
                && self.request.source_in_seconds < self.request.source_out_seconds
                && self.request.source_out_seconds - self.request.source_in_seconds
                    <= f64::from(options.max_window_seconds)
                && (0.25..=options.fps).contains(&self.request.fps)
                && (160..=options.max_width).contains(&self.request.max_width)
                && self.view.audio_included == policy.payload.uses_audio()
                && (policy.payload.uses_transcript() == !self.transcript.is_empty())
                && self.view.clock.source_offset_seconds >= self.request.source_in_seconds - 1e-9
                && self.view.clock.source_offset_seconds
                    + self.view.clock.local_valid_end_seconds * self.view.clock.seconds_scale
                    <= self.request.source_out_seconds + 1e-9,
            "native media exceeds authorized source/sampling/payload scope"
        );
        ensure!(
            policy.provider == NATIVE_VIDEO_PROVIDER
                && policy.primary_model == NATIVE_VIDEO_MODEL
                && policy.allowed_models == vec![NATIVE_VIDEO_MODEL.to_string()]
                && !policy.allow_fallback
                && policy.permitted_fallbacks.is_empty()
                && options.navigation == NativeVideoNavigation::Static
                && policy.provider_cost_bound_contract == cost_bound(options)?.contract_version,
            "native endpoint/model/tier/quote contract mismatch"
        );
        Ok(())
    }
}
pub(super) fn owned_directory(root: &Path, parts: &[&str]) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    for part in parts {
        path.push(part);
        if !path.exists() {
            fs::create_dir(&path)?;
        }
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "native media cache requires managed directories without symlinks"
        );
    }
    Ok(path)
}
fn execute(command: &mut std::process::Command, cancel: &dyn Fn() -> bool) -> Result<Vec<u8>> {
    let result = crate::stall::output_with_timeout_and_cancellation(
        command,
        "native source media",
        Duration::from_secs(180),
        &|| cancel(),
    )?;
    ensure!(
        result.status.success() && result.stderr.is_empty(),
        "native media decode/encode failed; inspect local tools and narrow the explicit source window"
    );
    Ok(result.stdout)
}

impl Catalog {
    pub fn prepare_native_video_window(
        &self,
        policy: &crate::RunPolicy,
        request: &NativeVideoWindowRequest,
        cancel: &dyn Fn() -> bool,
    ) -> Result<NativeVideoMedia> {
        self.prepare_native_video_window_mode(policy, request, false, cancel)
    }
    pub(crate) fn prepare_native_video_window_mode(
        &self,
        policy: &crate::RunPolicy,
        request: &NativeVideoWindowRequest,
        explicit_review: bool,
        cancel: &dyn Fn() -> bool,
    ) -> Result<NativeVideoMedia> {
        let crate::RunPolicyParameters::NativeVideoReview { options } = &policy.parameters else {
            anyhow::bail!("native media requires a native video policy");
        };
        validate_options(options)?;
        ensure!(
            crate::analysis::run_policy::run_policy_identity(policy)? == policy.policy_id
                && policy.readiness.ready_for_authorization,
            "native policy or source changed before local preparation"
        );
        ensure!(
            request.policy_id == policy.policy_id,
            "native window policy identity mismatch"
        );
        let scope = options
            .source_scopes
            .get(request.scope_index as usize)
            .context("invalid input: native window scope index outside policy")?;
        ensure!(
            [
                request.source_in_seconds,
                request.source_out_seconds,
                request.fps
            ]
            .iter()
            .all(|v| v.is_finite())
                && request.source_in_seconds >= scope.source_in_seconds
                && request.source_out_seconds <= scope.source_out_seconds
                && request.source_out_seconds > request.source_in_seconds
                && request.source_out_seconds - request.source_in_seconds
                    <= f64::from(options.max_window_seconds)
                && (0.25..=options.fps).contains(&request.fps)
                && (160..=options.max_width).contains(&request.max_width),
            "native window exceeds the explicitly approved source/sampling scope"
        );
        ensure!(!cancel(), "native media preparation cancelled");
        let _heavy = self.acquire_local_heavy_cancellable(cancel)?;
        let asset = self.get_asset(&scope.asset_id)?;
        crate::analysis::run_policy::validate_native_video_asset_mode(
            self,
            policy,
            &asset.id,
            explicit_review,
            cancel,
        )?;
        let selected = self.inspection_source(&asset, options.representation, cancel)?;
        let stream = inspection_media::probe(&selected.path, policy.payload.uses_audio(), cancel)?;
        let mapping = InspectionTimeMapping {
            time_base_numerator: stream.numerator,
            time_base_denominator: stream.denominator,
            representation_origin_pts: stream.origin,
            source_offset_seconds: selected.source_offset,
            representation_offset_seconds: selected.representation_offset,
            seconds_scale: selected.scale,
            precision: selected.precision.into(),
            source_frame_reduction: selected.reduction,
        };
        let start = selected.representation_offset
            + (request.source_in_seconds - selected.source_offset) / f64::from(selected.scale);
        let end = selected.representation_offset
            + (request.source_out_seconds - selected.source_offset) / f64::from(selected.scale);
        ensure!(
            start >= 0.0 && end <= stream.duration + 1e-7,
            "native source scope exceeds selected representation"
        );
        let available = inspection_media::frame_pts(&selected.path, &mapping, start, end, cancel)?;
        let first = available[0];
        let actual_source_start = mapping.source_seconds(first);
        let actual_representation_start =
            (first - stream.origin) as f64 * stream.numerator as f64 / stream.denominator as f64;
        let duration = end - actual_representation_start;
        ensure!(
            duration > 0.0 && actual_source_start >= request.source_in_seconds - 1e-9,
            "native window has no valid decoded start frame"
        );
        let clock = NativeVideoClock {
            source_offset_seconds: actual_source_start,
            seconds_scale: f64::from(selected.scale),
            local_valid_end_seconds: duration,
            first_representation_pts: first,
            representation_time_base_numerator: stream.numerator,
            representation_time_base_denominator: stream.denominator,
            precision: "sampledSecondsCorrespondence".into(),
        };

        let tools = tool_fingerprint(cancel)?;
        let identity_json = serde_json::to_string(
            &json!({"recipe":NATIVE_VIDEO_MEDIA_RECIPE,"projectId":policy.project_id,"assetId":asset.id,"sourceFingerprint":asset.fingerprint,
            "representation":options.representation,"representationFingerprint":selected.fingerprint,"sourceIn":actual_source_start,"sourceOut":request.source_out_seconds,
            "firstPts":first,"mapping":mapping,"rotation":asset.rotation_degrees,"orientationDecision":asset.orientation_decision,"width":request.max_width,"audio":policy.payload.uses_audio(),"encodedFps":24,"tools":tools}),
        )?;
        let identity = blake3::hash(identity_json.as_bytes()).to_hex().to_string();
        let parent = self
            .database_path()
            .parent()
            .context("catalog has no local media root")?;
        let root = owned_directory(parent, &["cache", "native-video", "media", &identity])?;
        let lock_path = root.join(".lock");
        if let Ok(m) = fs::symlink_metadata(&lock_path) {
            ensure!(
                !m.file_type().is_symlink(),
                "native media claim cannot be a symlink"
            );
        }
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        fs2::FileExt::try_lock_exclusive(&lock)
            .context("native media is already being prepared")?;
        let saved:Option<(String,String)>=self.connection()?.query_row("SELECT identity_json,report_json FROM native_video_media WHERE identity=?1 AND project_id=?2",params![identity,policy.project_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let mut cached = None;
        if let Some((prior, body)) = saved
            && prior == identity_json
            && body.len() <= 1024 * 1024
            && let Ok(mut view) = serde_json::from_str::<NativeVideoMediaView>(&body)
            && view.identity == identity
            && view.asset_id == asset.id
            && view.recipe == NATIVE_VIDEO_MEDIA_RECIPE
            && view.tools_fingerprint == tools
            && view.clock == clock
            && view.audio_included == policy.payload.uses_audio()
            && view.width > 0
            && view.height > 0
            && view.width <= request.max_width
            && view.height <= request.max_width
            && view.representation_audio_in_samples
                == policy
                    .payload
                    .uses_audio()
                    .then_some((actual_representation_start * 48000.0).ceil() as u64)
            && view.representation_audio_out_samples
                == policy
                    .payload
                    .uses_audio()
                    .then_some((end * 48000.0).floor() as u64)
            && let Ok(path) = owned_media_path(parent, &view.output_relative_path)
            && path.parent() == Some(root.as_path())
            && let Ok(hash) = crate::full_file_blake3(&path, cancel)
            && hash.hex_digest == view.output_fingerprint
            && hash.byte_count == view.byte_length
            && hash.byte_count <= MAX_MEDIA_BYTES
            && let Ok(qa) = validate_output(&path, policy.payload.uses_audio(), duration, cancel)
            && view.width == qa.width
            && view.height == qa.height
            && view.video_frame_count == qa.frames
            && view.encoded_fps == 24.0
            && view.encoded_duration_seconds == qa.duration
            && view.decoded_audio_samples == qa.audio_samples
        {
            view.reused = true;
            cached = Some(view);
        }
        let view = if let Some(view) = cached {
            view
        } else {
            let temp = tempfile::tempdir_in(&root)?;
            let audio = if policy.payload.uses_audio() {
                Some(inspection_media::audio_context(
                    &selected.path,
                    temp.path(),
                    stream.audio_start_seconds,
                    actual_representation_start,
                    end,
                    cancel,
                )?)
            } else {
                None
            };
            if policy.payload.uses_audio() {
                ensure!(
                    audio.as_ref().is_some_and(|a| a.playable.is_some()),
                    "explicit native audio is unavailable in the selected representation"
                );
            }
            let output = root.join(format!("window_{}.mp4", uuid::Uuid::new_v4().simple()));
            let turn = match asset.rotation_degrees {
                0 => "",
                90 => "transpose=clock,",
                180 => "hflip,vflip,",
                270 => "transpose=cclock,",
                _ => anyhow::bail!("unsupported native effective rotation"),
            };
            let end_absolute =
                end + stream.origin as f64 * stream.numerator as f64 / stream.denominator as f64;
            let filter = format!(
                "trim=start_pts={first}:end={end_absolute},setpts=PTS-{first},{turn}scale='min({},iw)':'min({},ih)':force_original_aspect_ratio=decrease:force_divisible_by=2,fps=24:start_time=0:round=near,tpad=stop_mode=clone:stop_duration=0.041666667",
                request.max_width, request.max_width
            );
            let mut command = crate::tool_command("ffmpeg");
            command
                .args(["-n", "-v", "error", "-copyts", "-ss"])
                .arg(
                    (actual_representation_start - 1.0)
                        .max(0.0)
                        .floor()
                        .to_string(),
                )
                .args(["-noautorotate", "-i"])
                .arg(&selected.path);
            if let Some(file) = audio.as_ref().and_then(|a| a.playable.as_ref()) {
                command.arg("-i").arg(&file.path);
            }
            command.args(["-map", "0:v:0", "-vf"]).arg(filter).args([
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-crf",
                "30",
                "-maxrate",
                "1200k",
                "-bufsize",
                "2400k",
                "-pix_fmt",
                "yuv420p",
                "-threads",
                "1",
                "-video_track_timescale",
                "240000",
            ]);
            if policy.payload.uses_audio() {
                command.args([
                    "-map", "1:a:0", "-c:a", "aac", "-ar", "48000", "-ac", "2", "-b:a", "128k",
                ]);
            } else {
                command.arg("-an");
            }
            command
                .args(["-t"])
                .arg(duration.to_string())
                .args([
                    "-sn",
                    "-dn",
                    "-map_metadata",
                    "-1",
                    "-metadata:s:v:0",
                    "rotate=0",
                    "-movflags",
                    "+faststart",
                ])
                .arg(&output);
            execute(&mut command, cancel)?;
            let hash = crate::full_file_blake3(&output, cancel)?;
            ensure!(
                hash.byte_count > 0 && hash.byte_count <= MAX_MEDIA_BYTES,
                "native video exceeds the bounded inline-media size"
            );
            let qa = validate_output(&output, policy.payload.uses_audio(), duration, cancel)?;
            ensure!(
                qa.width > 0
                    && qa.height > 0
                    && qa.width <= request.max_width
                    && qa.height <= request.max_width,
                "native encoded resolution exceeds the explicit policy"
            );
            let view=NativeVideoMediaView{identity:identity.clone(),asset_id:asset.id.clone(),recipe:NATIVE_VIDEO_MEDIA_RECIPE.into(),output_relative_path:output.strip_prefix(parent)?.to_str().context("native media path is not UTF-8")?.into(),output_fingerprint:hash.hex_digest,byte_length:hash.byte_count,
                width:qa.width,height:qa.height,video_frame_count:qa.frames,encoded_fps:24.0,provider_fps:request.fps,encoded_duration_seconds:qa.duration,audio_included:policy.payload.uses_audio(),
                representation_audio_in_samples:audio.as_ref().map(|a|a.source_in_samples),representation_audio_out_samples:audio.as_ref().map(|a|a.source_out_samples),decoded_audio_samples:qa.audio_samples,clock,tools_fingerprint:tools,reused:false,
                limitations:vec!["Local MP4 keeps a measured first-source-frame offset and resamples picture to 24 fps; original frame identity between samples is not restored.".into(),"Provider sampling is separately requested and is not proof of internal frame coverage. Source/utterance times remain locators.".into(),"PCM bounds round inward at 48 kHz on the selected representation clock, not original-master sample identity; AAC padding and the final picture hold are derived tails, outside the eligible local timestamp range.".into()]};
            self.require_inspection_source_current(
                &asset,
                &selected,
                options.representation,
                cancel,
            )?;
            ensure!(
                tool_fingerprint(cancel)? == view.tools_fingerprint,
                "native tools changed during media preparation"
            );
            self.connection()?.execute("INSERT INTO native_video_media(identity,project_id,asset_id,identity_json,report_json,created_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(identity) DO UPDATE SET report_json=excluded.report_json",params![identity,policy.project_id,asset.id,identity_json,serde_json::to_string(&view)?,crate::now()])?;
            view
        };
        self.require_inspection_source_current(&asset, &selected, options.representation, cancel)?;
        let mut bytes = Vec::new();
        fs::File::open(owned_media_path(parent, &view.output_relative_path)?)?
            .take(MAX_MEDIA_BYTES + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 == view.byte_length
                && blake3::hash(&bytes).to_hex().as_str() == view.output_fingerprint,
            "native media changed before payload preparation"
        );
        let transcript = transcript_parts(
            self,
            &asset,
            options,
            policy.payload.uses_transcript(),
            &view.clock,
        )?;
        let witness = PayloadWitness {
            media_fingerprint: view.output_fingerprint.clone(),
            audio_included: view.audio_included,
            transcript_segments: transcript
                .iter()
                .map(|t| {
                    (
                        t.key.clone(),
                        super::reply::TranscriptWitness {
                            local_span: (t.local_in_seconds, t.local_out_seconds),
                            source_span: t.source_span,
                        },
                    )
                })
                .collect(),
        };
        let mut view = view;
        view.provider_fps = request.fps;
        Ok(NativeVideoMedia {
            view,
            request: request.clone(),
            asset,
            selected,
            bytes: Arc::new(bytes),
            transcript,
            witness,
        })
    }
}

fn owned_media_path(parent: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    ensure!(
        !path.is_absolute()
            && path
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
        "native media path is outside managed storage"
    );
    let parts = path
        .components()
        .map(|p| {
            p.as_os_str()
                .to_str()
                .context("native media path is not UTF-8")
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        parts.len() == 5
            && parts[0..3] == ["cache", "native-video", "media"]
            && parts[3].len() == 64
            && parts[3].bytes().all(|b| b.is_ascii_hexdigit())
            && parts[4].starts_with("window_")
            && parts[4].ends_with(".mp4"),
        "native media path does not name an owned object"
    );
    let mut resolved = parent.to_path_buf();
    for part in parts {
        resolved.push(part);
        let m = fs::symlink_metadata(&resolved)?;
        ensure!(
            !m.file_type().is_symlink(),
            "native media cannot follow symlinks"
        );
    }
    ensure!(
        fs::metadata(&resolved)?.is_file(),
        "native media object is not a file"
    );
    Ok(resolved)
}
struct OutputQa {
    width: u32,
    height: u32,
    frames: u32,
    duration: f64,
    audio_samples: u64,
}
fn validate_output(
    path: &Path,
    want_audio: bool,
    duration: f64,
    cancel: &dyn Fn() -> bool,
) -> Result<OutputQa> {
    let mut command = crate::tool_command("ffprobe");
    command.args(["-v","error","-show_frames","-show_entries","stream=codec_type,time_base,start_pts,width,height,sample_rate,channels:stream_side_data=rotation:stream_tags=rotate:frame=media_type,best_effort_timestamp,nb_samples:format=duration","-of","json"]).arg(path);
    let bytes = execute(&mut command, cancel)?;
    ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "native decoded metadata exceeds bounded size"
    );
    let doc: Value = serde_json::from_slice(&bytes)?;
    let streams = doc["streams"]
        .as_array()
        .context("native output streams missing")?;
    let video = streams
        .iter()
        .find(|s| s["codec_type"] == "video")
        .context("native output has no video")?;
    ensure!(
        video["side_data_list"].as_array().is_none_or(|list| list
            .iter()
            .all(|side| side["rotation"].as_f64().is_none_or(|r| r.abs() < 1e-9)))
            && video["tags"]["rotate"]
                .as_str()
                .is_none_or(|s| s.parse::<f64>().is_ok_and(|r| r.abs() < 1e-9)),
        "native output retains an unbaked display rotation"
    );
    ensure!(
        streams
            .iter()
            .filter(|s| s["codec_type"] == "video")
            .count()
            == 1,
        "native output must contain one picture stream"
    );
    let audio = streams
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .collect::<Vec<_>>();
    ensure!(
        audio.len() == usize::from(want_audio),
        "native output audio presence differs from explicit payload policy"
    );
    if let Some(audio) = audio.first() {
        ensure!(
            audio["sample_rate"].as_str() == Some("48000") && audio["channels"].as_u64() == Some(2),
            "native output audio clock differs from declared PCM48 stereo recipe"
        );
    }
    let frame_rows = doc["frames"]
        .as_array()
        .context("native output has no decoded frame proof")?;
    let pts = frame_rows
        .iter()
        .filter(|f| f["media_type"] == "video")
        .map(|f| {
            f["best_effort_timestamp"]
                .as_i64()
                .context("native output frame has no decoded PTS")
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        !pts.is_empty() && pts.len() <= 1441 && pts[0] == 0 && pts.windows(2).all(|w| w[0] < w[1]),
        "native output frame clock is empty, unordered or not locally zero-based"
    );
    ensure!(
        video["time_base"].as_str() == Some("1/240000"),
        "native output track time base differs from declared clock"
    );
    let picture_end = pts.len() as f64 / 24.0;
    ensure!(
        picture_end >= duration - 1.0 / 240000.0
            && picture_end <= duration + 1.0 / 24.0 + 1.0 / 240000.0,
        "native picture does not cover the requested bounded interval"
    );
    for (index, &pts) in pts.iter().enumerate() {
        ensure!(
            (pts as f64 - index as f64 * 10000.0).abs() <= 1.0,
            "native output cadence differs from declared 24 fps"
        );
    }
    let encoded_duration = doc["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .context("native output has no encoded duration")?;
    ensure!(
        encoded_duration.is_finite()
            && encoded_duration > 0.0
            && encoded_duration <= duration + 1.0 / 24.0 + 1024.0 / 48000.0 + 0.002,
        "native encoded duration exceeds declared picture/audio tail allowance"
    );
    let audio_samples = frame_rows
        .iter()
        .filter(|f| f["media_type"] == "audio")
        .try_fold(0u64, |total, f| {
            total
                .checked_add(f["nb_samples"].as_u64().unwrap_or(0))
                .context("native audio sample count overflow")
        })?;
    if want_audio {
        ensure!(
            audio_samples > 0 && (audio_samples as f64 - duration * 48000.0).abs() <= 2048.0,
            "native audio does not decode through the declared window"
        );
    }
    Ok(OutputQa {
        width: u32::try_from(
            video["width"]
                .as_u64()
                .context("native output width missing")?,
        )?,
        height: u32::try_from(
            video["height"]
                .as_u64()
                .context("native output height missing")?,
        )?,
        frames: pts.len() as u32,
        duration: encoded_duration,
        audio_samples,
    })
}
fn transcript_parts(
    catalog: &Catalog,
    asset: &Asset,
    options: &NativeVideoOptions,
    included: bool,
    clock: &NativeVideoClock,
) -> Result<Vec<TranscriptPart>> {
    if !included {
        return Ok(Vec::new());
    }
    let transcript =
        crate::analysis::run_policy::accepted_native_video_transcript(catalog, asset, options)?
            .context("explicit native transcript is unavailable")?;
    let source_end =
        clock.source_offset_seconds + clock.local_valid_end_seconds * clock.seconds_scale;
    let mut parts = Vec::new();
    for segment in transcript.segments {
        let a = f64::from(segment.start_ms) / 1000.0;
        let b = f64::from(segment.end_ms) / 1000.0;
        if a >= clock.source_offset_seconds && b <= source_end && a < b {
            ensure!(
                parts.len() < 128 && segment.text.len() <= 8192,
                "native transcript context exceeds window bounds"
            );
            parts.push(TranscriptPart {
                key: format!("T{}", parts.len()),
                local_in_seconds: (a - clock.source_offset_seconds) / clock.seconds_scale,
                local_out_seconds: (b - clock.source_offset_seconds) / clock.seconds_scale,
                text: segment.text,
                source_span: crate::analysis::moments::MomentSpan {
                    start_ms: segment.start_ms,
                    end_ms: segment.end_ms,
                },
            });
        }
    }
    ensure!(
        !parts.is_empty(),
        "no whole accepted transcript segment fits the actual native window; no text was clipped or exported beyond it"
    );
    ensure!(
        parts.iter().map(|p| p.text.len()).sum::<usize>() <= 32768,
        "native transcript window exceeds payload bound"
    );
    Ok(parts)
}

#[cfg(test)]
#[path = "media_tests.rs"]
pub(in crate::analysis::native_video) mod media_tests;
