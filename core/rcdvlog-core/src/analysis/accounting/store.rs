//! SQLite transactions for canonical analysis request admission.
//!
//! Every write that can admit a paid transmission starts with SQLite
//! `BEGIN IMMEDIATE`. Totals are recomputed inside that transaction from one
//! authority, its frozen historical evidence and the reservation rows. Provider
//! I/O deliberately lives outside this module.

use anyhow::{Context, Result, anyhow, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde_json::{Value, json};

use super::legacy::{
    UntrackedTotals, persist_untracked_denial_boundary, refresh_untracked_evidence,
    untracked_totals,
};
use super::model::{
    ANALYSIS_ACCOUNTING_SCHEMA_VERSION, AnalysisAccountingAggregate, AnalysisAccountingContext,
    AnalysisBudgetAuthorityContext, AnalysisBudgetExhausted, AnalysisBudgetSnapshot,
    AnalysisOperationAttemptProjection, AnalysisProviderReceipt, AnalysisRequestEvent,
    AnalysisRequestLifecycle, AnalysisRequestRecord, AnalysisRetryAuthorization,
    AnalysisTargetedRetryAdmission, LEGACY_UNKNOWN_USAGE_STOP_REASON,
    MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY, MAX_REQUEST_ATTEMPTS, ProviderReceiptEvidence,
    analysis_request_cost_bound_fingerprint, attempt_base_key, bounded_failure_code, digest_bytes,
    digest_json, request_key,
};
use super::settlement::{
    ExistingReceipt, ReceiptDisposition, classify_receipt, settlement_for_receipt,
};
use crate::analysis::openrouter_stt::{
    NativeSttAudioWindow, NativeSttRequest, native_stt_cost_bound_contract_id,
    native_stt_request_cost_bound,
};
use crate::analysis::provider::AnalysisRequestCostBound;
use crate::analysis::run_policy::{AnalysisPayload, RunPolicyParameters};

const REQUEST_SELECT: &str = "SELECT request_key,budget_scope_id,project_id,policy_id,run_id,operation,\
 attempt_base_key,attempt_epoch,requested_provider,actual_provider,requested_model,actual_model,\
 provider_generation_id,policy_fingerprint,payload_fingerprint,source_fingerprint,request_fingerprint,\
 cost_bound_fingerprint,cost_bound_json,lifecycle,reserved_cost_usd,reported_cost_usd,\
 confirmed_cost_usd,uncertain_cost_usd,failure_code,dispatch_at,settled_at,created_at,updated_at \
 FROM analysis_request_reservations";

fn request_select(connection: &Connection) -> Result<String> {
    let estimated = if crate::migrations::column_exists(
        connection,
        "analysis_request_reservations",
        "estimated_cost_usd",
    )? {
        "estimated_cost_usd"
    } else {
        "NULL"
    };
    Ok(REQUEST_SELECT.replace(
        " FROM analysis_request_reservations",
        &format!(",{estimated} FROM analysis_request_reservations"),
    ))
}

#[derive(Debug, Clone)]
pub(crate) struct Authority {
    pub budget_scope_id: String,
    pub project_id: String,
    pub policy_id: String,
    pub policy_fingerprint: String,
    pub requested_provider: String,
    pub budget_usd: f64,
    pub legacy_confirmed_cost_usd: f64,
    pub legacy_uncertain_usage_reserve_usd: f64,
    pub legacy_usage_complete: bool,
    pub legacy_evidence: Value,
    pub dispatch_stopped: bool,
    pub stop_reason: String,
}

#[derive(Debug, Clone, Default)]
struct LedgerTotals {
    confirmed: f64,
    estimated: f64,
    active_reserved: f64,
    uncertain: f64,
    active_requests: usize,
}

impl LedgerTotals {
    fn committed(&self) -> f64 {
        self.confirmed + self.estimated + self.active_reserved + self.uncertain
    }
}

/// Typed request-family proof carried into the same IMMEDIATE transaction as
/// policy/source/model/run/budget validation. It prevents a VLM wrapper from
/// spending against a native audio contract and vice versa. `Any` is only for
/// authority initialization, which freezes evidence before a request exists.
#[derive(Clone, Copy)]
pub(crate) enum PolicyRequestFamily<'a> {
    Any,
    Vlm {
        cost_bound: &'a AnalysisRequestCostBound,
    },
    NativeStt {
        request: &'a NativeSttRequest,
        audio: &'a NativeSttAudioWindow,
        cost_bound: &'a AnalysisRequestCostBound,
    },
    NativeVideo {
        media: &'a crate::NativeVideoMedia,
        evidence_id: &'a str,
        cost_bound: &'a AnalysisRequestCostBound,
    },
}

/// Prove a genuine bounded native window only claims source milliseconds that
/// exist inside one canonically authorized asset scope. This is pure and runs
/// inside the same `IMMEDIATE` admission transaction that resolved the scope.
pub(crate) fn validate_native_audio_source_bounds(
    audio: &NativeSttAudioWindow,
    source_duration_ms: u32,
) -> Result<()> {
    for region in &audio.window().regions {
        let source_end = region
            .source_start_ms
            .checked_add(region.duration_ms)
            .ok_or_else(|| anyhow!("native STT source mapping overflowed"))?;
        ensure!(
            source_end <= source_duration_ms,
            "native STT audio window exceeds the authorized source duration"
        );
    }
    Ok(())
}

fn validate_policy_request_family(
    policy: &crate::RunPolicy,
    model: &str,
    family: PolicyRequestFamily<'_>,
) -> Result<()> {
    match family {
        PolicyRequestFamily::Any => Ok(()),
        PolicyRequestFamily::NativeVideo { cost_bound, .. } => {
            let RunPolicyParameters::NativeVideoReview { options } = &policy.parameters else {
                bail!("native video parameters missing");
            };
            ensure!(
                policy.kind == crate::AnalysisRunKind::NativeVideoReview
                    && model == crate::analysis::native_video::NATIVE_VIDEO_MODEL
                    && cost_bound == &crate::analysis::native_video::cost_bound(options)?,
                "native video admission quote/model mismatch"
            );
            Ok(())
        }
        PolicyRequestFamily::Vlm { cost_bound } => {
            ensure!(
                policy.kind.is_vlm(),
                "VLM accounted wrapper cannot spend against a native speech policy"
            );
            ensure!(
                cost_bound.audio.is_none() && cost_bound.published_token_tariff.is_none(),
                "VLM accounted wrapper cannot use a native audio cost quote"
            );
            Ok(())
        }
        PolicyRequestFamily::NativeStt {
            request,
            audio,
            cost_bound,
        } => {
            ensure!(
                policy.kind == crate::AnalysisRunKind::NativeSpeechTranscription
                    && policy.readiness.ready_for_authorization,
                "native STT account wrapper requires a ready native speech policy"
            );
            ensure!(
                policy.payload == AnalysisPayload::Audio,
                "native STT policy must be audio-only"
            );
            ensure!(
                policy.provider == request.provider(),
                "native STT policy provider does not match the request"
            );
            ensure!(
                model == request.model() && policy.primary_model == request.model(),
                "native STT policy model does not match the request"
            );
            ensure!(
                !policy.allow_fallback
                    && policy.permitted_fallbacks.is_empty()
                    && policy.allowed_models == vec![policy.primary_model.clone()],
                "native STT policy must admit exactly one model with no fallback"
            );
            let RunPolicyParameters::NativeSpeechTranscription {
                language,
                provider_policy,
                ..
            } = &policy.parameters
            else {
                bail!("native STT policy is missing native speech parameters");
            };
            ensure!(
                language.as_deref() == request.language(),
                "native STT request language differs from the frozen policy"
            );
            ensure!(
                *provider_policy == request.provider_policy(),
                "native STT request data policy differs from the frozen policy"
            );
            ensure!(
                request.temperature().to_bits() == 0.0_f64.to_bits(),
                "native STT request temperature must be the fixed policy value 0"
            );
            ensure!(
                policy.provider_cost_bound_contract == native_stt_cost_bound_contract_id(),
                "native STT policy cost-bound contract is not the registered native contract"
            );
            let expected_bound = native_stt_request_cost_bound(request, audio)?;
            ensure!(
                cost_bound == &expected_bound,
                "native STT request quote does not match the current registered native cost contract"
            );
            ensure!(
                cost_bound.contract_version == policy.provider_cost_bound_contract
                    && cost_bound.audio.is_some(),
                "native STT policy and current quote use different cost contracts"
            );
            Ok(())
        }
    }
}

#[derive(Debug)]
struct LegacyEvidence {
    confirmed: f64,
    uncertain: f64,
    usage_complete: bool,
    evidence: Value,
}

/// Create or verify the one canonical authority for this scope. The legacy
/// snapshot is captured only on first creation, in the same `IMMEDIATE`
/// transaction, so re-opening or re-authorizing cannot reset prior spend.
pub(crate) fn initialize_authority(
    connection: &mut Connection,
    context: &AnalysisBudgetAuthorityContext,
) -> Result<AnalysisBudgetSnapshot> {
    context.validate()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let scope = context.resolved_budget_scope_id()?;
    validate_authority_input(&transaction, context, &scope)?;
    let existing = load_authority_optional(&transaction, &scope)?;
    if existing.is_none() {
        let legacy = collect_legacy_evidence(
            &transaction,
            &context.project_id,
            &context.policy_id,
            &context.operation,
            context.budget_usd,
        )?;
        let dispatch_stopped = !legacy.usage_complete;
        let stop_reason = if dispatch_stopped {
            LEGACY_UNKNOWN_USAGE_STOP_REASON
        } else {
            ""
        };
        let now = crate::now();
        transaction.execute(
            "INSERT INTO analysis_budget_authorities(
                budget_scope_id,project_id,policy_id,policy_fingerprint,requested_provider,budget_usd,
                legacy_confirmed_cost_usd,legacy_uncertain_usage_reserve_usd,legacy_usage_complete,
                legacy_evidence_json,dispatch_stopped,stop_reason,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?13)",
            params![
                scope,
                context.project_id,
                context.policy_id,
                context.policy_fingerprint,
                context.provider,
                context.budget_usd,
                legacy.confirmed,
                legacy.uncertain,
                legacy.usage_complete as i64,
                serde_json::to_string(&legacy.evidence)?,
                dispatch_stopped as i64,
                stop_reason,
                now,
            ],
        )?;
    }
    let mut authority = load_authority(&transaction, &scope)?;
    // An older unwired runtime may have written run/pilot spending after this
    // authority was frozen. Preserve that floor as append-only evidence and
    // fail closed; never reset or loosen the frozen authority.
    refresh_untracked_evidence(&transaction, &authority, context.budget_usd)?;
    authority = load_authority(&transaction, &scope)?;
    verify_authority(
        &authority,
        &context.project_id,
        &context.policy_id,
        &context.policy_fingerprint,
        &context.provider,
        context.budget_usd,
        &scope,
    )?;
    let snapshot = snapshot_from_authority(&transaction, &authority)?;
    transaction.commit()?;
    Ok(snapshot)
}

/// Immutable identity fields shared by initial and targeted-retry admission.
struct AdmissionIdentity {
    request_key: String,
    budget_scope_id: String,
    project_id: String,
    policy_id: String,
    run_id: String,
    operation: String,
    attempt_base_key: String,
    attempt_epoch: u32,
    requested_provider: String,
    requested_model: String,
    policy_fingerprint: String,
    payload_fingerprint: String,
    source_fingerprint: String,
    request_fingerprint: String,
    cost_bound_fingerprint: String,
}

/// Immutable owner fields shared by fresh admission and an explicitly
/// authorized targeted retry. Both paths run the same canonical project,
/// policy, provider/model/source and owned-run checks before inserting a
/// reservation; targeted retry never fabricates a replacement accounting
/// context merely to reach those checks.
struct OwnerIdentity<'a> {
    project_id: &'a str,
    policy_id: &'a str,
    policy_fingerprint: &'a str,
    provider: &'a str,
    budget_usd: f64,
    model: &'a str,
    source_fingerprint: &'a str,
    run_id: &'a str,
    operation: &'a str,
    intended_attempt_key: &'a str,
}

impl<'a> OwnerIdentity<'a> {
    fn from_context(context: &'a AnalysisAccountingContext, model: &'a str) -> Self {
        Self {
            project_id: &context.project_id,
            policy_id: &context.policy_id,
            policy_fingerprint: &context.policy_fingerprint,
            provider: &context.provider,
            budget_usd: context.budget_usd,
            model,
            source_fingerprint: &context.source_fingerprint,
            run_id: &context.run_id,
            operation: &context.operation,
            intended_attempt_key: &context.intended_attempt_key,
        }
    }

    fn from_targeted_retry(
        prior: &'a AnalysisRequestRecord,
        run_id: &'a str,
        budget_usd: f64,
    ) -> Self {
        Self {
            project_id: &prior.project_id,
            policy_id: &prior.policy_id,
            policy_fingerprint: &prior.policy_fingerprint,
            provider: &prior.provider,
            budget_usd,
            model: &prior.requested_model,
            source_fingerprint: &prior.source_fingerprint,
            run_id,
            operation: "",
            intended_attempt_key: "",
        }
    }
}

/// Caller-supplied immutable claim for one explicit targeted retry. The durable
/// authorization, exact prior payload/model and current full cost contract are
/// proved together inside the admission transaction.
pub(crate) struct TargetedRetryClaim<'a> {
    pub project_id: &'a str,
    pub prior_request_key: &'a str,
    pub expected_next_attempt_epoch: u32,
    pub run_id: Option<&'a str>,
    pub model: &'a str,
    pub payload_fingerprint: &'a str,
    pub cost_bound_fingerprint: &'a str,
    pub cost_bound: &'a AnalysisRequestCostBound,
}

fn merged_event_detail(base: Value, extra: Value) -> Value {
    match (base, extra) {
        (Value::Object(mut base), Value::Object(extra)) => {
            base.extend(extra);
            Value::Object(base)
        }
        (base, _) => base,
    }
}

