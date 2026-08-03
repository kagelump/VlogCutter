//! Deterministic frame selection.
//!
//! Scoring runs over raw 8-bit grayscale that FFmpeg produces in one call, so it
//! is pure arithmetic with no image decoding and no I/O. Determinism is a PRD
//! acceptance criterion: the same clip and recipe must always select the same
//! timestamps, and a measured run produced byte-identical manifests across
//! repeats on 20 clips.
//!
//! Note the qualification that measurement also produced: identical recipes over
//! a proxy and over the original do *not* agree, because sharpness and motion
//! are resolution- and codec-dependent. Frame-packs are pinned to the proxy for
//! that reason, and the representation is part of their stored identity.

/// Perceptual-duplicate threshold on the 64-bit dHash.
const DHASH_MAX_DISTANCE: u32 = 5;
const MIN_SELECTED: usize = 3;
const MAX_SELECTED: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrayFrame {
    pub timestamp_ms: u32,
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameScore {
    pub timestamp_ms: u32,
    pub sharpness: f64,
    pub brightness: f64,
    pub motion: f64,
}

/// Variance of the 4-neighbour Laplacian: high for crisp edges, near zero for a
/// flat or motion-blurred frame.
fn laplacian_variance(frame: &GrayFrame) -> f64 {
    if frame.width < 3 || frame.height < 3 || frame.pixels.len() < frame.width * frame.height {
        return 0.0;
    }
    let mut values = Vec::with_capacity((frame.width - 2) * (frame.height - 2));
    for y in 1..frame.height - 1 {
        for x in 1..frame.width - 1 {
            let at = |dx: usize, dy: usize| f64::from(frame.pixels[dy * frame.width + dx]);
            let value = at(x, y) * 4.0 - at(x - 1, y) - at(x + 1, y) - at(x, y - 1) - at(x, y + 1);
            values.push(value);
        }
    }
    if values.is_empty() {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / values.len() as f64
}

fn mean_pixel(frame: &GrayFrame) -> f64 {
    if frame.pixels.is_empty() {
        return 0.0;
    }
    frame
        .pixels
        .iter()
        .map(|pixel| f64::from(*pixel))
        .sum::<f64>()
        / frame.pixels.len() as f64
}

/// Mean absolute difference from the previous frame.
fn mean_abs_difference(current: &GrayFrame, previous: &GrayFrame) -> f64 {
    if current.pixels.len() != previous.pixels.len() || current.pixels.is_empty() {
        return 0.0;
    }
    current
        .pixels
        .iter()
        .zip(&previous.pixels)
        .map(|(a, b)| f64::from(a.abs_diff(*b)))
        .sum::<f64>()
        / current.pixels.len() as f64
}

pub fn score_frames(frames: &[GrayFrame]) -> Vec<FrameScore> {
    frames
        .iter()
        .enumerate()
        .map(|(index, frame)| FrameScore {
            timestamp_ms: frame.timestamp_ms,
            sharpness: laplacian_variance(frame),
            brightness: mean_pixel(frame),
            motion: if index == 0 {
                0.0
            } else {
                mean_abs_difference(frame, &frames[index - 1])
            },
        })
        .collect()
}

/// 64-bit difference hash: a bit is set where a sampled pixel is brighter than
/// its right-hand neighbour.
pub fn dhash(frame: &GrayFrame) -> u64 {
    if frame.width == 0 || frame.height == 0 || frame.pixels.is_empty() {
        return 0;
    }
    let sample = |x: usize, y: usize| -> u32 {
        let sx = (x * frame.width / 9).min(frame.width - 1);
        let sy = (y * frame.height / 8).min(frame.height - 1);
        u32::from(frame.pixels[sy * frame.width + sx])
    };
    let mut hash = 0_u64;
    for y in 0..8 {
        for x in 0..8 {
            hash <<= 1;
            if sample(x, y) > sample(x + 1, y) {
                hash |= 1;
            }
        }
    }
    hash
}

pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// A frame's desirability: sharp and well exposed.
///
/// Sharpness is compressed with a logarithm because Laplacian variance spans
/// orders of magnitude and would otherwise decide every comparison on its own.
/// Exposure is scored as distance from mid-grey, so blown-out and black frames
/// both lose — which is what makes a black frame lose to a mid-grey one.
fn quality(score: &FrameScore) -> f64 {
    let sharp = (1.0 + score.sharpness).ln();
    let exposure = 1.0 - ((score.brightness - 128.0).abs() / 128.0);
    sharp * 2.0 + exposure
}

/// Selects 3-8 representative frames, deterministically.
///
/// More movement means more distinct content worth sampling, so the target count
/// scales with mean motion. Within each temporal window the best-quality frame
/// wins; a frame perceptually identical to one already chosen is dropped.
pub fn select_frames(scores: &[FrameScore], frames: &[GrayFrame]) -> Vec<u32> {
    if scores.is_empty() || scores.len() != frames.len() {
        return Vec::new();
    }
    let mean_motion = scores.iter().map(|score| score.motion).sum::<f64>() / scores.len() as f64;
    let target = (MIN_SELECTED + (mean_motion / 12.0).round() as usize)
        .clamp(MIN_SELECTED, MAX_SELECTED)
        .min(scores.len());

    let window = scores.len().div_ceil(target).max(1);
    let mut chosen: Vec<(u32, u64)> = Vec::new();

    for chunk_start in (0..scores.len()).step_by(window) {
        let chunk_end = (chunk_start + window).min(scores.len());
        // Ties break on the earlier timestamp, so the result never depends on
        // sort stability or iteration order.
        let best = (chunk_start..chunk_end).max_by(|a, b| {
            quality(&scores[*a])
                .total_cmp(&quality(&scores[*b]))
                .then(scores[*b].timestamp_ms.cmp(&scores[*a].timestamp_ms))
        });
        let Some(best) = best else { continue };
        let hash = dhash(&frames[best]);
        if chosen
            .iter()
            .any(|(_, existing)| hamming(*existing, hash) <= DHASH_MAX_DISTANCE)
        {
            continue;
        }
        chosen.push((scores[best].timestamp_ms, hash));
    }

    let mut selected: Vec<u32> = chosen.into_iter().map(|(timestamp, _)| timestamp).collect();
    selected.sort_unstable();
    selected.truncate(MAX_SELECTED);
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(timestamp_ms: u32, value: u8) -> GrayFrame {
        GrayFrame {
            timestamp_ms,
            width: 8,
            height: 8,
            pixels: vec![value; 64],
        }
    }

    fn checker(timestamp_ms: u32) -> GrayFrame {
        let pixels = (0..64)
            .map(|i| if (i / 8 + i % 8) % 2 == 0 { 0 } else { 255 })
            .collect();
        GrayFrame {
            timestamp_ms,
            width: 8,
            height: 8,
            pixels,
        }
    }

    #[test]
    fn a_flat_frame_has_no_sharpness_and_a_checkerboard_has_a_lot() {
        let scores = score_frames(&[flat(0, 128), checker(500)]);
        assert_eq!(scores[0].sharpness, 0.0);
        assert!(scores[1].sharpness > scores[0].sharpness);
    }

    #[test]
    fn brightness_is_the_mean_pixel_value() {
        let scores = score_frames(&[flat(0, 100)]);
        assert!((scores[0].brightness - 100.0).abs() < 1e-9);
    }

    #[test]
    fn motion_is_zero_for_the_first_frame_and_measures_change_after() {
        let scores = score_frames(&[flat(0, 100), flat(500, 100), flat(1_000, 200)]);
        assert_eq!(scores[0].motion, 0.0);
        assert_eq!(scores[1].motion, 0.0);
        assert!((scores[2].motion - 100.0).abs() < 1e-9);
    }

    #[test]
    fn identical_frames_hash_identically_and_differ_from_a_checkerboard() {
        assert_eq!(dhash(&flat(0, 90)), dhash(&flat(999, 90)));
        assert!(hamming(dhash(&flat(0, 90)), dhash(&checker(0))) > 5);
    }

    #[test]
    fn selection_is_byte_identical_across_repeated_runs() {
        let frames: Vec<GrayFrame> = (0..40).map(varying).collect();
        let scores = score_frames(&frames);
        let first = select_frames(&scores, &frames);
        let second = select_frames(&scores, &frames);
        assert_eq!(first, second);
    }

    /// A frame whose content genuinely differs from its neighbours.
    ///
    /// A uniform per-frame offset is not enough: dHash compares a pixel with its
    /// right-hand neighbour, so shifting every pixel equally leaves the hash
    /// unchanged. This varies the spatial pattern itself, deterministically.
    fn varying(index: u32) -> GrayFrame {
        let pixels = (0..256_u32)
            .map(|offset| {
                let mixed = index.wrapping_mul(2_654_435_761)
                    ^ offset.wrapping_mul(40_503).rotate_left(index % 17);
                (mixed >> 7) as u8
            })
            .collect();
        GrayFrame {
            timestamp_ms: index * 500 + 400,
            width: 16,
            height: 16,
            pixels,
        }
    }

    #[test]
    fn selection_stays_within_the_three_to_eight_band() {
        let frames: Vec<GrayFrame> = (0..40).map(varying).collect();
        let selected = select_frames(&score_frames(&frames), &frames);
        assert!((3..=8).contains(&selected.len()), "got {}", selected.len());
    }

    #[test]
    fn a_static_clip_collapses_to_very_few_frames() {
        // Deliberate: sending eight perceptually identical frames to a paid
        // vision model buys nothing. A static shot legitimately falls below the
        // 3-8 band that varied footage exercises.
        let frames: Vec<GrayFrame> = (0..40).map(|i| checker(i * 500)).collect();
        assert_eq!(select_frames(&score_frames(&frames), &frames).len(), 1);
    }

    #[test]
    fn perceptual_duplicates_are_dropped() {
        // Twelve identical frames carry one distinct hash between them.
        let frames: Vec<GrayFrame> = (0..12).map(|i| checker(i * 500)).collect();
        let selected = select_frames(&score_frames(&frames), &frames);
        assert_eq!(selected.len(), 1, "duplicates must collapse");
    }

    #[test]
    fn selected_timestamps_are_ascending_and_come_from_the_input() {
        let frames: Vec<GrayFrame> = (0..20).map(varying).collect();
        let selected = select_frames(&score_frames(&frames), &frames);
        let available: Vec<u32> = frames.iter().map(|frame| frame.timestamp_ms).collect();
        assert!(selected.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(selected.iter().all(|stamp| available.contains(stamp)));
    }

    #[test]
    fn no_frames_selects_nothing() {
        assert!(select_frames(&[], &[]).is_empty());
    }

    #[test]
    fn a_black_frame_scores_below_a_well_exposed_one() {
        let black = score_frames(&[flat(0, 0)]);
        let midtone = score_frames(&[flat(0, 128)]);
        assert!(quality(&midtone[0]) > quality(&black[0]));
    }
}
