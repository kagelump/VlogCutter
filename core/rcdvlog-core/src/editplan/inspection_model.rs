//! Source inspection is evidence, never a human acceptance or defect verdict.
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum SourceInspectionError {
    #[error("source inspection is stale: representation, source, or orientation changed")]
    StaleEvidence,
    #[error("source inspection is already in progress")]
    Busy,
}

impl SourceInspectionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::StaleEvidence => "stale_evidence",
            Self::Busy => "representation_busy",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InspectionRepresentation {
    Original,
    GeneratedProxy,
    CameraProxy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InspectionPurpose {
    Locator,
    ProposedBoundary,
    InspectedBoundary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceInspectionRequest {
    pub asset_id: String,
    pub representation: InspectionRepresentation,
    pub purpose: InspectionPurpose,
    pub source_in_seconds: f64,
    pub source_out_seconds: f64,
    #[serde(default)]
    pub before_seconds: f64,
    #[serde(default)]
    pub after_seconds: f64,
    /// Evenly distributed samples, including the first/last available frames.
    pub sample_count: u32,
    pub max_width: u32,
    /// Additional requested source times; actual decoded PTS is returned separately.
    #[serde(default)]
    pub source_times_seconds: Vec<f64>,
    /// Exact integer PTS in the selected representation's time base. Requires
    /// expectedRepresentationFingerprint from a prior inspection.
    #[serde(default)]
    pub exact_representation_pts: Vec<i64>,
    #[serde(default)]
    pub expected_representation_fingerprint: Option<String>,
    /// Independent opt-in. False never extracts sound or reads a transcript.
    #[serde(default)]
    pub include_audio: bool,
    #[serde(default)]
    pub include_transcript: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectionFile {
    pub path: String,
    pub blake3: String,
    pub byte_length: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectionTimeMapping {
    pub time_base_numerator: i64,
    pub time_base_denominator: i64,
    pub representation_origin_pts: i64,
    pub source_offset_seconds: f64,
    pub representation_offset_seconds: f64,
    pub seconds_scale: u32,
    /// actualSourcePts, verifiedCameraNominal, or secondsCorrespondence.
    pub precision: String,
    pub source_frame_reduction: Option<u32>,
}

impl InspectionTimeMapping {
    pub fn source_seconds(&self, pts: i64) -> f64 {
        let local = (pts - self.representation_origin_pts) as f64 * self.time_base_numerator as f64
            / self.time_base_denominator as f64;
        self.source_offset_seconds
            + (local - self.representation_offset_seconds) * f64::from(self.seconds_scale)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectedFrame {
    pub evidence_id: String,
    pub representation_pts: i64,
    pub source_seconds: f64,
    pub excerpt_seconds: f64,
    pub image: InspectionFile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectionWaveformBin {
    pub excerpt_start_seconds: f64,
    pub excerpt_end_seconds: f64,
    pub peak: f64,
    pub rms: f64,
    /// Energy cue only; never a speech or audio-quality verdict.
    pub low_energy: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectedAudio {
    /// complete, completeWithLeadingGap or noAudioStream. Failures are errors.
    pub state: String,
    pub playable: Option<InspectionFile>,
    pub sample_rate: u32,
    pub channels: u32,
    pub source_in_samples: u64,
    pub source_out_samples: u64,
    pub stream_source_in_seconds: Option<f64>,
    pub leading_gap_seconds: f64,
    pub waveform: Vec<InspectionWaveformBin>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectedUtterance {
    pub source_in_seconds: f64,
    pub source_out_seconds: f64,
    pub excerpt_in_seconds: f64,
    pub excerpt_out_seconds: f64,
    pub text: String,
    pub clipped_by_window: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInspection {
    pub evidence_id: String,
    pub recipe: String,
    pub request: SourceInspectionRequest,
    pub source_fingerprint: String,
    pub representation_fingerprint: String,
    pub rotation_degrees: u16,
    pub mapping: InspectionTimeMapping,
    pub window_in_seconds: f64,
    pub window_out_seconds: f64,
    pub frames: Vec<InspectedFrame>,
    pub audio: Option<InspectedAudio>,
    pub utterances: Vec<InspectedUtterance>,
    pub available_frame_count: u32,
    pub min_available_frame_interval_seconds: Option<f64>,
    pub max_available_frame_interval_seconds: Option<f64>,
    pub max_unsampled_gap_seconds: f64,
    pub limitations: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInspectionResult {
    pub inspection: SourceInspection,
    pub reused: bool,
}
