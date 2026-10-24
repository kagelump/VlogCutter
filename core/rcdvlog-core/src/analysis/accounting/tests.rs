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
        explicit_retry: false,
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
        explicit_retry: false,
    }
}

fn bound(maximum_cost_usd: f64) -> AnalysisRequestCostBound {
    AnalysisRequestCostBound::declared_for_synthetic_transport(maximum_cost_usd)
}

fn receipt(
    provider: &str,
    generation: &str,
    model: &str,
    cost: f64,
    usage_complete: bool,
) -> ProviderReceiptEvidence {
    ProviderReceiptEvidence {
        provider: provider.into(),
        provider_generation_id: generation.into(),
        actual_model: model.into(),
        reported_cost_usd: Some(cost),
        prompt_tokens: Some(10),
        completion_tokens: Some(5),
        usage_complete,
    }
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
    assert_eq!(MAX_REQUEST_ATTEMPTS, 4);
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

#[test]
fn late_receipt_settles_uncertainty_once_after_restart() {
    let (directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(&project, "late", 1.0))
        .unwrap();
    let request = catalog
        .reserve_analysis_request(
            &operation_context(&project, "late", "late-intent", 1.0),
            DEFAULT_MODEL,
            "payload-late",
            &bound(0.10),
        )
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&request.request_key)
        .unwrap();
    let expected = receipt(
        "actual-provider",
        "generation-late",
        "actual/model",
        0.03,
        true,
    );

    drop(catalog);
    let reopened = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let reconciled = reopened
        .reconcile_analysis_request(
            &request.request_key,
            &AnalysisRequestReconciliation::LateReceipt {
                receipt: expected.clone(),
            },
        )
        .unwrap();
    assert_eq!(reconciled.lifecycle, AnalysisRequestLifecycle::Completed);
    assert_eq!(
        reconciled.actual_provider.as_deref(),
        Some("actual-provider")
    );
    assert_eq!(reconciled.actual_model.as_deref(), Some("actual/model"));
    assert_eq!(
        reconciled.provider_generation_id.as_deref(),
        Some("generation-late")
    );
    assert!((reconciled.confirmed_cost_usd - 0.03).abs() < 1e-12);
    assert_eq!(reconciled.uncertain_usage_reserve_usd, 0.0);
    let event_count = reopened
        .analysis_request_events(&request.request_key)
        .unwrap()
        .len();
    let receipt_count = reopened
        .analysis_provider_receipts(&request.request_key)
        .unwrap()
        .len();
    let duplicate = reopened
        .reconcile_analysis_request(
            &request.request_key,
            &AnalysisRequestReconciliation::LateReceipt { receipt: expected },
        )
        .unwrap();
    assert_eq!(duplicate.lifecycle, AnalysisRequestLifecycle::Completed);
    assert_eq!(
        reopened
            .analysis_request_events(&request.request_key)
            .unwrap()
            .len(),
        event_count,
        "identical reconciliation must not append another audit event"
    );
    assert_eq!(
        reopened
            .analysis_provider_receipts(&request.request_key)
            .unwrap()
            .len(),
        receipt_count,
        "identical reconciliation must not append another receipt"
    );
    let snapshot = reopened
        .analysis_budget_snapshot(&analysis_operation_budget_scope_id(&project, "late"))
        .unwrap()
        .unwrap();
    assert!((snapshot.confirmed_cost_usd - 0.03).abs() < 1e-12);
    assert_eq!(snapshot.active_reserved_cost_usd, 0.0);
    assert_eq!(snapshot.uncertain_usage_reserve_usd, 0.0);
}

#[test]
fn incomplete_receipt_keeps_bound_and_same_identity_complete_can_correct_cost() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(&project, "correct", 1.0))
        .unwrap();
    let request = catalog
        .reserve_analysis_request(
            &operation_context(&project, "correct", "intent", 1.0),
            DEFAULT_MODEL,
            "payload",
            &bound(0.10),
        )
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&request.request_key)
        .unwrap();

    let incomplete = catalog
        .record_analysis_request_receipt(
            &request.request_key,
            &receipt("fake", "known-generation", "model-a", 0.02, false),
        )
        .unwrap();
    assert_eq!(incomplete.lifecycle, AnalysisRequestLifecycle::Uncertain);
    assert_eq!(incomplete.confirmed_cost_usd, 0.0);
    assert!((incomplete.reported_cost_usd.unwrap() - 0.02).abs() < 1e-12);
    assert!((incomplete.uncertain_usage_reserve_usd - 0.10).abs() < 1e-12);

    let completed = catalog
        .record_analysis_request_receipt(
            &request.request_key,
            &receipt("fake", "known-generation", "model-a", 0.03, true),
        )
        .unwrap();
    assert_eq!(completed.lifecycle, AnalysisRequestLifecycle::Completed);
    assert!((completed.confirmed_cost_usd - 0.03).abs() < 1e-12);
    assert_eq!(completed.uncertain_usage_reserve_usd, 0.0);
    assert!((completed.reported_cost_usd.unwrap() - 0.03).abs() < 1e-12);
    assert_eq!(
        catalog
            .analysis_provider_receipts(&request.request_key)
            .unwrap()
            .len(),
        2,
        "both incomplete and final complete evidence remain immutable"
    );

    let conflicting_final = receipt("fake", "known-generation", "model-a", 0.04, true);
    let error = catalog
        .record_analysis_request_receipt(&request.request_key, &conflicting_final)
        .unwrap_err();
    assert!(format!("{error:#}").contains("completed provider receipt"));
    let snapshot = catalog
        .analysis_budget_snapshot(&analysis_operation_budget_scope_id(&project, "correct"))
        .unwrap()
        .unwrap();
    assert!((snapshot.confirmed_cost_usd - 0.03).abs() < 1e-12);
}

