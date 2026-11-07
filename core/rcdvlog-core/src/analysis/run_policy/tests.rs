mod native_boundary_tests;
mod native_tests;
use super::*;
use crate::ProviderReceiptEvidence;
use crate::analysis::moments::{MomentPayload, MomentRunOptions};
use crate::analysis::provider::{
    AnalysisRequestCostBound, ProviderFailure, ProviderFailureCategory, VlmReceipt, VlmRequest,
    VlmResponse,
};
use crate::analysis::quality::DecodedSegment;
use crate::analysis::store::{StoredTranscript, TranscriptOutcomeReason, save_transcript};
use rusqlite::params;
use serde_json::{Value, json};
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Barrier, Mutex};
use tempfile::tempdir;

struct FakeProvider {
    calls: AtomicUsize,
    body: String,
    fail_transport: bool,
    fail_prepare: bool,
}

impl FakeProvider {
    fn observation() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            body: serde_json::json!({
                "title": "Synthetic pilot",
                "description": "A bounded synthetic observation.",
                "labels": [],
                "usable": true,
                "unusableReason": null,
                "bestMomentStartMs": null,
                "bestMomentEndMs": null,
                "thumbnailTimestampMs": null,
                "cameraMovement": "static",
                "ocrText": []
            })
            .to_string(),
            fail_transport: false,
            fail_prepare: false,
        }
    }

    fn moments(body: serde_json::Value) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            body: body.to_string(),
            fail_transport: false,
            fail_prepare: false,
        }
    }
}

impl VlmProvider for FakeProvider {
    fn prepare(&self) -> Result<()> {
        if self.fail_prepare {
            bail!("synthetic credentials unavailable");
        }
        Ok(())
    }

    fn analysis_cost_bound(
        &self,
        _model: &str,
        _request: &VlmRequest,
    ) -> Result<AnalysisRequestCostBound> {
        Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
            0.02,
        ))
    }

    fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
        let _call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_transport {
            bail!("synthetic transport interruption");
        }
        Ok(VlmResponse {
            model: model.into(),
            body: self.body.clone(),
            prompt_tokens: 10,
            completion_tokens: 5,
            cost_usd: 0.01,
            latency_ms: 1,
            receipt: VlmReceipt {
                actual_model: Some(model.into()),
                provider: Some("synthetic".into()),
                generation_id: Some(format!("generation-{}", uuid::Uuid::new_v4())),
                usage_complete: true,
            },
        })
    }
}

pub(super) fn generated_asset(
    catalog: &Catalog,
    project_id: &str,
    root: &std::path::Path,
    id: &str,
    seconds: u32,
) -> String {
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir).unwrap();
    let source = source_dir.join(format!("{id}.mp4"));
    let status = crate::tool_command("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg(format!(
            "color=c=0x{}:s=320x180:r=24:d={seconds}",
            &blake3::hash(id.as_bytes()).to_hex()[..6]
        ))
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&source)
        .status();
    let Ok(status) = status else {
        panic!("synthetic run-policy fixture requires ffmpeg")
    };
    assert!(status.success(), "synthetic fixture generation failed");
    crate::tests::insert_asset_at(catalog, project_id, id, &source);
    let asset_id = id.to_string();
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET duration_seconds=?2,source_out=?2 WHERE id=?1",
            params![asset_id, seconds],
        )
        .unwrap();
    catalog.validate_asset_video(&asset_id).unwrap();
    catalog.generate_proxy(&asset_id).unwrap();
    asset_id
}

pub(super) fn fixture() -> (tempfile::TempDir, Catalog, String, String) {
    let directory = tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Synthetic run policy").unwrap();
    let asset = generated_asset(&catalog, &project.id, directory.path(), "clip", 2);
    (directory, catalog, project.id, asset)
}

fn observation_request(project: &str, asset: &str, budget: f64) -> RunPolicyRequest {
    RunPolicyRequest::observation(
        project,
        vec![asset.to_string()],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.to_string()],
        false,
        budget,
        false,
    )
}

fn authorized_observation(
    catalog: &Catalog,
    project: &str,
    asset: &str,
) -> (RunPolicyRequest, RunPolicy, RunPilotRecord) {
    let request = observation_request(project, asset, 1.0);
    let policy = catalog.describe_run_policy(&request).unwrap();
    assert!(
        policy.readiness.ready_for_authorization,
        "{:#?}",
        policy.readiness
    );
    catalog.authorize_run_policy(&request).unwrap();
    let provider = FakeProvider::observation();
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &provider, false)
        .unwrap();
    assert_eq!(pilot.status, "passed", "{pilot:?}");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    (request, policy, pilot)
}

#[test]
fn denied_and_foreign_scope_fail_before_authorization() {
    let (_directory, catalog, project, asset) = fixture();
    let other = catalog.create_project("Other project").unwrap();
    let mut request = observation_request(&project, &asset, 1.0);
    request.asset_ids = vec!["missing".into()];
    assert!(catalog.describe_run_policy(&request).is_err());
    request.asset_ids = vec![asset.clone()];
    request.project_id = other.id;
    assert!(catalog.describe_run_policy(&request).is_err());
}

#[test]
fn unsupported_audio_and_fallback_choices_fail_closed() {
    let (_directory, catalog, project, asset) = fixture();
    let mut request = observation_request(&project, &asset, 1.0);
    request.payload = AnalysisPayload::AudioAndTranscript;
    let policy = catalog.describe_run_policy(&request).unwrap();
    assert!(!policy.readiness.capabilities_ready);
    assert!(!policy.readiness.ready_for_authorization);
    assert!(catalog.authorize_run_policy(&request).is_err());
    assert!(policy.capability_checks.iter().any(|check| {
        check.requirement == "payload.audio_backend" && check.status == CapabilityStatus::Failed
    }));

    let mut invalid_fallback = observation_request(&project, &asset, 1.0);
    invalid_fallback.allow_fallback = true;
    invalid_fallback.allowed_models = vec![DEFAULT_MODEL.into(), "unknown/model".into()];
    let policy = catalog.describe_run_policy(&invalid_fallback).unwrap();
    assert!(!policy.readiness.capabilities_ready);
    assert!(policy.capability_checks.iter().any(|check| {
        check.requirement.contains("unknown/model") && check.status == CapabilityStatus::Failed
    }));
}

#[test]
fn policy_budget_does_not_authorize_a_different_model_or_payload() {
    let (_directory, catalog, project, asset) = fixture();
    let request = observation_request(&project, &asset, 1.0);
    let authorized = catalog.authorize_run_policy(&request).unwrap();
    assert_eq!(
        authorized.policy.provider_cost_bound_contract,
        analysis_cost_bound_contract_id()
    );
    let mut changed_contract = authorized.policy.clone();
    changed_contract
        .provider_cost_bound_contract
        .push_str("-changed");
    assert_ne!(
        run_policy_identity(&changed_contract).unwrap(),
        authorized.policy.policy_id
    );

    let mut changed_model = request.clone();
    changed_model.primary_model = FALLBACK_MODEL.into();
    changed_model.allowed_models = vec![FALLBACK_MODEL.into()];
    let changed_policy = catalog.describe_run_policy(&changed_model).unwrap();
    assert_ne!(changed_policy.policy_id, authorized.policy.policy_id);
    assert!(
        catalog
            .load_run_authorization(&changed_policy.policy_id)
            .is_err()
    );

    let mut changed_payload = request.clone();
    changed_payload.payload = AnalysisPayload::Frames;
    let changed_policy = catalog.describe_run_policy(&changed_payload).unwrap();
    assert_ne!(changed_policy.policy_id, authorized.policy.policy_id);
    assert!(
        catalog
            .load_run_authorization(&changed_policy.policy_id)
            .is_err()
    );
}

#[test]
fn source_change_invalidates_authorization_and_pilot() {
    let (_directory, catalog, project, asset) = fixture();
    let (request, policy, _pilot) = authorized_observation(&catalog, &project, &asset);
    assert!(
        catalog
            .authorized_policy_for_bulk(&policy.policy_id, AnalysisRunKind::Observation)
            .is_ok()
    );
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET duration_seconds=3.0,source_out=3.0 WHERE id=?1",
            [&asset],
        )
        .unwrap();
    let error = catalog
        .authorized_policy_for_bulk(&policy.policy_id, AnalysisRunKind::Observation)
        .unwrap_err();
    assert!(format!("{error:#}").contains("changed"));
    assert_eq!(
        catalog.describe_run_policy(&request).unwrap().policy_id,
        catalog.describe_run_policy(&request).unwrap().policy_id
    );
}

#[test]
fn a_schema_valid_moment_response_with_invalid_source_times_fails_the_pilot() {
    let (_directory, catalog, project, asset) = fixture();
    let mut request = RunPolicyRequest::moment_discovery(
        project.clone(),
        vec![asset],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into()],
        false,
        AnalysisPayload::Frames,
        1.0,
        8,
        8,
        1,
        EditProfile::default(),
        false,
        false,
    );
    catalog.authorize_run_policy(&request).unwrap();
    let provider = FakeProvider::moments(serde_json::json!({
        "moments": [{
            "event": "inverted synthetic moment",
            "role": "action",
            "reason": "schema-valid but semantically invalid source times",
            "score": 0.5,
            "coreStartMs": 2000,
            "coreEndMs": 1000,
            "payoffStartMs": 500,
            "payoffEndMs": 1500,
            "setup": null,
            "context": null,
            "longHold": false,
            "evidenceKeys": ["frame:400"],
            "uncertainty": []
        }],
        "uncertain": false,
        "reason": "synthetic"
    }));
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &provider, false)
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(pilot.status, "failed", "{pilot:?}");
    assert!(pilot.validation.structural_schema_valid);
    assert!(!pilot.validation.source_time_semantics_valid);
    assert!(
        catalog
            .authorized_policy_for_bulk(&pilot.policy_id, AnalysisRunKind::MomentDiscovery)
            .is_err()
    );

    // A budget-only or sample approval cannot broaden to another asset.
    request.asset_ids.push("not-in-scope".into());
    assert!(catalog.describe_run_policy(&request).is_err());
}

