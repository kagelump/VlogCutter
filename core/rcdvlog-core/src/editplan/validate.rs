//! Edit-plan validation (D01-T020).
//!
//! Answers one question before anything expensive happens: can this plan
//! actually be rendered and exported? Errors block; warnings inform. The split
//! matters — a plan that would produce a broken timeline must not reach a
//! finishing editor, but a missing proxy only makes the preview slower.

use crate::Catalog;
use anyhow::Result;
use serde::{Deserialize, Serialize};

use std::path::Path;

/// What is wrong with a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    MissingAsset,
    AudioInvalid,
    OriginalOffline,
    ProxyMissing,
    RangeOutOfBounds,
    RangeInverted,
    RangeZeroLength,
    SourceRangeOverlap,
    ConfirmedRangeViolation,
    HumanChoiceViolation,
    SelectionConflict,
    SelectionReview,
    StaleScriptRevision,
    StaleRevision,
}

/// Errors block render and export; warnings do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

impl FindingKind {
    /// The stable code, referenced by the desktop and the CLI.
    pub const fn code(self) -> &'static str {
        match self {
            Self::SelectionConflict => "edit_plan_selection_conflict",
            Self::SelectionReview => "edit_plan_selection_review",
            Self::HumanChoiceViolation => "edit_plan_human_choice_violation",
            Self::AudioInvalid => "edit_plan_audio_invalid",
            Self::MissingAsset => "edit_plan_missing_asset",
            Self::OriginalOffline => "edit_plan_original_offline",
            Self::ProxyMissing => "edit_plan_proxy_missing",
            Self::RangeOutOfBounds => "edit_plan_range_out_of_bounds",
            Self::RangeInverted => "edit_plan_range_inverted",
            Self::RangeZeroLength => "edit_plan_range_zero_length",
            Self::SourceRangeOverlap => "edit_plan_source_range_overlap",
            Self::ConfirmedRangeViolation => "edit_plan_confirmed_range_violation",
            Self::StaleScriptRevision => "edit_plan_stale_script_revision",
            Self::StaleRevision => "edit_plan_stale_revision_warning",
        }
    }

    pub const fn severity(self) -> Severity {
        match self {
            Self::AudioInvalid
            | Self::SelectionConflict
            | Self::HumanChoiceViolation
            | Self::ConfirmedRangeViolation
            | Self::MissingAsset
            | Self::OriginalOffline
            | Self::RangeOutOfBounds
            | Self::RangeInverted
            | Self::RangeZeroLength => Severity::Error,
            Self::SelectionReview
            | Self::ProxyMissing
            | Self::SourceRangeOverlap
            | Self::StaleScriptRevision
            | Self::StaleRevision => Severity::Warning,
        }
    }
}

/// One problem, located precisely enough to act on.
///
/// `code` is a `&'static str` because it is a fixed vocabulary, not data — which
/// is also why this type serializes but does not deserialize. Nothing reads a
/// `Finding` back in; it is produced fresh from the plan every time.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub kind: FindingKind,
    pub severity: Severity,
    pub code: &'static str,
    pub position: Option<u32>,
    pub asset_id: Option<String>,
    pub message: String,
}

impl Finding {
    fn new(
        kind: FindingKind,
        position: Option<u32>,
        asset_id: Option<String>,
        message: String,
    ) -> Self {
        Self {
            kind,
            severity: kind.severity(),
            code: kind.code(),
            position,
            asset_id,
            message,
        }
    }
}

/// Everything wrong with one plan, ordered by position then kind so the report
/// reads down the timeline.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationReport {
    pub findings: Vec<Finding>,
}

impl ValidationReport {
    /// True when no finding has severity `Error`. Warnings never block.
    pub fn renderable(&self) -> bool {
        !self
            .findings
            .iter()
            .any(|finding| finding.severity == Severity::Error)
    }

    pub fn errors(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.severity == Severity::Error)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.severity == Severity::Warning)
    }
}

impl Catalog {
    /// Checks whether a plan can be rendered and exported.
    pub fn validate_edit_plan(&self, revision_id: &str) -> Result<ValidationReport> {
        self.validate_edit_plan_cancellable(revision_id, &|| false)
    }

    pub(super) fn validate_edit_plan_cancellable(
        &self,
        revision_id: &str,
        cancel: &dyn Fn() -> bool,
    ) -> Result<ValidationReport> {
        self.validate_edit_plan_audio_window(revision_id, cancel, None)
    }

    pub(super) fn validate_edit_plan_audio_window(
        &self,
        revision_id: &str,
        cancel: &dyn Fn() -> bool,
        audio_window: Option<(f64, f64)>,
    ) -> Result<ValidationReport> {
        anyhow::ensure!(!cancel(), "preview cancelled during validation");
        let revision = self.edit_plan_revision(revision_id)?;
        self.validate_edit_plan_snapshot(&revision, cancel, audio_window)
    }

