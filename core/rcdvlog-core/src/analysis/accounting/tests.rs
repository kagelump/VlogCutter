//! Focused offline admission tests.
//!
//! There is no production transport wrapper in this slice. Interruption is
//! modeled at the durable boundaries: before the dispatch marker, after it, and
//! after reopening the canonical catalog.

use std::sync::{Arc, Barrier};
use std::thread;

use serde_json::json;
use tempfile::tempdir;

use super::*;
use crate::analysis::provider::{
    AnalysisRequestCostBound, DEFAULT_MODEL, analysis_cost_bound_contract_id,
};
use crate::analysis::run_policy::{
    AnalysisPayload, RunAssetScope, RunPolicy, RunPolicyContext, RunPolicyParameters,
    RunPolicyReadiness,
};
use crate::{AnalysisRunKind, Catalog};

fn fixture() -> (tempfile::TempDir, Catalog, String) {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Synthetic accounting").unwrap();
    (directory, catalog, project.id)
}

fn operation_authority(
    project_id: &str,
    operation: &str,
    budget_usd: f64,
) -> AnalysisBudgetAuthorityContext {
    AnalysisBudgetAuthorityContext {
        project_id: project_id.to_string(),
        policy_id: String::new(),
        policy_fingerprint: "synthetic-operation-v1".into(),
        operation: operation.into(),
        provider: "openrouter".into(),
        budget_scope_id: String::new(),
        budget_usd,
    }
}

fn policy_authority(
    project_id: &str,
    policy_id: &str,
    budget_usd: f64,
) -> AnalysisBudgetAuthorityContext {
    AnalysisBudgetAuthorityContext {
        project_id: project_id.to_string(),
        policy_id: policy_id.into(),
        policy_fingerprint: policy_id.into(),
        operation: "pilot".into(),
        provider: "openrouter".into(),
        budget_scope_id: String::new(),
        budget_usd,
    }
}

fn operation_context(
    project_id: &str,
    operation: &str,
    attempt: &str,
    budget_usd: f64,
) -> AnalysisAccountingContext {
    AnalysisAccountingContext {
        project_id: project_id.to_string(),
        policy_id: String::new(),
        policy_fingerprint: "synthetic-operation-v1".into(),
        run_id: String::new(),
        operation: operation.into(),
        intended_attempt_key: attempt.into(),
        source_fingerprint: "synthetic-source-fingerprint".into(),
        provider: "openrouter".into(),
        budget_scope_id: String::new(),
        budget_usd,
    }
}

fn policy_context(
    project_id: &str,
    policy_id: &str,
    run_id: &str,
    attempt: &str,
    budget_usd: f64,
    source_fingerprint: &str,
) -> AnalysisAccountingContext {
    AnalysisAccountingContext {
        project_id: project_id.to_string(),
        policy_id: policy_id.into(),
        policy_fingerprint: policy_id.into(),
        run_id: run_id.into(),
        operation: "pilot".into(),
        intended_attempt_key: attempt.into(),
        source_fingerprint: source_fingerprint.into(),
        provider: "openrouter".into(),
        budget_scope_id: String::new(),
        budget_usd,
    }
}

fn bound(maximum_cost_usd: f64) -> AnalysisRequestCostBound {
    AnalysisRequestCostBound::declared_for_synthetic_transport(maximum_cost_usd)
}

