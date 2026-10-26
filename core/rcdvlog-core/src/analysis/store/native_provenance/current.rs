//! Currentness proof for retained native transcript artifacts.
//!
//! Historical provenance remains readable elsewhere. This module answers the
//! narrower present-time question: may existing evidence be reused as the
//! current transcript (and, for accepted Speech, as prompt context)? It performs
//! no provider/model/media preparation and never grants spending authority.
//!
//! All SQLite header, frozen-policy, window and accounting reads happen through
//! the caller's single read transaction. The only external identity is the
//! full selected derived-source proof supplied by the existing
//! `analysis_proxy_source` owner.

use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, Transaction};

use crate::analysis::accounting::store as accounting_store;
use crate::analysis::accounting::store::NativeSettledResultProof;
use crate::analysis::quality::native_stt::NATIVE_STT_QUALITY_RECIPE_VERSION;
use crate::analysis::run_policy::{AnalysisRunKind, RunAssetScope};
use crate::analysis::store::{
    StoredTranscript, TranscriptCurrentState, TranscriptStatus, has_native_transcript_association,
    load_transcript_current_state,
};
use crate::representation::AnalysisProxySource;
use crate::{Asset, Catalog};

use super::checkpoint::NativeTranscriptCheckpointWindow;
use super::plan::parse_validated_plan;
use super::read::load_native_transcript_provenance;
use super::types::{
    NativeSttWindowObservation, NativeTranscriptProvenance, NativeTranscriptWindowPlan,
};
use super::validation::{validate_native_policy_recipe_binding, validate_transcript_shape};
use super::write::validate_asset_scope;

/// Prove that one retained native transcript header is current terminal
/// evidence. Any malformed, incomplete, stale or unprovable association is
/// rejected; callers translate this error into a review/recovery exception
/// rather than a local rewrite.
pub(crate) fn prove_current_native_transcript(
    transaction: &Transaction<'_>,
    transcript: &StoredTranscript,
    source: &AnalysisProxySource,
) -> Result<()> {
    ensure!(
        !source.sampled_fingerprint.trim().is_empty()
            && source.sampled_fingerprint.trim() == source.sampled_fingerprint,
        "current native transcript source fingerprint is missing or malformed"
    );
    let provenance = load_native_transcript_provenance(transaction, &transcript.asset_id)?
        .ok_or_else(|| anyhow::anyhow!("native transcript association disappeared"))?;
    ensure!(
        provenance.asset_id == transcript.asset_id
            && provenance.recipe_key == transcript.recipe_key
            && provenance.source_fingerprint == transcript.source_fingerprint
            && provenance.status == transcript.status
            && provenance.outcome_reason == transcript.outcome_reason,
        "current native transcript header/provenance identities disagree"
    );
    ensure!(
        provenance.native_quality_recipe_version == NATIVE_STT_QUALITY_RECIPE_VERSION,
        "native transcript quality recipe is not current"
    );
    ensure!(
        provenance.policy.recipe_version == transcript.recipe_key
            && provenance.source_fingerprint == source.sampled_fingerprint,
        "native transcript recipe/source fingerprint is not current"
    );
    validate_native_policy_recipe_binding(&provenance.policy, &transcript.recipe_key)?;

    let scope = validate_asset_scope(transaction, &provenance.policy, transcript)?;
    ensure!(
        scope.sampled_representation_fingerprint.as_deref()
            == Some(source.sampled_fingerprint.as_str())
            && transcript.source_fingerprint == source.sampled_fingerprint,
        "native transcript selected derived-source fingerprint changed"
    );
    ensure!(
        scope.representation_fingerprint.as_deref() == Some(source.fingerprint.as_str()),
        "native transcript full derived-representation fingerprint changed"
    );
    ensure!(
        scope.source_duration_ms == source.duration_ms,
        "native transcript source duration/time mapping changed"
    );

    if transcript.status == TranscriptStatus::Failed {
        anyhow::bail!("failed/native-coverage-incomplete transcript is not current");
    }

    let plan = load_retained_plan(transaction, transcript, &provenance, scope)?;
    match transcript.status {
        TranscriptStatus::NoAudio | TranscriptStatus::NoSpeech => {
            ensure!(
                plan.is_none() && provenance.windows.is_empty() && provenance.coverage.is_none(),
                "verified native silence/audio absence must remain request-free and plan-free"
            );
            validate_transcript_shape(transcript, &[])?;
        }
        TranscriptStatus::Speech
        | TranscriptStatus::NoAcceptedSpeech
        | TranscriptStatus::Rejected
        | TranscriptStatus::ReviewRequired => {
            let plan = plan.ok_or_else(|| {
                anyhow::anyhow!(
                    "provider-backed complete native transcript has no retained window plan"
                )
            })?;
            prove_current_complete_transcript(transaction, transcript, &provenance, scope, &plan)?;
        }
        TranscriptStatus::Failed => unreachable!("failed status rejected above"),
    }
    Ok(())
}

