//! The accounted native-video leaf is tested with a fake transport only.
use super::*;
use crate::analysis::accounting::native_video::AccountedNativeVideoProvider;
use crate::analysis::accounting::{AnalysisAccountingContext, AnalysisRequestLifecycle};
use crate::analysis::native_video::{
    cache::{Claim, WindowOwner},
    protocol::{self, CallResult, PreparedRequest},
    transport::Transport,
};
use crate::{AnalysisPayload, Catalog, NativeVideoOptions, RunPolicy, RunPolicyParameters};
use anyhow::{Result, anyhow};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct FakeTransport<'a> {
    catalog: &'a Catalog,
    policy: &'a RunPolicy,
    prepares: AtomicUsize,
    transmits: AtomicUsize,
    response: Vec<u8>,
    fail_after_dispatch: bool,
    before_prepare: Option<Box<dyn Fn() + Send + Sync>>,
}

impl FakeTransport<'_> {
    fn accepted() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "responseId":"fake-generation",
            "modelVersion":NATIVE_VIDEO_MODEL,
            "usageMetadata":{"serviceTier":"standard","promptTokenCount":100,"cachedContentTokenCount":0,"candidatesTokenCount":20,"thoughtsTokenCount":0,"toolUsePromptTokenCount":0,"totalTokenCount":120},
            "candidates":[{"finishReason":"STOP","content":{"parts":[{"text":"{\"version\":1,\"clock\":\"localVideoZero\",\"moments\":[],\"flags\":[]}"}]}}]
        })).unwrap()
    }
    fn wrong_tier() -> Vec<u8> {
        let mut value: serde_json::Value = serde_json::from_slice(&Self::accepted()).unwrap();
        value["usageMetadata"]["serviceTier"] = json!("flex");
        serde_json::to_vec(&value).unwrap()
    }
    fn conserved_missing_auxiliary() -> Vec<u8> {
        let mut value: serde_json::Value = serde_json::from_slice(&Self::accepted()).unwrap();
        value["usageMetadata"]
            .as_object_mut()
            .unwrap()
            .remove("cachedContentTokenCount");
        value["usageMetadata"]
            .as_object_mut()
            .unwrap()
            .remove("thoughtsTokenCount");
        value["usageMetadata"]
            .as_object_mut()
            .unwrap()
            .remove("toolUsePromptTokenCount");
        serde_json::to_vec(&value).unwrap()
    }
}

impl Transport for FakeTransport<'_> {
    fn prepare(&self) -> Result<()> {
        self.prepares.fetch_add(1, Ordering::SeqCst);
        if let Some(callback) = &self.before_prepare {
            callback();
        }
        Ok(())
    }
    fn transmit(
        &self,
        _request: &PreparedRequest,
        options: &NativeVideoOptions,
        _cancelled: &dyn Fn() -> bool,
    ) -> Result<CallResult> {
        self.transmits.fetch_add(1, Ordering::SeqCst);
        let request = self
            .catalog
            .list_analysis_requests(&self.policy.project_id, Some(&self.policy.policy_id), 100)?
            .into_iter()
            .find(|r| {
                r.run_id == self.policy.policy_id
                    || r.lifecycle == AnalysisRequestLifecycle::Dispatched
            })
            .expect("canonical request row exists before transport");
        assert_eq!(
            request.lifecycle,
            AnalysisRequestLifecycle::Dispatched,
            "transport must only run after the durable dispatch marker"
        );
        if self.fail_after_dispatch {
            return Err(anyhow!("fake connection loss after dispatch"));
        }
        protocol::decode_response(200, self.response.clone(), None, 10, options)
    }
}

struct Harness {
    _dir: tempfile::TempDir,
    catalog: Catalog,
    policy: RunPolicy,
    run_id: String,
    context: AnalysisAccountingContext,
    media: NativeVideoMedia,
    wire: PreparedRequest,
    owner: Option<Box<WindowOwner>>,
}