#[test]
fn incomplete_higher_floor_never_shrinks_prior_uncertainty_and_overrun_stops_dispatch() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(&project, "floor", 1.0))
        .unwrap();
    let request = catalog
        .reserve_analysis_request(
            &operation_context(&project, "floor", "intent", 1.0),
            DEFAULT_MODEL,
            "payload",
            &bound(0.10),
        )
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&request.request_key)
        .unwrap();
    catalog
        .record_analysis_request_receipt(
            &request.request_key,
            &receipt("fake", "floor-generation", "model", 0.02, false),
        )
        .unwrap();
    let lower = catalog
        .record_analysis_request_receipt(
            &request.request_key,
            &receipt("fake", "floor-generation", "model", 0.01, false),
        )
        .unwrap();
    assert!((lower.uncertain_usage_reserve_usd - 0.10).abs() < 1e-12);
    assert!((lower.reported_cost_usd.unwrap() - 0.02).abs() < 1e-12);

    let higher = catalog
        .record_analysis_request_receipt(
            &request.request_key,
            &receipt("fake", "floor-generation", "model", 0.15, false),
        )
        .unwrap();
    assert_eq!(higher.lifecycle, AnalysisRequestLifecycle::Uncertain);
    assert_eq!(higher.confirmed_cost_usd, 0.0);
    assert!((higher.uncertain_usage_reserve_usd - 0.15).abs() < 1e-12);
    assert!((higher.reported_cost_usd.unwrap() - 0.15).abs() < 1e-12);
    let snapshot = catalog
        .analysis_budget_snapshot(&analysis_operation_budget_scope_id(&project, "floor"))
        .unwrap()
        .unwrap();
    assert!(snapshot.dispatch_stopped);
    assert_eq!(snapshot.stop_reason, "reported_cost_exceeded_reservation");
    let error = catalog
        .reserve_analysis_request(
            &operation_context(&project, "floor", "next", 1.0),
            DEFAULT_MODEL,
            "next-payload",
            &bound(0.01),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("dispatch is stopped"));
}

#[test]
fn missing_actual_provider_does_not_allow_generation_reuse_across_requested_route() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(&project, "generation", 1.0))
        .unwrap();
    let first = catalog
        .reserve_analysis_request(
            &operation_context(&project, "generation", "owner-a", 1.0),
            DEFAULT_MODEL,
            "payload-a",
            &bound(0.10),
        )
        .unwrap();
    let second = catalog
        .reserve_analysis_request(
            &operation_context(&project, "generation", "owner-b", 1.0),
            DEFAULT_MODEL,
            "payload-b",
            &bound(0.10),
        )
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&first.request_key)
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&second.request_key)
        .unwrap();

    let first_receipt = catalog
        .record_analysis_request_receipt(
            &first.request_key,
            &receipt("", "shared-generation", "model", 0.01, true),
        )
        .unwrap();
    assert_eq!(first_receipt.actual_provider, None);

    let error = catalog
        .record_analysis_request_receipt(
            &second.request_key,
            &receipt(
                "different-upstream-provider",
                "shared-generation",
                "model",
                0.01,
                true,
            ),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("different analysis request"));
    assert_eq!(
        catalog
            .analysis_request(&second.request_key)
            .unwrap()
            .unwrap()
            .lifecycle,
        AnalysisRequestLifecycle::Dispatched
    );
}

