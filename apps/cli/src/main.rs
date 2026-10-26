use anyhow::{Context, Result, anyhow, ensure};
#[cfg(test)]
use clap::CommandFactory;
use clap::{Args, Parser, Subcommand, ValueEnum, error::ErrorKind};
use rcdvlog_core::{
    Asset, AssetPatch, Catalog, MediaStreamInfo, ProxyEncoderPolicy, ProxyRepresentation,
    inspect_catalog, landscape_default_rotation, probe_media_stream,
    probe_proxy_encoder_capabilities,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod agent_commands;
mod analysis_recovery_commands;
mod calibration_commands;
mod chronology_commands;
mod moment_commands;
mod native_speech_commands;
mod preparation_commands;
mod server_commands;

#[derive(Debug, Parser)]
#[command(
    name = "rcdvlog",
    version,
    about = "Machine-readable control surface for RCD VlogCutter"
)]
struct Cli {
    /// Catalog database. Defaults to the desktop app's catalog.
    #[arg(long, global = true, env = "RCDVLOG_CATALOG")]
    catalog: Option<PathBuf>,

    /// Pretty-print JSON output.
    #[arg(long, global = true)]
    pretty: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Inspect the running build, explicit catalog, tools, and speech readiness.
    Doctor {
        #[command(flatten)]
        readiness: agent_commands::ReadinessArgs,
    },
    /// Bind or verify an explicitly selected project-local executable and catalog.
    Workspace {
        #[command(subcommand)]
        command: agent_commands::WorkspaceCommand,
    },
    /// Inspect or explicitly install local models or a selected native backend's VAD.
    Models {
        #[command(subcommand)]
        command: agent_commands::ModelsCommand,
    },
    /// Inspect catalog structure without migrations or writes.
    Catalog {
        #[command(subcommand)]
        command: CatalogCommand,
    },
    /// Discover the catalog, project, and clip last shown by the desktop app.
    Desktop {
        #[command(subcommand)]
        command: DesktopCommand,
    },
    /// Create, list, or summarize projects.
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Inspect or change Smart ingest's project-scoped camera mappings.
    CameraMappings {
        #[command(subcommand)]
        command: CameraMappingsCommand,
    },
    /// Preview or apply a project camera-identity merge.
    Cameras {
        #[command(subcommand)]
        command: CamerasCommand,
    },
    /// Discover, verify, or inspect camera supplied `.LRF` review sidecars.
    CameraProxy {
        #[command(subcommand)]
        command: CameraProxyCommand,
    },
    /// Reference all supported media in a folder.
    Ingest {
        #[arg(long)]
        project: String,
        #[arg(long)]
        folder: PathBuf,
        /// Explicitly assign every newly imported asset to this project camera.
        #[arg(long)]
        camera: Option<String>,
    },
    /// Find originals this project has lost track of. Never imports new media.
    Relink {
        #[arg(long)]
        project: String,
        #[arg(long)]
        folder: PathBuf,
    },
    /// Read or mutate clips.
    Clips {
        #[command(subcommand)]
        command: ClipsCommand,
    },
    /// Apply a fixed camera-clock boundary.
    Clock {
        #[command(subcommand)]
        command: ClockCommand,
    },
    /// Generate laptop review proxies.
    Proxy {
        #[command(subcommand)]
        command: ProxyCommand,
    },
    /// Generate project-local clip thumbnails.
    Thumbnails {
        #[command(subcommand)]
        command: ThumbnailCommand,
    },
    /// Inspect or operate the authenticated companion server.
    Server {
        #[command(flatten)]
        connection: server_commands::ServerConnectionArgs,
        #[command(subcommand)]
        command: server_commands::ServerCommand,
    },
    /// Read the derived chronology: sequences, epochs, phases, contradictions,
    /// and why any one clip landed where it did.
    Chronology {
        #[command(subcommand)]
        command: chronology_commands::ChronologyCommand,
    },
    /// Read and acknowledge the conditions the user needs to know about.
    Diagnostics {
        #[command(subcommand)]
        command: chronology_commands::DiagnosticsCommand,
    },
    /// Transcribe clips locally with VAD gating. Runs offline, no API key.
    Transcribe {
        #[command(subcommand)]
        command: TranscribeCommand,
    },
    /// Analyze clips with bounded cloud vision through OpenRouter.
    Analyze {
        #[command(subcommand)]
        command: AnalyzeCommand,
    },
    /// Discover and review bounded evidence-backed candidate moments.
    Moments {
        #[command(subcommand)]
        command: moment_commands::MomentCommand,
    },
    /// Preflight, run, or inspect bounded preparation evidence.
    Prepare {
        #[command(subcommand)]
        command: preparation_commands::PreparationCommand,
    },
    /// Explicitly benchmark and cache host encoder/VAD resource settings.
    Calibrate {
        #[command(subcommand)]
        command: calibration_commands::CalibrationCommand,
    },
    /// Consolidate or inspect the AI-authored label vocabulary.
    Labels {
        #[command(subcommand)]
        command: LabelsCommand,
    },
}

#[derive(Debug, Subcommand)]
enum TranscribeCommand {
    /// Transcribe one clip, replacing any existing transcript.
    Generate {
        #[arg(long)]
        asset: String,
        /// Explicit VAD thread override; bounded and provenance-bearing.
        #[arg(long)]
        vad_threads: Option<usize>,
        /// Explicit whisper decode thread override; bounded and provenance-bearing.
        #[arg(long)]
        decode_threads: Option<usize>,
    },
    /// Transcribe every clip in the project without a current transcript.
    Missing {
        #[arg(long)]
        project: String,
        /// Explicit VAD thread override; bounded and provenance-bearing.
        #[arg(long)]
        vad_threads: Option<usize>,
        /// Explicit whisper decode thread override; bounded and provenance-bearing.
        #[arg(long)]
        decode_threads: Option<usize>,
    },
    /// Transcribe an explicit, validated selection with one engine lifecycle.
    Batch {
        #[arg(long)]
        project: String,
        /// Exact project-owned asset ID (repeat for each selected asset).
        #[arg(long = "asset", required = true)]
        assets: Vec<String>,
        /// Regenerate current transcripts. Default reuses current fingerprinted work.
        #[arg(long)]
        force: bool,
        /// Explicit VAD thread override; bounded and provenance-bearing.
        #[arg(long)]
        vad_threads: Option<usize>,
        /// Explicit whisper decode thread override; bounded and provenance-bearing.
        #[arg(long)]
        decode_threads: Option<usize>,
    },
    /// Explicitly select cloud transcription and run its policy, pilot and batch.
    Native {
        #[command(subcommand)]
        command: native_speech_commands::NativeSpeechCommand,
    },
    /// Report transcript coverage without transcribing anything.
    Status {
        #[arg(long)]
        project: String,
    },
}

#[derive(Debug, Subcommand)]
enum AnalyzeCommand {
    /// Analyze one clip, replacing its prior derived observation.
    Generate {
        #[arg(long)]
        asset: String,
    },
    /// Describe the exact policy for a proposed observation batch without credentials or writes.
    Preflight(AnalyzePolicyArgs),
    /// Persist explicit approval for the exact described policy.
    Authorize(AnalyzePolicyArgs),
    /// Run one bounded capability/schema/source-time pilot for an authorized policy.
    Pilot(AnalyzePolicyArgs),
    /// Execute a stored policy and matching passed pilot over its exact scope.
    Run {
        #[arg(long)]
        policy_id: String,
    },
    /// Analyze clips without a current observation.
    Missing {
        #[arg(long)]
        project: String,
        /// Re-analyze current clips. The report counts non-empty AI-owned fields before overwrite.
        #[arg(long)]
        force: bool,
        /// Stop claiming new clips once returned provider spend reaches this USD ceiling.
        #[arg(long)]
        budget_usd: Option<f64>,
        /// Primary OpenRouter model for this run.
        #[arg(long)]
        primary_model: Option<String>,
        /// Models permitted for this run (repeat for each model).
        #[arg(long = "allow-model")]
        allowed_models: Vec<String>,
        /// Refuse automatic retry with the alternate model.
        #[arg(long)]
        no_fallback: bool,
        /// Do not attach the exact project label vocabulary to the prompt.
        /// Existing CLI runs otherwise materialize that context explicitly.
        #[arg(long)]
        no_project_vocabulary: bool,
    },
    /// Inspect and recover canonical paid-request accounting.
    Accounting {
        #[command(subcommand)]
        command: analysis_recovery_commands::AnalysisAccountingCommand,
    },
    /// Request a bounded cross-clip chronology suggestion from proxy frames.
    SuggestChronology {
        #[arg(long)]
        project: String,
        /// Selected clip IDs; repeat 1 to 8 times.
        #[arg(long = "asset", required = true)]
        assets: Vec<String>,
        #[arg(long, default_value = "qwen/qwen3.7-flash")]
        primary_model: String,
        #[arg(long = "allow-model")]
        allowed_models: Vec<String>,
        #[arg(long)]
        no_fallback: bool,
        /// Finite spend ceiling for this operation. Required unless
        /// RCDVLOG_VLM_MAX_COST_USD is configured.
        #[arg(long)]
        budget_usd: Option<f64>,
    },
    /// Report analysis coverage without network access or writes.
    Status {
        #[arg(long)]
        project: String,
    },
}

#[derive(Debug, Args, Clone)]
pub struct AnalyzePolicyArgs {
    #[arg(long)]
    project: String,
    /// Exact project-owned asset ID. Repeat for each asset in the proposed scope.
    #[arg(long = "asset", required = true)]
    assets: Vec<String>,
    #[arg(long, value_parser = ["openrouter"])]
    provider: String,
    #[arg(long)]
    primary_model: String,
    /// Models permitted for this run (repeat for each model).
    #[arg(long = "allow-model")]
    allowed_models: Vec<String>,
    /// Refuse automatic retry with the alternate model.
    #[arg(long)]
    no_fallback: bool,
    #[arg(long, value_parser = ["frames", "frames-and-transcript", "audio", "audio-and-transcript", "transcript"])]
    payload: String,
    #[arg(long)]
    budget_usd: f64,
    #[arg(long)]
    force: bool,
    /// Explicitly approve attaching the exact current project label vocabulary.
    #[arg(long)]
    project_vocabulary: bool,
    /// Explicitly recover an interrupted/unknown-usage pilot after review.
    #[arg(long)]
    retry_uncertain: bool,
}

