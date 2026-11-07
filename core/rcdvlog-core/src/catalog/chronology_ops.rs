//! Chronology facts, solving, suggestions, and projection into the catalog.

use crate::analysis::accounting::{
    begin_accounted_operation, operation_intended_attempt_key, operation_policy_fingerprint,
};
use crate::*;
use anyhow::{Result, bail};
use rusqlite::{Connection, TransactionBehavior, params};

impl Catalog {
    /// Loads the complete persisted chronology evidence for one project
    /// (D01-T007). Every collection is returned in deterministic order, sorted
    /// by primary key, so the solver cannot inherit nondeterminism from the
    /// loader. The facts are evidence only: derived values belong to the
    /// solver (D01-T008).
    pub fn chronology_facts(&self, project_id: &str) -> Result<ChronologyFacts> {
        self.require_project(project_id)?;
        let connection = self.connection()?;
        chronology::load_chronology_facts(&connection, project_id)
    }

    /// Solves this project's chronology (D01-T008). Loading is I/O; solving is
    /// pure, so identical persisted facts always produce an identical solution.
    pub fn solve_chronology(&self, project_id: &str) -> Result<ChronologySolution> {
        Ok(solve(&self.chronology_facts(project_id)?))
    }

    /// Read-only, revision-pinned page of chronology facts and solved evidence.
    pub fn chronology_evidence_page(
        &self,
        project_id: &str,
        expected_revision: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<ChronologyEvidencePage> {
        chronology::evidence_page(
            self.chronology_facts(project_id)?,
            expected_revision,
            offset,
            limit,
        )
    }

    /// Makes one read-only, proxy-only cross-clip evidence request. Results are
    /// suggestions and provenance; this method has no path to chronology writes.
    /// The budget contract fails closed on a cache miss when no finite ceiling is
    /// configured.
    pub fn suggest_chronology_with_provider(
        &self,
        project_id: &str,
        asset_ids: &[String],
        provider: &dyn VlmProvider,
        chain: &ProviderChain,
    ) -> Result<SuggestionResult> {
        self.suggest_chronology_accounted(project_id, asset_ids, provider, chain, None, &mut || {
            false
        })
    }

    /// Paid chronology suggestion with an explicit finite budget. This is the
    /// same provider-injected production pipeline used by adapters; the shared
    /// ledger owns admission, dispatch and receipts.
    pub fn suggest_chronology_with_provider_and_budget(
        &self,
        project_id: &str,
        asset_ids: &[String],
        provider: &dyn VlmProvider,
        chain: &ProviderChain,
        budget_usd: f64,
    ) -> Result<SuggestionResult> {
        self.suggest_chronology_with_provider_and_budget_and_cancel(
            project_id,
            asset_ids,
            provider,
            chain,
            budget_usd,
            &mut || false,
        )
    }

    /// Cancellation-aware explicit-budget chronology suggestion. Cancellation is
    /// checked before each actual primary/fallback transmission.
    pub fn suggest_chronology_with_provider_and_budget_and_cancel(
        &self,
        project_id: &str,
        asset_ids: &[String],
        provider: &dyn VlmProvider,
        chain: &ProviderChain,
        budget_usd: f64,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<SuggestionResult> {
        let budget_usd = resolve_paid_operation_budget(Some(budget_usd), "chronology suggestions")?;
        self.suggest_chronology_accounted(
            project_id,
            asset_ids,
            provider,
            chain,
            Some(budget_usd),
            is_cancelled,
        )
    }

    pub(crate) fn suggest_chronology_accounted(
        &self,
        project_id: &str,
        asset_ids: &[String],
        provider: &dyn VlmProvider,
        chain: &ProviderChain,
        explicit_budget_usd: Option<f64>,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<SuggestionResult> {
        const CHRONOLOGY_SUGGESTION_RECIPE_VERSION: &str = "chronology.suggestions.v1";
        let prepared = analysis::chronology_suggestions::prepare_request(
            self,
            project_id,
            asset_ids,
            chain,
            is_cancelled,
        )?;
        // Completed result content is reusable without any paid admission. New
        // requests never create a pending cache permit: the canonical ledger is
        // the sole in-flight fence.
        {
            let connection = self.connection()?;
            if let Some(result) = analysis::chronology_suggestions::load_cached(
                &connection,
                &prepared.cache_request_key,
            )? {
                return Ok(result);
            }
        }
        let budget_usd =
            resolve_paid_operation_budget(explicit_budget_usd, "chronology suggestions")?;
        let intended_attempt_key = operation_intended_attempt_key(
            project_id,
            CHRONOLOGY_SUGGESTIONS_OPERATION,
            &prepared.source_fingerprint,
            &prepared.request_identity,
        )?;
        let policy_fingerprint = operation_policy_fingerprint(
            project_id,
            CHRONOLOGY_SUGGESTIONS_OPERATION,
            chain,
            CHRONOLOGY_SUGGESTION_RECIPE_VERSION,
            budget_usd,
        )?;
        let session = begin_accounted_operation(
            self,
            project_id,
            CHRONOLOGY_SUGGESTIONS_OPERATION,
            &policy_fingerprint,
            budget_usd,
            CHRONOLOGY_SUGGESTIONS_OPERATION,
        )?;
        let outcome = (|| -> Result<SuggestionResult> {
            let context =
                session.request_context(intended_attempt_key, prepared.source_fingerprint.clone());
            let accounted = AccountedVlmProvider::new(self, context, provider)
                .with_pre_dispatch_check(|| {
                    analysis::chronology_suggestions::validate_source_unchanged(
                        self,
                        project_id,
                        &prepared.request,
                        &prepared.source_identities,
                    )
                });
            let mut result = analysis::chronology_suggestions::request_suggestion_with_cancel(
                &accounted,
                chain,
                &prepared.request,
                is_cancelled,
            )?;
            result.review = review_suggestion_against_facts(
                &prepared.request,
                &result.suggestion,
                &prepared.facts,
            );
            let summary = self.analysis_operation_accounting_summary(
                project_id,
                CHRONOLOGY_SUGGESTIONS_OPERATION,
                &session.run_id,
            )?;
            // Canonical provenance is populated before persistence so reopening
            // the cache cannot show only the last response or lose request keys.
            let result = analysis::chronology_suggestions::with_accounting_projection(
                result,
                &summary,
                &session.run_id,
            );
            {
                let mut connection = self.connection()?;
                analysis::chronology_suggestions::store_completed_with_associations(
                    &mut connection,
                    &prepared.cache_request_key,
                    &prepared.request.facts_revision,
                    &result,
                    &summary.request_keys,
                )?;
            }
            self.finish_operation_run(
                &session.run_id,
                CHRONOLOGY_SUGGESTIONS_OPERATION,
                if summary.usage_complete {
                    "completed"
                } else {
                    "failed"
                },
                if summary.usage_complete {
                    ""
                } else {
                    "chronology suggestion returned incomplete usage evidence"
                },
                false,
                &summary,
            )?;
            Ok(result)
        })();
        match outcome {
            Ok(result) => Ok(result),
            Err(error) => {
                self.fail_operation_run(
                    project_id,
                    CHRONOLOGY_SUGGESTIONS_OPERATION,
                    &session.run_id,
                    &error,
                );
                Err(error)
            }
        }
    }

    /// Writes the solution's derived times back into the `assets` projection
    /// columns.
    ///
    /// This is the whole-project route through the solver-owned writer of
    /// `assets.best_time` and `assets.offset_seconds` (`CHRONOLOGY_MODEL.md`,
    /// "Evidence only"). Asset-add materialization uses that same writer's
    /// bounded single-asset route; no adapter or parallel chronology rule may
    /// write them. They exist for query performance and are reproducible by
    /// re-solving.
    pub fn project_chronology_solution(
        &self,
        project_id: &str,
        solution: &ChronologySolution,
    ) -> Result<usize> {
        self.require_project(project_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let written =
            Self::project_chronology_solution_on_connection(&transaction, project_id, solution)?;
        transaction.commit()?;
        Ok(written)
    }

    pub(crate) fn project_chronology_solution_on_connection(
        connection: &Connection,
        project_id: &str,
        solution: &ChronologySolution,
    ) -> Result<usize> {
        let mut written = 0usize;
        for asset in &solution.assets {
            written += Self::project_solved_asset_on_connection(connection, project_id, asset)?;
        }
        Ok(written)
    }

    /// The one writer for the derived `assets.best_time`/`offset_seconds`
    /// projection. Both a whole chronology solve and a just-committed import
    /// route through this helper so epoch and exception semantics cannot fork.
    pub(crate) fn project_solved_asset_on_connection(
        connection: &Connection,
        project_id: &str,
        asset: &chronology::AssetChronology,
    ) -> Result<usize> {
        // The offset is the derived shift from the camera's own claim, so an
        // exception correctly replaces an epoch instead of merely displaying
        // over it.
        let offset_seconds = match (asset.raw_time.as_deref(), asset.trip_time.as_deref()) {
            (Some(raw), Some(trip)) => {
                match (chronology::parse_utc(raw), chronology::parse_utc(trip)) {
                    (Some(from), Some(to)) => (to - from).num_seconds(),
                    _ => 0,
                }
            }
            _ => 0,
        };
        Ok(connection.execute(
            "UPDATE assets SET best_time = ?1, offset_seconds = ?2 WHERE id = ?3 AND project_id = ?4",
            params![asset.trip_time, offset_seconds, asset.asset_id, project_id],
        )?)
    }

    /// Projects the solver's exact local result for one newly materialized
    /// asset. This is bounded to its sequence epochs and own exception rather
    /// than re-solving every row of a large import.
    pub(crate) fn project_new_asset_chronology_on_connection(
        connection: &Connection,
        project_id: &str,
        asset_id: &str,
        capture_sequence_id: &str,
        filename_counter: Option<u64>,
        filename: &str,
        captured_at_raw: Option<&str>,
    ) -> Result<()> {
        let solved = chronology::projected_asset_solution(
            connection,
            project_id,
            asset_id,
            capture_sequence_id,
            filename_counter,
            filename,
            captured_at_raw,
        )?;
        Self::project_solved_asset_on_connection(connection, project_id, &solved)?;
        Ok(())
    }

    pub(crate) fn project_current_chronology_in_transaction(
        transaction: &rusqlite::Transaction<'_>,
        project_id: &str,
    ) -> Result<ChronologySolution> {
        let facts = chronology::load_chronology_facts(transaction, project_id)?;
        let solution = solve(&facts);
        Self::project_chronology_solution_on_connection(transaction, project_id, &solution)?;
        Ok(solution)
    }

    /// Solves and projects under one immediate transaction, the ordinary path
    /// after any chronology change. Acquiring the write reservation before
    /// loading facts prevents an older solve from projecting after a newer
    /// chronology operation commits on another connection.
    pub fn resolve_chronology(&self, project_id: &str) -> Result<ChronologySolution> {
        self.resolve_chronology_with_hook(project_id, || {})
    }

    pub(crate) fn resolve_chronology_with_hook<F>(
        &self,
        project_id: &str,
        after_solve: F,
    ) -> Result<ChronologySolution>
    where
        F: FnOnce(),
    {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
            [project_id],
            |row| row.get(0),
        )?;
        if !exists {
            bail!("project not found: {project_id}");
        }
        let facts = chronology::load_chronology_facts(&transaction, project_id)?;
        let solution = solve(&facts);
        after_solve();
        Self::project_chronology_solution_on_connection(&transaction, project_id, &solution)?;
        transaction.commit()?;
        Ok(solution)
    }

    /// Reports the operations this build retained but could not materialize,
    /// because their operation type is unknown to this build (D01-T007
    /// quarantine policy). The operations are durable in `project_operations`,
    /// never advanced an aggregate revision, and apply normally once a build
    /// that knows the type opens the catalog.
    pub fn quarantined_operations(&self, project_id: &str) -> Result<Vec<QuarantinedOperation>> {
        self.require_project(project_id)?;
        let connection = self.connection()?;
        chronology::load_quarantined_operations(&connection, project_id)
    }
}
