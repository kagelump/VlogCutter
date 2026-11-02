//! Offset-consensus data model (ARCHITECTURE §8, §9). Data only: the
//! computation that produces these values lands in a later packet.
use crate::tv_review_sync::SyncBoundary;
use serde::{Deserialize, Serialize};

/// A maximal run of queries that agree on one mic-to-movie offset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConsensusSegment {
    pub segment_id: String,
    /// Capture clock the sample bounds are expressed on.
    pub clock_id: String,
    /// Median of movie_seconds - mic_center_seconds over supporting queries.
    pub offset_seconds: f64,
    /// Centre of the first supporting query, in capture samples.
    pub mic_start_sample: u64,
    /// Centre of the last supporting query, in capture samples.
    pub mic_end_sample: u64,
    pub supporting_queries: Vec<usize>,
    pub residual_rms_seconds: f64,
    /// Least-squares slope times 1e6.
    pub drift_ppm: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SoundtrackConsensus {
    pub schema_version: u32,
    pub plan_digest: String,
    pub segments: Vec<ConsensusSegment>,
    /// Splits between segments reuse the existing sync boundary type.
    pub boundaries: Vec<SyncBoundary>,
    pub unsupported_queries: Vec<usize>,
}
