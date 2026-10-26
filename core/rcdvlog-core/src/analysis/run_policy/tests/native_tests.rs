use super::*;
use crate::analysis::accounting::{
    AccountedNativeSttProvider, AnalysisAccountingContext, AnalysisBudgetAuthorityContext,
    AnalysisRequestLifecycle,
};
use crate::analysis::openrouter_stt::{
    NativeSttAudioWindow, NativeSttCallResult, NativeSttCost, NativeSttOutcome,
    NativeSttProviderPolicy, NativeSttReceipt, NativeSttRequest, NativeSttTransport,
    NativeSttUsage, OPENROUTER_STT_PROVIDER, OPENROUTER_WHISPER_MODEL, SttDataCollection,
    decode_native_stt_body,
};
use crate::analysis::windows::{SpeechRegion, pack_windows};
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

struct NeverVlmProvider {
    prepare_calls: AtomicUsize,
    bound_calls: AtomicUsize,
    analyze_calls: AtomicUsize,
}

impl NeverVlmProvider {
    fn new() -> Self {
        Self {
            prepare_calls: AtomicUsize::new(0),
            bound_calls: AtomicUsize::new(0),
            analyze_calls: AtomicUsize::new(0),
        }
    }
}

impl VlmProvider for NeverVlmProvider {
    fn prepare(&self) -> Result<()> {
        self.prepare_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn analysis_cost_bound(
        &self,
        _model: &str,
        _request: &VlmRequest,
    ) -> Result<AnalysisRequestCostBound> {
        self.bound_calls.fetch_add(1, Ordering::SeqCst);
        bail!("native proposal must not request a VLM cost bound")
    }

    fn analyze(&self, _model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
        self.analyze_calls.fetch_add(1, Ordering::SeqCst);
        bail!("native proposal must not call a VLM provider")
    }
}

fn native_request(project: &str, asset: &str, budget: f64) -> RunPolicyRequest {
    RunPolicyRequest::native_speech_transcription(
        project,
        vec![asset.to_string()],
        OPENROUTER_STT_PROVIDER,
        OPENROUTER_WHISPER_MODEL,
        budget,
        Some("en".into()),
        NativeSttProviderPolicy::new(true, SttDataCollection::Deny),
        false,
    )
}

fn expected_native_recipe(request: &RunPolicyRequest) -> String {
    let RunPolicyParameters::NativeSpeechTranscription {
        language,
        provider_policy,
        ..
    } = &request.parameters
    else {
        panic!("expected native parameters");
    };
    let native_request = NativeSttRequest::new(
        request.provider.clone(),
        request.primary_model.clone(),
        language.clone(),
        0.0,
        *provider_policy,
    )
    .unwrap();
    crate::analysis::store::native_stt_transcript_recipe(&native_request).unwrap()
}

fn blocked_native_recipe() -> String {
    format!("{}:unregistered", native_stt::NATIVE_STT_RECIPE_VERSION)
}

fn table_count(catalog: &Catalog, table: &str) -> i64 {
    catalog
        .connection()
        .unwrap()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[test]
fn native_speech_proposal_binds_registered_audio_only_scope_cost_and_exposes_working_route() {
    let (directory, catalog, project, asset) = fixture();
    catalog
        .add_label(&asset, "PRIVATE_NATIVE_VOCAB_CANARY")
        .unwrap();
    save_private_transcript(&catalog, &asset);
    let request = native_request(&project, &asset, 1.0);
    let policy = catalog.describe_run_policy(&request).unwrap();

    assert_eq!(policy.kind, AnalysisRunKind::NativeSpeechTranscription);
    assert_eq!(policy.kind.as_str(), "native_speech_transcription");
    assert_eq!(policy.provider, OPENROUTER_STT_PROVIDER);
    assert_eq!(policy.primary_model, OPENROUTER_WHISPER_MODEL);
    assert_eq!(policy.payload, AnalysisPayload::Audio);
    assert!(!policy.allow_fallback);
    assert!(policy.permitted_fallbacks.is_empty());
    assert_eq!(policy.allowed_models, vec![OPENROUTER_WHISPER_MODEL]);
    assert_eq!(policy.context, RunPolicyContext::default());
    assert_eq!(policy.scope.len(), 1);
    assert_eq!(policy.scope[0].asset_id, asset);
    assert_eq!(policy.scope[0].representation, framepack::REPRESENTATION);
    assert!(policy.scope[0].transcript_fingerprint.is_none());
    assert!(!policy.scope[0].source_identity.is_empty());
    assert_eq!(
        policy.provider_cost_bound_contract,
        native_stt_cost_bound_contract_id()
    );
    assert_ne!(
        policy.provider_cost_bound_contract,
        analysis_cost_bound_contract_id()
    );
    assert_eq!(policy.prompt_version, native_stt::NATIVE_STT_PROMPT_VERSION);
    assert_eq!(
        policy.output_schema_version,
        native_stt::NATIVE_STT_OUTPUT_SCHEMA_VERSION
    );
    assert_eq!(policy.recipe_version, expected_native_recipe(&request));
    assert!(policy.recipe_version.starts_with(
        crate::analysis::store::native_recipe::NATIVE_STT_TRANSCRIPT_RECIPE_SCHEMA_VERSION
    ));
    assert_ne!(policy.recipe_version, crate::analysis::STT_RECIPE_KEY);
    assert_eq!(policy.tool_version, RUN_POLICY_TOOL_VERSION);
    assert!(
        policy
            .capability_checks
            .iter()
            .any(
                |check| check.requirement == "native_stt.registered_protocol"
                    && check.status == CapabilityStatus::Passed
            )
    );
    assert!(policy.capability_checks.iter().any(|check| {
        check.requirement == "native_stt.accounted_runtime"
            && check.status == CapabilityStatus::Passed
    }));
    assert!(policy.capability_checks.iter().any(|check| {
        check.requirement == "native_stt.high_level_pilot"
            && check.status == CapabilityStatus::Passed
    }));
    assert!(
        !policy
            .readiness
            .blockers
            .iter()
            .any(|blocker| { blocker.code == "native_accounted_runtime_unavailable" })
    );
    assert!(policy.readiness.ready_for_authorization);
    // Paid native pilot readiness is transiently false in this fixture because
    // no VAD artifact or provider credential is present; the static high-level
    // pilot capability itself is implemented. The all-local no-provider outcome
    // remains runnable without those prerequisites.
    assert!(!policy.readiness.ready_for_pilot);
    assert!(policy.readiness.capabilities_ready);
    assert!(
        policy
            .readiness
            .warnings
            .iter()
            .any(|warning| warning
                .contains("native STT core bulk and CLI execution are implemented"))
    );
    assert!(
        policy
            .readiness
            .warnings
            .iter()
            .any(|warning| warning.contains("models install --backend native-openrouter"))
    );

    let spec = resolve_native_stt_model(OPENROUTER_STT_PROVIDER, OPENROUTER_WHISPER_MODEL).unwrap();
    let (expected, basis) = native_stt::expected_cost_usd(&policy.scope, spec).unwrap();
    assert!((policy.expected_cost_usd - expected).abs() < 1e-12);
    assert!(
        (policy.expected_cost_usd - 0.006).abs() < 1e-12,
        "two-second asset needs one conservative bulk window plus one separate pilot"
    );
    assert!(
        policy
            .cost_basis
            .contains("separate capability-pilot window")
    );
    assert_eq!(policy.cost_basis, basis);
    assert!(policy.cost_basis.contains("full 30000ms"));
    assert!(
        policy
            .cost_basis
            .contains("not a wire-enforced hard invoice cap")
    );
    assert!(policy.readiness.warnings.iter().any(|warning| {
        warning.contains("max_price, order, only and ignore")
            && warning.contains("not a hard invoice cap")
    }));

    let serialized = serde_json::to_string(&policy).unwrap();
    assert!(serialized.contains("\"kind\":\"native_speech_transcription\""));
    let serialized_value: Value = serde_json::from_str(&serialized).unwrap();
    assert_eq!(
        serialized_value["parameters"]["provider_policy"],
        json!({"zdr": true, "data_collection": "deny"})
    );
    let directory_text = directory.path().to_string_lossy().to_string();
    for canary in [
        "PRIVATE_NATIVE_VOCAB_CANARY",
        "PRIVATE_TRANSCRIPT_CANARY",
        directory_text.as_str(),
    ] {
        assert!(!serialized.contains(canary), "policy leaked {canary}");
    }
    let round_trip: RunPolicy = serde_json::from_str(&serialized).unwrap();
    assert_eq!(run_policy_identity(&round_trip).unwrap(), policy.policy_id);
    assert_eq!(
        catalog.describe_run_policy(&request).unwrap().policy_id,
        policy.policy_id
    );

    let run_rows = table_count(&catalog, "analysis_runs");
    let budget_rows = table_count(&catalog, "analysis_budget_authorities");
    let authorization = catalog.authorize_run_policy(&request).unwrap();
    assert_eq!(authorization.policy.policy_id, policy.policy_id);
    assert_eq!(
        table_count(&catalog, "analysis_run_authorizations"),
        1,
        "native policy authorization uses the real existing authorization owner"
    );
    assert_eq!(table_count(&catalog, "analysis_runs"), run_rows);
    assert_eq!(
        table_count(&catalog, "analysis_budget_authorities"),
        budget_rows
    );

    let provider = NeverVlmProvider::new();
    let pilot_error = catalog
        .run_run_policy_pilot_with_provider(&request, &provider, false)
        .unwrap_err();
    assert!(
        format!("{pilot_error:#}").contains("not a VLM operation"),
        "{pilot_error:#}"
    );
    assert_eq!(provider.prepare_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.bound_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.analyze_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        table_count(&catalog, "analysis_run_authorizations"),
        1,
        "rejected VLM pilot entry point must not change native authorization"
    );
    assert_eq!(table_count(&catalog, "analysis_runs"), run_rows);
    assert_eq!(
        table_count(&catalog, "analysis_budget_authorities"),
        budget_rows
    );
}

#[test]
fn native_speech_rejects_chat_models_routes_fallback_vocab_and_non_audio() {
    let (_directory, catalog, project, asset) = fixture();
    save_private_transcript(&catalog, &asset);
    catalog
        .add_label(&asset, "PRIVATE_NATIVE_VOCAB_CANARY")
        .unwrap();

    let mut chat_model = native_request(&project, &asset, 1.0);
    chat_model.primary_model = DEFAULT_MODEL.into();
    chat_model.allowed_models = vec![DEFAULT_MODEL.into()];
    let policy = catalog.describe_run_policy(&chat_model).unwrap();
    assert!(!policy.readiness.ready_for_authorization);
    assert!(policy.capability_checks.iter().any(|check| {
        check.requirement == "native_stt.registered_protocol"
            && check.status == CapabilityStatus::Failed
    }));
    assert!(
        policy
            .readiness
            .blockers
            .iter()
            .any(|blocker| blocker.code == "unsupported_native_stt_model")
    );
    assert_eq!(policy.recipe_version, blocked_native_recipe());
    assert!(!policy.recipe_version.starts_with(
        crate::analysis::store::native_recipe::NATIVE_STT_TRANSCRIPT_RECIPE_SCHEMA_VERSION
    ));

    let mut unqualified = native_request(&project, &asset, 1.0);
    unqualified.primary_model = "whisper-1".into();
    unqualified.allowed_models = vec!["whisper-1".into()];
    let unqualified_policy = catalog.describe_run_policy(&unqualified).unwrap();
    assert!(
        unqualified_policy
            .readiness
            .blockers
            .iter()
            .any(|blocker| blocker.code == "unsupported_native_stt_model")
    );
    assert_eq!(unqualified_policy.recipe_version, blocked_native_recipe());

    let mut invalid_language = native_request(&project, &asset, 1.0);
    if let RunPolicyParameters::NativeSpeechTranscription { language, .. } =
        &mut invalid_language.parameters
    {
        *language = Some("zz".into());
    }
    let invalid_language_policy = catalog.describe_run_policy(&invalid_language).unwrap();
    assert!(!invalid_language_policy.readiness.ready_for_authorization);
    assert!(
        invalid_language_policy
            .readiness
            .blockers
            .iter()
            .any(|blocker| blocker.code == "unsupported_language")
    );
    assert_eq!(
        invalid_language_policy.recipe_version,
        blocked_native_recipe()
    );

    let mut extra_allowlist = native_request(&project, &asset, 1.0);
    extra_allowlist.allow_fallback = true;
    extra_allowlist.allowed_models = vec![OPENROUTER_WHISPER_MODEL.into(), DEFAULT_MODEL.into()];
    let policy = catalog.describe_run_policy(&extra_allowlist).unwrap();
    assert_eq!(policy.payload, AnalysisPayload::Audio);
    assert!(!policy.allow_fallback);
    assert!(policy.permitted_fallbacks.is_empty());
    assert!(
        policy
            .readiness
            .blockers
            .iter()
            .any(|blocker| { blocker.code == "unsupported_model_allowlist" })
    );

    let mut vocabulary = native_request(&project, &asset, 1.0);
    vocabulary.include_project_vocabulary = true;
    let policy = catalog.describe_run_policy(&vocabulary).unwrap();
    assert_eq!(policy.context, RunPolicyContext::default());
    assert!(policy.scope[0].transcript_fingerprint.is_none());
    assert!(
        policy
            .readiness
            .blockers
            .iter()
            .any(|blocker| blocker.code == "unsupported_payload_context")
    );

    let mut wrong_payload = native_request(&project, &asset, 1.0);
    wrong_payload.payload = AnalysisPayload::FramesAndTranscript;
    let policy = catalog.describe_run_policy(&wrong_payload).unwrap();
    assert_eq!(policy.payload, AnalysisPayload::Audio);
    assert!(policy.scope[0].transcript_fingerprint.is_none());
    assert!(
        policy
            .readiness
            .blockers
            .iter()
            .any(|blocker| blocker.code == "unsupported_payload")
    );

    assert!(
        serde_json::from_value::<RunPolicyParameters>(json!({
            "kind": "native_speech_transcription",
            "language": "en",
            "provider_policy": {"zdr": true, "data_collection": "deny"},
            "force": false,
            "max_price": {"request": 0.1}
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<RunPolicyParameters>(json!({
            "kind": "native_speech_transcription",
            "language": "en",
            "provider_policy": {"order": ["openai"]},
            "force": false
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<RunPolicyParameters>(json!({
            "kind": "native_speech_transcription",
            "language": "en",
            "provider_policy": {},
            "force": false
        }))
        .is_err()
    );
    let mut request_json = serde_json::to_value(native_request(&project, &asset, 1.0)).unwrap();
    request_json
        .as_object_mut()
        .unwrap()
        .insert("maxPrice".into(), json!(0.1));
    assert!(serde_json::from_value::<RunPolicyRequest>(request_json).is_err());
}

#[test]
fn native_speech_identity_binds_scope_language_policy_budget_and_source_not_transcript_or_vocab() {
    let (directory, catalog, project, asset) = fixture();
    let request = native_request(&project, &asset, 1.0);
    let baseline = catalog.describe_run_policy(&request).unwrap();
    assert_eq!(baseline.recipe_version, expected_native_recipe(&request));

    let mut changed_language = request.clone();
    changed_language.parameters = RunPolicyParameters::NativeSpeechTranscription {
        language: Some("ja".into()),
        provider_policy: NativeSttProviderPolicy::new(true, SttDataCollection::Deny),
        force: false,
    };
    let changed_language_policy = catalog.describe_run_policy(&changed_language).unwrap();
    assert_ne!(changed_language_policy.policy_id, baseline.policy_id);
    assert_ne!(
        changed_language_policy.recipe_version,
        baseline.recipe_version
    );

    let mut changed_data_policy = request.clone();
    changed_data_policy.parameters = RunPolicyParameters::NativeSpeechTranscription {
        language: Some("en".into()),
        provider_policy: NativeSttProviderPolicy::new(true, SttDataCollection::Allow),
        force: false,
    };
    let changed_data_policy = catalog.describe_run_policy(&changed_data_policy).unwrap();
    assert_ne!(changed_data_policy.policy_id, baseline.policy_id);
    assert_ne!(changed_data_policy.recipe_version, baseline.recipe_version);

    let mut changed_budget = request.clone();
    changed_budget.budget_usd = 2.0;
    let changed_budget_policy = catalog.describe_run_policy(&changed_budget).unwrap();
    assert_ne!(changed_budget_policy.policy_id, baseline.policy_id);
    assert_eq!(
        changed_budget_policy.recipe_version, baseline.recipe_version,
        "budget changes must not alter the transcript recipe identity"
    );

    let second = generated_asset(&catalog, &project, directory.path(), "clip2", 2);
    let mut changed_scope = request.clone();
    changed_scope.asset_ids.push(second);
    let changed_scope_policy = catalog.describe_run_policy(&changed_scope).unwrap();
    assert_ne!(changed_scope_policy.policy_id, baseline.policy_id);
    assert_eq!(
        changed_scope_policy.recipe_version, baseline.recipe_version,
        "asset scope changes must not alter the transcript recipe identity"
    );

    let mut changed_rate_contract = baseline.clone();
    changed_rate_contract
        .provider_cost_bound_contract
        .push_str("-changed");
    assert_ne!(
        run_policy_identity(&changed_rate_contract).unwrap(),
        baseline.policy_id
    );

    save_private_transcript(&catalog, &asset);
    catalog
        .add_label(&asset, "IRRELEVANT_NATIVE_VOCAB")
        .unwrap();
    let unchanged = catalog.describe_run_policy(&request).unwrap();
    assert_eq!(unchanged.policy_id, baseline.policy_id);
    assert_eq!(unchanged.recipe_version, baseline.recipe_version);
    assert!(unchanged.scope[0].transcript_fingerprint.is_none());
    assert_eq!(unchanged.context, RunPolicyContext::default());

    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET duration_seconds=3.0,source_out=3.0 WHERE id=?1",
            [&asset],
        )
        .unwrap();
    let changed_source = catalog.describe_run_policy(&request).unwrap();
    assert_ne!(changed_source.policy_id, baseline.policy_id);
    assert_ne!(
        changed_source.scope[0].source_identity,
        baseline.scope[0].source_identity
    );
}

#[test]
fn native_speech_scope_ownership_duplicates_excluded_offline_original_and_stale_proxy_fail_closed()
{
    let (directory, catalog, project, asset) = fixture();
    let other = catalog.create_project("Other project").unwrap();
    let mut duplicate = native_request(&project, &asset, 1.0);
    duplicate.asset_ids.push(asset.clone());
    assert!(catalog.describe_run_policy(&duplicate).is_err());
    duplicate.asset_ids = vec![asset.clone()];
    duplicate.project_id = other.id.clone();
    assert!(catalog.describe_run_policy(&duplicate).is_err());
    duplicate.project_id = project.clone();
    duplicate.asset_ids = vec!["missing".into()];
    assert!(catalog.describe_run_policy(&duplicate).is_err());

    catalog
        .connection()
        .unwrap()
        .execute("UPDATE assets SET removed=1 WHERE id=?1", [&asset])
        .unwrap();
    assert!(
        catalog
            .describe_run_policy(&native_request(&project, &asset, 1.0))
            .is_err()
    );
    catalog
        .connection()
        .unwrap()
        .execute("UPDATE assets SET removed=0 WHERE id=?1", [&asset])
        .unwrap();

    let original = catalog.get_asset(&asset).unwrap().path;
    fs::remove_file(&original).unwrap();
    let policy = catalog
        .describe_run_policy(&native_request(&project, &asset, 1.0))
        .unwrap();
    assert!(
        !policy
            .readiness
            .blockers
            .iter()
            .any(|blocker| blocker.code == "source_not_ready"),
        "offline original with a usable derived proxy must remain policy-ready: {:#?}",
        policy.readiness
    );
    assert!(policy.scope[0].representation_fingerprint.is_some());

    let proxy = catalog.get_asset(&asset).unwrap().proxy_path.unwrap();
    fs::remove_file(&proxy).unwrap();
    let stale = catalog
        .describe_run_policy(&native_request(&project, &asset, 1.0))
        .unwrap();
    assert!(
        stale
            .readiness
            .blockers
            .iter()
            .any(|blocker| blocker.code == "source_not_ready")
    );
    assert!(stale.scope[0].representation_fingerprint.is_none());
    assert!(catalog.validate_run_policy_asset(&policy, &asset).is_err());
    let _ = directory;
}

#[test]
fn native_speech_budget_and_source_duration_boundaries_fail_closed() {
    let (_directory, catalog, project, asset) = fixture();
    for invalid_budget in [f64::NAN, f64::INFINITY, -1.0, 0.0] {
        let error = catalog
            .describe_run_policy(&native_request(&project, &asset, invalid_budget))
            .unwrap_err();
        assert!(format!("{error:#}").contains("budget"), "{error:#}");
    }
    let too_small = catalog
        .describe_run_policy(&native_request(&project, &asset, 0.0001))
        .unwrap();
    assert!(!too_small.readiness.budget_ready);
    assert!(
        too_small
            .readiness
            .blockers
            .iter()
            .any(|blocker| blocker.code == "expected_cost_exceeds_budget")
    );

    catalog
        .connection()
        .unwrap()
        .execute("UPDATE assets SET duration_seconds=0 WHERE id=?1", [&asset])
        .unwrap();
    let zero_duration = catalog
        .describe_run_policy(&native_request(&project, &asset, 1.0))
        .unwrap();
    assert!(!zero_duration.readiness.ready_for_authorization);
    assert!(
        zero_duration
            .readiness
            .blockers
            .iter()
            .any(|blocker| blocker.code == "source_not_ready")
    );
    assert_eq!(zero_duration.scope[0].source_duration_ms, 0);
    assert_eq!(
        native_stt::max_packed_windows_upper_bound(zero_duration.scope[0].source_duration_ms),
        0
    );

    catalog
        .connection()
        .unwrap()
        .execute(
            "UPDATE assets SET duration_seconds=-1 WHERE id=?1",
            [&asset],
        )
        .unwrap();
    let negative_duration = catalog
        .describe_run_policy(&native_request(&project, &asset, 1.0))
        .unwrap();
    assert!(!negative_duration.readiness.ready_for_authorization);
    assert!(
        negative_duration
            .readiness
            .blockers
            .iter()
            .any(|blocker| blocker.code == "source_not_ready")
    );
}

#[derive(Clone)]
enum FakeNativeResponse {
    Ok(Box<NativeSttCallResult>),
    TransportError,
}

struct CountingNativeTransport<'a> {
    catalog: &'a Catalog,
    run_id: String,
    prepare_calls: AtomicUsize,
    transcribe_calls: AtomicUsize,
    dispatched_seen_before_call: AtomicUsize,
    prepared: AtomicBool,
    fail_prepare: bool,
    mutate_asset_on_prepare: Option<String>,
    response: Mutex<FakeNativeResponse>,
}

impl<'a> CountingNativeTransport<'a> {
    fn new(
        catalog: &'a Catalog,
        run_id: &str,
        response: FakeNativeResponse,
    ) -> CountingNativeTransport<'a> {
        Self {
            catalog,
            run_id: run_id.to_string(),
            prepare_calls: AtomicUsize::new(0),
            transcribe_calls: AtomicUsize::new(0),
            dispatched_seen_before_call: AtomicUsize::new(0),
            prepared: AtomicBool::new(false),
            fail_prepare: false,
            mutate_asset_on_prepare: None,
            response: Mutex::new(response),
        }
    }

    fn failing_prepare(mut self) -> Self {
        self.fail_prepare = true;
        self
    }

    fn mutating_asset_on_prepare(mut self, asset_id: &str) -> Self {
        self.mutate_asset_on_prepare = Some(asset_id.to_string());
        self
    }

    fn prepare_seen(&self) -> bool {
        self.prepared.load(Ordering::SeqCst)
    }
}

impl NativeSttTransport for CountingNativeTransport<'_> {
    fn prepare(&self) -> Result<()> {
        self.prepare_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_prepare {
            bail!("synthetic native credential/configuration failure");
        }
        if let Some(asset_id) = &self.mutate_asset_on_prepare {
            self.catalog
                .connection()
                .unwrap()
                .execute(
                    "UPDATE assets SET duration_seconds=duration_seconds+1 WHERE id=?1",
                    [asset_id],
                )
                .unwrap();
        }
        self.prepared.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn transcribe(
        &self,
        _request: &NativeSttRequest,
        _audio: &NativeSttAudioWindow,
    ) -> Result<NativeSttCallResult> {
        self.transcribe_calls.fetch_add(1, Ordering::SeqCst);
        let lifecycle: String = self
            .catalog
            .connection()
            .unwrap()
            .query_row(
                "SELECT lifecycle FROM analysis_request_reservations WHERE run_id=?1 ORDER BY created_at DESC LIMIT 1",
                [&self.run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            lifecycle, "dispatched",
            "the durable dispatch marker must exist before the one transport call"
        );
        self.dispatched_seen_before_call
            .fetch_add(1, Ordering::SeqCst);
        match &*self.response.lock().unwrap() {
            FakeNativeResponse::Ok(result) => Ok((**result).clone()),
            FakeNativeResponse::TransportError => bail!("synthetic native transport interrupted"),
        }
    }
}

fn native_audio_window() -> NativeSttAudioWindow {
    let pcm = vec![0.1_f32; 16_000];
    let window = pack_windows(&[SpeechRegion {
        start_ms: 0,
        end_ms: 1_000,
    }])
    .remove(0);
    NativeSttAudioWindow::from_pcm(&pcm, &window).unwrap()
}

fn native_policy_request() -> NativeSttRequest {
    NativeSttRequest::new(
        OPENROUTER_STT_PROVIDER,
        OPENROUTER_WHISPER_MODEL,
        Some("en".into()),
        0.0,
        NativeSttProviderPolicy::new(true, SttDataCollection::Deny),
    )
    .unwrap()
}

fn valid_native_call(audio: &NativeSttAudioWindow, include_cost: bool) -> NativeSttCallResult {
    let mut payload = json!({
        "text": "Synthetic native transcript",
        "language": "english",
        "duration": 1.0,
        "segments": [{
            "id": 0,
            "start": 0.0,
            "end": 1.0,
            "text": "Synthetic native transcript"
        }],
        "model": OPENROUTER_WHISPER_MODEL,
        "provider": "openai"
    });
    if include_cost {
        payload["usage"] = json!({
            "cost": 0.002,
            "seconds": 1.0,
            "input_tokens": 12,
            "output_tokens": 4
        });
    }
    decode_native_stt_body(
        serde_json::to_vec(&payload).unwrap().as_slice(),
        audio.duration_ms(),
        Some("native-generation-1".into()),
        47,
    )
}

fn authorized_native_setup() -> (
    tempfile::TempDir,
    Catalog,
    String,
    String,
    RunPolicy,
    NativeSttRequest,
    NativeSttAudioWindow,
    String,
    AnalysisAccountingContext,
) {
    let (directory, catalog, project, asset) = fixture();
    let policy_request = native_request(&project, &asset, 1.0);
    let policy = catalog.describe_run_policy(&policy_request).unwrap();
    assert!(policy.readiness.ready_for_authorization);
    assert!(!policy.readiness.ready_for_pilot);
    catalog.authorize_run_policy(&policy_request).unwrap();
    let authority = AnalysisBudgetAuthorityContext {
        project_id: policy.project_id.clone(),
        policy_id: policy.policy_id.clone(),
        policy_fingerprint: policy.policy_id.clone(),
        operation: "pilot".into(),
        provider: policy.provider.clone(),
        budget_scope_id: String::new(),
        budget_usd: policy.budget_usd,
    };
    catalog
        .initialize_analysis_accounting_authority(&authority)
        .unwrap();
    let run_id = catalog
        .begin_accounted_analysis_run(&policy.project_id, &policy.policy_id, "pilot")
        .unwrap();
    let scope = policy.scope.first().unwrap();
    let context = AnalysisAccountingContext {
        project_id: policy.project_id.clone(),
        policy_id: policy.policy_id.clone(),
        policy_fingerprint: policy.policy_id.clone(),
        run_id: run_id.clone(),
        operation: "pilot".into(),
        intended_attempt_key: format!("{}:{}", policy.kind.as_str(), scope.asset_id),
        source_fingerprint: scope.source_identity.clone(),
        provider: policy.provider.clone(),
        budget_scope_id: String::new(),
        budget_usd: policy.budget_usd,
        explicit_retry: false,
    };
    let request = native_policy_request();
    let audio = native_audio_window();
    (
        directory, catalog, project, asset, policy, request, audio, run_id, context,
    )
}

#[test]
fn native_accounted_pilot_marks_before_one_call_and_settles_known_cost() {
    let (_directory, catalog, _project, _asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let call = valid_native_call(&audio, true);
    let transport = CountingNativeTransport::new(
        &catalog,
        &run_id,
        FakeNativeResponse::Ok(Box::new(call.clone())),
    );
    let result =
        AccountedNativeSttProvider::new(&catalog, &policy, context, &request, &audio, &transport)
            .transmit()
            .unwrap();

    assert_eq!(transport.prepare_calls.load(Ordering::SeqCst), 1);
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        transport.dispatched_seen_before_call.load(Ordering::SeqCst),
        1
    );
    assert_eq!(result.latency_ms(), 47);
    assert!(matches!(result.outcome(), NativeSttOutcome::Usable(_)));

    let record = catalog
        .analysis_request(&result.request_key)
        .unwrap()
        .unwrap();
    assert_eq!(record.lifecycle, AnalysisRequestLifecycle::Completed);
    assert!((record.confirmed_cost_usd - 0.002).abs() < 1e-12);
    assert_eq!(record.actual_provider.as_deref(), Some("openai"));
    assert_eq!(
        record.provider_generation_id.as_deref(),
        Some("native-generation-1")
    );
    let receipts = catalog
        .analysis_provider_receipts(&result.request_key)
        .unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].prompt_tokens, Some(12));
    assert_eq!(receipts[0].completion_tokens, Some(4));
    let events = catalog
        .analysis_request_events(&result.request_key)
        .unwrap();
    let receipt_event = events
        .iter()
        .find(|event| event.event_kind == "receipt")
        .unwrap();
    assert_eq!(
        receipt_event.detail["billedAudioSeconds"].as_f64(),
        Some(1.0)
    );
    assert_eq!(
        receipt_event.detail["reportedCostUsd"].as_f64(),
        Some(0.002)
    );
}

#[test]
fn native_unusable_payload_retains_returned_charge_and_typed_outcome() {
    let (_directory, catalog, _project, _asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let body = serde_json::to_vec(&json!({
        "usage": {"cost": 0.002, "seconds": 1.0},
        "model": OPENROUTER_WHISPER_MODEL,
        "provider": "openai"
    }))
    .unwrap();
    let call = decode_native_stt_body(
        &body,
        audio.duration_ms(),
        Some("native-generation-unusable".into()),
        13,
    );
    assert!(matches!(call.outcome, NativeSttOutcome::Unusable(_)));
    let transport =
        CountingNativeTransport::new(&catalog, &run_id, FakeNativeResponse::Ok(Box::new(call)));
    let result =
        AccountedNativeSttProvider::new(&catalog, &policy, context, &request, &audio, &transport)
            .transmit()
            .unwrap();
    assert!(matches!(result.outcome(), NativeSttOutcome::Unusable(_)));
    let record = catalog
        .analysis_request(&result.request_key)
        .unwrap()
        .unwrap();
    assert_eq!(record.lifecycle, AnalysisRequestLifecycle::Completed);
    assert!((record.confirmed_cost_usd - 0.002).abs() < 1e-12);
}

#[test]
fn native_missing_cost_is_uncertain_and_retains_the_reservation() {
    let (_directory, catalog, _project, _asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let call = valid_native_call(&audio, false);
    let transport =
        CountingNativeTransport::new(&catalog, &run_id, FakeNativeResponse::Ok(Box::new(call)));
    let result =
        AccountedNativeSttProvider::new(&catalog, &policy, context, &request, &audio, &transport)
            .transmit()
            .unwrap();
    let record = catalog
        .analysis_request(&result.request_key)
        .unwrap()
        .unwrap();
    assert_eq!(record.lifecycle, AnalysisRequestLifecycle::Uncertain);
    assert_eq!(record.reported_cost_usd, None);
    assert!(record.uncertain_usage_reserve_usd >= record.reserved_cost_usd);
    assert_eq!(
        record.provider_generation_id.as_deref(),
        Some("native-generation-1")
    );
}

#[test]
fn native_prepare_failure_releases_without_transport() {
    let (_directory, catalog, _project, _asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let call = valid_native_call(&audio, false);
    let transport =
        CountingNativeTransport::new(&catalog, &run_id, FakeNativeResponse::Ok(Box::new(call)))
            .failing_prepare();
    let error =
        AccountedNativeSttProvider::new(&catalog, &policy, context, &request, &audio, &transport)
            .transmit()
            .unwrap_err();
    assert!(format!("{error:#}").contains("pre_upload_failed"));
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 0);
    let released = catalog
        .list_analysis_requests(&policy.project_id, Some(&policy.policy_id), 10)
        .unwrap();
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].lifecycle, AnalysisRequestLifecycle::Released);
    assert!(released[0].dispatch_at.is_none());
}

#[test]
fn native_transport_error_after_marker_is_uncertain_not_released() {
    let (_directory, catalog, _project, _asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let transport =
        CountingNativeTransport::new(&catalog, &run_id, FakeNativeResponse::TransportError);
    let error =
        AccountedNativeSttProvider::new(&catalog, &policy, context, &request, &audio, &transport)
            .transmit()
            .unwrap_err();
    assert!(format!("{error:#}").contains("transport interrupted"));
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 1);
    let records = catalog
        .list_analysis_requests(&policy.project_id, Some(&policy.policy_id), 10)
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].lifecycle, AnalysisRequestLifecycle::Uncertain);
    assert!(records[0].dispatch_at.is_some());
}

#[test]
fn native_adapter_rejects_wrong_operation_and_request_binding_before_provider() {
    let (_directory, catalog, _project, _asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let call = valid_native_call(&audio, true);
    let transport =
        CountingNativeTransport::new(&catalog, &run_id, FakeNativeResponse::Ok(Box::new(call)));

    let mut wrong_operation = context.clone();
    wrong_operation.operation = "native_speech_transcription".into();
    let error = AccountedNativeSttProvider::new(
        &catalog,
        &policy,
        wrong_operation,
        &request,
        &audio,
        &transport,
    )
    .transmit()
    .unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("capability pilot") || message.contains("exact native operation"),
        "{message}"
    );

    let wrong_language = NativeSttRequest::new(
        OPENROUTER_STT_PROVIDER,
        OPENROUTER_WHISPER_MODEL,
        Some("ja".into()),
        0.0,
        NativeSttProviderPolicy::new(true, SttDataCollection::Deny),
    )
    .unwrap();
    let error = AccountedNativeSttProvider::new(
        &catalog,
        &policy,
        context.clone(),
        &wrong_language,
        &audio,
        &transport,
    )
    .transmit()
    .unwrap_err();
    assert!(format!("{error:#}").contains("language/data-policy/temperature"));

    let wrong_data_policy = NativeSttRequest::new(
        OPENROUTER_STT_PROVIDER,
        OPENROUTER_WHISPER_MODEL,
        Some("en".into()),
        0.0,
        NativeSttProviderPolicy::new(true, SttDataCollection::Allow),
    )
    .unwrap();
    let error = AccountedNativeSttProvider::new(
        &catalog,
        &policy,
        context,
        &wrong_data_policy,
        &audio,
        &transport,
    )
    .transmit()
    .unwrap_err();
    assert!(format!("{error:#}").contains("language/data-policy/temperature"));

    assert_eq!(transport.prepare_calls.load(Ordering::SeqCst), 0);
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 0);
    assert!(
        catalog
            .list_analysis_requests(&policy.project_id, Some(&policy.policy_id), 10)
            .unwrap()
            .is_empty(),
        "validation failures must occur before admission"
    );
}

#[test]
fn native_cancel_after_prepare_releases_without_send() {
    let (_directory, catalog, _project, _asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let call = valid_native_call(&audio, true);
    let transport =
        CountingNativeTransport::new(&catalog, &run_id, FakeNativeResponse::Ok(Box::new(call)));
    let error =
        AccountedNativeSttProvider::new(&catalog, &policy, context, &request, &audio, &transport)
            .transmit_with_cancel(|| transport.prepare_seen())
            .unwrap_err();
    assert!(format!("{error:#}").contains("cancelled"));
    assert_eq!(transport.prepare_calls.load(Ordering::SeqCst), 1);
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 0);
    let records = catalog
        .list_analysis_requests(&policy.project_id, Some(&policy.policy_id), 10)
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].lifecycle, AnalysisRequestLifecycle::Released);
    assert!(records[0].dispatch_at.is_none());
}

