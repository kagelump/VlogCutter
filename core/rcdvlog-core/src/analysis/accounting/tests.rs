//! Focused offline admission tests.
//!
//! Tests cover local release before dispatch, retained uncertainty after it,
//! interruption, concurrent final-budget races, late receipts and legacy spending
//! written after an authority snapshot. Runtime transport wiring is separate.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use serde_json::json;
use tempfile::tempdir;

use super::*;
use crate::analysis::openrouter_stt::{
    NativeSttAudioWindow, NativeSttProviderPolicy, NativeSttRequest, OPENROUTER_STT_PROVIDER,
    OPENROUTER_WHISPER_MODEL, SttDataCollection, build_native_stt_request_body,
    native_stt_request_cost_bound,
};
use crate::analysis::provider::{
    AnalysisRequestCostBound, DEFAULT_MODEL, FALLBACK_MODEL, ProviderChain, ProviderFailure,
    ProviderFailureCategory, VlmProvider, VlmReceipt, VlmRequest, VlmResponse,
    analysis_cost_bound_contract_id,
};
use crate::analysis::run_policy::{
    AnalysisPayload, RunAssetScope, RunPolicy, RunPolicyContext, RunPolicyParameters,
    RunPolicyReadiness,
};
use crate::analysis::windows::{SpeechRegion, pack_windows};
use crate::{AnalysisRunKind, Catalog};

mod native_boundary_regressions;

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

fn native_request(
    language: &str,
    temperature: f64,
    data_collection: SttDataCollection,
) -> NativeSttRequest {
    NativeSttRequest::new(
        OPENROUTER_STT_PROVIDER,
        OPENROUTER_WHISPER_MODEL,
        Some(language.to_string()),
        temperature,
        NativeSttProviderPolicy::new(true, data_collection),
    )
    .unwrap()
}

fn native_audio_window(
    sample_value: f32,
    source_start_ms: u32,
    duration_ms: u32,
) -> NativeSttAudioWindow {
    let source_end_ms = source_start_ms.checked_add(duration_ms).unwrap();
    let pcm = vec![sample_value; source_end_ms as usize * 16];
    let window = pack_windows(&[SpeechRegion {
        start_ms: source_start_ms,
        end_ms: source_end_ms,
    }])
    .remove(0);
    NativeSttAudioWindow::from_pcm(&pcm, &window).unwrap()
}

fn legacy_native_absent_bound_json() -> &'static str {
    r#"{"maximumCostUsd":0.01,"contractVersion":"synthetic.transport-declared.v1","promptTokensMax":1,"completionTokensMax":1,"imageCount":0,"promptPriceUsdPerMillion":0.0,"completionPriceUsdPerMillion":0.0,"imagePriceUsdEach":0.0,"requestPriceUsdEach":0.01,"assumptions":["synthetic transport declares a deterministic test-only bound"]}"#
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
        billed_audio_seconds: None,
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

fn pilot_record_json(
    project_id: &str,
    policy_id: &str,
    run_id: &str,
    cost_usd: serde_json::Value,
    usage_complete: serde_json::Value,
) -> String {
    json!({
        "schemaVersion": crate::RUN_PILOT_SCHEMA_VERSION,
        "policyId": policy_id,
        "projectId": project_id,
        "status": "uncertain",
        "startedAt": "2026-10-24T00:00:00Z",
        "finishedAt": "2026-10-24T00:00:01Z",
        "runId": run_id,
        "assetId": "synthetic-asset",
        "requestedModel": DEFAULT_MODEL,
        "costUsd": cost_usd,
        "usageComplete": usage_complete,
    })
    .to_string()
}

