# VLM Clip Analysis Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Analyze each clip with a cloud vision model, producing an AI-owned title, description, labels, usability verdict, best-moment range, OCR text, and a chosen thumbnail frame.

**Architecture:** Extends the `analysis` module built by the [STT plan](2026-08-03-stt-clip-transcription.md). Frame selection is pure scoring over raw grayscale bytes that FFmpeg produces in one call, so determinism is testable without image decoding. The provider chain (default → fallback → circuit breaker) and the domain validator are pure logic behind an `VlmProvider` trait; the OpenRouter HTTP client is the only networked file.

**Tech Stack:** Rust, `reqwest` 0.12 blocking, `base64`, FFmpeg for frame extraction and JPEG encoding, `rusqlite`.

## Deviations taken during implementation

Recorded so the plan matches what was actually built.

1. **The candidate budget strides; it does not truncate.** The plan's Task 3
   snippet capped candidates with `.take(MAX_CANDIDATES)` on a 2 fps stream,
   which keeps only the first 40 frames — the clip's opening 20.4 seconds. Every
   longer clip would then be titled, labeled, best-momented and thumbnailed from
   its opening alone, and the corpus contains clips of many minutes. FFmpeg
   decodes and pipes the whole clip either way, so the frames were being fetched
   and discarded. `parse_gray_frames` now selects `slot * available / count` for
   `count = min(available, 40)`, which is deterministic, allocates no more than
   before, and spans the clip. Recipe key bumped `…max40…v1` →
   `…stride-max40…v2` so stored packs and analyses go stale.
2. **Multi-statement writes are transactional.** `analysis::store::save_transcript`
   and `analysis::consolidate::apply_mapping` each issued a sequence of
   statements on a bare connection. `apply_mapping` deletes the project's entire
   `label_vocabulary` before repopulating it, so a mid-sequence failure left no
   vocabulary at all beside a half-remapped authorship table. Both now wrap their
   writes in `unchecked_transaction()`.

## Global Constraints

From the [design](../specs/2026-08-03-stt-vlm-clip-analysis-design.md) and `AGENTS.md`. Verbatim values.

- Frame-packs are built from the **proxy only**, never the original. No proxy → explicit failure.
- Frame-pack recipe key: `framepack.2fps.skip400ms.stride-max40.win3-8.sharp-bright-motion.dhash8-d5.v2` (see Deviation 1)
- Frames are encoded at **768 px** wide. Do not shrink: both Google models bill a flat token count regardless of width, and OCR agreement drops by a third to a half.
- Frame-pack identity is recipe key **+ representation kind/version + proxy file fingerprint**. A proxy regeneration invalidates stored frame-packs.
- Prompt version: `vlm.observe.v2`
- **Every array in the output schema carries `maxItems`.** Without it a repetition loop produced grammar-legal but truncated JSON.
- `landmarkCandidates` is **not** in the schema. It is a per-model hallucination surface.
- `cameraMovement` enum must include an explicit `other`: `qwen3.7-flash` emitted out-of-enum `panning` and has no strict structured-output support.
- Titles and descriptions are returned in **English** regardless of the language in the clip.
- Default model `qwen/qwen3.7-flash`; fallback `google/gemini-2.5-flash-lite`. After **10** default-model failures in one process, the fallback becomes the default for the remainder of the process. 429s and domain violations both count.
- Provenance records the model that **actually produced** the stored result.
- Per-model image-token ratio is **stored, not derived** from published prices.
- Batch key: `vlm.observe-v2.<model>-w<n>-v1`, weight = **frames**.
- An analyzed clip is **never re-analyzed** unless deleted or `--force`. Force is latest-wins.
- `notes` is human-only and must never be written by analysis.
- Never send project data to any endpoint other than the configured OpenRouter base URL.
- `cargo fmt` and `cargo clippy --all-targets -- -D warnings` before every commit.

## Decisions taken while planning

1. **Label origin lives in a local side table, not the synced operation payload.**
   `LabelAddPayload` is `deny_unknown_fields`, so adding `origin` would make older
   nodes reject new operations on receive. Labels still flow through the existing
   typed `add_label` operation unchanged; a local `analysis_labels` table records
   which (asset, normalized label) pairs analysis authored, and that is what
   consolidation and `--force` consult. Origin is deliberately not synced.
2. **Frame scoring reads raw grayscale from FFmpeg, not decoded JPEGs.** One
   FFmpeg call emits all candidates as 320×180 gray rawvideo; scoring is pure
   arithmetic over those bytes and is exhaustively testable. A second call encodes
   only the selected timestamps as 768 px JPEGs. This avoids an image-decoding
   dependency and matches the existing managed-child-process pattern.

## File Structure

| File | Responsibility |
| --- | --- |
| `analysis/frames.rs` | Pure frame scoring: sharpness, brightness, motion, dHash, selection |
| `analysis/framepack.rs` | FFmpeg candidate extraction and JPEG encoding |
| `analysis/schema.rs` | Output shape, JSON schema, domain validation |
| `analysis/provider.rs` | `VlmProvider` trait, model registry, provider chain + breaker |
| `analysis/openrouter.rs` | OpenRouter HTTP client (the only networked file) |
| `analysis/vlm_store.rs` | Analysis persistence, AI-origin label tracking |
| `analysis/vlm.rs` | Pipeline orchestration |
| `analysis/consolidate.rs` | Two-pass label vocabulary consolidation |
| `migrations.rs` | Migration 14 (modify) |
| `lib.rs`, `apps/cli/src/main.rs` | Catalog entry points and CLI (modify) |

---

### Task 1: Migration 14 — analysis tables and AI-owned fields

**Files:** Modify `core/rcdvlog-core/src/migrations.rs`

