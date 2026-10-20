//! Persisted chronology fact types, frozen by D01-T007
//! (`docs/tasks/desktop/CHRONOLOGY_MODEL.md`).
//!
//! These rows are *evidence only*. Trip time, local display time, confidence,
//! evidence, uncertainty, and total order are derived by the solver (D01-T008)
//! and are never stored here.

use serde::{Deserialize, Serialize};

/// One capture sequence: a likely monotonic filename chain scoped to one card.
/// Filename identity is scoped to the capture sequence because two cards reuse
/// the same counter (`PRD.md` §8.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSequence {
    pub id: String,
    pub camera_key: String,
    pub label: String,
    pub volume_hint: String,
    pub filename_prefix: String,
}

/// The per-asset hypothesis the solver starts from: what the camera claimed.
/// Raw camera timestamps are never edited (`PRD.md` §9.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetTimeHypothesis {
    pub asset_id: String,
    pub filename: String,
    pub captured_at_raw: Option<String>,
    pub capture_sequence_id: Option<String>,
    pub filename_counter: Option<u64>,
    pub duration_seconds: f64,
    /// Timestamp observations retained at ingest. `None` means legacy/unavailable;
    /// an empty vector means a new probe found no candidate timestamps.
    #[serde(default)]
    pub timestamp_candidates: Option<Vec<TimestampCandidate>>,
}

/// Immutable timestamp observation and its source-provided semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimestampCandidate {
    pub origin: String,
    pub original_text: String,
    pub parsed_value: Option<String>,
    pub timezone_claim: Option<String>,
    pub parse_status: String,
    pub semantics: Option<String>,
}

/// One offset model over a contiguous counter range in one capture sequence.
/// `start_counter` is the first counter the epoch governs; `end_counter` is
/// inclusive, with `None` meaning open-ended. An epoch never applies across
/// capture sequences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockEpoch {
    pub id: String,
    pub capture_sequence_id: String,
    pub start_counter: u64,
    pub end_counter: Option<u64>,
    pub offset_seconds: i64,
    pub confidence: f32,
    pub evidence: Vec<String>,
    pub label: String,
}

/// A rare single-clip override that breaks its capture sequence. Rides the
/// `projectAsset` aggregate, so its identity is the asset id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipTimeException {
    pub asset_id: String,
    pub trip_time: String,
    pub reason: String,
}

/// What kind of place a trip phase covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseKind {
    Location,
    Transit,
}

/// How local display time is derived inside a trip phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayMode {
    Local,
    Dual,
    Utc,
}

/// Which side of an existing visible phase becomes the newly authored phase
/// when the user cuts context at one clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseSplitDirection {
    Through,
    From,
}

/// Where the user physically was, with timezone context (`PRD.md` §10).
/// Local display time derives from the covering phase, never from camera
/// metadata timezone (architecture contract §3.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TripPhase {
    pub id: String,
    pub label: String,
    pub kind: PhaseKind,
    /// Optional clip identities that keep the phase boundary attached to the
    /// user's footage evidence when later clock corrections reorder cameras.
    /// The start includes the anchor clip; the end includes its anchor clip.
    /// Legacy phases leave these null and continue to use their fixed times.
    #[serde(default)]
    pub start_anchor_asset_id: Option<String>,
    #[serde(default)]
    pub end_anchor_asset_id: Option<String>,
    /// Last successfully resolved UTC bounds. For fixed phases these are the
    /// authority; for anchored phases they are a durable fallback when an
    /// anchor temporarily cannot be placed.
    pub start_trip_time: String,
    pub end_trip_time: String,
    pub timezone: Option<String>,
    pub origin_timezone: Option<String>,
    pub destination_timezone: Option<String>,
    pub display_mode: DisplayMode,
}

/// How strongly an event-order constraint binds the solver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintStrength {
    Hard,
    Soft,
}

/// Ordering knowledge independent of exact time (`PRD.md` §9.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventOrderConstraint {
    pub id: String,
    pub earlier_asset_id: String,
    pub later_asset_id: String,
    pub strength: ConstraintStrength,
    pub source: String,
}

/// The complete persisted evidence for one project. Every collection is
/// returned in a deterministic order, sorted by primary key, so the solver
/// cannot inherit nondeterminism from the loader.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChronologyFacts {
    pub project_id: String,
    pub assets: Vec<AssetTimeHypothesis>,
    pub sequences: Vec<CaptureSequence>,
    pub epochs: Vec<ClockEpoch>,
    pub exceptions: Vec<ClipTimeException>,
    pub phases: Vec<TripPhase>,
    pub constraints: Vec<EventOrderConstraint>,
}
