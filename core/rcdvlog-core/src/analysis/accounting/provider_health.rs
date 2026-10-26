//! Shared, bounded provider cooldown state in the project catalog.
//!
//! Rows are keyed by the authorized policy generation, provider, and model so
//! separate asset processes and resumed runs observe the same health state.

use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use thiserror::Error;

use crate::Catalog;

pub const PROVIDER_HEALTH_FAILURE_THRESHOLD: u32 = 3;
pub const PROVIDER_HEALTH_BASE_COOLDOWN_SECS: i64 = 30;
pub const PROVIDER_HEALTH_MAX_COOLDOWN_SECS: i64 = 900;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("provider is cooling down for {remaining_seconds} more seconds")]
pub struct ProviderCooldown {
    pub remaining_seconds: i64,
}

fn cooldown_seconds(failures: u32, retry_after_seconds: Option<i64>) -> i64 {
    let exponent = failures
        .saturating_sub(PROVIDER_HEALTH_FAILURE_THRESHOLD)
        .min(5);
    let bounded = PROVIDER_HEALTH_BASE_COOLDOWN_SECS
        .saturating_mul(1_i64 << exponent)
        .min(PROVIDER_HEALTH_MAX_COOLDOWN_SECS);
    bounded.max(
        retry_after_seconds
            .unwrap_or_default()
            .clamp(0, PROVIDER_HEALTH_MAX_COOLDOWN_SECS),
    )
}

impl Catalog {
    /// Atomically rejects a request that began during an active provider
    /// cooldown. Call after local preparation and before the paid dispatch mark.
    pub(crate) fn check_provider_health(
        &self,
        project_id: &str,
        policy_id: &str,
        provider: &str,
        model: &str,
    ) -> Result<i64> {
        ensure!(
            !policy_id.trim().is_empty(),
            "provider health requires a policy identity"
        );
        let connection = self.connection()?;
        let now = unix_seconds();
        let row: Option<(i64, i64)> = connection
            .query_row(
                "SELECT cooldown_until,revision FROM analysis_provider_health WHERE project_id=?1 AND budget_scope_id=?2 AND provider=?3 AND model=?4",
                params![project_id, policy_id, provider, model],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((until, _)) = row.filter(|(until, _)| *until > now) {
            return Err(ProviderCooldown {
                remaining_seconds: until - now,
            }
            .into());
        }
        Ok(row.map(|(_, revision)| revision).unwrap_or(0))
    }

    /// Record actual transport failures and validated-response rejections.
    pub(crate) fn record_provider_health_failure(
        &self,
        project_id: &str,
        policy_id: &str,
        provider: &str,
        model: &str,
        category: &str,
        retry_after_seconds: Option<i64>,
    ) -> Result<()> {
        ensure!(
            !policy_id.trim().is_empty(),
            "provider health requires a policy identity"
        );
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = unix_seconds();
        let prior: u32 = tx.query_row(
            "SELECT consecutive_failures FROM analysis_provider_health WHERE project_id=?1 AND budget_scope_id=?2 AND provider=?3 AND model=?4",
            params![project_id, policy_id, provider, model],
            |row| row.get(0),
        ).optional()?.unwrap_or(0);
        let failures = prior.saturating_add(1);
        let delay = if failures >= PROVIDER_HEALTH_FAILURE_THRESHOLD {
            cooldown_seconds(failures, retry_after_seconds)
        } else {
            retry_after_seconds
                .unwrap_or_default()
                .clamp(0, PROVIDER_HEALTH_MAX_COOLDOWN_SECS)
        };
        tx.execute(
            "INSERT INTO analysis_provider_health(project_id,budget_scope_id,provider,model,consecutive_failures,cooldown_until,last_failure_category,updated_at,revision) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,1) ON CONFLICT(project_id,budget_scope_id,provider,model) DO UPDATE SET consecutive_failures=excluded.consecutive_failures,cooldown_until=MAX(analysis_provider_health.cooldown_until,excluded.cooldown_until),last_failure_category=excluded.last_failure_category,updated_at=excluded.updated_at,revision=analysis_provider_health.revision+1",
            params![project_id, policy_id, provider, model, failures, now.saturating_add(delay), category, now],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn record_provider_health_success(
        &self,
        project_id: &str,
        policy_id: &str,
        provider: &str,
        model: &str,
        expected_revision: i64,
    ) -> Result<()> {
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE analysis_provider_health SET consecutive_failures=0,cooldown_until=0,last_failure_category='',updated_at=?6,revision=revision+1 WHERE project_id=?1 AND budget_scope_id=?2 AND provider=?3 AND model=?4 AND revision=?5",
            params![project_id, policy_id, provider, model, expected_revision, unix_seconds()],
        )?;
        tx.commit()?;
        Ok(())
    }
}

fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::accounting::{
        AccountedVlmProvider, AnalysisAccountingContext, AnalysisBudgetAuthorityContext,
    };
    use crate::analysis::provider::{
        AnalysisRequestCostBound, ProviderFailure, VlmProvider, VlmRequest, VlmResponse,
    };
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    };

