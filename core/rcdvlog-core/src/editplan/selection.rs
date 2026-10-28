//! Optional chapter-wide selection over bounded evidence. Immutable plan rows
//! own intent/profile inputs; the edit plan remains the only timeline.
pub use super::selection_model::*;
use crate::Catalog;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

pub(super) const RECIPE: &str = "chapter-selection.complete-moments.pacing.v1";
pub(super) const MAX_CANDIDATES: usize = 10_000;

pub(crate) fn load_metadata(
    connection: &Connection,
    revision: &str,
) -> Result<Option<SelectionMetadata>> {
    if !crate::migrations::column_exists(connection, "edit_plan_revisions", "selection_json")? {
        return Ok(None);
    }
    let body: Option<String> = connection
        .query_row(
            "SELECT selection_json FROM edit_plan_revisions WHERE revision_id=?1",
            [revision],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    body.map(|s| serde_json::from_str(&s).context("decode versioned selection inputs"))
        .transpose()
}
pub(crate) fn metadata_fingerprint(
    connection: &Connection,
    revision: &str,
) -> Result<Option<String>> {
    load_metadata(connection, revision)?
        .map(|m| Ok(blake3::hash(&serde_json::to_vec(&m)?).to_hex().to_string()))
        .transpose()
}
pub(crate) fn materialize_metadata(
    transaction: &Transaction<'_>,
    project: &str,
    revision: &str,
    metadata: &SelectionMetadata,
) -> Result<()> {
    super::selection_logic::validate_input(&metadata.input)?;
    ensure!(
        metadata.version == 1
            && metadata.recipe == RECIPE
            && metadata.input_identity.len() == 64
            && metadata.assignments.len() <= MAX_CANDIDATES,
        "invalid sequence selection metadata version or bound"
    );
    ensure!(
        serde_json::to_vec(metadata)?.len() <= 16 * 1024 * 1024,
        "sequence selection metadata exceeds storage bound"
    );
    let mut ids = std::collections::BTreeSet::new();
    for assignment in &metadata.assignments {
        ensure!(
            ids.insert(&assignment.operation_id)
                && !assignment.operation_id.is_empty()
                && assignment.operation_id.len() <= 255,
            "invalid duplicate selection assignment identity"
        );
        ensure!(
            metadata
                .input
                .sequences
                .iter()
                .any(|s| s.sequence_id == assignment.sequence_id),
            "selection assignment refers to an unknown sequence"
        );
        let asset_project = transaction.query_row(
            "SELECT project_id FROM assets WHERE id=?1",
            [&assignment.asset_id],
            |r| r.get::<_, String>(0),
        )?;
        ensure!(
            asset_project == project,
            "selection assignment belongs to another project"
        );
        ensure!(
            assignment.chosen_source_in_seconds.is_finite()
                && assignment.chosen_source_out_seconds.is_finite()
                && assignment.chosen_source_in_seconds >= 0.0
                && assignment.chosen_source_out_seconds > assignment.chosen_source_in_seconds,
            "selection assignment source interval is invalid"
        );
    }
    let existing = load_metadata(transaction, revision)?;
    ensure!(
        existing.is_none() || existing.as_ref() == Some(metadata),
        "immutable selection metadata cannot be replaced"
    );
    transaction.execute(
        "UPDATE edit_plan_revisions SET selection_json=?2 WHERE revision_id=?1 AND project_id=?3",
        params![revision, serde_json::to_string(metadata)?, project],
    )?;
    Ok(())
}

impl Catalog {
    pub fn edit_plan_selection_metadata(
        &self,
        revision: &str,
    ) -> Result<Option<SelectionMetadata>> {
        self.edit_plan_revision(revision)?;
        load_metadata(&self.connection()?, revision)
    }
}

impl Catalog {
    /// Read existing local evidence only. No provider, media generation or cache write.
    pub fn preview_sequence_selection(
        &self,
        request: &SequenceSelectionRequest,
    ) -> Result<SelectionProposal> {
        let prepared = super::selection_source::prepare(self, request)?;
        super::require_active(
            &self.connection()?,
            &request.project_id,
            request.expected_active_revision_id.as_deref(),
        )?;
        let mut proposal = if crate::table_exists(&self.connection()?, "edit_selection_cache")? {
            let body:Option<String>=self.connection()?.query_row("SELECT body_json FROM edit_selection_cache WHERE input_identity=?1 AND project_id=?2 AND length(body_json)<=33554432",params![prepared.identity,request.project_id],|r|r.get(0)).optional()?;
            if let Some(body) = body.filter(|b| b.len() <= 32 * 1024 * 1024) {
                let mut p: SelectionProposal = serde_json::from_str(&body)?;
                ensure!(
                    p.metadata.input_identity == prepared.identity
                        && p.metadata.input == request.input,
                    "invalid selection cache identity"
                );
                p.report.reused = true;
                p
            } else {
                super::selection_logic::choose(self, request, &prepared)?
            }
        } else {
            super::selection_logic::choose(self, request, &prepared)?
        };
        // A cache hit is not permission to use changed source or policy state.
        let connection = self.connection()?;
        prepared.require_current(&connection)?;
        ensure!(
            super::ranges::constraints_key(&connection, &prepared.constraint_scope)?
                == prepared.constraints_key,
            "selection constraints changed during planning"
        );
        super::require_active(
            &connection,
            &request.project_id,
            request.expected_active_revision_id.as_deref(),
        )?;
        proposal.report.input_identity = prepared.identity;
        Ok(proposal)
    }
    /// Keep even conflicting finite drafts inspectable. Ordinary validation and
    /// explicit activation remain the publication boundary.
    pub fn create_sequence_selection(
        &self,
        request: &SequenceSelectionRequest,
    ) -> Result<SelectionDraftResult> {
        let prepared = super::selection_source::prepare(self, request)?;
        let proposal = self.preview_sequence_selection(request)?;
        ensure!(
            prepared.identity == proposal.metadata.input_identity,
            "selection evidence changed between planning snapshots"
        );
        self.check_revision_inputs(
            &request.project_id,
            &proposal.operations,
            request.expected_base_revision_id.as_deref(),
        )?;
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        prepared.require_current(&transaction)?;
        ensure!(
            super::ranges::constraints_key(&transaction, &prepared.constraint_scope)?
                == prepared.constraints_key,
            "selection constraints changed before draft creation"
        );
        super::require_active(
            &transaction,
            &request.project_id,
            request.expected_active_revision_id.as_deref(),
        )?;
        let script = request
            .expected_base_revision_id
            .as_ref()
            .map(|id| super::read_revision(&transaction, id))
            .transpose()?
            .and_then(|r| r.script_revision_id);
        let (revision_id, operation) = super::revision_operation_with_selection(
            &transaction,
            &request.project_id,
            &proposal.operations,
            script.as_deref(),
            request.expected_base_revision_id.as_deref(),
            Some(&proposal.metadata),
        )?;
        crate::operations::commit_local_in_transaction(
            &transaction,
            &request.project_id,
            vec![operation],
        )?;
        let body = serde_json::to_string(&proposal)?;
        ensure!(
            body.len() <= 32 * 1024 * 1024,
            "selection cache exceeds bound"
        );
        transaction.execute("INSERT OR IGNORE INTO edit_selection_cache(input_identity,project_id,body_json) VALUES (?1,?2,?3)",params![prepared.identity,request.project_id,body])?;
        transaction.commit()?;
        Ok(SelectionDraftResult {
            revision: self.edit_plan_revision(&revision_id)?,
            report: proposal.report,
            validation: self.validate_edit_plan(&revision_id)?,
        })
    }
    /// Reports are recomputed from this immutable revision, including subsequent
    /// feedback trims/orders inheriting the original versioned selection intent.
    pub fn edit_plan_selection(&self, revision: &str) -> Result<SelectionRevisionView> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let plan = super::read_revision(&transaction, revision)?;
        let schema_ready = crate::migrations::column_exists(
            &transaction,
            "edit_plan_revisions",
            "selection_json",
        )?;
        let metadata = load_metadata(&transaction, revision)?;
        let report = metadata
            .as_ref()
            .map(|m| super::selection_report::report(&plan, m));
        Ok(SelectionRevisionView {
            schema_ready,
            revision_id: revision.into(),
            metadata,
            report,
        })
    }
}

