//! Bounded one-shot native speech capability-pilot orchestration.
//!
//! The existing run-policy owner supplies the frozen policy, shared pilot
//! session/authorization/history, owned accounted run and projection. The
//! transcription owner performs selected-proxy FFprobe/FFmpeg/Silero
//! preparation. This module only scans the frozen selected scope for the first
//! bounded speech window, performs the one accounted native transmission through
//! the shared accounting boundary, and attaches one typed C7 sample. It never
//! retries, falls back, re-splits, runs native bulk, or constructs a chat request.

use std::sync::OnceLock;

use anyhow::{Context, Result, bail, ensure};
use chrono::{SecondsFormat, Utc};

use crate::analysis::accounting::AccountedNativeSttProvider;
use crate::analysis::openrouter_stt::{
    NativeSttCost, NativeSttRequest, NativeSttTransport, OpenRouterSttClient,
};
use crate::analysis::provider::ProviderFailure;
use crate::analysis::quality::native_stt::{NativeSttQualityDecision, NativeSttQualityReason};
use crate::transcription::{
    NativeSpeechLocalOutcome, NativeSpeechPreparation, NativeSpeechPreparer,
};
use crate::{Catalog, ResourceOverrides};

use super::{
    AnalysisRunKind, NativePilotEvidence, NativePilotLocalOutcomeFact, NativePilotPaidSample,
    NativePilotSampleOutcome, PilotSession, PilotSessionStart, PilotValidation, RunPilotRecord,
    RunPolicyRequest, native_stt_request_from_policy, pilot_accounting_context_for_scope,
    prepare_pilot_session,
};

mod transport;
mod validation;
pub(super) use transport::DeferredEnvironmentNativeSttTransport;
pub(crate) use validation::native_serving_model_identity_requires_review;
use validation::validate_native_accounted_result;

#[derive(Debug)]
struct FirstNativeSpeech {
    preparation: NativeSpeechPreparation,
    request: NativeSttRequest,
    audio: crate::analysis::openrouter_stt::NativeSttAudioWindow,
}

struct NativeScopeScan {
    first: Option<FirstNativeSpeech>,
    local_outcomes: Vec<NativePilotLocalOutcomeFact>,
}

/// Execute the real one-shot native capability pilot. The caller supplies the
/// narrow transport seam; production uses the deferred secure credential client
/// above and focused tests use a recording fake through the same SQLite boundary.
pub(super) fn run_native_pilot_with_transport<C>(
    catalog: &Catalog,
    request: &RunPolicyRequest,
    transport: &dyn NativeSttTransport,
    retry_uncertain: bool,
    preparer: &mut NativeSpeechPreparer,
    is_cancelled: &mut C,
) -> Result<RunPilotRecord>
where
    C: FnMut() -> bool,
{
    ensure!(
        request.parameters.kind() == AnalysisRunKind::NativeSpeechTranscription,
        "native pilot entry point requires an explicitly selected native speech policy"
    );
    // Cancellation before shared authorization/source/key/claim work.
    if is_cancelled() {
        bail!("native pilot cancelled before provider transmission");
    }

    let mut session = match prepare_pilot_session(catalog, request, retry_uncertain)? {
        PilotSessionStart::AlreadyPassed(record) | PilotSessionStart::Cached(record) => {
            return Ok(*record);
        }
        PilotSessionStart::Ready(session) => *session,
    };

    let scan = scan_for_first_speech(catalog, &session.policy, preparer, is_cancelled)?;
    let local_outcomes = scan.local_outcomes;
    if is_cancelled() {
        bail!("native pilot cancelled after local speech scan");
    }
    let current_policy = catalog.describe_run_policy_for_local_preparation(request)?;
    ensure!(
        current_policy.policy_id == session.policy.policy_id
            && current_policy.readiness.ready_for_authorization,
        "native pilot policy or selected sources changed during local speech scan"
    );
    let Some(first_speech) = scan.first else {
        return finalize_all_local_outcome(catalog, &session, local_outcomes, is_cancelled);
    };

    // The only owned pilot asset/context is the actual first speech source.
    // Frozen policy.scope order is never mutated or reordered.
    session.first_scope = first_speech.preparation.evidence().scope.clone();
    first_speech
        .preparation
        .validate_source_current(catalog)
        .context("recheck prepared native speech source before owned pilot")?;

    if is_cancelled() {
        bail!("native pilot cancelled before provider transmission");
    }
    let started_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let run_id = catalog.begin_accounted_analysis_run(
        &session.policy.project_id,
        &session.policy.policy_id,
        "pilot",
    )?;
    let running = catalog.start_owned_pilot_run(&session, &run_id, &started_at)?;
    let context = pilot_accounting_context_for_scope(
        &session.policy,
        &run_id,
        &session.first_scope,
        retry_uncertain,
    )?;

    let accounted = AccountedNativeSttProvider::new(
        catalog,
        &session.policy,
        context,
        &first_speech.request,
        &first_speech.audio,
        transport,
    );
    let result = match accounted.transmit_with_cancel(&mut *is_cancelled) {
        Ok(result) => result,
        Err(error) => {
            return finish_native_transport_error(catalog, &session, &running, error);
        }
    };

    // The receipt is already persisted by the shared accounting boundary. Build
    // and attach the typed sample before honoring post-response cancellation.
    let sample = NativePilotPaidSample::from_accounted_result(
        &session.policy,
        &session.first_scope,
        &first_speech.request,
        &first_speech.audio,
        &result,
    )?;
    let (validation, failure_hint) =
        validate_native_accounted_result(&first_speech.request, &result, &sample);
    let evidence = NativePilotEvidence {
        schema_version: super::NATIVE_PILOT_EVIDENCE_SCHEMA_VERSION.to_string(),
        sample: Some(sample),
        local_outcomes,
    };
    let attached = catalog
        .attach_native_pilot_evidence(
            &running,
            evidence,
            Some((&first_speech.request, &first_speech.audio)),
        )?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "paid native pilot result could not attach to the exact running pilot row"
            )
        })?;

    if is_cancelled() {
        // The one paid result and its receipt are preserved. Cancellation only
        // stops future work (there is none in C8) and is reported explicitly.
        let _ = catalog.finalize_pilot_projection(
            &attached,
            &session.baseline,
            validation,
            failure_hint,
        )?;
        bail!("native pilot cancelled after provider response; paid result was retained");
    }

    catalog.finalize_pilot_projection(&attached, &session.baseline, validation, failure_hint)
}

