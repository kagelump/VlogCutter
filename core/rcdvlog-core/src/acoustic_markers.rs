//! Acoustic movie-time markers for TV review copies (#226).
//!
//! A *marked review copy* is a separate derivative of an exact, verified
//! rendered movie whose soundtrack carries short, audible, self-describing
//! markers. A laptop microphone recording of the TV can then be decoded into
//! anchors "recording time r shows movie time m". This module owns only the
//! deterministic signal: payload, synthesis, mixing and detection. Clock
//! alignment from anchors belongs to the shared synchronizer (#223).
//!
//! Recipe `acoustic-marker.chirp-bfsk.v1` (deliberately audible; reliability
//! before aesthetics):
//! - Preamble: 250 ms linear up-chirp 1000→4000 Hz, then a 20 ms guard.
//! - Payload: 56 symbols of 30 ms. `1` = up-chirp 1500→3500 Hz, `0` = down-chirp.
//!   5 ms raised-cosine edges on every element.
//! - Bits: version (4) · export key (16) · movie deciseconds (20) · CRC-16/CCITT-FALSE
//!   over those 40 bits (16). The export key is a 16-bit hash of the review
//!   target digest: it distinguishes exports without carrying any identifier.
//! - Markers start exactly at movie times on a 0.1 s grid, so the encoded time
//!   is the precise preamble onset.
//!
//! Detection uses an FFT matched filter normalised by local energy, sub-sample
//! peak interpolation, per-symbol up/down correlation, then CRC, version and
//! export-key checks. Everything that fails a check is reported as rejected,
//! never as an anchor.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

pub const ACOUSTIC_MARKER_RECIPE: &str = "acoustic-marker.chirp-bfsk.v1";
pub const MARKER_VERSION: u8 = 1;
pub const MARKER_SAMPLE_RATE: u32 = 48_000;
pub const DETECTION_SAMPLE_RATE: u32 = 16_000;
const PREAMBLE_SECONDS: f64 = 0.25;
const GUARD_SECONDS: f64 = 0.02;
const SYMBOL_SECONDS: f64 = 0.03;
const EDGE_SECONDS: f64 = 0.005;
const PAYLOAD_BITS: usize = 56;
const PREAMBLE_LOW: f64 = 1000.0;
const PREAMBLE_HIGH: f64 = 4000.0;
const SYMBOL_LOW: f64 = 1500.0;
const SYMBOL_HIGH: f64 = 3500.0;
/// Normalised matched-filter score needed to attempt a decode. CRC, version
/// and key checks are the false-positive gate; this only bounds work.
const PREAMBLE_THRESHOLD: f64 = 0.18;
const MAX_TIME_DECISECONDS: u32 = (1 << 20) - 1;

