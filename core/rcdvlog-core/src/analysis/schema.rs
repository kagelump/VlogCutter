//! The model's output contract, and the validator that enforces it.
//!
//! Two layers, in order: JSON parsing, then **domain** validation against closed
//! vocabularies and bounds. A schema-valid response is not a domain-valid
//! response — measured, 33 of 33 STT responses were schema-valid while still
//! returning a mix of `ja`, `en`, `zh-TW`, `zh-CN` and bare `zh`, and a vision
//! model emitted an out-of-enum `cameraMovement` under strict structured output.
//!
//! Nothing here repairs. A response that violates the contract is rejected with
//! a recorded reason and writes nothing, because a partially applied result is
//! indistinguishable downstream from a correct one.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Bounds every array in the schema. Without these a repetition loop produced
/// grammar-legal but truncated JSON that no parser could accept.
const MAX_LABELS: usize = 5;
const MAX_OCR: usize = 8;
const MAX_TITLE_CHARS: usize = 60;
const MAX_DESCRIPTION_CHARS: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraMovement {
    Static,
    Pan,
    Tilt,
    Handheld,
    Tracking,
    Zoom,
    /// Deliberate escape hatch. `qwen3.7-flash` emitted `panning`, which is not
    /// in the enum, and it has no strict structured-output support; without a
    /// legal "none of these" value the whole response would be discarded over a
    /// field that carries little weight.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnusableReason {
    Black,
    Blurry,
    Noise,
    Overexposed,
    Other,
}

/// One clip as the model observed it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipObservation {
    pub title: String,
    pub description: String,
    pub labels: Vec<String>,
    pub usable: bool,
    pub unusable_reason: Option<UnusableReason>,
    pub best_moment_start_ms: Option<u32>,
    pub best_moment_end_ms: Option<u32>,
    pub thumbnail_timestamp_ms: Option<u32>,
    pub camera_movement: CameraMovement,
    pub ocr_text: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawObservation {
    title: String,
    description: String,
    labels: Vec<String>,
    usable: bool,
    #[serde(default)]
    unusable_reason: Option<String>,
    #[serde(default)]
    best_moment_start_ms: Option<i64>,
    #[serde(default)]
    best_moment_end_ms: Option<i64>,
    #[serde(default)]
    thumbnail_timestamp_ms: Option<i64>,
    camera_movement: String,
    ocr_text: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    /// The response ran into the token limit mid-structure. Distinguished from
    /// ordinary malformed JSON because it has a different cause and fix.
    Truncated,
    Malformed(String),
    UnknownEnum {
        field: &'static str,
        value: String,
    },
    ThumbnailNotInPack {
        value: u32,
    },
    RangeOutOfBounds {
        start_ms: i64,
        end_ms: i64,
    },
    TooManyItems {
        field: &'static str,
        count: usize,
    },
    TooLong {
        field: &'static str,
        chars: usize,
    },
}

impl DomainError {
    /// Stable machine-readable reason, stored on the analysis row.
    pub fn as_reason(&self) -> String {
        match self {
            DomainError::Truncated => "truncated".to_string(),
            DomainError::Malformed(detail) => format!("malformed: {detail}"),
            DomainError::UnknownEnum { field, value } => {
                format!("unknown_enum: {field}={value}")
            }
            DomainError::ThumbnailNotInPack { value } => {
                format!("thumbnail_not_in_pack: {value}")
            }
            DomainError::RangeOutOfBounds { start_ms, end_ms } => {
                format!("range_out_of_bounds: {start_ms}..{end_ms}")
            }
            DomainError::TooManyItems { field, count } => {
                format!("too_many_items: {field}={count}")
            }
            DomainError::TooLong { field, chars } => format!("too_long: {field}={chars}"),
        }
    }
}

impl std::fmt::Display for DomainError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.as_reason())
    }
}

impl std::error::Error for DomainError {}

fn parse_camera_movement(value: &str) -> Option<CameraMovement> {
    match value.trim().to_ascii_lowercase().as_str() {
        "static" => Some(CameraMovement::Static),
        "pan" => Some(CameraMovement::Pan),
        "tilt" => Some(CameraMovement::Tilt),
        "handheld" => Some(CameraMovement::Handheld),
        "tracking" => Some(CameraMovement::Tracking),
        "zoom" => Some(CameraMovement::Zoom),
        "other" => Some(CameraMovement::Other),
        _ => None,
    }
}

