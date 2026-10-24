//! Compatibility evidence in the existing Catalog accounting owner.
//! Frozen legacy state stays immutable; post-snapshot unknown floors append once
//! per logical run/pilot. This module has no provider I/O or runtime scheduling.
use super::model::{
    ANALYSIS_ACCOUNTING_SCHEMA_VERSION, UNTRACKED_POST_INITIALIZATION_USAGE_STOP_REASON,
    digest_json,
};
use super::store::{Authority, legacy_amount, load_authority};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::HashSet;
#[derive(Debug, Clone, Default)]
pub(super) struct UntrackedTotals {
    pub(super) reserve: f64,
    pub(super) count: usize,
}
pub(super) fn untracked_totals(
    connection: &Connection,
    scope: &str,
    operation: Option<&str>,
) -> Result<UntrackedTotals> {
    connection
        .query_row(
            "SELECT COALESCE(SUM(reserve_usd),0.0), COUNT(*)
             FROM analysis_untracked_usage_evidence
             WHERE budget_scope_id=?1 AND (?2 IS NULL OR operation=?2)",
            params![scope, operation],
            |row| {
                Ok(UntrackedTotals {
                    reserve: row.get(0)?,
                    count: row.get::<_, i64>(1)? as usize,
                })
            },
        )
        .context("summarize post-initialization untracked analysis usage")
}

fn frozen_run_ids(evidence: &Value) -> HashSet<String> {
    let mut ids = HashSet::new();
    let Some(entries) = evidence.get("entries").and_then(Value::as_array) else {
        return ids;
    };
    for entry in entries {
        if let Some(run_id) = entry.get("runId").and_then(Value::as_str)
            && !run_id.trim().is_empty()
        {
            ids.insert(run_id.to_string());
        }
        if let Some(run_ids) = entry.get("runIds").and_then(Value::as_array) {
            ids.extend(
                run_ids
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|run_id| !run_id.trim().is_empty())
                    .map(str::to_string),
            );
        }
    }
    ids
}

fn frozen_run_entry<'a>(evidence: &'a Value, run_id: &str) -> Option<&'a Value> {
    evidence
        .get("entries")?
        .as_array()?
        .iter()
        .find(|entry| entry.get("runId").and_then(Value::as_str) == Some(run_id))
}

fn frozen_evidence_amount(entry: &Value) -> Option<f64> {
    entry
        .get("costUsd")
        .and_then(Value::as_f64)
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
}

fn frozen_pilot_entries(evidence: &Value) -> impl Iterator<Item = &Value> {
    evidence
        .get("entries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("source").and_then(Value::as_str) == Some("analysis_run_pilots"))
}

fn entry_run_ids(entry: &Value) -> Vec<&str> {
    let mut ids = Vec::new();
    if let Some(run_id) = entry.get("runId").and_then(Value::as_str)
        && !run_id.trim().is_empty()
    {
        ids.push(run_id);
    }
    if let Some(run_ids) = entry.get("runIds").and_then(Value::as_array) {
        ids.extend(
            run_ids
                .iter()
                .filter_map(Value::as_str)
                .filter(|run_id| !run_id.trim().is_empty()),
        );
    }
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn current_pilot_run_id(pilot: Option<&Value>) -> &str {
    pilot
        .and_then(|pilot| pilot.get("runId"))
        .and_then(Value::as_str)
        .filter(|run_id| !run_id.trim().is_empty())
        .unwrap_or_default()
}

/// Find the frozen pilot entry that owns the current mutable pilot row. Exact
/// fingerprints win, including for unreadable JSON. Identity matching supports
/// a later cost/status update of the same run. A single pre-fingerprint pilot
/// entry is treated as the same logical policy-level pilot so its frozen floor
/// is retained rather than added again.
fn matching_frozen_pilot<'a>(
    evidence: &'a Value,
    raw: &str,
    pilot: Option<&Value>,
) -> Option<&'a Value> {
    let fingerprint = blake3::hash(raw.as_bytes()).to_hex().to_string();
    let entries = frozen_pilot_entries(evidence).collect::<Vec<_>>();
    if let Some(exact) = entries
        .iter()
        .copied()
        .find(|entry| entry.get("recordFingerprint").and_then(Value::as_str) == Some(&fingerprint))
    {
        return Some(exact);
    }
    if let Some(pilot) = pilot {
        let run_id = current_pilot_run_id(Some(pilot));
        if !run_id.is_empty()
            && let Some(owner) = entries
                .iter()
                .copied()
                .find(|entry| entry_run_ids(entry).contains(&run_id))
        {
            return Some(owner);
        }
        let null = Value::Null;
        if let Some(legacy) = entries.iter().copied().find(|entry| {
            entry.get("recordFingerprint").is_none()
                && entry.get("runId").unwrap_or(&null) == pilot.get("runId").unwrap_or(&null)
                && entry.get("status").unwrap_or(&null) == pilot.get("status").unwrap_or(&null)
                && entry.get("costUsd").unwrap_or(&null) == pilot.get("costUsd").unwrap_or(&null)
                && entry.get("usageComplete").unwrap_or(&null)
                    == pilot.get("usageComplete").unwrap_or(&null)
        }) {
            return Some(legacy);
        }
    }
    match entries.as_slice() {
        [only] => Some(only),
        _ => None,
    }
}

