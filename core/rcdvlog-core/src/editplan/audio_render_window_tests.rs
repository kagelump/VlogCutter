use super::{AudioMixWindow, render_mix, render_mix_window};
use crate::editplan::audio::AudioEvent;
use std::{fs, path::Path, process::Command};
use tempfile::tempdir;

fn tone(path: &Path, hz: u32) {
    let output = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg(format!("sine=frequency={hz}:sample_rate=48000:duration=5"))
        .args(["-c:a", "pcm_s16le", "-ar", "48000", "-ac", "1"])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn event(
    source: &Path,
    source_in: u64,
    source_out: u64,
    timeline: u64,
    fade_in: u64,
    fade_out: u64,
) -> AudioEvent {
    AudioEvent {
        source: source.to_string_lossy().into_owned(),
        source_in_samples: source_in,
        source_out_samples: source_out,
        timeline_start_samples: timeline,
        gain_db: 0.0,
        fade_in_samples: fade_in,
        fade_out_samples: fade_out,
    }
}

#[test]
fn crop_matches_full_mix_slice_and_skips_nonoverlapping_sources() {
    let dir = tempdir().unwrap();
    let source_a = dir.path().join("picture-a.wav");
    let source_b = dir.path().join("outside-picture-jl-tail.wav");
    tone(&source_a, 440);
    tone(&source_b, 660);
    let rate = 48_000_u64;
    let mut events = vec![
        // Spans the excerpt end, so the crop must preserve its original release fade.
        event(&source_a, 0, 4 * rate, rate / 2, rate / 4, rate / 2),
        // J/L-style source audio begins outside the selected picture neighborhood;
        // the window starts inside its attack fade.
        event(
            &source_b,
            rate / 5,
            7 * rate / 5,
            11 * rate / 5,
            rate / 5,
            rate / 5,
        ),
        // Must be discarded by interval intersection before probing/opening its path.
        event(
            &dir.path().join("absent-outside-window.wav"),
            0,
            rate,
            10 * rate,
            0,
            0,
        ),
    ];
    events.shrink_to_fit();
    let full_dir = dir.path().join("full");
    let crop_dir = dir.path().join("crop");
    fs::create_dir(&full_dir).unwrap();
    fs::create_dir(&crop_dir).unwrap();
    let movie_end = 9 * rate / 2;
    let full = fs::read(
        render_mix(&full_dir, &events, movie_end as f64 / rate as f64, &|| {
            false
        })
        .unwrap(),
    )
    .unwrap();
    let window = AudioMixWindow {
        in_samples: 23 * rate / 10,
        out_samples: 43 * rate / 10,
    };
    let cropped =
        fs::read(render_mix_window(&crop_dir, &events, window, &|| false).unwrap()).unwrap();
    let start = (window.in_samples * 8) as usize;
    let end = (window.out_samples * 8) as usize;
    assert_eq!(cropped, full[start..end]);
}