fn scan_for_first_speech<C>(
    catalog: &Catalog,
    policy: &super::RunPolicy,
    preparer: &mut NativeSpeechPreparer,
    is_cancelled: &mut C,
) -> Result<NativeScopeScan>
where
    C: FnMut() -> bool,
{
    let mut local_outcomes = Vec::new();
    for scope in &policy.scope {
        if is_cancelled() {
            bail!("native pilot cancelled during local speech scan");
        }
        let preparation = preparer
            .prepare(
                catalog,
                &policy.policy_id,
                &scope.asset_id,
                &ResourceOverrides::default(),
                is_cancelled,
            )
            .context("prepare selected native speech asset")?;
        match preparation.outcome() {
            NativeSpeechLocalOutcome::NoAudio | NativeSpeechLocalOutcome::NoSpeech => {
                let fact = NativePilotLocalOutcomeFact::from_preparation(&preparation)?
                    .ok_or_else(|| anyhow::anyhow!("local native outcome fact was not built"))?;
                local_outcomes.push(fact);
                drop(preparation);
            }
            NativeSpeechLocalOutcome::PreparedSpeech => {
                ensure!(
                    preparation.window_count() >= 1,
                    "prepared native speech has no bounded window"
                );
                let request = native_stt_request_from_policy(policy)?;
                let audio = preparation.materialize_window(0)?;
                return Ok(NativeScopeScan {
                    first: Some(FirstNativeSpeech {
                        preparation,
                        request,
                        audio,
                    }),
                    local_outcomes,
                });
            }
        }
    }
    Ok(NativeScopeScan {
        first: None,
        local_outcomes,
    })
}

fn finalize_all_local_outcome<C>(
    catalog: &Catalog,
    session: &PilotSession,
    local_outcomes: Vec<NativePilotLocalOutcomeFact>,
    is_cancelled: &mut C,
) -> Result<RunPilotRecord>
where
    C: FnMut() -> bool,
{
    if is_cancelled() {
        bail!("native pilot cancelled before local-only provider-free finalization");
    }
    ensure!(
        !local_outcomes.is_empty() && local_outcomes.len() == session.policy.scope.len(),
        "all-local native outcome must contain exactly one fact for every selected source"
    );
    let started_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let run_id = catalog.begin_accounted_analysis_run(
        &session.policy.project_id,
        &session.policy.policy_id,
        "pilot",
    )?;
    let running = catalog.start_owned_pilot_run(session, &run_id, &started_at)?;
    let evidence = NativePilotEvidence::with_local_outcomes(local_outcomes);
    let attached = catalog
        .attach_native_pilot_evidence(&running, evidence, None)?
        .ok_or_else(|| anyhow::anyhow!("local native pilot evidence could not attach"))?;
    catalog.finalize_native_no_provider_projection(&attached, &session.baseline)
}

fn finish_native_transport_error(
    catalog: &Catalog,
    session: &PilotSession,
    running: &RunPilotRecord,
    error: anyhow::Error,
) -> Result<RunPilotRecord> {
    let failure = ProviderFailure::from_anyhow(&error);
    let has_new_request = catalog
        .list_analysis_requests(
            &session.policy.project_id,
            Some(&session.policy.policy_id),
            1000,
        )?
        .iter()
        .any(|request| request.run_id == running.run_id);
    if failure.category.is_local() && session.had_previous_pilot && !has_new_request {
        if let Some(prior) = session.previous_record.as_ref() {
            catalog.save_pilot(prior)?;
        }
        let safe = super::bounded(&failure.safe_detail(), 240);
        catalog.finish_analysis_run(
            &running.run_id,
            &crate::AnalysisRunOutcome {
                status: "failed".into(),
                stage_reached: "pilot".into(),
                cost_usd: 0.0,
                error: safe,
                ..Default::default()
            },
        )?;
        return Err(error);
    }
    let record = catalog.finalize_pilot_projection(
        running,
        &session.baseline,
        PilotValidation {
            structural_schema_valid: false,
            source_time_semantics_valid: false,
            capability_observed: false,
            detail: failure.safe_detail(),
        },
        Some(failure.safe_detail()),
    )?;
    if failure.category.is_local() {
        return Err(error);
    }
    Ok(record)
}
