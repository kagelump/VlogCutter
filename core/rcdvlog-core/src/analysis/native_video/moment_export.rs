//! Native discovery projects into the existing canonical moment artifact owner.
use super::*;
use crate::analysis::moments::{
    self, CoverageLevel, CoverageSegment, EditProfile, MomentArtifact, MomentPayload,
    MomentProvenance, MomentReceipt, MomentSpan, MomentStatus, NativeMomentSource,
};
use crate::{Catalog, RunPolicy, RunPolicyParameters};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use std::collections::{BTreeMap, BTreeSet};

const NATIVE_TIME_MAPPING: &str = "native-video.measured-first-pts.source-ms-locators.v1";
fn payload(payload: crate::AnalysisPayload) -> Result<MomentPayload> {
    Ok(match payload {
        crate::AnalysisPayload::Video => MomentPayload::Video,
        crate::AnalysisPayload::VideoAndAudio => MomentPayload::VideoAndAudio,
        crate::AnalysisPayload::VideoAndTranscript => MomentPayload::VideoAndTranscript,
        crate::AnalysisPayload::VideoAndAudioAndTranscript => {
            MomentPayload::VideoAndAudioAndTranscript
        }
        _ => anyhow::bail!("native moment projection requires an explicit video payload"),
    })
}
impl Catalog {
    pub(crate) fn publish_native_video_moments(
        &self,
        policy: &RunPolicy,
        asset_id: &str,
    ) -> Result<Option<MomentArtifact>> {
        let RunPolicyParameters::NativeVideoReview { options } = &policy.parameters else {
            anyhow::bail!("native policy missing");
        };
        if options.purpose != NativeVideoPurpose::Moments {
            return Ok(None);
        }
        let _heavy = self.acquire_local_heavy()?;
        crate::analysis::run_policy::validate_native_video_asset(self, policy, asset_id, &|| {
            false
        })?;
        let scope = policy
            .scope
            .iter()
            .find(|s| s.asset_id == asset_id)
            .context("native moment source missing")?;
        let connection = self.connection()?;
        let mut statement=connection.prepare("SELECT result_json FROM native_video_calls WHERE project_id=?1 AND asset_id=?2 AND state='completed' AND length(result_json)<=3145728 ORDER BY evidence_id LIMIT 10001")?;
        let rows = statement.query_map(params![policy.project_id, asset_id], |r| {
            r.get::<_, String>(0)
        })?;
        let mut policies = BTreeMap::new();
        let mut candidates = Vec::<(usize, crate::MomentCandidate)>::new();
        let mut candidate_bytes = 0usize;
        let mut dropped_candidates = 0u64;
        let mut evidence_ids = Vec::new();
        let mut receipts = Vec::new();
        let mut covered = Vec::new();
        let mut questions=BTreeSet::from(["Native video is locator evidence. Requested provider sampling does not prove internal frame coverage, exact source edits, complete spoken words or human acceptance.".to_string()]);
        let mut confirmed = 0.0;
        let mut estimated = 0.0;
        let mut estimated_any = false;
        let mut usage_complete = true;
        for (row_index, row) in rows.enumerate() {
            ensure!(
                row_index < 10000,
                "native source evidence exceeds bounded projection; inspect paginated windows"
            );
            let body = row?;
            ensure!(
                body.len() <= 3 * 1024 * 1024,
                "native result exceeds projection bound"
            );
            let result: NativeVideoWindowResult = serde_json::from_str(&body)?;
            if result.provenance.source_identity != scope.source_identity
                || result.provenance.prompt_version != NATIVE_VIDEO_PROMPT_VERSION
                || result.provenance.schema_version != NATIVE_VIDEO_SCHEMA_VERSION
                || result.provenance.media_recipe != NATIVE_VIDEO_MEDIA_RECIPE
                || result.provenance.actual_model.as_deref() != Some(NATIVE_VIDEO_MODEL)
                || result.provenance.reported_tier.as_deref() != Some(options.tier.as_str())
            {
                continue;
            }
            let request = &result.request;
            if !options.source_scopes.iter().any(|s| {
                s.asset_id == asset_id
                    && s.source_in_seconds <= request.source_in_seconds
                    && request.source_out_seconds <= s.source_out_seconds
            }) {
                continue;
            }
            if !policies.contains_key(&result.provenance.policy_id) {
                policies.insert(
                    result.provenance.policy_id.clone(),
                    self.load_run_authorization(&result.provenance.policy_id)?
                        .policy,
                );
            }
            let prior = &policies[&result.provenance.policy_id];
            let RunPolicyParameters::NativeVideoReview {
                options: prior_options,
            } = &prior.parameters
            else {
                continue;
            };
            if prior.payload != policy.payload
                || prior.provider != policy.provider
                || prior.primary_model != policy.primary_model
                || prior_options.purpose != NativeVideoPurpose::Moments
                || prior_options.thinking != options.thinking
                || prior_options.navigation != options.navigation
                || prior_options.max_output_tokens != options.max_output_tokens
            {
                continue;
            }
            let ledger = self
                .analysis_request(
                    result
                        .provenance
                        .request_key
                        .as_deref()
                        .context("native evidence request receipt missing")?,
                )?
                .context("native evidence canonical receipt missing")?;
            let complete = ledger.lifecycle == crate::AnalysisRequestLifecycle::Completed;
            usage_complete &= complete;
            confirmed += ledger.confirmed_cost_usd;
            if let Some(cost) = ledger.estimated_cost_usd {
                estimated += cost;
                estimated_any = true;
            }
            if !complete {
                questions.insert("Some returned native evidence has unresolved canonical usage; its complete conservative reserve remains in accounting.".into());
            }
            let clock = &result.provenance.clock;
            let start = (clock.source_offset_seconds * 1000.0).ceil() as u32;
            let end = ((clock.source_offset_seconds
                + clock.local_valid_end_seconds * clock.seconds_scale)
                * 1000.0)
                .floor() as u32;
            if start < end && end <= scope.source_duration_ms {
                covered.push(MomentSpan {
                    start_ms: start,
                    end_ms: end,
                });
            }
            evidence_ids.push(result.evidence_id.clone());
            if receipts.len() < 256 {
                receipts.push(MomentReceipt {
                    run_id: ledger.run_id.clone(),
                    step_key: result.evidence_id.clone(),
                    actual_model: ledger.actual_model.clone(),
                    provider: ledger.actual_provider.clone(),
                    generation_id: ledger.provider_generation_id.clone(),
                    cost_usd: ledger.reported_cost_usd,
                    usage_complete: complete,
                    reused: result.reused,
                    estimated_cost_usd: ledger.estimated_cost_usd,
                    cost_basis: Some(
                        if ledger.estimated_cost_usd.is_some() {
                            "published_rate_estimate"
                        } else if ledger.reported_cost_usd.is_some() {
                            "provider_reported_charge"
                        } else {
                            "unknown_reserved"
                        }
                        .into(),
                    ),
                    native_usage: result.provenance.usage.clone(),
                });
            }
            for candidate in result.candidates {
                let bytes = serde_json::to_vec(&candidate)?.len();
                candidate_bytes += bytes;
                candidates.push((bytes, candidate));
                candidates.sort_by(|a, b| {
                    b.1.score
                        .total_cmp(&a.1.score)
                        .then_with(|| a.1.id.cmp(&b.1.id))
                });
                while candidates.len() > 1024 || candidate_bytes > 4 * 1024 * 1024 {
                    if let Some((bytes, _)) = candidates.pop() {
                        candidate_bytes -= bytes;
                        dropped_candidates += 1;
                    }
                }
            }
        }
        if evidence_ids.is_empty() {
            return Ok(None);
        }
        if evidence_ids.len() > receipts.len() {
            questions.insert(format!("Receipt detail is bounded to {} of {} windows; all window identities remain available through native evidence pages.",receipts.len(),evidence_ids.len()));
        }
        if dropped_candidates > 0 {
            questions.insert(format!("Canonical candidate detail is bounded: {dropped_candidates} lower-priority candidates remain in native window results; no source choices were accepted or deleted."));
        }
        covered.sort_by_key(|s| (s.start_ms, s.end_ms));
        let mut union = Vec::<MomentSpan>::new();
        for span in covered {
            if let Some(last) = union.last_mut()
                && span.start_ms <= last.end_ms
            {
                last.end_ms = last.end_ms.max(span.end_ms);
            } else {
                union.push(span);
            }
        }
        let mut coverage = Vec::new();
        let mut cursor = 0;
        for span in union {
            if cursor < span.start_ms {
                coverage.push(segment(cursor, span.start_ms, CoverageLevel::Uninspected));
            }
            coverage.push(segment(span.start_ms, span.end_ms, CoverageLevel::Sampled));
            cursor = span.end_ms;
        }
        if cursor < scope.source_duration_ms {
            coverage.push(segment(
                cursor,
                scope.source_duration_ms,
                CoverageLevel::Uninspected,
            ));
        }
        if coverage
            .iter()
            .any(|c| c.level == CoverageLevel::Uninspected)
        {
            questions.insert("Source regions outside successful eligible native windows remain uninspected; no coverage or runtime filler is implied.".into());
        }
        let mut candidates = candidates.into_iter().map(|(_, c)| c).collect::<Vec<_>>();
        for (index, candidate) in candidates.iter_mut().enumerate() {
            candidate.rank = index as u32 + 1;
        }
        let configuration_hash = blake3::hash(&serde_json::to_vec(&(
            scope,
            &evidence_ids,
            options,
            policy.payload,
        ))?)
        .to_hex()
        .to_string();
        let identity = blake3::hash(&serde_json::to_vec(&(
            &configuration_hash,
            &candidates,
            &coverage,
            &receipts,
        ))?)
        .to_hex()
        .to_string();
        let artifact = MomentArtifact {
            asset_id: asset_id.into(),
            identity: identity.clone(),
            status: MomentStatus::NeedsReview,
            candidates,
            coverage,
            provenance: MomentProvenance {
                requested_provider: NATIVE_VIDEO_PROVIDER.into(),
                payload: payload(policy.payload)?,
                provider: NATIVE_VIDEO_PROVIDER.into(),
                requested_model: NATIVE_VIDEO_MODEL.into(),
                actual_model: NATIVE_VIDEO_MODEL.into(),
                prompt_version: NATIVE_VIDEO_PROMPT_VERSION.into(),
                schema_version: NATIVE_VIDEO_SCHEMA_VERSION.into(),
                recipe_version: NATIVE_VIDEO_MEDIA_RECIPE.into(),
                representation: scope.representation.clone(),
                representation_fingerprint: scope
                    .representation_fingerprint
                    .clone()
                    .context("native representation missing")?,
                time_mapping: NATIVE_TIME_MAPPING.into(),
                source_duration_ms: scope.source_duration_ms,
                transcript_fingerprint: scope.transcript_fingerprint.clone(),
                observation_fingerprint: None,
                profile: EditProfile::default(),
                configuration_hash,
                cost_usd: confirmed,
                usage_complete,
                receipts,
                estimated_cost_usd: estimated_any.then_some(estimated),
                cost_basis: Some(
                    "confirmed_provider_charges_and_separate_published_estimates; see receipts"
                        .into(),
                ),
                native_source: Some(NativeMomentSource {
                    policy_id: policy.policy_id.clone(),
                    source_identity: scope.source_identity.clone(),
                    evidence_ids,
                }),
            },
            review_questions: questions.into_iter().collect(),
        };
        ensure!(
            serde_json::to_vec(&artifact)?.len() <= 8 * 1024 * 1024,
            "native canonical moment projection exceeds bounded artifact size; window evidence remains intact"
        );
        crate::analysis::run_policy::validate_native_video_asset(self, policy, asset_id, &|| {
            false
        })?;
        moments::store::save_artifact(&self.connection()?, asset_id, &identity, &artifact)?;
        Ok(Some(artifact))
    }
    pub(crate) fn native_video_moment_current(
        &self,
        asset: &crate::Asset,
        artifact: &MomentArtifact,
        explicit_review: bool,
    ) -> bool {
        (|| -> Result<()> {
            let source = artifact
                .provenance
                .native_source
                .as_ref()
                .context("native moment source identity missing")?;
            let policy = self.load_run_authorization(&source.policy_id)?.policy;
            let scope = policy
                .scope
                .iter()
                .find(|s| s.asset_id == asset.id)
                .context("native moment source outside frozen policy")?;
            ensure!(
                artifact.provenance.recipe_version == NATIVE_VIDEO_MEDIA_RECIPE
                    && artifact.provenance.prompt_version == NATIVE_VIDEO_PROMPT_VERSION
                    && artifact.provenance.schema_version == NATIVE_VIDEO_SCHEMA_VERSION
                    && artifact.provenance.time_mapping == NATIVE_TIME_MAPPING
                    && artifact.provenance.payload == payload(policy.payload)?
                    && source.source_identity == scope.source_identity
                    && artifact.provenance.source_duration_ms == scope.source_duration_ms
                    && artifact.provenance.transcript_fingerprint == scope.transcript_fingerprint
                    && Some(&artifact.provenance.representation_fingerprint)
                        == scope.representation_fingerprint.as_ref(),
                "native moment recipe/source changed"
            );
            crate::analysis::run_policy::validate_native_video_asset_mode(
                self,
                &policy,
                &asset.id,
                explicit_review,
                &|| false,
            )
        })()
        .is_ok()
    }
}
fn segment(start: u32, end: u32, level: CoverageLevel) -> CoverageSegment {
    CoverageSegment {
        span: MomentSpan {
            start_ms: start,
            end_ms: end,
        },
        level,
        sample_times_ms: vec![],
        max_gap_ms: end - start,
    }
}