fn frozen_pilot_credit(entry: &Value, budget_usd: f64) -> f64 {
    entry
        .get("costUsd")
        .and_then(Value::as_f64)
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
        .unwrap_or(budget_usd)
}

fn pilot_observed_unknown_floor(pilot: Option<&Value>, budget_usd: f64) -> f64 {
    let Some(pilot) = pilot else {
        return budget_usd;
    };
    let complete = pilot.get("usageComplete").and_then(Value::as_bool) == Some(true);
    match pilot
        .get("costUsd")
        .and_then(Value::as_f64)
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
    {
        Some(cost) if cost > 0.0 || complete => cost,
        _ => budget_usd,
    }
}

pub(super) fn run_is_accounted(connection: &Connection, run_id: &str) -> Result<bool> {
    if run_id.trim().is_empty() {
        return Ok(false);
    }
    let contract: Option<String> = connection
        .query_row(
            "SELECT accounting_contract FROM analysis_runs WHERE id=?1",
            [run_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(contract.as_deref() == Some(ANALYSIS_ACCOUNTING_SCHEMA_VERSION))
}

struct UntrackedUsageFloor<'a> {
    evidence_kind: &'a str,
    base_key: &'a str,
    operation: &'a str,
    desired_floor_usd: f64,
    frozen_credit_usd: f64,
    evidence: Value,
}

/// Append one delta only when the currently observed unknown floor exceeds all
/// untracked evidence already recorded for the same logical run/pilot. Frozen
/// legacy amounts are passed as `already_frozen_usd` and remain untouched.
fn append_untracked_floor(
    connection: &Connection,
    authority: &Authority,
    floor: UntrackedUsageFloor<'_>,
) -> Result<bool> {
    let UntrackedUsageFloor {
        evidence_kind,
        base_key,
        operation,
        desired_floor_usd,
        frozen_credit_usd,
        evidence,
    } = floor;
    ensure!(
        desired_floor_usd.is_finite() && desired_floor_usd >= 0.0,
        "untracked usage floor must be finite and non-negative"
    );
    ensure!(
        frozen_credit_usd.is_finite() && frozen_credit_usd >= 0.0,
        "frozen untracked usage credit must be finite and non-negative"
    );
    let already_recorded: f64 = connection.query_row(
        "SELECT COALESCE(SUM(reserve_usd),0.0)
         FROM analysis_untracked_usage_evidence
         WHERE budget_scope_id=?1
           AND json_extract(evidence_json,'$.baseKey')=?2",
        params![authority.budget_scope_id, base_key],
        |row| row.get(0),
    )?;
    let already_credited = already_recorded + frozen_credit_usd;
    if desired_floor_usd <= already_credited + 1e-12 {
        return Ok(false);
    }
    let now = crate::now();
    let evidence_key = digest_json(&json!({
        "scope": authority.budget_scope_id,
        "baseKey": base_key,
        "desiredFloorUsd": desired_floor_usd,
        "frozenCreditUsd": frozen_credit_usd,
    }))?;
    let dedupe_key = evidence_key.clone();
    let mut evidence = evidence;
    if let Some(object) = evidence.as_object_mut() {
        object.insert("baseKey".into(), json!(base_key));
        object.insert("observedFloorUsd".into(), json!(desired_floor_usd));
        object.insert("frozenCreditUsd".into(), json!(frozen_credit_usd));
    }
    connection.execute(
        "INSERT INTO analysis_untracked_usage_evidence(
            evidence_key,budget_scope_id,project_id,policy_id,operation,evidence_kind,
            dedupe_key,reserve_usd,evidence_json,created_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            evidence_key,
            authority.budget_scope_id,
            authority.project_id,
            authority.policy_id,
            operation,
            evidence_kind,
            dedupe_key,
            desired_floor_usd - already_credited,
            serde_json::to_string(&evidence)?,
            now,
        ],
    )?;
    Ok(true)
}

