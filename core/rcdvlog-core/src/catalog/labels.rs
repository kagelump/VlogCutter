//! Label vocabulary, mapping, and consolidation.

use crate::analysis::accounting::{
    begin_accounted_operation, operation_intended_attempt_key, operation_policy_fingerprint,
};
use crate::analysis::consolidate::{
    apply_mapping as persist_label_mapping, assets_for_label, parse_mapping,
    proposals as label_proposals,
};
use crate::*;
use anyhow::{Result, bail};

pub(crate) fn empty_consolidation_report() -> ConsolidationReport {
    ConsolidationReport {
        remapped_assets: 0,
        vocabulary_size: 0,
        mappings: Vec::new(),
        model: String::new(),
        cost_usd: 0.0,
        uncertain_usage_reserve_usd: 0.0,
        active_reserved_cost_usd: 0.0,
        committed_or_reserved_usd: 0.0,
        usage_complete: true,
        run_id: String::new(),
        request_keys: Vec::new(),
        actual_provider: None,
        actual_model: None,
        provider_generation_id: None,
    }
}

pub(crate) fn normalize_labels(labels: &[String]) -> Result<Vec<String>> {
    let mut normalized = Vec::new();
    for label in labels {
        let label = label.trim();
        if label.is_empty() {
            continue;
        }
        if label.chars().count() > 64 {
            bail!("invalid label: labels must be 64 characters or fewer");
        }
        if !normalized
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(label))
        {
            normalized.push(label.to_string());
        }
    }
    if normalized.len() > 24 {
        bail!("invalid labels: an asset can have at most 24 labels");
    }
    Ok(normalized)
}

impl Catalog {
    pub fn label_vocabulary(&self, project_id: &str) -> Result<Vec<LabelProposal>> {
        analysis::consolidate::vocabulary(&self.connection()?, project_id)
    }