fn analyze_policy_request(args: &AnalyzePolicyArgs) -> Result<rcdvlog_core::RunPolicyRequest> {
    let payload = rcdvlog_core::AnalysisPayload::parse(&args.payload)
        .ok_or_else(|| anyhow!("invalid input: unsupported analysis payload"))?;
    Ok(rcdvlog_core::RunPolicyRequest {
        project_id: args.project.clone(),
        asset_ids: args.assets.clone(),
        provider: args.provider.clone(),
        primary_model: args.primary_model.clone(),
        allowed_models: args.allowed_models.clone(),
        allow_fallback: !args.no_fallback,
        payload,
        include_project_vocabulary: args.project_vocabulary,
        budget_usd: args.budget_usd,
        parameters: rcdvlog_core::RunPolicyParameters::Observation { force: args.force },
    })
}

#[derive(Debug, Subcommand)]
enum LabelsCommand {
    /// Make one text-only model call, then remap AI-authored labels only.
    Consolidate {
        #[arg(long)]
        project: String,
        /// Finite spend ceiling for this operation. Required unless
        /// RCDVLOG_VLM_MAX_COST_USD is configured.
        #[arg(long)]
        budget_usd: Option<f64>,
    },
    /// Read the current consolidated vocabulary and usage counts.
    Vocabulary {
        #[arg(long)]
        project: String,
    },
}

#[derive(Debug, Subcommand)]
enum ProjectCommand {
    Create {
        #[arg(long)]
        name: String,
    },
    List,
    Status {
        #[arg(long)]
        project: String,
    },
    /// Summarize diagnostic risks without probing every source file.
    Diagnose {
        #[arg(long)]
        project: String,
    },
}

#[derive(Debug, Subcommand)]
enum CameraMappingsCommand {
    /// List the durable evidence-to-camera mappings for one project.
    List {
        #[arg(long)]
        project: String,
    },
    /// Point one stored mapping at a different camera identity.
    Rename {
        #[arg(long)]
        project: String,
        #[arg(long)]
        mapping: String,
        #[arg(long)]
        camera: String,
    },
    /// Forget one stored mapping. Clearing an already-absent mapping is safe.
    Clear {
        #[arg(long)]
        project: String,
        #[arg(long)]
        mapping: String,
    },
}

#[derive(Debug, Subcommand)]
enum CamerasCommand {
    /// Preview exact selected asset identity and parsed counter repairs.
    RepairPreview {
        #[arg(long)]
        project: String,
        #[arg(long)]
        camera: String,
        #[arg(long="asset", required=true, num_args=1..)]
        assets: Vec<String>,
    },
    /// Apply an unchanged repair preview JSON file.
    Repair {
        #[arg(long)]
        preview: PathBuf,
    },
    /// Count the clips and cards that would move. Does not mutate the catalog.
    MergePreview {
        #[arg(long)]
        project: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        into: String,
    },
    /// Merge one accidental identity into another existing project camera.
    Merge {
        #[arg(long)]
        project: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        into: String,
    },
}

#[derive(Debug, Subcommand)]
enum CatalogCommand {
    Inspect,
}

#[derive(Debug, Subcommand)]
enum DesktopCommand {
    LastOpened,
}

#[derive(Debug, Subcommand)]
enum ClipsCommand {
    List {
        #[arg(long)]
        project: String,
        #[arg(long)]
        query: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Include clips removed from the labeling queue.
        #[arg(long)]
        include_removed: bool,
    },
    Get {
        #[arg(long)]
        asset: String,
    },
    /// Probe source/cache media and explain the exact render plan without writes.
    Diagnose {
        #[arg(long)]
        asset: String,
    },
    /// Validate source video and persist its usable or quarantined state.
    ValidateVideo {
        #[arg(long)]
        asset: String,
    },
    /// Persist current orientation metadata from the immutable original.
    RefreshOrientation {
        #[arg(long)]
        asset: String,
    },
    Update {
        #[arg(long)]
        asset: String,
        #[arg(long)]
        notes: Option<String>,
        /// Add a free-form label. Repeat to add more than one.
        #[arg(long = "add-label")]
        add_labels: Vec<String>,
        /// Remove a label by case-insensitive match. Repeat as needed.
        #[arg(long = "remove-label")]
        remove_labels: Vec<String>,
        /// Hide or restore the clip in the labeling queue.
        #[arg(long)]
        removed: Option<bool>,
        /// Clockwise display rotation: 0, 90, 180, or 270 degrees.
        #[arg(long)]
        rotation_degrees: Option<u16>,
        /// The `revision` this edit was decided from. The write is refused
        /// with `stale_revision` when the clip has changed since, which is how
        /// an agent avoids reversing a change it never saw. Omit only for a
        /// write that genuinely has no basis to be stale against.
        #[arg(long)]
        expected_revision: Option<u64>,
    },
}