/// The one balance/admission calculation used by both a fresh request and an
/// explicit targeted retry. The caller owns the outer IMMEDIATE transaction and
/// has already proved request-specific identity/authorization. This function
/// recomputes every conservative total and performs the single unclipped
/// full-bound insert.
fn admit_reservation(
    connection: &Connection,
    authority: &Authority,
    identity: AdmissionIdentity,
    cost_bound: &AnalysisRequestCostBound,
    event_detail: Value,
) -> Result<AnalysisRequestRecord> {
    let totals = totals_for_scope(connection, &identity.budget_scope_id)?;
    ensure!(
        totals.active_requests < MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY,
        "analysis budget authority already has {MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY} requests in flight"
    );
    let untracked = untracked_totals(connection, &identity.budget_scope_id, None)?;
    let committed = authority.legacy_confirmed_cost_usd
        + authority.legacy_uncertain_usage_reserve_usd
        + untracked.reserve
        + totals.committed();
    let available = (authority.budget_usd - committed).max(0.0);
    let requested = cost_bound.maximum_cost_usd;
    ensure!(
        requested.is_finite() && requested > 0.0,
        "analysis request cost bound must be finite and greater than zero"
    );
    if available + 1e-12 < requested {
        return Err(AnalysisBudgetExhausted {
            available_usd: available,
            requested_usd: requested,
        }
        .into());
    }

    let now = crate::now();
    connection.execute(
        "INSERT INTO analysis_request_reservations(
            request_key,budget_scope_id,project_id,policy_id,run_id,operation,attempt_base_key,
            attempt_epoch,requested_provider,requested_model,policy_fingerprint,payload_fingerprint,
            source_fingerprint,request_fingerprint,cost_bound_fingerprint,cost_bound_json,
            lifecycle,reserved_cost_usd,created_at,updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,
                'reserved',?17,?18,?18)",
        params![
            identity.request_key,
            identity.budget_scope_id,
            identity.project_id,
            identity.policy_id,
            identity.run_id,
            identity.operation,
            identity.attempt_base_key,
            identity.attempt_epoch as i64,
            identity.requested_provider,
            identity.requested_model,
            identity.policy_fingerprint,
            identity.payload_fingerprint,
            identity.source_fingerprint,
            identity.request_fingerprint,
            identity.cost_bound_fingerprint,
            serde_json::to_string(cost_bound)?,
            requested,
            now,
        ],
    )?;
    append_event(
        connection,
        &identity.request_key,
        "reserved",
        merged_event_detail(
            json!({
                "attemptEpoch": identity.attempt_epoch,
                "reservedCostUsd": requested,
                "budgetScopeId": identity.budget_scope_id,
                "legacyConfirmedCostUsd": authority.legacy_confirmed_cost_usd,
                "legacyUncertainUsageReserveUsd": authority.legacy_uncertain_usage_reserve_usd,
                "confirmedCostUsd": totals.confirmed,
                "activeReservedCostUsd": totals.active_reserved,
                "uncertainUsageReserveUsd": totals.uncertain,
                "costBoundFingerprint": identity.cost_bound_fingerprint,
                "costBound": cost_bound,
            }),
            event_detail,
        ),
    )?;
    load_request(connection, &identity.request_key)?
        .ok_or_else(|| anyhow!("reserved analysis request disappeared"))
}

/// Atomically check the full conservative bound against the one canonical
/// authority, then append a reservation. The bound is never clipped.
pub(crate) fn reserve(
    connection: &mut Connection,
    context: &AnalysisAccountingContext,
    model: &str,
    payload_fingerprint: &str,
    cost_bound: &AnalysisRequestCostBound,
) -> Result<AnalysisRequestRecord> {
    ensure!(
        cost_bound.published_token_tariff.is_none(),
        "published token pricing requires its explicit typed transport admission"
    );
    reserve_with_family(
        connection,
        context,
        model,
        payload_fingerprint,
        cost_bound,
        PolicyRequestFamily::Vlm { cost_bound },
    )
}

/// Native-audio admission uses the same canonical transaction but carries a
/// typed native request/quote proof so chat and native policies cannot cross.
pub(crate) fn reserve_native_stt(
    connection: &mut Connection,
    context: &AnalysisAccountingContext,
    request: &NativeSttRequest,
    audio: &NativeSttAudioWindow,
    payload_fingerprint: &str,
    cost_bound: &AnalysisRequestCostBound,
) -> Result<AnalysisRequestRecord> {
    ensure!(
        !context.policy_id.trim().is_empty(),
        "native STT admission requires a non-empty canonical policy identity"
    );
    ensure!(
        !context.run_id.trim().is_empty(),
        "native STT admission requires a real running accounted run"
    );
    ensure!(
        matches!(
            context.operation.as_str(),
            "pilot" | "native_speech_transcription"
        ),
        "native STT accounted admission requires the pilot or native speech transcription operation"
    );
    reserve_with_family(
        connection,
        context,
        request.model(),
        payload_fingerprint,
        cost_bound,
        PolicyRequestFamily::NativeStt {
            request,
            audio,
            cost_bound,
        },
    )
}

pub(crate) fn reserve_native_video(
    connection: &mut Connection,
    context: &AnalysisAccountingContext,
    media: &crate::NativeVideoMedia,
    evidence_id: &str,
    payload_fingerprint: &str,
    cost_bound: &AnalysisRequestCostBound,
) -> Result<AnalysisRequestRecord> {
    ensure!(
        !context.policy_id.is_empty()
            && !context.run_id.is_empty()
            && matches!(context.operation.as_str(), "pilot" | "native_video_review"),
        "native video requires canonical authorization and a real owned native run"
    );
    reserve_with_family(
        connection,
        context,
        crate::analysis::native_video::NATIVE_VIDEO_MODEL,
        payload_fingerprint,
        cost_bound,
        PolicyRequestFamily::NativeVideo {
            media,
            evidence_id,
            cost_bound,
        },
    )
}

fn reserve_with_family(
    connection: &mut Connection,
    context: &AnalysisAccountingContext,
    model: &str,
    payload_fingerprint: &str,
    cost_bound: &AnalysisRequestCostBound,
    family: PolicyRequestFamily<'_>,
) -> Result<AnalysisRequestRecord> {
    context.validate_request(model, payload_fingerprint)?;
    cost_bound.validate()?;
    let cost_bound_fingerprint = analysis_request_cost_bound_fingerprint(cost_bound)?;
    let base = attempt_base_key(context, model, payload_fingerprint, &cost_bound_fingerprint)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let scope = context.resolved_budget_scope_id()?;
    let authority = load_authority_optional(&transaction, &scope)?.ok_or_else(|| {
        anyhow!(
            "analysis budget authority is not initialized; snapshot legacy evidence before creating runtime run/pilot records"
        )
    })?;
    verify_authority(
        &authority,
        &context.project_id,
        &context.policy_id,
        &context.policy_fingerprint,
        &context.provider,
        context.budget_usd,
        &scope,
    )?;
    let owner = OwnerIdentity::from_context(context, model);
    validate_owner_identity(&transaction, &owner, family)?;
    if matches!(family, PolicyRequestFamily::Vlm { .. }) && !context.policy_id.is_empty() {
        // Serialize replacement-policy admission with all prior active or
        // uncertain attempts for this exact source. Because this check shares
        // the immediate reservation transaction, either the old attempt or
        // the replacement wins; the other cannot race past the drain fence.
        let conflicting_policy: Option<String> = transaction
            .query_row(
                "SELECT policy_id FROM analysis_request_reservations WHERE project_id=?1 AND operation IN ('pilot','observation','moment_discovery') AND source_fingerprint=?2 AND policy_id<>?3 AND lifecycle IN ('reserved','dispatched','uncertain') ORDER BY created_at LIMIT 1",
                params![context.project_id, context.source_fingerprint, context.policy_id],
                |row| row.get(0),
            )
            .optional()?;
        ensure!(
            conflicting_policy.is_none(),
            "a prior policy has active or unresolved paid work for this source; drain active requests and reconcile uncertain usage before replacement"
        );
    }
    if matches!(family, PolicyRequestFamily::NativeStt { .. }) {
        ensure_no_foreign_native_unresolved_for_source(&transaction, context)?;
    }
    if matches!(family, PolicyRequestFamily::NativeVideo { .. }) {
        let conflicting: bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM analysis_request_reservations WHERE project_id=?1 AND source_fingerprint=?2 AND policy_id<>?3 AND lifecycle IN ('reserved','dispatched','uncertain'))",
            params![context.project_id,context.source_fingerprint,context.policy_id],|r|r.get(0))?;
        ensure!(
            !conflicting,
            "a prior policy has active or unresolved native video usage for this source"
        );
    }
    if let PolicyRequestFamily::NativeVideo {
        media, evidence_id, ..
    } = family
    {
        // A replacement media/tool recipe changes the full source identity.
        // The stable asset fence still retains prior native uncertainty, so
        // those changes cannot turn an interrupted request into a fresh send.
        let prior_asset_work:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM native_video_calls c JOIN analysis_request_reservations r ON r.request_key=c.request_key WHERE c.project_id=?1 AND c.asset_id=?2 AND c.policy_id<>?3 AND r.lifecycle IN ('reserved','dispatched','uncertain'))",params![context.project_id,media.view().asset_id,context.policy_id],|r|r.get(0))?;
        ensure!(
            !prior_asset_work,
            "prior native video asset work is active or unresolved; reconcile its canonical receipt before a replacement policy"
        );
        let (run,policy_id,asset,state,request_json):(String,String,String,String,String)=transaction.query_row("SELECT run_id,policy_id,asset_id,state,request_json FROM native_video_calls WHERE evidence_id=?1 AND project_id=?2",params![evidence_id,context.project_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        ensure!(
            run == context.run_id
                && policy_id == context.policy_id
                && asset == media.view().asset_id
                && state == "prepared"
                && request_json == serde_json::to_string(media.request())?,
            "native request does not own its exact durable window"
        );
        let (policy, _) = crate::analysis::run_policy::load_run_authorization_on_connection(
            &transaction,
            &context.policy_id,
        )?;
        let RunPolicyParameters::NativeVideoReview { options } = &policy.policy.parameters else {
            bail!("native parameters missing");
        };
        let (prior_requests, prior_seconds):(u32,f64)=transaction.query_row("SELECT COUNT(*),COALESCE(SUM(json_extract(c.request_json,'$.sourceOutSeconds')-json_extract(c.request_json,'$.sourceInSeconds')),0) FROM native_video_calls c JOIN analysis_request_reservations r ON r.request_key=c.request_key WHERE c.project_id=?1 AND c.policy_id=?2 AND r.lifecycle<>'released'",params![context.project_id,context.policy_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        ensure!(
            prior_requests < options.max_requests
                && prior_seconds + media.request().source_out_seconds
                    - media.request().source_in_seconds
                    <= f64::from(options.max_media_seconds) + 1e-7,
            "native request/media limit reached"
        );
    }
    if let Some(stop_reason) =
        persist_untracked_denial_boundary(&transaction, &authority, context.budget_usd)?
    {
        // The evidence refresh itself is durable evidence. Commit it before
        // denying admission so a SQLite error return can never roll the newly
        // discovered unknown floor/stop state back out of existence.
        transaction.commit()?;
        bail!("analysis budget authority dispatch is stopped: {stop_reason}");
    }

    let prior = load_attempts(&transaction, &base)?;
    ensure!(
        !prior.iter().any(|record| record.lifecycle.is_active()),
        "an identical analysis request already exists; it is already reserved or in flight; refusing a duplicate paid dispatch"
    );
    let next_epoch = next_attempt_epoch(context, &prior, &transaction)?;
    let key = request_key(&base, next_epoch);
    let record = admit_reservation(
        &transaction,
        &authority,
        AdmissionIdentity {
            request_key: key,
            budget_scope_id: scope,
            project_id: context.project_id.clone(),
            policy_id: context.policy_id.clone(),
            run_id: context.run_id.clone(),
            operation: context.operation.clone(),
            attempt_base_key: base.clone(),
            attempt_epoch: next_epoch,
            requested_provider: context.provider.clone(),
            requested_model: model.to_string(),
            policy_fingerprint: context.policy_fingerprint.clone(),
            payload_fingerprint: payload_fingerprint.to_string(),
            source_fingerprint: context.source_fingerprint.clone(),
            request_fingerprint: base,
            cost_bound_fingerprint,
        },
        cost_bound,
        json!({}),
    )?;
    if let PolicyRequestFamily::NativeVideo { evidence_id, .. } = family {
        let linked=transaction.execute("UPDATE native_video_calls SET request_key=?3,state='reserved',updated_at=?4 WHERE evidence_id=?1 AND run_id=?2 AND state='prepared' AND request_key IS NULL",params![evidence_id,context.run_id,record.request_key,crate::now()])?;
        ensure!(
            linked == 1,
            "native reservation lost durable call ownership"
        );
    }
    transaction.commit()?;
    Ok(record)
}

/// Validate the durable authorization and latest-epoch fence without writing.
/// Recovery preparation uses this before creating any new run row; admission
/// repeats it inside its own IMMEDIATE transaction.
pub(crate) fn validate_retry_authorization(
    connection: &Connection,
    project_id: &str,
    prior_request_key: &str,
    expected_next_attempt_epoch: u32,
) -> Result<AnalysisRequestRecord> {
    let prior = load_request(connection, prior_request_key)?
        .ok_or_else(|| anyhow!("prior analysis request not found"))?;
    ensure!(
        prior.project_id == project_id,
        "prior analysis request does not belong to this project"
    );
    verify_targeted_retry_authorization(connection, &prior, expected_next_attempt_epoch)?;
    Ok(prior)
}

/// Validate the exact original payload/model and current provider cost contract
/// against a durable retry authorization. This is a read-only preflight; the
/// targeted admission transaction repeats these checks while it consumes the
/// expected epoch.
pub(crate) fn validate_targeted_retry_claim(
    connection: &Connection,
    claim: &TargetedRetryClaim<'_>,
) -> Result<AnalysisRequestRecord> {
    let prior = validate_retry_authorization(
        connection,
        claim.project_id,
        claim.prior_request_key,
        claim.expected_next_attempt_epoch,
    )?;
    ensure!(
        prior.requested_model == claim.model,
        "targeted retry requested model differs from the immutable prior request"
    );
    ensure!(
        prior.payload_fingerprint == claim.payload_fingerprint,
        "targeted retry payload does not match the original request payload fingerprint"
    );
    ensure!(
        prior.cost_bound_fingerprint == claim.cost_bound_fingerprint,
        "targeted retry provider cost contract differs from the immutable prior request"
    );
    let authority = load_authority(connection, &prior.budget_scope_id)?;
    verify_authority(
        &authority,
        &prior.project_id,
        &prior.policy_id,
        &prior.policy_fingerprint,
        &prior.provider,
        authority.budget_usd,
        &prior.budget_scope_id,
    )?;
    ensure!(
        !authority.dispatch_stopped,
        "analysis budget authority dispatch is stopped: {}",
        authority.stop_reason
    );
    if let Some(run_id) = claim.run_id {
        ensure!(
            run_id != prior.run_id,
            "targeted retry requires a new owned run, not the prior request's run"
        );
    }
    validate_owner_identity(
        connection,
        &OwnerIdentity::from_targeted_retry(
            &prior,
            claim.run_id.unwrap_or(""),
            authority.budget_usd,
        ),
        PolicyRequestFamily::Vlm {
            cost_bound: claim.cost_bound,
        },
    )?;
    Ok(prior)
}

/// Atomically consume one exact durable retry authorization for the prior
/// request and expected next epoch. The prior immutable identity supplies the
/// attempt base, source, policy and provider; callers cannot substitute a new
/// intended key, scope or payload. All earlier confirmed and uncertain usage
/// remains in the same balance calculation.
pub(crate) fn reserve_targeted_retry(
    connection: &mut Connection,
    admission: &AnalysisTargetedRetryAdmission,
    model: &str,
    payload_fingerprint: &str,
    cost_bound: &AnalysisRequestCostBound,
) -> Result<AnalysisRequestRecord> {
    admission.validate()?;
    cost_bound.validate()?;
    let cost_bound_fingerprint = analysis_request_cost_bound_fingerprint(cost_bound)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let prior = validate_targeted_retry_claim(
        &transaction,
        &TargetedRetryClaim {
            project_id: &admission.project_id,
            prior_request_key: &admission.prior_request_key,
            expected_next_attempt_epoch: admission.expected_next_attempt_epoch,
            run_id: Some(&admission.run_id),
            model,
            payload_fingerprint,
            cost_bound_fingerprint: &cost_bound_fingerprint,
            cost_bound,
        },
    )?;
    let authority = load_authority(&transaction, &prior.budget_scope_id)?;
    if let Some(stop_reason) =
        persist_untracked_denial_boundary(&transaction, &authority, authority.budget_usd)?
    {
        transaction.commit()?;
        bail!("analysis budget authority dispatch is stopped: {stop_reason}");
    }
    let next_epoch = admission.expected_next_attempt_epoch;
    let key = request_key(&prior.attempt_base_key, next_epoch);
    let record = admit_reservation(
        &transaction,
        &authority,
        AdmissionIdentity {
            request_key: key,
            budget_scope_id: prior.budget_scope_id.clone(),
            project_id: prior.project_id.clone(),
            policy_id: prior.policy_id.clone(),
            run_id: admission.run_id.clone(),
            operation: prior.operation.clone(),
            attempt_base_key: prior.attempt_base_key.clone(),
            attempt_epoch: next_epoch,
            requested_provider: prior.provider.clone(),
            requested_model: prior.requested_model.clone(),
            policy_fingerprint: prior.policy_fingerprint.clone(),
            payload_fingerprint: prior.payload_fingerprint.clone(),
            source_fingerprint: prior.source_fingerprint.clone(),
            request_fingerprint: prior.request_fingerprint.clone(),
            cost_bound_fingerprint,
        },
        cost_bound,
        json!({
            "priorRequestKey": prior.request_key,
            "attemptBaseKey": prior.attempt_base_key,
            "targetedRetry": true,
        }),
    )?;
    transaction.commit()?;
    Ok(record)
}

/// Prove the authorized retry is for the latest attempt and exactly the next
/// bounded epoch. A competing consumer or stale authorization is denied.
fn verify_targeted_retry_authorization(
    connection: &Connection,
    prior: &AnalysisRequestRecord,
    expected_next_attempt_epoch: u32,
) -> Result<()> {
    ensure!(
        prior.lifecycle.is_terminal(),
        "prior analysis request is still active; wait for or reconcile its outcome before retry"
    );
    let attempts = load_attempts(connection, &prior.attempt_base_key)?;
    let latest = attempts
        .last()
        .ok_or_else(|| anyhow!("analysis request attempt history disappeared"))?;
    ensure!(
        latest.request_key == prior.request_key && latest.attempt_epoch == prior.attempt_epoch,
        "stale retry request: a newer attempt already exists for this request identity"
    );
    ensure!(
        attempts.len() < MAX_REQUEST_ATTEMPTS as usize,
        "analysis request retry limit reached"
    );
    ensure!(
        expected_next_attempt_epoch == prior.attempt_epoch + 1,
        "targeted retry expected epoch must be exactly the next attempt epoch"
    );
    let epoch_exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM analysis_request_reservations
         WHERE attempt_base_key=?1 AND attempt_epoch=?2)",
        params![prior.attempt_base_key, expected_next_attempt_epoch as i64],
        |row| row.get(0),
    )?;
    ensure!(
        !epoch_exists,
        "targeted retry attempt epoch already exists; stale authorization is rejected"
    );
    let authorized: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM analysis_request_events
            WHERE request_key=?1 AND event_kind='retry_authorized'
              AND json_extract(detail_json,'$.nextAttemptEpoch')=?2
        )",
        params![prior.request_key, expected_next_attempt_epoch as i64],
        |row| row.get(0),
    )?;
    ensure!(
        authorized,
        "explicit retry has not been authorized for attempt epoch {expected_next_attempt_epoch}"
    );
    Ok(())
}

