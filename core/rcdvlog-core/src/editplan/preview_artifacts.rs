//! Durable ownership of one local preview attempt. Media identities and QA are
//! stored separately from small lifecycle rows so progress is O(cuts), not an
//! O(cuts × whole-project-manifest) rewrite stream.
use super::{EditPlanRevision, PreviewRequest, PreviewResult, preview_artifact_model::*};
use crate::Catalog;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::{
    cell::Cell,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub(super) struct PreviewRenderClaim {
    _file: File,
    pub workspace: PathBuf,
    pub artifact_id: String,
}

fn component(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value != "."
            && value != ".."
            && value.len() <= 255
            && !value.contains(['/', '\\'])
            && !value.chars().any(char::is_control),
        "invalid input: project/revision/artifact identity is not a safe cache component"
    );
    Ok(())
}
pub(super) fn project_directory(catalog: &Catalog) -> Result<PathBuf> {
    let parent = catalog
        .database_path()
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::canonicalize(parent).context("resolve project catalog directory")
}
pub(super) fn managed_child(parent: &Path, name: &str) -> Result<PathBuf> {
    component(name)?;
    let path = parent.join(name);
    match fs::create_dir(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    ensure!(
        !fs::symlink_metadata(&path)?.file_type().is_symlink() && fs::canonicalize(&path)? == path,
        "invalid input: managed preview cache cannot follow a directory symlink"
    );
    ensure!(
        path.is_dir(),
        "invalid input: managed preview cache path is not a directory"
    );
    Ok(path)
}
pub(super) fn workspace(
    catalog: &Catalog,
    project: &str,
    revision: &str,
    create: bool,
) -> Result<PathBuf> {
    component(project)?;
    component(revision)?;
    let mut path = project_directory(catalog)?;
    for name in ["cache", "previews", project, revision] {
        path = if create {
            managed_child(&path, name)?
        } else {
            path.join(name)
        };
    }
    Ok(path)
}
fn claim_file_path(catalog: &Catalog, project: &str, revision: &str) -> Result<PathBuf> {
    Ok(workspace(catalog, project, revision, false)?.join(".render.lock"))
}

pub(super) fn claim(catalog: &Catalog, plan: &EditPlanRevision) -> Result<PreviewRenderClaim> {
    let workspace = workspace(catalog, &plan.project_id, &plan.revision_id, true)?;
    let path = workspace.join(".render.lock");
    ensure!(
        !fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()),
        "invalid input: preview render claim cannot follow a symlink"
    );
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    fs2::FileExt::try_lock_exclusive(&file).map_err(|e| {
        if e.kind() == std::io::ErrorKind::WouldBlock {
            anyhow::Error::from(PreviewArtifactError::Busy {
                revision_id: plan.revision_id.clone(),
            })
        } else {
            e.into()
        }
    })?;
    let artifact_id = format!("preview_{}", uuid::Uuid::new_v4().simple());
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(artifact_id.as_bytes())?;
    file.sync_all()?;
    Ok(PreviewRenderClaim {
        _file: file,
        workspace,
        artifact_id,
    })
}

