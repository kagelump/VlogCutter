//! Financial transaction fixtures, not native-video policy or upload permits.
//! The private Any family isolates settlement from the future transport owner.
use super::*;
use crate::analysis::accounting::usage_pricing::{
    PublishedServiceTier, PublishedTokenTariff, UsagePricedReceipt,
};
use crate::{AnalysisBudgetAuthorityContext, Catalog};
fn fixture() -> (
    tempfile::TempDir,
    Catalog,
    AnalysisAccountingContext,
    AnalysisRequestCostBound,
) {
    let dir = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(dir.path().join("catalog.sqlite3")).unwrap();
    let project = catalog
        .create_project("Public synthetic token accounting")
        .unwrap();
    let context = AnalysisAccountingContext {
        project_id: project.id,
        policy_id: String::new(),
        policy_fingerprint: "usage-pricing-fixture-v1".into(),
        run_id: String::new(),
        operation: "usage-pricing-fixture".into(),
        intended_attempt_key: "request-one".into(),
        source_fingerprint: "synthetic-source".into(),
        provider: "gemini-direct".into(),
        budget_scope_id: String::new(),
        budget_usd: 0.01,
        explicit_retry: false,
    };
    catalog
        .initialize_analysis_accounting_authority(&AnalysisBudgetAuthorityContext {
            project_id: context.project_id.clone(),
            policy_id: context.policy_id.clone(),
            policy_fingerprint: context.policy_fingerprint.clone(),
            operation: context.operation.clone(),
            provider: context.provider.clone(),
            budget_scope_id: String::new(),
            budget_usd: context.budget_usd,
        })
        .unwrap();
    let bound = AnalysisRequestCostBound {
        maximum_cost_usd: 0.0031,
        contract_version: "synthetic-accounted-gemini-token-bound.v1".into(),
        prompt_tokens_max: 2_000,
        completion_tokens_max: 1_000,
        image_count: 0,
        prompt_price_usd_per_million: 0.30,
        completion_price_usd_per_million: 2.50,
        image_price_usd_each: 0.0,
        request_price_usd_each: 0.0,
        audio: None,
        published_token_tariff: Some(PublishedTokenTariff::gemini_35_flash_lite(
            PublishedServiceTier::Flex,
        )),
        assumptions: vec!["Synthetic financial fixture; not authorization to upload.".into()],
    };
    (dir, catalog, context, bound)
}
fn reserve_fixture(
    catalog: &Catalog,
    context: &AnalysisAccountingContext,
    bound: &AnalysisRequestCostBound,
) -> AnalysisRequestRecord {
    reserve_with_family(
        &mut catalog.connection().unwrap(),
        context,
        "gemini-3.5-flash-lite",
        "synthetic-payload",
        bound,
        PolicyRequestFamily::Any,
    )
    .unwrap()
}
fn usage(bound: &AnalysisRequestCostBound) -> ProviderReceiptEvidence {
    ProviderReceiptEvidence {
        provider: "gemini-direct".into(),
        provider_generation_id: "synthetic-generation".into(),
        actual_model: "gemini-3.5-flash-lite".into(),
        reported_cost_usd: None,
        prompt_tokens: Some(1000),
        completion_tokens: Some(300),
        usage_complete: true,
        billed_audio_seconds: None,
        usage_pricing: Some(UsagePricedReceipt {
            tariff_fingerprint: bound
                .published_token_tariff
                .as_ref()
                .unwrap()
                .fingerprint()
                .unwrap(),
            reported_tier: Some("flex".into()),
            prompt_tokens: Some(1000),
            cached_prompt_tokens: Some(800),
            candidate_tokens: Some(200),
            thinking_tokens: Some(100),
            tool_prompt_tokens: Some(0),
            total_tokens: Some(1300),
            latency_ms: 321,
        }),
    }
}
#[test]
fn usage_estimate_counts_cached_input_and_thinking_without_claiming_an_invoice() {
    let (_dir, catalog, context, bound) = fixture();
    let request = reserve_fixture(&catalog, &context, &bound);
    let mut connection = catalog.connection().unwrap();
    mark_dispatched(&mut connection, &request.request_key).unwrap();
    let evidence = usage(&bound);
    let result = record_receipt(&mut connection, &request.request_key, &evidence).unwrap();
    let expected = (200.0 * 0.15 + 800.0 * 0.02 + 300.0 * 1.25) / 1_000_000.0;
    assert_eq!(result.lifecycle, AnalysisRequestLifecycle::Completed);
    assert_eq!(result.estimated_cost_usd, Some(expected));
    assert_eq!(result.reported_cost_usd, None);
    assert_eq!(result.confirmed_cost_usd, 0.0);
    assert_eq!(result.uncertain_usage_reserve_usd, 0.0);
    let snapshot = budget_snapshot(&connection, &request.budget_scope_id)
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.estimated_cost_usd, expected);
    assert_eq!(snapshot.confirmed_cost_usd, 0.0);
    assert_eq!(snapshot.committed_or_reserved_usd, expected);
    let aggregate = accounting_aggregate_for_operation_scope(
        &connection,
        &context.project_id,
        &context.operation,
        None,
        None,
    )
    .unwrap();
    assert_eq!(aggregate.estimated_cost_usd, expected);
    assert_eq!(aggregate.committed_or_reserved_usd, expected);
    let history: i64 = connection
        .query_row("SELECT COUNT(*) FROM analysis_request_events", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        record_receipt(&mut connection, &request.request_key, &evidence).unwrap(),
        result
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM analysis_request_events", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        history
    );
    let json: String = connection
        .query_row(
            "SELECT detail_json FROM analysis_request_events WHERE event_kind='receipt'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        json.contains("thinkingTokens")
            && json.contains("cachedPromptTokens")
            && json.contains("published_rate_estimate")
    );
    assert!(
        reserve_with_family(
            &mut connection,
            &context,
            "gemini-3.5-flash-lite",
            "synthetic-payload",
            &bound,
            PolicyRequestFamily::Any
        )
        .is_err(),
        "a completed request cannot become a second paid attempt"
    );
}
#[test]
fn missing_or_contradictory_usage_retains_the_full_reservation() {
    for case in 0..9 {
        let (_dir, catalog, context, bound) = fixture();
        let request = reserve_fixture(&catalog, &context, &bound);
        let mut connection = catalog.connection().unwrap();
        mark_dispatched(&mut connection, &request.request_key).unwrap();
        let mut evidence = usage(&bound);
        let raw = evidence.usage_pricing.as_mut().unwrap();
        match case {
            0 => raw.thinking_tokens = None,
            1 => raw.cached_prompt_tokens = Some(1001),
            2 => raw.total_tokens = Some(1200),
            3 => raw.reported_tier = Some("standard".into()),
            4 => raw.tariff_fingerprint = "f".repeat(64),
            5 => raw.prompt_tokens = Some(u64::MAX),
            6 => evidence.actual_model = "another-model".into(),
            7 => evidence.usage_complete = false,
            _ => evidence.completion_tokens = Some(200),
        }
        let result = record_receipt(&mut connection, &request.request_key, &evidence).unwrap();
        assert_eq!(
            result.lifecycle,
            AnalysisRequestLifecycle::Uncertain,
            "case {case}"
        );
        assert_eq!(result.estimated_cost_usd, None);
        assert_eq!(result.uncertain_usage_reserve_usd, bound.maximum_cost_usd);
    }
}
#[test]
fn a_late_actual_charge_replaces_the_estimate_once_and_stops_an_overrun() {
    let (_dir, catalog, context, bound) = fixture();
    let request = reserve_fixture(&catalog, &context, &bound);
    let mut connection = catalog.connection().unwrap();
    mark_dispatched(&mut connection, &request.request_key).unwrap();
    let mut evidence = usage(&bound);
    record_receipt(&mut connection, &request.request_key, &evidence).unwrap();
    evidence.usage_pricing = None;
    evidence.reported_cost_usd = Some(0.02);
    let result = record_receipt(&mut connection, &request.request_key, &evidence).unwrap();
    assert_eq!(result.estimated_cost_usd, None);
    assert_eq!(result.confirmed_cost_usd, 0.02);
    let budget = budget_snapshot(&connection, &request.budget_scope_id)
        .unwrap()
        .unwrap();
    assert_eq!(budget.committed_or_reserved_usd, 0.02);
    assert_eq!(budget.estimated_cost_usd, 0.0);
    assert!(budget.dispatch_stopped);
    assert_eq!(budget.available_usd, 0.0);
    evidence.reported_cost_usd = Some(0.01);
    assert!(
        record_receipt(&mut connection, &request.request_key, &evidence).is_err(),
        "an invoice already confirmed stays immutable"
    );
}
#[test]
fn legacy_quotes_keep_their_identity_and_old_transports_do_not_admit_a_token_tariff() {
    let (_dir, catalog, context, bound) = fixture();
    let legacy = AnalysisRequestCostBound::declared_for_synthetic_transport(0.01);
    assert!(
        !serde_json::to_string(&legacy)
            .unwrap()
            .contains("publishedTokenTariff")
    );
    let mut no_tariff = usage(&bound);
    no_tariff.usage_pricing = None;
    assert!(
        !serde_json::to_string(&no_tariff)
            .unwrap()
            .contains("usagePricing")
    );
    assert!(
        reserve(
            &mut catalog.connection().unwrap(),
            &context,
            "gemini-3.5-flash-lite",
            "synthetic-payload",
            &bound
        )
        .is_err()
    );
    let mut changed = bound.clone();
    changed
        .published_token_tariff
        .as_mut()
        .unwrap()
        .input_usd_per_million = 0.0;
    assert!(changed.validate().is_err());
}
#[test]
fn newer_token_evidence_does_not_erase_a_reported_cost_floor_or_prior_estimate() {
    let (_dir, catalog, context, bound) = fixture();
    let request = reserve_fixture(&catalog, &context, &bound);
    let mut connection = catalog.connection().unwrap();
    mark_dispatched(&mut connection, &request.request_key).unwrap();
    let mut evidence = usage(&bound);
    evidence.usage_complete = false;
    evidence.reported_cost_usd = Some(0.005);
    record_receipt(&mut connection, &request.request_key, &evidence).unwrap();
    evidence.reported_cost_usd = None;
    evidence.usage_complete = true;
    let result = record_receipt(&mut connection, &request.request_key, &evidence).unwrap();
    assert_eq!(result.estimated_cost_usd, None);
    assert_eq!(result.uncertain_usage_reserve_usd, 0.005);
    let (_dir, catalog, context, bound) = fixture();
    let request = reserve_fixture(&catalog, &context, &bound);
    let mut connection = catalog.connection().unwrap();
    mark_dispatched(&mut connection, &request.request_key).unwrap();
    let mut evidence = usage(&bound);
    let first = record_receipt(&mut connection, &request.request_key, &evidence).unwrap();
    evidence.usage_pricing = None;
    evidence.prompt_tokens = None;
    evidence.completion_tokens = None;
    evidence.usage_complete = false;
    let enriched = record_receipt(&mut connection, &request.request_key, &evidence).unwrap();
    assert_eq!(enriched.estimated_cost_usd, first.estimated_cost_usd);
    assert_eq!(enriched.lifecycle, AnalysisRequestLifecycle::Completed);
}