/// Return every attempt for the immutable base identity, oldest first. Used by
/// inspection and retry preparation; never includes prompt/frame content.
pub(crate) fn attempt_history(
    connection: &Connection,
    request_key: &str,
) -> Result<Vec<AnalysisRequestRecord>> {
    let request = load_request(connection, request_key)?
        .ok_or_else(|| anyhow!("analysis request not found"))?;
    load_attempts(connection, &request.attempt_base_key)
}

/// Validate and return the next attempt epoch. A retry can only proceed with a
/// durable authorization event attached to the immediately prior request and
/// its exact next epoch; the caller's `explicit_retry` flag alone is never
/// sufficient. The subsequent IMMEDIATE insert and the unique
/// `(attempt_base_key, attempt_epoch)` constraint fence competing consumers.
fn next_attempt_epoch(
    context: &AnalysisAccountingContext,
    prior: &[AnalysisRequestRecord],
    connection: &Connection,
) -> Result<u32> {
    let Some(last) = prior.last() else {
        return Ok(0);
    };
    ensure!(
        context.explicit_retry,
        match last.lifecycle {
            AnalysisRequestLifecycle::Completed =>
                "a matching request already has a completed provider receipt; explicit retry authorization is required",
            AnalysisRequestLifecycle::Uncertain =>
                "a matching request outcome is uncertain; inspect/reconcile its receipt before explicit retry",
            AnalysisRequestLifecycle::Released =>
                "a matching request was released; explicit retry authorization is required",
            _ => "a matching request already exists; explicit retry authorization is required",
        }
    );
    ensure!(
        last.lifecycle.is_terminal(),
        "an identical analysis request is still active; refusing a duplicate paid dispatch"
    );
    ensure!(
        prior.len() < MAX_REQUEST_ATTEMPTS as usize,
        "analysis request retry limit reached; new evidence or a new policy is required"
    );
    let next_epoch = last.attempt_epoch + 1;
    let authorized: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM analysis_request_events
            WHERE request_key=?1 AND event_kind='retry_authorized'
              AND json_extract(detail_json,'$.nextAttemptEpoch')=?2
        )",
        params![last.request_key, next_epoch as i64],
        |row| row.get(0),
    )?;
    ensure!(
        authorized,
        "explicit retry has not been authorized for attempt epoch {next_epoch}"
    );
    Ok(next_epoch)
}

/// Commit the durable dispatch marker immediately before transport I/O. The
/// marker is the only local evidence boundary that turns a later generic
/// failure into uncertainty rather than a releasable pre-upload failure.
pub(crate) fn mark_dispatched(
    connection: &mut Connection,
    key: &str,
) -> Result<AnalysisRequestRecord> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current =
        load_request(&transaction, key)?.ok_or_else(|| anyhow!("analysis request not found"))?;
    if current.lifecycle == AnalysisRequestLifecycle::Dispatched {
        transaction.commit()?;
        return Ok(current);
    }
    ensure!(
        current.lifecycle == AnalysisRequestLifecycle::Reserved && current.dispatch_at.is_none(),
        "analysis request cannot be marked dispatched from lifecycle {}",
        current.lifecycle.as_str()
    );
    let authority = load_authority(&transaction, &current.budget_scope_id)?;
    if let Some(stop_reason) =
        persist_untracked_denial_boundary(&transaction, &authority, authority.budget_usd)?
    {
        // Preserve the discovered unknown floor/stop state before denying the
        // dispatch marker. The caller may then release its still-undispatched
        // reservation, but cannot transmit.
        transaction.commit()?;
        bail!("analysis budget authority dispatch is stopped: {stop_reason}");
    }
    let now = crate::now();
    transaction.execute(
        "UPDATE analysis_request_reservations
         SET lifecycle='dispatched',dispatch_at=?2,updated_at=?2
         WHERE request_key=?1",
        params![key, now],
    )?;
    append_event(
        &transaction,
        key,
        "dispatched",
        json!({
            "dispatchMarkerCommitted": true,
            "reservedCostUsd": current.reserved_cost_usd,
        }),
    )?;
    let record = load_request(&transaction, key)?
        .ok_or_else(|| anyhow!("dispatched analysis request disappeared"))?;
    transaction.commit()?;
    Ok(record)
}

/// Release a reservation only while the durable dispatch marker is absent.
/// Failure-code text is retained as bounded evidence but is never itself proof
/// that no upload occurred.
pub(crate) fn release_before_dispatch(
    connection: &mut Connection,
    key: &str,
    failure_code: &str,
) -> Result<AnalysisRequestRecord> {
    let safe_failure = bounded_failure_code(failure_code);
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current =
        load_request(&transaction, key)?.ok_or_else(|| anyhow!("analysis request not found"))?;
    if current.lifecycle == AnalysisRequestLifecycle::Released {
        ensure!(
            current.failure_code == safe_failure || current.failure_code.is_empty(),
            "released analysis request has conflicting failure evidence"
        );
        transaction.commit()?;
        return Ok(current);
    }
    ensure!(
        matches!(
            current.lifecycle,
            AnalysisRequestLifecycle::Reserved | AnalysisRequestLifecycle::Uncertain
        ) && current.dispatch_at.is_none(),
        "dispatch marker exists; local evidence cannot prove that no upload occurred"
    );
    let now = crate::now();
    transaction.execute(
        "UPDATE analysis_request_reservations
         SET lifecycle='released',reserved_cost_usd=0.0,reported_cost_usd=NULL,
             confirmed_cost_usd=0.0,uncertain_cost_usd=0.0,failure_code=?2,
             settled_at=?3,updated_at=?3
         WHERE request_key=?1",
        params![key, safe_failure, now],
    )?;
    append_event(
        &transaction,
        key,
        "local_release",
        json!({
            "failureCode": safe_failure,
            "reliableEvidence": "dispatch_marker_absent",
        }),
    )?;
    let record = load_request(&transaction, key)?
        .ok_or_else(|| anyhow!("released analysis request disappeared"))?;
    transaction.commit()?;
    Ok(record)
}

/// Retain honest uncertainty for a request that may have reached the provider.
/// The conservative admission bound becomes the unresolved-usage reserve until
/// receipt evidence is reconciled by the follow-up slice.
pub(crate) fn mark_uncertain(
    connection: &mut Connection,
    key: &str,
    failure_code: &str,
) -> Result<AnalysisRequestRecord> {
    let safe_failure = bounded_failure_code(failure_code);
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current =
        load_request(&transaction, key)?.ok_or_else(|| anyhow!("analysis request not found"))?;
    if current.lifecycle == AnalysisRequestLifecycle::Uncertain {
        ensure!(
            current.failure_code == safe_failure || current.failure_code.is_empty(),
            "uncertain analysis request has conflicting failure evidence"
        );
        transaction.commit()?;
        return Ok(current);
    }
    ensure!(
        current.lifecycle.is_active(),
        "analysis request cannot become uncertain from lifecycle {}",
        current.lifecycle.as_str()
    );
    let uncertain = current
        .reported_cost_usd
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
        .map_or(current.reserved_cost_usd, |cost| {
            current.reserved_cost_usd.max(cost)
        });
    let now = crate::now();
    transaction.execute(
        "UPDATE analysis_request_reservations
         SET lifecycle='uncertain',confirmed_cost_usd=0.0,estimated_cost_usd=NULL,uncertain_cost_usd=?2,
             failure_code=?3,settled_at=?4,updated_at=?4
         WHERE request_key=?1",
        params![key, uncertain, safe_failure, now],
    )?;
    append_event(
        &transaction,
        key,
        "uncertain",
        json!({
            "failureCode": safe_failure,
            "uncertainUsageReserveUsd": uncertain,
            "reportedCostUsd": current.reported_cost_usd,
        }),
    )?;
    let record = load_request(&transaction, key)?
        .ok_or_else(|| anyhow!("uncertain analysis request disappeared"))?;
    transaction.commit()?;
    Ok(record)
}

/// Link a result-cache identity to the canonical paid requests that produced
/// it. The event is append-only and never itself authorizes a transmission; it
/// lets later compatibility refresh distinguish a ledger-owned cache row from
/// genuinely untracked legacy work.
pub(crate) fn associate_cache_result(
    connection: &Connection,
    request_key: &str,
    cache_key: &str,
) -> Result<()> {
    ensure!(
        !request_key.trim().is_empty() && !cache_key.trim().is_empty(),
        "cache association requires request and cache identities"
    );
    let owned: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM analysis_request_reservations AS request
            JOIN analysis_chronology_suggestions AS cache ON cache.request_key=?2
            WHERE request.request_key=?1 AND request.operation=?3 AND request.run_id<>''
              AND request.project_id=cache.project_id AND cache.status='completed'
              AND json_extract(cache.result_json,'$.provenance.runId')=request.run_id
              AND EXISTS(SELECT 1 FROM json_each(cache.result_json,'$.provenance.requestKeys')
                         WHERE value=request.request_key)
        )",
        params![
            request_key,
            cache_key,
            super::model::CHRONOLOGY_SUGGESTIONS_OPERATION
        ],
        |row| row.get(0),
    )?;
    ensure!(
        owned,
        "cache association must match the producing project, operation, run and request"
    );
    let already: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM analysis_request_events
            WHERE request_key=?1 AND event_kind='cache_result'
              AND json_extract(detail_json,'$.cacheKey')=?2
         )",
        params![request_key, cache_key],
        |row| row.get(0),
    )?;
    if !already {
        append_event(
            connection,
            request_key,
            "cache_result",
            json!({"cacheKey": cache_key}),
        )?;
    }
    Ok(())
}

