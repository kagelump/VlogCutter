//! Analysis index, readiness, video validation, and analysis-run bookkeeping and budgets.

use crate::analysis::store::load_transcript;
use crate::analysis::vlm_store::{ai_labels_for_asset, analysis_is_current, load_analysis};
use crate::*;
use anyhow::{Context, Result, bail};
use chrono::{SecondsFormat, Utc};
use rusqlite::{OptionalExtension, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Resolves and validates the spend ceiling locally. Zero and negative ceilings
/// are rejected; they do not mean "disable spend" or "allow free requests".
pub(crate) fn resolve_analysis_budget(options: &AnalysisRunOptions) -> Result<f64> {
    let budget = if let Some(value) = options.budget_usd {
        value
    } else {
        match std::env::var("RCDVLOG_VLM_MAX_COST_USD") {
            Ok(value) => value
                .parse::<f64>()
                .with_context(|| "RCDVLOG_VLM_MAX_COST_USD must be a finite positive number")?,
            Err(std::env::VarError::NotPresent) => return Ok(f64::INFINITY),
            Err(std::env::VarError::NotUnicode(_)) => {
                anyhow::bail!("RCDVLOG_VLM_MAX_COST_USD is not valid Unicode")
            }
        }
    };
    anyhow::ensure!(budget.is_finite(), "analysis budget must be finite");
    anyhow::ensure!(budget > 0.0, "analysis budget must be greater than zero");
    Ok(budget)
}

/// Paid ad-hoc operations fail closed unless the existing budget resolver can
/// produce a finite ceiling from the explicit option or the documented
/// environment setting. An absent ceiling is not an unlimited allowance.
pub(crate) fn resolve_paid_operation_budget(explicit: Option<f64>, operation: &str) -> Result<f64> {
    let budget = resolve_analysis_budget(&AnalysisRunOptions {
        budget_usd: explicit,
        ..AnalysisRunOptions::default()
    })
    .with_context(|| format!("resolve {operation} analysis budget"))?;
    anyhow::ensure!(
        budget.is_finite(),
        "{operation} requires a finite analysis budget; pass an explicit budget or set RCDVLOG_VLM_MAX_COST_USD"
    );
    Ok(budget)
}

impl Catalog {
    /// Per-clip analysis facts the queue needs for every clip at once.
    ///
    /// The review queue filters on transcript text and flags unusable clips, so
    /// it needs these for the whole project. Fetching them per clip would be one
    /// round trip per row; this is two queries for the project. Only clips with
    /// something recorded appear.
    pub fn project_analysis_index(&self, project_id: &str) -> Result<Vec<AssetAnalysisIndexEntry>> {
        let connection = self.connection()?;
        let mut entries: HashMap<String, AssetAnalysisIndexEntry> = HashMap::new();

        let mut transcripts = connection.prepare(
            "SELECT t.asset_id, t.status, COALESCE(GROUP_CONCAT(s.text, ' '), '')
             FROM asset_transcripts t
             JOIN assets a ON a.id = t.asset_id
             LEFT JOIN transcript_segments s ON s.asset_id = t.asset_id
             WHERE a.project_id = ?1 AND COALESCE(a.membership_removed, 0) = 0
             GROUP BY t.asset_id, t.status",
        )?;
        let rows = transcripts.query_map([project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (asset_id, status, text) = row?;
            let entry = entries
                .entry(asset_id.clone())
                .or_insert_with(|| AssetAnalysisIndexEntry::empty(&asset_id));
            entry.transcript_status = Some(status);
            entry.transcript_text = text;
        }

        let mut analyses = connection.prepare(
            "SELECT n.asset_id, n.usable, n.unusable_reason
             FROM asset_analyses n
             JOIN assets a ON a.id = n.asset_id
             WHERE a.project_id = ?1 AND COALESCE(a.membership_removed, 0) = 0",
        )?;
        let rows = analyses.query_map([project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? != 0,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (asset_id, usable, unusable_reason) = row?;
            let entry = entries
                .entry(asset_id.clone())
                .or_insert_with(|| AssetAnalysisIndexEntry::empty(&asset_id));
            entry.analyzed = true;
            entry.usable = usable;
            entry.unusable_reason = unusable_reason;
        }

        let mut index = entries.into_values().collect::<Vec<_>>();
        index.sort_by(|left, right| left.asset_id.cmp(&right.asset_id));
        Ok(index)
    }

    /// The stored transcript for one clip, if it has one.
    ///
    /// `no_speech` is returned as a normal transcript with no segments, not as
    /// `None`: the difference between "the VAD gate found silence" and "this
    /// clip was never transcribed" is exactly what a reviewer needs to see.
    pub fn asset_transcript(&self, asset_id: &str) -> Result<Option<ClipTranscript>> {
        load_transcript(&self.connection()?, asset_id)
    }

    /// The stored observation for one clip, with the labels analysis authored.
    pub fn asset_analysis(&self, asset_id: &str) -> Result<Option<AssetAnalysisView>> {
        let connection = self.connection()?;
        let Some(analysis) = load_analysis(&connection, asset_id)? else {
            return Ok(None);
        };
        // Whether the observation still describes the current proxy. A stale
        // observation is shown rather than hidden, but must be marked: the clip
        // it describes is not the clip on screen.
        let current = self
            .proxy_fingerprint(asset_id)
            .ok()
            .is_some_and(|fingerprint| {
                analysis_is_current(&connection, asset_id, &fingerprint).unwrap_or(false)
            });
        Ok(Some(AssetAnalysisView {
            analysis,
            ai_labels: ai_labels_for_asset(&connection, asset_id)?,
            current,
        }))
    }

    /// Reports what an analysis run would do, cost, and need, without running it.
    ///
    /// Offline and read-only. Every count matches the skip rule of the stage it
    /// describes, so the dialog cannot promise work the batch will then skip.
    pub fn analysis_readiness(&self, project_id: &str) -> Result<AnalysisReadiness> {
        let connection = self.connection()?;
        let mut readiness = AnalysisReadiness {
            project_id: project_id.to_string(),
            total_clips: 0,
            reachable_originals: 0,
            unverified_video_clips: 0,
            quarantined_video_clips: 0,
            unavailable_video_clips: 0,
            missing_proxies: 0,
            missing_transcripts: 0,
            missing_analyses: 0,
            models: self.model_status()?,
            models_ready: false,
            api_key_present: resolve_api_key().is_ok(),
            estimated_cost_usd: 0.0,
            cost_basis: AnalysisCostBasis::Reference,
            observed_cost_per_clip_usd: 0.0,
            last_run: self.analysis_runs(project_id, 1)?.into_iter().next(),
        };
        readiness.models_ready = readiness.models.iter().all(|model| model.installed);

        for asset in self.list_assets(project_id)? {
            if asset.removed {
                continue;
            }
            let validation = self.asset_video_validation_for(&asset)?;
            if validation.bytes_reachable {
                readiness.reachable_originals += 1;
            }
            match validation.status {
                AssetVideoValidationStatus::Usable => {
                    if !validation.bytes_reachable && !asset.proxy_available {
                        readiness.unavailable_video_clips += 1;
                        continue;
                    }
                }
                AssetVideoValidationStatus::Unverified => {
                    readiness.unverified_video_clips += 1;
                    continue;
                }
                AssetVideoValidationStatus::Quarantined => {
                    readiness.quarantined_video_clips += 1;
                    continue;
                }
            }
            readiness.total_clips += 1;
            // A clip whose original is offline but whose proxy is present is not
            // "missing a proxy": there is no input to make a better one from.
            let proxy_is_the_only_representation =
                !asset.original_available && asset.proxy_available;
            if !has_current_proxy(&asset) && !proxy_is_the_only_representation {
                readiness.missing_proxies += 1;
            }
            // A clip without a current proxy has no current fingerprint, so it
            // counts as missing for both derived stages: stage zero will make
            // the proxy and the later stages will then have work to do.
            match self.proxy_fingerprint(&asset.id) {
                Ok(fingerprint) => {
                    // Current native terminal evidence is reusable here without
                    // opening the local engine or contacting a provider. Incomplete
                    // or unproved native work stays visibly missing/review.
                    if !self.current_transcript_state(&asset.id)?.is_current() {
                        readiness.missing_transcripts += 1;
                    }
                    if !analysis_is_current(&connection, &asset.id, &fingerprint)? {
                        readiness.missing_analyses += 1;
                    }
                }
                Err(_) => {
                    readiness.missing_transcripts += 1;
                    readiness.missing_analyses += 1;
                }
            }
        }

        let observed: Option<f64> = connection
            .query_row(
                "SELECT AVG(a.cost_usd) FROM asset_analyses a
                 JOIN assets s ON s.id = a.asset_id
                 WHERE s.project_id = ?1 AND a.cost_usd > 0",
                [project_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let per_clip = match observed {
            Some(cost) if cost > 0.0 => {
                readiness.cost_basis = AnalysisCostBasis::Observed;
                cost
            }
            _ => REFERENCE_COST_PER_CLIP_USD,
        };
        readiness.observed_cost_per_clip_usd = per_clip;
        readiness.estimated_cost_usd = per_clip * readiness.missing_analyses as f64;
        Ok(readiness)
    }

    /// Returns durable video eligibility separately from current byte reachability.
    /// Catalog rows created before explicit validation are unverified until checked.
    pub fn asset_video_validation(&self, asset_id: &str) -> Result<AssetVideoValidation> {
        let asset = self.get_asset(asset_id)?;
        self.asset_video_validation_for(&asset)
    }

    pub(crate) fn asset_video_validation_for(&self, asset: &Asset) -> Result<AssetVideoValidation> {
        let connection = self.connection()?;
        let has_validation_table = table_exists(&connection, "asset_video_validation")?;
        let stored: Option<(String, String, Option<String>, String)> = if has_validation_table {
            connection.query_row(
                "SELECT fingerprint, status, diagnostic, validated_at FROM asset_video_validation WHERE asset_id = ?1",
                [&asset.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            ).optional()?
        } else {
            None
        };
        let (status, diagnostic, validated_at) = match stored {
            Some((fingerprint, status, diagnostic, validated_at))
                if fingerprint == asset.fingerprint =>
            {
                let state = match status.as_str() {
                    "usable" => AssetVideoValidationStatus::Usable,
                    "quarantined" => AssetVideoValidationStatus::Quarantined,
                    _ => AssetVideoValidationStatus::Unverified,
                };
                (state, diagnostic, Some(validated_at))
            }
            _ => (AssetVideoValidationStatus::Unverified, None, None),
        };
        Ok(AssetVideoValidation {
            asset_id: asset.id.clone(),
            status,
            bytes_reachable: Path::new(&asset.path).is_file(),
            diagnostic,
            validated_at,
        })
    }

    /// Explicitly validates a legacy or new asset and durably records its video eligibility.
    /// Offline originals may validate against an existing proxy; neither representation is changed.
    pub fn validate_asset_video(&self, asset_id: &str) -> Result<AssetVideoValidation> {
        let asset = self.get_asset(asset_id)?;
        let validation_path = if Path::new(&asset.path).is_file() {
            PathBuf::from(&asset.path)
        } else if let Some(proxy_path) = asset
            .proxy_path
            .as_deref()
            .filter(|proxy_path| Path::new(proxy_path).is_file())
        {
            PathBuf::from(proxy_path)
        } else {
            bail!("original bytes and proxy are not reachable; video validation was not recorded");
        };
        let (status, diagnostic) = match probe_media_stream(&validation_path) {
            Ok(_) => ("usable", None),
            Err(error) => ("quarantined", Some(format!("{error:#}"))),
        };
        self.store_video_validation(asset_id, &asset.fingerprint, status, diagnostic.as_deref())?;
        self.asset_video_validation(asset_id)
    }

    pub(crate) fn store_video_validation(
        &self,
        asset_id: &str,
        fingerprint: &str,
        status: &str,
        diagnostic: Option<&str>,
    ) -> Result<()> {
        self.connection()?.execute(
            "INSERT INTO asset_video_validation(asset_id, fingerprint, status, diagnostic, validated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(asset_id) DO UPDATE SET fingerprint=excluded.fingerprint,
                status=excluded.status, diagnostic=excluded.diagnostic, validated_at=excluded.validated_at",
            params![asset_id, fingerprint, status, diagnostic, now()],
        )?;
        Ok(())
    }

    /// Opens a run record so spend is attributable even if the app exits.
    pub fn begin_analysis_run(&self, project_id: &str) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        self.connection()?.execute(
            "INSERT INTO analysis_runs (id, project_id, started_at, status, stage_reached)
             VALUES (?1, ?2, ?3, 'running', 'proxies')",
            params![
                id,
                project_id,
                Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
            ],
        )?;
        Ok(id)
    }

    /// Opens a run owned by a fully ledgered runtime. The marker is written in
    /// the same row as the run identity so a legitimate zero-request cancelled
    /// pilot is distinguishable from untracked legacy spending.
    pub fn begin_accounted_analysis_run(
        &self,
        project_id: &str,
        policy_id: &str,
        stage_reached: &str,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        self.connection()?.execute(
            "INSERT INTO analysis_runs
                (id, project_id, started_at, status, stage_reached, policy_id, accounting_contract)
             VALUES (?1, ?2, ?3, 'running', ?4, ?5, ?6)",
            params![
                id,
                project_id,
                Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                stage_reached,
                policy_id,
                crate::analysis::accounting::ANALYSIS_ACCOUNTING_SCHEMA_VERSION,
            ],
        )?;
        Ok(id)
    }

    /// Closes a run record with what it actually did.
    pub fn finish_analysis_run(&self, run_id: &str, outcome: &AnalysisRunOutcome) -> Result<()> {
        self.connection()?.execute(
            "UPDATE analysis_runs
             SET finished_at=?2, status=?3, stage_reached=?4, proxies_generated=?5,
                 clips_transcribed=?6, clips_analyzed=?7, cost_usd=?8, budget_exhausted=?9,
                 error=?10
             WHERE id=?1",
            params![
                run_id,
                Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                outcome.status,
                outcome.stage_reached,
                outcome.proxies_generated as i64,
                outcome.clips_transcribed as i64,
                outcome.clips_analyzed as i64,
                outcome.cost_usd,
                outcome.budget_exhausted as i64,
                outcome.error,
            ],
        )?;
        Ok(())
    }

    /// Confirmed current-run projection plus separately visible reservation and
    /// uncertainty fields for one non-policy paid operation. Request keys remain
    /// inspectable for the follow-up reconciliation CLI.
    pub fn analysis_operation_accounting_summary(
        &self,
        project_id: &str,
        operation: &str,
        run_id: &str,
    ) -> Result<AnalysisOperationAccountingSummary> {
        anyhow::ensure!(
            !project_id.trim().is_empty() && !operation.trim().is_empty(),
            "operation accounting project and operation are required"
        );
        anyhow::ensure!(
            !run_id.trim().is_empty(),
            "operation accounting run id is required"
        );
        let projection =
            self.analysis_accounting_aggregate_for_operation_run(project_id, operation, run_id)?;
        let request_keys = self.analysis_request_keys_for_run(project_id, operation, run_id)?;
        Ok(AnalysisOperationAccountingSummary {
            run_id: run_id.to_string(),
            request_count: projection.request_count,
            completed_request_count: projection.completed_request_count,
            active_request_count: projection.active_request_count,
            uncertain_request_count: projection.uncertain_request_count,
            confirmed_cost_usd: projection.confirmed_cost_usd,
            estimated_cost_usd: projection.estimated_cost_usd,
            active_reserved_cost_usd: projection.active_reserved_cost_usd,
            uncertain_usage_reserve_usd: projection.uncertain_usage_reserve_usd,
            committed_or_reserved_usd: projection.committed_or_reserved_usd,
            usage_complete: !projection.has_unresolved_usage(),
            actual_provider: projection.current_run_actual_provider,
            actual_model: projection.current_run_actual_model,
            provider_generation_id: projection.current_run_generation_id,
            request_keys,
        })
    }

    /// Best-effort durable close after any post-admission failure. Only typed
    /// safe failure detail is persisted; raw provider/parse text never enters run
    /// history. Receipts and confirmed-ledger cost remain untouched.
    pub(crate) fn fail_operation_run(
        &self,
        project_id: &str,
        operation: &str,
        run_id: &str,
        error: &anyhow::Error,
    ) {
        let Ok(summary) = self.analysis_operation_accounting_summary(project_id, operation, run_id)
        else {
            return;
        };
        let failure = ProviderFailure::from_anyhow(error);
        let _ = self.finish_operation_run(
            run_id,
            operation,
            "failed",
            &failure.safe_detail(),
            failure.category == crate::ProviderFailureCategory::LocalBudgetExhausted,
            &summary,
        );
    }

    /// Closes an operation-owned accounted run with confirmed current-run cost.
    /// Uncertainty remains on the ledger and is deliberately not copied into the
    /// aggregate run `cost_usd` column.
    pub(crate) fn finish_operation_run(
        &self,
        run_id: &str,
        operation: &str,
        status: &str,
        error: &str,
        budget_exhausted: bool,
        summary: &AnalysisOperationAccountingSummary,
    ) -> Result<()> {
        self.finish_analysis_run(
            run_id,
            &AnalysisRunOutcome {
                status: status.to_string(),
                stage_reached: operation.to_string(),
                clips_analyzed: usize::from(summary.completed_request_count > 0),
                cost_usd: summary.confirmed_cost_usd,
                budget_exhausted,
                error: error.chars().take(240).collect(),
                ..Default::default()
            },
        )
    }

    /// Most recent runs for a project, newest first.
    pub fn analysis_runs(&self, project_id: &str, limit: usize) -> Result<Vec<AnalysisRunRecord>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, project_id, started_at, finished_at, status, stage_reached,
                    policy_id, proxies_generated, clips_transcribed, clips_analyzed,
                    cost_usd, budget_exhausted, error, accounting_contract
             FROM analysis_runs WHERE project_id=?1
             ORDER BY started_at DESC, id DESC LIMIT ?2",
        )?;
        let runs = statement
            .query_map(params![project_id, limit as i64], |row| {
                Ok(AnalysisRunRecord {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    started_at: row.get(2)?,
                    finished_at: row.get(3)?,
                    status: row.get(4)?,
                    stage_reached: row.get(5)?,
                    policy_id: row.get(6)?,
                    proxies_generated: row.get::<_, i64>(7)? as usize,
                    clips_transcribed: row.get::<_, i64>(8)? as usize,
                    clips_analyzed: row.get::<_, i64>(9)? as usize,
                    cost_usd: row.get(10)?,
                    budget_exhausted: row.get::<_, i64>(11)? != 0,
                    error: row.get(12)?,
                    accounting_contract: row.get(13)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(runs)
    }
}
