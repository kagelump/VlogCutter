//! Low-resolution preview render (D01-T021).
//!
//! Renders from the selected review proxy when available, otherwise the original.
//! Each local attempt owns an immutable receipt and unique derived output;
//! completed files are retained for revision-bound review and comparison.
//!
//! Picture uses hard cuts on a common preview canvas. Legacy plans retain their
//! linked-source recipe. Explicit source audio is mixed on a separate 48 kHz
//! clock with only the requested gains and short fades, then muxed and decoded
//! before publication. No failed segment is silently omitted from a complete
//! preview.

use crate::{
    Catalog, media_tool_failure, run_captured_media_command_with_cancellation as run_media,
    tool_command,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
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
/// silently (`AGENTS.md` §15): outcomes name failures and block publication.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    pub output_path: String,
    pub duration_seconds: f64,
    pub clip_count: u32,
    pub failures: Vec<PreviewFailure>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type"
)]
pub enum PreviewProgress {
    Started {
        artifact_id: Option<String>,
        sequence: Option<u64>,
        clip_count: u32,
        worker_count: u32,
    },
    ClipCompleted {
        artifact_id: Option<String>,
        sequence: Option<u64>,
        position: u32,
        elapsed_seconds: f64,
        estimate_remaining_seconds: Option<f64>,
    },
    ClipFailed {
        artifact_id: Option<String>,
        sequence: Option<u64>,
        position: u32,
        message: String,
    },
    Finished {
        artifact_id: Option<String>,
        sequence: Option<u64>,
        output_path: String,
        duration_seconds: f64,
    },
    Cancelled {
        artifact_id: Option<String>,
        sequence: Option<u64>,
    },
}

struct RenderAttempt<'a, 'b> {
    request: &'a PreviewRequest,
    plan: &'a super::EditPlanRevision,
    attempt: &'a super::preview_artifacts::PreviewAttempt<'b>,
    heavy: Option<&'a crate::LocalHeavyLease>,
}

impl Catalog {
    /// Renders a low-resolution preview of a plan.
    ///
    /// Validation runs first: a plan that is not `renderable()` is refused
    /// before encoding. A durable failed attempt records a validation refusal.
    ///
    /// Caller and database cancellation are polled during metadata, resource
    /// waits, hashing and media children. No cancelled output is published.
    pub fn render_preview(
        &self,
        request: &PreviewRequest,
        progress: &mut dyn FnMut(PreviewProgress),
        cancel: &dyn Fn() -> bool,
    ) -> Result<PreviewResult> {
        self.render_preview_internal(request, progress, cancel, None)
    }

    /// An enclosing domain pipeline already owns the host's heavy-work lease.
    pub(crate) fn render_preview_with_heavy_lease(
        &self,
        request: &PreviewRequest,
        progress: &mut dyn FnMut(PreviewProgress),
        cancel: &dyn Fn() -> bool,
        heavy: &crate::LocalHeavyLease,
    ) -> Result<PreviewResult> {
        self.render_preview_internal(request, progress, cancel, Some(heavy))
    }

    fn render_preview_internal(
        &self,
        request: &PreviewRequest,
        progress: &mut dyn FnMut(PreviewProgress),
        cancel: &dyn Fn() -> bool,
        heavy: Option<&crate::LocalHeavyLease>,
    ) -> Result<PreviewResult> {
        let plan = self.edit_plan_revision(&request.revision_id)?;
        ensure!(
            (144..=1080).contains(&request.max_height),
            "invalid input: preview height must be 144-1080"
        );
        let claim = super::preview_artifacts::claim(self, &plan)?;
        let attempt =
            super::preview_artifacts::PreviewAttempt::begin(self, &plan, request, &claim)?;
        let cancelled = || cancel() || attempt.is_cancelled();
        let context = RenderAttempt {
            request,
            plan: &plan,
            attempt: &attempt,
            heavy,
        };
        let result = self.render_preview_attempt(&context, progress, &cancelled);
        match result {
            Ok(result) => Ok(result),
            Err(error) => {
                let was_cancelled = cancel() || attempt.cancellation_requested_now();
                let error = if was_cancelled {
                    error.context(super::PreviewArtifactError::Cancelled {
                        artifact_id: attempt.artifact_id.clone(),
                    })
                } else {
                    error
                };
                let sequence = attempt.finish_error(&error, was_cancelled)?;
                if was_cancelled {
                    progress(PreviewProgress::Cancelled {
                        artifact_id: Some(attempt.artifact_id.clone()),
                        sequence: Some(sequence),
                    });
                }
                Err(error)
            }
        }
    }