#[test]
fn interrupted_pilot_is_recorded_and_not_retried_implicitly() {
    let (_directory, catalog, project, asset) = fixture();
    let request = observation_request(&project, &asset, 1.0);
    catalog.authorize_run_policy(&request).unwrap();
    let provider = FakeProvider {
        calls: AtomicUsize::new(0),
        body: String::new(),
        fail_transport: true,
        fail_prepare: false,
    };
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &provider, false)
        .unwrap();
    assert_eq!(pilot.status, "uncertain");
    assert!(!pilot.usage_complete);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let error = catalog
        .run_run_policy_pilot_with_provider(&request, &provider, false)
        .unwrap_err();
    assert!(format!("{error:#}").contains("unknown usage"));
    // prepare failure is before any provider call and does not consume budget.
    let prepare_failure = FakeProvider {
        calls: AtomicUsize::new(0),
        body: String::new(),
        fail_transport: false,
        fail_prepare: true,
    };
    let mut second = observation_request(&project, &asset, 1.0);
    second.parameters = RunPolicyParameters::Observation { force: true };
    catalog.authorize_run_policy(&second).unwrap();
    assert!(
        catalog
            .run_run_policy_pilot_with_provider(&second, &prepare_failure, false)
            .is_err()
    );
    assert_eq!(prepare_failure.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn authorized_analysis_executes_only_policy_scope_and_counts_pilot_cost() {
    let (directory, catalog, project, first_asset) = fixture();
    let second_asset = generated_asset(&catalog, &project, directory.path(), "clip-two", 2);
    let request = observation_request(&project, &first_asset, 1.0);
    catalog.authorize_run_policy(&request).unwrap();
    let pilot_provider = FakeProvider::observation();
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &pilot_provider, false)
        .unwrap();
    assert_eq!(pilot.status, "passed");
    let state = catalog
        .authorized_policy_for_bulk(&pilot.policy_id, AnalysisRunKind::Observation)
        .unwrap();
    assert!(
        (state.remaining_budget_usd - (1.0 - pilot.cost_usd)).abs() < f64::EPSILON,
        "pilot spend must count against the same policy ceiling"
    );

    let bulk_provider = FakeProvider::observation();
    let report = catalog
        .analyze_authorized_with_provider(&pilot.policy_id, &bulk_provider, |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(bulk_provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(report.analyzed, 1);
    assert!(
        catalog
            .list_assets(&project)
            .unwrap()
            .iter()
            .find(|asset| asset.id == second_asset)
            .is_some()
    );
    assert!(
        crate::analysis::vlm_store::load_analysis(&catalog.connection().unwrap(), &second_asset)
            .unwrap()
            .is_none(),
        "the exact policy scope must not drift to another project asset"
    );
}

#[test]
fn moment_plan_reports_policy_readiness_and_exact_scope_identity() {
    let (_directory, catalog, project, asset) = fixture();
    let options = MomentRunOptions {
        model: DEFAULT_MODEL.into(),
        provider: "openrouter".into(),
        payload: MomentPayload::Frames,
        budget_usd: 1.0,
        coarse_frames: 8,
        refinement_frames: 8,
        max_candidates: 1,
        profile: EditProfile::default(),
        force: false,
        retry_uncertain: false,
        allowed_models: vec![DEFAULT_MODEL.into()],
        allow_fallback: false,
        policy_id: None,
    };
    let plan = catalog
        .plan_moments(&project, std::slice::from_ref(&asset), &options)
        .unwrap();
    assert_eq!(plan.run_policy.scope[0].asset_id, asset);
    assert!(
        plan.run_policy.scope[0]
            .representation_fingerprint
            .is_some()
    );
    assert!(plan.run_policy.scope[0].source_identity.len() >= 32);
    assert_eq!(plan.run_policy.budget_usd, 1.0);
    assert!(plan.run_policy.expected_cost_usd > 0.0);
    assert!(plan.run_policy.readiness.ready_for_authorization);
}
#[test]
fn authorized_moment_execution_uses_exact_scope_and_rejects_broadening() {
    let (directory, catalog, project, first_asset) = fixture();
    let _second_asset = generated_asset(&catalog, &project, directory.path(), "clip-two", 2);
    let request = RunPolicyRequest::moment_discovery(
        project.clone(),
        vec![first_asset.clone()],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into()],
        false,
        AnalysisPayload::Frames,
        1.0,
        8,
        8,
        1,
        EditProfile::default(),
        false,
        false,
    );
    let policy = catalog.authorize_run_policy(&request).unwrap().policy;
    let pilot_provider = FakeProvider::moments(serde_json::json!({
        "moments": [],
        "uncertain": true,
        "reason": "synthetic frames only"
    }));
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &pilot_provider, false)
        .unwrap();
    assert_eq!(pilot.status, "passed", "{pilot:?}");

    let mut broadened_options = policy.moment_options().unwrap();
    broadened_options.policy_id = Some(policy.policy_id.clone());
    let broadening_provider = FakeProvider::moments(serde_json::json!({
        "moments": [],
        "uncertain": true,
        "reason": "must not be called"
    }));
    let error = catalog
        .discover_moments_with_provider(
            &project,
            &[first_asset.clone(), "clip-two".into()],
            &broadened_options,
            &broadening_provider,
            |_| std::ops::ControlFlow::Continue(()),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("exactly match"));
    assert_eq!(broadening_provider.calls.load(Ordering::SeqCst), 0);

    let run_provider = FakeProvider::moments(serde_json::json!({
        "moments": [],
        "uncertain": true,
        "reason": "synthetic exact scope"
    }));
    let report = catalog
        .discover_authorized_moments_with_provider(&policy.policy_id, &run_provider, |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(report.total_assets, 1);
    assert!(run_provider.calls.load(Ordering::SeqCst) >= 1);
}

#[test]
fn unknown_model_and_schema_capability_are_rejected_before_authorization() {
    let (_directory, catalog, project, asset) = fixture();
    let mut request = observation_request(&project, &asset, 1.0);
    request.primary_model = "unknown/unsupported-model".into();
    request.allowed_models = vec!["unknown/unsupported-model".into()];
    let policy = catalog.describe_run_policy(&request).unwrap();
    assert!(!policy.readiness.capabilities_ready);
    assert!(!policy.readiness.ready_for_authorization);
    assert!(policy.capability_checks.iter().any(|check| {
        check.status == CapabilityStatus::Failed
            && (check.requirement.contains("unknown/unsupported-model")
                || check.requirement.contains("schema"))
    }));
    assert!(catalog.authorize_run_policy(&request).is_err());
}

#[test]
fn real_pilot_entrypoint_refuses_missing_credentials_before_provider_creation() {
    if crate::api_credential_present() {
        eprintln!("credential variable present; skipping missing-credential assertion");
        return;
    }
    let (_directory, catalog, project, asset) = fixture();
    let request = observation_request(&project, &asset, 1.0);
    catalog.authorize_run_policy(&request).unwrap();
    let error = catalog.run_run_policy_pilot(&request, false).unwrap_err();
    assert!(format!("{error:#}").contains("no pilot request was made"));
    assert!(
        catalog
            .load_run_pilot(&catalog.describe_run_policy(&request).unwrap().policy_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn cancellation_before_first_bulk_request_stops_without_provider_calls() {
    let (_directory, catalog, project, asset) = fixture();
    let request = observation_request(&project, &asset, 1.0);
    catalog.authorize_run_policy(&request).unwrap();
    let pilot_provider = FakeProvider::observation();
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &pilot_provider, false)
        .unwrap();
    assert_eq!(pilot.status, "passed");
    let provider = FakeProvider::observation();
    let report = catalog
        .analyze_authorized_with_provider(&pilot.policy_id, &provider, |_| {
            std::ops::ControlFlow::Break(())
        })
        .unwrap();
    assert!(report.cancelled);
    assert_eq!(report.analyzed, 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}
pub(super) struct CapturingProvider {
    pub(super) calls: AtomicUsize,
    requests: Mutex<Vec<VlmRequest>>,
    body: String,
    fail_message: Option<String>,
    cost_usd: f64,
    usage_complete: bool,
}

impl CapturingProvider {
    pub(super) fn observation() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            body: serde_json::json!({
                "title": "Synthetic captured observation",
                "description": "A bounded synthetic observation.",
                "labels": [],
                "usable": true,
                "unusableReason": null,
                "bestMomentStartMs": null,
                "bestMomentEndMs": null,
                "thumbnailTimestampMs": null,
                "cameraMovement": "static",
                "ocrText": []
            })
            .to_string(),
            fail_message: None,
            cost_usd: 0.01,
            usage_complete: true,
        }
    }

    fn failing(message: &str) -> Self {
        let mut provider = Self::observation();
        provider.fail_message = Some(message.to_string());
        provider
    }

    fn with_body(body: &str) -> Self {
        let mut provider = Self::observation();
        provider.body = body.to_string();
        provider
    }

    fn with_incomplete_usage() -> Self {
        let mut provider = Self::observation();
        provider.usage_complete = false;
        provider
    }
}

impl VlmProvider for CapturingProvider {
    fn analysis_cost_bound(
        &self,
        _model: &str,
        _request: &VlmRequest,
    ) -> Result<AnalysisRequestCostBound> {
        Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
            0.02,
        ))
    }

    fn analyze(&self, model: &str, request: &VlmRequest) -> Result<VlmResponse> {
        let _call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.requests.lock().unwrap().push(request.clone());
        if let Some(message) = &self.fail_message {
            bail!("{message}");
        }
        Ok(VlmResponse {
            model: model.into(),
            body: self.body.clone(),
            prompt_tokens: 10,
            completion_tokens: 5,
            cost_usd: self.cost_usd,
            latency_ms: 1,
            receipt: VlmReceipt {
                actual_model: Some(model.into()),
                provider: Some("synthetic".into()),
                generation_id: Some(format!("generation-capture-{}", uuid::Uuid::new_v4())),
                usage_complete: self.usage_complete,
            },
        })
    }
}