fn upsert_pilot_json(catalog: &Catalog, project_id: &str, policy_id: &str, record_json: &str) {
    catalog
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO analysis_run_pilots(
                policy_id,project_id,status,record_json,started_at,finished_at)
             VALUES(?1,?2,'uncertain',?3,'2026-10-24T00:00:00Z','2026-10-24T00:00:01Z')
             ON CONFLICT(policy_id) DO UPDATE SET
                status=excluded.status,record_json=excluded.record_json,
                started_at=excluded.started_at,finished_at=excluded.finished_at",
            rusqlite::params![policy_id, project_id, record_json],
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
    upsert_pilot_json(
        catalog,
        project_id,
        policy_id,
        &pilot_record_json(
            project_id,
            policy_id,
            "legacy-pilot-run",
            cost_usd,
            usage_complete,
        ),
    );
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

fn insert_accounted_run(
    catalog: &Catalog,
    project_id: &str,
    policy_id: &str,
    run_id: &str,
    status: &str,
    stage: &str,
    cost_usd: f64,
) {
    insert_run(
        catalog, project_id, policy_id, run_id, status, stage, cost_usd,
    );
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE analysis_runs SET accounting_contract=?2 WHERE id=?1",
            rusqlite::params![run_id, ANALYSIS_ACCOUNTING_SCHEMA_VERSION],
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
    insert_accounted_run(
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
    insert_accounted_run(
        &catalog,
        &project,
        policy_id,
        "run-current",
        "running",
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
    insert_accounted_run(
        &catalog,
        &project,
        policy_id,
        "run-current",
        "running",
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
    insert_accounted_run(
        &catalog, &project, policy_id, "run-old", "running", "bulk", 0.0,
    );
    insert_accounted_run(
        &catalog, &project, policy_id, "run-new", "running", "bulk", 0.0,
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
        billed_audio_seconds: None,
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

#[test]
fn post_initialization_untracked_run_and_pilot_floors_are_preserved_and_fail_closed() {
    let (_directory, catalog, project) = fixture();
    let policy_id = "post-init-untracked";
    let source = "source";
    let policy = project_policy(&project, policy_id, 2.0, source);
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    insert_run(
        &catalog,
        &project,
        policy_id,
        "frozen-legacy-run",
        "completed",
        "bulk",
        0.30,
    );
    let first = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    assert!((first.legacy_uncertain_usage_reserve_usd - 0.30).abs() < 1e-12);
    assert!(first.dispatch_stopped);

    // An old unwired runtime writes after the authority snapshot. It has no
    // accounting marker, so it is new unknown usage rather than frozen history.
    insert_run(
        &catalog,
        &project,
        policy_id,
        "post-init-untracked-run",
        "completed",
        "bulk",
        0.70,
    );
    insert_pilot(&catalog, &project, policy_id, json!(0.40), json!(false));
    let second = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    assert!((second.legacy_uncertain_usage_reserve_usd - 0.30).abs() < 1e-12);
    assert!(second.post_initialization_untracked_usage_reserve_usd >= 0.70);
    assert!(second.post_initialization_untracked_evidence_count >= 2);
    assert!(second.dispatch_stopped);
    let error = catalog
        .reserve_analysis_request(
            &policy_context(&project, policy_id, "", "blocked", 2.0, source),
            DEFAULT_MODEL,
            "payload-a",
            &bound(0.10),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("dispatch is stopped"));

    // Re-initializing does not append duplicate evidence.
    let third = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    assert_eq!(
        third.post_initialization_untracked_evidence_count,
        second.post_initialization_untracked_evidence_count
    );
}

#[test]
fn operation_refresh_combines_cache_and_unmarked_run_floors() {
    let (_directory, catalog, project) = fixture();
    let authority = operation_authority(&project, CHRONOLOGY_SUGGESTIONS_OPERATION, 1.0);
    let first = catalog
        .initialize_analysis_accounting_authority(&authority)
        .unwrap();
    assert!(!first.dispatch_stopped);

    catalog
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO analysis_chronology_suggestions(
                request_key,project_id,facts_revision,status,result_json,model,attempts,
                cost_usd,prompt_tokens,completion_tokens,latency_ms,created_at,updated_at)
             VALUES('late-cache',?1,'rev','completed',?2,'old-model',1,0.005,1,1,1,?3,?3)",
            rusqlite::params![
                project,
                r#"{"provenance":{"usageComplete":true}}"#,
                crate::now()
            ],
        )
        .unwrap();
    insert_run(
        &catalog,
        &project,
        "",
        "late-unmarked-operation-run",
        "completed",
        CHRONOLOGY_SUGGESTIONS_OPERATION,
        0.05,
    );

    let second = catalog
        .initialize_analysis_accounting_authority(&authority)
        .unwrap();
    assert!(second.dispatch_stopped);
    assert!(
        second.post_initialization_untracked_usage_reserve_usd >= 0.055 - 1e-12,
        "both cache and run evidence must contribute"
    );
    assert!(second.post_initialization_untracked_evidence_count >= 2);
    let count = second.post_initialization_untracked_evidence_count;
    let repeated = catalog
        .initialize_analysis_accounting_authority(&authority)
        .unwrap();
    assert_eq!(repeated.post_initialization_untracked_evidence_count, count);
}

#[test]
fn frozen_unmarked_operation_run_observes_later_cost_increase_without_duplicate_credit() {
    let (_directory, catalog, project) = fixture();
    let authority = operation_authority(&project, LABEL_CONSOLIDATION_OPERATION, 2.0);
    insert_run(
        &catalog,
        &project,
        "",
        "frozen-operation-run",
        "completed",
        LABEL_CONSOLIDATION_OPERATION,
        0.10,
    );
    let first = catalog
        .initialize_analysis_accounting_authority(&authority)
        .unwrap();
    assert!(first.dispatch_stopped);
    assert!((first.legacy_uncertain_usage_reserve_usd - 0.10).abs() < 1e-12);

    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE analysis_runs SET cost_usd=0.30 WHERE id='frozen-operation-run'",
            [],
        )
        .unwrap();
    let increased = catalog
        .initialize_analysis_accounting_authority(&authority)
        .unwrap();
    assert!(
        (increased.post_initialization_untracked_usage_reserve_usd - 0.20).abs() < 1e-12,
        "the frozen credit must count once and only the later delta appended"
    );
    let count = increased.post_initialization_untracked_evidence_count;
    let repeated = catalog
        .initialize_analysis_accounting_authority(&authority)
        .unwrap();
    assert_eq!(repeated.post_initialization_untracked_evidence_count, count);
    assert!((repeated.post_initialization_untracked_usage_reserve_usd - 0.20).abs() < 1e-12);
}

#[test]
fn reserve_persists_new_untracked_evidence_before_denying_admission() {
    let (directory, catalog, project) = fixture();
    let policy_id = "post-init-reserve-boundary";
    let source = "source";
    let policy = project_policy(&project, policy_id, 2.0, source);
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    let initial = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    assert!(!initial.dispatch_stopped);
    let run_id = catalog
        .begin_accounted_analysis_run(&project, policy_id, "pilot")
        .unwrap();
    insert_run(
        &catalog,
        &project,
        policy_id,
        "late-untracked-before-direct-reserve",
        "completed",
        "bulk",
        0.70,
    );
    let context = policy_context(
        &project,
        policy_id,
        &run_id,
        "direct-reserve-intent",
        2.0,
        source,
    );
    let error = catalog
        .reserve_analysis_request(&context, DEFAULT_MODEL, "payload-direct", &bound(0.10))
        .unwrap_err();
    assert!(format!("{error:#}").contains("dispatch is stopped"));

    let reopened = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let scope = analysis_policy_budget_scope_id(&project, policy_id);
    let snapshot = reopened
        .analysis_budget_snapshot(&scope)
        .unwrap()
        .expect("authority must survive reopen");
    assert!(snapshot.dispatch_stopped);
    assert!(snapshot.post_initialization_untracked_usage_reserve_usd >= 0.70);
    assert!(snapshot.post_initialization_untracked_evidence_count >= 1);
    assert!(
        reopened
            .list_analysis_requests(&project, Some(policy_id), 10)
            .unwrap()
            .is_empty(),
        "a persisted denial must not create a reservation"
    );

    let before = snapshot.post_initialization_untracked_evidence_count;
    let error = reopened
        .reserve_analysis_request(&context, DEFAULT_MODEL, "payload-direct", &bound(0.10))
        .unwrap_err();
    assert!(format!("{error:#}").contains("dispatch is stopped"));
    let after = reopened.analysis_budget_snapshot(&scope).unwrap().unwrap();
    assert_eq!(
        after.post_initialization_untracked_evidence_count, before,
        "repeated denials must not append duplicate evidence"
    );
}

#[test]
fn dispatch_persists_new_untracked_evidence_before_denying_marker() {
    let (directory, catalog, project) = fixture();
    let policy_id = "post-init-dispatch-boundary";
    let source = "source";
    let policy = project_policy(&project, policy_id, 2.0, source);
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    let initial = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    assert!(!initial.dispatch_stopped);
    let run_id = catalog
        .begin_accounted_analysis_run(&project, policy_id, "pilot")
        .unwrap();
    let context = policy_context(
        &project,
        policy_id,
        &run_id,
        "direct-dispatch-intent",
        2.0,
        source,
    );
    let reservation = catalog
        .reserve_analysis_request(&context, DEFAULT_MODEL, "payload-dispatch", &bound(0.10))
        .unwrap();
    assert_eq!(reservation.lifecycle, AnalysisRequestLifecycle::Reserved);

    insert_run(
        &catalog,
        &project,
        policy_id,
        "late-untracked-before-dispatch-marker",
        "completed",
        "bulk",
        0.70,
    );
    let error = catalog
        .mark_analysis_request_dispatched(&reservation.request_key)
        .unwrap_err();
    assert!(format!("{error:#}").contains("dispatch is stopped"));

    let reopened = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let scope = analysis_policy_budget_scope_id(&project, policy_id);
    let snapshot = reopened
        .analysis_budget_snapshot(&scope)
        .unwrap()
        .expect("authority must survive reopen");
    assert!(snapshot.dispatch_stopped);
    assert!(snapshot.post_initialization_untracked_usage_reserve_usd >= 0.70);
    let request = reopened
        .analysis_request(&reservation.request_key)
        .unwrap()
        .unwrap();
    assert_eq!(request.lifecycle, AnalysisRequestLifecycle::Reserved);
    assert!(request.dispatch_at.is_none());
    let before = snapshot.post_initialization_untracked_evidence_count;
    let error = reopened
        .mark_analysis_request_dispatched(&reservation.request_key)
        .unwrap_err();
    assert!(format!("{error:#}").contains("dispatch is stopped"));
    assert_eq!(
        reopened
            .analysis_budget_snapshot(&scope)
            .unwrap()
            .unwrap()
            .post_initialization_untracked_evidence_count,
        before
    );
}

#[test]
fn duplicate_post_initialization_pilot_and_owned_run_count_once_but_independent_runs_count() {
    let (_directory, catalog, project) = fixture();
    let policy_id = "post-init-duplicate-owner";
    let source = "source";
    let policy = project_policy(&project, policy_id, 2.0, source);
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    upsert_pilot_json(
        &catalog,
        &project,
        policy_id,
        &pilot_record_json(
            &project,
            policy_id,
            "duplicate-owner-run",
            json!(0.30),
            json!(false),
        ),
    );
    insert_run(
        &catalog,
        &project,
        policy_id,
        "duplicate-owner-run",
        "completed",
        "pilot",
        0.30,
    );
    insert_run(
        &catalog,
        &project,
        policy_id,
        "independent-unmarked-run",
        "completed",
        "bulk",
        0.20,
    );
    let snapshot = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    assert!((snapshot.post_initialization_untracked_usage_reserve_usd - 0.50).abs() < 1e-12);
    assert_eq!(snapshot.post_initialization_untracked_evidence_count, 2);
    let count = snapshot.post_initialization_untracked_evidence_count;
    let repeated = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    assert_eq!(repeated.post_initialization_untracked_evidence_count, count);
    assert!((repeated.post_initialization_untracked_usage_reserve_usd - 0.50).abs() < 1e-12);
}

#[test]
fn frozen_complete_pilot_credits_its_historical_floor_and_observes_only_increases() {
    let (_directory, catalog, project) = fixture();
    let policy_id = "post-init-pilot-increase";
    let source = "source";
    let policy = project_policy(&project, policy_id, 2.0, source);
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    insert_pilot(&catalog, &project, policy_id, json!(0.20), json!(true));
    let initial = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    assert!((initial.legacy_confirmed_cost_usd - 0.20).abs() < 1e-12);
    assert!(!initial.dispatch_stopped);

    upsert_pilot_json(
        &catalog,
        &project,
        policy_id,
        &pilot_record_json(
            &project,
            policy_id,
            "legacy-pilot-run",
            json!(0.30),
            json!(true),
        ),
    );
    insert_run(
        &catalog,
        &project,
        policy_id,
        "legacy-pilot-run",
        "completed",
        "pilot",
        0.30,
    );
    let increased = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    assert!((increased.legacy_confirmed_cost_usd - 0.20).abs() < 1e-12);
    assert!((increased.post_initialization_untracked_usage_reserve_usd - 0.10).abs() < 1e-12);

    upsert_pilot_json(
        &catalog,
        &project,
        policy_id,
        &pilot_record_json(
            &project,
            policy_id,
            "legacy-pilot-run",
            json!(0.40),
            json!(true),
        ),
    );
    let later = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    assert!((later.post_initialization_untracked_usage_reserve_usd - 0.20).abs() < 1e-12);

    upsert_pilot_json(
        &catalog,
        &project,
        policy_id,
        &pilot_record_json(
            &project,
            policy_id,
            "legacy-pilot-run",
            json!(0.35),
            json!(true),
        ),
    );
    let lower = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    assert!((lower.post_initialization_untracked_usage_reserve_usd - 0.20).abs() < 1e-12);
    let repeated = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    assert!((repeated.post_initialization_untracked_usage_reserve_usd - 0.20).abs() < 1e-12);
}

#[test]
fn unchanged_unreadable_frozen_pilot_is_matched_by_fingerprint_not_added_again() {
    let (_directory, catalog, project) = fixture();
    let policy_id = "post-init-unreadable-fingerprint";
    let source = "source";
    let policy = project_policy(&project, policy_id, 2.0, source);
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    upsert_pilot_json(&catalog, &project, policy_id, "{not valid pilot json");
    let initial = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    assert!(initial.dispatch_stopped);
    assert!((initial.legacy_uncertain_usage_reserve_usd - 2.0).abs() < 1e-12);
    let repeated = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    assert_eq!(repeated.post_initialization_untracked_evidence_count, 0);
    assert_eq!(
        repeated.post_initialization_untracked_usage_reserve_usd,
        0.0
    );
    assert!((repeated.legacy_uncertain_usage_reserve_usd - 2.0).abs() < 1e-12);
}

#[test]
fn pre_fingerprint_frozen_pilot_keeps_its_floor_and_matches_identity_updates() {
    let (_directory, catalog, project) = fixture();
    let policy_id = "post-init-pre-fingerprint";
    let source = "source";
    let policy = project_policy(&project, policy_id, 2.0, source);
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    insert_pilot(&catalog, &project, policy_id, json!(0.20), json!(false));
    // Simulate a frozen authority written by the pre-fingerprint schema. Its
    // immutable floor is retained; the refresh must match the current pilot by
    // identity rather than silently zeroing or double-counting it.
    let scope = analysis_policy_budget_scope_id(&project, policy_id);
    let legacy_evidence = json!({
        "entries": [{
            "source": "analysis_run_pilots",
            "policyId": policy_id,
            "costUsd": 0.20,
            "rawCostUsd": 0.20,
            "usageComplete": false,
            "status": "uncertain",
            "runId": "legacy-pilot-run",
            "runIds": ["legacy-pilot-run"]
        }],
        "present": true
    });
    catalog
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO analysis_budget_authorities(
                budget_scope_id,project_id,policy_id,policy_fingerprint,requested_provider,budget_usd,
                legacy_confirmed_cost_usd,legacy_uncertain_usage_reserve_usd,legacy_usage_complete,
                legacy_evidence_json,dispatch_stopped,stop_reason,created_at,updated_at)
             VALUES(?1,?2,?3,?3,'openrouter',2.0,0.0,0.20,0,?4,1,?5,?6,?6)",
            rusqlite::params![
                scope,
                project,
                policy_id,
                serde_json::to_string(&legacy_evidence).unwrap(),
                LEGACY_UNKNOWN_USAGE_STOP_REASON,
                crate::now(),
            ],
        )
        .unwrap();

    let unchanged = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    assert_eq!(unchanged.post_initialization_untracked_evidence_count, 0);
    assert!((unchanged.legacy_uncertain_usage_reserve_usd - 0.20).abs() < 1e-12);

    upsert_pilot_json(
        &catalog,
        &project,
        policy_id,
        &pilot_record_json(
            &project,
            policy_id,
            "legacy-pilot-run",
            json!(0.30),
            json!(false),
        ),
    );
    let changed = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    assert!((changed.post_initialization_untracked_usage_reserve_usd - 0.10).abs() < 1e-12);
}

#[test]
fn incomplete_zero_post_initialization_pilot_is_not_zero_spend_proof() {
    let (_directory, catalog, project) = fixture();
    let policy_id = "post-init-zero-floor";
    let source = "source";
    let policy = project_policy(&project, policy_id, 2.0, source);
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    upsert_pilot_json(
        &catalog,
        &project,
        policy_id,
        &pilot_record_json(
            &project,
            policy_id,
            "zero-unknown-pilot-run",
            json!(0.0),
            json!(false),
        ),
    );
    insert_run(
        &catalog,
        &project,
        policy_id,
        "zero-unknown-pilot-run",
        "completed",
        "pilot",
        0.0,
    );
    let snapshot = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    assert!(snapshot.dispatch_stopped);
    assert!(
        (snapshot.post_initialization_untracked_usage_reserve_usd - 2.0).abs() < 1e-12,
        "incomplete zero cost must reserve the full conservative ceiling"
    );
    assert_eq!(snapshot.post_initialization_untracked_evidence_count, 1);
}

#[test]
fn new_accounted_zero_request_cancelled_run_is_not_legacy_unknown() {
    let (_directory, catalog, project) = fixture();
    let policy_id = "accounted-zero-request";
    let source = "source";
    let policy = project_policy(&project, policy_id, 1.0, source);
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 1.0))
        .unwrap();
    let run_id = catalog
        .begin_accounted_analysis_run(&project, policy_id, "pilot")
        .unwrap();
    catalog
        .finish_analysis_run(
            &run_id,
            &crate::AnalysisRunOutcome {
                status: "cancelled".into(),
                stage_reached: "pilot".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let snapshot = catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 1.0))
        .unwrap();
    assert!(!snapshot.dispatch_stopped);
    assert_eq!(snapshot.post_initialization_untracked_evidence_count, 0);
    assert_eq!(
        snapshot.post_initialization_untracked_usage_reserve_usd,
        0.0
    );
    assert!(catalog.analysis_runs(&project, 10).unwrap().iter().any(
        |run| run.id == run_id && run.accounting_contract == ANALYSIS_ACCOUNTING_SCHEMA_VERSION
    ));
}

#[test]
fn cumulative_legacy_pilot_preserves_credit_across_replacement_runs_and_immutable_evidence() {
    let (_directory, catalog, project) = fixture();
    let policy_id = "cumulative-post-snapshot";
    let policy = project_policy(&project, policy_id, 2.0, "source");
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    insert_pilot(&catalog, &project, policy_id, json!(0.2), json!(true));
    catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    for (run, cost, expected) in [("later-pilot", 0.3, 0.1), ("latest-pilot", 0.4, 0.2)] {
        insert_run(&catalog, &project, policy_id, run, "failed", "pilot", cost);
        upsert_pilot_json(
            &catalog,
            &project,
            policy_id,
            &pilot_record_json(&project, policy_id, run, json!(cost), json!(false)),
        );
        let snapshot = catalog
            .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
            .unwrap();
        assert!((snapshot.confirmed_cost_usd - 0.2).abs() < 1e-12);
        assert!(
            (snapshot.post_initialization_untracked_usage_reserve_usd - expected).abs() < 1e-12
        );
        assert!(snapshot.dispatch_stopped);
    }
    let connection = catalog.connection().unwrap();
    assert!(
        connection
            .execute(
                "UPDATE analysis_untracked_usage_evidence SET reserve_usd=0",
                []
            )
            .is_err()
    );
    assert!(
        connection
            .execute("DELETE FROM analysis_untracked_usage_evidence", [])
            .is_err()
    );
    connection
        .execute("DELETE FROM projects WHERE id=?1", [&project])
        .unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM analysis_untracked_usage_evidence",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

struct BoundaryProvider {
    calls: Arc<AtomicUsize>,
    bound_usd: f64,
    fail_first_typed_transport: bool,
    body: String,
    receipt_provider: String,
    panic_after_dispatch: bool,
}

impl BoundaryProvider {
    fn new(bound_usd: f64) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            bound_usd,
            fail_first_typed_transport: false,
            body: json!({"ok": true}).to_string(),
            receipt_provider: "synthetic-boundary".into(),
            panic_after_dispatch: false,
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl VlmProvider for BoundaryProvider {
    fn analysis_cost_bound(
        &self,
        _model: &str,
        _request: &VlmRequest,
    ) -> anyhow::Result<AnalysisRequestCostBound> {
        Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
            self.bound_usd,
        ))
    }

    fn analyze(&self, model: &str, _request: &VlmRequest) -> anyhow::Result<VlmResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.panic_after_dispatch {
            panic!("synthetic process interruption after dispatch marker");
        }
        if self.fail_first_typed_transport && call == 1 {
            return Err(ProviderFailure::transport().into());
        }
        Ok(VlmResponse {
            model: model.into(),
            body: self.body.clone(),
            prompt_tokens: 4,
            completion_tokens: 2,
            cost_usd: 0.01,
            latency_ms: 1,
            receipt: VlmReceipt {
                actual_model: Some(model.into()),
                provider: Some(self.receipt_provider.clone()),
                generation_id: Some(format!("boundary-generation-{call}")),
                usage_complete: true,
            },
        })
    }
}

