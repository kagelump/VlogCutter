//! Accounted native speech-transcription leaf.
//!
//! One supplied bounded speech window crosses the same canonical admission,
//! dispatch marker, receipt settlement and uncertainty owners as VLM work. Both
//! the bounded capability pilot (`pilot`) and the authorized native bulk
//! operation (`native_speech_transcription`) use this exact adapter; local VAD,
//! transcript persistence, window planning and retry orchestration stay with
//! their owning modules.

use anyhow::{Result, ensure};

use super::lifecycle::AccountingBoundary;
use super::model::{AnalysisAccountingContext, ProviderReceiptEvidence};
use super::store::validate_native_audio_source_bounds;
use crate::Catalog;
use crate::analysis::openrouter_stt::{
    NativeSttAudioWindow, NativeSttCallResult, NativeSttOutcome, NativeSttRequest,
    NativeSttTransport, native_stt_cost_bound_contract_id, native_stt_request_cost_bound,
};
use crate::analysis::run_policy::{
    AnalysisPayload, AnalysisRunKind, RunPolicy, RunPolicyParameters,
};

/// One completed accounting lifecycle. Financial settlement has already been
/// persisted before this value is returned, including for an unusable body.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeSttAccountedResult {
    pub request_key: String,
    pub call: NativeSttCallResult,
}

impl NativeSttAccountedResult {
    pub fn latency_ms(&self) -> u64 {
        self.call.latency_ms
    }

    pub fn outcome(&self) -> &NativeSttOutcome {
        &self.call.outcome
    }
}

/// Wraps exactly one supplied bounded native speech window. The caller owns
/// authority initialization and the real accounted run; this adapter does not
/// create budgets, runs, scopes or credentials.
pub struct AccountedNativeSttProvider<'a> {
    catalog: &'a Catalog,
    policy: &'a RunPolicy,
    context: AnalysisAccountingContext,
    request: &'a NativeSttRequest,
    audio: &'a NativeSttAudioWindow,
    transport: &'a dyn NativeSttTransport,
}

impl<'a> AccountedNativeSttProvider<'a> {
    pub fn new(
        catalog: &'a Catalog,
        policy: &'a RunPolicy,
        context: AnalysisAccountingContext,
        request: &'a NativeSttRequest,
        audio: &'a NativeSttAudioWindow,
        transport: &'a dyn NativeSttTransport,
    ) -> Self {
        Self {
            catalog,
            policy,
            context,
            request,
            audio,
            transport,
        }
    }

    /// Perform the single accounted send. Cancellation is checked before
    /// admission, after admission, after local transport preparation, and
    /// immediately before the durable dispatch marker.
    pub fn transmit(&self) -> Result<NativeSttAccountedResult> {
        self.transmit_with_cancel(|| false)
    }

    pub fn transmit_with_cancel<C>(&self, mut is_cancelled: C) -> Result<NativeSttAccountedResult>
    where
        C: FnMut() -> bool,
    {
        let predispatch_guard = || self.validate_preconditions();
        AccountingBoundary::new(self.catalog, &self.context.run_id, Some(&predispatch_guard))
            .execute(
                &mut is_cancelled,
                || {
                    // The shared lifecycle checks cancellation before any
                    // authorization/source reads or reservation work.
                    self.validate_preconditions()?;
                    self.catalog.reserve_analysis_native_stt_request(
                        &self.context,
                        self.request,
                        self.audio,
                    )
                },
                || self.transport.prepare(),
                || self.transport.transcribe(self.request, self.audio),
                native_receipt_evidence,
            )
            .map(|transmission| NativeSttAccountedResult {
                request_key: transmission.request_key,
                call: transmission.value,
            })
    }