#[test]
fn completed_metadata_enrichment_is_preserved_once_then_idempotent() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(&project, "enrich", 1.0))
        .unwrap();
    let request = catalog
        .reserve_analysis_request(
            &operation_context(&project, "enrich", "intent", 1.0),
            DEFAULT_MODEL,
            "payload",
            &bound(0.10),
        )
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&request.request_key)
        .unwrap();
    let completed = catalog
        .record_analysis_request_receipt(&request.request_key, &receipt("", "", "", 0.03, true))
        .unwrap();
    assert_eq!(completed.lifecycle, AnalysisRequestLifecycle::Completed);
    assert_eq!(completed.actual_provider, None);
    assert_eq!(completed.actual_model, None);
    assert_eq!(completed.provider_generation_id, None);
    assert!((completed.confirmed_cost_usd - 0.03).abs() < 1e-12);

    let event_count = catalog
        .analysis_request_events(&request.request_key)
        .unwrap()
        .len();
    let enriched = receipt("actual-provider", "generation", "actual-model", 0.03, true);
    let enriched_record = catalog
        .record_analysis_request_receipt(&request.request_key, &enriched)
        .unwrap();
    assert_eq!(
        enriched_record.actual_provider.as_deref(),
        Some("actual-provider")
    );
    assert_eq!(
        enriched_record.provider_generation_id.as_deref(),
        Some("generation")
    );
    assert_eq!(
        enriched_record.actual_model.as_deref(),
        Some("actual-model")
    );
    assert!((enriched_record.confirmed_cost_usd - 0.03).abs() < 1e-12);
    let receipt_count = catalog
        .analysis_provider_receipts(&request.request_key)
        .unwrap()
        .len();
    assert_eq!(receipt_count, 2);
    assert_eq!(
        catalog
            .analysis_request_events(&request.request_key)
            .unwrap()
            .len(),
        event_count + 1
    );

    let repeated = catalog
        .record_analysis_request_receipt(&request.request_key, &enriched)
        .unwrap();
    assert_eq!(repeated, enriched_record);
    assert_eq!(
        catalog
            .analysis_provider_receipts(&request.request_key)
            .unwrap()
            .len(),
        receipt_count,
        "repeated enriched evidence must be idempotent"
    );
    assert_eq!(
        catalog
            .analysis_request_events(&request.request_key)
            .unwrap()
            .len(),
        event_count + 1
    );

    let conflicting = receipt("other-provider", "generation", "actual-model", 0.03, true);
    assert!(
        catalog
            .record_analysis_request_receipt(&request.request_key, &conflicting)
            .is_err(),
        "known actual provider identity cannot change during enrichment"
    );
}

#[test]
fn reserved_released_and_unknown_outcomes_never_become_charged_completions() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(&project, "never-paid", 1.0))
        .unwrap();
    let before = catalog
        .reserve_analysis_request(
            &operation_context(&project, "never-paid", "before", 1.0),
            DEFAULT_MODEL,
            "payload-before",
            &bound(0.10),
        )
        .unwrap();
    let error = catalog
        .record_analysis_request_receipt(
            &before.request_key,
            &receipt("fake", "never-uploaded", DEFAULT_MODEL, 0.01, true),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("never-dispatched or locally-released"));
    catalog
        .release_analysis_request_local_failure(&before.request_key, "local_extraction_failed")
        .unwrap();
    let error = catalog
        .record_analysis_request_receipt(
            &before.request_key,
            &receipt("fake", "still-never-uploaded", DEFAULT_MODEL, 0.01, true),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("never-dispatched or locally-released"));

    let never_marked = catalog
        .reserve_analysis_request(
            &operation_context(&project, "never-paid", "never-marked", 1.0),
            DEFAULT_MODEL,
            "payload-never-marked",
            &bound(0.10),
        )
        .unwrap();
    catalog
        .mark_analysis_request_uncertain(&never_marked.request_key, "provider_error:transport")
        .unwrap();
    let error = catalog
        .record_analysis_request_receipt(
            &never_marked.request_key,
            &receipt("fake", "no-marker", DEFAULT_MODEL, 0.01, true),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("durable dispatch marker"));
    let released = catalog
        .reconcile_analysis_request(
            &never_marked.request_key,
            &AnalysisRequestReconciliation::LocalNoUpload {
                failure_code: "verified_no_dispatch".into(),
            },
        )
        .unwrap();
    assert_eq!(released.lifecycle, AnalysisRequestLifecycle::Released);
    assert_eq!(released.uncertain_usage_reserve_usd, 0.0);

    let after = catalog
        .reserve_analysis_request(
            &operation_context(&project, "never-paid", "after", 1.0),
            DEFAULT_MODEL,
            "payload-after",
            &bound(0.10),
        )
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&after.request_key)
        .unwrap();
    let error = catalog
        .reconcile_analysis_request(
            &after.request_key,
            &AnalysisRequestReconciliation::LocalNoUpload {
                failure_code: "local_extraction_failed".into(),
            },
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("dispatch marker exists"));
    let unknown = catalog
        .reconcile_analysis_request(
            &after.request_key,
            &AnalysisRequestReconciliation::UncertainOutcome {
                failure_code: "provider_error:transport".into(),
            },
        )
        .unwrap();
    assert_eq!(unknown.lifecycle, AnalysisRequestLifecycle::Uncertain);
    assert!(unknown.dispatch_at.is_some());
    assert_eq!(unknown.confirmed_cost_usd, 0.0);
    assert!((unknown.uncertain_usage_reserve_usd - 0.10).abs() < 1e-12);
}