fn boundary_request() -> VlmRequest {
    VlmRequest {
        prompt: "synthetic accounted boundary prompt".into(),
        frames: vec!["data:image/jpeg;base64,AA==".into()],
        schema: json!({"type": "object"}),
    }
}

#[test]
fn accounted_boundary_reserves_chain_fallback_attempts_independently() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(
            &project,
            "boundary-chain",
            1.0,
        ))
        .unwrap();
    let run_id = catalog
        .begin_accounted_analysis_run(&project, "", "pilot")
        .unwrap();
    let mut context = operation_context(&project, "boundary-chain", "intent", 1.0);
    context.run_id = run_id;
    let provider = BoundaryProvider {
        fail_first_typed_transport: true,
        ..BoundaryProvider::new(0.02)
    };
    let accounted = AccountedVlmProvider::new(&catalog, context, &provider);
    let chain = ProviderChain::with_policy(
        DEFAULT_MODEL,
        &[DEFAULT_MODEL.into(), FALLBACK_MODEL.into()],
        true,
    )
    .unwrap();

    let outcome = chain.analyze(&accounted, &boundary_request()).unwrap();
    assert_eq!(outcome.response.model, FALLBACK_MODEL);
    assert_eq!(provider.call_count(), 2);

    let requests = catalog.list_analysis_requests(&project, None, 10).unwrap();
    assert_eq!(requests.len(), 2, "each chain attempt needs its own row");
    let uncertain = requests
        .iter()
        .find(|request| request.lifecycle == AnalysisRequestLifecycle::Uncertain)
        .expect("first transport failure must retain uncertainty");
    assert!(uncertain.dispatch_at.is_some());
    assert!(uncertain.confirmed_cost_usd == 0.0);
    let completed = requests
        .iter()
        .find(|request| request.lifecycle == AnalysisRequestLifecycle::Completed)
        .expect("fallback receipt must settle independently");
    assert_eq!(
        catalog
            .analysis_provider_receipts(&completed.request_key)
            .unwrap()
            .len(),
        1
    );
    assert!(
        catalog
            .analysis_provider_receipts(&uncertain.request_key)
            .unwrap()
            .is_empty(),
        "a typed transport failure without receipt metadata stays unknown"
    );
}

#[test]
fn accounted_boundary_race_for_final_budget_has_zero_actual_calls_overrun() {
    let (directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(
            &project,
            "boundary-race",
            0.10,
        ))
        .unwrap();
    let run_id = catalog
        .begin_accounted_analysis_run(&project, "", "pilot")
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(2));
    let second_catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let mut handles = Vec::new();
    for (index, runner) in [catalog.clone(), second_catalog].into_iter().enumerate() {
        let barrier = Arc::clone(&barrier);
        let calls = Arc::clone(&calls);
        let project = project.clone();
        let run_id = run_id.clone();
        handles.push(thread::spawn(move || {
            let mut context =
                operation_context(&project, "boundary-race", &format!("intent-{index}"), 0.10);
            context.run_id = run_id;
            let provider = BoundaryProvider {
                calls,
                ..BoundaryProvider::new(0.10)
            };
            let accounted = AccountedVlmProvider::new(&runner, context, &provider);
            barrier.wait();
            accounted.analyze(DEFAULT_MODEL, &boundary_request())
        }));
    }
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the losing worker must make zero provider calls"
    );
    assert_eq!(
        catalog
            .list_analysis_requests(&project, None, 10)
            .unwrap()
            .len(),
        1,
        "the denied worker must not create a reservation"
    );
    let snapshot = catalog
        .analysis_budget_snapshot(&analysis_operation_budget_scope_id(
            &project,
            "boundary-race",
        ))
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.active_requests, 0);
    assert!(snapshot.confirmed_cost_usd <= 0.10 + 1e-12);
}

