use super::types::{NativeSttWindowQualityObservation, validate_reported_language};
use crate::analysis::quality::native_stt::{
    NativeSttMetricSummary, NativeSttQualityDecision, NativeSttQualityMetrics,
    NativeSttQualityReason, NativeSttWordConfidenceCoverage,
};

fn metrics(repetition_ratio: Option<f32>) -> NativeSttQualityMetrics {
    NativeSttQualityMetrics {
        avg_logprob: NativeSttMetricSummary {
            known_segments: 1,
            total_segments: 1,
            mean: Some(-0.1),
        },
        segment_confidence: NativeSttMetricSummary {
            known_segments: 0,
            total_segments: 1,
            mean: None,
        },
        transcript_confidence: None,
        word_confidence: NativeSttWordConfidenceCoverage {
            known_words: 0,
            total_words: 1,
        },
        repetition_ratio,
    }
}

#[test]
fn native_quality_observation_rejects_nonfinite_and_incomplete_acceptance() {
    let mut observation = NativeSttWindowQualityObservation {
        decision: NativeSttQualityDecision::Accepted,
        reason: NativeSttQualityReason::AcceptedCompleteEvidence,
        metrics: metrics(Some(0.0)),
    };
    assert!(observation.validate().is_ok());

    observation.metrics.repetition_ratio = Some(0.5);
    assert!(observation.validate().is_err());

    observation.metrics.repetition_ratio = Some(0.0);
    observation.metrics.avg_logprob.mean = Some(f64::NAN);
    assert!(observation.validate().is_err());

    observation.metrics.avg_logprob.mean = Some(-0.1);
    observation.metrics.avg_logprob.known_segments = 0;
    observation.metrics.avg_logprob.mean = None;
    assert!(
        observation.validate().is_err(),
        "accepted observation must retain complete passing evidence"
    );
}

#[test]
fn recognized_reported_language_rejects_unknown_canaries() {
    assert!(validate_reported_language(&None).is_ok());
    assert!(validate_reported_language(&Some("en".into())).is_ok());
    assert!(validate_reported_language(&Some("English".into())).is_ok());
    assert!(validate_reported_language(&Some("PRIVATE_UNKNOWN_CANARY".into())).is_err());
    assert!(validate_reported_language(&Some(" en ".into())).is_err());
}

use super::super::{StoredTranscript, TranscriptOutcomeReason, TranscriptStatus};
use super::types::NativeSttWindowObservation;
use super::validation::{validate_segment_source_bounds, validate_transcript_shape};
use crate::analysis::quality::DecodedSegment;

fn observation(
    request_key: &str,
    quality: NativeSttWindowQualityObservation,
) -> NativeSttWindowObservation {
    NativeSttWindowObservation {
        request_key: request_key.into(),
        latency_ms: 10,
        reported_language: Some("en".into()),
        quality,
    }
}

fn accepted_quality() -> NativeSttWindowQualityObservation {
    NativeSttWindowQualityObservation {
        decision: NativeSttQualityDecision::Accepted,
        reason: NativeSttQualityReason::AcceptedCompleteEvidence,
        metrics: metrics(Some(0.0)),
    }
}

fn review_quality() -> NativeSttWindowQualityObservation {
    NativeSttWindowQualityObservation {
        decision: NativeSttQualityDecision::ReviewRequired,
        reason: NativeSttQualityReason::ReviewMissingQualityEvidence,
        metrics: NativeSttQualityMetrics {
            avg_logprob: NativeSttMetricSummary {
                known_segments: 0,
                total_segments: 1,
                mean: None,
            },
            segment_confidence: NativeSttMetricSummary {
                known_segments: 0,
                total_segments: 1,
                mean: None,
            },
            transcript_confidence: None,
            word_confidence: NativeSttWordConfidenceCoverage {
                known_words: 0,
                total_words: 1,
            },
            repetition_ratio: Some(0.0),
        },
    }
}

fn rejected_quality() -> NativeSttWindowQualityObservation {
    NativeSttWindowQualityObservation {
        decision: NativeSttQualityDecision::Rejected,
        reason: NativeSttQualityReason::LowProviderConfidence,
        metrics: metrics(Some(0.0)),
    }
}

fn transcript(
    status: TranscriptStatus,
    outcome_reason: TranscriptOutcomeReason,
) -> StoredTranscript {
    let segments = if matches!(
        status,
        TranscriptStatus::Speech | TranscriptStatus::ReviewRequired
    ) {
        vec![DecodedSegment {
            start_ms: 0,
            end_ms: 500,
            language: "en".into(),
            text: "synthetic native candidate".into(),
            mean_token_probability: None,
        }]
    } else {
        Vec::new()
    };
    StoredTranscript {
        asset_id: "a1".into(),
        recipe_key: "native.recipe".into(),
        source_fingerprint: "sampled-fp".into(),
        status,
        outcome_reason,
        reject_reason: String::new(),
        primary_language: "en".into(),
        speech_seconds: if segments.is_empty() { 0.0 } else { 0.5 },
        window_count: 2,
        provider_segment_count: 2,
        mean_token_probability: None,
        repetition_ratio: Some(0.0),
        segments,
    }
}

