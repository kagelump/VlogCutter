//! Atomic finalization of a fully retained native speech checkpoint.
//!
//! This is a catalog persistence boundary, not bulk/provider orchestration. It
//! reuses the existing checkpoint reads, RunPolicy authorization, accounting
//! settlement proof and complete-native writer. No provider, model, media
//! extraction or credential work occurs here; only bounded PCM held by the live
//! preparation is re-encoded lazily while the same IMMEDIATE transaction is open.

use std::collections::HashSet;

use anyhow::{Context, Result, ensure};

use super::*;
use crate::analysis::store::native_provenance::plan::parse_validated_plan;
use crate::analysis::store::native_provenance::save_native_transcript_on_transaction;
use crate::analysis::store::native_provenance::validation::validate_transcript_shape;

/// Finalize one exact complete C9 checkpoint into the existing terminal
/// transcript artifact. Source/claims and current accounting proofs are checked
/// atomically; SQL failures leave the entire retained checkpoint unchanged.
pub(crate) fn finalize_native_transcript_checkpoint(
    catalog: &Catalog,
    preparation: &NativeSpeechPreparation,
    request: &NativeSttRequest,
) -> Result<StoredTranscript> {
    ensure!(
        preparation.outcome() == NativeSpeechLocalOutcome::PreparedSpeech,
        "native transcript finalization requires prepared positive speech"
    );
    preparation
        .validate_source_current(catalog)
        .context("validate current native transcript source before finalization")?;
    let evidence = preparation.evidence();
    let live_plan = NativeTranscriptWindowPlan {
        schema_version: NATIVE_TRANSCRIPT_WINDOW_PLAN_SCHEMA_VERSION.to_string(),
        windows: preparation.windows().to_vec(),
    };
    validate_plan(&live_plan, evidence.source_duration_ms)?;

    let mut connection = catalog.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let policy = validated_checkpoint_policy(&transaction, evidence)?;
    validate_request_matches_policy(&policy, request, &evidence.recipe)?;
    let header = load_transcript_header(&transaction, &evidence.asset_id)?
        .ok_or_else(|| anyhow::anyhow!("native transcript checkpoint is missing"))?;
    ensure!(
        header.recipe_key == evidence.recipe,
        "native transcript checkpoint recipe changed before finalization"
    );
    let scope = validate_asset_scope(&transaction, &policy, &header)?;
    ensure!(
        scope == &evidence.scope,
        "native transcript preparation scope changed before finalization"
    );
    let native_policy_id: Option<String> = transaction
        .query_row(
            "SELECT native_policy_id FROM asset_transcripts WHERE asset_id=?1",
            [&evidence.asset_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    ensure!(
        native_policy_id.as_deref() == Some(policy.policy_id.as_str()),
        "native transcript checkpoint is owned by a different frozen policy"
    );

    let (plan_json, plan_fingerprint): (Option<String>, Option<String>) = transaction.query_row(
        "SELECT native_window_plan_json, native_window_plan_fingerprint
             FROM asset_transcripts WHERE asset_id=?1",
        [&evidence.asset_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let plan_json = plan_json.ok_or_else(|| {
        anyhow::anyhow!("native transcript checkpoint has no retained window plan")
    })?;
    let plan_fingerprint = plan_fingerprint
        .ok_or_else(|| anyhow::anyhow!("native transcript checkpoint has no plan fingerprint"))?;
    let stored_plan = parse_validated_plan(&plan_json, &plan_fingerprint, &policy, scope)?;
    ensure!(
        plan_matches_windows(&stored_plan, preparation.windows()),
        "native transcript checkpoint plan no longer matches the live prepared window map"
    );
    ensure!(
        semantic_fingerprint(&stored_plan, &policy, scope)? == plan_fingerprint,
        "native transcript checkpoint plan identity changed before finalization"
    );

    let is_incomplete_checkpoint = header.status == TranscriptStatus::Failed
        && header.outcome_reason == TranscriptOutcomeReason::NativeCoverageIncomplete;

    let transcript = if is_incomplete_checkpoint {
        finalize_incomplete_checkpoint(
            &transaction,
            preparation,
            request,
            &policy,
            scope,
            &header,
            &stored_plan,
        )?
    } else {
        validate_idempotent_terminal_checkpoint(
            &transaction,
            preparation,
            request,
            &policy,
            scope,
            &header,
            &stored_plan,
        )?
    };
    transaction.commit()?;
    Ok(transcript)
}

fn finalize_incomplete_checkpoint(
    transaction: &Transaction<'_>,
    preparation: &NativeSpeechPreparation,
    request: &NativeSttRequest,
    policy: &RunPolicy,
    scope: &RunAssetScope,
    header: &StoredTranscript,
    plan: &NativeTranscriptWindowPlan,
) -> Result<StoredTranscript> {
    let before = load_checkpoint_snapshot(transaction, &header.asset_id)?;
    ensure!(
        plan_matches_windows(&before.plan, preparation.windows())
            && before.coverage.plan_fingerprint == semantic_fingerprint(plan, policy, scope)?,
        "native transcript checkpoint plan changed while finalizing"
    );
    ensure!(
        before.coverage.planned_window_count as usize == plan.windows.len()
            && before.coverage.completed_window_count as usize == plan.windows.len()
            && before.coverage.remaining_window_count == 0,
        "native transcript finalization requires every planned window to remain completed"
    );
    ensure!(
        before.windows.len() == plan.windows.len()
            && header.window_count as usize == plan.windows.len(),
        "native transcript finalization requires exact planned, completed and linked window counts"
    );

    let mut seen_requests = HashSet::new();
    for (index, window) in before.windows.iter().enumerate() {
        ensure!(
            window.window_index as usize == index,
            "native transcript checkpoint windows are not contiguous and ordered"
        );
        ensure!(
            seen_requests.insert(window.observation.request_key.as_str()),
            "native transcript checkpoint duplicates an accounting request"
        );
        ensure_linked_request_owner(
            transaction,
            &window.observation.request_key,
            &header.asset_id,
        )?;
        let audio = preparation
            .materialize_window(index)
            .with_context(|| format!("materialize prepared native window {index}"))?;
        ensure!(
            audio.window() == &plan.windows[index],
            "native transcript prepared window {index} no longer matches the retained plan"
        );
        validate_current_checkpoint_window(transaction, policy, scope, request, &audio, window)?;
    }

    let transcript =
        assemble_native_transcript_completion(preparation.evidence(), plan, &before.windows)?;
    let retained_candidate_count = if matches!(
        transcript.status,
        TranscriptStatus::Speech | TranscriptStatus::ReviewRequired
    ) {
        before
            .windows
            .iter()
            .map(|window| window.candidates.len())
            .sum::<usize>()
    } else {
        0
    };
    ensure!(
        transcript.window_count as usize == plan.windows.len()
            && transcript.segments.len() == retained_candidate_count,
        "native complete-clip assembly did not preserve exact window coverage"
    );
    let observations: Vec<_> = before
        .windows
        .iter()
        .map(|window| window.observation.clone())
        .collect();
    save_native_transcript_on_transaction(
        transaction,
        &policy.policy_id,
        &transcript,
        &observations,
        Some(NativeTranscriptWindowPlanRow {
            plan_json: &serde_json::to_string(plan)?,
            fingerprint: &before.coverage.plan_fingerprint,
        }),
    )?;
    let saved = load_transcript_header(transaction, &header.asset_id)?
        .ok_or_else(|| anyhow::anyhow!("finalized native transcript disappeared"))?;
    ensure!(
        saved.status == transcript.status
            && saved.outcome_reason == transcript.outcome_reason
            && saved.window_count == transcript.window_count,
        "finalized native transcript does not match the validated assembly"
    );
    Ok(saved)
}

fn validate_idempotent_terminal_checkpoint(
    transaction: &Transaction<'_>,
    preparation: &NativeSpeechPreparation,
    request: &NativeSttRequest,
    policy: &RunPolicy,
    scope: &RunAssetScope,
    header: &StoredTranscript,
    plan: &NativeTranscriptWindowPlan,
) -> Result<StoredTranscript> {
    ensure!(
        matches!(
            header.status,
            TranscriptStatus::Speech
                | TranscriptStatus::NoAcceptedSpeech
                | TranscriptStatus::Rejected
                | TranscriptStatus::ReviewRequired
        ),
        "native transcript finalization cannot replace local, failed or foreign terminal work"
    );
    let provenance = load_native_transcript_provenance(transaction, &header.asset_id)?
        .ok_or_else(|| anyhow::anyhow!("terminal native transcript has no native provenance"))?;
    ensure!(
        provenance.native_policy_id == policy.policy_id
            && provenance.recipe_key == header.recipe_key
            && provenance.native_quality_recipe_version == NATIVE_STT_QUALITY_RECIPE_VERSION
            && provenance.status == header.status
            && provenance.outcome_reason == header.outcome_reason,
        "terminal native transcript identity changed before idempotent finalization"
    );
    let coverage = provenance.coverage.as_ref().ok_or_else(|| {
        anyhow::anyhow!("terminal native transcript has no retained plan coverage")
    })?;
    ensure!(
        coverage.plan_fingerprint == semantic_fingerprint(plan, policy, scope)?
            && coverage.planned_window_count as usize == plan.windows.len()
            && coverage.completed_window_count as usize == plan.windows.len()
            && coverage.remaining_window_count == 0,
        "terminal native transcript plan coverage is not exact and complete"
    );
    ensure!(
        provenance.windows.len() == plan.windows.len()
            && header.window_count as usize == plan.windows.len(),
        "terminal native transcript window counts are not exact"
    );

    let observations: Vec<_> = provenance
        .windows
        .iter()
        .map(|window| NativeSttWindowObservation {
            request_key: window.accounting.request.request_key.clone(),
            latency_ms: window.latency_ms,
            reported_language: window.reported_language.clone(),
            quality: window.quality.clone(),
        })
        .collect();
    validate_transcript_shape(header, &observations)?;

    let retains_candidate_text = matches!(
        header.status,
        TranscriptStatus::Speech | TranscriptStatus::ReviewRequired
    );
    let mut reproved_windows = Vec::new();
    let mut seen_requests = HashSet::new();
    for (index, window) in provenance.windows.iter().enumerate() {
        ensure!(
            window.window_index as usize == index,
            "terminal native transcript windows are not contiguous and ordered"
        );
        ensure!(
            seen_requests.insert(window.accounting.request.request_key.as_str()),
            "terminal native transcript duplicates an accounting request"
        );
        ensure_linked_request_owner(
            transaction,
            &window.accounting.request.request_key,
            &header.asset_id,
        )?;
        let audio = preparation
            .materialize_window(index)
            .with_context(|| format!("materialize prepared native terminal window {index}"))?;
        ensure!(
            audio.window() == &plan.windows[index],
            "terminal native transcript prepared window {index} no longer matches its retained plan"
        );
        let candidates = if retains_candidate_text {
            window.candidates.clone().ok_or_else(|| {
                anyhow::anyhow!("terminal native transcript candidate map is absent")
            })?
        } else {
            Vec::new()
        };
        let checkpoint_window = NativeTranscriptCheckpointWindow {
            window_index: window.window_index,
            observation: observations[index].clone(),
            candidates: candidates.clone(),
        };
        if retains_candidate_text {
            validate_current_checkpoint_window(
                transaction,
                policy,
                scope,
                request,
                &audio,
                &checkpoint_window,
            )?;
            reproved_windows.push(checkpoint_window);
        } else {
            // The terminal clip deliberately discarded all candidates. Prove
            // the exact same plan/payload/accounting/quality facts without
            // fabricating provider text from the paid body.
            validate_current_checkpoint_observation(
                transaction,
                policy,
                scope,
                request,
                &audio,
                &checkpoint_window,
            )?;
        }
    }
    if retains_candidate_text {
        let expected =
            assemble_native_transcript_completion(preparation.evidence(), plan, &reproved_windows)?;
        ensure!(
            header == &expected,
            "terminal native transcript summary disagrees with the exact current assembly"
        );
    } else {
        ensure!(
            header.segments.is_empty() && header.primary_language.is_empty(),
            "terminal native rejection must not retain candidate text or invent a detected language"
        );
        let speech_ms: u64 = plan
            .windows
            .iter()
            .map(|window| u64::from(window.speech_ms()))
            .sum();
        ensure!(
            header.speech_seconds == speech_ms as f64 / 1_000.0,
            "terminal native rejection speech duration disagrees with its measured local plan"
        );
    }
    Ok(header.clone())
}

fn ensure_linked_request_owner(
    connection: &Connection,
    request_key: &str,
    asset_id: &str,
) -> Result<()> {
    let owner: Option<String> = connection
        .query_row(
            "SELECT asset_id FROM native_transcript_windows WHERE request_key=?1",
            [request_key],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(
        owner.as_deref() == Some(asset_id),
        "native transcript accounting request is not owned uniquely by this asset"
    );
    Ok(())
}
