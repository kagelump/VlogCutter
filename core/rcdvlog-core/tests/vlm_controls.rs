//! Live negative controls for the vision prompt and current default model.
//!
//! Ignored by default because this spends real OpenRouter credit. Run with
//! `RCDVLOG_LIVE_VLM=1`; optionally point `RCDVLOG_VLM_CONTROL_FRAME` at a real
//! extracted JPEG so the heavy-blur control is derived from project footage.

use anyhow::{Context, Result, bail};
use rcdvlog_core::{
    DEFAULT_MODEL, FALLBACK_MODEL, OpenRouterProvider, VlmProvider, VlmRequest, frame_data_uri,
    output_schema, resolve_api_key, validate_observation,
};
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(mut command: Command) -> Result<()> {
    let status = command.status().context("run ffmpeg control generator")?;
    if !status.success() {
        bail!("ffmpeg control generator failed with {status}");
    }
    Ok(())
}

fn controls(directory: &Path) -> Result<Vec<PathBuf>> {
    let black = directory.join("black.jpg");
    let mut command = Command::new("ffmpeg");
    command
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=768x432:d=1",
            "-frames:v",
            "1",
        ])
        .arg(&black);
    run(command)?;

    let noise = directory.join("noise.jpg");
    let mut command = Command::new("ffmpeg");
    command
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "nullsrc=s=768x432,geq=random(1)*255:random(2)*255:random(3)*255",
            "-frames:v",
            "1",
        ])
        .arg(&noise);
    run(command)?;

    let blur = directory.join("blur.jpg");
    let mut command = Command::new("ffmpeg");
    command.args(["-y", "-hide_banner", "-loglevel", "error"]);
    if let Ok(reference) = std::env::var("RCDVLOG_VLM_CONTROL_FRAME") {
        command.arg("-i").arg(reference);
    } else {
        command.args(["-f", "lavfi", "-i", "testsrc2=s=768x432:d=1"]);
    }
    command
        .args(["-vf", "boxblur=50:20,scale=768:-2", "-frames:v", "1"])
        .arg(&blur);
    run(command)?;
    Ok(vec![black, noise, blur])
}

#[test]
#[ignore = "live OpenRouter regression; set RCDVLOG_LIVE_VLM=1"]
fn blank_noise_and_heavy_blur_are_refused_without_invented_ocr() -> Result<()> {
    if std::env::var("RCDVLOG_LIVE_VLM").as_deref() != Ok("1") {
        bail!("set RCDVLOG_LIVE_VLM=1 to acknowledge live OpenRouter spend");
    }
    let directory = tempfile::tempdir()?;
    let controls = controls(directory.path())?;
    let provider = OpenRouterProvider::new(resolve_api_key()?)?;
    let schema = output_schema(&[400], 1_000);
    let prompt = "These are frames from one clip. Return the bounded observation JSON. Set usable=false for black, noise, blank, or unreadably blurred input. Never invent OCR text.";
    let mut fallback_served = 0;
    let mut total_cost = 0.0;
    let mut default_available = true;
    for control in controls {
        let uri = frame_data_uri(&std::fs::read(&control)?);
        // Four repetitions protect against a lucky deterministic-looking pass.
        for repetition in 0..4 {
            let request = VlmRequest {
                prompt: prompt.to_string(),
                frames: vec![uri.clone()],
                schema: schema.clone(),
            };
            let response = match default_available
                .then(|| provider.analyze(DEFAULT_MODEL, &request))
                .transpose()
            {
                Ok(Some(response)) => response,
                Ok(None) => {
                    fallback_served += 1;
                    provider.analyze(FALLBACK_MODEL, &request)?
                }
                Err(_) => {
                    default_available = false;
                    fallback_served += 1;
                    provider.analyze(FALLBACK_MODEL, &request)?
                }
            };
            total_cost += response.cost_usd;
            let observation = validate_observation(&response.body, &[400], 1_000)
                .with_context(|| format!("{} repetition {repetition}", control.display()))?;
            assert!(
                !observation.usable,
                "{} repetition {repetition}",
                control.display()
            );
            assert!(
                observation.ocr_text.is_empty(),
                "{} repetition {repetition}: {:?}",
                control.display(),
                observation.ocr_text
            );
        }
    }
    eprintln!("controls: fallback_served={fallback_served}, cost_usd={total_cost:.6}");
    Ok(())
}
