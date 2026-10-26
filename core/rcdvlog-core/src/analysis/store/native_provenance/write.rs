//! Atomic native transcript persistence and cross-owner request validation.

use std::collections::HashSet;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::analysis::accounting::ANALYSIS_ACCOUNTING_SCHEMA_VERSION;
use crate::analysis::accounting::store as accounting_store;
use crate::analysis::accounting::store::NativeSettledResultProof;
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
    ensure!(
        transcript.outcome_reason != TranscriptOutcomeReason::NativeCoverageIncomplete,
        "native_coverage_incomplete is written only by the checkpoint owner"
    );
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
        None,
    )?;
    for (index, observation) in observations.iter().enumerate() {
        insert_native_window_observation(&transaction, &transcript.asset_id, index, observation)?;
    }
    transaction.commit()?;
    Ok(())
}

/// Link one exact, already-validated native provider window observation to the
/// transcript owner. The caller owns the surrounding transaction.
pub(super) fn insert_native_window_observation(
    connection: &Connection,
    asset_id: &str,
    window_index: usize,
    observation: &NativeSttWindowObservation,
) -> Result<()> {
    let quality_json = observation.quality.encode_json()?;
    connection
        .execute(
            "INSERT INTO native_transcript_windows
                (asset_id, window_index, request_key, latency_ms, reported_language, quality_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                asset_id,
                window_index as i64,
                observation.request_key,
                observation.latency_ms as i64,
                observation.reported_language,
                quality_json,
            ],
        )
        .context("link native transcript window accounting request")?;
    Ok(())
}

pub(super) fn validate_asset_scope<'a>(
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

        accounting_store::validate_native_settled_result_request(
            connection,
            &observation.request_key,
            &NativeSettledResultProof {
                project_id: &policy.project_id,
                policy_id: &policy.policy_id,
                policy_fingerprint: &policy.policy_id,
                expected_provider_cost_bound_contract: &policy.provider_cost_bound_contract,
                source_fingerprint: &scope.source_identity,
                requested_provider: &policy.provider,
                requested_model: &policy.primary_model,
                run_id: None,
                allowed_operations: &["pilot", AnalysisRunKind::NativeSpeechTranscription.as_str()],
            },
        )?;

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
