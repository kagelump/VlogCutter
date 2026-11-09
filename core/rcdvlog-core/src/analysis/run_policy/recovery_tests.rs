use super::tests::{
    CapturingProvider, ModelScriptProvider, scripted_response, valid_observation_body,
};
use super::*;
use crate::ProviderReceiptEvidence;
use crate::analysis::provider::{
    AnalysisRequestCostBound, ProviderFailure, ProviderFailureCategory, VlmProvider, VlmReceipt,
    VlmRequest, VlmResponse,
};
use crate::{AnalysisRequestLifecycle, AnalysisRequestReconciliation};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

struct InterruptingProvider {
    calls: AtomicUsize,
}

impl VlmProvider for InterruptingProvider {
    fn analysis_cost_bound(
        &self,
        _model: &str,
        _request: &VlmRequest,
    ) -> Result<AnalysisRequestCostBound> {
        Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
            0.03,
        ))
    }

    fn analyze(&self, _model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        panic!("synthetic process interruption after durable dispatch marker")
    }
}

#[test]
fn recovered_crashed_running_pilot_keeps_unknown_validation_and_known_usage() {
    let (_directory, catalog, project, asset) = super::tests::fixture();
    let request = RunPolicyRequest::observation(
        project.clone(),
        vec![asset.clone()],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into()],
        false,
        1.0,
        false,
    );
    let authorization = catalog.authorize_run_policy(&request).unwrap();
    let provider = InterruptingProvider {
        calls: AtomicUsize::new(0),
    };
    let interrupted = catch_unwind(AssertUnwindSafe(|| {
        catalog.run_run_policy_pilot_with_provider(&request, &provider, false)
    }));
    assert!(interrupted.is_err());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let policy_id = authorization.policy.policy_id;
    let running = catalog.load_run_pilot(&policy_id).unwrap().unwrap();
    assert_eq!(running.status, "running");
    let request_record = catalog
        .list_analysis_requests(&project, Some(&policy_id), 10)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(
        request_record.lifecycle,
        AnalysisRequestLifecycle::Dispatched
    );

    let report = catalog
        .reconcile_analysis_request_for_project(
            &project,
            &request_record.request_key,
            &AnalysisRequestReconciliation::LateReceipt {
                receipt: ProviderReceiptEvidence {
                    provider: "synthetic-script".into(),
                    provider_generation_id: "recovered-generation".into(),
                    actual_model: DEFAULT_MODEL.into(),
                    reported_cost_usd: Some(0.03),
                    prompt_tokens: Some(10),
                    completion_tokens: Some(5),
                    usage_complete: true,
                    billed_audio_seconds: None,
                    usage_pricing: None,
                },
            },
        )
        .unwrap();
    assert_eq!(
        report.request.lifecycle,
        AnalysisRequestLifecycle::Completed
    );
    let pilot = report.pilot.expect("existing pilot projection refreshed");
    assert_eq!(pilot.status, "failed");
    assert!(pilot.usage_complete);
    assert!(!pilot.validation.capability_observed);
    assert!(!pilot.validation.structural_schema_valid);
    assert!(!pilot.validation.source_time_semantics_valid);
    assert!(pilot.observed_capabilities.is_empty());
    assert!((pilot.confirmed_cost_usd - 0.03).abs() < 1e-12);
    assert_eq!(pilot.failure_code, "reconciled_receipt_requires_review");
    let run = catalog
        .analysis_runs(&project, 10)
        .unwrap()
        .into_iter()
        .find(|run| run.id == pilot.run_id)
        .unwrap();
    assert_eq!(run.status, "failed");
    assert!((run.cost_usd - 0.03).abs() < 1e-12);
    // Recovery read the prior pilot, but a later runtime run now owns its
    // projection. A stale write cannot replace that newer run/metadata.
    let mut newer = pilot.clone();
    newer.run_id = catalog
        .begin_accounted_analysis_run(&project, &policy_id, "pilot")
        .unwrap();
    newer.status = "running".into();
    newer.finished_at.clear();
    newer.provider = None;
    catalog.save_pilot(&newer).unwrap();
    assert!(!catalog.save_pilot_if_unchanged(&pilot, &pilot).unwrap());
    assert_eq!(catalog.load_run_pilot(&policy_id).unwrap(), Some(newer));
}
struct CaptureFailProvider {
    calls: AtomicUsize,
    payload: Mutex<Option<VlmRequest>>,
}