fn stop_authority_for_untracked(connection: &Connection, authority: &Authority) -> Result<()> {
    connection.execute(
        "UPDATE analysis_budget_authorities
         SET dispatch_stopped=1,
             stop_reason=CASE WHEN trim(stop_reason)='' THEN ?2 ELSE stop_reason END,
             updated_at=?3
         WHERE budget_scope_id=?1",
        params![
            authority.budget_scope_id,
            UNTRACKED_POST_INITIALIZATION_USAGE_STOP_REASON,
            crate::now(),
        ],
    )?;
    Ok(())
}

/// Detect spending that appears or changes after the immutable authority
/// snapshot. This can happen when an older unwired runtime writes a run or
/// pilot row after B1/B2 froze the authority. Evidence rows are append-only and
/// authority initialization/reservation/dispatch all call this before deciding.
pub(super) fn refresh_untracked_evidence(
    connection: &Connection,
    authority: &Authority,
    budget_usd: f64,
) -> Result<()> {
    if authority.policy_id.trim().is_empty() {
        return Ok(());
    }
    ensure!(
        budget_usd.is_finite() && budget_usd > 0.0,
        "analysis budget must be finite and greater than zero"
    );
    let frozen_run_ids = frozen_run_ids(&authority.legacy_evidence);
    let (mut appended, pilot_owned_run_ids) =
        refresh_untracked_pilot(connection, authority, budget_usd)?;
    appended |= refresh_untracked_runs(
        connection,
        authority,
        budget_usd,
        &frozen_run_ids,
        &pilot_owned_run_ids,
    )?;
    let total = untracked_totals(connection, &authority.budget_scope_id, None)?;
    if appended || total.count > 0 {
        stop_authority_for_untracked(connection, authority)?;
    }
    Ok(())
}

