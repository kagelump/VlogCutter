//! Candidate VlogCutter implementation: local, content-bound landmark index.
use crate::{full_file_blake3, tv_review_sync::SyncMovieClock};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
};

pub const RECIPE_ID_512: &str = "source-audio.landmarks.fft512-hop128-pairs.v1";
pub const RECIPE_ID_1024: &str = "source-audio.landmarks.fft1024-hop128-pairs.v1";
pub const RATE: u32 = 11_025;
pub const HOP: usize = 128;
pub const TIME_BIN_SECONDS: f64 = 1.0;
const MAX_MOVIE_SECONDS: f64 = 3_600.0;
const MAX_PCM_BYTES: usize = 160 * 1024 * 1024;
const MAX_INDEX_BYTES: usize = 64 * 1024 * 1024;
const MAX_POSTINGS: usize = 50_000_000;
const MAX_VOTE_WORK: usize = 50_000_000;
const MAX_VOTE_CELLS: usize = 2_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Recipe {
    pub id: String,
    pub sample_rate: u32,
    pub fft_size: u16,
    pub hop_size: u16,
    pub hashes_per_second: u16,
    pub fanout: u8,
}
impl Default for Recipe {
    fn default() -> Self {
        Self {
            id: RECIPE_ID_512.into(),
            sample_rate: RATE,
            fft_size: 512,
            hop_size: 128,
            hashes_per_second: 100,
            fanout: 3,
        }
    }
}
impl Recipe {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.id
                == if self.fft_size == 1024 {
                    RECIPE_ID_1024
                } else {
                    RECIPE_ID_512
                }
                && self.sample_rate == RATE
                && (self.fft_size == 512 || self.fft_size == 1024)
                && self.hop_size == 128
                && (20..=100).contains(&self.hashes_per_second)
                && (1..=3).contains(&self.fanout),
            "invalid input: unsupported landmark recipe"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Index {
    pub schema_version: u32,
    pub recipe: Recipe,
    pub movie: SyncMovieClock,
    pub pcm_digest: String,
    pub index_digest: String,
    pub sample_count: u64,
    pub frame_count: u32,
    /// Exact postings are kept even when a hash is common; search downweights saturation.
    pub postings: BTreeMap<u32, Vec<u32>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OffsetEvidence {
    pub time_bin: u32,
    pub offset_bin: i32,
    pub offset_seconds_from_capture_start: f64,
    pub independent_anchors: u32,
    pub supporting_seconds: u32,
    pub frequency_diversity: u32,
    pub rarity_weight: f64,
    pub saturated_anchors: u32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Search {
    pub schema_version: u32,
    pub recipe: Recipe,
    #[serde(default)]
    pub search_policy: SearchPolicy,
    pub movie_digest: String,
    pub index_digest: String,
    pub query_pcm_digest: String,
    pub query_sample_count: u64,
    pub query_landmark_count: u32,
    pub matched_landmark_count: u32,
    pub candidate_pair_count: u64,
    pub complete: bool,
    pub offsets: Vec<OffsetEvidence>,
}

/// Retrieval-weighting controls for explicit calibration ablations. Defaults keep
/// correlated votes capped and discount common hashes in the full movie index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchPolicy {
    pub rarity_weighting: bool,
    pub saturation_downweighting: bool,
    pub independence_caps: bool,
}
impl Default for SearchPolicy {
    fn default() -> Self {
        Self {
            rarity_weighting: true,
            saturation_downweighting: true,
            independence_caps: true,
        }
    }
}

#[derive(Debug, Clone)]
struct Peak {
    bin: u16,
    magnitude: f32,
}
#[derive(Debug, Clone)]
struct Landmark {
    hash: u32,
    frame: u32,
    frequencies: [u16; 2],
}
#[derive(Default)]
struct VoteCell {
    anchors: HashSet<u32>,
    seconds: HashSet<u32>,
    frequencies: HashSet<u16>,
    saturated: HashSet<u32>,
    weight: f64,
}

impl Index {
    pub fn from_movie(
        movie: SyncMovieClock,
        path: &Path,
        recipe: Recipe,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Self> {
        recipe.validate()?;
        validate_movie(&movie)?;
        ensure!(
            movie.duration_pts as f64 / movie.ticks_per_second() <= MAX_MOVIE_SECONDS,
            "invalid input: bounded source-audio reference duration required"
        );
        let identity = full_file_blake3(path, cancel)?;
        ensure!(
            identity.hex_digest == movie.movie_digest,
            "invalid input: final movie digest mismatch"
        );
        let mut command = crate::tool_command("ffmpeg");
        command
            .args([
                "-v",
                "error",
                "-nostdin",
                "-protocol_whitelist",
                "file,pipe",
                "-i",
            ])
            .arg(path)
            .args([
                "-map",
                "0:a:0",
                "-vn",
                "-af",
                "aresample=11025:async=1:first_pts=0",
                "-ac",
                "2",
                "-ar",
                "11025",
                "-f",
                "s16le",
                "pipe:1",
            ]);
        let output = crate::stall::output_with_timeout_cancellation_and_limit(
            &mut command,
            "source-audio full reference decode",
            std::time::Duration::from_secs(1800),
            &|| cancel(),
            MAX_PCM_BYTES,
        )?;
        ensure!(
            output.status.success(),
            "media tool failed: source-audio reference decode"
        );
        let pcm = stereo_to_mono(&output.stdout)?;
        let after = std::fs::metadata(path)?;
        ensure!(
            after.len() == identity.pre_read_size
                && full_file_blake3(path, cancel)?.hex_digest == movie.movie_digest,
            "input_changed"
        );
        Self::from_pcm(movie, &pcm, recipe, cancel)
    }
    pub fn from_pcm(
        movie: SyncMovieClock,
        pcm: &[i16],
        recipe: Recipe,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Self> {
        recipe.validate()?;
        validate_movie(&movie)?;
        ensure!(
            !pcm.is_empty() && pcm.len() <= RATE as usize * 60 * 60 * 4,
            "invalid input: bounded reference PCM required"
        );
        let landmarks = extract(pcm, &recipe, cancel)?;
        let mut postings: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for (i, l) in landmarks.iter().enumerate() {
            if i % 4096 == 0 && cancel() {
                anyhow::bail!("source-audio indexing cancelled");
            }
            postings.entry(l.hash).or_default().push(l.frame);
        }
        ensure!(
            postings.values().map(Vec::len).sum::<usize>() <= MAX_POSTINGS,
            "source-audio index posting budget exceeded"
        );
        let frame_count = frame_count(pcm.len(), usize::from(recipe.fft_size))?;
        let mut index = Self {
            schema_version: 1,
            recipe,
            movie,
            pcm_digest: pcm_hash(pcm),
            index_digest: String::new(),
            sample_count: pcm.len() as u64,
            frame_count,
            postings,
        };
        index.index_digest = index.digest()?;
        index.validate()?;
        Ok(index)
    }
    pub fn to_json(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        ensure!(
            bytes.len() <= MAX_INDEX_BYTES,
            "source-audio index exceeds 64 MiB"
        );
        Ok(bytes)
    }
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= MAX_INDEX_BYTES,
            "invalid input: source-audio index exceeds 64 MiB"
        );
        let index: Self = serde_json::from_slice(bytes)?;
        index.validate()?;
        Ok(index)
    }
    pub fn validate(&self) -> Result<()> {
        self.recipe.validate()?;
        validate_movie(&self.movie)?;
        ensure!(
            self.schema_version == 1 && self.sample_count > 0 && self.frame_count > 0,
            "invalid input: source-audio index version or length"
        );
        ensure!(
            self.sample_count <= (RATE as u64) * 60 * 60 * 4,
            "invalid input: source-audio index sample bound"
        );
        ensure!(
            self.frame_count
                == frame_count(
                    self.sample_count as usize,
                    usize::from(self.recipe.fft_size)
                )?,
            "invalid input: source-audio frame count mismatch"
        );
        ensure!(
            valid_digest(&self.pcm_digest) && valid_digest(&self.index_digest),
            "invalid input: source-audio index identity"
        );
        let mut total = 0usize;
        for ps in self.postings.values() {
            total = total.saturating_add(ps.len());
            ensure!(
                ps.windows(2).all(|p| p[0] < p[1]) && ps.iter().all(|p| *p < self.frame_count),
                "invalid input: source-audio posting order/bounds"
            );
        }
        ensure!(
            total <= MAX_POSTINGS && self.digest()? == self.index_digest,
            "invalid input: source-audio index digest or posting budget"
        );
        Ok(())
    }
    fn digest(&self) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(&(
            &self.schema_version,
            &self.recipe,
            &self.movie,
            &self.pcm_digest,
            self.sample_count,
            self.frame_count,
            &self.postings,
        ))?)
        .to_hex()
        .to_string())
    }
    pub fn search_pcm(&self, query: &[i16], cancel: &dyn Fn() -> bool) -> Result<Search> {
        self.search_pcm_with_policy(query, SearchPolicy::default(), cancel)
    }
    pub fn search_pcm_with_policy(
        &self,
        query: &[i16],
        search_policy: SearchPolicy,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Search> {
        self.validate()?;
        ensure!(
            !query.is_empty() && query.len() <= RATE as usize * 60 * 60,
            "invalid input: bounded query PCM required"
        );
        let landmarks = extract(query, &self.recipe, cancel)?;
        let total = self.postings.values().map(Vec::len).sum::<usize>();
        let mut votes: HashMap<(u32, i32), VoteCell> = HashMap::new();
        let mut work = 0usize;
        let mut pairs = 0u64;
        let mut matched = 0u32;
        for (i, l) in landmarks.iter().enumerate() {
            if i % 2048 == 0 && cancel() {
                anyhow::bail!("source-audio matching cancelled");
            }
            let Some(ps) = self.postings.get(&l.hash) else {
                continue;
            };
            if ps.is_empty() {
                continue;
            }
            matched += 1;
            let rarity = if search_policy.rarity_weighting {
                ((total as f64 + 1.0) / (ps.len() as f64 + 1.0))
                    .ln_1p()
                    .max(0.02)
            } else {
                1.0
            };
            let weight = rarity
                * if !search_policy.saturation_downweighting {
                    1.0
                } else if ps.len() > 1000 {
                    0.05
                } else if ps.len() > 250 {
                    0.2
                } else {
                    1.0
                };
            let time =
                (l.frame as f64 * HOP as f64 / RATE as f64 / TIME_BIN_SECONDS).floor() as u32;
            let mut anchor_offsets = HashSet::new();
            for &rf in ps {
                work += 1;
                ensure!(
                    work <= MAX_VOTE_WORK,
                    "source-audio candidate-work budget exceeded; search incomplete"
                );
                let delta = (rf as i64 - l.frame as i64) as f64 * HOP as f64 / RATE as f64;
                let ob = (delta / 0.08).round() as i32;
                if !anchor_offsets.insert(ob) {
                    continue;
                }
                pairs += 1;
                let key = (time, ob);
                if votes.len() >= MAX_VOTE_CELLS && !votes.contains_key(&key) {
                    anyhow::bail!("source-audio offset-vote budget exceeded; search incomplete");
                }
                let cell = votes.entry(key).or_default();
                let new_anchor = cell.anchors.insert(l.frame);
                if new_anchor {
                    cell.seconds
                        .insert((l.frame as f64 * HOP as f64 / RATE as f64).floor() as u32);
                    cell.frequencies.extend(l.frequencies);
                }
                if new_anchor || !search_policy.independence_caps {
                    cell.weight += weight;
                }
                if ps.len() > 250 {
                    cell.saturated.insert(l.frame);
                }
            }
        }
        let mut offsets: Vec<_> = votes
            .into_iter()
            .map(|((time_bin, offset_bin), v)| OffsetEvidence {
                time_bin,
                offset_bin,
                offset_seconds_from_capture_start: f64::from(offset_bin) * 0.08,
                independent_anchors: v.anchors.len() as u32,
                supporting_seconds: v.seconds.len() as u32,
                frequency_diversity: v.frequencies.len() as u32,
                rarity_weight: v.weight,
                saturated_anchors: v.saturated.len() as u32,
            })
            .collect();
        offsets.sort_by(|a, b| {
            a.time_bin
                .cmp(&b.time_bin)
                .then(b.rarity_weight.total_cmp(&a.rarity_weight))
                .then(a.offset_bin.cmp(&b.offset_bin))
        });
        Ok(Search {
            schema_version: 1,
            recipe: self.recipe.clone(),
            search_policy,
            movie_digest: self.movie.movie_digest.clone(),
            index_digest: self.index_digest.clone(),
            query_pcm_digest: pcm_hash(query),
            query_sample_count: query.len() as u64,
            query_landmark_count: landmarks.len() as u32,
            matched_landmark_count: matched,
            candidate_pair_count: pairs,
            complete: true,
            offsets,
        })
    }
}