#[test]
fn explicit_retry_is_required_bounded_and_retains_prior_charges() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(&project, "retry", 0.50))
        .unwrap();
    let mut context = operation_context(&project, "retry", "retry-intent", 0.50);
    let first = catalog
        .reserve_analysis_request(&context, DEFAULT_MODEL, "payload-a", &bound(0.10))
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&first.request_key)
        .unwrap();
    catalog
        .mark_analysis_request_uncertain(&first.request_key, "provider_error:transport")
        .unwrap();

    context.explicit_retry = true;
    let error = catalog
        .reserve_analysis_request(&context, DEFAULT_MODEL, "payload-a", &bound(0.10))
        .unwrap_err();
    assert!(format!("{error:#}").contains("has not been authorized"));

    let authorization = catalog
        .authorize_analysis_request_retry(&first.request_key)
        .unwrap();
    assert_eq!(authorization.next_attempt_epoch, 1);
    assert!((authorization.retained_uncertain_cost_usd - 0.10).abs() < 1e-12);
    catalog
        .authorize_analysis_request_retry(&first.request_key)
        .unwrap();
    assert_eq!(
        catalog
            .analysis_request_events(&first.request_key)
            .unwrap()
            .iter()
            .filter(|event| event.event_kind == "retry_authorized")
            .count(),
        1,
        "repeat authorization for the same next epoch is idempotent"
    );

    let retry = catalog
        .reserve_analysis_request(&context, DEFAULT_MODEL, "payload-a", &bound(0.10))
        .unwrap();
    assert_eq!(retry.attempt_epoch, 1);
    let snapshot = catalog
        .analysis_budget_snapshot(&analysis_operation_budget_scope_id(&project, "retry"))
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.confirmed_cost_usd, 0.0);
    assert!((snapshot.active_reserved_cost_usd - 0.10).abs() < 1e-12);
    assert!((snapshot.uncertain_usage_reserve_usd - 0.10).abs() < 1e-12);

    catalog
        .record_analysis_request_receipt(
            &first.request_key,
            &receipt("fake", "retry-late", DEFAULT_MODEL, 0.03, true),
        )
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&retry.request_key)
        .unwrap();
    catalog
        .record_analysis_request_receipt(
            &retry.request_key,
            &receipt("fake", "retry-current", DEFAULT_MODEL, 0.04, true),
        )
        .unwrap();
    let snapshot = catalog
        .analysis_budget_snapshot(&analysis_operation_budget_scope_id(&project, "retry"))
        .unwrap()
        .unwrap();
    assert!((snapshot.confirmed_cost_usd - 0.07).abs() < 1e-12);
    assert_eq!(snapshot.uncertain_usage_reserve_usd, 0.0);
    assert_eq!(
        catalog
            .list_analysis_requests(&project, None, 10)
            .unwrap()
            .len(),
        2,
        "all prior attempt rows remain in history"
    );
}

#[test]
fn stale_and_competing_retry_epochs_are_fenced() {
    let (directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(&project, "retry-race", 1.0))
        .unwrap();
    let mut context = operation_context(&project, "retry-race", "same-intent", 1.0);
    let first = catalog
        .reserve_analysis_request(&context, DEFAULT_MODEL, "payload", &bound(0.10))
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&first.request_key)
        .unwrap();
    catalog
        .mark_analysis_request_uncertain(&first.request_key, "provider_error:transport")
        .unwrap();
    catalog
        .authorize_analysis_request_retry(&first.request_key)
        .unwrap();
    context.explicit_retry = true;
    let second_catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for runner in [catalog.clone(), second_catalog] {
        let barrier = Arc::clone(&barrier);
        let context = context.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            runner.reserve_analysis_request(&context, DEFAULT_MODEL, "payload", &bound(0.10))
        }));
    }
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    assert_eq!(
        catalog
            .list_analysis_requests(&project, None, 10)
            .unwrap()
            .len(),
        2,
        "only one competing retry epoch may be consumed"
    );
    let stale = catalog
        .authorize_analysis_request_retry(&first.request_key)
        .unwrap_err();
    assert!(format!("{stale:#}").contains("stale retry authorization"));
}

