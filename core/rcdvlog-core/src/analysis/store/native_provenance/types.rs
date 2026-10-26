//! Public native provenance DTOs and the bounded per-window persistence value.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::analysis::accounting::AnalysisAccountingRequestInspection;
use crate::analysis::quality::DecodedSegment;
use crate::analysis::quality::native_stt::{
    NativeSttQualityDecision, NativeSttQualityMetrics, NativeSttQualityReason,
    recognized_native_stt_reported_language,
};

use super::super::{TranscriptOutcomeReason, TranscriptStatus};
use crate::analysis::run_policy::RunPolicy;
use crate::analysis::windows::PackedWindow;

/// Schema identity for the catalog read projection over native transcript
/// provenance. The projection has no independent store.
pub const NATIVE_TRANSCRIPT_PROVENANCE_SCHEMA_VERSION: &str =
    "analysis.native-transcript-provenance.v1";

/// Versioned, body-free window plan retained on an incomplete native checkpoint
/// or a terminal native transcript assembled from exactly that completed plan.
/// The ordered `PackedWindow` map is the only source-time coordination identity;
/// raw PCM/WAV, paths and provider bodies are never stored.
pub const NATIVE_TRANSCRIPT_WINDOW_PLAN_SCHEMA_VERSION: &str =
    "analysis.native-transcript-window-plan.v1";

/// Bounded defensive limit for a retained native window plan. Existing VAD
/// packing already limits a single window; this caps pathological catalogs.
pub const MAX_NATIVE_TRANSCRIPT_PLANNED_WINDOWS: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeTranscriptWindowPlan {
    pub schema_version: String,
    pub windows: Vec<PackedWindow>,
}

/// Read-only coverage facts for one retained native plan. An incomplete
/// checkpoint may expose a completed prefix; a terminal native artifact must
/// expose every planned window exactly once. Completed window results and actual
/// attempts/costs remain in their canonical child rows/accounting ledger; this
/// projection copies neither.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeTranscriptCoverage {
    pub plan_schema_version: String,
    pub plan_fingerprint: String,
    pub planned_window_count: u32,
    pub completed_window_count: u32,
    pub remaining_window_count: u32,
}

/// Bounded, body-free quality evidence retained for one native provider window.
///
/// Decision, reason and metric coverage reuse the pure C2 evaluator's exact
/// semantics. Candidate text is intentionally absent: it remains exclusively in
/// the canonical transcript segments/header owner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSttWindowQualityObservation {
    pub decision: NativeSttQualityDecision,
    pub reason: NativeSttQualityReason,
    pub metrics: NativeSttQualityMetrics,
}

impl NativeSttWindowQualityObservation {
    /// Keep only the non-text evidence from one C2 evaluation. The evaluation
    /// recipe must be the version this persistence boundary records on the
    /// transcript header.
    pub fn from_evaluation(
        evaluation: &crate::analysis::quality::native_stt::NativeSttQualityEvaluation,
    ) -> Result<Self> {
        ensure!(
            evaluation.recipe_version
                == crate::analysis::quality::native_stt::NATIVE_STT_QUALITY_RECIPE_VERSION,
            "native quality evaluation uses an unsupported recipe version"
        );
        Ok(Self {
            decision: evaluation.decision,
            reason: evaluation.reason,
            metrics: evaluation.metrics.clone(),
        })
    }

    pub(crate) fn encode_json(&self) -> Result<String> {
        self.validate()?;
        serde_json::to_string(self).context("encode validated native quality observation")
    }

    pub(crate) fn decode_json(value: &str, quality_recipe: &str) -> Result<Self> {
        let observation: Self =
            serde_json::from_str(value).context("parse persisted native quality observation")?;
        if quality_recipe == crate::analysis::quality::native_stt::NATIVE_STT_QUALITY_RECIPE_VERSION
        {
            observation.validate()?;
        } else {
            // Historical evidence is factual under its recorded recipe. Do not
            // apply current thresholds to a prior recipe during inspection.
            observation.validate_structure()?;
        }
        Ok(observation)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        self.validate_structure()?;
        match self.decision {
            NativeSttQualityDecision::Accepted => {
                ensure!(
                    self.metrics.has_complete_passing_evidence(),
                    "accepted native quality observation must contain complete passing evidence"
                );
                ensure!(
                    !self.metrics.has_known_low_complete_evidence(),
                    "accepted native quality observation cannot hide a known low complete metric"
                );
            }
            NativeSttQualityDecision::ReviewRequired => {}
            NativeSttQualityDecision::Rejected => return Ok(()),
        }
        ensure!(
            self.metrics
                .repetition_ratio
                .is_some_and(crate::analysis::quality::native_stt::native_stt_repetition_is_safe),
            "accepted/review native quality observation must not repeat unsafely"
        );
        Ok(())
    }

