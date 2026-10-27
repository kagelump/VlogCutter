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
    OriginalOffline,
    ProxyMissing,
    RangeOutOfBounds,
    RangeInverted,
    RangeZeroLength,
    SourceRangeOverlap,
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
            Self::MissingAsset => "edit_plan_missing_asset",
            Self::OriginalOffline => "edit_plan_original_offline",
            Self::ProxyMissing => "edit_plan_proxy_missing",
            Self::RangeOutOfBounds => "edit_plan_range_out_of_bounds",
            Self::RangeInverted => "edit_plan_range_inverted",
            Self::RangeZeroLength => "edit_plan_range_zero_length",
            Self::SourceRangeOverlap => "edit_plan_source_range_overlap",
            Self::StaleScriptRevision => "edit_plan_stale_script_revision",
            Self::StaleRevision => "edit_plan_stale_revision_warning",
        }
    }

    pub const fn severity(self) -> Severity {
        match self {
            Self::MissingAsset
            | Self::OriginalOffline
            | Self::RangeOutOfBounds
            | Self::RangeInverted
            | Self::RangeZeroLength => Severity::Error,
            Self::ProxyMissing
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
        let revision = self.edit_plan_revision(revision_id)?;
        let assets = self.list_assets(&revision.project_id)?;
        let mut findings: Vec<Finding> = Vec::new();

        for operation in &revision.operations {
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

        // Overlapping source ranges from one asset are legal — reusing a shot
        // is a real edit — but usually accidental, so they warn.
        for (index, left) in revision.operations.iter().enumerate() {
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
