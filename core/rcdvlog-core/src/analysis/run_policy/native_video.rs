//! Native video specializes description, not the authorization or money store.
use super::*;
use crate::analysis::native_video::{self as video, NATIVE_VIDEO_MODEL, NATIVE_VIDEO_PROVIDER};
use crate::{InspectionRepresentation, NativeVideoNavigation, NativeVideoOptions};

pub(super) fn describe(
    catalog: &Catalog,
    request: &RunPolicyRequest,
    assets: &[crate::Asset],
) -> Result<RunPolicy> {
    let RunPolicyParameters::NativeVideoReview { options } = &request.parameters else {
        bail!("native video parameters are missing");
    };
    video::validate_options(options)?;
    let mut blockers = Vec::new();
    let mut checks = Vec::new();
    let mut check = |requirement: &str, passed: bool, detail: &str| {
        checks.push(CapabilityCheck {
            requirement: requirement.into(),
            status: if passed {
                CapabilityStatus::Passed
            } else {
                CapabilityStatus::Failed
            },
            detail: detail.into(),
        });
        if !passed {
            blockers.push(RunPolicyBlocker {
                code: requirement.replace('.', "_"),
                message: detail.into(),
                asset_id: None,
            });
        }
    };
    check(
        "native_video.provider",
        request.provider == NATIVE_VIDEO_PROVIDER,
        "Only the explicitly selected direct Gemini endpoint is registered for native video.",
    );
    check(
        "native_video.model",
        request.primary_model == NATIVE_VIDEO_MODEL,
        "The selected native model must have a registered direct endpoint, video/schema contract and published tariff.",
    );
    check(
        "native_video.no_fallback",
        !request.allow_fallback
            && request
                .allowed_models
                .iter()
                .all(|model| model == &request.primary_model),
        "Native video allows exactly the selected model and requested Standard/Flex tier; no model or tier fallback.",
    );
    check(
        "native_video.payload",
        request.payload.uses_video(),
        "A native video payload must explicitly declare whether source audio and accepted transcript are included.",
    );
    check(
        "native_video.no_extra_context",
        !request.include_project_vocabulary,
        "Native video does not export project vocabulary or implicit trip context.",
    );
    check(
        "native_video.navigation",
        options.navigation == NativeVideoNavigation::Static,
        "Static host-controlled windows use the registered wire contract and still require an observed policy pilot. Provider-internal navigation is not yet observed for this exact endpoint/model/tier and bounded tool contract; it is never substituted silently.",
    );
    if request.payload.uses_transcript() {
        check(
            "native_video.transcript_representation",
            options.representation == InspectionRepresentation::GeneratedProxy,
            "Accepted transcript reuse requires the explicitly selected current generated proxy; no ASR or alternate representation is substituted.",
        );
    }
    let tools = match video::tool_fingerprint(&|| false) {
        Ok(identity) => identity,
        Err(_) => {
            blockers.push(RunPolicyBlocker {
                code: "native_video_media_tools_unavailable".into(),
                message:
                    "Native video needs the declared local encoder/probe tools before approval."
                        .into(),
                asset_id: None,
            });
            String::new()
        }
    };
    let mut scope = Vec::new();
    for asset in assets {
        let selected = match catalog.inspection_source(asset, options.representation, &|| false) {
            Ok(selected) => Some(selected),
            Err(error) => {
                blockers.push(RunPolicyBlocker {
                    code: "source_not_ready".into(),
                    message: format!(
                        "Selected native source representation is not ready: {}",
                        source_failure_code(&error)
                    ),
                    asset_id: Some(asset.id.clone()),
                });
                None
            }
        };
        if request.payload.uses_audio() {
            let audible = selected.as_ref().is_some_and(|source| {
                crate::editplan::inspection_media::probe(&source.path, true, &|| false)
                    .is_ok_and(|stream| stream.audio_start_seconds.is_some())
            });
            checks.push(CapabilityCheck{requirement:format!("native_video.source_audio.{}",asset.id),status:if audible{CapabilityStatus::Passed}else{CapabilityStatus::Failed},
                detail:"Explicit native audio export requires actual audio in the selected representation; no source fallback or synthetic silence is substituted.".into()});
            if !audible {
                blockers.push(RunPolicyBlocker{code:"native_video_source_audio_unavailable".into(),message:"Selected representation has no verified audio stream for this explicit audio payload.".into(),asset_id:Some(asset.id.clone())});
            }
        }
        let transcript = if request.payload.uses_transcript() {
            accepted_transcript(catalog, asset, options)?
        } else {
            None
        };
        scope.push(make_scope(
            asset,
            options,
            selected.as_ref(),
            transcript.as_ref(),
            &tools,
        )?);
    }
    let mut total_seconds = 0.0;
    let mut windows = 0u64;
    for requested in &options.source_scopes {
        let asset = assets
            .iter()
            .find(|a| a.id == requested.asset_id)
            .context("invalid input: native source scope is outside selected project assets")?;
        ensure!(
            requested.source_out_seconds <= asset.duration_seconds + 1e-7,
            "invalid input: native source scope exceeds source duration"
        );
        let seconds = requested.source_out_seconds - requested.source_in_seconds;
        total_seconds += seconds;
        windows += (seconds / f64::from(options.max_window_seconds)).ceil() as u64;
        if request.payload.uses_transcript() {
            let transcript = accepted_transcript(catalog, asset, options)?;
            let contained = transcript.as_ref().is_some_and(|t| {
                t.segments.iter().any(|s| {
                    f64::from(s.start_ms) / 1000.0 >= requested.source_in_seconds
                        && f64::from(s.end_ms) / 1000.0 <= requested.source_out_seconds
                })
            });
            if !contained {
                blockers.push(RunPolicyBlocker{code:"native_video_transcript_scope_unavailable".into(),message:"No current accepted whole transcript segment fits this source scope. Straddling text is not clipped or exported beyond the authorized window.".into(),asset_id:Some(asset.id.clone())});
            }
        }
    }
    ensure!(
        assets
            .iter()
            .all(|a| options.source_scopes.iter().any(|s| s.asset_id == a.id)),
        "invalid input: every selected asset needs an explicit native source scope"
    );
    ensure!(
        total_seconds <= f64::from(options.max_media_seconds) + 1e-7
            && windows <= u64::from(options.max_requests),
        "invalid input: initial native source coverage exceeds media/request limits"
    );
    let quote = video::cost_bound(options)?;
    let tariff = quote
        .published_token_tariff
        .as_ref()
        .context("native tariff missing")?;
    // Readiness projection only. Actual budget admission always reserves the
    // complete quote through the shared transaction, then prices returned usage.
    let predicted_input = total_seconds
        * (options.fps * 66.0
            + if request.payload.uses_audio() {
                32.0
            } else {
                0.0
            })
        + windows as f64 * 1024.0;
    let expected_cost_usd = (predicted_input * tariff.input_usd_per_million
        + windows as f64 * 512.0 * tariff.output_including_thinking_usd_per_million)
        / 1_000_000.0;
    let budget_ready =
        request.budget_usd >= quote.maximum_cost_usd && request.budget_usd >= expected_cost_usd;
    if !budget_ready {
        blockers.push(RunPolicyBlocker{code:"expected_cost_exceeds_budget".into(),message:format!("Native usage estimate is ${expected_cost_usd:.6}; one initial full reservation requires ${:.6}. The authorized ceiling must cover both.",quote.maximum_cost_usd),asset_id:None});
    }
    let credentials_present = video::credential_present();
    let capabilities_ready = checks.iter().all(|c| c.status != CapabilityStatus::Failed);
    let scope_ready = blockers
        .iter()
        .all(|b| b.code == "expected_cost_exceeds_budget");
    let readiness=RunPolicyReadiness{capabilities_ready,scope_ready,budget_ready,credentials_present,
        ready_for_authorization:capabilities_ready&&scope_ready&&budget_ready,
        ready_for_pilot:capabilities_ready&&scope_ready&&budget_ready&&credentials_present,
        blockers,warnings:vec![
            "Native video is sparse locator/review evidence. Provider times are not verified source-frame edits, word boundaries or editorial acceptance.".into(),
            "Only explicitly included audio/transcript may be exported. Names, species, OCR and spoken content remain uncertain until reviewed.".into(),
            "Published-rate estimates are separate from invoices. Missing or contradictory serving/usage evidence retains conservative reserves and is not automatically retransmitted.".into(),
            format!("One initial reservation is ${:.6}; concurrent reservations may require additional temporary headroom within the same ceiling.",quote.maximum_cost_usd),
        ]};
    let mut policy=RunPolicy{schema_version:RUN_POLICY_SCHEMA_VERSION.into(),policy_id:String::new(),project_id:request.project_id.clone(),kind:AnalysisRunKind::NativeVideoReview,
        provider:request.provider.clone(),primary_model:request.primary_model.clone(),allowed_models:vec![request.primary_model.clone()],allow_fallback:false,permitted_fallbacks:vec![],
        payload:request.payload,context:RunPolicyContext::default(),scope,expected_cost_usd,cost_basis:"published tariff and sparse-video input/output estimate; not an invoice or a provider-enforced total price cap".into(),budget_usd:request.budget_usd,parameters:request.parameters.clone(),
        prompt_version:video::NATIVE_VIDEO_PROMPT_VERSION.into(),output_schema_version:video::NATIVE_VIDEO_SCHEMA_VERSION.into(),recipe_version:video::NATIVE_VIDEO_MEDIA_RECIPE.into(),tool_version:RUN_POLICY_TOOL_VERSION.into(),provider_cost_bound_contract:quote.contract_version,capability_checks:checks,readiness};
    policy.policy_id = run_policy_identity(&policy)?;
    Ok(policy)
}

