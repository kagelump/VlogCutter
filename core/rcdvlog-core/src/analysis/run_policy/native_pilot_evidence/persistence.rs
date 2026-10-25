//! Transactional pilot evidence attachment and current prepared-window reuse.

use super::*;

impl Catalog {
    /// Attach bounded native evidence to the exact current running pilot record.
    ///
    /// This is an additive compare-and-write operation inside one
    /// `BEGIN IMMEDIATE` transaction. It proves the frozen authorization and
    /// accounting contract, the exact running run/policy/project/asset owner,
    /// current policy scopes, and (for a paid sample) the exact native payload
    /// hash plus immutable settled/uncertain account request and receipt history.
    /// It never changes final validation/status/money fields and returns `None`
    /// when `expected` is stale.
    #[allow(dead_code)] // Consumed by the next native runtime slice.
    pub(crate) fn attach_native_pilot_evidence(
        &self,
        expected: &RunPilotRecord,
        evidence: NativePilotEvidence,
        request_and_audio: Option<(&NativeSttRequest, &NativeSttAudioWindow)>,
    ) -> Result<Option<RunPilotRecord>> {
        evidence.validate()?;
        ensure!(
            !expected.policy_id.trim().is_empty()
                && !expected.project_id.trim().is_empty()
                && expected.status == "running",
            "native pilot evidence can attach only to a running owned pilot"
        );

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw: Option<String> = transaction
            .query_row(
                "SELECT record_json FROM analysis_run_pilots WHERE policy_id=?1",
                [&expected.policy_id],
                |row| row.get(0),
            )
            .optional()?;
        let current = raw
            .as_deref()
            .map(serde_json::from_str::<RunPilotRecord>)
            .transpose()
            .context("parse current pilot record before native evidence attach")?;
        if current.as_ref() != Some(expected) {
            transaction.commit()?;
            return Ok(None);
        }

        let (authorization, accounting_contract) =
            load_run_authorization_on_connection(&transaction, &expected.policy_id)?;
        ensure!(
            accounting_contract == crate::analysis::accounting::ANALYSIS_ACCOUNTING_SCHEMA_VERSION,
            "native pilot evidence policy was not authorized under the current accounting contract"
        );
        let policy = &authorization.policy;
        ensure!(
            policy.policy_id == expected.policy_id
                && policy.project_id == expected.project_id
                && policy.kind == AnalysisRunKind::NativeSpeechTranscription
                && policy.payload == AnalysisPayload::Audio
                && policy.readiness.ready_for_authorization,
            "native pilot evidence policy/project identity or native readiness changed"
        );
        ensure!(
            policy.allowed_models == vec![policy.primary_model.clone()]
                && !policy.allow_fallback
                && policy.permitted_fallbacks.is_empty()
                && policy.provider_cost_bound_contract
                    == crate::analysis::openrouter_stt::native_stt_cost_bound_contract_id(),
            "native pilot evidence requires the exact no-fallback native policy contract"
        );
        validate_running_run_owner(&transaction, policy, expected)?;
        validate_evidence_against_policy(
            &transaction,
            policy,
            expected,
            &evidence,
            request_and_audio,
        )?;

        let supplied_sample = evidence.sample.is_some();
        let mut merged = current
            .as_ref()
            .and_then(|record| record.native_evidence.clone())
            .unwrap_or_else(NativePilotEvidence::empty);
        if let Some(sample) = evidence.sample {
            ensure!(
                merged.sample.is_none(),
                "native pilot evidence already contains its one bounded paid sample"
            );
            merged.sample = Some(sample);
        }
        let existing_assets: HashSet<String> = merged
            .local_outcomes
            .iter()
            .map(|fact| fact.scope.asset_id.clone())
            .collect();
        for fact in evidence.local_outcomes {
            ensure!(
                !existing_assets.contains(&fact.scope.asset_id),
                "native pilot evidence already contains a local fact for this asset"
            );
            merged.local_outcomes.push(fact);
        }
        merged.schema_version = NATIVE_PILOT_EVIDENCE_SCHEMA_VERSION.to_string();
        merged.validate()?;
        if supplied_sample {
            validate_evidence_against_policy(
                &transaction,
                policy,
                expected,
                &merged,
                request_and_audio,
            )?;
        } else {
            // Adding only local facts leaves the previously proven paid sample
            // untouched. Its exact request/window cannot be re-proved without
            // re-supplying that transient request, but its structural evidence
            // remains validated above.
            let mut local_only = merged.clone();
            local_only.sample = None;
            validate_evidence_against_policy(&transaction, policy, expected, &local_only, None)?;
        }

        let mut replacement = current.expect("current pilot record checked above");
        replacement.native_evidence = Some(merged);
        let rows = transaction.execute(
            "UPDATE analysis_run_pilots SET record_json=?2
             WHERE policy_id=?1 AND project_id=?3 AND record_json=?4",
            params![
                replacement.policy_id,
                serde_json::to_string(&replacement)?,
                replacement.project_id,
                raw.expect("current pilot JSON checked above"),
            ],
        )?;
        if rows != 1 {
            transaction.rollback()?;
            return Ok(None);
        }
        transaction.commit()?;
        Ok(Some(replacement))
    }

