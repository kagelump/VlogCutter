use super::*;

/// Current reuse probe for the exact live preparation and request. Every
/// completed index is re-proved against the exact current payload/map. A
/// missing type after a dispatched/paid ledger row is never reported as a
/// cache miss.
pub(crate) fn probe_native_transcript_checkpoint_for_reuse(
    catalog: &Catalog,
    preparation: &NativeSpeechPreparation,
    request: &NativeSttRequest,
) -> Result<NativeTranscriptCheckpointReuse> {
    ensure!(
        preparation.outcome() == NativeSpeechLocalOutcome::PreparedSpeech,
        "native checkpoint reuse requires prepared positive speech"
    );
    preparation
        .validate_source_current(catalog)
        .context("validate current source before native checkpoint reuse")?;
    let evidence = preparation.evidence();
    let request_recipe = native_stt_transcript_recipe(request)?;
    ensure!(
        request_recipe == evidence.recipe,
        "native checkpoint reuse request does not match the live prepared recipe"
    );

    let connection = catalog.connection()?;
    let transaction = connection.unchecked_transaction()?;
    let provenance = load_native_transcript_provenance(&transaction, &evidence.asset_id)?
        .ok_or_else(|| anyhow::anyhow!("native checkpoint transcript disappeared"))?;
    let checkpoint = {
        let plan_json: Option<String> = transaction
            .query_row(
                "SELECT native_window_plan_json FROM asset_transcripts WHERE asset_id=?1",
                [&evidence.asset_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let plan_json = plan_json
            .ok_or_else(|| anyhow::anyhow!("native checkpoint transcript has no window plan"))?;
        let scope = provenance
            .policy
            .scope
            .iter()
            .find(|scope| scope.asset_id == evidence.asset_id)
            .ok_or_else(|| anyhow::anyhow!("native checkpoint frozen source scope is absent"))?;
        let fingerprint = provenance
            .coverage
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("native checkpoint coverage is absent"))?
            .plan_fingerprint
            .clone();
        let plan = crate::analysis::store::native_provenance::plan::parse_validated_plan(
            &plan_json,
            &fingerprint,
            &provenance.policy,
            scope,
        )?;
        checkpoint_from_provenance(provenance.clone(), plan)?
    };
    let policy = provenance.policy;
    ensure!(
        policy.policy_id == evidence.policy_id
            && policy.project_id == evidence.project_id
            && policy.provider == evidence.provider
            && policy.primary_model == evidence.model,
        "native checkpoint reuse policy identity changed"
    );

    validate_request_matches_policy(&policy, request, &evidence.recipe)?;
    let scope = policy
        .scope
        .iter()
        .find(|scope| *scope == &evidence.scope)
        .ok_or_else(|| anyhow::anyhow!("native checkpoint reuse source is no longer authorized"))?;
    ensure!(
        checkpoint.plan.windows == preparation.windows(),
        "native checkpoint reuse plan no longer matches the live prepared window map"
    );
    let current_fingerprint = semantic_fingerprint(&checkpoint.plan, &policy, scope)?;
    ensure!(
        current_fingerprint == checkpoint.coverage.plan_fingerprint,
        "native checkpoint reuse plan fingerprint does not match the current policy/source"
    );
    ensure!(
        checkpoint.asset_id == evidence.asset_id
            && checkpoint.coverage.completed_window_count as usize == checkpoint.windows.len(),
        "native checkpoint reuse snapshot is inconsistent"
    );

    let mut states = Vec::with_capacity(checkpoint.plan.windows.len());
    for (index, plan_window) in checkpoint.plan.windows.iter().enumerate() {
        let audio = preparation
            .materialize_window(index)
            .with_context(|| format!("materialize native checkpoint reuse window {index}"))?;
        ensure!(
            audio.window() == plan_window,
            "native checkpoint reuse prepared audio map changed"
        );
        let payload_fingerprint = analysis_native_stt_payload_fingerprint(request, &audio)?;
        if let Some(completed) = checkpoint.windows.get(index) {
            ensure!(
                provenance.native_quality_recipe_version == NATIVE_STT_QUALITY_RECIPE_VERSION,
                "native checkpoint reuse requires the current quality recipe"
            );
            validate_current_checkpoint_window(
                &transaction,
                &policy,
                scope,
                request,
                &audio,
                completed,
            )?;
            states.push(NativeTranscriptWindowReuse::Cached(completed.clone()));
            continue;
        }

        let blocking = accounting_store::native_request_requiring_result_recovery(
            &transaction,
            &policy.project_id,
            &policy.policy_id,
            &scope.source_identity,
            &policy.provider,
            &policy.primary_model,
            &payload_fingerprint,
        )?;
        if let Some(candidate) = blocking {
            states.push(NativeTranscriptWindowReuse::RecoveryRequired {
                window_index: index as u32,
                request_key: Some(candidate.request_key.clone()),
            });
        } else {
            states.push(NativeTranscriptWindowReuse::Missing {
                window_index: index as u32,
            });
        }
    }
    ensure!(
        states
            .iter()
            .take(checkpoint.windows.len())
            .all(|state| matches!(state, NativeTranscriptWindowReuse::Cached(_))),
        "native checkpoint completed prefix did not fully reuse"
    );
    transaction.commit()?;
    Ok(NativeTranscriptCheckpointReuse {
        checkpoint,
        windows: states,
    })
}
