//! SQLite transactions for canonical analysis request admission.
//!
//! Every write that can admit a paid transmission starts with SQLite
//! `BEGIN IMMEDIATE`. Totals are recomputed inside that transaction from one
//! authority, its frozen historical evidence and the reservation rows. Provider
//! I/O deliberately lives outside this module.

use anyhow::{Context, Result, anyhow, ensure};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde_json::{Value, json};

use super::model::{
    ANALYSIS_ACCOUNTING_SCHEMA_VERSION, AnalysisAccountingContext, AnalysisBudgetAuthorityContext,
    AnalysisBudgetSnapshot, AnalysisRequestEvent, AnalysisRequestLifecycle, AnalysisRequestRecord,
    LEGACY_UNKNOWN_USAGE_STOP_REASON, MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY,
    analysis_request_cost_bound_fingerprint, attempt_base_key, bounded_failure_code, request_key,
};
use crate::analysis::provider::AnalysisRequestCostBound;

pub(crate) const REQUEST_SELECT: &str = "SELECT request_key,budget_scope_id,project_id,policy_id,run_id,operation,\
 attempt_base_key,attempt_epoch,requested_provider,actual_provider,requested_model,actual_model,\
 provider_generation_id,policy_fingerprint,payload_fingerprint,source_fingerprint,request_fingerprint,\
 cost_bound_fingerprint,cost_bound_json,lifecycle,reserved_cost_usd,reported_cost_usd,\
 confirmed_cost_usd,uncertain_cost_usd,failure_code,dispatch_at,settled_at,created_at,updated_at \
 FROM analysis_request_reservations";

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
    active_reserved: f64,
    uncertain: f64,
    active_requests: usize,
}

impl LedgerTotals {
    fn committed(&self) -> f64 {
        self.confirmed + self.active_reserved + self.uncertain
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
    let authority = load_authority(&transaction, &scope)?;
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

/// Atomically check the full conservative bound against the one canonical
/// authority, then append a reservation. The bound is never clipped.
pub(crate) fn reserve(
    connection: &mut Connection,
    context: &AnalysisAccountingContext,
    model: &str,
    payload_fingerprint: &str,
    cost_bound: &AnalysisRequestCostBound,
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
    validate_request_identity(&transaction, context, model)?;
    ensure!(
        !authority.dispatch_stopped,
        "analysis budget authority dispatch is stopped: {}",
        authority.stop_reason
    );

    let duplicate: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM analysis_request_reservations WHERE attempt_base_key=?1)",
        [&base],
        |row| row.get(0),
    )?;
    ensure!(
        !duplicate,
        "an identical analysis request already exists; refusing a duplicate paid dispatch"
    );

    let totals = totals_for_scope(&transaction, &scope)?;
    ensure!(
        totals.active_requests < MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY,
        "analysis budget authority already has {MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY} requests in flight"
    );
    let committed = authority.legacy_confirmed_cost_usd
        + authority.legacy_uncertain_usage_reserve_usd
        + totals.committed();
    let available = (authority.budget_usd - committed).max(0.0);
    let requested = cost_bound.maximum_cost_usd;
    ensure!(
        requested.is_finite() && requested > 0.0,
        "analysis request cost bound must be finite and greater than zero"
    );
    ensure!(
        available + 1e-12 >= requested,
        "conservative request bound ${requested:.6} does not fit the remaining analysis budget ${available:.6}; clipping is not allowed"
    );

    let key = request_key(&base, 0);
    let now = crate::now();
    transaction.execute(
        "INSERT INTO analysis_request_reservations(
            request_key,budget_scope_id,project_id,policy_id,run_id,operation,attempt_base_key,
            attempt_epoch,requested_provider,requested_model,policy_fingerprint,payload_fingerprint,
            source_fingerprint,request_fingerprint,cost_bound_fingerprint,cost_bound_json,
            lifecycle,reserved_cost_usd,created_at,updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,0,?8,?9,?10,?11,?12,?13,?14,?15,
                'reserved',?16,?17,?17)",
        params![
            key,
            scope,
            context.project_id,
            context.policy_id,
            context.run_id,
            context.operation,
            base,
            context.provider,
            model,
            context.policy_fingerprint,
            payload_fingerprint,
            context.source_fingerprint,
            base,
            cost_bound_fingerprint,
            serde_json::to_string(cost_bound)?,
            requested,
            now,
        ],
    )?;
    append_event(
        &transaction,
        &key,
        "reserved",
        json!({
            "reservedCostUsd": requested,
            "budgetScopeId": scope,
            "legacyConfirmedCostUsd": authority.legacy_confirmed_cost_usd,
            "legacyUncertainUsageReserveUsd": authority.legacy_uncertain_usage_reserve_usd,
            "confirmedCostUsd": totals.confirmed,
            "activeReservedCostUsd": totals.active_reserved,
            "uncertainUsageReserveUsd": totals.uncertain,
            "costBoundFingerprint": cost_bound_fingerprint,
            "costBound": cost_bound,
        }),
    )?;
    let record = load_request(&transaction, &key)?
        .ok_or_else(|| anyhow!("reserved analysis request disappeared"))?;
    transaction.commit()?;
    Ok(record)
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
    ensure!(
        !authority.dispatch_stopped,
        "analysis budget authority dispatch is stopped: {}",
        authority.stop_reason
    );
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
        current.lifecycle == AnalysisRequestLifecycle::Reserved && current.dispatch_at.is_none(),
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
         SET lifecycle='uncertain',confirmed_cost_usd=0.0,uncertain_cost_usd=?2,
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

