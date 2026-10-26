//! Thin CLI adapter for explicit host calibration. No readiness or preflight
//! path calls into this module.

use anyhow::{Result, ensure};
use clap::{Args, Subcommand, ValueEnum};
use rcdvlog_core::{
    CalibrationMedia, PROXY_REPRESENTATION_KIND, ProxyCalibrationOptions, ProxyEncoderPolicy,
    ResourceCalibrationReport, SPEECH_REPRESENTATION_KIND, STT_RECIPE_KEY,
    SpeechCalibrationOptions,
};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum CalibrationCommand {
    /// Run one explicit, bounded calibration. Generated media is the default.
    Run(RunArgs),
    /// Inspect cached calibration entries without writes or benchmarks.
    Status(StatusArgs),
    /// Discover local calibration support and bounds without benchmarking.
    Capabilities,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RecipeArg {
    Proxy,
    Speech,
    PipelineSmoke,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum PolicyArg {
    Auto,
    Software,
}

impl From<PolicyArg> for ProxyEncoderPolicy {
    fn from(value: PolicyArg) -> Self {
        match value {
            PolicyArg::Auto => Self::Auto,
            PolicyArg::Software => Self::Software,
        }
    }
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[arg(long, value_enum, default_value_t = RecipeArg::Proxy)]
    recipe: RecipeArg,
    /// Explicitly authorize one representative media file. Without this flag,
    /// calibration generates and removes synthetic media in managed scratch.
    #[arg(long)]
    media: Option<PathBuf>,
    /// Encoder policy candidate set for proxy calibration.
    #[arg(long, value_enum, default_value_t = PolicyArg::Auto)]
    policy: PolicyArg,
    /// Comma-separated internal worker counts to compare (1..=4).
    #[arg(long = "worker-counts", value_delimiter = ',')]
    worker_counts: Vec<usize>,
    /// Comma-separated VAD thread counts to compare (1..=64).
    #[arg(long = "vad-threads", value_delimiter = ',')]
    vad_threads: Vec<usize>,
    /// Fixed decode setting for VAD calibration (4); decode tuning uses explicit transcription overrides.
    #[arg(long = "decode-threads", value_delimiter = ',')]
    decode_threads: Vec<usize>,
    #[arg(long, default_value_t = 1)]
    trials: u32,
    #[arg(long, default_value_t = rcdvlog_core::DEFAULT_GENERATED_CALIBRATION_SECONDS)]
    generated_duration_seconds: u32,
    /// Repeat calibration even when an exact completed cache entry exists.
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    #[arg(long, value_enum)]
    recipe: Option<RecipeArg>,
}

pub fn command_name(command: &CalibrationCommand) -> &'static str {
    match command {
        CalibrationCommand::Run(_) => "calibrate.run",
        CalibrationCommand::Status(_) => "calibrate.status",
        CalibrationCommand::Capabilities => "calibrate.capabilities",
    }
}

pub fn is_read_only(command: &CalibrationCommand) -> bool {
    matches!(
        command,
        CalibrationCommand::Status(_) | CalibrationCommand::Capabilities
    )
}

fn media(args: &RunArgs) -> CalibrationMedia {
    match &args.media {
        Some(path) => CalibrationMedia::Representative(path.clone()),
        None => CalibrationMedia::Generated,
    }
}

pub fn execute(catalog: &rcdvlog_core::Catalog, command: &CalibrationCommand) -> Result<Value> {
    match command {
        CalibrationCommand::Status(args) => {
            let (recipe_key, representation_kind) = match args.recipe {
                Some(RecipeArg::Proxy) => (
                    Some(rcdvlog_core::PROXY_SOFTWARE_RECIPE_KEY),
                    Some(PROXY_REPRESENTATION_KIND),
                ),
                Some(RecipeArg::Speech) => (Some(STT_RECIPE_KEY), Some(SPEECH_REPRESENTATION_KIND)),
                Some(RecipeArg::PipelineSmoke) => {
                    let executable = super::agent_commands::executable()?;
                    let identity =
                        rcdvlog_core::PipelineSmokeIdentity::from_executable(&executable)?;
                    return Ok(serde_json::to_value(
                        catalog.pipeline_smoke_status(identity)?,
                    )?);
                }
                None => (None, None),
            };
            Ok(serde_json::to_value(catalog.resource_calibration_status(
                recipe_key,
                representation_kind,
            )?)?)
        }
        CalibrationCommand::Capabilities => {
            let models = catalog.model_status()?;
            let ffmpeg = rcdvlog_core::resolve_tool_path("ffmpeg");
            let ffprobe = rcdvlog_core::resolve_tool_path("ffprobe");
            Ok(serde_json::json!({
                "schemaVersion": rcdvlog_core::RESOURCE_CALIBRATION_SCHEMA_VERSION,
                "recipeVersion": rcdvlog_core::RESOURCE_CALIBRATION_RECIPE_VERSION,
                "representations": [
                    rcdvlog_core::PROXY_REPRESENTATION_KIND,
                    rcdvlog_core::SPEECH_REPRESENTATION_KIND,
                    "pipeline_smoke",
                ],
                "generatedMedia": {
                    "available": ffmpeg.is_file() && ffprobe.is_file(),
                    "ffmpeg": ffmpeg.is_file(),
                    "ffprobe": ffprobe.is_file(),
                    "maxDurationSeconds": rcdvlog_core::MAX_GENERATED_CALIBRATION_SECONDS,
                },
                "representativeMedia": {
                    "maxDurationSeconds": rcdvlog_core::MAX_REPRESENTATIVE_MEDIA_DURATION_MS / 1000,
                    "maxBytes": rcdvlog_core::MAX_REPRESENTATIVE_MEDIA_BYTES,
                },
                "bounds": {
                    "maxTrialsPerCandidate": rcdvlog_core::MAX_CALIBRATION_TRIALS,
                    "maxCandidates": rcdvlog_core::MAX_CALIBRATION_CANDIDATES,
                    "maxTotalTrials": rcdvlog_core::MAX_CALIBRATION_TOTAL_TRIALS,
                },
                "explicitOverrides": {
                    "proxyEncoder": ["software"],
                    "proxyWorkerCount": [1, rcdvlog_core::MAX_CALIBRATION_WORKERS],
                    "vadThreads": [1, rcdvlog_core::MAX_CALIBRATION_THREADS],
                    "decodeThreads": [1, rcdvlog_core::MAX_CALIBRATION_THREADS],
                    "speechWorkerCount": 1,
                    "calibratedDecodeThreads": false,
                },
                "models": models,
                "localHeavy": {
                    "slots": 1,
                    "scope": "same-host same-user filesystem lease; process-local LIFO; not a distributed cluster lock",
                },
                "forceRecalibrate": true,
            }))
        }
        CalibrationCommand::Run(args) => {
            if matches!(args.recipe, RecipeArg::PipelineSmoke) {
                ensure!(
                    args.media.is_none(),
                    "pipeline smoke uses generated fixtures only; --media is unsupported"
                );
                ensure!(
                    args.worker_counts.is_empty()
                        && args.vad_threads.is_empty()
                        && args.decode_threads.is_empty(),
                    "pipeline smoke does not accept calibration tuning options"
                );
                let executable = super::agent_commands::executable()?;
                let identity = rcdvlog_core::PipelineSmokeIdentity::from_executable(&executable)?;
                return Ok(serde_json::to_value(
                    catalog.run_pipeline_smoke(identity, args.force)?,
                )?);
            }
            ensure!(
                args.trials >= 1,
                "invalid input: --trials must be at least 1"
            );
            let report: ResourceCalibrationReport = match args.recipe {
                RecipeArg::Proxy => catalog.calibrate_proxy(&ProxyCalibrationOptions {
                    media: media(args),
                    policy: args.policy.into(),
                    worker_counts: if args.worker_counts.is_empty() {
                        vec![1]
                    } else {
                        args.worker_counts.clone()
                    },
                    trials: args.trials,
                    generated_duration_seconds: args.generated_duration_seconds,
                    force: args.force,
                })?,
                RecipeArg::Speech => catalog.calibrate_speech(&SpeechCalibrationOptions {
                    media: media(args),
                    vad_threads: if args.vad_threads.is_empty() {
                        vec![1, 2, 4]
                    } else {
                        args.vad_threads.clone()
                    },
                    decode_threads: if args.decode_threads.is_empty() {
                        vec![4]
                    } else {
                        args.decode_threads.clone()
                    },
                    trials: args.trials,
                    generated_duration_seconds: args.generated_duration_seconds,
                    force: args.force,
                })?,
                RecipeArg::PipelineSmoke => unreachable!("handled above"),
            };
            Ok(serde_json::to_value(report)?)
        }
    }
}
