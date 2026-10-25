use super::*;
use crate::analysis::openrouter_stt::{NativeSttSegment, NativeSttTranscript, NativeSttWord};
use crate::analysis::windows::{SpeechRegion, pack_windows};

fn audio_for_speech_ms(duration_ms: u32) -> NativeSttAudioWindow {
    audio_for_regions(&[(0, duration_ms)])
}

fn audio_for_regions(regions: &[(u32, u32)]) -> NativeSttAudioWindow {
    let source_end_ms = regions.iter().map(|(_, end_ms)| *end_ms).max().unwrap_or(0);
    let pcm = vec![0.25_f32; source_end_ms as usize * 16];
    let speech_regions: Vec<SpeechRegion> = regions
        .iter()
        .map(|(start_ms, end_ms)| SpeechRegion {
            start_ms: *start_ms,
            end_ms: *end_ms,
        })
        .collect();
    let window = pack_windows(&speech_regions).remove(0);
    NativeSttAudioWindow::from_pcm(&pcm, &window).unwrap()
}

fn segment(start_ms: u32, end_ms: u32, text: &str) -> NativeSttSegment {
    NativeSttSegment {
        start_ms,
        end_ms,
        text: text.to_string(),
        confidence: None,
        avg_logprob: None,
        words: Vec::new(),
    }
}

fn with_logprob(mut segment: NativeSttSegment, avg_logprob: f64) -> NativeSttSegment {
    segment.avg_logprob = Some(avg_logprob);
    segment
}

fn with_confidence(mut segment: NativeSttSegment, confidence: f64) -> NativeSttSegment {
    segment.confidence = Some(confidence);
    segment
}

fn transcript(
    language: &str,
    duration_ms: u32,
    segments: Vec<NativeSttSegment>,
) -> NativeSttTranscript {
    NativeSttTranscript {
        text: segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        language: language.to_string(),
        confidence: None,
        duration_ms,
        segments,
    }
}

fn assert_finite_metrics(metrics: &NativeSttQualityMetrics) {
    for value in [
        metrics.avg_logprob.mean,
        metrics.segment_confidence.mean,
        metrics.transcript_confidence,
    ]
    .into_iter()
    .flatten()
    {
        assert!(value.is_finite(), "public metric leaked: {value:?}");
    }
    if let Some(repetition_ratio) = metrics.repetition_ratio {
        assert!(repetition_ratio.is_finite());
    }
}

#[test]
fn complete_avg_logprob_evidence_accepts_and_maps_candidates() {
    let audio = audio_for_speech_ms(2_000);
    let transcript = transcript(
        "en",
        2_000,
        vec![
            with_logprob(segment(0, 1_000, "hello"), -0.2),
            with_logprob(segment(1_000, 2_000, "world"), -0.4),
        ],
    );

    let result = evaluate_native_stt_quality(&transcript, &audio);

    assert!(result.is_accepted());
    assert_eq!(
        result.reason,
        NativeSttQualityReason::AcceptedCompleteEvidence
    );
    assert_eq!(result.reason_code(), "accepted_complete_evidence");
    assert_eq!(result.recipe_version, NATIVE_STT_QUALITY_RECIPE_VERSION);
    assert_eq!(result.language.as_deref(), Some("en"));
    assert_eq!(result.reported_language.as_deref(), Some("en"));
    assert_eq!(result.metrics.avg_logprob.known_segments, 2);
    assert_eq!(result.metrics.avg_logprob.total_segments, 2);
    assert!((result.metrics.avg_logprob.mean.unwrap() + 0.3).abs() < 1e-9);
    assert_eq!(result.metrics.segment_confidence.known_segments, 0);
    assert_eq!(result.metrics.transcript_confidence, None);
    assert_eq!(result.metrics.repetition_ratio, Some(0.0));
    assert_eq!(result.segments.len(), 2);
    assert_eq!(
        (result.segments[0].start_ms, result.segments[0].end_ms),
        (0, 1_000)
    );
    assert_eq!(
        (result.segments[1].start_ms, result.segments[1].end_ms),
        (1_000, 2_000)
    );
    assert!(
        result
            .segments
            .iter()
            .all(|segment| segment.mean_token_probability.is_none())
    );
    assert_finite_metrics(&result.metrics);
}

