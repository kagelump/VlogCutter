//! Version-one, path-free handoff of a verified local movie to TV review.
//! This is derived evidence. Importing it never imports a canonical edit plan.
use super::{EditPlanAudio, PreviewArtifactState};
use crate::Catalog;
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const REVIEW_TARGET_SCHEMA_VERSION: u32 = 1;
pub const REVIEW_TARGET_MAX_BYTES: usize = 16 * 1024 * 1024;
pub const REVIEW_TARGET_MAX_CUTS: usize = 20_000;
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Debug, thiserror::Error)]
pub enum ReviewTargetError {
    #[error("unsupported review target schema version")]
    UnsupportedVersion,
    #[error("review target digest mismatch")]
    DigestMismatch,
    #[error("verification expired or is in the future; re-export on the owning host")]
    VerificationExpired,
    #[error("exact complete artifact must pass fresh byte/input/constraint verification")]
    ArtifactNotVerified,
}
impl ReviewTargetError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedVersion => "review_target_unsupported_version",
            Self::DigestMismatch => "review_target_digest_mismatch",
            Self::VerificationExpired => "review_target_verification_expired",
            Self::ArtifactNotVerified => "review_target_artifact_not_verified",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewTargetHandoff {
    pub schema_version: u32,
    /// BLAKE3 of the versioned typed snapshot, excluding the renewable verification.
    pub target_digest: String,
    pub target: ReviewTargetSnapshot,
    pub verification: ReviewTargetVerification,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewTargetSnapshot {
    pub project_id: String,
    pub revision_id: String,
    pub viewed_artifact_id: String,
    pub plan_digest: String,
    pub constraints_key: String,
    pub movie: ReviewTargetMovie,
    pub cuts: Vec<ReviewTargetCut>,
    /// A user declaration only. No YouTube fetch, byte check, OAuth, or playback proof.
    pub youtube_video_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewTargetMovie {
    /// Digest of the entire rendered MP4, including its final mixed soundtrack.
    pub blake3: String,
    pub byte_length: u64,
    pub duration_seconds: f64,
    pub time_base_numerator: i64,
    pub time_base_denominator: i64,
    pub origin_pts: i64,
    pub video_frame_count: u64,
    pub audio_sample_rate: u32,
    pub audio_channels: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewTargetCut {
    pub operation_id: String,
    pub position: u32,
    pub asset_id: String,
    pub source_in_seconds: f64,
    pub source_out_seconds: f64,
    pub movie_in_pts: i64,
    pub movie_out_pts: i64,
    pub output_frame_count: u64,
    /// Canonical declaration, NOT measured audible presence or source-frame identity.
    /// null = legacy linked source; silent = explicit silence; source = independent timing.
    pub declared_audio: Option<EditPlanAudio>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewTargetVerification {
    pub verified_at: String,
    pub output_matches: bool,
    pub inputs_match: bool,
    pub constraints_match: bool,
    pub plan_renderable: bool,
    pub fully_decoded: bool,
}

fn opaque(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn safe_integer(value: i64) -> bool {
    (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&value)
}

impl ReviewTargetHandoff {
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= REVIEW_TARGET_MAX_BYTES,
            "invalid input: review target exceeds 16 MiB"
        );
        let handoff: Self = serde_json::from_slice(bytes)?;
        handoff.validate()?;
        Ok(handoff)
    }

    pub fn snapshot_digest(target: &ReviewTargetSnapshot) -> Result<String> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"vlogcutter.review-target.v1\0");
        hasher.update(&serde_json::to_vec(target)?);
        Ok(hasher.finalize().to_hex().to_string())
    }

    /// Structural/integrity validation only. A checksum cannot authenticate its issuer,
    /// establish current file availability, or verify what a TV is playing.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == REVIEW_TARGET_SCHEMA_VERSION,
            ReviewTargetError::UnsupportedVersion
        );
        let v = &self.verification;
        ensure!(
            v.output_matches
                && v.inputs_match
                && v.constraints_match
                && v.plan_renderable
                && v.fully_decoded,
            ReviewTargetError::ArtifactNotVerified
        );
        DateTime::parse_from_rfc3339(&v.verified_at).context("invalid verification timestamp")?;
        validate_snapshot(&self.target, &self.target_digest)
    }

    /// Policy is explicit; callers choose the maximum age. Never renew an imported
    /// receipt by changing its timestamp. Re-export from the owning catalog instead.
    pub fn require_fresh(&self, now: DateTime<Utc>, max_age_seconds: u32) -> Result<()> {
        self.validate()?;
        ensure!(
            max_age_seconds > 0,
            "invalid input: verification lifetime must be positive"
        );
        let at = DateTime::parse_from_rfc3339(&self.verification.verified_at)?;
        let age = now.signed_duration_since(at);
        ensure!(
            age >= chrono::Duration::zero()
                && age <= chrono::Duration::seconds(i64::from(max_age_seconds)),
            ReviewTargetError::VerificationExpired
        );
        Ok(())
    }

    /// Half-open decoded movie intervals, on elapsed movie time (not raw PTS).
    /// Candidates are picture locators only; never source trims or audio ownership.
    pub fn picture_candidates(&self, start_seconds: f64, end_seconds: f64) -> Result<Vec<String>> {
        self.validate()?;
        self.picture_candidates_validated(start_seconds, end_seconds)
    }

    /// Core batch callers must validate this immutable handoff once before
    /// querying. Avoid rehashing an entire target for every utterance slice.
    pub(crate) fn picture_candidates_validated(
        &self,
        start_seconds: f64,
        end_seconds: f64,
    ) -> Result<Vec<String>> {
        self.target
            .picture_candidates_validated(start_seconds, end_seconds)
    }
}
impl ReviewTargetSnapshot {
    pub(crate) fn picture_candidates_validated(
        &self,
        start_seconds: f64,
        end_seconds: f64,
    ) -> Result<Vec<String>> {
        ensure!(
            start_seconds.is_finite()
                && end_seconds.is_finite()
                && start_seconds >= 0.0
                && end_seconds >= start_seconds
                && end_seconds <= self.movie.duration_seconds,
            "invalid input: movie query outside artifact duration"
        );
        let m = &self.movie;
        let seconds = |pts: i64| {
            (pts as f64 - m.origin_pts as f64) * m.time_base_numerator as f64
                / m.time_base_denominator as f64
        };
        let first = self
            .cuts
            .partition_point(|cut| seconds(cut.movie_out_pts) <= start_seconds);
        Ok(self.cuts[first..]
            .iter()
            .take_while(|cut| {
                if start_seconds == end_seconds {
                    seconds(cut.movie_in_pts) <= start_seconds
                } else {
                    seconds(cut.movie_in_pts) < end_seconds
                }
            })
            .map(|c| c.operation_id.clone())
            .collect())
    }
}