impl VlmProvider for CaptureFailProvider {
    fn analysis_cost_bound(
        &self,
        _model: &str,
        _request: &VlmRequest,
    ) -> Result<AnalysisRequestCostBound> {
        Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
            0.03,
        ))
    }

    fn analyze(&self, _model: &str, request: &VlmRequest) -> Result<VlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.payload.lock().unwrap() = Some(request.clone());
        Err(ProviderFailure::transport().into())
    }
}

struct PolicyRetryProvider {
    calls: AtomicUsize,
    body: String,
}

impl VlmProvider for PolicyRetryProvider {
    fn analysis_cost_bound(
        &self,
        _model: &str,
        _request: &VlmRequest,
    ) -> Result<AnalysisRequestCostBound> {
        Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
            0.03,
        ))
    }

    fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(VlmResponse {
            model: model.into(),
            body: self.body.clone(),
            prompt_tokens: 11,
            completion_tokens: 7,
            cost_usd: 0.03,
            latency_ms: 2,
            receipt: VlmReceipt {
                actual_model: Some(model.into()),
                provider: Some("synthetic-policy-retry".into()),
                generation_id: Some("policy-retry-generation".into()),
                usage_complete: true,
            },
        })
    }
}

#[test]
fn policy_owned_targeted_retry_rederives_source_policy_and_payload_then_dispatches_once() {
    let (_directory, catalog, project, asset) = super::tests::fixture();
    let request = RunPolicyRequest::observation(
        project.clone(),
        vec![asset.clone()],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into()],
        false,
        1.0,
        false,
    );
    let _authorization = catalog.authorize_run_policy(&request).unwrap();
    let failed = CaptureFailProvider {
        calls: AtomicUsize::new(0),
        payload: Mutex::new(None),
    };
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &failed, false)
        .unwrap();
    assert_eq!(pilot.status, "uncertain");
    let prior = catalog
        .list_analysis_requests(&project, Some(&pilot.policy_id), 10)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let original_payload = failed.payload.lock().unwrap().clone().unwrap();
    catalog
        .authorize_analysis_request_retry_for_project(&project, &prior.request_key)
        .unwrap();

    let retry = PolicyRetryProvider {
        calls: AtomicUsize::new(0),
        body: "REVIEW_ONLY_POLICY_RETRY_BODY".into(),
    };
    let report = catalog
        .dispatch_targeted_analysis_retry_with_provider(
            &project,
            &prior.request_key,
            1,
            &original_payload,
            &retry,
        )
        .unwrap();
    assert_eq!(retry.calls.load(Ordering::SeqCst), 1);
    assert_eq!(report.request.attempt_epoch, 1);
    assert_eq!(
        report.request.lifecycle,
        AnalysisRequestLifecycle::Completed
    );
    assert_eq!(
        report.provider_response.as_ref().unwrap().body,
        "REVIEW_ONLY_POLICY_RETRY_BODY"
    );
    assert!((report.budget.confirmed_cost_usd - 0.03).abs() < 1e-12);
    assert!((report.budget.uncertain_usage_reserve_usd - 0.03).abs() < 1e-12);
}

