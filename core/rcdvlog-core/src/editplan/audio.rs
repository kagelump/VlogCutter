//! Versioned source audio placement. Picture order remains a single hard-cut track.
use super::EditPlanRevision;
use crate::Asset;
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const AUDIO_SAMPLE_RATE: u32 = 48_000;

/// Omitted audio means the original linked-source behavior. Explicit silence is
/// useful for B-roll covering another cut's retained dialogue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase", deny_unknown_fields)]
pub enum EditPlanAudio {
    Silent,
    Source(SourceAudio),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceAudio {
    pub version: u32,
    pub asset_id: String,
    pub source_in_seconds: f64,
    pub source_out_seconds: f64,
    pub timeline_start_seconds: f64,
    /// audio source clock minus picture source clock during coincident imagery.
    /// Zero preserves sync; a nonzero intentional offset must be explicit.
    pub sync_offset_seconds: f64,
    #[serde(default)]
    pub gain_db: f64,
    #[serde(default)]
    pub fade_in_seconds: f64,
    #[serde(default)]
    pub fade_out_seconds: f64,
}

#[derive(Debug, Clone)]
pub(super) struct AudioEvent {
    pub source: String,
    pub source_in_samples: u64,
    pub source_out_samples: u64,
    pub timeline_start_samples: u64,
    pub gain_db: f64,
    pub fade_in_samples: u64,
    pub fade_out_samples: u64,
}

impl AudioEvent {
    pub fn sample_count(&self) -> u64 {
        self.source_out_samples - self.source_in_samples
    }
    fn timeline_end(&self) -> u64 {
        self.timeline_start_samples + self.sample_count()
    }
}

pub(super) fn review_source(asset: &Asset) -> &str {
    asset
        .proxy_path
        .as_deref()
        .filter(|path| Path::new(path).is_file())
        .unwrap_or(&asset.path)
}

pub(super) fn audio_events_cancellable_window(
    plan: &EditPlanRevision,
    assets: &[Asset],
    cancel: &dyn Fn() -> bool,
    window: Option<(f64, f64)>,
) -> Result<Vec<AudioEvent>> {
    let rate = f64::from(AUDIO_SAMPLE_RATE);
    let movie_duration = plan.duration_seconds();
    ensure!(
        movie_duration.is_finite() && movie_duration <= 6.0 * 3600.0,
        "split-audio previews require a finite movie duration at most six hours"
    );
    let mut picture = Vec::new();
    let mut cursor = 0.0;
    let mut picture_frames = 0_u64;
    for cut in &plan.operations {
        ensure!(!cancel(), "preview cancelled while validating audio");
        let end = cursor + cut.source_out_seconds - cut.source_in_seconds;
        let end_frame = (end * 30000.0 / 1001.0).round() as u64;
        ensure!(
            end_frame > picture_frames,
            "cut {} is shorter than a preview frame",
            cut.operation_id
        );
        picture_frames = end_frame;
        picture.push((cut, cursor, end));
        cursor = end;
    }
    let mut events = Vec::new();
    let mut audio_bounds = std::collections::HashMap::new();
    for (cut, start, _) in &picture {
        ensure!(!cancel(), "preview cancelled while validating audio");
        let (
            asset_id,
            source_in,
            source_out,
            placement,
            sync_offset,
            gain,
            fade_in,
            fade_out,
            explicit,
        ) = match &cut.audio {
            Some(EditPlanAudio::Silent) => continue,
            Some(EditPlanAudio::Source(audio)) => {
                ensure!(
                    audio.version == 1,
                    "cut {}: unsupported source audio version {}",
                    cut.operation_id,
                    audio.version
                );
                (
                    &audio.asset_id,
                    audio.source_in_seconds,
                    audio.source_out_seconds,
                    audio.timeline_start_seconds,
                    audio.sync_offset_seconds,
                    audio.gain_db,
                    audio.fade_in_seconds,
                    audio.fade_out_seconds,
                    true,
                )
            }
            None => (
                &cut.asset_id,
                cut.source_in_seconds,
                cut.source_out_seconds,
                *start,
                0.0,
                0.0,
                0.0,
                0.0,
                false,
            ),
        };
        ensure!(
            [
                source_in,
                source_out,
                placement,
                sync_offset,
                gain,
                fade_in,
                fade_out
            ]
            .iter()
            .all(|value| value.is_finite()),
            "cut {}: all audio timing and gain values must be finite",
            cut.operation_id
        );
        if let Some((start, end)) = window
            && (placement >= end || placement + source_out - source_in <= start)
        {
            continue;
        }
        let asset = assets
            .iter()
            .find(|asset| &asset.id == asset_id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "cut {}: audio asset does not belong to this project",
                    cut.operation_id
                )
            })?;
        ensure!(
            source_in >= 0.0 && source_out > source_in && source_out <= asset.duration_seconds,
            "cut {}: audio range exceeds available source handles",
            cut.operation_id
        );
        ensure!(
            placement >= 0.0 && placement + source_out - source_in <= movie_duration + 1e-9,
            "cut {}: audio must stay inside the picture timeline",
            cut.operation_id
        );
        ensure!(
            (-60.0..=0.0).contains(&gain),
            "cut {}: gainDb must be -60..=0",
            cut.operation_id
        );
        ensure!(
            (0.0..=0.25).contains(&fade_in)
                && (0.0..=0.25).contains(&fade_out)
                && fade_in + fade_out <= source_out - source_in,
            "cut {}: audio fades must be nonnegative, at most 250 ms, and fit the source range",
            cut.operation_id
        );
        for (other, video_start, video_end) in &picture {
            if &other.asset_id == asset_id
                && placement < *video_end - 1e-9
                && *video_start < placement + (source_out - source_in) - 1e-9
            {
                let actual_offset =
                    (source_in - placement) - (other.source_in_seconds - video_start);
                ensure!(
                    (actual_offset - sync_offset).abs() <= 1.0 / rate + 1e-9,
                    "cut {}: audio/picture sync differs from explicit syncOffsetSeconds at cut {}",
                    cut.operation_id,
                    other.operation_id
                );
            }
        }
        let source = review_source(asset);
        ensure!(
            Path::new(source).is_file(),
            "cut {}: audio representation is offline",
            cut.operation_id
        );
        if !audio_bounds.contains_key(source) {
            audio_bounds.insert(source.to_string(), probe_audio_bounds(source, cancel)?);
        }
        let bounds = audio_bounds
            .get(source)
            .expect("audio bounds were inserted");
        if !bounds.present {
            ensure!(
                !explicit,
                "cut {}: selected audio source has no audio stream",
                cut.operation_id
            );
            continue;
        }
        if explicit {
            let (audio_start, audio_end) = bounds.range.ok_or_else(|| {
                anyhow::anyhow!(
                    "cut {}: selected audio handle duration could not be verified",
                    cut.operation_id
                )
            })?;
            ensure!(
                source_in >= audio_start.max(0.0) - 1e-9 && source_out <= audio_end + 1e-9,
                "cut {}: requested source audio exceeds the selected representation's audio handles",
                cut.operation_id
            );
        }
        // Inward source rounding never requests bytes beyond an accepted handle.
        let event = AudioEvent {
            source: source.into(),
            source_in_samples: (source_in * rate).ceil() as u64,
            source_out_samples: (source_out * rate).floor() as u64,
            timeline_start_samples: (placement * rate).round() as u64,
            gain_db: gain,
            fade_in_samples: (fade_in * rate).round() as u64,
            fade_out_samples: (fade_out * rate).round() as u64,
        };
        ensure!(
            event.source_out_samples > event.source_in_samples,
            "cut {}: audio range contains no whole 48 kHz sample",
            cut.operation_id
        );
        events.push(event);
    }
    // End before start at an equal sample: adjacent intervals do not overlap.
    let mut boundaries: Vec<_> = events
        .iter()
        .flat_map(|event| {
            [
                (event.timeline_start_samples, 1_i32),
                (event.timeline_end(), -1_i32),
            ]
        })
        .collect();
    boundaries.sort_unstable();
    let mut active = 0;
    for (_, delta) in boundaries {
        active += delta;
        if active > 2 {
            bail!(
                "at most two source-audio intervals may overlap; explicitly silence or shorten covered cuts"
            );
        }
    }
    Ok(events)
}

