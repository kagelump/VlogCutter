//! Thin CLI adapter for the explicit native cloud speech backend.
//!
//! Policy construction, local VAD preparation, accounting, provider transport,
//! quality checks and transcript persistence all remain in `rcdvlog-core`. This
//! module only parses explicit user choices and renders the canonical result.

use anyhow::{Result, ensure};
use clap::{Args, Subcommand, ValueEnum};
use rcdvlog_core::{
    Catalog, NativeSttProviderPolicy, ResourceOverrides, RunPolicyRequest, SttDataCollection,
};
use serde_json::{Value, json};

use super::agent_commands::CommandCancellation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum NativeDataCollectionArg {
    Allow,
    Deny,
}

/// Explicit string-valued boolean so the CLI accepts `--zdr true` / `--zdr false`
/// instead of silently treating omission as a provider-policy decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExplicitBoolArg {
    True,
    False,
}

impl ExplicitBoolArg {
    fn value(self) -> bool {
        matches!(self, Self::True)
    }
}

impl NativeDataCollectionArg {
    fn core(self) -> SttDataCollection {
        match self {
            Self::Allow => SttDataCollection::Allow,
            Self::Deny => SttDataCollection::Deny,
        }
    }
}

/// Explicit native policy selection shared by preflight/authorize/pilot.
#[derive(Debug, Clone, Args)]
pub struct NativePolicyArgs {
    #[arg(long)]
    project: String,
    /// Exact project-owned asset ID. Repeat for each asset in the batch scope.
    #[arg(long = "asset", required = true)]
    assets: Vec<String>,
    /// Explicit backend selection. Only the verified native OpenRouter audio
    /// transcription contract is accepted by this command family.
    #[arg(long, required = true, value_parser = ["native-openrouter"])]
    backend: String,
    /// Explicit provider-qualified native audio-transcription provider.
    #[arg(long, value_parser = ["openrouter"])]
    provider: String,
    /// Explicit verified native audio-transcription model.
    #[arg(long)]
    primary_model: String,
    /// Explicit lowercase ISO 639-1 language code for the audio request.
    #[arg(long)]
    language: String,
    /// Explicit zero-data-retention provider routing choice (`true` or `false`).
    #[arg(long, value_enum)]
    zdr: ExplicitBoolArg,
    /// Explicit provider data-collection routing choice.
    #[arg(long, value_enum)]
    data_collection: NativeDataCollectionArg,
    /// Finite spend ceiling for the whole frozen policy.
    #[arg(long)]
    budget_usd: f64,
    /// Explicitly select native output over an existing ordinary local transcript.
    #[arg(long)]
    force: bool,
}

impl NativePolicyArgs {
    fn request(&self) -> Result<RunPolicyRequest> {
        ensure!(
            self.backend == "native-openrouter",
            "invalid input: only --backend native-openrouter is supported"
        );
        Ok(RunPolicyRequest::native_speech_transcription(
            self.project.clone(),
            self.assets.clone(),
            self.provider.clone(),
            self.primary_model.clone(),
            self.budget_usd,
            Some(self.language.clone()),
            NativeSttProviderPolicy::new(self.zdr.value(), self.data_collection.core()),
            self.force,
        ))
    }
}

#[derive(Debug, Subcommand)]
pub enum NativeSpeechCommand {
    /// Describe the exact native provider/model/language/data-policy/budget policy
    /// without credentials, local model access or writes.
    Preflight(NativePolicyArgs),
    /// Persist explicit approval for the exact described policy.
    Authorize(NativePolicyArgs),
    /// Run the existing one-window capability pilot for this exact policy.
    Pilot {
        #[command(flatten)]
        policy: NativePolicyArgs,
        /// Explicitly recover an interrupted/unknown-usage pilot after review.
        #[arg(long)]
        retry_uncertain: bool,
    },
    /// Execute or safely resume the authorized native batch over its frozen scope.
    Run {
        #[arg(long)]
        policy_id: String,
    },
    /// Inspect the frozen policy, pilot and canonical persisted transcript
    /// provenance (actual provider/model, quality, latency, cost, uncertainty).
    Status {
        #[arg(long)]
        project: String,
        #[arg(long)]
        policy_id: Option<String>,
        /// Exact asset to inspect. Repeatable; defaults to the policy scope or all
        /// project assets when no policy id is supplied.
        #[arg(long = "asset")]
        assets: Vec<String>,
    },
}

pub fn command_name(command: &NativeSpeechCommand) -> &'static str {
    match command {
        NativeSpeechCommand::Preflight(_) => "transcribe.native.preflight",
        NativeSpeechCommand::Authorize(_) => "transcribe.native.authorize",
        NativeSpeechCommand::Pilot { .. } => "transcribe.native.pilot",
        NativeSpeechCommand::Run { .. } => "transcribe.native.run",
        NativeSpeechCommand::Status { .. } => "transcribe.native.status",
    }
}

pub fn is_read_only(command: &NativeSpeechCommand) -> bool {
    matches!(
        command,
        NativeSpeechCommand::Preflight(_) | NativeSpeechCommand::Status { .. }
    )
}