#[test]
fn local_admission_denial_does_not_fallback_or_poison_provider_health() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(
            &project,
            "boundary-denial",
            0.01,
        ))
        .unwrap();
    let run_id = catalog
        .begin_accounted_analysis_run(&project, "", "pilot")
        .unwrap();
    let mut context = operation_context(&project, "boundary-denial", "intent", 0.01);
    context.run_id = run_id;
    let provider = BoundaryProvider::new(0.02);
    let accounted = AccountedVlmProvider::new(&catalog, context, &provider);
    let chain = ProviderChain::with_policy(
        DEFAULT_MODEL,
        &[DEFAULT_MODEL.into(), FALLBACK_MODEL.into()],
        true,
    )
    .unwrap();
    let error = chain.analyze(&accounted, &boundary_request()).unwrap_err();
    assert_eq!(
        ProviderFailure::from_anyhow(&error).category,
        ProviderFailureCategory::LocalBudgetExhausted
    );
    assert_eq!(provider.call_count(), 0);
    assert_eq!(chain.default_failures(), 0);
    assert!(
        catalog
            .list_analysis_requests(&project, None, 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn missing_run_id_is_typed_local_admission_without_calls_or_health_poison() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(
            &project,
            "boundary-missing-run",
            1.0,
        ))
        .unwrap();
    let mut context =
        operation_context(&project, "boundary-missing-run", "missing-run-intent", 1.0);
    context.run_id.clear();
    let provider = BoundaryProvider::new(0.02);
    let accounted = AccountedVlmProvider::new(&catalog, context, &provider);
    let chain = ProviderChain::with_policy(
        DEFAULT_MODEL,
        &[DEFAULT_MODEL.into(), FALLBACK_MODEL.into()],
        true,
    )
    .unwrap();
    let error = chain.analyze(&accounted, &boundary_request()).unwrap_err();
    let failure = ProviderFailure::from_anyhow(&error);
    assert_eq!(failure.category, ProviderFailureCategory::LocalAdmission);
    assert!(failure.safe_detail().contains("reconcile"));
    assert_eq!(provider.call_count(), 0);
    assert_eq!(chain.default_failures(), 0);
    assert!(
        catalog
            .list_analysis_requests(&project, None, 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn stable_provider_failure_text_never_contains_error_canaries() {
    let canary = "PRIVATE_PROVIDER_BODY_AND_PATH_CANARY";
    let error = anyhow::Error::new(ProviderFailure::transport())
        .context(format!("synthetic exception {canary} /private/canary.mp4"));
    let failure = ProviderFailure::from_anyhow(&error);
    assert_eq!(failure.category, ProviderFailureCategory::Transport);
    assert!(!failure.safe_detail().contains(canary));
    assert!(!failure.to_string().contains(canary));
    assert!(
        ProviderFailure::local_admission()
            .safe_detail()
            .contains("explicit retry")
    );
    assert!(
        ProviderFailure::local_post_dispatch()
            .safe_detail()
            .contains("reconcile")
    );
}

#[test]
fn crashed_process_after_dispatch_marker_is_never_blindly_resent() {
    let (directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(
            &project,
            "boundary-crash",
            1.0,
        ))
        .unwrap();
    let run_id = catalog
        .begin_accounted_analysis_run(&project, "", "pilot")
        .unwrap();
    let mut context = operation_context(&project, "boundary-crash", "intent", 1.0);
    context.run_id = run_id;
    let crashing = BoundaryProvider {
        panic_after_dispatch: true,
        ..BoundaryProvider::new(0.02)
    };
    let accounted = AccountedVlmProvider::new(&catalog, context.clone(), &crashing);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        accounted.analyze(DEFAULT_MODEL, &boundary_request())
    }));

    let reopened = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let requests = reopened.list_analysis_requests(&project, None, 10).unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.lifecycle, AnalysisRequestLifecycle::Dispatched);
    assert_eq!(
        reopened
            .analysis_request_needs_reconciliation(&request.request_key)
            .unwrap(),
        Some(AnalysisRequestLifecycle::Dispatched)
    );

    let retry_provider = BoundaryProvider::new(0.02);
    let retry = AccountedVlmProvider::new(&reopened, context, &retry_provider);
    let error = retry
        .analyze(DEFAULT_MODEL, &boundary_request())
        .unwrap_err();
    assert_eq!(
        ProviderFailure::from_anyhow(&error).category,
        ProviderFailureCategory::LocalAdmission
    );
    assert_eq!(retry_provider.call_count(), 0);

    let reconciled = reopened
        .reconcile_analysis_request(
            &request.request_key,
            &AnalysisRequestReconciliation::UncertainOutcome {
                failure_code: "process_interrupted_after_dispatch".into(),
            },
        )
        .unwrap();
    assert_eq!(reconciled.lifecycle, AnalysisRequestLifecycle::Uncertain);
    assert!(reconciled.uncertain_usage_reserve_usd > 0.0);
}

#[test]
fn cancellation_after_reservation_releases_only_before_dispatch_marker() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(
            &project,
            "boundary-cancel",
            1.0,
        ))
        .unwrap();
    let run_id = catalog
        .begin_accounted_analysis_run(&project, "", "pilot")
        .unwrap();
    let mut context = operation_context(&project, "boundary-cancel", "intent", 1.0);
    context.run_id = run_id;
    let provider = BoundaryProvider::new(0.02);
    let accounted = AccountedVlmProvider::new(&catalog, context, &provider);
    let probes = AtomicUsize::new(0);
    let error = accounted
        .analyze_with_cancel(DEFAULT_MODEL, &boundary_request(), &mut || {
            probes.fetch_add(1, Ordering::SeqCst) == 1
        })
        .unwrap_err();
    assert_eq!(
        ProviderFailure::from_anyhow(&error).category,
        ProviderFailureCategory::Cancelled
    );
    assert_eq!(provider.call_count(), 0);
    let requests = catalog.list_analysis_requests(&project, None, 10).unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].lifecycle, AnalysisRequestLifecycle::Released);
    assert!(requests[0].dispatch_at.is_none());
    assert!(requests[0].uncertain_usage_reserve_usd == 0.0);
}

#[test]
fn current_run_provenance_uses_receipt_event_order_not_timestamps() {
    let (_directory, catalog, project) = fixture();
    let policy_id = "event-order-provenance";
    let source = "synthetic-source-fingerprint";
    let mut policy = project_policy(&project, policy_id, 1.0, source);
    policy.allowed_models = vec![DEFAULT_MODEL.into(), FALLBACK_MODEL.into()];
    policy.allow_fallback = true;
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 1.0))
        .unwrap();
    let run_id = catalog
        .begin_accounted_analysis_run(&project, policy_id, "pilot")
        .unwrap();
    let context = policy_context(&project, policy_id, &run_id, "same-intent", 1.0, source);

    let primary = catalog
        .reserve_analysis_request(&context, DEFAULT_MODEL, "primary-payload", &bound(0.10))
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&primary.request_key)
        .unwrap();
    catalog
        .record_analysis_request_receipt(
            &primary.request_key,
            &receipt(
                "primary-provider",
                "primary-generation",
                DEFAULT_MODEL,
                0.01,
                true,
            ),
        )
        .unwrap();
    // Wall-clock skew: the first receipt claims a much later settlement time.
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE analysis_request_reservations SET settled_at=?2 WHERE request_key=?1",
            rusqlite::params![primary.request_key, "2999-01-01T00:00:00Z"],
        )
        .unwrap();

    let fallback = catalog
        .reserve_analysis_request(&context, FALLBACK_MODEL, "fallback-payload", &bound(0.10))
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&fallback.request_key)
        .unwrap();
    catalog
        .record_analysis_request_receipt(
            &fallback.request_key,
            &receipt(
                "fallback-provider",
                "fallback-generation",
                FALLBACK_MODEL,
                0.01,
                true,
            ),
        )
        .unwrap();
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE analysis_request_reservations SET settled_at=?2 WHERE request_key=?1",
            rusqlite::params![fallback.request_key, "2000-01-01T00:00:00Z"],
        )
        .unwrap();

    let aggregate = catalog
        .analysis_accounting_aggregate_for_operation(policy_id, "pilot", Some(&run_id))
        .unwrap();
    assert_eq!(
        aggregate.current_run_actual_provider.as_deref(),
        Some("fallback-provider"),
        "event insertion order identifies the actual final fallback"
    );
    assert_eq!(
        aggregate.current_run_actual_model.as_deref(),
        Some(FALLBACK_MODEL)
    );
    assert_eq!(
        aggregate.current_run_generation_id.as_deref(),
        Some("fallback-generation")
    );

    // Even when both settlement timestamps are exactly equal, event insertion
    // order still identifies the actual final fallback.
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE analysis_request_reservations
             SET settled_at='2026-01-01T00:00:00Z'
             WHERE request_key IN (?1,?2)",
            rusqlite::params![primary.request_key, fallback.request_key],
        )
        .unwrap();
    let equal_timestamps = catalog
        .analysis_accounting_aggregate_for_operation(policy_id, "pilot", Some(&run_id))
        .unwrap();
    assert_eq!(
        equal_timestamps.current_run_actual_provider.as_deref(),
        Some("fallback-provider"),
        "equal timestamps must not fall back to summary/request-key ordering"
    );
    assert_eq!(
        equal_timestamps.current_run_generation_id.as_deref(),
        Some("fallback-generation")
    );

    let projection = catalog
        .analysis_operation_attempt_projection(policy_id, "pilot")
        .unwrap();
    assert_eq!(projection.actual_attempt_count, 2);
    assert_eq!(projection.prompt_tokens, 20);
    assert_eq!(projection.completion_tokens, 10);

    // A later metadata-only receipt event must not multiply or erase the known
    // token evidence from the latest token-bearing receipt.
    let metadata_only = ProviderReceiptEvidence {
        provider: "fallback-provider".into(),
        provider_generation_id: "fallback-generation".into(),
        actual_model: FALLBACK_MODEL.into(),
        reported_cost_usd: None,
        prompt_tokens: None,
        completion_tokens: None,
        usage_complete: false,
        billed_audio_seconds: None,
    };
    catalog
        .record_analysis_request_receipt(&fallback.request_key, &metadata_only)
        .unwrap();
    catalog
        .record_analysis_request_receipt(&fallback.request_key, &metadata_only)
        .unwrap();
    let repeated = catalog
        .analysis_operation_attempt_projection(policy_id, "pilot")
        .unwrap();
    assert_eq!(repeated.actual_attempt_count, 2);
    assert_eq!(repeated.prompt_tokens, 20);
    assert_eq!(repeated.completion_tokens, 10);
}