pub(super) struct AudioBounds {
    pub present: bool,
    pub range: Option<(f64, f64)>,
}

pub(super) fn probe_audio_bounds(source: &str, cancel: &dyn Fn() -> bool) -> Result<AudioBounds> {
    let mut command = crate::tool_command("ffprobe");
    command
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=start_time,duration,start_pts,duration_ts,time_base",
            "-of",
            "json",
        ])
        .arg(source);
    let output = crate::stall::output_with_timeout_and_cancellation(
        &mut command,
        "verify source audio handles",
        crate::stall::io_stall_timeout(),
        &cancel,
    )?;
    ensure!(
        output.status.success(),
        "cannot inspect selected source audio handles"
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let Some(stream) = json["streams"]
        .as_array()
        .and_then(|streams| streams.first())
    else {
        return Ok(AudioBounds {
            present: false,
            range: None,
        });
    };
    let tick_seconds = stream["time_base"]
        .as_str()
        .and_then(|value| value.split_once('/'))
        .and_then(|(n, d)| Some((n.parse::<f64>().ok()?, d.parse::<f64>().ok()?)))
        .filter(|(n, d)| n.is_finite() && d.is_finite() && *n > 0.0 && *d > 0.0)
        .map(|(n, d)| n / d);
    let start_ticks = stream["start_pts"]
        .as_i64()
        .zip(tick_seconds)
        .map(|(ticks, seconds)| ticks as f64 * seconds);
    let duration_ticks = stream["duration_ts"]
        .as_i64()
        .zip(tick_seconds)
        .map(|(ticks, seconds)| ticks as f64 * seconds);
    let start = stream["start_time"]
        .as_str()
        .and_then(|value| value.parse::<f64>().ok());
    let start = start_ticks.or(start);
    let duration = stream["duration"]
        .as_str()
        .and_then(|value| value.parse::<f64>().ok());
    let duration = duration_ticks.or(duration);
    let range = start
        .zip(duration)
        .filter(|(start, duration)| start.is_finite() && duration.is_finite() && *duration > 0.0)
        .map(|(start, duration)| (start, start + duration));
    Ok(AudioBounds {
        present: true,
        range,
    })
}