/// Reconcile the single mutable pilot aggregate once. Returns the pilot-owned
/// run ids so the run phase never counts the same logical work again.
fn refresh_untracked_pilot(
    connection: &Connection,
    authority: &Authority,
    budget_usd: f64,
) -> Result<(bool, HashSet<String>)> {
    let mut pilot_owned_run_ids = HashSet::new();
    for entry in frozen_pilot_entries(&authority.legacy_evidence) {
        pilot_owned_run_ids.extend(entry_run_ids(entry).into_iter().map(str::to_string));
    }
    let mut prior_pilots = connection.prepare(
        "SELECT json_extract(evidence_json,'$.runId') FROM analysis_untracked_usage_evidence
         WHERE budget_scope_id=?1 AND evidence_kind='untracked_pilot'",
    )?;
    for id in prior_pilots.query_map([&authority.budget_scope_id], |row| {
        row.get::<_, Option<String>>(0)
    })? {
        if let Some(id) = id? {
            pilot_owned_run_ids.insert(id);
        }
    }
    let pilot: Option<String> = connection
        .query_row(
            "SELECT record_json FROM analysis_run_pilots WHERE policy_id=?1",
            [&authority.policy_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(raw) = pilot else {
        return Ok((false, pilot_owned_run_ids));
    };
    let parsed = serde_json::from_str::<Value>(&raw).ok();
    let pilot_run_id = current_pilot_run_id(parsed.as_ref());
    if !pilot_run_id.is_empty() {
        pilot_owned_run_ids.insert(pilot_run_id.to_string());
    }
    if run_is_accounted(connection, pilot_run_id)? {
        return Ok((false, pilot_owned_run_ids));
    }
    let frozen = matching_frozen_pilot(&authority.legacy_evidence, &raw, parsed.as_ref());
    let raw_fingerprint = blake3::hash(raw.as_bytes()).to_hex().to_string();
    let exact_fingerprint = frozen.is_some_and(|entry| {
        entry.get("recordFingerprint").and_then(Value::as_str) == Some(raw_fingerprint.as_str())
    });
    if exact_fingerprint {
        return Ok((false, pilot_owned_run_ids));
    }
    // The one policy-owned pilot row is cumulative across retries/runs. Its
    // logical accounting identity therefore survives replacement of runId.
    let base_key = format!("pilot:{}", authority.policy_id);
    let observed_floor = pilot_observed_unknown_floor(parsed.as_ref(), budget_usd);
    let frozen_credit = frozen
        .map(|entry| frozen_pilot_credit(entry, budget_usd))
        .unwrap_or(0.0);
    let appended = append_untracked_floor(
        connection,
        authority,
        UntrackedUsageFloor {
            evidence_kind: "untracked_pilot",
            base_key: &base_key,
            operation: "pilot",
            desired_floor_usd: observed_floor,
            frozen_credit_usd: frozen_credit,
            evidence: json!({
                "source": "analysis_run_pilots_after_snapshot",
                "policyId": authority.policy_id,
                "runId": pilot_run_id,
                "recordUnreadable": parsed.is_none(),
                "frozenRecordMatched": frozen.is_some(),
            }),
        },
    )?;
    Ok((appended, pilot_owned_run_ids))
}

/// Reconcile unmarked run rows. Rows owned by the pilot aggregate are skipped;
/// every other real run keeps its own logical key.
fn refresh_untracked_runs(
    connection: &Connection,
    authority: &Authority,
    budget_usd: f64,
    frozen_run_ids: &HashSet<String>,
    pilot_owned_run_ids: &HashSet<String>,
) -> Result<bool> {
    let mut statement = connection.prepare(
        "SELECT id,status,stage_reached,CAST(cost_usd AS TEXT),accounting_contract
         FROM analysis_runs WHERE project_id=?1 AND policy_id=?2
         ORDER BY started_at ASC,id ASC",
    )?;
    let runs = statement
        .query_map(params![authority.project_id, authority.policy_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut appended = false;
    for (run_id, status, stage, raw_cost, accounting_contract) in runs {
        if accounting_contract == ANALYSIS_ACCOUNTING_SCHEMA_VERSION
            || pilot_owned_run_ids.contains(&run_id)
        {
            continue;
        }
        let current_cost = legacy_amount(raw_cost.as_deref());
        let current_floor = current_cost
            .filter(|cost| *cost > 0.0)
            .unwrap_or(budget_usd);
        let operation = if stage == "pilot" {
            "pilot"
        } else {
            stage.as_str()
        };
        if frozen_run_ids.contains(&run_id) {
            let Some(frozen) = frozen_run_entry(&authority.legacy_evidence, &run_id) else {
                // This ID was captured from the aggregate pilot record. That
                // aggregate already owns its charge/reserve.
                continue;
            };
            let unchanged = frozen.get("status").and_then(Value::as_str) == Some(status.as_str())
                && frozen.get("stageReached").and_then(Value::as_str) == Some(stage.as_str())
                && frozen.get("costUsd") == Some(&json!(current_cost));
            if unchanged {
                continue;
            }
            let already_frozen = frozen_evidence_amount(frozen).unwrap_or(0.0);
            let desired = (current_floor - already_frozen).max(0.0);
            appended |= append_untracked_floor(
                connection,
                authority,
                UntrackedUsageFloor {
                    evidence_kind: "untracked_run_change",
                    base_key: &format!("run:{run_id}"),
                    operation,
                    desired_floor_usd: desired,
                    frozen_credit_usd: 0.0,
                    evidence: json!({
                        "source": "analysis_runs_after_snapshot",
                        "runId": run_id,
                        "status": status,
                        "stageReached": stage,
                        "costUsd": current_cost,
                        "alreadyFrozenCostUsd": already_frozen,
                    }),
                },
            )?;
        } else {
            appended |= append_untracked_floor(
                connection,
                authority,
                UntrackedUsageFloor {
                    evidence_kind: "untracked_run",
                    base_key: &format!("run:{run_id}"),
                    operation,
                    desired_floor_usd: current_floor,
                    frozen_credit_usd: 0.0,
                    evidence: json!({
                        "source": "analysis_runs_after_snapshot",
                        "runId": run_id,
                        "status": status,
                        "stageReached": stage,
                        "costUsd": current_cost,
                    }),
                },
            )?;
        }
    }
    Ok(appended)
}

/// Refresh post-initialization evidence and report the committed stop boundary,
/// if any. Callers must commit this transaction before returning the denial.
pub(super) fn persist_untracked_denial_boundary(
    connection: &Connection,
    authority: &Authority,
    budget_usd: f64,
) -> Result<Option<String>> {
    refresh_untracked_evidence(connection, authority, budget_usd)?;
    let refreshed = load_authority(connection, &authority.budget_scope_id)?;
    Ok(refreshed.dispatch_stopped.then_some(refreshed.stop_reason))
}
