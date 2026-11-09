//! `Catalog` methods that persist, recover, and look up calibration results.

use super::*;

impl Catalog {
    /// Recover only runs whose owner is proven gone. Opening a second catalog
    /// while another live process owns a running calibration must be a no-op.
    pub(crate) fn recover_interrupted_resource_calibrations(&self) -> Result<()> {
        if self.read_only {
            return Ok(());
        }
        let mut connection = self.connection()?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='resource_calibrations')",
            [],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(());
        }
        let rows = {
            let mut statement = connection.prepare(
                "SELECT cache_key,owner_id,owner_pid,owner_process_start,heartbeat_at FROM resource_calibrations WHERE state='running'",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let abandoned = rows
            .into_iter()
            .filter(|(_, owner_id, pid, process_start, heartbeat_at)| {
                !owner_is_live(
                    owner_id.as_deref(),
                    pid.and_then(|value| u32::try_from(value).ok()),
                    process_start.as_deref(),
                    heartbeat_at.as_deref(),
                )
            })
            .collect::<Vec<_>>();
        if abandoned.is_empty() {
            return Ok(());
        }
        // Recovery is maintenance, never a reason to block a catalog reader.
        // Effective status already treats a dead owner's row as interrupted;
        // a later writer may persist that fact when SQLite is available.
        connection.busy_timeout(Duration::ZERO)?;
        let transaction = match connection.transaction_with_behavior(TransactionBehavior::Immediate)
        {
            Ok(transaction) => transaction,
            Err(error)
                if matches!(
                    error.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let now = timestamp();
        for (cache_key, owner_id, pid, process_start, heartbeat_at) in abandoned {
            let pid = pid.and_then(|value| u32::try_from(value).ok());
            if owner_is_live(
                owner_id.as_deref(),
                pid,
                process_start.as_deref(),
                heartbeat_at.as_deref(),
            ) {
                continue;
            }
            transaction.execute(
                "UPDATE resource_calibrations SET state='interrupted',updated_at=?1,completed_at=?1,invalidated_at=?1,invalidation_reason='owner process is no longer live' WHERE cache_key=?2 AND state='running' AND owner_id IS ?3",
                params![now, cache_key, owner_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub(super) fn begin_resource_calibration(
        &self,
        identity: &CalibrationIdentity,
        cache_key: &str,
    ) -> Result<()> {
        let now = timestamp();
        let identity_json = serde_json::to_string(identity)?;
        let owner = current_run_owner();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT state,owner_id,owner_pid,owner_process_start,heartbeat_at FROM resource_calibrations WHERE cache_key=?1",
                [cache_key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?;
        if let Some((state, owner_id, pid, process_start, heartbeat_at)) = existing {
            let pid = pid.and_then(|value| u32::try_from(value).ok());
            if state == "running"
                && owner_is_live(
                    owner_id.as_deref(),
                    pid,
                    process_start.as_deref(),
                    heartbeat_at.as_deref(),
                )
            {
                return Err(anyhow!(
                    "calibration for this exact identity is already running in a live process"
                ));
            }
        }
        transaction.execute(
            "INSERT INTO resource_calibrations(cache_key,schema_version,recipe_key,representation_kind,media_kind,sample_identity,state,identity_json,created_at,updated_at,owner_id,owner_pid,owner_process_start,heartbeat_at) VALUES(?1,?2,?3,?4,?5,?6,'running',?7,?8,?8,?9,?10,?11,?8) ON CONFLICT(cache_key) DO UPDATE SET state='running',report_json=NULL,selected_settings_json=NULL,created_at=excluded.created_at,updated_at=excluded.updated_at,completed_at=NULL,invalidated_at=NULL,invalidation_reason=NULL,identity_json=excluded.identity_json,owner_id=excluded.owner_id,owner_pid=excluded.owner_pid,owner_process_start=excluded.owner_process_start,heartbeat_at=excluded.heartbeat_at",
            params![
                cache_key,
                RESOURCE_CALIBRATION_SCHEMA_VERSION,
                identity.recipe_key,
                identity.representation_kind,
                identity.media.kind,
                identity.media.sample_identity,
                identity_json,
                now,
                owner.owner_id,
                owner.pid as i64,
                owner.process_start,
            ],
        )?;
        transaction.execute(
            "DELETE FROM resource_calibration_observations WHERE cache_key=?1",
            [cache_key],
        )?;
        let run_key = active_run_key(self.database_path(), cache_key);
        // Publish in-process ownership before the SQLite row becomes visible.
        active_calibration_runs()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(run_key.clone(), owner);
        if let Err(error) = transaction.commit() {
            active_calibration_runs()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&run_key);
            return Err(error.into());
        }
        Ok(())
    }

    pub(super) fn touch_resource_calibration(&self, cache_key: &str) -> Result<()> {
        let run_key = active_run_key(self.database_path(), cache_key);
        let owner_id = active_calibration_runs()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&run_key)
            .map(|owner| owner.owner_id.clone());
        let Some(owner_id) = owner_id else {
            return Ok(());
        };
        let connection = self.connection()?;
        connection.execute(
            "UPDATE resource_calibrations SET heartbeat_at=?1,updated_at=?1 WHERE cache_key=?2 AND state='running' AND owner_id=?3",
            params![timestamp(), cache_key, owner_id],
        )?;
        Ok(())
    }

    /// Persist the report, its selected settings and all observations in one
    /// SQLite transaction. A failure leaves the prior running row and no
    /// partial terminal report/observation set.
    pub(super) fn persist_resource_calibration(
        &self,
        report: &mut ResourceCalibrationReport,
    ) -> Result<()> {
        let now = timestamp();
        let final_key = report.identity.cache_key()?;
        let original_key = report.cache_key.clone();
        let _cleanup = ActiveCalibrationCleanup {
            database_path: self.database_path().to_path_buf(),
            keys: vec![original_key.clone(), final_key.clone()],
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if final_key != original_key {
            transaction.execute(
                "INSERT INTO resource_calibrations(cache_key,schema_version,recipe_key,representation_kind,media_kind,sample_identity,state,identity_json,report_json,selected_settings_json,created_at,updated_at,completed_at,invalidated_at,invalidation_reason,owner_id,owner_pid,owner_process_start,heartbeat_at) SELECT ?1,schema_version,recipe_key,representation_kind,media_kind,sample_identity,state,identity_json,report_json,selected_settings_json,created_at,updated_at,completed_at,invalidated_at,invalidation_reason,owner_id,owner_pid,owner_process_start,heartbeat_at FROM resource_calibrations WHERE cache_key=?2",
                params![final_key, original_key],
            )?;
            transaction.execute(
                "DELETE FROM resource_calibrations WHERE cache_key=?1",
                [&original_key],
            )?;
            report.cache_key = final_key.clone();
        }
        let selected = report
            .selected_settings
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        transaction.execute(
            "UPDATE resource_calibrations SET schema_version=?1,recipe_key=?2,representation_kind=?3,media_kind=?4,sample_identity=?5,identity_json=?6,state=?7,report_json=?8,selected_settings_json=?9,updated_at=?10,completed_at=?11,heartbeat_at=?10 WHERE cache_key=?12",
            params![
                RESOURCE_CALIBRATION_SCHEMA_VERSION,
                report.identity.recipe_key,
                report.identity.representation_kind,
                report.identity.media.kind,
                report.identity.media.sample_identity,
                serde_json::to_string(&report.identity)?,
                report.state.as_str(),
                serde_json::to_string(report)?,
                selected,
                now,
                report.completed_at,
                report.cache_key,
            ],
        )?;
        transaction.execute(
            "DELETE FROM resource_calibration_observations WHERE cache_key=?1",
            [&report.cache_key],
        )?;
        for candidate in &report.candidates {
            if let Some(timing) = &candidate.timing {
                for duration_ms in &timing.samples_ms {
                    transaction.execute(
                        "INSERT INTO resource_calibration_observations(cache_key,candidate_label,succeeded,duration_ms,failure,created_at) VALUES(?1,?2,1,?3,NULL,?4)",
                        params![report.cache_key, candidate.settings.label, *duration_ms as i64, now],
                    )?;
                }
            }
            for failure in &candidate.failures {
                transaction.execute(
                    "INSERT INTO resource_calibration_observations(cache_key,candidate_label,succeeded,duration_ms,failure,created_at) VALUES(?1,?2,0,NULL,?3,?4)",
                    params![report.cache_key, candidate.settings.label, bounded_text(failure, 1024), now],
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub(super) fn exact_complete_calibration(
        &self,
        cache_key: &str,
    ) -> Result<Option<ResourceCalibrationReport>> {
        let connection = self.connection()?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='resource_calibrations')",
            [],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(None);
        }
        let body: Option<String> = connection
            .query_row(
                "SELECT report_json FROM resource_calibrations WHERE cache_key=?1 AND state='complete' AND report_json IS NOT NULL",
                [cache_key],
                |row| row.get(0),
            )
            .optional()?;
        let Some(body) = body else {
            return Ok(None);
        };
        let report: ResourceCalibrationReport = serde_json::from_str(&body)?;
        if report.cache_key != cache_key
            || report.identity.cache_key()? != cache_key
            || !self.report_environment_matches_current(&report)?
        {
            return Ok(None);
        }
        Ok(Some(report))
    }

    pub(super) fn report_environment_matches_current(
        &self,
        report: &ResourceCalibrationReport,
    ) -> Result<bool> {
        if report.identity.schema_version != RESOURCE_CALIBRATION_SCHEMA_VERSION
            || report.identity.recipe_version != RESOURCE_CALIBRATION_RECIPE_VERSION
            || report.identity.host != host_identity()
            || report.identity.tools != tool_identities()
            || report.identity.devices != hardware_device_identities()
        {
            return Ok(false);
        }
        let policy = report
            .identity
            .candidates
            .iter()
            .find_map(|candidate| candidate.encoder_policy.as_deref())
            .and_then(|value| match value {
                "auto" => Some(ProxyEncoderPolicy::Auto),
                "software" => Some(ProxyEncoderPolicy::Software),
                _ => None,
            });
        let expected_configuration = calibration_configuration_key(
            &report.identity.media.kind,
            &report.identity.candidates,
            policy,
        )?;
        if report.identity.configuration_key != expected_configuration {
            return Ok(false);
        }
        let current_models = if report.identity.representation_kind == SPEECH_REPRESENTATION_KIND {
            model_identity(&self.model_status()?)
        } else {
            Vec::new()
        };
        let mut measured_models = report.identity.models.clone();
        for model in &mut measured_models {
            model.content_digest = None;
        }
        Ok(measured_models == current_models)
    }

    pub(super) fn running_owner_is_live(&self, cache_key: &str) -> Result<bool> {
        let connection = self.connection()?;
        let row = connection
            .query_row(
                "SELECT owner_id,owner_pid,owner_process_start,heartbeat_at FROM resource_calibrations WHERE cache_key=?1 AND state='running'",
                [cache_key],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((owner_id, pid, process_start, heartbeat_at)) = row else {
            return Ok(false);
        };
        Ok(owner_is_live(
            owner_id.as_deref(),
            pid.and_then(|value| u32::try_from(value).ok()),
            process_start.as_deref(),
            heartbeat_at.as_deref(),
        ))
    }

    pub(super) fn calibration_entries(
        &self,
        recipe_key: Option<&str>,
        representation_kind: Option<&str>,
    ) -> Result<Vec<CalibrationSummary>> {
        let connection = self.connection()?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='resource_calibrations')",
            [],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(Vec::new());
        }
        let mut statement = connection.prepare(
            "SELECT cache_key,recipe_key,representation_kind,media_kind,sample_identity,state,selected_settings_json,report_json,created_at,updated_at,completed_at,invalidation_reason FROM resource_calibrations WHERE (?1 IS NULL OR recipe_key=?1) AND (?2 IS NULL OR representation_kind=?2) ORDER BY updated_at DESC, cache_key ASC LIMIT 100",
        )?;
        let rows = statement.query_map(params![recipe_key, representation_kind], |row| {
            let state_text: String = row.get(5)?;
            let selected_text: Option<String> = row.get(6)?;
            let report_text: Option<String> = row.get(7)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                state_text,
                selected_text,
                report_text,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<String>>(11)?,
            ))
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let (
                cache_key,
                recipe_key,
                representation_kind,
                media_kind,
                sample_identity,
                state_text,
                selected_text,
                report_text,
                created_at,
                updated_at,
                completed_at,
                invalidation_reason,
            ) = row?;
            let state = CalibrationState::parse(&state_text)?;
            let selected_settings = selected_text
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?;
            let report: Option<ResourceCalibrationReport> = report_text
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?;
            let host_tools_match_current = report
                .as_ref()
                .map(|report| self.report_environment_matches_current(report))
                .transpose()?
                .unwrap_or(false);
            entries.push(CalibrationSummary {
                reusable: state == CalibrationState::Complete && host_tools_match_current,
                cache_key,
                recipe_key,
                representation_kind,
                media_kind,
                sample_identity,
                state,
                selected_settings,
                uncertainty: report.map(|report| report.uncertainty),
                created_at,
                updated_at,
                completed_at,
                invalidation_reason,
                host_tools_match_current,
            });
        }
        Ok(entries)
    }

    /// Read-only calibration inventory. A running row is shown as interrupted
    /// only when its owner is proven gone; a live owner in another process is
    /// never invalidated by a read-only status call.
    pub fn resource_calibration_status(
        &self,
        recipe_key: Option<&str>,
        representation_kind: Option<&str>,
    ) -> Result<CalibrationStatusReport> {
        let mut entries = self.calibration_entries(recipe_key, representation_kind)?;
        for entry in &mut entries {
            if entry.state == CalibrationState::Running {
                entry.reusable = false;
                if !self.running_owner_is_live(&entry.cache_key)? {
                    entry.state = CalibrationState::Interrupted;
                }
            }
        }
        Ok(CalibrationStatusReport {
            schema_version: RESOURCE_CALIBRATION_SCHEMA_VERSION.into(),
            recipe_key: recipe_key.map(str::to_string),
            representation_kind: representation_kind.map(str::to_string),
            entries,
            note: "Only complete calibrations matching host/build/tool/device/model/recipe/configuration facts are reusable. Missing, interrupted, failed, not-ready, and differently configured results remain uncertainty and are never converted into a completion promise.".into(),
        })
    }

    /// Find the newest reusable calibration for one kind/recipe. Generated
    /// media calibrations are preferred because their sample identity is
    /// reproducible; representative-media results are only reused when the
    /// caller provides the exact same media sample identity.
    pub(super) fn reusable_calibration_report(
        &self,
        representation_kind: &str,
        recipe_key: &str,
        sample_identity: Option<&str>,
    ) -> Result<Option<ResourceCalibrationReport>> {
        let connection = self.connection()?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='resource_calibrations')",
            [],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(None);
        }
        let mut statement = connection.prepare(
            "SELECT report_json FROM resource_calibrations WHERE state='complete' AND representation_kind=?1 AND recipe_key=?2 AND report_json IS NOT NULL ORDER BY updated_at DESC LIMIT 100",
        )?;
        let reports = statement
            .query_map(params![representation_kind, recipe_key], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for body in reports {
            let report: ResourceCalibrationReport = serde_json::from_str(&body)?;
            if report.identity.representation_kind != representation_kind
                || report.identity.recipe_key != recipe_key
                || !self.report_environment_matches_current(&report)?
            {
                continue;
            }
            if let Some(sample_identity) = sample_identity
                && report.identity.media.sample_identity != sample_identity
            {
                continue;
            }
            if report.identity.media.kind == "authorized_representative"
                && sample_identity.is_none()
            {
                continue;
            }
            return Ok(Some(report));
        }
        Ok(None)
    }

    pub(super) fn resource_environment_fingerprint(
        &self,
        representation_kind: &str,
        recipe_key: &str,
    ) -> Result<String> {
        let models = if representation_kind == SPEECH_REPRESENTATION_KIND {
            model_identity(&self.model_status()?)
        } else {
            Vec::new()
        };
        let scope = serde_json::json!({
            "schemaVersion": RESOURCE_CALIBRATION_SCHEMA_VERSION,
            "recipeKey": recipe_key,
            "recipeVersion": RESOURCE_CALIBRATION_RECIPE_VERSION,
            "host": host_identity(),
            "tools": tool_identities(),
            "devices": hardware_device_identities(),
            "models": models,
        });
        Ok(blake3::hash(&serde_json::to_vec(&scope)?)
            .to_hex()
            .to_string())
    }

    /// A history key is only reusable where every environment/configuration
    /// fact that contributed to the timing is still identical. This prevents a
    /// one-worker or software rate from being read on another host/build/GPU or
    /// under a different thread/worker setting.
    #[allow(clippy::too_many_arguments)]
    pub fn qualified_history_key(
        &self,
        base_key: &str,
        representation_kind: &str,
        recipe_key: &str,
        encoder: Option<&str>,
        worker_count: usize,
        vad_threads: usize,
        decode_threads: usize,
    ) -> Result<String> {
        let environment = self.resource_environment_fingerprint(representation_kind, recipe_key)?;
        let configuration = serde_json::json!({
            "encoder": encoder,
            "workerCount": worker_count,
            "vadThreads": vad_threads,
            "decodeThreads": decode_threads,
        });
        let configuration = blake3::hash(&serde_json::to_vec(&configuration)?)
            .to_hex()
            .to_string();
        Ok(format!(
            "{base_key}.h{}.c{}",
            &environment[..16],
            &configuration[..16]
        ))
    }
}