#[test]
fn late_old_run_receipt_cannot_win_current_run_event_provenance() {
    let (_directory, catalog, project) = fixture();
    let policy_id = "event-order-old-run";
    let source = "synthetic-source-fingerprint";
    let mut policy = project_policy(&project, policy_id, 2.0, source);
    policy.allowed_models = vec![DEFAULT_MODEL.into(), FALLBACK_MODEL.into()];
    policy.allow_fallback = true;
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 2.0))
        .unwrap();
    for run_id in ["run-old-event", "run-new-event"] {
        let id = catalog
            .begin_accounted_analysis_run(&project, policy_id, "pilot")
            .unwrap();
        // begin generates a UUID; use a fixed test-only identity update so the
        // aggregate can address each run explicitly.
        catalog
            .connection()
            .unwrap()
            .execute(
                "UPDATE analysis_runs SET id=?2 WHERE id=?1",
                rusqlite::params![id, run_id],
            )
            .unwrap();
    }

    let old_context = policy_context(
        &project,
        policy_id,
        "run-old-event",
        "old-intent",
        2.0,
        source,
    );
    let old = catalog
        .reserve_analysis_request(&old_context, DEFAULT_MODEL, "old-payload", &bound(0.10))
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&old.request_key)
        .unwrap();
    catalog
        .mark_analysis_request_uncertain(&old.request_key, "provider_error:transport")
        .unwrap();

    let new_context = policy_context(
        &project,
        policy_id,
        "run-new-event",
        "new-intent",
        2.0,
        source,
    );
    let current = catalog
        .reserve_analysis_request(&new_context, FALLBACK_MODEL, "new-payload", &bound(0.10))
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&current.request_key)
        .unwrap();
    catalog
        .record_analysis_request_receipt(
            &current.request_key,
            &receipt(
                "current-provider",
                "current-generation",
                FALLBACK_MODEL,
                0.02,
                true,
            ),
        )
        .unwrap();

    // A late receipt for the older run appends a newer event globally, but the
    // current-run provenance must filter by run identity before event ordering.
    catalog
        .reconcile_analysis_request(
            &old.request_key,
            &AnalysisRequestReconciliation::LateReceipt {
                receipt: receipt(
                    "late-old-provider",
                    "late-old-generation",
                    DEFAULT_MODEL,
                    0.01,
                    true,
                ),
            },
        )
        .unwrap();
    let aggregate = catalog
        .analysis_accounting_aggregate_for_operation(policy_id, "pilot", Some("run-new-event"))
        .unwrap();
    assert_eq!(
        aggregate.current_run_actual_provider.as_deref(),
        Some("current-provider")
    );
    assert_eq!(
        aggregate.current_run_generation_id.as_deref(),
        Some("current-generation")
    );
}

#[test]
fn missing_receipt_identity_stays_unknown_in_current_run_provenance() {
    let (_directory, catalog, project) = fixture();
    let policy_id = "event-order-unknown-provider";
    let source = "synthetic-source-fingerprint";
    let policy = project_policy(&project, policy_id, 1.0, source);
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 1.0))
        .unwrap();
    let run_id = catalog
        .begin_accounted_analysis_run(&project, policy_id, "pilot")
        .unwrap();
    let context = policy_context(&project, policy_id, &run_id, "unknown-intent", 1.0, source);
    let request = catalog
        .reserve_analysis_request(&context, DEFAULT_MODEL, "unknown-payload", &bound(0.10))
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&request.request_key)
        .unwrap();
    catalog
        .record_analysis_request_receipt(
            &request.request_key,
            &receipt("", "", DEFAULT_MODEL, 0.01, true),
        )
        .unwrap();
    let aggregate = catalog
        .analysis_accounting_aggregate_for_operation(policy_id, "pilot", Some(&run_id))
        .unwrap();
    assert_eq!(aggregate.current_run_actual_provider, None);
    assert_eq!(
        aggregate.current_run_actual_model.as_deref(),
        Some(DEFAULT_MODEL)
    );
    assert_eq!(aggregate.current_run_generation_id, None);
}

#[test]
fn saturation_denial_does_not_fallback_or_poison_provider_health() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(
            &project,
            "boundary-saturation",
            1.0,
        ))
        .unwrap();
    let run_id = catalog
        .begin_accounted_analysis_run(&project, "", "pilot")
        .unwrap();
    for index in 0..MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY {
        let mut context = operation_context(
            &project,
            "boundary-saturation",
            &format!("held-{index}"),
            1.0,
        );
        context.run_id = run_id.clone();
        let request = catalog
            .reserve_analysis_request(
                &context,
                DEFAULT_MODEL,
                &format!("held-payload-{index}"),
                &bound(0.02),
            )
            .unwrap();
        catalog
            .mark_analysis_request_dispatched(&request.request_key)
            .unwrap();
    }

    let provider = BoundaryProvider::new(0.02);
    let mut denied_context = operation_context(&project, "boundary-saturation", "denied", 1.0);
    denied_context.run_id = run_id;
    let accounted = AccountedVlmProvider::new(&catalog, denied_context, &provider);
    let chain = ProviderChain::with_policy(
        DEFAULT_MODEL,
        &[DEFAULT_MODEL.into(), FALLBACK_MODEL.into()],
        true,
    )
    .unwrap();
    let error = chain
        .analyze(&accounted, &boundary_request())
        .expect_err("saturated authority must deny before transport");
    assert_eq!(
        ProviderFailure::from_anyhow(&error).category,
        ProviderFailureCategory::LocalAdmission
    );
    assert_eq!(provider.call_count(), 0);
    assert_eq!(
        chain.default_failures(),
        0,
        "a local denial must not poison provider health"
    );
    assert_eq!(
        catalog
            .list_analysis_requests(&project, None, 20)
            .unwrap()
            .len(),
        MAX_IN_FLIGHT_REQUESTS_PER_BUDGET_AUTHORITY,
        "the denied worker must not create another reservation"
    );
}

#[test]
fn receipt_persistence_failure_after_dispatch_stays_uncertain_without_fallback() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(
            &project,
            "boundary-receipt-failure",
            1.0,
        ))
        .unwrap();
    let run_id = catalog
        .begin_accounted_analysis_run(&project, "", "pilot")
        .unwrap();
    catalog
        .connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER synthetic_receipt_persist_failure
             BEFORE INSERT ON analysis_provider_receipts
             WHEN NEW.provider='persist-fail'
             BEGIN
                 SELECT RAISE(ABORT, 'synthetic receipt persistence failure');
             END;",
        )
        .unwrap();
    let provider = BoundaryProvider {
        receipt_provider: "persist-fail".into(),
        ..BoundaryProvider::new(0.02)
    };
    let mut context = operation_context(&project, "boundary-receipt-failure", "intent", 1.0);
    context.run_id = run_id;
    let accounted = AccountedVlmProvider::new(&catalog, context, &provider);
    let chain = ProviderChain::with_policy(
        DEFAULT_MODEL,
        &[DEFAULT_MODEL.into(), FALLBACK_MODEL.into()],
        true,
    )
    .unwrap();
    let error = chain
        .analyze(&accounted, &boundary_request())
        .expect_err("receipt persistence failure must stop after one paid call");
    assert_eq!(
        ProviderFailure::from_anyhow(&error).category,
        ProviderFailureCategory::LocalPostDispatch
    );
    assert_eq!(provider.call_count(), 1);
    assert_eq!(chain.default_failures(), 0);
    let requests = catalog.list_analysis_requests(&project, None, 10).unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].lifecycle,
        AnalysisRequestLifecycle::Uncertain,
        "a paid response without a durable receipt must remain honestly uncertain"
    );
    assert!(requests[0].dispatch_at.is_some());
    assert!(
        catalog
            .analysis_provider_receipts(&requests[0].request_key)
            .unwrap()
            .is_empty()
    );
    assert_eq!(requests[0].confirmed_cost_usd, 0.0);
    assert!(requests[0].uncertain_usage_reserve_usd > 0.0);
}

#[test]
fn provenance_is_scoped_to_policy_and_preserves_known_metadata_when_receipt_enriches() {
    let (_directory, catalog, project) = fixture();
    let policy = project_policy(&project, "provenance-policy", 1.0, "source");
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    catalog
        .initialize_analysis_accounting_authority(&policy_authority(
            &project,
            &policy.policy_id,
            1.0,
        ))
        .unwrap();
    let run = catalog
        .begin_accounted_analysis_run(&project, &policy.policy_id, "pilot")
        .unwrap();
    let request = catalog
        .reserve_analysis_request(
            &policy_context(&project, &policy.policy_id, &run, "intent", 1.0, "source"),
            DEFAULT_MODEL,
            "payload",
            &bound(0.1),
        )
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&request.request_key)
        .unwrap();
    catalog
        .record_analysis_request_receipt(
            &request.request_key,
            &receipt("actual-upstream", "generation", "", 0.01, true),
        )
        .unwrap();
    let mut enriched = receipt("", "", "actual-model", 0.01, true);
    enriched.prompt_tokens = None;
    enriched.completion_tokens = None;
    catalog
        .record_analysis_request_receipt(&request.request_key, &enriched)
        .unwrap();
    let projection = catalog
        .analysis_accounting_aggregate(&policy.policy_id, Some(&run))
        .unwrap();
    assert_eq!(
        projection.current_run_actual_provider.as_deref(),
        Some("actual-upstream")
    );
    assert_eq!(
        projection.current_run_actual_model.as_deref(),
        Some("actual-model")
    );
    assert_eq!(
        projection.current_run_generation_id.as_deref(),
        Some("generation")
    );
    let other = catalog
        .analysis_accounting_aggregate("other-policy", Some(&run))
        .unwrap();
    assert_eq!(other.current_run_actual_provider, None);
    assert_eq!(other.current_run_generation_id, None);
}

