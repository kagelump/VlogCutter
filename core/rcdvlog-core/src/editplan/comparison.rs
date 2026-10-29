//! Bounded local comparisons reuse preview ownership and canonical human feedback.
use super::*;
#[cfg(test)]
#[path = "comparison_tests.rs"]
mod tests;
use crate::{AssembledContextRequest, Catalog, operations::commit_local_in_transaction};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    fs::{self, OpenOptions},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparisonVariantRequest {
    pub name: String,
    pub revision_id: String,
    pub scope: PreviewExcerptScope,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShortComparisonRequest {
    pub project_id: String,
    pub intent: String,
    pub max_height: u32,
    pub variants: Vec<ComparisonVariantRequest>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComparisonVariant {
    pub name: String,
    pub revision_id: String,
    pub scope: PreviewExcerptScope,
    pub context_evidence_id: String,
    pub preview: PreviewResult,
    pub validation: PreviewMediaValidation,
    pub full_timeline_origin_seconds: f64,
    pub audio_mappings: Vec<serde_json::Value>,
    pub elapsed_seconds: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortComparison {
    pub id: String,
    pub recipe: String,
    pub request: ShortComparisonRequest,
    pub state: String,
    pub variants: Vec<ComparisonVariant>,
    pub pending_artifact_id: Option<String>,
    pub player: Option<InspectionFile>,
    pub failure: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortComparisonStatus {
    pub comparison: ShortComparison,
    pub observed_state: String,
    pub cancel_requested: bool,
    pub current_for_request: Option<bool>,
    pub variant_verification: Vec<PreviewArtifactStatus>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparisonPreferenceRequest {
    pub comparison_id: String,
    pub preferred_variant: String,
    pub expected_preferred_revision_id: String,
    /// The literal "none" explicitly guards an unactivated project.
    pub expected_active_revision_id: String,
    pub decision_owner: super::RangeDecisionOwner,
    pub rationale: String,
    /// Explicit retention only; choosing a variant never invents hard locks.
    pub retain_operation_ids: Vec<String>,
    pub create_draft: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComparisonChoice {
    pub choice_id: String,
    pub comparison_id: String,
    pub preferred_revision_id: String,
    pub preferred_variant: String,
    pub note_ids: Vec<String>,
    pub expected_active_revision_id: String,
    pub draft_revision_id: Option<String>,
    pub draft_failure: Option<String>,
}

const RECIPE: &str = "short-comparison.scoped-preview.sequential-player.v1";
fn expected(value: &str) -> Option<&str> {
    (value != "none").then_some(value)
}
fn directory(catalog: &Catalog, id: &str) -> Result<std::path::PathBuf> {
    ensure!(
        id.strip_prefix("compare_")
            .is_some_and(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())),
        "invalid input: comparison ID"
    );
    let root = super::preview_artifacts::managed_child(
        &super::preview_artifacts::project_directory(catalog)?,
        "cache",
    )?;
    let root = super::preview_artifacts::managed_child(&root, "comparisons")?;
    super::preview_artifacts::managed_child(&root, id)
}
fn claim(catalog: &Catalog, id: &str) -> Result<std::fs::File> {
    let path = directory(catalog, id)?.join(".lock");
    ensure!(
        !fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()),
        "comparison claim is a symlink"
    );
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    fs2::FileExt::try_lock_exclusive(&file).context("comparison is already running")?;
    Ok(file)
}

impl Catalog {
    fn save_short_comparison(&self, comparison: &ShortComparison) -> Result<()> {
        let body = serde_json::to_string(comparison)?;
        ensure!(
            body.len() <= 2 * 1024 * 1024,
            "comparison manifest exceeds 2 MiB"
        );
        self.connection()?.execute(
            "UPDATE preview_comparisons SET state=?2,body_json=?3,updated_at=?4 WHERE id=?1",
            params![comparison.id, comparison.state, body, crate::now()],
        )?;
        Ok(())
    }

    pub fn create_short_comparison(
        &self,
        request: &ShortComparisonRequest,
        cancel: &dyn Fn() -> bool,
        progress: &mut dyn FnMut(&str, PreviewProgress),
    ) -> Result<ShortComparison> {
        ensure!(
            (2..=4).contains(&request.variants.len())
                && (144..=1080).contains(&request.max_height)
                && !request.intent.trim().is_empty()
                && request.intent.len() <= 4096,
            "invalid input: comparison limits"
        );
        let mut names = std::collections::BTreeSet::new();
        let mut scopes = std::collections::BTreeSet::new();
        for v in &request.variants {
            ensure!(
                scopes.insert((
                    &v.revision_id,
                    &v.scope.first_operation_id,
                    &v.scope.last_operation_id
                )),
                "invalid input: duplicate comparison variant scope"
            );
            ensure!(
                !v.name.trim().is_empty()
                    && v.name.len() <= 80
                    && !v.name.chars().any(char::is_control)
                    && names.insert(&v.name),
                "invalid input: variant names must be unique and bounded"
            );
            let plan = self.edit_plan_revision(&v.revision_id)?;
            ensure!(
                plan.project_id == request.project_id,
                "invalid input: comparison revision belongs to another project"
            );
            super::preview_excerpt::resolve(&plan, &v.scope)?;
        }
        ensure!(!cancel(), "comparison cancelled before work");
        let mut comparison = ShortComparison {
            id: format!("compare_{}", uuid::Uuid::new_v4().simple()),
            recipe: RECIPE.into(),
            request: request.clone(),
            state: "running".into(),
            variants: vec![],
            pending_artifact_id: None,
            player: None,
            failure: None,
        };
        let _claim = claim(self, &comparison.id)?;
        self.connection()?.execute("INSERT INTO preview_comparisons(id,project_id,state,body_json,updated_at) VALUES(?1,?2,'running',?3,?4)",params![comparison.id,request.project_id,serde_json::to_string(&comparison)?,crate::now()])?;
        let comparison_id = comparison.id.clone();
        let last_poll = Cell::new(None::<Instant>);
        let stopped = Cell::new(false);
        let cancelled = || {
            if cancel() || stopped.get() {
                return true;
            }
            if last_poll
                .get()
                .is_none_or(|t| t.elapsed() >= Duration::from_millis(100))
            {
                last_poll.set(Some(Instant::now()));
                stopped.set(
                    self.connection()
                        .and_then(|c| {
                            Ok(c.query_row(
                                "SELECT cancel_requested FROM preview_comparisons WHERE id=?1",
                                [&comparison_id],
                                |r| r.get::<_, bool>(0),
                            )?)
                        })
                        .unwrap_or(true),
                );
            }
            stopped.get()
        };
        let outcome = (|| -> Result<()> {
            for variant in &request.variants {
                ensure!(!cancelled(), "comparison cancelled");
                let started = Instant::now();
                let preview=self.render_preview_excerpt(&PreviewExcerptRequest{revision_id:variant.revision_id.clone(),max_height:request.max_height,scope:variant.scope.clone()},&mut |event| {
                    if let PreviewProgress::Started{artifact_id:Some(id),..}=&event {
                        let _=self.connection().and_then(|c|Ok(c.execute("UPDATE preview_comparisons SET active_artifact_id=?2 WHERE id=?1",params![comparison.id,id])?));
                    }
                    progress(&variant.name,event);
                },&cancelled)?;
                comparison.pending_artifact_id = preview.artifact_id.clone();
                self.save_short_comparison(&comparison)?;
                let context = self.assembled_review_context(
                    &AssembledContextRequest {
                        artifact_id: preview
                            .artifact_id
                            .clone()
                            .context("comparison preview lacks artifact")?,
                        intent: request.intent.clone(),
                        source_handles: vec![],
                        max_handle_media_seconds: 0.0,
                    },
                    &cancelled,
                )?;
                comparison.variants.push(ComparisonVariant {
                    name: variant.name.clone(),
                    revision_id: variant.revision_id.clone(),
                    scope: variant.scope.clone(),
                    context_evidence_id: context.evidence_id,
                    preview,
                    validation: context.validation,
                    full_timeline_origin_seconds: context.full_timeline_origin_seconds,
                    audio_mappings: context.audio_mappings,
                    elapsed_seconds: started.elapsed().as_secs_f64(),
                });
                comparison.pending_artifact_id = None;
                self.save_short_comparison(&comparison)?;
                self.connection()?.execute(
                    "UPDATE preview_comparisons SET active_artifact_id=NULL WHERE id=?1",
                    [&comparison.id],
                )?;
            }
            ensure!(!cancelled(), "comparison cancelled");
            let variants = comparison
                .variants
                .iter()
                .map(|v| {
                    Ok((
                        v.name.clone(),
                        reqwest::Url::from_file_path(&v.preview.output_path)
                            .map_err(|_| {
                                anyhow::anyhow!("comparison output is not an absolute file path")
                            })?
                            .to_string(),
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            let path = directory(self, &comparison.id)?.join("player.html");
            let html = super::comparison_player::player_html(&variants);
            ensure!(html.len() <= 64 * 1024, "comparison player exceeds bound");
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)?;
            use std::io::Write;
            file.write_all(html.as_bytes())?;
            file.sync_all()?;
            let hash = crate::full_file_blake3(&path, &cancelled)?;
            comparison.player = Some(InspectionFile {
                path: path.to_string_lossy().into_owned(),
                blake3: hash.hex_digest,
                byte_length: hash.byte_count,
            });
            Ok(())
        })();
        comparison.state = if outcome.is_ok() {
            "complete"
        } else if cancelled() {
            "cancelled"
        } else {
            "failed"
        }
        .into();
        if let Err(e) = outcome {
            comparison.failure = Some(e.to_string());
            comparison.player = None;
        }
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stopped: bool = tx.query_row(
            "SELECT cancel_requested FROM preview_comparisons WHERE id=?1",
            [&comparison.id],
            |r| r.get(0),
        )?;
        if comparison.pending_artifact_id.is_none() && comparison.state != "complete" {
            comparison.pending_artifact_id = tx.query_row(
                "SELECT active_artifact_id FROM preview_comparisons WHERE id=?1",
                [&comparison.id],
                |r| r.get(0),
            )?;
        }
        if stopped && comparison.state == "complete" {
            comparison.state = "cancelled".into();
            comparison.player = None;
            comparison.failure = Some("Comparison cancelled before publication".into());
        }
        tx.execute("UPDATE preview_comparisons SET state=?2,body_json=?3,active_artifact_id=NULL,updated_at=?4 WHERE id=?1",params![comparison.id,comparison.state,serde_json::to_string(&comparison)?,crate::now()])?;
        tx.commit()?;
        Ok(comparison)
    }

    /// Small SQLite and claim observation by default. Verification is explicit.
    pub fn short_comparison_status(&self, id: &str, verify: bool) -> Result<ShortComparisonStatus> {
        let (body, cancel): (String, bool) = self
            .connection()?
            .query_row(
                "SELECT body_json,cancel_requested FROM preview_comparisons WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .context("comparison not found")?;
        ensure!(
            body.len() <= 2 * 1024 * 1024,
            "comparison manifest exceeds bound"
        );
        let comparison: ShortComparison = serde_json::from_str(&body)?;
        ensure!(
            comparison.id == id
                && id
                    .strip_prefix("compare_")
                    .is_some_and(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())),
            "invalid comparison identity"
        );
        let owned_dir = super::preview_artifacts::project_directory(self)?
            .join("cache")
            .join("comparisons")
            .join(id);
        let directory_current =
            fs::canonicalize(&owned_dir).is_ok_and(|canonical| canonical == owned_dir);

        let mut observed = comparison.state.clone();
        if observed == "running" {
            let path = owned_dir.join(".lock");
            observed = "ownerUnknown".into();
            if directory_current
                && fs::symlink_metadata(&path)
                    .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
                && let Ok(file) = std::fs::File::open(path)
            {
                match fs2::FileExt::try_lock_shared(&file) {
                    Ok(()) => observed = "interrupted".into(),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        observed = "running".into()
                    }
                    Err(_) => {}
                }
            }
        }
        let mut status = ShortComparisonStatus {
            comparison,
            observed_state: observed,
            cancel_requested: cancel,
            current_for_request: None,
            variant_verification: vec![],
        };
        if status.comparison.state != "complete" {
            status.current_for_request = Some(false);
        } else if verify {
            let mut valid = status.comparison.recipe == RECIPE
                && status.comparison.variants.len() == status.comparison.request.variants.len();
            for variant in &status.comparison.variants {
                let artifact = self.preview_artifact_status(
                    variant
                        .preview
                        .artifact_id
                        .as_deref()
                        .context("comparison variant lacks artifact")?,
                    true,
                )?;
                valid &= artifact.current_for_request == Some(true)
                    && artifact.artifact.revision_id == variant.revision_id
                    && artifact.artifact.excerpt.as_ref() == Some(&variant.scope);
                status.variant_verification.push(artifact);
            }
            valid &= status.comparison.player.as_ref().is_some_and(|player| {
                let expected = owned_dir.join("player.html");
                directory_current
                    && std::path::Path::new(&player.path) == expected
                    && fs::symlink_metadata(&expected).is_ok_and(|m| {
                        m.is_file()
                            && !m.file_type().is_symlink()
                            && m.len() == player.byte_length
                            && m.len() <= 64 * 1024
                    })
                    && crate::full_file_blake3(&expected, &|| false).is_ok_and(|hash| {
                        hash.hex_digest == player.blake3 && hash.byte_count == player.byte_length
                    })
            });
            status.current_for_request = Some(valid);
        }
        Ok(status)
    }

    pub fn cancel_short_comparison(&self, id: &str) -> Result<ShortComparisonStatus> {
        self.short_comparison_status(id, false)?;
        self.connection()?.execute("UPDATE preview_comparisons SET cancel_requested=1,updated_at=?2 WHERE id=?1 AND state='running'",params![id,crate::now()])?;
        let active: Option<String> = self.connection()?.query_row(
            "SELECT active_artifact_id FROM preview_comparisons WHERE id=?1",
            [id],
            |r| r.get(0),
        )?;
        if let Some(artifact) = active {
            let _ = self.cancel_preview_artifact(&artifact);
        }
        self.short_comparison_status(id, false)
    }

    /// Only this explicit human command records a preference. Reviewer tool
    /// sessions cannot call it. The canonical vote/retention facts are feedback;
    /// this table stores their local comparison and draft references only.
    pub fn prefer_short_comparison(
        &self,
        request: &ComparisonPreferenceRequest,
    ) -> Result<ComparisonChoice> {
        ensure!(
            !request.rationale.trim().is_empty()
                && request.rationale.len() <= 3500
                && request.retain_operation_ids.len() <= 12,
            "invalid input: preference rationale or retention bounds"
        );
        let status = self.short_comparison_status(&request.comparison_id, true)?;
        ensure!(
            status.current_for_request == Some(true),
            "stale evidence: comparison requires complete current verified variants"
        );
        let comparison = status.comparison;
        let winner = comparison
            .variants
            .iter()
            .find(|v| v.name == request.preferred_variant)
            .context("invalid input: preferred variant not found")?;
        ensure!(
            winner.revision_id == request.expected_preferred_revision_id,
            "stale revision: preferred variant differs from expected revision"
        );
        ensure!(
            self.active_edit_plan_revision_id(&comparison.request.project_id)?
                .as_deref()
                == expected(&request.expected_active_revision_id),
            "stale revision: active edit plan changed"
        );
        let mut retained = std::collections::BTreeSet::new();
        for id in &request.retain_operation_ids {
            ensure!(
                retained.insert(id.clone())
                    && winner.validation.cuts.iter().any(|c| c.operation_id == *id),
                "invalid input: retained cuts must be distinct cuts actually viewed in the preferred variant"
            );
        }
        let first = winner
            .validation
            .cuts
            .first()
            .context("preferred variant has no cuts")?;
        let mut note_ids = Vec::new();
        let mut ids = vec![first.operation_id.clone()];
        ids.extend(
            request
                .retain_operation_ids
                .iter()
                .filter(|id| **id != first.operation_id)
                .cloned(),
        );
        let mut decisions = Vec::new();
        for operation_id in ids {
            let note = self.record_assembled_review_note(
                &winner.context_evidence_id,
                &super::feedback::RecordReviewNoteRequest {
                    project_id: comparison.request.project_id.clone(),
                    revision_id: winner.revision_id.clone(),
                    operation_id: operation_id.clone(),
                    author: super::feedback::ReviewAuthor::Human,
                    category: super::feedback::ReviewCategory::Preference,
                    observation: format!(
                        "Preferred variant {}. {}",
                        winner.name, request.rationale
                    ),
                    viewed_artifact_id: winner.preview.artifact_id.clone(),
                    movie_time_seconds: None,
                    evidence_refs: vec![comparison.id.clone()],
                },
            )?;
            if retained.contains(&operation_id) {
                decisions.push(super::feedback::DecideReviewFeedbackRequest {
                    note_id: note.note.note_id.clone(),
                    expected_revision: note.revision,
                    proposal_id: None,
                    decision_owner: request.decision_owner,
                    action: super::feedback::ReviewDecisionAction::Accepted,
                    rationale: request.rationale.clone(),
                    hard_choice: Some(super::feedback::ReviewHardChoice::KeepPictureAndAudio),
                });
            }
            note_ids.push(note.note.note_id);
        }
        for decision in decisions {
            self.decide_review_feedback(&decision)?;
        }
        let mut choice = ComparisonChoice {
            choice_id: format!("compare_choice_{}", uuid::Uuid::new_v4().simple()),
            comparison_id: comparison.id,
            preferred_revision_id: winner.revision_id.clone(),
            preferred_variant: winner.name.clone(),
            note_ids,
            expected_active_revision_id: request.expected_active_revision_id.clone(),
            draft_revision_id: None,
            draft_failure: None,
        };
        self.connection()?.execute("INSERT INTO preview_comparison_choices(id,comparison_id,body_json,created_at,updated_at) VALUES(?1,?2,?3,?4,?4)",params![choice.choice_id,choice.comparison_id,serde_json::to_string(&choice)?,crate::now()])?;
        if request.create_draft {
            match self
                .apply_comparison_choice(&choice.choice_id, &request.expected_active_revision_id)
            {
                Ok(applied) => choice = applied,
                Err(error) => {
                    // The human vote remains canonical if active work changed.
                    // Report its durable receipt rather than losing the vote or
                    // forcing a new active revision after a failed draft guard.
                    choice.draft_failure = Some(error.to_string());
                    self.connection()?.execute("UPDATE preview_comparison_choices SET body_json=?2,updated_at=?3 WHERE id=?1",params![choice.choice_id,serde_json::to_string(&choice)?,crate::now()])?;
                }
            }
        }
        Ok(choice)
    }

    pub fn comparison_choice(&self, id: &str) -> Result<ComparisonChoice> {
        let body: String = self
            .connection()?
            .query_row(
                "SELECT body_json FROM preview_comparison_choices WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .context("comparison choice not found")?;
        ensure!(
            body.len() <= 64 * 1024,
            "comparison choice receipt exceeds bound"
        );
        Ok(serde_json::from_str(&body)?)
    }

    /// The normal revision factory and explicit active guard own the only write
    /// path. Creation and receipt linkage commit atomically, so local retries
    /// cannot create another draft after an interrupted result write.
    pub fn apply_comparison_choice(
        &self,
        id: &str,
        expected_active: &str,
    ) -> Result<ComparisonChoice> {
        let mut choice = self.comparison_choice(id)?;
        if choice.draft_revision_id.is_some() {
            return Ok(choice);
        }
        let status = self.short_comparison_status(&choice.comparison_id, false)?;
        let selected = status
            .comparison
            .variants
            .iter()
            .find(|v| {
                v.name == choice.preferred_variant && v.revision_id == choice.preferred_revision_id
            })
            .context("comparison choice variant no longer exists")?;
        let verified = self.preview_artifact_status(
            selected
                .preview
                .artifact_id
                .as_deref()
                .context("preferred comparison lacks artifact")?,
            true,
        )?;
        let winner = self.edit_plan_revision(&choice.preferred_revision_id)?;
        let facts = verified
            .verification
            .as_ref()
            .context("preferred variant lacks verification")?;
        ensure!(
            status.comparison.state == "complete"
                && status.comparison.recipe == RECIPE
                && verified.latest_attempt
                && verified.file_present
                && facts.output_matches
                && facts.plan_renderable
                && winner.project_id == status.comparison.request.project_id,
            "stale evidence: preferred movie or current plan is invalid"
        );
        let current = {
            let _heavy = self.acquire_local_heavy()?;
            super::preview_media::prepare(
                self,
                &winner,
                &verified.artifact.request,
                &self.list_assets(&winner.project_id)?,
                &|| false,
                verified.artifact.excerpt.as_ref(),
            )?
            .identity
        };
        let stored = verified
            .artifact
            .identity
            .as_ref()
            .context("preferred variant lacks input identity")?;
        let mut compatible = current.clone();
        compatible.constraints_key = stored.constraints_key.clone();
        // Accepting explicit retention changes the constraint key. Revalidate
        // all current human rules, while requiring the exact viewed bytes,
        // source/recipe/intent identity and latest render. This does not relabel
        // the older receipt as current under its former constraints.
        ensure!(
            &compatible == stored,
            "stale evidence: preferred media or intent changed"
        );
        self.check_revision_inputs(
            &winner.project_id,
            &winner.operations,
            Some(&winner.revision_id),
        )?;
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let body: String = tx.query_row(
            "SELECT body_json FROM preview_comparison_choices WHERE id=?1",
            [id],
            |r| r.get(0),
        )?;
        choice = serde_json::from_str(&body)?;
        if choice.draft_revision_id.is_none() {
            super::require_active(&tx, &winner.project_id, expected(expected_active))?;
            ensure!(
                super::ranges::constraints_key(&tx, &winner)? == current.constraints_key,
                "human constraints changed before draft creation"
            );
            ensure!(
                super::feedback::retained_choice_violations(&tx, &winner)?.is_empty(),
                "comparison would violate accepted human choices"
            );
            let (revision_id, operation) = super::revision_operation(
                &tx,
                &winner.project_id,
                &winner.operations,
                winner.script_revision_id.as_deref(),
                Some(&winner.revision_id),
            )?;
            commit_local_in_transaction(&tx, &winner.project_id, vec![operation])?;
            choice.draft_revision_id = Some(revision_id);
            choice.draft_failure = None;
            tx.execute(
                "UPDATE preview_comparison_choices SET body_json=?2,updated_at=?3 WHERE id=?1",
                params![
                    choice.choice_id,
                    serde_json::to_string(&choice)?,
                    crate::now()
                ],
            )?;
        }
        tx.commit()?;
        Ok(choice)
    }
}