struct PrepareBreaksProxy {
    calls: AtomicUsize,
    proxy_path: std::path::PathBuf,
}

impl VlmProvider for PrepareBreaksProxy {
    fn prepare(&self) -> Result<()> {
        // Simulate local representation loss after run admission but before
        // any paid upload. Repeated prepare calls are idempotent.
        let _ = fs::remove_file(&self.proxy_path);
        Ok(())
    }

    fn analysis_cost_bound(
        &self,
        _model: &str,
        _request: &VlmRequest,
    ) -> Result<AnalysisRequestCostBound> {
        Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
            0.02,
        ))
    }

    fn analyze(&self, _model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        bail!("local pre-upload test provider must never be called")
    }
}

fn save_private_transcript(catalog: &Catalog, asset_id: &str) {
    save_transcript(
        &catalog.connection().unwrap(),
        &StoredTranscript {
            asset_id: asset_id.to_string(),
            recipe_key: crate::analysis::STT_RECIPE_KEY.to_string(),
            source_fingerprint: catalog.proxy_fingerprint(asset_id).unwrap(),
            status: TranscriptStatus::Speech,
            outcome_reason: TranscriptOutcomeReason::Accepted,
            reject_reason: String::new(),
            primary_language: "en".into(),
            speech_seconds: 1.0,
            window_count: 1,
            provider_segment_count: 1,
            mean_token_probability: Some(0.9),
            repetition_ratio: None,
            segments: vec![DecodedSegment {
                start_ms: 400,
                end_ms: 1_600,
                language: "en".into(),
                text: "PRIVATE_TRANSCRIPT_CANARY".into(),
                mean_token_probability: Some(0.9),
            }],
        },
    )
    .unwrap();
}

fn save_review_private_transcript(catalog: &Catalog, asset_id: &str) {
    save_transcript(
        &catalog.connection().unwrap(),
        &StoredTranscript {
            asset_id: asset_id.to_string(),
            recipe_key: crate::analysis::STT_RECIPE_KEY.to_string(),
            source_fingerprint: catalog.proxy_fingerprint(asset_id).unwrap(),
            status: TranscriptStatus::ReviewRequired,
            outcome_reason: TranscriptOutcomeReason::QualityEvidenceIncomplete,
            reject_reason: "review_missing_quality_evidence".into(),
            primary_language: "en".into(),
            speech_seconds: 1.0,
            window_count: 1,
            provider_segment_count: 1,
            mean_token_probability: None,
            repetition_ratio: None,
            segments: vec![DecodedSegment {
                start_ms: 400,
                end_ms: 1_600,
                language: "en".into(),
                text: "REVIEW_CANDIDATE_PRIVATE_CANARY".into(),
                mean_token_probability: None,
            }],
        },
    )
    .unwrap();
}

#[test]
fn review_required_candidates_are_not_transcript_scope_or_prompt_evidence() {
    let (_directory, catalog, project, asset) = fixture();
    save_review_private_transcript(&catalog, &asset);

    let mut request = observation_request(&project, &asset, 1.0);
    request.parameters = RunPolicyParameters::Observation { force: true };
    let described = catalog.describe_run_policy(&request).unwrap();
    assert!(
        described.scope[0].transcript_fingerprint.is_none(),
        "review candidates must not be admitted as transcript scope, even with force"
    );
    assert!(
        described
            .readiness
            .warnings
            .iter()
            .any(|warning| warning.contains("no accepted current transcript")),
        "{:?}",
        described.readiness.warnings
    );
    catalog.authorize_run_policy(&request).unwrap();

    let provider = CapturingProvider::observation();
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &provider, false)
        .unwrap();
    assert_eq!(pilot.status, "passed", "{pilot:?}");
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        !requests[0]
            .prompt
            .contains("REVIEW_CANDIDATE_PRIVATE_CANARY"),
        "review candidate text escaped into a provider prompt"
    );
}

#[test]
fn frames_only_pilot_and_bulk_never_transmit_transcript_or_vocabulary() {
    let (_directory, catalog, project, asset) = fixture();
    catalog.add_label(&asset, "PRIVATE_VOCAB_CANARY").unwrap();
    save_private_transcript(&catalog, &asset);

    let mut request = observation_request(&project, &asset, 1.0);
    request.payload = AnalysisPayload::Frames;
    request.include_project_vocabulary = false;
    let described = catalog.describe_run_policy(&request).unwrap();
    assert!(described.scope[0].transcript_fingerprint.is_none());
    assert!(!described.context.project_vocabulary);
    catalog.authorize_run_policy(&request).unwrap();

    let pilot_provider = CapturingProvider::observation();
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &pilot_provider, false)
        .unwrap();
    assert_eq!(pilot.status, "passed", "{pilot:?}");
    assert!(!pilot.requested_model.is_empty());
    let pilot_requests = pilot_provider.requests.lock().unwrap();
    assert_eq!(pilot_requests.len(), 1);
    let pilot_prompt = &pilot_requests[0].prompt;
    assert!(!pilot_prompt.contains("PRIVATE_TRANSCRIPT_CANARY"));
    assert!(!pilot_prompt.contains("PRIVATE_VOCAB_CANARY"));
    assert!(!pilot_requests[0].frames.is_empty());
    drop(pilot_requests);

    let bulk_provider = CapturingProvider::observation();
    let report = catalog
        .analyze_authorized_with_provider(&pilot.policy_id, &bulk_provider, |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(report.analyzed, 1);
    let bulk_requests = bulk_provider.requests.lock().unwrap();
    assert_eq!(bulk_requests.len(), 1);
    assert!(
        !bulk_requests[0]
            .prompt
            .contains("PRIVATE_TRANSCRIPT_CANARY")
    );
    assert!(!bulk_requests[0].prompt.contains("PRIVATE_VOCAB_CANARY"));
}

#[test]
fn pilot_diagnostics_never_persist_provider_error_or_response_canaries() {
    let (_directory, catalog, project, asset) = fixture();
    let request = observation_request(&project, &asset, 1.0);
    catalog.authorize_run_policy(&request).unwrap();

    let provider = CapturingProvider::failing(
        "HTTP 500 sk-or-v1-PRIVATE-ERROR-CANARY /private/footage/canary.mp4 prompt-body-canary",
    );
    let record = catalog
        .run_run_policy_pilot_with_provider(&request, &provider, false)
        .unwrap();
    assert_eq!(record.status, "uncertain");
    let persisted = serde_json::to_string(&record).unwrap();
    for canary in [
        "sk-or-v1-PRIVATE-ERROR-CANARY",
        "/private/footage/canary.mp4",
        "prompt-body-canary",
    ] {
        assert!(!persisted.contains(canary), "leaked {canary}: {persisted}");
    }
    assert!(record.failure_code.contains("provider_error"));

    let run = catalog.analysis_runs(&project, 10).unwrap().remove(0);
    for canary in [
        "sk-or-v1-PRIVATE-ERROR-CANARY",
        "/private/footage/canary.mp4",
        "prompt-body-canary",
    ] {
        assert!(!run.error.contains(canary), "run error leaked {canary}");
    }

    // Reconcile the interrupted attempt before an explicitly different
    // force-policy can proceed; force cannot bypass unknown paid work.
    let interrupted = catalog
        .list_analysis_requests(&project, Some(&record.policy_id), 10)
        .unwrap()
        .remove(0);
    catalog
        .record_analysis_request_receipt(
            &interrupted.request_key,
            &ProviderReceiptEvidence {
                provider: "synthetic".into(),
                provider_generation_id: "diagnostic-late-generation".into(),
                actual_model: DEFAULT_MODEL.into(),
                reported_cost_usd: Some(0.01),
                prompt_tokens: Some(10),
                completion_tokens: Some(5),
                usage_complete: true,
                billed_audio_seconds: None,
                usage_pricing: None,
            },
        )
        .unwrap();
    let mut different_request = request.clone();
    different_request.parameters = RunPolicyParameters::Observation { force: true };
    catalog.authorize_run_policy(&different_request).unwrap();
    let body_provider = CapturingProvider::with_body(
        r#"{"title":"PRIVATE_RESPONSE_BODY_CANARY","privatePath":"/private/body/path"}"#,
    );
    let body_record = catalog
        .run_run_policy_pilot_with_provider(&different_request, &body_provider, false)
        .unwrap();
    assert_eq!(body_record.status, "failed");
    let persisted = serde_json::to_string(&body_record).unwrap();
    assert!(!persisted.contains("PRIVATE_RESPONSE_BODY_CANARY"));
    assert!(!persisted.contains("/private/body/path"));
}

#[test]
fn unknown_usage_retry_retains_prior_cost_and_stays_blocked() {
    let (_directory, catalog, project, asset) = fixture();
    let request = observation_request(&project, &asset, 1.0);
    catalog.authorize_run_policy(&request).unwrap();

    let first = CapturingProvider::with_incomplete_usage();
    let first_pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &first, false)
        .unwrap();
    assert_eq!(first_pilot.status, "uncertain");
    assert!(!first_pilot.usage_complete);
    assert_eq!(first_pilot.confirmed_cost_usd, 0.0);
    assert!(first_pilot.uncertain_usage_reserve_usd >= 0.01);
    assert!((first_pilot.cost_usd - first_pilot.uncertain_usage_reserve_usd).abs() < 1e-9);

    let second = CapturingProvider::observation();
    let error = catalog
        .run_run_policy_pilot_with_provider(&request, &second, true)
        .unwrap_err();
    assert!(format!("{error:#}").contains("reconcile"));
    assert_eq!(second.calls.load(Ordering::SeqCst), 0);
    let retained = catalog
        .load_run_pilot(&first_pilot.policy_id)
        .unwrap()
        .unwrap();
    assert_eq!(retained, first_pilot);
    assert!(
        catalog
            .authorized_policy_for_bulk(&retained.policy_id, AnalysisRunKind::Observation)
            .is_err()
    );
}

