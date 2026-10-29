//! Whole-cut excerpts retain the immutable full revision as their owner.
use super::{EditPlanAudio, EditPlanRevision, PreviewRequest};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
pub(super) const EXCERPT_RECIPE: &str = "cutNeighborhood.absolute-audio-crop.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewExcerptScope {
    pub first_operation_id: String,
    pub last_operation_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewExcerptRequest {
    pub revision_id: String,
    pub max_height: u32,
    pub scope: PreviewExcerptScope,
}

#[derive(Debug, Clone)]
pub(super) struct ExcerptWindow {
    pub first: usize,
    pub last: usize,
    pub prefix_seconds: f64,
    pub first_frame: u64,
    pub end_frame: u64,
    pub frame_counts: Vec<u64>,
}
impl ExcerptWindow {
    pub fn in_seconds(&self) -> f64 {
        self.first_frame as f64 * 1001.0 / 30000.0
    }
    pub fn out_seconds(&self) -> f64 {
        self.end_frame as f64 * 1001.0 / 30000.0
    }
    pub fn duration(&self) -> f64 {
        self.out_seconds() - self.in_seconds()
    }
}

pub(super) fn resolve(
    plan: &EditPlanRevision,
    scope: &PreviewExcerptScope,
) -> Result<ExcerptWindow> {
    let first = plan
        .operations
        .iter()
        .position(|o| o.operation_id == scope.first_operation_id)
        .context("invalid input: first excerpt cut not found")?;
    let last = plan
        .operations
        .iter()
        .position(|o| o.operation_id == scope.last_operation_id)
        .context("invalid input: last excerpt cut not found")?;
    ensure!(
        first <= last && last - first < 12,
        "invalid input: excerpt requires 1-12 contiguous ordered cuts"
    );
    let mut seconds = 0.0;
    let mut previous = 0;
    let mut counts = Vec::new();
    let mut first_frame = 0;
    let mut prefix = 0.0;
    for (index, cut) in plan.operations.iter().enumerate().take(last + 1) {
        let duration = cut.source_out_seconds - cut.source_in_seconds;
        ensure!(
            duration.is_finite() && duration > 0.0,
            "invalid input: excerpt plan range is invalid"
        );
        if index == first {
            first_frame = previous;
            prefix = seconds;
        }
        seconds += duration;
        ensure!(
            seconds <= 21600.0,
            "invalid input: excerpt timeline exceeds six hours"
        );
        let end = (seconds * 30000.0 / 1001.0).round() as u64;
        ensure!(
            end > previous,
            "invalid input: excerpt cut is shorter than a preview frame"
        );
        if index >= first {
            counts.push(end - previous);
        }
        previous = end;
    }
    let window = ExcerptWindow {
        first,
        last,
        prefix_seconds: prefix,
        first_frame,
        end_frame: previous,
        frame_counts: counts,
    };
    ensure!(
        window.duration() > 0.0 && window.duration() <= 60.0,
        "invalid input: excerpt must be at most 60 seconds"
    );
    Ok(window)
}

pub(super) fn relevant_assets<'a>(
    plan: &'a EditPlanRevision,
    window: &ExcerptWindow,
) -> BTreeSet<&'a str> {
    let mut ids = BTreeSet::new();
    let mut at = 0.0;
    for (index, cut) in plan.operations.iter().enumerate() {
        let duration = cut.source_out_seconds - cut.source_in_seconds;
        if (window.first..=window.last).contains(&index) {
            ids.insert(cut.asset_id.as_str());
        }
        let audio = match &cut.audio {
            Some(EditPlanAudio::Silent) => None,
            Some(EditPlanAudio::Source(a)) => Some((
                a.asset_id.as_str(),
                a.timeline_start_seconds,
                a.source_out_seconds - a.source_in_seconds,
            )),
            None => Some((cut.asset_id.as_str(), at, duration)),
        };
        if let Some((id, start, len)) = audio
            && start < window.out_seconds()
            && start + len > window.in_seconds()
        {
            ids.insert(id);
        }
        at += duration;
    }
    ids
}

impl crate::Catalog {
    pub fn render_preview_excerpt(
        &self,
        request: &PreviewExcerptRequest,
        progress: &mut dyn FnMut(super::PreviewProgress),
        cancel: &dyn Fn() -> bool,
    ) -> Result<super::PreviewResult> {
        self.render_preview_scoped(
            &PreviewRequest {
                revision_id: request.revision_id.clone(),
                max_height: request.max_height,
            },
            Some(&request.scope),
            progress,
            cancel,
            None,
        )
    }
}
