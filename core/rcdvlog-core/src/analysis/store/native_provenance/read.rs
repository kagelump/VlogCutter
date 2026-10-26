//! Read-only native provenance projection over the same SQLite ledger.

use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, Transaction};

use crate::analysis::quality::DecodedSegment;

use crate::analysis::accounting::AnalysisAccountingRequestInspection;
use crate::analysis::accounting::store as accounting_store;

use super::super::{TranscriptOutcomeReason, TranscriptStatus};
use super::plan::{parse_validated_plan, reconstruct_candidates_by_window};
use super::types::{
    NATIVE_TRANSCRIPT_PROVENANCE_SCHEMA_VERSION, NativeSttWindowQualityObservation,
    NativeTranscriptCoverage, NativeTranscriptProvenance, NativeTranscriptWindowProvenance,
};

type NativeTranscriptHeaderRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
    i64,
    Option<String>,
    Option<String>,
);

/// Derive the narrow native provenance projection from the canonical transcript
/// header plus the same accounting ledger and immutable receipts.
///
/// The caller owns the SQLite read transaction. Every query below therefore runs
/// against one snapshot: a concurrent local replacement or late receipt cannot
/// produce a torn header/policy/window/accounting projection. Requested policy
/// fields come only from the frozen authorization row; actual
/// provider/model/generation/cost/uncertainty come only from current accounting
/// records and receipts. Missing actual identities remain `None`, and known
/// differences remain visible in the canonical DTO.
pub(crate) fn load_native_transcript_provenance(
    connection: &Transaction<'_>,
    asset_id: &str,
) -> Result<Option<NativeTranscriptProvenance>> {
    let header: Option<NativeTranscriptHeaderRow> = connection
        .query_row(
            "SELECT recipe_key, source_fingerprint, status, outcome_reason,
                    native_policy_id, native_quality_recipe_version,
                    window_count, provider_segment_count,
                    native_window_plan_json, native_window_plan_fingerprint
             FROM asset_transcripts WHERE asset_id=?1",
            [asset_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )
        .optional()?;
    let Some((
        recipe_key,
        source_fingerprint,
        status,
        outcome_reason,
        native_policy_id,
        native_quality_recipe_version,
        window_count,
        provider_segment_count,
        native_window_plan_json,
        native_window_plan_fingerprint,
    )) = header
    else {
        return Ok(None);
    };
    let (native_policy_id, native_quality_recipe_version) =
        match (native_policy_id, native_quality_recipe_version) {
            (None, None) => return Ok(None),
            (Some(policy_id), Some(quality_recipe_version)) => (policy_id, quality_recipe_version),
            _ => anyhow::bail!("native transcript association is incomplete"),
        };
    ensure!(
        !native_policy_id.trim().is_empty()
            && !native_quality_recipe_version.trim().is_empty()
            && native_quality_recipe_version.len() <= 128
            && native_quality_recipe_version.trim() == native_quality_recipe_version,
        "native transcript association is empty or malformed"
    );

    // This loader re-parses policy JSON and recomputes the canonical policy
    // identity, so tampered JSON under an unchanged id is rejected. It does not
    // require the frozen recipe/tariff to equal any current build constant:
    // historical provenance must remain readable.
    let (authorization, _accounting_contract) =
        crate::analysis::run_policy::load_run_authorization_on_connection(
            connection,
            &native_policy_id,
        )?;
    let policy = authorization.policy;
    ensure!(
        policy.policy_id == native_policy_id
            && policy.kind
                == crate::analysis::run_policy::AnalysisRunKind::NativeSpeechTranscription
            && policy.payload == crate::analysis::run_policy::AnalysisPayload::Audio
            && policy.recipe_version == recipe_key,
        "native transcript frozen policy association is inconsistent"
    );
    let scopes: Vec<_> = policy
        .scope
        .iter()
        .filter(|scope| scope.asset_id == asset_id)
        .collect();
    ensure!(
        scopes.len() == 1,
        "native transcript frozen source scope is inconsistent"
    );
    let scope = scopes[0];
    ensure!(
        scope.sampled_representation_fingerprint.as_deref() == Some(source_fingerprint.as_str()),
        "native transcript source fingerprint does not match its frozen policy"
    );

    let plan = match (
        native_window_plan_json.as_deref(),
        native_window_plan_fingerprint.as_deref(),
    ) {
        (None, None) => None,
        (Some(plan_json), Some(fingerprint)) => Some(parse_validated_plan(
            plan_json,
            fingerprint,
            &policy,
            scope,
        )?),
        _ => anyhow::bail!("native transcript window plan association is incomplete"),
    };
    if status == TranscriptStatus::Failed.as_str()
        && outcome_reason == TranscriptOutcomeReason::NativeCoverageIncomplete.as_str()
        && plan.is_none()
    {
        anyhow::bail!("native coverage incomplete transcript is missing its window plan");
    }

    let candidates_by_window = if let Some(plan) = &plan {
        let mut segment_statement = connection.prepare(
            "SELECT start_ms, end_ms, language, text, mean_token_probability
         FROM transcript_segments WHERE asset_id = ?1 ORDER BY segment_index",
        )?;
        let segments = segment_statement
            .query_map([asset_id], |row| {
                Ok(DecodedSegment {
                    start_ms: row.get::<_, u32>(0)?,
                    end_ms: row.get::<_, u32>(1)?,
                    language: row.get(2)?,
                    text: row.get(3)?,
                    mean_token_probability: row.get::<_, Option<f64>>(4)?.map(|value| value as f32),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Some(reconstruct_candidates_by_window(plan, &segments)?)
    } else {
        None
    };

    let mut statement = connection.prepare(
        "SELECT window_index, latency_ms, reported_language, quality_json, request_key
         FROM native_transcript_windows WHERE asset_id=?1 ORDER BY window_index ASC",
    )?;
    let rows = statement
        .query_map([asset_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let completed_window_count = rows.len();
    if let Some(plan) = &plan {
        ensure!(
            completed_window_count <= plan.windows.len(),
            "native transcript checkpoint window rows exceed its plan coverage"
        );
        ensure!(
            window_count == completed_window_count as i64,
            "native transcript checkpoint header window count disagrees with retained window rows"
        );
        ensure!(
            candidates_by_window
                .as_ref()
                .is_some_and(|by_window| by_window
                    .iter()
                    .skip(completed_window_count)
                    .all(Vec::is_empty)),
            "native transcript checkpoint candidates fall outside its completed prefix"
        );
    }

    let mut windows = Vec::with_capacity(rows.len());
    let mut recorded_provider_segments = 0_usize;
    for (
        expected_index,
        (window_index, latency_ms, reported_language, quality_json, request_key),
    ) in rows.into_iter().enumerate()
    {
        let window_index = u32::try_from(window_index)
            .map_err(|_| anyhow::anyhow!("native transcript window index is out of range"))?;
        ensure!(
            window_index as usize == expected_index,
            "native transcript window provenance is not contiguous and ordered"
        );
        let latency_ms = u64::try_from(latency_ms)
            .map_err(|_| anyhow::anyhow!("native transcript latency is out of range"))?;
        super::types::validate_reported_language(&reported_language)?;
        let quality = NativeSttWindowQualityObservation::decode_json(
            &quality_json,
            &native_quality_recipe_version,
        )?;
        let request = accounting_store::load_request(connection, &request_key)?
            .ok_or_else(|| anyhow::anyhow!("native transcript accounting request is missing"))?;
        ensure!(
            request.project_id == policy.project_id
                && request.policy_id == policy.policy_id
                && request.policy_fingerprint == policy.policy_id
                && request.source_fingerprint == scope.source_identity
                && request.provider == policy.provider
                && request.requested_model == policy.primary_model,
            "native transcript accounting request ownership changed"
        );
        let events = accounting_store::events(connection, &request_key)?;
        let receipts = accounting_store::receipts(connection, &request_key)?;
        let attempt_history = accounting_store::attempt_history(connection, &request_key)?;
        recorded_provider_segments = recorded_provider_segments
            .checked_add(quality.metrics.avg_logprob.total_segments)
            .ok_or_else(|| {
                anyhow::anyhow!("native checkpoint provider segment count overflowed")
            })?;
        windows.push(NativeTranscriptWindowProvenance {
            window_index,
            latency_ms,
            reported_language,
            quality,
            accounting: AnalysisAccountingRequestInspection {
                request,
                events,
                receipts,
                attempt_history,
            },
            candidates: candidates_by_window.as_ref().map(|by_window| {
                by_window
                    .get(window_index as usize)
                    .cloned()
                    .unwrap_or_default()
            }),
        });
    }

    if plan.is_some() {
        ensure!(
            usize::try_from(provider_segment_count).ok() == Some(recorded_provider_segments),
            "native checkpoint provider segment count disagrees with recorded window evidence"
        );
    }

    let coverage = plan.as_ref().map(|plan| NativeTranscriptCoverage {
        plan_schema_version: plan.schema_version.clone(),
        plan_fingerprint: native_window_plan_fingerprint
            .clone()
            .expect("a parsed plan always has its stored fingerprint"),
        planned_window_count: plan.windows.len() as u32,
        completed_window_count: completed_window_count as u32,
        remaining_window_count: (plan.windows.len() - completed_window_count) as u32,
    });
    Ok(Some(NativeTranscriptProvenance {
        schema_version: NATIVE_TRANSCRIPT_PROVENANCE_SCHEMA_VERSION.to_string(),
        asset_id: asset_id.to_string(),
        recipe_key,
        native_policy_id,
        native_quality_recipe_version,
        source_fingerprint,
        status: TranscriptStatus::parse(&status)?,
        outcome_reason: TranscriptOutcomeReason::parse(&outcome_reason)?,
        policy,
        windows,
        coverage,
    }))
}

impl crate::Catalog {
    /// Read-only native provenance for one canonical transcript. Returns `None`
    /// for ordinary local/offline transcripts and for assets without a
    /// native-associated header.
    ///
    /// All header, frozen-policy, window and accounting reads run in one SQLite
    /// read transaction. A late receipt changes the next projection without
    /// rewriting the transcript or issuing another provider call.
    pub fn native_transcript_provenance(
        &self,
        asset_id: &str,
    ) -> Result<Option<NativeTranscriptProvenance>> {
        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction()?;
        let provenance = load_native_transcript_provenance(&transaction, asset_id)?;
        transaction.commit()?;
        Ok(provenance)
    }
}