#[test]
fn unresolved_bulk_usage_blocks_replacement_policy_on_exact_source() {
    let (_directory, catalog, project, asset) = super::tests::fixture();
    let prior = RunPolicyRequest::observation(
        project.clone(),
        vec![asset.clone()],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into()],
        false,
        1.0,
        false,
    );
    let old_policy = catalog.authorize_run_policy(&prior).unwrap();
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&prior, &CapturingProvider::observation(), false)
        .unwrap();
    assert_eq!(pilot.status, "passed");
    let failed = CaptureFailProvider {
        calls: AtomicUsize::new(0),
        payload: Mutex::new(None),
    };
    let report = catalog
        .analyze_authorized_with_provider(&old_policy.policy.policy_id, &failed, |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(failed.calls.load(Ordering::SeqCst), 1);
    assert_eq!(report.analyzed, 1);
    assert!(
        catalog
            .list_analysis_requests(&project, Some(&old_policy.policy.policy_id), 20)
            .unwrap()
            .iter()
            .any(|request| request.operation == "observation"
                && request.lifecycle == AnalysisRequestLifecycle::Uncertain)
    );

    let replacement = RunPolicyRequest::observation(
        project.clone(),
        vec![asset],
        FALLBACK_MODEL,
        vec![FALLBACK_MODEL.into()],
        false,
        1.0,
        false,
    );
    let new_policy = catalog.authorize_run_policy(&replacement).unwrap();
    let provider = CapturingProvider::observation();
    let error = catalog
        .run_run_policy_pilot_with_provider(&replacement, &provider, false)
        .unwrap_err();
    assert!(format!("{error:#}").contains("prior bulk analysis request has unresolved usage"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_ne!(new_policy.policy.policy_id, old_policy.policy.policy_id);
}

#[test]
fn exact_failed_asset_recovers_under_new_allowlisted_policy_and_reuses_saved_result() {
    let (directory, catalog, project, asset) = super::tests::fixture();
    let successful_asset = super::tests::generated_asset(
        &catalog,
        &project,
        directory.path(),
        "clip-recovery-successful",
        2,
    );
    let old_request = RunPolicyRequest::observation(
        project.clone(),
        vec![asset.clone(), successful_asset.clone()],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into()],
        false,
        1.0,
        false,
    );
    let old_policy = catalog.authorize_run_policy(&old_request).unwrap();
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&old_request, &CapturingProvider::observation(), false)
        .unwrap();
    assert_eq!(pilot.status, "passed");

    let rejected = ModelScriptProvider::new(0.01, |model, call| {
        Ok(scripted_response(
            model,
            call,
            if call == 1 {
                valid_observation_body()
            } else {
                "not valid observation JSON".into()
            },
            0.01,
        ))
    });
    let failed = catalog
        .analyze_authorized_with_provider(&old_policy.policy.policy_id, &rejected, |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(failed.rejected, 1);
    assert_eq!(failed.analyzed, 2);
    assert_eq!(rejected.calls.load(Ordering::SeqCst), 2);
    let (successful_asset_id, failed_asset_id) = {
        let first = catalog.get_asset(&asset).unwrap();
        let second = catalog.get_asset(&successful_asset).unwrap();
        if first.title.is_empty() {
            (successful_asset.clone(), asset.clone())
        } else if second.title.is_empty() {
            (asset.clone(), successful_asset.clone())
        } else {
            panic!("synthetic run must have one saved result and one rejected result")
        }
    };
    let successful_title = catalog.get_asset(&successful_asset_id).unwrap().title;
    let same_policy = ModelScriptProvider::new(0.01, |model, call| {
        Ok(scripted_response(
            model,
            call,
            valid_observation_body(),
            0.01,
        ))
    });
    let protected = catalog
        .analyze_authorized_with_provider(&old_policy.policy.policy_id, &same_policy, |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(protected.skipped, 2);
    assert_eq!(
        same_policy.calls.load(Ordering::SeqCst),
        0,
        "ordinary same-policy reruns must not resend a paid rejection"
    );

    // The replacement authorization explicitly scopes only the exact failed
    // asset and permits only the healthy alternate model. The old paid
    // response remains in the old ledger and is never resent.
    let replacement = RunPolicyRequest::observation(
        project.clone(),
        vec![failed_asset_id.clone()],
        FALLBACK_MODEL,
        vec![FALLBACK_MODEL.into()],
        false,
        1.0,
        false,
    );
    let new_policy = catalog.authorize_run_policy(&replacement).unwrap();
    assert_ne!(new_policy.policy.policy_id, old_policy.policy.policy_id);
    assert_eq!(new_policy.policy.scope.len(), 1);
    assert_eq!(new_policy.policy.scope[0].asset_id, failed_asset_id);
    assert!(
        !new_policy
            .policy
            .scope
            .iter()
            .any(|scope| scope.asset_id == successful_asset_id)
    );
    let new_pilot = catalog
        .run_run_policy_pilot_with_provider(&replacement, &CapturingProvider::observation(), false)
        .unwrap();
    assert_eq!(new_pilot.status, "passed");
    let healthy = ModelScriptProvider::new(0.01, |model, call| {
        Ok(scripted_response(
            model,
            call,
            valid_observation_body(),
            0.01,
        ))
    });
    let recovered = catalog
        .analyze_authorized_with_provider(&new_policy.policy.policy_id, &healthy, |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(recovered.analyzed, 1);
    assert_eq!(healthy.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        catalog.get_asset(&successful_asset_id).unwrap().title,
        successful_title
    );
    let repeated = ModelScriptProvider::new(0.01, |model, call| {
        Ok(scripted_response(
            model,
            call,
            valid_observation_body(),
            0.01,
        ))
    });
    let reused = catalog
        .analyze_authorized_with_provider(&new_policy.policy.policy_id, &repeated, |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(reused.skipped, 1);
    assert_eq!(repeated.calls.load(Ordering::SeqCst), 0);
    let attempts = catalog
        .list_analysis_requests(&project, Some(&new_policy.policy.policy_id), 20)
        .unwrap();
    assert!(
        attempts
            .iter()
            .all(|attempt| attempt.actual_model.as_deref() == Some(FALLBACK_MODEL))
    );
}

#[test]
fn same_policy_fallback_rejection_is_not_replayed() {
    let (_directory, catalog, project, asset) = super::tests::fixture();
    let request = RunPolicyRequest::observation(
        project.clone(),
        vec![asset],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into(), FALLBACK_MODEL.into()],
        true,
        1.0,
        false,
    );
    let authorization = catalog.authorize_run_policy(&request).unwrap();
    catalog
        .run_run_policy_pilot_with_provider(&request, &CapturingProvider::observation(), false)
        .unwrap();
    let provider = ModelScriptProvider::new(0.01, |model, call| {
        Ok(scripted_response(
            model,
            call,
            "not valid observation JSON".into(),
            0.01,
        ))
    });
    let failed = catalog
        .analyze_authorized_with_provider(&authorization.policy.policy_id, &provider, |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(failed.rejected, 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    let asset_id = authorization.policy.scope[0].asset_id.clone();
    let saved =
        crate::analysis::vlm_store::load_analysis(&catalog.connection().unwrap(), &asset_id)
            .unwrap()
            .unwrap();
    assert_eq!(
        saved.status,
        crate::analysis::vlm_store::AnalysisStatus::Rejected
    );
    assert_eq!(saved.model, FALLBACK_MODEL);

    let repeat = ModelScriptProvider::new(0.01, |model, call| {
        Ok(scripted_response(
            model,
            call,
            valid_observation_body(),
            0.01,
        ))
    });
    let report = catalog
        .analyze_authorized_with_provider(&authorization.policy.policy_id, &repeat, |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(report.skipped, 1);
    assert_eq!(repeat.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn policy_source_or_project_mismatch_denies_targeted_retry_before_any_call() {
    let (_directory, catalog, project, asset) = super::tests::fixture();
    let request = RunPolicyRequest::observation(
        project.clone(),
        vec![asset.clone()],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into()],
        false,
        1.0,
        false,
    );
    catalog.authorize_run_policy(&request).unwrap();
    let failed = CaptureFailProvider {
        calls: AtomicUsize::new(0),
        payload: Mutex::new(None),
    };
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &failed, false)
        .unwrap();
    let prior = catalog
        .list_analysis_requests(&project, Some(&pilot.policy_id), 10)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let original_payload = failed.payload.lock().unwrap().clone().unwrap();
    catalog
        .authorize_analysis_request_retry_for_project(&project, &prior.request_key)
        .unwrap();

    let source = catalog.get_asset(&asset).unwrap();
    let proxy = source.proxy_path.expect("synthetic fixture has a proxy");
    std::fs::write(proxy, b"changed proxy bytes after authorization").unwrap();
    let provider = PolicyRetryProvider {
        calls: AtomicUsize::new(0),
        body: "must not be sent".into(),
    };
    let source_error = catalog
        .dispatch_targeted_analysis_retry_with_provider(
            &project,
            &prior.request_key,
            1,
            &original_payload,
            &provider,
        )
        .unwrap_err();
    assert!(
        format!("{source_error:#}")
            .to_lowercase()
            .contains("policy")
            || format!("{source_error:#}")
                .to_lowercase()
                .contains("source")
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);

    let wrong_project_error = catalog
        .dispatch_targeted_analysis_retry_with_provider(
            "not-the-owning-project",
            &prior.request_key,
            1,
            &original_payload,
            &provider,
        )
        .unwrap_err();
    assert!(format!("{wrong_project_error:#}").contains("does not belong"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);

    // A revoked canonical authorization is denied by the same shared
    // owner check even when the caller reaches the provider-injected direct
    // entry point. No provider method has been called.
    catalog
        .connection()
        .unwrap()
        .execute(
            "DELETE FROM analysis_run_authorizations WHERE policy_id=?1",
            [&prior.policy_id],
        )
        .unwrap();
    let revoked_provider = PolicyRetryProvider {
        calls: AtomicUsize::new(0),
        body: "must not be sent".into(),
    };
    let revoked_error = catalog
        .dispatch_targeted_analysis_retry_with_provider(
            &project,
            &prior.request_key,
            1,
            &original_payload,
            &revoked_provider,
        )
        .unwrap_err();
    assert!(
        format!("{revoked_error:#}").contains("not authorized"),
        "{revoked_error:#}"
    );
    assert_eq!(revoked_provider.calls.load(Ordering::SeqCst), 0);
}
struct PrepareMutatesProxyProvider {
    calls: AtomicUsize,
    proxy_path: std::path::PathBuf,
}

impl VlmProvider for PrepareMutatesProxyProvider {
    fn prepare(&self) -> Result<()> {
        std::fs::write(&self.proxy_path, b"changed after policy-owning prepare")
            .map_err(|_| anyhow::anyhow!("synthetic prepare mutation failed"))?;
        Ok(())
    }

    fn analysis_cost_bound(
        &self,
        _model: &str,
        _request: &VlmRequest,
    ) -> Result<AnalysisRequestCostBound> {
        Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
            0.03,
        ))
    }

    fn analyze(&self, _model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        panic!("mutated policy must be denied before transport")
    }
}

struct PrepareRevokesAuthorizationProvider {
    calls: AtomicUsize,
    catalog: Catalog,
    policy_id: String,
}

impl VlmProvider for PrepareRevokesAuthorizationProvider {
    fn prepare(&self) -> Result<()> {
        self.catalog
            .connection()
            .map_err(|_| anyhow::anyhow!("synthetic authorization mutation failed"))?
            .execute(
                "DELETE FROM analysis_run_authorizations WHERE policy_id=?1",
                [&self.policy_id],
            )
            .map_err(|_| anyhow::anyhow!("synthetic authorization mutation failed"))?;
        Ok(())
    }

    fn analysis_cost_bound(
        &self,
        _model: &str,
        _request: &VlmRequest,
    ) -> Result<AnalysisRequestCostBound> {
        Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
            0.03,
        ))
    }

    fn analyze(&self, _model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        panic!("revoked authorization must be denied before transport")
    }
}

struct PrepareCancelsProvider {
    calls: AtomicUsize,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

impl VlmProvider for PrepareCancelsProvider {
    fn prepare(&self) -> Result<()> {
        self.cancelled.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn analysis_cost_bound(
        &self,
        _model: &str,
        _request: &VlmRequest,
    ) -> Result<AnalysisRequestCostBound> {
        Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
            0.03,
        ))
    }

    fn analyze(&self, _model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        panic!("pre-marker cancellation must not reach transport")
    }
}

#[test]
fn policy_retry_prepare_mutation_is_rechecked_before_marker_and_released() {
    let (_directory, catalog, project, asset) = super::tests::fixture();
    let request = RunPolicyRequest::observation(
        project.clone(),
        vec![asset.clone()],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into()],
        false,
        1.0,
        false,
    );
    catalog.authorize_run_policy(&request).unwrap();
    let failed = CaptureFailProvider {
        calls: AtomicUsize::new(0),
        payload: Mutex::new(None),
    };
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &failed, false)
        .unwrap();
    assert_eq!(pilot.status, "uncertain");
    let prior = catalog
        .list_analysis_requests(&project, Some(&pilot.policy_id), 10)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let original_payload = failed.payload.lock().unwrap().clone().unwrap();
    catalog
        .authorize_analysis_request_retry_for_project(&project, &prior.request_key)
        .unwrap();
    let proxy_path = catalog
        .get_asset(&asset)
        .unwrap()
        .proxy_path
        .expect("synthetic fixture has a proxy");
    let provider = PrepareMutatesProxyProvider {
        calls: AtomicUsize::new(0),
        proxy_path: proxy_path.into(),
    };
    let report = catalog
        .dispatch_targeted_analysis_retry_with_provider(
            &project,
            &prior.request_key,
            1,
            &original_payload,
            &provider,
        )
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(report.request.lifecycle, AnalysisRequestLifecycle::Released);
    assert!(report.request.dispatch_at.is_none());
    let run = catalog
        .analysis_runs(&project, 100)
        .unwrap()
        .into_iter()
        .find(|run| run.id == report.request.run_id)
        .unwrap();
    assert_eq!(run.status, "failed");
    assert_eq!(run.cost_usd, 0.0);
    assert_eq!(report.attempt_history.len(), 2);
    assert_eq!(
        report.attempt_history[0].lifecycle,
        AnalysisRequestLifecycle::Uncertain
    );
    assert_eq!(
        report.attempt_history[1].lifecycle,
        AnalysisRequestLifecycle::Released
    );
    assert!((report.budget.uncertain_usage_reserve_usd - 0.03).abs() < 1e-12);
    assert_eq!(report.budget.active_reserved_cost_usd, 0.0);
    assert!(!report.budget.dispatch_stopped);
    assert!(
        report
            .provider_error_code
            .as_deref()
            .unwrap_or_default()
            .contains("pre_upload")
            || report
                .provider_error_code
                .as_deref()
                .unwrap_or_default()
                .contains("admission")
    );
}

#[test]
fn policy_retry_prepare_authorization_revocation_is_rechecked_before_marker() {
    let (_directory, catalog, project, asset) = super::tests::fixture();
    let request = RunPolicyRequest::observation(
        project.clone(),
        vec![asset],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into()],
        false,
        1.0,
        false,
    );
    catalog.authorize_run_policy(&request).unwrap();
    let failed = CaptureFailProvider {
        calls: AtomicUsize::new(0),
        payload: Mutex::new(None),
    };
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &failed, false)
        .unwrap();
    let prior = catalog
        .list_analysis_requests(&project, Some(&pilot.policy_id), 10)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let original_payload = failed.payload.lock().unwrap().clone().unwrap();
    catalog
        .authorize_analysis_request_retry_for_project(&project, &prior.request_key)
        .unwrap();
    let provider = PrepareRevokesAuthorizationProvider {
        calls: AtomicUsize::new(0),
        catalog: catalog.clone(),
        policy_id: pilot.policy_id.clone(),
    };
    let report = catalog
        .dispatch_targeted_analysis_retry_with_provider(
            &project,
            &prior.request_key,
            1,
            &original_payload,
            &provider,
        )
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(report.request.lifecycle, AnalysisRequestLifecycle::Released);
    assert!(report.request.dispatch_at.is_none());
    assert_eq!(
        report.attempt_history.last().unwrap().lifecycle,
        AnalysisRequestLifecycle::Released
    );
    assert!((report.budget.uncertain_usage_reserve_usd - 0.03).abs() < 1e-12);
    assert_eq!(report.budget.active_reserved_cost_usd, 0.0);
    assert!(!report.budget.dispatch_stopped);
}