pub fn decode_wav(path: &Path, cancel: &dyn Fn() -> bool) -> Result<Vec<i16>> {
    let mut command = crate::tool_command("ffmpeg");
    command
        .args([
            "-v",
            "error",
            "-nostdin",
            "-protocol_whitelist",
            "file,pipe",
            "-i",
        ])
        .arg(path)
        .args([
            "-map",
            "0:a:0",
            "-vn",
            "-af",
            "aresample=11025:async=1:first_pts=0",
            "-ac",
            "2",
            "-ar",
            "11025",
            "-f",
            "s16le",
            "pipe:1",
        ]);
    let output = crate::stall::output_with_timeout_cancellation_and_limit(
        &mut command,
        "source-audio capture decode",
        std::time::Duration::from_secs(600),
        &|| cancel(),
        MAX_PCM_BYTES,
    )?;
    ensure!(
        output.status.success(),
        "media tool failed: source-audio capture decode"
    );
    stereo_to_mono(&output.stdout)
}
fn stereo_to_mono(bytes: &[u8]) -> Result<Vec<i16>> {
    ensure!(
        bytes.len().is_multiple_of(4),
        "invalid input: malformed stereo PCM"
    );
    let stereo: Vec<i16> = bytes
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let (mut lp, mut rp, mut mp) = (0f64, 0f64, 0f64);
    for p in stereo.chunks_exact(2) {
        let a = f64::from(p[0]);
        let b = f64::from(p[1]);
        lp += a * a;
        rp += b * b;
        mp += ((a + b) * 0.5).powi(2);
    }
    let n = (stereo.len() / 2).max(1) as f64;
    Ok(if lp.max(rp) > 0.0 && mp / n < lp.max(rp) / n * 0.04 {
        let c = if lp >= rp { 0 } else { 1 };
        stereo.chunks_exact(2).map(|p| p[c]).collect()
    } else {
        stereo
            .chunks_exact(2)
            .map(|p| ((i32::from(p[0]) + i32::from(p[1])) / 2) as i16)
            .collect()
    })
}

