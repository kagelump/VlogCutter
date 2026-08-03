//! The hallucination gate for local transcription.
//!
//! Round 1 measured that the signals whisper hands out for free mostly do not
//! work. `no_speech_probability()` had median 0.000 on good and bad transcripts
//! alike — zero separating power — and is deliberately not used here. Language
//! confidence predicts quality but rejects 15 of 29 good chunks when used as a
//! gate. Repetition is the signal that separates: median 0.929 on bad chunks
//! against 0.000 on good ones.
//!
//! The thresholds below were validated against only four positive examples.
//! They are starting constants to revisit against a larger hand-labeled set,
//! not measured truth.

use std::collections::HashSet;

/// Reject a transcript once this fraction of its segments duplicate an earlier
/// segment. Bad chunks measured a median of 0.929; good chunks measured 0.000.
const REPETITION_REJECT_RATIO: f32 = 0.3;

/// Reject a transcript below this mean token probability. The one bad chunk
/// repetition did not catch scored 0.11; the lowest good chunk scored 0.207.
const MIN_MEAN_TOKEN_PROBABILITY: f32 = 0.20;

/// One decoded segment in clip time.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedSegment {
    pub start_ms: u32,
    pub end_ms: u32,
    pub language: String,
    pub text: String,
    pub mean_token_probability: f32,
}

/// Why a transcript was rejected. Recorded on the stored row so a rejection is
/// explainable rather than an unexplained absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    Repetition,
    LowTokenProbability,
}

impl RejectReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            RejectReason::Repetition => "repetition",
            RejectReason::LowTokenProbability => "low_token_probability",
        }
    }
}

/// Collapses a segment to its comparable form.
///
/// Case, surrounding whitespace and trailing punctuation vary between otherwise
/// identical hallucinated repeats, so comparing raw text would miss them. CJK
/// punctuation is included because the measured repetition case was Japanese.
fn normalize(text: &str) -> String {
    text.chars()
        .filter(|character| {
            !character.is_whitespace()
                && !character.is_ascii_punctuation()
                && !matches!(character, '。' | '、' | '，' | '！' | '？' | '：' | '；')
        })
        .flat_map(|character| character.to_lowercase())
        .collect()
}

/// Fraction of segments that duplicate an earlier segment's normalized text.
///
/// Six identical segments score 5/6; all-distinct segments score 0.
pub fn repetition_ratio(segments: &[DecodedSegment]) -> f32 {
    if segments.len() < 2 {
        return 0.0;
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut duplicates = 0_usize;
    for segment in segments {
        let normalized = normalize(&segment.text);
        if normalized.is_empty() {
            continue;
        }
        if !seen.insert(normalized) {
            duplicates += 1;
        }
    }
    duplicates as f32 / segments.len() as f32
}

/// Mean of the per-segment mean token probabilities.
pub fn mean_token_probability(segments: &[DecodedSegment]) -> f32 {
    if segments.is_empty() {
        return 0.0;
    }
    let total: f32 = segments
        .iter()
        .map(|segment| segment.mean_token_probability)
        .sum();
    total / segments.len() as f32
}

/// The measured gate: reject on repetition, or on very low token probability.
///
/// An empty transcript is not a rejection — that is the VAD gate's business,
/// and a clip with no speech is a successful `no_speech` result.
pub fn reject_reason(segments: &[DecodedSegment]) -> Option<RejectReason> {
    if segments.is_empty() {
        return None;
    }
    if repetition_ratio(segments) >= REPETITION_REJECT_RATIO {
        return Some(RejectReason::Repetition);
    }
    if mean_token_probability(segments) < MIN_MEAN_TOKEN_PROBABILITY {
        return Some(RejectReason::LowTokenProbability);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(text: &str, probability: f32) -> DecodedSegment {
        DecodedSegment {
            start_ms: 0,
            end_ms: 1_000,
            language: "ja".to_string(),
            text: text.to_string(),
            mean_token_probability: probability,
        }
    }

    #[test]
    fn distinct_segments_have_no_repetition() {
        let segments = vec![segment("次は新大阪です", 0.8), segment("ありがとう", 0.8)];
        assert_eq!(repetition_ratio(&segments), 0.0);
    }

    #[test]
    fn the_measured_hallucination_shape_scores_high() {
        // Round 1 Finding 2: whisper returned the same sentence six times on a
        // low-speech clip.
        let segments: Vec<DecodedSegment> = (0..6)
            .map(|_| segment("ご飯を食べてみます。", 0.7))
            .collect();
        // Five of six segments duplicate an earlier one.
        assert!((repetition_ratio(&segments) - 5.0 / 6.0).abs() < 1e-6);
    }

    #[test]
    fn repetition_ignores_case_whitespace_and_punctuation() {
        let segments = vec![
            segment("Go straight and turn left.", 0.8),
            segment("  go straight and turn left  ", 0.8),
        ];
        assert!((repetition_ratio(&segments) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn empty_input_is_not_repetitive_and_is_not_rejected() {
        assert_eq!(repetition_ratio(&[]), 0.0);
        assert_eq!(mean_token_probability(&[]), 0.0);
        assert!(reject_reason(&[]).is_none());
    }

    #[test]
    fn mean_token_probability_averages_across_segments() {
        let segments = vec![segment("a", 0.4), segment("b", 0.6)];
        assert!((mean_token_probability(&segments) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_repetitive_transcript_is_rejected() {
        let segments: Vec<DecodedSegment> = (0..6)
            .map(|_| segment("ご飯を食べてみます。", 0.7))
            .collect();
        assert_eq!(reject_reason(&segments), Some(RejectReason::Repetition));
    }

    #[test]
    fn a_low_confidence_transcript_is_rejected() {
        // Round 1 Finding 12: the fourth bad chunk scored 0.11 against a
        // good-chunk minimum of 0.207.
        let segments = vec![segment("garbled", 0.11), segment("nonsense", 0.11)];
        assert_eq!(
            reject_reason(&segments),
            Some(RejectReason::LowTokenProbability)
        );
    }

    #[test]
    fn a_good_transcript_is_not_rejected() {
        let segments = vec![
            segment("お客様、当日券購入希望のお客様は", 0.73),
            segment("正面のチケット引き換え所にお進みください。", 0.73),
        ];
        assert!(reject_reason(&segments).is_none());
    }

    #[test]
    fn a_single_segment_is_never_repetitive() {
        let segments = vec![segment("hello", 0.8)];
        assert_eq!(repetition_ratio(&segments), 0.0);
        assert!(reject_reason(&segments).is_none());
    }
}