#[test]
fn retry_limit_is_four_attempts_and_all_confirmed_charges_are_retained() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(
            &project,
            "retry-limit",
            1.0,
        ))
        .unwrap();
    let mut context = operation_context(&project, "retry-limit", "bounded-request", 1.0);
    let mut current = catalog
        .reserve_analysis_request(&context, DEFAULT_MODEL, "payload", &bound(0.01))
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&current.request_key)
        .unwrap();
    catalog
        .record_analysis_request_receipt(
            &current.request_key,
            &receipt("fake", "retry-generation-0", DEFAULT_MODEL, 0.01, true),
        )
        .unwrap();
    for epoch in 1..MAX_REQUEST_ATTEMPTS {
        catalog
            .authorize_analysis_request_retry(&current.request_key)
            .unwrap();
        context.explicit_retry = true;
        current = catalog
            .reserve_analysis_request(&context, DEFAULT_MODEL, "payload", &bound(0.01))
            .unwrap();
        assert_eq!(current.attempt_epoch, epoch);
        catalog
            .mark_analysis_request_dispatched(&current.request_key)
            .unwrap();
        catalog
            .record_analysis_request_receipt(
                &current.request_key,
                &receipt(
                    "fake",
                    &format!("retry-generation-{epoch}"),
                    DEFAULT_MODEL,
                    0.01,
                    true,
                ),
            )
            .unwrap();
    }
    let error = catalog
        .authorize_analysis_request_retry(&current.request_key)
        .unwrap_err();
    assert!(format!("{error:#}").contains("retry limit reached"));
    let requests = catalog.list_analysis_requests(&project, None, 10).unwrap();
    assert_eq!(requests.len(), MAX_REQUEST_ATTEMPTS as usize);
    assert!(
        requests
            .iter()
            .all(|request| request.lifecycle == AnalysisRequestLifecycle::Completed)
    );
    let snapshot = catalog
        .analysis_budget_snapshot(&analysis_operation_budget_scope_id(&project, "retry-limit"))
        .unwrap()
        .unwrap();
    assert!((snapshot.confirmed_cost_usd - 0.04).abs() < 1e-12);
}

#[test]
fn complete_returned_cost_above_bound_is_confirmed_and_stops_authority() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(&project, "overrun", 1.0))
        .unwrap();
    let request = catalog
        .reserve_analysis_request(
            &operation_context(&project, "overrun", "intent", 1.0),
            DEFAULT_MODEL,
            "payload",
            &bound(0.10),
        )
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&request.request_key)
        .unwrap();
    catalog
        .record_analysis_request_receipt(
            &request.request_key,
            &receipt("fake", "overrun-generation", DEFAULT_MODEL, 0.15, true),
        )
        .unwrap();
    let snapshot = catalog
        .analysis_budget_snapshot(&analysis_operation_budget_scope_id(&project, "overrun"))
        .unwrap()
        .unwrap();
    assert!((snapshot.confirmed_cost_usd - 0.15).abs() < 1e-12);
    assert!(snapshot.dispatch_stopped);
    assert_eq!(snapshot.stop_reason, "reported_cost_exceeded_reservation");
    let error = catalog
        .reserve_analysis_request(
            &operation_context(&project, "overrun", "next", 1.0),
            DEFAULT_MODEL,
            "next-payload",
            &bound(0.10),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("dispatch is stopped"));
}

#[test]
fn mixed_pilot_and_bulk_operation_aggregates_do_not_mix_charges() {
    let (_directory, catalog, project) = fixture();
    let policy_id = "mixed-operations";
    let source = "source";
    let policy = project_policy(&project, policy_id, 2.0, source);
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    insert_run(
        &catalog,
        &project,
        policy_id,
        "run-current",
        "completed",
        "bulk",
        0.0,
    );

    let mut pilot_context = policy_context(
        &project,
        policy_id,
        "run-current",
        "pilot-intent",
        2.0,
        source,
    );
    pilot_context.operation = "pilot".into();
    let pilot = catalog
        .reserve_analysis_request(&pilot_context, DEFAULT_MODEL, "pilot-payload", &bound(0.10))
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&pilot.request_key)
        .unwrap();
    catalog
        .record_analysis_request_receipt(
            &pilot.request_key,
            &receipt(
                "pilot-provider",
                "pilot-generation",
                DEFAULT_MODEL,
                0.02,
                true,
            ),
        )
        .unwrap();

    let mut bulk_context = policy_context(
        &project,
        policy_id,
        "run-current",
        "bulk-intent",
        2.0,
        source,
    );
    bulk_context.operation = "bulk".into();
    let bulk = catalog
        .reserve_analysis_request(&bulk_context, DEFAULT_MODEL, "bulk-payload", &bound(0.10))
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&bulk.request_key)
        .unwrap();
    catalog
        .record_analysis_request_receipt(
            &bulk.request_key,
            &receipt(
                "bulk-provider",
                "bulk-generation",
                DEFAULT_MODEL,
                0.07,
                true,
            ),
        )
        .unwrap();

    let pilot_aggregate = catalog
        .analysis_accounting_aggregate_for_operation(policy_id, "pilot", Some("run-current"))
        .unwrap();
    assert_eq!(pilot_aggregate.request_count, 1);
    assert_eq!(pilot_aggregate.completed_request_count, 1);
    assert!((pilot_aggregate.confirmed_cost_usd - 0.02).abs() < 1e-12);
    assert_eq!(
        pilot_aggregate.current_run_actual_provider.as_deref(),
        Some("pilot-provider")
    );
    assert!(pilot_aggregate.usage_complete);

    let whole_policy = catalog
        .analysis_accounting_aggregate(policy_id, Some("run-current"))
        .unwrap();
    assert_eq!(whole_policy.request_count, 2);
    assert!((whole_policy.confirmed_cost_usd - 0.09).abs() < 1e-12);
    assert!(whole_policy.usage_complete);
    let snapshot = catalog
        .analysis_budget_snapshot(&analysis_policy_budget_scope_id(&project, policy_id))
        .unwrap()
        .unwrap();
    assert!((snapshot.confirmed_cost_usd - 0.09).abs() < 1e-12);
}