    /// Re-prove that a recorded paid pilot sample is the exact current prepared
    /// request/window before a future bulk caller reuses it.
    ///
    /// This performs no provider/model work and is not a spend permit. A receipt
    /// without a recorded typed sample returns `None`; it cannot reconstruct
    /// text or quality evidence. Any stale/tampered proof is an explicit error.
    #[allow(dead_code)] // Consumed by the next native runtime slice.
    pub(crate) fn reusable_native_pilot_sample(
        &self,
        preparation: &NativeSpeechPreparation,
        request: &NativeSttRequest,
        audio: &NativeSttAudioWindow,
    ) -> Result<Option<NativePilotPaidSample>> {
        // The existing preparation guard owns filesystem/proxy currentness.
        preparation.validate_source_current(self)?;
        let preparation_evidence = preparation.evidence();
        let scope = &preparation_evidence.scope;

        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction()?;
        let (authorization, accounting_contract) =
            load_run_authorization_on_connection(&transaction, &preparation_evidence.policy_id)?;
        ensure!(
            accounting_contract == crate::analysis::accounting::ANALYSIS_ACCOUNTING_SCHEMA_VERSION,
            "reusable native pilot evidence policy uses a stale accounting contract"
        );
        let policy = &authorization.policy;
        ensure!(
            policy.policy_id == preparation_evidence.policy_id
                && policy.project_id == preparation_evidence.project_id
                && policy.kind == AnalysisRunKind::NativeSpeechTranscription
                && policy.payload == AnalysisPayload::Audio
                && policy.provider == preparation_evidence.provider
                && policy.primary_model == preparation_evidence.model
                && policy.recipe_version == preparation_evidence.recipe
                && policy.readiness.ready_for_authorization,
            "reusable native pilot evidence policy/recipe identity changed"
        );
        ensure_provider_request_matches_policy(policy, request)?;
        ensure!(
            policy
                .scope
                .iter()
                .filter(|candidate| *candidate == scope)
                .count()
                == 1,
            "reusable native pilot preparation scope is stale or foreign"
        );

        if preparation.outcome() != NativeSpeechLocalOutcome::PreparedSpeech {
            transaction.commit()?;
            return Ok(None);
        }
        let window_index = preparation
            .windows()
            .iter()
            .position(|window| window == audio.window())
            .ok_or_else(|| {
                anyhow::anyhow!("requested native window is not in the current prepared speech map")
            })?;
        let prepared_audio = preparation.materialize_window(window_index)?;
        let current_payload_fingerprint =
            crate::analysis::accounting::analysis_native_stt_payload_fingerprint(request, audio)?;
        ensure!(
            crate::analysis::accounting::analysis_native_stt_payload_fingerprint(
                request,
                &prepared_audio
            )? == current_payload_fingerprint,
            "supplied native audio differs from the current prepared samples"
        );

        let raw: Option<String> = transaction
            .query_row(
                "SELECT record_json FROM analysis_run_pilots WHERE policy_id=?1 AND project_id=?2",
                params![policy.policy_id, policy.project_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            transaction.commit()?;
            return Ok(None);
        };
        let pilot: RunPilotRecord =
            serde_json::from_str(&raw).context("parse pilot record for native sample reuse")?;
        ensure!(
            pilot.policy_id == policy.policy_id && pilot.project_id == policy.project_id,
            "reusable native pilot record owner changed"
        );
        let Some(evidence) = pilot.native_evidence.as_ref() else {
            transaction.commit()?;
            return Ok(None);
        };
        evidence.validate()?;
        let Some(sample) = evidence.sample.as_ref() else {
            transaction.commit()?;
            return Ok(None);
        };
        ensure!(
            sample.scope.asset_id == pilot.asset_id && policy.scope.contains(&sample.scope),
            "reusable native pilot sample is outside its owning frozen scope"
        );
        if sample.scope.asset_id != scope.asset_id || sample.window != *audio.window() {
            transaction.commit()?;
            return Ok(None);
        }
        ensure!(
            sample.coverage == NativePilotSampleCoverage::PartialWindow
                && sample.scope == *scope
                && sample.requested_provider == request.provider()
                && sample.requested_model == request.model()
                && sample.recipe_version == policy.recipe_version
                && sample.quality_recipe_version == NATIVE_STT_QUALITY_RECIPE_VERSION
                && sample.window == *audio.window(),
            "reusable native pilot sample coverage/scope/recipe/window changed"
        );
        ensure!(
            sample.payload_fingerprint == current_payload_fingerprint,
            "reusable native pilot sample payload fingerprint changed"
        );
        crate::analysis::accounting::store::validate_native_audio_source_bounds(
            audio,
            scope.source_duration_ms,
        )?;

        let request_record =
            crate::analysis::accounting::store::validate_native_settled_result_request(
                &transaction,
                &sample.request_key,
                &NativeSettledResultProof {
                    project_id: &policy.project_id,
                    policy_id: &policy.policy_id,
                    policy_fingerprint: &policy.policy_id,
                    expected_provider_cost_bound_contract: &policy.provider_cost_bound_contract,
                    source_fingerprint: &scope.source_identity,
                    requested_provider: &policy.provider,
                    requested_model: &policy.primary_model,
                    run_id: Some(&pilot.run_id),
                    allowed_operations: &["pilot"],
                },
            )?;
        ensure!(
            request_record.payload_fingerprint == current_payload_fingerprint,
            "reusable native pilot sample request payload identity changed"
        );
        let sample = sample.clone();
        transaction.commit()?;
        Ok(Some(sample))
    }
}

/// Validate the complete local-only native outcome inside the same transaction
/// that will write its projection. This helper does not build or update any
/// financial fields; the caller's existing aggregate/ledger projection proves
/// zero actual attempts and no unresolved usage before choosing the explicit
/// `no_provider_required` status.
pub(crate) fn validate_native_no_provider_finalization(
    connection: &Connection,
    expected: &RunPilotRecord,
) -> Result<()> {
    ensure!(
        expected.status == "running" && !expected.run_id.trim().is_empty(),
        "native no-provider evidence requires the exact running owned pilot"
    );
    let (authorization, accounting_contract) =
        load_run_authorization_on_connection(connection, &expected.policy_id)?;
    ensure!(
        accounting_contract == crate::analysis::accounting::ANALYSIS_ACCOUNTING_SCHEMA_VERSION,
        "native no-provider finalization uses a stale accounting contract"
    );
    let policy = &authorization.policy;
    ensure!(
        policy.policy_id == expected.policy_id
            && policy.project_id == expected.project_id
            && policy.kind == AnalysisRunKind::NativeSpeechTranscription
            && policy.payload == AnalysisPayload::Audio
            && policy.readiness.ready_for_authorization,
        "native no-provider policy/project identity or native readiness changed"
    );
    ensure!(
        policy.allowed_models == vec![policy.primary_model.clone()]
            && !policy.allow_fallback
            && policy.permitted_fallbacks.is_empty()
            && policy.provider_cost_bound_contract
                == crate::analysis::openrouter_stt::native_stt_cost_bound_contract_id(),
        "native no-provider evidence requires the exact no-fallback native policy contract"
    );
    validate_running_run_owner(connection, policy, expected)?;
    let evidence = expected
        .native_evidence
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("native no-provider pilot is missing its local facts"))?;
    evidence.validate()?;
    ensure!(
        evidence.sample.is_none(),
        "native no-provider finalization cannot carry a paid sample"
    );
    validate_evidence_against_policy(connection, policy, expected, evidence, None)?;
    ensure!(
        evidence.local_outcomes.len() == policy.scope.len(),
        "native no-provider finalization requires exactly one local fact for every selected scope entry"
    );
    let policy_assets: HashSet<&str> = policy
        .scope
        .iter()
        .map(|scope| scope.asset_id.as_str())
        .collect();
    let fact_assets: HashSet<&str> = evidence
        .local_outcomes
        .iter()
        .map(|fact| fact.scope.asset_id.as_str())
        .collect();
    ensure!(
        policy_assets == fact_assets && fact_assets.len() == policy.scope.len(),
        "native no-provider local facts do not cover the complete frozen scope"
    );
    Ok(())
}