#[test]
fn low_avg_logprob_rejects_and_discards_candidate_text() {
    let audio = audio_for_speech_ms(1_000);
    let transcript = transcript(
        "en",
        1_000,
        vec![with_logprob(segment(0, 1_000, "garbled canary text"), -2.0)],
    );

    let result = evaluate_native_stt_quality(&transcript, &audio);

    assert!(result.is_rejected());
    assert_eq!(
        result.reason,
        NativeSttQualityReason::LowAverageLogProbability
    );
    assert!(result.segments.is_empty());
    assert!(!format!("{result:?}").contains("garbled canary text"));
    assert_finite_metrics(&result.metrics);
}

#[test]
fn segment_confidence_is_an_independent_complete_metric() {
    let audio = audio_for_speech_ms(2_000);
    let accepted = evaluate_native_stt_quality(
        &transcript(
            "en",
            2_000,
            vec![
                with_confidence(segment(0, 1_000, "hello"), 0.91),
                with_confidence(segment(1_000, 2_000, "world"), 0.81),
            ],
        ),
        &audio,
    );
    assert!(accepted.is_accepted());
    assert_eq!(accepted.metrics.avg_logprob.mean, None);
    assert!(accepted.metrics.segment_confidence.mean.is_some());
    assert!(
        accepted
            .segments
            .iter()
            .all(|segment| segment.mean_token_probability.is_none())
    );

    let rejected = evaluate_native_stt_quality(
        &transcript(
            "en",
            2_000,
            vec![
                with_confidence(segment(0, 1_000, "hello"), 0.10),
                with_confidence(segment(1_000, 2_000, "world"), 0.05),
            ],
        ),
        &audio,
    );
    assert!(rejected.is_rejected());
    assert_eq!(
        rejected.reason,
        NativeSttQualityReason::LowProviderConfidence
    );
    assert!(rejected.segments.is_empty());
}

#[test]
fn complete_avg_logprob_with_absent_confidence_is_accepted() {
    let audio = audio_for_speech_ms(1_000);
    let transcript = transcript(
        "en",
        1_000,
        vec![with_logprob(segment(0, 1_000, "hello"), -0.1)],
    );

    let result = evaluate_native_stt_quality(&transcript, &audio);

    assert!(result.is_accepted());
    assert_eq!(result.metrics.segment_confidence.known_segments, 0);
    assert_eq!(result.metrics.avg_logprob.mean, Some(-0.1));
}

#[test]
fn high_confidence_never_fills_mean_token_probability() {
    let audio = audio_for_speech_ms(1_000);
    let transcript = transcript(
        "en",
        1_000,
        vec![with_confidence(segment(0, 1_000, "hello"), 0.999_999)],
    );

    let result = evaluate_native_stt_quality(&transcript, &audio);

    assert!(result.is_accepted());
    assert_eq!(result.metrics.avg_logprob.known_segments, 0);
    assert_eq!(result.metrics.avg_logprob.mean, None);
    assert_eq!(result.segments[0].mean_token_probability, None);
}

#[test]
fn absent_and_partial_comparable_evidence_requires_review_with_candidates() {
    let audio = audio_for_speech_ms(2_000);
    let absent = evaluate_native_stt_quality(
        &transcript(
            "en",
            2_000,
            vec![
                segment(0, 1_000, "first draft"),
                segment(1_000, 2_000, "second draft"),
            ],
        ),
        &audio,
    );
    assert!(absent.is_review_required());
    assert_eq!(
        absent.reason,
        NativeSttQualityReason::ReviewMissingQualityEvidence
    );
    assert_eq!(absent.segments.len(), 2);
    assert_eq!(absent.segments[0].text, "first draft");
    assert!(
        absent
            .segments
            .iter()
            .all(|segment| segment.mean_token_probability.is_none())
    );

    let partial = evaluate_native_stt_quality(
        &transcript(
            "en",
            2_000,
            vec![
                with_logprob(segment(0, 1_000, "first draft"), -0.2),
                segment(1_000, 2_000, "second draft"),
            ],
        ),
        &audio,
    );
    assert!(partial.is_review_required());
    assert_eq!(
        partial.reason,
        NativeSttQualityReason::ReviewIncompleteQualityEvidence
    );
    assert_eq!(partial.metrics.avg_logprob.known_segments, 1);
    assert_eq!(partial.metrics.avg_logprob.mean, None);
    assert_eq!(partial.segments.len(), 2);
}