#[test]
fn concurrent_reservations_count_the_estimate_once_in_the_same_budget_transaction() {
    let (_dir, catalog, context, bound) = fixture();
    let request = reserve_fixture(&catalog, &context, &bound);
    let mut connection = catalog.connection().unwrap();
    mark_dispatched(&mut connection, &request.request_key).unwrap();
    let completed = record_receipt(&mut connection, &request.request_key, &usage(&bound)).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    let mut workers = Vec::new();
    for index in 0..4 {
        let task_catalog = catalog.clone();
        let mut task_context = context.clone();
        task_context.intended_attempt_key = format!("concurrent-{index}");
        let task_bound = bound.clone();
        let task_barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            task_barrier.wait();
            reserve_with_family(
                &mut task_catalog.connection().unwrap(),
                &task_context,
                "gemini-3.5-flash-lite",
                "concurrent-payload",
                &task_bound,
                PolicyRequestFamily::Any,
            )
            .is_ok()
        }));
    }
    assert_eq!(
        workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .filter(|ok| *ok)
            .count(),
        3
    );
    let budget = budget_snapshot(&connection, &request.budget_scope_id)
        .unwrap()
        .unwrap();
    assert!(
        (budget.committed_or_reserved_usd
            - completed.estimated_cost_usd.unwrap()
            - 3.0 * bound.maximum_cost_usd)
            .abs()
            < 1e-12
    );
    assert!(budget.committed_or_reserved_usd <= context.budget_usd);
}