    pub(super) fn validate_edit_plan_snapshot(
        &self,
        revision: &super::EditPlanRevision,
        cancel: &dyn Fn() -> bool,
        audio_window: Option<(f64, f64)>,
    ) -> Result<ValidationReport> {
        let revision_id = &revision.revision_id;
        let assets = self.list_assets(&revision.project_id)?;
        let mut findings: Vec<Finding> = Vec::new();
        let constraints =
            super::ranges::load_for_project(&self.connection()?, &revision.project_id)?;
        if !crate::table_exists(&self.connection()?, "confirmed_source_ranges")? {
            let pending:bool=self.connection()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM project_operations WHERE project_id=?1 AND operation_type IN ('sourceRangeConfirmed','sourceRangeRevoked'))",
                [&revision.project_id],|r|r.get(0))?;
            if pending {
                findings.push(Finding::new(FindingKind::ConfirmedRangeViolation,None,None,
                "Human range decisions require an explicit writable catalog upgrade before this plan can be validated.".into()));
            }
        }
        for (asset_id, message) in
            super::feedback::retained_choice_violations(&self.connection()?, revision)?
        {
            findings.push(Finding::new(
                FindingKind::HumanChoiceViolation,
                None,
                Some(asset_id),
                message,
            ));
        }
        if !crate::table_exists(&self.connection()?, "edit_feedback_note_heads")? {
            let pending: bool = self.connection()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM project_operations WHERE project_id=?1 AND operation_type='editReviewDecisionRecorded'
                    AND json_extract(payload_json,'$.fact.state')='accepted' AND json_extract(payload_json,'$.fact.hardChoice') IS NOT NULL)",
                [&revision.project_id], |r|r.get(0))?;
            if pending {
                findings.push(Finding::new(FindingKind::HumanChoiceViolation, None, None,
                "Retained human feedback requires an explicit writable catalog upgrade before validation.".into()));
            }
        }
        if let Some(metadata) = super::selection::load_metadata(&self.connection()?, revision_id)? {
            for conflict in super::selection_report::report(revision, &metadata).conflicts {
                findings.push(Finding::new(
                    if conflict.blocking {
                        FindingKind::SelectionConflict
                    } else {
                        FindingKind::SelectionReview
                    },
                    conflict
                        .operation_id
                        .as_ref()
                        .and_then(|id| revision.operations.iter().find(|c| &c.operation_id == id))
                        .map(|c| c.position),
                    conflict.asset_id,
                    format!("{}: {}", conflict.code, conflict.message),
                ));
            }
        }
        for cut in &revision.operations {
            anyhow::ensure!(!cancel(), "preview cancelled during validation");
            if let Some(asset) = assets.iter().find(|a| a.id == cut.asset_id)
                && let Some(records) = constraints.get(&asset.id)
            {
                check_confirmed_range(
                    &mut findings,
                    cut.position,
                    asset,
                    records,
                    cut.source_in_seconds,
                    cut.source_out_seconds,
                    super::TrimMedia::Picture,
                );
            }
            let audio = match &cut.audio {
                Some(super::EditPlanAudio::Silent) => None,
                Some(super::EditPlanAudio::Source(audio)) => Some((
                    audio.asset_id.as_str(),
                    audio.source_in_seconds,
                    audio.source_out_seconds,
                )),
                None => Some((
                    cut.asset_id.as_str(),
                    cut.source_in_seconds,
                    cut.source_out_seconds,
                )),
            };
            if let Some((id, start, end)) = audio
                && let Some(asset) = assets.iter().find(|a| a.id == id)
                && let Some(records) = constraints.get(id)
                && records
                    .iter()
                    .any(|r| matches!(r.media, super::RangeMedia::Audio | super::RangeMedia::Both))
            {
                let has_audio = if cut.audio.is_some() {
                    true
                } else {
                    let path = asset
                        .proxy_path
                        .as_deref()
                        .filter(|p| Path::new(p).is_file())
                        .unwrap_or(&asset.path);
                    match super::preview::source_has_audio(path, cancel) {
                        Ok(present) => present,
                        Err(_) => {
                            findings.push(Finding::new(FindingKind::ConfirmedRangeViolation,Some(cut.position),Some(asset.id.clone()),
                                "Selected linked-audio presence is unchecked; source constraints cannot be verified until media is available.".into()));
                            false
                        }
                    }
                };
                if has_audio {
                    check_confirmed_range(
                        &mut findings,
                        cut.position,
                        asset,
                        records,
                        start,
                        end,
                        super::TrimMedia::Audio,
                    );
                }
            }
        }

        for operation in &revision.operations {
            anyhow::ensure!(!cancel(), "preview cancelled during validation");
            let position = Some(operation.position);
            let asset_id = Some(operation.asset_id.clone());
            let Some(asset) = assets.iter().find(|asset| asset.id == operation.asset_id) else {
                findings.push(Finding::new(
                    FindingKind::MissingAsset,
                    position,
                    asset_id,
                    format!(
                        "Clip {} at position {} is no longer in this project.",
                        operation.asset_id, operation.position
                    ),
                ));
                continue;
            };

            // Range shape first: an inverted or empty range is wrong regardless
            // of whether the media is reachable.
            if operation.source_out_seconds < operation.source_in_seconds {
                findings.push(Finding::new(
                    FindingKind::RangeInverted,
                    position,
                    asset_id.clone(),
                    format!(
                        "Position {} ends at {:.3}s, before it starts at {:.3}s.",
                        operation.position,
                        operation.source_out_seconds,
                        operation.source_in_seconds
                    ),
                ));
            } else if (operation.source_out_seconds - operation.source_in_seconds).abs()
                < f64::EPSILON
            {
                findings.push(Finding::new(
                    FindingKind::RangeZeroLength,
                    position,
                    asset_id.clone(),
                    format!("Position {} has no duration.", operation.position),
                ));
            }

            if !operation.source_in_seconds.is_finite()
                || !operation.source_out_seconds.is_finite()
                || !asset.duration_seconds.is_finite()
                || asset.duration_seconds <= 0.0
                || operation.source_in_seconds < 0.0
                || operation.source_out_seconds > asset.duration_seconds
            {
                findings.push(Finding::new(
                    FindingKind::RangeOutOfBounds,
                    position,
                    asset_id.clone(),
                    format!(
                        "Position {} asks for {:.3}s–{:.3}s of a clip that is only {:.3}s long.",
                        operation.position,
                        operation.source_in_seconds,
                        operation.source_out_seconds,
                        asset.duration_seconds
                    ),
                ));
            }

            // The export conforms against camera masters, so an offline
            // original blocks; a missing proxy only degrades the preview.
            if asset.path.is_empty() || !Path::new(&asset.path).exists() {
                findings.push(Finding::new(
                    FindingKind::OriginalOffline,
                    position,
                    asset_id.clone(),
                    format!(
                        "The original media for position {} is not reachable at {}.",
                        operation.position, asset.path
                    ),
                ));
            }
            if asset
                .proxy_path
                .as_deref()
                .is_none_or(|proxy| !Path::new(proxy).exists())
            {
                findings.push(Finding::new(
                    FindingKind::ProxyMissing,
                    position,
                    asset_id.clone(),
                    format!(
                        "Position {} has no proxy, so preview will use the original.",
                        operation.position
                    ),
                ));
            }
        }

        if revision.operations.iter().any(|cut| cut.audio.is_some())
            && let Err(error) = super::audio::audio_events_cancellable_window(
                revision,
                &assets,
                cancel,
                audio_window,
            )
        {
            findings.push(Finding::new(
                FindingKind::AudioInvalid,
                None,
                None,
                error.to_string(),
            ));
        }

        anyhow::ensure!(!cancel(), "preview cancelled during validation");
        // Overlapping source ranges from one asset are legal — reusing a shot
        // is a real edit — but usually accidental, so they warn.
        for (index, left) in revision.operations.iter().enumerate() {
            anyhow::ensure!(!cancel(), "preview cancelled during validation");
            for right in revision.operations.iter().skip(index + 1) {
                if left.asset_id != right.asset_id {
                    continue;
                }
                let overlaps = left.source_in_seconds < right.source_out_seconds
                    && right.source_in_seconds < left.source_out_seconds;
                if overlaps {
                    findings.push(Finding::new(
                        FindingKind::SourceRangeOverlap,
                        Some(right.position),
                        Some(right.asset_id.clone()),
                        format!(
                            "Positions {} and {} use overlapping parts of the same clip.",
                            left.position, right.position
                        ),
                    ));
                }
            }
        }

        let staleness = self.edit_plan_staleness(revision_id)?;
        if staleness.stale {
            findings.push(Finding::new(
                FindingKind::StaleScriptRevision,
                None,
                None,
                "This plan was built from an older version of the script.".to_string(),
            ));
        }
        if !revision.active
            && let Some(active) = self.active_edit_plan_revision_id(&revision.project_id)?
        {
            findings.push(Finding::new(
                FindingKind::StaleRevision,
                None,
                None,
                format!("Revision {active} is the active plan; this one is not."),
            ));
        }

        // Deterministic: down the timeline, then by kind for one position.
        findings.sort_by(|left, right| {
            left.position
                .cmp(&right.position)
                .then_with(|| left.code.cmp(right.code))
        });
        Ok(ValidationReport { findings })
    }
}

fn check_confirmed_range(
    findings: &mut Vec<Finding>,
    position: u32,
    asset: &crate::Asset,
    records: &[super::ConfirmedSourceRange],
    start: f64,
    end: f64,
    media: super::TrimMedia,
) {
    if !start.is_finite() || !end.is_finite() || end <= start {
        return;
    }
    let (allowed, conflicts) =
        super::ranges::feasible_ranges(records, &asset.fingerprint, asset.duration_seconds, media);
    if !conflicts.is_empty()
        || !allowed.iter().any(|r| {
            super::ranges::contains(
                *r,
                super::SourceTimeRange {
                    source_in_seconds: start,
                    source_out_seconds: end,
                },
            )
        })
    {
        findings.push(Finding::new(
            FindingKind::ConfirmedRangeViolation,
            Some(position),
            Some(asset.id.clone()),
            format!(
                "{media:?} range {start:.9}–{end:.9}s violates confirmed human source ranges. {}",
                conflicts.join(" ")
            ),
        ));
    }
}