#[test]
fn partial_known_low_avg_logprob_still_rejects() {
    let audio = audio_for_speech_ms(2_000);
    let transcript = transcript(
        "en",
        2_000,
        vec![
            with_logprob(segment(0, 1_000, "bad canary"), -2.0),
            segment(1_000, 2_000, "missing evidence"),
        ],
    );

    let result = evaluate_native_stt_quality(&transcript, &audio);

    assert!(result.is_rejected());
    assert_eq!(
        result.reason,
        NativeSttQualityReason::LowAverageLogProbability
    );
    assert!(result.segments.is_empty());
    assert_eq!(result.metrics.avg_logprob.known_segments, 1);
    assert_eq!(result.metrics.avg_logprob.total_segments, 2);
    assert_eq!(result.metrics.avg_logprob.mean, None);
}

#[test]
fn a_passing_metric_does_not_hide_a_known_low_alternate_metric() {
    let audio = audio_for_speech_ms(2_000);
    let transcript = transcript(
        "en",
        2_000,
        vec![
            // Average log-probability is complete and passes, while the single
            // reported segment confidence is known-low and partial.
            with_logprob(with_confidence(segment(0, 1_000, "hello"), 0.05), -0.1),
            with_logprob(segment(1_000, 2_000, "world"), -0.1),
        ],
    );

    let result = evaluate_native_stt_quality(&transcript, &audio);

    assert!(result.is_rejected());
    assert_eq!(result.reason, NativeSttQualityReason::LowProviderConfidence);
    assert!(result.segments.is_empty());
}

#[test]
fn full_transcript_confidence_is_complete_evidence_and_can_reject_independently() {
    let audio = audio_for_speech_ms(2_000);
    let mut accepted = transcript(
        "en",
        2_000,
        vec![segment(0, 1_000, "hello"), segment(1_000, 2_000, "world")],
    );
    accepted.confidence = Some(0.9);
    let accepted = evaluate_native_stt_quality(&accepted, &audio);
    assert!(accepted.is_accepted());
    assert_eq!(accepted.metrics.transcript_confidence, Some(0.9));

    let mut rejected = transcript(
        "en",
        2_000,
        vec![
            with_logprob(with_confidence(segment(0, 1_000, "hello"), 0.99), -0.01),
            with_logprob(with_confidence(segment(1_000, 2_000, "world"), 0.99), -0.01),
        ],
    );
    rejected.confidence = Some(0.1);
    let rejected = evaluate_native_stt_quality(&rejected, &audio);
    assert!(rejected.is_rejected());
    assert_eq!(
        rejected.reason,
        NativeSttQualityReason::LowProviderConfidence
    );
    assert!(rejected.segments.is_empty());
}

#[test]
fn repetition_rejects_even_when_all_quality_metrics_are_missing() {
    let audio = audio_for_speech_ms(2_000);
    let transcript = transcript(
        "en",
        2_000,
        vec![
            segment(0, 1_000, "same line!"),
            segment(1_000, 2_000, "SAME LINE"),
        ],
    );

    let result = evaluate_native_stt_quality(&transcript, &audio);

    assert!(result.is_rejected());
    assert_eq!(result.reason, NativeSttQualityReason::Repetition);
    assert!(result.segments.is_empty());
    assert_eq!(result.metrics.repetition_ratio, Some(0.5));
}

#[test]
fn repetition_at_the_existing_threshold_rejects() {
    let audio = audio_for_speech_ms(10_000);
    let texts = ["a", "b", "c", "d", "e", "f", "g", "a", "b", "c"];
    let segments = texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            with_logprob(
                segment(index as u32 * 1_000, (index as u32 + 1) * 1_000, text),
                -0.1,
            )
        })
        .collect();
    let transcript = transcript("en", 10_000, segments);

    let result = evaluate_native_stt_quality(&transcript, &audio);

    assert!(result.is_rejected());
    assert_eq!(result.reason, NativeSttQualityReason::Repetition);
    assert!((result.metrics.repetition_ratio.unwrap() - 0.3).abs() < 1e-6);
}

