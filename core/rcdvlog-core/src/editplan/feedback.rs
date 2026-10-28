//! Revision-bound review facts. Only explicit human decisions accept changes;
//! proposals never mutate plans or become source-range constraints by themselves.
pub use super::feedback_model::*;
use super::{EditPlanOperation, EditPlanRevision, read_revision};
use crate::operations::{
    NewOperation, advance_revision, commit_local_in_transaction, current_revision,
    revision_guard_passes,
};
use crate::{ApplyOperationResult, Catalog, ProjectOperation};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

pub(crate) const NOTE: &str = "editReviewNoteRecorded";
pub(crate) const PROPOSAL: &str = "editReviewChangeProposed";
pub(crate) const DECISION: &str = "editReviewDecisionRecorded";
pub(crate) const MAPPING: &str = "editReviewTargetMapped";
const MAX_FACTS: usize = 256;
const MAX_NOTES: usize = 20_000;
const MAX_HARD_CHOICES: usize = 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FactPayload {
    entity_id: String,
    note_id: String,
    fact: ReviewFeedbackFact,
}

fn id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}
fn bounded_id(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 255
            && !value.chars().any(|c| c.is_control() || c.is_whitespace()),
        "invalid input: invalid review identity"
    );
    Ok(())
}
fn text(value: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty() && value.len() <= 4096,
        "invalid input: review text/rationale must contain 1-4096 bytes"
    );
    Ok(())
}
fn snapshot(cut: &EditPlanOperation) -> ReviewCutSnapshot {
    ReviewCutSnapshot {
        operation_id: cut.operation_id.clone(),
        position: cut.position,
        asset_id: cut.asset_id.clone(),
        source_in_seconds: cut.source_in_seconds,
        source_out_seconds: cut.source_out_seconds,
        audio: cut.audio.clone(),
    }
}
fn audio_context(plan: &EditPlanRevision, at: usize) -> Result<(String, String, u32)> {
    let mut starts = Vec::with_capacity(plan.operations.len());
    let mut cursor = 0.0_f64;
    let mut valid = true;
    for cut in &plan.operations {
        starts.push(cursor);
        let duration = cut.source_out_seconds - cut.source_in_seconds;
        valid &= duration.is_finite() && duration > 0.0;
        cursor += duration.max(0.0);
        valid &= cursor.is_finite() && cursor <= 21600.0;
    }
    let last = (at + 1).min(plan.operations.len() - 1);
    let window_start = (starts[at.saturating_sub(1)] - 0.1).max(0.0);
    let window_end = starts[last] + plan.operations[last].source_out_seconds
        - plan.operations[last].source_in_seconds
        + 0.1;
    let samples = |seconds: f64| (seconds * 48000.0).round() as i64;
    let mut events = Vec::new();
    for (index, cut) in plan.operations.iter().enumerate() {
        let (source, source_in, source_out, placement, sync, gain, fade_in, fade_out, mode) =
            match &cut.audio {
                Some(super::EditPlanAudio::Silent) => continue,
                Some(super::EditPlanAudio::Source(audio)) => {
                    valid &= audio.version == 1;
                    (
                        audio.asset_id.as_str(),
                        audio.source_in_seconds,
                        audio.source_out_seconds,
                        audio.timeline_start_seconds,
                        audio.sync_offset_seconds,
                        audio.gain_db,
                        audio.fade_in_seconds,
                        audio.fade_out_seconds,
                        "source",
                    )
                }
                None => (
                    cut.asset_id.as_str(),
                    cut.source_in_seconds,
                    cut.source_out_seconds,
                    starts[index],
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    "linked",
                ),
            };
        let end = placement + source_out - source_in;
        valid &= [
            source_in, source_out, placement, sync, gain, fade_in, fade_out, end,
        ]
        .iter()
        .all(|v| v.is_finite())
            && source_in >= 0.0
            && source_out > source_in
            && placement >= 0.0
            && end <= cursor + 1e-9;
        if placement < window_end && end > window_start {
            let relative = samples(placement - window_start);
            events.push((relative, cut.operation_id.clone(), serde_json::json!({"operationId":cut.operation_id,"mode":mode,"assetId":source,
                "sourceInSamples":samples(source_in),"sourceOutSamples":samples(source_out),"relativeStartSamples":relative,
                "syncOffsetSamples":samples(sync),"gainDb":gain,"fadeInSamples":samples(fade_in),"fadeOutSamples":samples(fade_out)})));
        }
    }
    if !valid {
        // An invalid draft can still receive a note. Its unavailable timing is
        // labelled and compared conservatively, never converted into evidence.
        let media = plan.operations.iter().map(snapshot).collect::<Vec<_>>();
        return Ok((
            blake3::hash(&serde_json::to_vec(&media)?)
                .to_hex()
                .to_string(),
            "unavailableInvalidTiming".into(),
            0,
        ));
    }
    events.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    let count = events.len() as u32;
    let values = events
        .into_iter()
        .map(|(_, _, value)| value)
        .collect::<Vec<_>>();
    Ok((
        blake3::hash(&serde_json::to_vec(&(
            "declared-audio-context.48k-neighbors-100ms.v1",
            values,
        ))?)
        .to_hex()
        .to_string(),
        "declaredTimeline48k".into(),
        count,
    ))
}

