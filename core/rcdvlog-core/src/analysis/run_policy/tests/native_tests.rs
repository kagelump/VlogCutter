use super::*;
use crate::analysis::openrouter_stt::{
    OPENROUTER_STT_PROVIDER, OPENROUTER_WHISPER_MODEL, SttDataCollection,
};

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
fn native_speech_proposal_binds_registered_audio_only_scope_cost_and_stays_blocked() {
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
    assert_eq!(policy.recipe_version, native_stt::NATIVE_STT_RECIPE_VERSION);
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
            && check.status == CapabilityStatus::Failed
    }));
    assert!(
        policy
            .readiness
            .blockers
            .iter()
            .any(|blocker| { blocker.code == native_stt::NATIVE_STT_RUNTIME_UNAVAILABLE })
    );
    assert!(!policy.readiness.ready_for_authorization);
    assert!(!policy.readiness.ready_for_pilot);
    assert!(!policy.readiness.capabilities_ready);

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

    let auth_rows = table_count(&catalog, "analysis_run_authorizations");
    let run_rows = table_count(&catalog, "analysis_runs");
    let budget_rows = table_count(&catalog, "analysis_budget_authorities");
    let authorization_error = catalog.authorize_run_policy(&request).unwrap_err();
    assert!(format!("{authorization_error:#}").contains("proposal-only"));
    assert_eq!(
        table_count(&catalog, "analysis_run_authorizations"),
        auth_rows
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
        auth_rows
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

    let mut unqualified = native_request(&project, &asset, 1.0);
    unqualified.primary_model = "whisper-1".into();
    unqualified.allowed_models = vec!["whisper-1".into()];
    assert!(
        catalog
            .describe_run_policy(&unqualified)
            .unwrap()
            .readiness
            .blockers
            .iter()
            .any(|blocker| blocker.code == "unsupported_native_stt_model")
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

    let mut changed_language = request.clone();
    changed_language.parameters = RunPolicyParameters::NativeSpeechTranscription {
        language: Some("ja".into()),
        provider_policy: NativeSttProviderPolicy::new(true, SttDataCollection::Deny),
        force: false,
    };
    assert_ne!(
        catalog
            .describe_run_policy(&changed_language)
            .unwrap()
            .policy_id,
        baseline.policy_id
    );

    let mut changed_data_policy = request.clone();
    changed_data_policy.parameters = RunPolicyParameters::NativeSpeechTranscription {
        language: Some("en".into()),
        provider_policy: NativeSttProviderPolicy::new(true, SttDataCollection::Allow),
        force: false,
    };
    assert_ne!(
        catalog
            .describe_run_policy(&changed_data_policy)
            .unwrap()
            .policy_id,
        baseline.policy_id
    );

    let mut changed_budget = request.clone();
    changed_budget.budget_usd = 2.0;
    assert_ne!(
        catalog
            .describe_run_policy(&changed_budget)
            .unwrap()
            .policy_id,
        baseline.policy_id
    );

    let second = generated_asset(&catalog, &project, directory.path(), "clip2", 2);
    let mut changed_scope = request.clone();
    changed_scope.asset_ids.push(second);
    assert_ne!(
        catalog
            .describe_run_policy(&changed_scope)
            .unwrap()
            .policy_id,
        baseline.policy_id
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