fn project_policy(
    project_id: &str,
    policy_id: &str,
    budget_usd: f64,
    source_fingerprint: &str,
) -> RunPolicy {
    RunPolicy {
        schema_version: crate::RUN_POLICY_SCHEMA_VERSION.into(),
        policy_id: policy_id.into(),
        project_id: project_id.into(),
        kind: AnalysisRunKind::Observation,
        provider: "openrouter".into(),
        primary_model: DEFAULT_MODEL.into(),
        allowed_models: vec![DEFAULT_MODEL.into()],
        allow_fallback: false,
        permitted_fallbacks: vec![],
        payload: AnalysisPayload::Frames,
        context: RunPolicyContext::default(),
        scope: vec![RunAssetScope {
            asset_id: "synthetic-asset".into(),
            source_fingerprint: "synthetic-representation".into(),
            source_duration_ms: 1_000,
            representation: "proxy".into(),
            representation_fingerprint: Some("synthetic-representation".into()),
            sampled_representation_fingerprint: None,
            transcript_fingerprint: None,
            time_mapping: "identity".into(),
            source_identity: source_fingerprint.into(),
        }],
        expected_cost_usd: 0.01,
        cost_basis: "synthetic test policy".into(),
        budget_usd,
        parameters: RunPolicyParameters::Observation { force: false },
        prompt_version: crate::VLM_PROMPT_VERSION.into(),
        output_schema_version: crate::analysis::schema::CLIP_OBSERVATION_SCHEMA_VERSION.into(),
        recipe_version: crate::FRAMEPACK_RECIPE_KEY.into(),
        tool_version: crate::RUN_POLICY_TOOL_VERSION.into(),
        provider_cost_bound_contract: analysis_cost_bound_contract_id(),
        capability_checks: vec![],
        readiness: RunPolicyReadiness {
            capabilities_ready: true,
            scope_ready: true,
            budget_ready: true,
            credentials_present: true,
            ready_for_authorization: true,
            ready_for_pilot: true,
            blockers: vec![],
            warnings: vec![],
        },
    }
}

fn insert_authorization(catalog: &Catalog, policy: &RunPolicy, accounting_contract: &str) {
    catalog
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO analysis_run_authorizations(
                policy_id,project_id,policy_json,authorized_at,accounting_contract)
             VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![
                policy.policy_id,
                policy.project_id,
                serde_json::to_string(policy).unwrap(),
                "2026-10-25T00:00:00Z",
                accounting_contract,
            ],
        )
        .unwrap();
}

fn insert_pilot(
    catalog: &Catalog,
    project_id: &str,
    policy_id: &str,
    cost_usd: serde_json::Value,
    usage_complete: serde_json::Value,
) {
    let record = json!({
        "schemaVersion": crate::RUN_PILOT_SCHEMA_VERSION,
        "policyId": policy_id,
        "projectId": project_id,
        "status": "uncertain",
        "startedAt": "2026-10-24T00:00:00Z",
        "finishedAt": "2026-10-24T00:00:01Z",
        "runId": "legacy-pilot-run",
        "assetId": "synthetic-asset",
        "requestedModel": DEFAULT_MODEL,
        "costUsd": cost_usd,
        "usageComplete": usage_complete,
    })
    .to_string();
    catalog
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO analysis_run_pilots(
                policy_id,project_id,status,record_json,started_at,finished_at)
             VALUES(?1,?2,'uncertain',?3,'2026-10-24T00:00:00Z','2026-10-24T00:00:01Z')",
            rusqlite::params![policy_id, project_id, record],
        )
        .unwrap();
}

fn insert_run(
    catalog: &Catalog,
    project_id: &str,
    policy_id: &str,
    run_id: &str,
    status: &str,
    stage: &str,
    cost_usd: f64,
) {
    catalog
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO analysis_runs(
                id,project_id,started_at,finished_at,status,stage_reached,
                policy_id,clips_analyzed,cost_usd)
             VALUES(?1,?2,'2026-10-25T00:00:00Z','2026-10-25T00:00:01Z',?3,?4,?5,0,?6)",
            rusqlite::params![run_id, project_id, status, stage, policy_id, cost_usd],
        )
        .unwrap();
}

#[test]
fn schema_version_and_in_flight_limit_are_stable() {
    assert_eq!(ANALYSIS_ACCOUNTING_SCHEMA_VERSION, "analysis.accounting.v1");
    assert_eq!(MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY, 4);
}

#[test]
fn authority_initialization_is_required_before_reservation() {
    let (_directory, catalog, project) = fixture();
    let context = operation_context(&project, "uninitialized", "intent-1", 1.0);
    let error = catalog
        .reserve_analysis_request(&context, DEFAULT_MODEL, "payload-a", &bound(0.10))
        .unwrap_err();
    assert!(format!("{error:#}").contains("not initialized"));
}

