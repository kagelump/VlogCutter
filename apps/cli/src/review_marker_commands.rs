//! `review-markers`: acoustic movie-time markers for TV review copies (#226).
//! Thin adapter over `rcdvlog_core::acoustic_markers`; no catalog is opened.
use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use rcdvlog_core::ReviewTargetHandoff;
use rcdvlog_core::acoustic_markers::{
    MarkedCopyRequest, detect_markers, export_key_for, export_marked_review_copy, plan_markers,
    read_wav_mono,
};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Subcommand)]
pub enum ReviewMarkersCommand {
    /// Show where markers would go for a review target handoff.
    Plan {
        #[arg(long)]
        handoff: PathBuf,
        #[arg(long, default_value_t = 30.0)]
        interval_seconds: f64,
        #[arg(long, default_value_t = 2.0)]
        first_marker_seconds: f64,
        #[arg(long, default_value_t = -14.0, allow_hyphen_values = true)]
        level_dbfs: f64,
    },
    /// Write a separate create-only marked review copy of the verified master.
    Export {
        #[arg(long)]
        handoff: PathBuf,
        /// The verified master movie (BLAKE3 must match the handoff).
        #[arg(long)]
        movie: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value_t = 30.0)]
        interval_seconds: f64,
        #[arg(long, default_value_t = 2.0)]
        first_marker_seconds: f64,
        #[arg(long, default_value_t = -14.0, allow_hyphen_values = true)]
        level_dbfs: f64,
    },
    /// Detect marker anchors in a microphone recording (WAV).
    Detect {
        #[arg(long)]
        handoff: PathBuf,
        #[arg(long)]
        recording: PathBuf,
    },
}

pub fn command_name(command: &ReviewMarkersCommand) -> &'static str {
    match command {
        ReviewMarkersCommand::Plan { .. } => "review-markers.plan",
        ReviewMarkersCommand::Export { .. } => "review-markers.export",
        ReviewMarkersCommand::Detect { .. } => "review-markers.detect",
    }
}

fn read_handoff(path: &Path) -> Result<ReviewTargetHandoff> {
    let bytes =
        std::fs::read(path).with_context(|| "invalid input: cannot read the handoff file")?;
    let value: Value =
        serde_json::from_slice(&bytes).context("invalid input: handoff is not JSON")?;
    // Accept the CLI success envelope or its `data` member.
    let data = if value.get("ok") == Some(&Value::Bool(true)) {
        value.get("data").cloned().unwrap_or(value)
    } else {
        value
    };
    ReviewTargetHandoff::from_json(&serde_json::to_vec(&data)?)
}

fn absolute(path: &Path, what: &str) -> Result<()> {
    ensure!(
        path.is_absolute(),
        "invalid input: --{what} must be an absolute path"
    );
    Ok(())
}

pub fn execute(command: &ReviewMarkersCommand) -> Result<Value> {
    match command {
        ReviewMarkersCommand::Plan {
            handoff,
            interval_seconds,
            first_marker_seconds,
            level_dbfs,
        } => {
            let handoff = read_handoff(handoff)?;
            Ok(serde_json::to_value(plan_markers(
                &handoff.target_digest,
                handoff.target.movie.duration_seconds,
                *interval_seconds,
                *first_marker_seconds,
                *level_dbfs,
            )?)?)
        }
        ReviewMarkersCommand::Export {
            handoff,
            movie,
            output,
            interval_seconds,
            first_marker_seconds,
            level_dbfs,
        } => {
            absolute(movie, "movie")?;
            absolute(output, "output")?;
            let handoff = read_handoff(handoff)?;
            let ffmpeg = PathBuf::from(
                std::env::var_os("RCDVLOG_FFMPEG").unwrap_or_else(|| "ffmpeg".into()),
            );
            let ffprobe = PathBuf::from(
                std::env::var_os("RCDVLOG_FFPROBE").unwrap_or_else(|| "ffprobe".into()),
            );
            let manifest = export_marked_review_copy(&MarkedCopyRequest {
                handoff: &handoff,
                movie,
                output,
                interval_seconds: *interval_seconds,
                first_marker_seconds: *first_marker_seconds,
                level_dbfs: *level_dbfs,
                ffmpeg: &ffmpeg,
                ffprobe: &ffprobe,
            })?;
            let manifest_path = output.with_extension("markers.json");
            ensure!(
                !manifest_path.exists(),
                "invalid input: manifest already exists beside the output"
            );
            std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
            Ok(serde_json::to_value(manifest)?)
        }
        ReviewMarkersCommand::Detect { handoff, recording } => {
            let handoff = read_handoff(handoff)?;
            let (pcm, rate) = read_wav_mono(recording)
                .context("invalid input: recording must be a readable WAV")?;
            Ok(serde_json::to_value(detect_markers(
                &pcm,
                rate,
                export_key_for(&handoff.target_digest),
            )?)?)
        }
    }
}
