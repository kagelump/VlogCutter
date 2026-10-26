use super::*;
use crate::analysis::quality::DecodedSegment;
use crate::analysis::quality::native_stt::{
    NativeSttMetricSummary, NativeSttQualityDecision, NativeSttQualityMetrics,
    NativeSttQualityReason, NativeSttWordConfidenceCoverage,
};
use crate::analysis::run_policy::RunAssetScope;
use crate::analysis::store::NATIVE_TRANSCRIPT_WINDOW_PLAN_SCHEMA_VERSION;
use crate::analysis::store::native_provenance::types::{
    NativeSttWindowObservation, NativeSttWindowQualityObservation,
};
use crate::analysis::windows::{PackedRegion, PackedWindow};
use crate::transcription::NativeSpeechPreparationEvidence;

fn evidence() -> NativeSpeechPreparationEvidence {
    NativeSpeechPreparationEvidence {
        policy_id: "policy-native".into(),
        project_id: "project".into(),
        asset_id: "asset".into(),
        provider: "openrouter".into(),
        model: "openai/whisper-large-v3".into(),
        recipe: "native-recipe".into(),
        scope: RunAssetScope {
            asset_id: "asset".into(),
            source_fingerprint: "source-fingerprint".into(),
            source_duration_ms: 2_000,
            representation: crate::analysis::framepack::REPRESENTATION.into(),
            representation_fingerprint: Some("representation".into()),
            sampled_representation_fingerprint: Some("sampled".into()),
            transcript_fingerprint: None,
            time_mapping: "identity".into(),
            source_identity: "source-identity".into(),
        },
        proxy_full_fingerprint: "proxy-full".into(),
        proxy_sampled_fingerprint: "sampled".into(),
        source_duration_ms: 2_000,
        last_safe_sample_ms: 2_000,
        vad: None,
    }
}

fn plan() -> NativeTranscriptWindowPlan {
    NativeTranscriptWindowPlan {
        schema_version: NATIVE_TRANSCRIPT_WINDOW_PLAN_SCHEMA_VERSION.into(),
        windows: vec![
            PackedWindow {
                regions: vec![PackedRegion {
                    window_offset_ms: 0,
                    source_start_ms: 0,
                    duration_ms: 500,
                }],
            },
            PackedWindow {
                regions: vec![PackedRegion {
                    window_offset_ms: 0,
                    source_start_ms: 1_000,
                    duration_ms: 500,
                }],
            },
        ],
    }
}

fn metrics(total_segments: usize, mean: Option<f64>) -> NativeSttQualityMetrics {
    NativeSttQualityMetrics {
        avg_logprob: NativeSttMetricSummary {
            known_segments: mean.map_or(0, |_| total_segments),
            total_segments,
            mean,
        },
        segment_confidence: NativeSttMetricSummary {
            known_segments: 0,
            total_segments,
            mean: None,
        },
        transcript_confidence: None,
        word_confidence: NativeSttWordConfidenceCoverage {
            known_words: 0,
            total_words: total_segments,
        },
        repetition_ratio: Some(0.0),
    }
}

fn quality(
    decision: NativeSttQualityDecision,
    reason: NativeSttQualityReason,
    metrics: NativeSttQualityMetrics,
) -> NativeSttWindowQualityObservation {
    NativeSttWindowQualityObservation {
        decision,
        reason,
        metrics,
    }
}

fn accepted_quality(total_segments: usize) -> NativeSttWindowQualityObservation {
    quality(
        NativeSttQualityDecision::Accepted,
        NativeSttQualityReason::AcceptedCompleteEvidence,
        metrics(total_segments, Some(-0.1)),
    )
}

fn review_quality(total_segments: usize) -> NativeSttWindowQualityObservation {
    quality(
        NativeSttQualityDecision::ReviewRequired,
        NativeSttQualityReason::ReviewMissingQualityEvidence,
        metrics(total_segments, None),
    )
}

fn rejected_quality(reason: NativeSttQualityReason) -> NativeSttWindowQualityObservation {
    match reason {
        NativeSttQualityReason::EmptyProviderOutput => {
            quality(NativeSttQualityDecision::Rejected, reason, metrics(0, None))
        }
        _ => quality(
            NativeSttQualityDecision::Rejected,
            reason,
            metrics(1, Some(-2.0)),
        ),
    }
}