#[test]
fn frozen_legacy_evidence_is_included_once_in_whole_policy_projection() {
    let (_directory, catalog, project) = fixture();
    let policy_id = "legacy-aggregate";
    let source = "source";
    let policy = project_policy(&project, policy_id, 1.0, source);
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    insert_pilot(&catalog, &project, policy_id, json!(0.20), json!(true));
    catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 1.0))
        .unwrap();
    insert_run(
        &catalog,
        &project,
        policy_id,
        "run-current",
        "completed",
        "bulk",
        0.0,
    );
    let context = policy_context(&project, policy_id, "run-current", "new-pilot", 1.0, source);
    let request = catalog
        .reserve_analysis_request(&context, DEFAULT_MODEL, "payload", &bound(0.10))
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&request.request_key)
        .unwrap();
    catalog
        .record_analysis_request_receipt(
            &request.request_key,
            &receipt("fake", "legacy-new-generation", DEFAULT_MODEL, 0.01, true),
        )
        .unwrap();

    let whole = catalog
        .analysis_accounting_aggregate(policy_id, Some("run-current"))
        .unwrap();
    assert!(whole.legacy_included);
    assert!((whole.legacy_confirmed_cost_usd - 0.20).abs() < 1e-12);
    assert!((whole.confirmed_cost_usd - 0.21).abs() < 1e-12);
    let pilot = catalog
        .analysis_accounting_aggregate_for_operation(policy_id, "pilot", Some("run-current"))
        .unwrap();
    assert!(pilot.legacy_included);
    assert!((pilot.confirmed_cost_usd - 0.21).abs() < 1e-12);
    assert!((pilot.legacy_confirmed_cost_usd - 0.20).abs() < 1e-12);
}

#[test]
fn late_old_run_receipt_contributes_charge_without_overwriting_current_provenance() {
    let (_directory, catalog, project) = fixture();
    let policy_id = "aggregate-provenance";
    let source = "source";
    let policy = project_policy(&project, policy_id, 2.0, source);
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    insert_run(
        &catalog,
        &project,
        policy_id,
        "run-old",
        "completed",
        "bulk",
        0.0,
    );
    insert_run(
        &catalog,
        &project,
        policy_id,
        "run-new",
        "completed",
        "bulk",
        0.0,
    );

    let mut old_context =
        policy_context(&project, policy_id, "run-old", "same-intent", 2.0, source);
    let old = catalog
        .reserve_analysis_request(&old_context, DEFAULT_MODEL, "payload", &bound(0.10))
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&old.request_key)
        .unwrap();
    catalog
        .mark_analysis_request_uncertain(&old.request_key, "provider_error:transport")
        .unwrap();
    catalog
        .authorize_analysis_request_retry(&old.request_key)
        .unwrap();
    old_context.explicit_retry = true;
    old_context.run_id = "run-new".into();
    let current = catalog
        .reserve_analysis_request(&old_context, DEFAULT_MODEL, "payload", &bound(0.10))
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&current.request_key)
        .unwrap();
    catalog
        .record_analysis_request_receipt(
            &current.request_key,
            &receipt("new-provider", "new-generation", "new-model", 0.02, true),
        )
        .unwrap();

    let before_late = catalog
        .analysis_accounting_aggregate(policy_id, Some("run-new"))
        .unwrap();
    assert_eq!(before_late.request_count, 2);
    assert_eq!(before_late.completed_request_count, 1);
    assert_eq!(before_late.uncertain_request_count, 1);
    assert!(!before_late.usage_complete);
    assert!((before_late.uncertain_usage_reserve_usd - 0.10).abs() < 1e-12);
    assert_eq!(
        before_late.current_run_actual_provider.as_deref(),
        Some("new-provider")
    );
    assert_eq!(
        before_late.current_run_actual_model.as_deref(),
        Some("new-model")
    );
    assert_eq!(
        before_late.current_run_generation_id.as_deref(),
        Some("new-generation")
    );

    catalog
        .reconcile_analysis_request(
            &old.request_key,
            &AnalysisRequestReconciliation::LateReceipt {
                receipt: receipt("old-provider", "old-generation", "old-model", 0.01, true),
            },
        )
        .unwrap();
    let after_late = catalog
        .analysis_accounting_aggregate(policy_id, Some("run-new"))
        .unwrap();
    assert!(after_late.usage_complete);
    assert!((after_late.confirmed_cost_usd - 0.03).abs() < 1e-12);
    assert_eq!(after_late.uncertain_usage_reserve_usd, 0.0);
    assert_eq!(
        after_late.current_run_actual_provider.as_deref(),
        Some("new-provider"),
        "late old-run receipt must not overwrite current-run provenance"
    );
    assert_eq!(
        after_late.current_run_generation_id.as_deref(),
        Some("new-generation")
    );
    let old_record = catalog.analysis_request(&old.request_key).unwrap().unwrap();
    assert_eq!(old_record.actual_provider.as_deref(), Some("old-provider"));
}