#[test]
fn invalid_quality_evidence_rejects_without_candidate_text_or_nonfinite_metrics() {
    let audio = audio_for_speech_ms(1_000);
    let invalid_confidences = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.01, 1.01];
    for invalid in invalid_confidences {
        let transcript = transcript(
            "en",
            1_000,
            vec![with_confidence(
                segment(0, 1_000, "secret candidate canary"),
                invalid,
            )],
        );
        let result = evaluate_native_stt_quality(&transcript, &audio);
        assert!(result.is_rejected(), "confidence {invalid:?}");
        assert_eq!(
            result.reason,
            NativeSttQualityReason::InvalidQualityEvidence,
            "confidence {invalid:?}"
        );
        assert!(result.segments.is_empty());
        assert_finite_metrics(&result.metrics);
        assert!(!format!("{result:?}").contains("secret candidate canary"));
    }

    let invalid_logprobs = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.01];
    for invalid in invalid_logprobs {
        let transcript = transcript(
            "en",
            1_000,
            vec![with_logprob(
                segment(0, 1_000, "secret candidate canary"),
                invalid,
            )],
        );
        let result = evaluate_native_stt_quality(&transcript, &audio);
        assert!(result.is_rejected(), "logprob {invalid:?}");
        assert_eq!(
            result.reason,
            NativeSttQualityReason::InvalidQualityEvidence,
            "logprob {invalid:?}"
        );
        assert!(result.segments.is_empty());
        assert_finite_metrics(&result.metrics);
    }

    let mut invalid_full = transcript("en", 1_000, vec![segment(0, 1_000, "hello")]);
    invalid_full.confidence = Some(f64::NAN);
    let result = evaluate_native_stt_quality(&invalid_full, &audio);
    assert_eq!(
        result.reason,
        NativeSttQualityReason::InvalidQualityEvidence
    );
    assert_finite_metrics(&result.metrics);

    let mut word = segment(0, 1_000, "hello");
    word.words.push(NativeSttWord {
        text: "hello".to_string(),
        start_ms: None,
        end_ms: None,
        confidence: Some(f64::INFINITY),
    });
    let result = evaluate_native_stt_quality(&transcript("en", 1_000, vec![word]), &audio);
    assert_eq!(
        result.reason,
        NativeSttQualityReason::InvalidQualityEvidence
    );
    assert!(result.segments.is_empty());
    assert_finite_metrics(&result.metrics);
}

#[test]
fn extreme_finite_logprobs_never_overflow_public_aggregates() {
    let audio = audio_for_speech_ms(2_000);
    let transcript = transcript(
        "en",
        2_000,
        vec![
            with_logprob(segment(0, 1_000, "first"), -f64::MAX),
            with_logprob(segment(1_000, 2_000, "second"), -f64::MAX),
        ],
    );

    let result = evaluate_native_stt_quality(&transcript, &audio);

    assert!(result.is_rejected());
    assert_eq!(
        result.reason,
        NativeSttQualityReason::LowAverageLogProbability
    );
    let mean = result.metrics.avg_logprob.mean.unwrap();
    assert!(mean.is_finite());
    assert_eq!(mean, -f64::MAX);
    assert_finite_metrics(&result.metrics);
}

#[test]
fn extreme_logprob_rounding_cannot_hide_low_evidence_behind_passing_confidence() {
    let audio = audio_for_speech_ms(3_000);
    let mut transcript = transcript(
        "en",
        3_000,
        vec![
            with_logprob(segment(0, 1_000, "first"), -f64::MAX),
            with_logprob(segment(1_000, 2_000, "second"), -f64::MAX),
            with_logprob(segment(2_000, 3_000, "third"), -f64::MAX),
        ],
    );
    transcript.confidence = Some(0.9);

    let result = evaluate_native_stt_quality(&transcript, &audio);

    assert_eq!(result.decision, NativeSttQualityDecision::Rejected);
    assert_eq!(
        result.reason,
        NativeSttQualityReason::LowAverageLogProbability
    );
    assert_eq!(result.metrics.avg_logprob.mean, Some(-f64::MAX));
    assert_eq!(result.metrics.transcript_confidence, Some(0.9));
    assert!(result.segments.is_empty());
    assert_finite_metrics(&result.metrics);
}

