//! Deterministic OpenTimelineIO export (D01-T022).
//!
//! A pure-Rust writer. No Python, no `otiotool`, no shelling out to the
//! reference implementation — `PRD.md` §21.3 forbids a permanent Python
//! dependency, and an export path is not sufficient reason to breach it. The
//! accepted cost is that this repository owns schema fidelity, which is exactly
//! why D01-T024's real Resolve import gate is required rather than optional.
//!
//! Determinism is a hard requirement, and it is structural rather than
//! hoped-for:
//!
//! - JSON is written by hand with a fixed key order. No map type reaches
//!   serialization, so no iteration order can leak into the file.
//! - Clip identifiers are a function of the revision id and the operation's
//!   position, so re-exporting produces the same clips rather than new ones in
//!   a finishing editor.
//! - Time is rational — a frame count and a rate — never a float duration. A
//!   float that round-trips differently across platforms is a determinism
//!   hazard.
//! - Nothing environmental appears in the output: no timestamp, hostname,
//!   username, temporary path, process id, or random value.
//!
//! Source references are the *originals*. A finishing editor conforms against
//! camera masters; the preview is what uses proxies.

use super::model::EditPlanRevision;
use crate::{Asset, Catalog};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::Path;
use thiserror::Error;

/// Rate used when an asset reports no usable frame rate of its own. Refusing
/// outright would be worse: a clip with unknown metadata is common, and 24fps
/// is a defensible, documented, deterministic choice that a finishing editor
/// will conform anyway.
const FALLBACK_FRAME_RATE: f64 = 24.0;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OtioExportRequest {
    pub revision_id: String,
    pub output_path: String,
    /// The digest the caller expects to overwrite. `None` means the file must
    /// not exist.
    pub expected_existing_digest: Option<String>,
    pub timeline_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OtioExportResult {
    pub output_path: String,
    pub output_digest: String,
    pub clip_count: u32,
}

#[derive(Debug, Error)]
pub enum OtioExportError {
    #[error("OTIO export does not yet support independent or silent source audio")]
    UnsupportedAudio,
    #[error("output {path} already exists with digest {actual}")]
    OutputExists { path: String, actual: String },
    #[error("asset {asset_id} has no usable frame rate")]
    UnusableFrameRate { asset_id: String },
}

impl OtioExportError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedAudio => "otio_unsupported_audio",
            Self::OutputExists { .. } => "otio_output_exists",
            Self::UnusableFrameRate { .. } => "otio_unusable_frame_rate",
        }
    }
}

/// Converts seconds to a whole frame count at a rate.
///
/// Rounding rule, chosen once and tested: round half away from zero, then
/// clamp to zero. Truncation would drop a frame that is 0.999 of the way
/// through, and banker's rounding would make neighbouring cuts inconsistent in
/// a way an editor notices.
fn seconds_to_frames(seconds: f64, rate: f64) -> i64 {
    if !seconds.is_finite() || !rate.is_finite() || rate <= 0.0 {
        return 0;
    }
    let frames = (seconds * rate).round();
    if frames <= 0.0 { 0 } else { frames as i64 }
}

/// The frame rate to express a clip's times in.
fn effective_rate(asset: &Asset) -> f64 {
    if asset.frame_rate.is_finite() && asset.frame_rate > 0.0 {
        asset.frame_rate
    } else {
        FALLBACK_FRAME_RATE
    }
}

/// A rational time as OTIO expresses it. Written by hand so key order is fixed.
fn rational_time(value: i64, rate: f64) -> Value {
    serde_json::json!({
        "OTIO_SCHEMA": "RationalTime.1",
        "rate": rate,
        "value": value as f64,
    })
}

fn time_range(start: i64, duration: i64, rate: f64) -> Value {
    serde_json::json!({
        "OTIO_SCHEMA": "TimeRange.1",
        "duration": rational_time(duration, rate),
        "start_time": rational_time(start, rate),
    })
}

/// A stable clip identifier: a function of the revision and the position only,
/// so re-export yields the same clips instead of new ones.
fn clip_identifier(revision_id: &str, position: u32) -> String {
    format!("{revision_id}:{position:05}")
}

/// A `file://` URL for a source path. Only the characters that actually break a
/// URL are escaped, and the mapping is fixed, so the same path always produces
/// the same target.
fn file_url(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len() + 8);
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    format!("file://{encoded}")
}