#[test]
fn targeted_retry_cancellation_before_prepare_and_after_prepare_make_zero_calls() {
    // Cancellation before local preparation creates no paid run or request.
    let (_directory, catalog, project, asset) = super::tests::fixture();
    let request = RunPolicyRequest::observation(
        project.clone(),
        vec![asset.clone()],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into()],
        false,
        1.0,
        false,
    );
    catalog.authorize_run_policy(&request).unwrap();
    let failed = CaptureFailProvider {
        calls: AtomicUsize::new(0),
        payload: Mutex::new(None),
    };
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &failed, false)
        .unwrap();
    let prior = catalog
        .list_analysis_requests(&project, Some(&pilot.policy_id), 10)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let original_payload = failed.payload.lock().unwrap().clone().unwrap();
    catalog
        .authorize_analysis_request_retry_for_project(&project, &prior.request_key)
        .unwrap();
    let runs_before = catalog.analysis_runs(&project, 100).unwrap().len();
    let never_called = PolicyRetryProvider {
        calls: AtomicUsize::new(0),
        body: "unused".into(),
    };
    let error = catalog
        .dispatch_targeted_analysis_retry_with_provider_and_cancel(
            &project,
            &prior.request_key,
            1,
            &original_payload,
            &never_called,
            || true,
        )
        .unwrap_err();
    assert_eq!(
        ProviderFailure::from_anyhow(&error).category,
        ProviderFailureCategory::Cancelled
    );
    assert_eq!(never_called.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        catalog.analysis_runs(&project, 100).unwrap().len(),
        runs_before
    );
    assert_eq!(
        catalog
            .analysis_attempt_history(&prior.request_key)
            .unwrap()
            .len(),
        1
    );

    // Cancellation raised by provider.prepare happens after the reservation
    // but before the durable marker; the reservation is releasable and the
    // prior unknown remains intact.
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let provider = PrepareCancelsProvider {
        calls: AtomicUsize::new(0),
        cancelled: Arc::clone(&cancelled),
    };
    let report = catalog
        .dispatch_targeted_analysis_retry_with_provider_and_cancel(
            &project,
            &prior.request_key,
            1,
            &original_payload,
            &provider,
            move || cancelled.load(Ordering::SeqCst),
        )
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(report.request.lifecycle, AnalysisRequestLifecycle::Released);
    assert!(report.request.dispatch_at.is_none());
    let run = catalog
        .analysis_runs(&project, 100)
        .unwrap()
        .into_iter()
        .find(|run| run.id == report.request.run_id)
        .unwrap();
    assert_eq!(run.status, "cancelled");
    assert_eq!(run.cost_usd, 0.0);
    assert_eq!(report.attempt_history.len(), 2);
    assert_eq!(
        report.attempt_history.last().unwrap().lifecycle,
        AnalysisRequestLifecycle::Released
    );
    assert!((report.budget.uncertain_usage_reserve_usd - 0.03).abs() < 1e-12);
    assert_eq!(report.budget.active_reserved_cost_usd, 0.0);
    assert!(!report.budget.dispatch_stopped);
    assert!(
        report
            .provider_error_code
            .as_deref()
            .unwrap_or_default()
            .contains("cancelled")
    );
}