pub(crate) fn load_request(
    connection: &Connection,
    key: &str,
) -> Result<Option<AnalysisRequestRecord>> {
    connection
        .query_row(
            &format!("{REQUEST_SELECT} WHERE request_key=?1"),
            [key],
            row_to_request,
        )
        .optional()
        .context("read analysis request")
}

pub(crate) fn list_requests(
    connection: &Connection,
    project_id: &str,
    policy_id: Option<&str>,
    limit: usize,
) -> Result<Vec<AnalysisRequestRecord>> {
    let mut statement = connection.prepare(&format!(
        "{REQUEST_SELECT} WHERE project_id=?1 AND (?2='' OR policy_id=?2)
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
        )?;
        ensure!(
            policy.policy_id == context.policy_id,
            "analysis policy authorization identity changed"
        );
    }
    Ok(())
}

/// Validate request-specific identities against the canonical project, policy
/// authorization and owned run. This deliberately runs only after authority
/// initialization and reservation admission has begun; it never invents a
/// transmitted payload from runtime state.
fn validate_request_identity(
    connection: &Connection,
    context: &AnalysisAccountingContext,
    model: &str,
) -> Result<()> {
    if !context.policy_id.trim().is_empty() {
        let policy = load_authorized_policy(
            connection,
            &context.project_id,
            &context.policy_id,
            &context.policy_fingerprint,
            &context.provider,
            context.budget_usd,
        )?;
        ensure!(
            policy.allowed_models.iter().any(|allowed| allowed == model)
                || policy.primary_model == model,
            "analysis accounting model is outside the frozen authorized model allowlist"
        );
        ensure!(
            policy
                .scope
                .iter()
                .any(|scope| scope.source_identity == context.source_fingerprint),
            "analysis accounting source identity is outside the frozen authorized scope"
        );
    }
    if !context.run_id.trim().is_empty() {
        let run: Option<(String, String)> = connection
            .query_row(
                "SELECT project_id,policy_id FROM analysis_runs WHERE id=?1",
                [&context.run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (run_project, run_policy) =
            run.ok_or_else(|| anyhow!("analysis run does not exist"))?;
        ensure!(
            run_project == context.project_id,
            "analysis run belongs to a different project"
        );
        if !context.policy_id.trim().is_empty() {
            ensure!(
                run_policy == context.policy_id,
                "analysis run identity does not belong to the frozen policy/project authority"
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

fn load_authority(connection: &Connection, scope: &str) -> Result<Authority> {
    load_authority_optional(connection, scope)?
        .ok_or_else(|| anyhow!("analysis budget authority is not initialized"))
}

fn totals_for_scope(connection: &Connection, scope: &str) -> Result<LedgerTotals> {
    connection
        .query_row(
            "SELECT
                COALESCE(SUM(confirmed_cost_usd),0.0),
                COALESCE(SUM(CASE WHEN lifecycle IN ('reserved','dispatched')
                    THEN reserved_cost_usd ELSE 0.0 END),0.0),
                COALESCE(SUM(CASE WHEN lifecycle='uncertain'
                    THEN uncertain_cost_usd ELSE 0.0 END),0.0),
                COALESCE(SUM(CASE WHEN lifecycle IN ('reserved','dispatched')
                    THEN 1 ELSE 0 END),0)
             FROM analysis_request_reservations WHERE budget_scope_id=?1",
            [scope],
            |row| {
                Ok(LedgerTotals {
                    confirmed: row.get(0)?,
                    active_reserved: row.get(1)?,
                    uncertain: row.get(2)?,
                    active_requests: row.get::<_, i64>(3)? as usize,
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
    let confirmed = authority.legacy_confirmed_cost_usd + totals.confirmed;
    let uncertain = authority.legacy_uncertain_usage_reserve_usd + totals.uncertain;
    let committed = confirmed + totals.active_reserved + uncertain;
    Ok(AnalysisBudgetSnapshot {
        budget_scope_id: authority.budget_scope_id.clone(),
        project_id: authority.project_id.clone(),
        policy_id: authority.policy_id.clone(),
        requested_provider: authority.requested_provider.clone(),
        budget_usd: authority.budget_usd,
        legacy_confirmed_cost_usd: authority.legacy_confirmed_cost_usd,
        legacy_uncertain_usage_reserve_usd: authority.legacy_uncertain_usage_reserve_usd,
        confirmed_cost_usd: confirmed,
        active_reserved_cost_usd: totals.active_reserved,
        uncertain_usage_reserve_usd: uncertain,
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
    budget_usd: f64,
) -> Result<LegacyEvidence> {
    if policy_id.trim().is_empty() {
        return Ok(LegacyEvidence {
            confirmed: 0.0,
            uncertain: 0.0,
            usage_complete: true,
            evidence: json!({"source": "no_policy", "present": false}),
        });
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
    if let Some(pilot_json) = pilot_json {
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
                entries.push(json!({
                    "source": "analysis_run_pilots",
                    "policyId": policy_id,
                    "costUsd": cost,
                    "rawCostUsd": raw_cost,
                    "usageComplete": complete,
                    "status": pilot.get("status").cloned().unwrap_or(Value::Null),
                    "runId": pilot.get("runId").cloned().unwrap_or(Value::Null),
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
        if has_pilot_record && stage == "pilot" {
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
            "runId": run_id,
            "status": status,
            "stageReached": stage,
            "costUsd": cost,
            "rawCostUsd": raw_cost,
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

fn legacy_amount(raw: Option<&str>) -> Option<f64> {
    raw?.trim()
        .parse::<f64>()
        .ok()
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
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
        uncertain_usage_reserve_usd: row.get(23)?,
        failure_code: row.get(24)?,
        dispatch_at: row.get(25)?,
        settled_at: row.get(26)?,
        created_at: row.get(27)?,
        updated_at: row.get(28)?,
    })
}