**Produces:** `PROJECT_CATALOG_SCHEMA_VERSION == 14`; tables `asset_frame_packs`, `asset_analyses`, `analysis_labels`, `label_vocabulary`; columns `assets.title`, `assets.description`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn migration_14_adds_analysis_tables_and_ai_owned_columns() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate_project_catalog(&mut connection).unwrap();
    for table in [
        "asset_frame_packs",
        "asset_analyses",
        "analysis_labels",
        "label_vocabulary",
    ] {
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "missing table {table}");
    }
    let asset_columns: Vec<String> = connection
        .prepare("SELECT name FROM pragma_table_info('assets')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<String>>>()
        .unwrap();
    assert!(asset_columns.contains(&"title".to_string()));
    assert!(asset_columns.contains(&"description".to_string()));
    assert_eq!(PROJECT_CATALOG_SCHEMA_VERSION, 14);
}

#[test]
fn analysis_status_rejects_unknown_value() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate_project_catalog(&mut connection).unwrap();
    connection
        .execute(
            "INSERT INTO projects (id, name, created_at) VALUES ('p1','p','2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO assets (id, project_id, fingerprint, path, filename, size_bytes,
             duration_seconds, created_at)
             VALUES ('a1','p1','f1','/x.mp4','x.mp4',1,1.0,'2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    assert!(
        connection
            .execute(
                "INSERT INTO asset_analyses (asset_id, recipe_key, prompt_version, provider,
                 model, status, created_at)
                 VALUES ('a1','r','v2','openrouter','m','banana','2026-01-01T00:00:00Z')",
                [],
            )
            .is_err()
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rcdvlog-core --lib migration_14`
Expected: FAIL — `missing table asset_frame_packs`.

- [ ] **Step 3: Write minimal implementation**

Bump `PROJECT_CATALOG_SCHEMA_VERSION` to `14`, append to `PROJECT_CATALOG_MIGRATIONS`:

```rust
    SqliteMigration {
        id: 14,
        name: "clip_analysis",
        source_identity: "project-catalog/0014-clip-analysis/20260803",
        apply: add_clip_analysis,
    },