fn parse_unusable_reason(value: &str) -> Option<UnusableReason> {
    match value.trim().to_ascii_lowercase().as_str() {
        "black" => Some(UnusableReason::Black),
        "blurry" => Some(UnusableReason::Blurry),
        "noise" => Some(UnusableReason::Noise),
        "overexposed" => Some(UnusableReason::Overexposed),
        "other" => Some(UnusableReason::Other),
        _ => None,
    }
}

/// Parses and domain-validates one model response.
pub fn validate(
    raw: &str,
    frame_timestamps: &[u32],
    clip_duration_ms: u32,
) -> Result<ClipObservation, DomainError> {
    let parsed: RawObservation = serde_json::from_str(raw).map_err(|error| {
        // serde reports an unexpected end of input for a response that hit the
        // token limit mid-structure, which is the failure bounded arrays exist
        // to prevent and is worth naming separately.
        if error.is_eof() {
            DomainError::Truncated
        } else {
            DomainError::Malformed(error.to_string())
        }
    })?;

    if parsed.labels.len() > MAX_LABELS {
        return Err(DomainError::TooManyItems {
            field: "labels",
            count: parsed.labels.len(),
        });
    }
    if parsed.ocr_text.len() > MAX_OCR {
        return Err(DomainError::TooManyItems {
            field: "ocrText",
            count: parsed.ocr_text.len(),
        });
    }
    if parsed.title.chars().count() > MAX_TITLE_CHARS {
        return Err(DomainError::TooLong {
            field: "title",
            chars: parsed.title.chars().count(),
        });
    }
    if parsed.description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(DomainError::TooLong {
            field: "description",
            chars: parsed.description.chars().count(),
        });
    }

    let camera_movement =
        parse_camera_movement(&parsed.camera_movement).ok_or_else(|| DomainError::UnknownEnum {
            field: "cameraMovement",
            value: parsed.camera_movement.clone(),
        })?;

    let unusable_reason = match parsed.unusable_reason.as_deref() {
        None | Some("") => None,
        Some(value) => Some(
            parse_unusable_reason(value).ok_or(DomainError::UnknownEnum {
                field: "unusableReason",
                value: value.to_string(),
            })?,
        ),
    };

    if let Some(thumbnail) = parsed.thumbnail_timestamp_ms {
        // The field most able to produce an invalid edit decision: it must name
        // a frame that was actually supplied, not an invented moment.
        let matches = thumbnail >= 0 && frame_timestamps.contains(&(thumbnail as u32));
        if !matches {
            return Err(DomainError::ThumbnailNotInPack {
                value: thumbnail.max(0) as u32,
            });
        }
    }

    let (best_start, best_end) = match (parsed.best_moment_start_ms, parsed.best_moment_end_ms) {
        (Some(start), Some(end)) => {
            if start < 0 || end < 0 || end < start || end > i64::from(clip_duration_ms) {
                return Err(DomainError::RangeOutOfBounds {
                    start_ms: start,
                    end_ms: end,
                });
            }
            (Some(start as u32), Some(end as u32))
        }
        // A clip judged unusable has no best moment to offer; a half-specified
        // range is discarded rather than guessed at.
        _ => (None, None),
    };

    Ok(ClipObservation {
        title: parsed.title.trim().to_string(),
        description: parsed.description.trim().to_string(),
        labels: parsed
            .labels
            .into_iter()
            .map(|label| label.trim().to_string())
            .filter(|label| !label.is_empty())
            .collect(),
        usable: parsed.usable,
        unusable_reason,
        best_moment_start_ms: best_start,
        best_moment_end_ms: best_end,
        thumbnail_timestamp_ms: parsed.thumbnail_timestamp_ms.map(|value| value as u32),
        camera_movement,
        ocr_text: parsed
            .ocr_text
            .into_iter()
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .collect(),
    })
}