    /// Validate stored facts without reinterpreting a historical recipe's
    /// acceptance thresholds using those of the current build.
    fn validate_structure(&self) -> Result<()> {
        let reason_is_review = matches!(
            self.reason,
            NativeSttQualityReason::ReviewMissingQualityEvidence
                | NativeSttQualityReason::ReviewIncompleteQualityEvidence
        );
        match self.decision {
            NativeSttQualityDecision::Accepted => {
                ensure!(
                    self.reason == NativeSttQualityReason::AcceptedCompleteEvidence,
                    "accepted native quality observation must use accepted_complete_evidence"
                );
                ensure!(
                    self.metrics.has_complete_comparable_evidence(),
                    "accepted native quality observation must contain complete evidence"
                );
            }
            NativeSttQualityDecision::ReviewRequired => {
                ensure!(
                    reason_is_review,
                    "review_required native quality observation must use a review reason"
                );
                ensure!(
                    !self.metrics.has_complete_comparable_evidence(),
                    "review_required native quality observation cannot contain complete evidence"
                );
                ensure!(
                    self.metrics.has_any_comparable_evidence()
                        == (self.reason == NativeSttQualityReason::ReviewIncompleteQualityEvidence),
                    "native review reason does not match its evidence coverage"
                );
            }
            NativeSttQualityDecision::Rejected => ensure!(
                self.reason != NativeSttQualityReason::AcceptedCompleteEvidence
                    && !reason_is_review,
                "rejected native quality observation must use a rejection reason"
            ),
        }
        validate_metric_summary(&self.metrics.avg_logprob, "native avg_logprob", |mean| {
            mean.is_finite() && mean <= 0.0
        })?;
        validate_metric_summary(
            &self.metrics.segment_confidence,
            "native segment confidence",
            |mean| mean.is_finite() && (0.0..=1.0).contains(&mean),
        )?;
        ensure!(
            self.metrics.avg_logprob.total_segments
                == self.metrics.segment_confidence.total_segments,
            "native metric coverage must describe the same candidate set"
        );
        if self.decision != NativeSttQualityDecision::Rejected {
            ensure!(
                self.metrics.avg_logprob.total_segments > 0
                    && self.metrics.repetition_ratio.is_some(),
                "accepted/review quality observation must have candidates and measured repetition"
            );
        }
        if let Some(confidence) = self.metrics.transcript_confidence {
            ensure!(
                confidence.is_finite() && (0.0..=1.0).contains(&confidence),
                "native transcript confidence must be finite and in [0, 1]"
            );
        }
        ensure!(
            self.metrics.word_confidence.known_words <= self.metrics.word_confidence.total_words,
            "native word-confidence coverage exceeds the candidate word count"
        );
        if let Some(ratio) = self.metrics.repetition_ratio {
            ensure!(
                ratio.is_finite() && (0.0..=1.0).contains(&ratio),
                "native repetition ratio must be finite and in [0, 1]"
            );
        }
        Ok(())
    }
}

fn validate_metric_summary(
    summary: &crate::analysis::quality::native_stt::NativeSttMetricSummary,
    label: &str,
    valid_mean: impl Fn(f64) -> bool,
) -> Result<()> {
    ensure!(
        summary.known_segments <= summary.total_segments,
        "{label} coverage exceeds the candidate segment count"
    );
    let complete = summary.total_segments > 0 && summary.known_segments == summary.total_segments;
    ensure!(
        summary.mean.is_some() == complete,
        "{label} mean must be present exactly when coverage is complete"
    );
    if let Some(mean) = summary.mean {
        ensure!(
            valid_mean(mean),
            "{label} mean is non-finite or out of range"
        );
    }
    Ok(())
}

/// One measured native window observation to link to the canonical transcript.
///
/// The exact immutable accounting request supplies provider/model/cost truth.
/// This value adds only measured latency, recognized reported language and the
/// typed quality decision/reason/metric coverage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSttWindowObservation {
    pub request_key: String,
    pub latency_ms: u64,
    pub reported_language: Option<String>,
    pub quality: NativeSttWindowQualityObservation,
}

impl NativeSttWindowObservation {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            !self.request_key.trim().is_empty() && self.request_key.len() <= 512,
            "native window accounting request key is missing or oversized"
        );
        ensure!(
            self.request_key.trim() == self.request_key,
            "native window accounting request key must not contain surrounding whitespace"
        );
        ensure!(
            i64::try_from(self.latency_ms).is_ok(),
            "native window latency does not fit canonical SQLite storage"
        );
        validate_reported_language(&self.reported_language)?;
        self.quality.validate()
    }
}

pub(super) fn validate_reported_language(language: &Option<String>) -> Result<()> {
    let Some(language) = language else {
        return Ok(());
    };
    ensure!(
        language.trim() == language,
        "native window reported language must not contain surrounding whitespace"
    );
    let recognized = recognized_native_stt_reported_language(language).ok_or_else(|| {
        anyhow::anyhow!(
            "native window reported language is not recognized by the existing whisper registry"
        )
    })?;
    ensure!(
        recognized == *language,
        "native window reported language must use its canonical recognized provider spelling"
    );
    Ok(())
}

/// Read-only provenance for one native transcript window. `accounting` reuses
/// the canonical accounting inspection DTOs verbatim; no financial total is
/// independently recomputed here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeTranscriptWindowProvenance {
    pub window_index: u32,
    pub latency_ms: u64,
    pub reported_language: Option<String>,
    pub quality: NativeSttWindowQualityObservation,
    pub accounting: AnalysisAccountingRequestInspection,
    /// Reconstructed only for rows that carry an exact versioned window plan.
    /// Legacy complete native rows without a plan remain `None`: their
    /// candidates are not guessed from overlapping source ranges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<DecodedSegment>>,
}

/// Read-only derived projection over one canonical transcript plus its frozen
/// native policy and immutable accounting history. This is not persisted as a
/// second transcript/wire shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeTranscriptProvenance {
    pub schema_version: String,
    pub asset_id: String,
    pub recipe_key: String,
    pub native_policy_id: String,
    pub native_quality_recipe_version: String,
    pub source_fingerprint: String,
    pub status: TranscriptStatus,
    pub outcome_reason: TranscriptOutcomeReason,
    /// Requested provider/model/language/data policy/source come exclusively
    /// from the frozen canonical authorization row.
    pub policy: RunPolicy,
    pub windows: Vec<NativeTranscriptWindowProvenance>,
    /// Present for an incomplete native checkpoint or a terminal native artifact
    /// carrying a validated versioned plan. Omitted from legacy complete-row wire
    /// output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<NativeTranscriptCoverage>,
}
