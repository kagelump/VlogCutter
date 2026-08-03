//! Detecting the silent language-drop that packing makes more likely.
//!
//! Round 1 fed seven code-switched clips through as single windows. In three of
//! the seven, an entire language's speech vanished from the transcript with no
//! error and no low-confidence signal — the `en_ja`, `ja_en_ja` and `en_es`
//! cases. Because a whole utterance disappears, the shortfall against the audio
//! that went into the window is large and easy to detect.
//!
//! The response is to re-decode that window one speech region at a time. The
//! silence boundaries between regions are exactly where a speaker is likely to
//! have switched language, so splitting there targets the failure directly. Only
//! the minority of windows that actually trip the check pay the extra decode.

use crate::analysis::quality::DecodedSegment;
use crate::analysis::windows::{PackedRegion, PackedWindow, WINDOW_MS};

/// Re-split when a window returns less than this fraction of the speech that
/// went into it. Finding 9's failures dropped roughly half a window's content,
/// so this sits comfortably below a healthy decode and above a dropped half.
const MIN_COVERAGE_RATIO: f32 = 0.65;

/// Total distinct time covered by the returned segments.
///
/// Segments are merged before summing, because whisper can return overlapping
/// segments and double-counting would mask a real shortfall.
pub fn transcribed_ms(segments: &[DecodedSegment]) -> u32 {
    if segments.is_empty() {
        return 0;
    }
    let mut spans: Vec<(u32, u32)> = segments
        .iter()
        .map(|segment| {
            (
                segment.start_ms.min(segment.end_ms),
                segment.start_ms.max(segment.end_ms),
            )
        })
        .collect();
    spans.sort_unstable();

    let mut total = 0_u32;
    let (mut current_start, mut current_end) = spans[0];
    for (start, end) in spans.into_iter().skip(1) {
        if start <= current_end {
            current_end = current_end.max(end);
        } else {
            total += current_end - current_start;
            current_start = start;
            current_end = end;
        }
    }
    total + (current_end - current_start)
}

/// Whether this window's decode looks like it dropped a language.
///
/// A window with fewer than two regions has no internal silence boundary to
/// split on, so re-splitting could only repeat the identical decode.
pub fn needs_resplit(window: &PackedWindow, segments: &[DecodedSegment]) -> bool {
    if window.regions.len() < 2 {
        return false;
    }
    let speech_ms = window.speech_ms();
    if speech_ms == 0 {
        return false;
    }
    let covered = transcribed_ms(segments).min(WINDOW_MS);
    (covered as f32) < MIN_COVERAGE_RATIO * speech_ms as f32
}

/// Splits a window into one window per speech region.
///
/// Each region is rebased to offset zero because it becomes its own decode unit.
pub fn resplit(window: &PackedWindow) -> Vec<PackedWindow> {
    window
        .regions
        .iter()
        .map(|region| PackedWindow {
            regions: vec![PackedRegion {
                window_offset_ms: 0,
                source_start_ms: region.source_start_ms,
                duration_ms: region.duration_ms,
            }],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::windows::{SpeechRegion, pack_windows};

    fn segment(start_ms: u32, end_ms: u32) -> DecodedSegment {
        DecodedSegment {
            start_ms,
            end_ms,
            language: "ja".to_string(),
            text: "text".to_string(),
            mean_token_probability: 0.7,
        }
    }

    fn two_region_window() -> PackedWindow {
        // 6s of Japanese then 6s of English, the Finding 9 shape.
        pack_windows(&[
            SpeechRegion {
                start_ms: 0,
                end_ms: 6_000,
            },
            SpeechRegion {
                start_ms: 10_000,
                end_ms: 16_000,
            },
        ])
        .remove(0)
    }

    #[test]
    fn transcribed_duration_sums_segments() {
        let segments = vec![segment(0, 3_000), segment(4_000, 6_000)];
        assert_eq!(transcribed_ms(&segments), 5_000);
    }

    #[test]
    fn overlapping_segments_are_not_double_counted() {
        let segments = vec![segment(0, 4_000), segment(3_000, 6_000)];
        assert_eq!(transcribed_ms(&segments), 6_000);
    }

    #[test]
    fn unordered_segments_are_merged_correctly() {
        let segments = vec![segment(8_000, 10_000), segment(0, 2_000)];
        assert_eq!(transcribed_ms(&segments), 4_000);
    }

    #[test]
    fn full_coverage_does_not_resplit() {
        let window = two_region_window();
        // 12s packed, 11.5s transcribed.
        let segments = vec![segment(0, 5_500), segment(6_000, 12_000)];
        assert!(!needs_resplit(&window, &segments));
    }

    #[test]
    fn a_dropped_language_half_triggers_resplit() {
        let window = two_region_window();
        // Only the Japanese half came back: 6s of 12s.
        let segments = vec![segment(0, 6_000)];
        assert!(needs_resplit(&window, &segments));
    }

    #[test]
    fn a_single_region_window_never_resplits() {
        // There is no internal silence boundary to split on, so re-splitting
        // would just repeat the same decode.
        let window = pack_windows(&[SpeechRegion {
            start_ms: 0,
            end_ms: 20_000,
        }])
        .remove(0);
        assert!(!needs_resplit(&window, &[]));
    }

    #[test]
    fn an_empty_window_never_resplits() {
        assert!(!needs_resplit(&PackedWindow::default(), &[]));
    }

    #[test]
    fn resplit_produces_one_window_per_region_rebased_to_zero() {
        let window = two_region_window();
        let split = resplit(&window);
        assert_eq!(split.len(), 2);
        assert_eq!(split[0].regions.len(), 1);
        assert_eq!(split[0].regions[0].window_offset_ms, 0);
        assert_eq!(split[0].regions[0].source_start_ms, 0);
        assert_eq!(split[1].regions[0].window_offset_ms, 0);
        assert_eq!(split[1].regions[0].source_start_ms, 10_000);
        assert_eq!(split[1].regions[0].duration_ms, 6_000);
    }
}