    struct RateLimitedProvider(Arc<AtomicUsize>, Arc<Barrier>);
    impl VlmProvider for RateLimitedProvider {
        fn analysis_cost_bound(&self, _: &str, _: &VlmRequest) -> Result<AnalysisRequestCostBound> {
            Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
                0.01,
            ))
        }
        fn analyze(&self, _: &str, _: &VlmRequest) -> Result<VlmResponse> {
            self.0.fetch_add(1, Ordering::SeqCst);
            self.1.wait();
            Err(ProviderFailure::http_status_with_retry_after(429, Some(60)).into())
        }
    }

    struct InvalidSchemaProvider(Arc<AtomicUsize>);
    impl VlmProvider for InvalidSchemaProvider {
        fn analysis_cost_bound(&self, _: &str, _: &VlmRequest) -> Result<AnalysisRequestCostBound> {
            Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
                0.01,
            ))
        }
        fn analyze(&self, model: &str, _: &VlmRequest) -> Result<VlmResponse> {
            let call = self.0.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(VlmResponse {
                model: model.into(),
                body: "{}".into(),
                prompt_tokens: 4,
                completion_tokens: 2,
                cost_usd: 0.01,
                latency_ms: 1,
                receipt: crate::analysis::provider::VlmReceipt {
                    actual_model: Some(model.into()),
                    provider: Some("openrouter".into()),
                    generation_id: Some(format!("invalid-schema-{call}")),
                    usage_complete: true,
                },
            })
        }
    }

    #[test]
    fn repeated_schema_invalid_accounted_responses_trip_cooldown_after_validation() {
        let (_directory, catalog, project) = super::super::tests::fixture();
        catalog
            .initialize_analysis_accounting_authority(&AnalysisBudgetAuthorityContext {
                project_id: project.clone(),
                policy_id: String::new(),
                policy_fingerprint: "schema-health-v1".into(),
                operation: "observation".into(),
                provider: "openrouter".into(),
                budget_scope_id: String::new(),
                budget_usd: 1.0,
            })
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let raw = InvalidSchemaProvider(Arc::clone(&calls));
        let model = crate::analysis::provider::DEFAULT_MODEL;
        let allowed = vec![model.to_string()];
        let chain =
            crate::analysis::provider::ProviderChain::with_policy(model, &allowed, false).unwrap();
        for index in 0..PROVIDER_HEALTH_FAILURE_THRESHOLD {
            let run_id = catalog
                .begin_accounted_analysis_run(&project, "", "observation")
                .unwrap();
            let context = AnalysisAccountingContext {
                project_id: project.clone(),
                policy_id: String::new(),
                policy_fingerprint: "schema-health-v1".into(),
                run_id,
                operation: "observation".into(),
                intended_attempt_key: format!("schema-{index}"),
                source_fingerprint: format!("schema-source-{index}"),
                provider: "openrouter".into(),
                budget_scope_id: String::new(),
                budget_usd: 1.0,
                explicit_retry: false,
            };
            let accounted = AccountedVlmProvider::new(&catalog, context, &raw);
            let outcome = crate::analysis::vlm::analyze_clip(
                &accounted,
                &chain,
                &crate::analysis::vlm::PromptContext {
                    clip_duration_ms: 1000,
                    ..Default::default()
                },
                Vec::new(),
            )
            .unwrap();
            assert!(outcome.observation.is_none());
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            PROVIDER_HEALTH_FAILURE_THRESHOLD as usize
        );
        let run_id = catalog
            .begin_accounted_analysis_run(&project, "", "observation")
            .unwrap();
        let context = AnalysisAccountingContext {
            project_id: project.clone(),
            policy_id: String::new(),
            policy_fingerprint: "schema-health-v1".into(),
            run_id,
            operation: "observation".into(),
            intended_attempt_key: "after-cooldown".into(),
            source_fingerprint: "after-cooldown".into(),
            provider: "openrouter".into(),
            budget_scope_id: String::new(),
            budget_usd: 1.0,
            explicit_retry: false,
        };
        let accounted = AccountedVlmProvider::new(&catalog, context, &raw);
        let error = crate::analysis::vlm::analyze_clip(
            &accounted,
            &chain,
            &crate::analysis::vlm::PromptContext {
                clip_duration_ms: 1000,
                ..Default::default()
            },
            Vec::new(),
        )
        .unwrap_err();
        assert_eq!(
            ProviderFailure::from_anyhow(&error).category,
            crate::analysis::provider::ProviderFailureCategory::ProviderCooldown
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            PROVIDER_HEALTH_FAILURE_THRESHOLD as usize
        );
    }

    #[test]
    fn cooldown_is_shared_across_reopened_catalogs_and_is_bounded() {
        let (_directory, catalog, project) = super::super::tests::fixture();
        for _ in 0..PROVIDER_HEALTH_FAILURE_THRESHOLD - 1 {
            catalog
                .record_provider_health_failure(
                    &project,
                    "policy-test",
                    "openrouter",
                    "model-test",
                    "rate_limit",
                    None,
                )
                .unwrap();
        }
        let observed_revision = catalog
            .check_provider_health(&project, "policy-test", "openrouter", "model-test")
            .unwrap();
        assert_eq!(observed_revision, 2);
        catalog
            .record_provider_health_failure(
                &project,
                "policy-test",
                "openrouter",
                "model-test",
                "rate_limit",
                Some(10_000),
            )
            .unwrap();
        assert!(cooldown_seconds(100, Some(10_000)) <= PROVIDER_HEALTH_MAX_COOLDOWN_SECS);
        assert!(
            catalog
                .check_provider_health(&project, "policy-test", "openrouter", "model-test")
                .is_err()
        );
        let reopened = Catalog::open_existing(catalog.database_path()).unwrap();
        assert!(
            reopened
                .check_provider_health(&project, "policy-test", "openrouter", "model-test")
                .is_err()
        );
        reopened
            .record_provider_health_success(
                &project,
                "policy-test",
                "openrouter",
                "model-test",
                observed_revision,
            )
            .unwrap();
        assert!(
            reopened
                .check_provider_health(&project, "policy-test", "openrouter", "model-test")
                .is_err(),
            "stale in-flight success must not erase a newer cooldown"
        );
        reopened
            .record_provider_health_success(
                &project,
                "policy-test",
                "openrouter",
                "model-test",
                observed_revision + 1,
            )
            .unwrap();
        let cleared_revision = reopened
            .check_provider_health(&project, "policy-test", "openrouter", "model-test")
            .unwrap();
        assert_eq!(cleared_revision, observed_revision + 2);
        reopened
            .record_provider_health_failure(
                &project,
                "policy-test",
                "openrouter",
                "model-test",
                "rate_limit",
                Some(60),
            )
            .unwrap();
        assert!(
            reopened
                .check_provider_health(&project, "policy-test", "openrouter", "model-test")
                .is_err(),
            "Retry-After applies before the sustained-failure threshold"
        );
        reopened
            .record_provider_health_success(
                &project,
                "policy-test",
                "openrouter",
                "model-test",
                observed_revision + 1,
            )
            .unwrap();
        assert!(
            reopened
                .check_provider_health(&project, "policy-test", "openrouter", "model-test")
                .is_err(),
            "stale success cannot clear a recreated failure row"
        );
        reopened
            .record_provider_health_success(
                &project,
                "policy-test",
                "openrouter",
                "model-test",
                cleared_revision + 1,
            )
            .unwrap();
        reopened
            .check_provider_health(&project, "policy-test", "openrouter", "model-test")
            .unwrap();
    }

    #[test]
    fn concurrent_accounted_workers_share_429_cooldown_and_release_denied_reservation() {
        let (_directory, catalog, project) = super::super::tests::fixture();
        catalog
            .initialize_analysis_accounting_authority(&AnalysisBudgetAuthorityContext {
                project_id: project.clone(),
                policy_id: String::new(),
                policy_fingerprint: "synthetic-operation-policy".into(),
                operation: "observation".into(),
                provider: "openrouter".into(),
                budget_scope_id: String::new(),
                budget_usd: 1.0,
            })
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(3));
        let dispatched_barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for index in 0..3 {
            let worker_catalog = catalog.clone();
            let worker_project = project.clone();
            let calls = Arc::clone(&calls);
            let barrier = Arc::clone(&barrier);
            let dispatched_barrier = Arc::clone(&dispatched_barrier);
            workers.push(std::thread::spawn(move || {
                let run_id = worker_catalog
                    .begin_accounted_analysis_run(&worker_project, "", "observation")
                    .unwrap();
                let context = AnalysisAccountingContext {
                    project_id: worker_project,
                    policy_id: String::new(),
                    policy_fingerprint: "synthetic-operation-policy".into(),
                    run_id,
                    operation: "observation".into(),
                    intended_attempt_key: format!("attempt-{index}"),
                    source_fingerprint: format!("source-{index}"),
                    provider: "openrouter".into(),
                    budget_scope_id: String::new(),
                    budget_usd: 1.0,
                    explicit_retry: false,
                };
                let provider = RateLimitedProvider(calls, dispatched_barrier);
                let accounted = AccountedVlmProvider::new(&worker_catalog, context, &provider);
                barrier.wait();
                accounted
                    .analyze("qwen/qwen3.7-flash", &VlmRequest::default())
                    .unwrap_err()
            }));
        }
        for worker in workers {
            let error = worker.join().unwrap();
            assert_eq!(
                ProviderFailure::from_anyhow(&error).category,
                crate::analysis::provider::ProviderFailureCategory::HttpStatus,
                "worker stopped before its mock provider transmission: {error:#}"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        let before = catalog
            .list_analysis_requests(&project, None, 20)
            .unwrap()
            .len();
        let context = AnalysisAccountingContext {
            project_id: project.clone(),
            policy_id: String::new(),
            policy_fingerprint: "synthetic-operation-policy".into(),
            run_id: catalog
                .begin_accounted_analysis_run(&project, "", "observation")
                .unwrap(),
            operation: "observation".into(),
            intended_attempt_key: "attempt-resumed".into(),
            source_fingerprint: "source-resumed".into(),
            provider: "openrouter".into(),
            budget_scope_id: String::new(),
            budget_usd: 1.0,
            explicit_retry: false,
        };
        let resumed = RateLimitedProvider(Arc::clone(&calls), Arc::new(Barrier::new(1)));
        let reopened = Catalog::open_existing(catalog.database_path()).unwrap();
        let accounted = AccountedVlmProvider::new(&reopened, context, &resumed);
        let error = accounted
            .analyze("qwen/qwen3.7-flash", &VlmRequest::default())
            .unwrap_err();
        assert_eq!(
            ProviderFailure::from_anyhow(&error).category,
            crate::analysis::provider::ProviderFailureCategory::ProviderCooldown
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "cooldown denial must precede paid provider I/O"
        );
        let requests = catalog.list_analysis_requests(&project, None, 20).unwrap();
        assert_eq!(requests.len(), before + 1);
        let denied = requests
            .iter()
            .find(|request| request.failure_code == "provider_cooldown")
            .unwrap_or_else(|| {
                panic!("cooldown reservation was not recorded as denied: {requests:#?}")
            });
        assert_eq!(denied.lifecycle, crate::AnalysisRequestLifecycle::Released);
        assert_eq!(denied.failure_code, "provider_cooldown");
        assert!(cooldown_seconds(PROVIDER_HEALTH_FAILURE_THRESHOLD, Some(60)) >= 60);
        assert!(cooldown_seconds(100, Some(10_000)) <= PROVIDER_HEALTH_MAX_COOLDOWN_SECS);
    }
}