pub(super) fn validate_snapshot(t: &ReviewTargetSnapshot, target_digest: &str) -> Result<()> {
    ensure!(
        [&t.project_id, &t.revision_id, &t.viewed_artifact_id]
            .into_iter()
            .all(|v| opaque(v)),
        "invalid input: opaque target identities required"
    );
    ensure!(
        [
            target_digest,
            t.plan_digest.as_str(),
            t.constraints_key.as_str(),
            t.movie.blake3.as_str()
        ]
        .into_iter()
        .all(digest),
        "invalid input: full BLAKE3 digests required"
    );
    if let Some(id) = &t.youtube_video_id {
        ensure!(
            id.len() == 11 && opaque(id),
            "invalid input: YouTube video ID must contain 11 URL-safe characters"
        );
    }
    let m = &t.movie;
    ensure!(
        m.byte_length > 0
            && m.byte_length <= MAX_SAFE_INTEGER as u64
            && m.duration_seconds.is_finite()
            && m.duration_seconds > 0.0
            && m.duration_seconds <= 21_600.0
            && m.time_base_numerator > 0
            && m.time_base_denominator > 0
            && safe_integer(m.time_base_numerator)
            && safe_integer(m.time_base_denominator)
            && safe_integer(m.origin_pts)
            && m.video_frame_count > 0
            && m.video_frame_count <= MAX_SAFE_INTEGER as u64
            && m.audio_sample_rate == 48_000
            && m.audio_channels == 2,
        "invalid input: unsupported or incomplete movie clock/media receipt"
    );
    ensure!(
        !t.cuts.is_empty() && t.cuts.len() <= REVIEW_TARGET_MAX_CUTS,
        "invalid input: review target requires 1..20000 measured cuts"
    );
    let mut seen = HashSet::new();
    let mut cursor = m.origin_pts;
    let mut frames = 0_u64;
    for (position, cut) in t.cuts.iter().enumerate() {
        ensure!(
            opaque(&cut.operation_id)
                && opaque(&cut.asset_id)
                && seen.insert(&cut.operation_id)
                && cut.position as usize == position,
            "invalid input: duplicate/invalid cut identity or position"
        );
        ensure!(
            cut.source_in_seconds.is_finite()
                && cut.source_out_seconds.is_finite()
                && cut.source_in_seconds >= 0.0
                && cut.source_out_seconds > cut.source_in_seconds
                && cut.source_out_seconds <= 21_600.0,
            "invalid input: declared source range"
        );
        ensure!(
            cut.movie_in_pts == cursor
                && cut.movie_out_pts > cursor
                && safe_integer(cut.movie_out_pts)
                && cut.output_frame_count > 0,
            "invalid input: missing, overlapping or reversed measured cut mapping"
        );
        cursor = cut.movie_out_pts;
        frames = frames
            .checked_add(cut.output_frame_count)
            .context("frame count overflow")?;
        if let Some(EditPlanAudio::Source(a)) = &cut.declared_audio {
            ensure!(
                a.version == 1
                    && opaque(&a.asset_id)
                    && [
                        a.source_in_seconds,
                        a.source_out_seconds,
                        a.timeline_start_seconds,
                        a.sync_offset_seconds,
                        a.gain_db,
                        a.fade_in_seconds,
                        a.fade_out_seconds
                    ]
                    .iter()
                    .all(|v| v.is_finite())
                    && a.source_in_seconds >= 0.0
                    && a.source_out_seconds > a.source_in_seconds
                    && a.source_out_seconds <= 21_600.0
                    && a.timeline_start_seconds >= 0.0
                    && a.timeline_start_seconds + a.source_out_seconds - a.source_in_seconds
                        <= m.duration_seconds + 0.1
                    && (-60.0..=0.0).contains(&a.gain_db)
                    && (0.0..=0.25).contains(&a.fade_in_seconds)
                    && (0.0..=0.25).contains(&a.fade_out_seconds)
                    && a.fade_in_seconds + a.fade_out_seconds
                        <= a.source_out_seconds - a.source_in_seconds,
                "invalid input: declared independent audio"
            );
        }
    }
    let picture_end = (cursor as f64 - m.origin_pts as f64) * m.time_base_numerator as f64
        / m.time_base_denominator as f64;
    ensure!(
        picture_end <= m.duration_seconds + 0.000_001 && frames == m.video_frame_count,
        "invalid input: movie duration/frame coverage disagrees with mapping"
    );
    ensure!(
        ReviewTargetHandoff::snapshot_digest(t)? == target_digest,
        ReviewTargetError::DigestMismatch
    );
    Ok(())
}