#[test]
fn two_independently_opened_catalogs_race_for_the_final_budget() {
    let (directory, catalog, project) = fixture();
    let authority = operation_authority(&project, "race", 0.10);
    catalog
        .initialize_analysis_accounting_authority(&authority)
        .unwrap();
    let second_catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for (index, runner) in [catalog.clone(), second_catalog].into_iter().enumerate() {
        let barrier = Arc::clone(&barrier);
        let project = project.clone();
        handles.push(thread::spawn(move || {
            let context = operation_context(&project, "race", &format!("intent-{index}"), 0.10);
            barrier.wait();
            runner.reserve_analysis_request(&context, DEFAULT_MODEL, "payload-race", &bound(0.10))
        }));
    }
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    let scope = analysis_operation_budget_scope_id(&project, "race");
    let snapshot = catalog.analysis_budget_snapshot(&scope).unwrap().unwrap();
    assert!((snapshot.active_reserved_cost_usd - 0.10).abs() < 1e-12);
    assert_eq!(snapshot.active_requests, 1);
    assert_eq!(snapshot.confirmed_cost_usd, 0.0);
}

#[test]
fn duplicate_request_is_fenced_across_catalogs_and_after_reopen() {
    let (directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(&project, "duplicate", 0.50))
        .unwrap();
    let context = operation_context(&project, "duplicate", "same-intent", 0.50);
    let first = catalog
        .reserve_analysis_request(&context, DEFAULT_MODEL, "payload-a", &bound(0.10))
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&first.request_key)
        .unwrap();

    let reopened = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let error = reopened
        .reserve_analysis_request(&context, DEFAULT_MODEL, "payload-a", &bound(0.10))
        .unwrap_err();
    assert!(format!("{error:#}").contains("already exists"));
    assert_eq!(
        reopened
            .list_analysis_requests(&project, None, 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        reopened
            .analysis_request_needs_reconciliation(&first.request_key)
            .unwrap(),
        Some(AnalysisRequestLifecycle::Dispatched)
    );
}

#[test]
fn full_conservative_bound_must_fit_and_is_never_clipped() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(&project, "fit", 0.10))
        .unwrap();
    let context = operation_context(&project, "fit", "strict-fit", 0.10);
    let error = catalog
        .reserve_analysis_request(&context, DEFAULT_MODEL, "payload-a", &bound(0.11))
        .unwrap_err();
    assert!(format!("{error:#}").contains("clipping is not allowed"));
    let snapshot = catalog
        .analysis_budget_snapshot(&analysis_operation_budget_scope_id(&project, "fit"))
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.confirmed_cost_usd, 0.0);
    assert_eq!(snapshot.active_reserved_cost_usd, 0.0);
    assert_eq!(snapshot.available_usd, 0.10);
}

#[test]
fn policy_authority_provider_model_source_budget_and_run_spoofs_are_denied() {
    let (_directory, catalog, project) = fixture();
    let other = catalog.create_project("Other").unwrap();
    let policy_id = "synthetic-policy";
    let source = "synthetic-source";
    let policy = project_policy(&project, policy_id, 1.0, source);
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 1.0))
        .unwrap();

    let mut spoof_scope = policy_context(&project, policy_id, "", "scope", 1.0, source);
    spoof_scope.budget_scope_id = analysis_policy_budget_scope_id(&other.id, policy_id);
    let error = catalog
        .reserve_analysis_request(&spoof_scope, DEFAULT_MODEL, "payload-a", &bound(0.10))
        .unwrap_err();
    assert!(format!("{error:#}").contains("canonical authority"));

    let mut wrong_provider = policy_context(&project, policy_id, "", "provider", 1.0, source);
    wrong_provider.provider = "other-provider".into();
    let error = catalog
        .reserve_analysis_request(&wrong_provider, DEFAULT_MODEL, "payload-a", &bound(0.10))
        .unwrap_err();
    assert!(format!("{error:#}").contains("frozen to a different policy/provider identity"));

    let wrong_budget = policy_context(&project, policy_id, "", "budget", 2.0, source);
    let error = catalog
        .reserve_analysis_request(&wrong_budget, DEFAULT_MODEL, "payload-a", &bound(0.10))
        .unwrap_err();
    assert!(format!("{error:#}").contains("different ceiling"));

    let wrong_source = policy_context(&project, policy_id, "", "source", 1.0, "unapproved");
    let error = catalog
        .reserve_analysis_request(&wrong_source, DEFAULT_MODEL, "payload-a", &bound(0.10))
        .unwrap_err();
    assert!(format!("{error:#}").contains("outside the frozen authorized scope"));

    let wrong_model = policy_context(&project, policy_id, "", "model", 1.0, source);
    let error = catalog
        .reserve_analysis_request(&wrong_model, "unknown/model", "payload-a", &bound(0.10))
        .unwrap_err();
    assert!(format!("{error:#}").contains("allowlist"));

    insert_run(
        &catalog,
        &other.id,
        "",
        "other-project-run",
        "running",
        "pilot",
        0.0,
    );
    let wrong_run = policy_context(&project, policy_id, "other-project-run", "run", 1.0, source);
    let error = catalog
        .reserve_analysis_request(&wrong_run, DEFAULT_MODEL, "payload-a", &bound(0.10))
        .unwrap_err();
    assert!(format!("{error:#}").contains("different project"));
}

