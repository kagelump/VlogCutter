//! Native queue behavior through the shared fake transport and accounting owners.
use super::*;
use crate::analysis::native_video::transport::Transport;
use crate::{
    AnalysisPayload, Catalog, NativeVideoOptions, RunPolicy, RunPolicyParameters, RunPolicyRequest,
};
use anyhow::{Result, anyhow};
use serde_json::json;
use std::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread,
    time::Duration,
};

fn moment_reply() -> String {
    json!({"version":1,"clock":"localVideoZero","moments":[{
        "id":"generated-moment","score":0.8,
        "core":{"start":"00:00.250","end":"00:00.750"},
        "payoff":{"start":"00:00.500","end":"00:00.750"},
        "setup":null,"context":null,
        "allowedRange":{"start":"00:00.250","end":"00:00.750"},
        "reason":"A visible movement resolves.","event":"Movement","role":"visual event",
        "evidenceRefs":["nativeVideo"],"uncertainty":["Needs human review."],
        "dialogueBoundaries":[],"longHold":false,"speechBasis":"none"
    }],"flags":[]})
    .to_string()
}

struct FakeTransport {
    catalog: Catalog,
    project_id: String,
    calls: AtomicUsize,
    delay: Duration,
    expensive_after_call: Option<usize>,
    cancel_on_call: AtomicBool,
    cancellation_observed: AtomicBool,
}