/// The shared inspection owner validates recipe/proxy identity. This metadata
/// read includes local sound context only; the native encoder independently
/// strips sound unless the policy explicitly authorizes audio export.
pub(crate) fn accepted_transcript(
    catalog: &Catalog,
    asset: &crate::Asset,
    options: &NativeVideoOptions,
) -> Result<Option<crate::ClipTranscript>> {
    catalog.inspection_transcript(
        asset,
        &crate::SourceInspectionRequest {
            asset_id: asset.id.clone(),
            representation: options.representation,
            purpose: crate::InspectionPurpose::ProposedBoundary,
            source_in_seconds: 0.0,
            source_out_seconds: asset.duration_seconds,
            before_seconds: 0.0,
            after_seconds: 0.0,
            sample_count: 1,
            max_width: options.max_width,
            source_times_seconds: vec![],
            exact_representation_pts: vec![],
            expected_representation_fingerprint: None,
            include_audio: true,
            include_transcript: true,
        },
    )
}

fn make_scope(
    asset: &crate::Asset,
    options: &NativeVideoOptions,
    selected: Option<&crate::editplan::inspection::SelectedSource>,
    transcript: Option<&crate::ClipTranscript>,
    tools: &str,
) -> Result<RunAssetScope> {
    let transcript_fingerprint = transcript.map(digest).transpose()?;
    let representation_fingerprint = selected.as_ref().map(|s| s.fingerprint.clone());
    let identity = digest(
        &serde_json::json!({"assetId":asset.id,"sourceFingerprint":asset.fingerprint,"durationSeconds":asset.duration_seconds,
        "rotation":asset.rotation_degrees,"orientationDecision":asset.orientation_decision,"representation":options.representation,
        "representationFingerprint":representation_fingerprint,"provenance":selected.as_ref().map(|s|&s.provenance),
        "sourceOffset":selected.as_ref().map(|s|s.source_offset),"representationOffset":selected.as_ref().map(|s|s.representation_offset),
        "scale":selected.as_ref().map(|s|s.scale),"precision":selected.as_ref().map(|s|s.precision),"transcriptFingerprint":transcript_fingerprint,"tools":tools}),
    )?;
    Ok(RunAssetScope {
        asset_id: asset.id.clone(),
        source_fingerprint: asset.fingerprint.clone(),
        source_duration_ms: (asset.duration_seconds * 1000.0).floor() as u32,
        representation: serde_json::to_value(options.representation)?
            .as_str()
            .expect("representation serializes as text")
            .into(),
        representation_fingerprint,
        sampled_representation_fingerprint: None,
        transcript_fingerprint,
        time_mapping: "native-video.source-inspection-clock.v1".into(),
        source_identity: identity,
    })
}

