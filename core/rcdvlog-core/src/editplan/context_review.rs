//! Revision-bound assembled evidence; recommendations use the existing feedback owner.
use super::*;
#[cfg(test)]
#[path = "context_review_tests.rs"]
mod tests;
use crate::Catalog;
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssembledContextRequest {
    pub artifact_id: String,
    /// Explicit editorial goal supplied by the host. No inferred place/story claims.
    pub intent: String,
    #[serde(default)]
    pub source_handles: Vec<SourceInspectionRequest>,
    pub max_handle_media_seconds: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssembledReviewContext {
    pub evidence_id: String,
    pub artifact_id: String,
    pub project_id: String,
    pub revision_id: String,
    pub intent: String,
    pub selection_intent: Value,
    pub input_identity: PreviewInputIdentity,
    pub movie: PreviewResult,
    pub validation: PreviewMediaValidation,
    pub excerpt: Option<PreviewExcerptScope>,
    pub full_timeline_origin_seconds: f64,
    pub timeline_mapping_precision: String,
    pub audio_mappings: Vec<Value>,
    pub source_handles: Vec<Value>,
    pub unresolved_questions: Vec<String>,
}

impl Catalog {
    pub fn assembled_review_context(
        &self,
        request: &AssembledContextRequest,
        cancel: &dyn Fn() -> bool,
    ) -> Result<AssembledReviewContext> {
        ensure!(
            !request.intent.trim().is_empty()
                && request.intent.len() <= 4096
                && request.source_handles.len() <= 24
                && request.max_handle_media_seconds.is_finite()
                && (0.0..=120.0).contains(&request.max_handle_media_seconds),
            "invalid input: assembled review limits or intent"
        );
        let mut seconds = 0.0;
        for h in &request.source_handles {
            ensure!(
                h.source_in_seconds.is_finite()
                    && h.source_out_seconds.is_finite()
                    && h.before_seconds.is_finite()
                    && h.after_seconds.is_finite()
                    && h.before_seconds >= 0.0
                    && h.after_seconds >= 0.0
                    && h.source_out_seconds > h.source_in_seconds,
                "invalid input: source handle request"
            );
            seconds +=
                h.source_out_seconds - h.source_in_seconds + h.before_seconds + h.after_seconds;
        }
        ensure!(
            seconds <= request.max_handle_media_seconds,
            "invalid input: source handles exceed cumulative media cap"
        );
        ensure!(!cancel(), "assembled review cancelled");
        let status =
            self.preview_artifact_status_cancellable(&request.artifact_id, true, cancel)?;
        ensure!(
            status.current_for_request == Some(true),
            "stale evidence: assembled preview must be complete and verified for this exact request"
        );
        let artifact = status.artifact;
        ensure!(
            artifact.kind == "cutNeighborhood" && artifact.excerpt.is_some(),
            "invalid input: assembled context requires an explicitly scoped excerpt"
        );
        let movie = artifact.result.context("assembled preview has no output")?;
        let validation = artifact
            .validation
            .context("assembled preview lacks actual clock validation")?;
        ensure!(
            movie.duration_seconds <= 60.1 && validation.cuts.len() <= 12,
            "invalid input: review requires a preview neighborhood at most 60 seconds and 12 cuts"
        );
        let identity = artifact
            .identity
            .context("assembled preview has no identity")?;
        let plan = self.edit_plan_revision(&artifact.revision_id)?;
        let window = artifact
            .excerpt
            .as_ref()
            .map(|scope| super::preview_excerpt::resolve(&plan, scope))
            .transpose()?;
        let origin = window.as_ref().map_or(0.0, |w| w.in_seconds());
        let end = window
            .as_ref()
            .map_or_else(|| plan.duration_seconds(), |w| w.out_seconds());
        let mut mappings = Vec::new();
        let mut at = 0.0;
        for cut in &plan.operations {
            let duration = cut.source_out_seconds - cut.source_in_seconds;
            let audio = match &cut.audio {
                Some(EditPlanAudio::Silent) => None,
                Some(EditPlanAudio::Source(a)) => Some((
                    a.asset_id.as_str(),
                    a.source_in_seconds,
                    a.source_out_seconds,
                    a.timeline_start_seconds,
                    a.gain_db,
                    a.fade_in_seconds,
                    a.fade_out_seconds,
                    "explicitSource",
                )),
                None => Some((
                    cut.asset_id.as_str(),
                    cut.source_in_seconds,
                    cut.source_out_seconds,
                    at,
                    0.0,
                    0.0,
                    0.0,
                    "legacyLinkedSource",
                )),
            };
            if let Some((asset, start, out, placement, gain, fade_in, fade_out, policy)) = audio {
                let event_in = (placement * 48000.0).round() as u64;
                let source_in = (start * 48000.0).ceil() as u64;
                let source_out = (out * 48000.0).floor() as u64;
                let movie_in = (origin * 48000.0).round() as u64;
                let movie_out = (end * 48000.0).round() as u64;
                let overlap_in = event_in.max(movie_in);
                let overlap_out = (event_in + source_out.saturating_sub(source_in)).min(movie_out);
                if overlap_in < overlap_out {
                    let selected = identity
                        .sources
                        .iter()
                        .find(|s| s.asset_id == asset)
                        .context("assembled audio source is unidentified")?;
                    let present = selected
                        .clocks
                        .pointer("/selectedMedia/streams")
                        .and_then(Value::as_array)
                        .is_some_and(|streams| streams.iter().any(|s| s["codec_type"] == "audio"));
                    mappings.push(json!({"operationId":cut.operation_id,"assetId":asset,"policy":policy,"selectedAudioPresent":present,
                        "sourceInSamples48k":source_in+overlap_in-event_in,"sourceOutSamples48k":source_in+overlap_out-event_in,
                        "excerptInSamples48k":overlap_in-movie_in,"excerptOutSamples48k":overlap_out-movie_in,
                        "fullTimelineInSamples48k":overlap_in,"fullTimelineOutSamples48k":overlap_out,
                        "originalEventInSamples48k":event_in,"originalEventLengthSamples48k":source_out-source_in,
                        "gainDb":gain,"fadeInSeconds":fade_in,"fadeOutSeconds":fade_out,
                        "precision":"declaredSource48kGrid; AAC is derived; not word alignment"}));
                }
            }
            at += duration;
        }
        ensure!(
            mappings.len() <= 128,
            "invalid input: too many audio intervals for bounded assembled review"
        );
        let mut handles = Vec::new();
        for h in &request.source_handles {
            ensure!(!cancel(), "assembled review cancelled");
            // Handles must relate to the actual rendered picture or sound. They
            // remain separate source evidence, never extra edited movie frames.
            let relevant = validation.cuts.iter().any(|c| {
                c.asset_id == h.asset_id
                    && h.source_in_seconds < c.source_out_seconds
                    && h.source_out_seconds > c.source_in_seconds
            }) || mappings.iter().any(|m| {
                m["assetId"] == h.asset_id
                    && h.source_in_seconds
                        < m["sourceOutSamples48k"].as_u64().unwrap_or(0) as f64 / 48000.0
                    && h.source_out_seconds
                        > m["sourceInSamples48k"].as_u64().unwrap_or(0) as f64 / 48000.0
            });
            ensure!(
                relevant,
                "invalid input: source handle is unrelated to rendered context"
            );
            let result = match self.inspect_source_window(h, cancel) {
                Ok(r) => json!({"state":"complete","inspection":r.inspection,"reused":r.reused}),
                Err(e) => json!({"state":"unchecked","request":h,"reason":e.to_string()}),
            };
            handles.push(result);
        }
        let mut questions=vec!["Are the actions and spoken thoughts complete at the actual joins? Listen and watch; technical validation cannot decide this.".into(),
            "Do setup/action/reaction/payoff, neighboring relationships, repetition, orientation and sound continuity serve the supplied intent?".into(),
            "Is a change of place established by visible, spoken or human-supplied context? No location was inferred.".into()];
        if handles.is_empty() || handles.iter().any(|h| h["state"] != "complete") {
            questions.push("Source handles are missing or partly unchecked; request bounded local inspections before endpoint changes.".into());
        }
        let mut context=AssembledReviewContext {evidence_id:String::new(),artifact_id:artifact.artifact_id,project_id:plan.project_id.clone(),revision_id:plan.revision_id.clone(),intent:request.intent.clone(),
            selection_intent:serde_json::to_value(self.edit_plan_selection_metadata(&plan.revision_id)?)?,input_identity:identity,movie,validation,excerpt:artifact.excerpt,
            full_timeline_origin_seconds:origin,timeline_mapping_precision:"fullRevisionCumulativeFrameGrid for excerpt origin; decoded local PTS for movie; requested source spans remain locators".into(),audio_mappings:mappings,source_handles:handles,unresolved_questions:questions};
        context.evidence_id = format!(
            "assembled_{}",
            blake3::hash(&serde_json::to_vec(&context)?).to_hex()
        );
        let body = serde_json::to_string(&context)?;
        ensure!(
            body.len() <= 8 * 1024 * 1024,
            "assembled context exceeds 8 MiB"
        );
        ensure!(!cancel(), "assembled review cancelled");
        let current =
            self.preview_artifact_status_cancellable(&context.artifact_id, true, cancel)?;
        ensure!(
            current.current_for_request == Some(true)
                && current.artifact.identity.as_ref() == Some(&context.input_identity),
            "stale evidence: assembled inputs changed during inspection"
        );
        self.connection()?.execute("INSERT OR IGNORE INTO assembled_review_contexts(evidence_id,project_id,revision_id,artifact_id,body_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![context.evidence_id,context.project_id,context.revision_id,context.artifact_id,body,crate::now()])?;
        Ok(context)
    }

    /// Historical evidence read. This does not certify present media or intent.
    pub fn assembled_review_evidence(&self, id: &str) -> Result<AssembledReviewContext> {
        let body: String = self
            .connection()?
            .query_row(
                "SELECT body_json FROM assembled_review_contexts WHERE evidence_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .context("assembled review context not found")?;
        ensure!(
            body.len() <= 8 * 1024 * 1024,
            "assembled review context exceeds bound"
        );
        Ok(serde_json::from_str(&body)?)
    }

    pub fn record_assembled_review_note(
        &self,
        context_id: &str,
        request: &super::feedback::RecordReviewNoteRequest,
    ) -> Result<super::feedback::ReviewFeedbackView> {
        self.record_assembled_review_note_cancellable(context_id, request, &|| false)
    }

    pub fn record_assembled_review_note_cancellable(
        &self,
        context_id: &str,
        request: &super::feedback::RecordReviewNoteRequest,
        cancel: &dyn Fn() -> bool,
    ) -> Result<super::feedback::ReviewFeedbackView> {
        let context = self.assembled_review_evidence(context_id)?;
        ensure!(
            request.revision_id == context.revision_id
                && request.project_id == context.project_id
                && request.viewed_artifact_id.as_deref() == Some(context.artifact_id.as_str())
                && context
                    .validation
                    .cuts
                    .iter()
                    .any(|c| c.operation_id == request.operation_id),
            "invalid input: assembled feedback target differs from the viewed context"
        );
        let status =
            self.preview_artifact_status_cancellable(&context.artifact_id, true, cancel)?;
        for handle in &context.source_handles {
            if handle["state"] == "complete" {
                let inspected: SourceInspection =
                    serde_json::from_value(handle["inspection"].clone())?;
                self.require_current_source_inspection_cancellable(
                    &inspected.evidence_id,
                    &inspected.request.asset_id,
                    cancel,
                )?;
            }
        }
        ensure!(
            status.current_for_request == Some(true)
                && status.artifact.identity.as_ref() == Some(&context.input_identity),
            "stale evidence: assembled context changed"
        );
        let mut request = request.clone();
        if !request.evidence_refs.iter().any(|id| id == context_id) {
            request.evidence_refs.push(context_id.into());
        }
        ensure!(!cancel(), "assembled review note cancelled");
        self.record_review_note(&request)
    }
}