impl FakeTransport {
    fn new(catalog: &Catalog, project_id: &str) -> Self {
        Self {
            catalog: catalog.clone(),
            project_id: project_id.into(),
            calls: AtomicUsize::new(0),
            delay: Duration::ZERO,
            expensive_after_call: None,
            cancel_on_call: AtomicBool::new(false),
            cancellation_observed: AtomicBool::new(false),
        }
    }
    fn response(expensive: bool, call: usize) -> Vec<u8> {
        let usage = if expensive {
            json!({"serviceTier":"standard","promptTokenCount":1_000_000,"cachedContentTokenCount":0,"candidatesTokenCount":20,"thoughtsTokenCount":0,"toolUsePromptTokenCount":0,"totalTokenCount":1_000_020})
        } else {
            json!({"serviceTier":"standard","promptTokenCount":100,"cachedContentTokenCount":0,"candidatesTokenCount":20,"thoughtsTokenCount":0,"toolUsePromptTokenCount":0,"totalTokenCount":120})
        };
        serde_json::to_vec(&json!({
            "responseId":format!("queue-fake-generation-{call}"), "modelVersion":NATIVE_VIDEO_MODEL,
            "usageMetadata":usage,
            "candidates":[{"finishReason":"STOP","content":{"parts":[{"text":moment_reply()}]}}]
        }))
        .unwrap()
    }
    fn newest_run_id(&self) -> Result<String> {
        self.catalog
            .connection()?
            .query_row(
                "SELECT run_id FROM native_video_run_details ORDER BY rowid DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .map_err(Into::into)
    }
}

impl Transport for FakeTransport {
    fn prepare(&self) -> Result<()> {
        Ok(())
    }
    fn transmit(
        &self,
        _request: &protocol::PreparedRequest,
        options: &NativeVideoOptions,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<protocol::CallResult> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.delay.is_zero() {
            thread::sleep(self.delay);
        }
        if self.cancel_on_call.load(Ordering::Acquire) {
            let run_id = self.newest_run_id()?;
            self.catalog
                .cancel_native_video_run(&self.project_id, &run_id)?;
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while !cancelled() && std::time::Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if cancelled() {
                self.cancellation_observed.store(true, Ordering::Release);
            }
            return Err(anyhow!(
                "fake transport stopped after durable user cancellation"
            ));
        }
        let expensive = self.expensive_after_call.is_some_and(|first| call >= first);
        protocol::decode_response(200, Self::response(expensive, call), None, 100, options)
    }
}

fn configured_request(
    catalog: &Catalog,
    described: &RunPolicy,
    asset: &str,
    budget: f64,
    api_workers: u32,
) -> RunPolicyRequest {
    let mut request = described.request();
    request.budget_usd = budget;
    let RunPolicyParameters::NativeVideoReview { options } = &mut request.parameters else {
        unreachable!()
    };
    options.max_window_seconds = 1;
    options.max_media_seconds = 6;
    options.max_requests = 6;
    options.media_workers = 2;
    options.api_workers = api_workers;
    options.source_scopes[0].asset_id = asset.into();
    let _ = catalog;
    request
}

fn fixture(
    budget: f64,
    api_workers: u32,
) -> (tempfile::TempDir, Catalog, RunPolicyRequest, String) {
    let (dir, catalog, described, asset) = super::media::media_tests::setup(AnalysisPayload::Video);
    let request = configured_request(&catalog, &described, &asset, budget, api_workers);
    let policy = catalog.authorize_run_policy(&request).unwrap().policy;
    (dir, catalog, request, policy.policy_id)
}

fn bulk(
    catalog: &Catalog,
    policy_id: &str,
    transport: &FakeTransport,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<NativeVideoRunReport> {
    catalog.run_native_video_review_with_transport(
        policy_id,
        None,
        transport,
        cancelled,
        &mut |_| {},
    )
}

#[test]
fn pilot_gate_cache_bulk_projection_progress_and_status_are_durable_and_free_on_replay() {
    let (_dir, catalog, request, policy_id) = fixture(1.0, 2);
    let transport = FakeTransport::new(&catalog, &request.project_id);
    let never = || false;
    assert!(bulk(&catalog, &policy_id, &transport, &never).is_err());
    assert_eq!(
        transport.calls.load(Ordering::SeqCst),
        0,
        "bulk must require an exact passed pilot"
    );

    let pilot = catalog
        .run_native_video_pilot_with_transport(&request, &transport, &never)
        .unwrap();
    assert_eq!(pilot.status, "passed");
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        catalog
            .run_native_video_pilot_with_transport(&request, &transport, &never)
            .unwrap()
            .status,
        "passed"
    );
    assert_eq!(
        transport.calls.load(Ordering::SeqCst),
        1,
        "exact pilot evidence is reused"
    );

    let mut progress = Vec::new();
    let report = catalog
        .run_native_video_review_with_transport(&policy_id, None, &transport, &never, &mut |p| {
            progress.push(p)
        })
        .unwrap();
    assert_eq!(report.completed, 4);
    assert_eq!(report.interrupted, 0);
    assert_eq!(
        transport.calls.load(Ordering::SeqCst),
        4,
        "the pilot window is reused; three remaining windows transmit"
    );
    assert!(progress.len() >= 2);
    assert!(
        progress
            .windows(2)
            .all(|pair| pair[0].sequence <= pair[1].sequence
                && pair[0].completed <= pair[1].completed)
    );
    assert_eq!(report.evidence_count, 4);
    assert_eq!(report.evidence_ids.len(), 4);
    assert_eq!(report.moment_artifact_count, 1);
    assert!(report.accounting.estimated_cost_usd > 0.0);
    assert_eq!(report.accounting.confirmed_cost_usd, 0.0);

    let status = catalog
        .native_video_run_status(&request.project_id, &report.run_id)
        .unwrap();
    assert_eq!(status.state, "completed");
    assert_eq!(status.report.as_ref().unwrap().evidence_count, 4);
    let windows = catalog
        .list_native_video_run_windows(&request.project_id, &report.run_id, None, 100)
        .unwrap();
    assert_eq!(windows.len(), 4);
    assert!(windows.iter().all(|w| {
        w.state == "completed"
            && w.evidence_id
                .as_ref()
                .is_some_and(|id| report.evidence_ids.contains(id))
    }));
    let artifact_json: String = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT body_json FROM moment_artifacts WHERE asset_id=?1 ORDER BY rowid DESC LIMIT 1",
            [&request.asset_ids[0]],
            |r| r.get(0),
        )
        .unwrap();
    let artifact: crate::analysis::moments::MomentArtifact =
        serde_json::from_str(&artifact_json).unwrap();
    assert!(!artifact.candidates.is_empty());
    assert_eq!(
        artifact.provenance.payload,
        crate::analysis::moments::MomentPayload::Video
    );