fn validate_running_run_owner(
    connection: &Connection,
    policy: &RunPolicy,
    expected: &RunPilotRecord,
) -> Result<()> {
    ensure!(
        !expected.run_id.trim().is_empty() && !expected.asset_id.trim().is_empty(),
        "native pilot evidence requires an owned run and asset identity"
    );
    let run: Option<(String, String, String, String, String)> = connection
        .query_row(
            "SELECT project_id,policy_id,status,stage_reached,accounting_contract
             FROM analysis_runs WHERE id=?1",
            [&expected.run_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let (project_id, policy_id, status, stage, contract) =
        run.ok_or_else(|| anyhow::anyhow!("native pilot evidence accounting run does not exist"))?;
    ensure!(
        project_id == policy.project_id
            && policy_id == policy.policy_id
            && status == "running"
            && stage == "pilot"
            && contract == crate::analysis::accounting::ANALYSIS_ACCOUNTING_SCHEMA_VERSION,
        "native pilot evidence run ownership, operation or accounting contract changed"
    );
    ensure!(
        policy
            .scope
            .iter()
            .any(|scope| scope.asset_id == expected.asset_id),
        "native pilot running asset is outside the authorized policy scope"
    );
    Ok(())
}

fn validate_evidence_against_policy(
    connection: &Connection,
    policy: &RunPolicy,
    expected: &RunPilotRecord,
    evidence: &NativePilotEvidence,
    request_and_audio: Option<(&NativeSttRequest, &NativeSttAudioWindow)>,
) -> Result<()> {
    ensure!(
        evidence.schema_version == NATIVE_PILOT_EVIDENCE_SCHEMA_VERSION,
        "native pilot evidence schema version is unsupported"
    );
    if let Some(sample) = &evidence.sample {
        let (request, audio) = request_and_audio.ok_or_else(|| {
            anyhow::anyhow!("native pilot paid sample requires its exact request and audio window")
        })?;
        ensure!(
            sample.scope.asset_id == expected.asset_id,
            "native pilot paid sample asset is not the current running pilot asset"
        );
        let scope = policy
            .scope
            .iter()
            .find(|scope| scope.asset_id == sample.scope.asset_id)
            .ok_or_else(|| {
                anyhow::anyhow!("native pilot paid sample asset is outside policy scope")
            })?;
        ensure!(
            sample.scope == *scope,
            "native pilot paid sample scope does not match the frozen policy"
        );
        ensure_current_asset_scope(connection, policy, scope)?;
        ensure!(
            sample.requested_provider == policy.provider
                && sample.requested_model == policy.primary_model
                && sample.recipe_version == policy.recipe_version
                && sample.quality_recipe_version == NATIVE_STT_QUALITY_RECIPE_VERSION,
            "native pilot paid sample provider/model/recipe does not match the frozen policy"
        );
        ensure_provider_request_matches_policy(policy, request)?;
        ensure!(
            sample.window == *audio.window(),
            "native pilot paid sample window does not match the supplied NativeSttAudioWindow"
        );
        let payload_fingerprint =
            crate::analysis::accounting::analysis_native_stt_payload_fingerprint(request, audio)?;
        ensure!(
            sample.payload_fingerprint == payload_fingerprint,
            "native pilot paid sample payload fingerprint does not match the exact request/window"
        );
        crate::analysis::accounting::store::validate_native_audio_source_bounds(
            audio,
            scope.source_duration_ms,
        )?;
        let accounted = crate::analysis::accounting::store::validate_native_settled_result_request(
            connection,
            &sample.request_key,
            &NativeSettledResultProof {
                project_id: &policy.project_id,
                policy_id: &policy.policy_id,
                policy_fingerprint: &policy.policy_id,
                expected_provider_cost_bound_contract: &policy.provider_cost_bound_contract,
                source_fingerprint: &scope.source_identity,
                requested_provider: &policy.provider,
                requested_model: &policy.primary_model,
                run_id: Some(&expected.run_id),
                allowed_operations: &["pilot"],
            },
        )?;
        ensure!(
            accounted.payload_fingerprint == payload_fingerprint,
            "native pilot paid sample accounting request payload identity changed"
        );
    }

    for (index, fact) in evidence.local_outcomes.iter().enumerate() {
        let scope = policy
            .scope
            .iter()
            .find(|scope| scope.asset_id == fact.scope.asset_id)
            .ok_or_else(|| {
                anyhow::anyhow!("native pilot local fact {index} asset is outside policy scope")
            })?;
        ensure!(
            fact.scope == *scope,
            "native pilot local fact {index} scope does not match the frozen policy"
        );
        ensure!(
            fact.recipe_version == policy.recipe_version,
            "native pilot local fact {index} recipe does not match the frozen policy"
        );
        ensure_current_asset_scope(connection, policy, scope)?;
    }
    Ok(())
}

fn ensure_current_asset_scope(
    connection: &Connection,
    policy: &RunPolicy,
    scope: &RunAssetScope,
) -> Result<()> {
    let removed = crate::asset_column_expression(connection, "removed", "removed", "0")?;
    let membership_removed = crate::asset_column_expression(
        connection,
        "membership_removed",
        "membership_removed",
        "0",
    )?;
    let sql = format!(
        "SELECT project_id,fingerprint,duration_seconds,COALESCE({removed},0),COALESCE({membership_removed},0)
         FROM assets WHERE id=?1"
    );
    let row: Option<(String, String, f64, i64, i64)> = connection
        .query_row(&sql, [&scope.asset_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .optional()?;
    let (project_id, fingerprint, duration_seconds, removed, membership_removed) =
        row.ok_or_else(|| anyhow::anyhow!("native pilot evidence asset does not exist"))?;
    ensure!(
        project_id == policy.project_id
            && fingerprint == scope.source_fingerprint
            && removed == 0
            && membership_removed == 0,
        "native pilot evidence asset is foreign, excluded, removed or fingerprint-stale"
    );
    ensure!(
        duration_seconds.is_finite() && duration_seconds >= 0.0,
        "native pilot evidence asset duration is invalid"
    );
    let duration_ms = (duration_seconds * 1_000.0).round();
    ensure!(
        duration_ms.is_finite()
            && duration_ms <= u32::MAX as f64
            && duration_ms as u32 == scope.source_duration_ms,
        "native pilot evidence asset duration is stale"
    );
    Ok(())
}