pub(crate) fn validate_asset(
    catalog: &Catalog,
    policy: &RunPolicy,
    asset_id: &str,
    cancel: &dyn Fn() -> bool,
) -> Result<()> {
    validate_asset_mode(catalog, policy, asset_id, false, cancel)
}
pub(crate) fn validate_asset_mode(
    catalog: &Catalog,
    policy: &RunPolicy,
    asset_id: &str,
    explicit_review: bool,
    cancel: &dyn Fn() -> bool,
) -> Result<()> {
    let RunPolicyParameters::NativeVideoReview { options } = &policy.parameters else {
        bail!("native parameters missing");
    };
    let asset = catalog.get_asset(asset_id)?;
    ensure!(
        asset.project_id == policy.project_id
            && (!asset.removed
                || explicit_review
                || options.purpose == crate::NativeVideoPurpose::EndpointReview),
        "native asset membership or review scope changed"
    );
    let selected = catalog.inspection_source(&asset, options.representation, cancel)?;
    let transcript = if policy.payload.uses_transcript() {
        accepted_transcript(catalog, &asset, options)?
    } else {
        None
    };
    let tools = video::tool_fingerprint(cancel)?;
    let current = make_scope(
        &asset,
        options,
        Some(&selected),
        transcript.as_ref(),
        &tools,
    )?;
    let approved = policy
        .scope
        .iter()
        .find(|s| s.asset_id == asset_id)
        .context("native asset outside policy")?;
    ensure!(
        &current == approved,
        "native source, representation, transcript or tools changed"
    );
    Ok(())
}

