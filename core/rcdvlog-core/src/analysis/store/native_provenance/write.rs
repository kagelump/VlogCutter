//! Atomic native transcript persistence and cross-owner request validation.

use std::collections::HashSet;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::analysis::accounting::store as accounting_store;
use crate::analysis::accounting::{
    ANALYSIS_ACCOUNTING_SCHEMA_VERSION, AnalysisProviderReceipt, AnalysisRequestLifecycle,
};
use crate::analysis::quality::native_stt::NATIVE_STT_QUALITY_RECIPE_VERSION;
use crate::analysis::run_policy::{
    AnalysisRunKind, RunAssetScope, RunPolicy, load_run_authorization_on_connection,
};

use super::super::{StoredTranscript, TranscriptOutcomeReason, TranscriptStatus};
use super::types::NativeSttWindowObservation;
use super::validation::{validate_native_policy, validate_scope, validate_transcript_shape};

/// Internal native transcript save boundary.
///
/// It validates the frozen native policy, exact asset/source scope, every
/// immutable accounted window request, financial settlement/receipt presence and
/// request ownership inside one SQLite `BEGIN IMMEDIATE` transaction, then
/// replaces the header/segments/native links atomically. It performs no media
/// read, model load, network/provider effect or second financial calculation.
#[allow(dead_code)]
pub(crate) fn save_native_transcript(
    connection: &mut Connection,
    policy_id: &str,
    transcript: &StoredTranscript,
    observations: &[NativeSttWindowObservation],
) -> Result<()> {
    super::super::validate_review_required_contract(transcript)?;
    super::super::validate_probability_evidence(
        transcript.mean_token_probability,
        "native transcript header mean",
    )?;
    for (index, segment) in transcript.segments.iter().enumerate() {
        super::super::validate_probability_evidence(
            segment.mean_token_probability,
            &format!("native transcript segment {index} mean"),
        )?;
    }
    validate_transcript_shape(transcript, observations)?;
    ensure!(
        !policy_id.trim().is_empty() && policy_id.trim() == policy_id,
        "native transcript save requires a canonical non-empty policy identity"
    );
    for (index, observation) in observations.iter().enumerate() {
        observation
            .validate()
            .with_context(|| format!("validate native window observation {index}"))?;
    }

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let (authorization, accounting_contract) =
        load_run_authorization_on_connection(&transaction, policy_id)?;
    ensure!(
        accounting_contract == ANALYSIS_ACCOUNTING_SCHEMA_VERSION,
        "native transcript frozen policy was not authorized under the current accounting contract"
    );
    let policy = &authorization.policy;
    ensure!(
        policy.policy_id == policy_id,
        "native transcript authorization policy identity changed"
    );
    validate_native_policy(policy, transcript)?;

    let scope = validate_asset_scope(&transaction, policy, transcript)?;
    validate_request_windows(&transaction, policy, scope, transcript, observations)?;

    super::super::write_transcript_rows(
        &transaction,
        transcript,
        Some(policy_id),
        Some(NATIVE_STT_QUALITY_RECIPE_VERSION),
    )?;
    for (index, observation) in observations.iter().enumerate() {
        let quality_json = observation.quality.encode_json()?;
        transaction
            .execute(
                "INSERT INTO native_transcript_windows
                    (asset_id, window_index, request_key, latency_ms, reported_language, quality_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    transcript.asset_id,
                    index as i64,
                    observation.request_key,
                    observation.latency_ms as i64,
                    observation.reported_language,
                    quality_json,
                ],
            )
            .context("link native transcript window accounting request")?;
    }
    transaction.commit()?;
    Ok(())
}

