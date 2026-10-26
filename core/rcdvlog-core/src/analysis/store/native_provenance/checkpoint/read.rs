use super::*;

pub(super) fn load_transcript_header(
    connection: &Connection,
    asset_id: &str,
) -> Result<Option<StoredTranscript>> {
    crate::analysis::store::load_transcript(connection, asset_id)
}

pub(super) fn load_checkpoint_snapshot(
    connection: &Transaction<'_>,
    asset_id: &str,
) -> Result<NativeTranscriptCheckpoint> {
    let provenance = load_native_transcript_provenance(connection, asset_id)?
        .ok_or_else(|| anyhow::anyhow!("native checkpoint transcript disappeared"))?;
    ensure!(
        provenance.native_quality_recipe_version == NATIVE_STT_QUALITY_RECIPE_VERSION,
        "native checkpoint append/reopen requires the current native quality recipe"
    );
    let plan_json: Option<String> = connection
        .query_row(
            "SELECT native_window_plan_json FROM asset_transcripts WHERE asset_id=?1",
            [asset_id],
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
        .find(|scope| scope.asset_id == asset_id)
        .ok_or_else(|| anyhow::anyhow!("native checkpoint frozen source scope is absent"))?;
    let plan = crate::analysis::store::native_provenance::plan::parse_validated_plan(
        &plan_json,
        &provenance
            .coverage
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("native checkpoint coverage is absent"))?
            .plan_fingerprint,
        &provenance.policy,
        scope,
    )?;
    checkpoint_from_provenance(provenance, plan)
}

pub(crate) fn checkpoint_from_provenance(
    provenance: NativeTranscriptProvenance,
    plan: NativeTranscriptWindowPlan,
) -> Result<NativeTranscriptCheckpoint> {
    ensure!(
        provenance.status == TranscriptStatus::Failed
            && provenance.outcome_reason == TranscriptOutcomeReason::NativeCoverageIncomplete,
        "transcript is not an incomplete native checkpoint"
    );
    let coverage = provenance
        .coverage
        .ok_or_else(|| anyhow::anyhow!("native checkpoint is missing its coverage projection"))?;
    ensure!(
        coverage.planned_window_count as usize == plan.windows.len(),
        "native checkpoint coverage disagrees with its validated plan"
    );
    let mut windows = Vec::with_capacity(provenance.windows.len());
    for expected_index in 0..coverage.completed_window_count as usize {
        let window = provenance
            .windows
            .get(expected_index)
            .ok_or_else(|| anyhow::anyhow!("native checkpoint completed prefix is truncated"))?;
        ensure!(
            window.window_index as usize == expected_index,
            "native checkpoint window rows are not a contiguous ordered prefix"
        );
        let candidates = window
            .candidates
            .clone()
            .ok_or_else(|| anyhow::anyhow!("native checkpoint candidate mapping is absent"))?;
        windows.push(NativeTranscriptCheckpointWindow {
            window_index: window.window_index,
            observation: NativeSttWindowObservation {
                request_key: window.accounting.request.request_key.clone(),
                latency_ms: window.latency_ms,
                reported_language: window.reported_language.clone(),
                quality: window.quality.clone(),
            },
            candidates,
        });
    }
    ensure!(
        provenance.windows.len() == coverage.completed_window_count as usize,
        "native checkpoint coverage disagrees with retained window rows"
    );
    Ok(NativeTranscriptCheckpoint {
        asset_id: provenance.asset_id,
        status: provenance.status,
        outcome_reason: provenance.outcome_reason,
        coverage,
        plan,
        windows,
    })
}

/// Read one checkpoint snapshot without requiring today's policy readiness,
/// tariff or quality thresholds. Historical inspection remains possible even
/// when the current build would not authorize the same work.
pub(crate) fn load_native_transcript_checkpoint(
    catalog: &Catalog,
    asset_id: &str,
) -> Result<Option<NativeTranscriptCheckpoint>> {
    let connection = catalog.connection()?;
    let transaction = connection.unchecked_transaction()?;
    let provenance = load_native_transcript_provenance(&transaction, asset_id)?;
    let checkpoint = match provenance {
        Some(provenance) => {
            let plan_json: Option<String> = transaction
                .query_row(
                    "SELECT native_window_plan_json FROM asset_transcripts WHERE asset_id=?1",
                    [asset_id],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            match plan_json {
                Some(plan_json) => {
                    let scope = provenance
                        .policy
                        .scope
                        .iter()
                        .find(|scope| scope.asset_id == asset_id)
                        .ok_or_else(|| {
                            anyhow::anyhow!("native checkpoint frozen source scope is absent")
                        })?;
                    let fingerprint = provenance
                        .coverage
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("native checkpoint coverage is absent"))?
                        .plan_fingerprint
                        .clone();
                    let plan =
                        crate::analysis::store::native_provenance::plan::parse_validated_plan(
                            &plan_json,
                            &fingerprint,
                            &provenance.policy,
                            scope,
                        )?;
                    if provenance.status != TranscriptStatus::Failed
                        || provenance.outcome_reason
                            != TranscriptOutcomeReason::NativeCoverageIncomplete
                    {
                        // A terminal artifact may retain the same plan for exact
                        // currentness/coverage inspection, but this accessor is an
                        // incomplete-checkpoint projection only.
                        None
                    } else {
                        Some(checkpoint_from_provenance(provenance, plan)?)
                    }
                }
                None => None,
            }
        }
        None => None,
    };
    transaction.commit()?;
    Ok(checkpoint)
}