fn candidate(start_ms: u32, end_ms: u32, language: &str, text: &str) -> DecodedSegment {
    DecodedSegment {
        start_ms,
        end_ms,
        language: language.into(),
        text: text.into(),
        mean_token_probability: None,
    }
}

fn window(
    index: u32,
    request_key: &str,
    reported_language: &str,
    quality: NativeSttWindowQualityObservation,
    candidates: Vec<DecodedSegment>,
) -> NativeTranscriptCheckpointWindow {
    NativeTranscriptCheckpointWindow {
        window_index: index,
        observation: NativeSttWindowObservation {
            request_key: request_key.into(),
            latency_ms: 10 + u64::from(index),
            reported_language: Some(reported_language.into()),
            quality,
        },
        candidates,
    }
}

fn accepted_two_windows() -> Vec<NativeTranscriptCheckpointWindow> {
    vec![
        window(
            0,
            "request-0",
            "english",
            accepted_quality(1),
            vec![candidate(0, 500, "en", "first window")],
        ),
        window(
            1,
            "request-1",
            "english",
            accepted_quality(2),
            vec![candidate(1_000, 1_500, "en", "second window")],
        ),
    ]
}

#[test]
fn accepted_two_window_completion_preserves_provider_metrics_without_token_quality() {
    let transcript =
        assemble_native_transcript_completion(&evidence(), &plan(), &accepted_two_windows())
            .unwrap();
    assert_eq!(transcript.status, TranscriptStatus::Speech);
    assert_eq!(transcript.outcome_reason, TranscriptOutcomeReason::Accepted);
    assert_eq!(transcript.window_count, 2);
    assert_eq!(transcript.provider_segment_count, 3);
    assert_eq!(transcript.speech_seconds, 1.0);
    assert_eq!(transcript.primary_language, "en");
    assert_eq!(transcript.repetition_ratio, Some(0.0));
    assert_eq!(transcript.mean_token_probability, None);
    assert_eq!(transcript.segments.len(), 2);
    assert!(
        transcript
            .segments
            .iter()
            .all(|segment| segment.mean_token_probability.is_none())
    );
}

#[test]
fn per_window_safe_repetition_across_windows_is_rejected_without_mutating_observations() {
    let windows = vec![
        window(
            0,
            "request-0",
            "english",
            accepted_quality(1),
            vec![candidate(0, 500, "en", "same words")],
        ),
        window(
            1,
            "request-1",
            "english",
            accepted_quality(1),
            vec![candidate(1_000, 1_500, "en", "same words")],
        ),
    ];
    let transcript = assemble_native_transcript_completion(&evidence(), &plan(), &windows).unwrap();
    assert_eq!(transcript.status, TranscriptStatus::Rejected);
    assert_eq!(
        transcript.outcome_reason,
        TranscriptOutcomeReason::QualityRejected
    );
    assert_eq!(transcript.reject_reason, "repetition");
    assert_eq!(transcript.repetition_ratio, Some(0.5));
    assert!(transcript.segments.is_empty());
    assert_eq!(transcript.provider_segment_count, 2);
    assert!(windows.iter().all(|window| {
        window.observation.quality.decision == NativeSttQualityDecision::Accepted
            && window.candidates.len() == 1
    }));
}

#[test]
fn review_required_window_retains_candidates_only_as_unaccepted_review_output() {
    let windows = vec![
        window(
            0,
            "request-0",
            "english",
            accepted_quality(1),
            vec![candidate(0, 500, "en", "accepted window")],
        ),
        window(
            1,
            "request-1",
            "english",
            review_quality(1),
            vec![candidate(1_000, 1_500, "en", "review window")],
        ),
    ];
    let transcript = assemble_native_transcript_completion(&evidence(), &plan(), &windows).unwrap();
    assert_eq!(transcript.status, TranscriptStatus::ReviewRequired);
    assert_eq!(
        transcript.outcome_reason,
        TranscriptOutcomeReason::QualityEvidenceIncomplete
    );
    assert_eq!(transcript.segments.len(), 2);
    assert_eq!(transcript.repetition_ratio, Some(0.0));
}

