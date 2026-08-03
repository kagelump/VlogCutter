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

/// whisper's fixed decode window.
pub const WINDOW_MS: u32 = 30_000;

/// A span of detected speech in clip time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackedRegion {
    /// Offset of this region from the start of the packed window.
    pub window_offset_ms: u32,
    /// Where this audio came from in clip time.
    pub source_start_ms: u32,
    pub duration_ms: u32,
}

/// One 30-second decode unit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
}