#[test]
fn pre_ledger_authorization_requires_reauthorization_without_resetting_history() {
    let (_directory, catalog, project) = fixture();
    let policy_id = "pre-ledger-policy";
    let source = "source";
    let policy = project_policy(&project, policy_id, 1.0, source);
    insert_authorization(&catalog, &policy, "");
    insert_pilot(&catalog, &project, policy_id, json!(0.20), json!(false));

    let error = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 1.0))
        .unwrap_err();
    assert!(format!("{error:#}").contains("explicit reauthorization"));

    // Simulate the explicit reauthorization write performed by
    // authorize_run_policy. It upgrades the contract but must not reset the
    // already-frozen authority or its legacy uncertainty.
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE analysis_run_authorizations SET accounting_contract=?2 WHERE policy_id=?1",
            rusqlite::params![policy_id, ANALYSIS_ACCOUNTING_SCHEMA_VERSION],
        )
        .unwrap();
    let snapshot = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 1.0))
        .unwrap();
    assert!(!snapshot.legacy_usage_complete);
    assert!(snapshot.dispatch_stopped);
    assert_eq!(snapshot.confirmed_cost_usd, 0.0);
    assert!((snapshot.legacy_uncertain_usage_reserve_usd - 0.20).abs() < 1e-12);

    // Repeating reauthorization/initialization cannot wipe prior evidence or
    // unblock the same frozen stopped authority.
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE analysis_run_authorizations SET accounting_contract=?2 WHERE policy_id=?1",
            rusqlite::params![policy_id, ANALYSIS_ACCOUNTING_SCHEMA_VERSION],
        )
        .unwrap();
    let again = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 1.0))
        .unwrap();
    assert!((again.legacy_uncertain_usage_reserve_usd - 0.20).abs() < 1e-12);
    let error = catalog
        .reserve_analysis_request(
            &policy_context(&project, policy_id, "", "blocked", 1.0, source),
            DEFAULT_MODEL,
            "payload-a",
            &bound(0.10),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("dispatch is stopped"));
}

#[test]
fn valid_historical_pilot_is_counted_once_after_initialization_then_runtime_run_reserves() {
    let (_directory, catalog, project) = fixture();
    let policy_id = "historical-complete";
    let source = "source";
    let policy = project_policy(&project, policy_id, 1.0, source);
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    insert_pilot(&catalog, &project, policy_id, json!(0.95), json!(true));
    insert_run(
        &catalog,
        &project,
        policy_id,
        "legacy-pilot-run",
        "completed",
        "pilot",
        0.95,
    );

    let initialized = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 1.0))
        .unwrap();
    assert!(initialized.legacy_usage_complete);
    assert!(!initialized.dispatch_stopped);
    assert!((initialized.legacy_confirmed_cost_usd - 0.95).abs() < 1e-12);
    assert_eq!(initialized.legacy_uncertain_usage_reserve_usd, 0.0);

    // This is the required C ordering: initialize first, then create the
    // runtime-owned run row, then reserve. The fresh run must not be snapshotted
    // as new legacy uncertainty.
    insert_run(
        &catalog,
        &project,
        policy_id,
        "runtime-run",
        "running",
        "pilot",
        0.0,
    );
    let request = catalog
        .reserve_analysis_request(
            &policy_context(
                &project,
                policy_id,
                "runtime-run",
                "runtime-attempt",
                1.0,
                source,
            ),
            DEFAULT_MODEL,
            "payload-a",
            &bound(0.04),
        )
        .unwrap();
    assert_eq!(request.run_id, "runtime-run");
    let scope = analysis_policy_budget_scope_id(&project, policy_id);
    let snapshot = catalog.analysis_budget_snapshot(&scope).unwrap().unwrap();
    assert!((snapshot.legacy_confirmed_cost_usd - 0.95).abs() < 1e-12);
    assert!((snapshot.active_reserved_cost_usd - 0.04).abs() < 1e-12);
    assert!((snapshot.confirmed_cost_usd - 0.95).abs() < 1e-12);
    assert!(snapshot.legacy_usage_complete);
    let evidence = catalog
        .analysis_budget_authority_evidence(&scope)
        .unwrap()
        .unwrap();
    assert_eq!(evidence["present"], true);
    assert_eq!(evidence["entries"][0]["source"], "analysis_run_pilots");
    let preserved: i64 = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM analysis_runs WHERE id='legacy-pilot-run'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(preserved, 1);
}

