//! Bounded passive final-mix matching. Local PCM only; no ASR/providers/network.
//! Scores are similarities, not probabilities or proof of TV playback identity.
use crate::{
    full_file_blake3,
    tv_review_sync::{CapturePosition, SyncAnchor, SyncAnchorSource, SyncMovieClock},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One fresh whole-movie byte observation shared by bounded window decodes.
/// Keep this handle inside a scheduling lease. It is not issuer authentication.
pub struct SoundtrackMovie {
    path: std::path::PathBuf,
    movie: SyncMovieClock,
    metadata: std::fs::Metadata,
}
impl SoundtrackMovie {
    pub fn open(path: &Path, movie: SyncMovieClock, cancel: &dyn Fn() -> bool) -> Result<Self> {
        let metadata = std::fs::metadata(path)?;
        ensure!(
            metadata.is_file(),
            "invalid input: regular local movie required"
        );
        let observed = full_file_blake3(path, cancel)?;
        ensure!(
            observed.hex_digest == movie.movie_digest,
            "invalid input: final movie digest mismatch"
        );
        let handle = Self {
            path: path.into(),
            movie,
            metadata,
        };
        handle.check_unchanged()?;
        Ok(handle)
    }
    fn check_unchanged(&self) -> Result<()> {
        let after = std::fs::metadata(&self.path)?;
        ensure!(
            self.metadata.len() == after.len() && self.metadata.modified()? == after.modified()?,
            "input_changed"
        );
        Ok(())
    }
    pub fn index(
        &self,
        reference: SoundtrackReferenceSpec,
        cancel: &dyn Fn() -> bool,
    ) -> Result<SoundtrackIndex> {
        ensure!(
            reference.movie == self.movie,
            "invalid input: reference movie identity mismatch"
        );
        self.check_unchanged()?;
        let result = SoundtrackIndex::decode_window(reference, &self.path, cancel)?;
        self.check_unchanged()?;
        Ok(result)
    }
}

pub const SOUNDTRACK_RECIPE: &str = "soundtrack.log-band-delta.fft512-hop128.smooth3-lag6.pcm8k.v1";
/// Weighted, robust-feature recipe (ARCHITECTURE §7). Opt-in; None means v1.
pub const SOUNDTRACK_RECIPE_V2: &str =
    "soundtrack.log-band-delta.fft512-hop128.smooth3-lag6.pcm8k.wtd-robust.v2";
pub const PCM_RATE: u32 = 8000;
const FFT: usize = 512;
const HOP: usize = 128;
const WARMUP: usize = 9;
const BANDS: usize = 16;
const MAX_SAMPLES: usize = 600 * PCM_RATE as usize;
const MAX_INDEX_BYTES: usize = 16 * 1024 * 1024;
const MAX_PRODUCTS: u64 = 50_000_000;
/// Peaks reported per report and per merged session query (ARCHITECTURE §3).
pub const MAX_BEST_PEAKS: usize = 4;
/// Peaks closer than 16 hops in movie time are one observation (ARCHITECTURE §3).
pub const PEAK_MERGE_SECONDS: f64 = 16.0 * HOP as f64 / PCM_RATE as f64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SoundtrackReferenceSpec {
    pub schema_version: u32,
    pub movie: SyncMovieClock,
    pub start_seconds: f64,
    pub duration_seconds: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SoundtrackIndex {
    pub schema_version: u32,
    pub recipe: String,
    pub reference: SoundtrackReferenceSpec,
    pub pcm_digest: String,
    pub index_digest: String,
    pub sample_count: usize,
    pub features: Vec<[f32; BANDS]>,
    /// Broadband log energy per frame (v2 recipe only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frame_energy: Vec<f32>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SoundtrackCapture {
    pub evidence_id: String,
    pub clock_id: String,
    /// Original microphone clock, before bounded resampling to PCM8k.
    pub sample_rate: u32,
    pub first_sample: u64,
    pub monotonic_origin_ns: u64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SoundtrackPolicy {
    pub minimum_similarity: f64,
    pub minimum_peak_margin: f64,
    pub minimum_subwindow_similarity: f64,
    pub uncertainty_seconds: f64,
    pub maximum_candidates: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distinctive: Option<DistinctivePolicy>,
}
/// Opt-in acceptance of a distinctive low-similarity peak (ARCHITECTURE §4).
/// Recommended values are 0.5 / 0.25 / 6.0; they are never serde defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DistinctivePolicy {
    pub minimum_similarity: f64,
    pub minimum_margin: f64,
    pub minimum_z_score: f64,
}
impl DistinctivePolicy {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.minimum_similarity.is_finite()
                && (0.3..=1.0).contains(&self.minimum_similarity)
                && self.minimum_margin.is_finite()
                && (0.1..=0.8).contains(&self.minimum_margin)
                && self.minimum_z_score.is_finite()
                && (3.0..=20.0).contains(&self.minimum_z_score),
            "invalid input: distinctive soundtrack policy"
        );
        Ok(())
    }
}
impl Default for SoundtrackPolicy {
    fn default() -> Self {
        Self {
            minimum_similarity: 0.8,
            minimum_peak_margin: 0.08,
            minimum_subwindow_similarity: 0.3,
            uncertainty_seconds: 0.08,
            maximum_candidates: 4,
            distinctive: None,
        }
    }
}
impl SoundtrackPolicy {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.minimum_similarity.is_finite()
                && (0.4..=1.0).contains(&self.minimum_similarity)
                && self.minimum_peak_margin.is_finite()
                && (0.01..=0.5).contains(&self.minimum_peak_margin)
                && self.minimum_subwindow_similarity.is_finite()
                && (0.3..=1.0).contains(&self.minimum_subwindow_similarity)
                && self.uncertainty_seconds.is_finite()
                && (0.032..=2.0).contains(&self.uncertainty_seconds)
                && (2..=8).contains(&self.maximum_candidates),
            "invalid input: soundtrack matching policy"
        );
        if let Some(d) = &self.distinctive {
            d.validate()?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SoundtrackMatchState {
    Matched,
    Ambiguous,
    Unmatched,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SoundtrackCandidate {
    pub anchor: SyncAnchor,
    pub similarity: f64,
    pub peak_margin: f64,
    pub reference_offset_frames: usize,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SoundtrackMatchReport {
    pub schema_version: u32,
    pub recipe: String,
    pub reference_movie_digest: String,
    pub reference_pcm_digest: String,
    pub reference_index_digest: String,
    pub query_pcm_digest: String,
    pub capture: SoundtrackCapture,
    pub policy: SoundtrackPolicy,
    pub query_sample_count: usize,
    pub state: SoundtrackMatchState,
    pub reason: String,
    pub candidates: Vec<SoundtrackCandidate>,
    pub searched_offsets: usize,
    pub comparison_products: u64,
    pub grid_seconds: f64,
    pub best_similarity: Option<f64>,
    pub runner_up_similarity: Option<f64>,
    /// Best peaks of the searched reference, best first, at most 4 (§3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub best_peaks: Vec<SoundtrackPeak>,
    /// Robust z-score of the best coarse score (§4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distinctiveness: Option<f64>,
    /// BLAKE3 of the per-frame weight bytes when a mask was applied (§6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_digest: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SoundtrackPeak {
    pub similarity: f64,
    pub reference_offset_frames: usize,
    /// reference.start_seconds + (offset*HOP + query_len_samples/2) / PCM_RATE
    pub movie_seconds: f64,
    pub subwindow_similarities: [f64; 3],
}
fn valid_digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn opaque(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 180
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn validate_pcm(pcm: &[f32]) -> Result<()> {
    ensure!(
        !pcm.is_empty()
            && pcm.len() <= MAX_SAMPLES
            && pcm.iter().all(|s| s.is_finite() && s.abs() <= 16.0),
        "invalid input: bounded finite normalized mono 8k PCM required"
    );
    Ok(())
}
fn pcm_digest(pcm: &[f32]) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"vlogcutter.pcm8k.f32le.v1\0");
    for s in pcm {
        h.update(&s.to_le_bytes());
    }
    h.finalize().to_hex().to_string()
}
impl SoundtrackReferenceSpec {
    pub fn validate(&self) -> Result<()> {
        let m = &self.movie;
        ensure!(
            self.schema_version == 1
                && valid_digest(&m.movie_digest)
                && m.time_base_numerator > 0
                && m.time_base_denominator > 0
                && m.origin_pts.unsigned_abs() <= 9_007_199_254_740_991
                && m.duration_pts > 0
                && m.duration_pts <= 9_007_199_254_740_991
                && (m.origin_pts as i128 + m.duration_pts as i128).unsigned_abs()
                    <= 9_007_199_254_740_991
                && m.duration_pts as f64 / m.ticks_per_second() <= 21600.0,
            "invalid input: reference movie clock/version"
        );
        ensure!(
            self.start_seconds.is_finite()
                && self.duration_seconds.is_finite()
                && self.start_seconds >= 0.0
                && (4.0..=600.0).contains(&self.duration_seconds)
                && self.start_seconds + self.duration_seconds
                    <= m.duration_pts as f64 / m.ticks_per_second() + 1e-6,
            "invalid input: reference window outside movie or 4..600 second bound"
        );
        Ok(())
    }
}
impl SoundtrackIndex {
    /// The caller binds PCM to a previously byte-checked final movie window.
    /// Low-level PCM construction is not independent movie verification.
    pub fn from_pcm(
        reference: SoundtrackReferenceSpec,
        pcm: &[f32],
        cancel: &dyn Fn() -> bool,
    ) -> Result<Self> {
        reference.validate()?;
        validate_pcm(pcm)?;
        ensure!(
            (pcm.len() as f64 / PCM_RATE as f64 - reference.duration_seconds).abs() <= 0.05,
            "invalid input: reference PCM duration mismatch"
        );
        let mut index = Self {
            schema_version: 1,
            recipe: SOUNDTRACK_RECIPE.into(),
            reference,
            pcm_digest: pcm_digest(pcm),
            index_digest: String::new(),
            sample_count: pcm.len(),
            features: features(pcm, cancel)?,
            frame_energy: vec![],
        };
        index.index_digest = index.digest()?;
        Ok(index)
    }
    /// Hash the exact movie and decode its final mixed first audio stream once.
    /// An index is reusable derived evidence, not current renderability proof.
    pub fn from_movie(
        reference: SoundtrackReferenceSpec,
        path: &Path,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Self> {
        SoundtrackMovie::open(path, reference.movie.clone(), cancel)?.index(reference, cancel)
    }
    fn decode_window(
        reference: SoundtrackReferenceSpec,
        path: &Path,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Self> {
        reference.validate()?;
        let mut command = crate::tool_command("ffmpeg");
        command
            .args([
                "-v",
                "error",
                "-nostdin",
                "-protocol_whitelist",
                "file,pipe",
                "-ss",
            ])
            .arg(format!("{:.9}", reference.start_seconds))
            .arg("-i")
            .arg(path)
            .arg("-t")
            .arg(format!("{:.9}", reference.duration_seconds))
            .args([
                "-map",
                "0:a:0",
                "-vn",
                "-af",
                "aresample=8000:async=1:first_pts=0",
            ])
            .args(["-ac", "1", "-ar", "8000", "-f", "f32le", "pipe:1"]);
        let out = crate::stall::output_with_timeout_and_cancellation(
            &mut command,
            "soundtrack window decode",
            std::time::Duration::from_secs(180),
            &cancel,
        )?;
        ensure!(
            out.status.success(),
            "media tool failed: final soundtrack decode"
        );
        ensure!(
            out.stdout.len() <= MAX_SAMPLES * 4 && out.stdout.len() % 4 == 0,
            "invalid input: decoded soundtrack exceeds bound"
        );
        let pcm: Vec<f32> = out
            .stdout
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        Self::from_pcm(reference, &pcm, cancel)
    }
    fn digest(&self) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(&(
            SOUNDTRACK_RECIPE,
            &self.reference,
            &self.pcm_digest,
            self.sample_count,
            &self.features,
        ))?)
        .to_hex()
        .to_string())
    }
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= MAX_INDEX_BYTES,
            "invalid input: soundtrack index exceeds 16 MiB"
        );
        let index: Self = serde_json::from_slice(bytes)?;
        index.validate()?;
        Ok(index)
    }
    pub fn validate(&self) -> Result<()> {
        self.reference.validate()?;
        ensure!(
            self.schema_version == 1
                && self.recipe == SOUNDTRACK_RECIPE
                && valid_digest(&self.pcm_digest)
                && valid_digest(&self.index_digest)
                && self.sample_count >= FFT + 3 * HOP
                && self.sample_count <= MAX_SAMPLES
                && (self.sample_count as f64 / PCM_RATE as f64 - self.reference.duration_seconds)
                    .abs()
                    <= 0.05
                && self.features.len() == (self.sample_count - FFT) / HOP + 1
                && self
                    .features
                    .iter()
                    .flatten()
                    .all(|x| x.is_finite() && x.abs() <= 80.0)
                && self.index_digest == self.digest()?,
            "invalid input: soundtrack index version/content mismatch"
        );
        Ok(())
    }
    pub fn match_pcm(
        &self,
        pcm: &[f32],
        capture: SoundtrackCapture,
        policy: &SoundtrackPolicy,
        cancel: &dyn Fn() -> bool,
    ) -> Result<SoundtrackMatchReport> {
        self.validate()?;
        validate_pcm(pcm)?;
        ensure!(
            (4 * PCM_RATE as usize..=20 * PCM_RATE as usize).contains(&pcm.len())
                && pcm.len() <= self.sample_count,
            "invalid input: query must be 4..20 seconds within reference size"
        );
        ensure!(
            opaque(&capture.evidence_id)
                && opaque(&capture.clock_id)
                && (8000..=192000).contains(&capture.sample_rate)
                && capture.first_sample <= 9_007_199_254_740_991
                && capture.monotonic_origin_ns <= 9_007_199_254_740_991,
            "invalid input: microphone clock identity"
        );
        policy.validate()?;
        let mut report = SoundtrackMatchReport {
            schema_version: 1,
            recipe: SOUNDTRACK_RECIPE.into(),
            reference_movie_digest: self.reference.movie.movie_digest.clone(),
            reference_pcm_digest: self.pcm_digest.clone(),
            reference_index_digest: self.index_digest.clone(),
            query_pcm_digest: pcm_digest(pcm),
            capture: capture.clone(),
            policy: policy.clone(),
            query_sample_count: pcm.len(),
            state: SoundtrackMatchState::Unmatched,
            reason: "weak_or_non_distinctive".into(),
            candidates: vec![],
            searched_offsets: 0,
            comparison_products: 0,
            grid_seconds: HOP as f64 / PCM_RATE as f64,
            best_similarity: None,
            runner_up_similarity: None,
            best_peaks: vec![],
            distinctiveness: None,
            mask_digest: None,
        };
        ensure!(!cancel(), "soundtrack match cancelled");
        let rms =
            (pcm.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / pcm.len() as f64).sqrt();
        if rms < 0.003 {
            report.reason = "quiet_query".into();
            return Ok(report);
        }
        let query = features(pcm, cancel)?;
        let norm = query
            .iter()
            .skip(WARMUP)
            .flatten()
            .map(|x| f64::from(*x).powi(2))
            .sum::<f64>();
        if norm / ((query.len() * BANDS) as f64) < 0.0001 {
            report.reason = "insufficient_spectral_variation".into();
            return Ok(report);
        }
        let last = self.features.len() - query.len();
        let mut coarse = Vec::new();
        for offset in (0..=last).step_by(4) {
            ensure!(!cancel(), "soundtrack match cancelled");
            let score = similarity(
                &query,
                &self.features[offset..offset + query.len()],
                4,
                &mut report.comparison_products,
            )?;
            coarse.push((score, offset));
            report.searched_offsets += 1;
        }
        if policy.distinctive.is_some() {
            let scores: Vec<f64> = coarse.iter().map(|c| c.0).collect();
            report.distinctiveness = robust_z(&scores);
        }
        coarse.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        let mut peaks: Vec<(f64, usize)> = vec![];
        for (_, offset) in coarse {
            if peaks.iter().any(|(_, old)| old.abs_diff(offset) < 16) {
                continue;
            }
            let mut best = (-1.0, offset);
            for pos in offset.saturating_sub(4)..=(offset + 4).min(last) {
                let score = similarity(
                    &query,
                    &self.features[pos..pos + query.len()],
                    1,
                    &mut report.comparison_products,
                )?;
                if score > best.0 {
                    best = (score, pos);
                }
            }
            if !peaks.iter().any(|(_, old)| old.abs_diff(best.1) < 16) {
                peaks.push(best);
            }
            if peaks.len() == 16 {
                break;
            }
        }
        peaks.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        // §3: diagnostics for every searched report, including unmatched and
        // ambiguous ones. Filled before any early return below.
        for &(score, offset) in peaks.iter().take(MAX_BEST_PEAKS) {
            let mut subwindow_similarities = [0.0; 3];
            for (part, slot) in subwindow_similarities.iter_mut().enumerate() {
                let a = part * query.len() / 3;
                let b = (part + 1) * query.len() / 3;
                *slot = similarity(
                    &query[a..b],
                    &self.features[offset + a..offset + b],
                    1,
                    &mut report.comparison_products,
                )?;
            }
            report.best_peaks.push(SoundtrackPeak {
                similarity: score,
                reference_offset_frames: offset,
                movie_seconds: self.reference.start_seconds
                    + (offset * HOP + pcm.len() / 2) as f64 / PCM_RATE as f64,
                subwindow_similarities,
            });
        }
        report.best_similarity = peaks.first().map(|p| p.0);
        report.runner_up_similarity = peaks.get(1).map(|p| p.0);
        // Low-similarity peaks reach temporal support only when the distinctive
        // path is enabled (ARCHITECTURE §4). Without it this is the v1 gate.
        let gate = policy
            .distinctive
            .as_ref()
            .map_or(policy.minimum_similarity, |d| {
                d.minimum_similarity.min(policy.minimum_similarity)
            });
        if peaks.is_empty() || peaks[0].0 < gate {
            return Ok(report);
        }
        // A high score from half a query is insufficient after a buffer/seek.
        // Require independent support in every third of the query.
        let mut supported = Vec::new();
        for (score, offset) in peaks {
            let mut consistent = true;
            for part in 0..3 {
                let a = part * query.len() / 3;
                let b = (part + 1) * query.len() / 3;
                let local = similarity(
                    &query[a..b],
                    &self.features[offset + a..offset + b],
                    1,
                    &mut report.comparison_products,
                )?;
                if local < policy.minimum_subwindow_similarity {
                    consistent = false;
                    break;
                }
            }
            if consistent {
                supported.push((score, offset));
            }
        }
        let peaks = supported;
        if peaks.is_empty() {
            report.reason = "insufficient_temporal_support".into();
            return Ok(report);
        }
        let margin = peaks[0].0 - peaks.get(1).map_or(-1.0, |p| p.0);
        // Step 2 (v1): the old path is unchanged.
        let old_similar = peaks[0].0 >= policy.minimum_similarity;
        let old_matched = old_similar && margin >= policy.minimum_peak_margin;
        // Step 3 (ARCHITECTURE §4): consulted only when the old path did not match.
        let distinctive_pass = policy.distinctive.as_ref().is_some_and(|d| {
            peaks[0].0 >= d.minimum_similarity
                && margin >= d.minimum_margin
                && report
                    .distinctiveness
                    .is_some_and(|z| z >= d.minimum_z_score)
        });
        if !old_similar && !distinctive_pass {
            report.reason = "insufficient_temporal_support".into();
            return Ok(report);
        }
        let low_similarity = distinctive_pass && !old_matched;
        let (state, reason) = if old_matched {
            (SoundtrackMatchState::Matched, "distinctive_peak")
        } else if low_similarity {
            (
                SoundtrackMatchState::Matched,
                "distinctive_peak_low_similarity",
            )
        } else {
            (
                SoundtrackMatchState::Ambiguous,
                "multiple_similar_positions",
            )
        };
        report.state = state;
        report.reason = reason.into();
        let center = pcm.len() / 2;
        let capture_sample =
            capture.first_sample + (center as u64 * capture.sample_rate as u64 / PCM_RATE as u64);
        let monotonic_ns = capture.monotonic_origin_ns as u128
            + capture_sample as u128 * 1_000_000_000 / capture.sample_rate as u128;
        ensure!(
            capture_sample <= 9_007_199_254_740_991 && monotonic_ns <= 9_007_199_254_740_991,
            "invalid input: resulting capture clock exceeds v1 bounds"
        );
        let accepted_floor = if report.state == SoundtrackMatchState::Matched {
            peaks[0].0
        } else {
            peaks[0].0 - policy.minimum_peak_margin
        };
        for (score, offset) in peaks
            .into_iter()
            .filter(|p| p.0 >= accepted_floor)
            .take(policy.maximum_candidates)
        {
            let seconds =
                self.reference.start_seconds + (offset * HOP + center) as f64 / PCM_RATE as f64;
            let tps = self.reference.movie.ticks_per_second();
            let identity = serde_json::to_vec(&(
                "vlogcutter.soundtrack-anchor.v1",
                &capture,
                &report.query_pcm_digest,
                &self.index_digest,
                offset,
            ))?;
            report.candidates.push(SoundtrackCandidate {
                anchor: SyncAnchor {
                    evidence_id: format!("soundtrack_{}", blake3::hash(&identity).to_hex()),
                    capture: CapturePosition {
                        clock_id: capture.clock_id.clone(),
                        sample_index: capture_sample,
                        monotonic_ns: monotonic_ns as u64,
                    },
                    movie_pts: self.reference.movie.origin_pts + (seconds * tps).round() as i64,
                    source: SyncAnchorSource::PassiveSoundtrack,
                    confidence: if low_similarity {
                        (0.5 + margin).min(1.0)
                    } else {
                        score
                    },
                    movie_uncertainty_ticks: (policy.uncertainty_seconds * tps).ceil() as u64,
                    capture_uncertainty_samples: (HOP as f64 / PCM_RATE as f64
                        * capture.sample_rate as f64)
                        .ceil() as u32,
                    candidate_group_id: Some(format!("{}_matches", capture.evidence_id)),
                },
                similarity: score,
                peak_margin: margin,
                reference_offset_frames: offset,
            });
        }
        Ok(report)
    }
}
/// Robust z of the best coarse score against all coarse scores (ARCHITECTURE §4):
/// (best - median) / (1.4826 * MAD). None when the MAD is zero or undefined.
fn robust_z(scores: &[f64]) -> Option<f64> {
    let best = scores.iter().copied().reduce(f64::max)?;
    let center = median(scores)?;
    let deviations: Vec<f64> = scores.iter().map(|s| (s - center).abs()).collect();
    let mad = median(&deviations)?;
    if !mad.is_finite() || mad <= 0.0 {
        return None;
    }
    let z = (best - center) / (1.4826 * mad);
    z.is_finite().then_some(z)
}
fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    Some(if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    })
}
fn similarity(
    a: &[[f32; BANDS]],
    b: &[[f32; BANDS]],
    stride: usize,
    products: &mut u64,
) -> Result<f64> {
    let mut dot = 0.0;
    let mut aa = 0.0;
    let mut bb = 0.0;
    for i in (WARMUP..a.len()).step_by(stride) {
        for j in 0..BANDS {
            let x = f64::from(a[i][j]);
            let y = f64::from(b[i][j]);
            dot += x * y;
            aa += x * x;
            bb += y * y;
            *products += 1;
        }
    }
    ensure!(
        *products <= MAX_PRODUCTS,
        "invalid input: soundtrack search exceeds deterministic compute budget; use a shorter reference window"
    );
    Ok(if aa * bb > 1e-12 {
        (dot / (aa * bb).sqrt()).clamp(-1.0, 1.0)
    } else {
        -1.0
    })
}
fn features(pcm: &[f32], cancel: &dyn Fn() -> bool) -> Result<Vec<[f32; BANDS]>> {
    ensure!(
        pcm.len() >= FFT + 3 * HOP,
        "invalid input: PCM too short for features"
    );
    // Fixed logarithmic bands, approximately 94 Hz..3.8 kHz. Temporal log
    // differences remove constant gain and much stationary room coloration.
    let edges = [
        6, 8, 10, 13, 16, 20, 25, 31, 39, 49, 61, 76, 95, 119, 149, 187, 240,
    ];
    let mut out = Vec::new();
    let mut history = Vec::<[f32; BANDS]>::new();
    for start in (0..=pcm.len() - FFT).step_by(HOP) {
        ensure!(!cancel(), "soundtrack features cancelled");
        let mut real = [0.0f64; FFT];
        let mut imag = [0.0f64; FFT];
        for i in 0..FFT {
            real[i] = f64::from(pcm[start + i])
                * (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / (FFT - 1) as f64).cos());
        }
        fft(&mut real, &mut imag);
        let mut bands = [0.0f32; BANDS];
        for j in 0..BANDS {
            let energy = (edges[j]..edges[j + 1])
                .map(|k| real[k] * real[k] + imag[k] * imag[k])
                .sum::<f64>();
            bands[j] = (energy + 1e-8).ln() as f32;
        }
        history.push(bands);
        let mut delta = [0.0; BANDS];
        if history.len() >= WARMUP {
            let n = history.len() - 1;
            for (j, value) in delta.iter_mut().enumerate() {
                let now = (history[n][j] + history[n - 1][j] + history[n - 2][j]) / 3.0;
                let before = (history[n - 6][j] + history[n - 7][j] + history[n - 8][j]) / 3.0;
                *value = (now - before).clamp(-8.0, 8.0);
            }
        }
        out.push(delta);
    }
    Ok(out)
}
fn fft(real: &mut [f64; FFT], imag: &mut [f64; FFT]) {
    let mut j = 0;
    for i in 1..FFT {
        let mut bit = FFT >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            real.swap(i, j);
            imag.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= FFT {
        let angle = -std::f64::consts::TAU / len as f64;
        let (sin, cos) = angle.sin_cos();
        for start in (0..FFT).step_by(len) {
            let (mut wr, mut wi) = (1.0, 0.0);
            for k in 0..len / 2 {
                let a = start + k;
                let b = a + len / 2;
                let tr = wr * real[b] - wi * imag[b];
                let ti = wr * imag[b] + wi * real[b];
                real[b] = real[a] - tr;
                imag[b] = imag[a] - ti;
                real[a] += tr;
                imag[a] += ti;
                let next = wr * cos - wi * sin;
                wi = wr * sin + wi * cos;
                wr = next;
            }
        }
        len *= 2;
    }
}
/// Local fixture/capture adapter: mono PCM WAV only. Decode/resample through the
/// capture owner beforehand, retaining its original sample-clock calibration.
pub fn read_pcm8k_wav(path: &Path) -> Result<Vec<f32>> {
    let metadata = std::fs::metadata(path)?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_SAMPLES as u64 * 4 + 4096,
        "invalid input: WAV exceeds bound"
    );
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    ensure!(
        spec.channels == 1 && spec.sample_rate == PCM_RATE,
        "invalid input: mono 8k WAV required"
    );
    let pcm = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<std::result::Result<Vec<_>, _>>()?,
        hound::SampleFormat::Int => {
            ensure!(spec.bits_per_sample <= 32, "invalid input: WAV bit depth");
            let scale = 2f32.powi(spec.bits_per_sample as i32 - 1);
            reader
                .samples::<i32>()
                .map(|s| s.map(|x| x as f32 / scale))
                .collect::<std::result::Result<Vec<_>, _>>()?
        }
    };
    validate_pcm(&pcm).context("invalid query WAV")?;
    Ok(pcm)
}
#[cfg(test)]
#[path = "tv_soundtrack_tests.rs"]
mod tests;