/// Existing transcript/quality and accounting owners jointly prove terminal
/// evidence; this read-only helper never authorizes or dispatches a request.
fn prove_current_complete_transcript(
    transaction: &Transaction<'_>,
    transcript: &StoredTranscript,
    provenance: &NativeTranscriptProvenance,
    scope: &RunAssetScope,
    plan: &NativeTranscriptWindowPlan,
) -> Result<()> {
    let windows = checkpoint_windows(provenance)?;
    ensure!(
        provenance.coverage.as_ref().is_some_and(|coverage| {
            coverage.planned_window_count as usize == plan.windows.len()
                && coverage.completed_window_count as usize == windows.len()
                && coverage.remaining_window_count == 0
        }) && windows.len() == plan.windows.len(),
        "native transcript coverage is incomplete or inconsistent"
    );
    for window in &windows {
        super::checkpoint::ensure_checkpoint_quality_is_typed(
            window.observation.quality.decision,
            window.observation.quality.reason,
        )?;
    }
    let observations: Vec<_> = windows
        .iter()
        .map(|window| window.observation.clone())
        .collect();
    if transcript.status == TranscriptStatus::Rejected {
        // Rejected candidate text is intentionally discarded by the
        // terminal owner, so whole-clip repetition cannot be recomputed
        // from retained rows. The rejection is still safe (and never
        // prompt evidence); re-prove its terminal shape, exact planned
        // speech summary and canonical accounting instead of pretending
        // discarded text can be reconstructed.
        validate_transcript_shape(transcript, &observations)?;
        ensure!(
            transcript.primary_language.is_empty()
                && (transcript.speech_seconds - planned_speech_seconds(plan)?).abs() < f64::EPSILON,
            "rejected native transcript summary disagrees with its retained plan"
        );
    } else {
        let identity = crate::analysis::store::native_provenance::checkpoint::NativeTranscriptAssemblyIdentity {
                    asset_id: &transcript.asset_id,
                    recipe: &transcript.recipe_key,
                    scope,
                };
        let assembled = super::checkpoint::assemble_native_transcript_completion_for_source(
            identity, plan, &windows,
        )?;
        ensure!(
            assembled == *transcript,
            "retained native transcript status/summary disagrees with its exact window evidence"
        );
        validate_transcript_shape(transcript, &observations)?;
    }
    for window in &provenance.windows {
        accounting_store::validate_native_settled_result_request(
            transaction,
            &window.accounting.request.request_key,
            &NativeSettledResultProof {
                project_id: &provenance.policy.project_id,
                policy_id: &provenance.policy.policy_id,
                policy_fingerprint: &provenance.policy.policy_id,
                expected_provider_cost_bound_contract: &provenance
                    .policy
                    .provider_cost_bound_contract,
                source_fingerprint: &scope.source_identity,
                requested_provider: &provenance.policy.provider,
                requested_model: &provenance.policy.primary_model,
                run_id: None,
                allowed_operations: &["pilot", AnalysisRunKind::NativeSpeechTranscription.as_str()],
            },
        )?;
    }
    Ok(())
}

fn planned_speech_seconds(plan: &NativeTranscriptWindowPlan) -> Result<f64> {
    let speech_ms = plan
        .windows
        .iter()
        .try_fold(0_u64, |total, window| {
            total.checked_add(u64::from(window.speech_ms()))
        })
        .ok_or_else(|| anyhow::anyhow!("native transcript planned speech duration overflowed"))?;
    Ok(speech_ms as f64 / 1_000.0)
}

