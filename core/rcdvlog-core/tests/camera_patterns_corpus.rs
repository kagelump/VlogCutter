//! Validates the camera pattern table against a *real* footage corpus
//! (D01-T028), rather than against filenames this repository invented.
//!
//! The corpus path is never recorded here. Supply it at invocation:
//!
//! ```sh
//! FOOTAGE_ROOT=<path> cargo test -p rcdvlog-core --test camera_patterns_corpus -- --ignored --nocapture
//! ```
//!
//! Ignored by default so CI, which has no corpus, stays green. Everything it
//! does is read-only: it lists filenames and never opens, moves, or writes a
//! single media file.

use rcdvlog_core::{CameraIdentitySource, PatternConfidence, identify_camera, pattern_counter};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Filenames that are not media and are expected to be skipped: macOS
/// AppleDouble sidecars and directory metadata.
fn is_sidecar(filename: &str) -> bool {
    filename.starts_with("._") || filename == ".DS_Store"
}

#[test]
#[ignore = "requires a real footage corpus at $FOOTAGE_ROOT"]
fn every_clip_in_the_real_corpus_is_identified_and_counted() {
    let Ok(root) = std::env::var("FOOTAGE_ROOT") else {
        panic!("set FOOTAGE_ROOT to the corpus root");
    };
    let root = Path::new(&root);

    // camera label -> (clip count, counters seen)
    let mut by_camera: BTreeMap<String, (usize, Vec<u64>)> = BTreeMap::new();
    let mut unidentified: Vec<String> = Vec::new();
    let mut counterless: Vec<String> = Vec::new();
    let mut weak: Vec<String> = Vec::new();

    let mut folders: Vec<_> = fs::read_dir(root)
        .expect("read corpus root")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.path())
        .collect();
    folders.sort();
    assert!(!folders.is_empty(), "corpus root has no camera folders");

    for folder in folders {
        let mut names: Vec<String> = fs::read_dir(&folder)
            .expect("read camera folder")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| !is_sidecar(name))
            .collect();
        names.sort();

        for name in names {
            // No EXIF is the hard case, and the one the smoke run actually hit:
            // all 420 DJI clips fell through to "Unknown camera".
            let identity = identify_camera(None, &name);
            match &identity.source {
                CameraIdentitySource::Unknown => unidentified.push(name.clone()),
                CameraIdentitySource::FilenamePattern { confidence, .. } => {
                    if *confidence == PatternConfidence::Weak {
                        weak.push(identity.camera.clone());
                    }
                }
                CameraIdentitySource::Exif => {}
            }
            match pattern_counter(&name) {
                Some(counter) => {
                    let entry = by_camera
                        .entry(identity.camera.clone())
                        .or_insert((0, Vec::new()));
                    entry.0 += 1;
                    entry.1.push(counter);
                }
                None => counterless.push(name.clone()),
            }
        }
    }

    for (camera, (count, counters)) in &by_camera {
        let mut sorted = counters.clone();
        sorted.sort_unstable();
        sorted.dedup();
        println!(
            "{camera}: {count} clips, {} distinct counters, range {:?}..={:?}",
            sorted.len(),
            sorted.first(),
            sorted.last()
        );
        assert_eq!(
            sorted.len(),
            *count,
            "{camera}: counters must be distinct within one card's folder"
        );
    }

    assert!(
        unidentified.is_empty(),
        "{} clips matched no pattern, e.g. {:?}",
        unidentified.len(),
        &unidentified[..unidentified.len().min(5)]
    );
    assert!(
        counterless.is_empty(),
        "{} clips got no counter, e.g. {:?}",
        counterless.len(),
        &counterless[..counterless.len().min(5)]
    );
    println!("weak-pattern matches: {}", weak.len());
}