fn load_existing_receipts(connection: &Connection, key: &str) -> Result<Vec<ExistingReceipt>> {
    let mut statement = connection.prepare(
        "SELECT receipt_fingerprint,provider,provider_generation_id,actual_model,
                reported_cost_usd,usage_complete,
                (SELECT json_extract(e.detail_json,'$.usagePricing') FROM analysis_request_events e
                 WHERE e.request_key=receipts.request_key AND e.event_kind='receipt'
                   AND json_extract(e.detail_json,'$.receiptKey')=receipts.receipt_key
                 ORDER BY e.event_id DESC LIMIT 1)
         FROM analysis_provider_receipts receipts WHERE request_key=?1
         ORDER BY created_at ASC, receipt_key ASC",
    )?;
    let receipts = statement
        .query_map([key], |row| {
            Ok(ExistingReceipt {
                fingerprint: row.get(0)?,
                provider: row.get(1)?,
                generation: row.get(2)?,
                actual_model: row.get(3)?,
                reported_cost_usd: row.get(4)?,
                usage_complete: row.get::<_, i64>(5)? != 0,
                usage_pricing: row
                    .get::<_, Option<String>>(6)?
                    .map(|raw| {
                        serde_json::from_str(&raw).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                6,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })
                    })
                    .transpose()?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(receipts)
}

/// Return all immutable receipt evidence for one request, oldest first.
pub(crate) fn receipts(connection: &Connection, key: &str) -> Result<Vec<AnalysisProviderReceipt>> {
    let mut statement = connection.prepare(
        "SELECT receipt_key,request_key,provider,provider_generation_id,actual_model,
                reported_cost_usd,prompt_tokens,completion_tokens,usage_complete,
                receipt_fingerprint,created_at
         FROM analysis_provider_receipts WHERE request_key=?1
         ORDER BY created_at ASC, receipt_key ASC",
    )?;
    let receipts = statement
        .query_map([key], |row| {
            Ok(AnalysisProviderReceipt {
                receipt_key: row.get(0)?,
                request_key: row.get(1)?,
                provider: row.get(2)?,
                provider_generation_id: row.get(3)?,
                actual_model: row.get(4)?,
                reported_cost_usd: row.get(5)?,
                prompt_tokens: row.get::<_, Option<i64>>(6)?.map(|value| value as u32),
                completion_tokens: row.get::<_, Option<i64>>(7)?.map(|value| value as u32),
                usage_complete: row.get::<_, i64>(8)? != 0,
                receipt_fingerprint: row.get(9)?,
                created_at: row.get(10)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(receipts)
}

/// Generation IDs belong to the requested router/service. The optional actual
/// upstream provider is deliberately not part of this lookup, so missing or
/// differing upstream metadata cannot launder a duplicate generation.
fn enforce_generation_owner(
    connection: &Connection,
    key: &str,
    requested_provider: &str,
    generation_id: &str,
) -> Result<()> {
    if requested_provider.trim().is_empty() || generation_id.trim().is_empty() {
        return Ok(());
    }
    let duplicate_owner: Option<String> = connection
        .query_row(
            "SELECT receipts.request_key
             FROM analysis_provider_receipts AS receipts
             JOIN analysis_request_reservations AS requests
               ON requests.request_key = receipts.request_key
             WHERE requests.requested_provider=?1
               AND receipts.provider_generation_id=?2
               AND receipts.request_key<>?3
             LIMIT 1",
            params![requested_provider, generation_id, key],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(
        duplicate_owner.is_none(),
        "provider generation identity already belongs to a different analysis request; refusing duplicate paid receipt attribution"
    );
    Ok(())
}

fn insert_receipt(
    transaction: &rusqlite::Transaction<'_>,
    key: &str,
    evidence: &ProviderReceiptEvidence,
    fingerprint: &str,
    now: &str,
) -> Result<String> {
    let receipt_key = digest_bytes(&[
        key.as_bytes(),
        evidence.provider.as_bytes(),
        evidence.provider_generation_id.as_bytes(),
        fingerprint.as_bytes(),
    ]);
    transaction.execute(
        "INSERT INTO analysis_provider_receipts(
            receipt_key,request_key,provider,provider_generation_id,actual_model,
            reported_cost_usd,prompt_tokens,completion_tokens,usage_complete,
            receipt_fingerprint,created_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            receipt_key,
            key,
            evidence.provider,
            evidence.provider_generation_id,
            evidence.actual_model,
            evidence.reported_cost_usd,
            evidence.prompt_tokens.map(|value| value as i64),
            evidence.completion_tokens.map(|value| value as i64),
            evidence.usage_complete as i64,
            fingerprint,
            now,
        ],
    )?;
    Ok(receipt_key)
}

fn apply_receipt_settlement(
    transaction: &rusqlite::Transaction<'_>,
    current: &AnalysisRequestRecord,
    settlement: &super::settlement::ReceiptSettlement,
    now: &str,
) -> Result<()> {
    transaction.execute(
        "UPDATE analysis_request_reservations
         SET lifecycle=?2,actual_provider=?3,actual_model=?4,provider_generation_id=?5,
             reported_cost_usd=?6,confirmed_cost_usd=?7,uncertain_cost_usd=?8,
             failure_code=?9,settled_at=?10,updated_at=?10,estimated_cost_usd=?11
         WHERE request_key=?1",
        params![
            current.request_key,
            settlement.lifecycle.as_str(),
            settlement.actual_provider.as_deref(),
            settlement.actual_model.as_deref(),
            settlement.provider_generation_id.as_deref(),
            settlement.reported_cost_usd,
            settlement.confirmed_cost_usd,
            settlement.uncertain_usage_reserve_usd,
            settlement.failure_code,
            now,
            settlement.estimated_cost_usd,
        ],
    )?;
    Ok(())
}

fn stop_on_receipt_overrun(
    transaction: &rusqlite::Transaction<'_>,
    current: &AnalysisRequestRecord,
    evidence: &ProviderReceiptEvidence,
    settlement: &super::settlement::ReceiptSettlement,
    now: &str,
) -> Result<()> {
    if !settlement.observed_cost_exceeded_bound {
        return Ok(());
    }
    transaction.execute(
        "UPDATE analysis_budget_authorities
         SET dispatch_stopped=1,stop_reason='reported_cost_exceeded_reservation',updated_at=?2
         WHERE budget_scope_id=?1",
        params![current.budget_scope_id, now],
    )?;
    append_event(
        transaction,
        &current.request_key,
        "overrun",
        json!({
            "reservedCostUsd": current.reserved_cost_usd,
            "reportedCostUsd": evidence.reported_cost_usd,
            "confirmedCostUsd": settlement.confirmed_cost_usd,
            "uncertainUsageReserveUsd": settlement.uncertain_usage_reserve_usd,
            "usageComplete": evidence.usage_complete,
            "dispatchStopped": true,
            "note": "external provider charge exceeded the software admission bound",
        }),
    )?;
    Ok(())
}

/// Append immutable provider evidence and apply its settlement projection in one
/// `IMMEDIATE` transaction. Exact duplicate evidence is a no-op. A reserved or
/// locally released request can never be turned into a charged completion.
pub(crate) fn record_receipt(
    connection: &mut Connection,
    key: &str,
    evidence: &ProviderReceiptEvidence,
) -> Result<AnalysisRequestRecord> {
    evidence.validate()?;
    let fingerprint = digest_json(evidence)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current =
        load_request(&transaction, key)?.ok_or_else(|| anyhow!("analysis request not found"))?;
    let existing = load_existing_receipts(&transaction, key)?;
    match classify_receipt(&current, &existing, evidence, &fingerprint)? {
        ReceiptDisposition::Idempotent => {
            transaction.commit()?;
            return Ok(current);
        }
        ReceiptDisposition::Append => {}
    }
    enforce_generation_owner(
        &transaction,
        key,
        &current.provider,
        &evidence.provider_generation_id,
    )?;
    let settlement = settlement_for_receipt(&current, &existing, evidence)?;
    let now = crate::now();
    let receipt_key = insert_receipt(&transaction, key, evidence, &fingerprint, &now)?;
    stop_on_receipt_overrun(&transaction, &current, evidence, &settlement, &now)?;
    apply_receipt_settlement(&transaction, &current, &settlement, &now)?;
    let mut receipt_event = json!({
        "receiptKey": receipt_key,
        "receiptFingerprint": fingerprint,
        "usageComplete": evidence.usage_complete,
        "reportedCostUsd": evidence.reported_cost_usd,
        "confirmedCostUsd": settlement.confirmed_cost_usd,
        "uncertainUsageReserveUsd": settlement.uncertain_usage_reserve_usd,
        "metadataEnriched": settlement.metadata_enriched,
        "actualProvider": evidence.provider,
        "actualModel": evidence.actual_model,
        "providerGenerationId": evidence.provider_generation_id,
    });
    if let Some(seconds) = evidence.billed_audio_seconds
        && let Some(object) = receipt_event.as_object_mut()
    {
        // Native provider-billed duration is retained separately from the
        // local input window and the conservative admission reservation.
        object.insert("billedAudioSeconds".into(), json!(seconds));
    }
    if let Some(usage) = &evidence.usage_pricing {
        receipt_event["usagePricing"] = serde_json::to_value(usage)?;
    }
    if let Some(estimated) = settlement.estimated_cost_usd {
        receipt_event["estimatedCostUsd"] = json!(estimated);
        receipt_event["costBasis"] = json!("published_rate_estimate");
    }
    append_event(&transaction, key, "receipt", receipt_event)?;
    let record = load_request(&transaction, key)?
        .ok_or_else(|| anyhow!("receipted analysis request disappeared"))?;
    transaction.commit()?;
    Ok(record)
}

/// Explicitly authorize one bounded retry. Only the latest attempt of an exact
/// request identity can be authorized, and repeat authorization for the same
/// next epoch is idempotent. Prior receipts/charges/uncertainty are untouched.
pub(crate) fn authorize_retry(
    connection: &mut Connection,
    key: &str,
) -> Result<AnalysisRetryAuthorization> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current =
        load_request(&transaction, key)?.ok_or_else(|| anyhow!("analysis request not found"))?;
    ensure!(
        current.lifecycle.is_terminal(),
        "analysis request is still active; wait for or reconcile its outcome before retry"
    );
    let attempts = load_attempts(&transaction, &current.attempt_base_key)?;
    let latest = attempts
        .last()
        .ok_or_else(|| anyhow!("analysis request attempt history disappeared"))?;
    ensure!(
        latest.request_key == current.request_key,
        "stale retry authorization: a newer attempt already exists for this request identity"
    );
    ensure!(
        attempts.len() < MAX_REQUEST_ATTEMPTS as usize,
        "analysis request retry limit reached"
    );
    let next_epoch = current.attempt_epoch + 1;
    let already_reserved: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM analysis_request_reservations
         WHERE attempt_base_key=?1 AND attempt_epoch=?2)",
        params![current.attempt_base_key, next_epoch as i64],
        |row| row.get(0),
    )?;
    ensure!(
        !already_reserved,
        "attempt epoch {next_epoch} already exists; stale retry authorization is rejected"
    );
    let already_authorized: bool = transaction.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM analysis_request_events
            WHERE request_key=?1 AND event_kind='retry_authorized'
              AND json_extract(detail_json,'$.nextAttemptEpoch')=?2
        )",
        params![key, next_epoch as i64],
        |row| row.get(0),
    )?;
    if !already_authorized {
        append_event(
            &transaction,
            key,
            "retry_authorized",
            json!({
                "nextAttemptEpoch": next_epoch,
                "retainedUncertainCostUsd": current.uncertain_usage_reserve_usd,
                "priorReportedCostUsd": current.reported_cost_usd,
                "maxAttempts": MAX_REQUEST_ATTEMPTS,
            }),
        )?;
    }
    transaction.commit()?;
    Ok(AnalysisRetryAuthorization {
        original_request_key: current.request_key,
        attempt_base_key: current.attempt_base_key,
        next_attempt_epoch: next_epoch,
        retained_uncertain_cost_usd: current.uncertain_usage_reserve_usd,
        prior_reported_cost_usd: current.reported_cost_usd,
        attempts_already_recorded: attempts.len() as u32,
        max_attempts: MAX_REQUEST_ATTEMPTS,
    })
}

pub(crate) fn load_request(
    connection: &Connection,
    key: &str,
) -> Result<Option<AnalysisRequestRecord>> {
    let request_select = request_select(connection)?;
    connection
        .query_row(
            &format!("{request_select} WHERE request_key=?1"),
            [key],
            row_to_request,
        )
        .optional()
        .context("read analysis request")
}

/// Exact identity and settlement expectations for one already paid native
/// speech result. Callers that own a different artifact (canonical transcript or
/// bounded pilot evidence) share this proof instead of maintaining separate
/// financial/ownership SQL.
pub(crate) struct NativeSettledResultProof<'a> {
    pub project_id: &'a str,
    pub policy_id: &'a str,
    pub policy_fingerprint: &'a str,
    pub expected_provider_cost_bound_contract: &'a str,
    pub source_fingerprint: &'a str,
    pub requested_provider: &'a str,
    pub requested_model: &'a str,
    /// `Some` requires the request to belong to this exact run.
    pub run_id: Option<&'a str>,
    pub allowed_operations: &'a [&'a str],
}