impl Catalog {
    /// Fresh verification; no movie copy, source upload, timeline write or activation.
    pub fn export_review_target(
        &self,
        artifact_id: &str,
        youtube_video_id: Option<String>,
        cancel: &dyn Fn() -> bool,
    ) -> Result<ReviewTargetHandoff> {
        ensure!(opaque(artifact_id), "invalid input: artifact identity");
        ensure!(!cancel(), "review target export cancelled");
        if let Some(id) = &youtube_video_id {
            ensure!(
                id.len() == 11 && opaque(id),
                "invalid input: YouTube video ID must contain 11 URL-safe characters"
            );
        }
        let status = self.preview_artifact_status_cancellable(artifact_id, true, cancel)?;
        ensure!(
            status.current_for_request == Some(true)
                && status.file_present
                && status.observed_state == PreviewArtifactState::Complete,
            ReviewTargetError::ArtifactNotVerified
        );
        let artifact = status.artifact;
        ensure!(
            artifact.kind == "fullPreview" && artifact.excerpt.is_none(),
            "invalid input: v1 TV review requires a full preview, not an excerpt"
        );
        let plan = self.edit_plan_revision(&artifact.revision_id)?;
        let verified = status.verification.context("missing fresh verification")?;
        let fully_decoded = artifact
            .validation
            .as_ref()
            .is_some_and(|v| v.fully_decoded);
        let target = snapshot_from_artifact(artifact, &plan, youtube_video_id, cancel)?;
        let handoff = ReviewTargetHandoff {
            schema_version: REVIEW_TARGET_SCHEMA_VERSION,
            target_digest: ReviewTargetHandoff::snapshot_digest(&target)?,
            target,
            verification: ReviewTargetVerification {
                verified_at: verified.verified_at,
                output_matches: verified.output_matches,
                inputs_match: verified.inputs_match,
                constraints_match: verified.constraints_match,
                plan_renderable: verified.plan_renderable,
                fully_decoded,
            },
        };
        handoff.validate()?;
        ensure!(
            serde_json::to_vec(&handoff)?.len() <= REVIEW_TARGET_MAX_BYTES,
            "invalid input: review target exceeds 16 MiB"
        );
        Ok(handoff)
    }
}

