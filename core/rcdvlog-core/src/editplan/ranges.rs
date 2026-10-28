//! One authoritative operation-backed owner for confirmed source ranges.
use super::range_model::*;
use crate::operations::{
    NewOperation, SOURCE_RANGE_CONFIRMED, SOURCE_RANGE_REVOKED, advance_revision,
    commit_local_in_transaction, current_revision, revision_guard_passes,
};
use crate::{ApplyOperationResult, Catalog, ProjectOperation};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetPayload {
    constraint_id: String,
    asset_id: String,
    source_fingerprint: String,
    decision: RangeDecision,
    media: RangeMedia,
    decision_owner: RangeDecisionOwner,
    source_in_seconds: f64,
    source_out_seconds: f64,
    rationale: String,
    inspection: Option<ConfirmedRangeInspection>,
    overrides_evidence_ids: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RevokePayload {
    constraint_id: String,
    asset_id: String,
    rationale: String,
}

impl Catalog {
    /// Explicit caller assertion of a human decision. Derived observations and
    /// provider replies have no route into this method through inspection.
    pub fn confirm_source_range(
        &self,
        request: &ConfirmSourceRangeRequest,
    ) -> Result<SourceRangeConstraints> {
        validate_span(request.source_in_seconds, request.source_out_seconds)?;
        validate_rationale(&request.rationale)?;
        let asset = self.get_asset(&request.asset_id)?;
        if asset.fingerprint != request.expected_source_fingerprint {
            return Err(SourceRangeError::StaleSource.into());
        }
        let inspection=request.inspected_evidence_id.as_deref().map(|id| {
            let report=self.require_current_source_inspection(id,&asset.id)?;
            ensure!(request.source_in_seconds>=report.window_in_seconds && request.source_out_seconds<=report.window_out_seconds,
                "invalid input: confirmed range exceeds inspected window; inspect more context or explicitly record a manual decision without this evidence");
            Ok::<_,anyhow::Error>(ConfirmedRangeInspection {evidence_id:report.evidence_id,representation_fingerprint:report.representation_fingerprint,
                recipe:report.recipe,rotation_degrees:report.rotation_degrees,mapping:report.mapping,
                purpose:report.request.purpose,included_audio:report.request.include_audio,included_transcript:report.request.include_transcript,
                window_in_seconds:report.window_in_seconds,window_out_seconds:report.window_out_seconds,
                sample_count:report.frames.len() as u32,max_unsampled_gap_seconds:report.max_unsampled_gap_seconds})
        }).transpose()?;
        let payload = SetPayload {
            constraint_id: request
                .constraint_id
                .clone()
                .unwrap_or_else(|| format!("range_{}", uuid::Uuid::new_v4().simple())),
            asset_id: asset.id.clone(),
            source_fingerprint: asset.fingerprint.clone(),
            decision: request.decision,
            media: request.media,
            decision_owner: request.decision_owner,
            source_in_seconds: request.source_in_seconds,
            source_out_seconds: request.source_out_seconds,
            rationale: request.rationale.clone(),
            inspection,
            overrides_evidence_ids: request.overrides_evidence_ids.clone(),
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_revision(
            &transaction,
            &asset.project_id,
            &asset.id,
            request.expected_revision,
        )?;
        commit_local_in_transaction(
            &transaction,
            &asset.project_id,
            vec![NewOperation {
                aggregate_type: "sourceRanges",
                aggregate_id: asset.id.clone(),
                operation_type: SOURCE_RANGE_CONFIRMED,
                base_revision: Some(request.expected_revision),
                payload: serde_json::to_value(payload)?,
            }],
        )?;
        transaction.commit()?;
        self.source_range_constraints(&asset.id, 0, 100)
    }

    pub fn revoke_source_range(
        &self,
        asset_id: &str,
        constraint_id: &str,
        expected_revision: u64,
        rationale: &str,
    ) -> Result<SourceRangeConstraints> {
        let asset = self.get_asset(asset_id)?;
        validate_rationale(rationale)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_revision(&transaction, &asset.project_id, asset_id, expected_revision)?;
        commit_local_in_transaction(
            &transaction,
            &asset.project_id,
            vec![NewOperation {
                aggregate_type: "sourceRanges",
                aggregate_id: asset_id.into(),
                operation_type: SOURCE_RANGE_REVOKED,
                base_revision: Some(expected_revision),
                payload: serde_json::to_value(RevokePayload {
                    constraint_id: constraint_id.into(),
                    asset_id: asset_id.into(),
                    rationale: rationale.into(),
                })?,
            }],
        )?;
        transaction.commit()?;
        self.source_range_constraints(asset_id, 0, 100)
    }

    /// SQLite-only read: no migrations, media probing or cache generation.
    pub fn source_range_constraints(
        &self,
        asset_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<SourceRangeConstraints> {
        ensure!(
            (1..=200).contains(&limit),
            "invalid input: range page limit must be 1-200"
        );
        let asset = self.get_asset(asset_id)?;
        let connection = self.connection()?;
        // Records and their compare-and-swap revision must describe one WAL
        // snapshot; otherwise a writer between these reads could disguise a
        // stale decision as having observed the new revision.
        let connection = connection.unchecked_transaction()?;
        let schema_ready = crate::table_exists(&connection, "confirmed_source_ranges")?;
        let records = load_for_asset(&connection, asset_id)?;
        let revision = current_revision(&connection, &asset.project_id, "sourceRanges", asset_id)?;
        let (feasible_picture_ranges, picture_conflicts) = feasible_ranges(
            &records,
            &asset.fingerprint,
            asset.duration_seconds,
            TrimMedia::Picture,
        );
        let (feasible_audio_ranges, audio_conflicts) = feasible_ranges(
            &records,
            &asset.fingerprint,
            asset.duration_seconds,
            TrimMedia::Audio,
        );
        let total = records.len();
        Ok(SourceRangeConstraints {
            schema_ready,
            asset_id: asset.id,
            source_fingerprint: asset.fingerprint,
            revision,
            items: records.into_iter().skip(offset).take(limit).collect(),
            total_items: total,
            next_offset: (offset.saturating_add(limit) < total)
                .then_some(offset.saturating_add(limit)),
            feasible_picture_ranges,
            feasible_audio_ranges,
            picture_conflicts,
            audio_conflicts,
        })
    }

    /// Segment timing is advisory. Expansion to complete utterances is allowed
    /// only inside both confirmed clean ranges and the actually inspected window.
    pub fn suggest_source_trim(&self, request: &SourceTrimRequest) -> Result<SourceTrimSuggestion> {
        validate_span(request.source_in_seconds, request.source_out_seconds)?;
        let report = self
            .require_current_source_inspection(&request.inspected_evidence_id, &request.asset_id)?;
        ensure!(
            request.source_in_seconds >= report.window_in_seconds
                && request.source_out_seconds <= report.window_out_seconds,
            "invalid input: proposed trim exceeds inspected window"
        );
        let ranges = self.source_range_constraints(&request.asset_id, 0, 1)?;
        let window = SourceTimeRange {
            source_in_seconds: report.window_in_seconds,
            source_out_seconds: report.window_out_seconds,
        };
        let feasible = match request.media {
            TrimMedia::Picture => &ranges.feasible_picture_ranges,
            TrimMedia::Audio => &ranges.feasible_audio_ranges,
        };
        let clean: Vec<_> = feasible
            .iter()
            .filter_map(|r| intersection(*r, window))
            .collect();
        let proposed = SourceTimeRange {
            source_in_seconds: request.source_in_seconds,
            source_out_seconds: request.source_out_seconds,
        };
        let mut suggestions: Vec<_> = clean
            .iter()
            .filter_map(|r| intersection(*r, proposed))
            .collect();
        let mut conflicts = match request.media {
            TrimMedia::Picture => ranges.picture_conflicts,
            TrimMedia::Audio => ranges.audio_conflicts,
        };
        if suggestions.is_empty() {
            conflicts.push("No confirmed clean range overlaps the proposed trim.".into())
        }
        if request.prefer_utterances {
            let overlapping: Vec<_> = report
                .utterances
                .iter()
                .filter(|u| {
                    u.source_in_seconds < proposed.source_out_seconds
                        && u.source_out_seconds > proposed.source_in_seconds
                })
                .collect();
            if overlapping.is_empty() {
                conflicts.push(
                    "No current accepted utterance timing is available for this proposal.".into(),
                );
            } else {
                let mut complete = Vec::new();
                for utterance in overlapping {
                    let full = SourceTimeRange {
                        source_in_seconds: utterance.source_in_seconds,
                        source_out_seconds: utterance.source_out_seconds,
                    };
                    if !utterance.clipped_by_window && clean.iter().any(|r| contains(*r, full)) {
                        complete.push(full);
                    } else {
                        conflicts.push("A whole utterance crosses an exclusion or the inspected window; inspect more context or choose a deliberate fragment after listening.".into());
                    }
                }
                // Refuse to relabel a clipped fragment as a complete utterance.
                suggestions = merge(complete);
            }
        }
        Ok(SourceTrimSuggestion {inspected_evidence_id:report.evidence_id,media:request.media,constraints_revision:ranges.revision,
            clean_ranges:clean,proposed_ranges:suggestions,conflicts,
            limitations:vec!["Suggestions do not accept a trim or change constraints.".into(),
                "Segment timing is not word alignment or source-frame accuracy; inspect mapped endpoint frames and listen before acceptance.".into()],listening_required:request.prefer_utterances})
    }
}

fn require_revision(
    connection: &Connection,
    project: &str,
    asset: &str,
    expected: u64,
) -> Result<()> {
    let actual = current_revision(connection, project, "sourceRanges", asset)?;
    if actual != expected {
        return Err(SourceRangeError::StaleRevision { expected, actual }.into());
    }
    Ok(())
}

/// Apply facts an older build retained but could not understand. Applied or
/// superseded facts fail their base guard and are skipped without new conflicts.
pub(crate) fn redispatch(connection: &mut Connection) -> Result<()> {
    let mut statement=connection.prepare(
        "SELECT operation_id,project_id,origin_node_id,origin_sequence,hlc_wall_millis,hlc_counter,
            aggregate_type,aggregate_id,operation_type,payload_version,base_revision,payload_json
         FROM project_operations WHERE operation_type IN ('sourceRangeConfirmed','sourceRangeRevoked')
            AND payload_version=1 AND base_revision IS NOT NULL
            AND base_revision >= COALESCE((SELECT revision FROM materialized_revisions mr
                WHERE mr.project_id=project_operations.project_id AND mr.aggregate_type=project_operations.aggregate_type
                  AND mr.aggregate_id=project_operations.aggregate_id),0)
            AND NOT EXISTS (SELECT 1 FROM operation_conflicts c WHERE c.incoming_operation_id=project_operations.operation_id)
         ORDER BY hlc_wall_millis,hlc_counter,origin_node_id,operation_id"
    )?;
    let candidates = statement
        .query_map([], crate::operations::operation_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    if candidates.is_empty() {
        return Ok(());
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for op in candidates {
        if !crate::operations::revision_guard_passes_without_recording(&transaction, &op)? {
            continue;
        }
        transaction.execute_batch("SAVEPOINT edit_review_redispatch")?;
        if materialize_range_decision(&transaction, &op).is_err() {
            transaction.execute_batch("ROLLBACK TO edit_review_redispatch")?;
        }
        transaction.execute_batch("RELEASE edit_review_redispatch")?;
    }
    transaction.commit()?;
    Ok(())
}

pub(crate) fn load_for_asset(
    connection: &Connection,
    asset: &str,
) -> Result<Vec<ConfirmedSourceRange>> {
    if !crate::table_exists(connection, "confirmed_source_ranges")? {
        return Ok(vec![]);
    }
    let mut statement = connection.prepare(
        "SELECT body_json FROM confirmed_source_ranges WHERE asset_id=?1 ORDER BY constraint_id",
    )?;
    let bodies = statement
        .query_map([asset], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    bodies
        .into_iter()
        .map(|s| serde_json::from_str(&s).context("decode confirmed source range"))
        .collect()
}

pub(crate) fn load_for_project(
    connection: &Connection,
    project: &str,
) -> Result<std::collections::HashMap<String, Vec<ConfirmedSourceRange>>> {
    let mut result = std::collections::HashMap::<String, Vec<ConfirmedSourceRange>>::new();
    if !crate::table_exists(connection, "confirmed_source_ranges")? {
        return Ok(result);
    }
    let mut statement=connection.prepare("SELECT body_json FROM confirmed_source_ranges WHERE project_id=?1 AND active=1 ORDER BY constraint_id")?;
    for row in statement.query_map([project], |r| r.get::<_, String>(0))? {
        let record: ConfirmedSourceRange = serde_json::from_str(&row?)?;
        result
            .entry(record.asset_id.clone())
            .or_default()
            .push(record);
    }
    Ok(result)
}

pub(crate) fn constraints_key(
    connection: &Connection,
    plan: &super::EditPlanRevision,
) -> Result<String> {
    let mut assets = std::collections::BTreeSet::new();
    for cut in &plan.operations {
        assets.insert(cut.asset_id.as_str());
        if let Some(super::EditPlanAudio::Source(audio)) = &cut.audio {
            assets.insert(audio.asset_id.as_str());
        }
    }
    let mut statement = connection.prepare(
        "SELECT aggregate_id,revision,last_operation_id FROM materialized_revisions
        WHERE project_id=?1 AND aggregate_type='sourceRanges' ORDER BY aggregate_id",
    )?;
    let rows = statement
        .query_map([&plan.project_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|r| assets.contains(r.0.as_str()))
        .collect();
    let ranges_key = blake3::hash(&serde_json::to_vec(&rows)?)
        .to_hex()
        .to_string();
    Ok(
        if let Some(hard_choices) = super::feedback::hard_choice_key(connection, &plan.project_id)?
        {
            blake3::hash(&serde_json::to_vec(&(ranges_key, hard_choices))?)
                .to_hex()
                .to_string()
        } else {
            ranges_key
        },
    )
}
fn load_one(connection: &Connection, id: &str) -> Result<Option<ConfirmedSourceRange>> {
    let body: Option<String> = connection
        .query_row(
            "SELECT body_json FROM confirmed_source_ranges WHERE constraint_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    body.map(|s| serde_json::from_str(&s).context("decode confirmed source range"))
        .transpose()
}

pub(crate) fn materialize_range_decision(
    transaction: &Transaction<'_>,
    operation: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    ensure!(
        operation.aggregate_type == "sourceRanges"
            && operation.base_revision.is_some()
            && operation.payload_version == 1,
        "source range decisions require a version-1 sourceRanges aggregate with an explicit base revision"
    );
    let mut record = if operation.operation_type == SOURCE_RANGE_CONFIRMED {
        let p: SetPayload = serde_json::from_value(operation.payload.clone())?;
        ensure!(
            p.asset_id == operation.aggregate_id,
            "source range asset does not match aggregate"
        );
        validate_id(&p.constraint_id)?;
        validate_span(p.source_in_seconds, p.source_out_seconds)?;
        validate_rationale(&p.rationale)?;
        ensure!(
            p.overrides_evidence_ids.len() <= 64,
            "invalid input: maximum 64 explicit evidence overrides"
        );
        for id in &p.overrides_evidence_ids {
            validate_id(id)?;
        }
        let (fingerprint, duration): (String, f64) = transaction
            .query_row(
                "SELECT fingerprint,duration_seconds FROM assets WHERE id=?1 AND project_id=?2",
                params![p.asset_id, operation.project_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .context("source range asset not found in project")?;
        ensure!(
            p.source_fingerprint == fingerprint,
            "source range source fingerprint does not match asset"
        );
        ensure!(
            duration.is_finite() && p.source_out_seconds <= duration,
            "invalid input: confirmed range exceeds known source duration"
        );
        if let Some(old) = load_one(transaction, &p.constraint_id)? {
            ensure!(
                old.asset_id == p.asset_id
                    && old.project_id == operation.project_id
                    && old.source_fingerprint == p.source_fingerprint,
                "invalid input: constraint identity cannot be moved to another source"
            );
        }
        if let Some(e) = &p.inspection {
            validate_id(&e.evidence_id)?;
            validate_span(e.window_in_seconds, e.window_out_seconds)?;
            ensure!(
                e.window_in_seconds <= p.source_in_seconds
                    && e.window_out_seconds >= p.source_out_seconds
                    && e.window_out_seconds <= duration,
                "invalid input: constraint exceeds declared inspection window"
            );
            ensure!(
                !e.representation_fingerprint.is_empty()
                    && !e.recipe.is_empty()
                    && e.sample_count > 0
                    && e.sample_count <= 64
                    && e.max_unsampled_gap_seconds.is_finite()
                    && e.max_unsampled_gap_seconds >= 0.0
                    && matches!(e.rotation_degrees, 0 | 90 | 180 | 270)
                    && e.mapping.time_base_numerator > 0
                    && e.mapping.time_base_denominator > 0
                    && e.mapping.source_offset_seconds.is_finite()
                    && e.mapping.representation_offset_seconds.is_finite()
                    && e.mapping.seconds_scale > 0,
                "invalid input: malformed inspection provenance"
            );
        }
        let count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM confirmed_source_ranges WHERE asset_id=?1",
            [&p.asset_id],
            |r| r.get(0),
        )?;
        ensure!(
            count < 1024 || load_one(transaction, &p.constraint_id)?.is_some(),
            "invalid input: maximum 1024 human range records per asset"
        );
        ConfirmedSourceRange {
            constraint_id: p.constraint_id,
            project_id: operation.project_id.clone(),
            asset_id: p.asset_id,
            source_fingerprint: p.source_fingerprint,
            decision: p.decision,
            media: p.media,
            decision_owner: p.decision_owner,
            source_in_seconds: p.source_in_seconds,
            source_out_seconds: p.source_out_seconds,
            rationale: p.rationale,
            inspection: p.inspection,
            overrides_evidence_ids: p.overrides_evidence_ids,
            active: true,
            last_operation_id: String::new(),
            updated_at_millis: 0,
        }
    } else {
        ensure!(
            operation.operation_type == SOURCE_RANGE_REVOKED,
            "invalid range decision operation"
        );
        let p: RevokePayload = serde_json::from_value(operation.payload.clone())?;
        validate_rationale(&p.rationale)?;
        let mut old = load_one(transaction, &p.constraint_id)?
            .context("source range constraint not found")?;
        ensure!(
            old.project_id == operation.project_id
                && old.asset_id == operation.aggregate_id
                && old.asset_id == p.asset_id,
            "invalid input: constraint belongs to another source/project"
        );
        old.active = false;
        old.rationale = p.rationale;
        old
    };
    if !revision_guard_passes(transaction, operation)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    record.last_operation_id = operation.operation_id.clone();
    record.updated_at_millis = operation.logical_time.wall_millis;
    transaction.execute("INSERT INTO confirmed_source_ranges(constraint_id,project_id,asset_id,body_json,active) VALUES(?1,?2,?3,?4,?5)
        ON CONFLICT(constraint_id) DO UPDATE SET body_json=excluded.body_json,active=excluded.active",
        params![record.constraint_id,record.project_id,record.asset_id,serde_json::to_string(&record)?,record.active])?;
    advance_revision(transaction, operation)?;
    Ok(ApplyOperationResult::Applied)
}

fn validate_id(s: &str) -> Result<()> {
    ensure!(
        !s.is_empty() && s.len() <= 255 && !s.chars().any(|c| c.is_whitespace() || c.is_control()),
        "invalid input: invalid evidence/constraint ID"
    );
    Ok(())
}
fn validate_rationale(s: &str) -> Result<()> {
    ensure!(
        !s.trim().is_empty() && s.len() <= 4096,
        "invalid input: human rationale required (maximum 4096 bytes)"
    );
    Ok(())
}
fn validate_span(a: f64, b: f64) -> Result<()> {
    ensure!(
        a.is_finite() && b.is_finite() && a >= 0.0 && b > a,
        "invalid input: range must be finite, positive and ordered"
    );
    Ok(())
}

pub(crate) fn feasible_ranges(
    records: &[ConfirmedSourceRange],
    fingerprint: &str,
    duration: f64,
    media: TrimMedia,
) -> (Vec<SourceTimeRange>, Vec<String>) {
    let mut conflicts = Vec::new();
    let active: Vec<_> = records
        .iter()
        .filter(|r| {
            r.active
                && (r.media == RangeMedia::Both
                    || matches!(
                        (r.media, media),
                        (RangeMedia::Picture, TrimMedia::Picture)
                            | (RangeMedia::Audio, TrimMedia::Audio)
                    ))
        })
        .collect();
    if !duration.is_finite() || duration <= 0.0 {
        return (vec![], vec!["Source duration is unknown.".into()]);
    }
    if active
        .iter()
        .any(|r| r.source_fingerprint != fingerprint || r.source_out_seconds > duration)
    {
        return (vec![],vec!["Confirmed constraints describe changed source media/duration; explicit human reconciliation required.".into()]);
    }
    let mut allowed: Vec<_> = active
        .iter()
        .filter(|r| r.decision == RangeDecision::Allow)
        .map(|r| SourceTimeRange {
            source_in_seconds: r.source_in_seconds,
            source_out_seconds: r.source_out_seconds,
        })
        .collect();
    if allowed.is_empty() {
        allowed.push(SourceTimeRange {
            source_in_seconds: 0.0,
            source_out_seconds: duration,
        })
    }
    let mut clean = merge(allowed);
    for exclusion in active
        .iter()
        .filter(|r| r.decision == RangeDecision::Exclude)
    {
        let mut remaining = Vec::new();
        for r in clean {
            let a = exclusion.source_in_seconds;
            let b = exclusion.source_out_seconds;
            if b <= r.source_in_seconds || a >= r.source_out_seconds {
                remaining.push(r);
                continue;
            }
            if a > r.source_in_seconds {
                remaining.push(SourceTimeRange {
                    source_in_seconds: r.source_in_seconds,
                    source_out_seconds: a,
                })
            }
            if b < r.source_out_seconds {
                remaining.push(SourceTimeRange {
                    source_in_seconds: b,
                    source_out_seconds: r.source_out_seconds,
                })
            }
        }
        clean = remaining;
    }
    if clean.is_empty() {
        conflicts.push("Confirmed ranges leave no allowed source interval.".into())
    }
    (clean, conflicts)
}
fn merge(mut ranges: Vec<SourceTimeRange>) -> Vec<SourceTimeRange> {
    ranges.sort_by(|a, b| a.source_in_seconds.total_cmp(&b.source_in_seconds));
    let mut merged: Vec<SourceTimeRange> = Vec::new();
    for r in ranges {
        if let Some(last) = merged.last_mut()
            && r.source_in_seconds <= last.source_out_seconds
        {
            last.source_out_seconds = last.source_out_seconds.max(r.source_out_seconds)
        } else {
            merged.push(r)
        }
    }
    merged
}
pub(crate) fn contains(a: SourceTimeRange, b: SourceTimeRange) -> bool {
    b.source_in_seconds >= a.source_in_seconds && b.source_out_seconds <= a.source_out_seconds
}
fn intersection(a: SourceTimeRange, b: SourceTimeRange) -> Option<SourceTimeRange> {
    let start = a.source_in_seconds.max(b.source_in_seconds);
    let end = a.source_out_seconds.min(b.source_out_seconds);
    (end > start).then_some(SourceTimeRange {
        source_in_seconds: start,
        source_out_seconds: end,
    })
}

#[cfg(test)]
#[path = "ranges_tests.rs"]
mod tests;