/// The JSON schema sent to the provider.
///
/// Every array is bounded. `landmarkCandidates` is deliberately absent: across
/// ten models the field was populated by three and the content was fabricated —
/// `Tokyo Tower` proposed for an Osaka market hall — so its earlier apparent
/// safety was an artifact of one model leaving it empty.
pub fn output_schema(frame_timestamps: &[u32], clip_duration_ms: u32) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        // Gemini's structured-output dialect requires every declared property
        // to appear in `required`; optional values are represented as nullable.
        "required": ["title", "description", "labels", "usable", "unusableReason",
                     "bestMomentStartMs", "bestMomentEndMs", "thumbnailTimestampMs",
                     "cameraMovement", "ocrText"],
        "properties": {
            "title": {
                "type": "string",
                "description": "A short English title, 3-6 words. English regardless of the language shown."
            },
            "description": {
                "type": "string",
                "description": "Two or three English sentences describing what is visible."
            },
            "labels": {
                "type": "array",
                "maxItems": MAX_LABELS,
                "items": {"type": "string"},
                "description": "Prefer labels from the supplied vocabulary; propose a new one only when nothing fits."
            },
            "usable": {
                "type": "boolean",
                "description": "False if the frames are black, blank, or unreadable."
            },
            "unusableReason": {
                "anyOf": [
                    {"type": "string", "enum": ["black", "blurry", "noise", "overexposed", "other"]},
                    {"type": "null"}
                ]
            },
            "bestMomentStartMs": {
                "type": ["integer", "null"],
                "minimum": 0,
                "maximum": clip_duration_ms
            },
            "bestMomentEndMs": {
                "type": ["integer", "null"],
                "minimum": 0,
                "maximum": clip_duration_ms
            },
            "thumbnailTimestampMs": {
                "anyOf": [
                    {
                        "type": "integer",
                        "enum": frame_timestamps.iter().map(|stamp| json!(stamp)).collect::<Vec<Value>>()
                    },
                    {"type": "null"}
                ],
                "description": "Must be exactly one of the supplied frame timestamps."
            },
            "cameraMovement": {
                "type": "string",
                "enum": ["static", "pan", "tilt", "handheld", "tracking", "zoom", "other"]
            },
            "ocrText": {
                "type": "array",
                "maxItems": MAX_OCR,
                "items": {"type": "string"},
                "description": "Text visibly readable in the frames. Do not guess."
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_json() -> String {
        r#"{"title":"Malaysia pavilion walkthrough",
            "description":"A wide shot of an indoor exhibition space.",
            "labels":["museum exhibit","indoor"],
            "usable":true,"unusableReason":null,
            "bestMomentStartMs":1000,"bestMomentEndMs":4000,
            "thumbnailTimestampMs":2500,
            "cameraMovement":"pan","ocrText":["Kuala Lumpur"]}"#
            .to_string()
    }

    #[test]
    fn a_valid_response_parses() {
        let observation = validate(&ok_json(), &[400, 2_500, 6_000], 12_000).unwrap();
        assert_eq!(observation.title, "Malaysia pavilion walkthrough");
        assert_eq!(observation.thumbnail_timestamp_ms, Some(2_500));
        assert_eq!(observation.camera_movement, CameraMovement::Pan);
        assert_eq!(observation.labels.len(), 2);
        assert_eq!(observation.ocr_text, vec!["Kuala Lumpur"]);
    }

    #[test]
    fn truncated_json_is_rejected_as_truncation() {
        let full = ok_json();
        let truncated = full[..full.len() / 2].to_string();
        assert_eq!(
            validate(&truncated, &[400, 2_500], 12_000),
            Err(DomainError::Truncated)
        );
    }

    #[test]
    fn an_out_of_enum_camera_movement_is_rejected() {
        // qwen3.7-flash emitted exactly this value in the model eval.
        let raw = ok_json().replace("\"pan\"", "\"panning\"");
        assert!(matches!(
            validate(&raw, &[400, 2_500], 12_000),
            Err(DomainError::UnknownEnum {
                field: "cameraMovement",
                ..
            })
        ));
    }

    #[test]
    fn a_thumbnail_outside_the_frame_pack_is_rejected() {
        let raw = ok_json().replace(
            "\"thumbnailTimestampMs\":2500",
            "\"thumbnailTimestampMs\":9999",
        );
        assert_eq!(
            validate(&raw, &[400, 2_500], 12_000),
            Err(DomainError::ThumbnailNotInPack { value: 9_999 })
        );
    }

    #[test]
    fn a_best_moment_past_the_end_of_the_clip_is_rejected() {
        let raw = ok_json().replace("\"bestMomentEndMs\":4000", "\"bestMomentEndMs\":99000");
        assert!(matches!(
            validate(&raw, &[400, 2_500], 12_000),
            Err(DomainError::RangeOutOfBounds { .. })
        ));
    }

    #[test]
    fn an_inverted_best_moment_is_rejected() {
        let raw = ok_json()
            .replace("\"bestMomentStartMs\":1000", "\"bestMomentStartMs\":5000")
            .replace("\"bestMomentEndMs\":4000", "\"bestMomentEndMs\":2000");
        assert!(matches!(
            validate(&raw, &[400, 2_500], 12_000),
            Err(DomainError::RangeOutOfBounds { .. })
        ));
    }

    #[test]
    fn too_many_labels_is_rejected_rather_than_silently_truncated() {
        let raw = ok_json().replace(
            r#"["museum exhibit","indoor"]"#,
            r#"["a","b","c","d","e","f","g"]"#,
        );
        assert_eq!(
            validate(&raw, &[400, 2_500], 12_000),
            Err(DomainError::TooManyItems {
                field: "labels",
                count: 7
            })
        );
    }

    #[test]
    fn an_unusable_clip_may_omit_ranges() {
        let raw = r#"{"title":"Black frames","description":"Entirely black.",
            "labels":[],"usable":false,"unusableReason":"black",
            "bestMomentStartMs":null,"bestMomentEndMs":null,
            "thumbnailTimestampMs":null,"cameraMovement":"static","ocrText":[]}"#;
        let observation = validate(raw, &[400], 12_000).unwrap();
        assert!(!observation.usable);
        assert_eq!(observation.unusable_reason, Some(UnusableReason::Black));
        assert_eq!(observation.best_moment_start_ms, None);
    }

    #[test]
    fn an_over_long_title_is_rejected() {
        let raw = ok_json().replace(
            "Malaysia pavilion walkthrough",
            &"x".repeat(MAX_TITLE_CHARS + 1),
        );
        assert!(matches!(
            validate(&raw, &[400, 2_500], 12_000),
            Err(DomainError::TooLong { field: "title", .. })
        ));
    }

    #[test]
    fn blank_labels_and_ocr_entries_are_dropped() {
        let raw = ok_json()
            .replace(
                r#"["museum exhibit","indoor"]"#,
                r#"["museum exhibit","  "]"#,
            )
            .replace(r#"["Kuala Lumpur"]"#, r#"["Kuala Lumpur",""]"#);
        let observation = validate(&raw, &[400, 2_500], 12_000).unwrap();
        assert_eq!(observation.labels, vec!["museum exhibit"]);
        assert_eq!(observation.ocr_text, vec!["Kuala Lumpur"]);
    }

    #[test]
    fn the_schema_bounds_every_array() {
        let schema = output_schema(&[400, 2_500], 12_000);
        for array in ["labels", "ocrText"] {
            assert!(
                schema["properties"][array]["maxItems"].is_number(),
                "{array} is unbounded"
            );
        }
    }

    #[test]
    fn the_schema_omits_max_length_for_gemini_compatibility() {
        let schema = output_schema(&[400], 12_000);
        assert!(schema["properties"]["title"]["maxLength"].is_null());
        assert!(schema["properties"]["description"]["maxLength"].is_null());
    }

    #[test]
    fn the_schema_does_not_offer_a_landmark_field() {
        let schema = output_schema(&[400], 12_000);
        assert!(schema["properties"]["landmarkCandidates"].is_null());
    }

    #[test]
    fn the_schema_constrains_the_thumbnail_to_the_supplied_frames() {
        let schema = output_schema(&[400, 2_500], 12_000);
        let allowed = schema["properties"]["thumbnailTimestampMs"]["anyOf"][0]["enum"]
            .as_array()
            .unwrap()
            .clone();
        assert!(allowed.contains(&json!(400)));
        assert!(allowed.contains(&json!(2_500)));
        assert!(!allowed.contains(&json!(9_999)));
    }

    #[test]
    fn the_schema_offers_the_other_camera_movement_escape_hatch() {
        let schema = output_schema(&[400], 12_000);
        let allowed = schema["properties"]["cameraMovement"]["enum"]
            .as_array()
            .unwrap()
            .clone();
        assert!(allowed.contains(&json!("other")));
    }
}
