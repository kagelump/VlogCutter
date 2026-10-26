//! Authorized native speech transcription batch.
//!
//! This is the high-level owner above the existing local VAD preparation,
//! canonical accounting, bounded native transport and durable C9/C10 checkpoint
//! owners. The selected provider/model/language/data policy/source scope/budget
//! all come from one frozen `RunPolicy`; this module does not create a second
//! scheduler, ledger, transcript store or retry framework.

use std::ops::ControlFlow;

use anyhow::{Result, bail, ensure};
use serde::Serialize;

use crate::analysis::accounting::{AccountedNativeSttProvider, AnalysisAccountingAggregate};
use crate::analysis::openrouter_stt::{NativeSttRequest, NativeSttTransport};
use crate::analysis::provider::{ProviderFailure, ProviderFailureCategory};
use crate::analysis::run_policy::{
    AnalysisRunKind, RunPolicy, bulk_request_accounting_context,
    native_serving_model_identity_requires_review, native_stt_request_from_policy,
};
use crate::analysis::store::native_provenance::{
    NativeTranscriptCheckpointWindowInput, NativeTranscriptWindowReuse,
    save_native_transcript_on_transaction,
};
use crate::analysis::store::{
    StoredTranscript, TranscriptOutcomeReason, TranscriptStatus, has_native_transcript_association,
};
use crate::transcription::{
    NativeSpeechLocalOutcome, NativeSpeechPreparation, NativeSpeechPreparationCanceled,
    NativeSpeechPreparer,
};
use crate::{AnalysisRunOutcome, Catalog, ResourceOverrides};

use super::native_pilot::DeferredEnvironmentNativeSttTransport;

pub const NATIVE_SPEECH_BATCH_SCHEMA_VERSION: &str = "analysis.native-speech-batch.v1";
/// Compact per-asset result. Detailed window quality, actual receipt identities
/// and request history remain in the canonical provenance/accounting owners and
/// are exposed by the CLI status command rather than duplicated here.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSpeechAssetOutcome {
    pub asset_id: String,
    pub status: String,
    pub outcome_reason: String,
    pub reused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSpeechBatchFailure {
    pub asset_id: Option<String>,
    pub code: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSpeechBatchProgress {
    pub asset_id: Option<String>,
    pub outcome: Option<NativeSpeechAssetOutcome>,
    pub completed: usize,
    pub total: usize,
}

/// Invocation summary. Detailed asset facts remain in native transcript
/// provenance; this invocation's spending uses the existing accounting DTO.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSpeechBatchReport {
    pub schema_version: String,
    pub status: String,
    pub policy_id: String,
    /// Present only when this invocation created an owned paid run.
    pub run_id: Option<String>,
    pub assets_total: usize,
    pub completed: usize,
    pub reused: usize,
    pub skipped: usize,
    pub cancelled: bool,
    pub budget_exhausted: bool,
    pub accounting: Option<AnalysisAccountingAggregate>,
    pub outcomes: Vec<NativeSpeechAssetOutcome>,
    pub failures: Vec<NativeSpeechBatchFailure>,
}

enum NativeAssetStep {
    Outcome(Box<NativeSpeechAssetOutcome>),
    Skipped,
    Cancelled,
}