/// Total marker length in seconds.
pub fn marker_duration_seconds() -> f64 {
    PREAMBLE_SECONDS + GUARD_SECONDS + SYMBOL_SECONDS * PAYLOAD_BITS as f64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkerPayload {
    pub version: u8,
    pub export_key: u16,
    pub movie_deciseconds: u32,
}

/// 16-bit export key from a review target digest (or any export identity).
pub fn export_key_for(identity: &str) -> u16 {
    let hash = blake3::hash(format!("{ACOUSTIC_MARKER_RECIPE}\0{identity}").as_bytes());
    u16::from_be_bytes([hash.as_bytes()[0], hash.as_bytes()[1]])
}

fn crc16(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for byte in bytes {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

impl MarkerPayload {
    fn data_bytes(&self) -> [u8; 5] {
        let value: u64 = (u64::from(self.version & 0xF) << 36)
            | (u64::from(self.export_key) << 20)
            | u64::from(self.movie_deciseconds & MAX_TIME_DECISECONDS);
        let bytes = value.to_be_bytes();
        [bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]]
    }

    pub fn to_bits(&self) -> Vec<bool> {
        let data = self.data_bytes();
        let crc = crc16(&data);
        let mut all = data.to_vec();
        all.extend_from_slice(&crc.to_be_bytes());
        all.iter()
            .flat_map(|byte| (0..8).rev().map(move |i| byte >> i & 1 == 1))
            .collect()
    }

    /// `None` when the CRC does not match.
    pub fn from_bits(bits: &[bool]) -> Option<Self> {
        if bits.len() != PAYLOAD_BITS {
            return None;
        }
        let mut bytes = [0_u8; 7];
        for (i, bit) in bits.iter().enumerate() {
            if *bit {
                bytes[i / 8] |= 1 << (7 - i % 8);
            }
        }
        let data = [bytes[0], bytes[1], bytes[2], bytes[3], bytes[4]];
        if crc16(&data) != u16::from_be_bytes([bytes[5], bytes[6]]) {
            return None;
        }
        let value = data.iter().fold(0_u64, |acc, b| (acc << 8) | u64::from(*b));
        Some(Self {
            version: ((value >> 36) & 0xF) as u8,
            export_key: ((value >> 20) & 0xFFFF) as u16,
            movie_deciseconds: (value & u64::from(MAX_TIME_DECISECONDS)) as u32,
        })
    }
}

fn chirp(rate: u32, seconds: f64, f0: f64, f1: f64) -> Vec<f32> {
    let n = (seconds * f64::from(rate)).round() as usize;
    let edge = (EDGE_SECONDS * f64::from(rate)) as usize;
    let k = (f1 - f0) / seconds;
    (0..n)
        .map(|i| {
            let t = i as f64 / f64::from(rate);
            let phase = 2.0 * PI * (f0 * t + 0.5 * k * t * t);
            let taper = if i < edge {
                0.5 - 0.5 * (PI * i as f64 / edge as f64).cos()
            } else if i + edge >= n {
                0.5 - 0.5 * (PI * (n - 1 - i) as f64 / edge as f64).cos()
            } else {
                1.0
            };
            (phase.sin() * taper) as f32
        })
        .collect()
}

fn preamble(rate: u32) -> Vec<f32> {
    chirp(rate, PREAMBLE_SECONDS, PREAMBLE_LOW, PREAMBLE_HIGH)
}

fn symbol(rate: u32, bit: bool) -> Vec<f32> {
    if bit {
        chirp(rate, SYMBOL_SECONDS, SYMBOL_LOW, SYMBOL_HIGH)
    } else {
        chirp(rate, SYMBOL_SECONDS, SYMBOL_HIGH, SYMBOL_LOW)
    }
}

/// One unit-amplitude marker waveform at `rate`.
pub fn synthesize_marker(payload: &MarkerPayload, rate: u32) -> Vec<f32> {
    let mut out = preamble(rate);
    out.extend(std::iter::repeat_n(
        0.0,
        (GUARD_SECONDS * f64::from(rate)).round() as usize,
    ));
    for bit in payload.to_bits() {
        out.extend(symbol(rate, bit));
    }
    out
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkerPlan {
    pub recipe: String,
    pub export_key: u16,
    pub interval_seconds: f64,
    pub first_marker_seconds: f64,
    pub marker_seconds: Vec<f64>,
    pub marker_duration_seconds: f64,
    pub level_dbfs: f64,
}

/// Markers every `interval` seconds from `first`, on a 0.1 s grid, each
/// fully inside the movie.
pub fn plan_markers(
    identity: &str,
    movie_duration_seconds: f64,
    interval_seconds: f64,
    first_marker_seconds: f64,
    level_dbfs: f64,
) -> Result<MarkerPlan> {
    ensure!(
        movie_duration_seconds.is_finite() && movie_duration_seconds > 0.0,
        "invalid input: movie duration"
    );
    ensure!(
        interval_seconds.is_finite() && (5.0..=600.0).contains(&interval_seconds),
        "invalid input: marker interval must be 5–600 seconds"
    );
    ensure!(
        first_marker_seconds.is_finite() && first_marker_seconds >= 0.0,
        "invalid input: first marker time"
    );
    ensure!(
        level_dbfs.is_finite() && (-40.0..=-3.0).contains(&level_dbfs),
        "invalid input: marker level must be -40 to -3 dBFS"
    );
    let duration = marker_duration_seconds();
    let mut times = Vec::new();
    let mut tenths = (first_marker_seconds * 10.0).round() as u64;
    let step = (interval_seconds * 10.0).round() as u64;
    while (tenths as f64) / 10.0 + duration <= movie_duration_seconds
        && tenths <= u64::from(MAX_TIME_DECISECONDS)
    {
        times.push(tenths as f64 / 10.0);
        tenths += step;
    }
    Ok(MarkerPlan {
        recipe: ACOUSTIC_MARKER_RECIPE.into(),
        export_key: export_key_for(identity),
        interval_seconds,
        first_marker_seconds,
        marker_seconds: times,
        marker_duration_seconds: duration,
        level_dbfs,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixReport {
    pub input_peak: f32,
    pub input_rms: f32,
    pub input_silent: bool,
    pub output_peak: f32,
    /// Uniform gain applied to the whole mix to avoid clipping (0 when none).
    pub gain_applied_db: f64,
    pub markers_written: usize,
}

/// Mix the plan's markers into interleaved 48 kHz PCM, all channels equally.
/// Never clips: if the mix would exceed full scale, the whole copy is scaled
/// uniformly and the applied gain is reported.
pub fn mix_markers(
    interleaved: &mut [f32],
    channels: usize,
    plan: &MarkerPlan,
) -> Result<MixReport> {
    ensure!((1..=8).contains(&channels), "invalid input: channel count");
    ensure!(
        interleaved.len().is_multiple_of(channels),
        "invalid input: interleaved length"
    );
    let frames = interleaved.len() / channels;
    let input_peak = interleaved.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    let input_rms = if interleaved.is_empty() {
        0.0
    } else {
        (interleaved.iter().map(|s| s * s).sum::<f32>() / interleaved.len() as f32).sqrt()
    };
    let amplitude = 10_f64.powf(plan.level_dbfs / 20.0) as f32;
    let mut written = 0;
    for seconds in &plan.marker_seconds {
        let payload = MarkerPayload {
            version: MARKER_VERSION,
            export_key: plan.export_key,
            movie_deciseconds: (seconds * 10.0).round() as u32,
        };
        let start = (seconds * f64::from(MARKER_SAMPLE_RATE)).round() as usize;
        let wave = synthesize_marker(&payload, MARKER_SAMPLE_RATE);
        ensure!(start + wave.len() <= frames, "marker exceeds movie audio");
        for (i, sample) in wave.iter().enumerate() {
            for c in 0..channels {
                interleaved[(start + i) * channels + c] += sample * amplitude;
            }
        }
        written += 1;
    }
    let peak = interleaved.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    let mut gain_db = 0.0;
    if peak > 0.98 {
        let gain = 0.98 / peak;
        interleaved.iter_mut().for_each(|s| *s *= gain);
        gain_db = 20.0 * f64::from(gain).log10();
    }
    Ok(MixReport {
        input_peak,
        input_rms,
        input_silent: input_peak < 1e-4,
        output_peak: interleaved.iter().fold(0.0_f32, |m, s| m.max(s.abs())),
        gain_applied_db: gain_db,
        markers_written: written,
    })
}

// ── Detection ───────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Complex {
    re: f64,
    im: f64,
}

fn fft(buffer: &mut [Complex], inverse: bool) {
    let n = buffer.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            buffer.swap(i, j);
        }
    }
    let mut length = 2;
    while length <= n {
        let angle = 2.0 * PI / length as f64 * if inverse { 1.0 } else { -1.0 };
        let (w_re, w_im) = (angle.cos(), angle.sin());
        for start in (0..n).step_by(length) {
            let (mut c_re, mut c_im) = (1.0, 0.0);
            for k in 0..length / 2 {
                let a = buffer[start + k];
                let b = buffer[start + k + length / 2];
                let t_re = b.re * c_re - b.im * c_im;
                let t_im = b.re * c_im + b.im * c_re;
                buffer[start + k] = Complex {
                    re: a.re + t_re,
                    im: a.im + t_im,
                };
                buffer[start + k + length / 2] = Complex {
                    re: a.re - t_re,
                    im: a.im - t_im,
                };
                let next = c_re * w_re - c_im * w_im;
                c_im = c_re * w_im + c_im * w_re;
                c_re = next;
            }
        }
        length <<= 1;
    }
    if inverse {
        for value in buffer.iter_mut() {
            value.re /= n as f64;
            value.im /= n as f64;
        }
    }
}

/// Cross-correlation `c[n] = Σ x[n+m]·t[m]` for every valid `n`, by overlap-save.
fn correlate(signal: &[f32], template: &[f32]) -> Vec<f64> {
    let m = template.len();
    if signal.len() < m {
        return Vec::new();
    }
    let valid = signal.len() - m + 1;
    let size = (4 * m).next_power_of_two();
    let step = size - m + 1;
    let mut kernel = vec![Complex { re: 0.0, im: 0.0 }; size];
    for (i, value) in template.iter().enumerate() {
        kernel[i] = Complex {
            re: f64::from(*value),
            im: 0.0,
        };
    }
    fft(&mut kernel, false);
    let mut out = vec![0.0; valid];
    let mut start = 0;
    while start < valid {
        let mut block = vec![Complex { re: 0.0, im: 0.0 }; size];
        for (i, slot) in block.iter_mut().enumerate() {
            if let Some(value) = signal.get(start + i) {
                slot.re = f64::from(*value);
            }
        }
        fft(&mut block, false);
        for (b, k) in block.iter_mut().zip(&kernel) {
            // Multiply by the conjugate kernel: correlation, not convolution.
            let re = b.re * k.re + b.im * k.im;
            let im = b.im * k.re - b.re * k.im;
            *b = Complex { re, im };
        }
        fft(&mut block, true);
        for i in 0..step.min(valid - start) {
            out[start + i] = block[i].re;
        }
        start += step;
    }
    out
}

fn dot(a: &[f32], b: &[f32]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum()
}

fn energy(a: &[f32]) -> f64 {
    a.iter().map(|x| f64::from(*x) * f64::from(*x)).sum()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkerAnchor {
    /// Preamble onset on the recording clock (sub-sample precision).
    pub recording_seconds: f64,
    /// Exact movie time encoded in the marker.
    pub movie_seconds: f64,
    /// `recording - movie`; consistent across anchors only if playback was continuous.
    pub offset_seconds: f64,
    pub preamble_score: f64,
    pub mean_bit_confidence: f64,
    pub min_bit_confidence: f64,
    pub snr_db: f64,
    /// ± half a detection sample plus the preamble's bandwidth resolution.
    pub uncertainty_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectedMarker {
    pub recording_seconds: f64,
    pub preamble_score: f64,
    /// `crc`, `version`, `exportKeyMismatch`, `truncated`, `duplicate`.
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkerDetectionReport {
    pub recipe: String,
    pub expected_export_key: u16,
    pub recording_seconds: f64,
    pub anchors: Vec<MarkerAnchor>,
    pub rejected: Vec<RejectedMarker>,
    pub limitations: Vec<String>,
}

const DETECTION_LIMITATIONS: &[&str] = &[
    "Anchors are evidence for the shared synchronizer (#223); they are not an alignment by themselves.",
    "A missing marker is not evidence of a pause or seek; inconsistent offsets indicate seeks, pauses or rewatching.",
    "Room delay from the TV speaker to the microphone (about 3 ms per metre) is included in recording time.",
];

/// Decode markers from mono PCM at `rate`. Only markers whose CRC, version
/// and export key all match become anchors.
pub fn detect_markers(
    pcm: &[f32],
    rate: u32,
    expected_export_key: u16,
) -> Result<MarkerDetectionReport> {
    ensure!(
        rate >= 8_000,
        "invalid input: sample rate too low for markers"
    );
    let signal = if rate == DETECTION_SAMPLE_RATE {
        pcm.to_vec()
    } else {
        resample(pcm, rate, DETECTION_SAMPLE_RATE)
    };
    let r = DETECTION_SAMPLE_RATE;
    let template = preamble(r);
    let template_norm = energy(&template).sqrt();
    let m = template.len();
    let raw = correlate(&signal, &template);
    // Local energy over each template-length window, by running sum.
    let mut window_energy = vec![0.0_f64; raw.len()];
    let mut running: f64 = signal[..m.min(signal.len())]
        .iter()
        .map(|x| f64::from(*x).powi(2))
        .sum();
    for (n, slot) in window_energy.iter_mut().enumerate() {
        *slot = running;
        if n + m < signal.len() {
            running += f64::from(signal[n + m]).powi(2) - f64::from(signal[n]).powi(2);
        }
    }
    let score: Vec<f64> = raw
        .iter()
        .zip(&window_energy)
        .map(|(c, e)| {
            if *e > 1e-12 {
                c / (e.sqrt() * template_norm)
            } else {
                0.0
            }
        })
        .collect();

    let symbol_len = (SYMBOL_SECONDS * f64::from(r)).round() as usize;
    let guard = (GUARD_SECONDS * f64::from(r)).round() as usize;
    let up = symbol(r, true);
    let down = symbol(r, false);
    let total = m + guard + symbol_len * PAYLOAD_BITS;
    let suppression = (0.5 * f64::from(r)) as usize;

    let mut anchors: Vec<MarkerAnchor> = Vec::new();
    let mut rejected = Vec::new();
    let mut n = 0;
    while n < score.len() {
        if score[n] < PREAMBLE_THRESHOLD {
            n += 1;
            continue;
        }
        // Local maximum within the suppression window.
        let end = (n + suppression).min(score.len());
        let peak = (n..end)
            .max_by(|a, b| score[*a].partial_cmp(&score[*b]).unwrap())
            .unwrap();
        let peak_score = score[peak];
        let refined = if peak > 0 && peak + 1 < score.len() {
            let (a, b, c) = (score[peak - 1], score[peak], score[peak + 1]);
            let denominator = a - 2.0 * b + c;
            if denominator.abs() > 1e-12 {
                peak as f64 + 0.5 * (a - c) / denominator
            } else {
                peak as f64
            }
        } else {
            peak as f64
        };
        let recording_seconds = refined / f64::from(r);
        n = peak + suppression;
        if peak + total > signal.len() {
            rejected.push(RejectedMarker {
                recording_seconds,
                preamble_score: peak_score,
                reason: "truncated".into(),
            });
            continue;
        }
        let mut bits = Vec::with_capacity(PAYLOAD_BITS);
        let mut confidences = Vec::with_capacity(PAYLOAD_BITS);
        for i in 0..PAYLOAD_BITS {
            let start = peak + m + guard + i * symbol_len;
            let window = &signal[start..start + symbol_len];
            let u = dot(window, &up);
            let d = dot(window, &down);
            bits.push(u > d);
            let denominator = u.abs() + d.abs();
            confidences.push(if denominator > 0.0 {
                (u.abs() - d.abs()).abs() / denominator
            } else {
                0.0
            });
        }
        let Some(payload) = MarkerPayload::from_bits(&bits) else {
            rejected.push(RejectedMarker {
                recording_seconds,
                preamble_score: peak_score,
                reason: "crc".into(),
            });
            continue;
        };
        if payload.version != MARKER_VERSION {
            rejected.push(RejectedMarker {
                recording_seconds,
                preamble_score: peak_score,
                reason: "version".into(),
            });
            continue;
        }
        if payload.export_key != expected_export_key {
            rejected.push(RejectedMarker {
                recording_seconds,
                preamble_score: peak_score,
                reason: "exportKeyMismatch".into(),
            });
            continue;
        }
        let marker_samples = &signal[peak..peak + total];
        let marker_energy = energy(marker_samples) / total as f64;
        let before_start = peak.saturating_sub(total);
        let noise = &signal[before_start..peak];
        let noise_energy = if noise.is_empty() {
            1e-12
        } else {
            energy(noise) / noise.len() as f64
        };
        let movie_seconds = f64::from(payload.movie_deciseconds) / 10.0;
        anchors.push(MarkerAnchor {
            recording_seconds,
            movie_seconds,
            offset_seconds: recording_seconds - movie_seconds,
            preamble_score: peak_score,
            mean_bit_confidence: confidences.iter().sum::<f64>() / confidences.len() as f64,
            min_bit_confidence: confidences.iter().copied().fold(f64::INFINITY, f64::min),
            snr_db: 10.0 * (marker_energy.max(1e-12) / noise_energy.max(1e-12)).log10(),
            uncertainty_seconds: 0.5 / f64::from(r) + 1.0 / (PREAMBLE_HIGH - PREAMBLE_LOW),
        });
    }
    Ok(MarkerDetectionReport {
        recipe: ACOUSTIC_MARKER_RECIPE.into(),
        expected_export_key,
        recording_seconds: pcm.len() as f64 / f64::from(rate),
        anchors,
        rejected,
        limitations: DETECTION_LIMITATIONS
            .iter()
            .map(|s| s.to_string())
            .collect(),
    })
}

/// Band-limited (Hann-windowed sinc) resampling.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = f64::from(to) / f64::from(from);
    let cutoff = ratio.min(1.0) * 0.9;
    let half = (16.0 / cutoff).ceil() as i64;
    let len = (input.len() as f64 * ratio).floor() as usize;
    (0..len)
        .map(|n| {
            let center = n as f64 / ratio;
            let base = center.floor() as i64;
            let (mut acc, mut weight) = (0.0, 0.0);
            for k in (base - half + 1)..=(base + half) {
                if k < 0 || k as usize >= input.len() {
                    continue;
                }
                let x = center - k as f64;
                let sinc = if x.abs() < 1e-9 {
                    1.0
                } else {
                    (PI * x * cutoff).sin() / (PI * x * cutoff)
                };
                let w = sinc * (0.5 + 0.5 * (PI * x / half as f64).cos()) * cutoff;
                acc += f64::from(input[k as usize]) * w;
                weight += w;
            }
            if weight.abs() > 1e-12 {
                (acc / weight) as f32
            } else {
                0.0
            }
        })
        .collect()
}

// ── Marked review copy ─────────────────────────────────────────────────

pub const MARKED_REVIEW_COPY_SCHEMA: &str = "marked-review-copy.v1";

#[derive(Debug, Clone)]
pub struct MarkedCopyRequest<'a> {
    pub handoff: &'a crate::ReviewTargetHandoff,
    /// The verified master movie on the owning host. Its BLAKE3 and length
    /// must equal the handoff's; it is only ever read.
    pub movie: &'a std::path::Path,
    /// Create-only destination; refused if it exists.
    pub output: &'a std::path::Path,
    pub interval_seconds: f64,
    pub first_marker_seconds: f64,
    pub level_dbfs: f64,
    pub ffmpeg: &'a std::path::Path,
    pub ffprobe: &'a std::path::Path,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MovieIdentity {
    pub blake3: String,
    pub byte_length: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PictureStreamIdentity {
    pub codec: String,
    pub time_base: String,
    pub start_pts: Option<i64>,
    pub frame_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkedCopyVerification {
    pub expected_markers: usize,
    pub detected_markers: usize,
    pub offset_spread_seconds: f64,
    pub audio_duration_seconds: f64,
    pub picture_stream_matches_master: bool,
}

/// Lineage of a marked review copy. Path-free; safe to keep beside the copy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkedReviewCopyManifest {
    pub schema: String,
    pub recipe: String,
    pub purpose: String,
    pub project_id: String,
    pub revision_id: String,
    pub viewed_artifact_id: String,
    pub target_digest: String,
    pub master: MovieIdentity,
    pub marked: MovieIdentity,
    pub master_picture: PictureStreamIdentity,
    pub marked_picture: PictureStreamIdentity,
    pub encode: String,
    pub plan: MarkerPlan,
    pub mix: MixReport,
    pub verification: MarkedCopyVerification,
    pub limitations: Vec<String>,
}

/// BLAKE3 and byte length of a file, streamed.
pub fn file_identity(path: &std::path::Path) -> Result<MovieIdentity> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 1 << 20];
    let mut length = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        length += read as u64;
    }
    Ok(MovieIdentity {
        blake3: hasher.finalize().to_hex().to_string(),
        byte_length: length,
    })
}

fn picture_identity(
    ffprobe: &std::path::Path,
    movie: &std::path::Path,
) -> Result<PictureStreamIdentity> {
    let output = std::process::Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,time_base,start_pts,nb_frames",
            "-of",
            "json",
        ])
        .arg(movie)
        .output()?;
    ensure!(
        output.status.success(),
        "media tool failed: ffprobe could not read the movie"
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let stream = value["streams"]
        .get(0)
        .ok_or_else(|| anyhow::anyhow!("invalid input: movie has no video stream"))?;
    Ok(PictureStreamIdentity {
        codec: stream["codec_name"].as_str().unwrap_or_default().into(),
        time_base: stream["time_base"].as_str().unwrap_or_default().into(),
        start_pts: stream["start_pts"].as_i64(),
        frame_count: stream["nb_frames"].as_str().and_then(|v| v.parse().ok()),
    })
}

fn decode_audio(
    ffmpeg: &std::path::Path,
    movie: &std::path::Path,
    rate: u32,
    channels: u32,
) -> Result<Vec<f32>> {
    let output = std::process::Command::new(ffmpeg)
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(movie)
        .args([
            "-map",
            "0:a:0",
            "-f",
            "f32le",
            "-ac",
            &channels.to_string(),
            "-ar",
            &rate.to_string(),
            "-",
        ])
        .output()?;
    ensure!(
        output.status.success(),
        "media tool failed: ffmpeg could not decode the movie soundtrack"
    );
    Ok(output
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

/// Write a separate, create-only marked review copy of the exact verified
/// master. Video is stream-copied; only the copy's soundtrack carries
/// markers. The master and every editorial artifact are only read.
pub fn export_marked_review_copy(
    request: &MarkedCopyRequest<'_>,
) -> Result<MarkedReviewCopyManifest> {
    let handoff = request.handoff;
    handoff.validate()?;
    ensure!(
        !request.output.exists(),
        "invalid input: output already exists; marked copies are create-only"
    );
    ensure!(
        request.output.extension().and_then(|e| e.to_str()) == Some("mp4"),
        "invalid input: output must be an .mp4 file"
    );
    let master = file_identity(request.movie)?;
    ensure!(
        master.blake3 == handoff.target.movie.blake3
            && master.byte_length == handoff.target.movie.byte_length,
        "invalid input: movie bytes do not match the verified review target"
    );
    let plan = plan_markers(
        &handoff.target_digest,
        handoff.target.movie.duration_seconds,
        request.interval_seconds,
        request.first_marker_seconds,
        request.level_dbfs,
    )?;
    ensure!(
        !plan.marker_seconds.is_empty(),
        "invalid input: the movie is too short for any marker"
    );
    let channels = handoff.target.movie.audio_channels.max(1);
    let mut audio = decode_audio(request.ffmpeg, request.movie, MARKER_SAMPLE_RATE, channels)?;
    let master_seconds = audio.len() as f64 / f64::from(channels) / f64::from(MARKER_SAMPLE_RATE);
    ensure!(
        (master_seconds - handoff.target.movie.duration_seconds).abs() <= 0.25,
        "invalid input: decoded soundtrack length disagrees with the verified movie duration"
    );
    let mix = mix_markers(&mut audio, channels as usize, &plan)?;

    let directory = request
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let staging_audio = directory.join(format!(".marked-{}.f32", uuid::Uuid::new_v4().simple()));
    let staging_movie = directory.join(format!(".marked-{}.mp4", uuid::Uuid::new_v4().simple()));
    std::fs::write(
        &staging_audio,
        audio
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect::<Vec<u8>>(),
    )?;
    let encode = std::process::Command::new(request.ffmpeg)
        .args(["-v", "error", "-nostdin", "-y", "-i"])
        .arg(request.movie)
        .args([
            "-f",
            "f32le",
            "-ar",
            "48000",
            "-ac",
            &channels.to_string(),
            "-i",
        ])
        .arg(&staging_audio)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-map_metadata",
            "-1",
            "-movflags",
            "+faststart",
        ])
        .arg(&staging_movie)
        .status();
    let _ = std::fs::remove_file(&staging_audio);
    match encode {
        Ok(status) if status.success() => {}
        _ => {
            let _ = std::fs::remove_file(&staging_movie);
            anyhow::bail!("media tool failed: ffmpeg could not write the marked copy");
        }
    }
    let result = (|| -> Result<MarkedReviewCopyManifest> {
        let decoded = decode_audio(request.ffmpeg, &staging_movie, DETECTION_SAMPLE_RATE, 1)?;
        let detection = detect_markers(&decoded, DETECTION_SAMPLE_RATE, plan.export_key)?;
        let offsets: Vec<f64> = detection.anchors.iter().map(|a| a.offset_seconds).collect();
        let spread = offsets.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            - offsets.iter().copied().fold(f64::INFINITY, f64::min);
        let detected: Vec<f64> = detection.anchors.iter().map(|a| a.movie_seconds).collect();
        ensure!(
            detected == plan.marker_seconds && spread.is_finite() && spread < 0.002,
            "marked copy failed self-verification: markers were not all recovered consistently"
        );
        let master_picture = picture_identity(request.ffprobe, request.movie)?;
        let marked_picture = picture_identity(request.ffprobe, &staging_movie)?;
        let picture_matches = master_picture == marked_picture;
        ensure!(
            picture_matches,
            "marked copy failed self-verification: picture stream identity changed"
        );
        let audio_seconds = decoded.len() as f64 / f64::from(DETECTION_SAMPLE_RATE);
        Ok(MarkedReviewCopyManifest {
            schema: MARKED_REVIEW_COPY_SCHEMA.into(),
            recipe: ACOUSTIC_MARKER_RECIPE.into(),
            purpose: "TV review only. Not an editorial deliverable: its soundtrack contains audible markers.".into(),
            project_id: handoff.target.project_id.clone(),
            revision_id: handoff.target.revision_id.clone(),
            viewed_artifact_id: handoff.target.viewed_artifact_id.clone(),
            target_digest: handoff.target_digest.clone(),
            master,
            marked: MovieIdentity { blake3: String::new(), byte_length: 0 },
            master_picture,
            marked_picture,
            encode: "video stream copy; soundtrack decoded at 48 kHz, markers mixed, AAC 192 kb/s; container metadata stripped".into(),
            plan: plan.clone(),
            mix: mix.clone(),
            verification: MarkedCopyVerification {
                expected_markers: plan.marker_seconds.len(),
                detected_markers: detection.anchors.len(),
                offset_spread_seconds: spread,
                audio_duration_seconds: audio_seconds,
                picture_stream_matches_master: picture_matches,
            },
            limitations: vec![
                "Self-verification decodes the file; it does not establish YouTube processing, TV playback or room acoustics.".into(),
                "The YouTube video of this copy is a declaration; nothing proves YouTube serves these bytes.".into(),
                "Markers are audible by design (v1); masking/inaudibility is not claimed.".into(),
            ],
        })
    })();
    let mut manifest = match result {
        Ok(manifest) => manifest,
        Err(error) => {
            let _ = std::fs::remove_file(&staging_movie);
            return Err(error);
        }
    };
    if request.output.exists() {
        let _ = std::fs::remove_file(&staging_movie);
        anyhow::bail!("invalid input: output appeared during export; nothing was overwritten");
    }
    std::fs::rename(&staging_movie, request.output)?;
    manifest.marked = file_identity(request.output)?;
    Ok(manifest)
}

/// Read a mono/stereo 16-bit or float WAV, downmixed to mono.
pub fn read_wav_mono(path: &std::path::Path) -> Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let scale = (1_i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<std::result::Result<_, _>>()?
        }
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<std::result::Result<_, _>>()?,
    };
    Ok((
        samples
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect(),
        spec.sample_rate,
    ))
}

#[cfg(test)]
#[path = "acoustic_markers_tests.rs"]
mod tests;