    let replay = bulk(&catalog, &policy_id, &transport, &never).unwrap();
    assert_eq!(replay.completed, 4);
    assert_eq!(replay.reused, 4);
    assert_eq!(
        transport.calls.load(Ordering::SeqCst),
        4,
        "exact bulk replay is cache-only"
    );
}

#[test]
fn temporary_reservation_headroom_waits_for_settlement_without_budget_stop() {
    let (_dir, catalog, request, policy_id) = fixture(0.32, 2);
    let mut transport = FakeTransport::new(&catalog, &request.project_id);
    transport.delay = Duration::from_millis(100);
    let never = || false;
    let pilot = catalog
        .run_native_video_pilot_with_transport(&request, &transport, &never)
        .unwrap();
    assert_eq!(pilot.status, "passed");
    let report = bulk(&catalog, &policy_id, &transport, &never).unwrap();
    assert_eq!(report.completed, 4);
    assert!(!report.budget_stopped);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 4);
    assert_eq!(report.accounting.active_request_count, 0);
    assert_eq!(report.accounting.uncertain_request_count, 0);
    assert_eq!(report.accounting.uncertain_usage_reserve_usd, 0.0);
}

#[test]
fn high_usage_stops_new_claims_but_settles_in_flight_windows() {
    let (_dir, catalog, request, policy_id) = fixture(1.0, 2);
    let mut transport = FakeTransport::new(&catalog, &request.project_id);
    let never = || false;
    let pilot = catalog
        .run_native_video_pilot_with_transport(&request, &transport, &never)
        .unwrap();
    assert_eq!(pilot.status, "passed");
    transport.expensive_after_call = Some(1);
    let policy = catalog.load_run_authorization(&policy_id).unwrap().policy;
    let windows = (0..6)
        .map(|i| NativeVideoWindowRequest {
            policy_id: policy_id.clone(),
            scope_index: 0,
            source_in_seconds: i as f64 * 0.25,
            source_out_seconds: i as f64 * 0.25 + 1.0,
            fps: 1.0,
            max_width: 160,
        })
        .collect::<Vec<_>>();
    assert!(windows.iter().all(|w| w.source_out_seconds <= 4.0));
    let report = catalog
        .run_native_video_review_with_transport(
            &policy_id,
            Some(&windows),
            &transport,
            &never,
            &mut |_| {},
        )
        .unwrap();
    assert!(report.budget_stopped);
    assert!(
        report.completed >= 3 && report.completed < 6,
        "{}",
        report.completed
    );
    assert_eq!(report.interrupted, 6 - report.completed);
    assert_eq!(report.accounting.uncertain_request_count, 0);
    assert_eq!(report.accounting.active_request_count, 0);
    assert!(report.accounting.confirmed_cost_usd <= 0.0);
    let run_windows = catalog
        .list_native_video_run_windows(&request.project_id, &report.run_id, None, 100)
        .unwrap();
    assert!(
        run_windows
            .iter()
            .filter(|w| w.state == "completed")
            .all(|w| w.evidence_id.is_some())
    );
    assert!(
        run_windows
            .iter()
            .filter(|w| w.state == "not_completed")
            .all(|w| w.evidence_id.is_none())
    );
    let _ = policy;
}