impl Catalog {
    /// Production entry point. Credentials are resolved lazily by the existing
    /// deferred native transport only when a genuinely missing window reaches
    /// the accounted pre-dispatch boundary. NoAudio/NoSpeech and fully cached
    /// execution never require an API key.
    pub fn transcribe_native_speech_authorized<C, F>(
        &self,
        policy_id: &str,
        overrides: &ResourceOverrides,
        mut is_cancelled: C,
        on_progress: F,
    ) -> Result<NativeSpeechBatchReport>
    where
        C: FnMut() -> bool,
        F: FnMut(NativeSpeechBatchProgress) -> ControlFlow<()>,
    {
        let transport = DeferredEnvironmentNativeSttTransport::new();
        let mut preparer = NativeSpeechPreparer::new();
        let cancelled = std::cell::Cell::new(false);
        let mut progress = on_progress;
        self.run_native_speech_batch(
            policy_id,
            overrides,
            &transport,
            &mut preparer,
            &mut || cancelled.get() || is_cancelled(),
            &mut |update| {
                if progress(update).is_break() {
                    cancelled.set(true);
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            },
        )
    }

    /// Focused test seam for the exact production orchestration. It has no
    /// production-visible bypass and reuses the canonical SQLite accounting
    /// boundary.
    #[cfg(test)]
    pub(crate) fn transcribe_native_speech_authorized_with_transport_and_preparer<C, F>(
        &self,
        policy_id: &str,
        overrides: &ResourceOverrides,
        transport: &dyn NativeSttTransport,
        preparer: &mut NativeSpeechPreparer,
        is_cancelled: &mut C,
        on_progress: &mut F,
    ) -> Result<NativeSpeechBatchReport>
    where
        C: FnMut() -> bool,
        F: FnMut(NativeSpeechBatchProgress) -> ControlFlow<()>,
    {
        self.run_native_speech_batch(
            policy_id,
            overrides,
            transport,
            preparer,
            is_cancelled,
            on_progress,
        )
    }

    fn run_native_speech_batch<C, F>(
        &self,
        policy_id: &str,
        overrides: &ResourceOverrides,
        transport: &dyn NativeSttTransport,
        preparer: &mut NativeSpeechPreparer,
        is_cancelled: &mut C,
        on_progress: &mut F,
    ) -> Result<NativeSpeechBatchReport>
    where
        C: FnMut() -> bool,
        F: FnMut(NativeSpeechBatchProgress) -> ControlFlow<()>,
    {
        ensure!(
            !policy_id.trim().is_empty(),
            "native speech batch policy id is required"
        );
        let authorization = self.load_run_authorization(policy_id)?;
        ensure!(
            authorization.policy.policy_id == policy_id
                && authorization.policy.kind == AnalysisRunKind::NativeSpeechTranscription
                && authorization.policy.payload == crate::AnalysisPayload::Audio,
            "native speech batch requires an authorized audio-only native speech policy"
        );
        let policy =
            self.describe_run_policy_for_local_preparation(&authorization.policy.request())?;
        ensure!(
            policy.policy_id == policy_id && policy.readiness.ready_for_authorization,
            "native speech batch policy or selected source identity changed before work"
        );
        ensure!(
            policy.kind == AnalysisRunKind::NativeSpeechTranscription,
            "native speech batch selected policy is not native speech transcription"
        );
        let request = native_stt_request_from_policy(&policy)?;

        let mut report = NativeSpeechBatchReport {
            schema_version: NATIVE_SPEECH_BATCH_SCHEMA_VERSION.into(),
            status: "running".into(),
            policy_id: policy.policy_id.clone(),
            run_id: None,
            assets_total: policy.scope.len(),
            completed: 0,
            reused: 0,
            skipped: 0,
            cancelled: false,
            budget_exhausted: false,
            accounting: None,
            outcomes: Vec::new(),
            failures: Vec::new(),
        };
        let mut run_id: Option<String> = None;

        for scope in &policy.scope {
            if is_cancelled() {
                report.cancelled = true;
                break;
            }
            if on_progress(NativeSpeechBatchProgress {
                asset_id: Some(scope.asset_id.clone()),
                outcome: None,
                completed: report.completed,
                total: report.assets_total,
            })
            .is_break()
            {
                report.cancelled = true;
                break;
            }

            let asset_result = self.run_native_asset(
                &policy,
                &request,
                &scope.asset_id,
                overrides,
                transport,
                preparer,
                &mut run_id,
                is_cancelled,
            );
            match asset_result {
                Ok(NativeAssetStep::Outcome(outcome)) => {
                    let outcome = *outcome;
                    report.completed += 1;
                    if outcome.reused {
                        report.reused += 1;
                    }
                    let flow = on_progress(NativeSpeechBatchProgress {
                        asset_id: Some(scope.asset_id.clone()),
                        outcome: Some(outcome.clone()),
                        completed: report.completed,
                        total: report.assets_total,
                    });
                    report.outcomes.push(outcome);
                    if flow.is_break() {
                        report.cancelled = true;
                        break;
                    }
                }
                Ok(NativeAssetStep::Skipped) => report.skipped += 1,
                Ok(NativeAssetStep::Cancelled) => {
                    report.cancelled = true;
                    break;
                }
                Err(error) => {
                    let failure = native_batch_failure(Some(scope.asset_id.clone()), &error);
                    if failure.code == "native_batch_budget_exhausted" {
                        report.budget_exhausted = true;
                    }
                    report.failures.push(failure);
                    break;
                }
            }

            // Stop before starting the next asset if cancellation raced the
            // callback/update. Any typed window already written remains durable.
            if is_cancelled() {
                report.cancelled = true;
                break;
            }
        }

        if let Some(run_id) = run_id.as_deref() {
            let projection = self.analysis_accounting_aggregate_for_run(
                &policy.policy_id,
                AnalysisRunKind::NativeSpeechTranscription.as_str(),
                run_id,
            )?;
            let status = if report.cancelled {
                "cancelled"
            } else if report.failures.is_empty() && !projection.has_unresolved_usage() {
                "completed"
            } else {
                "failed"
            };
            let error = report
                .failures
                .first()
                .map(|failure| format!("{}:{}", failure.code, failure.detail))
                .unwrap_or_default();
            self.finish_analysis_run(
                run_id,
                &AnalysisRunOutcome {
                    status: status.into(),
                    stage_reached: AnalysisRunKind::NativeSpeechTranscription.as_str().into(),
                    clips_transcribed: report.completed,
                    clips_analyzed: report.completed,
                    cost_usd: projection.confirmed_cost_usd,
                    budget_exhausted: report.budget_exhausted,
                    error: error.chars().take(240).collect(),
                    ..Default::default()
                },
            )?;
            report.run_id = Some(run_id.to_string());
            report.accounting = Some(projection);
        }
        report.status = if report.cancelled {
            "cancelled"
        } else if report.failures.is_empty() {
            "completed"
        } else {
            "failed"
        }
        .into();
        Ok(report)
    }

    #[allow(clippy::too_many_arguments)]
    fn run_native_asset<C>(
        &self,
        policy: &RunPolicy,
        request: &NativeSttRequest,
        asset_id: &str,
        overrides: &ResourceOverrides,
        transport: &dyn NativeSttTransport,
        preparer: &mut NativeSpeechPreparer,
        run_id: &mut Option<String>,
        is_cancelled: &mut C,
    ) -> Result<NativeAssetStep>
    where
        C: FnMut() -> bool,
    {
        self.require_current_native_policy(policy)?;
        let asset = self.get_asset(asset_id)?;
        if asset.removed {
            return Ok(NativeAssetStep::Skipped);
        }
        match self.current_transcript_state(asset_id)? {
            crate::analysis::store::TranscriptCurrentState::CurrentNative(_) => {
                let provenance = self
                    .native_transcript_provenance(asset_id)?
                    .ok_or_else(|| {
                        anyhow::anyhow!("current native transcript provenance disappeared")
                    })?;
                ensure!(
                    provenance.recipe_key == policy.recipe_version
                        && provenance.policy.provider == policy.provider
                        && provenance.policy.primary_model == policy.primary_model,
                    "current native transcript recipe/provider/model does not match the selected native policy"
                );
                if provenance_requires_serving_model_review(&provenance, &policy.primary_model) {
                    bail!(
                        "native_stt_serving_model_identity_requires_review: inspect the canonical receipt before reusing this transcript"
                    );
                }
                return Ok(NativeAssetStep::Outcome(Box::new(
                    native_asset_outcome_from_provenance(&provenance, true),
                )));
            }
            crate::analysis::store::TranscriptCurrentState::ProtectedNative { reason } => {
                // Only an exact same-policy incomplete checkpoint may resume.
                // A terminal/foreign/malformed native association is a review or
                // typed recovery exception, never an ordinary cache miss.
                if self.native_transcript_checkpoint(asset_id)?.is_none() {
                    bail!("native transcript requires review/recovery: {reason}");
                }
            }
            crate::analysis::store::TranscriptCurrentState::CurrentLocal(_)
            | crate::analysis::store::TranscriptCurrentState::RetryableLocalFailed(_)
            | crate::analysis::store::TranscriptCurrentState::Missing => {}
        }

        if is_cancelled() {
            return Ok(NativeAssetStep::Cancelled);
        }
        self.require_current_native_policy(policy)?;
        let preparation =
            match preparer.prepare(self, &policy.policy_id, asset_id, overrides, is_cancelled) {
                Ok(preparation) => preparation,
                Err(error)
                    if error
                        .downcast_ref::<NativeSpeechPreparationCanceled>()
                        .is_some() =>
                {
                    return Ok(NativeAssetStep::Cancelled);
                }
                Err(error) => return Err(error.context("prepare selected native speech asset")),
            };

        match preparation.outcome() {
            NativeSpeechLocalOutcome::NoAudio | NativeSpeechLocalOutcome::NoSpeech => {
                let transcript = self.save_native_request_free_outcome(&preparation)?;
                return Ok(NativeAssetStep::Outcome(Box::new(
                    NativeSpeechAssetOutcome {
                        asset_id: transcript.asset_id,
                        status: transcript.status.as_str().into(),
                        outcome_reason: transcript.outcome_reason.as_str().into(),
                        reused: false,
                    },
                )));
            }
            NativeSpeechLocalOutcome::PreparedSpeech => {}
        }
        ensure!(
            preparation.window_count() > 0,
            "prepared native speech has no bounded window"
        );
        preparation.validate_source_current(self)?;
        if is_cancelled() {
            return Ok(NativeAssetStep::Cancelled);
        }
        // Positive speech may create/replace a durable native checkpoint, so
        // prove the exact passed pilot and whole-policy authorization before any
        // such local state changes. This never reads credentials or sends a
        // request; NoAudio/NoSpeech and fully cached assets have already returned.
        let authorized = self.authorized_policy_for_bulk(
            &policy.policy_id,
            AnalysisRunKind::NativeSpeechTranscription,
        )?;
        ensure!(
            authorized.authorization.policy.policy_id == policy.policy_id,
            "native speech batch authorization changed before checkpoint work"
        );

        let existing_checkpoint = self.native_transcript_checkpoint(asset_id)?;
        let checkpoint = if let Some(existing) = existing_checkpoint {
            ensure!(
                existing.plan.windows == preparation.windows(),
                "existing native checkpoint plan no longer matches the live prepared window map"
            );
            self.begin_native_transcript_checkpoint(&preparation)?
        } else {
            ensure!(
                !has_native_transcript_association(&self.connection()?, asset_id)?,
                "native transcript has an unproved association; explicit selection cannot replace it"
            );
            self.begin_native_transcript_checkpoint_for_explicit_selection(&preparation)?
        };

        // Reuse the exact C7 paid pilot sample before deciding whether another
        // upload is needed. The checkpoint owner re-proves the request/window and
        // canonical receipt; a receipt without typed evidence remains blocked.
        if checkpoint.windows.is_empty() {
            let audio = preparation.materialize_window(0)?;
            if let Some(sample) =
                self.reusable_native_pilot_sample(&preparation, request, &audio)?
            {
                let _ = self.append_native_transcript_checkpoint(
                    &preparation,
                    NativeTranscriptCheckpointWindowInput::PilotSample {
                        request,
                        audio: &audio,
                        sample: &sample,
                    },
                )?;
            }
        }

        let reuse = self.probe_native_transcript_checkpoint_reuse(&preparation, request)?;
        ensure!(
            reuse.checkpoint.plan.windows == preparation.windows(),
            "native checkpoint reuse plan differs from the live prepared windows"
        );
        if let Some(recovery) = reuse.windows.iter().find_map(|state| match state {
            NativeTranscriptWindowReuse::RecoveryRequired {
                window_index,
                request_key,
            } => Some((*window_index, request_key.clone())),
            _ => None,
        }) {
            bail!(
                "native paid window {} requires typed-result recovery before another send; inspect/reconcile accounting request {}",
                recovery.0,
                recovery.1.unwrap_or_else(|| "<unknown>".into())
            );
        }

        let mut new_windows = 0_u32;
        for state in &reuse.windows {
            let NativeTranscriptWindowReuse::Missing { window_index } = state else {
                continue;
            };
            if is_cancelled() {
                return Ok(NativeAssetStep::Cancelled);
            }
            // Every actual new send re-proves the passed pilot, whole-policy
            // authority, current source and frozen policy immediately before the
            // first accounting reservation.
            let authorized = self.authorized_policy_for_bulk(
                &policy.policy_id,
                AnalysisRunKind::NativeSpeechTranscription,
            )?;
            ensure!(
                authorized.authorization.policy.policy_id == policy.policy_id,
                "native speech batch authorization changed before provider dispatch"
            );
            self.require_current_native_policy(policy)?;
            preparation.validate_source_current(self)?;
            if run_id.is_none() {
                if is_cancelled() {
                    return Ok(NativeAssetStep::Cancelled);
                }
                *run_id = Some(self.begin_accounted_analysis_run(
                    &policy.project_id,
                    &policy.policy_id,
                    AnalysisRunKind::NativeSpeechTranscription.as_str(),
                )?);
            }
            let active_run_id = run_id.as_deref().expect("native run created above");
            let audio = preparation.materialize_window(*window_index as usize)?;
            let context = bulk_request_accounting_context(
                policy,
                active_run_id,
                asset_id,
                format!("{}:{asset_id}", policy.kind.as_str()),
                false,
            )?;
            let accounted =
                AccountedNativeSttProvider::new(self, policy, context, request, &audio, transport);
            let result = match accounted.transmit_with_cancel(&mut *is_cancelled) {
                Ok(result) => result,
                Err(error)
                    if ProviderFailure::from_anyhow(&error).category
                        == ProviderFailureCategory::Cancelled =>
                {
                    return Ok(NativeAssetStep::Cancelled);
                }
                Err(error) => return Err(error),
            };

            // The immutable receipt is already settled. Persist the typed C9
            // window before honoring post-response cancellation or doing any
            // other work that could stop the batch.
            let _ = self.append_native_transcript_checkpoint(
                &preparation,
                NativeTranscriptCheckpointWindowInput::AccountedResult {
                    request,
                    audio: &audio,
                    result: &result,
                },
            )?;
            new_windows = new_windows.saturating_add(1);
            // The final window has the same stop obligations as any earlier
            // response. Retain its typed prefix before surfacing unknown usage
            // or an overrun, even when there would be no further dispatch.
            super::verify_bulk_authority(self, policy)?;
            ensure!(
                !self
                    .analysis_accounting_aggregate(&policy.policy_id, None)?
                    .has_unknown_usage(),
                "canonical native usage is unknown; reconcile request evidence before continuing"
            );
            if native_serving_model_identity_requires_review(
                result.call.receipt.actual_model.as_deref(),
                request.model(),
            ) {
                bail!(
                    "native_stt_serving_model_identity_requires_review: receipt retained, inspect before retry"
                );
            }
            if is_cancelled() {
                return Ok(NativeAssetStep::Cancelled);
            }
        }

        let provenance = self
            .native_transcript_provenance(asset_id)?
            .ok_or_else(|| {
                anyhow::anyhow!("native transcript provenance is missing after window persistence")
            })?;
        if provenance_requires_serving_model_review(&provenance, &policy.primary_model) {
            bail!(
                "native_stt_serving_model_identity_requires_review: inspect the canonical receipt before continuing"
            );
        }
        let complete = provenance.coverage.as_ref().is_some_and(|coverage| {
            coverage.completed_window_count == coverage.planned_window_count
                && coverage.remaining_window_count == 0
        });
        ensure!(
            complete,
            "native speech coverage remains incomplete after typed window persistence"
        );
        // A fully pilot-seeded or previously cached checkpoint still needs its
        // atomic terminal assembly once its exact plan is complete.
        self.finalize_native_transcript_checkpoint(&preparation, request)?;
        let provenance = self
            .native_transcript_provenance(asset_id)?
            .ok_or_else(|| {
                anyhow::anyhow!("native transcript provenance disappeared after finalization")
            })?;
        Ok(NativeAssetStep::Outcome(Box::new(
            native_asset_outcome_from_provenance(&provenance, new_windows == 0),
        )))
    }

    fn require_current_native_policy(&self, policy: &RunPolicy) -> Result<()> {
        let current = self.describe_run_policy_for_local_preparation(&policy.request())?;
        ensure!(
            current.policy_id == policy.policy_id && current.readiness.ready_for_authorization,
            "native speech batch policy or selected source identity changed before local/cloud work"
        );
        Ok(())
    }

    fn save_native_request_free_outcome(
        &self,
        preparation: &NativeSpeechPreparation,
    ) -> Result<StoredTranscript> {
        preparation.validate_source_current(self)?;
        let evidence = preparation.evidence();
        ensure!(
            !has_native_transcript_association(&self.connection()?, &evidence.asset_id)?,
            "request-free native outcome cannot replace prior native work"
        );
        let (status, outcome_reason) = match preparation.outcome() {
            NativeSpeechLocalOutcome::NoAudio => (
                TranscriptStatus::NoAudio,
                TranscriptOutcomeReason::NoAudioStream,
            ),
            NativeSpeechLocalOutcome::NoSpeech => (
                TranscriptStatus::NoSpeech,
                TranscriptOutcomeReason::NoDetectedSpeech,
            ),
            NativeSpeechLocalOutcome::PreparedSpeech => {
                bail!("request-free native persistence requires NoAudio or NoSpeech")
            }
        };
        let transcript = StoredTranscript {
            asset_id: evidence.asset_id.clone(),
            recipe_key: evidence.recipe.clone(),
            source_fingerprint: evidence
                .scope
                .sampled_representation_fingerprint
                .clone()
                .ok_or_else(|| {
                    anyhow::anyhow!("native silent outcome has no sampled fingerprint")
                })?,
            status,
            outcome_reason,
            reject_reason: String::new(),
            primary_language: String::new(),
            speech_seconds: 0.0,
            window_count: 0,
            provider_segment_count: 0,
            mean_token_probability: None,
            repetition_ratio: None,
            segments: Vec::new(),
        };
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure!(
            !has_native_transcript_association(&transaction, &evidence.asset_id)?,
            "request-free native outcome association changed before persistence"
        );
        save_native_transcript_on_transaction(
            &transaction,
            &evidence.policy_id,
            &transcript,
            &[],
            None,
        )?;
        transaction.commit()?;
        Ok(transcript)
    }
}

fn native_asset_outcome_from_provenance(
    provenance: &crate::analysis::store::NativeTranscriptProvenance,
    reused: bool,
) -> NativeSpeechAssetOutcome {
    NativeSpeechAssetOutcome {
        asset_id: provenance.asset_id.clone(),
        status: provenance.status.as_str().into(),
        outcome_reason: provenance.outcome_reason.as_str().into(),
        reused,
    }
}

fn provenance_requires_serving_model_review(
    provenance: &crate::analysis::store::NativeTranscriptProvenance,
    requested_model: &str,
) -> bool {
    provenance.windows.iter().any(|window| {
        native_serving_model_identity_requires_review(
            window.accounting.request.actual_model.as_deref(),
            requested_model,
        )
    })
}

fn native_batch_failure(
    asset_id: Option<String>,
    error: &anyhow::Error,
) -> NativeSpeechBatchFailure {
    let provider_failure = ProviderFailure::from_anyhow(error);
    let code = match provider_failure.category {
        ProviderFailureCategory::LocalBudgetExhausted => "native_batch_budget_exhausted",
        ProviderFailureCategory::Cancelled => "native_batch_cancelled",
        ProviderFailureCategory::LocalAdmission => "native_batch_admission_denied",
        ProviderFailureCategory::LocalPreUpload => "native_batch_pre_upload_failed",
        ProviderFailureCategory::LocalPostDispatch => {
            "native_batch_post_dispatch_persistence_failed"
        }
        ProviderFailureCategory::Configuration => "native_batch_configuration_failed",
        ProviderFailureCategory::Transport => "native_batch_transport_failed",
        ProviderFailureCategory::HttpStatus => "native_batch_http_failed",
        ProviderFailureCategory::InvalidResponse => "native_batch_invalid_response",
        ProviderFailureCategory::Unclassified => "native_batch_failed",
    };
    let detail = if provider_failure.category == ProviderFailureCategory::Unclassified {
        format!("{error:#}")
    } else {
        provider_failure.safe_detail()
    };
    NativeSpeechBatchFailure {
        asset_id,
        code: code.into(),
        detail: detail.chars().take(240).collect(),
    }
}