#[test]
fn targeted_retry_dispatches_exact_authorized_payload_once_and_retains_old_unknown() {
    let (directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(
            &project,
            CHRONOLOGY_SUGGESTIONS_OPERATION,
            1.0,
        ))
        .unwrap();
    let payload = VlmRequest {
        prompt: "PRIVATE_ORIGINAL_PROMPT_CANARY".into(),
        frames: vec!["data:image/jpeg;base64,PRIVATE_FRAME_CANARY".into()],
        schema: json!({"type": "object"}),
    };
    let mut context = operation_context(
        &project,
        CHRONOLOGY_SUGGESTIONS_OPERATION,
        "targeted-intent",
        1.0,
    );
    context.source_fingerprint = "targeted-source-identity".into();
    let payload_fingerprint =
        analysis_request_payload_fingerprint(DEFAULT_MODEL, &payload).unwrap();
    let first = catalog
        .reserve_analysis_request(&context, DEFAULT_MODEL, &payload_fingerprint, &bound(0.01))
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&first.request_key)
        .unwrap();
    catalog
        .mark_analysis_request_uncertain(&first.request_key, "provider_error:transport")
        .unwrap();
    let authorization = catalog
        .authorize_analysis_request_retry_for_project(&project, &first.request_key)
        .unwrap();
    assert_eq!(authorization.next_attempt_epoch, 1);

    let mut provider = BoundaryProvider::new(0.01);
    provider.body = "PRIVATE_PROVIDER_RESPONSE_BODY_CANARY".into();
    let reopened = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let report = reopened
        .dispatch_targeted_analysis_retry_with_provider(
            &project,
            &first.request_key,
            1,
            &payload,
            &provider,
        )
        .unwrap();

    assert_eq!(provider.call_count(), 1);
    assert_eq!(report.request.attempt_epoch, 1);
    assert_eq!(
        report.request.lifecycle,
        AnalysisRequestLifecycle::Completed
    );
    assert_eq!(report.attempt_history.len(), 2);
    assert_eq!(
        report.attempt_history[0].lifecycle,
        AnalysisRequestLifecycle::Uncertain
    );
    assert_eq!(report.receipts.len(), 1);
    assert_eq!(
        report.provider_response.as_ref().unwrap().body,
        "PRIVATE_PROVIDER_RESPONSE_BODY_CANARY"
    );
    assert!(report.review_required);
    assert!((report.accounting.confirmed_cost_usd - 0.01).abs() < 1e-12);
    assert!((report.budget.confirmed_cost_usd - 0.01).abs() < 1e-12);
    assert!((report.budget.uncertain_usage_reserve_usd - 0.01).abs() < 1e-12);
    assert!(report.budget.uncertain_usage_reserve_usd > 0.0);

    let connection = reopened.connection().unwrap();
    let stored_events: String = connection
        .query_row(
            "SELECT group_concat(detail_json, '') FROM analysis_request_events",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!stored_events.contains("PRIVATE_ORIGINAL_PROMPT_CANARY"));
    assert!(!stored_events.contains("PRIVATE_FRAME_CANARY"));
    assert!(!stored_events.contains("PRIVATE_PROVIDER_RESPONSE_BODY_CANARY"));

    // A late final receipt can still resolve the old uncertainty exactly once
    // after the newer retry completed; neither attempt erases the other.
    reopened
        .record_analysis_request_receipt(
            &first.request_key,
            &receipt("fake", "old-late-final", DEFAULT_MODEL, 0.01, true),
        )
        .unwrap();
    let after_late = reopened
        .analysis_budget_snapshot(&report.budget.budget_scope_id)
        .unwrap()
        .unwrap();
    assert!((after_late.confirmed_cost_usd - 0.02).abs() < 1e-12);
    assert!(after_late.uncertain_usage_reserve_usd.abs() < 1e-12);
    assert_eq!(
        reopened
            .analysis_attempt_history(&first.request_key)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn targeted_retry_fails_closed_before_any_call_for_payload_epoch_or_cost_mismatch() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(
            &project,
            CHRONOLOGY_SUGGESTIONS_OPERATION,
            1.0,
        ))
        .unwrap();
    let payload = boundary_request();
    let payload_fingerprint =
        analysis_request_payload_fingerprint(DEFAULT_MODEL, &payload).unwrap();
    let mut context = operation_context(
        &project,
        CHRONOLOGY_SUGGESTIONS_OPERATION,
        "targeted-mismatch-intent",
        1.0,
    );
    context.source_fingerprint = "targeted-source-identity".into();
    let first = catalog
        .reserve_analysis_request(&context, DEFAULT_MODEL, &payload_fingerprint, &bound(0.01))
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&first.request_key)
        .unwrap();
    catalog
        .mark_analysis_request_uncertain(&first.request_key, "provider_error:transport")
        .unwrap();
    catalog
        .authorize_analysis_request_retry_for_project(&project, &first.request_key)
        .unwrap();

    let changed_payload = VlmRequest {
        prompt: "different payload".into(),
        ..payload.clone()
    };
    let provider = BoundaryProvider::new(0.01);
    assert!(
        catalog
            .dispatch_targeted_analysis_retry_with_provider(
                &project,
                &first.request_key,
                1,
                &changed_payload,
                &provider,
            )
            .is_err()
    );
    assert_eq!(provider.call_count(), 0);

    assert!(
        catalog
            .dispatch_targeted_analysis_retry_with_provider(
                &project,
                &first.request_key,
                2,
                &payload,
                &provider,
            )
            .is_err()
    );
    assert_eq!(provider.call_count(), 0);

    let changed_cost_provider = BoundaryProvider::new(0.02);
    assert!(
        catalog
            .dispatch_targeted_analysis_retry_with_provider(
                &project,
                &first.request_key,
                1,
                &payload,
                &changed_cost_provider,
            )
            .is_err()
    );
    assert_eq!(changed_cost_provider.call_count(), 0);
    assert_eq!(
        catalog
            .analysis_attempt_history(&first.request_key)
            .unwrap()
            .len(),
        1,
        "failed preflight cannot append a retry epoch"
    );
}

#[test]
fn reconciliation_requires_project_ownership_and_is_idempotent() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(
            &project,
            "reconcile-project",
            1.0,
        ))
        .unwrap();
    let first = catalog
        .reserve_analysis_request(
            &operation_context(&project, "reconcile-project", "intent", 1.0),
            DEFAULT_MODEL,
            "reconcile-payload",
            &bound(0.10),
        )
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&first.request_key)
        .unwrap();
    let late = receipt("fake", "late-idempotent", DEFAULT_MODEL, 0.03, true);
    let evidence = AnalysisRequestReconciliation::LateReceipt {
        receipt: late.clone(),
    };
    let first_report = catalog
        .reconcile_analysis_request_for_project(&project, &first.request_key, &evidence)
        .unwrap();
    let second_report = catalog
        .reconcile_analysis_request_for_project(&project, &first.request_key, &evidence)
        .unwrap();
    assert_eq!(
        first_report.request.lifecycle,
        AnalysisRequestLifecycle::Completed
    );
    assert_eq!(
        second_report.request.lifecycle,
        AnalysisRequestLifecycle::Completed
    );
    assert_eq!(
        catalog
            .analysis_provider_receipts(&first.request_key)
            .unwrap()
            .len(),
        1,
        "repeated late-receipt reconciliation must not grow history"
    );
    assert_eq!(
        catalog
            .analysis_request_events(&first.request_key)
            .unwrap()
            .iter()
            .filter(|event| event.event_kind == "receipt")
            .count(),
        1
    );
    let other_project = catalog.create_project("Other accounting project").unwrap();
    let error = catalog
        .reconcile_analysis_request_for_project(&other_project.id, &first.request_key, &evidence)
        .unwrap_err();
    assert!(format!("{error:#}").contains("does not belong"));
}

#[test]
fn targeted_retry_concurrent_epoch_consumers_allow_exactly_one_paid_call() {
    let (directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(
            &project,
            CHRONOLOGY_SUGGESTIONS_OPERATION,
            1.0,
        ))
        .unwrap();
    let payload = boundary_request();
    let payload_fingerprint =
        analysis_request_payload_fingerprint(DEFAULT_MODEL, &payload).unwrap();
    let mut context = operation_context(
        &project,
        CHRONOLOGY_SUGGESTIONS_OPERATION,
        "targeted-race-intent",
        1.0,
    );
    context.source_fingerprint = "targeted-race-source".into();
    let first = catalog
        .reserve_analysis_request(&context, DEFAULT_MODEL, &payload_fingerprint, &bound(0.01))
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&first.request_key)
        .unwrap();
    catalog
        .mark_analysis_request_uncertain(&first.request_key, "provider_error:transport")
        .unwrap();
    catalog
        .authorize_analysis_request_retry_for_project(&project, &first.request_key)
        .unwrap();

    let other_catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for runner in [catalog.clone(), other_catalog] {
        let barrier = Arc::clone(&barrier);
        let project = project.clone();
        let request_key = first.request_key.clone();
        let payload = payload.clone();
        handles.push(thread::spawn(move || {
            let provider = BoundaryProvider::new(0.01);
            let calls = Arc::new(provider.calls.clone());
            barrier.wait();
            let result = runner.dispatch_targeted_analysis_retry_with_provider(
                &project,
                &request_key,
                1,
                &payload,
                &provider,
            );
            (result, calls.load(Ordering::SeqCst))
        }));
    }
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        results.iter().filter(|(result, _)| result.is_ok()).count(),
        1
    );
    assert_eq!(
        results.iter().filter(|(result, _)| result.is_err()).count(),
        1
    );
    assert_eq!(results.iter().map(|(_, calls)| calls).sum::<usize>(), 1);
    assert_eq!(
        catalog
            .analysis_attempt_history(&first.request_key)
            .unwrap()
            .len(),
        2,
        "only the authorized next epoch may be admitted"
    );
}