#[test]
fn artifact_status_cannot_hide_unknown_or_rejected_window_quality() {
    let speech = transcript(TranscriptStatus::Speech, TranscriptOutcomeReason::Accepted);
    let error = validate_transcript_shape(
        &speech,
        &[
            observation("request-1", accepted_quality()),
            observation("request-2", review_quality()),
        ],
    )
    .unwrap_err();
    assert!(error.to_string().contains("every provider window"));

    let review = transcript(
        TranscriptStatus::ReviewRequired,
        TranscriptOutcomeReason::QualityEvidenceIncomplete,
    );
    let error = validate_transcript_shape(
        &review,
        &[
            observation("request-1", review_quality()),
            observation("request-2", rejected_quality()),
        ],
    )
    .unwrap_err();
    assert!(error.to_string().contains("cannot hide a known-bad"));

    let rejected = transcript(
        TranscriptStatus::Rejected,
        TranscriptOutcomeReason::QualityRejected,
    );
    let mut rejected_with_text = rejected.clone();
    rejected_with_text.window_count = 1;
    rejected_with_text.segments = speech.segments.clone();
    let error = validate_transcript_shape(
        &rejected_with_text,
        &[observation("request-1", rejected_quality())],
    )
    .unwrap_err();
    assert!(error.to_string().contains("must not retain candidate text"));

    let error = validate_transcript_shape(&speech, &[observation("request-1", accepted_quality())])
        .unwrap_err();
    assert!(error.to_string().contains("window count"));
}

#[test]
fn accepted_observation_requires_a_passing_metric_and_source_timestamps_are_bounded() {
    let mut invalid = accepted_quality();
    invalid.metrics.avg_logprob.mean = Some(-2.0);
    assert!(
        invalid.validate().is_err(),
        "low complete evidence must reject"
    );

    let mut alternate_passing = accepted_quality();
    alternate_passing.metrics.avg_logprob.mean = Some(-2.0);
    alternate_passing.metrics.transcript_confidence = Some(0.9);
    assert!(
        alternate_passing.validate().is_err(),
        "a passing alternate metric cannot hide a low complete metric"
    );

    let transcript = transcript(TranscriptStatus::Speech, TranscriptOutcomeReason::Accepted);
    assert!(validate_segment_source_bounds(&transcript, 500).is_ok());
    assert!(validate_segment_source_bounds(&transcript, 499).is_err());
}

#[test]
fn empty_provider_outcome_cannot_hide_quality_rejection_or_review() {
    let mut stored = transcript(
        TranscriptStatus::NoAcceptedSpeech,
        TranscriptOutcomeReason::EmptyProviderOutput,
    );
    stored.window_count = 1;
    stored.segments.clear();
    let mut bad = rejected_quality();
    bad.reason = NativeSttQualityReason::LowAverageLogProbability;
    assert!(validate_transcript_shape(&stored, &[observation("bad", bad)]).is_err());
    assert!(
        validate_transcript_shape(&stored, &[observation("review", review_quality())]).is_err()
    );
    let mut empty = rejected_quality();
    empty.reason = NativeSttQualityReason::EmptyProviderOutput;
    empty.metrics.avg_logprob = NativeSttMetricSummary {
        known_segments: 0,
        total_segments: 0,
        mean: None,
    };
    empty.metrics.segment_confidence = NativeSttMetricSummary {
        known_segments: 0,
        total_segments: 0,
        mean: None,
    };
    empty.metrics.word_confidence = NativeSttWordConfidenceCoverage {
        known_words: 0,
        total_words: 0,
    };
    assert!(validate_transcript_shape(&stored, &[observation("empty", empty)]).is_ok());
}

#[test]
fn historical_quality_inspection_preserves_recorded_decision_without_current_thresholds() {
    let mut historical = accepted_quality();
    historical.metrics.avg_logprob.mean = Some(-2.0);
    assert!(
        historical.encode_json().is_err(),
        "current writes still reject low quality"
    );
    let encoded = serde_json::to_string(&historical).unwrap();
    assert!(
        NativeSttWindowQualityObservation::decode_json(
            &encoded,
            crate::analysis::quality::native_stt::NATIVE_STT_QUALITY_RECIPE_VERSION,
        )
        .is_err()
    );
    let decoded = NativeSttWindowQualityObservation::decode_json(
        &encoded,
        "native-stt.quality.historical.v0",
    )
    .unwrap();
    assert_eq!(
        decoded, historical,
        "inspection must preserve facts under the recorded recipe"
    );
    let mut malformed: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    malformed["metrics"]["avgLogprob"]["mean"] = serde_json::Value::Null;
    assert!(
        NativeSttWindowQualityObservation::decode_json(
            &malformed.to_string(),
            "native-stt.quality.historical.v0",
        )
        .is_err(),
        "historical observations still require structurally valid finite evidence"
    );
}