#[test]
fn terminal_legacy_run_usage_is_unprovable_and_blocks_admission() {
    let (_directory, catalog, project) = fixture();
    let policy_id = "historical-terminal";
    let source = "source";
    let policy = project_policy(&project, policy_id, 1.0, source);
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    insert_run(
        &catalog,
        &project,
        policy_id,
        "legacy-terminal-run",
        "completed",
        "bulk",
        0.30,
    );

    let snapshot = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 1.0))
        .unwrap();
    assert_eq!(snapshot.confirmed_cost_usd, 0.0);
    assert!((snapshot.uncertain_usage_reserve_usd - 0.30).abs() < 1e-12);
    assert!(!snapshot.legacy_usage_complete);
    assert!(snapshot.dispatch_stopped);
    assert_eq!(snapshot.stop_reason, LEGACY_UNKNOWN_USAGE_STOP_REASON);

    let error = catalog
        .reserve_analysis_request(
            &policy_context(&project, policy_id, "", "blocked", 1.0, source),
            DEFAULT_MODEL,
            "payload-a",
            &bound(0.10),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("dispatch is stopped"));

    let retained: (String, f64) = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT status,cost_usd FROM analysis_runs WHERE id='legacy-terminal-run'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(retained, ("completed".into(), 0.30));
}

#[test]
fn incomplete_and_orphaned_legacy_pilot_usage_stays_uncertain() {
    let (_directory, catalog, project) = fixture();
    let incomplete_policy = "incomplete-pilot";
    let orphan_policy = "orphan-pilot";
    let source = "source";
    insert_authorization(
        &catalog,
        &project_policy(&project, incomplete_policy, 1.0, source),
        ANALYSIS_ACCOUNTING_SCHEMA_VERSION,
    );
    insert_authorization(
        &catalog,
        &project_policy(&project, orphan_policy, 1.0, source),
        ANALYSIS_ACCOUNTING_SCHEMA_VERSION,
    );
    insert_pilot(
        &catalog,
        &project,
        incomplete_policy,
        json!(0.20),
        json!(false),
    );
    insert_run(
        &catalog,
        &project,
        orphan_policy,
        "orphaned-pilot-run",
        "failed",
        "pilot",
        0.50,
    );

    let incomplete = catalog
        .initialize_analysis_accounting_authority(&policy_authority(
            &project,
            incomplete_policy,
            1.0,
        ))
        .unwrap();
    assert_eq!(incomplete.confirmed_cost_usd, 0.0);
    assert!((incomplete.uncertain_usage_reserve_usd - 0.20).abs() < 1e-12);
    assert!(!incomplete.legacy_usage_complete);
    assert!(incomplete.dispatch_stopped);

    let orphan = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, orphan_policy, 1.0))
        .unwrap();
    assert_eq!(orphan.confirmed_cost_usd, 0.0);
    assert!((orphan.uncertain_usage_reserve_usd - 0.50).abs() < 1e-12);
    assert!(!orphan.legacy_usage_complete);
    assert!(orphan.dispatch_stopped);
}

#[test]
fn malformed_legacy_amount_is_not_zero_charge_proof() {
    let (_directory, catalog, project) = fixture();
    let policy_id = "malformed-history";
    let source = "source";
    insert_authorization(
        &catalog,
        &project_policy(&project, policy_id, 1.0, source),
        ANALYSIS_ACCOUNTING_SCHEMA_VERSION,
    );
    insert_run(
        &catalog,
        &project,
        policy_id,
        "negative-cost-run",
        "completed",
        "bulk",
        -0.25,
    );
    let snapshot = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 1.0))
        .unwrap();
    assert_eq!(snapshot.confirmed_cost_usd, 0.0);
    assert!(!snapshot.legacy_usage_complete);
    assert!(snapshot.dispatch_stopped);
    assert!(
        snapshot.uncertain_usage_reserve_usd >= 1.0,
        "malformed history must conservatively reserve the full ceiling"
    );
}