#[test]
fn empty_or_malformed_provider_output_is_never_local_silence() {
    let audio = audio_for_speech_ms(1_000);
    let mut empty = transcript("en", 1_000, Vec::new());
    empty.text = "provider canary text".to_string();
    let result = evaluate_native_stt_quality(&empty, &audio);
    assert!(result.is_rejected());
    assert_eq!(result.reason, NativeSttQualityReason::EmptyProviderOutput);
    assert_eq!(result.reason_code(), "empty_provider_output");
    assert!(result.segments.is_empty());
    assert!(!format!("{result:?}").contains("provider canary text"));

    let mut blank_text = transcript("en", 1_000, vec![segment(0, 1_000, "hello")]);
    blank_text.text = "   ".to_string();
    assert_eq!(
        evaluate_native_stt_quality(&blank_text, &audio).reason,
        NativeSttQualityReason::InvalidText
    );

    let blank_segment = transcript("en", 1_000, vec![segment(0, 1_000, "  ")]);
    assert_eq!(
        evaluate_native_stt_quality(&blank_segment, &audio).reason,
        NativeSttQualityReason::InvalidText
    );

    let too_long = transcript(
        "en",
        1_001,
        vec![with_logprob(segment(0, 1_000, "hello"), -0.1)],
    );
    assert_eq!(
        evaluate_native_stt_quality(&too_long, &audio).reason,
        NativeSttQualityReason::InvalidDuration
    );

    let out_of_window = transcript(
        "en",
        1_000,
        vec![with_logprob(segment(0, 1_001, "hello"), -0.1)],
    );
    assert_eq!(
        evaluate_native_stt_quality(&out_of_window, &audio).reason,
        NativeSttQualityReason::InvalidTimestamp
    );
}

#[test]
fn source_offsets_and_separated_regions_map_without_clamping() {
    let audio = audio_for_regions(&[(1_000, 4_000)]);
    let offset = evaluate_native_stt_quality(
        &transcript(
            "en",
            3_000,
            vec![with_logprob(segment(0, 1_000, "hello"), -0.1)],
        ),
        &audio,
    );
    assert!(offset.is_accepted());
    assert_eq!(
        (offset.segments[0].start_ms, offset.segments[0].end_ms),
        (1_000, 2_000)
    );

    let audio = audio_for_regions(&[(1_000, 4_000), (9_000, 12_000)]);
    let separated = evaluate_native_stt_quality(
        &transcript(
            "en",
            6_000,
            vec![
                with_logprob(segment(0, 1_000, "first region"), -0.1),
                with_logprob(segment(3_000, 4_000, "second region"), -0.1),
            ],
        ),
        &audio,
    );
    assert!(separated.is_accepted());
    assert_eq!(
        (separated.segments[0].start_ms, separated.segments[0].end_ms),
        (1_000, 2_000)
    );
    assert_eq!(
        (separated.segments[1].start_ms, separated.segments[1].end_ms),
        (9_000, 10_000)
    );
}

#[test]
fn packed_gap_boundary_and_cross_gap_spans_preserve_existing_semantics() {
    let audio = audio_for_regions(&[(1_000, 4_000), (9_000, 12_000)]);

    // Offset 3000 is exactly the half-open boundary between packed regions.
    // Existing mapping semantics assign it to the region that starts there.
    let boundary = evaluate_native_stt_quality(
        &transcript(
            "en",
            6_000,
            vec![with_logprob(segment(0, 3_000, "ends at boundary"), -0.1)],
        ),
        &audio,
    );
    assert!(boundary.is_accepted());
    assert_eq!(
        (boundary.segments[0].start_ms, boundary.segments[0].end_ms),
        (1_000, 9_000)
    );

    // A span crossing the dropped silence widens to cover real elapsed clip time.
    let cross_gap = evaluate_native_stt_quality(
        &transcript(
            "en",
            6_000,
            vec![with_logprob(segment(2_000, 4_000, "crosses gap"), -0.1)],
        ),
        &audio,
    );
    assert!(cross_gap.is_accepted());
    assert_eq!(
        (cross_gap.segments[0].start_ms, cross_gap.segments[0].end_ms),
        (3_000, 10_000)
    );
}

