//! The camera filename-pattern table (D01-T028), documented in
//! `docs/tasks/desktop/CAMERA_PATTERNS.md`.
//!
//! This is deliberately *data*, not scattered branches: adding support for a
//! new camera is a new row here plus a row in the documentation, and nothing
//! else.

/// How much a pattern match actually claims.
///
/// A `Weak` pattern is good enough to group a card and useless as a claim about
/// the manufacturer — `C` plus four digits is used by Sony, by Canon cinema
/// bodies, and by others. Declaring the weakness is honest: suppressing the
/// match would lose the counter, and asserting a manufacturer would be a guess
/// presented as a fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternConfidence {
    Strong,
    Weak,
}

/// One camera's filename shape.
#[derive(Debug, Clone, Copy)]
pub struct CameraPattern {
    /// Stable slug, e.g. `dji-pocket-3`. Never displayed.
    pub id: &'static str,
    /// Human-readable camera name written into `assets.camera`.
    pub label: &'static str,
    /// Anchored regular expression over the filename stem.
    pub stem_pattern: &'static str,
    /// 1-based capture group holding the counter.
    pub counter_group: usize,
    pub confidence: PatternConfidence,
}

/// Evaluated in order; the first match wins. Strong patterns precede weak ones,
/// so a filename that could satisfy both is claimed by the stronger.
///
/// Every shape here was confirmed against real footage, not invented from a
/// manual.
pub const CAMERA_PATTERNS: &[CameraPattern] = &[
    // Timestamp-prefixed exports preserve the camera's original token after
    // the date. Keep the timestamp exact so malformed/date-only names remain
    // unidentified rather than inheriting a camera claim.
    CameraPattern {
        id: "dji-pocket-3-date-first",
        label: "DJI Pocket 3",
        stem_pattern: r"^\d{14}_DJI_(\d{4})_D$",
        counter_group: 1,
        confidence: PatternConfidence::Strong,
    },
    CameraPattern {
        id: "insta360-ultra-go-date-first",
        label: "Insta360 Ultra Go",
        stem_pattern: r"^\d{14}_VID_(\d+)$",
        counter_group: 1,
        confidence: PatternConfidence::Strong,
    },
    CameraPattern {
        id: "sony-c-series-date-first",
        label: "Sony (C-series)",
        stem_pattern: r"^\d{14}_C(\d{4})$",
        counter_group: 1,
        confidence: PatternConfidence::Weak,
    },
    CameraPattern {
        id: "dji-pocket-3",
        label: "DJI Pocket 3",
        stem_pattern: r"^DJI_\d{14}_(\d{4})_D$",
        counter_group: 1,
        confidence: PatternConfidence::Strong,
    },
    CameraPattern {
        id: "insta360-ultra-go",
        label: "Insta360 Ultra Go",
        stem_pattern: r"^VID_\d{8}_\d{6}_(\d+)$",
        counter_group: 1,
        confidence: PatternConfidence::Strong,
    },
    CameraPattern {
        id: "sony-c-series",
        label: "Sony (C-series)",
        stem_pattern: r"^C(\d{4})$",
        counter_group: 1,
        confidence: PatternConfidence::Weak,
    },
];