#[test]
fn known_low_window_rejects_and_discards_even_good_window_text() {
    let windows = vec![
        window(
            0,
            "request-0",
            "english",
            accepted_quality(1),
            vec![candidate(0, 500, "en", "would otherwise be good")],
        ),
        window(
            1,
            "request-1",
            "english",
            rejected_quality(NativeSttQualityReason::LowAverageLogProbability),
            Vec::new(),
        ),
    ];
    let transcript = assemble_native_transcript_completion(&evidence(), &plan(), &windows).unwrap();
    assert_eq!(transcript.status, TranscriptStatus::Rejected);
    assert_eq!(transcript.reject_reason, "low_average_log_probability");
    assert!(transcript.segments.is_empty());
    assert_eq!(transcript.primary_language, "");
    assert_eq!(transcript.provider_segment_count, 2);
}

#[test]
fn all_empty_and_mixed_empty_outcomes_remain_distinct_and_never_silence() {
    let all_empty = vec![
        window(
            0,
            "request-0",
            "english",
            rejected_quality(NativeSttQualityReason::EmptyProviderOutput),
            Vec::new(),
        ),
        window(
            1,
            "request-1",
            "english",
            rejected_quality(NativeSttQualityReason::EmptyProviderOutput),
            Vec::new(),
        ),
    ];
    let transcript =
        assemble_native_transcript_completion(&evidence(), &plan(), &all_empty).unwrap();
    assert_eq!(transcript.status, TranscriptStatus::NoAcceptedSpeech);
    assert_eq!(
        transcript.outcome_reason,
        TranscriptOutcomeReason::EmptyProviderOutput
    );
    assert!(transcript.segments.is_empty());

    let mixed = vec![
        window(
            0,
            "request-0",
            "english",
            accepted_quality(1),
            vec![candidate(0, 500, "en", "accepted speech")],
        ),
        window(
            1,
            "request-1",
            "english",
            rejected_quality(NativeSttQualityReason::EmptyProviderOutput),
            Vec::new(),
        ),
    ];
    let transcript = assemble_native_transcript_completion(&evidence(), &plan(), &mixed).unwrap();
    assert_eq!(transcript.status, TranscriptStatus::Rejected);
    assert_eq!(
        transcript.outcome_reason,
        TranscriptOutcomeReason::QualityRejected
    );
    assert_eq!(transcript.reject_reason, "mixed_empty_provider_output");
    assert!(transcript.segments.is_empty());
}

#[test]
fn language_dominance_uses_existing_duration_then_language_tie_semantics() {
    let windows = vec![
        window(
            0,
            "request-0",
            "english",
            accepted_quality(1),
            vec![candidate(0, 500, "en", "english text")],
        ),
        window(
            1,
            "request-1",
            "spanish",
            accepted_quality(1),
            vec![candidate(1_000, 1_500, "es", "texto español")],
        ),
    ];
    let transcript = assemble_native_transcript_completion(&evidence(), &plan(), &windows).unwrap();
    assert_eq!(transcript.status, TranscriptStatus::Speech);
    assert_eq!(transcript.primary_language, "es");
}

#[test]
fn malformed_indexes_duplicates_and_unmapped_candidates_fail_closed() {
    let mut gap = accepted_two_windows();
    gap[1].window_index = 2;
    assert!(assemble_native_transcript_completion(&evidence(), &plan(), &gap).is_err());

    let mut duplicate = accepted_two_windows();
    duplicate[1].window_index = 0;
    assert!(assemble_native_transcript_completion(&evidence(), &plan(), &duplicate).is_err());

    let mut malformed = accepted_two_windows();
    malformed[0].candidates[0].end_ms = 1_000;
    assert!(assemble_native_transcript_completion(&evidence(), &plan(), &malformed).is_err());

    let mut rejected_candidate = accepted_two_windows();
    rejected_candidate[0].observation.quality =
        rejected_quality(NativeSttQualityReason::LowAverageLogProbability);
    assert!(
        assemble_native_transcript_completion(&evidence(), &plan(), &rejected_candidate).is_err()
    );
}

#[test]
fn complete_native_assembly_cannot_invent_a_missing_source_fingerprint() {
    let mut preparation = evidence();
    preparation.scope.sampled_representation_fingerprint = None;
    let error =
        assemble_native_transcript_completion(&preparation, &plan(), &accepted_two_windows())
            .unwrap_err();
    assert!(
        error.to_string().contains("source fingerprint is missing"),
        "{error}"
    );
}