#[test]
fn in_flight_bound_is_four_and_release_restores_capacity() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(&project, "bounded", 2.0))
        .unwrap();
    let mut requests = Vec::new();
    for index in 0..4 {
        requests.push(
            catalog
                .reserve_analysis_request(
                    &operation_context(&project, "bounded", &format!("intent-{index}"), 2.0),
                    DEFAULT_MODEL,
                    &format!("payload-{index}"),
                    &bound(0.10),
                )
                .unwrap(),
        );
    }
    let error = catalog
        .reserve_analysis_request(
            &operation_context(&project, "bounded", "intent-5", 2.0),
            DEFAULT_MODEL,
            "payload-5",
            &bound(0.10),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("already has 4 requests in flight"));
    catalog
        .release_analysis_request_before_dispatch(&requests[0].request_key, "local_cancelled")
        .unwrap();
    catalog
        .reserve_analysis_request(
            &operation_context(&project, "bounded", "intent-5", 2.0),
            DEFAULT_MODEL,
            "payload-5",
            &bound(0.10),
        )
        .unwrap();
}

#[test]
fn preupload_release_and_committed_dispatch_have_distinct_recovery_paths() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(&project, "interrupt", 1.0))
        .unwrap();
    let before = catalog
        .reserve_analysis_request(
            &operation_context(&project, "interrupt", "before", 1.0),
            DEFAULT_MODEL,
            "payload-before",
            &bound(0.10),
        )
        .unwrap();
    let error = catalog
        .release_analysis_request_local_failure(&before.request_key, "network_error")
        .unwrap_err();
    assert!(format!("{error:#}").contains("typed local failure"));
    let released = catalog
        .release_analysis_request_local_failure(&before.request_key, "local_extraction_failed")
        .unwrap();
    assert_eq!(released.lifecycle, AnalysisRequestLifecycle::Released);
    assert_eq!(released.reserved_cost_usd, 0.0);
    assert_eq!(released.confirmed_cost_usd, 0.0);

    let after = catalog
        .reserve_analysis_request(
            &operation_context(&project, "interrupt", "after", 1.0),
            DEFAULT_MODEL,
            "payload-after",
            &bound(0.10),
        )
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&after.request_key)
        .unwrap();
    let error = catalog
        .release_analysis_request_before_dispatch(&after.request_key, "local_extraction_failed")
        .unwrap_err();
    assert!(format!("{error:#}").contains("dispatch marker exists"));
    let uncertain = catalog
        .mark_analysis_request_uncertain(&after.request_key, "provider_error:transport")
        .unwrap();
    assert_eq!(uncertain.lifecycle, AnalysisRequestLifecycle::Uncertain);
    assert_eq!(uncertain.confirmed_cost_usd, 0.0);
    assert!((uncertain.uncertain_usage_reserve_usd - 0.10).abs() < 1e-12);
    assert_eq!(
        catalog
            .analysis_request_needs_reconciliation(&after.request_key)
            .unwrap(),
        Some(AnalysisRequestLifecycle::Uncertain)
    );
}