#[test]
fn cancellation_before_pilot_transmission_makes_zero_provider_calls() {
    let (_directory, catalog, project, asset) = fixture();
    let request = observation_request(&project, &asset, 1.0);
    catalog.authorize_run_policy(&request).unwrap();
    let provider = CapturingProvider::observation();
    let error = catalog
        .run_run_policy_pilot_with_provider_and_cancel(&request, &provider, false, || true)
        .unwrap_err();
    assert!(format!("{error:#}").contains("cancelled"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert!(
        catalog
            .load_run_pilot(&catalog.describe_run_policy(&request).unwrap().policy_id)
            .unwrap()
            .is_none(),
        "a cancelled pre-transmission pilot must not persist a paid record"
    );
}

#[test]
fn persisted_source_change_invalidates_authorization_and_pilot_after_restart() {
    let (directory, catalog, project, asset) = fixture();
    let request = observation_request(&project, &asset, 1.0);
    catalog.authorize_run_policy(&request).unwrap();
    let pilot_provider = CapturingProvider::observation();
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &pilot_provider, false)
        .unwrap();
    assert_eq!(pilot.status, "passed");
    let catalog_path = directory.path().join("catalog.sqlite3");
    drop(catalog);

    let reopened = Catalog::open(&catalog_path).unwrap();
    reopened
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET fingerprint='changed-after-pilot' WHERE id=?1",
            params![asset],
        )
        .unwrap();
    let error = reopened
        .authorized_policy_for_bulk(&pilot.policy_id, AnalysisRunKind::Observation)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("changed"),
        "unexpected invalidation error: {error:#}"
    );
    assert!(
        reopened.load_run_pilot(&pilot.policy_id).unwrap().is_some(),
        "the old observation must remain auditable even though it is invalid"
    );
}

#[test]
fn explicit_batch_invocation_without_policy_id_materializes_approval_and_pilot() {
    let (_directory, catalog, project, asset) = fixture();
    save_private_transcript(&catalog, &asset);
    catalog.add_label(&asset, "PRIVATE_VOCAB_CANARY").unwrap();
    let options = crate::AnalysisRunOptions {
        payload: Some(AnalysisPayload::Frames),
        force: true,
        budget_usd: Some(1.0),
        primary_model: Some(DEFAULT_MODEL.to_string()),
        allowed_models: vec![DEFAULT_MODEL.to_string()],
        allow_fallback: Some(false),
        include_project_vocabulary: false,
        ..crate::AnalysisRunOptions::default()
    };
    let provider = CapturingProvider::observation();
    let report = catalog
        .analyze_missing_with_options_and_provider(&project, &options, Some(&provider), |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(report.analyzed, 1);
    assert!(
        provider.calls.load(Ordering::SeqCst) >= 2,
        "bulk must not run without the pilot call"
    );
    let policy_rows: i64 = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM analysis_run_authorizations WHERE project_id=?1",
            params![project],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(policy_rows, 1, "explicit invocation must persist approval");
    let recorded_policy_runs: i64 = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM analysis_runs WHERE project_id=?1 AND policy_id<>''",
            params![project],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        recorded_policy_runs >= 2,
        "pilot and bulk runs must both record policy provenance (found {recorded_policy_runs})"
    );
    assert!(provider.requests.lock().unwrap().iter().all(|request| {
        !request.prompt.contains("PRIVATE_TRANSCRIPT_CANARY")
            && !request.prompt.contains("PRIVATE_VOCAB_CANARY")
    }));
    let _ = asset;
}

#[test]
fn frames_only_moment_pilot_and_bulk_never_transmit_transcript() {
    let (_directory, catalog, project, asset) = fixture();
    save_private_transcript(&catalog, &asset);
    let request = RunPolicyRequest::moment_discovery(
        project.clone(),
        vec![asset.clone()],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into()],
        false,
        AnalysisPayload::Frames,
        1.0,
        8,
        8,
        1,
        EditProfile::default(),
        false,
        false,
    );
    let described = catalog.describe_run_policy(&request).unwrap();
    assert!(described.scope[0].transcript_fingerprint.is_none());
    catalog.authorize_run_policy(&request).unwrap();
    let moment_body = serde_json::json!({
        "moments": [],
        "uncertain": true,
        "reason": "synthetic frames only"
    })
    .to_string();

    let pilot_provider = CapturingProvider::with_body(&moment_body);
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &pilot_provider, false)
        .unwrap();
    assert_eq!(pilot.status, "passed", "{pilot:?}");
    {
        let requests = pilot_provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(!requests[0].prompt.contains("PRIVATE_TRANSCRIPT_CANARY"));
    }

    let bulk_provider = CapturingProvider::with_body(&moment_body);
    let report = catalog
        .discover_authorized_moments_with_provider(&pilot.policy_id, &bulk_provider, |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(report.total_assets, 1);
    let requests = bulk_provider.requests.lock().unwrap();
    assert!(!requests.is_empty());
    assert!(
        requests
            .iter()
            .all(|request| !request.prompt.contains("PRIVATE_TRANSCRIPT_CANARY"))
    );
}

#[test]
fn explicit_moment_invocation_without_policy_id_materializes_approval_and_pilot() {
    let (_directory, catalog, project, asset) = fixture();
    let options = MomentRunOptions {
        model: DEFAULT_MODEL.into(),
        provider: "openrouter".into(),
        payload: MomentPayload::Frames,
        budget_usd: 1.0,
        coarse_frames: 8,
        refinement_frames: 8,
        max_candidates: 1,
        profile: EditProfile::default(),
        force: false,
        retry_uncertain: false,
        allowed_models: vec![DEFAULT_MODEL.into()],
        allow_fallback: false,
        policy_id: None,
    };
    let provider = CapturingProvider::with_body(
        &serde_json::json!({
            "moments": [],
            "uncertain": true,
            "reason": "synthetic frames only"
        })
        .to_string(),
    );
    let report = catalog
        .discover_moments_for_assets_with_provider(&project, &[asset], &options, &provider, |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(report.total_assets, 1);
    assert!(
        provider.calls.load(Ordering::SeqCst) >= 2,
        "moment bulk must not run without the pilot call"
    );
    let policy_rows: i64 = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM analysis_run_authorizations WHERE project_id=?1",
            params![project],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(policy_rows, 1);
    let recorded_policy_runs: i64 = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM analysis_runs WHERE project_id=?1 AND policy_id<>''",
            params![project],
            |row| row.get(0),
        )
        .unwrap();
    assert!(recorded_policy_runs >= 2, "found {recorded_policy_runs}");
}
#[test]
fn default_observation_and_moment_cancel_before_any_pilot_call() {
    let (_directory, catalog, project, asset) = fixture();
    let provider = CapturingProvider::observation();
    let options = crate::AnalysisRunOptions {
        budget_usd: Some(1.0),
        force: true,
        payload: Some(AnalysisPayload::Frames),
        ..Default::default()
    };
    let report = catalog
        .analyze_missing_with_options_and_provider(&project, &options, Some(&provider), |_| {
            std::ops::ControlFlow::Break(())
        })
        .unwrap();
    assert!(report.cancelled);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let options = MomentRunOptions {
        model: DEFAULT_MODEL.into(),
        provider: "openrouter".into(),
        payload: MomentPayload::Frames,
        budget_usd: 1.0,
        coarse_frames: 8,
        refinement_frames: 8,
        max_candidates: 1,
        profile: EditProfile::default(),
        force: false,
        retry_uncertain: false,
        allowed_models: vec![DEFAULT_MODEL.into()],
        allow_fallback: false,
        policy_id: None,
    };
    let report = catalog
        .discover_moments_for_assets_with_provider(&project, &[asset], &options, &provider, |_| {
            std::ops::ControlFlow::Break(())
        })
        .unwrap();
    assert!(report.cancelled);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let count: i64 = catalog
        .connection()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM analysis_run_pilots", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn changed_transcript_timing_after_admission_blocks_transmission() {
    let (_directory, catalog, project, asset) = fixture();
    save_private_transcript(&catalog, &asset);
    let request = observation_request(&project, &asset, 1.0);
    catalog.authorize_run_policy(&request).unwrap();
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &CapturingProvider::observation(), false)
        .unwrap();
    let provider = CapturingProvider::observation();
    let mut changed = false;
    let report = catalog
        .analyze_authorized_with_provider(&pilot.policy_id, &provider, |_| {
            if !changed {
                catalog
                    .connection()
                    .unwrap()
                    .execute(
                        "UPDATE transcript_segments SET end_ms=end_ms+10 WHERE asset_id=?1",
                        [&asset],
                    )
                    .unwrap();
                changed = true;
            }
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.analyzed, 0);
}

#[test]
fn admitted_vocabulary_is_frozen_while_analysis_adds_labels() {
    let (directory, catalog, project, first) = fixture();
    let second = generated_asset(&catalog, &project, directory.path(), "clip-two", 2);
    catalog.add_label(&first, "APPROVED_VOCABULARY").unwrap();
    let mut request = observation_request(&project, &first, 1.0);
    request.asset_ids.push(second);
    request.include_project_vocabulary = true;
    catalog.authorize_run_policy(&request).unwrap();
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &CapturingProvider::observation(), false)
        .unwrap();
    let mut provider = CapturingProvider::observation();
    let mut body: Value = serde_json::from_str(&provider.body).unwrap();
    body["labels"] = json!(["new_generated_label"]);
    provider.body = body.to_string();
    let report = catalog
        .analyze_authorized_with_provider_and_workers(&pilot.policy_id, &provider, Some(1), |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(report.analyzed, 2, "{:?}", report.failures);
    assert!(
        provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.prompt.contains("APPROVED_VOCABULARY")
                && !request.prompt.contains("new_generated_label"))
    );
}

