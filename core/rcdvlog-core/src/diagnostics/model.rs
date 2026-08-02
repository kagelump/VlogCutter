//! Diagnostic record types, frozen by D01-T027
//! (`docs/tasks/desktop/DIAGNOSTICS_MODEL.md`).
//!
//! A diagnostic is a durable, local observation about one node's view of the
//! world. It is not a typed operation: it carries no HLC time, takes no part in
//! the revision guard, and is never synchronized to a peer. "The source volume
//! is offline" is true of *this* machine at *this* moment; asserting it about a
//! node that never observed it would be a lie.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How badly the user needs to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

impl DiagnosticSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "error" => Some(Self::Error),
            "warning" => Some(Self::Warning),
            "info" => Some(Self::Info),
            _ => None,
        }
    }
}

/// What the diagnostic is about. `subject_id` is empty for `Project`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum DiagnosticSubject {
    Project,
    Asset { asset_id: String },
    Operation { operation_id: String },
    Volume { volume_hint: String },
    CaptureSequence { capture_sequence_id: String },
}

impl DiagnosticSubject {
    /// The persisted `(subject_type, subject_id)` pair.
    pub fn columns(&self) -> (&'static str, String) {
        match self {
            Self::Project => ("project", String::new()),
            Self::Asset { asset_id } => ("asset", asset_id.clone()),
            Self::Operation { operation_id } => ("operation", operation_id.clone()),
            Self::Volume { volume_hint } => ("volume", volume_hint.clone()),
            Self::CaptureSequence {
                capture_sequence_id,
            } => ("captureSequence", capture_sequence_id.clone()),
        }
    }

    /// Rebuilds a subject from its persisted columns. An unrecognized
    /// `subject_type` degrades to `Project` rather than failing the read: a
    /// diagnostic written by a newer build must still be *visible* to this one.
    pub fn from_columns(subject_type: &str, subject_id: &str) -> Self {
        match subject_type {
            "asset" => Self::Asset {
                asset_id: subject_id.to_string(),
            },
            "operation" => Self::Operation {
                operation_id: subject_id.to_string(),
            },
            "volume" => Self::Volume {
                volume_hint: subject_id.to_string(),
            },
            "captureSequence" => Self::CaptureSequence {
                capture_sequence_id: subject_id.to_string(),
            },
            _ => Self::Project,
        }
    }
}

/// One durable condition the user needs to know about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub id: String,
    pub dedupe_key: String,
    pub severity: DiagnosticSeverity,
    /// Stable machine-readable slug, e.g. `ingest.unreadable_media`. The
    /// namespace is `<area>.<condition>`, lowercase and dot-separated, and is
    /// stable once shipped — display surfaces key behaviour off it.
    pub kind: String,
    pub subject: DiagnosticSubject,
    pub message: String,
    pub context: Value,
    pub occurrence_count: u64,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub acknowledged_at: Option<String>,
}

/// What a producer supplies. Ids and timestamps are assigned by the recorder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticRecord {
    pub dedupe_key: String,
    pub severity: DiagnosticSeverity,
    pub kind: String,
    pub subject: DiagnosticSubject,
    pub message: String,
    pub context: Value,
}

/// Which diagnostics to read back. The default is the unacknowledged set at
/// every severity — what a user opening the app should be shown.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticFilter {
    pub severity_at_least: Option<DiagnosticSeverity>,
    pub kind_prefix: Option<String>,
    pub include_acknowledged: bool,
}

/// Unacknowledged counts by severity, for a badge or a status line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticCounts {
    pub errors: u64,
    pub warnings: u64,
    pub infos: u64,
}
