//! Camera identification from EXIF, then from filename patterns (D01-T028).
//!
//! Resolution order is EXIF Make/Model, then the filename pattern table, then
//! the `"Unknown camera"` literal. EXIF is the camera's own claim about itself
//! and is authoritative when present; a pattern is inference and must never
//! override a fact.
//!
//! Two real defects motivate this module, both found against real footage:
//! `exif_camera` fell back to `"Unknown camera"` for all 420 DJI Pocket 3 clips
//! in the smoke corpus — collapsing every unidentified camera into one capture
//! sequence — and `DJI_20251221092058_0001_D.MP4` has no trailing digit run, so
//! its counter was NULL and it could not be targeted by any counter-ranged
//! correction at all.

pub mod mapping;
pub mod merge;
pub mod patterns;

pub use mapping::{CameraEvidence, CameraMapping};
pub use merge::{CameraMergePreview, CameraMergeReport};
pub use patterns::{CAMERA_PATTERNS, CameraPattern, PatternConfidence};

use regex::Regex;
use std::sync::OnceLock;

/// Where a camera identity came from, so a surface can show inference as
/// inference rather than as fact.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum CameraIdentitySource {
    Exif,
    FilenamePattern {
        pattern_id: String,
        confidence: PatternConfidence,
    },
    Unknown,
}

/// A resolved camera identity.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraIdentity {
    /// Written into `assets.camera`, exactly as the EXIF path writes today.
    pub camera: String,
    pub source: CameraIdentitySource,
}

/// The literal used when nothing identifies the camera. Unchanged from the
/// pre-D01-T028 behaviour so existing catalogs read the same.
pub const UNKNOWN_CAMERA: &str = "Unknown camera";