#[derive(Debug, Subcommand)]
enum ClockCommand {
    Apply {
        #[arg(long)]
        project: String,
        #[arg(long)]
        camera: String,
        #[arg(long)]
        start_filename: String,
        #[arg(long, allow_hyphen_values = true)]
        offset_minutes: i64,
        #[arg(long, default_value = "Agent clock correction")]
        label: String,
    },
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
enum ProxyPolicyArg {
    /// Probe and use supported hardware, then safely fall back to software.
    #[default]
    Auto,
    /// Never initialize hardware; deterministic software recipe.
    Software,
}

impl From<ProxyPolicyArg> for ProxyEncoderPolicy {
    fn from(value: ProxyPolicyArg) -> Self {
        match value {
            ProxyPolicyArg::Auto => Self::Auto,
            ProxyPolicyArg::Software => Self::Software,
        }
    }
}

#[derive(Debug, Subcommand)]
enum ProxyCommand {
    Generate {
        #[arg(long)]
        asset: String,
        /// Encoder policy: auto (hardware probe with software fallback) or software.
        #[arg(long, value_enum, default_value_t)]
        policy: ProxyPolicyArg,
    },
    Missing {
        #[arg(long)]
        project: String,
        /// Encoder policy: auto (hardware probe with software fallback) or software.
        #[arg(long, value_enum, default_value_t)]
        policy: ProxyPolicyArg,
        /// Explicit internal proxy worker-count override (1..=4).
        #[arg(long)]
        workers: Option<usize>,
    },
    /// Report missing and obsolete proxy representations without generating them.
    Status {
        #[arg(long, required_unless_present = "project", conflicts_with = "project")]
        asset: Option<String>,
        #[arg(long, required_unless_present = "asset", conflicts_with = "asset")]
        project: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Probe usable software/hardware encoder paths with bounded synthetic media.
    Probe {
        #[arg(long, value_enum, default_value_t)]
        policy: ProxyPolicyArg,
    },
    /// Show durable encoder/recipe/timing/failure/fallback provenance.
    Provenance {
        #[arg(long)]
        asset: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Show encoder- and worker-count-aware throughput observations.
    History,
}

#[derive(Debug, Subcommand)]
enum ThumbnailCommand {
    Generate {
        #[arg(long)]
        asset: String,
        #[arg(long, default_value_t = 1)]
        quality: u8,
    },
    Missing {
        #[arg(long)]
        project: String,
        #[arg(long, default_value_t = 1)]
        quality: u8,
    },
}

#[derive(Debug, Subcommand)]
enum CameraProxyCommand {
    /// Read-only scan of one exact card root; does not register or change files.
    Discover {
        #[arg(long)]
        project: String,
        #[arg(long)]
        card: PathBuf,
    },
    /// Explicitly register a candidate for an asset after full verification.
    Register {
        #[arg(long)]
        asset: String,
        #[arg(long)]
        card: PathBuf,
        #[arg(long)]
        sidecar: PathBuf,
    },
    /// Check the currently registered sidecar and report review readiness.
    Status {
        #[arg(long)]
        asset: String,
    },
    /// Sample a verified camera sidecar with source-time and representation identity.
    FramePack {
        #[arg(long)]
        asset: String,
    },
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopContext {
    last_catalog_path: Option<PathBuf>,
    last_project_id: Option<String>,
    last_asset_id: Option<String>,
    last_context_updated_at: Option<String>,
    paired_server_node_id: Option<String>,
    paired_server_endpoint: Option<String>,
    paired_server_label: Option<String>,
    paired_at: Option<String>,
}

fn desktop_settings_path() -> Result<PathBuf> {
    let data_dir =
        dirs::data_dir().ok_or_else(|| anyhow!("cannot resolve application data directory"))?;
    Ok(data_dir.join("com.rcd.vlogcutter").join("settings.json"))
}

fn desktop_context() -> Result<DesktopContext> {
    let path = desktop_settings_path()?;
    if !path.is_file() {
        return Ok(DesktopContext::default());
    }
    let value: Value = serde_json::from_slice(
        &std::fs::read(&path)
            .with_context(|| format!("read desktop settings {}", path.display()))?,
    )
    .with_context(|| format!("parse desktop settings {}", path.display()))?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

fn default_catalog_path() -> Result<PathBuf> {
    let data_dir =
        dirs::data_dir().ok_or_else(|| anyhow!("cannot resolve application data directory"))?;
    Ok(data_dir.join("com.rcd.vlogcutter").join("catalog.sqlite3"))
}

fn selected_catalog(cli: &Cli) -> Result<PathBuf> {
    if let Some(path) = cli.catalog.clone() {
        return Ok(path);
    }
    if let Some(path) = desktop_context()?.last_catalog_path {
        return Ok(path);
    }
    default_catalog_path()
}

fn command_name(command: &Commands) -> &'static str {
    match command {
        Commands::Doctor { .. } => "doctor",
        Commands::Workspace { command } => agent_commands::command_name(command),
        Commands::Models { command } => agent_commands::models_command_name(command),
        Commands::Catalog { .. } => "catalog.inspect",
        Commands::Desktop { .. } => "desktop.last-opened",
        Commands::Project { command } => match command {
            ProjectCommand::Create { .. } => "project.create",
            ProjectCommand::List => "project.list",
            ProjectCommand::Status { .. } => "project.status",
            ProjectCommand::Diagnose { .. } => "project.diagnose",
        },
        Commands::CameraMappings { command } => match command {
            CameraMappingsCommand::List { .. } => "camera-mappings.list",
            CameraMappingsCommand::Rename { .. } => "camera-mappings.rename",
            CameraMappingsCommand::Clear { .. } => "camera-mappings.clear",
        },
        Commands::Cameras { command } => match command {
            CamerasCommand::RepairPreview { .. } => "cameras.repair-preview",
            CamerasCommand::Repair { .. } => "cameras.repair",
            CamerasCommand::MergePreview { .. } => "cameras.merge-preview",
            CamerasCommand::Merge { .. } => "cameras.merge",
        },
        Commands::CameraProxy { command } => match command {
            CameraProxyCommand::Discover { .. } => "camera-proxy.discover",
            CameraProxyCommand::Register { .. } => "camera-proxy.register",
            CameraProxyCommand::Status { .. } => "camera-proxy.status",
            CameraProxyCommand::FramePack { .. } => "camera-proxy.frame-pack",
        },
        Commands::Ingest { .. } => "ingest",
        Commands::Relink { .. } => "relink",
        Commands::Clips { command } => match command {
            ClipsCommand::List { .. } => "clips.list",
            ClipsCommand::Get { .. } => "clips.get",
            ClipsCommand::Diagnose { .. } => "clips.diagnose",
            ClipsCommand::ValidateVideo { .. } => "clips.validate-video",
            ClipsCommand::RefreshOrientation { .. } => "clips.refresh-orientation",
            ClipsCommand::Update { .. } => "clips.update",
        },
        Commands::Clock { .. } => "clock.apply",
        Commands::Proxy { command } => match command {
            ProxyCommand::Generate { .. } => "proxy.generate",
            ProxyCommand::Missing { .. } => "proxy.missing",
            ProxyCommand::Status { .. } => "proxy.status",
            ProxyCommand::Probe { .. } => "proxy.probe",
            ProxyCommand::Provenance { .. } => "proxy.provenance",
            ProxyCommand::History => "proxy.history",
        },
        Commands::Transcribe { command } => match command {
            TranscribeCommand::Generate { .. } => "transcribe.generate",
            TranscribeCommand::Missing { .. } => "transcribe.missing",
            TranscribeCommand::Batch { .. } => "transcribe.batch",
            TranscribeCommand::Native { command } => native_speech_commands::command_name(command),
            TranscribeCommand::Status { .. } => "transcribe.status",
        },
        Commands::Analyze { command } => match command {
            AnalyzeCommand::Generate { .. } => "analyze.generate",
            AnalyzeCommand::Preflight(_) => "analyze.preflight",
            AnalyzeCommand::Authorize(_) => "analyze.authorize",
            AnalyzeCommand::Pilot(_) => "analyze.pilot",
            AnalyzeCommand::Run { .. } => "analyze.run",
            AnalyzeCommand::Accounting { command } => {
                analysis_recovery_commands::command_name(command)
            }
            AnalyzeCommand::Missing { .. } => "analyze.missing",
            AnalyzeCommand::SuggestChronology { .. } => "analyze.suggest-chronology",
            AnalyzeCommand::Status { .. } => "analyze.status",
        },
        Commands::Moments { command } => moment_commands::command_name(command),
        Commands::Prepare { command } => preparation_commands::command_name(command),
        Commands::Calibrate { command } => calibration_commands::command_name(command),
        Commands::Labels { command } => match command {
            LabelsCommand::Consolidate { .. } => "labels.consolidate",
            LabelsCommand::Vocabulary { .. } => "labels.vocabulary",
        },
        Commands::Thumbnails { command } => match command {
            ThumbnailCommand::Generate { .. } => "thumbnails.generate",
            ThumbnailCommand::Missing { .. } => "thumbnails.missing",
        },
        Commands::Chronology { command } => chronology_commands::chronology_command_name(command),
        Commands::Diagnostics { command } => chronology_commands::diagnostics_command_name(command),
        Commands::Server { command, .. } => server_commands::command_name(command),
    }
}

fn execute(cli: &Cli) -> Result<Value> {
    // Agent readiness never consults the desktop breadcrumb or opens a catalog
    // writable. Dispatch before legacy catalog selection.
    if let Commands::Doctor { readiness } = &cli.command {
        return agent_commands::doctor(cli.catalog.as_deref(), readiness);
    }
    if let Commands::Workspace { command } = &cli.command {
        return agent_commands::workspace(cli.catalog.as_deref(), command);
    }
    if let Commands::Models { command } = &cli.command {
        return agent_commands::models(cli.catalog.as_deref(), command);
    }
    if let Commands::Prepare { command } = &cli.command {
        let path = cli.catalog.as_deref().ok_or_else(|| {
            anyhow!("invalid input: prepare commands require --catalog or RCDVLOG_CATALOG")
        })?;
        ensure!(
            path.is_file(),
            "catalog not found: prepare commands require an existing file at {}",
            path.display()
        );
        let catalog = if preparation_commands::is_read_only(command) {
            Catalog::open_read_only_existing(path)?
        } else {
            Catalog::open(path)?
        };
        return preparation_commands::execute(&catalog, command);
    }
    if let Commands::Calibrate { command } = &cli.command {
        let path = cli.catalog.as_deref().ok_or_else(|| {
            anyhow!("invalid input: calibrate commands require --catalog or RCDVLOG_CATALOG")
        })?;
        ensure!(
            path.is_file(),
            "catalog not found: calibrate commands require an existing file at {}",
            path.display()
        );
        let catalog = if calibration_commands::is_read_only(command) {
            Catalog::open_read_only_existing(path)?
        } else {
            Catalog::open(path)?
        };
        return calibration_commands::execute(&catalog, command);
    }
    // Capability probing is a host diagnostic and intentionally does not need
    // an existing catalog or create one as a side effect.
    if let Commands::Proxy {
        command: ProxyCommand::Probe { policy },
    } = &cli.command
    {
        return Ok(serde_json::to_value(probe_proxy_encoder_capabilities(
            (*policy).into(),
        ))?);
    }
    let catalog_path = selected_catalog(cli)?;
    if let Commands::Server {
        connection,
        command,
    } = &cli.command
    {
        return server_commands::execute(connection, command, &catalog_path);
    }
    if matches!(cli.command, Commands::Desktop { .. }) {
        let context = desktop_context()?;
        return Ok(json!({
            "settingsPath": desktop_settings_path()?,
            "catalogPath": context.last_catalog_path,
            "projectId": context.last_project_id,
            "assetId": context.last_asset_id,
            "updatedAt": context.last_context_updated_at,
            "serverPairing": {
                "paired": context.paired_server_node_id.is_some(),
                "nodeId": context.paired_server_node_id,
                "endpoint": context.paired_server_endpoint,
                "label": context.paired_server_label,
                "pairedAt": context.paired_at,
            },
        }));
    }
    if matches!(cli.command, Commands::Catalog { .. }) {
        return Ok(serde_json::to_value(inspect_catalog(&catalog_path)?)?);
    }

    let read_only = match &cli.command {
        Commands::Transcribe {
            command: TranscribeCommand::Native { command },
        } => native_speech_commands::is_read_only(command),
        Commands::Project {
            command: ProjectCommand::Diagnose { .. },
        }
        | Commands::Clips {
            command: ClipsCommand::Diagnose { .. },
        }
        | Commands::Proxy {
            command:
                ProxyCommand::Status { .. } | ProxyCommand::Provenance { .. } | ProxyCommand::History,
        }
        | Commands::Transcribe {
            command: TranscribeCommand::Status { .. },
        }
        | Commands::Analyze {
            command: AnalyzeCommand::Status { .. } | AnalyzeCommand::Preflight(_),
        }
        | Commands::Labels {
            command: LabelsCommand::Vocabulary { .. },
        }
        | Commands::CameraMappings {
            command: CameraMappingsCommand::List { .. },
        }
        | Commands::Cameras {
            command: CamerasCommand::MergePreview { .. },
        }
        | Commands::Cameras {
            command: CamerasCommand::RepairPreview { .. },
        }
        | Commands::CameraProxy {
            command:
                CameraProxyCommand::Discover { .. }
                | CameraProxyCommand::Status { .. }
                | CameraProxyCommand::FramePack { .. },
        } => true,
        Commands::Analyze {
            command: AnalyzeCommand::Accounting { command },
        } => analysis_recovery_commands::is_read_only(command),
        Commands::Moments { command } => moment_commands::is_read_only(command),
        Commands::Chronology { command } => chronology_commands::chronology_is_read_only(command),
        Commands::Diagnostics { command } => chronology_commands::diagnostics_is_read_only(command),
        _ => false,
    };
    let catalog = if read_only {
        Catalog::open_read_only_existing(&catalog_path)?
    } else {
        Catalog::open(&catalog_path)?
    };
    match &cli.command {
        Commands::Doctor { .. } | Commands::Workspace { .. } | Commands::Models { .. } => {
            unreachable!()
        }
        Commands::Catalog { .. } | Commands::Desktop { .. } => unreachable!(),
        Commands::Chronology { command } => {
            chronology_commands::execute_chronology(&catalog, command)
        }
        Commands::Moments { command } => moment_commands::execute(&catalog, command),
        Commands::Prepare { .. } | Commands::Calibrate { .. } => unreachable!(),
        Commands::Diagnostics { command } => {
            chronology_commands::execute_diagnostics(&catalog, command)
        }
        Commands::Project { command } => match command {
            ProjectCommand::Create { name } => {
                Ok(serde_json::to_value(catalog.create_project(name)?)?)
            }
            ProjectCommand::List => Ok(serde_json::to_value(catalog.list_projects()?)?),
            ProjectCommand::Status { project } => project_status(&catalog, project),
            ProjectCommand::Diagnose { project } => project_diagnosis(&catalog, project),
        },
        Commands::CameraMappings { command } => match command {
            CameraMappingsCommand::List { project } => Ok(serde_json::to_value(
                catalog.list_camera_mappings(project)?,
            )?),
            CameraMappingsCommand::Rename {
                project,
                mapping,
                camera,
            } => Ok(serde_json::to_value(
                catalog.rename_camera_mapping(project, mapping, camera)?,
            )?),
            CameraMappingsCommand::Clear { project, mapping } => {
                catalog.clear_camera_mapping(project, mapping)?;
                Ok(json!({
                    "projectId": project,
                    "mappingId": mapping,
                    "cleared": true,
                }))
            }
        },
        Commands::CameraProxy { command } => match command {
            CameraProxyCommand::Discover { project, card } => Ok(serde_json::to_value(catalog.discover_camera_proxy_sidecars(project, card)?)?),
            CameraProxyCommand::Register { asset, card, sidecar } => Ok(serde_json::to_value(catalog.register_camera_proxy_sidecar(asset, card, sidecar)?)?),
            CameraProxyCommand::Status { asset } => Ok(serde_json::to_value(catalog.camera_proxy_sidecar(asset)?)?),
            CameraProxyCommand::FramePack { asset } => Ok(serde_json::to_value(catalog.camera_proxy_frame_pack(asset)?)?),
        },
        Commands::Cameras { command } => match command {
            CamerasCommand::RepairPreview { project, camera, assets } => Ok(serde_json::to_value(
                catalog.preview_camera_repair(project, camera, assets)?,
            )?),
            CamerasCommand::Repair { preview } => {
                let value: Value = serde_json::from_slice(&std::fs::read(preview)?)?;
                let preview: rcdvlog_core::CameraRepairPreview = serde_json::from_value(
                    value.get("data").cloned().unwrap_or(value),
                )?;
                Ok(json!({"repaired":catalog.apply_camera_repair(&preview)?,"projectId":preview.project_id,"camera":preview.camera}))
            }
            CamerasCommand::MergePreview {
                project,
                from,
                into,
            } => Ok(serde_json::to_value(
                catalog.preview_camera_merge(project, from, into)?,
            )?),
            CamerasCommand::Merge {
                project,
                from,
                into,
            } => Ok(serde_json::to_value(
                catalog.merge_camera(project, from, into)?,
            )?),
        },
        Commands::Ingest { project, folder, camera } => Ok(serde_json::to_value(
            match camera {
                Some(camera) => catalog.ingest_folder_with_camera(project, folder, camera, |completed, total, path| {
                    if completed == total || completed % 25 == 0 {
                        eprintln!("{}", json!({"event":"ingest.progress", "completed":completed, "total":total, "path":path}));
                    }
                })?,
                None => catalog.ingest_folder_with_progress(project, folder, |completed, total, path| {
                if completed == total || completed % 25 == 0 {
                    eprintln!(
                        "{}",
                        json!({
                            "event": "ingest.progress",
                            "completed": completed,
                            "total": total,
                            "path": path,
                        })
                    );
                }
                })?,
            },
        )?),
        Commands::Relink { project, folder } => Ok(serde_json::to_value(catalog.relink_folder(
            project,
            folder,
            |completed, total, path| {
                if completed == total || completed % 25 == 0 {
                    eprintln!(
                        "{}",
                        json!({
                            "event": "relink.progress",
                            "completed": completed,
                            "total": total,
                            "path": path,
                        })
                    );
                }
            },
        )?)?),
        Commands::Clips { command } => match command {
            ClipsCommand::List {
                project,
                query,
                limit,
                offset,
                include_removed,
            } => {
                let all = catalog.list_assets(project)?;
                let filtered: Vec<_> = all
                    .into_iter()
                    .filter(|asset| {
                        (*include_removed || !asset.removed)
                            && query.as_ref().is_none_or(|query| {
                                let needle = query.to_lowercase();
                                asset.filename.to_lowercase().contains(&needle)
                                    || asset.camera.to_lowercase().contains(&needle)
                                    || asset.notes.to_lowercase().contains(&needle)
                                    || asset
                                        .labels
                                        .iter()
                                        .any(|label| label.to_lowercase().contains(&needle))
                            })
                    })
                    .collect();
                let total = filtered.len();
                let items: Vec<_> = filtered.into_iter().skip(*offset).take(*limit).collect();
                Ok(json!({ "total": total, "offset": offset, "limit": limit, "items": items }))
            }
            ClipsCommand::Get { asset } => Ok(serde_json::to_value(catalog.get_asset(asset)?)?),
            ClipsCommand::Diagnose { asset } => clip_diagnosis(&catalog, asset),
            ClipsCommand::ValidateVideo { asset } => {
                Ok(serde_json::to_value(catalog.validate_asset_video(asset)?)?)
            }
            ClipsCommand::RefreshOrientation { asset } => Ok(serde_json::to_value(
                catalog.refresh_asset_orientation(asset)?,
            )?),
            ClipsCommand::Update {
                asset,
                notes,
                add_labels,
                remove_labels,
                removed,
                rotation_degrees,
                expected_revision,
            } => {
                let current = catalog.get_asset(asset)?;
                let mut labels = current.labels;
                labels.retain(|label| {
                    !remove_labels
                        .iter()
                        .any(|removed| removed.trim().eq_ignore_ascii_case(label))
                });
                labels.extend(add_labels.iter().cloned());
                let patch = AssetPatch {
                    notes: notes.clone().unwrap_or(current.notes),
                    labels,
                    removed: removed.unwrap_or(current.removed),
                    rotation_degrees: rotation_degrees.unwrap_or(current.rotation_degrees),
                    expected_revision: *expected_revision,
                };
                Ok(serde_json::to_value(catalog.update_asset(asset, &patch)?)?)
            }
        },
        Commands::Clock { command } => match command {
            ClockCommand::Apply {
                project,
                camera,
                start_filename,
                offset_minutes,
                label,
            } => Ok(serde_json::to_value(catalog.apply_clock_event(
                project,
                camera,
                start_filename,
                offset_minutes * 60,
                label,
            )?)?),
        },
        Commands::Proxy { command } => match command {
            ProxyCommand::Generate { asset, policy } => Ok(serde_json::to_value(
                catalog.generate_proxy_with_policy(asset, (*policy).into())?,
            )?),
            ProxyCommand::Missing { project, policy, workers } => Ok(serde_json::to_value(
                catalog.generate_missing_proxies_with_resource_overrides_and_progress(
                    project,
                    (*policy).into(),
                    &rcdvlog_core::ResourceOverrides {
                        worker_count: *workers,
                        ..rcdvlog_core::ResourceOverrides::default()
                    },
                    |_| ControlFlow::Continue(()),
                )?,
            )?),
            ProxyCommand::Status {
                asset,
                project,
                limit,
            } => proxy_status(&catalog, asset.as_deref(), project.as_deref(), *limit),
            ProxyCommand::Probe { .. } => unreachable!(),
            ProxyCommand::Provenance { asset, limit } => Ok(serde_json::to_value(
                catalog.proxy_generation_provenance(asset, *limit)?,
            )?),
            ProxyCommand::History => {
                Ok(serde_json::to_value(catalog.proxy_rate_histories()?)?)
            }
        },
        Commands::Transcribe { command } => match command {
            TranscribeCommand::Generate { asset, vad_threads, decode_threads } => {
                Ok(serde_json::to_value(catalog.transcribe_asset_with_resource_overrides(
                    asset,
                    &rcdvlog_core::ResourceOverrides {
                        vad_threads: *vad_threads,
                        decode_threads: *decode_threads,
                        ..rcdvlog_core::ResourceOverrides::default()
                    },
                )?)?)
            }
            TranscribeCommand::Missing { project, vad_threads, decode_threads } => Ok(serde_json::to_value(
                catalog.transcribe_missing_with_resource_overrides_and_progress(project, &rcdvlog_core::ResourceOverrides {
                    vad_threads: *vad_threads,
                    decode_threads: *decode_threads,
                    ..rcdvlog_core::ResourceOverrides::default()
                }, |progress| {
                    if progress.estimate.confidence != "inspecting_audio" {
                        eprintln!(
                            "{}",
                            json!({
                                "event": "transcribe.progress",
                                "outcome": progress.outcome,
                                "estimate": progress.estimate,
                            })
                        );
                    }
                    ControlFlow::Continue(())
                })?,
            )?),
            TranscribeCommand::Batch {
                project,
                assets,
                force,
                vad_threads,
                decode_threads,
            } => {
                let cancellation = agent_commands::CommandCancellation::arm()?;
                Ok(serde_json::to_value(catalog.transcribe_selected_with_resource_overrides(
                    project,
                    assets,
                    &rcdvlog_core::TranscriptionBatchOptions { force: *force },
                    &rcdvlog_core::ResourceOverrides {
                        vad_threads: *vad_threads,
                        decode_threads: *decode_threads,
                        ..rcdvlog_core::ResourceOverrides::default()
                    },
                    |progress| {
                        // Planning callbacks keep cancellation responsive before
                        // model readiness is known. Keep prerequisite errors as
                        // one JSON value instead of prepending progress events.
                        if progress.phase != "probing_audio" {
                            eprintln!(
                                "{}",
                                json!({"event": "transcribe.batch.progress", "progress": progress})
                            );
                        }
                        cancellation.flow()
                    },
                )?)?)
            }
            TranscribeCommand::Native { command } => {
                native_speech_commands::execute(&catalog, command)
            }
            TranscribeCommand::Status { project } => {
                Ok(serde_json::to_value(catalog.transcript_coverage(project)?)?)
            }
        },
        Commands::Analyze { command } => match command {
            AnalyzeCommand::Generate { asset } => {
                Ok(serde_json::to_value(catalog.analyze_asset(asset)?)?)
            }
            AnalyzeCommand::Preflight(args) => {
                let request = analyze_policy_request(args)?;
                Ok(serde_json::to_value(catalog.describe_run_policy(&request)?)?)
            }
            AnalyzeCommand::Authorize(args) => {
                let request = analyze_policy_request(args)?;
                Ok(serde_json::to_value(catalog.authorize_run_policy(&request)?)?)
            }
            AnalyzeCommand::Pilot(args) => {
                let request = analyze_policy_request(args)?;
                Ok(serde_json::to_value(catalog.run_run_policy_pilot(
                    &request,
                    args.retry_uncertain,
                )?)?)
            }
            AnalyzeCommand::Run { policy_id } => {
                catalog.authorized_policy_for_bulk(
                    policy_id,
                    rcdvlog_core::AnalysisRunKind::Observation,
                )?;
                ensure!(
                    rcdvlog_core::api_credential_present(),
                    "OpenRouter credentials are not available; no analysis request was made"
                );
                let provider = rcdvlog_core::OpenRouterProvider::new(
                    rcdvlog_core::resolve_api_key()?,
                )?;
                Ok(serde_json::to_value(catalog.analyze_authorized_with_provider(
                    policy_id,
                    &provider,
                    |progress| {
                        eprintln!(
                            "{}",
                            json!({
                                "event": "analyze.run.progress",
                                "outcome": progress.outcome,
                                "estimate": progress.estimate,
                            })
                        );
                        ControlFlow::Continue(())
                    },
                )?)?)
            }
            AnalyzeCommand::Accounting { command } => {
                analysis_recovery_commands::execute(&catalog, command)
            }
            AnalyzeCommand::Missing { project, force, budget_usd, primary_model, allowed_models, no_fallback, no_project_vocabulary } => Ok(serde_json::to_value(
                catalog.analyze_missing_with_options(project, &rcdvlog_core::AnalysisRunOptions {
                    force: *force,
                    budget_usd: *budget_usd,
                    primary_model: primary_model.clone(),
                    allowed_models: allowed_models.clone(),
                    allow_fallback: Some(!no_fallback),
                    include_project_vocabulary: !no_project_vocabulary,
                    ..Default::default()
                }, |progress| {
                    eprintln!(
                        "{}",
                        json!({
                            "event": "analyze.progress",
                            "outcome": progress.outcome,
                            "estimate": progress.estimate,
                        })
                    );
                    ControlFlow::Continue(())
                })?,
            )?),
            AnalyzeCommand::SuggestChronology { project, assets, primary_model, allowed_models, no_fallback, budget_usd } => {
                let allowed = if allowed_models.is_empty() && !no_fallback {
                    vec![primary_model.clone(), if primary_model == rcdvlog_core::DEFAULT_MODEL { rcdvlog_core::FALLBACK_MODEL.to_string() } else { rcdvlog_core::DEFAULT_MODEL.to_string() }]
                } else if allowed_models.is_empty() {
                    vec![primary_model.clone()]
                } else {
                    allowed_models.clone()
                };
                let chain = rcdvlog_core::ProviderChain::with_policy(primary_model, &allowed, !no_fallback)?;
                let provider = rcdvlog_core::OpenRouterProvider::new(rcdvlog_core::resolve_api_key()?)?;
                let result = match budget_usd {
                    Some(budget_usd) => catalog.suggest_chronology_with_provider_and_budget(
                        project, assets, &provider, &chain, *budget_usd,
                    )?,
                    None => catalog.suggest_chronology_with_provider(
                        project, assets, &provider, &chain,
                    )?,
                };
                Ok(serde_json::to_value(result)?)
            }
            AnalyzeCommand::Status { project } => {
                Ok(serde_json::to_value(catalog.analysis_coverage(project)?)?)
            }
        },
        Commands::Labels { command } => match command {
            LabelsCommand::Consolidate { project, budget_usd } => {
                let report = match budget_usd {
                    Some(budget_usd) => catalog.consolidate_labels_with_budget(project, *budget_usd)?,
                    None => catalog.consolidate_labels(project)?,
                };
                Ok(serde_json::to_value(report)?)
            }
            LabelsCommand::Vocabulary { project } => {
                Ok(serde_json::to_value(catalog.label_vocabulary(project)?)?)
            }
        },
        Commands::Thumbnails { command } => match command {
            ThumbnailCommand::Generate { asset, quality } => Ok(serde_json::to_value(
                catalog.generate_thumbnail(asset, *quality)?,
            )?),
            ThumbnailCommand::Missing { project, quality } => Ok(serde_json::to_value(
                catalog.generate_missing_thumbnails(project, *quality)?,
            )?),
        },
        Commands::Server { .. } => unreachable!(),
    }
}

fn proxy_version(asset: &Asset) -> &'static str {
    if !asset.proxy_available {
        return "missing";
    }
    match asset.proxy_representation {
        Some(ProxyRepresentation::RawV3) => "current_raw_v3",
        Some(ProxyRepresentation::MetadataTaggedRawV2) => "metadata_tagged_raw_v2",
        Some(ProxyRepresentation::MetadataBakedV1) => "legacy_metadata_baked_v1",
        None => "missing",
    }
}

fn thumbnail_version(path: Option<&str>) -> &'static str {
    match path {
        None => "missing",
        Some(path) if !Path::new(path).is_file() => "missing",
        Some(path) if path.ends_with(".v2.jpg") => "current_raw_v2",
        Some(_) => "legacy_metadata_baked_v1",
    }
}

fn probe_value(path: &str) -> (Option<MediaStreamInfo>, Value) {
    match probe_media_stream(path) {
        Ok(probe) => {
            let value = serde_json::to_value(&probe).unwrap_or(Value::Null);
            (Some(probe), value)
        }
        Err(error) => (None, json!({ "path": path, "error": format!("{error:#}") })),
    }
}

fn rotation_after(rotation: u16, addition: i32) -> u16 {
    ((rotation as i32 + addition).rem_euclid(360)) as u16
}

fn clip_diagnosis(catalog: &Catalog, asset_id: &str) -> Result<Value> {
    let asset = catalog.get_asset(asset_id)?;
    let revision = catalog.asset_revision_diagnostics(asset_id)?;
    let video_validation = catalog.asset_video_validation(asset_id)?;
    let (original_probe, original) = probe_value(&asset.path);
    let no_video_stream = original["error"]
        .as_str()
        .is_some_and(|error| error.contains("no video stream"));
    let original_bytes_reachable = video_validation.bytes_reachable;
    let incomplete_video_stream = original_bytes_reachable
        && original_probe.is_none()
        && original["error"]
            .as_str()
            .is_some_and(|error| error.contains("video stream"));
    let proxy_version = proxy_version(&asset);
    let proxy_probe = asset
        .proxy_path
        .as_deref()
        .filter(|path| Path::new(path).is_file())
        .map(probe_value);
    let thumbnail_version = thumbnail_version(asset.thumbnail_path.as_deref());

    let live_metadata_rotation = original_probe
        .as_ref()
        .map(|probe| probe.metadata_rotation_degrees);
    let live_default_rotation = original_probe.as_ref().map(|probe| {
        landscape_default_rotation(probe.width, probe.height, probe.metadata_rotation_degrees)
    });
    let has_override = asset.rotation_degrees != asset.default_rotation_degrees;
    let effective_rotation = if asset.orientation_probed || has_override {
        asset.rotation_degrees
    } else {
        live_default_rotation.unwrap_or(asset.rotation_degrees)
    };
    let effective_metadata = if asset.orientation_probed {
        asset.metadata_rotation_degrees
    } else {
        live_metadata_rotation.unwrap_or(asset.metadata_rotation_degrees)
    };

    let (selected_kind, selected_version, selected_probe) = if let Some((probe, _)) = &proxy_probe {
        ("proxy", proxy_version, probe.as_ref())
    } else {
        ("original", "source_with_metadata", original_probe.as_ref())
    };
    let proxy_live_rotation = proxy_probe
        .as_ref()
        .and_then(|(probe, _)| probe.as_ref())
        .map(|probe| probe.metadata_rotation_degrees);
    let current_proxy_contract_valid =
        proxy_version == "current_raw_v3" && proxy_live_rotation == Some(0);
    let representation_is_raw =
        selected_version == "current_raw_v3" && current_proxy_contract_valid;
    let baked_rotation = if representation_is_raw {
        0
    } else if selected_kind == "proxy"
        && matches!(
            selected_version,
            "current_raw_v3" | "metadata_tagged_raw_v2"
        )
    {
        proxy_live_rotation.unwrap_or(effective_metadata)
    } else {
        effective_metadata
    };
    let applied_rotation = rotation_after(effective_rotation, -(baked_rotation as i32));
    let (final_width, final_height) = selected_probe
        .map(|probe| {
            let (width, height) = (probe.display_width, probe.display_height);
            if matches!(applied_rotation, 90 | 270) {
                (height, width)
            } else {
                (width, height)
            }
        })
        .unwrap_or((0, 0));
    let final_orientation = if final_width > final_height {
        "landscape"
    } else if final_width < final_height {
        "portrait"
    } else {
        "unknown"
    };

    let mut warnings = Vec::new();
    if original_bytes_reachable && original_probe.is_none() {
        let issue = if no_video_stream {
            "no video stream"
        } else {
            "unusable video stream or probe failure"
        };
        warnings.push(format!(
            "original file bytes are reachable, but ffprobe found {issue}; this legacy catalog row is not usable video and should not be counted as ready"
        ));
    }
    if !asset.orientation_probed {
        warnings.push(
            "catalog orientation is unprobed; stored 0° means unknown, not confirmed".to_string(),
        );
    }
    if let Some(rotation) = live_metadata_rotation
        && rotation != asset.metadata_rotation_degrees
    {
        warnings.push(format!(
            "stored metadata rotation is {}° but the original currently reports {}°",
            asset.metadata_rotation_degrees, rotation
        ));
    }
    if proxy_version == "metadata_tagged_raw_v2" {
        warnings.push(
            "selected proxy is obsolete v2 media whose display rotation can be applied twice"
                .to_string(),
        );
    } else if proxy_version == "legacy_metadata_baked_v1" {
        warnings.push(
            "selected proxy is legacy v1 media with camera rotation baked into its pixels"
                .to_string(),
        );
    }
    if proxy_version == "current_raw_v3" && !current_proxy_contract_valid {
        warnings.push(
            "current v3 proxy failed its raw-orientation contract; regenerate it from the original"
                .to_string(),
        );
    }
    if final_orientation != "landscape" {
        warnings.push(format!(
            "computed rendered result is {final_orientation}, not landscape"
        ));
    }

    let mut recommended_actions = Vec::new();
    if video_validation.status != rcdvlog_core::AssetVideoValidationStatus::Usable {
        recommended_actions.push(format!(
            "rcdvlog --catalog {} clips validate-video --asset {}",
            catalog.database_path().display(),
            asset.id
        ));
    }
    if original_bytes_reachable && original_probe.is_none() {
        recommended_actions.push("verify the referenced file is the intended video and contains decodable video frames, valid dimensions, a frame rate, a codec, and a duration; keep it out of video review and chronology until those checks pass".to_string());
    }
    if !asset.orientation_probed {
        recommended_actions.push(format!(
            "rcdvlog --catalog {} clips refresh-orientation --asset {}",
            catalog.database_path().display(),
            asset.id
        ));
    }
    if proxy_version != "missing"
        && (proxy_version != "current_raw_v3" || !current_proxy_contract_valid)
        && asset.original_available
    {
        recommended_actions.push(format!(
            "rcdvlog --catalog {} proxy generate --asset {}",
            catalog.database_path().display(),
            asset.id
        ));
    }

    Ok(json!({
        "catalogPath": catalog.database_path(),
        "asset": asset,
        "revision": revision,
        "storedOrientation": {
            "probed": asset.orientation_probed,
            "metadataRotationDegrees": asset.metadata_rotation_degrees,
            "defaultRotationDegrees": asset.default_rotation_degrees,
            "rotationDegrees": asset.rotation_degrees,
            "hasUserOverride": has_override,
        },
        "liveOrientation": {
            "metadataRotationDegrees": live_metadata_rotation,
            "landscapeDefaultRotationDegrees": live_default_rotation,
            "effectiveRotationDegrees": effective_rotation,
        },
        "representations": {
            "original": original,
            "originalReadiness": {
                "bytesReachable": original_bytes_reachable,
                "usableVideo": original_probe.is_some(),
                "diagnosis": if no_video_stream { "no_video_stream" } else if incomplete_video_stream { "incomplete_video_stream" } else if original_probe.is_some() { "video_stream_available" } else { "probe_failed" },
            },
            "catalogVideoValidation": video_validation,
            "proxy": {
                "path": asset.proxy_path,
                "version": proxy_version,
                "rawOrientationContractValid": current_proxy_contract_valid,
                "probe": proxy_probe.as_ref().map(|(_, value)| value),
            },
            "thumbnail": {
                "path": asset.thumbnail_path,
                "version": thumbnail_version,
            },
        },
        "renderPlan": {
            "selectedRepresentation": selected_kind,
            "selectedVersion": selected_version,
            "representationRotationAlreadyAppliedDegrees": baked_rotation,
            "cssRotationDegrees": applied_rotation,
            "finalWidth": final_width,
            "finalHeight": final_height,
            "finalOrientation": final_orientation,
        },
        "warnings": warnings,
        "recommendedActions": recommended_actions,
    }))
}

fn proxy_status(
    catalog: &Catalog,
    asset_id: Option<&str>,
    project_id: Option<&str>,
    limit: usize,
) -> Result<Value> {
    let assets = if let Some(asset_id) = asset_id {
        vec![catalog.get_asset(asset_id)?]
    } else {
        catalog.list_assets(project_id.expect("clap requires asset or project"))?
    };
    let total = assets.len();
    let items = assets
        .iter()
        .take(limit)
        .map(|asset| {
            let version = proxy_version(asset);
            let validation = if version == "current_raw_v3" {
                asset
                    .proxy_path
                    .as_deref()
                    .map(|path| match probe_media_stream(path) {
                        Ok(probe) => json!({
                            "valid": probe.metadata_rotation_degrees == 0,
                            "metadataRotationDegrees": probe.metadata_rotation_degrees,
                        }),
                        Err(error) => json!({ "valid": false, "error": format!("{error:#}") }),
                    })
            } else {
                None
            };
            let provenance = catalog
                .proxy_generation_provenance(&asset.id, 1)
                .ok()
                .and_then(|mut entries| entries.pop());
            let registered = if version == "current_raw_v3" {
                catalog
                    .registered_proxy_generation_provenance(&asset.id)
                    .ok()
                    .flatten()
            } else {
                None
            };
            let ready = version == "current_raw_v3"
                && validation
                    .as_ref()
                    .and_then(|value| value["valid"].as_bool())
                    == Some(true);
            json!({
                "assetId": asset.id,
                "filename": asset.filename,
                "path": asset.proxy_path,
                "version": version,
                "recipeKey": registered.as_ref().map(|entry| entry.recipe_key.as_str()),
                "encoder": registered.as_ref().map(|entry| entry.selected_encoder.as_str()),
                "latestProvenance": provenance,
                "registeredProvenance": registered,
                "validation": validation,
                "ready": ready,
                "regenerationRequired": !ready,
                "originalAvailable": asset.original_available,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "total": total,
        "limit": limit,
        "summary": {
            "current": assets.iter().filter(|asset| proxy_version(asset) == "current_raw_v3").count(),
            "obsoleteV2": assets.iter().filter(|asset| proxy_version(asset) == "metadata_tagged_raw_v2").count(),
            "legacy": assets.iter().filter(|asset| proxy_version(asset) == "legacy_metadata_baked_v1").count(),
            "missing": assets.iter().filter(|asset| proxy_version(asset) == "missing").count(),
        },
        "encoderRateHistories": catalog.proxy_rate_histories()?,
        "items": items,
    }))
}

fn project_diagnosis(catalog: &Catalog, project_id: &str) -> Result<Value> {
    let status = project_status(catalog, project_id)?;
    let assets = catalog.list_assets(project_id)?;
    let operations = catalog.operation_diagnostics(project_id)?;
    let context = desktop_context().unwrap_or_default();
    Ok(json!({
        "catalogPath": catalog.database_path(),
        "status": status,
        "operations": operations,
        "orientation": {
            "unprobed": assets.iter().filter(|asset| !asset.orientation_probed).count(),
            "overridden": assets.iter().filter(|asset| asset.rotation_degrees != asset.default_rotation_degrees).count(),
        },
        "proxies": {
            "current": assets.iter().filter(|asset| proxy_version(asset) == "current_raw_v3").count(),
            "obsoleteV2": assets.iter().filter(|asset| proxy_version(asset) == "metadata_tagged_raw_v2").count(),
            "legacy": assets.iter().filter(|asset| proxy_version(asset) == "legacy_metadata_baked_v1").count(),
            "missing": assets.iter().filter(|asset| proxy_version(asset) == "missing").count(),
        },
        "desktopContext": {
            "matchesProject": context.last_project_id.as_deref() == Some(project_id),
            "selectedAssetId": context.last_asset_id,
            "updatedAt": context.last_context_updated_at,
        },
        "next": "Run clips diagnose --asset <id> for live ffprobe evidence and the exact render plan.",
    }))
}

fn project_status(catalog: &Catalog, project_id: &str) -> Result<Value> {
    let project = catalog
        .list_projects()?
        .into_iter()
        .find(|project| project.id == project_id)
        .ok_or_else(|| anyhow!("project not found: {project_id}"))?;
    let assets = catalog.list_assets(project_id)?;
    let validations = assets
        .iter()
        .map(|asset| catalog.asset_video_validation(&asset.id))
        .collect::<Result<Vec<_>>>()?;
    let validation_by_asset = validations
        .into_iter()
        .map(|validation| (validation.asset_id.clone(), validation))
        .collect::<std::collections::HashMap<_, _>>();
    let active_assets = assets
        .iter()
        .filter(|asset| !asset.removed)
        .collect::<Vec<_>>();
    let usable_assets = active_assets
        .iter()
        .copied()
        .filter(|asset| {
            validation_by_asset
                .get(&asset.id)
                .is_some_and(|validation| {
                    validation.status == rcdvlog_core::AssetVideoValidationStatus::Usable
                })
        })
        .collect::<Vec<_>>();
    let duration_seconds: f64 = usable_assets
        .iter()
        .map(|asset| asset.duration_seconds)
        .sum();
    let catalog_duration_seconds: f64 = active_assets
        .iter()
        .map(|asset| asset.duration_seconds)
        .sum();
    let usable_asset_ids = usable_assets
        .iter()
        .map(|asset| asset.id.clone())
        .collect::<std::collections::HashSet<_>>();
    let validation_counts = json!({
        "usableVideo": usable_assets.len(),
        "unverifiedVideo": active_assets
            .iter()
            .filter(|asset| {
                validation_by_asset.get(&asset.id).is_some_and(|validation| {
                    validation.status == rcdvlog_core::AssetVideoValidationStatus::Unverified
                })
            })
            .count(),
        "quarantinedVideo": active_assets
            .iter()
            .filter(|asset| {
                validation_by_asset.get(&asset.id).is_some_and(|validation| {
                    validation.status == rcdvlog_core::AssetVideoValidationStatus::Quarantined
                })
            })
            .count(),
        "reachableOriginals": active_assets
            .iter()
            .filter(|asset| {
                validation_by_asset
                    .get(&asset.id)
                    .is_some_and(|validation| validation.bytes_reachable)
            })
            .count(),
    });
    let analysis_readiness = catalog.analysis_readiness(project_id)?;
    Ok(json!({
        "project": project,
        "analysisReadiness": analysis_readiness,
        "media": {
            "assets": assets.len(),
            "durationSeconds": duration_seconds,
            "catalogMetadataDurationSeconds": catalog_duration_seconds,
            "videoValidation": validation_counts,
            "originalsOffline": active_assets
                .iter()
                .filter(|asset| !Path::new(&asset.path).is_file())
                .count(),
            "proxiesReady": active_assets
                .iter()
                .filter(|asset| {
                    usable_asset_ids.contains(&asset.id)
                        && proxy_version(asset) == "current_raw_v3"
                })
                .count(),
            "proxiesObsoleteV2": active_assets
                .iter()
                .filter(|asset| {
                    usable_asset_ids.contains(&asset.id)
                        && proxy_version(asset) == "metadata_tagged_raw_v2"
                })
                .count(),
            "proxiesLegacy": active_assets
                .iter()
                .filter(|asset| {
                    usable_asset_ids.contains(&asset.id)
                        && proxy_version(asset) == "legacy_metadata_baked_v1"
                })
                .count(),
            "labeled": assets.iter().filter(|asset| !asset.labels.is_empty()).count(),
            "removed": assets.iter().filter(|asset| asset.removed).count(),
            "thumbnailsReady": active_assets
                .iter()
                .filter(|asset| {
                    usable_asset_ids.contains(&asset.id)
                        && asset.thumbnail_quality > 0
                        && asset
                            .thumbnail_path
                            .as_deref()
                            .is_some_and(|path| Path::new(path).exists())
                })
                .count(),
        }
    }))
}

fn error_code(message: &str) -> &'static str {
    let message = message.to_lowercase();
    if message.contains("chronology.invalid_reference")
        && (message.contains("stale") || message.contains("changed"))
        || message.contains("preview is stale")
    {
        "chronology.stale_preview"
    } else if message.contains("chronology.invalid_reference") {
        "chronology.invalid_reference"
    } else if message.contains("chronology.hard_contradiction") {
        "chronology.hard_contradiction"
    } else if message.contains("chronology.invalid_timezone") {
        "chronology.invalid_timezone"
    } else if message.contains("chronology.invalid_interval") {
        "chronology.invalid_interval"
    } else if message.contains("catalog schema") || message.contains("migration history") {
        "schema_incompatible"
    } else if message.contains(rcdvlog_core::STALE_ASSET_REVISION) {
        // Distinct from every other failure because the caller's correct
        // response is distinct: re-read, re-decide, retry.
        "stale_revision"
    } else if message.contains("analysis request not found")
        || message.contains("prior analysis request not found")
    {
        "not_found"
    } else if message.contains("not ready")
        || message.contains("not authorized")
        || message.contains("does not match")
        || message.contains("targeted retry")
        || message.contains("analysis request")
        || message.contains("receipt evidence file")
        || message.contains("analysis_accounting:admission_denied")
        || message.contains("analysis_accounting:pre_upload_failed")
        || message.contains("analysis_accounting:cancelled")
        || message.contains("analysis inspection")
        || message.contains("provider receipt")
        || message.contains("local no-upload")
        || message.contains("reconciliation project")
        || message.contains("retry authorization project")
        || message.contains("unsupported")
        || message.contains("budget")
    {
        "invalid_input"
    } else if message.contains("not found") {
        "not_found"
    } else if message.contains("offline") {
        "media_offline"
    } else if [
        "association_mismatch:",
        "unsupported_rate_mapping:",
        "unsupported_timing:",
        "content_mismatch:",
        "mismatch:",
        "unsupported:",
        "truncated_or_unreadable:",
        "changed_during_verification:",
    ]
    .iter()
    .any(|reason| message.contains(reason))
    {
        "invalid_input"
    } else if message.contains("sidecar is unavailable") {
        "media_offline"
    } else if message.contains("empty") || message.contains("invalid") {
        "invalid_input"
    } else if message.contains("ffmpeg")
        || message.contains("ffprobe")
        || message.contains("exiftool")
    {
        "media_tool_failed"
    } else {
        "internal_error"
    }
}

fn write_json(value: &Value, pretty: bool, stderr: bool) {
    let output = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
    .expect("serialize CLI output");
    if stderr {
        eprintln!("{output}");
    } else {
        println!("{output}");
    }
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            error.print().ok();
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            write_json(
                &json!({
                    "ok": false,
                    "command": "parse",
                    "error": { "code": "invalid_arguments", "message": error.to_string() }
                }),
                false,
                true,
            );
            return ExitCode::from(2);
        }
    };
    let name = command_name(&cli.command);
    match execute(&cli).with_context(|| format!("{name} failed")) {
        Ok(data) => {
            write_json(
                &json!({ "ok": true, "command": name, "data": data }),
                cli.pretty,
                false,
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            let message = format!("{error:#}");
            let remote = error.downcast_ref::<server_commands::RemoteCommandError>();
            let job_agent = error.downcast_ref::<server_commands::JobAgentError>();
            let chronology_evidence_error =
                error.downcast_ref::<rcdvlog_core::ChronologyEvidenceError>();
            let media_tool_failure = error
                .downcast_ref::<rcdvlog_core::MediaToolFailure>()
                .is_some()
                || error
                    .downcast_ref::<rcdvlog_core::ProxyTranscodeFailure>()
                    .is_some();
            let readiness_error = error.downcast_ref::<rcdvlog_core::AgentReadinessError>();
            let transcription_selection_error =
                error.downcast_ref::<rcdvlog_core::TranscriptionSelectionError>();
            let transcription_readiness_error =
                error.downcast_ref::<rcdvlog_core::TranscriptionReadinessError>();
            let prerequisite_error = error.downcast_ref::<rcdvlog_core::MediaPrerequisiteError>();
            let representation_busy = error.downcast_ref::<rcdvlog_core::RepresentationBusy>();
            let code = if let Some(job_agent) = job_agent {
                job_agent.code
            } else if let Some(readiness) = readiness_error {
                readiness.code
            } else if let Some(selection) = transcription_selection_error {
                selection.code
            } else if let Some(readiness) = transcription_readiness_error {
                readiness.code
            } else if representation_busy.is_some() {
                rcdvlog_core::RepresentationBusy::CODE
            } else if prerequisite_error.is_some() {
                "media_prerequisite_failed"
            } else if let Some(remote) = remote {
                remote.code.as_str()
            } else if let Some(chronology_error) = chronology_evidence_error {
                chronology_error.code()
            } else if media_tool_failure {
                "media_tool_failed"
            } else {
                error_code(&message)
            };
            write_json(
                &json!({
                    "ok": false,
                    "command": name,
                    "error": {
                        "code": code,
                        "message": message,
                        "requestId": remote.and_then(|error| error.request_id.as_deref()),
                        "details": job_agent.map(|error| error.details.clone()).or_else(|| readiness_error.and_then(|error| error.details.as_deref()).cloned()),
                    }
                }),
                cli.pretty,
                true,
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{Connection, params};
    use tempfile::tempdir;

    fn parse(arguments: &[&str]) -> Cli {
        Cli::try_parse_from(arguments).unwrap()
    }

    fn insert_cli_asset(
        catalog_path: &Path,
        project_id: &str,
        id: &str,
        filename: &str,
        notes: &str,
        labels_json: &str,
        removed: bool,
    ) {
        Connection::open(catalog_path)
            .unwrap()
            .execute(
                r#"
                INSERT INTO assets (
                    id, project_id, fingerprint, path, filename, size_bytes,
                    duration_seconds, frame_rate, width, height, codec, camera,
                    captured_at_raw, best_time, notes, labels_json, removed,
                    created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, 100, 10, 30, 1920, 1080,
                          'h264', 'DJI', '2026-07-18T00:00:00Z',
                          '2026-07-18T00:00:00.000Z', ?6, ?7, ?8,
                          '2026-07-18T00:00:00.000Z')
                "#,
                params![
                    id,
                    project_id,
                    format!("fingerprint-{id}"),
                    format!("/missing/{filename}"),
                    filename,
                    notes,
                    labels_json,
                    removed,
                ],
            )
            .unwrap();
    }

    #[test]
    fn creates_lists_and_summarizes_a_project() {
        let directory = tempdir().unwrap();
        let catalog = directory.path().join("catalog.sqlite3");
        let catalog_arg = catalog.to_string_lossy();
        let create = parse(&[
            "rcdvlog",
            "--catalog",
            &catalog_arg,
            "project",
            "create",
            "--name",
            "CLI test",
        ]);
        let created = execute(&create).unwrap();
        let project_id = created["id"].as_str().unwrap();

        let list = parse(&["rcdvlog", "--catalog", &catalog_arg, "project", "list"]);
        assert_eq!(execute(&list).unwrap().as_array().unwrap().len(), 1);

        let status = parse(&[
            "rcdvlog",
            "--catalog",
            &catalog_arg,
            "project",
            "status",
            "--project",
            project_id,
        ]);
        assert_eq!(execute(&status).unwrap()["media"]["assets"], 0);
    }

    #[test]
    fn clap_contract_has_all_agent_commands() {
        let help = Cli::command().render_long_help().to_string();
        for command in [
            "doctor",
            "project",
            "camera-mappings",
            "ingest",
            "clips",
            "clock",
            "proxy",
            "thumbnails",
            "transcribe",
            "analyze",
            "models",
            "moments",
            "labels",
            "prepare",
            "calibrate",
        ] {
            assert!(help.contains(command));
        }
    }

    #[test]
    fn transcribe_subcommands_parse() {
        assert!(
            Cli::try_parse_from(["rcdvlog", "transcribe", "missing", "--project", "p1"]).is_ok()
        );
        assert!(
            Cli::try_parse_from(["rcdvlog", "transcribe", "generate", "--asset", "a1"]).is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "rcdvlog",
                "transcribe",
                "batch",
                "--project",
                "p1",
                "--asset",
                "a1",
                "--asset",
                "a2",
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from(["rcdvlog", "transcribe", "status", "--project", "p1"]).is_ok()
        );
    }

    #[test]
    fn calibration_and_resource_override_flags_parse() {
        let status = parse(&["rcdvlog", "calibrate", "status", "--recipe", "speech"]);
        assert_eq!(command_name(&status.command), "calibrate.status");
        let capabilities = parse(&["rcdvlog", "calibrate", "capabilities"]);
        assert_eq!(
            command_name(&capabilities.command),
            "calibrate.capabilities"
        );
        let run = parse(&[
            "rcdvlog",
            "calibrate",
            "run",
            "--recipe",
            "proxy",
            "--worker-counts",
            "1,2",
            "--trials",
            "2",
            "--force",
        ]);
        assert_eq!(command_name(&run.command), "calibrate.run");
        let transcribe = parse(&[
            "rcdvlog",
            "transcribe",
            "batch",
            "--project",
            "p1",
            "--asset",
            "a1",
            "--vad-threads",
            "2",
            "--decode-threads",
            "4",
        ]);
        assert_eq!(command_name(&transcribe.command), "transcribe.batch");
        let proxy = parse(&[
            "rcdvlog",
            "proxy",
            "missing",
            "--project",
            "p1",
            "--workers",
            "2",
        ]);
        assert_eq!(command_name(&proxy.command), "proxy.missing");
    }

    #[test]
    fn transcribe_generate_requires_an_asset() {
        assert!(Cli::try_parse_from(["rcdvlog", "transcribe", "generate"]).is_err());
    }

    #[test]
    fn transcribe_status_requires_a_project() {
        assert!(Cli::try_parse_from(["rcdvlog", "transcribe", "status"]).is_err());
    }

    #[test]
    fn speech_model_backend_flags_preserve_default_and_parse_explicit_native_selection() {
        let local = parse(&["rcdvlog", "models", "status"]);
        assert_eq!(command_name(&local.command), "models.status");
        let local_install = parse(&["rcdvlog", "models", "install"]);
        assert_eq!(command_name(&local_install.command), "models.install");

        let native = parse(&[
            "rcdvlog",
            "models",
            "status",
            "--backend",
            "native-openrouter",
            "--provider",
            "openrouter",
            "--model",
            "openai/whisper-1",
        ]);
        assert_eq!(command_name(&native.command), "models.status");
        let native_install = parse(&[
            "rcdvlog",
            "models",
            "install",
            "--backend",
            "native-openrouter",
            "--provider",
            "openrouter",
            "--model",
            "openai/whisper-1",
        ]);
        assert_eq!(command_name(&native_install.command), "models.install");

        let mut command = Cli::command();
        let models = command.find_subcommand_mut("models").unwrap();
        let status = models.find_subcommand_mut("status").unwrap();
        let help = status.render_long_help().to_string();
        assert!(help.contains("local-whisper"));
        assert!(help.contains("native-openrouter"));
        assert!(help.contains("--provider"));
        assert!(help.contains("--model"));
    }

    #[test]
    fn analysis_and_label_subcommands_parse() {
        assert!(Cli::try_parse_from(["rcdvlog", "analyze", "missing", "--project", "p1"]).is_ok());
        assert!(
            Cli::try_parse_from([
                "rcdvlog",
                "analyze",
                "missing",
                "--project",
                "p1",
                "--force",
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["rcdvlog", "analyze", "generate", "--asset", "a1"]).is_ok());
        assert!(Cli::try_parse_from(["rcdvlog", "analyze", "status", "--project", "p1"]).is_ok());
        assert!(
            Cli::try_parse_from([
                "rcdvlog",
                "analyze",
                "missing",
                "--project",
                "p1",
                "--budget-usd",
                "1.5",
                "--primary-model",
                "google/gemini-2.5-flash-lite",
                "--allow-model",
                "google/gemini-2.5-flash-lite",
                "--no-fallback",
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "rcdvlog",
                "analyze",
                "suggest-chronology",
                "--project",
                "p1",
                "--asset",
                "a1",
                "--asset",
                "a2",
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from(["rcdvlog", "labels", "consolidate", "--project", "p1",]).is_ok()
        );
        assert!(
            Cli::try_parse_from(["rcdvlog", "labels", "vocabulary", "--project", "p1",]).is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "rcdvlog",
                "analyze",
                "accounting",
                "inspect",
                "--project",
                "p1",
                "--operation",
                "pilot",
                "--limit",
                "20",
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "rcdvlog",
                "analyze",
                "accounting",
                "reconcile",
                "--project",
                "p1",
                "--request",
                "r1",
                "--local-no-upload",
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "rcdvlog",
                "analyze",
                "accounting",
                "reconcile",
                "--project",
                "p1",
                "--request",
                "r1",
                "--local-no-upload",
                "--unknown-outcome",
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "rcdvlog",
                "analyze",
                "accounting",
                "retry-authorize",
                "--project",
                "p1",
                "--request",
                "r1",
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "rcdvlog",
                "analyze",
                "accounting",
                "targeted-retry-dispatch",
                "--project",
                "p1",
                "--request",
                "r1",
                "--expected-next-attempt-epoch",
                "1",
                "--payload-file",
                "/tmp/payload.json",
            ])
            .is_ok()
        );
    }

    #[test]
    fn transcribe_status_reports_coverage_on_an_empty_project() {
        let directory = tempdir().unwrap();
        let catalog_path = directory.path().join("catalog.sqlite3");
        let catalog = Catalog::open(&catalog_path).unwrap();
        let project = catalog.create_project("Coverage test").unwrap();
        let catalog_arg = catalog_path.to_string_lossy().to_string();

        let status = parse(&[
            "rcdvlog",
            "--catalog",
            &catalog_arg,
            "transcribe",
            "status",
            "--project",
            &project.id,
        ]);
        let value = execute(&status).unwrap();
        assert_eq!(value["total"], 0);
        assert_eq!(value["missing"], 0);
        assert_eq!(value["noAudio"], 0);
        assert_eq!(value["noAcceptedSpeech"], 0);
        assert_eq!(value["legacyNoSpeechUnverified"], 0);
        assert_eq!(value["recipeKey"], rcdvlog_core::STT_RECIPE_KEY);
    }

    #[test]
    fn clip_update_is_a_patch_for_labels_and_removed_state() {
        let directory = tempdir().unwrap();
        let catalog_path = directory.path().join("catalog.sqlite3");
        let catalog = Catalog::open(&catalog_path).unwrap();
        let project = catalog.create_project("Patch test").unwrap();
        insert_cli_asset(
            &catalog_path,
            &project.id,
            "asset_a",
            "DJI_A.MP4",
            "original note",
            "[\"arrival\"]",
            false,
        );
        let catalog_arg = catalog_path.to_string_lossy();

        let notes_only = parse(&[
            "rcdvlog",
            "--catalog",
            &catalog_arg,
            "clips",
            "update",
            "--asset",
            "asset_a",
            "--notes",
            "changed note",
        ]);
        let updated = execute(&notes_only).unwrap();
        assert_eq!(updated["notes"], "changed note");
        assert_eq!(updated["labels"], json!(["arrival"]));
        assert_eq!(updated["removed"], false);

        let label_update = parse(&[
            "rcdvlog",
            "--catalog",
            &catalog_arg,
            "clips",
            "update",
            "--asset",
            "asset_a",
            "--remove-label",
            "ARRIVAL",
            "--add-label",
            "train",
            "--add-label",
            "Train",
            "--removed",
            "true",
        ]);
        let updated = execute(&label_update).unwrap();
        assert_eq!(updated["labels"], json!(["train"]));
        assert_eq!(updated["removed"], true);
        assert_eq!(updated["notes"], "changed note");
    }

    #[test]
    fn clip_list_filters_before_paginating() {
        let directory = tempdir().unwrap();
        let catalog_path = directory.path().join("catalog.sqlite3");
        let catalog = Catalog::open(&catalog_path).unwrap();
        let project = catalog.create_project("Filter test").unwrap();
        for (id, filename, notes) in [
            ("asset_a", "DJI_A.MP4", "arrival exterior"),
            ("asset_b", "DJI_B.MP4", "arrival interior"),
            ("asset_c", "DJI_C.MP4", "departure"),
        ] {
            insert_cli_asset(&catalog_path, &project.id, id, filename, notes, "[]", false);
        }
        let catalog_arg = catalog_path.to_string_lossy();
        let command = parse(&[
            "rcdvlog",
            "--catalog",
            &catalog_arg,
            "clips",
            "list",
            "--project",
            &project.id,
            "--query",
            "ARRIVAL",
            "--limit",
            "1",
            "--offset",
            "1",
        ]);
        let result = execute(&command).unwrap();
        assert_eq!(result["total"], 2);
        assert_eq!(result["items"].as_array().unwrap().len(), 1);
        assert_eq!(result["items"][0]["filename"], "DJI_B.MP4");
    }

    #[test]
    fn proxy_probe_is_machine_readable_and_does_not_create_a_catalog() {
        let directory = tempdir().unwrap();
        let catalog_path = directory.path().join("not-created-probe.sqlite3");
        let catalog_arg = catalog_path.to_string_lossy();
        let probe = parse(&[
            "rcdvlog",
            "--catalog",
            &catalog_arg,
            "proxy",
            "probe",
            "--policy",
            "software",
        ]);
        assert_eq!(command_name(&probe.command), "proxy.probe");
        let result = execute(&probe).unwrap();
        assert_eq!(result["policy"], "software");
        assert_eq!(result["selectedEncoder"], "software");
        assert!(
            result["hardware"][0]["status"] == "not_requested"
                || result["hardware"][0]["status"] == "unsupported_platform"
        );
        assert!(!catalog_path.exists());
    }

    #[test]
    fn proxy_generate_accepts_explicit_encoder_policy() {
        let generate = parse(&[
            "rcdvlog",
            "--catalog",
            "/synthetic/catalog.sqlite3",
            "proxy",
            "generate",
            "--asset",
            "asset_synthetic",
            "--policy",
            "software",
        ]);
        assert_eq!(command_name(&generate.command), "proxy.generate");
    }

    #[test]
    fn doctor_does_not_create_the_selected_catalog() {
        let directory = tempdir().unwrap();
        let catalog_path = directory.path().join("not-created.sqlite3");
        let catalog_arg = catalog_path.to_string_lossy();
        let doctor = parse(&["rcdvlog", "--catalog", &catalog_arg, "doctor"]);
        let result = execute(&doctor).unwrap();
        assert_eq!(result["catalog"], catalog_arg.as_ref());
        assert!(!catalog_path.exists());
    }

    #[test]
    fn catalog_inspection_reports_pending_schema_without_migrating() {
        let directory = tempdir().unwrap();
        let catalog_path = directory.path().join("legacy.sqlite3");
        let connection = Connection::open(&catalog_path).unwrap();
        connection
            .execute_batch("CREATE TABLE projects (id TEXT); CREATE TABLE assets (id TEXT);")
            .unwrap();
        drop(connection);
        let catalog_arg = catalog_path.to_string_lossy();
        let inspect = parse(&["rcdvlog", "--catalog", &catalog_arg, "catalog", "inspect"]);
        let result = execute(&inspect).unwrap();
        assert_eq!(result["readOnly"], true);
        assert_eq!(result["schemaHistoryPresent"], false);
        assert_eq!(result["currentSchemaVersion"], 0);
        assert_eq!(
            result["latestSupportedSchemaVersion"],
            rcdvlog_core::PROJECT_CATALOG_SCHEMA_VERSION
        );
        assert_eq!(result["schemaStatus"], "legacy_unversioned");
        assert_eq!(
            result["pendingMigrations"].as_array().unwrap().len(),
            rcdvlog_core::PROJECT_CATALOG_SCHEMA_VERSION as usize
        );
        assert!(
            result["pendingAssetColumns"]
                .as_array()
                .unwrap()
                .iter()
                .any(|column| column == "orientation_probed")
        );
        let columns = Connection::open(&catalog_path)
            .unwrap()
            .prepare("PRAGMA table_info(assets)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(!columns.iter().any(|column| column == "orientation_probed"));
        assert_eq!(
            Connection::open(&catalog_path)
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn reports_stable_error_codes_and_requires_explicit_project_arguments() {
        assert_eq!(error_code("asset not found"), "not_found");
        // An agent's response to a stale write is to re-read and retry, which
        // no other failure calls for; the code has to say which one this is.
        assert_eq!(
            error_code("stale asset revision: expected revision 3, catalog is at 5"),
            "stale_revision",
        );
        assert_eq!(error_code("original is offline"), "media_offline");
        assert_eq!(error_code("invalid source range"), "invalid_input");
        assert_eq!(error_code("ffmpeg failed"), "media_tool_failed");
        assert_eq!(
            error_code("catalog schema version 5 is newer than supported version 4"),
            "schema_incompatible"
        );
        assert_eq!(error_code("database locked"), "internal_error");
        assert!(Cli::try_parse_from(["rcdvlog", "clips", "list"]).is_err());
        assert!(Cli::try_parse_from(["rcdvlog", "project", "status"]).is_err());
        assert!(Cli::try_parse_from(["rcdvlog", "camera-mappings", "list"]).is_err());
    }

    #[test]
    fn server_command_families_have_stable_noninteractive_clap_shapes() {
        for suffix in [
            vec!["status"],
            vec!["projects", "--limit", "10"],
            vec!["library", "list"],
            vec!["library", "diagnose", "--media-object", "media_1"],
            vec!["storage", "list"],
            vec!["storage", "diagnose", "--root", "root_1"],
            vec!["scans", "list"],
            vec!["jobs", "diagnose", "--job", "job_1"],
            vec!["sync", "status"],
            vec!["representations", "status", "--representation", "rep_1"],
            vec!["credentials", "list"],
            vec!["backups", "diagnose", "--backup", "backup_1"],
        ] {
            let mut arguments = vec![
                "rcdvlog",
                "server",
                "--endpoint",
                "https://server.example.test",
                "--credential-file",
                "/tmp/credential",
            ];
            arguments.extend(suffix);
            assert!(Cli::try_parse_from(arguments).is_ok());
        }
        assert!(
            Cli::try_parse_from([
                "rcdvlog",
                "server",
                "--endpoint",
                "https://server.example.test",
                "status"
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["rcdvlog", "server", "status"]).is_err());
    }
}