fn extract(pcm: &[i16], recipe: &Recipe, cancel: &dyn Fn() -> bool) -> Result<Vec<Landmark>> {
    let fft_size = usize::from(recipe.fft_size);
    let n = frame_count(pcm.len(), fft_size)? as usize;
    if n < 3 {
        return Ok(Vec::new());
    }
    let mut peaks = vec![Vec::new()];
    let mut prev = spectrum(pcm, 0, fft_size);
    let mut cur = spectrum(pcm, 1, fft_size);
    for frame in 1..n - 1 {
        if frame % 512 == 0 && cancel() {
            anyhow::bail!("source-audio landmark extraction cancelled");
        }
        let next = spectrum(pcm, frame + 1, fft_size);
        let end_bin = ((5_000usize * fft_size) / RATE as usize).min(fft_size / 2 - 1);
        let floor = median(&cur[4..end_bin]).max(1e-5) * 2.2;
        let mut row = Vec::new();
        for f in 4..end_bin {
            let x = cur[f];
            if x >= floor && x >= prev[f] && x >= next[f] && x >= cur[f - 1] && x >= cur[f + 1] {
                row.push(Peak {
                    bin: f as u16,
                    magnitude: x,
                });
            }
        }
        row.sort_by(|a, b| b.magnitude.total_cmp(&a.magnitude).then(a.bin.cmp(&b.bin)));
        row.truncate(8);
        peaks.push(row);
        prev = cur;
        cur = next;
    }
    peaks.push(Vec::new());
    let mut quota = 0f64;
    let mut out = Vec::new();
    let mut unique = HashSet::new();
    let fps = RATE as f64 / HOP as f64;
    for a in 0..peaks.len() {
        if a % 512 == 0 && cancel() {
            anyhow::bail!("source-audio landmark pairing cancelled");
        }
        quota += f64::from(recipe.hashes_per_second) / fps;
        let budget = quota.floor() as usize;
        quota -= budget as f64;
        if budget == 0 || peaks[a].is_empty() {
            continue;
        }
        let mut pairs = Vec::new();
        for p in peaks[a].iter().take(3) {
            let hi = (a + 173).min(peaks.len());
            let mut targets = Vec::new();
            for t in (a + 9..hi).step_by(4) {
                for q in &peaks[t] {
                    if q.bin != p.bin {
                        targets.push((
                            p.magnitude + q.magnitude,
                            hash_pair(p.bin, q.bin, (t - a) as u16),
                            p.bin,
                            q.bin,
                        ));
                    }
                }
            }
            targets.sort_by(|x, y| y.0.total_cmp(&x.0).then(x.1.cmp(&y.1)));
            targets.truncate(usize::from(recipe.fanout));
            pairs.extend(targets);
        }
        pairs.sort_by(|x, y| y.0.total_cmp(&x.0).then(x.1.cmp(&y.1)));
        for (_, hash, f1, f2) in pairs.into_iter().take(budget.min(3)) {
            let frame = a as u32;
            if unique.insert((frame, hash)) {
                out.push(Landmark {
                    hash,
                    frame,
                    frequencies: [f1, f2],
                });
            }
        }
    }
    Ok(out)
}
fn spectrum(pcm: &[i16], frame: usize, fft_size: usize) -> Vec<f32> {
    let mut re = vec![0f64; fft_size];
    let mut im = vec![0f64; fft_size];
    let start = frame * HOP;
    for (i, sample) in re.iter_mut().enumerate() {
        let x = pcm.get(start + i).copied().unwrap_or(0) as f64 / 32768.0;
        *sample =
            x * (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (fft_size - 1) as f64).cos());
    }
    fft(&mut re, &mut im);
    (0..=fft_size / 2)
        .map(|i| re[i].hypot(im[i]) as f32)
        .collect()
}
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut b = n >> 1;
        while j & b != 0 {
            j ^= b;
            b >>= 1
        }
        j ^= b;
        if i < j {
            re.swap(i, j);
            im.swap(i, j)
        }
    }
    let mut len = 2;
    while len <= n {
        let a = -2.0 * std::f64::consts::PI / len as f64;
        let (wr0, wi0) = (a.cos(), a.sin());
        for s in (0..n).step_by(len) {
            let (mut wr, mut wi) = (1.0, 0.0);
            for k in 0..len / 2 {
                let u = s + k;
                let v = u + len / 2;
                let (tr, ti) = (wr * re[v] - wi * im[v], wr * im[v] + wi * re[v]);
                re[v] = re[u] - tr;
                im[v] = im[u] - ti;
                re[u] += tr;
                im[u] += ti;
                let nw = wr * wr0 - wi * wi0;
                wi = wr * wi0 + wi * wr0;
                wr = nw;
            }
        }
        len <<= 1
    }
}
fn median(v: &[f32]) -> f32 {
    let mut x = v.to_vec();
    let mid = x.len() / 2;
    x.select_nth_unstable_by(mid, f32::total_cmp);
    x[mid]
}
fn hash_pair(a: u16, b: u16, d: u16) -> u32 {
    (((a / 2).min(255) as u32) << 16)
        | (((b / 2).min(255) as u32) << 8)
        | (d.div_ceil(2).min(255) as u32)
}
fn frame_count(samples: usize, fft_size: usize) -> Result<u32> {
    ensure!(
        samples >= fft_size,
        "invalid input: source audio shorter than FFT"
    );
    u32::try_from(1 + (samples - fft_size) / HOP).context("source-audio frame count overflow")
}
fn pcm_hash(pcm: &[i16]) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"source-audio.pcm.s16le.11025.v1\0");
    for x in pcm {
        h.update(&x.to_le_bytes());
    }
    h.finalize().to_hex().to_string()
}
fn valid_digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn validate_movie(movie: &SyncMovieClock) -> Result<()> {
    ensure!(
        movie.time_base_numerator > 0
            && movie.time_base_denominator > 0
            && movie.duration_pts > 0
            && valid_digest(&movie.movie_digest)
            && movie.duration_pts as f64
                / (movie.time_base_denominator as f64 / movie.time_base_numerator as f64)
                <= MAX_MOVIE_SECONDS,
        "invalid input: source-audio movie clock"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn movie() -> SyncMovieClock {
        SyncMovieClock {
            movie_digest: "a".repeat(64),
            time_base_numerator: 1,
            time_base_denominator: 1000,
            origin_pts: 0,
            duration_pts: 100_000,
        }
    }
    fn tone(seconds: usize) -> Vec<i16> {
        (0..RATE as usize * seconds)
            .map(|i| {
                ((0.6 * (i as f64 * 233.0 * 2.0 * std::f64::consts::PI / RATE as f64).sin()
                    + 0.3 * (i as f64 * 877.0 * 2.0 * std::f64::consts::PI / RATE as f64).sin())
                    * 12000.0) as i16
            })
            .collect()
    }
    #[test]
    fn index_is_versioned_content_bound_and_round_trips() {
        let r = Recipe {
            hashes_per_second: 20,
            ..Recipe::default()
        };
        let x = Index::from_pcm(movie(), &tone(8), r, &|| false).unwrap();
        let bytes = x.to_json().unwrap();
        assert_eq!(Index::from_json(&bytes).unwrap(), x);
        let mut bad = x;
        bad.postings.clear();
        assert!(bad.validate().is_err());
    }
    #[test]
    fn higher_resolution_recipe_changes_the_immutable_index_identity() {
        let pcm = tone(8);
        let low = Index::from_pcm(movie(), &pcm, Recipe::default(), &|| false).unwrap();
        let high_recipe = Recipe {
            fft_size: 1024,
            id: RECIPE_ID_1024.into(),
            ..Recipe::default()
        };
        let high = Index::from_pcm(movie(), &pcm, high_recipe, &|| false).unwrap();
        assert_ne!(low.index_digest, high.index_digest);
        assert_eq!(high.recipe.fft_size, 1024);
        assert_ne!(low.recipe.id, high.recipe.id);
    }
    #[test]
    fn repeated_audio_retains_competing_offset_modes() {
        let r = Recipe {
            hashes_per_second: 50,
            ..Recipe::default()
        };
        let p = tone(8);
        let mut repeated = p.clone();
        repeated.extend(vec![0; RATE as usize * 2]);
        repeated.extend(p.clone());
        let x = Index::from_pcm(movie(), &repeated, r, &|| false).unwrap();
        let s = x.search_pcm(&p, &|| false).unwrap();
        let offsets: HashSet<_> = s
            .offsets
            .iter()
            .filter(|v| v.independent_anchors > 4)
            .map(|v| v.offset_bin)
            .collect();
        assert!(offsets.len() >= 2);
    }
    #[test]
    fn retrieval_ablation_controls_are_recorded_and_change_duplicate_vote_weight() {
        let mut state = 0x1234_5678_u32;
        let p: Vec<i16> = (0..RATE as usize * 8)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((state >> 16) as i16) / 3
            })
            .collect();
        let mut repeated = p.clone();
        repeated.extend(vec![0; RATE as usize * 2]);
        repeated.extend(p.clone());
        let recipe = Recipe {
            hashes_per_second: 100,
            ..Recipe::default()
        };
        let index = Index::from_pcm(movie(), &repeated, recipe, &|| false).unwrap();
        let capped = index.search_pcm(&p, &|| false).unwrap();
        let uncapped_policy = SearchPolicy {
            independence_caps: false,
            ..SearchPolicy::default()
        };
        let uncapped = index
            .search_pcm_with_policy(&p, uncapped_policy, &|| false)
            .unwrap();
        assert_eq!(uncapped.search_policy, uncapped_policy);
        assert!(uncapped.offsets.iter().any(|alt| {
            capped
                .offsets
                .iter()
                .find(|other| other.time_bin == alt.time_bin && other.offset_bin == alt.offset_bin)
                .is_some_and(|other| alt.rarity_weight > other.rarity_weight)
        }));
        let unweighted_policy = SearchPolicy {
            rarity_weighting: false,
            ..SearchPolicy::default()
        };
        let unweighted = index
            .search_pcm_with_policy(&p, unweighted_policy, &|| false)
            .unwrap();
        assert_eq!(unweighted.search_policy, unweighted_policy);
        assert_ne!(capped.offsets, unweighted.offsets);
    }
    #[test]
    fn cancellation_stops_landmark_retrieval() {
        let index = Index::from_pcm(movie(), &tone(8), Recipe::default(), &|| false).unwrap();
        assert!(index.search_pcm(&tone(8), &|| true).is_err());
    }
    #[test]
    fn stereo_cancellation_selects_an_original_channel() {
        let bytes: Vec<u8> = (0..100)
            .flat_map(|_| {
                [12_000i16, -12_000i16]
                    .into_iter()
                    .flat_map(i16::to_le_bytes)
            })
            .collect();
        let mono = stereo_to_mono(&bytes).unwrap();
        assert!(mono.iter().all(|sample| *sample == 12_000));
    }
    #[test]
    fn silence_does_not_create_landmarks() {
        let silence = vec![0i16; RATE as usize * 10];
        assert!(
            extract(&silence, &Recipe::default(), &|| false)
                .unwrap()
                .is_empty()
        );
    }
}