impl Catalog {
    /// One explicitly authorized bounded native-video capability sample. This
    /// uses the same carrier, accounting, cache and reply parser as bulk work.
    pub fn run_native_video_pilot(
        &self,
        request: &RunPolicyRequest,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<RunPilotRecord> {
        self.run_native_video_pilot_with_transport(
            request,
            &video::transport::DirectTransport::default(),
            cancelled,
        )
    }
    pub(crate) fn run_native_video_pilot_with_transport(
        &self,
        request: &RunPolicyRequest,
        transport: &dyn video::transport::Transport,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<RunPilotRecord> {
        ensure!(
            request.parameters.kind() == AnalysisRunKind::NativeVideoReview,
            "native video pilot requires its exact policy kind"
        );
        ensure!(!cancelled(), "native pilot cancelled before local work");
        let described = self.describe_run_policy_for_local_preparation(request)?;
        let _claim = video::runtime::claim_policy(self, &described.policy_id)?;
        let mut session = match prepare_pilot_session(self, request, false)? {
            PilotSessionStart::AlreadyPassed(record) | PilotSessionStart::Cached(record) => {
                if record.status == "passed" {
                    self.require_native_video_pilot(&described)?;
                }
                return Ok(*record);
            }
            PilotSessionStart::Ready(session) => *session,
        };
        let first = video::runtime::plan_native_video_windows(&session.policy)?
            .into_iter()
            .next()
            .context("native policy has no windows")?;
        let RunPolicyParameters::NativeVideoReview { options } = &session.policy.parameters else {
            bail!("native parameters missing");
        };
        let first_asset = &options.source_scopes[first.scope_index as usize].asset_id;
        session.first_scope = session
            .policy
            .scope
            .iter()
            .find(|s| &s.asset_id == first_asset)
            .context("native pilot asset scope missing")?
            .clone();
        let media = self.prepare_native_video_window(&session.policy, &first, cancelled)?;
        ensure!(!cancelled(), "native pilot cancelled before run ownership");
        let run_id = self.begin_accounted_analysis_run(
            &session.policy.project_id,
            &session.policy.policy_id,
            "pilot",
        )?;
        let mut running = self.start_owned_pilot_run(&session, &run_id, &crate::now())?;
        let outcome = video::runtime::execute_window(
            self,
            &session.policy,
            &run_id,
            "pilot",
            &media,
            transport,
            video::runtime::ExecutionControl {
                cancelled,
                wait_for_headroom: &|| false,
            },
        );
        let (validation, failure) = match outcome {
            Ok(result) => {
                running.native_video_evidence_id = Some(result.evidence_id.clone());
                self.save_pilot(&running)?;
                let valid = result.state == "completed";
                let proof = self.validate_native_video_pilot_evidence(&session.policy, &result);
                (
                    PilotValidation {
                        structural_schema_valid: valid,
                        source_time_semantics_valid: valid,
                        capability_observed: proof.is_ok(),
                        detail: if proof.is_ok() {
                            "native_static_video_sample_observed".into()
                        } else {
                            "native_video_sample_requires_review_or_usage_reconciliation".into()
                        },
                    },
                    (!valid || proof.is_err()).then(|| "native_video_sample_not_qualified".into()),
                )
            }
            Err(_) => (
                PilotValidation {
                    structural_schema_valid: false,
                    source_time_semantics_valid: false,
                    capability_observed: false,
                    detail: "native_video_pilot_incomplete".into(),
                },
                Some("native_video_pilot_incomplete".into()),
            ),
        };
        self.finalize_pilot_projection(&running, &session.baseline, validation, failure)
    }

    /// Check persisted exact protocol evidence without rescanning an unrelated
    /// first asset on each later request. Each transmitted asset has its own
    /// live source/representation/transcript guard.
    pub(crate) fn validate_native_video_pilot_evidence(
        &self,
        policy: &RunPolicy,
        result: &crate::NativeVideoWindowResult,
    ) -> Result<()> {
        let RunPolicyParameters::NativeVideoReview { options } = &policy.parameters else {
            bail!("native parameters missing");
        };
        let first = video::runtime::plan_native_video_windows(policy)?
            .into_iter()
            .next()
            .context("native windows missing")?;
        let first_scope = options
            .source_scopes
            .get(first.scope_index as usize)
            .context("native source scope missing")?;
        let expected = policy
            .scope
            .iter()
            .find(|s| s.asset_id == first_scope.asset_id)
            .context("native asset scope missing")?;
        ensure!(
            result.state == "completed"
                && result.provenance.asset_id == expected.asset_id
                && result.provenance.source_identity == expected.source_identity
                && result.provenance.source_fingerprint == expected.source_fingerprint
                && Some(&result.provenance.representation_fingerprint)
                    == expected.representation_fingerprint.as_ref()
                && result.request.source_in_seconds.to_bits() == first.source_in_seconds.to_bits()
                && result.request.source_out_seconds.to_bits()
                    == first.source_out_seconds.to_bits()
                && result.request.fps.to_bits() == first.fps.to_bits()
                && result.request.max_width == first.max_width
                && result.provenance.requested_model == video::NATIVE_VIDEO_MODEL
                && result.provenance.actual_model.as_deref() == Some(video::NATIVE_VIDEO_MODEL)
                && result.provenance.requested_tier == options.tier
                && result.provenance.reported_tier.as_deref() == Some(options.tier.as_str())
                && result.provenance.media_recipe == video::NATIVE_VIDEO_MEDIA_RECIPE
                && result.provenance.prompt_version == video::NATIVE_VIDEO_PROMPT_VERSION
                && result.provenance.schema_version == video::NATIVE_VIDEO_SCHEMA_VERSION,
            "native pilot response is not exact current source/protocol evidence"
        );
        let prior = self.load_run_authorization(&result.provenance.policy_id)?;
        let RunPolicyParameters::NativeVideoReview {
            options: prior_options,
        } = &prior.policy.parameters
        else {
            bail!("native pilot prior policy missing");
        };
        ensure!(
            prior.policy.provider == policy.provider
                && prior.policy.primary_model == policy.primary_model
                && prior.policy.payload == policy.payload
                && prior_options.purpose == options.purpose
                && prior_options.navigation == options.navigation
                && prior_options.thinking == options.thinking
                && prior_options.max_output_tokens == options.max_output_tokens,
            "native cached pilot settings differ from policy"
        );
        let request = self
            .analysis_request(
                result
                    .provenance
                    .request_key
                    .as_deref()
                    .context("native pilot has no canonical request receipt")?,
            )?
            .context("native pilot request receipt missing")?;
        ensure!(
            request.lifecycle == crate::AnalysisRequestLifecycle::Completed
                && request.provider == policy.provider
                && request.actual_provider.as_deref() == Some(policy.provider.as_str())
                && request.actual_model.as_deref() == Some(video::NATIVE_VIDEO_MODEL)
                && request.cost_bound.as_ref() == Some(&video::cost_bound(options)?)
                && request.source_fingerprint == expected.source_identity,
            "native pilot accounting is incomplete, mismatched or not a registered video request"
        );
        Ok(())
    }
    pub(crate) fn require_native_video_pilot(&self, policy: &RunPolicy) -> Result<()> {
        let pilot = self
            .load_run_pilot(&policy.policy_id)?
            .context("native capability pilot missing")?;
        ensure!(
            pilot.status == "passed"
                && pilot.failure_code.is_empty()
                && pilot.validation.structural_schema_valid
                && pilot.validation.source_time_semantics_valid
                && pilot.validation.capability_observed
                && pilot.usage_complete,
            "native video capability pilot has not passed"
        );
        let result = self
            .native_video_window_result(
                &policy.project_id,
                pilot
                    .native_video_evidence_id
                    .as_deref()
                    .context("native pilot evidence identity missing")?,
            )?
            .context("native pilot evidence body missing")?;
        self.validate_native_video_pilot_evidence(policy, &result)
    }
    pub(crate) fn refresh_native_video_pilot_after_recovery(
        &self,
        policy: &RunPolicy,
        run_id: &str,
        result: &crate::NativeVideoWindowResult,
    ) -> Result<()> {
        let Some(existing) = self.load_run_pilot(&policy.policy_id)? else {
            return Ok(());
        };
        if existing.run_id != run_id {
            return Ok(());
        }
        let valid = result.state == "completed";
        let observed = valid
            && self
                .validate_native_video_pilot_evidence(policy, result)
                .is_ok();
        let mut replacement = existing.clone();
        // The policy claim proves no current native runtime owns a formerly
        // running row. Local recovery validates bytes; it never runs a pilot.
        if replacement.status == "running" {
            replacement.status = "failed".into();
        }
        replacement.native_video_evidence_id = Some(result.evidence_id.clone());
        replacement.validation = PilotValidation {
            structural_schema_valid: valid,
            source_time_semantics_valid: valid,
            capability_observed: observed,
            detail: "native_response_recovered_locally_without_transmission".into(),
        };
        ensure!(
            self.save_pilot_if_unchanged(&existing, &replacement)?,
            "native pilot projection changed during local recovery; inspect the current record"
        );
        let ledger = self
            .analysis_request(
                result
                    .provenance
                    .request_key
                    .as_deref()
                    .context("native recovered receipt missing")?,
            )?
            .context("native recovered receipt missing")?;
        self.refresh_pilot_projection_after_reconciliation(&ledger)?;
        Ok(())
    }
}