pub fn execute(catalog: &Catalog, command: &NativeSpeechCommand) -> Result<Value> {
    match command {
        NativeSpeechCommand::Preflight(args) => Ok(serde_json::to_value(
            catalog.describe_run_policy(&args.request()?)?,
        )?),
        NativeSpeechCommand::Authorize(args) => Ok(serde_json::to_value(
            catalog.authorize_run_policy(&args.request()?)?,
        )?),
        NativeSpeechCommand::Pilot {
            policy,
            retry_uncertain,
        } => Ok(serde_json::to_value(
            catalog.run_run_policy_pilot(&policy.request()?, *retry_uncertain)?,
        )?),
        NativeSpeechCommand::Run { policy_id } => {
            let cancellation = CommandCancellation::arm()?;
            let report = catalog.transcribe_native_speech_authorized(
                policy_id,
                &ResourceOverrides::default(),
                || cancellation.flow().is_break(),
                |progress| {
                    eprintln!(
                        "{}",
                        json!({
                            "event": "transcribe.native.progress",
                            "progress": progress,
                        })
                    );
                    cancellation.flow()
                },
            )?;
            Ok(serde_json::to_value(report)?)
        }
        NativeSpeechCommand::Status {
            project,
            policy_id,
            assets,
        } => {
            let authorization = policy_id
                .as_deref()
                .map(|policy_id| catalog.load_run_authorization(policy_id))
                .transpose()?;
            if let Some(authorization) = &authorization {
                ensure!(
                    authorization.policy.project_id == *project,
                    "native speech policy belongs to a different project"
                );
            }
            let pilot = policy_id
                .as_deref()
                .map(|policy_id| catalog.load_run_pilot(policy_id))
                .transpose()?
                .flatten();
            let selected_assets = if !assets.is_empty() {
                assets.clone()
            } else if let Some(authorization) = &authorization {
                authorization
                    .policy
                    .scope
                    .iter()
                    .map(|scope| scope.asset_id.clone())
                    .collect()
            } else {
                catalog
                    .list_assets(project)?
                    .into_iter()
                    .map(|asset| asset.id)
                    .collect()
            };
            let mut inspected = Vec::new();
            for asset_id in selected_assets {
                ensure!(
                    catalog.get_asset(&asset_id)?.project_id == *project,
                    "invalid input: native speech asset belongs to a different project"
                );
                let transcript = catalog.asset_transcript(&asset_id)?;
                let provenance = catalog.native_transcript_provenance(&asset_id)?;
                if transcript.is_some() || provenance.is_some() {
                    inspected.push(json!({
                        "assetId": asset_id,
                        "transcript": transcript,
                        "nativeProvenance": provenance,
                    }));
                }
            }
            Ok(json!({
                "schemaVersion": "analysis.native-speech-status.v1",
                "projectId": project,
                "policy": authorization.map(|value| value.policy),
                "pilot": pilot,
                "assets": inspected,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    #[derive(Debug, Parser)]
    #[command(name = "test")]
    struct TestCli {
        #[command(subcommand)]
        command: NativeSpeechCommand,
    }

    fn parse(arguments: &[&str]) -> NativeSpeechCommand {
        let mut argv = vec!["rcdvlog"];
        argv.extend_from_slice(arguments);
        TestCli::try_parse_from(argv)
            .expect("native speech command should parse")
            .command
    }

    #[test]
    fn native_commands_require_explicit_provider_model_language_and_data_policy() {
        let command = parse(&[
            "preflight",
            "--project",
            "project",
            "--asset",
            "asset-a",
            "--asset",
            "asset-b",
            "--backend",
            "native-openrouter",
            "--provider",
            "openrouter",
            "--primary-model",
            "openai/whisper-1",
            "--language",
            "en",
            "--zdr",
            "true",
            "--data-collection",
            "deny",
            "--budget-usd",
            "1.25",
        ]);
        let NativeSpeechCommand::Preflight(args) = command else {
            panic!("expected preflight");
        };
        let request = args.request().unwrap();
        assert_eq!(request.provider, "openrouter");
        assert_eq!(request.primary_model, "openai/whisper-1");
        assert_eq!(request.asset_ids, vec!["asset-a", "asset-b"]);
        assert_eq!(request.budget_usd, 1.25);
        assert_eq!(
            request.parameters,
            rcdvlog_core::RunPolicyParameters::NativeSpeechTranscription {
                language: Some("en".into()),
                provider_policy: NativeSttProviderPolicy::new(true, SttDataCollection::Deny),
                force: false,
            }
        );
    }

    #[test]
    fn native_commands_reject_unknown_backend_and_missing_policy_fields() {
        assert!(
            TestCli::try_parse_from([
                "rcdvlog",
                "preflight",
                "--project",
                "project",
                "--asset",
                "asset",
                "--backend",
                "local-whisper",
                "--provider",
                "openrouter",
                "--primary-model",
                "openai/whisper-1",
                "--language",
                "en",
                "--zdr",
                "true",
                "--data-collection",
                "deny",
                "--budget-usd",
                "1.0",
            ])
            .is_err()
        );
        assert!(
            TestCli::try_parse_from(["rcdvlog", "run", "--policy-id", "policy"]).is_ok(),
            "run must require only the stored policy identity"
        );
        assert!(
            TestCli::try_parse_from(["rcdvlog", "run"]).is_err(),
            "run must require --policy-id"
        );
    }

    #[test]
    fn every_native_command_is_in_help_and_status_is_read_only() {
        let mut command = TestCli::command();
        assert!(command.render_help().to_string().contains("preflight"));
        assert!(is_read_only(&parse(&["status", "--project", "project"])));
        assert!(!is_read_only(&parse(&[
            "pilot",
            "--project",
            "project",
            "--asset",
            "asset",
            "--backend",
            "native-openrouter",
            "--provider",
            "openrouter",
            "--primary-model",
            "openai/whisper-1",
            "--language",
            "en",
            "--zdr",
            "false",
            "--data-collection",
            "allow",
            "--budget-usd",
            "1.0",
        ])));
    }
}