#[test]
fn uncertain_premarker_request_cannot_bypass_the_inflight_admission_cap() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(
            &project,
            "uncertain-cap",
            10.0,
        ))
        .unwrap();
    let mut requests = Vec::new();
    for index in 0..4 {
        let context = operation_context(
            &project,
            "uncertain-cap",
            &format!("uncertain-cap-{index}"),
            10.0,
        );
        requests.push(
            catalog
                .reserve_analysis_request(
                    &context,
                    DEFAULT_MODEL,
                    &format!("uncertain-cap-payload-{index}"),
                    &bound(0.01),
                )
                .unwrap(),
        );
    }
    // A marker-absent local unknown is releasable/retryable evidence, not a
    // dispatched in-flight request. It must not later become a fifth wire call
    // while four other reservations still occupy the authority cap.
    catalog
        .mark_analysis_request_uncertain(
            &requests[0].request_key,
            "local_interrupted_before_marker",
        )
        .unwrap();
    let fifth = catalog
        .reserve_analysis_request(
            &operation_context(&project, "uncertain-cap", "uncertain-cap-fifth", 10.0),
            DEFAULT_MODEL,
            "uncertain-cap-payload-fifth",
            &bound(0.01),
        )
        .unwrap();
    assert_eq!(fifth.lifecycle, AnalysisRequestLifecycle::Reserved);

    let error = catalog
        .mark_analysis_request_dispatched(&requests[0].request_key)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("cannot be marked dispatched"),
        "{error:#}"
    );
    let unchanged = catalog
        .analysis_request(&requests[0].request_key)
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.lifecycle, AnalysisRequestLifecycle::Uncertain);
    assert!(unchanged.dispatch_at.is_none());
    let snapshot = catalog
        .analysis_budget_snapshot(&analysis_operation_budget_scope_id(
            &project,
            "uncertain-cap",
        ))
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.active_requests, 4);
}

#[test]
fn targeted_retry_claim_uses_the_same_owner_checks_as_initial_admission() {
    let (_directory, catalog, project) = fixture();
    let other = catalog
        .create_project("Other targeted owner project")
        .unwrap();
    let policy_id = "targeted-owner-policy";
    let source = "targeted-owner-source";
    let policy = project_policy(&project, policy_id, 1.0, source);
    insert_authorization(&catalog, &policy, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    catalog
        .initialize_analysis_accounting_authority(&policy_authority(&project, policy_id, 1.0))
        .unwrap();
    let first_run = catalog
        .begin_accounted_analysis_run(&project, policy_id, "pilot")
        .unwrap();
    let payload_fingerprint = "targeted-owner-payload";
    let first = catalog
        .reserve_analysis_request(
            &policy_context(
                &project,
                policy_id,
                &first_run,
                "targeted-owner-intent",
                1.0,
                source,
            ),
            DEFAULT_MODEL,
            payload_fingerprint,
            &bound(0.01),
        )
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&first.request_key)
        .unwrap();
    catalog
        .mark_analysis_request_uncertain(&first.request_key, "provider_error:transport")
        .unwrap();
    catalog
        .authorize_analysis_request_retry_for_project(&project, &first.request_key)
        .unwrap();
    let cost_bound = bound(0.01);
    let cost_bound_fingerprint = analysis_request_cost_bound_fingerprint(&cost_bound).unwrap();
    let retry_run = catalog
        .begin_accounted_analysis_run(&project, policy_id, "targeted_retry")
        .unwrap();

    let mut admission = AnalysisTargetedRetryAdmission {
        project_id: project.clone(),
        prior_request_key: first.request_key.clone(),
        expected_next_attempt_epoch: 1,
        run_id: retry_run.clone(),
    };
    catalog
        .validate_analysis_targeted_retry_claim(
            &admission,
            DEFAULT_MODEL,
            payload_fingerprint,
            &cost_bound_fingerprint,
            &cost_bound,
        )
        .unwrap();

    admission.run_id = first_run.clone();
    assert!(
        catalog
            .validate_analysis_targeted_retry_claim(
                &admission,
                DEFAULT_MODEL,
                payload_fingerprint,
                &cost_bound_fingerprint,
                &cost_bound,
            )
            .is_err(),
        "the prior interrupted run cannot be reused as a new retry run"
    );
    admission.run_id = retry_run.clone();

    // A run from another project is rejected by the shared owner check.
    insert_accounted_run(
        &catalog,
        &other.id,
        "",
        "targeted-other-project-run",
        "running",
        "targeted_retry",
        0.0,
    );
    admission.run_id = "targeted-other-project-run".into();
    let error = catalog
        .validate_analysis_targeted_retry_claim(
            &admission,
            DEFAULT_MODEL,
            payload_fingerprint,
            &cost_bound_fingerprint,
            &cost_bound,
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("different project"),
        "{error:#}"
    );

    // A legacy/non-accounted run cannot own a new paid admission.
    insert_run(
        &catalog,
        &project,
        policy_id,
        "targeted-legacy-run",
        "running",
        "targeted_retry",
        0.0,
    );
    admission.run_id = "targeted-legacy-run".into();
    let error = catalog
        .validate_analysis_targeted_retry_claim(
            &admission,
            DEFAULT_MODEL,
            payload_fingerprint,
            &cost_bound_fingerprint,
            &cost_bound,
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("durable request accounting"),
        "{error:#}"
    );

    // A terminal run is not a fresh paid permit.
    let finished_run = catalog
        .begin_accounted_analysis_run(&project, policy_id, "targeted_retry")
        .unwrap();
    catalog
        .finish_analysis_run(
            &finished_run,
            &crate::AnalysisRunOutcome {
                status: "failed".into(),
                stage_reached: "targeted_retry".into(),
                ..Default::default()
            },
        )
        .unwrap();
    admission.run_id = finished_run;
    let error = catalog
        .validate_analysis_targeted_retry_claim(
            &admission,
            DEFAULT_MODEL,
            payload_fingerprint,
            &cost_bound_fingerprint,
            &cost_bound,
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("currently running"),
        "{error:#}"
    );

    // Project ownership is checked before any retry epoch is consumed.
    admission.project_id = other.id.clone();
    admission.run_id = retry_run;
    let error = catalog
        .validate_analysis_targeted_retry_claim(
            &admission,
            DEFAULT_MODEL,
            payload_fingerprint,
            &cost_bound_fingerprint,
            &cost_bound,
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("does not belong"),
        "{error:#}"
    );

    // Changed source scope/provider authorization is rejected through the same
    // load_authorized_policy owner used by initial admission.
    let mut changed_scope = policy.clone();
    changed_scope.scope[0].source_identity = "changed-targeted-source".into();
    catalog
        .connection()
        .unwrap()
        .execute(
            "DELETE FROM analysis_run_authorizations WHERE policy_id=?1",
            [policy_id],
        )
        .unwrap();
    insert_authorization(&catalog, &changed_scope, ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    admission.project_id = project.clone();
    let error = catalog
        .validate_analysis_targeted_retry_claim(
            &admission,
            DEFAULT_MODEL,
            payload_fingerprint,
            &cost_bound_fingerprint,
            &cost_bound,
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("outside the frozen authorized scope"),
        "{error:#}"
    );

    let mut changed_provider = policy.clone();
    changed_provider.provider = "changed-targeted-provider".into();
    catalog
        .connection()
        .unwrap()
        .execute(
            "DELETE FROM analysis_run_authorizations WHERE policy_id=?1",
            [policy_id],
        )
        .unwrap();
    insert_authorization(
        &catalog,
        &changed_provider,
        ANALYSIS_ACCOUNTING_SCHEMA_VERSION,
    );
    let error = catalog
        .validate_analysis_targeted_retry_claim(
            &admission,
            DEFAULT_MODEL,
            payload_fingerprint,
            &cost_bound_fingerprint,
            &cost_bound,
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("does not match the frozen authorization"),
        "{error:#}"
    );

    // Revoked authorization cannot be bypassed by a direct boundary caller.
    catalog
        .connection()
        .unwrap()
        .execute(
            "DELETE FROM analysis_run_authorizations WHERE policy_id=?1",
            [policy_id],
        )
        .unwrap();
    let error = catalog
        .validate_analysis_targeted_retry_claim(
            &admission,
            DEFAULT_MODEL,
            payload_fingerprint,
            &cost_bound_fingerprint,
            &cost_bound,
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("not canonically authorized"),
        "{error:#}"
    );
    assert_eq!(
        catalog
            .analysis_attempt_history(&first.request_key)
            .unwrap()
            .len(),
        1,
        "rejected claims must not append retry epochs"
    );
}

#[test]
fn inspection_request_or_run_filters_show_only_the_selected_budget_scopes() {
    let (_directory, catalog, project) = fixture();
    for operation in ["inspect-one", "inspect-two"] {
        catalog
            .initialize_analysis_accounting_authority(&operation_authority(
                &project, operation, 1.0,
            ))
            .unwrap();
        let run_id = catalog
            .begin_accounted_analysis_run(&project, "", operation)
            .unwrap();
        let mut context = operation_context(&project, operation, operation, 1.0);
        context.run_id = run_id;
        catalog
            .reserve_analysis_request(&context, DEFAULT_MODEL, operation, &bound(0.01))
            .unwrap();
    }
    let request = catalog
        .list_analysis_requests(&project, None, 10)
        .unwrap()
        .into_iter()
        .find(|request| request.operation == "inspect-one")
        .unwrap();
    let inspection = catalog
        .inspect_analysis_accounting(
            &project,
            None,
            Some("inspect-one"),
            None,
            Some(&request.request_key),
            10,
            0,
        )
        .unwrap();
    assert_eq!(inspection.requests.len(), 1);
    assert_eq!(inspection.budgets.len(), 1);
    assert_eq!(
        inspection.budgets[0].budget_scope_id,
        analysis_operation_budget_scope_id(&project, "inspect-one")
    );
    assert_eq!(inspection.selection.matching_request_count, 1);

    let run_inspection = catalog
        .inspect_analysis_accounting(
            &project,
            None,
            Some("inspect-two"),
            Some(&request.run_id),
            None,
            10,
            0,
        )
        .unwrap();
    assert!(run_inspection.requests.is_empty());
    assert!(run_inspection.budgets.is_empty());

    let other = catalog.create_project("Other inspection project").unwrap();
    let error = catalog
        .inspect_analysis_accounting(
            &other.id,
            None,
            None,
            None,
            Some(&request.request_key),
            10,
            0,
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("not found"), "{error:#}");
}

#[test]
fn production_targeted_retry_rejects_non_openrouter_route_before_credentials_or_transport() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&AnalysisBudgetAuthorityContext {
            project_id: project.clone(),
            policy_id: String::new(),
            policy_fingerprint: "synthetic-other-route".into(),
            operation: CHRONOLOGY_SUGGESTIONS_OPERATION.into(),
            provider: "synthetic-other-provider".into(),
            budget_scope_id: String::new(),
            budget_usd: 1.0,
        })
        .unwrap();
    let run_id = catalog
        .begin_accounted_analysis_run(&project, "", "targeted_retry")
        .unwrap();
    let payload = boundary_request();
    let payload_fingerprint =
        analysis_request_payload_fingerprint(DEFAULT_MODEL, &payload).unwrap();
    let mut context = operation_context(
        &project,
        CHRONOLOGY_SUGGESTIONS_OPERATION,
        "other-route-intent",
        1.0,
    );
    context.run_id = run_id;
    context.provider = "synthetic-other-provider".into();
    context.policy_fingerprint = "synthetic-other-route".into();
    let first = catalog
        .reserve_analysis_request(&context, DEFAULT_MODEL, &payload_fingerprint, &bound(0.01))
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&first.request_key)
        .unwrap();
    catalog
        .mark_analysis_request_uncertain(&first.request_key, "provider_error:transport")
        .unwrap();
    catalog
        .authorize_analysis_request_retry_for_project(&project, &first.request_key)
        .unwrap();
    let error = catalog
        .dispatch_targeted_analysis_retry(&project, &first.request_key, 1, &payload)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("not the configured OpenRouter route"),
        "{error:#}"
    );
    assert_eq!(
        catalog
            .analysis_attempt_history(&first.request_key)
            .unwrap()
            .len(),
        1,
        "route rejection happens before a new paid run or epoch"
    );
}