#[test]
fn old_schema_accounting_reads_are_observational_and_estimates_survive_reopen() {
    let (dir, catalog, context, bound) = fixture();
    let request = reserve_fixture(&catalog, &context, &bound);
    let mut connection = catalog.connection().unwrap();
    mark_dispatched(&mut connection, &request.request_key).unwrap();
    let expected = record_receipt(&mut connection, &request.request_key, &usage(&bound)).unwrap();
    drop(connection);
    drop(catalog);
    let path = dir.path().join("catalog.sqlite3");
    let again = Catalog::open(&path).unwrap();
    assert_eq!(
        again
            .analysis_request(&request.request_key)
            .unwrap()
            .unwrap(),
        expected
    );
    again
        .connection()
        .unwrap()
        .execute_batch("DROP TABLE schema_migrations")
        .unwrap();
    drop(again);
    let replayed = Catalog::open(&path).unwrap();
    assert_eq!(
        replayed
            .analysis_request(&request.request_key)
            .unwrap()
            .unwrap(),
        expected
    );
    // A schema-56 structural fixture has no new estimate projection. Keep its
    // existing rows and migration history; readonly callers must not upgrade it.
    let connection = replayed.connection().unwrap();
    connection.execute_batch("ALTER TABLE analysis_request_reservations DROP COLUMN estimated_cost_usd; DELETE FROM schema_migrations WHERE migration_id=57; PRAGMA user_version=56;").unwrap();
    drop(connection);
    drop(replayed);
    let before = std::fs::read(&path).unwrap();
    let readonly = Catalog::open_read_only_existing(&path).unwrap();
    let observed = readonly
        .analysis_request(&request.request_key)
        .unwrap()
        .unwrap();
    assert_eq!(observed.estimated_cost_usd, None);
    assert_eq!(observed.reported_cost_usd, None);
    drop(readonly);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
