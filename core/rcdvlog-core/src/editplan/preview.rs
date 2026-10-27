//! Low-resolution preview render (D01-T021).
//!
//! Renders the plan from *review proxies*, not originals. This is a preview:
//! `PRD.md` §23 Milestone 6 asks for low resolution, and rendering from camera
//! masters would be slow and would touch source media for no benefit.
//!
//! The preview is not an export and is never handed to a finishing editor. It
//! carries no determinism requirement — D01-T022 owns that.
//!
//! Hard cuts only. No transition, no crossfade, no audio ramp, including none
//! introduced incidentally by an FFmpeg filter default: each segment is cut
//! with a stream copy and the segments are concatenated, so there is no filter
//! graph that could add one.

use crate::{Catalog, media_tool_failure, run_captured_media_command, tool_command};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRequest {
    pub revision_id: String,
    pub max_height: u32,
}

/// One clip that could not be rendered. Per-clip failures never abort the batch
/// silently (`AGENTS.md` §15): the render continues and names what it dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewFailure {
    pub position: u32,
    pub asset_id: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewResult {
    pub output_path: String,
    pub duration_seconds: f64,
    pub clip_count: u32,
    pub failures: Vec<PreviewFailure>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum PreviewProgress {
    Started {
        clip_count: u32,
        worker_count: u32,
    },
    ClipCompleted {
        position: u32,
        elapsed_seconds: f64,
        estimate_remaining_seconds: Option<f64>,
    },
    ClipFailed {
        position: u32,
        message: String,
    },
    Finished {
        output_path: String,
        duration_seconds: f64,
    },
    Cancelled,
}

impl Catalog {
    /// Renders a low-resolution preview of a plan.
    ///
    /// Validation runs first: a plan that is not `renderable()` is refused
    /// before FFmpeg is ever invoked, and that refusal is a refusal — not a
    /// render failure.
    ///
    /// `cancel` is polled between clips, which is the granularity at which a
    /// concat render can stop cleanly. A cancelled render leaves nothing that
    /// could be mistaken for a finished preview.
    pub fn render_preview(
        &self,
        request: &PreviewRequest,
        progress: &mut dyn FnMut(PreviewProgress),
        cancel: &dyn Fn() -> bool,
    ) -> Result<PreviewResult> {
        let plan = self.edit_plan_revision(&request.revision_id)?;
        let report = self.validate_edit_plan(&request.revision_id)?;
        if !report.renderable() {
            let blocking: Vec<&str> = report.errors().map(|finding| finding.code).collect();
            bail!(
                "edit plan {} cannot be rendered: {}",
                request.revision_id,
                blocking.join(", ")
            );
        }

        let assets = self.list_assets(&plan.project_id)?;
        let workspace = self.preview_workspace(&plan.project_id, &plan.revision_id)?;
        // A previous attempt's segments must never be concatenated into this
        // one, so the workspace starts empty every time.
        let segments_dir = workspace.join("segments");
        if segments_dir.exists() {
            fs::remove_dir_all(&segments_dir)?;
        }
        fs::create_dir_all(&segments_dir)?;

        progress(PreviewProgress::Started {
            clip_count: plan.operations.len() as u32,
            worker_count: 1,
        });

        let mut segments: Vec<PathBuf> = Vec::new();
        let mut failures: Vec<PreviewFailure> = Vec::new();
        let mut rendered_seconds = 0.0f64;
        let started = std::time::Instant::now();

        for (index, operation) in plan.operations.iter().enumerate() {
            if cancel() {
                // Leave nothing that could be presented as a complete preview.
                let _ = fs::remove_dir_all(&segments_dir);
                progress(PreviewProgress::Cancelled);
                return Err(anyhow!("preview render cancelled"));
            }

            let Some(asset) = assets
                .iter()
                .find(|candidate| candidate.id == operation.asset_id)
            else {
                let message = format!("clip {} is not in this project", operation.asset_id);
                progress(PreviewProgress::ClipFailed {
                    position: operation.position,
                    message: message.clone(),
                });
                failures.push(PreviewFailure {
                    position: operation.position,
                    asset_id: operation.asset_id.clone(),
                    message,
                });
                continue;
            };

            // Proxy first; the original is the fallback so a project without
            // proxies still previews rather than failing outright.
            let source = asset
                .proxy_path
                .as_deref()
                .filter(|proxy| std::path::Path::new(proxy).is_file())
                .unwrap_or(asset.path.as_str());

            let segment = segments_dir.join(format!("{:05}.mp4", operation.position));
            let duration = (operation.source_out_seconds - operation.source_in_seconds).max(0.0);
            // Arguments are structured through `Command`, never assembled as a
            // shell string: media paths contain spaces and non-ASCII characters
            // (`AGENTS.md` §15).
            let canvas_height = request.max_height.max(2) / 2 * 2;
            let canvas_width = (canvas_height * 16 / 9) / 2 * 2;
            let rotation = match asset.rotation_degrees % 360 {
                90 => "transpose=clock,",
                180 => "hflip,vflip,",
                270 => "transpose=cclock,",
                _ => "",
            };
            let normalize = format!(
                "{rotation}scale={canvas_width}:{canvas_height}:force_original_aspect_ratio=decrease,pad={canvas_width}:{canvas_height}:(ow-iw)/2:(oh-ih)/2,fps=30000/1001,setsar=1,setpts=PTS-STARTPTS"
            );
            let has_audio = source_has_audio(source)?;
            let mut command = tool_command("ffmpeg");
            command
                .args(["-y", "-hide_banner", "-loglevel", "error", "-ss"])
                .arg(operation.source_in_seconds.to_string())
                .arg("-t")
                .arg(duration.to_string())
                .arg("-noautorotate")
                .arg("-i")
                .arg(source);
            if has_audio {
                command.args(["-map", "0:v:0", "-map", "0:a:0"]);
            } else {
                command.args([
                    "-f",
                    "lavfi",
                    "-i",
                    "anullsrc=channel_layout=stereo:sample_rate=48000",
                    "-map",
                    "0:v:0",
                    "-map",
                    "1:a:0",
                ]);
            }
            command
                .arg("-t")
                .arg(duration.to_string())
                .args([
                    "-vf",
                    &normalize,
                    "-c:v",
                    "libx264",
                    "-preset",
                    "veryfast",
                    "-crf",
                    "28",
                    "-pix_fmt",
                    "yuv420p",
                    "-r",
                    "30000/1001",
                    "-video_track_timescale",
                    "30000",
                    "-c:a",
                    "aac",
                    "-b:a",
                    "96k",
                    "-ar",
                    "48000",
                    "-ac",
                    "2",
                    "-af",
                    "aresample=48000:async=1:first_pts=0,asetpts=PTS-STARTPTS",
                    "-map_metadata",
                    "-1",
                ])
                .arg(&segment);
            let (status, diagnostic) =
                run_captured_media_command(&mut command, std::path::Path::new(source), &segment)
                    .context("start ffmpeg preview segment process")?;

            if !status.success() {
                let message =
                    media_tool_failure("ffmpeg preview segment", status, diagnostic).to_string();
                progress(PreviewProgress::ClipFailed {
                    position: operation.position,
                    message: message.clone(),
                });
                failures.push(PreviewFailure {
                    position: operation.position,
                    asset_id: operation.asset_id.clone(),
                    message,
                });
                let _ = fs::remove_file(&segment);
                continue;
            }

            segments.push(segment);
            rendered_seconds += duration;
            let elapsed = started.elapsed().as_secs_f64();
            let completed = index + 1;
            progress(PreviewProgress::ClipCompleted {
                position: operation.position,
                elapsed_seconds: elapsed,
                estimate_remaining_seconds: (completed > 0).then(|| {
                    elapsed / completed as f64 * (plan.operations.len() - completed) as f64
                }),
            });
        }

        if segments.is_empty() {
            bail!(
                "preview produced no segments; {} clips failed",
                failures.len()
            );
        }

        // Concat demuxer: joins already-encoded segments end to end with no
        // filter graph, so no transition can be introduced incidentally.
        let list_path = workspace.join("segments.txt");
        let mut list = String::new();
        for segment in &segments {
            // The concat demuxer's own quoting rule: single quotes, with an
            // embedded quote written as '\''.
            let escaped = segment.to_string_lossy().replace('\'', r"'\''");
            list.push_str(&format!("file '{escaped}'\n"));
        }
        fs::write(&list_path, list)?;

        let output = workspace.join("preview.mp4");
        let temporary = workspace.join("preview.tmp.mp4");
        let mut command = tool_command("ffmpeg");
        command
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "concat",
                "-safe",
                "0",
                "-i",
            ])
            .arg(&list_path)
            .args(["-c", "copy", "-movflags", "+faststart"])
            .arg(&temporary);
        let (status, diagnostic) = run_captured_media_command(&mut command, &list_path, &temporary)
            .context("start ffmpeg preview concat process")?;
        if !status.success() {
            let _ = fs::remove_file(&temporary);
            return Err(media_tool_failure(
                "ffmpeg preview concat",
                status,
                diagnostic,
            ));
        }
        // Renamed only once complete, so a crash mid-render never leaves a
        // truncated file sitting at the preview's path.
        fs::rename(&temporary, &output).context("commit rendered preview")?;

        progress(PreviewProgress::Finished {
            output_path: output.to_string_lossy().into_owned(),
            duration_seconds: rendered_seconds,
        });
        Ok(PreviewResult {
            output_path: output.to_string_lossy().into_owned(),
            duration_seconds: rendered_seconds,
            clip_count: segments.len() as u32,
            failures,
        })
    }

    /// The cache directory for one plan revision's preview, keyed by revision
    /// id so one revision's preview is never confused with another's.
    fn preview_workspace(&self, project_id: &str, revision_id: &str) -> Result<PathBuf> {
        let parent = self
            .database_path()
            .parent()
            .ok_or_else(|| anyhow!("catalog has no parent directory"))?
            .to_path_buf();
        let workspace = parent
            .join("cache")
            .join("previews")
            .join(project_id)
            .join(revision_id);
        fs::create_dir_all(&workspace)?;
        Ok(workspace)
    }
}

fn source_has_audio(source: &str) -> Result<bool> {
    let mut command = tool_command("ffprobe");
    command
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_type",
            "-of",
            "json",
        ])
        .arg(source);
    let output = crate::stall::output_with_timeout(
        &mut command,
        "ffprobe preview audio stream",
        crate::stall::io_stall_timeout(),
    )?;
    if !output.status.success() {
        bail!("ffprobe could not inspect preview audio stream");
    }
    let document: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("parse ffprobe preview audio stream response")?;
    Ok(document["streams"]
        .as_array()
        .is_some_and(|streams| !streams.is_empty()))
}