fn harness() -> Harness {
    let (dir, catalog, described, _) = super::media::media_tests::setup(AnalysisPayload::Video);
    let policy = catalog
        .authorize_run_policy(&described.request())
        .unwrap()
        .policy;
    catalog
        .initialize_analysis_accounting_authority(
            &crate::analysis::run_policy::pilot_authority_context(&policy),
        )
        .unwrap();
    let run_id = catalog
        .begin_accounted_analysis_run(&policy.project_id, &policy.policy_id, "pilot")
        .unwrap();
    let mut context = crate::analysis::run_policy::pilot_accounting_context_for_scope(
        &policy,
        &run_id,
        &policy.scope[0],
        false,
    )
    .unwrap();
    let media = catalog
        .prepare_native_video_window(
            &policy,
            &super::media::media_tests::window(&policy, 1.0),
            &|| false,
        )
        .unwrap();
    context.intended_attempt_key = format!(
        "native_video_review:{}:{}",
        media.view().asset_id,
        media.view().identity
    );
    let options = match &policy.parameters {
        RunPolicyParameters::NativeVideoReview { options } => options,
        _ => unreachable!(),
    };
    let wire = protocol::prepare(options, &media).unwrap();
    let owner = match catalog
        .claim_native_video_window(&policy, &run_id, &media, &wire)
        .unwrap()
    {
        Claim::Ready(owner) => owner,
        Claim::Cached(_) => unreachable!(),
    };
    Harness {
        _dir: dir,
        catalog,
        policy,
        run_id,
        context,
        media,
        wire,
        owner: Some(owner),
    }
}

#[test]
fn observed_response_survives_missing_domain_result_and_recovers_without_transmission() {
    let mut h = harness();
    let transport = FakeTransport {
        catalog: &h.catalog,
        policy: &h.policy,
        prepares: AtomicUsize::new(0),
        transmits: AtomicUsize::new(0),
        response: FakeTransport::accepted(),
        fail_after_dispatch: false,
        before_prepare: None,
    };
    let (key, call) = leaf(&h, &transport).transmit(&|| false).unwrap();
    let evidence = h.owner.as_ref().unwrap().evidence_id.clone();
    let raw = h
        .catalog
        .native_video_raw_response(&h.policy.project_id, &evidence)
        .unwrap()
        .unwrap();
    assert_eq!(raw.body, call.raw_response);
    assert_eq!(raw.http_status, Some(200));
    assert!(
        h.catalog
            .native_video_window_result(&h.policy.project_id, &evidence)
            .unwrap()
            .is_none()
    );
    drop(h.owner.take()); // Simulated exit after financial settlement, before domain persistence.
    let asset = h.catalog.get_asset(&h.media.view().asset_id).unwrap();
    h.catalog
        .update_asset(
            &asset.id,
            &crate::AssetPatch {
                notes: asset.notes,
                labels: asset.labels,
                removed: true,
                rotation_degrees: asset.rotation_degrees,
                expected_revision: Some(asset.revision),
            },
        )
        .unwrap();
    let recovered = h
        .catalog
        .recover_native_video_window(&h.policy.project_id, &evidence)
        .unwrap();
    assert_eq!(recovered.state, "completed");
    assert!(recovered.reused);
    assert_eq!(
        recovered.provenance.request_key.as_deref(),
        Some(key.as_str())
    );
    assert_eq!(transport.transmits.load(Ordering::SeqCst), 1);
    assert!(
        h.catalog
            .get_asset(&h.media.view().asset_id)
            .unwrap()
            .removed
    );
    let receipt = h.catalog.analysis_request(&key).unwrap().unwrap();
    assert!(receipt.estimated_cost_usd.is_some());
    assert_eq!(receipt.reported_cost_usd, None);
}

