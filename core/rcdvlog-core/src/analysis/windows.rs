//! Packing detected speech into whisper's fixed 30-second decode windows.
//!
//! whisper.cpp bills a full 30-second window per decode regardless of how much
//! audio the window actually contains, so the cost of a clip is its window
//! count, not its duration. Measured head to head, the 8-25 second chunking the
//! PRD originally specified ran 30% slower while processing 39% less audio.
//! Packing non-adjacent speech regions into as few windows as possible is
//! therefore the dominant cost lever after the VAD gate itself.
//!
//! Every packed region records where it came from, because the timestamps
//! whisper returns are relative to the packed window and are meaningless until
//! they are translated back to clip time.

use serde::{Deserialize, Serialize};

/// whisper's fixed decode window.
pub const WINDOW_MS: u32 = 30_000;

/// A span of detected speech in clip time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechRegion {
    pub start_ms: u32,
    pub end_ms: u32,
}

impl SpeechRegion {
    pub fn duration_ms(&self) -> u32 {
        self.end_ms.saturating_sub(self.start_ms)
    }
}

/// A speech region placed inside a packed window, retaining its origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackedRegion {
    /// Offset of this region from the start of the packed window.
    pub window_offset_ms: u32,
    /// Where this audio came from in clip time.
    pub source_start_ms: u32,
    pub duration_ms: u32,
}

/// One 30-second decode unit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackedWindow {
    pub regions: Vec<PackedRegion>,
}

impl PackedWindow {
    /// Total speech packed into this window. Never exceeds `WINDOW_MS`.
    pub fn speech_ms(&self) -> u32 {
        self.regions.iter().map(|region| region.duration_ms).sum()
    }
}

/// Packs speech regions into as few 30-second windows as possible.
///
/// A region that does not fit in the current window starts a new one rather
/// than being cut mid-utterance. A region longer than a whole window is the one
/// case where splitting is unavoidable, and it is split on window boundaries.
pub fn pack_windows(regions: &[SpeechRegion]) -> Vec<PackedWindow> {
    let mut windows: Vec<PackedWindow> = Vec::new();
    let mut current = PackedWindow::default();
    let mut used_ms = 0_u32;

    for region in regions {
        let mut source_start = region.start_ms;
        let mut remaining = region.duration_ms();

        while remaining > 0 {
            let free = WINDOW_MS - used_ms;
            // Start a new window when this region cannot fit whole, unless the
            // current window is empty — in which case the region is longer than
            // a window and must be split.
            if remaining > free && used_ms > 0 {
                windows.push(std::mem::take(&mut current));
                used_ms = 0;
                continue;
            }
            let take = remaining.min(WINDOW_MS - used_ms);
            current.regions.push(PackedRegion {
                window_offset_ms: used_ms,
                source_start_ms: source_start,
                duration_ms: take,
            });
            used_ms += take;
            source_start += take;
            remaining -= take;
            if used_ms == WINDOW_MS {
                windows.push(std::mem::take(&mut current));
                used_ms = 0;
            }
        }
    }

    if !current.regions.is_empty() {
        windows.push(current);
    }
    windows
}

/// Translates an offset in packed-window time back to clip time.
///
/// Returns `None` only for an empty window. An offset past the end of the
/// packed audio is clamped to the end of the last region, because whisper pads
/// short windows to a full 30 seconds and will happily return timestamps inside
/// that padding.
pub fn window_offset_to_source_ms(window: &PackedWindow, offset_ms: u32) -> Option<u32> {
    let last = window.regions.last()?;
    for region in &window.regions {
        let region_end = region.window_offset_ms + region.duration_ms;
        // The half-open interval means an offset exactly on a boundary belongs
        // to the region that starts there, not the one that ends there.
        if offset_ms >= region.window_offset_ms && offset_ms < region_end {
            return Some(region.source_start_ms + (offset_ms - region.window_offset_ms));
        }
    }
    Some(last.source_start_ms + last.duration_ms)
}