#[test]
fn legacy_native_absent_request_roundtrips_and_remains_reconcilable() {
    let (_directory, catalog, project) = fixture();
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(
            &project,
            "legacy-native-absent",
            0.02,
        ))
        .unwrap();
    let legacy_json = legacy_native_absent_bound_json();
    let legacy_bound: AnalysisRequestCostBound = serde_json::from_str(legacy_json).unwrap();
    assert_eq!(legacy_bound.audio, None);
    assert_eq!(serde_json::to_string(&legacy_bound).unwrap(), legacy_json);
    let legacy_fingerprint = analysis_request_cost_bound_fingerprint(&legacy_bound).unwrap();
    assert_eq!(
        legacy_fingerprint,
        blake3::hash(legacy_json.as_bytes()).to_hex().to_string()
    );

    let context = operation_context(&project, "legacy-native-absent", "legacy-intent", 0.02);
    let record = catalog
        .reserve_analysis_request(
            &context,
            DEFAULT_MODEL,
            "legacy-payload-fingerprint",
            &legacy_bound,
        )
        .unwrap();
    catalog
        .mark_analysis_request_dispatched(&record.request_key)
        .unwrap();
    let reconciled = catalog
        .reconcile_analysis_request(
            &record.request_key,
            &AnalysisRequestReconciliation::UncertainOutcome {
                failure_code: "provider_error:transport".into(),
            },
        )
        .unwrap();
    assert_eq!(reconciled.lifecycle, AnalysisRequestLifecycle::Uncertain);
    assert!(reconciled.cost_bound.as_ref().unwrap().audio.is_none());
    assert!((reconciled.uncertain_usage_reserve_usd - 0.01).abs() < 1e-12);
    let inspected = catalog
        .analysis_request(&record.request_key)
        .unwrap()
        .unwrap();
    let inspected_json = serde_json::to_string(&inspected).unwrap();
    assert!(!inspected_json.contains("\"audio\":null"));
    assert_eq!(inspected.cost_bound_fingerprint, legacy_fingerprint);
}

#[test]
fn native_payload_fingerprint_is_deterministic_content_free_and_field_sensitive() {
    let request = native_request("en", 0.0, SttDataCollection::Deny);
    let audio = native_audio_window(0.25, 100, 1_000);
    let fingerprint = analysis_native_stt_payload_fingerprint(&request, &audio).unwrap();
    assert_eq!(
        fingerprint,
        "a70248f1f4b57d703d42c608ae1622b848eb31db2b35e6dcd0b9c722915746d7"
    );
    assert_eq!(
        fingerprint,
        analysis_native_stt_payload_fingerprint(&request, &audio).unwrap()
    );
    assert_eq!(fingerprint.len(), 64);
    assert!(fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()));

    let changed_audio = native_audio_window(0.5, 100, 1_000);
    assert_ne!(
        fingerprint,
        analysis_native_stt_payload_fingerprint(&request, &changed_audio).unwrap()
    );

    // Same constant PCM and duration, but a different source mapping. The WAV
    // bytes are identical while the clip-time origin changed, so only the
    // source-map part of the fingerprint can detect this.
    let remapped = native_audio_window(0.25, 200, 1_000);
    assert_eq!(audio.wav_bytes(), remapped.wav_bytes());
    assert_ne!(
        fingerprint,
        analysis_native_stt_payload_fingerprint(&request, &remapped).unwrap()
    );

    let changed_language = native_request("fr", 0.0, SttDataCollection::Deny);
    assert_ne!(
        fingerprint,
        analysis_native_stt_payload_fingerprint(&changed_language, &audio).unwrap()
    );

    let changed_temperature = native_request("en", 0.5, SttDataCollection::Deny);
    assert_ne!(
        fingerprint,
        analysis_native_stt_payload_fingerprint(&changed_temperature, &audio).unwrap()
    );

    let changed_policy = native_request("en", 0.0, SttDataCollection::Allow);
    assert_ne!(
        fingerprint,
        analysis_native_stt_payload_fingerprint(&changed_policy, &audio).unwrap()
    );

    let body = build_native_stt_request_body(&request, &audio).unwrap();
    let encoded_audio = body["input_audio"]["data"].as_str().unwrap();
    assert!(!fingerprint.contains(encoded_audio));
}

#[test]
fn native_audio_bound_fits_the_existing_ledger_and_race_denies_without_clipping() {
    let (directory, catalog, project) = fixture();
    let request = native_request("en", 0.0, SttDataCollection::Deny);
    let audio = native_audio_window(0.25, 100, 1_000);
    let native_bound = native_stt_request_cost_bound(&request, &audio).unwrap();
    assert!((native_bound.maximum_cost_usd - 0.003).abs() < 1e-15);
    let payload_fingerprint = analysis_native_stt_payload_fingerprint(&request, &audio).unwrap();

    // Exactly the full native reservation fits. The existing canonical ledger
    // must accept it without clipping or inventing chat-token components.
    catalog
        .initialize_analysis_accounting_authority(&operation_authority(
            &project,
            "native-race",
            native_bound.maximum_cost_usd,
        ))
        .unwrap();
    let second_catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for (index, runner) in [catalog.clone(), second_catalog].into_iter().enumerate() {
        let barrier = Arc::clone(&barrier);
        let project = project.clone();
        let model = request.model().to_string();
        let payload_fingerprint = payload_fingerprint.clone();
        let native_bound = native_bound.clone();
        handles.push(thread::spawn(move || {
            let context = operation_context(
                &project,
                "native-race",
                &format!("native-intent-{index}"),
                native_bound.maximum_cost_usd,
            );
            barrier.wait();
            runner.reserve_analysis_request(&context, &model, &payload_fingerprint, &native_bound)
        }));
    }
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    let denied = results
        .iter()
        .find_map(|result| result.as_ref().err())
        .unwrap();
    assert!(
        denied.downcast_ref::<AnalysisBudgetExhausted>().is_some(),
        "race loser must be denied by the shared budget, not an unrelated error: {denied:#}"
    );
    let reserved = results
        .into_iter()
        .find_map(Result::ok)
        .expect("one full native reservation must fit");

    assert!((reserved.reserved_cost_usd - 0.003).abs() < 1e-15);
    let persisted_audio = reserved
        .cost_bound
        .as_ref()
        .and_then(|bound| bound.audio.as_ref())
        .expect("audio component persisted with the request");
    assert_eq!(persisted_audio.input_duration_ms, 1_000);
    assert_eq!(persisted_audio.billable_seconds_max, 30);
    let scope = analysis_operation_budget_scope_id(&project, "native-race");
    let snapshot = catalog.analysis_budget_snapshot(&scope).unwrap().unwrap();
    assert!((snapshot.active_reserved_cost_usd - 0.003).abs() < 1e-15);
    assert_eq!(snapshot.active_requests, 1);

    let body = build_native_stt_request_body(&request, &audio).unwrap();
    let encoded_audio = body["input_audio"]["data"].as_str().unwrap();
    let record_json = serde_json::to_string(&reserved).unwrap();
    assert!(!record_json.contains(encoded_audio));
    assert!(!record_json.contains("input_audio"));
    assert_eq!(reserved.payload_fingerprint, payload_fingerprint);
    assert_eq!(reserved.payload_fingerprint.len(), 64);
}