fn load_retained_plan(
    transaction: &Transaction<'_>,
    transcript: &StoredTranscript,
    provenance: &NativeTranscriptProvenance,
    scope: &RunAssetScope,
) -> Result<Option<NativeTranscriptWindowPlan>> {
    let row: Option<(Option<String>, Option<String>)> = transaction
        .query_row(
            "SELECT native_window_plan_json, native_window_plan_fingerprint
             FROM asset_transcripts WHERE asset_id=?1",
            [&transcript.asset_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (plan_json, plan_fingerprint) =
        row.ok_or_else(|| anyhow::anyhow!("native transcript header disappeared"))?;
    match (plan_json, plan_fingerprint) {
        (None, None) => Ok(None),
        (Some(plan_json), Some(plan_fingerprint)) => Ok(Some(parse_validated_plan(
            &plan_json,
            &plan_fingerprint,
            &provenance.policy,
            scope,
        )?)),
        _ => anyhow::bail!("native transcript window plan association is incomplete"),
    }
}

fn checkpoint_windows(
    provenance: &NativeTranscriptProvenance,
) -> Result<Vec<NativeTranscriptCheckpointWindow>> {
    provenance
        .windows
        .iter()
        .enumerate()
        .map(|(expected_index, window)| {
            ensure!(
                window.window_index as usize == expected_index,
                "native transcript window evidence is not contiguous and ordered"
            );
            let candidates = window
                .candidates
                .clone()
                .ok_or_else(|| anyhow::anyhow!("native transcript candidate mapping is absent"))?;
            Ok(NativeTranscriptCheckpointWindow {
                window_index: window.window_index,
                observation: NativeSttWindowObservation {
                    request_key: window.accounting.request.request_key.clone(),
                    latency_ms: window.latency_ms,
                    reported_language: window.reported_language.clone(),
                    quality: window.quality.clone(),
                },
                candidates,
            })
        })
        .collect()
}

impl Catalog {
    /// Currentness with an already proved selected-derived-source identity. This
    /// is the preferred read-only path for policy/prompt callers that already
    /// ran the existing `analysis_proxy_source` owner.
    pub(crate) fn current_transcript_state_with_proxy(
        &self,
        asset: &Asset,
        proxy: &AnalysisProxySource,
    ) -> Result<TranscriptCurrentState> {
        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction()?;
        let state = load_transcript_current_state(
            &transaction,
            &asset.id,
            &proxy.sampled_fingerprint,
            Some(proxy),
        )?;
        transaction.commit()?;
        Ok(state)
    }

    /// Currentness from the canonical transcript owner. Local rows retain their
    /// exact existing recipe/sampled-fingerprint rule. Native rows obtain the
    /// full selected-derived-source identity from the existing representation
    /// owner, but never load a model, reconstruct PCM, resolve credentials or
    /// contact a provider.
    pub(crate) fn current_transcript_state(
        &self,
        asset_id: &str,
    ) -> Result<TranscriptCurrentState> {
        let connection = self.connection()?;
        let native_associated = has_native_transcript_association(&connection, asset_id)?;
        drop(connection);

        let proxy = if native_associated {
            let asset = self.get_asset(asset_id)?;
            match self.analysis_proxy_source(&asset) {
                Ok(proxy) => Some(proxy),
                Err(error) => {
                    return Ok(TranscriptCurrentState::ProtectedNative {
                        reason: format!(
                            "current selected derived-source proof is unavailable: {error:#}"
                        ),
                    });
                }
            }
        } else {
            None
        };
        let selected_fingerprint = match &proxy {
            Some(proxy) => proxy.sampled_fingerprint.clone(),
            None => match self.proxy_fingerprint(asset_id) {
                Ok(fingerprint) => fingerprint,
                Err(_) => return Ok(TranscriptCurrentState::Missing),
            },
        };
        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction()?;
        let state = load_transcript_current_state(
            &transaction,
            asset_id,
            &selected_fingerprint,
            proxy.as_ref(),
        )?;
        transaction.commit()?;
        Ok(state)
    }
}