#[test]
fn stored_schema_version_change_invalidates_authorization() {
    let (_directory, catalog, project, asset) = fixture();
    let request = observation_request(&project, &asset, 1.0);
    let authorization = catalog.authorize_run_policy(&request).unwrap();
    let policy_id = authorization.policy.policy_id;
    let policy_json: String = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT policy_json FROM analysis_run_authorizations WHERE policy_id=?1",
            params![policy_id],
            |row| row.get(0),
        )
        .unwrap();
    let original = format!(
        "\"outputSchemaVersion\":\"{}\"",
        crate::analysis::schema::CLIP_OBSERVATION_SCHEMA_VERSION
    );
    let tampered = format!(
        "\"outputSchemaVersion\":\"{}.changed\"",
        crate::analysis::schema::CLIP_OBSERVATION_SCHEMA_VERSION
    );
    assert!(policy_json.contains(&original));
    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE analysis_run_authorizations SET policy_json=?2 WHERE policy_id=?1",
            params![policy_id, policy_json.replace(&original, &tampered)],
        )
        .unwrap();
    let error = catalog.load_run_authorization(&policy_id).unwrap_err();
    assert!(
        format!("{error:#}").contains("contents no longer match"),
        "unexpected schema invalidation error: {error:#}"
    );
}

type ProviderScript = Box<dyn Fn(&str, usize) -> Result<VlmResponse> + Send + Sync>;

pub(super) struct ModelScriptProvider {
    pub(super) calls: AtomicUsize,
    bound_usd: f64,
    script: ProviderScript,
}

impl ModelScriptProvider {
    pub(super) fn new(
        bound_usd: f64,
        script: impl Fn(&str, usize) -> Result<VlmResponse> + Send + Sync + 'static,
    ) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            bound_usd,
            script: Box::new(script),
        }
    }
}

impl VlmProvider for ModelScriptProvider {
    fn analysis_cost_bound(
        &self,
        _model: &str,
        _request: &VlmRequest,
    ) -> Result<AnalysisRequestCostBound> {
        Ok(AnalysisRequestCostBound::declared_for_synthetic_transport(
            self.bound_usd,
        ))
    }

    fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        (self.script)(model, call)
    }
}

pub(super) fn scripted_response(model: &str, call: usize, body: String, cost: f64) -> VlmResponse {
    VlmResponse {
        model: model.into(),
        body,
        prompt_tokens: 10,
        completion_tokens: 5,
        cost_usd: cost,
        latency_ms: 1,
        receipt: VlmReceipt {
            actual_model: Some(model.into()),
            provider: Some("synthetic-script".into()),
            generation_id: Some(format!("script-generation-{call}-{model}")),
            usage_complete: true,
        },
    }
}

pub(super) fn valid_observation_body() -> String {
    serde_json::json!({
        "title": "Synthetic fallback pilot",
        "description": "A bounded synthetic observation.",
        "labels": [],
        "usable": true,
        "unusableReason": null,
        "bestMomentStartMs": null,
        "bestMomentEndMs": null,
        "thumbnailTimestampMs": null,
        "cameraMovement": "static",
        "ocrText": []
    })
    .to_string()
}

#[test]
fn passed_pilot_is_idempotent_without_another_paid_call_or_run() {
    let (_directory, catalog, project, asset) = fixture();
    let request = observation_request(&project, &asset, 1.0);
    catalog.authorize_run_policy(&request).unwrap();
    let first_provider = CapturingProvider::observation();
    let first = catalog
        .run_run_policy_pilot_with_provider(&request, &first_provider, false)
        .unwrap();
    assert_eq!(first.status, "passed");
    assert_eq!(first_provider.calls.load(Ordering::SeqCst), 1);

    let second_provider = CapturingProvider::observation();
    let second = catalog
        .run_run_policy_pilot_with_provider(&request, &second_provider, false)
        .unwrap();
    assert_eq!(second, first);
    assert_eq!(second_provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        catalog.analysis_runs(&project, 10).unwrap().len(),
        1,
        "a passed pilot must not open another paid run"
    );
    assert_eq!(
        catalog
            .list_analysis_requests(&project, Some(&first.policy_id), 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn domain_invalid_response_falls_back_through_a_second_accounted_request() {
    let (_directory, catalog, project, asset) = fixture();
    let request = RunPolicyRequest::observation(
        project.clone(),
        vec![asset],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into(), FALLBACK_MODEL.into()],
        true,
        1.0,
        false,
    );
    catalog.authorize_run_policy(&request).unwrap();
    let provider = ModelScriptProvider::new(0.02, |model, call| {
        let body = if model == DEFAULT_MODEL {
            r#"{"not":"a valid observation"}"#.to_string()
        } else {
            valid_observation_body()
        };
        Ok(scripted_response(model, call, body, 0.01))
    });
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &provider, false)
        .unwrap();
    assert_eq!(pilot.status, "passed", "{pilot:?}");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(pilot.actual_model.as_deref(), Some(FALLBACK_MODEL));
    assert_eq!(pilot.confirmed_cost_usd, 0.02);
    assert_eq!(pilot.uncertain_usage_reserve_usd, 0.0);
    let requests = catalog
        .list_analysis_requests(&project, Some(&pilot.policy_id), 10)
        .unwrap();
    assert_eq!(
        requests.len(),
        2,
        "primary and domain fallback are separate"
    );
    assert!(requests.iter().all(|request| {
        catalog
            .analysis_provider_receipts(&request.request_key)
            .unwrap()
            .len()
            == 1
    }));
}

#[test]
fn transport_fallback_incomplete_attempt_is_retained_until_late_receipt() {
    let (_directory, catalog, project, asset) = fixture();
    let request = RunPolicyRequest::observation(
        project.clone(),
        vec![asset],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into(), FALLBACK_MODEL.into()],
        true,
        1.0,
        false,
    );
    catalog.authorize_run_policy(&request).unwrap();
    let provider = ModelScriptProvider::new(0.02, |model, call| {
        if model == DEFAULT_MODEL {
            Err(ProviderFailure::transport().into())
        } else {
            Ok(scripted_response(
                model,
                call,
                valid_observation_body(),
                0.01,
            ))
        }
    });
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &provider, false)
        .unwrap();
    assert_eq!(pilot.status, "uncertain", "{pilot:?}");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(pilot.confirmed_cost_usd, 0.01);
    assert!(pilot.uncertain_usage_reserve_usd >= 0.02);
    let requests = catalog
        .list_analysis_requests(&project, Some(&pilot.policy_id), 10)
        .unwrap();
    assert_eq!(requests.len(), 2);
    let incomplete = requests
        .iter()
        .find(|request| request.lifecycle == crate::AnalysisRequestLifecycle::Uncertain)
        .unwrap();
    let complete = requests
        .iter()
        .find(|request| request.lifecycle == crate::AnalysisRequestLifecycle::Completed)
        .unwrap();

    catalog
        .record_analysis_request_receipt(
            &incomplete.request_key,
            &ProviderReceiptEvidence {
                provider: "synthetic-script".into(),
                provider_generation_id: "late-generation-primary".into(),
                actual_model: DEFAULT_MODEL.into(),
                reported_cost_usd: Some(0.01),
                prompt_tokens: Some(10),
                completion_tokens: Some(5),
                usage_complete: true,
                billed_audio_seconds: None,
                usage_pricing: None,
            },
        )
        .unwrap();
    let aggregate = catalog
        .analysis_accounting_aggregate_for_operation(&pilot.policy_id, "pilot", None)
        .unwrap();
    assert_eq!(aggregate.uncertain_request_count, 0);
    assert!((aggregate.confirmed_cost_usd - 0.02).abs() < 1e-12);
    assert!((aggregate.uncertain_usage_reserve_usd).abs() < 1e-12);
    assert!(aggregate.usage_complete);
    assert_eq!(
        catalog
            .analysis_provider_receipts(&complete.request_key)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        catalog
            .analysis_provider_receipts(&incomplete.request_key)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn targeted_retry_authorization_retains_prior_unknown_cost_and_executes_once() {
    let (_directory, catalog, project, asset) = fixture();
    let request = observation_request(&project, &asset, 1.0);
    catalog.authorize_run_policy(&request).unwrap();
    let first = CapturingProvider::with_incomplete_usage();
    let first_pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &first, false)
        .unwrap();
    assert_eq!(first_pilot.status, "uncertain");
    let prior = catalog
        .list_analysis_requests(&project, Some(&first_pilot.policy_id), 10)
        .unwrap()
        .remove(0);
    assert_eq!(prior.lifecycle, crate::AnalysisRequestLifecycle::Uncertain);

    // Generic `retry_uncertain` alone cannot mint a new paid attempt.
    let unauthorized = CapturingProvider::observation();
    let error = catalog
        .run_run_policy_pilot_with_provider(&request, &unauthorized, true)
        .unwrap_err();
    let failure = ProviderFailure::from_anyhow(&error);
    assert_eq!(failure.category, ProviderFailureCategory::LocalAdmission);
    assert!(failure.safe_detail().contains("explicit retry"));
    assert_eq!(unauthorized.calls.load(Ordering::SeqCst), 0);

    catalog
        .authorize_run_policy_pilot_retry(&first_pilot.policy_id, &prior.request_key)
        .unwrap();
    let second = CapturingProvider::observation();
    let second_pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &second, true)
        .unwrap();
    assert_eq!(second.calls.load(Ordering::SeqCst), 1);
    assert_eq!(second_pilot.status, "uncertain");
    assert!(!second_pilot.usage_complete);
    assert_eq!(second_pilot.confirmed_cost_usd, 0.01);
    assert!(second_pilot.uncertain_usage_reserve_usd >= 0.02);
    let requests = catalog
        .list_analysis_requests(&project, Some(&first_pilot.policy_id), 10)
        .unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().any(|request| {
        request.lifecycle == crate::AnalysisRequestLifecycle::Uncertain
            && request.attempt_epoch == 0
    }));
    assert!(requests.iter().any(|request| {
        request.lifecycle == crate::AnalysisRequestLifecycle::Completed
            && request.attempt_epoch == 1
    }));
}