#[test]
fn late_old_pilot_receipt_refreshes_whole_pilot_but_keeps_current_run_metadata() {
    let (_directory, catalog, project, asset) = super::tests::fixture();
    let request = RunPolicyRequest::observation(
        project.clone(),
        vec![asset.clone()],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into()],
        false,
        1.0,
        false,
    );
    catalog.authorize_run_policy(&request).unwrap();
    let failed = CaptureFailProvider {
        calls: AtomicUsize::new(0),
        payload: Mutex::new(None),
    };
    let first_pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &failed, false)
        .unwrap();
    assert_eq!(first_pilot.status, "uncertain");
    let old_request = catalog
        .list_analysis_requests(&project, Some(&first_pilot.policy_id), 10)
        .unwrap()
        .into_iter()
        .find(|record| record.run_id == first_pilot.run_id)
        .unwrap();
    catalog
        .authorize_run_policy_pilot_retry(&first_pilot.policy_id, &old_request.request_key)
        .unwrap();
    let second_provider = PolicyRetryProvider {
        calls: AtomicUsize::new(0),
        body: r#"{"title":"Synthetic late receipt pilot","description":"valid synthetic observation","labels":[],"usable":true,"unusableReason":null,"bestMomentStartMs":null,"bestMomentEndMs":null,"thumbnailTimestampMs":null,"cameraMovement":"static","ocrText":[]}"#.into(),
    };
    let second_pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &second_provider, true)
        .unwrap();
    assert_eq!(second_provider.calls.load(Ordering::SeqCst), 1);
    assert!(!second_pilot.usage_complete);
    let current_metadata = (
        second_pilot.provider.clone(),
        second_pilot.actual_model.clone(),
        second_pilot.generation_id.clone(),
    );
    assert!(current_metadata.0.is_some());
    assert!(current_metadata.1.is_some());
    assert!(current_metadata.2.is_some());
    let first_receipt = ProviderReceiptEvidence {
        provider: "synthetic-old-late".into(),
        provider_generation_id: "old-late-generation".into(),
        actual_model: DEFAULT_MODEL.into(),
        reported_cost_usd: Some(0.03),
        prompt_tokens: Some(19),
        completion_tokens: Some(7),
        usage_complete: true,
        billed_audio_seconds: None,
        usage_pricing: None,
    };
    let evidence = AnalysisRequestReconciliation::LateReceipt {
        receipt: first_receipt,
    };
    let first_report = catalog
        .reconcile_analysis_request_for_project(&project, &old_request.request_key, &evidence)
        .unwrap();
    let first_projection = first_report.pilot.expect("pilot projection refreshed");
    assert!((first_projection.confirmed_cost_usd - 0.06).abs() < 1e-12);
    assert_eq!(first_projection.uncertain_usage_reserve_usd, 0.0);
    assert_eq!(first_projection.status, "passed");
    assert_eq!(
        (
            first_projection.provider.clone(),
            first_projection.actual_model.clone(),
            first_projection.generation_id.clone(),
        ),
        current_metadata,
        "a late old-run receipt must not replace current-run provenance"
    );
    let current_run = first_projection.run_id.clone();
    let (run_cost, run_status): (f64, String) = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT cost_usd,status FROM analysis_runs WHERE id=?1",
            [&current_run],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(
        (run_cost - 0.03).abs() < 1e-12,
        "historical run cost is its own confirmed projection, not whole-pilot cost"
    );
    assert_eq!(
        run_status, "completed",
        "a persisted pilot validation plus fully known usage closes the owned run as completed"
    );
    let finished_at = first_projection.finished_at.clone();
    let history_after_first = catalog
        .analysis_request_events(&old_request.request_key)
        .unwrap();

    let second_report = catalog
        .reconcile_analysis_request_for_project(&project, &old_request.request_key, &evidence)
        .unwrap();
    let second_projection = second_report.pilot.expect("pilot projection retained");
    assert_eq!(first_projection, second_projection);
    assert_eq!(second_projection.finished_at, finished_at);
    assert_eq!(
        catalog
            .analysis_request_events(&old_request.request_key)
            .unwrap(),
        history_after_first,
        "identical reconciliation must not append metadata churn"
    );
    assert_eq!(
        catalog
            .analysis_provider_receipts(&old_request.request_key)
            .unwrap()
            .len(),
        1
    );
}