#[test]
fn prior_native_asset_uncertainty_survives_a_different_source_identity_recipe() {
    let h = harness();
    let failed = FakeTransport {
        catalog: &h.catalog,
        policy: &h.policy,
        prepares: AtomicUsize::new(0),
        transmits: AtomicUsize::new(0),
        response: vec![],
        fail_after_dispatch: true,
        before_prepare: None,
    };
    assert!(leaf(&h, &failed).transmit(&|| false).is_err());
    // Historical source identity recipes are opaque and can differ after a
    // tool/representation upgrade; the stable asset association remains.
    h.catalog.connection().unwrap().execute("UPDATE analysis_request_reservations SET source_fingerprint='historical-native-source-identity' WHERE run_id=?1",[&h.run_id]).unwrap();
    let mut request = h.policy.request();
    request.budget_usd = 2.0;
    let RunPolicyParameters::NativeVideoReview { options } = &mut request.parameters else {
        unreachable!()
    };
    options.thinking = NativeVideoThinking::Low;
    let policy = h.catalog.authorize_run_policy(&request).unwrap().policy;
    h.catalog
        .initialize_analysis_accounting_authority(
            &crate::analysis::run_policy::pilot_authority_context(&policy),
        )
        .unwrap();
    let run = h
        .catalog
        .begin_accounted_analysis_run(&policy.project_id, &policy.policy_id, "pilot")
        .unwrap();
    let media = h
        .catalog
        .prepare_native_video_window(
            &policy,
            &super::media::media_tests::window(&policy, 1.0),
            &|| false,
        )
        .unwrap();
    let RunPolicyParameters::NativeVideoReview { options } = &policy.parameters else {
        unreachable!()
    };
    let wire = protocol::prepare(options, &media).unwrap();
    let Claim::Ready(owner) = h
        .catalog
        .claim_native_video_window(&policy, &run, &media, &wire)
        .unwrap()
    else {
        unreachable!()
    };
    let mut context = crate::analysis::run_policy::pilot_accounting_context_for_scope(
        &policy,
        &run,
        &policy.scope[0],
        false,
    )
    .unwrap();
    context.intended_attempt_key = format!(
        "native_video_review:{}:{}",
        media.view().asset_id,
        media.view().identity
    );
    let transport = FakeTransport {
        catalog: &h.catalog,
        policy: &policy,
        prepares: AtomicUsize::new(0),
        transmits: AtomicUsize::new(0),
        response: FakeTransport::accepted(),
        fail_after_dispatch: false,
        before_prepare: None,
    };
    let denied = AccountedNativeVideoProvider {
        catalog: &h.catalog,
        policy: &policy,
        context,
        media: &media,
        evidence_id: &owner.evidence_id,
        request: &wire,
        transport: &transport,
    }
    .transmit(&|| false)
    .err()
    .unwrap();
    assert!(denied.to_string().contains("prior native video asset work"));
    assert_eq!(transport.transmits.load(Ordering::SeqCst), 0);
}

#[test]
fn transport_panics_preserve_pre_and_post_dispatch_accounting_boundaries() {
    struct Panicking {
        before: bool,
    }
    impl Transport for Panicking {
        fn prepare(&self) -> Result<()> {
            if self.before {
                panic!("fixture local preparation panic");
            }
            Ok(())
        }
        fn transmit(
            &self,
            _: &PreparedRequest,
            _: &NativeVideoOptions,
            _: &dyn Fn() -> bool,
        ) -> Result<CallResult> {
            panic!("fixture post-dispatch transport panic")
        }
    }
    for before in [true, false] {
        let h = harness();
        let transport = Panicking { before };
        assert!(leaf(&h, &transport).transmit(&|| false).is_err());
        let request = h
            .catalog
            .list_analysis_requests(&h.policy.project_id, Some(&h.policy.policy_id), 10)
            .unwrap()
            .remove(0);
        if before {
            assert_eq!(request.lifecycle, AnalysisRequestLifecycle::Released);
            assert!(request.dispatch_at.is_none());
            assert_eq!(request.uncertain_usage_reserve_usd, 0.0);
        } else {
            assert_eq!(request.lifecycle, AnalysisRequestLifecycle::Uncertain);
            assert!(request.dispatch_at.is_some());
            assert!(request.uncertain_usage_reserve_usd > 0.0);
        }
    }
}