/// Upgrade-only recovery for version-3 plan operations retained by older builds.
/// Applied operations never replay or advance their aggregate twice.
pub(crate) fn redispatch(connection: &mut Connection) -> Result<()> {
    let mut statement=connection.prepare("SELECT operation_id,project_id,origin_node_id,origin_sequence,hlc_wall_millis,hlc_counter,
        aggregate_type,aggregate_id,operation_type,payload_version,base_revision,payload_json FROM project_operations
        WHERE operation_type='editPlanRevisionCreated' AND payload_version=3
        AND base_revision IS NOT NULL AND base_revision>=COALESCE((SELECT revision FROM materialized_revisions mr
            WHERE mr.project_id=project_operations.project_id AND mr.aggregate_type=project_operations.aggregate_type AND mr.aggregate_id=project_operations.aggregate_id),0)
        AND NOT EXISTS(SELECT 1 FROM operation_conflicts c WHERE c.incoming_operation_id=project_operations.operation_id)
        ORDER BY hlc_wall_millis,hlc_counter,origin_node_id,operation_id")?;
    let candidates = statement
        .query_map([], crate::operations::operation_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    if candidates.is_empty() {
        return Ok(());
    }
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    for op in candidates {
        if !crate::operations::revision_guard_passes_without_recording(&transaction, &op)? {
            continue;
        }
        transaction.execute_batch("SAVEPOINT selection_redispatch")?;
        if crate::operations::materialize_edit_plan_revision_created(&transaction, &op).is_err() {
            transaction.execute_batch("ROLLBACK TO selection_redispatch")?;
        }
        transaction.execute_batch("RELEASE selection_redispatch")?;
    }
    transaction.commit()?;
    Ok(())
}
#[cfg(test)]
#[path = "selection_evidence_tests.rs"]
mod evidence_tests;
#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