#[test]
fn persistent_user_cancel_after_dispatch_retains_unknown_reserve_and_blocks_resend() {
    let (_dir, catalog, request, policy_id) = fixture(1.0, 1);
    let transport = FakeTransport::new(&catalog, &request.project_id);
    let never = || false;
    assert_eq!(
        catalog
            .run_native_video_pilot_with_transport(&request, &transport, &never)
            .unwrap()
            .status,
        "passed"
    );
    let policy = catalog.load_run_authorization(&policy_id).unwrap().policy;
    let one_window = crate::analysis::native_video::plan_native_video_windows(&policy)
        .unwrap()
        .into_iter()
        .nth(1)
        .unwrap();
    transport.cancel_on_call.store(true, Ordering::Release);
    let report = catalog
        .run_native_video_review_with_transport(
            &policy_id,
            Some(std::slice::from_ref(&one_window)),
            &transport,
            &never,
            &mut |_| {},
        )
        .unwrap();
    assert!(report.cancelled);
    assert!(transport.cancellation_observed.load(Ordering::Acquire));
    assert_eq!(report.accounting.uncertain_request_count, 1);
    assert!(report.accounting.uncertain_usage_reserve_usd > 0.0);
    let calls = transport.calls.load(Ordering::SeqCst);
    let replay = catalog.run_native_video_review_with_transport(
        &policy_id,
        Some(std::slice::from_ref(&one_window)),
        &transport,
        &never,
        &mut |_| {},
    );
    assert_eq!(
        transport.calls.load(Ordering::SeqCst),
        calls,
        "an uncertain dispatched payload cannot be resent implicitly"
    );
    assert!(
        replay.is_err(),
        "unknown usage blocks a new paid run before another transmission"
    );
}

#[test]
fn local_recovery_restores_observed_pilot_after_result_write_failure_without_another_send() {
    let (_dir, catalog, request, policy_id) = fixture(1.0, 2);
    let transport = FakeTransport::new(&catalog, &request.project_id);
    catalog.connection().unwrap().execute_batch("CREATE TRIGGER fixture_fail_native_result BEFORE UPDATE OF result_json ON native_video_calls WHEN NEW.result_json IS NOT NULL BEGIN SELECT RAISE(ABORT,'fixture_result_write_failure'); END;").unwrap();
    let failed = catalog
        .run_native_video_pilot_with_transport(&request, &transport, &|| false)
        .unwrap();
    assert_eq!(failed.status, "failed");
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    let windows = catalog
        .list_native_video_windows(&request.project_id, &policy_id, None, 100)
        .unwrap();
    assert_eq!(windows.len(), 1);
    assert!(windows[0].raw_response_available);
    assert!(!windows[0].result_available);
    catalog
        .connection()
        .unwrap()
        .execute_batch("DROP TRIGGER fixture_fail_native_result")
        .unwrap();
    let result = catalog
        .recover_native_video_window(&request.project_id, &windows[0].evidence_id)
        .unwrap();
    assert_eq!(result.state, "completed");
    assert!(result.reused);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        catalog.load_run_pilot(&policy_id).unwrap().unwrap().status,
        "passed"
    );
    assert!(
        catalog
            .authorized_policy_for_bulk(&policy_id, crate::AnalysisRunKind::NativeVideoReview)
            .is_ok()
    );
    let aggregate = catalog
        .analysis_accounting_aggregate(&policy_id, None)
        .unwrap();
    assert_eq!(aggregate.request_count, 1);
    assert!(aggregate.estimated_cost_usd > 0.0);
    assert_eq!(aggregate.confirmed_cost_usd, 0.0);
}

#[test]
fn failed_progress_observer_stops_workers_and_finishes_as_failure_not_user_cancel() {
    let (_dir, catalog, request, policy_id) = fixture(1.0, 2);
    let mut transport = FakeTransport::new(&catalog, &request.project_id);
    transport.delay = Duration::from_millis(100);
    catalog
        .run_native_video_pilot_with_transport(&request, &transport, &|| false)
        .unwrap();
    let report = catalog
        .run_native_video_review_with_transport(&policy_id, None, &transport, &|| false, &mut |p| {
            if p.sequence > 0 {
                panic!("fixture progress consumer stopped");
            }
        })
        .unwrap();
    assert!(!report.cancelled);
    assert!(report.interrupted > 0);
    assert_eq!(report.accounting.active_request_count, 0);
    assert_eq!(
        catalog
            .native_video_run_status(&request.project_id, &report.run_id)
            .unwrap()
            .state,
        "failed"
    );
}
