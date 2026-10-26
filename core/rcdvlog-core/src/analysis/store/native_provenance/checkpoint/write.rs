use super::*;

/// Begin a positive-speech native checkpoint plan.
///
/// The live preparation's source proof and claims remain held by the caller.
/// Existing local/complete/foreign-policy work is never cleared implicitly; an
/// identical current checkpoint is returned idempotently.
pub(crate) fn begin_native_transcript_checkpoint(
    catalog: &Catalog,
    preparation: &NativeSpeechPreparation,
    start: NativeTranscriptCheckpointStart,
) -> Result<NativeTranscriptCheckpoint> {
    ensure!(
        preparation.outcome() == NativeSpeechLocalOutcome::PreparedSpeech,
        "native checkpoint requires prepared positive speech"
    );
    ensure!(
        preparation.window_count() > 0 && !preparation.windows().is_empty(),
        "native checkpoint requires a positive bounded window plan"
    );
    preparation
        .validate_source_current(catalog)
        .context("validate current native checkpoint source")?;
    let evidence = preparation.evidence();
    let plan = NativeTranscriptWindowPlan {
        schema_version: NATIVE_TRANSCRIPT_WINDOW_PLAN_SCHEMA_VERSION.to_string(),
        windows: preparation.windows().to_vec(),
    };
    validate_plan(&plan, evidence.scope.source_duration_ms)?;

    let mut connection = catalog.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let policy = validated_checkpoint_policy(&transaction, evidence)?;
    let fingerprint = semantic_fingerprint(&plan, &policy, &evidence.scope)?;
    let source_fingerprint = evidence
        .scope
        .sampled_representation_fingerprint
        .clone()
        .ok_or_else(|| anyhow::anyhow!("native checkpoint source has no sampled fingerprint"))?;
    let header = incomplete_header(evidence, &plan, &[])?;
    let scope = validate_asset_scope(&transaction, &policy, &header)?;
    ensure!(
        scope == &evidence.scope,
        "native checkpoint preparation scope changed before persistence"
    );

    if let Some(existing) = load_transcript_header(&transaction, &evidence.asset_id)? {
        let native_associated =
            has_native_transcript_association(&transaction, &evidence.asset_id)?;
        match start {
            NativeTranscriptCheckpointStart::ResumeOnly => ensure!(
                existing.recipe_key == evidence.recipe
                    && existing.source_fingerprint == source_fingerprint
                    && existing.status == TranscriptStatus::Failed
                    && existing.outcome_reason == TranscriptOutcomeReason::NativeCoverageIncomplete
                    && native_associated,
                "native checkpoint cannot replace existing local or terminal transcript work"
            ),
            NativeTranscriptCheckpointStart::ExplicitNativeSelection => ensure!(
                !native_associated,
                "explicit native selection cannot replace prior native policy, quality, plan or paid-window work"
            ),
        }
        if !native_associated {
            // This is the narrow explicit backend-selection transition: the
            // caller selected native output over ordinary generated local
            // transcript rows. Human asset metadata and chronology are not in
            // this table and are never touched here. Resume-only callers have
            // already rejected this branch above.
            replace_checkpoint_rows(
                &transaction,
                &policy.policy_id,
                &header,
                &plan,
                &fingerprint,
                &[],
            )?;
            let snapshot = load_checkpoint_snapshot(&transaction, &evidence.asset_id)?;
            transaction.commit()?;
            return Ok(snapshot);
        }
        let (plan_json, existing_fingerprint): (Option<String>, Option<String>) = transaction
            .query_row(
                "SELECT native_window_plan_json, native_window_plan_fingerprint
                 FROM asset_transcripts WHERE asset_id=?1",
                [&evidence.asset_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
        let existing_plan_json = plan_json
            .ok_or_else(|| anyhow::anyhow!("existing native checkpoint has no window plan"))?;
        let existing_fingerprint = existing_fingerprint
            .ok_or_else(|| anyhow::anyhow!("existing native checkpoint has no plan fingerprint"))?;
        let existing_plan = crate::analysis::store::native_provenance::plan::parse_validated_plan(
            &existing_plan_json,
            &existing_fingerprint,
            &policy,
            scope,
        )?;
        ensure!(
            plan_matches_windows(&existing_plan, preparation.windows())
                && existing_fingerprint == fingerprint,
            "native checkpoint plan conflicts with the live prepared window map"
        );
        let snapshot = load_checkpoint_snapshot(&transaction, &evidence.asset_id)?;
        ensure!(
            snapshot.coverage.plan_fingerprint == fingerprint,
            "native checkpoint fingerprint changed while opening"
        );
        transaction.commit()?;
        return Ok(snapshot);
    }

    replace_checkpoint_rows(
        &transaction,
        &policy.policy_id,
        &header,
        &plan,
        &fingerprint,
        &[],
    )?;
    let snapshot = load_checkpoint_snapshot(&transaction, &evidence.asset_id)?;
    transaction.commit()?;
    Ok(snapshot)
}

/// Append one contiguous, quality-evaluated paid window result. The exact
/// accounting proof, source/plan/payload/map identity and canonical
/// header/segments/window links all commit or roll back together.
pub(crate) fn append_native_transcript_checkpoint(
    catalog: &Catalog,
    preparation: &NativeSpeechPreparation,
    input: NativeTranscriptCheckpointWindowInput<'_>,
) -> Result<NativeTranscriptCheckpoint> {
    ensure!(
        preparation.outcome() == NativeSpeechLocalOutcome::PreparedSpeech,
        "native checkpoint append requires prepared positive speech"
    );
    preparation
        .validate_source_current(catalog)
        .context("validate current native checkpoint source before append")?;
    let evidence = preparation.evidence();
    let pilot_sample = match &input {
        NativeTranscriptCheckpointWindowInput::PilotSample { sample, .. } => Some(*sample),
        _ => None,
    };
    let (request, audio, evaluated) = evaluate_checkpoint_input(input, evidence)?;
    let EvaluatedNativeCheckpointWindow {
        observation,
        candidates,
    } = evaluated;
    observation
        .validate()
        .context("validate native checkpoint window observation")?;
    ensure_checkpoint_quality_is_typed(observation.quality.decision, observation.quality.reason)?;
    crate::analysis::quality::native_stt::validate_native_mapped_candidates(
        observation.quality.decision,
        observation.quality.reason,
        &observation.reported_language,
        &candidates,
        audio.window(),
    )?;

    let payload_fingerprint = analysis_native_stt_payload_fingerprint(request, audio)?;
    let mut connection = catalog.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let policy = validated_checkpoint_policy(&transaction, evidence)?;
    validate_request_matches_policy(&policy, request, &evidence.recipe)?;
    ensure!(
        observation.reported_language.is_none()
            || observation
                .reported_language
                .as_deref()
                .is_some_and(|language| { normalized_native_stt_language(language).is_some() }),
        "native checkpoint reported language is not recognized"
    );

    let before = load_checkpoint_snapshot(&transaction, &evidence.asset_id)?;
    let scope = policy
        .scope
        .iter()
        .find(|scope| *scope == &evidence.scope)
        .ok_or_else(|| {
            anyhow::anyhow!("native checkpoint prepared scope is no longer authorized")
        })?;
    ensure!(
        before.plan.windows == preparation.windows(),
        "native checkpoint plan no longer matches the live prepared window map"
    );
    let current_fingerprint = semantic_fingerprint(&before.plan, &policy, scope)?;
    ensure!(
        current_fingerprint == before.coverage.plan_fingerprint,
        "native checkpoint plan fingerprint no longer matches the frozen policy/source"
    );

    let mut matching_indexes = before
        .plan
        .windows
        .iter()
        .enumerate()
        .filter(|(_, window)| *window == audio.window())
        .map(|(index, _)| index);
    let window_index = matching_indexes
        .next()
        .ok_or_else(|| anyhow::anyhow!("native checkpoint audio window is absent from the plan"))?;
    ensure!(
        matching_indexes.next().is_none(),
        "native checkpoint audio window maps to more than one plan entry"
    );
    let prepared_audio = preparation.materialize_window(window_index)?;
    ensure!(
        analysis_native_stt_payload_fingerprint(request, &prepared_audio)? == payload_fingerprint,
        "native checkpoint supplied audio differs from the exact current prepared PCM"
    );
    if let Some(sample) = pilot_sample {
        let canonical = crate::analysis::run_policy::reusable_native_pilot_sample_on_transaction(
            &transaction,
            preparation,
            request,
            audio,
        )?
        .ok_or_else(|| {
            anyhow::anyhow!("canonical native pilot sample is missing for checkpoint seeding")
        })?;
        ensure!(
            &canonical == sample,
            "supplied native pilot sample differs from its canonical pilot evidence"
        );
    }
    let completed = before.windows.len();
    let existing_owner: Option<String> = transaction
        .query_row(
            "SELECT asset_id FROM native_transcript_windows WHERE request_key=?1",
            [&observation.request_key],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(
        existing_owner
            .as_deref()
            .is_none_or(|owner| owner == evidence.asset_id),
        "native checkpoint accounting request is already linked to another asset"
    );

    let appended = NativeTranscriptCheckpointWindow {
        window_index: u32::try_from(window_index)?,
        observation,
        candidates,
    };
    validate_current_checkpoint_window(&transaction, &policy, scope, request, audio, &appended)?;

    if window_index < completed {
        let existing = &before.windows[window_index].observation;
        let same_result = existing.request_key == appended.observation.request_key
            && existing.latency_ms == appended.observation.latency_ms
            && existing.reported_language == appended.observation.reported_language
            && existing.quality == appended.observation.quality
            && before.windows[window_index].candidates == appended.candidates;
        ensure!(
            same_result,
            "native checkpoint window index already contains a conflicting result"
        );
        transaction.commit()?;
        return Ok(before);
    }
    ensure!(
        window_index == completed,
        "native checkpoint append must extend the contiguous completed prefix"
    );
    ensure!(
        !before
            .windows
            .iter()
            .any(|window| window.observation.request_key == appended.observation.request_key),
        "native checkpoint append duplicates an existing request"
    );
    for completed_window in &before.windows {
        let current_audio =
            preparation.materialize_window(completed_window.window_index as usize)?;
        validate_current_checkpoint_window(
            &transaction,
            &policy,
            scope,
            request,
            &current_audio,
            completed_window,
        )?;
    }
    let mut windows = before.windows.clone();
    windows.push(appended);
    let header = incomplete_header(evidence, &before.plan, &windows)?;
    validate_asset_scope(&transaction, &policy, &header)?;
    validate_plan(&before.plan, scope.source_duration_ms)?;
    ensure!(
        semantic_fingerprint(&before.plan, &policy, scope)? == before.coverage.plan_fingerprint,
        "native checkpoint plan identity changed before append"
    );
    replace_checkpoint_rows(
        &transaction,
        &policy.policy_id,
        &header,
        &before.plan,
        &before.coverage.plan_fingerprint,
        &windows,
    )?;
    let after = load_checkpoint_snapshot(&transaction, &evidence.asset_id)?;
    ensure!(
        after.windows.len() == completed + 1
            && after.coverage.completed_window_count as usize == completed + 1,
        "native checkpoint append did not retain the expected completed prefix"
    );
    transaction.commit()?;
    Ok(after)
}

pub(super) fn incomplete_header(
    evidence: &NativeSpeechPreparationEvidence,
    plan: &NativeTranscriptWindowPlan,
    windows: &[NativeTranscriptCheckpointWindow],
) -> Result<StoredTranscript> {
    let segments: Vec<DecodedSegment> = windows
        .iter()
        .flat_map(|window| window.candidates.iter().cloned())
        .collect();
    let primary_language = segments
        .first()
        .map(|candidate| candidate.language.clone())
        .or_else(|| {
            windows.iter().find_map(|window| {
                window
                    .observation
                    .reported_language
                    .as_deref()
                    .and_then(normalized_native_stt_language)
            })
        })
        .unwrap_or_default();
    let provider_segment_count = windows
        .iter()
        .try_fold(0_u32, |total, window| {
            total.checked_add(
                u32::try_from(
                    window
                        .observation
                        .quality
                        .metrics
                        .avg_logprob
                        .total_segments,
                )
                .ok()?,
            )
        })
        .ok_or_else(|| anyhow::anyhow!("native checkpoint provider segment count overflowed"))?;
    let speech_ms = plan
        .windows
        .iter()
        .try_fold(0_u64, |total, window| {
            total.checked_add(u64::from(window.speech_ms()))
        })
        .ok_or_else(|| anyhow::anyhow!("native checkpoint speech duration overflowed"))?;
    Ok(StoredTranscript {
        asset_id: evidence.asset_id.clone(),
        recipe_key: evidence.recipe.clone(),
        source_fingerprint: evidence
            .scope
            .sampled_representation_fingerprint
            .clone()
            .ok_or_else(|| {
                anyhow::anyhow!("native checkpoint source has no sampled fingerprint")
            })?,
        status: TranscriptStatus::Failed,
        outcome_reason: TranscriptOutcomeReason::NativeCoverageIncomplete,
        reject_reason: String::new(),
        primary_language,
        speech_seconds: speech_ms as f64 / 1_000.0,
        window_count: u32::try_from(windows.len())?,
        provider_segment_count,
        mean_token_probability: None,
        repetition_ratio: None,
        segments,
    })
}

/// Use the caller's IMMEDIATE transaction: the existing row writer clears old
/// child links and every retained observation is restored before commit.
pub(super) fn replace_checkpoint_rows(
    transaction: &Transaction<'_>,
    policy_id: &str,
    header: &StoredTranscript,
    plan: &NativeTranscriptWindowPlan,
    fingerprint: &str,
    windows: &[NativeTranscriptCheckpointWindow],
) -> Result<()> {
    let plan_json =
        serde_json::to_string(plan).context("serialize native transcript window plan")?;
    crate::analysis::store::write_transcript_rows(
        transaction,
        header,
        Some(policy_id),
        Some(NATIVE_STT_QUALITY_RECIPE_VERSION),
        Some(NativeTranscriptWindowPlanRow {
            plan_json: &plan_json,
            fingerprint,
        }),
    )?;
    for window in windows {
        insert_native_window_observation(
            transaction,
            &header.asset_id,
            usize::try_from(window.window_index)?,
            &window.observation,
        )?;
    }
    Ok(())
}