    /// Mandatory pre-dispatch guard. It re-reads the canonical authorization
    /// and freshly materialized policy identity, proves the exact scope entry
    /// for the context source, and validates the referenced asset after local
    /// transport preparation. It never trusts a caller-supplied "current" flag.
    fn validate_preconditions(&self) -> Result<()> {
        ensure!(
            matches!(
                self.context.operation.as_str(),
                "pilot" | "native_speech_transcription"
            ),
            "native STT accounted adapter requires the pilot or native bulk operation"
        );
        ensure!(
            !self.policy.policy_id.trim().is_empty()
                && self.context.policy_id == self.policy.policy_id
                && self.context.policy_fingerprint == self.policy.policy_id,
            "native STT policy/accounting identity is missing or mismatched"
        );
        ensure!(
            self.context.project_id == self.policy.project_id
                && self.context.provider == self.policy.provider
                && (self.context.budget_usd - self.policy.budget_usd).abs() <= 1e-9,
            "native STT accounting context does not match the frozen policy"
        );
        ensure!(
            self.policy.kind == AnalysisRunKind::NativeSpeechTranscription
                && self.policy.payload == AnalysisPayload::Audio,
            "native STT adapter requires an audio-only native speech policy"
        );
        let RunPolicyParameters::NativeSpeechTranscription {
            language,
            provider_policy,
            ..
        } = &self.policy.parameters
        else {
            anyhow::bail!("native STT policy is missing native speech parameters");
        };
        ensure!(
            self.policy.provider == self.request.provider()
                && self.policy.primary_model == self.request.model()
                && self.policy.allowed_models == vec![self.policy.primary_model.clone()]
                && !self.policy.allow_fallback
                && self.policy.permitted_fallbacks.is_empty(),
            "native STT request model/provider/fallback does not match the frozen policy"
        );
        ensure!(
            language.as_deref() == self.request.language()
                && *provider_policy == self.request.provider_policy()
                && self.request.temperature().to_bits() == 0.0_f64.to_bits(),
            "native STT request language/data-policy/temperature does not match the frozen policy"
        );
        ensure!(
            self.policy.provider_cost_bound_contract == native_stt_cost_bound_contract_id(),
            "native STT policy is not bound to the registered native cost contract"
        );
        let current_quote = native_stt_request_cost_bound(self.request, self.audio)?;
        ensure!(
            current_quote.contract_version == self.policy.provider_cost_bound_contract
                && current_quote.audio.is_some(),
            "native STT current quote does not match the frozen native cost contract"
        );

        let authorization = self
            .catalog
            .load_run_authorization(&self.policy.policy_id)?;
        ensure!(
            authorization.policy.policy_id == self.policy.policy_id
                && authorization.policy.kind == AnalysisRunKind::NativeSpeechTranscription,
            "native STT canonical authorization changed before dispatch"
        );
        let current = self
            .catalog
            .describe_run_policy_for_local_preparation(&self.policy.request())?;
        ensure!(
            current.policy_id == self.policy.policy_id && current.readiness.ready_for_authorization,
            "native STT canonical policy or source identity changed before dispatch"
        );
        if self.context.operation == "native_speech_transcription" {
            let authorized = self.catalog.authorized_policy_for_bulk(
                &self.policy.policy_id,
                AnalysisRunKind::NativeSpeechTranscription,
            )?;
            ensure!(
                authorized.authorization.policy.policy_id == self.policy.policy_id,
                "native STT bulk capability authorization changed before dispatch"
            );
            for unresolved in self
                .catalog
                .analysis_native_requests_needing_reconciliation_for_source(
                    &self.policy.project_id,
                    &self.context.source_fingerprint,
                )?
            {
                ensure!(
                    unresolved.policy_id == self.policy.policy_id,
                    "a prior native speech request has unresolved usage; reconcile it before dispatch"
                );
            }
        }

        let matching_scopes: Vec<_> = self
            .policy
            .scope
            .iter()
            .filter(|scope| scope.source_identity == self.context.source_fingerprint)
            .collect();
        ensure!(
            matching_scopes.len() == 1,
            "native STT accounting source must match exactly one authorized scope entry"
        );
        let scope = matching_scopes[0];
        ensure!(
            self.context.intended_attempt_key
                == format!("{}:{}", self.policy.kind.as_str(), scope.asset_id),
            "native STT intended request key is not the stable policy/source identity"
        );
        validate_native_audio_source_bounds(self.audio, scope.source_duration_ms)?;
        self.catalog
            .validate_run_policy_asset(self.policy, &scope.asset_id)
    }
}

fn native_receipt_evidence(call: &NativeSttCallResult) -> Result<ProviderReceiptEvidence> {
    ProviderReceiptEvidence::from_native_stt_receipt(&call.receipt)
}
