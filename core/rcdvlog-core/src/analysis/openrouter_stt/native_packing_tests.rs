//! Focused strict source-mapping regressions for the native WAV constructor.

use super::*;
use crate::analysis::windows::{PackedRegion, PackedWindow, translate_span};

fn pcm_for_ms(duration_ms: u32) -> Vec<f32> {
    vec![0.25_f32; duration_ms as usize * 16]
}

fn region(window_offset_ms: u32, source_start_ms: u32, duration_ms: u32) -> PackedRegion {
    PackedRegion {
        window_offset_ms,
        source_start_ms,
        duration_ms,
    }
}

#[test]
fn native_window_constructor_rejects_reversed_and_overlapping_source_regions() {
    let pcm = pcm_for_ms(2_000);
    let reversed = PackedWindow {
        regions: vec![region(0, 1_000, 10), region(10, 500, 10)],
    };
    let error = NativeSttAudioWindow::from_pcm(&pcm, &reversed).unwrap_err();
    assert!(
        format!("{error:#}").contains("overlap or are out of order"),
        "{error:#}"
    );

    let overlapping = PackedWindow {
        regions: vec![region(0, 1_000, 10), region(10, 1_005, 10)],
    };
    let error = NativeSttAudioWindow::from_pcm(&pcm, &overlapping).unwrap_err();
    assert!(
        format!("{error:#}").contains("overlap or are out of order"),
        "{error:#}"
    );
}

#[test]
fn native_window_constructor_preserves_a_gapped_speech_source_mapping() {
    let pcm = pcm_for_ms(1_000);
    let window = PackedWindow {
        regions: vec![region(0, 100, 10), region(10, 500, 20)],
    };
    let audio = NativeSttAudioWindow::from_pcm(&pcm, &window).unwrap();

    assert_eq!(audio.window(), &window);
    assert_eq!(audio.duration_ms(), 30);
    assert_eq!(audio.sample_count(), 30 * 16);
    assert_eq!(translate_span(audio.window(), 0, 30), Some((100, 520)));
}