```

```rust
/// Migration 14. Cloud vision analysis as derived artifacts.
///
/// `title` and `description` are AI-written and human-editable, so they live on
/// `assets` beside `notes` rather than in the analysis row: an edit must survive
/// the analysis being deleted. `notes` stays human-only.
///
/// `analysis_labels` records which labels analysis authored. It is deliberately
/// local and unsynced — the labels themselves flow through the existing typed
/// label operation, and adding an origin field to that payload would break
/// older nodes, whose deserializer denies unknown fields.
fn add_clip_analysis(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        ALTER TABLE assets ADD COLUMN title TEXT NOT NULL DEFAULT '';
        ALTER TABLE assets ADD COLUMN description TEXT NOT NULL DEFAULT '';

        CREATE TABLE IF NOT EXISTS asset_frame_packs (
            asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
            recipe_key TEXT NOT NULL,
            representation TEXT NOT NULL,
            source_fingerprint TEXT NOT NULL,
            manifest_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS asset_analyses (
            asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
            recipe_key TEXT NOT NULL,
            prompt_version TEXT NOT NULL,
            provider TEXT NOT NULL,
            model TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('ok', 'rejected', 'failed')),
            reject_reason TEXT NOT NULL DEFAULT '',
            usable INTEGER NOT NULL DEFAULT 1,
            unusable_reason TEXT NOT NULL DEFAULT '',
            best_moment_start_ms INTEGER,
            best_moment_end_ms INTEGER,
            thumbnail_timestamp_ms INTEGER,
            camera_movement TEXT NOT NULL DEFAULT 'other',
            ocr_json TEXT NOT NULL DEFAULT '[]',
            transcript_used INTEGER NOT NULL DEFAULT 0,
            prompt_tokens INTEGER NOT NULL DEFAULT 0,
            completion_tokens INTEGER NOT NULL DEFAULT 0,
            cost_usd REAL NOT NULL DEFAULT 0,
            latency_ms INTEGER NOT NULL DEFAULT 0,
            attempts INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS analysis_labels (
            asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            normalized_label TEXT NOT NULL,
            display_label TEXT NOT NULL,
            proposed_label TEXT NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY(asset_id, normalized_label)
        );

        CREATE TABLE IF NOT EXISTS label_vocabulary (
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            normalized_label TEXT NOT NULL,
            display_label TEXT NOT NULL,
            usage_count INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            PRIMARY KEY(project_id, normalized_label)
        );
        "#,
    )?;
    Ok(())
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p rcdvlog-core`
Expected: PASS. Update any test asserting schema version 13.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add -A && git commit -m "feat(analysis): migration 14 adds clip-analysis tables"
```

---

### Task 2: Deterministic frame scoring

**Files:** Create `core/rcdvlog-core/src/analysis/frames.rs`

**Produces:**
- `pub struct GrayFrame { pub timestamp_ms: u32, pub width: usize, pub height: usize, pub pixels: Vec<u8> }`
- `pub struct FrameScore { pub timestamp_ms: u32, pub sharpness: f64, pub brightness: f64, pub motion: f64 }`
- `pub fn score_frames(frames: &[GrayFrame]) -> Vec<FrameScore>`
- `pub fn dhash(frame: &GrayFrame) -> u64`
- `pub fn hamming(a: u64, b: u64) -> u32`
- `pub fn select_frames(scores: &[FrameScore], frames: &[GrayFrame]) -> Vec<u32>`

Selection contract, matching the validated recipe: target count scales 3–8 with mean motion; the best-scoring frame per temporal window wins; frames whose dHash is within distance 5 of an already-selected frame are dropped; output is sorted ascending and byte-identical across runs.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn flat(timestamp_ms: u32, value: u8) -> GrayFrame {
        GrayFrame { timestamp_ms, width: 8, height: 8, pixels: vec![value; 64] }
    }

    fn checker(timestamp_ms: u32) -> GrayFrame {
        let pixels = (0..64)
            .map(|i| if (i / 8 + i % 8) % 2 == 0 { 0 } else { 255 })
            .collect();
        GrayFrame { timestamp_ms, width: 8, height: 8, pixels }
    }

    #[test]
    fn a_flat_frame_has_no_sharpness_and_a_checkerboard_has_a_lot() {
        let scores = score_frames(&[flat(0, 128), checker(500)]);
        assert_eq!(scores[0].sharpness, 0.0);
        assert!(scores[1].sharpness > scores[0].sharpness);
    }

    #[test]
    fn brightness_is_the_mean_pixel_value() {
        let scores = score_frames(&[flat(0, 100)]);
        assert!((scores[0].brightness - 100.0).abs() < 1e-9);
    }

    #[test]
    fn motion_is_zero_for_the_first_frame_and_measures_change_after() {
        let scores = score_frames(&[flat(0, 100), flat(500, 100), flat(1_000, 200)]);
        assert_eq!(scores[0].motion, 0.0);
        assert_eq!(scores[1].motion, 0.0);
        assert!((scores[2].motion - 100.0).abs() < 1e-9);
    }

    #[test]
    fn identical_frames_hash_identically_and_differ_from_a_checkerboard() {
        assert_eq!(dhash(&flat(0, 90)), dhash(&flat(999, 90)));
        assert!(hamming(dhash(&flat(0, 90)), dhash(&checker(0))) > 5);
    }

    #[test]
    fn selection_is_byte_identical_across_repeated_runs() {
        let frames: Vec<GrayFrame> = (0..40)
            .map(|i| {
                let mut frame = checker(i * 500);
                frame.pixels[(i as usize) % 64] = i as u8;
                frame
            })
            .collect();
        let scores = score_frames(&frames);
        let first = select_frames(&scores, &frames);
        let second = select_frames(&scores, &frames);
        assert_eq!(first, second);
    }

    #[test]
    fn selection_stays_within_the_three_to_eight_band() {
        let frames: Vec<GrayFrame> = (0..40)
            .map(|i| {
                let mut frame = checker(i * 500);
                frame.pixels[(i as usize) % 64] = (i * 7) as u8;
                frame
            })
            .collect();
        let selected = select_frames(&score_frames(&frames), &frames);
        assert!((3..=8).contains(&selected.len()), "got {}", selected.len());
    }

    #[test]
    fn perceptual_duplicates_are_dropped() {
        // Twelve identical frames collapse to one distinct hash.
        let frames: Vec<GrayFrame> = (0..12).map(|i| checker(i * 500)).collect();
        let selected = select_frames(&score_frames(&frames), &frames);
        assert_eq!(selected.len(), 1, "duplicates must collapse");
    }

    #[test]
    fn selected_timestamps_are_ascending_and_come_from_the_input() {
        let frames: Vec<GrayFrame> = (0..20)
            .map(|i| {
                let mut frame = checker(i * 500 + 400);
                frame.pixels[(i as usize) % 64] = (i * 11) as u8;
                frame
            })
            .collect();
        let selected = select_frames(&score_frames(&frames), &frames);
        let available: Vec<u32> = frames.iter().map(|f| f.timestamp_ms).collect();
        assert!(selected.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(selected.iter().all(|t| available.contains(t)));
    }

    #[test]
    fn no_frames_selects_nothing() {
        assert!(select_frames(&[], &[]).is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rcdvlog-core --lib analysis::frames`
Expected: FAIL to compile — module not declared.

- [ ] **Step 3: Write minimal implementation**

Add `pub mod frames;` to `analysis/mod.rs` plus the recipe constant:

```rust
/// Identity of the frame-pack recipe. Qualified at use by the representation
/// and its fingerprint, because the same recipe over a proxy and an original
/// produces different frames.
pub const FRAMEPACK_RECIPE_KEY: &str =
    "framepack.2fps.skip400ms.max40.win3-8.sharp-bright-motion.dhash8-d5.v1";
```

```rust
//! Deterministic frame selection.
//!
//! Scoring runs over raw 8-bit grayscale that FFmpeg produces in one call, so it
//! is pure arithmetic with no image decoding and no I/O. Determinism is a PRD
//! acceptance criterion: the same clip and recipe must always select the same
//! timestamps.
//!
//! Note the measured qualification: identical recipes over a proxy and over the
//! original do *not* agree, because sharpness and motion are resolution- and
//! codec-dependent. Frame-packs are pinned to the proxy for that reason.

/// Perceptual-duplicate threshold on the 64-bit dHash.
const DHASH_MAX_DISTANCE: u32 = 5;
const MIN_SELECTED: usize = 3;
const MAX_SELECTED: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrayFrame {
    pub timestamp_ms: u32,
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameScore {
    pub timestamp_ms: u32,
    pub sharpness: f64,
    pub brightness: f64,
    pub motion: f64,
}

/// Variance of the 4-neighbour Laplacian: high for crisp edges, zero for a flat
/// or motion-blurred frame.
fn laplacian_variance(frame: &GrayFrame) -> f64 {
    if frame.width < 3 || frame.height < 3 {
        return 0.0;
    }
    let mut values = Vec::with_capacity((frame.width - 2) * (frame.height - 2));
    for y in 1..frame.height - 1 {
        for x in 1..frame.width - 1 {
            let at = |dx: usize, dy: usize| f64::from(frame.pixels[dy * frame.width + dx]);
            let value = at(x, y) * 4.0 - at(x - 1, y) - at(x + 1, y) - at(x, y - 1) - at(x, y + 1);
            values.push(value);
        }
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64
}

fn mean_pixel(frame: &GrayFrame) -> f64 {
    if frame.pixels.is_empty() {
        return 0.0;
    }
    frame.pixels.iter().map(|p| f64::from(*p)).sum::<f64>() / frame.pixels.len() as f64
}

/// Mean absolute difference from the previous frame. Zero for the first frame.
fn mean_abs_difference(current: &GrayFrame, previous: &GrayFrame) -> f64 {
    if current.pixels.len() != previous.pixels.len() || current.pixels.is_empty() {
        return 0.0;
    }
    current
        .pixels
        .iter()
        .zip(&previous.pixels)
        .map(|(a, b)| f64::from(a.abs_diff(*b)))
        .sum::<f64>()
        / current.pixels.len() as f64
}

pub fn score_frames(frames: &[GrayFrame]) -> Vec<FrameScore> {
    frames
        .iter()
        .enumerate()
        .map(|(index, frame)| FrameScore {
            timestamp_ms: frame.timestamp_ms,
            sharpness: laplacian_variance(frame),
            brightness: mean_pixel(frame),
            motion: if index == 0 {
                0.0
            } else {
                mean_abs_difference(frame, &frames[index - 1])
            },
        })
        .collect()
}

/// 64-bit difference hash over a 9x8 resample: bit set where a pixel is
/// brighter than its right-hand neighbour.
pub fn dhash(frame: &GrayFrame) -> u64 {
    if frame.width == 0 || frame.height == 0 {
        return 0;
    }
    let sample = |x: usize, y: usize| -> u32 {
        let sx = x * frame.width / 9;
        let sy = y * frame.height / 8;
        u32::from(frame.pixels[sy.min(frame.height - 1) * frame.width + sx.min(frame.width - 1)])
    };
    let mut hash = 0_u64;
    for y in 0..8 {
        for x in 0..8 {
            hash <<= 1;
            if sample(x, y) > sample(x + 1, y) {
                hash |= 1;
            }
        }
    }
    hash
}

pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// A frame's desirability: sharp and well-exposed, penalised for motion blur.
///
/// Brightness is scored as distance from mid-grey so blown-out and black frames
/// both lose. Sharpness is compressed with a logarithm because Laplacian
/// variance spans orders of magnitude and would otherwise dominate absolutely.
fn quality(score: &FrameScore) -> f64 {
    let sharp = (1.0 + score.sharpness).ln();
    let exposure = 1.0 - ((score.brightness - 128.0).abs() / 128.0);
    sharp * 2.0 + exposure
}

/// Selects 3-8 representative frames, deterministically.
pub fn select_frames(scores: &[FrameScore], frames: &[GrayFrame]) -> Vec<u32> {
    if scores.is_empty() {
        return Vec::new();
    }
    let mean_motion = scores.iter().map(|s| s.motion).sum::<f64>() / scores.len() as f64;
    // More movement means more distinct content worth sampling.
    let target = (MIN_SELECTED + (mean_motion / 12.0).round() as usize).clamp(MIN_SELECTED, MAX_SELECTED);
    let target = target.min(scores.len());

    let window = scores.len().div_ceil(target).max(1);
    let mut chosen: Vec<(u32, u64)> = Vec::new();

    for chunk_start in (0..scores.len()).step_by(window) {
        let chunk_end = (chunk_start + window).min(scores.len());
        // Ties break on timestamp so the result never depends on sort stability.
        let best = (chunk_start..chunk_end).max_by(|a, b| {
            quality(&scores[*a])
                .total_cmp(&quality(&scores[*b]))
                .then(scores[*b].timestamp_ms.cmp(&scores[*a].timestamp_ms))
        });
        let Some(best) = best else { continue };
        let hash = dhash(&frames[best]);
        if chosen
            .iter()
            .any(|(_, existing)| hamming(*existing, hash) <= DHASH_MAX_DISTANCE)
        {
            continue;
        }
        chosen.push((scores[best].timestamp_ms, hash));
    }

    let mut selected: Vec<u32> = chosen.into_iter().map(|(timestamp, _)| timestamp).collect();
    selected.sort_unstable();
    selected.truncate(MAX_SELECTED);
    selected
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p rcdvlog-core --lib analysis::frames`
Expected: PASS, 8 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add -A && git commit -m "feat(analysis): deterministic frame scoring and selection"
```

---

### Task 3: Frame extraction via FFmpeg

**Files:** Create `core/rcdvlog-core/src/analysis/framepack.rs`

**Produces:**
- `pub fn candidate_command(proxy: &Path, fps: u32, width: usize, height: usize) -> Command`
- `pub fn jpeg_command(proxy: &Path, timestamp_ms: u32, output: &Path) -> Command`
- `pub fn parse_gray_frames(bytes: &[u8], width: usize, height: usize, fps: u32, skip_ms: u32) -> Vec<GrayFrame>`
- `pub struct FramePackManifest { pub recipe_key: String, pub representation: String, pub source_fingerprint: String, pub timestamps_ms: Vec<u32> }`
- `pub fn build_frame_pack(proxy: &Path, fingerprint: &str) -> Result<FramePackManifest>`
- `pub fn encode_frames(proxy: &Path, manifest: &FramePackManifest, directory: &Path) -> Result<Vec<PathBuf>>`

Sampling contract: 2 fps, first and last 400 ms skipped, at most 40 candidates **spread evenly across the whole clip** (see Deviation 1), 320×180 working size.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn args(command: &Command) -> Vec<String> {
        command.get_args().map(|a| a.to_string_lossy().to_string()).collect()
    }

    #[test]
    fn candidates_are_raw_gray_at_the_working_size_and_rate() {
        let rendered = args(&candidate_command(Path::new("/c/p.mp4"), 2, 320, 180));
        assert!(rendered.iter().any(|a| a.contains("fps=2")));
        assert!(rendered.iter().any(|a| a.contains("320x180")));
        assert!(rendered.windows(2).any(|p| p == ["-pix_fmt", "gray"]));
        assert!(rendered.windows(2).any(|p| p == ["-f", "rawvideo"]));
    }

    #[test]
    fn jpeg_encoding_requests_768px_and_a_single_frame() {
        let rendered = args(&jpeg_command(Path::new("/c/p.mp4"), 2_500, Path::new("/t/f.jpg")));
        assert!(rendered.iter().any(|a| a == "2.500"));
        assert!(rendered.iter().any(|a| a.contains("768")));
        assert!(rendered.windows(2).any(|p| p == ["-frames:v", "1"]));
    }

    #[test]
    fn raw_bytes_split_into_frames_at_the_sampling_interval() {
        let frame_bytes = 4 * 2;
        let bytes = vec![7_u8; frame_bytes * 3];
        let frames = parse_gray_frames(&bytes, 4, 2, 2, 400);
        assert_eq!(frames.len(), 3);
        // 2 fps starting after the 400 ms skip.
        assert_eq!(frames[0].timestamp_ms, 400);
        assert_eq!(frames[1].timestamp_ms, 900);
        assert_eq!(frames[2].timestamp_ms, 1_400);
        assert_eq!(frames[0].pixels.len(), frame_bytes);
    }

    #[test]
    fn a_trailing_partial_frame_is_discarded() {
        let bytes = vec![0_u8; 8 * 2 + 3];
        assert_eq!(parse_gray_frames(&bytes, 4, 2, 2, 400).len(), 2);
    }

    #[test]
    fn candidates_are_capped_at_forty() {
        let bytes = vec![0_u8; 8 * 100];
        assert_eq!(parse_gray_frames(&bytes, 4, 2, 2, 400).len(), 40);
    }

    #[test]
    fn empty_input_parses_to_no_frames() {
        assert!(parse_gray_frames(&[], 4, 2, 2, 400).is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rcdvlog-core --lib analysis::framepack`
Expected: FAIL to compile.

- [ ] **Step 3: Write minimal implementation**

```rust
//! Turning a proxy into a frame-pack.
//!
//! Two FFmpeg calls: one emits every candidate as raw grayscale at the working
//! size, which `frames` scores; the second encodes only the selected timestamps
//! as 768 px JPEGs for the model. Frames always come from the proxy — measured,
//! the same recipe over the original produced a different pack on 3 of 4 clips
//! and took 44x longer.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::analysis::FRAMEPACK_RECIPE_KEY;
use crate::analysis::frames::{GrayFrame, score_frames, select_frames};
use crate::tool_command;

const CANDIDATE_FPS: u32 = 2;
const WORK_WIDTH: usize = 320;
const WORK_HEIGHT: usize = 180;
/// The first and last moments of a clip are usually a hand on the camera.
const SKIP_MS: u32 = 400;
const MAX_CANDIDATES: usize = 40;
/// Both Google models bill a flat token count regardless of width, and OCR
/// agreement drops by a third to a half below this. Do not "optimize" it.
const JPEG_WIDTH: u32 = 768;
/// The representation frame-packs are pinned to.
pub const REPRESENTATION: &str = "proxy.h264-720p.orientation-v3";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FramePackManifest {
    pub recipe_key: String,
    pub representation: String,
    pub source_fingerprint: String,
    pub timestamps_ms: Vec<u32>,
}

pub fn candidate_command(proxy: &Path, fps: u32, width: usize, height: usize) -> Command {
    let mut command = tool_command("ffmpeg");
    command
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(proxy)
        .arg("-vf")
        .arg(format!("fps={fps},scale={width}x{height}"))
        .arg("-pix_fmt")
        .arg("gray")
        .arg("-f")
        .arg("rawvideo")
        .arg("-");
    command
}

pub fn jpeg_command(proxy: &Path, timestamp_ms: u32, output: &Path) -> Command {
    let mut command = tool_command("ffmpeg");
    command
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-ss")
        .arg(format!("{}.{:03}", timestamp_ms / 1_000, timestamp_ms % 1_000))
        .arg("-i")
        .arg(proxy)
        .arg("-frames:v")
        .arg("1")
        .arg("-vf")
        .arg(format!("scale={JPEG_WIDTH}:-2"))
        .arg("-q:v")
        .arg("3")
        .arg(output);
    command
}

/// Splits the raw stream into frames, stamping each with its sample time.
pub fn parse_gray_frames(
    bytes: &[u8],
    width: usize,
    height: usize,
    fps: u32,
    skip_ms: u32,
) -> Vec<GrayFrame> {
    let frame_bytes = width * height;
    if frame_bytes == 0 || fps == 0 {
        return Vec::new();
    }
    let interval_ms = 1_000 / fps;
    bytes
        .chunks_exact(frame_bytes)
        .take(MAX_CANDIDATES)
        .enumerate()
        .map(|(index, chunk)| GrayFrame {
            timestamp_ms: skip_ms + index as u32 * interval_ms,
            width,
            height,
            pixels: chunk.to_vec(),
        })
        .collect()
}

pub fn build_frame_pack(proxy: &Path, fingerprint: &str) -> Result<FramePackManifest> {
    if !proxy.is_file() {
        bail!("proxy is offline: {}", proxy.display());
    }
    let output = candidate_command(proxy, CANDIDATE_FPS, WORK_WIDTH, WORK_HEIGHT)
        .stdout(Stdio::piped())
        .output()
        .context("spawn ffmpeg frame sampling")?;
    if !output.status.success() {
        bail!("ffmpeg frame sampling failed with {}", output.status);
    }
    let frames = parse_gray_frames(
        &output.stdout,
        WORK_WIDTH,
        WORK_HEIGHT,
        CANDIDATE_FPS,
        SKIP_MS,
    );
    if frames.is_empty() {
        bail!("no frames could be sampled from {}", proxy.display());
    }
    let timestamps_ms = select_frames(&score_frames(&frames), &frames);
    Ok(FramePackManifest {
        recipe_key: FRAMEPACK_RECIPE_KEY.to_string(),
        representation: REPRESENTATION.to_string(),
        source_fingerprint: fingerprint.to_string(),
        timestamps_ms,
    })
}

pub fn encode_frames(
    proxy: &Path,
    manifest: &FramePackManifest,
    directory: &Path,
) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(directory)?;
    let mut written = Vec::new();
    for timestamp in &manifest.timestamps_ms {
        let path = directory.join(format!("f{timestamp:08}.jpg"));
        let status = jpeg_command(proxy, *timestamp, &path)
            .status()
            .context("spawn ffmpeg frame encode")?;
        if !status.success() {
            bail!("ffmpeg frame encode failed with {status}");
        }
        written.push(path);
    }
    Ok(written)
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p rcdvlog-core --lib analysis::framepack`
Expected: PASS, 6 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add -A && git commit -m "feat(analysis): build frame-packs from proxies with ffmpeg"
```

---

### Task 4: Output schema and domain validation

**Files:** Create `core/rcdvlog-core/src/analysis/schema.rs`

**Produces:** `ClipObservation`, `CameraMovement`, `UnusableReason`, `pub fn output_schema(...) -> serde_json::Value`, `pub enum DomainError`, `pub fn validate(raw: &str, frame_timestamps: &[u32], clip_duration_ms: u32) -> Result<ClipObservation, DomainError>`.

Validation must reject, never repair: unknown enum values, a thumbnail timestamp not among the supplied frames, a best-moment range outside clip bounds or inverted, truncated JSON, and over-long arrays.

- [ ] **Step 1: Write the failing test**

```rust
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
    }

    #[test]
    fn truncated_json_is_rejected_as_truncation() {
        let truncated = ok_json()[..ok_json().len() / 2].to_string();
        assert!(matches!(
            validate(&truncated, &[400, 2_500], 12_000),
            Err(DomainError::Truncated)
        ));
    }

    #[test]
    fn an_out_of_enum_camera_movement_is_rejected() {
        // qwen3.7-flash emitted exactly this value.
        let raw = ok_json().replace("\"pan\"", "\"panning\"");
        assert!(matches!(
            validate(&raw, &[400, 2_500], 12_000),
            Err(DomainError::UnknownEnum { .. })
        ));
    }

    #[test]
    fn a_thumbnail_outside_the_frame_pack_is_rejected() {
        let raw = ok_json().replace("\"thumbnailTimestampMs\":2500", "\"thumbnailTimestampMs\":9999");
        assert!(matches!(
            validate(&raw, &[400, 2_500], 12_000),
            Err(DomainError::ThumbnailNotInPack { .. })
        ));
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
    fn too_many_labels_is_rejected_rather_than_truncated_silently() {
        let raw = ok_json().replace(
            r#"["museum exhibit","indoor"]"#,
            r#"["a","b","c","d","e","f","g"]"#,
        );
        assert!(matches!(
            validate(&raw, &[400, 2_500], 12_000),
            Err(DomainError::TooManyItems { .. })
        ));
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
    fn the_schema_does_not_offer_a_landmark_field() {
        let schema = output_schema(&[400], 12_000);
        assert!(schema["properties"]["landmarkCandidates"].is_null());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rcdvlog-core --lib analysis::schema`
Expected: FAIL to compile.

- [ ] **Step 3: Write minimal implementation**

Implement `ClipObservation` with `serde`, `CameraMovement`/`UnusableReason` as enums including `Other`, and `validate` performing: parse (a `serde_json` EOF error maps to `DomainError::Truncated`), then bounds/enum/membership checks in that order. `output_schema` emits `maxItems: 5` on `labels`, `maxItems: 8` on `ocrText`, an enum list for `cameraMovement` including `other`, and `thumbnailTimestampMs` described as one of the supplied timestamps. No `landmarkCandidates` key.

- [ ] **Step 4: Run tests**

Run: `cargo test -p rcdvlog-core --lib analysis::schema`
Expected: PASS, 10 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add -A && git commit -m "feat(analysis): bounded output schema with domain validation"
```

---

### Task 5: Model registry and provider chain

**Files:** Create `core/rcdvlog-core/src/analysis/provider.rs`

**Produces:** `pub struct ModelSpec { pub id, pub image_tokens_per_frame, pub supports_max_items, pub reasoning }`, `pub const MODELS: &[ModelSpec]`, `pub trait VlmProvider`, `pub struct ProviderChain`, `pub struct ChainOutcome`.

Behaviour: `qwen/qwen3.7-flash` first; on any failure retry once on `google/gemini-2.5-flash-lite`; after 10 default failures in one process the fallback becomes the default. The chain records which model served each result.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct ScriptedProvider {
        failures: RefCell<Vec<&'static str>>,
        calls: RefCell<Vec<String>>,
    }

    impl VlmProvider for ScriptedProvider {
        fn analyze(&self, model: &str, _request: &VlmRequest) -> Result<VlmResponse> {
            self.calls.borrow_mut().push(model.to_string());
            if self.failures.borrow().contains(&model) {
                anyhow::bail!("upstream 429");
            }
            Ok(VlmResponse {
                model: model.to_string(),
                body: "{}".to_string(),
                prompt_tokens: 1,
                completion_tokens: 1,
                cost_usd: 0.0,
                latency_ms: 1,
            })
        }
    }

    fn provider(failing: Vec<&'static str>) -> ScriptedProvider {
        ScriptedProvider { failures: RefCell::new(failing), calls: RefCell::new(Vec::new()) }
    }

    #[test]
    fn the_default_model_serves_when_it_works() {
        let scripted = provider(vec![]);
        let mut chain = ProviderChain::new();
        let outcome = chain.analyze(&scripted, &VlmRequest::default()).unwrap();
        assert_eq!(outcome.response.model, DEFAULT_MODEL);
        assert_eq!(outcome.attempts, 1);
        assert_eq!(*scripted.calls.borrow(), vec![DEFAULT_MODEL]);
    }

    #[test]
    fn a_failure_falls_back_once_and_records_the_serving_model() {
        let scripted = provider(vec![DEFAULT_MODEL]);
        let mut chain = ProviderChain::new();
        let outcome = chain.analyze(&scripted, &VlmRequest::default()).unwrap();
        assert_eq!(outcome.response.model, FALLBACK_MODEL);
        assert_eq!(outcome.attempts, 2);
    }

    #[test]
    fn both_failing_is_an_error() {
        let scripted = provider(vec![DEFAULT_MODEL, FALLBACK_MODEL]);
        let mut chain = ProviderChain::new();
        assert!(chain.analyze(&scripted, &VlmRequest::default()).is_err());
    }

    #[test]
    fn ten_default_failures_flip_the_default_for_the_rest_of_the_process() {
        let scripted = provider(vec![DEFAULT_MODEL]);
        let mut chain = ProviderChain::new();
        for _ in 0..10 {
            chain.analyze(&scripted, &VlmRequest::default()).unwrap();
        }
        assert!(chain.breaker_tripped());
        scripted.calls.borrow_mut().clear();
        chain.analyze(&scripted, &VlmRequest::default()).unwrap();
        // The default model is no longer attempted at all.
        assert_eq!(*scripted.calls.borrow(), vec![FALLBACK_MODEL]);
    }

    #[test]
    fn the_breaker_does_not_trip_below_the_threshold() {
        let scripted = provider(vec![DEFAULT_MODEL]);
        let mut chain = ProviderChain::new();
        for _ in 0..9 {
            chain.analyze(&scripted, &VlmRequest::default()).unwrap();
        }
        assert!(!chain.breaker_tripped());
    }

    #[test]
    fn every_registered_model_declares_its_measured_image_token_ratio() {
        // Deriving cost from published per-token prices is wrong by up to 5x.
        for model in MODELS {
            assert!(model.image_tokens_per_frame > 0, "{}", model.id);
        }
        assert!(MODELS.iter().any(|m| m.id == DEFAULT_MODEL));
        assert!(MODELS.iter().any(|m| m.id == FALLBACK_MODEL));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rcdvlog-core --lib analysis::provider`
Expected: FAIL to compile.

- [ ] **Step 3: Write minimal implementation**

Constants `DEFAULT_MODEL = "qwen/qwen3.7-flash"`, `FALLBACK_MODEL = "google/gemini-2.5-flash-lite"`, `BREAKER_THRESHOLD = 10`. `MODELS` carries measured `image_tokens_per_frame` (qwen ≈ 2175/pack, gemini-2.5-flash-lite ≈ 1235/pack — store per-pack medians divided by the measured frame count and document the source). `ProviderChain::analyze` tries `current_default()`, counts a default-model failure, and retries the fallback once.

- [ ] **Step 4: Run tests**

Run: `cargo test -p rcdvlog-core --lib analysis::provider`
Expected: PASS, 6 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add -A && git commit -m "feat(analysis): model registry and provider chain with circuit breaker"
```

---

### Task 6: OpenRouter client

**Files:** Create `core/rcdvlog-core/src/analysis/openrouter.rs`; modify `core/rcdvlog-core/Cargo.toml`

**Produces:** `pub struct OpenRouterProvider { base_url: String, api_key: String }` implementing `VlmProvider`, plus `pub fn resolve_api_key() -> Result<String>`.

The only file that performs network I/O. Requests go to the configured base URL and nowhere else.

**Credential resolution order**, first match wins:

1. `OPENROUTER_API_KEY` environment variable — the agent/CI path.
2. `~/.openrouter.key` — a file containing only the key. Trailing whitespace and
   newlines are trimmed.

An OAuth2 flow is planned and will slot in ahead of both; until then the file is
the app's normal path. The key is never logged, never placed in a URL or argv,
and never written into the catalog or any error message.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_missing_api_key_is_an_actionable_error() {
    // Safe: the test never sets the variable.
    if std::env::var("OPENROUTER_API_KEY").is_ok() {
        return;
    }
    let error = api_key_from_env().unwrap_err().to_string();
    assert!(error.contains("OPENROUTER_API_KEY"), "{error}");
}

#[test]
fn the_request_body_carries_the_frames_and_bounded_schema() {
    let body = build_request_body(
        "qwen/qwen3.7-flash",
        "prompt text",
        &["data:image/jpeg;base64,AAAA".to_string()],
        &serde_json::json!({"type":"object","properties":{}}),
    );
    assert_eq!(body["model"], "qwen/qwen3.7-flash");
    assert_eq!(body["temperature"], 0);
    let content = &body["messages"][0]["content"];
    assert!(content.as_array().unwrap().iter().any(|part| part["type"] == "image_url"));
    assert!(body["response_format"]["json_schema"].is_object());
}

#[test]
fn the_api_key_never_appears_in_the_request_body() {
    let body = build_request_body("m", "p", &[], &serde_json::json!({}));
    assert!(!body.to_string().contains("sk-"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rcdvlog-core --lib analysis::openrouter`
Expected: FAIL to compile.

- [ ] **Step 3: Write minimal implementation**

Add to `core/rcdvlog-core/Cargo.toml`: `reqwest = { version = "0.12", features = ["blocking", "json"] }` and `base64.workspace = true`. Implement `build_request_body` as a pure function (that is what the tests exercise), and `OpenRouterProvider::analyze` posting it with an `Authorization: Bearer` header, mapping non-2xx to an error carrying the status.

- [ ] **Step 4: Run tests**

Run: `cargo test -p rcdvlog-core --lib analysis::openrouter`
Expected: PASS, 3 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add -A && git commit -m "feat(analysis): OpenRouter vision client"
```

---

### Task 7: Persistence, ownership, and thumbnail commit

**Files:** Create `core/rcdvlog-core/src/analysis/vlm_store.rs`

**Produces:** `save_analysis`, `load_analysis`, `analysis_is_current`, `save_frame_pack`, `load_frame_pack`, `record_ai_labels`, `ai_labels_for_asset`, `set_title_and_description`.

Rules enforced here: `notes` is never written; `title`/`description` are written directly; labels go through the catalog's existing typed `add_label`; AI authorship is recorded in `analysis_labels`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn an_analysis_round_trips_with_its_provenance() { /* assert model, cost, tokens survive */ }

#[test]
fn a_frame_pack_from_a_different_proxy_is_stale() { /* fingerprint mismatch => not current */ }

#[test]
fn saving_an_analysis_never_writes_notes() {
    // Set a human note, save an analysis, assert the note is byte-identical.
}

#[test]
fn ai_labels_are_recorded_separately_from_human_labels() {
    // add_label twice: once via the catalog directly, once through record_ai_labels.
    // ai_labels_for_asset returns only the second.
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rcdvlog-core --lib analysis::vlm_store`
Expected: FAIL to compile.

- [ ] **Step 3: Write minimal implementation**

Mirror `analysis/store.rs`: upsert keyed on `asset_id`, staleness compares recipe key + prompt version + frame-pack fingerprint.

- [ ] **Step 4: Run tests** — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(analysis): persist analyses, frame-packs, and AI label authorship"
```

---

### Task 8: Pipeline orchestration

**Files:** Create `core/rcdvlog-core/src/analysis/vlm.rs`

**Produces:** `pub fn build_prompt(context: &PromptContext) -> String`, `pub fn analyze_clip(...) -> Result<AnalysisOutcome>`.

The prompt carries: the frame timestamps, the stored transcript text when its status is `Speech`, the project's current label vocabulary, optional trip context, and the explicit unusable escape hatch. A `Rejected` or `NoSpeech` transcript contributes nothing.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_speech_transcript_is_included_in_the_prompt() { /* text appears */ }

#[test]
fn a_rejected_transcript_is_never_included() {
    // Hallucinated text must not reach the vision prompt.
}

#[test]
fn the_existing_vocabulary_is_offered_to_the_model() { /* labels appear */ }

#[test]
fn the_prompt_keeps_the_unusable_escape_hatch() {
    assert!(build_prompt(&context).contains("unusable"));
}

#[test]
fn a_domain_invalid_response_is_stored_as_rejected_with_its_reason() { /* fake provider */ }

#[test]
fn a_valid_response_writes_title_description_and_labels() { /* fake provider */ }
```

- [ ] **Step 2–5:** implement against a fake `VlmProvider`, run, commit.

---

### Task 9: Two-pass label consolidation

**Files:** Create `core/rcdvlog-core/src/analysis/consolidate.rs`

**Produces:** `pub fn proposals(connection, project_id) -> Result<Vec<(String, usize)>>`, `pub fn apply_mapping(connection, project_id, mapping) -> Result<ConsolidationReport>`, `pub fn consolidation_prompt(proposals) -> String`.

Consolidation is a **remap**, not a re-analysis: one text-only call, no vision, no frame-pack, no new cost per clip. It only ever remaps labels recorded in `analysis_labels`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_mapping_remaps_only_ai_authored_labels() { /* human label untouched */ }

#[test]
fn consolidation_collapses_near_duplicates_into_one_vocabulary_entry() { /* 3 -> 1 */ }

#[test]
fn a_mapping_naming_an_unknown_label_is_rejected() { /* no silent invention */ }

#[test]
fn the_vocabulary_records_usage_counts() { /* counts match */ }
```

- [ ] **Step 2–5:** implement, run, commit.

---

### Task 10: Catalog entry points and CLI

**Files:** Modify `core/rcdvlog-core/src/lib.rs`, `apps/cli/src/main.rs`

**Produces:** `analyze_asset`, `analyze_missing_with_progress`, `analysis_coverage`, `consolidate_labels`; CLI `analyze generate|missing|status`, `labels consolidate|vocabulary`.

Batch weight is **frames**; batch key includes the serving model. `--force` reports how many clips have human-edited titles or descriptions before overwriting.

- [ ] **Steps:** mirror the STT catalog/CLI tasks exactly, including the read-only routing for `analyze status` and `labels vocabulary`, and the `command_name` arm.

---

### Task 11: Negative controls and end-to-end

**Files:** Create `core/rcdvlog-core/tests/vlm_controls.rs`, `core/rcdvlog-core/tests/vlm_end_to_end.rs`

The negative controls are a **permanent regression test**: black, RGB-noise, and heavy-blur variants of a real frame-pack, asserting the model refuses rather than invents. Ten models scored 30/30 clean with zero invented OCR, landmarks, people, or actions; that is a property to protect across model and prompt changes.

- [ ] **Step 1:** Generate the control frames with FFmpeg `color=black`, `geq` noise, and `boxblur`, committed as a fixture generator rather than binary fixtures.
- [ ] **Step 2:** `#[ignore]`d live test asserting `hasUsableContent` false / `unusable` true on every control, and zero `ocrText` entries.
- [ ] **Step 3:** `#[ignore]`d end-to-end test over a real project reporting clips analyzed, rejected, fallback-served count, total cost, and wall time.
- [ ] **Step 4:** Record observed numbers in `AGENTS.md` and the smoke test, including how many clips the fallback served.
- [ ] **Step 5:** Commit.

## Self-Review

**Spec coverage.** Design §4 → Tasks 2–6; §5 ownership → Tasks 1, 7; §6 consolidation → Task 9; §7 surfaces → Task 10; §8 extensibility → Task 5's registry; §9 testing → Task 11.

**Known gaps, stated rather than hidden.**
- `image_tokens_per_frame` in the registry is derived from per-pack medians in the round-2b table divided by that run's frame counts. It is an estimate until re-measured from live `usage` fields, which Task 6 records for exactly that purpose.
- Tasks 7–10 give test names and contracts rather than full bodies; they mirror structures already built and reviewed in the STT plan.
- **Cost:** a full 273-clip run spends real money against the user's OpenRouter account (~$0.03 on qwen, ~$0.08 on the fallback). Task 11's live tests are `#[ignore]`d and must not be run without explicit confirmation.