/// Reuse the canonical accounting owner to prove that a native result is backed
/// by one immutable, settled provider request and receipt. This is a financial
/// and ownership proof, not a quality/capability pass and not permission to
/// spend; domain owners add their own payload/source mapping checks.
///
/// Unknown-cost receipts are accepted only through the durable `uncertain`
/// lifecycle with its honest unresolved-usage reserve. A caller cannot turn that
/// lifecycle into a clean financial pass here.
pub(crate) fn validate_native_settled_result_request(
    connection: &Connection,
    request_key: &str,
    proof: &NativeSettledResultProof<'_>,
) -> Result<AnalysisRequestRecord> {
    ensure!(
        !request_key.trim().is_empty()
            && !proof.project_id.trim().is_empty()
            && !proof.policy_id.trim().is_empty()
            && !proof.policy_fingerprint.trim().is_empty()
            && !proof
                .expected_provider_cost_bound_contract
                .trim()
                .is_empty()
            && !proof.source_fingerprint.trim().is_empty()
            && !proof.requested_provider.trim().is_empty()
            && !proof.requested_model.trim().is_empty()
            && !proof.allowed_operations.is_empty()
            && proof
                .allowed_operations
                .iter()
                .all(|operation| !operation.trim().is_empty()),
        "native result request proof is incomplete"
    );
    let request = load_request(connection, request_key)?
        .ok_or_else(|| anyhow!("native result accounting request was not found"))?;
    ensure!(
        request.project_id == proof.project_id
            && request.policy_id == proof.policy_id
            && request.policy_fingerprint == proof.policy_fingerprint,
        "native result accounting request policy/project ownership changed"
    );
    ensure!(
        proof
            .allowed_operations
            .iter()
            .any(|operation| *operation == request.operation),
        "native result accounting request is not an allowed distinct speech operation"
    );
    ensure!(
        request.source_fingerprint == proof.source_fingerprint
            && request.provider == proof.requested_provider
            && request.requested_model == proof.requested_model,
        "native result accounting request source/provider/model does not match its owner"
    );
    ensure!(
        !request.payload_fingerprint.trim().is_empty()
            && !request.request_fingerprint.trim().is_empty()
            && !request.cost_bound_fingerprint.trim().is_empty(),
        "native result accounting request identity proof is incomplete"
    );
    if let Some(run_id) = proof.run_id {
        ensure!(
            !run_id.trim().is_empty() && request.run_id == run_id,
            "native result accounting request is not owned by the expected run"
        );
    }
    ensure!(
        !request.run_id.trim().is_empty(),
        "native result accounting request has no owning accounted run"
    );
    ensure!(
        request.dispatch_at.is_some()
            && !matches!(
                request.lifecycle,
                AnalysisRequestLifecycle::Reserved
                    | AnalysisRequestLifecycle::Dispatched
                    | AnalysisRequestLifecycle::Released
            ),
        "native result cannot link an undispatched, active or released accounting request"
    );
    let receipts = receipts(connection, request_key)?;
    ensure!(
        !receipts.is_empty(),
        "native result accounting request has no immutable provider receipt"
    );
    let attempt_history = attempt_history(connection, request_key)?;
    ensure!(
        attempt_history
            .iter()
            .any(|attempt| attempt.request_key == request.request_key),
        "native result accounting request is absent from its immutable attempt history"
    );
    match request.lifecycle {
        AnalysisRequestLifecycle::Completed => {
            ensure!(
                request.settled_at.is_some()
                    && request.confirmed_cost_usd.is_finite()
                    && request.confirmed_cost_usd >= 0.0,
                "completed native result request is missing its settled receipt boundary"
            );
        }
        AnalysisRequestLifecycle::Uncertain => {
            ensure!(
                request.uncertain_usage_reserve_usd.is_finite()
                    && request.uncertain_usage_reserve_usd > 0.0,
                "uncertain native result request has no honest unresolved-usage reserve"
            );
        }
        AnalysisRequestLifecycle::Reserved
        | AnalysisRequestLifecycle::Dispatched
        | AnalysisRequestLifecycle::Released => unreachable!("rejected above"),
    }
    let cost_bound = request
        .cost_bound
        .as_ref()
        .ok_or_else(|| anyhow!("native result accounting request has no cost bound"))?;
    cost_bound.validate()?;
    ensure!(
        cost_bound.audio.is_some()
            && cost_bound.contract_version == proof.expected_provider_cost_bound_contract,
        "native result accounting request is not bound to the canonical native audio cost family"
    );
    let run: Option<(String, String, String)> = connection
        .query_row(
            "SELECT project_id,policy_id,accounting_contract FROM analysis_runs WHERE id=?1",
            [&request.run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let (run_project, run_policy, run_contract) =
        run.ok_or_else(|| anyhow!("native result accounting run does not exist"))?;
    ensure!(
        run_project == proof.project_id
            && run_policy == proof.policy_id
            && run_contract == ANALYSIS_ACCOUNTING_SCHEMA_VERSION,
        "native result accounting run ownership or contract does not match the frozen policy"
    );
    Ok(request)
}

/// Unresolved attempts for one exact source/operation. A generated replacement
/// policy must not bypass prior unknown paid work for the same asset.
pub(crate) fn unresolved_requests_for_source(
    connection: &Connection,
    project_id: &str,
    operation: &str,
    source_fingerprint: &str,
) -> Result<Vec<AnalysisRequestRecord>> {
    let request_select = request_select(connection)?;
    let mut statement = connection.prepare(&format!(
        "{request_select}
         WHERE project_id=?1 AND operation=?2 AND source_fingerprint=?3
           AND lifecycle IN ('reserved','dispatched','uncertain')
         ORDER BY created_at ASC, request_key ASC"
    ))?;
    statement
        .query_map(
            params![project_id, operation, source_fingerprint],
            row_to_request,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("list unresolved analysis requests for source")
}

/// Prior unresolved native speech work for one exact source identity. This
/// intentionally spans both native operations and every policy identity: a new
/// force/replacement policy may not convert unknown usage into a fresh miss.
pub(crate) fn native_unresolved_requests_for_source(
    connection: &Connection,
    project_id: &str,
    source_fingerprint: &str,
) -> Result<Vec<AnalysisRequestRecord>> {
    let request_select = request_select(connection)?;
    let mut statement = connection.prepare(&format!(
        "{request_select}
         WHERE project_id=?1 AND source_fingerprint=?2
           AND operation IN ('pilot','native_speech_transcription')
           AND lifecycle IN ('reserved','dispatched','uncertain')
         ORDER BY created_at ASC, request_key ASC"
    ))?;
    statement
        .query_map(params![project_id, source_fingerprint], row_to_request)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("list unresolved native speech requests for source")
}

pub(crate) fn request_keys_for_run(
    connection: &Connection,
    project_id: &str,
    operation: &str,
    run_id: &str,
) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT request_key FROM analysis_request_reservations
         WHERE project_id=?1 AND operation=?2 AND run_id=?3 ORDER BY request_key",
    )?;
    Ok(statement
        .query_map(params![project_id, operation, run_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(crate) fn list_requests(
    connection: &Connection,
    project_id: &str,
    policy_id: Option<&str>,
    limit: usize,
) -> Result<Vec<AnalysisRequestRecord>> {
    let request_select = request_select(connection)?;
    let mut statement = connection.prepare(&format!(
        "{request_select} WHERE project_id=?1 AND (?2='' OR policy_id=?2)
         ORDER BY created_at DESC, request_key DESC LIMIT ?3"
    ))?;
    let records = statement
        .query_map(
            params![project_id, policy_id.unwrap_or(""), limit as i64],
            row_to_request,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(records)
}

/// Find any native request that prevents treating an exact payload as never
/// attempted. Filter in SQLite without a global inspection-page limit, so old
/// paid/active work cannot be hidden by unrelated newer requests.
pub(crate) fn native_request_requiring_result_recovery(
    connection: &Connection,
    project_id: &str,
    policy_id: &str,
    source_fingerprint: &str,
    provider: &str,
    model: &str,
    payload_fingerprint: &str,
) -> Result<Option<AnalysisRequestRecord>> {
    let request_select = request_select(connection)?;
    connection
        .query_row(
            &format!(
                "{request_select} WHERE project_id=?1 AND policy_id=?2
           AND source_fingerprint=?3 AND requested_provider=?4 AND requested_model=?5
           AND payload_fingerprint=?6
           AND operation IN ('pilot','native_speech_transcription')
           AND (lifecycle!='released' OR dispatch_at IS NOT NULL)
         ORDER BY created_at ASC, request_key ASC LIMIT 1"
            ),
            params![
                project_id,
                policy_id,
                source_fingerprint,
                provider,
                model,
                payload_fingerprint
            ],
            row_to_request,
        )
        .optional()
        .context("find native request requiring typed-result recovery")
}

/// Exact scoped inspection query. It filters in SQLite rather than loading a
/// global page and discarding non-matching rows.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scoped_requests(
    connection: &Connection,
    project_id: &str,
    policy_id: Option<&str>,
    operation: Option<&str>,
    run_id: Option<&str>,
    request_key_filter: Option<&str>,
    limit: usize,
    offset: usize,
) -> Result<Vec<AnalysisRequestRecord>> {
    let request_select = request_select(connection)?;
    let mut statement = connection.prepare(&format!(
        "{request_select}
         WHERE project_id=?1
           AND (?2='' OR policy_id=?2)
           AND (?3='' OR operation=?3)
           AND (?4='' OR run_id=?4)
           AND (?5='' OR request_key=?5)
         ORDER BY created_at DESC, request_key ASC
         LIMIT ?6 OFFSET ?7"
    ))?;
    let requests = statement
        .query_map(
            params![
                project_id,
                policy_id.unwrap_or(""),
                operation.unwrap_or(""),
                run_id.unwrap_or(""),
                request_key_filter.unwrap_or(""),
                limit as i64,
                offset as i64,
            ],
            row_to_request,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(requests)
}

pub(crate) fn scoped_request_count(
    connection: &Connection,
    project_id: &str,
    policy_id: Option<&str>,
    operation: Option<&str>,
    run_id: Option<&str>,
    request_key_filter: Option<&str>,
) -> Result<usize> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM analysis_request_reservations
         WHERE project_id=?1
           AND (?2='' OR policy_id=?2)
           AND (?3='' OR operation=?3)
           AND (?4='' OR run_id=?4)
           AND (?5='' OR request_key=?5)",
        params![
            project_id,
            policy_id.unwrap_or(""),
            operation.unwrap_or(""),
            run_id.unwrap_or(""),
            request_key_filter.unwrap_or(""),
        ],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

/// Exact authority-scope ids matching the same project/policy/operation
/// selection, without fetching all authority rows or all request rows.
pub(crate) fn budget_scope_ids_for_selection(
    connection: &Connection,
    project_id: &str,
    policy_id: Option<&str>,
    operation: Option<&str>,
) -> Result<Vec<String>> {
    let operation_scope = operation
        .map(|operation| super::model::analysis_operation_budget_scope_id(project_id, operation));
    let mut statement = connection.prepare(
        "SELECT budget_scope_id FROM analysis_budget_authorities
         WHERE project_id=?1
           AND (?2='' OR policy_id=?2)
           AND (?2<>'' OR ?3='' OR budget_scope_id=?4)
         ORDER BY budget_scope_id ASC",
    )?;
    let scopes = statement
        .query_map(
            params![
                project_id,
                policy_id.unwrap_or(""),
                operation.unwrap_or(""),
                operation_scope.unwrap_or_default(),
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(scopes)
}

/// Exact budget scopes for a request/run-filtered inspection. This avoids
/// showing every authority in the project when the operator selected one
/// request or run.
pub(crate) fn budget_scope_ids_for_request_selection(
    connection: &Connection,
    project_id: &str,
    policy_id: Option<&str>,
    operation: Option<&str>,
    run_id: Option<&str>,
    request_key_filter: Option<&str>,
) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT DISTINCT budget_scope_id FROM analysis_request_reservations
         WHERE project_id=?1
           AND (?2='' OR policy_id=?2)
           AND (?3='' OR operation=?3)
           AND (?4='' OR run_id=?4)
           AND (?5='' OR request_key=?5)
         ORDER BY budget_scope_id ASC",
    )?;
    Ok(statement
        .query_map(
            params![
                project_id,
                policy_id.unwrap_or(""),
                operation.unwrap_or(""),
                run_id.unwrap_or(""),
                request_key_filter.unwrap_or(""),
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(crate) fn events(
    connection: &Connection,
    request_key: &str,
) -> Result<Vec<AnalysisRequestEvent>> {
    let mut statement = connection.prepare(
        "SELECT event_id,request_key,event_kind,detail_json,created_at
         FROM analysis_request_events WHERE request_key=?1 ORDER BY event_id ASC",
    )?;
    let events = statement
        .query_map([request_key], |row| {
            let detail: String = row.get(3)?;
            Ok(AnalysisRequestEvent {
                event_id: row.get(0)?,
                request_key: row.get(1)?,
                event_kind: row.get(2)?,
                detail: serde_json::from_str(&detail).unwrap_or_else(|_| json!({})),
                created_at: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(events)
}

pub(crate) fn budget_snapshot(
    connection: &Connection,
    scope: &str,
) -> Result<Option<AnalysisBudgetSnapshot>> {
    load_authority_optional(connection, scope)?
        .map(|authority| snapshot_from_authority(connection, &authority))
        .transpose()
}

pub(crate) fn authority_evidence(connection: &Connection, scope: &str) -> Result<Option<Value>> {
    connection
        .query_row(
            "SELECT legacy_evidence_json FROM analysis_budget_authorities
             WHERE budget_scope_id=?1",
            [scope],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("read analysis budget authority evidence")
        .map(|raw| {
            raw.map(|raw| {
                serde_json::from_str(&raw).unwrap_or_else(|_| json!({"unreadable": true}))
            })
        })
}

fn validate_authority_input(
    connection: &Connection,
    context: &AnalysisBudgetAuthorityContext,
    scope: &str,
) -> Result<()> {
    ensure!(
        context.resolved_budget_scope_id()? == scope,
        "analysis budget authority scope changed during validation"
    );
    let project_exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
        [&context.project_id],
        |row| row.get(0),
    )?;
    ensure!(project_exists, "accounting project does not exist");
    if !context.policy_id.trim().is_empty() {
        let policy = load_authorized_policy(
            connection,
            &context.project_id,
            &context.policy_id,
            &context.policy_fingerprint,
            &context.provider,
            context.budget_usd,
            PolicyRequestFamily::Any,
        )?;
        ensure!(
            policy.policy_id == context.policy_id,
            "analysis policy authorization identity changed"
        );
    }
    Ok(())
}

/// Atomic source fence for every genuinely new native paid window. Native pilot
/// and bulk requests share a source identity but may be created under different
/// policy/force identities; unresolved prior native work must remain an explicit
/// recovery obligation instead of being hidden by a replacement policy.
fn ensure_no_foreign_native_unresolved_for_source(
    connection: &Connection,
    context: &AnalysisAccountingContext,
) -> Result<()> {
    let prior = native_unresolved_requests_for_source(
        connection,
        &context.project_id,
        &context.source_fingerprint,
    )?;
    for request in prior {
        ensure!(
            request.policy_id == context.policy_id,
            "a prior native speech request has unresolved usage; reconcile it and authorize the exact prior request before a replacement policy can send"
        );
    }
    Ok(())
}

/// Validate immutable request-owner identities against the canonical project,
/// active policy authorization and owned run. Fresh admission and targeted
/// retry both call this exact function; an empty run id is only accepted by
/// lower-level operation callers that do not use the accounted provider
/// boundary, while a provided run id must be the real accounted running row.
fn validate_owner_identity(
    connection: &Connection,
    identity: &OwnerIdentity<'_>,
    family: PolicyRequestFamily<'_>,
) -> Result<()> {
    if !identity.policy_id.trim().is_empty() {
        let policy = load_authorized_policy(
            connection,
            identity.project_id,
            identity.policy_id,
            identity.policy_fingerprint,
            identity.provider,
            identity.budget_usd,
            family,
        )?;
        ensure!(
            policy
                .allowed_models
                .iter()
                .any(|allowed| allowed == identity.model)
                || policy.primary_model == identity.model,
            "analysis accounting model is outside the frozen authorized model allowlist"
        );
        let authorized_scope = policy
            .scope
            .iter()
            .find(|scope| scope.source_identity == identity.source_fingerprint)
            .ok_or_else(|| {
                anyhow!(
                    "analysis accounting source identity is outside the frozen authorized scope"
                )
            })?;
        if let PolicyRequestFamily::NativeStt { audio, .. } = family {
            ensure!(
                identity.intended_attempt_key
                    == format!("{}:{}", policy.kind.as_str(), authorized_scope.asset_id),
                "native STT intended request key is not the stable policy/source identity"
            );
            validate_native_audio_source_bounds(audio, authorized_scope.source_duration_ms)?;
        }
        if let PolicyRequestFamily::NativeVideo { media, .. } = family {
            media.validate_policy_binding(
                &policy,
                identity.source_fingerprint,
                identity.intended_attempt_key,
            )?;
        }
    }
    if !identity.run_id.trim().is_empty() {
        let run: Option<(String, String, String, String, String)> = connection
            .query_row(
                "SELECT project_id,policy_id,accounting_contract,status,stage_reached
                 FROM analysis_runs WHERE id=?1",
                [identity.run_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let (run_project, run_policy, accounting_contract, status, stage_reached) =
            run.ok_or_else(|| anyhow!("analysis run does not exist"))?;
        ensure!(
            run_project == identity.project_id,
            "analysis run belongs to a different project"
        );
        if !identity.policy_id.trim().is_empty() {
            ensure!(
                run_policy == identity.policy_id,
                "analysis run identity does not belong to the frozen policy/project authority"
            );
        }
        ensure!(
            accounting_contract == ANALYSIS_ACCOUNTING_SCHEMA_VERSION,
            "analysis run predates durable request accounting; a real accounted run is required"
        );
        ensure!(
            status == "running",
            "analysis request requires a currently running owned analysis run"
        );
        if matches!(
            family,
            PolicyRequestFamily::NativeStt { .. } | PolicyRequestFamily::NativeVideo { .. }
        ) {
            ensure!(
                !identity.operation.trim().is_empty() && stage_reached == identity.operation,
                "native STT request requires an owned run created for its exact native operation"
            );
        }
    }
    Ok(())
}

fn load_authorized_policy(
    connection: &Connection,
    project_id: &str,
    policy_id: &str,
    policy_fingerprint: &str,
    provider: &str,
    budget_usd: f64,
    family: PolicyRequestFamily<'_>,
) -> Result<crate::RunPolicy> {
    let authorization: Option<(String, String, String)> = connection
        .query_row(
            "SELECT project_id,policy_json,accounting_contract
             FROM analysis_run_authorizations WHERE policy_id=?1",
            [policy_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let (authorized_project, policy_json, accounting_contract) = authorization
        .ok_or_else(|| anyhow!("analysis policy {policy_id} is not canonically authorized"))?;
    ensure!(
        accounting_contract == ANALYSIS_ACCOUNTING_SCHEMA_VERSION,
        "analysis policy {policy_id} predates durable request accounting; explicit reauthorization is required"
    );
    ensure!(
        authorized_project == project_id,
        "analysis policy authorization belongs to a different project"
    );
    let policy: crate::RunPolicy = serde_json::from_str(&policy_json)
        .context("decode canonical analysis policy authorization")?;
    ensure!(
        policy.policy_id == policy_id,
        "analysis policy authorization identity changed"
    );
    ensure!(
        policy.project_id == project_id,
        "analysis policy authorization belongs to a different project"
    );
    ensure!(
        policy_fingerprint == policy.policy_id,
        "analysis accounting policy fingerprint does not match the frozen authorization"
    );
    ensure!(
        policy.provider == provider,
        "analysis accounting requested provider does not match the frozen authorization"
    );
    ensure!(
        (policy.budget_usd - budget_usd).abs() <= 1e-9,
        "analysis accounting budget does not match the frozen authorized ceiling"
    );
    validate_policy_request_family(&policy, &policy.primary_model, family)?;
    Ok(policy)
}

fn verify_authority(
    authority: &Authority,
    project_id: &str,
    policy_id: &str,
    policy_fingerprint: &str,
    provider: &str,
    budget_usd: f64,
    scope: &str,
) -> Result<()> {
    ensure!(
        authority.budget_scope_id == scope,
        "analysis budget authority identity changed"
    );
    ensure!(
        authority.project_id == project_id,
        "analysis budget authority belongs to a different project"
    );
    ensure!(
        authority.policy_id == policy_id
            && authority.policy_fingerprint == policy_fingerprint
            && authority.requested_provider == provider,
        "analysis budget authority is frozen to a different policy/provider identity"
    );
    ensure!(
        (authority.budget_usd - budget_usd).abs() <= 1e-9,
        "analysis budget authority {scope} is already frozen at a different ceiling"
    );
    Ok(())
}

fn load_attempts(connection: &Connection, base: &str) -> Result<Vec<AnalysisRequestRecord>> {
    let request_select = request_select(connection)?;
    let mut statement = connection.prepare(&format!(
        "{request_select} WHERE attempt_base_key=?1 ORDER BY attempt_epoch ASC"
    ))?;
    let records = statement
        .query_map([base], row_to_request)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(records)
}

fn load_authority_optional(connection: &Connection, scope: &str) -> Result<Option<Authority>> {
    connection
        .query_row(
            "SELECT budget_scope_id,project_id,policy_id,policy_fingerprint,requested_provider,
                    budget_usd,legacy_confirmed_cost_usd,legacy_uncertain_usage_reserve_usd,
                    legacy_usage_complete,legacy_evidence_json,dispatch_stopped,stop_reason
             FROM analysis_budget_authorities WHERE budget_scope_id=?1",
            [scope],
            |row| {
                let raw_evidence: String = row.get(9)?;
                Ok(Authority {
                    budget_scope_id: row.get(0)?,
                    project_id: row.get(1)?,
                    policy_id: row.get(2)?,
                    policy_fingerprint: row.get(3)?,
                    requested_provider: row.get(4)?,
                    budget_usd: row.get(5)?,
                    legacy_confirmed_cost_usd: row.get(6)?,
                    legacy_uncertain_usage_reserve_usd: row.get(7)?,
                    legacy_usage_complete: row.get::<_, i64>(8)? != 0,
                    legacy_evidence: serde_json::from_str(&raw_evidence)
                        .unwrap_or_else(|_| json!({})),
                    dispatch_stopped: row.get::<_, i64>(10)? != 0,
                    stop_reason: row.get(11)?,
                })
            },
        )
        .optional()
        .context("read analysis budget authority")
}

pub(super) fn load_authority(connection: &Connection, scope: &str) -> Result<Authority> {
    load_authority_optional(connection, scope)?
        .ok_or_else(|| anyhow!("analysis budget authority is not initialized"))
}

fn totals_for_scope(connection: &Connection, scope: &str) -> Result<LedgerTotals> {
    let estimated = if crate::migrations::column_exists(
        connection,
        "analysis_request_reservations",
        "estimated_cost_usd",
    )? {
        "COALESCE(SUM(estimated_cost_usd),0.0)"
    } else {
        "0.0"
    };
    connection
        .query_row(
            &format!(
                "SELECT
                COALESCE(SUM(confirmed_cost_usd),0.0),
                COALESCE(SUM(CASE WHEN lifecycle IN ('reserved','dispatched')
                    THEN reserved_cost_usd ELSE 0.0 END),0.0),
                COALESCE(SUM(CASE WHEN lifecycle='uncertain'
                    THEN uncertain_cost_usd ELSE 0.0 END),0.0),
                COALESCE(SUM(CASE WHEN lifecycle IN ('reserved','dispatched')
                    THEN 1 ELSE 0 END),0), {estimated}
             FROM analysis_request_reservations WHERE budget_scope_id=?1"
            ),
            [scope],
            |row| {
                Ok(LedgerTotals {
                    confirmed: row.get(0)?,
                    active_reserved: row.get(1)?,
                    uncertain: row.get(2)?,
                    active_requests: row.get::<_, i64>(3)? as usize,
                    estimated: row.get(4)?,
                })
            },
        )
        .context("summarize analysis request accounting")
}

fn snapshot_from_authority(
    connection: &Connection,
    authority: &Authority,
) -> Result<AnalysisBudgetSnapshot> {
    let totals = totals_for_scope(connection, &authority.budget_scope_id)?;
    let untracked = untracked_totals(connection, &authority.budget_scope_id, None)?;
    let confirmed = authority.legacy_confirmed_cost_usd + totals.confirmed;
    let uncertain =
        authority.legacy_uncertain_usage_reserve_usd + totals.uncertain + untracked.reserve;
    let committed = confirmed + totals.estimated + totals.active_reserved + uncertain;
    Ok(AnalysisBudgetSnapshot {
        budget_scope_id: authority.budget_scope_id.clone(),
        project_id: authority.project_id.clone(),
        policy_id: authority.policy_id.clone(),
        requested_provider: authority.requested_provider.clone(),
        budget_usd: authority.budget_usd,
        legacy_confirmed_cost_usd: authority.legacy_confirmed_cost_usd,
        legacy_uncertain_usage_reserve_usd: authority.legacy_uncertain_usage_reserve_usd,
        confirmed_cost_usd: confirmed,
        estimated_cost_usd: totals.estimated,
        active_reserved_cost_usd: totals.active_reserved,
        uncertain_usage_reserve_usd: uncertain,
        post_initialization_untracked_usage_reserve_usd: untracked.reserve,
        post_initialization_untracked_evidence_count: untracked.count,
        committed_or_reserved_usd: committed,
        available_usd: (authority.budget_usd - committed).max(0.0),
        active_requests: totals.active_requests,
        max_in_flight_requests: MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY,
        dispatch_stopped: authority.dispatch_stopped,
        stop_reason: authority.stop_reason.clone(),
        legacy_usage_complete: authority.legacy_usage_complete,
        legacy_evidence: authority.legacy_evidence.clone(),
    })
}

/// Snapshot pre-ledger policy evidence exactly once. A pilot record is the
/// canonical aggregate of its own attempts and its pilot-stage run row is
/// skipped to avoid double counting. Every `analysis_runs` aggregate lacks a
/// usage-completeness proof, so even a terminal run row contributes uncertainty,
/// never a confirmed charge. Malformed/negative/non-finite amounts are retained
/// as unproven usage and conservatively reserve the full authority ceiling.
fn collect_legacy_evidence(
    connection: &Connection,
    project_id: &str,
    policy_id: &str,
    operation: &str,
    budget_usd: f64,
) -> Result<LegacyEvidence> {
    if policy_id.trim().is_empty() {
        return collect_operation_legacy_evidence(connection, project_id, operation, budget_usd);
    }

    let mut confirmed = 0.0;
    let mut uncertain = 0.0;
    let mut usage_complete = true;
    let mut entries = Vec::new();
    let mut malformed_amount_reserved = false;

    let pilot_json: Option<String> = connection
        .query_row(
            "SELECT record_json FROM analysis_run_pilots WHERE policy_id=?1",
            [policy_id],
            |row| row.get(0),
        )
        .optional()?;
    let has_pilot_record = pilot_json.is_some();
    let mut pilot_run_id = String::new();
    if let Some(pilot_json) = pilot_json {
        let record_fingerprint = blake3::hash(pilot_json.as_bytes()).to_hex().to_string();
        match serde_json::from_str::<Value>(&pilot_json) {
            Ok(pilot) => {
                let raw_cost = pilot.get("costUsd").cloned();
                let cost = raw_cost
                    .as_ref()
                    .and_then(Value::as_f64)
                    .filter(|cost| cost.is_finite() && *cost >= 0.0);
                let complete = pilot.get("usageComplete").and_then(Value::as_bool) == Some(true);
                if complete {
                    if let Some(cost) = cost {
                        confirmed += cost;
                    } else {
                        uncertain += budget_usd;
                        malformed_amount_reserved = true;
                        usage_complete = false;
                    }
                } else {
                    uncertain += cost.unwrap_or(budget_usd);
                    malformed_amount_reserved |= cost.is_none();
                    usage_complete = false;
                }
                pilot_run_id = pilot
                    .get("runId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let mut run_ids = Vec::new();
                if !pilot_run_id.is_empty() {
                    run_ids.push(pilot_run_id.clone());
                }
                entries.push(json!({
                    "source": "analysis_run_pilots",
                    "policyId": policy_id,
                    "costUsd": cost,
                    "rawCostUsd": raw_cost,
                    "usageComplete": complete,
                    "status": pilot.get("status").cloned().unwrap_or(Value::Null),
                    "runId": pilot.get("runId").cloned().unwrap_or(Value::Null),
                    "runIds": run_ids,
                    "recordFingerprint": record_fingerprint,
                }));
            }
            Err(error) => {
                uncertain += budget_usd;
                malformed_amount_reserved = true;
                usage_complete = false;
                entries.push(json!({
                    "source": "analysis_run_pilots",
                    "policyId": policy_id,
                    "unreadable": true,
                    "recordFingerprint": record_fingerprint,
                    "error": error.to_string(),
                }));
            }
        }
    }

    let mut statement = connection.prepare(
        "SELECT id,status,stage_reached,CAST(cost_usd AS TEXT)
         FROM analysis_runs WHERE project_id=?1 AND policy_id=?2
         ORDER BY started_at ASC,id ASC",
    )?;
    let runs = statement
        .query_map(params![project_id, policy_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (run_id, status, stage, raw_cost) in runs {
        if has_pilot_record && stage == "pilot" && run_id == pilot_run_id {
            continue;
        }
        let cost = legacy_amount(raw_cost.as_deref());
        if let Some(cost) = cost {
            uncertain += cost;
        } else if !malformed_amount_reserved {
            uncertain += budget_usd;
            malformed_amount_reserved = true;
        }
        // analysis_runs has no usage_complete column. Terminal status and an
        // aggregate cost are therefore source evidence, never proof of a known
        // final provider charge.
        usage_complete = false;
        entries.push(json!({
            "source": "analysis_runs",
            "baseKey": format!("run:{run_id}"),
            "runId": run_id,
            "status": status,
            "stageReached": stage,
            "costUsd": cost,
            "rawCostUsd": raw_cost,
            "reservedFloorUsd": cost.unwrap_or(budget_usd),
            "usageComplete": false,
        }));
    }

    Ok(LegacyEvidence {
        confirmed,
        uncertain,
        usage_complete,
        evidence: json!({
            "source": "pre_ledger_canonical_summaries",
            "present": !entries.is_empty(),
            "entries": entries,
        }),
    })
}

/// Legacy operation evidence belongs to the same canonical collector, never a
/// second balance. Chronology's existing result cache is the only pre-ledger
/// receipt owner for that operation; unmarked operation runs are used for the
/// ad-hoc label path, which had no cache. Completed chronology entries with an
/// explicit `provenance.usageComplete=true` and a known non-negative cost are
/// counted once as confirmed history. A missing completeness field, running
/// row, unreadable result or malformed amount remains unknown and stops the
/// operation authority.
fn collect_operation_legacy_evidence(
    connection: &Connection,
    project_id: &str,
    operation: &str,
    budget_usd: f64,
) -> Result<LegacyEvidence> {
    if operation.trim().is_empty() {
        return Ok(no_operation_legacy_evidence());
    }
    let mut merged = no_operation_legacy_evidence();
    if operation == super::model::CHRONOLOGY_SUGGESTIONS_OPERATION {
        let cache = collect_chronology_cache_legacy(connection, project_id, budget_usd)?;
        if cache.entries_observed > 0 {
            merged = merge_operation_legacy_evidence(merged, cache.legacy);
        }
    }
    // Cache and run rows are distinct historical owners. A cache row must never
    // suppress independent unmarked run evidence in the same initial snapshot.
    let runs = collect_unmarked_operation_runs(connection, project_id, operation, budget_usd)?;
    Ok(merge_operation_legacy_evidence(merged, runs))
}

fn merge_operation_legacy_evidence(
    mut base: LegacyEvidence,
    extra: LegacyEvidence,
) -> LegacyEvidence {
    let extra_present = extra.evidence.get("present").and_then(Value::as_bool) == Some(true);
    if !extra_present {
        return base;
    }
    base.confirmed += extra.confirmed;
    base.uncertain += extra.uncertain;
    base.usage_complete &= extra.usage_complete;
    let mut entries = Vec::new();
    for evidence in [&base.evidence, &extra.evidence] {
        if let Some(existing) = evidence.get("entries").and_then(Value::as_array) {
            entries.extend(existing.iter().cloned());
        }
    }
    base.evidence = json!({
        "source": "pre_ledger_operation_evidence",
        "present": true,
        "entries": entries,
    });
    base
}

fn no_operation_legacy_evidence() -> LegacyEvidence {
    LegacyEvidence {
        confirmed: 0.0,
        uncertain: 0.0,
        usage_complete: true,
        evidence: json!({"source": "no_policy", "present": false}),
    }
}

struct ObservedChronologyCache {
    cost_usd: Option<f64>,
    usage_complete: bool,
    reserved_floor_usd: f64,
}

fn classify_chronology_cache(
    status: &str,
    cost_usd: Option<f64>,
    result_json: &str,
    budget_usd: f64,
) -> ObservedChronologyCache {
    // The raw interpretation has to distinguish an absent old completeness
    // field from an explicit true. Legacy JSON absent that field is not proof
    // that the usage was complete or that zero is a known charge.
    let explicit_complete = serde_json::from_str::<Value>(result_json)
        .ok()
        .and_then(|value| {
            value
                .get("provenance")
                .and_then(|provenance| provenance.get("usageComplete"))
                .and_then(Value::as_bool)
        });
    let completed_with_known_cost =
        status == "completed" && explicit_complete == Some(true) && cost_usd.is_some();
    let usage_complete = completed_with_known_cost;
    let reserved_floor_usd = if usage_complete {
        cost_usd.unwrap_or(0.0)
    } else {
        match cost_usd {
            Some(cost) if cost > 0.0 => cost,
            _ => budget_usd,
        }
    };
    ObservedChronologyCache {
        cost_usd,
        usage_complete,
        reserved_floor_usd,
    }
}

struct ChronologyLegacyEvidence {
    legacy: LegacyEvidence,
    entries_observed: usize,
}

fn collect_chronology_cache_legacy(
    connection: &Connection,
    project_id: &str,
    budget_usd: f64,
) -> Result<ChronologyLegacyEvidence> {
    let mut statement = connection.prepare(
        "SELECT request_key,status,CAST(cost_usd AS TEXT),result_json
         FROM analysis_chronology_suggestions WHERE project_id=?1
         ORDER BY created_at ASC,request_key ASC",
    )?;
    let rows = statement
        .query_map([project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut confirmed = 0.0;
    let mut uncertain = 0.0;
    let mut usage_complete = true;
    let mut entries = Vec::new();
    for (request_key, status, raw_cost, result_json) in &rows {
        let cost = legacy_amount(raw_cost.as_deref());
        let observed = classify_chronology_cache(status, cost, result_json, budget_usd);
        if observed.usage_complete {
            confirmed += observed.cost_usd.unwrap_or(0.0);
        } else {
            usage_complete = false;
            uncertain += observed.reserved_floor_usd;
        }
        entries.push(json!({
            "source": "analysis_chronology_suggestions",
            "baseKey": format!("chronology-cache:{request_key}"),
            "requestKey": request_key,
            "status": status,
            "costUsd": cost,
            "rawCostUsd": raw_cost,
            "usageComplete": observed.usage_complete,
            "reservedFloorUsd": observed.reserved_floor_usd,
            "completenessFieldPresent": serde_json::from_str::<Value>(result_json)
                .ok()
                .and_then(|value| value.get("provenance").cloned())
                .and_then(|provenance| provenance.get("usageComplete").cloned())
                .is_some(),
        }));
    }
    Ok(ChronologyLegacyEvidence {
        legacy: LegacyEvidence {
            confirmed,
            uncertain,
            usage_complete,
            evidence: json!({
                "source": "pre_ledger_operation_cache",
                "present": !entries.is_empty(),
                "entries": entries,
            }),
        },
        entries_observed: rows.len(),
    })
}

fn collect_unmarked_operation_runs(
    connection: &Connection,
    project_id: &str,
    operation: &str,
    budget_usd: f64,
) -> Result<LegacyEvidence> {
    let mut statement = connection.prepare(
        "SELECT id,status,stage_reached,CAST(cost_usd AS TEXT)
         FROM analysis_runs
         WHERE project_id=?1 AND policy_id='' AND stage_reached=?2
         ORDER BY started_at ASC,id ASC",
    )?;
    let rows = statement
        .query_map(rusqlite::params![project_id, operation], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.is_empty() {
        return Ok(no_operation_legacy_evidence());
    }
    let mut uncertain = 0.0;
    let mut malformed_reserved = false;
    let mut entries = Vec::new();
    for (run_id, status, stage, raw_cost) in rows {
        let cost = legacy_amount(raw_cost.as_deref());
        if let Some(cost) = cost {
            uncertain += cost;
        } else if !malformed_reserved {
            uncertain += budget_usd;
            malformed_reserved = true;
        }
        entries.push(json!({
            "source": "analysis_runs",
            "baseKey": format!("run:{run_id}"),
            "runId": run_id,
            "status": status,
            "stageReached": stage,
            "costUsd": cost,
            "rawCostUsd": raw_cost,
            "reservedFloorUsd": cost.unwrap_or(budget_usd),
            "usageComplete": false,
        }));
    }
    Ok(LegacyEvidence {
        confirmed: 0.0,
        uncertain,
        usage_complete: entries.is_empty(),
        evidence: json!({
            "source": "pre_ledger_operation_runs",
            "present": !entries.is_empty(),
            "entries": entries,
        }),
    })
}

pub(super) fn legacy_amount(raw: Option<&str>) -> Option<f64> {
    raw?.trim()
        .parse::<f64>()
        .ok()
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
}

/// Canonical aggregate over every matching ledger attempt. For a pilot caller
/// `operation` is `Some("pilot")`, which excludes bulk charges while still
/// including primary, fallback and prior-run pilot attempts. The unfiltered
/// projection adds all frozen pre-ledger evidence exactly once; the pilot
/// projection includes only provable frozen pilot entries. Because that
/// evidence was captured before the first post-ledger reservation, no row is
/// counted in both places.
pub(crate) fn accounting_aggregate(
    connection: &Connection,
    policy_id: &str,
    operation: Option<&str>,
    current_run_id: Option<&str>,
) -> Result<AnalysisAccountingAggregate> {
    accounting_aggregate_matching(
        connection,
        AggregateScope::Policy(policy_id),
        operation,
        current_run_id,
        None,
        true,
    )
}

/// Canonical accounting projection for exactly one runtime run. It reuses the
/// same request lifecycle calculation as the whole-policy aggregate but omits
/// frozen legacy and post-snapshot evidence, which belong to the authority
/// total rather than to this run. Runtime reports therefore show confirmed
/// current-run cost without double-counting pilot or earlier bulk charges.
pub(crate) fn accounting_aggregate_for_run(
    connection: &Connection,
    policy_id: &str,
    operation: &str,
    run_id: &str,
) -> Result<AnalysisAccountingAggregate> {
    ensure!(
        !operation.trim().is_empty(),
        "accounting aggregate operation is required"
    );
    ensure!(
        !run_id.trim().is_empty(),
        "accounting aggregate run id is required"
    );
    accounting_aggregate_matching(
        connection,
        AggregateScope::Policy(policy_id),
        Some(operation),
        Some(run_id),
        Some(run_id),
        false,
    )
}

/// Canonical whole-operation projection for a non-RunPolicy authority. The
/// operation authority is selected by its project/operation scope id rather
/// than by an empty policy id, which keeps the calculation shared with policy
/// projections without permitting cross-project or cross-operation mixing.
pub(crate) fn accounting_aggregate_for_operation_scope(
    connection: &Connection,
    project_id: &str,
    operation: &str,
    current_run_id: Option<&str>,
    run_id_filter: Option<&str>,
) -> Result<AnalysisAccountingAggregate> {
    ensure!(
        !project_id.trim().is_empty(),
        "accounting aggregate project is required"
    );
    ensure!(
        !operation.trim().is_empty(),
        "accounting aggregate operation is required"
    );
    accounting_aggregate_matching(
        connection,
        AggregateScope::Operation {
            project_id,
            operation,
        },
        Some(operation),
        current_run_id,
        run_id_filter,
        run_id_filter.is_none(),
    )
}

/// Confirmed current-run projection for one non-policy operation.
pub(crate) fn accounting_aggregate_for_operation_run(
    connection: &Connection,
    project_id: &str,
    operation: &str,
    run_id: &str,
) -> Result<AnalysisAccountingAggregate> {
    ensure!(
        !run_id.trim().is_empty(),
        "accounting aggregate run id is required"
    );
    accounting_aggregate_for_operation_scope(
        connection,
        project_id,
        operation,
        Some(run_id),
        Some(run_id),
    )
}

#[derive(Clone, Copy)]
enum AggregateScope<'a> {
    Policy(&'a str),
    Operation {
        project_id: &'a str,
        operation: &'a str,
    },
}

impl<'a> AggregateScope<'a> {
    fn project_id(self) -> Option<&'a str> {
        match self {
            Self::Policy(_) => None,
            Self::Operation { project_id, .. } => Some(project_id),
        }
    }

    fn policy_id(self) -> &'a str {
        match self {
            Self::Policy(policy_id) => policy_id,
            Self::Operation { .. } => "",
        }
    }
}

fn accounting_aggregate_matching(
    connection: &Connection,
    scope: AggregateScope<'_>,
    operation: Option<&str>,
    current_run_id: Option<&str>,
    run_id_filter: Option<&str>,
    include_authority_evidence: bool,
) -> Result<AnalysisAccountingAggregate> {
    let request_select = request_select(connection)?;
    let requests = match scope {
        AggregateScope::Policy(policy_id) => {
            if let Some(run_id) = run_id_filter {
                let mut statement = connection.prepare(&format!(
                    "{request_select} WHERE policy_id=?1 AND (?2 IS NULL OR operation=?2) AND run_id=?3
                     ORDER BY created_at ASC, request_key ASC"
                ))?;
                statement
                    .query_map(params![policy_id, operation, run_id], row_to_request)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            } else {
                let mut statement = connection.prepare(&format!(
                    "{request_select} WHERE policy_id=?1 AND (?2 IS NULL OR operation=?2)
                     ORDER BY created_at ASC, request_key ASC"
                ))?;
                statement
                    .query_map(params![policy_id, operation], row_to_request)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            }
        }
        AggregateScope::Operation {
            project_id,
            operation: scoped_operation,
        } => {
            let budget_scope_id =
                super::model::analysis_operation_budget_scope_id(project_id, scoped_operation);
            if let Some(run_id) = run_id_filter {
                let mut statement = connection.prepare(&format!(
                    "{request_select} WHERE budget_scope_id=?1 AND operation=?2 AND run_id=?3
                     ORDER BY created_at ASC, request_key ASC"
                ))?;
                statement
                    .query_map(
                        params![budget_scope_id, scoped_operation, run_id],
                        row_to_request,
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            } else {
                let mut statement = connection.prepare(&format!(
                    "{request_select} WHERE budget_scope_id=?1 AND operation=?2
                     ORDER BY created_at ASC, request_key ASC"
                ))?;
                statement
                    .query_map(params![budget_scope_id, scoped_operation], row_to_request)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            }
        }
    };
    let authority = if include_authority_evidence {
        match scope {
            AggregateScope::Policy(policy_id) => {
                load_authority_by_policy_optional(connection, policy_id)?
            }
            AggregateScope::Operation {
                project_id,
                operation,
            } => {
                let budget_scope_id =
                    super::model::analysis_operation_budget_scope_id(project_id, operation);
                load_authority_optional(connection, &budget_scope_id)?
            }
        }
    } else {
        None
    };
    let untracked = match authority.as_ref() {
        Some(authority) => untracked_totals(connection, &authority.budget_scope_id, operation)?,
        None => UntrackedTotals::default(),
    };
    let legacy = if include_authority_evidence {
        aggregate_legacy_evidence(authority.as_ref(), operation)
    } else {
        None
    };
    let legacy_included = legacy.is_some();
    let legacy_confirmed = legacy.as_ref().map_or(0.0, |legacy| legacy.confirmed);
    let legacy_uncertain = legacy.as_ref().map_or(0.0, |legacy| legacy.uncertain);
    let legacy_usage_complete = legacy.as_ref().is_none_or(|legacy| legacy.usage_complete);
    let legacy_has_evidence = legacy.as_ref().is_some_and(|legacy| {
        legacy.evidence.get("present").and_then(Value::as_bool) == Some(true)
    });

    let mut aggregate = AnalysisAccountingAggregate {
        project_id: requests
            .first()
            .map(|request| request.project_id.clone())
            .or_else(|| {
                authority
                    .as_ref()
                    .map(|authority| authority.project_id.clone())
            })
            .or_else(|| scope.project_id().map(str::to_string))
            .unwrap_or_default(),
        policy_id: scope.policy_id().to_string(),
        operation: operation.map(str::to_string),
        request_count: requests.len(),
        completed_request_count: 0,
        active_request_count: 0,
        uncertain_request_count: 0,
        released_request_count: 0,
        confirmed_cost_usd: 0.0,
        estimated_cost_usd: 0.0,
        active_reserved_cost_usd: 0.0,
        uncertain_usage_reserve_usd: 0.0,
        legacy_included,
        legacy_confirmed_cost_usd: legacy_confirmed,
        legacy_uncertain_usage_reserve_usd: legacy_uncertain,
        legacy_usage_complete,
        post_initialization_untracked_usage_reserve_usd: untracked.reserve,
        post_initialization_untracked_evidence_count: untracked.count,
        committed_or_reserved_usd: 0.0,
        usage_complete: false,
        current_run_actual_provider: None,
        current_run_actual_model: None,
        current_run_generation_id: None,
        current_run_settled_at: None,
    };
    for request in requests {
        aggregate.confirmed_cost_usd += request.confirmed_cost_usd;
        aggregate.estimated_cost_usd += request.estimated_cost_usd.unwrap_or(0.0);
        match request.lifecycle {
            AnalysisRequestLifecycle::Completed => aggregate.completed_request_count += 1,
            AnalysisRequestLifecycle::Reserved | AnalysisRequestLifecycle::Dispatched => {
                aggregate.active_request_count += 1;
                aggregate.active_reserved_cost_usd += request.reserved_cost_usd;
            }
            AnalysisRequestLifecycle::Uncertain => {
                aggregate.uncertain_request_count += 1;
                aggregate.uncertain_usage_reserve_usd += request.uncertain_usage_reserve_usd;
            }
            AnalysisRequestLifecycle::Released => aggregate.released_request_count += 1,
        }
    }
    if let Some(run_id) = current_run_id
        && let Some(provenance) =
            current_run_receipt_provenance(connection, scope, run_id, operation)?
    {
        aggregate.current_run_actual_provider = nonempty(provenance.provider);
        aggregate.current_run_actual_model = nonempty(provenance.model);
        aggregate.current_run_generation_id = nonempty(provenance.generation);
        aggregate.current_run_settled_at = provenance.settled_at;
    }
    if legacy_included {
        aggregate.confirmed_cost_usd += legacy_confirmed;
        aggregate.uncertain_usage_reserve_usd += legacy_uncertain;
    }
    aggregate.uncertain_usage_reserve_usd += untracked.reserve;
    aggregate.committed_or_reserved_usd = aggregate.confirmed_cost_usd
        + aggregate.estimated_cost_usd
        + aggregate.active_reserved_cost_usd
        + aggregate.uncertain_usage_reserve_usd;
    let relevant_evidence_exists = aggregate.request_count > 0
        || (legacy_included && legacy_has_evidence)
        || untracked.count > 0;
    aggregate.usage_complete = relevant_evidence_exists
        && aggregate.active_request_count == 0
        && aggregate.uncertain_request_count == 0
        && (!legacy_included || legacy_usage_complete)
        && untracked.count == 0;
    Ok(aggregate)
}

/// Factual attempts are durable dispatched reservation rows, not chain error
/// metadata or local denials. Token totals select the latest token-bearing
/// provider-receipt event by append-only event sequence, so metadata-only
/// enrichment cannot erase known usage and clock ties cannot multiply it.
/// Cost is deliberately absent; callers use the canonical accounting aggregate.
pub(crate) fn operation_attempt_projection(
    connection: &Connection,
    policy_id: &str,
    operation: &str,
) -> Result<AnalysisOperationAttemptProjection> {
    let mut statement = connection.prepare(
        "SELECT request_key,requested_model
         FROM analysis_request_reservations
         WHERE policy_id=?1 AND operation=?2 AND dispatch_at IS NOT NULL
         ORDER BY created_at ASC, request_key ASC",
    )?;
    let attempts = statement
        .query_map(params![policy_id, operation], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut projection = AnalysisOperationAttemptProjection::default();
    for (request_key, requested_model) in attempts {
        projection.actual_attempt_count = projection.actual_attempt_count.saturating_add(1);
        projection.attempted_models.push(requested_model);
        let tokens: Option<(Option<i64>, Option<i64>)> = connection
            .query_row(
                "SELECT receipt.prompt_tokens,receipt.completion_tokens
                 FROM analysis_request_events AS event
                 JOIN analysis_provider_receipts AS receipt
                   ON receipt.request_key=event.request_key
                  AND receipt.receipt_key=json_extract(event.detail_json,'$.receiptKey')
                 WHERE event.request_key=?1 AND event.event_kind='receipt'
                   AND (receipt.prompt_tokens IS NOT NULL
                        OR receipt.completion_tokens IS NOT NULL)
                 ORDER BY event.event_id DESC
                 LIMIT 1",
                [&request_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((prompt, completion)) = tokens {
            if let Some(prompt) = prompt.and_then(|value| u32::try_from(value).ok()) {
                projection.prompt_tokens = projection.prompt_tokens.saturating_add(prompt);
            }
            if let Some(completion) = completion.and_then(|value| u32::try_from(value).ok()) {
                projection.completion_tokens =
                    projection.completion_tokens.saturating_add(completion);
            }
        }
    }
    Ok(projection)
}

/// Filter only frozen entries whose operation is provable. Legacy pilot rows
/// and orphaned pilot-stage runs belong to the pilot; generic bulk run summaries
/// cannot be attributed to a more specific modern operation.
fn aggregate_legacy_evidence(
    authority: Option<&Authority>,
    operation: Option<&str>,
) -> Option<LegacyEvidence> {
    let authority = authority?;
    if operation.is_none() || authority.policy_id.trim().is_empty() {
        return Some(LegacyEvidence {
            confirmed: authority.legacy_confirmed_cost_usd,
            uncertain: authority.legacy_uncertain_usage_reserve_usd,
            usage_complete: authority.legacy_usage_complete,
            evidence: authority.legacy_evidence.clone(),
        });
    }
    if operation != Some("pilot") {
        return None;
    }
    let entries = authority.legacy_evidence.get("entries")?.as_array()?;
    let mut legacy = LegacyEvidence {
        confirmed: 0.0,
        uncertain: 0.0,
        usage_complete: true,
        evidence: json!({"present": false}),
    };
    let mut malformed_reserved = false;
    for entry in entries.iter().filter(|entry| {
        entry.get("source").and_then(Value::as_str) == Some("analysis_run_pilots")
            || (entry.get("source").and_then(Value::as_str) == Some("analysis_runs")
                && entry.get("stageReached").and_then(Value::as_str) == Some("pilot"))
    }) {
        legacy.evidence = json!({"present": true});
        let cost = entry
            .get("costUsd")
            .and_then(Value::as_f64)
            .filter(|cost| cost.is_finite() && *cost >= 0.0);
        if entry.get("usageComplete").and_then(Value::as_bool) == Some(true) && cost.is_some() {
            legacy.confirmed += cost.unwrap_or(0.0);
        } else {
            legacy.usage_complete = false;
            if let Some(cost) = cost {
                legacy.uncertain += cost;
            } else if !malformed_reserved {
                legacy.uncertain += authority.budget_usd;
                malformed_reserved = true;
            }
        }
    }
    Some(legacy)
}

struct CurrentRunReceiptProvenance {
    provider: String,
    model: String,
    generation: String,
    settled_at: Option<String>,
}

/// Select the latest completed receipt for the current run using the append-only
/// event sequence, never wall-clock timestamps or receipt-key ordering. A late
/// receipt from an older run cannot participate because the owning request's
/// run identity is filtered before ordering.
fn current_run_receipt_provenance(
    connection: &Connection,
    scope: AggregateScope<'_>,
    run_id: &str,
    operation: Option<&str>,
) -> Result<Option<CurrentRunReceiptProvenance>> {
    let (sql, scope_id) = match scope {
        AggregateScope::Policy(policy_id) => (
            "SELECT COALESCE(request.actual_provider,''),COALESCE(request.actual_model,''),
                COALESCE(request.provider_generation_id,''),request.settled_at
             FROM analysis_request_reservations AS request
             JOIN analysis_request_events AS event ON event.request_key=request.request_key
             WHERE request.policy_id=?1 AND request.run_id=?2
               AND (?3 IS NULL OR request.operation=?3)
               AND request.lifecycle='completed' AND event.event_kind='receipt'
             ORDER BY event.event_id DESC LIMIT 1",
            policy_id.to_string(),
        ),
        AggregateScope::Operation {
            project_id,
            operation: scoped_operation,
        } => (
            "SELECT COALESCE(request.actual_provider,''),COALESCE(request.actual_model,''),
                COALESCE(request.provider_generation_id,''),request.settled_at
             FROM analysis_request_reservations AS request
             JOIN analysis_request_events AS event ON event.request_key=request.request_key
             WHERE request.budget_scope_id=?1 AND request.run_id=?2
               AND request.operation=?3
               AND request.lifecycle='completed' AND event.event_kind='receipt'
             ORDER BY event.event_id DESC LIMIT 1",
            super::model::analysis_operation_budget_scope_id(project_id, scoped_operation),
        ),
    };
    connection
        .query_row(sql, params![scope_id, run_id, operation], |row| {
            Ok(CurrentRunReceiptProvenance {
                provider: row.get(0)?,
                model: row.get(1)?,
                generation: row.get(2)?,
                settled_at: row.get(3)?,
            })
        })
        .optional()
        .context("select current-run receipt provenance by event sequence")
}

fn nonempty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn load_authority_by_policy_optional(
    connection: &Connection,
    policy_id: &str,
) -> Result<Option<Authority>> {
    let scope: Option<String> = connection
        .query_row(
            "SELECT budget_scope_id FROM analysis_budget_authorities
             WHERE policy_id=?1 ORDER BY created_at ASC, budget_scope_id ASC LIMIT 1",
            [policy_id],
            |row| row.get(0),
        )
        .optional()?;
    match scope {
        Some(scope) => load_authority_optional(connection, &scope),
        None => Ok(None),
    }
}

fn append_event(connection: &Connection, key: &str, kind: &str, detail: Value) -> Result<()> {
    connection.execute(
        "INSERT INTO analysis_request_events(request_key,event_kind,detail_json,created_at)
         VALUES(?1,?2,?3,?4)",
        params![key, kind, serde_json::to_string(&detail)?, crate::now()],
    )?;
    Ok(())
}

fn parse_lifecycle(value: &str) -> rusqlite::Result<AnalysisRequestLifecycle> {
    AnalysisRequestLifecycle::parse(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(19, rusqlite::types::Type::Text, error.into())
    })
}

pub(crate) fn row_to_request(row: &Row<'_>) -> rusqlite::Result<AnalysisRequestRecord> {
    let raw_lifecycle: String = row.get(19)?;
    let raw_cost_bound: String = row.get(18)?;
    Ok(AnalysisRequestRecord {
        request_key: row.get(0)?,
        budget_scope_id: row.get(1)?,
        project_id: row.get(2)?,
        policy_id: row.get(3)?,
        run_id: row.get(4)?,
        operation: row.get(5)?,
        attempt_base_key: row.get(6)?,
        attempt_epoch: row.get::<_, i64>(7)? as u32,
        provider: row.get(8)?,
        actual_provider: row.get(9)?,
        requested_model: row.get(10)?,
        actual_model: row.get(11)?,
        provider_generation_id: row.get(12)?,
        policy_fingerprint: row.get(13)?,
        payload_fingerprint: row.get(14)?,
        source_fingerprint: row.get(15)?,
        request_fingerprint: row.get(16)?,
        cost_bound_fingerprint: row.get(17)?,
        cost_bound: serde_json::from_str(&raw_cost_bound).ok(),
        lifecycle: parse_lifecycle(&raw_lifecycle)?,
        reserved_cost_usd: row.get(20)?,
        reported_cost_usd: row.get(21)?,
        confirmed_cost_usd: row.get(22)?,
        estimated_cost_usd: row.get(29)?,
        uncertain_usage_reserve_usd: row.get(23)?,
        failure_code: row.get(24)?,
        dispatch_at: row.get(25)?,
        settled_at: row.get(26)?,
        created_at: row.get(27)?,
        updated_at: row.get(28)?,
    })
}

#[cfg(test)]
#[path = "usage_pricing_store_tests.rs"]
mod usage_pricing_tests;