#[test]
fn retry_history_counts_actual_ledger_attempts_and_budget_denial_adds_none() {
    let (_directory, catalog, project, asset) = fixture();
    let request = RunPolicyRequest::observation(
        project.clone(),
        vec![asset],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into()],
        false,
        0.05,
        false,
    );
    catalog.authorize_run_policy(&request).unwrap();

    let invalid = ModelScriptProvider::new(0.03, |model, call| {
        Ok(scripted_response(
            model,
            call,
            r#"{"not":"a valid observation"}"#.into(),
            0.01,
        ))
    });
    let first = catalog
        .run_run_policy_pilot_with_provider(&request, &invalid, false)
        .unwrap();
    assert_eq!(first.status, "failed");
    assert_eq!(first.attempts, 1);
    assert_eq!(first.prompt_tokens, 10);
    let first_request = catalog
        .list_analysis_requests(&project, Some(&first.policy_id), 10)
        .unwrap()
        .remove(0);
    catalog
        .authorize_run_policy_pilot_retry(&first.policy_id, &first_request.request_key)
        .unwrap();

    let interrupted = ModelScriptProvider::new(0.03, |_model, _call| {
        Err(ProviderFailure::transport().into())
    });
    let second = catalog
        .run_run_policy_pilot_with_provider(&request, &interrupted, true)
        .unwrap();
    assert_eq!(interrupted.calls.load(Ordering::SeqCst), 1);
    assert_eq!(second.status, "uncertain");
    assert_eq!(second.attempts, 2, "prior actual receipt must not be lost");
    assert_eq!(second.attempted_models.len(), 2);
    assert_eq!(second.prompt_tokens, 10, "all actual receipts stay visible");
    assert_eq!(second.completion_tokens, 5);
    let requests = catalog
        .list_analysis_requests(&project, Some(&second.policy_id), 10)
        .unwrap();
    assert_eq!(requests.len(), 2);
    let second_request = requests
        .iter()
        .find(|request| request.run_id == second.run_id)
        .unwrap();
    catalog
        .authorize_run_policy_pilot_retry(&second.policy_id, &second_request.request_key)
        .unwrap();

    let denied = CapturingProvider::observation();
    let error = catalog
        .run_run_policy_pilot_with_provider(&request, &denied, true)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("budget_exhausted"),
        "{error:#}"
    );
    assert_eq!(denied.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        catalog
            .list_analysis_requests(&project, Some(&second.policy_id), 10)
            .unwrap()
            .len(),
        2,
        "budget denial must not create a paid request row"
    );
    let preserved = catalog.load_run_pilot(&second.policy_id).unwrap().unwrap();
    assert_eq!(preserved.attempts, 2);
    assert_eq!(preserved.attempted_models.len(), 2);
    assert_eq!(preserved.prompt_tokens, 10);
    assert_eq!(preserved.completion_tokens, 5);
}

#[test]
fn invalid_no_fallback_pilot_records_schema_health_feedback() {
    let (_directory, catalog, project, asset) = fixture();
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
    let invalid = ModelScriptProvider::new(0.03, |model, call| {
        Ok(scripted_response(model, call, "{}".into(), 0.01))
    });
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &invalid, false)
        .unwrap();
    assert_eq!(pilot.status, "failed");
    assert_eq!(invalid.calls.load(Ordering::SeqCst), 1);
    let category: String = catalog.connection().unwrap().query_row(
        "SELECT last_failure_category FROM analysis_provider_health WHERE project_id=?1 AND model=?2",
        params![project, DEFAULT_MODEL],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(category, "schema");
    let failures: i64 = catalog.connection().unwrap().query_row(
        "SELECT consecutive_failures FROM analysis_provider_health WHERE project_id=?1 AND model=?2",
        params![project, DEFAULT_MODEL],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(
        failures, 1,
        "a no-fallback pilot must still record validation failure"
    );
}

#[test]
fn prepare_failure_before_dispatch_records_zero_unknown_pilot() {
    let (_directory, catalog, project, asset) = fixture();
    let request = observation_request(&project, &asset, 1.0);
    let policy = catalog.describe_run_policy(&request).unwrap();
    catalog.authorize_run_policy(&request).unwrap();
    let provider = FakeProvider {
        calls: AtomicUsize::new(0),
        body: String::new(),
        fail_transport: false,
        fail_prepare: true,
    };
    let error = catalog
        .run_run_policy_pilot_with_provider(&request, &provider, false)
        .unwrap_err();
    assert!(format!("{error:#}").contains("pre_upload"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let requests = catalog
        .list_analysis_requests(&project, Some(&policy.policy_id), 10)
        .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].lifecycle,
        crate::AnalysisRequestLifecycle::Released
    );
    assert!(requests[0].dispatch_at.is_none());
    let aggregate = catalog
        .analysis_accounting_aggregate_for_operation(&policy.policy_id, "pilot", None)
        .unwrap();
    assert_eq!(aggregate.confirmed_cost_usd, 0.0);
    assert_eq!(aggregate.uncertain_usage_reserve_usd, 0.0);
    assert!(aggregate.usage_complete);
    let pilot = catalog.load_run_pilot(&policy.policy_id).unwrap().unwrap();
    assert_eq!(pilot.status, "failed");
    assert!(pilot.usage_complete);
    assert_eq!(pilot.cost_usd, 0.0);
    assert_eq!(pilot.confirmed_cost_usd, 0.0);
    assert_eq!(pilot.uncertain_usage_reserve_usd, 0.0);
}

#[test]
fn denied_bulk_admission_does_not_cancel_an_already_reserved_request() {
    struct HeldProvider {
        inner: FakeProvider,
        preparations: AtomicUsize,
        denied: (Mutex<bool>, std::sync::Condvar),
    }
    impl VlmProvider for HeldProvider {
        fn prepare(&self) -> Result<()> {
            // The first preparation is the batch's credential preflight.
            // Hold the admitted worker after reservation, before dispatch,
            // until the coordinator has handled the losing admission.
            if self.preparations.fetch_add(1, Ordering::SeqCst) > 0 {
                let (lock, changed) = &self.denied;
                let (denied, _) = changed
                    .wait_timeout_while(
                        lock.lock().unwrap(),
                        std::time::Duration::from_secs(10),
                        |denied| !*denied,
                    )
                    .unwrap();
                ensure!(
                    *denied,
                    "another worker must report its denial before the admitted worker dispatches"
                );
            }
            Ok(())
        }
        fn analysis_cost_bound(
            &self,
            model: &str,
            request: &VlmRequest,
        ) -> Result<AnalysisRequestCostBound> {
            self.inner.analysis_cost_bound(model, request)
        }
        fn analyze(&self, model: &str, request: &VlmRequest) -> Result<VlmResponse> {
            self.inner.analyze(model, request)
        }
    }
    let (directory, catalog, project, first_asset) = fixture();
    let second = generated_asset(
        &catalog,
        &project,
        directory.path(),
        "denied-before-send",
        2,
    );
    let request = RunPolicyRequest::observation(
        project.clone(),
        vec![first_asset, second],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into()],
        false,
        0.03,
        false,
    );
    catalog.authorize_run_policy(&request).unwrap();
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &FakeProvider::observation(), false)
        .unwrap();
    assert_eq!(pilot.status, "passed");
    let provider = HeldProvider {
        inner: FakeProvider::observation(),
        preparations: AtomicUsize::new(0),
        denied: (Mutex::new(false), std::sync::Condvar::new()),
    };
    let report = catalog
        .analyze_authorized_with_provider_and_workers(
            &pilot.policy_id,
            &provider,
            Some(2),
            |progress| {
                if progress.asset_id.is_some() && progress.outcome.is_none() {
                    let (lock, changed) = &provider.denied;
                    *lock.lock().unwrap() = true;
                    changed.notify_all();
                }
                std::ops::ControlFlow::Continue(())
            },
        )
        .unwrap();
    assert!(report.budget_exhausted);
    assert!(!report.cancelled, "budget denial is not user cancellation");
    assert_eq!(
        report.analyzed, 1,
        "the reserved worker must finish: {report:?}"
    );
    assert_eq!(provider.inner.calls.load(Ordering::SeqCst), 1);
    let aggregate = catalog
        .analysis_accounting_aggregate(&pilot.policy_id, None)
        .unwrap();
    assert_eq!(aggregate.confirmed_cost_usd, 0.02);
    assert_eq!(aggregate.active_request_count, 0);
    assert_eq!(aggregate.uncertain_request_count, 0);
    assert_eq!(
        aggregate.committed_or_reserved_usd,
        aggregate.confirmed_cost_usd
    );
}