#[test]
fn native_source_change_during_prepare_releases_without_send() {
    let (_directory, catalog, _project, asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let call = valid_native_call(&audio, true);
    let transport =
        CountingNativeTransport::new(&catalog, &run_id, FakeNativeResponse::Ok(Box::new(call)))
            .mutating_asset_on_prepare(&asset);
    let error =
        AccountedNativeSttProvider::new(&catalog, &policy, context, &request, &audio, &transport)
            .transmit()
            .unwrap_err();
    assert!(
        format!("{error:#}").contains("pre_upload_failed"),
        "unexpected guard error: {error:#}"
    );
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 0);
    let records = catalog
        .list_analysis_requests(&policy.project_id, Some(&policy.policy_id), 10)
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].lifecycle, AnalysisRequestLifecycle::Released);
}

#[test]
fn native_duplicate_completed_request_cannot_resend_from_a_new_run() {
    let (_directory, catalog, _project, _asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let first = CountingNativeTransport::new(
        &catalog,
        &run_id,
        FakeNativeResponse::Ok(Box::new(valid_native_call(&audio, true))),
    );
    AccountedNativeSttProvider::new(&catalog, &policy, context.clone(), &request, &audio, &first)
        .transmit()
        .unwrap();
    assert_eq!(first.transcribe_calls.load(Ordering::SeqCst), 1);

    let new_run = catalog
        .begin_accounted_analysis_run(&policy.project_id, &policy.policy_id, "pilot")
        .unwrap();
    let mut repeat = context;
    repeat.run_id = new_run.clone();
    let second = CountingNativeTransport::new(
        &catalog,
        &new_run,
        FakeNativeResponse::Ok(Box::new(valid_native_call(&audio, true))),
    );
    let error =
        AccountedNativeSttProvider::new(&catalog, &policy, repeat, &request, &audio, &second)
            .transmit()
            .unwrap_err();
    assert!(
        format!("{error:#}").contains("completed provider receipt"),
        "{error:#}"
    );
    assert_eq!(second.prepare_calls.load(Ordering::SeqCst), 0);
    assert_eq!(second.transcribe_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn native_store_rejects_vlm_request_family_for_native_policy() {
    let (_directory, catalog, _project, _asset, policy, _request, _audio, _run_id, context) =
        authorized_native_setup();
    let error = catalog
        .reserve_analysis_request(
            &context,
            OPENROUTER_WHISPER_MODEL,
            "vlm-payload-fingerprint",
            &AnalysisRequestCostBound::declared_for_synthetic_transport(0.01),
        )
        .unwrap_err();
    assert!(
        format!("{error:#}")
            .contains("VLM accounted wrapper cannot spend against a native speech policy"),
        "{error:#}"
    );
    assert!(
        catalog
            .list_analysis_requests(&policy.project_id, Some(&policy.policy_id), 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn native_vlm_wrapper_rejects_native_audio_policy_before_provider() {
    let (_directory, catalog, _project, _asset, mut policy, request, audio, run_id, context) =
        authorized_native_setup();
    policy.kind = AnalysisRunKind::Observation;
    let call = valid_native_call(&audio, true);
    let transport =
        CountingNativeTransport::new(&catalog, &run_id, FakeNativeResponse::Ok(Box::new(call)));
    let error =
        AccountedNativeSttProvider::new(&catalog, &policy, context, &request, &audio, &transport)
            .transmit()
            .unwrap_err();
    assert!(format!("{error:#}").contains("audio-only native speech policy"));
    assert_eq!(transport.prepare_calls.load(Ordering::SeqCst), 0);
    assert_eq!(transport.transcribe_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn old_receipt_json_without_native_billing_field_is_byte_identical() {
    let evidence = ProviderReceiptEvidence {
        provider: "openai".into(),
        provider_generation_id: "generation-old".into(),
        actual_model: "model-old".into(),
        reported_cost_usd: Some(0.01),
        prompt_tokens: Some(3),
        completion_tokens: Some(2),
        usage_complete: true,
        billed_audio_seconds: None,
    };
    let encoded = serde_json::to_string(&evidence).unwrap();
    assert_eq!(
        encoded,
        r#"{"provider":"openai","providerGenerationId":"generation-old","actualModel":"model-old","reportedCostUsd":0.01,"promptTokens":3,"completionTokens":2,"usageComplete":true}"#
    );
}

#[test]
fn native_store_rejects_native_request_family_for_vlm_policy() {
    let (_directory, catalog, project, asset) = fixture();
    let vlm_request = RunPolicyRequest::observation(
        &project,
        vec![asset],
        DEFAULT_MODEL,
        vec![DEFAULT_MODEL.to_string()],
        false,
        1.0,
        false,
    );
    let vlm_policy = catalog.describe_run_policy(&vlm_request).unwrap();
    assert!(vlm_policy.readiness.ready_for_authorization);
    catalog.authorize_run_policy(&vlm_request).unwrap();
    let authority = AnalysisBudgetAuthorityContext {
        project_id: vlm_policy.project_id.clone(),
        policy_id: vlm_policy.policy_id.clone(),
        policy_fingerprint: vlm_policy.policy_id.clone(),
        operation: "pilot".into(),
        provider: vlm_policy.provider.clone(),
        budget_scope_id: String::new(),
        budget_usd: vlm_policy.budget_usd,
    };
    catalog
        .initialize_analysis_accounting_authority(&authority)
        .unwrap();
    let run_id = catalog
        .begin_accounted_analysis_run(&vlm_policy.project_id, &vlm_policy.policy_id, "pilot")
        .unwrap();
    let scope = vlm_policy.scope.first().unwrap();
    let context = AnalysisAccountingContext {
        project_id: vlm_policy.project_id.clone(),
        policy_id: vlm_policy.policy_id.clone(),
        policy_fingerprint: vlm_policy.policy_id.clone(),
        run_id,
        operation: "pilot".into(),
        intended_attempt_key: format!("{}:{}", vlm_policy.kind.as_str(), scope.asset_id),
        source_fingerprint: scope.source_identity.clone(),
        provider: vlm_policy.provider.clone(),
        budget_scope_id: String::new(),
        budget_usd: vlm_policy.budget_usd,
        explicit_retry: false,
    };
    let request = native_policy_request();
    let audio = native_audio_window();
    let error = catalog
        .reserve_analysis_native_stt_request(&context, &request, &audio)
        .unwrap_err();
    assert!(
        format!("{error:#}")
            .contains("native STT account wrapper requires a ready native speech policy"),
        "{error:#}"
    );
    assert!(
        catalog
            .list_analysis_requests(&vlm_policy.project_id, Some(&vlm_policy.policy_id), 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn native_catalog_admission_rejects_non_policy_and_non_pilot_shortcuts() {
    let (_directory, catalog, _project, _asset, _policy, request, audio, _run_id, context) =
        authorized_native_setup();

    let mut non_policy = context.clone();
    non_policy.policy_id.clear();
    let error = catalog
        .reserve_analysis_native_stt_request(&non_policy, &request, &audio)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("non-empty canonical policy identity"),
        "{error:#}"
    );

    let mut non_pilot = context;
    non_pilot.operation = "native_speech_transcription".into();
    let error = catalog
        .reserve_analysis_native_stt_request(&non_pilot, &request, &audio)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("owned run created for its exact native operation"),
        "{error:#}"
    );
}

#[test]
fn native_receipt_mapping_never_invents_identity_tokens_or_complete_usage() {
    let receipt = NativeSttReceipt {
        generation_id: Some("generation-overflow".into()),
        actual_model: None,
        actual_provider: None,
        usage: NativeSttUsage {
            cost: NativeSttCost::Unknown,
            seconds: Some(1.25),
            input_tokens: Some(u64::MAX),
            output_tokens: None,
        },
    };
    let evidence = ProviderReceiptEvidence::from_native_stt_receipt(&receipt).unwrap();
    assert!(evidence.provider.is_empty());
    assert!(evidence.actual_model.is_empty());
    assert_eq!(
        evidence.provider_generation_id.as_str(),
        "generation-overflow"
    );
    assert_eq!(evidence.reported_cost_usd, None);
    assert_eq!(evidence.prompt_tokens, None);
    assert_eq!(evidence.completion_tokens, None);
    assert!(!evidence.usage_complete);
    assert_eq!(evidence.billed_audio_seconds, Some(1.25));
}

#[test]
fn native_header_only_receipt_with_late_cost_reconciles_once_and_conflicts_fail() {
    let (_directory, catalog, _project, _asset, policy, request, audio, run_id, context) =
        authorized_native_setup();
    let transport = CountingNativeTransport::new(
        &catalog,
        &run_id,
        FakeNativeResponse::Ok(Box::new(valid_native_call(&audio, false))),
    );
    let result =
        AccountedNativeSttProvider::new(&catalog, &policy, context, &request, &audio, &transport)
            .transmit()
            .unwrap();
    let uncertain = catalog
        .analysis_request(&result.request_key)
        .unwrap()
        .unwrap();
    assert_eq!(uncertain.lifecycle, AnalysisRequestLifecycle::Uncertain);
    assert_eq!(
        uncertain.provider_generation_id.as_deref(),
        Some("native-generation-1")
    );

    let late = ProviderReceiptEvidence::from_native_stt_receipt(&NativeSttReceipt {
        generation_id: Some("native-generation-1".into()),
        actual_model: Some(OPENROUTER_WHISPER_MODEL.into()),
        actual_provider: Some("openai".into()),
        usage: NativeSttUsage {
            cost: NativeSttCost::Known(0.002),
            seconds: Some(1.0),
            input_tokens: Some(12),
            output_tokens: Some(4),
        },
    })
    .unwrap();
    let completed = catalog
        .record_analysis_request_receipt(&result.request_key, &late)
        .unwrap();
    assert_eq!(completed.lifecycle, AnalysisRequestLifecycle::Completed);
    assert!((completed.confirmed_cost_usd - 0.002).abs() < 1e-12);
    let repeated = catalog
        .record_analysis_request_receipt(&result.request_key, &late)
        .unwrap();
    assert_eq!(repeated, completed);

    let conflicting = ProviderReceiptEvidence::from_native_stt_receipt(&NativeSttReceipt {
        generation_id: Some("different-native-generation".into()),
        actual_model: Some(OPENROUTER_WHISPER_MODEL.into()),
        actual_provider: Some("openai".into()),
        usage: NativeSttUsage {
            cost: NativeSttCost::Known(0.002),
            seconds: Some(1.0),
            input_tokens: Some(12),
            output_tokens: Some(4),
        },
    })
    .unwrap();
    assert!(
        catalog
            .record_analysis_request_receipt(&result.request_key, &conflicting)
            .is_err()
    );
}

#[cfg(test)]
#[path = "native_provenance_tests.rs"]
mod native_provenance_tests;

#[cfg(test)]
#[path = "native_pilot_evidence_tests.rs"]
mod native_pilot_evidence_tests;

#[cfg(test)]
#[path = "native_runtime_tests.rs"]
mod native_runtime_tests;