    fn render_preview_attempt(
        &self,
        context: &RenderAttempt<'_, '_>,
        progress: &mut dyn FnMut(PreviewProgress),
        cancel: &dyn Fn() -> bool,
    ) -> Result<PreviewResult> {
        let request = context.request;
        let plan = context.plan;
        let attempt = context.attempt;
        ensure!(
            plan.duration_seconds().is_finite()
                && plan.duration_seconds() > 0.0
                && plan.duration_seconds() <= 21600.0,
            "invalid input: preview duration must be positive and no longer than six hours"
        );
        let constraints_key = super::ranges::constraints_key(&self.connection()?, plan)?;
        let report = self.validate_edit_plan_cancellable(&request.revision_id, cancel)?;
        if !report.renderable() {
            let blocking: Vec<&str> = report.errors().map(|finding| finding.code).collect();
            bail!(
                "edit plan {} cannot be rendered: {}",
                request.revision_id,
                blocking.join(", ")
            );
        }

        let assets = self.list_assets(&plan.project_id)?;
        let split_audio = plan.operations.iter().any(|cut| cut.audio.is_some());
        let audio_events = if split_audio {
            super::audio::audio_events_cancellable(plan, &assets, cancel)?
        } else {
            Vec::new()
        };
        let workspace = attempt.directory.clone();
        let sequence = attempt.stage("waitingForResource")?;
        progress(PreviewProgress::Started {
            artifact_id: Some(attempt.artifact_id.clone()),
            sequence: Some(sequence),
            clip_count: plan.operations.len() as u32,
            worker_count: 1,
        });
        let _heavy = if context.heavy.is_some() {
            None
        } else {
            Some(self.acquire_local_heavy_cancellable(cancel)?)
        };
        attempt.stage("identifyingSources")?;
        let prepared = super::preview_media::prepare(self, plan, request, &assets, cancel)?;
        attempt.identity(&prepared.identity)?;
        let segments_dir = workspace.join("segments");
        fs::create_dir(&segments_dir)?;
        let mut segments: Vec<PathBuf> = Vec::new();
        let mut failures: Vec<PreviewFailure> = Vec::new();
        let mut segment_frames = Vec::new();
        let started = std::time::Instant::now();
        let mut picture_frames = 0_u64;
        let mut requested_picture_end = 0.0;

        for (index, operation) in plan.operations.iter().enumerate() {
            if cancel() {
                // Leave nothing that could be presented as a complete preview.
                return Err(anyhow!("preview render cancelled"));
            }

            let Some(asset) = assets
                .iter()
                .find(|candidate| candidate.id == operation.asset_id)
            else {
                let message = format!("clip {} is not in this project", operation.asset_id);
                progress(PreviewProgress::ClipFailed {
                    artifact_id: Some(attempt.artifact_id.clone()),
                    sequence: Some(attempt.stage("segmentFailed")?),
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

            let source = prepared
                .paths
                .get(&asset.id)
                .and_then(|p| p.to_str())
                .context("preview selected source unavailable")?;
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
            let mut normalize = format!(
                "{rotation}scale={canvas_width}:{canvas_height}:force_original_aspect_ratio=decrease,pad={canvas_width}:{canvas_height}:(ow-iw)/2:(oh-ih)/2,fps=30000/1001,setsar=1,setpts=PTS-STARTPTS"
            );
            let mut expected_frames = None;
            let segment_duration = if split_audio {
                requested_picture_end += duration;
                let end_frame = (requested_picture_end * 30000.0 / 1001.0).round() as u64;
                let frames = end_frame.saturating_sub(picture_frames);
                ensure!(
                    frames > 0,
                    "cut {} is shorter than a preview frame",
                    operation.operation_id
                );
                picture_frames = end_frame;
                expected_frames = Some(frames);
                normalize.push_str(",tpad=stop_mode=clone:stop_duration=0.1");
                frames as f64 * 1001.0 / 30000.0
            } else {
                duration
            };
            let has_audio = !split_audio && source_has_audio(source, cancel)?;
            let mut command = tool_command("ffmpeg");
            command
                .args(["-y", "-hide_banner", "-loglevel", "error", "-ss"])
                .arg(operation.source_in_seconds.to_string())
                .arg("-t")
                .arg(duration.to_string())
                .arg("-noautorotate")
                .arg("-i")
                .arg(source);
            if split_audio {
                command.args(["-map", "0:v:0", "-an"]);
            } else if has_audio {
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
                .arg(segment_duration.to_string())
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
            let (status, diagnostic) = run_media(
                &mut command,
                std::path::Path::new(source),
                &segment,
                &mut || cancel(),
            )
            .context("start ffmpeg preview segment process")?;

            let checked = if status.success() {
                super::preview_media::segment_frame_count(
                    &segment,
                    &workspace,
                    &super::preview_media::SegmentExpectation {
                        width: canvas_width,
                        height: canvas_height,
                        seconds: segment_duration,
                        frames: expected_frames,
                        audio: !split_audio,
                    },
                    cancel,
                )
            } else {
                Err(media_tool_failure(
                    "ffmpeg preview segment",
                    status,
                    diagnostic,
                ))
            };
            let count = match checked {
                Ok(count) => count,
                Err(error) => {
                    let message = error.to_string();
                    let sequence = attempt.outcome(&super::PreviewCutOutcome {
                        operation_id: operation.operation_id.clone(),
                        position: operation.position,
                        asset_id: operation.asset_id.clone(),
                        state: "failed".into(),
                        reused: false,
                        cache_key: None,
                        failure: Some(message.clone()),
                    })?;
                    progress(PreviewProgress::ClipFailed {
                        artifact_id: Some(attempt.artifact_id.clone()),
                        sequence: Some(sequence),
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
            };
            segment_frames.push(count);
            let sequence = attempt.outcome(&super::PreviewCutOutcome {
                operation_id: operation.operation_id.clone(),
                position: operation.position,
                asset_id: operation.asset_id.clone(),
                state: "encoded".into(),
                reused: false,
                cache_key: None,
                failure: None,
            })?;
            segments.push(segment);
            let elapsed = started.elapsed().as_secs_f64();
            let completed = index + 1;
            progress(PreviewProgress::ClipCompleted {
                artifact_id: Some(attempt.artifact_id.clone()),
                sequence: Some(sequence),
                position: operation.position,
                elapsed_seconds: elapsed,
                estimate_remaining_seconds: (completed > 0).then(|| {
                    elapsed / completed as f64 * (plan.operations.len() - completed) as f64
                }),
            });
        }

        if !failures.is_empty() {
            bail!(
                "preview incomplete: {} source segments failed; no completed preview was published",
                failures.len()
            );
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
        let (status, diagnostic) =
            run_media(&mut command, &list_path, &temporary, &mut || cancel())
                .context("start ffmpeg preview concat process")?;
        if !status.success() {
            let _ = fs::remove_file(&temporary);
            return Err(media_tool_failure(
                "ffmpeg preview concat",
                status,
                diagnostic,
            ));
        }
        if split_audio {
            ensure!(!cancel(), "preview render cancelled before audio mix");
            let mix = super::audio_render::render_mix(
                &workspace,
                &audio_events,
                plan.duration_seconds(),
                cancel,
            )?;
            let muxed = workspace.join("preview.audio.tmp.mp4");
            let rendered_seconds = super::audio_render::mux_mix(&temporary, &mix, &muxed, cancel)?;
            ensure!(
                (rendered_seconds - plan.duration_seconds()).abs() <= 0.1,
                "split audio preview duration differs from planned duration"
            );
            ensure!(!cancel(), "preview render cancelled before publication");
            fs::rename(&muxed, &temporary)?;
            fs::remove_file(mix)?;
        }
        attempt.stage("validatingOutput")?;
        let validation = super::preview_media::validate_movie(
            &temporary,
            &workspace,
            plan,
            request,
            &segment_frames,
            cancel,
        )?;
        let current = super::preview_media::prepare(
            self,
            plan,
            request,
            &self.list_assets(&plan.project_id)?,
            cancel,
        )?;
        ensure!(
            current.identity.sources == prepared.identity.sources
                && current.identity.tools == prepared.identity.tools
                && current.identity.plan_digest == prepared.identity.plan_digest,
            "preview source media or rendering recipe changed during the attempt"
        );
        let result = PreviewResult {
            artifact_id: Some(attempt.artifact_id.clone()),
            output_path: output.to_string_lossy().into_owned(),
            duration_seconds: validation.duration_seconds,
            clip_count: segments.len() as u32,
            failures,
        };
        ensure!(!cancel(), "preview render cancelled before publication");
        // Only the short publication window excludes concurrent human writes;
        // media rendering never holds SQLite's write lock.
        let mut publication_connection = self.connection()?;
        let publication = publication_connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure!(
            super::ranges::constraints_key(&publication, plan)? == constraints_key,
            "human source constraints changed during rendering; review this revision before publishing another preview"
        );
        let sequence = attempt.complete(&publication, &result, &validation)?;
        fs::rename(&temporary, &output).context("commit rendered preview")?;
        publication.commit()?;

        progress(PreviewProgress::Finished {
            artifact_id: Some(attempt.artifact_id.clone()),
            sequence: Some(sequence),
            output_path: output.to_string_lossy().into_owned(),
            duration_seconds: validation.duration_seconds,
        });
        Ok(result)
    }
}

pub(super) fn source_has_audio(source: &str, cancel: &dyn Fn() -> bool) -> Result<bool> {
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
    let output = crate::stall::output_with_timeout_and_cancellation(
        &mut command,
        "ffprobe preview audio stream",
        crate::stall::io_stall_timeout(),
        &cancel,
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