#[test]
fn concurrent_bulk_workers_cannot_oversubscribe_the_whole_policy_budget() {
    let (directory, catalog, project, first_asset) = fixture();
    let second_asset = generated_asset(&catalog, &project, directory.path(), "clip-race-second", 2);
    // Two assets cost $0.02 at the reference estimate. The pilot's returned
    // $0.01 plus one $0.02 full request bound exactly exhausts this ceiling,
    // so a second concurrent admission must fail atomically.
    let request = RunPolicyRequest::observation(
        project.clone(),
        vec![first_asset, second_asset],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into()],
        false,
        0.03,
        false,
    );
    catalog.authorize_run_policy(&request).unwrap();
    let pilot_provider = FakeProvider::observation();
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &pilot_provider, false)
        .unwrap();
    assert_eq!(pilot.status, "passed", "{pilot:?}");
    assert_eq!(pilot_provider.calls.load(Ordering::SeqCst), 1);

    let provider = FakeProvider::observation();
    let barrier = Barrier::new(2);
    let left = catalog.clone();
    let right = catalog.clone();
    let policy_id = pilot.policy_id.clone();
    let (left_report, right_report) = std::thread::scope(|scope| {
        let left = scope.spawn(|| {
            barrier.wait();
            left.analyze_authorized_with_provider_and_workers(
                &policy_id,
                &provider,
                Some(2),
                |_| std::ops::ControlFlow::Continue(()),
            )
        });
        let right = scope.spawn(|| {
            barrier.wait();
            right.analyze_authorized_with_provider_and_workers(
                &policy_id,
                &provider,
                Some(2),
                |_| std::ops::ControlFlow::Continue(()),
            )
        });
        (left.join().unwrap(), right.join().unwrap())
    });
    let reports = [left_report, right_report];
    assert!(
        reports.iter().any(|report| report.is_ok()),
        "at least one concurrent bulk attempt should be admitted: {reports:?}"
    );
    assert!(
        reports
            .iter()
            .flatten()
            .any(|report| report.budget_exhausted),
        "the remaining whole-policy budget cannot fit another full request bound: {reports:?}"
    );
    // Exactly one bulk call can fit after the pilot; local admission denial
    // must not consume a fallback or create an unpaid uncertainty row.
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "the canonical full-bound admission must prevent any oversubscription"
    );
    let aggregate = catalog
        .analysis_accounting_aggregate(&policy_id, None)
        .unwrap();
    assert!(aggregate.confirmed_cost_usd <= 0.03 + 1e-9);
    assert_eq!(aggregate.confirmed_cost_usd, 0.02);
    assert_eq!(aggregate.active_request_count, 0);
    assert_eq!(aggregate.uncertain_request_count, 0);
    assert_eq!(
        aggregate.committed_or_reserved_usd, aggregate.confirmed_cost_usd,
        "reservations must settle to confirmed receipts without a second balance"
    );
}

#[test]
fn cancelled_partial_bulk_work_keeps_receipts_and_restart_reuses_results() {
    let (directory, catalog, project, first_asset) = fixture();
    let second_asset = generated_asset(
        &catalog,
        &project,
        directory.path(),
        "clip-restart-second",
        2,
    );
    let request = RunPolicyRequest::observation(
        project.clone(),
        vec![first_asset, second_asset],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into()],
        false,
        1.0,
        false,
    );
    catalog.authorize_run_policy(&request).unwrap();
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &FakeProvider::observation(), false)
        .unwrap();
    assert_eq!(pilot.status, "passed", "{pilot:?}");

    let first_provider = FakeProvider::observation();
    let mut stopped = false;
    let first_report = catalog
        .analyze_authorized_with_provider_and_workers(
            &pilot.policy_id,
            &first_provider,
            Some(1),
            |progress| {
                if progress.outcome.is_some() && !stopped {
                    stopped = true;
                    return std::ops::ControlFlow::Break(());
                }
                std::ops::ControlFlow::Continue(())
            },
        )
        .unwrap();
    assert!(first_report.cancelled);
    assert_eq!(first_report.analyzed, 1);
    assert_eq!(first_provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(first_report.cost_usd, 0.01);

    let observation_requests = catalog
        .list_analysis_requests(&project, Some(&pilot.policy_id), 1000)
        .unwrap()
        .into_iter()
        .filter(|request| request.operation == "observation")
        .collect::<Vec<_>>();
    assert_eq!(observation_requests.len(), 1);
    assert_eq!(
        observation_requests[0].lifecycle,
        crate::AnalysisRequestLifecycle::Completed
    );
    assert_eq!(observation_requests[0].confirmed_cost_usd, 0.01);
    let first_run_aggregate = catalog
        .analysis_accounting_aggregate_for_run(
            &pilot.policy_id,
            "observation",
            &observation_requests[0].run_id,
        )
        .unwrap();
    assert_eq!(first_run_aggregate.confirmed_cost_usd, 0.01);
    assert!(!first_run_aggregate.has_unresolved_usage());

    let second_provider = FakeProvider::observation();
    let second_report = catalog
        .analyze_authorized_with_provider_and_workers(
            &pilot.policy_id,
            &second_provider,
            Some(1),
            |_| std::ops::ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(second_provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(second_report.analyzed, 1, "{:?}", second_report.failures);
    assert!(!second_report.cancelled);
    let observation_aggregate = catalog
        .analysis_accounting_aggregate_for_operation(&pilot.policy_id, "observation", None)
        .unwrap();
    assert_eq!(observation_aggregate.request_count, 2);
    assert_eq!(observation_aggregate.confirmed_cost_usd, 0.02);
    assert_eq!(observation_aggregate.uncertain_request_count, 0);
}

#[test]
fn unknown_bulk_usage_blocks_implicit_resend_and_is_not_treated_as_zero() {
    let (_directory, catalog, project, asset) = fixture();
    let request = observation_request(&project, &asset, 1.0);
    catalog.authorize_run_policy(&request).unwrap();
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &FakeProvider::observation(), false)
        .unwrap();
    assert_eq!(pilot.status, "passed", "{pilot:?}");

    let interrupted = FakeProvider {
        calls: AtomicUsize::new(0),
        body: String::new(),
        fail_transport: true,
        fail_prepare: false,
    };
    let report = catalog
        .analyze_authorized_with_provider_and_workers(
            &pilot.policy_id,
            &interrupted,
            Some(1),
            |_| std::ops::ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(interrupted.calls.load(Ordering::SeqCst), 1);
    assert!(!report.usage_complete);
    let observation_requests = catalog
        .list_analysis_requests(&project, Some(&pilot.policy_id), 1000)
        .unwrap()
        .into_iter()
        .filter(|request| request.operation == "observation")
        .collect::<Vec<_>>();
    assert_eq!(observation_requests.len(), 1);
    assert_eq!(
        observation_requests[0].lifecycle,
        crate::AnalysisRequestLifecycle::Uncertain
    );
    assert!(observation_requests[0].uncertain_usage_reserve_usd > 0.0);

    let replacement = FakeProvider::observation();
    let error = catalog
        .analyze_authorized_with_provider_and_workers(
            &pilot.policy_id,
            &replacement,
            Some(1),
            |_| std::ops::ControlFlow::Continue(()),
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("unknown or interrupted"),
        "{error:#}"
    );
    assert_eq!(
        replacement.calls.load(Ordering::SeqCst),
        0,
        "a potentially completed paid request must never be blindly resent"
    );
}

#[test]
fn authorized_moment_bulk_accounts_distinct_per_asset_and_span_requests() {
    let (directory, catalog, project, first_asset) = fixture();
    let second_asset = generated_asset(
        &catalog,
        &project,
        directory.path(),
        "clip-moment-ledger-second",
        2,
    );
    let request = RunPolicyRequest::moment_discovery(
        project.clone(),
        vec![first_asset.clone(), second_asset.clone()],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into()],
        false,
        AnalysisPayload::Frames,
        1.0,
        8,
        8,
        1,
        EditProfile::default(),
        false,
        false,
    );
    let policy = catalog.authorize_run_policy(&request).unwrap().policy;
    let pilot_provider = FakeProvider::moments(serde_json::json!({
        "moments": [],
        "uncertain": true,
        "reason": "synthetic pilot"
    }));
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &pilot_provider, false)
        .unwrap();
    assert_eq!(pilot.status, "passed", "{pilot:?}");

    let bulk_provider = FakeProvider::moments(serde_json::json!({
        "moments": [],
        "uncertain": true,
        "reason": "synthetic distinct span accounting"
    }));
    let report = catalog
        .discover_authorized_moments_with_provider(&policy.policy_id, &bulk_provider, |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(report.processed, 2, "{:?}", report.failures);
    assert!(bulk_provider.calls.load(Ordering::SeqCst) >= 2);

    let requests = catalog
        .list_analysis_requests(&project, Some(&policy.policy_id), 1000)
        .unwrap()
        .into_iter()
        .filter(|request| {
            request.operation == "moment_discovery" && request.run_id == report.run_id
        })
        .collect::<Vec<_>>();
    assert!(
        requests.len() >= 2,
        "expected per-span requests: {requests:?}"
    );
    let mut keys = requests
        .iter()
        .map(|request| request.attempt_base_key.clone())
        .collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    assert_eq!(
        keys.len(),
        requests.len(),
        "each asset/span attempt must have its own identity"
    );
    let authorized_sources = policy
        .scope
        .iter()
        .map(|scope| scope.source_identity.clone())
        .collect::<Vec<_>>();
    let mut observed_sources = requests
        .iter()
        .map(|request| request.source_fingerprint.clone())
        .collect::<Vec<_>>();
    observed_sources.sort();
    observed_sources.dedup();
    assert_eq!(
        observed_sources.len(),
        2,
        "both authorized asset source identities must be represented"
    );
    assert!(
        observed_sources
            .iter()
            .all(|source| authorized_sources.contains(source))
    );
    assert!(requests.iter().all(|request| {
        request.lifecycle == crate::AnalysisRequestLifecycle::Completed
            && request.dispatch_at.is_some()
            && request.confirmed_cost_usd == 0.01
    }));
    let aggregate = catalog
        .analysis_accounting_aggregate_for_run(
            &policy.policy_id,
            "moment_discovery",
            &report.run_id,
        )
        .unwrap();
    assert_eq!(aggregate.request_count, requests.len());
    assert_eq!(aggregate.confirmed_cost_usd, requests.len() as f64 * 0.01);
    assert_eq!(report.cost_usd, aggregate.confirmed_cost_usd);
    assert!(!aggregate.has_unresolved_usage());
}

#[test]
fn bulk_domain_fallback_records_two_complete_ledger_receipts() {
    let (_directory, catalog, project, asset) = fixture();
    let request = RunPolicyRequest::observation(
        project.clone(),
        vec![asset],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into(), FALLBACK_MODEL.into()],
        true,
        1.0,
        false,
    );
    catalog.authorize_run_policy(&request).unwrap();
    let provider = ModelScriptProvider::new(0.02, |model, call| {
        let body = if model == DEFAULT_MODEL {
            r#"{"not":"a valid bulk observation"}"#.to_string()
        } else {
            valid_observation_body()
        };
        Ok(scripted_response(model, call, body, 0.01))
    });
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &provider, false)
        .unwrap();
    assert_eq!(pilot.status, "passed", "{pilot:?}");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);

    let report = catalog
        .analyze_authorized_with_provider(&pilot.policy_id, &provider, |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    assert_eq!(report.fallback_served, 1);
    assert_eq!(report.cost_usd, 0.02);
    let requests = catalog
        .list_analysis_requests(&project, Some(&pilot.policy_id), 1000)
        .unwrap()
        .into_iter()
        .filter(|request| request.operation == "observation")
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| {
        request.lifecycle == crate::AnalysisRequestLifecycle::Completed
            && request.confirmed_cost_usd == 0.01
            && catalog
                .analysis_provider_receipts(&request.request_key)
                .unwrap()
                .len()
                == 1
    }));
    let aggregate = catalog
        .analysis_accounting_aggregate_for_run(&pilot.policy_id, "observation", &requests[0].run_id)
        .unwrap();
    assert_eq!(aggregate.confirmed_cost_usd, 0.02);
    assert!(!aggregate.has_unresolved_usage());
}