fn target(plan: &EditPlanRevision, operation: &str) -> Result<ReviewTarget> {
    let at = plan
        .operations
        .iter()
        .position(|cut| cut.operation_id == operation)
        .ok_or_else(|| ReviewFeedbackError::NotFound {
            id: operation.into(),
        })?;
    let cut = &plan.operations[at];
    let (context_audio_digest, context_audio_precision, context_audio_event_count) =
        audio_context(plan, at)?;
    Ok(ReviewTarget {
        revision_id: plan.revision_id.clone(),
        cut: snapshot(cut),
        previous_cut: at.checked_sub(1).map(|i| snapshot(&plan.operations[i])),
        next_cut: plan.operations.get(at + 1).map(snapshot),
        context_audio_digest,
        context_audio_precision,
        context_audio_event_count,
        source_occurrences: plan
            .operations
            .iter()
            .filter(|c| c.asset_id == cut.asset_id)
            .count() as u32,
        viewed_movie: None,
    })
}
fn same_target(left: &ReviewTarget, right: &ReviewTarget) -> bool {
    left.cut == right.cut
        && left.previous_cut == right.previous_cut
        && left.next_cut == right.next_cut
        && left.source_occurrences == right.source_occurrences
        && left.context_audio_digest == right.context_audio_digest
        && left.context_audio_precision == right.context_audio_precision
        && left.context_audio_event_count == right.context_audio_event_count
}
fn status(target: &ReviewTarget, plan: &EditPlanRevision) -> ReviewTargetStatus {
    let mut flags = Vec::new();
    let current = plan
        .operations
        .iter()
        .find(|c| c.operation_id == target.cut.operation_id);
    if let Some(cut) = current {
        if cut.asset_id != target.cut.asset_id {
            flags.push("source_replaced".into());
        }
        if cut.source_in_seconds != target.cut.source_in_seconds
            || cut.source_out_seconds != target.cut.source_out_seconds
        {
            flags.push("source_range_changed".into());
        }
        if cut.position != target.cut.position {
            flags.push("reordered".into());
        }
        if cut.audio != target.cut.audio {
            flags.push("audio_changed".into());
        }
        if let Ok(now) = self::target(plan, &cut.operation_id) {
            if now.previous_cut != target.previous_cut || now.next_cut != target.next_cut {
                flags.push("neighbors_changed".into());
            }
            if now.source_occurrences != target.source_occurrences {
                flags.push("source_reuse_changed".into());
            }
            if now.context_audio_digest != target.context_audio_digest
                || now.context_audio_precision != target.context_audio_precision
            {
                flags.push("audio_context_changed".into());
            }
        }
    } else {
        flags.push("cut_missing_or_split".into());
    }
    let candidates = if current.is_none() {
        plan.operations
            .iter()
            .filter(|c| {
                c.asset_id == target.cut.asset_id
                    && c.source_in_seconds < target.cut.source_out_seconds
                    && target.cut.source_in_seconds < c.source_out_seconds
            })
            .map(|c| c.operation_id.clone())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let truncated = candidates.len() > 8;
    ReviewTargetStatus {
        requires_mapping: !flags.is_empty(),
        flags,
        candidate_operation_ids: candidates.into_iter().take(8).collect(),
        candidates_truncated: truncated,
    }
}
fn scoped_status(
    connection: &Connection,
    target: &ReviewTarget,
    plan: &EditPlanRevision,
) -> Result<ReviewTargetStatus> {
    let mut result = status(target, plan);
    if super::selection::metadata_fingerprint(connection, &target.revision_id)?
        != super::selection::metadata_fingerprint(connection, &plan.revision_id)?
    {
        result.flags.push("editorial_intent_changed".into());
        result.requires_mapping = true;
    }
    Ok(result)
}
fn fact_parts(fact: &ReviewFeedbackFact) -> (&'static str, &str, &str) {
    match fact {
        ReviewFeedbackFact::Note(n) => ("note", &n.note_id, &n.note_id),
        ReviewFeedbackFact::Proposal(p) => ("proposal", &p.proposal_id, &p.note_id),
        ReviewFeedbackFact::Decision(d) => ("decision", &d.decision_id, &d.note_id),
        ReviewFeedbackFact::Mapping(m) => ("mapping", &m.mapping_id, &m.note_id),
    }
}
fn load_fact(connection: &Connection, entity: &str) -> Result<ReviewFeedbackFact> {
    let body: Option<String> = connection
        .query_row(
            "SELECT body_json FROM edit_feedback_facts WHERE entity_id=?1",
            [entity],
            |r| r.get(0),
        )
        .optional()?;
    serde_json::from_str(&body.ok_or_else(|| ReviewFeedbackError::NotFound { id: entity.into() })?)
        .context("decode edit review fact")
}
fn load_note(connection: &Connection, note: &str) -> Result<ReviewNote> {
    let ReviewFeedbackFact::Note(note) = load_fact(connection, note)? else {
        return Err(ReviewFeedbackError::NotFound { id: note.into() }.into());
    };
    Ok(note)
}
fn latest(connection: &Connection, note: &str, kind: &str) -> Result<Option<ReviewFeedbackFact>> {
    let body:Option<String>=connection.query_row("SELECT body_json FROM edit_feedback_facts WHERE note_id=?1 AND kind=?2 ORDER BY note_revision DESC LIMIT 1",params![note,kind],|r|r.get(0)).optional()?;
    body.map(|s| serde_json::from_str(&s).context("decode latest review fact"))
        .transpose()
}
fn latest_decision(connection: &Connection, note: &str) -> Result<Option<ReviewDecision>> {
    Ok(match latest(connection, note, "decision")? {
        Some(ReviewFeedbackFact::Decision(d)) => Some(d),
        None => None,
        _ => anyhow::bail!("invalid stored review decision"),
    })
}
fn proposal(connection: &Connection, note: &str, id: &str) -> Result<ReviewProposal> {
    match load_fact(connection, id)? {
        ReviewFeedbackFact::Proposal(p) if p.note_id == note => Ok(p),
        _ => Err(ReviewFeedbackError::NotFound { id: id.into() }.into()),
    }
}
fn effective_target(
    connection: &Connection,
    note: &ReviewNote,
    proposal: Option<&ReviewProposal>,
) -> Result<ReviewTarget> {
    let mut query=connection.prepare("SELECT body_json FROM edit_feedback_facts WHERE note_id=?1 AND kind='mapping' ORDER BY note_revision DESC LIMIT 256")?;
    let rows = query
        .query_map([&note.note_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for body in rows {
        let ReviewFeedbackFact::Mapping(mapping) = serde_json::from_str(&body)? else {
            anyhow::bail!("invalid stored review mapping")
        };
        if mapping.proposal_id.as_deref() == proposal.map(|p| p.proposal_id.as_str()) {
            return Ok(mapping.target);
        }
    }
    Ok(proposal.map_or_else(|| note.target.clone(), |p| p.target.clone()))
}
fn require_revision(connection: &Connection, note: &ReviewNote, expected: u64) -> Result<()> {
    let actual = current_revision(connection, &note.project_id, "editReview", &note.note_id)?;
    if actual != expected {
        return Err(ReviewFeedbackError::StaleRevision { expected, actual }.into());
    }
    ensure!(
        actual < MAX_FACTS as u64,
        "invalid input: review note history reached 256 facts; supersede it with a new note"
    );
    Ok(())
}
fn operation(fact: ReviewFeedbackFact, expected: u64) -> Result<NewOperation> {
    let (kind, entity, note) = fact_parts(&fact);
    let op_type = match kind {
        "note" => NOTE,
        "proposal" => PROPOSAL,
        "decision" => DECISION,
        "mapping" => MAPPING,
        _ => unreachable!(),
    };
    Ok(NewOperation {
        aggregate_type: "editReview",
        aggregate_id: note.into(),
        operation_type: op_type,
        base_revision: Some(expected),
        payload: serde_json::to_value(FactPayload {
            entity_id: entity.into(),
            note_id: note.into(),
            fact,
        })?,
    })
}
fn validate_target(connection: &Connection, project: &str, reviewed: &ReviewTarget) -> Result<()> {
    let plan = read_revision(connection, &reviewed.revision_id)?;
    ensure!(
        plan.project_id == project
            && same_target(reviewed, &target(&plan, &reviewed.cut.operation_id)?),
        "invalid input: review target does not match its immutable revision"
    );
    if let Some(movie) = &reviewed.viewed_movie {
        bounded_id(&movie.artifact_id)?;
        ensure!(
            movie.output_fingerprint.len() == 64
                && movie
                    .output_fingerprint
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit())
                && movie.movie_time_base_numerator > 0
                && movie.movie_time_base_denominator > 0
                && movie.movie_time_base_numerator <= 1_000_000_000
                && movie.movie_time_base_denominator <= 1_000_000_000
                && movie.picture_mapping.movie_out_pts > movie.picture_mapping.movie_in_pts
                && movie.picture_mapping.output_frame_count > 0
                && movie.movie_duration_seconds.is_finite()
                && movie.movie_duration_seconds > 0.0
                && movie.picture_mapping.operation_id == reviewed.cut.operation_id
                && movie.picture_mapping.asset_id == reviewed.cut.asset_id
                && movie.picture_mapping.source_in_seconds == reviewed.cut.source_in_seconds
                && movie.picture_mapping.source_out_seconds == reviewed.cut.source_out_seconds,
            "invalid input: movie evidence does not identify this reviewed cut"
        );
        let timestamp_mapping = if movie.requested_movie_seconds.is_some() {
            "declaredPlaybackSeconds"
        } else {
            "none"
        };
        ensure!(
            movie
                .requested_movie_seconds
                .is_none_or(|v| v.is_finite() && v >= 0.0 && v <= movie.movie_duration_seconds)
                && movie.timestamp_mapping == timestamp_mapping
                && movie.source_fingerprints.len() <= 6
                && movie
                    .source_fingerprints
                    .iter()
                    .all(|source| source.asset_id.len() <= 255
                        && !source.asset_id.is_empty()
                        && !source.source_fingerprint.is_empty()
                        && source.source_fingerprint.len() <= 255
                        && source.representation_fingerprint.len() == 64
                        && source
                            .representation_fingerprint
                            .bytes()
                            .all(|c| c.is_ascii_hexdigit())
                        && [0, 90, 180, 270].contains(&source.rotation_degrees))
                && movie.limitations.len() <= 16
                && movie.limitations.iter().all(|s| s.len() <= 4096),
            "invalid input: invalid bounded movie evidence"
        );
    }
    Ok(())
}
fn validate_change(change: &ReviewChange) -> Result<()> {
    if let ReviewChange::Trim {
        source_in_seconds: a,
        source_out_seconds: b,
    }
    | ReviewChange::Replace {
        source_in_seconds: a,
        source_out_seconds: b,
        ..
    } = change
    {
        ensure!(
            a.is_finite() && b.is_finite() && *a >= 0.0 && b > a,
            "invalid input: proposed trim must be finite, ordered and positive"
        );
    }
    let audio = match change {
        ReviewChange::Replace { audio, .. } | ReviewChange::Audio { audio } => audio.as_ref(),
        _ => None,
    };
    if let Some(super::EditPlanAudio::Source(source)) = audio {
        ensure!(
            [
                source.source_in_seconds,
                source.source_out_seconds,
                source.timeline_start_seconds,
                source.sync_offset_seconds,
                source.gain_db,
                source.fade_in_seconds,
                source.fade_out_seconds
            ]
            .iter()
            .all(|v| v.is_finite()),
            "invalid input: proposal audio values must be finite"
        );
    }
    Ok(())
}
fn apply_change(
    operations: &mut Vec<EditPlanOperation>,
    target: &ReviewTarget,
    change: &ReviewChange,
    rationale: &str,
) -> Result<()> {
    let at = operations
        .iter()
        .position(|c| c.operation_id == target.cut.operation_id)
        .ok_or(ReviewFeedbackError::ChangedTarget)?;
    match change {
        ReviewChange::Trim {
            source_in_seconds,
            source_out_seconds,
        } => {
            operations[at].source_in_seconds = *source_in_seconds;
            operations[at].source_out_seconds = *source_out_seconds;
        }
        ReviewChange::Replace {
            asset_id,
            source_in_seconds,
            source_out_seconds,
            audio,
        } => {
            let cut = &mut operations[at];
            cut.asset_id = asset_id.clone();
            cut.source_in_seconds = *source_in_seconds;
            cut.source_out_seconds = *source_out_seconds;
            cut.audio = audio.clone();
        }
        ReviewChange::Audio { audio } => operations[at].audio = audio.clone(),
        ReviewChange::Omit => {
            operations.remove(at);
        }
        ReviewChange::Reorder {
            before_operation_id,
        } => {
            ensure!(
                before_operation_id.as_deref() != Some(&target.cut.operation_id),
                "invalid input: a cut cannot be moved before itself"
            );
            let cut = operations.remove(at);
            let before = before_operation_id
                .as_ref()
                .map(|id| {
                    operations
                        .iter()
                        .position(|c| &c.operation_id == id)
                        .ok_or(ReviewFeedbackError::ChangedTarget)
                })
                .transpose()?
                .unwrap_or(operations.len());
            operations.insert(before, cut);
        }
    }
    for (index, cut) in operations.iter_mut().enumerate() {
        cut.position = index as u32;
        if cut.operation_id == target.cut.operation_id {
            cut.rationale = rationale.into();
        }
    }
    Ok(())
}
fn change_satisfied(
    plan: &EditPlanRevision,
    target: &ReviewTarget,
    change: &ReviewChange,
) -> Result<bool> {
    let actual = plan
        .operations
        .iter()
        .find(|c| c.operation_id == target.cut.operation_id);
    if matches!(change, ReviewChange::Omit) {
        return Ok(actual.is_none());
    }
    let Some(actual) = actual else {
        return Ok(false);
    };
    if let ReviewChange::Reorder {
        before_operation_id,
    } = change
    {
        return Ok(if let Some(before) = before_operation_id {
            plan.operations
                .iter()
                .find(|c| &c.operation_id == before)
                .is_some_and(|c| actual.position < c.position)
        } else {
            actual.position as usize + 1 == plan.operations.len()
        });
    }
    let original = EditPlanOperation {
        operation_id: target.cut.operation_id.clone(),
        position: 0,
        asset_id: target.cut.asset_id.clone(),
        source_in_seconds: target.cut.source_in_seconds,
        source_out_seconds: target.cut.source_out_seconds,
        rationale: String::new(),
        audio: target.cut.audio.clone(),
    };
    let mut expected = vec![original];
    apply_change(&mut expected, target, change, "")?;
    let expected = &expected[0];
    Ok(actual.asset_id == expected.asset_id
        && actual.source_in_seconds == expected.source_in_seconds
        && actual.source_out_seconds == expected.source_out_seconds
        && actual.audio == expected.audio)
}

impl Catalog {
    pub fn review_note(&self, note_id: &str) -> Result<ReviewNote> {
        let connection = self.connection()?;
        if !crate::table_exists(&connection, "edit_feedback_facts")? {
            return Err(ReviewFeedbackError::NotFound { id: note_id.into() }.into());
        }
        load_note(&connection, note_id)
    }

    pub fn record_review_note(
        &self,
        request: &RecordReviewNoteRequest,
    ) -> Result<ReviewFeedbackView> {
        text(&request.observation)?;
        ensure!(
            request.category != ReviewCategory::Intent || request.author == ReviewAuthor::Human,
            "invalid input: free-text editorial intent must be human authored"
        );
        ensure!(
            request.evidence_refs.len() <= 16,
            "invalid input: at most 16 review evidence references"
        );
        for value in &request.evidence_refs {
            bounded_id(value)?;
        }
        let plan = self.edit_plan_revision(&request.revision_id)?;
        ensure!(
            plan.project_id == request.project_id,
            "invalid input: reviewed revision belongs to another project"
        );
        let mut reviewed = target(&plan, &request.operation_id)?;
        if let Some(artifact_id) = &request.viewed_artifact_id {
            let receipt = self.preview_artifact_status(artifact_id, false)?.artifact;
            ensure!(
                receipt.state == super::PreviewArtifactState::Complete
                    && receipt.revision_id == plan.revision_id
                    && receipt.project_id == request.project_id,
                "invalid input: viewed artifact is not a complete preview of this revision"
            );
            let qa = receipt
                .validation
                .context("viewed preview has no media validation")?;
            let identity = receipt
                .identity
                .context("viewed preview has no input identity")?;
            let mapping = qa
                .cuts
                .iter()
                .find(|c| c.operation_id == request.operation_id)
                .context("reviewed cut is absent from viewed movie")?
                .clone();
            ensure!(
                request
                    .movie_time_seconds
                    .is_none_or(|v| v.is_finite() && v >= 0.0 && v <= qa.duration_seconds),
                "invalid input: note movie timestamp is outside the viewed output"
            );
            let mut source_ids = Vec::new();
            for cut in std::iter::once(&reviewed.cut)
                .chain(reviewed.previous_cut.as_ref())
                .chain(reviewed.next_cut.as_ref())
            {
                source_ids.push(cut.asset_id.as_str());
                if let Some(super::EditPlanAudio::Source(audio)) = &cut.audio {
                    source_ids.push(audio.asset_id.as_str());
                }
            }
            reviewed.viewed_movie=Some(ReviewMovieEvidence {artifact_id:artifact_id.clone(),output_fingerprint:qa.output_fingerprint,
                movie_time_base_numerator:qa.movie_time_base_numerator,movie_time_base_denominator:qa.movie_time_base_denominator,
                movie_duration_seconds:qa.duration_seconds,requested_movie_seconds:request.movie_time_seconds,
                timestamp_mapping:if request.movie_time_seconds.is_some(){"declaredPlaybackSeconds"}else{"none"}.into(),picture_mapping:mapping,
                source_fingerprints:identity.sources.into_iter().filter(|s|source_ids.contains(&s.asset_id.as_str())).map(|s|ReviewSourceReference{
                    asset_id:s.asset_id,source_fingerprint:s.source_fingerprint,representation_fingerprint:s.representation_fingerprint,rotation_degrees:s.rotation_degrees}).collect(),
                limitations:vec!["The output fingerprint identifies the recorded completed movie. Recording a note does not freshly verify source/output bytes or assert human listening.".into(),
                    "Entered playback seconds are a declared locator, not a verified frame/source boundary; edits require explicit inspected source ranges.".into()]});
        } else {
            ensure!(
                request.movie_time_seconds.is_none(),
                "invalid input: movie timestamps require the viewed artifact ID"
            );
        }
        let note = ReviewNote {
            note_id: id("reviewnote"),
            project_id: request.project_id.clone(),
            author: request.author,
            category: request.category,
            observation: request.observation.clone(),
            target: reviewed,
            evidence_refs: request.evidence_refs.clone(),
        };
        let note_id = note.note_id.clone();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        commit_local_in_transaction(
            &transaction,
            &request.project_id,
            vec![operation(ReviewFeedbackFact::Note(note), 0)?],
        )?;
        transaction.commit()?;
        self.review_feedback(&note_id, &request.revision_id)
    }
    pub fn propose_review_change(
        &self,
        request: &ProposeReviewChangeRequest,
    ) -> Result<ReviewFeedbackView> {
        text(&request.rationale)?;
        validate_change(&request.change)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let note = load_note(&transaction, &request.note_id)?;
        require_revision(&transaction, &note, request.expected_revision)?;
        let proposal = ReviewProposal {
            proposal_id: id("reviewprop"),
            note_id: note.note_id.clone(),
            author: request.author,
            target: effective_target(&transaction, &note, None)?,
            change: request.change.clone(),
            rationale: request.rationale.clone(),
        };
        let revision = proposal.target.revision_id.clone();
        commit_local_in_transaction(
            &transaction,
            &note.project_id,
            vec![operation(
                ReviewFeedbackFact::Proposal(proposal),
                request.expected_revision,
            )?],
        )?;
        transaction.commit()?;
        self.review_feedback(&note.note_id, &revision)
    }
    pub fn decide_review_feedback(
        &self,
        request: &DecideReviewFeedbackRequest,
    ) -> Result<ReviewFeedbackView> {
        text(&request.rationale)?;
        // This is an explicit caller assertion of a human decision. Bounded
        // reviewer tools have no access to this mutation.
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let note = load_note(&transaction, &request.note_id)?;
        require_revision(&transaction, &note, request.expected_revision)?;
        let state = match request.action {
            ReviewDecisionAction::Accepted => ReviewDecisionState::Accepted,
            ReviewDecisionAction::Deferred => ReviewDecisionState::Deferred,
            ReviewDecisionAction::Superseded => ReviewDecisionState::Superseded,
            ReviewDecisionAction::Addressed => ReviewDecisionState::Addressed,
        };
        let target = effective_target(&transaction, &note, None)?;
        let decision = ReviewDecision {
            decision_origin: ReviewDecisionOrigin::Human,
            decision_id: id("reviewdecision"),
            note_id: note.note_id.clone(),
            proposal_id: request.proposal_id.clone(),
            state,
            rationale: request.rationale.clone(),
            hard_choice: request.hard_choice,
            retained_target: request.hard_choice.map(|_| target.clone()),
            applied_revision_id: None,
            accepted_decision_id: None,
        };
        commit_local_in_transaction(
            &transaction,
            &note.project_id,
            vec![operation(
                ReviewFeedbackFact::Decision(decision),
                request.expected_revision,
            )?],
        )?;
        transaction.commit()?;
        self.review_feedback(&note.note_id, &target.revision_id)
    }
    pub fn map_review_target(
        &self,
        request: &MapReviewTargetRequest,
    ) -> Result<ReviewFeedbackView> {
        text(&request.rationale)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let note = load_note(&transaction, &request.note_id)?;
        require_revision(&transaction, &note, request.expected_revision)?;
        let plan = read_revision(&transaction, &request.revision_id)?;
        ensure!(
            plan.project_id == note.project_id,
            "invalid input: mapped revision belongs to another project"
        );
        let mapping = ReviewTargetMapping {
            decision_owner: request.decision_owner,
            mapping_id: id("reviewmap"),
            note_id: note.note_id.clone(),
            proposal_id: request.proposal_id.clone(),
            target: target(&plan, &request.operation_id)?,
            rationale: request.rationale.clone(),
        };
        commit_local_in_transaction(
            &transaction,
            &note.project_id,
            vec![operation(
                ReviewFeedbackFact::Mapping(mapping),
                request.expected_revision,
            )?],
        )?;
        transaction.commit()?;
        self.review_feedback(&note.note_id, &request.revision_id)
    }
    pub fn review_feedback(&self, note_id: &str, revision_id: &str) -> Result<ReviewFeedbackView> {
        let connection = self.connection()?;
        if !crate::table_exists(&connection, "edit_feedback_facts")? {
            return Err(ReviewFeedbackError::NotFound { id: note_id.into() }.into());
        }
        let transaction = connection.unchecked_transaction()?;
        let plan = read_revision(&transaction, revision_id)?;
        let view = view(&transaction, note_id, &plan)?;
        transaction.commit()?;
        Ok(view)
    }
    pub fn review_feedback_report(
        &self,
        project: &str,
        revision_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<ReviewFeedbackPage> {
        ensure!(
            (1..=100).contains(&limit),
            "invalid input: feedback page limit must be 1-100"
        );
        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction()?;
        let plan = read_revision(&transaction, revision_id)?;
        ensure!(
            plan.project_id == project,
            "invalid input: report revision belongs to another project"
        );
        if !crate::table_exists(&transaction, "edit_feedback_facts")? {
            return Ok(ReviewFeedbackPage {
                schema_ready: false,
                project_id: project.into(),
                revision_id: revision_id.into(),
                total: 0,
                next_offset: None,
                items: vec![],
            });
        }
        let total = transaction.query_row(
            "SELECT COUNT(*) FROM edit_feedback_facts WHERE project_id=?1 AND kind='note'",
            [project],
            |r| r.get::<_, i64>(0),
        )? as usize;
        let mut query=transaction.prepare("SELECT note_id FROM edit_feedback_facts WHERE project_id=?1 AND kind='note' ORDER BY recorded_at_millis,note_id LIMIT ?2 OFFSET ?3")?;
        let ids = query
            .query_map(
                params![project, i64::try_from(limit)?, i64::try_from(offset)?],
                |r| r.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(query);
        let items = ids
            .into_iter()
            .map(|id| view(&transaction, &id, &plan))
            .collect::<Result<Vec<_>>>()?;
        transaction.commit()?;
        Ok(ReviewFeedbackPage {
            schema_ready: true,
            project_id: project.into(),
            revision_id: revision_id.into(),
            total,
            next_offset: (offset.saturating_add(limit) < total)
                .then_some(offset.saturating_add(limit)),
            items,
        })
    }
    pub fn apply_review_feedback(
        &self,
        request: &ApplyReviewFeedbackRequest,
    ) -> Result<ApplyReviewFeedbackResult> {
        ensure!(
            !request.accepted.is_empty() && request.accepted.len() <= 64,
            "invalid input: apply 1-64 accepted review proposals"
        );
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        super::require_active(
            &transaction,
            &request.project_id,
            request.expected_active_revision_id.as_deref(),
        )?;
        let base = read_revision(&transaction, &request.base_revision_id)?;
        ensure!(
            base.project_id == request.project_id,
            "invalid input: base revision belongs to another project"
        );
        let mut operations = base.operations.clone();
        let mut changes = Vec::new();
        let mut notes = std::collections::BTreeSet::new();
        let mut cuts = std::collections::BTreeSet::new();
        for accepted in &request.accepted {
            ensure!(
                notes.insert(accepted.note_id.clone()),
                "invalid input: duplicate review note"
            );
            let note = load_note(&transaction, &accepted.note_id)?;
            ensure!(
                note.project_id == request.project_id,
                "invalid input: review note belongs to another project"
            );
            require_revision(&transaction, &note, accepted.expected_revision)?;
            let proposed = proposal(&transaction, &note.note_id, &accepted.proposal_id)?;
            let decision = latest_decision(&transaction, &note.note_id)?
                .ok_or(ReviewFeedbackError::NotAccepted)?;
            ensure!(
                decision.state == ReviewDecisionState::Accepted
                    && decision.proposal_id.as_deref() == Some(&accepted.proposal_id)
                    && decision.decision_origin == ReviewDecisionOrigin::Human,
                ReviewFeedbackError::NotAccepted
            );
            let target = effective_target(&transaction, &note, Some(&proposed))?;
            ensure!(
                !scoped_status(&transaction, &target, &base)?.requires_mapping,
                ReviewFeedbackError::ChangedTarget
            );
            ensure!(
                cuts.insert(target.cut.operation_id.clone()),
                ReviewFeedbackError::ConflictingChanges
            );
            apply_change(
                &mut operations,
                &target,
                &proposed.change,
                &proposed.rationale,
            )?;
            changes.push((accepted, proposed, target, decision));
        }
        self.check_revision_inputs(&request.project_id, &operations, Some(&base.revision_id))?;
        let (revision_id, creation) = super::revision_operation(
            &transaction,
            &request.project_id,
            &operations,
            base.script_revision_id.as_deref(),
            Some(&base.revision_id),
        )?;
        let draft = EditPlanRevision {
            revision_id: revision_id.clone(),
            project_id: base.project_id.clone(),
            script_revision_id: base.script_revision_id.clone(),
            parent_revision_id: Some(base.revision_id.clone()),
            active: false,
            operations,
            created_at: String::new(),
        };
        let mut mutations = vec![creation];
        for (accepted, proposed, target, decision) in changes {
            ensure!(
                change_satisfied(&draft, &target, &proposed.change)?,
                ReviewFeedbackError::ConflictingChanges
            );
            let applied = ReviewDecision {
                decision_origin: ReviewDecisionOrigin::Host,
                decision_id: id("reviewdecision"),
                note_id: accepted.note_id.clone(),
                proposal_id: Some(proposed.proposal_id),
                state: ReviewDecisionState::Applied,
                rationale: proposed.rationale,
                hard_choice: None,
                retained_target: None,
                applied_revision_id: Some(revision_id.clone()),
                accepted_decision_id: Some(decision.decision_id),
            };
            mutations.push(operation(
                ReviewFeedbackFact::Decision(applied),
                accepted.expected_revision,
            )?);
        }
        commit_local_in_transaction(&transaction, &request.project_id, mutations)?;
        transaction.commit()?;
        Ok(ApplyReviewFeedbackResult {
            revision: self.edit_plan_revision(&revision_id)?,
            diff: self.diff_edit_plans(&base.revision_id, &revision_id)?,
            validation: self.validate_edit_plan(&revision_id)?,
            addressed_note_ids: notes.into_iter().collect(),
        })
    }
}

fn view(
    connection: &Connection,
    note_id: &str,
    plan: &EditPlanRevision,
) -> Result<ReviewFeedbackView> {
    let note = load_note(connection, note_id)?;
    ensure!(
        note.project_id == plan.project_id,
        "invalid input: note and report revision belong to different projects"
    );
    let revision = current_revision(connection, &note.project_id, "editReview", note_id)?;
    let effective_target = effective_target(connection, &note, None)?;
    let latest_proposal = match latest(connection, note_id, "proposal")? {
        Some(ReviewFeedbackFact::Proposal(p)) => Some(p),
        None => None,
        _ => anyhow::bail!("invalid review proposal"),
    };
    let latest_decision = latest_decision(connection, note_id)?;
    let decided_proposal = latest_decision
        .as_ref()
        .and_then(|d| d.proposal_id.as_deref())
        .map(|id| proposal(connection, note_id, id))
        .transpose()?;
    let proposal_target_status = decided_proposal
        .as_ref()
        .or(latest_proposal.as_ref())
        .map(|p| {
            self::effective_target(connection, &note, Some(p))
                .and_then(|t| scoped_status(connection, &t, plan))
        })
        .transpose()?;
    let target_status = scoped_status(connection, &effective_target, plan)?;
    let accepted_target_changed = if latest_decision
        .as_ref()
        .is_some_and(|d| d.proposal_id.is_some())
    {
        proposal_target_status
            .as_ref()
            .is_some_and(|s| s.requires_mapping)
    } else {
        target_status.requires_mapping
    };
    let resolution = match latest_decision.as_ref().map(|d| d.state) {
        Some(ReviewDecisionState::Applied | ReviewDecisionState::Addressed) => "addressed",
        Some(ReviewDecisionState::Deferred) => "deferred",
        Some(ReviewDecisionState::Superseded) => "superseded",
        Some(ReviewDecisionState::Accepted) if accepted_target_changed => "unresolved",
        Some(ReviewDecisionState::Accepted) => "accepted",
        None => "unresolved",
    }
    .into();
    let history_total = connection.query_row(
        "SELECT COUNT(*) FROM edit_feedback_facts WHERE note_id=?1",
        [note_id],
        |r| r.get::<_, i64>(0),
    )? as usize;
    let mut query=connection.prepare("SELECT operation_id,note_revision,recorded_at_millis,body_json FROM edit_feedback_facts WHERE note_id=?1 ORDER BY note_revision DESC LIMIT 20")?;
    let rows = query
        .query_map([note_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let history = rows
        .into_iter()
        .map(|(operation_id, note_revision, recorded_at_millis, body)| {
            Ok(ReviewFeedbackHistoryItem {
                operation_id,
                note_revision: u64::try_from(note_revision)?,
                recorded_at_millis,
                fact: serde_json::from_str(&body)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ReviewFeedbackView {
        current_revision_id: plan.revision_id.clone(),
        target_status,
        note,
        revision,
        effective_target,
        latest_proposal,
        latest_decision,
        decided_proposal,
        proposal_target_status,
        resolution,
        history_truncated: history_total > history.len(),
        history,
        history_total,
    })
}

pub(crate) fn materialize(
    transaction: &Transaction<'_>,
    op: &ProjectOperation,
) -> Result<ApplyOperationResult> {
    ensure!(
        op.aggregate_type == "editReview" && op.payload_version == 1 && op.base_revision.is_some(),
        "edit review facts require a version-1 aggregate and explicit base revision"
    );
    let p: FactPayload = serde_json::from_value(op.payload.clone())?;
    let (kind, entity, note_id) = fact_parts(&p.fact);
    bounded_id(entity)?;
    bounded_id(note_id)?;
    ensure!(
        p.entity_id == entity && p.note_id == note_id && op.aggregate_id == note_id,
        "review fact identity does not match its aggregate"
    );
    let expected_type = match kind {
        "note" => NOTE,
        "proposal" => PROPOSAL,
        "decision" => DECISION,
        "mapping" => MAPPING,
        _ => unreachable!(),
    };
    ensure!(
        op.operation_type == expected_type && serde_json::to_vec(&p)?.len() <= 65536,
        "invalid review fact type or payload bound"
    );
    // Parse structural fields before conflict accounting; dependent current
    // state is checked only after the aggregate guard has passed.
    match &p.fact {
        ReviewFeedbackFact::Note(n) => {
            text(&n.observation)?;
            ensure!(
                n.project_id == op.project_id
                    && n.evidence_refs.len() <= 16
                    && (n.category != ReviewCategory::Intent || n.author == ReviewAuthor::Human),
                "invalid review note owner/scope"
            );
            for id in &n.evidence_refs {
                bounded_id(id)?;
            }
            validate_target(transaction, &op.project_id, &n.target)?;
        }
        ReviewFeedbackFact::Proposal(proposed) => {
            text(&proposed.rationale)?;
            validate_change(&proposed.change)?;
            validate_target(transaction, &op.project_id, &proposed.target)?;
        }
        ReviewFeedbackFact::Decision(d) => text(&d.rationale)?,
        ReviewFeedbackFact::Mapping(m) => {
            text(&m.rationale)?;
            validate_target(transaction, &op.project_id, &m.target)?;
        }
    }
    if !revision_guard_passes(transaction, op)? {
        return Ok(ApplyOperationResult::Conflict);
    }
    ensure!(
        op.base_revision.unwrap() < MAX_FACTS as u64,
        "review fact history exceeded its bound"
    );
    // Reserve space for acceptance, application and explicit withdrawal. A
    // bounded history must never strand an accepted human retention lock.
    let limit = match &p.fact {
        ReviewFeedbackFact::Proposal(_) | ReviewFeedbackFact::Mapping(_) => MAX_FACTS - 3,
        ReviewFeedbackFact::Decision(d) if d.state == ReviewDecisionState::Accepted => {
            MAX_FACTS - 2
        }
        ReviewFeedbackFact::Decision(d) if d.state != ReviewDecisionState::Superseded => {
            MAX_FACTS - 1
        }
        _ => MAX_FACTS,
    };
    ensure!(
        op.base_revision.unwrap() < limit as u64,
        "invalid input: review history is full; explicitly supersede this note and record a new one"
    );
    if let ReviewFeedbackFact::Note(_) = &p.fact {
        ensure!(
            op.base_revision == Some(0),
            "review observation is immutable; record a new note instead of replacing it"
        );
        let count = transaction.query_row(
            "SELECT COUNT(*) FROM edit_feedback_facts WHERE project_id=?1 AND kind='note'",
            [&op.project_id],
            |r| r.get::<_, i64>(0),
        )?;
        ensure!(
            count < MAX_NOTES as i64,
            "project review notes exceeded their bound"
        );
    } else {
        let note = load_note(transaction, note_id)?;
        ensure!(
            note.project_id == op.project_id,
            "review fact belongs to another project"
        );
        match &p.fact {
            ReviewFeedbackFact::Proposal(proposed) => {
                ensure!(
                    proposed.target == effective_target(transaction, &note, None)?,
                    "proposal target requires an explicit note mapping"
                );
                let mut operations =
                    read_revision(transaction, &proposed.target.revision_id)?.operations;
                apply_change(
                    &mut operations,
                    &proposed.target,
                    &proposed.change,
                    &proposed.rationale,
                )?;
                if let ReviewChange::Replace { asset_id, .. } = &proposed.change {
                    ensure!(
                        transaction.query_row(
                            "SELECT project_id FROM assets WHERE id=?1",
                            [asset_id],
                            |r| r.get::<_, String>(0)
                        )? == op.project_id,
                        "replacement asset belongs to another project"
                    );
                }
                if let ReviewChange::Replace {
                    audio: Some(super::EditPlanAudio::Source(audio)),
                    ..
                }
                | ReviewChange::Audio {
                    audio: Some(super::EditPlanAudio::Source(audio)),
                } = &proposed.change
                {
                    ensure!(
                        transaction.query_row(
                            "SELECT project_id FROM assets WHERE id=?1",
                            [&audio.asset_id],
                            |r| r.get::<_, String>(0)
                        )? == op.project_id,
                        "proposal audio belongs to another project"
                    );
                }
            }
            ReviewFeedbackFact::Mapping(mapping) => {
                if let Some(id) = &mapping.proposal_id {
                    proposal(transaction, note_id, id)?;
                }
                ensure!(
                    latest_decision(transaction, note_id)?.is_none_or(
                        |d| d.hard_choice.is_none() || d.state != ReviewDecisionState::Accepted
                    ),
                    "explicitly supersede the retained human choice before remapping this note"
                );
            }
            ReviewFeedbackFact::Decision(decision) => {
                if let Some(prior) = latest_decision(transaction, note_id)?
                    && prior.state == ReviewDecisionState::Accepted
                    && prior.hard_choice.is_some()
                {
                    ensure!(
                        decision.state == ReviewDecisionState::Superseded
                            || (decision.state == ReviewDecisionState::Accepted
                                && decision.hard_choice.is_some()),
                        "explicitly supersede the retained human choice before deferring, addressing or replacing its decision"
                    );
                }
                let proposed = decision
                    .proposal_id
                    .as_deref()
                    .map(|id| proposal(transaction, note_id, id))
                    .transpose()?;
                if decision.state == ReviewDecisionState::Applied {
                    ensure!(
                        decision.decision_origin == ReviewDecisionOrigin::Host
                            && decision.hard_choice.is_none()
                            && decision.retained_target.is_none(),
                        "applied review receipts cannot invent a human decision or hard constraint"
                    );
                    let accepted = latest_decision(transaction, note_id)?
                        .ok_or(ReviewFeedbackError::NotAccepted)?;
                    ensure!(
                        accepted.state == ReviewDecisionState::Accepted
                            && accepted.decision_origin == ReviewDecisionOrigin::Human
                            && decision.accepted_decision_id.as_deref()
                                == Some(&accepted.decision_id)
                            && decision.proposal_id == accepted.proposal_id,
                        ReviewFeedbackError::NotAccepted
                    );
                    let proposed = proposed.context("applied review receipt has no proposal")?;
                    let applied = read_revision(
                        transaction,
                        decision
                            .applied_revision_id
                            .as_deref()
                            .context("applied review receipt has no revision")?,
                    )?;
                    ensure!(
                        applied.project_id == op.project_id,
                        "applied review revision belongs to another project"
                    );
                    let parent = read_revision(
                        transaction,
                        applied
                            .parent_revision_id
                            .as_deref()
                            .context("applied feedback revision has no base")?,
                    )?;
                    let target = effective_target(transaction, &note, Some(&proposed))?;
                    ensure!(
                        !scoped_status(transaction, &target, &parent)?.requires_mapping
                            && change_satisfied(&applied, &target, &proposed.change)?,
                        "applied review receipt does not match the accepted target/change"
                    );
                } else {
                    ensure!(
                        decision.decision_origin == ReviewDecisionOrigin::Human
                            && decision.applied_revision_id.is_none()
                            && decision.accepted_decision_id.is_none(),
                        "review acceptance/deferral requires a human decision"
                    );
                    if let Some(_policy) = decision.hard_choice {
                        ensure!(
                            decision.state == ReviewDecisionState::Accepted && proposed.is_none(),
                            "retention applies to an existing reviewed cut; accepted proposals do not silently become permanent locks"
                        );
                        let target = effective_target(transaction, &note, None)?;
                        ensure!(
                            decision.retained_target.as_ref() == Some(&target),
                            "retained choice does not identify the reviewed cut"
                        );
                        let duration = transaction.query_row(
                            "SELECT duration_seconds FROM assets WHERE id=?1 AND project_id=?2",
                            params![target.cut.asset_id, op.project_id],
                            |r| r.get::<_, f64>(0),
                        )?;
                        ensure!(
                            target.cut.source_in_seconds >= 0.0
                                && target.cut.source_out_seconds > target.cut.source_in_seconds
                                && target.cut.source_out_seconds <= duration,
                            "retained cut exceeds known source bounds"
                        );
                        let count=transaction.query_row("SELECT COUNT(*) FROM edit_feedback_note_heads WHERE project_id=?1 AND hard_choice=1 AND note_id<>?2",params![op.project_id,note_id],|r|r.get::<_,i64>(0))?;
                        ensure!(
                            count < MAX_HARD_CHOICES as i64,
                            "too many retained human cut choices"
                        );
                    } else {
                        ensure!(
                            decision.retained_target.is_none(),
                            "a soft decision cannot carry a hard retained target"
                        );
                    }
                }
            }
            ReviewFeedbackFact::Note(_) => unreachable!(),
        }
    }
    transaction.execute("INSERT INTO edit_feedback_facts(operation_id,entity_id,project_id,note_id,kind,note_revision,recorded_at_millis,body_json)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![op.operation_id,p.entity_id,op.project_id,p.note_id,kind,i64::try_from(op.base_revision.unwrap()+1)?,op.logical_time.wall_millis,serde_json::to_string(&p.fact)?])?;
    if matches!(p.fact, ReviewFeedbackFact::Note(_)) {
        transaction.execute(
            "INSERT INTO edit_feedback_note_heads(note_id,project_id) VALUES(?1,?2)",
            params![p.note_id, op.project_id],
        )?;
    }
    if let ReviewFeedbackFact::Decision(d) = &p.fact {
        let hard = d.state == ReviewDecisionState::Accepted && d.hard_choice.is_some();
        transaction.execute("UPDATE edit_feedback_note_heads SET latest_decision_id=?2,hard_choice=?3 WHERE note_id=?1",params![p.note_id,p.entity_id,hard])?;
    }
    advance_revision(transaction, op)?;
    Ok(ApplyOperationResult::Applied)
}

pub(super) fn hard_choices(connection: &Connection, project: &str) -> Result<Vec<ReviewDecision>> {
    if !crate::table_exists(connection, "edit_feedback_note_heads")? {
        return Ok(vec![]);
    }
    let mut statement=connection.prepare("SELECT f.body_json FROM edit_feedback_note_heads h JOIN edit_feedback_facts f ON f.entity_id=h.latest_decision_id
        WHERE h.project_id=?1 AND h.hard_choice=1 ORDER BY h.note_id LIMIT 1025")?;
    let rows = statement
        .query_map([project], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        rows.len() <= MAX_HARD_CHOICES,
        "retained review choices exceed their bound"
    );
    rows.into_iter()
        .map(|s| match serde_json::from_str(&s)? {
            ReviewFeedbackFact::Decision(d)
                if d.state == ReviewDecisionState::Accepted && d.hard_choice.is_some() =>
            {
                Ok(d)
            }
            _ => anyhow::bail!("invalid retained human choice projection"),
        })
        .collect()
}
pub(super) fn hard_choice_key(connection: &Connection, project: &str) -> Result<Option<String>> {
    if !crate::table_exists(connection, "edit_feedback_note_heads")? {
        return Ok(None);
    }
    let mut statement=connection.prepare("SELECT note_id,latest_decision_id FROM edit_feedback_note_heads WHERE project_id=?1 AND hard_choice=1 ORDER BY note_id")?;
    let rows = statement
        .query_map([project], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((!rows.is_empty()).then(|| {
        blake3::hash(&serde_json::to_vec(&rows).expect("string pairs serialize"))
            .to_hex()
            .to_string()
    }))
}
pub(super) fn retained_choice_violations(
    connection: &Connection,
    plan: &EditPlanRevision,
) -> Result<Vec<(String, String)>> {
    let cuts = plan
        .operations
        .iter()
        .map(|c| (c.operation_id.as_str(), c))
        .collect::<std::collections::HashMap<_, _>>();
    let mut violations = Vec::new();
    for decision in hard_choices(connection, &plan.project_id)? {
        let target = decision
            .retained_target
            .context("retained decision is missing its target")?;
        let matches = cuts
            .get(target.cut.operation_id.as_str())
            .is_some_and(|cut| {
                cut.asset_id == target.cut.asset_id
                    && cut.source_in_seconds == target.cut.source_in_seconds
                    && cut.source_out_seconds == target.cut.source_out_seconds
                    && (decision.hard_choice == Some(ReviewHardChoice::KeepPicture)
                        || cut.audio == target.cut.audio)
            });
        if !matches {
            violations.push((target.cut.asset_id,format!("Retained human cut {} from note {} changed or is absent; explicitly supersede that decision before changing the choice.",target.cut.operation_id,decision.note_id)));
        }
    }
    Ok(violations)
}

/// Upgrade-only recovery of facts an older build retained as unknown. Applied
/// and superseded rows never rewrite history or create phantom conflicts.
pub(crate) fn redispatch(connection: &mut Connection) -> Result<()> {
    let mut statement=connection.prepare("SELECT operation_id,project_id,origin_node_id,origin_sequence,hlc_wall_millis,hlc_counter,
        aggregate_type,aggregate_id,operation_type,payload_version,base_revision,payload_json FROM project_operations
        WHERE operation_type IN ('editReviewNoteRecorded','editReviewChangeProposed','editReviewDecisionRecorded','editReviewTargetMapped')
        AND payload_version=1 AND base_revision IS NOT NULL AND base_revision>=COALESCE((SELECT revision FROM materialized_revisions mr
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
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for op in candidates {
        if !crate::operations::revision_guard_passes_without_recording(&transaction, &op)? {
            continue;
        }
        transaction.execute_batch("SAVEPOINT edit_feedback_redispatch")?;
        if materialize(&transaction, &op).is_err() {
            transaction.execute_batch("ROLLBACK TO edit_feedback_redispatch")?;
        }
        transaction.execute_batch("RELEASE edit_feedback_redispatch")?;
    }
    transaction.commit()?;
    Ok(())
}

#[cfg(test)]
#[path = "feedback_tests.rs"]
mod tests;