/// Compiled patterns, built once. The table is `const`, so compilation cannot
/// fail at runtime for any reason a test would not already have caught — an
/// unparseable row is a build-time authoring error and is skipped rather than
/// panicking a user's ingest.
fn compiled() -> &'static [(&'static CameraPattern, Regex)] {
    static COMPILED: OnceLock<Vec<(&'static CameraPattern, Regex)>> = OnceLock::new();
    COMPILED.get_or_init(|| {
        CAMERA_PATTERNS
            .iter()
            .filter_map(|pattern| {
                Regex::new(pattern.stem_pattern)
                    .ok()
                    .map(|compiled| (pattern, compiled))
            })
            .collect()
    })
}

/// The filename stem: extension stripped, matching the D01-T007 rule.
fn stem(filename: &str) -> &str {
    match filename.rfind('.') {
        Some(extension) => &filename[..extension],
        None => filename,
    }
}

/// The first pattern whose anchored expression matches this filename's stem.
fn matching_pattern(filename: &str) -> Option<(&'static CameraPattern, regex::Captures<'_>)> {
    let stem = stem(filename);
    compiled()
        .iter()
        .find_map(|(pattern, expression)| expression.captures(stem).map(|found| (*pattern, found)))
}

/// Resolves a camera identity.
///
/// `exif` is the already-resolved EXIF make and model, `None` when absent. A
/// clip with valid EXIF is identified from EXIF even when its filename also
/// matches a pattern — the pattern is not consulted at all.
pub fn identify_camera(exif: Option<&str>, filename: &str) -> CameraIdentity {
    if let Some(claimed) = exif.map(str::trim).filter(|value| !value.is_empty()) {
        return CameraIdentity {
            camera: claimed.to_string(),
            source: CameraIdentitySource::Exif,
        };
    }
    match matching_pattern(filename) {
        Some((pattern, _)) => CameraIdentity {
            camera: pattern.label.to_string(),
            source: CameraIdentitySource::FilenamePattern {
                pattern_id: pattern.id.to_string(),
                confidence: pattern.confidence,
            },
        },
        None => CameraIdentity {
            camera: UNKNOWN_CAMERA.to_string(),
            source: CameraIdentitySource::Unknown,
        },
    }
}

/// The counter a matching pattern supplies, or `None` when no pattern matches.
///
/// Callers fall back to the D01-T007 trailing-digit rule on `None`. A pattern
/// is consulted *first* because widening the trailing-digit rule instead would
/// pull `DJI_20251221092058_0001_D` down to its 14-digit timestamp, which is
/// not a counter and would order clips by a value that already exists as a
/// time.
pub fn pattern_counter(filename: &str) -> Option<u64> {
    let (pattern, captures) = matching_pattern(filename)?;
    captures
        .get(pattern.counter_group)?
        .as_str()
        .parse::<u64>()
        .ok()
}

impl crate::Catalog {
    /// Records what identification concluded, so inference is visible as
    /// inference (D01-T028). Never fails an ingest: a diagnostic that cannot be
    /// written is strictly less bad than a clip that cannot be imported.
    pub(crate) fn record_camera_identity_diagnostics(
        &self,
        project_id: &str,
        path: &std::path::Path,
        resolved_camera: &str,
    ) {
        let folder = path
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned())
            .unwrap_or_default();
        let filename = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();

        if resolved_camera == UNKNOWN_CAMERA {
            let _ = self.record_unidentified_camera(project_id, &folder, &filename);
            return;
        }
        // A weak match resolved the camera from the filename alone; say so once
        // per pattern so the user can confirm or relabel.
        if let Some((pattern, _)) = matching_pattern(&filename)
            && pattern.confidence == PatternConfidence::Weak
            && pattern.label == resolved_camera
        {
            let _ = self.record_weak_pattern_match(
                project_id,
                pattern.id,
                pattern.label,
                &crate::chronology::capture_sequence_id_for(project_id, resolved_camera),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exif_beats_a_matching_filename_pattern() {
        let identity = identify_camera(Some("Sony ILCE-7C"), "DJI_20251221092058_0001_D.MP4");
        assert_eq!(identity.camera, "Sony ILCE-7C");
        assert_eq!(identity.source, CameraIdentitySource::Exif);
    }

    #[test]
    fn the_three_confirmed_shapes_identify_and_count() {
        let dji = identify_camera(None, "DJI_20251221092058_0001_D.MP4");
        assert_eq!(dji.camera, "DJI Pocket 3");
        assert_eq!(pattern_counter("DJI_20251221092058_0001_D.MP4"), Some(1));

        let insta = identify_camera(None, "VID_20251216_134946_113.mp4");
        assert_eq!(insta.camera, "Insta360 Ultra Go");
        assert_eq!(pattern_counter("VID_20251216_134946_113.mp4"), Some(113));

        let sony = identify_camera(None, "C0684.MP4");
        assert_eq!(sony.camera, "Sony (C-series)");
        assert_eq!(pattern_counter("C0684.MP4"), Some(684));
        assert_eq!(
            sony.source,
            CameraIdentitySource::FilenamePattern {
                pattern_id: "sony-c-series".to_string(),
                confidence: PatternConfidence::Weak,
            }
        );
    }

    #[test]
    fn no_pattern_invents_nothing() {
        let identity = identify_camera(None, "clip.mp4");
        assert_eq!(identity.camera, UNKNOWN_CAMERA);
        assert_eq!(identity.source, CameraIdentitySource::Unknown);
        assert_eq!(pattern_counter("clip.mp4"), None);
        // Blank EXIF is absent EXIF, not a camera named "".
        assert_eq!(
            identify_camera(Some("   "), "clip.mp4").source,
            CameraIdentitySource::Unknown
        );
    }

    #[test]
    fn a_run_of_dji_names_yields_distinct_increasing_counters() {
        let counters: Vec<u64> = (1..=420)
            .map(|n| {
                pattern_counter(&format!("DJI_20251221092058_{n:04}_D.MP4"))
                    .expect("every DJI name must supply a counter")
            })
            .collect();
        assert_eq!(counters.len(), 420);
        assert!(counters.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn every_pattern_in_the_table_compiles_and_has_its_counter_group() {
        assert_eq!(
            compiled().len(),
            CAMERA_PATTERNS.len(),
            "a pattern in the table failed to compile"
        );
        for (pattern, expression) in compiled() {
            assert!(
                expression.captures_len() > pattern.counter_group,
                "{} declares counter group {} but has fewer groups",
                pattern.id,
                pattern.counter_group
            );
        }
    }
}