fn state(value: &str) -> Result<PreviewArtifactState> {
    match value {
        "running" => Ok(PreviewArtifactState::Running),
        "complete" => Ok(PreviewArtifactState::Complete),
        "cancelled" => Ok(PreviewArtifactState::Cancelled),
        "failed" => Ok(PreviewArtifactState::Failed),
        "interrupted" => Ok(PreviewArtifactState::Interrupted),
        _ => Err(PreviewArtifactError::InvalidReceipt.into()),
    }
}
fn load_base(connection: &Connection, id: &str) -> Result<PreviewArtifact> {
    let row=connection.query_row("SELECT body_json,generation,sequence,state,cancel_requested,project_id,revision_id FROM preview_artifacts WHERE artifact_id=?1",
        [id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,bool>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?))).optional()?
        .ok_or_else(||PreviewArtifactError::NotFound {artifact_id:id.into()})?;
    let mut artifact: PreviewArtifact =
        serde_json::from_str(&row.0).context("decode preview receipt")?;
    ensure!(
        (artifact.kind == "fullPreview" && artifact.excerpt.is_none())
            || (artifact.kind == "cutNeighborhood" && artifact.excerpt.is_some()),
        PreviewArtifactError::InvalidReceipt
    );
    ensure!(
        artifact
            .identity
            .as_ref()
            .is_none_or(|identity| identity.excerpt == artifact.excerpt),
        PreviewArtifactError::InvalidReceipt
    );
    ensure!(
        artifact.artifact_id == id
            && artifact.project_id == row.5
            && artifact.revision_id == row.6
            && artifact.request.revision_id == row.6,
        PreviewArtifactError::InvalidReceipt
    );
    component(id)?;
    component(&artifact.project_id)?;
    component(&artifact.revision_id)?;
    ensure!(row.1 > 0 && row.2 > 0, PreviewArtifactError::InvalidReceipt);
    artifact.generation = u64::try_from(row.1)?;
    artifact.sequence = u64::try_from(row.2)?;
    artifact.state = state(&row.3)?;
    artifact.cancel_requested = row.4;
    Ok(artifact)
}
fn save_base(connection: &Connection, artifact: &mut PreviewArtifact) -> Result<()> {
    // Large immutable payloads and cut outcome pages are reconstructed on reads.
    artifact.identity = None;
    artifact.result = None;
    artifact.validation = None;
    artifact.cut_outcomes.clear();
    artifact.outcome_total = 0;
    artifact.outcomes_truncated = false;
    artifact.sequence += 1;
    connection.execute(
        "UPDATE preview_artifacts SET state=?2,sequence=?3,body_json=?4 WHERE artifact_id=?1",
        params![
            artifact.artifact_id,
            artifact.state.as_str(),
            i64::try_from(artifact.sequence)?,
            serde_json::to_string(artifact)?
        ],
    )?;
    Ok(())
}
fn load_full(catalog: &Catalog, connection: &Connection, id: &str) -> Result<PreviewArtifact> {
    let mut artifact = load_base(connection, id)?;
    let (identity,result,validation):(Option<String>,Option<String>,Option<String>)=connection.query_row(
        "SELECT identity_json,result_json,validation_json FROM preview_artifacts WHERE artifact_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    artifact.identity = identity.map(|s| serde_json::from_str(&s)).transpose()?;
    artifact.result = result.map(|s| serde_json::from_str(&s)).transpose()?;
    artifact.validation = validation.map(|s| serde_json::from_str(&s)).transpose()?;
    if let Some(path) = output_path(catalog, &artifact)?
        && let Some(result) = &mut artifact.result
    {
        ensure!(
            result.artifact_id.as_deref() == Some(id),
            PreviewArtifactError::InvalidReceipt
        );
        result.output_path = path.to_string_lossy().into_owned();
    }
    if artifact.state == PreviewArtifactState::Complete {
        ensure!(
            artifact.result.is_some()
                && artifact.validation.is_some()
                && artifact.identity.is_some()
                && artifact.output_relative_path.is_some(),
            PreviewArtifactError::InvalidReceipt
        );
    }
    artifact.outcome_total = connection.query_row(
        "SELECT COUNT(*) FROM preview_cut_outcomes WHERE artifact_id=?1",
        [id],
        |r| r.get::<_, i64>(0),
    )? as usize;
    let mut statement=connection.prepare("SELECT body_json FROM preview_cut_outcomes WHERE artifact_id=?1 ORDER BY position LIMIT 100")?;
    let bodies = statement
        .query_map([id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    artifact.cut_outcomes = bodies
        .into_iter()
        .map(|s| serde_json::from_str(&s))
        .collect::<serde_json::Result<Vec<_>>>()?;
    artifact.outcomes_truncated = artifact.outcome_total > artifact.cut_outcomes.len();
    Ok(artifact)
}
fn output_path(catalog: &Catalog, artifact: &PreviewArtifact) -> Result<Option<PathBuf>> {
    let Some(relative) = &artifact.output_relative_path else {
        return Ok(None);
    };
    let relative = Path::new(relative);
    ensure!(
        !relative.is_absolute()
            && relative
                .components()
                .all(|p| matches!(p, std::path::Component::Normal(_))),
        PreviewArtifactError::InvalidReceipt
    );
    let expected = workspace(catalog, &artifact.project_id, &artifact.revision_id, false)?
        .join("artifacts")
        .join(&artifact.artifact_id)
        .join("preview.mp4");
    let path = project_directory(catalog)?.join(relative);
    ensure!(path == expected, PreviewArtifactError::InvalidReceipt);
    Ok(Some(path))
}
/// Observe only owned regular files, without following a substituted cache directory.
pub(super) fn output_is_owned_file(catalog: &Catalog, artifact: &PreviewArtifact) -> bool {
    let Ok(Some(path)) = output_path(catalog, artifact) else {
        return false;
    };
    let Ok(root) = project_directory(catalog) else {
        return false;
    };
    let Ok(relative) = path.strip_prefix(&root) else {
        return false;
    };
    let mut current = root;
    let mut components = relative.components().peekable();
    while let Some(part) = components.next() {
        current.push(part.as_os_str());
        let Ok(metadata) = fs::symlink_metadata(&current) else {
            return false;
        };
        if metadata.file_type().is_symlink()
            || if components.peek().is_some() {
                !metadata.is_dir()
            } else {
                !metadata.is_file()
            }
        {
            return false;
        }
    }
    true
}
fn observed_state(catalog: &Catalog, artifact: &PreviewArtifact) -> Result<PreviewArtifactState> {
    if artifact.state != PreviewArtifactState::Running {
        return Ok(artifact.state);
    }
    let path = claim_file_path(catalog, &artifact.project_id, &artifact.revision_id)?;
    let Ok(file) = OpenOptions::new().read(true).open(&path) else {
        return Ok(PreviewArtifactState::Interrupted);
    };
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(PreviewArtifactState::Interrupted),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            let mut owner = String::new();
            file.take(256).read_to_string(&mut owner)?;
            Ok(if owner == artifact.artifact_id {
                PreviewArtifactState::Running
            } else {
                PreviewArtifactState::Interrupted
            })
        }
        Err(e) => Err(e.into()),
    }
}
fn latest(connection: &Connection, id: &str) -> Result<bool> {
    Ok(connection.query_row("SELECT NOT EXISTS(SELECT 1 FROM preview_artifacts newer WHERE newer.request_key=current.request_key AND newer.generation>current.generation)
        FROM preview_artifacts current WHERE current.artifact_id=?1",[id],|r|r.get(0))?)
}

pub(super) struct PreviewAttempt<'a> {
    catalog: &'a Catalog,
    pub artifact_id: String,
    pub directory: PathBuf,
    cancel_reader: Connection,
    last_poll: Cell<Option<Instant>>,
    cancelled: Cell<bool>,
}
impl<'a> PreviewAttempt<'a> {
    pub fn begin(
        catalog: &'a Catalog,
        plan: &EditPlanRevision,
        request: &PreviewRequest,
        claim: &PreviewRenderClaim,
        excerpt: Option<&super::PreviewExcerptScope>,
    ) -> Result<Self> {
        let artifacts = managed_child(&claim.workspace, "artifacts")?;
        component(&claim.artifact_id)?;
        let directory = artifacts.join(&claim.artifact_id);
        fs::create_dir(&directory).context("create unique preview attempt workspace")?;
        let request_body = if let Some(scope) = excerpt {
            serde_json::to_vec(&(super::preview_excerpt::EXCERPT_RECIPE, request, scope))?
        } else {
            serde_json::to_vec(&("fullPreview", request))?
        };
        let request_key = blake3::hash(&request_body).to_hex().to_string();
        let mut connection = catalog.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement = transaction.prepare(
            "SELECT artifact_id FROM preview_artifacts WHERE revision_id=?1 AND state='running'",
        )?;
        let prior = statement
            .query_map([&plan.revision_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        for id in prior {
            let mut record = load_base(&transaction, &id)?;
            record.state = PreviewArtifactState::Interrupted;
            record.stage = "interrupted".into();
            record.completed_at = Some(crate::now());
            record.failure =
                Some("The prior attempt no longer owns the revision render claim.".into());
            save_base(&transaction, &mut record)?;
        }
        let mut artifact = PreviewArtifact {
            artifact_id: claim.artifact_id.clone(),
            project_id: plan.project_id.clone(),
            revision_id: plan.revision_id.clone(),
            generation: 0,
            request: request.clone(),
            kind: if excerpt.is_some() {
                "cutNeighborhood"
            } else {
                "fullPreview"
            }
            .into(),
            excerpt: excerpt.cloned(),
            state: PreviewArtifactState::Running,
            sequence: 1,
            stage: "validating".into(),
            cancel_requested: false,
            identity: None,
            result: None,
            validation: None,
            output_relative_path: None,
            failure: None,
            started_at: crate::now(),
            completed_at: None,
            cut_outcomes: vec![],
            outcome_total: 0,
            outcomes_truncated: false,
        };
        transaction.execute("INSERT INTO preview_artifacts(artifact_id,project_id,revision_id,request_key,state,sequence,body_json,started_at)
            VALUES(?1,?2,?3,?4,'running',1,?5,?6)",params![artifact.artifact_id,artifact.project_id,artifact.revision_id,request_key,serde_json::to_string(&artifact)?,artifact.started_at])?;
        artifact.generation = transaction.last_insert_rowid() as u64;
        transaction.execute(
            "UPDATE preview_artifacts SET body_json=?2 WHERE artifact_id=?1",
            params![artifact.artifact_id, serde_json::to_string(&artifact)?],
        )?;
        transaction.commit()?;
        Ok(Self {
            catalog,
            artifact_id: artifact.artifact_id,
            directory,
            cancel_reader: catalog.connection()?,
            last_poll: Cell::new(None),
            cancelled: Cell::new(false),
        })
    }
    pub fn is_cancelled(&self) -> bool {
        if self.cancelled.get() {
            return true;
        }
        if self
            .last_poll
            .get()
            .is_some_and(|time| time.elapsed() < Duration::from_millis(100))
        {
            return false;
        }
        self.last_poll.set(Some(Instant::now()));
        // An unreadable/missing owning receipt cannot authorize publication.
        let cancelled = self
            .cancel_reader
            .query_row(
                "SELECT cancel_requested FROM preview_artifacts WHERE artifact_id=?1",
                [&self.artifact_id],
                |r| r.get::<_, bool>(0),
            )
            .unwrap_or(true);
        self.cancelled.set(cancelled);
        cancelled
    }

    pub fn cancellation_requested_now(&self) -> bool {
        self.last_poll.set(None);
        self.is_cancelled()
    }
    pub fn stage(&self, stage: &str) -> Result<u64> {
        let mut connection = self.catalog.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut record = load_base(&transaction, &self.artifact_id)?;
        ensure!(
            record.state == PreviewArtifactState::Running,
            PreviewArtifactError::InvalidReceipt
        );
        record.stage = stage.into();
        save_base(&transaction, &mut record)?;
        transaction.commit()?;
        Ok(record.sequence)
    }
    pub fn identity(&self, identity: &PreviewInputIdentity) -> Result<()> {
        self.catalog.connection()?.execute("UPDATE preview_artifacts SET identity_json=?2 WHERE artifact_id=?1 AND state='running'",
            params![self.artifact_id,serde_json::to_string(identity)?])?;
        Ok(())
    }
    pub fn outcome(&self, outcome: &PreviewCutOutcome) -> Result<u64> {
        let mut connection = self.catalog.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO preview_cut_outcomes(artifact_id,position,body_json) VALUES(?1,?2,?3)
            ON CONFLICT(artifact_id,position) DO UPDATE SET body_json=excluded.body_json",
            params![
                self.artifact_id,
                outcome.position,
                serde_json::to_string(outcome)?
            ],
        )?;
        let mut record = load_base(&transaction, &self.artifact_id)?;
        record.stage = "rendering".into();
        save_base(&transaction, &mut record)?;
        transaction.commit()?;
        Ok(record.sequence)
    }
    pub fn finish_error(&self, error: &anyhow::Error, cancelled: bool) -> Result<u64> {
        let mut connection = self.catalog.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut record = load_base(&transaction, &self.artifact_id)?;
        if record.state == PreviewArtifactState::Complete {
            return Ok(record.sequence);
        }
        record.state = if cancelled {
            PreviewArtifactState::Cancelled
        } else {
            PreviewArtifactState::Failed
        };
        record.stage = record.state.as_str().into();
        record.failure = Some(error.to_string().chars().take(4096).collect());
        record.completed_at = Some(crate::now());
        save_base(&transaction, &mut record)?;
        transaction.commit()?;
        Ok(record.sequence)
    }
    pub fn complete(
        &self,
        transaction: &Transaction<'_>,
        result: &PreviewResult,
        validation: &PreviewMediaValidation,
    ) -> Result<u64> {
        let mut record = load_base(transaction, &self.artifact_id)?;
        ensure!(
            record.state == PreviewArtifactState::Running && !record.cancel_requested,
            "preview cancelled before publication"
        );
        let relative = Path::new(&result.output_path)
            .strip_prefix(project_directory(self.catalog)?)
            .context("preview output escaped project cache")?
            .to_path_buf();
        record.state = PreviewArtifactState::Complete;
        record.stage = "complete".into();
        record.completed_at = Some(crate::now());
        record.output_relative_path = Some(relative.to_string_lossy().into_owned());
        save_base(transaction, &mut record)?;
        transaction.execute(
            "UPDATE preview_artifacts SET result_json=?2,validation_json=?3 WHERE artifact_id=?1",
            params![
                self.artifact_id,
                serde_json::to_string(result)?,
                serde_json::to_string(validation)?
            ],
        )?;
        Ok(record.sequence)
    }
}

impl Catalog {
    pub fn preview_artifacts(
        &self,
        revision_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<PreviewArtifactPage> {
        ensure!(
            (1..=100).contains(&limit),
            "invalid input: preview page limit must be 1-100"
        );
        self.edit_plan_revision(revision_id)?;
        let connection = self.connection()?;
        if !crate::table_exists(&connection, "preview_artifacts")? {
            return Ok(PreviewArtifactPage {
                schema_ready: false,
                revision_id: revision_id.into(),
                total: 0,
                next_offset: None,
                items: vec![],
            });
        }
        let connection = connection.unchecked_transaction()?;
        let total = connection.query_row(
            "SELECT COUNT(*) FROM preview_artifacts WHERE revision_id=?1",
            [revision_id],
            |r| r.get::<_, i64>(0),
        )? as usize;
        let mut statement=connection.prepare("SELECT artifact_id FROM preview_artifacts WHERE revision_id=?1 ORDER BY generation DESC LIMIT ?2 OFFSET ?3")?;
        let ids = statement
            .query_map(
                params![revision_id, i64::try_from(limit)?, i64::try_from(offset)?],
                |r| r.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let mut items = Vec::new();
        for id in ids {
            let record = load_base(&connection, &id)?;
            let observed = observed_state(self, &record)?;
            let result: Option<String> = connection.query_row(
                "SELECT result_json FROM preview_artifacts WHERE artifact_id=?1",
                [id],
                |r| r.get(0),
            )?;
            let result: Option<PreviewResult> =
                result.map(|s| serde_json::from_str(&s)).transpose()?;
            let path = output_path(self, &record)?.map(|p| p.to_string_lossy().into_owned());
            items.push(PreviewArtifactSummary {
                artifact_id: record.artifact_id,
                revision_id: record.revision_id,
                kind: record.kind,
                excerpt: record.excerpt,
                generation: record.generation,
                sequence: record.sequence,
                state: record.state,
                observed_state: observed,
                stage: record.stage,
                cancel_requested: record.cancel_requested,
                max_height: record.request.max_height,
                started_at: record.started_at,
                completed_at: record.completed_at,
                output_path: path,
                duration_seconds: result.as_ref().map(|r| r.duration_seconds),
                clip_count: result.map(|r| r.clip_count),
            });
        }
        Ok(PreviewArtifactPage {
            schema_ready: true,
            revision_id: revision_id.into(),
            total,
            next_offset: (offset.saturating_add(limit) < total)
                .then_some(offset.saturating_add(limit)),
            items,
        })
    }
    pub fn cancel_preview_artifact(&self, artifact_id: &str) -> Result<PreviewArtifactStatus> {
        let status = self.preview_artifact_status(artifact_id, false)?;
        if status.observed_state != PreviewArtifactState::Running {
            return Err(PreviewArtifactError::AlreadyTerminal {
                state: status.observed_state.as_str().into(),
            }
            .into());
        }
        self.connection()?.execute("UPDATE preview_artifacts SET cancel_requested=1,sequence=sequence+1 WHERE artifact_id=?1 AND state='running'",[artifact_id])?;
        self.preview_artifact_status(artifact_id, false)
    }
    pub fn wait_preview_artifact(
        &self,
        artifact_id: &str,
        timeout: Duration,
    ) -> Result<PreviewArtifactStatus> {
        ensure!(
            timeout <= Duration::from_secs(86400),
            "invalid input: preview observation window exceeds 24 hours"
        );
        let start = Instant::now();
        loop {
            let status = self.preview_artifact_status(artifact_id, false)?;
            if status.observed_state != PreviewArtifactState::Running {
                return Ok(status);
            }
            if start.elapsed() >= timeout {
                return Err(PreviewArtifactError::ObservationTimedOut {
                    artifact_id: artifact_id.into(),
                }
                .into());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    pub fn preview_artifact_status(
        &self,
        artifact_id: &str,
        verify: bool,
    ) -> Result<PreviewArtifactStatus> {
        self.preview_artifact_status_cancellable(artifact_id, verify, &|| false)
    }

    pub(crate) fn preview_artifact_status_cancellable(
        &self,
        artifact_id: &str,
        verify: bool,
        cancel: &dyn Fn() -> bool,
    ) -> Result<PreviewArtifactStatus> {
        let connection = self.connection()?;
        if !crate::table_exists(&connection, "preview_artifacts")? {
            return Err(PreviewArtifactError::NotFound {
                artifact_id: artifact_id.into(),
            }
            .into());
        }
        // One SQLite read snapshot owns the receipt, large manifests, outcomes,
        // latest generation and active revision. Never expose a mixed lifecycle row.
        let snapshot = connection.unchecked_transaction()?;
        let mut artifact = load_full(self, &snapshot, artifact_id)?;
        let mut is_latest = latest(&snapshot, artifact_id)?;
        let mut active_revision_id = snapshot
            .query_row(
                "SELECT revision_id FROM edit_plan_revisions WHERE project_id=?1 AND active=1",
                [&artifact.project_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        snapshot.commit()?;
        let mut observed = observed_state(self, &artifact)?;
        if artifact.state == PreviewArtifactState::Running
            && observed == PreviewArtifactState::Interrupted
        {
            // The writer may have committed and released its claim after our
            // snapshot. Re-read before declaring a lost writer or ending a wait.
            let snapshot = connection.unchecked_transaction()?;
            artifact = load_full(self, &snapshot, artifact_id)?;
            is_latest = latest(&snapshot, artifact_id)?;
            active_revision_id = snapshot
                .query_row(
                    "SELECT revision_id FROM edit_plan_revisions WHERE project_id=?1 AND active=1",
                    [&artifact.project_id],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            snapshot.commit()?;
            observed = observed_state(self, &artifact)?;
        }
        let file_present = output_is_owned_file(self, &artifact);
        let mut status = PreviewArtifactStatus {
            artifact: artifact.clone(),
            observed_state: observed,
            latest_attempt: is_latest,
            active_revision_id,
            file_present,
            current_for_request: if observed != PreviewArtifactState::Complete || !file_present {
                Some(false)
            } else {
                None
            },
            verification: None,
        };
        if verify && observed == PreviewArtifactState::Complete {
            let _heavy = self.acquire_local_heavy_cancellable(cancel)?;
            let verification = super::preview_media::verify_artifact(self, &artifact, cancel)?;
            // Verification may outlast a newly requested render generation.
            status.latest_attempt = latest(&self.connection()?, artifact_id)?;
            status.active_revision_id = self.active_edit_plan_revision_id(&artifact.project_id)?;
            status.file_present = output_is_owned_file(self, &artifact);
            status.current_for_request = Some(
                status.file_present
                    && verification.output_matches
                    && verification.inputs_match
                    && verification.constraints_match
                    && verification.plan_renderable,
            );
            status.verification = Some(verification);
        }
        artifact.cut_outcomes.shrink_to_fit();
        status.artifact = artifact;
        Ok(status)
    }
}

#[cfg(test)]
#[path = "preview_artifacts_tests.rs"]
mod tests;