pub(super) fn snapshot_from_artifact(
    artifact: super::PreviewArtifact,
    plan: &super::EditPlanRevision,
    youtube_video_id: Option<String>,
    cancel: &dyn Fn() -> bool,
) -> Result<ReviewTargetSnapshot> {
    let identity = artifact.identity.context("missing preview identity")?;
    let validation = artifact
        .validation
        .context("missing measured movie mapping")?;
    ensure!(
        plan.project_id == artifact.project_id
            && identity.recipe == super::preview_media::RECIPE
            && identity.max_height == artifact.request.max_height
            && identity.revision_id == plan.revision_id
            && validation.cuts.len() == plan.operations.len(),
        "invalid receipt: project/revision/cut count mismatch"
    );
    let mut cuts = Vec::with_capacity(plan.operations.len());
    for (op, measured) in plan.operations.iter().zip(&validation.cuts) {
        ensure!(!cancel(), "review target export cancelled");
        ensure!(
            op.operation_id == measured.operation_id
                && op.asset_id == measured.asset_id
                && op.source_in_seconds == measured.source_in_seconds
                && op.source_out_seconds == measured.source_out_seconds,
            "invalid receipt: canonical cut differs from measured mapping"
        );
        cuts.push(ReviewTargetCut {
            operation_id: op.operation_id.clone(),
            position: op.position,
            asset_id: op.asset_id.clone(),
            source_in_seconds: op.source_in_seconds,
            source_out_seconds: op.source_out_seconds,
            movie_in_pts: measured.movie_in_pts,
            movie_out_pts: measured.movie_out_pts,
            output_frame_count: measured.output_frame_count,
            declared_audio: op.audio.clone(),
        });
    }
    Ok(ReviewTargetSnapshot {
        project_id: artifact.project_id,
        revision_id: plan.revision_id.clone(),
        viewed_artifact_id: artifact.artifact_id,
        plan_digest: identity.plan_digest,
        constraints_key: identity.constraints_key,
        movie: ReviewTargetMovie {
            blake3: validation.output_fingerprint,
            byte_length: validation.byte_length,
            duration_seconds: validation.duration_seconds,
            time_base_numerator: validation.movie_time_base_numerator,
            time_base_denominator: validation.movie_time_base_denominator,
            origin_pts: validation.movie_origin_pts,
            video_frame_count: validation.video_frame_count,
            audio_sample_rate: validation.audio_sample_rate,
            audio_channels: validation.audio_channels,
        },
        cuts,
        youtube_video_id,
    })
}

#[cfg(test)]
#[path = "review_target_tests.rs"]
mod tests;