#[test]
fn events_and_frozen_authority_history_are_append_only_across_reopen() {
    let (directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(&project, "immutable", 1.0))
        .unwrap();
    let request = catalog
        .reserve_analysis_request(
            &operation_context(&project, "immutable", "intent-1", 1.0),
            DEFAULT_MODEL,
            "payload-a",
            &bound(0.10),
        )
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&request.request_key)
        .unwrap();
    let uncertain = catalog
        .mark_analysis_request_uncertain(&request.request_key, "provider_error:timeout")
        .unwrap();
    assert_eq!(uncertain.reported_cost_usd, None);
    assert_eq!(uncertain.confirmed_cost_usd, 0.0);
    assert_eq!(uncertain.uncertain_usage_reserve_usd, 0.10);

    let events = catalog
        .analysis_request_events(&request.request_key)
        .unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_kind.as_str())
            .collect::<Vec<_>>(),
        ["reserved", "dispatched", "uncertain"]
    );
    let update_event = catalog.connection().unwrap().execute(
        "UPDATE analysis_request_events SET event_kind='tampered' WHERE event_id=?1",
        [events[0].event_id],
    );
    assert!(
        format!("{:#}", update_event.unwrap_err()).contains("append-only"),
        "event history must reject updates"
    );
    let update_authority = catalog.connection().unwrap().execute(
        "UPDATE analysis_budget_authorities SET budget_usd=99.0 WHERE budget_scope_id=?1",
        [analysis_operation_budget_scope_id(&project, "immutable")],
    );
    assert!(
        format!("{:#}", update_authority.unwrap_err()).contains("immutable"),
        "frozen authority history must reject updates"
    );

    let reopened = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let events_after = reopened
        .analysis_request_events(&request.request_key)
        .unwrap();
    assert_eq!(events_after.len(), 3);
    let reports = reopened
        .analysis_accounting_report(&project, None, 10)
        .unwrap();
    assert_eq!(reports.requests.len(), 1);
    assert_eq!(reports.budgets.len(), 1);
    assert_eq!(reports.requests[0].confirmed_cost_usd, 0.0);
    let evidence = reopened
        .analysis_budget_authority_evidence(&analysis_operation_budget_scope_id(
            &project,
            "immutable",
        ))
        .unwrap()
        .unwrap();
    assert_eq!(evidence["source"], "no_policy");
}

#[test]
fn a_stopped_authority_blocks_reserved_work_at_the_dispatch_boundary() {
    let (_directory, catalog, project) = fixture();
    let authority = operation_authority(&project, "stop-before-wire", 1.0);
    let snapshot = catalog
        .initialize_analysis_accounting_authority(&authority)
        .unwrap();
    let context = operation_context(&project, "stop-before-wire", "reserved", 1.0);
    let request = catalog
        .reserve_analysis_request(&context, DEFAULT_MODEL, "payload", &bound(0.10))
        .unwrap();
    // Models receipt settlement stopping the shared authority while another
    // worker has reserved but has not yet entered the transport boundary.
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE analysis_budget_authorities SET dispatch_stopped=1,
         stop_reason='reported_cost_exceeded_reservation' WHERE budget_scope_id=?1",
            [&snapshot.budget_scope_id],
        )
        .unwrap();
    let error = catalog
        .mark_analysis_request_dispatched(&request.request_key)
        .unwrap_err();
    assert!(format!("{error:#}").contains("dispatch is stopped"));
    let retained = catalog
        .analysis_request(&request.request_key)
        .unwrap()
        .unwrap();
    assert_eq!(retained.lifecycle, AnalysisRequestLifecycle::Reserved);
    assert!(retained.dispatch_at.is_none());
    catalog
        .release_analysis_request_before_dispatch(&request.request_key, "local_dispatch_stopped")
        .unwrap();
}

#[test]
fn direct_history_deletion_is_rejected_but_project_cleanup_cascades() {
    let (_directory, catalog, project) = fixture();
    let authority = operation_authority(&project, "immutable-history", 1.0);
    catalog
        .initialize_analysis_accounting_authority(&authority)
        .unwrap();
    let context = operation_context(&project, "immutable-history", "reserved", 1.0);
    let request = catalog
        .reserve_analysis_request(&context, DEFAULT_MODEL, "payload", &bound(0.10))
        .unwrap();
    let connection = catalog.connection().unwrap();
    for statement in [
        "DELETE FROM analysis_request_events WHERE request_key=?1",
        "DELETE FROM analysis_request_reservations WHERE request_key=?1",
    ] {
        assert!(
            connection
                .execute(statement, [&request.request_key])
                .is_err()
        );
    }
    assert!(
        connection
            .execute(
                "DELETE FROM analysis_budget_authorities WHERE budget_scope_id=?1",
                [&request.budget_scope_id],
            )
            .is_err()
    );
    connection
        .execute("DELETE FROM projects WHERE id=?1", [&project])
        .unwrap();
    let remaining: i64 = connection
        .query_row("SELECT COUNT(*) FROM analysis_request_events", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(remaining, 0);
    assert!(
        catalog
            .analysis_request(&request.request_key)
            .unwrap()
            .is_none()
    );
}