#[test]
fn bulk_transport_fallback_retains_primary_uncertainty_and_fallback_receipt() {
    let (_directory, catalog, project, asset) = fixture();
    let request = RunPolicyRequest::observation(
        project.clone(),
        vec![asset],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into(), FALLBACK_MODEL.into()],
        true,
        1.0,
        false,
    );
    catalog.authorize_run_policy(&request).unwrap();
    let provider = ModelScriptProvider::new(0.02, |model, call| {
        if call == 1 || model == FALLBACK_MODEL {
            Ok(scripted_response(
                model,
                call,
                valid_observation_body(),
                0.01,
            ))
        } else {
            Err(ProviderFailure::transport().into())
        }
    });
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &provider, false)
        .unwrap();
    assert_eq!(pilot.status, "passed", "{pilot:?}");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

    let report = catalog
        .analyze_authorized_with_provider(&pilot.policy_id, &provider, |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    assert!(!report.usage_complete);
    assert_eq!(report.cost_usd, 0.01);
    let requests = catalog
        .list_analysis_requests(&project, Some(&pilot.policy_id), 1000)
        .unwrap()
        .into_iter()
        .filter(|request| request.operation == "observation")
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.lifecycle == crate::AnalysisRequestLifecycle::Uncertain)
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.lifecycle == crate::AnalysisRequestLifecycle::Completed)
            .count(),
        1
    );
    assert!(requests.iter().any(|request| {
        request.lifecycle == crate::AnalysisRequestLifecycle::Uncertain
            && request.uncertain_usage_reserve_usd >= 0.02
    }));
    let aggregate = catalog
        .analysis_accounting_aggregate_for_run(&pilot.policy_id, "observation", &requests[0].run_id)
        .unwrap();
    assert_eq!(aggregate.confirmed_cost_usd, 0.01);
    assert_eq!(aggregate.uncertain_request_count, 1);
    assert!(aggregate.has_unresolved_usage());
}

#[test]
fn local_proxy_loss_after_admission_makes_zero_paid_calls_and_zero_reservations() {
    let (_directory, catalog, project, asset) = fixture();
    let request = observation_request(&project, &asset, 1.0);
    catalog.authorize_run_policy(&request).unwrap();
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &FakeProvider::observation(), false)
        .unwrap();
    assert_eq!(pilot.status, "passed", "{pilot:?}");
    let proxy_path = catalog
        .get_asset(&asset)
        .unwrap()
        .proxy_path
        .expect("synthetic asset has a proxy");
    let provider = PrepareBreaksProxy {
        calls: AtomicUsize::new(0),
        proxy_path: proxy_path.into(),
    };
    let report = catalog
        .analyze_authorized_with_provider(&pilot.policy_id, &provider, |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(report.analyzed, 0);
    assert_eq!(report.cost_usd, 0.0);
    assert_eq!(report.failures.len(), 1);
    assert!(
        catalog
            .list_analysis_requests(&project, Some(&pilot.policy_id), 1000)
            .unwrap()
            .iter()
            .all(|request| request.operation != "observation"),
        "a local pre-upload failure must not create a paid request row"
    );
    let analysis_run: (String, String) = catalog
        .connection()
        .unwrap()
        .query_row(
            "SELECT id,accounting_contract FROM analysis_runs
                 WHERE policy_id=?1 AND stage_reached='analysis'
                 ORDER BY started_at DESC,id DESC LIMIT 1",
            params![pilot.policy_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(analysis_run.1, crate::ANALYSIS_ACCOUNTING_SCHEMA_VERSION);
    let aggregate = catalog
        .analysis_accounting_aggregate_for_run(&pilot.policy_id, "observation", &analysis_run.0)
        .unwrap();
    assert_eq!(aggregate.request_count, 0);
    assert_eq!(aggregate.confirmed_cost_usd, 0.0);
    assert_eq!(aggregate.active_reserved_cost_usd, 0.0);
    assert!(!aggregate.has_unresolved_usage());
}

#[test]
fn force_policy_cannot_bypass_unresolved_prior_pilot_request() {
    let (_directory, catalog, project, asset) = fixture();
    let request = observation_request(&project, &asset, 1.0);
    catalog.authorize_run_policy(&request).unwrap();
    let first = CapturingProvider::with_incomplete_usage();
    let first_pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &first, false)
        .unwrap();
    assert_eq!(first_pilot.status, "uncertain");

    let mut forced = request.clone();
    forced.parameters = RunPolicyParameters::Observation { force: true };
    catalog.authorize_run_policy(&forced).unwrap();
    let replacement = CapturingProvider::observation();
    let error = catalog
        .run_run_policy_pilot_with_provider(&forced, &replacement, true)
        .unwrap_err();
    assert!(format!("{error:#}").contains("replacement policy"));
    assert_eq!(replacement.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn bulk_domain_fallback_local_denial_remains_typed_and_retains_primary_charge() {
    let (_directory, catalog, project, asset) = fixture();
    let request = RunPolicyRequest::observation(
        project.clone(),
        vec![asset],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.into(), FALLBACK_MODEL.into()],
        true,
        0.05,
        false,
    );
    catalog.authorize_run_policy(&request).unwrap();
    let pilot = catalog
        .run_run_policy_pilot_with_provider(&request, &FakeProvider::observation(), false)
        .unwrap();
    assert_eq!(pilot.status, "passed");
    let provider = ModelScriptProvider::new(0.04, |model, call| {
        Ok(scripted_response(model, call, "{}".into(), 0.01))
    });
    let report = catalog
        .analyze_authorized_with_provider(&pilot.policy_id, &provider, |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert!(report.budget_exhausted);
    assert_eq!(report.cost_usd, 0.01);
    assert!(report.usage_complete);
    let aggregate = catalog
        .analysis_accounting_aggregate(&pilot.policy_id, None)
        .unwrap();
    assert!((aggregate.confirmed_cost_usd - 0.02).abs() < 1e-12);
    assert_eq!(aggregate.uncertain_request_count, 0);
}

#[test]
fn moment_fallback_local_denial_or_cancel_cannot_erase_paid_primary_uncertainty() {
    for cancel in [false, true] {
        let (_directory, catalog, project, asset) = fixture();
        let request = RunPolicyRequest::moment_discovery(
            project.clone(),
            vec![asset.clone()],
            DEFAULT_MODEL,
            vec![DEFAULT_MODEL.into(), FALLBACK_MODEL.into()],
            true,
            AnalysisPayload::Frames,
            0.07,
            8,
            8,
            1,
            EditProfile::default(),
            false,
            false,
        );
        catalog.authorize_run_policy(&request).unwrap();
        let body = json!({"moments": [], "uncertain": true, "reason": "synthetic"}).to_string();
        let pilot = catalog
            .run_run_policy_pilot_with_provider(
                &request,
                &CapturingProvider::with_body(&body),
                false,
            )
            .unwrap();
        assert_eq!(pilot.status, "passed", "{pilot:?}");
        let provider =
            ModelScriptProvider::new(0.05, |_, _| Err(ProviderFailure::transport().into()));
        let report = catalog
            .discover_authorized_moments_with_provider(&pilot.policy_id, &provider, |_| {
                if cancel && provider.calls.load(Ordering::SeqCst) > 0 {
                    std::ops::ControlFlow::Break(())
                } else {
                    std::ops::ControlFlow::Continue(())
                }
            })
            .unwrap();
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "local fallback denial must not transmit"
        );
        assert_eq!(report.cancelled, cancel);
        assert!(!report.usage_complete);
        let aggregate = catalog
            .analysis_accounting_aggregate(&pilot.policy_id, None)
            .unwrap();
        assert_eq!(aggregate.uncertain_request_count, 1);
        assert!((aggregate.uncertain_usage_reserve_usd - 0.05).abs() < 1e-12);
        let states: Vec<String> = catalog
            .connection()
            .unwrap()
            .prepare(
                "SELECT json_extract(body_json,'$.body.state') FROM moment_steps WHERE asset_id=?1",
            )
            .unwrap()
            .query_map([&asset], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            states.iter().any(|state| state == "uncertain"),
            "{states:?}"
        );
        assert!(
            !states.iter().any(|state| state == "local_failure"),
            "{states:?}"
        );
    }
}