/// Translates a window-time span to a clip-time span.
///
/// Each end is translated through its own region, so a span crossing dropped
/// silence widens to cover the real elapsed clip time. The result is ordered.
pub fn translate_span(
    window: &PackedWindow,
    start_offset_ms: u32,
    end_offset_ms: u32,
) -> Option<(u32, u32)> {
    let start = window_offset_to_source_ms(window, start_offset_ms)?;
    let end = window_offset_to_source_ms(window, end_offset_ms)?;
    Some((start.min(end), start.max(end)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(start_ms: u32, end_ms: u32) -> SpeechRegion {
        SpeechRegion { start_ms, end_ms }
    }

    #[test]
    fn no_regions_produces_no_windows() {
        assert!(pack_windows(&[]).is_empty());
    }

    #[test]
    fn short_regions_share_one_window() {
        let windows = pack_windows(&[region(1_000, 4_000), region(9_000, 12_000)]);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].regions.len(), 2);
        assert_eq!(windows[0].regions[0].window_offset_ms, 0);
        assert_eq!(windows[0].regions[0].source_start_ms, 1_000);
        assert_eq!(windows[0].regions[0].duration_ms, 3_000);
        // Silence between the regions is dropped, so the second region starts
        // immediately after the first inside the window.
        assert_eq!(windows[0].regions[1].window_offset_ms, 3_000);
        assert_eq!(windows[0].regions[1].source_start_ms, 9_000);
        assert_eq!(windows[0].speech_ms(), 6_000);
    }

    #[test]
    fn a_region_that_does_not_fit_starts_a_new_window_rather_than_being_cut() {
        // 20 s then 15 s: the second cannot fit in the remaining 10 s.
        let windows = pack_windows(&[region(0, 20_000), region(30_000, 45_000)]);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].regions.len(), 1);
        assert_eq!(windows[1].regions.len(), 1);
        assert_eq!(windows[1].regions[0].window_offset_ms, 0);
        assert_eq!(windows[1].regions[0].source_start_ms, 30_000);
        assert_eq!(windows[1].regions[0].duration_ms, 15_000);
    }

    #[test]
    fn a_region_longer_than_one_window_is_split_across_windows() {
        let windows = pack_windows(&[region(0, 70_000)]);
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].regions[0].duration_ms, 30_000);
        assert_eq!(windows[0].regions[0].source_start_ms, 0);
        assert_eq!(windows[1].regions[0].duration_ms, 30_000);
        assert_eq!(windows[1].regions[0].source_start_ms, 30_000);
        assert_eq!(windows[2].regions[0].duration_ms, 10_000);
        assert_eq!(windows[2].regions[0].source_start_ms, 60_000);
    }

    #[test]
    fn no_window_ever_exceeds_thirty_seconds() {
        let regions: Vec<SpeechRegion> = (0..40)
            .map(|index| region(index * 5_000, index * 5_000 + 4_000))
            .collect();
        for window in pack_windows(&regions) {
            assert!(window.speech_ms() <= WINDOW_MS, "window overflowed");
        }
    }

    #[test]
    fn zero_length_regions_are_dropped() {
        let windows = pack_windows(&[region(1_000, 1_000), region(2_000, 5_000)]);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].regions.len(), 1);
        assert_eq!(windows[0].regions[0].source_start_ms, 2_000);
    }

    #[test]
    fn offset_inside_first_region_maps_to_its_source_time() {
        let windows = pack_windows(&[region(1_000, 4_000), region(9_000, 12_000)]);
        let window = &windows[0];
        assert_eq!(window_offset_to_source_ms(window, 0), Some(1_000));
        assert_eq!(window_offset_to_source_ms(window, 2_500), Some(3_500));
    }

    #[test]
    fn offset_inside_a_later_region_skips_the_dropped_silence() {
        let windows = pack_windows(&[region(1_000, 4_000), region(9_000, 12_000)]);
        let window = &windows[0];
        // Window offset 3000 is the boundary: the first region ended, so this is
        // the start of the second region at clip time 9000, not 4000.
        assert_eq!(window_offset_to_source_ms(window, 3_000), Some(9_000));
        assert_eq!(window_offset_to_source_ms(window, 4_000), Some(10_000));
    }

    #[test]
    fn offset_past_the_packed_content_is_clamped_to_the_end() {
        // whisper pads to a full 30s window, so it can return timestamps beyond
        // the audio that was actually packed.
        let windows = pack_windows(&[region(1_000, 4_000)]);
        let window = &windows[0];
        assert_eq!(window_offset_to_source_ms(window, 25_000), Some(4_000));
    }

    #[test]
    fn empty_window_translates_nothing() {
        let window = PackedWindow::default();
        assert_eq!(window_offset_to_source_ms(&window, 0), None);
    }

    #[test]
    fn a_span_crossing_a_region_boundary_uses_each_ends_own_region() {
        let windows = pack_windows(&[region(1_000, 4_000), region(9_000, 12_000)]);
        let window = &windows[0];
        // 2000..4000 in window time spans the boundary at 3000.
        assert_eq!(translate_span(window, 2_000, 4_000), Some((3_000, 10_000)));
    }

    #[test]
    fn a_span_never_ends_before_it_starts() {
        let windows = pack_windows(&[region(5_000, 8_000)]);
        let window = &windows[0];
        // Offsets arrive reversed; the translated span is still ordered.
        let (start, end) = translate_span(window, 2_000, 1_000).unwrap();
        assert!(start <= end);
        assert_eq!((start, end), (6_000, 7_000));
    }
}