fn validate_asset_scope<'a>(
    connection: &Connection,
    policy: &'a RunPolicy,
    transcript: &StoredTranscript,
) -> Result<&'a RunAssetScope> {
    let matching_scopes: Vec<_> = policy
        .scope
        .iter()
        .filter(|scope| scope.asset_id == transcript.asset_id)
        .collect();
    ensure!(
        matching_scopes.len() == 1,
        "native transcript asset must have exactly one frozen policy scope entry"
    );
    let scope = matching_scopes[0];

    let removed = crate::asset_column_expression(connection, "removed", "removed", "0")?;
    let membership_removed = crate::asset_column_expression(
        connection,
        "membership_removed",
        "membership_removed",
        "0",
    )?;
    let sql = format!(
        "SELECT project_id, fingerprint, duration_seconds,
                COALESCE({removed},0), COALESCE({membership_removed},0)
         FROM assets WHERE id=?1"
    );
    let row: Option<(String, String, f64, i64, i64)> = connection
        .query_row(&sql, [&transcript.asset_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .optional()?;
    let (asset_project, asset_fingerprint, duration_seconds, removed, membership_removed) =
        row.ok_or_else(|| anyhow::anyhow!("native transcript asset does not exist"))?;
    ensure!(
        asset_project == policy.project_id && removed == 0 && membership_removed == 0,
        "native transcript asset is excluded or belongs to another project"
    );
    ensure!(
        duration_seconds.is_finite() && duration_seconds >= 0.0,
        "native transcript asset duration is invalid"
    );
    let duration_ms = (duration_seconds * 1_000.0).round();
    ensure!(
        duration_ms.is_finite() && duration_ms <= u32::MAX as f64,
        "native transcript asset duration is not representable in source milliseconds"
    );
    ensure!(
        scope.source_duration_ms == duration_ms as u32,
        "native transcript approved source duration is stale"
    );
    validate_scope(scope, &asset_fingerprint, duration_ms as u32, transcript)?;
    Ok(scope)
}

fn validate_request_windows(
    connection: &Connection,
    policy: &RunPolicy,
    scope: &RunAssetScope,
    transcript: &StoredTranscript,
    observations: &[NativeSttWindowObservation],
) -> Result<()> {
    let mut seen_requests = HashSet::new();
    for (index, observation) in observations.iter().enumerate() {
        ensure!(
            seen_requests.insert(observation.request_key.clone()),
            "native transcript observation {index} duplicates an accounting request"
        );
        let existing_owner: Option<String> = connection
            .query_row(
                "SELECT asset_id FROM native_transcript_windows WHERE request_key=?1",
                [&observation.request_key],
                |row| row.get(0),
            )
            .optional()?;
        ensure!(
            existing_owner
                .as_deref()
                .is_none_or(|owner| owner == transcript.asset_id),
            "native transcript observation {index} accounting request is already linked to another asset"
        );

        let request = accounting_store::load_request(connection, &observation.request_key)?
            .ok_or_else(|| anyhow::anyhow!("native transcript accounting request was not found"))?;
        ensure!(
            request.project_id == policy.project_id
                && request.policy_id == policy.policy_id
                && request.policy_fingerprint == policy.policy_id,
            "native transcript accounting request policy/project ownership changed"
        );
        ensure!(
            request.operation == "pilot"
                || request.operation == AnalysisRunKind::NativeSpeechTranscription.as_str(),
            "native transcript accounting request is not an explicit native speech operation"
        );
        ensure!(
            request.source_fingerprint == scope.source_identity
                && request.provider == policy.provider
                && request.requested_model == policy.primary_model,
            "native transcript accounting request source/provider/model does not match the frozen policy"
        );
        ensure!(
            !request.payload_fingerprint.trim().is_empty()
                && !request.request_fingerprint.trim().is_empty()
                && !request.cost_bound_fingerprint.trim().is_empty(),
            "native transcript accounting request identity proof is incomplete"
        );
        ensure!(
            request.dispatch_at.is_some()
                && !matches!(
                    request.lifecycle,
                    AnalysisRequestLifecycle::Reserved
                        | AnalysisRequestLifecycle::Dispatched
                        | AnalysisRequestLifecycle::Released
                ),
            "native transcript cannot link an undispatched, active or released accounting request"
        );
        let receipts: Vec<AnalysisProviderReceipt> =
            accounting_store::receipts(connection, &observation.request_key)?;
        ensure!(
            !receipts.is_empty(),
            "native transcript accounting request has no immutable provider receipt"
        );
        let attempt_history =
            accounting_store::attempt_history(connection, &observation.request_key)?;
        ensure!(
            attempt_history
                .iter()
                .any(|attempt| attempt.request_key == request.request_key),
            "native transcript accounting request is absent from its immutable attempt history"
        );
        match request.lifecycle {
            AnalysisRequestLifecycle::Completed => {
                ensure!(
                    request.settled_at.is_some()
                        && request.confirmed_cost_usd.is_finite()
                        && request.confirmed_cost_usd >= 0.0,
                    "completed native transcript request is missing its settled receipt boundary"
                );
            }
            AnalysisRequestLifecycle::Uncertain => {
                ensure!(
                    request.uncertain_usage_reserve_usd.is_finite()
                        && request.uncertain_usage_reserve_usd > 0.0,
                    "uncertain native transcript request has no honest unresolved-usage reserve"
                );
            }
            _ => anyhow::bail!(
                "native transcript requires a settled completed or honestly uncertain accounting request"
            ),
        }
        let cost_bound = request.cost_bound.as_ref().ok_or_else(|| {
            anyhow::anyhow!("native transcript accounting request has no cost bound")
        })?;
        cost_bound.validate()?;
        ensure!(
            cost_bound.audio.is_some()
                && cost_bound.contract_version == policy.provider_cost_bound_contract,
            "native transcript accounting request is not bound to the canonical native audio cost family"
        );
        ensure!(
            !request.run_id.trim().is_empty(),
            "native transcript accounting request has no owning accounted run"
        );
        let run: Option<(String, String, String)> = connection
            .query_row(
                "SELECT project_id,policy_id,accounting_contract FROM analysis_runs WHERE id=?1",
                [&request.run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (run_project, run_policy, run_contract) =
            run.ok_or_else(|| anyhow::anyhow!("native transcript accounting run does not exist"))?;
        ensure!(
            run_project == policy.project_id
                && run_policy == policy.policy_id
                && run_contract == ANALYSIS_ACCOUNTING_SCHEMA_VERSION,
            "native transcript accounting run ownership or contract does not match the frozen policy"
        );

        // Explicitly keep silent/no-audio artifacts request-free and prevent a
        // provider-backed artifact from carrying a fabricated request count.
        if transcript.status == TranscriptStatus::NoAudio
            || transcript.status == TranscriptStatus::NoSpeech
        {
            anyhow::bail!("verified silence cannot link provider request observations");
        }
        if transcript.outcome_reason == TranscriptOutcomeReason::LegacyNoSpeechUnverified {
            anyhow::bail!("legacy unverified silence cannot claim native provider provenance");
        }
    }
    Ok(())
}