fn leaf<'a>(h: &'a Harness, transport: &'a dyn Transport) -> AccountedNativeVideoProvider<'a> {
    AccountedNativeVideoProvider {
        catalog: &h.catalog,
        policy: &h.policy,
        context: h.context.clone(),
        media: &h.media,
        evidence_id: &h.owner.as_ref().unwrap().evidence_id,
        request: &h.wire,
        transport,
    }
}

#[test]
fn success_settles_shared_accounting_persists_result_and_exact_cache_hit_is_free() {
    let mut h = harness();
    let transport = FakeTransport {
        catalog: &h.catalog,
        policy: &h.policy,
        prepares: AtomicUsize::new(0),
        transmits: AtomicUsize::new(0),
        response: FakeTransport::accepted(),
        fail_after_dispatch: false,
        before_prepare: None,
    };
    let (request_key, call) = leaf(&h, &transport).transmit(&|| false).unwrap();
    assert_eq!(transport.prepares.load(Ordering::SeqCst), 1);
    assert_eq!(transport.transmits.load(Ordering::SeqCst), 1);
    let request = h.catalog.analysis_request(&request_key).unwrap().unwrap();
    assert_eq!(request.lifecycle, AnalysisRequestLifecycle::Completed);
    assert_eq!(request.reported_cost_usd, None);
    assert!(request.estimated_cost_usd.is_some());
    let owner = h.owner.take().unwrap();
    let saved = h
        .catalog
        .complete_native_video_window(&owner, call, request_key, &h.media)
        .unwrap();
    assert_eq!(saved.state, "completed");
    drop(owner);
    let before = h
        .catalog
        .analysis_accounting_report(&h.policy.project_id, Some(&h.policy.policy_id), 100)
        .unwrap()
        .requests
        .len();
    let cached = h
        .catalog
        .claim_native_video_window(&h.policy, &h.run_id, &h.media, &h.wire)
        .unwrap();
    assert!(matches!(cached, Claim::Cached(result) if result.reused));
    let after = h
        .catalog
        .analysis_accounting_report(&h.policy.project_id, Some(&h.policy.policy_id), 100)
        .unwrap()
        .requests
        .len();
    assert_eq!(before, after);
    assert_eq!(transport.transmits.load(Ordering::SeqCst), 1);
}

#[test]
fn conserved_usage_estimate_persists_without_forging_missing_raw_counters() {
    let h = harness();
    let transport = FakeTransport {
        catalog: &h.catalog,
        policy: &h.policy,
        prepares: AtomicUsize::new(0),
        transmits: AtomicUsize::new(0),
        response: FakeTransport::conserved_missing_auxiliary(),
        fail_after_dispatch: false,
        before_prepare: None,
    };
    let (key, call) = leaf(&h, &transport).transmit(&|| false).unwrap();
    let usage = call.receipt.usage_pricing.as_ref().unwrap();
    assert!(call.receipt.usage_complete);
    assert_eq!(usage.cached_prompt_tokens, None);
    assert_eq!(usage.thinking_tokens, None);
    assert_eq!(usage.tool_prompt_tokens, None);
    assert_eq!(call.receipt.completion_tokens, None);
    let request = h.catalog.analysis_request(&key).unwrap().unwrap();
    assert_eq!(request.lifecycle, AnalysisRequestLifecycle::Completed);
    assert_eq!(request.reported_cost_usd, None);
    assert!(request.estimated_cost_usd.is_some());
}

