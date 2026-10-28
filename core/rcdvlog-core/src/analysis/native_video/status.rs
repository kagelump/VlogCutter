//! Bounded observation and explicit cancellation of an existing native run.
use super::*;
use crate::Catalog;
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
impl Catalog {
    pub fn list_native_video_run_windows(
        &self,
        project_id: &str,
        run_id: &str,
        after_index: Option<u32>,
        limit: u32,
    ) -> Result<Vec<NativeVideoRunWindowStatus>> {
        ensure!(
            (1..=100).contains(&limit),
            "native run-window page must contain 1-100 rows"
        );
        self.native_video_run_status(project_id, run_id)?;
        let connection = self.connection()?;
        let mut statement=connection.prepare("SELECT window_index,request_json,state,evidence_id,failure_code,reused FROM native_video_run_windows WHERE run_id=?1 AND window_index>?2 ORDER BY window_index LIMIT ?3")?;
        let rows = statement.query_map(
            params![run_id, after_index.map(i64::from).unwrap_or(-1), limit],
            |r| {
                Ok((
                    r.get::<_, u32>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, bool>(5)?,
                ))
            },
        )?;
        rows.map(|row| {
            let (window_index, request, state, evidence_id, failure_code, reused) = row?;
            ensure!(
                request.len() <= 65536,
                "native window metadata exceeds bound"
            );
            Ok(NativeVideoRunWindowStatus {
                window_index,
                request: serde_json::from_str(&request)?,
                state,
                evidence_id,
                failure_code,
                reused,
            })
        })
        .collect()
    }
    pub fn list_native_video_windows(
        &self,
        project_id: &str,
        policy_id: &str,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<NativeVideoWindowStatus>> {
        self.require_project(project_id)?;
        ensure!(
            (1..=100).contains(&limit),
            "native evidence page must contain 1-100 rows"
        );
        let connection = self.connection()?;
        let mut statement=connection.prepare("SELECT evidence_id,run_id,policy_id,state,request_json,provenance_json,raw_response IS NOT NULL,result_json IS NOT NULL,failure_code,semantic_identity FROM native_video_calls WHERE project_id=?1 AND policy_id=?2 AND evidence_id>?3 ORDER BY evidence_id LIMIT ?4")?;
        let rows = statement.query_map(
            params![project_id, policy_id, after.unwrap_or(""), limit],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, bool>(6)?,
                    r.get::<_, bool>(7)?,
                    r.get::<_, Option<String>>(8)?,
                    r.get::<_, String>(9)?,
                ))
            },
        )?;
        let mut result = Vec::new();
        for row in rows {
            let (
                evidence_id,
                run_id,
                policy_id,
                stored_state,
                request,
                provenance,
                raw_response_available,
                result_available,
                failure_code,
                semantic,
            ) = row?;
            ensure!(
                request.len() <= 65536 && provenance.len() <= 1024 * 1024,
                "native evidence metadata exceeds bounds"
            );
            let observed_state = if matches!(stored_state.as_str(), "prepared" | "reserved") {
                observe_claim(self, &semantic)
            } else {
                stored_state.clone()
            };
            result.push(NativeVideoWindowStatus {
                evidence_id,
                run_id,
                policy_id,
                stored_state,
                observed_state,
                request: serde_json::from_str(&request)?,
                provenance: serde_json::from_str(&provenance)?,
                raw_response_available,
                result_available,
                failure_code,
            });
        }
        Ok(result)
    }
    pub(crate) fn native_video_cancel_requested(&self, run_id: &str) -> Result<bool> {
        self.connection()?
            .query_row(
                "SELECT cancel_requested FROM native_video_run_details WHERE run_id=?1",
                [run_id],
                |r| r.get(0),
            )
            .optional()
            .map(|v| v.unwrap_or(false))
            .map_err(Into::into)
    }
    pub fn cancel_native_video_run(&self, project_id: &str, run_id: &str) -> Result<bool> {
        self.require_project(project_id)?;
        let changed=self.connection()?.execute("UPDATE native_video_run_details SET cancel_requested=1,updated_at=?3 WHERE run_id=?1 AND EXISTS(SELECT 1 FROM analysis_runs r WHERE r.id=?1 AND r.project_id=?2 AND r.status='running')",params![run_id,project_id,crate::now()])?;
        Ok(changed == 1)
    }
    pub fn native_video_run_status(
        &self,
        project_id: &str,
        run_id: &str,
    ) -> Result<NativeVideoRunStatus> {
        self.require_project(project_id)?;
        let (policy_id,mut state,cancel,progress,report,budget_stopped):(String,String,bool,String,Option<String>,bool)=self.connection()?.query_row("SELECT d.policy_id,r.status,d.cancel_requested,d.progress_json,d.report_json,r.budget_exhausted FROM native_video_run_details d JOIN analysis_runs r ON r.id=d.run_id WHERE d.run_id=?1 AND r.project_id=?2",params![run_id,project_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).context("native run not found in project")?;
        if state == "completed" && budget_stopped {
            state = "budget_stopped".into();
        }
        ensure!(
            progress.len() <= 1024 * 1024
                && report.as_ref().is_none_or(|s| s.len() <= 2 * 1024 * 1024),
            "native status exceeds bounded size"
        );
        if state == "running" {
            ensure!(
                policy_id.len() == 64 && policy_id.bytes().all(|b| b.is_ascii_hexdigit()),
                "native status policy identity is malformed"
            );
            let relative = format!("cache/native-video/claims/policy_{policy_id}.lock");
            let root = self
                .database_path()
                .parent()
                .context("catalog has no local root")?;
            let mut path = root.to_path_buf();
            let observed = (|| -> Result<bool> {
                for part in std::path::Path::new(&relative).components() {
                    path.push(part);
                    let m = std::fs::symlink_metadata(&path)?;
                    ensure!(
                        !m.file_type().is_symlink(),
                        "native status cannot follow claim symlinks"
                    );
                }
                let file = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)?;
                match fs2::FileExt::try_lock_exclusive(&file) {
                    Ok(()) => Ok(false),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
                    Err(e) => Err(e.into()),
                }
            })();
            state = match observed {
                Ok(true) => "running",
                Ok(false) => "interrupted",
                Err(_) => "ownerUnknown",
            }
            .into();
        }
        let mut progress: NativeVideoProgress = serde_json::from_str(&progress)?;
        let aggregate = self.analysis_accounting_aggregate(&policy_id, Some(run_id))?;
        progress.estimated_cost_usd = aggregate.estimated_cost_usd;
        progress.confirmed_cost_usd = aggregate.confirmed_cost_usd;
        progress.uncertain_usage_reserve_usd = aggregate.uncertain_usage_reserve_usd;
        progress.active_reserved_cost_usd = aggregate.active_reserved_cost_usd;
        progress.committed_or_reserved_usd = aggregate.committed_or_reserved_usd;
        let mut report = report
            .map(|s| serde_json::from_str::<NativeVideoRunReport>(&s))
            .transpose()?;
        if let Some(report) = &mut report {
            report.accounting = aggregate;
        }
        Ok(NativeVideoRunStatus {
            run_id: run_id.into(),
            policy_id,
            state,
            cancel_requested: cancel,
            progress,
            report,
        })
    }
    /// A bounded interruptible observer; timeout returns the current state.
    pub fn wait_native_video_run(
        &self,
        project_id: &str,
        run_id: &str,
        timeout: std::time::Duration,
    ) -> Result<NativeVideoRunStatus> {
        ensure!(
            timeout <= std::time::Duration::from_secs(3600),
            "native wait must be at most one hour"
        );
        let started = std::time::Instant::now();
        loop {
            let status = self.native_video_run_status(project_id, run_id)?;
            if status.state != "running" || started.elapsed() >= timeout {
                return Ok(status);
            }
            std::thread::sleep(
                std::time::Duration::from_millis(200)
                    .min(timeout.saturating_sub(started.elapsed())),
            );
        }
    }
}
fn observe_claim(catalog: &Catalog, key: &str) -> String {
    if !(key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit())) {
        return "ownerUnknown".into();
    }
    let observed = (|| -> Result<bool> {
        let mut path = catalog
            .database_path()
            .parent()
            .context("catalog has no local root")?
            .to_path_buf();
        for part in ["cache", "native-video", "claims", &format!("{key}.lock")] {
            path.push(part);
            let m = std::fs::symlink_metadata(&path)?;
            ensure!(
                !m.file_type().is_symlink(),
                "native status cannot follow symlink"
            );
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(false),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
            Err(e) => Err(e.into()),
        }
    })();
    match observed {
        Ok(true) => "running",
        Ok(false) => "interrupted",
        Err(_) => "ownerUnknown",
    }
    .into()
}
