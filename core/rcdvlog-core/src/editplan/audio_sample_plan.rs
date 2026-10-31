//! Plan bounded audit requests from measured joins and declared audio events.
use super::audio_integrity::{AudioIntegrityRequest, AudioIntegritySampleRequest};
use super::*;
use crate::Catalog;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AudioSamplePlanRequest {
    pub artifact_id: String,
    pub sample_seconds: f64,
    pub max_samples: usize,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioSamplePlan {
    pub request: AudioIntegrityRequest,
    pub deferred_boundaries: usize,
    pub unchecked_movie_ranges: Vec<AudioIntegritySampleRequest>,
    pub limitations: Vec<String>,
}
impl Catalog {
    /// Read-only planning. The subsequent audit performs fresh byte verification.
    pub fn plan_audio_integrity_samples(
        &self,
        input: &AudioSamplePlanRequest,
    ) -> Result<AudioSamplePlan> {
        ensure!(
            input.sample_seconds.is_finite()
                && (0.02..=1.0).contains(&input.sample_seconds)
                && (1..=32).contains(&input.max_samples),
            "invalid input: audio sample planner limits"
        );
        let artifact = self
            .preview_artifact_status(&input.artifact_id, false)?
            .artifact;
        ensure!(
            artifact.state == PreviewArtifactState::Complete,
            "invalid input: sample planning requires a completed preview"
        );
        let scope = artifact
            .excerpt
            .clone()
            .context("invalid input: create a bounded excerpt before planning audit samples")?;
        let validation = artifact
            .validation
            .context("invalid input: preview lacks measured mappings")?;
        let plan = self.edit_plan_revision(&artifact.revision_id)?;
        let window = super::preview_excerpt::resolve(&plan, &scope)?;
        let duration = validation.duration_seconds;
        let max_tick = (duration * 48000.0).floor() as u64;
        // Keep one sample inside inclusive floating-second validation limits;
        // subtracting two divided timestamps can straddle an exact 20ms/1s edge.
        let size = ((input.sample_seconds * 48000.0).floor() as u64).clamp(961, 47_999);
        ensure!(
            max_tick >= 960,
            "invalid input: excerpt is too short for an audio sample"
        );
        let mut boundaries = BTreeSet::new();
        let seconds = |pts: i64| {
            (pts - validation.movie_origin_pts) as f64 * validation.movie_time_base_numerator as f64
                / validation.movie_time_base_denominator as f64
        };
        for cut in validation
            .cuts
            .iter()
            .take(validation.cuts.len().saturating_sub(1))
        {
            boundaries.insert((seconds(cut.movie_out_pts) * 48000.0).round().max(0.0) as u64);
        }
        for cut in &plan.operations {
            if let Some(EditPlanAudio::Source(audio)) = &cut.audio {
                for point in [
                    audio.timeline_start_seconds,
                    audio.timeline_start_seconds + audio.source_out_seconds
                        - audio.source_in_seconds,
                ] {
                    let local = point - window.in_seconds();
                    if local >= 0.0 && local <= duration {
                        boundaries.insert((local * 48000.0).round() as u64);
                    }
                }
            }
        }
        if boundaries.is_empty() {
            boundaries.insert(max_tick / 2);
        }
        let total = boundaries.len();
        let mut intervals = BTreeSet::new();
        for center in boundaries.into_iter().take(input.max_samples) {
            let start = center
                .saturating_sub(size / 2)
                .min(max_tick.saturating_sub(size));
            let end = (start + size).min(max_tick);
            if end - start >= 960 {
                intervals.insert((start, end));
            }
        }
        let samples: Vec<_> = intervals
            .iter()
            .map(|(a, b)| AudioIntegritySampleRequest {
                movie_in_seconds: *a as f64 / 48000.0,
                movie_out_seconds: *b as f64 / 48000.0,
            })
            .collect();
        let mut unchecked = Vec::new();
        let mut cursor = 0;
        for (a, b) in intervals {
            if a > cursor {
                unchecked.push(AudioIntegritySampleRequest {
                    movie_in_seconds: cursor as f64 / 48000.0,
                    movie_out_seconds: a as f64 / 48000.0,
                });
            }
            cursor = cursor.max(b);
        }
        if cursor < max_tick {
            unchecked.push(AudioIntegritySampleRequest {
                movie_in_seconds: cursor as f64 / 48000.0,
                movie_out_seconds: max_tick as f64 / 48000.0,
            });
        }
        Ok(AudioSamplePlan {deferred_boundaries:total.saturating_sub(input.max_samples),
            request:AudioIntegrityRequest {revision_id:artifact.revision_id,scope,artifact_id:Some(input.artifact_id.clone()),samples},
            unchecked_movie_ranges:unchecked,limitations:vec![
                "Planning reads stored measured mappings; the audit must verify current bytes.".into(),
                "Bounded energy samples and declared audio boundaries do not certify listening or word completeness.".into()]})
    }
}