    /// Applies an already validated label remap through typed label operations.
    /// Exposed separately so deterministic tests and future review UI can show
    /// the proposed mapping before accepting it.
    pub fn apply_label_mapping(
        &self,
        project_id: &str,
        mappings: &[LabelMapping],
    ) -> Result<ConsolidationReport> {
        let connection = self.connection()?;
        let proposals = label_proposals(&connection, project_id)?;
        let envelope = serde_json::json!({"mappings": mappings});
        parse_mapping(&envelope.to_string(), &proposals)?;
        let actions = mappings
            .iter()
            .map(|mapping| {
                Ok((
                    mapping.clone(),
                    assets_for_label(&connection, project_id, &mapping.from)?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        drop(connection);
        for (mapping, assets) in &actions {
            if mapping.from.eq_ignore_ascii_case(&mapping.to) {
                continue;
            }
            for asset_id in assets {
                self.remove_label(asset_id, &mapping.from)?;
                self.add_label(asset_id, &mapping.to)?;
            }
        }
        let (remapped_assets, vocabulary_size) =
            persist_label_mapping(&self.connection()?, project_id, mappings)?;
        let mut report = empty_consolidation_report();
        report.remapped_assets = remapped_assets;
        report.vocabulary_size = vocabulary_size;
        report.mappings = mappings.to_vec();
        Ok(report)
    }

    /// Performs one text-only vocabulary consolidation request through the
    /// canonical operation authority and accounted provider boundary. Production
    /// credentials and the existing finite budget resolver are resolved here.
    pub fn consolidate_labels(&self, project_id: &str) -> Result<ConsolidationReport> {
        if label_proposals(&self.connection()?, project_id)?.is_empty() {
            return Ok(empty_consolidation_report());
        }
        let budget_usd = resolve_paid_operation_budget(None, "label consolidation")?;
        let provider = OpenRouterProvider::new(resolve_api_key()?)?;
        let chain = ProviderChain::new();
        self.consolidate_labels_with_provider(project_id, &provider, &chain, budget_usd)
    }

    /// Provider-injected label consolidation. The same production pipeline is
    /// used by focused offline verification; callers only substitute transport
    /// and an explicit finite budget.
    pub fn consolidate_labels_with_provider(
        &self,
        project_id: &str,
        provider: &dyn VlmProvider,
        chain: &ProviderChain,
        budget_usd: f64,
    ) -> Result<ConsolidationReport> {
        self.consolidate_labels_with_provider_and_cancel(
            project_id,
            provider,
            chain,
            budget_usd,
            &mut || false,
        )
    }

    /// Cancellation-aware provider-injected consolidation. Cancellation after a
    /// primary attempt prevents the domain-validation fallback transmission.
    pub fn consolidate_labels_with_provider_and_cancel(
        &self,
        project_id: &str,
        provider: &dyn VlmProvider,
        chain: &ProviderChain,
        budget_usd: f64,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<ConsolidationReport> {
        let budget_usd = resolve_paid_operation_budget(Some(budget_usd), "label consolidation")?;
        self.consolidate_labels_accounted(project_id, provider, chain, budget_usd, is_cancelled)
    }

    /// Budget-first production wrapper used by adapters after resolving the
    /// existing environment/options ceiling.
    pub fn consolidate_labels_with_budget(
        &self,
        project_id: &str,
        budget_usd: f64,
    ) -> Result<ConsolidationReport> {
        let provider = OpenRouterProvider::new(resolve_api_key()?)?;
        let chain = ProviderChain::new();
        self.consolidate_labels_with_provider(project_id, &provider, &chain, budget_usd)
    }

    pub(crate) fn consolidate_labels_accounted(
        &self,
        project_id: &str,
        provider: &dyn VlmProvider,
        chain: &ProviderChain,
        budget_usd: f64,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<ConsolidationReport> {
        const CONSOLIDATION_RECIPE_VERSION: &str = "labels.consolidation.v1";
        let prepared = {
            let connection = self.connection()?;
            match analysis::consolidate::prepare(&connection, project_id, chain)? {
                Some(prepared) => prepared,
                None => return Ok(empty_consolidation_report()),
            }
        };
        let intended_attempt_key = operation_intended_attempt_key(
            project_id,
            LABEL_CONSOLIDATION_OPERATION,
            &prepared.source_fingerprint,
            &prepared.request_identity,
        )?;
        let policy_fingerprint = operation_policy_fingerprint(
            project_id,
            LABEL_CONSOLIDATION_OPERATION,
            chain,
            CONSOLIDATION_RECIPE_VERSION,
            budget_usd,
        )?;
        let session = begin_accounted_operation(
            self,
            project_id,
            LABEL_CONSOLIDATION_OPERATION,
            &policy_fingerprint,
            budget_usd,
            LABEL_CONSOLIDATION_OPERATION,
        )?;
        let outcome = (|| -> Result<ConsolidationReport> {
            let context =
                session.request_context(intended_attempt_key, prepared.source_fingerprint.clone());
            let expected_source = prepared.source_fingerprint.clone();
            let accounted = AccountedVlmProvider::new(self, context, provider)
                .with_pre_dispatch_check(move || {
                    let connection = self.connection()?;
                    analysis::consolidate::validate_source_unchanged(
                        &connection,
                        project_id,
                        &expected_source,
                    )
                });
            let mapping = analysis::consolidate::run_with_cancel(
                &accounted,
                chain,
                &prepared.request,
                &prepared.proposals,
                is_cancelled,
            )?;
            // The model may have been paid before a concurrent AI-label write.
            // Never apply a mapping over different source evidence.
            {
                let connection = self.connection()?;
                analysis::consolidate::validate_source_unchanged(
                    &connection,
                    project_id,
                    &prepared.source_fingerprint,
                )?;
            }
            let report = self.apply_label_mapping(project_id, &mapping)?;
            let summary = self.analysis_operation_accounting_summary(
                project_id,
                LABEL_CONSOLIDATION_OPERATION,
                &session.run_id,
            )?;
            let report = analysis::consolidate::with_accounting_projection(
                report,
                &summary,
                &session.run_id,
            );
            self.finish_operation_run(
                &session.run_id,
                LABEL_CONSOLIDATION_OPERATION,
                if summary.usage_complete {
                    "completed"
                } else {
                    "failed"
                },
                if summary.usage_complete {
                    ""
                } else {
                    "label consolidation returned incomplete usage evidence"
                },
                false,
                &summary,
            )?;
            Ok(report)
        })();
        match outcome {
            Ok(report) => Ok(report),
            Err(error) => {
                self.fail_operation_run(
                    project_id,
                    LABEL_CONSOLIDATION_OPERATION,
                    &session.run_id,
                    &error,
                );
                Err(error)
            }
        }
    }
}