#[test]
fn wrong_tier_is_retained_as_rejected_uncertain_usage_without_invoice_or_estimate() {
    let h = harness();
    let transport = FakeTransport {
        catalog: &h.catalog,
        policy: &h.policy,
        prepares: AtomicUsize::new(0),
        transmits: AtomicUsize::new(0),
        response: FakeTransport::wrong_tier(),
        fail_after_dispatch: false,
        before_prepare: None,
    };
    let (key, call) = leaf(&h, &transport).transmit(&|| false).unwrap();
    assert_eq!(
        call.output.unwrap_err(),
        "provider_tier_missing_or_mismatched"
    );
    let request = h.catalog.analysis_request(&key).unwrap().unwrap();
    assert_eq!(request.reported_cost_usd, None);
    assert_eq!(request.estimated_cost_usd, None);
    assert!(request.uncertain_usage_reserve_usd > 0.0);
    assert_eq!(request.lifecycle, AnalysisRequestLifecycle::Uncertain);
}

#[test]
fn failure_after_dispatch_retains_reserve_and_claim_prevents_implicit_retransmission() {
    let h = harness();
    let transport = FakeTransport {
        catalog: &h.catalog,
        policy: &h.policy,
        prepares: AtomicUsize::new(0),
        transmits: AtomicUsize::new(0),
        response: vec![],
        fail_after_dispatch: true,
        before_prepare: None,
    };
    assert!(leaf(&h, &transport).transmit(&|| false).is_err());
    let rows = h
        .catalog
        .list_analysis_requests(&h.policy.project_id, Some(&h.policy.policy_id), 100)
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].lifecycle, AnalysisRequestLifecycle::Uncertain);
    assert!(rows[0].uncertain_usage_reserve_usd > 0.0);
    assert!(
        h.catalog
            .claim_native_video_window(&h.policy, &h.run_id, &h.media, &h.wire)
            .is_err()
    );
    assert_eq!(transport.transmits.load(Ordering::SeqCst), 1);
}

#[test]
fn cancellation_after_reservation_but_before_dispatch_releases_without_transport() {
    let h = harness();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancelled);
    let callback = move || flag.store(true, Ordering::SeqCst);
    let transport = FakeTransport {
        catalog: &h.catalog,
        policy: &h.policy,
        prepares: AtomicUsize::new(0),
        transmits: AtomicUsize::new(0),
        response: FakeTransport::accepted(),
        fail_after_dispatch: false,
        before_prepare: Some(Box::new(callback)),
    };
    let flag = Arc::clone(&cancelled);
    let error = match leaf(&h, &transport).transmit(&|| flag.load(Ordering::SeqCst)) {
        Ok(_) => panic!("cancelled request unexpectedly transmitted"),
        Err(error) => error,
    };
    assert!(!error.to_string().is_empty());
    let rows = h
        .catalog
        .list_analysis_requests(&h.policy.project_id, Some(&h.policy.policy_id), 100)
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].lifecycle, AnalysisRequestLifecycle::Released);
    assert_eq!(rows[0].uncertain_usage_reserve_usd, 0.0);
    assert_eq!(transport.transmits.load(Ordering::SeqCst), 0);
}

#[test]
fn source_change_during_prepare_fails_guard_before_transport_and_releases() {
    let h = harness();
    let path = Arc::new(std::path::PathBuf::from(
        h.catalog.get_asset(&h.media.view().asset_id).unwrap().path,
    ));
    let change = {
        let path = Arc::clone(&path);
        move || std::fs::write(path.as_path(), b"mutated generated source").unwrap()
    };
    let transport = FakeTransport {
        catalog: &h.catalog,
        policy: &h.policy,
        prepares: AtomicUsize::new(0),
        transmits: AtomicUsize::new(0),
        response: FakeTransport::accepted(),
        fail_after_dispatch: false,
        before_prepare: Some(Box::new(change)),
    };
    assert!(leaf(&h, &transport).transmit(&|| false).is_err());
    let rows = h
        .catalog
        .list_analysis_requests(&h.policy.project_id, Some(&h.policy.policy_id), 100)
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].lifecycle, AnalysisRequestLifecycle::Released);
    assert_eq!(transport.transmits.load(Ordering::SeqCst), 0);
}