/// Serializes a plan to a byte-stable OTIO document.
///
/// Pure: no clock, no environment, no filesystem. Separated from `export_otio`
/// precisely so determinism is testable without touching a disk.
///
/// One video track, hard cuts only. No transition, no effect, no gap, no
/// filler — the model cannot express them and this writer must not invent them.
pub fn otio_document(
    plan: &EditPlanRevision,
    assets: &[Asset],
    timeline_name: &str,
) -> Result<String> {
    if plan.operations.iter().any(|cut| cut.audio.is_some()) {
        return Err(OtioExportError::UnsupportedAudio.into());
    }
    let mut clips: Vec<Value> = Vec::with_capacity(plan.operations.len());
    // The timeline rate is the first clip's rate, so a single-camera timeline
    // carries its camera's rate rather than an invented global one.
    let mut timeline_rate = FALLBACK_FRAME_RATE;

    for (index, operation) in plan.operations.iter().enumerate() {
        let asset = assets
            .iter()
            .find(|candidate| candidate.id == operation.asset_id)
            .ok_or(OtioExportError::UnusableFrameRate {
                asset_id: operation.asset_id.clone(),
            })?;
        let rate = effective_rate(asset);
        if index == 0 {
            timeline_rate = rate;
        }
        let start = seconds_to_frames(operation.source_in_seconds, rate);
        let end = seconds_to_frames(operation.source_out_seconds, rate);
        let duration = (end - start).max(0);

        clips.push(serde_json::json!({
            "OTIO_SCHEMA": "Clip.1",
            "media_reference": {
                "OTIO_SCHEMA": "ExternalReference.1",
                "available_range": time_range(0, seconds_to_frames(asset.duration_seconds, rate), rate),
                "metadata": {},
                "name": asset.filename,
                // The original, never the proxy: a finishing editor conforms
                // against camera masters.
                "target_url": file_url(&asset.path),
            },
            "metadata": {
                "rcdvlog": {
                    "assetId": operation.asset_id,
                    "clipId": clip_identifier(&plan.revision_id, operation.position),
                    "position": operation.position,
                    "rationale": operation.rationale,
                    "revisionId": plan.revision_id,
                }
            },
            "name": asset.filename,
            "source_range": time_range(start, duration, rate),
        }));
    }

    let document = serde_json::json!({
        "OTIO_SCHEMA": "Timeline.1",
        "global_start_time": rational_time(0, timeline_rate),
        "metadata": {
            "rcdvlog": {
                "projectId": plan.project_id,
                "revisionId": plan.revision_id,
            }
        },
        "name": timeline_name,
        "tracks": {
            "OTIO_SCHEMA": "Stack.1",
            "children": [{
                "OTIO_SCHEMA": "Track.1",
                "children": clips,
                "kind": "Video",
                "metadata": {},
                "name": "V1",
                "source_range": null,
            }],
            "metadata": {},
            "name": "tracks",
            "source_range": null,
        },
    });

    // `serde_json` preserves insertion order for `json!` objects (its default
    // map is order-preserving), so the key order written above is the key order
    // on disk. A trailing newline keeps the file POSIX-clean.
    Ok(format!("{}\n", serde_json::to_string_pretty(&document)?))
}

impl Catalog {
    /// Writes a plan to an OTIO file.
    ///
    /// An existing output is never silently replaced: the caller must supply
    /// the digest it expects to overwrite, and a mismatch refuses the write
    /// with the digest actually found.
    pub fn export_otio(&self, request: &OtioExportRequest) -> Result<OtioExportResult> {
        let plan = self.edit_plan_revision(&request.revision_id)?;
        let assets = self.list_assets(&plan.project_id)?;
        let document = otio_document(&plan, &assets, &request.timeline_name)?;
        let digest = blake3::hash(document.as_bytes()).to_hex().to_string();

        let output = Path::new(&request.output_path);
        if output.exists() {
            let existing = blake3::hash(&fs::read(output)?).to_hex().to_string();
            match request.expected_existing_digest.as_deref() {
                Some(expected) if expected == existing => {}
                _ => {
                    return Err(OtioExportError::OutputExists {
                        path: request.output_path.clone(),
                        actual: existing,
                    }
                    .into());
                }
            }
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(output, document.as_bytes())?;

        let result = OtioExportResult {
            output_path: request.output_path.clone(),
            output_digest: digest.clone(),
            clip_count: plan.operations.len() as u32,
        };
        self.record_edit_plan_export(&super::model::EditPlanExport {
            export_id: String::new(),
            revision_id: request.revision_id.clone(),
            output_path: request.output_path.clone(),
            output_digest: digest,
            exported_at: String::new(),
        })?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_is_half_away_from_zero_and_clamped_at_zero() {
        assert_eq!(seconds_to_frames(1.0, 30.0), 30);
        assert_eq!(seconds_to_frames(0.5, 30.0), 15);
        // 0.4999 of a frame rounds down, 0.5 rounds up.
        assert_eq!(seconds_to_frames(1.0 / 30.0 * 0.4999, 30.0), 0);
        assert_eq!(seconds_to_frames(1.0 / 30.0 * 0.5, 30.0), 1);
        assert_eq!(seconds_to_frames(-5.0, 30.0), 0);
        assert_eq!(seconds_to_frames(f64::NAN, 30.0), 0);
        assert_eq!(seconds_to_frames(1.0, 0.0), 0);
    }

    #[test]
    fn clip_identifiers_depend_only_on_revision_and_position() {
        assert_eq!(clip_identifier("editrev_abc", 7), "editrev_abc:00007");
        assert_eq!(
            clip_identifier("editrev_abc", 7),
            clip_identifier("editrev_abc", 7)
        );
        assert_ne!(
            clip_identifier("editrev_abc", 7),
            clip_identifier("editrev_abd", 7)
        );
    }

    #[test]
    fn file_urls_escape_only_what_breaks_a_url() {
        assert_eq!(
            file_url("/Volumes/Card One/DCIM/C0001.MP4"),
            "file:///Volumes/Card%20One/DCIM/C0001.MP4"
        );
        // Deterministic for non-ASCII rather than lossy.
        assert_eq!(file_url("/a/é.mp4"), "file:///a/%C3%A9.mp4");
    }
}