#[test]
fn history_immutability_includes_receipts_and_project_cleanup_cascades() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(&project, "history", 1.0))
        .unwrap();
    let request = catalog
        .reserve_analysis_request(
            &operation_context(&project, "history", "intent", 1.0),
            DEFAULT_MODEL,
            "payload",
            &bound(0.10),
        )
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&request.request_key)
        .unwrap();
    catalog
        .record_analysis_request_receipt(
            &request.request_key,
            &receipt("fake", "history-generation", DEFAULT_MODEL, 0.01, true),
        )
        .unwrap();
    let connection = catalog.connection().unwrap();
    for statement in [
        "DELETE FROM analysis_provider_receipts WHERE request_key=?1",
        "UPDATE analysis_provider_receipts SET reported_cost_usd=99.0 WHERE request_key=?1",
        "DELETE FROM analysis_request_events WHERE request_key=?1",
        "DELETE FROM analysis_request_reservations WHERE request_key=?1",
    ] {
        assert!(
            connection
                .execute(statement, [&request.request_key])
                .is_err(),
            "append-only accounting history rejected: {statement}"
        );
    }
    connection
        .execute("DELETE FROM projects WHERE id=?1", [&project])
        .unwrap();
    let remaining_receipts: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM analysis_provider_receipts",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let remaining_events: i64 = connection
        .query_row("SELECT COUNT(*) FROM analysis_request_events", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(remaining_receipts, 0);
    assert_eq!(remaining_events, 0);
}

#[test]
fn complete_usage_without_cost_stays_unknown_until_matching_cost_receipt() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(
            &project,
            "missing-cost",
            1.0,
        ))
        .unwrap();
    let request = catalog
        .reserve_analysis_request(
            &operation_context(&project, "missing-cost", "intent", 1.0),
            DEFAULT_MODEL,
            "payload",
            &bound(0.10),
        )
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&request.request_key)
        .unwrap();
    let missing_cost = ProviderReceiptEvidence {
        provider: "actual-provider".into(),
        provider_generation_id: "generation".into(),
        actual_model: "actual-model".into(),
        reported_cost_usd: None,
        prompt_tokens: Some(10),
        completion_tokens: Some(5),
        usage_complete: true,
    };
    let unknown = catalog
        .record_analysis_request_receipt(&request.request_key, &missing_cost)
        .unwrap();
    assert_eq!(unknown.lifecycle, AnalysisRequestLifecycle::Uncertain);
    assert_eq!(unknown.failure_code, "usage_cost_missing");
    assert_eq!(unknown.reported_cost_usd, None);
    assert!((unknown.uncertain_usage_reserve_usd - 0.10).abs() < 1e-12);
    assert_eq!(unknown.confirmed_cost_usd, 0.0);

    let completed = catalog
        .record_analysis_request_receipt(
            &request.request_key,
            &receipt("actual-provider", "generation", "actual-model", 0.03, true),
        )
        .unwrap();
    assert_eq!(completed.lifecycle, AnalysisRequestLifecycle::Completed);
    assert!((completed.confirmed_cost_usd - 0.03).abs() < 1e-12);
    assert_eq!(completed.uncertain_usage_reserve_usd, 0.0);
}