#[test]
fn reversed_overlapping_and_out_of_window_timestamps_reject_without_text() {
    let audio = audio_for_speech_ms(2_000);

    let reversed = transcript(
        "en",
        2_000,
        vec![with_logprob(segment(1_000, 500, "reversed canary"), -0.1)],
    );
    let result = evaluate_native_stt_quality(&reversed, &audio);
    assert_eq!(result.reason, NativeSttQualityReason::InvalidTimestamp);
    assert!(result.segments.is_empty());

    let overlapping = transcript(
        "en",
        2_000,
        vec![
            with_logprob(segment(0, 1_200, "first canary"), -0.1),
            with_logprob(segment(1_000, 2_000, "second canary"), -0.1),
        ],
    );
    let result = evaluate_native_stt_quality(&overlapping, &audio);
    assert_eq!(result.reason, NativeSttQualityReason::InvalidTimestamp);
    assert!(result.segments.is_empty());

    let beyond_actual_window = transcript(
        "en",
        1_000,
        vec![with_logprob(segment(900, 1_001, "beyond canary"), -0.1)],
    );
    let short_audio = audio_for_speech_ms(1_000);
    let result = evaluate_native_stt_quality(&beyond_actual_window, &short_audio);
    assert_eq!(result.reason, NativeSttQualityReason::InvalidTimestamp);
    assert!(result.segments.is_empty());
}

#[test]
fn known_language_codes_and_full_names_are_normalized_through_the_registry() {
    let audio = audio_for_speech_ms(1_000);
    for (reported, expected_code) in [
        ("en", "en"),
        ("EN", "en"),
        ("English", "en"),
        ("english", "en"),
        ("japanese", "ja"),
    ] {
        let result = evaluate_native_stt_quality(
            &transcript(
                reported,
                1_000,
                vec![with_logprob(segment(0, 1_000, "hello"), -0.1)],
            ),
            &audio,
        );
        assert!(result.is_accepted(), "language {reported}");
        assert_eq!(result.language.as_deref(), Some(expected_code));
        assert_eq!(result.reported_language.as_deref(), Some(reported));
        assert_eq!(result.segments[0].language, expected_code);
    }
}

#[test]
fn unknown_language_rejection_never_leaks_the_raw_language_or_body() {
    let audio = audio_for_speech_ms(1_000);
    let transcript = transcript(
        "zz-CANARY-DO-NOT-LEAK",
        1_000,
        vec![with_logprob(
            segment(0, 1_000, "body canary do not leak"),
            -0.1,
        )],
    );

    let result = evaluate_native_stt_quality(&transcript, &audio);

    assert!(result.is_rejected());
    assert_eq!(result.reason, NativeSttQualityReason::UnknownLanguage);
    assert_eq!(result.reason_code(), "unknown_language");
    assert_eq!(result.language, None);
    assert_eq!(result.reported_language, None);
    assert!(result.segments.is_empty());
    let debug = format!("{result:?}");
    assert!(!debug.contains("CANARY"));
    assert!(!debug.contains("body canary"));
}

#[test]
fn word_confidence_is_validated_but_not_used_as_segment_or_token_evidence() {
    let audio = audio_for_speech_ms(1_000);
    let mut word_only = segment(0, 1_000, "hello");
    word_only.words.push(NativeSttWord {
        text: "hello".to_string(),
        start_ms: Some(0),
        end_ms: Some(500),
        confidence: Some(0.01),
    });
    let result = evaluate_native_stt_quality(&transcript("en", 1_000, vec![word_only]), &audio);
    assert!(result.is_review_required());
    assert_eq!(
        result.reason,
        NativeSttQualityReason::ReviewMissingQualityEvidence
    );
    assert_eq!(
        result.metrics.word_confidence,
        NativeSttWordConfidenceCoverage {
            known_words: 1,
            total_words: 1,
        }
    );
    assert_eq!(result.metrics.avg_logprob.mean, None);
    assert_eq!(result.metrics.transcript_confidence, None);
    assert_eq!(result.segments[0].mean_token_probability, None);
}

#[test]
fn exact_threshold_values_are_not_rejected_as_low() {
    let audio = audio_for_speech_ms(1_000);
    let logprob_boundary = evaluate_native_stt_quality(
        &transcript(
            "en",
            1_000,
            vec![with_logprob(
                segment(0, 1_000, "hello"),
                NATIVE_STT_AVG_LOGPROB_LN_0_20_REJECT_THRESHOLD,
            )],
        ),
        &audio,
    );
    assert!(logprob_boundary.is_accepted());

    let confidence_boundary = evaluate_native_stt_quality(
        &transcript(
            "en",
            1_000,
            vec![with_confidence(
                segment(0, 1_000, "hello"),
                NATIVE_STT_PROVIDER_CONFIDENCE_REJECT_THRESHOLD,
            )],
        ),
        &audio,
    );
    assert!(confidence_boundary.is_accepted());
}