#[test]
fn schema_trigger_rejects_direct_cross_request_generation_reuse() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(&project, "trigger", 1.0))
        .unwrap();
    let first = catalog
        .reserve_analysis_request(
            &operation_context(&project, "trigger", "first", 1.0),
            DEFAULT_MODEL,
            "payload-first",
            &bound(0.10),
        )
        .unwrap();
    let second = catalog
        .reserve_analysis_request(
            &operation_context(&project, "trigger", "second", 1.0),
            DEFAULT_MODEL,
            "payload-second",
            &bound(0.10),
        )
        .unwrap();
    let connection = catalog.connection().unwrap();
    for (request_key, receipt_key, provider, fingerprint) in [
        (&first.request_key, "receipt-1", "", "fingerprint-1"),
        (
            &second.request_key,
            "receipt-2",
            "different-upstream",
            "fingerprint-2",
        ),
    ] {
        let result = connection.execute(
            "INSERT INTO analysis_provider_receipts(
                receipt_key,request_key,provider,provider_generation_id,actual_model,
                reported_cost_usd,prompt_tokens,completion_tokens,usage_complete,
                receipt_fingerprint,created_at)
             VALUES(?1,?2,?3,'shared-generation','model',0.01,1,1,1,?4,'2026-10-25T00:00:00Z')",
            rusqlite::params![receipt_key, request_key, provider, fingerprint],
        );
        if receipt_key == "receipt-1" {
            result.unwrap();
        } else {
            assert!(
                format!("{:#}", result.unwrap_err()).contains("different analysis request"),
                "migration 39 trigger must enforce requested-route generation ownership"
            );
        }
    }
}

#[test]
fn receipt_identity_whitespace_cannot_bypass_generation_ownership() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(&project, "identity", 1.0))
        .unwrap();
    let request = catalog
        .reserve_analysis_request(
            &operation_context(&project, "identity", "intent", 1.0),
            DEFAULT_MODEL,
            "payload",
            &bound(0.1),
        )
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&request.request_key)
        .unwrap();
    for evidence in [
        receipt("upstream", " generation", "model", 0.01, true),
        receipt(" upstream ", "generation", "model", 0.01, true),
        receipt("upstream", "generation", "model ", 0.01, true),
    ] {
        assert!(
            catalog
                .record_analysis_request_receipt(&request.request_key, &evidence)
                .is_err()
        );
    }
    assert!(
        catalog
            .analysis_provider_receipts(&request.request_key)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn filtered_pilot_projection_retains_frozen_unknown_pilot_usage() {
    let (_directory, catalog, project) = fixture();
    let policy = project_policy(&project, "legacy-unknown-projection", 1.0, "source");
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    insert_pilot(
        &catalog,
        &project,
        &policy.policy_id,
        json!(0.2),
        json!(false),
    );
    insert_run(
        &catalog,
        &project,
        &policy.policy_id,
        "old-bulk",
        "completed",
        "bulk",
        0.3,
    );
    catalog
        .initialize_analysis_accounting_authority(&policy_authority(
            &project,
            &policy.policy_id,
            1.0,
        ))
        .unwrap();
    let pilot = catalog
        .analysis_accounting_aggregate_for_operation(&policy.policy_id, "pilot", None)
        .unwrap();
    assert!(pilot.legacy_included);
    assert!(!pilot.usage_complete);
    assert_eq!(pilot.confirmed_cost_usd, 0.0);
    assert!((pilot.uncertain_usage_reserve_usd - 0.2).abs() < 1e-12);
    let whole = catalog
        .analysis_accounting_aggregate(&policy.policy_id, None)
        .unwrap();
    assert!((whole.uncertain_usage_reserve_usd - 0.5).abs() < 1e-12);
}

#[test]
fn raw_prompt_and_frame_bytes_never_enter_accounting_rows() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(&project, "privacy", 1.0))
        .unwrap();
    let payload = analysis_request_payload_fingerprint(
        DEFAULT_MODEL,
        &crate::VlmRequest {
            prompt: "PRIVATE_PROMPT_CANARY".into(),
            frames: vec!["data:image/jpeg;base64,PRIVATE_FRAME_CANARY".into()],
            schema: json!({"type":"object"}),
        },
    )
    .unwrap();
    let record = catalog
        .reserve_analysis_request(
            &operation_context(&project, "privacy", "intent", 1.0),
            DEFAULT_MODEL,
            &payload,
            &bound(0.1),
        )
        .unwrap();
    let evidence = json!({
        "request": record,
        "events": catalog.analysis_request_events(&record.request_key).unwrap(),
        "authority": catalog.analysis_budget_authority_evidence(&record.budget_scope_id).unwrap(),
    })
    .to_string();
    assert!(!evidence.contains("PRIVATE_PROMPT_CANARY"));
    assert!(!evidence.contains("PRIVATE_FRAME_CANARY"));
}
